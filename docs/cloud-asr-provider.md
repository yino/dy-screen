# 云端 ASR 接入说明（客户端直连百炼）

客户端**不经过自有服务端**：它用既有接口 `GET /api/client/ai/llm-ak` 取得的 `provider=ALI` 短期
token，在内存中直连阿里云百炼，完成音频直传、异步文件转写任务提交、轮询与结果下载。

服务端本轮不新增任何接口，也没有服务端音频中转、对象存储或上传凭证；可用的仍是既有激活、心跳与
`ai/llm-ak` 三个接口。客户端也不需要对象存储 SDK。

## 当前行为

- ASR 任务保留现有 FFmpeg、VAD、本地模型、缓存、单并发和恢复流程，识别产物沿用统一 `AsrResult` 契约。
- 默认识别路径是百炼直连；只有工程识别配置显式选择本地引擎时才使用本地 `whisper.cpp`
  （由 `ProjectSelectableAsrEngine` 在云端与本地两条一等公民路径之间选择）。
- **云端失败不自动回退本地**：`CloudFirstAsrEngine::transcribe` 失败即明确失败，并按可区分原因
  映射为脱敏错误；本地引擎不能作为云端失败的兜底。
- 桌面运行时在授权 token provider 存在时注入 `UpstreamFileUploadPublisher`；没有授权时才退化为
  `UnavailableTemporaryAudioPublisher`，并在运行时诊断中显式报告云端不可用。
- 临时 token 只在内存中缓存、不落盘：剩余有效期不足 30 秒即视为不可用，重新激活或清除激活状态
  会清空缓存。

## 音频直传的三步流程

16 kHz、单声道、16-bit PCM WAV 必须按实测确认的三步流程发布：

1. `POST /api/v1/files`（multipart）直传 WAV，取得 `file_id`；
2. `GET /api/v1/files/{file_id}`，取回响应中由上游签发的签名临时 URL（含 STS `security-token`、
   `Expires`、`Signature`）；
3. 以该 URL 作为 `input.file_url` 调用 `POST /api/v1/services/audio/asr/transcription`。

**实测证伪的替代路径不得使用**：

- `GET /api/v1/uploads?action=getPolicy` 加对象存储表单直传：对象被策略锁定为 `private`，转写服务
  读不到（`oss://` 引用报 `FILE_DOWNLOAD_FAILED`，HTTPS 直链报 `FILE_403_FORBIDDEN`）；
- 跳过第 2 步、直接用 `file_id` 提交：同样报 `FILE_DOWNLOAD_FAILED`。

上游文件服务没有删除接口，对象按上游保留策略回收；发布器因此只维护本地幂等清理台账——成功、失败、
取消、重试与重复清理同一租约都返回成功。

## 引用与结果地址的安全边界

- 音频引用必须由上游签发：HTTPS 始终允许；`http` 仅在上游签发且同时带签名与有效期参数时允许，
  因为上游文件服务实测签发的就是 `http` 签名地址。
- 转写结果里的 `transcription_url` 实测同为 `http` 签名地址；客户端沿用同一条校验，只接受上游签发
  形态，其他方案或缺少签名与有效期的地址一律拒绝。
- 不得把 token、Authorization Header、完整签名 URL、上游 `file_id` 或本地路径写入 SQLite、普通日志、
  诊断事件或前端状态。本地幂等台账只保存 `request_id`、任务类型、输入指纹、上游 task id 与
  状态/对账标记，用于崩溃恢复和避免重复付费提交。

## 百炼调用

- `POST /api/v1/services/audio/asr/transcription`
- 模型：`qwen3-asr-flash-filetrans`
- `X-DashScope-Async: enable`
- `Authorization: Bearer <temporary token>`
- `GET /api/v1/tasks/{task_id}` 轮询：最长等待 300 秒，轮询间隔 1500 ms

结果结构为 `transcripts[].sentences[]`，句段时间是毫秒级 `begin_time`/`end_time`（当前模型只到句级，
没有词级时间戳）。任务失败、上游 401/403/429/5xx、超时和结果结构异常都会映射为脱敏的可区分错误，
并按冻结策略如实失败，**不回退本地引擎**。

## 去重与恢复

上游不支持调用方幂等键（相同输入连续提交两次会得到两个不同的 `task_id`），因此去重完全落在本地：
客户端在调用上游之前先写入以 `request_id` + `task_type` 为唯一键的 `ai_remote_job_records` 记录，
同键同输入复用已有记录、同键不同输入拒绝。进程崩溃或断电后仍能按 `request_id` 找回同一个上游任务，
不会产生第二次付费提交。

## 配额提示

项目后端错误码的客户端提示：

- `3001`：当前账号未开通 AI 服务
- `3002`：AI 临时密钥获取失败
- `3003`：AI 转写次数已用完
- `3004`：AI 转写时长额度已用完

这些错误码属于项目后端协议，不作为百炼上游错误码解析。
