## 1. 个人主页发现核心

- [x] 1.1 在 `tests/profile_resolver.rs` 先添加个人主页 URL 规范化失败测试，覆盖 `douyin.com/user/{profile_sec_uid}`、查询参数清理、错误 host、缺失身份和不支持 scheme
- [x] 1.2 在 `tests/fixtures/` 增加脱敏的正在直播、未开播、仅含直播文字、唯一直播链接回退和页面结构变化主页 fixture，确保不包含真实签名流 URL
- [x] 1.3 在 `src/model.rs` 定义来源类型、个人主页身份、`ProfileInspection`、稳定 `web_rid`、当前 `room_id` 和可分类发现错误模型，并验证序列化不泄露页面载荷
- [x] 1.4 抽取或扩展 React Flight 解码辅助，使直播间和个人主页 resolver 共享载荷解码但保持各自对象识别逻辑，运行现有 resolver 测试确认无回归
- [x] 1.5 在新的 `src/profile_resolver.rs` 实现结构化个人主页解析，优先读取主页身份、`roomIdStr`、直播对象和 `owner.web_rid`
- [x] 1.6 实现唯一 `live.douyin.com/{web_rid}` 链接回退，并测试“直播中/正在直播”文字不能单独产生房间身份
- [x] 1.7 实现使用现有 User-Agent、连接超时和总超时的公开 HTTP `ProfileResolver`，明确区分 Offline、网络可重试、访问限制和 UnsupportedPageLayout
- [x] 1.8 为个人主页发现日志和错误添加脱敏测试，确认不输出 HTML、React Flight 全量载荷、跟踪参数或签名直播流 URL

## 2. 三层身份与 SQLite 迁移

- [x] 2.1 在 `src-tauri/tests/repository.rs` 先构造旧版数据库并添加 migration 失败测试，验证现有主播、归档状态、监听开关、会话和视频关系必须完整保留
- [x] 2.2 在 `src-tauri/src/database.rs` 增加事务性表重建 migration，引入 `source_kind`、`source_url`、可空 `profile_sec_uid`、`web_rid`、`room_url` 和 `room_id`
- [x] 2.3 为 `profile_sec_uid` 和 `web_rid` 建立非空 partial unique index，移除 `room_id` 的长期唯一性，并测试相同 `web_rid` 不得创建重复主播
- [x] 2.4 迁移现有记录为 `source_kind='room'`，从规范化 `room_url` 路径回填 `web_rid`；对无法回填的异常旧 URL保留记录并写入可诊断状态
- [x] 2.5 扩展 `src-tauri/src/domain.rs`、数据库映射和前后端 DTO，使个人主页来源允许尚未存在直播间 URL、`web_rid` 和当前 `room_id`
- [x] 2.6 为 repository 增加按 `profile_sec_uid`、`web_rid` 查询及原子绑定方法，覆盖活动重复、已归档恢复和当前 `room_id` 更新
- [x] 2.7 实现延迟发现冲突事务：无历史临时记录绑定主页身份到已有主播并删除临时记录；双方有历史时保留数据、暂停冲突记录并返回目标 ID
- [x] 2.8 添加迁移幂等、重复身份、归档恢复、延迟合并、历史冲突和新直播周期更新 `room_id` 的 repository 回归测试

## 3. 创建与编辑主播命令

- [x] 3.1 在 `src-tauri/tests/app_helpers.rs` 添加统一来源 URL 分类测试，覆盖个人主页、直播间、跟踪参数清理和不支持 URL
- [x] 3.2 重构 `src-tauri/src/app_support.rs`，分别规范化个人主页来源和直播间来源，保留现有直播间 URL 校验错误语义
- [x] 3.3 为 `create_streamer` 添加先失败测试：正在直播主页保存完整三层身份、未开播主页保存可空直播字段、空名称使用主页昵称、直播间直连空名称仍失败
- [x] 3.4 修改 `create_streamer`，个人主页必须先确认公开主页身份；正在直播时先检查 `web_rid` 冲突，未开播时保存为等待首次开播
- [x] 3.5 为 `update_streamer` 添加只改名称不重置身份、切换来源后重新发现、来源冲突恢复原 worker和原记录的测试
- [x] 3.6 修改 `update_streamer` 为停止 worker、校验来源、事务更新、按原监听开关恢复的顺序，并确保失败时恢复旧 worker
- [x] 3.7 扩展 Tauri command 错误码和中文摘要，区分无效主页、主页无法访问、主页结构变化、重复主页和重复直播入口

## 4. supervisor 双阶段状态机

- [x] 4.1 为 supervisor 注入可替换的个人主页 resolver、直播间 resolver、抖动生成器和延迟函数，使轮询与退避可使用确定性时间测试
- [x] 4.2 添加启动恢复测试：无 `web_rid` 的已启用主页恢复发现阶段，有 `web_rid` 的主播直接恢复现有 30 秒直播间阶段，暂停主播不启动请求
- [x] 4.3 实现个人主页发现阶段及 `discovering`、`waiting_first_live`、`profile_error` 状态，使用 60 秒加 0–10 秒抖动和 60/120/300 秒错误退避
- [x] 4.4 添加首次发现测试，验证 repository 必须先原子保存 `web_rid`、标准化 `room_url` 和当前 `room_id`，随后同一轮立即调用直播间 resolver
- [x] 4.5 把首次发现结果接入现有并发许可、磁盘检查和 `record_live` 流程，确认 FFmpeg 只接收 `StreamResolver` 验证后的 `RoomStreams`
- [x] 4.6 扩展直播间错误分类为 Offline、Retryable 和 EntryInvalid，并测试正常离线与普通网络失败不清除稳定入口
- [x] 4.7 实现连续 3 次 EntryInvalid 后的 `rediscovering` 回退，清除直播绑定但保留个人主页身份、内部主播 ID和历史数据
- [x] 4.8 添加延迟发现重复身份测试，验证临时 worker停止、发布 `streamer_merged` 目标 ID、不会启动第二个录制，历史冲突只暂停不删除
- [x] 4.9 验证用户立即检查、系统唤醒、网络恢复、暂停/恢复、暂停全部、应用退出和取消退避在两种阶段均保持去重和可取消
- [x] 4.10 验证相同 `web_rid` 后续解析出不同 `room_id` 时仍复用同一主播和逻辑直播周期，并把最新 `room_id` 用于新录制会话

## 5. React 表单和状态展示

- [x] 5.1 扩展 `ui/src/types.ts` 和 `ui/src/api.ts` 的主播来源字段、可空直播身份、发现状态与 `streamer_merged` 事件类型
- [x] 5.2 在 `ui/src/App.test.tsx` 先添加表单测试，覆盖个人主页和直播间 URL、个人主页名称可空、直播间名称必填及 backend 字段错误展示
- [x] 5.3 将添加/编辑弹窗字段改为“个人主页或直播间链接”，更新 placeholder、帮助文本和提交逻辑，最终有效性仍以后端校验为准
- [x] 5.4 添加主播表格组件测试，覆盖来源标签、等待首次开播、正在发现、主页检查失败、已发现直播间和重新发现直播间状态
- [x] 5.5 更新监控表格和详情区域：未发现入口时禁用“打开直播间”，已发现时展示标准化 `live.douyin.com/{web_rid}` 地址
- [x] 5.6 处理 `streamer_merged` 事件，刷新列表并定位到目标主播，不保留已删除临时记录的选中状态
- [x] 5.7 更新浏览器演示 API，模拟来源分类和发现状态但明确不执行真实主页解析、网络轮询或录制

## 6. 集成、安全与兼容验收

- [x] 6.1 使用脱敏示例主页 fixture 执行核心集成测试，验证 `profile_sec_uid`、昵称、`web_rid=236150550962` 和当前 `room_id` 的稳定映射
- [x] 6.2 添加未开播到首次开播的端到端 supervisor 测试，验证保存、重启恢复、首次发现、持久化、现有 resolver 接管和录制会话创建顺序
- [x] 6.3 添加直播入口失效到主页重新发现的端到端测试，覆盖三次阈值、正常离线不回查、网络失败不清除和新 `web_rid` 更新
- [x] 6.4 验证多个人主页 worker 的抖动与退避不会突破单主播单 worker约束，也不会阻塞已知直播间检查、录制分片登记或应用退出
- [x] 6.5 检查代码和依赖中未引入浏览器自动化、Cookie 存储、登录或验证码处理，并验证日志、事件和错误仍不泄露签名 URL或主页载荷
- [ ] 6.6 在 macOS 使用公开示例主页执行只读手工验收：表单添加、发现直播入口、进入现有监听流程、暂停恢复和重启恢复；记录 Windows/Linux 尚待实机验证边界

> 2026-07-21 验收记录：公开示例主页当前返回抖音访问控制引导页，未提供免登录主页身份或 React Flight 数据。客户端已正确分类为访问受限并停止本轮发现；按本变更的公开免登录边界，不执行 Cookie、登录、验证码或浏览器自动化绕过。因此本项仍等待平台重新提供可直接访问的公开 HTML。自动化测试已覆盖表单、首次发现、暂停恢复、重启恢复和现有录制链路。

## 7. 文档和最终验证

- [x] 7.1 更新中文 README，说明两类添加链接、名称补全、等待首次开播、三层身份、轮询周期、入口失效回查及公开免登录限制
- [x] 7.2 更新 Makefile 帮助和定向测试目标，使开发者可以运行个人主页 resolver fixture、数据库迁移和双阶段 supervisor 测试
- [x] 7.3 运行 Rust 格式检查、Clippy、核心测试、Tauri 测试、前端测试和类型构建，确保现有直播间直连、录制、视频库和预览功能无回归
- [x] 7.4 运行个人主页真实只读验收、`openspec validate --all --strict` 和 Tauri release 构建，并完成最终需求逐项对照
