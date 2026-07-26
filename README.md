# 直播管家（dy-screen）

直播管家是一个基于 Tauri 2.0、React、TypeScript、Rust 和 SQLite 的本地桌面客户端，用于通过公开抖音个人主页或直播间入口同时监听多个主播，在开播后自动保存包含视频和声音的 MKV 分片，并允许用户主动把多个视频转换为带时间戳的本地语音转写。

当前版本交付“可靠录制 + 用户触发的本地 ASR + 可选高光候选分析”能力。高光分析通过 Rust 中的受限 Agent 工作流调用 DeepSeek，只发送用户授权的规范化转写、时间戳、标签和分析目标；自动切片、字幕烧录和成品视频导出仍属于后续独立能力。

## 已实现功能

- 添加监控主播：输入公开抖音个人主页或直播间链接；个人主页名称可留空并使用页面昵称补全，直播间直连仍要求填写名称；
- 为每个主播维护最多 10 个有序标签，例如“带货”“搞笑”，并可填写未来切片使用的可选指导；
- 个人主页未开播时允许先保存，后台持续等待首次开播；
- 使用 `profile_sec_uid`、稳定 `web_rid` 和当前 `room_id` 区分主页身份、长期直播入口和本次直播场次；
- 首次发现直播入口后持久化标准化 `https://live.douyin.com/{web_rid}`，随后复用现有直播解析与录制链路；
- 直播间解析优先使用原生 HTTP；命中公开页访问限制后自动回退到一个全局、持久化且串行共享的系统 WebView；
- WebView 能自行进入公开直播页时静默恢复监听，确实需要交互时才显示“需要访问验证”，由用户主动打开验证窗口；
- 多个主播共享同一浏览器会话，浏览器解析期间不会阻塞已经运行的 FFmpeg 录制；
- 规范化来源链接、真实访问公开页面、提取身份并阻止重复主页或稳定直播入口；
- 使用 SQLite 保存主播、监听状态、设置、录制会话和视频分片；
- 使用 SQLite 保存当前设备的客户端激活状态；未激活时不恢复监听或录制，心跳确认停用、过期或解绑后立即暂停后台任务并要求重新激活；
- 应用启动后自动恢复之前开启的监听任务；
- 尚未发现直播入口的个人主页约每 60 秒加 0–10 秒抖动检查，错误按 60、120、300 秒退避；
- 已发现且离线的直播入口按 60 秒基础周期加 0–15 秒抖动检查；所有公开页面访问至少间隔 5 秒；只有连续 3 次非法 URL、HTTP 404 或 410 才回查个人主页，普通离线、访问受限、页面结构变化和网络错误不会清除稳定入口；
- 检测到开播后自动启动 FFmpeg，直播结束后关闭本次录制会话；
- FFmpeg 退出后重新确认房间状态；仍在直播时在同一逻辑会话中最多续录 3 次，只有明确未开播才结束会话；
- 默认最多同时录制 4 个直播间，可在设置页修改；
- 录像默认保存到 `~/Downloads/dy-screen/`，允许指定其他目录；
- 录制期间增量监听完成清单，MKV 分片一旦正确关闭就立即登记，不等待整场直播结束；
- 展示直播状态、监听状态、本次视频数量和历史视频数量；
- 提供本次监听视频和历史视频库，历史视频按录制会话分组；
- 视频库按当前分页异步生成第一帧 JPEG 封面，点击封面可直接使用内置播放器预览；
- 在监控中心和视频库中使用内置播放器预览已完成分片，同时保留系统播放器打开和 Finder/文件管理器定位；
- MKV 首次预览时按需生成 MP4 缓存：优先无损重封装，编码不兼容时自动回退为 H.264 + AAC；
- 支持删除单个视频或整个已结束会话；删除失败时恢复数据库状态并显示错误；
- 关闭主窗口后驻留系统托盘，监听和录制继续运行；
- 托盘提供打开窗口、暂停全部、恢复全部和退出；
- 每次原生请求、浏览器导航、页面探测状态变化、回退和最终动作都会向控制台及按日 JSONL 输出同一条脱敏诊断；不变的页面探测最多每 30 秒记录一次心跳；
- 提供安全的直播间响应诊断、FFmpeg/FFprobe 环境诊断、访问验证状态、去重系统通知、日志目录和可选开机启动；
- 磁盘低于 10 GB 时警告，低于 2 GB 时不启动新录制，低于 1 GB 时安全停止活动录制；
- AI 剪辑工作区支持创建项目、通过系统文件选择器导入多个本地视频，或选择一场已结束直播并按稳定顺序展开全部登记分片；
- 只有用户点击“开始分析”后才运行 ASR，打开页面、应用启动和新录像完成都不会自动识别；
- 本地流水线使用 FFprobe、FFmpeg、Silero VAD 和 `whisper.cpp small-q5_1`，macOS arm64 使用 Metal，Windows x64 使用 CPU；
- ASR 全局单并发，默认允许已完成视频在录制期间识别，不占用录制并发许可；设置中关闭并行后可恢复录制优先；相同源版本与识别配置可以跨项目复用稳定转写产物；
- 项目结果提供播放器、点击句段跳转、当前句段高亮、可关闭的跟随播放、仅存在于 WebView 的临时字幕、复制及 TXT/JSON 导出；
- AI 工作区支持标签/分析目标快照、队列中的“下一个处理”和“立即切换”，并在 ASR 完成后按用户授权运行候选发现 Agent 与评分 Agent；默认展示总分不低于 70 的前 10 个高光候选供勾选保存；
- 设置页支持 DeepSeek 模型、超时和系统凭据状态；API Key 只保存到操作系统凭据库，连接诊断使用固定提示，不保存原始响应；
- 高光分析使用版本化通用、带货、搞笑、知识和故事 Skills，未知标签只作为数据，不可改变 Agent 工具边界；
- 第一版转写不可编辑，不生成 SRT/ASS，不修改或烧录原视频，也不提供波形、多轨、裁剪或视频渲染入口；
- 主播、设置、视频、ASR 和高光结果均保存在本机；客户端只向授权运营服务上报白名单启动/功能/错误埋点，不上传媒体、ASR 文本、本地路径、Cookie 或直播页面正文。

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
├── src/                         Rust 直播解析、浏览器快照、FFmpeg 录制和多任务核心
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
│   ├── src/room_resolution.rs   原生 HTTP/WebView 双通道解析与共享会话状态机
│   ├── src/tauri_browser.rs     受限抖音验证窗口、最小快照和导航安全策略
│   ├── src/preview.rs           预览队列、FFmpeg 转换、缓存和状态模型
│   ├── src/thumbnail.rs         首帧封面、分页批次、JPEG 缓存和生命周期
│   ├── src/ai/                  AI 项目、命令、调度、恢复和本地 ASR 运行时
│   ├── src/api.rs               激活、心跳、AppStart、埋点统一服务端 API 客户端
│   ├── src/activation.rs        SQLite 激活摘要、心跳和埋点生命周期
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

配置授权运营服务端。开发默认使用 `http://localhost/api/`，也可以复制示例配置后修改：

```bash
cp .env.example .env
# DY_SCREEN_API_BASE_URL=https://your-domain.example/api/
```

`make app-dev`、`make app-build` 和正式资源构建都会把 `DY_SCREEN_API_BASE_URL` 传给 Tauri。发布环境必须显式配置正式 HTTPS 地址；所有请求都由 `src-tauri/src/api.rs` 统一处理，当前使用明文 JSON，并已预留后续请求/响应加解密 codec。

启动 Tauri 桌面客户端：

```bash
make app-dev
```

首次启动后：

1. 在不可关闭的激活弹窗输入服务端签发的激活码；激活成功前不会启动监听或录制；
2. 点击“添加主播”；
3. 可输入便于识别的主播名称；个人主页可留空；
4. 输入公开个人主页，例如 `https://www.douyin.com/user/...`，或直播间链接，例如 `https://live.douyin.com/452086788686`；
5. 个人主页可以不填名称，直播间直连需要填写名称；
6. 保持“添加后立即监听”开启；
7. 可选添加“带货”“搞笑”等主播标签，并使用上移、下移按钮调整未来切片上下文优先级；
8. 保存后，后台会立即检查一次主页或直播状态；
9. 原生 HTTP 受限时客户端会自动切换到隐藏且静音的系统 WebView，不会自动弹窗抢焦点；
10. 如果监控中心显示“需要访问验证”，点击“立即验证”并在抖音窗口完成人工操作；客户端检测到恢复后会自动隐藏窗口、触发访问状态检查并唤醒等待任务；
11. 主播开播时自动录制，下播时自动结束会话。

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
                                  ├─→ 播放器联动浏览、复制、TXT/JSON 导出
                                  └─→ 用户授权 → Candidate Agent → Ranking Agent → 高光候选
```

使用步骤：

1. 打开“AI 剪辑”，创建一个草稿项目；
2. 点击“添加本地视频”通过系统文件选择器一次导入多个外部视频，或点击“添加整场直播”选择一个已经结束的直播会话；
3. 在草稿中查看音轨、时长和错误，拖动排序或移除不需要的视频；
4. 确认环境诊断通过后点击“开始分析”；
5. 在左侧选择视频，在右侧播放器和时间戳文本列表中浏览结果；
6. 点击句段可跳转到源视频时间；“跟随播放”只控制列表滚动，“显示字幕”只在当前 WebView 临时覆盖；
7. 可复制单句、当前视频或整个项目，也可导出包含稳定定位信息的 TXT/JSON。

ASR 完成后，打开设置页填写 DeepSeek 模型和 API Key，点击“测试连接”验证凭据；回到已完成项目，确认发送范围后点击“开始高光分析”。分析只发送规范化文本和相对时间，不发送视频、音频、本地路径、Cookie 或签名流地址。候选结果的时间和句段 ID由 Rust 本地校验，选择保存只写入 SQLite，不触发 FFmpeg。

“添加本地视频”和“添加整场直播”是两个独立入口：前者用于外部文件多选；后者以录制会话为单位，一次加入该场直播的全部登记分片，不再打开文件或分片选择器。整场导入后，每个分片仍在统一输入列表中单独展示，用户可在开始分析前删除或调整顺序；重复分片会跳过，缺失、损坏、未完成、不可读取或无音轨的分片会保留为不可用项并计入反馈。

历史会话是否可选只取决于该会话是否已经结束。即使主播当前正在进行一场新直播，其以前已经结束的回放仍可选择；当前仍在录制、写入中的会话不可选择，也不能通过陈旧界面状态导入。导入和整理历史输入不会自动启动 ASR；只有用户点击“开始分析”才会创建任务。默认情况下，已完成视频可以和其他直播录制并行识别；关闭设置中的并行开关后，才会恢复录制优先。

本地导入不会把文件上传到网络，也不会复制原视频。项目只保存受信路径引用、源指纹、状态和转写产物；外部视频被移动、删除或修改后，对应输入会独立失败。移除直播分片也只会删除项目输入，不会删除视频库记录或原始录像文件。

`AsrEngine` 是业务层依赖的中立接口，输入包含准备后的音频、语言提示、热词和时间戳策略，输出包含引擎/模型身份、语言、句段时间、原始文本、可选置信信息和警告。第一版只有 `WhisperCppEngine`，后续增加云端或其他本地引擎时应新增 Adapter，不修改项目、缓存、时间轴和 UI 契约；本版不会自动上传或自动回退云端。

### ASR 资源与性能

- 最低基线为 8 GB 物理内存，任务前还会检查当前可用内存和至少 2 GB 临时磁盘余量；
- 默认模型是约 181 MiB 的多语言 `small-q5_1`，实际运行还需要模型工作区、音频缓冲和系统资源；
- ASR 全局最多运行一个输入，并限制为最多 4 个线程；默认不因其他直播处于录制状态而等待，低配置设备可以在设置中关闭并行处理；
- 每个输入结束后退出 `whisper.cpp` 子进程并释放模型资源，因此多视频之间会重复加载模型，但取消和故障隔离更清晰；
- VAD 用于排除静音、挂机和纯音乐，不能保证所有背景音乐场景都被正确过滤；
- 中文商品名、主播名和金额仍可能识别错误，可以在项目中配置热词。未经真实样本核验的转写不应被当作事实记录。

环境诊断失败时不会创建运行任务。正式客户端启动前会先进入“准备运行资源”页面：优先使用安装包内资源，缺失或损坏时只从构建期固定的 HTTPS 资源服务器下载并逐文件校验；下载完成后无需重新安装即可重新检测。资源服务器地址、manifest 签名公钥和组件版本不是用户设置，前端也不能指定任意可执行路径。
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

浏览器预览使用 `localStorage` 模拟主播、标签和设置，不能解析真实直播状态、建立抖音 WebView 会话或启动 FFmpeg。访问会话固定显示“真实访问验证仅桌面端可用”，不会伪造恢复成功。浏览器演示中的标签不会写入 SQLite；真实监听和录制必须使用 `make app-dev`。

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

本机已经准备好的资源会持久保存在 `resources/asr-source/`：其中包含模型、sidecar、FFmpeg 动态库、源码归档和 `SHA256SUMS`。该目录中的大型二进制按 `.gitignore` 保存在本机，不会提交到 Git；后续直接执行 `make asr-stage-macos` 或 `make asr-build-macos` 即可复用，不依赖 `/private/tmp`，也不会重复下载。没有本地资源时，再通过 `ASR_SOURCE` 指定可信资源目录。

仓库中的 manifest 是跨平台模板；`asr-bundle stage` 会为所选平台生成只包含一个平台且完整封存全部原生文件哈希的发行 manifest。模板目录不能直接作为正式运行资源。

### Runtime Resource Pack 发行与首次启动

资源包包含 FFmpeg/FFprobe、Whisper、VAD sidecar、small 量化模型、Silero 模型、OpenCC 字典、平台动态库、Windows VC++ 运行库和许可证。`asr-bundle stage` 会同时生成 `runtime-manifest.json`，清单采用 v2 结构，记录平台、版本、逐文件 SHA-256、最小内存/磁盘和许可证来源。

```bash
# 生成并校验 macOS arm64 随包资源
make asr-stage-macos ASR_SOURCE=/absolute/path/to/resources
make runtime-resource-verify ASR_STAGE=resources/asr-stage

# 构建正式的资源发行包；缺少资源时直接失败
make app-build-resources ASR_SOURCE=/absolute/path/to/resources

# 生成自有 HTTPS 静态托管目录（上传前必须用正式 Ed25519 私钥签署清单）
make runtime-resource-publish \
  ASR_STAGE=resources/asr-stage \
  RESOURCE_BASE_URL=https://yino-cut.oss-cn-beijing.aliyuncs.com/cut/stable/0.2.0/macos/aarch64/2026.07.3/ \
  RESOURCE_RELEASE_DIR=dist/runtime-resources
```

托管目录约定为 `channel/appVersion/platform/arch/bundleVersion/`，并在同一固定地址提供 `runtime-manifest.json` 及清单声明的资源文件。服务器应支持 HTTPS、Range 和大文件缓存；应用不会上传视频、音频、转写、主播信息、Cookie 或本地数据库。用户安装后如果资源未就绪，只能在资源页查看版本/大小/组件、下载、取消、重试或重新检测，监控、录制、视频库和 AI 剪辑保持锁定。

当前 macOS arm64 发行构建使用的固定资源基地址为：
`https://yino-cut.oss-cn-beijing.aliyuncs.com/cut/stable/0.2.0/macos/aarch64/2026.07.3/`。
`index.json` 仅用于发布目录索引，不能直接作为下载基地址。

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
make asr-build-macos
```

该命令默认从 `resources/asr-source/` 重新封存 `resources/asr-stage/`，再将同一份资源复制进 `.app` 的 `Contents/Resources/resources/asr/`。如需使用其他资源目录，可显式传入 `ASR_SOURCE=/absolute/path/to/asr-resources`。

Windows x64（在 Windows x64 构建机运行）：

```powershell
make asr-build-windows ASR_SOURCE=C:\absolute\path\to\asr-resources
```

Apple Silicon 开发机可用 `make asr-check-windows` 对根 crate 和 Tauri crate 执行 Windows
x64 全目标交叉编译与严格 Clippy；该检查不能替代 Windows 实机识别、签名和安装验收。

Windows x64 构建机可在 Visual Studio 2022 Developer PowerShell 中运行
`make asr-whisper-windows`，生成静态 CPU sidecar。脚本固定 SSE4.2 最低指令集并显式关闭
AVX/AVX2/BMI2，避免构建机 CPU 自动优化导致安装后非法指令崩溃。

两个命令先调用 Rust `asr-bundle` 工具校验并生成 `resources/asr-stage/`，再使用对应 Tauri 配置覆盖构建。macOS 默认生成可直接运行的 `.app`；在有 Finder 会话的构建机上设置 `ASR_BUNDLES=app,dmg` 可同时生成 `.dmg`。Windows 覆盖生成 NSIS 安装器并使用离线 WebView2 安装模式。正式发行仍必须在各自目标机完成签名、公证或 Authenticode、安装、卸载和离线 ASR 验收，不能用开发构建代替发行证据。

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
    └── 是：按 60 秒 + 0–15 秒抖动检查标准化直播间
            ├── 未开播：保留稳定入口
            ├── 网络失败：退避但不回查主页
            ├── 原生 HTTP 访问受限：切换共享 WebView
            │       ├── 浏览器可解析：恢复监听或录制
            │       └── 仍需交互：等待用户主动完成访问验证
            ├── 页面结构变化：保留入口并退避重试
            ├── 连续 3 次 404/410：清除直播绑定并回查主页
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
- 监听状态：已暂停、正在发现直播间、等待首次开播、主页检查失败、重新发现直播间、等待开播、浏览器解析中、需要访问验证、等待资源、录制中、正在重试、录制异常。

全局浏览器访问状态另行显示为：原生访问正常、浏览器解析中、需要访问验证、浏览器会话可用或会话需要重新建立。它不会把主播误报为下播或直播入口失效。

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
- `streamer_tags`：每个主播的标签名称、可选指导和稳定优先级顺序；
- `recording_sessions`：每次直播周期的逻辑会话；
- `videos`：完成 MKV 分片的路径、大小、音频和文件状态；
- `ai_projects`：用户主动创建的 AI 项目、冻结识别配置、状态和总体进度；
- `ai_project_inputs`：有序本地视频或直播分片引用、源指纹、源内时长和项目时间偏移；
- `asr_artifacts`：按源指纹和识别配置指纹发布、复用和失效的 ASR 产物；
- `transcript_segments`：稳定句段 ID、源内时间戳、原始/规范化文本和可选置信信息；
- `llm_provider_settings`：Provider、模型、超时、Prompt 版本等非敏感设置；API Key 不在 SQLite 中；
- `ai_highlight_runs`、`ai_highlight_chunks`、`ai_highlight_candidates`：高光运行快照、分块状态、结构化评分和用户选择；
- `settings`：录像目录、质量、协议、并发、FFmpeg 和桌面设置；
- `schema_migrations`：数据库迁移版本。

### 日志

macOS 默认日志目录：

```text
~/Library/Logs/com.yino.dyscreen/
```

设置页和检查失败主播的操作菜单都可以打开日志目录。监听检查按 UTC 日期写入 `dy-screen-YYYY-MM-DD.jsonl`，每行只包含：

- `timestamp`、`streamerId`、可选 `webRid` 和内部 `requestId`；
- `channel`：真实访问使用 `native` 或 `browser`，Supervisor 的派生状态汇总使用 `monitor`，避免把浏览器结果误标成原生响应；
- `stage`：请求、响应、导航、探测、回退或最终结果；
- `classification`：`live`、`offline`、`access_restricted`、`verification_required`、`layout_changed`、`entry_invalid`、`retryable_error` 等白名单枚举；
- 可选 HTTP 状态、Content-Type、响应字节数和安全 marker 布尔值；
- 耗时、连续失败次数、下一动作和可选重试时间。

每次真实访问在全局访问门放行后先输出 `request/pending`，完成时再输出原生 `response` 或浏览器 `navigation`；随后页面探测状态变化、回退和最终监听结果继续使用同一个 `requestId`。验证页面状态持续不变时只记录首次状态和最长 30 秒一次的心跳，不按 watcher 轮询频率刷屏。如果只有请求开始而没有完成记录，说明该访问仍在进行、已被取消或进程在返回前中断。每条白名单 JSON 都同时写到 stderr 和当日 JSONL。实例锁和退出流程还会向 stderr 输出 `instance_lock_acquired`、`instance_rejected`、`signal_registration_failed`、`shutdown_started`、`shutdown_deduplicated`、`shutdown_completed` 或 `shutdown_timed_out` 生命周期事件，只包含时间、固定事件和固定退出原因。日志不会写入 HTML、Cookie、React Flight 原始载荷、页面标题、直播间查询参数、错误正文、PID、本地路径或签名流地址。应用启动时只清理超过 30 天的应用日志，不会删除录像或数据库。

### 抖音浏览器会话

原生 HTTP 返回访问验证中间页后，客户端会按需创建标签为 `douyin-access` 的系统 WebView。该窗口默认隐藏，多个直播间串行共用同一个平台浏览数据目录；应用重启后会重新探测，而不是直接假定旧会话仍然有效。

- 房间解析目标只允许 `https://live.douyin.com`；WKWebView 导航回调还只允许验证码组件所需的精确 `about:blank` 和 `https://rmc.bytedance.com/verifycenter/captcha/` 路径，其他站点、相似域名、非 HTTPS、新窗口和下载会被拒绝；
- 远程页面不具备主窗口 capability，不能调用客户端通用 Tauri command 或读取本地文件；
- Rust 只主动读取规范化 URL、加载状态、安全 marker 和有界的受支持初始化脚本；
- 每次导航都校验请求代次、目标 URL、完整加载状态和目标 `web_rid`；旧房间页面或推荐房间对象不会被用于启动录制；
- 应用不读取、导出或写日志记录 Cookie、账号密码、完整 HTML、`localStorage` 或签名流参数；
- 应用不会自动识别验证码、模拟拖动或点击、接入第三方打码、切换代理或自动登录；
- 用户关闭验证窗口只会隐藏窗口并保留会话；设置页“清除抖音会话”会取消待处理浏览器解析并删除 WebView browsing data，但保留主播、录像和设置；
- 一个房间等待人工验证时，后续浏览器导航会暂停以保留当前页面，但其他房间仍可执行原生 HTTP 检查；窗口打开期间客户端自动观察当前页，“检查访问状态”会先尝试原生恢复，仍受限时才受控重载当前目标，恢复后自动唤醒所有主播；
- 清除会话时已经运行的 FFmpeg 不会被强制停止，只有后续状态刷新或流地址续签需要重新建立会话。

## 主播标签与未来切片上下文

添加或编辑主播时，可以维护一组仅保存在本机的有序标签：

- 每个主播最多 10 个标签；
- 标签名称去除首尾空白后必须为 1–24 个字符；
- 同一主播内标签名称按大小写无关规则去重；
- 每个标签可以填写最多 500 个字符的可选 Prompt 指导；
- 标签顺序代表未来切片上下文优先级，可使用上移、下移按钮调整；
- 归档和恢复主播会保留标签，重复身份安全合并时以已有主播标签为优先并追加新标签；
- 主播列表显示紧凑标签徽章，详情区域显示完整名称和指导。

例如，为主播依次配置：

```text
1. 带货：重点提取商品卖点、价格、优惠和购买理由
2. 搞笑：重点关注包袱、反转和幽默表达
```

未来切片功能可以读取结构化的 `StreamerPromptContext`，从 ASR 文本中优先寻找商品表达或搞笑表达相关句子。当前版本只保存和展示标签，不生成 Prompt、不调用 LLM、不执行 ASR、高光评分、句子提取或视频切片，也不会自动根据主页或录制内容推断标签。

## 关闭窗口和退出

- 点击主窗口关闭按钮只会隐藏窗口；
- 后台 worker、直播检查和活动录制继续运行；
- 点击托盘“打开主窗口”可以恢复并聚焦窗口；
- 托盘支持暂停全部和恢复全部监听；
- 显式退出且存在活动录制时，主窗口会要求确认；
- 确认后，后端先取消活动录制并等待完成分片收尾，超时后才会结束任务；
- macOS/Linux 的 `SIGTERM` 和 `SIGINT` 与前端、托盘、Tauri 退出请求共用同一个幂等关闭流程，重复退出事件不会重复结束会话；
- 所有 worker 共享 10 秒 Supervisor 子期限，整个应用关闭使用 20 秒总期限；正常路径会等待 FFmpeg、WebView、预览、缩略图和 AI runtime 清理后退出；
- 应用数据目录持有进程级独占锁，锁在打开 SQLite 和执行启动恢复前获取；第二实例只输出 `instance_rejected` 并退出，不会把第一实例的活动会话误标为中断或启动重复 FFmpeg；
- 只有旧实例完成关闭并释放锁后，新实例才能协调遗留分片并恢复已启用监听。

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

### 视频库首帧封面

历史视频库在当前 50 条分页完成元数据加载后异步请求首帧封面，列表会先显示固定 16:9 占位，不等待 FFmpeg。后端优先提取视频起点的第一个可解码帧；起点失败时只在第 1 秒重试一次。JPEG 保留画面方向和比例，最长边不超过 480px，卡片中使用居中裁切。本次监听视频的紧凑列表不会请求封面。

封面任务全局最多并行 2 个。翻页、修改筛选或离开视频库时会释放旧批次，并取消没有其他页面引用且尚未开始的任务；已经开始的单帧提取可以完成并写入缓存。原文件缺失时只会读取已经存在且仍有效的 MP4 预览缓存，不会为了封面生成完整 MP4。

macOS 的封面缓存默认位于：

```text
~/Library/Caches/com.yino.dyscreen/video-thumbnails/
```

封面 JPEG 和 JSON 清单是独立可再生缓存，不写入 SQLite，也不会修改 MKV/MP4。超过 30 天未访问的条目会被清理，并按最久未使用顺序把总容量限制在 512 MB；正在生成或当前页面正在显示的封面受保护。删除视频或整个会话会同步清理对应封面。生成失败时卡片保持稳定占位并提供重试图标，文件缺失或浏览器演示模式则显示不可用占位，已有预览、系统打开和定位操作不受封面错误影响。

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
| `make app-dev` | 启动 Tauri 桌面开发客户端；保留 Vite 前端热更新并禁用会强制结束录制进程的 Rust watcher |
| `make app-build` | 构建桌面应用 |
| `make asr-ffmpeg-macos FFMPEG_SOURCE=...` | 从锁定官方源码构建 LGPL、支持直播录制与本地媒体处理、可相对定位的 macOS 运行时 FFmpeg |
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
| `make thumbnail-doctor` | 检查 FFmpeg/FFprobe 和 JPEG 封面编码能力 |
| `make test-thumbnail` | 执行封面缓存、批次并发、删除竞态和 React 视频库测试 |
| `make test-thumbnail-integration` | 使用真实 FFmpeg 样本验证横屏、竖屏、无音轨与损坏媒体封面 |
| `make asr-test-contract` | 单独验证中立 `AsrEngine` 契约、资源、错误、取消和调度边界 |
| `make asr-test-media` | 单独验证 FFprobe、FFmpeg、临时音频和原视频不变 |
| `make asr-test-vad ASR_RESOURCE_ROOT=...` | 使用封存资源单独运行真实人声、静音和纯音乐 VAD |
| `make asr-test-whisper ASR_RESOURCE_ROOT=...` | 单独运行真实中文识别、结构化输出、取消和清理 |
| `make asr-test-cli ASR_RESOURCE_ROOT=... ASR_TEST_VIDEO=...` | CMD 验证 probe/audio/vad/asr 四个停止点并输出完整 ASR JSON |
| `make asr-test-stages ASR_RESOURCE_ROOT=...` | 顺序执行契约、媒体、VAD、Whisper 和 CMD 全部阶段 |
| `make test-profile` | 执行个人主页 fixture、URL 规范化和脱敏测试 |
| `make test-migration` | 执行三层身份 SQLite 迁移与唯一性测试 |
| `make test-supervisor-profile` | 执行个人主页/直播间双阶段状态机测试 |
| `make test-tags` | 执行主播标签 migration、repository、服务和前端测试 |
| `make test-ai` | 执行 ASR 调度、高光 repository、凭据 fake 和 Skills 单测 |
| `make accept-deepseek` | 启动桌面端，通过设置页系统凭据和“测试连接”执行显式真实 DeepSeek 验收；不会从命令行读取 Key |
| `make test-tag-migration` | 执行主播标签 SQLite migration 测试 |
| `make test-tag-repository` | 执行标签持久化、合并、重启和上下文测试 |
| `make test-tag-service` | 执行标签校验及主播创建、编辑服务测试 |
| `make test-tag-ui` | 执行标签表单、徽章、详情和浏览器演示测试 |
| `make test-browser-access` | 聚合执行核心快照、双通道服务、Tauri driver、Supervisor、React 和日志 fixture 测试 |
| `make test-access-core` | 执行浏览器最小快照、严格解析和诊断白名单测试 |
| `make test-room-resolution` | 执行原生优先、WebView 回退、粘性模式、串行队列、取消和恢复测试 |
| `make test-tauri-browser` | 执行验证窗口导航、下载、新窗口、旧 callback 和 capability 安全测试 |
| `make test-access-supervisor` | 执行多房访问门、验证等待、状态持久化和同会话续录测试 |
| `make test-access-ui` | 执行访问横幅、主播行、设置操作、事件恢复和浏览器降级测试 |
| `make test-access-fixtures` | 比对本地受支持页/验证页产生的 stderr 与 JSONL，并检查敏感值不泄漏 |
| `make test-app-lifecycle` | 执行实例锁互斥、锁释放后重获和关闭只领取一次的测试 |
| `make accept-access-fixtures ACCESS_FIXTURE_LOG_DIR=...` | 运行本地双页面验收并保留可查看的按日 JSONL |
| `make diagnose-real-room ACCEPT_ROOM_URL=...` | 对真实公开房间执行原生 HTTP 脱敏诊断，不启动 WebView |
| `make tail-access-log APP_LOG_DIR=...` | 持续查看客户端当天的访问 JSONL |
| `make accept-real-room ACCEPT_ROOM_URL=...` | 显式启动 Tauri，执行单房 WebView 回退、验证和录制验收 |
| `make accept-real-multi ACCEPT_ROOM_URLS="..." ACCEPT_MINUTES=30` | 显式启动至少三房的长时间监听与续录验收 |
| `make test` | 执行全部测试 |
| `make check` | 执行格式、Clippy、全部测试和前端构建 |
| `make spec-validate` | 严格校验全部 OpenSpec 主规格和活动变更 |
| `make verify` | 执行全量检查、OpenSpec 校验和桌面应用构建 |
| `make inspect-profile` | 只读检查公开个人主页及当前直播入口 |
| `make inspect-room` | 安全诊断直播间 HTTP 元信息、marker 和分类 |
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

安全诊断直播间：

```bash
make inspect-room \
  ROOM_URL='https://live.douyin.com/452086788686' \
  JSON=1
```

诊断分类含义：

| 分类 | 含义 | 监听处理 |
| --- | --- | --- |
| `live` | 找到受支持的在线房间结构 | 进入录制流程 |
| `offline` | 找到房间身份但当前无可用直播流 | 保留入口并按 60 秒加抖动等待 |
| `access_restricted` | 原生 HTTP 状态或安全 marker 表明需要额外访问验证 | 桌面客户端切换共享 WebView；CLI 只报告分类 |
| `verification_required` | WebView 有界探测后仍需要用户交互 | 保留入口和录制会话，等待用户主动验证 |
| `layout_changed` | 请求成功但当前版本无法识别页面结构 | 保留入口并有界退避 |
| `entry_invalid` | URL 非法或返回 HTTP 404/410 | 个人主页来源连续 3 次后重新发现 |
| `retryable_error` | 网络错误或其他暂时 HTTP 错误 | 保留入口并有界退避 |

输出只包含规范化直播间 URL、HTTP 状态、Content-Type、响应字节数、安全 marker 布尔值、分类和脱敏错误，不包含页面正文、Cookie、React Flight 原始载荷或签名流地址。该 CLI 命令只使用原生 HTTP，不会启动桌面 WebView；因此它返回 `access_restricted` 而 Tauri 客户端随后通过浏览器恢复，是符合设计的正常结果。

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

`make app-dev` 默认使用 `tauri dev --no-watch`。React/TypeScript 仍由 Vite 热更新；修改 Rust 后按 `Ctrl-C` 等待 `shutdown_completed`，再重新执行 `make app-dev`。不要直接使用带 Rust watcher 的 `tauri dev`，该 watcher 在 macOS 重建时会强制终止父进程，应用无法接管该终止并安全关闭 FFmpeg。

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

浏览器会话解析的离线聚焦验证：

```bash
make test-browser-access
make accept-access-fixtures \
  ACCESS_FIXTURE_LOG_DIR=/private/tmp/dy-screen-access-fixtures
```

真实公开房间不进入普通 `test`、`check` 或 CI。先确认原生通道的当前分类：

```bash
make diagnose-real-room \
  ACCEPT_ROOM_URL='https://live.douyin.com/703940802949'
```

然后启动桌面验收，并在另一个终端持续查看访问日志：

```bash
make accept-real-room \
  ACCEPT_ROOM_URL='https://live.douyin.com/703940802949'

make tail-access-log
```

单房验收必须同时保留以下证据：原生 `access_restricted` 诊断、浏览器通道 `live`、FFmpeg 启动、SQLite 中的活动会话、实际生成且 FFprobe 确认含音轨的 MKV，以及主动结束一次 FFmpeg 后在同一逻辑会话产生的后续分片。房间已下播时应换用当时正在直播且有权录制的公开房间，不能把离线结果写成成功。

多房验收使用至少三个公开房间：

```bash
make accept-real-multi \
  ACCEPT_ROOM_URLS='https://live.douyin.com/ROOM_A https://live.douyin.com/ROOM_B https://live.douyin.com/ROOM_C' \
  ACCEPT_MINUTES=30
```

连续观察至少 30 分钟，确认页面访问按至少 5 秒间隔串行执行、多个 FFmpeg 可以独立并发、重复验证状态只通知一次、人工恢复后等待 worker 被唤醒，并至少完成一次同会话地址刷新续录。

## 当前限制

- 录制、托盘、通知和文件管理器行为仍优先在 macOS 开发；Windows x64 的本地 ASR 适配器和安装配置已有自动化覆盖，但正式安装、签名、SmartScreen、中文用户目录与卸载仍需 Windows 实机发行验收；
- 普通开发构建继续允许使用设置中的 FFmpeg/FFprobe；正式 ASR 安装包通过平台覆盖配置携带独立 sidecar，发行流水线必须提供非符号链接、许可明确的可分发二进制；
- 抖音修改页面或 React Flight 数据结构后，客户端会显示“页面结构变化”并保留稳定入口；只有取得合法脱敏 fixture 和测试覆盖后才增加新格式支持；
- 只支持普通公开个人主页和直播间；原生 HTTP 受限后可由隔离 WebView 恢复，页面仍要求交互时会显示“需要访问验证”，不会把它误报为离线或入口失效；
- 不支持验证码自动识别、行为模拟、第三方打码、账号自动登录、Cookie 导出、代理池、DRM、付费或私有直播间绕过；
- 不录制弹幕、礼物动画或网页 UI；
- 内置播放器一次只预览单个已完成分片，不提供整场分片合并、统一时间轴或无缝连播；
- 预览仅在需要时生成可清理 MP4 缓存，不会在每次录制结束后自动转换全部录像；
- AI 工作区第一版只生成和浏览只读时间戳文本；不支持人工编辑、SRT/ASS、说话人分离、LLM 高光评分、裁剪计划、视频拼接、字幕烧录或成品导出；
- 本地 ASR 只支持 macOS arm64 Metal 和 Windows x64 CPU；不支持 Intel Mac、Windows ARM64、Linux、CUDA/Vulkan、多模型切换、在线下载或任意模型路径；
- small 量化模型已通过固定中文短句测试，但至少 10 场真实中文直播的商品名、金额、主播名召回率和 8 GB 双平台长时性能仍需目标设备样本验收；效果不达标时应另立用户明确授权的云端 `AsrEngine` Adapter 变更，本版不自动上传或回退；
- 主播标签目前只作为未来切片上下文预留，不会自动生成 Prompt 或触发模型调用；
- 主播标签表单、排序按钮、徽章折叠和详情布局已在 macOS 自动化测试与构建中验证，Windows/Linux 仍需实机确认字体、窄窗口和 WebView 表单行为；
- Windows/Linux 的托盘、开机启动、通知和文件管理器行为仍需在对应平台验证；
- 生产发布前仍需完成 macOS Developer ID 签名与 notarization、Windows Authenticode 签名与时间戳，以及两个平台的干净机器离线安装验证。

### 真实页面验收边界

2026-07-24 的只读 HTTP 诊断确认：同一设备上的普通浏览器可以打开公开直播间时，无状态请求仍可能获得 HTTP 200 的访问验证中间页。这不仅可能与 IP 有关，还与持久会话、JavaScript 环境、TLS/HTTP 特征和浏览器上下文有关，不能通过更换 User-Agent 可靠解决。

客户端现在会先记录原生尝试，再切换到隔离的持久化 WKWebView；如果系统 WebView 也停留在验证页，则等待用户主动打开同一窗口处理。脱敏 fixture、状态机、串行队列、会话清除、通知去重、监听等待、录制续接和 UI 操作均有自动化测试。真实房间是否正在直播以及目标页面是否继续在 WKWebView 主文档暴露受支持初始化脚本，仍必须按上节命令当场验收；未完成真实录制和 30 分钟多房证据前，不应宣称线上问题已经彻底解决。Windows/Linux 的系统 WebView、托盘、通知和真实网络差异仍需对应实机验证。

## 合规说明

仅应录制你有权保存和处理的内容，并遵守平台规则、版权要求、隐私要求及适用法律。本项目只处理普通公开页面；系统 WebView 仅执行标准页面脚本和用户主动完成的页面交互。应用不会自动识别或绕过验证码，不导入、复制、导出或记录 Cookie，不模拟行为，不使用代理池，也不会绕过权限控制、付费限制或 DRM。
