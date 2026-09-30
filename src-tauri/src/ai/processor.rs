//! 单输入 ASR 流水线：缓存、媒体探测、音频准备、VAD、识别和原子产物发布。
//!
//! 各昂贵阶段均通过 trait 注入，使业务链路可以使用假实现独立测试。

use std::path::PathBuf;
use std::sync::Arc;

use dy_screen::asr::{
    AsrEngine, AsrError, AsrErrorKind, AsrProgressEvent, AsrProgressSink, AsrRequest,
    AudioPreparationRequest, FrozenMediaSource, MediaAudioPreparer, MediaInspector, TextNormalizer,
    TimestampPolicy, VadConfig, VadEngine,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use super::{
    AiInputStatus, AiProjectStatus, AiRepository, AiRepositoryError, NewAsrArtifact,
    SourceFingerprint, TranscriptSegmentDraft,
};

/// 本地识别引擎在识别配置中的稳定标识；不等于该值时走默认的百炼直连路径。
pub const LOCAL_ENGINE_ID: &str = "whisper.cpp";

/// 在开始任何昂贵阶段之前判断所选识别路径是否可用。
///
/// 云端路径不可用时必须直接失败并保留任务状态：MUST NOT 启动 FFmpeg、VAD 或 ASR，
/// 也 MUST NOT 静默改用本地识别。本地路径由用户显式选择，其资源检查由调度器负责。
pub fn ensure_path_available(
    cloud_path: bool,
    cloud_ready: bool,
) -> dy_screen::asr::EngineResult<()> {
    if cloud_path && !cloud_ready {
        return Err(AsrError::new(
            AsrErrorKind::EnvironmentUnavailable,
            "no_asr_path_available",
            "云端识别当前不可用，且未显式选择本地识别；请先完成授权或修复本地资源后重试",
            true,
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AiProcessOutcome {
    CacheHit,
    Transcribed,
    SkippedNoSpeech,
}

/// 可恢复的 AI 状态事件。事件只包含稳定 ID、阶段和中文说明，不包含媒体路径。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AiJobEvent {
    pub project_id: i64,
    pub input_id: i64,
    pub project_status: AiProjectStatus,
    pub project_progress_percent: u8,
    pub input_status: AiInputStatus,
    pub input_progress_percent: u8,
    pub stage: String,
    pub message: String,
}

pub trait AiJobPublisher: Send + Sync {
    fn publish(&self, event: AiJobEvent);
}

#[derive(Debug, Error)]
pub enum AiProcessorError {
    #[error(transparent)]
    Repository(#[from] AiRepositoryError),
    #[error(transparent)]
    Asr(#[from] AsrError),
}

/// 单输入处理器通过 trait 组合媒体、VAD 和 ASR，缓存命中时不会调用这些昂贵阶段。
#[derive(Clone)]
pub struct AiInputProcessor {
    repository: AiRepository,
    inspector: Arc<dyn MediaInspector>,
    preparer: Arc<dyn MediaAudioPreparer>,
    vad: Arc<dyn VadEngine>,
    engine: Arc<dyn AsrEngine>,
    normalizer: TextNormalizer,
    vad_config: VadConfig,
    temporary_root: PathBuf,
    publisher: Arc<dyn AiJobPublisher>,
}

impl AiInputProcessor {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        repository: AiRepository,
        inspector: Arc<dyn MediaInspector>,
        preparer: Arc<dyn MediaAudioPreparer>,
        vad: Arc<dyn VadEngine>,
        engine: Arc<dyn AsrEngine>,
        normalizer: TextNormalizer,
        vad_config: VadConfig,
        temporary_root: PathBuf,
        publisher: Arc<dyn AiJobPublisher>,
    ) -> Self {
        Self {
            repository,
            inspector,
            preparer,
            vad,
            engine,
            normalizer,
            vad_config,
            temporary_root,
            publisher,
        }
    }

    pub async fn process_input(
        &self,
        project_id: i64,
        input_id: i64,
        cancellation: CancellationToken,
    ) -> Result<AiProcessOutcome, AiProcessorError> {
        let generation = self.repository.get_input(input_id)?.scheduler_generation;
        self.process_input_with_generation(project_id, input_id, generation, cancellation)
            .await
    }

    pub async fn process_input_with_generation(
        &self,
        project_id: i64,
        input_id: i64,
        generation: u64,
        cancellation: CancellationToken,
    ) -> Result<AiProcessOutcome, AiProcessorError> {
        let result = self
            .process_input_inner(project_id, input_id, generation, cancellation)
            .await;
        if let Err(error) = &result {
            let (code, message) = match error {
                AiProcessorError::Asr(error) => (error.code.as_str(), error.safe_message.as_str()),
                AiProcessorError::Repository(_) => {
                    ("ai_repository_error", "无法保存语音识别任务状态")
                }
            };
            if let Ok(input) = self.repository.get_input(input_id)
                && !matches!(
                    input.status,
                    AiInputStatus::Completed
                        | AiInputStatus::Skipped
                        | AiInputStatus::Cancelled
                        | AiInputStatus::Failed
                )
            {
                let _ = self.repository.accept_scheduler_event(
                    input_id,
                    generation,
                    if code == "asr_cancelled" {
                        AiInputStatus::Cancelled
                    } else {
                        AiInputStatus::Failed
                    },
                    Some((code, message)),
                );
            }
            self.publish(project_id, input_id, "failed", 100, message);
        }
        let _ = self.repository.recompute_project_progress(project_id);
        result
    }

    async fn process_input_inner(
        &self,
        project_id: i64,
        input_id: i64,
        generation: u64,
        cancellation: CancellationToken,
    ) -> Result<AiProcessOutcome, AiProcessorError> {
        let detail = self.repository.get_project(project_id)?;
        if detail.project.status != AiProjectStatus::Running {
            return Err(
                AiRepositoryError::InvalidState("只有运行中的项目可以处理输入".to_owned()).into(),
            );
        }
        let input = self.repository.get_input(input_id)?;
        if input.project_id != project_id
            || input.status != AiInputStatus::Pending
            || input.scheduler_generation != generation
        {
            return Err(
                AiRepositoryError::InvalidState("项目输入不处于可执行状态".to_owned()).into(),
            );
        }
        self.transition_generation(input_id, generation, AiInputStatus::Validating, None)?;
        self.publish(project_id, input_id, "validating", 10, "正在验证原始视频");
        if cancellation.is_cancelled() {
            return Err(AsrError::cancelled().into());
        }

        let frozen = FrozenMediaSource::from_path(PathBuf::from(&input.source_path).as_path())?;
        let current_source = SourceFingerprint {
            normalized_path: frozen.path.to_string_lossy().into_owned(),
            size_bytes: frozen.size_bytes,
            modified_at_ms: i64::try_from(frozen.modified_at_ms).map_err(|_| {
                AsrError::new(
                    AsrErrorKind::InvalidInput,
                    "media_timestamp_unavailable",
                    "视频修改时间超出支持范围",
                    false,
                )
            })?,
            video_id: input.video_id,
        };
        if current_source != input.source_fingerprint {
            return Err(AsrError::new(
                AsrErrorKind::InvalidInput,
                "media_changed",
                "视频文件在项目开始后发生变化",
                false,
            )
            .into());
        }

        if let Some(artifact) = self
            .repository
            .find_published_artifact(&current_source, &detail.project.recognition_profile_hash)?
        {
            self.repository.attach_artifact(input_id, artifact.id)?;
            self.transition_generation(input_id, generation, AiInputStatus::Completed, None)?;
            self.publish(
                project_id,
                input_id,
                "completed",
                100,
                "已复用相同视频的识别结果",
            );
            return Ok(AiProcessOutcome::CacheHit);
        }

        let inspection = self
            .inspector
            .inspect(&frozen, cancellation.child_token())
            .await?;
        if !inspection.audio_present {
            return Err(AsrError::new(
                AsrErrorKind::InvalidInput,
                "no_audio_track",
                "视频没有音轨，无法进行语音识别",
                false,
            )
            .into());
        }
        // 默认路径是百炼直连；只有工程识别配置显式指向本地引擎时才在本地执行。
        // 该判定必须早于任何昂贵阶段：两条路径都不可用时不得启动 FFmpeg、VAD 或 ASR。
        let profile = &detail.project.recognition_profile;
        let cloud_path = profile.engine_id != LOCAL_ENGINE_ID;
        if cloud_path {
            let report = self.engine.diagnose().await?;
            ensure_path_available(cloud_path, report.ready)?;
        }
        self.transition_generation(input_id, generation, AiInputStatus::PreparingAudio, None)?;
        self.publish(
            project_id,
            input_id,
            "preparing_audio",
            25,
            "正在准备标准音频",
        );
        let preparation = AudioPreparationRequest {
            task_id: format!("project-{project_id}-input-{input_id}"),
            source: frozen,
            duration_ms: inspection.duration_ms,
            temporary_root: self
                .temporary_root
                .join(project_id.to_string())
                .join(input_id.to_string()),
        };
        let audio = self
            .preparer
            .prepare_temporary_wav(&preparation, cancellation.child_token())
            .await?;

        let mut speech_regions_for_request = Vec::new();
        if cloud_path {
            // 云端路径处理完整来源音频：VAD 不是前置门禁，其缺失、诊断失败或
            // 返回空区间都不得阻止云端识别。
            self.publish(
                project_id,
                input_id,
                "preparing_audio",
                45,
                "正在准备云端识别所需的完整音频",
            );
        } else {
            self.transition_generation(input_id, generation, AiInputStatus::DetectingSpeech, None)?;
            self.publish(
                project_id,
                input_id,
                "detecting_speech",
                45,
                "正在检测有效人声",
            );
            let speech_regions = self
                .vad
                .detect(audio.audio(), &self.vad_config, cancellation.child_token())
                .await?;
            if speech_regions.is_empty() {
                self.transition_generation(input_id, generation, AiInputStatus::Skipped, None)?;
                self.publish(project_id, input_id, "skipped", 100, "没有检测到有效人声");
                return Ok(AiProcessOutcome::SkippedNoSpeech);
            }
            speech_regions_for_request = speech_regions;
        }

        self.transition_generation(input_id, generation, AiInputStatus::Transcribing, None)?;
        self.publish(project_id, input_id, "transcribing", 70, "正在识别语音");
        let progress: Arc<dyn AsrProgressSink> = Arc::new(ProcessorProgressSink {
            project_id,
            input_id,
            repository: self.repository.clone(),
            publisher: self.publisher.clone(),
        });
        let result = self
            .engine
            .transcribe(
                AsrRequest {
                    request_id: format!("project-{project_id}-input-{input_id}"),
                    audio: audio.audio().clone(),
                    language_hint: profile.language_hint.clone(),
                    hotwords: profile.hotwords.clone(),
                    speech_regions: speech_regions_for_request,
                    timestamp_policy: TimestampPolicy::Segment,
                    max_threads: self.engine.capabilities().maximum_threads,
                    engine_id: Some(profile.engine_id.clone()),
                },
                progress,
                cancellation,
            )
            .await?;
        // 识别配置身份校验必须比对**实际执行引擎**的身份：云端结果不得携带它
        // 委托的本地引擎身份，否则缓存键与诊断无法区分两条路径。
        if result.identity.engine_id != profile.engine_id
            || result.identity.engine_version != profile.engine_version
            || result.identity.model_id != profile.model_id
            || result.identity.model_version != profile.model_version
        {
            return Err(AsrError::new(
                AsrErrorKind::EnvironmentUnavailable,
                "recognition_profile_mismatch",
                "实际执行引擎与项目冻结配置不一致",
                false,
            )
            .into());
        }
        if result.segments.is_empty() {
            self.transition_generation(input_id, generation, AiInputStatus::Skipped, None)?;
            self.publish(project_id, input_id, "skipped", 100, "识别结果没有有效文本");
            return Ok(AiProcessOutcome::SkippedNoSpeech);
        }

        let artifact = self.repository.create_artifact(NewAsrArtifact {
            source_fingerprint: current_source,
            recognition_profile_hash: detail.project.recognition_profile_hash,
            engine_id: result.identity.engine_id,
            engine_version: result.identity.engine_version,
            model_id: result.identity.model_id,
            model_version: result.identity.model_version,
            provider: result.provider.clone(),
            fallback_reason: result.fallback_reason.as_deref().map(safe_fallback_reason),
        })?;
        let drafts: Vec<TranscriptSegmentDraft> = result
            .segments
            .into_iter()
            .map(|segment| TranscriptSegmentDraft {
                source_start_ms: segment.start_ms,
                source_end_ms: segment.end_ms,
                normalized_text: self.normalizer.normalize(&segment.text),
                raw_text: segment.text,
                confidence: segment.confidence,
            })
            .collect();
        self.repository.publish_artifact(
            artifact.id,
            result.audio_duration_ms,
            result.detected_language.as_deref(),
            &drafts,
        )?;
        self.repository.attach_artifact(input_id, artifact.id)?;
        self.transition_generation(input_id, generation, AiInputStatus::Completed, None)?;
        self.publish(project_id, input_id, "completed", 100, "语音识别完成");
        Ok(AiProcessOutcome::Transcribed)
    }

    fn transition_generation(
        &self,
        input_id: i64,
        generation: u64,
        next: AiInputStatus,
        error: Option<(&str, &str)>,
    ) -> Result<(), AiProcessorError> {
        if !self
            .repository
            .accept_scheduler_event(input_id, generation, next, error)?
        {
            return Err(AiRepositoryError::InvalidState(
                "ASR 任务代次已变化，忽略旧任务".to_owned(),
            )
            .into());
        }
        Ok(())
    }

    fn publish(&self, project_id: i64, input_id: i64, stage: &str, progress: u8, message: &str) {
        if let Some(event) = build_job_event(
            &self.repository,
            project_id,
            input_id,
            stage,
            progress,
            message,
        ) {
            self.publisher.publish(event);
        }
    }
}

struct ProcessorProgressSink {
    project_id: i64,
    input_id: i64,
    repository: AiRepository,
    publisher: Arc<dyn AiJobPublisher>,
}

impl AsrProgressSink for ProcessorProgressSink {
    fn publish(&self, event: AsrProgressEvent) {
        let model_progress = event.completed_units.min(100) as u8;
        // ASR 是输入阶段的后半段；把模型内部 0..100 映射到 75..99，终态仍由数据库状态给出 100。
        let input_progress = 75_u8.saturating_add(model_progress / 4).min(99);
        if let Some(snapshot) = build_job_event(
            &self.repository,
            self.project_id,
            self.input_id,
            &format!("asr_{:?}", event.stage).to_lowercase(),
            input_progress,
            &event.message,
        ) {
            self.publisher.publish(snapshot);
        }
    }
}

pub(crate) fn safe_fallback_reason(code: &str) -> String {
    match code {
        "temporary_audio_publisher_unavailable" => "临时音频发布能力未配置".to_owned(),
        "ai_authorization_unavailable" => "当前没有有效的 AI 授权".to_owned(),
        "ai_token_ai_quota" => "AI 转写额度不可用".to_owned(),
        "ai_token_ai_token_service" => "AI 临时密钥服务暂时不可用".to_owned(),
        "dashscope_unauthorized" => "云端转写授权已失效".to_owned(),
        "dashscope_forbidden" => "云端转写权限不足".to_owned(),
        "dashscope_rate_limited" => "云端转写请求受到限流".to_owned(),
        "dashscope_service_unavailable" => "云端转写服务暂时不可用".to_owned(),
        "dashscope_poll_timeout" => "云端转写等待超时".to_owned(),
        _ => "云端转写不可用".to_owned(),
    }
}

pub(crate) fn build_job_event(
    repository: &AiRepository,
    project_id: i64,
    input_id: i64,
    stage: &str,
    input_progress_percent: u8,
    message: &str,
) -> Option<AiJobEvent> {
    let project = repository.recompute_project_progress(project_id).ok()?;
    let detail = repository.get_project(project_id).ok()?;
    let input = detail.inputs.iter().find(|input| input.id == input_id)?;
    let persisted_input_progress = input.progress_percent;
    let effective_input_progress = input_progress_percent
        .max(persisted_input_progress)
        .min(100);
    let persisted_total: u64 = detail
        .inputs
        .iter()
        .map(|candidate| {
            if candidate.id == input_id {
                u64::from(effective_input_progress)
            } else {
                u64::from(candidate.progress_percent)
            }
        })
        .sum();
    let project_progress_percent = if detail.inputs.is_empty() {
        project.progress_percent
    } else {
        u8::try_from(persisted_total / detail.inputs.len() as u64).unwrap_or(100)
    };
    Some(AiJobEvent {
        project_id,
        input_id,
        project_status: project.status,
        project_progress_percent,
        input_status: input.status,
        input_progress_percent: effective_input_progress,
        stage: stage.to_owned(),
        message: message.to_owned(),
    })
}

#[cfg(test)]
mod path_availability_tests {
    use super::*;

    /// 任务 3.4：两条路径都不可用时必须保留任务状态并引导资源准备。
    #[test]
    fn cloud_unavailable_blocks_before_expensive_stages() {
        let error = ensure_path_available(true, false).expect_err("必须拒绝");
        assert_eq!(error.code, "no_asr_path_available");
        assert_eq!(error.kind, AsrErrorKind::EnvironmentUnavailable);
        assert!(error.retryable, "修复授权或资源后应可重试");
        // 提示必须引导用户处理授权或资源，且不得暗示会改用本地识别。
        assert!(error.safe_message.contains("授权") || error.safe_message.contains("资源"));
        assert!(!error.safe_message.contains("自动"), "不得暗示自动回退");
    }

    /// 云端可用时必须放行；显式选择本地路径时不由此判定拦截。
    #[test]
    fn available_cloud_or_explicit_local_is_allowed() {
        assert!(ensure_path_available(true, true).is_ok());
        assert!(ensure_path_available(false, false).is_ok(), "本地路径不由此判定");
        assert!(ensure_path_available(false, true).is_ok());
    }
}
