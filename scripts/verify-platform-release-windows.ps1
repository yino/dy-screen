# 在原生 Windows x64 安装开发 NSIS，并验证安装目录内的 ASR、预览和导出资源。
# 本脚本只覆盖 OpenSpec 8.4 平台能力，不替代正式 Authenticode、SmartScreen 或升级验收。
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$Installer,
    [Parameter(Mandatory = $true)]
    [string]$InstallDirectory,
    [Parameter(Mandatory = $true)]
    [string]$Output,
    [string]$RepoRoot = (Get-Location).Path,
    [string]$Cargo = "cargo.exe"
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

function Assert-X64Pe {
    param([string]$Path, [string]$Label)
    Assert-RegularFile -Path $Path -Label $Label
    $stream = [IO.File]::Open($Path, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
    try {
        $reader = [IO.BinaryReader]::new($stream)
        if ($reader.ReadUInt16() -ne 0x5A4D) { throw "$Label 不是 PE 文件。" }
        $stream.Position = 0x3C
        $peOffset = $reader.ReadUInt32()
        if ($peOffset -lt 0x40 -or $peOffset -gt ($stream.Length - 6)) { throw "$Label 的 PE 头无效。" }
        $stream.Position = $peOffset
        if ($reader.ReadUInt32() -ne 0x00004550 -or $reader.ReadUInt16() -ne 0x8664) {
            throw "$Label 不是 Windows x64 PE 文件。"
        }
    }
    finally {
        $stream.Dispose()
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
    if (-not $process.Start()) { throw "无法启动平台发行验收进程。" }
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

function Invoke-EvidenceStep {
    param(
        [string]$Name,
        [string]$FileName,
        [string[]]$Arguments,
        [string]$WorkingDirectory,
        [hashtable]$Environment = @{}
    )
    $result = Invoke-CapturedProcess -FileName $FileName -Arguments $Arguments `
        -WorkingDirectory $WorkingDirectory -Environment $Environment
    $combined = $result.Stdout + $result.Stderr
    [IO.File]::WriteAllText(
        (Join-Path $script:LogRoot ($Name + ".log")),
        $combined,
        [Text.UTF8Encoding]::new($false)
    )
    $script:Steps.Add([ordered]@{
        name = $Name
        exitCode = $result.ExitCode
        wallMs = $result.WallMs
        outputSha256 = Get-TextSha256 $combined
    })
    if ($result.ExitCode -ne 0) { throw "Windows 平台发行验收步骤失败：$Name。" }
    return $result
}

function Write-AtomicJson {
    param([string]$Path, [object]$Value)
    if (Test-Path -LiteralPath $Path) { throw "平台发行证据已经存在，不会覆盖。" }
    $absolute = [IO.Path]::GetFullPath($Path)
    $parent = [IO.Path]::GetDirectoryName($absolute)
    [IO.Directory]::CreateDirectory($parent) | Out-Null
    $temporary = [IO.Path]::Combine($parent, "." + [IO.Path]::GetFileName($absolute) + "." + $PID + ".part")
    try {
        $bytes = [Text.UTF8Encoding]::new($false).GetBytes(
            (($Value | ConvertTo-Json -Depth 10) + [Environment]::NewLine)
        )
        $stream = [IO.FileStream]::new($temporary, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
        try {
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

function Wait-PathRemoved {
    param([string]$Path, [int]$TimeoutSeconds = 30)
    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    while (Test-Path -LiteralPath $Path) {
        if ([DateTime]::UtcNow -ge $deadline) { return $false }
        Start-Sleep -Milliseconds 250
    }
    return $true
}

if (-not [Runtime.InteropServices.RuntimeInformation]::IsOSPlatform(
    [Runtime.InteropServices.OSPlatform]::Windows
) -or [Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString().ToLowerInvariant() -ne "x64") {
    throw "Windows 平台发行验收只能在原生 Windows x64 执行。"
}

$repo = [IO.Path]::GetFullPath($RepoRoot)
$installerAbsolute = [IO.Path]::GetFullPath($Installer)
$installDirectoryAbsolute = [IO.Path]::GetFullPath($InstallDirectory)
$outputAbsolute = [IO.Path]::GetFullPath($Output)
Assert-RegularFile -Path $installerAbsolute -Label "NSIS 安装器"
if (Test-Path -LiteralPath $installDirectoryAbsolute) { throw "安装目录必须在验收前不存在。" }
if (Test-Path -LiteralPath $outputAbsolute) { throw "平台发行证据已经存在。" }

$script:LogRoot = Join-Path ([IO.Path]::GetDirectoryName($outputAbsolute)) `
    ([IO.Path]::GetFileNameWithoutExtension($outputAbsolute) + "-logs")
if (Test-Path -LiteralPath $script:LogRoot) { throw "平台发行日志目录已经存在。" }
[IO.Directory]::CreateDirectory($script:LogRoot) | Out-Null
$script:Steps = [Collections.Generic.List[object]]::new()
$installerSha256 = Get-Sha256 $installerAbsolute
$uninstallAttempted = $false

try {
    Invoke-EvidenceStep -Name "install" -FileName $installerAbsolute -WorkingDirectory ([IO.Path]::GetDirectoryName($installerAbsolute)) `
        -Arguments @("/S", "/D=$installDirectoryAbsolute") | Out-Null

    $appBinary = Join-Path $installDirectoryAbsolute "dy-screen-app.exe"
    $resourceRoot = Join-Path $installDirectoryAbsolute "resources\asr"
    $uninstaller = Join-Path $installDirectoryAbsolute "uninstall.exe"
    foreach ($entry in @(
        @{ Path = $appBinary; Label = "已安装主程序" },
        @{ Path = (Join-Path $resourceRoot "bin\windows-x86_64\ffmpeg.exe"); Label = "已安装 FFmpeg" },
        @{ Path = (Join-Path $resourceRoot "bin\windows-x86_64\ffprobe.exe"); Label = "已安装 FFprobe" },
        @{ Path = (Join-Path $resourceRoot "bin\windows-x86_64\whisper-cli.exe"); Label = "已安装 Whisper" },
        @{ Path = (Join-Path $resourceRoot "bin\windows-x86_64\vad-speech-segments.exe"); Label = "已安装 VAD" }
    )) {
        Assert-X64Pe -Path $entry.Path -Label $entry.Label
    }
    Assert-RegularFile -Path $uninstaller -Label "卸载器"
    $manifestPath = Join-Path $resourceRoot "manifest.json"
    $runtimeManifestPath = Join-Path $resourceRoot "runtime-manifest.json"
    Assert-RegularFile -Path $manifestPath -Label "已安装资源 manifest"
    Assert-RegularFile -Path $runtimeManifestPath -Label "已安装 runtime manifest"
    $manifest = Get-Content -LiteralPath $manifestPath -Raw -Encoding UTF8 | ConvertFrom-Json
    $platforms = @($manifest.platforms | Where-Object { $_.os -eq "windows" -and $_.arch -eq "x86_64" })
    if ($manifest.platforms.Count -ne 1 -or $platforms.Count -ne 1) { throw "安装包不是单平台 Windows x64 资源包。" }
    $platform = $platforms[0]
    $ffmpeg = Join-Path $resourceRoot ([string]$platform.ffmpeg)
    $ffprobe = Join-Path $resourceRoot ([string]$platform.ffprobe)
    $whisper = Join-Path $resourceRoot ([string]$platform.sidecar)
    $vadSidecar = Join-Path $resourceRoot ([string]$platform.vadSidecar)
    $model = Join-Path $resourceRoot ([string]$manifest.model.file)
    $vadModel = Join-Path $resourceRoot ([string]$manifest.vad.file)

    Invoke-EvidenceStep -Name "resource-integrity" -FileName $Cargo -WorkingDirectory $repo -Arguments @(
        "run", "--offline", "--bin", "asr-bundle", "--", "verify",
        "--root", $resourceRoot, "--platform", "windows-x86-64"
    ) | Out-Null
    Invoke-EvidenceStep -Name "ffmpeg-capabilities" -FileName "powershell.exe" -WorkingDirectory $repo -Arguments @(
        "-NoProfile", "-NonInteractive", "-File",
        (Join-Path $repo "scripts\verify-clip-ffmpeg-capabilities.ps1"), "-Ffmpeg", $ffmpeg
    ) | Out-Null
    $asrEnvironment = @{ ASR_RESOURCE_ROOT = $resourceRoot }
    Invoke-EvidenceStep -Name "vad" -FileName $Cargo -WorkingDirectory $repo -Environment $asrEnvironment -Arguments @(
        "test", "--offline", "--test", "asr_vad_integration",
        "real_silero_vad_detects_speech_and_rejects_silence_and_music",
        "--", "--ignored", "--exact", "--nocapture"
    ) | Out-Null
    Invoke-EvidenceStep -Name "whisper" -FileName $Cargo -WorkingDirectory $repo -Environment $asrEnvironment -Arguments @(
        "test", "--offline", "--test", "asr_whisper_integration",
        "real_whisper_engine_transcribes_fixture_and_exits_cleanly",
        "--", "--ignored", "--exact", "--nocapture"
    ) | Out-Null
    $clipEnvironment = @{
        DY_SCREEN_REQUIRE_CLIP_PLATFORM_VALIDATION = "1"
        DY_SCREEN_CLIP_FFMPEG = $ffmpeg
        DY_SCREEN_CLIP_FFPROBE = $ffprobe
    }
    Invoke-EvidenceStep -Name "preview" -FileName $Cargo -WorkingDirectory $repo -Environment $clipEnvironment -Arguments @(
        "test", "--offline", "--manifest-path", "src-tauri/Cargo.toml", "--lib",
        "transition_assets::tests::real_h264_and_hevc_samples_keep_audio_and_generate_compatible_preview",
        "--", "--exact", "--nocapture"
    ) | Out-Null
    Invoke-EvidenceStep -Name "export" -FileName $Cargo -WorkingDirectory $repo -Environment $clipEnvironment -Arguments @(
        "test", "--offline", "--manifest-path", "src-tauri/Cargo.toml", "--lib",
        "ai::clip_export::tests::exports_mixed_source_dimensions_to_a_playable_mp4",
        "--", "--exact", "--nocapture"
    ) | Out-Null

    $appSha256 = Get-Sha256 $appBinary
    $resourceManifestSha256 = Get-Sha256 $manifestPath
    $runtimeManifestSha256 = Get-Sha256 $runtimeManifestPath
    $ffmpegSha256 = Get-Sha256 $ffmpeg
    $ffprobeSha256 = Get-Sha256 $ffprobe
    $whisperSha256 = Get-Sha256 $whisper
    $vadSidecarSha256 = Get-Sha256 $vadSidecar
    $modelSha256 = Get-Sha256 $model
    $vadModelSha256 = Get-Sha256 $vadModel
    if ($modelSha256 -ne [string]$manifest.model.sha256 -or $vadModelSha256 -ne [string]$manifest.vad.sha256) {
        throw "已安装模型哈希与 manifest 不一致。"
    }

    $uninstallAttempted = $true
    Invoke-EvidenceStep -Name "uninstall" -FileName $uninstaller -WorkingDirectory $installDirectoryAbsolute `
        -Arguments @("/S") | Out-Null
    if (-not (Wait-PathRemoved -Path $installDirectoryAbsolute)) {
        throw "NSIS 卸载后安装目录仍存在。"
    }

    $baseConfig = Get-Content -LiteralPath (Join-Path $repo "src-tauri\tauri.conf.json") -Raw -Encoding UTF8 | ConvertFrom-Json
    $report = [ordered]@{
        schemaVersion = 1
        collectedAtUtc = [DateTime]::UtcNow.ToString("yyyy-MM-ddTHH:mm:ssZ")
        platform = "windows"
        architecture = "x64"
        nativeHost = $true
        osVersion = [Environment]::OSVersion.VersionString
        appVersion = [string]$baseConfig.version
        bundleVersion = [string]$manifest.bundleVersion
        installerSha256 = $installerSha256
        appBinarySha256 = $appSha256
        resourceManifestSha256 = $resourceManifestSha256
        runtimeManifestSha256 = $runtimeManifestSha256
        ffmpegSha256 = $ffmpegSha256
        ffprobeSha256 = $ffprobeSha256
        whisperSha256 = $whisperSha256
        vadSidecarSha256 = $vadSidecarSha256
        modelSha256 = $modelSha256
        vadModelSha256 = $vadModelSha256
        steps = @($script:Steps)
        allPassed = $true
    }
    Write-AtomicJson -Path $outputAbsolute -Value $report
    Write-Host "Windows x64 原生平台发行验收通过。"
}
finally {
    if (-not $uninstallAttempted -and (Test-Path -LiteralPath (Join-Path $installDirectoryAbsolute "uninstall.exe"))) {
        try { & (Join-Path $installDirectoryAbsolute "uninstall.exe") /S | Out-Null } catch {}
    }
}
