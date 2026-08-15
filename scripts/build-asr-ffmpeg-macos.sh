#!/bin/sh
set -eu

# 从锁定的 FFmpeg 官方源码构建指定架构的 macOS LGPL sidecar。
# 同一受控二进制服务直播录制、预览、封面和本地 ASR；不启用 GPL/nonfree 或第三方编解码库。

RELEASE_SHA256='464beb5e7bf0c311e68b45ae2f04e9cc2af88851abb4082231742a74d97b524c'
GITHUB_TAG_SHA256='9fd092511605bbebafe095ea6d38d9e40f34d12f7386e1258372df8be0576eb7'

if [ "$#" -ne 3 ]; then
  printf '%s\n' '用法：build-asr-ffmpeg-macos.sh <FFmpeg 8.1.2 锁定源码归档> <输出目录> <aarch64|x86_64>' >&2
  exit 2
fi

SOURCE_ARCHIVE=$1
OUTPUT_ROOT=$2
MACOS_ARCH=$3

case "$MACOS_ARCH" in
  aarch64) APPLE_ARCH=arm64 ;;
  x86_64) APPLE_ARCH=x86_64 ;;
  *)
    printf '错误：不支持的 macOS 架构 %s，只接受 aarch64 或 x86_64。\n' "$MACOS_ARCH" >&2
    exit 2
    ;;
esac
MACOS_RESOURCE_DIR=macos-$MACOS_ARCH

SOURCE_DIRECTORY=$(dirname "$SOURCE_ARCHIVE")
SOURCE_NAME=$(basename "$SOURCE_ARCHIVE")
SOURCE_ARCHIVE=$(cd "$SOURCE_DIRECTORY" && pwd)/$SOURCE_NAME
OUTPUT_PARENT=$(dirname "$OUTPUT_ROOT")
OUTPUT_NAME=$(basename "$OUTPUT_ROOT")
mkdir -p "$OUTPUT_PARENT"
OUTPUT_ROOT=$(cd "$OUTPUT_PARENT" && pwd)/$OUTPUT_NAME

for tool in shasum tar make clang install_name_tool otool codesign lipo rg; do
  command -v "$tool" >/dev/null 2>&1 || {
    printf '错误：缺少构建工具 %s。\n' "$tool" >&2
    exit 1
  }
done
if [ "$MACOS_ARCH" = x86_64 ] && ! command -v nasm >/dev/null 2>&1; then
  printf '%s\n' '错误：构建 macOS Intel FFmpeg 需要 nasm，请先安装后重试。' >&2
  exit 1
fi

ACTUAL_SHA256=$(shasum -a 256 "$SOURCE_ARCHIVE" | awk '{print $1}')
case "$ACTUAL_SHA256" in
  "$RELEASE_SHA256") SOURCE_ROOT_NAME=ffmpeg-8.1.2 ;;
  "$GITHUB_TAG_SHA256") SOURCE_ROOT_NAME=FFmpeg-n8.1.2 ;;
  *)
    printf '%s\n' '错误：FFmpeg 源码 SHA-256 与锁定值不一致。' >&2
    exit 1
    ;;
esac

if [ -e "$OUTPUT_ROOT" ]; then
  printf '%s\n' '错误：输出目录已经存在，请使用一个新的目录。' >&2
  exit 1
fi

TEMP_BASE=${TMPDIR:-/tmp}
TEMP_BASE=${TEMP_BASE%/}
BUILD_ROOT=$(mktemp -d "$TEMP_BASE/dy-screen-ffmpeg.XXXXXX")
trap 'rm -rf "$BUILD_ROOT"' EXIT HUP INT TERM
tar -xf "$SOURCE_ARCHIVE" -C "$BUILD_ROOT"
SOURCE_ROOT="$BUILD_ROOT/$SOURCE_ROOT_NAME"
INSTALL_ROOT="$BUILD_ROOT/install"

cd "$SOURCE_ROOT"
./configure \
  --prefix="$INSTALL_ROOT" \
  --arch="$APPLE_ARCH" \
  --cc=clang \
  --extra-cflags="-arch $APPLE_ARCH -mmacosx-version-min=12.0" \
  --extra-ldflags="-arch $APPLE_ARCH -mmacosx-version-min=12.0" \
  --disable-autodetect \
  --disable-doc \
  --disable-debug \
  --disable-everything \
  --enable-shared \
  --disable-static \
  --enable-pic \
  --enable-ffmpeg \
  --enable-ffprobe \
  --enable-avcodec \
  --enable-avformat \
  --enable-avfilter \
  --enable-swresample \
  --enable-swscale \
  --enable-network \
  --enable-securetransport \
  --enable-zlib \
  --enable-videotoolbox \
  --enable-protocol=file,pipe,http,https,tcp,tls,crypto,httpproxy \
  --enable-demuxer=mov,matroska,flv,hls,mpegts,mp3,wav,ogg,flac,aac,concat,image2 \
  --enable-parser=aac,aac_latm,ac3,av1,flac,h264,hevc,mjpeg,mpeg4video,mpegaudio,opus,vorbis,vp8,vp9 \
  --enable-decoder=aac,aac_fixed,alac,flac,mp3,mp3float,opus,vorbis,ac3,eac3,pcm_s16le,pcm_s24le,pcm_s32le,pcm_f32le,h264,hevc,av1,vp8,vp9,mpeg4,mjpeg,png \
  --enable-encoder=pcm_s16le,aac,h264_videotoolbox,mjpeg \
  --enable-muxer=wav,segment,matroska,mov,mp4,image2 \
  --enable-filter=aresample,aformat,anull,asetpts,scale,format,setpts,pad,setsar,volume,fade,concat,overlay \
  --enable-bsf=aac_adtstoasc,h264_mp4toannexb,hevc_mp4toannexb

JOBS=$(sysctl -n hw.ncpu 2>/dev/null || getconf _NPROCESSORS_ONLN 2>/dev/null || printf '%s' 4)
make -j"$JOBS" install

BIN_ROOT="$OUTPUT_ROOT/bin/$MACOS_RESOURCE_DIR"
LIB_ROOT="$OUTPUT_ROOT/lib/$MACOS_RESOURCE_DIR"
LICENSE_ROOT="$OUTPUT_ROOT/licenses"
mkdir -p "$BIN_ROOT" "$LIB_ROOT" "$LICENSE_ROOT"
cp "$INSTALL_ROOT/bin/ffmpeg" "$BIN_ROOT/ffmpeg"
cp "$INSTALL_ROOT/bin/ffprobe" "$BIN_ROOT/ffprobe"
cp "$INSTALL_ROOT/lib/"*.dylib "$LIB_ROOT/"
cp "$SOURCE_ROOT/COPYING.LGPLv2.1" "$LICENSE_ROOT/FFmpeg-LGPL-2.1.txt"

# 只保留带主版本号的真实 dylib，移除安装目录中的无版本别名。
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

for library in "$LIB_ROOT/"*.dylib; do
  case "$(basename "$library")" in
    libavcodec.62.dylib|libavdevice.62.dylib|libavfilter.11.dylib|libavformat.62.dylib|libavutil.60.dylib|libswresample.6.dylib|libswscale.9.dylib)
      ;;
    *)
      rm "$library"
      ;;
  esac
done

for library in "$LIB_ROOT/"*.dylib; do
  library_name=$(basename "$library")
  install_name_tool -id "@rpath/$library_name" "$library"
  otool -L "$library" | awk -v prefix="$INSTALL_ROOT/lib/" 'index($1, prefix) == 1 {print $1}' | while IFS= read -r dependency; do
    install_name_tool -change "$dependency" "@loader_path/$(basename "$dependency")" "$library"
  done
  codesign --force --sign - "$library"
  assert_macho_architecture "$library"
  assert_macos_deployment_target "$library"
done

for binary in "$BIN_ROOT/ffmpeg" "$BIN_ROOT/ffprobe"; do
  otool -L "$binary" | awk -v prefix="$INSTALL_ROOT/lib/" 'index($1, prefix) == 1 {print $1}' | while IFS= read -r dependency; do
    install_name_tool -change "$dependency" "@executable_path/../../lib/$MACOS_RESOURCE_DIR/$(basename "$dependency")" "$binary"
  done
  codesign --force --sign - "$binary"
  assert_macho_architecture "$binary"
  assert_macos_deployment_target "$binary"
done

if otool -L "$BIN_ROOT/ffmpeg" "$BIN_ROOT/ffprobe" "$LIB_ROOT/"*.dylib | rg -F "$INSTALL_ROOT"; then
  printf '%s\n' '错误：FFmpeg 产物仍引用构建临时目录。' >&2
  exit 1
fi

TARGET_RUNNER=
if [ "$(uname -m)" = arm64 ] && [ "$MACOS_ARCH" = x86_64 ]; then
  if arch -x86_64 /usr/bin/true >/dev/null 2>&1; then
    TARGET_RUNNER='arch -x86_64'
  else
    printf '%s\n' '提示：当前 Apple Silicon 未安装 Rosetta，跳过目标二进制运行能力检查；Intel 真机发行验收仍为必需。'
  fi
fi

run_target() {
  if [ -n "$TARGET_RUNNER" ]; then
    arch -x86_64 "$@"
  elif [ "$(uname -m)" = "$APPLE_ARCH" ]; then
    "$@"
  else
    return 126
  fi
}

RUNTIME_CHECKED=false
if run_target "$BIN_ROOT/ffmpeg" -version > "$OUTPUT_ROOT/ffmpeg-version.txt" &&
  run_target "$BIN_ROOT/ffprobe" -version > "$OUTPUT_ROOT/ffprobe-version.txt"; then
  RUNTIME_CHECKED=true
  cat "$OUTPUT_ROOT/ffmpeg-version.txt"
  cat "$OUTPUT_ROOT/ffprobe-version.txt"
else
  printf 'target=%s; runtime validation deferred to target Mac\n' "$MACOS_ARCH" > "$OUTPUT_ROOT/ffmpeg-version.txt"
  printf 'target=%s; runtime validation deferred to target Mac\n' "$MACOS_ARCH" > "$OUTPUT_ROOT/ffprobe-version.txt"
fi

if rg -q -- '--enable-(gpl|nonfree)' "$OUTPUT_ROOT/ffmpeg-version.txt"; then
  printf '%s\n' '错误：ASR FFmpeg 构建意外启用了 GPL 或 nonfree。' >&2
  exit 1
fi

if [ "$RUNTIME_CHECKED" = true ]; then
PROTOCOLS_OUTPUT=$(run_target "$BIN_ROOT/ffmpeg" -hide_banner -protocols 2>&1)
DEMUXERS_OUTPUT=$(run_target "$BIN_ROOT/ffmpeg" -hide_banner -demuxers 2>&1)
MUXERS_OUTPUT=$(run_target "$BIN_ROOT/ffmpeg" -hide_banner -muxers 2>&1)
ENCODERS_OUTPUT=$(run_target "$BIN_ROOT/ffmpeg" -hide_banner -encoders 2>&1)
DECODERS_OUTPUT=$(run_target "$BIN_ROOT/ffmpeg" -hide_banner -decoders 2>&1)
FILTERS_OUTPUT=$(run_target "$BIN_ROOT/ffmpeg" -hide_banner -filters 2>&1)

for protocol in http https tcp tls; do
  printf '%s\n' "$PROTOCOLS_OUTPUT" | rg -q "^  $protocol$" || {
    printf '错误：运行时 FFmpeg 缺少 %s 协议。\n' "$protocol" >&2
    exit 1
  }
done
for demuxer in concat flv hls image2 matroska mov mpegts; do
  printf '%s\n' "$DEMUXERS_OUTPUT" | rg -q "^ D +$demuxer( |,)" || {
    printf '错误：运行时 FFmpeg 缺少 %s demuxer。\n' "$demuxer" >&2
    exit 1
  }
done
for decoder in aac h264 hevc png; do
  printf '%s\n' "$DECODERS_OUTPUT" | rg -q "^ [VAS][A-Z.]{5} $decoder +" || {
    printf '错误：运行时 FFmpeg 缺少 %s decoder。\n' "$decoder" >&2
    exit 1
  }
done
for muxer in segment matroska mov mp4 image2 wav; do
  printf '%s\n' "$MUXERS_OUTPUT" | rg -q "^  E +$muxer( |,)" || {
    printf '错误：运行时 FFmpeg 缺少 %s muxer。\n' "$muxer" >&2
    exit 1
  }
done
for encoder in aac h264_videotoolbox mjpeg pcm_s16le; do
  printf '%s\n' "$ENCODERS_OUTPUT" | rg -q "^ [VAS][A-Z.]{5} $encoder +" || {
    printf '错误：运行时 FFmpeg 缺少 %s encoder。\n' "$encoder" >&2
    exit 1
  }
done
for filter in aformat asetpts concat fade format overlay pad scale setsar setpts volume; do
  printf '%s\n' "$FILTERS_OUTPUT" | rg -q "^ [A-Z.]+ +$filter +" || {
    printf '错误：运行时 FFmpeg 缺少 %s filter。\n' "$filter" >&2
    exit 1
  }
done
fi

printf '%s\n' \
  "source=$SOURCE_NAME" \
  "sha256=$ACTUAL_SHA256" \
  "architecture=$MACOS_ARCH" \
  "apple_architecture=$APPLE_ARCH" \
  "runtime_validation=$RUNTIME_CHECKED" \
  'license=LGPL-2.1-or-later' \
  'network=enabled-for-recording' \
  'recording=https-flv-hls-segment-matroska' \
  'preview=mov-h264-hevc-aac-h264_videotoolbox-mjpeg' \
  'clip-export=h264-hevc-aac-decode-h264_videotoolbox-aac-mp4-png-concat-overlay' \
  > "$OUTPUT_ROOT/build-record.txt"

printf 'macOS %s 应用运行时 FFmpeg 已构建到：%s\n' "$MACOS_ARCH" "$OUTPUT_ROOT"
