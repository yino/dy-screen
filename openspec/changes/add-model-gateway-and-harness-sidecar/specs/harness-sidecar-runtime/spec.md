## Purpose

为桌面客户端提供受控的进程外 Harness 运行时与模型调用能力：由 Rust 作为任务状态与工程数据的权威来源，sidecar 只按固定版本的 Profile 与 Skill 执行受限分析；全部模型调用由客户端 Rust 侧持 `ai/llm-ak` 的短期 token 直连阿里云百炼，不引入服务端网关，也不新增服务端接口。

## ADDED Requirements

### Requirement: 以进程外 sidecar 承载受控 Harness
系统 SHALL 由 Rust 进程作为任务状态与工程数据的权威来源，启动并监管独立子进程中的 DeepSeek Harness，并 SHALL 通过其进程外 JSON-RPC 协议发送冻结的 ASR 输入、接收结构化结果。客户端的 Harness 聊天记录 MUST NOT 作为业务任务的唯一状态来源。

#### Scenario: 启动并完成一次分析
- **WHEN** Rust 工作流已冻结本次分析输入并启动 sidecar
- **THEN** Rust 通过 JSON-RPC 发送冻结输入，接收结构化结果并把结果持久化到本地业务存储

#### Scenario: sidecar 异常退出
- **WHEN** sidecar 进程在请求进行中退出或不再响应
- **THEN** Rust 判定该次调用失败、释放资源并按已持久化的阶段尝试恢复，不把未完成的调用标记为成功

#### Scenario: 状态以 Rust 为准
- **WHEN** sidecar 的运行记录与 Rust 持久化的任务状态不一致
- **THEN** 系统以 Rust 持久化的状态为准决定阶段、重试与恢复

### Requirement: 使用应用专用 Profile 限制模型路由与工具
系统 SHALL 使用随应用打包的应用专用 Profile，其中只注册模型路由、Skill 与明确授权的业务工具。shell 执行、任意文件编辑、浏览器浏览与任意网络访问工具 MUST NOT 进入生产 Profile；系统 MUST NOT 依赖用户全局安装的 Agent 配置或用户目录下的 Profile。

#### Scenario: 启动生产 Profile
- **WHEN** Rust 以生产 Profile 启动 sidecar
- **THEN** 可用的工具集合只包含模型路由、已打包 Skill 与明确授权的业务工具

#### Scenario: 尝试使用通用工具
- **WHEN** 分析过程尝试调用 shell、任意文件编辑、浏览器浏览或任意网络访问
- **THEN** 该调用被拒绝且不影响已完成的其他阶段结果

#### Scenario: 用户目录存在同名配置
- **WHEN** 用户目录中存在同名 Agent 配置或 Profile
- **THEN** 系统忽略该配置，只使用随应用打包的版本

### Requirement: 随应用打包固定版本运行时与 Skill
系统 SHALL 随应用打包固定版本的 Harness 运行时、Profile 与 Skill，并 SHALL 把它们纳入运行资源的组件清单与完整性校验。版本升级 SHALL 固定目标版本并 SHALL 通过 macOS arm64、macOS Intel x86_64 与 Windows x64 的协议与恢复测试。

#### Scenario: 运行时与 Skill 版本固定
- **WHEN** 应用启动 AI 能力
- **THEN** 系统使用随包清单声明的运行时、Profile 与 Skill 版本，不从网络获取未声明的组件

#### Scenario: 组件缺失或校验失败
- **WHEN** 运行时、Profile 或 Skill 缺失、哈希不匹配或架构不符
- **THEN** 系统阻止启动 sidecar、显示组件级原因，并保留录制与既有浏览能力

#### Scenario: 升级到新版本运行时
- **WHEN** 发行包声明了新的 Harness 运行时版本
- **THEN** 系统在 macOS arm64、macOS Intel x86_64 与 Windows x64 上分别通过协议与恢复测试后才允许发布

### Requirement: 把 ASR 文本视为待分析数据
系统 SHALL 把 ASR 文本、候选理由与用户修正文案始终视为待分析数据，MUST NOT 让它们成为 Harness 的系统指令、工具配置或权限来源。

#### Scenario: 转写文本包含指令样式内容
- **WHEN** ASR 文本包含形似系统指令、工具调用或配置的内容
- **THEN** Harness 把它作为普通数据送交分析，不改变工具集合、模型路由或工作流阶段

#### Scenario: 主播标签包含未知值
- **WHEN** 标签快照包含未内置的取值
- **THEN** 系统把它作为明确分隔的用户数据处理，不作为系统指令

### Requirement: 使用客户端短期 token 直连百炼调用模型
系统 SHALL 由客户端 Rust 侧持有 `GET /api/client/ai/llm-ak` 签发的短期 token，并用它直连阿里云百炼完成模型调用。系统 MUST NOT 引入服务端模型网关或网关令牌，MUST NOT 为此新增服务端接口。短期 token MUST 只在 Rust 内存中持有；Harness 与 WebView MUST NOT 接触该 token 或上游原始响应；短期 token 过期时 MUST 按 `expireAt` 刷新，而不是把过期报告为网络故障。

#### Scenario: 取得短期 token 并调用模型
- **WHEN** Rust 完成一次运行前的配置解析并需要调用模型
- **THEN** Rust 使用内存中的有效短期 token 直连百炼，调用结束后不把 token 写入任何持久化存储

#### Scenario: 短期 token 过期或额度耗尽
- **WHEN** 短期 token 已过期且刷新失败，或 heartbeat 响应表明账号未开通、对应额度已耗尽
- **THEN** 系统停止该任务、向界面显示可区分的授权或额度原因，且不把该结果描述为网络故障

#### Scenario: 客户端不持有长期密钥
- **WHEN** 检查客户端持久化数据、设置与凭据存储
- **THEN** 不存在可供调用上游模型的长期密钥，也不存在可由前端读取的短期 token 明文

### Requirement: 把图片与视频生成作为受控业务工具
系统 SHALL 把媒体生成暴露为受控业务工具：Harness 只能提交结构化请求并取得业务任务 ID，MUST NOT 持有短期 token 或图片/视频 Provider 凭据，也 MUST NOT 取得任意下载地址。实际提交由 Rust 侧完成。

#### Scenario: 提交结构化生成请求
- **WHEN** Skill 需要生成参考图或视频并提交符合 schema 的结构化请求
- **THEN** 系统返回业务任务 ID 与状态，不返回短期 token、Provider 凭据或任意下载地址

#### Scenario: Skill 尝试提交任意地址或原始参数
- **WHEN** 生成请求包含任意 URL、本地路径、Provider 原始参数或滤镜与命令行片段
- **THEN** 系统拒绝该请求且不产生付费调用

### Requirement: 由 Rust 侧校验生成请求与本地门禁
系统 SHALL 在 Rust 侧校验生成请求的时间范围、互斥类型、最大尝试次数与本地冻结的额度上限。开通与额度结论 MUST 取自 heartbeat 响应，客户端 MUST NOT 以本地设置替代该结论；超限请求 MUST NOT 发出。

#### Scenario: 时间范围或互斥类型非法
- **WHEN** 生成请求的时间范围越界、与已验证的 ASR 句段不符，或同一高光同时请求两种互斥表现形式
- **THEN** Rust 拒绝该请求并记录脱敏原因

#### Scenario: 调用次数超限
- **WHEN** 某一高光的生成尝试次数超过冻结的最大尝试次数
- **THEN** 系统停止该高光的生成并转入既定的失败处理

#### Scenario: 本地门禁拒绝
- **WHEN** 任务级动画数量、累计秒数或金额上限任一被触发
- **THEN** 客户端停止提交，不通过绕过门禁的方式重新提交

### Requirement: 有界取消、资源限制与回收
系统 SHALL 为 Harness 与渲染运行时使用独立子进程，以便分别限制内存与执行时间、支持取消和回收。取消 MUST 终止受控子进程、清理未发布的本地临时文件，并保留可复用的成功产物。

#### Scenario: 用户取消分析
- **WHEN** 用户在分析进行中取消
- **THEN** 系统在有界时间内终止 sidecar 调用、清理未发布临时文件，并保留此前已完整发布的阶段结果

#### Scenario: 超过资源上限
- **WHEN** sidecar 或渲染进程超过冻结的内存或执行时间上限
- **THEN** 系统终止该子进程、把该次调用标记为失败并释放资源，不影响其他任务

#### Scenario: 迟到结果
- **WHEN** 取消后收到迟到的 sidecar 结果
- **THEN** 系统忽略该结果，不覆盖人工编辑或其他已发布内容

### Requirement: 仅服务所在设备
客户端 Harness SHALL 只服务当前设备。系统 MUST NOT 为桌面任务在服务端常驻 Agent，也 MUST NOT 把本设备的 Harness 暴露为可供其他设备调用的服务。

#### Scenario: 多设备使用
- **WHEN** 同一账号在多台设备上使用客户端
- **THEN** 每台设备各自启动并监管本地 sidecar，任务状态与结果按设备与任务隔离

#### Scenario: 尝试远程复用
- **WHEN** 其他进程或设备尝试调用本机 sidecar
- **THEN** 系统拒绝该调用，sidecar 只接受由 Rust 监管进程发起的请求
