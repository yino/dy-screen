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
# RGBA/alpha PNG 解码校验（任务 10.3）：透明中间产物依赖它，缺失即发行校验失败。
PIX_FMTS=$($FFMPEG -hide_banner -pix_fmts 2>&1) || {
  printf '%s\n' '错误：无法读取 FFmpeg 像素格式能力。' >&2
  exit 1
}
printf '%s\n' "$PIX_FMTS" | grep -Eq '(^| )rgba( |$)' || {
  printf '%s\n' '错误：当前 FFmpeg 缺少 rgba 像素格式，无法承载带 Alpha 的透明中间产物。' >&2
  exit 1
}
ALPHA_PROBE_DIR=$(mktemp -d)
trap 'rm -rf "$ALPHA_PROBE_DIR"' EXIT INT TERM
# 内嵌 8x8 RGBA PNG（含 alpha=0 像素）：不依赖 lavfi 等输入源滤镜，
# 因此在随包的极简构建上也能真实走一遍"PNG 解码 → rgba → 编码"。
printf '%s' 'iVBORw0KGgoAAAANSUhEUgAAAAgAAAAICAYAAADED76LAAAACXBIWXMAAAABAAAAAQBPJcTWAAAAEklEQVR4nGP8z4AfsBCQHyYKABaIAR2G3je1AAAAAElFTkSuQmCC' | base64 -d > "$ALPHA_PROBE_DIR/alpha.png" 2>/dev/null \
  || printf '%s' 'iVBORw0KGgoAAAANSUhEUgAAAAgAAAAICAYAAADED76LAAAACXBIWXMAAAABAAAAAQBPJcTWAAAAEklEQVR4nGP8z4AfsBCQHyYKABaIAR2G3je1AAAAAElFTkSuQmCC' | base64 -D > "$ALPHA_PROBE_DIR/alpha.png"
if [ ! -s "$ALPHA_PROBE_DIR/alpha.png" ]; then
  printf '%s\n' '错误：无法写入 Alpha PNG 探测样本。' >&2
  exit 1
fi
"$FFMPEG" -hide_banner -v error -i "$ALPHA_PROBE_DIR/alpha.png" -vf format=rgba \
  -frames:v 1 -y "$ALPHA_PROBE_DIR/alpha.jpg" || {
  printf '%s\n' '错误：当前 FFmpeg 无法解码带 Alpha 的 PNG（RGBA 路径不可用）。' >&2
  exit 1
}
[ -s "$ALPHA_PROBE_DIR/alpha.jpg" ] || {
  printf '%s\n' '错误：Alpha PNG 解码探测没有产出结果。' >&2
  exit 1
}

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
