#[cfg(unix)]
use std::fs;
use std::path::Path;
#[cfg(unix)]
use std::sync::atomic::{AtomicUsize, Ordering};
#[cfg(unix)]
use std::sync::{Arc, Mutex};
#[cfg(unix)]
use std::time::Duration;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use dy_screen::error::RecorderError;
#[cfg(unix)]
use dy_screen::manager::EventSink;
#[cfg(unix)]
use dy_screen::model::JobEvent;
use dy_screen::model::{Protocol, SelectedStream};
use dy_screen::recorder::{FfmpegConfig, build_ffmpeg_plan, check_ffmpeg};
#[cfg(unix)]
use dy_screen::recorder::{FfmpegRecorder, RecordingConfig};
#[cfg(unix)]
use tokio_util::sync::CancellationToken;

fn selected_stream() -> SelectedStream {
    SelectedStream {
        quality: "HD1".to_owned(),
        protocol: Protocol::Flv,
        url: "https://pull.example/live.flv?auth_key=top-secret".to_owned(),
        fell_back: false,
    }
}

#[derive(Default)]
#[cfg(unix)]
struct CapturingSink {
    events: Mutex<Vec<JobEvent>>,
}

#[derive(Default)]
#[cfg(unix)]
struct RejectFirstSegmentSink {
    attempts: AtomicUsize,
    accepted: Mutex<Vec<JobEvent>>,
}

#[cfg(unix)]
impl EventSink for RejectFirstSegmentSink {
    fn emit(&self, event: JobEvent) -> bool {
        if matches!(event, JobEvent::SegmentFinalized { .. }) {
            let attempt = self.attempts.fetch_add(1, Ordering::SeqCst);
            if attempt == 0 {
                return false;
            }
            self.accepted.lock().expect("event lock").push(event);
        }
        true
    }
}

#[cfg(unix)]
impl EventSink for CapturingSink {
    fn emit(&self, event: JobEvent) -> bool {
        self.events.lock().expect("event lock").push(event);
        true
    }
}

#[test]
fn builds_shell_free_segmented_stream_copy_plan() {
    let config = FfmpegConfig {
        executable: "/opt/homebrew/bin/ffmpeg".into(),
        segment_seconds: 60,
        user_agent: "dy-screen-test".to_owned(),
    };

    let plan = build_ffmpeg_plan(
        &config,
        &selected_stream(),
        Path::new("recordings/room/session"),
    )
    .expect("valid FFmpeg plan");
    let args: Vec<String> = plan
        .args
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();

    assert_eq!(plan.program, Path::new("/opt/homebrew/bin/ffmpeg"));
    assert!(args.windows(2).any(|pair| pair == ["-map", "0:v:0"]));
    assert!(args.windows(2).any(|pair| pair == ["-map", "0:a:0?"]));
    assert!(args.windows(2).any(|pair| pair == ["-c", "copy"]));
    assert!(args.windows(2).any(|pair| pair == ["-f", "segment"]));
    assert!(args.windows(2).any(|pair| pair == ["-segment_time", "60"]));
    assert!(args.windows(2).any(|pair| pair == ["-strftime", "1"]));
    assert!(
        args.windows(2)
            .any(|pair| pair[0] == "-segment_list" && pair[1].ends_with("segments.csv"))
    );
    assert!(
        args.windows(2)
            .any(|pair| pair == ["-segment_list_type", "csv"])
    );
    assert!(args.iter().any(|arg| arg.ends_with("%Y%m%d-%H%M%S.mkv")));
    assert!(args.iter().any(|arg| arg == &selected_stream().url));
    assert!(!format!("{plan:?}").contains("top-secret"));
}

#[test]
fn rejects_zero_segment_duration() {
    let config = FfmpegConfig {
        executable: "ffmpeg".into(),
        segment_seconds: 0,
        user_agent: "dy-screen-test".to_owned(),
    };
    let error = build_ffmpeg_plan(&config, &selected_stream(), Path::new("out")).unwrap_err();
    assert!(matches!(error, RecorderError::InvalidSegmentDuration));
}

#[tokio::test]
async fn reports_missing_ffmpeg_executable() {
    let error = check_ffmpeg(Path::new("/definitely/missing/dy-screen-ffmpeg"))
        .await
        .unwrap_err();
    assert!(matches!(error, RecorderError::FfmpegUnavailable { .. }));
}

#[cfg(unix)]
#[tokio::test]
async fn supervises_fake_ffmpeg_and_collects_completed_segments() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let fake_ffmpeg = temp.path().join("fake-ffmpeg");
    write_fake_ffmpeg(&fake_ffmpeg, false);
    let recorder = FfmpegRecorder::new(RecordingConfig {
        ffmpeg: FfmpegConfig {
            executable: fake_ffmpeg,
            segment_seconds: 60,
            user_agent: "dy-screen-test".to_owned(),
        },
        ffprobe_executable: fake_ffprobe(temp.path(), true),
        probe_timeout: Duration::from_secs(5),
        output_root: temp.path().join("recordings"),
        shutdown_timeout: Duration::from_secs(1),
    });

    let result = recorder
        .record_selected(
            "https://live.douyin.com/room".to_owned(),
            "room/unsafe".to_owned(),
            selected_stream(),
            CancellationToken::new(),
        )
        .await;

    assert!(result.success, "{:?}", result.error);
    assert_eq!(result.audio_present, Some(true));
    assert_eq!(result.segments.len(), 1, "{result:?}");
    assert!(result.segments[0].exists());
    assert!(
        result
            .output_dir
            .as_ref()
            .expect("output directory")
            .to_string_lossy()
            .contains("room_unsafe")
    );
    let serialized = serde_json::to_string(&result).expect("serializable result");
    assert!(!serialized.contains("top-secret"));
}

#[cfg(unix)]
#[tokio::test]
async fn cancellation_requests_graceful_shutdown_and_preserves_segments() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let fake_ffmpeg = temp.path().join("fake-ffmpeg-waits");
    write_fake_ffmpeg(&fake_ffmpeg, true);
    let recorder = FfmpegRecorder::new(RecordingConfig {
        ffmpeg: FfmpegConfig {
            executable: fake_ffmpeg,
            segment_seconds: 60,
            user_agent: "dy-screen-test".to_owned(),
        },
        ffprobe_executable: fake_ffprobe(temp.path(), true),
        probe_timeout: Duration::from_secs(5),
        output_root: temp.path().join("recordings"),
        shutdown_timeout: Duration::from_secs(1),
    });
    let output_root = temp.path().join("recordings");
    let cancellation = CancellationToken::new();
    let recording_cancellation = cancellation.clone();
    let recording = tokio::spawn(async move {
        recorder
            .record_selected(
                "https://live.douyin.com/room".to_owned(),
                "room".to_owned(),
                selected_stream(),
                recording_cancellation,
            )
            .await
    });
    let mut segment_started = false;
    for _ in 0..500 {
        if contains_mkv(&output_root) {
            segment_started = true;
            break;
        }
        if recording.is_finished() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(segment_started, "fake FFmpeg never created its segment");
    cancellation.cancel();
    let result = recording.await.expect("recording task");

    assert!(!result.success);
    assert_eq!(result.error.as_deref(), Some("recording was cancelled"));
    assert_eq!(result.segments.len(), 1, "{result:?}");
    assert!(result.segments[0].exists());
}

#[cfg(unix)]
#[tokio::test]
async fn concurrent_same_room_jobs_use_unique_session_directories() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let fake_ffmpeg = temp.path().join("fake-ffmpeg");
    write_fake_ffmpeg(&fake_ffmpeg, false);
    let recorder = FfmpegRecorder::new(RecordingConfig {
        ffmpeg: FfmpegConfig {
            executable: fake_ffmpeg,
            segment_seconds: 60,
            user_agent: "dy-screen-test".to_owned(),
        },
        ffprobe_executable: fake_ffprobe(temp.path(), true),
        probe_timeout: Duration::from_secs(5),
        output_root: temp.path().join("recordings"),
        shutdown_timeout: Duration::from_secs(1),
    });

    let first = recorder.record_selected(
        "https://live.douyin.com/room".to_owned(),
        "same-room".to_owned(),
        selected_stream(),
        CancellationToken::new(),
    );
    let second = recorder.record_selected(
        "https://live.douyin.com/room".to_owned(),
        "same-room".to_owned(),
        selected_stream(),
        CancellationToken::new(),
    );
    let (first, second) = tokio::join!(first, second);

    assert_ne!(first.output_dir, second.output_dir);
    assert!(first.success);
    assert!(second.success);
}

#[cfg(unix)]
#[tokio::test]
async fn abnormal_exit_separates_unlisted_partial_segments() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let fake_ffmpeg = temp.path().join("fake-ffmpeg-fails");
    write_failing_fake_ffmpeg(&fake_ffmpeg);
    let recorder = FfmpegRecorder::new(RecordingConfig {
        ffmpeg: FfmpegConfig {
            executable: fake_ffmpeg,
            segment_seconds: 60,
            user_agent: "dy-screen-test".to_owned(),
        },
        ffprobe_executable: fake_ffprobe(temp.path(), true),
        probe_timeout: Duration::from_secs(5),
        output_root: temp.path().join("recordings"),
        shutdown_timeout: Duration::from_secs(1),
    });

    let result = recorder
        .record_selected(
            "https://live.douyin.com/room".to_owned(),
            "room".to_owned(),
            selected_stream(),
            CancellationToken::new(),
        )
        .await;

    assert!(!result.success);
    assert_eq!(result.segments.len(), 1);
    assert_eq!(result.partial_segments.len(), 1);
    assert_ne!(result.segments[0], result.partial_segments[0]);
}

#[cfg(unix)]
#[tokio::test]
async fn reports_when_completed_segment_has_no_audio_track() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let fake_ffmpeg = temp.path().join("fake-ffmpeg");
    write_fake_ffmpeg(&fake_ffmpeg, false);
    let recorder = FfmpegRecorder::new(RecordingConfig {
        ffmpeg: FfmpegConfig {
            executable: fake_ffmpeg,
            segment_seconds: 60,
            user_agent: "dy-screen-test".to_owned(),
        },
        ffprobe_executable: fake_ffprobe(temp.path(), false),
        probe_timeout: Duration::from_secs(5),
        output_root: temp.path().join("recordings"),
        shutdown_timeout: Duration::from_secs(1),
    });

    let result = recorder
        .record_selected(
            "https://live.douyin.com/room".to_owned(),
            "room".to_owned(),
            selected_stream(),
            CancellationToken::new(),
        )
        .await;

    assert!(result.success);
    assert_eq!(result.audio_present, Some(false));
}

#[cfg(unix)]
#[tokio::test]
async fn emits_recording_and_finalized_segment_events_from_core() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let fake_ffmpeg = temp.path().join("fake-ffmpeg");
    write_fake_ffmpeg(&fake_ffmpeg, false);
    let recorder = FfmpegRecorder::new(RecordingConfig {
        ffmpeg: FfmpegConfig {
            executable: fake_ffmpeg,
            segment_seconds: 60,
            user_agent: "dy-screen-test".to_owned(),
        },
        ffprobe_executable: fake_ffprobe(temp.path(), true),
        probe_timeout: Duration::from_secs(5),
        output_root: temp.path().join("recordings"),
        shutdown_timeout: Duration::from_secs(1),
    });
    let sink = Arc::new(CapturingSink::default());

    let result = recorder
        .record_selected_with_event_sink(
            "https://live.douyin.com/room".to_owned(),
            "room".to_owned(),
            selected_stream(),
            CancellationToken::new(),
            sink.clone(),
        )
        .await;

    assert!(result.success);
    let events = sink.events.lock().expect("event lock");
    assert!(
        events
            .iter()
            .any(|event| matches!(event, JobEvent::RecordingStarted { .. }))
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, JobEvent::SegmentFinalized { .. }))
    );
}

#[cfg(unix)]
#[tokio::test]
async fn emits_completed_segment_before_long_running_ffmpeg_exits() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let fake_ffmpeg = temp.path().join("fake-ffmpeg");
    write_fake_ffmpeg(&fake_ffmpeg, true);
    let recorder = FfmpegRecorder::new(RecordingConfig {
        ffmpeg: FfmpegConfig {
            executable: fake_ffmpeg,
            segment_seconds: 60,
            user_agent: "dy-screen-test".to_owned(),
        },
        ffprobe_executable: fake_ffprobe(temp.path(), true),
        probe_timeout: Duration::from_millis(500),
        output_root: temp.path().join("recordings"),
        shutdown_timeout: Duration::from_secs(1),
    });
    let sink = Arc::new(CapturingSink::default());
    let cancellation = CancellationToken::new();
    let task = tokio::spawn({
        let sink = sink.clone();
        let cancellation = cancellation.clone();
        async move {
            recorder
                .record_selected_with_event_sink(
                    "https://live.douyin.com/room".to_owned(),
                    "room".to_owned(),
                    selected_stream(),
                    cancellation,
                    sink,
                )
                .await
        }
    });

    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let finalized = sink
                .events
                .lock()
                .expect("event lock")
                .iter()
                .any(|event| matches!(event, JobEvent::SegmentFinalized { .. }));
            if finalized {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("completed segment should be emitted while FFmpeg is still running");

    assert!(!task.is_finished());
    {
        let events = sink.events.lock().expect("event lock");
        let finalized = events
            .iter()
            .find_map(|event| match event {
                JobEvent::SegmentFinalized {
                    started_at,
                    ended_at,
                    duration_seconds,
                    ..
                } => Some((started_at, ended_at, duration_seconds)),
                _ => None,
            })
            .expect("finalized segment event");
        assert!(finalized.0.is_some());
        assert!(finalized.1.is_some());
        assert_eq!(*finalized.2, Some(6));
    }
    cancellation.cancel();
    let _ = task.await.expect("recording task");
    let finalized_count = sink
        .events
        .lock()
        .expect("event lock")
        .iter()
        .filter(|event| matches!(event, JobEvent::SegmentFinalized { .. }))
        .count();
    assert_eq!(finalized_count, 1, "final scan must not emit duplicates");
}

#[cfg(unix)]
#[tokio::test]
async fn retries_finalized_segment_until_event_sink_accepts_it() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let fake_ffmpeg = temp.path().join("fake-ffmpeg");
    write_fake_ffmpeg(&fake_ffmpeg, true);
    let recorder = FfmpegRecorder::new(RecordingConfig {
        ffmpeg: FfmpegConfig {
            executable: fake_ffmpeg,
            segment_seconds: 60,
            user_agent: "dy-screen-test".to_owned(),
        },
        ffprobe_executable: fake_ffprobe(temp.path(), true),
        probe_timeout: Duration::from_millis(200),
        output_root: temp.path().join("recordings"),
        shutdown_timeout: Duration::from_secs(1),
    });
    let sink = Arc::new(RejectFirstSegmentSink::default());
    let cancellation = CancellationToken::new();
    let task = tokio::spawn({
        let sink = sink.clone();
        let cancellation = cancellation.clone();
        async move {
            recorder
                .record_selected_with_event_sink(
                    "https://live.douyin.com/room".to_owned(),
                    "room".to_owned(),
                    selected_stream(),
                    cancellation,
                    sink,
                )
                .await
        }
    });

    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if !sink.accepted.lock().expect("event lock").is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("rejected segment should be retried");

    assert!(sink.attempts.load(Ordering::SeqCst) >= 2);
    assert!(!task.is_finished());
    cancellation.cancel();
    let _ = task.await.expect("recording task");
}

#[cfg(unix)]
#[tokio::test]
async fn parses_completed_manifest_when_output_path_contains_comma() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let fake_ffmpeg = temp.path().join("fake-ffmpeg");
    write_fake_ffmpeg(&fake_ffmpeg, false);
    let recorder = FfmpegRecorder::new(RecordingConfig {
        ffmpeg: FfmpegConfig {
            executable: fake_ffmpeg,
            segment_seconds: 60,
            user_agent: "dy-screen-test".to_owned(),
        },
        ffprobe_executable: fake_ffprobe(temp.path(), true),
        probe_timeout: Duration::from_secs(5),
        output_root: temp.path().join("recordings,with-comma"),
        shutdown_timeout: Duration::from_secs(1),
    });

    let result = recorder
        .record_selected(
            "https://live.douyin.com/room".to_owned(),
            "room".to_owned(),
            selected_stream(),
            CancellationToken::new(),
        )
        .await;

    assert!(result.success);
    assert_eq!(result.segments.len(), 1, "{result:?}");
    assert!(result.partial_segments.is_empty(), "{result:?}");
}

#[cfg(unix)]
#[tokio::test]
async fn slow_audio_probe_is_bounded_and_does_not_block_completion() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let fake_ffmpeg = temp.path().join("fake-ffmpeg");
    write_fake_ffmpeg(&fake_ffmpeg, false);
    let recorder = FfmpegRecorder::new(RecordingConfig {
        ffmpeg: FfmpegConfig {
            executable: fake_ffmpeg,
            segment_seconds: 60,
            user_agent: "dy-screen-test".to_owned(),
        },
        ffprobe_executable: fake_slow_ffprobe(temp.path()),
        probe_timeout: Duration::from_millis(50),
        output_root: temp.path().join("recordings"),
        shutdown_timeout: Duration::from_secs(1),
    });

    let result = tokio::time::timeout(
        Duration::from_secs(5),
        recorder.record_selected(
            "https://live.douyin.com/room".to_owned(),
            "room".to_owned(),
            selected_stream(),
            CancellationToken::new(),
        ),
    )
    .await
    .expect("recording completion must be bounded");

    assert!(result.success);
    assert_eq!(result.audio_present, None);
}

#[cfg(unix)]
fn write_fake_ffmpeg(path: &Path, wait_for_stdin: bool) {
    let wait = if wait_for_stdin { "read line\n" } else { "" };
    let script = format!(
        "#!/bin/sh\nif [ \"$1\" = \"-version\" ]; then exit 0; fi\nlast=\"\"\nmanifest=\"\"\nprevious=\"\"\nfor arg in \"$@\"; do if [ \"$previous\" = \"-segment_list\" ]; then manifest=\"$arg\"; fi; previous=\"$arg\"; last=\"$arg\"; done\nout=$(printf '%s' \"$last\" | sed 's/%Y%m%d-%H%M%S/20260718-120000/')\nmkdir -p \"$(dirname \"$out\")\"\nprintf demo > \"$out\"\nprintf '\"%s\",0,6\\n' \"$out\" > \"$manifest\"\n{wait}exit 0\n"
    );
    fs::write(path, script).expect("write fake FFmpeg");
    let mut permissions = fs::metadata(path)
        .expect("fake FFmpeg metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("make fake FFmpeg executable");
}

#[cfg(unix)]
fn write_failing_fake_ffmpeg(path: &Path) {
    let script = "#!/bin/sh\nif [ \"$1\" = \"-version\" ]; then exit 0; fi\nlast=\"\"\nmanifest=\"\"\nprevious=\"\"\nfor arg in \"$@\"; do if [ \"$previous\" = \"-segment_list\" ]; then manifest=\"$arg\"; fi; previous=\"$arg\"; last=\"$arg\"; done\ncomplete=$(printf '%s' \"$last\" | sed 's/%Y%m%d-%H%M%S/20260718-120000/')\npartial=$(printf '%s' \"$last\" | sed 's/%Y%m%d-%H%M%S/20260718-120100/')\nmkdir -p \"$(dirname \"$complete\")\"\nprintf complete > \"$complete\"\nprintf partial > \"$partial\"\nprintf '\"%s\",0,6\\n' \"$complete\" > \"$manifest\"\nexit 2\n";
    fs::write(path, script).expect("write failing fake FFmpeg");
    let mut permissions = fs::metadata(path)
        .expect("fake FFmpeg metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("make fake FFmpeg executable");
}

#[cfg(unix)]
fn fake_ffprobe(directory: &Path, audio_present: bool) -> std::path::PathBuf {
    let path = directory.join(if audio_present {
        "fake-ffprobe-audio"
    } else {
        "fake-ffprobe-no-audio"
    });
    let output = if audio_present { "0" } else { "" };
    let script = format!("#!/bin/sh\nprintf '{output}\\n'\n");
    fs::write(&path, script).expect("write fake FFprobe");
    let mut permissions = fs::metadata(&path)
        .expect("fake FFprobe metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&path, permissions).expect("make fake FFprobe executable");
    path
}

#[cfg(unix)]
fn fake_slow_ffprobe(directory: &Path) -> std::path::PathBuf {
    let path = directory.join("fake-ffprobe-slow");
    fs::write(&path, "#!/bin/sh\nsleep 10\nprintf '0\\n'\n").expect("write slow FFprobe");
    let mut permissions = fs::metadata(&path)
        .expect("slow FFprobe metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&path, permissions).expect("make slow FFprobe executable");
    path
}

#[cfg(unix)]
fn contains_mkv(directory: &Path) -> bool {
    let Ok(entries) = fs::read_dir(directory) else {
        return false;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() && contains_mkv(&path) {
            return true;
        }
        if path.extension().is_some_and(|extension| extension == "mkv") {
            return true;
        }
    }
    false
}
