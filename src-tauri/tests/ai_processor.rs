use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use dy_screen::asr::{
    AsrCapabilities, AsrConfidenceKind, AsrEngine, AsrEngineIdentity, AsrEnvironmentCheck,
    AsrEnvironmentReport, AsrError, AsrProgressEvent, AsrProgressSink, AsrRequest, AsrResult,
    AsrSegment, AudioPreparationCapabilities, AudioPreparationRequest, EngineResult,
    FrozenMediaSource, MediaAudioPreparer, MediaInspection, MediaInspector, PcmPipeProcess,
    PreparedAudio, PreparedAudioFormat, PreparedAudioLease, SpeechRegion, TextNormalizer,
    TimestampPolicy, TranscriptAssembler, TranscriptInput, TranscriptInputSource,
    TranscriptSourceSegment, VadConfig, VadEngine,
};
use dy_screen_app_lib::ai::{
    AiInputProcessor, AiInputSourceKind, AiJobEvent, AiJobPublisher, AiProcessOutcome,
    AiProjectStatus, AiRepository, NewAiProjectInput, NewAsrArtifact, RecognitionProfile,
    SourceFingerprint,
};
use dy_screen_app_lib::database::Database;
use tempfile::tempdir;
use tokio_util::sync::CancellationToken;

#[derive(Default)]
struct CallCounters {
    inspect: AtomicUsize,
    prepare: AtomicUsize,
    vad: AtomicUsize,
    asr: AtomicUsize,
}

struct FakeInspector(Arc<CallCounters>);

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

struct FakePreparer(Arc<CallCounters>);

#[async_trait]
impl MediaAudioPreparer for FakePreparer {
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
        std::fs::create_dir_all(&request.temporary_root).unwrap();
        let path = request.temporary_root.join("fake.wav");
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
            "fake_pipe_unavailable",
            "测试替身不提供 PCM 管道",
        ))
    }
}

struct FakeVad(Arc<CallCounters>);

#[async_trait]
impl VadEngine for FakeVad {
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

struct FakeEngine {
    counters: Arc<CallCounters>,
    identity: AsrEngineIdentity,
}

#[async_trait]
impl AsrEngine for FakeEngine {
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
                code: "fake".to_owned(),
                passed: true,
                message: "测试环境就绪".to_owned(),
            }],
        })
    }

    async fn transcribe(
        &self,
        request: AsrRequest,
        progress: Arc<dyn AsrProgressSink>,
        cancellation: CancellationToken,
    ) -> EngineResult<AsrResult> {
        request.validate()?;
        if cancellation.is_cancelled() {
            return Err(AsrError::cancelled());
        }
        self.counters.asr.fetch_add(1, Ordering::SeqCst);
        progress.publish(AsrProgressEvent {
            request_id: request.request_id.clone(),
            stage: dy_screen::asr::AsrProgressStage::Transcribing,
            completed_units: 80,
            total_units: Some(100),
            message: "假引擎识别中".to_owned(),
        });
        Ok(AsrResult {
            request_id: request.request_id,
            identity: self.identity(),
            detected_language: Some("zh".to_owned()),
            audio_duration_ms: request.audio.duration_ms,
            segments: vec![AsrSegment {
                start_ms: 100,
                end_ms: 3_900,
                text: "歡迎來到直播間,價格是￥99!".to_owned(),
                confidence: Some(0.9),
            }],
            warnings: Vec::new(),
        })
    }
}

#[derive(Default)]
struct EventCollector(Mutex<Vec<AiJobEvent>>);

impl AiJobPublisher for EventCollector {
    fn publish(&self, event: AiJobEvent) {
        self.0.lock().unwrap().push(event);
    }
}

fn profile(model_version: &str, hotwords: &[&str], vad_padding_ms: u64) -> RecognitionProfile {
    RecognitionProfile {
        engine_id: "fake-engine".to_owned(),
        engine_version: "1".to_owned(),
        model_id: "fake-model".to_owned(),
        model_version: model_version.to_owned(),
        language_hint: Some("zh".to_owned()),
        vad_model_id: "fake-vad".to_owned(),
        vad_threshold_millis: 500,
        vad_padding_ms,
        timestamp_policy: "segment".to_owned(),
        normalization_version: "zh-normalize-v1".to_owned(),
        hotwords: hotwords.iter().map(|word| (*word).to_owned()).collect(),
    }
}

fn fingerprint(path: &Path, video_id: Option<i64>) -> SourceFingerprint {
    let frozen = FrozenMediaSource::from_path(path).unwrap();
    SourceFingerprint {
        normalized_path: frozen.path.to_string_lossy().into_owned(),
        size_bytes: frozen.size_bytes,
        modified_at_ms: i64::try_from(frozen.modified_at_ms).unwrap(),
        video_id,
    }
}

fn create_running_input(
    repository: &AiRepository,
    path: &Path,
    profile: &RecognitionProfile,
) -> (i64, i64, SourceFingerprint) {
    let project = repository.create_project("处理测试", profile).unwrap();
    let source = fingerprint(path, None);
    let input = repository
        .add_input(
            project.id,
            NewAiProjectInput {
                position: 0,
                source_kind: AiInputSourceKind::LocalFile,
                video_id: None,
                display_name: path.file_name().unwrap().to_string_lossy().into_owned(),
                source_path: source.normalized_path.clone(),
                source_fingerprint: source.clone(),
                duration_ms: Some(4_000),
                audio_present: Some(true),
            },
        )
        .unwrap();
    repository.freeze_project(project.id).unwrap();
    repository
        .transition_project(project.id, AiProjectStatus::Running)
        .unwrap();
    (project.id, input.id, source)
}

fn processor(
    repository: AiRepository,
    counters: Arc<CallCounters>,
    identity: AsrEngineIdentity,
    temporary_root: PathBuf,
    vad_padding_ms: u64,
) -> (AiInputProcessor, Arc<EventCollector>) {
    let events = Arc::new(EventCollector::default());
    let processor = AiInputProcessor::new(
        repository,
        Arc::new(FakeInspector(counters.clone())),
        Arc::new(FakePreparer(counters.clone())),
        Arc::new(FakeVad(counters.clone())),
        Arc::new(FakeEngine { counters, identity }),
        TextNormalizer::with_opencc_characters(
            "zh-normalize-v1",
            include_str!("../../resources/asr/normalization/TSCharacters.txt"),
        ),
        VadConfig {
            speech_padding_ms: vad_padding_ms,
            ..VadConfig::default()
        },
        temporary_root,
        events.clone(),
    );
    (processor, events)
}

#[tokio::test]
async fn cache_hit_reuses_stable_segments_and_skips_all_expensive_interfaces() {
    let directory = tempdir().unwrap();
    let source_path = directory.path().join("source.mp4");
    std::fs::write(&source_path, b"same source").unwrap();
    let database = Database::open_in_memory().unwrap();
    database.migrate().unwrap();
    let repository = AiRepository::new(database);
    let profile = profile("model-v1", &["商品名"], 500);
    let identity = AsrEngineIdentity {
        engine_id: profile.engine_id.clone(),
        engine_version: profile.engine_version.clone(),
        model_id: profile.model_id.clone(),
        model_version: profile.model_version.clone(),
    };
    let counters = Arc::new(CallCounters::default());
    let (processor, events) = processor(
        repository.clone(),
        counters.clone(),
        identity,
        directory.path().join("temporary"),
        500,
    );
    let (first_project, first_input, _) = create_running_input(&repository, &source_path, &profile);
    assert_eq!(
        processor
            .process_input(first_project, first_input, CancellationToken::new())
            .await
            .unwrap(),
        AiProcessOutcome::Transcribed
    );
    assert_eq!(counters.inspect.load(Ordering::SeqCst), 1);
    assert_eq!(counters.prepare.load(Ordering::SeqCst), 1);
    assert_eq!(counters.vad.load(Ordering::SeqCst), 1);
    assert_eq!(counters.asr.load(Ordering::SeqCst), 1);
    let original_segments = repository.list_segments_for_input(first_input).unwrap();
    assert_eq!(
        original_segments[0].normalized_text,
        "欢迎来到直播间，价格是99元！"
    );

    let (second_project, second_input, _) =
        create_running_input(&repository, &source_path, &profile);
    assert_eq!(
        processor
            .process_input(second_project, second_input, CancellationToken::new())
            .await
            .unwrap(),
        AiProcessOutcome::CacheHit
    );
    assert_eq!(counters.inspect.load(Ordering::SeqCst), 1);
    assert_eq!(counters.prepare.load(Ordering::SeqCst), 1);
    assert_eq!(counters.vad.load(Ordering::SeqCst), 1);
    assert_eq!(counters.asr.load(Ordering::SeqCst), 1);
    let reused_segments = repository.list_segments_for_input(second_input).unwrap();
    assert_eq!(reused_segments, original_segments);
    let snapshots = events.0.lock().unwrap();
    assert!(snapshots.iter().any(|event| {
        event.project_id == first_project
            && event.input_id == first_input
            && event.stage == "asr_transcribing"
            && event.input_status == dy_screen_app_lib::ai::AiInputStatus::Transcribing
            && event.input_progress_percent == 95
    }));
    assert!(snapshots.iter().any(|event| {
        event.project_id == second_project
            && event.input_id == second_input
            && event.project_status == AiProjectStatus::Completed
            && event.project_progress_percent == 100
            && event.input_progress_percent == 100
    }));
    drop(snapshots);
    let restored = repository.get_project(second_project).unwrap();
    assert_eq!(restored.project.progress_percent, 100);
    assert_eq!(restored.inputs[0].progress_percent, 100);

    let assembled = TranscriptAssembler::default().assemble(&[TranscriptInput {
        input_id: second_input,
        video_id: None,
        source: TranscriptInputSource::External,
        duration_ms: 4_000,
        completed: true,
        segments: reused_segments
            .iter()
            .map(|segment| TranscriptSourceSegment {
                id: segment.id.clone(),
                source_start_ms: segment.source_start_ms,
                source_end_ms: segment.source_end_ms,
                raw_text: segment.raw_text.clone(),
                normalized_text: segment.normalized_text.clone(),
                confidence: segment.confidence,
            })
            .collect(),
    }]);
    assert_eq!(assembled.segments[0].id, original_segments[0].id);
}

#[tokio::test]
async fn source_model_vad_hotword_changes_and_pending_artifacts_are_cache_misses() {
    let directory = tempdir().unwrap();
    let database = Database::open_in_memory().unwrap();
    database.migrate().unwrap();
    let repository = AiRepository::new(database);
    let counters = Arc::new(CallCounters::default());

    let cases = [
        (
            "source-a.mp4",
            "engine-v1",
            "model-v1",
            "vad-v1",
            vec!["商品名"],
            500_u64,
        ),
        (
            "source-b.mp4",
            "engine-v1",
            "model-v1",
            "vad-v1",
            vec!["商品名"],
            500_u64,
        ),
        (
            "source-a.mp4",
            "engine-v2",
            "model-v1",
            "vad-v1",
            vec!["商品名"],
            500_u64,
        ),
        (
            "source-a.mp4",
            "engine-v1",
            "model-v2",
            "vad-v1",
            vec!["商品名"],
            500_u64,
        ),
        (
            "source-a.mp4",
            "engine-v1",
            "model-v1",
            "vad-v2",
            vec!["商品名"],
            500_u64,
        ),
        (
            "source-a.mp4",
            "engine-v1",
            "model-v1",
            "vad-v1",
            vec!["另一个热词"],
            500_u64,
        ),
        (
            "source-a.mp4",
            "engine-v1",
            "model-v1",
            "vad-v1",
            vec!["商品名"],
            800_u64,
        ),
    ];
    std::fs::write(directory.path().join("source-a.mp4"), b"source a").unwrap();
    std::fs::write(directory.path().join("source-b.mp4"), b"source b changed").unwrap();

    for (index, (file, engine, model, vad_model, hotwords, padding)) in cases.iter().enumerate() {
        let mut profile = profile(model, hotwords, *padding);
        profile.engine_version = (*engine).to_owned();
        profile.vad_model_id = (*vad_model).to_owned();
        let identity = AsrEngineIdentity {
            engine_id: profile.engine_id.clone(),
            engine_version: profile.engine_version.clone(),
            model_id: profile.model_id.clone(),
            model_version: profile.model_version.clone(),
        };
        let source_path = directory.path().join(file);
        let (project_id, input_id, source) =
            create_running_input(&repository, &source_path, &profile);
        if index == 0 {
            repository
                .create_artifact(NewAsrArtifact {
                    source_fingerprint: source,
                    recognition_profile_hash: profile.fingerprint().unwrap(),
                    engine_id: profile.engine_id.clone(),
                    engine_version: profile.engine_version.clone(),
                    model_id: profile.model_id.clone(),
                    model_version: profile.model_version.clone(),
                })
                .unwrap();
        }
        let (processor, _events) = processor(
            repository.clone(),
            counters.clone(),
            identity,
            directory.path().join(format!("temporary-{index}")),
            *padding,
        );
        assert_eq!(
            processor
                .process_input(project_id, input_id, CancellationToken::new())
                .await
                .unwrap(),
            AiProcessOutcome::Transcribed
        );
    }
    assert_eq!(counters.inspect.load(Ordering::SeqCst), cases.len());
    assert_eq!(counters.prepare.load(Ordering::SeqCst), cases.len());
    assert_eq!(counters.vad.load(Ordering::SeqCst), cases.len());
    assert_eq!(counters.asr.load(Ordering::SeqCst), cases.len());
}
