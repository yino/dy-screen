# 生成式动画链路 · 交接说明（Round 26 快照）

> 这份文档只回答一个问题：**"现在什么通了、什么还没接、卡在哪。"**
>
> 它刻意按**接线状态**而不是按任务清单组织——因为在整个实施过程中最难发现的缺陷，
> 全都是同一类：**零件写好了、测试全绿、但那根线没接**（详见第五节）。

---

## 一、验收标准

| 层级 | 验收 |
|---|---|
| **主验收** | 用 `/Users/yino/Downloads/20261008-004320.mkv` 跑通智能成片全流程并导出 MP4 成品 | ✅ **已达成** |
| 阶段 2 | 行业自动识别 + 内置 prompt 的精彩提取 | ✅ |
| 阶段 3 | 片段级前置的流程重构 | ✅ |
| 阶段 4 | AI 转场生成替换素材库匹配 | 🟡 素材库匹配 ✅；**AI 生成侧未真实调用万相**（需付费授权） |

**主验收已由无界面驱动真实跑通并导出 MP4**（见第六节 `make smoke-smart-clip`）。
此前"需要人在 GUI 上执行"的判断是**错的**：核心逻辑全在 Rust 库里，可脚本化驱动。

**剩余唯一阻塞**：AI 生成转场需要真实付费调用万相（Wanx），需人工授权。

---

## 二、已闭环

| 项 | 证据 |
|---|---|
| 云 ASR 解析缺陷 | `output.result.transcription_url`（此前误读 `output.results[]`）× |
| 模型误发 | `llm_provider_settings.model_id=deepseek-chat` → 加 qwen 白名单纠正 |
| 失败原因被掩盖 | `smart_workflow` 聚合真实错误，不再硬编码"本地 ASR 失败" |
| 行业自动识别 | 7 个行业 + 9 个内置 Skill，`select_skills` 全接通 |
| `replace` 合成 | 目标范围计算 → 导出叠加 → 字幕保留 → 原声保留 |
| `insert` 合成 | 独立单元 → 时长顺延 → 48kHz 静音 → 边界淡化 → 字幕偏移 |
| 时长适配 | `Trim` / `SlowDown` / `Rejected` |
| 增强方式互斥 | 动画高光不叠特效字幕；写入前拒绝第二条 `applied` |
| 审阅栏「增强结果」 | 后端查询 → Tauri 命令 → 前端渲染 → 组件测试 |
| 时间轴动画单元 | 渲染 + 属性面板（生成来源/校验状态/移除） |
| 邻接变化失效 | `mark_clip_project_edited` 收口，重排/移除/追加自动失效 |
| **片段级前置** | 阶段顺序改为 `…Highlight → Correction → Draft…`；v32 新增句段级纠正层，合辑字幕创建时采用纠正文本；转写纠正流程带批次重试 |
| 长视频多批 | 批次重试 2→3 次 + 取消感知退避（实测 6 批全绿） |
| 取消收尾 | 不再残留 `running` 僵尸运行 |
| 读层容错 | 素材未下载/失效不再导致工程打不开 |

---

## 三、零件接线状态

调用点统计口径：**排除定义行、测试行、导出列表**后的生产代码调用数。

| 零件 | 位置 | 生产调用 | 状态 |
|---|---|---|---|
| `shift_subtitles_for_inserts` | clip_subtitle.rs | `app.rs` 导出编排 | ✅ 已接 |
| `enhancement_review_items` | domain.rs | `enhancement_review_for_project` | ✅ 已接 |
| `candidates_eligible_for_effect_caption` | domain.rs | `run_enhancement_for_batch` | ✅ 已接 |
| `insert_binding_is_valid` | domain.rs | `invalidate_stale_insert_bindings` | ✅ 已接（Round 24 消除重复实现） |
| `resolve_replace_target_range` | domain.rs | 仅集成骨架 | ⏳ 待生成链路 |
| `adapt_generated_duration` | domain.rs | 仅集成骨架 | ⏳ 待生成链路回填真实时长 |
| `generated_animation_reuse_key` | domain.rs | 仅集成骨架 | ⏳ 待付费提交 |
| `derive_effect_window_from_transcript` | clip_subtitle.rs | — | ⏳ 待阶段 3 |

集成骨架：`src-tauri/tests/generated_animation_pipeline.rs`（固定样本，零付费调用）。

---

## 四、待接线（按阻塞原因分组）

### 需要付费授权（直连万相）

| 缺什么 | 影响 |
|---|---|
| `animation-storyboard` Skill（分镜生成） | 无分镜 → 无提示词 → 无法提交 |
| 直连万相提交 + 轮询（短期 token） | 无资产 → `replace`/`insert` 无源 |
| 下载 + 哈希/尺寸/帧率校验 | 资产无法进入 `validated` |
| 时长回填 | `adapt_generated_duration` 拿不到真实生成时长 |
| heartbeat 开通/额度结论 | 8.6 的"可区分原因"无法实现 |

### 需要真实样本

| 缺什么 | 影响 |
|---|---|
| 真实降级率统计（6.4） | 容差与目标时长档位无数据支撑 |

### 需要数据模型决策

| 缺什么 | 影响 |
|---|---|
| "纠错结果写到哪"（阶段 3） | 校正文案无法前移到合辑之前 |

### 需要真机

| 缺什么 | 影响 |
|---|---|
| macOS arm64 / Intel / Windows x64 | 11.1–11.4 三平台验收 |

---

## 五、已修缺陷清单（含发现轮次）

这一节是本文档最有价值的部分：**全部缺陷在修复前的状态下，单元测试都是全绿的。**

### 5.1 只有真实运行才会暴露的缺陷（LLM / ASR 链路）

| # | 缺陷 | 症状 | 性质 |
|---|---|---|---|
| 1 | 云 ASR 解析读 `output.results[]`（复数） | `dashscope_missing_transcripts` | 🔴 真实响应只有 `output.result`（单数） |
| 2 | 默认模型 `deepseek-chat` 发往百炼 | `highlight_provider_failed` | 🔴 上游 **404** `model_not_found` |
| 3 | `rig` 的 `deepseek::Usage` 要求 `prompt_cache_hit_tokens` | **HTTP 200 却解析失败** | 🔴 百炼不返回该字段 |
| 4 | `json_schema` 不被兼容模式支持 | **400** `Required body invalid` | 🔴 已加纯文本 + 本地 JSON 降级 |
| 5 | 模型漏填 `totalScore`（返回 0） | 候选**全部落选**且无错误码 | 🔴 已用分项平均兜底 |
| 6 | 默认超时 30 s 对长提示词太短 | 反复 `Temporary` → 部分批次失败 | 🔴 提到 90 s + v31 迁移修正已持久化的旧默认 |
| 7 | 候选时长 6.16 s < 校验要求 15 s | 候选被**静默丢弃** | 🔴 已用相邻句段兜底扩展 |

### 5.2 流程 / 状态类缺陷

| # | 缺陷 | 症状 | 性质 |
|---|---|---|---|
| 8 | `clip_export_sources` 从不填 `replace`/`insert_after` | 两种合成方式**永不生效** | 🔴 功能假通 |
| 9 | spec 场景「应用/移除生成资产」前后端**无任何接口** | 用户无法移除已应用的动画 | 🔴 入口缺失 |
| 10 | `invalidate_stale_insert_bindings` 无调用点 | 重排片段后**继续应用错位绑定** | 🔴 语义失效 |
| 11 | 长视频多批：`MAX_CHUNK_ATTEMPTS=2` 且无退避 | 单批连撞两次波动即整批丢失 | 🟡 已提至 3 次 + 退避 |
| 12 | 读工程时一条失效素材引用导致整体失败 | 用户**打不开自己的工程** | 🟡 读层容忍、导出层严格 |

### 5.3 非产品缺陷（如实列出，不计入上一节）

| 类别 | 内容 | 说明 |
|---|---|---|
| 库层一致性 | `execute` 取消时直接 `return`，不推进终态 | 生产路径由命令层 `record_fatal_error` 兜住；库层补齐以防未来调用者踩坑 |
| 我的驱动局限 | headless 驱动不实现 bridge 拼装 | 所以它导出时**静默跳过转场**；生产路径会显式报错 |
| 我的误判 | 曾把"素材下架"当作读层失败根因 | 实测 `asset_key` 有**外键约束**，悬空 key 不可能存在；真因是**下载记录缺失** |

**共同病根**：测试测的是**零件**，不是**整机**。所有测试都用 fake provider，绕过了真实的协议、字段与模型行为。

### 5.4 已验证的健康路径（"未找到缺陷"也是结论）

| 路径 | 结论 |
|---|---|
| 单视频完整流程 | ✅ 8 片段 + 7 转场 + 纠错，143.7 s / 110 MB |
| 15 分钟长视频（6 批） | ✅ 6/6 批完成，143 秒 |
| 多输入 / 连续多工程 | ✅ run 与产物完全隔离 |
| 单片段工程（无转场） | ✅ 优雅降级，不阻塞导出 |
| 取消 | ✅ 不再留僵尸运行 |
| 素材失效 | ✅ 工程仍可打开 |

---

## 六、验证方式

> 注意：本仓库**没有 Cargo workspace**——根 `Cargo.toml` 是 `dy-screen`，
> `src-tauri/Cargo.toml` 是 `dy-screen-app`。两者必须**分别**跑，从根执行
> `cargo test` **不会**覆盖 Tauri 应用（应用侧约占全部测试的 3/4）。
>
> 一键入口：`make test`（= `test-frontend` + `test-core` + `test-app`）。
> CI 侧新增 `.github/workflows/tests.yml`——此前 CI **从不运行通用测试**
> （只有发布验收与 Windows 打包契约，其中只跑 1 个应用侧测试）。

```bash
# ---- dy-screen（根 crate）----
cargo test --offline                      # 35 个测试二进制

# ---- dy-screen-app（Tauri 应用）----
cargo check --offline --manifest-path src-tauri/Cargo.toml
cargo test  --offline --manifest-path src-tauri/Cargo.toml          # 33 个测试二进制
# 或只跑关心的集成测试
cargo test  --offline --manifest-path src-tauri/Cargo.toml --lib
cargo test  --offline --manifest-path src-tauri/Cargo.toml --test ai_repository
cargo test  --offline --manifest-path src-tauri/Cargo.toml --test smart_workflow
cargo test  --offline --manifest-path src-tauri/Cargo.toml --test ai_commands
cargo test  --offline --manifest-path src-tauri/Cargo.toml --test generated_animation_pipeline

# ---- 前端 ----
npx tsc --noEmit -p ui/tsconfig.json     # 构建脚本 npm run build 也含这一步
npx vitest run --config ui/vite.config.ts

# ---- 主验收（需要人）----
make app-dev
#   然后用 /Users/yino/Downloads/20261008-004320.mkv 跑一次
```

**跑完立刻自查**（只读数据库，不启动应用）：

```bash
./scripts/check-smart-clip-run.sh          # 全部工程
./scripts/check-smart-clip-run.sh 43       # 只看某个 AI 工程
```

它按流水线阶段（识别 → 提取精彩 → 剪辑工程 → 导出 → 生成资产）输出状态与
**真实失败原因**，并按错误码给出定位方向。退出码：`0` 最新工程已导出完成、
`1` 存在未完成/失败阶段、`2` 数据库或 sqlite3 不可用。

> 注意它只认**最新一次**工程的结果：历史成功导出不代表这次跑通
> （本机历史数据：6 次成功导出，但最新工程 #14 仍为 `idle`）。

### 升级到本版本时的迁移（已在真实数据库副本上预演）

本版本新增 **v29**（建 `ai_generated_animation_assets` 表）与 **v30**
（给 `ai_highlight_enhancements` 加 `cost_minor` 列）。二者由 `database.migrate()`
在**启动时自动执行**，无需手工步骤。

**预演结论**（在真实数据库副本上手工执行 DDL 得到）：

| 项 | 结果 |
|---|---|
| v30 `ALTER TABLE ADD COLUMN` | ✅ 成功 |
| v29 `CREATE TABLE IF NOT EXISTS` | ✅ 成功（幂等） |
| 既有数据 | ✅ 完好（38 个工程未受影响） |

即：**重新构建 + 启动不会因迁移失败**。

**当前基线（Round 35 实测）**：`cargo check` 0 warning；
`dy-screen` **202 passed / 0 failed**；
`dy-screen-app` **576 passed / 0 failed / 3 ignored**；
前端 `tsc` 通过 + **143 passed**。
合计 **778 个后端测试 + 143 个前端测试全绿**。

---

## 七、外部依赖风险：模型下线（2026-10-10）

这条不是代码问题，但会直接让"提取精彩/文本纠错"全量失败，因此单列。

**事实**（阿里云官方公告 + 开发者实测）：

| 事实 | 来源 |
|---|---|
| 北京时间 **2026-10-10 00:00** 起，百炼下线 179 个模型条目 | [模型下线机制说明](https://help.aliyun.com/zh/model-studio/model-depreciation)、[公告 118177](https://cn.aliyun.com/notice/118177) |
| 名单覆盖 qwen3-max 全系、qwen-image 系、**qwen-plus 的 4 个快照**、以及 **DeepSeek / GLM / Kimi** 第三方条目 | [开发者实测](https://developer.aliyun.com/article/1765739) |
| 已下线模型的调用报错是 **403 access_denied**，且文案不含"已下线" | 同上 |

**对本工程的影响与已做的处理**：

| 影响 | 处理 |
|---|---|
| `qwen-plus` **主线**不在下线名单（只有带日期的快照） | 默认模型保持 `qwen-plus`，无需改动 |
| 403 曾被归类为"凭据不可用"，会把"模型已下线"误报成 API Key 问题 | 已拆出 `LlmError::AccessDenied`，文案写明"凭据无权限，或所选模型已下线" |
| 400/404/422 曾被归类为可重试的 Provider 失败 | 已拆出 `LlmError::RequestRejected`（不可重试） |
| 残留的 `deepseek-chat` 会被静默纠正为默认模型 | 已用 `ResolutionSource::CorrectedUnsupportedModel` 让用户在设置页看得见 |

**运维建议**：若在 10-10 之后出现 `403`，先怀疑模型下线，而不是 API Key。

---

## 八、需要决策的点

1. **付费授权**：是否允许直连万相的真实调用（打通第四节第一组）。
2. **阶段 3 数据模型**：纠错结果写到"转写句段"（改 ASR 模型）、"候选级字幕"（新增中间层），
   还是保持现状（纠错合辑字幕，该步不前移）？
3. **"应用"语义**：绑定在生成时已确定，`validated` 即生效——是否需要
   "已生成但用户尚未采纳"的中间态？
