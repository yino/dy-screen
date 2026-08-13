#!/bin/sh
set -eu

if [ "$#" -ne 2 ]; then
  printf '%s\n' '用法：verify-macos-bundle-architecture.sh <应用.app> <aarch64|x86_64>' >&2
  exit 2
fi

APP_ROOT=$1
MACOS_ARCH=$2
case "$MACOS_ARCH" in
  aarch64) APPLE_ARCH=arm64 ;;
  x86_64) APPLE_ARCH=x86_64 ;;
  *)
    printf '错误：不支持的 macOS 架构 %s，只接受 aarch64 或 x86_64。\n' "$MACOS_ARCH" >&2
    exit 2
    ;;
esac

for tool in jq lipo otool; do
  command -v "$tool" >/dev/null 2>&1 || {
    printf '错误：缺少安装包架构校验工具 %s。\n' "$tool" >&2
    exit 1
  }
done
if [ ! -d "$APP_ROOT/Contents/MacOS" ]; then
  printf '错误：应用包缺少 Contents/MacOS：%s\n' "$APP_ROOT" >&2
  exit 1
fi
RESOURCE_ROOT="$APP_ROOT/Contents/Resources/resources/asr"
MANIFEST="$RESOURCE_ROOT/manifest.json"
if [ ! -f "$MANIFEST" ] || [ -L "$MANIFEST" ]; then
  printf '%s\n' '错误：应用包缺少普通文件 resources/asr/manifest.json。' >&2
  exit 1
fi

assert_macho_architecture() {
  if [ ! -f "$1" ] || [ -L "$1" ]; then
    printf '错误：安装包原生资源缺失或不是普通文件：%s\n' "$1" >&2
    exit 1
  fi
  ARCHITECTURES=$(lipo -archs "$1")
  if [ "$ARCHITECTURES" != "$APPLE_ARCH" ]; then
    printf '错误：%s 的 Mach-O 架构为 %s，安装包目标要求 %s。\n' "$1" "$ARCHITECTURES" "$APPLE_ARCH" >&2
    exit 1
  fi
  MINIMUM_VERSION=$(otool -l "$1" | awk '/LC_BUILD_VERSION/{found=1; next} found && /minos/{print $2; exit}')
  if [ -z "$MINIMUM_VERSION" ]; then
    printf '错误：%s 缺少 LC_BUILD_VERSION 最低系统版本。\n' "$1" >&2
    exit 1
  fi
  if ! awk -v version="$MINIMUM_VERSION" 'BEGIN { exit !(version + 0 <= 12.0) }'; then
    printf '错误：%s 的最低 macOS 版本为 %s，高于发行基线 12.0。\n' "$1" "$MINIMUM_VERSION" >&2
    exit 1
  fi
}

APP_BINARY_COUNT=0
for binary in "$APP_ROOT/Contents/MacOS/"*; do
  [ -f "$binary" ] || continue
  assert_macho_architecture "$binary"
  APP_BINARY_COUNT=$((APP_BINARY_COUNT + 1))
done
if [ "$APP_BINARY_COUNT" -eq 0 ]; then
  printf '%s\n' '错误：应用包没有主程序。' >&2
  exit 1
fi

PLATFORM_COUNT=$(jq --arg arch "$MACOS_ARCH" '[.platforms[] | select(.os == "macos" and .arch == $arch)] | length' "$MANIFEST")
if [ "$PLATFORM_COUNT" -ne 1 ] || [ "$(jq '.platforms | length' "$MANIFEST")" -ne 1 ]; then
  printf '错误：应用包 manifest 没有且仅有 macOS %s 平台。\n' "$MACOS_ARCH" >&2
  exit 1
fi

jq -r '.platforms[0] | .sidecar, .vadSidecar, .ffmpeg, .ffprobe, (.libraries[]?)' "$MANIFEST" |
while IFS= read -r relative; do
  case "$relative" in
    ''|/*|*'..'*|*\\*)
      printf '错误：manifest 包含不安全资源路径：%s\n' "$relative" >&2
      exit 1
      ;;
  esac
  assert_macho_architecture "$RESOURCE_ROOT/$relative"
done

printf 'macOS %s 应用包架构校验通过：%s\n' "$MACOS_ARCH" "$APP_ROOT"
