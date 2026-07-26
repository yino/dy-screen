## MODIFIED Requirements

### Requirement: 检测 FFmpeg 缺失
系统 SHALL 在初始化录制任务前验证资源管理器已经安装并校验当前平台的 FFmpeg 和 FFprobe；生产环境 MUST 使用资源管理器返回的受控路径，不得使用用户设置、PATH 或前端提交的任意可执行文件路径覆盖。

#### Scenario: 资源中的 FFmpeg 可用
- **WHEN** 资源门禁已通过且资源清单中的 FFmpeg/FFprobe 存在、权限正确、架构匹配、版本和 SHA-256 校验通过
- **THEN** 系统使用这组媒体工具启动录制，并把同一组受控工具提供给预览、封面和 ASR

#### Scenario: FFmpeg 资源缺失
- **WHEN** 资源包中缺少 FFmpeg、FFprobe 或其平台动态库
- **THEN** 系统在资源准备阶段阻止进入主功能，不启动任何录制进程，并提供下载或修复入口

#### Scenario: FFmpeg 可执行文件不存在
- **WHEN** 已配置的可执行文件无法找到或启动
- **THEN** 系统返回可操作的前置依赖错误，且不启动任何录制进程

#### Scenario: 用户配置路径无效
- **WHEN** 旧数据库中存在 FFmpeg/FFprobe 路径，或前端尝试提交任意可执行路径
- **THEN** 生产录制忽略该路径并使用已验证资源；开发测试的显式覆盖不得影响发行构建

#### Scenario: FFmpeg 启动失败
- **WHEN** 已校验的 FFmpeg 进程仍无法启动或版本输出不符合 manifest
- **THEN** 系统报告类型化资源错误，不启动当前或其他新的录制任务，并保留已完成分片
