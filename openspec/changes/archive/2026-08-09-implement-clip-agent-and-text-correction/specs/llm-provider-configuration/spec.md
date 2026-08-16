## MODIFIED Requirements

### Requirement: 安全配置 DeepSeek Provider
系统 SHALL 只允许配置官方 DeepSeek Provider、用户自定义模型 ID、受控请求参数、非敏感的合格/优秀精彩阈值和独立的转场自动应用阈值，并 MUST NOT 接受任意 Base URL、代理地址或浏览器端 Provider 配置。合格与优秀阈值 MUST 为 0–100 的整数且优秀阈值不得低于合格阈值；转场自动应用阈值 MUST 为 0–10 的整数且默认值为 8。

#### Scenario: 保存 DeepSeek 设置
- **WHEN** 用户提交有效模型 ID、API Key、精彩阈值和转场自动应用阈值
- **THEN** 系统把模型、非敏感参数和阈值保存到 SQLite，把 API Key 保存到当前操作系统凭据库，并只向界面返回 `keyConfigured=true`

#### Scenario: 旧设置没有转场阈值
- **WHEN** 升级后的客户端读取尚未包含转场自动应用阈值的既有设置
- **THEN** 系统使用并持久化默认 8 分，且不改变既有模型、API Key 或精彩阈值

#### Scenario: 保存无效精彩阈值
- **WHEN** 用户提交超出 0–100 范围的精彩阈值或优秀阈值低于合格阈值
- **THEN** 后端拒绝保存并返回中文校验说明，且保留已有有效设置

#### Scenario: 保存无效转场阈值
- **WHEN** 用户提交非整数或超出 0–10 范围的转场自动应用阈值
- **THEN** 后端拒绝保存并返回中文校验说明，且保留已有有效设置

#### Scenario: 提交任意 Provider 地址
- **WHEN** 前端尝试提交自定义 Base URL 或非 DeepSeek Provider
- **THEN** 后端拒绝该字段且不发起网络请求

## ADDED Requirements

### Requirement: 为每次转场 Agent 运行冻结阈值
系统 SHALL 在一键转场 Agent 运行创建时从本地 Provider 设置读取 `transitionAutoApplyScore` 并保存到运行快照。运行创建后的设置修改 MUST 只影响后续运行，模型响应 MUST NOT 覆盖或建议改变已冻结阈值。

#### Scenario: 使用默认转场阈值
- **WHEN** 用户从未修改转场自动应用分数并启动一键 Agent
- **THEN** 新运行冻结阈值为 8 分

#### Scenario: 使用自定义转场阈值
- **WHEN** 用户保存 0–10 范围内的整数阈值后启动新运行
- **THEN** 新运行冻结该自定义值并按它判断自动应用
