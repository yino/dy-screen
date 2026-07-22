#![cfg(unix)]

use std::path::Path;
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

fn write_executable(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;

    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
    let mut permissions = std::fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(path, permissions).unwrap();
}

fn engine_fixture() -> (tempfile::TempDir, WhisperCppEngine, PreparedAudio) {
    let directory = tempdir().unwrap();
    let root = directory.path().join("资源 空格");
    let bin = root.join("bin");
    let model = b"model";
    let vad = b"vad";
    let normalization = "歡 欢\n".as_bytes();
    let sidecar = bin.join("whisper cli");
    let marker = bin.join("mode-sleep");
    write_executable(
        &sidecar,
        &format!(
            "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then printf '%s\\n' 'whisper.cpp version: 1.9.1'; exit 0; fi\nif [ -f '{}' ]; then exec sleep 30; fi\nexit 9\n",
            marker.display()
        ),
    );
    for name in ["vad sidecar", "ffmpeg", "ffprobe"] {
        write_executable(&bin.join(name), "#!/bin/sh\nexit 0\n");
    }
    std::fs::create_dir_all(root.join("models")).unwrap();
    std::fs::create_dir_all(root.join("normalization")).unwrap();
    std::fs::write(root.join("models/model.bin"), model).unwrap();
    std::fs::write(root.join("models/vad.bin"), vad).unwrap();
    std::fs::write(root.join("normalization/map.txt"), normalization).unwrap();
    let integrity = |file: &str| {
        let bytes = std::fs::read(root.join(file)).unwrap();
        serde_json::json!({
            "file": file,
            "sizeBytes": bytes.len(),
            "sha256": hex::encode(Sha256::digest(bytes))
        })
    };
    let os = if cfg!(target_os = "macos") {
        "macos"
    } else {
        std::env::consts::OS
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
            "os":os,"arch":std::env::consts::ARCH,"accelerator":"cpu",
            "sidecar":"bin/whisper cli","vadSidecar":"bin/vad sidecar",
            "ffmpeg":"bin/ffmpeg","ffprobe":"bin/ffprobe",
            "minimumMemoryBytes":8589934592_u64,"minimumFreeDiskBytes":2147483648_u64,
            "maximumThreads":4,
            "resourceIntegrity":[
                integrity("bin/whisper cli"),
                integrity("bin/vad sidecar"),
                integrity("bin/ffmpeg"),
                integrity("bin/ffprobe")
            ]
        }]
    })
    .to_string();
    let resources = AsrResourceResolver::resolve(&root, &manifest).unwrap();
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
        resources,
        ResourcePreflight::new(Arc::new(FixedProbe)),
        VadConfig::default(),
    );
    (directory, engine, audio)
}

fn request(audio: PreparedAudio) -> AsrRequest {
    AsrRequest {
        request_id: "adapter-test".to_owned(),
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
async fn sidecar_failure_is_classified_without_leaking_paths_or_stderr() {
    let (_directory, engine, audio) = engine_fixture();
    let error = engine
        .transcribe(
            request(audio),
            Arc::new(QuietProgress),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, "whisper_failed");
    assert!(!error.safe_message.contains("资源 空格"));
    assert!(!error.safe_message.contains("输入 视频"));
}

#[tokio::test]
async fn running_sidecar_is_killed_within_bounded_time_on_cancellation() {
    let (directory, engine, audio) = engine_fixture();
    std::fs::write(directory.path().join("资源 空格/bin/mode-sleep"), b"sleep").unwrap();
    let cancellation = CancellationToken::new();
    let cancel_after_launch = cancellation.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        cancel_after_launch.cancel();
    });
    let started = Instant::now();
    let error = engine
        .transcribe(request(audio), Arc::new(QuietProgress), cancellation)
        .await
        .unwrap_err();
    assert_eq!(error.code, "asr_cancelled");
    assert!(started.elapsed() < Duration::from_secs(5));
}
