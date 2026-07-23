## Context

直播间 resolver 当前把非成功 HTTP 统一折叠为 `reqwest::Error`，把无法识别的 HTML 统一折叠为 `UnsupportedPageLayout`。supervisor 随后又把 `UnsupportedPageLayout`、非法 URL 和 HTTP 404 合并为 `EntryInvalid`，导致平台访问限制和页面结构变化也会累计“入口失效”次数。个人主页 resolver 已经具有访问限制页面特征识别，但直播间链路没有复用。

客户端虽然创建并清理日志目录，但监听检查没有持续写入可用于定位问题的日志；主播 DTO 也只有最后错误文本，无法说明连续失败次数和下次重试时间。本变更跨越 Rust 核心、SQLite、Tauri supervisor、CLI 和 React 界面，同时必须确保页面正文、Cookie 和签名流 URL 不进入诊断输出。

## Goals / Non-Goals

**Goals:**

- 用稳定、可测试的分类区分入口失效、访问受限、页面结构变化、暂时网络错误、正常离线和正常直播。
- 只有明确的非法 URL、HTTP 404 或 410 才参与入口失效恢复逻辑。
- 持久化并展示连续失败次数和下次重试时间，使应用重启后仍有可解释状态。
- 提供不会泄露页面载荷或签名地址的 CLI 诊断、终端日志和 14 天文件日志。
- 保持核心 resolver 不依赖 Tauri，使诊断和解析能力仍可由 CLI、测试及桌面后端复用。

**Non-Goals:**

- 不导入账号 Cookie、登录态或验证码结果，不通过浏览器自动化或 WebView 抓包绕过访问控制。
- 不承诺解析没有脱敏 fixture 和测试覆盖的未知页面格式。
- 不改变 FFmpeg 录制、MKV 分片、MP4 预览、ASR 或 AI 切片流程。
- 不保存页面 HTML、React Flight 原始载荷或签名直播流地址用于事后分析。

## Decisions

### 1. 在 Rust 核心中建立房间诊断分类

新增可序列化的房间诊断模型，分类值为 `live`、`offline`、`access_restricted`、`layout_changed`、`entry_invalid` 和 `retryable_error`。诊断只包含规范化房间 URL、可选 HTTP 状态、可选 Content-Type、响应字节数以及访问限制、Flight 初始化数据和受支持房间对象等布尔特征。

`StreamResolver` 的普通 `inspect` 和安全 `diagnose` 共用同一次页面获取及分类函数。前者在分类后返回业务对象或类型化错误，后者只返回脱敏诊断对象。相比在 supervisor 中分析错误字符串，此方案让 CLI、Tauri 和测试共享同一事实来源。

备选方案是在 supervisor 中按字符串匹配现有错误。该方案无法可靠区分 HTTP 状态与正文特征，也容易因错误文案变化失效，因此不采用。

### 2. 复用访问限制页面特征

将个人主页已有的访问限制特征判断提升为 crate 内共享函数，直播间页面在尝试 Flight 解析前先检查该函数。只检查固定布尔 marker，不提取或输出 nonce、签名和验证码内容。

HTTP 401、403、418 和 429 直接分类为访问受限；404 和 410 分类为入口失效；其他 4xx/5xx 及网络错误分类为可重试。页面请求成功但无受支持结构时分类为页面结构变化。

### 3. supervisor 使用分类驱动状态机

扩展 `ResolveFailure`，分别表示 `Offline`、`AccessRestricted`、`LayoutChanged`、`Retryable` 和 `EntryInvalid`。访问受限、页面结构变化和网络错误都按 30、60、120、300 秒封顶退避，并永久保留 `web_rid`；只有 `EntryInvalid` 连续 3 次且来源为个人主页时才清除直播绑定并回查主页。

新增监听状态 `access_restricted` 和 `layout_changed`。用户立即检查会唤醒原 worker，并立即清除等待中的预计重试时间，随后重新分类。

### 4. 持久化最小诊断状态

SQLite v5 migration 在 `streamers` 增加非空 `failure_count` 和可空 `next_retry_at`。migration 逐列检查后使用 `ALTER TABLE` 和默认值保留所有现有主播、标签、会话和录像，因而也能接管曾经提前创建这些列但没有 v5 标记的开发数据库。成功、正常离线、暂停或开始新检查时清零诊断状态；失败时由 supervisor 原子写入中文错误、失败次数和 UTC 重试时间。

开发期间诊断迁移曾短暂占用 v4，而合入的 AI schema 同样使用 v4。启动时若发现 v4 已登记但四张 AI 表不存在，系统以幂等建表修复 AI schema，并额外登记 v6；不重写历史迁移标记，也不丢失既有主播和录像。

没有新增页面响应表，避免在数据库中形成敏感载荷存档。回滚代码时新增列可以保留，旧二进制会忽略它们。

### 5. 使用 JSON Lines 写入安全日志

Tauri 启动时把应用日志目录注入 supervisor。每次直播间检查完成后写一行 JSON，字段仅包括时间、`streamer_id`、`web_rid`、分类、HTTP 状态、连续失败次数和下次重试时间。开发构建同时写到标准错误，正式构建至少写文件。日志文件按 UTC 日期命名，沿用启动时 14 天清理逻辑。

日志写入失败不得中断监听，只在开发终端输出一次脱敏 I/O 错误。为避免新增日志框架依赖，第一版使用进程内互斥文件追加器；单条日志先序列化完整后一次追加。

### 6. UI 直接呈现可操作诊断

主播表格新增两种中文状态“访问受限”和“页面结构变化”。最近检查列直接显示经过后端脱敏的中文错误摘要，并在存在失败时显示“连续 N 次”和预计重试时间。操作菜单保留立即检查，增加打开直播间和打开日志目录；没有稳定入口时禁用打开直播间。

浏览器演示 API 同步新 DTO 默认值，但不模拟真实网络诊断。

## Risks / Trade-offs

- [平台可能同时改变 marker 和 Flight 格式] → 未识别响应保持 `layout_changed` 并保留入口，通过安全诊断和后续脱敏 fixture 再增加适配。
- [HTTP 403 也可能由区域或临时 CDN 策略产生] → 统一作为可重试的访问受限，不宣称入口永久失效。
- [应用异常退出时内存失败计数可能与数据库短暂不同] → 每次失败原子持久化，worker 启动时读取已保存次数作为退避起点。
- [同步文件追加可能造成短暂阻塞] → 单条日志很小且只在状态检查完成时写入；后续如日志量增大可迁移为异步通道。
- [诊断 marker 不能解释所有平台响应] → marker 只作为安全证据，不输出或保存正文；分类结果明确允许“不支持的页面结构”。

## Migration Plan

1. 运行 SQLite v5 migration，为旧主播补充 `failure_count = 0` 和 `next_retry_at = NULL`；已有列时只补 migration 标记。
2. 发布同时更新的 Rust DTO 与 TypeScript 类型，避免字段不一致。
3. 启动已有 worker；旧的 `entry_invalid` 记录在第一次新检查时按新分类刷新，不主动清除现有 `web_rid`。
4. 旧开发数据库若存在诊断 v4 冲突，则以 v6 记录 AI schema 修复；如需回滚应用代码，保留新增列和修复标记即可，旧代码的显式 SELECT 不受影响。

## Open Questions

无。新增页面格式必须先取得合法、脱敏的 fixture，再通过后续 OpenSpec 变更增加解析适配。
