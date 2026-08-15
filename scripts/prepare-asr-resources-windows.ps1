# 将 Windows 原生产物、公共模型和 Microsoft 运行库组装成新的可信资源源目录。
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$CommonSource,
    [Parameter(Mandatory = $true)]
    [string]$FfmpegRoot,
    [Parameter(Mandatory = $true)]
    [string]$WhisperRoot,
    [Parameter(Mandatory = $true)]
    [string]$VcRedist,
    [Parameter(Mandatory = $true)]
    [string]$VcLicense,
    [Parameter(Mandatory = $true)]
    [string]$OutputRoot,
    [string]$BundleVersion
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$ExpectedFfmpegSha256 = @(
    "464beb5e7bf0c311e68b45ae2f04e9cc2af88851abb4082231742a74d97b524c",
    "9fd092511605bbebafe095ea6d38d9e40f34d12f7386e1258372df8be0576eb7"
)
$ExpectedWhisperSha256 = "279af4ce60dbf397362868f3bacc75b56a4332ac2541cae155070093f6aaf0e3"
$ExpectedVcRedistSha256 = "cc0ff0eb1dc3f5188ae6300faef32bf5beeba4bdd6e8e445a9184072096b713b"

function Assert-RegularFile {
    param([string]$Path, [string]$Label)
    $item = Get-Item -LiteralPath $Path -Force -ErrorAction Stop
    if ($item.PSIsContainer -or (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0)) {
        throw "$Label 必须是普通文件，不能是目录或重解析点。"
    }
}

function Assert-RegularDirectory {
    param([string]$Path, [string]$Label)
    $item = Get-Item -LiteralPath $Path -Force -ErrorAction Stop
    if (-not $item.PSIsContainer -or (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0)) {
        throw "$Label 必须是普通目录，不能是重解析点。"
    }
}

function Assert-X64Pe {
    param([string]$Path, [string]$Label)
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

function Copy-VerifiedFile {
    param([string]$Source, [string]$Target, [string]$Label)
    Assert-RegularFile -Path $Source -Label $Label
    $parent = [IO.Path]::GetDirectoryName($Target)
    [IO.Directory]::CreateDirectory($parent) | Out-Null
    Copy-Item -LiteralPath $Source -Destination $Target
}

function Get-Integrity {
    param([string]$Root, [string]$Relative)
    $path = Join-Path $Root ($Relative.Replace("/", "\"))
    Assert-RegularFile -Path $path -Label "资源 $Relative"
    return [ordered]@{
        file = $Relative
        sizeBytes = [int64](Get-Item -LiteralPath $path).Length
        sha256 = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToLowerInvariant()
    }
}

if (-not [Runtime.InteropServices.RuntimeInformation]::IsOSPlatform(
    [Runtime.InteropServices.OSPlatform]::Windows
) -or [Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString().ToLowerInvariant() -ne "x64") {
    throw "Windows 可信资源只能在原生 Windows x64 环境组装。"
}
Assert-RegularDirectory -Path $CommonSource -Label "公共资源目录"
Assert-RegularDirectory -Path $FfmpegRoot -Label "FFmpeg 构建目录"
Assert-RegularDirectory -Path $WhisperRoot -Label "Whisper 构建目录"
Assert-RegularFile -Path $VcRedist -Label "VC++ x64 运行库"
Assert-RegularFile -Path $VcLicense -Label "VC++ 运行库许可说明"
if (Test-Path -LiteralPath $OutputRoot) { throw "输出目录已经存在，请使用一个新的目录。" }

$vcSha256 = (Get-FileHash -LiteralPath $VcRedist -Algorithm SHA256).Hash.ToLowerInvariant()
if ($vcSha256 -ne $ExpectedVcRedistSha256) {
    throw "VC++ x64 运行库 SHA-256 与锁定版本不一致。"
}
$vcSignature = Get-AuthenticodeSignature -LiteralPath $VcRedist
if ($vcSignature.Status -ne [System.Management.Automation.SignatureStatus]::Valid -or
    $null -eq $vcSignature.SignerCertificate -or
    $vcSignature.SignerCertificate.Subject -notmatch "Microsoft Corporation") {
    throw "VC++ x64 运行库没有有效的 Microsoft Authenticode 签名。"
}
# Microsoft 的 x64 redistributable 使用 x86 bootstrapper 外壳安装 x64 payload，
# 因此不能用 PE machine 字段判断 payload 架构；文件哈希、签名和版本共同固定该输入。
$vcVersion = (Get-Item -LiteralPath $VcRedist).VersionInfo.FileVersion
if ([string]::IsNullOrWhiteSpace($vcVersion)) { throw "无法读取 VC++ x64 运行库版本。" }

$part = "$OutputRoot.part.$([Guid]::NewGuid().ToString('N'))"
$manifestPath = Join-Path $CommonSource "manifest.json"
Assert-RegularFile -Path $manifestPath -Label "公共资源 manifest"
$manifest = Get-Content -LiteralPath $manifestPath -Raw -Encoding UTF8 | ConvertFrom-Json
$windowsPlatforms = @($manifest.platforms | Where-Object { $_.os -eq "windows" -and $_.arch -eq "x86_64" })
if ($windowsPlatforms.Count -ne 1) { throw "公共资源 manifest 必须且只能声明一个 Windows x64 平台。" }
$platform = $windowsPlatforms[0]
if (@($platform.minimumCpuFeatures) -notcontains "sse4.2" -or $platform.accelerator -ne "cpu") {
    throw "Windows 平台 manifest 必须声明 CPU 与 SSE4.2 基线。"
}

$ffmpegBuildRecord = Join-Path $FfmpegRoot "build-record.txt"
$ffmpegVersionRecord = Join-Path $FfmpegRoot "ffmpeg-version.txt"
$ffprobeVersionRecord = Join-Path $FfmpegRoot "ffprobe-version.txt"
$whisperBuildRecord = Join-Path $WhisperRoot "build-record.txt"
$whisperVersionRecord = Join-Path $WhisperRoot "whisper-version.txt"
foreach ($record in @(
    $ffmpegBuildRecord,
    $ffmpegVersionRecord,
    $ffprobeVersionRecord,
    $whisperBuildRecord,
    $whisperVersionRecord
)) {
    Assert-RegularFile -Path $record -Label "Windows 原生资源构建记录"
}
$ffmpegRecordText = Get-Content -LiteralPath $ffmpegBuildRecord -Raw -Encoding UTF8
$ffmpegVersionText = Get-Content -LiteralPath $ffmpegVersionRecord -Raw -Encoding UTF8
$ffprobeVersionText = Get-Content -LiteralPath $ffprobeVersionRecord -Raw -Encoding UTF8
$whisperRecordText = Get-Content -LiteralPath $whisperBuildRecord -Raw -Encoding UTF8
$whisperVersionText = Get-Content -LiteralPath $whisperVersionRecord -Raw -Encoding UTF8
$ffmpegSourceSha256 = @($ExpectedFfmpegSha256 | Where-Object {
    $ffmpegRecordText -match "source_sha256=$_"
}) | Select-Object -First 1
if ([string]::IsNullOrWhiteSpace($ffmpegSourceSha256) -or
    $ffmpegRecordText -notmatch "license=LGPL-2.1-or-later" -or
    $ffmpegRecordText -notmatch "runtime_dependencies=windows-system-only" -or
    $ffmpegVersionText -notmatch "ffmpeg version\s+8\.1\.2" -or
    $ffprobeVersionText -notmatch "ffprobe version\s+8\.1\.2") {
    throw "FFmpeg 构建记录与锁定版本、许可证或依赖边界不一致。"
}
if ($whisperRecordText -notmatch "source_sha256=$ExpectedWhisperSha256" -or
    $whisperRecordText -notmatch "minimum_cpu=sse4\.2" -or
    $whisperRecordText -notmatch "avx=disabled" -or
    $whisperRecordText -notmatch "avx2=disabled" -or
    $whisperRecordText -notmatch "network=disabled" -or
    $whisperVersionText -notmatch "1\.9\.1") {
    throw "Whisper 构建记录与锁定版本、CPU 或离线边界不一致。"
}

$expectedCommonHashes = @{}
$expectedCommonHashes[[string]$manifest.model.file] = [string]$manifest.model.sha256
$expectedCommonHashes[[string]$manifest.vad.file] = [string]$manifest.vad.sha256
$expectedCommonHashes[[string]$manifest.normalization.file] = [string]$manifest.normalization.sha256

try {
    [IO.Directory]::CreateDirectory($part) | Out-Null
    foreach ($entry in @(
        @{ Source = (Join-Path $FfmpegRoot "bin\windows-x86_64\ffmpeg.exe"); Relative = "bin/windows-x86_64/ffmpeg.exe"; Label = "FFmpeg" },
        @{ Source = (Join-Path $FfmpegRoot "bin\windows-x86_64\ffprobe.exe"); Relative = "bin/windows-x86_64/ffprobe.exe"; Label = "FFprobe" },
        @{ Source = (Join-Path $WhisperRoot "bin\windows-x86_64\whisper-cli.exe"); Relative = "bin/windows-x86_64/whisper-cli.exe"; Label = "Whisper" },
        @{ Source = (Join-Path $WhisperRoot "bin\windows-x86_64\vad-speech-segments.exe"); Relative = "bin/windows-x86_64/vad-speech-segments.exe"; Label = "VAD sidecar" }
    )) {
        Assert-X64Pe -Path $entry.Source -Label $entry.Label
        Copy-VerifiedFile -Source $entry.Source -Target (Join-Path $part ($entry.Relative.Replace("/", "\"))) -Label $entry.Label
    }

    foreach ($relative in @(
        [string]$manifest.model.file,
        [string]$manifest.vad.file,
        [string]$manifest.normalization.file
    ) + @($manifest.licenseFiles)) {
        $sourcePath = Join-Path $CommonSource ($relative.Replace("/", "\"))
        if ($expectedCommonHashes.ContainsKey([string]$relative)) {
            $actualHash = (Get-FileHash -LiteralPath $sourcePath -Algorithm SHA256).Hash.ToLowerInvariant()
            if ($actualHash -ne $expectedCommonHashes[[string]$relative]) {
                throw "公共资源 $relative 的 SHA-256 与 manifest 不一致。"
            }
        }
        Copy-VerifiedFile `
            -Source $sourcePath `
            -Target (Join-Path $part ($relative.Replace("/", "\"))) `
            -Label "公共资源 $relative"
    }
    Copy-VerifiedFile -Source $VcRedist `
        -Target (Join-Path $part "runtime\windows-x86_64\vc_redist.x64.exe") `
        -Label "VC++ x64 运行库"
    Copy-VerifiedFile -Source $VcLicense `
        -Target (Join-Path $part "licenses\Microsoft-VCRedist.txt") `
        -Label "VC++ 运行库许可说明"
    foreach ($record in @(
        @{ Source = $ffmpegBuildRecord; Target = "build-records\windows\ffmpeg-build-record.txt" },
        @{ Source = $ffmpegVersionRecord; Target = "build-records\windows\ffmpeg-version.txt" },
        @{ Source = $ffprobeVersionRecord; Target = "build-records\windows\ffprobe-version.txt" },
        @{ Source = $whisperBuildRecord; Target = "build-records\windows\whisper-build-record.txt" },
        @{ Source = $whisperVersionRecord; Target = "build-records\windows\whisper-version.txt" }
    )) {
        Copy-VerifiedFile -Source $record.Source -Target (Join-Path $part $record.Target) -Label "原生资源构建记录"
    }

    if (-not [string]::IsNullOrWhiteSpace($BundleVersion)) { $manifest.bundleVersion = $BundleVersion }
    $licenseFiles = @($manifest.licenseFiles | ForEach-Object { [string]$_ })
    if ($licenseFiles -notcontains "licenses/Microsoft-VCRedist.txt") {
        $licenseFiles += "licenses/Microsoft-VCRedist.txt"
    }
    $manifest.licenseFiles = $licenseFiles
    $platform.resourceIntegrity = @(
        Get-Integrity -Root $part -Relative "bin/windows-x86_64/whisper-cli.exe"
        Get-Integrity -Root $part -Relative "bin/windows-x86_64/vad-speech-segments.exe"
        Get-Integrity -Root $part -Relative "bin/windows-x86_64/ffmpeg.exe"
        Get-Integrity -Root $part -Relative "bin/windows-x86_64/ffprobe.exe"
        Get-Integrity -Root $part -Relative "runtime/windows-x86_64/vc_redist.x64.exe"
    )
    $manifest.platforms = @($platform)
    $manifestJson = $manifest | ConvertTo-Json -Depth 12
    [IO.File]::WriteAllText(
        (Join-Path $part "manifest.json"),
        $manifestJson + [Environment]::NewLine,
        [Text.UTF8Encoding]::new($false)
    )

    $records = [ordered]@{
        schemaVersion = 1
        platform = "windows-x86-64"
        bundleVersion = [string]$manifest.bundleVersion
        vcRuntimeVersion = $vcVersion
        vcRuntimeSha256 = $vcSha256
        vcRuntimeMicrosoftSignatureValid = $true
        ffmpegSourceSha256 = $ffmpegSourceSha256
        whisperSourceSha256 = $ExpectedWhisperSha256
        assembledAtUtc = [DateTime]::UtcNow.ToString("o")
    }
    $recordJson = $records | ConvertTo-Json -Depth 4
    [IO.File]::WriteAllText(
        (Join-Path $part "windows-build-record.json"),
        $recordJson + [Environment]::NewLine,
        [Text.UTF8Encoding]::new($false)
    )

    $hashLines = Get-ChildItem -LiteralPath $part -Recurse -File -Force |
        Where-Object { $_.Name -ne "SHA256SUMS" } |
        Sort-Object FullName |
        ForEach-Object {
            $relative = $_.FullName.Substring($part.Length).TrimStart([char[]]"\/").Replace("\", "/")
            "$((Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant())  $relative"
        }
    [IO.File]::WriteAllText(
        (Join-Path $part "SHA256SUMS"),
        (($hashLines -join "`n") + "`n"),
        [Text.UTF8Encoding]::new($false)
    )
    [IO.File]::WriteAllText((Join-Path $part ".dy-screen-windows-resource-source"), "windows-x86-64`n", [Text.UTF8Encoding]::new($false))
    Move-Item -LiteralPath $part -Destination $OutputRoot
    Write-Host "Windows x64 可信资源源目录已生成。"
}
catch {
    if (Test-Path -LiteralPath $part) { Remove-Item -LiteralPath $part -Recurse -Force }
    throw
}
