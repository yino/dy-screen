//! 真实 ASR CLI 分阶段验收。
//!
//! 测试使用一个封存资源根依次运行 probe、audio、VAD 和完整 ASR，确保每个阶段都能从
//! 同一个视频入口独立观察，同时验证无音轨探测、无人声输出、原视频不变和临时目录清理。

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;
use sha2::{Digest, Sha256};

fn resource_root() -> PathBuf {
    std::env::var_os("ASR_RESOURCE_ROOT")
        .map(PathBuf::from)
        .expect("ignored integration test requires ASR_RESOURCE_ROOT")
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/asr")
        .join(name)
}

fn file_sha256(path: &Path) -> String {
    hex::encode(Sha256::digest(std::fs::read(path).unwrap()))
}

fn cli_temporary_root(video: &Path) -> PathBuf {
    let canonical = std::fs::canonicalize(video).unwrap();
    let task_hash = hex::encode(Sha256::digest(canonical.to_string_lossy().as_bytes()));
    std::env::temp_dir()
        .join("dy-screen-asr-cli")
        .join(&task_hash[..16])
}

fn run_stage(video: &Path, root: &Path, stage: &str) -> Value {
    let output = Command::new(env!("CARGO_BIN_EXE_dy-screen"))
        .arg("asr")
        .arg(video)
        .arg("--resource-root")
        .arg(root)
        .arg("--stop-after")
        .arg(stage)
        .arg("--json")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stage={stage} stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "stage={stage} 返回无效 JSON: {error}; stdout={}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

#[test]
fn help_exposes_probe_audio_vad_and_asr_stop_points() {
    let output = Command::new(env!("CARGO_BIN_EXE_dy-screen"))
        .args(["asr", "--help"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("--stop-after"), "{stdout}");
    for stage in ["probe", "audio", "vad", "asr"] {
        assert!(stdout.contains(stage), "ASR help 缺少 {stage}: {stdout}");
    }
}

#[test]
#[ignore = "需要当前目标平台封存的 FFprobe、FFmpeg、VAD、Whisper 和模型资源"]
fn real_cli_runs_each_stage_without_modifying_source_or_leaving_temporary_audio() {
    let root = resource_root();
    let video = fixture("short_zh.mp4");
    let source_hash = file_sha256(&video);
    let temporary_root = cli_temporary_root(&video);
    let _ = std::fs::remove_dir_all(&temporary_root);

    let probe = run_stage(&video, &root, "probe");
    assert_eq!(probe["stage"], "probe");
    assert_eq!(probe["audioPresent"], true);
    assert!(probe["durationMs"].as_u64().unwrap() > 0);

    let audio = run_stage(&video, &root, "audio");
    assert_eq!(audio["stage"], "audio");
    assert_eq!(audio["format"], "pcm_s16_le_wav");
    assert_eq!(audio["sampleRateHz"], 16_000);
    assert_eq!(audio["channels"], 1);

    let vad = run_stage(&video, &root, "vad");
    assert_eq!(vad["stage"], "vad");
    assert!(!vad["regions"].as_array().unwrap().is_empty());

    let asr = run_stage(&video, &root, "asr");
    assert!(asr["text"].as_str().unwrap().contains("直播间"));
    assert!(
        asr["text"].as_str().unwrap().contains("99")
            || asr["text"].as_str().unwrap().contains("九十九")
    );

    assert_eq!(file_sha256(&video), source_hash);
    assert!(
        !temporary_root.exists(),
        "所有阶段结束后必须清理哈希隔离临时目录"
    );

    let video_only = run_stage(&fixture("video_only.mp4"), &root, "probe");
    assert_eq!(video_only["audioPresent"], false);
    let no_speech = run_stage(&fixture("no_speech.mp4"), &root, "vad");
    assert!(no_speech["regions"].as_array().unwrap().is_empty());
}
