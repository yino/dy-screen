## ADDED Requirements

### Requirement: 在原生 HTTP 和浏览器会话之间受控回退
系统 SHALL 以原生 HTTP 作为公开直播页快速通道，并 SHALL 在其返回 `access_restricted` 时通过持久化浏览器会话重试；浏览器回退结果 MUST 使用与原生通道相同的 `RoomInspection` 和流选择规则。

#### Scenario: 原生解析成功
- **WHEN** 原生 HTTP 页面包含受支持在线或离线房间结构
- **THEN** 系统直接返回结果，不创建或导航浏览器窗口

#### Scenario: 原生解析命中访问限制
- **WHEN** 原生 HTTP 返回访问限制状态或安全 marker
- **THEN** 系统记录原生尝试并切换到浏览器通道，不把房间误报为离线、入口失效或页面结构变化

#### Scenario: 浏览器解析成功
- **WHEN** 浏览器会话获得受支持房间结构
- **THEN** 系统仅在文档 URL、完整加载状态和房间父级 `web_rid` 均关联目标入口时返回当前 `room_id`、状态和流变体，并标识解析通道为 `browser`

#### Scenario: 浏览器也需要用户处理
- **WHEN** 浏览器页面在有界探测后仍处于访问验证状态
- **THEN** 系统返回 `verification_required`，不启动 FFmpeg 且不继续原生 HTTP 密集重试

### Requirement: 对风控通道使用粘性选择
系统 SHALL 在一次原生 HTTP 访问受限后至少 30 分钟优先使用浏览器通道，并 SHALL 在冷却期结束后最多执行一次原生快速通道试探。

#### Scenario: 冷却期内检查其他主播
- **WHEN** 任一公开直播页已触发原生访问限制且冷却期尚未结束
- **THEN** 后续房间检查直接进入浏览器解析队列，不先重复原生请求

#### Scenario: 风控前已发出的原生请求较晚成功
- **WHEN** 多个普通原生检查已并发取得许可，其中一个先触发浏览器粘性模式，另一个随后返回成功
- **THEN** 较晚的普通成功不得清除浏览器粘性模式，只有冷却期后的唯一恢复探测成功才能恢复原生优先

#### Scenario: 冷却期结束后原生通道恢复
- **WHEN** 单次原生试探返回受支持结构
- **THEN** 系统恢复原生优先模式并继续保留可复用浏览器会话

#### Scenario: 冷却期结束后仍受限制
- **WHEN** 单次原生试探再次返回访问限制
- **THEN** 系统重置冷却期并继续使用浏览器通道

## MODIFIED Requirements

### Requirement: 从页面初始化数据发现直播流
系统 SHALL 从原生 HTTP 响应或受控浏览器最小页面快照中解析公开直播页的受支持 React Flight 初始化数据，并返回稳定直播入口对应的当前房间标识、直播状态、默认清晰度以及可用的签名 FLV/HLS 直播流变体；系统 MUST 在解析前区分访问限制页面、需要用户访问验证、明确失效 HTTP 状态和未知页面结构。

#### Scenario: 正在直播的房间包含直播流数据
- **WHEN** 任一受支持通道获得带有 `stream_url` 的房间对象
- **THEN** 系统返回当前 `room_id` 和按清晰度、协议规范化后的直播流变体，并允许 repository 更新主播的最新 `room_id`

#### Scenario: 房间已下播或没有直播流数据
- **WHEN** 页面中找不到包含可用直播流 URL 的在线房间对象，但能从受支持结构确认该稳定直播入口存在
- **THEN** 系统返回明确的离线结果，并保留已知 `web_rid`

#### Scenario: 原生页面要求额外访问验证
- **WHEN** 原生 HTTP 状态或页面安全 marker 表明公开页面当前需要额外访问验证
- **THEN** 系统返回 `access_restricted` 通道结果并交由受控浏览器回退，不继续解析原生页面正文

#### Scenario: 浏览器页面仍要求用户处理
- **WHEN** 系统 WebView 在有界探测后仍无法进入受支持公开房间页面
- **THEN** 系统返回 `verification_required` 和脱敏中文错误，不自动处理页面交互

#### Scenario: 页面结构不受支持
- **WHEN** 任一通道请求成功但页面没有可由脱敏 fixture 和测试证明的受支持房间结构
- **THEN** 系统返回 `layout_changed` 分类，不将其标记为入口失效，也不在错误中包含页面载荷或签名直播流 token

#### Scenario: HTTP 明确表示入口失效
- **WHEN** 规范化直播间的原生请求返回 HTTP 404 或 410
- **THEN** 系统返回 `entry_invalid` 分类，不启动浏览器回退且不把其他 HTTP 错误归入该分类
