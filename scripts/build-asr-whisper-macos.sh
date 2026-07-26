#!/bin/sh
set -eu

# 从锁定的 whisper.cpp v1.9.1 源码归档构建可独立分发的 macOS arm64 sidecar。
# 使用静态 Whisper/GGML 链接并嵌入 Metal shader，最终产物只允许依赖 macOS 系统库。

EXPECTED_SHA256='279af4ce60dbf397362868f3bacc75b56a4332ac2541cae155070093f6aaf0e3'
# Windows x64 uses the separately audited archive lock; keep both platform locks
# visible in the release script audit so a platform cannot silently drift.
WINDOWS_ARCHIVE_SHA256='d8cd961352377b1cc612224016a9ebdfe0ae508dc2b2f9ef514b341d672e3fdc'
EXPECTED_VERSION='1.9.1'
EXPECTED_COMMIT='f049fff95a089aa9969deb009cdd4892b3e74916'

if [ "$#" -ne 2 ]; then
  printf '%s\n' '用法：build-asr-whisper-macos.sh <whisper.cpp-v1.9.1.tar.gz> <输出目录>' >&2
  exit 2
fi

SOURCE_ARCHIVE=$1
OUTPUT_ROOT=$2

for tool in shasum tar cmake codesign otool rg; do
  command -v "$tool" >/dev/null 2>&1 || {
    printf '错误：缺少构建工具 %s。\n' "$tool" >&2
    exit 1
  }
done

if [ "$(uname -m)" != 'arm64' ]; then
  printf '%s\n' '错误：该脚本只在 Apple Silicon 构建 macOS arm64 sidecar。' >&2
  exit 1
fi

ACTUAL_SHA256=$(shasum -a 256 "$SOURCE_ARCHIVE" | awk '{print $1}')
if [ "$ACTUAL_SHA256" != "$EXPECTED_SHA256" ]; then
  printf '%s\n' '错误：whisper.cpp 源码 SHA-256 与锁定值不一致。' >&2
  exit 1
fi

if [ -e "$OUTPUT_ROOT" ]; then
  printf '%s\n' '错误：输出目录已经存在，请使用一个新的目录。' >&2
  exit 1
fi

TEMP_BASE=${TMPDIR:-/tmp}
TEMP_BASE=${TEMP_BASE%/}
BUILD_ROOT=$(mktemp -d "$TEMP_BASE/dy-screen-whisper.XXXXXX")
trap 'rm -rf "$BUILD_ROOT"' EXIT HUP INT TERM
tar -xf "$SOURCE_ARCHIVE" -C "$BUILD_ROOT"
SOURCE_ROOT="$BUILD_ROOT/whisper.cpp-f049fff95a089aa9969deb009cdd4892b3e74916"
BUILD_DIR="$BUILD_ROOT/build"

if [ ! -f "$SOURCE_ROOT/CMakeLists.txt" ]; then
  printf '%s\n' '错误：whisper.cpp 源码归档目录结构不符合锁定版本。' >&2
  exit 1
fi

cmake -S "$SOURCE_ROOT" -B "$BUILD_DIR" \
  -DCMAKE_BUILD_TYPE=Release \
  -DCMAKE_OSX_ARCHITECTURES=arm64 \
  -DBUILD_SHARED_LIBS=OFF \
  -DGGML_STATIC=ON \
  -DGGML_METAL=ON \
  -DGGML_METAL_EMBED_LIBRARY=ON \
  -DGGML_OPENMP=OFF \
  -DWHISPER_BUILD_TESTS=OFF \
  -DWHISPER_BUILD_EXAMPLES=ON \
  -DWHISPER_BUILD_SERVER=OFF \
  -DWHISPER_CURL=OFF

JOBS=$(sysctl -n hw.ncpu 2>/dev/null || getconf _NPROCESSORS_ONLN 2>/dev/null || printf '%s' 4)
cmake --build "$BUILD_DIR" --config Release --parallel "$JOBS" \
  --target whisper-cli whisper-vad-speech-segments

BIN_ROOT="$OUTPUT_ROOT/bin/macos-aarch64"
LICENSE_ROOT="$OUTPUT_ROOT/licenses"
mkdir -p "$BIN_ROOT" "$LICENSE_ROOT"
cp "$BUILD_DIR/bin/whisper-cli" "$BIN_ROOT/whisper-cli"
cp "$BUILD_DIR/bin/whisper-vad-speech-segments" "$BIN_ROOT/vad-speech-segments"
cp "$SOURCE_ROOT/LICENSE" "$LICENSE_ROOT/WhisperCpp-MIT.txt"

for binary in "$BIN_ROOT/whisper-cli" "$BIN_ROOT/vad-speech-segments"; do
  BAD_DEPENDENCIES=$(otool -L "$binary" | awk 'NR > 1 && $1 !~ /^\/System\// && $1 !~ /^\/usr\/lib\// {print $1}')
  if [ -n "$BAD_DEPENDENCIES" ]; then
    printf '错误：%s 仍依赖包外动态库：\n%s\n' "$binary" "$BAD_DEPENDENCIES" >&2
    exit 1
  fi
  if otool -l "$binary" | rg -F "$BUILD_ROOT"; then
    printf '错误：%s 仍引用构建临时目录。\n' "$binary" >&2
    exit 1
  fi
  codesign --force --sign - "$binary"
  codesign --verify --strict "$binary"
done

VERSION_OUTPUT=$("$BIN_ROOT/whisper-cli" --version)
case "$VERSION_OUTPUT" in
  *"$EXPECTED_VERSION"*) ;;
  *)
    printf '%s\n' '错误：whisper-cli 版本与锁定版本不一致。' >&2
    exit 1
    ;;
esac
printf '%s\n' "$VERSION_OUTPUT" > "$OUTPUT_ROOT/whisper-version.txt"

printf '%s\n' \
  'source=whisper.cpp-v1.9.1.tar.gz' \
  "source_sha256=$EXPECTED_SHA256" \
  "source_commit=$EXPECTED_COMMIT" \
  'architecture=arm64' \
  'linkage=static-whisper-ggml' \
  'metal=enabled-embedded' \
  'network=disabled' \
  > "$OUTPUT_ROOT/build-record.txt"

printf 'macOS arm64 whisper.cpp sidecar 已构建到：%s\n' "$OUTPUT_ROOT"
