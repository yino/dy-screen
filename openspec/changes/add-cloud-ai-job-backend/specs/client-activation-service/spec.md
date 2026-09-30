> 状态：**已搁置（本轮不实施）**。当前口径下客户端只保留既有三个接口（`activate`、`heartbeat`、`ai/llm-ak`），不新增 9 个 AI 任务接口。本 delta 仅作为历史设计保留；其中“单一 API 客户端封装、业务模块不得绕过”的约束仍适用于既有接口。

## MODIFIED Requirements

### Requirement: 统一封装客户端授权 API
系统 SHALL 在 Rust 中提供单一 API 客户端封装，集中读取 `DY_SCREEN_API_BASE_URL`、规范化 `/api/` 前缀、处理 HTTP 恒 200 的业务 `code`、超时、请求关联头和脱敏错误；激活、心跳、AppStart、埋点及其他需授权 API 的业务代码 MUST NOT 直接创建 HTTP 客户端。客户端 SHALL 在所有需授权请求中同时发送权威 `device_id`、`activate_code` 请求头和兼容 `device-id`、`activate-code` 别名。客户端 SHALL 通过同一封装访问 `POST /api/client/ai/runs`、`POST /api/client/ai/uploads`、`POST /api/client/ai/asr-jobs`、`POST /api/client/ai/llm/v1/chat/completions`、`POST /api/client/ai/image-jobs`、`POST /api/client/ai/video-jobs`、`GET /api/client/ai/jobs/{jobId}`、`GET /api/client/ai/jobs/by-request/{requestId}` 和 `POST /api/client/ai/jobs/{jobId}/cancel`，并 MUST NOT 为这些接口在业务模块中另建 HTTP 客户端。本期 SHALL 保持明文 JSON 编解码，不实现信封加密、请求签名或 `encKey` 轮换。

#### Scenario: 配置服务端地址
- **WHEN** 启动时设置 `DY_SCREEN_API_BASE_URL=http://localhost/api/`
- **THEN** 所有客户端请求都发送到该地址下的 `/client/...` 路径，并且不在日志中输出完整查询参数

#### Scenario: 发送需授权请求头
- **WHEN** 心跳或其他需授权接口发出请求
- **THEN** 统一客户端同时发送 `device_id`、`activate_code`、`device-id` 和 `activate-code`，且任何业务调用方都不能自行复制或改写凭据

#### Scenario: 代理丢失权威请求头
- **WHEN** 客户端确认已构造授权请求头但服务端返回 `1001` 或 `2001`
- **THEN** API 客户端返回稳定的请求头契约错误分类和关联 ID，提示检查代理下划线请求头配置，不把该结果伪装成普通网络错误

#### Scenario: 服务端返回业务错误
- **WHEN** HTTP 状态为 200 但响应 `code` 不是成功码
- **THEN** API 客户端返回包含稳定错误分类和中文消息的错误，不把原始响应正文传播到前端

#### Scenario: API 请求编解码
- **WHEN** 激活、心跳或其他客户端接口编码请求并解析响应
- **THEN** 系统使用现有明文 JSON codec，且不得发送未完整实现的信封头、设备签名或伪造的加密正文

#### Scenario: AI 任务接口复用统一客户端
- **WHEN** 业务模块调用任一 AI 任务、上传凭证、模型网关或任务查询接口
- **THEN** 请求经由统一客户端发出并携带权威 `device_id` 与 `activate_code`，业务模块不自行构造请求头

#### Scenario: AI 任务请求出现代理契约错误
- **WHEN** 任一 AI 任务接口返回 `1001` 或 `2001`
- **THEN** API 客户端返回与其他授权接口一致的请求头契约错误分类，业务模块不把它归类为 AI 预算或上游故障

## ADDED Requirements

### Requirement: 从服务端授权生命周期接收 AI 预算额度
系统 SHALL 在成功心跳响应中兼容可选的 AI 分项预算信号，覆盖 ASR 时长、模型 token、图片次数与视频秒数。授权生命周期 MUST 只发布合法分项，并 MUST NOT 使用本地设置替代服务端额度；分项缺失或非法 MUST NOT 改变心跳成功结论、令牌续约或激活状态。

#### Scenario: 心跳下发 AI 预算
- **WHEN** 成功心跳携带合法的 AI 分项预算
- **THEN** 客户端在保持既有续约行为的同时发布最新只读预算状态，供界面显示各分项剩余额度

#### Scenario: 心跳没有 AI 预算字段
- **WHEN** 成功心跳未包含 AI 预算信号
- **THEN** 客户端保留最近一次合法预算状态，授权续约、监听、录制与本地 ASR 继续运行

#### Scenario: AI 预算字段非法
- **WHEN** 任一分项不是可表示的整数，或其单位与声明不符
- **THEN** 客户端忽略该分项、记录不含响应正文的脱敏诊断，并继续按成功心跳处理授权

#### Scenario: 本地设置与服务端额度冲突
- **WHEN** 本地配置与心跳下发的分项额度不一致
- **THEN** 客户端以服务端额度为准，不因本地设置放宽已在服务端耗尽的额度
