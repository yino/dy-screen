# 客户端激活服务规格

## Purpose

定义桌面客户端与授权运营服务端之间的统一 API、激活门禁、心跳续约、强制下线、启动登记和安全埋点边界。

## Requirements

### Requirement: 统一封装客户端授权 API
系统 SHALL 在 Rust 中提供单一 API 客户端封装，集中读取 `DY_SCREEN_API_BASE_URL`、规范化 `/api/` 前缀、处理 HTTP 恒 200 的业务 `code`、超时和脱敏错误；激活、心跳、AppStart 和埋点业务代码 MUST NOT 直接创建 HTTP 客户端。客户端 SHALL 预留请求/响应加解密入口，本期保持 JSON 明文。

#### Scenario: 配置服务端地址
- **WHEN** 启动时设置 `DY_SCREEN_API_BASE_URL=http://localhost/api/`
- **THEN** 所有客户端请求都发送到该地址下的 `/client/...` 路径，并且不在日志中输出完整查询参数

#### Scenario: 服务端返回业务错误
- **WHEN** HTTP 状态为 200 但响应 `code` 不是成功码
- **THEN** API 客户端返回包含稳定错误分类和中文消息的错误，不把原始响应正文传播到前端

#### Scenario: API 请求编解码
- **WHEN** 未来启用请求或响应加密
- **THEN** 只需替换统一客户端的 codec 实现，激活、心跳、AppStart 和埋点调用方无需修改

### Requirement: 激活前阻止桌面业务
系统 SHALL 将设备 ID和激活码状态持久化到 SQLite，并 SHALL 在没有有效激活状态时显示不可关闭的激活入口；在激活成功前 MUST NOT 恢复监听、录制、视频处理或其他依赖授权的后台任务。

#### Scenario: 首次启动没有激活码
- **WHEN** 本地没有激活记录或记录不完整
- **THEN** 客户端显示激活弹窗，激活码为空时不能关闭弹窗或进入主功能

#### Scenario: 激活成功
- **WHEN** 用户提交有效激活码且服务端返回令牌和有效期
- **THEN** 客户端保存脱敏所需的本地状态、关闭弹窗、发布 `active` 状态并恢复已启用监听

#### Scenario: 激活失败
- **WHEN** 服务端返回不存在、过期、停用或已绑定其他设备
- **THEN** 弹窗保持打开，显示中文脱敏原因，既有用户输入不丢失且不启动后台任务

### Requirement: 维护心跳并处理强制下线
系统 SHALL 在激活成功后按有界周期发送心跳，并 SHALL 处理服务端下发的 `serverTime`、令牌续签和 `revoked/reason`；收到 `disabled`、`expired` 或 `unbound` 时 MUST 停止授权依赖任务、清理令牌并要求重新激活。

#### Scenario: 心跳续约成功
- **WHEN** 心跳返回 `revoked=false` 和 `reason=ok`
- **THEN** 客户端更新令牌/有效期/服务端时间偏移，保持监听运行并刷新授权状态

#### Scenario: 服务端强制下线
- **WHEN** 心跳返回 `revoked=true` 或状态为 `DISABLED`、`EXPIRED`、`INACTIVE`
- **THEN** 客户端幂等暂停后台任务、清除不可继续使用的令牌、发布前端状态并重新显示激活入口

#### Scenario: 暂时网络失败
- **WHEN** 心跳请求因网络或服务端不可达失败
- **THEN** 客户端保留本地激活记录，按有界退避重试并展示“正在重试”状态，不泄露请求地址或令牌

### Requirement: 调用 AppStart 与安全埋点
系统 SHALL 在启动时 best-effort 调用免鉴权 AppStart，并 SHALL 上报白名单 `app_open`、`feature_use`、`export`、`ai_call`、`error` 事件；埋点 MUST 仅包含允许属性，不得包含媒体内容、ASR 文本、Cookie、激活码或未经脱敏的本地路径。

#### Scenario: AppStart 无可用版本
- **WHEN** AppStart 返回成功且 `data=null`
- **THEN** 客户端继续启动并将“无可用更新”作为非阻断结果处理

#### Scenario: 埋点服务不可用
- **WHEN** 埋点请求失败或服务端不可达
- **THEN** 客户端丢弃或延迟有限事件队列，不阻塞激活、监听、录制和退出

#### Scenario: 埋点属性超出白名单
- **WHEN** 调用方传入不在 `feature/action/result/duration/platform/count/source` 中的属性
- **THEN** API 客户端在发送前剔除该属性，并保持事件本身可 best-effort 上报
