# dy-screen 文档、Makefile 与 OpenSpec 中文化实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 为普通录制用户和开发者提供可直接执行的中文 README 与 Makefile，并将 `openspec/` 下所有人类可读内容中文化且保持 OpenSpec 校验通过。

**Architecture:** Makefile 只封装现有 Cargo、FFmpeg、FFprobe 和 CLI，不引入新运行时。README 以 Make 命令为首选入口并保留 Cargo 原生命令。OpenSpec change 文件保持原有意图和结构，delta specs 同步生成中文主 specs，机器字段与解析关键字保留英文。

**Tech Stack:** GNU Make、Rust/Cargo、FFmpeg/FFprobe、OpenSpec、Markdown、YAML

---

### Task 1: 创建零额外依赖 Makefile

**Files:**
- Create: `Makefile`

- [x] **Step 1: 定义工具、直播参数与默认值**

定义 `CARGO`、`FFMPEG`、`FFPROBE`、`ROOM_URL`、`ROOM_URLS`、`QUALITY`、`PROTOCOL`、`OUTPUT`、`SEGMENT_SECONDS`、`PROBE_TIMEOUT_SECONDS`、`JSON` 和发布二进制路径。默认直播间使用 `https://live.douyin.com/452086788686`，默认目标为 `help`。

- [x] **Step 2: 添加开发目标**

添加 `help`、`doctor`、`build`、`release`、`fmt`、`fmt-check`、`lint`、`test`、`check` 和 `clean`。`check` 顺序执行格式检查、Clippy 和全目标测试；`doctor` 使用 `command -v` 检查 Cargo、FFmpeg 与 FFprobe。

- [x] **Step 3: 添加录制目标**

添加 `resolve`、`record` 和 `record-multi`。所有 CLI 参数以独立 argv 传递，不使用 `eval`；`record-multi` 要求调用者通过 `ROOM_URLS="url1 url2"` 提供至少两个地址。

- [x] **Step 4: 验证 Makefile 语法与帮助输出**

Run: `make help`

Expected: 输出所有公开目标及常用覆盖参数，退出码为 0。

Run: `make -n resolve record record-multi ROOM_URLS="https://live.douyin.com/A https://live.douyin.com/B"`

Expected: 展开为 `dy-screen resolve/record` 命令，参数与变量一致，不启动网络请求。

### Task 2: 重写中文 README

**Files:**
- Modify: `README.md`

- [x] **Step 1: 重组普通用户入口**

依次提供项目定位、已验证结论、功能边界、依赖安装、快速开始、单房间录制、多房间录制、停止方式和输出目录说明。快速开始首选 `make doctor`、`make release`、`make resolve`、`make record`。

- [x] **Step 2: 补充开发者入口**

列出 Make 目标和可覆盖变量，提供 Cargo 原生命令等价示例，说明 Rust 模块职责及 Tauri 2.0 managed state、command、event、sidecar 的接入边界。

- [x] **Step 3: 明确风险和非目标**

说明当前采用直播源直录而非浏览器画面录屏；不包含弹幕/礼物/UI；不绕过登录、验证码、DRM 或权限；暂不实现重连、磁盘配额、ASR、NLP、高光和切片。

- [x] **Step 4: 核对 README 与 Makefile**

Run: `rg -n '^[-[:alnum:]_]+:|ROOM_URL|ROOM_URLS|QUALITY|SEGMENT_SECONDS' Makefile README.md`

Expected: README 中提到的目标和变量均能在 Makefile 中找到。

### Task 3: 中文化 OpenSpec change 与配置

**Files:**
- Modify: `openspec/config.yaml`
- Keep machine-only: `openspec/changes/validate-tauri-multi-room-recording/.openspec.yaml`
- Modify: `openspec/changes/validate-tauri-multi-room-recording/proposal.md`
- Modify: `openspec/changes/validate-tauri-multi-room-recording/design.md`
- Modify: `openspec/changes/validate-tauri-multi-room-recording/tasks.md`
- Modify: `openspec/changes/validate-tauri-multi-room-recording/specs/douyin-stream-discovery/spec.md`
- Modify: `openspec/changes/validate-tauri-multi-room-recording/specs/multi-room-recording/spec.md`

- [x] **Step 1: 读取 OpenSpec 模板规则**

Run: `openspec instructions proposal --change validate-tauri-multi-room-recording --json`

Run: `openspec instructions design --change validate-tauri-multi-room-recording --json`

Run: `openspec instructions specs --change validate-tauri-multi-room-recording --json`

Run: `openspec instructions tasks --change validate-tauri-multi-room-recording --json`

Expected: 获得每个既有 artifact 的结构规则，确认可翻译正文与必须保留的结构。

- [x] **Step 2: 中文化配置和规划文档**

将 `config.yaml` 注释、proposal、design 和 tasks 的人类可读内容翻译为中文。保留 `schema: spec-driven`、日期、代码标识符、命令和路径。所有已完成任务继续使用 `[x]`。

- [x] **Step 3: 中文化 delta specs**

保留 `## ADDED Requirements`、`### Requirement:`、`#### Scenario:`、`**WHEN**`、`**THEN**`，将需求标题、规范性描述和场景条件/结果翻译为中文。规范性描述继续包含 `SHALL` 或 `MUST NOT`，避免削弱需求语义。

- [x] **Step 4: 检查 change 内部一致性**

核对 proposal 的能力名称、design 的决策、tasks 的完成项以及两份 delta specs 的需求范围一致，不改变已实现范围。

### Task 4: 同步中文主规格

**Files:**
- Create: `openspec/specs/douyin-stream-discovery/spec.md`
- Create: `openspec/specs/multi-room-recording/spec.md`

- [x] **Step 1: 从中文 delta specs 创建主规格**

每份主规格包含 `# <能力名> Specification`、`## Purpose` 和 `## Requirements`。Purpose 使用完整中文描述，不使用占位符；Requirements 复制并规范化对应 delta 中的中文需求，去掉 `ADDED` 层级。

- [x] **Step 2: 验证同步幂等性和结构**

再次逐项比较 delta 与 main spec，确保需求和场景数量一致，不重复需求。

### Task 5: 全量验证与交付

**Files:**
- Verify: `README.md`
- Verify: `Makefile`
- Verify: `openspec/**/*.md`
- Verify: `openspec/**/*.yaml`

- [x] **Step 1: 运行 Makefile 开发检查**

Run: `make check CARGO=/Users/yino/.rustup/toolchains/stable-aarch64-apple-darwin/bin/cargo`

Expected: `cargo fmt --check`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all-targets` 全部通过，共 23 个测试成功。

- [x] **Step 2: 构建发布二进制并检查 CLI**

Run: `make release CARGO=/Users/yino/.rustup/toolchains/stable-aarch64-apple-darwin/bin/cargo`

Run: `./target/release/dy-screen --help`

Expected: 发布构建退出码为 0，CLI 显示 `resolve` 和 `record`。

- [x] **Step 3: 运行 OpenSpec 校验**

Run: `openspec status --change validate-tauri-multi-room-recording`

Run: `openspec validate validate-tauri-multi-room-recording`

Expected: 4/4 artifacts complete，change valid。

- [x] **Step 4: 检查中文化覆盖与工作区状态**

Run: `rg -n 'The system|The planned|The current repository|Alternative considered|Goals:|Non-Goals:|Open Questions' openspec`

Expected: 无匹配。

Run: `git status --short`

Expected: 显示本次新增/修改文件；不提交、不归档、不删除用户原有 `package.json`。
