# 在原生 Windows x64 构建 Tauri NSIS 开发包或正式发行包。
[CmdletBinding()]
param(
    [string]$RepoRoot = (Get-Location).Path,
    [ValidateSet("Development", "Release")]
    [string]$Mode = "Development",
    [Parameter(Mandatory = $true)]
    [string]$ResourceBaseUrl,
    [string]$CertificateThumbprint,
    [string]$TimestampUrl,
    [string]$Cargo = "cargo.exe",
    [string]$Npm = "npm.cmd"
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

function Invoke-Checked {
    param([string]$FileName, [string[]]$Arguments, [string]$Label)
    & $FileName @Arguments
    if ($LASTEXITCODE -ne 0) { throw "$Label 失败。" }
}

function Assert-ExpectedSignature {
    param([string]$Path, [string]$ExpectedThumbprint, [string]$Label)
    $signature = Get-AuthenticodeSignature -LiteralPath $Path
    if ($signature.Status -ne [System.Management.Automation.SignatureStatus]::Valid -or
        $null -eq $signature.SignerCertificate -or
        $signature.SignerCertificate.Thumbprint -ne $ExpectedThumbprint -or
        $null -eq $signature.TimeStamperCertificate) {
        throw "$Label 的 Authenticode 签名或时间戳无效。"
    }
}

if (-not [Runtime.InteropServices.RuntimeInformation]::IsOSPlatform(
    [Runtime.InteropServices.OSPlatform]::Windows
) -or [Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString().ToLowerInvariant() -ne "x64") {
    throw "Windows NSIS 只能在原生 Windows x64 构建机生成。"
}
$resourceUrl = [Uri]$ResourceBaseUrl
if (-not $resourceUrl.IsAbsoluteUri -or $resourceUrl.Scheme -ne "https" -or
    -not [string]::IsNullOrEmpty($resourceUrl.UserInfo)) {
    throw "资源基础地址必须使用不含凭据的 HTTPS URL。"
}
$repo = [IO.Path]::GetFullPath($RepoRoot)
$stage = Join-Path $repo "resources\asr-stage"
$asrBundleManifest = Join-Path $stage "manifest.json"
$runtimeManifest = Join-Path $stage "runtime-manifest.json"
foreach ($path in @($asrBundleManifest, $runtimeManifest)) {
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { throw "Windows staging 不完整。" }
}

Invoke-Checked -FileName $Cargo -Label "Windows 资源校验" -Arguments @(
    "run", "--offline", "--bin", "asr-bundle", "--", "verify",
    "--root", $stage, "--platform", "windows-x86-64"
)
Invoke-Checked -FileName "powershell.exe" -Label "Windows FFmpeg 剪辑能力校验" -Arguments @(
    "-NoProfile", "-NonInteractive", "-File",
    (Join-Path $repo "scripts\verify-clip-ffmpeg-capabilities.ps1"),
    "-Ffmpeg", (Join-Path $stage "bin\windows-x86_64\ffmpeg.exe")
)

$release = $Mode -eq "Release"
$thumbprint = $null
if ($release) {
    if ($CertificateThumbprint -notmatch '^[0-9A-Fa-f]{40,64}$') { throw "正式构建必须提供有效证书指纹。" }
    $thumbprint = $CertificateThumbprint.ToUpperInvariant()
    if ([string]::IsNullOrWhiteSpace($TimestampUrl)) { throw "正式构建必须提供 HTTPS 时间戳服务。" }
    $timestamp = [Uri]$TimestampUrl
    if (-not $timestamp.IsAbsoluteUri -or $timestamp.Scheme -ne "https" -or
        -not [string]::IsNullOrEmpty($timestamp.UserInfo)) {
        throw "正式构建必须提供不含凭据的 HTTPS 时间戳服务。"
    }
    Invoke-Checked -FileName $Cargo -Label "Runtime manifest 签名校验" -Arguments @(
        "run", "--offline", "--bin", "asr-bundle", "--", "verify-signature", "--root", $stage
    )
    Invoke-Checked -FileName "powershell.exe" -Label "Windows 原生资源签名校验" -Arguments @(
        "-NoProfile", "-NonInteractive", "-File",
        (Join-Path $repo "scripts\sign-windows-resource-binaries.ps1"),
        "-ResourceRoot", $stage,
        "-CertificateThumbprint", $thumbprint,
        "-TimestampUrl", $TimestampUrl,
        "-VerifyOnly"
    )
}

$windowsConfigPath = Join-Path $repo "src-tauri\tauri.windows.conf.json"
$windowsConfig = Get-Content -LiteralPath $windowsConfigPath -Raw -Encoding UTF8 | ConvertFrom-Json
if ($release) {
    foreach ($property in @(
        @{ Name = "certificateThumbprint"; Value = $thumbprint },
        @{ Name = "digestAlgorithm"; Value = "sha256" },
        @{ Name = "timestampUrl"; Value = $TimestampUrl },
        @{ Name = "tsp"; Value = $true }
    )) {
        if ($windowsConfig.bundle.windows.PSObject.Properties.Name -contains $property.Name) {
            $windowsConfig.bundle.windows.($property.Name) = $property.Value
        }
        else {
            $windowsConfig.bundle.windows | Add-Member -NotePropertyName $property.Name -NotePropertyValue $property.Value
        }
    }
}

$temporaryConfig = Join-Path ([IO.Path]::GetTempPath()) ("dy-screen-windows-config-" + [Guid]::NewGuid().ToString("N") + ".json")
$previousResourceUrl = $env:DY_SCREEN_RESOURCE_BASE_URL
$locationPushed = $false
try {
    [IO.File]::WriteAllText(
        $temporaryConfig,
        (($windowsConfig | ConvertTo-Json -Depth 12) + [Environment]::NewLine),
        [Text.UTF8Encoding]::new($false)
    )
    $env:DY_SCREEN_RESOURCE_BASE_URL = $ResourceBaseUrl
    Push-Location $repo
    $locationPushed = $true
    Invoke-Checked -FileName $Npm -Label "Tauri Windows NSIS 构建" -Arguments @(
        "run", "tauri:build", "--", "--config", $temporaryConfig
    )
}
finally {
    if ($locationPushed) { Pop-Location }
    $env:DY_SCREEN_RESOURCE_BASE_URL = $previousResourceUrl
    if (Test-Path -LiteralPath $temporaryConfig) { Remove-Item -LiteralPath $temporaryConfig -Force }
}

$installer = Get-ChildItem -LiteralPath (Join-Path $repo "src-tauri\target\release\bundle\nsis") `
    -Filter "*.exe" -File | Sort-Object LastWriteTimeUtc -Descending | Select-Object -First 1
if ($null -eq $installer) { throw "Tauri 构建完成但没有找到 NSIS 安装器。" }
if ($release) { Assert-ExpectedSignature -Path $installer.FullName -ExpectedThumbprint $thumbprint -Label "NSIS 安装器" }

$baseConfig = Get-Content -LiteralPath (Join-Path $repo "src-tauri\tauri.conf.json") -Raw -Encoding UTF8 | ConvertFrom-Json
$runtime = Get-Content -LiteralPath $runtimeManifest -Raw -Encoding UTF8 | ConvertFrom-Json
$record = [ordered]@{
    schemaVersion = 1
    mode = $Mode.ToLowerInvariant()
    official = $release
    productName = [string]$baseConfig.productName
    appVersion = [string]$baseConfig.version
    platform = "windows-x86-64"
    bundleVersion = [string]$runtime.bundleVersion
    installerFile = $installer.Name
    installerSizeBytes = [int64]$installer.Length
    installerSha256 = (Get-FileHash -LiteralPath $installer.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
    authenticodeValid = $release
    certificateThumbprint = if ($release) { $thumbprint } else { $null }
    builtAtUtc = [DateTime]::UtcNow.ToString("o")
}
$recordRoot = Join-Path $repo "dist\windows"
[IO.Directory]::CreateDirectory($recordRoot) | Out-Null
$recordPath = Join-Path $recordRoot (
    "build-" + $Mode.ToLowerInvariant() + "-" + $baseConfig.version + "-" +
    $record.installerSha256.Substring(0, 12) + ".json"
)
if (Test-Path -LiteralPath $recordPath) {
    Write-Host "相同安装器的不可变 Windows 构建记录已经存在。"
    exit 0
}
[IO.File]::WriteAllText(
    $recordPath,
    (($record | ConvertTo-Json -Depth 5) + [Environment]::NewLine),
    [Text.UTF8Encoding]::new($false)
)
Write-Host "Windows NSIS 构建完成：$($installer.Name)"
