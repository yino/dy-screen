## 1. 架构契约与测试

- [x] 1.1 增加 macOS 打包契约测试，覆盖架构映射、Intel target、资源平台、DMG 命名和非法架构门禁
- [x] 1.2 扩展 ASR manifest 与资源测试，声明 `macos-x86-64` CPU 平台及目标架构文件路径

## 2. macOS 原生运行资源

- [x] 2.1 扩展 FFmpeg macOS 构建脚本，支持 `aarch64`/`x86_64` 目标参数并校验全部 Mach-O 切片与相对依赖
- [x] 2.2 扩展 whisper.cpp macOS 构建脚本，为 arm64 保留 Metal、为 x86_64 使用可移植 CPU 构建并校验 Mach-O 切片
- [x] 2.3 增加 macOS 可信资源源组装脚本，把目标架构原生资源与共用模型、字典、许可证合并为单平台 manifest

## 3. 打包与发布入口

- [x] 3.1 重构 Makefile 的 macOS staging、校验、发布和 Tauri 构建目标，使其统一使用 `MACOS_ARCH` 派生值并拒绝未知架构
- [x] 3.2 增加 Apple Silicon 构建 Intel `.app`/DMG 的便捷入口、工具链诊断和架构专属产物路径
- [x] 3.3 保持现有 arm64 默认命令兼容，并验证 Tauri 包内应用、sidecar 与动态库架构一致

## 4. 文档与验收

- [x] 4.1 更新 README、帮助文案和资源基线，记录 Intel 资源构建、打包命令、产物位置与限制
- [x] 4.2 运行脚本语法、Rust 契约测试、OpenSpec 严格校验、x86_64 交叉构建和 Mach-O 静态审计
- [ ] 4.3 在真实 Intel Mac 完成签名、公证、离线启动、录制、预览、封面、ASR 与剪辑导出验收并保存脱敏证据
