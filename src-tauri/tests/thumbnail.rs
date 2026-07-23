use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use dy_screen_app_lib::domain::Video;
use dy_screen_app_lib::thumbnail::{
    FfmpegThumbnailExecutor, ThumbnailBatch, ThumbnailCache, ThumbnailCacheManifest,
    ThumbnailEvent, ThumbnailExecution, ThumbnailExecutor, ThumbnailFailure, ThumbnailImage,
    ThumbnailPublisher, ThumbnailRequest, ThumbnailService, ThumbnailSourceIdentity,
    ThumbnailSourceKind, ThumbnailState, build_thumbnail_args, thumbnail_cache_key,
    trusted_thumbnail_request, validate_thumbnail_jpeg,
};
use tokio_util::sync::CancellationToken;

fn write_test_jpeg(path: &Path, width: u16, height: u16) {
    let mut bytes = vec![0xff, 0xd8, 0xff, 0xc0, 0x00, 0x11, 0x08];
    bytes.extend_from_slice(&height.to_be_bytes());
    bytes.extend_from_slice(&width.to_be_bytes());
    bytes.extend_from_slice(&[3, 1, 0x11, 0, 2, 0x11, 0, 3, 0x11, 0, 0xff, 0xd9]);
    std::fs::write(path, bytes).unwrap();
}

#[test]
fn thumbnail_dtos_use_stable_camel_case_and_snake_case_states() {
    let value = serde_json::to_value(ThumbnailFailure::new(
        "thumbnail_extract_failed",
        "无法提取视频封面",
    ))
    .unwrap();
    assert_eq!(value["code"], "thumbnail_extract_failed");
    assert_eq!(value["message"], "无法提取视频封面");
    assert_eq!(
        serde_json::to_string(&ThumbnailState::Unavailable).unwrap(),
        "\"unavailable\""
    );
}

#[test]
fn source_fingerprint_is_stable_and_changes_for_source_kind_or_version() {
    let original = ThumbnailSourceIdentity {
        video_id: 7,
        canonical_path: "/录像/segment-001.mkv".to_owned(),
        size_bytes: 1024,
        modified_millis: 1_784_430_000_000,
        source_kind: ThumbnailSourceKind::Original,
        profile_version: 1,
    };
    let preview = ThumbnailSourceIdentity {
        source_kind: ThumbnailSourceKind::PreviewCache,
        ..original.clone()
    };
    let changed = ThumbnailSourceIdentity {
        size_bytes: 2048,
        ..original.clone()
    };

    assert_eq!(
        thumbnail_cache_key(&original),
        thumbnail_cache_key(&original)
    );
    assert_ne!(
        thumbnail_cache_key(&original),
        thumbnail_cache_key(&preview)
    );
    assert_ne!(
        thumbnail_cache_key(&original),
        thumbnail_cache_key(&changed)
    );
    assert_eq!(thumbnail_cache_key(&original).len(), 64);
}

#[test]
fn ffmpeg_plan_uses_start_then_one_second_fallback_and_safe_argument_array() {
    let input = Path::new("/tmp/中文 录像;$(touch unsafe).mkv");
    let output = Path::new("/tmp/封面 输出.part.jpg");
    let start = build_thumbnail_args(input, output, false);
    let fallback = build_thumbnail_args(input, output, true);

    assert!(!start.iter().any(|argument| argument == "-ss"));
    assert!(fallback.windows(2).any(|pair| pair == ["-ss", "1"]));
    assert!(start.windows(2).any(|pair| pair == ["-map", "0:v:0"]));
    assert!(start.windows(2).any(|pair| pair == ["-frames:v", "1"]));
    assert!(start.iter().any(|argument| argument.contains("480")));
    assert!(!start.iter().any(|argument| argument == "-noautorotate"));
    assert_eq!(
        start.last().map(String::as_str),
        Some("/tmp/封面 输出.part.jpg")
    );
    assert_eq!(
        start
            .iter()
            .filter(|argument| argument.as_str() == input.to_string_lossy())
            .count(),
        1
    );
}

#[test]
fn jpeg_validation_rejects_non_jpeg_and_oversized_images() {
    let directory = tempfile::tempdir().unwrap();
    let valid = directory.path().join("valid.jpg");
    let oversized = directory.path().join("oversized.jpg");
    let invalid = directory.path().join("invalid.jpg");
    write_test_jpeg(&valid, 480, 270);
    write_test_jpeg(&oversized, 481, 270);
    std::fs::write(&invalid, b"not-a-jpeg").unwrap();

    assert_eq!(validate_thumbnail_jpeg(&valid, 480).unwrap(), (480, 270));
    assert!(validate_thumbnail_jpeg(&oversized, 480).is_err());
    assert!(validate_thumbnail_jpeg(&invalid, 480).is_err());
}

#[test]
fn thumbnail_cache_publishes_atomically_and_reuses_matching_identity() {
    let directory = tempfile::tempdir().unwrap();
    let cache = ThumbnailCache::new(directory.path().join("video-thumbnails"));
    cache.initialize().unwrap();
    let identity = ThumbnailSourceIdentity {
        video_id: 9,
        canonical_path: "/tmp/segment.mkv".to_owned(),
        size_bytes: 1024,
        modified_millis: 100,
        source_kind: ThumbnailSourceKind::Original,
        profile_version: 1,
    };
    let key = thumbnail_cache_key(&identity);
    let paths = cache.paths(&key);
    write_test_jpeg(&paths.temporary_image, 480, 270);

    let manifest = cache.publish(&identity, 480, 270, 200).unwrap();

    assert!(paths.image.is_file());
    assert!(paths.manifest.is_file());
    assert!(!paths.temporary_image.exists());
    assert_eq!(
        cache.lookup(&identity, 300).unwrap().unwrap().key,
        manifest.key
    );
}

#[test]
fn cache_rejects_malicious_manifest_and_symlinked_image() {
    let directory = tempfile::tempdir().unwrap();
    let cache = ThumbnailCache::new(directory.path().join("cache"));
    cache.initialize().unwrap();
    let key = "a".repeat(64);
    let paths = cache.paths(&key);
    let outside = directory.path().join("outside.jpg");
    write_test_jpeg(&outside, 10, 10);
    std::fs::write(
        &paths.manifest,
        serde_json::to_vec(&ThumbnailCacheManifest {
            key: key.clone(),
            video_id: 1,
            source: ThumbnailSourceIdentity {
                video_id: 1,
                canonical_path: "/tmp/video.mkv".to_owned(),
                size_bytes: 10,
                modified_millis: 1,
                source_kind: ThumbnailSourceKind::Original,
                profile_version: 1,
            },
            created_at_millis: 1,
            last_accessed_at_millis: 1,
            width: 10,
            height: 10,
            image_file: outside.to_string_lossy().into_owned(),
        })
        .unwrap(),
    )
    .unwrap();

    assert!(cache.read_manifest(&key).unwrap().is_none());
    assert!(outside.is_file());

    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&outside, &paths.image).unwrap();
        assert!(cache.read_manifest(&key).unwrap().is_none());
    }
}

#[test]
fn cleanup_removes_parts_expired_lru_and_orphans_but_keeps_protected_entry() {
    let directory = tempfile::tempdir().unwrap();
    let cache =
        ThumbnailCache::with_limits(directory.path().to_path_buf(), Duration::from_secs(60), 55);
    cache.initialize().unwrap();
    std::fs::write(directory.path().join("abandoned.part.jpg"), b"partial").unwrap();
    std::fs::write(directory.path().join("orphan.jpg"), b"orphan").unwrap();

    let mut protected = HashSet::new();
    for (index, (key, accessed)) in [
        ("expired", 1_000),
        ("least", 100_000),
        ("protected", 101_000),
    ]
    .into_iter()
    .enumerate()
    {
        let identity = ThumbnailSourceIdentity {
            video_id: index as i64 + 1,
            canonical_path: format!("/tmp/{key}.mkv"),
            size_bytes: 10,
            modified_millis: 1,
            source_kind: ThumbnailSourceKind::Original,
            profile_version: 1,
        };
        let cache_key = thumbnail_cache_key(&identity);
        write_test_jpeg(&cache.paths(&cache_key).temporary_image, 10, 10);
        cache.publish(&identity, 10, 10, accessed).unwrap();
        if key == "protected" {
            protected.insert(cache_key);
        }
    }

    let report = cache.cleanup_at(120_000, &protected).unwrap();
    assert!(report.removed_parts >= 1);
    assert!(report.removed_entries >= 2);
    assert_eq!(cache.manifests().unwrap().len(), 1);
    assert_eq!(
        cache.manifests().unwrap()[0].key,
        protected.into_iter().next().unwrap()
    );
}

#[test]
fn cache_key_paths_never_escape_the_cache_root() {
    let directory = tempfile::tempdir().unwrap();
    let cache = ThumbnailCache::new(directory.path().join("cache"));
    cache.initialize().unwrap();
    for unsafe_key in ["../escape", "", "a/b", &"g".repeat(64)] {
        assert!(cache.validate_key(unsafe_key).is_err());
    }
    let safe = "f".repeat(64);
    let paths = cache.paths(&safe);
    assert!(paths.image.starts_with(cache.root()));
    assert_eq!(
        PathBuf::from(&paths.image),
        cache.root().join(format!("{safe}.jpg"))
    );
}

struct ActiveCallGuard<'a> {
    active: &'a AtomicUsize,
}

impl Drop for ActiveCallGuard<'_> {
    fn drop(&mut self) {
        self.active.fetch_sub(1, Ordering::SeqCst);
    }
}

struct ControlledExecutor {
    calls: AtomicUsize,
    active: AtomicUsize,
    max_active: AtomicUsize,
    failures_remaining: AtomicUsize,
    delay: Duration,
}

impl ControlledExecutor {
    fn new(delay: Duration, failures: usize) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            active: AtomicUsize::new(0),
            max_active: AtomicUsize::new(0),
            failures_remaining: AtomicUsize::new(failures),
            delay,
        }
    }
}

#[async_trait]
impl ThumbnailExecutor for ControlledExecutor {
    async fn extract(
        &self,
        execution: ThumbnailExecution,
        cancellation: CancellationToken,
    ) -> Result<ThumbnailImage, ThumbnailFailure> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_active.fetch_max(active, Ordering::SeqCst);
        let _guard = ActiveCallGuard {
            active: &self.active,
        };
        tokio::select! {
            _ = tokio::time::sleep(self.delay) => {}
            _ = cancellation.cancelled() => {
                return Err(ThumbnailFailure::new("cancelled", "视频封面任务已取消"));
            }
        }
        if self
            .failures_remaining
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |value| {
                value.checked_sub(1)
            })
            .is_ok()
        {
            return Err(ThumbnailFailure::new(
                "thumbnail_extract_failed",
                "无法提取视频封面",
            ));
        }
        write_test_jpeg(&execution.temporary_output, 480, 270);
        Ok(ThumbnailImage {
            width: 480,
            height: 270,
        })
    }
}

#[derive(Default)]
struct CollectingPublisher {
    events: Mutex<Vec<ThumbnailEvent>>,
}

impl ThumbnailPublisher for CollectingPublisher {
    fn publish(&self, event: &ThumbnailEvent) {
        self.events.lock().unwrap().push(event.clone());
    }
}

fn request(video_id: i64, path: &Path) -> ThumbnailRequest {
    ThumbnailRequest::available(
        video_id,
        path.to_string_lossy(),
        ThumbnailSourceKind::Original,
        "ffmpeg",
        "ffprobe",
    )
}

async fn wait_for_batch(
    service: &ThumbnailService,
    batch_id: &str,
    expected: ThumbnailState,
) -> ThumbnailBatch {
    for _ in 0..200 {
        let batch = service.get_batch(batch_id).unwrap();
        if batch.items.iter().all(|item| item.state == expected) {
            return batch;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("等待视频封面状态超时");
}

#[tokio::test]
async fn batches_deduplicate_sources_and_limit_ffmpeg_concurrency_to_two() {
    let directory = tempfile::tempdir().unwrap();
    let executor = Arc::new(ControlledExecutor::new(Duration::from_millis(25), 0));
    let publisher = Arc::new(CollectingPublisher::default());
    let service = ThumbnailService::with_executor_and_publisher(
        ThumbnailCache::new(directory.path().join("cache")),
        executor.clone(),
        publisher.clone(),
    )
    .unwrap();
    let mut requests = Vec::new();
    for video_id in 1..=4 {
        let source = directory.path().join(format!("video-{video_id}.mkv"));
        std::fs::write(&source, b"video").unwrap();
        requests.push(request(video_id, &source));
    }

    let first = service.request_batch(requests.clone()).await.unwrap();
    let duplicate = service.request_batch(requests.clone()).await.unwrap();
    wait_for_batch(&service, &first.batch_id, ThumbnailState::Ready).await;
    wait_for_batch(&service, &duplicate.batch_id, ThumbnailState::Ready).await;

    assert_eq!(executor.calls.load(Ordering::SeqCst), 4);
    assert!(executor.max_active.load(Ordering::SeqCst) <= 2);
    assert!(
        publisher.events.lock().unwrap().iter().all(|event| {
            event.batch_id == first.batch_id || event.batch_id == duplicate.batch_id
        })
    );

    let cached = service.request_batch(requests).await.unwrap();
    assert!(cached.items.iter().all(|item| {
        item.state == ThumbnailState::Ready && item.media.as_ref().unwrap().cache_hit
    }));
    assert_eq!(executor.calls.load(Ordering::SeqCst), 4);
}

#[tokio::test]
async fn releasing_a_page_cancels_unstarted_unreferenced_jobs() {
    let directory = tempfile::tempdir().unwrap();
    let executor = Arc::new(ControlledExecutor::new(Duration::from_millis(80), 0));
    let service = ThumbnailService::with_executor(
        ThumbnailCache::new(directory.path().join("cache")),
        executor.clone(),
    )
    .unwrap();
    let mut requests = Vec::new();
    for video_id in 1..=8 {
        let source = directory.path().join(format!("page-{video_id}.mkv"));
        std::fs::write(&source, b"video").unwrap();
        requests.push(request(video_id, &source));
    }
    let batch = service.request_batch(requests).await.unwrap();
    tokio::time::sleep(Duration::from_millis(10)).await;

    service.release_batch(&batch.batch_id);
    for _ in 0..100 {
        if service.active_job_count() == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    assert!(executor.calls.load(Ordering::SeqCst) <= 2);
    assert_eq!(service.active_job_count(), 0);
}

#[tokio::test]
async fn failed_source_is_not_automatically_retried_but_explicit_retry_can_succeed() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("retry.mkv");
    std::fs::write(&source, b"video").unwrap();
    let executor = Arc::new(ControlledExecutor::new(Duration::from_millis(5), 1));
    let service = ThumbnailService::with_executor(
        ThumbnailCache::new(directory.path().join("cache")),
        executor.clone(),
    )
    .unwrap();
    let media_request = request(1, &source);
    let batch = service
        .request_batch(vec![media_request.clone()])
        .await
        .unwrap();
    wait_for_batch(&service, &batch.batch_id, ThumbnailState::Failed).await;

    let second = service
        .request_batch(vec![media_request.clone()])
        .await
        .unwrap();
    assert_eq!(second.items[0].state, ThumbnailState::Failed);
    assert_eq!(executor.calls.load(Ordering::SeqCst), 1);

    let retry = service.retry(&batch.batch_id, media_request).await.unwrap();
    assert_eq!(retry.state, ThumbnailState::Queued);
    wait_for_batch(&service, &batch.batch_id, ThumbnailState::Ready).await;
    assert_eq!(executor.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn unavailable_sources_do_not_start_an_executor() {
    let directory = tempfile::tempdir().unwrap();
    let executor = Arc::new(ControlledExecutor::new(Duration::from_millis(1), 0));
    let service = ThumbnailService::with_executor(
        ThumbnailCache::new(directory.path().join("cache")),
        executor.clone(),
    )
    .unwrap();
    let batch = service
        .request_batch(vec![ThumbnailRequest::unavailable(
            1,
            "source_missing",
            "视频文件已被移动或删除",
        )])
        .await
        .unwrap();

    assert_eq!(batch.items[0].state, ThumbnailState::Unavailable);
    assert_eq!(executor.calls.load(Ordering::SeqCst), 0);
}

#[test]
fn trusted_media_prefers_original_and_only_falls_back_to_existing_preview_cache() {
    let directory = tempfile::tempdir().unwrap();
    let original = directory.path().join("original.mkv");
    let preview = directory.path().join("preview.mp4");
    std::fs::write(&original, b"original").unwrap();
    std::fs::write(&preview, b"preview").unwrap();
    let mut video = Video {
        id: 1,
        session_id: 1,
        streamer_id: 1,
        streamer_name: "测试主播".to_owned(),
        path: original.to_string_lossy().into_owned(),
        started_at: None,
        ended_at: None,
        duration_seconds: Some(60),
        size_bytes: 8,
        audio_present: Some(true),
        status: "complete".to_owned(),
        has_preview_cache: true,
    };

    let selected = trusted_thumbnail_request(&video, Some(&preview), "ffmpeg", "ffprobe");
    assert_eq!(selected.source_kind, Some(ThumbnailSourceKind::Original));
    assert_eq!(selected.source_path.as_deref(), original.to_str());

    std::fs::remove_file(&original).unwrap();
    video.status = "missing".to_owned();
    let selected = trusted_thumbnail_request(&video, Some(&preview), "ffmpeg", "ffprobe");
    assert_eq!(
        selected.source_kind,
        Some(ThumbnailSourceKind::PreviewCache)
    );
    assert_eq!(selected.source_path.as_deref(), preview.to_str());

    std::fs::remove_file(&preview).unwrap();
    let selected = trusted_thumbnail_request(&video, Some(&preview), "ffmpeg", "ffprobe");
    assert!(selected.source_path.is_none());
    assert_eq!(
        selected.unavailable_code.as_deref(),
        Some("preview_cache_invalid")
    );
}

#[tokio::test]
async fn eviction_cancels_running_job_and_prevents_late_cache_publish() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("evicted.mkv");
    std::fs::write(&source, b"video").unwrap();
    let cache = ThumbnailCache::new(directory.path().join("cache"));
    let executor = Arc::new(ControlledExecutor::new(Duration::from_secs(5), 0));
    let service = ThumbnailService::with_executor(cache.clone(), executor.clone()).unwrap();
    let batch = service
        .request_batch(vec![request(1, &source)])
        .await
        .unwrap();
    for _ in 0..100 {
        if executor.calls.load(Ordering::SeqCst) > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    service.begin_eviction(&[1]);
    service.commit_eviction(&[1]).unwrap();
    for _ in 0..100 {
        if service.active_job_count() == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    assert_eq!(service.active_job_count(), 0);
    assert!(cache.manifests().unwrap().is_empty());
    assert!(service.get_batch(&batch.batch_id).unwrap().items.is_empty());
}

#[tokio::test]
async fn rollback_eviction_keeps_existing_thumbnail_cache_available() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("rollback.mkv");
    std::fs::write(&source, b"video").unwrap();
    let executor = Arc::new(ControlledExecutor::new(Duration::from_millis(5), 0));
    let service = ThumbnailService::with_executor(
        ThumbnailCache::new(directory.path().join("cache")),
        executor.clone(),
    )
    .unwrap();
    let media_request = request(1, &source);
    let batch = service
        .request_batch(vec![media_request.clone()])
        .await
        .unwrap();
    wait_for_batch(&service, &batch.batch_id, ThumbnailState::Ready).await;

    service.begin_eviction(&[1]);
    service.rollback_eviction(&[1]);
    let restored = service.request_batch(vec![media_request]).await.unwrap();

    assert_eq!(restored.items[0].state, ThumbnailState::Ready);
    assert!(restored.items[0].media.as_ref().unwrap().cache_hit);
    assert_eq!(executor.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn asset_authorization_accepts_only_exact_owned_regular_jpeg() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("asset.mkv");
    std::fs::write(&source, b"video").unwrap();
    let service = ThumbnailService::with_executor(
        ThumbnailCache::new(directory.path().join("cache")),
        Arc::new(ControlledExecutor::new(Duration::from_millis(5), 0)),
    )
    .unwrap();
    let batch = service
        .request_batch(vec![request(1, &source)])
        .await
        .unwrap();
    let batch = wait_for_batch(&service, &batch.batch_id, ThumbnailState::Ready).await;
    let media = PathBuf::from(&batch.items[0].media.as_ref().unwrap().path);

    assert_eq!(
        service.authorize_media(1, &media).unwrap(),
        std::fs::canonicalize(&media).unwrap()
    );
    assert!(service.authorize_media(2, &media).is_err());
    assert!(service.authorize_media(1, directory.path()).is_err());
    let outside = directory.path().join("outside.jpg");
    write_test_jpeg(&outside, 10, 10);
    assert!(service.authorize_media(1, &outside).is_err());

    #[cfg(unix)]
    {
        let linked = directory.path().join("linked.jpg");
        std::os::unix::fs::symlink(&media, &linked).unwrap();
        assert!(service.authorize_media(1, &linked).is_err());
    }
}

#[tokio::test]
async fn shutdown_cancels_and_waits_for_running_jobs() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("shutdown.mkv");
    std::fs::write(&source, b"video").unwrap();
    let executor = Arc::new(ControlledExecutor::new(Duration::from_secs(5), 0));
    let service = ThumbnailService::with_executor(
        ThumbnailCache::new(directory.path().join("cache")),
        executor,
    )
    .unwrap();
    service
        .request_batch(vec![request(1, &source)])
        .await
        .unwrap();

    tokio::time::timeout(Duration::from_secs(1), service.shutdown())
        .await
        .expect("关闭必须等待任务响应取消");
    assert_eq!(service.active_job_count(), 0);
}

#[cfg(unix)]
#[tokio::test]
async fn ffmpeg_executor_retries_once_at_one_second_after_start_failure() {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.mkv");
    let fixture = directory.path().join("fixture.jpg");
    let ffmpeg = directory.path().join("fake-ffmpeg.sh");
    let ffprobe = directory.path().join("fake-ffprobe.sh");
    let output = directory.path().join("output.part.jpg");
    std::fs::write(&source, b"video").unwrap();
    write_test_jpeg(&fixture, 480, 270);
    std::fs::write(&ffprobe, "#!/bin/sh\nprintf '0\\n'\n").unwrap();
    std::fs::write(
        &ffmpeg,
        "#!/bin/sh\nseek=0\nfor arg in \"$@\"; do\n  [ \"$arg\" = \"-ss\" ] && seek=1\n  output=\"$arg\"\ndone\n[ \"$seek\" = \"1\" ] || exit 1\ncp \"$(dirname \"$0\")/fixture.jpg\" \"$output\"\n",
    )
    .unwrap();
    for script in [&ffmpeg, &ffprobe] {
        let mut permissions = std::fs::metadata(script).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(script, permissions).unwrap();
    }
    let media_request = ThumbnailRequest::available(
        1,
        source.to_string_lossy(),
        ThumbnailSourceKind::Original,
        ffmpeg.to_string_lossy(),
        ffprobe.to_string_lossy(),
    );
    let identity = dy_screen_app_lib::thumbnail::thumbnail_source_identity(&media_request).unwrap();

    let image = ThumbnailExecutor::extract(
        &FfmpegThumbnailExecutor,
        ThumbnailExecution {
            request: media_request,
            identity,
            temporary_output: output,
        },
        CancellationToken::new(),
    )
    .await
    .unwrap();

    assert_eq!(
        image,
        ThumbnailImage {
            width: 480,
            height: 270
        }
    );
}

#[tokio::test]
#[ignore = "需要本机 FFmpeg/FFprobe，供 make test-thumbnail-integration 调用"]
async fn real_ffmpeg_generates_landscape_portrait_and_video_without_audio() {
    let ffmpeg = std::env::var("FFMPEG").unwrap_or_else(|_| "ffmpeg".to_owned());
    let ffprobe = std::env::var("FFPROBE").unwrap_or_else(|_| "ffprobe".to_owned());
    let directory = tempfile::tempdir().unwrap();
    for (index, (width, height)) in [(640, 360), (360, 640)].into_iter().enumerate() {
        let source = directory.path().join(format!("sample-{index}.mp4"));
        let status = std::process::Command::new(&ffmpeg)
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                &format!("color=c=green:s={width}x{height}:d=1"),
                "-an",
                "-c:v",
                "libx264",
                source.to_str().unwrap(),
            ])
            .status()
            .unwrap();
        assert!(status.success());
        let media_request = ThumbnailRequest::available(
            index as i64 + 1,
            source.to_string_lossy(),
            ThumbnailSourceKind::Original,
            &ffmpeg,
            &ffprobe,
        );
        let identity =
            dy_screen_app_lib::thumbnail::thumbnail_source_identity(&media_request).unwrap();
        let output = directory.path().join(format!("sample-{index}.part.jpg"));
        let image = ThumbnailExecutor::extract(
            &FfmpegThumbnailExecutor,
            ThumbnailExecution {
                request: media_request,
                identity,
                temporary_output: output.clone(),
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert!(image.width.max(image.height) <= 480);
        assert_eq!(
            validate_thumbnail_jpeg(&output, 480).unwrap(),
            (image.width, image.height)
        );
    }

    let corrupt = directory.path().join("corrupt.mkv");
    std::fs::write(&corrupt, b"not-media").unwrap();
    let media_request = ThumbnailRequest::available(
        99,
        corrupt.to_string_lossy(),
        ThumbnailSourceKind::Original,
        ffmpeg,
        ffprobe,
    );
    let identity = dy_screen_app_lib::thumbnail::thumbnail_source_identity(&media_request).unwrap();
    let failure = ThumbnailExecutor::extract(
        &FfmpegThumbnailExecutor,
        ThumbnailExecution {
            request: media_request,
            identity,
            temporary_output: directory.path().join("corrupt.part.jpg"),
        },
        CancellationToken::new(),
    )
    .await
    .unwrap_err();
    assert_eq!(failure.code, "thumbnail_probe_failed");
}
