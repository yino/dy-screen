## Why

项目已经具备 Windows x64 的资源清单、Whisper CPU sidecar 构建、NSIS 配置和验收脚本，但本地可信资源仍只有 macOS 产物，且缺少 Windows FFmpeg/FFprobe 构建、资源发布、签名和安装包实机验收的完整闭环。现在需要把 Windows 从“接口已预留”推进到可重复构建、可安装、可离线运行和可审计交付的正式目标平台。

## What Changes

- 增加 Windows x64 专用资源准备流水线，固定并校验 FFmpeg/FFprobe、`whisper.cpp`/VAD sidecar、small 多语言模型、Silero VAD、OpenCC 字典、许可证和 Microsoft VC++ x64 运行库。
- 增加可复现的 Windows FFmpeg/FFprobe 构建或受信二进制导入流程，确保录制、预览、封面、本地 ASR 和带字幕 MP4 剪辑导出所需能力全部存在。
- 完善 Windows 单平台 staging、哈希清单、Runtime Resource Pack 发布目录和固定 HTTPS 资源渠道输出，禁止混入 macOS 文件、开发机绝对路径或未声明 DLL。
- 完善 Tauri 2.0 Windows x64 NSIS 构建，随包提供离线 WebView2 与 VC++ 运行库，保持应用升级时的 Bundle ID、数据库、录像和资源目录兼容。
- 增加 Authenticode 签名接口和发行门禁，覆盖主程序、安装器、卸载器、原生 sidecar、DLL、时间戳和 SmartScreen 人工证据引用；签名密钥不得进入仓库、脚本参数日志或安装包资源。
- 增加 Windows x64 目标机验收，覆盖中文与空格路径、首次安装、覆盖升级、卸载、断网启动、资源在线修复、录制、预览、封面、ASR、字幕剪辑导出、任务取消和进程清理。
- 更新 Makefile、PowerShell 脚本和中文文档，提供从可信源码/资源到 NSIS、资源服务器目录和验收 JSON 的单一发布流程。
- 非目标：Windows ARM64、Windows 7/8、CUDA/Vulkan 加速、Microsoft Store/MSIX、自动更新、代码签名证书采购以及任何访问控制或验证码绕过。

## Capabilities

### New Capabilities

无。

### Modified Capabilities

- `release-resource-bundling`: 将 Windows x64 从通用资源映射要求扩展为可复现资源构建、单平台发布、NSIS 安装、签名和真实目标机验收的完整发行契约。

## Impact

- 构建与资源：`Makefile`、`resources/asr-source/` 模板、`asr-bundle`、新增/现有 PowerShell 构建与校验脚本。
- 桌面打包：`src-tauri/tauri.windows.conf.json`、NSIS hooks、Tauri 资源映射、WebView2 与 VC++ 离线运行时。
- 发行安全：Windows PE 架构和依赖审计、文件哈希、许可证、Authenticode/时间戳接口、资源 manifest 签名和敏感信息检查。
- 验证与文档：Windows x64 实机测试、NSIS 安装/升级/卸载证据、资源服务器目录、README 与 Wiki 发布手册。
- 不改变前端业务 API、SQLite schema、录制数据模型、ASR 结果格式和现有 macOS arm64 发行行为。
