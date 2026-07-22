//! 最终 `83/83` 证据审计器的完整链路与篡改拒绝测试。

use std::fs;
use std::path::{Path, PathBuf};

use dy_screen::asr::{
    AsrCompletionEvidencePaths, AsrQualityDataset, audit_asr_completion_evidence,
    evaluate_quality_results, render_quality_markdown,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tempfile::{TempDir, tempdir};

struct Fixture {
    _root: TempDir,
    paths: AsrCompletionEvidencePaths,
}

fn sha(label: &str) -> String {
    hex::encode(Sha256::digest(label.as_bytes()))
}

fn write_json(path: &Path, value: &Value) {
    let mut bytes = serde_json::to_vec_pretty(value).unwrap();
    bytes.push(b'\n');
    fs::write(path, bytes).unwrap();
}

fn report_path(root: &Path, name: &str) -> PathBuf {
    root.join(format!("{name}.json"))
}

fn completion_fixture() -> Fixture {
    let root = tempdir().unwrap();
    let mac_cli = sha("mac-cli");
    let mac_manifest = sha("mac-manifest");
    let windows_cli = sha("windows-cli");
    let windows_manifest = sha("windows-manifest");
    let windows_runtime = sha("windows-runtime");
    let timestamp = "2026-07-23T00:00:00Z";

    let windows_target = report_path(root.path(), "windows-target");
    write_json(
        &windows_target,
        &json!({
            "schemaVersion": 1, "collectedAtUtc": timestamp,
            "platform": "windows", "architecture": "x64",
            "manifestSha256": windows_manifest, "sidecarSha256": sha("windows-sidecar"),
            "runtimeSha256": windows_runtime, "cpuBaselineDeclared": true,
            "runtimeDeclared": true, "buildExitCode": 0, "adapterTestsExitCode": 0,
            "powershellParserTestsExitCode": 0, "integrationTestsExitCode": 0,
            "cliStageTestsExitCode": 0, "cliExitCode": 0,
            "runningCancellationPassed": true, "unicodeAndSpacePathPassed": true,
            "structuredOutputPassed": true, "sourceUnchanged": true,
            "sourceSha256Before": sha("windows-target-source"),
            "sourceSha256After": sha("windows-target-source"), "allPassed": true
        }),
    );

    let macos_release = report_path(root.path(), "macos-release");
    write_json(
        &macos_release,
        &json!({
            "schemaVersion": 1, "collectedAtUtc": timestamp,
            "platform": "macos", "architecture": "arm64",
            "dmgSha256": sha("dmg"), "mainBinarySha256": sha("mac-main"),
            "resourceManifestSha256": mac_manifest, "asrCliSha256": mac_cli,
            "asrBundleVerifierSha256": sha("mac-bundle"),
            "appCodeSignatureValid": true, "developerIdSigned": true,
            "hardenedRuntime": true, "machOCount": 12, "signedMachOCount": 12,
            "allMachOSigned": true, "appStapled": true, "dmgStapled": true,
            "gatekeeperAppPassed": true, "gatekeeperDmgPassed": true,
            "dmgVerified": true, "resourceBundleValid": true,
            "installedUnderApplications": true, "offlineObserved": true,
            "appLaunchPassed": true, "asrExitCode": 0, "asrJsonValid": true,
            "asrExpectedTextPassed": true, "asrStdoutSha256": sha("mac-stdout"),
            "asrStderrSha256": sha("mac-stderr"), "sourceUnchanged": true,
            "sourceSha256Before": sha("mac-source"),
            "sourceSha256After": sha("mac-source"), "allPassed": true
        }),
    );

    let windows_release = report_path(root.path(), "windows-release");
    write_json(
        &windows_release,
        &json!({
            "schemaVersion": 1, "collectedAtUtc": timestamp,
            "platform": "windows", "architecture": "x64",
            "smartScreenEvidenceId": "SEC-2026-0001",
            "installerSha256": sha("windows-installer"),
            "mainBinarySha256": sha("windows-main"), "asrCliSha256": windows_cli,
            "asrBundleVerifierSha256": sha("windows-bundle"),
            "resourceManifestSha256": windows_manifest, "runtimeSha256": windows_runtime,
            "installerSignatureValid": true, "signedFileCount": 8,
            "validSignedFileCount": 8, "allInstalledBinariesSigned": true,
            "runtimeSignatureValid": true, "installExitCode": 0,
            "resourceBundleValid": true, "vcRuntimeInstalled": true,
            "webView2Installed": true, "offlineObserved": true,
            "unicodeUserProfile": true, "appLaunchPassed": true, "cliExitCode": 0,
            "cliJsonValid": true, "asrExpectedTextPassed": true,
            "cliStdoutSha256": sha("windows-stdout"),
            "cliStderrSha256": sha("windows-stderr"), "sourceUnchanged": true,
            "sourceSha256Before": sha("windows-source"),
            "sourceSha256After": sha("windows-source"), "uninstallExitCode": 0,
            "uninstallRemovedInstallDirectory": true, "allPassed": true
        }),
    );

    let macos_performance = report_path(root.path(), "macos-performance");
    write_performance(
        &macos_performance,
        "macos",
        "arm64",
        "peakResidentSetBytes",
        &mac_cli,
        &mac_manifest,
        timestamp,
    );
    let windows_performance = report_path(root.path(), "windows-performance");
    write_performance(
        &windows_performance,
        "windows",
        "x64",
        "peakWorkingSetBytes",
        &windows_cli,
        &windows_manifest,
        timestamp,
    );

    let quality_dataset = root.path().join("quality-dataset.json");
    let samples = (0..10)
        .map(|index| {
            let product = format!("商品{index}");
            let streamer = format!("主播{index}");
            json!({
                "id": format!("sample-{index:02}"),
                "authorizationReference": format!("AUTH-2026-{index:04}"),
                "video": format!("authorized-video-{index:02}.mp4"),
                "durationMs": 1000,
                "speechRegions": [{ "startMs": 0, "endMs": 500 }],
                "terms": {
                    "products": [{ "label": product, "aliases": [] }],
                    "amounts": [{ "label": "99元", "aliases": ["九十九元"] }],
                    "streamers": [{ "label": streamer, "aliases": [] }]
                },
                "anchors": [{
                    "label": format!("商品{index}"), "aliases": [],
                    "startMs": 100, "toleranceMs": 100
                }]
            })
        })
        .collect::<Vec<_>>();
    write_json(
        &quality_dataset,
        &json!({
            "schemaVersion": 1,
            "datasetId": "authorized-live-v1",
            "samples": samples
        }),
    );
    let dataset_bytes = fs::read(&quality_dataset).unwrap();
    let dataset_sha256 = hex::encode(Sha256::digest(&dataset_bytes));
    let quality_results = root.path().join("quality-results");
    fs::create_dir(&quality_results).unwrap();
    for index in 0..10 {
        let id = format!("sample-{index:02}");
        let text = format!("商品{index}，99元，主播{index}");
        let output = json!({
            "engineId": "whisper.cpp", "engineVersion": "v1.9.1",
            "modelId": "whisper-small-multilingual-q5_1", "modelVersion": "locked",
            "language": "zh", "durationMs": 1000,
            "segments": [{
                "startMs": 100, "endMs": 200, "rawText": text,
                "normalizedText": text, "confidence": 0.9
            }],
            "text": text
        });
        let mut output_bytes = serde_json::to_vec_pretty(&output).unwrap();
        output_bytes.push(b'\n');
        fs::write(quality_results.join(format!("{id}.json")), &output_bytes).unwrap();
        let source = sha(&format!("source-{index}"));
        write_json(
            &quality_results.join(format!("{id}.evidence.json")),
            &json!({
                "schemaVersion": 1, "datasetSha256": dataset_sha256, "sampleId": id,
                "exitCode": 0, "wallMs": 500, "sourceSizeBytesBefore": 100,
                "sourceSizeBytesAfter": 100, "sourceSha256Before": source,
                "sourceSha256After": source, "sourceUnchanged": true,
                "asrBinarySha256": mac_cli, "resourceManifestSha256": mac_manifest,
                "stdoutValidJson": true, "stdoutSizeBytes": output_bytes.len(),
                "stdoutSha256": sha(&format!("stdout-{index}")),
                "resultSizeBytes": output_bytes.len(),
                "resultSha256": hex::encode(Sha256::digest(&output_bytes)),
                "stderrSizeBytes": 0, "stderrSha256": hex::encode(Sha256::digest([]))
            }),
        );
    }
    let dataset = AsrQualityDataset::from_path(&quality_dataset).unwrap();
    let quality =
        evaluate_quality_results(&dataset, &dataset_sha256, &quality_results, 10).unwrap();
    let quality_json = root.path().join("quality-report.json");
    write_json(&quality_json, &serde_json::to_value(&quality).unwrap());
    let quality_markdown = root.path().join("quality-report.md");
    fs::write(&quality_markdown, render_quality_markdown(&quality)).unwrap();

    Fixture {
        paths: AsrCompletionEvidencePaths {
            windows_target_report: windows_target,
            macos_release_report: macos_release,
            windows_release_report: windows_release,
            macos_performance_report: macos_performance,
            windows_performance_report: windows_performance,
            quality_dataset,
            quality_results,
            quality_json_report: quality_json,
            quality_markdown_report: quality_markdown,
        },
        _root: root,
    }
}

fn write_performance(
    path: &Path,
    platform: &str,
    architecture: &str,
    peak_field: &str,
    cli_sha256: &str,
    manifest_sha256: &str,
    timestamp: &str,
) {
    let mut value = json!({
        "schemaVersion": 1, "collectedAtUtc": timestamp,
        "platform": platform, "architecture": architecture,
        "physicalMemoryBytes": 8 * 1024_u64.pow(3),
        "strict8GbDeviceRequired": true, "strict8GbDeviceGatePassed": true,
        "inputSha256Before": sha(&format!("{platform}-input")),
        "inputSha256After": sha(&format!("{platform}-input")),
        "sourceUnchanged": true, "asrBinarySha256": cli_sha256,
        "resourceManifestSha256": manifest_sha256, "exitCode": 0,
        "wallMs": 1000, "audioDurationMs": 2000, "realtimeFactor": 0.5,
        "maximumThreadCount": 4, "maximumProcessCount": 2,
        "thermalStateBefore": "normal", "thermalStateAfter": "normal",
        "lingeringChildProcessCount": 0, "resourcesReleased": true,
        "stdoutValidJson": true, "stdoutSha256": sha(&format!("{platform}-stdout")),
        "stderrSha256": sha(&format!("{platform}-stderr"))
    });
    value[peak_field] = json!(512 * 1024 * 1024_u64);
    write_json(path, &value);
}

#[test]
fn complete_matching_evidence_is_the_only_ready_state() {
    let fixture = completion_fixture();
    let report = audit_asr_completion_evidence(&fixture.paths);
    assert!(report.ready_to_complete, "{:#?}", report.checks);
    assert!(report.tasks.iter().all(|task| task.ready));
    assert!(report.checks.iter().all(|check| check.passed));
}

#[test]
fn dev_machine_tampering_and_private_material_are_rejected_per_task() {
    let fixture = completion_fixture();
    let mut performance: Value =
        serde_json::from_slice(&fs::read(&fixture.paths.macos_performance_report).unwrap())
            .unwrap();
    performance["physicalMemoryBytes"] = json!(36 * 1024_u64.pow(3));
    write_json(&fixture.paths.macos_performance_report, &performance);

    let mut quality: Value =
        serde_json::from_slice(&fs::read(&fixture.paths.quality_json_report).unwrap()).unwrap();
    quality["failureRate"] = json!(0.75);
    write_json(&fixture.paths.quality_json_report, &quality);

    let mut target: Value =
        serde_json::from_slice(&fs::read(&fixture.paths.windows_target_report).unwrap()).unwrap();
    target["videoPath"] = json!(r"C:\Users\tester\video.mp4");
    write_json(&fixture.paths.windows_target_report, &target);

    let report = audit_asr_completion_evidence(&fixture.paths);
    assert!(!report.ready_to_complete);
    for task in ["1.6", "9.8", "9.9"] {
        assert!(
            !report
                .tasks
                .iter()
                .find(|value| value.task_id == task)
                .unwrap()
                .ready,
            "task {task} 必须被拒绝"
        );
    }
    assert!(
        report
            .checks
            .iter()
            .any(|check| check.code == "windows_target_file" && !check.passed)
    );
    assert!(
        report
            .checks
            .iter()
            .any(|check| check.code == "macos_performance_identity" && !check.passed)
    );
    assert!(
        report
            .checks
            .iter()
            .any(|check| check.code == "quality_recomputed_reports" && !check.passed)
    );
}

#[test]
fn missing_evidence_returns_a_full_non_sensitive_failure_report() {
    let root = tempdir().unwrap();
    let missing = root.path().join("missing");
    let report = audit_asr_completion_evidence(&AsrCompletionEvidencePaths {
        windows_target_report: missing.clone(),
        macos_release_report: missing.clone(),
        windows_release_report: missing.clone(),
        macos_performance_report: missing.clone(),
        windows_performance_report: missing.clone(),
        quality_dataset: missing.clone(),
        quality_results: missing.clone(),
        quality_json_report: missing.clone(),
        quality_markdown_report: missing,
    });
    assert!(!report.ready_to_complete);
    assert_eq!(report.tasks.len(), 6);
    assert!(report.tasks.iter().all(|task| !task.ready));
    let serialized = serde_json::to_string(&report).unwrap();
    assert!(!serialized.contains(root.path().to_string_lossy().as_ref()));
}
