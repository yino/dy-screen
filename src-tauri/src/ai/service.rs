//! 用户主动创建和冻结多视频 AI 项目的领域服务。
//!
//! 构造、查询或打开页面不会自动读取视频或启动 ASR；只有显式开始操作会进入 preflight。

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use dy_screen::asr::{AsrError, FrozenMediaSource, MediaInspector};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::database::{Database, DatabaseError};

use super::{
    AiInputSourceKind, AiInputStatus, AiProject, AiProjectDetail, AiProjectInput, AiRepository,
    AiRepositoryError, NewAiProjectInput, RecognitionProfile, SourceFingerprint,
};

/// 系统文件选择器产生的后端授权项。普通前端 DTO 不能只提交任意路径替代授权标识。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustedLocalFile {
    grant_id: String,
    path: PathBuf,
}

impl TrustedLocalFile {
    pub fn new(grant_id: impl Into<String>, path: PathBuf) -> Self {
        Self {
            grant_id: grant_id.into(),
            path,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ImportRejection {
    pub display_name: String,
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ImportBatchResult {
    pub added: Vec<AiProjectInput>,
    pub rejected: Vec<ImportRejection>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SessionImportResult {
    pub added: Vec<AiProjectInput>,
    pub added_count: usize,
    pub duplicate_count: usize,
    pub unavailable_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PreflightReport {
    pub ready: bool,
    pub engine_id: String,
    pub engine_version: String,
    pub model_id: String,
    pub model_version: String,
    pub platform_supported: bool,
    pub sidecars_ready: bool,
    pub models_ready: bool,
    pub memory_ready: bool,
    pub disk_ready: bool,
    pub message: String,
}

#[async_trait]
pub trait AiPreflight: Send + Sync {
    async fn check(&self) -> std::result::Result<PreflightReport, AsrError>;
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AiProjectSummary {
    pub total_inputs: usize,
    pub valid_inputs: usize,
    pub unavailable_inputs: usize,
    pub total_duration_ms: u64,
    pub engine_id: String,
    pub model_id: String,
    pub environment_ready: bool,
    pub environment_message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AiSessionOption {
    pub session_id: i64,
    pub streamer_name: String,
    pub started_at: String,
    pub ended_at: String,
    pub video_count: usize,
    pub total_duration_ms: u64,
    pub unavailable_video_count: usize,
}

#[derive(Debug, Error)]
pub enum ServiceError {
    #[error(transparent)]
    Repository(#[from] AiRepositoryError),
    #[error(transparent)]
    Database(#[from] DatabaseError),
    #[error("媒体检查失败：{0}")]
    Media(String),
    #[error("选择的直播会话仍在录制，请等待直播结束")]
    SessionStillRecording,
    #[error("项目没有可分析的有效音轨")]
    NoValidInput,
    #[error("本地 ASR 环境未就绪：{0}")]
    Preflight(String),
}

/// 用户主动操作的 AI 项目服务。构造或读取服务不会自动创建项目或运行 ASR。
#[derive(Clone)]
pub struct AiProjectService {
    database: Database,
    repository: AiRepository,
    inspector: Arc<dyn MediaInspector>,
    preflight: Arc<dyn AiPreflight>,
}

impl AiProjectService {
    pub fn new(
        database: Database,
        inspector: Arc<dyn MediaInspector>,
        preflight: Arc<dyn AiPreflight>,
    ) -> Self {
        Self {
            repository: AiRepository::new(database.clone()),
            database,
            inspector,
            preflight,
        }
    }

    /// 打开 AI 页面只读取项目列表，不读取媒体或创建任务。
    pub fn open_workspace(&self) -> Result<Vec<AiProject>, ServiceError> {
        Ok(self.repository.list_projects()?)
    }

    pub fn list_completed_sessions(
        &self,
        limit: usize,
    ) -> Result<Vec<AiSessionOption>, ServiceError> {
        self.database
            .list_completed_sessions(limit)?
            .into_iter()
            .map(|session| {
                let streamer = self.database.get_streamer(session.streamer_id)?;
                let videos = self.database.list_session_videos(session.id)?;
                Ok(AiSessionOption {
                    session_id: session.id,
                    streamer_name: streamer.name,
                    started_at: session.started_at,
                    ended_at: session.ended_at.unwrap_or_default(),
                    video_count: videos.len(),
                    total_duration_ms: videos
                        .iter()
                        .filter_map(|video| video.duration_seconds)
                        .filter_map(|seconds| u64::try_from(seconds).ok())
                        .map(|seconds| seconds.saturating_mul(1_000))
                        .sum(),
                    unavailable_video_count: videos
                        .iter()
                        .filter(|video| {
                            video.status != "complete" || !Path::new(&video.path).is_file()
                        })
                        .count(),
                })
            })
            .collect()
    }

    pub fn create_draft(
        &self,
        name: &str,
        profile: &RecognitionProfile,
    ) -> Result<AiProject, ServiceError> {
        Ok(self.repository.create_project(name, profile)?)
    }

    pub async fn import_local_files(
        &self,
        project_id: i64,
        grants: Vec<TrustedLocalFile>,
        cancellation: CancellationToken,
    ) -> Result<ImportBatchResult, ServiceError> {
        let detail = self.repository.get_project(project_id)?;
        let mut position = next_input_position(&detail.inputs);
        let mut result = ImportBatchResult {
            added: Vec::new(),
            rejected: Vec::new(),
        };
        for grant in grants {
            let display_name = display_name(&grant.path);
            if grant.grant_id.trim().is_empty() {
                result.rejected.push(ImportRejection {
                    display_name,
                    code: "untrusted_file_grant".to_owned(),
                    message: "本地视频必须通过系统文件选择器添加".to_owned(),
                });
                continue;
            }
            let source = match FrozenMediaSource::from_path(&grant.path) {
                Ok(source) => source,
                Err(error) => {
                    result.rejected.push(rejection(display_name, error));
                    continue;
                }
            };
            let inspection = match self
                .inspector
                .inspect(&source, cancellation.child_token())
                .await
            {
                Ok(inspection) => inspection,
                Err(error) => {
                    result.rejected.push(rejection(display_name, error));
                    continue;
                }
            };
            let fingerprint = source_fingerprint(&source, None)?;
            let input = NewAiProjectInput {
                position,
                source_kind: AiInputSourceKind::LocalFile,
                video_id: None,
                display_name: display_name.clone(),
                source_path: source.path.to_string_lossy().into_owned(),
                source_fingerprint: fingerprint,
                duration_ms: Some(inspection.duration_ms),
                audio_present: Some(inspection.audio_present),
            };
            match self.repository.add_input(project_id, input) {
                Ok(input) => {
                    result.added.push(input);
                    position += 1;
                }
                Err(AiRepositoryError::DuplicateInput) => result.rejected.push(ImportRejection {
                    display_name,
                    code: "duplicate_input".to_owned(),
                    message: "该视频已经存在于当前项目".to_owned(),
                }),
                Err(error) => return Err(error.into()),
            }
        }
        Ok(result)
    }

    /// 将一个已结束会话的完成分片展开为有序输入，缺失分片仍保留并标记失败。
    pub async fn select_completed_session(
        &self,
        project_id: i64,
        session_id: i64,
        cancellation: CancellationToken,
    ) -> Result<SessionImportResult, ServiceError> {
        let session = self.database.get_session(session_id)?;
        if session.ended_at.is_none() {
            return Err(ServiceError::SessionStillRecording);
        }
        let videos = self.database.list_session_videos(session_id)?;
        let existing = self.repository.get_project(project_id)?.inputs;
        let mut position = next_input_position(&existing);
        let mut existing_video_ids = existing
            .iter()
            .filter_map(|input| input.video_id)
            .collect::<HashSet<_>>();
        let mut existing_hashes = existing
            .into_iter()
            .map(|input| input.source_fingerprint_hash)
            .collect::<HashSet<_>>();
        let mut result = SessionImportResult {
            added: Vec::new(),
            added_count: 0,
            duplicate_count: 0,
            unavailable_count: 0,
        };
        for video in videos {
            if existing_video_ids.contains(&video.id) {
                result.duplicate_count += 1;
                continue;
            }
            let path = PathBuf::from(&video.path);
            let source = FrozenMediaSource::from_path(&path);
            let (fingerprint, duration_ms, audio_present, unavailable) = match source {
                Ok(source) if video.status == "complete" && video.audio_present != Some(false) => {
                    match self
                        .inspector
                        .inspect(&source, cancellation.child_token())
                        .await
                    {
                        Ok(inspection) if inspection.audio_present => (
                            source_fingerprint(&source, Some(video.id))?,
                            Some(inspection.duration_ms),
                            Some(true),
                            None,
                        ),
                        Ok(inspection) => (
                            source_fingerprint(&source, Some(video.id))?,
                            Some(inspection.duration_ms),
                            Some(false),
                            Some((
                                "session_video_no_audio".to_owned(),
                                "录像分片没有可识别音轨".to_owned(),
                            )),
                        ),
                        Err(error) => (
                            fallback_video_fingerprint(&video),
                            video_duration_ms(&video),
                            video.audio_present,
                            Some((error.code, error.safe_message)),
                        ),
                    }
                }
                Ok(source) if video.status == "complete" => (
                    source_fingerprint(&source, Some(video.id))?,
                    video_duration_ms(&video),
                    Some(false),
                    Some((
                        "session_video_no_audio".to_owned(),
                        "录像分片没有可识别音轨".to_owned(),
                    )),
                ),
                _ => (
                    fallback_video_fingerprint(&video),
                    video_duration_ms(&video),
                    video.audio_present,
                    Some((
                        "session_video_unavailable".to_owned(),
                        "录像分片缺失或尚未完成".to_owned(),
                    )),
                ),
            };
            let fingerprint_hash = fingerprint.fingerprint()?;
            if existing_hashes.contains(&fingerprint_hash) {
                result.duplicate_count += 1;
                continue;
            }
            let input = match self.repository.add_input(
                project_id,
                NewAiProjectInput {
                    position,
                    source_kind: AiInputSourceKind::VideoLibrary,
                    video_id: Some(video.id),
                    display_name: display_name(&path),
                    source_path: video.path,
                    source_fingerprint: fingerprint,
                    duration_ms,
                    audio_present,
                },
            ) {
                Ok(input) => input,
                Err(AiRepositoryError::DuplicateInput) => {
                    result.duplicate_count += 1;
                    continue;
                }
                Err(error) => return Err(error.into()),
            };
            let input = if let Some((code, message)) = unavailable {
                self.repository.transition_input(
                    input.id,
                    AiInputStatus::Failed,
                    Some((&code, &message)),
                )?;
                result.unavailable_count += 1;
                self.repository.get_input(input.id)?
            } else {
                input
            };
            existing_video_ids.insert(video.id);
            existing_hashes.insert(fingerprint_hash);
            result.added.push(input);
            result.added_count += 1;
            position += 1;
        }
        Ok(result)
    }

    pub fn reorder_inputs(&self, project_id: i64, ordered_ids: &[i64]) -> Result<(), ServiceError> {
        Ok(self.repository.reorder_inputs(project_id, ordered_ids)?)
    }

    pub fn remove_input(&self, project_id: i64, input_id: i64) -> Result<(), ServiceError> {
        Ok(self.repository.remove_input(project_id, input_id)?)
    }

    pub async fn summary(&self, project_id: i64) -> Result<AiProjectSummary, ServiceError> {
        let detail = self.repository.get_project(project_id)?;
        let environment = self
            .preflight
            .check()
            .await
            .map_err(|error| ServiceError::Preflight(error.safe_message))?;
        Ok(summary_from(&detail, &environment))
    }

    /// preflight 全部通过后才冻结输入并创建排队状态；失败时项目保持草稿。
    pub async fn start_analysis(&self, project_id: i64) -> Result<AiProject, ServiceError> {
        let detail = self.repository.get_project(project_id)?;
        let valid_inputs = detail
            .inputs
            .iter()
            .filter(|input| {
                input.status == AiInputStatus::Pending
                    && input.audio_present == Some(true)
                    && input.duration_ms.is_some()
            })
            .count();
        if valid_inputs == 0 {
            return Err(ServiceError::NoValidInput);
        }
        let report = self
            .preflight
            .check()
            .await
            .map_err(|error| ServiceError::Preflight(error.safe_message))?;
        if !report.ready {
            return Err(ServiceError::Preflight(report.message));
        }
        Ok(self.repository.freeze_project(project_id)?)
    }
}

fn summary_from(detail: &AiProjectDetail, environment: &PreflightReport) -> AiProjectSummary {
    AiProjectSummary {
        total_inputs: detail.inputs.len(),
        valid_inputs: detail
            .inputs
            .iter()
            .filter(|input| {
                input.status == AiInputStatus::Pending
                    && input.audio_present == Some(true)
                    && input.duration_ms.is_some()
            })
            .count(),
        unavailable_inputs: detail
            .inputs
            .iter()
            .filter(|input| input.status == AiInputStatus::Failed)
            .count(),
        total_duration_ms: detail
            .inputs
            .iter()
            .filter_map(|input| input.duration_ms)
            .sum(),
        engine_id: environment.engine_id.clone(),
        model_id: environment.model_id.clone(),
        environment_ready: environment.ready,
        environment_message: environment.message.clone(),
    }
}

fn source_fingerprint(
    source: &FrozenMediaSource,
    video_id: Option<i64>,
) -> Result<SourceFingerprint, ServiceError> {
    Ok(SourceFingerprint {
        normalized_path: source.path.to_string_lossy().into_owned(),
        size_bytes: source.size_bytes,
        modified_at_ms: i64::try_from(source.modified_at_ms)
            .map_err(|_| ServiceError::Media("视频修改时间超出支持范围".to_owned()))?,
        video_id,
    })
}

fn fallback_video_fingerprint(video: &crate::domain::Video) -> SourceFingerprint {
    SourceFingerprint {
        normalized_path: video.path.clone(),
        size_bytes: video.size_bytes.max(0) as u64,
        modified_at_ms: 0,
        video_id: Some(video.id),
    }
}

fn video_duration_ms(video: &crate::domain::Video) -> Option<u64> {
    video
        .duration_seconds
        .and_then(|value| u64::try_from(value).ok())
        .map(|value| value.saturating_mul(1_000))
}

fn display_name(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("未命名视频")
        .to_owned()
}

fn next_input_position(inputs: &[AiProjectInput]) -> i64 {
    inputs
        .iter()
        .map(|input| input.position)
        .max()
        .map_or(0, |position| position.saturating_add(1))
}

fn rejection(display_name: String, error: AsrError) -> ImportRejection {
    ImportRejection {
        display_name,
        code: error.code,
        message: error.safe_message,
    }
}
