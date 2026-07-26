# release-resource-bundling Specification

## Purpose
TBD - created by archiving change bundle-and-auto-provision-asr-resources. Update Purpose after archive.
## Requirements
### Requirement: 生成平台专属 Runtime Resource Pack
发行工具 SHALL 为 macOS arm64 和 Windows x64 分别生成只包含目标平台组件的资源包，并 SHALL 包含媒体处理、ASR sidecar、模型、规范化字典、许可证和平台运行时所需文件。

#### Scenario: 生成 macOS arm64 资源包
- **WHEN** 构建机提供可信的 macOS arm64 staging 源
- **THEN** 工具生成包含 Metal `whisper.cpp`、VAD、FFmpeg、FFprobe、相对路径动态库、模型和 manifest 的资源包

#### Scenario: 生成 Windows x64 资源包
- **WHEN** Windows x64 构建机提供可信的 staging 源
- **THEN** 工具生成包含 CPU sidecar、FFmpeg、FFprobe、模型、manifest、VC++ 运行库说明和许可证的资源包

#### Scenario: staging 源缺少组件
- **WHEN** staging 源缺少平台声明的任何必需文件或存在符号链接
- **THEN** 构建失败且不生成可发布的安装包或资源归档

### Requirement: 将已验证资源映射进 Tauri 安装包
正式 Tauri 构建 SHALL 在生成 `.app`、`.dmg` 或 Windows NSIS 安装器前完成 staging 和完整性校验，并 SHALL 将当前平台资源映射到受控应用资源目录。

#### Scenario: macOS 发行构建
- **WHEN** 执行 macOS ASR 发行目标
- **THEN** `.app/Contents/Resources/resources/` 包含当前平台资源，`ASR_BUNDLES=app,dmg` 时同时生成可分发 DMG

#### Scenario: Windows 发行构建
- **WHEN** 执行 Windows ASR 发行目标
- **THEN** NSIS 安装器包含当前平台资源和所需离线运行时，并在安装后保持资源相对路径可解析

#### Scenario: 资源校验未通过
- **WHEN** staging manifest、文件大小、哈希、许可证或平台架构检查失败
- **THEN** 构建立即失败，不允许生成看似可用但缺少本地 AI 资源的发行包

### Requirement: 生成可供自有服务器托管的发行目录
发行工具 SHALL 生成静态 HTTPS 托管目录，包含按 channel、应用版本、平台、架构和资源版本组织的 manifest、签名、归档/分片、SHA-256 和许可证文件，并 SHALL 支持通过构建参数注入基础 URL。

#### Scenario: 生成资源发布目录
- **WHEN** 发行流水线完成目标平台资源 staging
- **THEN** 输出目录包含客户端可直接请求的 index/manifest、签名、公钥指纹、资源包和许可证文件

#### Scenario: 注入用户自有基础地址
- **WHEN** 构建命令传入资源服务器基础 URL
- **THEN** manifest 中的下载地址按该基础 URL 生成，业务代码不需要修改或重新编译下载逻辑

#### Scenario: 基础地址不安全
- **WHEN** 构建参数不是 HTTPS 或包含本地文件路径
- **THEN** 发行工具拒绝生成远程下载 manifest，但仍可生成仅随包使用的离线资源包

### Requirement: 验证安装包离线可用和资源下载修复
发行验收 SHALL 同时验证完整资源随包离线启动、资源缺失时下载修复、资源篡改被拒绝、安装后录制/预览/封面/ASR 共用受控 FFmpeg 和平台 sidecar。

#### Scenario: 完整安装包离线启动
- **WHEN** 设备断网且安装包资源完整
- **THEN** 应用完成启动门禁并可执行监听、录制、视频预览、首帧封面和本地 ASR

#### Scenario: 缺失资源在线修复
- **WHEN** 安装后移除或损坏资源且设备可以访问自有 HTTPS 服务器
- **THEN** 应用显示下载进度，完成校验后恢复可用状态，不修改用户录像和数据库

#### Scenario: 篡改资源被拒绝
- **WHEN** 安装包或下载归档中的任一资源内容被替换
- **THEN** 启动或安装校验失败，应用不执行该文件并显示可操作修复信息

#### Scenario: 发行产物不含开发机路径
- **WHEN** 对 `.app`、DMG、NSIS 和资源 manifest 执行路径审计
- **THEN** 产物不包含 Homebrew、CMake 构建目录、绝对临时路径或开发机私有资源链接
