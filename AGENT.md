# 切片智能体项目开发手册

本文件适用于仓库根目录及全部子目录。所有参与本项目的 AI 代理和开发者都必须遵守本文件；用户在当前任务中的明确要求优先级最高。

## 1. 项目定位

切片智能体是一个基于 Tauri 2.0、React、TypeScript、Rust、SQLite 和 FFmpeg 的本地桌面应用，主要能力包括：

- 通过公开抖音个人主页或直播间链接添加主播；
- 后台监听多个主播的直播状态；
- 主播开播后自动录制包含视频和声音的 MKV 分片；
- 保存录制会话和视频元数据；
- 在客户端中浏览、预览和管理历史视频；
- 对已完成视频执行本地 ASR、DeepSeek 精彩分析、字幕校对和 MP4 导出；
- 同步版本化转场素材目录，并通过受限 Agent 匹配相邻精彩的带声桥接素材。

当前桌面客户端版本为 `0.3.0`。录制、监听、数据持久化、ASR、AI 剪辑和转场素材能力都必须以代码、自动化测试及对应 OpenSpec 为准，不得根据界面占位或任务勾选推断功能已经实现。

## 2. 强制规则

### 2.1 语言

- 所有项目文档、OpenSpec 文档、业务注释、提交说明和面向用户的文案必须使用中文。
- Rust、TypeScript、SQL 标识符以及第三方 API、协议、命令名称可以保留英文。
- 错误信息必须使用清晰、可操作且经过脱敏的中文。
- 不要在文档中遗留 `TODO`、`TBD`、占位描述或无法执行的模糊步骤。

### 2.2 修改范围

- 只修改当前任务明确涉及的文件和行为。
- 工作区中已有的修改、未跟踪文件和其他 OpenSpec 变更均视为用户内容，禁止覆盖、删除、归档或顺带提交。
- 修改前必须执行 `git status --short`，确认工作区状态。
- 不得因为格式化、重构或测试方便而改动无关代码。
- 不得擅自归档 OpenSpec、创建 Git 提交、推送分支或创建 PR，除非用户明确要求。

### 2.3 安全操作

- 禁止使用 `git reset --hard`、`git clean -fd`、未经确认的 `git checkout --` 等破坏性命令。
- 禁止覆盖或删除用户录像、SQLite 数据库和预览缓存。
- 涉及视频或会话删除时，必须保证数据库状态和文件状态可恢复或可重试。
- 修改文件优先使用补丁方式，保留文件现有结构和用户改动。

### 2.4 测试与验证

- 新功能、行为修改和缺陷修复必须有对应自动化测试。
- 缺陷修复应先增加能够复现问题的失败测试，再修改生产代码。
- 数据库 migration 必须覆盖旧数据库、幂等执行、数据保留、外键关系和冲突数据。
- 不得仅凭代码阅读宣称功能完成；必须运行与修改范围相匹配的验证命令。
- 测试失败时必须说明真实失败原因，不得通过删除断言、跳过测试或放宽约束掩盖问题。

## 3. 项目结构

```text
dy-screen/
├── src/                         Rust 直播解析、主页发现、录制和多任务核心
├── tests/                       Rust 核心集成测试和脱敏页面 fixture
├── ui/                          React + TypeScript + Vite 前端
│   └── src/
│       ├── App.tsx              监控中心、视频库、设置和 AI 工作区入口
│       ├── AiWorkspace.tsx      ASR、精彩候选、剪辑工作台和转场素材交互
│       ├── api.ts               Tauri command 与浏览器演示 API
│       ├── types.ts             前端领域类型
│       └── styles.css           客户端样式
├── src-tauri/                   Tauri 2.0 桌面后端
│   ├── src/app.rs               Tauri command、事件、托盘和应用生命周期
│   ├── src/app_lifecycle.rs     单实例锁、关闭领取和生命周期安全诊断
│   ├── src/room_resolution.rs   原生 HTTP/WebView 双通道解析与共享会话
│   ├── src/tauri_browser.rs     受限验证窗口、最小快照和导航策略
│   ├── src/app_support.rs       command 公共校验和文件操作辅助
│   ├── src/streamer_service.rs  主播创建、编辑和来源校验服务
│   ├── src/database.rs          SQLite migration 和 repository
│   ├── src/supervisor.rs        监听 worker、自动录制、重试和磁盘保护
│   ├── src/preview.rs           MP4 预览转换、缓存和任务管理
│   ├── src/transition_materials.rs 素材目录同步、SQLite 模型和工程边界
│   ├── src/transition_assets.rs 素材下载、校验和 H.264 兼容预览
│   ├── src/transition_matching.rs 受限 DeepSeek 转场匹配 Agent
│   ├── src/ai/                  ASR、精彩、字幕、时间轴和 FFmpeg 导出
│   ├── src/domain.rs            Tauri 后端领域模型和 DTO
│   └── tests/                   后端集成测试
├── openspec/                    中文规格、活动变更和归档变更
├── examples/                    参考页面和示例资源
├── Makefile                     开发、测试、构建和诊断入口
└── README.md                    项目使用说明
```

## 4. 架构边界

### 4.1 Rust 核心

- `src/` 不依赖 Tauri UI，负责公开页面解析、流选择、FFmpeg 录制和多房间任务。
- `ProfileResolver` 只负责公开个人主页身份和稳定直播入口发现。
- `StreamResolver` 负责原生 HTTP 直播间状态和签名直播流解析；Tauri 业务层必须通过 `RoomResolutionService` 统一使用原生/WebView 双通道。
- FFmpeg 只能接收经过 `StreamResolver` 验证的直播流地址。
- 不得把完整 HTML、React Flight 载荷或签名直播流 URL写入日志、错误或事件。

### 4.2 Tauri 后端

- 前端不得直接访问 SQLite、文件系统或 FFmpeg。
- 数据读写和录制控制必须通过类型化 Tauri command 进入 Rust 后端。
- `streamer_service.rs` 负责业务编排，`database.rs` 负责持久化，`supervisor.rs` 负责长期任务状态机。
- 后台任务必须支持取消、暂停、恢复、应用退出和系统唤醒。
- 同一个主播最多只能存在一个有效监听 worker 和一个活动录制会话。
- 桌面进程必须在打开 SQLite 和执行启动恢复前获取独占实例锁；第二实例不得访问业务数据库或启动后台任务。
- `SIGTERM`、`SIGINT`、托盘和前端退出必须复用同一个幂等、有界关闭流程，禁止直接结束父进程后遗留 FFmpeg。
- 授权心跳只发布素材目录版本信号；素材同步、下载或转码故障不得撤销授权或阻塞监听、录制、ASR 和普通剪辑。
- 前端只能提交素材键、版本和工程边界 ID，不得提交任意下载 URL、本地路径、FFmpeg 参数或滤镜字符串。

### 4.3 React 前端

- `ui/src/types.ts` 必须与 Rust DTO 的序列化字段保持一致。
- Rust 使用 `camelCase` 输出时，前端不得自行猜测或重复转换字段。
- Tauri 事件用于及时刷新状态，主动查询用于应用恢复和事件丢失后的最终一致性。
- 浏览器演示模式只模拟界面行为，不得伪装真实主页解析、网络轮询、SQLite 或 FFmpeg 录制。
- 转场素材列表必须只读取本地 SQLite DTO；同步状态通过类型化查询和 `transition-material-event` 恢复，不得在 WebView 直接访问素材 API 或 CDN。

## 5. 核心业务约束

### 5.1 三层主播身份

- `profile_sec_uid`：个人主页稳定身份。
- `web_rid`：长期稳定的直播入口身份，对应 `https://live.douyin.com/{web_rid}`。
- `room_id`：当前直播场次身份，可能在下一次开播时变化。
- 长期去重不得只依赖 `room_id`。
- 更新 `room_id` 时必须复用原主播、历史会话和监听 worker。

### 5.2 个人主页发现

- 只接受 `douyin.com/user/{profile_sec_uid}` 或 `www.douyin.com/user/{profile_sec_uid}`。
- 持久化前移除查询参数和 fragment。
- 优先使用结构化数据中的主页身份、`roomIdStr`、直播对象和 `owner.web_rid`。
- “直播中”或“正在直播”文字不能单独用于推断直播间 ID。
- 结构化直播对象必须能够与目标主页身份或目标 `roomIdStr` 建立关联，禁止绑定推荐列表中的其他直播间。
- 主页未开播时允许保存主播，并进入等待首次开播状态。

### 5.3 监听状态机

- 未发现 `web_rid` 的个人主页按 60 秒基础周期加 0–10 秒抖动检查。
- 主页可重试错误按 60、120、300 秒封顶退避。
- 已发现且离线的直播入口按 60 秒基础周期加 0–15 秒抖动检查，所有公开页面访问至少间隔 5 秒。
- 原生 HTTP 返回 `access_restricted` 后必须切换共享持久 WebView，并在冷却期内避免重复原生请求。
- `verification_required` 必须等待用户主动处理，不能记为下播、入口失效或消耗录制续接次数。
- 普通离线和网络错误不得清除稳定 `web_rid`。
- 只有连续 3 次明确入口失效，个人主页来源才回退到重新发现阶段。
- 等待录制资源后必须重新解析直播间，并持久化最新 `room_id` 后再启动录制。

### 5.4 录制和预览

- 原始录像默认使用 MKV 分片，避免异常中断导致整场文件不可用。
- 原始录像是数据源，不得被预览转换覆盖或替换。
- 内置预览按需生成 MP4 缓存：优先重封装，不兼容时转为 H.264 + AAC。
- FFmpeg/FFprobe 必须通过参数数组调用，禁止拼接未经校验的 shell 命令。
- 任务取消、应用退出或转换失败后必须清理 `.part` 等临时文件。
- 签名流地址仅能在内存中用于当前解析和录制，不得持久化到 SQLite。

### 5.5 转场素材和匹配 Agent

- `(asset_key, asset_version)` 是不可变素材标识；目录更新不得覆盖历史工程固定引用或删除仍被引用的缓存。
- 正式环境只接受 HTTPS；绝对素材 URL 必须与 `cdnBaseUrl` 同源，开发态 HTTP 只允许显式 localhost。
- `changed=true` 的完整目录必须先全量校验，再在单个 SQLite 事务中发布；任一记录非法时继续使用上一目录。
- 媒体只在预览、人工应用或 Agent 高置信度选中后下载，必须限制大小、校验 SHA-256、使用临时文件和原子发布。
- HEVC 等 WebView 不兼容源只生成 H.264/AAC 预览缓存；最终导出使用已校验源文件。
- 转场 Agent 只能接收有界相邻 ASR、主播标签和素材语义字段，候选最多 12 个；禁止发送 URL、路径、凭据、设备 ID、媒体或任意工具权限。
- 只有合法且置信度不低于 `0.75` 的当前 `bridge` 候选可以自动应用；低分只保存建议，人工锁始终优先。
- 桥接素材是独立时间轴单元，保留原声并增加总时长；桥接期间不得显示或烧录来源字幕。

## 6. 标准开发流程

1. 阅读本文件、`README.md`、相关 OpenSpec 和目标代码。
2. 执行 `git status --short`，识别用户已有修改。
3. 判断任务属于分析、诊断、提案、实现、归档还是提交。
4. 涉及新功能或行为变化时，优先创建或更新 OpenSpec。
5. 先编写能够描述目标行为的测试。
6. 运行目标测试，确认测试因缺少目标行为而失败。
7. 实现最小且聚焦的修改。
8. 运行目标测试、格式检查和相关回归测试。
9. 根据风险运行 `make check` 或 `make verify`。
10. 检查 `git diff --check` 和 `git status --short`。
11. 向用户报告修改内容、验证结果、已知限制和未处理事项。

诊断类任务默认只分析和说明原因；除非用户同时要求修复，否则不要直接修改生产代码。

## 7. OpenSpec 工作流

### 7.1 基本要求

- `openspec/` 下所有规格、提案、设计、任务和归档文档必须使用中文。
- Requirement 和 Scenario 的标题、描述必须清晰，不得使用模糊占位词。
- OpenSpec 中的 `SHALL`、`MUST`、`WHEN`、`THEN` 等格式关键字可以保留英文。
- 一个变更只处理一个边界清晰的目标；范围过大时应拆分变更。
- 不得修改、归档或提交其他活动变更。

### 7.2 常用流程

```bash
# 查看活动变更
openspec list --json

# 查看变更状态
openspec status --change "<change-name>" --json

# 获取实施指令
openspec instructions apply --change "<change-name>" --json

# 严格校验全部规格和活动变更
openspec validate --all --strict
```

标准顺序：

1. 提案：生成 `proposal.md`、`design.md`、delta specs 和 `tasks.md`。
2. 实施：逐项完成任务，并及时更新 `- [ ]`/`- [x]`。
3. 同步：将 delta specs 智能合并到 `openspec/specs/`，保留主规格中未被修改的要求。
4. 校验：执行 `openspec validate --all --strict`。
5. 归档：仅在用户明确要求后移动到按日期命名的 archive 目录。

存在未完成任务时，归档前必须明确报告数量和原因。平台风控、登录限制或待实机验证等外部阻塞必须保留在任务记录中，不得为了显示完成而强行勾选。

## 8. 编码规范

### 8.1 Rust

- 使用 `cargo fmt` 兼容格式，Clippy 不允许新增告警。
- 错误类型应提供稳定分类和脱敏的 `safe_message` 或等价输出。
- 异步外部请求和子进程必须设置超时并支持取消。
- 不要在持有同步 `Mutex` 锁时执行 `.await`。
- worker、录制任务和预览任务必须具有明确的生命周期和清理路径。
- 对可能被旧任务回写的状态使用 generation、request ID 或 cancellation token 防止竞态。
- 不要使用 `unwrap()` 处理生产环境可恢复错误；测试代码可以在前置条件明确时使用。

### 8.2 SQLite

- migration 必须在事务中执行，并且可重复检查版本。
- 表重建必须保留主播、归档状态、监听开关、录制会话和视频外键关系。
- 新增唯一身份时要考虑合法旧数据冲突，不能让升级直接失败或自动删除历史。
- 多步骤身份绑定、恢复和合并必须保持原子性。
- 双方都存在历史数据时不得自动合并或删除记录，应暂停冲突记录并返回可诊断状态。

### 8.3 TypeScript 和 React

- 开启并保持 TypeScript 类型检查通过，避免使用无必要的 `any`。
- API 类型、事件类型和 Rust DTO 必须同步修改。
- 表单最终有效性以后端校验为准，前端校验只用于即时反馈。
- 异步操作必须展示加载、失败、重试或禁用状态，避免重复提交。
- 状态事件处理必须校验主播、视频或请求身份，避免旧事件覆盖新任务。

### 8.4 测试和 fixture

- 测试名称应直接描述业务行为。
- 优先测试真实 repository、状态机和解析函数，不要只验证 mock 被调用。
- 外部网络、系统通知和 FFmpeg 进程可以在明确边界处使用测试替身。
- HTML fixture 必须脱敏，不得包含真实 Cookie、访问令牌或可用签名流 URL。
- 测试需覆盖成功、离线、网络错误、页面结构变化、取消、重启恢复和冲突数据。

## 9. 常用命令

### 9.1 环境和开发

```bash
make doctor       # 检查 Node、npm、Cargo、FFmpeg 和 FFprobe
make install      # 安装前端依赖
make web-dev      # 仅启动浏览器演示界面
make app-dev      # 启动 Tauri 客户端；仅保留前端热更新，Rust 修改后需安全手动重启
make app-build    # 构建桌面应用
```

### 9.2 格式、测试和验证

```bash
make fmt                      # 格式化两个 Rust workspace
make fmt-check                # 检查 Rust 格式
make lint                     # 对两个 Rust workspace 执行 Clippy
make test                     # 前端、核心和 Tauri 全部测试
make check                    # 格式、Clippy、测试和前端构建
make spec-validate            # OpenSpec 严格校验
make verify                   # make check + OpenSpec 校验 + 桌面构建
make test-profile             # 个人主页解析测试
make test-migration           # SQLite 身份 migration 测试
make test-supervisor-profile  # 个人主页/直播间双阶段状态机测试
make test-preview             # 视频预览服务和前端播放器测试
make test-preview-integration # 使用真实 FFmpeg 样本验证预览转换
make clip-subtitle-doctor     # 审计 H.264/HEVC/AAC 和桥接导出 FFmpeg 能力
make test-clip-transition-integration FFMPEG=... FFPROBE=... FFMPEG_FIXTURE_GENERATOR=... # 真实带声小样验收
make test-transition-catalog-fixture TRANSITION_CATALOG_FIXTURE=... # 服务端 33 条目录验收
make test-browser-access      # 双通道解析、WebView、安全日志和前端聚焦测试
make test-app-lifecycle      # 单实例锁和幂等关闭领取测试
make accept-access-fixtures  # 本地支持页/验证页 stderr 与 JSONL 验收
```

### 9.3 验证选择

- 只修改 Markdown 文档：检查内容和 `git diff --check`；涉及 OpenSpec 时额外执行 `make spec-validate`。
- 修改 Rust 核心：运行目标测试，然后执行 `make fmt-check`、`make lint` 和 `make check`。
- 修改 Tauri/SQLite/supervisor：运行对应 `src-tauri` 测试，再执行 `make check`。
- 修改 React/TypeScript：执行 `npm test`、`npm run build`，最终执行 `make check`。
- 修改预览或 FFmpeg 行为：执行 `make test-preview`；环境具备 FFmpeg 时执行 `make test-preview-integration`。
- 修改转场目录、素材预览或剪辑导出：运行目录/repository 目标测试、`make clip-subtitle-doctor` 和 `make test-clip-transition-integration`；服务端样例变化时额外执行 `make test-transition-catalog-fixture`。
- 修改发布、配置或桌面生命周期：执行 `make verify`。
- 录制期间禁止使用 Tauri Rust watcher；项目开发入口必须保持 `--no-watch`，Rust 改动后先等待 `shutdown_completed` 再重启。

真实公开页面验收只能作为自动化测试的补充，不能替代 fixture 和状态机测试。

## 10. Git 规范

### 10.1 暂存

- 提交前执行 `git status --short`、`git diff` 和 `git diff --check`。
- 必须精确列出要暂存的文件，禁止在存在用户修改时直接执行 `git add .`。
- 暂存后使用 `git diff --cached --name-only` 和 `git diff --cached --check` 再次核对。
- 只提交当前任务文件，其他用户修改必须保持未暂存状态。

### 10.2 提交信息

提交必须包含简洁主题、详细中文正文和标准 AI 标识。例如：

```text
feat: 支持个人主页直播发现

- 新增公开个人主页解析和访问控制分类
- 增加 SQLite 身份迁移及冲突测试
- 更新客户端状态展示和中文文档

AI-Assisted-By: OpenAI Codex
```

主题建议使用：

- `feat:` 新功能；
- `fix:` 缺陷修复；
- `docs:` 文档修改；
- `test:` 测试修改；
- `refactor:` 不改变行为的重构；
- `chore:` 构建、依赖或维护工作。

AI 标识统一使用以下格式：

- Codex：`AI-Assisted-By: OpenAI Codex`
- Claude：`AI-Assisted-By: Anthropic Claude`
- Gemini：`AI-Assisted-By: Google Gemini`
- 其他代理：`AI-Assisted-By: <组织或模型正式名称>`

禁止使用含糊的 `AI generated`、`机器人提交` 或无法识别具体代理的标识。

## 11. 安全、隐私和合规边界

- 只支持普通公开页面；原生 HTTP 受限时允许使用隔离、持久化且权限受限的系统 WebView 执行标准页面脚本。
- 禁止应用逻辑导入、复制、导出、记录或注入账号 Cookie、登录态、验证码结果和私有访问令牌；平台 WebView browsing data 只能由系统管理并允许用户显式清除。
- 禁止自动识别验证码、模拟验证交互、第三方打码、代理池、账号自动登录或其他访问控制绕过。
- 禁止绕过付费、私密直播、DRM、地区限制或其他权限控制。
- 页面要求登录、验证码或额外访问权限时，应返回脱敏错误并按策略重试或停止。
- 不得将主播信息、视频元数据、录像、日志或设置上传到外部服务，除非后续规格和用户明确授权。
- DeepSeek 精彩和转场匹配只接收用户授权的最小文本字段；禁止发送激活码、设备 ID、Cookie、下载 URL、本地路径、视频或音频。
- 远程素材 URL、激活请求头和绝对缓存路径必须留在 Rust；前端只接收素材语义 DTO、状态和不可逆预览句柄。
- 不得在日志、事件、SQLite、README、测试输出或提交信息中泄露签名流查询参数。
- 录像和内容处理必须遵守平台规则、版权要求、隐私要求及适用法律。

## 12. 完成定义

只有同时满足以下条件，才能向用户报告任务完成：

- 实现与用户要求和 OpenSpec 一致；
- 新增或修改行为具有自动化测试；
- 相关目标测试通过；
- 必要的格式检查、Clippy、前端构建和 OpenSpec 校验通过；
- 未引入签名 URL、Cookie、HTML 载荷或本地敏感路径泄露；
- README、Makefile、类型和规格已按需要同步；
- `git status --short` 中只剩明确保留的用户修改；
- 已向用户说明验证命令、产物位置、已知限制和外部阻塞；
- 未在没有授权的情况下归档、提交、推送或删除内容。

若任务因平台风控、缺少依赖、等待用户选择或需要实机验证而无法完成，必须明确报告阻塞条件，并保留已经完成且可验证的工作。
