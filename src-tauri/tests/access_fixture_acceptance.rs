use std::process::Command;

#[test]
fn tauri_manifest_keeps_the_desktop_app_as_default_binary() {
    let manifest = include_str!("../Cargo.toml");
    assert!(manifest.contains("default-run = \"dy-screen-app\""));
}

#[test]
fn supported_and_challenge_fixtures_match_stderr_and_jsonl_without_secrets() {
    let directory = tempfile::tempdir().expect("create fixture log directory");
    let binary = env!("CARGO_BIN_EXE_access_fixture");
    let mut stderr_lines = Vec::new();

    for (fixture, expected) in [
        ("supported", "live"),
        ("challenge", "verification_required"),
    ] {
        let output = Command::new(binary)
            .arg(fixture)
            .arg(directory.path())
            .output()
            .expect("run access fixture tool");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stderr = String::from_utf8(output.stderr).expect("stderr is UTF-8 JSONL");
        let line = stderr.trim();
        let value: serde_json::Value =
            serde_json::from_str(line).expect("stderr is one JSON object");
        assert_eq!(value["classification"], expected);
        assert_eq!(value["channel"], "browser");
        assert_eq!(value["stage"], "probe");
        stderr_lines.push(line.to_owned());
    }

    let log_path = std::fs::read_dir(directory.path())
        .expect("read fixture log directory")
        .next()
        .expect("JSONL exists")
        .expect("valid JSONL entry")
        .path();
    let jsonl = std::fs::read_to_string(log_path).expect("read JSONL");
    let file_lines = jsonl.lines().collect::<Vec<_>>();
    assert_eq!(file_lines, stderr_lines);
    assert!(!jsonl.contains("top-secret"));
    assert!(!jsonl.contains("fixture-signature-must-not-leak"));
    assert!(!jsonl.to_ascii_lowercase().contains("cookie"));
    assert!(!jsonl.contains("stream_url"));
}
