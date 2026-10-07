#!/usr/bin/env bash
# 下载并校验 Node 22 LTS 运行时（macOS arm64 / Intel）。
# 用法：scripts/build-harness-runtime.sh [darwin-arm64|darwin-x64]
# 产物：resources/harness-source/runtime/<platform>/（含 bin/node、npm 等）
set -euo pipefail

PLATFORM="${1:-$(uname -s | tr '[:upper:]' '[:lower:]')-$(uname -m)}"
case "$PLATFORM" in
  darwin-arm64|darwin-x64) ;;
  *) echo "不支持的平台：$PLATFORM（本脚本只处理 macOS）" >&2; exit 2 ;;
esac

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
LOCK="$ROOT/resources/harness-source/runtime/node-lock.json"
DEST="$ROOT/resources/harness-source/runtime/$PLATFORM"

VERSION="$(python3 -c "import json,sys; print(json.load(open('$LOCK'))['version'])")"
ARCHIVE_NAME="$(python3 -c "import json,sys; print(json.load(open('$LOCK'))['platforms']['$PLATFORM']['archive'])")"
SHA256="$(python3 -c "import json,sys; print(json.load(open('$LOCK'))['platforms']['$PLATFORM']['sha256'])")"
URL="https://nodejs.org/dist/v$VERSION/$ARCHIVE_NAME"

echo "锁定 Node ${VERSION}（${PLATFORM}）"
ARCHIVE="$DEST/$ARCHIVE_NAME"

mkdir -p "$DEST"
if [ ! -f "$ARCHIVE" ]; then
  echo "下载 $URL"
  curl -fL --retry 3 --max-time 300 -o "$ARCHIVE" "$URL"
fi

echo "校验 SHA256"
ACTUAL="$(shasum -a 256 "$ARCHIVE" | awk '{print $1}')"
if [ "$ACTUAL" != "$SHA256" ]; then
  echo "哈希不匹配：期望 $SHA256，实际 $ACTUAL" >&2
  rm -f "$ARCHIVE"
  exit 1
fi

echo "解压"
tar -xzf "$ARCHIVE" -C "$DEST" --strip-components=1 "node-v$VERSION-$PLATFORM"

echo "验证：$("$DEST/bin/node" --version)"
echo "完成：$DEST"
