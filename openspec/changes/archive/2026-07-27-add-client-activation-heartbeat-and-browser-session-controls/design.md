## Context

客户端当前在 Tauri 初始化后直接创建并恢复 `Supervisor`，浏览器会话也由 `RoomResolutionService` 独立管理。服务端已经提供 `/api/client/activate`、`/heartbeat`、`/telemetry` 和 `/app-start`，但仓库没有客户端封装、授权本地状态或生命周期协调。实现必须保持现有公开页解析边界，不把卡密、令牌、页面正文或媒体数据写入日志。

## Goals / Non-Goals

**Goals:**

- 用一个 Rust `ApiClient` 统一封装服务端 URL、请求头、HTTP/业务码解析、脱敏错误以及未来的加解密钩子。
- 用 SQLite 保存设备 ID、激活码、令牌和心跳状态；激活前不恢复 `Supervisor`，激活成功后恢复。
- 启动调用 AppStart 和 `app_open` 埋点；后台心跳周期续约，失效时暂停监听并向前端发布授权状态。
- 验证 WebView 成功探测后自动发出一次访问状态检查通知；窗口隐藏时通过受限脚本将页面媒体静音。

**Non-Goals:**

- 本期不实现 API 信封加密、签名、密钥轮换或第三方验证码自动处理。
- 不绕过抖音访问限制，不读取完整 DOM、Cookie 或直播正文。
- 不让服务端参与视频、音频、ASR 或 LLM 内容处理。

## Decisions

### 统一 API 客户端

在 `src-tauri/src/api.rs` 中定义 `ApiClient`、`ApiConfig`、统一 `ClientEnvelope<T>` 和请求方法。URL 按运行时 `DY_SCREEN_API_BASE_URL`、编译时 `option_env!`、开发默认值的顺序解析，并在构造时规范化为 `/api/` 结尾。请求/响应通过 `RequestCodec` trait 预留 `encode_request`/`decode_response`，当前实现原样传递 JSON。所有 endpoint 只在此文件中定义，业务模块只调用类型化方法。

选择单文件封装而不是在各 command 中直接使用 `reqwest`，是为了统一 HTTP 恒 200、业务 `code` 判断、超时和中文错误脱敏；不采用前端 `fetch`，因为授权门禁和心跳必须覆盖后台 Rust worker。

### 本地授权与生命周期

在 SQLite 新增 `client_activation` 单行表（设备 ID唯一），迁移版本单调递增。`ActivationService` 负责读取/写入该表、调用激活、维护授权状态和发布事件；它不把激活码返回给前端，只返回 `configured/active/status/message/nextHeartbeatAt` 等摘要。`activate_client` 成功后由 command 显式调用 `Supervisor::restore`，启动时只有 `ACTIVE` 状态才恢复任务。

心跳在独立 Tokio 任务中按 90 秒周期执行，网络失败采用有界退避但不立即清除本地激活；服务端返回 `revoked=true`、`disabled`、`expired` 或 `unbound` 时清理令牌、暂停 Supervisor 并发布不可忽略的前端事件。应用关闭时取消心跳任务。

### 启动和埋点

AppStart 是免鉴权请求，启动时 best-effort 执行并只记录版本决策摘要；`app_open` 与后续 feature/error 事件进入内存批队列，按 20 条或 10 秒刷新发送，失败丢弃并写脱敏诊断。埋点请求不影响激活弹窗、录制或应用退出。

### 浏览器验证回查与静音

`RoomResolutionService::complete_verification` 在发布 `session_ready` 后通知一次新的访问检查触发器；桌面 publisher 监听该事件并调用现有 `Supervisor::check_all_now`，因此用户完成验证后不需要再次点击“检查访问状态”。为避免重复导航，触发器只合并同一代次的通知。

验证窗口创建后和每次导航/快照回调后执行受限 `document.querySelectorAll('video,audio')` 静音脚本，并设置 `muted=true`、`volume=0`；脚本失败只记录内部分类，不阻断页面解析。窗口仍保持隐藏、非聚焦，用户主动点击验证时才显示。

## Risks / Trade-offs

- [服务端不可用] → AppStart/埋点失败不阻塞启动；无本地授权时仍必须等待激活，已有有效授权可按本地状态和心跳重试策略继续短期运行。
- [本地激活码属于敏感配置] → 前端只显示是否已配置和脱敏状态，日志不输出激活码、令牌或完整 API URL 查询串；SQLite 文件权限沿用应用数据目录。
- [心跳和 Supervisor 生命周期竞态] → 所有暂停/恢复操作经过现有 Supervisor API，授权任务使用 `CancellationToken`，停止事件幂等化。
- [WebView 平台不支持脚本注入静音] → 继续尝试受限脚本但把失败视为非致命，解析流程和用户验证不受影响。
- [默认 API 地址误用于生产] → 启动日志和设置诊断显示当前来源（不显示敏感参数）；README 与 `.env.example` 要求发布环境显式配置。

## Migration Plan

1. 启动时执行 SQLite 新迁移，旧数据库自动创建空授权记录；未激活实例展示激活弹窗且不恢复监听。
2. 配置 `DY_SCREEN_API_BASE_URL` 后构建客户端；未配置时仅允许开发默认 `http://localhost/api/`。
3. 回滚时保留迁移表和授权表，不删除用户数据；禁用授权服务后仍可读取旧录像，但需要显式恢复旧版本客户端。

## Open Questions

- 服务端正式部署域名和是否要求 HTTPS 由发布环境通过 `DY_SCREEN_API_BASE_URL` 决定，本期不再引入第二套配置来源。
