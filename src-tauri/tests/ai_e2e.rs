use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use dy_screen::asr::{
    AsrCapabilities, AsrConfidenceKind, AsrEngine, AsrEngineIdentity, AsrEnvironmentCheck,
    AsrEnvironmentReport, AsrError, AsrErrorKind, AsrProgressSink, AsrRequest, AsrResult,
    AsrSegment, AudioPreparationCapabilities, AudioPreparationRequest, EngineResult,
    FrozenMediaSource, MediaAudioPreparer, MediaInspection, MediaInspector, PcmPipeProcess,
    PreparedAudio, PreparedAudioFormat, PreparedAudioLease, SpeechRegion, TextNormalizer,
    TimestampPolicy, VadConfig, VadEngine,
};
use dy_screen_app_lib::ai::{
    AiInputProcessor, AiInputSourceKind, AiLifecycle, AiPreflight, AiProcessOutcome,
    AiProjectService, AiProjectStatus, AiRepository, AiTranscriptProjection, PreflightReport,
    RecognitionProfile, TrustedLocalFile,
};
use dy_screen_app_lib::database::Database;
use dy_screen_app_lib::domain::{NewStreamer, NewVideo};
use tempfile::tempdir;
use tokio_util::sync::CancellationToken;

#[derive(Default)]
struct PipelineCalls {
    inspect: AtomicUsize,
    prepare: AtomicUsize,
    vad: AtomicUsize,
    asr: AtomicUsize,
}

struct E2eInspector(Arc<PipelineCalls>);

#[async_trait]
impl MediaInspector for E2eInspector {
    async fn inspect(
        &self,
        _source: &FrozenMediaSource,
        cancellation: CancellationToken,
    ) -> EngineResult<MediaInspection> {
        if cancellation.is_cancelled() {
            return Err(AsrError::cancelled());
        }
        self.0.inspect.fetch_add(1, Ordering::SeqCst);
        Ok(MediaInspection {
            duration_ms: 4_000,
            audio_present: true,
            audio_codec: Some("aac".to_owned()),
            audio_sample_rate_hz: Some(48_000),
            audio_channels: Some(2),
        })
    }
}

struct E2ePreparer(Arc<PipelineCalls>);

#[async_trait]
impl MediaAudioPreparer for E2ePreparer {
    fn capabilities(&self) -> AudioPreparationCapabilities {
        AudioPreparationCapabilities {
            pcm_pipe: false,
            temporary_wav: true,
        }
    }

    async fn prepare_temporary_wav(
        &self,
        request: &AudioPreparationRequest,
        cancellation: CancellationToken,
    ) -> EngineResult<PreparedAudioLease> {
        if cancellation.is_cancelled() {
            return Err(AsrError::cancelled());
        }
        self.0.prepare.fetch_add(1, Ordering::SeqCst);
        if request.source.path.to_string_lossy().contains("失败") {
            return Err(AsrError::new(
                AsrErrorKind::ProcessFailed,
                "fake_prepare_failed",
                "测试音频准备失败",
                true,
            ));
        }
        std::fs::create_dir_all(&request.temporary_root).unwrap();
        let path = request.temporary_root.join("prepared.wav");
        std::fs::write(&path, b"fake pcm").unwrap();
        Ok(PreparedAudioLease::new(PreparedAudio {
            path,
            format: PreparedAudioFormat::PcmS16LeWav,
            sample_rate_hz: 16_000,
            channels: 1,
            duration_ms: request.duration_ms,
        }))
    }

    async fn open_pcm_pipe(
        &self,
        _request: &AudioPreparationRequest,
        _cancellation: CancellationToken,
    ) -> EngineResult<PcmPipeProcess> {
        Err(AsrError::invalid_input(
            "e2e_pipe_unavailable",
            "端到端测试不使用 PCM 管道",
        ))
    }
}

struct E2eVad(Arc<PipelineCalls>);

#[async_trait]
impl VadEngine for E2eVad {
    async fn detect(
        &self,
        audio: &PreparedAudio,
        _config: &VadConfig,
        cancellation: CancellationToken,
    ) -> EngineResult<Vec<SpeechRegion>> {
        if cancellation.is_cancelled() {
            return Err(AsrError::cancelled());
        }
        self.0.vad.fetch_add(1, Ordering::SeqCst);
        Ok(vec![SpeechRegion {
            start_ms: 0,
            end_ms: audio.duration_ms,
        }])
    }
}

struct E2eEngine {
    calls: Arc<PipelineCalls>,
    identity: AsrEngineIdentity,
}

#[async_trait]
impl AsrEngine for E2eEngine {
    fn identity(&self) -> AsrEngineIdentity {
        self.identity.clone()
    }

    fn capabilities(&self) -> AsrCapabilities {
        AsrCapabilities {
            timestamp_policies: vec![TimestampPolicy::Segment],
            supports_language_hint: true,
            supports_hotwords: true,
            confidence_kind: Some(AsrConfidenceKind::Probability),
            maximum_threads: 4,
        }
    }

    async fn diagnose(&self) -> EngineResult<AsrEnvironmentReport> {
        Ok(AsrEnvironmentReport {
            ready: true,
            platform: "test".to_owned(),
            identity: self.identity(),
            checks: vec![AsrEnvironmentCheck {
                code: "e2e".to_owned(),
                passed: true,
                message: "端到端测试环境就绪".to_owned(),
            }],
        })
    }

    async fn transcribe(
        &self,
        request: AsrRequest,
        _progress: Arc<dyn AsrProgressSink>,
        cancellation: CancellationToken,
    ) -> EngineResult<AsrResult> {
        request.validate()?;
        if cancellation.is_cancelled() {
            return Err(AsrError::cancelled());
        }
        self.calls.asr.fetch_add(1, Ordering::SeqCst);
        Ok(AsrResult {
            request_id: request.request_id,
            identity: self.identity(),
            detected_language: Some("zh".to_owned()),
            audio_duration_ms: request.audio.duration_ms,
            segments: vec![AsrSegment {
                start_ms: 100,
                end_ms: 3_900,
                text: "歡迎來到直播間,價格是￥99!".to_owned(),
                confidence: Some(0.92),
            }],
            warnings: Vec::new(),
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
            message: "端到端测试环境就绪".to_owned(),
        })
    }
}

struct QuietPublisher;

impl dy_screen_app_lib::ai::AiJobPublisher for QuietPublisher {
    fn publish(&self, _event: dy_screen_app_lib::ai::AiJobEvent) {}
}

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
        hotwords: vec!["商品名".to_owned()],
    }
}

fn setup() -> (
    Database,
    AiProjectService,
    AiRepository,
    AiInputProcessor,
    Arc<PipelineCalls>,
    tempfile::TempDir,
) {
    let directory = tempdir().unwrap();
    let database = Database::open_in_memory().unwrap();
    database.migrate().unwrap();
    let repository = AiRepository::new(database.clone());
    let calls = Arc::new(PipelineCalls::default());
    let service = AiProjectService::new(
        database.clone(),
        Arc::new(E2eInspector(calls.clone())),
        Arc::new(ReadyPreflight),
    );
    let processor = AiInputProcessor::new(
        repository.clone(),
        Arc::new(E2eInspector(calls.clone())),
        Arc::new(E2ePreparer(calls.clone())),
        Arc::new(E2eVad(calls.clone())),
        Arc::new(E2eEngine {
            calls: calls.clone(),
            identity: AsrEngineIdentity {
                engine_id: "fake-engine".to_owned(),
                engine_version: "1".to_owned(),
                model_id: "fake-model".to_owned(),
                model_version: "1".to_owned(),
            },
        }),
        TextNormalizer::with_opencc_characters(
            "zh-normalize-v1",
            include_str!("../../resources/asr/normalization/TSCharacters.txt"),
        ),
        VadConfig::default(),
        directory.path().join("asr-temporary"),
        Arc::new(QuietPublisher),
    );
    (database, service, repository, processor, calls, directory)
}

fn write_video(path: &Path) {
    std::fs::write(path, format!("fixture:{}", path.display())).unwrap();
}

async fn add_local_inputs(service: &AiProjectService, project_id: i64, paths: &[PathBuf]) {
    let grants = paths
        .iter()
        .enumerate()
        .map(|(index, path)| TrustedLocalFile::new(format!("grant-{index}"), path.clone()))
        .collect();
    let imported = service
        .import_local_files(project_id, grants, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(imported.added.len(), paths.len());
    assert!(imported.rejected.is_empty());
}

#[tokio::test]
async fn external_multi_video_pipeline_handles_partial_failure_cache_and_exports() {
    let (_database, service, repository, processor, calls, directory) = setup();
    let success = directory.path().join("成功 视频.mp4");
    let failure = directory.path().join("失败 视频.mp4");
    write_video(&success);
    write_video(&failure);
    let project = service.create_draft("外部多视频", &profile()).unwrap();
    add_local_inputs(&service, project.id, &[success.clone(), failure.clone()]).await;
    service.start_analysis(project.id).await.unwrap();
    repository
        .transition_project(project.id, AiProjectStatus::Running)
        .unwrap();
    let inputs = repository.get_project(project.id).unwrap().inputs;
    assert_eq!(
        processor
            .process_input(project.id, inputs[0].id, CancellationToken::new())
            .await
            .unwrap(),
        AiProcessOutcome::Transcribed
    );
    let error = processor
        .process_input(project.id, inputs[1].id, CancellationToken::new())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("测试音频准备失败"));

    let detail = repository.get_project(project.id).unwrap();
    assert_eq!(detail.project.status, AiProjectStatus::CompletedWithErrors);
    let projection = AiTranscriptProjection::load(&repository, project.id).unwrap();
    assert_eq!(projection.inputs[0].segments.len(), 1);
    assert_eq!(projection.inputs[1].gap_duration_ms, Some(4_000));
    let stable_id = projection.inputs[0].segments[0].stable_segment_id.clone();
    assert!(stable_id.starts_with("seg_"));
    assert!(projection.to_txt().contains("欢迎来到直播间，价格是99元！"));
    let json = projection.to_json().unwrap();
    assert!(json.contains(&stable_id));
    assert!(json.contains("fake_prepare_failed"));

    let expensive_calls = (
        calls.inspect.load(Ordering::SeqCst),
        calls.prepare.load(Ordering::SeqCst),
        calls.vad.load(Ordering::SeqCst),
        calls.asr.load(Ordering::SeqCst),
    );
    let cached_project = service.create_draft("跨项目缓存", &profile()).unwrap();
    add_local_inputs(&service, cached_project.id, &[success]).await;
    service.start_analysis(cached_project.id).await.unwrap();
    repository
        .transition_project(cached_project.id, AiProjectStatus::Running)
        .unwrap();
    let cached_input = repository.get_project(cached_project.id).unwrap().inputs[0].id;
    assert_eq!(
        processor
            .process_input(cached_project.id, cached_input, CancellationToken::new())
            .await
            .unwrap(),
        AiProcessOutcome::CacheHit
    );
    assert_eq!(
        expensive_calls,
        (
            calls.inspect.load(Ordering::SeqCst) - 1,
            calls.prepare.load(Ordering::SeqCst),
            calls.vad.load(Ordering::SeqCst),
            calls.asr.load(Ordering::SeqCst),
        ),
        "第二次导入允许执行一次媒体摘要探测，处理阶段必须完全命中缓存"
    );
    let cached_projection = AiTranscriptProjection::load(&repository, cached_project.id).unwrap();
    assert_eq!(
        cached_projection.inputs[0].segments[0].stable_segment_id,
        stable_id
    );
}

#[tokio::test]
async fn completed_live_session_expands_and_processes_an_ordered_project_timeline() {
    let (database, service, repository, processor, _calls, directory) = setup();
    let first = directory.path().join("直播分片 001.mkv");
    let second = directory.path().join("直播分片 002.mkv");
    write_video(&first);
    write_video(&second);
    let streamer = database
        .add_streamer(&NewStreamer::room("测试主播", "901", "room-901", false))
        .unwrap();
    let session = database
        .start_session(streamer.id, directory.path().to_str().unwrap())
        .unwrap();
    for (index, path) in [&first, &second].into_iter().enumerate() {
        database
            .add_video(&NewVideo {
                session_id: session.id,
                path: path.to_string_lossy().into_owned(),
                started_at: Some(format!("2026-07-22T01:00:0{}Z", index * 4)),
                ended_at: Some(format!("2026-07-22T01:00:0{}Z", index * 4 + 4)),
                duration_seconds: Some(4),
                size_bytes: std::fs::metadata(path).unwrap().len() as i64,
                audio_present: Some(true),
                status: "complete".to_owned(),
            })
            .unwrap();
    }
    database
        .finish_session(session.id, "completed", None)
        .unwrap();
    let project = service.create_draft("整场直播", &profile()).unwrap();
    let selected = service
        .select_completed_session(project.id, session.id, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(selected.len(), 2);
    assert!(
        selected
            .iter()
            .all(|input| input.source_kind == AiInputSourceKind::VideoLibrary)
    );
    service.start_analysis(project.id).await.unwrap();
    repository
        .transition_project(project.id, AiProjectStatus::Running)
        .unwrap();
    for input in repository.get_project(project.id).unwrap().inputs {
        processor
            .process_input(project.id, input.id, CancellationToken::new())
            .await
            .unwrap();
    }
    let projection = AiTranscriptProjection::load(&repository, project.id).unwrap();
    assert_eq!(projection.project.status, AiProjectStatus::Completed);
    assert_eq!(projection.inputs[0].project_offset_ms, Some(0));
    assert_eq!(projection.inputs[1].project_offset_ms, Some(4_000));
    assert_eq!(projection.inputs[0].segments[0].project_start_ms, Some(100));
    assert_eq!(
        projection.inputs[1].segments[0].project_start_ms,
        Some(4_100)
    );
    assert!(
        projection
            .inputs
            .iter()
            .all(|input| input.video_id.is_some())
    );
}

#[tokio::test]
async fn cancellation_and_restart_recovery_leave_retryable_state_and_clean_temporary_audio() {
    let (_database, service, repository, processor, _calls, directory) = setup();
    let source = directory.path().join("取消 视频.mp4");
    write_video(&source);
    let cancelled_project = service.create_draft("取消项目", &profile()).unwrap();
    add_local_inputs(
        &service,
        cancelled_project.id,
        std::slice::from_ref(&source),
    )
    .await;
    service.start_analysis(cancelled_project.id).await.unwrap();
    repository
        .transition_project(cancelled_project.id, AiProjectStatus::Running)
        .unwrap();
    let cancelled_input = repository.get_project(cancelled_project.id).unwrap().inputs[0].id;
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let error = processor
        .process_input(cancelled_project.id, cancelled_input, cancellation)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("已取消"));
    let cancelled = repository.get_project(cancelled_project.id).unwrap();
    assert_eq!(cancelled.project.status, AiProjectStatus::Cancelled);

    let interrupted_project = service.create_draft("重启恢复", &profile()).unwrap();
    add_local_inputs(&service, interrupted_project.id, &[source]).await;
    service
        .start_analysis(interrupted_project.id)
        .await
        .unwrap();
    repository
        .transition_project(interrupted_project.id, AiProjectStatus::Running)
        .unwrap();
    let interrupted_input = repository
        .get_project(interrupted_project.id)
        .unwrap()
        .inputs[0]
        .id;
    repository
        .transition_input(
            interrupted_input,
            dy_screen_app_lib::ai::AiInputStatus::Validating,
            None,
        )
        .unwrap();
    repository
        .transition_input(
            interrupted_input,
            dy_screen_app_lib::ai::AiInputStatus::PreparingAudio,
            None,
        )
        .unwrap();
    let temporary_root = directory.path().join("recovery-temporary");
    let nested = temporary_root.join("project").join("input");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::write(nested.join("orphan.wav.part"), b"partial").unwrap();
    let report = AiLifecycle::new(repository.clone(), temporary_root)
        .recover_startup()
        .unwrap();
    assert_eq!(report.database.projects, 1);
    assert_eq!(report.database.inputs, 1);
    assert_eq!(report.removed_temporary_audio_files, 1);
    let recovered = repository.get_project(interrupted_project.id).unwrap();
    assert_eq!(recovered.project.status, AiProjectStatus::Queued);
    assert_eq!(
        recovered.inputs[0].status,
        dy_screen_app_lib::ai::AiInputStatus::Pending
    );
}
