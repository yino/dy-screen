use std::process::Command;

#[test]
fn help_exposes_profile_resolve_record_and_asr_commands() {
    let output = Command::new(env!("CARGO_BIN_EXE_dy-screen"))
        .arg("--help")
        .output()
        .expect("run CLI help");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("inspect-profile"));
    assert!(stdout.contains("resolve"));
    assert!(stdout.contains("record"));
    assert!(stdout.contains("asr"));
}

#[test]
fn asr_help_exposes_backward_compatible_stage_diagnostics() {
    let output = Command::new(env!("CARGO_BIN_EXE_dy-screen"))
        .args(["asr", "--help"])
        .output()
        .expect("run ASR help");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("--stop-after"), "{stdout}");
    assert!(stdout.contains("probe"), "{stdout}");
    assert!(stdout.contains("audio"), "{stdout}");
    assert!(stdout.contains("vad"), "{stdout}");
    assert!(stdout.contains("asr"), "{stdout}");
    assert!(stdout.contains("default: asr"), "{stdout}");
}

#[test]
fn asr_rejects_missing_video_before_launching_media_processes() {
    let output = Command::new(env!("CARGO_BIN_EXE_dy-screen"))
        .args([
            "asr",
            "/path/that/does/not/exist.mp4",
            "--resource-root",
            "/path/that/does/not/exist",
        ])
        .output()
        .expect("run ASR command");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("资源清单缺失"), "{stderr}");
}

#[test]
fn resolve_rejects_unsupported_host_without_network_access() {
    let output = Command::new(env!("CARGO_BIN_EXE_dy-screen"))
        .args(["resolve", "https://example.com/not-douyin"])
        .output()
        .expect("run resolve command");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("unsupported room URL host"), "{stderr}");
}
