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
    #[error("当前账号未开通 AI 服务，请开通后重试")]
    NotActivated,
    #[error("AI 额度已用完，请等待额度恢复")]
    QuotaExhausted,
    #[error("LLM 返回的文本纠错结果无效")]
    InvalidResponse,
    #[error(transparent)]
    Llm(#[from] LlmError),
    #[error(transparent)]
    Repository(#[from] AiRepositoryError),
}

pub type Result<T> = std::result::Result<T, ClipTextCorrectionError>;

/// 纠错运行前的授权与额度门禁。
///
/// 未开通与额度耗尽是**两种不同的原因**，必须分别判定并分别报告；不得把其中任一
/// 描述为 Provider 网络故障。
pub trait ClipCorrectionGate: Send + Sync {
    /// 客户端是否已激活。
    fn activation_ready(&self) -> bool;
    /// 账号是否已开通 AI 服务。
    fn service_activated(&self) -> bool;
    /// 当前是否仍有可用额度。
    fn quota_available(&self) -> bool;
}

/// 由激活状态驱动的门禁：未激活时明确拒绝。
///
/// 开通与额度不在此判定：客户端没有这两个状态的本地权威来源（heartbeat 响应不含
/// 额度字段），它们由上游业务错误码 3001 / 3003 / 3004 映射而来，见
/// `map_upstream_business_code`。
#[derive(Clone)]
pub struct ActivationCorrectionGate {
    activation: crate::activation::ActivationService,
}

impl ActivationCorrectionGate {
    /// 由激活服务创建门禁。
    pub fn new(activation: crate::activation::ActivationService) -> Self {
        Self { activation }
    }
}

impl ClipCorrectionGate for ActivationCorrectionGate {
    fn activation_ready(&self) -> bool {
        self.activation.is_active()
    }
    fn service_activated(&self) -> bool {
        true
    }
    fn quota_available(&self) -> bool {
        true
    }
}

/// 把上游业务错误码映射为可区分的授权/额度原因。
///
/// 返回 `None` 表示该错误码不属于授权或额度类别。客户端没有额度的本地权威来源，
/// 因此额度耗尽只能由上游错误码判定，不能由 heartbeat 推断。
pub fn map_upstream_business_code(code: i32) -> Option<ClipTextCorrectionError> {
    match code {
        3001 => Some(ClipTextCorrectionError::NotActivated),
        3003 | 3004 => Some(ClipTextCorrectionError::QuotaExhausted),
        _ => None,
    }
}

/// 默认门禁：全部放行，供无授权上下文的调用方与测试使用。
#[derive(Debug, Default, Clone, Copy)]
pub struct PermissiveCorrectionGate;

impl ClipCorrectionGate for PermissiveCorrectionGate {
    fn activation_ready(&self) -> bool {
        true
    }
    fn service_activated(&self) -> bool {
        true
    }
    fn quota_available(&self) -> bool {
        true
    }
}

/// 按门禁状态判定本次运行是否可继续。
///
/// 判定顺序固定为"激活 → 开通 → 额度"，使错误原因稳定可预期。
pub fn evaluate_correction_gate(
    gate: &dyn ClipCorrectionGate,
) -> std::result::Result<(), ClipTextCorrectionError> {
    if !gate.activation_ready() {
        return Err(ClipTextCorrectionError::Credential);
    }
    if !gate.service_activated() {
        return Err(ClipTextCorrectionError::NotActivated);
    }
    if !gate.quota_available() {
        return Err(ClipTextCorrectionError::QuotaExhausted);
    }
    Ok(())
}

#[derive(Clone)]
pub struct ClipTextCorrectionWorkflow {
    repository: AiRepository,
    provider: Arc<dyn SubtitleCorrectionProvider>,
    credentials: Arc<dyn CredentialStore>,
    publisher: Arc<dyn ClipWorkflowPublisher>,
    gate: Arc<dyn ClipCorrectionGate>,
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
            gate: Arc::new(PermissiveCorrectionGate),
        }
    }

    /// 注入授权与额度门禁。
    pub fn with_gate(mut self, gate: Arc<dyn ClipCorrectionGate>) -> Self {
        self.gate = gate;
        self
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
        // 授权与额度必须早于任何批次提交判定：不通过时保留字幕不变，
        // 并给出可区分的原因（未开通 / 额度耗尽）。
        evaluate_correction_gate(self.gate.as_ref())?;
        let plan = self
            .repository
            .prepare_clip_text_correction(clip_project_id, expected_project_version)?;
        if plan.targets.is_empty() {
            return Err(ClipTextCorrectionError::NoEligibleSubtitles);
        }
        let settings = self.repository.get_llm_provider_settings(true)?;
        // 统一调用点：运行开始前解析一次并冻结，本运行的全部批次共用同一模型。
        let frozen_call = crate::ai::llm::FrozenModelCall::resolve(&settings)?;
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
            frozen_call.model_id(),
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
                    &api_key,
                    SubtitleCorrectionRequest {
                        prompt,
                        frozen: frozen_call.clone(),
                    },
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

#[cfg(test)]
mod correction_gate_tests {
    use super::*;

    struct FixedGate {
        activation: bool,
        activated: bool,
        quota: bool,
    }

    impl ClipCorrectionGate for FixedGate {
        fn activation_ready(&self) -> bool {
            self.activation
        }
        fn service_activated(&self) -> bool {
            self.activated
        }
        fn quota_available(&self) -> bool {
            self.quota
        }
    }

    fn gate(activation: bool, activated: bool, quota: bool) -> FixedGate {
        FixedGate {
            activation,
            activated,
            quota,
        }
    }

    /// 任务 4.4：未开通与额度耗尽必须是可区分的原因，且都不被描述为网络故障。
    #[test]
    fn gate_reports_distinguishable_authorization_causes() {
        // 全部就绪时放行。
        assert!(evaluate_correction_gate(&gate(true, true, true)).is_ok());
        // 未开通。
        let not_activated = evaluate_correction_gate(&gate(true, false, true)).unwrap_err();
        // 额度耗尽。
        let quota = evaluate_correction_gate(&gate(true, true, false)).unwrap_err();
        assert!(matches!(
            not_activated,
            ClipTextCorrectionError::NotActivated
        ));
        assert!(matches!(quota, ClipTextCorrectionError::QuotaExhausted));
        // 两者必须可区分（不得合并成同一个原因）。
        assert_ne!(not_activated.to_string(), quota.to_string());
        // 原因说明必须可操作，且不得暗示网络故障。
        for error in [&not_activated, &quota] {
            let message = error.to_string();
            assert!(
                !message.contains("网络") && !message.contains("Provider"),
                "授权/额度问题不得被描述为网络故障: {message}"
            );
        }
        assert!(not_activated.to_string().contains("未开通"));
        assert!(quota.to_string().contains("额度"));
        // 未激活优先于未开通：判定顺序稳定。
        let inactive = evaluate_correction_gate(&gate(false, false, false)).unwrap_err();
        assert!(matches!(inactive, ClipTextCorrectionError::Credential));
    }

    /// 默认门禁放行，保持无授权上下文调用方的既有行为不变。
    #[test]
    fn permissive_gate_keeps_default_behavior() {
        assert!(evaluate_correction_gate(&PermissiveCorrectionGate).is_ok());
    }

    /// 额度耗尽只能由上游业务错误码判定；客户端没有额度的本地权威来源。
    #[test]
    fn upstream_business_codes_map_to_distinguishable_causes() {
        assert!(matches!(
            map_upstream_business_code(3001),
            Some(ClipTextCorrectionError::NotActivated)
        ));
        assert!(matches!(
            map_upstream_business_code(3003),
            Some(ClipTextCorrectionError::QuotaExhausted)
        ));
        assert!(matches!(
            map_upstream_business_code(3004),
            Some(ClipTextCorrectionError::QuotaExhausted)
        ));
        // 未开通与额度耗尽不得映射成同一原因。
        assert_ne!(
            map_upstream_business_code(3001).unwrap().to_string(),
            map_upstream_business_code(3003).unwrap().to_string()
        );
        // 与授权/额度无关的错误码不得被误判。
        for code in [1001, 1007, 2001, 9000, 0] {
            assert!(
                map_upstream_business_code(code).is_none(),
                "错误码 {code} 不属于授权或额度类别"
            );
        }
    }
}

#[cfg(test)]
mod correction_batching_equivalence_tests {
    use super::*;

    fn target(subtitle_id: i64, text: &str) -> ClipTextCorrectionTarget {
        ClipTextCorrectionTarget {
            subtitle_id,
            text: text.to_owned(),
        }
    }

    /// 任务 4.1 的等价性断言：转换到统一调用点**没有改变**分批边界与批次顺序。
    ///
    /// 模型解析来源的变化不得影响批处理：同一组目标在同一上限下必须得到逐字相同的
    /// 批次划分，且目标顺序保持输入顺序。
    #[test]
    fn model_resolution_does_not_change_batch_boundaries_or_order() {
        let targets = vec![
            target(0, &"甲".repeat(30)),
            target(1, &"乙".repeat(30)),
            target(2, &"丙".repeat(30)),
            target(3, &"丁".repeat(10)),
        ];
        let batches = batch_targets(&targets, 70);
        // 边界由字符数决定：30+30 <= 70 同批；再加 30 超限则开新批。
        let sizes: Vec<usize> = batches.iter().map(Vec::len).collect();
        assert_eq!(sizes, vec![2, 2], "分批边界必须与转换前一致: {sizes:?}");
        // 顺序必须保持输入顺序，跨批不重排。
        let order: Vec<i64> = batches
            .iter()
            .flat_map(|batch| batch.iter().map(|item| item.subtitle_id))
            .collect();
        assert_eq!(order, vec![0, 1, 2, 3], "批次顺序必须保持输入顺序");
        // 同一输入重复计算必须得到相同划分（确定性）。
        let again = batch_targets(&targets, 70);
        assert_eq!(
            sizes,
            again.iter().map(Vec::len).collect::<Vec<_>>(),
            "分批必须确定"
        );
        // 单条超出上限时不得死循环、也不得丢条目。
        let oversized = batch_targets(&[target(0, &"长".repeat(200))], 70);
        assert_eq!(oversized.len(), 1);
        assert_eq!(oversized[0].len(), 1);
        // 空输入产出空批次。
        assert!(batch_targets(&[], 70).is_empty());
    }
}
