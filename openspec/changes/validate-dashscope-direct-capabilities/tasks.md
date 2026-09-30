> 说明：本 change 的探针已在 2026-09-30 执行完毕，因此 1–5 组任务已勾选并附实测证据；6 组是结论回写任务，需要在下游 change 中完成。

## 1. 凭据与能力矩阵

- [x] 1.1 从本机激活凭据换取短期凭据，验证 `provider` 为 `ALI`、`ak` 形如 `st-lt-v1.*`、有效期字段可用，且凭据未写入仓库、日志或报告（以 `git status` 无凭据文件、报告正文不含凭据值为验证）
- [x] 1.2 实测通义兼容模式对话：`POST /compatible-mode/v1/chat/completions`，模型 `qwen-plus`，验证返回 `http=200`、回复正常且 `usage` 含 token 用量
- [x] 1.3 实测万相文生图：`POST /api/v1/services/aigc/text2image/image-synthesis`，模型 `wanx2.1-t2i-turbo`，轮询至终态并验证 `SUCCEEDED` 且 `usage` 含 `image_count`
- [x] 1.4 实测万相图生视频：`POST /api/v1/services/aigc/video-generation/video-synthesis`，模型 `wanx2.1-i2v-turbo`，轮询至终态并验证 `SUCCEEDED` 且 `usage` 含 `video_count` 与 `video_duration`
- [x] 1.5 实测文件转写：`POST /api/v1/services/audio/asr/transcription`，模型 `qwen3-asr-flash-filetrans`，提交并轮询终态，验证失败时不把结果伪装为成功

## 2. 音频上传可消费性

- [x] 2.1 请求上传策略 `GET /api/v1/uploads?action=getPolicy&model=qwen3-asr-flash-filetrans`，验证响应含 `upload_host`、`upload_dir`、`policy`、`signature`、`oss_access_key_id` 与有效期字段
- [x] 2.2 按官方表单字段完成对象存储直传，验证上传返回 `http=200`；同时验证以 `public-read` 上传会被上游策略拒绝，据此记录"对象访问级别由上游策略固定为私有"
- [x] 2.3 以对象存储 `oss://<key>` 引用提交转写，验证终态为 `FILE_DOWNLOAD_FAILED`
- [x] 2.4 以对象存储 HTTPS 直链引用提交转写，验证终态为 `FILE_403_FORBIDDEN`
- [x] 2.5 核对官方 SDK 源码中对象存储上传的键构造与引用形态，确认官方 `oss://` 上传用于当前工作空间内部服务消费，不是为外部拉取的转写服务提供输入；据此判定 `getPolicy` 表单路径不可作为转写输入
- [x] 2.6 实测 `POST /api/v1/files` 直传本地标准音频，验证返回 `file_id` 与 `http=200`
- [x] 2.7 实测 `GET /api/v1/files/{file_id}`，验证响应 `data.url` 为带 STS `security-token`、`Expires` 与 `Signature` 的 OSS 签名临时 URL，并记录其为 `http` 方案、长度约 938 字符
- [x] 2.8 以该临时 URL 作为 `input.file_url` 提交转写，验证终态为 `SUCCEEDED` 且 `usage` 含 `seconds`
- [x] 2.9 记录被证伪的中间结论：直接以 `file_id` 提交得 `FILE_DOWNLOAD_FAILED`（缺中间取 URL 步骤）；以 `file://<file_id>` 提交得 `DECODE_ERROR`（表示已读取但内容不可解码，不可据此判定引用形态不可用）
- [x] 2.10 汇总结论：转写须走三步直传流程，云端 ASR 可实施且不需要服务端改动；客户端 `TemporaryAudioReference::validate()` 只接受 `https` 的既有约束须针对上游签发临时 URL 放宽，且放宽不得波及用户可输入地址

## 3. 幂等与记账依据

- [x] 3.1 以相同输入与相同自定义幂等键连续提交两次异步任务，验证返回两个不同 `task_id`，据此判定上游不具备调用方幂等能力
- [x] 3.2 验证响应头中不存在幂等相关字段，只有通用请求标识与网关标识
- [x] 3.3 验证三类成功调用的用量字段均出现在响应体 `usage` 中，且请求体中不存在可被采信的价格或额度字段

## 4. 证据与结论冻结

- [x] 4.1 在 `design.md` 中记录四类能力的终态、脱敏错误码与用量字段，并记录取得日期与端点，验证报告正文不含凭据值、完整签名 URL 与上游任务 ID
- [x] 4.2 在 `specs/dashscope-direct-access/spec.md` 中冻结能力矩阵、音频可消费性与无上游幂等键时的去重与记账三项行为契约
- [x] 4.3 核对计费用量合计（文生图 `image_count=1`、图生视频 `video_count=1` 与 `video_duration=5`、两次转写各 `seconds=4`、LLM 15 token），验证未产生重复付费提交

## 5. 变更校验

- [x] 5.1 运行 `openspec validate validate-dashscope-direct-capabilities --strict`，验证通过
- [x] 5.2 验证本 change 未修改 `src/`、`src-tauri/`、`ui/` 与 `openspec/specs/` 下任何文件（以 `git status --short` 核对）

## 6. 结论回写下游 change

- [x] 6.1 在 `wire-cloud-asr-upload-and-jobs` 中把音频上传实现改为三步直传流程（`POST /api/v1/files` → `GET /api/v1/files/{file_id}` → 提交转写），并把回退决策改写为本轮不做本地回退，验证该 change 不再引用 `getPolicy` 表单路径作为转写输入
- [x] 6.2 在 `add-generative-video-animation` 中把图片与视频生成来源确认改为万相直连，并引用本 change 的实测用量字段作为记账依据，验证该 change 不再引用服务端 `image-jobs` 与 `video-jobs`
- [x] 6.3 在 `add-model-gateway-and-harness-sidecar` 中把三条 LLM 链路的凭据来源改为现有短期凭据，验证该 change 不再引用服务端任务令牌与网关接口
- [x] 6.4 在 `docs/v2-delivery-order.md` 中把本 change 的结论登记为第 0 步的完成证据，验证实施顺序中 ASR 一步写明三步直传流程与引用方案约束

## 7. 本地帧可达性与生成规格特征

- [x] 7.1 从仓库内真实录制抽帧（`ffmpeg -ss 30 -frames:v 1`，720x1280 JPEG），验证"本地文件 → 上游可读引用"在图片路径同样成立
- [x] 7.2 实测 `oss://<key>` 作为图生视频 `img_url` 被拒（`InvalidParameter.DataInspection`），确认该引用形态对图片路径同样不可用
- [x] 7.3 实测 `POST /api/v1/files` + `GET /api/v1/files/{file_id}` 取回的签名临时 URL 作为 `img_url` 被接受并进入 `PENDING`，确认本地帧走与音频同构的三步直传
- [x] 7.4 五组对照实测产出方向与分辨率：横屏输入得 `832x464`、竖屏输入得 `464x832`，方向跟随输入；实际时长恒为 `5.367` 秒（请求 5 秒，+7.3%，超过 5% 容差）
- [x] 7.5 复测失败形态的可复现性：两个同为 `720x1280` 的真竖屏帧一败一成，确认 `AlgoError` 为偶发上游内部缺陷而非规格约束，并据此改写下游的失败与重试要求
- [x] 7.6 记录文生图与图生视频均返回 `actual_prompt` 且与输入显著不同，作为分镜提示词设计与结果复核的依据
- [x] 7.7 把结论写入 `local-frame-accessibility.md`、`docs/review/README.md` 与 `add-generative-video-animation` 的 design，并记录竖屏成功产出的实测属性（464x832、5.367 秒；媒体不入库）
