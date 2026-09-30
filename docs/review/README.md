# 探针产出实测记录（纯文本）

来源：`validate-dashscope-direct-capabilities` 步骤 0 与随后的本地帧可达性验证，2026-09-30。
用途：确认万相文生图与图生视频在客户端直连口径下可用，核对返回的媒体属性，并验证"本地文件 → 上游可读引用"。

> **产出的图片与视频不入库**：它们仅是本地证据，已由 `.gitignore` 排除（`docs/review/*.mp4` 等）。
> 下表保留全部实测属性（尺寸、帧率、字节数、耗时），需要原始媒体时按同样的请求参数在本地重新发起。

| 产出 | 来源任务 | 请求参数 | 实测属性 |
| --- | --- | --- | --- |
| 文生图 | 文生图 `wanx2.1-t2i-turbo` | prompt = `a red circle on white background`，size = `1024*1024`，n = 1 | PNG 1024x1024，8-bit RGB，72,716 字节，耗时约 12 秒 |
| 图生视频（横屏） | 图生视频 `wanx2.1-i2v-turbo` | 参考图 = 上游示例图（沙滩+金毛，2048x1365 横屏），resolution = `480P`，duration = 5 | MP4 / H.264，768x512，30 fps，**实际时长 5.367 秒**，5,025,427 字节，耗时约 107 秒 |
| 图生视频（竖屏） | 图生视频 `wanx2.1-i2v-turbo` | 参考图 = 仓库内真实录制抽帧（720x1280 竖屏），resolution = `480P`，duration = 5 | MP4 / H.264，464x832，30 fps，**实际时长 5.367 秒**，3,424,802 字节，耗时约 200 秒 |

## 一、生成规格的实测特征（对结果校验与降级有直接影响）

1. **实际时长 ≠ 请求时长**：请求 `duration=5` 得到 **5.367 秒**，偏差 **+7.3%**，超过 `add-generative-video-animation` 冻结的 5% 容差。
   → 印证了 `replace` 必须"由生成资产实际时长反向确定目标源范围"：若先声明范围再要求等长，这类请求会大量不可满足并浪费已付费调用。
2. **上游会改写提示词**：文生图与图生视频都返回 `actual_prompt`，与 `orig_prompt` 明显不同。
   例：`a red circle on white background` → `Minimalist graphic design, perfectly centered vibrant crimson circle...`。
   → 分镜 Skill 的提示词设计 MUST NOT 假定上游原样使用输入；结果校验 SHOULD 记录 `actual_prompt` 以便复核风格偏离。
3. **延迟量级差异大**：文生图约 **12 秒**、图生视频约 **107 秒**（成功案例）。
   → 轮询超时必须覆盖该量级；界面需要按此量级给出真实进度预期。
4. **输出规格**：请求 `480P` 得到 **768×512**；文生图 `1024*1024` 得到 1024×1024 PNG。

## 二、本地帧可达性（已完成验证）

结论、两条路径的对照与实现影响见 `openspec/changes/validate-dashscope-direct-capabilities/local-frame-accessibility.md`。摘要：

- 本地帧必须走**三步直传**：`POST /api/v1/files` → `GET /api/v1/files/{file_id}` 取签名临时 URL → 作为 `input.img_url`。
- `oss://` 引用被拒：`InvalidParameter.DataInspection`。
- `getPolicy` 表单路径同样不可用于参考帧（对象私有，上游读不到），与音频路径同构。

## 三、真实录制帧的端到端验证（已完成）

以仓库内真实录制抽帧走三步直传提交图生视频，共 5 组对照（`wanx2.1-i2v-turbo` / `480P` / `duration=5`）：

| 输入帧 | 规格 | 终态 | 产出 |
| --- | --- | --- | --- |
| 录制抽帧 1（竖屏） | `720x1280` | ❌ `AlgoError`（190 秒） | 无 |
| 录制抽帧 2（竖屏） | `720x1280` | ✅ `SUCCEEDED`（约 200 秒） | `464x832`，5.367 秒 |
| 录制抽帧裁切（横屏） | `1024x574` | ✅ `SUCCEEDED`（约 200 秒） | `832x464`，5.367 秒 |
| 横屏帧放大旋转（竖屏） | `1280x2276` | ✅ `SUCCEEDED` | `464x832`，5.367 秒 |
| 上游公开示例图（横屏） | `2048x1365` | ✅ `SUCCEEDED`（约 107 秒） | `768x512`，5.367 秒 |

**结论一：本地帧路径可用。** 三步直传后以签名临时 URL 作为 `img_url` 被接受；`oss://` 引用被拒（`InvalidParameter.DataInspection`）。

**结论二：失败是偶发上游缺陷，与输入方向无关。** 两个同为 `720x1280` 的真竖屏帧一败一成；
失例堆栈为上游插帧阶段空文件（`TypeError: expected str... not NoneType`），不是内容审查也不是引用不可读。
因此生成链路必须按"偶发失败 + 受门禁约束的重试"设计，且每次重试都是新的付费提交。

**结论三：产出方向跟随输入方向**，分辨率按 `480P` 档位归一（横屏 `832x464`、竖屏 `464x832`），
实际时长恒为 `5.367` 秒（请求 5 秒，+7.3%，稳定超过 5% 容差）。

竖屏真实帧的成功产出为 `464x832`、5.367 秒（媒体不入库，属性见上表）。

## 计费

| 调用 | 结果 |
| --- | --- |
| 文生图 1 次 | `SUCCEEDED`，`image_count=1` |
| 图生视频（示例图）1 次 | `SUCCEEDED`，`video_count=1` / `video_duration=5` |
| 图生视频（真实竖屏帧 1）1 次 | `FAILED`（`AlgoError`，未返回用量） |
| 图生视频（真实横屏帧）1 次 | `SUCCEEDED`，`video_count=1` / `video_duration=5` |
| 图生视频（放大竖屏帧）1 次 | `SUCCEEDED`，`video_count=1` / `video_duration=5` |
| 图生视频（真实竖屏帧 2）1 次 | `SUCCEEDED`，`video_count=1` / `video_duration=5` |
| 文件转写 2 次 | 均 `SUCCEEDED`，各 `seconds=4` |

合计：图片 1 张、图生视频 4 次成功 + 1 次失败、转写 2 次、LLM 15 token。

