# ASR 资源基线

## 固定版本

第一版固定使用以下资源，用户不能导入任意运行时或切换模型。资源可以随安装包提供；本机缺失或
损坏时，客户端只允许从发行构建注入的固定 HTTPS Runtime Resource Pack 地址下载同一受控版本：

- `whisper.cpp v1.9.1`，源码提交 `f049fff95a089aa9969deb009cdd4892b3e74916`；
- 多语言 `ggml-small-q5_1.bin`，190,085,487 字节；
- Silero VAD `ggml-silero-v6.2.0.bin`，885,098 字节；
- macOS arm64 使用 Metal 构建；macOS Intel x86_64 与 Windows x64 使用 CPU 构建。

`whisper-cli` 在 macOS arm64 使用 Metal。`whisper.cpp v1.9.1` 的独立
`whisper-vad-speech-segments` 在当前 Apple Silicon 实测启用 `--use-gpu` 会异常退出，
因此 VAD sidecar 固定使用 CPU；Silero VAD 本身很轻量，不影响主模型使用 Metal。
macOS Intel 构建关闭 Metal、BLAS 和 `GGML_NATIVE`，并关闭 AVX、AVX2、FMA、F16C、BMI2，
只声明 SSE4.2 CPU 基线，避免把 Apple Silicon 构建机特征带入 Intel 包。

模型和 VAD 的 SHA-256、来源、逻辑 ID、最低资源要求以
[`resources/asr/manifest.json`](../../resources/asr/manifest.json) 为准。任务开始前必须校验
文件大小与 SHA-256，不能只检查文件名。

应用每次启动在后台完整校验一次资源，成功后只在当前进程缓存解析结果；文件大小、修改时间、
类型或 manifest 变化都会触发完整复核。已安装版本与随包版本并存时选择满足应用要求的较新完整
版本。运行机制和故障处理见 [运行资源与视频预览](运行资源与视频预览.md)。

仓库内 `manifest.json` 是跨平台发行模板，其中 `resourceIntegrity` 在尚未取得目标平台
产物时保持空数组。`asr-bundle stage` 会读取可信构建目录，对所选平台的 Whisper、VAD、
FFmpeg、FFprobe、动态库和 Windows VC++ 安装器逐文件计算大小与 SHA-256，并把完整记录
封入只包含一个平台的暂存 manifest。应用运行时拒绝没有完整封存、缺项、重复项、额外项
或哈希不匹配的 manifest，因此不能用模板目录直接启动正式 ASR。

## 选择理由

`small-q5_1` 在 8 GB 设备上比未量化 small 模型占用更低，同时保留多语言和中文能力。
Silero v6.2.0 是 `whisper.cpp v1.9.1` 官方下载脚本列出的 VAD 版本。第一版优先保证
离线安装、跨平台一致契约和可诊断失败；真实中文样本不达标时，后续新增由用户明确授权
的云端 `AsrEngine`，本版不自动上传或回退。

## 完整性与供应链

1. 构建流水线从固定版本来源取得资源。
2. 流水线计算 SHA-256 并与 manifest 比较。
3. macOS 对主程序与 sidecar 签名并完成 notarization。
4. Windows 对安装包与 sidecar 签名，并验证 VC++ x64 运行库。
5. 应用只从受保护的资源目录解析文件，前端不能传入任意可执行路径。

资源许可证和再分发说明统一维护在 `THIRD_PARTY_NOTICES.md`，模型升级必须同时更新
manifest、许可证说明、真实样本基准和两个平台安装验证记录。

## 发行暂存

正式构建不从网络下载资源。构建机先准备一个与 manifest 路径一致的可信目录，再使用根
crate 的 Rust 工具复制和校验单一目标平台。macOS 平台值分别为 `macos-aarch64` 和
`macos-x86-64`，建议使用独立 staging 目录：

```bash
make asr-stage-macos \
  MACOS_ARCH=aarch64 \
  MACOS_ASR_SOURCE=resources/asr-source-macos-aarch64 \
  ASR_STAGE=resources/asr-stage

make asr-stage-macos \
  MACOS_ARCH=x86_64 \
  MACOS_ASR_SOURCE=resources/asr-source-macos-x86_64 \
  ASR_STAGE=resources/asr-stage-x86_64
```

Windows 平台值为 `windows-x86-64`。工具会验证模型、VAD 和规范化字典的大小与 SHA-256，
封存并复核全部平台二进制的大小与 SHA-256，检查权限和引擎版本，并拒绝符号链接。
Windows 暂存目录还必须包含
`runtime/windows-x86_64/vc_redist.x64.exe`；NSIS 安装器在复制资源后以静默模式安装，
不能把运行库缺失留到用户第一次识别时才发现。

暂存完成后必须把单平台 manifest、构建日志、版本输出和签名记录一起归档。安装后的环境
诊断会再次校验这些哈希；任何 sidecar、动态库或运行库安装器被替换后都会在启动任务前
失败，而不是等到模型进程运行时才暴露。

在 Apple Silicon 开发机维护 Windows 条件编译边界时，可安装 `cargo-xwin`、Rust
`llvm-tools-preview` 和 Homebrew LLVM，然后运行：

```bash
make asr-check-windows
```

该命令对根 crate 与 Tauri crate 执行 Windows x64 全目标 `check` 和 `clippy -D warnings`。
它只能证明代码可针对 MSVC 目标编译，不能替代 Windows 目标机、签名、安装器或运行时验收。

`src-tauri/tauri.macos.conf.json` 与 `src-tauri/tauri.windows.conf.json` 只在正式 ASR 构建时
作为配置覆盖使用，普通开发构建不携带大模型。资源进入安装包后统一位于运行时
`resources/asr/`，后端不接受前端或普通设置提供的其他路径。

macOS FFmpeg 使用 `scripts/build-asr-ffmpeg-macos.sh` 从官方 `ffmpeg-8.1.2.tar.xz` 构建。
源码锁定 SHA-256 为
`464beb5e7bf0c311e68b45ae2f04e9cc2af88851abb4082231742a74d97b524c`。构建禁用
GPL/nonfree 与外部编解码库，只保留直播录制所需的受控 HTTPS 网络协议，以及预览、封面、
ASR 和剪辑导出所需的容器、编解码器与滤镜，许可证为 LGPL-2.1-or-later。7 个 FFmpeg dylib 通过 manifest 的
`libraries` 字段进入资源检查，并改写为 `@loader_path`/`@executable_path` 相对依赖；脚本
在结束前扫描并拒绝任何构建临时绝对路径。`MACOS_ARCH=aarch64|x86_64` 会同时选择 Clang
架构、资源目录和最低 macOS 12.0；Intel 构建需要 NASM。

macOS Whisper 使用 `scripts/build-asr-whisper-macos.sh` 从锁定 SHA-256 的
`whisper.cpp-v1.9.1.tar.gz` 构建。脚本关闭共享库和网络下载并静态链接 Whisper/GGML；arm64
启用并内嵌 Metal shader，Intel x86_64 使用可移植 CPU 配置。最终 sidecar 只允许依赖
`/System` 与 `/usr/lib`。构建结束后执行 ad-hoc 签名、版本检查和 Mach-O 依赖扫描；
`asr-bundle verify` 会在发行暂存阶段再次拒绝包外动态依赖或绝对 RPATH。

Apple Silicon 构建 Intel Runtime Resource Pack 的完整入口为：

```bash
rustup target add x86_64-apple-darwin
brew install cmake nasm
make macos-build-doctor MACOS_ARCH=x86_64
make asr-ffmpeg-macos MACOS_ARCH=x86_64
make asr-whisper-macos MACOS_ARCH=x86_64
make asr-prepare-macos MACOS_ARCH=x86_64 ASR_SOURCE=resources/asr-source
make asr-stage-macos MACOS_ARCH=x86_64 \
  MACOS_ASR_SOURCE=resources/asr-source-macos-x86_64 \
  ASR_STAGE=resources/asr-stage-x86_64
make runtime-resource-verify MACOS_ARCH=x86_64 ASR_STAGE=resources/asr-stage-x86_64
```

组装和安装包审计要求 FFmpeg、FFprobe、Whisper、VAD 与 7 个 dylib 都是目标单切片 Mach-O，
最低系统版本不高于 macOS 12.0。Rosetta 运行只能作为辅助能力检查；正式 Intel 发行仍要求
真实 Intel Mac 的 Developer ID、notarization、Gatekeeper、离线启动和完整业务验收证据。

Windows Whisper 使用 `scripts/build-asr-whisper-windows.ps1` 在 Visual Studio 2022 x64
Developer PowerShell 中构建。脚本显式关闭 `GGML_NATIVE`、AVX、AVX2、FMA、F16C 和 BMI2，
只启用 SSE4.2，使 PE 的最低 CPU 要求与 manifest 一致；Whisper/GGML 和 MSVC CRT 静态
链接，并通过 `dumpbin` 拒绝意外的 Whisper/GGML/OpenMP DLL 依赖。该脚本的产物仍必须在
Windows x64 目标机完成 Authenticode、VC++ 运行库、Unicode 路径、取消和真实识别验收。

平台签名属于文件完整性的一部分。正式发布必须先签名 sidecar/动态库，再进行单平台暂存
并封存签名后哈希；外层 `.app` 或 NSIS 安装器签名完成后再次校验包内资源。若封存后重新
签名任何 sidecar，必须重新生成 manifest，不能保留旧哈希。
