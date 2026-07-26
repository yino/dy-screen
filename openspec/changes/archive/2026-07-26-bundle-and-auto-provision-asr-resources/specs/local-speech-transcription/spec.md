## MODIFIED Requirements

### Requirement: 通过中立本地 AsrEngine 执行 ASR
系统 SHALL 通过不包含供应商专属字段的 `AsrEngine` 请求、结果、能力和错误契约执行识别，第一版 SHALL 使用 Rust `WhisperCppEngine` Adapter 调用经过资源管理器验证的 `whisper.cpp` 多语言模型，并 MUST NOT 在识别过程中调用云端服务或发起媒体上传。

#### Scenario: 业务层提交统一请求
- **WHEN** 项目服务提交经过媒体层准备的音频资产、语言提示、热词和时间戳策略
- **THEN** `AsrEngine` 返回统一引擎身份、模型身份、语言、时长、句段时间范围、文本、可选置信信息和警告，且业务层不读取 `whisper.cpp` 专属输出

#### Scenario: 本地环境就绪
- **WHEN** 当前平台的已安装资源包包含通过校验的 `whisper.cpp` sidecar、随包或下载的 ASR 模型和 VAD 模型
- **THEN** 系统在本机启动受控子进程完成中文或中英混合语音识别

#### Scenario: 本地环境未准备完成
- **WHEN** 资源缺失、资源下载未完成、sidecar、模型、权限、版本、完整性或最低资源诊断失败
- **THEN** 系统不启动 FFmpeg、VAD 或 ASR，保留任务状态并引导用户进入资源准备或修复流程

#### Scenario: 本地环境缺失
- **WHEN** sidecar、模型、权限、版本、完整性或最低资源诊断失败
- **THEN** 系统拒绝启动任务、保留草稿输入并显示重新检测或修复安装的中文错误

#### Scenario: 默认识别流程运行
- **WHEN** 用户启动本地 ASR 项目且资源已经通过资源门禁
- **THEN** 系统不得上传视频、音频、热词、转写或使用数据，也不得为识别过程发起网络请求

### Requirement: 跨平台定位随包 sidecar
系统 SHALL 在 macOS arm64 与 Windows x64 资源包中提供平台匹配的 `whisper.cpp`、FFmpeg 和 FFprobe sidecar，并 SHALL 由后端从经过校验的应用资源目录自动定位，MUST NOT 要求普通用户配置任意可执行路径。

#### Scenario: 在 Apple Silicon Mac 运行
- **WHEN** 应用在受支持的 macOS arm64 设备上启动环境诊断
- **THEN** 系统定位已签名且哈希匹配的 arm64 Metal sidecar，并拒绝架构或版本不匹配的二进制

#### Scenario: 在 Windows x64 运行
- **WHEN** 应用在受支持的 Windows x64 设备上启动环境诊断
- **THEN** 系统定位已签名且哈希匹配的 x64 CPU sidecar，并使用支持 Unicode 与空格路径的参数调用方式

#### Scenario: 在未支持平台运行
- **WHEN** 当前系统是 Intel Mac、Windows ARM64 或其他未声明支持的平台
- **THEN** 系统将资源准备标记为不受支持且不尝试运行不匹配的 sidecar

### Requirement: 随安装包提供默认模型并严格校验
系统 SHALL 在 macOS arm64 与 Windows x64 的安装包或受信 Runtime Resource Pack 中提供相同版本的 small 多语言量化模型、兼容 VAD 模型和资源 manifest，并 MUST NOT 在第一版支持任意模型路径、未经签名的资源导入或未授权的模型切换。

#### Scenario: 随包或下载资源完整
- **WHEN** 资源门禁确认模型文件存在、声明大小与 SHA-256 匹配且资源版本兼容
- **THEN** 系统允许继续执行内存、磁盘和媒体检查

#### Scenario: 随包资源完整
- **WHEN** 任务前检查确认模型文件存在、声明大小与 SHA-256 匹配且资源版本兼容
- **THEN** 系统允许继续执行内存、磁盘和媒体检查

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
