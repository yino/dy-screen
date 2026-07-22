# ASR 固定媒体样本

本目录保存本地 ASR 各阶段测试使用的小型、脱敏媒体文件。测试代码直接读取这些文件，
不会联网，也不会访问用户录像。

| 文件 | 目的 |
| --- | --- |
| `short_zh.mp4` | 中文短句、金额与基础时间戳 |
| `no_speech.mp4` | 有音轨但全静音，验证 VAD 不生成虚假文本 |
| `music_only.mp4` | 纯音乐，验证非人声过滤 |
| `video_only.mp4` | 无音轨视频，验证媒体探测的独立失败 |
| `mixed_zh_en.mp4` | 中英混合识别 |
| `session_part_001.mp4` / `session_part_002.mp4` | 连续分片边界重复与项目时间映射 |
| `Windows 中文路径/测试 视频.mp4` | Windows Unicode、空格路径和参数数组调用 |

`manifest.json` 是测试期望的唯一来源。需要更新音频内容时，在 macOS 维护机运行：

```bash
./scripts/generate-asr-fixtures.sh
```

生成后必须重新执行 FFprobe fixture 测试和真实 Whisper 集成测试，并根据实际结果审查
`manifest.json`，禁止只替换二进制而保留过期断言。
