use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Duration;

use dy_screen::asr::{
    AudioPreparationRequest, FfmpegAudioPreparer, FfprobeMediaInspector, FrozenMediaSource,
    MediaAudioPreparer, MediaInspector, cleanup_stale_audio_files,
};
use sha2::{Digest, Sha256};
use tempfile::tempdir;
use tokio::io::AsyncReadExt;
use tokio_util::sync::CancellationToken;

fn fixture(path: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/asr")
        .join(path)
}

#[tokio::test]
async fn ffprobe_inspects_original_media_with_and_without_audio() {
    let inspector = FfprobeMediaInspector::new(PathBuf::from("ffprobe"), Duration::from_secs(5));
    let speech = FrozenMediaSource::from_path(&fixture("short_zh.mp4")).unwrap();
    let speech_probe = inspector
        .inspect(&speech, CancellationToken::new())
        .await
        .unwrap();
    assert!(speech_probe.audio_present);
    assert_eq!(speech_probe.duration_ms, 4_191);
    assert_eq!(speech_probe.audio_codec.as_deref(), Some("aac"));

    let video_only = FrozenMediaSource::from_path(&fixture("video_only.mp4")).unwrap();
    let video_probe = inspector
        .inspect(&video_only, CancellationToken::new())
        .await
        .unwrap();
    assert!(!video_probe.audio_present);
    assert_eq!(video_probe.duration_ms, 3_000);
}

#[test]
fn frozen_media_rejects_source_changes() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("changing.mp4");
    std::fs::write(&path, b"before").unwrap();
    let frozen = FrozenMediaSource::from_path(&path).unwrap();
    std::fs::write(&path, b"after-with-different-size").unwrap();
    assert_eq!(frozen.verify_unchanged().unwrap_err().code, "media_changed");
}

#[tokio::test]
async fn ffmpeg_prepares_unicode_path_wav_and_cleans_it_without_touching_source() {
    let directory = tempdir().unwrap();
    let source_path = fixture("Windows 中文路径/测试 视频.mp4");
    let source_before = Sha256::digest(std::fs::read(&source_path).unwrap());
    let source = FrozenMediaSource::from_path(&source_path).unwrap();
    let preparer = FfmpegAudioPreparer::new(PathBuf::from("ffmpeg"));
    assert!(preparer.capabilities().pcm_pipe);
    assert!(preparer.capabilities().temporary_wav);
    let request = AudioPreparationRequest {
        task_id: "unicode-path".to_owned(),
        source,
        duration_ms: 4_191,
        temporary_root: directory.path().join("asr-temp"),
    };
    let lease = preparer
        .prepare_temporary_wav(&request, CancellationToken::new())
        .await
        .unwrap();
    let wav_path = lease.audio().path.clone();
    assert!(wav_path.is_file());
    let output = std::process::Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "stream=sample_rate,channels,codec_name",
            "-of",
            "json",
        ])
        .arg(&wav_path)
        .output()
        .unwrap();
    let probe = String::from_utf8_lossy(&output.stdout);
    assert!(probe.contains("pcm_s16le"));
    assert!(probe.contains("16000"));
    assert!(probe.contains("\"channels\": 1"));
    drop(lease);
    assert!(!wav_path.exists());
    assert_eq!(
        Sha256::digest(std::fs::read(&source_path).unwrap()),
        source_before
    );
}

#[tokio::test]
async fn ffmpeg_pcm_pipe_is_streamable_and_cancellable() {
    let directory = tempdir().unwrap();
    let source = FrozenMediaSource::from_path(&fixture("short_zh.mp4")).unwrap();
    let preparer = FfmpegAudioPreparer::new(PathBuf::from("ffmpeg"));
    let request = AudioPreparationRequest {
        task_id: "pipe-test".to_owned(),
        source,
        duration_ms: 4_191,
        temporary_root: directory.path().join("unused"),
    };
    let mut pipe = preparer
        .open_pcm_pipe(&request, CancellationToken::new())
        .await
        .unwrap();
    let mut stdout = pipe.take_stdout().expect("PCM stdout");
    let mut buffer = vec![0_u8; 3_200];
    let read = stdout.read(&mut buffer).await.unwrap();
    assert!(read > 0);
    drop(stdout);
    pipe.cancel().await;
}

#[tokio::test]
async fn ffmpeg_reports_unwritable_temporary_root_without_modifying_source() {
    let directory = tempdir().unwrap();
    let blocked_root = directory.path().join("not-a-directory");
    std::fs::write(&blocked_root, b"file blocks directory creation").unwrap();
    let source_path = fixture("short_zh.mp4");
    let before = Sha256::digest(std::fs::read(&source_path).unwrap());
    let preparer = FfmpegAudioPreparer::new(PathBuf::from("ffmpeg"));
    let request = AudioPreparationRequest {
        task_id: "disk-error".to_owned(),
        source: FrozenMediaSource::from_path(&source_path).unwrap(),
        duration_ms: 4_191,
        temporary_root: blocked_root,
    };
    let error = preparer
        .prepare_temporary_wav(&request, CancellationToken::new())
        .await
        .unwrap_err();
    assert_eq!(error.code, "temporary_directory_unavailable");
    assert_eq!(Sha256::digest(std::fs::read(&source_path).unwrap()), before);
}

#[test]
fn startup_cleanup_only_removes_inactive_asr_audio_files() {
    let directory = tempdir().unwrap();
    let nested = directory.path().join("project-1").join("input-2");
    std::fs::create_dir_all(&nested).unwrap();
    for name in ["active.wav", "stale.wav", "stale-two.wav.part", "keep.mp4"] {
        std::fs::write(directory.path().join(name), b"test").unwrap();
    }
    std::fs::write(nested.join("nested-stale.wav.part"), b"test").unwrap();
    let active = HashSet::from(["active".to_owned()]);
    assert_eq!(
        cleanup_stale_audio_files(directory.path(), &active).unwrap(),
        3
    );
    assert!(directory.path().join("active.wav").is_file());
    assert!(directory.path().join("keep.mp4").is_file());
    assert!(!directory.path().join("stale.wav").exists());
    assert!(!directory.path().join("stale-two.wav.part").exists());
    assert!(!nested.exists());
}
