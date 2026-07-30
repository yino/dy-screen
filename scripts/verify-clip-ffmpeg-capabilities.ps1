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

$decoders = Get-FfmpegCapabilities "-decoders"
$demuxers = Get-FfmpegCapabilities "-demuxers"
$muxers = Get-FfmpegCapabilities "-muxers"
$filters = Get-FfmpegCapabilities "-filters"
$encoders = Get-FfmpegCapabilities "-encoders"

Assert-Matches $decoders '^\s*[VAS]\S*\s+png\s+' "FFmpeg 缺少 PNG 解码器。"
foreach ($demuxer in @("concat", "image2")) {
    Assert-Matches $demuxers ("^\s*D\s+" + [Regex]::Escape($demuxer) + "(?:\s|,)") "FFmpeg 缺少 $demuxer demuxer。"
}
Assert-Matches $muxers '^\s*E\s+mp4(?:\s|,)' "FFmpeg 缺少 MP4 muxer。"
foreach ($filter in @("aformat", "asetpts", "concat", "fade", "format", "overlay", "pad", "scale", "setsar", "setpts", "volume")) {
    Assert-Matches $filters ("^\s*\S+\s+" + [Regex]::Escape($filter) + "\s+") "FFmpeg 缺少 $filter 滤镜。"
}
Assert-Matches $encoders '^\s*[VAS]\S*\s+aac\s+' "FFmpeg 缺少 AAC 编码器。"
Assert-Matches $encoders '^\s*[VAS]\S*\s+(?:h264_videotoolbox|h264_mf|libx264)\s+' "FFmpeg 没有受支持的 H.264 编码器。"

Write-Host "带 ASR 字幕的剪辑导出能力检查通过。"
