# AI ASR 变更验收记录

对应 OpenSpec 变更：`add-user-triggered-ai-asr`。

逐 Requirement/Scenario 的证据映射见
[`AI-ASR-需求测试追踪矩阵.md`](AI-ASR-需求测试追踪矩阵.md)。该矩阵由
`tests/asr_spec_traceability.rs` 校验，新增或重命名规格场景时必须同步补充证据。

## 自动化验收范围

| 规格能力 | 主要证据 | 结论 |
| --- | --- | --- |
| 用户主动创建与启动 | 项目服务、命令和 React 测试证明打开页面、应用启动、录像完成均不自动创建任务 | 通过 |
| 本地多视频导入 | 一次性后端授权、Unicode/空格路径、顺序、去重、失效源与不复制原视频测试 | 通过 |
| 已结束直播会话 | 会话展开、进行中拒绝、缺失分片和视频库 ID 测试；后端 E2E 处理两个有序分片 | 通过 |
| FFprobe 与 FFmpeg | 有/无音轨、16 kHz 单声道 PCM、管道、取消、磁盘错误、临时清理和源哈希不变 | 通过 |
| VAD | 单元边界映射；真实 Silero 对中文人声、全静音和纯音乐 fixture 验证 | 通过 |
| CLI 分阶段诊断 | 同一封存资源根依次运行 probe、audio、vad、asr；验证无音轨、无人声、源哈希和临时目录 | 通过 |
| 中立 AsrEngine | 假引擎契约、能力、进度、取消和缓存测试不读取 Whisper 专属字段 | 通过 |
| WhisperCppEngine | JSON、置信信息、错误脱敏、取消、macOS Metal 真实中文短句；Windows CPU 目标测试源码 | macOS 自动化与签名后执行通过，Windows 待目标机执行 |
| 稳定句段与项目时间 | 多输入累计偏移、失败空洞、直播边界去重、跨项目稳定句段 ID | 通过 |
| 缓存与原子发布 | 完全命中跳过昂贵阶段；源、模型、VAD、热词和规范化变化失效；半成品拒绝 | 通过 |
| 调度与录制优先 | FIFO、全局单并发、活动录制等待、取消/失败许可释放、窗口无关和退出顺序 | 通过 |
| 取消与重启恢复 | 子进程取消、运行状态恢复为待重试、半成品废弃和递归临时文件清理 | 通过 |
| Tauri 安全边界 | 类型化命令、一次性文件授权、任意路径拒绝、DTO/事件/错误脱敏 | 通过 |
| 播放器与只读文本 UI | 点击定位、高亮、跟随、临时字幕、分页、响应式、复制和 TXT/JSON | 通过 |
| 第一版非目标 | 约束测试确认不存在编辑、SRT/ASS、波形、多轨、裁剪或视频渲染入口 | 通过 |
| 随包资源 | 单平台 Rust 暂存工具、模型及全部平台文件哈希、权限/符号链接门禁、macOS/Windows Tauri 覆盖与许可证清单 | macOS `.app` 资源与 ad-hoc 签名通过，Windows 及正式发行签名待目标环境 |

## 阶段测试命令

基础阶段：

```bash
cargo test --all-targets
cargo test --manifest-path src-tauri/Cargo.toml --all-targets
npm test
```

真实 VAD：

```bash
make asr-test-vad ASR_RESOURCE_ROOT=/absolute/path/to/resources/asr
```

真实 Whisper：

```bash
ASR_RESOURCE_ROOT=/absolute/path/to/resources/asr \
cargo test --offline --test asr_whisper_integration -- --ignored --nocapture
```

单视频完整阶段：

```bash
make asr-test-cli \
  ASR_RESOURCE_ROOT=/absolute/path/to/resources/asr \
  ASR_TEST_VIDEO=/absolute/path/to/video.mp4
```

直接观察某个停止点：

```bash
ASR_RESOURCE_ROOT=/absolute/path/to/resources/asr \
cargo run --offline -- asr /absolute/path/to/video.mp4 --stop-after probe --json

ASR_RESOURCE_ROOT=/absolute/path/to/resources/asr \
cargo run --offline -- asr /absolute/path/to/video.mp4 --stop-after audio --json

ASR_RESOURCE_ROOT=/absolute/path/to/resources/asr \
cargo run --offline -- asr /absolute/path/to/video.mp4 --stop-after vad --json
```

## 已验证真实样本

固定中文短视频 `tests/fixtures/asr/short_zh.mp4` 的锁定模型输出包含：

```text
欢迎来到直播间，今天的价格是99元。
```

该结果证明 FFprobe、FFmpeg、Silero VAD、Metal `whisper.cpp`、结构化 JSON、中文规范化和清理链路可以协同工作。它不能替代真实直播质量基准。

### 当前开发机参考性能

2026-07-22 在 Apple Silicon、36 GB 物理内存、12 个逻辑核心的开发机上，以 release 主程序、Metal Whisper、最多 4 个线程处理上述 `4191 ms` 固定视频：

- 墙钟时间：`1.92 s`；
- 处理实时因子：约 `0.46`；
- `/usr/bin/time -l` 最大常驻集：`504,266,752` 字节；
- macOS peak memory footprint：`193,970,704` 字节；
- swap：`0`；
- 输出置信度：约 `0.9073`。

这只是短视频、模型已在本机文件缓存中的开发机参考值。该机器不是 8 GB 目标设备，也没有覆盖长直播温度、持续稳定性和输入结束后的长期资源回收，因此不能据此勾选双平台 8 GB 性能任务。

### macOS arm64 目标行为

当前 Apple Silicon 目标机已验证：

- Metal `whisper-cli` 能加载锁定的 `small-q5_1` 并生成 full JSON；
- 10 分钟输入进入真实识别阶段后发送 Ctrl-C，主程序返回“语音识别已取消”，临时目录无文件残留；
- 将 arm64 Mach-O sidecar 执行 ad-hoc SHA-256 签名后，`codesign --verify --strict` 通过，签名后的二进制仍可使用 Metal 加载模型并输出中文结构化结果。

该证据完成开发目标机的签名后执行验证，但 ad-hoc 签名不等于 Developer ID、notarization 或安装后 Gatekeeper 验收；正式发行任务仍保持未完成。

当前机器 `security find-identity -p codesigning` 返回 `0 valid identities found`，没有可用于正式发行的 Developer ID 身份。Tauri 能生成新的 `.app`，但 DMG 脚本在当前执行环境两次失败且没有生成可验收 DMG；因此不能提交 notarization，也不能勾选 9.4。当前机器为 36 GB Apple M3 Pro，也不能代替 8 GB 目标机性能验收。

### macOS `.app` 资源包

已从锁定源码构建静态链接 Whisper/GGML、内嵌 Metal shader 的 `whisper-cli` 和 CPU VAD；二者只依赖 macOS 系统框架，不携带绝对 RPATH。FFmpeg 8.1.2 使用禁用网络与 GPL/nonfree 的 LGPL 配置，并改写、签名 7 个包内 dylib。Tauri `.app` 实际包含：主程序、静态 Metal `whisper-cli`、CPU VAD、FFmpeg/FFprobe、FFmpeg dylib、small 模型、VAD 模型、单平台封存 manifest、规范化字典、第三方清单和 5 份完整许可证文本。

审计曾发现早期动态 `whisper-cli` 保留了指向开发机构建目录的绝对 RPATH，导致旧的包内测试可能借用外部 `libwhisper`/`libggml`。该产物已废弃并由静态构建替换；`asr-bundle verify` 现在同时拒绝包外动态依赖和绝对 RPATH。

对新 `.app` 执行 ad-hoc 深度签名后，`codesign --verify --deep --strict` 通过；对 `Contents/Resources/resources/asr` 执行包含全部平台文件哈希和 Mach-O 可移植性检查的 `asr-bundle verify` 通过。随后临时隐藏旧 Whisper 构建目录，仅以 `.app` 包内资源执行完整 ASR，仍输出“欢迎来到直播间，今天的价格是99元。”。这证明 `.app` 资源独立可运行，但没有替代 Developer ID、DMG 安装、notarization、stapling 和 Gatekeeper 测试。

## 发行环境验收

以下项目必须在真实发行环境保留证据，仓库内自动化不能替代：

### macOS arm64

在 Developer ID 签名、notarization、stapling 和安装完成后，断开默认网络路由并执行：

```bash
make asr-verify-release-macos \
  ASR_APP='/Applications/直播管家.app' \
  ASR_DMG='/secure/release/直播管家.dmg' \
  ASR_VIDEO='/secure/fixtures/short_zh.mp4' \
  ASR_RELEASE_EVIDENCE='/secure/evidence/macos-release.json'
```

[`verify-asr-release-macos.sh`](../../scripts/verify-asr-release-macos.sh) 同时检查 Developer ID、hardened runtime、全部 Mach-O sidecar、DMG/app stapling、Gatekeeper、DMG 完整性、`/Applications` 安装位置、包内资源哈希、无默认路由时的真实 ASR、原视频未变化和 GUI 启动。只有输出 `allPassed=true` 才能作为任务 9.4 的仓库外证据。

- Developer ID 对主程序、`whisper-cli`、VAD、FFmpeg 和 FFprobe 签名；
- hardened runtime、notarization、stapling 和 Gatekeeper 检查；
- 安装后的资源路径、Metal 识别、取消和完全离线运行；
- 8 GB 目标机的实时因子、峰值统一内存、线程、温度与资源释放。

### Windows x64

2026-07-22 在 Apple Silicon 开发机使用 `cargo-xwin 0.23.0`、MSVC 17 SDK/CRT、Rust
`llvm-tools-preview` 和 LLVM 22，对根 crate 与 Tauri crate 的 `x86_64-pc-windows-msvc`
全目标 `cargo check` 及 `clippy -D warnings` 均通过。该证据覆盖 Windows 条件编译、Tauri
Windows 依赖和资源编译脚本的静态构建边界，但没有运行生成物，因此不勾选 Windows 目标机
识别或正式安装包任务。

在 Authenticode 签名和时间戳完成后，先通过真实 SmartScreen 界面检查并保存内部证据编号；随后禁用网络适配器，在中文 Windows 用户配置目录中运行：

```powershell
make asr-verify-release-windows `
  ASR_INSTALLER="C:\Release\直播管家-setup.exe" `
  ASR_INSTALL_DIR="$env:LOCALAPPDATA\直播管家" `
  ASR_VIDEO="C:\ASR验收\short_zh.mp4" `
  ASR_SIGNER_THUMBPRINT="0123456789ABCDEF0123456789ABCDEF01234567" `
  ASR_SMARTSCREEN_EVIDENCE="release-ticket-2026-001" `
  ASR_RELEASE_EVIDENCE="C:\ASR验收\windows-release.json"
```

[`verify-asr-release-windows.ps1`](../../scripts/verify-asr-release-windows.ps1) 会执行静默安装、校验安装器/主程序/卸载器/sidecar/dll 的 Authenticode 与时间戳、Microsoft VC++ 安装器签名、包内资源哈希、VC++ 和 WebView2 注册状态、无活动网络适配器、中文用户配置目录、GUI 启动、中文空格路径离线 ASR、源文件不变和静默卸载。SmartScreen 本身保留人工证据 ID；只有报告 `allPassed=true` 才能作为任务 9.2/9.5 的候选证据。

- 在目标机执行 `tests/asr_whisper_windows_adapter.rs`；
- 使用 `make asr-test-windows-target ASR_RESOURCE_ROOT=... ASR_TARGET_EVIDENCE=...` 一次执行真实 CPU Whisper、Unicode/空格路径、结构化输出、运行中取消、SSE4.2/VC++ manifest 和源哈希验收；
- Authenticode 对主程序、sidecar 和 NSIS 安装器签名并加入时间戳；
- SmartScreen、VC++ x64、离线 WebView2、安装/卸载与中文用户目录；
- CPU 指令基线、取消、Unicode/空格路径、缺失运行库错误和离线识别；
- 8 GB 目标机的实时因子、峰值内存、线程、温度与资源释放。

### 中文直播质量

仓库已提供 `asr-quality collect/evaluate`、10 样本数据集模板和双平台性能采集脚本。工具会封存数据集、输入、ASR 二进制及资源 manifest 哈希，拒绝覆盖既有证据，并且不保存视频路径或 stderr 原文。使用方法见 [`ASR-真实样本与性能验收.md`](ASR-真实样本与性能验收.md)。这些工具只是执行入口；当前尚未取得 10 场授权直播和双平台 8 GB 目标机报告，因此 9.8、9.9 仍保持未完成。

至少选择 10 场有合法处理权限的中文直播，记录：

- 商品名、金额和主播名召回率；
- 句段时间误差；
- 静音/音乐幻觉率；
- 输入失败率、处理实时因子和资源释放；
- 热词开启前后的差异。

当前固定短句效果可读，但没有足够真实直播数据证明 small 量化模型达到产品质量门槛。如果上述样本显示商品名、金额或整体可读性不达标，应保留本地结果和评测数据，并新建“用户明确授权的云端 `AsrEngine` Adapter”变更，单独设计供应商、费用、上传确认、凭据和数据保留；本变更禁止自动上传或自动云端回退。

## 许可证与供应链

正式发行使用 `THIRD_PARTY_NOTICES.md` 作为组件清单，并归档 manifest、哈希、版本输出和构建配置。FFmpeg 产物包含 `--enable-nonfree` 时禁止分发；包含 `--enable-gpl` 时必须按 GPL 履行源代码提供义务。构建工具拒绝符号链接，避免把 Homebrew 或 CI 绝对路径带进安装包。

签名会改变 Mach-O 或 PE 文件字节，因此正式顺序必须是：先签名平台 sidecar 和动态库，再执行 `asr-bundle stage` 封存签名后哈希，随后生成并签名外层应用/安装器，最后再次运行包内资源校验。不得在封存后重新签名 sidecar 而不刷新 manifest。
