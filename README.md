# 直播管家（dy-screen）

直播管家是一个基于 Tauri 2.0、React、TypeScript、Rust 和 SQLite 的本地桌面客户端，用于通过公开抖音个人主页或直播间入口同时监听多个主播，在开播后自动保存包含视频和声音的 MKV 分片，并允许用户主动把多个视频转换为带时间戳的本地语音转写。

当前版本交付“可靠录制 + 用户触发的本地 ASR”能力。AI 剪辑工作区已经支持视频与只读文本时间轴联动；NLP/LLM 高光判断、自动切片和成品视频导出仍属于后续独立能力。

## 已实现功能

- 添加监控主播：输入公开抖音个人主页或直播间链接；个人主页名称可留空并使用页面昵称补全，直播间直连仍要求填写名称；
- 个人主页未开播时允许先保存，后台持续等待首次开播；
- 使用 `profile_sec_uid`、稳定 `web_rid` 和当前 `room_id` 区分主页身份、长期直播入口和本次直播场次；
- 首次发现直播入口后持久化标准化 `https://live.douyin.com/{web_rid}`，随后复用现有直播解析与录制链路；
- 规范化来源链接、真实访问公开页面、提取身份并阻止重复主页或稳定直播入口；
- 使用 SQLite 保存主播、监听状态、设置、录制会话和视频分片；
- 应用启动后自动恢复之前开启的监听任务；
- 尚未发现直播入口的个人主页约每 60 秒加 0–10 秒抖动检查，错误按 60、120、300 秒退避；
- 已发现直播入口后每 30 秒检查一次；只有连续 3 次明确入口失效才回查个人主页，普通离线和网络错误不会清除稳定入口；
- 检测到开播后自动启动 FFmpeg，直播结束后关闭本次录制会话；
- FFmpeg 退出后重新确认房间状态；仍在直播时在同一逻辑会话中最多续录 3 次，只有明确未开播才结束会话；
- 默认最多同时录制 4 个直播间，可在设置页修改；
- 录像默认保存到 `~/Downloads/dy-screen/`，允许指定其他目录；
- 录制期间增量监听完成清单，MKV 分片一旦正确关闭就立即登记，不等待整场直播结束；
- 展示直播状态、监听状态、本次视频数量和历史视频数量；
- 提供本次监听视频和历史视频库，历史视频按录制会话分组；
- 在监控中心和视频库中使用内置播放器预览已完成分片，同时保留系统播放器打开和 Finder/文件管理器定位；
- MKV 首次预览时按需生成 MP4 缓存：优先无损重封装，编码不兼容时自动回退为 H.264 + AAC；
- 支持删除单个视频或整个已结束会话；删除失败时恢复数据库状态并显示错误；
- 关闭主窗口后驻留系统托盘，监听和录制继续运行；
- 托盘提供打开窗口、暂停全部、恢复全部和退出；
- 提供 FFmpeg/FFprobe 环境诊断、系统通知、日志目录和可选开机启动；
- 磁盘低于 10 GB 时警告，低于 2 GB 时不启动新录制，低于 1 GB 时安全停止活动录制；
- AI 剪辑工作区支持创建项目、通过系统文件选择器导入多个本地视频，或选择一场已结束直播并按顺序展开全部完成分片；
- 只有用户点击“开始分析”后才运行 ASR，打开页面、应用启动和新录像完成都不会自动识别；
- 本地流水线使用 FFprobe、FFmpeg、Silero VAD 和 `whisper.cpp small-q5_1`，macOS arm64 使用 Metal，Windows x64 使用 CPU；
- ASR 全局单并发并等待活动录制结束，不占用录制并发许可；相同源版本与识别配置可以跨项目复用稳定转写产物；
- 项目结果提供播放器、点击句段跳转、当前句段高亮、可关闭的跟随播放、仅存在于 WebView 的临时字幕、复制及 TXT/JSON 导出；
- 第一版转写不可编辑，不生成 SRT/ASS，不修改或烧录原视频，也不提供波形、多轨、裁剪或视频渲染入口；
- 所有主播、设置、视频元数据均保存在本机，不包含云同步和遥测。

## 录制方式

本项目采用“直播源直录”，不是桌面截图式录屏。Rust 后端解析公开直播页中的 FLV/HLS 地址，再通过 FFmpeg 使用 `-c copy` 保存原始视频和音频。

这种方式具有以下特点：

- 不需要让浏览器窗口保持可见；
- 不会录入鼠标、桌面通知或其他窗口；
- 不进行视频转码时 CPU 占用较低；
- 保存直播源本身的视频和声音；
- 不包含弹幕、礼物动画和网页控件。

如果产品必须保留直播页面 UI、弹幕或礼物动画，需要另外实现系统级屏幕捕获方案。

## 技术结构

```text
dy-screen/
├── src/                         Rust 直播解析、FFmpeg 录制和多任务核心
├── tests/                       录制核心测试与页面 fixture
├── ui/                          React + TypeScript + Vite 客户端界面
│   └── src/
│       ├── App.tsx              监控中心、视频库、设置和 AI 工作区入口
│       ├── AiWorkspace.tsx      AI 项目、播放器与只读时间戳文本界面
│       ├── api.ts               Tauri command 与浏览器演示适配
│       └── styles.css           参考图风格和响应式主题
├── src-tauri/                   Tauri 2.0 桌面后端
│   ├── src/database.rs          SQLite migration 和 repository
│   ├── src/supervisor.rs        监听 worker、自动录制、重试和磁盘保护
│   ├── src/preview.rs           预览队列、FFmpeg 转换、缓存和状态模型
│   ├── src/ai/                  AI 项目、命令、调度、恢复和本地 ASR 运行时
│   ├── src/app.rs               command、事件、托盘和桌面生命周期
│   └── tests/                   数据库与状态机测试
├── openspec/                    中文 OpenSpec 规格和变更
├── Makefile                     开发、构建、测试和 CLI 命令
└── README.md                    本文档
```

前端不直接执行 SQL。所有数据读写、文件操作和录制控制都通过类型化 Tauri command 进入 Rust 后端。

## 环境要求

录制与桌面能力当前优先在 macOS 开发；本地 ASR 的首版发行目标明确为 macOS arm64 和 Windows x64。Intel Mac、Windows ARM64 和 Linux ASR 不在本版支持范围。

- Node.js 20 或更高版本；
- npm；
- Rust stable 和 Cargo；
- FFmpeg，同时需要 FFprobe；
- GNU Make 或兼容的 `make`；
- macOS 构建 Tauri 时需要 Xcode Command Line Tools。

macOS 使用 Homebrew 安装示例：

```bash
xcode-select --install
brew install node rustup ffmpeg
rustup toolchain install stable --profile minimal
rustup default stable
```

如果终端找不到 Cargo：

```bash
export PATH="/opt/homebrew/opt/rustup/bin:$HOME/.cargo/bin:$PATH"
```

Makefile 会自动尝试 `$HOME/.cargo/bin/cargo` 和 Homebrew Rustup 路径，也可以显式指定：

```bash
make doctor CARGO="$HOME/.cargo/bin/cargo"
```

## 快速开始

进入项目目录：

```bash
cd /opt/yino/python/douyin
```

检查环境：

```bash
make doctor
```

安装前端依赖：

```bash
make install
```

启动 Tauri 桌面客户端：

```bash
make app-dev
```

首次启动后：

1. 点击“添加主播”；
2. 可输入便于识别的主播名称；个人主页可留空；
3. 输入公开个人主页，例如 `https://www.douyin.com/user/...`，或直播间链接，例如 `https://live.douyin.com/452086788686`；
4. 个人主页可以不填名称，直播间直连需要填写名称；
5. 保持“添加后立即监听”开启；
6. 保存后，后台会立即检查一次主页或直播状态；
7. 主播开播时自动录制，下播时自动结束会话。

直播间可能随时下播，示例地址只用于展示格式。

## AI 剪辑工作区与本地 ASR

第一版 AI 工作区的输入和输出边界是：

```text
多个本地视频 / 一场已结束直播
              ↓
       用户确认并点击开始
              ↓
FFprobe → FFmpeg → Silero VAD → AsrEngine
                                  ↓
                     WhisperCppEngine Adapter
                                  ↓
         稳定句段 ID + 源内/项目时间戳 + 文本
                                  ↓
         播放器联动浏览、复制、TXT/JSON 导出
```

使用步骤：

1. 打开“AI 剪辑”，创建一个草稿项目；
2. 通过系统文件选择器一次导入多个视频，或选择一个已经结束的直播会话；
3. 在草稿中查看音轨、时长和错误，拖动排序或移除不需要的视频；
4. 确认环境诊断通过后点击“开始分析”；
5. 在左侧选择视频，在右侧播放器和时间戳文本列表中浏览结果；
6. 点击句段可跳转到源视频时间；“跟随播放”只控制列表滚动，“显示字幕”只在当前 WebView 临时覆盖；
7. 可复制单句、当前视频或整个项目，也可导出包含稳定定位信息的 TXT/JSON。

本地导入不会把文件上传到网络，也不会复制原视频。项目只保存受信路径引用、源指纹、状态和转写产物；外部视频被移动、删除或修改后，对应输入会独立失败。选择直播会话时只接受已结束会话，不读取仍在写入的录像分片。

`AsrEngine` 是业务层依赖的中立接口，输入包含准备后的音频、语言提示、热词和时间戳策略，输出包含引擎/模型身份、语言、句段时间、原始文本、可选置信信息和警告。第一版只有 `WhisperCppEngine`，后续增加云端或其他本地引擎时应新增 Adapter，不修改项目、缓存、时间轴和 UI 契约；本版不会自动上传或自动回退云端。

### ASR 资源与性能

- 最低基线为 8 GB 物理内存，任务前还会检查当前可用内存和至少 2 GB 临时磁盘余量；
- 默认模型是约 181 MiB 的多语言 `small-q5_1`，实际运行还需要模型工作区、音频缓冲和系统资源；
- ASR 全局最多运行一个输入，并限制为最多 4 个线程；存在活动录制时，新 ASR 输入保持等待；
- 每个输入结束后退出 `whisper.cpp` 子进程并释放模型资源，因此多视频之间会重复加载模型，但取消和故障隔离更清晰；
- VAD 用于排除静音、挂机和纯音乐，不能保证所有背景音乐场景都被正确过滤；
- 中文商品名、主播名和金额仍可能识别错误，可以在项目中配置热词。未经真实样本核验的转写不应被当作事实记录。

环境诊断失败时不会创建运行任务。模型或 sidecar 缺失、损坏时，第一版不在线下载，也不允许用户指定任意可执行路径；请使用完整安装包修复或重新安装。
正式暂存时会为 Whisper、VAD、FFmpeg、FFprobe、动态库和 Windows VC++ 安装器封存文件大小与 SHA-256；运行时要求单平台 manifest 完整覆盖全部文件，任一文件被替换都会在启动任务前失败。

真实中文直播质量与 8 GB 双平台性能使用独立证据工具验收：`asr-quality collect/evaluate` 记录至少 10 个授权样本的召回率、时间误差、静音幻觉、失败率和 RTF；macOS/Windows 性能脚本记录峰值内存、线程、热状态和子进程释放。完整字段、隐私边界和命令见 [`docs/wiki/ASR-真实样本与性能验收.md`](docs/wiki/ASR-真实样本与性能验收.md)。模板或开发机数据不能替代真实目标设备证据。

## 仅预览界面

如果只想查看 React 页面，不启动 Tauri 后端：

```bash
make web-dev
```

然后打开：

```text
http://localhost:1420/
```

浏览器预览使用 `localStorage` 模拟主播和设置，不能解析真实直播状态，也不能启动 FFmpeg。真实监听和录制必须使用 `make app-dev`。

## 构建桌面应用

普通开发构建不强制包含大模型，适合 UI、数据库和非真实 ASR 测试：

```bash
make app-build
```

默认产物位于：

```text
src-tauri/target/release/bundle/macos/直播管家.app
```

正式 ASR 安装包必须先准备一个符合 `resources/asr/manifest.json` 结构的可信资源目录。该目录包含公共模型、VAD、规范化字典和至少一个目标平台的 `whisper.cpp`、VAD、FFmpeg、FFprobe；Windows 还包含官方 `vc_redist.x64.exe`。构建工具不会联网，也拒绝把指向 Homebrew 或开发机路径的符号链接放进安装包。

仓库中的 manifest 是跨平台模板；`asr-bundle stage` 会为所选平台生成只包含一个平台且完整封存全部原生文件哈希的发行 manifest。模板目录不能直接作为正式运行资源。

macOS 的 ASR 专用 FFmpeg 应从锁定的官方 `ffmpeg-8.1.2.tar.xz` 构建：

```bash
make asr-ffmpeg-macos \
  FFMPEG_SOURCE=/absolute/path/to/ffmpeg-8.1.2.tar.xz \
  FFMPEG_ASR_OUTPUT=/absolute/path/to/output
```

脚本校验源码 SHA-256，只启用常见本地容器、音频解码和 PCM WAV，禁用网络、GPL/nonfree 与第三方编码器；输出使用 LGPL-2.1-or-later，携带完整许可证，并把 dylib 改写为包内相对路径。Homebrew 常规 FFmpeg 启用了 GPL 外部组件且依赖开发机动态库，不能直接复制进正式安装包。

macOS Whisper sidecar 应使用静态、可移植构建：

```bash
make asr-whisper-macos \
  WHISPER_SOURCE=/absolute/path/to/whisper.cpp-v1.9.1.tar.gz \
  WHISPER_ASR_OUTPUT=/absolute/path/to/output
```

脚本锁定源码 SHA-256 和提交，静态链接 Whisper/GGML、内嵌 Metal shader，并拒绝包外动态库和绝对 RPATH。不能直接复制仍引用 CMake 构建目录的动态 `whisper-cli`。

macOS arm64：

```bash
make asr-build-macos ASR_SOURCE=/absolute/path/to/asr-resources
```

Windows x64（在 Windows x64 构建机运行）：

```powershell
make asr-build-windows ASR_SOURCE=C:\absolute\path\to\asr-resources
```

Apple Silicon 开发机可用 `make asr-check-windows` 对根 crate 和 Tauri crate 执行 Windows
x64 全目标交叉编译与严格 Clippy；该检查不能替代 Windows 实机识别、签名和安装验收。

Windows x64 构建机可在 Visual Studio 2022 Developer PowerShell 中运行
`make asr-whisper-windows`，生成静态 CPU sidecar。脚本固定 SSE4.2 最低指令集并显式关闭
AVX/AVX2/BMI2，避免构建机 CPU 自动优化导致安装后非法指令崩溃。

两个命令先调用 Rust `asr-bundle` 工具校验并生成 `resources/asr-stage/`，再使用对应 Tauri 配置覆盖构建。macOS 覆盖生成 `.app` 与 `.dmg`；Windows 覆盖生成 NSIS 安装器并使用离线 WebView2 安装模式。正式发行仍必须在各自目标机完成签名、公证或 Authenticode、安装、卸载和离线 ASR 验收，不能用开发构建代替发行证据。

## 自动监听与录制逻辑

每个启用监听的主播最多拥有一个长期 worker。个人主页来源采用两阶段状态机：

```text
启动/恢复监听
    ↓
是否已经发现稳定 web_rid？
    ├── 否：检查公开个人主页
    │       ├── 未发现入口：60 秒 + 0–10 秒抖动后重试
    │       ├── 暂时失败：60/120/300 秒退避
    │       └── 首次发现：先保存 web_rid/room_url/room_id，再立即检查直播间
    └── 是：每 30 秒检查标准化直播间
            ├── 未开播：保留稳定入口
            ├── 网络失败：退避但不回查主页
            ├── 连续 3 次入口失效：清除直播绑定并回查主页
            └── 正在直播
            ↓
       等待全局录制许可
            ↓
       再次解析最新签名地址
            ↓
       检查磁盘并创建逻辑会话
            ↓
       FFmpeg 录制 MKV 分片
            ├── FFmpeg 退出且房间仍在线：同一会话内继续录制
            ├── 再次解析明确未开播：关闭会话，等待下一次开播
            ├── 用户暂停/退出：安全停止并保留完成分片
            └── 异常断流：重新解析并最多续录 3 次
```

直播状态和监听状态分开保存：

- 直播状态：检查中、未开播、直播中、检查失败；
- 监听状态：已暂停、正在发现直播间、等待首次开播、主页检查失败、重新发现直播间、等待开播、等待资源、录制中、正在重试、录制异常。

因此，“主播未开播”和“应用没有监听”不会被混为同一个状态。

## 本地数据位置

### 录像目录

默认：

```text
~/Downloads/dy-screen/
```

可以在设置页修改。修改只影响后续新会话，历史文件不会自动搬迁。

录制核心的目录结构类似：

```text
~/Downloads/dy-screen/
└── <room-id>/
    └── <recording-session>/
        ├── 20260718-180000.mkv
        ├── 20260718-181500.mkv
        └── segments.csv
```

FFmpeg 只能在合适的关键帧处切分，因此实际时长可能略大于设置的分片秒数。

### SQLite

macOS 默认位于 Tauri 应用数据目录：

```text
~/Library/Application Support/com.yino.dyscreen/dy-screen.sqlite3
```

主要表：

- `streamers`：来源类型、来源 URL、`profile_sec_uid`、`web_rid`、当前 `room_id`、监听开关及状态；
- `recording_sessions`：每次直播周期的逻辑会话；
- `videos`：完成 MKV 分片的路径、大小、音频和文件状态；
- `ai_projects`：用户主动创建的 AI 项目、冻结识别配置、状态和总体进度；
- `ai_project_inputs`：有序本地视频或直播分片引用、源指纹、源内时长和项目时间偏移；
- `asr_artifacts`：按源指纹和识别配置指纹发布、复用和失效的 ASR 产物；
- `transcript_segments`：稳定句段 ID、源内时间戳、原始/规范化文本和可选置信信息；
- `settings`：录像目录、质量、协议、并发、FFmpeg 和桌面设置；
- `schema_migrations`：数据库迁移版本。

### 日志

macOS 默认日志目录：

```text
~/Library/Logs/com.yino.dyscreen/
```

设置页可以直接打开日志目录。应用启动时只清理超过 14 天的普通日志文件，不会删除录像或数据库。

## 关闭窗口和退出

- 点击主窗口关闭按钮只会隐藏窗口；
- 后台 worker、直播检查和活动录制继续运行；
- 点击托盘“打开主窗口”可以恢复并聚焦窗口；
- 托盘支持暂停全部和恢复全部监听；
- 显式退出且存在活动录制时，主窗口会要求确认；
- 确认后，后端先取消活动录制并等待完成分片收尾，超时后才会结束任务；
- 所有 worker 共享一个全局 10 秒退出期限，超时任务会终止并执行 SQLite 会话对账，退出耗时不会随主播数量线性增加。

## 视频预览与 MKV 打开方式

客户端的“本次监听视频”和“视频库”提供三种操作：

- “预览”：在客户端内置播放器中播放单个已完成分片；
- “系统打开”：调用系统默认播放器打开原始文件；
- “定位”：在 Finder 或文件管理器中显示文件。

Tauri WebView 不能稳定直接播放 MKV，因此第一次点击 MKV 的“预览”时，后端会在应用缓存目录中准备 MP4：

1. 优先使用 `-c copy` 无损重封装，速度快且不损失画质；
2. 如果音视频编码不适合 WebView，则自动转为 H.264 + AAC；
3. 后续预览相同源文件时直接复用缓存；
4. 原始 MKV 始终保留，并继续用于归档、ASR 和后续剪辑。

预览任务全局最多运行一个，避免与多直播间录制争抢过多 CPU 和磁盘。关闭播放器不会取消正在进行的转换。

macOS 的预览缓存默认位于：

```text
~/Library/Caches/com.yino.dyscreen/video-preview/
```

缓存超过 7 天未访问会被清理，总容量默认限制为 10 GB。缓存清理不会删除原始录像；在应用内删除视频或整个会话时，对应预览缓存也会一并清理。

如果 FFmpeg/FFprobe 缺失、磁盘空间不足或文件损坏，播放器会显示中文错误，并提供重试或系统播放器回退入口。

macOS 可以使用 IINA 或 VLC：

```bash
brew install --cask iina
```

也可以直接使用 FFplay：

```bash
ffplay '/完整路径/视频.mkv'
```

MKV 适合直播录制，因为进程异常时通常比 MP4 更容易保留已经完成的内容。后续如需 MP4，可在录制结束后重封装，不必重新编码：

```bash
ffmpeg -i input.mkv -c copy output.mp4
```

## Makefile 命令

查看帮助：

```bash
make help
```

| 命令 | 用途 |
| --- | --- |
| `make doctor` | 检查 Node、npm、Cargo、FFmpeg 和 FFprobe |
| `make install` | 安装前端依赖 |
| `make web-dev` | 启动浏览器界面预览 |
| `make typecheck` | 执行 React/TypeScript 类型检查 |
| `make frontend-build` | 类型检查并构建 React 前端 |
| `make app-dev` | 启动 Tauri 桌面开发客户端 |
| `make app-build` | 构建桌面应用 |
| `make asr-ffmpeg-macos FFMPEG_SOURCE=...` | 从锁定官方源码构建 LGPL、无网络、可相对定位的 macOS ASR FFmpeg |
| `make asr-stage-macos ASR_SOURCE=...` | 校验并准备单平台 macOS arm64 ASR 随包目录 |
| `make asr-build-macos ASR_SOURCE=...` | 构建包含本地 ASR 资源的 macOS `.app`/`.dmg` |
| `make asr-stage-windows ASR_SOURCE=...` | 校验并准备单平台 Windows x64 ASR 随包目录 |
| `make asr-build-windows ASR_SOURCE=...` | 构建包含 VC++/WebView2 离线安装能力的 Windows NSIS 安装器 |
| `make asr-check-windows` | 交叉检查 Windows x64 根/Tauri crate 与严格 Clippy |
| `make asr-test-windows-target ...` | 在真实 Windows x64 设备执行 CPU、Unicode、运行中取消、VC++ 与真实中文 ASR 验收 |
| `make asr-verify-release-macos ...` | 验证已安装 macOS 包的 Developer ID、公证、Gatekeeper、包内资源与离线 ASR |
| `make asr-verify-release-windows ...` | 安装并验证 Windows 包的 Authenticode、SmartScreen 证据、运行库、中文目录、离线 ASR 与卸载 |
| `make asr-quality-collect ...` | 顺序采集至少 10 个授权中文直播样本并生成脱敏证据 |
| `make asr-quality-evaluate ...` | 从不可变样本证据生成 JSON 与 Markdown 质量报告 |
| `make asr-performance-macos ...` | 在真实 8 GB macOS arm64 设备采集 RTF、内存、线程和资源释放 |
| `make asr-performance-windows ...` | 在真实 8 GB Windows x64 设备采集 RTF、内存、线程和资源释放 |
| `make asr-evidence-audit ...` | 交叉审计六项外部门禁，仅在完整一致的真实证据下输出 `readyToComplete=true` |
| `make fmt` | 格式化根 crate 和 Tauri crate |
| `make fmt-check` | 检查 Rust 格式 |
| `make lint` | 对两个 Rust crate 执行 Clippy 严格检查 |
| `make test-frontend` | 执行 React 组件测试 |
| `make test-core` | 执行录制核心测试 |
| `make test-app` | 执行 SQLite、supervisor 和预览服务测试 |
| `make preview-doctor` | 检查 FFmpeg/FFprobe 和可用 H.264 编码器 |
| `make test-preview` | 执行预览 Rust 测试和播放器组件测试 |
| `make test-preview-integration` | 使用真实 FFmpeg 样本验证重封装、回退转码和无音轨视频 |
| `make asr-test-contract` | 单独验证中立 `AsrEngine` 契约、资源、错误、取消和调度边界 |
| `make asr-test-media` | 单独验证 FFprobe、FFmpeg、临时音频和原视频不变 |
| `make asr-test-vad ASR_RESOURCE_ROOT=...` | 使用封存资源单独运行真实人声、静音和纯音乐 VAD |
| `make asr-test-whisper ASR_RESOURCE_ROOT=...` | 单独运行真实中文识别、结构化输出、取消和清理 |
| `make asr-test-cli ASR_RESOURCE_ROOT=... ASR_TEST_VIDEO=...` | CMD 验证 probe/audio/vad/asr 四个停止点并输出完整 ASR JSON |
| `make asr-test-stages ASR_RESOURCE_ROOT=...` | 顺序执行契约、媒体、VAD、Whisper 和 CMD 全部阶段 |
| `make test-profile` | 执行个人主页 fixture、URL 规范化和脱敏测试 |
| `make test-migration` | 执行三层身份 SQLite 迁移与唯一性测试 |
| `make test-supervisor-profile` | 执行个人主页/直播间双阶段状态机测试 |
| `make test` | 执行全部测试 |
| `make check` | 执行格式、Clippy、全部测试和前端构建 |
| `make spec-validate` | 严格校验全部 OpenSpec 主规格和活动变更 |
| `make verify` | 执行全量检查、OpenSpec 校验和桌面应用构建 |
| `make inspect-profile` | 只读检查公开个人主页及当前直播入口 |
| `make resolve` | 使用原 CLI 解析直播间 |
| `make record` | 使用原 CLI 录制单个直播间 |
| `make record-multi` | 使用原 CLI 同时录制多个直播间 |

## ASR 阶段命令行测试

根 crate 提供独立的 `asr` 子命令，便于不启动 Tauri 和 React 就验证单个视频的完整阶段：FFprobe 音轨探测、FFmpeg 16 kHz 单声道 WAV、Silero VAD、`WhisperCppEngine`、中文规范化和 JSON 输出。

```bash
ASR_RESOURCE_ROOT=/absolute/path/to/asr-resources \
cargo run --offline -- asr /absolute/path/to/video.mp4 --json
```

需要只诊断某一阶段时使用 `--stop-after probe|audio|vad|asr`；默认值为 `asr`，不会改变原有
完整识别命令。前三个停止点分别输出媒体探测、标准音频元数据和 VAD 人声区间 JSON，并且
不会暴露临时音频路径：

```bash
ASR_RESOURCE_ROOT=/absolute/path/to/asr-resources \
cargo run --offline -- asr /absolute/path/to/video.mp4 --stop-after vad --json
```

也可以增加热词：

```bash
ASR_RESOURCE_ROOT=/absolute/path/to/asr-resources \
cargo run --offline -- asr /absolute/path/to/video.mp4 \
  --hotword 主播名 --hotword 商品名 --json
```

命令只接受本地视频和受控资源根，输出句段的开始/结束毫秒、原始文本、规范化文本和可用时的置信信息。按 `Ctrl-C` 会取消 FFmpeg 或 Whisper 子进程并清理临时音频。生产 UI 不暴露资源根参数，也不能用它绕过随包资源诊断。

## 原 CLI 录制方式

桌面客户端之外，原 Rust CLI 仍可独立使用。

只读检查个人主页：

```bash
make inspect-profile \
  PROFILE_URL='https://www.douyin.com/user/...' \
  JSON=1
```

该命令只输出规范化主页身份、昵称、稳定 `web_rid`、标准直播间 URL 和当前 `room_id`，不会输出页面 HTML 或签名直播流地址。

解析直播间：

```bash
make resolve \
  ROOM_URL='https://live.douyin.com/452086788686' \
  QUALITY=HD1 \
  PROTOCOL=flv
```

录制单个直播间：

```bash
make record \
  ROOM_URL='https://live.douyin.com/452086788686' \
  OUTPUT=recordings \
  SEGMENT_SECONDS=900
```

同时录制多个直播间：

```bash
make record-multi \
  ROOM_URLS='https://live.douyin.com/ROOM_A https://live.douyin.com/ROOM_B'
```

按 `Ctrl-C` 后程序会先请求 FFmpeg 优雅退出，已经完成的 MKV 分片仍会保留。

## 开发与验证

执行全量检查：

```bash
make check
```

也可以分别执行：

```bash
npm test
npm run typecheck
npm run build
cargo test --all-targets
cargo clippy --all-targets -- -D warnings
cargo test --manifest-path src-tauri/Cargo.toml --all-targets
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings
openspec validate --all --strict
```

## 当前限制

- 录制、托盘、通知和文件管理器行为仍优先在 macOS 开发；Windows x64 的本地 ASR 适配器和安装配置已有自动化覆盖，但正式安装、签名、SmartScreen、中文用户目录与卸载仍需 Windows 实机发行验收；
- 普通开发构建继续允许使用设置中的 FFmpeg/FFprobe；正式 ASR 安装包通过平台覆盖配置携带独立 sidecar，发行流水线必须提供非符号链接、许可明确的可分发二进制；
- 抖音修改页面或 React Flight 数据结构后，解析器和 fixture 可能需要更新；
- 仅支持无需登录即可访问的公开个人主页和直播间；主页受风控、要求登录或页面结构变化时会显示可重试错误；
- 不支持验证码、Cookie 自动化、DRM、付费或私有直播间绕过；
- 不录制弹幕、礼物动画或网页 UI；
- 内置播放器一次只预览单个已完成分片，不提供整场分片合并、统一时间轴或无缝连播；
- 预览仅在需要时生成可清理 MP4 缓存，不会在每次录制结束后自动转换全部录像；
- AI 工作区第一版只生成和浏览只读时间戳文本；不支持人工编辑、SRT/ASS、说话人分离、LLM 高光评分、裁剪计划、视频拼接、字幕烧录或成品导出；
- 本地 ASR 只支持 macOS arm64 Metal 和 Windows x64 CPU；不支持 Intel Mac、Windows ARM64、Linux、CUDA/Vulkan、多模型切换、在线下载或任意模型路径；
- small 量化模型已通过固定中文短句测试，但至少 10 场真实中文直播的商品名、金额、主播名召回率和 8 GB 双平台长时性能仍需目标设备样本验收；效果不达标时应另立用户明确授权的云端 `AsrEngine` Adapter 变更，本版不自动上传或回退；
- Windows/Linux 的托盘、开机启动、通知和文件管理器行为仍需在对应平台验证；
- 生产发布前仍需完成 macOS Developer ID 签名与 notarization、Windows Authenticode 签名与时间戳，以及两个平台的干净机器离线安装验证。

### 真实页面验收边界

2026-07-21 使用公开示例主页执行了只读 HTTP 验收。请求可以到达抖音，但当时返回的是包含 `__ac_nonce`、`__ac_signature` 和 `byted_acrawler` 的访问控制引导页，没有公开主页身份或 React Flight 数据。客户端会把该响应识别为“需要登录、验证码或额外访问权限”，按个人主页错误退避重试，不保存页面内容，也不尝试绕过。

脱敏 fixture、SQLite 迁移、重启恢复、首次发现、直播间 resolver 接管、录制会话创建、入口连续失效回查和界面状态均已在 macOS 开发环境通过自动化测试。当前公开示例主页的真实“发现直播入口”步骤仍取决于平台是否再次提供无需登录和风控脚本的公开 HTML。Windows/Linux 的托盘、通知、文件管理器和真实网络差异尚待对应实机验证。

## 合规说明

仅应录制你有权保存和处理的内容，并遵守平台规则、版权要求、隐私要求及适用法律。本项目只请求普通公开 HTTP 页面，不导入 Cookie，不使用浏览器自动化，不尝试登录或处理验证码，也不会绕过权限控制、付费限制或 DRM。
