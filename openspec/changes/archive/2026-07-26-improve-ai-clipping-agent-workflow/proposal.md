## Why

当前本地 ASR 调度器在删除运行中项目后可能遗留无法推进的队列状态，也不支持用户改变待处理视频的执行顺序；同时，已生成的时间戳文本尚不能转化为可预览、可评分和可选择的高光候选。项目需要先修复可取消、可重排的任务生命周期，再在用户明确授权后通过 Rust Agent 安全接入 DeepSeek，把主播标签和项目目标转化为稳定、可恢复的高光分析工作流。

## What Changes

- 修复删除排队或运行项目后的调度器释放、旧事件隔离和后续任务继续执行问题；删除过程先取消并有界等待当前子进程和队列项退出，再清理项目数据。
- 将全局单 worker FIFO 改为可观察、可重排且支持抢占的持久化队列，提供“下一个处理”和经确认的“立即切换”；活动录制继续拥有最高资源优先级。
- 增加 DeepSeek 设置、模型自定义、连接测试和密钥状态；API Key 只保存到系统凭据库，SQLite 仅保存非敏感 Provider 配置。
- 使用 Rig Rust SDK 和 DeepSeek 的 OpenAI 兼容接口实现受限 Agent，不引入 Node.js、浏览器端密钥或 Node sidecar。
- 增加可组合分析 Skills：通用爆点、带货转化、搞笑包袱、知识密度和故事情绪；根据主播标签快照及项目级标签确定性选择和组合。
- 增加候选发现 Agent 与全局评分 Agent，复用同一用户模型配置，并由 Rust 固定执行分块、候选校验、去重、复评、取消、重试、恢复和缓存流程。
- 高光候选输出 0–100 总分、维度分、理由、标签命中和绑定的 ASR 句段/源视频时间范围；默认保留前 10 条且总分不低于 70。
- 在 AI 剪辑工作区增加队列控制、Provider 设置、分析授权、工作流进度和高光候选列表；候选支持排序、播放器跳转、预览、勾选和保存。
- 普通 CI 只使用 fake Provider 和确定性 fixture，不调用真实 DeepSeek；真实 API 验收由显式命令触发并要求用户自行配置 Key。
- 本次不执行 FFmpeg 成品切片或视频导出，不实现开放式多 Agent swarm、任意 Provider 地址、任意 Tool/MCP、自动上传、自动高光分析或思维链保存。

## Capabilities

### New Capabilities

- `llm-provider-configuration`: 定义 DeepSeek Provider 的系统凭据存储、非敏感设置、模型选择、连接诊断、请求安全和用户授权边界。
- `ai-highlight-agent-workflow`: 定义 Rig Agent、可组合 Skills、转写分块、候选发现、全局评分、结构化校验、恢复缓存和候选选择行为。

### Modified Capabilities

- `ai-analysis-projects`: 增加安全删除运行项目、全局任务重排/抢占、项目级标签、高光分析阶段和候选交互。
- `local-speech-transcription`: 将不可重排 FIFO 调度调整为删除可释放、可观察、可优先和可抢占的全局单并发队列。
- `streamer-tagging`: 允许在用户主动启动高光分析时将主播标签快照作为 Agent Skill 选择与评分上下文。
- `desktop-client-shell`: 增加 DeepSeek 设置、队列控制、高光工作流状态和候选列表界面，同时保持敏感配置不进入浏览器演示数据。

## Impact

- Rust 核心：重构 `TranscriptionScheduler` 队列模型，增加任务代次、优先级、抢占、状态快照和删除确认协议。
- Tauri 后端：扩展 AI repository、migration、commands、事件与 shutdown；新增 LLM Provider、系统凭据、Rig Agent、Skills、工作流和结构化高光结果模块。
- React：扩展 AI 剪辑工作区和设置页的队列、Provider、工作流与高光候选交互，保持播放器和 ASR 时间戳联动。
- 依赖：新增 Rig 及系统凭据库相关 Rust crate；不新增 Node 运行时或 `vercel/ai`。
- 数据：SQLite 新增非敏感 LLM 设置、高光运行、分块、候选、评分、选择和标签快照；API Key、原始请求、原始响应和模型思维链不落库。
- 网络与隐私：只有用户显式启动高光分析后才发送必要的脱敏转写和标签到 DeepSeek；视频、音频、本地路径和签名流地址始终保留在本机。
