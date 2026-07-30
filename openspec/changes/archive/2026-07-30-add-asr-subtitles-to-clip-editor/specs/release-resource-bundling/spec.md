## MODIFIED Requirements

### Requirement: 验证安装包离线可用和资源下载修复
发行验收 SHALL 同时验证完整资源随包离线启动、资源缺失时下载修复、资源篡改被拒绝、安装后录制/预览/封面/ASR/带字幕剪辑导出共用受控 FFmpeg 和平台 sidecar。用于剪辑导出的 FFmpeg SHALL 具备 H.264/AAC 编码、PNG 解码、concat/image2 demuxer 以及 concat、overlay、scale、pad、format、setpts、asetpts、aformat、volume 和 fade 滤镜；缺少任一必需能力时 MUST 拒绝发布。

#### Scenario: 完整安装包离线启动
- **WHEN** 设备断网且安装包资源完整
- **THEN** 应用完成启动门禁并可执行监听、录制、视频预览、首帧封面、本地 ASR 和带 ASR 字幕的剪辑导出

#### Scenario: 缺失资源在线修复
- **WHEN** 安装后移除或损坏资源且设备可以访问自有 HTTPS 服务器
- **THEN** 应用显示下载进度，完成校验后恢复可用状态，不修改用户录像、ASR 产物和数据库

#### Scenario: 篡改资源被拒绝
- **WHEN** 安装包或下载归档中的任一资源内容被替换
- **THEN** 启动或安装校验失败，应用不执行该文件并显示可操作修复信息

#### Scenario: 发行 FFmpeg 缺少字幕导出能力
- **WHEN** 平台 FFmpeg 缺少 PNG 解码、图片序列拼接、画面叠加或受控 H.264/AAC 编码能力之一
- **THEN** 发行校验失败，不允许生成看似可用但无法导出带字幕视频的安装包或资源包

#### Scenario: 发行产物不含开发机路径
- **WHEN** 对 `.app`、DMG、NSIS 和资源 manifest 执行路径审计
- **THEN** 产物不包含 Homebrew、CMake 构建目录、绝对临时路径或开发机私有资源链接
