//! AI 项目、输入、ASR 产物和稳定句段的 SQLite 事务边界。
//!
//! 原子发布、状态迁移和引用清理都在此层维护，且永不删除原始媒体或随包模型。

use std::collections::HashMap;

use chrono::Utc;
use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};
use thiserror::Error;
use unicode_segmentation::UnicodeSegmentation;

use crate::database::{Database, DatabaseError};

use super::domain::{
    AiArtifactStatus, AiClipEffect, AiClipExportStatus, AiClipProject, AiClipProjectDetail,
    AiClipProjectSource, AiClipSegment, AiClipSegmentUpdate, AiClipSubtitle, AiClipSubtitleUpdate,
    AiClipTimelineUnit, AiClipTimelineUnitKind, AiEffectCaptionRenderAttempt, AiEnhancementKind,
    AiEnhancementReviewItem, AiEnhancementStatus, AiGeneratedAnimationAsset,
    AiGeneratedAnimationComposition, AiGeneratedAnimationInsertBinding,
    AiGeneratedAnimationReplaceRange, AiGeneratedAnimationStatus, AiHighlightCandidate,
    AiHighlightCandidatePage, AiHighlightChunk, AiHighlightEnhancement, AiHighlightProgress,
    AiHighlightRun, AiHighlightRunStatus, AiInputSourceKind, AiInputStatus, AiProject,
    AiProjectDetail, AiProjectInput, AiProjectStatus, AiRenderAttemptStatus, AiSmartClipSourceInput,
    AiSmartDraft, AiSmartDraftOwnership, AiSmartDraftStatus, AsrArtifact,
    NewAiEffectCaptionRenderAttempt, NewAiGeneratedAnimationAsset, NewAiHighlightChunk,
    NewAiHighlightEnhancement, NewAiHighlightRun, NewAiProjectInput, NewAsrArtifact,
    RecognitionProfile, RecoverySummary, SourceFingerprint, TranscriptSegment,
    TranscriptSegmentDraft, GENERATED_ANIMATION_CACHE_DIR, enhancement_review_items,
    generated_animation_file_name, insert_binding_is_valid,
};

use super::llm::{HighlightCandidateDraft, HighlightCandidateScore, LlmProviderSettings};

#[derive(Debug, Error)]
pub enum AiRepositoryError {
    #[error(transparent)]
    Database(#[from] DatabaseError),
    #[error("数据库错误：{0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("找不到记录：{0}")]
    NotFound(&'static str),
    #[error("当前项目不是可修改的草稿状态")]
    ProjectNotDraft,
    #[error("项目至少需要一个有效视频输入")]
    EmptyProject,
    #[error("输入文件已存在于当前项目")]
    DuplicateInput,
    #[error("状态转换无效：{0}")]
    InvalidState(String),
    #[error("数据完整性错误：{0}")]
    Integrity(String),
    #[error("序列化错误：{0}")]
    Serialization(String),
    #[error("剪辑工程版本已更新，请重新加载后再编辑字幕")]
    ClipVersionConflict,
    #[error("剪辑工程正在导出，暂时不能修改字幕")]
    ClipExportInProgress,
    #[error("字幕内容无效：{0}")]
    InvalidClipSubtitle(String),
    #[error("同一幂等键已用于不同输入，拒绝复用")]
    IdempotencyConflict,
}

/// 本地付费任务幂等记录；付费提交前先落盘，用于重启后找回同一上游任务。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteJobRecord {
    /// 稳定请求标识。
    pub request_id: String,
    /// 任务类型，例如 `asr`。
    pub task_type: String,
    /// 输入指纹。
    pub input_hash: String,
    /// 上游任务标识；未取到时为空。
    pub upstream_task_id: Option<String>,
    /// 记录状态。
    pub status: String,
    /// 是否需要向用户确认上游是否已受理。
    pub reconcile_required: bool,
}

/// 幂等键的三元组（账号维度在本机即当前安装）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteJobKey {
    /// 稳定请求标识。
    pub request_id: String,
    /// 任务类型。
    pub task_type: String,
    /// 输入指纹。
    pub input_hash: String,
}

pub type Result<T> = std::result::Result<T, AiRepositoryError>;

#[derive(Debug, Clone)]
pub struct ClipExportSource {
    pub segment: AiClipSegment,
    pub source_path: String,
    pub source_fingerprint: SourceFingerprint,
    pub subtitles: Vec<AiClipSubtitle>,
    pub bridge_after: Option<ClipExportBridge>,
    /// `replace` 生成式动画在导出计划中的输入；`None` 表示该片段无画面替换。
    pub replace: Option<ClipExportReplace>,
    /// `insert` 生成式动画在导出计划中的输入；`None` 表示该片段后无桥接单元。
    pub insert_after: Option<ClipExportInsert>,
}

/// `insert` 合成方式在导出计划中的输入（步骤 4 任务 5.3）。
///
/// 该单元增加工程总时长并归属左侧片段；生成音轨一律丢弃，改用 48 kHz 双声道静音
/// 补齐（决策二）。邻接绑定使用稳定 ID 三元组，任一改变即失效。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipExportInsert {
    /// 已校验的生成资产文件路径。
    pub asset_path: String,
    /// 资产实际时长毫秒。
    pub duration_ms: u64,
    /// 归属的左侧片段稳定 ID。
    pub left_segment_id: i64,
    /// 右侧片段稳定 ID（必须有效，否则拒绝 `insert`）。
    pub right_segment_id: i64,
    /// 边界 ID。
    pub boundary_id: i64,
}

/// `replace` 合成方式在导出计划中的输入（步骤 4 任务 4.3）。
///
/// 目标范围是**片段内相对时间**（相对该片段的 `source_start_ms`）：由 repository
/// 依据生成资产的绑定与片段来源区间换算后填入，导出侧不解析模型时间戳。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipExportReplace {
    /// 已校验的生成资产文件路径。
    pub asset_path: String,
    /// 片段内相对起点（毫秒）。
    pub start_ms: u64,
    /// 片段内相对终点（毫秒）。
    pub end_ms: u64,
}

#[derive(Debug, Clone)]
pub struct ClipExportBridge {
    pub boundary_id: i64,
    pub asset_key: String,
    pub asset_version: i64,
    pub source_path: String,
    pub duration_ms: u64,
    pub has_audio: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipTextCorrectionTarget {
    pub subtitle_id: i64,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipTextCorrectionPlan {
    pub project_version: u32,
    pub targets: Vec<ClipTextCorrectionTarget>,
    pub skipped_manual: usize,
    pub skipped_hidden: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipTextCorrectionUpdate {
    pub subtitle_id: i64,
    pub expected_text: String,
    pub corrected_text: String,
}

pub fn project_clip_subtitles(
    clip_segments: &[AiClipSegment],
    transcripts_by_input: &HashMap<i64, Vec<TranscriptSegment>>,
) -> (Vec<AiClipSubtitle>, bool) {
    let mut project_cursor_ms = 0_u64;
    let mut subtitles = Vec::new();
    let mut complete = !clip_segments.is_empty();

    for clip in clip_segments {
        let mut clip_subtitle_count = 0_usize;
        let mut transcript_segments = transcripts_by_input
            .get(&clip.input_id)
            .into_iter()
            .flatten()
            .filter(|segment| {
                !segment.normalized_text.trim().is_empty()
                    && segment.source_start_ms < clip.source_end_ms
                    && segment.source_end_ms > clip.source_start_ms
            })
            .collect::<Vec<_>>();
        transcript_segments.sort_by(|left, right| {
            left.source_start_ms
                .cmp(&right.source_start_ms)
                .then(left.ordinal.cmp(&right.ordinal))
                .then(left.id.cmp(&right.id))
        });

        for transcript in transcript_segments {
            let source_start_ms = transcript.source_start_ms.max(clip.source_start_ms);
            let source_end_ms = transcript.source_end_ms.min(clip.source_end_ms);
            if source_end_ms <= source_start_ms {
                continue;
            }
            subtitles.push(AiClipSubtitle {
                id: 0,
                clip_project_id: clip.clip_project_id,
                stable_segment_id: transcript.id.clone(),
                clip_segment_id: clip.id,
                input_id: clip.input_id,
                original_text: transcript.normalized_text.trim().to_owned(),
                text: transcript.normalized_text.trim().to_owned(),
                hidden: false,
                source_start_ms,
                source_end_ms,
                project_start_ms: project_cursor_ms
                    .saturating_add(source_start_ms.saturating_sub(clip.source_start_ms)),
                project_end_ms: project_cursor_ms
                    .saturating_add(source_end_ms.saturating_sub(clip.source_start_ms)),
            });
            clip_subtitle_count += 1;
        }
        if clip_subtitle_count == 0 {
            complete = false;
        }
        project_cursor_ms = project_cursor_ms
            .saturating_add(clip.source_end_ms.saturating_sub(clip.source_start_ms));
    }
    (subtitles, complete)
}

fn seed_clip_subtitle_snapshots(
    transaction: &rusqlite::Transaction<'_>,
    clip_project_id: i64,
    clip_segment_id: Option<i64>,
) -> Result<()> {
    let now = Utc::now().to_rfc3339();
    transaction.execute(
        r#"INSERT OR IGNORE INTO ai_clip_subtitles(
               clip_project_id, clip_segment_id, input_id, stable_segment_id,
               source_start_ms, source_end_ms, original_text, text, hidden,
               created_at, updated_at
           )
           SELECT s.clip_project_id, s.id, s.input_id, ts.id,
                  MAX(ts.source_start_ms, s.source_start_ms),
                  MIN(ts.source_end_ms, s.source_end_ms),
                  trim(ts.normalized_text),
                  -- 片段级前置：合辑字幕优先取**句段级纠正文本**（v32）。这样校正
                  -- 发生在拼合辑之前，且不修改 ASR 原始转写。
                  COALESCE(NULLIF(trim(c.corrected_text), ''), trim(ts.normalized_text)),
                  0, ?3, ?3
           FROM ai_clip_segments s
           JOIN ai_project_inputs input ON input.id = s.input_id
           JOIN asr_artifacts artifact ON artifact.id = input.artifact_id
           JOIN transcript_segments ts ON ts.artifact_id = artifact.id
           LEFT JOIN transcript_segment_corrections c ON c.segment_id = ts.id
           WHERE s.clip_project_id = ?1
             AND (?2 IS NULL OR s.id = ?2)
             AND artifact.status = 'published'
             AND length(trim(ts.normalized_text)) > 0
             AND ts.source_start_ms < s.source_end_ms
             AND ts.source_end_ms > s.source_start_ms
             AND NOT EXISTS (
                 SELECT 1 FROM ai_clip_subtitles existing
                 WHERE existing.clip_segment_id = s.id
             )"#,
        params![clip_project_id, clip_segment_id, now],
    )?;
    Ok(())
}

fn normalize_clip_subtitle_text(value: &str) -> Result<String> {
    if value
        .chars()
        .any(|character| character.is_control() && !character.is_whitespace())
    {
        return Err(AiRepositoryError::InvalidClipSubtitle(
            "不能包含控制字符".to_owned(),
        ));
    }
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() {
        return Err(AiRepositoryError::InvalidClipSubtitle(
            "文本不能为空；不需要显示时请使用隐藏字幕".to_owned(),
        ));
    }
    if normalized.graphemes(true).count() > 500 {
        return Err(AiRepositoryError::InvalidClipSubtitle(
            "文本不能超过 500 个字符".to_owned(),
        ));
    }
    Ok(normalized)
}

fn ensure_clip_project_editable(
    transaction: &rusqlite::Transaction<'_>,
    clip_project_id: i64,
) -> Result<u32> {
    let (status, version) = transaction
        .query_row(
            "SELECT export_status, version FROM ai_clip_projects WHERE id = ?1",
            [clip_project_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
        )
        .optional()?
        .ok_or(AiRepositoryError::NotFound("剪辑工程"))?;
    if status == AiClipExportStatus::Exporting.as_str() {
        return Err(AiRepositoryError::ClipExportInProgress);
    }
    u32::try_from(version).map_err(|_| AiRepositoryError::Integrity("剪辑工程版本无效".to_owned()))
}

fn mark_clip_project_edited(
    transaction: &rusqlite::Transaction<'_>,
    clip_project_id: i64,
    now: &str,
) -> Result<()> {
    transaction.execute(
        r#"UPDATE ai_clip_projects
           SET version = version + 1,
               ownership = CASE WHEN smart_workflow_id IS NULL THEN ownership ELSE 'user' END,
               export_status = 'idle', export_progress = 0, output_path = NULL,
               last_error_code = NULL, last_error_message = NULL, updated_at = ?1
           WHERE id = ?2"#,
        params![now, clip_project_id],
    )?;
    transaction.execute(
        r#"UPDATE ai_smart_drafts
           SET ownership = 'user', status = CASE
                   WHEN status IN ('active', 'review_ready') THEN 'review_ready'
                   ELSE status END,
               updated_at = ?1
           WHERE clip_project_id = ?2 AND ownership = 'automation'"#,
        params![now, clip_project_id],
    )?;
    // 任务 5.7：片段结构变化后，邻接绑定已不成立的 `insert` 资产必须失效并重新审阅。
    invalidate_stale_insert_bindings(transaction, clip_project_id, now)?;
    Ok(())
}

/// 失效邻接绑定已不成立的 `insert` 生成资产（任务 5.7 / spec 场景「左右邻接关系改变」）。
///
/// 只处理仍在 `validated` 的单元：已失效的保持原状（幂等），已产生费用的记录不删除。
fn invalidate_stale_insert_bindings(
    transaction: &rusqlite::Transaction<'_>,
    clip_project_id: i64,
    now: &str,
) -> Result<()> {
    let segment_ids = transaction
        .prepare("SELECT id FROM ai_clip_segments WHERE clip_project_id = ?1 ORDER BY position, id")?
        .query_map([clip_project_id], |row| row.get::<_, i64>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let bindings = transaction
        .prepare(
            r#"SELECT asset.id, asset.left_segment_id, asset.right_segment_id, asset.boundary_id
               FROM ai_generated_animation_assets asset
               JOIN ai_clip_projects project ON project.highlight_run_id = asset.highlight_run_id
               WHERE project.id = ?1 AND asset.composition = 'insert' AND asset.status = 'validated'"#,
        )?
        .query_map([clip_project_id], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, Option<i64>>(1)?,
                row.get::<_, Option<i64>>(2)?,
                row.get::<_, Option<i64>>(3)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    for (asset_id, left, right, boundary_id) in bindings {
        let (Some(left), Some(right), Some(boundary_id)) = (left, right, boundary_id) else {
            continue;
        };
        // 用与审阅/失效判定同一份逻辑，避免两处实现漂移。
        let binding = AiGeneratedAnimationInsertBinding {
            left_segment_id: left,
            right_segment_id: right,
            boundary_id,
        };
        if !insert_binding_is_valid(&binding, &segment_ids) {
            transaction.execute(
                "UPDATE ai_generated_animation_assets SET status = 'invalidated', rejection_code = 'adjacency_changed', updated_at = ?2 WHERE id = ?1",
                params![asset_id, now],
            )?;
        }
    }
    Ok(())
}

fn mark_clip_project_automated(
    transaction: &rusqlite::Transaction<'_>,
    clip_project_id: i64,
    now: &str,
) -> Result<()> {
    let changed = transaction.execute(
        r#"UPDATE ai_clip_projects
           SET version = version + 1, export_status = 'idle', export_progress = 0,
               output_path = NULL, last_error_code = NULL, last_error_message = NULL,
               updated_at = ?1
           WHERE id = ?2 AND smart_workflow_id IS NOT NULL
             AND ownership = 'automation' AND export_status = 'idle'"#,
        params![now, clip_project_id],
    )?;
    if changed == 0 {
        return Err(AiRepositoryError::ClipVersionConflict);
    }
    let changed = transaction.execute(
        r#"UPDATE ai_smart_drafts
           SET automation_project_version = automation_project_version + 1, updated_at = ?1
           WHERE clip_project_id = ?2 AND ownership = 'automation'
             AND status IN ('active', 'review_ready')"#,
        params![now, clip_project_id],
    )?;
    if changed == 0 {
        return Err(AiRepositoryError::ClipVersionConflict);
    }
    Ok(())
}

/// AI repository 在一个 SQLite 事务边界内维护项目、输入和识别产物。
#[derive(Clone)]
pub struct AiRepository {
    database: Database,
}

impl AiRepository {
    pub fn new(database: Database) -> Self {
        Self { database }
    }

    pub fn create_project(&self, name: &str, profile: &RecognitionProfile) -> Result<AiProject> {
        let name = name.trim();
        if name.is_empty() {
            return Err(AiRepositoryError::Integrity("项目名称不能为空".to_owned()));
        }
        let profile_hash = profile.fingerprint()?;
        let profile_json = serde_json::to_string(profile)
            .map_err(|_| AiRepositoryError::Serialization("识别配置无法序列化".to_owned()))?;
        let now = Utc::now().to_rfc3339();
        let connection = self.database.connection()?;
        connection.execute(
            r#"
            INSERT INTO ai_projects(
                name, status, recognition_profile_json, recognition_profile_hash,
                input_frozen, progress_percent, created_at, updated_at
            ) VALUES(?1, 'draft', ?2, ?3, 0, 0, ?4, ?4)
            "#,
            params![name, profile_json, profile_hash, now],
        )?;
        let id = connection.last_insert_rowid();
        drop(connection);
        self.get_project_row(id)
    }

    pub fn rename_project(&self, id: i64, name: &str) -> Result<AiProject> {
        self.ensure_draft(id)?;
        let name = name.trim();
        if name.is_empty() {
            return Err(AiRepositoryError::Integrity("项目名称不能为空".to_owned()));
        }
        self.database.connection()?.execute(
            "UPDATE ai_projects SET name = ?1, updated_at = ?2 WHERE id = ?3",
            params![name, Utc::now().to_rfc3339(), id],
        )?;
        self.get_project_row(id)
    }

    pub fn list_projects(&self) -> Result<Vec<AiProject>> {
        let connection = self.database.connection()?;
        let mut statement =
            connection.prepare(&project_select("ORDER BY updated_at DESC, id DESC"))?;
        let rows = statement.query_map([], map_project)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
            .into_iter()
            .map(parse_project)
            .collect()
    }

    pub fn get_project(&self, id: i64) -> Result<AiProjectDetail> {
        Ok(AiProjectDetail {
            project: self.get_project_row(id)?,
            inputs: self.list_inputs(id)?,
        })
    }

    pub fn delete_project(&self, id: i64) -> Result<()> {
        let changed = self
            .database
            .connection()?
            .execute("DELETE FROM ai_projects WHERE id = ?1", [id])?;
        if changed == 0 {
            return Err(AiRepositoryError::NotFound("AI 项目"));
        }
        Ok(())
    }

    pub fn add_input(&self, project_id: i64, input: NewAiProjectInput) -> Result<AiProjectInput> {
        self.ensure_draft(project_id)?;
        let source_hash = input.source_fingerprint.fingerprint()?;
        let source_json = serde_json::to_string(&input.source_fingerprint)
            .map_err(|_| AiRepositoryError::Serialization("源指纹无法序列化".to_owned()))?;
        let connection = self.database.connection()?;
        let inserted = connection.execute(
            r#"
            INSERT INTO ai_project_inputs(
                project_id, position, source_kind, video_id, display_name, source_path,
                source_fingerprint_json, source_fingerprint_hash, duration_ms,
                audio_present, status, created_at, updated_at
            ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 'pending', ?11, ?11)
            "#,
            params![
                project_id,
                input.position,
                input.source_kind.as_str(),
                input.video_id,
                input.display_name.trim(),
                input.source_path,
                source_json,
                source_hash,
                input.duration_ms.map(|value| value as i64),
                input.audio_present,
                Utc::now().to_rfc3339(),
            ],
        );
        match inserted {
            Ok(_) => {
                let id = connection.last_insert_rowid();
                drop(connection);
                self.get_input(id)
            }
            Err(error)
                if error.sqlite_error_code() == Some(rusqlite::ErrorCode::ConstraintViolation) =>
            {
                Err(AiRepositoryError::DuplicateInput)
            }
            Err(error) => Err(error.into()),
        }
    }

    pub fn reorder_inputs(&self, project_id: i64, ordered_ids: &[i64]) -> Result<()> {
        self.ensure_draft(project_id)?;
        let mut connection = self.database.connection()?;
        let transaction = connection.transaction()?;
        let existing: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM ai_project_inputs WHERE project_id = ?1",
            [project_id],
            |row| row.get(0),
        )?;
        if existing as usize != ordered_ids.len() {
            return Err(AiRepositoryError::Integrity(
                "输入排序必须包含项目的全部输入".to_owned(),
            ));
        }
        for (position, id) in ordered_ids.iter().enumerate() {
            let changed = transaction.execute(
                "UPDATE ai_project_inputs SET position = ?1 + 1000000 WHERE id = ?2 AND project_id = ?3",
                params![position as i64, id, project_id],
            )?;
            if changed != 1 {
                return Err(AiRepositoryError::Integrity(
                    "输入排序包含不属于当前项目的记录".to_owned(),
                ));
            }
        }
        transaction.execute(
            "UPDATE ai_project_inputs SET position = position - 1000000, updated_at = ?1 WHERE project_id = ?2",
            params![Utc::now().to_rfc3339(), project_id],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn remove_input(&self, project_id: i64, input_id: i64) -> Result<()> {
        self.ensure_draft(project_id)?;
        let changed = self.database.connection()?.execute(
            "DELETE FROM ai_project_inputs WHERE id = ?1 AND project_id = ?2",
            params![input_id, project_id],
        )?;
        if changed == 0 {
            return Err(AiRepositoryError::NotFound("项目输入"));
        }
        Ok(())
    }

    pub fn freeze_project(&self, project_id: i64) -> Result<AiProject> {
        self.ensure_draft(project_id)?;
        let inputs = self.list_inputs(project_id)?;
        if inputs.is_empty() {
            return Err(AiRepositoryError::EmptyProject);
        }
        let mut connection = self.database.connection()?;
        let transaction = connection.transaction()?;
        let mut offset = Some(0_u64);
        for input in inputs {
            transaction.execute(
                "UPDATE ai_project_inputs SET project_offset_ms = ?1, updated_at = ?2 WHERE id = ?3",
                params![
                    offset.map(|value| value as i64),
                    Utc::now().to_rfc3339(),
                    input.id
                ],
            )?;
            offset = match (offset, input.duration_ms) {
                (Some(current), Some(duration)) => Some(current.saturating_add(duration)),
                _ => None,
            };
        }
        transaction.execute(
            "UPDATE ai_projects SET input_frozen = 1, status = 'queued', updated_at = ?1 WHERE id = ?2",
            params![Utc::now().to_rfc3339(), project_id],
        )?;
        transaction.commit()?;
        drop(connection);
        self.get_project_row(project_id)
    }

    /// 将用户明确选择的失败或已取消输入恢复为待执行；不会自动无限重试。
    pub fn prepare_input_retry(&self, input_id: i64) -> Result<AiProjectInput> {
        let input = self.get_input(input_id)?;
        if !matches!(
            input.status,
            AiInputStatus::Failed | AiInputStatus::Cancelled
        ) {
            return Err(AiRepositoryError::InvalidState(
                "只有失败或已取消输入可以重试".to_owned(),
            ));
        }
        let project = self.get_project_row(input.project_id)?;
        if project.status == AiProjectStatus::Completed {
            return Err(AiRepositoryError::InvalidState(
                "已完整完成的项目没有可重试输入".to_owned(),
            ));
        }
        let mut connection = self.database.connection()?;
        let transaction = connection.transaction()?;
        transaction.execute(
            r#"
            UPDATE ai_project_inputs
            SET status = 'pending', scheduler_generation = scheduler_generation + 1,
                queue_priority = 0, queue_sequence = NULL, artifact_id = NULL, last_error_code = NULL,
                last_error_message = NULL, updated_at = ?1
            WHERE id = ?2 AND status IN ('failed', 'cancelled')
            "#,
            params![Utc::now().to_rfc3339(), input_id],
        )?;
        if matches!(
            project.status,
            AiProjectStatus::CompletedWithErrors
                | AiProjectStatus::Failed
                | AiProjectStatus::Cancelled
        ) {
            transaction.execute(
                r#"
                UPDATE ai_projects
                SET status = 'queued', completed_at = NULL, last_error_code = NULL,
                    last_error_message = NULL, updated_at = ?1
                WHERE id = ?2
                "#,
                params![Utc::now().to_rfc3339(), input.project_id],
            )?;
        }
        transaction.commit()?;
        drop(connection);
        let _ = self.recompute_project_progress(input.project_id)?;
        self.get_input(input_id)
    }

    /// 取消项目只终止仍未完成的输入，已完整发布的输入和产物保持不变。
    pub fn cancel_project(&self, project_id: i64) -> Result<AiProject> {
        let project = self.get_project_row(project_id)?;
        if !matches!(
            project.status,
            AiProjectStatus::Queued | AiProjectStatus::Running
        ) {
            return Err(AiRepositoryError::InvalidState(
                "只有排队或运行中的项目可以取消".to_owned(),
            ));
        }
        let mut connection = self.database.connection()?;
        let transaction = connection.transaction()?;
        let now = Utc::now().to_rfc3339();
        transaction.execute(
            r#"
            UPDATE ai_project_inputs
            SET status = 'cancelled', last_error_code = 'user_cancelled',
                last_error_message = '用户已取消语音识别', updated_at = ?1
            WHERE project_id = ?2
              AND status IN ('pending', 'validating', 'preparing_audio', 'detecting_speech', 'transcribing')
            "#,
            params![now, project_id],
        )?;
        transaction.execute(
            r#"
            UPDATE ai_projects
            SET status = 'cancelled', completed_at = ?1, updated_at = ?1
            WHERE id = ?2
            "#,
            params![now, project_id],
        )?;
        transaction.commit()?;
        drop(connection);
        self.recompute_project_progress(project_id)
    }

    /// 标记项目正在删除，阻止新的调度、重试和精彩运行。
    pub fn mark_project_deleting(&self, project_id: i64) -> Result<AiProject> {
        let project = self.get_project_row(project_id)?;
        if !matches!(
            project.status,
            AiProjectStatus::Queued | AiProjectStatus::Running
        ) {
            return Err(AiRepositoryError::InvalidState(
                "只有排队或运行中的项目可以进入删除状态".to_owned(),
            ));
        }
        let now = Utc::now().to_rfc3339();
        self.database.connection()?.execute(
            "UPDATE ai_projects SET status = 'deleting', deleting_at = ?1, updated_at = ?1 WHERE id = ?2",
            params![now, project_id],
        )?;
        self.get_project_row(project_id)
    }

    /// 完成两阶段删除。只删除 AI 项目及其派生数据，不触碰源视频。
    pub fn delete_deleting_project(&self, project_id: i64) -> Result<()> {
        let project = self.get_project_row(project_id)?;
        if project.status != AiProjectStatus::Deleting {
            return Err(AiRepositoryError::InvalidState(
                "项目未处于删除状态".to_owned(),
            ));
        }
        self.delete_project(project_id)
    }

    pub fn recover_deleting_projects(&self) -> Result<Vec<i64>> {
        let connection = self.database.connection()?;
        let mut statement = connection
            .prepare("SELECT id FROM ai_projects WHERE status = 'deleting' ORDER BY id")?;
        let ids = statement
            .query_map([], |row| row.get::<_, i64>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(ids)
    }

    pub fn set_project_context(
        &self,
        project_id: i64,
        project_tags: &[String],
        analysis_goal: Option<&str>,
    ) -> Result<AiProject> {
        self.ensure_draft(project_id)?;
        let mut tags = project_tags
            .iter()
            .map(|tag| tag.trim().to_owned())
            .filter(|tag| !tag.is_empty())
            .collect::<Vec<_>>();
        tags.sort_by_key(|tag| tag.to_lowercase());
        tags.dedup_by(|left, right| left.eq_ignore_ascii_case(right));
        if tags.len() > 10 || tags.iter().any(|tag| tag.chars().count() > 24) {
            return Err(AiRepositoryError::Integrity(
                "项目标签数量或长度无效".to_owned(),
            ));
        }
        let tags_json = serde_json::to_string(&tags)
            .map_err(|_| AiRepositoryError::Serialization("项目标签无法序列化".to_owned()))?;
        let goal = analysis_goal
            .map(str::trim)
            .filter(|value| !value.is_empty());
        if goal.is_some_and(|value| value.chars().count() > 500) {
            return Err(AiRepositoryError::Integrity(
                "分析目标不能超过 500 个字符".to_owned(),
            ));
        }
        self.database.connection()?.execute(
            "UPDATE ai_projects SET project_tags_json = ?1, analysis_goal = ?2, updated_at = ?3 WHERE id = ?4",
            params![tags_json, goal, Utc::now().to_rfc3339(), project_id],
        )?;
        self.get_project_row(project_id)
    }

    pub fn update_scheduler_state(
        &self,
        input_id: i64,
        generation: u64,
        queue_priority: u32,
        queue_sequence: Option<u64>,
    ) -> Result<AiProjectInput> {
        if generation == 0 {
            return Err(AiRepositoryError::Integrity(
                "调度代次必须大于零".to_owned(),
            ));
        }
        let changed = self.database.connection()?.execute(
            "UPDATE ai_project_inputs SET scheduler_generation = ?1, queue_priority = ?2, queue_sequence = ?3, updated_at = ?4 WHERE id = ?5",
            params![generation as i64, queue_priority, queue_sequence.map(|value| value as i64), Utc::now().to_rfc3339(), input_id],
        )?;
        if changed == 0 {
            return Err(AiRepositoryError::NotFound("项目输入"));
        }
        self.get_input(input_id)
    }

    pub fn accept_scheduler_event(
        &self,
        input_id: i64,
        generation: u64,
        next: AiInputStatus,
        error: Option<(&str, &str)>,
    ) -> Result<bool> {
        let input = self.get_input(input_id)?;
        if input.scheduler_generation != generation
            || matches!(
                input.status,
                AiInputStatus::Completed | AiInputStatus::Cancelled | AiInputStatus::Failed
            )
        {
            return Ok(false);
        }
        self.transition_input(input_id, next, error)?;
        Ok(true)
    }

    /// 特效字幕渲染缓存的保留天数；**0 表示不清理**。
    ///
    /// 此前 `prune_render_cache` 零调用，缓存永不回收；保留期做成设置项后，
    /// 回收策略由用户决定，默认 3 天。
    pub fn render_cache_retention_days(&self) -> Result<u32> {
        Ok(self.database.get_settings()?.render_cache_retention_days)
    }

    pub fn get_llm_provider_settings(&self, key_configured: bool) -> Result<LlmProviderSettings> {
        let connection = self.database.connection()?;
        let row = connection
            .query_row(
                "SELECT provider, model_id, timeout_ms, prompt_version, qualified_score, excellent_score, transition_auto_apply_score, updated_at, model_source, resolved_at, next_resolution_at FROM llm_provider_settings WHERE id = 1",
                [],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, i64>(6)?,
                        row.get::<_, String>(7)?,
                        row.get::<_, Option<String>>(8)?,
                        row.get::<_, Option<String>>(9)?,
                        row.get::<_, Option<String>>(10)?,
                    ))
                },
            )
            .optional()?;
        super::llm::settings_from_row(row, key_configured)
            .map_err(|error| AiRepositoryError::Integrity(error.to_string()))
    }

    pub fn save_llm_provider_settings(
        &self,
        settings: &LlmProviderSettings,
    ) -> Result<LlmProviderSettings> {
        settings
            .validate()
            .map_err(|error| AiRepositoryError::Integrity(error.to_string()))?;
        let now = Utc::now().to_rfc3339();
        self.database.connection()?.execute(
            r#"INSERT INTO llm_provider_settings(id, provider, model_id, timeout_ms, prompt_version, qualified_score, excellent_score, transition_auto_apply_score, updated_at, model_source, resolved_at, next_resolution_at)
               VALUES(1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
               ON CONFLICT(id) DO UPDATE SET provider=excluded.provider, model_id=excluded.model_id,
                   timeout_ms=excluded.timeout_ms, prompt_version=excluded.prompt_version,
                   qualified_score=excluded.qualified_score, excellent_score=excluded.excellent_score,
                   transition_auto_apply_score=excluded.transition_auto_apply_score,
                   updated_at=excluded.updated_at, model_source=excluded.model_source,
                   resolved_at=excluded.resolved_at, next_resolution_at=excluded.next_resolution_at"#,
            params![settings.provider, settings.model_id.trim(), settings.timeout_ms as i64, settings.prompt_version, settings.qualified_score, settings.excellent_score, settings.transition_auto_apply_score, now, settings.model_source, settings.resolved_at, settings.next_resolution_at],
        )?;
        self.get_llm_provider_settings(settings.key_configured)
    }

    pub fn create_highlight_run(&self, input: NewAiHighlightRun) -> Result<AiHighlightRun> {
        if !input.user_authorized {
            return Err(AiRepositoryError::InvalidState(
                "精彩分析必须由用户显式授权".to_owned(),
            ));
        }
        let project = self.get_project_row(input.project_id)?;
        if !matches!(
            project.status,
            AiProjectStatus::Completed | AiProjectStatus::CompletedWithErrors
        ) {
            return Err(AiRepositoryError::InvalidState(
                "只有 ASR 完成的项目可以进行精彩分析".to_owned(),
            ));
        }
        if input.analysis_fingerprint.trim().is_empty() {
            return Err(AiRepositoryError::Integrity("分析指纹不能为空".to_owned()));
        }
        if input.qualified_score > 100
            || input.excellent_score > 100
            || input.excellent_score < input.qualified_score
        {
            return Err(AiRepositoryError::Integrity(
                "精彩运行阈值必须在 0 到 100 之间，且优秀阈值不能低于合格阈值".to_owned(),
            ));
        }
        let tags_json = serde_json::to_string(&input.tags_snapshot)
            .map_err(|_| AiRepositoryError::Serialization("标签快照无法序列化".to_owned()))?;
        let skills_json = serde_json::to_string(&input.skills_snapshot)
            .map_err(|_| AiRepositoryError::Serialization("Skills 快照无法序列化".to_owned()))?;
        let now = Utc::now().to_rfc3339();
        let connection = self.database.connection()?;
        let inserted = connection.execute(
            r#"INSERT INTO ai_highlight_runs(
                project_id, status, model_id, prompt_version, tags_snapshot_json,
                skills_snapshot_json, analysis_goal, analysis_fingerprint, qualified_score, excellent_score,
                user_authorized, total_segments, total_chars, estimated_batches,
                created_at, updated_at
            ) VALUES(?1, 'pending', ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 1, ?10, ?11, ?12, ?13, ?13)"#,
            params![
                input.project_id,
                input.model_id.trim(),
                input.prompt_version,
                tags_json,
                skills_json,
                input.analysis_goal,
                input.analysis_fingerprint,
                input.qualified_score,
                input.excellent_score,
                input.total_segments as i64,
                input.total_chars as i64,
                input.estimated_batches as i64,
                now,
            ],
        );
        let run_id = match inserted {
            Ok(_) => connection.last_insert_rowid(),
            Err(error)
                if error.sqlite_error_code() == Some(rusqlite::ErrorCode::ConstraintViolation) =>
            {
                connection
                    .query_row(
                        "SELECT id FROM ai_highlight_runs WHERE project_id = ?1 AND analysis_fingerprint = ?2",
                        params![input.project_id, input.analysis_fingerprint],
                        |row| row.get(0),
                    )
                    .optional()?
                    .ok_or(AiRepositoryError::Integrity("精彩运行指纹冲突".to_owned()))?
            }
            Err(error) => return Err(error.into()),
        };
        // 释放 Database 的连接锁后再读取完整 DTO，避免内存 SQLite 重入死锁。
        drop(connection);
        self.get_highlight_run(run_id)
    }

    pub fn get_highlight_run(&self, run_id: i64) -> Result<AiHighlightRun> {
        let connection = self.database.connection()?;
        let row = connection
            .query_row(
                "SELECT id, project_id, status, model_id, prompt_version, tags_snapshot_json, skills_snapshot_json, analysis_goal, analysis_fingerprint, qualified_score, excellent_score, user_authorized, total_segments, total_chars, estimated_batches, total_tokens, last_error_code, last_error_message, created_at, updated_at FROM ai_highlight_runs WHERE id = ?1",
                [run_id],
                map_highlight_run,
            )
            .optional()?
            .ok_or(AiRepositoryError::NotFound("精彩运行"))?;
        parse_highlight_run(row)
    }

    pub fn latest_highlight_run_for_project(
        &self,
        project_id: i64,
    ) -> Result<Option<AiHighlightRun>> {
        self.get_project_row(project_id)?;
        let connection = self.database.connection()?;
        let row = connection
            .query_row(
                "SELECT id, project_id, status, model_id, prompt_version, tags_snapshot_json, skills_snapshot_json, analysis_goal, analysis_fingerprint, qualified_score, excellent_score, user_authorized, total_segments, total_chars, estimated_batches, total_tokens, last_error_code, last_error_message, created_at, updated_at FROM ai_highlight_runs WHERE project_id = ?1 ORDER BY id DESC LIMIT 1",
                [project_id],
                map_highlight_run,
            )
            .optional()?;
        row.map(parse_highlight_run).transpose()
    }

    pub fn update_highlight_run_status(
        &self,
        run_id: i64,
        status: AiHighlightRunStatus,
        error: Option<(&str, &str)>,
    ) -> Result<AiHighlightRun> {
        let (code, message) = error
            .map(|(code, message)| (Some(code), Some(message)))
            .unwrap_or((None, None));
        self.database.connection()?.execute(
            "UPDATE ai_highlight_runs SET status = ?1, last_error_code = ?2, last_error_message = ?3, updated_at = ?4 WHERE id = ?5",
            params![status.as_str(), code, message, Utc::now().to_rfc3339(), run_id],
        )?;
        self.get_highlight_run(run_id)
    }

    pub fn add_highlight_run_tokens(
        &self,
        run_id: i64,
        token_usage: u64,
    ) -> Result<AiHighlightRun> {
        self.database.connection()?.execute(
            "UPDATE ai_highlight_runs SET total_tokens = total_tokens + ?1, updated_at = ?2 WHERE id = ?3",
            params![token_usage as i64, Utc::now().to_rfc3339(), run_id],
        )?;
        self.get_highlight_run(run_id)
    }

    pub fn add_highlight_chunks(
        &self,
        run_id: i64,
        chunks: &[NewAiHighlightChunk],
    ) -> Result<Vec<AiHighlightChunk>> {
        let mut connection = self.database.connection()?;
        let transaction = connection.transaction()?;
        let now = Utc::now().to_rfc3339();
        for chunk in chunks {
            if chunk.segment_ids.is_empty() {
                return Err(AiRepositoryError::Integrity(
                    "精彩分块不能没有句段".to_owned(),
                ));
            }
            transaction.execute(
                "INSERT OR IGNORE INTO ai_highlight_chunks(run_id, ordinal, input_id, segment_ids_json, context_segment_ids_json, status, created_at, updated_at) VALUES(?1, ?2, ?3, ?4, ?5, 'pending', ?6, ?6)",
                params![run_id, chunk.ordinal, chunk.input_id, serde_json::to_string(&chunk.segment_ids).map_err(|_| AiRepositoryError::Serialization("分块句段无法序列化".to_owned()))?, serde_json::to_string(&chunk.context_segment_ids).map_err(|_| AiRepositoryError::Serialization("上下文句段无法序列化".to_owned()))?, now],
            )?;
        }
        transaction.commit()?;
        drop(connection);
        self.list_highlight_chunks(run_id)
    }

    pub fn list_highlight_chunks(&self, run_id: i64) -> Result<Vec<AiHighlightChunk>> {
        let connection = self.database.connection()?;
        let mut statement = connection.prepare("SELECT id, run_id, ordinal, input_id, segment_ids_json, context_segment_ids_json, status, candidate_count, token_usage, last_error_code, last_error_message FROM ai_highlight_chunks WHERE run_id = ?1 ORDER BY ordinal")?;
        let rows = statement.query_map([run_id], map_highlight_chunk)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
            .into_iter()
            .map(parse_highlight_chunk)
            .collect()
    }

    pub fn highlight_progress(&self, run_id: i64) -> Result<AiHighlightProgress> {
        self.get_highlight_run(run_id)?;
        let connection = self.database.connection()?;
        let (total, pending, running, completed, failed, candidates) = connection.query_row(
            r#"SELECT
                COUNT(*),
                SUM(CASE WHEN status = 'pending' THEN 1 ELSE 0 END),
                SUM(CASE WHEN status = 'running' THEN 1 ELSE 0 END),
                SUM(CASE WHEN status = 'completed' THEN 1 ELSE 0 END),
                SUM(CASE WHEN status = 'failed' THEN 1 ELSE 0 END),
                COALESCE(SUM(candidate_count), 0)
               FROM ai_highlight_chunks WHERE run_id = ?1"#,
            [run_id],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, Option<i64>>(1)?.unwrap_or(0),
                    row.get::<_, Option<i64>>(2)?.unwrap_or(0),
                    row.get::<_, Option<i64>>(3)?.unwrap_or(0),
                    row.get::<_, Option<i64>>(4)?.unwrap_or(0),
                    row.get::<_, i64>(5)?,
                ))
            },
        )?;
        let count = |value: i64| {
            u64::try_from(value)
                .map_err(|_| AiRepositoryError::Integrity("精彩批次统计无效".to_owned()))
        };
        Ok(AiHighlightProgress {
            run_id,
            total_batches: count(total)?,
            pending_batches: count(pending)?,
            running_batches: count(running)?,
            completed_batches: count(completed)?,
            failed_batches: count(failed)?,
            candidate_count: count(candidates)?,
        })
    }

    pub fn mark_highlight_chunk_running(&self, run_id: i64, chunk_id: i64) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let mut connection = self.database.connection()?;
        let transaction = connection.transaction()?;
        let changed = transaction.execute(
            "UPDATE ai_highlight_chunks SET status = 'running', attempt_count = attempt_count + 1, last_error_code = NULL, last_error_message = NULL, updated_at = ?1 WHERE id = ?2 AND run_id = ?3 AND status != 'completed'",
            params![now, chunk_id, run_id],
        )?;
        if changed == 0 {
            return Err(AiRepositoryError::InvalidState(
                "精彩批次不存在或已经完成".to_owned(),
            ));
        }
        transaction.execute(
            "UPDATE ai_highlight_runs SET status = 'running', updated_at = ?1 WHERE id = ?2",
            params![now, run_id],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn complete_highlight_chunk(
        &self,
        run_id: i64,
        chunk_id: i64,
        drafts: &[HighlightCandidateDraft],
        token_usage: u64,
    ) -> Result<()> {
        let drafts_json = serde_json::to_string(drafts)
            .map_err(|_| AiRepositoryError::Serialization("精彩候选草稿无法序列化".to_owned()))?;
        let now = Utc::now().to_rfc3339();
        let mut connection = self.database.connection()?;
        let transaction = connection.transaction()?;
        let changed = transaction.execute(
            "UPDATE ai_highlight_chunks SET status = 'completed', candidate_count = ?1, token_usage = ?2, candidate_drafts_json = ?3, last_error_code = NULL, last_error_message = NULL, updated_at = ?4 WHERE id = ?5 AND run_id = ?6 AND status != 'completed'",
            params![drafts.len() as i64, token_usage as i64, drafts_json, now, chunk_id, run_id],
        )?;
        if changed == 0 {
            return Err(AiRepositoryError::InvalidState(
                "精彩批次不存在或已经完成".to_owned(),
            ));
        }
        transaction.execute(
            "UPDATE ai_highlight_runs SET total_tokens = total_tokens + ?1, updated_at = ?2 WHERE id = ?3",
            params![token_usage as i64, now, run_id],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn fail_highlight_chunk(
        &self,
        run_id: i64,
        chunk_id: i64,
        code: &str,
        message: &str,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let mut connection = self.database.connection()?;
        let transaction = connection.transaction()?;
        transaction.execute(
            "UPDATE ai_highlight_chunks SET status = 'failed', last_error_code = ?1, last_error_message = ?2, updated_at = ?3 WHERE id = ?4 AND run_id = ?5",
            params![code, message, now, chunk_id, run_id],
        )?;
        transaction.execute(
            "UPDATE ai_highlight_runs SET last_error_code = ?1, last_error_message = ?2, updated_at = ?3 WHERE id = ?4",
            params![code, message, now, run_id],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn list_completed_highlight_drafts(
        &self,
        run_id: i64,
    ) -> Result<Vec<(AiHighlightChunk, Vec<HighlightCandidateDraft>)>> {
        let connection = self.database.connection()?;
        let mut statement = connection.prepare(
            "SELECT id, run_id, ordinal, input_id, segment_ids_json, context_segment_ids_json, status, candidate_count, token_usage, last_error_code, last_error_message, candidate_drafts_json FROM ai_highlight_chunks WHERE run_id = ?1 AND status = 'completed' ORDER BY ordinal",
        )?;
        let rows = statement.query_map([run_id], |row| {
            Ok((map_highlight_chunk(row)?, row.get::<_, String>(11)?))
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
            .into_iter()
            .map(|(row, drafts_json)| {
                let chunk = parse_highlight_chunk(row)?;
                let drafts = serde_json::from_str(&drafts_json)
                    .map_err(|_| AiRepositoryError::Integrity("精彩候选草稿快照损坏".to_owned()))?;
                Ok((chunk, drafts))
            })
            .collect()
    }

    pub fn replace_highlight_results(
        &self,
        run_id: i64,
        drafts: &[(i64, HighlightCandidateDraft)],
        scores: &[HighlightCandidateScore],
    ) -> Result<Vec<AiHighlightCandidate>> {
        let mut connection = self.database.connection()?;
        let transaction = connection.transaction()?;
        let now = Utc::now().to_rfc3339();
        let previous_candidates = {
            let mut statement = transaction.prepare(
                "SELECT candidate_key, total_score, selected FROM ai_highlight_candidates WHERE run_id = ?1",
            )?;
            statement
                .query_map([run_id], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        (row.get::<_, f32>(1)?, row.get::<_, bool>(2)?),
                    ))
                })?
                .collect::<std::result::Result<HashMap<_, _>, _>>()?
        };
        let excellent_score = transaction.query_row(
            "SELECT excellent_score FROM ai_highlight_runs WHERE id = ?1",
            [run_id],
            |row| row.get::<_, i64>(0),
        )?;
        transaction.execute(
            "DELETE FROM ai_highlight_candidates WHERE run_id = ?1",
            [run_id],
        )?;
        for (chunk_id, draft) in drafts {
            validate_candidate_draft(
                &draft.segment_ids,
                draft.start_ms,
                draft.end_ms,
                &draft.candidate_key,
            )?;
            let score = scores
                .iter()
                .find(|score| score.candidate_key == draft.candidate_key)
                .ok_or_else(|| AiRepositoryError::Integrity("候选缺少全局评分".to_owned()))?;
            validate_score(score)?;
            transaction.execute(
                r#"INSERT INTO ai_highlight_candidates(
                    run_id, chunk_id, candidate_key, title, input_id, segment_ids_json,
                    start_ms, end_ms, total_score, hook_score, information_score,
                    emotion_score, tag_relevance_score, completeness_score,
                    shareability_score, reason, matched_tags_json, rank, selected, created_at
                ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20)"#,
                params![run_id, chunk_id, draft.candidate_key, draft.title, draft.input_id,
                    serde_json::to_string(&draft.segment_ids).map_err(|_| AiRepositoryError::Serialization("候选句段无法序列化".to_owned()))?,
                    draft.start_ms as i64, draft.end_ms as i64, score.total_score, score.hook_score,
                    score.information_score, score.emotion_score, score.tag_relevance_score,
                    score.completeness_score, score.shareability_score, score.reason,
                    serde_json::to_string(&draft.matched_tags).map_err(|_| AiRepositoryError::Serialization("命中标签无法序列化".to_owned()))?, score.rank,
                    match previous_candidates.get(&draft.candidate_key) {
                        None => score.total_score >= excellent_score as f32,
                        Some((_, true)) => true,
                        Some((previous_score, false)) => *previous_score < excellent_score as f32
                            && score.total_score >= excellent_score as f32,
                    }, now],
            )?;
        }
        transaction.commit()?;
        drop(connection);
        self.list_highlight_candidates(run_id)
    }

    /// 将一个分块的候选和全局评分在同一事务中发布，避免 UI 看到半条候选。
    pub fn publish_highlight_results(
        &self,
        run_id: i64,
        chunk_id: i64,
        drafts: &[HighlightCandidateDraft],
        scores: &[HighlightCandidateScore],
        token_usage: u64,
    ) -> Result<Vec<AiHighlightCandidate>> {
        let mut connection = self.database.connection()?;
        let transaction = connection.transaction()?;
        let now = Utc::now().to_rfc3339();
        for draft in drafts {
            validate_candidate_draft(
                &draft.segment_ids,
                draft.start_ms,
                draft.end_ms,
                &draft.candidate_key,
            )?;
            let score = scores
                .iter()
                .find(|score| score.candidate_key == draft.candidate_key);
            let score =
                score.ok_or_else(|| AiRepositoryError::Integrity("候选缺少全局评分".to_owned()))?;
            validate_score(score)?;
            transaction.execute(
                r#"INSERT INTO ai_highlight_candidates(
                    run_id, chunk_id, candidate_key, title, input_id, segment_ids_json,
                    start_ms, end_ms, total_score, hook_score, information_score,
                    emotion_score, tag_relevance_score, completeness_score,
                    shareability_score, reason, matched_tags_json, rank, created_at
                ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)
                ON CONFLICT(run_id, candidate_key) DO UPDATE SET
                    title=excluded.title, segment_ids_json=excluded.segment_ids_json,
                    start_ms=excluded.start_ms, end_ms=excluded.end_ms,
                    total_score=excluded.total_score, hook_score=excluded.hook_score,
                    information_score=excluded.information_score, emotion_score=excluded.emotion_score,
                    tag_relevance_score=excluded.tag_relevance_score, completeness_score=excluded.completeness_score,
                    shareability_score=excluded.shareability_score, reason=excluded.reason,
                    matched_tags_json=excluded.matched_tags_json, rank=excluded.rank"#,
                params![run_id, chunk_id, draft.candidate_key, draft.title, draft.input_id,
                    serde_json::to_string(&draft.segment_ids).map_err(|_| AiRepositoryError::Serialization("候选句段无法序列化".to_owned()))?,
                    draft.start_ms as i64, draft.end_ms as i64, score.total_score, score.hook_score,
                    score.information_score, score.emotion_score, score.tag_relevance_score,
                    score.completeness_score, score.shareability_score, score.reason,
                    serde_json::to_string(&draft.matched_tags).map_err(|_| AiRepositoryError::Serialization("命中标签无法序列化".to_owned()))?, score.rank, now],
            )?;
        }
        transaction.execute("UPDATE ai_highlight_chunks SET status = 'completed', candidate_count = ?1, token_usage = ?2, updated_at = ?3 WHERE id = ?4 AND run_id = ?5", params![drafts.len() as i64, token_usage as i64, now, chunk_id, run_id])?;
        transaction.execute("UPDATE ai_highlight_runs SET total_tokens = total_tokens + ?1, status = 'candidates', updated_at = ?2 WHERE id = ?3", params![token_usage as i64, now, run_id])?;
        transaction.commit()?;
        drop(connection);
        self.list_highlight_candidates(run_id)
    }

    pub fn list_highlight_candidates(&self, run_id: i64) -> Result<Vec<AiHighlightCandidate>> {
        let connection = self.database.connection()?;
        let mut statement = connection.prepare("SELECT id, run_id, chunk_id, candidate_key, title, input_id, segment_ids_json, start_ms, end_ms, total_score, hook_score, information_score, emotion_score, tag_relevance_score, completeness_score, shareability_score, reason, matched_tags_json, rank, selected FROM ai_highlight_candidates WHERE run_id = ?1 ORDER BY COALESCE(rank, 999999), total_score DESC, id")?;
        let rows = statement.query_map([run_id], map_highlight_candidate)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
            .into_iter()
            .map(parse_highlight_candidate)
            .collect()
    }

    pub fn list_qualified_highlight_candidates(
        &self,
        run_id: i64,
        page: u32,
        page_size: u32,
    ) -> Result<AiHighlightCandidatePage> {
        if page_size == 0 || page_size > 100 {
            return Err(AiRepositoryError::Integrity(
                "候选分页大小必须在 1 到 100 之间".to_owned(),
            ));
        }
        let connection = self.database.connection()?;
        let (qualified_score, total_candidates, qualified_candidates, selected_candidates) = connection
            .query_row(
                r#"SELECT
                    qualified_score,
                    (SELECT COUNT(*) FROM ai_highlight_candidates WHERE run_id = ai_highlight_runs.id),
                    (SELECT COUNT(*) FROM ai_highlight_candidates WHERE run_id = ai_highlight_runs.id AND total_score >= ai_highlight_runs.qualified_score),
                    (SELECT COUNT(*) FROM ai_highlight_candidates WHERE run_id = ai_highlight_runs.id AND selected = 1)
                   FROM ai_highlight_runs WHERE id = ?1"#,
                [run_id],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?, row.get::<_, i64>(2)?, row.get::<_, i64>(3)?)),
            )
            .map_err(|error| match error {
                rusqlite::Error::QueryReturnedNoRows => AiRepositoryError::NotFound("精彩运行"),
                error => AiRepositoryError::Sqlite(error),
            })?;
        let offset = i64::from(page)
            .checked_mul(i64::from(page_size))
            .ok_or_else(|| AiRepositoryError::Integrity("候选分页页码无效".to_owned()))?;
        let mut statement = connection.prepare(
            "SELECT id, run_id, chunk_id, candidate_key, title, input_id, segment_ids_json, start_ms, end_ms, total_score, hook_score, information_score, emotion_score, tag_relevance_score, completeness_score, shareability_score, reason, matched_tags_json, rank, selected FROM ai_highlight_candidates WHERE run_id = ?1 AND total_score >= ?2 ORDER BY COALESCE(rank, 999999), total_score DESC, id LIMIT ?3 OFFSET ?4",
        )?;
        let rows = statement.query_map(
            params![run_id, qualified_score, i64::from(page_size), offset],
            map_highlight_candidate,
        )?;
        let items = rows
            .collect::<std::result::Result<Vec<_>, _>>()?
            .into_iter()
            .map(parse_highlight_candidate)
            .collect::<Result<Vec<_>>>()?;
        Ok(AiHighlightCandidatePage {
            items,
            page,
            page_size,
            total_candidates: u64::try_from(total_candidates)
                .map_err(|_| AiRepositoryError::Integrity("候选总数无效".to_owned()))?,
            qualified_candidates: u64::try_from(qualified_candidates)
                .map_err(|_| AiRepositoryError::Integrity("合格候选数无效".to_owned()))?,
            selected_candidates: u64::try_from(selected_candidates)
                .map_err(|_| AiRepositoryError::Integrity("已选择候选数无效".to_owned()))?,
        })
    }

    /// 已选择的候选需要独立分页读取，不能从当前“合格候选”页推导；否则在大结果集
    /// 中切换选择会把不在当前页的人工选择错误清空。
    pub fn list_selected_highlight_candidates(
        &self,
        run_id: i64,
        page: u32,
        page_size: u32,
    ) -> Result<AiHighlightCandidatePage> {
        if page_size == 0 || page_size > 100 {
            return Err(AiRepositoryError::Integrity(
                "候选分页大小必须在 1 到 100 之间".to_owned(),
            ));
        }
        let connection = self.database.connection()?;
        let (_qualified_score, total_candidates, qualified_candidates, selected_candidates) = connection
            .query_row(
                r#"SELECT
                    qualified_score,
                    (SELECT COUNT(*) FROM ai_highlight_candidates WHERE run_id = ai_highlight_runs.id),
                    (SELECT COUNT(*) FROM ai_highlight_candidates WHERE run_id = ai_highlight_runs.id AND total_score >= ai_highlight_runs.qualified_score),
                    (SELECT COUNT(*) FROM ai_highlight_candidates WHERE run_id = ai_highlight_runs.id AND selected = 1)
                   FROM ai_highlight_runs WHERE id = ?1"#,
                [run_id],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?, row.get::<_, i64>(2)?, row.get::<_, i64>(3)?)),
            )
            .map_err(|error| match error {
                rusqlite::Error::QueryReturnedNoRows => AiRepositoryError::NotFound("精彩运行"),
                error => AiRepositoryError::Sqlite(error),
            })?;
        let offset = i64::from(page)
            .checked_mul(i64::from(page_size))
            .ok_or_else(|| AiRepositoryError::Integrity("候选分页页码无效".to_owned()))?;
        let mut statement = connection.prepare(
            "SELECT id, run_id, chunk_id, candidate_key, title, input_id, segment_ids_json, start_ms, end_ms, total_score, hook_score, information_score, emotion_score, tag_relevance_score, completeness_score, shareability_score, reason, matched_tags_json, rank, selected FROM ai_highlight_candidates WHERE run_id = ?1 AND selected = 1 ORDER BY COALESCE(rank, 999999), total_score DESC, id LIMIT ?2 OFFSET ?3",
        )?;
        let rows = statement.query_map(
            params![run_id, i64::from(page_size), offset],
            map_highlight_candidate,
        )?;
        let items = rows
            .collect::<std::result::Result<Vec<_>, _>>()?
            .into_iter()
            .map(parse_highlight_candidate)
            .collect::<Result<Vec<_>>>()?;
        Ok(AiHighlightCandidatePage {
            items,
            page,
            page_size,
            total_candidates: u64::try_from(total_candidates)
                .map_err(|_| AiRepositoryError::Integrity("候选总数无效".to_owned()))?,
            qualified_candidates: u64::try_from(qualified_candidates)
                .map_err(|_| AiRepositoryError::Integrity("合格候选数无效".to_owned()))?,
            selected_candidates: u64::try_from(selected_candidates)
                .map_err(|_| AiRepositoryError::Integrity("已选择候选数无效".to_owned()))?,
        })
    }

    pub fn select_highlight_candidates(
        &self,
        run_id: i64,
        ids: &[i64],
    ) -> Result<Vec<AiHighlightCandidate>> {
        let mut connection = self.database.connection()?;
        let transaction = connection.transaction()?;
        transaction.execute(
            "UPDATE ai_highlight_candidates SET selected = 0 WHERE run_id = ?1",
            [run_id],
        )?;
        for id in ids {
            let changed = transaction.execute(
                "UPDATE ai_highlight_candidates SET selected = 1 WHERE run_id = ?1 AND id = ?2",
                params![run_id, id],
            )?;
            if changed == 0 {
                return Err(AiRepositoryError::NotFound("精彩候选"));
            }
        }
        transaction.commit()?;
        drop(connection);
        self.list_highlight_candidates(run_id)
    }

    /// UI 的分页选择必须只影响一个候选，避免当前页提交覆盖其它页的选择状态。
    pub fn set_highlight_candidate_selected(
        &self,
        run_id: i64,
        candidate_id: i64,
        selected: bool,
    ) -> Result<AiHighlightCandidate> {
        let changed = self.database.connection()?.execute(
            r#"UPDATE ai_highlight_candidates
               SET selected = ?1
               WHERE id = ?2
                 AND run_id = ?3
                 AND total_score >= (SELECT qualified_score FROM ai_highlight_runs WHERE id = ?3)"#,
            params![selected, candidate_id, run_id],
        )?;
        if changed == 0 {
            return Err(AiRepositoryError::InvalidState(
                "只能选择达到本次合格阈值的精彩候选".to_owned(),
            ));
        }
        self.list_highlight_candidates(run_id)?
            .into_iter()
            .find(|candidate| candidate.id == candidate_id)
            .ok_or(AiRepositoryError::NotFound("精彩候选"))
    }

    pub fn get_or_create_clip_project(&self, run_id: i64) -> Result<AiClipProjectDetail> {
        let mut connection = self.database.connection()?;
        let transaction = connection.transaction()?;
        let existing = transaction
            .query_row(
                "SELECT id FROM ai_clip_projects WHERE highlight_run_id = ?1 AND smart_workflow_id IS NULL",
                [run_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        let project_id = if let Some(id) = existing {
            id
        } else {
            let (project_name, status, selected_count) = transaction.query_row(
                r#"SELECT p.name, r.status,
                          (SELECT COUNT(*) FROM ai_highlight_candidates WHERE run_id = r.id AND selected = 1)
                   FROM ai_highlight_runs r JOIN ai_projects p ON p.id = r.project_id WHERE r.id = ?1"#,
                [run_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                },
            )?;
            if !matches!(status.as_str(), "completed" | "partial") {
                return Err(AiRepositoryError::InvalidState(
                    "精彩分析尚未完成，暂时不能创建剪辑工程".to_owned(),
                ));
            }
            if selected_count == 0 {
                return Err(AiRepositoryError::InvalidState(
                    "请至少选择一个精彩候选后再编辑视频".to_owned(),
                ));
            }
            let now = Utc::now().to_rfc3339();
            transaction.execute(
                "INSERT INTO ai_clip_projects(highlight_run_id, name, export_status, export_progress, created_at, updated_at) VALUES(?1, ?2, 'idle', 0, ?3, ?3)",
                params![run_id, format!("{} - 精彩剪辑", project_name), now],
            )?;
            let id = transaction.last_insert_rowid();
            transaction.execute(
                r#"INSERT INTO ai_clip_project_sources(
                       clip_project_id, highlight_run_id, workflow_batch_id,
                       source_order, primary_source, created_at
                   ) VALUES(?1, ?2, NULL, 0, 1, ?3)"#,
                params![id, run_id, now],
            )?;
            transaction.execute(
                r#"INSERT INTO ai_clip_segments(clip_project_id, candidate_id, input_id, position, title, source_start_ms, source_end_ms, volume_percent, effect, created_at, updated_at)
                   SELECT ?1, c.id, c.input_id,
                          ROW_NUMBER() OVER (ORDER BY i.position, c.start_ms, c.id) - 1,
                          c.title, c.start_ms, c.end_ms, 100, 'none', ?2, ?2
                   FROM ai_highlight_candidates c
                   JOIN ai_project_inputs i ON i.id = c.input_id
                   WHERE c.run_id = ?3 AND c.selected = 1
                   ORDER BY i.position, c.start_ms, c.id"#,
                params![id, now, run_id],
            )?;
            id
        };
        seed_clip_subtitle_snapshots(&transaction, project_id, None)?;
        transaction.commit()?;
        drop(connection);
        crate::transition_materials::TransitionMaterialRepository::new(self.database.clone())
            .reconcile_boundaries(project_id)
            .map_err(|error| AiRepositoryError::Integrity(error.to_string()))?;
        self.get_clip_project(project_id)
    }

    pub fn list_clip_project_sources(
        &self,
        clip_project_id: i64,
    ) -> Result<Vec<AiClipProjectSource>> {
        let connection = self.database.connection()?;
        let mut statement = connection.prepare(
            r#"SELECT clip_project_id, highlight_run_id, workflow_batch_id,
                      source_order, primary_source
               FROM ai_clip_project_sources WHERE clip_project_id = ?1
               ORDER BY source_order, highlight_run_id"#,
        )?;
        statement
            .query_map([clip_project_id], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, bool>(4)?,
                ))
            })?
            .map(|row| {
                let row = row?;
                Ok(AiClipProjectSource {
                    clip_project_id: row.0,
                    highlight_run_id: row.1,
                    workflow_batch_id: row.2,
                    source_order: u32::try_from(row.3)
                        .map_err(|_| AiRepositoryError::Integrity("剪辑来源顺序损坏".to_owned()))?,
                    primary_source: row.4,
                })
            })
            .collect()
    }

    pub fn create_smart_clip_project(
        &self,
        workflow_id: i64,
        generation: u32,
        name: &str,
        sources: &[AiSmartClipSourceInput],
        candidate_ids: &[i64],
    ) -> Result<(AiClipProjectDetail, AiSmartDraft)> {
        if generation == 0 || sources.is_empty() || candidate_ids.is_empty() {
            return Err(AiRepositoryError::InvalidState(
                "智能合辑必须包含来源和入选候选".to_owned(),
            ));
        }
        let trimmed_name = name.trim();
        if trimmed_name.is_empty() {
            return Err(AiRepositoryError::InvalidState(
                "智能合辑名称不能为空".to_owned(),
            ));
        }
        let unique_sources = sources
            .iter()
            .map(|source| source.highlight_run_id)
            .collect::<std::collections::HashSet<_>>();
        let unique_candidates = candidate_ids
            .iter()
            .copied()
            .collect::<std::collections::HashSet<_>>();
        if unique_sources.len() != sources.len() || unique_candidates.len() != candidate_ids.len() {
            return Err(AiRepositoryError::Integrity(
                "智能合辑来源或候选不能重复".to_owned(),
            ));
        }
        let mut connection = self.database.connection()?;
        let transaction = connection.transaction()?;
        let workflow = transaction
            .query_row(
                "SELECT status FROM ai_smart_workflows WHERE id = ?1",
                [workflow_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .ok_or(AiRepositoryError::NotFound("智能成片任务"))?;
        if matches!(workflow.as_str(), "completed" | "cancelled") {
            return Err(AiRepositoryError::InvalidState(
                "已结束智能任务不能创建草稿".to_owned(),
            ));
        }
        if transaction
            .query_row(
                "SELECT 1 FROM ai_smart_drafts WHERE workflow_id = ?1 AND generation = ?2",
                params![workflow_id, generation],
                |_| Ok(()),
            )
            .optional()?
            .is_some()
        {
            return Err(AiRepositoryError::InvalidState(
                "该智能草稿代次已经存在".to_owned(),
            ));
        }
        let now = Utc::now().to_rfc3339();
        transaction.execute(
            r#"INSERT INTO ai_clip_projects(
                   highlight_run_id, smart_workflow_id, draft_generation, ownership,
                   name, export_status, export_progress, version, created_at, updated_at
               ) VALUES(?1, ?2, ?3, 'automation', ?4, 'idle', 0, 1, ?5, ?5)"#,
            params![
                sources[0].highlight_run_id,
                workflow_id,
                generation,
                trimmed_name,
                now,
            ],
        )?;
        let clip_project_id = transaction.last_insert_rowid();
        for (source_order, source) in sources.iter().enumerate() {
            // 允许 `failed`：**片段级前置**下，批次的文案纠正发生在合辑创建之前，
            // 批次可能已因纠正失败而标记为 Failed——它仍属于本任务，不能因此让
            // **其它**批次的合辑创建被整体拒绝（失败批次的候选为空，不产生内容）。
            // 这与另一处来源校验（见下方 `ai_clip_project_sources` 分支）语义一致。
            let valid = transaction
                .query_row(
                    r#"SELECT 1 FROM ai_smart_workflow_batches
                       WHERE id = ?1 AND workflow_id = ?2 AND highlight_run_id = ?3
                         AND status IN ('highlight', 'completed', 'failed')"#,
                    params![
                        source.workflow_batch_id,
                        workflow_id,
                        source.highlight_run_id
                    ],
                    |_| Ok(()),
                )
                .optional()?
                .is_some();
            if !valid {
                return Err(AiRepositoryError::Integrity(
                    "剪辑来源不属于当前智能任务的已处理批次".to_owned(),
                ));
            }
            transaction.execute(
                r#"INSERT INTO ai_clip_project_sources(
                       clip_project_id, highlight_run_id, workflow_batch_id,
                       source_order, primary_source, created_at
                   ) VALUES(?1, ?2, ?3, ?4, ?5, ?6)"#,
                params![
                    clip_project_id,
                    source.highlight_run_id,
                    source.workflow_batch_id,
                    i64::try_from(source_order).unwrap_or(i64::MAX),
                    source_order == 0,
                    now,
                ],
            )?;
        }
        let placeholders = std::iter::repeat_n("?", candidate_ids.len())
            .collect::<Vec<_>>()
            .join(",");
        let mut candidate_sql = format!(
            r#"SELECT c.id, c.input_id, c.title, c.start_ms, c.end_ms, source.source_order
               FROM ai_highlight_candidates c
               JOIN ai_clip_project_sources source
                 ON source.clip_project_id = ? AND source.highlight_run_id = c.run_id
               WHERE c.id IN ({placeholders}) AND c.selected = 1
               ORDER BY source.source_order,
                        (SELECT position FROM ai_project_inputs WHERE id = c.input_id),
                        c.start_ms, c.id"#
        );
        let mut bind_values = Vec::<rusqlite::types::Value>::with_capacity(candidate_ids.len() + 1);
        bind_values.push(clip_project_id.into());
        bind_values.extend(candidate_ids.iter().copied().map(Into::into));
        let candidates = transaction
            .prepare(&candidate_sql)?
            .query_map(rusqlite::params_from_iter(bind_values), |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        candidate_sql.clear();
        if candidates.len() != candidate_ids.len() {
            return Err(AiRepositoryError::Integrity(
                "智能合辑包含未入选或来源集合外候选".to_owned(),
            ));
        }
        for (position, candidate) in candidates.iter().enumerate() {
            transaction.execute(
                r#"INSERT INTO ai_clip_segments(
                       clip_project_id, candidate_id, input_id, position, title,
                       source_start_ms, source_end_ms, volume_percent, effect,
                       created_at, updated_at
                   ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, 100, 'none', ?8, ?8)"#,
                params![
                    clip_project_id,
                    candidate.0,
                    candidate.1,
                    i64::try_from(position).unwrap_or(i64::MAX),
                    candidate.2,
                    candidate.3,
                    candidate.4,
                    now,
                ],
            )?;
        }
        seed_clip_subtitle_snapshots(&transaction, clip_project_id, None)?;
        transaction.execute(
            r#"INSERT INTO ai_smart_drafts(
                   workflow_id, generation, clip_project_id, ownership, status,
                   automation_project_version, created_at, updated_at
               ) VALUES(?1, ?2, ?3, 'automation', 'active', 1, ?4, ?4)"#,
            params![workflow_id, generation, clip_project_id, now],
        )?;
        let draft_id = transaction.last_insert_rowid();
        transaction.execute(
            r#"UPDATE ai_smart_workflows
               SET active_draft_generation = ?1, stage = 'draft', status = 'running',
                   event_sequence = event_sequence + 1, updated_at = ?2
               WHERE id = ?3"#,
            params![generation, now, workflow_id],
        )?;
        transaction.commit()?;
        drop(connection);
        crate::transition_materials::TransitionMaterialRepository::new(self.database.clone())
            .reconcile_boundaries(clip_project_id)
            .map_err(|error| AiRepositoryError::Integrity(error.to_string()))?;
        let detail = self.get_clip_project(clip_project_id)?;
        let draft = self.get_smart_draft(draft_id)?;
        Ok((detail, draft))
    }

    pub fn append_smart_clip_candidates(
        &self,
        clip_project_id: i64,
        expected_version: u32,
        candidate_ids: &[i64],
    ) -> Result<AiClipProjectDetail> {
        if candidate_ids.is_empty() {
            return self.get_clip_project(clip_project_id);
        }
        let unique = candidate_ids
            .iter()
            .copied()
            .collect::<std::collections::HashSet<_>>();
        if unique.len() != candidate_ids.len() {
            return Err(AiRepositoryError::Integrity(
                "自动追加候选不能重复".to_owned(),
            ));
        }
        let mut connection = self.database.connection()?;
        let transaction = connection.transaction()?;
        let (version, ownership, workflow_id, draft_generation, export_status) = transaction
            .query_row(
                r#"SELECT version, ownership, smart_workflow_id, draft_generation, export_status
                   FROM ai_clip_projects WHERE id = ?1"#,
                [clip_project_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<i64>>(2)?,
                        row.get::<_, Option<i64>>(3)?,
                        row.get::<_, String>(4)?,
                    ))
                },
            )
            .optional()?
            .ok_or(AiRepositoryError::NotFound("剪辑工程"))?;
        if version != i64::from(expected_version)
            || ownership != "automation"
            || workflow_id.is_none()
            || draft_generation.is_none()
        {
            return Err(AiRepositoryError::ClipVersionConflict);
        }
        if export_status != "idle" {
            return Err(AiRepositoryError::ClipExportInProgress);
        }
        let placeholders = std::iter::repeat_n("?", candidate_ids.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            r#"SELECT c.id, c.input_id, c.title, c.start_ms, c.end_ms, source.source_order
               FROM ai_highlight_candidates c
               JOIN ai_clip_project_sources source
                 ON source.clip_project_id = ? AND source.highlight_run_id = c.run_id
               WHERE c.id IN ({placeholders}) AND c.selected = 1
                 AND NOT EXISTS (
                     SELECT 1 FROM ai_clip_segments segment
                     WHERE segment.clip_project_id = ? AND segment.candidate_id = c.id
                 )
               ORDER BY source.source_order,
                        (SELECT position FROM ai_project_inputs WHERE id = c.input_id),
                        c.start_ms, c.id"#
        );
        let mut bind_values = Vec::<rusqlite::types::Value>::with_capacity(candidate_ids.len() + 2);
        bind_values.push(clip_project_id.into());
        bind_values.extend(candidate_ids.iter().copied().map(Into::into));
        bind_values.push(clip_project_id.into());
        let candidates = transaction
            .prepare(&sql)?
            .query_map(rusqlite::params_from_iter(bind_values), |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        if candidates.len() != candidate_ids.len() {
            return Err(AiRepositoryError::Integrity(
                "自动追加包含重复、未入选或来源集合外候选".to_owned(),
            ));
        }
        let start_position = transaction.query_row(
            "SELECT COUNT(*) FROM ai_clip_segments WHERE clip_project_id = ?1",
            [clip_project_id],
            |row| row.get::<_, i64>(0),
        )?;
        let now = Utc::now().to_rfc3339();
        for (position, candidate) in (start_position..).zip(candidates) {
            transaction.execute(
                r#"INSERT INTO ai_clip_segments(
                       clip_project_id, candidate_id, input_id, position, title,
                       source_start_ms, source_end_ms, volume_percent, effect,
                       created_at, updated_at
                   ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, 100, 'none', ?8, ?8)"#,
                params![
                    clip_project_id,
                    candidate.0,
                    candidate.1,
                    position,
                    candidate.2,
                    candidate.3,
                    candidate.4,
                    now,
                ],
            )?;
            let segment_id = transaction.last_insert_rowid();
            seed_clip_subtitle_snapshots(&transaction, clip_project_id, Some(segment_id))?;
        }
        transaction.execute(
            r#"UPDATE ai_clip_projects SET version = version + 1, updated_at = ?1
               WHERE id = ?2 AND version = ?3 AND ownership = 'automation'"#,
            params![now, clip_project_id, expected_version],
        )?;
        transaction.execute(
            r#"UPDATE ai_smart_drafts
               SET automation_project_version = automation_project_version + 1, updated_at = ?1
               WHERE clip_project_id = ?2 AND ownership = 'automation'
                 AND automation_project_version = ?3"#,
            params![now, clip_project_id, expected_version],
        )?;
        transaction.commit()?;
        drop(connection);
        crate::transition_materials::TransitionMaterialRepository::new(self.database.clone())
            .reconcile_boundaries(clip_project_id)
            .map_err(|error| AiRepositoryError::Integrity(error.to_string()))?;
        self.get_clip_project(clip_project_id)
    }

    pub fn append_smart_clip_sources_and_candidates(
        &self,
        clip_project_id: i64,
        expected_version: u32,
        sources: &[AiSmartClipSourceInput],
        candidate_ids: &[i64],
    ) -> Result<AiClipProjectDetail> {
        if sources.is_empty() || candidate_ids.is_empty() {
            return Err(AiRepositoryError::InvalidState(
                "智能草稿增量必须包含来源和候选".to_owned(),
            ));
        }
        let mut connection = self.database.connection()?;
        let transaction = connection.transaction()?;
        let (version, ownership, workflow_id, export_status) = transaction
            .query_row(
                r#"SELECT version, ownership, smart_workflow_id, export_status
                   FROM ai_clip_projects WHERE id = ?1"#,
                [clip_project_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<i64>>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                },
            )
            .optional()?
            .ok_or(AiRepositoryError::NotFound("剪辑工程"))?;
        let workflow_id = workflow_id.ok_or_else(|| {
            AiRepositoryError::InvalidState("普通工程不能接受智能任务增量".to_owned())
        })?;
        if version != i64::from(expected_version) || ownership != "automation" {
            return Err(AiRepositoryError::ClipVersionConflict);
        }
        if export_status != "idle" {
            return Err(AiRepositoryError::ClipExportInProgress);
        }
        let now = Utc::now().to_rfc3339();
        let mut source_order = transaction.query_row(
            r#"SELECT COALESCE(MAX(source_order) + 1, 0)
               FROM ai_clip_project_sources WHERE clip_project_id = ?1"#,
            [clip_project_id],
            |row| row.get::<_, i64>(0),
        )?;
        for source in sources {
            let valid = transaction
                .query_row(
                    r#"SELECT 1 FROM ai_smart_workflow_batches batch
                       WHERE batch.id = ?1 AND batch.workflow_id = ?2
                         AND batch.highlight_run_id = ?3
                         AND (
                           batch.status IN ('highlight', 'completed')
                           OR EXISTS (
                             SELECT 1 FROM ai_clip_project_sources project_source
                             WHERE project_source.clip_project_id = ?4
                               AND project_source.workflow_batch_id = batch.id
                               AND project_source.highlight_run_id = batch.highlight_run_id
                           )
                         )"#,
                    params![
                        source.workflow_batch_id,
                        workflow_id,
                        source.highlight_run_id,
                        clip_project_id,
                    ],
                    |_| Ok(()),
                )
                .optional()?
                .is_some();
            if !valid {
                return Err(AiRepositoryError::Integrity(
                    "增量来源不属于当前智能任务的稳定批次".to_owned(),
                ));
            }
            let inserted = transaction.execute(
                r#"INSERT OR IGNORE INTO ai_clip_project_sources(
                       clip_project_id, highlight_run_id, workflow_batch_id,
                       source_order, primary_source, created_at
                   ) VALUES(?1, ?2, ?3, ?4, 0, ?5)"#,
                params![
                    clip_project_id,
                    source.highlight_run_id,
                    source.workflow_batch_id,
                    source_order,
                    now,
                ],
            )?;
            if inserted > 0 {
                source_order += 1;
            }
        }
        let ordered_source_runs = transaction
            .prepare(
                r#"SELECT source.highlight_run_id
                   FROM ai_clip_project_sources source
                   JOIN ai_smart_workflow_batches batch ON batch.id = source.workflow_batch_id
                   WHERE source.clip_project_id = ?1
                   ORDER BY batch.position, batch.id, source.highlight_run_id"#,
            )?
            .query_map([clip_project_id], |row| row.get::<_, i64>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        transaction.execute(
            r#"UPDATE ai_clip_project_sources SET source_order = source_order + 1000000000
               WHERE clip_project_id = ?1"#,
            [clip_project_id],
        )?;
        for (position, highlight_run_id) in ordered_source_runs.into_iter().enumerate() {
            transaction.execute(
                r#"UPDATE ai_clip_project_sources SET source_order = ?1
                   WHERE clip_project_id = ?2 AND highlight_run_id = ?3"#,
                params![
                    i64::try_from(position).unwrap_or(i64::MAX),
                    clip_project_id,
                    highlight_run_id,
                ],
            )?;
        }
        let unique_candidates = candidate_ids
            .iter()
            .copied()
            .collect::<std::collections::HashSet<_>>();
        if unique_candidates.len() != candidate_ids.len() {
            return Err(AiRepositoryError::Integrity(
                "智能草稿增量候选不能重复".to_owned(),
            ));
        }
        let placeholders = std::iter::repeat_n("?", candidate_ids.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            r#"SELECT c.id, c.input_id, c.title, c.start_ms, c.end_ms
               FROM ai_highlight_candidates c
               JOIN ai_clip_project_sources source
                 ON source.clip_project_id = ? AND source.highlight_run_id = c.run_id
               WHERE c.id IN ({placeholders}) AND c.selected = 1
                 AND NOT EXISTS (
                     SELECT 1 FROM ai_clip_segments segment
                     WHERE segment.clip_project_id = ? AND segment.candidate_id = c.id
                 )
               ORDER BY source.source_order,
                        (SELECT position FROM ai_project_inputs WHERE id = c.input_id),
                        c.start_ms, c.id"#
        );
        let mut bind_values = Vec::<rusqlite::types::Value>::with_capacity(candidate_ids.len() + 2);
        bind_values.push(clip_project_id.into());
        bind_values.extend(candidate_ids.iter().copied().map(Into::into));
        bind_values.push(clip_project_id.into());
        let candidates = transaction
            .prepare(&sql)?
            .query_map(rusqlite::params_from_iter(bind_values), |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let already_present = transaction.query_row(
            &format!(
                "SELECT COUNT(*) FROM ai_clip_segments WHERE clip_project_id = ? AND candidate_id IN ({placeholders})"
            ),
            rusqlite::params_from_iter(
                std::iter::once(rusqlite::types::Value::from(clip_project_id))
                    .chain(candidate_ids.iter().copied().map(Into::into)),
            ),
            |row| row.get::<_, i64>(0),
        )?;
        if candidates.len() + usize::try_from(already_present).unwrap_or_default()
            != candidate_ids.len()
        {
            return Err(AiRepositoryError::Integrity(
                "智能草稿增量包含未入选或来源集合外候选".to_owned(),
            ));
        }
        if candidates.is_empty() {
            transaction.commit()?;
            drop(connection);
            return self.get_clip_project(clip_project_id);
        }
        let start_position = transaction.query_row(
            "SELECT COUNT(*) FROM ai_clip_segments WHERE clip_project_id = ?1",
            [clip_project_id],
            |row| row.get::<_, i64>(0),
        )?;
        for (position, candidate) in (start_position..).zip(candidates) {
            transaction.execute(
                r#"INSERT INTO ai_clip_segments(
                       clip_project_id, candidate_id, input_id, position, title,
                       source_start_ms, source_end_ms, volume_percent, effect,
                       created_at, updated_at
                   ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, 100, 'none', ?8, ?8)"#,
                params![
                    clip_project_id,
                    candidate.0,
                    candidate.1,
                    position,
                    candidate.2,
                    candidate.3,
                    candidate.4,
                    now,
                ],
            )?;
            let segment_id = transaction.last_insert_rowid();
            seed_clip_subtitle_snapshots(&transaction, clip_project_id, Some(segment_id))?;
        }
        let ordered_segment_ids = transaction
            .prepare(
                r#"SELECT segment.id
                   FROM ai_clip_segments segment
                   JOIN ai_highlight_candidates candidate ON candidate.id = segment.candidate_id
                   JOIN ai_clip_project_sources source
                     ON source.clip_project_id = segment.clip_project_id
                    AND source.highlight_run_id = candidate.run_id
                   JOIN ai_project_inputs input ON input.id = segment.input_id
                   WHERE segment.clip_project_id = ?1
                   ORDER BY source.source_order, input.position,
                            segment.source_start_ms, segment.source_end_ms, segment.id"#,
            )?
            .query_map([clip_project_id], |row| row.get::<_, i64>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        transaction.execute(
            "UPDATE ai_clip_segments SET position = position + 1000000000 WHERE clip_project_id = ?1",
            [clip_project_id],
        )?;
        for (position, segment_id) in ordered_segment_ids.into_iter().enumerate() {
            transaction.execute(
                "UPDATE ai_clip_segments SET position = ?1 WHERE id = ?2",
                params![i64::try_from(position).unwrap_or(i64::MAX), segment_id],
            )?;
        }
        let changed = transaction.execute(
            r#"UPDATE ai_clip_projects SET version = version + 1, updated_at = ?1
               WHERE id = ?2 AND version = ?3 AND ownership = 'automation'
                 AND export_status = 'idle'"#,
            params![now, clip_project_id, expected_version],
        )?;
        let draft_changed = transaction.execute(
            r#"UPDATE ai_smart_drafts
               SET automation_project_version = automation_project_version + 1,
                   status = 'active', updated_at = ?1
               WHERE clip_project_id = ?2 AND ownership = 'automation'
                 AND automation_project_version = ?3
                 AND status IN ('active', 'review_ready')"#,
            params![now, clip_project_id, expected_version],
        )?;
        if changed != 1 || draft_changed != 1 {
            return Err(AiRepositoryError::ClipVersionConflict);
        }
        transaction.commit()?;
        drop(connection);
        crate::transition_materials::TransitionMaterialRepository::new(self.database.clone())
            .reconcile_boundaries(clip_project_id)
            .map_err(|error| AiRepositoryError::Integrity(error.to_string()))?;
        self.get_clip_project(clip_project_id)
    }

    pub fn mark_smart_draft_review_ready(
        &self,
        clip_project_id: i64,
        expected_version: u32,
    ) -> Result<AiSmartDraft> {
        let now = Utc::now().to_rfc3339();
        let changed = self.database.connection()?.execute(
            r#"UPDATE ai_smart_drafts
               SET status = 'review_ready', first_reviewable_at = COALESCE(first_reviewable_at, ?1),
                   updated_at = ?1
               WHERE clip_project_id = ?2 AND automation_project_version = ?3
                 AND status IN ('active', 'review_ready')"#,
            params![now, clip_project_id, expected_version],
        )?;
        if changed == 0 {
            return Err(AiRepositoryError::ClipVersionConflict);
        }
        let draft_id = self.database.connection()?.query_row(
            "SELECT id FROM ai_smart_drafts WHERE clip_project_id = ?1",
            [clip_project_id],
            |row| row.get::<_, i64>(0),
        )?;
        self.get_smart_draft(draft_id)
    }

    pub fn get_smart_draft(&self, draft_id: i64) -> Result<AiSmartDraft> {
        let row = self
            .database
            .connection()?
            .query_row(
                r#"SELECT id, workflow_id, generation, clip_project_id, ownership,
                          status, automation_project_version, frozen_project_version,
                          first_reviewable_at, created_at, updated_at
                   FROM ai_smart_drafts WHERE id = ?1"#,
                [draft_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, i64>(6)?,
                        row.get::<_, Option<i64>>(7)?,
                        row.get::<_, Option<String>>(8)?,
                        row.get::<_, String>(9)?,
                        row.get::<_, String>(10)?,
                    ))
                },
            )
            .optional()?
            .ok_or(AiRepositoryError::NotFound("智能草稿"))?;
        Ok(AiSmartDraft {
            id: row.0,
            workflow_id: row.1,
            generation: u32::try_from(row.2)
                .map_err(|_| AiRepositoryError::Integrity("草稿代次损坏".to_owned()))?,
            clip_project_id: row.3,
            ownership: AiSmartDraftOwnership::parse(&row.4)
                .ok_or_else(|| AiRepositoryError::Integrity("草稿所有权损坏".to_owned()))?,
            status: AiSmartDraftStatus::parse(&row.5)
                .ok_or_else(|| AiRepositoryError::Integrity("草稿状态损坏".to_owned()))?,
            automation_project_version: u32::try_from(row.6)
                .map_err(|_| AiRepositoryError::Integrity("自动草稿工程版本损坏".to_owned()))?,
            frozen_project_version: row
                .7
                .map(|value| {
                    u32::try_from(value).map_err(|_| {
                        AiRepositoryError::Integrity("冻结草稿工程版本损坏".to_owned())
                    })
                })
                .transpose()?,
            first_reviewable_at: row.8,
            created_at: row.9,
            updated_at: row.10,
        })
    }

    /// 写入/更新一批**句段级**文案纠正（v32）。
    ///
    /// 供"片段级前置"流程使用：校正发生在拼合辑之前，作用于转写句段，
    /// 之后生成的合辑字幕会取纠正后的文本。传入空文本表示撤销该句段的纠正。
    pub fn record_transcript_segment_corrections(
        &self,
        run_id: Option<i64>,
        corrections: &[(String, String, String)],
    ) -> Result<usize> {
        if corrections.is_empty() {
            return Ok(0);
        }
        let mut connection = self.database.connection()?;
        let transaction = connection.transaction()?;
        let now = Utc::now().to_rfc3339();
        let mut applied = 0_usize;
        for (segment_id, original_text, corrected_text) in corrections {
            let corrected = corrected_text.trim();
            if corrected.is_empty() {
                applied += transaction.execute(
                    "DELETE FROM transcript_segment_corrections WHERE segment_id = ?1",
                    [segment_id],
                )?;
                continue;
            }
            applied += transaction.execute(
                r#"INSERT INTO transcript_segment_corrections(
                       segment_id, original_text, corrected_text, run_id, created_at, updated_at
                   ) VALUES(?1, ?2, ?3, ?4, ?5, ?5)
                   ON CONFLICT(segment_id) DO UPDATE SET
                       corrected_text = excluded.corrected_text,
                       run_id = excluded.run_id,
                       updated_at = excluded.updated_at"#,
                params![segment_id, original_text, corrected, run_id, now],
            )?;
        }
        transaction.commit()?;
        Ok(applied)
    }

    /// 读取指定句段的纠正文本（未纠正的不出现）。
    pub fn transcript_segment_corrections(
        &self,
        segment_ids: &[String],
    ) -> Result<HashMap<String, String>> {
        if segment_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let connection = self.database.connection()?;
        let mut statement = connection.prepare(
            "SELECT segment_id, corrected_text FROM transcript_segment_corrections WHERE segment_id = ?1",
        )?;
        let mut result = HashMap::new();
        for segment_id in segment_ids {
            if let Some(text) = statement
                .query_row([segment_id], |row| row.get::<_, String>(1))
                .optional()?
            {
                result.insert(segment_id.clone(), text);
            }
        }
        Ok(result)
    }

    pub fn get_clip_project(&self, clip_project_id: i64) -> Result<AiClipProjectDetail> {
        let transition_repository =
            crate::transition_materials::TransitionMaterialRepository::new(self.database.clone());
        let boundaries = transition_repository
            .list_boundaries(clip_project_id)
            .map_err(|error| AiRepositoryError::Integrity(error.to_string()))?;
        let mut bridge_materials = HashMap::new();
        for boundary in boundaries.iter().filter(|item| item.active && !item.stale) {
            if let (Some(key), Some(version)) = (&boundary.asset_key, boundary.asset_version) {
                // 素材可能已被目录更新下架（服务端 `is_current` 变化）。**读操作不能因为
                // 一条失效引用而整体失败**——否则用户连工程都打不开（实测报
                // `找不到转场素材`）。这里跳过该边界，让工程仍可打开；导出路径会在
                // `source_for_export` 处严格校验并给出可操作错误。
                let (Ok(material), Ok(download)) = (
                    transition_repository.material(key, version),
                    transition_repository.download(key, version),
                ) else {
                    if cfg!(debug_assertions) {
                        eprintln!(
                            "[clip-project] 转场素材不可用，已跳过边界 {} 的桥接素材：{key} v{version}",
                            boundary.id
                        );
                    }
                    continue;
                };
                bridge_materials.insert(
                    (boundary.left_stable_id, boundary.right_stable_id),
                    (boundary.id, material, download),
                );
            }
        }
        let mut connection = self.database.connection()?;
        {
            let transaction = connection.transaction()?;
            seed_clip_subtitle_snapshots(&transaction, clip_project_id, None)?;
            transaction.commit()?;
        }
        let project = connection
            .query_row(
                "SELECT id, highlight_run_id, name, export_status, export_progress, output_path, last_error_code, last_error_message, output_width, output_height, version, created_at, updated_at, smart_workflow_id, draft_generation, ownership, export_frozen_version FROM ai_clip_projects WHERE id = ?1",
                [clip_project_id],
                map_clip_project,
            )
            .optional()?
            .ok_or(AiRepositoryError::NotFound("剪辑工程"))?;
        let mut statement = connection.prepare(
            "SELECT id, clip_project_id, candidate_id, input_id, position, title, source_start_ms, source_end_ms, volume_percent, effect FROM ai_clip_segments WHERE clip_project_id = ?1 ORDER BY position, id",
        )?;
        let segments = statement
            .query_map([clip_project_id], map_clip_segment)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut subtitle_statement = connection.prepare(
            r#"SELECT subtitle.id, subtitle.clip_project_id, subtitle.stable_segment_id,
                      subtitle.clip_segment_id, subtitle.input_id, subtitle.original_text,
                      subtitle.text, subtitle.hidden, subtitle.source_start_ms,
                      subtitle.source_end_ms
               FROM ai_clip_subtitles subtitle
               JOIN ai_clip_segments segment ON segment.id = subtitle.clip_segment_id
               WHERE subtitle.clip_project_id = ?1
               ORDER BY segment.position, subtitle.source_start_ms, subtitle.id"#,
        )?;
        let mut subtitles = subtitle_statement
            .query_map([clip_project_id], map_clip_subtitle)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut project_cursor_ms = 0_u64;
        // 已校验的 `insert` 生成资产：按 (左片段, 右片段) 索引。它占用时间轴并增加工程
        // 总时长，因此预览与导出必须用同一份时长口径（否则字幕会与画面错位）。
        let mut insert_assets: HashMap<(i64, i64), AiGeneratedAnimationAsset> = HashMap::new();
        if let Some(ai_project_id) = connection
            .query_row(
                "SELECT project_id FROM ai_highlight_runs WHERE id = ?1",
                [project.highlight_run_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
        {
            let mut asset_statement = connection.prepare(&format!(
                "SELECT {GENERATED_ANIMATION_COLUMNS} FROM ai_generated_animation_assets WHERE project_id = ?1 AND status = 'validated' AND composition = 'insert' ORDER BY id"
            ))?;
            let rows = asset_statement
                .query_map([ai_project_id], map_generated_animation)?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            for row in rows {
                let asset = parse_generated_animation(row)?;
                if let Some(binding) = &asset.insert_binding {
                    insert_assets.insert((binding.left_segment_id, binding.right_segment_id), asset);
                }
            }
        }
        let mut segment_offsets = HashMap::new();
        let mut timeline_units = Vec::new();
        for (index, segment) in segments.iter().enumerate() {
            let segment_start = project_cursor_ms;
            segment_offsets.insert(segment.id, (project_cursor_ms, segment.source_start_ms));
            project_cursor_ms = project_cursor_ms.saturating_add(
                segment
                    .source_end_ms
                    .saturating_sub(segment.source_start_ms),
            );
            timeline_units.push(AiClipTimelineUnit {
                key: format!("segment:{}", segment.id),
                kind: AiClipTimelineUnitKind::Segment,
                project_start_ms: segment_start,
                project_end_ms: project_cursor_ms,
                clip_segment_id: Some(segment.id),
                boundary_id: None,
                title: segment.title.clone(),
                asset_key: None,
                asset_version: None,
                source_status: None,
                preview_status: None,
                generated_asset_id: None,
                composition: None,
                asset_status: None,
            });
            if let Some(right) = segments.get(index + 1) {
                if let Some((boundary_id, material, download)) =
                    bridge_materials.get(&(segment.id, right.id))
                {
                    let bridge_start = project_cursor_ms;
                    project_cursor_ms = project_cursor_ms.saturating_add(material.duration_ms);
                    timeline_units.push(AiClipTimelineUnit {
                        key: format!("bridge:{boundary_id}"),
                        kind: AiClipTimelineUnitKind::Bridge,
                        project_start_ms: bridge_start,
                        project_end_ms: project_cursor_ms,
                        clip_segment_id: None,
                        boundary_id: Some(*boundary_id),
                        title: material.title.clone(),
                        asset_key: Some(material.asset_key.clone()),
                        asset_version: Some(material.asset_version),
                        source_status: Some(download.source_status),
                        preview_status: Some(download.preview_status),
                        generated_asset_id: None,
                        composition: None,
                        asset_status: None,
                    });
                } else if let Some(asset) = insert_assets.get(&(segment.id, right.id)) {
                    // 生成式动画单元与桥接互斥地占用同一边界。
                    let insert_start = project_cursor_ms;
                    project_cursor_ms =
                        project_cursor_ms.saturating_add(asset.duration_ms.unwrap_or(0));
                    timeline_units.push(AiClipTimelineUnit {
                        key: format!("animation:{}", asset.id),
                        kind: AiClipTimelineUnitKind::Animation,
                        project_start_ms: insert_start,
                        project_end_ms: project_cursor_ms,
                        clip_segment_id: None,
                        boundary_id: asset
                            .insert_binding
                            .as_ref()
                            .map(|binding| binding.boundary_id),
                        title: "生成式动画".to_owned(),
                        asset_key: None,
                        asset_version: None,
                        source_status: None,
                        preview_status: None,
                        generated_asset_id: Some(asset.id),
                        composition: Some(asset.composition),
                        asset_status: Some(asset.status),
                    });
                }
            }
        }
        for subtitle in &mut subtitles {
            let Some((cursor, segment_start_ms)) = segment_offsets.get(&subtitle.clip_segment_id)
            else {
                return Err(AiRepositoryError::Integrity(
                    "工程字幕引用的片段不存在".to_owned(),
                ));
            };
            subtitle.project_start_ms =
                cursor.saturating_add(subtitle.source_start_ms.saturating_sub(*segment_start_ms));
            subtitle.project_end_ms =
                cursor.saturating_add(subtitle.source_end_ms.saturating_sub(*segment_start_ms));
        }
        let subtitles_complete = !segments.is_empty()
            && segments.iter().all(|segment| {
                subtitles
                    .iter()
                    .any(|subtitle| subtitle.clip_segment_id == segment.id)
            });
        let dimensions = super::ClipOutputDimensions {
            width: project.output_width.unwrap_or(1_920),
            height: project.output_height.unwrap_or(1_080),
        };
        let subtitle_frames =
            super::build_clip_subtitle_frames(&subtitles, dimensions, project_cursor_ms);
        Ok(AiClipProjectDetail {
            project,
            segments,
            subtitles,
            subtitle_frames,
            subtitles_complete,
            boundaries,
            project_duration_ms: project_cursor_ms,
            timeline_units,
        })
    }

    pub fn update_clip_segment(
        &self,
        clip_project_id: i64,
        segment_id: i64,
        update: &AiClipSegmentUpdate,
    ) -> Result<AiClipProjectDetail> {
        if update.volume_percent > 200 {
            return Err(AiRepositoryError::Integrity(
                "片段音量必须在 0% 到 200% 之间".to_owned(),
            ));
        }
        let mut connection = self.database.connection()?;
        let transaction = connection.transaction()?;
        ensure_clip_project_editable(&transaction, clip_project_id)?;
        let now = Utc::now().to_rfc3339();
        let changed = transaction.execute(
            "UPDATE ai_clip_segments SET volume_percent = ?1, effect = ?2, updated_at = ?3 WHERE id = ?4 AND clip_project_id = ?5",
            params![i64::from(update.volume_percent), update.effect.as_str(), now, segment_id, clip_project_id],
        )?;
        if changed == 0 {
            return Err(AiRepositoryError::NotFound("剪辑片段"));
        }
        mark_clip_project_edited(&transaction, clip_project_id, &now)?;
        transaction.commit()?;
        drop(connection);
        crate::transition_materials::TransitionMaterialRepository::new(self.database.clone())
            .reconcile_boundaries(clip_project_id)
            .map_err(|error| AiRepositoryError::Integrity(error.to_string()))?;
        self.get_clip_project(clip_project_id)
    }

    pub fn update_clip_subtitle(
        &self,
        clip_project_id: i64,
        subtitle_id: i64,
        update: &AiClipSubtitleUpdate,
    ) -> Result<AiClipProjectDetail> {
        let text = normalize_clip_subtitle_text(&update.text)?;
        self.mutate_clip_subtitle(
            clip_project_id,
            subtitle_id,
            update.expected_project_version,
            Some((&text, update.hidden)),
        )
    }

    pub fn reset_clip_subtitle(
        &self,
        clip_project_id: i64,
        subtitle_id: i64,
        expected_project_version: u32,
    ) -> Result<AiClipProjectDetail> {
        self.mutate_clip_subtitle(clip_project_id, subtitle_id, expected_project_version, None)
    }

    fn mutate_clip_subtitle(
        &self,
        clip_project_id: i64,
        subtitle_id: i64,
        expected_project_version: u32,
        update: Option<(&str, bool)>,
    ) -> Result<AiClipProjectDetail> {
        let mut connection = self.database.connection()?;
        let transaction = connection.transaction()?;
        let (status, version) = transaction
            .query_row(
                "SELECT export_status, version FROM ai_clip_projects WHERE id = ?1",
                [clip_project_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
            )
            .optional()?
            .ok_or(AiRepositoryError::NotFound("剪辑工程"))?;
        if status == AiClipExportStatus::Exporting.as_str() {
            return Err(AiRepositoryError::ClipExportInProgress);
        }
        if version != i64::from(expected_project_version) {
            return Err(AiRepositoryError::ClipVersionConflict);
        }
        let now = Utc::now().to_rfc3339();
        let changed = if let Some((text, hidden)) = update {
            transaction.execute(
                r#"UPDATE ai_clip_subtitles
                   SET text = ?1, hidden = ?2, updated_at = ?3
                   WHERE id = ?4 AND clip_project_id = ?5"#,
                params![text, hidden, now, subtitle_id, clip_project_id],
            )?
        } else {
            transaction.execute(
                r#"UPDATE ai_clip_subtitles
                   SET text = original_text, hidden = 0, updated_at = ?1
                   WHERE id = ?2 AND clip_project_id = ?3"#,
                params![now, subtitle_id, clip_project_id],
            )?
        };
        if changed == 0 {
            return Err(AiRepositoryError::NotFound("工程字幕"));
        }
        mark_clip_project_edited(&transaction, clip_project_id, &now)?;
        transaction.commit()?;
        drop(connection);
        crate::transition_materials::TransitionMaterialRepository::new(self.database.clone())
            .reconcile_boundaries(clip_project_id)
            .map_err(|error| AiRepositoryError::Integrity(error.to_string()))?;
        self.get_clip_project(clip_project_id)
    }

    pub fn prepare_clip_text_correction(
        &self,
        clip_project_id: i64,
        expected_project_version: u32,
    ) -> Result<ClipTextCorrectionPlan> {
        let connection = self.database.connection()?;
        let (status, version) = connection
            .query_row(
                "SELECT export_status, version FROM ai_clip_projects WHERE id = ?1",
                [clip_project_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
            )
            .optional()?
            .ok_or(AiRepositoryError::NotFound("剪辑工程"))?;
        if status == AiClipExportStatus::Exporting.as_str() {
            return Err(AiRepositoryError::ClipExportInProgress);
        }
        if version != i64::from(expected_project_version) {
            return Err(AiRepositoryError::ClipVersionConflict);
        }
        let mut statement = connection.prepare(
            r#"SELECT subtitle.id, subtitle.original_text, subtitle.text, subtitle.hidden
               FROM ai_clip_subtitles subtitle
               JOIN ai_clip_segments segment ON segment.id = subtitle.clip_segment_id
               WHERE subtitle.clip_project_id = ?1
               ORDER BY segment.position, subtitle.source_start_ms, subtitle.id"#,
        )?;
        let rows = statement
            .query_map([clip_project_id], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, bool>(3)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut targets = Vec::new();
        let mut skipped_manual = 0;
        let mut skipped_hidden = 0;
        for (subtitle_id, original_text, text, hidden) in rows {
            if hidden {
                skipped_hidden += 1;
            } else if text != original_text {
                skipped_manual += 1;
            } else {
                targets.push(ClipTextCorrectionTarget { subtitle_id, text });
            }
        }
        Ok(ClipTextCorrectionPlan {
            project_version: u32::try_from(version)
                .map_err(|_| AiRepositoryError::Integrity("剪辑工程版本无效".to_owned()))?,
            targets,
            skipped_manual,
            skipped_hidden,
        })
    }

    pub fn apply_clip_text_corrections(
        &self,
        clip_project_id: i64,
        expected_project_version: u32,
        updates: &[ClipTextCorrectionUpdate],
    ) -> Result<(AiClipProjectDetail, usize)> {
        self.apply_clip_text_corrections_with_ownership(
            clip_project_id,
            expected_project_version,
            updates,
            false,
        )
    }

    /// 智能编排器使用的纠错提交路径。它与人工纠错共享同一套逐字幕比较和
    /// 原子提交规则，但成功时保持自动化所有权，避免后续直播精彩被错误地
    /// 路由到新草稿代次。
    pub fn apply_automated_clip_text_corrections(
        &self,
        clip_project_id: i64,
        expected_project_version: u32,
        updates: &[ClipTextCorrectionUpdate],
    ) -> Result<(AiClipProjectDetail, usize)> {
        self.apply_clip_text_corrections_with_ownership(
            clip_project_id,
            expected_project_version,
            updates,
            true,
        )
    }

    fn apply_clip_text_corrections_with_ownership(
        &self,
        clip_project_id: i64,
        expected_project_version: u32,
        updates: &[ClipTextCorrectionUpdate],
        automated: bool,
    ) -> Result<(AiClipProjectDetail, usize)> {
        let normalized = updates
            .iter()
            .map(|update| {
                Ok((
                    update,
                    normalize_clip_subtitle_text(&update.corrected_text)?,
                ))
            })
            .collect::<Result<Vec<_>>>()?;
        let mut connection = self.database.connection()?;
        let transaction = connection.transaction()?;
        let version = ensure_clip_project_editable(&transaction, clip_project_id)?;
        if version != expected_project_version {
            return Err(AiRepositoryError::ClipVersionConflict);
        }
        let now = Utc::now().to_rfc3339();
        let mut changed_items = 0;
        for (update, corrected_text) in normalized {
            let current = transaction
                .query_row(
                    r#"SELECT original_text, text, hidden
                       FROM ai_clip_subtitles
                       WHERE id = ?1 AND clip_project_id = ?2"#,
                    params![update.subtitle_id, clip_project_id],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, bool>(2)?,
                        ))
                    },
                )
                .optional()?
                .ok_or(AiRepositoryError::NotFound("工程字幕"))?;
            if current.2 || current.0 != current.1 || current.1 != update.expected_text {
                return Err(AiRepositoryError::ClipVersionConflict);
            }
            if corrected_text != current.1 {
                transaction.execute(
                    r#"UPDATE ai_clip_subtitles SET text = ?1, updated_at = ?2
                       WHERE id = ?3 AND clip_project_id = ?4"#,
                    params![corrected_text, now, update.subtitle_id, clip_project_id],
                )?;
                changed_items += 1;
            }
        }
        if changed_items > 0 {
            if automated {
                mark_clip_project_automated(&transaction, clip_project_id, &now)?;
            } else {
                mark_clip_project_edited(&transaction, clip_project_id, &now)?;
            }
        }
        transaction.commit()?;
        drop(connection);
        Ok((self.get_clip_project(clip_project_id)?, changed_items))
    }

    #[allow(clippy::too_many_arguments)]
    pub fn begin_clip_text_correction_run(
        &self,
        clip_project_id: i64,
        project_version: u32,
        model_id: &str,
        prompt_version: &str,
        input_fingerprint: &str,
        total_items: usize,
        skipped_manual: usize,
        skipped_hidden: usize,
        total_batches: usize,
    ) -> Result<i64> {
        let connection = self.database.connection()?;
        connection.execute(
            r#"INSERT INTO ai_clip_text_correction_runs(
                   clip_project_id, project_version, status, stage, model_id,
                   prompt_version, input_fingerprint, total_items, skipped_manual,
                   skipped_hidden, total_batches, created_at, updated_at
               ) VALUES(?1, ?2, 'running', 'correcting', ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10)"#,
            params![
                clip_project_id,
                project_version,
                model_id,
                prompt_version,
                input_fingerprint,
                i64::try_from(total_items).unwrap_or(i64::MAX),
                i64::try_from(skipped_manual).unwrap_or(i64::MAX),
                i64::try_from(skipped_hidden).unwrap_or(i64::MAX),
                i64::try_from(total_batches).unwrap_or(i64::MAX),
                Utc::now().to_rfc3339(),
            ],
        )?;
        Ok(connection.last_insert_rowid())
    }

    pub fn update_clip_text_correction_run(
        &self,
        run_id: i64,
        stage: &str,
        completed_batches: usize,
        processed_items: usize,
        token_usage: u64,
    ) -> Result<()> {
        let changed = self.database.connection()?.execute(
            r#"UPDATE ai_clip_text_correction_runs
               SET stage = ?1, completed_batches = ?2, processed_items = ?3,
                   token_usage = ?4, updated_at = ?5
               WHERE id = ?6 AND status = 'running'"#,
            params![
                stage,
                i64::try_from(completed_batches).unwrap_or(i64::MAX),
                i64::try_from(processed_items).unwrap_or(i64::MAX),
                i64::try_from(token_usage).unwrap_or(i64::MAX),
                Utc::now().to_rfc3339(),
                run_id,
            ],
        )?;
        if changed == 0 {
            return Err(AiRepositoryError::NotFound("文本纠错运行"));
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn finish_clip_text_correction_run(
        &self,
        run_id: i64,
        status: &str,
        stage: &str,
        processed_items: usize,
        changed_items: usize,
        unchanged_items: usize,
        completed_batches: usize,
        token_usage: u64,
        error: Option<(&str, &str)>,
    ) -> Result<()> {
        let (error_code, error_message) = error
            .map(|(code, message)| {
                (
                    Some(code.to_owned()),
                    Some(
                        message
                            .trim()
                            .chars()
                            .filter(|character| !character.is_control())
                            .take(256)
                            .collect::<String>(),
                    ),
                )
            })
            .unwrap_or((None, None));
        let changed = self.database.connection()?.execute(
            r#"UPDATE ai_clip_text_correction_runs
               SET status = ?1, stage = ?2, processed_items = ?3,
                   changed_items = ?4, unchanged_items = ?5, completed_batches = ?6,
                   token_usage = ?7, error_code = ?8, error_message = ?9, updated_at = ?10
               WHERE id = ?11 AND status = 'running'"#,
            params![
                status,
                stage,
                i64::try_from(processed_items).unwrap_or(i64::MAX),
                i64::try_from(changed_items).unwrap_or(i64::MAX),
                i64::try_from(unchanged_items).unwrap_or(i64::MAX),
                i64::try_from(completed_batches).unwrap_or(i64::MAX),
                i64::try_from(token_usage).unwrap_or(i64::MAX),
                error_code,
                error_message,
                Utc::now().to_rfc3339(),
                run_id,
            ],
        )?;
        if changed == 0 {
            return Err(AiRepositoryError::NotFound("文本纠错运行"));
        }
        Ok(())
    }

    pub fn insert_clip_candidate(
        &self,
        clip_project_id: i64,
        candidate_id: i64,
        insert_index: u32,
    ) -> Result<AiClipProjectDetail> {
        let mut connection = self.database.connection()?;
        let transaction = connection.transaction()?;
        ensure_clip_project_editable(&transaction, clip_project_id)?;
        let segment_count = transaction.query_row(
            "SELECT COUNT(*) FROM ai_clip_segments WHERE clip_project_id = ?1",
            [clip_project_id],
            |row| row.get::<_, i64>(0),
        )?;
        if i64::from(insert_index) > segment_count {
            return Err(AiRepositoryError::Integrity(
                "视频素材插入位置超出时间轴范围".to_owned(),
            ));
        }
        let candidate = transaction
            .query_row(
                r#"SELECT c.input_id, c.title, c.start_ms, c.end_ms
                   FROM ai_clip_project_sources source
                   JOIN ai_highlight_candidates c ON c.run_id = source.highlight_run_id
                   WHERE source.clip_project_id = ?1 AND c.id = ?2 AND c.selected = 1"#,
                params![clip_project_id, candidate_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                    ))
                },
            )
            .optional()?
            .ok_or_else(|| {
                AiRepositoryError::Integrity(
                    "只能追加当前工程来源集合中已进入待切片的视频".to_owned(),
                )
            })?;
        if transaction
            .query_row(
                "SELECT 1 FROM ai_clip_segments WHERE clip_project_id = ?1 AND candidate_id = ?2",
                params![clip_project_id, candidate_id],
                |_| Ok(()),
            )
            .optional()?
            .is_some()
        {
            return Err(AiRepositoryError::Integrity(
                "该视频片段已经在时间轴中".to_owned(),
            ));
        }
        let now = Utc::now().to_rfc3339();
        transaction.execute(
            "UPDATE ai_clip_segments SET position = position + 1, updated_at = ?1 WHERE clip_project_id = ?2 AND position >= ?3",
            params![now, clip_project_id, i64::from(insert_index)],
        )?;
        transaction.execute(
            r#"INSERT INTO ai_clip_segments(
                   clip_project_id, candidate_id, input_id, position, title,
                   source_start_ms, source_end_ms, volume_percent, effect, created_at, updated_at
               ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, 100, 'none', ?8, ?8)"#,
            params![
                clip_project_id,
                candidate_id,
                candidate.0,
                i64::from(insert_index),
                candidate.1,
                candidate.2,
                candidate.3,
                now,
            ],
        )?;
        let clip_segment_id = transaction.last_insert_rowid();
        seed_clip_subtitle_snapshots(&transaction, clip_project_id, Some(clip_segment_id))?;
        mark_clip_project_edited(&transaction, clip_project_id, &now)?;
        transaction.commit()?;
        drop(connection);
        crate::transition_materials::TransitionMaterialRepository::new(self.database.clone())
            .reconcile_boundaries(clip_project_id)
            .map_err(|error| AiRepositoryError::Integrity(error.to_string()))?;
        self.get_clip_project(clip_project_id)
    }

    pub fn reorder_clip_segments(
        &self,
        clip_project_id: i64,
        ordered_ids: &[i64],
    ) -> Result<AiClipProjectDetail> {
        let mut connection = self.database.connection()?;
        let transaction = connection.transaction()?;
        ensure_clip_project_editable(&transaction, clip_project_id)?;
        let existing = transaction
            .prepare(
                "SELECT id FROM ai_clip_segments WHERE clip_project_id = ?1 ORDER BY position, id",
            )?
            .query_map([clip_project_id], |row| row.get::<_, i64>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        if existing.len() != ordered_ids.len()
            || existing.iter().collect::<std::collections::HashSet<_>>()
                != ordered_ids.iter().collect::<std::collections::HashSet<_>>()
        {
            return Err(AiRepositoryError::Integrity(
                "重排必须包含工程中的全部片段且不能重复".to_owned(),
            ));
        }
        for (position, id) in ordered_ids.iter().enumerate() {
            transaction.execute(
                "UPDATE ai_clip_segments SET position = ?1, updated_at = ?2 WHERE id = ?3 AND clip_project_id = ?4",
                params![position as i64, Utc::now().to_rfc3339(), id, clip_project_id],
            )?;
        }
        let now = Utc::now().to_rfc3339();
        mark_clip_project_edited(&transaction, clip_project_id, &now)?;
        transaction.commit()?;
        drop(connection);
        crate::transition_materials::TransitionMaterialRepository::new(self.database.clone())
            .reconcile_boundaries(clip_project_id)
            .map_err(|error| AiRepositoryError::Integrity(error.to_string()))?;
        self.get_clip_project(clip_project_id)
    }

    pub fn remove_clip_segment(
        &self,
        clip_project_id: i64,
        segment_id: i64,
    ) -> Result<AiClipProjectDetail> {
        let mut connection = self.database.connection()?;
        let transaction = connection.transaction()?;
        ensure_clip_project_editable(&transaction, clip_project_id)?;
        if transaction.execute(
            "DELETE FROM ai_clip_segments WHERE id = ?1 AND clip_project_id = ?2",
            params![segment_id, clip_project_id],
        )? == 0
        {
            return Err(AiRepositoryError::NotFound("剪辑片段"));
        }
        let remaining = transaction.query_row(
            "SELECT COUNT(*) FROM ai_clip_segments WHERE clip_project_id = ?1",
            [clip_project_id],
            |row| row.get::<_, i64>(0),
        )?;
        if remaining == 0 {
            return Err(AiRepositoryError::InvalidState(
                "剪辑工程至少需要保留一个片段".to_owned(),
            ));
        }
        let rows = transaction
            .prepare(
                "SELECT id FROM ai_clip_segments WHERE clip_project_id = ?1 ORDER BY position, id",
            )?
            .query_map([clip_project_id], |row| row.get::<_, i64>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        for (position, id) in rows.iter().enumerate() {
            transaction.execute(
                "UPDATE ai_clip_segments SET position = ?1, updated_at = ?2 WHERE id = ?3",
                params![position as i64, Utc::now().to_rfc3339(), id],
            )?;
        }
        let now = Utc::now().to_rfc3339();
        mark_clip_project_edited(&transaction, clip_project_id, &now)?;
        transaction.commit()?;
        drop(connection);
        crate::transition_materials::TransitionMaterialRepository::new(self.database.clone())
            .reconcile_boundaries(clip_project_id)
            .map_err(|error| AiRepositoryError::Integrity(error.to_string()))?;
        self.get_clip_project(clip_project_id)
    }

    pub fn set_clip_export_state(
        &self,
        clip_project_id: i64,
        status: AiClipExportStatus,
        progress: u8,
        output_path: Option<&str>,
        error: Option<(&str, &str)>,
    ) -> Result<AiClipProject> {
        let (code, message) = error
            .map(|(code, message)| (Some(code), Some(message)))
            .unwrap_or((None, None));
        let now = Utc::now().to_rfc3339();
        let mut connection = self.database.connection()?;
        let transaction = connection.transaction()?;
        let changed = transaction.execute(
            "UPDATE ai_clip_projects SET export_status = ?1, export_progress = ?2, output_path = ?3, last_error_code = ?4, last_error_message = ?5, updated_at = ?6 WHERE id = ?7",
            params![status.as_str(), i64::from(progress), output_path, code, message, now, clip_project_id],
        )?;
        if changed == 0 {
            return Err(AiRepositoryError::NotFound("剪辑工程"));
        }
        let draft_status = match status {
            AiClipExportStatus::Exporting => Some("exporting"),
            AiClipExportStatus::Completed => Some("exported"),
            AiClipExportStatus::Cancelled => Some("frozen"),
            AiClipExportStatus::Failed => Some("failed"),
            AiClipExportStatus::Idle => None,
        };
        if let Some(draft_status) = draft_status {
            transaction.execute(
                "UPDATE ai_smart_drafts SET status = ?1, updated_at = ?2 WHERE clip_project_id = ?3",
                params![draft_status, now, clip_project_id],
            )?;
        }
        transaction.commit()?;
        drop(connection);
        self.get_clip_project(clip_project_id)
            .map(|detail| detail.project)
    }

    pub fn begin_clip_export(
        &self,
        clip_project_id: i64,
        expected_project_version: u32,
    ) -> Result<AiClipProject> {
        let mut connection = self.database.connection()?;
        let transaction = connection.transaction()?;
        let version = ensure_clip_project_editable(&transaction, clip_project_id)?;
        if version != expected_project_version {
            return Err(AiRepositoryError::ClipVersionConflict);
        }
        transaction.execute(
            r#"UPDATE ai_clip_projects
               SET export_status = 'exporting', export_progress = 0, output_path = NULL,
                   export_frozen_version = ?1,
                   last_error_code = NULL, last_error_message = NULL, updated_at = ?2
               WHERE id = ?3"#,
            params![
                expected_project_version,
                Utc::now().to_rfc3339(),
                clip_project_id
            ],
        )?;
        transaction.execute(
            r#"UPDATE ai_smart_drafts
               SET status = 'exporting', frozen_project_version = ?1, updated_at = ?2
               WHERE clip_project_id = ?3 AND frozen_project_version IS NULL"#,
            params![
                expected_project_version,
                Utc::now().to_rfc3339(),
                clip_project_id
            ],
        )?;
        transaction.commit()?;
        drop(connection);
        self.get_clip_project(clip_project_id)
            .map(|detail| detail.project)
    }

    pub fn set_clip_output_dimensions(
        &self,
        clip_project_id: i64,
        width: u32,
        height: u32,
    ) -> Result<AiClipProject> {
        if width == 0 || height == 0 || !width.is_multiple_of(2) || !height.is_multiple_of(2) {
            return Err(AiRepositoryError::Integrity(
                "剪辑输出尺寸必须是正偶数".to_owned(),
            ));
        }
        let mut connection = self.database.connection()?;
        let transaction = connection.transaction()?;
        ensure_clip_project_editable(&transaction, clip_project_id)?;
        let current = transaction
            .query_row(
                "SELECT output_width, output_height FROM ai_clip_projects WHERE id = ?1",
                [clip_project_id],
                |row| Ok((row.get::<_, Option<i64>>(0)?, row.get::<_, Option<i64>>(1)?)),
            )
            .optional()?
            .ok_or(AiRepositoryError::NotFound("剪辑工程"))?;
        if let (Some(current_width), Some(current_height)) = current {
            if current_width != i64::from(width) || current_height != i64::from(height) {
                return Err(AiRepositoryError::InvalidState(
                    "剪辑工程的输出画幅已经冻结".to_owned(),
                ));
            }
        } else {
            transaction.execute(
                "UPDATE ai_clip_projects SET output_width = ?1, output_height = ?2, updated_at = ?3 WHERE id = ?4",
                params![i64::from(width), i64::from(height), Utc::now().to_rfc3339(), clip_project_id],
            )?;
        }
        transaction.commit()?;
        drop(connection);
        self.get_clip_project(clip_project_id)
            .map(|detail| detail.project)
    }

    /// 导出子进程不会跨应用重启恢复；启动时将遗留的运行状态安全降级为可重试状态。
    pub fn recover_interrupted_clip_exports(&self) -> Result<u64> {
        let mut connection = self.database.connection()?;
        let transaction = connection.transaction()?;
        let changed = transaction.execute(
            r#"UPDATE ai_clip_projects
               SET export_status = 'cancelled', export_progress = 0, output_path = NULL,
                   last_error_code = 'app_restarted',
                   last_error_message = '上次视频导出因应用退出而取消，可重新导出',
                   updated_at = ?1
               WHERE export_status = 'exporting'"#,
            [Utc::now().to_rfc3339()],
        )?;
        transaction.execute(
            r#"UPDATE ai_smart_drafts SET status = 'failed', updated_at = ?1
               WHERE status = 'exporting'"#,
            [Utc::now().to_rfc3339()],
        )?;
        transaction.commit()?;
        Ok(changed as u64)
    }

    pub fn clip_export_sources(
        &self,
        clip_project_id: i64,
        generated_animation_root: &std::path::Path,
    ) -> Result<Vec<ClipExportSource>> {
        let detail = self.get_clip_project(clip_project_id)?;
        let connection = self.database.connection()?;
        if detail.project.export_status == AiClipExportStatus::Exporting {
            return Err(AiRepositoryError::InvalidState(
                "剪辑工程正在导出".to_owned(),
            ));
        }
        // 已校验的生成式动画资产：`replace` 落到片段源内范围，`insert` 绑定左侧片段。
        // 状态不是 `validated`、或缓存文件缺失的资产必须阻止导出，不得被静默跳过。
        let mut replace_by_segment: HashMap<i64, ClipExportReplace> = HashMap::new();
        let mut insert_by_segment: HashMap<i64, ClipExportInsert> = HashMap::new();
        if let Some(ai_project_id) = connection
            .query_row(
                "SELECT project_id FROM ai_highlight_runs WHERE id = ?1",
                [detail.project.highlight_run_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
        {
            let mut asset_statement = connection.prepare(&format!(
                "SELECT {GENERATED_ANIMATION_COLUMNS} FROM ai_generated_animation_assets WHERE project_id = ?1 ORDER BY id"
            ))?;
            let asset_rows = asset_statement
                .query_map([ai_project_id], map_generated_animation)?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            for row in asset_rows {
                let asset = parse_generated_animation(row)?;
                // 路径由约定推导、不入库：清理缓存不影响工程。
                let path = generated_animation_root
                    .join(GENERATED_ANIMATION_CACHE_DIR)
                    .join(generated_animation_file_name(asset.id));
                if asset.status != AiGeneratedAnimationStatus::Validated || !path.is_file() {
                    return Err(AiRepositoryError::InvalidState(format!(
                        "生成式动画资产 {} 缺失或未通过校验，无法导出",
                        asset.id
                    )));
                }
                let source_path = path.to_string_lossy().into_owned();
                match asset.composition {
                    AiGeneratedAnimationComposition::Replace => {
                        let Some(range) = &asset.replace_range else {
                            continue;
                        };
                        if let Some(segment) = detail.segments.iter().find(|segment| {
                            range.start_ms >= segment.source_start_ms
                                && range.end_ms <= segment.source_end_ms
                        }) {
                            replace_by_segment.insert(
                                segment.id,
                                ClipExportReplace {
                                    asset_path: source_path,
                                    start_ms: range.start_ms - segment.source_start_ms,
                                    end_ms: range.end_ms - segment.source_start_ms,
                                },
                            );
                        }
                    }
                    AiGeneratedAnimationComposition::Insert => {
                        let Some(binding) = &asset.insert_binding else {
                            continue;
                        };
                        insert_by_segment.insert(
                            binding.left_segment_id,
                            ClipExportInsert {
                                asset_path: source_path,
                                duration_ms: asset.duration_ms.unwrap_or(0),
                                left_segment_id: binding.left_segment_id,
                                right_segment_id: binding.right_segment_id,
                                boundary_id: binding.boundary_id,
                            },
                        );
                    }
                }
            }
        }
        let mut statement = connection.prepare(
            r#"SELECT s.id, s.clip_project_id, s.candidate_id, s.input_id, s.position, s.title,
                      s.source_start_ms, s.source_end_ms, s.volume_percent, s.effect,
                      i.source_path, i.source_fingerprint_json
               FROM ai_clip_segments s
               JOIN ai_project_inputs i ON i.id = s.input_id
               WHERE s.clip_project_id = ?1 ORDER BY s.position, s.id"#,
        )?;
        let rows = statement.query_map([clip_project_id], |row| {
            let segment = map_clip_segment(row)?;
            Ok((
                segment,
                row.get::<_, String>(10)?,
                row.get::<_, String>(11)?,
            ))
        })?;
        let sources = rows
            .collect::<std::result::Result<Vec<_>, _>>()?
            .into_iter()
            .map(|(segment, source_path, fingerprint_json)| {
                let source_fingerprint = serde_json::from_str(&fingerprint_json)
                    .map_err(|_| AiRepositoryError::Integrity("剪辑来源指纹数据损坏".to_owned()))?;
                let subtitles = detail
                    .subtitles
                    .iter()
                    .filter(|subtitle| subtitle.clip_segment_id == segment.id)
                    .cloned()
                    .collect();
                let replace = replace_by_segment.get(&segment.id).cloned();
                let insert_after = insert_by_segment.get(&segment.id).cloned();
                Ok(ClipExportSource {
                    segment,
                    source_path,
                    source_fingerprint,
                    subtitles,
                    bridge_after: None,
                    replace,
                    insert_after,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        if sources.is_empty() {
            return Err(AiRepositoryError::InvalidState(
                "剪辑工程至少需要一个片段才能导出".to_owned(),
            ));
        }
        Ok(sources)
    }

    pub fn transition_project(&self, project_id: i64, next: AiProjectStatus) -> Result<()> {
        let current = self.get_project_row(project_id)?.status;
        if !current.can_transition_to(next) {
            return Err(AiRepositoryError::InvalidState(format!(
                "项目不能从 {} 转为 {}",
                current.as_str(),
                next.as_str()
            )));
        }
        let now = Utc::now().to_rfc3339();
        self.database.connection()?.execute(
            r#"
            UPDATE ai_projects
            SET status = ?1,
                started_at = CASE WHEN ?1 = 'running' THEN COALESCE(started_at, ?2) ELSE started_at END,
                completed_at = CASE WHEN ?1 IN ('completed', 'completed_with_errors', 'cancelled', 'failed') THEN ?2 ELSE completed_at END,
                updated_at = ?2
            WHERE id = ?3
            "#,
            params![next.as_str(), now, project_id],
        )?;
        Ok(())
    }

    pub fn transition_input(
        &self,
        input_id: i64,
        next: AiInputStatus,
        error: Option<(&str, &str)>,
    ) -> Result<()> {
        let current = self.get_input(input_id)?.status;
        if !current.can_transition_to(next) {
            return Err(AiRepositoryError::InvalidState(format!(
                "输入不能从 {} 转为 {}",
                current.as_str(),
                next.as_str()
            )));
        }
        let (error_code, error_message) = error
            .map(|(code, message)| (Some(code), Some(message)))
            .unwrap_or((None, None));
        self.database.connection()?.execute(
            r#"
            UPDATE ai_project_inputs
            SET status = ?1, last_error_code = ?2, last_error_message = ?3, updated_at = ?4
            WHERE id = ?5
            "#,
            params![
                next.as_str(),
                error_code,
                error_message,
                Utc::now().to_rfc3339(),
                input_id
            ],
        )?;
        Ok(())
    }

    /// 根据持久化输入阶段聚合项目进度，并在全部输入终止时给出稳定最终状态。
    pub fn recompute_project_progress(&self, project_id: i64) -> Result<AiProject> {
        let current_project = self.get_project_row(project_id)?;
        let inputs = self.list_inputs(project_id)?;
        if inputs.is_empty() {
            return self.get_project_row(project_id);
        }
        let progress_total: u64 = inputs
            .iter()
            .map(|input| u64::from(input.status.progress_percent()))
            .sum();
        let progress_percent = (progress_total / inputs.len() as u64) as u8;
        let terminal = inputs.iter().all(|input| {
            matches!(
                input.status,
                AiInputStatus::Completed
                    | AiInputStatus::Skipped
                    | AiInputStatus::Cancelled
                    | AiInputStatus::Failed
            )
        });
        let next_status = if terminal && current_project.status == AiProjectStatus::Running {
            let successful = inputs
                .iter()
                .filter(|input| {
                    matches!(
                        input.status,
                        AiInputStatus::Completed | AiInputStatus::Skipped
                    )
                })
                .count();
            let failed = inputs
                .iter()
                .filter(|input| input.status == AiInputStatus::Failed)
                .count();
            let cancelled = inputs
                .iter()
                .filter(|input| input.status == AiInputStatus::Cancelled)
                .count();
            if successful > 0 && failed + cancelled > 0 {
                AiProjectStatus::CompletedWithErrors
            } else if successful > 0 {
                AiProjectStatus::Completed
            } else if cancelled == inputs.len() {
                AiProjectStatus::Cancelled
            } else {
                AiProjectStatus::Failed
            }
        } else {
            current_project.status
        };
        let now = Utc::now().to_rfc3339();
        self.database.connection()?.execute(
            r#"
            UPDATE ai_projects
            SET progress_percent = ?1, status = ?2,
                completed_at = CASE WHEN ?2 IN ('completed', 'completed_with_errors', 'cancelled', 'failed') THEN COALESCE(completed_at, ?3) ELSE completed_at END,
                updated_at = ?3
            WHERE id = ?4
            "#,
            params![progress_percent, next_status.as_str(), now, project_id],
        )?;
        self.get_project_row(project_id)
    }

    /// 付费提交前的本地幂等落盘。
    ///
    /// 语义：同键同输入返回已有记录（`reused`）；同键不同输入拒绝并返回
    /// `IdempotencyConflict`。该记录必须在调用上游之前写入，进程崩溃或断电后
    /// 仍能按 `request_id` 找回同一个上游任务，从而不产生第二次付费提交。
    pub fn record_remote_job(
        &self,
        key: &RemoteJobKey,
        upstream_task_id: Option<&str>,
    ) -> Result<RemoteJobRecord> {
        if key.request_id.trim().is_empty() || key.task_type.trim().is_empty() {
            return Err(AiRepositoryError::Integrity(
                "幂等请求标识与任务类型不能为空".to_owned(),
            ));
        }
        if key.input_hash.trim().is_empty() {
            return Err(AiRepositoryError::Integrity(
                "幂等输入指纹不能为空".to_owned(),
            ));
        }
        let now = Utc::now().to_rfc3339();
        let connection = self.database.connection()?;
        let existing = connection
            .query_row(
                "SELECT input_hash, upstream_task_id, status, reconcile_required
                   FROM ai_remote_job_records WHERE request_id = ?1 AND task_type = ?2",
                params![key.request_id, key.task_type],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                    ))
                },
            )
            .optional()?;
        if let Some((input_hash, stored_task_id, _status, reconcile_required)) = existing {
            if input_hash != key.input_hash {
                return Err(AiRepositoryError::IdempotencyConflict);
            }
            // 同键同输入：只补齐上游任务标识（已有值不被覆盖），状态保持可复用。
            let effective_task_id = upstream_task_id
                .map(str::to_owned)
                .or_else(|| stored_task_id.clone());
            connection.execute(
                "UPDATE ai_remote_job_records
                    SET upstream_task_id = COALESCE(upstream_task_id, ?1),
                        status = 'reused',
                        updated_at = ?2
                  WHERE request_id = ?3 AND task_type = ?4",
                params![upstream_task_id, now, key.request_id, key.task_type],
            )?;
            return Ok(RemoteJobRecord {
                request_id: key.request_id.clone(),
                task_type: key.task_type.clone(),
                input_hash,
                upstream_task_id: effective_task_id,
                status: "reused".to_owned(),
                reconcile_required: reconcile_required != 0,
            });
        }
        let status = if upstream_task_id.is_some() {
            "submitted"
        } else {
            // 已提交但未取到上游任务标识：必须进入待对账，不得自动重新提交。
            "awaiting_reconcile"
        };
        let reconcile_required = i64::from(upstream_task_id.is_none());
        connection.execute(
            "INSERT INTO ai_remote_job_records(
                 request_id, task_type, input_hash, upstream_task_id,
                 status, reconcile_required, created_at, updated_at
             ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
            params![
                key.request_id,
                key.task_type,
                key.input_hash,
                upstream_task_id,
                status,
                reconcile_required,
                now
            ],
        )?;
        Ok(RemoteJobRecord {
            request_id: key.request_id.clone(),
            task_type: key.task_type.clone(),
            input_hash: key.input_hash.clone(),
            upstream_task_id: upstream_task_id.map(str::to_owned),
            status: status.to_owned(),
            reconcile_required: reconcile_required != 0,
        })
    }

    /// 按幂等键读取已落盘记录，供重启恢复使用。
    pub fn remote_job_record(
        &self,
        request_id: &str,
        task_type: &str,
    ) -> Result<Option<RemoteJobRecord>> {
        let connection = self.database.connection()?;
        let record = connection
            .query_row(
                "SELECT request_id, task_type, input_hash, upstream_task_id, status, reconcile_required
                   FROM ai_remote_job_records WHERE request_id = ?1 AND task_type = ?2",
                params![request_id, task_type],
                |row| {
                    Ok(RemoteJobRecord {
                        request_id: row.get(0)?,
                        task_type: row.get(1)?,
                        input_hash: row.get(2)?,
                        upstream_task_id: row.get(3)?,
                        status: row.get(4)?,
                        reconcile_required: row.get::<_, i64>(5)? != 0,
                    })
                },
            )
            .optional()?;
        Ok(record)
    }

    /// 需要向用户确认上游是否已受理的记录。
    pub fn records_awaiting_reconcile(&self) -> Result<Vec<RemoteJobRecord>> {
        let connection = self.database.connection()?;
        let mut statement = connection.prepare(
            "SELECT request_id, task_type, input_hash, upstream_task_id, status, reconcile_required
               FROM ai_remote_job_records
              WHERE reconcile_required = 1 OR status = 'awaiting_reconcile'
              ORDER BY id ASC",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(RemoteJobRecord {
                request_id: row.get(0)?,
                task_type: row.get(1)?,
                input_hash: row.get(2)?,
                upstream_task_id: row.get(3)?,
                status: row.get(4)?,
                reconcile_required: row.get::<_, i64>(5)? != 0,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    pub fn create_artifact(&self, input: NewAsrArtifact) -> Result<AsrArtifact> {
        let source_hash = input.source_fingerprint.fingerprint()?;
        let source_json = serde_json::to_string(&input.source_fingerprint)
            .map_err(|_| AiRepositoryError::Serialization("源指纹无法序列化".to_owned()))?;
        let now = Utc::now().to_rfc3339();
        let connection = self.database.connection()?;
        connection.execute(
            r#"
            INSERT INTO asr_artifacts(
                source_video_id, source_fingerprint_json, source_fingerprint_hash,
                recognition_profile_hash, engine_id, engine_version, model_id,
                model_version, provider, fallback_reason, status, created_at, last_used_at
            ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 'pending', ?11, ?11)
            "#,
            params![
                input.source_fingerprint.video_id,
                source_json,
                source_hash,
                input.recognition_profile_hash,
                input.engine_id,
                input.engine_version,
                input.model_id,
                input.model_version,
                input.provider,
                input.fallback_reason,
                now,
            ],
        )?;
        let id = connection.last_insert_rowid();
        drop(connection);
        self.get_artifact(id)
    }

    pub fn publish_artifact(
        &self,
        artifact_id: i64,
        duration_ms: u64,
        language: Option<&str>,
        drafts: &[TranscriptSegmentDraft],
    ) -> Result<Vec<TranscriptSegment>> {
        if duration_ms == 0 || drafts.is_empty() {
            return Err(AiRepositoryError::Integrity(
                "完整识别产物必须包含有效时长和句段".to_owned(),
            ));
        }
        let mut previous_end = 0;
        for draft in drafts {
            if draft.source_start_ms >= draft.source_end_ms
                || draft.source_end_ms > duration_ms
                || draft.source_start_ms < previous_end
                || draft.raw_text.trim().is_empty()
                || draft.normalized_text.trim().is_empty()
                || draft
                    .confidence
                    .is_some_and(|confidence| !(0.0..=1.0).contains(&confidence))
            {
                return Err(AiRepositoryError::Integrity(
                    "转写句段时间、文本或置信信息无效".to_owned(),
                ));
            }
            previous_end = draft.source_end_ms;
        }

        let mut connection = self.database.connection()?;
        let transaction = connection.transaction()?;
        let status: Option<String> = transaction
            .query_row(
                "SELECT status FROM asr_artifacts WHERE id = ?1",
                [artifact_id],
                |row| row.get(0),
            )
            .optional()?;
        if status.as_deref() != Some("pending") {
            return Err(AiRepositoryError::InvalidState(
                "只有未发布产物可以事务发布".to_owned(),
            ));
        }
        let mut segments = Vec::with_capacity(drafts.len());
        for (ordinal, draft) in drafts.iter().enumerate() {
            let id = stable_segment_id(artifact_id, ordinal, draft);
            transaction.execute(
                r#"
                INSERT INTO transcript_segments(
                    id, artifact_id, ordinal, source_start_ms, source_end_ms,
                    raw_text, normalized_text, confidence
                ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                "#,
                params![
                    id,
                    artifact_id,
                    ordinal as i64,
                    draft.source_start_ms as i64,
                    draft.source_end_ms as i64,
                    draft.raw_text,
                    draft.normalized_text,
                    draft.confidence,
                ],
            )?;
            segments.push(TranscriptSegment {
                id,
                artifact_id,
                ordinal: ordinal as i64,
                source_start_ms: draft.source_start_ms,
                source_end_ms: draft.source_end_ms,
                raw_text: draft.raw_text.clone(),
                normalized_text: draft.normalized_text.clone(),
                confidence: draft.confidence,
            });
        }
        let now = Utc::now().to_rfc3339();
        transaction.execute(
            r#"
            UPDATE asr_artifacts
            SET status = 'published', duration_ms = ?1, language = ?2,
                published_at = ?3, last_used_at = ?3
            WHERE id = ?4
            "#,
            params![duration_ms as i64, language, now, artifact_id],
        )?;
        transaction.commit()?;
        Ok(segments)
    }

    pub fn find_published_artifact(
        &self,
        source: &SourceFingerprint,
        recognition_profile_hash: &str,
    ) -> Result<Option<AsrArtifact>> {
        let source_hash = source.fingerprint()?;
        let connection = self.database.connection()?;
        let row = connection
            .query_row(
                &artifact_select(
                    "WHERE source_fingerprint_hash = ?1 AND recognition_profile_hash = ?2 AND status = 'published' ORDER BY id DESC LIMIT 1",
                ),
                params![source_hash, recognition_profile_hash],
                map_artifact,
            )
            .optional()?;
        let artifact = row.map(parse_artifact).transpose()?;
        if let Some(artifact) = &artifact {
            connection.execute(
                "UPDATE asr_artifacts SET last_used_at = ?1 WHERE id = ?2",
                params![Utc::now().to_rfc3339(), artifact.id],
            )?;
        }
        Ok(artifact)
    }

    pub fn attach_artifact(&self, input_id: i64, artifact_id: i64) -> Result<()> {
        let connection = self.database.connection()?;
        let published: bool = connection
            .query_row(
                "SELECT status = 'published' FROM asr_artifacts WHERE id = ?1",
                [artifact_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or(AiRepositoryError::NotFound("识别产物"))?;
        if !published {
            return Err(AiRepositoryError::InvalidState(
                "未完整发布的识别产物不能关联项目".to_owned(),
            ));
        }
        let changed = connection.execute(
            "UPDATE ai_project_inputs SET artifact_id = ?1, updated_at = ?2 WHERE id = ?3",
            params![artifact_id, Utc::now().to_rfc3339(), input_id],
        )?;
        if changed == 0 {
            return Err(AiRepositoryError::NotFound("项目输入"));
        }
        Ok(())
    }

    pub fn artifact_for_input(&self, input_id: i64) -> Result<Option<AsrArtifact>> {
        let connection = self.database.connection()?;
        let row = connection
            .query_row(
                &artifact_select(
                    "WHERE id = (SELECT artifact_id FROM ai_project_inputs WHERE id = ?1)",
                ),
                [input_id],
                map_artifact,
            )
            .optional()?;
        row.map(parse_artifact).transpose()
    }

    pub fn list_segments_for_input(&self, input_id: i64) -> Result<Vec<TranscriptSegment>> {
        let connection = self.database.connection()?;
        let mut statement = connection.prepare(
            r#"
            SELECT ts.id, ts.artifact_id, ts.ordinal, ts.source_start_ms,
                   ts.source_end_ms, ts.raw_text, ts.normalized_text, ts.confidence
            FROM ai_project_inputs input
            JOIN transcript_segments ts ON ts.artifact_id = input.artifact_id
            WHERE input.id = ?1
            ORDER BY ts.ordinal
            "#,
        )?;
        let rows = statement.query_map([input_id], map_segment)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
            .into_iter()
            .map(parse_segment)
            .collect()
    }

    pub fn recover_interrupted(&self) -> Result<RecoverySummary> {
        let mut connection = self.database.connection()?;
        let transaction = connection.transaction()?;
        let now = Utc::now().to_rfc3339();
        let inputs = transaction.execute(
            r#"
            UPDATE ai_project_inputs
            SET status = 'pending', scheduler_generation = scheduler_generation + 1,
                queue_priority = 0, queue_sequence = NULL, last_error_code = 'interrupted',
                last_error_message = '应用异常退出，等待用户重试', updated_at = ?1
            WHERE status IN ('validating', 'preparing_audio', 'detecting_speech', 'transcribing')
            "#,
            [&now],
        )?;
        let projects = transaction.execute(
            "UPDATE ai_projects SET status = 'queued', updated_at = ?1 WHERE status = 'running'",
            [&now],
        )?;
        let artifacts = transaction.execute(
            "UPDATE asr_artifacts SET status = 'invalidated' WHERE status = 'pending'",
            [],
        )?;
        // 重启时必须先核对已提交的付费任务，而不是直接重新提交。这里只统计需要
        // 对账的数量；具体核对由 `pending_remote_jobs` 的消费者向上游查询后消解。
        let awaiting: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM ai_remote_job_records
              WHERE reconcile_required = 1 OR status = 'awaiting_reconcile'",
            [],
            |row| row.get(0),
        )?;
        transaction.commit()?;
        Ok(RecoverySummary {
            projects,
            inputs,
            artifacts,
            awaiting_remote_reconcile: usize::try_from(awaiting).unwrap_or(usize::MAX),
        })
    }

    /// 重启后需要向上游核对的付费任务。
    ///
    /// 调用方 MUST 先按这些记录的 `request_id` 与 `upstream_task_id` 查询上游终态：
    /// 已受理则继续等待或复用，未受理才可释放预留并允许重试。MUST NOT 直接重新提交。
    pub fn pending_remote_jobs(&self) -> Result<Vec<RemoteJobRecord>> {
        self.records_awaiting_reconcile()
    }

    pub fn invalidate_artifacts_for_video(&self, video_id: i64) -> Result<usize> {
        let mut connection = self.database.connection()?;
        let transaction = connection.transaction()?;
        transaction.execute(
            r#"
            UPDATE ai_project_inputs
            SET artifact_id = NULL, status = 'failed', last_error_code = 'source_deleted',
                last_error_message = '原始视频已删除', updated_at = ?1
            WHERE video_id = ?2
            "#,
            params![Utc::now().to_rfc3339(), video_id],
        )?;
        let changed = transaction.execute(
            "UPDATE asr_artifacts SET status = 'invalidated' WHERE source_video_id = ?1",
            [video_id],
        )?;
        transaction.commit()?;
        Ok(changed)
    }

    pub fn cleanup_unreferenced_artifacts(&self, older_than: &str) -> Result<usize> {
        let changed = self.database.connection()?.execute(
            r#"
            DELETE FROM asr_artifacts
            WHERE last_used_at < ?1
              AND NOT EXISTS(
                  SELECT 1 FROM ai_project_inputs input WHERE input.artifact_id = asr_artifacts.id
              )
            "#,
            [older_than],
        )?;
        Ok(changed)
    }

    fn ensure_draft(&self, project_id: i64) -> Result<()> {
        let project = self.get_project_row(project_id)?;
        if project.status != AiProjectStatus::Draft || project.input_frozen {
            return Err(AiRepositoryError::ProjectNotDraft);
        }
        Ok(())
    }

    fn get_project_row(&self, id: i64) -> Result<AiProject> {
        let connection = self.database.connection()?;
        let row = connection
            .query_row(&project_select("WHERE id = ?1"), [id], map_project)
            .optional()?
            .ok_or(AiRepositoryError::NotFound("AI 项目"))?;
        parse_project(row)
    }

    fn list_inputs(&self, project_id: i64) -> Result<Vec<AiProjectInput>> {
        let connection = self.database.connection()?;
        let mut statement =
            connection.prepare(&input_select("WHERE project_id = ?1 ORDER BY position, id"))?;
        let rows = statement.query_map([project_id], map_input)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
            .into_iter()
            .map(parse_input)
            .collect()
    }

    pub fn get_input(&self, id: i64) -> Result<AiProjectInput> {
        let connection = self.database.connection()?;
        let row = connection
            .query_row(&input_select("WHERE id = ?1"), [id], map_input)
            .optional()?
            .ok_or(AiRepositoryError::NotFound("项目输入"))?;
        parse_input(row)
    }

    fn get_artifact(&self, id: i64) -> Result<AsrArtifact> {
        let connection = self.database.connection()?;
        let row = connection
            .query_row(&artifact_select("WHERE id = ?1"), [id], map_artifact)
            .optional()?
            .ok_or(AiRepositoryError::NotFound("识别产物"))?;
        parse_artifact(row)
    }
}

pub(crate) fn migrate_ai_v4(connection: &mut Connection) -> crate::database::Result<()> {
    let applied = connection
        .query_row(
            "SELECT 1 FROM schema_migrations WHERE version = 4",
            [],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    let ai_table_count: i64 = connection.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name IN ('ai_projects', 'ai_project_inputs', 'asr_artifacts', 'transcript_segments')",
        [],
        |row| row.get(0),
    )?;
    if applied && ai_table_count == 4 {
        return Ok(());
    }
    // v4 was briefly used by the diagnostics migration in a development build.
    // Keep that marker and record this repair separately instead of rewriting history.
    let migration_version = if applied { 6 } else { 4 };
    let transaction = connection.transaction()?;
    transaction.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS ai_projects (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL CHECK(length(trim(name)) > 0),
            status TEXT NOT NULL CHECK(status IN (
                'draft', 'queued', 'running', 'completed',
                'completed_with_errors', 'cancelled', 'failed'
            )),
            recognition_profile_json TEXT NOT NULL,
            recognition_profile_hash TEXT NOT NULL,
            input_frozen INTEGER NOT NULL DEFAULT 0,
            progress_percent INTEGER NOT NULL DEFAULT 0 CHECK(progress_percent BETWEEN 0 AND 100),
            last_error_code TEXT,
            last_error_message TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            started_at TEXT,
            completed_at TEXT
        );

        CREATE TABLE IF NOT EXISTS asr_artifacts (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            source_video_id INTEGER REFERENCES videos(id) ON DELETE SET NULL,
            source_fingerprint_json TEXT NOT NULL,
            source_fingerprint_hash TEXT NOT NULL,
            recognition_profile_hash TEXT NOT NULL,
            engine_id TEXT NOT NULL,
            engine_version TEXT NOT NULL,
            model_id TEXT NOT NULL,
            model_version TEXT NOT NULL,
            status TEXT NOT NULL CHECK(status IN ('pending', 'published', 'invalidated')),
            duration_ms INTEGER,
            language TEXT,
            created_at TEXT NOT NULL,
            published_at TEXT,
            last_used_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS ai_project_inputs (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            project_id INTEGER NOT NULL REFERENCES ai_projects(id) ON DELETE CASCADE,
            position INTEGER NOT NULL CHECK(position >= 0),
            source_kind TEXT NOT NULL CHECK(source_kind IN ('local_file', 'video_library')),
            video_id INTEGER REFERENCES videos(id) ON DELETE SET NULL,
            display_name TEXT NOT NULL,
            source_path TEXT NOT NULL,
            source_fingerprint_json TEXT NOT NULL,
            source_fingerprint_hash TEXT NOT NULL,
            duration_ms INTEGER,
            audio_present INTEGER,
            project_offset_ms INTEGER,
            status TEXT NOT NULL CHECK(status IN (
                'pending', 'validating', 'preparing_audio', 'detecting_speech',
                'transcribing', 'completed', 'skipped', 'cancelled', 'failed'
            )),
            artifact_id INTEGER REFERENCES asr_artifacts(id) ON DELETE SET NULL,
            last_error_code TEXT,
            last_error_message TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            UNIQUE(project_id, position),
            UNIQUE(project_id, source_fingerprint_hash)
        );

        CREATE TABLE IF NOT EXISTS transcript_segments (
            id TEXT PRIMARY KEY,
            artifact_id INTEGER NOT NULL REFERENCES asr_artifacts(id) ON DELETE CASCADE,
            ordinal INTEGER NOT NULL CHECK(ordinal >= 0),
            source_start_ms INTEGER NOT NULL CHECK(source_start_ms >= 0),
            source_end_ms INTEGER NOT NULL CHECK(source_end_ms > source_start_ms),
            raw_text TEXT NOT NULL,
            normalized_text TEXT NOT NULL,
            confidence REAL CHECK(confidence IS NULL OR (confidence >= 0 AND confidence <= 1)),
            UNIQUE(artifact_id, ordinal)
        );

        CREATE INDEX IF NOT EXISTS idx_ai_projects_status_updated
            ON ai_projects(status, updated_at DESC);
        CREATE INDEX IF NOT EXISTS idx_ai_inputs_project_status
            ON ai_project_inputs(project_id, status, position);
        CREATE INDEX IF NOT EXISTS idx_asr_artifacts_source
            ON asr_artifacts(source_fingerprint_hash, recognition_profile_hash, status);
        CREATE UNIQUE INDEX IF NOT EXISTS idx_asr_artifacts_published_cache
            ON asr_artifacts(source_fingerprint_hash, recognition_profile_hash)
            WHERE status = 'published';
        CREATE INDEX IF NOT EXISTS idx_transcript_segments_artifact_time
            ON transcript_segments(artifact_id, source_start_ms, ordinal);
        "#,
    )?;
    transaction.execute(
        "INSERT OR IGNORE INTO schema_migrations(version, applied_at) VALUES(?1, ?2)",
        params![migration_version, Utc::now().to_rfc3339()],
    )?;
    transaction.commit()?;
    Ok(())
}

/// v7 为可恢复调度和精彩分析增加持久化边界。
///
/// 该迁移不保存凭据。旧版本的项目表使用 CHECK 约束，必须在事务中重建
/// 表才能安全加入 deleting 状态；其它新增列均带有兼容旧数据的默认值。
pub(crate) fn migrate_ai_v7(connection: &mut Connection) -> crate::database::Result<()> {
    let applied = connection
        .query_row(
            "SELECT 1 FROM schema_migrations WHERE version = 7",
            [],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if applied {
        return Ok(());
    }
    // SQLite 的 ALTER TABLE 会自动改写子表外键；重建期间暂时关闭外键，
    // 最终由 Database::migrate 的完整性检查确认引用仍然有效。
    connection.pragma_update(None, "foreign_keys", "OFF")?;
    let migration = (|| -> crate::database::Result<()> {
        let transaction = connection.transaction()?;
        let project_has_deleting = transaction
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'ai_projects'",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .is_some_and(|sql| sql.contains("'deleting'"));
        if !project_has_deleting {
            transaction.execute_batch(
            r#"
            CREATE TABLE ai_projects_v7 (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL CHECK(length(trim(name)) > 0),
                status TEXT NOT NULL CHECK(status IN (
                    'draft', 'queued', 'running', 'deleting', 'completed',
                    'completed_with_errors', 'cancelled', 'failed'
                )),
                recognition_profile_json TEXT NOT NULL,
                recognition_profile_hash TEXT NOT NULL,
                input_frozen INTEGER NOT NULL DEFAULT 0,
                progress_percent INTEGER NOT NULL DEFAULT 0 CHECK(progress_percent BETWEEN 0 AND 100),
                last_error_code TEXT,
                last_error_message TEXT,
                project_tags_json TEXT NOT NULL DEFAULT '[]',
                analysis_goal TEXT,
                deleting_at TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                started_at TEXT,
                completed_at TEXT
            );
            INSERT INTO ai_projects_v7(
                id, name, status, recognition_profile_json, recognition_profile_hash,
                input_frozen, progress_percent, last_error_code, last_error_message,
                created_at, updated_at, started_at, completed_at
            ) SELECT id, name, status, recognition_profile_json, recognition_profile_hash,
                input_frozen, progress_percent, last_error_code, last_error_message,
                created_at, updated_at, started_at, completed_at
                FROM ai_projects;
            DROP TABLE ai_projects;
            ALTER TABLE ai_projects_v7 RENAME TO ai_projects;
            "#,
        )?;
        } else {
            let has_project_tags = transaction
            .query_row(
                "SELECT 1 FROM pragma_table_info('ai_projects') WHERE name = 'project_tags_json'",
                [],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
            if !has_project_tags {
                transaction.execute(
                "ALTER TABLE ai_projects ADD COLUMN project_tags_json TEXT NOT NULL DEFAULT '[]'",
                [],
            )?;
                transaction.execute("ALTER TABLE ai_projects ADD COLUMN analysis_goal TEXT", [])?;
                transaction.execute("ALTER TABLE ai_projects ADD COLUMN deleting_at TEXT", [])?;
            }
        }

        for (column, definition) in [
            (
                "scheduler_generation",
                "INTEGER NOT NULL DEFAULT 1 CHECK(scheduler_generation > 0)",
            ),
            (
                "queue_priority",
                "INTEGER NOT NULL DEFAULT 0 CHECK(queue_priority >= 0)",
            ),
            ("queue_sequence", "INTEGER"),
        ] {
            let exists = transaction
                .query_row(
                    "SELECT 1 FROM pragma_table_info('ai_project_inputs') WHERE name = ?1",
                    [column],
                    |_| Ok(()),
                )
                .optional()?
                .is_some();
            if !exists {
                transaction.execute(
                    &format!("ALTER TABLE ai_project_inputs ADD COLUMN {column} {definition}"),
                    [],
                )?;
            }
        }

        transaction.execute_batch(
            r#"
        CREATE INDEX IF NOT EXISTS idx_ai_inputs_queue
            ON ai_project_inputs(status, queue_priority DESC, queue_sequence ASC, id ASC);
        CREATE INDEX IF NOT EXISTS idx_ai_inputs_project_generation
            ON ai_project_inputs(project_id, scheduler_generation, status);

        CREATE TABLE IF NOT EXISTS llm_provider_settings (
            id INTEGER PRIMARY KEY CHECK(id = 1),
            provider TEXT NOT NULL CHECK(provider = 'deepseek'),
            model_id TEXT NOT NULL CHECK(length(trim(model_id)) > 0),
            timeout_ms INTEGER NOT NULL DEFAULT 30000 CHECK(timeout_ms BETWEEN 1000 AND 120000),
            prompt_version TEXT NOT NULL DEFAULT 'highlight-v1',
            updated_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS ai_highlight_runs (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            project_id INTEGER NOT NULL REFERENCES ai_projects(id) ON DELETE CASCADE,
            status TEXT NOT NULL CHECK(status IN (
                'pending', 'running', 'candidates', 'ranking', 'completed',
                'partial', 'cancelled', 'failed'
            )),
            model_id TEXT NOT NULL,
            prompt_version TEXT NOT NULL,
            tags_snapshot_json TEXT NOT NULL,
            skills_snapshot_json TEXT NOT NULL,
            analysis_goal TEXT,
            analysis_fingerprint TEXT NOT NULL,
            user_authorized INTEGER NOT NULL DEFAULT 0,
            total_segments INTEGER NOT NULL DEFAULT 0,
            total_chars INTEGER NOT NULL DEFAULT 0,
            estimated_batches INTEGER NOT NULL DEFAULT 0,
            total_tokens INTEGER NOT NULL DEFAULT 0,
            last_error_code TEXT,
            last_error_message TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            UNIQUE(project_id, analysis_fingerprint)
        );

        CREATE TABLE IF NOT EXISTS ai_highlight_chunks (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            run_id INTEGER NOT NULL REFERENCES ai_highlight_runs(id) ON DELETE CASCADE,
            ordinal INTEGER NOT NULL CHECK(ordinal >= 0),
            input_id INTEGER NOT NULL REFERENCES ai_project_inputs(id) ON DELETE CASCADE,
            segment_ids_json TEXT NOT NULL,
            context_segment_ids_json TEXT NOT NULL DEFAULT '[]',
            status TEXT NOT NULL CHECK(status IN ('pending', 'running', 'completed', 'failed')),
            candidate_count INTEGER NOT NULL DEFAULT 0,
            token_usage INTEGER NOT NULL DEFAULT 0,
            last_error_code TEXT,
            last_error_message TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            UNIQUE(run_id, ordinal)
        );

        CREATE TABLE IF NOT EXISTS ai_highlight_candidates (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            run_id INTEGER NOT NULL REFERENCES ai_highlight_runs(id) ON DELETE CASCADE,
            chunk_id INTEGER REFERENCES ai_highlight_chunks(id) ON DELETE SET NULL,
            candidate_key TEXT NOT NULL,
            title TEXT NOT NULL,
            input_id INTEGER NOT NULL REFERENCES ai_project_inputs(id) ON DELETE CASCADE,
            segment_ids_json TEXT NOT NULL,
            start_ms INTEGER NOT NULL CHECK(start_ms >= 0),
            end_ms INTEGER NOT NULL CHECK(end_ms > start_ms),
            total_score REAL NOT NULL CHECK(total_score BETWEEN 0 AND 100),
            hook_score REAL NOT NULL CHECK(hook_score BETWEEN 0 AND 100),
            information_score REAL NOT NULL CHECK(information_score BETWEEN 0 AND 100),
            emotion_score REAL NOT NULL CHECK(emotion_score BETWEEN 0 AND 100),
            tag_relevance_score REAL NOT NULL CHECK(tag_relevance_score BETWEEN 0 AND 100),
            completeness_score REAL NOT NULL CHECK(completeness_score BETWEEN 0 AND 100),
            shareability_score REAL NOT NULL CHECK(shareability_score BETWEEN 0 AND 100),
            reason TEXT NOT NULL,
            matched_tags_json TEXT NOT NULL DEFAULT '[]',
            rank INTEGER,
            selected INTEGER NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL,
            UNIQUE(run_id, candidate_key)
        );
        CREATE INDEX IF NOT EXISTS idx_ai_highlight_candidates_rank
            ON ai_highlight_candidates(run_id, selected DESC, total_score DESC, id ASC);
        CREATE INDEX IF NOT EXISTS idx_ai_highlight_chunks_status
            ON ai_highlight_chunks(run_id, status, ordinal);
        "#,
        )?;
        transaction.execute(
        "CREATE INDEX IF NOT EXISTS idx_ai_projects_status_updated ON ai_projects(status, updated_at DESC)",
        [],
    )?;
        transaction.execute(
            "INSERT INTO schema_migrations(version, applied_at) VALUES(7, ?1)",
            [Utc::now().to_rfc3339()],
        )?;
        transaction.commit()?;
        Ok(())
    })();
    connection.pragma_update(None, "foreign_keys", "ON")?;
    migration
}

/// v9 保存逐批候选草稿，使长时间精彩分析可以展示真实进度并在中断后继续。
pub(crate) fn migrate_ai_v9(connection: &mut Connection) -> crate::database::Result<()> {
    let applied = connection
        .query_row(
            "SELECT 1 FROM schema_migrations WHERE version = 9",
            [],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if applied {
        return Ok(());
    }
    let transaction = connection.transaction()?;
    let has_drafts = transaction
        .query_row(
            "SELECT 1 FROM pragma_table_info('ai_highlight_chunks') WHERE name = 'candidate_drafts_json'",
            [],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if !has_drafts {
        transaction.execute(
            "ALTER TABLE ai_highlight_chunks ADD COLUMN candidate_drafts_json TEXT NOT NULL DEFAULT '[]'",
            [],
        )?;
    }
    let has_attempt_count = transaction
        .query_row(
            "SELECT 1 FROM pragma_table_info('ai_highlight_chunks') WHERE name = 'attempt_count'",
            [],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if !has_attempt_count {
        transaction.execute(
            "ALTER TABLE ai_highlight_chunks ADD COLUMN attempt_count INTEGER NOT NULL DEFAULT 0",
            [],
        )?;
    }
    transaction.execute(
        "INSERT INTO schema_migrations(version, applied_at) VALUES(9, ?1)",
        [Utc::now().to_rfc3339()],
    )?;
    transaction.commit()?;
    Ok(())
}

/// v12 冻结精彩筛选阈值，保证历史运行的推荐和自动选择语义稳定。
pub(crate) fn migrate_ai_v12(connection: &mut Connection) -> crate::database::Result<()> {
    let applied = connection
        .query_row(
            "SELECT 1 FROM schema_migrations WHERE version = 12",
            [],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if applied {
        return Ok(());
    }
    let transaction = connection.transaction()?;
    for (table, column, definition) in [
        (
            "llm_provider_settings",
            "qualified_score",
            "INTEGER NOT NULL DEFAULT 70 CHECK(qualified_score BETWEEN 0 AND 100)",
        ),
        (
            "llm_provider_settings",
            "excellent_score",
            "INTEGER NOT NULL DEFAULT 80 CHECK(excellent_score BETWEEN 0 AND 100)",
        ),
        (
            "ai_highlight_runs",
            "qualified_score",
            "INTEGER NOT NULL DEFAULT 70 CHECK(qualified_score BETWEEN 0 AND 100)",
        ),
        (
            "ai_highlight_runs",
            "excellent_score",
            "INTEGER NOT NULL DEFAULT 80 CHECK(excellent_score BETWEEN 0 AND 100)",
        ),
    ] {
        let exists = transaction
            .query_row(
                "SELECT 1 FROM pragma_table_info(?1) WHERE name = ?2",
                params![table, column],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if !exists {
            transaction.execute(
                &format!("ALTER TABLE {table} ADD COLUMN {column} {definition}"),
                [],
            )?;
        }
    }
    transaction.execute_batch(
        r#"
        CREATE TRIGGER IF NOT EXISTS validate_llm_highlight_threshold_insert
        BEFORE INSERT ON llm_provider_settings
        WHEN NEW.excellent_score < NEW.qualified_score
        BEGIN
            SELECT RAISE(ABORT, '优秀片段阈值不能低于合格片段阈值');
        END;
        CREATE TRIGGER IF NOT EXISTS validate_llm_highlight_threshold_update
        BEFORE UPDATE OF qualified_score, excellent_score ON llm_provider_settings
        WHEN NEW.excellent_score < NEW.qualified_score
        BEGIN
            SELECT RAISE(ABORT, '优秀片段阈值不能低于合格片段阈值');
        END;
        "#,
    )?;
    transaction.execute(
        "INSERT INTO schema_migrations(version, applied_at) VALUES(12, ?1)",
        [Utc::now().to_rfc3339()],
    )?;
    transaction.commit()?;
    Ok(())
}

/// v13 持久化本地剪辑工程和受控导出状态。来源路径不会写入工程表，导出时仅通过
/// 已冻结的 AI 输入重新取得并校验，避免工程 JSON 成为任意文件读取入口。
pub(crate) fn migrate_ai_v13(connection: &mut Connection) -> crate::database::Result<()> {
    let applied = connection
        .query_row(
            "SELECT 1 FROM schema_migrations WHERE version = 13",
            [],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if applied {
        return Ok(());
    }
    let transaction = connection.transaction()?;
    transaction.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS ai_clip_projects (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            highlight_run_id INTEGER NOT NULL UNIQUE REFERENCES ai_highlight_runs(id) ON DELETE CASCADE,
            name TEXT NOT NULL,
            export_status TEXT NOT NULL DEFAULT 'idle' CHECK(export_status IN ('idle', 'exporting', 'completed', 'cancelled', 'failed')),
            export_progress INTEGER NOT NULL DEFAULT 0 CHECK(export_progress BETWEEN 0 AND 100),
            output_path TEXT,
            last_error_code TEXT,
            last_error_message TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS ai_clip_segments (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            clip_project_id INTEGER NOT NULL REFERENCES ai_clip_projects(id) ON DELETE CASCADE,
            candidate_id INTEGER NOT NULL REFERENCES ai_highlight_candidates(id) ON DELETE CASCADE,
            input_id INTEGER NOT NULL REFERENCES ai_project_inputs(id) ON DELETE CASCADE,
            position INTEGER NOT NULL CHECK(position >= 0),
            title TEXT NOT NULL,
            source_start_ms INTEGER NOT NULL CHECK(source_start_ms >= 0),
            source_end_ms INTEGER NOT NULL CHECK(source_end_ms > source_start_ms),
            volume_percent INTEGER NOT NULL DEFAULT 100 CHECK(volume_percent BETWEEN 0 AND 200),
            effect TEXT NOT NULL DEFAULT 'none' CHECK(effect IN ('none', 'fade_in', 'fade_out', 'fade_in_out', 'flash', 'black')),
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            UNIQUE(clip_project_id, candidate_id)
        );
        CREATE INDEX IF NOT EXISTS idx_ai_clip_segments_project_position
            ON ai_clip_segments(clip_project_id, position, id);
        "#,
    )?;
    transaction.execute(
        "INSERT INTO schema_migrations(version, applied_at) VALUES(13, ?1)",
        [Utc::now().to_rfc3339()],
    )?;
    transaction.commit()?;
    Ok(())
}

/// v14 冻结剪辑工程的输出画幅，并为可恢复工程保存单调递增的编辑版本。
pub(crate) fn migrate_ai_v14(connection: &mut Connection) -> crate::database::Result<()> {
    let applied = connection
        .query_row(
            "SELECT 1 FROM schema_migrations WHERE version = 14",
            [],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if applied {
        return Ok(());
    }
    let transaction = connection.transaction()?;
    for (column, definition) in [
        (
            "output_width",
            "INTEGER CHECK(output_width IS NULL OR output_width > 0)",
        ),
        (
            "output_height",
            "INTEGER CHECK(output_height IS NULL OR output_height > 0)",
        ),
        ("version", "INTEGER NOT NULL DEFAULT 1 CHECK(version > 0)"),
    ] {
        let exists = transaction
            .query_row(
                "SELECT 1 FROM pragma_table_info('ai_clip_projects') WHERE name = ?1",
                [column],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if !exists {
            transaction.execute(
                &format!("ALTER TABLE ai_clip_projects ADD COLUMN {column} {definition}"),
                [],
            )?;
        }
    }
    transaction.execute(
        "INSERT INTO schema_migrations(version, applied_at) VALUES(14, ?1)",
        [Utc::now().to_rfc3339()],
    )?;
    transaction.commit()?;
    Ok(())
}

/// v16 为剪辑工程保存独立字幕副本。迁移只新增表，不回写或删除既有 ASR、
/// 剪辑工程和成品路径；旧工程在首次打开时由 repository 幂等回填。
pub(crate) fn migrate_ai_v16(connection: &mut Connection) -> crate::database::Result<()> {
    let applied = connection
        .query_row(
            "SELECT 1 FROM schema_migrations WHERE version = 16",
            [],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if applied {
        return Ok(());
    }
    let transaction = connection.transaction()?;
    transaction.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS ai_clip_subtitles (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            clip_project_id INTEGER NOT NULL REFERENCES ai_clip_projects(id) ON DELETE CASCADE,
            clip_segment_id INTEGER NOT NULL REFERENCES ai_clip_segments(id) ON DELETE CASCADE,
            input_id INTEGER NOT NULL REFERENCES ai_project_inputs(id) ON DELETE CASCADE,
            stable_segment_id TEXT NOT NULL,
            source_start_ms INTEGER NOT NULL CHECK(source_start_ms >= 0),
            source_end_ms INTEGER NOT NULL CHECK(source_end_ms > source_start_ms),
            original_text TEXT NOT NULL CHECK(length(trim(original_text)) > 0),
            text TEXT NOT NULL CHECK(length(trim(text)) > 0),
            hidden INTEGER NOT NULL DEFAULT 0 CHECK(hidden IN (0, 1)),
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            UNIQUE(clip_segment_id, stable_segment_id)
        );
        CREATE INDEX IF NOT EXISTS idx_ai_clip_subtitles_project_segment_time
            ON ai_clip_subtitles(clip_project_id, clip_segment_id, source_start_ms, id);
        "#,
    )?;
    transaction.execute(
        "INSERT INTO schema_migrations(version, applied_at) VALUES(16, ?1)",
        [Utc::now().to_rfc3339()],
    )?;
    transaction.commit()?;
    Ok(())
}

/// v21 增加剪辑字幕纠错审计、两阶段转场评分和独立自动应用阈值。
pub(crate) fn migrate_ai_v21(connection: &mut Connection) -> crate::database::Result<()> {
    let applied = connection
        .query_row(
            "SELECT 1 FROM schema_migrations WHERE version = 21",
            [],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if applied {
        return Ok(());
    }
    let transaction = connection.transaction()?;
    for (table, column, definition) in [
        (
            "llm_provider_settings",
            "transition_auto_apply_score",
            "INTEGER NOT NULL DEFAULT 8 CHECK(transition_auto_apply_score BETWEEN 0 AND 10)",
        ),
        (
            "ai_clip_transition_boundaries",
            "score",
            "REAL CHECK(score IS NULL OR score BETWEEN 0 AND 10)",
        ),
        (
            "ai_clip_transition_boundaries",
            "scene_score",
            "REAL CHECK(scene_score IS NULL OR scene_score BETWEEN 0 AND 10)",
        ),
        (
            "ai_clip_transition_boundaries",
            "continuity_score",
            "REAL CHECK(continuity_score IS NULL OR continuity_score BETWEEN 0 AND 10)",
        ),
        (
            "ai_clip_transition_boundaries",
            "rhythm_score",
            "REAL CHECK(rhythm_score IS NULL OR rhythm_score BETWEEN 0 AND 10)",
        ),
        (
            "ai_clip_transition_boundaries",
            "material_score",
            "REAL CHECK(material_score IS NULL OR material_score BETWEEN 0 AND 10)",
        ),
        (
            "ai_clip_transition_boundaries",
            "suggestion_score",
            "REAL CHECK(suggestion_score IS NULL OR suggestion_score BETWEEN 0 AND 10)",
        ),
        (
            "ai_clip_transition_boundaries",
            "suggestion_scene_score",
            "REAL CHECK(suggestion_scene_score IS NULL OR suggestion_scene_score BETWEEN 0 AND 10)",
        ),
        (
            "ai_clip_transition_boundaries",
            "suggestion_continuity_score",
            "REAL CHECK(suggestion_continuity_score IS NULL OR suggestion_continuity_score BETWEEN 0 AND 10)",
        ),
        (
            "ai_clip_transition_boundaries",
            "suggestion_rhythm_score",
            "REAL CHECK(suggestion_rhythm_score IS NULL OR suggestion_rhythm_score BETWEEN 0 AND 10)",
        ),
        (
            "ai_clip_transition_boundaries",
            "suggestion_material_score",
            "REAL CHECK(suggestion_material_score IS NULL OR suggestion_material_score BETWEEN 0 AND 10)",
        ),
        (
            "ai_transition_match_runs",
            "project_version",
            "INTEGER NOT NULL DEFAULT 0 CHECK(project_version >= 0)",
        ),
        (
            "ai_transition_match_runs",
            "model_id",
            "TEXT NOT NULL DEFAULT 'deepseek-chat'",
        ),
        (
            "ai_transition_match_runs",
            "match_prompt_version",
            "TEXT NOT NULL DEFAULT 'transition-match-v2'",
        ),
        (
            "ai_transition_match_runs",
            "score_prompt_version",
            "TEXT NOT NULL DEFAULT 'transition-score-v1'",
        ),
        (
            "ai_transition_match_runs",
            "threshold",
            "INTEGER NOT NULL DEFAULT 8 CHECK(threshold BETWEEN 0 AND 10)",
        ),
        (
            "ai_transition_match_runs",
            "stage",
            "TEXT NOT NULL DEFAULT 'matching'",
        ),
        (
            "ai_transition_match_runs",
            "total_boundaries",
            "INTEGER NOT NULL DEFAULT 0 CHECK(total_boundaries >= 0)",
        ),
        (
            "ai_transition_match_runs",
            "matched_boundaries",
            "INTEGER NOT NULL DEFAULT 0 CHECK(matched_boundaries >= 0)",
        ),
        (
            "ai_transition_match_runs",
            "auto_applied",
            "INTEGER NOT NULL DEFAULT 0 CHECK(auto_applied >= 0)",
        ),
        (
            "ai_transition_match_runs",
            "suggestions",
            "INTEGER NOT NULL DEFAULT 0 CHECK(suggestions >= 0)",
        ),
        (
            "ai_transition_match_runs",
            "none_suggestions",
            "INTEGER NOT NULL DEFAULT 0 CHECK(none_suggestions >= 0)",
        ),
        (
            "ai_transition_match_runs",
            "failed_boundaries",
            "INTEGER NOT NULL DEFAULT 0 CHECK(failed_boundaries >= 0)",
        ),
        (
            "ai_transition_match_runs",
            "token_usage",
            "INTEGER NOT NULL DEFAULT 0 CHECK(token_usage >= 0)",
        ),
    ] {
        let exists = transaction
            .query_row(
                "SELECT 1 FROM pragma_table_info(?1) WHERE name = ?2",
                params![table, column],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if !exists {
            transaction.execute(
                &format!("ALTER TABLE {table} ADD COLUMN {column} {definition}"),
                [],
            )?;
        }
    }
    transaction.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS ai_clip_text_correction_runs (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            clip_project_id INTEGER NOT NULL REFERENCES ai_clip_projects(id) ON DELETE CASCADE,
            project_version INTEGER NOT NULL CHECK(project_version >= 0),
            status TEXT NOT NULL CHECK(status IN ('running', 'completed', 'cancelled', 'failed')),
            stage TEXT NOT NULL,
            model_id TEXT NOT NULL,
            prompt_version TEXT NOT NULL,
            input_fingerprint TEXT NOT NULL,
            total_items INTEGER NOT NULL DEFAULT 0 CHECK(total_items >= 0),
            processed_items INTEGER NOT NULL DEFAULT 0 CHECK(processed_items >= 0),
            changed_items INTEGER NOT NULL DEFAULT 0 CHECK(changed_items >= 0),
            unchanged_items INTEGER NOT NULL DEFAULT 0 CHECK(unchanged_items >= 0),
            skipped_manual INTEGER NOT NULL DEFAULT 0 CHECK(skipped_manual >= 0),
            skipped_hidden INTEGER NOT NULL DEFAULT 0 CHECK(skipped_hidden >= 0),
            total_batches INTEGER NOT NULL DEFAULT 0 CHECK(total_batches >= 0),
            completed_batches INTEGER NOT NULL DEFAULT 0 CHECK(completed_batches >= 0),
            token_usage INTEGER NOT NULL DEFAULT 0 CHECK(token_usage >= 0),
            error_code TEXT,
            error_message TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_ai_clip_text_correction_runs_project
            ON ai_clip_text_correction_runs(clip_project_id, created_at DESC);
        "#,
    )?;
    transaction.execute(
        "INSERT INTO schema_migrations(version, applied_at) VALUES(21, ?1)",
        [Utc::now().to_rfc3339()],
    )?;
    transaction.commit()?;
    Ok(())
}

/// v22 为统一智能成片建立父任务/批次/阶段/候选/草稿结构，并将剪辑工程
/// 从单一精彩运行唯一绑定迁移为显式来源集合。
pub(crate) fn migrate_ai_v22(connection: &mut Connection) -> crate::database::Result<()> {
    let applied = connection
        .query_row(
            "SELECT 1 FROM schema_migrations WHERE version = 22",
            [],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if applied {
        return Ok(());
    }

    connection.pragma_update(None, "foreign_keys", "OFF")?;
    let migration = (|| -> crate::database::Result<()> {
        let transaction = connection.transaction()?;
        transaction.execute_batch(
            r#"
            CREATE TABLE ai_smart_workflows (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL CHECK(length(trim(name)) > 0),
                mode TEXT NOT NULL CHECK(mode IN ('local', 'live')),
                status TEXT NOT NULL CHECK(status IN (
                    'draft', 'queued', 'running', 'awaiting_selection', 'review_ready',
                    'paused', 'completed', 'cancelled', 'failed'
                )),
                stage TEXT NOT NULL CHECK(stage IN (
                    'preflight', 'asr', 'highlight', 'draft', 'correction', 'transition',
                    'enhancement', 'review'
                )),
                generation INTEGER NOT NULL DEFAULT 1 CHECK(generation > 0),
                source_session_id INTEGER REFERENCES recording_sessions(id) ON DELETE RESTRICT,
                source_summary TEXT NOT NULL,
                provider TEXT NOT NULL,
                model_id TEXT NOT NULL,
                text_scope TEXT NOT NULL,
                authorization_digest TEXT,
                authorized_at TEXT,
                configuration_fingerprint TEXT NOT NULL,
                active_draft_generation INTEGER CHECK(active_draft_generation IS NULL OR active_draft_generation > 0),
                live_cursor_video_id INTEGER REFERENCES videos(id) ON DELETE SET NULL,
                event_sequence INTEGER NOT NULL DEFAULT 0 CHECK(event_sequence >= 0),
                candidate_count INTEGER NOT NULL DEFAULT 0 CHECK(candidate_count >= 0),
                selected_count INTEGER NOT NULL DEFAULT 0 CHECK(selected_count >= 0),
                pending_batch_count INTEGER NOT NULL DEFAULT 0 CHECK(pending_batch_count >= 0),
                last_error_code TEXT,
                last_error_message TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                CHECK((mode = 'live' AND source_session_id IS NOT NULL) OR (mode = 'local' AND source_session_id IS NULL)),
                CHECK((authorization_digest IS NULL) = (authorized_at IS NULL))
            );
            CREATE INDEX idx_ai_smart_workflows_status_updated
                ON ai_smart_workflows(status, updated_at DESC, id DESC);
            CREATE UNIQUE INDEX idx_ai_smart_workflows_active_live_session
                ON ai_smart_workflows(source_session_id)
                WHERE mode = 'live' AND status NOT IN ('completed', 'cancelled');

            CREATE TABLE ai_smart_workflow_batches (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                workflow_id INTEGER NOT NULL REFERENCES ai_smart_workflows(id) ON DELETE CASCADE,
                position INTEGER NOT NULL CHECK(position >= 0),
                video_id INTEGER REFERENCES videos(id) ON DELETE RESTRICT,
                source_fingerprint TEXT NOT NULL,
                project_id INTEGER REFERENCES ai_projects(id) ON DELETE RESTRICT,
                highlight_run_id INTEGER REFERENCES ai_highlight_runs(id) ON DELETE RESTRICT,
                status TEXT NOT NULL CHECK(status IN (
                    'pending', 'queued', 'asr', 'highlight', 'completed', 'failed', 'cancelled'
                )),
                finalized_at TEXT NOT NULL,
                last_error_code TEXT,
                last_error_message TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                UNIQUE(workflow_id, position),
                UNIQUE(workflow_id, source_fingerprint),
                UNIQUE(workflow_id, video_id)
            );
            CREATE INDEX idx_ai_smart_batches_workflow_status_position
                ON ai_smart_workflow_batches(workflow_id, status, position, id);

            CREATE TABLE ai_smart_stage_attempts (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                workflow_id INTEGER NOT NULL REFERENCES ai_smart_workflows(id) ON DELETE CASCADE,
                batch_id INTEGER REFERENCES ai_smart_workflow_batches(id) ON DELETE CASCADE,
                draft_generation INTEGER CHECK(draft_generation IS NULL OR draft_generation > 0),
                stage TEXT NOT NULL CHECK(stage IN (
                    'preflight', 'asr', 'highlight', 'draft', 'correction', 'transition',
                    'enhancement', 'review'
                )),
                input_fingerprint TEXT NOT NULL,
                attempt_generation INTEGER NOT NULL CHECK(attempt_generation > 0),
                status TEXT NOT NULL CHECK(status IN (
                    'pending', 'running', 'completed', 'interrupted', 'cancelled', 'failed'
                )),
                progress INTEGER NOT NULL DEFAULT 0 CHECK(progress BETWEEN 0 AND 100),
                result_kind TEXT,
                result_id INTEGER,
                duration_ms INTEGER CHECK(duration_ms IS NULL OR duration_ms >= 0),
                last_error_code TEXT,
                last_error_message TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                UNIQUE(workflow_id, batch_id, draft_generation, stage, attempt_generation)
            );
            CREATE INDEX idx_ai_smart_attempts_scope_stage_generation
                ON ai_smart_stage_attempts(workflow_id, batch_id, draft_generation, stage, attempt_generation DESC);
            CREATE UNIQUE INDEX idx_ai_smart_attempts_completed_fingerprint
                ON ai_smart_stage_attempts(workflow_id, IFNULL(batch_id, -1), IFNULL(draft_generation, -1), stage, input_fingerprint)
                WHERE status = 'completed';

            CREATE TABLE ai_smart_candidates (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                workflow_id INTEGER NOT NULL REFERENCES ai_smart_workflows(id) ON DELETE CASCADE,
                dedupe_key TEXT NOT NULL,
                semantic_fingerprint TEXT NOT NULL,
                canonical_candidate_id INTEGER NOT NULL REFERENCES ai_highlight_candidates(id) ON DELETE RESTRICT,
                total_score INTEGER NOT NULL CHECK(total_score BETWEEN 0 AND 100),
                qualified INTEGER NOT NULL DEFAULT 0 CHECK(qualified IN (0, 1)),
                selected INTEGER NOT NULL DEFAULT 0 CHECK(selected IN (0, 1)),
                session_start_ms INTEGER NOT NULL CHECK(session_start_ms >= 0),
                session_end_ms INTEGER NOT NULL CHECK(session_end_ms > session_start_ms),
                first_finalized_at TEXT NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                UNIQUE(workflow_id, dedupe_key)
            );
            CREATE INDEX idx_ai_smart_candidates_workflow_score_time
                ON ai_smart_candidates(workflow_id, qualified, selected, total_score DESC, session_start_ms, id);

            CREATE TABLE ai_smart_candidate_sources (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                smart_candidate_id INTEGER NOT NULL REFERENCES ai_smart_candidates(id) ON DELETE CASCADE,
                batch_id INTEGER NOT NULL REFERENCES ai_smart_workflow_batches(id) ON DELETE CASCADE,
                candidate_id INTEGER NOT NULL REFERENCES ai_highlight_candidates(id) ON DELETE RESTRICT,
                video_id INTEGER REFERENCES videos(id) ON DELETE RESTRICT,
                input_id INTEGER NOT NULL REFERENCES ai_project_inputs(id) ON DELETE RESTRICT,
                stable_segment_ids_json TEXT NOT NULL,
                source_start_ms INTEGER NOT NULL CHECK(source_start_ms >= 0),
                source_end_ms INTEGER NOT NULL CHECK(source_end_ms > source_start_ms),
                session_start_ms INTEGER NOT NULL CHECK(session_start_ms >= 0),
                session_end_ms INTEGER NOT NULL CHECK(session_end_ms > session_start_ms),
                source_order INTEGER NOT NULL CHECK(source_order >= 0),
                created_at TEXT NOT NULL,
                UNIQUE(smart_candidate_id, batch_id, candidate_id, source_order)
            );
            CREATE INDEX idx_ai_smart_candidate_sources_candidate_order
                ON ai_smart_candidate_sources(smart_candidate_id, source_order, id);

            CREATE TABLE ai_smart_drafts (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                workflow_id INTEGER NOT NULL REFERENCES ai_smart_workflows(id) ON DELETE CASCADE,
                generation INTEGER NOT NULL CHECK(generation > 0),
                clip_project_id INTEGER NOT NULL UNIQUE,
                ownership TEXT NOT NULL CHECK(ownership IN ('automation', 'user')),
                status TEXT NOT NULL CHECK(status IN (
                    'active', 'review_ready', 'exporting', 'frozen', 'exported', 'failed', 'superseded'
                )),
                automation_project_version INTEGER NOT NULL CHECK(automation_project_version > 0),
                frozen_project_version INTEGER CHECK(frozen_project_version IS NULL OR frozen_project_version > 0),
                first_reviewable_at TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                UNIQUE(workflow_id, generation),
                FOREIGN KEY(clip_project_id) REFERENCES ai_clip_projects_v22(id) ON DELETE CASCADE
            );

            CREATE TABLE ai_clip_projects_v22 (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                highlight_run_id INTEGER NOT NULL REFERENCES ai_highlight_runs(id) ON DELETE CASCADE,
                smart_workflow_id INTEGER REFERENCES ai_smart_workflows(id) ON DELETE CASCADE,
                draft_generation INTEGER CHECK(draft_generation IS NULL OR draft_generation > 0),
                ownership TEXT NOT NULL DEFAULT 'user' CHECK(ownership IN ('automation', 'user')),
                name TEXT NOT NULL,
                export_status TEXT NOT NULL DEFAULT 'idle' CHECK(export_status IN ('idle', 'exporting', 'completed', 'cancelled', 'failed')),
                export_progress INTEGER NOT NULL DEFAULT 0 CHECK(export_progress BETWEEN 0 AND 100),
                output_path TEXT,
                last_error_code TEXT,
                last_error_message TEXT,
                output_width INTEGER CHECK(output_width IS NULL OR output_width > 0),
                output_height INTEGER CHECK(output_height IS NULL OR output_height > 0),
                version INTEGER NOT NULL DEFAULT 1 CHECK(version > 0),
                export_frozen_version INTEGER CHECK(export_frozen_version IS NULL OR export_frozen_version > 0),
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                CHECK((smart_workflow_id IS NULL AND draft_generation IS NULL AND ownership = 'user') OR (smart_workflow_id IS NOT NULL AND draft_generation IS NOT NULL)),
                CHECK((output_width IS NULL) = (output_height IS NULL))
            );
            INSERT INTO ai_clip_projects_v22(
                id, highlight_run_id, smart_workflow_id, draft_generation, ownership,
                name, export_status, export_progress, output_path, last_error_code,
                last_error_message, output_width, output_height, version,
                export_frozen_version, created_at, updated_at
            )
            SELECT id, highlight_run_id, NULL, NULL, 'user', name, export_status,
                   export_progress, output_path, last_error_code, last_error_message,
                   output_width, output_height, version, NULL, created_at, updated_at
            FROM ai_clip_projects;

            CREATE TABLE ai_clip_project_sources (
                clip_project_id INTEGER NOT NULL REFERENCES ai_clip_projects_v22(id) ON DELETE CASCADE,
                highlight_run_id INTEGER NOT NULL REFERENCES ai_highlight_runs(id) ON DELETE RESTRICT,
                workflow_batch_id INTEGER REFERENCES ai_smart_workflow_batches(id) ON DELETE RESTRICT,
                source_order INTEGER NOT NULL CHECK(source_order >= 0),
                primary_source INTEGER NOT NULL DEFAULT 0 CHECK(primary_source IN (0, 1)),
                created_at TEXT NOT NULL,
                PRIMARY KEY(clip_project_id, highlight_run_id),
                UNIQUE(clip_project_id, source_order)
            );
            INSERT INTO ai_clip_project_sources(
                clip_project_id, highlight_run_id, workflow_batch_id, source_order,
                primary_source, created_at
            )
            SELECT id, highlight_run_id, NULL, 0, 1, created_at
            FROM ai_clip_projects_v22;

            DROP TABLE ai_clip_projects;
            ALTER TABLE ai_clip_projects_v22 RENAME TO ai_clip_projects;
            CREATE UNIQUE INDEX idx_ai_clip_projects_manual_default_run
                ON ai_clip_projects(highlight_run_id)
                WHERE smart_workflow_id IS NULL;
            CREATE UNIQUE INDEX idx_ai_clip_projects_workflow_generation
                ON ai_clip_projects(smart_workflow_id, draft_generation)
                WHERE smart_workflow_id IS NOT NULL;
            CREATE INDEX idx_ai_clip_project_sources_run
                ON ai_clip_project_sources(highlight_run_id, clip_project_id);
            CREATE UNIQUE INDEX idx_ai_clip_project_sources_primary
                ON ai_clip_project_sources(clip_project_id)
                WHERE primary_source = 1;

            INSERT INTO schema_migrations(version, applied_at) VALUES(22, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'));
            "#,
        )?;
        transaction.commit()?;
        Ok(())
    })();
    let restore_foreign_keys = connection.pragma_update(None, "foreign_keys", "ON");
    migration?;
    restore_foreign_keys?;
    let violations: i64 =
        connection.query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
            row.get(0)
        })?;
    if violations != 0 {
        return Err(DatabaseError::MigrationIntegrity(format!(
            "AI v22 迁移发现 {violations} 条外键异常"
        )));
    }
    Ok(())
}

/// v23 固定直播智能任务创建时的视频水位，排除历史分片并允许后续乱序登记。
pub(crate) fn migrate_ai_v23(connection: &mut Connection) -> crate::database::Result<()> {
    let applied = connection
        .query_row(
            "SELECT 1 FROM schema_migrations WHERE version = 23",
            [],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if applied {
        return Ok(());
    }
    let transaction = connection.transaction()?;
    transaction.execute(
        "ALTER TABLE ai_smart_workflows ADD COLUMN live_start_video_id INTEGER",
        [],
    )?;
    transaction.execute(
        "INSERT INTO schema_migrations(version, applied_at) VALUES(23, ?1)",
        [Utc::now().to_rfc3339()],
    )?;
    transaction.commit()?;
    Ok(())
}

/// v26 增加转写身份、分块策略与规范化版本字段，并新增本地幂等记录表。
///
/// 迁移只增列、只建表，不重写任何旧行；旧产物保持原有 `provider` 与模型身份。
pub(crate) fn migrate_ai_v26(connection: &mut Connection) -> crate::database::Result<()> {
    let applied = connection
        .query_row(
            "SELECT 1 FROM schema_migrations WHERE version = 26",
            [],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if applied {
        return Ok(());
    }
    let transaction = connection.transaction()?;
    // asr_artifacts：识别身份、分块策略与规范化版本（缓存键组成部分）。
    for (column, definition) in [
        ("chunk_strategy", "TEXT"),
        ("normalization_version", "TEXT"),
        ("vad_parameters", "TEXT"),
        ("hotwords_fingerprint", "TEXT"),
    ] {
        let exists: bool = transaction
            .query_row(
                &format!(
                    "SELECT 1 FROM pragma_table_info('asr_artifacts') WHERE name = '{column}'"
                ),
                [],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if !exists {
            transaction.execute(
                &format!("ALTER TABLE asr_artifacts ADD COLUMN {column} {definition}"),
                [],
            )?;
        }
    }
    // 本地幂等记录：付费提交前先落盘，重启后可找回同一上游任务。
    transaction.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS ai_remote_job_records (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            request_id TEXT NOT NULL,
            task_type TEXT NOT NULL,
            input_hash TEXT NOT NULL,
            upstream_task_id TEXT,
            status TEXT NOT NULL CHECK(status IN ('submitted', 'awaiting_reconcile', 'reused')),
            reconcile_required INTEGER NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            UNIQUE(request_id, task_type)
        );
        CREATE INDEX IF NOT EXISTS idx_ai_remote_job_records_state
            ON ai_remote_job_records(status, reconcile_required);
        "#,
    )?;
    transaction.execute(
        "INSERT INTO schema_migrations(version, applied_at) VALUES(26, ?1)",
        [Utc::now().to_rfc3339()],
    )?;
    transaction.commit()?;
    Ok(())
}

/// v28 为模型调用设置补充「解析来源、解析时间与下次解析时间」（步骤 2 任务 9.4）。
///
/// 迁移号说明：27 已被步骤 3（特效字幕）占用；本 change 按「实施顺序 = 迁移顺序」取 28，
/// 步骤 4（生成式动画）顺延为 29，`docs/v2-delivery-order.md` 已同步。
///
/// 只增列、不重写旧行：既有阈值取值与 `provider` 字段保持原样（`provider` 退化为
/// 兼容列，调用路径由统一调用点决定），因此旧库升级后行为与取值都不变。
pub(crate) fn migrate_ai_v28(connection: &mut Connection) -> crate::database::Result<()> {
    let applied = connection
        .query_row(
            "SELECT 1 FROM schema_migrations WHERE version = 28",
            [],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if applied {
        return Ok(());
    }
    let transaction = connection.transaction()?;
    for column in ["model_source", "resolved_at", "next_resolution_at"] {
        let exists: bool = transaction
            .query_row(
                &format!(
                    "SELECT 1 FROM pragma_table_info('llm_provider_settings') WHERE name = '{column}'"
                ),
                [],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if !exists {
            transaction.execute(
                &format!("ALTER TABLE llm_provider_settings ADD COLUMN {column} TEXT"),
                [],
            )?;
        }
    }
    transaction.execute(
        "INSERT INTO schema_migrations(version, applied_at) VALUES(28, ?1)",
        [Utc::now().to_rfc3339()],
    )?;
    transaction.commit()?;
    Ok(())
}

/// v29 增加生成式动画资产表（步骤 4 任务 1.2）。
///
/// 只建表、只加索引，不重写任何旧行，因此旧工程行为不变。生成资产是单工程单高光的
/// 产物，MUST NOT 写入目录素材表；`replace` 与 `insert` 的绑定由 CHECK 约束保证互斥且完整。
pub(crate) fn migrate_ai_v29(connection: &mut Connection) -> crate::database::Result<()> {
    let applied = connection
        .query_row(
            "SELECT 1 FROM schema_migrations WHERE version = 29",
            [],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if applied {
        return Ok(());
    }
    let transaction = connection.transaction()?;
    transaction.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS ai_generated_animation_assets (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            project_id INTEGER NOT NULL,
            candidate_id INTEGER NOT NULL,
            highlight_run_id INTEGER,
            generation INTEGER NOT NULL CHECK(generation >= 1),
            reuse_key TEXT NOT NULL,
            composition TEXT NOT NULL CHECK(composition IN ('replace', 'insert')),
            status TEXT NOT NULL CHECK(status IN ('pending', 'validated', 'rejected', 'invalidated')),
            replace_start_ms INTEGER,
            replace_end_ms INTEGER,
            left_segment_id INTEGER,
            right_segment_id INTEGER,
            boundary_id INTEGER,
            upstream_task_id TEXT,
            sha256 TEXT,
            width INTEGER,
            height INTEGER,
            fps INTEGER,
            duration_ms INTEGER,
            rejection_code TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            UNIQUE(candidate_id, generation),
            CHECK (
                (composition = 'replace'
                    AND replace_start_ms IS NOT NULL AND replace_end_ms IS NOT NULL
                    AND replace_end_ms > replace_start_ms
                    AND left_segment_id IS NULL AND right_segment_id IS NULL AND boundary_id IS NULL)
                OR
                (composition = 'insert'
                    AND left_segment_id IS NOT NULL AND right_segment_id IS NOT NULL
                    AND boundary_id IS NOT NULL
                    AND replace_start_ms IS NULL AND replace_end_ms IS NULL)
            )
        );
        CREATE INDEX IF NOT EXISTS idx_ai_generated_animation_project
            ON ai_generated_animation_assets(project_id, status);
        CREATE INDEX IF NOT EXISTS idx_ai_generated_animation_candidate
            ON ai_generated_animation_assets(candidate_id, generation DESC);
        CREATE INDEX IF NOT EXISTS idx_ai_generated_animation_reuse
            ON ai_generated_animation_assets(reuse_key, status);
        "#,
    )?;
    transaction.execute(
        "INSERT INTO schema_migrations(version, applied_at) VALUES(29, ?1)",
        [Utc::now().to_rfc3339()],
    )?;
    transaction.commit()?;
    Ok(())
}

/// v30 为高光增强记录补充「已产生成本」（步骤 4 任务 8.3）。
///
/// 只增列、不重写旧行：既有记录的成本视为 0（历史记录不含按次付费的生成分支）。
/// v32：新增**转写句段级**的文案纠正层，支撑"片段级前置"流程。
///
/// 目标流程是「识别 → 提取精彩 → 每片段特效生成 → **校正文案** → 最后拼合辑」，
/// 即校正发生在**拼合辑之前**。当前实现把校正作用在 `ai_clip_subtitles`（合辑字幕），
/// 那要求先有合辑，顺序相反。
///
/// 因此新增本表承载"句段级纠正"：合辑字幕快照生成时优先取纠正文本
/// （`COALESCE(纠正, normalized_text)`），从而让校正真正前置，且**不修改
/// `transcript_segments`**——转写是 ASR 的原始产物，不应被覆盖。
pub(crate) fn migrate_ai_v32(connection: &mut Connection) -> crate::database::Result<()> {
    let applied = connection
        .query_row(
            "SELECT 1 FROM schema_migrations WHERE version = 32",
            [],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if applied {
        return Ok(());
    }
    let transaction = connection.transaction()?;
    transaction.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS transcript_segment_corrections (
            segment_id TEXT PRIMARY KEY
                REFERENCES transcript_segments(id) ON DELETE CASCADE,
            original_text TEXT NOT NULL,
            corrected_text TEXT NOT NULL,
            run_id INTEGER,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_transcript_segment_corrections_run
            ON transcript_segment_corrections(run_id);
        "#,
    )?;
    transaction.execute(
        "INSERT INTO schema_migrations(version, applied_at) VALUES(32, ?1)",
        [Utc::now().to_rfc3339()],
    )?;
    transaction.commit()?;
    Ok(())
}

/// v31：把"旧默认 30 秒"的单次调用超时提升到新默认 90 秒。
///
/// 实测一次精彩分析批次（长提示词 + 中文 JSON 输出）需要 6–30 秒以上，30 秒会
/// 频繁撞线并被归为可重试的 `Temporary`，表现为"部分批次分析失败"。此前 30 秒
/// 是**默认值**，会被原样持久化进设置表，因此只提升常量不足以让既有用户受益。
///
/// 只改仍等于旧默认的行：用户显式设过其它值的不会被覆盖。
pub(crate) fn migrate_ai_v31(connection: &mut Connection) -> crate::database::Result<()> {
    let applied = connection
        .query_row(
            "SELECT 1 FROM schema_migrations WHERE version = 31",
            [],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if applied {
        return Ok(());
    }
    let transaction = connection.transaction()?;
    transaction.execute(
        "UPDATE llm_provider_settings SET timeout_ms = 90000 WHERE timeout_ms = 30000",
        [],
    )?;
    transaction.execute(
        "INSERT INTO schema_migrations(version, applied_at) VALUES(31, ?1)",
        [Utc::now().to_rfc3339()],
    )?;
    transaction.commit()?;
    Ok(())
}

pub(crate) fn migrate_ai_v30(connection: &mut Connection) -> crate::database::Result<()> {
    let applied = connection
        .query_row(
            "SELECT 1 FROM schema_migrations WHERE version = 30",
            [],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if applied {
        return Ok(());
    }
    let transaction = connection.transaction()?;
    transaction.execute(
        "ALTER TABLE ai_highlight_enhancements ADD COLUMN cost_minor INTEGER NOT NULL DEFAULT 0",
        [],
    )?;
    transaction.execute(
        "INSERT INTO schema_migrations(version, applied_at) VALUES(30, ?1)",
        [Utc::now().to_rfc3339()],
    )?;
    transaction.commit()?;
    Ok(())
}

/// v27 增加高光表现形式增强记录与特效字幕渲染缓存（步骤 3 任务 1.2 / 任务组 9）。
///
/// 迁移只建表、只加索引，不重写任何旧行，因此旧工程行为不变。
///
/// 说明：本版本号归属步骤 3（26=步骤 1、27=步骤 3、28=步骤 4），因此智能阶段
/// 枚举新增 `enhancement` 所需的 CHECK 扩展也落在本迁移内 —— 新增阶段值属于步骤 3
/// 的编排改动，不能占用步骤 4 的 28。
pub(crate) fn migrate_ai_v27(connection: &mut Connection) -> crate::database::Result<()> {
    let applied = connection
        .query_row(
            "SELECT 1 FROM schema_migrations WHERE version = 27",
            [],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if applied {
        return Ok(());
    }
    let transaction = connection.transaction()?;
    transaction.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS ai_highlight_enhancements (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            project_id INTEGER NOT NULL,
            candidate_id INTEGER NOT NULL,
            highlight_run_id INTEGER,
            generation INTEGER NOT NULL CHECK(generation >= 1),
            kind TEXT NOT NULL CHECK(kind IN ('effect_caption', 'video_animation')),
            status TEXT NOT NULL CHECK(status IN ('suggested', 'applied', 'rejected', 'superseded')),
            start_ms INTEGER NOT NULL CHECK(start_ms >= 0),
            end_ms INTEGER NOT NULL CHECK(end_ms > start_ms),
            emphasis_words_json TEXT NOT NULL DEFAULT '[]',
            composition_version TEXT,
            composition_fingerprint TEXT,
            render_cache_key TEXT,
            source_fingerprint TEXT NOT NULL,
            suppression_ranges_json TEXT NOT NULL DEFAULT '[]',
            rejection_code TEXT,
            rejection_detail TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            UNIQUE(candidate_id, generation)
        );
        CREATE INDEX IF NOT EXISTS idx_ai_highlight_enhancements_project
            ON ai_highlight_enhancements(project_id, status);
        CREATE INDEX IF NOT EXISTS idx_ai_highlight_enhancements_candidate
            ON ai_highlight_enhancements(candidate_id, generation DESC);

        CREATE TABLE IF NOT EXISTS ai_effect_caption_renders (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            project_id INTEGER NOT NULL,
            candidate_id INTEGER NOT NULL,
            cache_key TEXT NOT NULL UNIQUE,
            composition_version TEXT NOT NULL,
            composition_fingerprint TEXT NOT NULL,
            font_version TEXT NOT NULL,
            browser_version TEXT NOT NULL,
            renderer_version TEXT NOT NULL,
            engine_script_version TEXT NOT NULL,
            width INTEGER NOT NULL,
            height INTEGER NOT NULL,
            fps INTEGER NOT NULL,
            frame_count INTEGER NOT NULL,
            frames_dir TEXT NOT NULL,
            status TEXT NOT NULL CHECK(status IN ('rendering', 'ready', 'failed')),
            failure_code TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_ai_effect_caption_renders_candidate
            ON ai_effect_caption_renders(candidate_id, width, height, fps, status);
        "#,
    )?;
    transaction.execute(
        "INSERT INTO schema_migrations(version, applied_at) VALUES(27, ?1)",
        [Utc::now().to_rfc3339()],
    )?;
    transaction.commit()?;
    Ok(())
}

/// 生成式动画资产（步骤 4 任务 1.1 / 1.2）。
///
/// 生成资产是单工程单高光的产物：MUST NOT 写入目录素材表、不产生 `renderMode`、
/// 不被边界召回作为候选。`replace` 与 `insert` 的绑定必须互斥且完整。
impl AiRepository {
    /// 写入（或在未校验时覆盖）一条生成式动画资产。
    ///
    /// 已 `validated` 的行不被覆盖：媒体校验通过的资产已有下游引用，必须保持稳定。
    pub fn record_generated_animation_asset(
        &self,
        input: NewAiGeneratedAnimationAsset,
    ) -> Result<AiGeneratedAnimationAsset> {
        if input.generation < 1 {
            return Err(AiRepositoryError::Integrity(
                "生成动画代次必须从 1 开始".to_owned(),
            ));
        }
        if input.reuse_key.trim().is_empty() {
            return Err(AiRepositoryError::Integrity(
                "生成动画复用键不能为空".to_owned(),
            ));
        }
        match (input.composition, input.replace_range, input.insert_binding) {
            (AiGeneratedAnimationComposition::Replace, Some(range), None) => {
                if range.end_ms <= range.start_ms {
                    return Err(AiRepositoryError::Integrity(
                        "replace 目标范围必须满足 end_ms > start_ms".to_owned(),
                    ));
                }
            }
            (AiGeneratedAnimationComposition::Insert, None, Some(binding)) => {
                if binding.left_segment_id <= 0
                    || binding.right_segment_id <= 0
                    || binding.boundary_id <= 0
                {
                    return Err(AiRepositoryError::Integrity(
                        "insert 邻接绑定 ID 必须为正".to_owned(),
                    ));
                }
            }
            _ => {
                return Err(AiRepositoryError::Integrity(
                    "生成动画绑定与合成方式不匹配".to_owned(),
                ));
            }
        }
        let now = Utc::now().to_rfc3339();
        let connection = self.database.connection()?;
        connection.execute(
            r#"
            INSERT INTO ai_generated_animation_assets(
                project_id, candidate_id, highlight_run_id, generation, reuse_key, composition, status,
                replace_start_ms, replace_end_ms, left_segment_id, right_segment_id, boundary_id,
                upstream_task_id, sha256, width, height, fps, duration_ms, rejection_code,
                created_at, updated_at
            ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?20)
            ON CONFLICT(candidate_id, generation) DO UPDATE SET
                reuse_key = excluded.reuse_key,
                composition = excluded.composition,
                status = excluded.status,
                replace_start_ms = excluded.replace_start_ms,
                replace_end_ms = excluded.replace_end_ms,
                left_segment_id = excluded.left_segment_id,
                right_segment_id = excluded.right_segment_id,
                boundary_id = excluded.boundary_id,
                upstream_task_id = excluded.upstream_task_id,
                sha256 = excluded.sha256,
                width = excluded.width,
                height = excluded.height,
                fps = excluded.fps,
                duration_ms = excluded.duration_ms,
                rejection_code = excluded.rejection_code,
                updated_at = excluded.updated_at
            WHERE ai_generated_animation_assets.status <> 'validated'
            "#,
            params![
                input.project_id,
                input.candidate_id,
                input.highlight_run_id,
                input.generation,
                input.reuse_key,
                input.composition.as_str(),
                input.status.as_str(),
                input.replace_range.map(|range| range.start_ms as i64),
                input.replace_range.map(|range| range.end_ms as i64),
                input.insert_binding.map(|binding| binding.left_segment_id),
                input
                    .insert_binding
                    .map(|binding| binding.right_segment_id),
                input.insert_binding.map(|binding| binding.boundary_id),
                input.upstream_task_id,
                input.sha256,
                input.width.map(i64::from),
                input.height.map(i64::from),
                input.fps.map(i64::from),
                input.duration_ms.map(|value| value as i64),
                input.rejection_code,
                now,
            ],
        )?;
        drop(connection);
        self.get_generated_animation_asset(input.candidate_id, input.generation)?
            .ok_or(AiRepositoryError::NotFound("生成动画资产"))
    }

    /// 按候选与代次读取一条生成式动画资产。
    pub fn get_generated_animation_asset(
        &self,
        candidate_id: i64,
        generation: i64,
    ) -> Result<Option<AiGeneratedAnimationAsset>> {
        let connection = self.database.connection()?;
        connection
            .query_row(
                &format!(
                    "SELECT {GENERATED_ANIMATION_COLUMNS} FROM ai_generated_animation_assets WHERE candidate_id = ?1 AND generation = ?2"
                ),
                params![candidate_id, generation],
                map_generated_animation,
            )
            .optional()?
            .map(parse_generated_animation)
            .transpose()
    }

    /// 按资产 ID 读取生成式动画资产。
    pub fn get_generated_animation_asset_by_id(
        &self,
        asset_id: i64,
    ) -> Result<Option<AiGeneratedAnimationAsset>> {
        let connection = self.database.connection()?;
        connection
            .query_row(
                &format!(
                    "SELECT {GENERATED_ANIMATION_COLUMNS} FROM ai_generated_animation_assets WHERE id = ?1"
                ),
                [asset_id],
                map_generated_animation,
            )
            .optional()?
            .map(parse_generated_animation)
            .transpose()
    }

    /// 把生成资产标记为失效（用户移除或邻接变化）：必须重新审阅，MUST NOT 自动重新生成。
    ///
    /// 幂等：已失效的资产重复移除不报错。
    pub fn invalidate_generated_animation_asset(
        &self,
        asset_id: i64,
        reason: &str,
    ) -> Result<AiGeneratedAnimationAsset> {
        let asset = self
            .get_generated_animation_asset_by_id(asset_id)?
            .ok_or(AiRepositoryError::NotFound("生成式动画资产"))?;
        if asset.status != AiGeneratedAnimationStatus::Invalidated {
            let connection = self.database.connection()?;
            connection.execute(
                "UPDATE ai_generated_animation_assets SET status = 'invalidated', rejection_code = ?2, updated_at = ?3 WHERE id = ?1",
                params![asset_id, reason, Utc::now().to_rfc3339()],
            )?;
        }
        self.get_generated_animation_asset_by_id(asset_id)?
            .ok_or(AiRepositoryError::NotFound("生成式动画资产"))
    }

    /// 列出工程的全部生成式动画资产。
    pub fn list_generated_animation_assets(        &self,
        project_id: i64,
    ) -> Result<Vec<AiGeneratedAnimationAsset>> {
        let connection = self.database.connection()?;
        let mut statement = connection.prepare(&format!(
            "SELECT {GENERATED_ANIMATION_COLUMNS} FROM ai_generated_animation_assets WHERE project_id = ?1 ORDER BY candidate_id, generation"
        ))?;
        let rows = statement.query_map([project_id], map_generated_animation)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .map(parse_generated_animation)
            .collect()
    }

    /// 按复用键查找已通过校验的生成资产；命中即复用，不产生新的上游费用。
    pub fn find_validated_generated_animation(
        &self,
        project_id: i64,
        reuse_key: &str,
    ) -> Result<Option<AiGeneratedAnimationAsset>> {
        let connection = self.database.connection()?;
        connection
            .query_row(
                &format!(
                    "SELECT {GENERATED_ANIMATION_COLUMNS} FROM ai_generated_animation_assets WHERE project_id = ?1 AND reuse_key = ?2 AND status = 'validated' ORDER BY generation DESC LIMIT 1"
                ),
                params![project_id, reuse_key],
                map_generated_animation,
            )
            .optional()?
            .map(parse_generated_animation)
            .transpose()
    }
}

/// 高光表现形式增强记录（步骤 3 任务 1.5：过期代次只能停在历史建议）。
impl AiRepository {
    /// 写入一条增强记录。
    ///
    /// `current_source_fingerprint` 是**当前**工程已发布内容的指纹。与建议携带的
    /// `source_fingerprint` 不一致，说明该建议是用户编辑之前的旧代次：它只能以
    /// `suggested` 落库，并且不得覆盖同一候选上已经 `applied` 的结果。
    pub fn record_enhancement(
        &self,
        input: NewAiHighlightEnhancement,
        current_source_fingerprint: &str,
    ) -> Result<AiHighlightEnhancement> {
        if input.generation < 1 {
            return Err(AiRepositoryError::Integrity(
                "增强代次必须从 1 开始".to_owned(),
            ));
        }
        if input.end_ms <= input.start_ms {
            return Err(AiRepositoryError::Integrity(
                "增强区间必须满足 end_ms > start_ms".to_owned(),
            ));
        }
        if input.source_fingerprint.trim().is_empty() {
            return Err(AiRepositoryError::Integrity(
                "增强来源指纹不能为空".to_owned(),
            ));
        }
        let stale = input.source_fingerprint != current_source_fingerprint;
        // 决策五：同一高光只保留一种可导出的增强方式，两条分支不得同时进入工程。
        if !stale && input.status == AiEnhancementStatus::Applied {
            let conflicting = self
                .database
                .connection()?
                .query_row(
                    "SELECT 1 FROM ai_highlight_enhancements
                      WHERE candidate_id = ?1 AND status = 'applied' AND kind <> ?2
                      LIMIT 1",
                    params![input.candidate_id, input.kind.as_str()],
                    |_| Ok(()),
                )
                .optional()?
                .is_some();
            if conflicting {
                return Err(AiRepositoryError::InvalidState(
                    "该高光已有另一种可导出的增强方式".to_owned(),
                ));
            }
        }
        let (status, rejection_code, rejection_detail) = if stale {
            (
                AiEnhancementStatus::Suggested,
                Some("enhancement_stale_generation".to_owned()),
                Some("工程内容已被修改，该建议只保留为历史建议".to_owned()),
            )
        } else {
            (
                input.status,
                input.rejection_code.clone(),
                input.rejection_detail.clone(),
            )
        };
        let emphasis_words_json = serde_json::to_string(&input.emphasis_words)
            .map_err(|_| AiRepositoryError::Serialization("强调词无法序列化".to_owned()))?;
        let suppression_ranges_json = serde_json::to_string(&input.suppression_ranges)
            .map_err(|_| AiRepositoryError::Serialization("抑制区间无法序列化".to_owned()))?;
        let now = Utc::now().to_rfc3339();
        let connection = self.database.connection()?;
        connection.execute(
            r#"
            INSERT INTO ai_highlight_enhancements(
                project_id, candidate_id, highlight_run_id, generation, kind, status,
                start_ms, end_ms, emphasis_words_json, composition_version,
                composition_fingerprint, render_cache_key, source_fingerprint,
                suppression_ranges_json, rejection_code, rejection_detail, cost_minor,
                created_at, updated_at
            ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?18)
            ON CONFLICT(candidate_id, generation) DO UPDATE SET
                status = excluded.status,
                start_ms = excluded.start_ms,
                end_ms = excluded.end_ms,
                emphasis_words_json = excluded.emphasis_words_json,
                composition_version = excluded.composition_version,
                composition_fingerprint = excluded.composition_fingerprint,
                render_cache_key = excluded.render_cache_key,
                source_fingerprint = excluded.source_fingerprint,
                suppression_ranges_json = excluded.suppression_ranges_json,
                rejection_code = excluded.rejection_code,
                rejection_detail = excluded.rejection_detail,
                cost_minor = excluded.cost_minor,
                updated_at = excluded.updated_at
            WHERE ai_highlight_enhancements.status <> 'applied'
            "#,
            params![
                input.project_id,
                input.candidate_id,
                input.highlight_run_id,
                input.generation,
                input.kind.as_str(),
                status.as_str(),
                input.start_ms as i64,
                input.end_ms as i64,
                emphasis_words_json,
                input.composition_version,
                input.composition_fingerprint,
                input.render_cache_key,
                input.source_fingerprint,
                suppression_ranges_json,
                rejection_code,
                rejection_detail,
                input.cost_minor as i64,
                now,
            ],
        )?;
        drop(connection);
        self.get_enhancement(input.candidate_id, input.generation)?
            .ok_or(AiRepositoryError::NotFound("增强记录"))
    }

    pub fn get_enhancement(
        &self,
        candidate_id: i64,
        generation: i64,
    ) -> Result<Option<AiHighlightEnhancement>> {
        let connection = self.database.connection()?;
        let row = connection
            .query_row(
                &format!("SELECT {ENHANCEMENT_COLUMNS} FROM ai_highlight_enhancements WHERE candidate_id = ?1 AND generation = ?2"),
                params![candidate_id, generation],
                map_enhancement,
            )
            .optional()?;
        row.map(parse_enhancement).transpose()
    }

    /// 候选当前生效的增强：优先返回 `applied`，否则返回最新代次。
    pub fn active_enhancement_for_candidate(
        &self,
        candidate_id: i64,
    ) -> Result<Option<AiHighlightEnhancement>> {
        let connection = self.database.connection()?;
        let row = connection
            .query_row(
                &format!(
                    "SELECT {ENHANCEMENT_COLUMNS} FROM ai_highlight_enhancements WHERE candidate_id = ?1 ORDER BY (status = 'applied') DESC, generation DESC, id DESC LIMIT 1"
                ),
                [candidate_id],
                map_enhancement,
            )
            .optional()?;
        row.map(parse_enhancement).transpose()
    }

    /// 工程内所有已生效的增强（导出与基础字幕抑制的输入）。
    pub fn applied_enhancements_for_project(
        &self,
        project_id: i64,
    ) -> Result<Vec<AiHighlightEnhancement>> {
        let connection = self.database.connection()?;
        let mut statement = connection.prepare(&format!(
            "SELECT {ENHANCEMENT_COLUMNS} FROM ai_highlight_enhancements WHERE project_id = ?1 AND status = 'applied' ORDER BY start_ms, id"
        ))?;
        let rows = statement
            .query_map([project_id], map_enhancement)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        rows.into_iter().map(parse_enhancement).collect()
    }

    /// 列出工程的全部增强记录（含非终态），供审阅栏按高光展示进行中状态（任务 9.2）。
    pub fn list_enhancements_for_project(
        &self,
        project_id: i64,
    ) -> Result<Vec<AiHighlightEnhancement>> {
        let connection = self.database.connection()?;
        let mut statement = connection.prepare(&format!(
            "SELECT {ENHANCEMENT_COLUMNS} FROM ai_highlight_enhancements WHERE project_id = ?1 ORDER BY candidate_id, generation DESC, id DESC"
        ))?;
        let rows = statement
            .query_map([project_id], map_enhancement)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        rows.into_iter().map(parse_enhancement).collect()
    }

    /// 按高光组装增强审阅项（步骤 4 任务 9.1）。
    ///
    /// 组合增强记录与生成资产：已就绪的高光给出最终方式、合成方式、资产校验状态、
    /// 降级原因与已产生成本；尚无结果的高光以进行中表达，不阻塞整个审阅栏。
    pub fn enhancement_review_for_project(
        &self,
        project_id: i64,
        candidate_ids: &[i64],
    ) -> Result<Vec<AiEnhancementReviewItem>> {
        let enforcements = self.list_enhancements_for_project(project_id)?;
        let assets = self.list_generated_animation_assets(project_id)?;
        Ok(enhancement_review_items(
            candidate_ids,
            &enforcements,
            &assets,
        ))
    }

    /// 把同一候选上更早代次的 `applied` 记录标记为 `superseded`（任务 1.5）。
    pub fn supersede_older_enhancements(
        &self,
        candidate_id: i64,
        generation: i64,
    ) -> Result<usize> {
        let connection = self.database.connection()?;
        let changed = connection.execute(
            "UPDATE ai_highlight_enhancements SET status = 'superseded', updated_at = ?1 WHERE candidate_id = ?2 AND generation < ?3 AND status = 'applied'",
            params![Utc::now().to_rfc3339(), candidate_id, generation],
        )?;
        Ok(changed)
    }
}

/// 特效字幕渲染尝试（步骤 3 任务组 9 的持久化基础）。
impl AiRepository {
    /// 开始一次渲染尝试。
    ///
    /// 返回 `(尝试, 是否命中缓存)`：同一 `cache_key` 已存在且状态为 `ready` 时命中缓存，
    /// 调用方应直接复用帧序列；否则把该行重置为 `rendering` 并返回。不同 `cache_key`
    /// （任一版本字段变化）必然落到新行，因此"任一字段变化即创建新的渲染尝试"。
    pub fn begin_render_attempt(
        &self,
        input: NewAiEffectCaptionRenderAttempt,
    ) -> Result<(AiEffectCaptionRenderAttempt, bool)> {
        if input.cache_key.trim().is_empty() {
            return Err(AiRepositoryError::Integrity(
                "渲染缓存键不能为空".to_owned(),
            ));
        }
        if input.width == 0 || input.height == 0 || input.fps == 0 {
            return Err(AiRepositoryError::Integrity("渲染规格无效".to_owned()));
        }
        let now = Utc::now().to_rfc3339();
        let connection = self.database.connection()?;
        // 命中缓存的判定必须发生在重置之前。
        let previous_status: Option<String> = connection
            .query_row(
                "SELECT status FROM ai_effect_caption_renders WHERE cache_key = ?1",
                [&input.cache_key],
                |row| row.get(0),
            )
            .optional()?;
        let cached = previous_status.as_deref() == Some("ready");
        connection.execute(
            r#"
            INSERT INTO ai_effect_caption_renders(
                project_id, candidate_id, cache_key, composition_version, composition_fingerprint,
                font_version, browser_version, renderer_version, engine_script_version,
                width, height, fps, frame_count, frames_dir, status, failure_code, created_at, updated_at
            ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, 0, ?13, 'rendering', NULL, ?14, ?14)
            ON CONFLICT(cache_key) DO UPDATE SET
                -- 命中缓存（已 ready 且已有帧）时保持 ready，交给调用方直接复用；
                -- 仅在失败或未完成时复位为 rendering。
                status = CASE
                    WHEN ai_effect_caption_renders.status = 'ready'
                         AND ai_effect_caption_renders.frame_count > 0
                    THEN ai_effect_caption_renders.status
                    ELSE 'rendering'
                END,
                failure_code = CASE
                    WHEN ai_effect_caption_renders.status = 'ready'
                         AND ai_effect_caption_renders.frame_count > 0
                    THEN ai_effect_caption_renders.failure_code
                    ELSE NULL
                END,
                updated_at = excluded.updated_at
            "#,
            params![
                input.project_id,
                input.candidate_id,
                input.cache_key,
                input.composition_version,
                input.composition_fingerprint,
                input.font_version,
                input.browser_version,
                input.renderer_version,
                input.engine_script_version,
                input.width as i64,
                input.height as i64,
                input.fps as i64,
                input.frames_dir,
                now,
            ],
        )?;
        drop(connection);
        let attempt = self
            .find_render_attempt_by_key(&input.cache_key)?
            .ok_or(AiRepositoryError::NotFound("渲染尝试"))?;
        Ok((attempt, cached))
    }

    pub fn find_render_attempt_by_key(
        &self,
        cache_key: &str,
    ) -> Result<Option<AiEffectCaptionRenderAttempt>> {
        let connection = self.database.connection()?;
        let row = connection
            .query_row(
                &format!("SELECT {RENDER_ATTEMPT_COLUMNS} FROM ai_effect_caption_renders WHERE cache_key = ?1"),
                [cache_key],
                map_render_attempt,
            )
            .optional()?;
        row.map(parse_render_attempt).transpose()
    }

    pub fn find_ready_render_attempt(
        &self,
        cache_key: &str,
    ) -> Result<Option<AiEffectCaptionRenderAttempt>> {
        let connection = self.database.connection()?;
        let row = connection
            .query_row(
                &format!("SELECT {RENDER_ATTEMPT_COLUMNS} FROM ai_effect_caption_renders WHERE cache_key = ?1 AND status = 'ready' AND frame_count > 0"),
                [cache_key],
                map_render_attempt,
            )
            .optional()?;
        row.map(parse_render_attempt).transpose()
    }

    pub fn mark_render_attempt_ready(
        &self,
        attempt_id: i64,
        frame_count: u64,
    ) -> Result<AiEffectCaptionRenderAttempt> {
        if frame_count == 0 {
            return Err(AiRepositoryError::Integrity(
                "渲染完成必须至少有一帧".to_owned(),
            ));
        }
        let connection = self.database.connection()?;
        let changed = connection.execute(
            &format!(
                "UPDATE ai_effect_caption_renders SET status = '{}', frame_count = ?1, failure_code = NULL, updated_at = ?2 WHERE id = ?3",
                AiRenderAttemptStatus::Ready.as_str()
            ),
            params![frame_count as i64, Utc::now().to_rfc3339(), attempt_id],
        )?;
        if changed == 0 {
            return Err(AiRepositoryError::NotFound("渲染尝试"));
        }
        drop(connection);
        self.get_render_attempt(attempt_id)?
            .ok_or(AiRepositoryError::NotFound("渲染尝试"))
    }

    pub fn mark_render_attempt_failed(
        &self,
        attempt_id: i64,
        failure_code: &str,
    ) -> Result<AiEffectCaptionRenderAttempt> {
        if failure_code.trim().is_empty() {
            return Err(AiRepositoryError::Integrity("失败原因不能为空".to_owned()));
        }
        let connection = self.database.connection()?;
        connection.execute(
            &format!(
                "UPDATE ai_effect_caption_renders SET status = '{}', frame_count = 0, failure_code = ?1, updated_at = ?2 WHERE id = ?3",
                AiRenderAttemptStatus::Failed.as_str()
            ),
            params![failure_code, Utc::now().to_rfc3339(), attempt_id],
        )?;
        drop(connection);
        self.get_render_attempt(attempt_id)?
            .ok_or(AiRepositoryError::NotFound("渲染尝试"))
    }

    /// 删除某候选的全部渲染缓存条目，返回被删除条目的帧目录（任务 9.3）。
    ///
    /// 只删除渲染缓存行；工程、ASR 产物与已导出成片不在这些表与目录内，因此不受影响。
    pub fn delete_render_attempts_for_candidate(&self, candidate_id: i64) -> Result<Vec<String>> {
        let mut connection = self.database.connection()?;
        let transaction = connection.transaction()?;
        let directories = {
            let mut statement = transaction.prepare(
                "SELECT frames_dir FROM ai_effect_caption_renders WHERE candidate_id = ?1",
            )?;
            statement
                .query_map([candidate_id], |row| row.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?
        };
        transaction.execute(
            "DELETE FROM ai_effect_caption_renders WHERE candidate_id = ?1",
            [candidate_id],
        )?;
        transaction.commit()?;
        Ok(directories)
    }

    /// 删除指定时间之前创建且**未在渲染中**的缓存条目，返回被删除条目的帧目录。
    pub fn prune_render_attempts_created_before(&self, cutoff: &str) -> Result<Vec<String>> {
        let mut connection = self.database.connection()?;
        let transaction = connection.transaction()?;
        let directories = {
            let mut statement = transaction.prepare(
                "SELECT frames_dir FROM ai_effect_caption_renders WHERE created_at < ?1 AND status <> 'rendering'",
            )?;
            statement
                .query_map([cutoff], |row| row.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?
        };
        transaction.execute(
            "DELETE FROM ai_effect_caption_renders WHERE created_at < ?1 AND status <> 'rendering'",
            [cutoff],
        )?;
        transaction.commit()?;
        Ok(directories)
    }

    pub fn render_attempt_count(&self) -> Result<usize> {
        let connection = self.database.connection()?;
        let count: i64 = connection.query_row(
            "SELECT COUNT(*) FROM ai_effect_caption_renders",
            [],
            |row| row.get(0),
        )?;
        Ok(count as usize)
    }

    pub fn get_render_attempt(
        &self,
        attempt_id: i64,
    ) -> Result<Option<AiEffectCaptionRenderAttempt>> {
        let connection = self.database.connection()?;
        let row = connection
            .query_row(
                &format!(
                    "SELECT {RENDER_ATTEMPT_COLUMNS} FROM ai_effect_caption_renders WHERE id = ?1"
                ),
                [attempt_id],
                map_render_attempt,
            )
            .optional()?;
        row.map(parse_render_attempt).transpose()
    }
}

const RENDER_ATTEMPT_COLUMNS: &str = "id, project_id, candidate_id, cache_key, composition_version, composition_fingerprint, font_version, browser_version, renderer_version, engine_script_version, width, height, fps, frame_count, frames_dir, status, failure_code, created_at, updated_at";

type RenderAttemptRow = (
    i64,
    i64,
    i64,
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    i64,
    i64,
    i64,
    i64,
    String,
    String,
    Option<String>,
    String,
    String,
);

fn map_render_attempt(row: &rusqlite::Row<'_>) -> rusqlite::Result<RenderAttemptRow> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
        row.get(8)?,
        row.get(9)?,
        row.get(10)?,
        row.get(11)?,
        row.get(12)?,
        row.get(13)?,
        row.get(14)?,
        row.get(15)?,
        row.get(16)?,
        row.get(17)?,
        row.get(18)?,
    ))
}

fn parse_render_attempt(row: RenderAttemptRow) -> Result<AiEffectCaptionRenderAttempt> {
    let (
        id,
        project_id,
        candidate_id,
        cache_key,
        composition_version,
        composition_fingerprint,
        font_version,
        browser_version,
        renderer_version,
        engine_script_version,
        width,
        height,
        fps,
        frame_count,
        frames_dir,
        status,
        failure_code,
        created_at,
        updated_at,
    ) = row;
    Ok(AiEffectCaptionRenderAttempt {
        id,
        project_id,
        candidate_id,
        cache_key,
        composition_version,
        composition_fingerprint,
        font_version,
        browser_version,
        renderer_version,
        engine_script_version,
        width: u32::try_from(width)
            .map_err(|_| AiRepositoryError::Integrity("渲染宽度无效".to_owned()))?,
        height: u32::try_from(height)
            .map_err(|_| AiRepositoryError::Integrity("渲染高度无效".to_owned()))?,
        fps: u32::try_from(fps)
            .map_err(|_| AiRepositoryError::Integrity("渲染帧率无效".to_owned()))?,
        frame_count: u64::try_from(frame_count)
            .map_err(|_| AiRepositoryError::Integrity("帧数无效".to_owned()))?,
        frames_dir,
        status: AiRenderAttemptStatus::parse(&status)?,
        failure_code,
        updated_at,
        created_at,
    })
}

const GENERATED_ANIMATION_COLUMNS: &str = "id, project_id, candidate_id, highlight_run_id, generation, reuse_key, composition, status, replace_start_ms, replace_end_ms, left_segment_id, right_segment_id, boundary_id, upstream_task_id, sha256, width, height, fps, duration_ms, rejection_code, created_at, updated_at";

type GeneratedAnimationRow = (
    i64,
    i64,
    i64,
    Option<i64>,
    i64,
    String,
    String,
    String,
    Option<i64>,
    Option<i64>,
    Option<i64>,
    Option<i64>,
    Option<i64>,
    Option<String>,
    Option<String>,
    Option<i64>,
    Option<i64>,
    Option<i64>,
    Option<i64>,
    Option<String>,
    String,
    String,
);

#[allow(clippy::type_complexity)]
fn map_generated_animation(row: &rusqlite::Row<'_>) -> rusqlite::Result<GeneratedAnimationRow> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
        row.get(8)?,
        row.get(9)?,
        row.get(10)?,
        row.get(11)?,
        row.get(12)?,
        row.get(13)?,
        row.get(14)?,
        row.get(15)?,
        row.get(16)?,
        row.get(17)?,
        row.get(18)?,
        row.get(19)?,
        row.get(20)?,
        row.get(21)?,
    ))
}

fn parse_generated_animation(row: GeneratedAnimationRow) -> Result<AiGeneratedAnimationAsset> {
    let (
        id,
        project_id,
        candidate_id,
        highlight_run_id,
        generation,
        reuse_key,
        composition,
        status,
        replace_start_ms,
        replace_end_ms,
        left_segment_id,
        right_segment_id,
        boundary_id,
        upstream_task_id,
        sha256,
        width,
        height,
        fps,
        duration_ms,
        rejection_code,
        created_at,
        updated_at,
    ) = row;
    let composition = AiGeneratedAnimationComposition::parse(&composition)?;
    let status = AiGeneratedAnimationStatus::parse(&status)?;
    let replace_range = match (replace_start_ms, replace_end_ms) {
        (Some(start), Some(end)) => Some(AiGeneratedAnimationReplaceRange {
            start_ms: u64::try_from(start).map_err(|_| {
                AiRepositoryError::Integrity("生成动画替换起点无效".to_owned())
            })?,
            end_ms: u64::try_from(end)
                .map_err(|_| AiRepositoryError::Integrity("生成动画替换终点无效".to_owned()))?,
        }),
        _ => None,
    };
    let insert_binding = match (left_segment_id, right_segment_id, boundary_id) {
        (Some(left), Some(right), Some(boundary)) => Some(AiGeneratedAnimationInsertBinding {
            left_segment_id: left,
            right_segment_id: right,
            boundary_id: boundary,
        }),
        _ => None,
    };
    let to_u32 = |value: Option<i64>, field: &'static str| -> Result<Option<u32>> {
        value
            .map(|value| {
                u32::try_from(value)
                    .map_err(|_| AiRepositoryError::Integrity(format!("生成动画{field}无效")))
            })
            .transpose()
    };
    Ok(AiGeneratedAnimationAsset {
        id,
        project_id,
        candidate_id,
        highlight_run_id,
        generation,
        reuse_key,
        composition,
        status,
        replace_range,
        insert_binding,
        upstream_task_id,
        sha256,
        width: to_u32(width, "宽度")?,
        height: to_u32(height, "高度")?,
        fps: to_u32(fps, "帧率")?,
        duration_ms: duration_ms
            .map(|value| {
                u64::try_from(value)
                    .map_err(|_| AiRepositoryError::Integrity("生成动画时长无效".to_owned()))
            })
            .transpose()?,
        rejection_code,
        created_at,
        updated_at,
    })
}

const ENHANCEMENT_COLUMNS: &str = "id, project_id, candidate_id, highlight_run_id, generation, kind, status, start_ms, end_ms, emphasis_words_json, composition_version, composition_fingerprint, render_cache_key, source_fingerprint, suppression_ranges_json, rejection_code, rejection_detail, cost_minor, created_at, updated_at";

type EnhancementRow = (
    i64,
    i64,
    i64,
    Option<i64>,
    i64,
    String,
    String,
    i64,
    i64,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    String,
    String,
    Option<String>,
    Option<String>,
    i64,
    String,
    String,
);

fn map_enhancement(row: &rusqlite::Row<'_>) -> rusqlite::Result<EnhancementRow> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
        row.get(8)?,
        row.get(9)?,
        row.get(10)?,
        row.get(11)?,
        row.get(12)?,
        row.get(13)?,
        row.get(14)?,
        row.get(15)?,
        row.get(16)?,
        row.get(17)?,
        row.get(18)?,
        row.get(19)?,
    ))
}

fn parse_enhancement(row: EnhancementRow) -> Result<AiHighlightEnhancement> {
    let (
        id,
        project_id,
        candidate_id,
        highlight_run_id,
        generation,
        kind,
        status,
        start_ms,
        end_ms,
        emphasis_words_json,
        composition_version,
        composition_fingerprint,
        render_cache_key,
        source_fingerprint,
        suppression_ranges_json,
        rejection_code,
        rejection_detail,
        cost_minor,
        created_at,
        updated_at,
    ) = row;
    Ok(AiHighlightEnhancement {
        id,
        project_id,
        candidate_id,
        highlight_run_id,
        generation,
        kind: AiEnhancementKind::parse(&kind)?,
        status: AiEnhancementStatus::parse(&status)?,
        start_ms: u64::try_from(start_ms)
            .map_err(|_| AiRepositoryError::Integrity("增强起点无效".to_owned()))?,
        end_ms: u64::try_from(end_ms)
            .map_err(|_| AiRepositoryError::Integrity("增强终点无效".to_owned()))?,
        emphasis_words: serde_json::from_str(&emphasis_words_json)
            .map_err(|_| AiRepositoryError::Integrity("强调词快照损坏".to_owned()))?,
        composition_version,
        composition_fingerprint,
        render_cache_key,
        source_fingerprint,
        suppression_ranges: serde_json::from_str(&suppression_ranges_json)
            .map_err(|_| AiRepositoryError::Integrity("抑制区间快照损坏".to_owned()))?,
        rejection_code,
        rejection_detail,
        cost_minor: u64::try_from(cost_minor)
            .map_err(|_| AiRepositoryError::Integrity("增强成本无效".to_owned()))?,
        created_at,
        updated_at,
    })
}

/// v25 为 ASR 产物记录实际 provider 和脱敏回退原因。
pub(crate) fn migrate_ai_v25(connection: &mut Connection) -> crate::database::Result<()> {
    let applied = connection
        .query_row(
            "SELECT 1 FROM schema_migrations WHERE version = 25",
            [],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if applied {
        return Ok(());
    }
    let transaction = connection.transaction()?;
    let has_provider: bool = transaction
        .query_row(
            "SELECT 1 FROM pragma_table_info('asr_artifacts') WHERE name = 'provider'",
            [],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if !has_provider {
        transaction.execute(
            "ALTER TABLE asr_artifacts ADD COLUMN provider TEXT NOT NULL DEFAULT 'local'",
            [],
        )?;
    }
    let has_fallback: bool = transaction
        .query_row(
            "SELECT 1 FROM pragma_table_info('asr_artifacts') WHERE name = 'fallback_reason'",
            [],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if !has_fallback {
        transaction.execute(
            "ALTER TABLE asr_artifacts ADD COLUMN fallback_reason TEXT",
            [],
        )?;
    }
    transaction.execute(
        "INSERT INTO schema_migrations(version, applied_at) VALUES(25, ?1)",
        [Utc::now().to_rfc3339()],
    )?;
    transaction.commit()?;
    Ok(())
}

/// v24 增加已结束直播回放智能任务，同时保持既有 live 行为和索引语义。
pub(crate) fn migrate_ai_v24(connection: &mut Connection) -> crate::database::Result<()> {
    let applied = connection
        .query_row(
            "SELECT 1 FROM schema_migrations WHERE version = 24",
            [],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if applied {
        return Ok(());
    }

    connection.pragma_update(None, "foreign_keys", "OFF")?;
    let migration = (|| -> crate::database::Result<()> {
        let transaction = connection.transaction()?;
        transaction.execute_batch(
            r#"
            CREATE TABLE ai_smart_workflows_v24 (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL CHECK(length(trim(name)) > 0),
                mode TEXT NOT NULL CHECK(mode IN ('local', 'replay', 'live')),
                status TEXT NOT NULL CHECK(status IN (
                    'draft', 'queued', 'running', 'awaiting_selection', 'review_ready',
                    'paused', 'completed', 'cancelled', 'failed'
                )),
                stage TEXT NOT NULL CHECK(stage IN (
                    'preflight', 'asr', 'highlight', 'draft', 'correction', 'transition',
                    'enhancement', 'review'
                )),
                generation INTEGER NOT NULL DEFAULT 1 CHECK(generation > 0),
                source_session_id INTEGER REFERENCES recording_sessions(id) ON DELETE RESTRICT,
                source_summary TEXT NOT NULL,
                provider TEXT NOT NULL,
                model_id TEXT NOT NULL,
                text_scope TEXT NOT NULL,
                authorization_digest TEXT,
                authorized_at TEXT,
                configuration_fingerprint TEXT NOT NULL,
                active_draft_generation INTEGER CHECK(active_draft_generation IS NULL OR active_draft_generation > 0),
                live_cursor_video_id INTEGER REFERENCES videos(id) ON DELETE SET NULL,
                event_sequence INTEGER NOT NULL DEFAULT 0 CHECK(event_sequence >= 0),
                candidate_count INTEGER NOT NULL DEFAULT 0 CHECK(candidate_count >= 0),
                selected_count INTEGER NOT NULL DEFAULT 0 CHECK(selected_count >= 0),
                pending_batch_count INTEGER NOT NULL DEFAULT 0 CHECK(pending_batch_count >= 0),
                last_error_code TEXT,
                last_error_message TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                live_start_video_id INTEGER REFERENCES videos(id) ON DELETE SET NULL,
                CHECK(
                    (mode IN ('live', 'replay') AND source_session_id IS NOT NULL)
                    OR (mode = 'local' AND source_session_id IS NULL)
                ),
                CHECK((authorization_digest IS NULL) = (authorized_at IS NULL))
            );

            INSERT INTO ai_smart_workflows_v24(
                id, name, mode, status, stage, generation, source_session_id,
                source_summary, provider, model_id, text_scope, authorization_digest,
                authorized_at, configuration_fingerprint, active_draft_generation,
                live_cursor_video_id, event_sequence, candidate_count, selected_count,
                pending_batch_count, last_error_code, last_error_message, created_at,
                updated_at, live_start_video_id
            )
            SELECT id, name, mode, status, stage, generation, source_session_id,
                   source_summary, provider, model_id, text_scope, authorization_digest,
                   authorized_at, configuration_fingerprint, active_draft_generation,
                   live_cursor_video_id, event_sequence, candidate_count, selected_count,
                   pending_batch_count, last_error_code, last_error_message, created_at,
                   updated_at, live_start_video_id
            FROM ai_smart_workflows;

            DROP TABLE ai_smart_workflows;
            ALTER TABLE ai_smart_workflows_v24 RENAME TO ai_smart_workflows;
            CREATE INDEX idx_ai_smart_workflows_status_updated
                ON ai_smart_workflows(status, updated_at DESC, id DESC);
            CREATE UNIQUE INDEX idx_ai_smart_workflows_active_live_session
                ON ai_smart_workflows(source_session_id)
                WHERE mode = 'live' AND status NOT IN ('completed', 'cancelled');
            INSERT INTO schema_migrations(version, applied_at)
                VALUES(24, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'));
            "#,
        )?;
        transaction.commit()?;
        Ok(())
    })();
    let restore_foreign_keys = connection.pragma_update(None, "foreign_keys", "ON");
    migration?;
    restore_foreign_keys?;
    let violations: i64 =
        connection.query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
            row.get(0)
        })?;
    if violations != 0 {
        return Err(DatabaseError::MigrationIntegrity(format!(
            "AI v24 迁移发现 {violations} 条外键异常"
        )));
    }
    Ok(())
}

type ProjectRow = (
    i64,
    String,
    String,
    String,
    String,
    bool,
    i64,
    Option<String>,
    Option<String>,
    String,
    Option<String>,
    Option<String>,
    String,
    String,
);

fn project_select(suffix: &str) -> String {
    format!(
        r#"
        SELECT id, name, status, recognition_profile_json, recognition_profile_hash,
               input_frozen, progress_percent, last_error_code, last_error_message,
               project_tags_json, analysis_goal, deleting_at,
               created_at, updated_at
        FROM ai_projects {suffix}
        "#
    )
}

fn map_project(row: &rusqlite::Row<'_>) -> rusqlite::Result<ProjectRow> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
        row.get(8)?,
        row.get(9)?,
        row.get(10)?,
        row.get(11)?,
        row.get(12)?,
        row.get(13)?,
    ))
}

fn parse_project(row: ProjectRow) -> Result<AiProject> {
    Ok(AiProject {
        id: row.0,
        name: row.1,
        status: AiProjectStatus::parse(&row.2)?,
        recognition_profile: serde_json::from_str(&row.3)
            .map_err(|_| AiRepositoryError::Integrity("识别配置数据损坏".to_owned()))?,
        recognition_profile_hash: row.4,
        input_frozen: row.5,
        progress_percent: u8::try_from(row.6)
            .map_err(|_| AiRepositoryError::Integrity("项目进度无效".to_owned()))?,
        last_error_code: row.7,
        last_error_message: row.8,
        project_tags: serde_json::from_str(&row.9)
            .map_err(|_| AiRepositoryError::Integrity("项目标签数据损坏".to_owned()))?,
        analysis_goal: row.10,
        deleting_at: row.11,
        created_at: row.12,
        updated_at: row.13,
    })
}

type InputRow = (
    i64,
    i64,
    i64,
    String,
    Option<i64>,
    String,
    String,
    String,
    String,
    Option<i64>,
    Option<bool>,
    Option<i64>,
    String,
    Option<i64>,
    Option<String>,
    Option<String>,
    i64,
    i64,
    Option<i64>,
);

fn input_select(suffix: &str) -> String {
    format!(
        r#"
        SELECT id, project_id, position, source_kind, video_id, display_name,
               source_path, source_fingerprint_json, source_fingerprint_hash,
               duration_ms, audio_present, project_offset_ms, status, artifact_id,
               last_error_code, last_error_message, scheduler_generation,
               queue_priority, queue_sequence
        FROM ai_project_inputs {suffix}
        "#
    )
}

fn map_input(row: &rusqlite::Row<'_>) -> rusqlite::Result<InputRow> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
        row.get(8)?,
        row.get(9)?,
        row.get(10)?,
        row.get(11)?,
        row.get(12)?,
        row.get(13)?,
        row.get(14)?,
        row.get(15)?,
        row.get(16)?,
        row.get(17)?,
        row.get(18)?,
    ))
}

fn parse_input(row: InputRow) -> Result<AiProjectInput> {
    Ok(AiProjectInput {
        id: row.0,
        project_id: row.1,
        position: row.2,
        source_kind: AiInputSourceKind::parse(&row.3)?,
        video_id: row.4,
        display_name: row.5,
        source_path: row.6,
        source_fingerprint: serde_json::from_str(&row.7)
            .map_err(|_| AiRepositoryError::Integrity("源指纹数据损坏".to_owned()))?,
        source_fingerprint_hash: row.8,
        duration_ms: row.9.map(|value| value as u64),
        audio_present: row.10,
        project_offset_ms: row.11.map(|value| value as u64),
        status: AiInputStatus::parse(&row.12)?,
        progress_percent: AiInputStatus::parse(&row.12)?.progress_percent(),
        artifact_id: row.13,
        last_error_code: row.14,
        last_error_message: row.15,
        scheduler_generation: u64::try_from(row.16)
            .map_err(|_| AiRepositoryError::Integrity("调度代次无效".to_owned()))?,
        queue_priority: u32::try_from(row.17)
            .map_err(|_| AiRepositoryError::Integrity("队列优先级无效".to_owned()))?,
        queue_sequence: row.18.map(|value| value as u64),
    })
}

type ArtifactRow = (
    i64,
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    Option<String>,
    String,
    Option<i64>,
    Option<String>,
    String,
    Option<String>,
    String,
);

type SegmentRow = (String, i64, i64, i64, i64, String, String, Option<f32>);

fn artifact_select(suffix: &str) -> String {
    format!(
        r#"
        SELECT id, source_fingerprint_json, source_fingerprint_hash,
               recognition_profile_hash, engine_id, engine_version, model_id,
               model_version, provider, fallback_reason, status, duration_ms,
               language, created_at, published_at, last_used_at
        FROM asr_artifacts {suffix}
        "#
    )
}

fn map_artifact(row: &rusqlite::Row<'_>) -> rusqlite::Result<ArtifactRow> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
        row.get(8)?,
        row.get(9)?,
        row.get(10)?,
        row.get(11)?,
        row.get(12)?,
        row.get(13)?,
        row.get(14)?,
        row.get(15)?,
    ))
}

fn parse_artifact(row: ArtifactRow) -> Result<AsrArtifact> {
    Ok(AsrArtifact {
        id: row.0,
        source_fingerprint: serde_json::from_str(&row.1)
            .map_err(|_| AiRepositoryError::Integrity("产物源指纹损坏".to_owned()))?,
        source_fingerprint_hash: row.2,
        recognition_profile_hash: row.3,
        engine_id: row.4,
        engine_version: row.5,
        model_id: row.6,
        model_version: row.7,
        provider: row.8,
        fallback_reason: row.9,
        status: AiArtifactStatus::parse(&row.10)?,
        duration_ms: row.11.map(|value| value as u64),
        language: row.12,
        created_at: row.13,
        published_at: row.14,
        last_used_at: row.15,
    })
}

fn map_segment(row: &rusqlite::Row<'_>) -> rusqlite::Result<SegmentRow> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
    ))
}

fn parse_segment(row: SegmentRow) -> Result<TranscriptSegment> {
    Ok(TranscriptSegment {
        id: row.0,
        artifact_id: row.1,
        ordinal: row.2,
        source_start_ms: u64::try_from(row.3)
            .map_err(|_| AiRepositoryError::Integrity("句段开始时间无效".to_owned()))?,
        source_end_ms: u64::try_from(row.4)
            .map_err(|_| AiRepositoryError::Integrity("句段结束时间无效".to_owned()))?,
        raw_text: row.5,
        normalized_text: row.6,
        confidence: row.7,
    })
}

fn stable_segment_id(artifact_id: i64, ordinal: usize, draft: &TranscriptSegmentDraft) -> String {
    let mut hasher = Sha256::new();
    hasher.update(artifact_id.to_le_bytes());
    hasher.update((ordinal as u64).to_le_bytes());
    hasher.update(draft.source_start_ms.to_le_bytes());
    hasher.update(draft.source_end_ms.to_le_bytes());
    hasher.update(draft.raw_text.as_bytes());
    format!("seg_{}", &hex::encode(hasher.finalize())[..32])
}

type HighlightRunRow = (
    i64,
    i64,
    String,
    String,
    String,
    String,
    String,
    Option<String>,
    String,
    i64,
    i64,
    bool,
    i64,
    i64,
    i64,
    i64,
    Option<String>,
    Option<String>,
    String,
    String,
);

fn map_highlight_run(row: &rusqlite::Row<'_>) -> rusqlite::Result<HighlightRunRow> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
        row.get(8)?,
        row.get(9)?,
        row.get(10)?,
        row.get(11)?,
        row.get(12)?,
        row.get(13)?,
        row.get(14)?,
        row.get(15)?,
        row.get(16)?,
        row.get(17)?,
        row.get(18)?,
        row.get(19)?,
    ))
}

fn parse_highlight_run(row: HighlightRunRow) -> Result<AiHighlightRun> {
    Ok(AiHighlightRun {
        id: row.0,
        project_id: row.1,
        status: AiHighlightRunStatus::parse(&row.2)?,
        model_id: row.3,
        prompt_version: row.4,
        tags_snapshot: serde_json::from_str(&row.5)
            .map_err(|_| AiRepositoryError::Integrity("精彩标签快照损坏".to_owned()))?,
        skills_snapshot: serde_json::from_str(&row.6)
            .map_err(|_| AiRepositoryError::Integrity("Skills 快照损坏".to_owned()))?,
        analysis_goal: row.7,
        analysis_fingerprint: row.8,
        qualified_score: u8::try_from(row.9)
            .map_err(|_| AiRepositoryError::Integrity("合格片段阈值无效".to_owned()))?,
        excellent_score: u8::try_from(row.10)
            .map_err(|_| AiRepositoryError::Integrity("优秀片段阈值无效".to_owned()))?,
        user_authorized: row.11,
        total_segments: u64::try_from(row.12)
            .map_err(|_| AiRepositoryError::Integrity("句段数无效".to_owned()))?,
        total_chars: u64::try_from(row.13)
            .map_err(|_| AiRepositoryError::Integrity("文本量无效".to_owned()))?,
        estimated_batches: u64::try_from(row.14)
            .map_err(|_| AiRepositoryError::Integrity("批次数无效".to_owned()))?,
        total_tokens: u64::try_from(row.15)
            .map_err(|_| AiRepositoryError::Integrity("token 用量无效".to_owned()))?,
        last_error_code: row.16,
        last_error_message: row.17,
        created_at: row.18,
        updated_at: row.19,
    })
}

fn map_clip_project(row: &rusqlite::Row<'_>) -> rusqlite::Result<AiClipProject> {
    let status = row.get::<_, String>(3)?;
    let parsed_status = AiClipExportStatus::parse(&status).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            3,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "无效剪辑导出状态",
            )),
        )
    })?;
    let progress = row.get::<_, i64>(4)?;
    let progress = u8::try_from(progress).map_err(|_| {
        rusqlite::Error::FromSqlConversionFailure(
            4,
            rusqlite::types::Type::Integer,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "无效导出进度",
            )),
        )
    })?;
    let output_width = optional_positive_u32(row, 8, "无效剪辑输出宽度")?;
    let output_height = optional_positive_u32(row, 9, "无效剪辑输出高度")?;
    if output_width.is_some() != output_height.is_some() {
        return Err(rusqlite::Error::FromSqlConversionFailure(
            8,
            rusqlite::types::Type::Integer,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "剪辑输出规格不完整",
            )),
        ));
    }
    let version = row.get::<_, i64>(10)?;
    let version = u32::try_from(version)
        .ok()
        .filter(|version| *version > 0)
        .ok_or_else(|| {
            rusqlite::Error::FromSqlConversionFailure(
                10,
                rusqlite::types::Type::Integer,
                Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "无效剪辑工程版本",
                )),
            )
        })?;
    let smart_workflow_id = row.get::<_, Option<i64>>(13)?;
    let draft_generation = optional_positive_u32(row, 14, "无效智能草稿代次")?;
    let ownership_value = row.get::<_, String>(15)?;
    let ownership = AiSmartDraftOwnership::parse(&ownership_value).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            15,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "无效智能草稿所有权",
            )),
        )
    })?;
    let export_frozen_version = optional_positive_u32(row, 16, "无效导出冻结版本")?;
    Ok(AiClipProject {
        id: row.get(0)?,
        highlight_run_id: row.get(1)?,
        smart_workflow_id,
        draft_generation,
        ownership,
        name: row.get(2)?,
        output_width,
        output_height,
        version,
        export_frozen_version,
        export_status: parsed_status,
        export_progress: progress,
        output_path: row.get(5)?,
        last_error_code: row.get(6)?,
        last_error_message: row.get(7)?,
        created_at: row.get(11)?,
        updated_at: row.get(12)?,
    })
}

fn optional_positive_u32(
    row: &rusqlite::Row<'_>,
    index: usize,
    message: &'static str,
) -> rusqlite::Result<Option<u32>> {
    row.get::<_, Option<i64>>(index)?.map_or(Ok(None), |value| {
        u32::try_from(value)
            .ok()
            .filter(|value| *value > 0)
            .map(Some)
            .ok_or_else(|| {
                rusqlite::Error::FromSqlConversionFailure(
                    index,
                    rusqlite::types::Type::Integer,
                    Box::new(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        message,
                    )),
                )
            })
    })
}

fn map_clip_segment(row: &rusqlite::Row<'_>) -> rusqlite::Result<AiClipSegment> {
    let effect = row.get::<_, String>(9)?;
    let parsed_effect = AiClipEffect::parse(&effect).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            9,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "无效内置效果",
            )),
        )
    })?;
    let position = row.get::<_, i64>(4)?;
    let start = row.get::<_, i64>(6)?;
    let end = row.get::<_, i64>(7)?;
    let volume = row.get::<_, i64>(8)?;
    Ok(AiClipSegment {
        id: row.get(0)?,
        clip_project_id: row.get(1)?,
        candidate_id: row.get(2)?,
        input_id: row.get(3)?,
        position: u32::try_from(position)
            .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(4, position))?,
        title: row.get(5)?,
        source_start_ms: u64::try_from(start)
            .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(6, start))?,
        source_end_ms: u64::try_from(end)
            .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(7, end))?,
        volume_percent: u16::try_from(volume)
            .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(8, volume))?,
        effect: parsed_effect,
    })
}

fn map_clip_subtitle(row: &rusqlite::Row<'_>) -> rusqlite::Result<AiClipSubtitle> {
    let source_start_ms = row.get::<_, i64>(8)?;
    let source_end_ms = row.get::<_, i64>(9)?;
    Ok(AiClipSubtitle {
        id: row.get(0)?,
        clip_project_id: row.get(1)?,
        stable_segment_id: row.get(2)?,
        clip_segment_id: row.get(3)?,
        input_id: row.get(4)?,
        original_text: row.get(5)?,
        text: row.get(6)?,
        hidden: row.get(7)?,
        source_start_ms: u64::try_from(source_start_ms)
            .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(8, source_start_ms))?,
        source_end_ms: u64::try_from(source_end_ms)
            .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(9, source_end_ms))?,
        project_start_ms: 0,
        project_end_ms: 0,
    })
}

type HighlightChunkRow = (
    i64,
    i64,
    i64,
    i64,
    String,
    String,
    String,
    i64,
    i64,
    Option<String>,
    Option<String>,
);

fn map_highlight_chunk(row: &rusqlite::Row<'_>) -> rusqlite::Result<HighlightChunkRow> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
        row.get(8)?,
        row.get(9)?,
        row.get(10)?,
    ))
}

fn parse_highlight_chunk(row: HighlightChunkRow) -> Result<AiHighlightChunk> {
    Ok(AiHighlightChunk {
        id: row.0,
        run_id: row.1,
        ordinal: row.2,
        input_id: row.3,
        segment_ids: serde_json::from_str(&row.4)
            .map_err(|_| AiRepositoryError::Integrity("分块句段快照损坏".to_owned()))?,
        context_segment_ids: serde_json::from_str(&row.5)
            .map_err(|_| AiRepositoryError::Integrity("上下文句段快照损坏".to_owned()))?,
        status: row.6,
        candidate_count: u64::try_from(row.7)
            .map_err(|_| AiRepositoryError::Integrity("候选数量无效".to_owned()))?,
        token_usage: u64::try_from(row.8)
            .map_err(|_| AiRepositoryError::Integrity("分块 token 用量无效".to_owned()))?,
        last_error_code: row.9,
        last_error_message: row.10,
    })
}

type HighlightCandidateRow = (
    i64,
    i64,
    Option<i64>,
    String,
    String,
    i64,
    String,
    i64,
    i64,
    f32,
    f32,
    f32,
    f32,
    f32,
    f32,
    f32,
    String,
    String,
    Option<i64>,
    bool,
);

fn map_highlight_candidate(row: &rusqlite::Row<'_>) -> rusqlite::Result<HighlightCandidateRow> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
        row.get(8)?,
        row.get(9)?,
        row.get(10)?,
        row.get(11)?,
        row.get(12)?,
        row.get(13)?,
        row.get(14)?,
        row.get(15)?,
        row.get(16)?,
        row.get(17)?,
        row.get(18)?,
        row.get(19)?,
    ))
}

fn parse_highlight_candidate(row: HighlightCandidateRow) -> Result<AiHighlightCandidate> {
    Ok(AiHighlightCandidate {
        id: row.0,
        run_id: row.1,
        chunk_id: row.2,
        candidate_key: row.3,
        title: row.4,
        input_id: row.5,
        segment_ids: serde_json::from_str(&row.6)
            .map_err(|_| AiRepositoryError::Integrity("候选句段数据损坏".to_owned()))?,
        start_ms: u64::try_from(row.7)
            .map_err(|_| AiRepositoryError::Integrity("候选开始时间无效".to_owned()))?,
        end_ms: u64::try_from(row.8)
            .map_err(|_| AiRepositoryError::Integrity("候选结束时间无效".to_owned()))?,
        total_score: row.9,
        hook_score: row.10,
        information_score: row.11,
        emotion_score: row.12,
        tag_relevance_score: row.13,
        completeness_score: row.14,
        shareability_score: row.15,
        reason: row.16,
        matched_tags: serde_json::from_str(&row.17)
            .map_err(|_| AiRepositoryError::Integrity("候选标签数据损坏".to_owned()))?,
        rank: row.18.map(|value| value as u32),
        selected: row.19,
    })
}

fn validate_candidate_draft(
    segment_ids: &[String],
    start_ms: u64,
    end_ms: u64,
    key: &str,
) -> Result<()> {
    if key.trim().is_empty()
        || segment_ids.is_empty()
        || end_ms <= start_ms
        || end_ms - start_ms < 15_000
        || end_ms - start_ms > 90_000
    {
        return Err(AiRepositoryError::Integrity(
            "精彩候选必须引用句段且时长在 15 到 90 秒之间".to_owned(),
        ));
    }
    Ok(())
}

fn validate_score(score: &HighlightCandidateScore) -> Result<()> {
    let values = [
        score.total_score,
        score.hook_score,
        score.information_score,
        score.emotion_score,
        score.tag_relevance_score,
        score.completeness_score,
        score.shareability_score,
    ];
    if values
        .iter()
        .any(|value| !value.is_finite() || !(0.0..=100.0).contains(value))
        || score.rank == 0
    {
        return Err(AiRepositoryError::Integrity(
            "精彩评分必须在 0 到 100 之间且排名有效".to_owned(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod remote_job_idempotency_tests {
    use super::*;
    use crate::database::Database;

    fn repository() -> AiRepository {
        let database = Database::open_in_memory().expect("in-memory database");
        database.migrate().expect("migrate");
        AiRepository::new(database)
    }

    fn key(request_id: &str, input_hash: &str) -> RemoteJobKey {
        RemoteJobKey {
            request_id: request_id.to_owned(),
            task_type: "asr".to_owned(),
            input_hash: input_hash.to_owned(),
        }
    }

    /// 任务 9.1 + 9.2：重启恢复必须能按 requestId 找回任务，且不产生重复付费提交。
    #[test]
    fn restart_recovery_reports_jobs_needing_upstream_check() {
        let repository = repository();
        // 已拿到上游任务标识：不需要对账。
        repository
            .record_remote_job(&key("req-a", "hash-a"), Some("task-a"))
            .expect("record a");
        // 已提交但未拿到任务标识：必须进入待对账。
        repository
            .record_remote_job(&key("req-b", "hash-b"), None)
            .expect("record b");

        let summary = repository.recover_interrupted().expect("recover");
        assert_eq!(
            summary.awaiting_remote_reconcile, 1,
            "只有缺少上游任务标识的记录需要核对"
        );
        let a = repository
            .remote_job_record("req-a", "asr")
            .expect("lookup a")
            .expect("present a");
        assert_eq!(a.upstream_task_id.as_deref(), Some("task-a"));
        let b = repository
            .remote_job_record("req-b", "asr")
            .expect("lookup b")
            .expect("present b");
        assert!(b.reconcile_required);
        let pending = repository.pending_remote_jobs().expect("pending");
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].request_id, "req-b");
        // 再次恢复不改变结论（幂等）。
        let again = repository.recover_interrupted().expect("recover twice");
        assert_eq!(again.awaiting_remote_reconcile, 1);
    }

    /// 任务 9.3：未完整发布的识别片段不得被标记为可复用。
    #[test]
    fn recovery_never_marks_unpublished_artifacts_reusable() {
        let repository = repository();
        let summary = repository.recover_interrupted().expect("recover");
        assert_eq!(summary.artifacts, 0);
        let connection = repository.database.connection().expect("connection");
        let reusable: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM asr_artifacts WHERE status = 'published'",
                [],
                |row| row.get(0),
            )
            .expect("count published");
        assert_eq!(reusable, 0, "恢复不得把未发布产物当作可复用");
    }

    /// 任务 7.2：在升级前的旧库快照上执行迁移，旧产物的身份与缓存命中行为必须不变。
    #[test]
    fn migration_v26_preserves_legacy_artifact_identity() {
        let database = Database::open_in_memory().expect("in-memory database");
        database.migrate().expect("migrate to latest");
        let repository = AiRepository::new(database.clone());
        // 构造一行"升级前"的产物：只有 v25 及以前存在的列，身份为本地引擎。
        // 源指纹使用真实指纹，以便稍后验证缓存命中行为未被迁移改变。
        let source = SourceFingerprint {
            normalized_path: "/tmp/legacy.mkv".into(),
            size_bytes: 1,
            modified_at_ms: 1,
            video_id: None,
        };
        let source_json = serde_json::to_string(&source).expect("serialize source");
        let source_hash = source.fingerprint().expect("source fingerprint");
        {
            let connection = database.connection().expect("connection");
            connection
                .execute(
                    r#"INSERT INTO asr_artifacts(
                           source_video_id, source_fingerprint_json, source_fingerprint_hash,
                           recognition_profile_hash, engine_id, engine_version, model_id,
                           model_version, status, duration_ms, language, created_at,
                           published_at, last_used_at, provider, fallback_reason
                       ) VALUES(NULL, ?1, ?2, 'profile-hash', 'whisper.cpp', 'legacy',
                               'small', 'legacy', 'published', 60000, 'zh', '2026-01-01T00:00:00Z',
                               '2026-01-01T00:00:01Z', '2026-01-01T00:00:01Z', 'local', 'old_reason')"#,
                    params![source_json, source_hash],
                )
                .expect("insert legacy artifact");
        }
        // 再次执行迁移（幂等），并确认新增列不参与旧行重写。
        database.migrate().expect("migrate again");
        let connection = database.connection().expect("connection");
        let (provider, engine_id, model_id, fallback): (String, String, String, Option<String>) =
            connection
                .query_row(
                    "SELECT provider, engine_id, model_id, fallback_reason FROM asr_artifacts WHERE source_fingerprint_hash = ?1",
                    params![source_hash],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )
                .expect("legacy row must survive");
        assert_eq!(provider, "local", "旧产物 provider 不得被迁移改写");
        assert_eq!(engine_id, "whisper.cpp", "旧产物引擎身份不得被改写");
        assert_eq!(model_id, "small");
        assert_eq!(fallback.as_deref(), Some("old_reason"));
        // 新增列对旧行必须为空（只增列、不填值）。
        let (chunk, normalization): (Option<String>, Option<String>) = connection
            .query_row(
                "SELECT chunk_strategy, normalization_version FROM asr_artifacts WHERE source_fingerprint_hash = ?1",
                params![source_hash],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("columns exist");
        assert!(
            chunk.is_none() && normalization.is_none(),
            "旧行不得被回填新列"
        );
        drop(connection);
        // 缓存命中行为不变：同一源指纹与识别配置指纹仍命中该已发布产物。
        let hit = repository
            .find_published_artifact(&source, "profile-hash")
            .expect("lookup must not fail")
            .expect("升级后旧产物仍必须可被缓存命中");
        assert_eq!(hit.provider, "local");
        assert_eq!(hit.engine_id, "whisper.cpp");
        assert_eq!(hit.model_id, "small");
        // 识别配置指纹不同则不得命中（旧行为不变）。
        assert!(
            repository
                .find_published_artifact(&source, "other-profile")
                .expect("lookup")
                .is_none(),
            "识别配置指纹不同时不得复用旧产物"
        );
    }

    /// 任务 2.3 + 2.6：提交前落盘；同键同输入复用，同键不同输入拒绝。
    #[test]
    fn same_key_same_input_reuses_and_different_input_is_rejected() {
        let repository = repository();
        let first = repository
            .record_remote_job(&key("req-1", "hash-a"), Some("task-1"))
            .expect("first record");
        assert_eq!(first.status, "submitted");
        assert_eq!(first.upstream_task_id.as_deref(), Some("task-1"));
        assert!(!first.reconcile_required);

        // 同键同输入：复用同一记录，不新建、不换上游任务。
        let again = repository
            .record_remote_job(&key("req-1", "hash-a"), Some("task-1"))
            .expect("reuse");
        assert_eq!(again.status, "reused");
        assert_eq!(again.upstream_task_id.as_deref(), Some("task-1"));

        // 同键不同输入：必须拒绝。
        let conflict = repository
            .record_remote_job(&key("req-1", "hash-b"), Some("task-2"))
            .expect_err("conflict");
        assert!(matches!(conflict, AiRepositoryError::IdempotencyConflict));
        // 冲突后原记录不得被覆盖。
        let unchanged = repository
            .remote_job_record("req-1", "asr")
            .expect("lookup")
            .expect("present");
        assert_eq!(unchanged.input_hash, "hash-a");
        assert_eq!(unchanged.upstream_task_id.as_deref(), Some("task-1"));

        // 不同键各自独立。
        let other = repository
            .record_remote_job(&key("req-2", "hash-b"), Some("task-9"))
            .expect("other key");
        assert_eq!(other.status, "submitted");
    }

    /// 任务 9.4：已提交但缺少上游任务标识必须进入待对账，不得自动重新提交。
    #[test]
    fn submitted_without_task_id_awaits_reconcile() {
        let repository = repository();
        let record = repository
            .record_remote_job(&key("req-3", "hash-c"), None)
            .expect("record without task id");
        assert_eq!(record.status, "awaiting_reconcile");
        assert!(record.reconcile_required);
        let pending = repository.records_awaiting_reconcile().expect("list");
        assert!(
            pending.iter().any(|item| item.request_id == "req-3"),
            "待对账记录必须可查询: {pending:?}"
        );
        // 后续取到上游任务标识时可补齐，且不再需要人工对账标记的账号级语义不变。
        let filled = repository
            .record_remote_job(&key("req-3", "hash-c"), Some("task-3"))
            .expect("fill task id");
        assert_eq!(filled.upstream_task_id.as_deref(), Some("task-3"));
        assert!(filled.reconcile_required, "历史对账标记保留以便复核");
    }

    /// 任务 2.3：进程在提交后立即退出，重启仍能按 requestId 找回原任务。
    #[test]
    fn record_survives_reopen_and_is_queryable_by_request_id() {
        let database = Database::open_in_memory().expect("in-memory database");
        database.migrate().expect("migrate");
        let repository = AiRepository::new(database.clone());
        repository
            .record_remote_job(&key("req-recover", "hash-d"), Some("task-recover"))
            .expect("record");
        // 用同一连接句柄新建仓库实例，模拟重启后重新打开数据库。
        let reopened = AiRepository::new(database);
        let found = reopened
            .remote_job_record("req-recover", "asr")
            .expect("lookup")
            .expect("present after reopen");
        assert_eq!(found.upstream_task_id.as_deref(), Some("task-recover"));
        assert_eq!(found.input_hash, "hash-d");
    }
}

#[cfg(test)]
mod recognition_identity_fingerprint_tests {
    use super::*;

    /// 升级前的识别配置 JSON（没有 provider / chunkStrategy 字段）。
    const LEGACY_PROFILE_JSON: &str = r#"{
        "engineId": "dashscope-filetrans",
        "engineVersion": "1",
        "modelId": "qwen3-asr-flash-filetrans",
        "modelVersion": "qwen3-asr-flash-filetrans",
        "languageHint": "zh",
        "vadModelId": "ggml-vad@1",
        "vadThresholdMillis": 120,
        "vadPaddingMs": 300,
        "timestampPolicy": "segment",
        "normalizationVersion": "OpenCC ver.1.4.1",
        "hotwords": ["品牌名"]
    }"#;

    fn profile(provider: Option<&str>, chunk: Option<&str>) -> RecognitionProfile {
        let mut value: RecognitionProfile = serde_json::from_str(LEGACY_PROFILE_JSON).unwrap();
        value.provider = provider.map(str::to_owned);
        value.chunk_strategy = chunk.map(str::to_owned);
        value
    }

    /// 任务 7.2：升级不得改变既有工程的缓存指纹，否则所有旧工程会被误判为需重识别。
    #[test]
    fn legacy_profile_fingerprint_is_unchanged_by_added_fields() {
        let legacy: RecognitionProfile = serde_json::from_str(LEGACY_PROFILE_JSON).unwrap();
        assert!(legacy.provider.is_none());
        assert!(legacy.chunk_strategy.is_none());
        // 反序列化后必须序列化回完全相同的 JSON（不新增键）。
        let round_trip = serde_json::to_string(&legacy).unwrap();
        assert!(
            !round_trip.contains("provider") && !round_trip.contains("chunkStrategy"),
            "旧配置的新增字段不得进入序列化结果: {round_trip}"
        );
        // 与"显式给出 None"的配置指纹相同。
        assert_eq!(
            legacy.fingerprint().unwrap(),
            profile(None, None).fingerprint().unwrap()
        );
        // 指纹稳定：同一输入多次计算结果一致。
        assert_eq!(legacy.fingerprint().unwrap(), legacy.fingerprint().unwrap());
    }

    /// 任务 7.3：provider 与分块策略任一变化都必须产生新指纹（即不复用旧产物）。
    #[test]
    fn provider_and_chunk_strategy_participate_in_fingerprint() {
        let base = profile(None, None).fingerprint().unwrap();
        let cloud = profile(Some("cloud"), None).fingerprint().unwrap();
        let local = profile(Some("local"), None).fingerprint().unwrap();
        let chunked = profile(Some("cloud"), Some("fixed-overlap-120s-2s"))
            .fingerprint()
            .unwrap();

        assert_ne!(base, cloud, "provider 必须参与指纹");
        assert_ne!(cloud, local, "云端与本地必须是不同指纹");
        assert_ne!(cloud, chunked, "分块策略必须参与指纹");
        // 任务 7.4：两条路径不得互相复用产物。
        assert_ne!(local, chunked);
        // 热词顺序不影响指纹（既有语义不得回归）。
        let mut first = profile(Some("cloud"), None);
        first.hotwords = vec!["b".into(), "a".into()];
        let mut second = profile(Some("cloud"), None);
        second.hotwords = vec!["a".into(), "b".into()];
        assert_eq!(
            first.fingerprint().unwrap(),
            second.fingerprint().unwrap(),
            "热词顺序不得导致无意义缓存失效"
        );
    }

    /// 识别配置身份必须与执行引擎一致，不得用本地身份冒充云端。
    #[test]
    fn cloud_and_local_profiles_carry_their_own_engine_identity() {
        let cloud = profile(Some("cloud"), None);
        let local = profile(Some("local"), None);
        assert_eq!(cloud.engine_id, "dashscope-filetrans");
        assert_ne!(cloud.provider, local.provider);
        assert_ne!(cloud.fingerprint().unwrap(), local.fingerprint().unwrap());
    }
}

#[cfg(test)]
mod smart_migration_tests {
    use super::*;

    #[test]
    fn v22_backfills_existing_clip_project_source_and_is_idempotent() {
        let mut connection = Connection::open_in_memory().unwrap();
        connection
            .pragma_update(None, "foreign_keys", "ON")
            .unwrap();
        connection
            .execute_batch(
                r#"
                CREATE TABLE schema_migrations (
                    version INTEGER PRIMARY KEY,
                    applied_at TEXT NOT NULL
                );
                CREATE TABLE streamers(id INTEGER PRIMARY KEY);
                CREATE TABLE recording_sessions(
                    id INTEGER PRIMARY KEY,
                    streamer_id INTEGER,
                    started_at TEXT,
                    ended_at TEXT,
                    status TEXT,
                    output_root TEXT
                );
                CREATE TABLE videos(id INTEGER PRIMARY KEY, session_id INTEGER);
                CREATE TABLE ai_projects(id INTEGER PRIMARY KEY, name TEXT NOT NULL);
                CREATE TABLE ai_project_inputs(
                    id INTEGER PRIMARY KEY,
                    project_id INTEGER NOT NULL REFERENCES ai_projects(id) ON DELETE CASCADE
                );
                CREATE TABLE ai_highlight_runs(
                    id INTEGER PRIMARY KEY,
                    project_id INTEGER NOT NULL REFERENCES ai_projects(id) ON DELETE CASCADE
                );
                CREATE TABLE ai_highlight_chunks(
                    id INTEGER PRIMARY KEY,
                    run_id INTEGER NOT NULL REFERENCES ai_highlight_runs(id) ON DELETE CASCADE
                );
                CREATE TABLE ai_highlight_candidates(
                    id INTEGER PRIMARY KEY,
                    run_id INTEGER NOT NULL REFERENCES ai_highlight_runs(id) ON DELETE CASCADE,
                    chunk_id INTEGER NOT NULL REFERENCES ai_highlight_chunks(id) ON DELETE CASCADE,
                    input_id INTEGER NOT NULL REFERENCES ai_project_inputs(id) ON DELETE CASCADE
                );
                CREATE TABLE ai_clip_projects (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    highlight_run_id INTEGER NOT NULL UNIQUE REFERENCES ai_highlight_runs(id) ON DELETE CASCADE,
                    name TEXT NOT NULL,
                    export_status TEXT NOT NULL DEFAULT 'idle',
                    export_progress INTEGER NOT NULL DEFAULT 0,
                    output_path TEXT,
                    last_error_code TEXT,
                    last_error_message TEXT,
                    output_width INTEGER,
                    output_height INTEGER,
                    version INTEGER NOT NULL DEFAULT 1,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL
                );
                INSERT INTO ai_projects(id, name) VALUES(1, '历史项目');
                INSERT INTO ai_project_inputs(id, project_id) VALUES(1, 1);
                INSERT INTO ai_highlight_runs(id, project_id) VALUES(7, 1);
                INSERT INTO ai_highlight_chunks(id, run_id) VALUES(8, 7);
                INSERT INTO ai_highlight_candidates(id, run_id, chunk_id, input_id) VALUES(9, 7, 8, 1);
                INSERT INTO ai_clip_projects(
                    id, highlight_run_id, name, export_status, export_progress,
                    output_width, output_height, version, created_at, updated_at
                ) VALUES(11, 7, '历史剪辑', 'idle', 0, 1920, 1080, 3, 'before', 'before');
                "#,
            )
            .unwrap();

        migrate_ai_v22(&mut connection).unwrap();
        migrate_ai_v22(&mut connection).unwrap();

        let clip = connection
            .query_row(
                r#"SELECT id, highlight_run_id, smart_workflow_id, draft_generation,
                          ownership, name, version
                   FROM ai_clip_projects WHERE id = 11"#,
                [],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, Option<i64>>(2)?,
                        row.get::<_, Option<i64>>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, i64>(6)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(
            clip,
            (
                11,
                7,
                None,
                None,
                "user".to_owned(),
                "历史剪辑".to_owned(),
                3
            )
        );
        let source = connection
            .query_row(
                r#"SELECT highlight_run_id, source_order, primary_source
                   FROM ai_clip_project_sources WHERE clip_project_id = 11"#,
                [],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(source, (7, 0, 1));
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM schema_migrations WHERE version = 22",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            1
        );
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            0
        );
    }

    #[test]
    fn v24_preserves_live_rows_and_enforces_replay_source_contract() {
        let mut connection = Connection::open_in_memory().unwrap();
        connection
            .pragma_update(None, "foreign_keys", "ON")
            .unwrap();
        connection
            .execute_batch(
                r#"
                CREATE TABLE schema_migrations (
                    version INTEGER PRIMARY KEY,
                    applied_at TEXT NOT NULL
                );
                CREATE TABLE recording_sessions(id INTEGER PRIMARY KEY);
                CREATE TABLE videos(id INTEGER PRIMARY KEY);
                INSERT INTO recording_sessions(id) VALUES(7);
                CREATE TABLE ai_smart_workflows (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    name TEXT NOT NULL,
                    mode TEXT NOT NULL CHECK(mode IN ('local', 'live')),
                    status TEXT NOT NULL,
                    stage TEXT NOT NULL,
                    generation INTEGER NOT NULL DEFAULT 1,
                    source_session_id INTEGER REFERENCES recording_sessions(id) ON DELETE RESTRICT,
                    source_summary TEXT NOT NULL,
                    provider TEXT NOT NULL,
                    model_id TEXT NOT NULL,
                    text_scope TEXT NOT NULL,
                    authorization_digest TEXT,
                    authorized_at TEXT,
                    configuration_fingerprint TEXT NOT NULL,
                    active_draft_generation INTEGER,
                    live_cursor_video_id INTEGER REFERENCES videos(id) ON DELETE SET NULL,
                    event_sequence INTEGER NOT NULL DEFAULT 0,
                    candidate_count INTEGER NOT NULL DEFAULT 0,
                    selected_count INTEGER NOT NULL DEFAULT 0,
                    pending_batch_count INTEGER NOT NULL DEFAULT 0,
                    last_error_code TEXT,
                    last_error_message TEXT,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL,
                    live_start_video_id INTEGER REFERENCES videos(id) ON DELETE SET NULL,
                    CHECK(
                        (mode = 'live' AND source_session_id IS NOT NULL)
                        OR (mode = 'local' AND source_session_id IS NULL)
                    ),
                    CHECK((authorization_digest IS NULL) = (authorized_at IS NULL))
                );
                INSERT INTO ai_smart_workflows(
                    id, name, mode, status, stage, source_session_id, source_summary,
                    provider, model_id, text_scope, configuration_fingerprint,
                    created_at, updated_at
                ) VALUES(
                    11, '历史边播边剪', 'live', 'running', 'asr', 7, '历史直播场次',
                    'deepseek', 'deepseek-chat', 'selected_clip_subtitles', 'live-config',
                    'before', 'before'
                );
                "#,
            )
            .unwrap();

        migrate_ai_v24(&mut connection).unwrap();
        migrate_ai_v24(&mut connection).unwrap();

        assert_eq!(
            connection
                .query_row(
                    "SELECT mode, source_session_id FROM ai_smart_workflows WHERE id = 11",
                    [],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
                )
                .unwrap(),
            ("live".to_owned(), 7)
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM schema_migrations WHERE version = 24",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            1
        );

        let insert = |connection: &Connection, mode: &str, session_id: Option<i64>, name: &str| {
            connection.execute(
                r#"INSERT INTO ai_smart_workflows(
                       name, mode, status, stage, source_session_id, source_summary,
                       provider, model_id, text_scope, configuration_fingerprint,
                       created_at, updated_at
                   ) VALUES(?1, ?2, 'draft', 'preflight', ?3, '受信来源',
                            'deepseek', 'deepseek-chat', 'selected_clip_subtitles',
                            'config', 'now', 'now')"#,
                params![name, mode, session_id],
            )
        };
        assert!(insert(&connection, "replay", Some(7), "历史回放").is_ok());
        assert!(insert(&connection, "replay", None, "无来源回放").is_err());
        assert!(insert(&connection, "local", Some(7), "错误本地任务").is_err());
        assert!(insert(&connection, "live", Some(7), "重复活动直播").is_err());
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            0
        );
    }

    // ---- 步骤 3 任务组 1：增强记录、迁移 27 与代次语义 ----

    const ENHANCEMENT_TEST_PROFILE_JSON: &str = r#"{
        "engineId": "dashscope-filetrans",
        "engineVersion": "1",
        "modelId": "qwen3-asr-flash-filetrans",
        "modelVersion": "qwen3-asr-flash-filetrans",
        "languageHint": "zh",
        "vadModelId": "ggml-vad@1",
        "vadThresholdMillis": 120,
        "vadPaddingMs": 300,
        "timestampPolicy": "segment",
        "normalizationVersion": "OpenCC ver.1.4.1",
        "hotwords": ["品牌名"]
    }"#;

    fn enhancement_test_profile() -> RecognitionProfile {
        serde_json::from_str(ENHANCEMENT_TEST_PROFILE_JSON).expect("测试识别配置")
    }

    use crate::ai::domain::AiSuppressionRange;

    fn enhancement(
        project_id: i64,
        candidate_id: i64,
        generation: i64,
        status: AiEnhancementStatus,
        source_fingerprint: &str,
    ) -> NewAiHighlightEnhancement {
        NewAiHighlightEnhancement {
            project_id,
            candidate_id,
            highlight_run_id: None,
            generation,
            kind: AiEnhancementKind::EffectCaption,
            status,
            start_ms: 0,
            end_ms: 4_000,
            emphasis_words: vec!["绷不住".to_owned()],
            composition_version: Some(format!("composition-v{generation}")),
            composition_fingerprint: Some(format!("composition-fp-{generation}")),
            render_cache_key: None,
            source_fingerprint: source_fingerprint.to_owned(),
            suppression_ranges: vec![AiSuppressionRange {
                start_ms: 0,
                end_ms: 4_000,
            }],
            rejection_code: None,
            rejection_detail: None,
            cost_minor: 0,
        }
    }

    fn replace_animation(
        project_id: i64,
        candidate_id: i64,
        generation: i64,
        status: AiGeneratedAnimationStatus,
        reuse_key: &str,
    ) -> NewAiGeneratedAnimationAsset {
        NewAiGeneratedAnimationAsset {
            project_id,
            candidate_id,
            highlight_run_id: None,
            generation,
            reuse_key: reuse_key.to_owned(),
            composition: AiGeneratedAnimationComposition::Replace,
            status,
            replace_range: Some(AiGeneratedAnimationReplaceRange {
                start_ms: 1_000,
                end_ms: 6_000,
            }),
            insert_binding: None,
            upstream_task_id: Some(format!("task-replace-{generation}")),
            sha256: Some("a".repeat(64)),
            width: Some(768),
            height: Some(512),
            fps: Some(30),
            duration_ms: Some(5_000),
            rejection_code: None,
        }
    }

    fn insert_animation(
        project_id: i64,
        candidate_id: i64,
        generation: i64,
        status: AiGeneratedAnimationStatus,
        reuse_key: &str,
    ) -> NewAiGeneratedAnimationAsset {
        NewAiGeneratedAnimationAsset {
            project_id,
            candidate_id,
            highlight_run_id: None,
            generation,
            reuse_key: reuse_key.to_owned(),
            composition: AiGeneratedAnimationComposition::Insert,
            status,
            replace_range: None,
            insert_binding: Some(AiGeneratedAnimationInsertBinding {
                left_segment_id: 11,
                right_segment_id: 12,
                boundary_id: 21,
            }),
            upstream_task_id: Some(format!("task-insert-{generation}")),
            sha256: Some("b".repeat(64)),
            width: Some(768),
            height: Some(512),
            fps: Some(30),
            duration_ms: Some(3_000),
            rejection_code: None,
        }
    }

    /// v32：句段级纠正层支撑"片段级前置"——校正发生在拼合辑之前。
    /// 本测试验证纠正的读写与替换语义（空文本表示撤销）。
    #[test]
    fn transcript_segment_corrections_round_trip_and_can_be_revoked() {
        let database = Database::open_in_memory().expect("in-memory database");
        database.migrate().expect("migrate to latest");
        let repository = AiRepository::new(database.clone());
        {
            let connection = database.connection().expect("connection");
            connection
                .execute_batch(
                    r#"
                    INSERT INTO asr_artifacts(id, source_fingerprint_json, source_fingerprint_hash,
                        recognition_profile_hash, engine_id, engine_version, model_id, model_version,
                        status, duration_ms, created_at, published_at, last_used_at)
                    VALUES(1, '{}', 'fp', 'profile', 'whisper.cpp', '1', 'm', 'v',
                        'published', 1000, 'now', 'now', 'now');
                    INSERT INTO transcript_segments(id, artifact_id, ordinal, source_start_ms,
                        source_end_ms, raw_text, normalized_text)
                    VALUES('seg_a', 1, 0, 0, 1000, '帐号', '帐号'),
                          ('seg_b', 1, 1, 1000, 2000, '朱教', '朱教');
                    "#,
                )
                .expect("seed transcript");
        }

        let applied = repository
            .record_transcript_segment_corrections(
                Some(7),
                &[
                    ("seg_a".to_owned(), "帐号".to_owned(), "账号".to_owned()),
                    ("seg_b".to_owned(), "朱教".to_owned(), "助教".to_owned()),
                ],
            )
            .expect("record corrections");
        assert_eq!(applied, 2);

        let read = repository
            .transcript_segment_corrections(&["seg_a".to_owned(), "seg_b".to_owned()])
            .expect("read corrections");
        assert_eq!(read.get("seg_a").map(String::as_str), Some("账号"));
        assert_eq!(read.get("seg_b").map(String::as_str), Some("助教"));

        // 空文本 = 撤销该句段的纠正。
        repository
            .record_transcript_segment_corrections(
                Some(7),
                &[("seg_a".to_owned(), "帐号".to_owned(), String::new())],
            )
            .expect("revoke");
        let read = repository
            .transcript_segment_corrections(&["seg_a".to_owned(), "seg_b".to_owned()])
            .expect("read after revoke");
        assert!(!read.contains_key("seg_a"), "撤销后不应再返回纠正");
        assert_eq!(read.get("seg_b").map(String::as_str), Some("助教"));

        // 覆盖写入：同一句段再次纠正应替换而非重复。
        repository
            .record_transcript_segment_corrections(
                None,
                &[("seg_b".to_owned(), "朱教".to_owned(), "助教老师".to_owned())],
            )
            .expect("overwrite");
        let read = repository
            .transcript_segment_corrections(&["seg_b".to_owned()])
            .expect("read after overwrite");
        assert_eq!(read.get("seg_b").map(String::as_str), Some("助教老师"));
    }

    /// 在受控超时值上重放 v31 迁移，返回迁移后的值。
    fn migrate_v31_with_timeout(timeout_ms: i64) -> i64 {
        let database = Database::open_in_memory().expect("in-memory database");
        database.migrate().expect("migrate to latest");
        let mut connection = database.connection().expect("connection");
        connection
            .execute("DELETE FROM schema_migrations WHERE version = 31", [])
            .expect("reset v31");
        connection
            .execute("DELETE FROM llm_provider_settings", [])
            .expect("clear settings");
        connection
            .execute(
                "INSERT INTO llm_provider_settings(provider, model_id, timeout_ms, prompt_version, updated_at) \
                 VALUES('deepseek', 'qwen-plus', ?1, 'highlight-v1', '2026-01-01T00:00:00Z')",
                [timeout_ms],
            )
            .expect("insert settings");
        super::migrate_ai_v31(&mut connection).expect("run v31");
        connection
            .query_row("SELECT timeout_ms FROM llm_provider_settings", [], |row| {
                row.get(0)
            })
            .expect("read timeout")
    }

    /// v31：旧默认 30 秒会被**原样持久化**进设置表（该表受 `CHECK(id = 1)` 限制只有一行），
    /// 因此只提升 `DEFAULT_LLM_TIMEOUT_MS` 常量不足以让既有用户受益——必须迁移。
    /// 同时确认**用户显式设置的值不会被覆盖**。
    #[test]
    fn migration_v31_raises_only_the_legacy_default_timeout() {
        assert_eq!(
            migrate_v31_with_timeout(30_000),
            90_000,
            "旧默认 30 秒应被提升到新默认"
        );
        assert_eq!(
            migrate_v31_with_timeout(120_000),
            120_000,
            "用户显式设置的最大值必须保留"
        );
        assert_eq!(
            migrate_v31_with_timeout(60_000),
            60_000,
            "非旧默认的值不得被改写"
        );
    }

    /// 默认值必须足以覆盖实测耗时：真实调用观察到单批 28–43 秒。
    #[test]
    fn default_llm_timeout_covers_observed_calls() {
        assert!(
            crate::ai::llm::DEFAULT_LLM_TIMEOUT_MS >= 60_000,
            "默认超时必须显著高于实测 28–43 秒，实际 {}",
            crate::ai::llm::DEFAULT_LLM_TIMEOUT_MS
        );
    }

    #[test]
    fn migration_v29_is_idempotent_and_creates_generated_animation_table() {
        let database = Database::open_in_memory().expect("in-memory database");
        database.migrate().expect("migrate to latest");
        database.migrate().expect("migrate again is idempotent");
        let connection = database.connection().expect("connection");
        let exists: bool = connection
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'ai_generated_animation_assets'",
                [],
                |_| Ok(()),
            )
            .optional()
            .expect("query")
            .is_some();
        assert!(exists, "v29 必须建出生成动画资产表");
        // 生成资产 MUST NOT 产生 renderMode。
        let has_render_mode: bool = connection
            .query_row(
                "SELECT 1 FROM pragma_table_info('ai_generated_animation_assets') WHERE name = 'render_mode'",
                [],
                |_| Ok(()),
            )
            .optional()
            .expect("query")
            .is_some();
        assert!(!has_render_mode, "生成资产 MUST NOT 产生 renderMode");
    }

    #[test]
    fn generated_animation_binding_must_match_composition() {
        let database = Database::open_in_memory().expect("in-memory database");
        database.migrate().expect("migrate to latest");
        let repository = AiRepository::new(database);
        let project = repository
            .create_project("生成动画", &enhancement_test_profile())
            .expect("create project");

        // replace 带 insert 绑定：拒绝。
        let mut wrong = replace_animation(
            project.id,
            42,
            1,
            AiGeneratedAnimationStatus::Pending,
            "rk-1",
        );
        wrong.insert_binding = Some(AiGeneratedAnimationInsertBinding {
            left_segment_id: 1,
            right_segment_id: 2,
            boundary_id: 3,
        });
        assert!(matches!(
            repository.record_generated_animation_asset(wrong),
            Err(AiRepositoryError::Integrity(_))
        ));

        // replace 缺目标范围：拒绝。
        let mut missing =
            replace_animation(project.id, 42, 1, AiGeneratedAnimationStatus::Pending, "rk-1");
        missing.replace_range = None;
        assert!(matches!(
            repository.record_generated_animation_asset(missing),
            Err(AiRepositoryError::Integrity(_))
        ));

        // insert 右片段无效：拒绝。
        let mut no_right =
            insert_animation(project.id, 42, 1, AiGeneratedAnimationStatus::Pending, "rk-1");
        no_right.insert_binding = Some(AiGeneratedAnimationInsertBinding {
            left_segment_id: 11,
            right_segment_id: 0,
            boundary_id: 21,
        });
        assert!(matches!(
            repository.record_generated_animation_asset(no_right),
            Err(AiRepositoryError::Integrity(_))
        ));
    }

    #[test]
    fn generated_animation_round_trips_and_reuse_key_only_matches_validated() {
        let database = Database::open_in_memory().expect("in-memory database");
        database.migrate().expect("migrate to latest");
        let repository = AiRepository::new(database);
        let project = repository
            .create_project("生成动画", &enhancement_test_profile())
            .expect("create project");

        // pending 的资产不参与复用。
        let pending = repository
            .record_generated_animation_asset(replace_animation(
                project.id,
                42,
                1,
                AiGeneratedAnimationStatus::Pending,
                "reuse-1",
            ))
            .expect("record pending");
        assert!(
            repository
                .find_validated_generated_animation(project.id, "reuse-1")
                .expect("query")
                .is_none()
        );

        // 校验通过后可复用，且媒体属性被完整读回。
        let validated = repository
            .record_generated_animation_asset(replace_animation(
                project.id,
                42,
                1,
                AiGeneratedAnimationStatus::Validated,
                "reuse-1",
            ))
            .expect("record validated");
        assert_eq!(
            validated.id, pending.id,
            "同一 candidate+generation 覆盖而非新增"
        );
        assert_eq!(validated.status, AiGeneratedAnimationStatus::Validated);
        assert_eq!(validated.width, Some(768));
        assert_eq!(validated.height, Some(512));
        assert_eq!(validated.fps, Some(30));
        assert_eq!(validated.duration_ms, Some(5_000));
        assert_eq!(
            validated.replace_range,
            Some(AiGeneratedAnimationReplaceRange {
                start_ms: 1_000,
                end_ms: 6_000,
            })
        );
        assert!(validated.insert_binding.is_none());
        let reused = repository
            .find_validated_generated_animation(project.id, "reuse-1")
            .expect("query")
            .expect("validated 资产必须可复用");
        assert_eq!(reused.id, pending.id);

        // insert 同样可读回，绑定三元组完整。
        let insert = repository
            .record_generated_animation_asset(insert_animation(
                project.id,
                43,
                1,
                AiGeneratedAnimationStatus::Validated,
                "reuse-2",
            ))
            .expect("record insert");
        assert_eq!(insert.composition, AiGeneratedAnimationComposition::Insert);
        assert_eq!(
            insert.insert_binding,
            Some(AiGeneratedAnimationInsertBinding {
                left_segment_id: 11,
                right_segment_id: 12,
                boundary_id: 21,
            })
        );
        assert!(insert.replace_range.is_none());
        assert_eq!(
            repository
                .list_generated_animation_assets(project.id)
                .expect("list")
                .len(),
            2
        );
    }

    #[test]
    fn validated_generated_animation_is_not_overwritten() {
        let database = Database::open_in_memory().expect("in-memory database");
        database.migrate().expect("migrate to latest");
        let repository = AiRepository::new(database);
        let project = repository
            .create_project("生成动画", &enhancement_test_profile())
            .expect("create project");
        let validated = repository
            .record_generated_animation_asset(replace_animation(
                project.id,
                42,
                1,
                AiGeneratedAnimationStatus::Validated,
                "reuse-1",
            ))
            .expect("record validated");
        // 已校验的行不被覆盖：再次写入 pending 必须保留 validated 状态与媒体属性。
        let attempted = repository
            .record_generated_animation_asset(replace_animation(
                project.id,
                42,
                1,
                AiGeneratedAnimationStatus::Pending,
                "reuse-1",
            ))
            .expect("record attempts");
        assert_eq!(attempted.id, validated.id);
        assert_eq!(attempted.status, AiGeneratedAnimationStatus::Validated);
        assert_eq!(attempted.sha256, validated.sha256);
    }

    #[test]
    fn generated_animation_never_pollutes_transition_catalog() {
        let database = Database::open_in_memory().expect("in-memory database");
        database.migrate().expect("migrate to latest");
        let repository = AiRepository::new(database.clone());
        let project = repository
            .create_project("生成动画", &enhancement_test_profile())
            .expect("create project");
        repository
            .record_generated_animation_asset(insert_animation(
                project.id,
                43,
                1,
                AiGeneratedAnimationStatus::Validated,
                "reuse-2",
            ))
            .expect("record insert");
        // 生成资产是单工程产物：MUST NOT 写入目录素材表，也不影响边界召回。
        let connection = database.connection().expect("connection");
        let catalog_rows: i64 = connection
            .query_row("SELECT COUNT(*) FROM transition_materials", [], |row| {
                row.get(0)
            })
            .expect("count");
        assert_eq!(catalog_rows, 0, "生成资产 MUST NOT 写入目录素材表");
    }

    #[test]
    fn enhancement_review_for_project_surfaces_in_progress_and_ready_highlights() {
        let database = Database::open_in_memory().expect("in-memory database");
        database.migrate().expect("migrate to latest");
        let repository = AiRepository::new(database);
        let project = repository
            .create_project("审阅", &enhancement_test_profile())
            .expect("create project");
        // 高光 42 已就绪：insert 动画 applied + 已校验资产 + 成本。
        let mut applied = enhancement(project.id, 42, 1, AiEnhancementStatus::Applied, "fp");
        applied.kind = AiEnhancementKind::VideoAnimation;
        applied.cost_minor = 250;
        repository
            .record_enhancement(applied, "fp")
            .expect("写入动画增强");
        repository
            .record_generated_animation_asset(insert_animation(
                project.id,
                42,
                1,
                AiGeneratedAnimationStatus::Validated,
                "reuse-1",
            ))
            .expect("写入生成资产");
        // 高光 43 尚无任何结果（进行中）。
        let items = repository
            .enhancement_review_for_project(project.id, &[42, 43])
            .expect("组装审阅项");
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].kind, Some(AiEnhancementKind::VideoAnimation));
        assert_eq!(
            items[0].composition,
            Some(AiGeneratedAnimationComposition::Insert)
        );
        assert_eq!(
            items[0].asset_status,
            Some(AiGeneratedAnimationStatus::Validated)
        );
        assert_eq!(items[0].cost_minor, 250);
        // 进行中的高光不阻塞列表：`kind = None`。
        assert_eq!(items[1].kind, None);
        assert_eq!(items[1].cost_minor, 0);
    }

    #[test]
    fn migration_v30_adds_cost_column_and_round_trips_cost() {
        let database = Database::open_in_memory().expect("in-memory database");
        database.migrate().expect("migrate to latest");
        database.migrate().expect("migrate again is idempotent");
        let repository = AiRepository::new(database);
        let project = repository
            .create_project("成本", &enhancement_test_profile())
            .expect("create project");
        let mut input = enhancement(project.id, 42, 1, AiEnhancementStatus::Applied, "fp");
        input.kind = AiEnhancementKind::VideoAnimation;
        input.cost_minor = 250;
        let written = repository
            .record_enhancement(input, "fp")
            .expect("写入带成本的增强记录");
        assert_eq!(written.cost_minor, 250);
        // 可查询，且记录里不含 Prompt 正文或下载地址。
        let listed = repository
            .applied_enhancements_for_project(project.id)
            .expect("查询");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].cost_minor, 250);
        assert_eq!(listed[0].kind, AiEnhancementKind::VideoAnimation);
    }

    #[test]
    fn record_enhancement_rejects_a_second_applied_kind_for_the_same_candidate() {
        let database = Database::open_in_memory().expect("in-memory database");
        database.migrate().expect("migrate to latest");
        let repository = AiRepository::new(database);
        let project = repository
            .create_project("互斥", &enhancement_test_profile())
            .expect("create project");
        // 先写一条 applied 的特效字幕。
        repository
            .record_enhancement(
                enhancement(project.id, 42, 1, AiEnhancementStatus::Applied, "fp"),
                "fp",
            )
            .expect("首条 applied 必须写入");
        // 同一候选再来一条 applied 的动画：拒绝（两条分支不得同时进入工程）。
        let mut animation = enhancement(project.id, 42, 2, AiEnhancementStatus::Applied, "fp");
        animation.kind = AiEnhancementKind::VideoAnimation;
        assert!(matches!(
            repository.record_enhancement(animation, "fp"),
            Err(AiRepositoryError::InvalidState(_))
        ));
        // 同一候选的非终态动画不影响字幕分支。
        let mut suggested = enhancement(project.id, 42, 3, AiEnhancementStatus::Suggested, "fp");
        suggested.kind = AiEnhancementKind::VideoAnimation;
        repository
            .record_enhancement(suggested, "fp")
            .expect("非终态必须可写入");
    }

    #[test]
    fn migration_v27_is_idempotent_and_keeps_existing_projects_unchanged() {
        let database = Database::open_in_memory().expect("in-memory database");
        database.migrate().expect("migrate to latest");
        let repository = AiRepository::new(database.clone());
        let project = repository
            .create_project("旧工程", &enhancement_test_profile())
            .expect("create project");
        // 再次迁移必须幂等，且不得改写既有工程。
        database.migrate().expect("migrate again");
        let unchanged = repository
            .get_project_row(project.id)
            .expect("project survives");
        assert_eq!(unchanged.name, "旧工程");
        assert_eq!(unchanged.status, AiProjectStatus::Draft);
        let connection = database.connection().expect("connection");
        for table in ["ai_highlight_enhancements", "ai_effect_caption_renders"] {
            let exists: bool = connection
                .query_row(
                    "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
                    [table],
                    |_| Ok(()),
                )
                .optional()
                .expect("query sqlite_master")
                .is_some();
            assert!(exists, "迁移 27 必须建表 {table}");
        }
        let version: i64 = connection
            .query_row(
                "SELECT version FROM schema_migrations WHERE version = 27",
                [],
                |row| row.get(0),
            )
            .expect("version 27 recorded");
        assert_eq!(version, 27);
    }

    #[test]
    fn stale_enhancement_is_history_only_and_does_not_replace_applied() {
        let database = Database::open_in_memory().expect("in-memory database");
        database.migrate().expect("migrate");
        let repository = AiRepository::new(database.clone());
        let project = repository
            .create_project("增强工程", &enhancement_test_profile())
            .expect("create project");

        let applied = repository
            .record_enhancement(
                enhancement(project.id, 42, 1, AiEnhancementStatus::Applied, "fp-1"),
                "fp-1",
            )
            .expect("record applied");
        assert_eq!(applied.status, AiEnhancementStatus::Applied);

        // 用户改了工程：当前指纹变成 fp-2，而这条建议仍基于 fp-1。
        let stale = repository
            .record_enhancement(
                enhancement(project.id, 42, 2, AiEnhancementStatus::Applied, "fp-1"),
                "fp-2",
            )
            .expect("record stale");
        assert_eq!(
            stale.status,
            AiEnhancementStatus::Suggested,
            "过期代次只能是历史建议"
        );
        assert_eq!(
            stale.rejection_code.as_deref(),
            Some("enhancement_stale_generation")
        );

        let active = repository
            .active_enhancement_for_candidate(42)
            .expect("query active")
            .expect("active exists");
        assert_eq!(active.generation, 1, "人工编辑后的生效结果不得被旧代次替换");
        assert_eq!(
            active.composition_version.as_deref(),
            Some("composition-v1")
        );
    }

    #[test]
    fn applied_enhancement_is_never_downgraded_by_a_later_write() {
        let database = Database::open_in_memory().expect("in-memory database");
        database.migrate().expect("migrate");
        let repository = AiRepository::new(database.clone());
        let project = repository
            .create_project("增强工程", &enhancement_test_profile())
            .expect("create project");
        repository
            .record_enhancement(
                enhancement(project.id, 7, 1, AiEnhancementStatus::Applied, "fp-1"),
                "fp-1",
            )
            .expect("record applied");
        // 同一 (candidate, generation) 再写一条建议：不得覆盖已生效结果。
        let written = repository
            .record_enhancement(
                enhancement(project.id, 7, 1, AiEnhancementStatus::Suggested, "fp-1"),
                "fp-1",
            )
            .expect("record suggestion");
        assert_eq!(written.status, AiEnhancementStatus::Applied);
        assert_eq!(
            written.updated_at, written.created_at,
            "被拒的覆盖不得改动已生效行"
        );
    }

    #[test]
    fn supersede_marks_previous_generation_and_export_reads_only_applied() {
        let database = Database::open_in_memory().expect("in-memory database");
        database.migrate().expect("migrate");
        let repository = AiRepository::new(database.clone());
        let project = repository
            .create_project("增强工程", &enhancement_test_profile())
            .expect("create project");
        repository
            .record_enhancement(
                enhancement(project.id, 9, 1, AiEnhancementStatus::Applied, "fp-1"),
                "fp-1",
            )
            .expect("gen 1");
        repository
            .record_enhancement(
                enhancement(project.id, 9, 2, AiEnhancementStatus::Applied, "fp-1"),
                "fp-1",
            )
            .expect("gen 2");
        // 两条 applied 都命中：导出按候选取最新，旧代次需要显式让位。
        let changed = repository
            .supersede_older_enhancements(9, 2)
            .expect("supersede");
        assert_eq!(changed, 1);
        let applied = repository
            .applied_enhancements_for_project(project.id)
            .expect("applied list");
        assert_eq!(applied.len(), 1);
        assert_eq!(applied[0].generation, 2);
        let superseded = repository
            .get_enhancement(9, 1)
            .expect("query gen 1")
            .expect("gen 1 exists");
        assert_eq!(superseded.status, AiEnhancementStatus::Superseded);
    }

    #[test]
    fn enhancement_kind_and_status_parse_reject_unknown_values() {
        assert!(matches!(
            AiEnhancementKind::parse("effect_caption"),
            Ok(AiEnhancementKind::EffectCaption)
        ));
        assert!(
            AiEnhancementKind::parse("subtitle_karaoke").is_err(),
            "未知增强方式必须被拒绝"
        );
        assert!(
            AiEnhancementStatus::parse("pending").is_err(),
            "未知增强状态必须被拒绝"
        );
    }

    #[test]
    fn enhancement_interval_and_generation_are_validated() {
        let database = Database::open_in_memory().expect("in-memory database");
        database.migrate().expect("migrate");
        let repository = AiRepository::new(database.clone());
        let project = repository
            .create_project("增强工程", &enhancement_test_profile())
            .expect("create project");
        let mut invalid_generation =
            enhancement(project.id, 1, 0, AiEnhancementStatus::Suggested, "fp-1");
        assert!(
            repository
                .record_enhancement(invalid_generation.clone(), "fp-1")
                .is_err()
        );
        invalid_generation.generation = 1;
        invalid_generation.end_ms = invalid_generation.start_ms;
        assert!(
            repository
                .record_enhancement(invalid_generation, "fp-1")
                .is_err()
        );
    }

    // ---- 步骤 3 任务组 5/9：渲染尝试的缓存键复用 ----

    fn render_attempt_input(
        project_id: i64,
        candidate_id: i64,
        cache_key: &str,
        frames_dir: &str,
    ) -> NewAiEffectCaptionRenderAttempt {
        NewAiEffectCaptionRenderAttempt {
            project_id,
            candidate_id,
            cache_key: cache_key.to_owned(),
            composition_version: "composition-v1".to_owned(),
            composition_fingerprint: "composition-fp".to_owned(),
            font_version: "caption-fonts-2026.10".to_owned(),
            browser_version: "154.0.8037.58".to_owned(),
            renderer_version: "0.8.96".to_owned(),
            engine_script_version: "38592478aad3f515".to_owned(),
            width: 1080,
            height: 1920,
            fps: 30,
            frames_dir: frames_dir.to_owned(),
        }
    }

    #[test]
    fn render_attempt_is_reused_for_same_cache_key_and_recreated_for_new_key() {
        let database = Database::open_in_memory().expect("in-memory database");
        database.migrate().expect("migrate");
        let repository = AiRepository::new(database.clone());
        let project = repository
            .create_project("渲染工程", &enhancement_test_profile())
            .expect("create project");
        let (first, cached) = repository
            .begin_render_attempt(render_attempt_input(
                project.id,
                5,
                "key-a",
                "/tmp/frames-a",
            ))
            .expect("begin first");
        assert!(!cached, "首次渲染不能算命中缓存");
        assert_eq!(first.status, AiRenderAttemptStatus::Rendering);
        let ready = repository
            .mark_render_attempt_ready(first.id, 450)
            .expect("mark ready");
        assert_eq!(ready.status, AiRenderAttemptStatus::Ready);
        assert_eq!(ready.frame_count, 450);

        // 同一 cache_key：命中缓存并复用同一行。
        let (second, cached) = repository
            .begin_render_attempt(render_attempt_input(
                project.id,
                5,
                "key-a",
                "/tmp/frames-a",
            ))
            .expect("begin again");
        assert!(cached, "相同缓存键必须命中缓存");
        assert_eq!(second.id, first.id);

        // 任一版本字段变化 → 新的缓存键 → 新的渲染尝试。
        let mut changed = render_attempt_input(project.id, 5, "key-b", "/tmp/frames-b");
        changed.browser_version = "155.0.0.0".to_owned();
        let (third, cached) = repository
            .begin_render_attempt(changed)
            .expect("begin new key");
        assert!(!cached);
        assert_ne!(third.id, first.id);
        assert_eq!(
            repository
                .find_ready_render_attempt("key-a")
                .expect("query ready")
                .map(|attempt| attempt.id),
            Some(first.id)
        );
        assert!(
            repository
                .find_ready_render_attempt("key-b")
                .expect("query ready")
                .is_none()
        );
    }

    #[test]
    fn failed_render_attempt_is_retried_on_the_same_key() {
        let database = Database::open_in_memory().expect("in-memory database");
        database.migrate().expect("migrate");
        let repository = AiRepository::new(database.clone());
        let project = repository
            .create_project("渲染工程", &enhancement_test_profile())
            .expect("create project");
        let (attempt, _) = repository
            .begin_render_attempt(render_attempt_input(
                project.id,
                6,
                "key-c",
                "/tmp/frames-c",
            ))
            .expect("begin");
        let failed = repository
            .mark_render_attempt_failed(attempt.id, "render_timeout")
            .expect("mark failed");
        assert_eq!(failed.status, AiRenderAttemptStatus::Failed);
        assert_eq!(failed.failure_code.as_deref(), Some("render_timeout"));
        assert_eq!(failed.frame_count, 0);
        assert!(
            repository
                .find_ready_render_attempt("key-c")
                .expect("query ready")
                .is_none()
        );

        // 重试同一个键：复用同一行并把状态复位为 rendering，同时清掉上次失败原因。
        let (retried, cached) = repository
            .begin_render_attempt(render_attempt_input(
                project.id,
                6,
                "key-c",
                "/tmp/frames-c",
            ))
            .expect("retry");
        assert!(!cached, "失败后的重试不算命中缓存");
        assert_eq!(retried.id, attempt.id);
        assert_eq!(retried.status, AiRenderAttemptStatus::Rendering);
        assert!(retried.failure_code.is_none());
    }

    #[test]
    fn render_attempt_rejects_invalid_spec_and_empty_key() {
        let database = Database::open_in_memory().expect("in-memory database");
        database.migrate().expect("migrate");
        let repository = AiRepository::new(database.clone());
        let project = repository
            .create_project("渲染工程", &enhancement_test_profile())
            .expect("create project");
        let mut empty_key = render_attempt_input(project.id, 7, "  ", "/tmp/frames-d");
        assert!(repository.begin_render_attempt(empty_key.clone()).is_err());
        empty_key.cache_key = "key-d".to_owned();
        empty_key.fps = 0;
        assert!(repository.begin_render_attempt(empty_key).is_err());
    }

    // ---- 步骤 3 任务组 6.4：预览的帧不得被导出复用 ----

    #[test]
    fn preview_frames_are_not_reused_by_export_and_version_change_forces_new_attempt() {
        use crate::ai::effect_caption_render::RenderCacheKey;
        use crate::ai::effect_caption_spec::{
            CanvasSpec, HighlightRenderWindow, RenderPurpose, RenderSpec, plan_render_spec,
        };

        let database = Database::open_in_memory().expect("in-memory database");
        database.migrate().expect("migrate");
        let repository = AiRepository::new(database.clone());
        let project = repository
            .create_project("预览导出工程", &enhancement_test_profile())
            .expect("create project");
        let window = HighlightRenderWindow {
            start_ms: 12_000,
            end_ms: 27_000,
        };
        let preview = plan_render_spec(RenderPurpose::Preview, CanvasSpec::default(), window)
            .expect("preview");
        let export =
            plan_render_spec(RenderPurpose::Export, CanvasSpec::default(), window).expect("export");
        let key = |spec: &RenderSpec, composition: &str, font: &str| {
            RenderCacheKey::new(&spec.version_set(
                composition,
                "composition-fingerprint",
                font,
                "154.0.8037.58",
                "0.8.96",
                "38592478aad3f515",
            ))
            .as_str()
            .to_owned()
        };

        let preview_key = key(&preview, "composition-v1", "caption-fonts-2026.10");
        let export_key = key(&export, "composition-v1", "caption-fonts-2026.10");
        assert_ne!(preview_key, export_key, "预览与导出必须是不同的缓存键");

        // 预览完成并置 ready。
        let (attempt, _) = repository
            .begin_render_attempt(render_attempt_input(
                project.id,
                11,
                &preview_key,
                "/tmp/preview",
            ))
            .expect("begin preview");
        repository
            .mark_render_attempt_ready(attempt.id, preview.frame_count)
            .expect("preview ready");

        // 导出：预览的 ready 不能命中导出的键，必须重新渲染。
        assert!(
            repository
                .find_ready_render_attempt(&export_key)
                .expect("query export key")
                .is_none(),
            "导出不得复用预览的帧"
        );
        let (export_attempt, cached) = repository
            .begin_render_attempt(render_attempt_input(
                project.id,
                11,
                &export_key,
                "/tmp/export",
            ))
            .expect("begin export");
        assert!(!cached);
        repository
            .mark_render_attempt_ready(export_attempt.id, export.frame_count)
            .expect("export ready");
        assert_eq!(
            repository
                .find_ready_render_attempt(&export_key)
                .expect("query export key")
                .map(|attempt| attempt.frame_count),
            Some(export.frame_count)
        );

        // 字体版本变化（等价于文案/字体变化）：新键必须重新渲染，且不复用旧帧。
        let changed_key = key(&export, "composition-v1", "caption-fonts-2026.11");
        assert_ne!(changed_key, export_key);
        assert!(
            repository
                .find_ready_render_attempt(&changed_key)
                .expect("query changed key")
                .is_none(),
            "字体版本变化后不得复用旧帧"
        );
        let (changed, cached) = repository
            .begin_render_attempt(render_attempt_input(
                project.id,
                11,
                &changed_key,
                "/tmp/export-2",
            ))
            .expect("begin changed");
        assert!(!cached);
        assert_ne!(
            changed.id, export_attempt.id,
            "版本变化必须创建新的渲染尝试"
        );
    }

    #[test]
    fn migration_v28_adds_resolution_columns_without_touching_existing_values() {
        let database = Database::open_in_memory().expect("in-memory database");
        database.migrate().expect("migrate");
        let connection = database.connection().expect("connection");
        for column in ["model_source", "resolved_at", "next_resolution_at"] {
            let exists: bool = connection
                .query_row(
                    &format!(
                        "SELECT 1 FROM pragma_table_info('llm_provider_settings') WHERE name = '{column}'"
                    ),
                    [],
                    |_| Ok(()),
                )
                .optional()
                .expect("pragma")
                .is_some();
            assert!(exists, "迁移 28 必须新增列 {column}");
        }
        // 只增列：旧库升级后既有阈值与 provider 取值保持不变。
        // 必须先释放连接再走仓库：内存库连接池同时只允许一个连接，
        // 持有连接再取连接会死锁。
        drop(connection);
        let repository = AiRepository::new(database.clone());
        let mut settings = repository
            .get_llm_provider_settings(true)
            .expect("读取设置");
        settings.qualified_score = 66;
        settings.excellent_score = 77;
        settings.transition_auto_apply_score = 9;
        repository
            .save_llm_provider_settings(&settings)
            .expect("保存设置");
        let saved = repository
            .get_llm_provider_settings(true)
            .expect("再读设置");
        assert_eq!(saved.qualified_score, 66);
        assert_eq!(saved.excellent_score, 77);
        assert_eq!(saved.transition_auto_apply_score, 9);
        // 新字段默认未设置（不臆造解析信息）。
        assert!(saved.model_source.is_none());
        assert!(saved.resolved_at.is_none());
        assert!(saved.next_resolution_at.is_none());
    }

    #[test]
    fn resolution_metadata_round_trips_through_settings() {
        let database = Database::open_in_memory().expect("in-memory database");
        database.migrate().expect("migrate");
        let repository = AiRepository::new(database);
        let mut settings = repository
            .get_llm_provider_settings(true)
            .expect("读取设置");
        settings.model_id = "qwen-max".to_owned();
        settings.model_source = Some("heartbeat".to_owned());
        settings.resolved_at = Some("2026-10-07T00:00:00+00:00".to_owned());
        settings.next_resolution_at = Some("2026-10-07T00:01:30+00:00".to_owned());
        repository
            .save_llm_provider_settings(&settings)
            .expect("保存设置");
        let saved = repository
            .get_llm_provider_settings(true)
            .expect("再读设置");
        assert_eq!(saved.model_id, "qwen-max");
        assert_eq!(saved.model_source.as_deref(), Some("heartbeat"));
        assert_eq!(
            saved.resolved_at.as_deref(),
            Some("2026-10-07T00:00:00+00:00")
        );
        assert_eq!(
            saved.next_resolution_at.as_deref(),
            Some("2026-10-07T00:01:30+00:00")
        );
    }

    #[test]
    fn migration_v27_allows_the_enhancement_smart_stage() {
        let database = Database::open_in_memory().expect("in-memory database");
        database.migrate().expect("migrate");
        let connection = database.connection().expect("connection");
        // 阶段 CHECK 必须包含增强阶段：否则智能成片在转场之后无法推进。
        let (workflow_sql, attempt_sql): (String, String) = connection
            .query_row(
                "SELECT (SELECT sql FROM sqlite_master WHERE name = 'ai_smart_workflows'), (SELECT sql FROM sqlite_master WHERE name = 'ai_smart_stage_attempts')",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("schema sql");
        assert!(workflow_sql.contains("'enhancement'"), "{workflow_sql}");
        assert!(attempt_sql.contains("'enhancement'"), "{attempt_sql}");

        // 行为验证：真的能写入 enhancement 阶段的行。
        let now = "2026-10-03T00:00:00Z";
        connection
            .execute(
                r#"INSERT INTO ai_smart_workflows(
                       name, mode, status, stage, generation, source_summary, provider,
                       model_id, text_scope, configuration_fingerprint, created_at, updated_at
                   ) VALUES('增强工作流', 'local', 'review_ready', 'enhancement', 1, '摘要',
                       'deepseek', 'deepseek-chat', 'minimal', 'fingerprint', ?1, ?1)"#,
                [now],
            )
            .expect("insert workflow with enhancement stage");
        let workflow_id = connection.last_insert_rowid();
        connection
            .execute(
                r#"INSERT INTO ai_smart_stage_attempts(
                       workflow_id, stage, input_fingerprint, attempt_generation, status,
                       progress, created_at, updated_at
                   ) VALUES(?1, 'enhancement', 'fingerprint', 1, 'completed', 100, ?2, ?2)"#,
                params![workflow_id, now],
            )
            .expect("insert enhancement stage attempt");
        let stage: String = connection
            .query_row(
                "SELECT stage FROM ai_smart_stage_attempts WHERE workflow_id = ?1",
                [workflow_id],
                |row| row.get(0),
            )
            .expect("read stage");
        assert_eq!(stage, "enhancement");
    }
}
