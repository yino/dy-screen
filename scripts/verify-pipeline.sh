#!/usr/bin/env bash
# 五步能力流水线检测：识别字幕 → 提取精彩 → 生成合辑 → 矫正文案 → 包装
#
# 每步先检测"本地可验证"的部分（单元/集成测试 + 本地二进制），
# 需要真实凭据的（云 ASR、模型调用）检测凭据：有就提示可跑真调，没有就跳过并留证。
set -uo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

PASS=0; FAIL=0; SKIP=0
NAME=""
say() { printf '\n\033[1m=== %s ===\033[0m\n' "$1"; }
ok()   { PASS=$((PASS+1)); printf '  ✅ PASS  %s\n' "$1"; }
bad()  { FAIL=$((FAIL+1)); printf '  ❌ FAIL  %s\n' "$1"; }
skip() { SKIP=$((SKIP+1)); printf '  ⏭  SKIP  %s\n' "$1"; }

# 跑某个 crate 里带前缀的 lib 测试，返回 pass/fail 计数。
run_tests() { # $1=manifest 相对路径  $2=测试过滤前缀
  local manifest="$1" filter="$2" log
  log="$(mktemp)"
  if cargo test --manifest-path "$manifest" --lib "$filter" >"$log" 2>&1; then
    local passed
    passed="$(awk '/^test result:/{sum+=$4} END{print sum+0}' "$log")"
    ok "${filter}（${passed} 用例）"
  else
    bad "$filter 测试失败："
    sed -n '/failures:/,/^$/p' "$log" | head -8
    FAIL=$((FAIL+1))
  fi
  rm -f "$log"
}

# 凭据检测：返回 0=有，1=无。
has_cred() {
  [ -n "${DASHSCOPE_API_KEY:-}" ] || [ -n "${ALIYUN_ACCESS_KEY_ID:-}" ] || [ -n "${DEEPSEEK_API_KEY:-}" ]
}

say "① 识别字幕（本地 whisper + 云 ASR dashscope）"
run_tests src-tauri/Cargo.toml "asr"
# 本地 ASR：whisper 二进制是否就绪（可离线转写）。
if [ -x "$ROOT/resources/asr-source-macos-arm64/bin/macos-aarch64/whisper-cli" ] || \
   [ -x "$ROOT/resources/asr-stage/bin/macos-aarch64/whisper-cli" ]; then
  ok "本地 whisper-cli 二进制就绪（可离线转写）"
else
  bad "本地 whisper-cli 二进制缺失"
fi
# 云 ASR（阿里云百炼 dashscope）：需要真实凭据。
if has_cred; then
  ok "检测到阿里云/模型凭据环境变量，可发起真实云 ASR 调用"
  printf '     （真实端到端请用激活账号走 activation → 短期 token → dashscope）\n'
else
  skip "云 ASR 真实调用：本机无阿里云百炼凭据（DASHSCOPE_API_KEY/激活未配置），已用无凭据降级测试覆盖"
fi

say "② 提取精彩片段（highlight，模型调用）"
run_tests src-tauri/Cargo.toml "ai::highlight"
if has_cred; then
  ok "检测到模型凭据，可发起真实 highlight 调用"
else
  skip "真实模型调用：需激活账号短期 token（本机未配置），已用冻结/门禁/注入测试覆盖"
fi

say "③ 生成合辑（clip_export，FFmpeg）"
run_tests src-tauri/Cargo.toml "ai::clip_export"

say "④ 矫正文案（clip_text_correction，模型调用）"
run_tests src-tauri/Cargo.toml "ai::clip_text_correction"

say "⑤ 包装（effect_caption，浏览器渲染）"
run_tests src-tauri/Cargo.toml "ai::effect_caption"

printf '\n──────────────────────────────\n'
printf '汇总：✅ %d 通过 · ❌ %d 失败 · ⏭ %d 跳过\n' "$PASS" "$FAIL" "$SKIP"
printf '注：⏭ 跳过项 = 需真实阿里云百炼/激活凭据，本机无凭据无法伪造跑通，已用离线测试留证。\n'
[ "$FAIL" -eq 0 ]
