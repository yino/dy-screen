# Windows x64 构建与发行

本文说明如何在原生 Windows 10/11 x64 环境构建“切片智能体”的运行资源、静态资源目录和
Tauri 2.0 NSIS 安装器。macOS 上的 `cargo-xwin` 只用于条件编译，不能生成或验收正式安装包。

## 支持边界

- 系统：Windows 10/11 x64，CPU 至少支持 SSE4.2，建议至少 8 GB 内存。
- 安装器：NSIS，当前用户安装，Bundle ID 保持 `com.yino.dyscreen`。
- ASR：Whisper CPU，不启用 AVX/AVX2、GPU 或网络模型下载。
- 视频：从锁定的 FFmpeg 8.1.2 源码构建 LGPL 版本，H.264 导出使用 `h264_mf`。
- 不支持 Windows ARM64/x86、Windows 7/8、MSIX、Microsoft Store 和 GPU ASR。

## 构建环境

在 Visual Studio 2022 x64 Developer PowerShell 中准备：

- Visual Studio 2022 Build Tools（C++ 桌面工具集）和 Windows 10/11 SDK；
- Rust stable MSVC、Node.js 22、npm、CMake、NSIS；
- MSYS2 UCRT64，以及 GCC、make、pkg-config、NASM、tar 和 zlib 开发包；
- Windows PowerShell 5.1、`dumpbin.exe` 和 `signtool.exe`。

先执行：

```powershell
make windows-build-doctor
make asr-test-windows-scripts
```

诊断只输出工具名称、版本和布尔状态，不输出证书私钥、密码或用户目录。缺少工具时退出码为 2。

## 构建原生资源

仓库本地可信目录保存锁定源码归档；大型模型和二进制被 `.gitignore` 排除，不进入 Git。

```powershell
make asr-ffmpeg-windows
make asr-whisper-windows
```

FFmpeg 脚本校验官方 8.1.2 归档 SHA-256、LGPL/GPL 边界、x64 PE、系统 DLL 依赖以及录制、
预览、封面、ASR 音频准备和带字幕 MP4 导出能力。Whisper 脚本校验 v1.9.1 归档、提交、
SSE4.2 CPU 基线、静态 Whisper/GGML/MSVC 运行库和 PE 依赖。

从 Microsoft 官方渠道取得 `vc_redist.x64.exe`，并准备对应许可/来源说明。组装脚本不会联网：

```powershell
make asr-prepare-windows `
  VC_REDIST_SOURCE=C:/release-inputs/vc_redist.x64.exe `
  VC_REDIST_LICENSE=C:/release-inputs/Microsoft-VCRedist.txt
```

脚本验证 Microsoft Authenticode、x64 PE、文件版本、普通文件和重解析点边界，再原子生成
`resources/asr-source-windows/`、`manifest.json`、`SHA256SUMS` 和脱敏构建记录。

## 无签名开发包

开发资源必须使用非 `stable` channel。开发安装包可无签名，只用于本地安装测试：

```powershell
make app-build-windows-dev `
  WINDOWS_ASR_SOURCE=resources/asr-source-windows `
  RESOURCE_CHANNEL=development `
  WINDOWS_RESOURCE_BASE_URL=https://resources.example/development/0.2.0/windows/x86_64/2026.07.4/
```

如需生成可上传的开发资源目录：

```powershell
make runtime-resource-publish-windows-dev `
  RESOURCE_CHANNEL=development `
  WINDOWS_RESOURCE_BASE_URL=https://resources.example/development/0.2.0/windows/x86_64/2026.07.4/
```

`asr-bundle` 会拒绝把无签名开发资源写入 `stable`。

## 正式签名与打包顺序

正式发布必须严格按以下顺序执行，不能在 staging 后重新签署 sidecar：

1. 使用证书存储中的 Authenticode 证书签署项目自产 PE/DLL。
2. 从签名后的可信源目录生成单平台 staging，封存签名后 SHA-256。
3. 生成 Runtime manifest 规范 payload，在隔离签名环境使用 Ed25519 私钥签名。
4. 应用签名并使用应用内嵌公钥立即验证。
5. 校验 staging、FFmpeg 能力、Authenticode 和 Runtime manifest 签名。
6. 构建并签署 NSIS，再生成在线资源目录。

```powershell
make asr-sign-windows-resources `
  WINDOWS_CERTIFICATE_THUMBPRINT=证书指纹 `
  WINDOWS_TIMESTAMP_URL=https://时间戳服务

make asr-stage-windows `
  WINDOWS_ASR_SOURCE=resources/asr-source-windows `
  RESOURCE_CHANNEL=stable

make runtime-manifest-payload
# 在隔离签名环境签署 dist/runtime-manifest.payload。
make runtime-manifest-apply-signature `
  RUNTIME_MANIFEST_SIGNATURE=C:/release-inputs/runtime-manifest.sig

make app-build-windows-release `
  WINDOWS_CERTIFICATE_THUMBPRINT=证书指纹 `
  WINDOWS_TIMESTAMP_URL=https://时间戳服务

make runtime-resource-publish-windows `
  WINDOWS_CERTIFICATE_THUMBPRINT=证书指纹 `
  WINDOWS_TIMESTAMP_URL=https://时间戳服务
```

Ed25519 私钥必须与 `EMBEDDED_MANIFEST_PUBLIC_KEY` 匹配。仓库不保存私钥；没有匹配私钥时，
必须先经过独立安全审查轮换应用内公钥，不能关闭签名校验或伪造签名。

NSIS 默认输出到 `src-tauri/target/release/bundle/nsis/`，脱敏构建记录输出到 `dist/windows/`。
正式记录包含安装包名称、大小、SHA-256、资源版本和证书指纹，不包含 PFX、密码或认证头。

## 安装、升级与卸载

安装器随包提供离线 WebView2 和 Microsoft VC++ x64 运行库。VC++ 返回 0、3010 或 1638 视为成功；
1638 表示系统已安装更新的兼容运行库。其他退出码中止安装。覆盖升级保持 Bundle ID，不应删除应用
数据目录中的 SQLite、设置、录像、
预览、ASR 产物、系统凭据和上一份完整资源。卸载仅管理安装目录；正式发行仍必须在干净设备对
首次安装、同版本重装、旧版本升级和卸载逐项做文件哈希验收。

DeepSeek Key 在 Windows 上保存到当前用户 Credential Manager 的通用凭据
`dy-screen.deepseek.default`，不会写入 SQLite、日志、安装包或资源修复目录。

## 正式验收

```powershell
make asr-check-windows
make asr-test-windows-target ASR_RESOURCE_ROOT=resources/asr-stage ASR_TARGET_EVIDENCE=C:/evidence/target.json
make asr-verify-release-windows `
  ASR_INSTALLER=C:/release/切片智能体-setup.exe `
  ASR_INSTALL_DIR="C:/验收 用户/切片智能体" `
  ASR_VIDEO="C:/验收 用户/测试 视频.mp4" `
  ASR_SIGNER_THUMBPRINT=证书指纹 `
  ASR_SMARTSCREEN_EVIDENCE=人工证据编号 `
  ASR_RELEASE_EVIDENCE=C:/evidence/release.json
```

验收必须覆盖断网首次启动、中文和空格路径、公开直播录制、预览、首帧、本地 ASR、带字幕导出、
取消清理、资源损坏/修复、升级/卸载和无遗留 FFmpeg/Whisper/VAD 进程。缺少原生 Windows、
有效代码签名、时间戳、SmartScreen 或目标机报告时，不得更新 stable index。

## 故障排查

- `Windows staging 不完整`：先执行 `asr-prepare-windows` 和 `asr-stage-windows`。
- `Runtime manifest 签名无效`：检查 key ID、payload 是否重建，以及私钥是否匹配内嵌公钥。
- `组件路径、大小、哈希...不一致`：签名后资源发生变化；重新 staging 和 Runtime 签名。
- `FFmpeg 缺少 h264_mf/overlay`：检查 MSYS2 UCRT64 依赖和 FFmpeg 配置，不得回退 GPL 编码器。
- `VC++ 签名无效`：重新从 Microsoft 官方渠道取得 x64 安装器，禁止使用镜像站二进制。
- `开发资源不得发布到 stable`：使用 `RESOURCE_CHANNEL=development` 和匹配的 HTTPS 路径。

每次候选发行复制 [Windows 发行清单模板](../templates/windows-release-checklist.md)，填写真实哈希和证据编号。
