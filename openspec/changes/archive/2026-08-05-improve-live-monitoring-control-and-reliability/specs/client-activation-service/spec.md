## ADDED Requirements

### Requirement: 从服务端授权生命周期接收录制额度
系统 SHALL 在 AppStart 和心跳响应中兼容可选的 snake_case 整数字段 `max_screen_limit`，并 SHALL 将任一接口最新返回的合法正整数作为当前进程唯一的录制额度；在当前进程从未收到合法字段时额度 MUST 默认为 4，客户端 MUST NOT 使用本地设置替代服务端额度。

#### Scenario: AppStart 下发额度
- **WHEN** AppStart 成功响应的 `data.max_screen_limit` 为合法正整数
- **THEN** API 客户端按精确 snake_case 字段解析该值，授权生命周期立即把它应用到 Supervisor 并发布最新只读额度状态

#### Scenario: 心跳更新额度
- **WHEN** 后续成功心跳响应携带与当前值不同的合法 `max_screen_limit`
- **THEN** 客户端在保持既有授权续约行为的同时立即更新 Supervisor 额度并触发录制目标重算

#### Scenario: 两个接口中只有一个携带字段
- **WHEN** AppStart 或心跳之一已经返回合法额度，而另一个成功响应未包含 `max_screen_limit`
- **THEN** 客户端保留最近一次合法额度，不因可选字段缺失回退到 4

#### Scenario: 从未收到额度字段
- **WHEN** AppStart 返回 `data=null`，或所有成功的 AppStart 与心跳响应均未包含 `max_screen_limit`
- **THEN** 客户端继续使用默认额度 4，且启动、监听和授权续约均不被阻断

#### Scenario: 服务端额度无效
- **WHEN** 响应字段不是可表示的正整数
- **THEN** API 客户端不应用该值，保留最近合法额度或默认值 4，并只记录不含响应正文的脱敏诊断

#### Scenario: 额度请求暂时失败
- **WHEN** AppStart 或心跳发生超时、网络错误或服务端业务错误
- **THEN** 客户端保持当前额度并沿用既有重试或 best-effort 行为，不停止现有录制或改用本地设置
