# 真实 Harness 运行时与随包资产设计

本文件是 `design.md` 决策一~十四的补充，对应「全部按推荐」落地的第一步：先钉死
**模型路由契约**与**组合生成 skill 分层**两个盲点，再谈写代码。

## 决策十五：模型路由契约——Harness 是薄编排，模型调用走 Rust 持 token

- Harness **不持短期 token、不触网**。skill 需要模型时，发一个受控 `tool.invoke`：
  `{"tool": "model.generate_text", "params": {"prompt": "...", "schema": "...", "maxTokens": N}}`。
- Rust 侧 `model.generate_text` 工具：
  1. 校验 `prompt`（长度、不含凭据/URL——沿用数据边界 `privacy.rs` 的登记）；
  2. 经 `ModelCallRuntime` 取冻结快照 + 短期 token（S1 已交付的 `freeze_for_run` + `acquire_token`）；
  3. 直连百炼兼容端点（`extract_with_handle`），返回**结构化结果**；
  4. 失败给出可区分分类（`credential` / `temporary` / `provider`），与三条既有链路一致。
- `model.generate_text` 进入 `ToolAllowlist` 白名单，与 `media.generate`（任务组 7）并列；
  Harness 无法绕过 Rust 直接持 token 或直连上游（结构性成立，`MediaUpstream` 同款约束）。

**与 S1 的关系**：`model.generate_text` 就是 S1 统一调用点的**受控业务工具封装**，
不是第二条模型路径——它内部复用 `ModelCallRuntime` / `extract_with_handle`，不新增任何
地址或凭据来源。

## 决策十六：组合生成拆两层——skill 编排（Harness）+ model 工具（Rust，落地步骤 3 任务 2.4）

**校准**（基于契约事实）：步骤 3 已存在
`EffectCaptionCompositionProvider::compose(prompt, token) -> Result<String>`（单次结构化输出），
以及 `composition_prompt` / `build_effect_caption_composition` / `validate_effect_caption`。
因此组合生成是「单次结构化输出」，按决策九应留在 Rust——但为了让 Harness 端到端有真实业务，
把它拆成两层：

1. **skill 层（Harness 内编排）**：`effect-caption-compose` skill 读输入（`CompositionRequest`
   字段：高光时长、起点、已发布句段、强调词），调用 `composition_prompt` 组装提示词，
   然后发 `model.generate_text` 请求；
2. **model 工具层（Rust 持 token）**：`model.generate_text` 接收该提示词 → 直连百炼 →
   返回 HTML → **必须**过 `validate_effect_caption` 白名单校验才回传给 skill。

**这一步同时落地步骤 3 任务 2.4**：新增 `UnifiedCallPointCompositionProvider`，
实现 `EffectCaptionCompositionProvider`，内部用 `ModelCallRuntime` 取 token + 冻结快照，
把模型返回的 HTML 交给 `validate_effect_caption` 校验（校验失败即拒绝，不落库——对应步骤 3
任务 3.9 的既有语义）。这样组合生成的模型调用就是"客户端直连百炼的短期 token"，且提示词与
响应正文不写普通日志（沿用 S1 诊断的脱敏约束）。

> 结论：Q2 的「组合生成 skill 作为第一个真实 skill」成立，但**成立方式**是分层——
> skill 编排在 Harness，模型调用与白名单在 Rust。这既给 Harness 真实业务，又不违反
> 「单次结构化输出留在 Rust」的既有决策。

## 决策十七：零 npm 依赖的 Node Harness 程序

- 纯 Node 内置模块（`node:readline` / `node:fs` / `JSON`），**不引入任何 npm 依赖**，
  供应链面 = 一个 Node 22 LTS tarball + 本仓库源码。
- 组件：stdio JSON-RPC 服务端（对齐 `ai/harness.rs` 的 `HARNESS_PROTOCOL_VERSION="1"`、
  换行分隔、单条 ≤1MiB）、Profile 加载器（只读随包 profile，忽略用户目录）、
  Skill 执行器（跑 skill 的工具循环）、`tool.invoke` 客户端（把受控工具请求发回 Rust）。
- 状态权威在 Rust：sidecar 运行记录只用于诊断，崩溃/超时/取消由 Rust 判定
  （`HarnessSidecar` / `HarnessError` 已就位）。

## 决策十八：资产与分发

| 资产 | 方式 | 理由 |
| --- | --- | --- |
| Harness 程序 + Profile + 5 精彩 Skill + 2 转场 Skill | **随包**（很小，几十 KB） | 断网/离线首启也能跑编排 |
| Node 22 LTS 运行时 | **按平台下载 + 本地缓存** | 30–50MB×3，避免安装包膨胀；按平台只下当份 |
| 断网降级 | 新分支不可用、基础能力（录制/浏览/预览/本地 ASR）不受影响 | 决策十四 |

- Node 锁 22 LTS 官方 tarball（nodejs.org）+ SHA256 固定进 Runtime Resource Pack 清单；
  本机只完整端到端 mac arm64；Intel/Windows 的下载+哈希脚本写好，留 CI
  `macos-15-intel` / `windows-2022`。
- 组件 `harness.runtime`（Node）走下载、`harness.profile` / `harness.skill`（文件）走随包，
  三者都已登记进 `ENHANCEMENT_COMPONENTS`（任务 1.2）。

## 验收载体与执行顺序

1. `model.generate_text` 受控工具 + `UnifiedCallPointCompositionProvider`（Rust，落地 2.4）+ 测试；
2. 生成 Profile 模板 + 5 精彩 Skill + 2 转场 Skill 文件（`resources/harness-source/`）；
3. 零依赖 Node Harness 程序（`harness/`），本机 v26 验证协议与工具循环；
4. 组合生成 skill 端到端：Harness 编排 → `model.generate_text` → Rust 持 token → 白名单校验 → 回传；
5. 锁 Node 22 LTS arm64 tarball + SHA256，接 Runtime Resource Pack 下载器；
6. Intel/Windows 脚本 + CI 改动，留证据。
