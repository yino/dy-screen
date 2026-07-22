use std::fs;
use std::path::Path;

use dy_screen::asr::{AsrBundleManifest, AsrQualityDataset};

#[test]
fn macos_ffmpeg_build_is_locked_lgpl_offline_and_relocatable() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let script = fs::read_to_string(root.join("scripts/build-asr-ffmpeg-macos.sh")).unwrap();
    for required in [
        "464beb5e7bf0c311e68b45ae2f04e9cc2af88851abb4082231742a74d97b524c",
        "--disable-network",
        "--disable-everything",
        "--enable-shared",
        "--disable-static",
        "--enable-decoder=",
        "@loader_path",
        "@executable_path/../../lib/macos-aarch64",
        "--enable-(gpl|nonfree)",
        "COPYING.LGPLv2.1",
    ] {
        assert!(script.contains(required), "FFmpeg 构建脚本缺少 {required}");
    }
    assert!(!script.contains("--enable-network"));
    assert!(!script.contains("--enable-gpl "));
    assert!(!script.contains("--enable-nonfree "));
}

#[test]
fn macos_whisper_build_is_locked_static_metal_offline_and_relocatable() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let script = fs::read_to_string(root.join("scripts/build-asr-whisper-macos.sh")).unwrap();
    for required in [
        "d8cd961352377b1cc612224016a9ebdfe0ae508dc2b2f9ef514b341d672e3fdc",
        "f049fff95a089aa9969deb009cdd4892b3e74916",
        "-DBUILD_SHARED_LIBS=OFF",
        "-DGGML_STATIC=ON",
        "-DGGML_METAL=ON",
        "-DGGML_METAL_EMBED_LIBRARY=ON",
        "-DWHISPER_CURL=OFF",
        "BAD_DEPENDENCIES",
        "codesign --verify --strict",
    ] {
        assert!(script.contains(required), "Whisper 构建脚本缺少 {required}");
    }
    assert!(!script.contains("curl "));
    assert!(!script.contains("wget "));
}

#[test]
fn windows_whisper_build_locks_cpu_baseline_and_static_dependencies() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let script = fs::read_to_string(root.join("scripts/build-asr-whisper-windows.ps1")).unwrap();
    for required in [
        "d8cd961352377b1cc612224016a9ebdfe0ae508dc2b2f9ef514b341d672e3fdc",
        "f049fff95a089aa9969deb009cdd4892b3e74916",
        "-DBUILD_SHARED_LIBS=OFF",
        "-DGGML_STATIC=ON",
        "-DGGML_NATIVE=OFF",
        "-DGGML_SSE42=ON",
        "-DGGML_AVX=OFF",
        "-DGGML_AVX2=OFF",
        "-DGGML_BMI2=OFF",
        "-DGGML_METAL=OFF",
        "-DWHISPER_CURL=OFF",
        "dumpbin.exe /dependents",
        "machine \\(x64\\)",
    ] {
        assert!(
            script.contains(required),
            "Windows Whisper 构建脚本缺少 {required}"
        );
    }
}

#[test]
fn manifest_declares_runtime_libraries_and_complete_license_texts() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let manifest = AsrBundleManifest::from_json(
        &fs::read_to_string(root.join("resources/asr/manifest.json")).unwrap(),
    )
    .unwrap();
    let macos = manifest.platform("macos", "aarch64").unwrap();
    assert_eq!(macos.libraries.len(), 7);
    assert_eq!(manifest.license_files.len(), 5);
    for relative in manifest.license_files {
        let path = root.join("resources/asr").join(relative);
        assert!(path.is_file(), "缺少完整许可证文本 {}", path.display());
        assert!(fs::metadata(path).unwrap().len() > 500);
    }
}

#[test]
fn quality_dataset_template_requires_ten_authorized_samples() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let template = root.join("docs/templates/asr-quality-dataset.example.json");
    let mut dataset = AsrQualityDataset::from_path(&template).unwrap();
    assert_eq!(dataset.samples.len(), 10);
    assert!(
        dataset.validate(10).is_err(),
        "占位授权引用不得用于正式验收"
    );
    for (index, sample) in dataset.samples.iter_mut().enumerate() {
        sample.authorization_reference = format!("consent-ticket-{:02}", index + 1);
    }
    dataset.validate(10).unwrap();
    assert!(
        dataset
            .samples
            .iter()
            .all(|sample| !sample.authorization_reference.trim().is_empty())
    );
}

#[test]
fn performance_collectors_capture_required_metrics_without_raw_paths_or_stderr() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let macos = fs::read_to_string(root.join("scripts/collect-asr-performance-macos.sh")).unwrap();
    for required in [
        "--require-8gb",
        "physicalMemoryBytes",
        "inputSha256Before",
        "resourceManifestSha256",
        "realtimeFactor",
        "peakResidentSetBytes",
        "maximumThreadCount",
        "ps -M -p",
        "thermalStateBefore",
        "lingeringChildProcessCount",
        "stderrSha256",
    ] {
        assert!(macos.contains(required), "macOS 性能脚本缺少 {required}");
    }
    assert!(!macos.contains("python3"));
    assert!(!macos.contains("stderrText"));
    assert!(!macos.contains("videoPath"));
    assert!(
        macos.find("no thermal warning").unwrap() < macos.find("*warning*").unwrap(),
        "无热警告必须先于通用 warning 分支匹配"
    );

    let windows =
        fs::read_to_string(root.join("scripts/collect-asr-performance-windows.ps1")).unwrap();
    for required in [
        "Require8GB",
        "ConvertTo-WindowsCommandLineArgument",
        "ProcessStartInfo",
        "physicalMemoryBytes",
        "inputSha256Before",
        "resourceManifestSha256",
        "realtimeFactor",
        "peakWorkingSetBytes",
        "maximumThreadCount",
        "thermalStateBefore",
        "lingeringChildProcessCount",
        "stderrSha256",
    ] {
        assert!(
            windows.contains(required),
            "Windows 性能脚本缺少 {required}"
        );
    }
    assert!(!windows.contains("stderr = $stderrText"));
    assert!(!windows.contains("videoPath"));
}

#[test]
fn windows_target_runner_covers_real_cpu_unicode_cancel_and_runtime_evidence() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let script = fs::read_to_string(root.join("scripts/test-asr-windows-target.ps1")).unwrap();
    for required in [
        "asr_whisper_windows_adapter",
        "asr_whisper_integration",
        "asr_cli_stages",
        "windows_powershell_scripts",
        "--ignored",
        "Windows 中文路径",
        "测试 视频.mp4",
        "minimumCpuFeatures",
        "sse4.2",
        "runtimeFile",
        "runningCancellationPassed",
        "unicodeAndSpacePathPassed",
        "structuredOutputPassed",
        "sourceUnchanged",
        "allPassed",
    ] {
        assert!(
            script.contains(required),
            "Windows 目标机脚本缺少 {required}"
        );
    }
    assert!(!script.contains("repoRoot ="));
    assert!(!script.contains("resourceRoot ="));
    assert!(!script.contains("fixturePath ="));
    assert!(!script.contains("testLog ="));
}

#[test]
fn macos_release_verifier_requires_developer_id_notarization_offline_asr_and_launch() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let script = fs::read_to_string(root.join("scripts/verify-asr-release-macos.sh")).unwrap();
    for required in [
        "Developer ID Application:",
        "flags=.*runtime",
        "codesign --verify --deep --strict",
        "stapler validate",
        "spctl --assess",
        "hdiutil verify",
        "/Applications/*.app",
        "route -n get default",
        "asr-bundle",
        "resourceBundleValid",
        "offlineObserved",
        "appLaunchPassed",
        "asrExpectedTextPassed",
        "sourceUnchanged",
        "allPassed",
    ] {
        assert!(
            script.contains(required),
            "macOS 发行验收脚本缺少 {required}"
        );
    }
    assert!(!script.contains("asrStdoutText"));
    assert!(!script.contains("asrStderrText"));
    assert!(!script.contains("appPath"));
}

#[test]
fn windows_release_verifier_covers_signatures_runtime_unicode_offline_install_and_uninstall() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let script = fs::read_to_string(root.join("scripts/verify-asr-release-windows.ps1")).unwrap();
    for required in [
        "Get-AuthenticodeSignature",
        "TimeStamperCertificate",
        "ExpectedSignerThumbprint",
        "SmartScreenEvidenceId",
        "Get-NetAdapter",
        "unicodeUserProfile",
        "vcRuntimeInstalled",
        "webView2Installed",
        "resourceBundleValid",
        "allInstalledBinariesSigned",
        "ASR 验收 空格",
        "appLaunchPassed",
        "asrExpectedTextPassed",
        "uninstallRemovedInstallDirectory",
        "sourceUnchanged",
        "asrCliSha256",
        "asrBundleVerifierSha256",
        "allPassed",
    ] {
        assert!(
            script.contains(required),
            "Windows 发行验收脚本缺少 {required}"
        );
    }
    assert!(!script.contains("installerPath ="));
    assert!(!script.contains("installDirectory ="));
    assert!(!script.contains("cliStdoutText"));
    assert!(!script.contains("cliStderrText"));
}

#[cfg(unix)]
#[test]
fn macos_performance_collector_has_valid_posix_shell_syntax() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let status = std::process::Command::new("sh")
        .arg("-n")
        .arg(root.join("scripts/collect-asr-performance-macos.sh"))
        .status()
        .unwrap();
    assert!(status.success());

    let status = std::process::Command::new("sh")
        .arg("-n")
        .arg(root.join("scripts/verify-asr-release-macos.sh"))
        .status()
        .unwrap();
    assert!(status.success());
}

#[test]
fn quality_and_performance_workflows_are_documented_and_exposed_by_make() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let makefile = fs::read_to_string(root.join("Makefile")).unwrap();
    let wiki = fs::read_to_string(root.join("docs/wiki/ASR-真实样本与性能验收.md")).unwrap();
    for target in [
        "asr-quality-collect:",
        "asr-quality-evaluate:",
        "asr-test-windows-target:",
        "asr-verify-release-macos:",
        "asr-verify-release-windows:",
        "asr-performance-macos:",
        "asr-performance-windows:",
        "asr-evidence-audit:",
    ] {
        assert!(makefile.contains(target), "Makefile 缺少 {target}");
    }
    assert!(makefile.contains("POWERSHELL ?= powershell.exe"));
    assert!(makefile.contains("EXECUTABLE_SUFFIX := $(if $(filter Windows_NT,$(OS)),.exe,)"));
    assert!(makefile.contains("BINARY ?= target/release/dy-screen$(EXECUTABLE_SUFFIX)"));
    for required in [
        "authorizationReference",
        "asr-quality -- collect",
        "asr-quality -- evaluate",
        "make asr-evidence-audit",
        "readyToComplete=true",
        "asrCliSha256",
        "collect-asr-performance-macos.sh",
        "collect-asr-performance-windows.ps1",
        "test-asr-windows-target.ps1",
        "模板、单元测试、固定短句 fixture",
    ] {
        assert!(wiki.contains(required), "验收 Wiki 缺少 {required}");
    }
}

#[test]
fn staged_asr_test_commands_are_independent_documented_and_use_one_resource_root() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let makefile = fs::read_to_string(root.join("Makefile")).unwrap();
    let wiki = fs::read_to_string(root.join("docs/wiki/AI-本地语音识别.md")).unwrap();
    for target in [
        "asr-test-contract:",
        "asr-test-media:",
        "asr-test-vad:",
        "asr-test-whisper:",
        "asr-test-cli:",
        "asr-test-stages:",
    ] {
        assert!(makefile.contains(target), "Makefile 缺少 {target}");
    }
    for required in [
        "ASR_RESOURCE_ROOT",
        "make asr-test-contract",
        "make asr-test-media",
        "make asr-test-vad",
        "make asr-test-whisper",
        "make asr-test-cli",
        "make asr-test-stages",
    ] {
        assert!(wiki.contains(required), "分阶段测试 Wiki 缺少 {required}");
    }
}

#[test]
fn openspec_traceability_matrix_is_linked_from_the_acceptance_record() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let record = fs::read_to_string(root.join("docs/wiki/AI-ASR-验收记录.md")).unwrap();
    assert!(record.contains("AI-ASR-需求测试追踪矩阵.md"));
}
