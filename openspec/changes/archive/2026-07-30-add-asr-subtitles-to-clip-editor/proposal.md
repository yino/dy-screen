## Why

当前剪辑工程只拼接精彩视频和音频，没有把前序 ASR 产物带入编辑器与成品，用户导出后仍需在其他工具中重新制作字幕。现有稳定句段已经包含源视频内时间戳和规范化文本，应直接复用为剪辑字幕，避免重复识别和时间漂移。

## What Changes

- 将剪辑片段与对应输入的已发布 ASR 稳定句段一并投影到工程时间轴，并裁剪越过片段边界的句段。
- 在剪辑编辑器播放器上默认显示当前工程时间对应的字幕，跨片段播放、定位和重排后保持同步。
- 导出 MP4 时由 Rust 生成受控的透明字幕画面序列，并通过 FFmpeg 将字幕烧录到最终画面。
- 扩展随应用分发的 FFmpeg 能力和发行校验，保证 PNG 解码、concat 图片序列和 overlay 滤镜在受支持平台可用。
- 字幕缺失时保持片段可预览，但拒绝导出不完整的“带字幕”成品并给出重新完成 ASR 的明确提示。
- 第一版不提供字幕文本编辑、逐字时间轴、翻译、自定义字体、位置或样式，也不重新执行 ASR。

## Capabilities

### New Capabilities

- `ai-clip-subtitles`: 定义 ASR 句段到剪辑工程的时间映射、编辑器字幕预览、受控字幕渲染和 MP4 烧录行为。

### Modified Capabilities

- `release-resource-bundling`: 要求随包 FFmpeg 和发行验收具备剪辑字幕画面序列的解码、拼接和叠加能力。

## Impact

- 后端：`src-tauri/src/ai/repository.rs`、`domain.rs`、`clip_export.rs` 与剪辑导出 Tauri command。
- 前端：剪辑工程 DTO、`AiWorkspace.tsx` 播放器字幕层、样式和交互测试。
- 发行：macOS FFmpeg 构建脚本、资源能力校验、README 与 Makefile 验证入口。
- 新增纯 Rust 字体栅格化与 PNG 编码依赖；字幕字体从受控运行资源优先解析，并在受支持操作系统字体中提供兼容回退。
