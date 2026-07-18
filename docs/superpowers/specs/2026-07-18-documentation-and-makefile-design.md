# dy-screen 文档与 Makefile 设计

## 目标

为普通录制用户和项目开发者提供统一、中文、可执行的项目入口，同时保持 OpenSpec 文件可被命令行工具正确解析。

## README 设计

README 采用由浅入深的结构：项目定位与验证结论、快速开始、Make 命令、单房间与多房间录制、Cargo 原生命令、输出结构、Tauri 2.0 接入边界、已知限制及合规说明。普通用户应能通过复制 `make record` 示例开始录制，开发者应能通过 `make check` 完成格式、静态检查和测试。

## Makefile 设计

Makefile 不引入 `cargo-make`、`just` 或额外脚本，仅封装现有 Cargo、FFmpeg 和 FFprobe 命令。默认目标为 `help`，并提供：

- 环境检查：`doctor`
- 开发构建：`build`
- 发布构建：`release`
- 格式化与检查：`fmt`、`fmt-check`、`lint`、`test`、`check`
- 直播解析：`resolve`
- 单房间录制：`record`
- 多房间录制：`record-multi`
- 构建产物清理：`clean`

录制目标通过 `ROOM_URL`、`ROOM_URLS`、`QUALITY`、`PROTOCOL`、`OUTPUT`、`SEGMENT_SECONDS`、`PROBE_TIMEOUT_SECONDS`、`FFMPEG`、`FFPROBE` 和 `JSON` 覆盖默认值。Makefile 面向 macOS/Linux；Windows 开发阶段使用 WSL 或 Git Bash，产品分发由后续 Tauri sidecar 方案处理。

## OpenSpec 中文化设计

`openspec/` 下所有面向人的标题、说明、需求、场景和任务翻译为中文。为了保持工具兼容性，以下机器接口不翻译：

- YAML 键、schema 值、日期以及 change 目录名；
- OpenSpec 结构标记，包括 `ADDED Requirements`、`Requirement`、`Scenario`、`WHEN`、`THEN`；
- 代码标识符、命令、协议名、清晰度名和路径。

当前 change 的两份 delta spec 将同步为 `openspec/specs/` 下的中文主规格。change 保持活动状态，不执行归档。

## 验证

完成后执行：

1. `make help`、`make doctor` 和 `make check`；
2. `make release` 并检查 CLI help；
3. `openspec status` 与 `openspec validate`；
4. 检查 OpenSpec 人类可读正文是否仍残留成段英文；
5. 检查 README 中的 Make 命令与 Makefile 实际目标、变量一致。

## 非目标

本次不修改 Rust 录制逻辑、不接入 Tauri UI、不实现 ASR/NLP/高光切片，也不归档或提交 OpenSpec change。
