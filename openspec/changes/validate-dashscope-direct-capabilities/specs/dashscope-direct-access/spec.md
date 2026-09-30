## Purpose

定义客户端以服务端签发的短期凭据直连阿里云百炼时的能力边界与已实测事实基线，使下游 change 的实施前置条件建立在可复现的实测结论上，而不是假设。

## ADDED Requirements

### Requirement: 以短期凭据直连百炼的能力矩阵

系统 SHALL 以服务端 `GET /api/client/ai/llm-ak` 签发的短期凭据（`provider` 为 `ALI`）直连阿里云百炼，并 SHALL 在实施前对下列四类能力取得可复现的实测结论：通义兼容模式对话、万相文生图、万相图生视频、百炼文件转写。每项结论 MUST 记录终态、脱敏错误码与上游返回的用量字段。凭据 MUST NOT 落入仓库、普通日志、诊断事件或报告正文。

#### Scenario: 直连通义兼容模式对话

- **WHEN** 使用短期凭据向 `/compatible-mode/v1/chat/completions` 发起一次非流式对话请求
- **THEN** 上游返回成功响应，响应中包含 `usage` 的 token 用量字段

#### Scenario: 直连万相文生图

- **WHEN** 使用短期凭据向 `/api/v1/services/aigc/text2image/image-synthesis` 提交一次异步文生图任务并轮询终态
- **THEN** 任务到达成功终态，且响应中包含图片计费用量字段

#### Scenario: 直连万相图生视频

- **WHEN** 使用短期凭据向 `/api/v1/services/aigc/video-generation/video-synthesis` 提交一次异步图生视频任务并轮询终态
- **THEN** 任务到达成功终态，且响应中包含视频条数与时长计费用量字段

#### Scenario: 直连百炼文件转写取得结论

- **WHEN** 使用短期凭据向 `/api/v1/services/audio/asr/transcription` 提交一次异步文件转写任务并轮询终态
- **THEN** 系统记录该次调用的真实终态；若失败，MUST 记录脱敏错误码且 MUST NOT 把失败伪装为成功

### Requirement: 客户端直传音频的三步流程

系统 SHALL 以三步流程把本地音频交给转写服务：`POST /api/v1/files` 直传取得 `file_id`；`GET /api/v1/files/{file_id}` 取回上游签发的签名临时 URL；再以该 URL 作为转写请求的音频引用。系统 MUST NOT 把 `GET /uploads?action=getPolicy` 的表单直传产物用作转写输入。系统 SHALL 在提交前取回临时 URL，MUST NOT 长期缓存该 URL。

#### Scenario: 三步流程取得转写结果

- **WHEN** 系统直传一个标准音频文件、取回其签名临时 URL，并以该 URL 提交转写任务
- **THEN** 任务到达成功终态，响应用量包含音频秒数

#### Scenario: 引用形态缺少取回步骤

- **WHEN** 系统跳过取回签名临时 URL 的步骤，直接以 `file_id` 作为音频引用提交转写
- **THEN** 任务失败并返回文件不可下载类错误码；系统 MUST NOT 据此判定转写能力不可用

#### Scenario: 表单直传产物不可作为转写输入

- **WHEN** 系统以 `getPolicy` 表单直传得到的对象键或直链作为音频引用提交转写
- **THEN** 任务失败并返回文件不可下载或禁止访问类错误码；系统 MUST 改用三步流程

### Requirement: 临时音频引用的方案约束

系统 SHALL 允许上游签发的签名临时音频 URL 作为合法引用，并 SHALL 通过"由上游签发且含签名与有效期参数"判定其合法性。该放宽 MUST NOT 使任何用户可输入的地址获得通过。

#### Scenario: 上游签发的临时地址被接受

- **WHEN** 音频引用为上游在文件详情响应中签发的临时 URL
- **THEN** 系统接受该引用并发起转写，不因其为 `http` 方案而拒绝

#### Scenario: 用户可输入的任意地址被拒绝

- **WHEN** 音频引用来自用户输入、配置或非上游签发来源
- **THEN** 系统拒绝该引用且不发起任何付费调用

### Requirement: 无上游幂等键时的去重与记账

上游异步任务接口 SHALL 被确认是否接受调用方自定义的幂等键。当确认上游不接受自定义幂等键时，系统 MUST 以客户端本地持久化的 `requestId` 与输入指纹承担去重责任，MUST NOT 依赖重复提交由上游复用任务，并 MUST NOT 在未能确认上游是否已受理时自动重复提交付费请求。

#### Scenario: 重复提交不会由上游复用

- **WHEN** 以相同输入与相同自定义幂等键向上游连续提交两次异步任务
- **THEN** 上游返回两个不同的任务标识，系统据此判定上游不具备调用方幂等能力

#### Scenario: 记账依据来自上游响应

- **WHEN** 任一付费调用到达终态
- **THEN** 系统只以上游响应中的用量字段作为记账依据，MUST NOT 采用请求体中的价格或剩余额度
