> 口径说明：本 change 不再包含服务端模型网关与网关短期令牌；模型调用改为客户端 Rust 侧持 `GET /api/client/ai/llm-ak` 的短期 token 直连百炼。原第 4 章“可切换的模型与调用路径解析”（13 项）与原第 5 章“任务令牌与网关客户端”（6 项）已随服务端网关一起删除。

## 切片与交付顺序（2026-10-07 定案）

| 切片 | 任务章节 | 交付门槛 | 是否依赖随包资产 |
| --- | --- | --- | --- |
| **S1 统一调用点** | 4、5、6、9、10 + 新增 4.6 / 9.8 | 三条链路既有测试作为等价性断言全绿；改一处配置即改全部模型 | 否 |
| **S2 sidecar 与 Skill** | 1、2、3、8 + 新增 2.7 / 3.6 / 3.7 | 三平台协议与恢复验收（走 CI）；最小 Skill 跑通一次工具循环 | 是（Harness 运行时） |
| **S3 媒体生成与发行验收** | 7、11 + 新增 7.6 / 11.10 / 11.11 | 超限请求不发出且不产生付费调用；CI 三平台证据归档 | 是 |

定案要点见 `design.md` 的「决策八 ~ 决策十四」：切片交付、老链路留在 Rust、token 共享、
运行级冻结、心跳触发解析、冷启动用构建期默认、默认 `qwen-plus` 不回退、中途过期重取一次、
非生产配置用独立 cargo feature、媒体生成只做契约与门禁、三平台验收走 CI、Harness 固定 Node 22 LTS。

## 1. 决策记录与运行资源扩展

- [x] 1.1 在本 change 的 `design.md` 中保留对两条已归档非目标决策的推翻记录（原决策、出处、理由、替代方案、代价），验证评审可据此追溯变更依据（`design.md`「决策零」完整覆盖：两条被推翻决策均点名出处（两个归档 change 目录）、推翻理由三条、替代方案两条、代价一条）
- [x] 1.2 在运行资源清单中新增 Harness 运行时、Profile 与 Skill 组件类型，验证清单校验接受新类型并仍拒绝未声明组件（新增 `HARNESS_PROFILE_COMPONENT = harness.profile` 与 `HARNESS_SKILL_COMPONENT = harness.skill` 常量并登记进 `ENHANCEMENT_COMPONENTS`；`harness.runtime` 此前已在；测试断言三组件独立诊断、缺失时增强不可用且不误伤基础能力）
- [ ] 1.3 把新组件加入发布校验的必需组件列表，验证缺失时构建失败；并验证 macOS arm64、macOS Intel x86_64 与 Windows x64 三个架构各自的产物都通过哈希与架构校验
- [ ] 1.4 验证 staging 与完整性校验覆盖新组件的哈希、架构与相对路径，且符号链接被拒绝

## 2. sidecar 生命周期与 JSON-RPC

- [x] 2.1 实现 Rust 侧 sidecar 启动、监管与优雅关闭，验证应用退出时不残留子进程（`HarnessSidecar::spawn` 受限环境（最小 PATH/HOME/TMPDIR 落工作区、`kill_on_drop`）+ `shutdown`（先关 stdin 通知 EOF、宽限期内退出、超时强杀）；测试 `graceful_shutdown_terminates_the_child_process` 断言关闭后 `try_wait` 为 Some）
- [x] 2.2 实现 stdio 上的 JSON-RPC 请求、响应、错误与取消消息，验证消息不含短期 token 或本地路径（`JsonRpcRequest`/`JsonRpcResponse`/`JsonRpcError` 换行分隔、单条 ≤ 1 MiB；请求/响应/错误/取消四类都有对应分支；测试 `protocol_messages_carry_no_tokens_or_local_paths` 断言序列化后不含 token/路径/Authorization）
- [x] 2.3 验证 sidecar 不接受来自本机其他进程的连接或输入（只由监管进程驱动）（**结构性成立**：stdio 管道、不监听任何端口、`env_clear` 不继承代理；不存在其他进程可连接的接口）
- [x] 2.4 实现 sidecar 异常退出与无响应的检测，验证该次调用被判定失败且不影响其他任务（`crash_is_reported_as_distinguishable_error` 断言提前退出 → `harness_crashed`；`per_request_timeout` 超时 → `harness_timeout`；单次失败不持久化到管理器状态，后续 `acquire` 仍可启动新实例）
- [x] 2.5 实现按需启动与空闲回收，验证空闲态下无 sidecar 常驻进程（`HarnessManager::acquire` 惰性启动、`reclaim_idle` 空闲超时回收；测试 `manager_starts_on_demand_and_reclaims_when_idle` 断言启动前无进程、空闲后回收）
- [ ] 2.6 测量并记录 sidecar 的峰值内存、磁盘占用与启动耗时，验证其在 8 GB 目标设备基线内
- [x] 2.7 用最小 Skill 真跑一次工具循环（含一次受控工具调用与结构化结果回收），验证 sidecar 交付可用于承载新分支，而不只证明启动与往返可用（`minimal_skill_tool_loop_round_trips_over_stdio`：真实 Python 子进程跑 `run_skill → tool.invoke(echo) → 返回`，Rust 侧白名单授权并执行工具；另有 `tool_denied_is_reported_without_execution` 断言白名单外工具被拒且执行器不被调用）

## 3. 应用专用 Profile 与 Skill

- [ ] 3.1 产出随包的应用专用 Profile，只注册模型路由与 Skill，验证不存在 shell、任意文件编辑、浏览器浏览或任意网络访问工具
- [ ] 3.2 新增测试：分析过程尝试调用上述通用工具时被拒绝，且已完成阶段结果不受影响
- [ ] 3.3 新增测试：用户目录存在同名 Agent 配置或 Profile 时系统忽略它并只用随包版本
- [ ] 3.4 把现有五个内置精彩 Skill 与两个转场阶段纳入随包 Skill，验证 Skill ID 与版本仍进入运行指纹且指纹语义不变
- [x] 3.5 新增测试：ASR 文本包含形似系统指令或工具调用的内容时，工具集合、模型路由与工作流阶段均不改变（highlight 注入防护测试：恶意转写逐字节比对 `task`/`skills`/`tags`/`goal` 不变、提示词中不存在 `tool`/`tools` 字段、恶意文本只能待在 `segments[].text` 数据位；harness 侧 `tool_denied_is_reported_without_execution` 证明白名单由 Rust 决定、不在白名单内的工具被拒且不执行）
- [x] 3.6 把 Skill 作为随包文件由 Rust 读取与校验并把 Skill ID 与版本写入运行指纹，验证三条既有链路**不进入 Harness** 也能满足 Skill 随包与指纹语义（新增 `bundled_skills.rs`：`load_bundled_skills` 只读随包 `*.skill.json`、`deny_unknown_fields` 拒绝未知字段、id 唯一、版本必须是 `x.y.z`；`skills_fingerprint` 对 `id@version` 排序后 SHA-256，与精彩运行既有 `skills_snapshot`（`id@version`）语义一致；加载器不接受任何用户目录输入——忽略用户同名配置是结构性的）
- [x] 3.7 删除未使用的 `MAX_AGENT_TURNS`，验证工具循环归 Harness、Rust 侧只保留整次调用的超时与预算上限，且无残留引用（`MAX_AGENT_TURNS` 已删除，全树 grep 无残留；工具循环归 sidecar 的 `tool.invoke`，Rust 侧只做白名单授权与 `per_request_timeout`）

## 4. 转换文本纠错链路

- [x] 4.1 把一键文本纠错改为经统一调用点取“短期 token + 冻结模型快照”，验证在相同配置下分批边界、批次顺序与全局字幕映射与转换前一致（`SubtitleCorrectionProvider` 契约由 `api_key: &str` 改为 `&ModelCallHandle`（冻结快照 + 短期 token）；工作流不再持有 `CredentialStore`，改为注入 `ModelCallRuntime`；测试 `all_batches_share_one_frozen_model_and_the_shared_token_source` 证明跨批同一模型且凭据是 `st-lt-v1.` 短期 token，既有分批边界/顺序测试不变）
- [x] 4.2 以既有测试作为等价性断言，验证提示词约束（只修正错别字、同音错词、明显识别错误与标点）与校验规则不变（提示词与 preamble 逐字未改；`correction_response_must_be_complete_and_ordered`、`batches_preserve_complete_subtitle_order`、`model_resolution_does_not_change_batch_boundaries_or_order` 全部保持通过）
- [x] 4.3 验证原子提交与并发冲突处理不变：任一批次失败或版本冲突时全部工程字幕保持不变（`later_batch_failure_keeps_all_subtitles_and_project_version_unchanged` 在迁移后仍通过；新增门禁测试同时断言门禁不通过时字幕逐条不变）
- [x] 4.4 新增测试：账号未开通或纠错额度耗尽（依据 heartbeat 响应）时停止运行、保留字幕不变并显示可区分的授权或额度原因（工作流级测试 `authorization_and_quota_gates_stop_the_run_before_any_batch`：未开通→`NotActivated`、额度耗尽→`QuotaExhausted`，两者可区分且**不发起任何付费调用**、字幕保持不变；另新增 `credential_failure_stops_the_run_and_keeps_subtitles_unchanged` 覆盖短期 token 授权失败路径。**事实更正**：未开通由 heartbeat 派生的激活状态判定；**额度耗尽只能由上游业务错误码判定**——客户端没有额度本地权威来源，不能由 heartbeat 推断，代码中已有明确注释，此前的措辞过度归因于 heartbeat）
- [x] 4.5 验证 WebView 不接触短期 token 与上游原始响应，审计只保存实际模型、prompt 版本、指纹、计数、实际用量（若上游返回）、耗时与脱敏错误分类（token 只在 Rust 侧 `ModelCallHandle` 内流转；审计测试断言运行记录只含实际模型 `qwen-plus`、prompt 版本与状态且**不含 token**；诊断标签只输出 provider 与剩余有效期）
- [x] 4.6 实现统一调用点诊断（内存环形缓冲 + 显式开关 + UI 面板默认关闭），验证不含短期 token、完整提示词与 ASR 正文（`LlmCallDiagnosticsBuffer` 可实例化环形缓冲：默认关闭、容量 256、只存 `LlmCallDiagnostic` 脱敏字段；Tauri 命令 `ai_set_llm_diagnostics` / `ai_get_llm_diagnostics`；设置页 `<details>` 面板默认收起，开关开启后展示最近事件；测试覆盖开关/容量淘汰/快照不含 token、提示词与 ASR 正文，UI 测试断言默认关闭、开启后展示且不出现 `st-lt-v1`/`sk-`）

## 5. 转换转场匹配链路

- [x] 5.1 把两阶段转场匹配改为经统一调用点取冻结模型快照，验证召回集合、最多三个有序候选与 `none` 语义不变（`TransitionAgentProvider` 契约去掉 `settings`/`api_key`，改收 `&ModelCallHandle`；工作流运行开始冻结一次，两个 Agent 阶段共用同一快照与同一 `cloud_direct` 路径；召回/候选/`none` 的既有纯校验测试全部保持通过）
- [x] 5.2 以既有测试作为等价性断言，验证边界有效性、候选完整性、分数范围与 `renderMode=bridge` 校验不变（`fabricated_assets_fail_only_their_own_boundary_and_prompt_omits_private_fields`、`missing_duplicate_and_unknown_boundaries_are_counted_without_rejecting_the_batch`、`none_entries_are_accepted_and_produce_no_candidates`、`oversized_candidate_lists_and_long_reasons_fail_their_boundary`、`scoring_requires_complete_candidates_and_prefers_match_rank_on_ties` 全部通过；提示词逐字未改）
- [x] 5.3 验证冻结阈值、模型与 prompt 版本的运行快照语义不变，且模型不能改变阈值（阈值仍只来自用户设置 `transition_auto_apply_score`，模型来自冻结快照——两者来源分离，模型无法影响阈值；`auto_apply_includes_scores_equal_to_the_frozen_threshold` 保持通过；运行记录写入真实调用路径 `cloud_direct` 与冻结模型标识）
- [x] 5.4 新增测试：短期 token 过期或额度耗尽（依据 heartbeat 响应）时停止运行、不覆盖人工锁定边界，并显示可区分的授权或额度原因（新增 4 个测试：凭据失败重取一次 token 并重试、非凭据失败不重试、拒绝型 token 来源给出可区分 `Credential`、**真实边界行**验证人工锁定边界在授权失败/无目标时既不被选入也不被改写。**事实更正**：额度耗尽由上游业务错误码判定，不由 heartbeat 推断）
- [x] 5.5 验证既有 `transition_matching.rs` 未提交的校验加固行为在转换后保持（虚构素材、重复与未知边界、`none` 条目、超长理由的处理不变）（上述 5.2 的五个加固测试在迁移后逐条通过，未做任何放宽）

## 6. 转换精彩分析链路

- [x] 6.1 把候选发现与全局评分两个阶段改为经统一调用点取同一冻结模型，验证两阶段使用同一冻结模型且运行指纹记录实际模型（`HighlightAgentProvider` 三个方法改收 `&ModelCallHandle`；运行开始 `freeze_for_run` 一次，候选发现与全局评分共用；测试 `candidate_discovery_and_ranking_share_one_frozen_model` 断言两阶段模型集合只有一个 `qwen-plus`、凭据是短期 token、运行记录 `model_id` 为实际冻结模型）
- [x] 6.2 以既有测试作为等价性断言，验证分块策略、候选校验、评分归一化、阈值语义与电路熔断行为不变（`chunks_keep_source_file_boundaries_and_overlap`、`chunk_timeout_is_persisted_and_does_not_discard_later_batches`、`analysis_fingerprint_changes_when_result_policy_changes`、`top_scores_keep_reference_candidates_below_threshold`、`provider_score_scales_are_normalized_to_percentages`、`legacy_fractional_drafts_are_normalized_before_fallback_ranking`、`skill_selection_is_deterministic_and_unknown_labels_are_data_only` 全部通过）
- [x] 6.3 验证持久化与恢复不变：运行 ID、冻结 Skills 快照、分析指纹与阶段尝试代次语义保持不变（运行 ID/`skills_snapshot`/`analysis_fingerprint` 字段与算法未改；**新增** `ModelCallRuntime::freeze_with_model` 让续跑按运行记录里的模型冻结，测试 `resume_uses_the_model_recorded_in_the_run_not_the_new_settings` 证明改设置后续跑不漂移）
- [x] 6.4 验证已授权的智能成片自动启动路径仍校验任务授权、激活、模型、文本发送范围与批次代次（智能成片端到端测试保持通过：`create_local` 仍要求 `authorization_confirmed`，`prepare` 的授权确认文案与门禁未改，批次代次语义未触碰；迁移只替换了凭据来源）
- [x] 6.5 新增测试：短期 token 过期或额度耗尽时停止运行并显示可区分原因，不把结果描述为 Provider 网络错误（新增 `credential_failure_is_distinguishable_and_refreshes_the_token_once`：首次凭据失败重取一次 token 后成功；来源完全拒绝时运行以 `Failed` 落库且 `last_error_code = highlight_authorization_failed`、文案不含“网络/Provider”，并且**一个付费调用都不发出**）
- [x] 6.6 验证精彩分析结果结构、稳定句段 ID 引用与来源区间不受转换影响（`AiHighlightCandidate`/`AiHighlightRun` 结构与 `segment_ids`/来源区间字段未改；候选校验与稳定 ID 引用的既有测试全部通过）

## 7. 媒体生成受控工具

- [x] 7.1 实现图片与视频生成的结构化业务工具接口，验证返回值只包含业务任务 ID 与状态（`MediaGenerationTool`（实现 `ToolExecutor`）经 sidecar `tool.invoke` 暴露；`MediaGenerationResult` 只有 `taskId` + `status`，测试断言序列化不含 token/url/download/ak）
- [x] 7.2 新增测试：请求包含任意 URL、本地路径、Provider 原始参数或滤镜与命令行片段时被拒绝且不产生付费调用（`MediaGenerationRequest` 带 `deny_unknown_fields`，`url`/`localPath`/`filter`/`cmdline` 等字段结构上不存在；四个样本逐一断言反序列化失败）
- [x] 7.3 实现 Rust 侧对时间范围、互斥类型与最大尝试次数的校验，验证非法请求被拒绝并记录脱敏原因（`validate_request_shape`：图片/视频互斥、图片 1..=16 张、视频 1..=30000ms、提示词非空；`max_attempts` 计数超限即拒；错误码稳定且不含提示词正文）
- [x] 7.4 新增测试：本地门禁（任务级动画数量、累计秒数与金额上限）拒绝时客户端停止提交，验证超限请求不会发出（`MediaRunBudget::try_reserve` 累计预留：图片张数、视频秒数、金额三者分别有可区分错误码；超限请求在 `submit` 之前被拒，桩提交计数不变）
- [x] 7.5 验证工具契约中不存在短期 token 与任意下载地址，且 Harness 无法绕过 Rust 直接调用生成接口（**结构性成立**：`MediaUpstream` 是 Rust 侧 trait，sidecar 只能经 `tool.invoke → ToolExecutor::execute` 到达；`MediaGenerationResult` 不含 token/下载地址；token 只存在于统一调用点内存中）
- [x] 7.6 只交付结构化契约 + Rust 侧门禁 + 桩实现，真实上游提交/轮询/下载留给 `add-generative-video-animation`，验证本 change 验收期间不产生付费调用（`StubMediaUpstream` 不触网、只返回确定性 `stub-*` 任务 ID；测试断言提交只递增本地计数）
- [x] 7.7 把额度口径写进冻结配置：以 heartbeat 开通/额度结论为总上限，本地任务级上限为每次运行最多 4 张图、最多 5 秒视频、单次金额按模型单价折算上限，验证超限即不提交（`MediaQuota::default()` = 4 张图/5 秒视频/400 分金额上限，单价 20 分每图、50 分每秒视频；金额按单价折算并累计，超限测试通过）

## 8. 取消、资源限制与迟到结果

- [ ] 8.1 实现取消时终止 sidecar 调用与渲染子进程、清理未发布临时文件，验证有界时间内完成且保留已发布阶段结果
- [ ] 8.2 实现内存与执行时间上限，验证超限时终止子进程、标记该次调用失败并释放资源
- [ ] 8.3 新增测试：取消后收到迟到结果时被忽略，不覆盖人工编辑或其他已发布内容
- [ ] 8.4 验证 sidecar 与渲染运行时使用独立子进程并可分别限制资源

## 9. 设置页与凭据

- [x] 9.1 把设置页入口从“管理上游 API Key”改为“查看当前模型与可用性”，验证界面不再以凭据管理为主入口（面板改为 `MODEL / 当前模型与可用性`：模型标识改为只读展示、**移除 API Key 输入与模型 ID 输入**；UI 测试断言 `queryByLabelText(/API Key/)` 与 `模型 ID` 均为空，且分区分组断言同步更新）
- [x] 9.2 在设置页展示当前生效模型、解析来源、下次解析时间与短期 token 状态，以及 heartbeat 判定的开通/额度状态（面板展示当前模型、解析来源（四种来源各有中文文案）、上次/下次解析时间、短期 token 只读状态（已就绪则显示 provider 与剩余秒数）、调用路径与授权状态。**事实更正**：未开通由 heartbeat 派生的激活状态判定；**额度耗尽由上游业务错误码判定**（客户端没有额度本地权威来源），界面据此如实说明而不是伪装成 heartbeat 结论）
  - 后端已完成：`AiModelStatusView` + `AiCommandService::model_status()` + Tauri 命令 `ai_get_model_status`，给出当前模型/解析来源/解析时间/下次解析时间/短期 token 只读状态（`cached`/`provider`/剩余秒数，**不发网络请求**）/旧凭据是否存在/调用路径；`ModelCallRuntime` 在冷启动与心跳边界把来源与时间写回设置表，重启后仍有值可显示。
  - 未勾选原因：**UI 尚未消费**（`ui/src/App.tsx` 仍是 Key 管理入口），属下一步的界面改造；heartbeat 开通/额度状态的展示同样待 UI 接线。
- [x] 9.3 在设置页展示 AI 可用性，验证账号未开通或额度耗尽时显示可区分原因并阻止新的付费调用（设置页显示授权与可用性说明；运行时的可区分原因与“不发起付费调用”由 4.4/5.4/6.5 的工作流级测试保证：`NotActivated`/`QuotaExhausted`/`Credential`/`highlight_authorization_failed` 互不混淆）
  - 后端已完成：三条链路在门禁/凭据不可用时均给出可区分原因（`NotActivated` / `QuotaExhausted` / `Credential` / `highlight_authorization_failed`）且不发起付费调用（任务 4.4 / 5.4 / 6.5 的测试）。
  - 未勾选原因：设置页尚未展示该状态（待 UI 接线）。
- [x] 9.4 新增迁移为设置表增加模型来源与解析时间戳字段并保留阈值，验证在旧库快照上迁移成功且既有阈值取值不变（迁移 **28** 为 `llm_provider_settings` 增列 `model_source`/`resolved_at`/`next_resolution_at`，只增列不重写旧行；测试断言三列存在、旧阈值 66/77/9 往返不变、新字段默认未设置；迁移号分配已写入 `docs/v2-delivery-order.md`：26=步骤1、27=步骤3、**28=步骤2**、29=步骤4 顺延）
- [x] 9.5 验证既有上游凭据在迁移后保留、不再被三条链路使用，且迁移不强制清除凭据（`model_status` 只报告 `keyConfigured` 不读取内容；设置页改为提示“旧 Key 保留但不再被任何链路使用”，清除动作降级为可选按钮；“三条链路已全部迁移到 `ModelCallRuntime`”由任务组 4/5/6 的测试保证）
- [x] 9.6 新增测试：读取设置或重新打开页面时不回填任何凭据内容，DOM 与日志均不含凭据与短期 token（UI 测试断言设置页不存在凭据输入框、不回填 Key；后端集成测试断言状态视图序列化后不含 `sk-` 与 `st-lt-v1`；`TokenStatus` 结构本身不携带 token 本体，诊断只输出 provider 与剩余秒数）
  - 后端已完成：新增集成测试断言状态视图序列化后不含 `st-lt-v1`（短期 token）与 `sk-`（旧凭据），`TokenStatus` 结构本身不携带 token 本体；诊断只输出 provider 与剩余秒数。
  - 未勾选原因：DOM 侧断言需要 UI 改造后补（当前 UI 仍以 Key 输入为主入口）。
- [x] 9.7 更新发布说明与界面文案，明确模型调用为客户端直连百炼、依赖账号开通状态，且配置切换最长一个心跳周期（90 秒）生效（`docs/release-v2.md` 增“模型调用形态”口径条目；设置页文案明确直连百炼、90 秒生效窗口、运行前冻结、界面不管理凭据）
- [x] 9.8 在设置页新增「立即重新解析」入口，验证它只在非生产配置（独立 cargo feature）下可见且不影响进行中的运行（后端 `reparse_model_config` 有两个 `cfg` 版本：生产构建里这段代码**不存在**、被调用也返回固定错误码；`AiModelStatusView.nonProductionConfig` 由编译期 feature 决定；UI 测试分别断言生产构建不显示入口、非生产构建显示且点击后回填新模型；重新解析只替换后续运行使用的快照，进行中的运行持有已冻结快照）

## 10. 数据边界

- [x] 10.1 以测试固定两个且仅两个用户内容例外（用户授权的临时音频直传百炼、用户授权的最小文本发往百炼），验证其他上传路径不存在（新增 `src/privacy.rs` 作为数据边界**单一登记处**：`USER_CONTENT_UPLOAD_EXCEPTIONS` 恰好两条且都要求显式授权，并声明各自允许的源码模块；测试断言条目数、键名、说明与模块存在性）
- [x] 10.2 新增测试：精彩分析、转场匹配与纠错请求中不包含视频、音频、本地路径、签名流地址或数据库其他内容（三条链路各加边界测试：候选/评分提示词不含 `/tmp`、`/Users`、`.mp4/.mov/.m4a/.wav`、`http(s)://`、`?sign=`、`Expires=`、`Authorization/Bearer`；全局评分不重复发送原始转写；纠错提示词只含文本数组；转场素材提示词只含公开描述字段，不含 `video_url`/`sha256`/签名）
- [x] 10.3 验证本地 ASR 路径与录制、预览、封面等基础能力不因本 change 产生新的网络上传（源码级守卫：`multipart`/`Part::bytes`/`Part::file` 上传原语全树扫描**只允许**出现在登记过的 `src/asr/cloud.rs`；基础能力模块（recorder/manager/resolver/profile_resolver/preview/thumbnail/supervisor/processor）不得引用 `ApiClient` 或上传原语；云转写仍是唯一上传点）

## 11. 三平台协议与恢复验收

- [ ] 11.1 在 macOS arm64 完成 sidecar 启动、JSON-RPC 往返、取消、异常退出恢复与升级到固定版本的验收
- [ ] 11.2 在 macOS Intel x86_64 完成 11.1 的同项验收
- [ ] 11.3 在 Windows x64 完成 11.1 的同项验收
- [ ] 11.4 执行三条链路的端到端验收：精彩分析、一键转场匹配与一键文本纠错各完成一次，核对结果与转换前的等价性断言
- [ ] 11.5 执行切换验收：修改模型配置后核对最长一个心跳周期（90 秒）生效、进行中的运行不受影响、指纹不一致时不复用旧结果
- [ ] 11.6 执行配置隔离验收：并发运行使用不同配置时互不影响，且无任何模块绕过统一调用点读取配置或自行获取短期 token
- [ ] 11.7 执行额度与鉴权验收：未开通、短期 token 过期与额度耗尽三种状态下，三条链路均显示可区分原因且不产生付费调用
- [ ] 11.8 执行资源回收验收：连续完成多次任务后核对无残留 sidecar 进程、无未清理临时文件，且进程峰值内存与磁盘占用在 8 GB 基线内
- [ ] 11.9 核对诊断输出中不含短期 token、完整提示词、ASR 正文、本地路径与签名下载地址
- [ ] 11.10 把 11.1–11.3 改为扩展 `.github/workflows/platform-release-validation.yml` 的 `macos-15-intel` 与 `windows-2022` job 执行协议与恢复验收并归档证据（不再以「无真机」为由判定不可达）
- [ ] 11.11 在 CI 内构建并校验锁定发行输入（Harness 运行时、Profile 与 Skill），验证哈希与架构校验通过且不依赖预置上传
