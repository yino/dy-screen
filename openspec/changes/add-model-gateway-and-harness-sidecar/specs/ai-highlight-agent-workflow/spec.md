## MODIFIED Requirements

### Requirement: 通过两个受限 Agent 角色分析精彩
系统 SHALL 使用客户端直连百炼所取得的同一冻结模型构建候选发现 Agent 和全局评分 Agent，并 SHALL 由 Rust 工作流决定调用阶段、顺序、重试、停止和持久化；Agent MUST NOT 获得写文件、执行命令、数据库 mutation、网络搜索或切片工具。

#### Scenario: 分析单个转写块
- **WHEN** Rust 工作流向候选发现 Agent 提交一个有界转写块和已选 Skills
- **THEN** Agent 只返回绑定稳定句段 ID 的结构化候选，不决定下一工作流阶段

#### Scenario: 全局复评候选
- **WHEN** 所有分块候选已经通过本地校验和去重
- **THEN** 全局评分 Agent 只接收压缩候选摘要并返回结构化评分和排序

#### Scenario: 两个 Agent 使用同一模型
- **WHEN** 一次精彩运行创建候选发现与全局评分两个阶段的调用
- **THEN** 两个阶段使用运行开始前冻结的同一模型与同一短期 token 提供方，且运行指纹记录实际模型
