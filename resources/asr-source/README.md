# 本机 ASR 资源源目录

此目录用于保存已经校验过的本地 ASR 资源和源码归档，供后续离线构建复用。

- `manifest.json`、`models/`、`normalization/`、`licenses/` 和 `bin/`、`lib/` 是当前 macOS arm64 的封存输入；
- `sources/` 保存 Whisper.cpp v1.9.1 与 FFmpeg 8.1.2 官方源码归档；
- `make asr-stage-macos` 默认从本目录生成 `resources/asr-stage/`；
- `make asr-build-macos` 默认复用本目录，不会重新下载或依赖 `/private/tmp`。

资源包含大型二进制和模型，按 `.gitignore` 规则保存在本机，不应提交到 Git。更新资源时必须重新运行 `asr-bundle stage` 和完整 ASR 验证。
