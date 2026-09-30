## Context

见 `proposal.md` 的 Why。与探针设计相关的现状约束：

- 客户端现有的服务端 AI 接口只有三个：`GET client/activate`、`GET client/heartbeat`、`GET client/ai/llm-ak`。本 change 不新增任何接口。
- `GET client/ai/llm-ak` 返回 `{ak, expireAt, provider}`，实测 `provider` 为 `ALI`，`ak` 形如 `st-lt-v1.<base64url>`，长度 52。客户端已实现 30 秒提前失效的内存缓存（`src-tauri/src/api.rs` 的 `AiLlmAkCache`）。
- 客户端已有完整的百炼异步文件转写适配（`src/asr/cloud.rs`，675 行）：提交、轮询、结果解析、错误映射、`CloudFirstAsrEngine` 路由；缺的只是 `TemporaryAudioPublisher` 的实现（现为 `UnavailableTemporaryAudioPublisher`）。
- `TemporaryAudioReference::validate()` 只接受 `https` 方案的引用，且要求非空 `lease_id`。该约束在探针前就与本 change 的实测结论产生冲突。
- 本机已存在可用激活凭据（`client_activation_secrets`），因此探针不需要人工提供凭据。

## Goals / Non-Goals

**Goals:**

- 取得四类能力的可复现实测结论，并明确每项是"可用""不可用"还是"需要架构调整"。
- 把"上传成功"与"可被转写服务消费"这两件事明确区分开，避免下游 change 建立在错误前提上。
- 冻结上游幂等能力与记账依据两个事实，供客户端本地去重与记账规格直接引用。

**Non-Goals:**

- 不实现客户端生产代码，不修改 `src/`、`src-tauri/`、`ui/`。
- 不修改我方服务端，不新增接口。
- 不评估渲染后端表现力、耗时与体积（属 `add-effect-caption-renderer` 的进入条件）。
- 不做图片与视频 Provider 选型（已定为万相）。

## Decisions

### 决策一：探针用独立 HTTP 调用完成，不复用也不改动客户端代码

**选择**：以 curl 与一次性脚本直接调用上游端点，凭据从本机激活凭据换取，全部产物落在受限权限的临时目录。

**理由**：本 change 的职责是取得事实，不是实现通路。复用客户端代码会把"探针失败"与"客户端实现错误"混在一起，无法判定到底是上游能力不支持还是代码有 bug。独立调用让每个结论都对应一条可复现的请求。

**已考虑的替代方案**：先在客户端实现 `TemporaryAudioPublisher` 再跑通链路。被否决：一旦上游能力不支持，这部分实现就是纯浪费，且会掩盖真实结论。

### 决策二：能力判定以"上游终态"为准，不以提交是否被接受为准

**选择**：提交返回 `http=200` 与 `task_id` 只算"被受理"，只有轮询到 `SUCCEEDED` 才算"可用"。

**理由**：实测中所有失败形态在提交阶段都返回 `200` 与 `task_id`。若以提交成功为判据，会得出与事实完全相反的结论。这一点在文件转写与对象存储引用形态上尤其关键。

**已考虑的替代方案**：以 HTTP 状态码判定。被否决：见上，提交阶段的状态码不携带判别信息。

### 决策三：能力矩阵按"能力 × 凭据"而非"端点 × HTTP 码"组织

**选择**：结论按四类能力分别冻结，每项记录终态、脱敏错误码与用量字段。

**理由**：下游 change 是按能力组织的（转写、动画、字幕、LLM 链路），按端点组织无法直接映射到实施前置条件。

### 实测结论与证据

以下结论均在 2026-09-30 以 `provider=ALI` 的真实短期凭据取得。凭据值不记录。

**可用项：**

1. **通义兼容模式对话** —— `POST https://dashscope.aliyuncs.com/compatible-mode/v1/chat/completions`，模型 `qwen-plus`，返回 `http=200` 与正常回复；`usage` 返回 `prompt_tokens`/`completion_tokens`/`total_tokens`。结论：三条既有 LLM 链路改用临时凭据可行。
2. **万相文生图** —— `POST /api/v1/services/aigc/text2image/image-synthesis`，模型 `wanx2.1-t2i-turbo`，终态 `SUCCEEDED`，`usage` 返回 `{"image_count":1}`，`output.task_metrics` 返回成功/失败计数。
3. **万相图生视频** —— `POST /api/v1/services/aigc/video-generation/video-synthesis`，模型 `wanx2.1-i2v-turbo`，终态 `SUCCEEDED`，`usage` 返回 `{"video_duration":5,"video_ratio":"standard","video_count":1}`。

4. **百炼文件转写（客户端直传，三步流程）** —— 可用。经实测确认的完整流程：

| 步骤 | 请求 | 实测结果 |
| --- | --- | --- |
| 1 | `POST /api/v1/files`（multipart 直传本地 WAV） | 返回 `file_id`，例如 `0c98ec55-efef-4ea9-a9bb-4573c7a1449b` |
| 2 | `GET /api/v1/files/{file_id}` | 响应 `data.url` 为带 STS `security-token` 的 OSS 签名临时 URL（含 `Expires`、`OSSAccessKeyId=STS.*`、`Signature`，约 938 字符） |
| 3 | `POST /api/v1/services/audio/asr/transcription`，`input.file_url` 取步骤 2 的临时 URL | 终态 `SUCCEEDED`，`usage` 返回 `{"seconds":4}`，`output.result.transcription_url` 指向结果 JSON |

以真实中文语音样本验证，结果 JSON 为 `{file_url, audio_info:{format:"pcm_s16le", sample_rate:16000}, transcripts:[{channel_id, text, sentences:[...]}]}`，句段内容为 `[100ms -> 3820ms] "hello world，这里是阿里巴巴语音实验室。"`。**结论：云端转写在客户端直连口径下可实施，不需要任何服务端改动。**

**过程中被证伪的两条中间结论（保留记录，避免重复踩坑）：**

- 直接以 `file_id` 作为 `file_url` 提交 → `FILE_DOWNLOAD_FAILED`。缺的不是能力，而是中间必须先用 `GET /api/v1/files/{file_id}` 取回签名 URL。
- 以 `file://<file_id>` 提交 → `DECODE_ERROR`。该错误表示"已读取但内容不可解码"，不是"读不到"，不可据此判定引用形态不可用。

**另一条上传路径（`GET /uploads?action=getPolicy` 加对象存储表单直传）确认不适用于转写：**

该路径把对象访问级别固定为私有（策略条件中 `x-oss-object-acl` 为 `private`，以 `public-read` 上传会被策略明确拒绝：`Policy Condition failed: ["eq", "$x-oss-object-acl", "private"]`）。实测引用形态与结果：

| 引用形态 | 终态错误码 |
| --- | --- |
| 对象存储 `oss://<key>` | `FILE_DOWNLOAD_FAILED` |
| 对象存储 HTTPS 直链 | `FILE_403_FORBIDDEN` |

官方 SDK 源码（`dashscope/utils/oss_utils.py`）证实该路径的键构造与引用形态是给当前工作空间内部服务消费的（多模态对话传图），不是为外部拉取的转写服务提供输入。**因此转写必须走 `POST /api/v1/files` + `GET /api/v1/files/{id}` 的三步流程，而不是 `getPolicy` 表单直传。**

**结果结构兼容性（已验证）：**

结果 JSON 的实际结构为 `{file_url, audio_info:{format, sample_rate}, transcripts:[{channel_id, text, sentences:[{sentence_id, begin_time, end_time, language, emotion, text}]}]}`，其中 `begin_time`/`end_time` 为毫秒。该结构与客户端既有解析器 `parse_dashscope_segments`（读取 `transcripts[].sentences[]` 的 `begin_time`/`end_time`/`text`）一致，无需改写解析逻辑。

**一项已验证的规格前提：**

当前模型返回的句段不含词级时间戳（`sentences[]` 只有句级 `begin_time`/`end_time`）。这直接支持 `add-effect-caption-renderer` 已冻结的要求：逐字动画在只有句级 ASR 时间时只能按字素分配视觉节奏，不得被描述为真实逐字语音对齐。

**结论性事实：**

5. **上游不具备调用方幂等能力** —— 以相同输入与相同自定义幂等键连续提交两次异步任务，返回两个不同的 `task_id`；响应头中不存在幂等相关字段。因此客户端本地 `requestId` + 输入指纹是去重的唯一承担者。
6. **记账号只在上游响应中** —— 三类成功调用的用量字段均出现在响应体的 `usage` 中（LLM 为 token，图片为张数，视频为条数与秒数）。请求体中不存在任何可被采信的价格或额度字段。

### 决策四：转写音频走三步直传流程，`getPolicy` 表单路径不作为转写输入

**选择**：客户端以 `POST /api/v1/files` 直传标准音频取得 `file_id`，再以 `GET /api/v1/files/{file_id}` 取回带 STS `security-token` 的签名临时 URL，把该 URL 作为 `input.file_url` 提交转写。不使用 `GET /uploads?action=getPolicy` 的表单直传结果作为转写输入。

**理由**：实测确认三步流程可用且不需要任何服务端改动；`getPolicy` 路径的对象被策略锁定为私有，转写服务无法读取。两者的差异是对象可达性，不是转写能力。

**已考虑的替代方案**：
1. 服务端新增一个只读接口把音频换成可读 URL。被否决：三步流程已证明客户端可独立完成，无需为此破坏"零服务端新增接口"的口径。
2. 客户端持有自备的公网可读对象存储。被否决：分发与配置复杂度高，且用户需自备云资源。
3. 本轮放弃云端转写、只做本地识别。被否决：与"本轮不做本地回退"冲突，且失败点已被证明不存在。

**前提与约束（必须在实现中处理）：**

- 临时 URL 由上游签发，有效期约 24 小时，可在一次任务内复用；但仍 SHALL 在每次提交前取回，不复用长期缓存。
- 临时 URL 为 `http` 方案，而客户端现有 `TemporaryAudioReference::validate()` 只接受 `https`。该约束 SHALL 针对上游签发的临时 URL 放宽，或改为校验"由上游签发且含签名与有效期参数"；放宽 MUST NOT 波及任何用户可输入的地址。这是本 change 对客户端实现的唯一真实约束。
- `POST /api/v1/files` 直传与 `getPolicy` 表单直传是两条独立路径，MUST NOT 混用其产物。
- 结果结构中的 `audio_info` 为 `pcm_s16le`/16000 Hz，与客户端音频准备规格一致。

## Risks / Trade-offs

- [结论有时效性] 上游策略与能力可能变化 → 结论记录取得日期与具体端点；任一结论变化时只需重跑探针，不影响已冻结的规格结构。
- [付费调用不可撤销] 探针产生真实费用 → 已按最小粒度提交（单张图片、单条 5 秒视频）；不再重复提交；失败形态全部集中在提交后即失败，不产生额外计费。
- [凭据泄漏面] 探针需要真实凭据 → 凭据只存在内存与受限权限临时文件；报告、规格与任务中不出现凭据值、完整签名 URL 与上游任务 ID。已删除本地凭据临时文件并复核仓库无凭据。
- [签名临时 URL 的时效边界] 临时 URL 内含 STS `security-token`，有效期由上游决定 → 不作为长期缓存使用，每次提交前取回；该 URL 也不得进入数据库、日志、诊断事件与前端状态。
- [转写质量未验证] 探针只验证了"可上传、可下载、可解码、可产出句段"，用的是上游官方中文示例音频 → 中文长短样本、口音、噪声与分块边界去重的质量验证留给下游 change。
- [`.env` 指向公网 IP 与明文 HTTP] 探针经现有客户端同款链路取值，该链路未加密 → 该现状不在本 change 范围内，但在音频内容进入该链路前必须重新评估。
- [放宽引用方案校验的边界风险] 为接受上游签发的 `http` 临时 URL 需要放宽既有 `https` 校验 → 放宽规则必须限定为"由上游签发且含签名与有效期参数"，并新增"用户可输入地址被拒绝"的负向测试，否则会打开任意地址输入的缺口。

## Migration Plan

1. 取得本机激活凭据并换取短期凭据（不落库、不入日志）。
2. 逐类能力提交并轮询至终态，记录终态、错误码与用量。
3. 汇总为能力矩阵，与本 change 的规格一并冻结。
4. 把结论回写到受影响的 change：`wire-cloud-asr-upload-and-jobs` 的音频三步直传流程与引用方案约束、`add-generative-video-animation` 的万相直连确认、`add-model-gateway-and-harness-sidecar` 的三条链路凭据来源。
5. 回滚：本 change 只产出文档与规格，不引入运行时行为，回滚即删除 change 目录，不影响任何既有能力。
