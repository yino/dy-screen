# v2 实施顺序与写域纪律

> 状态：本文件是 v2 **实施顺序、进入条件与写域归属的唯一来源**。任何 change 的 `tasks.md`、`proposal.md`、`design.md` 或 `docs/release-v2.md` 与本文件冲突时，以本文件为准，并回改冲突方。
>
> 口径来源：`docs/release-v2.md` §1（三层边界）与 §2（范围与非目标）。本文只规定“按什么顺序做、谁改哪些文件、怎么协作”，不重复定义能力契约。

## 1. 实施顺序

严格按 0 → 1 → 2 → 3 → 4 推进。后一步的进入条件未满足前，不开始后一步的实现任务。

| 步骤 | 内容 | 承载 change | 进入条件 |
| --- | --- | --- | --- |
| 0 | 百炼能力探针 | `validate-dashscope-direct-capabilities` | **已完成（2026-09-30）**，结论见 §6 |
| 1 | ASR 直连改造 | `wire-cloud-asr-upload-and-jobs` | ✅ 已满足：临时 token 可提交文件转写；音频上传须走三步直传流程（见 §6） |
| 2 | Harness sidecar 与模型调用直连 | `add-model-gateway-and-harness-sidecar` | ✅ 已满足：临时 token 可完成 LLM 调用；仍需步骤 1 交付并可独立验收 |
| 3 | 特效字幕 | `add-effect-caption-renderer` | **65/77 已实现并提交**；剩 12 项为结构性阻塞（随包渲染器/浏览器/字体资产 6、Intel/Windows 真机 5、步骤 2 依赖 1），逐条证据见 `openspec/changes/add-effect-caption-renderer/evidence/completion-audit.md` |
| 4 | 生成式动画 | `add-generative-video-animation` | ✅ 已满足：万相文生图与图生视频均实测 `SUCCEEDED`；仍需步骤 2 交付 |

### 已搁置

- `add-cloud-ai-job-backend`（服务端 AI 任务接口与分项账本）**本轮不实施，整体搁置**。仅保留两项供后续实现复用：
  1. `(账号, 任务类型, requestId)` 的幂等语义——在当前口径下实现为客户端本地 SQLite 落盘 `requestId` 与输入指纹，同键同输入复用、同键不同输入拒绝；
  2. 客户端 `src-tauri/src/api.rs` 单一 API 客户端的契约对齐方式——业务模块不得绕过它直接发请求。
- 恢复条件：产品决定重新引入服务端代理与分项账本时，由 Lead 重新评估并把该 change 从搁置状态改回实施队列。

## 2. 写域归属（实施期，按步骤）

同一写域在同一时刻只能有一个实施者；跨步骤共享的文件必须串行提交。

| 写域 | 归属步骤 |
| --- | --- |
| `src/asr/**` | 1 |
| `src-tauri/src/ai/processor.rs`、`src-tauri/src/ai/desktop_runtime.rs`、`src-tauri/src/ai/repository.rs`（迁移 26） | 1 |
| `src-tauri/src/ai/`（Harness sidecar 监管、Profile/Skill 打包）、`src-tauri/src/ai/llm.rs`、`highlight.rs`、`transition_matching.rs`、`clip_text_correction.rs`、`ui/src/App.tsx` 设置页 | 2 |
| 渲染编排与组合校验、`src-tauri/src/ai/clip_subtitle*.rs`、`src-tauri/src/ai/clip_export.rs`、`repository.rs`（迁移 27）、运行资源清单与发布校验 | 3 |
| `repository.rs`（迁移 28）、`clip_export.rs` 的 `replace`/`insert`、`src-tauri/src/transition_assets.rs`（复用下载与校验） | 4 |
| `docs/**`、`openspec/**` | 由 Lead 指派；文档改写期间不与代码写域并行修改同一 change 目录 |

`src-tauri/src/ai/repository.rs` 被步骤 1、3、4 共同使用：迁移版本号（26 → 27 → 28）必须按步骤顺序串行确定与合并，后一步不得在前一步合入前预占版本号。

## 3. 全队串行纪律：重型 Cargo 命令

`cargo test`、`cargo nextest run`、`cargo build`、`cargo clippy` 都会独占 `target/` 目录与 Cargo 构建锁。**同一时刻全队只允许一个成员运行重型 Cargo 命令，默认串行。**

1. 发起前在共享任务板或对 Lead 声明“即将运行 `<命令>`”，得到确认后运行。
2. 运行结束（无论成败）立即声明释放，让排队者继续。
3. 出现 `Blocking waiting for file lock on build directory` 时，说明他人正在运行：**排队等待，不要重试抢占**，也不要改用 `--target-dir` 另起一份构建目录来绕过（会造成磁盘与缓存膨胀）。
4. `cargo fmt --check`、纯 `openspec validate` 等轻量只读命令不占用该纪律；`cargo fmt`（写入）与 `cargo clippy` 视为重型命令。

## 4. 每一步的完成定义（DoD）

1. 该 change 的 `tasks.md` 全部勾选；未实施的任务不得预先勾选。
2. `openspec validate <change-name> --strict` 通过。
3. 在 macOS arm64、macOS Intel x86_64 与 Windows x64 三个目标上完成该步要求的验收；Intel 上至少完成一次端到端跑通。
4. 若该步引入随包组件（Harness sidecar、渲染浏览器、运行资源），必须同时提供三个架构的构建产物，并通过发布校验的哈希、架构与相对路径检查。

## 5. 跨步骤不变量

- 不新增 admin-service 接口；激活态按本地激活/心跳状态判定，**未开通与额度耗尽的唯一来源是上游业务错误码 3001 / 3003 / 3004**——heartbeat 响应不含额度字段，不得据它推断额度。
- 不做服务端分项账本；记账以客户端本地 SQLite 的 `requestId` + 输入指纹为准，重试只做同键复用。
- 不做本地 `whisper.cpp` 的自动回退；云端不可用即明确失败并报出可区分原因。
- 保留 macOS Intel x86_64；随包 sidecar、渲染浏览器与 Harness 都必须有 Intel 构建。
- 模型 MUST NOT 生成脚本；被渲染页面中只允许存在渲染器自带的、钉版本的引擎脚本，且渲染器运行时必须做完整性校验。
- 依赖图片或视频生成能力、或依赖音频直传路径的表述，一律以 `validate-dashscope-direct-capabilities` 的探针结论为准。

## 6. 步骤 0 完成证据（2026-09-30 实测）

以 `GET /api/client/ai/llm-ak` 签发的真实短期 token（`provider=ALI`）实测。凭据值、完整签名 URL 与上游任务 ID 均不记录。

### 6.1 能力矩阵

| 能力 | 结论 | 实测依据 |
| --- | --- | --- |
| 通义 LLM（兼容模式） | ✅ 可用 | `POST /compatible-mode/v1/chat/completions`，模型 `qwen-plus`，`usage.total_tokens=15` |
| 万相文生图 | ✅ 可用 | `POST /api/v1/services/aigc/text2image/image-synthesis`，模型 `wanx2.1-t2i-turbo`，终态 `SUCCEEDED`，`usage.image_count=1` |
| 万相图生视频 | ✅ 可用 | `POST /api/v1/services/aigc/video-generation/video-synthesis`，模型 `wanx2.1-i2v-turbo`，终态 `SUCCEEDED`，`usage.video_count=1` 且 `video_duration=5` |
| 百炼文件转写 | ✅ 可用（须走三步流程） | 终态 `SUCCEEDED`，`usage.seconds=4`，结果 JSON 句段含毫秒级 `begin_time`/`end_time` |
| 上游调用方幂等键 | ❌ 不支持 | 相同输入与相同自定义幂等键连续提交两次得到两个不同 `task_id`，响应头无幂等字段 |

**结论：四类能力在客户端直连口径下全部可用，不需要任何服务端新增接口。**

### 6.2 音频直传的三步流程（步骤 1 必须实现为这三步）

1. `POST /api/v1/files`（multipart）直传 16 kHz、单声道、16-bit PCM WAV → 取得 `file_id`。
2. `GET /api/v1/files/{file_id}` → 响应 `data.url` 为上游签发的签名临时 URL（含 STS `security-token`、`Expires`、`Signature`，约 938 字符，`http` 方案）。
3. 以该 URL 作为 `input.file_url` 调用 `POST /api/v1/services/audio/asr/transcription` → 终态 `SUCCEEDED`。

**必须避开的被证伪路径**：`GET /api/v1/uploads?action=getPolicy` 加对象存储表单直传（对象被策略锁定为 `private`，转写服务读不到：`oss://` 得 `FILE_DOWNLOAD_FAILED`、HTTPS 直链得 `FILE_403_FORBIDDEN`）；跳过第 2 步直接用 `file_id` 提交（得 `FILE_DOWNLOAD_FAILED`）。

**由此产生的实现约束**：`TemporaryAudioReference::validate()` 现只接受 `https`，须放宽为“由上游签发且含签名与有效期参数”，并新增“用户可输入地址被拒绝”的负向测试。

### 6.3 附带确认的两项事实

- **结果结构兼容**：实际返回 `transcripts[].sentences[]`，与客户端既有 `parse_dashscope_segments` 的读取路径一致，解析逻辑无需改写。
- **无词级时间戳**：当前模型只到句级 `begin_time`/`end_time`，直接印证 `add-effect-caption-renderer` 已冻结的要求——逐字动画只能按字素分配视觉节奏，不得称为真实逐字语音对齐。

### 6.4 探针计费

文生图 1 张、图生视频 1 条 5 秒、转写 2 次各 4 秒、LLM 15 token。均为最小粒度，无重复提交。
