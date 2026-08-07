#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 3 ]]; then
  printf '%s\n' '用法：build-macos-dmg.sh <应用.app> <输出.dmg> <卷名称>' >&2
  exit 2
fi

app_path=$1
dmg_path=$2
volume_name=$3

if [[ "$(uname -s)" != "Darwin" ]]; then
  printf '%s\n' '错误：DMG 只能在 macOS 上构建。' >&2
  exit 2
fi
if [[ ! -d "$app_path" || "$app_path" != *.app ]]; then
  printf '错误：应用包不存在或不是 .app：%s\n' "$app_path" >&2
  exit 2
fi
if [[ "$dmg_path" != *.dmg ]]; then
  printf '错误：DMG 输出路径必须以 .dmg 结尾：%s\n' "$dmg_path" >&2
  exit 2
fi
if [[ -z "$volume_name" ]]; then
  printf '%s\n' '错误：DMG 卷名称不能为空。' >&2
  exit 2
fi

command -v hdiutil >/dev/null 2>&1 || { printf '%s\n' '错误：找不到 hdiutil。' >&2; exit 1; }
command -v ditto >/dev/null 2>&1 || { printf '%s\n' '错误：找不到 ditto。' >&2; exit 1; }

mkdir -p "$(dirname "$dmg_path")"
stage_dir=$(mktemp -d "${TMPDIR:-/tmp}/dy-screen-dmg.XXXXXX")
cleanup() {
  rm -rf -- "$stage_dir"
}
trap cleanup EXIT

ditto --rsrc --extattr "$app_path" "$stage_dir/$(basename "$app_path")"
ln -s /Applications "$stage_dir/Applications"

hdiutil create \
  -ov \
  -srcfolder "$stage_dir" \
  -volname "$volume_name" \
  -fs HFS+ \
  -format UDZO \
  "$dmg_path"
hdiutil verify "$dmg_path"

printf 'macOS DMG 已生成并通过校验：%s\n' "$dmg_path"
