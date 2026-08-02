## 1. Windows 构建环境与测试基线

- [x] 1.1 增加 Windows x64 构建环境诊断脚本，校验 64 位系统、Visual Studio 2022 Build Tools、Windows SDK、Rust MSVC、Node/npm、CMake、NSIS、PowerShell 5.1、MSYS2 UCRT64、`dumpbin` 和 `signtool`。
- [x] 1.2 为构建环境诊断增加 PowerShell AST、工具缺失、错误架构和版本输出测试，确保错误信息不泄露用户名或绝对私有路径。
- [x] 1.3 扩展 `make asr-check-windows` 和 CI 静态检查，覆盖根 crate、Tauri crate、Windows 专用测试和所有 Windows PowerShell 脚本语法。

## 2. Windows FFmpeg 与 Whisper 原生资源

- [x] 2.1 为 Windows FFmpeg 构建脚本增加锁定源码哈希、输出目录保护、LGPL/GPL 边界、PE 架构和未声明 DLL 依赖的失败测试。
- [x] 2.2 实现 `scripts/build-asr-ffmpeg-windows.ps1`，使用锁定的 FFmpeg 8.1.2 与 MSYS2 UCRT64 生成 Windows x64 `ffmpeg.exe` 和 `ffprobe.exe`。
- [x] 2.3 在 Windows FFmpeg 构建后校验 HTTPS/FLV/HLS/MKV、segment、mov/mp4、PNG、AAC、PCM、MJPEG、`h264_mf`、concat/image2 和字幕 overlay 所需滤镜能力，并输出版本、配置、来源和许可证记录。
- [x] 2.4 补强 `build-asr-whisper-windows.ps1` 的普通文件、PE 依赖、版本和构建记录检查，并将新脚本纳入 Windows AST 测试。
- [ ] 2.5 在原生 Windows x64 构建机运行 FFmpeg 与 Whisper/VAD 构建，保存到本地可信资源目录并核对 SSE4.2 基线、无 AVX/AVX2/GPU/网络和无构建目录依赖。

## 3. Windows 可信资源组装与 staging

- [x] 3.1 增加 Windows 资源组装脚本测试，覆盖缺少模型/VAD/字典/许可证/VC++ 运行库、符号链接或重解析点、哈希不符、非 x64 PE 和已有输出目录。
- [x] 3.2 实现 Windows 可信资源组装脚本，合并 FFmpeg、Whisper/VAD、small-q5_1 模型、Silero VAD、OpenCC 字典、许可证和官方 `vc_redist.x64.exe`，并原子生成 manifest 与 `SHA256SUMS`。
- [x] 3.3 校验 `vc_redist.x64.exe` 的 Microsoft Authenticode、文件版本、x64 身份和 SHA-256；仅把验证结果与来源写入构建记录，不保存签名证书隐私字段。
- [x] 3.4 扩展 `asr-bundle` Windows staging/verify 测试，拒绝跨平台混装、未声明 PE/DLL、开发机路径、缺失许可证和组件哈希不完整。
- [ ] 3.5 使用组装后的 `resources/asr-source/` 生成 `resources/asr-stage/`，运行 `asr-bundle verify --platform windows-x86-64` 和 Windows FFmpeg 剪辑能力检查。

## 4. Windows Runtime Resource Pack 发布

- [x] 4.1 增加 Windows 发布目录测试，覆盖 channel、应用版本、`windows/x86_64`、bundleVersion、上层 index、直接可读 `runtime-manifest.json` 和逐文件下载路径。
- [x] 4.2 实现 `runtime-resource-publish-windows`，从单次 Windows staging 原子生成可上传的静态 HTTPS 目录、许可证、manifest 和全部声明组件。
- [x] 4.3 在发布门禁中比较 NSIS staging 与在线目录的 manifest、组件大小和 SHA-256，并拒绝 HTTP、本地路径、占位签名、空签名或 stable 目录覆盖。
- [ ] 4.4 使用固定测试服务器验证 Windows 资源的 manifest 获取、Range/重试、逐文件哈希、取消和修复流程，确认客户端不上传业务数据。

## 5. Tauri NSIS 与 Authenticode

- [x] 5.1 增加 Windows Tauri 配置和 NSIS hooks 测试，固定产品名“切片智能体”、Bundle ID、currentUser、离线 WebView2、资源映射和 VC++ 成功/3010/失败处理。
- [x] 5.2 增加无签名开发 NSIS 目标，要求 Windows staging 和 FFmpeg 能力先通过，并在产物元数据中明确标记为非正式发行。
- [x] 5.3 增加正式 Windows NSIS 目标，通过证书存储或 CI secret 引用签署项目自产 PE/DLL、安装器和卸载器，并验证证书指纹和 RFC 3161 时间戳。
- [ ] 5.4 验证 Microsoft WebView2 与 VC++ 安装器的 Microsoft 签名且不重新签署，扫描构建日志和产物，确保没有 PFX、密码、认证头或开发机绝对路径。
- [ ] 5.5 在原生 Windows x64 构建机生成带完整资源的开发 NSIS，记录安装包路径、大小、SHA-256、资源 bundleVersion 和构建工具版本。

## 6. Windows 自动化与业务回归

- [ ] 6.1 在 Windows x64 运行 Rust/React/Tauri 全量测试、格式检查、Clippy、前端生产构建和 NSIS 配置检查。
- [ ] 6.2 在中文且含空格的路径运行资源诊断、FFmpeg 录制/预览/封面、真实 Whisper ASR、取消清理和带字幕 MP4 导出集成测试。
- [ ] 6.3 验证 Windows Credential Manager 中的 DeepSeek Key 只通过凭据抽象访问，应用升级、重装和资源修复不会把密钥写入 SQLite、日志或安装包。
- [ ] 6.4 运行现有多主播监听、录制会话恢复、视频库、AI 调度、SQLite migration 和剪辑导出回归测试，修复 Windows 路径或进程生命周期差异。

## 7. 安装、升级、卸载与正式发行验收

- [ ] 7.1 在干净 Windows 10/11 x64 设备验证断网首次安装、离线 WebView2、VC++ 运行库、应用启动和资源门禁，生成不可覆盖的脱敏证据。
- [ ] 7.2 在已安装正式资源下验证公开直播录制、视频预览、首帧封面、本地 ASR 和带 ASR 字幕的 MP4 剪辑导出，确认退出后没有遗留 FFmpeg/Whisper/VAD 子进程。
- [ ] 7.3 验证资源删除、篡改、下载取消、断点重试和固定 HTTPS 在线修复，确认任何失败都不修改录像、数据库、ASR 产物或当前完整资源版本。
- [ ] 7.4 验证同版本重装、旧版本覆盖升级和卸载，逐项核对 SQLite、设置、录像、预览、ASR 产物、凭据和资源回滚版本的保留策略。
- [ ] 7.5 使用正式 Authenticode 证书生成候选安装包，运行 `make asr-verify-release-windows`，核对安装器、主程序、sidecar、DLL、卸载器、时间戳、Microsoft 运行库和 SmartScreen 证据引用。
- [ ] 7.6 只有全部 Windows 发行报告通过后才上传不可变资源目录、更新 stable Windows index 并发布安装包；缺少证书、SmartScreen 或实机证据时保持任务未完成。

## 8. 文档与最终校验

- [x] 8.1 更新 README、Makefile help 和 Wiki，说明 Windows 构建前置条件、资源组成、开发/正式打包、签名、安装、升级、卸载和故障排查命令。
- [x] 8.2 增加 Windows 发行清单模板，记录应用版本、资源版本、安装包/manifest/组件哈希、许可证、签名指纹、时间戳和验收报告编号。
- [x] 8.3 运行 `openspec validate --all --strict`、`git diff --check`、敏感信息扫描和完整构建命令审计，确保规格、任务和实际证据状态一致。
