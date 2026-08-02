# 检查“切片智能体”Windows x64 原生资源与 NSIS 构建环境。
[CmdletBinding()]
param(
    [switch]$Json
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

function ConvertTo-SafeVersion {
    param([string]$Value)
    if ([string]::IsNullOrWhiteSpace($Value)) { return "已安装" }
    $trimmed = $Value.Trim()
    if ($trimmed.Length -gt 200 -or $trimmed -match '(?i)(?:[A-Z]:[\\/]|\\\\|/Users/|/home/)') {
        return "已安装（版本输出已脱敏）"
    }
    return $trimmed
}

function Get-CommandVersion {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Command,
        [string[]]$Arguments = @("--version")
    )

    $resolved = Get-Command $Command -ErrorAction SilentlyContinue
    if ($null -eq $resolved) {
        return [pscustomobject]@{ Name = $Command; Ready = $false; Version = $null }
    }
    try {
        if ($Arguments.Count -eq 0) {
            $fileVersion = [Diagnostics.FileVersionInfo]::GetVersionInfo($resolved.Source).FileVersion
            $safeVersion = ConvertTo-SafeVersion $fileVersion
            return [pscustomobject]@{ Name = $Command; Ready = $true; Version = $safeVersion }
        }
        $output = & $resolved.Source @Arguments 2>&1 | Out-String
        $firstLine = @($output -split "`r?`n" | Where-Object { -not [string]::IsNullOrWhiteSpace($_) }) |
            Select-Object -First 1
        $safeVersion = ConvertTo-SafeVersion ([string]$firstLine)
        return [pscustomobject]@{ Name = $Command; Ready = $LASTEXITCODE -eq 0; Version = $safeVersion }
    }
    catch {
        return [pscustomobject]@{ Name = $Command; Ready = $false; Version = $null }
    }
}

$isWindows = [Runtime.InteropServices.RuntimeInformation]::IsOSPlatform(
    [Runtime.InteropServices.OSPlatform]::Windows
)
$architecture = [Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString().ToLowerInvariant()
$processArchitecture = [Runtime.InteropServices.RuntimeInformation]::ProcessArchitecture.ToString().ToLowerInvariant()
$platformReady = $isWindows -and $architecture -eq "x64" -and $processArchitecture -eq "x64"
$powershellReady = $PSVersionTable.PSVersion -ge [Version]"5.1"

$tools = @(
    Get-CommandVersion -Command "rustc.exe"
    Get-CommandVersion -Command "cargo.exe"
    Get-CommandVersion -Command "node.exe"
    Get-CommandVersion -Command "npm.cmd"
    Get-CommandVersion -Command "cmake.exe"
    Get-CommandVersion -Command "cl.exe" -Arguments @()
    Get-CommandVersion -Command "dumpbin.exe" -Arguments @()
    Get-CommandVersion -Command "signtool.exe" -Arguments @("/?")
    Get-CommandVersion -Command "makensis.exe" -Arguments @("/VERSION")
)

$msysRoot = if ([string]::IsNullOrWhiteSpace($env:MSYS2_ROOT)) { "C:\msys64" } else { $env:MSYS2_ROOT }
$msysBash = Join-Path $msysRoot "usr\bin\bash.exe"
$ucrtGcc = Join-Path $msysRoot "ucrt64\bin\gcc.exe"
$msysReady = (Test-Path -LiteralPath $msysBash -PathType Leaf) -and
    (Test-Path -LiteralPath $ucrtGcc -PathType Leaf)
$rustHost = if (($tools | Where-Object { $_.Name -eq "rustc.exe" }).Ready) {
    & rustc.exe -vV 2>&1 | Out-String
} else { "" }
$rustMsvcReady = $LASTEXITCODE -eq 0 -and $rustHost -match "(?m)^host:\s*x86_64-pc-windows-msvc\s*$"
$vsReady = $env:VSCMD_VER -match '^17\.' -and
    ($tools | Where-Object { $_.Name -eq "cl.exe" }).Ready -and
    ($tools | Where-Object { $_.Name -eq "dumpbin.exe" }).Ready
$windowsSdkReady = ($tools | Where-Object { $_.Name -eq "signtool.exe" }).Ready -and
    $env:WindowsSDKVersion -match '^10\.'

$report = [ordered]@{
    schemaVersion = 1
    platform = "windows-x86-64"
    ready = $platformReady -and $powershellReady -and $rustMsvcReady -and $vsReady -and $windowsSdkReady -and
        $msysReady -and -not ($tools | Where-Object { -not $_.Ready })
    checks = [ordered]@{
        windowsX64 = $platformReady
        powershell51 = $powershellReady
        rustMsvc = $rustMsvcReady
        visualStudioDeveloperShell = $vsReady
        windowsSdk = $windowsSdkReady
        msys2Ucrt64 = $msysReady
    }
    tools = @($tools | ForEach-Object {
        [ordered]@{ name = $_.Name; ready = [bool]$_.Ready; version = $_.Version }
    })
}

if ($Json) {
    $report | ConvertTo-Json -Depth 6
}
else {
    Write-Host "Windows x64 构建环境：$($report.ready)"
    foreach ($check in $report.checks.GetEnumerator()) {
        Write-Host "  $($check.Key): $($check.Value)"
    }
    foreach ($tool in $report.tools) {
        $version = if ($null -eq $tool.version) { "不可用" } else { $tool.version }
        Write-Host "  $($tool.name): $version"
    }
}

if (-not $report.ready) {
    exit 2
}
