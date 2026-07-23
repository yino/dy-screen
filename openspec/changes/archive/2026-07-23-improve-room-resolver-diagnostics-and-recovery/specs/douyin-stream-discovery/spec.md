## MODIFIED Requirements

### Requirement: 从页面初始化数据发现直播流
系统 SHALL 解析公开直播页的受支持 React Flight 初始化数据，并返回稳定直播入口对应的当前房间标识、直播状态、默认清晰度以及可用的签名 FLV/HLS 直播流变体；系统 MUST 在解析前区分访问限制页面、明确失效 HTTP 状态和未知页面结构。

#### Scenario: 正在直播的房间包含直播流数据
- **WHEN** 获取到的页面包含带有 `stream_url` 的受支持房间对象
- **THEN** 系统返回当前 `room_id` 和按清晰度、协议规范化后的直播流变体，并允许 repository 更新主播的最新 `room_id`

#### Scenario: 房间已下播或没有直播流数据
- **WHEN** 页面中找不到包含可用直播流 URL 的在线房间对象，但能从受支持结构确认该稳定直播入口存在
- **THEN** 系统返回明确的离线结果，并保留已知 `web_rid`

#### Scenario: 页面要求额外访问权限
- **WHEN** HTTP 状态或页面安全 marker 表明需要登录、验证码或额外访问权限
- **THEN** 系统返回 `access_restricted` 分类和脱敏中文错误，不继续解析页面正文

#### Scenario: 页面结构不受支持
- **WHEN** 请求成功但页面没有可由脱敏 fixture 和测试证明的受支持房间结构
- **THEN** 系统返回 `layout_changed` 分类，不将其标记为入口失效，也不在错误中包含页面载荷或签名直播流 token

#### Scenario: HTTP 明确表示入口失效
- **WHEN** 规范化直播间请求返回 HTTP 404 或 410
- **THEN** 系统返回 `entry_invalid` 分类，不把其他 HTTP 错误归入该分类

## ADDED Requirements

### Requirement: 提供安全的直播间诊断
系统 SHALL 提供只读直播间诊断能力，并 MUST 将输出限制为规范化房间 URL、可选 HTTP 状态、可选 Content-Type、响应字节数、安全 marker 布尔值、分类和脱敏中文错误。

#### Scenario: 诊断未识别页面
- **WHEN** 用户对请求成功但结构未知的公开直播间执行 `inspect-room`
- **THEN** 命令返回 `layout_changed`、HTTP 元信息和安全 marker，不输出 HTML、Cookie、React Flight 原始载荷或签名流 URL

#### Scenario: 诊断网络失败
- **WHEN** 直播间请求在收到 HTTP 响应前发生超时或网络错误
- **THEN** 命令返回 `retryable_error` 和脱敏错误，HTTP 状态为空且不输出底层请求敏感信息

#### Scenario: 诊断输入 URL 无效
- **WHEN** 输入不是受支持的 `live.douyin.com` 公开直播间 URL
- **THEN** 命令不发起网络请求并返回 `entry_invalid` 和脱敏校验错误

### Requirement: 仅支持经过 fixture 验证的页面格式
系统 MUST 只为具有脱敏 fixture、成功解析测试、离线测试和敏感数据不泄露测试的页面格式增加直播间解析适配。

#### Scenario: 线上出现未知字段组合
- **WHEN** 公开页面出现尚无脱敏 fixture 覆盖的新字段组合
- **THEN** 系统保持 `layout_changed` 分类，不通过宽泛字段猜测选择房间对象或签名直播流
