#!/bin/sh
set -eu

if [ "$#" -ne 6 ]; then
  printf '%s\n' '用法：prepare-asr-resources-macos.sh <共用资源目录> <FFmpeg 构建目录> <Whisper 构建目录> <输出目录> <aarch64|x86_64> <bundleVersion>' >&2
  exit 2
fi

COMMON_SOURCE=$1
FFMPEG_ROOT=$2
WHISPER_ROOT=$3
OUTPUT_ROOT=$4
MACOS_ARCH=$5
BUNDLE_VERSION=$6

case "$MACOS_ARCH" in
  aarch64) APPLE_ARCH=arm64 ;;
  x86_64) APPLE_ARCH=x86_64 ;;
  *)
    printf '错误：不支持的 macOS 架构 %s，只接受 aarch64 或 x86_64。\n' "$MACOS_ARCH" >&2
    exit 2
    ;;
esac
MACOS_RESOURCE_DIR=macos-$MACOS_ARCH

SCRIPT_ROOT=$(CDPATH= cd -- "$(dirname "$0")" && pwd)
REPO_ROOT=$(dirname "$SCRIPT_ROOT")
MANIFEST_TEMPLATE="$REPO_ROOT/resources/asr/manifest.json"

for tool in jq lipo shasum; do
  command -v "$tool" >/dev/null 2>&1 || {
    printf '错误：缺少资源组装工具 %s。\n' "$tool" >&2
    exit 1
  }
done

if [ -z "$BUNDLE_VERSION" ] || printf '%s' "$BUNDLE_VERSION" | grep -Eq '[^A-Za-z0-9._-]'; then
  printf '%s\n' '错误：bundleVersion 只能包含 ASCII 字母、数字、点、横线和下划线。' >&2
  exit 2
fi
if [ -e "$OUTPUT_ROOT" ]; then
  printf '%s\n' '错误：输出目录已经存在，请使用一个新的目录。' >&2
  exit 1
fi
if [ ! -f "$MANIFEST_TEMPLATE" ] || [ -L "$MANIFEST_TEMPLATE" ]; then
  printf '%s\n' '错误：resources/asr/manifest.json 模板缺失或不是普通文件。' >&2
  exit 1
fi

OUTPUT_PARENT=$(dirname "$OUTPUT_ROOT")
OUTPUT_NAME=$(basename "$OUTPUT_ROOT")
mkdir -p "$OUTPUT_PARENT"
OUTPUT_PARENT=$(CDPATH= cd -- "$OUTPUT_PARENT" && pwd)
OUTPUT_ROOT="$OUTPUT_PARENT/$OUTPUT_NAME"
TEMP_ROOT="$OUTPUT_ROOT.part"
if [ -e "$TEMP_ROOT" ]; then
  printf '%s\n' '错误：存在未清理的 macOS 资源组装临时目录。' >&2
  exit 1
fi
trap 'rm -rf "$TEMP_ROOT"' EXIT HUP INT TERM
mkdir -p "$TEMP_ROOT/bin/$MACOS_RESOURCE_DIR" "$TEMP_ROOT/lib/$MACOS_RESOURCE_DIR" \
  "$TEMP_ROOT/models" "$TEMP_ROOT/normalization" "$TEMP_ROOT/licenses"

require_regular_file() {
  if [ ! -f "$1" ] || [ -L "$1" ]; then
    printf '错误：资源必须是普通文件：%s\n' "$1" >&2
    exit 1
  fi
}

assert_macho_architecture() {
  require_regular_file "$1"
  ARCHITECTURES=$(lipo -archs "$1")
  if [ "$ARCHITECTURES" != "$APPLE_ARCH" ]; then
    printf '错误：%s 的 Mach-O 架构为 %s，目标要求 %s。\n' "$1" "$ARCHITECTURES" "$APPLE_ARCH" >&2
    exit 1
  fi
}

for name in ggml-small-q5_1.bin ggml-silero-v6.2.0.bin; do
  require_regular_file "$COMMON_SOURCE/models/$name"
  cp "$COMMON_SOURCE/models/$name" "$TEMP_ROOT/models/$name"
done
require_regular_file "$COMMON_SOURCE/normalization/TSCharacters.txt"
cp "$COMMON_SOURCE/normalization/TSCharacters.txt" "$TEMP_ROOT/normalization/TSCharacters.txt"
for name in WhisperCpp-MIT.txt OpenAI-Whisper-MIT.txt Silero-VAD-MIT.txt OpenCC-Apache-2.0.txt FFmpeg-LGPL-2.1.txt; do
  require_regular_file "$COMMON_SOURCE/licenses/$name"
  cp "$COMMON_SOURCE/licenses/$name" "$TEMP_ROOT/licenses/$name"
done

for name in ffmpeg ffprobe; do
  SOURCE="$FFMPEG_ROOT/bin/$MACOS_RESOURCE_DIR/$name"
  assert_macho_architecture "$SOURCE"
  cp "$SOURCE" "$TEMP_ROOT/bin/$MACOS_RESOURCE_DIR/$name"
done
for SOURCE in "$FFMPEG_ROOT/lib/$MACOS_RESOURCE_DIR/"*.dylib; do
  assert_macho_architecture "$SOURCE"
  cp "$SOURCE" "$TEMP_ROOT/lib/$MACOS_RESOURCE_DIR/$(basename "$SOURCE")"
done
for name in whisper-cli vad-speech-segments; do
  SOURCE="$WHISPER_ROOT/bin/$MACOS_RESOURCE_DIR/$name"
  assert_macho_architecture "$SOURCE"
  cp "$SOURCE" "$TEMP_ROOT/bin/$MACOS_RESOURCE_DIR/$name"
done

PLATFORM_COUNT=$(jq --arg arch "$MACOS_ARCH" '[.platforms[] | select(.os == "macos" and .arch == $arch)] | length' "$MANIFEST_TEMPLATE")
if [ "$PLATFORM_COUNT" -ne 1 ]; then
  printf '错误：manifest 模板没有唯一声明 macOS %s 平台。\n' "$MACOS_ARCH" >&2
  exit 1
fi
jq --arg arch "$MACOS_ARCH" --arg bundle "$BUNDLE_VERSION" \
  '.bundleVersion = $bundle | .platforms = [.platforms[] | select(.os == "macos" and .arch == $arch)]' \
  "$MANIFEST_TEMPLATE" > "$TEMP_ROOT/manifest.json"

for executable in "$TEMP_ROOT/bin/$MACOS_RESOURCE_DIR/"*; do
  chmod 755 "$executable"
done
printf '%s\n' \
  "platform=macos-$MACOS_ARCH" \
  "bundle_version=$BUNDLE_VERSION" \
  "manifest_template=resources/asr/manifest.json" \
  "ffmpeg_build_record_sha256=$(shasum -a 256 "$FFMPEG_ROOT/build-record.txt" | awk '{print $1}')" \
  "whisper_build_record_sha256=$(shasum -a 256 "$WHISPER_ROOT/build-record.txt" | awk '{print $1}')" \
  > "$TEMP_ROOT/resource-source-record.txt"

mv "$TEMP_ROOT" "$OUTPUT_ROOT"
trap - EXIT HUP INT TERM
printf 'macOS %s 可信资源源已组装到：%s\n' "$MACOS_ARCH" "$OUTPUT_ROOT"
