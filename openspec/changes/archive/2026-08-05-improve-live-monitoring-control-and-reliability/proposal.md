## Why

当直播中的主播数量超过录制额度时，当前实现只按并发许可的竞争顺序录制，用户无法指定有限名额应优先分配给哪些主播；同时，监听 worker 可从 Tauri 主线程进入 `tokio::spawn`，会在没有 Tokio reactor 的上下文中直接 panic 并导致桌面进程闪退。录制额度还由本地设置控制，验证码仅通过低显著性的通知提示，均不符合服务端授权控制和长期无人值守监听的可靠性要求。

## What Changes

- 为所有已开启监听的主播维护持久化录制优先级；所有主播继续检查直播状态，系统只为当前直播中优先级最高且不超过额度的主播录制。
- 用户调整优先级或服务端额度变化时立即重新调度：优雅停止被移出的录制、保留已完成分片，再重新解析新入选主播的最新地址并开始新会话；空闲名额自动由下一位直播主播补入。
- **BREAKING**：移除设置页中“最大并发录制”的可编辑控件，本地设置不再决定录制额度。
- 从 AppStart 或心跳成功响应的可选 `max_screen_limit` 整数字段接收服务端额度；最新合法值立即生效，从未取得该字段时使用默认值 4，单次响应缺失字段或网络失败时保持当前额度。
- 将 Supervisor 的任务启动统一交给可从 Tauri 主线程、同步 command、唤醒事件和异步任务安全调用的运行时调度边界，消除“there is no reactor running”进程级 panic。
- 公开页进入 `verification_required` 时显示一次去重的系统级操作弹窗；用户点击“立即验证”后直接显示并聚焦受限抖音验证窗口，选择稍后处理时继续保留现有横幅。
- 验证强提醒不受普通录制通知开关影响，但在同一轮验证阻塞期间不得重复弹出；访问恢复后才允许下一轮提醒。
- 不把登录自动化、验证码识别或访问控制绕过纳入本次变更。
- `make: No rule to make target 'DEV_REQUIRE_ACTIVATION'` 属于缺少 `=1` 的命令调用错误，不纳入客户端产品行为变更。

## Capabilities

### New Capabilities

无。

### Modified Capabilities

- `streamer-monitoring`：增加持久化录制优先级、按额度确定目标集合、即时抢占与补位，并保证所有 worker 启动入口具备运行时安全性。
- `client-activation-service`：AppStart 和心跳响应可下发 `max_screen_limit`，并由最新合法响应动态控制录制额度。
- `desktop-client-shell`：移除本地并发编辑入口，展示服务端额度与录制优先级控制，并将访问验证提示升级为可直接开始验证的去重系统弹窗。

## Impact

- Tauri 后端：`api.rs`、`activation.rs`、`app.rs`、`supervisor.rs`、`database.rs`、领域 DTO、系统弹窗和验证窗口桥接。
- React 前端：监控列表的优先级操作、设置页只读额度展示、API 类型、浏览器演示行为与状态提示。
- 持久化：为主播增加稳定排序字段并迁移旧数据；旧 `max_concurrent_recordings` 设置保留兼容但不再作为生产调度输入。
- 服务端契约：AppStart 和心跳响应新增可选 snake_case 字段 `max_screen_limit`。
- 测试：API 兼容、数据库迁移、动态调度和抢占、无 Tokio reactor 调用、系统弹窗去重/操作、前端交互与现有监听回归。
