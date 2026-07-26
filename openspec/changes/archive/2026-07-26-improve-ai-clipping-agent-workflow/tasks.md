## 1. ASR 调度器缺陷复现与核心队列

- [x] 1.1 先增加删除当前项目后其他任务继续执行的失败单测，覆盖活动 job、排队 job、取消确认和执行许可释放
- [x] 1.2 先增加“下一个处理”和“立即切换”的失败单测，覆盖全局顺序、单 executor、不重复 job 和被抢占输入重新排队
- [x] 1.3 将单向 FIFO 重构为命令驱动调度 actor，实现 enqueue、项目取消等待、提升、抢占、快照和 shutdown
- [x] 1.4 为调度 job 和事件增加 generation、稳定入队序列、队列位置及迟到事件隔离
- [x] 1.5 增加录制资源门、取消超时、executor 失败和 shutdown 回归测试
- [x] 1.6 增加录制时 ASR 并行设置，默认处理已完成稳定视频，并覆盖录制优先降级和旧设置补全测试

## 2. SQLite 调度与高光数据迁移

- [x] 2.1 先增加旧数据库升级、重复 migration、外键和现有 ASR 数据保留测试
- [x] 2.2 为 AI 项目和输入增加删除状态、调度 generation、一次性优先级、稳定入队序列及必要索引
- [x] 2.3 新增非敏感 LLM 设置、高光运行、分块、候选、评分、选择和标签快照表
- [x] 2.4 实现 repository 的调度代次、删除协议、启动恢复、队列顺序和旧事件条件更新
- [x] 2.5 实现高光配置、运行阶段、分块、候选原子发布、选择和缓存指纹 repository

## 3. ASR 运行时与 Tauri 队列控制

- [x] 3.1 更新本地 ASR runtime 使用新调度器，并在启动时从数据库重建 pending 队列
- [x] 3.2 实现取消项目并等待、删除超时恢复、设为下一个、立即切换和队列快照服务
- [x] 3.3 扩展 Tauri commands、事件和 DTO，且对项目、输入、状态与确认参数执行后端校验
- [x] 3.4 增加 command/runtime 集成测试，证明删除活动项目不会阻塞其他项目且旧事件不能回写新代次

## 4. DeepSeek 配置与凭据安全

- [x] 4.1 定义 `CredentialStore` 和 `HighlightAgentProvider` 中立契约，并增加内存 fake 的成功、缺失、替换和清除测试
- [x] 4.2 实现系统凭据库 Adapter，SQLite 只保存模型和非敏感参数，查询只返回 `keyConfigured`
- [x] 4.3 实现 Provider 设置保存、读取、清除和最小连接诊断服务及类型化 Tauri commands
- [x] 4.4 增加安全测试，证明 Key、原始响应、转写正文和本地路径不进入数据库、DTO、事件或普通日志

## 5. Rig、DeepSeek Agent 与分析 Skills

- [x] 5.1 锁定并接入 Rig Rust SDK，通过官方 DeepSeek OpenAI 兼容地址构建生产 Provider Adapter
- [x] 5.2 定义强类型候选、评分、token 用量和 Provider 错误，并实现 Rig TypedPrompt/Extractor 解析与取消/超时边界
- [x] 5.3 实现通用、带货、搞笑、知识和故事 Skills 的确定性选择、版本、权重和未知标签隔离
- [x] 5.4 实现候选发现 Agent 与全局评分 Agent，限制最大轮次和只读上下文能力
- [ ] 5.5 使用 fake Provider 增加 Skills、Prompt 注入隔离、结构化输出、伪造句段、越界时间和评分范围测试

## 6. 高光工作流与恢复

- [x] 6.1 先增加长转写分块、重叠、单文件边界、候选去重、默认阈值和 Top 10 的失败单测
- [x] 6.2 实现项目/主播标签快照、发送预估、分析指纹和用户显式授权
- [x] 6.3 实现分块、候选发现、本地校验、去重、全局复评、发布和 token 聚合工作流
- [ ] 6.4 实现工作流取消、两次暂时错误重试、部分失败、断点恢复和完整结果缓存复用
- [ ] 6.5 实现候选查询、播放器定位投影、勾选保存和不触发 FFmpeg 的服务/commands
- [ ] 6.6 增加应用 lifecycle 清理和恢复测试，确保退出不遗留 LLM 任务且不影响录制与 ASR

## 7. AI 剪辑客户端界面

- [x] 7.1 先扩展 TypeScript API/fake 与组件测试，覆盖队列位置、提升、抢占确认、删除中和旧事件隔离
- [x] 7.2 在输入列表增加当前任务、全局队列位置、“下一个处理”和“立即切换”交互
- [x] 7.3 在设置页增加 DeepSeek Key、模型、凭据状态、测试连接、替换和清除交互
- [x] 7.4 在 AI 工作区增加项目标签、分析目标、发送预估、显式授权、Skills 和工作流阶段
- [ ] 7.5 增加高光候选的评分排序、维度详情、理由、标签、播放器跳转、勾选和保存
- [ ] 7.6 完成宽屏/窄窗口、长文本、加载、空状态、错误、键盘与浏览器演示降级测试

## 8. 文档与完整验证

- [x] 8.1 更新 README 的调度控制、DeepSeek 隐私、系统凭据、Agent/Skills、高光候选和非目标说明
- [x] 8.2 更新 Makefile 的调度、Provider fake、Agent 工作流和显式真实 DeepSeek 验收命令
- [x] 8.3 运行 Rust 格式、Clippy、核心/Tauri/React 全量测试和前端构建，修复全部回归
- [x] 8.4 运行 `openspec validate --all --strict`、`git diff --check` 和敏感信息审计
- [ ] 8.5 对照 proposal、design、6 份 delta spec 和全部任务完成最终 `grill-me` 验收审计
