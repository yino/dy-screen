//! 服务端转场素材目录的校验、版本化持久化和工程边界选择。

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::Utc;
use reqwest::Url;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::api::{
    ApiClient, TransitionMaterialCatalogResponse, TransitionMaterialResponse,
    TransitionMaterialSignal,
};
use crate::database::{Database, DatabaseError};

pub const TRANSITION_CATALOG_MIGRATION_VERSION: i64 = 18;
const MAX_MATERIAL_BYTES: u64 = 256 * 1024 * 1024;
const MAX_MATERIAL_DURATION_MS: u64 = 5 * 60 * 1_000;
const CATALOG_REQUEST_QUEUE_CAPACITY: usize = 4;
const CATALOG_RETRY_DELAYS: [Duration; 3] = [
    Duration::from_secs(30),
    Duration::from_secs(120),
    Duration::from_secs(300),
];

#[derive(Debug, Error)]
pub enum TransitionMaterialError {
    #[error(transparent)]
    Database(#[from] DatabaseError),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error("素材目录无效：{0}")]
    InvalidCatalog(String),
    #[error("找不到转场素材")]
    MaterialNotFound,
    #[error("剪辑工程边界无效")]
    InvalidBoundary,
    #[error("剪辑工程正在导出，暂时不能修改转场")]
    ClipExportInProgress,
    #[error("该边界已由用户锁定")]
    ManuallyLocked,
    #[error("转场 Agent 运行审计无效")]
    InvalidRunAudit,
}

pub type Result<T> = std::result::Result<T, TransitionMaterialError>;

pub trait TransitionCatalogPublisher: Send + Sync {
    fn publish(&self, state: &TransitionCatalogState);
}

#[derive(Default)]
pub struct SilentTransitionCatalogPublisher;

impl TransitionCatalogPublisher for SilentTransitionCatalogPublisher {
    fn publish(&self, _state: &TransitionCatalogState) {}
}

#[derive(Debug, Clone)]
struct CatalogSyncRequest {
    signal: TransitionMaterialSignal,
    device_id: String,
    activate_code: String,
    attempt: usize,
}

#[derive(Clone)]
pub struct TransitionCatalogCoordinator {
    sender: mpsc::Sender<CatalogSyncRequest>,
    latest_request: Arc<Mutex<Option<CatalogSyncRequest>>>,
    last_signal: Arc<Mutex<Option<(i64, String)>>>,
    repository: TransitionMaterialRepository,
    publisher: Arc<dyn TransitionCatalogPublisher>,
}

impl TransitionCatalogCoordinator {
    pub fn spawn(
        repository: TransitionMaterialRepository,
        api: ApiClient,
        cancellation: CancellationToken,
        publisher: Arc<dyn TransitionCatalogPublisher>,
    ) -> Self {
        let (sender, receiver) = mpsc::channel(CATALOG_REQUEST_QUEUE_CAPACITY);
        let coordinator = Self {
            sender: sender.clone(),
            latest_request: Arc::new(Mutex::new(None)),
            last_signal: Arc::new(Mutex::new(None)),
            repository: repository.clone(),
            publisher: publisher.clone(),
        };
        tauri::async_runtime::spawn(run_catalog_coordinator(
            receiver,
            repository,
            api,
            cancellation,
            publisher,
        ));
        coordinator
    }

    pub fn notify(
        &self,
        signal: TransitionMaterialSignal,
        device_id: &str,
        activate_code: &str,
    ) -> bool {
        if !signal.is_valid() {
            return false;
        }
        let key = (signal.catalog_version, signal.minimum_app_version.clone());
        if let Ok(mut last_signal) = self.last_signal.lock() {
            if last_signal.as_ref() == Some(&key) {
                return false;
            }
            *last_signal = Some(key);
        } else {
            return false;
        }
        let request = CatalogSyncRequest {
            signal,
            device_id: device_id.to_owned(),
            activate_code: activate_code.to_owned(),
            attempt: 0,
        };
        if let Ok(mut latest) = self.latest_request.lock() {
            *latest = Some(request.clone());
        }
        self.sender.try_send(request).is_ok()
    }

    pub fn retry(&self) -> Result<()> {
        let mut request = self
            .latest_request
            .lock()
            .map_err(|_| {
                TransitionMaterialError::InvalidCatalog("素材同步状态锁已损坏".to_owned())
            })?
            .clone()
            .ok_or_else(|| {
                TransitionMaterialError::InvalidCatalog("尚未收到服务端素材版本".to_owned())
            })?;
        request.attempt = 0;
        self.sender
            .try_send(request)
            .map_err(|_| TransitionMaterialError::InvalidCatalog("素材同步任务正在运行".to_owned()))
    }

    pub fn state(&self) -> Result<TransitionCatalogState> {
        self.repository.catalog_state()
    }

    pub fn report_signal_failure(&self, message: &str) -> Result<TransitionCatalogState> {
        let state = self.repository.set_catalog_status(
            CatalogSyncStatus::Failed,
            None,
            None,
            Some(("catalog_signal_failed", message)),
        )?;
        self.publisher.publish(&state);
        Ok(state)
    }
}

async fn run_catalog_coordinator(
    mut receiver: mpsc::Receiver<CatalogSyncRequest>,
    repository: TransitionMaterialRepository,
    api: ApiClient,
    cancellation: CancellationToken,
    publisher: Arc<dyn TransitionCatalogPublisher>,
) {
    let mut pending: Option<CatalogSyncRequest> = None;
    loop {
        let request = if let Some(request) = pending.take() {
            request
        } else {
            tokio::select! {
                _ = cancellation.cancelled() => return,
                request = receiver.recv() => match request {
                    Some(request) => request,
                    None => return,
                }
            }
        };
        match sync_catalog_once(&repository, &api, &request, publisher.as_ref()).await {
            Ok(()) => {}
            Err(error) => {
                let state = repository
                    .set_catalog_status(
                        CatalogSyncStatus::Failed,
                        Some(request.signal.catalog_version),
                        Some(&request.signal.minimum_app_version),
                        Some(("catalog_sync_failed", &error.to_string())),
                    )
                    .ok();
                if let Some(state) = state.as_ref() {
                    publisher.publish(state);
                }
                let delay = CATALOG_RETRY_DELAYS[request
                    .attempt
                    .min(CATALOG_RETRY_DELAYS.len().saturating_sub(1))];
                let mut retry = request;
                retry.attempt = retry.attempt.saturating_add(1);
                tokio::select! {
                    _ = cancellation.cancelled() => return,
                    newer = receiver.recv() => pending = newer,
                    _ = tokio::time::sleep(delay) => pending = Some(retry),
                }
            }
        }
    }
}

async fn sync_catalog_once(
    repository: &TransitionMaterialRepository,
    api: &ApiClient,
    request: &CatalogSyncRequest,
    publisher: &dyn TransitionCatalogPublisher,
) -> Result<()> {
    if !app_version_meets_minimum(
        env!("CARGO_PKG_VERSION"),
        &request.signal.minimum_app_version,
    ) {
        let state = repository.set_catalog_status(
            CatalogSyncStatus::UpgradeRequired,
            Some(request.signal.catalog_version),
            Some(&request.signal.minimum_app_version),
            Some((
                "client_upgrade_required",
                "客户端版本过低，请更新后同步转场素材",
            )),
        )?;
        publisher.publish(&state);
        return Ok(());
    }
    let checking = repository.set_catalog_status(
        CatalogSyncStatus::Checking,
        Some(request.signal.catalog_version),
        Some(&request.signal.minimum_app_version),
        None,
    )?;
    publisher.publish(&checking);
    if checking.local_catalog_version == request.signal.catalog_version
        && checking.status != CatalogSyncStatus::Failed
    {
        let ready = repository.set_catalog_status(
            CatalogSyncStatus::Ready,
            Some(request.signal.catalog_version),
            Some(&request.signal.minimum_app_version),
            None,
        )?;
        publisher.publish(&ready);
        return Ok(());
    }
    let syncing = repository.set_catalog_status(
        CatalogSyncStatus::Syncing,
        Some(request.signal.catalog_version),
        Some(&request.signal.minimum_app_version),
        None,
    )?;
    publisher.publish(&syncing);
    let response = api
        .transition_materials(
            &request.device_id,
            &request.activate_code,
            syncing.local_catalog_version,
        )
        .await
        .map_err(|error| {
            TransitionMaterialError::InvalidCatalog(format!(
                "目录请求失败：{}",
                error.safe_message()
            ))
        })?;
    let state = repository.apply_catalog(&response, cfg!(debug_assertions))?;
    publisher.publish(&state);
    Ok(())
}

fn app_version_meets_minimum(current: &str, minimum: &str) -> bool {
    match (semantic_version(current), semantic_version(minimum)) {
        (Some(current), Some(minimum)) => current >= minimum,
        _ => false,
    }
}

fn semantic_version(value: &str) -> Option<(u64, u64, u64)> {
    let core = value.trim().split(['-', '+']).next()?;
    let mut parts = core.split('.');
    let version = (
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
    );
    parts.next().is_none().then_some(version)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CatalogSyncStatus {
    Idle,
    Checking,
    Syncing,
    Ready,
    UpgradeRequired,
    Failed,
}

impl CatalogSyncStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Checking => "checking",
            Self::Syncing => "syncing",
            Self::Ready => "ready",
            Self::UpgradeRequired => "upgrade_required",
            Self::Failed => "failed",
        }
    }

    fn parse(value: &str) -> Self {
        match value {
            "checking" => Self::Checking,
            "syncing" => Self::Syncing,
            "ready" => Self::Ready,
            "upgrade_required" => Self::UpgradeRequired,
            "failed" => Self::Failed,
            _ => Self::Idle,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TransitionCatalogState {
    pub local_catalog_version: i64,
    pub remote_catalog_version: Option<i64>,
    pub minimum_app_version: Option<String>,
    pub status: CatalogSyncStatus,
    pub last_checked_at: Option<String>,
    pub last_success_at: Option<String>,
    pub last_error_code: Option<String>,
    pub last_error_message: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TransitionMaterial {
    pub asset_key: String,
    pub asset_version: i64,
    pub title: String,
    pub description: String,
    pub tags: Vec<String>,
    pub category: String,
    pub render_mode: String,
    pub video_url: String,
    pub preview_url: Option<String>,
    pub cover_url: Option<String>,
    pub sha256: String,
    pub size_bytes: u64,
    pub duration_ms: u64,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub video_codec: String,
    pub has_audio: bool,
    pub sort_order: i64,
    pub is_current: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TransitionMaterialView {
    pub asset_key: String,
    pub asset_version: i64,
    pub title: String,
    pub description: String,
    pub tags: Vec<String>,
    pub category: String,
    pub render_mode: String,
    pub duration_ms: u64,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub video_codec: String,
    pub has_audio: bool,
    pub sort_order: i64,
    pub thumbnail_available: bool,
    pub download: TransitionMaterialDownload,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MaterialDownloadStatus {
    Missing,
    Downloading,
    Transcoding,
    Ready,
    Failed,
}

impl MaterialDownloadStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Missing => "missing",
            Self::Downloading => "downloading",
            Self::Transcoding => "transcoding",
            Self::Ready => "ready",
            Self::Failed => "failed",
        }
    }

    fn parse(value: &str) -> Self {
        match value {
            "downloading" => Self::Downloading,
            "transcoding" => Self::Transcoding,
            "ready" => Self::Ready,
            "failed" => Self::Failed,
            _ => Self::Missing,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TransitionMaterialDownload {
    pub asset_key: String,
    pub asset_version: i64,
    pub source_status: MaterialDownloadStatus,
    pub source_relative_path: Option<String>,
    pub preview_status: MaterialDownloadStatus,
    pub preview_relative_path: Option<String>,
    pub validated_size_bytes: Option<u64>,
    pub last_error_code: Option<String>,
    pub last_error_message: Option<String>,
    pub updated_at: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BoundarySelectionSource {
    None,
    Agent,
    Manual,
}

impl BoundarySelectionSource {
    fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Agent => "agent",
            Self::Manual => "manual",
        }
    }

    fn parse(value: &str) -> Self {
        match value {
            "agent" => Self::Agent,
            "manual" => Self::Manual,
            _ => Self::None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ClipTransitionBoundary {
    pub id: i64,
    pub clip_project_id: i64,
    pub left_clip_segment_id: Option<i64>,
    pub right_clip_segment_id: Option<i64>,
    pub left_stable_id: i64,
    pub right_stable_id: i64,
    pub asset_key: Option<String>,
    pub asset_version: Option<i64>,
    pub selection_source: BoundarySelectionSource,
    pub confidence: Option<f64>,
    pub score: Option<f64>,
    pub scene_score: Option<f64>,
    pub continuity_score: Option<f64>,
    pub rhythm_score: Option<f64>,
    pub material_score: Option<f64>,
    pub reason: Option<String>,
    pub suggested_asset_key: Option<String>,
    pub suggested_asset_version: Option<i64>,
    pub suggestion_confidence: Option<f64>,
    pub suggestion_score: Option<f64>,
    pub suggestion_scene_score: Option<f64>,
    pub suggestion_continuity_score: Option<f64>,
    pub suggestion_rhythm_score: Option<f64>,
    pub suggestion_material_score: Option<f64>,
    pub suggestion_reason: Option<String>,
    pub suggestion_none: bool,
    pub manually_locked: bool,
    pub stale: bool,
    pub active: bool,
    pub updated_at: String,
}

#[derive(Debug, Clone)]
pub struct BoundaryAgentScoreInput<'a> {
    pub asset_key: &'a str,
    pub asset_version: i64,
    pub total_score: f64,
    pub scene_score: f64,
    pub continuity_score: f64,
    pub rhythm_score: f64,
    pub material_score: f64,
    pub reason: &'a str,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct TransitionMatchRunOutcome {
    pub matched_boundaries: usize,
    pub auto_applied: usize,
    pub suggestions: usize,
    pub none_suggestions: usize,
    pub failed_boundaries: usize,
    pub token_usage: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransitionMatchRunReview {
    pub run_id: i64,
    pub threshold: u8,
    pub matched: usize,
    pub auto_applied: usize,
    pub suggestions: usize,
    pub none_suggestions: usize,
    pub token_usage: u64,
}

#[derive(Debug, Clone)]
pub struct BoundarySelectionInput<'a> {
    pub clip_project_id: i64,
    pub left_clip_segment_id: i64,
    pub right_clip_segment_id: i64,
    pub asset: Option<(&'a str, i64)>,
    pub selection_source: BoundarySelectionSource,
    pub confidence: Option<f64>,
    pub reason: Option<&'a str>,
    pub manually_locked: bool,
}

#[derive(Clone)]
pub struct TransitionMaterialRepository {
    database: Database,
}

impl TransitionMaterialRepository {
    pub fn new(database: Database) -> Self {
        Self { database }
    }

    pub(crate) fn database(&self) -> Database {
        self.database.clone()
    }

    pub fn catalog_state(&self) -> Result<TransitionCatalogState> {
        let connection = self.database.connection()?;
        connection
            .query_row(
                r#"SELECT local_catalog_version, remote_catalog_version, minimum_app_version,
                          sync_status, last_checked_at, last_success_at,
                          last_error_code, last_error_message
                   FROM transition_catalog_state WHERE id = 1"#,
                [],
                |row| {
                    Ok(TransitionCatalogState {
                        local_catalog_version: row.get(0)?,
                        remote_catalog_version: row.get(1)?,
                        minimum_app_version: row.get(2)?,
                        status: CatalogSyncStatus::parse(&row.get::<_, String>(3)?),
                        last_checked_at: row.get(4)?,
                        last_success_at: row.get(5)?,
                        last_error_code: row.get(6)?,
                        last_error_message: row.get(7)?,
                    })
                },
            )
            .map_err(TransitionMaterialError::from)
    }

    pub fn set_catalog_status(
        &self,
        status: CatalogSyncStatus,
        remote_version: Option<i64>,
        minimum_app_version: Option<&str>,
        error: Option<(&str, &str)>,
    ) -> Result<TransitionCatalogState> {
        let now = Utc::now().to_rfc3339();
        let (error_code, error_message) = error
            .map(|(code, message)| (Some(code), Some(bounded_message(message))))
            .unwrap_or((None, None));
        self.database.connection()?.execute(
            r#"UPDATE transition_catalog_state
               SET remote_catalog_version = COALESCE(?1, remote_catalog_version),
                   minimum_app_version = COALESCE(?2, minimum_app_version),
                   sync_status = ?3, last_checked_at = ?4,
                   last_error_code = ?5, last_error_message = ?6
               WHERE id = 1"#,
            params![
                remote_version,
                minimum_app_version,
                status.as_str(),
                now,
                error_code,
                error_message
            ],
        )?;
        self.catalog_state()
    }

    pub fn apply_catalog(
        &self,
        response: &TransitionMaterialCatalogResponse,
        allow_insecure_localhost: bool,
    ) -> Result<TransitionCatalogState> {
        if response.catalog_version <= 0 {
            return Err(TransitionMaterialError::InvalidCatalog(
                "目录版本必须是正整数".to_owned(),
            ));
        }
        let current = self.catalog_state()?;
        if response.catalog_version < current.local_catalog_version {
            return Err(TransitionMaterialError::InvalidCatalog(
                "服务端目录版本低于本地版本".to_owned(),
            ));
        }
        let now = Utc::now().to_rfc3339();
        if !response.changed {
            self.database.connection()?.execute(
                r#"UPDATE transition_catalog_state
                   SET remote_catalog_version = ?1, sync_status = 'ready',
                       last_checked_at = ?2, last_error_code = NULL,
                       last_error_message = NULL WHERE id = 1"#,
                params![response.catalog_version, now],
            )?;
            return self.catalog_state();
        }

        let materials = response.materials.as_ref().ok_or_else(|| {
            TransitionMaterialError::InvalidCatalog("发生变化的目录必须包含完整素材列表".to_owned())
        })?;
        let base_url = validate_base_url(&response.cdn_base_url, allow_insecure_localhost)?;
        let normalized = normalize_materials(materials, &base_url, allow_insecure_localhost)?;
        let mut connection = self.database.connection()?;
        let transaction = connection.transaction()?;
        transaction.execute("UPDATE transition_materials SET is_current = 0", [])?;
        for material in normalized {
            let tags_json = serde_json::to_string(&material.tags).map_err(|_| {
                TransitionMaterialError::InvalidCatalog("素材标签无法序列化".to_owned())
            })?;
            transaction.execute(
                r#"INSERT INTO transition_materials(
                       asset_key, asset_version, title, description, tags_json, category,
                       render_mode, video_url, preview_url, cover_url, sha256, size_bytes,
                       duration_ms, width, height, fps, video_codec, has_audio, sort_order,
                       is_current, created_at, updated_at
                   ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                            ?13, ?14, ?15, ?16, ?17, ?18, ?19, 1, ?20, ?20)
                   ON CONFLICT(asset_key, asset_version) DO UPDATE SET
                       title=excluded.title, description=excluded.description,
                       tags_json=excluded.tags_json, category=excluded.category,
                       render_mode=excluded.render_mode, video_url=excluded.video_url,
                       preview_url=excluded.preview_url, cover_url=excluded.cover_url,
                       sha256=excluded.sha256, size_bytes=excluded.size_bytes,
                       duration_ms=excluded.duration_ms, width=excluded.width,
                       height=excluded.height, fps=excluded.fps,
                       video_codec=excluded.video_codec, has_audio=excluded.has_audio,
                       sort_order=excluded.sort_order, is_current=1, updated_at=excluded.updated_at"#,
                params![
                    material.asset_key,
                    material.asset_version,
                    material.title,
                    material.description,
                    tags_json,
                    material.category,
                    material.render_mode,
                    material.video_url,
                    material.preview_url,
                    material.cover_url,
                    material.sha256,
                    material.size_bytes as i64,
                    material.duration_ms as i64,
                    i64::from(material.width),
                    i64::from(material.height),
                    material.fps,
                    material.video_codec,
                    i64::from(material.has_audio),
                    material.sort_order,
                    now,
                ],
            )?;
            transaction.execute(
                r#"INSERT OR IGNORE INTO transition_material_downloads(
                       asset_key, asset_version, source_status, preview_status, updated_at
                   ) VALUES(?1, ?2, 'missing', 'missing', ?3)"#,
                params![material.asset_key, material.asset_version, now],
            )?;
        }
        transaction.execute(
            r#"UPDATE transition_catalog_state
               SET local_catalog_version = ?1, remote_catalog_version = ?1,
                   cdn_base_url = ?2, sync_status = 'ready', last_checked_at = ?3,
                   last_success_at = ?3, last_error_code = NULL, last_error_message = NULL
               WHERE id = 1"#,
            params![response.catalog_version, base_url.as_str(), now],
        )?;
        transaction.commit()?;
        drop(connection);
        self.catalog_state()
    }

    pub fn list_current_materials(&self) -> Result<Vec<TransitionMaterial>> {
        self.list_materials("WHERE is_current = 1 ORDER BY sort_order, asset_key, asset_version")
    }

    pub fn list_current_views(&self) -> Result<Vec<TransitionMaterialView>> {
        self.list_current_materials()?
            .into_iter()
            .map(|material| {
                let download = self.download(&material.asset_key, material.asset_version)?;
                Ok(TransitionMaterialView {
                    asset_key: material.asset_key,
                    asset_version: material.asset_version,
                    title: material.title,
                    description: material.description,
                    tags: material.tags,
                    category: material.category,
                    render_mode: material.render_mode,
                    duration_ms: material.duration_ms,
                    width: material.width,
                    height: material.height,
                    fps: material.fps,
                    video_codec: material.video_codec,
                    has_audio: material.has_audio,
                    sort_order: material.sort_order,
                    thumbnail_available: material.preview_url.is_some()
                        || material.cover_url.is_some(),
                    download,
                })
            })
            .collect()
    }

    pub fn material(&self, asset_key: &str, asset_version: i64) -> Result<TransitionMaterial> {
        let connection = self.database.connection()?;
        let row = connection
            .query_row(
                &material_select("WHERE asset_key = ?1 AND asset_version = ?2"),
                params![asset_key, asset_version],
                map_material_row,
            )
            .optional()?
            .ok_or(TransitionMaterialError::MaterialNotFound)?;
        parse_material_row(row)
    }

    fn list_materials(&self, suffix: &str) -> Result<Vec<TransitionMaterial>> {
        let connection = self.database.connection()?;
        let mut statement = connection.prepare(&material_select(suffix))?;
        let rows = statement.query_map([], map_material_row)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
            .into_iter()
            .map(parse_material_row)
            .collect()
    }

    pub fn download(
        &self,
        asset_key: &str,
        asset_version: i64,
    ) -> Result<TransitionMaterialDownload> {
        let connection = self.database.connection()?;
        connection
            .query_row(
                r#"SELECT asset_key, asset_version, source_status, source_relative_path,
                          preview_status, preview_relative_path, validated_size_bytes,
                          last_error_code, last_error_message, updated_at
                   FROM transition_material_downloads
                   WHERE asset_key = ?1 AND asset_version = ?2"#,
                params![asset_key, asset_version],
                |row| {
                    Ok(TransitionMaterialDownload {
                        asset_key: row.get(0)?,
                        asset_version: row.get(1)?,
                        source_status: MaterialDownloadStatus::parse(&row.get::<_, String>(2)?),
                        source_relative_path: row.get(3)?,
                        preview_status: MaterialDownloadStatus::parse(&row.get::<_, String>(4)?),
                        preview_relative_path: row.get(5)?,
                        validated_size_bytes: row
                            .get::<_, Option<i64>>(6)?
                            .and_then(|value| u64::try_from(value).ok()),
                        last_error_code: row.get(7)?,
                        last_error_message: row.get(8)?,
                        updated_at: row.get(9)?,
                    })
                },
            )
            .optional()?
            .ok_or(TransitionMaterialError::MaterialNotFound)
    }

    pub fn set_source_download(
        &self,
        asset_key: &str,
        asset_version: i64,
        status: MaterialDownloadStatus,
        relative_path: Option<&str>,
        validated_size_bytes: Option<u64>,
        error: Option<(&str, &str)>,
    ) -> Result<TransitionMaterialDownload> {
        validate_relative_cache_path(status, relative_path)?;
        let (error_code, error_message) = normalized_error(error);
        let changed = self.database.connection()?.execute(
            r#"UPDATE transition_material_downloads
               SET source_status = ?1, source_relative_path = ?2,
                   validated_size_bytes = ?3, last_error_code = ?4,
                   last_error_message = ?5, updated_at = ?6
               WHERE asset_key = ?7 AND asset_version = ?8"#,
            params![
                status.as_str(),
                relative_path,
                validated_size_bytes.map(|value| value as i64),
                error_code,
                error_message,
                Utc::now().to_rfc3339(),
                asset_key,
                asset_version,
            ],
        )?;
        if changed == 0 {
            return Err(TransitionMaterialError::MaterialNotFound);
        }
        self.download(asset_key, asset_version)
    }

    pub fn set_preview_download(
        &self,
        asset_key: &str,
        asset_version: i64,
        status: MaterialDownloadStatus,
        relative_path: Option<&str>,
        error: Option<(&str, &str)>,
    ) -> Result<TransitionMaterialDownload> {
        validate_relative_cache_path(status, relative_path)?;
        let (error_code, error_message) = normalized_error(error);
        let changed = self.database.connection()?.execute(
            r#"UPDATE transition_material_downloads
               SET preview_status = ?1, preview_relative_path = ?2,
                   last_error_code = ?3, last_error_message = ?4, updated_at = ?5
               WHERE asset_key = ?6 AND asset_version = ?7"#,
            params![
                status.as_str(),
                relative_path,
                error_code,
                error_message,
                Utc::now().to_rfc3339(),
                asset_key,
                asset_version,
            ],
        )?;
        if changed == 0 {
            return Err(TransitionMaterialError::MaterialNotFound);
        }
        self.download(asset_key, asset_version)
    }

    pub fn reconcile_boundaries(
        &self,
        clip_project_id: i64,
    ) -> Result<Vec<ClipTransitionBoundary>> {
        let mut connection = self.database.connection()?;
        let transaction = connection.transaction()?;
        ensure_clip_project_editable(&transaction, clip_project_id)?;
        let segment_ids = transaction
            .prepare(
                "SELECT id FROM ai_clip_segments WHERE clip_project_id = ?1 ORDER BY position, id",
            )?
            .query_map([clip_project_id], |row| row.get::<_, i64>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let active_pairs = transaction
            .prepare(
                r#"SELECT left_stable_id, right_stable_id
                   FROM ai_clip_transition_boundaries
                   WHERE clip_project_id = ?1 AND active = 1"#,
            )?
            .query_map([clip_project_id], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
            })?
            .collect::<std::result::Result<HashSet<_>, _>>()?;
        transaction.execute(
            "UPDATE ai_clip_transition_boundaries SET active = 0 WHERE clip_project_id = ?1",
            [clip_project_id],
        )?;
        let now = Utc::now().to_rfc3339();
        for pair in segment_ids.windows(2) {
            let was_active = active_pairs.contains(&(pair[0], pair[1]));
            transaction.execute(
                r#"INSERT INTO ai_clip_transition_boundaries(
                       clip_project_id, left_clip_segment_id, right_clip_segment_id,
                       left_stable_id, right_stable_id, selection_source,
                       manually_locked, stale, active, created_at, updated_at
                   ) VALUES(?1, ?2, ?3, ?2, ?3, 'none', 0, 0, 1, ?4, ?4)
                   ON CONFLICT(clip_project_id, left_stable_id, right_stable_id) DO UPDATE SET
                       left_clip_segment_id=excluded.left_clip_segment_id,
                       right_clip_segment_id=excluded.right_clip_segment_id, active=1,
                       stale=CASE
                           WHEN ?5 = 1 THEN ai_clip_transition_boundaries.stale
                           WHEN ai_clip_transition_boundaries.selection_source = 'agent' THEN 1
                           ELSE ai_clip_transition_boundaries.stale
                       END,
                       updated_at=excluded.updated_at"#,
                params![
                    clip_project_id,
                    pair[0],
                    pair[1],
                    now,
                    i64::from(was_active)
                ],
            )?;
        }
        transaction.commit()?;
        drop(connection);
        self.list_boundaries(clip_project_id)
    }

    pub fn save_boundary_selection(
        &self,
        input: BoundarySelectionInput<'_>,
    ) -> Result<ClipTransitionBoundary> {
        if input
            .confidence
            .is_some_and(|value| !(0.0..=1.0).contains(&value))
        {
            return Err(TransitionMaterialError::InvalidBoundary);
        }
        let mut connection = self.database.connection()?;
        let transaction = connection.transaction()?;
        ensure_clip_project_editable(&transaction, input.clip_project_id)?;
        let current = active_boundary(
            &transaction,
            input.clip_project_id,
            input.left_clip_segment_id,
            input.right_clip_segment_id,
        )?;
        if current.manually_locked && input.selection_source == BoundarySelectionSource::Agent {
            return Err(TransitionMaterialError::ManuallyLocked);
        }
        if let Some((asset_key, asset_version)) = input.asset {
            let valid = transaction
                .query_row(
                    r#"SELECT 1 FROM transition_materials
                       WHERE asset_key = ?1 AND asset_version = ?2
                         AND is_current = 1 AND render_mode = 'bridge'"#,
                    params![asset_key, asset_version],
                    |_| Ok(()),
                )
                .optional()?
                .is_some();
            if !valid {
                return Err(TransitionMaterialError::MaterialNotFound);
            }
        }
        let (asset_key, asset_version) = input
            .asset
            .map(|(key, version)| (Some(key), Some(version)))
            .unwrap_or((None, None));
        let now = Utc::now().to_rfc3339();
        transaction.execute(
            r#"UPDATE ai_clip_transition_boundaries
               SET asset_key = ?1, asset_version = ?2, selection_source = ?3,
                   confidence = ?4, score = NULL, scene_score = NULL,
                   continuity_score = NULL, rhythm_score = NULL, material_score = NULL,
                   reason = ?5, manually_locked = ?6,
                   stale = 0, updated_at = ?7
               WHERE id = ?8"#,
            params![
                asset_key,
                asset_version,
                input.selection_source.as_str(),
                input.confidence,
                input.reason.map(bounded_message),
                i64::from(input.manually_locked),
                now,
                current.id,
            ],
        )?;
        if input.selection_source == BoundarySelectionSource::Manual {
            mark_project_edited(&transaction, input.clip_project_id, &now)?;
        } else {
            mark_project_automated(&transaction, input.clip_project_id, &now)?;
        }
        transaction.commit()?;
        drop(connection);
        self.boundary(current.id)
    }

    pub fn save_agent_score_suggestion(
        &self,
        boundary_id: i64,
        score: Option<&BoundaryAgentScoreInput<'_>>,
        none: bool,
        reason: &str,
        auto_apply: bool,
    ) -> Result<ClipTransitionBoundary> {
        let valid_score = score.is_none_or(|item| {
            [
                item.total_score,
                item.scene_score,
                item.continuity_score,
                item.rhythm_score,
                item.material_score,
            ]
            .into_iter()
            .all(|value| value.is_finite() && (0.0..=10.0).contains(&value))
        });
        if !valid_score || (none && score.is_some()) || (!none && score.is_none()) {
            return Err(TransitionMaterialError::InvalidBoundary);
        }
        let mut connection = self.database.connection()?;
        let transaction = connection.transaction()?;
        let current = transaction
            .query_row(
                &format!("{} WHERE id = ?1 AND active = 1", boundary_select()),
                [boundary_id],
                map_boundary,
            )
            .optional()?
            .ok_or(TransitionMaterialError::InvalidBoundary)?;
        ensure_clip_project_editable(&transaction, current.clip_project_id)?;
        if current.manually_locked {
            return Err(TransitionMaterialError::ManuallyLocked);
        }
        if let Some(score) = score {
            let valid = transaction
                .query_row(
                    r#"SELECT 1 FROM transition_materials
                       WHERE asset_key = ?1 AND asset_version = ?2
                         AND is_current = 1 AND render_mode = 'bridge'"#,
                    params![score.asset_key, score.asset_version],
                    |_| Ok(()),
                )
                .optional()?
                .is_some();
            if !valid {
                return Err(TransitionMaterialError::MaterialNotFound);
            }
        }
        let (suggested_key, suggested_version) = score
            .map(|item| (Some(item.asset_key), Some(item.asset_version)))
            .unwrap_or((None, None));
        let total_score = score.map(|item| item.total_score);
        let scene_score = score.map(|item| item.scene_score);
        let continuity_score = score.map(|item| item.continuity_score);
        let rhythm_score = score.map(|item| item.rhythm_score);
        let material_score = score.map(|item| item.material_score);
        let score_reason = score.map(|item| item.reason).unwrap_or(reason);
        let now = Utc::now().to_rfc3339();
        if auto_apply && !none {
            transaction.execute(
                r#"UPDATE ai_clip_transition_boundaries
                   SET asset_key = ?1, asset_version = ?2, selection_source = 'agent',
                       confidence = NULL, score = ?3, scene_score = ?4,
                       continuity_score = ?5, rhythm_score = ?6, material_score = ?7,
                       reason = ?8, stale = 0,
                       suggested_asset_key = ?1, suggested_asset_version = ?2,
                       suggestion_confidence = NULL, suggestion_score = ?3,
                       suggestion_scene_score = ?4, suggestion_continuity_score = ?5,
                       suggestion_rhythm_score = ?6, suggestion_material_score = ?7,
                       suggestion_reason = ?8, suggestion_none = 0, updated_at = ?9
                   WHERE id = ?10"#,
                params![
                    suggested_key,
                    suggested_version,
                    total_score,
                    scene_score,
                    continuity_score,
                    rhythm_score,
                    material_score,
                    bounded_message(score_reason),
                    now,
                    boundary_id,
                ],
            )?;
            mark_project_automated(&transaction, current.clip_project_id, &now)?;
        } else {
            transaction.execute(
                r#"UPDATE ai_clip_transition_boundaries
                   SET suggested_asset_key = ?1, suggested_asset_version = ?2,
                       suggestion_confidence = NULL, suggestion_score = ?3,
                       suggestion_scene_score = ?4, suggestion_continuity_score = ?5,
                       suggestion_rhythm_score = ?6, suggestion_material_score = ?7,
                       suggestion_reason = ?8, suggestion_none = ?9,
                       stale = 0, updated_at = ?10
                   WHERE id = ?11"#,
                params![
                    suggested_key,
                    suggested_version,
                    total_score,
                    scene_score,
                    continuity_score,
                    rhythm_score,
                    material_score,
                    bounded_message(score_reason),
                    i64::from(none),
                    now,
                    boundary_id,
                ],
            )?;
        }
        transaction.commit()?;
        drop(connection);
        self.boundary(boundary_id)
    }

    pub fn set_boundary_lock(
        &self,
        boundary_id: i64,
        locked: bool,
    ) -> Result<ClipTransitionBoundary> {
        let mut connection = self.database.connection()?;
        let transaction = connection.transaction()?;
        let boundary = transaction
            .query_row(
                &format!("{} WHERE id = ?1 AND active = 1", boundary_select()),
                [boundary_id],
                map_boundary,
            )
            .optional()?
            .ok_or(TransitionMaterialError::InvalidBoundary)?;
        ensure_clip_project_editable(&transaction, boundary.clip_project_id)?;
        let now = Utc::now().to_rfc3339();
        transaction.execute(
            "UPDATE ai_clip_transition_boundaries SET manually_locked = ?1, updated_at = ?2 WHERE id = ?3",
            params![i64::from(locked), now, boundary_id],
        )?;
        mark_project_edited(&transaction, boundary.clip_project_id, &now)?;
        transaction.commit()?;
        drop(connection);
        self.boundary(boundary_id)
    }

    pub fn list_boundaries(&self, clip_project_id: i64) -> Result<Vec<ClipTransitionBoundary>> {
        let connection = self.database.connection()?;
        let mut statement = connection.prepare(&format!(
            "{} WHERE clip_project_id = ?1 ORDER BY active DESC, left_stable_id, right_stable_id, id",
            boundary_select()
        ))?;
        statement
            .query_map([clip_project_id], map_boundary)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(TransitionMaterialError::from)
    }

    pub fn boundary(&self, id: i64) -> Result<ClipTransitionBoundary> {
        self.database
            .connection()?
            .query_row(
                &format!("{} WHERE id = ?1", boundary_select()),
                [id],
                map_boundary,
            )
            .optional()?
            .ok_or(TransitionMaterialError::InvalidBoundary)
    }

    pub fn latest_completed_match_run(
        &self,
        clip_project_id: i64,
    ) -> Result<Option<TransitionMatchRunReview>> {
        let row = self
            .database
            .connection()?
            .query_row(
                r#"SELECT id, threshold, matched_boundaries, auto_applied,
                          suggestions, none_suggestions, token_usage
                   FROM ai_transition_match_runs
                   WHERE clip_project_id = ?1 AND status = 'completed'
                   ORDER BY id DESC
                   LIMIT 1"#,
                [clip_project_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, i64>(6)?,
                    ))
                },
            )
            .optional()?;
        row.map(
            |(
                run_id,
                threshold,
                matched,
                auto_applied,
                suggestions,
                none_suggestions,
                token_usage,
            )| {
                Ok(TransitionMatchRunReview {
                    run_id,
                    threshold: u8::try_from(threshold)
                        .map_err(|_| TransitionMaterialError::InvalidRunAudit)?,
                    matched: usize::try_from(matched)
                        .map_err(|_| TransitionMaterialError::InvalidRunAudit)?,
                    auto_applied: usize::try_from(auto_applied)
                        .map_err(|_| TransitionMaterialError::InvalidRunAudit)?,
                    suggestions: usize::try_from(suggestions)
                        .map_err(|_| TransitionMaterialError::InvalidRunAudit)?,
                    none_suggestions: usize::try_from(none_suggestions)
                        .map_err(|_| TransitionMaterialError::InvalidRunAudit)?,
                    token_usage: u64::try_from(token_usage)
                        .map_err(|_| TransitionMaterialError::InvalidRunAudit)?,
                })
            },
        )
        .transpose()
    }

    #[allow(clippy::too_many_arguments)]
    pub fn begin_match_run(
        &self,
        clip_project_id: i64,
        project_version: u32,
        provider: &str,
        model_id: &str,
        match_prompt_version: &str,
        score_prompt_version: &str,
        threshold: u8,
        total_boundaries: usize,
        input_fingerprint: &str,
    ) -> Result<i64> {
        let connection = self.database.connection()?;
        connection.execute(
            r#"INSERT INTO ai_transition_match_runs(
                   clip_project_id, project_version, provider, model_id,
                   match_prompt_version, score_prompt_version, threshold,
                   total_boundaries, input_fingerprint, status, stage, created_at, updated_at
               ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'running', 'matching', ?10, ?10)"#,
            params![
                clip_project_id,
                project_version,
                provider,
                model_id,
                match_prompt_version,
                score_prompt_version,
                threshold,
                i64::try_from(total_boundaries).unwrap_or(i64::MAX),
                input_fingerprint,
                Utc::now().to_rfc3339()
            ],
        )?;
        Ok(connection.last_insert_rowid())
    }

    pub fn update_match_run_stage(
        &self,
        run_id: i64,
        stage: &str,
        matched_boundaries: usize,
        token_usage: u64,
    ) -> Result<()> {
        let changed = self.database.connection()?.execute(
            r#"UPDATE ai_transition_match_runs
               SET stage = ?1, matched_boundaries = ?2, token_usage = ?3, updated_at = ?4
               WHERE id = ?5 AND status = 'running'"#,
            params![
                stage,
                i64::try_from(matched_boundaries).unwrap_or(i64::MAX),
                i64::try_from(token_usage).unwrap_or(i64::MAX),
                Utc::now().to_rfc3339(),
                run_id,
            ],
        )?;
        if changed == 0 {
            return Err(TransitionMaterialError::InvalidBoundary);
        }
        Ok(())
    }

    pub fn finish_match_run(
        &self,
        run_id: i64,
        success: bool,
        final_stage: &str,
        outcome: TransitionMatchRunOutcome,
        error: Option<(&str, &str)>,
    ) -> Result<()> {
        let (error_code, error_message) = normalized_error(error);
        let changed = self.database.connection()?.execute(
            r#"UPDATE ai_transition_match_runs
               SET status = ?1, stage = ?2, matched_boundaries = ?3,
                   auto_applied = ?4, suggestions = ?5, none_suggestions = ?6,
                   failed_boundaries = ?7, token_usage = ?8,
                   error_code = ?9, error_message = ?10, updated_at = ?11
               WHERE id = ?12 AND status = 'running'"#,
            params![
                if success { "completed" } else { "failed" },
                final_stage,
                i64::try_from(outcome.matched_boundaries).unwrap_or(i64::MAX),
                i64::try_from(outcome.auto_applied).unwrap_or(i64::MAX),
                i64::try_from(outcome.suggestions).unwrap_or(i64::MAX),
                i64::try_from(outcome.none_suggestions).unwrap_or(i64::MAX),
                i64::try_from(outcome.failed_boundaries).unwrap_or(i64::MAX),
                i64::try_from(outcome.token_usage).unwrap_or(i64::MAX),
                error_code,
                error_message,
                Utc::now().to_rfc3339(),
                run_id
            ],
        )?;
        if changed == 0 {
            return Err(TransitionMaterialError::InvalidBoundary);
        }
        Ok(())
    }
}

pub(crate) fn migrate_transition_materials_v18(
    connection: &mut Connection,
) -> crate::database::Result<()> {
    let applied = connection
        .query_row(
            "SELECT 1 FROM schema_migrations WHERE version = ?1",
            [TRANSITION_CATALOG_MIGRATION_VERSION],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    let table_count: i64 = connection.query_row(
        r#"SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name IN (
               'transition_catalog_state', 'transition_materials',
               'transition_material_downloads', 'ai_clip_transition_boundaries',
               'ai_transition_match_runs'
           )"#,
        [],
        |row| row.get(0),
    )?;
    if applied && table_count == 5 {
        ensure_boundary_suggestion_columns(connection)?;
        return Ok(());
    }
    let transaction = connection.transaction()?;
    transaction.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS transition_catalog_state (
            id INTEGER PRIMARY KEY CHECK(id = 1),
            local_catalog_version INTEGER NOT NULL DEFAULT 0 CHECK(local_catalog_version >= 0),
            remote_catalog_version INTEGER CHECK(remote_catalog_version IS NULL OR remote_catalog_version > 0),
            minimum_app_version TEXT,
            cdn_base_url TEXT,
            sync_status TEXT NOT NULL DEFAULT 'idle' CHECK(sync_status IN (
                'idle', 'checking', 'syncing', 'ready', 'upgrade_required', 'failed'
            )),
            last_checked_at TEXT,
            last_success_at TEXT,
            last_error_code TEXT,
            last_error_message TEXT
        );
        INSERT OR IGNORE INTO transition_catalog_state(id) VALUES(1);

        CREATE TABLE IF NOT EXISTS transition_materials (
            asset_key TEXT NOT NULL,
            asset_version INTEGER NOT NULL CHECK(asset_version > 0),
            title TEXT NOT NULL CHECK(length(trim(title)) > 0),
            description TEXT NOT NULL,
            tags_json TEXT NOT NULL DEFAULT '[]',
            category TEXT NOT NULL,
            render_mode TEXT NOT NULL CHECK(render_mode IN ('bridge')),
            video_url TEXT NOT NULL,
            preview_url TEXT,
            cover_url TEXT,
            sha256 TEXT NOT NULL CHECK(length(sha256) = 64),
            size_bytes INTEGER NOT NULL CHECK(size_bytes > 0),
            duration_ms INTEGER NOT NULL CHECK(duration_ms > 0),
            width INTEGER NOT NULL CHECK(width > 0),
            height INTEGER NOT NULL CHECK(height > 0),
            fps REAL NOT NULL CHECK(fps > 0),
            video_codec TEXT NOT NULL,
            has_audio INTEGER NOT NULL CHECK(has_audio IN (0, 1)),
            sort_order INTEGER NOT NULL DEFAULT 0 CHECK(sort_order >= 0),
            is_current INTEGER NOT NULL DEFAULT 1 CHECK(is_current IN (0, 1)),
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            PRIMARY KEY(asset_key, asset_version)
        );
        CREATE INDEX IF NOT EXISTS idx_transition_materials_current_sort
            ON transition_materials(is_current, sort_order, asset_key);

        CREATE TABLE IF NOT EXISTS transition_material_downloads (
            asset_key TEXT NOT NULL,
            asset_version INTEGER NOT NULL,
            source_status TEXT NOT NULL DEFAULT 'missing' CHECK(source_status IN (
                'missing', 'downloading', 'transcoding', 'ready', 'failed'
            )),
            source_relative_path TEXT,
            preview_status TEXT NOT NULL DEFAULT 'missing' CHECK(preview_status IN (
                'missing', 'downloading', 'transcoding', 'ready', 'failed'
            )),
            preview_relative_path TEXT,
            validated_size_bytes INTEGER,
            last_error_code TEXT,
            last_error_message TEXT,
            updated_at TEXT NOT NULL,
            PRIMARY KEY(asset_key, asset_version),
            FOREIGN KEY(asset_key, asset_version)
                REFERENCES transition_materials(asset_key, asset_version) ON DELETE RESTRICT
        );

        CREATE TABLE IF NOT EXISTS ai_clip_transition_boundaries (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            clip_project_id INTEGER NOT NULL REFERENCES ai_clip_projects(id) ON DELETE CASCADE,
            left_clip_segment_id INTEGER REFERENCES ai_clip_segments(id) ON DELETE SET NULL,
            right_clip_segment_id INTEGER REFERENCES ai_clip_segments(id) ON DELETE SET NULL,
            left_stable_id INTEGER NOT NULL,
            right_stable_id INTEGER NOT NULL,
            asset_key TEXT,
            asset_version INTEGER,
            selection_source TEXT NOT NULL DEFAULT 'none' CHECK(selection_source IN ('none', 'agent', 'manual')),
            confidence REAL CHECK(confidence IS NULL OR (confidence >= 0 AND confidence <= 1)),
            reason TEXT,
            suggested_asset_key TEXT,
            suggested_asset_version INTEGER,
            suggestion_confidence REAL CHECK(suggestion_confidence IS NULL OR (suggestion_confidence >= 0 AND suggestion_confidence <= 1)),
            suggestion_reason TEXT,
            suggestion_none INTEGER NOT NULL DEFAULT 0 CHECK(suggestion_none IN (0, 1)),
            manually_locked INTEGER NOT NULL DEFAULT 0 CHECK(manually_locked IN (0, 1)),
            stale INTEGER NOT NULL DEFAULT 0 CHECK(stale IN (0, 1)),
            active INTEGER NOT NULL DEFAULT 1 CHECK(active IN (0, 1)),
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            CHECK(left_stable_id <> right_stable_id),
            CHECK((asset_key IS NULL) = (asset_version IS NULL)),
            CHECK((suggested_asset_key IS NULL) = (suggested_asset_version IS NULL)),
            UNIQUE(clip_project_id, left_stable_id, right_stable_id),
            FOREIGN KEY(asset_key, asset_version)
                REFERENCES transition_materials(asset_key, asset_version) ON DELETE RESTRICT
        );
        CREATE INDEX IF NOT EXISTS idx_ai_clip_transition_boundaries_project_active
            ON ai_clip_transition_boundaries(clip_project_id, active, left_stable_id);

        CREATE TABLE IF NOT EXISTS ai_transition_match_runs (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            clip_project_id INTEGER NOT NULL REFERENCES ai_clip_projects(id) ON DELETE CASCADE,
            provider TEXT NOT NULL,
            input_fingerprint TEXT NOT NULL,
            status TEXT NOT NULL CHECK(status IN ('running', 'completed', 'failed')),
            error_code TEXT,
            error_message TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_ai_transition_match_runs_project
            ON ai_transition_match_runs(clip_project_id, created_at DESC);
        "#,
    )?;
    transaction.execute(
        "INSERT OR IGNORE INTO schema_migrations(version, applied_at) VALUES(?1, ?2)",
        params![
            TRANSITION_CATALOG_MIGRATION_VERSION,
            Utc::now().to_rfc3339()
        ],
    )?;
    transaction.commit()?;
    ensure_boundary_suggestion_columns(connection)?;
    Ok(())
}

fn ensure_boundary_suggestion_columns(connection: &Connection) -> crate::database::Result<()> {
    let columns = connection
        .prepare("SELECT name FROM pragma_table_info('ai_clip_transition_boundaries')")?
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<std::result::Result<HashSet<_>, _>>()?;
    for (name, sql) in [
        (
            "suggested_asset_key",
            "ALTER TABLE ai_clip_transition_boundaries ADD COLUMN suggested_asset_key TEXT",
        ),
        (
            "suggested_asset_version",
            "ALTER TABLE ai_clip_transition_boundaries ADD COLUMN suggested_asset_version INTEGER",
        ),
        (
            "suggestion_confidence",
            "ALTER TABLE ai_clip_transition_boundaries ADD COLUMN suggestion_confidence REAL",
        ),
        (
            "suggestion_reason",
            "ALTER TABLE ai_clip_transition_boundaries ADD COLUMN suggestion_reason TEXT",
        ),
        (
            "suggestion_none",
            "ALTER TABLE ai_clip_transition_boundaries ADD COLUMN suggestion_none INTEGER NOT NULL DEFAULT 0",
        ),
    ] {
        if !columns.contains(name) {
            connection.execute(sql, [])?;
        }
    }
    Ok(())
}

fn validate_base_url(value: &str, allow_insecure_localhost: bool) -> Result<Url> {
    let url = Url::parse(value.trim())
        .map_err(|_| TransitionMaterialError::InvalidCatalog("CDN 基础地址无法解析".to_owned()))?;
    validate_remote_url(&url, allow_insecure_localhost)?;
    if url.query().is_some() || url.fragment().is_some() {
        return Err(TransitionMaterialError::InvalidCatalog(
            "CDN 基础地址不能包含查询参数或片段".to_owned(),
        ));
    }
    Ok(url)
}

fn normalize_materials(
    materials: &[TransitionMaterialResponse],
    base_url: &Url,
    allow_insecure_localhost: bool,
) -> Result<Vec<TransitionMaterial>> {
    let mut keys = HashSet::with_capacity(materials.len());
    materials
        .iter()
        .map(|material| {
            validate_material_fields(material)?;
            if !keys.insert((material.asset_key.clone(), material.asset_version)) {
                return Err(TransitionMaterialError::InvalidCatalog(
                    "素材键和版本重复".to_owned(),
                ));
            }
            Ok(TransitionMaterial {
                asset_key: material.asset_key.trim().to_owned(),
                asset_version: material.asset_version,
                title: material.title.trim().to_owned(),
                description: material.description.trim().to_owned(),
                tags: normalize_tags(&material.tags)?,
                category: material.category.trim().to_owned(),
                render_mode: material.render_mode.clone(),
                video_url: resolve_asset_url(
                    base_url,
                    &material.video_path,
                    allow_insecure_localhost,
                )?,
                preview_url: material
                    .preview_path
                    .as_deref()
                    .map(|value| resolve_asset_url(base_url, value, allow_insecure_localhost))
                    .transpose()?,
                cover_url: material
                    .cover_path
                    .as_deref()
                    .map(|value| resolve_asset_url(base_url, value, allow_insecure_localhost))
                    .transpose()?,
                sha256: material.sha256.to_ascii_lowercase(),
                size_bytes: material.size_bytes,
                duration_ms: material.duration_ms,
                width: material.width,
                height: material.height,
                fps: material.fps,
                video_codec: material.video_codec.trim().to_ascii_lowercase(),
                has_audio: material.has_audio,
                sort_order: material.sort_order,
                is_current: true,
            })
        })
        .collect()
}

fn validate_material_fields(material: &TransitionMaterialResponse) -> Result<()> {
    let valid_key = !material.asset_key.is_empty()
        && material.asset_key.len() <= 128
        && material
            .asset_key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'));
    if !valid_key
        || material.asset_version <= 0
        || material.title.trim().is_empty()
        || material.title.chars().count() > 128
        || material.description.chars().count() > 1_000
        || material.category.trim().is_empty()
        || material.category.chars().count() > 64
        || material.render_mode != "bridge"
        || material.sha256.len() != 64
        || !material.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
        || !(1..=MAX_MATERIAL_BYTES).contains(&material.size_bytes)
        || !(1..=MAX_MATERIAL_DURATION_MS).contains(&material.duration_ms)
        || !(1..=8192).contains(&material.width)
        || !(1..=8192).contains(&material.height)
        || !material.fps.is_finite()
        || !(0.0..=240.0).contains(&material.fps)
        || material.fps == 0.0
        || material.video_codec.trim().is_empty()
        || material.video_codec.len() > 64
        || material.sort_order < 0
    {
        return Err(TransitionMaterialError::InvalidCatalog(
            "素材字段超出客户端允许范围".to_owned(),
        ));
    }
    Ok(())
}

fn normalize_tags(tags: &[String]) -> Result<Vec<String>> {
    let mut normalized = Vec::new();
    for tag in tags {
        let tag = tag.trim();
        if tag.is_empty() || tag.chars().count() > 32 || tag.chars().any(char::is_control) {
            return Err(TransitionMaterialError::InvalidCatalog(
                "素材标签无效".to_owned(),
            ));
        }
        if !normalized.iter().any(|current| current == tag) {
            normalized.push(tag.to_owned());
        }
        if normalized.len() > 32 {
            return Err(TransitionMaterialError::InvalidCatalog(
                "素材标签数量过多".to_owned(),
            ));
        }
    }
    Ok(normalized)
}

fn resolve_asset_url(
    base_url: &Url,
    value: &str,
    allow_insecure_localhost: bool,
) -> Result<String> {
    let value = value.trim();
    let lower = value.to_ascii_lowercase();
    if value.is_empty() || value.split('/').any(|part| part == "..") || lower.contains("%2e%2e") {
        return Err(TransitionMaterialError::InvalidCatalog(
            "素材地址包含非法路径".to_owned(),
        ));
    }
    let url = Url::parse(value)
        .or_else(|_| base_url.join(value))
        .map_err(|_| TransitionMaterialError::InvalidCatalog("素材地址无法解析".to_owned()))?;
    validate_remote_url(&url, allow_insecure_localhost)?;
    if !same_origin(base_url, &url) {
        return Err(TransitionMaterialError::InvalidCatalog(
            "素材地址与受信 CDN 不同源".to_owned(),
        ));
    }
    Ok(url.into())
}

fn validate_remote_url(url: &Url, allow_insecure_localhost: bool) -> Result<()> {
    let host = url
        .host_str()
        .ok_or_else(|| TransitionMaterialError::InvalidCatalog("远程地址缺少主机".to_owned()))?;
    let local_http = url.scheme() == "http"
        && allow_insecure_localhost
        && matches!(host, "localhost" | "127.0.0.1" | "::1");
    if (url.scheme() != "https" && !local_http)
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(TransitionMaterialError::InvalidCatalog(
            "远程地址不在客户端信任范围".to_owned(),
        ));
    }
    Ok(())
}

fn same_origin(left: &Url, right: &Url) -> bool {
    left.scheme() == right.scheme()
        && left.host_str() == right.host_str()
        && left.port_or_known_default() == right.port_or_known_default()
}

type MaterialRow = (
    String,
    i64,
    String,
    String,
    String,
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    String,
    i64,
    i64,
    i64,
    i64,
    f64,
    String,
    bool,
    i64,
    bool,
);

fn material_select(suffix: &str) -> String {
    format!(
        r#"SELECT asset_key, asset_version, title, description, tags_json, category,
                  render_mode, video_url, preview_url, cover_url, sha256, size_bytes,
                  duration_ms, width, height, fps, video_codec, has_audio, sort_order,
                  is_current FROM transition_materials {suffix}"#
    )
}

fn map_material_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<MaterialRow> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
        row.get(8)?,
        row.get(9)?,
        row.get(10)?,
        row.get(11)?,
        row.get(12)?,
        row.get(13)?,
        row.get(14)?,
        row.get(15)?,
        row.get(16)?,
        row.get(17)?,
        row.get(18)?,
        row.get(19)?,
    ))
}

fn parse_material_row(row: MaterialRow) -> Result<TransitionMaterial> {
    Ok(TransitionMaterial {
        asset_key: row.0,
        asset_version: row.1,
        title: row.2,
        description: row.3,
        tags: serde_json::from_str(&row.4)
            .map_err(|_| TransitionMaterialError::InvalidCatalog("素材标签数据损坏".to_owned()))?,
        category: row.5,
        render_mode: row.6,
        video_url: row.7,
        preview_url: row.8,
        cover_url: row.9,
        sha256: row.10,
        size_bytes: u64::try_from(row.11)
            .map_err(|_| TransitionMaterialError::InvalidCatalog("素材大小无效".to_owned()))?,
        duration_ms: u64::try_from(row.12)
            .map_err(|_| TransitionMaterialError::InvalidCatalog("素材时长无效".to_owned()))?,
        width: u32::try_from(row.13)
            .map_err(|_| TransitionMaterialError::InvalidCatalog("素材宽度无效".to_owned()))?,
        height: u32::try_from(row.14)
            .map_err(|_| TransitionMaterialError::InvalidCatalog("素材高度无效".to_owned()))?,
        fps: row.15,
        video_codec: row.16,
        has_audio: row.17,
        sort_order: row.18,
        is_current: row.19,
    })
}

fn validate_relative_cache_path(
    status: MaterialDownloadStatus,
    relative_path: Option<&str>,
) -> Result<()> {
    let ready_has_path = status != MaterialDownloadStatus::Ready || relative_path.is_some();
    let path_is_safe = relative_path.is_none_or(|path| {
        let path = std::path::Path::new(path);
        !path.is_absolute()
            && path.components().all(|component| {
                !matches!(
                    component,
                    std::path::Component::ParentDir
                        | std::path::Component::RootDir
                        | std::path::Component::Prefix(_)
                )
            })
    });
    if !ready_has_path || !path_is_safe {
        return Err(TransitionMaterialError::InvalidCatalog(
            "素材缓存路径无效".to_owned(),
        ));
    }
    Ok(())
}

fn normalized_error(error: Option<(&str, &str)>) -> (Option<String>, Option<String>) {
    error
        .map(|(code, message)| (Some(code.to_owned()), Some(bounded_message(message))))
        .unwrap_or((None, None))
}

fn bounded_message(value: &str) -> String {
    value
        .trim()
        .chars()
        .filter(|character| !character.is_control())
        .take(256)
        .collect()
}

fn boundary_select() -> &'static str {
    r#"SELECT id, clip_project_id, left_clip_segment_id, right_clip_segment_id,
              left_stable_id, right_stable_id, asset_key, asset_version,
              selection_source, confidence, score, scene_score, continuity_score,
              rhythm_score, material_score, reason,
              suggested_asset_key, suggested_asset_version, suggestion_confidence,
              suggestion_score, suggestion_scene_score, suggestion_continuity_score,
              suggestion_rhythm_score, suggestion_material_score,
              suggestion_reason, suggestion_none,
              manually_locked, stale, active, updated_at
       FROM ai_clip_transition_boundaries"#
}

fn map_boundary(row: &rusqlite::Row<'_>) -> rusqlite::Result<ClipTransitionBoundary> {
    Ok(ClipTransitionBoundary {
        id: row.get(0)?,
        clip_project_id: row.get(1)?,
        left_clip_segment_id: row.get(2)?,
        right_clip_segment_id: row.get(3)?,
        left_stable_id: row.get(4)?,
        right_stable_id: row.get(5)?,
        asset_key: row.get(6)?,
        asset_version: row.get(7)?,
        selection_source: BoundarySelectionSource::parse(&row.get::<_, String>(8)?),
        confidence: row.get(9)?,
        score: row.get(10)?,
        scene_score: row.get(11)?,
        continuity_score: row.get(12)?,
        rhythm_score: row.get(13)?,
        material_score: row.get(14)?,
        reason: row.get(15)?,
        suggested_asset_key: row.get(16)?,
        suggested_asset_version: row.get(17)?,
        suggestion_confidence: row.get(18)?,
        suggestion_score: row.get(19)?,
        suggestion_scene_score: row.get(20)?,
        suggestion_continuity_score: row.get(21)?,
        suggestion_rhythm_score: row.get(22)?,
        suggestion_material_score: row.get(23)?,
        suggestion_reason: row.get(24)?,
        suggestion_none: row.get(25)?,
        manually_locked: row.get(26)?,
        stale: row.get(27)?,
        active: row.get(28)?,
        updated_at: row.get(29)?,
    })
}

fn active_boundary(
    transaction: &rusqlite::Transaction<'_>,
    clip_project_id: i64,
    left_id: i64,
    right_id: i64,
) -> Result<ClipTransitionBoundary> {
    transaction
        .query_row(
            &format!(
                "{} WHERE clip_project_id = ?1 AND left_clip_segment_id = ?2 AND right_clip_segment_id = ?3 AND active = 1",
                boundary_select()
            ),
            params![clip_project_id, left_id, right_id],
            map_boundary,
        )
        .optional()?
        .ok_or(TransitionMaterialError::InvalidBoundary)
}

fn ensure_clip_project_editable(
    transaction: &rusqlite::Transaction<'_>,
    clip_project_id: i64,
) -> Result<()> {
    let status = transaction
        .query_row(
            "SELECT export_status FROM ai_clip_projects WHERE id = ?1",
            [clip_project_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .ok_or(TransitionMaterialError::InvalidBoundary)?;
    if status == "exporting" {
        return Err(TransitionMaterialError::ClipExportInProgress);
    }
    Ok(())
}

fn mark_project_edited(
    transaction: &rusqlite::Transaction<'_>,
    clip_project_id: i64,
    now: &str,
) -> Result<()> {
    transaction.execute(
        r#"UPDATE ai_clip_projects
           SET version = version + 1,
               ownership = CASE WHEN smart_workflow_id IS NULL THEN ownership ELSE 'user' END,
               export_status = 'idle', export_progress = 0,
               output_path = NULL, last_error_code = NULL, last_error_message = NULL,
               updated_at = ?1 WHERE id = ?2"#,
        params![now, clip_project_id],
    )?;
    transaction.execute(
        r#"UPDATE ai_smart_drafts
           SET ownership = 'user', status = CASE
                   WHEN status IN ('active', 'review_ready') THEN 'review_ready'
                   ELSE status END,
               updated_at = ?1
           WHERE clip_project_id = ?2 AND ownership = 'automation'"#,
        params![now, clip_project_id],
    )?;
    Ok(())
}

fn mark_project_automated(
    transaction: &rusqlite::Transaction<'_>,
    clip_project_id: i64,
    now: &str,
) -> Result<()> {
    transaction.execute(
        r#"UPDATE ai_clip_projects
           SET version = version + 1, export_status = 'idle', export_progress = 0,
               output_path = NULL, last_error_code = NULL, last_error_message = NULL,
               updated_at = ?1 WHERE id = ?2"#,
        params![now, clip_project_id],
    )?;
    transaction.execute(
        r#"UPDATE ai_smart_drafts
           SET automation_project_version = automation_project_version + 1, updated_at = ?1
           WHERE clip_project_id = ?2 AND ownership = 'automation'"#,
        params![now, clip_project_id],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::{AiClipTimelineUnitKind, AiRepository};
    use crate::api::{ApiConfig, PlainJsonCodec};

    fn material(key: &str, version: i64) -> TransitionMaterialResponse {
        TransitionMaterialResponse {
            asset_key: key.to_owned(),
            asset_version: version,
            title: "震惊转场".to_owned(),
            description: "用于表达意外".to_owned(),
            tags: vec!["震惊".to_owned(), "反应".to_owned()],
            category: "neutral".to_owned(),
            render_mode: "bridge".to_owned(),
            video_path: format!("https://cdn.example/assets/{key}.mp4"),
            preview_path: None,
            cover_path: None,
            sha256: "a".repeat(64),
            size_bytes: 1_024,
            duration_ms: 2_000,
            width: 1280,
            height: 720,
            fps: 30.0,
            video_codec: "h264".to_owned(),
            has_audio: true,
            sort_order: 1,
        }
    }

    fn seed_clip_project(database: &Database) {
        let connection = database.connection().unwrap();
        connection
            .execute_batch(
                r#"
                INSERT INTO ai_projects(
                    id, name, status, recognition_profile_json, recognition_profile_hash,
                    input_frozen, progress_percent, project_tags_json, created_at, updated_at
                ) VALUES(1, '转场测试', 'completed', '{}', 'profile', 1, 100, '[]', 'now', 'now');
                INSERT INTO ai_project_inputs(
                    id, project_id, position, source_kind, display_name, source_path,
                    source_fingerprint_json, source_fingerprint_hash, duration_ms,
                    audio_present, status, created_at, updated_at
                ) VALUES
                    (1, 1, 0, 'local_file', 'a.mp4', '/tmp/a.mp4', '{}', 'a', 3000, 1, 'completed', 'now', 'now'),
                    (2, 1, 1, 'local_file', 'b.mp4', '/tmp/b.mp4', '{}', 'b', 3000, 1, 'completed', 'now', 'now'),
                    (3, 1, 2, 'local_file', 'c.mp4', '/tmp/c.mp4', '{}', 'c', 3000, 1, 'completed', 'now', 'now');
                INSERT INTO ai_highlight_runs(
                    id, project_id, status, model_id, prompt_version, tags_snapshot_json,
                    skills_snapshot_json, analysis_fingerprint, user_authorized,
                    total_segments, total_chars, estimated_batches, total_tokens,
                    created_at, updated_at
                ) VALUES(1, 1, 'completed', 'deepseek-chat', 'test', '[]', '[]', 'run', 1, 3, 30, 1, 10, 'now', 'now');
                INSERT INTO ai_highlight_candidates(
                    id, run_id, candidate_key, title, input_id, segment_ids_json,
                    start_ms, end_ms, total_score, hook_score, information_score,
                    emotion_score, tag_relevance_score, completeness_score,
                    shareability_score, reason, matched_tags_json, selected, created_at
                ) VALUES
                    (1, 1, 'a', 'A', 1, '[]', 0, 1000, 90, 90, 90, 90, 90, 90, 90, 'a', '[]', 1, 'now'),
                    (2, 1, 'b', 'B', 2, '[]', 0, 1000, 90, 90, 90, 90, 90, 90, 90, 'b', '[]', 1, 'now'),
                    (3, 1, 'c', 'C', 3, '[]', 0, 1000, 90, 90, 90, 90, 90, 90, 90, 'c', '[]', 1, 'now');
                INSERT INTO ai_clip_projects(
                    id, highlight_run_id, name, export_status, export_progress, created_at, updated_at
                ) VALUES(1, 1, '工程', 'idle', 0, 'now', 'now');
                INSERT INTO ai_clip_segments(
                    id, clip_project_id, candidate_id, input_id, position, title,
                    source_start_ms, source_end_ms, volume_percent, effect, created_at, updated_at
                ) VALUES
                    (11, 1, 1, 1, 0, 'A', 0, 1000, 100, 'none', 'now', 'now'),
                    (12, 1, 2, 2, 1, 'B', 0, 1000, 100, 'none', 'now', 'now'),
                    (13, 1, 3, 3, 2, 'C', 0, 1000, 100, 'none', 'now', 'now');
                INSERT INTO ai_clip_subtitles(
                    clip_project_id, clip_segment_id, input_id, stable_segment_id,
                    source_start_ms, source_end_ms, original_text, text, hidden, created_at, updated_at
                ) VALUES
                    (1, 11, 1, 'a-sub', 0, 500, '字幕A', '字幕A', 0, 'now', 'now'),
                    (1, 12, 2, 'b-sub', 0, 500, '字幕B', '字幕B', 0, 'now', 'now'),
                    (1, 13, 3, 'c-sub', 0, 500, '字幕C', '字幕C', 0, 'now', 'now');
                "#,
            )
            .unwrap();
    }

    #[test]
    fn validates_same_origin_and_local_http_rules() {
        let base = validate_base_url("https://cdn.example/materials/", false).unwrap();
        assert_eq!(
            resolve_asset_url(&base, "../assets/a.mp4", false)
                .unwrap_err()
                .to_string(),
            "素材目录无效：素材地址包含非法路径"
        );
        assert!(resolve_asset_url(&base, "https://other.example/a.mp4", false).is_err());
        assert!(validate_base_url("http://cdn.example/materials/", false).is_err());
        assert!(validate_base_url("http://127.0.0.1/materials/", true).is_ok());
    }

    #[test]
    fn semantic_version_gate_is_strict_and_supports_release_suffixes() {
        assert!(app_version_meets_minimum("0.3.0", "0.3.0"));
        assert!(app_version_meets_minimum("0.4.0", "0.3.9"));
        assert!(app_version_meets_minimum("0.3.1+build.2", "0.3.0"));
        assert!(!app_version_meets_minimum("0.2.9", "0.3.0"));
        assert!(!app_version_meets_minimum("development", "0.3.0"));
    }

    #[test]
    #[ignore = "需要通过 DY_SCREEN_TRANSITION_CATALOG_FIXTURE 指定服务端目录 JSON"]
    fn service_catalog_fixture_publishes_all_33_materials_without_exposing_urls() {
        let fixture = std::env::var_os("DY_SCREEN_TRANSITION_CATALOG_FIXTURE")
            .map(std::path::PathBuf::from)
            .expect("必须指定 DY_SCREEN_TRANSITION_CATALOG_FIXTURE");
        let payload: serde_json::Value =
            serde_json::from_slice(&std::fs::read(fixture).unwrap()).unwrap();
        assert_eq!(
            payload.get("code").and_then(serde_json::Value::as_i64),
            Some(200)
        );
        let response: TransitionMaterialCatalogResponse =
            serde_json::from_value(payload.get("data").cloned().unwrap()).unwrap();
        let materials = response.materials.as_ref().unwrap();
        assert_eq!(materials.len(), 33);
        assert_eq!(
            materials
                .iter()
                .filter(|material| material.video_codec == "h264")
                .count(),
            13
        );
        assert_eq!(
            materials
                .iter()
                .filter(|material| material.video_codec == "hevc")
                .count(),
            20
        );
        assert!(materials.iter().all(|material| material.has_audio));
        assert_eq!(
            materials
                .iter()
                .filter(|material| material.preview_path.is_some())
                .count(),
            32
        );

        let database = Database::open_in_memory().unwrap();
        database.migrate().unwrap();
        let repository = TransitionMaterialRepository::new(database);
        let state = repository.apply_catalog(&response, false).unwrap();
        assert_eq!(state.local_catalog_version, 2);
        assert_eq!(state.status, CatalogSyncStatus::Ready);
        let views = repository.list_current_views().unwrap();
        assert_eq!(views.len(), 33);
        assert_eq!(
            views
                .iter()
                .filter(|material| material.thumbnail_available)
                .count(),
            32
        );
        let frontend_payload = serde_json::to_string(&views).unwrap();
        assert!(!frontend_payload.contains("https://"));
        assert!(!frontend_payload.contains("/assets/"));
    }

    #[derive(Default)]
    struct CapturingPublisher {
        statuses: Mutex<Vec<CatalogSyncStatus>>,
    }

    impl TransitionCatalogPublisher for CapturingPublisher {
        fn publish(&self, state: &TransitionCatalogState) {
            self.statuses.lock().unwrap().push(state.status.clone());
        }
    }

    fn unreachable_api() -> ApiClient {
        ApiClient::new(
            ApiConfig {
                base_url: "http://127.0.0.1:9/api".to_owned(),
                timeout: Duration::from_millis(50),
                client_version: "0.3.0".to_owned(),
                platform: "mac".to_owned(),
            },
            Arc::new(PlainJsonCodec),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn coordinator_version_gate_and_same_version_path_do_not_require_network() {
        let database = Database::open_in_memory().unwrap();
        database.migrate().unwrap();
        let repository = TransitionMaterialRepository::new(database);
        repository
            .apply_catalog(
                &TransitionMaterialCatalogResponse {
                    catalog_version: 1,
                    changed: true,
                    cdn_base_url: "https://cdn.example/materials/".to_owned(),
                    materials: Some(vec![material("tm_test", 1)]),
                },
                false,
            )
            .unwrap();
        let publisher = CapturingPublisher::default();
        sync_catalog_once(
            &repository,
            &unreachable_api(),
            &CatalogSyncRequest {
                signal: TransitionMaterialSignal {
                    catalog_version: 1,
                    minimum_app_version: "0.3.0".to_owned(),
                },
                device_id: "device".to_owned(),
                activate_code: "code".to_owned(),
                attempt: 0,
            },
            &publisher,
        )
        .await
        .unwrap();
        assert_eq!(
            repository.catalog_state().unwrap().status,
            CatalogSyncStatus::Ready
        );

        sync_catalog_once(
            &repository,
            &unreachable_api(),
            &CatalogSyncRequest {
                signal: TransitionMaterialSignal {
                    catalog_version: 2,
                    minimum_app_version: "9.0.0".to_owned(),
                },
                device_id: "device".to_owned(),
                activate_code: "code".to_owned(),
                attempt: 0,
            },
            &publisher,
        )
        .await
        .unwrap();
        let state = repository.catalog_state().unwrap();
        assert_eq!(state.status, CatalogSyncStatus::UpgradeRequired);
        assert_eq!(state.local_catalog_version, 1);
        assert_eq!(state.remote_catalog_version, Some(2));
    }

    #[test]
    fn full_catalog_publish_is_transactional_and_preserves_old_versions() {
        let database = Database::open_in_memory().unwrap();
        database.migrate().unwrap();
        let repository = TransitionMaterialRepository::new(database);
        repository
            .apply_catalog(
                &TransitionMaterialCatalogResponse {
                    catalog_version: 1,
                    changed: true,
                    cdn_base_url: "https://cdn.example/materials/".to_owned(),
                    materials: Some(vec![material("tm_test", 1)]),
                },
                false,
            )
            .unwrap();
        repository
            .set_source_download(
                "tm_test",
                1,
                MaterialDownloadStatus::Ready,
                Some("materials/tm_test/1/source.mp4"),
                Some(1_024),
                None,
            )
            .unwrap();
        repository
            .apply_catalog(
                &TransitionMaterialCatalogResponse {
                    catalog_version: 2,
                    changed: true,
                    cdn_base_url: "https://cdn.example/materials/".to_owned(),
                    materials: Some(vec![material("tm_test", 2)]),
                },
                false,
            )
            .unwrap();

        assert_eq!(
            repository.list_current_materials().unwrap()[0].asset_version,
            2
        );
        assert!(!repository.material("tm_test", 1).unwrap().is_current);
        assert_eq!(
            repository.download("tm_test", 1).unwrap().source_status,
            MaterialDownloadStatus::Ready
        );
        assert_eq!(repository.catalog_state().unwrap().local_catalog_version, 2);
    }

    #[test]
    fn changed_false_and_invalid_full_catalog_do_not_replace_snapshot() {
        let database = Database::open_in_memory().unwrap();
        database.migrate().unwrap();
        let repository = TransitionMaterialRepository::new(database);
        repository
            .apply_catalog(
                &TransitionMaterialCatalogResponse {
                    catalog_version: 1,
                    changed: true,
                    cdn_base_url: "https://cdn.example/materials/".to_owned(),
                    materials: Some(vec![material("tm_test", 1)]),
                },
                false,
            )
            .unwrap();
        repository
            .apply_catalog(
                &TransitionMaterialCatalogResponse {
                    catalog_version: 1,
                    changed: false,
                    cdn_base_url: "https://cdn.example/materials/".to_owned(),
                    materials: None,
                },
                false,
            )
            .unwrap();
        let mut duplicate = material("tm_duplicate", 1);
        duplicate.video_path = "https://untrusted.example/a.mp4".to_owned();
        assert!(
            repository
                .apply_catalog(
                    &TransitionMaterialCatalogResponse {
                        catalog_version: 2,
                        changed: true,
                        cdn_base_url: "https://cdn.example/materials/".to_owned(),
                        materials: Some(vec![duplicate]),
                    },
                    false,
                )
                .is_err()
        );
        assert_eq!(repository.catalog_state().unwrap().local_catalog_version, 1);
        assert_eq!(repository.list_current_materials().unwrap().len(), 1);
    }

    #[test]
    fn manual_boundary_lock_and_deleted_segment_keep_audit_record() {
        let database = Database::open_in_memory().unwrap();
        database.migrate().unwrap();
        seed_clip_project(&database);
        let repository = TransitionMaterialRepository::new(database.clone());
        repository
            .apply_catalog(
                &TransitionMaterialCatalogResponse {
                    catalog_version: 1,
                    changed: true,
                    cdn_base_url: "https://cdn.example/materials/".to_owned(),
                    materials: Some(vec![material("tm_test", 1)]),
                },
                false,
            )
            .unwrap();
        assert_eq!(repository.reconcile_boundaries(1).unwrap().len(), 2);
        let selected = repository
            .save_boundary_selection(BoundarySelectionInput {
                clip_project_id: 1,
                left_clip_segment_id: 11,
                right_clip_segment_id: 12,
                asset: Some(("tm_test", 1)),
                selection_source: BoundarySelectionSource::Manual,
                confidence: None,
                reason: Some("用户选择"),
                manually_locked: true,
            })
            .unwrap();
        assert!(selected.manually_locked);
        let detail = AiRepository::new(database.clone())
            .get_clip_project(1)
            .unwrap();
        assert_eq!(detail.project_duration_ms, 5_000);
        assert_eq!(detail.timeline_units.len(), 4);
        let bridge_unit = detail
            .timeline_units
            .iter()
            .find(|unit| unit.kind == AiClipTimelineUnitKind::Bridge)
            .unwrap();
        assert_eq!(
            (bridge_unit.project_start_ms, bridge_unit.project_end_ms),
            (1_000, 3_000)
        );
        assert_eq!(
            detail
                .subtitles
                .iter()
                .find(|subtitle| subtitle.clip_segment_id == 12)
                .unwrap()
                .project_start_ms,
            3_000
        );
        assert!(
            !detail
                .subtitle_frames
                .iter()
                .any(|frame| { frame.project_start_ms <= 1_500 && frame.project_end_ms > 1_500 })
        );
        assert!(matches!(
            repository.save_boundary_selection(BoundarySelectionInput {
                clip_project_id: 1,
                left_clip_segment_id: 11,
                right_clip_segment_id: 12,
                asset: Some(("tm_test", 1)),
                selection_source: BoundarySelectionSource::Agent,
                confidence: Some(0.99),
                reason: Some("模型建议"),
                manually_locked: false,
            }),
            Err(TransitionMaterialError::ManuallyLocked)
        ));

        let automatic_boundary = repository
            .list_boundaries(1)
            .unwrap()
            .into_iter()
            .find(|item| item.left_stable_id == 12 && item.right_stable_id == 13)
            .unwrap();
        let low_score = BoundaryAgentScoreInput {
            asset_key: "tm_test",
            asset_version: 1,
            total_score: 7.4,
            scene_score: 7.5,
            continuity_score: 7.3,
            rhythm_score: 7.2,
            material_score: 7.6,
            reason: "分数不足，仅建议",
        };
        let suggestion = repository
            .save_agent_score_suggestion(
                automatic_boundary.id,
                Some(&low_score),
                false,
                low_score.reason,
                false,
            )
            .unwrap();
        assert_eq!(suggestion.asset_key, None);
        assert_eq!(suggestion.suggested_asset_key.as_deref(), Some("tm_test"));
        assert_eq!(suggestion.suggestion_confidence, None);
        assert_eq!(suggestion.suggestion_score, Some(7.4));
        let high_score = BoundaryAgentScoreInput {
            total_score: 8.0,
            scene_score: 8.1,
            continuity_score: 8.0,
            rhythm_score: 7.9,
            material_score: 8.2,
            reason: "达到冻结阈值",
            ..low_score
        };
        let applied = repository
            .save_agent_score_suggestion(
                automatic_boundary.id,
                Some(&high_score),
                false,
                high_score.reason,
                true,
            )
            .unwrap();
        assert_eq!(applied.asset_key.as_deref(), Some("tm_test"));
        assert_eq!(applied.selection_source, BoundarySelectionSource::Agent);
        assert_eq!(applied.score, Some(8.0));

        database
            .connection()
            .unwrap()
            .execute("DELETE FROM ai_clip_segments WHERE id = 11", [])
            .unwrap();
        repository.reconcile_boundaries(1).unwrap();
        let audit = repository.boundary(selected.id).unwrap();
        assert!(!audit.active);
        assert_eq!(audit.left_clip_segment_id, None);
        assert_eq!(audit.left_stable_id, 11);
        assert_eq!(audit.asset_key.as_deref(), Some("tm_test"));
    }

    #[test]
    fn latest_completed_match_run_ignores_newer_failed_audit() {
        let database = Database::open_in_memory().unwrap();
        database.migrate().unwrap();
        seed_clip_project(&database);
        let repository = TransitionMaterialRepository::new(database);

        let first = repository
            .begin_match_run(
                1, 1, "deepseek", "model", "match-v1", "score-v1", 7, 2, "first",
            )
            .unwrap();
        repository
            .finish_match_run(
                first,
                true,
                "completed",
                TransitionMatchRunOutcome {
                    matched_boundaries: 1,
                    auto_applied: 0,
                    suggestions: 1,
                    none_suggestions: 0,
                    failed_boundaries: 0,
                    token_usage: 40,
                },
                None,
            )
            .unwrap();
        let latest_completed = repository
            .begin_match_run(
                1, 2, "deepseek", "model", "match-v1", "score-v1", 8, 2, "second",
            )
            .unwrap();
        repository
            .finish_match_run(
                latest_completed,
                true,
                "completed",
                TransitionMatchRunOutcome {
                    matched_boundaries: 2,
                    auto_applied: 1,
                    suggestions: 1,
                    none_suggestions: 0,
                    failed_boundaries: 0,
                    token_usage: 80,
                },
                None,
            )
            .unwrap();
        let failed = repository
            .begin_match_run(
                1, 3, "deepseek", "model", "match-v1", "score-v1", 9, 2, "third",
            )
            .unwrap();
        repository
            .finish_match_run(
                failed,
                false,
                "failed",
                TransitionMatchRunOutcome::default(),
                Some(("provider", "failed")),
            )
            .unwrap();

        assert_eq!(
            repository.latest_completed_match_run(1).unwrap(),
            Some(TransitionMatchRunReview {
                run_id: latest_completed,
                threshold: 8,
                matched: 2,
                auto_applied: 1,
                suggestions: 1,
                none_suggestions: 0,
                token_usage: 80,
            })
        );
    }
}
