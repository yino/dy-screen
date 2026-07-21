## Context

客户端当前把 `streamers.room_url` 和 `streamers.room_id` 视为创建主播时必须获得的身份：表单只接受 `live.douyin.com/{数字}`，Tauri command 会立即访问直播间页面，SQLite 两列均为 `NOT NULL`，supervisor 每 30 秒直接解析 `room_url`。这套流程无法保存尚未开播、主页上暂时没有直播入口的主播。

示例个人主页在直播时通过头像链接展示“直播中”，链接路径包含稳定 `web_rid=236150550962`，查询参数和内嵌 React Flight 数据包含当前 `roomIdStr=7664620130978581282`。页面文本可能显示“直播中”或“正在直播”，因此文字不是可靠身份来源；结构化载荷中的主页身份、`owner.web_rid`、`roomIdStr` 和直播对象才是主要解析依据。

这一变更跨越 Rust resolver、SQLite 身份模型、Tauri command、supervisor 状态机和 React 表单。实现必须继续限定公开免登录页面，不引入浏览器自动化或账号状态，并让已发现直播入口无缝复用现有直播流解析、并发录制、续录和分片登记链路。

## Goals / Non-Goals

**Goals:**

- 同时支持公开个人主页 URL 和现有公开直播间 URL。
- 允许未开播个人主页先持久化，并在重启后继续等待首次开播。
- 区分稳定主页身份、稳定直播入口与当前直播场次身份。
- 首次发现 `web_rid` 后直接切换到现有直播间检查和录制路径。
- 对稳定入口失效提供有界、可解释的主页重新发现。
- 保证重复来源不会产生重复 worker、重复录制或历史数据误合并。
- 在 UI 中清楚区分等待发现、正在发现、等待开播、检查失败和录制状态。

**Non-Goals:**

- 不使用 WebView DOM、Playwright、浏览器扩展或系统浏览器执行后台主页发现。
- 不导入 Cookie、不要求登录、不处理验证码，不绕过私密、付费或风控访问控制。
- 不根据“直播中”文字或任意数字猜测直播入口。
- 不改变 FFmpeg 录制、清晰度/协议选择、分片格式、视频预览或 ASR/NLP 输入。
- 不自动合并两条都已经拥有历史数据的冲突主播记录。

## Decisions

### 1. 新增独立 `ProfileResolver`，不把个人主页逻辑塞入直播间 resolver

Rust 核心增加个人主页 URL 校验和 `ProfileResolver`。它与 `StreamResolver` 共享相同的 `reqwest::Client` 配置、User-Agent、超时和 React Flight 解码辅助，但返回独立的 `ProfileInspection`：

- `Offline`：已确认 `profile_sec_uid` 和可选昵称，但没有直播入口；
- `Live`：同时包含 `profile_sec_uid`、昵称、稳定 `web_rid`、标准化直播间 URL 和可选当前 `room_id`；
- 错误：无效 URL、网络失败、访问限制或不支持的页面结构。

解析优先读取结构化直播对象及 `owner.web_rid`/`roomIdStr`；找不到时只允许回退到唯一的 `live.douyin.com/{数字}` 链接。直播文字仅用于诊断，不参与 ID 构造。

选择独立 resolver 是为了让个人主页“发现入口”和直播间“解析签名流”保持不同职责。备选方案是扩展 `StreamResolver::resolve` 接受两类 URL，但这会让离线主页、离线直播间和签名流错误混在一个返回类型中。另一个备选方案是后台 WebView 抓取 DOM，它依赖登录状态、占用更多资源且不适合无界面运行，因此不采用。

### 2. 使用 `profile_sec_uid`、`web_rid`、`room_id` 三层身份

`streamers` 迁移为：

- `source_kind TEXT NOT NULL`：`profile` 或 `room`；
- `source_url TEXT NOT NULL`：用户提交并规范化后的主页或直播间来源；
- `profile_sec_uid TEXT NULL`：个人主页稳定身份，使用 partial unique index；
- `web_rid TEXT NULL`：稳定直播入口路径 ID，使用 partial unique index；
- `room_url TEXT NULL`：发现后固定为 `https://live.douyin.com/{web_rid}`；
- `room_id TEXT NULL`：最近一次直播页解析出的当前房间 ID，不承担长期唯一性。

现有 `room_id NOT NULL UNIQUE` 约束需要通过 SQLite 表重建迁移移除，避免相同稳定入口在新直播周期出现不同 `room_id` 时被误认为新主播。会话和录像继续通过内部 `streamer_id` 关联，不受外部身份变化影响。

备选方案是复用 `room_url` 保存个人主页、用空字符串代表未知房间；它会把状态编码进字符串、破坏 URL 类型边界并使 supervisor 容易把主页误交给直播间 resolver，因此不采用。

### 3. 添加时必须确认主页身份，但不要求正在直播

创建命令先规范化来源：

- 直播间来源继续要求名称，立即解析直播间并写入 `web_rid`、`room_url` 和当前 `room_id`；
- 个人主页来源必须至少成功解析 `profile_sec_uid` 和主页身份。若正在直播，同时写入直播绑定；若未直播，则允许直播字段为空。名称为空时使用主页昵称；如果页面也没有昵称则返回表单错误。

网络错误或页面结构不支持时不创建记录，因为系统无法确认 URL 是否属于有效公开主播。成功确认但未直播不是错误，记录进入 `waiting_first_live`。

### 4. supervisor 根据是否存在 `web_rid` 选择阶段

worker 每轮读取最新主播记录：

- 没有 `web_rid`：进入主页发现阶段，状态为 `discovering`，成功离线后为 `waiting_first_live`；基础间隔 60 秒并加入 0–10 秒随机抖动；主页可重试错误使用 60、120、300 秒封顶退避。
- 已有 `web_rid`：直接使用 `room_url` 进入现有直播间 resolver；离线时保持 30 秒基础周期。

个人主页首次返回 `Live` 时，repository 先在事务中保存 `web_rid`、`room_url` 和当前 `room_id`，再立即继续同一 worker 的直播间解析，不等待下一个周期。这样 FFmpeg 仍只消费 `StreamResolver` 验证过的 `RoomStreams`。

随机抖动由可注入或可测试的延迟函数生成，避免应用启动时多个主页 worker 同时请求。用户“立即检查”、系统唤醒和网络恢复绕过正常等待但仍通过现有去重唤醒通道。

### 5. 只对明确入口失效回退主页

直播间 resolver 的错误分类扩展为 `Offline`、`Retryable` 和 `EntryInvalid`。只有 HTTP 404、连续页面结构不支持或明确入口不存在计入入口失效次数；正常离线和普通网络错误不计数。具有个人主页来源的主播连续 3 次 `EntryInvalid` 后清除当前直播绑定，状态改为 `rediscovering` 并立即运行主页发现。

清除动作只影响 `web_rid`、`room_url` 和当前 `room_id`，保留 `profile_sec_uid`、来源 URL、内部主播 ID 和历史数据。直播间直连来源没有主页可回退，连续入口失效只显示错误，不猜测其他入口。

### 6. 重复身份在 repository 事务中处理

创建、编辑和延迟发现绑定都通过统一 repository 方法检查 partial unique identity：

- 已有活动记录：返回目标主播 ID；
- 已归档记录：恢复该记录并绑定缺失的兼容来源身份；
- 延迟发现的临时主页记录无会话/视频：把主页身份绑定到已有目标，停止临时 worker并删除临时记录，发布包含目标 ID 的 `streamer_merged` 事件；
- 两条记录都有历史：不自动合并，暂停冲突记录并返回脱敏冲突错误。

自动合并只处理没有历史的临时记录，因此不会移动外键或文件。备选方案是新增多来源 alias 表；它更通用，但当前只有主页和直播间两类来源，会显著扩大第一阶段迁移和 UI 范围，因此暂不采用。

### 7. 表单使用统一来源字段并由 backend 最终校验

前端将“直播间链接”改为“个人主页或直播间链接”，接受 `/user/{profile_sec_uid}` 和 `live.douyin.com/{web_rid}`。浏览器演示 API只模拟 URL 分类和状态，不伪装真实主页解析。名称输入对个人主页标记为可选，对直播间直连仍由 backend 返回必填错误。

主播 DTO 增加来源类型、来源 URL、`profileSecUid`、`webRid`、可选 `roomUrl`/`roomId`。表格新增来源标签和发现状态；已发现主页显示标准化直播入口，未发现时禁用“打开直播间”。

### 8. 编辑来源采用停止、校验、事务更新、恢复顺序

只修改名称时不停止 worker或清除身份。来源变化时先停止 worker，再校验新来源和冲突；repository 事务成功后按监听开关启动新阶段。校验或写库失败时恢复原记录和原 worker。历史会话始终保留在同一内部主播 ID 下。

## Risks / Trade-offs

- [抖音个人主页 React Flight 结构变化] → 使用独立 fixture、结构化递归查找和唯一直播链接回退；返回稳定页面结构错误，不使用文字猜测。
- [公开主页频繁访问触发限流或风控] → 未发现阶段使用 60 秒加抖动、错误有界退避，并限制同一主播只有一个 worker。
- [稳定 `web_rid` 与当前 `room_id` 语义混淆] → DTO、数据库列和测试明确区分，录制会话仍以内部主播 ID 关联。
- [SQLite 表重建迁移风险] → 在事务中创建新表、复制并校验行数、重建索引和外键；迁移测试覆盖旧库、归档记录和历史关系。
- [延迟发现后与已有主播冲突] → 只自动处理无历史临时记录；有历史冲突暂停并要求人工决策。
- [首次发现需要主页和直播间两次请求] → 接受一次额外请求以保持 FFmpeg 入口只依赖现有经过验证的直播间 resolver。
- [入口失效误判导致不必要主页回查] → 只统计明确 `EntryInvalid`，连续 3 次才切换；普通离线和网络错误不清除绑定。

## Migration Plan

1. 增加新的 SQLite migration，通过表重建引入来源和三层身份字段，回填现有直播间记录的 `source_kind='room'`、`source_url=room_url` 和 URL 路径中的 `web_rid`。
2. 先增加核心个人主页模型、URL 校验和 fixture 解析测试，再接入真实 HTTP executor。
3. 扩展 repository 和 DTO，保持现有直播间创建、编辑和 worker 测试继续通过。
4. 将 supervisor 拆分为主页发现阶段和现有直播间阶段，增加抖动、入口失效计数、延迟冲突合并及重启恢复测试。
5. 更新 Tauri command、React 表单、演示 API、状态标签和 README，使用示例主页 fixture 完成端到端验收。
6. 回滚时保留新增列和数据，只移除个人主页入口及发现 worker；现有直播间来源仍可继续使用回填后的 `room_url`。

## Open Questions

本次设计决策均已确认。短链接、Cookie/登录访问、多平台真实网络风控差异和拥有历史数据的手动合并界面分别留作后续变更。
