use std::path::PathBuf;

use dy_screen::asr::{
    AsrResourceResolver, AudioPreparationRequest, FfmpegAudioPreparer, FrozenMediaSource,
    MediaAudioPreparer, ResolvedAsrResources, VadConfig, VadEngine, WhisperCppVadEngine,
};
use tempfile::tempdir;
use tokio_util::sync::CancellationToken;

fn resources() -> ResolvedAsrResources {
    let root = std::env::var_os("ASR_RESOURCE_ROOT")
        .map(PathBuf::from)
        .expect("ignored integration test requires ASR_RESOURCE_ROOT");
    let manifest = std::fs::read_to_string(root.join("manifest.json")).unwrap();
    AsrResourceResolver::resolve(&root, &manifest).unwrap()
}

async fn detect(
    resources: &ResolvedAsrResources,
    fixture: &str,
) -> Vec<dy_screen::asr::SpeechRegion> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let source_path = root.join("tests/fixtures/asr").join(fixture);
    let source = FrozenMediaSource::from_path(&source_path).unwrap();
    let temporary = tempdir().unwrap();
    let preparer = FfmpegAudioPreparer::new(resources.ffmpeg.clone());
    let request = AudioPreparationRequest {
        task_id: fixture.replace(['.', '/'], "-"),
        source,
        duration_ms: if fixture == "short_zh.mp4" {
            4_191
        } else {
            3_000
        },
        temporary_root: temporary.path().to_path_buf(),
    };
    let audio = preparer
        .prepare_temporary_wav(&request, CancellationToken::new())
        .await
        .unwrap();
    // v1.9.1 的独立 VAD 示例在当前 Apple Silicon 上启用 GPU 会崩溃，
    // 因此 VAD 固定走 CPU；Whisper 主识别仍使用 Metal。
    WhisperCppVadEngine::new(
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
    .unwrap()
}

#[tokio::test]
#[ignore = "需要锁定版本的 whisper.cpp VAD sidecar 与 Silero 模型"]
async fn real_silero_vad_detects_speech_and_rejects_silence_and_music() {
    let resources = resources();
    let speech = detect(&resources, "short_zh.mp4").await;
    assert!(!speech.is_empty());
    assert!(speech.iter().all(|region| region.start_ms < region.end_ms));
    assert!(speech.iter().all(|region| region.end_ms <= 4_191));

    assert!(detect(&resources, "no_speech.mp4").await.is_empty());
    assert!(detect(&resources, "music_only.mp4").await.is_empty());
}
