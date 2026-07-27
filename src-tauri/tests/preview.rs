use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use dy_screen_app_lib::preview::{
    PreviewCache, PreviewCacheIdentity, PreviewCacheManifest, PreviewExecution, PreviewExecutor,
    PreviewFailure, PreviewGenerationMethod, PreviewProgressCallback, PreviewRequest,
    PreviewService, PreviewSnapshot, PreviewState, build_remux_args, build_transcode_args,
    cache_key, estimate_preview_output_bytes, has_preview_capacity, is_webview_compatible,
    map_preview_progress, parse_progress_percent,
};
use tokio_util::sync::CancellationToken;

#[test]
fn preview_snapshot_uses_stable_camel_case_json() {
    let snapshot = PreviewSnapshot::queued(42, "request-42".to_owned());

    let value = serde_json::to_value(snapshot).expect("预览状态应可序列化");

    assert_eq!(value["requestId"], "request-42");
    assert_eq!(value["videoId"], 42);
    assert_eq!(value["state"], "queued");
    assert!(value["progressPercent"].is_null());
}

#[test]
fn remux_plan_maps_optional_audio_without_shell_joining() {
    let args = build_remux_args(
        Path::new("/tmp/中文 录像.mkv"),
        Path::new("/tmp/cache output.part.mp4"),
    );

    assert_eq!(
        args,
        vec![
            "-hide_banner",
            "-nostdin",
            "-y",
            "-i",
            "/tmp/中文 录像.mkv",
            "-map",
            "0:v:0",
            "-map",
            "0:a:0?",
            "-c",
            "copy",
            "-movflags",
            "+faststart",
            "-progress",
            "pipe:1",
            "-nostats",
            "-f",
            "mov",
            "-brand",
            "mp42",
            "/tmp/cache output.part.mp4",
        ]
    );
}

#[test]
fn transcode_plan_targets_h264_and_optional_aac() {
    let args = build_transcode_args(
        Path::new("input.mkv"),
        Path::new("preview.part.mp4"),
        "libx264",
    );

    assert!(args.windows(2).any(|pair| pair == ["-c:v", "libx264"]));
    assert!(args.windows(2).any(|pair| pair == ["-c:a", "aac"]));
    assert!(args.windows(2).any(|pair| pair == ["-map", "0:a:0?"]));
    assert!(args.windows(2).any(|pair| pair == ["-f", "mov"]));
    assert!(args.windows(2).any(|pair| pair == ["-brand", "mp42"]));
    assert_eq!(args.last().map(String::as_str), Some("preview.part.mp4"));
}

#[test]
fn cache_key_is_stable_and_changes_with_source_version() {
    let base = PreviewCacheIdentity {
        video_id: 9,
        canonical_path: "/录像/segment-001.mkv".to_owned(),
        size_bytes: 1024,
        modified_millis: 1_784_430_000_000,
        profile_version: 1,
    };
    let same = base.clone();
    let changed = PreviewCacheIdentity {
        size_bytes: 2048,
        ..base.clone()
    };

    assert_eq!(cache_key(&base), cache_key(&same));
    assert_ne!(cache_key(&base), cache_key(&changed));
    assert_eq!(cache_key(&base).len(), 64);
}

#[test]
fn progress_parser_returns_bounded_percentage_or_none() {
    assert_eq!(
        parse_progress_percent("out_time_us=5000000", Some(10.0)),
        Some(50.0)
    );
    assert_eq!(
        parse_progress_percent("out_time_ms=5000000", Some(10.0)),
        Some(50.0)
    );
    assert_eq!(
        parse_progress_percent("out_time_us=20000000", Some(10.0)),
        Some(100.0)
    );
    assert_eq!(
        parse_progress_percent("progress=continue", Some(10.0)),
        None
    );
    assert_eq!(parse_progress_percent("out_time_us=5000000", None), None);
}

#[test]
fn preview_state_serializes_as_snake_case() {
    assert_eq!(
        serde_json::to_string(&PreviewState::Transcoding).unwrap(),
        "\"transcoding\""
    );
}

#[test]
fn preview_failure_exposes_stable_code_to_tauri() {
    let value = serde_json::to_value(PreviewFailure::new(
        "source_missing",
        "视频文件已被移动或删除",
    ))
    .unwrap();
    assert_eq!(value["code"], "source_missing");
    assert_eq!(value["message"], "视频文件已被移动或删除");
}

#[test]
fn cache_cleanup_removes_part_expired_and_lru_entries() {
    let directory = tempfile::tempdir().unwrap();
    let cache =
        PreviewCache::with_limits(directory.path().to_path_buf(), Duration::from_secs(60), 7);
    cache.initialize().unwrap();
    std::fs::write(directory.path().join("abandoned.part.mp4"), b"partial").unwrap();

    for (key, video_id, accessed, bytes) in [
        ("expired", 1, 1_000, b"old".as_slice()),
        ("least-recent", 2, 100_000, b"1234".as_slice()),
        ("newest", 3, 110_000, b"5678".as_slice()),
    ] {
        let paths = cache.paths(key);
        std::fs::write(&paths.media, bytes).unwrap();
        cache
            .write_manifest(&PreviewCacheManifest {
                key: key.to_owned(),
                video_id,
                source_path: format!("/tmp/{key}.mkv"),
                source_size_bytes: bytes.len() as u64,
                source_modified_millis: 1,
                profile_version: 1,
                created_at_millis: accessed,
                last_accessed_at_millis: accessed,
                generation_method: PreviewGenerationMethod::Remux,
                media_file: paths.media.to_string_lossy().into_owned(),
            })
            .unwrap();
    }

    let report = cache.cleanup_at(120_000, &HashSet::new()).unwrap();

    assert!(report.removed_parts >= 1);
    assert!(!cache.paths("expired").media.exists());
    assert!(!cache.paths("least-recent").media.exists());
    assert!(cache.paths("newest").media.exists());
}

#[test]
fn cache_cleanup_removes_orphaned_final_media_and_manifest() {
    let directory = tempfile::tempdir().unwrap();
    let cache = PreviewCache::new(directory.path().to_path_buf());
    cache.initialize().unwrap();
    std::fs::write(cache.paths("orphan-media").media, b"orphan").unwrap();
    std::fs::write(cache.paths("orphan-manifest").manifest, b"{}").unwrap();

    let report = cache.cleanup_at(120_000, &HashSet::new()).unwrap();

    assert_eq!(report.removed_entries, 2);
    assert!(!cache.paths("orphan-media").media.exists());
    assert!(!cache.paths("orphan-manifest").manifest.exists());
}

#[test]
fn cache_can_find_and_evict_latest_video_entry() {
    let directory = tempfile::tempdir().unwrap();
    let cache = PreviewCache::with_limits(
        directory.path().to_path_buf(),
        Duration::from_secs(60),
        1024,
    );
    cache.initialize().unwrap();
    for (key, accessed) in [("older", 100), ("newer", 200)] {
        let paths = cache.paths(key);
        std::fs::write(&paths.media, b"mp4").unwrap();
        cache
            .write_manifest(&PreviewCacheManifest {
                key: key.to_owned(),
                video_id: 7,
                source_path: "/tmp/missing.mkv".to_owned(),
                source_size_bytes: 3,
                source_modified_millis: 1,
                profile_version: 1,
                created_at_millis: accessed,
                last_accessed_at_millis: accessed,
                generation_method: PreviewGenerationMethod::Transcode,
                media_file: paths.media.to_string_lossy().into_owned(),
            })
            .unwrap();
    }

    let latest = cache.find_latest_for_video(7).unwrap().unwrap();
    assert_eq!(latest.key, "newer");

    cache.evict_video(7).unwrap();
    assert!(cache.find_latest_for_video(7).unwrap().is_none());
}

#[test]
fn cache_manifest_cannot_escape_cache_directory() {
    let directory = tempfile::tempdir().unwrap();
    let cache = PreviewCache::new(directory.path().join("cache"));
    cache.initialize().unwrap();
    let outside = directory.path().join("outside.mp4");
    std::fs::write(&outside, b"must-stay").unwrap();
    std::fs::write(
        cache.paths("malicious").manifest,
        serde_json::to_vec_pretty(&PreviewCacheManifest {
            key: "malicious".to_owned(),
            video_id: 99,
            source_path: "/tmp/source.mkv".to_owned(),
            source_size_bytes: 9,
            source_modified_millis: 1,
            profile_version: 1,
            created_at_millis: 1,
            last_accessed_at_millis: 1,
            generation_method: PreviewGenerationMethod::Remux,
            media_file: outside.to_string_lossy().into_owned(),
        })
        .unwrap(),
    )
    .unwrap();

    cache.evict_video(99).unwrap();

    assert!(outside.is_file());
    assert!(cache.find_latest_for_video(99).unwrap().is_none());
}

#[test]
fn capacity_check_preserves_two_gibibyte_safety_margin() {
    const GIB: u64 = 1024 * 1024 * 1024;
    assert!(has_preview_capacity(3 * GIB, GIB));
    assert!(!has_preview_capacity(3 * GIB - 1, GIB));
}

#[test]
fn transcode_capacity_estimate_is_conservative_for_more_compact_sources() {
    const GIB: u64 = 1024 * 1024 * 1024;
    assert_eq!(estimate_preview_output_bytes(GIB), 2 * GIB);
    assert_eq!(estimate_preview_output_bytes(u64::MAX), u64::MAX);
}

#[test]
fn webview_compatibility_requires_h264_and_optional_aac() {
    assert!(is_webview_compatible(
        Some("h264"),
        Some("yuv420p"),
        Some("High"),
        Some(41),
        &[],
    ));
    assert!(is_webview_compatible(
        Some("h264"),
        Some("yuv420p"),
        Some("Main"),
        Some(40),
        &["aac"],
    ));
    assert!(!is_webview_compatible(
        Some("h264"),
        Some("yuv420p10le"),
        Some("High 10"),
        Some(50),
        &["aac"],
    ));
    assert!(!is_webview_compatible(
        Some("h264"),
        Some("yuv422p"),
        Some("High 4:2:2"),
        Some(41),
        &["aac"],
    ));
    assert!(!is_webview_compatible(
        Some("h264"),
        Some("yuv420p"),
        Some("High"),
        Some(60),
        &["aac"],
    ));
    assert!(!is_webview_compatible(
        Some("ffv1"),
        Some("yuv420p"),
        None,
        None,
        &["pcm_s16le"],
    ));
    assert!(!is_webview_compatible(
        Some("h264"),
        Some("yuv420p"),
        Some("High"),
        Some(41),
        &["pcm_s16le"],
    ));
    assert!(!is_webview_compatible(
        None,
        Some("yuv420p"),
        Some("High"),
        Some(41),
        &["aac"],
    ));
}

#[test]
fn preview_progress_stays_monotonic_when_remux_falls_back_to_transcode() {
    assert_eq!(map_preview_progress(PreviewState::Remuxing, 100.0), 15.0);
    assert_eq!(map_preview_progress(PreviewState::Transcoding, 0.0), 15.0);
    assert_eq!(map_preview_progress(PreviewState::Transcoding, 50.0), 56.5);
    assert_eq!(map_preview_progress(PreviewState::Transcoding, 100.0), 98.0);
}

#[derive(Default)]
struct FakeExecutor {
    calls: AtomicUsize,
    active: AtomicUsize,
    max_active: AtomicUsize,
}

#[async_trait]
impl PreviewExecutor for FakeExecutor {
    async fn prepare(
        &self,
        execution: PreviewExecution,
        progress: PreviewProgressCallback,
        cancellation: CancellationToken,
    ) -> Result<PreviewGenerationMethod, PreviewFailure> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_active.fetch_max(active, Ordering::SeqCst);
        progress(PreviewState::Remuxing, Some(50.0));
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(40)) => {}
            _ = cancellation.cancelled() => {
                self.active.fetch_sub(1, Ordering::SeqCst);
                return Err(PreviewFailure::new("cancelled", "预览任务已取消"));
            }
        }
        std::fs::write(&execution.temporary_output, b"fake-mp4")
            .map_err(|error| PreviewFailure::new("cache_write_failed", error.to_string()))?;
        self.active.fetch_sub(1, Ordering::SeqCst);
        Ok(PreviewGenerationMethod::Remux)
    }
}

fn preview_request(video_id: i64, source: &Path) -> PreviewRequest {
    PreviewRequest {
        video_id,
        source_path: source.to_string_lossy().into_owned(),
        source_status: "complete".to_owned(),
        ffmpeg_path: "ffmpeg".to_owned(),
        ffprobe_path: "ffprobe".to_owned(),
    }
}

async fn wait_until_ready(service: &PreviewService, request_id: &str) -> PreviewSnapshot {
    for _ in 0..100 {
        let snapshot = service.get(request_id).expect("任务状态应存在");
        if matches!(snapshot.state, PreviewState::Ready | PreviewState::Failed) {
            return snapshot;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("等待预览任务完成超时")
}

#[tokio::test]
async fn service_deduplicates_same_source_version_and_publishes_cache() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.mkv");
    std::fs::write(&source, b"source").unwrap();
    let executor = Arc::new(FakeExecutor::default());
    let service = PreviewService::with_executor(
        PreviewCache::new(directory.path().join("cache")),
        executor.clone(),
    )
    .unwrap();

    let first = service.request(preview_request(1, &source)).await.unwrap();
    let second = service.request(preview_request(1, &source)).await.unwrap();

    assert_eq!(first.request_id, second.request_id);
    let ready = wait_until_ready(&service, &first.request_id).await;
    assert_eq!(ready.state, PreviewState::Ready);
    assert!(Path::new(&ready.media.unwrap().path).is_file());
    assert_eq!(executor.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn service_runs_only_one_conversion_at_a_time() {
    let directory = tempfile::tempdir().unwrap();
    let first_source = directory.path().join("first.mkv");
    let second_source = directory.path().join("second.mkv");
    std::fs::write(&first_source, b"first").unwrap();
    std::fs::write(&second_source, b"second").unwrap();
    let executor = Arc::new(FakeExecutor::default());
    let service = PreviewService::with_executor(
        PreviewCache::new(directory.path().join("cache")),
        executor.clone(),
    )
    .unwrap();

    let first = service
        .request(preview_request(1, &first_source))
        .await
        .unwrap();
    let second = service
        .request(preview_request(2, &second_source))
        .await
        .unwrap();
    let _ = tokio::join!(
        wait_until_ready(&service, &first.request_id),
        wait_until_ready(&service, &second.request_id),
    );

    assert_eq!(executor.calls.load(Ordering::SeqCst), 2);
    assert_eq!(executor.max_active.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn missing_source_reuses_cache_and_updates_last_access() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.mkv");
    std::fs::write(&source, b"source").unwrap();
    let executor = Arc::new(FakeExecutor::default());
    let cache = PreviewCache::new(directory.path().join("cache"));
    let service = PreviewService::with_executor(cache.clone(), executor).unwrap();

    let queued = service.request(preview_request(44, &source)).await.unwrap();
    let _ = wait_until_ready(&service, &queued.request_id).await;
    let before = cache
        .find_latest_for_video(44)
        .unwrap()
        .unwrap()
        .last_accessed_at_millis;
    tokio::time::sleep(Duration::from_millis(2)).await;
    std::fs::remove_file(&source).unwrap();

    let cached = service
        .request(PreviewRequest {
            source_status: "missing".to_owned(),
            ..preview_request(44, &source)
        })
        .await
        .unwrap();
    let after = cache
        .find_latest_for_video(44)
        .unwrap()
        .unwrap()
        .last_accessed_at_millis;

    assert!(cached.media.unwrap().source_missing);
    assert!(after > before);
}

#[tokio::test]
async fn service_rebuilds_cache_after_automatic_cleanup_removed_ready_media() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.mkv");
    std::fs::write(&source, b"source").unwrap();
    let executor = Arc::new(FakeExecutor::default());
    let cache = PreviewCache::new(directory.path().join("cache"));
    let service = PreviewService::with_executor(cache.clone(), executor.clone()).unwrap();

    let first = service.request(preview_request(55, &source)).await.unwrap();
    let _ = wait_until_ready(&service, &first.request_id).await;
    cache.evict_video(55).unwrap();

    let second = service.request(preview_request(55, &source)).await.unwrap();
    let _ = wait_until_ready(&service, &second.request_id).await;

    assert_eq!(executor.calls.load(Ordering::SeqCst), 2);
    assert!(Path::new(&service.get(&second.request_id).unwrap().media.unwrap().path).is_file());
}

#[tokio::test]
async fn evicting_video_cancels_active_conversion_and_prevents_cache_recreation() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.mkv");
    std::fs::write(&source, b"source").unwrap();
    let executor = Arc::new(FakeExecutor::default());
    let cache = PreviewCache::new(directory.path().join("cache"));
    let service = PreviewService::with_executor(cache.clone(), executor).unwrap();

    let queued = service.request(preview_request(77, &source)).await.unwrap();
    service.evict_video(77).unwrap();
    tokio::time::sleep(Duration::from_millis(80)).await;

    assert!(service.get(&queued.request_id).is_none());
    assert!(cache.find_latest_for_video(77).unwrap().is_none());
}

#[derive(Default)]
struct SlowCancellationExecutor {
    started: AtomicUsize,
    finished: AtomicUsize,
}

#[async_trait]
impl PreviewExecutor for SlowCancellationExecutor {
    async fn prepare(
        &self,
        _execution: PreviewExecution,
        _progress: PreviewProgressCallback,
        cancellation: CancellationToken,
    ) -> Result<PreviewGenerationMethod, PreviewFailure> {
        self.started.store(1, Ordering::SeqCst);
        cancellation.cancelled().await;
        tokio::time::sleep(Duration::from_millis(30)).await;
        self.finished.store(1, Ordering::SeqCst);
        Err(PreviewFailure::new("cancelled", "预览任务已取消"))
    }
}

#[tokio::test]
async fn shutdown_waits_until_active_preview_jobs_finish() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.mkv");
    std::fs::write(&source, b"source").unwrap();
    let executor = Arc::new(SlowCancellationExecutor::default());
    let service = PreviewService::with_executor(
        PreviewCache::new(directory.path().join("cache")),
        executor.clone(),
    )
    .unwrap();

    service.request(preview_request(88, &source)).await.unwrap();
    for _ in 0..100 {
        if executor.started.load(Ordering::SeqCst) == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    service.shutdown().await;

    assert_eq!(executor.finished.load(Ordering::SeqCst), 1);
}

#[tokio::test]
#[ignore = "需要本机安装 FFmpeg/FFprobe"]
async fn real_ffmpeg_handles_remux_transcode_and_video_without_audio() {
    let ffmpeg = std::env::var("FFMPEG").unwrap_or_else(|_| "ffmpeg".to_owned());
    let ffprobe = std::env::var("FFPROBE").unwrap_or_else(|_| "ffprobe".to_owned());
    if std::process::Command::new(&ffmpeg)
        .arg("-version")
        .output()
        .is_err()
        || std::process::Command::new(&ffprobe)
            .arg("-version")
            .output()
            .is_err()
    {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let cache = PreviewCache::new(directory.path().join("cache"));
    let service = PreviewService::new(cache.clone()).unwrap();

    let samples = [
        (
            1,
            "中文 录像.mkv",
            vec!["-c:v", "libx264", "-pix_fmt", "yuv420p", "-c:a", "aac"],
        ),
        (
            2,
            "transcode.mkv",
            vec!["-c:v", "ffv1", "-c:a", "pcm_s16le"],
        ),
        (
            3,
            "silent.mkv",
            vec!["-c:v", "libx264", "-pix_fmt", "yuv420p", "-an"],
        ),
    ];
    for (video_id, name, codecs) in samples {
        let source = directory.path().join(name);
        let mut command = std::process::Command::new(&ffmpeg);
        command.args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=160x90:rate=10",
        ]);
        if name != "silent.mkv" {
            command.args(["-f", "lavfi", "-i", "sine=frequency=1000"]);
        }
        command.args(["-t", "0.5"]);
        command.args(codecs);
        let output = command.arg(&source).output().unwrap();
        assert!(
            output.status.success(),
            "生成测试视频失败：{}",
            String::from_utf8_lossy(&output.stderr)
        );

        let snapshot = service
            .request(PreviewRequest {
                video_id,
                source_path: source.to_string_lossy().into_owned(),
                source_status: "complete".to_owned(),
                ffmpeg_path: ffmpeg.clone(),
                ffprobe_path: ffprobe.clone(),
            })
            .await
            .unwrap();
        let ready = wait_until_ready(&service, &snapshot.request_id).await;
        assert_eq!(
            ready.state,
            PreviewState::Ready,
            "样本 {name}：{}",
            ready.error_message.unwrap_or_default(),
        );
        assert!(Path::new(&ready.media.unwrap().path).is_file());
    }

    let corrupted = directory.path().join("损坏文件.mkv");
    std::fs::write(&corrupted, b"not-a-video").unwrap();
    let failed = service
        .request(PreviewRequest {
            video_id: 4,
            source_path: corrupted.to_string_lossy().into_owned(),
            source_status: "complete".to_owned(),
            ffmpeg_path: ffmpeg.clone(),
            ffprobe_path: ffprobe.clone(),
        })
        .await
        .unwrap();
    let failed = wait_until_ready(&service, &failed.request_id).await;
    assert_eq!(failed.state, PreviewState::Failed);
    assert_eq!(failed.error_code.as_deref(), Some("media_invalid"));

    let repeated = service
        .request(PreviewRequest {
            video_id: 1,
            source_path: directory
                .path()
                .join("中文 录像.mkv")
                .to_string_lossy()
                .into_owned(),
            source_status: "complete".to_owned(),
            ffmpeg_path: ffmpeg.clone(),
            ffprobe_path: ffprobe.clone(),
        })
        .await
        .unwrap();
    assert_eq!(repeated.state, PreviewState::Ready);
    assert!(repeated.media.unwrap().cache_hit);

    let direct_mp4 = directory.path().join("direct.mp4");
    let output = std::process::Command::new(&ffmpeg)
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=160x90:rate=10",
            "-t",
            "0.5",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-an",
        ])
        .arg(&direct_mp4)
        .output()
        .unwrap();
    assert!(output.status.success());
    let direct = service
        .request(PreviewRequest {
            video_id: 5,
            source_path: direct_mp4.to_string_lossy().into_owned(),
            source_status: "complete".to_owned(),
            ffmpeg_path: ffmpeg.clone(),
            ffprobe_path: ffprobe.clone(),
        })
        .await
        .unwrap();
    assert_eq!(direct.state, PreviewState::Ready);
    assert!(!direct.media.unwrap().generated);

    assert_eq!(
        cache
            .find_latest_for_video(1)
            .unwrap()
            .unwrap()
            .generation_method,
        PreviewGenerationMethod::Remux,
    );
    assert_eq!(
        cache
            .find_latest_for_video(2)
            .unwrap()
            .unwrap()
            .generation_method,
        PreviewGenerationMethod::Transcode,
    );
    assert_eq!(
        cache
            .find_latest_for_video(3)
            .unwrap()
            .unwrap()
            .generation_method,
        PreviewGenerationMethod::Remux,
    );
}
