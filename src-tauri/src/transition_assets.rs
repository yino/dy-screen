//! 版本化转场素材的受控下载、完整性校验和 WebView 兼容预览。

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::{Mutex as AsyncMutex, watch};
use tokio_util::sync::CancellationToken;

use crate::preview::{
    FfmpegPreviewExecutor, PreviewExecution, PreviewExecutor, PreviewGenerationMethod,
    PreviewProgressCallback, PreviewRequest, PreviewState,
};
use crate::transition_materials::{
    MaterialDownloadStatus, TransitionMaterial, TransitionMaterialDownload,
    TransitionMaterialRepository,
};

const MAX_DOWNLOAD_BYTES: u64 = 256 * 1024 * 1024;
const MAX_THUMBNAIL_BYTES: u64 = 10 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MaterialAssetFailure {
    pub code: String,
    pub message: String,
}

impl MaterialAssetFailure {
    fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for MaterialAssetFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for MaterialAssetFailure {}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MaterialAssetSnapshot {
    pub asset_key: String,
    pub asset_version: i64,
    pub state: MaterialDownloadStatus,
    pub media_url: Option<String>,
    pub generated: bool,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
}

#[derive(Clone, Default)]
pub struct MaterialAssetRegistry {
    paths: Arc<Mutex<HashMap<String, PathBuf>>>,
}

impl MaterialAssetRegistry {
    fn insert(&self, token: String, path: PathBuf) -> Result<(), MaterialAssetFailure> {
        self.paths
            .lock()
            .map_err(|_| MaterialAssetFailure::new("state_unavailable", "素材预览状态不可用"))?
            .insert(token, path);
        Ok(())
    }

    pub fn resolve(&self, token: &str) -> Option<PathBuf> {
        self.paths
            .lock()
            .ok()
            .and_then(|paths| paths.get(token).cloned())
    }
}

pub fn serve_material_asset(
    registry: &MaterialAssetRegistry,
    request: &tauri::http::Request<Vec<u8>>,
) -> tauri::http::Response<Vec<u8>> {
    let token = request.uri().path().trim_start_matches('/');
    let Some(path) = registry.resolve(token) else {
        return response_with_status(tauri::http::StatusCode::NOT_FOUND, Vec::new());
    };
    let Ok(metadata) = std::fs::symlink_metadata(&path) else {
        return response_with_status(tauri::http::StatusCode::NOT_FOUND, Vec::new());
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return response_with_status(tauri::http::StatusCode::FORBIDDEN, Vec::new());
    }
    let file_size = metadata.len();
    let requested_range = request
        .headers()
        .get(tauri::http::header::RANGE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| parse_byte_range(value, file_size));
    let (start, end, status) = requested_range
        .map(|(start, end)| (start, end, tauri::http::StatusCode::PARTIAL_CONTENT))
        .unwrap_or((0, file_size.saturating_sub(1), tauri::http::StatusCode::OK));
    let length = if file_size == 0 {
        0
    } else {
        end.saturating_sub(start).saturating_add(1)
    };
    let mut file = match std::fs::File::open(&path) {
        Ok(file) => file,
        Err(_) => return response_with_status(tauri::http::StatusCode::NOT_FOUND, Vec::new()),
    };
    if file.seek(SeekFrom::Start(start)).is_err() {
        return response_with_status(tauri::http::StatusCode::INTERNAL_SERVER_ERROR, Vec::new());
    }
    let Ok(length_usize) = usize::try_from(length) else {
        return response_with_status(tauri::http::StatusCode::RANGE_NOT_SATISFIABLE, Vec::new());
    };
    let mut body = vec![0; length_usize];
    if file.read_exact(&mut body).is_err() {
        return response_with_status(tauri::http::StatusCode::INTERNAL_SERVER_ERROR, Vec::new());
    }
    let mime = match path.extension().and_then(|value| value.to_str()) {
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("webp") => "image/webp",
        _ => "video/mp4",
    };
    let mut builder = tauri::http::Response::builder()
        .status(status)
        .header(tauri::http::header::CONTENT_TYPE, mime)
        .header(tauri::http::header::ACCEPT_RANGES, "bytes")
        .header(tauri::http::header::CONTENT_LENGTH, length.to_string());
    if status == tauri::http::StatusCode::PARTIAL_CONTENT {
        builder = builder.header(
            tauri::http::header::CONTENT_RANGE,
            format!("bytes {start}-{end}/{file_size}"),
        );
    }
    builder.body(body).unwrap_or_else(|_| {
        response_with_status(tauri::http::StatusCode::INTERNAL_SERVER_ERROR, Vec::new())
    })
}

fn response_with_status(
    status: tauri::http::StatusCode,
    body: Vec<u8>,
) -> tauri::http::Response<Vec<u8>> {
    tauri::http::Response::builder()
        .status(status)
        .body(body)
        .unwrap_or_else(|_| tauri::http::Response::new(Vec::new()))
}

fn parse_byte_range(value: &str, file_size: u64) -> Option<(u64, u64)> {
    let value = value.strip_prefix("bytes=")?;
    if value.contains(',') || file_size == 0 {
        return None;
    }
    let (start, end) = value.split_once('-')?;
    let start = start.parse::<u64>().ok()?;
    let end = if end.is_empty() {
        file_size.saturating_sub(1)
    } else {
        end.parse::<u64>().ok()?.min(file_size.saturating_sub(1))
    };
    (start <= end && start < file_size).then_some((start, end))
}

#[derive(Debug, Clone)]
pub struct TransitionMaterialCache {
    root: PathBuf,
}

impl TransitionMaterialCache {
    pub fn new(root: PathBuf) -> Result<Self, MaterialAssetFailure> {
        std::fs::create_dir_all(&root)
            .map_err(|_| MaterialAssetFailure::new("cache_io", "无法创建转场素材缓存目录"))?;
        let root = std::fs::canonicalize(root)
            .map_err(|_| MaterialAssetFailure::new("cache_io", "无法读取转场素材缓存目录"))?;
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn source_relative_path(&self, material: &TransitionMaterial) -> PathBuf {
        PathBuf::from(&material.asset_key)
            .join(material.asset_version.to_string())
            .join("source.mp4")
    }

    pub fn preview_relative_path(&self, material: &TransitionMaterial) -> PathBuf {
        PathBuf::from(&material.asset_key)
            .join(material.asset_version.to_string())
            .join("preview.mp4")
    }

    pub fn thumbnail_relative_path(
        &self,
        material: &TransitionMaterial,
        extension: &str,
    ) -> PathBuf {
        PathBuf::from(&material.asset_key)
            .join(material.asset_version.to_string())
            .join(format!("thumbnail.{extension}"))
    }

    pub fn resolve(&self, relative: &Path) -> Result<PathBuf, MaterialAssetFailure> {
        if relative.is_absolute()
            || relative.components().any(|component| {
                matches!(
                    component,
                    std::path::Component::ParentDir
                        | std::path::Component::RootDir
                        | std::path::Component::Prefix(_)
                )
            })
        {
            return Err(MaterialAssetFailure::new(
                "cache_path_invalid",
                "转场素材缓存路径无效",
            ));
        }
        Ok(self.root.join(relative))
    }
}

#[async_trait]
pub trait MaterialFetcher: Send + Sync {
    async fn fetch(
        &self,
        url: &str,
        maximum_bytes: u64,
        cancellation: CancellationToken,
    ) -> Result<Vec<u8>, MaterialAssetFailure>;
}

#[derive(Clone)]
pub struct ReqwestMaterialFetcher {
    client: reqwest::Client,
}

impl ReqwestMaterialFetcher {
    pub fn new() -> Result<Self, MaterialAssetFailure> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .map_err(|_| {
                MaterialAssetFailure::new("download_configuration", "无法初始化素材下载器")
            })?;
        Ok(Self { client })
    }
}

#[async_trait]
impl MaterialFetcher for ReqwestMaterialFetcher {
    async fn fetch(
        &self,
        url: &str,
        maximum_bytes: u64,
        cancellation: CancellationToken,
    ) -> Result<Vec<u8>, MaterialAssetFailure> {
        let mut response = tokio::select! {
            _ = cancellation.cancelled() => {
                return Err(MaterialAssetFailure::new("cancelled", "素材下载已取消"));
            }
            response = self.client.get(url).send() => response.map_err(|_| {
                MaterialAssetFailure::new("download_failed", "无法下载转场素材")
            })?
        };
        if !response.status().is_success()
            || response
                .content_length()
                .is_some_and(|length| length > maximum_bytes)
        {
            return Err(MaterialAssetFailure::new(
                "download_rejected",
                "素材服务器返回了无效文件",
            ));
        }
        let mut bytes = Vec::new();
        loop {
            let chunk = tokio::select! {
                _ = cancellation.cancelled() => {
                    return Err(MaterialAssetFailure::new("cancelled", "素材下载已取消"));
                }
                chunk = response.chunk() => chunk.map_err(|_| {
                    MaterialAssetFailure::new("download_failed", "素材下载连接已中断")
                })?
            };
            let Some(chunk) = chunk else { break };
            let next_size = bytes.len().saturating_add(chunk.len()) as u64;
            if next_size > maximum_bytes {
                return Err(MaterialAssetFailure::new(
                    "download_too_large",
                    "转场素材超过客户端允许大小",
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    }
}

type DownloadResult = Result<TransitionMaterialDownload, MaterialAssetFailure>;
type InflightDownloads = HashMap<(String, i64), watch::Receiver<Option<DownloadResult>>>;

#[derive(Clone)]
pub struct TransitionMaterialAssetService {
    repository: TransitionMaterialRepository,
    cache: TransitionMaterialCache,
    fetcher: Arc<dyn MaterialFetcher>,
    preview_executor: Arc<dyn PreviewExecutor>,
    inflight: Arc<AsyncMutex<InflightDownloads>>,
    cancellation: CancellationToken,
    registry: MaterialAssetRegistry,
}

#[derive(Debug, Clone)]
pub struct VerifiedTransitionMaterialSource {
    pub material: TransitionMaterial,
    pub source_path: PathBuf,
}

impl TransitionMaterialAssetService {
    pub fn new(
        repository: TransitionMaterialRepository,
        cache: TransitionMaterialCache,
        cancellation: CancellationToken,
        registry: MaterialAssetRegistry,
    ) -> Result<Self, MaterialAssetFailure> {
        Ok(Self::with_dependencies(
            repository,
            cache,
            Arc::new(ReqwestMaterialFetcher::new()?),
            Arc::new(FfmpegPreviewExecutor),
            cancellation,
            registry,
        ))
    }

    pub fn with_dependencies(
        repository: TransitionMaterialRepository,
        cache: TransitionMaterialCache,
        fetcher: Arc<dyn MaterialFetcher>,
        preview_executor: Arc<dyn PreviewExecutor>,
        cancellation: CancellationToken,
        registry: MaterialAssetRegistry,
    ) -> Self {
        Self {
            repository,
            cache,
            fetcher,
            preview_executor,
            inflight: Arc::new(AsyncMutex::new(HashMap::new())),
            cancellation,
            registry,
        }
    }

    pub fn cache(&self) -> &TransitionMaterialCache {
        &self.cache
    }

    pub fn authorized_path(&self, token: &str) -> Option<PathBuf> {
        self.registry.resolve(token)
    }

    pub async fn download_source(&self, asset_key: &str, asset_version: i64) -> DownloadResult {
        let material = self
            .repository
            .material(asset_key, asset_version)
            .map_err(|_| MaterialAssetFailure::new("material_not_found", "找不到转场素材"))?;
        if let Ok(download) = self.repository.download(asset_key, asset_version)
            && download.source_status == MaterialDownloadStatus::Ready
            && let Some(relative) = download.source_relative_path.as_deref()
            && let Ok(path) = self.cache.resolve(Path::new(relative))
            && source_matches(&path, &material).await
        {
            return Ok(download);
        }

        let key = (asset_key.to_owned(), asset_version);
        let (leader, mut receiver, sender) = {
            let mut inflight = self.inflight.lock().await;
            if let Some(receiver) = inflight.get(&key) {
                (false, receiver.clone(), None)
            } else {
                let (sender, receiver) = watch::channel(None);
                inflight.insert(key.clone(), receiver.clone());
                (true, receiver, Some(sender))
            }
        };
        if !leader {
            loop {
                if let Some(result) = receiver.borrow().clone() {
                    return result;
                }
                receiver.changed().await.map_err(|_| {
                    MaterialAssetFailure::new("download_failed", "素材下载任务异常结束")
                })?;
            }
        }

        let result = self.download_source_inner(&material).await;
        if let Some(sender) = sender {
            let _ = sender.send(Some(result.clone()));
        }
        self.inflight.lock().await.remove(&key);
        result
    }

    pub async fn source_for_export(
        &self,
        asset_key: &str,
        asset_version: i64,
    ) -> Result<VerifiedTransitionMaterialSource, MaterialAssetFailure> {
        let material = self
            .repository
            .material(asset_key, asset_version)
            .map_err(|_| MaterialAssetFailure::new("material_not_found", "找不到转场素材"))?;
        let download = self
            .repository
            .download(asset_key, asset_version)
            .map_err(repository_failure)?;
        if download.source_status != MaterialDownloadStatus::Ready {
            return Err(MaterialAssetFailure::new(
                "material_not_ready",
                format!("转场素材“{}”尚未下载完成，请重试素材下载", material.title),
            ));
        }
        let relative = download.source_relative_path.as_deref().ok_or_else(|| {
            MaterialAssetFailure::new("material_not_ready", "转场素材缓存记录不完整，请重试下载")
        })?;
        let source_path = self.cache.resolve(Path::new(relative))?;
        if !source_matches(&source_path, &material).await {
            return Err(MaterialAssetFailure::new(
                "material_integrity_failed",
                format!("转场素材“{}”完整性校验失败，请重新下载", material.title),
            ));
        }
        Ok(VerifiedTransitionMaterialSource {
            material,
            source_path,
        })
    }

    async fn download_source_inner(&self, material: &TransitionMaterial) -> DownloadResult {
        self.repository
            .set_source_download(
                &material.asset_key,
                material.asset_version,
                MaterialDownloadStatus::Downloading,
                None,
                None,
                None,
            )
            .map_err(repository_failure)?;
        let relative = self.cache.source_relative_path(material);
        let target = self.cache.resolve(&relative)?;
        let parent = target.parent().ok_or_else(|| {
            MaterialAssetFailure::new("cache_path_invalid", "转场素材缓存路径无效")
        })?;
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|_| MaterialAssetFailure::new("cache_io", "无法创建素材版本目录"))?;
        let temporary = target.with_extension("part.mp4");
        let result = async {
            let bytes = self
                .fetcher
                .fetch(
                    &material.video_url,
                    material.size_bytes.min(MAX_DOWNLOAD_BYTES),
                    self.cancellation.child_token(),
                )
                .await?;
            if bytes.len() as u64 != material.size_bytes {
                return Err(MaterialAssetFailure::new(
                    "size_mismatch",
                    "素材文件大小校验失败",
                ));
            }
            let digest = hex::encode(Sha256::digest(&bytes));
            if digest != material.sha256 {
                return Err(MaterialAssetFailure::new(
                    "sha256_mismatch",
                    "素材完整性校验失败",
                ));
            }
            tokio::fs::write(&temporary, bytes)
                .await
                .map_err(|_| MaterialAssetFailure::new("cache_io", "无法写入素材临时文件"))?;
            if target.exists() {
                tokio::fs::remove_file(&target)
                    .await
                    .map_err(|_| MaterialAssetFailure::new("cache_io", "无法替换旧素材缓存"))?;
            }
            tokio::fs::rename(&temporary, &target)
                .await
                .map_err(|_| MaterialAssetFailure::new("cache_io", "无法发布素材缓存"))?;
            let relative = relative.to_string_lossy().into_owned();
            self.repository
                .set_source_download(
                    &material.asset_key,
                    material.asset_version,
                    MaterialDownloadStatus::Ready,
                    Some(&relative),
                    Some(material.size_bytes),
                    None,
                )
                .map_err(repository_failure)
        }
        .await;
        if let Err(error) = &result {
            let _ = tokio::fs::remove_file(&temporary).await;
            let _ = self.repository.set_source_download(
                &material.asset_key,
                material.asset_version,
                MaterialDownloadStatus::Failed,
                None,
                None,
                Some((&error.code, &error.message)),
            );
        }
        result
    }

    pub async fn prepare_preview(
        &self,
        asset_key: &str,
        asset_version: i64,
        ffmpeg_path: &Path,
        ffprobe_path: &Path,
    ) -> Result<MaterialAssetSnapshot, MaterialAssetFailure> {
        let material = self
            .repository
            .material(asset_key, asset_version)
            .map_err(|_| MaterialAssetFailure::new("material_not_found", "找不到转场素材"))?;
        let source_download = self.download_source(asset_key, asset_version).await?;
        let source_relative = source_download.source_relative_path.ok_or_else(|| {
            MaterialAssetFailure::new("source_unavailable", "转场素材尚未下载完成")
        })?;
        let source = self.cache.resolve(Path::new(&source_relative))?;
        let request = PreviewRequest {
            video_id: stable_preview_id(&material),
            source_path: source.to_string_lossy().into_owned(),
            source_status: "complete".to_owned(),
            ffmpeg_path: ffmpeg_path.to_string_lossy().into_owned(),
            ffprobe_path: ffprobe_path.to_string_lossy().into_owned(),
        };
        let cancellation = self.cancellation.child_token();
        if self
            .preview_executor
            .direct_playable(&request, cancellation.clone())
            .await
            .map_err(preview_failure)?
        {
            let token = self.authorize_path(&material, &source)?;
            self.repository
                .set_preview_download(
                    asset_key,
                    asset_version,
                    MaterialDownloadStatus::Ready,
                    Some(&source_relative),
                    None,
                )
                .map_err(repository_failure)?;
            return Ok(MaterialAssetSnapshot {
                asset_key: asset_key.to_owned(),
                asset_version,
                state: MaterialDownloadStatus::Ready,
                media_url: Some(material_asset_url(&token)),
                generated: false,
                error_code: None,
                error_message: None,
            });
        }

        let relative = self.cache.preview_relative_path(&material);
        let target = self.cache.resolve(&relative)?;
        if target.is_file() {
            let token = self.authorize_path(&material, &target)?;
            return Ok(MaterialAssetSnapshot {
                asset_key: asset_key.to_owned(),
                asset_version,
                state: MaterialDownloadStatus::Ready,
                media_url: Some(material_asset_url(&token)),
                generated: true,
                error_code: None,
                error_message: None,
            });
        }
        let temporary = target.with_extension("part.mp4");
        self.repository
            .set_preview_download(
                asset_key,
                asset_version,
                MaterialDownloadStatus::Transcoding,
                None,
                None,
            )
            .map_err(repository_failure)?;
        let progress: PreviewProgressCallback = Arc::new(|_state: PreviewState, _progress| {});
        let result = self
            .preview_executor
            .prepare(
                PreviewExecution {
                    request,
                    temporary_output: temporary.clone(),
                },
                progress,
                cancellation,
            )
            .await;
        match result {
            Ok(PreviewGenerationMethod::Remux | PreviewGenerationMethod::Transcode) => {
                if !temporary.is_file() {
                    return self.preview_failed(
                        &material,
                        &temporary,
                        MaterialAssetFailure::new("preview_missing", "FFmpeg 未生成转场预览文件"),
                    );
                }
                tokio::fs::rename(&temporary, &target)
                    .await
                    .map_err(|_| MaterialAssetFailure::new("cache_io", "无法发布转场预览缓存"))?;
                let relative = relative.to_string_lossy().into_owned();
                self.repository
                    .set_preview_download(
                        asset_key,
                        asset_version,
                        MaterialDownloadStatus::Ready,
                        Some(&relative),
                        None,
                    )
                    .map_err(repository_failure)?;
                let token = self.authorize_path(&material, &target)?;
                Ok(MaterialAssetSnapshot {
                    asset_key: asset_key.to_owned(),
                    asset_version,
                    state: MaterialDownloadStatus::Ready,
                    media_url: Some(material_asset_url(&token)),
                    generated: true,
                    error_code: None,
                    error_message: None,
                })
            }
            Err(error) => self.preview_failed(&material, &temporary, preview_failure(error)),
        }
    }

    pub async fn prepare_thumbnail(
        &self,
        asset_key: &str,
        asset_version: i64,
    ) -> Result<MaterialAssetSnapshot, MaterialAssetFailure> {
        let material = self
            .repository
            .material(asset_key, asset_version)
            .map_err(|_| MaterialAssetFailure::new("material_not_found", "找不到转场素材"))?;
        let remote_url = material
            .preview_url
            .as_deref()
            .or(material.cover_url.as_deref())
            .ok_or_else(|| {
                MaterialAssetFailure::new("thumbnail_unavailable", "该素材没有预览图")
            })?;
        for extension in ["png", "jpg", "webp"] {
            let relative = self.cache.thumbnail_relative_path(&material, extension);
            let path = self.cache.resolve(&relative)?;
            if path.is_file() {
                let token = self.authorize_path(&material, &path)?;
                return Ok(MaterialAssetSnapshot {
                    asset_key: asset_key.to_owned(),
                    asset_version,
                    state: MaterialDownloadStatus::Ready,
                    media_url: Some(material_asset_url(&token)),
                    generated: false,
                    error_code: None,
                    error_message: None,
                });
            }
        }
        let bytes = self
            .fetcher
            .fetch(
                remote_url,
                MAX_THUMBNAIL_BYTES,
                self.cancellation.child_token(),
            )
            .await?;
        let extension = image_extension(&bytes)
            .ok_or_else(|| MaterialAssetFailure::new("thumbnail_invalid", "素材预览图格式无效"))?;
        let relative = self.cache.thumbnail_relative_path(&material, extension);
        let target = self.cache.resolve(&relative)?;
        let parent = target.parent().ok_or_else(|| {
            MaterialAssetFailure::new("cache_path_invalid", "素材预览图缓存路径无效")
        })?;
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|_| MaterialAssetFailure::new("cache_io", "无法创建素材预览图目录"))?;
        let temporary = target.with_extension(format!("part.{extension}"));
        tokio::fs::write(&temporary, bytes)
            .await
            .map_err(|_| MaterialAssetFailure::new("cache_io", "无法写入素材预览图"))?;
        tokio::fs::rename(&temporary, &target)
            .await
            .map_err(|_| MaterialAssetFailure::new("cache_io", "无法发布素材预览图"))?;
        let token = self.authorize_path(&material, &target)?;
        Ok(MaterialAssetSnapshot {
            asset_key: asset_key.to_owned(),
            asset_version,
            state: MaterialDownloadStatus::Ready,
            media_url: Some(material_asset_url(&token)),
            generated: false,
            error_code: None,
            error_message: None,
        })
    }

    fn preview_failed(
        &self,
        material: &TransitionMaterial,
        temporary: &Path,
        error: MaterialAssetFailure,
    ) -> Result<MaterialAssetSnapshot, MaterialAssetFailure> {
        let _ = std::fs::remove_file(temporary);
        let _ = self.repository.set_preview_download(
            &material.asset_key,
            material.asset_version,
            MaterialDownloadStatus::Failed,
            None,
            Some((&error.code, &error.message)),
        );
        Err(error)
    }

    fn authorize_path(
        &self,
        material: &TransitionMaterial,
        path: &Path,
    ) -> Result<String, MaterialAssetFailure> {
        let canonical = std::fs::canonicalize(path)
            .map_err(|_| MaterialAssetFailure::new("cache_io", "无法读取转场素材缓存"))?;
        if !canonical.starts_with(self.cache.root()) {
            return Err(MaterialAssetFailure::new(
                "cache_path_invalid",
                "转场素材缓存超出受信目录",
            ));
        }
        let token = format!(
            "tm-{}-{}-{}",
            material.asset_key,
            material.asset_version,
            &hex::encode(Sha256::digest(canonical.to_string_lossy().as_bytes()))[..12]
        );
        self.registry.insert(token.clone(), canonical)?;
        Ok(token)
    }
}

fn material_asset_url(token: &str) -> String {
    #[cfg(target_os = "windows")]
    {
        format!("http://transition-material.localhost/{token}")
    }
    #[cfg(not(target_os = "windows"))]
    {
        format!("transition-material://localhost/{token}")
    }
}

fn image_extension(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]) {
        Some("png")
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("jpg")
    } else if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        Some("webp")
    } else {
        None
    }
}

async fn source_matches(path: &Path, material: &TransitionMaterial) -> bool {
    let Ok(metadata) = tokio::fs::metadata(path).await else {
        return false;
    };
    if !metadata.is_file() || metadata.len() != material.size_bytes {
        return false;
    }
    let Ok(bytes) = tokio::fs::read(path).await else {
        return false;
    };
    hex::encode(Sha256::digest(bytes)) == material.sha256
}

fn stable_preview_id(material: &TransitionMaterial) -> i64 {
    let digest = Sha256::digest(format!("{}:{}", material.asset_key, material.asset_version));
    let mut bytes = [0_u8; 8];
    bytes.copy_from_slice(&digest[..8]);
    i64::from_be_bytes(bytes).saturating_abs().saturating_neg()
}

fn repository_failure(error: impl std::fmt::Display) -> MaterialAssetFailure {
    MaterialAssetFailure::new("repository_error", error.to_string())
}

fn preview_failure(error: impl std::fmt::Display) -> MaterialAssetFailure {
    MaterialAssetFailure::new("preview_failed", error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{TransitionMaterialCatalogResponse, TransitionMaterialResponse};
    use crate::database::Database;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Clone)]
    struct FakeFetcher {
        bytes: Vec<u8>,
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl MaterialFetcher for FakeFetcher {
        async fn fetch(
            &self,
            _url: &str,
            _maximum_bytes: u64,
            _cancellation: CancellationToken,
        ) -> Result<Vec<u8>, MaterialAssetFailure> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            tokio::task::yield_now().await;
            Ok(self.bytes.clone())
        }
    }

    struct FakePreviewExecutor {
        direct: bool,
        fail: bool,
    }

    #[async_trait]
    impl PreviewExecutor for FakePreviewExecutor {
        async fn direct_playable(
            &self,
            _request: &PreviewRequest,
            _cancellation: CancellationToken,
        ) -> Result<bool, crate::preview::PreviewFailure> {
            Ok(self.direct)
        }

        async fn prepare(
            &self,
            execution: PreviewExecution,
            _progress: PreviewProgressCallback,
            _cancellation: CancellationToken,
        ) -> Result<PreviewGenerationMethod, crate::preview::PreviewFailure> {
            if self.fail {
                return Err(crate::preview::PreviewFailure::new(
                    "codec_missing",
                    "缺少 HEVC 解码器",
                ));
            }
            tokio::fs::write(execution.temporary_output, b"preview")
                .await
                .unwrap();
            Ok(PreviewGenerationMethod::Transcode)
        }
    }

    fn setup(
        bytes: &[u8],
        declared_sha: Option<String>,
        direct: bool,
        fail_preview: bool,
    ) -> (
        TransitionMaterialAssetService,
        tempfile::TempDir,
        Arc<AtomicUsize>,
    ) {
        let database = Database::open_in_memory().unwrap();
        database.migrate().unwrap();
        let repository = TransitionMaterialRepository::new(database);
        repository
            .apply_catalog(
                &TransitionMaterialCatalogResponse {
                    catalog_version: 1,
                    changed: true,
                    cdn_base_url: "https://cdn.example/materials/".to_owned(),
                    materials: Some(vec![TransitionMaterialResponse {
                        asset_key: "tm_test".to_owned(),
                        asset_version: 1,
                        title: "测试".to_owned(),
                        description: "测试素材".to_owned(),
                        tags: vec!["测试".to_owned()],
                        category: "neutral".to_owned(),
                        render_mode: "bridge".to_owned(),
                        video_path: "https://cdn.example/assets/test.mp4".to_owned(),
                        preview_path: None,
                        cover_path: None,
                        sha256: declared_sha.unwrap_or_else(|| hex::encode(Sha256::digest(bytes))),
                        size_bytes: bytes.len() as u64,
                        duration_ms: 2_000,
                        width: 1280,
                        height: 720,
                        fps: 30.0,
                        video_codec: if direct { "h264" } else { "hevc" }.to_owned(),
                        has_audio: true,
                        sort_order: 1,
                    }]),
                },
                false,
            )
            .unwrap();
        let directory = tempfile::tempdir().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let service = TransitionMaterialAssetService::with_dependencies(
            repository,
            TransitionMaterialCache::new(directory.path().join("materials")).unwrap(),
            Arc::new(FakeFetcher {
                bytes: bytes.to_vec(),
                calls: calls.clone(),
            }),
            Arc::new(FakePreviewExecutor {
                direct,
                fail: fail_preview,
            }),
            CancellationToken::new(),
            MaterialAssetRegistry::default(),
        );
        (service, directory, calls)
    }

    #[tokio::test]
    async fn concurrent_downloads_share_one_fetch_and_reuse_valid_cache() {
        let (service, _directory, calls) = setup(b"source-video", None, true, false);
        let (first, second) = tokio::join!(
            service.download_source("tm_test", 1),
            service.download_source("tm_test", 1)
        );
        assert_eq!(first.unwrap().source_status, MaterialDownloadStatus::Ready);
        assert_eq!(second.unwrap().source_status, MaterialDownloadStatus::Ready);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        service.download_source("tm_test", 1).await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn checksum_failure_removes_part_and_records_failure() {
        let (service, _directory, _calls) =
            setup(b"source-video", Some("a".repeat(64)), true, false);
        let error = service.download_source("tm_test", 1).await.unwrap_err();
        assert_eq!(error.code, "sha256_mismatch");
        let download = service.repository.download("tm_test", 1).unwrap();
        assert_eq!(download.source_status, MaterialDownloadStatus::Failed);
        assert!(
            !service
                .cache
                .root()
                .join("tm_test/1/source.part.mp4")
                .exists()
        );
    }

    #[tokio::test]
    async fn direct_h264_and_generated_hevc_preview_have_distinct_states() {
        let (direct, _directory, _calls) = setup(b"h264-source", None, true, false);
        let snapshot = direct
            .prepare_preview("tm_test", 1, Path::new("ffmpeg"), Path::new("ffprobe"))
            .await
            .unwrap();
        assert!(!snapshot.generated);
        let direct_token = snapshot
            .media_url
            .as_deref()
            .unwrap()
            .rsplit('/')
            .next()
            .unwrap();
        assert!(direct.authorized_path(direct_token).is_some());

        let (transcoded, _directory, _calls) = setup(b"hevc-source", None, false, false);
        let snapshot = transcoded
            .prepare_preview("tm_test", 1, Path::new("ffmpeg"), Path::new("ffprobe"))
            .await
            .unwrap();
        assert!(snapshot.generated);
        let generated_token = snapshot
            .media_url
            .as_deref()
            .unwrap()
            .rsplit('/')
            .next()
            .unwrap();
        assert!(transcoded.authorized_path(generated_token).is_some());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn real_h264_and_hevc_samples_keep_audio_and_generate_compatible_preview() {
        use std::process::Command as ProcessCommand;

        let fixture_ffmpeg = std::env::var_os("DY_SCREEN_FIXTURE_FFMPEG")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("ffmpeg"));
        let runtime_ffmpeg = std::env::var_os("DY_SCREEN_CLIP_FFMPEG")
            .map(PathBuf::from)
            .unwrap_or_else(|| fixture_ffmpeg.clone());
        let runtime_ffprobe = std::env::var_os("DY_SCREEN_CLIP_FFPROBE")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("ffprobe"));
        let Ok(encoders) = ProcessCommand::new(&fixture_ffmpeg)
            .args(["-hide_banner", "-encoders"])
            .output()
        else {
            return;
        };
        let encoders = String::from_utf8_lossy(&encoders.stdout);
        if !["libx264", "libx265"].iter().all(|encoder| {
            encoders
                .lines()
                .any(|line| line.split_whitespace().nth(1) == Some(*encoder))
        }) || ProcessCommand::new(&runtime_ffmpeg)
            .arg("-version")
            .output()
            .is_err()
            || ProcessCommand::new(&runtime_ffprobe)
                .arg("-version")
                .output()
                .is_err()
        {
            return;
        }

        let directory = tempfile::tempdir().unwrap();
        let h264 = directory.path().join("bridge-h264.mp4");
        let hevc = directory.path().join("bridge-hevc.mp4");
        for (path, color, encoder, tag) in [
            (&h264, "red", "libx264", None),
            (&hevc, "blue", "libx265", Some("hvc1")),
        ] {
            let mut command = ProcessCommand::new(&fixture_ffmpeg);
            command.args([
                "-y",
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                &format!("color=c={color}:s=160x120:r=24:d=0.8"),
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=660:sample_rate=48000:duration=0.8",
                "-shortest",
                "-c:v",
                encoder,
                "-pix_fmt",
                "yuv420p",
                "-c:a",
                "aac",
            ]);
            if let Some(tag) = tag {
                command.args(["-tag:v", tag]);
            }
            assert!(command.arg(path).status().unwrap().success());
        }

        let request = |source: &Path| PreviewRequest {
            video_id: 1,
            source_path: source.to_string_lossy().into_owned(),
            source_status: "complete".to_owned(),
            ffmpeg_path: runtime_ffmpeg.to_string_lossy().into_owned(),
            ffprobe_path: runtime_ffprobe.to_string_lossy().into_owned(),
        };
        let executor = FfmpegPreviewExecutor;
        assert!(
            executor
                .direct_playable(&request(&h264), CancellationToken::new())
                .await
                .unwrap()
        );
        assert!(
            !executor
                .direct_playable(&request(&hevc), CancellationToken::new())
                .await
                .unwrap()
        );

        let preview = directory.path().join("bridge-hevc-preview.part.mp4");
        let method = executor
            .prepare(
                PreviewExecution {
                    request: request(&hevc),
                    temporary_output: preview.clone(),
                },
                Arc::new(|_, _| {}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(method, PreviewGenerationMethod::Transcode);
        let output = ProcessCommand::new(&runtime_ffprobe)
            .args([
                "-v",
                "error",
                "-show_entries",
                "stream=codec_type,codec_name",
                "-of",
                "csv=p=0",
            ])
            .arg(&preview)
            .output()
            .unwrap();
        assert!(output.status.success());
        let streams = String::from_utf8_lossy(&output.stdout);
        assert!(streams.lines().any(|line| line == "h264,video"));
        assert!(streams.lines().any(|line| line == "aac,audio"));
    }

    #[tokio::test]
    async fn preview_capability_failure_is_retriable_and_keeps_source() {
        let (service, _directory, _calls) = setup(b"hevc-source", None, false, true);
        let error = service
            .prepare_preview("tm_test", 1, Path::new("ffmpeg"), Path::new("ffprobe"))
            .await
            .unwrap_err();
        assert_eq!(error.code, "preview_failed");
        let download = service.repository.download("tm_test", 1).unwrap();
        assert_eq!(download.source_status, MaterialDownloadStatus::Ready);
        assert_eq!(download.preview_status, MaterialDownloadStatus::Failed);
    }

    #[test]
    fn thumbnail_format_validation_accepts_only_supported_images() {
        assert_eq!(
            image_extension(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]),
            Some("png")
        );
        assert_eq!(image_extension(&[0xff, 0xd8, 0xff, 0xe0]), Some("jpg"));
        assert_eq!(image_extension(b"RIFF1234WEBP"), Some("webp"));
        assert_eq!(image_extension(b"<svg></svg>"), None);
    }

    #[test]
    fn opaque_material_protocol_serves_bounded_byte_ranges() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("preview.mp4");
        std::fs::write(&path, b"0123456789").unwrap();
        let registry = MaterialAssetRegistry::default();
        registry.insert("opaque-token".to_owned(), path).unwrap();
        let request = tauri::http::Request::builder()
            .uri("transition-material://localhost/opaque-token")
            .header(tauri::http::header::RANGE, "bytes=2-5")
            .body(Vec::new())
            .unwrap();

        let response = serve_material_asset(&registry, &request);

        assert_eq!(response.status(), tauri::http::StatusCode::PARTIAL_CONTENT);
        assert_eq!(response.body(), b"2345");
        assert_eq!(
            response.headers()[tauri::http::header::CONTENT_RANGE],
            "bytes 2-5/10"
        );
    }
}
