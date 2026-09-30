## MODIFIED Requirements

### Requirement: 使用 VAD 筛选有效人声
系统 SHALL 在本地识别路径中使用 VAD 得到有效人声区间，并 SHALL 在检测区间首尾保留可配置的保护时间以降低音节被截断的风险。VAD MUST NOT 成为云端转写路径的前置门禁：本地 VAD 不可用或未检测到人声时，系统 SHALL 仍允许按云端路径处理完整来源音频。

#### Scenario: 音频包含人声和长静音
- **WHEN** 本地识别路径的 VAD 在音频中找到一个或多个人声区间
- **THEN** 系统只对带边界保护的人声范围执行本地 ASR，并把结果时间戳映射回源视频

#### Scenario: 没有检测到有效人声
- **WHEN** 本地识别路径的输入只包含静音、挂机或 VAD 未检测到人声
- **THEN** 系统把该输入标记为无有效人声，不生成虚假转写且继续处理其他输入

#### Scenario: 云端路径且本地 VAD 不可用
- **WHEN** 本地 VAD sidecar 或 VAD 模型缺失、诊断失败或检测返回空区间，而云端转写路径可用
- **THEN** 系统仍按云端路径处理完整来源音频，不因本地 VAD 结果把输入标记为无有效人声

### Requirement: 通过中立本地 AsrEngine 执行 ASR
系统 SHALL 通过不包含供应商专属字段的 `AsrEngine` 请求、结果、能力和错误契约执行识别；系统 SHALL 分别诊断百炼直连转写、可选本地识别和音频准备三项能力，MUST NOT 因缺少本地 `whisper.cpp` 模型而阻止云端识别。系统 SHALL 在云端路径可用时优先使用云端 ASR；本地 `whisper.cpp` 路径 SHALL 只在用户显式选择时使用，MUST NOT 作为云端失败的自动回退。选定的引擎 SHALL 返回其真实引擎与模型身份，MUST NOT 委托或冒用其他引擎的身份；识别配置身份校验 SHALL 依据实际执行引擎的身份。业务层不得读取任一供应商专属输出。

#### Scenario: 业务层提交统一请求
- **WHEN** 项目服务提交经过媒体层准备的音频资产、语言提示、热词和时间戳策略
- **THEN** 选定的 ASR engine 返回统一引擎身份、模型身份、语言、时长、句段时间范围、文本、可选置信信息和警告，且业务层不读取供应商专属输出

#### Scenario: 云端环境就绪
- **WHEN** heartbeat 表明账号已开通且额度可用、短期 token 可获取、百炼接口与网络条件均满足
- **THEN** 系统在云端 engine 执行识别并把结果转换为统一 ASR 契约，标记真实的云端 provider

#### Scenario: 云端环境未就绪
- **WHEN** 未开通、额度耗尽、短期 token 不可用、网络失败或上游任务失败
- **THEN** 系统记录脱敏的失败分类并明确失败、保留可重试状态，MUST NOT 自动改用本地 engine，也不把上游凭据或本地路径暴露给业务结果

#### Scenario: 本地环境就绪
- **WHEN** 用户显式选择本地识别路径，且当前平台的已安装资源包包含通过校验的 `whisper.cpp` sidecar、随包或下载的 ASR 模型和 VAD 模型
- **THEN** 系统在本机启动受控子进程完成中文或中英混合语音识别

#### Scenario: 本地环境未准备完成
- **WHEN** 云端路径不可用且资源缺失、资源下载未完成、sidecar、模型、权限、版本、完整性或最低资源诊断失败
- **THEN** 系统不启动 FFmpeg、VAD 或 ASR，保留任务状态并引导用户进入资源准备或修复流程

#### Scenario: 本地环境缺失
- **WHEN** 云端路径不可用且 sidecar、模型、权限、版本、完整性或最低资源诊断失败
- **THEN** 系统拒绝启动任务、保留草稿输入并显示重新检测或修复安装的中文错误

#### Scenario: 默认识别流程运行
- **WHEN** 用户启动 ASR 项目且 heartbeat 开通状态、额度与短期 token 条件满足
- **THEN** 系统执行云端识别；云端不可用时明确失败，MUST NOT 自动执行本地识别，并在失败路径中同样不得上传原始视频、热词或使用数据

#### Scenario: 缺少本地 ASR 模型但云端可用
- **WHEN** 本地 ASR 模型缺失或本地 ASR 诊断失败，而百炼直连路径可用
- **THEN** 系统执行云端识别，且不得把“缺少本地模型”作为跳过识别的理由

#### Scenario: 引擎身份真实
- **WHEN** 一次识别由云端 engine 完成
- **THEN** 返回的引擎与模型身份是云端 engine 的真实身份，MUST NOT 沿用在其他路径执行过的 engine 身份

### Requirement: 安全复用相同识别产物
系统 SHALL 仅在源指纹与识别配置指纹均一致且产物已完整发布时复用 ASR 结果。识别配置指纹 SHALL 包含实际 `provider`、模型、模型版本、分块策略、VAD 参数、热词与规范化版本；任一字段变化时 SHALL 创建新的识别产物。

#### Scenario: 相同视频再次分析
- **WHEN** 另一个项目引用相同源版本并使用相同的 `provider`、模型版本、分块策略、VAD、热词和规范化配置
- **THEN** 系统复用已完成转写并更新最后使用时间，不再次执行 FFmpeg 或 ASR

#### Scenario: 模型或源发生变化
- **WHEN** 视频内容标识、`provider`、模型版本、分块策略、VAD 参数、热词或规范化版本发生变化
- **THEN** 系统不得复用旧产物并创建新的受控识别尝试

#### Scenario: 应用在发布结果前退出
- **WHEN** 识别片段尚未事务性发布时任务中断
- **THEN** 系统不得把半成品标记为可复用，并在恢复时重新排队或等待用户重试

### Requirement: 有界取消、恢复和错误隔离
系统 SHALL 支持取消模型与 FFmpeg 子进程以及按上游能力取消云端任务、在应用重启后恢复未完成任务，并 SHALL 让单输入错误与其他输入隔离。用户取消 MUST 直接终止且不回退本地；授权与额度错误 MUST 显式报告。

#### Scenario: 用户取消识别
- **WHEN** 用户取消运行项目
- **THEN** 系统在有界时间内终止本地子进程或按上游能力取消云端任务、清理临时输出并保留已完整发布的可复用产物，MUST NOT 回退本地，也 MUST NOT 把取消报告为云端故障

#### Scenario: 应用异常退出后重启
- **WHEN** 启动时发现处于运行阶段但没有活动进程的输入
- **THEN** 系统清理临时数据、按已持久化的 `requestId` 与上游 `task_id` 核对任务状态，并把输入恢复为可重试状态，不把它误报为完成

#### Scenario: 单个输入处理失败
- **WHEN** FFprobe、FFmpeg、VAD、云端任务或本地 ASR 对某个输入返回错误
- **THEN** 系统保存脱敏错误、释放资源并继续项目中的下一个有效输入

#### Scenario: 取消时上游取消失败
- **WHEN** 本地已取消但上游取消请求失败或上游不支持取消
- **THEN** 系统向前端说明该任务仍可能按实际用量结算，不自动重新提交付费请求

### Requirement: 跨平台定位随包 sidecar
系统 SHALL 在 macOS arm64、macOS Intel x86_64 与 Windows x64 资源包中提供平台匹配的 `whisper.cpp`、FFmpeg 和 FFprobe sidecar，并 SHALL 由后端从经过校验的应用资源目录自动定位，MUST NOT 要求普通用户配置任意可执行路径。

#### Scenario: 在 Apple Silicon Mac 运行
- **WHEN** 应用在受支持的 macOS arm64 设备上启动环境诊断
- **THEN** 系统定位已签名且哈希匹配的 arm64 Metal sidecar，并拒绝架构或版本不匹配的二进制

#### Scenario: 在 Intel Mac 运行
- **WHEN** 应用在受支持的 macOS Intel x86_64 设备上启动环境诊断
- **THEN** 系统定位已签名且哈希匹配的 x86_64 CPU sidecar，并拒绝架构或版本不匹配的二进制；MUST NOT 依赖 Rosetta 转译来满足架构校验

#### Scenario: 在 Windows x64 运行
- **WHEN** 应用在受支持的 Windows x64 设备上启动环境诊断
- **THEN** 系统定位已签名且哈希匹配的 x64 CPU sidecar，并使用支持 Unicode 与空格路径的参数调用方式

#### Scenario: 在未支持平台运行
- **WHEN** 当前系统是 Windows ARM64 或其他未声明支持的平台
- **THEN** 系统将资源准备标记为不受支持且不尝试运行不匹配的 sidecar

### Requirement: 随安装包提供默认模型并严格校验
系统 SHALL 在 macOS arm64、macOS Intel x86_64 与 Windows x64 的安装包或受信 Runtime Resource Pack 中提供相同版本的 small 多语言量化模型、兼容 VAD 模型和资源 manifest，并 MUST NOT 在第一版支持任意模型路径、未经签名的资源导入或未授权的模型切换。

#### Scenario: 随包或下载资源完整
- **WHEN** 资源门禁确认模型文件存在、声明大小与 SHA-256 匹配且资源版本兼容
- **THEN** 系统允许继续执行内存、磁盘和媒体检查

#### Scenario: 随包资源完整
- **WHEN** 任务前检查确认模型文件存在、声明大小与 SHA-256 匹配且资源版本兼容
- **THEN** 系统允许继续执行内存、磁盘和媒体检查

#### Scenario: 在 Intel Mac 校验资源
- **WHEN** 应用在 macOS Intel x86_64 设备上校验随包资源
- **THEN** 系统按该架构声明的文件大小与 SHA-256 逐项校验，任一不符即拒绝启动并提示修复安装

#### Scenario: 首次启动资源缺失
- **WHEN** 模型或 VAD 资源不在安装包且本机没有完整已安装版本
- **THEN** 系统要求用户从固定 HTTPS 渠道下载并校验资源，下载完成前不得开始 ASR 或其他主功能

#### Scenario: 资源缺失或损坏
- **WHEN** 模型文件不存在、大小不符、SHA-256 不匹配、签名失败或版本不兼容
- **THEN** 系统不得启动 FFmpeg、VAD 或 ASR，并显示资源下载、修复或退出入口

#### Scenario: 随包模型缺失或损坏
- **WHEN** 模型文件不存在、大小不符、SHA-256 不匹配或版本不兼容
- **THEN** 系统不得启动 FFmpeg、VAD 或 ASR，并提示用户修复或重新安装当前应用版本

#### Scenario: 设备资源不足
- **WHEN** 8 GB 基线检查发现当前可用内存或磁盘空间不足以安全执行默认 small 模型
- **THEN** 系统保持资源或任务等待，且不得以云端服务作为自动回退

## ADDED Requirements

### Requirement: 云端路径失败不自动回退本地
系统 SHALL 在百炼直连路径失败时明确失败并返回可区分原因，MUST NOT 自动改用本地 `whisper.cpp` 完成同一次处理。本地识别路径 SHALL 只由用户显式选择触发，两条路径的产物身份与失败原因 MUST 可区分。

#### Scenario: 云端不可用且本地资源可用
- **WHEN** 百炼直连路径因网络、额度或上游错误失败，而本地模型与 sidecar 均可用
- **THEN** 系统报告云端失败的可区分原因并保留可重试状态，MUST NOT 启动本地引擎完成本次处理

#### Scenario: 产物身份可区分
- **WHEN** 同一输入分别经云端与本地路径执行
- **THEN** 两条路径各自产出独立产物，`provider` 与模型身份均为真实身份且可区分，互不复用
