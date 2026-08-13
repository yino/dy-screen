#![cfg(target_os = "windows")]

use std::path::PathBuf;
use std::process::Command;

/// 使用 Windows 自带 PowerShell AST 解析器校验脚本语法，不执行脚本主体。
///
/// 这比在非 Windows 平台做括号计数更可靠，也确保首版不要求额外安装 PowerShell 7。
#[test]
fn windows_release_and_evidence_scripts_parse_with_builtin_powershell() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let parser = r#"
$tokens = $null
$errors = $null
[System.Management.Automation.Language.Parser]::ParseFile(
    $args[0],
    [ref]$tokens,
    [ref]$errors
) | Out-Null
if ($errors.Count -gt 0) {
    $errors | ForEach-Object { [Console]::Error.WriteLine($_.Message) }
    exit 1
}
"#;
    for relative in [
        "scripts/windows-build-doctor.ps1",
        "scripts/build-asr-ffmpeg-windows.ps1",
        "scripts/build-asr-whisper-windows.ps1",
        "scripts/prepare-asr-resources-windows.ps1",
        "scripts/sign-windows-resource-binaries.ps1",
        "scripts/build-windows-installer.ps1",
        "scripts/collect-asr-performance-windows.ps1",
        "scripts/test-asr-windows-target.ps1",
        "scripts/verify-platform-release-windows.ps1",
        "scripts/verify-asr-release-windows.ps1",
    ] {
        let output = Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command", parser])
            .arg(root.join(relative))
            .output()
            .expect("Windows 必须提供内置 powershell.exe");
        assert!(
            output.status.success(),
            "PowerShell 脚本 {relative} 语法无效：{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn windows_doctor_reports_missing_tools_as_safe_structured_failure() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let output = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-File"])
        .arg(root.join("scripts/windows-build-doctor.ps1"))
        .arg("-Json")
        .env("PATH", "")
        .env_remove("MSYS2_ROOT")
        .output()
        .expect("Windows 必须提供内置 powershell.exe");
    assert_eq!(output.status.code(), Some(2));
    let stdout = String::from_utf8(output.stdout).expect("doctor JSON 使用 UTF-8");
    let report: serde_json::Value = serde_json::from_str(&stdout).expect("doctor 输出 JSON");
    assert_eq!(report["ready"], false);
    assert_eq!(report["platform"], "windows-x86-64");
    assert!(!stdout.contains("C:\\Users\\"));
    assert!(!stdout.to_ascii_lowercase().contains("password"));
}
