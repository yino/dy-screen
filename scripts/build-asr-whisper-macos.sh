#!/bin/sh
set -eu

# 从锁定的 whisper.cpp v1.9.1 源码归档构建指定架构的 macOS sidecar。
# arm64 内嵌 Metal shader，x86_64 使用可移植 CPU 基线；最终产物只允许依赖系统库。

EXPECTED_SHA256='279af4ce60dbf397362868f3bacc75b56a4332ac2541cae155070093f6aaf0e3'
EXPECTED_VERSION='1.9.1'
EXPECTED_COMMIT='f049fff95a089aa9969deb009cdd4892b3e74916'

if [ "$#" -ne 3 ]; then
  printf '%s\n' '用法：build-asr-whisper-macos.sh <whisper.cpp-v1.9.1.tar.gz> <输出目录> <aarch64|x86_64>' >&2
  exit 2
fi

SOURCE_ARCHIVE=$1
OUTPUT_ROOT=$2
MACOS_ARCH=$3

case "$MACOS_ARCH" in
  aarch64)
    APPLE_ARCH=arm64
    GGML_METAL=ON
    GGML_METAL_EMBED_LIBRARY=ON
    ;;
  x86_64)
    APPLE_ARCH=x86_64
    GGML_METAL=OFF
    GGML_METAL_EMBED_LIBRARY=OFF
    ;;
  *)
    printf '错误：不支持的 macOS 架构 %s，只接受 aarch64 或 x86_64。\n' "$MACOS_ARCH" >&2
    exit 2
    ;;
esac
MACOS_RESOURCE_DIR=macos-$MACOS_ARCH

for tool in shasum tar cmake codesign otool lipo rg; do
  command -v "$tool" >/dev/null 2>&1 || {
    printf '错误：缺少构建工具 %s。\n' "$tool" >&2
    exit 1
  }
done

if [ "$(uname -s)" != Darwin ]; then
  printf '%s\n' '错误：该脚本只能在 macOS 构建机运行。' >&2
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
  -DCMAKE_OSX_ARCHITECTURES=$APPLE_ARCH \
  -DCMAKE_OSX_DEPLOYMENT_TARGET=12.0 \
  -DBUILD_SHARED_LIBS=OFF \
  -DGGML_STATIC=ON \
  -DGGML_NATIVE=OFF \
  -DGGML_SSE42=ON \
  -DGGML_AVX=OFF \
  -DGGML_AVX2=OFF \
  -DGGML_FMA=OFF \
  -DGGML_F16C=OFF \
  -DGGML_BMI2=OFF \
  -DGGML_BLAS=OFF \
  -DGGML_METAL=$GGML_METAL \
  -DGGML_METAL_EMBED_LIBRARY=$GGML_METAL_EMBED_LIBRARY \
  -DGGML_OPENMP=OFF \
  -DWHISPER_BUILD_TESTS=OFF \
  -DWHISPER_BUILD_EXAMPLES=ON \
  -DWHISPER_BUILD_SERVER=OFF \
  -DWHISPER_CURL=OFF

JOBS=$(sysctl -n hw.ncpu 2>/dev/null || getconf _NPROCESSORS_ONLN 2>/dev/null || printf '%s' 4)
cmake --build "$BUILD_DIR" --config Release --parallel "$JOBS" \
  --target whisper-cli whisper-vad-speech-segments

BIN_ROOT="$OUTPUT_ROOT/bin/$MACOS_RESOURCE_DIR"
LICENSE_ROOT="$OUTPUT_ROOT/licenses"
mkdir -p "$BIN_ROOT" "$LICENSE_ROOT"
cp "$BUILD_DIR/bin/whisper-cli" "$BIN_ROOT/whisper-cli"
cp "$BUILD_DIR/bin/whisper-vad-speech-segments" "$BIN_ROOT/vad-speech-segments"
cp "$SOURCE_ROOT/LICENSE" "$LICENSE_ROOT/WhisperCpp-MIT.txt"

assert_macho_architecture() {
  ARCHITECTURES=$(lipo -archs "$1")
  if [ "$ARCHITECTURES" != "$APPLE_ARCH" ]; then
    printf '错误：%s 的 Mach-O 架构为 %s，目标要求 %s。\n' "$1" "$ARCHITECTURES" "$APPLE_ARCH" >&2
    exit 1
  fi
}

assert_macos_deployment_target() {
  MINIMUM_VERSION=$(otool -l "$1" | awk '/LC_BUILD_VERSION/{found=1; next} found && /minos/{print $2; exit}')
  if [ "$MINIMUM_VERSION" != 12.0 ]; then
    printf '错误：%s 的最低 macOS 版本为 %s，目标要求 12.0。\n' "$1" "${MINIMUM_VERSION:-未知}" >&2
    exit 1
  fi
}

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
  assert_macho_architecture "$binary"
  assert_macos_deployment_target "$binary"
done

TARGET_RUNNER=
if [ "$(uname -m)" = arm64 ] && [ "$MACOS_ARCH" = x86_64 ] && arch -x86_64 /usr/bin/true >/dev/null 2>&1; then
  TARGET_RUNNER='arch -x86_64'
fi
if [ -n "$TARGET_RUNNER" ]; then
  VERSION_OUTPUT=$(arch -x86_64 "$BIN_ROOT/whisper-cli" --version)
elif [ "$(uname -m)" = "$APPLE_ARCH" ]; then
  VERSION_OUTPUT=$("$BIN_ROOT/whisper-cli" --version)
else
  VERSION_OUTPUT="whisper.cpp $EXPECTED_VERSION; target=$MACOS_ARCH; runtime validation deferred"
fi
case "$VERSION_OUTPUT" in *"$EXPECTED_VERSION"*) ;; *)
  printf '%s\n' '错误：whisper-cli 版本与锁定版本不一致。' >&2
  exit 1
esac
printf '%s\n' "$VERSION_OUTPUT" > "$OUTPUT_ROOT/whisper-version.txt"

printf '%s\n' \
  'source=whisper.cpp-v1.9.1.tar.gz' \
  "source_sha256=$EXPECTED_SHA256" \
  "source_commit=$EXPECTED_COMMIT" \
  "architecture=$MACOS_ARCH" \
  'linkage=static-whisper-ggml' \
  "metal=$GGML_METAL" \
  'cpu_baseline=sse4.2-no-avx' \
  'network=disabled' \
  > "$OUTPUT_ROOT/build-record.txt"

printf 'macOS %s whisper.cpp sidecar 已构建到：%s\n' "$MACOS_ARCH" "$OUTPUT_ROOT"
