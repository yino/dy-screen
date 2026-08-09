//! 受限转场匹配 Agent：只使用本地目录语义与相邻工程字幕。

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::ai::{
    AiRepository, CredentialStore, LlmError, LlmProviderSettings, TransitionAgentMatch,
    TransitionAgentOutput, TransitionAgentProvider, TransitionAgentRequest, TransitionAgentScore,
    TransitionScoreOutput, TransitionScoreRequest,
};
use crate::clip_workflow::{
    ClipWorkflowProgress, ClipWorkflowPublisher, SilentClipWorkflowPublisher,
};
use crate::transition_assets::TransitionMaterialAssetService;
use crate::transition_materials::{
    BoundaryAgentScoreInput, ClipTransitionBoundary, TransitionMatchRunOutcome, TransitionMaterial,
    TransitionMaterialError, TransitionMaterialRepository,
};

pub const TRANSITION_MATCH_PROMPT_VERSION: &str = "transition-match-v2";
pub const TRANSITION_SCORE_PROMPT_VERSION: &str = "transition-score-v1";
const MAX_CONTEXT_CHARS: usize = 480;
const MAX_CANDIDATES: usize = 12;
const MAX_MATCHED_CANDIDATES: usize = 3;
const MAX_REASON_CHARS: usize = 160;

#[derive(Debug, Error)]
pub enum TransitionMatchingError {
    #[error("当前工程没有可匹配的转场位置")]
    NoBoundaries,
    #[error("本地转场素材目录为空，请先同步素材")]
    EmptyCatalog,
    #[error("DeepSeek Key 尚未配置")]
    Credential,
    #[error("Agent 返回了无效的转场匹配结果")]
    InvalidResponse,
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
    credentials: Arc<dyn CredentialStore>,
    assets: TransitionMaterialAssetService,
    publisher: Arc<dyn ClipWorkflowPublisher>,
}

impl TransitionMatchingWorkflow {
    pub fn new(
        repository: TransitionMaterialRepository,
        ai_repository: AiRepository,
        provider: Arc<dyn TransitionAgentProvider>,
        credentials: Arc<dyn CredentialStore>,
        assets: TransitionMaterialAssetService,
    ) -> Self {
        Self {
            repository,
            ai_repository,
            provider,
            credentials,
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
        let (settings, api_key) = self.provider_context()?;
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
        let match_prompt = format!(
            "为每个边界从 candidates 中按场景、语义和情绪适配度选择最多 {MAX_MATCHED_CANDIDATES} 个有序桥接视频候选。无法确定时返回 none=true 且 candidates 为空；否则 none=false。必须完整覆盖所有边界，reason 不超过 {MAX_REASON_CHARS} 个字符，不得返回候选之外的键或任何渲染参数。输入：{payload}"
        );
        let fingerprint = hex::encode(Sha256::digest(match_prompt.as_bytes()));
        let run_id = self.repository.begin_match_run(
            clip_project_id,
            project_version,
            "deepseek",
            &settings.model_id,
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
        let output = match self
            .provider
            .match_transitions(
                &settings,
                &api_key,
                TransitionAgentRequest {
                    prompt: match_prompt,
                },
                cancellation.clone(),
            )
            .await
            .map_err(TransitionMatchingError::from)
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
        outcome.token_usage = output.token_usage;
        let matched = match validate_match_output(&output, &allowed) {
            Ok(value) => value,
            Err(error) => {
                let _ = self.repository.finish_match_run(
                    run_id,
                    false,
                    "matching_failed",
                    outcome,
                    Some(("transition_agent_invalid_response", &error.to_string())),
                );
                return Err(error);
            }
        };
        outcome.matched_boundaries = matched.len();
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
        let scored_candidates = if score_boundaries.is_empty() {
            Vec::new()
        } else {
            let score_payload = serde_json::to_string(&score_boundaries)
                .map_err(|_| TransitionMatchingError::Repository)?;
            let score_prompt = format!(
                "逐个评价输入中的全部候选。每个候选都必须返回 0 到 10 的 totalScore、sceneScore、continuityScore、rhythmScore、materialScore 和不超过 {MAX_REASON_CHARS} 个字符的 reason；必须完整覆盖候选集合，不得新增、删除或替换素材。输入：{score_payload}"
            );
            let score_output = match self
                .provider
                .score_transitions(
                    &settings,
                    &api_key,
                    TransitionScoreRequest {
                        prompt: score_prompt,
                    },
                    cancellation.clone(),
                )
                .await
                .map_err(TransitionMatchingError::from)
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
            match validate_score_output(&score_output, &matched) {
                Ok(scores) => scores,
                Err(error) => {
                    let _ = self.repository.finish_match_run(
                        run_id,
                        false,
                        "scoring_failed",
                        outcome,
                        Some(("transition_score_invalid_response", &error.to_string())),
                    );
                    return Err(error);
                }
            }
        };

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
            matched.len(),
            "正在按阈值应用结果",
        );

        let mut auto_downloads = Vec::new();
        for item in &matched {
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
            let best = best_score_for_match(item, &scored_candidates)?;
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
        self.publish(
            clip_project_id,
            run_id,
            "completed",
            outcome.matched_boundaries,
            outcome.matched_boundaries,
            "转场 Agent 已完成",
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

    fn provider_context(&self) -> Result<(LlmProviderSettings, String)> {
        let api_key = self
            .credentials
            .get()
            .map_err(|_| TransitionMatchingError::Credential)?
            .filter(|value| !value.trim().is_empty())
            .ok_or(TransitionMatchingError::Credential)?;
        let settings = self
            .ai_repository
            .get_llm_provider_settings(true)
            .map_err(|_| TransitionMatchingError::Repository)?;
        Ok((settings, api_key))
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
        let json = connection
            .query_row(
                r#"SELECT run.tags_snapshot_json FROM ai_clip_projects project
               JOIN ai_highlight_runs run ON run.id = project.highlight_run_id
               WHERE project.id = ?1"#,
                [clip_project_id],
                |row| row.get::<_, String>(0),
            )
            .map_err(|_| TransitionMatchingError::Repository)?;
        serde_json::from_str(&json).map_err(|_| TransitionMatchingError::Repository)
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

fn validate_match_output(
    output: &TransitionAgentOutput,
    allowed: &HashMap<i64, HashSet<(String, i64)>>,
) -> Result<Vec<TransitionAgentMatch>> {
    if output.matches.len() != allowed.len() {
        return Err(TransitionMatchingError::InvalidResponse);
    }
    let mut seen = HashSet::new();
    for item in &output.matches {
        if !seen.insert(item.boundary_id)
            || !allowed.contains_key(&item.boundary_id)
            || item.reason.trim().is_empty()
            || item.reason.chars().count() > MAX_REASON_CHARS
        {
            return Err(TransitionMatchingError::InvalidResponse);
        }
        if item.none {
            if !item.candidates.is_empty() {
                return Err(TransitionMatchingError::InvalidResponse);
            }
            continue;
        }
        if item.candidates.is_empty() || item.candidates.len() > MAX_MATCHED_CANDIDATES {
            return Err(TransitionMatchingError::InvalidResponse);
        }
        let mut candidate_seen = HashSet::new();
        for candidate in &item.candidates {
            let key = (candidate.asset_key.clone(), candidate.asset_version);
            if !candidate_seen.insert(key.clone()) || !allowed[&item.boundary_id].contains(&key) {
                return Err(TransitionMatchingError::InvalidResponse);
            }
        }
    }
    Ok(output.matches.clone())
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
            .ok_or(TransitionMatchingError::InvalidResponse)?;
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
                .ok_or(TransitionMatchingError::InvalidResponse)?;
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

fn validate_score_output(
    output: &TransitionScoreOutput,
    matches: &[TransitionAgentMatch],
) -> Result<Vec<TransitionAgentScore>> {
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
    if output.scores.len() != expected.len() {
        return Err(TransitionMatchingError::InvalidResponse);
    }
    let mut seen = HashSet::new();
    for score in &output.scores {
        let key = (
            score.boundary_id,
            score.asset_key.clone(),
            score.asset_version,
        );
        if !seen.insert(key.clone())
            || !expected.contains(&key)
            || score.reason.trim().is_empty()
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
            return Err(TransitionMatchingError::InvalidResponse);
        }
    }
    Ok(output.scores.clone())
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
    candidates
        .into_iter()
        .next()
        .ok_or(TransitionMatchingError::InvalidResponse)
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

    #[test]
    fn response_rejects_fabricated_assets_and_prompt_omits_private_fields() {
        let allowed = HashMap::from([(1, HashSet::from([("known".to_owned(), 2)]))]);
        let output = TransitionAgentOutput {
            matches: vec![crate::ai::TransitionAgentMatch {
                boundary_id: 1,
                candidates: vec![crate::ai::TransitionAgentCandidate {
                    asset_key: "invented".to_owned(),
                    asset_version: 2,
                }],
                none: false,
                reason: "匹配".to_owned(),
            }],
            token_usage: 0,
        };
        assert!(validate_match_output(&output, &allowed).is_err());
        let json = serde_json::to_string(&prompt_material(material("known", "标题", &["标签"], 0)))
            .unwrap();
        assert!(!json.contains("videoUrl"));
        assert!(!json.contains("sha256"));
    }

    #[test]
    fn scoring_requires_complete_candidates_and_prefers_match_rank_on_ties() {
        let matched = vec![TransitionAgentMatch {
            boundary_id: 1,
            candidates: vec![
                crate::ai::TransitionAgentCandidate {
                    asset_key: "first".to_owned(),
                    asset_version: 1,
                },
                crate::ai::TransitionAgentCandidate {
                    asset_key: "second".to_owned(),
                    asset_version: 1,
                },
            ],
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
        let validated = validate_score_output(&output, &matched).unwrap();
        assert_eq!(
            best_score_for_match(&matched[0], &validated)
                .unwrap()
                .asset_key,
            "first"
        );
        assert!(
            validate_score_output(
                &TransitionScoreOutput {
                    scores: vec![score("first")],
                    token_usage: 0,
                },
                &matched,
            )
            .is_err()
        );
    }

    #[test]
    fn auto_apply_includes_scores_equal_to_the_frozen_threshold() {
        assert!(!score_meets_threshold(7.99, 8));
        assert!(score_meets_threshold(8.0, 8));
        assert!(score_meets_threshold(9.5, 8));
    }
}
