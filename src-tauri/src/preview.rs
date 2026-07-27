use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use fs2::available_space;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::{Notify, Semaphore};
use tokio_util::sync::CancellationToken;

const PREVIEW_PROFILE_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PreviewState {
    Queued,
    Probing,
    Remuxing,
    Transcoding,
    Ready,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PreviewMedia {
    pub path: String,
    pub mime_type: String,
    pub cache_hit: bool,
    pub generated: bool,
    pub source_missing: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PreviewRequest {
    pub video_id: i64,
    pub source_path: String,
    pub source_status: String,
    pub ffmpeg_path: String,
    pub ffprobe_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PreviewFailure {
    pub code: String,
    pub message: String,
}

impl PreviewFailure {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for PreviewFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for PreviewFailure {}

#[derive(Debug, Clone)]
pub struct PreviewExecution {
    pub request: PreviewRequest,
    pub temporary_output: PathBuf,
}

pub type PreviewProgressCallback = Arc<dyn Fn(PreviewState, Option<f64>) + Send + Sync>;

#[async_trait]
pub trait PreviewExecutor: Send + Sync {
    async fn direct_playable(
        &self,
        _request: &PreviewRequest,
        cancellation: CancellationToken,
    ) -> Result<bool, PreviewFailure> {
        if cancellation.is_cancelled() {
            return Err(PreviewFailure::new("cancelled", "预览任务已取消"));
        }
        Ok(false)
    }

    async fn prepare(
        &self,
        execution: PreviewExecution,
        progress: PreviewProgressCallback,
        cancellation: CancellationToken,
    ) -> Result<PreviewGenerationMethod, PreviewFailure>;
}

pub trait PreviewPublisher: Send + Sync {
    fn publish(&self, snapshot: &PreviewSnapshot);
}

#[derive(Default)]
pub struct NoopPreviewPublisher;

impl PreviewPublisher for NoopPreviewPublisher {
    fn publish(&self, _snapshot: &PreviewSnapshot) {}
}

#[derive(Debug, Default)]
pub struct FfmpegPreviewExecutor;

#[derive(Clone)]
pub struct PreviewService {
    inner: Arc<PreviewServiceInner>,
}

struct PreviewServiceInner {
    cache: PreviewCache,
    executor: Arc<dyn PreviewExecutor>,
    publisher: Arc<dyn PreviewPublisher>,
    snapshots: Mutex<HashMap<String, PreviewSnapshot>>,
    latest_by_video: Mutex<HashMap<i64, String>>,
    playback_refs: Mutex<HashMap<String, usize>>,
    job_cancellations: Mutex<HashMap<String, CancellationToken>>,
    cache_publish_lock: Mutex<()>,
    active_requests: AtomicUsize,
    shutdown_notify: Notify,
    semaphore: Semaphore,
    shutdown: CancellationToken,
}

struct ActiveRequestGuard {
    inner: Arc<PreviewServiceInner>,
}

impl Drop for ActiveRequestGuard {
    fn drop(&mut self) {
        self.inner.active_requests.fetch_sub(1, Ordering::SeqCst);
        self.inner.shutdown_notify.notify_one();
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PreviewSnapshot {
    pub request_id: String,
    pub video_id: i64,
    pub state: PreviewState,
    pub progress_percent: Option<f64>,
    pub message: String,
    pub media: Option<PreviewMedia>,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
}

impl PreviewSnapshot {
    pub fn queued(video_id: i64, request_id: String) -> Self {
        Self {
            request_id,
            video_id,
            state: PreviewState::Queued,
            progress_percent: None,
            message: "预览任务已进入队列".to_owned(),
            media: None,
            error_code: None,
            error_message: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PreviewCacheIdentity {
    pub video_id: i64,
    pub canonical_path: String,
    pub size_bytes: u64,
    pub modified_millis: u128,
    pub profile_version: u32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PreviewGenerationMethod {
    Remux,
    Transcode,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PreviewCacheManifest {
    pub key: String,
    pub video_id: i64,
    pub source_path: String,
    pub source_size_bytes: u64,
    pub source_modified_millis: u128,
    pub profile_version: u32,
    pub created_at_millis: u128,
    pub last_accessed_at_millis: u128,
    pub generation_method: PreviewGenerationMethod,
    pub media_file: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewCachePaths {
    pub media: PathBuf,
    pub temporary_media: PathBuf,
    pub manifest: PathBuf,
    pub temporary_manifest: PathBuf,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PreviewCleanupReport {
    pub removed_parts: usize,
    pub removed_entries: usize,
    pub remaining_bytes: u64,
}

#[derive(Debug, Clone)]
pub struct PreviewCache {
    root: PathBuf,
    retention: Duration,
    max_bytes: u64,
}

impl PreviewCache {
    pub fn new(root: PathBuf) -> Self {
        Self::with_limits(
            root,
            Duration::from_secs(7 * 24 * 60 * 60),
            10 * 1024 * 1024 * 1024,
        )
    }

    pub fn with_limits(root: PathBuf, retention: Duration, max_bytes: u64) -> Self {
        Self {
            root,
            retention,
            max_bytes,
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn initialize(&self) -> io::Result<()> {
        fs::create_dir_all(&self.root)
    }

    pub fn paths(&self, key: &str) -> PreviewCachePaths {
        PreviewCachePaths {
            media: self.root.join(format!("{key}.mp4")),
            temporary_media: self.root.join(format!("{key}.part.mp4")),
            manifest: self.root.join(format!("{key}.json")),
            temporary_manifest: self.root.join(format!("{key}.part.json")),
        }
    }

    pub fn write_manifest(&self, manifest: &PreviewCacheManifest) -> io::Result<()> {
        self.initialize()?;
        if !is_safe_cache_key(&manifest.key)
            || Path::new(&manifest.media_file) != self.paths(&manifest.key).media
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "预览缓存清单包含无效路径",
            ));
        }
        let paths = self.paths(&manifest.key);
        let bytes = serde_json::to_vec_pretty(manifest).map_err(io::Error::other)?;
        fs::write(&paths.temporary_manifest, bytes)?;
        replace_file(&paths.temporary_manifest, &paths.manifest)
    }

    pub fn read_manifest(&self, key: &str) -> io::Result<Option<PreviewCacheManifest>> {
        if !is_safe_cache_key(key) {
            return Ok(None);
        }
        let path = self.paths(key).manifest;
        match fs::read(path) {
            Ok(bytes) => {
                let Ok(manifest) = serde_json::from_slice::<PreviewCacheManifest>(&bytes) else {
                    remove_if_exists(&self.paths(key).manifest)?;
                    return Ok(None);
                };
                let expected_media = self.paths(key).media;
                if manifest.key != key
                    || Path::new(&manifest.media_file) != expected_media
                    || !is_regular_file_without_symlink(&expected_media)
                {
                    remove_if_exists(&self.paths(key).manifest)?;
                    return Ok(None);
                }
                Ok(Some(manifest))
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    pub fn touch_manifest(
        &self,
        key: &str,
        accessed_at_millis: u128,
    ) -> io::Result<Option<PreviewCacheManifest>> {
        let Some(mut manifest) = self.read_manifest(key)? else {
            return Ok(None);
        };
        if !Path::new(&manifest.media_file).is_file() {
            self.remove_entry(&manifest)?;
            return Ok(None);
        }
        manifest.last_accessed_at_millis = accessed_at_millis;
        self.write_manifest(&manifest)?;
        Ok(Some(manifest))
    }

    pub fn find_latest_for_video(&self, video_id: i64) -> io::Result<Option<PreviewCacheManifest>> {
        let mut manifests = self.manifests()?;
        manifests.retain(|manifest| {
            manifest.video_id == video_id && Path::new(&manifest.media_file).is_file()
        });
        manifests.sort_by_key(|manifest| manifest.last_accessed_at_millis);
        Ok(manifests.pop())
    }

    pub fn evict_video(&self, video_id: i64) -> io::Result<()> {
        for manifest in self.manifests()? {
            if manifest.video_id == video_id {
                self.remove_entry(&manifest)?;
            }
        }
        Ok(())
    }

    pub fn cleanup_at(
        &self,
        now_millis: u128,
        protected_media: &HashSet<PathBuf>,
    ) -> io::Result<PreviewCleanupReport> {
        self.initialize()?;
        let mut report = PreviewCleanupReport::default();
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            let name = entry.file_name();
            if entry.file_type()?.is_file() && name.to_string_lossy().contains(".part.") {
                fs::remove_file(entry.path())?;
                report.removed_parts += 1;
            }
        }

        let manifest_count_before = fs::read_dir(&self.root)?
            .filter_map(Result::ok)
            .filter(|entry| {
                let path = entry.path();
                path.extension().and_then(|value| value.to_str()) == Some("json")
                    && !path
                        .file_name()
                        .is_some_and(|name| name.to_string_lossy().contains(".part."))
            })
            .count();
        let manifests = self.manifests()?;
        report.removed_entries += manifest_count_before.saturating_sub(manifests.len());
        let known_media = manifests
            .iter()
            .map(|manifest| PathBuf::from(&manifest.media_file))
            .collect::<HashSet<_>>();
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            let path = entry.path();
            if entry.file_type()?.is_file()
                && path.extension().and_then(|value| value.to_str()) == Some("mp4")
                && !path
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().contains(".part."))
                && !known_media.contains(&path)
                && !protected_media.contains(&path)
            {
                fs::remove_file(path)?;
                report.removed_entries += 1;
            }
        }

        let retention_millis = self.retention.as_millis();
        let mut retained = Vec::new();
        for manifest in manifests {
            let media = PathBuf::from(&manifest.media_file);
            if !media.is_file() {
                remove_if_exists(&self.paths(&manifest.key).manifest)?;
                continue;
            }
            let expired =
                now_millis.saturating_sub(manifest.last_accessed_at_millis) > retention_millis;
            if expired && !protected_media.contains(&media) {
                self.remove_entry(&manifest)?;
                report.removed_entries += 1;
            } else {
                retained.push(manifest);
            }
        }

        retained.sort_by_key(|manifest| manifest.last_accessed_at_millis);
        let mut total = retained
            .iter()
            .map(|manifest| file_len(Path::new(&manifest.media_file)))
            .sum::<u64>();
        for manifest in retained {
            if total <= self.max_bytes {
                break;
            }
            let media = PathBuf::from(&manifest.media_file);
            if protected_media.contains(&media) {
                continue;
            }
            let bytes = file_len(&media);
            self.remove_entry(&manifest)?;
            total = total.saturating_sub(bytes);
            report.removed_entries += 1;
        }
        report.remaining_bytes = total;
        Ok(report)
    }

    fn manifests(&self) -> io::Result<Vec<PreviewCacheManifest>> {
        self.initialize()?;
        let mut manifests = Vec::new();
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json")
                || path
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().contains(".part."))
            {
                continue;
            }
            let Some(key) = path.file_stem().and_then(|value| value.to_str()) else {
                remove_if_exists(&path)?;
                continue;
            };
            if let Some(manifest) = self.read_manifest(key)? {
                manifests.push(manifest);
            }
        }
        Ok(manifests)
    }

    fn remove_entry(&self, manifest: &PreviewCacheManifest) -> io::Result<()> {
        let paths = self.paths(&manifest.key);
        remove_if_exists(&paths.media)?;
        remove_if_exists(&paths.manifest)?;
        remove_if_exists(&paths.temporary_media)?;
        remove_if_exists(&paths.temporary_manifest)
    }

    pub fn has_video(&self, video_id: i64) -> io::Result<bool> {
        Ok(self.find_latest_for_video(video_id)?.is_some())
    }

    pub fn video_ids(&self) -> io::Result<HashSet<i64>> {
        Ok(self
            .manifests()?
            .into_iter()
            .map(|manifest| manifest.video_id)
            .collect())
    }
}

impl PreviewService {
    pub fn new(cache: PreviewCache) -> Result<Self, PreviewFailure> {
        Self::with_executor_and_publisher(
            cache,
            Arc::new(FfmpegPreviewExecutor),
            Arc::new(NoopPreviewPublisher),
        )
    }

    pub fn with_executor(
        cache: PreviewCache,
        executor: Arc<dyn PreviewExecutor>,
    ) -> Result<Self, PreviewFailure> {
        Self::with_executor_and_publisher(cache, executor, Arc::new(NoopPreviewPublisher))
    }

    pub fn with_executor_and_publisher(
        cache: PreviewCache,
        executor: Arc<dyn PreviewExecutor>,
        publisher: Arc<dyn PreviewPublisher>,
    ) -> Result<Self, PreviewFailure> {
        cache
            .initialize()
            .map_err(|_| PreviewFailure::new("cache_io", "无法创建视频预览缓存目录"))?;
        cache
            .cleanup_at(now_millis(), &HashSet::new())
            .map_err(|_| PreviewFailure::new("cache_io", "无法清理视频预览缓存"))?;
        Ok(Self {
            inner: Arc::new(PreviewServiceInner {
                cache,
                executor,
                publisher,
                snapshots: Mutex::new(HashMap::new()),
                latest_by_video: Mutex::new(HashMap::new()),
                playback_refs: Mutex::new(HashMap::new()),
                job_cancellations: Mutex::new(HashMap::new()),
                cache_publish_lock: Mutex::new(()),
                active_requests: AtomicUsize::new(0),
                shutdown_notify: Notify::new(),
                semaphore: Semaphore::new(1),
                shutdown: CancellationToken::new(),
            }),
        })
    }

    pub fn cache_root(&self) -> &Path {
        self.inner.cache.root()
    }

    pub async fn request(
        &self,
        request: PreviewRequest,
    ) -> Result<PreviewSnapshot, PreviewFailure> {
        self.request_internal(request, false).await
    }

    pub async fn retry(&self, request: PreviewRequest) -> Result<PreviewSnapshot, PreviewFailure> {
        self.request_internal(request, true).await
    }

    async fn request_internal(
        &self,
        request: PreviewRequest,
        retry: bool,
    ) -> Result<PreviewSnapshot, PreviewFailure> {
        let _request_guard = self.begin_request();
        if self.inner.shutdown.is_cancelled() {
            return Err(PreviewFailure::new("cancelled", "预览服务已停止"));
        }
        let source = PathBuf::from(&request.source_path);
        if !source.is_file() {
            return self.cached_missing_source(request.video_id);
        }
        if request.source_status != "complete" && request.source_status != "missing" {
            return Err(PreviewFailure::new(
                "video_not_complete",
                "只能预览已经完成的视频分片",
            ));
        }
        if source
            .file_name()
            .is_some_and(|name| name.to_string_lossy().contains(".partial"))
        {
            return Err(PreviewFailure::new(
                "video_not_complete",
                "正在写入的视频文件不能预览",
            ));
        }

        let extension = source
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if extension != "mkv" && extension != "mp4" {
            return Err(PreviewFailure::new(
                "unsupported_format",
                "当前只支持预览 MKV 或 MP4 视频",
            ));
        }
        let metadata = fs::metadata(&source)
            .map_err(|_| PreviewFailure::new("source_missing", "视频文件已被移动或删除"))?;
        let canonical = fs::canonicalize(&source).unwrap_or_else(|_| source.clone());
        let identity = PreviewCacheIdentity {
            video_id: request.video_id,
            canonical_path: canonical.to_string_lossy().into_owned(),
            size_bytes: metadata.len(),
            modified_millis: metadata
                .modified()
                .ok()
                .and_then(|value| value.duration_since(SystemTime::UNIX_EPOCH).ok())
                .map(|value| value.as_millis())
                .unwrap_or_default(),
            profile_version: PREVIEW_PROFILE_VERSION,
        };
        let key = cache_key(&identity);

        if extension == "mp4"
            && self
                .inner
                .executor
                .direct_playable(&request, self.inner.shutdown.child_token())
                .await?
        {
            let snapshot = ready_snapshot(
                request.video_id,
                key.clone(),
                canonical,
                false,
                false,
                false,
            );
            self.store_snapshot(snapshot.clone());
            return Ok(snapshot);
        }

        if !retry
            && let Some(manifest) = self
                .inner
                .cache
                .touch_manifest(&key, now_millis())
                .map_err(|_| PreviewFailure::new("cache_io", "无法读取视频预览缓存"))?
        {
            let snapshot = ready_snapshot(
                request.video_id,
                key.clone(),
                PathBuf::from(manifest.media_file),
                true,
                true,
                false,
            );
            self.store_snapshot(snapshot.clone());
            return Ok(snapshot);
        }

        if let Some(existing) = self.get(&key) {
            let reusable = match existing.state {
                PreviewState::Queued
                | PreviewState::Probing
                | PreviewState::Remuxing
                | PreviewState::Transcoding => true,
                PreviewState::Ready => existing
                    .media
                    .as_ref()
                    .is_some_and(|media| Path::new(&media.path).is_file()),
                PreviewState::Failed => !retry,
            };
            if reusable {
                return Ok(existing);
            }
            self.remove_snapshot(&key);
        }
        if retry {
            let paths = self.inner.cache.paths(&key);
            remove_if_exists(&paths.temporary_media)
                .map_err(|_| PreviewFailure::new("cache_io", "无法清理失败的预览任务"))?;
        }

        let snapshot = PreviewSnapshot::queued(request.video_id, key.clone());
        self.store_snapshot(snapshot.clone());
        let cancellation = self.inner.shutdown.child_token();
        self.inner
            .job_cancellations
            .lock()
            .expect("预览取消锁已损坏")
            .insert(key.clone(), cancellation.clone());
        let service = self.clone();
        let job_key = key.clone();
        tauri::async_runtime::spawn(async move {
            service.run_task(request, identity, key, cancellation).await;
            service.finish_job(&job_key);
        });
        Ok(snapshot)
    }

    pub fn get(&self, request_id: &str) -> Option<PreviewSnapshot> {
        self.inner
            .snapshots
            .lock()
            .expect("预览状态锁已损坏")
            .get(request_id)
            .cloned()
    }

    pub fn latest_for_video(&self, video_id: i64) -> Option<PreviewSnapshot> {
        let request_id = self
            .inner
            .latest_by_video
            .lock()
            .expect("预览索引锁已损坏")
            .get(&video_id)
            .cloned()?;
        self.get(&request_id)
    }

    pub fn retain_playback(&self, request_id: &str) {
        let mut references = self.inner.playback_refs.lock().expect("预览引用锁已损坏");
        *references.entry(request_id.to_owned()).or_insert(0) += 1;
    }

    pub fn release_playback(&self, request_id: &str) {
        let mut references = self.inner.playback_refs.lock().expect("预览引用锁已损坏");
        if let Some(count) = references.get_mut(request_id) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                references.remove(request_id);
            }
        }
    }

    pub fn evict_video(&self, video_id: i64) -> Result<(), PreviewFailure> {
        self.inner
            .latest_by_video
            .lock()
            .expect("预览索引锁已损坏")
            .remove(&video_id);
        let request_ids = self
            .inner
            .snapshots
            .lock()
            .expect("预览状态锁已损坏")
            .iter()
            .filter(|(_, snapshot)| snapshot.video_id == video_id)
            .map(|(request_id, _)| request_id.clone())
            .collect::<Vec<_>>();
        {
            let cancellations = self
                .inner
                .job_cancellations
                .lock()
                .expect("预览取消锁已损坏");
            for request_id in &request_ids {
                if let Some(cancellation) = cancellations.get(request_id) {
                    cancellation.cancel();
                }
            }
        }
        for request_id in request_ids {
            self.remove_snapshot(&request_id);
        }
        let _publish_guard = self
            .inner
            .cache_publish_lock
            .lock()
            .expect("预览缓存发布锁已损坏");
        self.inner
            .cache
            .evict_video(video_id)
            .map_err(|_| PreviewFailure::new("cache_io", "无法清理视频预览缓存"))
    }

    pub fn has_cached_video(&self, video_id: i64) -> Result<bool, PreviewFailure> {
        self.inner
            .cache
            .has_video(video_id)
            .map_err(|_| PreviewFailure::new("cache_io", "无法读取视频预览缓存"))
    }

    pub fn cached_video_ids(&self) -> Result<HashSet<i64>, PreviewFailure> {
        self.inner
            .cache
            .video_ids()
            .map_err(|_| PreviewFailure::new("cache_io", "无法读取视频预览缓存"))
    }

    pub fn cached_media_for_video(&self, video_id: i64) -> Result<Option<PathBuf>, PreviewFailure> {
        let Some(manifest) = self
            .inner
            .cache
            .find_latest_for_video(video_id)
            .map_err(|_| PreviewFailure::new("cache_io", "无法读取视频预览缓存"))?
        else {
            return Ok(None);
        };
        let manifest = self
            .inner
            .cache
            .touch_manifest(&manifest.key, now_millis())
            .map_err(|_| PreviewFailure::new("cache_io", "无法更新视频预览缓存"))?;
        Ok(manifest.map(|manifest| PathBuf::from(manifest.media_file)))
    }

    pub async fn shutdown(&self) {
        self.inner.shutdown.cancel();
        loop {
            let notified = self.inner.shutdown_notify.notified();
            let requests_finished = self.inner.active_requests.load(Ordering::SeqCst) == 0;
            let jobs_finished = self
                .inner
                .job_cancellations
                .lock()
                .expect("预览取消锁已损坏")
                .is_empty();
            if requests_finished && jobs_finished {
                return;
            }
            notified.await;
        }
    }

    async fn run_task(
        &self,
        request: PreviewRequest,
        identity: PreviewCacheIdentity,
        key: String,
        cancellation: CancellationToken,
    ) {
        let permit = tokio::select! {
            permit = self.inner.semaphore.acquire() => permit,
            _ = cancellation.cancelled() => {
                self.fail_snapshot(&key, PreviewFailure::new("cancelled", "预览任务已取消"));
                return;
            }
        };
        let Ok(_permit) = permit else {
            self.fail_snapshot(&key, PreviewFailure::new("cancelled", "预览服务已停止"));
            return;
        };
        if cancellation.is_cancelled() {
            self.fail_snapshot(&key, PreviewFailure::new("cancelled", "预览任务已取消"));
            return;
        }

        let protected = self.protected_media();
        if self
            .inner
            .cache
            .cleanup_at(now_millis(), &protected)
            .is_err()
        {
            self.fail_snapshot(
                &key,
                PreviewFailure::new("cache_io", "无法清理视频预览缓存"),
            );
            return;
        }
        let available = available_space(self.inner.cache.root()).unwrap_or_default();
        if !has_preview_capacity(
            available,
            estimate_preview_output_bytes(identity.size_bytes),
        ) {
            self.fail_snapshot(
                &key,
                PreviewFailure::new(
                    "cache_space_low",
                    "磁盘空间不足，请清理预览缓存或释放磁盘空间",
                ),
            );
            return;
        }

        let paths = self.inner.cache.paths(&key);
        let _ = remove_if_exists(&paths.temporary_media);
        let progress_service = self.clone();
        let progress_key = key.clone();
        let progress: PreviewProgressCallback = Arc::new(move |state, percent| {
            progress_service.update_progress(&progress_key, state, percent);
        });
        let execution = PreviewExecution {
            request: request.clone(),
            temporary_output: paths.temporary_media.clone(),
        };
        let result = self
            .inner
            .executor
            .prepare(execution, progress, cancellation.clone())
            .await;
        if cancellation.is_cancelled() {
            let _ = remove_if_exists(&paths.temporary_media);
            return;
        }
        match result {
            Ok(method) => {
                let _publish_guard = self
                    .inner
                    .cache_publish_lock
                    .lock()
                    .expect("预览缓存发布锁已损坏");
                if cancellation.is_cancelled() {
                    let _ = remove_if_exists(&paths.temporary_media);
                    return;
                }
                if !paths.temporary_media.is_file() {
                    self.fail_snapshot(
                        &key,
                        PreviewFailure::new("media_invalid", "预览转换没有生成有效视频"),
                    );
                    return;
                }
                if replace_file(&paths.temporary_media, &paths.media).is_err() {
                    self.fail_snapshot(
                        &key,
                        PreviewFailure::new("cache_io", "无法发布视频预览缓存"),
                    );
                    return;
                }
                let timestamp = now_millis();
                let manifest = PreviewCacheManifest {
                    key: key.clone(),
                    video_id: request.video_id,
                    source_path: identity.canonical_path,
                    source_size_bytes: identity.size_bytes,
                    source_modified_millis: identity.modified_millis,
                    profile_version: identity.profile_version,
                    created_at_millis: timestamp,
                    last_accessed_at_millis: timestamp,
                    generation_method: method,
                    media_file: paths.media.to_string_lossy().into_owned(),
                };
                if self.inner.cache.write_manifest(&manifest).is_err() {
                    let _ = remove_if_exists(&paths.media);
                    self.fail_snapshot(
                        &key,
                        PreviewFailure::new("cache_io", "无法写入视频预览缓存清单"),
                    );
                    return;
                }
                let mut protected = self.protected_media();
                protected.insert(paths.media.clone());
                let cleanup_succeeded = self
                    .inner
                    .cache
                    .cleanup_at(now_millis(), &protected)
                    .is_ok_and(|report| report.remaining_bytes <= self.inner.cache.max_bytes);
                if !cleanup_succeeded {
                    let _ = self.inner.cache.remove_entry(&manifest);
                    self.fail_snapshot(
                        &key,
                        PreviewFailure::new(
                            "cache_space_low",
                            "预览文件超出缓存容量限制，请释放磁盘空间",
                        ),
                    );
                    return;
                }
                self.store_snapshot(ready_snapshot(
                    request.video_id,
                    key,
                    paths.media,
                    false,
                    true,
                    false,
                ));
            }
            Err(error) => {
                let _ = remove_if_exists(&paths.temporary_media);
                self.fail_snapshot(&key, error);
            }
        }
    }

    fn cached_missing_source(&self, video_id: i64) -> Result<PreviewSnapshot, PreviewFailure> {
        let Some(manifest) = self
            .inner
            .cache
            .find_latest_for_video(video_id)
            .map_err(|_| PreviewFailure::new("cache_io", "无法读取视频预览缓存"))?
        else {
            return Err(PreviewFailure::new(
                "source_missing",
                "视频文件已被移动或删除，且没有可用预览缓存",
            ));
        };
        let manifest = self
            .inner
            .cache
            .touch_manifest(&manifest.key, now_millis())
            .map_err(|_| PreviewFailure::new("cache_io", "无法更新视频预览缓存"))?
            .ok_or_else(|| PreviewFailure::new("source_missing", "可用预览缓存已经失效"))?;
        let snapshot = ready_snapshot(
            video_id,
            manifest.key,
            PathBuf::from(manifest.media_file),
            true,
            true,
            true,
        );
        self.store_snapshot(snapshot.clone());
        Ok(snapshot)
    }

    fn protected_media(&self) -> HashSet<PathBuf> {
        let playback = self
            .inner
            .playback_refs
            .lock()
            .expect("预览引用锁已损坏")
            .keys()
            .cloned()
            .collect::<HashSet<_>>();
        self.inner
            .snapshots
            .lock()
            .expect("预览状态锁已损坏")
            .iter()
            .filter(|(request_id, snapshot)| {
                playback.contains(*request_id)
                    || matches!(
                        snapshot.state,
                        PreviewState::Queued
                            | PreviewState::Probing
                            | PreviewState::Remuxing
                            | PreviewState::Transcoding
                    )
            })
            .filter_map(|(_, snapshot)| snapshot.media.as_ref())
            .map(|media| PathBuf::from(&media.path))
            .collect()
    }

    fn update_progress(&self, request_id: &str, state: PreviewState, percent: Option<f64>) {
        let snapshot = {
            let mut snapshots = self.inner.snapshots.lock().expect("预览状态锁已损坏");
            let Some(snapshot) = snapshots.get_mut(request_id) else {
                return;
            };
            snapshot.state = state;
            snapshot.progress_percent = match (snapshot.progress_percent, percent) {
                (Some(previous), Some(next)) => Some(previous.max(next)),
                (_, next) => next,
            };
            snapshot.message = state_message(state).to_owned();
            snapshot.clone()
        };
        self.inner.publisher.publish(&snapshot);
    }

    fn fail_snapshot(&self, request_id: &str, failure: PreviewFailure) {
        let snapshot = {
            let mut snapshots = self.inner.snapshots.lock().expect("预览状态锁已损坏");
            let Some(snapshot) = snapshots.get_mut(request_id) else {
                return;
            };
            snapshot.state = PreviewState::Failed;
            snapshot.progress_percent = None;
            snapshot.message = "视频预览准备失败".to_owned();
            snapshot.error_code = Some(failure.code);
            snapshot.error_message = Some(failure.message);
            snapshot.clone()
        };
        self.inner.publisher.publish(&snapshot);
    }

    fn store_snapshot(&self, snapshot: PreviewSnapshot) {
        self.inner
            .latest_by_video
            .lock()
            .expect("预览索引锁已损坏")
            .insert(snapshot.video_id, snapshot.request_id.clone());
        self.inner
            .snapshots
            .lock()
            .expect("预览状态锁已损坏")
            .insert(snapshot.request_id.clone(), snapshot.clone());
        self.inner.publisher.publish(&snapshot);
    }

    fn remove_snapshot(&self, request_id: &str) {
        let removed = self
            .inner
            .snapshots
            .lock()
            .expect("预览状态锁已损坏")
            .remove(request_id);
        self.inner
            .playback_refs
            .lock()
            .expect("预览引用锁已损坏")
            .remove(request_id);
        if let Some(snapshot) = removed {
            let mut latest = self.inner.latest_by_video.lock().expect("预览索引锁已损坏");
            if latest.get(&snapshot.video_id).map(String::as_str) == Some(request_id) {
                latest.remove(&snapshot.video_id);
            }
        }
    }

    fn finish_job(&self, request_id: &str) {
        self.inner
            .job_cancellations
            .lock()
            .expect("预览取消锁已损坏")
            .remove(request_id);
        self.inner.shutdown_notify.notify_one();
    }

    fn begin_request(&self) -> ActiveRequestGuard {
        self.inner.active_requests.fetch_add(1, Ordering::SeqCst);
        ActiveRequestGuard {
            inner: self.inner.clone(),
        }
    }
}

pub fn estimate_preview_output_bytes(source_size_bytes: u64) -> u64 {
    source_size_bytes.saturating_mul(2)
}

pub fn has_preview_capacity(available_bytes: u64, estimated_output_bytes: u64) -> bool {
    const SAFETY_MARGIN: u64 = 2 * 1024 * 1024 * 1024;
    available_bytes >= estimated_output_bytes.saturating_add(SAFETY_MARGIN)
}

pub fn cache_key(identity: &PreviewCacheIdentity) -> String {
    let mut hasher = Sha256::new();
    hasher.update(identity.video_id.to_le_bytes());
    hasher.update([0]);
    hasher.update(identity.canonical_path.as_bytes());
    hasher.update([0]);
    hasher.update(identity.size_bytes.to_le_bytes());
    hasher.update(identity.modified_millis.to_le_bytes());
    hasher.update(identity.profile_version.to_le_bytes());
    hex::encode(hasher.finalize())
}

pub fn build_remux_args(input: &Path, output: &Path) -> Vec<String> {
    vec![
        "-hide_banner".to_owned(),
        "-nostdin".to_owned(),
        "-y".to_owned(),
        "-i".to_owned(),
        input.to_string_lossy().into_owned(),
        "-map".to_owned(),
        "0:v:0".to_owned(),
        "-map".to_owned(),
        "0:a:0?".to_owned(),
        "-c".to_owned(),
        "copy".to_owned(),
        "-movflags".to_owned(),
        "+faststart".to_owned(),
        "-progress".to_owned(),
        "pipe:1".to_owned(),
        "-nostats".to_owned(),
        "-f".to_owned(),
        "mov".to_owned(),
        "-brand".to_owned(),
        "mp42".to_owned(),
        output.to_string_lossy().into_owned(),
    ]
}

pub fn build_transcode_args(input: &Path, output: &Path, encoder: &str) -> Vec<String> {
    vec![
        "-hide_banner".to_owned(),
        "-nostdin".to_owned(),
        "-y".to_owned(),
        "-i".to_owned(),
        input.to_string_lossy().into_owned(),
        "-map".to_owned(),
        "0:v:0".to_owned(),
        "-map".to_owned(),
        "0:a:0?".to_owned(),
        "-pix_fmt".to_owned(),
        "yuv420p".to_owned(),
        "-c:v".to_owned(),
        encoder.to_owned(),
        "-c:a".to_owned(),
        "aac".to_owned(),
        "-b:a".to_owned(),
        "160k".to_owned(),
        "-movflags".to_owned(),
        "+faststart".to_owned(),
        "-progress".to_owned(),
        "pipe:1".to_owned(),
        "-nostats".to_owned(),
        "-f".to_owned(),
        "mov".to_owned(),
        "-brand".to_owned(),
        "mp42".to_owned(),
        output.to_string_lossy().into_owned(),
    ]
}

pub fn parse_progress_percent(line: &str, duration_seconds: Option<f64>) -> Option<f64> {
    let duration = duration_seconds.filter(|value| value.is_finite() && *value > 0.0)?;
    let (name, value) = line.split_once('=')?;
    if name != "out_time_us" && name != "out_time_ms" {
        return None;
    }
    let micros = value.parse::<f64>().ok()?;
    Some((micros / 1_000_000.0 / duration * 100.0).clamp(0.0, 100.0))
}

pub fn map_preview_progress(state: PreviewState, percent: f64) -> f64 {
    let percent = percent.clamp(0.0, 100.0);
    match state {
        PreviewState::Remuxing => percent * 0.15,
        PreviewState::Transcoding => 15.0 + percent * 0.83,
        _ => percent,
    }
}

#[derive(Debug, Deserialize)]
struct ProbeResponse {
    #[serde(default)]
    streams: Vec<ProbeStream>,
    format: Option<ProbeFormat>,
}

#[derive(Debug, Deserialize)]
struct ProbeStream {
    codec_type: Option<String>,
    codec_name: Option<String>,
    pix_fmt: Option<String>,
    profile: Option<String>,
    level: Option<i32>,
}

#[derive(Debug, Deserialize)]
struct ProbeFormat {
    duration: Option<String>,
}

#[derive(Debug, Clone)]
struct MediaProbe {
    duration_seconds: Option<f64>,
    video_codec: Option<String>,
    video_pixel_format: Option<String>,
    video_profile: Option<String>,
    video_level: Option<i32>,
    audio_codecs: Vec<String>,
}

#[async_trait]
impl PreviewExecutor for FfmpegPreviewExecutor {
    async fn direct_playable(
        &self,
        request: &PreviewRequest,
        cancellation: CancellationToken,
    ) -> Result<bool, PreviewFailure> {
        let output = probe_media(
            &request.ffprobe_path,
            Path::new(&request.source_path),
            cancellation,
        )
        .await?;
        Ok(media_is_webview_compatible(&output))
    }

    async fn prepare(
        &self,
        execution: PreviewExecution,
        progress: PreviewProgressCallback,
        cancellation: CancellationToken,
    ) -> Result<PreviewGenerationMethod, PreviewFailure> {
        progress(PreviewState::Probing, None);
        let source = PathBuf::from(&execution.request.source_path);
        let probe = probe_media(
            &execution.request.ffprobe_path,
            &source,
            cancellation.clone(),
        )
        .await?;
        if probe.video_codec.is_none() {
            return Err(PreviewFailure::new(
                "media_invalid",
                "视频文件不包含可播放的视频轨",
            ));
        }

        progress(
            PreviewState::Remuxing,
            Some(map_preview_progress(PreviewState::Remuxing, 0.0)),
        );
        let remux = run_ffmpeg(
            &execution.request.ffmpeg_path,
            build_remux_args(&source, &execution.temporary_output),
            probe.duration_seconds,
            PreviewState::Remuxing,
            progress.clone(),
            cancellation.clone(),
        )
        .await;
        if remux.is_ok()
            && probe_media(
                &execution.request.ffprobe_path,
                &execution.temporary_output,
                cancellation.clone(),
            )
            .await
            .is_ok_and(|output| media_is_webview_compatible(&output))
        {
            return Ok(PreviewGenerationMethod::Remux);
        }
        if let Err(error) = &remux
            && error.code != "conversion_failed"
        {
            return Err(error.clone());
        }

        remove_if_exists(&execution.temporary_output)
            .map_err(|_| PreviewFailure::new("cache_io", "无法清理重封装临时文件"))?;
        let encoders =
            select_h264_encoders(&execution.request.ffmpeg_path, cancellation.clone()).await?;
        let mut last_failure = None;
        for encoder in encoders {
            progress(
                PreviewState::Transcoding,
                Some(map_preview_progress(PreviewState::Transcoding, 0.0)),
            );
            match run_ffmpeg(
                &execution.request.ffmpeg_path,
                build_transcode_args(&source, &execution.temporary_output, &encoder),
                probe.duration_seconds,
                PreviewState::Transcoding,
                progress.clone(),
                cancellation.clone(),
            )
            .await
            {
                Ok(()) => {
                    let output = probe_media(
                        &execution.request.ffprobe_path,
                        &execution.temporary_output,
                        cancellation.clone(),
                    )
                    .await?;
                    if media_is_webview_compatible(&output) {
                        return Ok(PreviewGenerationMethod::Transcode);
                    }
                    last_failure = Some(PreviewFailure::new(
                        "media_invalid",
                        "转换结果不包含 WebView 兼容的音视频轨",
                    ));
                }
                Err(error) if error.code == "conversion_failed" => {
                    last_failure = Some(error);
                }
                Err(error) => return Err(error),
            }
            remove_if_exists(&execution.temporary_output)
                .map_err(|_| PreviewFailure::new("cache_io", "无法清理转码临时文件"))?;
        }
        Err(last_failure.unwrap_or_else(|| {
            PreviewFailure::new("encoder_unavailable", "没有可用的 H.264 编码器")
        }))
    }
}

async fn probe_media(
    executable: &str,
    path: &Path,
    cancellation: CancellationToken,
) -> Result<MediaProbe, PreviewFailure> {
    let child = Command::new(executable)
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration:stream=codec_type,codec_name,pix_fmt,profile,level",
            "-of",
            "json",
        ])
        .arg(path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| {
            PreviewFailure::new(
                "ffprobe_missing",
                "无法启动 FFprobe，请检查应用设置中的路径",
            )
        })?;
    let output = tokio::select! {
        _ = cancellation.cancelled() => {
            return Err(PreviewFailure::new("cancelled", "预览任务已取消"));
        }
        output = child.wait_with_output() => output.map_err(|_| {
            PreviewFailure::new("media_invalid", "无法等待 FFprobe 返回视频信息")
        })?,
    };
    if !output.status.success() {
        return Err(PreviewFailure::new(
            "media_invalid",
            "无法读取视频轨道信息，文件可能已损坏",
        ));
    }
    let response: ProbeResponse = serde_json::from_slice(&output.stdout)
        .map_err(|_| PreviewFailure::new("media_invalid", "FFprobe 返回了无法识别的视频信息"))?;
    let duration_seconds = response
        .format
        .and_then(|format| format.duration)
        .and_then(|duration| duration.parse::<f64>().ok())
        .filter(|duration| duration.is_finite() && *duration > 0.0);
    let video_stream = response
        .streams
        .iter()
        .find(|stream| stream.codec_type.as_deref() == Some("video"));
    let video_codec = video_stream.and_then(|stream| stream.codec_name.clone());
    let video_pixel_format = video_stream.and_then(|stream| stream.pix_fmt.clone());
    let video_profile = video_stream.and_then(|stream| stream.profile.clone());
    let video_level = video_stream.and_then(|stream| stream.level);
    let audio_codecs = response
        .streams
        .iter()
        .filter(|stream| stream.codec_type.as_deref() == Some("audio"))
        .filter_map(|stream| stream.codec_name.clone())
        .collect();
    Ok(MediaProbe {
        duration_seconds,
        video_codec,
        video_pixel_format,
        video_profile,
        video_level,
        audio_codecs,
    })
}

fn media_is_webview_compatible(probe: &MediaProbe) -> bool {
    is_webview_compatible(
        probe.video_codec.as_deref(),
        probe.video_pixel_format.as_deref(),
        probe.video_profile.as_deref(),
        probe.video_level,
        &probe
            .audio_codecs
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
    )
}

pub fn is_webview_compatible(
    video_codec: Option<&str>,
    pixel_format: Option<&str>,
    profile: Option<&str>,
    level: Option<i32>,
    audio_codecs: &[&str],
) -> bool {
    let supported_profile = matches!(
        profile,
        Some("Constrained Baseline" | "Baseline" | "Main" | "High")
    );
    video_codec == Some("h264")
        && matches!(pixel_format, Some("yuv420p" | "yuvj420p"))
        && supported_profile
        && level.is_some_and(|value| (0..=52).contains(&value))
        && audio_codecs.iter().all(|codec| *codec == "aac")
}

async fn select_h264_encoders(
    executable: &str,
    cancellation: CancellationToken,
) -> Result<Vec<String>, PreviewFailure> {
    let child = Command::new(executable)
        .args(["-hide_banner", "-encoders"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| {
            PreviewFailure::new("ffmpeg_missing", "无法启动 FFmpeg，请检查应用设置中的路径")
        })?;
    let output = tokio::select! {
        _ = cancellation.cancelled() => {
            return Err(PreviewFailure::new("cancelled", "预览任务已取消"));
        }
        output = child.wait_with_output() => output.map_err(|_| {
            PreviewFailure::new("encoder_unavailable", "无法等待 FFmpeg 编码器列表")
        })?,
    };
    if !output.status.success() {
        return Err(PreviewFailure::new(
            "encoder_unavailable",
            "无法读取 FFmpeg 编码器列表",
        ));
    }
    let listing = String::from_utf8_lossy(&output.stdout);
    let encoders = ["h264_videotoolbox", "libx264", "h264_mf"]
        .into_iter()
        .filter(|encoder| {
            listing
                .split_whitespace()
                .any(|candidate| candidate == *encoder)
        })
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if encoders.is_empty() {
        Err(PreviewFailure::new(
            "encoder_unavailable",
            "当前 FFmpeg 不包含可用的 H.264 编码器",
        ))
    } else {
        Ok(encoders)
    }
}

async fn run_ffmpeg(
    executable: &str,
    args: Vec<String>,
    duration_seconds: Option<f64>,
    state: PreviewState,
    progress: PreviewProgressCallback,
    cancellation: CancellationToken,
) -> Result<(), PreviewFailure> {
    let mut child = Command::new(executable)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| {
            PreviewFailure::new("ffmpeg_missing", "无法启动 FFmpeg，请检查应用设置中的路径")
        })?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| PreviewFailure::new("conversion_failed", "无法读取 FFmpeg 进度"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| PreviewFailure::new("conversion_failed", "无法读取 FFmpeg 错误输出"))?;
    let stderr_task = tokio::spawn(async move {
        let mut reader = BufReader::new(stderr);
        let mut bytes = Vec::new();
        let _ = reader.read_to_end(&mut bytes).await;
        bytes
    });
    let mut lines = BufReader::new(stdout).lines();
    loop {
        tokio::select! {
            _ = cancellation.cancelled() => {
                let _ = child.kill().await;
                let _ = stderr_task.await;
                return Err(PreviewFailure::new("cancelled", "预览任务已取消"));
            }
            line = lines.next_line() => {
                match line {
                    Ok(Some(line)) => {
                        if let Some(percent) = parse_progress_percent(&line, duration_seconds) {
                            progress(state, Some(map_preview_progress(state, percent)));
                        }
                    }
                    Ok(None) => break,
                    Err(_) => break,
                }
            }
        }
    }
    let status = tokio::select! {
        _ = cancellation.cancelled() => {
            let _ = child.kill().await;
            let _ = stderr_task.await;
            return Err(PreviewFailure::new("cancelled", "预览任务已取消"));
        }
        status = child.wait() => status.map_err(|_| PreviewFailure::new("conversion_failed", "无法等待 FFmpeg 转换完成"))?,
    };
    let _ = stderr_task.await;
    if !status.success() {
        return Err(PreviewFailure::new(
            "conversion_failed",
            "FFmpeg 无法生成兼容的 MP4 预览",
        ));
    }
    progress(state, Some(map_preview_progress(state, 100.0)));
    Ok(())
}

fn ready_snapshot(
    video_id: i64,
    request_id: String,
    path: PathBuf,
    cache_hit: bool,
    generated: bool,
    source_missing: bool,
) -> PreviewSnapshot {
    PreviewSnapshot {
        request_id,
        video_id,
        state: PreviewState::Ready,
        progress_percent: Some(100.0),
        message: if cache_hit {
            "预览缓存已准备完成".to_owned()
        } else {
            "视频可以播放".to_owned()
        },
        media: Some(PreviewMedia {
            path: path.to_string_lossy().into_owned(),
            mime_type: "video/mp4".to_owned(),
            cache_hit,
            generated,
            source_missing,
        }),
        error_code: None,
        error_message: None,
    }
}

fn state_message(state: PreviewState) -> &'static str {
    match state {
        PreviewState::Queued => "预览任务已进入队列",
        PreviewState::Probing => "正在读取视频信息",
        PreviewState::Remuxing => "正在无损准备 MP4 预览",
        PreviewState::Transcoding => "正在转换兼容的视频格式",
        PreviewState::Ready => "视频可以播放",
        PreviewState::Failed => "视频预览准备失败",
    }
}

fn now_millis() -> u128 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default()
}

fn replace_file(source: &Path, destination: &Path) -> io::Result<()> {
    remove_if_exists(destination)?;
    fs::rename(source, destination)
}

fn remove_if_exists(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn file_len(path: &Path) -> u64 {
    fs::metadata(path)
        .map(|metadata| metadata.len())
        .unwrap_or(0)
}

fn is_safe_cache_key(key: &str) -> bool {
    !key.is_empty()
        && key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

fn is_regular_file_without_symlink(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .map(|metadata| metadata.file_type().is_file() && !metadata.file_type().is_symlink())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::ProbeResponse;

    #[test]
    fn ffprobe_negative_level_does_not_invalidate_media_response() {
        let response = br#"{
            "streams": [
                {"codec_name":"ffv1","codec_type":"video","pix_fmt":"bgr0","level":-99},
                {"codec_name":"pcm_s16le","codec_type":"audio"}
            ],
            "format":{"duration":"0.5"}
        }"#;

        let parsed = serde_json::from_slice::<ProbeResponse>(response);

        assert!(parsed.is_ok());
    }
}
