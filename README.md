# 直播管家（dy-screen）

直播管家是一个基于 Tauri 2.0、React、TypeScript、Rust 和 SQLite 的本地桌面客户端，用于同时监听多个公开抖音直播间，并在主播开播后自动保存包含视频和声音的 MKV 分片。

当前版本优先验证并交付“可靠录制”能力。AI 剪辑页面已经预留，但 ASR、NLP、高光识别和自动切片尚未实现。

## 已实现功能

- 添加监控主播：输入主播名称和公开抖音直播间链接；
- 规范化直播间链接、真实访问公开页面、提取房间标识并阻止重复添加；
- 使用 SQLite 保存主播、监听状态、设置、录制会话和视频分片；
- 应用启动后自动恢复之前开启的监听任务；
- 未开播时每 30 秒检查一次，错误时按 30、60、120、300 秒退避；
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
- AI 剪辑页面只展示未来流程，不读取视频，也不创建 AI 任务；
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
│       ├── App.tsx              监控中心、视频库、设置和 AI 占位页
│       ├── api.ts               Tauri command 与浏览器演示适配
│       └── styles.css           参考图风格和响应式主题
├── src-tauri/                   Tauri 2.0 桌面后端
│   ├── src/database.rs          SQLite migration 和 repository
│   ├── src/supervisor.rs        监听 worker、自动录制、重试和磁盘保护
│   ├── src/preview.rs           预览队列、FFmpeg 转换、缓存和状态模型
│   ├── src/app.rs               command、事件、托盘和桌面生命周期
│   └── tests/                   数据库与状态机测试
├── openspec/                    中文 OpenSpec 规格和变更
├── Makefile                     开发、构建、测试和 CLI 命令
└── README.md                    本文档
```

前端不直接执行 SQL。所有数据读写、文件操作和录制控制都通过类型化 Tauri command 进入 Rust 后端。

## 环境要求

当前优先支持 macOS，代码结构保留 Windows 和 Linux 迁移空间。

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
2. 输入便于识别的主播名称；
3. 输入公开直播间链接，例如 `https://live.douyin.com/452086788686`；
4. 保持“添加后立即监听”开启；
5. 保存后，后台会立即检查一次直播状态；
6. 主播开播时自动录制，下播时自动结束会话。

直播间可能随时下播，示例地址只用于展示格式。

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

构建 macOS `.app`：

```bash
make app-build
```

默认产物位于：

```text
src-tauri/target/release/bundle/macos/直播管家.app
```

第一版继续依赖本机安装的 FFmpeg/FFprobe。正式分发前还需要单独确定 sidecar 来源、签名、许可证和多平台打包策略。

## 自动监听与录制逻辑

每个启用监听的主播最多拥有一个长期 worker：

```text
启动/恢复监听
    ↓
立即检查直播页
    ├── 未开播：等待 30 秒后再次检查
    ├── 检查失败：30/60/120/300 秒退避
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
- 监听状态：已暂停、等待开播、等待资源、录制中、正在重试、录制异常。

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

- `streamers`：主播、链接、监听开关、直播状态和监听状态；
- `recording_sessions`：每次直播周期的逻辑会话；
- `videos`：完成 MKV 分片的路径、大小、音频和文件状态；
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
| `make fmt` | 格式化根 crate 和 Tauri crate |
| `make fmt-check` | 检查 Rust 格式 |
| `make lint` | 对两个 Rust crate 执行 Clippy 严格检查 |
| `make test-frontend` | 执行 React 组件测试 |
| `make test-core` | 执行录制核心测试 |
| `make test-app` | 执行 SQLite、supervisor 和预览服务测试 |
| `make preview-doctor` | 检查 FFmpeg/FFprobe 和可用 H.264 编码器 |
| `make test-preview` | 执行预览 Rust 测试和播放器组件测试 |
| `make test-preview-integration` | 使用真实 FFmpeg 样本验证重封装、回退转码和无音轨视频 |
| `make test` | 执行全部测试 |
| `make check` | 执行格式、Clippy、全部测试和前端构建 |
| `make spec-validate` | 严格校验全部 OpenSpec 主规格和活动变更 |
| `make verify` | 执行全量检查、OpenSpec 校验和桌面应用构建 |
| `make resolve` | 使用原 CLI 解析直播间 |
| `make record` | 使用原 CLI 录制单个直播间 |
| `make record-multi` | 使用原 CLI 同时录制多个直播间 |

## 原 CLI 录制方式

桌面客户端之外，原 Rust CLI 仍可独立使用。

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

- 第一版优先在 macOS 上开发和验证；
- FFmpeg/FFprobe 尚未作为 Tauri sidecar 打包，运行机器必须自行安装；
- 抖音修改页面或 React Flight 数据结构后，解析器和 fixture 可能需要更新；
- 仅支持无需登录即可访问的公开直播间；
- 不支持验证码、Cookie 自动化、DRM、付费或私有直播间绕过；
- 不录制弹幕、礼物动画或网页 UI；
- 内置播放器一次只预览单个已完成分片，不提供整场分片合并、统一时间轴或无缝连播；
- 预览仅在需要时生成可清理 MP4 缓存，不会在每次录制结束后自动转换全部录像；
- AI 剪辑、ASR、NLP 和高光切片尚未实现；
- Windows/Linux 的托盘、开机启动、通知和文件管理器行为仍需在对应平台验证；
- 生产发布前仍需完成 FFmpeg sidecar、应用签名、公证和安装包策略。

## 合规说明

仅应录制你有权保存和处理的内容，并遵守平台规则、版权要求、隐私要求及适用法律。本项目不会绕过登录、权限控制、付费限制或 DRM。
