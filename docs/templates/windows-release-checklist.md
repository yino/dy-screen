# Windows x64 发行清单模板

复制本文件生成一次不可覆盖的候选发行记录。所有“通过”项必须附真实文件、命令输出或脱敏报告，
不得用模板值、macOS 交叉编译或无签名开发包代替。

## 版本与产物

| 字段 | 值 |
| --- | --- |
| 发行编号 | 待填写 |
| 应用版本 / Git commit | 待填写 |
| channel | `stable` |
| 平台 / 架构 | `windows / x86_64` |
| 资源 bundleVersion | 待填写 |
| 固定 HTTPS 基础地址 | 待填写 |
| NSIS 文件 / 大小 / SHA-256 | 待填写 |
| `runtime-manifest.json` SHA-256 | 待填写 |
| `index.json` SHA-256 | 待填写 |

## 资源与许可证

| 组件 | 版本 | 大小 | SHA-256 | 来源 / 许可证 |
| --- | --- | --- | --- | --- |
| `ffmpeg.exe` | 待填写 | 待填写 | 待填写 | FFmpeg 官方源码 / LGPL |
| `ffprobe.exe` | 待填写 | 待填写 | 待填写 | FFmpeg 官方源码 / LGPL |
| `whisper-cli.exe` | 待填写 | 待填写 | 待填写 | whisper.cpp / MIT |
| `vad-speech-segments.exe` | 待填写 | 待填写 | 待填写 | whisper.cpp / MIT |
| small-q5_1 模型 | 待填写 | 待填写 | 待填写 | 待填写 |
| Silero VAD | 待填写 | 待填写 | 待填写 | MIT |
| OpenCC 字典 | 待填写 | 待填写 | 待填写 | Apache-2.0 |
| `vc_redist.x64.exe` | 待填写 | 待填写 | 待填写 | Microsoft |

- [ ] `manifest.json`、`runtime-manifest.json` 与逐文件真实哈希一致。
- [ ] `SHA256SUMS` 已保存且不包含开发机绝对路径。
- [ ] FFmpeg 未启用 GPL/nonfree，全部第三方许可证随包提供。
- [ ] Microsoft WebView2/VC++ 安装器只验证 Microsoft 签名，未被重新签署。

## 签名

| 字段 | 值 |
| --- | --- |
| Authenticode 证书 SHA-256 指纹 | 待填写 |
| RFC 3161 时间戳服务 | 待填写 |
| 时间戳时间 | 待填写 |
| Runtime manifest key ID | 待填写 |
| Runtime manifest 公钥指纹 | 待填写 |
| SmartScreen 证据编号 | 待填写 |

- [ ] 主程序、项目 sidecar/DLL、安装器和卸载器签名有效且带时间戳。
- [ ] 日志和产物不包含 PFX、私钥、密码、认证头或开发机绝对路径。

## Windows 目标机证据

| 报告 | 编号 / SHA-256 | 结论 |
| --- | --- | --- |
| Windows 构建工具版本记录 | 待填写 | 待填写 |
| 中文与空格路径业务验收 | 待填写 | 待填写 |
| 断网首次安装与启动 | 待填写 | 待填写 |
| 资源删除/篡改/取消/修复 | 待填写 | 待填写 |
| 同版本重装与覆盖升级 | 待填写 | 待填写 |
| 卸载与用户数据保留 | 待填写 | 待填写 |
| SmartScreen 人工证据 | 待填写 | 待填写 |

- [ ] 公开直播录制、预览、首帧、本地 ASR 和带 ASR 字幕 MP4 导出通过。
- [ ] 本地智能成片完成 ASR、高光、自动入选、字幕纠错、转场匹配、人工审阅和显式 H.264/AAC 导出。
- [ ] 直播智能成片只处理完成分片；乱序/重复事件、积压、失败重试、应用重启和直播结束均收敛。
- [ ] 人工编辑与导出冻结后，新高光进入下一版草稿，旧 MP4 和工程保持不变。
- [ ] 取消和退出后没有遗留 FFmpeg、Whisper 或 VAD 子进程。
- [ ] SQLite、设置、录像、预览、ASR 产物、Credential Manager Key 和资源回滚版本符合保留策略。
- [ ] stable 资源目录不存在同路径旧产物，上传不会覆盖不可变 bundleVersion。

## 发布批准

| 角色 | 姓名 / 标识 | 时间 | 结论 |
| --- | --- | --- | --- |
| 构建 | 待填写 | 待填写 | 待填写 |
| 安全/签名 | 待填写 | 待填写 | 待填写 |
| Windows 验收 | 待填写 | 待填写 | 待填写 |
| 发布 | 待填写 | 待填写 | 待填写 |

只有以上全部通过，才可上传不可变资源目录、更新 stable Windows index 并发布 NSIS。
