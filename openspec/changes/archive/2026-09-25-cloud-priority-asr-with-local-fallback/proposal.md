## Why

当前 ASR 完全依赖本地 `whisper.cpp` 模型，识别能力受本机资源、模型准备状态和 CPU/GPU 性能限制。后端已经提供 `/api/client/ai/llm-ak` 临时 AI 密钥接口，并约定阿里云百炼作为 `ALI` provider；接入该能力可以在授权和配额可用时提高转写效率，同时保留本地模型作为离线和故障回退路径。

## What Changes

- 新增 ASR 云端优先策略：处理输入时先通过 `/api/client/ai/llm-ak` 获取短期密钥，返回 `provider=ALI` 且密钥未过期时尝试阿里云百炼 ASR。
- 新增阿里云百炼 `qwen3-asr-flash-filetrans` 异步文件转写适配，包括任务提交、有限轮询、结果下载、取消和统一结果转换。
- 增加临时音频发布器抽象和运行时能力探测，使未来后端上传或对象存储实现能够提供百炼可访问的短时 HTTPS `file_url`；本次不绑定具体部署方案，未配置发布器时不得把本地路径发送给百炼并自动回退本地。
- 云端密钥获取失败、云端任务失败、网络异常或上游不可用时回退现有本地 `whisper.cpp`，并保留可解释的 provider/fallback 状态。
- 增加项目后端 AI 授权、次数/时长额度错误（`3001`~`3004`）的中文提示，并区分百炼上游 HTTP/API 错误。
- 临时密钥只允许在内存中短期使用，不写入数据库、日志、诊断事件或前端状态。
- 云端和本地结果继续使用统一 ASR 结果、时间轴、缓存和 SQLite 发布契约。

## Capabilities

### New Capabilities

- `cloud-asr-provider`: 定义 AI 临时密钥获取、阿里云百炼异步 ASR、短期音频 URL、云端优先和本地回退行为。

### Modified Capabilities

- `local-speech-transcription`: 修改 ASR 引擎选择、隐私、资源门禁、错误提示和统一产物要求，使本地引擎成为云端不可用时的受控回退，而不破坏原有本地识别能力。

## Impact

- Rust API、ASR engine 抽象、AI processor/scheduler、临时音频生命周期和运行时配置。
- `/api/client/ai/llm-ak` 客户端调用及现有 envelope、鉴权请求头和错误映射。
- 阿里云百炼网络依赖、异步任务轮询、取消/超时和上游错误脱敏。
- SQLite ASR artifact/transcript 元数据与前端任务状态、provider 和用户提示展示。
- 后端或对象存储部署配置：未来可实现受控的临时音频 URL 发布器；本次变更不要求具体上传服务，未配置时仍必须安全回退本地 ASR。

非目标：不下发长期 `DASHSCOPE_API_KEY`；不改变本地模型资源校验；不支持任意第三方 ASR provider；不把本地文件路径直接作为云端 URL；不绕过授权、次数或时长配额。
