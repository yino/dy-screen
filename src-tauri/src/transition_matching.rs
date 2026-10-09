//! 受限转场匹配 Agent：只使用本地目录语义与相邻工程字幕。

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::ai::{
    AiRepository, LlmError, TransitionAgentMatch, TransitionAgentOutput, TransitionAgentProvider,
    TransitionAgentRequest, TransitionAgentScore, TransitionScoreOutput, TransitionScoreRequest,
};
use crate::clip_workflow::{
    ClipWorkflowProgress, ClipWorkflowPublisher, SilentClipWorkflowPublisher,
};
use crate::transition_assets::TransitionMaterialAssetService;
use crate::transition_materials::{
    BoundaryAgentScoreInput, ClipTransitionBoundary, TransitionMatchRunOutcome, TransitionMaterial,
    TransitionMaterialError, TransitionMaterialRepository,
};

pub const TRANSITION_MATCH_PROMPT_VERSION: &str = "transition-match-v3";
pub const TRANSITION_SCORE_PROMPT_VERSION: &str = "transition-score-v1";
const MAX_CONTEXT_CHARS: usize = 480;
const MAX_CANDIDATES: usize = 12;
const MAX_MATCHED_CANDIDATES: usize = 3;
const MAX_REASON_CHARS: usize = 160;
/// 匹配结果校验失败时的最大尝试次数。只针对语义校验失败重试，网络和鉴权错误不重试。
const MAX_MATCH_ATTEMPTS: usize = 2;
/// 诊断摘要的上限，避免把模型输出回写成不受控的长文本。
const MAX_DIAGNOSTIC_CHARS: usize = 240;

#[derive(Debug, Error)]
pub enum TransitionMatchingError {
    #[error("当前工程没有可匹配的转场位置")]
    NoBoundaries,
    #[error("本地转场素材目录为空，请先同步素材")]
    EmptyCatalog,
    #[error("当前没有可用的模型调用凭据")]
    Credential,
    #[error("Agent 返回了无效的转场匹配结果：{0}")]
    InvalidResponse(String),
    #[error(transparent)]
    Material(#[from] TransitionMaterialError),
    #[error(transparent)]
    Llm(#[from] LlmError),
    #[error("无法读取剪辑工程上下文")]
    Repository,
}

pub type Result<T> = std::result::Result<T, TransitionMatchingError>;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TransitionMatchSummary {
    pub run_id: i64,
    pub threshold: u8,
    pub matched: usize,
    pub auto_applied: usize,
    pub suggestions: usize,
    pub none_suggestions: usize,
    pub token_usage: u64,
    pub boundaries: Vec<ClipTransitionBoundary>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct PromptBoundary {
    boundary_id: i64,
    before_asr: String,
    after_asr: String,
    streamer_tags: Vec<String>,
    candidates: Vec<PromptMaterial>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct PromptMaterial {
    asset_key: String,
    asset_version: i64,
    title: String,
    description: String,
    tags: Vec<String>,
    category: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ScorePromptBoundary {
    boundary_id: i64,
    before_asr: String,
    after_asr: String,
    streamer_tags: Vec<String>,
    candidates: Vec<PromptMaterial>,
}

#[derive(Clone)]
pub struct TransitionMatchingWorkflow {
    repository: TransitionMaterialRepository,
    ai_repository: AiRepository,
    provider: Arc<dyn TransitionAgentProvider>,
    /// 统一调用点：模型与短期 token 都从这里取，链路自身不持有凭据。
    model_call: Arc<crate::ai::ModelCallRuntime>,
    assets: TransitionMaterialAssetService,
    publisher: Arc<dyn ClipWorkflowPublisher>,
}

impl TransitionMatchingWorkflow {
    pub fn new(
        repository: TransitionMaterialRepository,
        ai_repository: AiRepository,
        provider: Arc<dyn TransitionAgentProvider>,
        model_call: Arc<crate::ai::ModelCallRuntime>,
        assets: TransitionMaterialAssetService,
    ) -> Self {
        Self {
            repository,
            ai_repository,
            provider,
            model_call,
            assets,
            publisher: Arc::new(SilentClipWorkflowPublisher),
        }
    }

    pub fn with_publisher(mut self, publisher: Arc<dyn ClipWorkflowPublisher>) -> Self {
        self.publisher = publisher;
        self
    }

    pub async fn match_boundaries(
        &self,
        clip_project_id: i64,
        target_boundary_id: Option<i64>,
        cancellation: CancellationToken,
    ) -> Result<TransitionMatchSummary> {
        let project_version = self
            .ai_repository
            .get_clip_project(clip_project_id)
            .map_err(|_| TransitionMatchingError::Repository)?
            .project
            .version;
        // 统一调用点：运行开始前冻结模型（两个 Agent 阶段共用同一模型）；
        // 阈值仍来自用户设置——模型无法改变阈值（任务 5.3）。
        let settings = self
            .ai_repository
            .get_llm_provider_settings(true)
            .map_err(|_| TransitionMatchingError::Repository)?;
        let frozen = self
            .model_call
            .freeze_for_run()
            .map_err(Self::map_llm_error)?;
        let threshold = settings.transition_auto_apply_score;
        let boundaries = self.repository.reconcile_boundaries(clip_project_id)?;
        let targets = boundaries
            .into_iter()
            .filter(|item| item.active && !item.manually_locked)
            .filter(|item| target_boundary_id.is_none_or(|id| item.id == id))
            .collect::<Vec<_>>();
        if targets.is_empty() {
            return Err(TransitionMatchingError::NoBoundaries);
        }
        let materials = self
            .repository
            .list_current_materials()?
            .into_iter()
            .filter(|item| item.render_mode == "bridge")
            .collect::<Vec<_>>();
        if materials.is_empty() {
            return Err(TransitionMatchingError::EmptyCatalog);
        }
        let tags = self.project_tags(clip_project_id)?;
        let mut allowed = HashMap::<i64, HashSet<(String, i64)>>::new();
        let mut prompt_boundaries = Vec::with_capacity(targets.len());
        for boundary in &targets {
            let (before_asr, after_asr) = self.boundary_asr(boundary)?;
            let candidates = recall_materials(&materials, &before_asr, &after_asr, &tags);
            allowed.insert(
                boundary.id,
                candidates
                    .iter()
                    .map(|item| (item.asset_key.clone(), item.asset_version))
                    .collect(),
            );
            prompt_boundaries.push(PromptBoundary {
                boundary_id: boundary.id,
                before_asr,
                after_asr,
                streamer_tags: tags.clone(),
                candidates: candidates.into_iter().map(prompt_material).collect(),
            });
        }
        let payload = serde_json::to_string(&prompt_boundaries)
            .map_err(|_| TransitionMatchingError::Repository)?;
        let boundary_count = prompt_boundaries.len();
        let match_prompt = format!(
            "为每个边界从 candidates 中按场景、语义和情绪适配度选择最多 {MAX_MATCHED_CANDIDATES} 个有序桥接视频候选。无法确定时返回 none=true 且 candidates 为空；否则 none=false。必须为全部 {boundary_count} 个边界各返回且仅返回一条记录，reason 不超过 {MAX_REASON_CHARS} 个字符，none=false 时只能使用该边界自己的 candidates 列表里的素材键，不得返回候选之外的键或任何渲染参数。输入：{payload}"
        );
        let fingerprint = hex::encode(Sha256::digest(match_prompt.as_bytes()));
        let run_id = self.repository.begin_match_run(
            clip_project_id,
            project_version,
            crate::ai::ModelCallPath::CloudDirect.as_str(),
            frozen.model_id(),
            TRANSITION_MATCH_PROMPT_VERSION,
            TRANSITION_SCORE_PROMPT_VERSION,
            threshold,
            targets.len(),
            &fingerprint,
        )?;
        let mut outcome = TransitionMatchRunOutcome::default();
        self.publish(
            clip_project_id,
            run_id,
            "matching",
            0,
            targets.len(),
            "正在匹配转场场景",
        );
        // 边界必须逐个对应。历史上单个边界的越界候选会让整批结果被拒，
        // 因此这里接受合法边界、累计无法使用的边界，并只在结果不可用时重试一次；
        // 两次尝试中保留可用边界更多的一次，避免重试反而丢回已经可用的结果。
        let mut best: Option<MatchValidation> = None;
        let mut retry_reason: Option<String> = None;
        for attempt in 1..=MAX_MATCH_ATTEMPTS {
            if attempt > 1 {
                self.publish(
                    clip_project_id,
                    run_id,
                    "matching",
                    0,
                    targets.len(),
                    "正在重新匹配转场场景",
                );
            }
            let prompt = if attempt == 1 {
                match_prompt.clone()
            } else {
                format!(
                    "{match_prompt}\n\n上一次返回未通过校验：{}。请重新输出，必须为全部 {boundary_count} 个边界各返回且仅返回一条记录，none=false 时只能使用该边界 candidates 列表中的素材键。",
                    retry_reason.as_deref().unwrap_or("结果不完整")
                )
            };
            let output = match self
                .call_with_token_refresh(
                    &frozen,
                    |handle| {
                        let prompt = prompt.clone();
                        let cancellation = cancellation.clone();
                        async move {
                            self.provider
                                .match_transitions(
                                    &handle,
                                    TransitionAgentRequest { prompt },
                                    cancellation,
                                )
                                .await
                        }
                    },
                    cancellation.clone(),
                )
                .await
            {
                Ok(output) => output,
                Err(error) => {
                    let _ = self.repository.finish_match_run(
                        run_id,
                        false,
                        if cancellation.is_cancelled() {
                            "cancelled"
                        } else {
                            "matching_failed"
                        },
                        outcome,
                        Some(("transition_agent_failed", &error.to_string())),
                    );
                    return Err(error);
                }
            };
            outcome.token_usage = outcome.token_usage.saturating_add(output.token_usage);
            let validation = validate_match_output(&output, &allowed);
            let retry_worthwhile = attempt < MAX_MATCH_ATTEMPTS
                && (validation.accepted.is_empty()
                    || validation.failed > validation.accepted.len());
            retry_reason = validation.detail.clone();
            let better = best.as_ref().is_none_or(|previous: &MatchValidation| {
                validation.accepted.len() > previous.accepted.len()
            });
            if better {
                best = Some(validation);
            }
            if !retry_worthwhile {
                break;
            }
        }
        let validation = best.unwrap_or(MatchValidation {
            accepted: Vec::new(),
            failed: allowed.len(),
            detail: None,
        });
        let accepted = validation.accepted;
        let failed_boundaries = validation.failed;
        if accepted.is_empty() {
            let error = TransitionMatchingError::InvalidResponse(
                validation
                    .detail
                    .unwrap_or_else(|| "Agent 未返回任何可用边界".to_owned()),
            );
            let _ = self.repository.finish_match_run(
                run_id,
                false,
                "matching_failed",
                outcome,
                Some(("transition_agent_invalid_response", &error.to_string())),
            );
            return Err(error);
        }
        outcome.matched_boundaries = accepted.len();
        outcome.failed_boundaries = failed_boundaries;
        let matched = accepted;
        self.repository.update_match_run_stage(
            run_id,
            "scoring",
            outcome.matched_boundaries,
            outcome.token_usage,
        )?;
        self.publish(
            clip_project_id,
            run_id,
            "scoring",
            0,
            matched.len(),
            "正在评分候选素材",
        );

        if cancellation.is_cancelled() {
            let error = TransitionMatchingError::Llm(LlmError::Cancelled);
            let _ = self.repository.finish_match_run(
                run_id,
                false,
                "cancelled",
                outcome,
                Some(("transition_agent_cancelled", &error.to_string())),
            );
            return Err(error);
        }

        let score_boundaries = build_score_prompt(&prompt_boundaries, &matched)?;
        let score_validation = if score_boundaries.is_empty() {
            ScoreValidation {
                accepted: Vec::new(),
                detail: None,
            }
        } else {
            let score_payload = serde_json::to_string(&score_boundaries)
                .map_err(|_| TransitionMatchingError::Repository)?;
            let score_prompt = format!(
                "逐个评价输入中的全部候选。每个候选都必须返回 0 到 10 的 totalScore、sceneScore、continuityScore、rhythmScore、materialScore 和不超过 {MAX_REASON_CHARS} 个字符的 reason；必须完整覆盖候选集合，不得新增、删除或替换素材。输入：{score_payload}"
            );
            let score_output = match self
                .call_with_token_refresh(
                    &frozen,
                    |handle| {
                        let prompt = score_prompt.clone();
                        let cancellation = cancellation.clone();
                        async move {
                            self.provider
                                .score_transitions(
                                    &handle,
                                    TransitionScoreRequest { prompt },
                                    cancellation,
                                )
                                .await
                        }
                    },
                    cancellation.clone(),
                )
                .await
            {
                Ok(output) => output,
                Err(error) => {
                    let _ = self.repository.finish_match_run(
                        run_id,
                        false,
                        if cancellation.is_cancelled() {
                            "cancelled"
                        } else {
                            "scoring_failed"
                        },
                        outcome,
                        Some(("transition_score_agent_failed", &error.to_string())),
                    );
                    return Err(error);
                }
            };
            outcome.token_usage = outcome.token_usage.saturating_add(score_output.token_usage);
            let validation = validate_score_output(&score_output, &matched);
            if validation.accepted.is_empty() {
                let error = TransitionMatchingError::InvalidResponse(
                    validation
                        .detail
                        .clone()
                        .unwrap_or_else(|| "Agent 未返回任何可用评分".to_owned()),
                );
                let _ = self.repository.finish_match_run(
                    run_id,
                    false,
                    "scoring_failed",
                    outcome,
                    Some(("transition_score_invalid_response", &error.to_string())),
                );
                return Err(error);
            }
            validation
        };
        let accepted_scores = score_validation.accepted;
        // 只有拿到合法评分的边界才能进入应用阶段，其余按无法处理计入失败边界。
        let mut applicable = Vec::with_capacity(matched.len());
        for item in matched {
            if item.none {
                applicable.push(item);
                continue;
            }
            let scored = item.candidates.iter().any(|candidate| {
                accepted_scores.iter().any(|score| {
                    score.boundary_id == item.boundary_id
                        && score.asset_key == candidate.asset_key
                        && score.asset_version == candidate.asset_version
                })
            });
            if scored {
                applicable.push(item);
            } else {
                outcome.failed_boundaries += 1;
            }
        }
        outcome.matched_boundaries = applicable.len();

        self.repository.update_match_run_stage(
            run_id,
            "applying",
            outcome.matched_boundaries,
            outcome.token_usage,
        )?;
        self.publish(
            clip_project_id,
            run_id,
            "applying",
            0,
            applicable.len(),
            "正在按阈值应用结果",
        );

        let mut auto_downloads = Vec::new();
        for item in &applicable {
            if cancellation.is_cancelled() {
                let error = TransitionMatchingError::Llm(LlmError::Cancelled);
                let _ = self.repository.finish_match_run(
                    run_id,
                    false,
                    "cancelled",
                    outcome,
                    Some(("transition_agent_cancelled", &error.to_string())),
                );
                return Err(error);
            }
            if item.none {
                self.repository.save_agent_score_suggestion(
                    item.boundary_id,
                    None,
                    true,
                    &bounded_reason(&item.reason),
                    false,
                )?;
                outcome.none_suggestions += 1;
                continue;
            }
            let best = best_score_for_match(item, &accepted_scores)?;
            let input = BoundaryAgentScoreInput {
                asset_key: &best.asset_key,
                asset_version: best.asset_version,
                total_score: best.total_score,
                scene_score: best.scene_score,
                continuity_score: best.continuity_score,
                rhythm_score: best.rhythm_score,
                material_score: best.material_score,
                reason: &best.reason,
            };
            let auto_apply = score_meets_threshold(best.total_score, threshold);
            self.repository.save_agent_score_suggestion(
                item.boundary_id,
                Some(&input),
                false,
                &bounded_reason(&best.reason),
                auto_apply,
            )?;
            if auto_apply {
                outcome.auto_applied += 1;
                auto_downloads.push((best.asset_key.clone(), best.asset_version));
            } else {
                outcome.suggestions += 1;
            }
        }
        self.repository
            .finish_match_run(run_id, true, "completed", outcome, None)?;
        let completion_message = if outcome.failed_boundaries == 0 {
            "转场 Agent 已完成".to_owned()
        } else {
            format!(
                "转场 Agent 已完成，{} 个边界成功，{} 个边界需人工处理",
                outcome.matched_boundaries, outcome.failed_boundaries
            )
        };
        self.publish(
            clip_project_id,
            run_id,
            "completed",
            outcome.matched_boundaries,
            outcome.matched_boundaries + outcome.failed_boundaries,
            &completion_message,
        );
        for (asset_key, asset_version) in auto_downloads {
            let assets = self.assets.clone();
            tauri::async_runtime::spawn(async move {
                let _ = assets.download_source(&asset_key, asset_version).await;
            });
        }
        Ok(TransitionMatchSummary {
            run_id,
            threshold,
            matched: outcome.matched_boundaries,
            auto_applied: outcome.auto_applied,
            suggestions: outcome.suggestions,
            none_suggestions: outcome.none_suggestions,
            token_usage: outcome.token_usage,
            boundaries: self.repository.list_boundaries(clip_project_id)?,
        })
    }

    /// 把统一调用点的错误映射为可区分的转场错误。
    fn map_llm_error(error: LlmError) -> TransitionMatchingError {
        match error {
            LlmError::Credential => TransitionMatchingError::Credential,
            other => TransitionMatchingError::Llm(other),
        }
    }

    /// 取当前可用的模型调用句柄（模型来自运行内冻结快照）。
    async fn current_handle(
        &self,
        frozen: &crate::ai::FrozenModelCall,
    ) -> Result<crate::ai::ModelCallHandle> {
        let token = self
            .model_call
            .acquire_token(chrono::Utc::now().timestamp())
            .await
            .map_err(Self::map_llm_error)?;
        Ok(crate::ai::ModelCallHandle {
            frozen: frozen.clone(),
            token,
        })
    }

    /// 调用一次 Provider；凭据类失败时重取一次 token 并重试当前调用。
    async fn call_with_token_refresh<T, F, Fut>(
        &self,
        frozen: &crate::ai::FrozenModelCall,
        call: F,
        cancellation: CancellationToken,
    ) -> Result<T>
    where
        F: Fn(crate::ai::ModelCallHandle) -> Fut,
        Fut: std::future::Future<Output = std::result::Result<T, LlmError>>,
    {
        let _ = cancellation;
        let handle = self.current_handle(frozen).await?;
        match call(handle).await {
            Ok(value) => Ok(value),
            Err(LlmError::Credential) => {
                let refreshed = self.current_handle(frozen).await?;
                call(refreshed).await.map_err(Self::map_llm_error)
            }
            Err(error) => Err(Self::map_llm_error(error)),
        }
    }

    fn publish(
        &self,
        clip_project_id: i64,
        run_id: i64,
        stage: &str,
        completed: usize,
        total: usize,
        message: &str,
    ) {
        self.publisher.publish(&ClipWorkflowProgress {
            workflow: "transitionAgent".to_owned(),
            clip_project_id,
            run_id: Some(run_id),
            stage: stage.to_owned(),
            completed,
            total,
            message: message.to_owned(),
        });
    }

    fn project_tags(&self, clip_project_id: i64) -> Result<Vec<String>> {
        let database = self.repository.database();
        let connection = database
            .connection()
            .map_err(|_| TransitionMatchingError::Repository)?;
        let values = connection
            .prepare(
                r#"SELECT run.tags_snapshot_json
                   FROM ai_clip_project_sources source
                   JOIN ai_highlight_runs run ON run.id = source.highlight_run_id
                   WHERE source.clip_project_id = ?1
                   ORDER BY source.source_order, run.id"#,
            )
            .map_err(|_| TransitionMatchingError::Repository)?
            .query_map([clip_project_id], |row| row.get::<_, String>(0))
            .map_err(|_| TransitionMatchingError::Repository)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|_| TransitionMatchingError::Repository)?;
        let mut tags = std::collections::BTreeSet::new();
        for json in values {
            for tag in serde_json::from_str::<Vec<String>>(&json)
                .map_err(|_| TransitionMatchingError::Repository)?
            {
                tags.insert(tag);
            }
        }
        Ok(tags.into_iter().collect())
    }

    fn boundary_asr(&self, boundary: &ClipTransitionBoundary) -> Result<(String, String)> {
        let left_id = boundary
            .left_clip_segment_id
            .ok_or(TransitionMatchingError::NoBoundaries)?;
        let right_id = boundary
            .right_clip_segment_id
            .ok_or(TransitionMatchingError::NoBoundaries)?;
        let database = self.repository.database();
        let connection = database
            .connection()
            .map_err(|_| TransitionMatchingError::Repository)?;
        let read = |segment_id: i64| -> Result<String> {
            let mut statement = connection.prepare(
                "SELECT text FROM ai_clip_subtitles WHERE clip_segment_id = ?1 AND hidden = 0 ORDER BY source_start_ms, id"
            ).map_err(|_| TransitionMatchingError::Repository)?;
            let values = statement
                .query_map([segment_id], |row| row.get::<_, String>(0))
                .map_err(|_| TransitionMatchingError::Repository)?
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(|_| TransitionMatchingError::Repository)?;
            Ok(values.join(" "))
        };
        Ok((
            tail_chars(&read(left_id)?, MAX_CONTEXT_CHARS),
            head_chars(&read(right_id)?, MAX_CONTEXT_CHARS),
        ))
    }
}

fn prompt_material(material: TransitionMaterial) -> PromptMaterial {
    PromptMaterial {
        asset_key: material.asset_key,
        asset_version: material.asset_version,
        title: material.title,
        description: material.description,
        tags: material.tags,
        category: material.category,
    }
}

fn recall_materials(
    materials: &[TransitionMaterial],
    before: &str,
    after: &str,
    streamer_tags: &[String],
) -> Vec<TransitionMaterial> {
    let context = format!("{} {} {}", before, after, streamer_tags.join(" ")).to_lowercase();
    let tokens = semantic_tokens(&context);
    let mut scored = materials
        .iter()
        .cloned()
        .map(|material| {
            let semantic = format!(
                "{} {} {} {}",
                material.title,
                material.description,
                material.tags.join(" "),
                material.category
            )
            .to_lowercase();
            let token_score = tokens
                .iter()
                .filter(|token| semantic.contains(token.as_str()))
                .count();
            let tag_score = streamer_tags
                .iter()
                .filter(|tag| semantic.contains(&tag.to_lowercase()))
                .count()
                * 8;
            (token_score + tag_score, material)
        })
        .collect::<Vec<_>>();
    scored.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then(left.1.sort_order.cmp(&right.1.sort_order))
            .then(left.1.asset_key.cmp(&right.1.asset_key))
            .then(right.1.asset_version.cmp(&left.1.asset_version))
    });
    scored
        .into_iter()
        .take(MAX_CANDIDATES)
        .map(|(_, item)| item)
        .collect()
}

fn semantic_tokens(value: &str) -> HashSet<String> {
    let chars = value
        .chars()
        .filter(|ch| !ch.is_whitespace())
        .collect::<Vec<_>>();
    chars
        .windows(2)
        .map(|window| window.iter().collect::<String>())
        .filter(|token| token.chars().any(|ch| ch.is_alphanumeric()))
        .take(96)
        .collect()
}

/// 匹配结果的容错校验结果。
struct MatchValidation {
    /// 通过校验、可以进入评分阶段的边界条目。
    accepted: Vec<TransitionAgentMatch>,
    /// 无法使用的边界数量。
    failed: usize,
    /// 首个违规原因的脱敏摘要，只包含边界 ID、素材键和计数。
    detail: Option<String>,
}

/// 评分结果的容错校验结果。
struct ScoreValidation {
    /// 通过校验的评分条目。
    accepted: Vec<TransitionAgentScore>,
    /// 首个违规原因的脱敏摘要。
    detail: Option<String>,
}

fn bounded_detail(message: String) -> String {
    message.chars().take(MAX_DIAGNOSTIC_CHARS).collect()
}

/// 逐个边界校验匹配结果。合法条目保留，违规或缺失的边界计入失败，
/// 由调用方决定是重试还是按部分成功继续。
fn validate_match_output(
    output: &TransitionAgentOutput,
    allowed: &HashMap<i64, HashSet<(String, i64)>>,
) -> MatchValidation {
    let mut accepted = Vec::new();
    let mut failed_boundaries = std::collections::BTreeSet::new();
    let mut detail: Option<String> = None;
    let note = |detail: &mut Option<String>, message: String| {
        if detail.is_none() {
            *detail = Some(bounded_detail(message));
        }
    };
    let mut seen = HashSet::new();
    for item in &output.matches {
        if !allowed.contains_key(&item.boundary_id) {
            note(
                &mut detail,
                format!("返回了输入之外的边界 {}", item.boundary_id),
            );
            continue;
        }
        if !seen.insert(item.boundary_id) {
            // 同一边界出现两条互相冲突的记录时不做取舍，撤销先前结果并按失败处理，
            // 保证一个边界只会被接受一次或计入失败。
            accepted.retain(|entry: &TransitionAgentMatch| entry.boundary_id != item.boundary_id);
            failed_boundaries.insert(item.boundary_id);
            note(&mut detail, format!("边界 {} 被重复返回", item.boundary_id));
            continue;
        }
        if item.reason.trim().is_empty() || item.reason.chars().count() > MAX_REASON_CHARS {
            failed_boundaries.insert(item.boundary_id);
            note(
                &mut detail,
                format!(
                    "边界 {} 的 reason 缺失或超过 {} 字",
                    item.boundary_id, MAX_REASON_CHARS
                ),
            );
            continue;
        }
        if item.none {
            if !item.candidates.is_empty() {
                failed_boundaries.insert(item.boundary_id);
                note(
                    &mut detail,
                    format!("边界 {} 标记为无需转场但返回了候选", item.boundary_id),
                );
                continue;
            }
            accepted.push(item.clone());
            continue;
        }
        if item.candidates.is_empty() || item.candidates.len() > MAX_MATCHED_CANDIDATES {
            failed_boundaries.insert(item.boundary_id);
            note(
                &mut detail,
                format!(
                    "边界 {} 的候选数量必须在 1 到 {} 之间",
                    item.boundary_id, MAX_MATCHED_CANDIDATES
                ),
            );
            continue;
        }
        let mut candidate_seen = HashSet::new();
        let mut offending = None;
        for candidate in &item.candidates {
            let key = (candidate.asset_key.clone(), candidate.asset_version);
            if !candidate_seen.insert(key.clone()) || !allowed[&item.boundary_id].contains(&key) {
                offending = Some(candidate.asset_key.clone());
                break;
            }
        }
        if let Some(asset_key) = offending {
            failed_boundaries.insert(item.boundary_id);
            note(
                &mut detail,
                format!(
                    "边界 {} 使用了候选集合之外的素材 {asset_key}",
                    item.boundary_id
                ),
            );
            continue;
        }
        accepted.push(item.clone());
    }
    for boundary_id in allowed.keys() {
        if !seen.contains(boundary_id) {
            failed_boundaries.insert(*boundary_id);
            note(&mut detail, format!("未返回边界 {boundary_id}"));
        }
    }
    MatchValidation {
        accepted,
        failed: failed_boundaries.len(),
        detail,
    }
}

fn build_score_prompt(
    prompt_boundaries: &[PromptBoundary],
    matches: &[TransitionAgentMatch],
) -> Result<Vec<ScorePromptBoundary>> {
    let mut result = Vec::new();
    for item in matches.iter().filter(|item| !item.none) {
        let boundary = prompt_boundaries
            .iter()
            .find(|boundary| boundary.boundary_id == item.boundary_id)
            .ok_or_else(|| {
                TransitionMatchingError::InvalidResponse(bounded_detail(format!(
                    "边界 {} 缺少召回上下文",
                    item.boundary_id
                )))
            })?;
        let mut candidates = Vec::with_capacity(item.candidates.len());
        for selected in &item.candidates {
            let material = boundary
                .candidates
                .iter()
                .find(|candidate| {
                    candidate.asset_key == selected.asset_key
                        && candidate.asset_version == selected.asset_version
                })
                .cloned()
                .ok_or_else(|| {
                    TransitionMatchingError::InvalidResponse(bounded_detail(format!(
                        "候选 {} 缺少召回信息",
                        selected.asset_key
                    )))
                })?;
            candidates.push(material);
        }
        result.push(ScorePromptBoundary {
            boundary_id: boundary.boundary_id,
            before_asr: boundary.before_asr.clone(),
            after_asr: boundary.after_asr.clone(),
            streamer_tags: boundary.streamer_tags.clone(),
            candidates,
        });
    }
    Ok(result)
}

/// 校验评分结果。合法评分保留，越界或重复的评分忽略并记录原因，
/// 缺少评分的候选由调用方按边界处理。
fn validate_score_output(
    output: &TransitionScoreOutput,
    matches: &[TransitionAgentMatch],
) -> ScoreValidation {
    let expected = matches
        .iter()
        .flat_map(|item| {
            item.candidates.iter().map(move |candidate| {
                (
                    item.boundary_id,
                    candidate.asset_key.clone(),
                    candidate.asset_version,
                )
            })
        })
        .collect::<HashSet<_>>();
    let mut accepted = Vec::new();
    let mut detail: Option<String> = None;
    let note = |detail: &mut Option<String>, message: String| {
        if detail.is_none() {
            *detail = Some(bounded_detail(message));
        }
    };
    let mut seen = HashSet::new();
    for score in &output.scores {
        let key = (
            score.boundary_id,
            score.asset_key.clone(),
            score.asset_version,
        );
        if !expected.contains(&key) {
            note(
                &mut detail,
                format!(
                    "评分引用了未匹配的候选 {}（边界 {}）",
                    score.asset_key, score.boundary_id
                ),
            );
            continue;
        }
        if !seen.insert(key) {
            note(
                &mut detail,
                format!("候选 {} 的评分被重复返回", score.asset_key),
            );
            continue;
        }
        if score.reason.trim().is_empty()
            || score.reason.chars().count() > MAX_REASON_CHARS
            || [
                score.total_score,
                score.scene_score,
                score.continuity_score,
                score.rhythm_score,
                score.material_score,
            ]
            .into_iter()
            .any(|value| !value.is_finite() || !(0.0..=10.0).contains(&value))
        {
            note(
                &mut detail,
                format!("候选 {} 的评分或 reason 不合法", score.asset_key),
            );
            continue;
        }
        accepted.push(score.clone());
    }
    ScoreValidation { accepted, detail }
}

fn best_score_for_match<'a>(
    item: &TransitionAgentMatch,
    scores: &'a [TransitionAgentScore],
) -> Result<&'a TransitionAgentScore> {
    let ranks = item
        .candidates
        .iter()
        .enumerate()
        .map(|(rank, candidate)| ((candidate.asset_key.clone(), candidate.asset_version), rank))
        .collect::<HashMap<_, _>>();
    let mut candidates = scores
        .iter()
        .filter(|score| score.boundary_id == item.boundary_id)
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        right
            .total_score
            .total_cmp(&left.total_score)
            .then(
                ranks[&(left.asset_key.clone(), left.asset_version)]
                    .cmp(&ranks[&(right.asset_key.clone(), right.asset_version)]),
            )
            .then(left.asset_key.cmp(&right.asset_key))
            .then(left.asset_version.cmp(&right.asset_version))
    });
    candidates.into_iter().next().ok_or_else(|| {
        TransitionMatchingError::InvalidResponse(bounded_detail(format!(
            "边界 {} 没有可用评分",
            item.boundary_id
        )))
    })
}

fn head_chars(value: &str, maximum: usize) -> String {
    value.chars().take(maximum).collect()
}
fn tail_chars(value: &str, maximum: usize) -> String {
    value
        .chars()
        .rev()
        .take(maximum)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}
fn bounded_reason(value: &str) -> String {
    value
        .trim()
        .chars()
        .filter(|ch| !ch.is_control())
        .take(MAX_REASON_CHARS)
        .collect()
}

fn score_meets_threshold(score: f64, threshold: u8) -> bool {
    score >= f64::from(threshold)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transition_assets::{MaterialAssetRegistry, TransitionMaterialCache};
    use async_trait::async_trait;
    use rusqlite::params;

    fn material(key: &str, title: &str, tags: &[&str], sort_order: i64) -> TransitionMaterial {
        TransitionMaterial {
            asset_key: key.to_owned(),
            asset_version: 1,
            title: title.to_owned(),
            description: format!("{title}描述"),
            tags: tags.iter().map(|v| (*v).to_owned()).collect(),
            category: "reaction".to_owned(),
            render_mode: "bridge".to_owned(),
            video_url: "https://cdn.example.com/a.mp4".to_owned(),
            preview_url: None,
            cover_url: None,
            sha256: "a".repeat(64),
            size_bytes: 1,
            duration_ms: 1000,
            width: 1920,
            height: 1080,
            fps: 30.0,
            video_codec: "h264".to_owned(),
            has_audio: true,
            sort_order,
            is_current: true,
        }
    }

    /// 建一个含两个片段的工程，reconcile 后得到 1 个边界。
    fn project_with_two_segments() -> crate::database::Database {
        let database = crate::database::Database::open_in_memory().unwrap();
        database.migrate().unwrap();
        let connection = database.connection().unwrap();
        connection
            .pragma_update(None, "foreign_keys", "OFF")
            .unwrap();
        connection
            .execute(
                r#"INSERT INTO ai_clip_projects(
                       id, highlight_run_id, name, version, export_status, export_progress,
                       created_at, updated_at
                   ) VALUES(1, 1, '转场锁定测试', 1, 'idle', 0, ?1, ?1)"#,
                ["2026-08-08T00:00:00Z"],
            )
            .unwrap();
        // candidate_id 在工程内唯一，因此两个片段使用不同候选。
        for (id, position, candidate, start, end) in [
            (11_i64, 0_i64, 1_i64, 0_i64, 5_000_i64),
            (12, 1, 2, 5_000, 10_000),
        ] {
            connection
                .execute(
                    r#"INSERT INTO ai_clip_segments(
                           id, clip_project_id, candidate_id, input_id, position, title,
                           source_start_ms, source_end_ms, created_at, updated_at
                       ) VALUES(?1, 1, ?2, 1, ?3, '片段', ?4, ?5, ?6, ?6)"#,
                    params![id, candidate, position, start, end, "2026-08-08T00:00:00Z"],
                )
                .unwrap();
        }
        connection
            .pragma_update(None, "foreign_keys", "ON")
            .unwrap();
        drop(connection);
        database
    }

    #[tokio::test]
    async fn authorization_failure_never_overwrites_a_manually_locked_boundary() {
        let database = project_with_two_segments();
        let material_repository = TransitionMaterialRepository::new(database.clone());
        let boundaries = material_repository.reconcile_boundaries(1).unwrap();
        assert_eq!(boundaries.len(), 1, "两个片段之间应有一个边界");
        let locked = material_repository
            .set_boundary_lock(boundaries[0].id, true)
            .unwrap();
        assert!(locked.manually_locked, "边界必须被人工锁定");

        let directory = tempfile::tempdir().unwrap();
        let model_call = Arc::new(
            crate::ai::ModelCallRuntime::bootstrap(
                AiRepository::new(database.clone()),
                Arc::new(RejectingTokenSource),
                chrono::Utc::now().timestamp_millis(),
            )
            .unwrap(),
        );
        let assets = TransitionMaterialAssetService::new(
            TransitionMaterialRepository::new(database.clone()),
            TransitionMaterialCache::new(directory.path().join("transition-cache")).unwrap(),
            CancellationToken::new(),
            MaterialAssetRegistry::default(),
        )
        .unwrap();
        let workflow = TransitionMatchingWorkflow::new(
            TransitionMaterialRepository::new(database.clone()),
            AiRepository::new(database.clone()),
            Arc::new(NoopTransitionProvider),
            model_call,
            assets,
        );
        // 只有被人工锁定的边界 → 没有可匹配目标；无论如何都不得改动锁定边界。
        let error = workflow
            .match_boundaries(1, None, CancellationToken::new())
            .await
            .unwrap_err();
        assert!(
            matches!(
                error,
                TransitionMatchingError::NoBoundaries | TransitionMatchingError::Credential
            ),
            "锁定边界不得进入匹配目标：{error:?}"
        );
        let after = material_repository.boundary(locked.id).unwrap();
        assert_eq!(
            after.asset_key, None,
            "授权失败/无目标时不得为锁定边界写入素材"
        );
        assert_eq!(after.selection_source, locked.selection_source);
        assert!(after.manually_locked, "人工锁定标记必须保持");
    }

    /// 任务 10.2：转场匹配/评分的提示词只含素材的公开描述字段，
    /// 不含下载地址、签名地址或本地路径（`prompt_material` 是唯一的组装入口）。
    #[test]
    fn transition_prompt_material_omits_urls_paths_and_signatures() {
        let mut value = material("known", "反应镜头", &["reaction"], 1);
        // 素材目录从服务端同步，地址字段可能很长：提示词里只允许公开描述。
        value.video_url = "https://cdn.example.com/a.mp4?sign=secret&Expires=123".to_owned();
        value.sha256 = "a".repeat(64);
        let prompt_material = prompt_material(value);
        let encoded = serde_json::to_string(&[&prompt_material]).unwrap();
        assert!(encoded.contains("known"), "必须保留素材键");
        assert!(encoded.contains("反应镜头"), "必须保留公开标题");
        for forbidden in [
            "http://", "https://", "?sign=", "Expires=", "/tmp/", "/Users/", "sha256",
        ] {
            assert!(
                !encoded.contains(forbidden),
                "素材提示词不得包含「{forbidden}」：{encoded}"
            );
        }
    }

    #[test]
    fn context_is_bounded_and_recall_is_deterministic() {
        let materials = vec![
            material("laugh", "爆笑反应", &["搞笑"], 2),
            material("sale", "下单", &["带货"], 1),
        ];
        let recalled = recall_materials(
            &materials,
            "这个真的太好笑了",
            "哈哈哈",
            &["搞笑".to_owned()],
        );
        assert_eq!(recalled[0].asset_key, "laugh");
        assert_eq!(
            head_chars(&"你".repeat(600), MAX_CONTEXT_CHARS)
                .chars()
                .count(),
            MAX_CONTEXT_CHARS
        );
        assert_eq!(
            tail_chars(&"你".repeat(600), MAX_CONTEXT_CHARS)
                .chars()
                .count(),
            MAX_CONTEXT_CHARS
        );
    }

    fn candidate(key: &str, version: i64) -> crate::ai::TransitionAgentCandidate {
        crate::ai::TransitionAgentCandidate {
            asset_key: key.to_owned(),
            asset_version: version,
        }
    }

    fn match_entry(
        boundary_id: i64,
        keys: &[&str],
        none: bool,
        reason: &str,
    ) -> TransitionAgentMatch {
        TransitionAgentMatch {
            boundary_id,
            candidates: keys.iter().map(|key| candidate(key, 1)).collect(),
            none,
            reason: reason.to_owned(),
        }
    }

    #[test]
    fn fabricated_assets_fail_only_their_own_boundary_and_prompt_omits_private_fields() {
        let allowed = HashMap::from([
            (1, HashSet::from([("known".to_owned(), 2)])),
            (
                2,
                HashSet::from([("known".to_owned(), 2), ("other".to_owned(), 2)]),
            ),
        ]);
        let output = TransitionAgentOutput {
            matches: vec![
                TransitionAgentMatch {
                    boundary_id: 1,
                    candidates: vec![candidate("invented", 2)],
                    none: false,
                    reason: "匹配".to_owned(),
                },
                TransitionAgentMatch {
                    boundary_id: 2,
                    candidates: vec![candidate("other", 2)],
                    none: false,
                    reason: "匹配".to_owned(),
                },
            ],
            token_usage: 0,
        };
        let validation = validate_match_output(&output, &allowed);
        // 越界候选只让边界 1 失败，边界 2 仍然可用。
        assert_eq!(validation.accepted.len(), 1);
        assert_eq!(validation.accepted[0].boundary_id, 2);
        assert_eq!(validation.failed, 1);
        assert!(
            validation
                .detail
                .as_deref()
                .is_some_and(|detail| detail.contains('1') && detail.contains("invented"))
        );
        let json = serde_json::to_string(&prompt_material(material("known", "标题", &["标签"], 0)))
            .unwrap();
        assert!(!json.contains("videoUrl"));
        assert!(!json.contains("sha256"));
    }

    #[test]
    fn missing_duplicate_and_unknown_boundaries_are_counted_without_rejecting_the_batch() {
        let allowed = HashMap::from([
            (1, HashSet::from([("known".to_owned(), 1)])),
            (2, HashSet::from([("known".to_owned(), 1)])),
            (3, HashSet::from([("known".to_owned(), 1)])),
        ]);
        let output = TransitionAgentOutput {
            matches: vec![
                match_entry(1, &["known"], false, "合法"),
                match_entry(2, &["known"], false, "合法"),
                match_entry(1, &["known"], false, "重复边界"),
                match_entry(99, &["known"], false, "未知边界"),
            ],
            token_usage: 0,
        };
        let validation = validate_match_output(&output, &allowed);
        // 边界 1 因重复返回而整体撤销，边界 2 合法，边界 3 未覆盖；
        // 未知边界 99 只记录原因，不计入本项目的失败边界。
        assert_eq!(validation.accepted.len(), 1);
        assert_eq!(validation.accepted[0].boundary_id, 2);
        assert_eq!(validation.failed, 2);
        assert_eq!(validation.accepted.len() + validation.failed, allowed.len());
        assert!(validation.detail.is_some());
    }

    #[test]
    fn none_entries_are_accepted_and_produce_no_candidates() {
        let allowed = HashMap::from([(1, HashSet::from([("known".to_owned(), 1)]))]);
        let output = TransitionAgentOutput {
            matches: vec![match_entry(1, &[], true, "本段无需转场")],
            token_usage: 0,
        };
        let validation = validate_match_output(&output, &allowed);
        assert_eq!(validation.accepted.len(), 1);
        assert_eq!(validation.failed, 0);
        assert!(validation.detail.is_none());

        let invalid = TransitionAgentOutput {
            matches: vec![match_entry(1, &["known"], true, "带候选的无转场")],
            token_usage: 0,
        };
        let validation = validate_match_output(&invalid, &allowed);
        assert!(validation.accepted.is_empty());
        assert_eq!(validation.failed, 1);
    }

    #[test]
    fn oversized_candidate_lists_and_long_reasons_fail_their_boundary() {
        let allowed = HashMap::from([(
            1,
            HashSet::from([
                ("a".to_owned(), 1),
                ("b".to_owned(), 1),
                ("c".to_owned(), 1),
                ("d".to_owned(), 1),
            ]),
        )]);
        let too_many = TransitionAgentOutput {
            matches: vec![match_entry(1, &["a", "b", "c", "d"], false, "超过候选上限")],
            token_usage: 0,
        };
        assert_eq!(validate_match_output(&too_many, &allowed).failed, 1);

        let long_reason = TransitionAgentOutput {
            matches: vec![match_entry(
                1,
                &["a"],
                false,
                &"长".repeat(MAX_REASON_CHARS + 1),
            )],
            token_usage: 0,
        };
        let validation = validate_match_output(&long_reason, &allowed);
        assert!(validation.accepted.is_empty());
        assert_eq!(validation.failed, 1);
    }

    #[test]
    fn scoring_requires_complete_candidates_and_prefers_match_rank_on_ties() {
        let matched = vec![TransitionAgentMatch {
            boundary_id: 1,
            candidates: vec![candidate("first", 1), candidate("second", 1)],
            none: false,
            reason: "两个候选".to_owned(),
        }];
        let score = |key: &str| TransitionAgentScore {
            boundary_id: 1,
            asset_key: key.to_owned(),
            asset_version: 1,
            total_score: 8.0,
            scene_score: 8.0,
            continuity_score: 8.0,
            rhythm_score: 8.0,
            material_score: 8.0,
            reason: "评分合法".to_owned(),
        };
        let output = TransitionScoreOutput {
            scores: vec![score("second"), score("first")],
            token_usage: 12,
        };
        let validated = validate_score_output(&output, &matched);
        assert_eq!(validated.accepted.len(), 2);
        assert!(validated.detail.is_none());
        assert_eq!(
            best_score_for_match(&matched[0], &validated.accepted)
                .unwrap()
                .asset_key,
            "first"
        );
        // 部分覆盖不再整批失败，缺失的候选会在应用阶段让该边界计入失败边界。
        let partial = validate_score_output(
            &TransitionScoreOutput {
                scores: vec![score("first")],
                token_usage: 0,
            },
            &matched,
        );
        assert_eq!(partial.accepted.len(), 1);
        let stray = validate_score_output(
            &TransitionScoreOutput {
                scores: vec![score("invented")],
                token_usage: 0,
            },
            &matched,
        );
        assert!(stray.accepted.is_empty());
        assert!(
            stray
                .detail
                .as_deref()
                .is_some_and(|detail| detail.contains("invented"))
        );
    }

    // ---- 统一调用点迁移后的新语义（任务 5.1 / 5.3 / 5.4）----

    /// 依次发放不同 token 的来源，用于证明"重取"确实发生。
    struct RotatingTokenSource {
        issued: std::sync::atomic::AtomicUsize,
    }

    #[async_trait]
    impl crate::ai::LlmTokenSource for RotatingTokenSource {
        async fn fetch(&self) -> std::result::Result<crate::ai::LlmTokenLease, LlmError> {
            let index = self
                .issued
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(crate::ai::LlmTokenLease {
                token: format!("st-lt-v1.lease-{index}"),
                expire_at: chrono::Utc::now().timestamp() + 3_600,
                provider: "ALI".to_owned(),
            })
        }
    }

    /// 总是拒绝的来源，用于授权失败路径。
    struct RejectingTokenSource;

    #[async_trait]
    impl crate::ai::LlmTokenSource for RejectingTokenSource {
        async fn fetch(&self) -> std::result::Result<crate::ai::LlmTokenLease, LlmError> {
            Err(LlmError::Credential)
        }
    }

    struct NoopTransitionProvider;

    #[async_trait]
    impl TransitionAgentProvider for NoopTransitionProvider {
        async fn match_transitions(
            &self,
            _call: &crate::ai::ModelCallHandle,
            _request: TransitionAgentRequest,
            _cancellation: CancellationToken,
        ) -> std::result::Result<TransitionAgentOutput, LlmError> {
            Ok(TransitionAgentOutput {
                matches: Vec::new(),
                token_usage: 0,
            })
        }

        async fn score_transitions(
            &self,
            _call: &crate::ai::ModelCallHandle,
            _request: TransitionScoreRequest,
            _cancellation: CancellationToken,
        ) -> std::result::Result<TransitionScoreOutput, LlmError> {
            Ok(TransitionScoreOutput {
                scores: Vec::new(),
                token_usage: 0,
            })
        }
    }

    fn test_workflow(
        token_source: Arc<dyn crate::ai::LlmTokenSource>,
    ) -> (TransitionMatchingWorkflow, tempfile::TempDir) {
        let directory = tempfile::tempdir().unwrap();
        let database = crate::database::Database::open_in_memory().unwrap();
        database.migrate().unwrap();
        let model_call = Arc::new(
            crate::ai::ModelCallRuntime::bootstrap(
                AiRepository::new(database.clone()),
                token_source,
                chrono::Utc::now().timestamp_millis(),
            )
            .unwrap(),
        );
        let assets = TransitionMaterialAssetService::new(
            TransitionMaterialRepository::new(database.clone()),
            TransitionMaterialCache::new(directory.path().join("transition-cache")).unwrap(),
            CancellationToken::new(),
            MaterialAssetRegistry::default(),
        )
        .unwrap();
        let workflow = TransitionMatchingWorkflow::new(
            TransitionMaterialRepository::new(database.clone()),
            AiRepository::new(database),
            Arc::new(NoopTransitionProvider),
            model_call,
            assets,
        );
        (workflow, directory)
    }

    #[test]
    fn credential_errors_are_distinguishable_from_provider_errors() {
        assert!(matches!(
            TransitionMatchingWorkflow::map_llm_error(LlmError::Credential),
            TransitionMatchingError::Credential
        ));
        assert!(matches!(
            TransitionMatchingWorkflow::map_llm_error(LlmError::Temporary),
            TransitionMatchingError::Llm(LlmError::Temporary)
        ));
        assert!(matches!(
            TransitionMatchingWorkflow::map_llm_error(LlmError::Provider),
            TransitionMatchingError::Llm(LlmError::Provider)
        ));
        // 授权原因不得被描述成 Provider 网络错误。
        assert_ne!(
            TransitionMatchingError::Credential.to_string(),
            TransitionMatchingError::Llm(LlmError::Temporary).to_string()
        );
    }

    #[tokio::test]
    async fn credential_failure_refreshes_the_token_once_and_retries_the_call() {
        let (workflow, _directory) = test_workflow(Arc::new(RotatingTokenSource {
            issued: std::sync::atomic::AtomicUsize::new(0),
        }));
        let frozen = workflow.model_call.freeze_for_run().unwrap();
        let seen = std::sync::Mutex::new(Vec::new());
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let outcome: std::result::Result<(), TransitionMatchingError> = workflow
            .call_with_token_refresh(
                &frozen,
                |handle| {
                    let seen = &seen;
                    let calls = &calls;
                    async move {
                        seen.lock().unwrap().push(handle.token.token.clone());
                        if calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                            return Err(LlmError::Credential);
                        }
                        Ok(())
                    }
                },
                CancellationToken::new(),
            )
            .await;
        outcome.expect("重取 token 后必须成功");
        let tokens = seen.lock().unwrap().clone();
        assert_eq!(tokens.len(), 2, "凭据失败必须重取一次并重试");
        assert_ne!(tokens[0], tokens[1], "重试必须使用重取后的新 token");
        // 两个 Agent 阶段共用同一冻结模型：模型来自快照而不是各阶段现读设置。
        assert_eq!(frozen.model_id(), "qwen-plus");
    }

    #[tokio::test]
    async fn non_credential_failures_are_not_retried() {
        let (workflow, _directory) = test_workflow(Arc::new(RotatingTokenSource {
            issued: std::sync::atomic::AtomicUsize::new(0),
        }));
        let frozen = workflow.model_call.freeze_for_run().unwrap();
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let outcome: std::result::Result<(), TransitionMatchingError> = workflow
            .call_with_token_refresh(
                &frozen,
                |_handle| {
                    let calls = &calls;
                    async move {
                        calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        Err(LlmError::Temporary)
                    }
                },
                CancellationToken::new(),
            )
            .await;
        assert!(matches!(
            outcome.unwrap_err(),
            TransitionMatchingError::Llm(LlmError::Temporary)
        ));
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "非凭据失败不得重试，避免把限流/网络错误掩盖成重试"
        );
    }

    #[tokio::test]
    async fn rejecting_token_source_surfaces_a_distinguishable_credential_error() {
        let (workflow, _directory) = test_workflow(Arc::new(RejectingTokenSource));
        let frozen = workflow.model_call.freeze_for_run().unwrap();
        let outcome: std::result::Result<(), TransitionMatchingError> = workflow
            .call_with_token_refresh(
                &frozen,
                |_handle| async move { Ok(()) },
                CancellationToken::new(),
            )
            .await;
        assert!(matches!(
            outcome.unwrap_err(),
            TransitionMatchingError::Credential
        ));
    }

    #[test]
    fn auto_apply_includes_scores_equal_to_the_frozen_threshold() {
        assert!(!score_meets_threshold(7.99, 8));
        assert!(score_meets_threshold(8.0, 8));
        assert!(score_meets_threshold(9.5, 8));
    }
}
