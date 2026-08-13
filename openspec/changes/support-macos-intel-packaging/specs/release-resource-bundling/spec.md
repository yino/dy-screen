## MODIFIED Requirements

### Requirement: 生成平台专属 Runtime Resource Pack
发行工具 SHALL 为 macOS arm64、macOS Intel x86_64 和 Windows x64 分别生成只包含目标平台组件的资源包，并 SHALL 包含媒体处理、ASR sidecar、模型、规范化字典、许可证和平台运行时所需文件。

#### Scenario: 生成 macOS arm64 资源包
- **WHEN** 构建机提供可信的 macOS arm64 staging 源
- **THEN** 工具生成包含 Metal `whisper.cpp`、VAD、FFmpeg、FFprobe、相对路径动态库、模型和 manifest 的资源包

#### Scenario: 在 Apple Silicon 生成 macOS Intel 资源包
- **WHEN** Apple Silicon 构建机从锁定源码为 x86_64 目标构建并组装完整 staging 源
- **THEN** 工具生成只包含 x86_64 CPU `whisper.cpp`、VAD、FFmpeg、FFprobe、相对路径动态库、模型和 manifest 的 `macos-x86-64` 资源包

#### Scenario: 生成 Windows x64 资源包
- **WHEN** Windows x64 构建机提供可信的 staging 源
- **THEN** 工具生成包含 CPU sidecar、FFmpeg、FFprobe、模型、manifest、VC++ 运行库说明和许可证的资源包

#### Scenario: staging 源缺少组件
- **WHEN** staging 源缺少平台声明的任何必需文件、原生文件架构与目标不一致或存在符号链接
- **THEN** 构建失败且不生成可发布的安装包或资源归档

### Requirement: 将已验证资源映射进 Tauri 安装包
正式 Tauri 构建 SHALL 在生成 `.app`、`.dmg` 或 Windows NSIS 安装器前完成 staging 和完整性校验，并 SHALL 将当前目标平台和架构的资源映射到受控应用资源目录。

#### Scenario: macOS arm64 发行构建
- **WHEN** 未指定架构且 arm64 macOS 构建机执行 macOS ASR 发行目标，或显式指定 `MACOS_ARCH=aarch64`
- **THEN** 流程使用 `aarch64-apple-darwin` 主程序和 `macos-aarch64` 资源生成文件名包含 `aarch64` 的 `.app` 与 DMG

#### Scenario: Apple Silicon 交叉构建 macOS Intel 发行包
- **WHEN** arm64 macOS 构建机已安装 `x86_64-apple-darwin` Rust target、提供 `macos-x86-64` 可信资源并显式指定 `MACOS_ARCH=x86_64`
- **THEN** 流程使用 x86_64 主程序、sidecar 和动态库生成文件名包含 `x86_64` 的独立 `.app` 与 DMG

#### Scenario: Windows 发行构建
- **WHEN** 执行 Windows ASR 发行目标
- **THEN** NSIS 安装器包含当前平台资源和所需离线运行时，并在安装后保持资源相对路径可解析

#### Scenario: 资源校验未通过
- **WHEN** staging manifest、文件大小、哈希、许可证、目标架构或平台检查失败
- **THEN** 构建立即失败，不允许生成看似可用但缺少本地 AI 资源的发行包

#### Scenario: 指定不支持的 macOS 架构
- **WHEN** macOS 构建命令传入 `aarch64` 与 `x86_64` 之外的架构值
- **THEN** 流程在 staging 或编译前返回可操作错误且不复用其他架构产物

### Requirement: 生成可供自有服务器托管的发行目录
发行工具 SHALL 生成静态 HTTPS 托管目录，包含按 channel、应用版本、平台、架构和资源版本组织的 manifest、签名、归档/分片、SHA-256 和许可证文件，并 SHALL 支持通过构建参数注入基础 URL。

#### Scenario: 生成 macOS 架构专属资源发布目录
- **WHEN** 发行流水线完成 `macos-aarch64` 或 `macos-x86-64` 资源 staging
- **THEN** 输出目录分别使用 `macos/aarch64` 或 `macos/x86_64`，且上层 index 的架构字段与资源 manifest 一致

#### Scenario: 生成其他目标资源发布目录
- **WHEN** 发行流水线完成目标平台资源 staging
- **THEN** 输出目录包含客户端可直接请求的 index/manifest、签名、公钥指纹、资源包和许可证文件

#### Scenario: 注入用户自有基础地址
- **WHEN** 构建命令传入与目标平台和架构匹配的资源服务器基础 URL
- **THEN** manifest 中的下载地址按该基础 URL 生成，业务代码不需要修改或重新编译下载逻辑

#### Scenario: 基础地址不安全
- **WHEN** 构建参数不是 HTTPS 或包含本地文件路径
- **THEN** 发行工具拒绝生成远程下载 manifest，但仍可生成仅随包使用的离线资源包

## ADDED Requirements

### Requirement: 从受信源码交叉构建 macOS 原生资源
macOS 发行流水线 SHALL 从固定版本和 SHA-256 的受信输入为显式目标架构构建 `ffmpeg`、`ffprobe`、`whisper-cli` 和 VAD sidecar，并 MUST 校验 Mach-O 架构、版本、许可证和全部非系统动态依赖。

#### Scenario: 构建目标架构 FFmpeg 与 FFprobe
- **WHEN** macOS 构建机提供锁定 FFmpeg 源码并选择 `aarch64` 或 `x86_64`
- **THEN** 流水线生成仅包含目标切片、不启用 GPL/nonfree、依赖包内相对 dylib 且具备声明媒体能力的 FFmpeg 与 FFprobe

#### Scenario: 构建目标架构 Whisper 与 VAD
- **WHEN** macOS 构建机提供锁定 whisper.cpp 源码并选择 `aarch64` 或 `x86_64`
- **THEN** arm64 产物静态链接并内嵌 Metal，x86_64 产物关闭主机原生 CPU 优化和 Metal，且两者均不依赖未声明 Whisper/GGML/OpenMP 动态库

#### Scenario: 原生资源切片或依赖错误
- **WHEN** 任一 Mach-O 文件缺少目标切片、包含非目标切片或引用 CMake、临时目录及未声明第三方动态库
- **THEN** 资源构建或组装失败且不得进入 staging、签名或 Tauri 打包阶段

### Requirement: 在真实 Intel Mac 完成发行验收
正式 macOS Intel 发行 SHALL 在受支持的 Intel Mac 目标设备生成脱敏验收证据；Apple Silicon 交叉编译、静态 Mach-O 检查或 Rosetta 运行 MUST NOT 单独替代真实 Intel 发行验收。

#### Scenario: 验证 Intel 离线完整功能
- **WHEN** x86_64 正式安装包在断网 Intel Mac 完成安装
- **THEN** 应用可以启动并完成公开直播录制、视频预览、首帧封面、本地 ASR 和带 ASR 字幕的 MP4 剪辑导出，且退出后没有遗留 FFmpeg、Whisper 或 VAD 子进程

#### Scenario: Intel 正式门禁缺少外部证据
- **WHEN** 缺少有效 Developer ID 签名、公证、Gatekeeper 或真实 Intel Mac 验收报告之一
- **THEN** 流水线不得把 x86_64 DMG 标记为正式可发布，也不得更新 stable macOS Intel index
