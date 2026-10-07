# 步骤 2 完成度审计与收口报告

变更：`add-model-gateway-and-harness-sidecar`
审计时间：2026-10-07（Asia/Shanghai）
任务总数：68 —— **已完成 46 / 未完成 22（全部为随包资产 / 三平台 CI / 真实运行时依赖的结构性阻塞，逐条留证）**

## 一、切片交付结果

| 切片 | 范围 | 结果 |
| --- | --- | --- |
| **S1 统一调用点** | 短期 token 复用、运行级冻结、心跳解析、非生产 feature、三条链路迁移、设置页、数据边界、调用点诊断 | ✅ 全部交付 |
| **S2 sidecar 与 Skill** | sidecar 生命周期与 stdio JSON-RPC、工具授权（只信 Rust）、随包 Skill 加载器、ASR 注入防护 | ✅ 资产无关部分交付；随包资产本体阻塞 |
| **S3 媒体生成工具** | 结构化契约 + 本地门禁 + 桩实现 + 额度冻结 | ✅ 资产无关部分交付；真实调用留步骤 4 |

## 二、未完成的 22 项与阻塞分类

### A. 随包资产缺失（7 项）

`resources/` 实测只有 ASR 相关目录（见步骤 3 验收回执）；Harness 运行时（Node 22 LTS 三架构
tarball）、随包 Profile 与 Skill 文件、以及据此产生的三架构哈希/体积都不存在：

| 任务 | 缺什么 |
| --- | --- |
| 1.3 | 新组件纳入发布校验必需列表 + macOS arm64 / Intel / Windows x64 三架构产物哈希与架构校验 |
| 1.4 | staging 与完整性校验覆盖新组件的哈希/架构/相对路径与符号链接拒绝 |
| 2.6 | sidecar 峰值内存/磁盘/启动耗时实测（8 GB 基线） |
| 3.1 | 随包应用专用 Profile 本体（只注册模型路由与 Skill，无 shell/文件/浏览器/网络工具） |
| 3.2 | 分析过程调用通用工具被拒的测试（需真实 Harness 运行 Profile） |
| 3.3 | 用户目录同名配置被忽略的测试（需真实 Profile 加载路径） |
| 3.4 | 五个精彩 Skill + 两个转场阶段纳入随包 Skill（五个 Skill 内容已作为常量存在、加载器就绪；转场两阶段**尚无 Skill 身份**，需先定义） |

### B. 三平台协议与恢复验收 / CI 扩展（11 项）

需要 Node 22 LTS 三架构 Harness 资产 + 扩展 `platform-release-validation.yml` 的
`macos-15-intel` / `windows-2022` job：

| 任务 | 缺什么 |
| --- | --- |
| 11.1 / 11.2 / 11.3 | macOS arm64 / Intel / Windows 三平台的 sidecar 启动、JSON-RPC 往返、取消、崩溃恢复与升级验收 |
| 11.4 | 三条链路端到端验收（需激活账号 + 真实短期 token 环境） |
| 11.5 | 90 秒切换窗口、运行内冻结、指纹不一致不复用的端到端验收 |
| 11.6 | 并发配置隔离验收 |
| 11.7 | 未开通 / token 过期 / 额度耗尽三态的可区分原因端到端验收 |
| 11.8 | 连续任务后的资源回收与 8 GB 基线 |
| 11.9 | 诊断输出不含 token/提示词/ASR 正文/本地路径/签名地址（**后端已由 4.6 测试覆盖**，缺三平台归档） |
| 11.10 | 把 11.1–11.3 扩展进 CI job（`macos-15-intel` / `windows-2022`） |
| 11.11 | CI 内构建并校验锁定发行输入（Harness 运行时/Profile/Skill） |

### C. 取消 / 资源限制 / 迟到结果的编排接线（4 项）

部分依赖真实运行时的资源特征与跨 sidecar/渲染的编排接线：

| 任务 | 缺什么 |
| --- | --- |
| 8.1 | 取消时终止 sidecar 调用与渲染子进程、清理未发布临时文件、有界时间、保留已发布阶段结果 |
| 8.2 | 内存与执行时间上限实测（`ProcessLimits` 已存在，内存探测需真实运行时） |
| 8.3 | 取消后迟到结果被忽略的测试（需编排接线 + 真实取消路径） |
| 8.4 | sidecar 与渲染运行时独立子进程、分别限制资源（结构性已满足：二者独立 `spawn`；缺显式测试） |

## 三、假勾审计（机械核对）

方法：从 `tasks.md` 全部 46 条已勾选任务中提取反引号内的代码符号，逐个在
`src`、`src-tauri/src`、`src-tauri/tests`、`ui/src`、`scripts` 中查找定义。

审计 128 个符号，其中 3 个"未找到"均为**可解释的假阳性**：
- `MAX_AGENT_TURNS`（×2）：任务 3.7 明确"删除"该符号——它的缺位正是交付内容；
- `taskId`：serde `rename_all = "camelCase"` 产生的 JSON 字段名（`task_id`），不是代码符号。

结论：**0 个虚假引用**。

## 四、验证基线（最近一次全量）

| 门禁 | 结果 |
| --- | --- |
| `cd src-tauri && cargo test --all-targets --no-fail-fast` | 523 passed / 0 failed |
| `cargo test --workspace --all-targets`（根） | 199 passed / 0 failed |
| `cargo test --features non-prod-model-config --lib` | 271 passed |
| `pnpm vitest run`（ui） | 137 passed |
| `cargo clippy --all-targets -- -D warnings`（两侧 × 两 feature） | 干净 |
| `cargo fmt --all -- --check` | 干净 |
| `make spec-validate` | 32/32 strict |

## 五、结论

- 步骤 2 中**所有不依赖随包资产 / 三平台 / 真实运行时的可实现任务已完成并通过门禁**（46/68）。
- 剩余 22 项全部为结构性阻塞：随包 Harness/Profile/Skill 资产 7、三平台 CI 验收 11、取消/资源限制编排 4。
  每一项的"缺什么"已在第二节列出，未做任何虚假勾选。
- 资产就位后可直接解锁：1.x（清单已就绪）、3.x（加载器/注入防护已就绪）、11.x（CI 扩展是既有 workflow 的增量）、
  2.6/8.x（监管与门禁已就绪，只差真实运行时实测）。
