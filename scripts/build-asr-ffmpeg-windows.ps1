# 从锁定的 FFmpeg 官方源码构建应用共用的 Windows x64 LGPL sidecar。
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$SourceArchive,
    [Parameter(Mandatory = $true)]
    [string]$OutputRoot,
    [string]$Msys2Root = $(if ([string]::IsNullOrWhiteSpace($env:MSYS2_ROOT)) { "C:\msys64" } else { $env:MSYS2_ROOT })
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$ExpectedSha256 = "464beb5e7bf0c311e68b45ae2f04e9cc2af88851abb4082231742a74d97b524c"
$ExpectedVersion = "8.1.2"

function Assert-RegularFile {
    param([string]$Path, [string]$Label)
    $item = Get-Item -LiteralPath $Path -Force -ErrorAction Stop
    if ($item.PSIsContainer -or (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0)) {
        throw "$Label 必须是普通文件，不能是目录或重解析点。"
    }
}

function Assert-X64Pe {
    param([string]$Path, [string]$Label)
    $headers = & dumpbin.exe /headers $Path 2>&1 | Out-String
    if ($LASTEXITCODE -ne 0 -or $headers -notmatch "(?im)^\s*8664 machine \(x64\)") {
        throw "$Label 不是 Windows x64 PE 文件。"
    }
}

function Assert-NoBundledRuntimeDependency {
    param([string]$Path, [string]$Label)
    $dependencies = & dumpbin.exe /dependents $Path 2>&1 | Out-String
    if ($LASTEXITCODE -ne 0) { throw "无法读取 $Label 的 PE 依赖。" }
    if ($dependencies -match "(?im)\b(?:avcodec|avdevice|avfilter|avformat|avutil|swresample|swscale|libgcc|libstdc\+\+|libwinpthread|zlib1|msys-|cygwin)[^\r\n]*\.dll\b") {
        throw "$Label 仍依赖未随包声明的 FFmpeg、MSYS2 或编译器 DLL。"
    }
}

foreach ($tool in @("dumpbin.exe")) {
    if (-not (Get-Command $tool -ErrorAction SilentlyContinue)) {
        throw "缺少构建工具 $tool；请使用 Visual Studio 2022 x64 Developer PowerShell。"
    }
}
if (-not [Runtime.InteropServices.RuntimeInformation]::IsOSPlatform(
    [Runtime.InteropServices.OSPlatform]::Windows
) -or [Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString().ToLowerInvariant() -ne "x64") {
    throw "Windows FFmpeg 只能在原生 Windows x64 构建机生成。"
}

Assert-RegularFile -Path $SourceArchive -Label "FFmpeg 源码归档"
if (Test-Path -LiteralPath $OutputRoot) { throw "输出目录已经存在，请使用一个新的目录。" }
$actualSha256 = (Get-FileHash -LiteralPath $SourceArchive -Algorithm SHA256).Hash.ToLowerInvariant()
if ($actualSha256 -ne $ExpectedSha256) { throw "FFmpeg 源码 SHA-256 与锁定值不一致。" }

$bash = Join-Path $Msys2Root "usr\bin\bash.exe"
$gcc = Join-Path $Msys2Root "ucrt64\bin\gcc.exe"
Assert-RegularFile -Path $bash -Label "MSYS2 bash"
Assert-RegularFile -Path $gcc -Label "MSYS2 UCRT64 gcc"

$temporaryRoot = Join-Path ([IO.Path]::GetTempPath()) ("dy-screen-ffmpeg-windows-" + [Guid]::NewGuid().ToString("N"))
$buildScript = Join-Path $temporaryRoot "build.sh"
try {
    [IO.Directory]::CreateDirectory($temporaryRoot) | Out-Null
    $script = @'
#!/usr/bin/env bash
set -euo pipefail
export PATH="/ucrt64/bin:/usr/bin"
source_archive=$(cygpath -u "$1")
output_root=$(cygpath -u "$2")
work_root=$(mktemp -d)
trap 'rm -rf "$work_root"' EXIT
tar -xf "$source_archive" -C "$work_root"
source_root="$work_root/ffmpeg-8.1.2"
install_root="$work_root/install"
cd "$source_root"
./configure \
  --prefix="$install_root" \
  --target-os=mingw32 \
  --arch=x86_64 \
  --disable-autodetect \
  --disable-doc \
  --disable-debug \
  --disable-everything \
  --enable-static \
  --disable-shared \
  --disable-pthreads \
  --enable-w32threads \
  --enable-ffmpeg \
  --enable-ffprobe \
  --enable-avcodec \
  --enable-avformat \
  --enable-avfilter \
  --enable-swresample \
  --enable-swscale \
  --enable-network \
  --enable-schannel \
  --enable-mediafoundation \
  --enable-zlib \
  --pkg-config-flags=--static \
  --enable-protocol=file,pipe,http,https,tcp,tls,crypto,httpproxy \
  --enable-demuxer=mov,matroska,flv,hls,mpegts,mp3,wav,ogg,flac,aac,concat,image2 \
  --enable-parser=aac,aac_latm,ac3,av1,flac,h264,hevc,mjpeg,mpeg4video,mpegaudio,opus,vorbis,vp8,vp9 \
  --enable-decoder=aac,aac_fixed,alac,flac,mp3,mp3float,opus,vorbis,ac3,eac3,pcm_s16le,pcm_s24le,pcm_s32le,pcm_f32le,h264,hevc,av1,vp8,vp9,mpeg4,mjpeg,png \
  --enable-encoder=pcm_s16le,aac,h264_mf,mjpeg \
  --enable-muxer=wav,segment,matroska,mov,mp4,image2 \
  --enable-filter=aresample,aformat,anull,asetpts,scale,format,setpts,pad,setsar,volume,fade,concat,overlay \
  --enable-bsf=aac_adtstoasc,h264_mp4toannexb,hevc_mp4toannexb \
  --extra-cflags='-O2' \
  --extra-ldflags='-static -static-libgcc'
make -j"${NUMBER_OF_PROCESSORS:-4}" install
mkdir -p "$output_root/bin/windows-x86_64" "$output_root/licenses"
cp "$install_root/bin/ffmpeg.exe" "$output_root/bin/windows-x86_64/ffmpeg.exe"
cp "$install_root/bin/ffprobe.exe" "$output_root/bin/windows-x86_64/ffprobe.exe"
cp "$source_root/COPYING.LGPLv2.1" "$output_root/licenses/FFmpeg-LGPL-2.1.txt"
'@
    [IO.File]::WriteAllText($buildScript, $script, [Text.UTF8Encoding]::new($false))
    & $bash $buildScript ([IO.Path]::GetFullPath($SourceArchive)) ([IO.Path]::GetFullPath($OutputRoot))
    if ($LASTEXITCODE -ne 0) { throw "FFmpeg Windows x64 构建失败。" }

    $ffmpeg = Join-Path $OutputRoot "bin\windows-x86_64\ffmpeg.exe"
    $ffprobe = Join-Path $OutputRoot "bin\windows-x86_64\ffprobe.exe"
    foreach ($binary in @($ffmpeg, $ffprobe)) {
        Assert-RegularFile -Path $binary -Label "FFmpeg 原生程序"
        Assert-X64Pe -Path $binary -Label "FFmpeg 原生程序"
        Assert-NoBundledRuntimeDependency -Path $binary -Label "FFmpeg 原生程序"
    }

    $version = & $ffmpeg -hide_banner -version 2>&1 | Out-String
    if ($LASTEXITCODE -ne 0 -or $version -notmatch "ffmpeg version\s+$([Regex]::Escape($ExpectedVersion))") {
        throw "FFmpeg 版本与锁定版本不一致。"
    }
    if ($version -match "--enable-(?:gpl|nonfree)") { throw "FFmpeg 构建意外启用了 GPL 或 nonfree。" }

    $listings = @{}
    foreach ($listing in @("protocols", "demuxers", "muxers", "encoders", "decoders", "filters")) {
        $listings[$listing] = & $ffmpeg -hide_banner ("-" + $listing) 2>&1 | Out-String
        if ($LASTEXITCODE -ne 0) { throw "无法读取 FFmpeg $listing 能力。" }
    }
    foreach ($protocol in @("http", "https", "tcp", "tls")) {
        if ($listings.protocols -notmatch "(?m)^\s{2}$([Regex]::Escape($protocol))\s*$") { throw "FFmpeg 缺少 $protocol 协议。" }
    }
    foreach ($demuxer in @("concat", "flv", "hls", "image2", "matroska", "mov", "mpegts")) {
        if ($listings.demuxers -notmatch "(?m)^\s*D\s+$([Regex]::Escape($demuxer))(?:\s|,)") { throw "FFmpeg 缺少 $demuxer demuxer。" }
    }
    foreach ($muxer in @("segment", "matroska", "mov", "mp4", "image2", "wav")) {
        if ($listings.muxers -notmatch "(?m)^\s*E\s+$([Regex]::Escape($muxer))(?:\s|,)") { throw "FFmpeg 缺少 $muxer muxer。" }
    }
    foreach ($encoder in @("aac", "h264_mf", "mjpeg", "pcm_s16le")) {
        if ($listings.encoders -notmatch "(?m)^\s*[VAS]\S*\s+$([Regex]::Escape($encoder))\s+") { throw "FFmpeg 缺少 $encoder encoder。" }
    }
    foreach ($decoder in @("aac", "h264", "hevc", "png")) {
        if ($listings.decoders -notmatch "(?m)^\s*[VAS]\S*\s+$([Regex]::Escape($decoder))\s+") { throw "FFmpeg 缺少 $decoder decoder。" }
    }
    foreach ($filter in @("aformat", "asetpts", "concat", "fade", "format", "overlay", "pad", "scale", "setsar", "setpts", "volume")) {
        if ($listings.filters -notmatch "(?m)^\s*\S+\s+$([Regex]::Escape($filter))\s+") { throw "FFmpeg 缺少 $filter 滤镜。" }
    }

    [IO.File]::WriteAllText((Join-Path $OutputRoot "ffmpeg-version.txt"), $version, [Text.UTF8Encoding]::new($false))
    $probeVersion = & $ffprobe -hide_banner -version 2>&1 | Out-String
    if ($LASTEXITCODE -ne 0 -or $probeVersion -notmatch "ffprobe version\s+$([Regex]::Escape($ExpectedVersion))") {
        throw "FFprobe 版本与锁定版本不一致。"
    }
    [IO.File]::WriteAllText((Join-Path $OutputRoot "ffprobe-version.txt"), $probeVersion, [Text.UTF8Encoding]::new($false))
    @(
        "source=ffmpeg-8.1.2.tar.xz"
        "source_sha256=$ExpectedSha256"
        "architecture=x86_64"
        "license=LGPL-2.1-or-later"
        "toolchain=msys2-ucrt64"
        "runtime_dependencies=windows-system-only"
        "recording=https-flv-hls-segment-matroska"
        "preview=mov-h264-hevc-aac-h264_mf-mjpeg"
        "clip_export=h264-hevc-aac-decode-h264_mf-aac-mp4-png-concat-overlay"
    ) | Set-Content -LiteralPath (Join-Path $OutputRoot "build-record.txt") -Encoding utf8

    Write-Host "Windows x64 FFmpeg 已构建到受控输出目录。"
}
catch {
    if (Test-Path -LiteralPath $OutputRoot) {
        Remove-Item -LiteralPath $OutputRoot -Recurse -Force
    }
    throw
}
finally {
    if (Test-Path -LiteralPath $temporaryRoot) {
        Remove-Item -LiteralPath $temporaryRoot -Recurse -Force
    }
}
