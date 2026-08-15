use std::fs;
use std::path::{Path, PathBuf};

fn repository_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(relative: impl AsRef<Path>) -> String {
    fs::read_to_string(repository_root().join(relative)).expect("读取 Windows 发行契约文件")
}

#[test]
fn windows_doctor_covers_native_build_and_signing_toolchain_without_private_paths() {
    let script = read("scripts/windows-build-doctor.ps1");
    for required in [
        "windowsX64",
        "powershell51",
        "rustMsvc",
        "visualStudioDeveloperShell",
        "windowsSdk",
        "msys2Ucrt64",
        "cargo.exe",
        "node.exe",
        "cmake.exe",
        "dumpbin.exe",
        "signtool.exe",
        "makensis.exe",
        "ConvertTo-SafeVersion",
        "x86_64-pc-windows-msvc",
        "VSCMD_VER",
        "WindowsSDKVersion",
    ] {
        assert!(script.contains(required), "环境诊断缺少 {required}");
    }
    assert!(!script.contains("C:\\Users\\"));
    assert!(!script.to_ascii_lowercase().contains("password"));
}

#[test]
fn windows_ffmpeg_builder_pins_lgpl_source_and_all_runtime_capabilities() {
    let script = read("scripts/build-asr-ffmpeg-windows.ps1");
    assert!(script.contains("464beb5e7bf0c311e68b45ae2f04e9cc2af88851abb4082231742a74d97b524c"));
    for required in [
        "--disable-everything",
        "--disable-autodetect",
        "--enable-schannel",
        "--enable-mediafoundation",
        "--enable-encoder=pcm_s16le,aac,h264_mf,mjpeg",
        "--enable-demuxer=mov,matroska,flv,hls",
        "concat,image2",
        "--enable-filter=aresample,aformat,anull,asetpts,scale,format,setpts,pad,setsar,volume,fade,concat,overlay",
        "FFmpeg-LGPL-2.1.txt",
        "Assert-X64Pe",
        "Assert-NoBundledRuntimeDependency",
    ] {
        assert!(script.contains(required), "FFmpeg 构建缺少 {required}");
    }
    assert!(!script.contains("libx264"));
    assert!(!script.contains("--enable-gpl"));
    assert!(!script.contains("--enable-nonfree"));
    for stage in [
        "configure-build",
        "pe-dependency-validation",
        "capability-validation",
        "build-records",
    ] {
        assert!(script.contains(stage), "Windows FFmpeg 缺少诊断阶段 {stage}");
    }
    assert!(script.contains("Windows FFmpeg configure/build failed"));
    assert!(script.contains("ConvertTo-CiAnnotationValue"));
}

#[test]
fn windows_resource_assembly_requires_x64_pe_microsoft_runtime_and_atomic_output() {
    let script = read("scripts/prepare-asr-resources-windows.ps1");
    let gitignore = read(".gitignore");
    for required in [
        "Assert-X64Pe",
        "Get-AuthenticodeSignature",
        "Microsoft Corporation",
        "cc0ff0eb1dc3f5188ae6300faef32bf5beeba4bdd6e8e445a9184072096b713b",
        "VC++ x64 运行库 SHA-256 与锁定版本不一致",
        "x86 bootstrapper",
        "Microsoft-VCRedist.txt",
        "resourceIntegrity",
        "SHA256SUMS",
        "build-records\\windows",
        "source_sha256=$ExpectedFfmpegSha256",
        "source_sha256=$ExpectedWhisperSha256",
        "公共资源 $relative 的 SHA-256",
        ".dy-screen-windows-resource-source",
        ".part.",
    ] {
        assert!(script.contains(required), "Windows 资源组装缺少 {required}");
    }
    assert!(!script.contains("Invoke-WebRequest"));
    assert!(!script.contains("Start-BitsTransfer"));
    assert!(!script.contains("Assert-X64Pe -Path $VcRedist"));
    assert!(
        gitignore
            .lines()
            .any(|line| line == "/resources/asr-source-windows/")
    );
}

#[test]
fn windows_tauri_and_nsis_contract_is_offline_stable_and_current_user_scoped() {
    let base: serde_json::Value = serde_json::from_str(&read("src-tauri/tauri.conf.json")).unwrap();
    let windows: serde_json::Value =
        serde_json::from_str(&read("src-tauri/tauri.windows.conf.json")).unwrap();
    assert_eq!(base["productName"], "切片智能体");
    assert_eq!(base["identifier"], "com.yino.dyscreen");
    assert_eq!(windows["bundle"]["targets"][0], "nsis");
    assert_eq!(
        windows["bundle"]["windows"]["webviewInstallMode"]["type"],
        "offlineInstaller"
    );
    assert_eq!(
        windows["bundle"]["windows"]["nsis"]["installMode"],
        "currentUser"
    );

    let hooks = read("src-tauri/windows/asr-runtime-hooks.nsh");
    assert!(hooks.contains("vc_redist.x64.exe"));
    assert!(hooks.contains("IntCmp $0 3010 runtime_ready"));
    assert!(hooks.contains("Abort"));
}

#[test]
fn release_build_uses_certificate_store_and_explicit_unsigned_development_mode() {
    let installer = read("scripts/build-windows-installer.ps1");
    let signer = read("scripts/sign-windows-resource-binaries.ps1");
    let makefile = read("Makefile");
    for required in [
        "Development",
        "Release",
        "certificateThumbprint",
        "digestAlgorithm",
        "timestampUrl",
        "verify-signature",
        "sign-windows-resource-binaries.ps1",
    ] {
        assert!(installer.contains(required), "NSIS 构建缺少 {required}");
    }
    assert!(signer.contains("signtool.exe sign /sha1"));
    assert!(signer.contains("TimeStamperCertificate"));
    assert!(!signer.to_ascii_lowercase().contains(".pfx"));
    assert!(!signer.to_ascii_lowercase().contains("password"));
    for target in [
        "windows-build-doctor:",
        "asr-ffmpeg-windows:",
        "asr-prepare-windows:",
        "app-build-windows-dev:",
        "app-build-windows-release:",
        "runtime-resource-publish-windows:",
    ] {
        assert!(makefile.contains(target), "Makefile 缺少 {target}");
    }
}

#[test]
fn windows_deepseek_key_uses_credential_manager_instead_of_sqlite_or_files() {
    let cargo = read("src-tauri/Cargo.toml");
    let credentials = read("src-tauri/src/ai/llm.rs");
    let repository = read("src-tauri/src/ai/repository.rs");
    for required in [
        "Win32_Security_Credentials",
        "CredReadW",
        "CredWriteW",
        "CredDeleteW",
        "CredFree",
        "CRED_PERSIST_LOCAL_MACHINE",
    ] {
        assert!(
            cargo.contains(required) || credentials.contains(required),
            "Windows 凭据适配器缺少 {required}"
        );
    }
    assert!(!credentials.contains("rusqlite"));
    assert!(!repository.contains("api_key TEXT"));
    assert!(!repository.contains("apiKey TEXT"));
}

#[test]
fn native_platform_release_validation_pins_inputs_and_requires_real_media_execution() {
    let workflow = read(".github/workflows/platform-release-validation.yml");
    let macos = read("scripts/verify-platform-release-macos.sh");
    let windows = read("scripts/verify-platform-release-windows.ps1");
    for required in [
        "macos-15-intel",
        "windows-2022",
        "464beb5e7bf0c311e68b45ae2f04e9cc2af88851abb4082231742a74d97b524c",
        "279af4ce60dbf397362868f3bacc75b56a4332ac2541cae155070093f6aaf0e3",
        "ae85e4a935d7a567bd102fe55afc16bb595bdb618e11b2fc7591bc08120411bb",
        "2aa269b785eeb53a82983a20501ddf7c1d9c48e33ab63a41391ac6c9f7fb6987",
        "cc0ff0eb1dc3f5188ae6300faef32bf5beeba4bdd6e8e445a9184072096b713b",
        "download.visualstudio.microsoft.com/download/pr/9d270333-8b7b-4f96-9458-6fcdb2ec0b25/CC0FF0EB1DC3F5188AE6300FAEF32BF5BEEBA4BDD6E8E445A9184072096B713B/VC_redist.x64.exe",
        "cargo fetch --locked",
        "DY_SCREEN_REQUIRE_CLIP_PLATFORM_VALIDATION=1",
        "DY_SCREEN_REQUIRE_CLIP_PLATFORM_VALIDATION = \"1\"",
        "verify-platform-release-macos.sh",
        "verify-platform-release-windows.ps1",
    ] {
        assert!(
            workflow.contains(required) || macos.contains(required) || windows.contains(required),
            "原生发行验收缺少 {required}"
        );
    }
    assert!(macos.contains("$(uname -m)\" != x86_64"));
    assert!(macos.contains("sysctl.proc_translated"));
    assert!(macos.contains("CFBundleExecutable"));
    assert!(macos.contains("plutil -extract"));
    assert!(windows.contains("OSArchitecture.ToString().ToLowerInvariant() -ne \"x64\""));
    assert!(windows.contains("/D=$installDirectoryAbsolute"));
    assert!(windows.contains("Wait-PathRemoved"));
    assert!(windows.contains("Start-Sleep -Milliseconds 250"));
    assert!(workflow.contains("(.steps | length) == 7"));
    assert!(workflow.contains("@($report.steps).Count -ne 8"));
    assert!(workflow.contains(
        "--silent --show-error --fail --location --retry 5 --retry-all-errors"
    ));
    assert!(workflow.contains("$ErrorActionPreference = \"Continue\""));
    assert!(workflow.contains("锁定发行输入下载失败::$Name (curl exit $curlExitCode)"));
    assert!(!workflow.contains("shell: powershell"));
    assert_eq!(workflow.matches("shell: pwsh").count(), 5);
    assert!(macos.contains("::error title=macOS Intel 原生验收失败"));
    assert!(windows.contains("::error title=Windows x64 原生验收失败"));
    assert!(workflow.contains("::error title=Windows x64 资源构建失败::$Name"));
    for stage in [
        "root-cargo-fetch",
        "tauri-cargo-fetch",
        "visual-studio-environment",
        "ffmpeg-build",
        "whisper-build",
        "resource-assembly",
        "asr-bundle-stage",
    ] {
        assert!(workflow.contains(stage), "Windows CI 缺少诊断阶段 {stage}");
    }
}
