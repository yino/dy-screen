# dy-screen

面向 Tauri 2.0 的抖音多直播间录制可行性 Demo。它从公开直播页解析当前有效的 FLV/HLS 直播源，并通过 FFmpeg 同时保存多个直播间的原始视频和声音。

当前阶段只验证录制链路。ASR、NLP、高光识别和自动切片将在录制稳定后单独设计。

## 当前结论

录制方案已经通过真实直播间验证：

- 能解析公开 `live.douyin.com` 页面中的 React Flight 初始化数据；
- 能识别 FLV/HLS 和 `FULL_HD1`、`HD1`、`SD1`、`SD2` 等清晰度；
- 能同时启动多个相互隔离的录制任务；
- 使用 FFmpeg `-c copy` 保存源视频和可选音频，不进行转码；
- 使用 MKV 分片降低网络中断或进程异常造成的文件损坏风险；
- 能通过 FFprobe 检查完成分片是否包含音频；
- 所有状态、错误和 JSON 输出都会移除签名 URL 的查询参数。

2026-07-18 对 `https://live.douyin.com/452086788686` 的实测结果：

- FLV/HLS 各解析出 4 档清晰度；
- `HD1 + FLV` 直接命中，无协议或清晰度回退；
- 录制文件为 H.264 视频，分辨率 `720×1280`；
- 音频为 AAC、`48 kHz`、双声道；
- 人工停止后，已完成 MKV 分片均可继续播放和处理。

> 直播间随时可能下播。如果示例地址失效，请通过 `ROOM_URL` 或 `ROOM_URLS` 换成正在直播的公开房间。

## 录制的是什么

本 Demo 采用“直播源直录”，不是桌面或浏览器画面截图录屏。这种方式通常具有以下优势：

- 不需要保持浏览器窗口可见；
- 不会录入鼠标、通知或其他桌面内容；
- 不进行视频转码时 CPU 占用较低；
- 保存的是直播源本身的视频和声音。

因此，它不会录制弹幕、礼物动画、浏览器控件或其他页面 UI。如果产品必须保留这些画面，需要另行实现系统级屏幕捕获方案。

## 环境要求

- macOS 或 Linux；
- Rust stable 和 Cargo；
- FFmpeg，同时需要 FFprobe；
- GNU Make 或兼容的 `make`。

macOS/Homebrew 安装示例：

```bash
brew install rustup ffmpeg
rustup toolchain install stable --profile minimal
rustup default stable
```

如果终端找不到 Cargo，可将 Rustup 路径加入环境变量：

```bash
export PATH="/opt/homebrew/opt/rustup/bin:$HOME/.cargo/bin:$PATH"
```

Windows 开发阶段建议使用 WSL 或 Git Bash。正式客户端应由 Tauri 2.0 打包对应平台的 FFmpeg/FFprobe sidecar。

## 快速开始

进入项目目录：

```bash
cd /opt/yino/python/douyin
```

检查依赖：

```bash
make doctor
```

构建发布版本：

```bash
make release
```

解析默认直播间：

```bash
make resolve
```

开始录制：

```bash
make record
```

按 `Ctrl-C` 停止。程序会先请求 FFmpeg 优雅退出，等待完成分片收尾，超过限定时间才会强制终止。当前 CLI 将人工取消视为非成功结束，因此 shell 可能看到退出码 `1`，但已经完成的 MKV 分片仍会保留。

## 使用 Makefile

查看全部命令和参数：

```bash
make help
```

常用目标：

| 目标 | 用途 |
| --- | --- |
| `make doctor` | 检查 Cargo、FFmpeg 和 FFprobe |
| `make build` | 构建调试版本 |
| `make release` | 构建发布版本 |
| `make resolve` | 解析一个直播间及可用清晰度 |
| `make record` | 录制一个直播间 |
| `make record-multi` | 同时录制多个直播间 |
| `make fmt` | 格式化 Rust 代码 |
| `make fmt-check` | 检查 Rust 代码格式 |
| `make lint` | 执行 Clippy 严格检查 |
| `make test` | 执行全部 Rust 测试 |
| `make check` | 依次执行格式检查、Clippy 和测试 |
| `make clean` | 清理 Cargo 构建产物，不删除录像 |

可覆盖参数：

| 变量 | 默认值 | 说明 |
| --- | --- | --- |
| `ROOM_URL` | `https://live.douyin.com/452086788686` | 单个直播间地址 |
| `ROOM_URLS` | `ROOM_URL` 的值 | 空格分隔的多个直播间地址 |
| `QUALITY` | `HD1` | `FULL_HD1`、`HD1`、`SD1` 或 `SD2` |
| `PROTOCOL` | `flv` | `flv` 或 `hls` |
| `OUTPUT` | `recordings` | 录像根目录 |
| `SEGMENT_SECONDS` | `900` | 每个 MKV 分片的目标时长，单位为秒 |
| `PROBE_TIMEOUT_SECONDS` | `3` | FFprobe 音频检查超时，单位为秒 |
| `FFMPEG` | `ffmpeg` | FFmpeg 命令或绝对路径 |
| `FFPROBE` | `ffprobe` | FFprobe 命令或绝对路径 |
| `CARGO` | 自动发现 Rustup Cargo | Cargo 命令或绝对路径 |
| `JSON` | `0` | 设为 `1` 输出 JSON 或 JSON-lines |

### 解析指定直播间

```bash
make resolve \
  ROOM_URL='https://live.douyin.com/452086788686' \
  QUALITY=FULL_HD1 \
  PROTOCOL=flv \
  JSON=1
```

解析命令只输出房间状态、清晰度、协议和脱敏后的 CDN 地址，不会输出签名查询参数。

### 录制一个直播间

```bash
make record \
  ROOM_URL='https://live.douyin.com/452086788686' \
  QUALITY=HD1 \
  PROTOCOL=flv \
  SEGMENT_SECONDS=900 \
  OUTPUT=recordings
```

如果 FFmpeg 不在 `PATH` 中，可以传入绝对路径：

```bash
make record \
  FFMPEG=/opt/homebrew/bin/ffmpeg \
  FFPROBE=/opt/homebrew/bin/ffprobe
```

### 同时录制多个直播间

```bash
make record-multi \
  ROOM_URLS='https://live.douyin.com/ROOM_A https://live.douyin.com/ROOM_B https://live.douyin.com/ROOM_C' \
  QUALITY=HD1 \
  OUTPUT=recordings
```

每个直播间对应独立 Tokio 任务和 FFmpeg 子进程。某个房间离线、解析失败或 FFmpeg 异常时，其他房间会继续录制；命令最终会逐房间报告结果，并在任一任务失败时返回非零退出码。

## 不使用 Makefile

Makefile 只是命令封装，也可以直接使用发布二进制。

构建：

```bash
cargo build --release
```

解析直播间：

```bash
./target/release/dy-screen resolve \
  'https://live.douyin.com/452086788686' \
  --quality HD1 \
  --protocol flv \
  --json
```

录制直播间：

```bash
./target/release/dy-screen record \
  'https://live.douyin.com/452086788686' \
  --quality HD1 \
  --protocol flv \
  --ffmpeg /opt/homebrew/bin/ffmpeg \
  --ffprobe /opt/homebrew/bin/ffprobe \
  --probe-timeout-seconds 3 \
  --segment-seconds 900 \
  --output recordings
```

多个房间只需在 `record` 后继续追加直播间 URL。

开发检查：

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
```

## 输出结构

```text
recordings/
  <room-id>/
    <session-id>/
      20260718-180000.mkv
      20260718-181500.mkv
      segments.csv
```

- 每次启动录制都会创建独立 session 目录；
- MKV 文件名使用分片开始时间；
- `segments.csv` 由 FFmpeg 在分片正确关闭后写入；
- 核心层只把清单中的文件放入 `segments`；
- 异常退出时未进入清单的 MKV 会放入 `partial_segments`，不会直接投递给后续 ASR；
- FFprobe 会为完成分片报告 `audio_present`。

FFmpeg 只能在合适的关键帧处切分，因此实际分片时长可能略大于 `SEGMENT_SECONDS`。

## JSON 事件

设置 `JSON=1` 后，录制过程会输出适合前端或任务系统消费的 JSON-lines 事件：

- `queued`
- `resolving`
- `resolved`
- `recording_started`
- `segment_finalized`
- `finished`

这些模型已经支持 Serde 序列化，后续可以直接映射为 Tauri event。

## Rust 模块

核心代码位于 Rust library，CLI 只是适配层：

- `resolver`：直播页请求、React Flight 解析、清晰度和协议选择；
- `recorder`：FFmpeg 参数、子进程监督、取消、分片清单和 FFprobe；
- `manager`：多房间并发和单房间故障隔离；
- `model`：请求、结果和生命周期事件；
- `error`：不会泄漏签名参数的类型化错误；
- `main.rs`：命令行参数和文本/JSON 输出。

## Tauri 2.0 接入建议

下一阶段可以让 Tauri managed state 持有 `RecordingManager<LiveJobRunner>`：

1. 前端调用 `start_recordings` command，传入直播间 URL、清晰度和输出策略；
2. Rust 后端为该批任务创建共享 `CancellationToken`；
3. manager 为每个房间启动独立任务和 FFmpeg sidecar；
4. 将核心产生的生命周期事件通过 `AppHandle::emit` 转发给前端；
5. `stop_recording` command 只触发 cancellation，不直接操作 FFmpeg PID；
6. 完成的 MKV 分片进入独立 ASR 队列，避免内容分析阻塞录制生命周期。

生产分发前需要确定各平台 FFmpeg/FFprobe sidecar 的来源、许可证、包体积和更新策略。

## 后续 ASR 与高光流程

每个完成的 MKV 分片可以作为后续处理边界：

1. 提取音频并提交 ASR；
2. 保存带时间戳的转写文本；
3. 通过 NLP/LLM 计算候选高光区间；
4. 将文本时间轴映射回原始视频；
5. 使用 FFmpeg 无损重封装或转码输出短视频。

本阶段没有实现以上流程，只保留分片、音频检测和生命周期事件接口。

## 已知限制

- 抖音修改页面或 React Flight 数据结构后，解析 fixture 和解析器可能需要更新；
- 签名流地址会过期，当前只在启动 FFmpeg 前解析一次；
- 断流后的自动重新解析、重试和无缝续录尚未实现；
- 暂无并发数量限制、磁盘空间配额、定时任务和崩溃恢复；
- 不支持登录自动化、验证码、DRM、付费直播、私有直播间或其他访问控制绕过；
- 不录制弹幕、礼物动画和浏览器 UI；
- 当前尚未创建 Tauri 前端和生产 sidecar 打包配置。

## 常见问题

### 提示找不到 Cargo

确认 Rust stable 已安装并将 Cargo 加入 `PATH`，或者临时指定：

```bash
make check CARGO=/absolute/path/to/cargo
```

### 提示找不到 FFmpeg

```bash
make doctor \
  FFMPEG=/absolute/path/to/ffmpeg \
  FFPROBE=/absolute/path/to/ffprobe
```

### 提示直播间已下播或没有可用流

先在浏览器确认房间仍在直播，再替换地址：

```bash
make resolve ROOM_URL='https://live.douyin.com/新的房间号'
```

### 为什么停止后命令返回非零状态

当前 Demo 将用户取消视为未自然完成，因此可能返回退出码 `1`。只要日志已经出现 `segment_finalized`，且文件列在 `segments.csv` 中，该分片就是已完成文件。

## 合规说明

仅应录制你有权保存和处理的内容，并遵守平台规则、版权要求、隐私要求及适用法律。本项目不提供访问控制绕过能力。
