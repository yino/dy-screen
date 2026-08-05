## ADDED Requirements

### Requirement: 从授权心跳发布转场素材目录信号
系统 SHALL 在成功心跳响应中兼容可选的 camelCase 对象 `transitionMaterials`，其中包含正整数 `catalogVersion` 和语义版本 `minimumAppVersion`。授权生命周期 MUST 只把合法值发布给独立素材目录协调器；字段缺失、字段非法、版本门禁或后续素材同步失败 MUST NOT 改变心跳成功结论、令牌续约、激活状态或授权依赖后台任务。

#### Scenario: 心跳发布新的素材目录版本
- **WHEN** 成功心跳携带合法 `transitionMaterials` 且 `catalogVersion` 与本地目录版本不同
- **THEN** 授权生命周期保持正常续约，并向单实例素材协调器发布去重后的版本检查信号

#### Scenario: 心跳重复相同目录版本
- **WHEN** 连续成功心跳携带相同 `catalogVersion` 和 `minimumAppVersion`
- **THEN** 素材协调器不得为同一版本并发或重复启动全量同步，但可更新最后发现时间

#### Scenario: 心跳没有素材字段
- **WHEN** 成功心跳未包含 `transitionMaterials`
- **THEN** 客户端保留本地目录和素材同步状态，授权续约及监听任务继续运行

#### Scenario: 素材字段非法
- **WHEN** `catalogVersion` 不是正整数或 `minimumAppVersion` 不是可解析的语义版本
- **THEN** 客户端忽略该素材信号、记录不含响应正文的脱敏诊断，并继续按成功心跳处理授权

#### Scenario: 客户端低于最低版本
- **WHEN** 当前客户端版本低于心跳发布的 `minimumAppVersion`
- **THEN** 素材协调器进入 `upgrade_required`、停止应用新目录并展示升级要求，但不得撤销授权或删除上一可用目录

#### Scenario: 素材同步失败
- **WHEN** 心跳成功发布版本后，独立目录请求或 SQLite 事务失败
- **THEN** 授权状态、令牌、监听、录制和心跳周期保持不变，只有素材同步状态进入可重试失败
