## ADDED Requirements

### Requirement: 同步服务端转场素材目录
系统 SHALL 以本地已提交的 `catalogVersion` 调用 `GET /api/v1/transition-materials?clientVersion=<catalogVersion>`，并 SHALL 复用统一 Rust API 客户端的业务信封解析、超时、`device_id` 和 `activate_code` 请求头。首次同步 MUST 使用版本 `0`；`changed=true` 时系统 MUST 在完整校验后事务化发布全量目录，`changed=false` 时 MUST 保留当前本地快照。

#### Scenario: 首次获取完整目录
- **WHEN** 本地没有已提交目录且服务端返回 `changed=true`、合法 `catalogVersion` 和完整 `materials`
- **THEN** 系统在单个事务中保存目录元数据、提交服务端版本并让目录可供离线查询

#### Scenario: 服务端目录没有变化
- **WHEN** 服务端返回 `changed=false` 且未返回或返回空的 `materials`
- **THEN** 系统更新最后检查时间但不清空、不替换或降级本地目录

#### Scenario: 完整目录包含非法记录
- **WHEN** `changed=true` 的响应中任一素材缺少必要字段、键重复或媒体元数据非法
- **THEN** 系统回滚整个目录事务、继续使用上一可用版本并记录不含响应正文的脱敏错误

#### Scenario: 服务端版本低于本地版本
- **WHEN** 响应 `catalogVersion` 小于本地已提交版本
- **THEN** 系统拒绝自动降级、保留本地目录并发布可诊断的版本冲突状态

### Requirement: 校验素材标识和受信远程地址
系统 SHALL 以 `(assetKey, assetVersion)` 作为不可变素材版本标识，并 MUST 验证 `renderMode`、SHA-256、大小、时长、尺寸、帧率和编解码元数据。正式构建 MUST 只接受 HTTPS；相对媒体路径 SHALL 相对 `cdnBaseUrl` 解析，绝对媒体 URL MUST 与 `cdnBaseUrl` 的 scheme、host 和有效端口同源。开发构建 MAY 只对显式配置的 localhost 放行 HTTP。

#### Scenario: 解析相对素材路径
- **WHEN** 合法目录使用相对 `videoPath`、`previewPath` 或 `coverPath`
- **THEN** Rust 使用受信 `cdnBaseUrl` 解析并保存规范化引用，前端不接收可自行修改的下载 URL

#### Scenario: 绝对素材 URL 与 CDN 同源
- **WHEN** 绝对素材 URL 与 `cdnBaseUrl` 具有相同 scheme、host 和有效端口但路径前缀不同
- **THEN** 系统接受该 URL 并继续执行字段和完整性校验

#### Scenario: 拒绝不受信地址
- **WHEN** 素材 URL 使用非 HTTP(S) scheme、包含凭据、指向不同 origin，或正式构建使用 HTTP
- **THEN** 系统拒绝整个新目录且不发起对应下载

#### Scenario: 拒绝未知渲染模式
- **WHEN** 目录记录包含客户端不支持的 `renderMode`
- **THEN** 系统不发布该全量目录，并保留上一可用目录供既有工程使用

### Requirement: 在 SQLite 中保留版本化目录和下载状态
系统 SHALL 在 SQLite 保存目录状态、版本化素材元数据和源文件/预览缓存的下载状态，媒体二进制 MUST 保存在应用数据目录而非 SQLite。目录更新后未出现在新目录中的旧版本 MUST 标记为非当前但不得立即删除；工程引用 MUST 固定到 `assetKey + assetVersion`。

#### Scenario: 素材发布新版本
- **WHEN** 新目录为同一 `assetKey` 返回更高 `assetVersion`
- **THEN** 新版本成为当前可选项，既有工程仍引用并可读取原版本

#### Scenario: 素材从当前目录移除
- **WHEN** 全量新目录不再包含某个历史素材版本
- **THEN** 素材不再出现在默认可选目录，但历史工程引用和已经校验的本地文件继续保留

#### Scenario: 应用离线重启
- **WHEN** 客户端无法连接服务端但 SQLite 中存在已提交目录
- **THEN** 系统允许浏览本地目录和使用已下载素材，并把同步失败与授权状态分别展示

### Requirement: 按需下载并校验素材文件
系统 SHALL 只在 LLM 选中、用户请求预览或用户手动应用素材时按需下载媒体，并 SHALL 按 `(assetKey, assetVersion)` 合并并发请求。下载 MUST 写入临时文件、限制响应大小、核对声明的 `sizeBytes` 和 SHA-256，并仅在校验成功后原子发布稳定缓存文件。

#### Scenario: 多个消费者请求同一素材
- **WHEN** 匹配结果和用户预览同时请求同一素材版本
- **THEN** 系统只运行一个下载任务，并向所有消费者发布同一个完成或失败状态

#### Scenario: 素材下载并校验成功
- **WHEN** 下载内容大小和 SHA-256 均与目录一致
- **THEN** 系统原子发布文件、将状态更新为 `ready` 并允许预览和导出使用

#### Scenario: 素材校验失败
- **WHEN** 下载内容超出限制、大小不符、摘要不符或连接中断
- **THEN** 系统删除临时文件、标记稳定错误码、保留可重试入口且不把文件标记为可用

#### Scenario: 已校验缓存再次使用
- **WHEN** 相同素材版本已有与目录元数据一致的 `ready` 文件
- **THEN** 系统复用本地文件且不重复请求 CDN

### Requirement: 为 WebView 生成兼容预览缓存
系统 SHALL 根据素材元数据和实际探测结果判断 WebView 预览兼容性。H.264 等兼容源可直接通过受控本地媒体入口预览；HEVC 或实际不可播放的源 MUST 使用受控 FFmpeg 按需生成 H.264/AAC MP4 预览缓存，且最终导出 MUST 继续使用已校验源文件。

#### Scenario: 预览 H.264 素材
- **WHEN** 已下载素材经探测可由当前 WebView 播放
- **THEN** 系统直接提供受控本地预览并保持原文件不变

#### Scenario: 预览 HEVC 素材
- **WHEN** 已下载素材为 HEVC 或 WebView 能力探测判定不兼容
- **THEN** 系统展示转码进度、生成可复用 H.264/AAC 预览缓存并在完成后开始预览

#### Scenario: 素材没有预览图
- **WHEN** `previewPath` 和 `coverPath` 均为空或缩略图下载失败
- **THEN** 素材目录显示稳定文字占位，仍允许用户下载并预览视频

#### Scenario: 预览转码失败
- **WHEN** FFmpeg 缺少所需解码能力或转码进程失败
- **THEN** 系统保留源素材、显示可操作的中文诊断且不把失败缓存交给 WebView

### Requirement: 发布独立素材同步状态
系统 SHALL 向前端发布本地版本、服务端版本、最后成功时间及 `idle/checking/syncing/ready/upgrade_required/failed` 状态，并 MUST 使用稳定错误码和脱敏中文消息。素材同步或下载失败 MUST NOT 改变授权、监听、录制、ASR 或普通剪辑能力。

#### Scenario: 目录同步失败但授权有效
- **WHEN** 心跳续约成功而目录请求超时或解析失败
- **THEN** 客户端保持激活和既有后台任务，只在素材库展示失败状态与重试入口

#### Scenario: 用户手动重试目录同步
- **WHEN** 素材状态为失败且用户点击重试
- **THEN** 独立同步任务立即按当前本地版本重新请求，并防止与周期任务并发重复执行
