use std::process::Command;

#[test]
fn quality_cli_help_exposes_collect_and_evaluate() {
    let output = Command::new(env!("CARGO_BIN_EXE_asr-quality"))
        .arg("--help")
        .output()
        .expect("run asr-quality help");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("collect"));
    assert!(stdout.contains("evaluate"));
}

#[cfg(unix)]
#[test]
fn collect_sanitizes_local_paths_and_evaluate_writes_reports() {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    let root = tempfile::tempdir().unwrap();
    let video = root.path().join("授权直播样本.mp4");
    fs::write(&video, b"immutable authorized fixture").unwrap();
    let dataset = root.path().join("dataset.json");
    fs::write(
        &dataset,
        serde_json::to_vec_pretty(&serde_json::json!({
            "schemaVersion": 1,
            "datasetId": "authorized-zh-live-v1",
            "samples": [{
                "id": "sample-01",
                "authorizationReference": "consent-ticket-001",
                "video": "授权直播样本.mp4",
                "durationMs": 1_000,
                "speechRegions": [{"startMs": 100, "endMs": 600}],
                "terms": {
                    "products": [{"label": "测试商品", "aliases": []}],
                    "amounts": [{"label": "99元", "aliases": ["九十九元"]}],
                    "streamers": [{"label": "小明", "aliases": []}]
                },
                "anchors": [{
                    "label": "测试商品",
                    "aliases": [],
                    "startMs": 100,
                    "toleranceMs": 200
                }]
            }]
        }))
        .unwrap(),
    )
    .unwrap();

    let fake_asr = root.path().join("fake-asr");
    fs::write(
        &fake_asr,
        r#"#!/bin/sh
printf '%s\n' '{"video":"/Users/private/秘密直播.mp4","engineId":"fake-engine","engineVersion":"1.0","modelId":"fake-model","modelVersion":"1.0","language":"zh","durationMs":1000,"segments":[{"startMs":100,"endMs":500,"rawText":"小明介绍测试商品九十九元","normalizedText":"小明介绍测试商品99元","confidence":0.9}],"text":"小明介绍测试商品99元"}'
printf '%s\n' '/Users/private/秘密直播.mp4 diagnostic' >&2
"#,
    )
    .unwrap();
    let mut permissions = fs::metadata(&fake_asr).unwrap().permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&fake_asr, permissions).unwrap();

    let results = root.path().join("results");
    let collect = Command::new(env!("CARGO_BIN_EXE_asr-quality"))
        .arg("collect")
        .arg("--dataset")
        .arg(&dataset)
        .arg("--results")
        .arg(&results)
        .arg("--asr-binary")
        .arg(&fake_asr)
        .args(["--minimum-samples", "1"])
        .output()
        .expect("collect quality evidence");
    assert!(
        collect.status.success(),
        "{}",
        String::from_utf8_lossy(&collect.stderr)
    );

    let result_text = fs::read_to_string(results.join("sample-01.json")).unwrap();
    let evidence_text = fs::read_to_string(results.join("sample-01.evidence.json")).unwrap();
    assert!(!result_text.contains("/Users/private"));
    assert!(!result_text.contains("\"video\""));
    assert!(!evidence_text.contains("/Users/private"));
    assert!(!evidence_text.contains("diagnostic"));
    assert!(evidence_text.contains("stderrSha256"));
    assert!(evidence_text.contains("resultSha256"));
    assert!(evidence_text.contains("\"sourceUnchanged\": true"));

    let json_report = root.path().join("quality-report.json");
    let markdown_report = root.path().join("quality-report.md");
    let evaluate = Command::new(env!("CARGO_BIN_EXE_asr-quality"))
        .arg("evaluate")
        .arg("--dataset")
        .arg(&dataset)
        .arg("--results")
        .arg(&results)
        .arg("--json-report")
        .arg(&json_report)
        .arg("--markdown-report")
        .arg(&markdown_report)
        .args(["--minimum-samples", "1"])
        .output()
        .expect("evaluate quality evidence");
    assert!(
        evaluate.status.success(),
        "{}",
        String::from_utf8_lossy(&evaluate.stderr)
    );
    let report: serde_json::Value =
        serde_json::from_slice(&fs::read(json_report).unwrap()).unwrap();
    assert_eq!(report["successfulSamples"], 1);
    assert_eq!(report["products"]["recall"], 1.0);
    assert!(
        fs::read_to_string(markdown_report)
            .unwrap()
            .contains("商品名召回率")
    );

    fs::write(results.join("sample-01.json"), format!("{result_text} ")).unwrap();
    let tampered_report = root.path().join("tampered-report.json");
    let tampered_markdown = root.path().join("tampered-report.md");
    let tampered = Command::new(env!("CARGO_BIN_EXE_asr-quality"))
        .arg("evaluate")
        .arg("--dataset")
        .arg(&dataset)
        .arg("--results")
        .arg(&results)
        .arg("--json-report")
        .arg(&tampered_report)
        .arg("--markdown-report")
        .arg(&tampered_markdown)
        .args(["--minimum-samples", "1"])
        .output()
        .expect("evaluate tampered evidence");
    assert!(tampered.status.success());
    let tampered_value: serde_json::Value =
        serde_json::from_slice(&fs::read(tampered_report).unwrap()).unwrap();
    assert_eq!(
        tampered_value["samples"][0]["failureCode"],
        "result_integrity_mismatch"
    );

    let repeated = Command::new(env!("CARGO_BIN_EXE_asr-quality"))
        .arg("collect")
        .arg("--dataset")
        .arg(&dataset)
        .arg("--results")
        .arg(&results)
        .arg("--asr-binary")
        .arg(&fake_asr)
        .args(["--minimum-samples", "1"])
        .output()
        .expect("reject repeated collection");
    assert!(!repeated.status.success());
    assert!(String::from_utf8_lossy(&repeated.stderr).contains("请使用新的结果目录"));
}
