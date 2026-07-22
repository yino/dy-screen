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
        let result = self
            .process_input_inner(project_id, input_id, cancellation)
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
                let _ = self.repository.transition_input(
                    input_id,
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
        cancellation: CancellationToken,
    ) -> Result<AiProcessOutcome, AiProcessorError> {
        let detail = self.repository.get_project(project_id)?;
        if detail.project.status != AiProjectStatus::Running {
            return Err(
                AiRepositoryError::InvalidState("只有运行中的项目可以处理输入".to_owned()).into(),
            );
        }
        let input = self.repository.get_input(input_id)?;
        if input.project_id != project_id || input.status != AiInputStatus::Pending {
            return Err(
                AiRepositoryError::InvalidState("项目输入不处于可执行状态".to_owned()).into(),
            );
        }
        self.repository
            .transition_input(input_id, AiInputStatus::Validating, None)?;
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
            self.repository
                .transition_input(input_id, AiInputStatus::Completed, None)?;
            self.publish(
                project_id,
                input_id,
                "completed",
                100,
                "已复用相同视频的本地转写结果",
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
        self.repository
            .transition_input(input_id, AiInputStatus::PreparingAudio, None)?;
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

        self.repository
            .transition_input(input_id, AiInputStatus::DetectingSpeech, None)?;
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
            self.repository
                .transition_input(input_id, AiInputStatus::Skipped, None)?;
            self.publish(project_id, input_id, "skipped", 100, "没有检测到有效人声");
            return Ok(AiProcessOutcome::SkippedNoSpeech);
        }

        let profile = &detail.project.recognition_profile;
        let identity = self.engine.identity();
        if identity.engine_id != profile.engine_id
            || identity.engine_version != profile.engine_version
            || identity.model_id != profile.model_id
            || identity.model_version != profile.model_version
        {
            return Err(AsrError::new(
                AsrErrorKind::EnvironmentUnavailable,
                "recognition_profile_mismatch",
                "当前识别引擎与项目冻结配置不一致",
                false,
            )
            .into());
        }
        self.repository
            .transition_input(input_id, AiInputStatus::Transcribing, None)?;
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
                    speech_regions,
                    timestamp_policy: TimestampPolicy::Segment,
                    max_threads: self.engine.capabilities().maximum_threads,
                },
                progress,
                cancellation,
            )
            .await?;
        if result.segments.is_empty() {
            self.repository
                .transition_input(input_id, AiInputStatus::Skipped, None)?;
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
        self.repository
            .transition_input(input_id, AiInputStatus::Completed, None)?;
        self.publish(project_id, input_id, "completed", 100, "语音识别完成");
        Ok(AiProcessOutcome::Transcribed)
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
