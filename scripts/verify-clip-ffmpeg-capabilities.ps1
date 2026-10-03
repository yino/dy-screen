[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$Ffmpeg
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

if (-not (Test-Path -LiteralPath $Ffmpeg -PathType Leaf)) {
    throw "剪辑能力审计找不到 FFmpeg。"
}

function Get-FfmpegCapabilities {
    param([string]$Listing)
    $output = & $Ffmpeg -hide_banner $Listing 2>&1 | Out-String
    if ($LASTEXITCODE -ne 0) {
        throw "无法读取 FFmpeg $Listing 能力。"
    }
    return $output
}

function Assert-Matches {
    param([string]$Value, [string]$Pattern, [string]$Message)
    if (-not [Regex]::IsMatch($Value, $Pattern, [Text.RegularExpressions.RegexOptions]::Multiline)) {
        throw $Message
    }
}

try {
    $decoders = Get-FfmpegCapabilities "-decoders"
    $demuxers = Get-FfmpegCapabilities "-demuxers"
    $muxers = Get-FfmpegCapabilities "-muxers"
    $filters = Get-FfmpegCapabilities "-filters"
    $encoders = Get-FfmpegCapabilities "-encoders"

    foreach ($decoder in @("h264", "hevc", "aac", "png")) {
        Assert-Matches $decoders ("^\s*[VAS]\S*\s+" + [Regex]::Escape($decoder) + "\s+") "FFmpeg 缺少 $decoder 解码器。"
    }
    # RGBA/alpha PNG 解码校验（任务 10.3）：透明中间产物依赖它，缺失即发行校验失败。
    $pixelFormats = Get-FfmpegCapabilities "-pix_fmts"
    Assert-Matches $pixelFormats '^\s*\S*\s+rgba\s' "FFmpeg 缺少 rgba 像素格式，无法承载带 Alpha 的透明中间产物。"

    $probeDirectory = Join-Path ([System.IO.Path]::GetTempPath()) ("dy-screen-alpha-" + [Guid]::NewGuid().ToString("N"))
    New-Item -ItemType Directory -Path $probeDirectory | Out-Null
    try {
        $alphaPng = Join-Path $probeDirectory "alpha.png"
        [System.IO.File]::WriteAllBytes($alphaPng, [Convert]::FromBase64String("iVBORw0KGgoAAAANSUhEUgAAAAgAAAAICAYAAADED76LAAAACXBIWXMAAAABAAAAAQBPJcTWAAAAEklEQVR4nGP8z4AfsBCQHyYKABaIAR2G3je1AAAAAElFTkSuQmCC"))
        $alphaJpg = Join-Path $probeDirectory "alpha.jpg"
        & $Ffmpeg -hide_banner -v error -i $alphaPng -vf format=rgba -frames:v 1 -y $alphaJpg
        if ($LASTEXITCODE -ne 0 -or -not (Test-Path -LiteralPath $alphaJpg -PathType Leaf)) {
            throw "FFmpeg 无法解码带 Alpha 的 PNG（RGBA 路径不可用）。"
        }
    }
    finally {
        Remove-Item -Recurse -Force $probeDirectory -ErrorAction SilentlyContinue
    }

    foreach ($demuxer in @("concat", "image2")) {
        Assert-Matches $demuxers ("^\s*D\s+" + [Regex]::Escape($demuxer) + "(?:\s|,)") "FFmpeg 缺少 $demuxer demuxer。"
    }
    Assert-Matches $muxers '^\s*E\s+mp4(?:\s|,)' "FFmpeg 缺少 MP4 muxer。"
    foreach ($filter in @("aformat", "asetpts", "concat", "fade", "format", "overlay", "pad", "scale", "setsar", "setpts", "volume")) {
        Assert-Matches $filters ("^\s*\S+\s+" + [Regex]::Escape($filter) + "\s+") "FFmpeg 缺少 $filter 滤镜。"
    }
    Assert-Matches $encoders '^\s*[VAS]\S*\s+aac\s+' "FFmpeg 缺少 AAC 编码器。"
    Assert-Matches $encoders '^\s*[VAS]\S*\s+(?:h264_videotoolbox|h264_mf|libx264)\s+' "FFmpeg 没有受支持的 H.264 编码器。"

    Write-Host "H.264/HEVC 桥接素材预览和带 ASR 字幕的剪辑导出能力检查通过。"
}
catch {
    $diagnostic = [string]$_.Exception.Message
    foreach ($root in @($env:RUNNER_TEMP, $env:GITHUB_WORKSPACE)) {
        if (-not [string]::IsNullOrWhiteSpace($root)) {
            $diagnostic = $diagnostic.Replace($root, "<runner-path>")
        }
    }
    $diagnostic = $diagnostic.Replace("%", "%25").Replace("`r", "%0D").Replace("`n", "%0A")
    Write-Host "::error title=Windows FFmpeg 剪辑能力校验失败::diagnostic=$diagnostic"
    throw
}
