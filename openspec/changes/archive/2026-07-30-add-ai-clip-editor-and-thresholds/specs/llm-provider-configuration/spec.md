## MODIFIED Requirements

### Requirement: 安全配置 DeepSeek Provider
系统 SHALL 只允许配置官方 DeepSeek Provider、用户自定义模型 ID、受控请求参数以及非敏感的合格/优秀精彩阈值，并 MUST NOT 在第一版接受任意 Base URL、代理地址或浏览器端 Provider 配置。合格与优秀阈值 MUST 为 0–100 的整数，且优秀阈值 MUST 不低于合格阈值。

#### Scenario: 保存 DeepSeek 设置
- **WHEN** 用户提交有效模型 ID、API Key、合格阈值和优秀阈值
- **THEN** 系统把模型、非敏感参数和阈值保存到 SQLite，把 API Key 保存到当前操作系统凭据库，并只向界面返回 `keyConfigured=true`

#### Scenario: 保存无效阈值
- **WHEN** 用户提交超出 0–100 范围的阈值或优秀阈值低于合格阈值
- **THEN** 后端拒绝保存并返回中文校验说明，且保留已有有效设置

#### Scenario: 提交任意 Provider 地址
- **WHEN** 前端尝试提交自定义 Base URL 或非 DeepSeek Provider
- **THEN** 后端拒绝该字段且不发起网络请求
