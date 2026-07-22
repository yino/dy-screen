#![cfg(target_os = "windows")]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use dy_screen::asr::{
    AsrEngine, AsrProgressEvent, AsrProgressSink, AsrRequest, AsrResourceResolver, PreparedAudio,
    PreparedAudioFormat, ResourcePreflight, SpeechRegion, SystemResourceProbe, TimestampPolicy,
    VadConfig, WhisperCppEngine,
};
use sha2::{Digest, Sha256};
use tempfile::tempdir;
use tokio_util::sync::CancellationToken;

const FAKE_SIDECAR_SOURCE: &str = r##"
use std::path::PathBuf;
use std::time::Duration;

fn main() {
    let arguments = std::env::args_os().skip(1).collect::<Vec<_>>();
    if arguments.len() == 1 && arguments[0] == "--version" {
        println!("whisper.cpp version: 1.9.1");
        return;
    }
    let executable = std::env::current_exe().unwrap();
    let directory = executable.parent().unwrap();
    if directory.join("mode-runtime-missing").is_file() {
        unsafe extern "system" { fn ExitProcess(exit_code: u32) -> !; }
        unsafe { ExitProcess(0xC0000135) }
    }
    if directory.join("mode-sleep").is_file() {
        std::thread::sleep(Duration::from_secs(30));
        return;
    }
    if directory.join("mode-fail").is_file() { std::process::exit(9); }
    if !arguments.iter().any(|value| value == "--no-gpu") { std::process::exit(12); }
    std::fs::write(
        directory.join("arguments.txt"),
        arguments.iter().map(|value| value.to_string_lossy()).collect::<Vec<_>>().join("\n"),
    ).unwrap();
    let output_index = arguments.iter().position(|value| value == "--output-file").unwrap();
    let output_prefix = PathBuf::from(&arguments[output_index + 1]);
    std::fs::write(output_prefix.with_extension("json"), r#"{
      "result":{"language":"zh"},
      "transcription":[{
        "offsets":{"from":0,"to":1000},
        "text":" Windows 中文路径识别成功 ",
        "tokens":[{"text":"中文","p":0.91}]
      }]
    }"#).unwrap();
}
"##;

struct FixedProbe;

impl SystemResourceProbe for FixedProbe {
    fn total_memory_bytes(&self) -> Option<u64> {
        Some(16 * 1024 * 1024 * 1024)
    }
    fn available_memory_bytes(&self) -> Option<u64> {
        Some(8 * 1024 * 1024 * 1024)
    }
    fn available_disk_bytes(&self, _path: &Path) -> Option<u64> {
        Some(8 * 1024 * 1024 * 1024)
    }
}

struct QuietProgress;

impl AsrProgressSink for QuietProgress {
    fn publish(&self, _event: AsrProgressEvent) {}
}

struct Fixture {
    _directory: tempfile::TempDir,
    engine: WhisperCppEngine,
    audio: PreparedAudio,
    bin: PathBuf,
    resources: dy_screen::asr::ResolvedAsrResources,
}

fn compile_fake_sidecar(output: &Path) {
    let source = output.with_extension("rs");
    std::fs::write(&source, FAKE_SIDECAR_SOURCE).unwrap();
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let status = Command::new(rustc)
        .args(["--edition=2024", "-O"])
        .arg(&source)
        .arg("-o")
        .arg(output)
        .status()
        .unwrap();
    assert!(status.success(), "无法编译 Windows ASR 测试 sidecar");
}

fn fixture() -> Fixture {
    let directory = tempdir().unwrap();
    let root = directory.path().join("资源 空格");
    let bin = root.join("bin/windows-x86_64");
    std::fs::create_dir_all(&bin).unwrap();
    let sidecar = bin.join("whisper-cli.exe");
    compile_fake_sidecar(&sidecar);
    for name in ["vad-speech-segments.exe", "ffmpeg.exe", "ffprobe.exe"] {
        std::fs::copy(&sidecar, bin.join(name)).unwrap();
    }
    let model = b"model";
    let vad = b"vad";
    let normalization = "歡 欢\n".as_bytes();
    std::fs::create_dir_all(root.join("models")).unwrap();
    std::fs::create_dir_all(root.join("normalization")).unwrap();
    std::fs::create_dir_all(root.join("runtime/windows-x86_64")).unwrap();
    std::fs::write(root.join("models/model.bin"), model).unwrap();
    std::fs::write(root.join("models/vad.bin"), vad).unwrap();
    std::fs::write(root.join("normalization/map.txt"), normalization).unwrap();
    std::fs::write(
        root.join("runtime/windows-x86_64/vc_redist.x64.exe"),
        b"runtime fixture",
    )
    .unwrap();
    let integrity = |file: &str| {
        let bytes = std::fs::read(root.join(file)).unwrap();
        serde_json::json!({
            "file": file,
            "sizeBytes": bytes.len(),
            "sha256": hex::encode(Sha256::digest(bytes))
        })
    };
    let manifest = serde_json::json!({
        "schemaVersion": 1,
        "bundleVersion": "test",
        "engine": {"id":"whisper.cpp","version":"v1.9.1","sourceCommit":"test"},
        "model": {
            "logicalId":"small","version":"1","file":"models/model.bin",
            "sizeBytes":model.len(),"sha256":hex::encode(Sha256::digest(model)),
            "source":"test","license":"test"
        },
        "vad": {
            "logicalId":"vad","version":"1","file":"models/vad.bin",
            "sizeBytes":vad.len(),"sha256":hex::encode(Sha256::digest(vad)),
            "source":"test","license":"test"
        },
        "normalization": {
            "logicalId":"map","version":"1","file":"normalization/map.txt",
            "sizeBytes":normalization.len(),"sha256":hex::encode(Sha256::digest(normalization)),
            "source":"test","license":"test"
        },
        "platforms":[{
            "os":"windows","arch":"x86_64","accelerator":"cpu",
            "sidecar":"bin/windows-x86_64/whisper-cli.exe",
            "vadSidecar":"bin/windows-x86_64/vad-speech-segments.exe",
            "ffmpeg":"bin/windows-x86_64/ffmpeg.exe",
            "ffprobe":"bin/windows-x86_64/ffprobe.exe",
            "minimumMemoryBytes":8589934592_u64,
            "minimumFreeDiskBytes":2147483648_u64,
            "maximumThreads":4,
            "minimumCpuFeatures":["sse4.2"],
            "runtime":"Microsoft Visual C++ 2015-2022 Redistributable x64",
            "runtimeFile":"runtime/windows-x86_64/vc_redist.x64.exe",
            "resourceIntegrity":[
                integrity("bin/windows-x86_64/whisper-cli.exe"),
                integrity("bin/windows-x86_64/vad-speech-segments.exe"),
                integrity("bin/windows-x86_64/ffmpeg.exe"),
                integrity("bin/windows-x86_64/ffprobe.exe"),
                integrity("runtime/windows-x86_64/vc_redist.x64.exe")
            ]
        }]
    })
    .to_string();
    let resources = AsrResourceResolver::resolve(&root, &manifest).unwrap();
    assert!(!resources.use_gpu);
    let audio_path = root.join("输入 视频.wav");
    std::fs::write(&audio_path, b"fake wav").unwrap();
    let audio = PreparedAudio {
        path: audio_path,
        format: PreparedAudioFormat::PcmS16LeWav,
        sample_rate_hz: 16_000,
        channels: 1,
        duration_ms: 1_000,
    };
    let engine = WhisperCppEngine::new(
        resources.clone(),
        ResourcePreflight::new(Arc::new(FixedProbe)),
        VadConfig::default(),
    );
    Fixture {
        _directory: directory,
        engine,
        audio,
        bin,
        resources,
    }
}

fn request(audio: PreparedAudio) -> AsrRequest {
    AsrRequest {
        request_id: "windows-adapter".to_owned(),
        audio,
        language_hint: Some("zh".to_owned()),
        hotwords: vec!["中文 热词".to_owned()],
        speech_regions: vec![SpeechRegion {
            start_ms: 0,
            end_ms: 1_000,
        }],
        timestamp_policy: TimestampPolicy::Segment,
        max_threads: 4,
    }
}

#[tokio::test]
async fn cpu_adapter_supports_unicode_and_space_paths_with_argument_arrays() {
    let fixture = fixture();
    assert!(fixture.engine.diagnose().await.unwrap().ready);
    let result = fixture
        .engine
        .transcribe(
            request(fixture.audio.clone()),
            Arc::new(QuietProgress),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(result.segments[0].text, "Windows 中文路径识别成功");
    let arguments = std::fs::read_to_string(fixture.bin.join("arguments.txt")).unwrap();
    assert!(arguments.contains("--no-gpu"));
    assert!(arguments.contains("输入 视频.wav"));
    assert!(!arguments.contains("cmd.exe"));
}

#[tokio::test]
async fn exit_cancel_and_missing_runtime_have_stable_safe_errors() {
    let fixture = fixture();
    std::fs::write(fixture.bin.join("mode-fail"), b"fail").unwrap();
    let error = fixture
        .engine
        .transcribe(
            request(fixture.audio.clone()),
            Arc::new(QuietProgress),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, "whisper_failed");
    std::fs::remove_file(fixture.bin.join("mode-fail")).unwrap();

    std::fs::write(fixture.bin.join("mode-sleep"), b"sleep").unwrap();
    let cancellation = CancellationToken::new();
    let cancel_after_launch = cancellation.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        cancel_after_launch.cancel();
    });
    let started = Instant::now();
    let error = fixture
        .engine
        .transcribe(
            request(fixture.audio.clone()),
            Arc::new(QuietProgress),
            cancellation,
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, "asr_cancelled");
    assert!(started.elapsed() < Duration::from_secs(5));
    std::fs::remove_file(fixture.bin.join("mode-sleep")).unwrap();

    std::fs::write(fixture.bin.join("mode-runtime-missing"), b"missing").unwrap();
    let error = fixture
        .engine
        .transcribe(
            request(fixture.audio),
            Arc::new(QuietProgress),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, "whisper_runtime_missing");
    assert!(error.safe_message.contains("Visual C++"));
    assert!(
        !error
            .safe_message
            .contains(fixture.resources.root.to_string_lossy().as_ref())
    );
}
