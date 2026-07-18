## ADDED Requirements

### Requirement: 校验支持的直播间 URL
系统 SHALL 只接受 host 为 `live.douyin.com` 的 HTTP 或 HTTPS URL，并 SHALL 在发起录制请求前拒绝不支持的 host。

#### Scenario: 有效的公开直播间 URL
- **WHEN** 用户提供一个 HTTPS `live.douyin.com` 直播间 URL
- **THEN** 系统接受该 URL 并用于直播流发现

#### Scenario: 不支持的 host
- **WHEN** 用户提供的 URL host 不是 `live.douyin.com`
- **THEN** 系统返回校验错误，且不启动 FFmpeg

### Requirement: 从页面初始化数据发现直播流
系统 SHALL 解析公开直播页的 React Flight 初始化数据，并返回房间标识、直播状态、默认清晰度以及可用的签名 FLV/HLS 直播流变体。

#### Scenario: 正在直播的房间包含直播流数据
- **WHEN** 获取到的页面包含带有 `stream_url` 的房间对象
- **THEN** 系统返回按清晰度和协议规范化后的直播流变体

#### Scenario: 房间已下播或没有直播流数据
- **WHEN** 页面中找不到包含可用直播流 URL 的在线房间对象
- **THEN** 系统返回明确的“已下播或直播流不可用”错误

#### Scenario: 页面结构不受支持
- **WHEN** 页面包含初始化脚本，但无法使用受支持的载荷结构进行解码
- **THEN** 系统返回不包含任何签名直播流 token 的页面结构变更错误

### Requirement: 确定性选择直播流变体
系统 SHALL 在用户请求的清晰度可用时选择该清晰度，否则选择页面默认清晰度，否则选择已知最高的可用清晰度；在选定清晰度中，除非用户明确请求协议，否则系统 SHALL 优先选择 FLV 而不是 HLS。

#### Scenario: 请求的清晰度和协议均可用
- **WHEN** 用户请求 `HD1` 和 FLV，且两者都可用
- **THEN** 系统选择 `HD1` FLV URL

#### Scenario: 请求的清晰度不可用
- **WHEN** 请求的清晰度不存在，但页面默认清晰度可用
- **THEN** 系统选择页面默认清晰度并报告发生了回退

#### Scenario: 选定清晰度没有 FLV
- **WHEN** 系统优先选择 FLV，但选定清晰度只有 HLS
- **THEN** 系统选择 HLS 变体

### Requirement: 避免在常规状态输出中泄露签名 URL
系统 MUST NOT 在普通日志、错误或结构化状态事件中包含签名 CDN URL 的查询字符串。

#### Scenario: 直播流解析成功
- **WHEN** 系统解析到包含鉴权查询参数的直播流 URL
- **THEN** 常规输出只标识协议、清晰度和已脱敏 endpoint，不暴露查询字符串
