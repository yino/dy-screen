#!/bin/sh
set -eu

# 该脚本只用于维护仓库内的固定 ASR 媒体样本。测试运行时直接读取已经生成的
# 小文件，不依赖 macOS 的语音合成服务，因此 Windows CI 也能复用同一组 fixture。
ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
OUTPUT="$ROOT/tests/fixtures/asr"
WORK="${TMPDIR:-/tmp}/dy-screen-asr-fixtures"

command -v ffmpeg >/dev/null 2>&1 || {
  printf '%s\n' '错误：生成 ASR fixture 需要 FFmpeg。' >&2
  exit 1
}
command -v say >/dev/null 2>&1 || {
  printf '%s\n' '错误：固定中文语音 fixture 只能在带 say 命令的 macOS 维护机上重新生成。' >&2
  exit 1
}

mkdir -p "$OUTPUT/Windows 中文路径" "$WORK"

render_speech() {
  name=$1
  text=$2
  say -v Tingting "$text" -o "$WORK/$name.aiff"
  ffmpeg -hide_banner -loglevel error -y \
    -f lavfi -i color=c=0x1f2937:s=320x180:r=25:d=12 \
    -i "$WORK/$name.aiff" -shortest -t 12 \
    -c:v libx264 -preset veryfast -pix_fmt yuv420p \
    -c:a aac -ar 16000 -ac 1 -movflags +faststart \
    "$OUTPUT/$name.mp4"
}

render_speech short_zh '欢迎来到直播间，今天的价格是九十九元。'
render_speech mixed_zh_en '今天推荐 AI Camera，price is ninety nine dollars。'
render_speech session_part_001 '第一段内容，现在开始介绍这款商品。'
render_speech session_part_002 '现在开始介绍这款商品，库存不多。'

ffmpeg -hide_banner -loglevel error -y \
  -f lavfi -i color=c=black:s=320x180:r=25:d=3 \
  -f lavfi -i anullsrc=r=16000:cl=mono:d=3 -shortest \
  -c:v libx264 -preset veryfast -pix_fmt yuv420p \
  -c:a aac -ar 16000 -ac 1 -movflags +faststart \
  "$OUTPUT/no_speech.mp4"

ffmpeg -hide_banner -loglevel error -y \
  -f lavfi -i color=c=0x111827:s=320x180:r=25:d=3 \
  -f lavfi -i 'sine=frequency=440:sample_rate=16000:duration=3' -shortest \
  -c:v libx264 -preset veryfast -pix_fmt yuv420p \
  -c:a aac -ar 16000 -ac 1 -movflags +faststart \
  "$OUTPUT/music_only.mp4"

ffmpeg -hide_banner -loglevel error -y \
  -f lavfi -i color=c=0x334155:s=320x180:r=25:d=3 \
  -c:v libx264 -preset veryfast -pix_fmt yuv420p -an -movflags +faststart \
  "$OUTPUT/video_only.mp4"

ffmpeg -hide_banner -loglevel error -y -i "$OUTPUT/short_zh.mp4" -c copy \
  "$OUTPUT/Windows 中文路径/测试 视频.mp4"

printf '%s\n' "ASR fixture 已生成到 $OUTPUT"
