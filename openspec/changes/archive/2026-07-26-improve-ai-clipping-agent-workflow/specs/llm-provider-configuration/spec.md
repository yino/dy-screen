## ADDED Requirements

### Requirement: 安全配置 DeepSeek Provider
系统 SHALL 只允许配置官方 DeepSeek Provider、用户自定义模型 ID 和受控请求参数，并 MUST NOT 在第一版接受任意 Base URL、代理地址或浏览器端 Provider 配置。

#### Scenario: 保存 DeepSeek 设置
- **WHEN** 用户提交有效模型 ID 和 API Key
- **THEN** 系统把模型及非敏感参数保存到 SQLite，把 API Key 保存到当前操作系统凭据库，并只向界面返回 `keyConfigured=true`

#### Scenario: 提交任意 Provider 地址
- **WHEN** 前端尝试提交自定义 Base URL 或非 DeepSeek Provider
- **THEN** 后端拒绝该字段且不发起网络请求

### Requirement: API Key 不进入应用数据和日志
系统 MUST NOT 把 DeepSeek API Key 写入 SQLite、设置 JSON、日志、事件、错误、剪贴板恢复数据或浏览器演示存储，并 SHALL 允许用户显式替换和清除系统凭据。

#### Scenario: 查询 Provider 设置
- **WHEN** 界面读取当前 DeepSeek 设置
- **THEN** 系统只返回模型、非敏感参数、更新时间和是否已配置 Key，不返回 Key 内容或可推断片段

#### Scenario: 清除凭据
- **WHEN** 用户确认清除 DeepSeek API Key
- **THEN** 系统从操作系统凭据库删除 Key、保留非敏感模型设置并禁止新的精彩分析

### Requirement: 提供有界连接诊断
系统 SHALL 使用当前凭据和模型执行不包含用户转写的最小连接诊断，并 SHALL 返回脱敏分类、模型和可操作中文信息。

#### Scenario: 连接与模型可用
- **WHEN** 用户点击测试连接且 DeepSeek 接受当前 Key 和模型
- **THEN** 系统显示连接成功且不创建精彩运行或保存响应正文

#### Scenario: 凭据无效或被限流
- **WHEN** DeepSeek 返回鉴权失败、模型不存在、限流或暂时网络错误
- **THEN** 系统返回稳定错误码和中文说明，不记录响应正文、请求头或 Key

### Requirement: 仅在用户授权后发送最小文本
系统 SHALL 只在用户针对项目显式启动精彩分析后发送必要的规范化转写、稳定句段 ID、相对时间、标签和分析目标，并 MUST NOT 发送视频、音频、本地路径、签名流地址或数据库内容。

#### Scenario: 查看分析预估
- **WHEN** 用户打开已完成 ASR 的精彩分析步骤
- **THEN** 系统显示将发送的数据类型、句段数、文本量、预计批次和模型，但不发起 DeepSeek 请求

#### Scenario: 用户确认开始分析
- **WHEN** 用户确认文本将发送到 DeepSeek并点击开始
- **THEN** 系统冻结本次最小请求上下文并创建可取消的精彩运行

### Requirement: 限制 Provider 重试与诊断数据
系统 SHALL 为每次请求设置超时和取消边界，只对限流或暂时网络错误最多自动重试两次，并 SHALL 仅记录脱敏 request ID、阶段、模型、耗时、token 用量、分类和重试次数。

#### Scenario: 请求暂时失败后恢复
- **WHEN** DeepSeek 第一次请求返回可重试错误且第二次成功
- **THEN** 系统保存一次重试计数和最终结构化结果，不重复已经完成的其他工作流阶段

#### Scenario: 用户取消网络分析
- **WHEN** 用户取消精彩运行
- **THEN** 系统停止后续调用、忽略迟到响应并保留此前完整发布的阶段结果
