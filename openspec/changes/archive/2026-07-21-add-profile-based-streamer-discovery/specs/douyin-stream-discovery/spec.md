## MODIFIED Requirements

### Requirement: 校验支持的直播间 URL
系统 SHALL 接受 host 为 `live.douyin.com` 的公开直播间 URL作为直播流解析输入，并 SHALL 接受经过个人主页发现流程验证的标准化直播间 URL；任何用于 FFmpeg 的流发现请求最终 MUST 规范化为 `https://live.douyin.com/{web_rid}`。

#### Scenario: 有效的公开直播间 URL
- **WHEN** 用户直接提供或个人主页发现流程返回一个 HTTPS `live.douyin.com/{web_rid}` URL
- **THEN** 系统接受该 URL 并用于现有直播流发现

#### Scenario: 个人主页尚未发现直播入口
- **WHEN** 个人主页有效但当前没有稳定 `web_rid`
- **THEN** 系统不得调用直播流 resolver 或启动 FFmpeg，而是继续等待主页发现

#### Scenario: 不支持的 host
- **WHEN** 最终直播间 URL host 不是 `live.douyin.com`
- **THEN** 系统返回校验错误，且不启动 FFmpeg

### Requirement: 从页面初始化数据发现直播流
系统 SHALL 解析公开直播页的 React Flight 初始化数据，并返回稳定直播入口对应的当前房间标识、直播状态、默认清晰度以及可用的签名 FLV/HLS 直播流变体。

#### Scenario: 正在直播的房间包含直播流数据
- **WHEN** 获取到的页面包含带有 `stream_url` 的房间对象
- **THEN** 系统返回当前 `room_id` 和按清晰度、协议规范化后的直播流变体，并允许 repository 更新主播的最新 `room_id`

#### Scenario: 房间已下播或没有直播流数据
- **WHEN** 页面中找不到包含可用直播流 URL 的在线房间对象，但能确认该稳定直播入口存在
- **THEN** 系统返回明确的离线结果，并保留已知 `web_rid`

#### Scenario: 页面结构不受支持
- **WHEN** 页面包含初始化脚本，但无法使用受支持的载荷结构进行解码
- **THEN** 系统返回可分类的入口结构错误，不包含任何签名直播流 token，并允许 supervisor 统计是否需要主页重新发现
