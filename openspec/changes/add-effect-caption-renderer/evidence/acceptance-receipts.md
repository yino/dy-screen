# 任务组 12 验收回执

本文件记录 `add-effect-caption-renderer` 步骤 3 任务组 12（12.1–12.8）的验收执行情况：
哪些在本机真实执行、命令与结果是什么；哪些因硬件或随包资产缺失而**结构性不可达**，
以及不可达的具体证据。

采集时间：2026-10-03（Asia/Shanghai）

## 验收环境（实测）

| 事实 | 实测输出 |
| --- | --- |
| 主机 | `Darwin arm64`，macOS 26.6.2（Build 25G83） |
| 可运行平台 | **仅 macOS arm64**（无 macOS Intel 真机、无 Windows 真机） |
| Rust 已安装目标 | `aarch64-apple-darwin`、`x86_64-apple-darwin`、`x86_64-pc-windows-msvc`（仅交叉检查，不等于可运行验收） |
| `resources/` 内容 | `asr`、`asr-build`、`asr-source`、`asr-source-macos-aarch64`、`asr-source-macos-x86_64`、`asr-stage`、`asr-stage-x86_64` |
| 随包渲染运行时 | **不存在**（`resources/render` 不存在） |
| 固定版本渲染浏览器 | **不存在**（`resources/playwright`、`resources/*chrome*` 均不存在） |
| 随包锁定字体 | **不存在**（仓库内无 `.ttf`/`.otf` 资产） |

上述三条缺失是把 12.1–12.4 判定为不可达的直接原因：它们要求"增强方式选择 → 组合生成 →
白名单校验 → 运行时完整性校验 → 受限渲染 → 透明层合成 → 基础字幕抑制 → 导出"整链在真机上跑通。

## 真实执行的验收（12.5–12.8）

验收测试文件：`src-tauri/tests/effect_caption_acceptance.rs`

```sh
cd src-tauri && cargo test --test effect_caption_acceptance
```

结果：

```
running 4 tests
test task_12_5_all_nine_boundary_samples_are_rejected_without_a_single_render ... ok
test task_12_6_tampered_tree_is_rejected_and_engine_version_invalidates_cache ... ok
test task_12_7_repeat_export_reuses_cache_and_text_change_rerenders ... ok
test task_12_8_diagnostics_never_leak_body_prompt_asr_paths_or_tokens ... ok

test result: ok. 4 passed; 0 failed
```

### 12.5 越界验收

- 九类样本（脚本、事件处理器、外部引用、脚本引用、未支持 CSS、超限、文本引用越界、
  全屏不透明背景、非确定性时序）逐条经真实 `validate_effect_caption` 校验；
- 断言：全部被拒绝、**每一类的错误码与任务 3.8 一致**、`applied = 0`、
  **"启动渲染进程"计数为 0**、渲染缓存条目数为 0、失败记录不含抑制区间（基础字幕保留）。
- "未启动渲染"的**类型级**保证：渲染入口要求 `ValidatedEffectCaption`，该校验失败即无法构造。

### 12.6 完整性验收

- 真实目录树 + 清单：未篡改时 `verify_renderer_runtime` 通过；
  篡改 `dist/engine.js` 一个字节后拒绝，错误码 `renderer_tree_hash_mismatch`；
- 引擎脚本版本从 `38592478aad3f515` 变为 `deadbeefdeadbeef` 后缓存键不同，
  旧 `ready` 帧不得复用（`find_ready_render_attempt` 返回空）。

### 12.7 缓存验收

- 同一组合版本重复导出：第二次 `Reused`，渲染器调用次数保持 1（未重新渲染）；
- 修改文案导致组合版本/指纹变化：新建尝试行、渲染器调用次数变为 2、记录的组合版本更新为
  `composition-v2`，缓存条目数为 2（旧条目保留供诊断）。

### 12.8 诊断脱敏验收

- 汇总本流程真实产生的诊断字符串（九类拒绝的 code/detail、运行时篡改的 code/detail、
  阶段计划与预览规格的结构化 dump），断言其中不含：
  组合正文（`这个价格真的绷不住了`）、`sk-`、`ghp_`、`/Users/`、`\Users\`、`/opt/yino`、
  `com.aliyuncs.com`、`composition_prompt`；
- 已发布句段转换（`published_ranges_from_subtitles`）只携带句段 ID 与时间区间，不携带正文。

## 不可达的验收（12.1–12.4）与逐条证据

| 任务 | 要求 | 判定 | 证据 |
| --- | --- | --- | --- |
| 12.1 | macOS arm64 端到端验收 | **部分不可达** | 整链中「组合生成」需要步骤 2 的模型调用绑定（任务 2.4，`llm.rs` 属步骤 2 写作用域）与可用凭据；「受限渲染」需要随包渲染运行时 + 固定版本浏览器。两者在本机均不存在（见环境表）。其余环节已分别有真实测试：白名单校验（组 3）、运行时完整性（组 4，含真实子进程）、透明层合成与导出（组 7，真实 FFmpeg 集成测试）、基础字幕抑制（组 8）。 |
| 12.2 | macOS Intel x86_64 同项验收 | **不可达** | 本机为 `Darwin arm64`，无 Intel 真机可运行。已补做交叉目标**编译检查**（不等于运行验收），结果见下方"交叉检查"节。 |
| 12.3 | Windows x64 同项验收 | **不可达** | 无 Windows 真机。交叉目标 `x86_64-pc-windows-msvc` 仅为目标已安装，不能运行应用与渲染器。 |
| 12.4 | 三平台各导出两次、逐帧一致且跨设备字形布局一致 | **不可达** | 依赖 12.1–12.3 在三台设备上完成；单平台确定性（帧序列摘要比较、允许 ≤0.9% 光栅差异）已在组 5 实现并测试。 |

## 交叉检查（补充证据，不构成验收）

尝试对 Intel 目标做可编译性检查：

```sh
cargo check --target x86_64-apple-darwin --all-targets          # 根工作区
cd src-tauri && cargo check --target x86_64-apple-darwin --all-targets
```

两次（根工作区与 src-tauri）都以 exit 101 结束，失败原因与代码无关：

```
Caused by:
  unable to get packages from source
Caused by:
  failed to unpack package `cpufeatures v0.3.0`
Caused by:
  failed to create directory `/Users/yino/.cargo/registry/src/index.crates.io-.../cpufeatures-0.3.0`
Caused by:
  Operation not permitted (os error 1)
```

即：为**新目标**展开依赖需要写 `~/.cargo/registry`，而当前沙箱策略不允许写该目录
（工作区之外）。改用工作区外的 `CARGO_HOME` 重试时，本机没有 `timeout` 命令（macOS 默认不提供），
该尝试未执行。

结论：Intel 目标的**可编译性检查在本环境也未完成**；即使完成，它也只证明可编译，
不能替代"在 Intel 真机上跑通整链"的验收要求（12.2）。

## 结论

- 12.5–12.8：本机真实执行通过，已勾选。
- 12.1–12.4：因随包渲染器/固定版本浏览器/锁定字体资产与 Intel/Windows 真机缺失而不可达，
  保持未勾选，并按上表逐条给出证据。
