//! Tauri 对 Runtime Resource Pack 的最小适配层。
//!
//! 资源服务器地址来自构建期 `DY_SCREEN_RESOURCE_BASE_URL`，不作为用户设置暴露。
//! 前端只收到组件名、版本、大小和状态，不收到本地路径、请求头或命令行。

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use chrono::Utc;
use dy_screen::runtime_resources::{
    ResourceProgress, ResourceStatus, ResourceStatusSnapshot, ResolvedRuntimeResources,
    RuntimeDownloader, RuntimeInstaller, RuntimeManifest, RuntimeResourceError,
    compare_versions, resolve_runtime_resources,
};
use tokio_util::sync::CancellationToken;

use crate::database::Database;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeComponentView {
    pub id: String,
    pub version: String,
    pub required: bool,
    pub file_count: usize,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeResourceView {
    pub status: ResourceStatus,
    pub ready: bool,
    pub bundle_version: Option<String>,
    pub platform: String,
    pub app_min_version: String,
    pub manifest_sha256: Option<String>,
    pub components: Vec<RuntimeComponentView>,
    pub total_size_bytes: u64,
    pub minimum_free_disk_bytes: u64,
    pub minimum_memory_bytes: u64,
    pub source: String,
    pub downloaded_bytes: u64,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub updated_at: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeResourceEvent {
    pub status: RuntimeResourceView,
    pub progress: ResourceProgress,
}

#[derive(Debug, Clone)]
struct VerifiedFileMetadata {
    path: PathBuf,
    size_bytes: u64,
    modified_at: Option<SystemTime>,
}

#[derive(Debug, Clone)]
struct CachedRuntimeResources {
    resolved: ResolvedRuntimeResources,
    files: Vec<VerifiedFileMetadata>,
}

impl CachedRuntimeResources {
    fn capture(resolved: ResolvedRuntimeResources) -> Result<Self, RuntimeResourceError> {
        let mut paths = resolved
            .components
            .values()
            .flat_map(|paths| paths.iter().cloned())
            .collect::<Vec<_>>();
        paths.push(resolved.root.join("runtime-manifest.json"));
        let files = paths
            .into_iter()
            .map(|path| {
                let metadata = fs::symlink_metadata(&path)
                    .map_err(|_| RuntimeResourceError::MissingFile(path.display().to_string()))?;
                if metadata.file_type().is_symlink() || !metadata.is_file() {
                    return Err(RuntimeResourceError::Integrity(path.display().to_string()));
                }
                Ok(VerifiedFileMetadata {
                    path,
                    size_bytes: metadata.len(),
                    modified_at: metadata.modified().ok(),
                })
            })
            .collect::<Result<Vec<_>, RuntimeResourceError>>()?;
        Ok(Self { resolved, files })
    }

    fn metadata_is_current(&self) -> bool {
        self.files.iter().all(|verified| {
            fs::symlink_metadata(&verified.path).is_ok_and(|metadata| {
                !metadata.file_type().is_symlink()
                    && metadata.is_file()
                    && metadata.len() == verified.size_bytes
                    && metadata.modified().ok() == verified.modified_at
            })
        })
    }
}

#[derive(Clone)]
pub struct RuntimeResourceState {
    database: Database,
    install_root: PathBuf,
    bundled_root: PathBuf,
    fixed_base_url: Option<String>,
    snapshot: Arc<Mutex<RuntimeResourceView>>,
    resolved: Arc<Mutex<Option<CachedRuntimeResources>>>,
    cancellation: Arc<Mutex<Option<CancellationToken>>>,
}

impl RuntimeResourceState {
    pub fn new(database: Database, app_data_dir: PathBuf, bundled_root: PathBuf) -> Self {
        let platform = format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH);
        let persisted = database.runtime_resource_status().ok();
        let initial = persisted
            .as_ref()
            .map(|record| RuntimeResourceView {
                status: ResourceStatus::Verifying,
                ready: false,
                bundle_version: record.bundle_version.clone(),
                platform: record.platform.clone(),
                app_min_version: record.app_min_version.clone(),
                manifest_sha256: record.manifest_sha256.clone(),
                components: Vec::new(),
                total_size_bytes: record.total_bytes,
                minimum_free_disk_bytes: 0,
                minimum_memory_bytes: 0,
                source: "随包资源或固定 HTTPS 资源服务器".to_owned(),
                downloaded_bytes: record.progress_bytes,
                error_code: None,
                error_message: None,
                updated_at: Utc::now().to_rfc3339(),
            })
            .unwrap_or(RuntimeResourceView {
                status: ResourceStatus::Verifying,
                ready: false,
                bundle_version: None,
                platform,
                app_min_version: env!("CARGO_PKG_VERSION").to_owned(),
                manifest_sha256: None,
                components: Vec::new(),
                total_size_bytes: 0,
                minimum_free_disk_bytes: 0,
                minimum_memory_bytes: 0,
                source: "随包资源或固定 HTTPS 资源服务器".to_owned(),
                downloaded_bytes: 0,
                error_code: None,
                error_message: None,
                updated_at: Utc::now().to_rfc3339(),
            });
        Self {
            database,
            install_root: app_data_dir.join("runtime-resources"),
            bundled_root,
            fixed_base_url: option_env!("DY_SCREEN_RESOURCE_BASE_URL").map(str::to_owned),
            snapshot: Arc::new(Mutex::new(initial)),
            resolved: Arc::new(Mutex::new(None)),
            cancellation: Arc::new(Mutex::new(None)),
        }
    }

    pub fn view(&self) -> RuntimeResourceView {
        self.snapshot.lock().map(|value| value.clone()).unwrap_or_else(|_| RuntimeResourceView {
            status: ResourceStatus::Failed,
            ready: false,
            bundle_version: None,
            platform: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
            app_min_version: env!("CARGO_PKG_VERSION").to_owned(),
            manifest_sha256: None,
            components: Vec::new(),
            total_size_bytes: 0,
            minimum_free_disk_bytes: 0,
            minimum_memory_bytes: 0,
            source: "固定 HTTPS 资源服务器".to_owned(),
            downloaded_bytes: 0,
            error_code: Some("state_unavailable".to_owned()),
            error_message: Some("资源状态暂时不可用".to_owned()),
            updated_at: Utc::now().to_rfc3339(),
        })
    }

    pub fn fixed_source(&self) -> String {
        self.fixed_base_url.clone().unwrap_or_else(|| "发行构建未配置资源服务器".to_owned())
    }

    fn resolved_resources(&self) -> Result<ResolvedRuntimeResources, RuntimeResourceError> {
        let stale = self
            .resolved
            .lock()
            .map_err(|_| RuntimeResourceError::Install("运行资源缓存状态不可用".to_owned()))?
            .as_ref()
            .is_some_and(|resources| !resources.metadata_is_current());
        if stale {
            let _ = self.refresh();
        }
        self.resolved
            .lock()
            .map_err(|_| RuntimeResourceError::Install("运行资源缓存状态不可用".to_owned()))?
            .as_ref()
            .map(|resources| resources.resolved.clone())
            .ok_or_else(|| RuntimeResourceError::MissingFile("已验证的运行资源".to_owned()))
    }

    pub fn media_tools(&self) -> Result<(PathBuf, PathBuf), RuntimeResourceError> {
        let resolved = self.resolved_resources()?;
        let ffmpeg = resolved
            .components
            .get("media.ffmpeg")
            .and_then(|paths| paths.first())
            .cloned()
            .ok_or_else(|| RuntimeResourceError::MissingFile("media.ffmpeg".to_owned()))?;
        let ffprobe = resolved
            .components
            .get("media.ffprobe")
            .and_then(|paths| paths.first())
            .cloned()
            .ok_or_else(|| RuntimeResourceError::MissingFile("media.ffprobe".to_owned()))?;
        Ok((ffmpeg, ffprobe))
    }

    pub fn current_root(&self) -> Option<PathBuf> {
        self.resolved_resources()
            .ok()
            .map(|resources| resources.root)
    }

    pub fn refresh(&self) -> RuntimeResourceView {
        let result = self.find_and_resolve();
        let view = match result {
            Ok((manifest, resources, source)) => {
                let cached = CachedRuntimeResources::capture(resources);
                if let Ok(mut current) = self.resolved.lock() {
                    *current = cached.ok();
                }
                if self.resolved.lock().is_ok_and(|current| current.is_some()) {
                    self.view_for_manifest(&manifest, ResourceStatus::Ready, source, None)
                } else {
                    self.fail_verification()
                }
            }
            Err(error) => {
                if let Ok(mut current) = self.resolved.lock() {
                    *current = None;
                }
                let previous = self.view();
                RuntimeResourceView {
                    status: if previous.status == ResourceStatus::Ready { ResourceStatus::Rollback } else { ResourceStatus::Failed },
                    ready: false,
                    bundle_version: previous.bundle_version,
                    platform: previous.platform,
                    app_min_version: previous.app_min_version,
                    manifest_sha256: previous.manifest_sha256,
                    components: previous.components,
                    total_size_bytes: previous.total_size_bytes,
                    minimum_free_disk_bytes: previous.minimum_free_disk_bytes,
                    minimum_memory_bytes: previous.minimum_memory_bytes,
                    source: self.fixed_source(),
                    downloaded_bytes: previous.downloaded_bytes,
                    error_code: Some(error.code().to_owned()),
                    error_message: Some(safe_error_message(&error)),
                    updated_at: Utc::now().to_rfc3339(),
                }
            }
        };
        self.persist_view(&view);
        if let Ok(mut current) = self.snapshot.lock() { *current = view.clone(); }
        view
    }

    pub fn fail_verification(&self) -> RuntimeResourceView {
        if let Ok(mut current) = self.resolved.lock() {
            *current = None;
        }
        let previous = self.view();
        let view = RuntimeResourceView {
            status: ResourceStatus::Failed,
            ready: false,
            bundle_version: previous.bundle_version,
            platform: previous.platform,
            app_min_version: previous.app_min_version,
            manifest_sha256: previous.manifest_sha256,
            components: previous.components,
            total_size_bytes: previous.total_size_bytes,
            minimum_free_disk_bytes: previous.minimum_free_disk_bytes,
            minimum_memory_bytes: previous.minimum_memory_bytes,
            source: previous.source,
            downloaded_bytes: previous.downloaded_bytes,
            error_code: Some("resource_verification_failed".to_owned()),
            error_message: Some("运行资源校验任务异常退出，请重新检测".to_owned()),
            updated_at: Utc::now().to_rfc3339(),
        };
        self.set_view(view.clone());
        view
    }

    pub fn cancel(&self) {
        if let Ok(guard) = self.cancellation.lock()
            && let Some(token) = guard.as_ref()
        { token.cancel(); }
    }

    pub fn finish_download(&self) {
        if let Ok(mut guard) = self.cancellation.lock() {
            *guard = None;
        }
    }

    pub fn fail_download(&self, error: &RuntimeResourceError) {
        let mut view = self.view();
        view.status = if matches!(error, RuntimeResourceError::Cancelled) {
            ResourceStatus::Cancelled
        } else {
            ResourceStatus::Failed
        };
        view.ready = false;
        view.error_code = Some(error.code().to_owned());
        view.error_message = Some(safe_error_message(error));
        view.updated_at = Utc::now().to_rfc3339();
        self.set_view(view);
    }

    pub async fn download<F>(&self, mut publish: F) -> Result<RuntimeResourceView, RuntimeResourceError>
    where
        F: FnMut(RuntimeResourceView, ResourceProgress) + Send,
    {
        let base_url = self.fixed_base_url.clone().ok_or_else(|| RuntimeResourceError::Download("发行构建未配置固定 HTTPS 资源地址".into()))?;
        let cancellation = CancellationToken::new();
        if let Ok(mut guard) = self.cancellation.lock() {
            if guard.is_some() {
                return Err(RuntimeResourceError::Download("资源下载已在进行中".into()));
            }
            *guard = Some(cancellation.clone());
        }
        let downloader = RuntimeDownloader::new(&base_url, 10 * 1024 * 1024 * 1024)?;
        let manifest = downloader.fetch_manifest("runtime-manifest.json", &cancellation).await?;
        let source = self.install_root.join(format!("{}.download", manifest.bundle_version));
        if tokio::fs::try_exists(&source).await.unwrap_or(false) { tokio::fs::remove_dir_all(&source).await.ok(); }
        tokio::fs::create_dir_all(&source).await.map_err(|error| RuntimeResourceError::Download(error.to_string()))?;
        let total_size = manifest.components.iter().flat_map(|component| component.files.iter()).map(|file| file.size_bytes).sum();
        let mut downloaded = 0u64;
        let mut view = self.view_for_manifest(&manifest, ResourceStatus::Downloading, base_url.clone(), None);
        view.total_size_bytes = total_size;
        self.set_view(view.clone());
        for component in &manifest.components {
            for file in &component.files {
                let destination = source.join(&file.path);
                let component_id = component.id.clone();
                downloader.download_file(&file.path, &destination, file.size_bytes, &file.sha256, &cancellation, |progress| {
                    let current = ResourceProgress { component_id: Some(component_id.clone()), downloaded_bytes: downloaded.saturating_add(progress.downloaded_bytes), total_bytes: total_size, phase: progress.phase };
                    let mut next = view.clone();
                    next.downloaded_bytes = current.downloaded_bytes;
                    next.error_code = None;
                    next.error_message = None;
                    self.set_view(next.clone());
                    publish(next, current);
                }).await?;
                downloaded = downloaded.saturating_add(file.size_bytes);
            }
        }
        let installer = RuntimeInstaller::new(self.install_root.clone())?;
        let resolved = installer.install_directory(&source, &manifest)?;
        let _ = tokio::fs::remove_dir_all(&source).await;
        let cached = CachedRuntimeResources::capture(resolved.clone())?;
        if let Ok(mut current) = self.resolved.lock() {
            *current = Some(cached);
        }
        let view = self.view_for_resolved(&manifest, &resolved, base_url);
        self.set_view(view.clone());
        if let Ok(mut guard) = self.cancellation.lock() { *guard = None; }
        Ok(view)
    }

    fn find_and_resolve(&self) -> Result<(RuntimeManifest, ResolvedRuntimeResources, String), RuntimeResourceError> {
        let installer = RuntimeInstaller::new(self.install_root.clone())?;
        installer.cleanup_temporary()?;
        let mut candidates = Vec::new();
        let mut first_error = None;
        for (root, source) in [
            (installer.current_root(), "应用数据目录"),
            (Some(self.bundled_root.clone()), "安装包内资源"),
        ] {
            let Some(root) = root else { continue };
            let Some(found) = self.read_manifest(&root) else { continue };
            match found {
                Ok((manifest, _)) => {
                    if let Err(error) = manifest.require_app_version(env!("CARGO_PKG_VERSION")) {
                        first_error.get_or_insert(error);
                    } else {
                        candidates.push((manifest, root, source));
                    }
                }
                Err(error) => {
                    first_error.get_or_insert(error);
                }
            }
        }
        candidates.sort_by(|(left, _, _), (right, _, _)| {
            compare_versions(&right.bundle_version, &left.bundle_version)
        });
        for (manifest, root, source) in candidates {
            match resolve_runtime_resources(&root, &manifest) {
                Ok(resolved) => return Ok((manifest, resolved, source.to_owned())),
                Err(error) => {
                    first_error.get_or_insert(error);
                }
            }
        }
        Err(first_error.unwrap_or_else(|| {
            RuntimeResourceError::MissingFile("runtime-manifest.json".into())
        }))
    }

    fn read_manifest(&self, root: &Path) -> Option<Result<(RuntimeManifest, String), RuntimeResourceError>> {
        let path = root.join("runtime-manifest.json");
        let text = std::fs::read_to_string(path).ok()?;
        Some(RuntimeManifest::from_json(&text).map(|manifest| (manifest, text)))
    }

    fn view_for_manifest(&self, manifest: &RuntimeManifest, status: ResourceStatus, source: String, error: Option<RuntimeResourceError>) -> RuntimeResourceView {
        let platform = manifest.platforms.iter().find(|platform| platform.os == std::env::consts::OS && platform.arch == std::env::consts::ARCH);
        let components = manifest.components.iter().map(|component| RuntimeComponentView { id: component.id.clone(), version: component.version.clone(), required: component.required, file_count: component.files.len(), size_bytes: component.files.iter().map(|file| file.size_bytes).sum() }).collect::<Vec<_>>();
        RuntimeResourceView { status, ready: status == ResourceStatus::Ready, bundle_version: Some(manifest.bundle_version.clone()), platform: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH), app_min_version: manifest.app_min_version.clone(), manifest_sha256: manifest.manifest_sha256().ok(), components, total_size_bytes: manifest.components.iter().flat_map(|component| component.files.iter()).map(|file| file.size_bytes).sum(), minimum_free_disk_bytes: platform.map(|value| value.minimum_free_disk_bytes).unwrap_or(0), minimum_memory_bytes: platform.map(|value| value.minimum_memory_bytes).unwrap_or(0), source, downloaded_bytes: if status == ResourceStatus::Ready { manifest.components.iter().flat_map(|component| component.files.iter()).map(|file| file.size_bytes).sum() } else { 0 }, error_code: error.as_ref().map(|value| value.code().to_owned()), error_message: error.as_ref().map(safe_error_message), updated_at: Utc::now().to_rfc3339() }
    }

    fn view_for_resolved(&self, manifest: &RuntimeManifest, _resolved: &ResolvedRuntimeResources, source: String) -> RuntimeResourceView {
        self.view_for_manifest(manifest, ResourceStatus::Ready, source, None)
    }

    fn set_view(&self, view: RuntimeResourceView) {
        self.persist_view(&view);
        if let Ok(mut current) = self.snapshot.lock() { *current = view; }
    }

    fn persist_view(&self, view: &RuntimeResourceView) {
        let _ = self.database.save_runtime_resource_status(&ResourceStatusSnapshot { status: view.status, bundle_version: view.bundle_version.clone(), manifest_sha256: view.manifest_sha256.clone(), current_component: None, downloaded_bytes: view.downloaded_bytes, total_bytes: view.total_size_bytes, error_code: view.error_code.clone(), error_message: view.error_message.clone() });
    }
}

fn safe_error_message(error: &RuntimeResourceError) -> String {
    match error {
        RuntimeResourceError::Download(_) => "资源服务器访问失败，请检查网络后重试".to_owned(),
        RuntimeResourceError::MissingFile(_) => "资源包缺少必需组件，请重新下载".to_owned(),
        RuntimeResourceError::InvalidSignature => "资源清单签名校验失败，已拒绝安装".to_owned(),
        RuntimeResourceError::Integrity(_) => "资源文件校验失败，未安装不完整文件".to_owned(),
        RuntimeResourceError::InvalidPath(_) => "资源清单包含不安全路径，已拒绝安装".to_owned(),
        RuntimeResourceError::Install(_) => "资源安装失败，未保留半成品文件".to_owned(),
        RuntimeResourceError::InvalidManifest(_) => "资源清单格式或约束无效".to_owned(),
        _ => error.to_string(),
    }
}
