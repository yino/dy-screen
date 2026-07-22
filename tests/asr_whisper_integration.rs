use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

#[cfg(target_os = "windows")]
use dy_screen::asr::AsrBundleManifest;
use dy_screen::asr::{
    AsrEngine, AsrProgressEvent, AsrProgressSink, AsrProgressStage, AsrRequest,
    AsrResourceResolver, AudioPreparationRequest, FfmpegAudioPreparer, FrozenMediaSource,
    MediaAudioPreparer, PreparedAudioFormat, ResourcePreflight, SystemResourceProbe,
    TextNormalizer, TimestampPolicy, VadConfig, VadEngine, WhisperCppEngine, WhisperCppVadEngine,
};
use tempfile::tempdir;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

struct HighResourceProbe;

impl SystemResourceProbe for HighResourceProbe {
    fn total_memory_bytes(&self) -> Option<u64> {
        Some(16 * 1024 * 1024 * 1024)
    }

    fn available_memory_bytes(&self) -> Option<u64> {
        Some(8 * 1024 * 1024 * 1024)
    }

    fn available_disk_bytes(&self, _path: &Path) -> Option<u64> {
        Some(20 * 1024 * 1024 * 1024)
    }
}

#[derive(Default)]
struct ProgressCollector(Mutex<Vec<AsrProgressEvent>>);

impl AsrProgressSink for ProgressCollector {
    fn publish(&self, event: AsrProgressEvent) {
        self.0.lock().unwrap().push(event);
    }
}

struct TranscribingSignal(Arc<Notify>);

impl AsrProgressSink for TranscribingSignal {
    fn publish(&self, event: AsrProgressEvent) {
        if event.stage == AsrProgressStage::Transcribing {
            self.0.notify_one();
        }
    }
}

fn resource_root() -> PathBuf {
    std::env::var_os("ASR_RESOURCE_ROOT")
        .map(PathBuf::from)
        .expect("ignored integration test requires ASR_RESOURCE_ROOT")
}

#[tokio::test]
#[ignore = "需要当前目标平台锁定的 whisper.cpp、模型、VAD 与 FFmpeg 资源"]
async fn real_whisper_engine_transcribes_fixture_and_exits_cleanly() {
    let root = resource_root();
    let manifest = std::fs::read_to_string(root.join("manifest.json")).unwrap();
    let resources = AsrResourceResolver::resolve(&root, &manifest).unwrap();
    let preflight = ResourcePreflight::new(Arc::new(HighResourceProbe));
    let engine = WhisperCppEngine::new(resources.clone(), preflight, VadConfig::default());
    let report = engine.diagnose().await.unwrap();
    assert!(report.ready, "{:?}", report.checks);

    #[cfg(target_os = "windows")]
    {
        let manifest = AsrBundleManifest::from_json(&manifest).unwrap();
        let windows = manifest.platform("windows", "x86_64").unwrap();
        assert_eq!(windows.accelerator, "cpu");
        assert!(
            windows
                .minimum_cpu_features
                .iter()
                .any(|value| value == "sse4.2")
        );
        assert!(windows.runtime.is_some());
        assert!(windows.runtime_file.is_some());
    }

    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/asr/short_zh.mp4");
    let temporary = tempdir().unwrap();
    let unicode_directory = temporary.path().join("Windows 中文路径");
    std::fs::create_dir_all(&unicode_directory).unwrap();
    let unicode_fixture = unicode_directory.join("测试 视频.mp4");
    std::fs::copy(&fixture, &unicode_fixture).unwrap();
    let request = AudioPreparationRequest {
        task_id: "real-whisper".to_owned(),
        source: FrozenMediaSource::from_path(&unicode_fixture).unwrap(),
        duration_ms: 4_191,
        temporary_root: temporary.path().to_path_buf(),
    };
    let audio = FfmpegAudioPreparer::new(resources.ffmpeg.clone())
        .prepare_temporary_wav(&request, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(audio.audio().format, PreparedAudioFormat::PcmS16LeWav);
    let regions = WhisperCppVadEngine::new(
        resources.vad_sidecar.clone(),
        resources.vad_model.clone(),
        4,
        false,
    )
    .detect(
        audio.audio(),
        &VadConfig::default(),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert!(!regions.is_empty());

    let progress = Arc::new(ProgressCollector::default());
    let result = engine
        .transcribe(
            AsrRequest {
                request_id: "real-whisper-short-zh".to_owned(),
                audio: audio.audio().clone(),
                language_hint: Some("zh".to_owned()),
                hotwords: vec!["直播间".to_owned(), "九十九元".to_owned()],
                speech_regions: regions,
                timestamp_policy: TimestampPolicy::Segment,
                max_threads: 4,
            },
            progress.clone(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let raw = result
        .segments
        .iter()
        .map(|segment| segment.text.as_str())
        .collect::<String>();
    assert!(raw.contains("直播间"), "{raw}");
    assert!(raw.contains("99") || raw.contains("九十九"), "{raw}");
    assert_eq!(result.detected_language.as_deref(), Some("zh"));
    assert!(
        result
            .segments
            .iter()
            .all(|segment| segment.end_ms <= 4_191)
    );
    let normalized = TextNormalizer::new("zh-normalize-v1").normalize(&raw);
    assert!(normalized.contains('，') || !raw.contains(','));
    let stages = progress.0.lock().unwrap();
    assert!(stages.len() >= 4);
    assert_eq!(stages.last().unwrap().completed_units, 100);

    assert!(
        std::fs::read_dir(temporary.path())
            .unwrap()
            .flatten()
            .all(|entry| entry.path().extension().and_then(|value| value.to_str()) != Some("json")),
        "结构化临时输出必须在解析后清理"
    );
}

#[tokio::test]
#[ignore = "需要当前目标平台锁定的真实 whisper.cpp 资源"]
async fn real_whisper_running_process_is_cancelled_and_leaves_no_json() {
    let root = resource_root();
    let manifest = std::fs::read_to_string(root.join("manifest.json")).unwrap();
    let resources = AsrResourceResolver::resolve(&root, &manifest).unwrap();
    let engine = WhisperCppEngine::new(
        resources.clone(),
        ResourcePreflight::new(Arc::new(HighResourceProbe)),
        VadConfig::default(),
    );
    assert!(engine.diagnose().await.unwrap().ready);

    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/asr/short_zh.mp4");
    let temporary = tempdir().unwrap();
    let request = AudioPreparationRequest {
        task_id: "real-whisper-cancel".to_owned(),
        source: FrozenMediaSource::from_path(&fixture).unwrap(),
        duration_ms: 4_191,
        temporary_root: temporary.path().to_path_buf(),
    };
    let audio = FfmpegAudioPreparer::new(resources.ffmpeg.clone())
        .prepare_temporary_wav(&request, CancellationToken::new())
        .await
        .unwrap();
    let cancellation = CancellationToken::new();
    let cancel_after_launch = cancellation.clone();
    let transcribing = Arc::new(Notify::new());
    let signal = transcribing.clone();
    tokio::spawn(async move {
        signal.notified().await;
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        cancel_after_launch.cancel();
    });
    let started = std::time::Instant::now();
    let error = engine
        .transcribe(
            AsrRequest {
                request_id: "real-whisper-running-cancel".to_owned(),
                audio: audio.audio().clone(),
                language_hint: Some("zh".to_owned()),
                hotwords: Vec::new(),
                speech_regions: vec![dy_screen::asr::SpeechRegion {
                    start_ms: 0,
                    end_ms: 4_191,
                }],
                timestamp_policy: TimestampPolicy::Segment,
                max_threads: 4,
            },
            Arc::new(TranscribingSignal(transcribing)),
            cancellation,
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, "asr_cancelled");
    assert!(started.elapsed() < std::time::Duration::from_secs(10));
    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    assert!(
        std::fs::read_dir(temporary.path())
            .unwrap()
            .flatten()
            .all(|entry| entry.path().extension().and_then(|value| value.to_str()) != Some("json")),
        "取消后的结构化临时输出必须被清理"
    );
}

#[tokio::test]
#[ignore = "需要锁定的 whisper.cpp 资源"]
async fn engine_rejects_cancelled_and_no_speech_requests_before_launching_sidecar() {
    let root = resource_root();
    let manifest = std::fs::read_to_string(root.join("manifest.json")).unwrap();
    let resources = AsrResourceResolver::resolve(&root, &manifest).unwrap();
    let engine = WhisperCppEngine::new(
        resources,
        ResourcePreflight::new(Arc::new(HighResourceProbe)),
        VadConfig::default(),
    );
    let temporary = tempdir().unwrap();
    let audio_path = temporary.path().join("empty.wav");
    std::fs::write(&audio_path, b"not read because validation stops first").unwrap();
    let request = AsrRequest {
        request_id: "precondition".to_owned(),
        audio: dy_screen::asr::PreparedAudio {
            path: audio_path,
            format: PreparedAudioFormat::PcmS16LeWav,
            sample_rate_hz: 16_000,
            channels: 1,
            duration_ms: 1_000,
        },
        language_hint: Some("zh".to_owned()),
        hotwords: Vec::new(),
        speech_regions: Vec::new(),
        timestamp_policy: TimestampPolicy::Segment,
        max_threads: 1,
    };
    let progress = Arc::new(ProgressCollector::default());
    let error = engine
        .transcribe(request.clone(), progress.clone(), CancellationToken::new())
        .await
        .unwrap_err();
    assert_eq!(error.code, "no_speech_detected");

    let cancelled = CancellationToken::new();
    cancelled.cancel();
    let error = engine
        .transcribe(request, progress, cancelled)
        .await
        .unwrap_err();
    assert_eq!(error.code, "asr_cancelled");
}
