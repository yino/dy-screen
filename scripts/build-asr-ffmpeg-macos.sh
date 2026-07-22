#!/bin/sh
set -eu

# 从锁定的 FFmpeg 官方源码构建仅服务本地 ASR 的 macOS arm64 LGPL sidecar。
# 不启用网络、GPL/nonfree 或第三方编解码库；输出包含可相对定位的 dylib 和许可证。

EXPECTED_SHA256='464beb5e7bf0c311e68b45ae2f04e9cc2af88851abb4082231742a74d97b524c'

if [ "$#" -ne 2 ]; then
  printf '%s\n' '用法：build-asr-ffmpeg-macos.sh <ffmpeg-8.1.2.tar.xz> <输出目录>' >&2
  exit 2
fi

SOURCE_ARCHIVE=$1
OUTPUT_ROOT=$2

for tool in shasum tar make clang install_name_tool otool codesign rg; do
  command -v "$tool" >/dev/null 2>&1 || {
    printf '错误：缺少构建工具 %s。\n' "$tool" >&2
    exit 1
  }
done

ACTUAL_SHA256=$(shasum -a 256 "$SOURCE_ARCHIVE" | awk '{print $1}')
if [ "$ACTUAL_SHA256" != "$EXPECTED_SHA256" ]; then
  printf '%s\n' '错误：FFmpeg 源码 SHA-256 与锁定值不一致。' >&2
  exit 1
fi

if [ -e "$OUTPUT_ROOT" ]; then
  printf '%s\n' '错误：输出目录已经存在，请使用一个新的目录。' >&2
  exit 1
fi

TEMP_BASE=${TMPDIR:-/tmp}
TEMP_BASE=${TEMP_BASE%/}
BUILD_ROOT=$(mktemp -d "$TEMP_BASE/dy-screen-ffmpeg.XXXXXX")
trap 'rm -rf "$BUILD_ROOT"' EXIT HUP INT TERM
tar -xf "$SOURCE_ARCHIVE" -C "$BUILD_ROOT"
SOURCE_ROOT="$BUILD_ROOT/ffmpeg-8.1.2"
INSTALL_ROOT="$BUILD_ROOT/install"

cd "$SOURCE_ROOT"
./configure \
  --prefix="$INSTALL_ROOT" \
  --disable-autodetect \
  --disable-doc \
  --disable-debug \
  --disable-network \
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
  --enable-protocol=file,pipe \
  --enable-demuxer=mov,matroska,flv,mpegts,mp3,wav,ogg,flac,aac \
  --enable-parser=aac,aac_latm,ac3,flac,mpegaudio,opus,vorbis \
  --enable-decoder=aac,aac_fixed,alac,flac,mp3,mp3float,opus,vorbis,ac3,eac3,pcm_s16le,pcm_s24le,pcm_s32le,pcm_f32le \
  --enable-encoder=pcm_s16le \
  --enable-muxer=wav \
  --enable-filter=aresample,aformat,anull

JOBS=$(sysctl -n hw.ncpu 2>/dev/null || getconf _NPROCESSORS_ONLN 2>/dev/null || printf '%s' 4)
make -j"$JOBS" install

BIN_ROOT="$OUTPUT_ROOT/bin/macos-aarch64"
LIB_ROOT="$OUTPUT_ROOT/lib/macos-aarch64"
LICENSE_ROOT="$OUTPUT_ROOT/licenses"
mkdir -p "$BIN_ROOT" "$LIB_ROOT" "$LICENSE_ROOT"
cp "$INSTALL_ROOT/bin/ffmpeg" "$BIN_ROOT/ffmpeg"
cp "$INSTALL_ROOT/bin/ffprobe" "$BIN_ROOT/ffprobe"
cp "$INSTALL_ROOT/lib/"*.dylib "$LIB_ROOT/"
cp "$SOURCE_ROOT/COPYING.LGPLv2.1" "$LICENSE_ROOT/FFmpeg-LGPL-2.1.txt"

# 只保留带主版本号的真实 dylib，移除安装目录中的无版本别名。
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
done

for binary in "$BIN_ROOT/ffmpeg" "$BIN_ROOT/ffprobe"; do
  otool -L "$binary" | awk -v prefix="$INSTALL_ROOT/lib/" 'index($1, prefix) == 1 {print $1}' | while IFS= read -r dependency; do
    install_name_tool -change "$dependency" "@executable_path/../../lib/macos-aarch64/$(basename "$dependency")" "$binary"
  done
  codesign --force --sign - "$binary"
done

if otool -L "$BIN_ROOT/ffmpeg" "$BIN_ROOT/ffprobe" "$LIB_ROOT/"*.dylib | rg -F "$INSTALL_ROOT"; then
  printf '%s\n' '错误：FFmpeg 产物仍引用构建临时目录。' >&2
  exit 1
fi

"$BIN_ROOT/ffmpeg" -version | tee "$OUTPUT_ROOT/ffmpeg-version.txt"
"$BIN_ROOT/ffprobe" -version | tee "$OUTPUT_ROOT/ffprobe-version.txt"

if rg -q -- '--enable-(gpl|nonfree)' "$OUTPUT_ROOT/ffmpeg-version.txt"; then
  printf '%s\n' '错误：ASR FFmpeg 构建意外启用了 GPL 或 nonfree。' >&2
  exit 1
fi

printf '%s\n' \
  "source=ffmpeg-8.1.2.tar.xz" \
  "sha256=$EXPECTED_SHA256" \
  'license=LGPL-2.1-or-later' \
  'network=disabled' \
  > "$OUTPUT_ROOT/build-record.txt"

printf 'macOS arm64 ASR FFmpeg 已构建到：%s\n' "$OUTPUT_ROOT"
