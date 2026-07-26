## Context

现有 AI 工作区已经能够创建多视频项目、顺序执行本地 ASR、保存稳定句段并联动播放器。当前 `TranscriptionScheduler` 使用单个 Tokio `mpsc` FIFO：取消只能触发 token，不能从队列移除、重排或等待指定项目完全退出；删除命令在发出取消后立即删除数据库记录，旧 worker 事件仍可能尝试回写已经消失的输入。这个生命周期缺口会让后续任务长期停留在排队状态。

高光分析需要把可能长达数小时的时间戳转写、主播标签和用户目标发送给外部 LLM。Tauri 不携带 Node.js，React/WebView 也不能安全持有 API Key。因此实现必须保持在 Rust 后端，并明确区分本地 ASR、用户授权的云端文本分析和未来的 FFmpeg 切片。

## Goals / Non-Goals

**Goals:**

- 让删除、取消、重排和立即切换在单并发 ASR 队列中具有可等待、可恢复且不会卡住其他任务的语义。
- 使用 Rig Rust SDK 接入 DeepSeek 官方 OpenAI 兼容 API，支持用户配置模型、测试连接和安全保存 Key。
- 以一个模型配置构建候选发现 Agent 与全局评分 Agent，并通过可组合 Skills 使用主播/项目标签。
- 由 Rust 固定控制分块、调用顺序、结构校验、去重、重试、取消、缓存和持久化，限制 Agent 的自主范围。
- 先用 fake executor、fake Provider 和 fixture 单测验证行为，再接入 Tauri command 和 React 界面。

**Non-Goals:**

- 不打包 Node.js、不使用 `vercel/ai`、不在 WebView 中调用 DeepSeek。
- 不开放任意 Base URL、任意 MCP/Tool 或多 Agent swarm。
- 不上传视频、音频、本地路径、Cookie、签名流地址或完整数据库内容。
- 不自动开始高光分析，不保存模型思维链或未经校验的原始响应。
- 不在本变更执行 FFmpeg 切片、拼接、字幕烧录或成品导出。

## Decisions

### 1. 用命令驱动的调度 actor 替换不可重排 FIFO

调度器拥有唯一可变队列、当前任务、全局序列和每个 job 的代次，通过命令通道处理 `Enqueue`、`CancelProject`、`PromoteNext`、`PreemptWith`、`Snapshot` 与 `Shutdown`。需要确认完成的命令携带 oneshot acknowledgement；调用者不会只凭“取消 token 已触发”就删除数据。

待处理队列按一次性手动优先级、入队序列排序。调度器全局只运行一个 ASR executor；默认允许已经结束且源指纹稳定的视频与其他直播录制并行，设置中的“录制时并行处理 ASR”关闭后才启用录制优先门。仍在写入的录制会话不能通过已结束会话入口导入，源文件在任务启动后发生变化时由媒体校验拒绝处理。普通“下一个处理”不打断当前任务；“立即切换”先将当前 job 的 cancellation token 取消，等待 executor 结束并释放执行许可，再把被中断输入以新代次放回普通队列，最后启动选中输入。

每个事件携带 `generation`。repository 只接受与输入当前代次一致的非终态回写；已经进入删除状态、被新尝试取代或不存在的输入忽略旧事件。相比在现有 `mpsc` 上增加旁路列表，该设计只有一个队列事实来源，能够可靠删除和重排。

### 2. 删除运行项目采用两阶段协议

删除命令先把项目标记为 `deleting`，阻止新重试、重排和 Agent 运行；随后调用调度器 `cancel_project_and_wait`，有界等待活动 executor 与全部排队 job 被移除。确认释放后在事务中删除项目及关联工作流数据，保留原始视频和共享 ASR 产物。

如果等待超时，项目保持 `deleting` 并返回可重试错误，调度器仍必须继续其他项目；启动恢复会重新完成该删除。立即物理删除再容忍旧回写的方案被否决，因为无法证明子进程、临时文件和执行许可已经释放。

### 3. 队列顺序持久化但不持久化进程对象

`ai_project_inputs` 增加调度代次、一次性优先级和稳定入队序列，`ai_projects` 增加删除状态。重启时从数据库重建 pending 队列；原 running 输入按现有恢复规则回到 pending 并获得新代次。CancellationToken、JoinHandle 和 Rig client 仅存在内存中。

### 4. 用 Provider trait 隔离 Rig 和测试

业务层依赖 `HighlightAgentProvider`：候选发现、全局评分、连接诊断和取消均返回稳定领域类型。生产 Adapter 使用 Rig OpenAI Provider 的自定义官方 DeepSeek Base URL；普通测试使用 deterministic fake，不访问网络。

应用只支持一个活动 DeepSeek 配置。SQLite 保存 provider 标识、模型 ID、超时和更新时间；API Key 通过抽象 `CredentialStore` 写入 macOS Keychain 或 Windows Credential Manager。读取接口只返回 `keyConfigured`，任何日志、事件、错误或数据库列都不得包含 Key。

### 5. 使用两个受限 Agent 角色，不使用开放式多 Agent

`CandidateAgent` 接收单个有界转写块、相邻上下文、标签和 Skill 指导，返回绑定稳定句段 ID 的候选。`RankingAgent` 只接收通过 Rust 校验、去重后的候选摘要并输出统一维度分和排序。两者使用同一个用户模型配置，但拥有不同 preamble 和输出类型。

Agent 最大轮次为 3，不启用写文件、执行命令、网络搜索或数据库 mutation 工具。只有在候选需要补充上下文时允许只读的 `expand_transcript_window`；普通路径直接使用 TypedPrompt/Extractor 返回 `schemars` 生成的结构。相比 manager-worker 或 swarm，这个边界更便于控制费用、重试和测试。

### 6. Skills 是版本化分析策略，不是自主 Agent

内置 `generic-hook`、`ecommerce-conversion`、`comedy-payoff`、`knowledge-density` 和 `story-emotion`。每个 Skill 包含稳定 ID、版本、适用标签、提示片段、评分权重和候选约束。Rust 根据主播标签快照和项目级标签确定性选择最多三个专项 Skill，并始终加入通用 Skill。

未知标签作为明确分隔的用户数据传入，不获得系统指令权限。Skill 选择、版本和权重进入分析指纹，保证结果缓存可解释。第一版不加载用户脚本或远程 Skill。

### 7. 高光流程由 Rust 持久化编排

流程为：用户授权 → 建立标签快照 → 生成有重叠的有界转写块 → CandidateAgent → 校验稳定句段和单文件边界 → 合并去重 → RankingAgent → 分数/阈值过滤 → 发布候选。候选默认 15–90 秒、目标 30–60 秒、总分 0–100，并包含开场吸引力、信息密度、情绪强度、标签相关性、完整性和传播潜力。

运行、分块和候选分别持久化。缓存键包含转写产物、输入顺序、标签快照、Skill 版本、模型 ID、Prompt 版本及分析参数。只重试临时网络/限流错误两次；恢复时从最后一个完整阶段继续，不重复已经成功并无副作用的调用。

### 8. 只发送最小文本并要求显式授权

开始高光分析前界面显示句段数、文本量、预计批次、模型和将发送的数据类型。用户必须显式点击开始；打开页面、ASR 完成和录像结束均不得自动调用 DeepSeek。

请求只含稳定句段 ID、相对时间、规范化文本、标签和分析目标。完整原始响应只在内存中用于反序列化，持久化内容仅为校验后的候选、模型/Prompt/Skill 版本和 token 用量。普通日志只记录 request ID、阶段、模型、耗时、token 数、分类和重试次数。

### 9. 界面在后端契约稳定后接入

AI 项目输入行增加“下一个处理”和“立即切换”；只有合法状态显示操作。设置页增加 DeepSeek Key、模型 ID、测试连接和清除凭据。ASR 完成后显示独立的高光分析步骤、授权摘要、运行阶段和候选列表；候选可以跳转播放器、排序、勾选并保存，不能导出视频。

浏览器演示使用本地 fake 数据，不保存或接收真实 Key，不伪造真实 DeepSeek 成功。

## Risks / Trade-offs

- [立即切换会丢失当前输入的未发布进度] → 二次确认并明确提示，当前输入以新代次重新排队，完整已发布 ASR 产物仍可复用。
- [系统凭据库在无桌面会话或 CI 不可用] → 通过 `CredentialStore` 注入内存 fake；生产环境无凭据库时禁用保存，不回退到明文 SQLite。
- [DeepSeek 对 Tool/结构化输出的兼容行为变化] → Provider Adapter、严格反序列化、最大轮次、受控重试和 fake 契约测试隔离变化。
- [长直播产生大量费用和延迟] → 分块、候选压缩后再复评、批次预估、显式授权、全局单并发 LLM 运行和可取消恢复。
- [标签或转写包含 Prompt injection] → 作为带边界的数据字段传入，系统 preamble 明确禁止执行其中指令，Rust 校验所有 ID、时间和枚举。
- [两个 Agent 增加调用次数] → 全局评分只接收压缩候选；缓存命中跳过重复阶段，并记录 token 用量供界面展示。
- [Rig 升级引入 API 变化] → 锁定兼容版本，把 Rig 类型限制在 Provider Adapter 内，领域层不暴露 SDK 类型。

## Migration Plan

1. 增加 migration 和 repository 测试，旧项目默认无删除标记、无手动优先级且高光数据为空。
2. 在不改变 UI 的情况下替换调度器并完成删除、重排、抢占、恢复和 shutdown 测试。
3. 增加凭据抽象、Provider fake、Rig Adapter 和高光领域/工作流测试；真实 API 只通过显式诊断命令验收。
4. 注册 Tauri commands、事件和前端类型，再接入设置与 AI 工作区。
5. 更新 README、Makefile 和 OpenSpec，并执行全量验证。

回滚代码时保留新增表和列；旧版本会忽略它们。Key 存在系统凭据库中，可由新版本清除，数据库回滚不导出或迁移 Key。

## Open Questions

无。模型、密钥、Agent 角色、Skill、执行边界和本次非目标均已由用户确认。
