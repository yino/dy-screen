//! 剪辑工程字幕的一键保守纠错工作流。

use std::sync::Arc;

use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio_util::sync::CancellationToken;
use unicode_segmentation::UnicodeSegmentation;

use crate::clip_workflow::{
    ClipWorkflowProgress, ClipWorkflowPublisher, SilentClipWorkflowPublisher,
};

use super::{
    AiRepository, AiRepositoryError, ClipTextCorrectionSummary, ClipTextCorrectionTarget,
    ClipTextCorrectionUpdate, CredentialStore, LlmError, SubtitleCorrectionOutput,
    SubtitleCorrectionProvider, SubtitleCorrectionRequest,
};

pub const CLIP_TEXT_CORRECTION_PROMPT_VERSION: &str = "clip-text-correction-v1";
const MAX_BATCH_CHARS: usize = 6_000;

#[derive(Debug, Error)]
pub enum ClipTextCorrectionError {
    #[error("当前工程没有可一键纠错的字幕")]
    NoEligibleSubtitles,
    #[error("DeepSeek Key 尚未配置")]
    Credential,
    #[error("LLM 返回的文本纠错结果无效")]
    InvalidResponse,
    #[error(transparent)]
    Llm(#[from] LlmError),
    #[error(transparent)]
    Repository(#[from] AiRepositoryError),
}

pub type Result<T> = std::result::Result<T, ClipTextCorrectionError>;

#[derive(Clone)]
pub struct ClipTextCorrectionWorkflow {
    repository: AiRepository,
    provider: Arc<dyn SubtitleCorrectionProvider>,
    credentials: Arc<dyn CredentialStore>,
    publisher: Arc<dyn ClipWorkflowPublisher>,
}

impl ClipTextCorrectionWorkflow {
    pub fn new(
        repository: AiRepository,
        provider: Arc<dyn SubtitleCorrectionProvider>,
        credentials: Arc<dyn CredentialStore>,
    ) -> Self {
        Self {
            repository,
            provider,
            credentials,
            publisher: Arc::new(SilentClipWorkflowPublisher),
        }
    }

    pub fn with_publisher(mut self, publisher: Arc<dyn ClipWorkflowPublisher>) -> Self {
        self.publisher = publisher;
        self
    }

    pub async fn correct_project(
        &self,
        clip_project_id: i64,
        expected_project_version: u32,
        cancellation: CancellationToken,
    ) -> Result<ClipTextCorrectionSummary> {
        self.correct_project_with_ownership(
            clip_project_id,
            expected_project_version,
            cancellation,
            false,
        )
        .await
    }

    pub async fn correct_smart_project(
        &self,
        clip_project_id: i64,
        expected_project_version: u32,
        cancellation: CancellationToken,
    ) -> Result<ClipTextCorrectionSummary> {
        self.correct_project_with_ownership(
            clip_project_id,
            expected_project_version,
            cancellation,
            true,
        )
        .await
    }

    async fn correct_project_with_ownership(
        &self,
        clip_project_id: i64,
        expected_project_version: u32,
        cancellation: CancellationToken,
        automated: bool,
    ) -> Result<ClipTextCorrectionSummary> {
        self.publish(
            clip_project_id,
            None,
            "preparing",
            0,
            0,
            "正在准备可纠错字幕",
        );
        let plan = self
            .repository
            .prepare_clip_text_correction(clip_project_id, expected_project_version)?;
        if plan.targets.is_empty() {
            return Err(ClipTextCorrectionError::NoEligibleSubtitles);
        }
        let settings = self.repository.get_llm_provider_settings(true)?;
        let api_key = self
            .credentials
            .get()
            .map_err(|_| ClipTextCorrectionError::Credential)?
            .filter(|value| !value.trim().is_empty())
            .ok_or(ClipTextCorrectionError::Credential)?;
        let batches = batch_targets(&plan.targets, MAX_BATCH_CHARS);
        let fingerprint = correction_fingerprint(&plan.targets);
        let run_id = self.repository.begin_clip_text_correction_run(
            clip_project_id,
            plan.project_version,
            &settings.model_id,
            CLIP_TEXT_CORRECTION_PROMPT_VERSION,
            &fingerprint,
            plan.targets.len(),
            plan.skipped_manual,
            plan.skipped_hidden,
            batches.len(),
        )?;
        self.publish(
            clip_project_id,
            Some(run_id),
            "correcting",
            0,
            batches.len(),
            "正在调用 LLM 纠错",
        );

        let mut updates = Vec::with_capacity(plan.targets.len());
        let mut token_usage = 0_u64;
        let mut processed = 0_usize;
        for (batch_index, batch) in batches.iter().enumerate() {
            if cancellation.is_cancelled() {
                let error = ClipTextCorrectionError::Llm(LlmError::Cancelled);
                let _ = self.repository.finish_clip_text_correction_run(
                    run_id,
                    "cancelled",
                    "cancelled",
                    processed,
                    0,
                    0,
                    batch_index,
                    token_usage,
                    Some(("clip_text_correction_cancelled", &error.to_string())),
                );
                return Err(error);
            }
            let texts = batch
                .iter()
                .map(|item| item.text.as_str())
                .collect::<Vec<_>>();
            let payload = serde_json::to_string(&texts)
                .map_err(|_| ClipTextCorrectionError::InvalidResponse)?;
            let prompt = format!(
                "纠正以下 ASR 文本数组。只修正错别字、同音错词、明显识别错误和标点；保持原意、语气、专名、数字、条目数量和顺序，不润色、不改写、不合并或拆分。corrections 必须为与输入等长的 index + text 数组，index 从 0 连续递增。输入文本数组：{payload}"
            );
            let output = match self
                .provider
                .correct_subtitles(
                    &settings,
                    &api_key,
                    SubtitleCorrectionRequest { prompt },
                    cancellation.clone(),
                )
                .await
                .map_err(ClipTextCorrectionError::from)
            {
                Ok(output) => output,
                Err(error) => {
                    let status = if cancellation.is_cancelled() {
                        "cancelled"
                    } else {
                        "failed"
                    };
                    let _ = self.repository.finish_clip_text_correction_run(
                        run_id,
                        status,
                        status,
                        processed,
                        0,
                        0,
                        batch_index,
                        token_usage,
                        Some(("clip_text_correction_provider_failed", &error.to_string())),
                    );
                    return Err(error);
                }
            };
            token_usage = token_usage.saturating_add(output.token_usage);
            let corrected = match validate_correction_output(&output, batch.len()) {
                Ok(corrected) => corrected,
                Err(error) => {
                    let _ = self.repository.finish_clip_text_correction_run(
                        run_id,
                        "failed",
                        "validation_failed",
                        processed,
                        0,
                        0,
                        batch_index,
                        token_usage,
                        Some(("clip_text_correction_invalid_response", &error.to_string())),
                    );
                    return Err(error);
                }
            };
            updates.extend(batch.iter().zip(corrected).map(|(target, corrected_text)| {
                ClipTextCorrectionUpdate {
                    subtitle_id: target.subtitle_id,
                    expected_text: target.text.clone(),
                    corrected_text,
                }
            }));
            processed += batch.len();
            self.repository.update_clip_text_correction_run(
                run_id,
                "correcting",
                batch_index + 1,
                processed,
                token_usage,
            )?;
            self.publish(
                clip_project_id,
                Some(run_id),
                "correcting",
                batch_index + 1,
                batches.len(),
                "正在调用 LLM 纠错",
            );
        }

        if cancellation.is_cancelled() {
            let error = ClipTextCorrectionError::Llm(LlmError::Cancelled);
            let _ = self.repository.finish_clip_text_correction_run(
                run_id,
                "cancelled",
                "cancelled",
                processed,
                0,
                0,
                batches.len(),
                token_usage,
                Some(("clip_text_correction_cancelled", &error.to_string())),
            );
            return Err(error);
        }
        self.repository.update_clip_text_correction_run(
            run_id,
            "saving",
            batches.len(),
            processed,
            token_usage,
        )?;
        self.publish(
            clip_project_id,
            Some(run_id),
            "saving",
            batches.len(),
            batches.len(),
            "正在校验并保存工程字幕",
        );
        let applied = if automated {
            self.repository.apply_automated_clip_text_corrections(
                clip_project_id,
                plan.project_version,
                &updates,
            )
        } else {
            self.repository.apply_clip_text_corrections(
                clip_project_id,
                plan.project_version,
                &updates,
            )
        };
        let (detail, changed) = match applied {
            Ok(result) => result,
            Err(error) => {
                let wrapped = ClipTextCorrectionError::Repository(error);
                let _ = self.repository.finish_clip_text_correction_run(
                    run_id,
                    "failed",
                    "saving_failed",
                    processed,
                    0,
                    0,
                    batches.len(),
                    token_usage,
                    Some(("clip_text_correction_commit_failed", &wrapped.to_string())),
                );
                return Err(wrapped);
            }
        };
        let unchanged = processed.saturating_sub(changed);
        self.repository.finish_clip_text_correction_run(
            run_id,
            "completed",
            "completed",
            processed,
            changed,
            unchanged,
            batches.len(),
            token_usage,
            None,
        )?;
        self.publish(
            clip_project_id,
            Some(run_id),
            "completed",
            batches.len(),
            batches.len(),
            "文本纠错已完成",
        );
        Ok(ClipTextCorrectionSummary {
            run_id,
            processed,
            changed,
            unchanged,
            skipped_manual: plan.skipped_manual,
            skipped_hidden: plan.skipped_hidden,
            total_batches: batches.len(),
            token_usage,
            detail,
        })
    }

    fn publish(
        &self,
        clip_project_id: i64,
        run_id: Option<i64>,
        stage: &str,
        completed: usize,
        total: usize,
        message: &str,
    ) {
        self.publisher.publish(&ClipWorkflowProgress {
            workflow: "textCorrection".to_owned(),
            clip_project_id,
            run_id,
            stage: stage.to_owned(),
            completed,
            total,
            message: message.to_owned(),
        });
    }
}

fn batch_targets(
    targets: &[ClipTextCorrectionTarget],
    maximum_chars: usize,
) -> Vec<Vec<ClipTextCorrectionTarget>> {
    let mut batches = Vec::new();
    let mut current = Vec::new();
    let mut current_chars = 0_usize;
    for target in targets {
        let chars = target.text.chars().count();
        if !current.is_empty() && current_chars.saturating_add(chars) > maximum_chars {
            batches.push(std::mem::take(&mut current));
            current_chars = 0;
        }
        current.push(target.clone());
        current_chars = current_chars.saturating_add(chars);
    }
    if !current.is_empty() {
        batches.push(current);
    }
    batches
}

fn validate_correction_output(
    output: &SubtitleCorrectionOutput,
    expected_len: usize,
) -> Result<Vec<String>> {
    if output.corrections.len() != expected_len {
        return Err(ClipTextCorrectionError::InvalidResponse);
    }
    let mut corrected = Vec::with_capacity(expected_len);
    for (expected_index, item) in output.corrections.iter().enumerate() {
        if usize::try_from(item.index).ok() != Some(expected_index) {
            return Err(ClipTextCorrectionError::InvalidResponse);
        }
        corrected.push(validate_corrected_text(&item.text)?);
    }
    Ok(corrected)
}

fn validate_corrected_text(value: &str) -> Result<String> {
    if value
        .chars()
        .any(|character| character.is_control() && !character.is_whitespace())
    {
        return Err(ClipTextCorrectionError::InvalidResponse);
    }
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() || normalized.graphemes(true).count() > 500 {
        return Err(ClipTextCorrectionError::InvalidResponse);
    }
    Ok(normalized)
}

fn correction_fingerprint(targets: &[ClipTextCorrectionTarget]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(CLIP_TEXT_CORRECTION_PROMPT_VERSION.as_bytes());
    for target in targets {
        hasher.update(target.subtitle_id.to_le_bytes());
        hasher.update((target.text.len() as u64).to_le_bytes());
        hasher.update(target.text.as_bytes());
    }
    hex::encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;
    use rusqlite::params;

    use super::*;
    use crate::ai::SubtitleCorrectionItem;
    use crate::database::Database;

    #[derive(Default)]
    struct FailSecondBatchProvider {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl SubtitleCorrectionProvider for FailSecondBatchProvider {
        async fn correct_subtitles(
            &self,
            _settings: &super::super::LlmProviderSettings,
            _api_key: &str,
            _request: SubtitleCorrectionRequest,
            _cancellation: CancellationToken,
        ) -> std::result::Result<SubtitleCorrectionOutput, LlmError> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            if call == 1 {
                return Err(LlmError::Temporary);
            }
            Ok(SubtitleCorrectionOutput {
                corrections: (0..12)
                    .map(|index| SubtitleCorrectionItem {
                        index,
                        text: "改".repeat(500),
                    })
                    .collect(),
                token_usage: 12,
            })
        }
    }

    fn target(id: i64, text: &str) -> ClipTextCorrectionTarget {
        ClipTextCorrectionTarget {
            subtitle_id: id,
            text: text.to_owned(),
        }
    }

    #[test]
    fn batches_preserve_complete_subtitle_order() {
        let batches = batch_targets(
            &[target(1, "一二三"), target(2, "四五六"), target(3, "七八")],
            6,
        );
        assert_eq!(batches.len(), 2);
        assert_eq!(
            batches[0]
                .iter()
                .map(|item| item.subtitle_id)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert_eq!(batches[1][0].subtitle_id, 3);
    }

    #[test]
    fn correction_response_must_be_complete_and_ordered() {
        let valid = SubtitleCorrectionOutput {
            corrections: vec![
                SubtitleCorrectionItem {
                    index: 0,
                    text: "文本一".to_owned(),
                },
                SubtitleCorrectionItem {
                    index: 1,
                    text: "文本二".to_owned(),
                },
            ],
            token_usage: 8,
        };
        assert_eq!(
            validate_correction_output(&valid, 2).unwrap(),
            vec!["文本一", "文本二"]
        );
        let invalid = SubtitleCorrectionOutput {
            corrections: vec![
                SubtitleCorrectionItem {
                    index: 1,
                    text: "文本二".to_owned(),
                },
                SubtitleCorrectionItem {
                    index: 0,
                    text: "文本一".to_owned(),
                },
            ],
            token_usage: 0,
        };
        assert!(validate_correction_output(&invalid, 2).is_err());
    }

    #[tokio::test]
    async fn later_batch_failure_keeps_all_subtitles_and_project_version_unchanged() {
        let database = Database::open_in_memory().unwrap();
        database.migrate().unwrap();
        {
            let connection = database.connection().unwrap();
            connection
                .pragma_update(None, "foreign_keys", "OFF")
                .unwrap();
            connection
                .execute(
                    r#"INSERT INTO ai_clip_projects(
                           id, highlight_run_id, name, version, export_status, export_progress,
                           created_at, updated_at
                       ) VALUES(1, 1, '批次回滚测试', 1, 'idle', 0, ?1, ?1)"#,
                    ["2026-08-08T00:00:00Z"],
                )
                .unwrap();
            connection
                .execute(
                    r#"INSERT INTO ai_clip_segments(
                           id, clip_project_id, candidate_id, input_id, position, title,
                           source_start_ms, source_end_ms, created_at, updated_at
                       ) VALUES(11, 1, 1, 1, 0, '测试片段', 0, 13000, ?1, ?1)"#,
                    ["2026-08-08T00:00:00Z"],
                )
                .unwrap();
            for index in 0_i64..13 {
                let text = "字".repeat(500);
                connection
                    .execute(
                        r#"INSERT INTO ai_clip_subtitles(
                               clip_project_id, clip_segment_id, input_id, stable_segment_id,
                               source_start_ms, source_end_ms, original_text, text, hidden,
                               created_at, updated_at
                           ) VALUES(1, 11, 1, ?1, ?2, ?3, ?4, ?4, 0, ?5, ?5)"#,
                        params![
                            format!("segment-{index}"),
                            index * 1000,
                            (index + 1) * 1000,
                            text,
                            "2026-08-08T00:00:00Z"
                        ],
                    )
                    .unwrap();
            }
            connection
                .pragma_update(None, "foreign_keys", "ON")
                .unwrap();
        }

        let credentials = Arc::new(super::super::MemoryCredentialStore::new());
        credentials.set("sk-test").unwrap();
        let provider = Arc::new(FailSecondBatchProvider::default());
        let workflow = ClipTextCorrectionWorkflow::new(
            AiRepository::new(database.clone()),
            provider.clone(),
            credentials,
        );

        assert!(
            workflow
                .correct_project(1, 1, CancellationToken::new())
                .await
                .is_err()
        );
        assert_eq!(provider.calls.load(Ordering::SeqCst), 2);

        let connection = database.connection().unwrap();
        let version = connection
            .query_row(
                "SELECT version FROM ai_clip_projects WHERE id = 1",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap();
        let changed = connection
            .query_row(
                "SELECT COUNT(*) FROM ai_clip_subtitles WHERE text != original_text",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap();
        let run = connection
            .query_row(
                r#"SELECT status, completed_batches
                   FROM ai_clip_text_correction_runs ORDER BY id DESC LIMIT 1"#,
                [],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
            )
            .unwrap();
        assert_eq!(version, 1);
        assert_eq!(changed, 0);
        assert_eq!(run, ("failed".to_owned(), 1));
    }
}
