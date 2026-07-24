## 真实验收记录

### 单房浏览器解析与录制

- 验收时间：2026-07-24（Asia/Shanghai）
- 平台：macOS arm64，Tauri 开发构建，系统 WKWebView
- 实际公开房间：`https://live.douyin.com/168376497175`
- 稳定 `web_rid`：`168376497175`
- 当次 `room_id`：`7665777078012480298`
- SQLite 逻辑会话：`81`，主播：`8`，续录计数：`1`

控制台和当日 JSONL 记录了原生响应、浏览器导航、旧文档 `pending`、目标文档 `live` 与 `start_recording`。浏览器目标结果出现后约 1 秒创建第二个 FFmpeg 输出目录，证明共享 WebView 能在地址刷新阶段确认目标房间并启动同一逻辑会话的续录。

第一份已登记 MKV 的 FFprobe 结果为 H.264 视频（720x1270）和 AAC 双声道音频（44.1 kHz），时长 94.79 秒，大小 25,567,466 字节。第二次续录生成 32,768,000 字节 MKV，FFprobe 能识别同样的 H.264 与 AAC 轨道；随后应用被终止，文件没有完整时长且 `segments.csv` 为空，因此不把它算作已完成分片，也不以此完成多房长时验收。

全过程日志仅包含白名单诊断字段，没有 Cookie、页面正文、Flight 原文或签名直播流 URL。

### 多房长时验收

尚未完成。已有授权监控对象在本轮验收时只有一个短暂在线，随后明确下播；陌生公开直播间只用于只读在线状态筛选，不用于录制。任务必须等待至少三个有权录制的公开房间同时在线，并连续运行至少 30 分钟后才能勾选。

2026-07-24 03:37（Asia/Shanghai）使用最新固定 debug 构建再次启动客户端，检查数据库中全部 4 个已启用且未归档的授权对象：`625411260021`、`598666330345`、`703940802949`、`168376497175`。四次原生访问均返回 HTTP 200、`pacePayload=true`、`supportedRoom=true` 和明确 `offline`，数据库状态均为 `offline/waiting`，活动录制会话为 0。每次访问在控制台和 `/Users/yino/Library/Logs/com.yino.dyscreen/dy-screen-2026-07-23.jsonl` 中分别形成 `native/response` 与 `monitor/result` 记录，且敏感字段扫描没有发现 URL、Cookie、`auth_key` 或直播流字段。由于 0 路在线，本次没有启动 FFmpeg，应用和 Vite 在检查后均已停止。

随后又以至少 5 秒间隔对数据库中 4 个已归档或暂停的历史对象 `559686664524`、`755507917100`、`323572698176`、`248851548659` 执行只读原生诊断；它们同样返回 HTTP 200、受支持页面结构和明确 `offline`。检查没有恢复主播、修改监听开关或启动录制，因此当前全部 8 个既有对象均不能提供三路长时验收条件。

### 自动化验证与交付审计

2026-07-24 对 proposal、design、四份 delta spec、生产代码、自动化测试、README、Makefile 和任务清单完成逐项审计：

| 审计范围 | 生产证据 | 验证证据 | 结论 |
| --- | --- | --- | --- |
| 原生 HTTP 与浏览器双通道、访问分类和安全快照 | `src/resolver.rs`、`src/access.rs`、`src/browser_snapshot.rs`、`src-tauri/src/room_resolution.rs` | `tests/resolver.rs`、`tests/browser_snapshot.rs`、`src-tauri/tests/room_resolution.rs` | 已覆盖直播、离线、访问限制、未知结构、粘性模式、唯一恢复探测和旧并发结果隔离 |
| 持久化 WebView、串行调度、人工验证和窗口权限 | `src-tauri/src/tauri_browser.rs`、`src-tauri/src/room_resolution.rs` | `src-tauri/tests/room_resolution.rs`、`src-tauri/src/tauri_browser.rs` 内单元测试 | 已覆盖旧文档隔离、目标 `web_rid`、导航白名单、禁止新窗口/下载、清除会话和验证恢复 |
| 监听状态、轮询、续录和逐次安全日志 | `src-tauri/src/supervisor.rs`、`src-tauri/src/database.rs`、`src-tauri/src/app.rs` | `src-tauri/tests/supervisor.rs`、`src-tauri/tests/supervisor_profile.rs`、`src-tauri/tests/access_fixture_acceptance.rs` | 已覆盖至少 5 秒串行访问、验证等待、同会话续录状态、`monitor` 汇总通道及 stderr/JSONL 白名单一致性 |
| 客户端访问状态与操作 | `ui/src/App.tsx`、`ui/src/api.ts`、`ui/src/types.ts` | `ui/src/App.test.tsx`、`ui/src/api.test.ts` | 已覆盖横幅、立即验证、重新检查、清除确认、通知去重、状态恢复和浏览器演示降级 |
| 文档和诊断入口 | `README.md`、`Makefile` | `make check`、`openspec validate --all --strict` | 使用、排障、隐私边界、聚焦测试、本地 fixture 和真实验收命令均已记录 |
| 真实公开房间 | 本文“单房浏览器解析与录制”记录 | 会话 `81`、完整 H.264/AAC MKV、控制台和 JSONL | 单房解析、录制和一次同会话续录启动已证明；三房 30 分钟验收仍待授权直播源 |

最终命令结果：

- `make check`：Rust 两个 workspace 的格式和严格 Clippy 通过，核心与 Tauri 全量测试通过，React `58/58` 通过，前端生产构建通过；需要真实 ASR 资源或 FFmpeg 样本的既有 ignored 测试保持原状。
- `cargo test --manifest-path src-tauri/Cargo.toml --test room_resolution`：`11/11` 通过，包含“旧并发原生成功不能清除浏览器粘性模式”的回归测试。
- `openspec validate --all --strict`：`13/13` 通过。
- `git diff --check`：通过。

ASR 与 AI 工作区隔离审计：`openspec/changes/add-user-triggered-ai-asr/` 保持存在且未被本变更归档或删除；AI 工作区的创建、导入、环境诊断和项目生命周期测试随 `make check` 一并通过。本变更只在共享客户端文件中追加抖音访问会话交互，没有删除 AI 页面能力。

### 验收数据清理

- 清理前通过 SQLite `.backup` 创建 `/private/tmp/dy-screen-before-cleanup-20260724.sqlite3`，备份完整性为 `ok`。
- 精确删除错误测试会话 `80` 及目录 `/Users/yino/Downloads/dy-screen/7665777078012480298/20260723T182616.438055000Z-0000`，该会话没有已登记视频。
- 将 `output_root` 从临时验收目录恢复为 `/Users/yino/Downloads/dy-screen`。
- 清理后数据库完整性为 `ok`；会话 `81`、其完整视频记录和对应 MKV 文件均保留。
