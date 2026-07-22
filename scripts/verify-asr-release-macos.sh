#!/bin/sh
# 验证已安装 macOS arm64 正式发行包，并生成不含本地路径和工具原始输出的 JSON 证据。
# 只有 Developer ID、stapling、Gatekeeper、离线 ASR 和应用启动全部通过时才返回成功。

set -eu

usage() {
  printf '%s\n' \
    '用法：verify-asr-release-macos.sh --app APP --dmg DMG --asr-cli FILE --asr-bundle FILE --video FILE --output FILE --launch-app' >&2
}

APP=
DMG=
ASR_CLI=
ASR_BUNDLE=
VIDEO=
OUTPUT=
LAUNCH_APP=false

while [ "$#" -gt 0 ]; do
  case "$1" in
    --app) APP=${2-}; shift 2 ;;
    --dmg) DMG=${2-}; shift 2 ;;
    --asr-cli) ASR_CLI=${2-}; shift 2 ;;
    --asr-bundle) ASR_BUNDLE=${2-}; shift 2 ;;
    --video) VIDEO=${2-}; shift 2 ;;
    --output) OUTPUT=${2-}; shift 2 ;;
    --launch-app) LAUNCH_APP=true; shift ;;
    -h|--help) usage; exit 0 ;;
    *) usage; exit 2 ;;
  esac
done

if [ -z "$APP" ] || [ -z "$DMG" ] || [ -z "$ASR_CLI" ] || [ -z "$ASR_BUNDLE" ] || [ -z "$VIDEO" ] || [ -z "$OUTPUT" ]; then
  usage
  exit 2
fi
if [ "$(uname -s)" != Darwin ] || [ "$(uname -m)" != arm64 ]; then
  printf '%s\n' '错误：该脚本只接受 macOS arm64 正式发行目标机。' >&2
  exit 2
fi

require_regular_file() {
  if [ ! -f "$1" ] || [ -L "$1" ]; then
    printf '错误：%s必须是普通文件，不能是符号链接。\n' "$2" >&2
    exit 2
  fi
}

hash_file() {
  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 -- "$1" | awk '{print $1}'
  else
    openssl dgst -sha256 "$1" | awk '{print $NF}'
  fi
}

bool_command() {
  if "$@" >/dev/null 2>&1; then
    printf '%s' true
  else
    printf '%s' false
  fi
}

for TOOL in codesign spctl xcrun hdiutil file route open osascript pgrep; do
  if ! command -v "$TOOL" >/dev/null 2>&1; then
    printf '错误：缺少正式发行验收工具 %s。\n' "$TOOL" >&2
    exit 2
  fi
done
if [ ! -d "$APP" ] || [ -L "$APP" ]; then
  printf '%s\n' '错误：应用必须是普通 .app 目录。' >&2
  exit 2
fi
require_regular_file "$DMG" 'DMG'
require_regular_file "$ASR_CLI" 'ASR 阶段 CLI'
require_regular_file "$ASR_BUNDLE" 'ASR 资源校验器'
require_regular_file "$VIDEO" '固定中文视频'
if [ -e "$OUTPUT" ] || [ -L "$OUTPUT" ]; then
  printf '%s\n' '错误：macOS 发行证据已存在；为保留不可变证据不会覆盖。' >&2
  exit 2
fi

RESOURCE_ROOT="$APP/Contents/Resources/resources/asr"
INFO_PLIST="$APP/Contents/Info.plist"
require_regular_file "$RESOURCE_ROOT/manifest.json" '包内资源 manifest'
require_regular_file "$INFO_PLIST" '应用 Info.plist'
APP_EXECUTABLE=$(/usr/libexec/PlistBuddy -c 'Print:CFBundleExecutable' "$INFO_PLIST" 2>/dev/null || true)
BUNDLE_ID=$(/usr/libexec/PlistBuddy -c 'Print:CFBundleIdentifier' "$INFO_PLIST" 2>/dev/null || true)
if [ -z "$APP_EXECUTABLE" ] || [ -z "$BUNDLE_ID" ]; then
  printf '%s\n' '错误：应用缺少可执行文件名或 bundle identifier。' >&2
  exit 2
fi
MAIN_BINARY="$APP/Contents/MacOS/$APP_EXECUTABLE"
require_regular_file "$MAIN_BINARY" '应用主程序'

OUTPUT_PARENT=$(dirname "$OUTPUT")
mkdir -p "$OUTPUT_PARENT"
WORK_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/dy-screen-asr-release.XXXXXX")
OUTPUT_TEMP=
cleanup() {
  rm -rf "$WORK_ROOT"
  if [ -n "$OUTPUT_TEMP" ] && [ -f "$OUTPUT_TEMP" ]; then
    rm -f "$OUTPUT_TEMP"
  fi
}
trap cleanup EXIT HUP INT TERM

SIGN_DETAILS="$WORK_ROOT/codesign.txt"
codesign -dv --verbose=4 "$APP" >"$WORK_ROOT/codesign-stdout.txt" 2>"$SIGN_DETAILS" || true
APP_CODE_SIGNATURE_VALID=$(bool_command codesign --verify --deep --strict --verbose=2 "$APP")
DEVELOPER_ID_SIGNED=false
if grep -Eq '^Authority=Developer ID Application:' "$SIGN_DETAILS"; then
  DEVELOPER_ID_SIGNED=true
fi
HARDENED_RUNTIME=false
if grep -Eq '^CodeDirectory .*flags=.*runtime' "$SIGN_DETAILS"; then
  HARDENED_RUNTIME=true
fi

SIGNED_MACHO_COUNT=0
MACHO_COUNT=0
for CANDIDATE in "$RESOURCE_ROOT/bin/macos-aarch64/"* "$RESOURCE_ROOT/lib/macos-aarch64/"*; do
  [ -f "$CANDIDATE" ] || continue
  if file "$CANDIDATE" | grep -q 'Mach-O'; then
    MACHO_COUNT=$((MACHO_COUNT + 1))
    if codesign --verify --strict "$CANDIDATE" >/dev/null 2>&1 \
      && codesign -dv --verbose=4 "$CANDIDATE" 2>&1 | grep -Eq '^Authority=Developer ID Application:'; then
      SIGNED_MACHO_COUNT=$((SIGNED_MACHO_COUNT + 1))
    fi
  fi
done
ALL_MACHO_SIGNED=false
if [ "$MACHO_COUNT" -gt 0 ] && [ "$MACHO_COUNT" -eq "$SIGNED_MACHO_COUNT" ]; then
  ALL_MACHO_SIGNED=true
fi

APP_STAPLED=$(bool_command xcrun stapler validate "$APP")
DMG_STAPLED=$(bool_command xcrun stapler validate "$DMG")
GATEKEEPER_APP=$(bool_command spctl --assess --type execute --verbose=4 "$APP")
GATEKEEPER_DMG=$(bool_command spctl --assess --type open --context context:primary-signature --verbose=4 "$DMG")
DMG_VALID=$(bool_command hdiutil verify "$DMG")
RESOURCE_BUNDLE_VALID=$(bool_command "$ASR_BUNDLE" verify --root "$RESOURCE_ROOT" --platform macos-aarch64)

INSTALLED_UNDER_APPLICATIONS=false
case "$APP" in
  /Applications/*.app) INSTALLED_UNDER_APPLICATIONS=true ;;
esac
OFFLINE_OBSERVED=true
if route -n get default >/dev/null 2>&1 || route -n get -inet6 default >/dev/null 2>&1; then
  OFFLINE_OBSERVED=false
fi

SOURCE_SIZE_BEFORE=$(stat -f '%z' "$VIDEO")
SOURCE_SHA256_BEFORE=$(hash_file "$VIDEO")
ASR_STDOUT="$WORK_ROOT/asr.json"
ASR_STDERR="$WORK_ROOT/asr.stderr"
set +e
ASR_RESOURCE_ROOT="$RESOURCE_ROOT" "$ASR_CLI" asr "$VIDEO" --json >"$ASR_STDOUT" 2>"$ASR_STDERR"
ASR_EXIT_CODE=$?
set -e
ASR_JSON_VALID=false
ASR_TEXT_EXPECTED=false
ASR_DURATION=$(/usr/bin/plutil -extract durationMs raw -o - -- "$ASR_STDOUT" 2>/dev/null || true)
ASR_TEXT=$(/usr/bin/plutil -extract text raw -o - -- "$ASR_STDOUT" 2>/dev/null || true)
case "$ASR_DURATION" in
  ''|*[!0-9]*) ;;
  *) ASR_JSON_VALID=true ;;
esac
if printf '%s' "$ASR_TEXT" | grep -Fq '直播间' && { printf '%s' "$ASR_TEXT" | grep -Fq '99' || printf '%s' "$ASR_TEXT" | grep -Fq '九十九'; }; then
  ASR_TEXT_EXPECTED=true
fi
SOURCE_SIZE_AFTER=$(stat -f '%z' "$VIDEO")
SOURCE_SHA256_AFTER=$(hash_file "$VIDEO")
SOURCE_UNCHANGED=false
if [ "$SOURCE_SIZE_BEFORE" = "$SOURCE_SIZE_AFTER" ] && [ "$SOURCE_SHA256_BEFORE" = "$SOURCE_SHA256_AFTER" ]; then
  SOURCE_UNCHANGED=true
fi

APP_LAUNCH_PASSED=false
if [ "$LAUNCH_APP" = true ]; then
  if open -n "$APP" >/dev/null 2>&1; then
    sleep 3
    if pgrep -x "$APP_EXECUTABLE" >/dev/null 2>&1; then
      APP_LAUNCH_PASSED=true
    fi
    osascript -e "tell application id \"$BUNDLE_ID\" to quit" >/dev/null 2>&1 || true
  fi
fi

DMG_SHA256=$(hash_file "$DMG")
MAIN_BINARY_SHA256=$(hash_file "$MAIN_BINARY")
RESOURCE_MANIFEST_SHA256=$(hash_file "$RESOURCE_ROOT/manifest.json")
ASR_CLI_SHA256=$(hash_file "$ASR_CLI")
ASR_BUNDLE_SHA256=$(hash_file "$ASR_BUNDLE")
ASR_STDOUT_SHA256=$(hash_file "$ASR_STDOUT")
ASR_STDERR_SHA256=$(hash_file "$ASR_STDERR")
ASR_STDOUT_SIZE=$(stat -f '%z' "$ASR_STDOUT")
ASR_STDERR_SIZE=$(stat -f '%z' "$ASR_STDERR")

ALL_PASSED=false
if [ "$APP_CODE_SIGNATURE_VALID" = true ] \
  && [ "$DEVELOPER_ID_SIGNED" = true ] \
  && [ "$HARDENED_RUNTIME" = true ] \
  && [ "$ALL_MACHO_SIGNED" = true ] \
  && [ "$APP_STAPLED" = true ] \
  && [ "$DMG_STAPLED" = true ] \
  && [ "$GATEKEEPER_APP" = true ] \
  && [ "$GATEKEEPER_DMG" = true ] \
  && [ "$DMG_VALID" = true ] \
  && [ "$RESOURCE_BUNDLE_VALID" = true ] \
  && [ "$INSTALLED_UNDER_APPLICATIONS" = true ] \
  && [ "$OFFLINE_OBSERVED" = true ] \
  && [ "$ASR_EXIT_CODE" -eq 0 ] \
  && [ "$ASR_JSON_VALID" = true ] \
  && [ "$ASR_TEXT_EXPECTED" = true ] \
  && [ "$SOURCE_UNCHANGED" = true ] \
  && [ "$APP_LAUNCH_PASSED" = true ]; then
  ALL_PASSED=true
fi

COLLECTED_AT_UTC=$(date -u '+%Y-%m-%dT%H:%M:%SZ')
OUTPUT_TEMP="$OUTPUT_PARENT/.$(basename "$OUTPUT").$$.part"
printf '%s\n' \
  '{' \
  '  "schemaVersion": 1,' \
  "  \"collectedAtUtc\": \"$COLLECTED_AT_UTC\"," \
  '  "platform": "macos",' \
  '  "architecture": "arm64",' \
  "  \"dmgSha256\": \"$DMG_SHA256\"," \
  "  \"mainBinarySha256\": \"$MAIN_BINARY_SHA256\"," \
  "  \"resourceManifestSha256\": \"$RESOURCE_MANIFEST_SHA256\"," \
  "  \"asrCliSha256\": \"$ASR_CLI_SHA256\"," \
  "  \"asrBundleVerifierSha256\": \"$ASR_BUNDLE_SHA256\"," \
  "  \"appCodeSignatureValid\": $APP_CODE_SIGNATURE_VALID," \
  "  \"developerIdSigned\": $DEVELOPER_ID_SIGNED," \
  "  \"hardenedRuntime\": $HARDENED_RUNTIME," \
  "  \"machOCount\": $MACHO_COUNT," \
  "  \"signedMachOCount\": $SIGNED_MACHO_COUNT," \
  "  \"allMachOSigned\": $ALL_MACHO_SIGNED," \
  "  \"appStapled\": $APP_STAPLED," \
  "  \"dmgStapled\": $DMG_STAPLED," \
  "  \"gatekeeperAppPassed\": $GATEKEEPER_APP," \
  "  \"gatekeeperDmgPassed\": $GATEKEEPER_DMG," \
  "  \"dmgVerified\": $DMG_VALID," \
  "  \"resourceBundleValid\": $RESOURCE_BUNDLE_VALID," \
  "  \"installedUnderApplications\": $INSTALLED_UNDER_APPLICATIONS," \
  "  \"offlineObserved\": $OFFLINE_OBSERVED," \
  "  \"appLaunchPassed\": $APP_LAUNCH_PASSED," \
  "  \"asrExitCode\": $ASR_EXIT_CODE," \
  "  \"asrJsonValid\": $ASR_JSON_VALID," \
  "  \"asrExpectedTextPassed\": $ASR_TEXT_EXPECTED," \
  "  \"asrStdoutSizeBytes\": $ASR_STDOUT_SIZE," \
  "  \"asrStdoutSha256\": \"$ASR_STDOUT_SHA256\"," \
  "  \"asrStderrSizeBytes\": $ASR_STDERR_SIZE," \
  "  \"asrStderrSha256\": \"$ASR_STDERR_SHA256\"," \
  "  \"sourceSizeBytesBefore\": $SOURCE_SIZE_BEFORE," \
  "  \"sourceSizeBytesAfter\": $SOURCE_SIZE_AFTER," \
  "  \"sourceSha256Before\": \"$SOURCE_SHA256_BEFORE\"," \
  "  \"sourceSha256After\": \"$SOURCE_SHA256_AFTER\"," \
  "  \"sourceUnchanged\": $SOURCE_UNCHANGED," \
  "  \"allPassed\": $ALL_PASSED" \
  '}' >"$OUTPUT_TEMP"
chmod 600 "$OUTPUT_TEMP"
mv "$OUTPUT_TEMP" "$OUTPUT"
printf 'macOS 正式发行验收完成：allPassed=%s\n' "$ALL_PASSED"
if [ "$ALL_PASSED" != true ]; then
  exit 1
fi
