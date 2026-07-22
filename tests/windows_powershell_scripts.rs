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
        "scripts/build-asr-whisper-windows.ps1",
        "scripts/collect-asr-performance-windows.ps1",
        "scripts/test-asr-windows-target.ps1",
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
