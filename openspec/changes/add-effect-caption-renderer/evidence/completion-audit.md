# 步骤 3 完成度审计与收口报告

变更：`add-effect-caption-renderer`（步骤 3）
审计时间：2026-10-03（Asia/Shanghai）
任务总数：77 —— **已完成 65 / 未完成 12（全部为结构性阻塞，逐条留证）**

## 一、完成情况

| 任务组 | 范围 | 状态 |
| --- | --- | --- |
| 1 | 增强方式契约、迁移 27、代次语义 | 全部完成（2.4 除外，属步骤 2 范围） |
| 2 | 组合生成契约与随包样式 | 完成（2.4 未完成：模型调用绑定属步骤 2 的 `llm.rs` 写作用域） |
| 3 | 组合白名单校验与拒绝路径证据 | 全部完成（含 3.8 九类样本、3.9 校验失败不落库） |
| 4 | 受限渲染子进程与运行时完整性 | 全部完成（真实子进程测试） |
| 5 | 字体锁定、确定性与缓存键 | 完成（5.1 随包字体/浏览器资产、5.3 双设备一致性转为阻塞项） |
| 6 | 预览与导出规格 | 完成（7.6 的真实测量转入阻塞项） |
| 7 | RGBA 帧序列合成与导出 | 完成 7.1–7.5（真实 FFmpeg 集成测试）；7.6 阻塞 |
| 8 | 基础字幕抑制 | 全部完成 |
| 9 | 渲染缓存复用与清理 | 全部完成 |
| 10 | 资源清单与发布校验 | 完成 10.1–10.3；10.4–10.7 阻塞 |
| 11 | 编排、降级与阶段语义 | 全部完成（含 11.1 接线） |
| 12 | 验收 | 完成 12.5–12.8；12.1–12.4 阻塞 |

## 二、未完成的 12 项与阻塞分类

### A. 随包资产缺失（6 项）

`resources/` 实测内容只有 ASR 相关目录；`resources/render`、`resources/playwright`、
`resources/*chrome*` 均不存在，仓库内无 `.ttf`/`.otf`。因此以下任务无法真实执行：

| 任务 | 缺什么 |
| --- | --- |
| 5.1 | 随包锁定字体与固定版本渲染浏览器（已在清单与校验层落地，缺资产本体） |
| 7.6 | 全分辨率帧序列的真实峰值磁盘与耗时（需真实渲染器） |
| 10.4 | 在真实资源包上跑发布校验与离线启动验收 |
| 10.6 | 在真实发行产物上扫描开发机路径与未声明可执行文件 |
| 10.7 | 含新组件的安装包体积与首次启动磁盘占用增量 |
| 12.1 | 端到端整链中的「组合生成」与「受限渲染」两环 |

### B. 硬件/平台缺失（5 项）

本机实测为 `Darwin arm64`（macOS 26.6.2），无 macOS Intel 真机、无 Windows 真机：

| 任务 | 缺什么 |
| --- | --- |
| 5.3 | 第二台字库不同的设备（跨设备字形/布局一致性） |
| 10.5 | macOS arm64 / macOS Intel / Windows x64 三平台离线启动验收 |
| 12.2 | macOS Intel x86_64 运行验收 |
| 12.3 | Windows x64 运行验收 |
| 12.4 | 三平台各导出两次的跨设备一致性 |

补充：Intel 目标的**可编译性检查**也在本环境未完成 —— 为新目标展开依赖需要写
`~/.cargo/registry`，当前沙箱策略拒绝该工作区外写入（原始报错见
`evidence/acceptance-receipts.md`）。

### C. 依赖步骤 2（1 项）

| 任务 | 缺什么 |
| --- | --- |
| 2.4 | 客户端直连百炼的短期 token 调用点绑定；`llm.rs` 属步骤 2（`add-model-gateway-and-harness-sidecar`，0/59）写作用域 |

## 三、假勾审计（机械核对）

方法：从 `tasks.md` 全部 65 条已勾选任务中提取反引号内的代码符号，逐个在
`src-tauri/src`、`src-tauri/tests`、`src`、`tests`、`scripts`、`ui/src` 中查找定义。

结果：**审计 114 个符号，全部找到定义，0 个虚假引用。**

```
已勾选任务数：65
审计符号数：114
全部符号均可在代码/脚本中找到定义 ✅
```

## 四、验证基线（本轮最后一次全量执行）

| 门禁 | 结果 |
| --- | --- |
| `cd src-tauri && cargo test --all-targets --no-fail-fast` | 31 suites，**465 passed / 0 failed** |
| `cargo test --workspace --all-targets`（根） | 34 suites，**196 passed / 0 failed** |
| `cargo clippy --all-targets -- -D warnings`（两侧） | 干净 |
| `cargo fmt --all -- --check`（两侧） | 干净 |
| `make spec-validate` | **32 passed / 0 failed**（strict） |

步骤 3 新增测试分布（`#[test]` / `#[tokio::test]` 计数）：

| 模块/文件 | 测试数 |
| --- | --- |
| `ai/effect_caption.rs`（白名单） | 19 |
| `ai/effect_caption_composition.rs` | 9 |
| `ai/effect_caption_render.rs`（受限渲染/完整性/确定性） | 21 |
| `ai/effect_caption_spec.rs`（预览/导出规格） | 7 |
| `ai/effect_caption_cache.rs`（缓存复用） | 5 |
| `ai/effect_caption_orchestration.rs`（编排与逐条降级） | 7 |
| `ai/clip_export.rs`（透明层合成，含真实 FFmpeg 集成） | 12 |
| `ai/clip_subtitle.rs`（基础字幕抑制） | 8 |
| `tests/effect_caption_acceptance.rs`（12.5–12.8 验收） | 4 |
| `tests/clip_ffmpeg_alpha_audit.rs`（FFmpeg alpha 审计） | 4 |

## 五、真实抓到的缺陷（本步骤内被发现并修复）

| 缺陷 | 发现方式 |
| --- | --- |
| 透明层引用错流：加入字幕输入后未递增 `input_index`，特效层引用到字幕流、真正叠加输入被闲置 | 组 7 真实 FFmpeg 集成测试 |
| 增强阶段静默空转：使用更早阶段的批次快照，`highlight_run_id` 仍为 `None` | 组 11 端到端测试 |
| `begin_render_attempt` 在缓存命中时把 `ready` 重置为 `rendering` | 组 9 测试 |
| 越界样本根属性错误码掩盖真实拒绝原因（`data-hyperframes-*` vs 契约属性） | 组 12 验收测试 |
| FFmpeg 审计缺少 alpha 能力探测（原脚本只查 `png` 解码器存在） | 组 10.3（新增真实探测） |

## 六、结论

- 步骤 3 中**所有可在本机实现的任务已完成并通过门禁**（65/77）。
- 剩余 12 项**全部为结构性阻塞**（随包资产 6、硬件/平台 5、步骤 2 依赖 1），
  每一项都给出了具体缺什么、如何验证过缺什么，未做任何虚假勾选。
- 假勾审计（114 个符号机械核对）通过，全量测试与静态门禁全绿。
