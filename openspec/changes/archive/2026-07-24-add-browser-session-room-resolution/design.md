## Context

当前 `StreamResolver` 使用固定 User-Agent 的无状态 `reqwest` 客户端请求公开直播页，只解析 `self.__pace_f.push` 中经过测试的 React Flight 房间对象。2026-07-24 的真实诊断表明，同一台机器上的系统浏览器能够观看公开直播，而原生 HTTP 请求得到 HTTP 200、约 6 KB 的访问验证中间页；多个主播在固定轮询后同时进入 `access_restricted`。现有 supervisor 虽会退避，但录制流结束时仍依赖同一原生 HTTP 通道刷新签名地址，因此会漏检开播或终止续录。

项目必须继续保持 Rust 录制核心、FFmpeg 分片和 SQLite 业务数据独立于 UI；浏览器能力只作为公开页面解析通道。不得自动识别或模拟访问验证，不得访问私有、付费或受权限保护的直播，不得在日志和业务数据库保存 Cookie、页面正文、原始 React Flight 载荷或签名流 URL。

## Goals / Non-Goals

**Goals:**

- 在原生 HTTP 被公开页风控限制时，使用系统 WebView 的真实 JavaScript 和持久会话环境恢复房间状态与签名流发现。
- 让直播间直连、个人主页接管和录制中流地址刷新共享同一双通道 resolver。
- 多个主播共享一个串行浏览器会话，避免并发导航和密集公开页面请求。
- 在需要用户处理标准访问验证时提供明确、非抢焦点、可恢复的客户端流程。
- 为每一次原生 HTTP 与浏览器访问输出足够排障但不泄密的控制台和 JSONL 记录。
- 用 fixture、fake WebView、状态机测试和 macOS 真实公开直播间验收证明回退与续录行为。

**Non-Goals:**

- 不实现自动验证码识别、交互行为模拟、第三方打码、代理池、IP 切换或账号自动登录。
- 不复制 WebView Cookie 到 SQLite、日志、前端或原生 HTTP 客户端。
- 不支持私有、付费、DRM 或其他需要额外内容权限的直播。
- MVP 不打包 Playwright、Chromium 或常驻浏览器 sidecar。
- MVP 不承诺所有平台真实风控行为一致；先在 macOS WKWebView 验收，保留跨平台接口。

## Decisions

### 1. 使用双通道 resolver 和粘性浏览器模式

新增 `RoomResolutionService` 作为 supervisor 与主播校验的唯一房间解析入口。服务先尝试现有 `StreamResolver`；遇到 `access_restricted` 时立即切换为浏览器通道，并在至少 30 分钟冷却期内保持粘性浏览器模式。冷却期后只允许一次原生 HTTP 试探，失败则继续浏览器模式，防止每个 worker 重复触发验证页。

启动时多个 worker 可能已经并发取得普通原生尝试许可；其中任一请求触发浏览器粘性模式后，其他较晚返回的普通原生成功不得清除该模式。只有冷却期结束后由状态机授予的唯一恢复探测成功，才能回到原生优先。

共享 WebView 一旦进入 `verification_required`，后续房间不得继续浏览器导航或覆盖当前验证页，但不同房间仍允许执行受全局节流保护的原生 HTTP 检查。原生成功的房间直接返回自身结果；只有同样需要浏览器回退的房间才登记为等待。已运行的 FFmpeg 不受影响；验证恢复或用户清除会话后统一唤醒等待 worker。

替代方案是永久改用 WebView，但这会让所有轮询依赖 UI 线程并增加资源占用；只复制 Cookie 给 `reqwest` 也不能复现浏览器 TLS、HTTP/2、Header 和 JavaScript 环境，因此均不采用。

### 2. 使用单个持久化系统 WebView 串行解析

Tauri 后端按需创建标签固定的 `douyin-access` WebviewWindow，使用平台持久数据存储且关闭 incognito。所有主播通过异步队列串行导航；每次导航、页面完成和脚本探测都带内部请求代次，过期回调不得完成新请求。

窗口默认隐藏。浏览器自动完成标准页面脚本时不打扰用户；确认页面要求交互后，服务发布 `verification_required`，前端显示横幅和通知，只有用户点击“立即验证”才显示窗口。

### 3. 在 WebView 内读取受支持脚本，不向远程页面开放 IPC

WebView 只允许 `https` 且 host 为 `douyin.com` 或其子域的顶层导航，拒绝其他 scheme、目录下载和任意新窗口。后端在页面加载完成后通过 `eval_with_callback` 同步读取以下最小快照：规范化 URL、标题、readyState、访问限制 marker 布尔值，以及包含 `self.__pace_f.push` 的有限脚本文本集合。

导航返回不代表目标文档已经替换完成。每次解析必须同时满足：请求代次仍为当前代次、快照 URL 与目标规范化直播间完全一致、`readyState` 为 `complete`，并且 Flight 房间对象自身或其受支持父级身份的 `web_rid` 与目标一致。旧页面、加载中页面和其他房间的推荐对象只记为 `pending`，不得返回直播流或启动 FFmpeg。

脚本文本仅在进程内交给现有严格 parser，生命周期不超过本次解析；不写磁盘、不进入事件或错误。远程页面不获得主窗口 capability，也不暴露通用 Tauri invoke。相比把完整 HTML 或 Cookie 传给 backend，该方案缩小了数据面和远程页面权限。

### 4. 浏览器解析采用有界探测和显式状态

页面完成后最多探测 15 秒。探测到受支持房间对象即返回 `live` 或 `offline`；探测期间存在访问限制 marker 时允许页面自身继续标准流程；期限结束仍受限则返回 `verification_required`，无 marker 且无受支持结构则返回 `layout_changed`。用户主动打开验证窗口后，后台 watcher 只轮询当前验证页的最小快照，不重新导航；检测到目标房间受支持结构后自动隐藏窗口、切换为 `session_ready` 并唤醒全部等待 worker。“检查访问状态”作为显式恢复操作，先重试当前目标的原生通道；原生成功时直接清除旧验证目标，仍受限时才为浏览器目标分配新请求代次并受控重新导航一次，避免中间页永久卡住时掩盖已经恢复的原生结果。

全局访问状态为 `native`、`browser_resolving`、`verification_required`、`session_ready` 或 `session_expired`；主播监听状态保持独立。用户关闭窗口不会删除会话，只让当前请求进入等待验证；“清除浏览器会话”调用 Tauri browsing-data 清理并取消未完成请求。

### 5. 调整多房访问节奏并保留录制连续性

公开页面访问门最小间隔从 2 秒提高到 5 秒。已知离线直播间基础轮询从 30 秒提高到 60 秒并加入 0 到 15 秒抖动；失败继续使用 30、60、120、300 秒有界退避。浏览器导航也通过同一全局访问门和单任务锁。

FFmpeg 结束或流地址失效后，现有同一逻辑会话内三次续录规则不变，但每次续录必须通过双通道 resolver 获取最新签名地址。浏览器等待验证期间不创建错误的新直播周期，已经关闭的 MKV 分片继续登记。

### 6. 每次访问输出统一的安全诊断

定义 `AccessDiagnosticEntry`，每次真实访问在全局访问门放行并即将发出时先写入 `request/pending`，完成后再写入原生 `response` 或浏览器 `navigation`，页面探测、回退和最终动作继续分别记录。相同请求代次下分类、marker 和响应摘要未变化的 probe 只记录首次状态和最长 30 秒一次的心跳，状态变化或手动重载的新代次立即记录，避免验证 watcher 按秒刷屏。每条记录都同时写入 stderr 和当日 JSONL，字段包括时间、streamer ID、web_rid、请求 ID、通道、阶段、分类、可选 HTTP 状态、Content-Type、响应字节数、安全 marker、耗时、失败次数、后续动作和下次重试时间。只有请求记录而没有完成记录时，表示访问仍在进行、已被取消或进程在返回前中断，便于定位无响应问题。`native` 与 `browser` 只表示真实访问通道，Supervisor 追加的派生状态汇总明确使用 `monitor`，不得把浏览器结果误标为原生响应。成功、离线、浏览器回退和用户验证都必须记录；生产 GUI 没有控制台时 JSONL 仍是权威记录。

日志序列化前采用白名单字段和枚举，URL 只保留规范化 `web_rid`，错误只用脱敏文本。日志写入失败不得停止监听或录制。

### 7. 通过服务接口隔离 Tauri 和可测试核心

核心新增浏览器快照解析函数和通道无关的诊断 DTO。Tauri 新增 `BrowserPageDriver` trait、真实 `TauriBrowserPageDriver` 与 fake；`RoomResolutionService` 持有原生 resolver、driver、模式状态和串行锁。Supervisor、StreamerService 和命令只依赖高层 inspector trait，不直接操作窗口。

前端通过受控 Tauri commands 查询状态、打开验证窗口、重新检查和清除会话；浏览器演示 API 固定返回“不支持真实访问验证”，不伪造成功会话。

### 8. 在数据库启动恢复前获取进程级独占锁

桌面进程在创建应用数据目录后立即通过现有 `fs2` 依赖获取锁文件的非阻塞独占锁，随后才允许打开 SQLite、执行 migration、调用 `reconcile_startup()` 或创建任何后台服务。锁句柄由 `AppState` 持有到应用状态销毁；锁文件可以保留在磁盘，进程退出后由操作系统释放文件锁。

第二实例无法获取锁时必须在 setup 阶段返回稳定的“应用已在运行”错误并立即结束，不得打开业务数据库、修改会话状态、启动 worker 或创建 FFmpeg。相比仅依赖 SQLite 写锁，该方案把“是否拥有恢复权”放在任何业务写入之前，也覆盖数据库当前空闲但第一实例仍在录制的情况。

### 9. 统一系统信号与应用退出的有界关闭流程

macOS/Linux 进程安装 `SIGTERM` 和 `SIGINT` 监听器，并把信号、托盘退出、前端确认退出和 Tauri `ExitRequested` 汇入同一个一次性关闭领取器。首个来源领取成功后并发关闭预览、缩略图、Supervisor、浏览器解析和 AI runtime；其他来源只记录去重结果，不再次执行恢复或结束会话。

关闭流程使用 20 秒总上限。正常路径等待 Supervisor 取消 worker，Recorder 先向 FFmpeg stdin 写入 `q` 并在自身超时后强制结束子进程，随后协调数据库会话；总上限耗尽时记录脱敏超时诊断并退出，由 `kill_on_drop` 作为最后兜底。实例锁只在 `AppState` 销毁时释放，确保新实例不能在旧实例尚未完成关闭时执行启动恢复。

实例锁、关闭领取、信号接收、关闭完成和关闭超时均向 stderr 输出单行结构化诊断，只包含时间、事件和固定原因枚举，不记录 PID、Cookie、本地路径、页面正文或签名流 URL。

### 10. 禁用会强制结束 Rust 进程的开发 watcher

macOS 实机验证确认 Tauri CLI 的 Rust 文件 watcher 在重建时直接结束旧应用进程，不发送应用可以接管的 `SIGTERM`、`SIGINT` 或 Tauri `ExitRequested`；旧进程因而无法执行 Rust destructor 或 `kill_on_drop`，FFmpeg 会被系统接管并继续写入。应用进程内部无法可靠拦截 `SIGKILL` 或等价的外部强制终止。

项目的 `tauri:dev` 脚本 SHALL 使用 `tauri dev --no-watch`。Vite 仍负责 React/TypeScript 热更新；修改 Rust 后必须先通过 `Ctrl-C` 或显式 `SIGTERM` 触发安全关闭，再重新执行 `make app-dev`。这让默认开发入口遵守与正式客户端相同的录制所有权边界，不以隐藏的孤儿清理脚本掩盖强制终止。

## Risks / Trade-offs

- [抖音再次调整页面脚本结构] → 仅解析有脱敏 fixture 的结构，未知内容返回 `layout_changed`，保留安全诊断而不宽泛猜测。
- [WKWebView 与用户常用浏览器的会话不同] → 在应用内维护独立持久会话，并以系统 WebView 真实验收；不承诺复用外部浏览器状态。
- [隐藏 WebView 无法完成需要交互的标准验证] → 超时后进入 `verification_required`，用户主动显示同一个窗口继续处理。
- [单 WebView 导航成为多房瓶颈] → 解析只发生在轮询与续录边界，5 秒访问门本就需要串行；FFmpeg 录制仍完全并发。
- [导航完成后仍读到前一房间文档] → 请求代次、目标 URL、完整加载状态和目标 `web_rid` 四层校验；不匹配快照只记为等待，绝不启动录制。
- [远程页面尝试调用本地能力] → 验证窗口不加入主 capability，禁止通用 IPC，只由 Rust 主动 `eval_with_callback` 读取白名单快照。
- [脚本快照包含签名 URL] → 只在内存中进入 parser，设置数量与总大小上限，禁止 Debug 输出并在解析结束后释放。
- [用户清除会话导致活动解析失败] → 清理前取消队列和当前请求，活动 FFmpeg 不受影响，后续刷新明确进入 `session_expired`。
- [真实站点测试不稳定] → CI 只运行 fixture/fake；真实直播间验收为显式 ignored 目标并输出诊断证据。
- [用户重复启动导致两个进程同时恢复] → 在任何 SQLite 打开和恢复之前获取操作系统独占锁，第二实例失败即退出。
- [Tauri Rust watcher 强制终止父进程且不发送可接管信号] → 默认开发脚本使用 `--no-watch`，保留 Vite HMR，Rust 修改要求安全停止后手动重启。
- [SIGTERM 不触发 Tauri `ExitRequested`] → 独立监听 Unix 终止信号并复用幂等关闭领取器；用真实 FFmpeg 子进程验证父进程退出后没有孤儿。
- [某个子服务关闭永久等待] → 多服务并发关闭并设置 20 秒总上限，同时保留 Recorder 自身的优雅退出和强制结束兜底。

## Migration Plan

1. 先加入纯 Rust 浏览器快照模型、解析、模式状态机和日志白名单测试，不改变生产 supervisor。
2. 接入 Tauri WebView driver、受限导航和会话控制 commands，使用本地 fixture 页面验证窗口生命周期。
3. 将 StreamerService 和 Supervisor 切换到统一 `RoomResolutionService`，启用新轮询节奏和状态 DTO。
4. 接入前端横幅、操作与浏览器演示降级，更新 README 和 Makefile。
5. 在 macOS 使用指定公开直播间执行原生失败、WebView 成功、开始录制和一次续录验收；失败可通过配置关闭浏览器回退并回滚到原生 resolver。

## Implementation Findings

2026-07-24 的 macOS WKWebView 真实验收确认：目标公开直播页仍在主文档暴露受支持 Flight 数据，实际结构可沿 `roomInfo.web_rid -> roomInfo.room.stream_url` 严格关联目标房间。验收同时暴露了 `navigate()` 返回后短暂保留前一房间完整文档的竞态，因此目标 URL、`readyState` 和父级 `web_rid` 校验是正确性约束，不只是解析优化。该发现不改变禁止网络拦截、Cookie 导出和宽泛对象猜测的安全边界。

2026-07-24 的进程故障注入进一步确认：显式 `SIGTERM` 能在一秒内完成 Supervisor、FFmpeg 和会话关闭，但 Tauri CLI Rust watcher 会绕过信号处理并留下 `PPID=1` 的 FFmpeg。默认开发入口因此改为 `--no-watch`；该约束是录制数据一致性要求，不是开发体验偏好。

2026-07-24 的后续实机诊断确认：一个直播间进入 `verification_required` 后，旧实现会在任何原生请求前短路所有其他房间，同时“检查访问状态”只读取已卡住的快照，导致灰色中间页永久占用共享 WebView。修正后只隔离浏览器导航，其他房间保留原生检查机会；手动检查先重试当前目标的原生通道，仍受限时才执行新代次浏览器重载；验证等待不增加失败次数，重复 probe 采用状态变化加 30 秒心跳记录。真实对照中 `168376497175` 因占用验证目标而未再次尝试原生通道，`625411260021` 则在一次受限结果后约 30 秒通过原生通道恢复录制；同一时刻独立原生诊断确认两个房间均为 `live`，证明差异来自通道状态而非主播支持范围。

2026-07-24 的真实浏览器复现进一步确认：受限直播页主文档标题为“验证码中间页”，滑块组件通过 `https://rmc.bytedance.com/verifycenter/captcha/` iframe 加载。macOS Wry/WKWebView 的导航回调会同时检查主框架和 iframe，且不向应用暴露框架类型；原策略仅允许 `douyin.com`，因此取消了验证码 iframe 及其初始 `about:blank`，窗口只剩灰色 loading。修正后导航策略只额外允许精确 `about:blank` 和该官方验证码路径；任意其他 ByteDance 路径、相似域名、非 HTTPS、新窗口和下载仍被拒绝，房间目标与快照解析仍严格限定为 `live.douyin.com`，远程页面继续不具备 Tauri capability。
