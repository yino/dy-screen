# 第三方组件与模型声明

本文件适用于包含本地 ASR 资源的直播管家安装包。发行构建必须使用
`resources/asr/manifest.json` 锁定的版本、来源和 SHA-256；不得用名称相同但来源、编译选项
或许可证不明的文件替换。安装包同时携带本文件，完整许可证文本应随所采用的具体二进制
发行物一并保存。

## whisper.cpp

- 组件：`whisper-cli`、`vad-speech-segments`；
- 固定版本：`v1.9.1`；
- 固定提交：`f049fff95a089aa9969deb009cdd4892b3e74916`；
- 上游：https://github.com/ggml-org/whisper.cpp；
- 许可证：MIT；
- 分发要求：保留上游版权和 MIT 许可声明。macOS arm64 使用 Metal 构建，Windows x64
  使用 CPU 构建；发行记录必须保存编译命令、目标架构和产物哈希。

## OpenAI Whisper small 多语言模型

- 文件：`ggml-small-q5_1.bin`；
- 逻辑 ID：`whisper-small-multilingual-q5_1`；
- 来源：https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-small-q5_1.bin；
- SHA-256：`ae85e4a935d7a567bd102fe55afc16bb595bdb618e11b2fc7591bc08120411bb`；
- 许可证：OpenAI Whisper 仓库以 MIT 许可证发布，GGML 转换与下载脚本来自
  `whisper.cpp` MIT 项目；
- 分发要求：保留来源、模型名称和许可说明，不宣称模型输出必然准确。模型可能生成错误
  文本，业务界面应把结果作为机器转写而不是事实证明。

## Silero VAD

- 文件：`ggml-silero-v6.2.0.bin`；
- 逻辑 ID：`silero-vad-v6.2.0`；
- 来源：https://huggingface.co/ggml-org/whisper-vad/resolve/main/ggml-silero-v6.2.0.bin；
- SHA-256：`2aa269b785eeb53a82983a20501ddf7c1d9c48e33ab63a41391ac6c9f7fb6987`；
- 许可证：MIT；
- 分发要求：保留来源和 MIT 许可声明。VAD 只判断可能的人声范围，不生成文字。

## FFmpeg 与 FFprobe

- 组件：平台匹配的 `ffmpeg` 与 `ffprobe`；
- macOS 固定源码：`ffmpeg-8.1.2.tar.xz`，SHA-256
  `464beb5e7bf0c311e68b45ae2f04e9cc2af88851abb4082231742a74d97b524c`；
- 上游：https://ffmpeg.org/；
- 许可证：FFmpeg 默认可按 LGPL-2.1-or-later 分发；启用 GPL 组件后，整个 FFmpeg 二进制
  受 GPL-2.0-or-later 约束；启用 nonfree 组件的构建不得再分发；
- 发行门禁：构建流水线必须保存 `ffmpeg -version` 和 `ffprobe -version` 输出，并拒绝包含
  `--enable-nonfree` 的产物。若配置包含 `--enable-gpl`，安装包及对应源代码提供方式必须
  按 GPL 要求处理；首版推荐使用不含 GPL/nonfree 编码器的 LGPL 构建；
- 动态链接：若产物依赖动态库，必须把相应库及许可证一起打包，并在干净目标机离线验证；
  不得把指向 Homebrew、开发机或 CI 路径的符号链接放入安装包。
- macOS 首版使用 `scripts/build-asr-ffmpeg-macos.sh` 构建 LGPL-2.1-or-later、禁用网络且不含
  GPL/nonfree 外部组件的最小音频 sidecar，并把 7 个 dylib 改写为包内相对依赖。

## OpenCC TSCharacters

- 文件：`normalization/TSCharacters.txt`；
- 固定版本：OpenCC `ver.1.4.1`；
- 来源：https://github.com/BYVoid/OpenCC/blob/ver.1.4.1/data/dictionary/TSCharacters.txt；
- SHA-256：`737c21c66f55a419dd6956cb3089476cdefc5a36877452631617696df1e5d925`；
- 许可证：Apache-2.0；
- 分发要求：保留 Apache-2.0 许可证与上游 NOTICE（如上游发行物包含）。

## Microsoft Visual C++ 运行库

- 文件：Windows 安装包中的 `vc_redist.x64.exe`；
- 名称：Microsoft Visual C++ 2015-2022 Redistributable x64；
- 来源：https://learn.microsoft.com/cpp/windows/latest-supported-vc-redist；
- 分发要求：只从 Microsoft 官方地址取得并遵守 Microsoft Visual Studio 许可条款。NSIS
  安装器以 `/install /quiet /norestart` 调用；退出码不是 `0` 或 `3010` 时安装失败，不允许
  悄悄跳过后继续提供不可用的 ASR 环境。

## WebView2 Runtime

- Windows 安装包使用 Tauri 的 `offlineInstaller` 模式携带 WebView2 离线安装程序；
- 来源与许可遵循 Microsoft Edge WebView2 Runtime 分发条款；
- 该组件服务桌面界面，不参与视频、音频或转写上传。

## 发行检查清单

每个正式安装包必须保留以下证据：

1. `asr-bundle verify` 通过记录；
2. manifest、模型、VAD、规范化字典和平台 sidecar 的归档哈希；
3. `whisper-cli --version`、`ffmpeg -version`、`ffprobe -version` 输出；
4. macOS 主程序和全部可执行资源的 Developer ID 签名、公证及 Gatekeeper 检查结果；
5. Windows 主程序、sidecar、安装器的 Authenticode 签名和时间戳结果；
6. 干净目标机离线安装、卸载、中文路径和 ASR 阶段测试结果；
7. 本文件及对应完整许可证文本已进入安装包。
