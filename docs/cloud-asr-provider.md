# 云端 ASR 发布器接入说明

当前客户端已实现阿里云百炼文件转写协议、临时 AI 密钥获取、云端优先路由和本地 `whisper.cpp` 回退，但采用方案 C：本版本只提供 `TemporaryAudioPublisher` 抽象，不内置后端上传接口或对象存储 SDK。

## 当前行为

- ASR 任务保留现有 FFmpeg、VAD、本地模型、缓存、单并发和恢复流程。
- 客户端从 `/api/client/ai/llm-ak` 获取 `provider=ALI` 的短期 token，并只在内存中缓存。
- token 剩余有效期不足 30 秒时视为不可用；重新激活或清除激活状态会清除缓存。
- 只有临时 token provider 和临时音频发布器同时可用时，才会调用百炼文件转写。
- 当前桌面运行时注入的是 `UnavailableTemporaryAudioPublisher`，因此不会上传本地文件，也不会把本地路径或 `file://` URI交给百炼；任务会自动使用本地 ASR。

## 后续发布器契约

未来后端临时上传或对象存储实现必须实现 `TemporaryAudioPublisher`：

1. 输入只能是任务生命周期内生成的 16 kHz、单声道、16-bit PCM WAV。
2. 输出必须是短期 HTTPS URL 和不包含凭据的不透明 `lease_id`。
3. URL 必须允许百炼在任务执行期间访问，但不得永久公开。
4. 成功、失败、取消和回退后都必须允许幂等清理。
5. 不得把 token、Authorization Header、完整 URL、本地路径或百炼 task id 写入 SQLite、普通日志、诊断事件或前端状态。

接入具体发布器后，运行时诊断的 `cloudAsrAvailable` 才可返回 `true`。

## 百炼调用

客户端适配器使用：

- `POST /api/v1/services/audio/asr/transcription`
- 模型：`qwen3-asr-flash-filetrans`
- `X-DashScope-Async: enable`
- `Authorization: Bearer <temporary token>`
- `GET /api/v1/tasks/{task_id}` 轮询任务

任务状态、上游 401/403/429/5xx、超时和结果结构异常都会映射为脱敏错误，并在本地资源可用时回退 `whisper.cpp`。

## 配额提示

项目后端错误码的客户端提示：

- `3001`：当前账号未开通 AI 服务
- `3002`：AI 临时密钥获取失败
- `3003`：AI 转写次数已用完
- `3004`：AI 转写时长额度已用完

这些错误码属于项目后端协议，不作为百炼上游错误码解析。
