use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use dy_screen::asr::{
    AsrError, AsrJobExecutor, EngineResult, RecordingActivityGate, SchedulerEvent,
    SchedulerEventSink, SchedulerJob, TranscriptionScheduler,
};
use dy_screen_app_lib::ai::{
    AiInputSourceKind, AiInputStatus, AiLifecycle, AiProjectStatus, AiRepository,
    NewAiProjectInput, NewAsrArtifact, RecognitionProfile, SourceFingerprint,
};
use dy_screen_app_lib::database::Database;
use tempfile::tempdir;
use tokio_util::sync::CancellationToken;

fn profile() -> RecognitionProfile {
    RecognitionProfile {
        engine_id: "fake-engine".to_owned(),
        engine_version: "1".to_owned(),
        model_id: "fake-model".to_owned(),
        model_version: "1".to_owned(),
        language_hint: Some("zh".to_owned()),
        vad_model_id: "fake-vad".to_owned(),
        vad_threshold_millis: 500,
        vad_padding_ms: 500,
        timestamp_policy: "segment".to_owned(),
        normalization_version: "zh-normalize-v1".to_owned(),
        hotwords: Vec::new(),
    }
}

fn fingerprint(path: &Path) -> SourceFingerprint {
    let metadata = std::fs::metadata(path).unwrap();
    SourceFingerprint {
        normalized_path: path.to_string_lossy().into_owned(),
        size_bytes: metadata.len(),
        modified_at_ms: metadata
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64,
        video_id: None,
    }
}

fn running_input(repository: &AiRepository, source: &Path) -> (i64, i64, SourceFingerprint) {
    let project = repository
        .create_project("生命周期测试", &profile())
        .unwrap();
    let source_fingerprint = fingerprint(source);
    let input = repository
        .add_input(
            project.id,
            NewAiProjectInput {
                position: 0,
                source_kind: AiInputSourceKind::LocalFile,
                video_id: None,
                display_name: "source.mp4".to_owned(),
                source_path: source.to_string_lossy().into_owned(),
                source_fingerprint: source_fingerprint.clone(),
                duration_ms: Some(4_000),
                audio_present: Some(true),
            },
        )
        .unwrap();
    repository.freeze_project(project.id).unwrap();
    repository
        .transition_project(project.id, AiProjectStatus::Running)
        .unwrap();
    for status in [
        AiInputStatus::Validating,
        AiInputStatus::PreparingAudio,
        AiInputStatus::DetectingSpeech,
        AiInputStatus::Transcribing,
    ] {
        repository.transition_input(input.id, status, None).unwrap();
    }
    (project.id, input.id, source_fingerprint)
}

struct IdleGate;

#[async_trait]
impl RecordingActivityGate for IdleGate {
    fn is_recording_active(&self) -> bool {
        false
    }

    async fn wait_until_idle(&self, cancellation: CancellationToken) -> EngineResult<()> {
        if cancellation.is_cancelled() {
            Err(AsrError::cancelled())
        } else {
            Ok(())
        }
    }
}

struct BlockingExecutor {
    started: Arc<AtomicBool>,
}

#[async_trait]
impl AsrJobExecutor for BlockingExecutor {
    async fn execute(
        &self,
        _job: SchedulerJob,
        cancellation: CancellationToken,
    ) -> EngineResult<()> {
        self.started.store(true, Ordering::SeqCst);
        cancellation.cancelled().await;
        Err(AsrError::cancelled())
    }
}

struct IgnoreEvents;

impl SchedulerEventSink for IgnoreEvents {
    fn publish(&self, _event: SchedulerEvent) {}
}

#[test]
fn abnormal_restart_recovers_database_state_and_stale_audio_without_process_state() {
    let directory = tempdir().unwrap();
    let source = directory.path().join("restart-source.mp4");
    std::fs::write(&source, b"restart source").unwrap();
    let database = Database::open_in_memory().unwrap();
    database.migrate().unwrap();
    let repository = AiRepository::new(database);
    let (project_id, input_id, _) = running_input(&repository, &source);
    let temporary_root = directory.path().join("asr-audio");
    let nested = temporary_root.join("stale-project");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::write(nested.join("stale-task.wav.part"), b"partial").unwrap();

    let report = AiLifecycle::new(repository.clone(), temporary_root)
        .recover_startup()
        .unwrap();
    assert_eq!(report.database.projects, 1);
    assert_eq!(report.database.inputs, 1);
    assert_eq!(report.removed_temporary_audio_files, 1);
    let restored = repository.get_project(project_id).unwrap();
    assert_eq!(restored.project.status, AiProjectStatus::Queued);
    assert_eq!(restored.inputs[0].id, input_id);
    assert_eq!(restored.inputs[0].status, AiInputStatus::Pending);
    assert!(!nested.exists());
}

#[tokio::test]
async fn explicit_shutdown_cancels_work_invalidates_pending_artifacts_and_cleans_nested_audio() {
    let directory = tempdir().unwrap();
    let source = directory.path().join("source.mp4");
    std::fs::write(&source, b"original media remains immutable").unwrap();
    let before = std::fs::read(&source).unwrap();
    let database = Database::open_in_memory().unwrap();
    database.migrate().unwrap();
    let repository = AiRepository::new(database);
    let (project_id, input_id, source_fingerprint) = running_input(&repository, &source);
    repository
        .create_artifact(NewAsrArtifact {
            source_fingerprint,
            recognition_profile_hash: profile().fingerprint().unwrap(),
            engine_id: "fake-engine".to_owned(),
            engine_version: "1".to_owned(),
            model_id: "fake-model".to_owned(),
            model_version: "1".to_owned(),
        })
        .unwrap();

    let temporary_root = directory.path().join("asr-audio");
    let nested = temporary_root
        .join(project_id.to_string())
        .join(input_id.to_string());
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::write(nested.join("project-1-input-1.wav.part"), b"partial").unwrap();
    std::fs::write(nested.join("project-1-input-1.wav"), b"complete temporary").unwrap();

    let started = Arc::new(AtomicBool::new(false));
    let scheduler = TranscriptionScheduler::start(
        Arc::new(BlockingExecutor {
            started: started.clone(),
        }),
        Arc::new(IdleGate),
        Arc::new(IgnoreEvents),
        4,
    );
    scheduler
        .enqueue(SchedulerJob {
            id: "project-1-input-1".to_owned(),
            project_id,
            input_id,
        })
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(1), async {
        while !started.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();

    let report = AiLifecycle::new(repository.clone(), temporary_root)
        .shutdown(&scheduler)
        .await
        .unwrap();
    assert_eq!(report.database.projects, 1);
    assert_eq!(report.database.inputs, 1);
    assert_eq!(report.database.artifacts, 1);
    assert_eq!(report.removed_temporary_audio_files, 2);
    let restored = repository.get_project(project_id).unwrap();
    assert_eq!(restored.project.status, AiProjectStatus::Queued);
    assert_eq!(restored.inputs[0].status, AiInputStatus::Pending);
    assert_eq!(
        restored.inputs[0].last_error_code.as_deref(),
        Some("interrupted")
    );
    assert!(!nested.exists());
    assert_eq!(std::fs::read(&source).unwrap(), before);
}
