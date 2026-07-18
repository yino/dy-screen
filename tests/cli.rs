use std::process::Command;

#[test]
fn help_exposes_resolve_and_record_commands() {
    let output = Command::new(env!("CARGO_BIN_EXE_dy-screen"))
        .arg("--help")
        .output()
        .expect("run CLI help");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("resolve"));
    assert!(stdout.contains("record"));
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
