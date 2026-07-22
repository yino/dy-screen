use std::process::Command;

#[test]
fn evidence_cli_help_exposes_all_external_inputs() {
    let output = Command::new(env!("CARGO_BIN_EXE_asr-evidence"))
        .arg("--help")
        .output()
        .expect("run asr-evidence help");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    for argument in [
        "--windows-target",
        "--macos-release",
        "--windows-release",
        "--macos-performance",
        "--windows-performance",
        "--quality-dataset",
        "--quality-results",
        "--quality-json",
        "--quality-markdown",
    ] {
        assert!(stdout.contains(argument), "帮助缺少 {argument}: {stdout}");
    }
}

#[test]
fn evidence_cli_rejects_missing_reports_with_pathless_json() {
    let root = tempfile::tempdir().unwrap();
    let missing = root.path().join("private-evidence-path");
    let output = Command::new(env!("CARGO_BIN_EXE_asr-evidence"))
        .arg("--windows-target")
        .arg(&missing)
        .arg("--macos-release")
        .arg(&missing)
        .arg("--windows-release")
        .arg(&missing)
        .arg("--macos-performance")
        .arg(&missing)
        .arg("--windows-performance")
        .arg(&missing)
        .arg("--quality-dataset")
        .arg(&missing)
        .arg("--quality-results")
        .arg(&missing)
        .arg("--quality-json")
        .arg(&missing)
        .arg("--quality-markdown")
        .arg(&missing)
        .output()
        .expect("audit missing completion evidence");

    assert!(!output.status.success());
    assert!(output.stderr.is_empty(), "{:?}", output.stderr);
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["readyToComplete"], false);
    assert_eq!(report["tasks"].as_array().unwrap().len(), 6);
    assert!(
        report["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .all(|task| task["ready"] == false)
    );
    assert!(!String::from_utf8_lossy(&output.stdout).contains("private-evidence-path"));
    assert!(
        !String::from_utf8_lossy(&output.stdout).contains(root.path().to_string_lossy().as_ref())
    );
}
