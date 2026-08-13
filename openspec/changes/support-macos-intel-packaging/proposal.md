## Why

当前 macOS 发行流程将主机、Rust 目标、运行资源平台和 DMG 文件名固定为 `arm64`，导致 Apple Silicon 构建机无法为仍在使用 Intel Mac 的用户生成完整安装包。macOS SDK 支持在 Apple Silicon 上交叉构建 x86_64 应用，因此需要把应用与 FFmpeg、whisper.cpp、VAD 等原生资源统一切换到显式目标架构。

## What Changes

- 为 macOS 发行入口增加 `aarch64` 与 `x86_64` 架构选择，并根据架构设置 Rust target、Tauri 产物目录、DMG 文件名和资源下载地址。
- 扩展 macOS FFmpeg 与 whisper.cpp 构建脚本，使其能在 Apple Silicon 主机生成指定架构的原生二进制，并校验输出 Mach-O 架构及动态依赖。
- 扩展 Runtime Resource Pack 的 manifest、staging、校验和发布目录，支持独立的 `macos-x86-64` 资源包。
- 增加打包契约测试和操作文档，明确交叉构建所需工具链、命令、产物位置与 Intel 真机验收边界。
- 保持默认 macOS 架构为当前主机架构，现有 arm64 命令继续可用。
- 非目标：本次不生成 universal binary/DMG，不自动申请签名或公证凭据，也不以 Rosetta 启动替代 Intel Mac 真机发行验收。

## Capabilities

### New Capabilities

无。

### Modified Capabilities

- `release-resource-bundling`: macOS 正式发行从仅支持 arm64 扩展为分别支持 arm64 与 Intel x86_64 的应用、运行资源和 DMG 产物。

## Impact

- 构建入口：`Makefile`、macOS DMG 构建参数及帮助文案。
- 原生资源：macOS FFmpeg、whisper.cpp 构建脚本，ASR manifest 和 staging 目录约定。
- 测试与文档：发行脚本契约测试、README 和运行资源发行说明。
- 工具链：Intel 交叉构建需要 Rust `x86_64-apple-darwin` target，并复用本机 macOS SDK、Clang 和 CMake。
