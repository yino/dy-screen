## ADDED Requirements

### Requirement: 校验并规范化公开个人主页
系统 SHALL 接受 host 为 `www.douyin.com` 或 `douyin.com`、路径为 `/user/{profile_sec_uid}` 的 HTTP/HTTPS 公开个人主页 URL，并 SHALL 移除查询参数和 fragment 后再持久化。

#### Scenario: 提交有效个人主页
- **WHEN** 用户提交包含有效 `profile_sec_uid` 的公开个人主页 URL
- **THEN** 系统返回规范化主页 URL 和稳定的 `profile_sec_uid`

#### Scenario: 提交不支持的个人主页
- **WHEN** URL host、scheme 或路径不符合公开抖音个人主页格式
- **THEN** 系统拒绝该 URL，且不发起主页请求或写入 SQLite

### Requirement: 从结构化数据发现直播身份
系统 SHALL 优先解析个人主页 React Flight 或等价内嵌结构化数据中的主页昵称、`roomIdStr`、`owner.web_rid` 和直播对象，并 SHALL 只把“直播中”或“正在直播”文本作为辅助信号。

#### Scenario: 个人主页正在直播
- **WHEN** 主页结构化数据包含有效直播对象、稳定 `web_rid` 和当前 `roomIdStr`
- **THEN** 系统返回主页身份、标准化 `https://live.douyin.com/{web_rid}`、稳定 `web_rid` 和当前 `room_id`

#### Scenario: 结构化数据缺少入口但页面包含直播链接
- **WHEN** 主页没有可识别直播对象，但包含唯一且有效的 `live.douyin.com/{web_rid}` 直播入口
- **THEN** 系统使用该链接作为回退发现结果，并交给直播间 resolver 再次验证

#### Scenario: 仅出现直播文字
- **WHEN** 页面出现“直播中”或“正在直播”但没有可验证的直播入口或结构化房间身份
- **THEN** 系统不得猜测直播间 ID，并将结果视为暂时无法发现

### Requirement: 区分未开播和无法解析
系统 SHALL 在成功确认个人主页身份但没有直播入口时返回“当前未发现直播间”，并 SHALL 将网络失败、风控响应和页面结构变化作为可重试错误处理。

#### Scenario: 主播当前未开播
- **WHEN** 主页身份有效但结构化数据和链接均没有直播入口
- **THEN** 系统保留主页身份并返回未开播发现结果，不要求存在 `web_rid` 或 `room_id`

#### Scenario: 主页访问暂时失败
- **WHEN** 请求超时、网络不可用或公开页面返回临时错误
- **THEN** 系统返回脱敏的可重试错误，不把该主页判定为无效主播

#### Scenario: 页面结构不受支持
- **WHEN** 页面可访问但无法确认个人主页身份或解析受支持的初始化数据
- **THEN** 系统返回页面结构变更错误，且不从任意数字或文本猜测房间身份

### Requirement: 保持公开免登录访问边界
系统 SHALL 只通过普通公开 HTTP 页面发现个人主页直播入口，并 MUST NOT 使用浏览器自动化、账号 Cookie、登录状态、验证码或访问控制绕过。

#### Scenario: 页面要求登录或验证码
- **WHEN** 个人主页只能在登录、验证码或额外访问控制后继续访问
- **THEN** 系统停止本次发现并返回可重试或不支持错误，不尝试绕过限制

### Requirement: 避免泄露主页和直播敏感参数
系统 SHALL 在保存个人主页和直播入口时去除非必要查询参数，并 MUST NOT 在普通日志、事件或界面错误中输出签名直播流 URL、完整页面载荷或访问令牌。

#### Scenario: 主页直播链接包含跟踪参数
- **WHEN** 发现的直播链接带有 `room_id`、来源跟踪或其他查询参数
- **THEN** 系统只持久化 `https://live.douyin.com/{web_rid}` 和规范化身份字段

#### Scenario: 解析错误包含页面内容
- **WHEN** 底层解析失败信息包含 HTML、初始化载荷或签名地址
- **THEN** 对外错误只返回稳定错误码和脱敏中文摘要
