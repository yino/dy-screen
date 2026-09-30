## MODIFIED Requirements

### Requirement: 验证安装包离线可用和资源下载修复
发行验收 SHALL 同时验证完整资源随包离线启动、资源缺失时下载修复、资源篡改被拒绝、安装后录制/预览/封面/ASR/带字幕剪辑导出/特效字幕渲染共用受控 FFmpeg、渲染运行时、固定版本渲染浏览器、渲染器引擎脚本和平台 sidecar。用于剪辑导出的 FFmpeg SHALL 具备 H.264/AAC 编码、PNG 解码、RGBA/alpha PNG 解码、concat/image2 demuxer 以及 concat、overlay、scale、pad、format、setpts、asetpts、aformat、volume 和 fade 滤镜；缺少任一必需能力时 MUST 拒绝发布。

#### Scenario: 完整安装包离线启动
- **WHEN** 设备断网且安装包资源完整
- **THEN** 应用完成启动门禁并可执行监听、录制、视频预览、首帧封面、本地 ASR、带 ASR 字幕的剪辑导出和一条高光的特效字幕渲染

#### Scenario: 缺失资源在线修复
- **WHEN** 安装后移除或损坏资源且设备可以访问自有 HTTPS 服务器
- **THEN** 应用显示下载进度，完成校验后恢复可用状态，不修改用户录像、ASR 产物和数据库

#### Scenario: 篡改资源被拒绝
- **WHEN** 安装包或下载归档中的任一资源内容被替换
- **THEN** 启动或安装校验失败，应用不执行该文件并显示可操作修复信息

#### Scenario: 发行 FFmpeg 缺少字幕导出能力
- **WHEN** 平台 FFmpeg 缺少 PNG 解码、RGBA/alpha PNG 解码、图片序列拼接、画面叠加或受控 H.264/AAC 编码能力之一
- **THEN** 发行校验失败，不允许生成看似可用但无法导出带字幕视频的安装包或资源包

#### Scenario: 发行产物不含开发机路径
- **WHEN** 对 `.app`、DMG、NSIS 和资源 manifest 执行路径审计
- **THEN** 产物不包含 Homebrew、CMake 构建目录、绝对临时路径或开发机私有资源链接

#### Scenario: 缺少渲染运行时或渲染浏览器
- **WHEN** 当前平台的资源包未包含渲染运行时、固定版本渲染浏览器、渲染器引擎脚本、锁定字体，或其哈希与架构校验失败
- **THEN** 发行校验失败，且应用在运行时不执行任何组合渲染

#### Scenario: 引擎脚本被替换或版本不符
- **WHEN** 随包引擎脚本内容被替换，或其版本、哈希与 manifest 不一致
- **THEN** 发行校验或运行时完整性校验失败，应用拒绝渲染并显示可操作修复信息，MUST NOT 回退到未校验的脚本
