## MODIFIED Requirements

### Requirement: 支持目标桌面发行平台
系统 SHALL 在 macOS arm64、macOS Intel x86_64 与 Windows x64 使用受控随包资源执行智能成片，并 MUST 在任一必要 FFmpeg、FFprobe、Whisper、ASR/VAD 模型、Harness sidecar、渲染浏览器或平台能力未通过诊断时阻止创建可执行批次，同时保留录制和已有数据浏览能力。Windows ARM64 与其他未声明支持的平台 MUST NOT 被视为受支持的智能成片平台。

#### Scenario: 目标平台资源完整
- **WHEN** macOS arm64、macOS Intel x86_64 或 Windows x64 的受控资源、完整性、架构和媒体能力诊断通过
- **THEN** 系统允许创建和恢复智能成片任务，并使用资源管理器返回的已验证路径

#### Scenario: Intel 安装包包含错误架构资源
- **WHEN** 在 macOS Intel x86_64 上，受控资源、Harness 运行时、渲染浏览器或模型中任一项为错误架构或架构校验失败
- **THEN** 系统阻止智能处理、显示组件级架构原因，并保留录制与已有数据浏览能力，不回退到任意系统路径

#### Scenario: 未声明支持平台
- **WHEN** 客户端在 Windows ARM64 等未声明支持的平台上启动智能成片诊断
- **THEN** 系统把资源准备标记为不受支持、阻止创建可执行批次并显示组件级原因，不回退到任意系统路径

#### Scenario: 受支持平台资源不完整
- **WHEN** macOS arm64、macOS Intel x86_64 或 Windows x64 上任一必要 sidecar、动态依赖或模型缺失、哈希不匹配或架构不符
- **THEN** 系统阻止智能处理、显示组件级修复说明，并保留录制与已有数据浏览能力

#### Scenario: 保留非智能能力
- **WHEN** 当前平台未声明支持智能成片，或其受控资源诊断失败
- **THEN** 系统仍保留录制、视频库浏览、预览与封面等不依赖智能资源的既有能力
