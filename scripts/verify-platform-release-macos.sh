#!/bin/sh
set -eu

if [ "$#" -lt 2 ] || [ "$#" -gt 3 ]; then
  printf '%s\n' '用法：verify-platform-release-macos.sh <应用.app> <证据.json> [cargo]' >&2
  exit 2
fi

APP_ROOT=$1
OUTPUT=$2
CARGO=${3:-cargo}
SCRIPT_ROOT=$(CDPATH= cd -- "$(dirname "$0")" && pwd)
REPO_ROOT=$(dirname "$SCRIPT_ROOT")

if [ "$(uname -s)" != Darwin ] || [ "$(uname -m)" != x86_64 ]; then
  printf '%s\n' '错误：macOS Intel 发行验收只能在原生 x86_64 Mac 执行，禁止 Rosetta 或交叉目标替代。' >&2
  exit 1
fi
if [ "$(sysctl -in sysctl.proc_translated 2>/dev/null || printf '0')" = 1 ]; then
  printf '%s\n' '错误：检测到 Rosetta 转译进程，不能作为原生 macOS Intel 发行证据。' >&2
  exit 1
fi
for tool in jq shasum lipo otool plutil "$CARGO"; do
  command -v "$tool" >/dev/null 2>&1 || {
    printf '错误：macOS Intel 发行验收缺少工具 %s。\n' "$tool" >&2
    exit 1
  }
done
if [ ! -d "$APP_ROOT" ] || [ -L "$APP_ROOT" ]; then
  printf '%s\n' '错误：应用包缺失或是符号链接。' >&2
  exit 1
fi
if [ -e "$OUTPUT" ]; then
  printf '%s\n' '错误：平台发行证据已经存在，不会覆盖。' >&2
  exit 1
fi

RESOURCE_ROOT="$APP_ROOT/Contents/Resources/resources/asr"
MANIFEST="$RESOURCE_ROOT/manifest.json"
RUNTIME_MANIFEST="$RESOURCE_ROOT/runtime-manifest.json"
if [ ! -f "$MANIFEST" ] || [ -L "$MANIFEST" ] || [ ! -f "$RUNTIME_MANIFEST" ] || [ -L "$RUNTIME_MANIFEST" ]; then
  printf '%s\n' '错误：应用包缺少普通文件 ASR manifest 或 runtime manifest。' >&2
  exit 1
fi
if [ "$(jq -r '.platforms | length' "$MANIFEST")" -ne 1 ] ||
  [ "$(jq -r '.platforms[0].os' "$MANIFEST")" != macos ] ||
  [ "$(jq -r '.platforms[0].arch' "$MANIFEST")" != x86_64 ]; then
  printf '%s\n' '错误：应用包不是单平台 macOS x86_64 资源包。' >&2
  exit 1
fi

OUTPUT_PARENT=$(dirname "$OUTPUT")
OUTPUT_NAME=$(basename "$OUTPUT")
mkdir -p "$OUTPUT_PARENT"
OUTPUT_PARENT=$(CDPATH= cd -- "$OUTPUT_PARENT" && pwd)
OUTPUT="$OUTPUT_PARENT/$OUTPUT_NAME"
PART="$OUTPUT.part"
LOG_ROOT="$OUTPUT_PARENT/${OUTPUT_NAME%.json}-logs"
if [ -e "$PART" ] || [ -e "$LOG_ROOT" ]; then
  printf '%s\n' '错误：存在未清理的平台发行证据临时文件。' >&2
  exit 1
fi
mkdir "$LOG_ROOT"
trap 'rm -f "$PART"' EXIT HUP INT TERM

run_step() {
  STEP_NAME=$1
  shift
  STEP_LOG="$LOG_ROOT/$STEP_NAME.log"
  STEP_STARTED=$(date +%s)
  if "$@" >"$STEP_LOG" 2>&1; then
    STEP_EXIT=0
  else
    STEP_EXIT=$?
  fi
  STEP_FINISHED=$(date +%s)
  STEP_SHA=$(shasum -a 256 "$STEP_LOG" | awk '{print $1}')
  jq -n \
    --arg name "$STEP_NAME" \
    --argjson exitCode "$STEP_EXIT" \
    --argjson wallMs "$(((STEP_FINISHED - STEP_STARTED) * 1000))" \
    --arg outputSha256 "$STEP_SHA" \
    '{name:$name,exitCode:$exitCode,wallMs:$wallMs,outputSha256:$outputSha256}' \
    >"$LOG_ROOT/$STEP_NAME.json"
  if [ "$STEP_EXIT" -ne 0 ]; then
    printf '错误：macOS Intel 平台验收步骤失败：%s。日志：%s\n' "$STEP_NAME" "$STEP_LOG" >&2
    return "$STEP_EXIT"
  fi
}

FFMPEG_RELATIVE=$(jq -r '.platforms[0].ffmpeg' "$MANIFEST")
FFPROBE_RELATIVE=$(jq -r '.platforms[0].ffprobe' "$MANIFEST")
WHISPER_RELATIVE=$(jq -r '.platforms[0].sidecar' "$MANIFEST")
VAD_RELATIVE=$(jq -r '.platforms[0].vadSidecar' "$MANIFEST")
FFMPEG="$RESOURCE_ROOT/$FFMPEG_RELATIVE"
FFPROBE="$RESOURCE_ROOT/$FFPROBE_RELATIVE"

cd "$REPO_ROOT"
run_step architecture "$SCRIPT_ROOT/verify-macos-bundle-architecture.sh" "$APP_ROOT" x86_64
run_step resource-integrity "$CARGO" run --offline --bin asr-bundle -- verify \
  --root "$RESOURCE_ROOT" --platform macos-x86-64
run_step ffmpeg-capabilities "$SCRIPT_ROOT/verify-clip-ffmpeg-capabilities.sh" "$FFMPEG"
run_step vad env ASR_RESOURCE_ROOT="$RESOURCE_ROOT" "$CARGO" test --offline \
  --test asr_vad_integration real_silero_vad_detects_speech_and_rejects_silence_and_music \
  -- --ignored --exact --nocapture
run_step whisper env ASR_RESOURCE_ROOT="$RESOURCE_ROOT" "$CARGO" test --offline \
  --test asr_whisper_integration real_whisper_engine_transcribes_fixture_and_exits_cleanly \
  -- --ignored --exact --nocapture
run_step preview env DY_SCREEN_REQUIRE_CLIP_PLATFORM_VALIDATION=1 \
  DY_SCREEN_CLIP_FFMPEG="$FFMPEG" DY_SCREEN_CLIP_FFPROBE="$FFPROBE" \
  "$CARGO" test --offline --manifest-path src-tauri/Cargo.toml --lib \
  transition_assets::tests::real_h264_and_hevc_samples_keep_audio_and_generate_compatible_preview \
  -- --exact --nocapture
run_step export env DY_SCREEN_REQUIRE_CLIP_PLATFORM_VALIDATION=1 \
  DY_SCREEN_CLIP_FFMPEG="$FFMPEG" DY_SCREEN_CLIP_FFPROBE="$FFPROBE" \
  "$CARGO" test --offline --manifest-path src-tauri/Cargo.toml --lib \
  ai::clip_export::tests::exports_mixed_source_dimensions_to_a_playable_mp4 \
  -- --exact --nocapture

INFO_PLIST="$APP_ROOT/Contents/Info.plist"
if [ ! -f "$INFO_PLIST" ] || [ -L "$INFO_PLIST" ]; then
  printf '%s\n' '错误：应用包缺少普通文件 Info.plist。' >&2
  exit 1
fi
APP_EXECUTABLE=$(plutil -extract CFBundleExecutable raw "$INFO_PLIST")
case "$APP_EXECUTABLE" in
  ''|.|..|*/*|*\\*)
    printf '%s\n' '错误：Info.plist 包含无效的 CFBundleExecutable。' >&2
    exit 1
    ;;
esac
APP_BINARY="$APP_ROOT/Contents/MacOS/$APP_EXECUTABLE"
if [ ! -f "$APP_BINARY" ] || [ -L "$APP_BINARY" ]; then
  printf '%s\n' '错误：Info.plist 指定的应用主程序缺失或不是普通文件。' >&2
  exit 1
fi
STEPS=$(jq -s '.' "$LOG_ROOT"/*.json)
COLLECTED_AT=$(date -u '+%Y-%m-%dT%H:%M:%SZ')
OS_VERSION=$(sw_vers -productVersion)
KERNEL_VERSION=$(uname -r)
APP_VERSION=$(jq -r '.version' "$REPO_ROOT/src-tauri/tauri.conf.json")
BUNDLE_VERSION=$(jq -r '.bundleVersion' "$MANIFEST")

jq -n \
  --arg collectedAtUtc "$COLLECTED_AT" \
  --arg osVersion "$OS_VERSION" \
  --arg kernelVersion "$KERNEL_VERSION" \
  --arg appVersion "$APP_VERSION" \
  --arg bundleVersion "$BUNDLE_VERSION" \
  --arg appBinarySha256 "$(shasum -a 256 "$APP_BINARY" | awk '{print $1}')" \
  --arg resourceManifestSha256 "$(shasum -a 256 "$MANIFEST" | awk '{print $1}')" \
  --arg runtimeManifestSha256 "$(shasum -a 256 "$RUNTIME_MANIFEST" | awk '{print $1}')" \
  --arg ffmpegSha256 "$(shasum -a 256 "$FFMPEG" | awk '{print $1}')" \
  --arg ffprobeSha256 "$(shasum -a 256 "$FFPROBE" | awk '{print $1}')" \
  --arg whisperSha256 "$(shasum -a 256 "$RESOURCE_ROOT/$WHISPER_RELATIVE" | awk '{print $1}')" \
  --arg vadSidecarSha256 "$(shasum -a 256 "$RESOURCE_ROOT/$VAD_RELATIVE" | awk '{print $1}')" \
  --arg modelSha256 "$(shasum -a 256 "$RESOURCE_ROOT/$(jq -r '.model.file' "$MANIFEST")" | awk '{print $1}')" \
  --arg vadModelSha256 "$(shasum -a 256 "$RESOURCE_ROOT/$(jq -r '.vad.file' "$MANIFEST")" | awk '{print $1}')" \
  --argjson steps "$STEPS" \
  '{schemaVersion:1,collectedAtUtc:$collectedAtUtc,platform:"macos",architecture:"x86_64",nativeHost:true,osVersion:$osVersion,kernelVersion:$kernelVersion,appVersion:$appVersion,bundleVersion:$bundleVersion,appBinarySha256:$appBinarySha256,resourceManifestSha256:$resourceManifestSha256,runtimeManifestSha256:$runtimeManifestSha256,ffmpegSha256:$ffmpegSha256,ffprobeSha256:$ffprobeSha256,whisperSha256:$whisperSha256,vadSidecarSha256:$vadSidecarSha256,modelSha256:$modelSha256,vadModelSha256:$vadModelSha256,steps:$steps,allPassed:true}' \
  >"$PART"
mv "$PART" "$OUTPUT"
trap - EXIT HUP INT TERM
printf 'macOS Intel 原生发行验收通过：%s\n' "$OUTPUT"
