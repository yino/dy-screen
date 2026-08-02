## Context

仓库已经存在 Windows x64 的 Tauri 覆盖配置、NSIS VC++ 安装钩子、`whisper.cpp` CPU sidecar 构建脚本、单平台 staging、交叉编译检查和安装后验收脚本。共享模型、VAD、OpenCC 字典、许可证和锁定的 FFmpeg/Whisper 源码也已保存在本地可信资源目录，但实际资源中只有 macOS arm64 二进制。

当前缺口集中在交付链路：没有 Windows FFmpeg/FFprobe 构建脚本，没有把 Windows 原生产物、官方运行库和公共模型组装成可信源目录的命令，没有 Windows 资源服务器发布目标，也没有用真实 NSIS 产物完成签名、安装、升级、卸载和离线业务验收。现有 `runtime-resource-publish` 还只输出 macOS 路径，正式资源清单和下载目录必须与客户端直接读取 `runtime-manifest.json` 及逐文件下载的契约保持一致。

Windows 正式产物涉及 Tauri、Rust、PowerShell、MSVC、MSYS2/FFmpeg、NSIS、WebView2、VC++ Runtime 和 Authenticode，必须在原生 Windows x64 构建机闭环；macOS 上的 `cargo-xwin` 只能提供条件编译检查，不能替代安装包构建和运行证据。

## Goals / Non-Goals

**Goals:**

- 在 Windows 10/11 x64、SSE4.2、8 GB 内存基线下生成完整的可信 Runtime Resource Pack。
- 从锁定的 FFmpeg 8.1.2 与 whisper.cpp 1.9.1 源码重复构建 Windows CPU 原生组件，并审计架构、版本、许可证和 DLL 依赖。
- 生成名为“切片智能体”的 Tauri 2.0 NSIS 安装器，离线提供 WebView2 与 VC++ x64 运行环境，安装后无需用户配置 FFmpeg、模型或任意可执行路径。
- 同时输出随包资源和可上传到固定 HTTPS 地址的 Windows 资源目录，二者使用同一份文件哈希和版本。
- 为开发包与正式发行包建立不同门禁：开发包允许显式无签名，正式发行包必须完成 Authenticode、时间戳、SmartScreen 证据和目标机验收。
- 保持 `com.yino.dyscreen`、SQLite、应用数据目录和录像目录兼容，覆盖升级不得丢失用户数据。

**Non-Goals:**

- 不支持 Windows ARM64、x86、Windows 7/8、Server Core 或 Wine。
- 不提供 CUDA、Vulkan、DirectML 或其他 GPU ASR 加速。
- 不引入 GPL `libx264`、nonfree 编解码器或来源不明的第三方 FFmpeg 成品。
- 不实现 Microsoft Store/MSIX、自动更新和增量补丁。
- 不采购、托管或导出代码签名私钥，不把证书密码写入仓库、日志或资源清单。
- 不改变直播访问边界，也不实现登录、验证码、DRM、付费内容或私有直播间绕过。

## Decisions

### 1. 正式构建固定在原生 Windows x64 环境

使用 Windows 10/11 x64、Visual Studio 2022 Build Tools、Windows 10/11 SDK、Rust MSVC、Node.js、PowerShell 5.1、CMake、NSIS 和 MSYS2 UCRT64 作为正式构建环境。构建前置检查输出工具版本并拒绝错误架构；`cargo-xwin` 继续用于非 Windows 开发机的快速静态回归。

选择原生 Windows 是因为 Tauri 的 NSIS/WebView2 离线打包、MSVC PE、Media Foundation 编码、Authenticode 和安装行为都无法由 macOS 交叉构建可靠验证。备选的 macOS 交叉生成 Windows 安装包只能覆盖部分编译步骤，不能作为可发布产物。

### 2. Windows FFmpeg 从锁定官方源码构建

新增 PowerShell 驱动的 Windows FFmpeg 构建入口，在 MSYS2 UCRT64 环境编译锁定 SHA-256 的 FFmpeg 8.1.2。构建保持 LGPL，关闭 GPL/nonfree 和自动探测，只启用应用实际需要的 HTTPS/FLV/HLS/MKV、预览、首帧、PCM/AAC、PNG、MP4、concat/overlay 等协议、编解码器、封装和滤镜；H.264 导出使用 Windows 系统 Media Foundation `h264_mf`。

输出优先为只依赖 Windows 系统 DLL 的 `ffmpeg.exe`/`ffprobe.exe`。若工具链不可避免地产生运行 DLL，则必须把每个 DLL 纳入平台 `libraries`、哈希、签名和安装验证，禁止依赖 MSYS2 安装目录。选择源码构建而不是下载社区静态包，是为了固定来源、许可证、能力集合和供应链哈希；不使用 `libx264`，避免把发行边界升级为 GPL。

### 3. 可信资源源目录由显式组装命令生成

新增 Windows 资源准备脚本，把以下输入组装到新的可信源目录：

- Windows `ffmpeg.exe`、`ffprobe.exe`；
- Windows x64 `whisper-cli.exe`、`vad-speech-segments.exe`；
- 跨平台的 small-q5_1 模型、Silero VAD、OpenCC 字典和许可证；
- Microsoft 官方 `vc_redist.x64.exe`；
- 锁定版本、来源、大小、SHA-256、PE 架构、CPU 基线和 DLL 依赖记录。

脚本只接受调用者提供的本地普通文件，不隐式下载，不覆盖已有输出，不接受符号链接/重解析点。Microsoft 运行库必须验证有效 Microsoft Authenticode 签名；项目自产 PE 在正式阶段使用发行证书签名。大型二进制继续由 `.gitignore` 排除，仓库只保存脚本、模板、哈希和许可证说明。

### 4. staging、随包和在线资源复用同一份单平台清单

`asr-bundle stage --platform windows-x86-64` 继续作为唯一 staging 边界，并补强 Windows PE、必需组件、未声明文件和许可证检查。输出只包含 Windows x64 文件，不包含 macOS 二进制或源码归档。

新增 Windows 发布目标，将 staging 目录按 `channel/appVersion/windows/x86_64/bundleVersion/` 输出，并在构建期向应用注入能够直接解析到该平台目录的 HTTPS 基础地址。该目录必须包含客户端实际读取的 `runtime-manifest.json` 和清单声明的逐文件资源；上层 `index.json` 仅承担版本路由，不替代客户端当前下载契约。正式清单在发布前签名，签名私钥只从受控 CI/签名环境读取。

随 NSIS 打包的资源与在线目录来自同一次 staging，正式验收比较两者 manifest 与组件哈希，避免安装包和修复服务器提供不同二进制。

### 5. NSIS 分为开发构建和正式发行构建

Windows Tauri 配置保持 `currentUser` 安装、稳定 Bundle ID、离线 WebView2 和 VC++ post-install hook。资源校验、FFmpeg 能力检查或运行库校验失败时，不启动 Tauri bundler。

开发目标生成带明显说明的无签名 NSIS，用于本地安装调试。正式目标先签署项目自产 PE 和 DLL，再构建并签署 NSIS 安装器/卸载器，使用带时间戳的 Authenticode；Microsoft 的 WebView2/VC++ 安装器只验证 Microsoft 签名，不重新签署。证书通过 Windows Certificate Store 或 CI secret 引用，命令输出只保留证书指纹和签名状态。

选择 NSIS 是因为项目已经采用 Tauri NSIS、需要当前用户安装和自定义运行库钩子。MSIX/Store 会改变资源位置、签名和升级模型，不在本次范围内。

### 6. 真实 Windows 证据是正式发行完成条件

自动化测试先覆盖 PowerShell AST、manifest/staging、PE 路径、Tauri 配置和 Rust Windows 条件编译。随后必须在干净 Windows x64 用户环境运行：

- 中文和空格路径安装、启动、录制、预览、首帧、本地 ASR、带字幕剪辑导出；
- 断网首次启动、VC++/WebView2 离线安装、资源损坏拦截、固定 HTTPS 资源修复；
- 同版本重装、旧版到新版覆盖升级、卸载和应用数据保留；
- ASR/FFmpeg 取消、退出后的子进程清理；
- 主程序、自产 sidecar/DLL、安装器、卸载器的签名/时间戳，以及 Microsoft 运行库签名；
- SmartScreen 人工证据编号和脱敏 JSON 验收报告。

开发构建通过不代表正式发行完成。没有 Windows 实机、签名证书或 SmartScreen 证据时，对应任务保持未完成并明确记录外部条件。

## Risks / Trade-offs

- [MSYS2 是滚动工具链，可能导致不同时间构建结果变化] → 锁定 FFmpeg 源码和配置，记录 MSYS2、编译器与 SDK 版本，验证最终能力、PE 依赖和哈希；正式资源按不可变 bundleVersion 发布。
- [`h264_mf` 在部分系统版本或精简系统中不可用] → 支持范围限定为完整 Windows 10/11 x64，并在目标机执行实际 MP4/字幕导出；诊断失败时阻止导出，不回退到 GPL 编码器。
- [离线 WebView2、模型和原生资源使 NSIS 体积显著增加] → 保留随包完整资源保证首次离线可用，同时输出相同版本的在线修复目录；发布报告记录安装包大小和磁盘需求。
- [VC++ 安装返回重启码或被企业策略阻止] → NSIS 接受成功与 3010，其他退出码中止安装并显示中文错误；验收覆盖已有/缺失运行库与重启要求。
- [Authenticode 私钥泄露] → 只使用证书存储或 CI secret 引用，脚本禁止 PFX 路径/密码进入日志和产物，仓库测试扫描敏感字段。
- [升级或卸载误删用户录像与数据库] → 安装器只管理应用目录和受控随包资源；升级/卸载测试在操作前后对应用数据和录像样本做哈希核对。
- [Windows 实机不可用导致只能完成脚本] → 自动化与实机任务分开记录，正式发行门禁不得用 macOS 交叉检查替代。

## Migration Plan

1. 补充 Windows 构建环境检查、FFmpeg 构建和资源组装脚本，并用固定测试 fixture 验证脚本语法与失败门禁。
2. 在 Windows x64 构建机生成 FFmpeg、Whisper/VAD 与可信资源源目录，记录版本、SHA-256、依赖和许可证。
3. 扩展 staging 和 Windows 资源发布目标，生成新的 bundleVersion；保持现有 macOS bundleVersion 和服务器目录不变。
4. 先生成无签名开发 NSIS，验证安装、运行、业务功能、升级和卸载；失败时删除开发安装包，不更新 stable 目录。
5. 在受控签名环境签署自产 PE、正式 NSIS 与卸载器，上传 Windows 资源目录到独立不可变路径。
6. 在干净 Windows x64 设备执行正式验收；只有全部门禁通过后才更新 stable Windows index。
7. 回滚时恢复上一个 stable Windows index 和安装包，保留已安装资源旧版本及全部用户录像、数据库和 ASR 产物。

## Open Questions

无阻塞设计问题。正式发行阶段需要外部提供 Windows x64 构建/验收设备、有效 Authenticode 证书与时间戳服务；这些是发行输入，不改变上述实现方案。
