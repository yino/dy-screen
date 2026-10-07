# Harness 运行时接入 Runtime Resource Pack

本文件说明如何把 Harness 运行时（Node 22 LTS）与随包资产接入现有 Runtime Resource Pack
（决策十八）。代码侧的组件类型、capability 分离与缺失降级已在 `src/runtime_resources.rs`
就绪（`harness.runtime` / `harness.profile` / `harness.skill` 登记进 `ENHANCEMENT_COMPONENTS`，
任务 1.2 已测试「缺失增强组件不误伤基础能力」）。

## 资产分层（决策十八）

| 资产 | 分发方式 | 落地位置 |
| --- | --- | --- |
| Harness 程序（`harness/main.mjs`） | **随包**（应用源码） | `harness/`（随应用安装包） |
| Profile（`profile.json`）+ Skill（`*.skill.json`） | **随包**（应用资源） | `resources/harness-source/` |
| Node 22 LTS 运行时 | **按平台下载 + 本地缓存** | 通过 Runtime Resource Pack |

> `harness.main.mjs` 是源码（不是二进制），随应用打包；只有 Node 二进制需要走
> Runtime Resource Pack 的按平台下载。

## 接入步骤

1. **锁定 Node**：`resources/harness-source/runtime/node-lock.json` 锁 v22.22.2 +
   三平台官方 SHA256（取自 nodejs.org `SHASUMS256.txt`）。
2. **解压运行时**：`scripts/build-harness-runtime.sh/.ps1` 下载 → SHA256 校验 → 解压到
   `resources/harness-source/runtime/<platform>/`。
3. **生成清单片段**：`scripts/build-harness-manifest-fragment.py <platform>` 输出该平台的
   `harness.runtime` / `harness.profile` / `harness.skill` 三个 `RuntimeComponent` 条目 +
   `Node-MIT` 许可证条目（含每个文件的 `path` / `sizeBytes` / `sha256` / `executable`）。
4. **并入每平台 runtime-manifest.json**：把片段并入 `components` 与 `licenses`；
   `archive.size_bytes` / `archive.sha256` 按含 Node 二进制的新 bundle 重新计算；
   `signature` 由发布流程（`make runtime-manifest-payload` + 签名）施加。

## 断网降级

`harness.*` 三个组件的 `capability` 均为 `enhancement`、`required` 均为 `false`：
Node 未下载 / 清单缺失时，`capability_snapshot` 只把**增强能力**（特效字幕渲染、
Harness 编排）置为不可用，基础能力（录制、浏览、预览、本地 ASR）不受影响。
首次联网时下载器按当前平台缓存 Node，之后断网仍可用。

## 三平台验收（留 CI）

本机只完整端到端了 `darwin-arm64`（下载 → 校验 → 解压 → `node --version` = v22.22.2 →
跑通 `effect-caption-compose` 工具循环）。`darwin-x64` 与 `win-x64` 的 tarball 下载 +
SHA256 校验由 `scripts/build-harness-runtime.{sh,ps1}` 完成，留 CI
`macos-15-intel` / `windows-2022` 执行（对应步骤 2 任务 11.1–11.3、11.10、11.11）。
