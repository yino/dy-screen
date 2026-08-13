#!/bin/sh
set -eu

if [ "$#" -ne 2 ]; then
  printf '%s\n' '用法：generate-tauri-macos-resource-config.sh <ASR staging 目录> <输出 JSON>' >&2
  exit 2
fi

STAGE_ROOT=$1
OUTPUT=$2
for tool in jq; do
  command -v "$tool" >/dev/null 2>&1 || {
    printf '错误：缺少 Tauri 资源配置生成工具 %s。\n' "$tool" >&2
    exit 1
  }
done
if [ ! -d "$STAGE_ROOT" ] || [ -L "$STAGE_ROOT" ] || [ ! -f "$STAGE_ROOT/runtime-manifest.json" ]; then
  printf '错误：ASR staging 目录无效：%s\n' "$STAGE_ROOT" >&2
  exit 1
fi
STAGE_ROOT=$(CDPATH= cd -- "$STAGE_ROOT" && pwd)/

OUTPUT_PARENT=$(dirname "$OUTPUT")
OUTPUT_NAME=$(basename "$OUTPUT")
mkdir -p "$OUTPUT_PARENT"
OUTPUT_PARENT=$(CDPATH= cd -- "$OUTPUT_PARENT" && pwd)
OUTPUT="$OUTPUT_PARENT/$OUTPUT_NAME"
PART="$OUTPUT.part"
if [ -L "$OUTPUT" ] || [ -e "$PART" ]; then
  printf '%s\n' '错误：Tauri 资源覆盖配置输出不安全或存在未清理临时文件。' >&2
  exit 1
fi

jq -n --arg source "$STAGE_ROOT" \
  '{"bundle":{"resources":{($source):"resources/asr/"}}}' > "$PART"
mv "$PART" "$OUTPUT"
printf 'Tauri macOS 资源配置已生成：%s\n' "$OUTPUT"
