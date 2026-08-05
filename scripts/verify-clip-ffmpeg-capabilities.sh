#!/bin/sh
set -eu

if [ "$#" -ne 1 ]; then
  printf '%s\n' '用法：verify-clip-ffmpeg-capabilities.sh <ffmpeg>' >&2
  exit 2
fi

FFMPEG=$1
case "$FFMPEG" in
  */*) ;;
  *) FFMPEG=$(command -v "$FFMPEG" 2>/dev/null || true) ;;
esac
if [ ! -x "$FFMPEG" ]; then
  printf '%s\n' '错误：剪辑能力审计找不到可执行的 FFmpeg。' >&2
  exit 1
fi

DECODERS=$($FFMPEG -hide_banner -decoders 2>&1) || {
  printf '%s\n' '错误：无法读取 FFmpeg 解码器能力。' >&2
  exit 1
}
DEMUXERS=$($FFMPEG -hide_banner -demuxers 2>&1) || {
  printf '%s\n' '错误：无法读取 FFmpeg demuxer 能力。' >&2
  exit 1
}
FILTERS=$($FFMPEG -hide_banner -filters 2>&1) || {
  printf '%s\n' '错误：无法读取 FFmpeg 滤镜能力。' >&2
  exit 1
}
MUXERS=$($FFMPEG -hide_banner -muxers 2>&1) || {
  printf '%s\n' '错误：无法读取 FFmpeg muxer 能力。' >&2
  exit 1
}
ENCODERS=$($FFMPEG -hide_banner -encoders 2>&1) || {
  printf '%s\n' '错误：无法读取 FFmpeg 编码器能力。' >&2
  exit 1
}

for decoder in h264 hevc aac png; do
  printf '%s\n' "$DECODERS" | grep -Eq "^ [VAS][A-Z.]{5} $decoder +" || {
    printf '错误：当前 FFmpeg 缺少 %s 解码器。\n' "$decoder" >&2
    exit 1
  }
done
for demuxer in concat image2; do
  printf '%s\n' "$DEMUXERS" | grep -Eq "^ D +$demuxer( |,)" || {
    printf '错误：当前 FFmpeg 缺少 %s demuxer。\n' "$demuxer" >&2
    exit 1
  }
done
printf '%s\n' "$MUXERS" | grep -Eq '^  E +mp4( |,)' || {
  printf '%s\n' '错误：当前 FFmpeg 缺少 MP4 muxer。' >&2
  exit 1
}
for filter in aformat asetpts concat fade format overlay pad scale setsar setpts volume; do
  printf '%s\n' "$FILTERS" | grep -Eq "^ [A-Z.]+ +$filter +" || {
    printf '错误：当前 FFmpeg 缺少 %s 滤镜。\n' "$filter" >&2
    exit 1
  }
done
printf '%s\n' "$ENCODERS" | grep -Eq '^ [VAS][A-Z.]{5} aac +' || {
  printf '%s\n' '错误：当前 FFmpeg 缺少 AAC 编码器。' >&2
  exit 1
}
printf '%s\n' "$ENCODERS" | grep -Eq '^ [VAS][A-Z.]{5} (h264_videotoolbox|h264_mf|libx264) +' || {
  printf '%s\n' '错误：当前 FFmpeg 没有受支持的 H.264 编码器。' >&2
  exit 1
}

printf '%s\n' 'H.264/HEVC 桥接素材预览和带 ASR 字幕的剪辑导出能力检查通过。'
