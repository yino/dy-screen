use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use dy_screen::asr::{AsrError, EngineResult, FrozenMediaSource, MediaInspection, MediaInspector};
use dy_screen_app_lib::ai::{
    AiInputStatus, AiPreflight, AiProjectService, AiProjectStatus, PreflightReport,
    RecognitionProfile, TrustedLocalFile,
};
use dy_screen_app_lib::database::Database;
use dy_screen_app_lib::domain::{NewStreamer, NewVideo};
use tempfile::tempdir;
use tokio_util::sync::CancellationToken;

fn profile() -> RecognitionProfile {
    RecognitionProfile {
        engine_id: "whisper.cpp".to_owned(),
        engine_version: "v1.9.1".to_owned(),
        model_id: "whisper-small-multilingual-q5_1".to_owned(),
        model_version: "small-q5_1@5359861".to_owned(),
        language_hint: Some("zh".to_owned()),
        vad_model_id: "silero-vad-v6.2.0".to_owned(),
        vad_threshold_millis: 500,
        vad_padding_ms: 500,
        timestamp_policy: "segment".to_owned(),
        normalization_version: "zh-normalize-v1".to_owned(),
        hotwords: Vec::new(),
    }
}

struct FakeInspector;

#[async_trait]
impl MediaInspector for FakeInspector {
    async fn inspect(
        &self,
        source: &FrozenMediaSource,
        cancellation: CancellationToken,
    ) -> EngineResult<MediaInspection> {
        if cancellation.is_cancelled() {
            return Err(AsrError::cancelled());
        }
        let name = source
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        Ok(MediaInspection {
            duration_ms: if name.contains("long") { 8_000 } else { 4_000 },
            audio_present: !name.contains("no-audio"),
            audio_codec: Some("aac".to_owned()),
            audio_sample_rate_hz: Some(48_000),
            audio_channels: Some(2),
        })
    }
}

struct MutablePreflight {
    ready: AtomicBool,
}

impl MutablePreflight {
    fn new(ready: bool) -> Self {
        Self {
            ready: AtomicBool::new(ready),
        }
    }

    fn set_ready(&self, ready: bool) {
        self.ready.store(ready, Ordering::SeqCst);
    }
}

#[async_trait]
impl AiPreflight for MutablePreflight {
    async fn check(&self) -> Result<PreflightReport, AsrError> {
        let ready = self.ready.load(Ordering::SeqCst);
        Ok(PreflightReport {
            ready,
            engine_id: "whisper.cpp".to_owned(),
            engine_version: "v1.9.1".to_owned(),
            model_id: "whisper-small-multilingual-q5_1".to_owned(),
            model_version: "small-q5_1@5359861".to_owned(),
            platform_supported: true,
            sidecars_ready: ready,
            models_ready: ready,
            memory_ready: ready,
            disk_ready: ready,
            message: if ready {
                "本地 ASR 环境已就绪".to_owned()
            } else {
                "模型缺失，请重新安装应用".to_owned()
            },
        })
    }
}

fn service(preflight: Arc<MutablePreflight>) -> (Database, AiProjectService) {
    let database = Database::open_in_memory().unwrap();
    database.migrate().unwrap();
    let service = AiProjectService::new(database.clone(), Arc::new(FakeInspector), preflight);
    (database, service)
}

fn write_video(path: &Path, content: &[u8]) {
    std::fs::write(path, content).unwrap();
}

#[test]
fn opening_workspace_recording_completion_and_startup_do_not_create_projects() {
    let (_, service) = service(Arc::new(MutablePreflight::new(true)));
    assert!(service.open_workspace().unwrap().is_empty());
    assert!(service.open_workspace().unwrap().is_empty());
}

#[tokio::test]
async fn trusted_multi_file_import_preserves_order_deduplicates_and_never_copies_sources() {
    let directory = tempdir().unwrap();
    let first = directory.path().join("first.mp4");
    let second = directory.path().join("second long.mp4");
    write_video(&first, b"first source");
    write_video(&second, b"second source");
    let (_, service) = service(Arc::new(MutablePreflight::new(true)));
    let project = service.create_draft("本地视频", &profile()).unwrap();

    let result = service
        .import_local_files(
            project.id,
            vec![
                TrustedLocalFile::new("grant-1", first.clone()),
                TrustedLocalFile::new("grant-2", second.clone()),
                TrustedLocalFile::new("grant-duplicate", first.clone()),
                TrustedLocalFile::new("", directory.path().join("arbitrary.mp4")),
                TrustedLocalFile::new("grant-missing", directory.path().join("missing.mp4")),
            ],
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(result.added.len(), 2);
    assert_eq!(result.added[0].display_name, "first.mp4");
    assert_eq!(result.added[1].display_name, "second long.mp4");
    assert_eq!(result.added[1].duration_ms, Some(8_000));
    assert_eq!(result.rejected.len(), 3);
    assert!(
        result
            .rejected
            .iter()
            .any(|item| item.code == "duplicate_input")
    );
    assert!(
        result
            .rejected
            .iter()
            .any(|item| item.code == "untrusted_file_grant")
    );
    assert_eq!(std::fs::read(first).unwrap(), b"first source");
    assert_eq!(std::fs::read(second).unwrap(), b"second source");
    assert_eq!(service.open_workspace().unwrap().len(), 1);
}

#[tokio::test]
async fn completed_session_expands_ordered_videos_and_marks_missing_segments() {
    let directory = tempdir().unwrap();
    let existing = directory.path().join("001.mkv");
    write_video(&existing, b"recorded segment");
    let missing = directory.path().join("002.mkv");
    let (database, service) = service(Arc::new(MutablePreflight::new(true)));
    let streamer = database
        .add_streamer(&NewStreamer::room("主播", "901", "room-901", false))
        .unwrap();
    let active = database
        .start_session(streamer.id, directory.path().to_str().unwrap())
        .unwrap();
    let project = service.create_draft("整场直播", &profile()).unwrap();
    let error = service
        .select_completed_session(project.id, active.id, CancellationToken::new())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("仍在录制"));

    database
        .add_video(&NewVideo {
            session_id: active.id,
            path: existing.to_string_lossy().into_owned(),
            started_at: Some("2026-07-22T01:00:00Z".to_owned()),
            ended_at: Some("2026-07-22T01:00:04Z".to_owned()),
            duration_seconds: Some(4),
            size_bytes: 16,
            audio_present: Some(true),
            status: "complete".to_owned(),
        })
        .unwrap();
    database
        .add_video(&NewVideo {
            session_id: active.id,
            path: missing.to_string_lossy().into_owned(),
            started_at: Some("2026-07-22T01:00:04Z".to_owned()),
            ended_at: Some("2026-07-22T01:00:08Z".to_owned()),
            duration_seconds: Some(4),
            size_bytes: 20,
            audio_present: Some(true),
            status: "complete".to_owned(),
        })
        .unwrap();
    database
        .finish_session(active.id, "completed", None)
        .unwrap();

    let inputs = service
        .select_completed_session(project.id, active.id, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(inputs.len(), 2);
    assert_eq!(inputs[0].display_name, "001.mkv");
    assert_eq!(inputs[0].status, AiInputStatus::Pending);
    assert_eq!(inputs[1].display_name, "002.mkv");
    assert_eq!(inputs[1].status, AiInputStatus::Failed);
    assert!(inputs[1].video_id.is_some());
}

#[tokio::test]
async fn summary_and_preflight_keep_draft_on_failure_then_freeze_on_success() {
    let directory = tempdir().unwrap();
    let valid = directory.path().join("valid.mp4");
    let no_audio = directory.path().join("no-audio.mp4");
    write_video(&valid, b"valid");
    write_video(&no_audio, b"no audio");
    let preflight = Arc::new(MutablePreflight::new(false));
    let (_, service) = service(preflight.clone());
    let project = service.create_draft("启动检查", &profile()).unwrap();
    service
        .import_local_files(
            project.id,
            vec![
                TrustedLocalFile::new("grant-valid", valid),
                TrustedLocalFile::new("grant-no-audio", no_audio),
            ],
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let summary = service.summary(project.id).await.unwrap();
    assert_eq!(summary.total_inputs, 2);
    assert_eq!(summary.valid_inputs, 1);
    assert_eq!(summary.total_duration_ms, 8_000);
    assert!(!summary.environment_ready);

    let error = service.start_analysis(project.id).await.unwrap_err();
    assert!(error.to_string().contains("模型缺失"));
    assert_eq!(
        service.open_workspace().unwrap()[0].status,
        AiProjectStatus::Draft
    );

    preflight.set_ready(true);
    let queued = service.start_analysis(project.id).await.unwrap();
    assert_eq!(queued.status, AiProjectStatus::Queued);
    assert!(queued.input_frozen);
}
