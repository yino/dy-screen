## Why

v2 已从"客户端经我方服务端网关调用上游"改为"客户端直连阿里云百炼"，并砍掉全部服务端新增接口。这个方向成立与否，取决于服务端签发的短期凭据（`GET /api/client/ai/llm-ak`，返回 `provider=ALI` 的 `st-lt-v1.*` token）究竟能驱动哪些百炼能力。该矩阵此前只是假设；若在实现中途才发现某类能力不可用，会连带推翻特效字幕与生成式动画两条分支的依赖设计。因此在动任何客户端代码前，必须用真实凭据对四类能力取得可复现的实测结论。

## What Changes

- 用现有 `GET /api/client/ai/llm-ak` 取得的真实临时凭据，实测四类能力的可用性并记录终态、错误码与用量返回：通义 LLM（兼容模式）、万相文生图、万相图生视频、百炼文件转写。
- 实测客户端直传音频的两条路径：`POST /api/v1/files` 直传配合 `GET /api/v1/files/{file_id}` 取回签名临时 URL，以及 `GET /uploads?action=getPolicy` 加对象存储表单直传；逐条记录对象访问级别、引用形态与转写服务的消费结果，并确定哪一条可作为转写输入。
- 冻结"能力矩阵"这一事实基线，并把每一项标记为可用或不可用；不可用项必须记录被证伪的引用形态与原因，避免下游重复踩坑。
- 冻结上游幂等能力结论：确认上游是否接受客户端自定义幂等键，据此确定客户端本地 `requestId` + 输入指纹去重的强度。
- 明确"调用记账只能依赖上游响应中的用量字段"这一约束，为客户端本地记账规格提供依据。
- 非目标：不实现任何客户端生产代码；不改动 `src/`、`src-tauri/`、`ui/`；不新增或修改我方服务端接口；不做图片与视频 Provider 选型（已定为万相）；不评估渲染后端。

## Capabilities

### New Capabilities

- `dashscope-direct-access`: 客户端以服务端签发的短期凭据直连阿里云百炼时的能力边界与已实测事实基线，包括四类能力的可用性、音频上传可消费路径与上游幂等能力。

### Modified Capabilities

无。

## Impact

- 证据产出：本 change 的 `design.md` 与 `docs/` 下的探针报告，记录端点、请求形态、终态与脱敏后的错误码。
- 受影响的下游 change：`wire-cloud-asr-upload-and-jobs`（音频上传与转写路径）、`add-generative-video-animation`（万相直连已确认可用）、`add-effect-caption-renderer`（渲染链路不依赖百炼，仅确认无影响）、`add-model-gateway-and-harness-sidecar`（three 条 LLM 链路改用临时 token 已确认可行）。
- 计费：本 change 会产生真实付费调用。实测合计消耗 `image_count=1`、`video_count=1`、`video_duration=5` 秒，以及可忽略的 LLM token。
- 凭据边界：临时凭据从本机激活凭据换取，仅存在于内存与受限权限的临时文件；不得进入仓库、日志或报告正文。
- 结论性影响：四类能力在客户端直连口径下均可用，**不需要任何服务端新增接口**。文件转写经实测确认须走 `POST /api/v1/files` 直传 + `GET /api/v1/files/{file_id}` 取签名临时 URL + 提交转写的三步流程；`GET /uploads?action=getPolicy` 的表单直传产物不可作为转写输入。`wire-cloud-asr-upload-and-jobs` 需据此把音频上传实现改为三步流程，并放宽客户端对引用方案只接受 `https` 的既有约束。
