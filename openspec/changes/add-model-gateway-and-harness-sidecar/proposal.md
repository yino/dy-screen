> 命名说明：本目录名 `add-model-gateway-and-harness-sidecar` 是历史标识。按 v2 的“客户端直连百炼”口径，本轮**不再包含服务端模型网关**；模型调用改为客户端持 `ai/llm-ak` 短期 token 直连百炼。目录名保留以避免破坏既有引用，内容以本文为准。

## Why

现有三条 LLM 链路（精彩分析、转场匹配、文本纠错）都由客户端持有用户自备的 DeepSeek API Key 直连上游，硬编码 `DEEPSEEK_BASE_URL`，且设置页要求用户自备 Key。v2 的口径把这个形态改为：客户端用既有的 `GET /api/client/ai/llm-ak` 短期 token 直连阿里云百炼，不再要求用户自备凭据，也不引入服务端模型网关。

同时 v2 的特效字幕与生成式动画需要一个能按职责组织 Skill、并只被授予受控业务工具的进程外运行时；现有实现只有单次结构化输出，没有工具循环，`MAX_AGENT_TURNS` 是未被使用的常量。本 change 建立受控的 DeepSeek Harness sidecar 运行时，并把三条既有链路迁移到“客户端直连百炼”的统一调用形态。

## What Changes

- 新增受控的 DeepSeek Harness sidecar：Rust 进程作为任务状态与工程数据的权威来源，启动并监管 sidecar，经进程外 JSON-RPC 发送冻结的 ASR 输入并接收结果。Harness 与后续渲染运行时使用独立子进程，便于分别限制资源、取消和回收。
- 随应用打包固定版本的运行时、Profile 与 Skill，不依赖用户全局安装或用户目录的 Agent 配置；升级需固定版本并通过 macOS arm64、macOS Intel x86_64 与 Windows x64 的协议与恢复测试。
- 应用专用 Profile 只注册模型路由、Skill 与明确授权的业务工具。shell、任意文件编辑、浏览器浏览与任意网络访问工具 MUST NOT 进入生产 Profile。
- ASR 文本始终视为待分析数据，不能作为 Harness 的系统指令或工具配置。
- 模型调用改为**单一形态**：客户端 Rust 侧持 `GET /api/client/ai/llm-ak` 返回的短期 token（`{ak, expireAt, provider}`，`provider=ALI`，形如 `st-lt-v1.xxx`）直连百炼。**不新增任何服务端接口**，也不存在服务端模型网关或网关短期令牌。
- 现有三条链路（精彩分析、转场匹配、文本纠错）改为经同一个客户端调用点取模型与参数，不再各自维护地址与模型；一次运行开始前冻结配置快照，运行指纹与缓存键包含实际模型、模型版本与相关运行时版本。
- 模型配置在每次心跳周期边界重新解析，**配置切换接受最长一个心跳周期（90 秒）生效**；**进行中的运行不受配置切换影响**。重启后重新解析模型配置；已发布结果按产出它的运行指纹匹配，指纹不一致时保留为历史建议、不自动应用。
- 新增图片与视频生成的受控业务工具：Harness 只能提交结构化请求并取得业务任务 ID，不能持有短期 token，也不能持有任意下载地址；实际调用由 Rust 侧完成，并由 Rust 校验时间、互斥类型、调用次数与本地冻结的额度上限。
- 客户端 Harness 只服务所在设备，不为桌面任务部署服务端常驻 Agent。
- **BREAKING**：模型调用不再由各业务模块各自直连上游；设置页不再要求用户自备上游 API Key，改为展示当前模型、解析来源、短期 token 状态与 heartbeat 判定的开通/额度状态。
- 非目标：本次不实现特效字幕与生成式动画两条分支本身（分别在 `add-effect-caption-renderer` 与 `add-generative-video-animation`）；不实现组合渲染；不改动本地 ASR、录制、预览与转场素材目录；不新增任何服务端接口，不实现服务端记账（`add-cloud-ai-job-backend` 已搁置）；不为桌面任务常驻服务端 Agent runner。

## Capabilities

### New Capabilities

- `harness-sidecar-runtime`: 受控 Harness sidecar 的进程监管、进程外 JSON-RPC 协议、应用专用 Profile 与 Skill 打包、受限业务工具授权、版本钉死，以及仅服务所在设备的边界。

### Modified Capabilities

- `llm-provider-configuration`: 从“用户自备 Key 直连”改为“客户端持短期 token 直连百炼”，新增配置解析时机（心跳周期边界）、运行内冻结、运行指纹与缓存键、重启后重新解析、生产构建不接受手输地址的要求；不再有“两条一等公民路径”。
- `ai-highlight-agent-workflow`: 候选发现与全局评分 Agent 的模型来源改为客户端直连百炼的同一冻结模型。
- `ai-transition-material-matching`: 两个受限 Agent 阶段的模型来源改为客户端直连百炼的同一冻结模型。
- `ai-clip-text-correction`: 分批纠错调用的模型来源改为客户端直连百炼，客户端不再要求用户配置 Key。
- `desktop-client-shell`: 设置页不再管理上游 API Key；数据本地边界保留“经用户授权的最小文本发往百炼”这一例外。

## Impact

- 新增依赖：进程外 Harness 运行时与其打包资源，需要进入 Runtime Resource Pack 的组件清单与发布校验，并提供 macOS arm64、macOS Intel x86_64 与 Windows x64 三个架构；这是对已归档变更“不引入 Node.js 或 Node sidecar”决策的正式推翻，理由与替代方案记录在本 change 的 `design.md`。
- 客户端：`src-tauri/src/ai/llm.rs`（移除硬编码上游地址与“只支持单一 Provider”的校验，改为经统一调用点取 Base URL 与模型）、新增统一调用点与其来源记录、`src-tauri/src/ai/desktop_runtime.rs`（sidecar 生命周期与诊断）、`src-tauri/src/ai/highlight.rs` 与 `src-tauri/src/transition_matching.rs` 与 `src-tauri/src/ai/clip_text_correction.rs`（三条链路改为经统一调用点调用）、`ui/src/App.tsx` 设置页。
- 数据与迁移：`llm_provider_settings` 新增模型来源与解析时间戳字段；不再要求用户自备上游 Key，既有凭据保留不强制清除。需要一次迁移保留阈值与既有配置。
- 服务端依赖：**无新增接口**。仅复用 `POST /api/client/activate`、`POST /api/client/heartbeat` 与 `GET /api/client/ai/llm-ak`；短期 token 只在 Rust 侧内存中持有。
- 可用性影响：AI 能力依赖账号开通状态与 heartbeat 下发的额度结论；需要按 heartbeat 响应显示可用性，并在界面与诊断中显示当前生效模型与解析来源。
- 隐私与合规：提示词与 ASR 正文经客户端直连发出，客户端须落实脱敏日志；不得发送视频、音频、本地路径、签名流地址、激活信息或数据库其他内容。
