# 直播管家（dy-screen）

直播管家是一个基于 Tauri 2.0、React、TypeScript、Rust 和 SQLite 的本地桌面客户端，用于通过公开抖音个人主页或直播间入口同时监听多个主播，并在开播后自动保存包含视频和声音的 MKV 分片。

当前版本优先验证并交付“可靠录制”能力。AI 剪辑页面已经预留，但 ASR、NLP、高光识别和自动切片尚未实现。

## 已实现功能

- 添加监控主播：输入公开抖音个人主页或直播间链接；个人主页名称可留空并使用页面昵称补全，直播间直连仍要求填写名称；
- 为每个主播维护最多 10 个有序标签，例如“带货”“搞笑”，并可填写未来切片使用的可选指导；
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
2. 可输入便于识别的主播名称；个人主页可留空；
3. 输入公开个人主页，例如 `https://www.douyin.com/user/...`，或直播间链接，例如 `https://live.douyin.com/452086788686`；
4. 个人主页可以不填名称，直播间直连需要填写名称；
5. 保持“添加后立即监听”开启；
6. 可选添加“带货”“搞笑”等主播标签，并使用上移、下移按钮调整未来切片上下文优先级；
7. 保存后，后台会立即检查一次主页或直播状态；
8. 主播开播时自动录制，下播时自动结束会话。

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

浏览器预览使用 `localStorage` 模拟主播、标签和设置，不能解析真实直播状态，也不能启动 FFmpeg。浏览器演示中的标签不会写入 SQLite；真实监听和录制必须使用 `make app-dev`。

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
- `streamer_tags`：每个主播的标签名称、可选指导和稳定优先级顺序；
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
| `make test-profile` | 执行个人主页 fixture、URL 规范化和脱敏测试 |
| `make test-migration` | 执行三层身份 SQLite 迁移与唯一性测试 |
| `make test-supervisor-profile` | 执行个人主页/直播间双阶段状态机测试 |
| `make test-tags` | 执行主播标签 migration、repository、服务和前端测试 |
| `make test-tag-migration` | 执行主播标签 SQLite migration 测试 |
| `make test-tag-repository` | 执行标签持久化、合并、重启和上下文测试 |
| `make test-tag-service` | 执行标签校验及主播创建、编辑服务测试 |
| `make test-tag-ui` | 执行标签表单、徽章、详情和浏览器演示测试 |
| `make test` | 执行全部测试 |
| `make check` | 执行格式、Clippy、全部测试和前端构建 |
| `make spec-validate` | 严格校验全部 OpenSpec 主规格和活动变更 |
| `make verify` | 执行全量检查、OpenSpec 校验和桌面应用构建 |
| `make inspect-profile` | 只读检查公开个人主页及当前直播入口 |
| `make resolve` | 使用原 CLI 解析直播间 |
| `make record` | 使用原 CLI 录制单个直播间 |
| `make record-multi` | 使用原 CLI 同时录制多个直播间 |

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

- 第一版优先在 macOS 上开发和验证；
- FFmpeg/FFprobe 尚未作为 Tauri sidecar 打包，运行机器必须自行安装；
- 抖音修改页面或 React Flight 数据结构后，解析器和 fixture 可能需要更新；
- 仅支持无需登录即可访问的公开个人主页和直播间；主页受风控、要求登录或页面结构变化时会显示可重试错误；
- 不支持验证码、Cookie 自动化、DRM、付费或私有直播间绕过；
- 不录制弹幕、礼物动画或网页 UI；
- 内置播放器一次只预览单个已完成分片，不提供整场分片合并、统一时间轴或无缝连播；
- 预览仅在需要时生成可清理 MP4 缓存，不会在每次录制结束后自动转换全部录像；
- AI 剪辑、ASR、NLP 和高光切片尚未实现；
- 主播标签目前只作为未来切片上下文预留，不会自动生成 Prompt 或触发模型调用；
- 主播标签表单、排序按钮、徽章折叠和详情布局已在 macOS 自动化测试与构建中验证，Windows/Linux 仍需实机确认字体、窄窗口和 WebView 表单行为；
- Windows/Linux 的托盘、开机启动、通知和文件管理器行为仍需在对应平台验证；
- 生产发布前仍需完成 FFmpeg sidecar、应用签名、公证和安装包策略。

### 真实页面验收边界

2026-07-21 使用公开示例主页执行了只读 HTTP 验收。请求可以到达抖音，但当时返回的是包含 `__ac_nonce`、`__ac_signature` 和 `byted_acrawler` 的访问控制引导页，没有公开主页身份或 React Flight 数据。客户端会把该响应识别为“需要登录、验证码或额外访问权限”，按个人主页错误退避重试，不保存页面内容，也不尝试绕过。

脱敏 fixture、SQLite 迁移、重启恢复、首次发现、直播间 resolver 接管、录制会话创建、入口连续失效回查和界面状态均已在 macOS 开发环境通过自动化测试。当前公开示例主页的真实“发现直播入口”步骤仍取决于平台是否再次提供无需登录和风控脚本的公开 HTML。Windows/Linux 的托盘、通知、文件管理器和真实网络差异尚待对应实机验证。

## 合规说明

仅应录制你有权保存和处理的内容，并遵守平台规则、版权要求、隐私要求及适用法律。本项目只请求普通公开 HTTP 页面，不导入 Cookie，不使用浏览器自动化，不尝试登录或处理验证码，也不会绕过权限控制、付费限制或 DRM。
