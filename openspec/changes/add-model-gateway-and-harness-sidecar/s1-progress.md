# S1 进度（统一调用点与三条链路迁移）

切片定义见 `design.md`「决策八」；口径见「决策十」。本文件记录 S1 的**实时进度**，
便于跨轮续接与被中断后快速恢复。

## 已完成

| 项 | 证据 |
| --- | --- |
| 构建期默认模型改为 `qwen-plus`（百炼通义兼容模式，探针已实测） | `llm.rs` `DEFAULT_MODEL_ID`；测试 `build_default_model_is_qwen_plus_and_fresh_settings_resolve_to_it` |
| 心跳周期边界解析器 `ModelCallResolver`（冷启动立即可用、`next_resolution_at_ms` = +90s、变化上报） | 测试 `resolver_is_available_before_the_first_heartbeat_and_reports_next_resolution`、`heartbeat_refresh_reports_change_and_freezes_do_not_move` |
| 运行内冻结：`freeze_for_run()` 产出的快照不受后续配置切换影响 | 同上（冻结后切模型，旧快照不动） |
| 解析来源可观测：`ResolutionSource::{BuildDefault, HeartbeatResolution, LocalOverride, RunOverride}` + `as_str()` | `settings_model_is_reported_as_local_override` 等 |
| 地址不可覆盖：任何一层都改不了 Base URL | `heartbeat_resolution_wins_over_build_default_and_never_changes_base_url`、`base_url_is_never_overridable` |
| 非生产配置独立 cargo feature（`non-prod-model-config`，默认关闭）；生产构建里覆盖路径**不存在** | `production_build_ignores_non_production_overrides` / `non_production_build_applies_local_and_run_overrides`；两种配置分别跑门禁 |
| 短期 token 取用：`LlmTokenSource` + `acquire_llm_token`（余量不足**重取一次**，仍不足则授权失败；来源报错直接上抛） | 三个 token 测试 + `token_diagnostic_never_contains_the_token_body` |
| 删除未使用的 `MAX_AGENT_TURNS` 及其 re-export | `grep MAX_AGENT_TURNS` 仅剩任务描述 |
| 解析器输入显式化：`ModelResolutionInputs`（解析器自身不读环境变量，避免并发测试污染） | `ModelResolutionInputs::from_non_production_environment` 是唯一读环境处 |

**验证**：`cargo test --all-targets` 474 passed（默认构建）；`--features non-prod-model-config --lib` 223 passed；
根工作区 196 passed；两种配置 `clippy -D warnings` 与 `fmt` 干净；`openspec validate --all --strict` 32/32。

## 待办（按建议顺序）

1. **真实 token 来源**：实现 `LlmTokenSource` 的生产实现，复用 `AiApi::ai_llm_ak`（与云 ASR 共享 `AiLlmAkCache`，同一 scope）；
   把 `ModelCallResolver` 挂到应用装配处（心跳响应到达时调用 `apply_heartbeat`）。
2. **迁移文本纠错链路**（任务 4.1–4.5）：`SubtitleCorrectionProvider` 改为取"冻结快照 + 短期 token"，
   不再收用户 `api_key`；以既有测试作为等价性断言。
3. **迁移转场匹配链路**（5.1–5.5）→ **迁移精彩分析链路**（6.1–6.6），按风险从低到高。
4. **设置页与迁移**（9.1–9.8）：主入口改为"查看当前模型与可用性"；迁移为设置表增加模型来源与解析时间戳字段
   （**注意迁移号**：27 已被步骤 3 占用，28 原计划归步骤 4 —— 实施前需先定号并在 `docs/v2-delivery-order.md` 同步）。
5. **数据边界**（10.1–10.3）与 **调用点诊断**（4.6：内存环形缓冲 + 显式开关 + UI 面板默认关闭）。
6. **S1 收尾**：`立即重新解析`入口（9.8，仅非生产可见）；11.5/11.6/11.7 的切换、隔离与额度验收（macOS arm64 可跑部分）。

## 已知遗留

- `MAX_AGENT_TURNS` 已删除，但"Rust 侧整次调用的**预算上限**"尚未实现（任务 3.7 因此未勾选）。
- 短期 token 目前仍只被云 ASR 使用；三条 LLM 链路仍在收用户 `api_key`（迁移尚未开始）。
- 设置表尚未新增"模型来源/解析时间戳"字段（9.4 未做，涉及迁移号决策）。
