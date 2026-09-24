# cloud-asr-provider Specification

## Purpose
为现有本地语音转写增加受控的阿里云百炼云端识别能力，在授权、配额和网络条件满足时提高处理效率，同时确保本地模型仍可离线运行并作为可靠回退。

## Requirements

### Requirement: 按授权优先使用阿里云 ASR
系统 SHALL 在开始一次 ASR 输入处理前，通过已鉴权的 `/api/client/ai/llm-ak` 获取临时 AI 密钥；仅当响应成功、`provider` 为 `ALI`、密钥非空且 `expireAt` 未过期时，才允许提交阿里云百炼 ASR，否则 SHALL 进入本地 ASR 回退流程。

#### Scenario: 临时密钥有效
- **WHEN** 服务端返回有效临时密钥且 provider 为 `ALI`
- **THEN** 系统使用该密钥以 Bearer 方式提交百炼文件转写任务，并不启动本地识别作为首选路径

#### Scenario: 未授权或额度耗尽
- **WHEN** `/api/client/ai/llm-ak` 返回 `3001`、`3003` 或 `3004`
- **THEN** 系统显示对应的中文授权/次数/时长提示，并使用本地 ASR 完成本次处理（若本地环境可用）

### Requirement: 提供百炼异步文件转写
系统 SHALL 使用百炼文件转写 API 提交 `qwen3-asr-flash-filetrans` 任务，获取 `task_id` 后以有界间隔轮询任务状态；成功时 SHALL 将结果中的文本、句段时间范围和可用词级时间信息转换为统一 ASR 结果。

#### Scenario: 云端任务成功
- **WHEN** 百炼任务状态变为 `SUCCEEDED` 且结果可下载或直接解析
- **THEN** 系统发布统一转写产物，保留源视频时间轴语义，并标记 provider 为 `aliyun`

#### Scenario: 云端任务失败或超时
- **WHEN** 百炼返回失败状态、不可解析结果或达到轮询/请求超时上限
- **THEN** 系统保存脱敏的失败原因并转入本地 ASR 回退，不得无限轮询

### Requirement: 为云端识别提供短期可访问音频
系统 SHALL 仅向百炼提交受控的短期 HTTPS `file_url` 或等价的受信文件引用；系统 MUST NOT 将本地文件路径、`file://` URI 或永久公开 URL 直接提交给百炼。临时音频 SHALL 在任务结束、失败或取消后按生命周期清理。

#### Scenario: 临时音频 URL 可用
- **WHEN** 标准音频已上传到受控后端或对象存储并获得未超过任务所需时长的 HTTPS URL
- **THEN** 系统使用该 URL 提交百炼任务，并在云端任务完成或放弃后撤销或清理该对象

#### Scenario: 无法生成受控 URL
- **WHEN** 部署未提供临时上传/对象存储能力或生成 URL 失败
- **THEN** 系统不发起云端请求，记录脱敏原因并直接使用本地 ASR

### Requirement: 保护临时密钥和上游错误
系统 MUST 只在内存中保存临时密钥，不得把密钥、Authorization Header、完整音频 URL 或本地路径写入数据库、普通日志、诊断事件或前端状态；上游错误 SHALL 按 HTTP 状态、API code/message 和 request_id 脱敏记录。

#### Scenario: 任务成功或终止
- **WHEN** 云端 ASR 任务成功、失败、取消或回退完成
- **THEN** 系统释放临时密钥引用并确保持久化数据中不存在密钥或认证头

#### Scenario: 百炼返回限流或鉴权错误
- **WHEN** 上游返回 401、403、429 或 5xx
- **THEN** 系统执行有界重试策略，向用户显示可解释提示，并在无法恢复时回退本地而不重复消耗授权

### Requirement: 对云端和本地结果保持统一产物契约
系统 SHALL 将云端与本地识别结果写入相同的 ASR artifact/transcript 结构，并 SHALL 记录非敏感的 provider、模型版本和回退原因，以保证既有缓存、时间轴、导出和复用行为可用。

#### Scenario: 云端结果发布
- **WHEN** 云端转写结果通过统一校验
- **THEN** 系统按现有稳定句段 ID、源内时间戳和规范化规则发布结果

#### Scenario: 云端回退本地
- **WHEN** 云端路径不可用且本地资源门禁通过
- **THEN** 系统使用本地 whisper.cpp 发布同一结构的结果，并向前端提供回退原因
