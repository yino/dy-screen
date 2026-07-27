# AI ASR 需求、测试与证据追踪矩阵

本文逐项追踪已归档 OpenSpec 变更
`openspec/changes/archive/2026-07-23-add-user-triggered-ai-asr/` 的三份 delta spec。状态定义：

- **自动化通过**：仓库测试直接覆盖行为，并已由 `make check` 验证。
- **真实开发机通过**：除自动化外，使用封存 FFmpeg、VAD、Whisper 和模型执行了真实媒体。
- **部分完成**：代码、脚本和静态测试已具备，但规格要求的真实 Windows、正式签名、公证、
  8 GB 设备或授权直播证据尚未取得。

矩阵只说明证据，不代替 OpenSpec `tasks.md`。外部证据未实际生成前不得把对应任务标为完成。

## `ai-analysis-projects`

| Requirement | Scenarios | 主要自动化/真实证据 | 状态 |
| --- | --- | --- | --- |
| `仅由用户创建并启动 AI 分析项目` | `只打开 AI 剪辑页面`<br>`新录像分片完成`<br>`用户开始分析`<br>`本地 ASR 资源不可用` | `opening_workspace_recording_completion_and_startup_do_not_create_projects`；`summary_and_preflight_keep_draft_on_failure_then_freeze_on_success`；App 空状态与环境失败测试 | 自动化通过 |
| `支持导入多个本地视频` | `导入多个本地文件`<br>`导入重复文件`<br>`前端提交未授权路径` | `trusted_multi_file_import_preserves_order_deduplicates_and_never_copies_sources`；`arbitrary_grants_are_rejected_before_media_processes_and_backend_grants_are_one_time` | 自动化通过 |
| `支持选择已结束直播会话` | `选择已结束会话`<br>`选择进行中的会话`<br>`会话包含不可用分片` | `completed_session_expands_ordered_videos_and_marks_missing_segments`；`completed_live_session_expands_and_processes_an_ordered_project_timeline` | 自动化通过 |
| `在开始前管理统一输入列表` | `调整多个输入顺序`<br>`查看项目摘要`<br>`运行期间修改输入` | `draft_crud_ordering_and_state_transitions_are_strict`；`summary_and_preflight_keep_draft_on_failure_then_freeze_on_success`；AI 工作区输入测试 | 自动化通过 |
| `展示并恢复项目执行状态` | `多视频依次处理`<br>`页面错过状态事件`<br>`单个输入失败` | `external_multi_video_pipeline_handles_partial_failure_cache_and_exports`；`cancellation_and_restart_recovery_leave_retryable_state_and_clean_temporary_audio`；AI 工作区主动查询恢复测试 | 自动化通过 |
| `联动展示视频与时间戳文本` | `选择已有转写的视频`<br>`点击可定位句段`<br>`播放器经过当前句段`<br>`预览无法可靠定位` | `展示播放器与只读时间戳文本，点击句段跳转并用 WebView 元素显示临时字幕`；`播放器不可定位时仍允许复制文本，并提供 TXT 与 JSON 只读导出` | 自动化通过 |
| `支持只存在于界面的临时字幕覆盖` | `开启临时字幕`<br>`关闭字幕或切换视频` | AI 工作区播放器测试；`第一版源码不提供编辑、字幕文件、图形化时间轴或视频渲染入口` | 自动化通过 |
| `复制并导出只读转写结果` | `复制当前视频或项目全文`<br>`导出 TXT`<br>`导出 JSON` | `projection_preserves_stable_ids_video_boundaries_and_failed_input_gaps_without_paths`；外部多视频 E2E；AI 工作区 TXT/JSON 测试 | 自动化通过 |
| `第一版结果工作区保持只读` | `查看已完成项目` | `第一版源码不提供编辑、字幕文件、图形化时间轴或视频渲染入口`；状态恢复 UI 测试 | 自动化通过 |
| `支持取消、重试和删除项目` | `取消运行项目`<br>`重试失败输入`<br>`删除 AI 项目` | `freezing_persists_offsets_retry_requeues_failed_input_and_cancel_is_project_scoped`；`运行项目支持取消，部分完成项目支持失败项重试和只删除 AI 数据` | 自动化通过 |

## `desktop-client-shell`

| Requirement | Scenarios | 主要自动化/真实证据 | 状态 |
| --- | --- | --- | --- |
| `提供 AI 剪辑工作区` | `打开 AI 剪辑工作区`<br>`本地 ASR 环境未就绪`<br>`打开已有转写的项目` | App 无项目、环境不可用、草稿创建测试；AI 工作区播放器和结果测试 | 自动化通过 |
| `自适应展示播放器和转写列表` | `宽桌面窗口查看结果`<br>`窄窗口查看结果`<br>`长直播产生大量句段` | `同时提供宽屏左右布局和窄窗口上下布局，并处理长文本`；`长直播句段按 200 条分页，关闭跟随后可自由切换页面` | 自动化通过 |
| `提供播放器结果查看控件` | `用户切换跟随播放`<br>`用户开启显示字幕` | AI 工作区点击、跟随、高亮和临时字幕测试 | 自动化通过 |
| `不暴露第一版未支持的剪辑控件` | `用户查看转写结果操作` | `第一版源码不提供编辑、字幕文件、图形化时间轴或视频渲染入口` | 自动化通过 |
| `提供清晰空状态` | `首次启动没有主播`<br>`尚未创建 AI 项目` | App 首次启动主播空状态与 AI 无项目空状态测试 | 自动化通过 |
| `提供本地设置` | `设置保存成功`<br>`ASR 资源诊断失败` | repository 设置迁移/保存测试；`recording_settings_reject_unsafe_values`；`missing_resource_diagnostic_is_pathless_and_keeps_locked_default_profile_available` | 自动化通过 |
| `保持数据本地和输出脱敏` | `backend 返回直播流错误`<br>`本地 ASR 运行` | URL/错误脱敏测试；`production_settings_and_tauri_command_surface_do_not_accept_asr_executable_or_model_paths`；`sidecar_failure_is_classified_without_leaking_paths_or_stderr` | 自动化通过 |
| `预留 AI 剪辑页面`（REMOVED） | 无场景；由实际 AI 工作区替代 | App 导航和 AI 工作区测试证明占位页已移除 | 自动化通过 |

## `local-speech-transcription`

| Requirement | Scenarios | 主要自动化/真实证据 | 状态 |
| --- | --- | --- | --- |
| `只识别可验证的视频音轨` | `完整视频包含音轨`<br>`视频没有音轨`<br>`源文件在开始后变化`<br>`预览缓存缺失或被清理` | `ffprobe_inspects_original_media_with_and_without_audio`；`frozen_media_rejects_source_changes`；CLI probe 无音轨测试；processor 缓存与 UI 预览独立测试 | 真实开发机通过 |
| `使用 FFmpeg 准备短期标准音频` | `识别器支持管道输入`<br>`识别器要求文件输入`<br>`启动时发现遗留临时音频` | `ffmpeg_pcm_pipe_is_streamable_and_cancellable`；`ffmpeg_prepares_unicode_path_wav_and_cleans_it_without_touching_source`；`startup_cleanup_only_removes_inactive_asr_audio_files`；CLI RAII 清理 | 真实开发机通过 |
| `使用 VAD 筛选有效人声` | `音频包含人声和长静音`<br>`没有检测到有效人声` | `real_silero_vad_detects_speech_and_rejects_silence_and_music`；CLI vad 阶段的人声/无人声输出 | 真实开发机通过 |
| `通过中立本地 AsrEngine 执行 ASR` | `业务层提交统一请求`<br>`本地环境就绪`<br>`本地环境缺失`<br>`默认识别流程运行` | 中立契约测试；fake Adapter；资源诊断；真实 Metal Whisper；安全边界测试；CLI 完整 ASR | macOS 真实通过；Windows 目标机证据待完成 |
| `跨平台定位随包 sidecar` | `在 Apple Silicon Mac 运行`<br>`在 Windows x64 运行`<br>`在未支持平台运行` | macOS 封存资源和 Metal 测试；Windows xwin/Adapter/目标脚本；unsupported platform 资源解析测试 | 部分完成：Windows 真实目标机待完成 |
| `随安装包提供默认模型并严格校验` | `随包资源完整`<br>`随包模型缺失或损坏`<br>`设备资源不足` | `resolver_and_preflight_validate_platform_hash_permissions_memory_and_disk`；`eight_gigabyte_preflight_rejects_low_total_or_available_memory`；asr-bundle 原子暂存和包内资源验证 | 部分完成：正式双平台签名安装包待完成 |
| `保存可定位的转写片段` | `模型返回有效片段`<br>`相同产物被另一个项目复用`<br>`模型不提供可靠置信度`<br>`使用项目热词` | Whisper JSON/置信测试；`artifact_publish_is_atomic_reusable_and_uses_stable_segment_ids`；processor 缓存测试；真实热词 CLI | 自动化与真实短句通过 |
| `构建多视频项目时间轴` | `合并多个成功输入`<br>`连续直播分片出现边界重复`<br>`中间输入失败` | transcript 单元测试；`projection_preserves_stable_ids_video_boundaries_and_failed_input_gaps_without_paths`；直播会话 E2E | 自动化通过 |
| `提供可复制和导出的转写投影` | `生成当前视频文本投影`<br>`生成项目 JSON 投影`<br>`项目包含失败输入或时间轴空洞` | projection 测试；外部多视频 E2E；AI 工作区复制/TXT/JSON 测试 | 自动化通过 |
| `安全复用相同识别产物` | `相同视频再次分析`<br>`模型或源发生变化`<br>`应用在发布结果前退出` | `cache_hit_reuses_stable_segments_and_skips_all_expensive_interfaces`；`source_model_vad_hotword_changes_and_pending_artifacts_are_cache_misses`；原子发布与恢复测试 | 自动化通过 |
| `限制 ASR 并发并保护录制` | `多个项目等待识别`<br>`存在活动录像`<br>`窗口被隐藏`<br>`一个输入识别完成` | scheduler FIFO/单并发/录制门控测试；输入结束资源释放；录制无回归测试 | 自动化通过 |
| `有界取消、恢复和错误隔离` | `用户取消识别`<br>`应用异常退出后重启`<br>`单个输入处理失败` | real Whisper 运行中取消；scheduler 取消；lifecycle 恢复；部分失败 E2E | 真实开发机与自动化通过 |
| `保护原始媒体和本地隐私` | `删除转写产物`<br>`模型进程返回包含路径的错误` | repository 删除/清理测试；源哈希测试；Adapter 错误脱敏；事件和 projection 不含路径 | 自动化与真实源哈希通过 |

## OpenSpec 外部未完成门禁

以下六项当前状态：未完成。脚本、字段和静态测试只是执行入口，不能代替真实报告：

- [ ] **1.6**：真实 Windows x64 CPU、Unicode/空格路径、CLI 阶段、取消、SSE4.2 和 VC++ 证据。
- [ ] **9.2**：正式签名 Windows x64 安装资源、时间戳、运行库和包内哈希证据。
- [ ] **9.4**：Developer ID、hardened runtime、公证、stapling、Gatekeeper、DMG、`/Applications` 离线运行证据。
- [ ] **9.5**：Windows Authenticode、SmartScreen、中文用户目录、安装/卸载和离线运行证据。
- [ ] **9.8**：macOS arm64 与 Windows x64 两台真实 8 GB 设备的严格性能报告。
- [ ] **9.9**：至少 10 场具有授权引用的真实中文直播质量报告。

该变更已按当时“外部证据后补”的决策归档，归档不代表上述六项已经通过。只有六项报告均存在、
不可覆盖、哈希一致且各自 `allPassed=true`（质量报告按定义通过）后，才能把外部验收状态从
`77/83` 更新为 `83/83`。在此之前，发行说明和 Wiki 必须继续明确标为“外部门禁未完成”。
