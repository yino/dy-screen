## Why

云端 ASR 目前处于“令牌链路已通、能力实际不可用”的状态。桌面运行时把临时音频发布器硬编码为不可用实现（`src-tauri/src/ai/desktop_runtime.rs` 注入 `UnavailableTemporaryAudioPublisher`），运行时诊断的 `cloud_asr_available` 恒为 `false`，因此每一次转写实际都走本地 `whisper.cpp`。同一条路径上还有两个前置门禁让“云端优先”无法成立：本地 VAD 无条件先于云端执行，识别配置身份校验要求云端结果沿用它委托的本地引擎身份。

本 change 按 v2 的“客户端直连百炼”口径把这些契约落到真正可用的云端路径上：客户端用既有的 `GET /api/client/ai/llm-ak` 短期 token 直连阿里云百炼，自己完成音频直传、异步任务提交与轮询；不再申请服务端上传凭证，也不再把任务交给服务端。同时修正受影响的识别身份、预检、失败与取消语义，并保留 macOS Intel x86_64 的平台承诺。

## What Changes

- 实现临时音频直传百炼：用 `ai/llm-ak` 的短期 token 直连百炼完成音频上传/提交（具体路径以 `validate-dashscope-direct-capabilities` 的探针结论为准），替换桌面运行时中硬编码的不可用发布器，并让诊断反映真实可用性。
- 复用并收口客户端已有的百炼异步文件转写适配（`POST /api/v1/services/audio/asr/transcription`、`X-DashScope-Async: enable`、`GET /api/v1/tasks/{task_id}`）：提交、轮询与结果下载都由客户端完成，不再依赖服务端任务接口。
- 本地幂等记账：付费提交前在本地事务中持久化稳定 `requestId`、任务类型、输入指纹与上游 `task_id`；同键同输入复用，同键不同输入拒绝。**不新增任何服务端接口**，未开通与额度耗尽由上游业务错误码 3001 / 3003 / 3004 判定（heartbeat 响应不含额度字段）。
- 拆分预检为百炼直连转写、可选本地识别与音频准备三项独立诊断；缺少本地 `whisper.cpp` 模型不再阻止云端识别。
- 超过百炼单任务上限时按有重叠的固定区间分块，逐块上传并各自提交子任务；取得全部子结果后把块内时间还原为源视频时间，按文本与时间交叠去重，再为合并句段分配稳定 ID。不得删除静音后直接拼接。
- 转写产物记录真实的 `provider`、模型、模型版本、分块策略与规范化版本，缓存键包含这些字段；旧产物保持原身份，不改写为云端结果。
- **失败与取消语义（不再有本地回退）**：用户取消直接终止；授权、未开通与额度错误显式报告且不被描述为云端故障；瞬时网络与云端故障在有界重试后**明确失败**，不自动切换本地 `whisper.cpp`。本地识别路径仍可由用户显式选择使用，但不由云端路径自动降级进入。
- 云端转写成功但本地临时音频清理失败时保留成功结果并重试清理，不重新付费转写。
- **平台承诺保留 Intel**：目标桌面平台为 macOS arm64、macOS Intel x86_64 与 Windows x64；删除前一版“收敛为 macOS arm64 与 Windows x64”的条款，并要求随包的 Harness sidecar、渲染浏览器与运行资源同样提供 Intel 构建。
- 首稿时效定义收敛：把 10 分钟目标明确为覆盖基础草稿（高光与基础字幕）可审阅，增强结果异步补齐并可后审阅。
- **BREAKING**：客户端不再申请服务端上传凭证、不再创建服务端 ASR 任务；`add-cloud-ai-job-backend` 已整体搁置（仅保留 `requestId` 幂等语义与 `api.rs` 契约对齐方式）。
- 非目标：本次不引入 Harness sidecar 的功能改造、组合渲染；不实现特效字幕与生成式动画；不改变录制、预览、封面与转场素材的现有行为；不新增本地回退模型；不改变现有统一 ASR 产物结构对导出与复用的兼容性；不修改 `src-tauri/src/api.rs` 中既有三个接口之外的任何服务端契约。

## Capabilities

### New Capabilities

无。

### Modified Capabilities

- `cloud-asr-provider`: 临时音频从“资格判定”变为客户端直连百炼的受限上传与资源引用；支持有重叠分块与源时间还原；记录真实转写身份与缓存键；取消、授权额度与瞬时故障不再回退本地；本地 `requestId` 幂等替代服务端任务找回。
- `local-speech-transcription`: VAD 不再是云端路径的前置门禁；识别配置身份依据实际执行引擎且不得委托冒用；缺少本地模型不阻止云端；缓存键纳入转写身份字段；取消直接终止；本地识别仅作为用户显式选择，不作为云端失败的自动回退。
- `smart-clipping-workflows`: 目标桌面发行平台为 macOS arm64、macOS Intel x86_64 与 Windows x64；10 分钟首稿时效明确为覆盖基础草稿。

## Impact

- 云端路径：`src/asr/cloud.rs`（发布器实现、百炼提交与轮询、分块与合并、失败分类），`src-tauri/src/ai/desktop_runtime.rs`（依赖注入与三项独立诊断），`src-tauri/src/ai/processor.rs`（VAD 门禁、识别配置身份校验、上传与清理调用）。
- 数据与迁移：`src-tauri/src/ai/repository.rs` 需要为转写身份、分块策略与本地幂等记录新增列与迁移（当前最新迁移版本为 25），并保证旧产物身份不变。
- 服务端依赖：**无新增接口**。仅复用既有的 `POST /api/client/activate`、`POST /api/client/heartbeat` 与 `GET /api/client/ai/llm-ak`；ASR 调用不经服务端中转。
- 移除的依赖：服务端上传凭证、服务端 ASR 任务创建/查询/取消接口（整体搁置于 `add-cloud-ai-job-backend`）。
- 平台与发行：保留 Intel 后，需要核对 README、验收记录与发行清单中 Intel 的表述，并确认 sidecar、渲染浏览器与 Harness 均有 Intel 构建。
- 隐私：短期 token 只在内存中持有；不得把 token、Authorization Header、完整音频 URL、签名下载地址与本地路径写入数据库、普通日志、诊断事件与前端状态。
