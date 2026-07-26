## Context

当前项目已经具备 `asr-bundle` staging、macOS/Windows Tauri 资源映射、`manifest.json` 和本地 ASR 完整性诊断，但两条路径仍然割裂：普通 `app-build` 没有媒体/ASR 原生资源，完整 ASR 构建依赖开发机的 `resources/asr-source/`；生产应用在资源缺失时只能提示重新安装，无法从用户提供的资源服务器修复。

本变更把 FFmpeg、FFprobe、Whisper sidecar、VAD sidecar、模型、规范化字典和平台运行时视为一个受版本控制的 Runtime Resource Pack。资源包既可以在发行构建时放进安装包，也可以在首次启动或资源损坏后从固定 HTTPS 渠道下载。用户必须完成资源准备和完整性校验后才能使用监控、录制、视频库和 AI 剪辑功能。

资源服务器地址由发行构建参数注入，后续使用用户提供的自有 HTTPS 地址；地址不是普通用户设置，也不接受前端运行时任意修改。资源包不上传视频、音频、转写、日志或用户数据。

## Goals / Non-Goals

**Goals:**

- 定义跨平台、可校验、可升级的 Runtime Resource Pack 和版本 manifest。
- 让 macOS arm64 与 Windows x64 的正式安装包优先携带当前平台全部必要资源。
- 在包内资源缺失、损坏或版本不匹配时，提供用户确认的 HTTPS 下载、断点续传、取消、重试、进度、校验和原子安装。
- 在资源完成前显示独立的资源准备界面，锁定主功能；资源准备完成后才初始化监听、录制、视频库和 AI runtime。
- 让录制、预览、封面和 ASR 使用资源管理器验证后的 FFmpeg/FFprobe 路径，不依赖开发机或用户配置的任意可执行文件。
- 支持单版本安装、失败回滚和启动时清理遗留临时包；应用重启后可从持久化状态恢复下载或重新校验。
- 在没有网络、下载失败或用户取消时安全停留在资源门禁，不破坏现有录像和数据库。

**Non-Goals:**

- 不实现通用动态插件扫描、插件市场、脚本插件、第三方运行时安装或运行时加载任意可执行文件。
- 不允许用户导入任意模型、替换 sidecar、修改资源服务器或绕过完整性校验。
- 不实现资源服务器管理后台、账号系统、付款、带宽计费或多租户权限。
- 不在运行时上传业务数据，也不把下载器用于直播流、视频、音频、ASR 文本或 LLM 请求。
- 不在本变更中完成真实 Developer ID/notarization、Windows Authenticode 或公网 CDN 部署；只提供构建和验收接口。

## Decisions

### 1. 采用“随包优先，缺失时下载”的资源包模型

官方发行包包含当前平台资源，以保证首次安装后可以离线启动。启动校验发现资源完整时不发起网络请求；只有资源缺失、损坏或版本低于应用要求时，才进入资源准备界面并允许下载。这样既保留离线可靠性，也避免因安装包体积或增量升级导致应用无法修复。

备选方案是只提供一个小 bootstrap 应用并始终在线下载。该方案首次启动强依赖网络、代理和 CDN 可用性，也增加用户无法判断失败原因的风险，因此不采用。

### 2. 使用签名 manifest 加逐文件哈希，而不是只信任 HTTPS

资源目录包含 `manifest.json`、压缩包或分片、许可证和资源文件。manifest 至少记录 `schemaVersion`、`bundleVersion`、目标 OS/架构、组件 ID/版本、相对路径、文件大小、SHA-256、包大小、下载 URL、许可证引用和最低应用版本。应用内嵌资源签名公钥，使用 Ed25519 验证 manifest 签名；下载完成后再校验归档哈希和每个解包文件哈希。

HTTPS 负责传输保密性和基本防篡改，签名负责抵御错误 CDN、缓存污染或地址误配。签名公钥只能随新应用版本更新，不接受来自远端 manifest 的公钥。

### 3. 资源安装使用应用数据目录和原子替换

资源安装根目录位于 Tauri `app_data_dir` 下，例如 `resources/<bundleVersion>/`。下载写入 `<bundleVersion>.part/`，每个文件先写临时文件并完成 fsync/哈希校验，再原子改名；全部组件验证通过后写入 `installed.json` marker，并更新 `current` 指针。旧版本只保留一个，只有未被活动进程使用时才清理。

应用包内资源在首次启动时可以直接作为只读资源使用；若需要下载更新版本，安装到 app data 后由后端优先解析已验证的 app data 资源。资源解析器不接受 WebView 提交的路径。

### 4. 使用受控下载器，不引入动态插件协议

Rust 核心增加 `ResourceCatalog`、`ResourceDownloader`、`ResourceInstaller` 和 `ResourceGate`：

```text
Tauri 启动
  └─ ResourceGate
       ├─ 读取内嵌/已安装 manifest
       ├─ 验证平台、版本、签名、大小、SHA-256、权限和磁盘空间
       ├─ 完整 -> 初始化 AppState 和主功能
       └─ 缺失/损坏 -> 资源准备 UI
              ├─ 用户确认 HTTPS 下载
              ├─ 断点续传到 .part
              ├─ 校验并原子安装
              └─ 重新诊断，成功后解锁主功能
```

下载器使用参数化 HTTP client、连接/读取超时、大小上限、取消 token 和单资源锁；禁止 shell、重定向到任意路径和下载后直接执行。下载状态、失败代码和当前版本写入 SQLite，敏感网络头和本地完整路径不进入普通日志。

### 5. 资源包组件边界

资源包按平台包含以下组件：

- `media.ffmpeg`、`media.ffprobe`：录制、预览、首帧封面和 ASR 音频准备共用的受控媒体工具；macOS 同时封存相对路径动态库。
- `asr.whisper`、`asr.vad-sidecar`：平台匹配的 `whisper.cpp` 和 VAD 可执行文件。
- `asr.model`、`asr.vad-model`、`asr.normalization`：small 多语言量化模型、Silero VAD 模型和 OpenCC 字典。
- `platform.windows.vc-runtime`：Windows x64 所需 VC++ Redistributable 安装资源，由安装器或资源诊断按平台策略处理。
- `licenses`：第三方组件、模型和运行库许可证及来源清单。

重复文件在资源 staging 阶段只保留一份，FFmpeg/FFprobe 的受控路径通过依赖注入同时服务录制和 AI runtime。开发模式可以使用显式环境变量覆盖资源根目录，但生产构建和普通设置不提供任意可执行路径入口。

### 6. 启动门禁和任务生命周期

Tauri 最小窗口先展示资源准备页，不创建主播 worker、不启动 FFmpeg、不打开视频库操作、不恢复 ASR 队列。只有 `ResourceGate::Ready` 后才构造 `AppState` 和 supervisor。应用启动时可以执行安全的 SQLite 迁移和资源状态读取，但不能让用户绕过门禁进入主界面。

资源下载或校验期间支持取消；取消后应用停留在资源门禁，退出时清理过期 `.part` 文件但保留可恢复下载元数据。下载失败不自动无限重试，用户每次显式点击重试。资源升级失败时继续使用仍然完整且满足最低版本的旧版本；没有可用旧版本时阻止进入主功能。

### 7. 发行构建和自有服务器协议

Makefile 增加资源打包、签名 manifest、生成归档/分片、验证包内资源和配置资源基础 URL 的目标。Tauri macOS/Windows 配置将平台资源映射到安装包；发行流水线产出 `.app/.dmg` 或 NSIS 安装包，以及可供下载器使用的资源归档、manifest、签名和许可证。

资源服务器只需要静态 HTTPS 文件服务，目录按 `channel/appVersion/platform/arch/bundleVersion/` 组织。应用先请求小型 index/manifest，再按 URL 下载单个归档或分片；服务器不需要理解用户身份，也不接收回传遥测。用户提供地址后只替换发行配置，不改变应用业务逻辑。

### 8. 兼容与数据库迁移

SQLite 新增 `runtime_resources` 和 `runtime_downloads` 表，记录平台、应用要求版本、已安装版本、状态、进度、错误代码、manifest 哈希和时间戳。旧数据库迁移默认创建空状态；首次启动先完成资源门禁，再恢复现有主播和录制数据。保留现有 `ffmpeg_path/ffprobe_path` 字段用于向前兼容和开发诊断，但生产录制路径改由已验证资源解析器提供。

## Risks / Trade-offs

- [资源包约 200 MB 以上，安装包和首次下载耗时增加] → 发行包仍提供完整离线资源；下载器支持断点续传和进度；UI 明确展示大小和磁盘要求。
- [资源服务器不可用会阻止整个应用使用] → 允许用户在资源完整时完全离线运行；保留一个满足最低版本的旧资源版本；失败只停在门禁，不损坏录像数据库。
- [统一 FFmpeg 版本可能与用户已有录制习惯不同] → 固定版本通过 manifest 诊断，保留旧设置字段但不在生产使用任意路径；发布前用录制、预览、封面和 ASR fixture 做兼容验收。
- [远端 manifest 或归档被替换] → 内嵌公钥验证签名，HTTPS、归档哈希和逐文件哈希三重校验，验证失败不安装不执行。
- [更新中断导致资源目录半成品] → `.part` 隔离、单写入锁、fsync、原子 marker 和启动清理；没有完整 marker 的目录永远不可被解析。
- [应用启动时锁定主功能影响用户查看历史录像] → 这是用户明确选择的强制下载策略；资源页面展示可操作诊断、下载/取消/重试和日志入口，不把错误伪装成直播或录制失败。
- [Windows VC++/WebView2 运行时安装失败] → 把运行库状态纳入平台 manifest 和安装器验收，提供离线安装资源及明确的中文错误，不启动 sidecar。

## Migration Plan

1. 增加资源 manifest v2、签名校验和平台资源包目录结构；保留旧 manifest 读取但不允许其解锁强制门禁。
2. 增加 SQLite 资源状态/下载迁移，并实现旧数据库启动时的空状态初始化。
3. 抽取 FFmpeg/FFprobe 资源解析接口，让录制、预览、封面和 ASR 共享经过校验的路径；开发覆盖保留在测试配置中。
4. 实现 Rust 下载器、断点续传、取消、原子安装和启动门禁，再接入 Tauri events/commands。
5. 增加资源准备页和应用初始化锁；成功后再初始化主播 supervisor、视频库和 ASR recovery。
6. 更新 macOS/Windows staging、Tauri bundle 配置和 Makefile，生成离线安装包与自有服务器静态目录。
7. 在无网络、断点、篡改、磁盘不足、旧版本回滚、中文路径和重复启动场景运行自动化测试；在 macOS arm64/Windows x64 真机验证安装后录制、预览、封面和 ASR。
8. 回滚时保留旧资源目录和数据库状态，关闭新门禁并恢复既有应用解析；不删除用户录像或 AI 数据。

## Open Questions

- 用户提供的资源服务器是否支持 HTTP Range、稳定的 `ETag`/`Last-Modified` 和大文件断点续传？
- 资源 manifest 的签名公钥由项目维护者生成并随应用版本发布，还是由资源服务器运营方提供后由项目固定？
- Windows VC++ Redistributable 是否允许随 NSIS 安装器静默安装，还是必须在资源页由用户手动确认？
