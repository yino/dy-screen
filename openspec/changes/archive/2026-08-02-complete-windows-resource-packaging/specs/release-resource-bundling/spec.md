## ADDED Requirements

### Requirement: 从受信源码构建 Windows x64 原生资源
Windows 发行流水线 SHALL 从固定版本和 SHA-256 的受信输入构建或组装 x64 `ffmpeg.exe`、`ffprobe.exe`、`whisper-cli.exe` 和 VAD sidecar，并 MUST 校验 PE 架构、CPU 基线、版本、许可证和全部非系统 DLL 依赖。

#### Scenario: 构建 Windows FFmpeg 与 FFprobe
- **WHEN** Windows x64 构建机提供锁定的 FFmpeg 源码和声明的构建工具链
- **THEN** 流水线生成不启用 GPL/nonfree 的 x64 FFmpeg/FFprobe，并验证其具备 HTTPS FLV/HLS 录制、MKV 分片、预览、首帧、音频准备、PNG、MP4、AAC、`h264_mf`、concat 和 overlay 能力

#### Scenario: 构建 Windows Whisper 与 VAD
- **WHEN** Windows x64 构建机提供锁定的 whisper.cpp 源码
- **THEN** 流水线生成以 SSE4.2 为最低 CPU 基线、关闭 AVX/AVX2/GPU/网络且不依赖未声明 Whisper/GGML/OpenMP DLL 的 CPU sidecar

#### Scenario: 原生资源依赖构建机目录
- **WHEN** 任一 PE 引用了 MSYS2、CMake、临时目录或未在 manifest 声明的第三方 DLL
- **THEN** 资源准备失败且不得进入 staging、签名或 Tauri 打包阶段

### Requirement: 生成完整的 Windows x64 可信资源源目录
Windows 资源准备工具 SHALL 把媒体 sidecar、ASR sidecar、small 多语言模型、Silero VAD、OpenCC 字典、许可证和 Microsoft VC++ x64 运行库组装为新的可信源目录，并 SHALL 保存来源、版本、大小、SHA-256、平台和依赖记录。

#### Scenario: 组装完整 Windows 资源
- **WHEN** 调用者提供全部已校验的本地普通文件
- **THEN** 工具生成只包含 Windows x64 平台条目和共享只读资源的 manifest，并可由 `asr-bundle stage --platform windows-x86-64` 完整封存

#### Scenario: 校验 Microsoft 运行库
- **WHEN** 资源源目录包含 `vc_redist.x64.exe`
- **THEN** 工具验证文件是有效 x64 安装器且具有有效 Microsoft Authenticode 签名，并将其纳入大小、SHA-256 和许可证/来源记录

#### Scenario: 输入缺失或不可审计
- **WHEN** 输入缺少必需组件、使用符号链接/重解析点、哈希不符或来源记录缺失
- **THEN** 工具拒绝生成或覆盖可信源目录，且不得留下可被后续构建误用的半成品 marker

### Requirement: 发布与客户端契约一致的 Windows Runtime Resource Pack
发行工具 SHALL 从同一次 Windows staging 同时生成 NSIS 随包资源和固定 HTTPS 资源目录，二者的 `runtime-manifest.json`、组件版本、大小和 SHA-256 MUST 一致。

#### Scenario: 生成 Windows 资源服务器目录
- **WHEN** 发行者执行 Windows 资源发布目标
- **THEN** 工具按 channel、应用版本、`windows/x86_64` 和 bundleVersion 生成平台目录，其中包含客户端可直接读取的 `runtime-manifest.json`、清单声明的逐文件资源、许可证和上层版本 index

#### Scenario: 注入 Windows 固定资源地址
- **WHEN** 正式 Tauri 构建配置 Windows 资源基础 URL
- **THEN** URL 使用 HTTPS 且直接定位到本应用版本的 Windows x64 资源目录，普通用户和 WebView 无法替换为任意地址

#### Scenario: 比较随包与在线资源
- **WHEN** 发行门禁检查 NSIS staging 和待上传资源目录
- **THEN** 两侧 manifest 和每个组件哈希完全一致，否则构建失败且不得更新 stable index

### Requirement: 构建可安装和可签名的 Windows NSIS 发行包
Windows 正式构建 SHALL 使用稳定 Bundle ID 和当前用户安装模式生成“切片智能体”NSIS 安装器，SHALL 随包提供经过验证的应用资源、离线 WebView2 和 VC++ x64 运行库，并 MUST 在资源或运行时门禁失败时停止打包。

#### Scenario: 构建开发安装包
- **WHEN** Windows 开发者显式执行开发打包目标且资源门禁通过
- **THEN** 流水线可以生成标记为非正式的无签名 NSIS，用于本地安装测试，但不得把它发布到 stable 渠道

#### Scenario: 构建正式安装包
- **WHEN** 发行环境提供受控 Authenticode 证书引用和时间戳服务
- **THEN** 流水线签署所有项目自产 PE/DLL、安装器和卸载器，验证签名、证书指纹和时间戳，并只验证而不重新签署 Microsoft WebView2/VC++ 安装器

#### Scenario: 安装离线运行时
- **WHEN** 用户在断网 Windows x64 设备安装正式 NSIS 且系统缺少所需运行时
- **THEN** 安装器静默安装随包 WebView2 与 VC++ x64 运行库，接受成功或需要重启状态，并在其他失败状态下中止安装并显示中文原因

#### Scenario: 覆盖升级应用
- **WHEN** 用户从相同 Bundle ID 的旧版本升级到新版本
- **THEN** 安装器替换受控应用文件且保留 SQLite、设置、录像、预览、ASR 产物和已安装资源回滚版本

### Requirement: 在真实 Windows x64 设备完成发行验收
正式 Windows 发行 SHALL 在受支持的 Windows 10/11 x64 目标设备生成不可覆盖的脱敏验收证据；macOS 交叉编译、脚本解析或无签名开发安装包 MUST NOT 替代真实发行验收。

#### Scenario: 验证离线完整功能
- **WHEN** 正式安装包在断网、中文且含空格的用户路径完成安装
- **THEN** 应用可以启动并完成公开直播录制、视频预览、首帧封面、本地 ASR 和带 ASR 字幕的 MP4 剪辑导出，且退出后没有遗留 FFmpeg/Whisper/VAD 子进程

#### Scenario: 验证在线资源修复
- **WHEN** 已安装资源被移除或篡改且设备可访问固定 HTTPS Windows 资源目录
- **THEN** 应用保持主功能门禁、拒绝执行损坏文件，并在下载和逐文件校验通过后恢复使用且不修改用户数据

#### Scenario: 验证安装生命周期
- **WHEN** 验收依次执行首次安装、同版本重装、旧版本覆盖升级和卸载
- **THEN** 报告记录退出码、应用/资源哈希、签名与时间戳、SmartScreen 证据引用、运行库状态和用户数据保留结论

#### Scenario: 正式门禁缺少外部证据
- **WHEN** 缺少有效签名证书、SmartScreen 证据或真实 Windows x64 验收报告之一
- **THEN** 流水线不得把安装包标记为正式可发布，也不得更新 stable Windows index
