use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use dy_screen::asr::{AsrError, EngineResult, FrozenMediaSource, MediaInspection, MediaInspector};
use dy_screen_app_lib::ai::{
    AiCommandError, AiCommandService, AiCreateProjectRequest, AiEnvironmentCheckView,
    AiEnvironmentDiagnostic, AiHighlightRunStatus, AiInputSourceKind, AiInputStatus,
    AiJobController, AiPreflight, AiProjectService, AiProjectStatus, AiRepository, CredentialStore,
    FakeHighlightProvider, HighlightWorkflow, MemoryCredentialStore, NewAiHighlightRun,
    NewAiProjectInput, PreflightReport, RecognitionProfile, SourceFingerprint,
};
use dy_screen_app_lib::database::Database;
use dy_screen_app_lib::domain::{NewStreamer, NewVideo};
use tempfile::tempdir;
use tokio_util::sync::CancellationToken;

struct FakeInspector {
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl MediaInspector for FakeInspector {
    async fn inspect(
        &self,
        _source: &FrozenMediaSource,
        cancellation: CancellationToken,
    ) -> EngineResult<MediaInspection> {
        if cancellation.is_cancelled() {
            return Err(AsrError::cancelled());
        }
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(MediaInspection {
            duration_ms: 4_000,
            audio_present: true,
            audio_codec: Some("aac".to_owned()),
            audio_sample_rate_hz: Some(48_000),
            audio_channels: Some(2),
        })
    }
}

struct ReadyPreflight;

#[async_trait]
impl AiPreflight for ReadyPreflight {
    async fn check(&self) -> Result<PreflightReport, AsrError> {
        Ok(PreflightReport {
            ready: true,
            engine_id: "fake-engine".to_owned(),
            engine_version: "1".to_owned(),
            model_id: "fake-model".to_owned(),
            model_version: "1".to_owned(),
            platform_supported: true,
            sidecars_ready: true,
            models_ready: true,
            memory_ready: true,
            disk_ready: true,
            message: "本地 ASR 环境就绪".to_owned(),
        })
    }
}

struct FakeController {
    repository: AiRepository,
    enqueued: Mutex<Vec<i64>>,
}

#[async_trait]
impl AiJobController for FakeController {
    fn default_profile(&self) -> Result<RecognitionProfile, AiCommandError> {
        Ok(RecognitionProfile {
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
        })
    }

    async fn enqueue_project(&self, project_id: i64) -> Result<(), AiCommandError> {
        self.enqueued.lock().unwrap().push(project_id);
        Ok(())
    }

    async fn cancel_project(&self, project_id: i64) -> Result<(), AiCommandError> {
        self.repository
            .cancel_project(project_id)
            .map(|_| ())
            .map_err(|error| AiCommandError::new("cancel_failed", error.to_string(), true))
    }

    async fn retry_input(&self, input_id: i64) -> Result<(), AiCommandError> {
        self.repository
            .prepare_input_retry(input_id)
            .map(|_| ())
            .map_err(|error| AiCommandError::new("retry_failed", error.to_string(), true))
    }

    async fn diagnose(&self) -> Result<AiEnvironmentDiagnostic, AiCommandError> {
        Ok(AiEnvironmentDiagnostic {
            ready: true,
            platform: "test".to_owned(),
            engine_id: "fake-engine".to_owned(),
            engine_version: "1".to_owned(),
            model_id: "fake-model".to_owned(),
            model_version: "1".to_owned(),
            checks: vec![AiEnvironmentCheckView {
                code: "fake".to_owned(),
                passed: true,
                message: "测试资源完整".to_owned(),
            }],
            message: "本地 ASR 环境就绪".to_owned(),
            runtime: None,
        })
    }
}

fn command_service(
    database: Database,
    calls: Arc<AtomicUsize>,
) -> (AiCommandService, Arc<FakeController>) {
    let repository = AiRepository::new(database.clone());
    let controller = Arc::new(FakeController {
        repository: repository.clone(),
        enqueued: Mutex::new(Vec::new()),
    });
    let service = AiProjectService::new(
        database,
        Arc::new(FakeInspector { calls }),
        Arc::new(ReadyPreflight),
    );
    (
        AiCommandService::new(service, repository, controller.clone()),
        controller,
    )
}

#[tokio::test]
async fn arbitrary_grants_are_rejected_before_media_processes_and_backend_grants_are_one_time() {
    let directory = tempdir().unwrap();
    let source = directory.path().join("中文 视频.mp4");
    std::fs::write(&source, b"fake video").unwrap();
    let database = Database::open_in_memory().unwrap();
    database.migrate().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let (commands, _) = command_service(database, calls.clone());
    let project = commands
        .create_project(AiCreateProjectRequest {
            name: "授权测试".to_owned(),
            hotwords: Vec::new(),
        })
        .unwrap();

    let error = commands
        .import_local_grants(project.id, vec!["frontend-invented-path".to_owned()])
        .await
        .unwrap_err();
    assert_eq!(error.code, "untrusted_file_grant");
    assert_eq!(calls.load(Ordering::SeqCst), 0);

    let grants = commands
        .register_backend_file_selection(vec![source.clone()])
        .unwrap();
    assert_eq!(grants.len(), 1);
    let serialized = serde_json::to_string(&grants).unwrap();
    assert!(!serialized.contains(directory.path().to_string_lossy().as_ref()));
    let imported = commands
        .import_local_grants(project.id, vec![grants[0].grant_id.clone()])
        .await
        .unwrap();
    assert_eq!(imported.added.len(), 1);
    assert!(
        !serde_json::to_string(&imported)
            .unwrap()
            .contains(directory.path().to_string_lossy().as_ref())
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    let reused = commands
        .import_local_grants(project.id, vec![grants[0].grant_id.clone()])
        .await
        .unwrap_err();
    assert_eq!(reused.code, "untrusted_file_grant");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn completed_session_command_returns_counts_and_sanitized_project_detail() {
    let directory = tempdir().unwrap();
    let source = directory.path().join("历史整场.mkv");
    std::fs::write(&source, b"recorded session segment").unwrap();
    let database = Database::open_in_memory().unwrap();
    database.migrate().unwrap();
    let streamer = database
        .add_streamer(&NewStreamer::room(
            "正在直播的主播",
            "903",
            "room-903",
            true,
        ))
        .unwrap();
    let history = database
        .start_session(streamer.id, directory.path().to_str().unwrap())
        .unwrap();
    database
        .add_video(&NewVideo {
            session_id: history.id,
            path: source.to_string_lossy().into_owned(),
            started_at: Some("2026-07-22T01:00:00Z".to_owned()),
            ended_at: Some("2026-07-22T01:00:04Z".to_owned()),
            duration_seconds: Some(4),
            size_bytes: 24,
            audio_present: Some(true),
            status: "complete".to_owned(),
        })
        .unwrap();
    database
        .finish_session(history.id, "completed", None)
        .unwrap();
    let _active = database
        .start_session(streamer.id, directory.path().to_str().unwrap())
        .unwrap();
    let (commands, _) = command_service(database, Arc::new(AtomicUsize::new(0)));
    let project = commands
        .create_project(AiCreateProjectRequest {
            name: "整场命令".to_owned(),
            hotwords: Vec::new(),
        })
        .unwrap();

    let streamer_page = commands
        .list_replay_streamers(Some("903"), None, 500)
        .unwrap();
    assert_eq!(streamer_page.items.len(), 1);
    assert_eq!(streamer_page.items[0].streamer_id, streamer.id);
    let session_page = commands
        .list_replay_sessions(streamer.id, project.id, None, None, 500)
        .unwrap();
    assert_eq!(session_page.items.len(), 1);
    assert!(!session_page.items[0].fully_imported);
    let serialized_directory = serde_json::to_string(&session_page).unwrap();
    assert!(!serialized_directory.contains(source.to_string_lossy().as_ref()));

    let imported = commands
        .add_completed_session(project.id, history.id)
        .await
        .unwrap();

    assert_eq!(imported.added_count, 1);
    assert_eq!(imported.duplicate_count, 0);
    assert_eq!(imported.unavailable_count, 0);
    assert_eq!(imported.detail.inputs.len(), 1);
    assert!(imported.detail.inputs[0].video_id.is_some());
    let serialized = serde_json::to_value(&imported).unwrap();
    assert_eq!(serialized["addedCount"], 1);
    assert_eq!(serialized["duplicateCount"], 0);
    assert_eq!(serialized["unavailableCount"], 0);
    assert!(
        !serialized
            .to_string()
            .contains(source.to_string_lossy().as_ref())
    );
    let imported_page = commands
        .list_replay_sessions(streamer.id, project.id, None, None, 20)
        .unwrap();
    assert!(imported_page.items[0].fully_imported);
    assert_eq!(imported_page.items[0].imported_video_count, 1);

    let repeated = commands
        .add_completed_session(project.id, history.id)
        .await
        .unwrap();
    assert_eq!(repeated.added_count, 0);
    assert_eq!(repeated.duplicate_count, 1);
    assert_eq!(repeated.detail.inputs.len(), 1);
}

#[tokio::test]
async fn typed_commands_create_start_cancel_retry_query_and_diagnose_projects() {
    let directory = tempdir().unwrap();
    let source = directory.path().join("source.mp4");
    std::fs::write(&source, b"fake video").unwrap();
    let database = Database::open_in_memory().unwrap();
    database.migrate().unwrap();
    let repository = AiRepository::new(database.clone());
    let (commands, controller) = command_service(database, Arc::new(AtomicUsize::new(0)));
    let project = commands
        .create_project(AiCreateProjectRequest {
            name: "类型化命令".to_owned(),
            hotwords: vec![" 商品名 ".to_owned(), "商品名".to_owned()],
        })
        .unwrap();
    assert_eq!(project.recognition_profile.hotwords, vec!["商品名"]);
    let grant = commands
        .register_backend_file_selection(vec![PathBuf::from(&source)])
        .unwrap();
    commands
        .import_local_grants(project.id, vec![grant[0].grant_id.clone()])
        .await
        .unwrap();
    let queued = commands.start_project(project.id).await.unwrap();
    assert_eq!(queued.status, AiProjectStatus::Queued);
    assert_eq!(&*controller.enqueued.lock().unwrap(), &[project.id]);
    assert!(commands.diagnose().await.unwrap().ready);

    repository
        .transition_project(project.id, AiProjectStatus::Running)
        .unwrap();
    let input_id = repository.get_project(project.id).unwrap().inputs[0].id;
    repository
        .transition_input(input_id, AiInputStatus::Validating, None)
        .unwrap();
    repository
        .transition_input(
            input_id,
            AiInputStatus::Failed,
            Some(("fake_failure", "测试失败")),
        )
        .unwrap();
    repository.recompute_project_progress(project.id).unwrap();
    let retried = commands.retry_input(input_id).await.unwrap();
    assert_eq!(retried.project.status, AiProjectStatus::Queued);
    assert_eq!(retried.inputs[0].status, AiInputStatus::Pending);
    let cancelled = commands.cancel_project(project.id).await.unwrap();
    assert_eq!(cancelled.status, AiProjectStatus::Cancelled);
    assert_eq!(
        commands.get_project(project.id).unwrap().inputs[0].status,
        AiInputStatus::Cancelled
    );
    let retried_cancelled = commands.retry_input(input_id).await.unwrap();
    assert_eq!(retried_cancelled.project.status, AiProjectStatus::Queued);
    assert_eq!(retried_cancelled.inputs[0].status, AiInputStatus::Pending);
}

#[test]
fn highlight_resume_from_sync_tauri_command_thread_does_not_require_a_tokio_reactor() {
    let database = Database::open_in_memory().unwrap();
    database.migrate().unwrap();
    let repository = AiRepository::new(database.clone());
    let (commands, _) = command_service(database, Arc::new(AtomicUsize::new(0)));
    let project = repository
        .create_project("同步恢复精彩", &ReadyControllerProfile::profile())
        .unwrap();
    let source_path = "/tmp/sync-highlight-resume.mp4";
    let input = repository
        .add_input(
            project.id,
            NewAiProjectInput {
                position: 0,
                source_kind: AiInputSourceKind::LocalFile,
                video_id: None,
                display_name: "sync-highlight-resume.mp4".to_owned(),
                source_path: source_path.to_owned(),
                source_fingerprint: SourceFingerprint {
                    normalized_path: source_path.to_owned(),
                    size_bytes: 1,
                    modified_at_ms: 1,
                    video_id: None,
                },
                duration_ms: Some(20_000),
                audio_present: Some(true),
            },
        )
        .unwrap();
    repository.freeze_project(project.id).unwrap();
    repository
        .transition_project(project.id, AiProjectStatus::Running)
        .unwrap();
    repository
        .transition_input(input.id, AiInputStatus::Validating, None)
        .unwrap();
    repository
        .transition_input(input.id, AiInputStatus::Completed, None)
        .unwrap();
    repository.recompute_project_progress(project.id).unwrap();
    let run = repository
        .create_highlight_run(NewAiHighlightRun {
            project_id: project.id,
            model_id: "deepseek-chat".to_owned(),
            prompt_version: "highlight-v1".to_owned(),
            tags_snapshot: Vec::new(),
            skills_snapshot: vec!["generic-hook@1.0.0".to_owned()],
            analysis_goal: None,
            analysis_fingerprint: "sync-resume-no-reactor".to_owned(),
            qualified_score: 70,
            excellent_score: 80,
            total_segments: 0,
            total_chars: 0,
            estimated_batches: 0,
            user_authorized: true,
        })
        .unwrap();
    let credentials = MemoryCredentialStore::new();
    credentials.set("test-key").unwrap();
    let commands = commands.with_highlight_workflow(Arc::new(HighlightWorkflow::new(
        repository.clone(),
        Arc::new(FakeHighlightProvider::default()),
        Arc::new(credentials),
    )));

    let resumed = commands.resume_highlight_analysis(run.id).unwrap();
    assert_eq!(resumed.status, AiHighlightRunStatus::Pending);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        let status = repository.get_highlight_run(run.id).unwrap().status;
        if !matches!(
            status,
            AiHighlightRunStatus::Pending
                | AiHighlightRunStatus::Running
                | AiHighlightRunStatus::Candidates
                | AiHighlightRunStatus::Ranking
        ) {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "后台精彩恢复未结束");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

struct ReadyControllerProfile;

impl ReadyControllerProfile {
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
}
