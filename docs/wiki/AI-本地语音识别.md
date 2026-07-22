# AI 工作区与本地语音识别

## 功能边界

第一版只把一批视频转换为可定位、可复用、只读的文本结果。它不会把字幕写进视频，也不会执行 LLM 语义判断、自动裁剪或成品渲染。

```text
用户创建草稿
  ├─ 系统文件选择器导入多个本地视频
  └─ 选择已结束直播 → 展开完成分片
           ↓
      排序、移除、诊断
           ↓ 用户点击开始分析
      冻结输入与识别配置
           ↓
FFprobe → FFmpeg → VAD → AsrEngine → 规范化 → 原子发布
           ↓
播放器 + 只读时间戳文本 + TXT/JSON
```

打开页面、应用启动、新录像完成和窗口恢复都不会自动创建项目或运行识别。录制活动期间，新的 ASR 输入保持等待。

## 领域边界

核心代码位于根 crate 的 `src/asr/`，不依赖 Tauri：

- `MediaInspector`：验证冻结源并返回音轨、时长和编码摘要；
- `MediaAudioPreparer`：把原始视频解码为 16 kHz、单声道、16-bit PCM；
- `VadEngine`：返回需要识别的人声时间范围；
- `AsrEngine`：供应商无关的识别接口；
- `TranscriptAssembler`：计算源内时间和多输入项目时间；
- `TranscriptionScheduler`：全局单并发、FIFO、取消和录制优先门控。

Tauri 后端位于 `src-tauri/src/ai/`：

- `service.rs`：草稿、受信输入、会话选择和 preflight；
- `repository.rs`：项目、输入、产物、句段和状态事务；
- `processor.rs`：单输入阶段编排和缓存短路；
- `desktop_runtime.rs`：真实 FFprobe、FFmpeg、VAD、Whisper 与调度器装配；
- `lifecycle.rs`：显式退出和异常重启恢复；
- `projection.rs`：复制、TXT 和 JSON 的只读投影；
- `tauri_commands.rs`：WebView 可调用的类型化命令与安全边界。

React 工作区位于 `ui/src/AiWorkspace.tsx`。前端只持有脱敏 DTO、一次性文件授权 ID、稳定项目/输入/句段 ID，不接收源路径或模型 stderr。

## AsrEngine 适配层

业务层只依赖 `AsrEngine`：

```rust
#[async_trait]
pub trait AsrEngine: Send + Sync {
    fn identity(&self) -> AsrEngineIdentity;
    fn capabilities(&self) -> AsrCapabilities;
    async fn diagnose(&self) -> EngineResult<AsrEnvironmentReport>;
    async fn transcribe(
        &self,
        request: AsrRequest,
        progress: Arc<dyn AsrProgressSink>,
        cancellation: CancellationToken,
    ) -> EngineResult<AsrResult>;
}
```

`AsrRequest` 只表达音频资产、语言提示、热词、人声范围、时间戳策略和线程上限；`AsrResult` 只表达引擎/模型身份、语言、音频时长、句段、可选置信信息和警告。类型中不存在命令行参数、sidecar 路径或云平台字段。

第一版 `WhisperCppEngine` 是 Rust Adapter，直接管理受控 `whisper-cli` 子进程。后续新增云端引擎时，应实现新的 Adapter 并把鉴权、上传确认、费用和数据保留放在独立 OpenSpec 变更中；项目、缓存、时间轴和 UI 继续使用当前契约。禁止在现有 Adapter 内静默自动回退云端。

## 识别阶段

### 1. 冻结与再次验证源

草稿保存规范路径、大小、修改时间和可选视频库 ID。开始分析时冻结输入顺序；执行前再次读取源指纹。文件被移动、删除或修改时，仅当前输入失败。

### 2. FFprobe

FFprobe 只读取原始媒体，不依赖预览 MP4。没有音轨、时长不可靠或仍在写入的文件不会进入识别。

### 3. FFmpeg

FFmpeg 使用参数数组生成短期 PCM WAV。临时目录按项目和输入隔离；成功、失败、取消、显式退出和异常重启都会清理 `.wav` 与 `.part`。原视频在阶段测试前后校验 SHA-256 不变。

### 4. VAD

Silero VAD 只判断人声范围。默认阈值为 `0.5`，最短人声 `250 ms`，最短静音 `100 ms`，人声边界保护 `500 ms`。完全静音或纯音乐输入进入 `skipped`，不生成空白或幻觉转写。

锁定的 `whisper.cpp v1.9.1` 独立 VAD 示例在当前 Apple Silicon 上启用 GPU 会退出，因此 VAD 固定使用 CPU；Whisper 主识别仍使用 Metal。

### 5. WhisperCppEngine

macOS arm64 资源声明为 Metal，Windows x64 声明为 CPU 并强制 `--no-gpu`。Adapter 使用参数数组处理 Unicode 和空格路径，解析完整 JSON，响应取消，删除结构化临时输出，并把进程错误转换为稳定中文错误。

Windows loader 的缺失 DLL 状态映射为 `whisper_runtime_missing`，架构或映像不兼容映射为 `whisper_binary_incompatible`；普通异常退出映射为 `whisper_failed`。错误不会包含源路径、命令行或 stderr。

### 6. 文本规范化与发布

原始文本不可变保存；规范化层执行繁转简字符映射、中文标点、数字和金额整理，并把规范化版本纳入识别配置指纹。全部句段在 SQLite 事务中发布后，产物才可复用。

## 缓存与稳定 ID

缓存键由源指纹和识别配置指纹共同决定。识别配置包含引擎、模型、语言、VAD、热词、时间戳和规范化版本。完全命中时跳过 FFprobe、FFmpeg、VAD 和 ASR；源或任一配置变化都会创建新产物。

每个发布句段拥有稳定 `seg_...` ID。另一个项目复用产物时保留稳定句段 ID和源内时间，只重新计算该项目的累计时间偏移。未来 LLM 必须返回稳定句段 ID 或明确时间范围，不能只返回一段无法定位的复制文本。

## UI 选择

第一版不做专业图形时间轴。左侧是有序视频和状态，主区域是当前播放器与时间戳文本：

- 点击文本跳转到源视频开始时间；
- 播放进入句段范围时高亮当前行；
- “跟随播放”只控制自动滚动，关闭后可以自由阅读；
- “显示字幕”使用 WebView 元素临时覆盖，不生成文件、不调用 FFmpeg、不修改视频；
- 长列表每页 200 句；宽窗口左右布局，窄窗口上下布局；
- 只允许浏览、复制、TXT/JSON 导出，不允许编辑、拆分、合并、拖动或裁剪。

预览缓存只服务播放。预览缺失或被清理不会影响原视频 ASR、缓存键或转写产物；无法可靠映射预览时间时，文本仍可浏览和复制，但跳转、高亮和临时字幕被禁用。

## 阶段命令行

不启动桌面界面即可验证单视频完整链路：

```bash
ASR_RESOURCE_ROOT=/absolute/path/to/asr-resources \
cargo run --offline -- asr /absolute/path/to/video.mp4 --json
```

输出包含引擎、模型、语言、时长、源内句段时间、原始文本、规范化文本和可选置信信息。该命令是开发与发行阶段测试入口；生产 UI 不接受资源根或任意 sidecar 路径。

## 随包资源

`resources/asr/manifest.json` 是版本、来源、大小、哈希、平台和最低资源要求的唯一来源。`asr-bundle` 构建工具只从已准备的可信目录复制，不联网：

```bash
cargo run --offline --bin asr-bundle -- stage \
  --source /absolute/path/to/asr-resources \
  --target resources/asr-stage \
  --platform macos-aarch64
```

工具会拒绝符号链接、错误哈希、错误权限、缺失平台和不匹配的引擎版本，并生成只含一个目标平台的暂存目录。macOS/Windows 的 Tauri 配置覆盖把该目录放到运行时 `resources/asr/`。Windows NSIS 额外离线安装 VC++ x64 与 WebView2；许可证见仓库根目录 `THIRD_PARTY_NOTICES.md`。

## 测试分层

- 契约：`tests/asr_contract.rs`；
- 固定媒体与预期：`tests/asr_fixtures.rs`；
- FFprobe/FFmpeg/临时文件：`tests/asr_media.rs`；
- VAD 单元与真实模型：`src/asr/vad.rs`、`tests/asr_vad_integration.rs`；
- Whisper JSON、错误和平台 Adapter：`src/asr/whisper_cpp.rs`、`tests/asr_whisper_adapter.rs`、`tests/asr_whisper_windows_adapter.rs`；
- 真实中文识别：`tests/asr_whisper_integration.rs`；
- 时间映射：`tests/asr_normalization.rs`、`tests/asr_scheduler.rs` 和相关 transcript 测试；
- Tauri 项目、repository、processor、命令、安全与恢复：`src-tauri/tests/ai_*.rs`；
- 完整业务链路：`src-tauri/tests/ai_e2e.rs`；
- React 交互与首版约束：`ui/src/AiWorkspace.test.tsx`、`ui/src/AiWorkspace.constraints.test.ts`。

每个阶段都有独立 Make 入口。真实 VAD、Whisper 和 CLI 共用一个经过 manifest、大小、哈希、
权限与平台校验的 `ASR_RESOURCE_ROOT`，避免测试命令分别注入任意 sidecar 或模型路径：

```bash
# 供应商无关契约、资源检查、错误脱敏、取消与调度
make asr-test-contract

# FFprobe、FFmpeg、16 kHz 单声道临时 WAV、取消与原视频哈希不变
make asr-test-media

# 真实 Silero VAD：中文人声、全静音、纯音乐
make asr-test-vad ASR_RESOURCE_ROOT=/absolute/path/to/sealed-asr-resources

# 真实 whisper.cpp：结构化结果、中文识别、运行中取消、临时 JSON 清理
make asr-test-whisper ASR_RESOURCE_ROOT=/absolute/path/to/sealed-asr-resources

# 验证 probe/audio/vad/asr 四个停止点，再让 stdout 返回完整 ASR JSON
make asr-test-cli ASR_RESOURCE_ROOT=/absolute/path/to/sealed-asr-resources

# 使用任意本地视频重复 CLI 阶段
make asr-test-cli \
  ASR_RESOURCE_ROOT=/absolute/path/to/sealed-asr-resources \
  ASR_TEST_VIDEO=/absolute/path/to/video.mp4

# 顺序执行以上全部阶段
make asr-test-stages ASR_RESOURCE_ROOT=/absolute/path/to/sealed-asr-resources
```

`asr-test-cli` 的 JSON 包含引擎/模型身份、语言、视频时长、句段起止毫秒、原始文本、
规范化文本和可用时的置信概率。该入口只接受本地文件；“上传”仍表示桌面文件选择器导入，
不会把视频发送到网络。

也可以直接使用同一个 `asr` 命令在某个阶段停止。未传 `--stop-after` 时默认保持原有完整
ASR 行为，因此已有脚本不需要修改：

```bash
ASR_RESOURCE_ROOT=/absolute/path/to/sealed-asr-resources \
cargo run --offline -- asr /absolute/path/to/video.mp4 --stop-after probe --json

ASR_RESOURCE_ROOT=/absolute/path/to/sealed-asr-resources \
cargo run --offline -- asr /absolute/path/to/video.mp4 --stop-after audio --json

ASR_RESOURCE_ROOT=/absolute/path/to/sealed-asr-resources \
cargo run --offline -- asr /absolute/path/to/video.mp4 --stop-after vad --json

ASR_RESOURCE_ROOT=/absolute/path/to/sealed-asr-resources \
cargo run --offline -- asr /absolute/path/to/video.mp4 --stop-after asr --json
```

- `probe` 返回音轨、源时长、编码、采样率和声道；无音轨视频也能成功返回诊断。
- `audio` 返回 FFmpeg 生成的 16 kHz 单声道 PCM 格式，但不暴露临时 WAV 路径。
- `vad` 返回映射到源视频的有效人声毫秒区间；无人声时返回空数组而不是虚假文本。
- `asr` 返回原有完整识别 JSON。所有阶段都复用参数数组、取消令牌和哈希隔离临时目录。

真实 Metal 测试需要允许子进程访问 GPU；受限沙箱内启用 Metal 可能退出 139，CPU 模式不能替代正式 Metal 发行验收。
