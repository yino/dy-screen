## ADDED Requirements

### Requirement: 展示客户端激活门禁
系统 SHALL 在主窗口初始化时显示授权状态；没有有效激活时 SHALL 展示不可通过关闭按钮隐藏的激活弹窗，并 SHALL 锁定主导航和后台业务入口，直到激活成功或用户显式退出应用。

#### Scenario: 激活弹窗阻止使用
- **WHEN** 客户端启动且授权状态为 `missing`、`invalid` 或 `revoked`
- **THEN** 页面显示激活码输入、脱敏错误和重试入口，关闭按钮不可用，监控/视频库/AI 等功能不可操作

#### Scenario: 心跳状态展示
- **WHEN** 授权状态为 `active`、`checking` 或 `retrying`
- **THEN** 顶部或设置页显示当前状态和最近心跳时间，不显示激活码或令牌明文

#### Scenario: 强制下线后重新激活
- **WHEN** 心跳确认卡密停用、过期或设备已解绑
- **THEN** 客户端暂停后台任务、显示不可关闭的激活弹窗并保留脱敏原因

### Requirement: 保持启动授权生命周期
系统 SHALL 在启动时调用 AppStart、在激活后维护心跳并在关闭时停止授权任务；网络错误 MUST 以非敏感中文提示展示，并 SHALL 不阻塞应用退出。

#### Scenario: 启动登记失败
- **WHEN** AppStart 或 `app_open` 埋点请求失败
- **THEN** 主界面仍按本地授权状态继续显示，错误仅进入脱敏诊断

#### Scenario: 用户显式退出
- **WHEN** 用户确认退出客户端
- **THEN** 心跳和埋点任务先取消，应用在既有关闭期限内退出，不等待无限网络请求
