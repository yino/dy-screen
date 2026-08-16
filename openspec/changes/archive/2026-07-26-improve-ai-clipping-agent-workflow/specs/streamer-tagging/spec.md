## MODIFIED Requirements

### Requirement: 提供未来切片使用的结构化标签上下文
系统 SHALL 提供只读、确定性排序且可序列化的 `StreamerPromptContext`，包含主播内部 ID、主播名称以及标签名称、可选 Prompt 指导和优先级；该上下文 SHALL 仅在用户显式启动精彩分析后作为 Agent Skill 选择和评分输入，MUST NOT 因保存标签、录制完成或 ASR 完成自动触发模型调用。

#### Scenario: 读取带货搞笑主播的上下文
- **WHEN** 主播标签依次为“带货”和“搞笑”且用户启动关联项目的精彩分析
- **THEN** 系统冻结包含两个标签及优先级的快照，选择通用、带货和搞笑 Skills，并重点评估商品表达和搞笑包袱相关句段

#### Scenario: 主播没有标签
- **WHEN** 用户启动没有主播标签的项目精彩分析
- **THEN** 系统使用空主播标签数组和通用 Skill，而不是错误、默认推断标签或额外主页请求

#### Scenario: 标签内容进入 Agent Prompt
- **WHEN** 精彩工作流使用 `StreamerPromptContext` 构建 Agent 输入
- **THEN** 标签内容必须作为明确分隔的用户配置数据处理，不得覆盖系统安全规则、Tool 权限、Skill 版本或输出格式

### Requirement: 保持标签完全本地和用户可控
系统 SHALL 只在本地 SQLite 和本地界面中创建、修改和推断主播标签，MUST NOT 自动从主页、转写文本或外部模型推断标签；只有用户显式授权项目精彩分析时，系统 MAY 把本次冻结标签名称和 Prompt 指导随最小转写上下文发送给已配置的 DeepSeek Provider。

#### Scenario: 新增主播但用户没有填写标签
- **WHEN** 用户添加主播且没有配置标签
- **THEN** 系统保存空标签集合，不发起额外网络请求或模型调用

#### Scenario: 编辑标签
- **WHEN** 用户手动修改标签名称、指导或顺序
- **THEN** 系统只保存用户明确提交的结果，不自动改写、补充或发送标签

#### Scenario: 用户启动精彩分析
- **WHEN** 用户已查看发送范围并确认对关联项目执行 DeepSeek 精彩分析
- **THEN** 系统只发送本次冻结的标签数据且记录授权时间，不上传主播主页、视频、音频或其他主播记录
