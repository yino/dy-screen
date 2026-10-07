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
| **真实 token 来源** `ActivationLlmTokenSource`：走既有激活凭据 + `GET client/ai/llm-ak`，**复用 `AiApi` 内部缓存**（与云 ASR 同一份） | `model_call.rs`；错误分类测试 `token_api_errors_map_to_distinguishable_categories` |
| **端到端运行时** `ModelCallRuntime`：bootstrap 冷启动、`snapshot()`、`next_resolution_at_ms()`、`freeze_for_run()`、`on_heartbeat()`、`acquire_token()` | `ai/model_call.rs` 7 个测试（含两实例互不污染、来源报错不重取） |
| **心跳边界接线**：`app.rs` 在 `HeartbeatOutcome::Active` 时调用 `on_heartbeat` 并发出 `model-call-event`（只含公开字段：modelId/source/changed/nextResolutionAtMs） | `app.rs` `spawn_activation_lifecycle` |
| **精彩分析链路迁移完成**：`HighlightAgentProvider` 三方法收 `&ModelCallHandle`；两阶段共用冻结快照；续跑按运行记录模型冻结；凭据失败立即停止且 `highlight_authorization_failed`；新增 3 个测试 | `highlight.rs`、`llm.rs`（删除已无调用者的旧提取辅助） |
| **转场匹配链路迁移完成**：`TransitionAgentProvider` 收 `&ModelCallHandle`；两阶段共用冻结快照；运行记录记 `cloud_direct` + 冻结模型；凭据失败重取一次重试；新增 5 个测试（含真实边界行的人工锁定保护） | `transition_matching.rs`；`tests/smart_workflow.rs` 5 处构造点迁移 |
| **文本纠错链路迁移完成**：`SubtitleCorrectionProvider` 收 `&ModelCallHandle`；工作流注入 `ModelCallRuntime`；每批取 token + 凭据失败重取一次重试本批；门禁前置；`Credential`/`NotActivated`/`QuotaExhausted` 三者可区分 | `clip_text_correction.rs` 新增 6 个测试；`tests/smart_workflow.rs` 5 处构造点迁移 |
| **启动降级**：模型调用点 bootstrap 失败**不阻断应用启动**，降级到构建期默认设置并只记录固定分类 | `app.rs`；日志 `bootstrap_fallback` / `bootstrap_fallback_persist_failed` |

**验证（最近一次全量）**：`cargo test --all-targets` 481 passed（默认构建）；
`--features non-prod-model-config --lib` 230 passed；根工作区 196 passed；
两种配置 `clippy -D warnings` 与 `fmt` 干净；`openspec validate --all --strict` 32/32。

## 待办（按建议顺序）

1. ~~真实 token 来源与心跳接线~~ **已完成**（见上表）。
2. ~~迁移文本纠错链路（任务 4.1–4.5）~~ **已完成**：provider 契约改为 `&ModelCallHandle`；
   每批从统一调用点取 token（中途凭据失败重取一次并重试本批）；门禁在任何付费调用前拦下；
   审计不含 token。既有测试即等价性断言，另有 6 个新测试。
3. ~~迁移转场匹配链路（5.1–5.5）~~ **已完成**：provider 契约改收 `&ModelCallHandle`；两阶段共用一份冻结快照；
   运行记录改记真实调用路径 `cloud_direct` 与冻结模型；凭据失败重取一次并重试；人工锁定边界在失败路径下不被改写。
   ~~迁移精彩分析链路（6.1–6.6）~~ **已完成**：三个 Agent 方法改收 `&ModelCallHandle`；
   两阶段共用冻结快照；续跑按运行记录模型冻结（新增 `freeze_with_model`，防漂移）；
   凭据失败立即停止并落 `highlight_authorization_failed`（不发起付费调用）。
   **S1 剩余：设置页与迁移（9.1–9.8）、数据边界（10.1–10.3）、调用点诊断（4.6）。**
4. **设置页与迁移**（9.1–9.8）：主入口改为"查看当前模型与可用性"；迁移为设置表增加模型来源与解析时间戳字段
   （**注意迁移号**：27 已被步骤 3 占用，28 原计划归步骤 4 —— 实施前需先定号并在 `docs/v2-delivery-order.md` 同步）。
5. **数据边界**（10.1–10.3）与 **调用点诊断**（4.6：内存环形缓冲 + 显式开关 + UI 面板默认关闭）。
6. **S1 收尾**：`立即重新解析`入口（9.8，仅非生产可见）；11.5/11.6/11.7 的切换、隔离与额度验收（macOS arm64 可跑部分）。

## 已知遗留

- `MAX_AGENT_TURNS` 已删除，但"Rust 侧整次调用的**预算上限**"尚未实现（任务 3.7 因此未勾选）。
- **三条链路已全部迁移完成**：纠错、转场、精彩分析都不再持有用户 `api_key`，统一走 `ModelCallRuntime`。
- 设置页仍以“管理上游 API Key”为主入口（9.1–9.3、9.4 迁移未做）。
- `model-call-event` 已发出但设置页尚未消费（任务 9.2）。
- 设置表尚未新增"模型来源/解析时间戳"字段（9.4 未做，涉及迁移号决策）。
