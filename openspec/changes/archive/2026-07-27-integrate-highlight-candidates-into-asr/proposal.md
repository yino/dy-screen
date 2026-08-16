## Why

当前精彩候选以纵向完整卡片展示，而 ASR 文本位于独立区域；候选较多或理由较长时，用户需要在两个区域间反复滚动，难以结合原文和视频完成比较与选择。候选已经保存来源视频、稳定句段和时间范围，现在应把这些结构化结果直接映射到 ASR 工作区，形成可定位、可预览且长度有界的选择流程。

## What Changes

- 将精彩区域收缩为运行状态、候选数量和重新分析入口，不再纵向展开全部候选详情。
- 在 ASR 文本区域提供“全文”“精彩候选”“已选择”三种视图，并提供按评分或时间浏览候选的紧凑导航。
- 一次只展开当前候选的总分、理由、标签和可折叠维度评分；使用候选 `segmentIds` 在 ASR 列表中标记真实句段范围。
- 点击候选时自动切换来源视频、跳转对应转写页并定位到开始时间；提供仅播放候选时间范围的片段预览。
- 使用带明确文案的“加入待切片”开关，并在切换时立即持久化选择；失败时恢复原状态并展示可操作错误。
- 处理跨视频候选导航、候选重叠、预览尚未就绪和历史运行恢复等边界。
- 当前变更不生成、裁剪、拼接或导出视频，也不修改精彩 Agent Prompt、评分策略、SQLite 表结构或原始媒体。

## Capabilities

### New Capabilities

- 无。

### Modified Capabilities

- `desktop-client-shell`: 将精彩候选选择与 ASR 文本、视频播放器整合为长度有界的桌面工作区交互。
- `ai-highlight-agent-workflow`: 修改候选展示、定位、范围预览和选择持久化要求，避免展开全部候选和重复转写文本。

## Impact

- 主要影响 `ui/src/AiWorkspace.tsx`、`ui/src/styles.css` 及对应 React 测试。
- 复用现有 `AiHighlightCandidate` 的 `inputId`、`segmentIds`、`startMs`、`endMs`、评分和选择字段，以及现有候选选择 command。
- 不新增前后端 DTO、Tauri command、第三方依赖或 SQLite migration。
- 浏览器演示 API 和测试替身需要继续覆盖候选定位与即时保存行为，但不得发起真实 LLM 请求。
