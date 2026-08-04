## Context

当前 Supervisor 为每个启用监听的主播创建长期 worker，worker 确认开播后直接竞争 `RecordingLimiter`。这种先到先得模型只能限制数量，无法表达用户希望录制哪些主播；等待许可的 worker 也缺少统一的目标集合重算入口。并发值来自本地 `AppSettings.max_concurrent_recordings`，设置页允许用户在 1 至 16 之间修改，与新增的服务端授权额度要求冲突。

`Supervisor::start` 是同步方法，内部直接调用 `tokio::spawn`。它既会从 Tokio 任务调用，也会从同步 Tauri command、`RunEvent::Resumed` 等主线程回调调用；后者没有当前 Tokio reactor 时会在创建 worker 前 panic，已提供的日志正好停在直播结果后的 `src/supervisor.rs:580`。

公开页访问进入 `verification_required` 时，当前 publisher 只在普通通知开关开启时发送一次系统通知。主界面已有持续横幅和验证窗口，但应用隐藏、系统通知权限不足或通知被关闭时提示不明显。本变更跨越服务端 API、授权生命周期、Supervisor 调度、SQLite、Tauri 系统交互和 React 展示，需要统一设计这些状态的所有权和并发边界。

## Goals / Non-Goals

**Goals:**

- 让所有启用主播继续监听，同时按持久化优先级和服务端额度选出确定的录制目标。
- 调整优先级或额度后立即、安全地抢占和补位，任何时刻不超过当前额度。
- 让 AppStart 与心跳共同支持可选 `max_screen_limit`，且本地设置无法覆盖服务端值。
- 从任意 Tauri/Tokio 调用上下文安全、幂等地启动 worker，不再因缺少当前 reactor 使进程退出。
- 在确实需要用户处理验证码时显示去重的系统级操作弹窗，并让主操作直接打开验证窗口。
- 保留原始录像、完成分片、监听身份、访问会话和退出清理的既有安全约束。

**Non-Goals:**

- 不增加登录自动化、验证码识别、Cookie 导入、访问控制绕过或私有直播支持。
- 不允许用户从本地设置覆盖、扩大或缩小服务端录制额度。
- 不修改 FFmpeg 编码、分片格式、清晰度选择或现有公共页解析算法。
- 不为错误的 `make app-dev DEV_REQUIRE_ACTIVATION` 调用增加同名 Make target；启用门禁仍使用 `DEV_REQUIRE_ACTIVATION=1`。

## Decisions

### 使用进程内服务端额度状态而非本地设置

新增共享的运行时额度状态，初始值固定为 4。`AppStartResponse` 和 `HeartbeatResponse` 使用显式 `#[serde(rename = "max_screen_limit", default)]` 接收 `Option<i64>`，避免既有 `rename_all = "camelCase"` 把服务端指定的 snake_case 字段误解析。只有可转换为正 `usize` 的值才会发布给 Supervisor；任一接口的最新合法值覆盖当前值，响应缺失字段或请求失败不覆盖已有值，因此只有从未收到合法字段的进程使用默认 4。

额度不写入用户设置，也不从旧 `max_concurrent_recordings` 恢复。兼容期可保留旧 SQLite key 和 DTO 字段以读取旧数据库，但保存设置的业务路径不再调用 `set_max_concurrent`，监控快照另行暴露当前只读 `maxScreenLimit`。这样旧数据无需破坏性迁移，同时服务端始终是唯一权威来源。

替代方案是只支持 AppStart 或只支持心跳。前者不能运行时调额，后者在未激活或首个心跳前缺少启动值；同时支持两者并采用“最新合法值”可以兼容服务端逐步上线字段。

### 使用持久化全序优先级而非固定白名单

为 `streamers` 增加 `recording_priority`。migration 按现有主播 ID 升序生成确定性序号，新主播追加到末尾；归档不需要压缩序号，恢复时保留原值，身份合并时保留两条记录中更高的优先级并在事务内消除冲突。repository 提供原子移动操作，按相邻位置或目标位置重排受影响区间，并以 `id` 作为异常重复序号的稳定兜底。

固定白名单会在入选主播离线时闲置服务端额度；全序优先级则从“已启用监听且当前在线”的候选中取前 N 位，既支持把 `a,b,c,d` 改为 `a,b,c,e`，也能在其中一位下播后自动补入下一位。优先级独立于监控表按状态和名称的视觉排序，界面显示序号并提供提高/降低操作，避免拖拽在长表格和键盘操作中的可访问性问题。

### 由 Supervisor 集中重算目标集合

把“在线候选”“活动录制”“等待录制”和“当前额度”集中在一个录制调度状态中。worker 确认直播后登记候选并等待调度分配，同时以既有检查周期继续确认在线状态；获得分配后仍必须重新解析最新地址，避免使用等待期间过期的签名 URL。

优先级、在线状态、活动录制结束或服务端额度变化都会触发一次串行化 reconcile：

1. 从最新数据库优先级和在线候选计算期望集合。
2. 为不再入选的活动录制发送带 `priority_changed` 或 `server_limit_reduced` 原因的协同取消。
3. 等待对应 `RecordingPermit` 实际释放，再按优先级向新入选候选授予名额。
4. 使用 generation 校验候选和录制 token，防止旧任务退出后删除或覆盖新状态。

用户优先级切换关闭的会话沿用现有 `cancelled` 状态，并在会话错误/原因字段及脱敏监听日志中保存稳定原因；已完成分片照常对账。调度状态不在持有同步 `Mutex` 时执行 `.await`，数据库写入失败时不发布新顺序或启动抢占。

替代方案是给现有信号量增加优先等待队列。该方案不能可靠抢占已占用许可，也难以原子处理额度降低和优先级更新，因此采用显式目标集合协调器。

### 通过应用运行时调度器创建 worker

Supervisor 不再直接依赖“当前线程存在 Tokio reactor”的 `tokio::spawn`。生产构造函数注入或保存应用级任务调度器，使用 Tauri 全局异步运行时的 `spawn`；测试构造函数使用 Tokio handle 或可控 fake。`start` 在 worker 表锁内完成去重和 generation 分配，但只有调度成功后才登记可用 worker；调度失败返回中文脱敏错误并撤销占位。

`check_now`、`check_all_now`、`resume`、启动恢复、访问恢复和 `RunEvent::Resumed` 全部调用同一入口。worker 内部创建磁盘监控等子任务时已经运行在应用 runtime 上，可继续使用 Tokio 子任务，但需要增加覆盖无当前 runtime 的普通线程回归测试。

替代方案是只把 `RunEvent::Resumed` 包进 `tauri::async_runtime::spawn`。这无法保护同步 command 和未来新增回调，仍会让 `Supervisor::start` 的正确性依赖每个调用者，因此不采用。

### 使用非阻塞系统对话框升级验证提醒

在 `DesktopRoomResolutionPublisher` 的状态发布路径增加验证提示协调器。状态首次进入 `verification_required` 时，协调器通过现有 `tauri-plugin-dialog` 显示 warning 类型、带“立即验证”和“稍后处理”自定义按钮的非阻塞系统对话框；该对话框是必要操作提示，不受普通录制通知设置影响。普通系统通知仍遵循用户通知开关。

用户选择“立即验证”后，回调通过 `AppHandle` 获取当前 `RoomResolutionService`，在 Tauri 全局异步运行时调用 `show_verification`，直接显示并聚焦持久 WebView；选择稍后或关闭只结束对话框，主界面横幅继续保留。一个原子去重门覆盖从首次 `verification_required` 到状态恢复的整轮阻塞，多个主播重复上报不会堆叠弹窗，状态离开后再重置。

系统对话框只提示并承接用户选择，不自动操作抖音页面；在 `browser_resolving` 阶段仍保持低干扰，也不改变验证窗口现有的导航白名单和会话隔离。

## Risks / Trade-offs

- [优先级变更与录制退出存在短暂过渡] → 先取消旧录制并等待 permit drop，再启动新录制；界面在过渡期显示等待资源，不允许瞬时超额。
- [等待候选的在线状态可能过期] → 等待期间按现有有界周期复查，真正分配后再次解析并确认直播。
- [服务端返回异常巨大或负数] → 只接受可表示的正整数，异常值保持最近合法额度并写入无响应正文的诊断；实现阶段增加资源上限评审但不得由用户设置覆盖。
- [AppStart 与心跳响应先后到达] → 所有应用动作通过单一额度状态串行发布，按实际接收顺序采用最新合法值。
- [额度降低导致用户关心的录制被停止] → 使用持久化优先级确定保留集合，优雅停止最低优先级录制并保存稳定原因和完成分片。
- [系统对话框在多个状态事件下重复出现] → 用跨 publisher clone 共享的原子门去重，只在离开验证状态后重置。
- [系统对话框回调发生在退出期间] → 回调先检查 AppState 和关闭门；已退出或正在关闭时不创建验证任务。
- [数据库 migration 中旧优先级初始化不符合用户期待] → 使用可预测的 ID 顺序且上线后允许立即调整，不删除或重建任何主播、会话和视频。

## Migration Plan

1. 新增幂等 SQLite migration，为现有主播回填 `recording_priority`，验证记录数、外键和历史数据不变。
2. 扩展 API response、授权生命周期和只读额度状态，先以默认 4 兼容尚未下发字段的服务端。
3. 引入应用级 worker 调度器并覆盖全部启动入口，先用回归测试复现普通线程调用不再 panic。
4. 引入录制目标协调器、优先级 repository/command 和抢占原因，替换先到先得的许可竞争。
5. 更新监控列表、设置页和浏览器演示类型；旧 `max_concurrent_recordings` 数据保留但停止影响生产调度。
6. 增加系统验证弹窗协调器，保留现有横幅、普通通知和受限 WebView。
7. 运行数据库、API、Supervisor、生命周期、访问会话、前端和严格 OpenSpec 回归。

回滚应用版本时，旧版本会忽略新增优先级列并继续读取既有本地并发设置；migration 不删除旧 key。服务端字段为可选，尚未升级的客户端会忽略它。由于旧版本不理解新的服务端权威语义，发布时不得把回滚版本用于验证服务端额度强制执行。

## Open Questions

无。需求决策已按确认的推荐方案固化。
