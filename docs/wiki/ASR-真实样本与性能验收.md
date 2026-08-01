# ASR 真实样本与性能验收

本文说明如何为 OpenSpec 变更 `add-user-triggered-ai-asr` 的 9.8、9.9 生成可审计证据。脚本和报告模板不能替代真实目标设备、正式发行包或有授权的直播样本；只有执行完成并复核证据后，才能勾选对应任务。

## 1. 隐私与授权边界

- 只使用已取得录制、处理和内部质量评估权限的直播视频。
- 每个样本必须填写 `authorizationReference`，引用同意书、许可记录或内部工单；不要把授权文件正文、个人联系方式或凭据写入数据集。
- 真实视频、数据集和采集结果默认保存在受控本地目录，不提交到 Git。
- 采集结果不保存视频路径、资源路径、命令行或 stderr 原文，只保存样本 ID、ASR 脱敏结果、文件哈希、大小、状态和耗时。
- `datasetSha256`、输入前后 SHA-256、ASR 可执行文件 SHA-256 和资源 manifest SHA-256 用于证明一次报告引用的是哪组不可变输入。

模板位于 [`docs/templates/asr-quality-dataset.example.json`](../templates/asr-quality-dataset.example.json)。复制到受控目录后，替换示例路径、授权引用、时长、人声区间、术语和时间锚点。`replace-with-*` 占位授权引用会被校验器主动拒绝；正式评测默认拒绝少于 10 个样本。

## 2. 数据集字段

| 字段 | 含义 |
| --- | --- |
| `datasetId` | 不包含个人信息的稳定数据集标识 |
| `id` | 样本稳定 ID，只能使用字母、数字、点、下划线和连字符 |
| `authorizationReference` | 可追溯的授权或许可记录编号 |
| `video` | 本机绝对路径，或相对于数据集 JSON 的路径 |
| `durationMs` | 人工或 FFprobe 复核的源视频时长 |
| `speechRegions` | 人工标注、按时间排序且不重叠的人声区间；区间外用于计算静音幻觉 |
| `terms.products` | 商品名及可接受别名 |
| `terms.amounts` | 金额及中文数字等价写法 |
| `terms.streamers` | 主播名及可接受别名 |
| `anchors` | 术语第一次或代表性出现的起始时间和允许误差 |

参考术语必须来自人工标注。采集器不会自动把参考答案注入热词；如果要评估真实项目热词，应通过重复的 `--hotword` 参数明确提供，并为开启/关闭热词分别使用新的结果目录。

## 3. 质量样本采集

先生成 release CLI，并准备经过 `asr-bundle verify` 的单平台资源：

```bash
cargo build --offline --release --bin dy-screen --bin asr-quality

cargo run --offline --bin asr-quality -- collect \
  --dataset /secure/asr-quality/dataset.json \
  --results /secure/asr-quality/run-2026-01-01 \
  --asr-binary target/release/dy-screen \
  --resource-root /secure/asr-resources \
  --minimum-samples 10
```

每个样本产生两个文件：

- `<id>.json`：去除 `video` 路径后的引擎、模型、语言、时长、句段和文本；
- `<id>.evidence.json`：数据集、输入、ASR 二进制、资源 manifest、原始 stdout/stderr 和脱敏结果 JSON 的哈希与大小，以及退出码、墙钟耗时和源文件未变化结论。评估前会重新核对脱敏结果完整性。

采集目录中的同名文件不会被覆盖。重复验收必须使用新的目录，避免把不同运行混成一份证据。单个 ASR 失败不会阻止后续样本采集；失败会进入最终失败率。

## 4. 质量指标计算

```bash
cargo run --offline --bin asr-quality -- evaluate \
  --dataset /secure/asr-quality/dataset.json \
  --results /secure/asr-quality/run-2026-01-01 \
  --json-report /secure/asr-quality/run-2026-01-01-report.json \
  --markdown-report /secure/asr-quality/run-2026-01-01-report.md \
  --minimum-samples 10
```

报告包含：

- 商品名、金额和主播名召回率；
- 时间锚点匹配数、平均绝对误差、P95 误差和容差内比例；
- 静音区间中的转写句段数、句段幻觉率和持续时间幻觉率；
- 样本失败率；
- 有有效耗时证据样本的平均及最大实时因子（RTF）。

当前 OpenSpec 没有凭空设定产品阈值。报告必须如实保留；若 small 量化模型在商品名、金额或整体可读性上不满足业务需要，应另立“用户明确授权的云端 `AsrEngine` Adapter”变更，不能在本版本自动上传或回退云端。

## 5. macOS arm64 8 GB 性能采集

在实际 8 GB Apple Silicon 设备上执行。Tauri 应用包只安装 `dy-screen-app` 主程序，不包含阶段诊断用的 `dy-screen` CLI；性能采集必须使用与正式发行验收相同、单独构建并签名的 release CLI。`--require-8gb` 只接受 7–9 GiB 的物理内存报告，防止误用开发机数据完成任务：

```bash
./scripts/collect-asr-performance-macos.sh \
  --video /secure/performance/long-authorized-sample.mp4 \
  --asr-binary /secure/release/dy-screen \
  --resource-root /Applications/切片智能体.app/Contents/Resources/resources/asr \
  --output /secure/performance/macos-arm64-8gb.json \
  --require-8gb
```

脚本记录平台、架构、物理内存、输入/二进制/manifest 哈希、墙钟耗时、RTF、进程树峰值常驻内存、最大总线程数、最大进程数、前后热状态、源文件不变以及退出两秒后的遗留子进程数。stdout/stderr 只保留大小和 SHA-256。报告中的 `asrBinarySha256` 必须等于同一候选发行报告中的 `asrCliSha256`，否则最终证据审计会拒绝任务 9.8。

## 6. Windows x64 功能目标机验收

任务 1.6 必须在真实 Windows x64 设备执行，交叉编译不能代替。准备单平台、完整封存的 Windows 资源目录后运行：

```powershell
make asr-test-windows-target `
  ASR_RESOURCE_ROOT="C:\ASR验收\resources" `
  ASR_TARGET_EVIDENCE="C:\ASR验收\windows-target.json"
```

该入口调用 [`scripts/test-asr-windows-target.ps1`](../../scripts/test-asr-windows-target.ps1)，执行以下真实目标行为：

- manifest 必须声明 Windows x64 CPU、SSE4.2 最低基线和 VC++ x64 运行库；
- Windows Adapter 测试覆盖参数数组、Unicode/空格路径、运行中取消、进程退出和缺失运行库错误映射；
- 锁定 CPU Whisper、FFmpeg、VAD 和模型执行真实中文 fixture；
- fixture 被复制到 `Windows 中文路径/测试 视频.mp4` 后通过 CMD 返回结构化中文 ASR；
- 复核原始 fixture 的大小和 SHA-256 未变化；
- 报告只保存资源/输入/日志哈希、退出码和布尔结论，不保存本地路径或日志原文。

只有报告中的 `allPassed=true` 且使用正式候选资源时，才能作为任务 1.6 的证据；它不替代 Authenticode、SmartScreen 或安装/卸载验收。

## 7. Windows x64 8 GB 性能采集

必须在安装后的 Windows x64 目标机执行。Tauri 安装器只安装 `dy-screen-app.exe` 主程序；性能采集使用与正式发行验收相同、单独构建并签名的 release `dy-screen.exe` CLI，并读取安装目录中的正式资源：

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass `
  -File scripts/collect-asr-performance-windows.ps1 `
  -Video "C:\ASR验收\long-authorized-sample.mp4" `
  -AsrBinary "C:\ASR验收\release\dy-screen.exe" `
  -ResourceRoot "C:\Program Files\切片智能体\resources\asr" `
  -Output "C:\ASR验收\windows-x64-8gb.json" `
  -Require8GB
```

Windows 脚本使用进程树采样记录峰值 Working Set、最大线程/进程数，并在退出两秒后通过 PID 和启动时间复核子进程释放。ACPI 无法提供温度时 `thermalStateBefore/After` 为 `unknown`，不得伪造温度。报告中的 `asrBinarySha256` 必须等于同一候选发行报告中的 `asrCliSha256`。

## 8. 完成判定

### 任务 9.8

只有以下证据同时存在时才完成：

- macOS arm64 真实 8 GB 设备报告，严格门禁通过；
- Windows x64 真实 8 GB 设备报告，严格门禁通过；
- 两端都记录有效 RTF、峰值内存、最大线程数、稳定性/热状态和零遗留子进程；
- 使用的二进制与资源哈希能够追溯到对应发行包。

### 任务 9.9

只有以下证据同时存在时才完成：

- 至少 10 场不同、具有 `authorizationReference` 的真实中文直播样本；
- 数据集哈希与全部样本证据一致；
- JSON 和 Markdown 报告均生成并人工复核；
- 商品名、金额、主播名、时间误差、静音幻觉率、失败率和 RTF 都有实际数值或有明确的 `N/A` 原因。

### 六项外部门禁的最终审计

收集任务 1.6、9.2、9.4、9.5、9.8 和 9.9 的全部证据后，使用统一审计入口交叉核对平台、架构、8 GB 门禁、不可变输入以及 CLI、资源 manifest、运行库、数据集和质量报告哈希：

```bash
make asr-evidence-audit \
  ASR_COMPLETION_WINDOWS_TARGET=/secure/evidence/windows-target.json \
  ASR_COMPLETION_MACOS_RELEASE=/secure/evidence/macos-release.json \
  ASR_COMPLETION_WINDOWS_RELEASE=/secure/evidence/windows-release.json \
  ASR_COMPLETION_MACOS_PERFORMANCE=/secure/evidence/macos-arm64-8gb.json \
  ASR_COMPLETION_WINDOWS_PERFORMANCE=/secure/evidence/windows-x64-8gb.json \
  ASR_QUALITY_DATASET=/secure/asr-quality/dataset.json \
  ASR_QUALITY_RESULTS=/secure/asr-quality/run-2026-01-01 \
  ASR_QUALITY_JSON_REPORT=/secure/asr-quality/run-2026-01-01-report.json \
  ASR_QUALITY_MARKDOWN_REPORT=/secure/asr-quality/run-2026-01-01-report.md
```

审计器会从数据集和样本证据重新计算质量 JSON/Markdown，不信任单独传入的汇总数字。只有命令成功退出且输出 JSON 的 `readyToComplete=true` 时，六项外部任务才具备完成条件；失败输出只包含检查代码、哈希和脱敏结论，不包含本地路径或报告原文。审计通过也不会自动修改 `tasks.md`，仍需人工复核原始授权和目标机记录后逐项勾选。

模板、单元测试、固定短句 fixture、开发机 36 GB 性能或跨平台编译成功都不能替代上述真实证据。
