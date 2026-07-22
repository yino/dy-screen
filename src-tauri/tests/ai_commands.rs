use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use dy_screen::asr::{AsrError, EngineResult, FrozenMediaSource, MediaInspection, MediaInspector};
use dy_screen_app_lib::ai::{
    AiCommandError, AiCommandService, AiCreateProjectRequest, AiEnvironmentCheckView,
    AiEnvironmentDiagnostic, AiInputStatus, AiJobController, AiPreflight, AiProjectService,
    AiProjectStatus, AiRepository, PreflightReport, RecognitionProfile,
};
use dy_screen_app_lib::database::Database;
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
}
