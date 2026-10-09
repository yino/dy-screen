//! 转写层文案纠正（v32）——支撑「片段级前置」流程。
//!
//! 目标流程是「识别 → 提取精彩 → 每片段特效生成 → **校正文案** → 最后拼合辑」，
//! 即校正发生在**拼合辑之前**。本模块把校正作用在**转写句段**上，结果写入
//! `transcript_segment_corrections`；之后由 `seed_clip_subtitle_snapshots` 在生成
//! 合辑字幕时取纠正后的文本（见 `repository.rs` 的 v32 说明）。
//!
//! 与合辑字幕纠正**共用同一个 Provider 与提示词格式**（输入只是 `index + text`），
//! 差别只在目标层，因此这里不复制模型调用逻辑。
//!
//! **不改写 `transcript_segments`**：转写是 ASR 的原始产物，纠正单独成层。

use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use super::clip_text_correction::ClipTextCorrectionError;
use super::llm::{SubtitleCorrectionProvider, SubtitleCorrectionRequest};
use super::{AiRepository, AiTranscriptProjection, ModelCallRuntime};

/// 与合辑字幕纠正一致的批大小上限（按字符）。
const MAX_BATCH_CHARS: usize = 6_000;
/// 单批最大尝试次数（含首次）。
const MAX_BATCH_ATTEMPTS: usize = 3;
/// 重试前的退避基数（毫秒）：第 n 次重试等待 `n * 基数`。
const BATCH_RETRY_BACKOFF_MS: u64 = 1_500;

/// 可重试的上游失败：网络/限流/服务波动。
fn retryable(error: &super::LlmError) -> bool {
    matches!(
        error,
        super::LlmError::Temporary
            | super::LlmError::Provider
            | super::LlmError::InvalidResponse
    )
}

/// 一条待纠正的转写句段。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptCorrectionTarget {
    pub segment_id: String,
    pub text: String,
}

/// 转写纠正的结果摘要。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TranscriptCorrectionSummary {
    /// 进入纠正的句段数。
    pub processed: usize,
    /// 文本确实被改变的句段数。
    pub changed: usize,
    /// 文本未变的句段数。
    pub unchanged: usize,
    pub total_batches: usize,
    pub token_usage: u64,
}

/// 转写层文案纠正流程。
pub struct TranscriptTextCorrectionWorkflow {
    repository: AiRepository,
    provider: Arc<dyn SubtitleCorrectionProvider>,
    model_call: Arc<ModelCallRuntime>,
}

impl TranscriptTextCorrectionWorkflow {
    pub fn new(
        repository: AiRepository,
        provider: Arc<dyn SubtitleCorrectionProvider>,
        model_call: Arc<ModelCallRuntime>,
    ) -> Self {
        Self {
            repository,
            provider,
            model_call,
        }
    }

    /// 纠正指定工程的**全部转写句段**。
    ///
    /// 只处理非空白文本；结果按"有变化才写"的原则落库，未变化的句段不会写入纠正层。
    pub async fn correct_project(
        &self,
        project_id: i64,
        cancellation: CancellationToken,
    ) -> Result<TranscriptCorrectionSummary, ClipTextCorrectionError> {
        let projection = AiTranscriptProjection::load(&self.repository, project_id).map_err(
            |error| match error {
                super::AiProjectionError::Repository(inner) => {
                    ClipTextCorrectionError::Repository(inner)
                }
                _ => ClipTextCorrectionError::InvalidResponse,
            },
        )?;
        let mut targets = Vec::new();
        for input in &projection.inputs {
            for segment in &input.segments {
                let text = segment.normalized_text.trim();
                if text.is_empty() {
                    continue;
                }
                targets.push(TranscriptCorrectionTarget {
                    segment_id: segment.stable_segment_id.clone(),
                    text: text.to_owned(),
                });
            }
        }
        if targets.is_empty() {
            return Err(ClipTextCorrectionError::NoEligibleSubtitles);
        }

        let frozen = self.model_call.freeze_for_run()?;
        let batches = batch_targets(&targets, MAX_BATCH_CHARS);
        let mut summary = TranscriptCorrectionSummary {
            total_batches: batches.len(),
            processed: targets.len(),
            ..Default::default()
        };

        for batch in &batches {
            if cancellation.is_cancelled() {
                return Err(super::LlmError::Cancelled.into());
            }
            let prompt = build_prompt(batch);
            // 批次级重试：与精彩分析的批次策略一致。上游波动是瞬时的，
            // 只试一次会让整批纠正丢失（表现为流程失败）。
            let mut attempt = 0_usize;
            let output = loop {
                let handle = self.current_handle(&frozen).await?;
                match self
                    .provider
                    .correct_subtitles(
                        &handle,
                        SubtitleCorrectionRequest {
                            prompt: prompt.clone(),
                        },
                        cancellation.clone(),
                    )
                    .await
                {
                    Ok(output) => break output,
                    Err(error) if attempt + 1 < MAX_BATCH_ATTEMPTS && retryable(&error) => {
                        let backoff_ms = BATCH_RETRY_BACKOFF_MS * (attempt as u64 + 1);
                        tokio::select! {
                            _ = cancellation.cancelled() => {
                                return Err(ClipTextCorrectionError::from(super::LlmError::Cancelled));
                            }
                            _ = tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)) => {}
                        }
                        attempt += 1;
                    }
                    Err(error) => return Err(ClipTextCorrectionError::from(error)),
                }
            };
            summary.token_usage = summary.token_usage.saturating_add(output.token_usage);

            // 输出必须与输入等长且 index 连续，否则整批丢弃——宁可保留原文，
            // 也不能把错位的文本写到别的句段上。
            if output.corrections.len() != batch.len() {
                return Err(ClipTextCorrectionError::InvalidResponse);
            }
            let mut corrections = Vec::new();
            for (index, item) in output.corrections.iter().enumerate() {
                if item.index as usize != index {
                    return Err(ClipTextCorrectionError::InvalidResponse);
                }
                let corrected = item.text.trim();
                let original = batch[index].text.as_str();
                if corrected.is_empty() || corrected == original {
                    summary.unchanged += 1;
                    continue;
                }
                corrections.push((
                    batch[index].segment_id.clone(),
                    original.to_owned(),
                    corrected.to_owned(),
                ));
                summary.changed += 1;
            }
            if !corrections.is_empty() {
                self.repository
                    .record_transcript_segment_corrections(None, &corrections)?;
            }
        }
        Ok(summary)
    }

    async fn current_handle(
        &self,
        frozen: &super::FrozenModelCall,
    ) -> Result<super::ModelCallHandle, ClipTextCorrectionError> {
        let token = self
            .model_call
            .acquire_token(chrono::Utc::now().timestamp())
            .await?;
        Ok(super::ModelCallHandle {
            frozen: frozen.clone(),
            token,
        })
    }
}

fn batch_targets(
    targets: &[TranscriptCorrectionTarget],
    maximum_chars: usize,
) -> Vec<Vec<TranscriptCorrectionTarget>> {
    let mut batches = Vec::new();
    let mut current = Vec::new();
    let mut current_chars = 0_usize;
    for target in targets {
        let chars = target.text.chars().count();
        if !current.is_empty() && current_chars.saturating_add(chars) > maximum_chars {
            batches.push(std::mem::take(&mut current));
            current_chars = 0;
        }
        current_chars = current_chars.saturating_add(chars);
        current.push(target.clone());
    }
    if !current.is_empty() {
        batches.push(current);
    }
    batches
}

fn build_prompt(batch: &[TranscriptCorrectionTarget]) -> String {
    let texts = batch
        .iter()
        .map(|target| target.text.clone())
        .collect::<Vec<_>>();
    let payload = serde_json::to_string(&texts).unwrap_or_else(|_| "[]".to_owned());
    format!(
        "纠正以下 ASR 文本数组。只修正错别字、同音错词、明显识别错误和标点；保持原意、语气、专名、数字、条目数量和顺序，不润色、不改写、不合并或拆分。corrections 必须为与输入等长的 index + text 数组，index 从 0 连续递增。输入文本数组：{payload}"
    )
}
