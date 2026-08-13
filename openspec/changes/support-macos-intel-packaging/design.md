## Context

当前 macOS 流程只声明 `macos-aarch64`：Makefile 根据固定目录 staging、验证和发布资源，Tauri 默认在主机 target 目录输出应用，FFmpeg 与 whisper.cpp 脚本也固定生成 arm64 文件。Apple Silicon 的 Clang、macOS SDK、Rust 和 CMake 均支持 `x86_64-apple-darwin` 交叉构建，但只有应用、全部 sidecar 和动态库使用同一目标架构时，安装包才可在 Intel Mac 原生运行。

资源模板同时承担平台契约。仅向 Tauri 传 `--target x86_64-apple-darwin` 会生成 Intel 主程序，却仍把 arm64 FFmpeg/Whisper 打入包内；该产物能够安装但核心录制、预览和 ASR 会失败，因此发行入口必须统一派生应用与资源架构。

## Goals / Non-Goals

**Goals:**

- Apple Silicon 和 Intel macOS 构建机均可通过 `MACOS_ARCH=aarch64|x86_64` 选择独立发行架构。
- 构建入口统一映射 Rust target、Runtime Resource Pack 平台、资源路径、远程目录和 DMG 名称，并拒绝未知架构。
- 从锁定源码交叉构建目标架构的 FFmpeg、FFprobe、whisper-cli 和 VAD sidecar，校验所有 Mach-O 切片与动态依赖。
- 提供可重复的 macOS 可信资源源组装流程，保留共用模型、字典、许可证和来源记录。
- 在无法完成签名、公证或 Intel 真机运行时，仍可通过静态契约测试、交叉编译和 Mach-O 检查明确区分已验证范围。

**Non-Goals:**

- 不生成同时包含两种切片的 universal 应用或 DMG。
- 不在脚本中管理 Developer ID 证书、公证账号或发布密钥。
- 不把 Rosetta 运行结果视为 Intel Mac 发行验收证据。
- 不改变 Windows x64 打包与运行资源契约。

## Decisions

### 使用单一 `MACOS_ARCH` 作为发行架构输入

Makefile 接受 `aarch64` 或 `x86_64`，默认从 `uname -m` 推导，并由该值生成 `MACOS_RUST_TARGET`、`MACOS_RESOURCE_PLATFORM` 和 `MACOS_RESOURCE_DIR`。所有 macOS staging、校验、发布与 Tauri 构建都复用这些派生值。

备选方案是增加独立的 `build-mac-intel-release` 并复制整套目标。该方式短期直观，但会让 arm64 与 Intel 门禁逐步分叉；统一参数更容易保持一致，并仍可提供 Intel 别名目标改善可发现性。

### 分别生成架构专属 DMG

Tauri 通过 `--target <rust-target>` 将产物写入 `src-tauri/target/<rust-target>/release/bundle/`。自定义无 Finder DMG 脚本继续复用，文件名显式包含 `aarch64` 或 `x86_64`。

不选择 universal 包，因为 FFmpeg 动态库、whisper sidecar、资源 manifest 和签名都需要额外合并及双切片校验，产物体积也会显著增加；独立安装包更容易诊断和回滚。

### 原生资源脚本显式接收目标架构

FFmpeg 脚本把 Rust 风格架构映射为 Apple Clang 的 `arm64`/`x86_64`，通过 configure 的 `--arch`、`--cc`、`--extra-cflags=-arch ...` 和 `--extra-ldflags=-arch ...` 固定切片。whisper.cpp 使用 `CMAKE_OSX_ARCHITECTURES`；aarch64 保持 Metal，x86_64 使用 CPU 后端并关闭 `GGML_NATIVE`，以避免构建机 CPU 特征泄漏到 Intel 发行基线。

每个脚本都使用 `lipo -archs` 或 `file` 校验主二进制和动态库只包含目标切片，再进行依赖与签名检查。交叉构建出的 x86_64 文件不能直接在未安装 Rosetta 的 Apple Silicon 上执行，因此版本/能力运行检查只在主机可执行目标切片时进行，静态架构与依赖门禁始终执行；最终能力检查必须在 Intel 真机发行验收中完成。

### 由组装脚本生成单架构可信源

新增 macOS 资源组装脚本读取跨平台模板，从指定 FFmpeg 与 Whisper 输出复制目标架构文件，并复用共用模型、字典和许可证。脚本生成只声明目标 macOS 平台的 `manifest.json` 与来源记录，再交给现有 `asr-bundle stage` 计算逐文件完整性。

不直接在版本库内复制 Intel 大型二进制；它们继续由本地锁定源码构建并受 `.gitignore` 管理。

## Risks / Trade-offs

- [Apple SDK 或第三方 crate 提高最低系统版本] → 保持 Tauri `minimumSystemVersion=12.0`，对 Intel target 执行完整 `cargo check/build` 并在 Intel macOS 12+ 真机验证。
- [Apple Silicon 无法直接运行 x86_64 sidecar] → 静态校验架构、RPATH 和依赖；安装 Rosetta 时可做辅助能力检查，但正式发布仍要求 Intel 真机。
- [x86_64 CPU 特征被构建机自动启用] → whisper.cpp 关闭 `GGML_NATIVE` 和 Metal，使用通用 CPU 构建；真机覆盖最低支持型号。
- [两个架构误用同一 staging 目录] → staging marker、manifest 平台和 Mach-O 架构校验不匹配时立即失败，文档建议使用架构专属目录。
- [签名后哈希变化] → 延续先签 sidecar、再 staging、最后签外层应用的顺序；任何资源重签后重新 staging。

## Migration Plan

1. 扩展 manifest 与脚本契约，保留 arm64 默认行为。
2. 为现有 arm64 流程运行契约测试与资源校验，确认无回归。
3. 安装 Rust `x86_64-apple-darwin` target，从仓库已有锁定源码构建 Intel FFmpeg/Whisper。
4. 组装并 staging Intel 资源，交叉构建 `.app` 与 DMG，执行 Mach-O 和包内资源审计。
5. 正式发布前在 Intel Mac 完成启动、录制、预览、封面、ASR、剪辑导出、签名和公证验收。

回滚时不使用 `MACOS_ARCH=x86_64`，原有 arm64 默认命令和资源目录保持不变；尚未发布的 Intel 资源目录不更新 stable index。

## Open Questions

- Intel 真机验收后是否仍需把 Metal 作为可选加速后端，可根据性能证据另开变更决定。
- 若未来需要单一下载入口，再评估 universal 应用、sidecar 和 FFmpeg dylib 的双切片合并及签名流程。
