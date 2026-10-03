# v2 实施交接状态（会话中断点）

> 快照时间：2026-09-30。基线 commit：`e3cf574`。**本文件描述的是"当前工作区已验证的真实状态"**，不是计划。
> 唯一实施顺序来源是 [`v2-delivery-order.md`](v2-delivery-order.md)；本文件只记录进度、已修缺陷与下一步入口。

## 1. 已验证状态

```
  cargo test --workspace --all-targets   exit=0
      33 个测试套件通过 / 0 失败
      tauri crate 115 passed / 0 failed
  cargo check（tauri crate）              0 警告 0 错误
  openspec validate --strict              7 个 change 全部通过
  git                                     41 个已跟踪文件修改 + 10 个新增，【未提交】
```

| 步骤 | change | 进度 |
| --- | --- | --- |
| 1 | `wire-cloud-asr-upload-and-jobs` | **47/55**（余 8 项 = 三平台真机验收） |
| 2 | `add-model-gateway-and-harness-sidecar` | 0/59（核心已落地，无可诚实勾选项） |
| 3 | `add-effect-caption-renderer` | 65/77（**可实现部分已全部完成**；剩 12 项结构性阻塞，见 [completion-audit.md](../openspec/changes/add-effect-caption-renderer/evidence/completion-audit.md)） |
| 4 | `add-generative-video-animation` | 0/60（探针已确认万相可用） |

## 2. 已完成且经测试验证的实现（步骤 1）

- **三步直传发布器**：`POST /api/v1/files` → `GET /api/v1/files/{file_id}` → 提交转写。`getPolicy` 表单路径已证伪并弃用。
- **引用校验放宽**：接受"上游签发 + 含签名与有效期"的 `http` 临时 URL；用户可输入地址一律拒绝。
- **引擎身份真实化**：云端引擎返回 `dashscope-filetrans` 自身身份；识别配置身份校验改为比对**实际执行引擎**（移到 `transcribe` 之后）。
- **不做隐式回退**：`ProjectSelectableAsrEngine` 按工程冻结的 `engine_id` 选路；默认走云端。
- **VAD 门禁移除**：VAD 只在本地路径执行；云端路径处理完整音频。
- **分块与合并**：`plan_audio_chunks`（有重叠、避免退化尾块）、`restore_source_time`、`merge_chunk_segments`、`resolve_time_overlaps`、`stable_segment_id`。
- **六分支失败分类**：`CloudFailureBranch`（取消/授权/未开通/额度/瞬时/资源失效）。
- **取消语义**：`cancel_task` + `cancel_outcome`，无论上游是否接受都告知"仍可能按实际用量产生费用"。
- **清理失败不回滚成功结果**：`apply_cleanup_outcome` 保留成功结果并标记清理待重试。
- **迁移版本 26**：`asr_artifacts` 增列 + `ai_remote_job_records` 幂等表；`RecognitionProfile` 增 `provider`/`chunk_strategy`（`skip_serializing_if` 保证旧配置指纹不变）。
- **重启恢复**：`RecoverySummary.awaiting_remote_reconcile` + `pending_remote_jobs()`；恢复只统计待对账，不自动重提。
- **路径可用性门禁**：`ensure_path_available` 在 FFmpeg/VAD/ASR 之前拦截。

## 3. 已完成且经测试验证的实现（步骤 2，任务未勾选）

- **统一调用点**：`ModelCallPath` / `ResolutionSource` / `ModelCallSnapshot` / `resolve_model_call`（「构建期默认 → 本机覆盖 → 单次运行覆盖」；**Base URL 永不可被覆盖**）。
- **运行内冻结**：`FrozenModelCall::resolve` / `fingerprint_input` / `model_id`；`extract_with_frozen` 使用冻结值取地址、模型与超时。
- **运行指纹与实际模型**：`analysis_fingerprint` 纳入 `call_fingerprint`；精彩运行记录改写为**实际使用的**模型。
- **纠错链路转换**：`SubtitleCorrectionRequest` 携带 `frozen`；`correct_subtitles` 移除已被架空的 `settings` 参数。
- **授权/额度门禁**：`ClipCorrectionGate` / `evaluate_correction_gate` / `map_upstream_business_code`；`ActivationCorrectionGate` 已注入 `app.rs`。
- **文档口径修正**：清除全部"额度按 heartbeat 判定"表述（实测心跳响应无额度字段），改为"未开通与额度耗尽由上游错误码 3001/3003/3004 判定"。

## 4. 累计修复的真实缺陷（均由测试/编译器/实测逼出）

| # | 缺陷 | 若未修复的后果 |
| --- | --- | --- |
| 1 | 引擎选择方向反了（`None` 落到本地） | 默认路径静默走本地识别 |
| 2 | 分块退化尾块（仅重叠区） | 每次多一次付费调用 |
| 3 | 转写结果 URL 只收 https（实测为 http 签名） | 真实链路永远拿不到句段 |
| 4 | 默认识别配置携带本地身份 | 云端结果冒用本地身份，缓存互相污染 |
| 5 | 清理失败回滚成功结果 | 付费结果被丢弃，可能重复付费 |
| 6 | 边界残句时间交叠未被去重 | **真实长音频转写发布失败**（`publish_artifact` 拒绝重叠） |
| 7 | 空白覆盖静默回退默认模型 | 用户配置的模型被忽略且无报错 |

## 5. 已产出的实测证据

| 文档 | 内容 |
| --- | --- |
| [review/README.md](review/README.md) | 探针实测属性与生成规格特征（纯文本，媒体不入库） |
| [review/asr-chunking-acceptance.md](review/asr-chunking-acceptance.md) | 真实中文 4 分钟样本分块验收（含修正前后与 `publish_artifact` 交叉验证） |
| [validate-dashscope-direct-capabilities/](../openspec/changes/validate-dashscope-direct-capabilities/) | 四类能力矩阵 + 三步直传 + 本地帧可达性（34/34） |
| [renderer sandbox-feasibility.md](../openspec/changes/add-effect-caption-renderer/sandbox-feasibility.md) | 渲染后两轮实测（原生路径 4.81 与字幕带路径 0.847） |

## 6. 下一步入口（按顺序）

1. **步骤 1 第 11 组（8 项）**：需要 macOS Intel x86_64 与 Windows x64 真机，以及运行完整 Tauri 应用做故障注入。**本环境无法完成**。
2. **步骤 2 任务组 4 收尾**：
   - 把纠错链路的凭据来源从 `CredentialStore`（用户自备 Key）换成 `ai/llm-ak` 的**短期 token**；
   - 把 `map_upstream_business_code` 接进 provider 的错误返回路径（当前只有纯函数与测试）；
   - 补 4.2/4.3/4.5 的等价性断言与 WebView 隔离审计。
3. **步骤 2 任务组 5/6**：照任务组 4 的模式转换转场匹配与精彩分析（请求携带 `FrozenModelCall` → 移除被架空的 `settings` → 加等价性断言）。
4. **步骤 3 开工前置**：先扩展渲染器集成面为"带捕获 + 外部合成"，并落实 `add-effect-caption-renderer/design.md` 的 6 条约束。
5. **步骤 4**：探针已确认万相可用；按 `add-generative-video-animation` 任务组 1→2→3 推进。

## 7. 未决问题（需产品/负责人裁定）

1. **验收口径**：目标要求三平台验收，本环境只有 macOS ARM64。是否改为"单平台端到端 + 另两平台待真机"？
2. **6.5 文本容差**：真实样本上分块合并与不分块参考的完全一致率为 42.3%（相似度 ≥0.8 为 69.2%）。是否可接受？若要提高，需要边界留上下文或残句二次识别（新增实现与成本）。
3. **24 秒覆盖复测**：6.5 的可比对窗口为 full[0,180s)，另有约 60 秒未比对；全覆盖需追加付费调用。
