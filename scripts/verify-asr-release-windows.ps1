# 在真实 Windows x64 目标机安装、验证并卸载正式 NSIS 包，生成脱敏 JSON 证据。
# SmartScreen 声誉界面无法可靠自动化，因此必须传入可追溯的人工证据 ID。
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$Installer,
    [Parameter(Mandatory = $true)]
    [string]$InstallDirectory,
    [string]$InstalledAppRelative = "dy-screen-app.exe",
    [string]$ResourceRootRelative = "resources\asr",
    [string]$UninstallerRelative = "uninstall.exe",
    [Parameter(Mandatory = $true)]
    [string]$AsrCli,
    [Parameter(Mandatory = $true)]
    [string]$AsrBundle,
    [Parameter(Mandatory = $true)]
    [string]$Video,
    [Parameter(Mandatory = $true)]
    [string]$ExpectedSignerThumbprint,
    [Parameter(Mandatory = $true)]
    [string]$SmartScreenEvidenceId,
    [Parameter(Mandatory = $true)]
    [string]$Output
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

function Assert-RegularFile {
    param([string]$Path, [string]$Label)
    $item = Get-Item -LiteralPath $Path -Force -ErrorAction Stop
    if ($item.PSIsContainer -or (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0)) {
        throw "$Label 必须是普通文件，不能是目录或重解析点。"
    }
}

function Get-Sha256 {
    param([string]$Path)
    return (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
}

function Get-TextSha256 {
    param([string]$Text)
    $bytes = [Text.Encoding]::UTF8.GetBytes($Text)
    $sha = [Security.Cryptography.SHA256]::Create()
    try {
        return ([BitConverter]::ToString($sha.ComputeHash($bytes))).Replace("-", "").ToLowerInvariant()
    }
    finally {
        $sha.Dispose()
    }
}

function ConvertTo-WindowsCommandLineArgument {
    param([string]$Value)
    if ($Value.Length -gt 0 -and $Value -notmatch '[\s"]') { return $Value }
    $builder = [Text.StringBuilder]::new()
    [void]$builder.Append('"')
    $backslashes = 0
    foreach ($character in $Value.ToCharArray()) {
        if ($character -eq '\') {
            $backslashes++
            continue
        }
        if ($character -eq '"') {
            [void]$builder.Append(('\' * (($backslashes * 2) + 1)))
            [void]$builder.Append('"')
            $backslashes = 0
            continue
        }
        if ($backslashes -gt 0) {
            [void]$builder.Append(('\' * $backslashes))
            $backslashes = 0
        }
        [void]$builder.Append($character)
    }
    if ($backslashes -gt 0) { [void]$builder.Append(('\' * ($backslashes * 2))) }
    [void]$builder.Append('"')
    return $builder.ToString()
}

function Invoke-CapturedProcess {
    param(
        [string]$FileName,
        [string[]]$Arguments,
        [string]$WorkingDirectory,
        [hashtable]$Environment = @{}
    )
    $startInfo = [Diagnostics.ProcessStartInfo]::new()
    $startInfo.FileName = $FileName
    $startInfo.Arguments = (($Arguments | ForEach-Object { ConvertTo-WindowsCommandLineArgument $_ }) -join " ")
    $startInfo.WorkingDirectory = $WorkingDirectory
    $startInfo.UseShellExecute = $false
    $startInfo.CreateNoWindow = $true
    $startInfo.RedirectStandardOutput = $true
    $startInfo.RedirectStandardError = $true
    $startInfo.StandardOutputEncoding = [Text.Encoding]::UTF8
    $startInfo.StandardErrorEncoding = [Text.Encoding]::UTF8
    foreach ($entry in $Environment.GetEnumerator()) {
        $startInfo.EnvironmentVariables[[string]$entry.Key] = [string]$entry.Value
    }
    $process = [Diagnostics.Process]::new()
    $process.StartInfo = $startInfo
    $stopwatch = [Diagnostics.Stopwatch]::StartNew()
    if (-not $process.Start()) { throw "无法启动发行验收进程。" }
    $stdoutTask = $process.StandardOutput.ReadToEndAsync()
    $stderrTask = $process.StandardError.ReadToEndAsync()
    $process.WaitForExit()
    $stopwatch.Stop()
    $result = [pscustomobject]@{
        ExitCode = $process.ExitCode
        WallMs = [int64]$stopwatch.ElapsedMilliseconds
        Stdout = $stdoutTask.GetAwaiter().GetResult()
        Stderr = $stderrTask.GetAwaiter().GetResult()
    }
    $process.Dispose()
    return $result
}

function Test-Signature {
    param([string]$Path, [string]$ExpectedThumbprint, [bool]$RequireExpectedSigner)
    try {
        $signature = Get-AuthenticodeSignature -LiteralPath $Path
        if ($signature.Status -ne [System.Management.Automation.SignatureStatus]::Valid) { return $false }
        if ($null -eq $signature.SignerCertificate -or $null -eq $signature.TimeStamperCertificate) {
            return $false
        }
        if ($RequireExpectedSigner) {
            return $signature.SignerCertificate.Thumbprint -eq $ExpectedThumbprint
        }
        return $true
    }
    catch {
        return $false
    }
}

function Test-OffLine {
    try {
        $active = @(Get-NetAdapter -ErrorAction Stop | Where-Object {
            $_.Status -eq "Up" -and $_.InterfaceDescription -notmatch "Loopback"
        })
        return $active.Count -eq 0
    }
    catch {
        return $false
    }
}

function Write-AtomicJson {
    param([string]$Path, [object]$Value)
    if (Test-Path -LiteralPath $Path) {
        throw "Windows 发行证据已存在；为保留不可变证据不会覆盖。"
    }
    $absolute = [IO.Path]::GetFullPath($Path)
    $parent = [IO.Path]::GetDirectoryName($absolute)
    [IO.Directory]::CreateDirectory($parent) | Out-Null
    $temporary = [IO.Path]::Combine($parent, "." + [IO.Path]::GetFileName($absolute) + "." + $PID + ".part")
    try {
        $json = $Value | ConvertTo-Json -Depth 8
        $stream = [IO.FileStream]::new(
            $temporary,
            [IO.FileMode]::CreateNew,
            [IO.FileAccess]::Write,
            [IO.FileShare]::None
        )
        try {
            $bytes = [Text.UTF8Encoding]::new($false).GetBytes($json + [Environment]::NewLine)
            $stream.Write($bytes, 0, $bytes.Length)
            $stream.Flush($true)
        }
        finally {
            $stream.Dispose()
        }
        [IO.File]::Move($temporary, $absolute)
    }
    catch {
        if (Test-Path -LiteralPath $temporary) { Remove-Item -LiteralPath $temporary -Force }
        throw
    }
}

if (-not [Runtime.InteropServices.RuntimeInformation]::IsOSPlatform(
    [Runtime.InteropServices.OSPlatform]::Windows
)) {
    throw "该验收脚本只能在 Windows x64 目标机执行。"
}
$architecture = [Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString().ToLowerInvariant()
if ($architecture -ne "x64") { throw "该验收脚本只能在 Windows x64 目标机执行。" }
if ($ExpectedSignerThumbprint -notmatch '^[0-9A-Fa-f]{40,64}$') {
    throw "ExpectedSignerThumbprint 格式无效。"
}
$expectedThumbprint = $ExpectedSignerThumbprint.ToUpperInvariant()
if ($SmartScreenEvidenceId -notmatch '^[A-Za-z0-9][A-Za-z0-9._-]{2,127}$' -or
    $SmartScreenEvidenceId.ToLowerInvariant().StartsWith("replace-")) {
    throw "SmartScreenEvidenceId 必须是非占位的安全证据编号。"
}

$installerAbsolute = [IO.Path]::GetFullPath($Installer)
$installDirectoryAbsolute = [IO.Path]::GetFullPath($InstallDirectory)
$outputAbsolute = [IO.Path]::GetFullPath($Output)
$asrCliAbsolute = [IO.Path]::GetFullPath($AsrCli)
$asrBundleAbsolute = [IO.Path]::GetFullPath($AsrBundle)
$videoAbsolute = [IO.Path]::GetFullPath($Video)
Assert-RegularFile -Path $installerAbsolute -Label "NSIS 安装器"
Assert-RegularFile -Path $asrCliAbsolute -Label "ASR 阶段 CLI"
Assert-RegularFile -Path $asrBundleAbsolute -Label "ASR 资源校验器"
Assert-RegularFile -Path $videoAbsolute -Label "固定中文视频"
if (Test-Path -LiteralPath $Output) { throw "Windows 发行证据已存在。" }
if (Test-Path -LiteralPath $installDirectoryAbsolute) {
    throw "安装目录必须在验收开始前不存在，以证明安装流程。"
}
$installPrefix = $installDirectoryAbsolute.TrimEnd('\') + '\'
if ($outputAbsolute.StartsWith($installPrefix, [StringComparison]::OrdinalIgnoreCase)) {
    throw "发行证据不能写入即将卸载的安装目录。"
}

$installerSha256 = Get-Sha256 $installerAbsolute
$installerSignatureValid = Test-Signature $installerAbsolute $expectedThumbprint $true
$offlineObserved = Test-OffLine
$unicodeUserProfile = $env:USERPROFILE -match '[^\x00-\x7F]'
$sourceSizeBefore = [int64](Get-Item -LiteralPath $videoAbsolute).Length
$sourceShaBefore = Get-Sha256 $videoAbsolute

$install = Invoke-CapturedProcess -FileName $installerAbsolute -Arguments @("/S") -WorkingDirectory ([IO.Path]::GetDirectoryName($installerAbsolute))
Start-Sleep -Seconds 2
$installedApp = Join-Path $installDirectoryAbsolute $InstalledAppRelative
$resourceRoot = Join-Path $installDirectoryAbsolute $ResourceRootRelative
$uninstaller = Join-Path $installDirectoryAbsolute $UninstallerRelative
Assert-RegularFile -Path $installedApp -Label "已安装主程序"
Assert-RegularFile -Path (Join-Path $resourceRoot "manifest.json") -Label "已安装资源 manifest"
Assert-RegularFile -Path $uninstaller -Label "卸载器"

$manifestPath = Join-Path $resourceRoot "manifest.json"
$manifest = Get-Content -LiteralPath $manifestPath -Raw -Encoding UTF8 | ConvertFrom-Json
$platform = @($manifest.platforms | Where-Object { $_.os -eq "windows" -and $_.arch -eq "x86_64" })[0]
$runtimeFile = Join-Path $resourceRoot ([string]$platform.runtimeFile)
Assert-RegularFile -Path $runtimeFile -Label "已安装 VC++ x64 运行库"
$resourceFfmpeg = Join-Path $resourceRoot ([string]$platform.ffmpeg)
$ffmpegClipCapabilitiesValid = $true
try {
    & (Join-Path $PSScriptRoot "verify-clip-ffmpeg-capabilities.ps1") -Ffmpeg $resourceFfmpeg | Out-Null
}
catch {
    $ffmpegClipCapabilitiesValid = $false
}

$signedFiles = [Collections.Generic.List[string]]::new()
$signedFiles.Add($installedApp)
$signedFiles.Add($uninstaller)
foreach ($relative in @($platform.sidecar, $platform.vadSidecar, $platform.ffmpeg, $platform.ffprobe)) {
    $signedFiles.Add((Join-Path $resourceRoot ([string]$relative)))
}
foreach ($relative in @($platform.libraries)) {
    $signedFiles.Add((Join-Path $resourceRoot ([string]$relative)))
}
$signedFileCount = 0
$validSignedFileCount = 0
foreach ($file in $signedFiles) {
    Assert-RegularFile -Path $file -Label "已安装签名二进制"
    $signedFileCount++
    if (Test-Signature $file $expectedThumbprint $true) { $validSignedFileCount++ }
}
$allInstalledBinariesSigned = $signedFileCount -gt 0 -and $signedFileCount -eq $validSignedFileCount
$runtimeSignatureValid = Test-Signature $runtimeFile $expectedThumbprint $false

$bundle = Invoke-CapturedProcess -FileName $asrBundleAbsolute -WorkingDirectory ([IO.Path]::GetDirectoryName($asrBundleAbsolute)) -Arguments @(
    "verify", "--root", $resourceRoot, "--platform", "windows-x86-64"
)
$resourceBundleValid = $bundle.ExitCode -eq 0

$vcRuntimeInstalled = $false
foreach ($registryPath in @(
    "HKLM:\SOFTWARE\Microsoft\VisualStudio\14.0\VC\Runtimes\x64",
    "HKLM:\SOFTWARE\WOW6432Node\Microsoft\VisualStudio\14.0\VC\Runtimes\x64"
)) {
    try {
        if ((Get-ItemProperty -LiteralPath $registryPath -ErrorAction Stop).Installed -eq 1) {
            $vcRuntimeInstalled = $true
        }
    }
    catch {}
}
$webView2Installed = $false
foreach ($registryPath in @(
    "HKLM:\SOFTWARE\Microsoft\EdgeUpdate\Clients\{F1E7E7E5-CE13-4A7D-9F5E-6A7E1B7E9E0B}",
    "HKCU:\SOFTWARE\Microsoft\EdgeUpdate\Clients\{F1E7E7E5-CE13-4A7D-9F5E-6A7E1B7E9E0B}"
)) {
    if (Test-Path -LiteralPath $registryPath) { $webView2Installed = $true }
}
# Edge WebView2 的稳定产品 GUID 也可能由系统级 Evergreen Runtime 注册。
if (-not $webView2Installed) {
    foreach ($clientsRoot in @(
        "HKLM:\SOFTWARE\Microsoft\EdgeUpdate\Clients",
        "HKCU:\SOFTWARE\Microsoft\EdgeUpdate\Clients"
    )) {
        if (@(Get-ChildItem $clientsRoot -ErrorAction SilentlyContinue |
            ForEach-Object { Get-ItemProperty $_.PSPath -ErrorAction SilentlyContinue } |
            Where-Object { $_.name -match "WebView2" }).Count -gt 0) {
            $webView2Installed = $true
        }
    }
}

$appProcess = Start-Process -FilePath $installedApp -PassThru
Start-Sleep -Seconds 3
$appLaunchPassed = -not $appProcess.HasExited
if (-not $appProcess.HasExited) {
    Stop-Process -Id $appProcess.Id -Force
    $appProcess.WaitForExit()
}
$appProcess.Dispose()

$unicodeDirectory = Join-Path $env:USERPROFILE ("ASR 验收 空格 " + [Guid]::NewGuid().ToString("N"))
[IO.Directory]::CreateDirectory($unicodeDirectory) | Out-Null
$unicodeVideo = Join-Path $unicodeDirectory "测试 视频.mp4"
Copy-Item -LiteralPath $videoAbsolute -Destination $unicodeVideo -Force
$cli = Invoke-CapturedProcess -FileName $asrCliAbsolute -WorkingDirectory ([IO.Path]::GetDirectoryName($asrCliAbsolute)) -Arguments @(
    "asr", $unicodeVideo, "--resource-root", $resourceRoot, "--json"
)
$cliJsonValid = $false
$asrExpectedTextPassed = $false
try {
    $cliJson = $cli.Stdout | ConvertFrom-Json -ErrorAction Stop
    $text = [string]$cliJson.text
    $cliJsonValid = [int64]$cliJson.durationMs -gt 0 -and @($cliJson.segments).Count -gt 0
    $asrExpectedTextPassed = $text.Contains("直播间") -and ($text.Contains("99") -or $text.Contains("九十九"))
}
catch {}

$sourceSizeAfter = [int64](Get-Item -LiteralPath $videoAbsolute).Length
$sourceShaAfter = Get-Sha256 $videoAbsolute
$sourceUnchanged = $sourceSizeAfter -eq $sourceSizeBefore -and $sourceShaAfter -eq $sourceShaBefore
$mainBinarySha256 = Get-Sha256 $installedApp
$asrCliSha256 = Get-Sha256 $asrCliAbsolute
$asrBundleVerifierSha256 = Get-Sha256 $asrBundleAbsolute
$manifestSha256 = Get-Sha256 $manifestPath
$runtimeSha256 = Get-Sha256 $runtimeFile

$uninstall = Invoke-CapturedProcess -FileName $uninstaller -Arguments @("/S") -WorkingDirectory $installDirectoryAbsolute
Start-Sleep -Seconds 2
$uninstallRemovedInstallDirectory = -not (Test-Path -LiteralPath $installDirectoryAbsolute)
if (Test-Path -LiteralPath $unicodeDirectory) {
    Remove-Item -LiteralPath $unicodeDirectory -Recurse -Force
}

$allPassed = $installerSignatureValid -and
    $install.ExitCode -eq 0 -and
    $allInstalledBinariesSigned -and
    $runtimeSignatureValid -and
    $resourceBundleValid -and
    $ffmpegClipCapabilitiesValid -and
    $vcRuntimeInstalled -and
    $webView2Installed -and
    $offlineObserved -and
    $unicodeUserProfile -and
    $appLaunchPassed -and
    $cli.ExitCode -eq 0 -and
    $cliJsonValid -and
    $asrExpectedTextPassed -and
    $sourceUnchanged -and
    $uninstall.ExitCode -eq 0 -and
    $uninstallRemovedInstallDirectory

$report = [ordered]@{
    schemaVersion = 1
    collectedAtUtc = [DateTime]::UtcNow.ToString("yyyy-MM-ddTHH:mm:ssZ")
    platform = "windows"
    architecture = "x64"
    osVersion = [Environment]::OSVersion.VersionString
    smartScreenEvidenceId = $SmartScreenEvidenceId
    installerSha256 = $installerSha256
    mainBinarySha256 = $mainBinarySha256
    asrCliSha256 = $asrCliSha256
    asrBundleVerifierSha256 = $asrBundleVerifierSha256
    resourceManifestSha256 = $manifestSha256
    runtimeSha256 = $runtimeSha256
    installerSignatureValid = $installerSignatureValid
    signedFileCount = $signedFileCount
    validSignedFileCount = $validSignedFileCount
    allInstalledBinariesSigned = $allInstalledBinariesSigned
    runtimeSignatureValid = $runtimeSignatureValid
    installExitCode = $install.ExitCode
    installWallMs = $install.WallMs
    installOutputSha256 = Get-TextSha256 ($install.Stdout + $install.Stderr)
    resourceBundleValid = $resourceBundleValid
    ffmpegClipCapabilitiesValid = $ffmpegClipCapabilitiesValid
    bundleVerifierOutputSha256 = Get-TextSha256 ($bundle.Stdout + $bundle.Stderr)
    vcRuntimeInstalled = $vcRuntimeInstalled
    webView2Installed = $webView2Installed
    offlineObserved = $offlineObserved
    unicodeUserProfile = $unicodeUserProfile
    appLaunchPassed = $appLaunchPassed
    cliExitCode = $cli.ExitCode
    cliWallMs = $cli.WallMs
    cliJsonValid = $cliJsonValid
    asrExpectedTextPassed = $asrExpectedTextPassed
    cliStdoutSha256 = Get-TextSha256 $cli.Stdout
    cliStderrSha256 = Get-TextSha256 $cli.Stderr
    sourceSizeBytesBefore = $sourceSizeBefore
    sourceSizeBytesAfter = $sourceSizeAfter
    sourceSha256Before = $sourceShaBefore
    sourceSha256After = $sourceShaAfter
    sourceUnchanged = $sourceUnchanged
    uninstallExitCode = $uninstall.ExitCode
    uninstallWallMs = $uninstall.WallMs
    uninstallOutputSha256 = Get-TextSha256 ($uninstall.Stdout + $uninstall.Stderr)
    uninstallRemovedInstallDirectory = $uninstallRemovedInstallDirectory
    allPassed = $allPassed
}
Write-AtomicJson -Path $Output -Value $report
Write-Host "Windows 正式发行验收完成：allPassed=$allPassed"
if (-not $allPassed) { exit 1 }
