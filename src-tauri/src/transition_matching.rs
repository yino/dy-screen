//! 受限转场匹配 Agent：只使用本地目录语义与相邻工程字幕。

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::ai::{
    AiRepository, CredentialStore, LlmError, TransitionAgentOutput, TransitionAgentProvider,
    TransitionAgentRequest,
};
use crate::transition_assets::TransitionMaterialAssetService;
use crate::transition_materials::{
    ClipTransitionBoundary, TransitionMaterial, TransitionMaterialError,
    TransitionMaterialRepository,
};

pub const TRANSITION_AUTO_APPLY_CONFIDENCE: f64 = 0.75;
const MAX_CONTEXT_CHARS: usize = 480;
const MAX_CANDIDATES: usize = 12;
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
    pub matched: usize,
    pub auto_applied: usize,
    pub suggestions: usize,
    pub none_suggestions: usize,
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

#[derive(Clone)]
pub struct TransitionMatchingWorkflow {
    repository: TransitionMaterialRepository,
    ai_repository: AiRepository,
    provider: Arc<dyn TransitionAgentProvider>,
    credentials: Arc<dyn CredentialStore>,
    assets: TransitionMaterialAssetService,
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
        }
    }

    pub async fn match_boundaries(
        &self,
        clip_project_id: i64,
        target_boundary_id: Option<i64>,
        cancellation: CancellationToken,
    ) -> Result<TransitionMatchSummary> {
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
        let prompt = format!(
            "为每个边界从其 candidates 中选择最符合相邻文案语义和情绪的桥接视频。无法确定时返回 none=true。confidence 必须为 0 到 1，reason 不超过 80 个汉字。不得返回候选之外的键或任何渲染参数。输入：{payload}"
        );
        let fingerprint = hex::encode(Sha256::digest(prompt.as_bytes()));
        let run_id = self
            .repository
            .begin_match_run(clip_project_id, "deepseek", &fingerprint)?;
        let output = match self.call_provider(prompt, cancellation).await {
            Ok(output) => output,
            Err(error) => {
                let _ = self.repository.finish_match_run(
                    run_id,
                    false,
                    Some(("transition_agent_failed", &error.to_string())),
                );
                return Err(error);
            }
        };
        let validated = match validate_output(&output, &allowed) {
            Ok(value) => value,
            Err(error) => {
                let _ = self.repository.finish_match_run(
                    run_id,
                    false,
                    Some(("transition_agent_invalid_response", &error.to_string())),
                );
                return Err(error);
            }
        };

        let mut auto_downloads = Vec::new();
        let mut auto_applied = 0;
        let mut suggestions = 0;
        let mut none_suggestions = 0;
        for item in validated {
            let asset = item.asset_key.as_deref().zip(item.asset_version);
            let auto_apply = !item.none && item.confidence >= TRANSITION_AUTO_APPLY_CONFIDENCE;
            self.repository.save_agent_suggestion(
                item.boundary_id,
                asset,
                item.none,
                item.confidence,
                &bounded_reason(&item.reason),
                auto_apply,
            )?;
            if auto_apply {
                auto_applied += 1;
                if let Some((key, version)) = asset {
                    auto_downloads.push((key.to_owned(), version));
                }
            } else if item.none {
                none_suggestions += 1;
            } else {
                suggestions += 1;
            }
        }
        self.repository.finish_match_run(run_id, true, None)?;
        for (asset_key, asset_version) in auto_downloads {
            let assets = self.assets.clone();
            tauri::async_runtime::spawn(async move {
                let _ = assets.download_source(&asset_key, asset_version).await;
            });
        }
        Ok(TransitionMatchSummary {
            matched: auto_applied + suggestions + none_suggestions,
            auto_applied,
            suggestions,
            none_suggestions,
            boundaries: self.repository.list_boundaries(clip_project_id)?,
        })
    }

    async fn call_provider(
        &self,
        prompt: String,
        cancellation: CancellationToken,
    ) -> Result<TransitionAgentOutput> {
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
        self.provider
            .match_transitions(
                &settings,
                &api_key,
                TransitionAgentRequest { prompt },
                cancellation,
            )
            .await
            .map_err(TransitionMatchingError::from)
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

fn validate_output(
    output: &TransitionAgentOutput,
    allowed: &HashMap<i64, HashSet<(String, i64)>>,
) -> Result<Vec<crate::ai::TransitionAgentMatch>> {
    let mut seen = HashSet::new();
    for item in &output.matches {
        if !seen.insert(item.boundary_id)
            || !allowed.contains_key(&item.boundary_id)
            || !item.confidence.is_finite()
            || !(0.0..=1.0).contains(&item.confidence)
            || item.reason.trim().is_empty()
            || item.reason.chars().count() > MAX_REASON_CHARS
        {
            return Err(TransitionMatchingError::InvalidResponse);
        }
        match (item.none, item.asset_key.as_ref(), item.asset_version) {
            (true, None, None) => {}
            (false, Some(key), Some(version))
                if allowed[&item.boundary_id].contains(&(key.clone(), version)) => {}
            _ => return Err(TransitionMatchingError::InvalidResponse),
        }
    }
    Ok(output.matches.clone())
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
                asset_key: Some("invented".to_owned()),
                asset_version: Some(2),
                none: false,
                confidence: 0.9,
                reason: "匹配".to_owned(),
            }],
            token_usage: 0,
        };
        assert!(validate_output(&output, &allowed).is_err());
        let json = serde_json::to_string(&prompt_material(material("known", "标题", &["标签"], 0)))
            .unwrap();
        assert!(!json.contains("videoUrl"));
        assert!(!json.contains("sha256"));
    }
}
