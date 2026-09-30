## MODIFIED Requirements

### Requirement: 通过本地 Rust 分批调用现有 LLM Provider
系统 SHALL 由本地 Tauri/Rust 层持 `ai/llm-ak` 的短期 token 直连阿里云百炼，把完整字幕边界上的有界文本数组分批发送给受限纠错角色，且 MUST NOT 要求用户预先配置上游 API Key。提示词 MUST 只允许修正错别字、同音错词、明显 ASR 识别错误和标点，MUST 要求保持原意、语气、专名、数字、数组长度和顺序；系统 MUST NOT 发送视频、音频、本地路径、激活信息、上游凭据、短期 token 或其他数据库内容。

#### Scenario: 文本超过单次预算
- **WHEN** 可纠错字幕超过当前字符或 token 预算
- **THEN** Rust 在字幕边界生成多个批次、依次使用同一冻结模型调用，并保持全局字幕映射稳定

#### Scenario: 用户启动纠错
- **WHEN** 已激活用户点击一键文本纠错
- **THEN** 本地 Rust 取得或复用有效期内的短期 token 后发起受限请求并发布批次进度，WebView 不接触短期 token 或原始响应

#### Scenario: 敏感字段不进入请求
- **WHEN** Rust 构建任一纠错批次
- **THEN** 请求业务数据只包含必要文本数组和受控指令，不包含媒体、路径、凭据或任意前端附加字段

#### Scenario: 未开通或额度耗尽
- **WHEN** heartbeat 响应表明账号未开通 AI 服务，或纠错额度已耗尽
- **THEN** Rust 停止该运行、保留全部工程字幕不变并显示可区分的授权或额度原因，不把它描述为网络故障
