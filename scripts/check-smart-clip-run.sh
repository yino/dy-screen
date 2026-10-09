#!/usr/bin/env bash
#
# 智能成片「跑一次」之后的健康自查。
#
#   scripts/check-smart-clip-run.sh [project_id]
#
# 只读应用数据库，按流水线阶段输出状态与**真实失败原因**，用于在你跑完之后
# 一眼定位卡在哪一步，而不必把日志贴回来人工分析。
#
# 退出码：0 = 全流程无失败；1 = 存在失败阶段；2 = 环境/数据库不可用。

set -euo pipefail

DB="${DY_SCREEN_DB:-$HOME/Library/Application Support/com.yino.dyscreen/dy-screen.sqlite3}"

if [[ ! -f "$DB" ]]; then
  printf '找不到应用数据库：%s\n' "$DB" >&2
  printf '可用 DY_SCREEN_DB 指定其它路径。\n' >&2
  exit 2
fi

if ! command -v sqlite3 >/dev/null 2>&1; then
  printf '需要 sqlite3 命令。\n' >&2
  exit 2
fi

q() { sqlite3 -readonly "$DB" "$1"; }

project_filter=""
if [[ $# -ge 1 ]]; then
  project_filter="AND p.id = $1"
fi

printf '数据库：%s\n' "$DB"
printf '时间：%s\n\n' "$(date '+%Y-%m-%d %H:%M:%S')"

# ---------------------------------------------------------------- 阶段 1：识别
printf '== 阶段 1：语音识别（云/本地）==\n'
q "SELECT p.id || '  ' || p.status || '  输入:' ||
      (SELECT count(*) FROM ai_project_inputs i WHERE i.project_id = p.id) ||
      '  完成:' ||
      (SELECT count(*) FROM ai_project_inputs i WHERE i.project_id = p.id AND i.status = 'completed') ||
      '  失败:' ||
      (SELECT count(*) FROM ai_project_inputs i WHERE i.project_id = p.id AND i.status = 'failed')
   FROM ai_projects p WHERE 1=1 $project_filter ORDER BY p.id DESC LIMIT 5;"
printf -- '-- 失败输入的脱敏原因：\n'
q "SELECT '  #' || i.id || '  ' || coalesce(i.last_error_code, '(无)') || '  ' ||
      coalesce(substr(i.last_error_message, 1, 40), '')
   FROM ai_project_inputs i JOIN ai_projects p ON p.id = i.project_id
   WHERE i.status = 'failed' $project_filter ORDER BY i.id DESC LIMIT 5;"

# ------------------------------------------------------------ 阶段 2：提取精彩
printf '\n== 阶段 2：提取精彩 ==\n'
q "SELECT '  run #' || r.id || '  项目:' || r.project_id || '  ' || r.status ||
      '  模型:' || r.model_id || '  候选:' ||
      (SELECT count(*) FROM ai_highlight_candidates c WHERE c.run_id = r.id) || '  已选:' ||
      (SELECT count(*) FROM ai_highlight_candidates c WHERE c.run_id = r.id AND c.selected = 1)
   FROM ai_highlight_runs r WHERE 1=1 ${project_filter//AND p.id/AND r.project_id}
   ORDER BY r.id DESC LIMIT 5;"
printf -- '-- 批次失败原因（区分「模型/参数被拒」与「网络类失败」）：\n'
q "SELECT '  run#' || h.run_id || ' 批次' || h.ordinal || '  ' ||
      coalesce(h.last_error_code, '(无)') || '  ' || coalesce(substr(h.last_error_message, 1, 36), '')
   FROM ai_highlight_chunks h
   WHERE h.status = 'failed' ORDER BY h.run_id DESC, h.ordinal LIMIT 8;"

# ---------------------------------------------------------- 阶段 3-6：剪辑链路
printf '\n== 阶段 3-6：剪辑工程 / 字幕 / 导出 ==\n'
q "SELECT '  工程 #' || c.id || '  运行:' || c.highlight_run_id ||
      '  版本:' || c.version || '  导出:' || c.export_status ||
      '  片段:' || (SELECT count(*) FROM ai_clip_segments s WHERE s.clip_project_id = c.id) ||
      '  字幕:' || (SELECT count(*) FROM ai_clip_subtitles t WHERE t.clip_project_id = c.id)
   FROM ai_clip_projects c ORDER BY c.id DESC LIMIT 5;"
printf -- '-- 工程级错误与产物：\n'
q "SELECT '  工程 #' || id || '  ' || coalesce(last_error_code, '(无错误)') || '  ' ||
      coalesce(substr(last_error_message, 1, 40), '') ||
      case when output_path is null then '' else '  产物:' || output_path end
   FROM ai_clip_projects ORDER BY id DESC LIMIT 5;"

# ------------------------------------------------------ 阶段 7：生成式动画资产
printf '\n== 生成式动画资产（若已启用）==\n'
q "SELECT '  资产 #' || a.id || '  ' || a.composition || '  ' || a.status ||
      '  时长:' || coalesce(cast(a.duration_ms as text), '-') || 'ms' ||
      '  成本:' || coalesce(cast(a.rejection_code as text), '-')
   FROM ai_generated_animation_assets a ORDER BY a.id DESC LIMIT 5;" 2>/dev/null \
  || printf '  (该项目尚未使用生成式动画)\n'

# ------------------------------------------------------------------ 结论判定
printf '\n== 结论 ==\n'
failed_inputs=$(q "SELECT count(*) FROM ai_project_inputs WHERE status = 'failed';")
failed_runs=$(q "SELECT count(*) FROM ai_highlight_runs WHERE status = 'failed';")
failed_chunks=$(q "SELECT count(*) FROM ai_highlight_chunks WHERE status = 'failed';")
historical_exports=$(q "SELECT count(*) FROM ai_clip_projects WHERE export_status = 'completed';")
latest_project=$(q "SELECT coalesce(max(id), 0) FROM ai_clip_projects;")
latest_export=$(q "SELECT coalesce((SELECT export_status FROM ai_clip_projects ORDER BY id DESC LIMIT 1), 'none');")

printf '  失败输入：%s   失败运行：%s   失败批次：%s\n' \
  "$failed_inputs" "$failed_runs" "$failed_chunks"
printf '  最新工程 #%s 导出状态：%s   （历史成功导出次数：%s）\n' \
  "$latest_project" "$latest_export" "$historical_exports"

# 只认**最新一次**工程的结果：历史成功不代表这次跑通。
if [[ "$latest_export" == "completed" ]]; then
  printf '\n✅ 最新工程已导出完成——本次链路走通。\n'
  exit 0
fi

if [[ "$failed_inputs" != "0" || "$failed_runs" != "0" || "$failed_chunks" != "0" ]]; then
  printf '\n⛔ 存在失败阶段（含历史累计）。按下面的分类核对**本次**的失败原因：\n'
  printf '   - dashscope_request_rejected / highlight_request_rejected → 模型或参数被上游拒绝\n'
  printf '   - dashscope_forbidden / highlight_access_denied        → 凭据无权，或所选模型已下线\n'
  printf '   - highlight_provider_failed                            → 网络/未知，可重试\n'
  printf '   - dashscope_missing_transcripts                        → 云端转写返回结构不含文本（解析问题）\n'
fi

if [[ "$latest_export" == "none" ]]; then
  printf '\n⚠️ 还没有任何剪辑工程——流程可能停在「提取精彩」之前。\n'
else
  printf '\n⚠️ 最新工程尚未导出完成（状态：%s）。\n' "$latest_export"
fi
exit 1
