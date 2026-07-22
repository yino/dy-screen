#!/bin/sh
# 在 macOS arm64 目标设备上采集一次完整 ASR 的可审计性能证据。
# 证据只保存哈希、硬件摘要和数值指标，不保存视频/资源路径或模型 stderr 原文。

set -eu

usage() {
  printf '%s\n' \
    '用法：collect-asr-performance-macos.sh --video FILE --asr-binary FILE --resource-root DIR --output FILE [--require-8gb]' >&2
}

VIDEO=
ASR_BINARY=
RESOURCE_ROOT=
OUTPUT=
REQUIRE_8GB=false

while [ "$#" -gt 0 ]; do
  case "$1" in
    --video) VIDEO=${2-}; shift 2 ;;
    --asr-binary) ASR_BINARY=${2-}; shift 2 ;;
    --resource-root) RESOURCE_ROOT=${2-}; shift 2 ;;
    --output) OUTPUT=${2-}; shift 2 ;;
    --require-8gb) REQUIRE_8GB=true; shift ;;
    -h|--help) usage; exit 0 ;;
    *) usage; exit 2 ;;
  esac
done

if [ -z "$VIDEO" ] || [ -z "$ASR_BINARY" ] || [ -z "$RESOURCE_ROOT" ] || [ -z "$OUTPUT" ]; then
  usage
  exit 2
fi
if [ "$(uname -s)" != Darwin ] || [ "$(uname -m)" != arm64 ]; then
  printf '%s\n' '错误：该脚本只接受 macOS arm64 目标设备。' >&2
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

thermal_state() {
  THERMAL_OUTPUT=$(pmset -g therm 2>/dev/null || true)
  case "$(printf '%s' "$THERMAL_OUTPUT" | tr '[:upper:]' '[:lower:]')" in
    *'no thermal warning'*) printf '%s' normal ;;
    *critical*) printf '%s' critical ;;
    *warning*) printf '%s' warning ;;
    *) printf '%s' unknown ;;
  esac
}

physical_memory_bytes() {
  MEMORY=$(sysctl -n hw.memsize 2>/dev/null || true)
  case "$MEMORY" in
    ''|*[!0-9]*) ;;
    *) printf '%s' "$MEMORY"; return ;;
  esac
  system_profiler SPHardwareDataType 2>/dev/null | awk '
    /Memory:/ {
      value=$2; unit=$3;
      if (unit == "GB") printf "%.0f", value * 1024 * 1024 * 1024;
      else if (unit == "MB") printf "%.0f", value * 1024 * 1024;
      exit;
    }
  '
}

descendants() {
  PARENT=$1
  printf '%s\n' "$PARENT"
  for CHILD in $(pgrep -P "$PARENT" 2>/dev/null || true); do
    descendants "$CHILD"
  done
}

remember_pid() {
  case " $OBSERVED_PIDS " in
    *" $1 "*) ;;
    *) OBSERVED_PIDS="$OBSERVED_PIDS $1" ;;
  esac
}

require_regular_file "$VIDEO" '视频'
require_regular_file "$ASR_BINARY" 'ASR 可执行文件'
require_regular_file "$RESOURCE_ROOT/manifest.json" '资源 manifest'
if [ -e "$OUTPUT" ] || [ -L "$OUTPUT" ]; then
  printf '%s\n' '错误：性能证据文件已存在；为保留不可变证据不会覆盖。' >&2
  exit 2
fi

PHYSICAL_MEMORY_BYTES=$(physical_memory_bytes)
case "$PHYSICAL_MEMORY_BYTES" in
  ''|*[!0-9]*) printf '%s\n' '错误：无法读取物理内存。' >&2; exit 2 ;;
esac
EIGHT_GIB_MIN=$((7 * 1024 * 1024 * 1024))
EIGHT_GIB_MAX=$((9 * 1024 * 1024 * 1024))
STRICT_GATE_PASSED=false
if [ "$PHYSICAL_MEMORY_BYTES" -ge "$EIGHT_GIB_MIN" ] && [ "$PHYSICAL_MEMORY_BYTES" -le "$EIGHT_GIB_MAX" ]; then
  STRICT_GATE_PASSED=true
fi
if [ "$REQUIRE_8GB" = true ] && [ "$STRICT_GATE_PASSED" != true ]; then
  printf '错误：严格 8 GB 门禁失败，当前物理内存为 %s 字节。\n' "$PHYSICAL_MEMORY_BYTES" >&2
  exit 2
fi

OUTPUT_PARENT=$(dirname "$OUTPUT")
mkdir -p "$OUTPUT_PARENT"
if [ -L "$OUTPUT_PARENT" ] || [ ! -d "$OUTPUT_PARENT" ]; then
  printf '%s\n' '错误：输出目录必须是普通目录。' >&2
  exit 2
fi

WORK_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/dy-screen-asr-performance.XXXXXX")
OUTPUT_TEMP=
cleanup() {
  rm -rf "$WORK_ROOT"
  if [ -n "$OUTPUT_TEMP" ] && [ -f "$OUTPUT_TEMP" ]; then
    rm -f "$OUTPUT_TEMP"
  fi
}
trap cleanup EXIT HUP INT TERM

STDOUT_FILE="$WORK_ROOT/stdout.json"
STDERR_FILE="$WORK_ROOT/stderr.txt"
INPUT_SIZE_BEFORE=$(stat -f '%z' "$VIDEO")
INPUT_SHA256_BEFORE=$(hash_file "$VIDEO")
ASR_BINARY_SHA256=$(hash_file "$ASR_BINARY")
RESOURCE_MANIFEST_SHA256=$(hash_file "$RESOURCE_ROOT/manifest.json")
THERMAL_BEFORE=$(thermal_state)

set +e
/usr/bin/time -l "$ASR_BINARY" asr "$VIDEO" --resource-root "$RESOURCE_ROOT" --json >"$STDOUT_FILE" 2>"$STDERR_FILE" &
ROOT_PID=$!
set -e

OBSERVED_PIDS=
PEAK_RESIDENT_SET_BYTES=0
MAXIMUM_THREAD_COUNT=0
MAXIMUM_PROCESS_COUNT=0
while kill -0 "$ROOT_PID" 2>/dev/null; do
  CURRENT_PIDS=$(descendants "$ROOT_PID" | awk '!seen[$0]++')
  CURRENT_RSS_KIB=0
  CURRENT_THREADS=0
  CURRENT_PROCESS_COUNT=0
  for PID in $CURRENT_PIDS; do
    remember_pid "$PID"
    RSS_KIB=$(ps -o rss= -p "$PID" 2>/dev/null | awk 'NR == 1 {print $1 + 0}' || true)
    THREADS=$(ps -M -p "$PID" 2>/dev/null | awk 'NR > 1 {count++} END {print count + 0}' || true)
    RSS_KIB=${RSS_KIB:-0}
    THREADS=${THREADS:-0}
    CURRENT_RSS_KIB=$((CURRENT_RSS_KIB + RSS_KIB))
    CURRENT_THREADS=$((CURRENT_THREADS + THREADS))
    CURRENT_PROCESS_COUNT=$((CURRENT_PROCESS_COUNT + 1))
  done
  CURRENT_RSS_BYTES=$((CURRENT_RSS_KIB * 1024))
  [ "$CURRENT_RSS_BYTES" -le "$PEAK_RESIDENT_SET_BYTES" ] || PEAK_RESIDENT_SET_BYTES=$CURRENT_RSS_BYTES
  [ "$CURRENT_THREADS" -le "$MAXIMUM_THREAD_COUNT" ] || MAXIMUM_THREAD_COUNT=$CURRENT_THREADS
  [ "$CURRENT_PROCESS_COUNT" -le "$MAXIMUM_PROCESS_COUNT" ] || MAXIMUM_PROCESS_COUNT=$CURRENT_PROCESS_COUNT
  sleep 0.1
done

set +e
wait "$ROOT_PID"
EXIT_CODE=$?
set -e
REAL_SECONDS=$(awk '/^[[:space:]]*[0-9.]+ real[[:space:]]/ {print $1; exit}' "$STDERR_FILE" 2>/dev/null || true)
case "$REAL_SECONDS" in
  ''|*[!0-9.]*) WALL_MS=0 ;;
  *) WALL_MS=$(awk -v seconds="$REAL_SECONDS" 'BEGIN {printf "%.0f", seconds * 1000}') ;;
esac

TIME_PEAK=$(awk '/maximum resident set size/ {print $1; exit}' "$STDERR_FILE" 2>/dev/null || true)
case "$TIME_PEAK" in
  ''|*[!0-9]*) ;;
  *) [ "$TIME_PEAK" -le "$PEAK_RESIDENT_SET_BYTES" ] || PEAK_RESIDENT_SET_BYTES=$TIME_PEAK ;;
esac

STDOUT_VALID_JSON=false
AUDIO_DURATION_MS=null
DURATION=$(/usr/bin/plutil -extract durationMs raw -o - -- "$STDOUT_FILE" 2>/dev/null || true)
case "$DURATION" in
  ''|*[!0-9]*) ;;
  *) STDOUT_VALID_JSON=true; AUDIO_DURATION_MS=$DURATION ;;
esac
REALTIME_FACTOR=null
if [ "$AUDIO_DURATION_MS" != null ] && [ "$AUDIO_DURATION_MS" -gt 0 ]; then
  REALTIME_FACTOR=$(awk -v wall="$WALL_MS" -v duration="$AUDIO_DURATION_MS" 'BEGIN {printf "%.6f", wall / duration}')
fi

INPUT_SIZE_AFTER=null
INPUT_SHA256_AFTER=null
SOURCE_UNCHANGED=false
if [ -f "$VIDEO" ] && [ ! -L "$VIDEO" ]; then
  INPUT_SIZE_AFTER_VALUE=$(stat -f '%z' "$VIDEO")
  INPUT_SHA256_AFTER_VALUE=$(hash_file "$VIDEO")
  INPUT_SIZE_AFTER=$INPUT_SIZE_AFTER_VALUE
  INPUT_SHA256_AFTER="\"$INPUT_SHA256_AFTER_VALUE\""
  if [ "$INPUT_SIZE_AFTER_VALUE" = "$INPUT_SIZE_BEFORE" ] && [ "$INPUT_SHA256_AFTER_VALUE" = "$INPUT_SHA256_BEFORE" ]; then
    SOURCE_UNCHANGED=true
  fi
fi

sleep 2
LINGERING_CHILD_PROCESS_COUNT=0
for PID in $OBSERVED_PIDS; do
  if [ "$PID" != "$ROOT_PID" ] && kill -0 "$PID" 2>/dev/null; then
    LINGERING_CHILD_PROCESS_COUNT=$((LINGERING_CHILD_PROCESS_COUNT + 1))
  fi
done
RESOURCES_RELEASED=false
[ "$LINGERING_CHILD_PROCESS_COUNT" -ne 0 ] || RESOURCES_RELEASED=true
THERMAL_AFTER=$(thermal_state)

STDOUT_SIZE_BYTES=$(stat -f '%z' "$STDOUT_FILE")
STDERR_SIZE_BYTES=$(stat -f '%z' "$STDERR_FILE")
STDOUT_SHA256=$(hash_file "$STDOUT_FILE")
STDERR_SHA256=$(hash_file "$STDERR_FILE")
COLLECTED_AT_UTC=$(date -u '+%Y-%m-%dT%H:%M:%SZ')
OUTPUT_TEMP="$OUTPUT_PARENT/.$(basename "$OUTPUT").$$.part"

printf '%s\n' \
  '{' \
  '  "schemaVersion": 1,' \
  "  \"collectedAtUtc\": \"$COLLECTED_AT_UTC\"," \
  '  "platform": "macos",' \
  '  "architecture": "arm64",' \
  "  \"physicalMemoryBytes\": $PHYSICAL_MEMORY_BYTES," \
  "  \"strict8GbDeviceRequired\": $REQUIRE_8GB," \
  "  \"strict8GbDeviceGatePassed\": $STRICT_GATE_PASSED," \
  "  \"inputSizeBytesBefore\": $INPUT_SIZE_BEFORE," \
  "  \"inputSizeBytesAfter\": $INPUT_SIZE_AFTER," \
  "  \"inputSha256Before\": \"$INPUT_SHA256_BEFORE\"," \
  "  \"inputSha256After\": $INPUT_SHA256_AFTER," \
  "  \"sourceUnchanged\": $SOURCE_UNCHANGED," \
  "  \"asrBinarySha256\": \"$ASR_BINARY_SHA256\"," \
  "  \"resourceManifestSha256\": \"$RESOURCE_MANIFEST_SHA256\"," \
  "  \"exitCode\": $EXIT_CODE," \
  "  \"wallMs\": $WALL_MS," \
  "  \"audioDurationMs\": $AUDIO_DURATION_MS," \
  "  \"realtimeFactor\": $REALTIME_FACTOR," \
  "  \"peakResidentSetBytes\": $PEAK_RESIDENT_SET_BYTES," \
  "  \"maximumThreadCount\": $MAXIMUM_THREAD_COUNT," \
  "  \"maximumProcessCount\": $MAXIMUM_PROCESS_COUNT," \
  "  \"thermalStateBefore\": \"$THERMAL_BEFORE\"," \
  "  \"thermalStateAfter\": \"$THERMAL_AFTER\"," \
  "  \"lingeringChildProcessCount\": $LINGERING_CHILD_PROCESS_COUNT," \
  "  \"resourcesReleased\": $RESOURCES_RELEASED," \
  "  \"stdoutValidJson\": $STDOUT_VALID_JSON," \
  "  \"stdoutSizeBytes\": $STDOUT_SIZE_BYTES," \
  "  \"stdoutSha256\": \"$STDOUT_SHA256\"," \
  "  \"stderrSizeBytes\": $STDERR_SIZE_BYTES," \
  "  \"stderrSha256\": \"$STDERR_SHA256\"" \
  '}' >"$OUTPUT_TEMP"

chmod 600 "$OUTPUT_TEMP"
mv "$OUTPUT_TEMP" "$OUTPUT"
printf 'macOS ASR 性能证据已生成：exit=%s，RTF=%s，峰值内存=%s 字节，遗留子进程=%s\n' \
  "$EXIT_CODE" "$REALTIME_FACTOR" "$PEAK_RESIDENT_SET_BYTES" "$LINGERING_CHILD_PROCESS_COUNT"

if [ "$EXIT_CODE" -ne 0 ] || [ "$STDOUT_VALID_JSON" != true ] || [ "$SOURCE_UNCHANGED" != true ] || [ "$RESOURCES_RELEASED" != true ]; then
  exit 1
fi
