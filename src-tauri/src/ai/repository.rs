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
    AiClipSegment, AiClipSegmentUpdate, AiClipSubtitle, AiClipSubtitleUpdate, AiClipTimelineUnit,
    AiClipTimelineUnitKind, AiHighlightCandidate, AiHighlightCandidatePage, AiHighlightChunk,
    AiHighlightProgress, AiHighlightRun, AiHighlightRunStatus, AiInputSourceKind, AiInputStatus,
    AiProject, AiProjectDetail, AiProjectInput, AiProjectStatus, AsrArtifact, NewAiHighlightChunk,
    NewAiHighlightRun, NewAiProjectInput, NewAsrArtifact, RecognitionProfile, RecoverySummary,
    SourceFingerprint, TranscriptSegment, TranscriptSegmentDraft,
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
}

pub type Result<T> = std::result::Result<T, AiRepositoryError>;

#[derive(Debug, Clone)]
pub struct ClipExportSource {
    pub segment: AiClipSegment,
    pub source_path: String,
    pub source_fingerprint: SourceFingerprint,
    pub subtitles: Vec<AiClipSubtitle>,
    pub bridge_after: Option<ClipExportBridge>,
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
                  trim(ts.normalized_text), trim(ts.normalized_text), 0, ?3, ?3
           FROM ai_clip_segments s
           JOIN ai_project_inputs input ON input.id = s.input_id
           JOIN asr_artifacts artifact ON artifact.id = input.artifact_id
           JOIN transcript_segments ts ON ts.artifact_id = artifact.id
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
               export_status = 'idle', export_progress = 0, output_path = NULL,
               last_error_code = NULL, last_error_message = NULL, updated_at = ?1
           WHERE id = ?2"#,
        params![now, clip_project_id],
    )?;
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

    /// 标记项目正在删除，阻止新的调度、重试和高光运行。
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

    pub fn get_llm_provider_settings(&self, key_configured: bool) -> Result<LlmProviderSettings> {
        let connection = self.database.connection()?;
        let row = connection
            .query_row(
                "SELECT provider, model_id, timeout_ms, prompt_version, qualified_score, excellent_score, updated_at FROM llm_provider_settings WHERE id = 1",
                [],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, String>(6)?,
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
            r#"INSERT INTO llm_provider_settings(id, provider, model_id, timeout_ms, prompt_version, qualified_score, excellent_score, updated_at)
               VALUES(1, ?1, ?2, ?3, ?4, ?5, ?6, ?7)
               ON CONFLICT(id) DO UPDATE SET provider=excluded.provider, model_id=excluded.model_id,
                   timeout_ms=excluded.timeout_ms, prompt_version=excluded.prompt_version,
                   qualified_score=excluded.qualified_score, excellent_score=excluded.excellent_score,
                   updated_at=excluded.updated_at"#,
            params![settings.provider, settings.model_id.trim(), settings.timeout_ms as i64, settings.prompt_version, settings.qualified_score, settings.excellent_score, now],
        )?;
        self.get_llm_provider_settings(settings.key_configured)
    }

    pub fn create_highlight_run(&self, input: NewAiHighlightRun) -> Result<AiHighlightRun> {
        if !input.user_authorized {
            return Err(AiRepositoryError::InvalidState(
                "高光分析必须由用户显式授权".to_owned(),
            ));
        }
        let project = self.get_project_row(input.project_id)?;
        if !matches!(
            project.status,
            AiProjectStatus::Completed | AiProjectStatus::CompletedWithErrors
        ) {
            return Err(AiRepositoryError::InvalidState(
                "只有 ASR 完成的项目可以进行高光分析".to_owned(),
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
                "高光运行阈值必须在 0 到 100 之间，且优秀阈值不能低于合格阈值".to_owned(),
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
                    .ok_or(AiRepositoryError::Integrity("高光运行指纹冲突".to_owned()))?
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
            .ok_or(AiRepositoryError::NotFound("高光运行"))?;
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
                    "高光分块不能没有句段".to_owned(),
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
                .map_err(|_| AiRepositoryError::Integrity("高光批次统计无效".to_owned()))
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
                "高光批次不存在或已经完成".to_owned(),
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
            .map_err(|_| AiRepositoryError::Serialization("高光候选草稿无法序列化".to_owned()))?;
        let now = Utc::now().to_rfc3339();
        let mut connection = self.database.connection()?;
        let transaction = connection.transaction()?;
        let changed = transaction.execute(
            "UPDATE ai_highlight_chunks SET status = 'completed', candidate_count = ?1, token_usage = ?2, candidate_drafts_json = ?3, last_error_code = NULL, last_error_message = NULL, updated_at = ?4 WHERE id = ?5 AND run_id = ?6 AND status != 'completed'",
            params![drafts.len() as i64, token_usage as i64, drafts_json, now, chunk_id, run_id],
        )?;
        if changed == 0 {
            return Err(AiRepositoryError::InvalidState(
                "高光批次不存在或已经完成".to_owned(),
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
                    .map_err(|_| AiRepositoryError::Integrity("高光候选草稿快照损坏".to_owned()))?;
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
                rusqlite::Error::QueryReturnedNoRows => AiRepositoryError::NotFound("高光运行"),
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
                rusqlite::Error::QueryReturnedNoRows => AiRepositoryError::NotFound("高光运行"),
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
                return Err(AiRepositoryError::NotFound("高光候选"));
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
                "只能选择达到本次合格阈值的高光候选".to_owned(),
            ));
        }
        self.list_highlight_candidates(run_id)?
            .into_iter()
            .find(|candidate| candidate.id == candidate_id)
            .ok_or(AiRepositoryError::NotFound("高光候选"))
    }

    pub fn get_or_create_clip_project(&self, run_id: i64) -> Result<AiClipProjectDetail> {
        let mut connection = self.database.connection()?;
        let transaction = connection.transaction()?;
        let existing = transaction
            .query_row(
                "SELECT id FROM ai_clip_projects WHERE highlight_run_id = ?1",
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
                    "高光分析尚未完成，暂时不能创建剪辑工程".to_owned(),
                ));
            }
            if selected_count == 0 {
                return Err(AiRepositoryError::InvalidState(
                    "请至少选择一个高光候选后再编辑视频".to_owned(),
                ));
            }
            let now = Utc::now().to_rfc3339();
            transaction.execute(
                "INSERT INTO ai_clip_projects(highlight_run_id, name, export_status, export_progress, created_at, updated_at) VALUES(?1, ?2, 'idle', 0, ?3, ?3)",
                params![run_id, format!("{} - 高光剪辑", project_name), now],
            )?;
            let id = transaction.last_insert_rowid();
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

    pub fn get_clip_project(&self, clip_project_id: i64) -> Result<AiClipProjectDetail> {
        let transition_repository =
            crate::transition_materials::TransitionMaterialRepository::new(self.database.clone());
        let boundaries = transition_repository
            .list_boundaries(clip_project_id)
            .map_err(|error| AiRepositoryError::Integrity(error.to_string()))?;
        let mut bridge_materials = HashMap::new();
        for boundary in boundaries.iter().filter(|item| item.active && !item.stale) {
            if let (Some(key), Some(version)) = (&boundary.asset_key, boundary.asset_version) {
                let material = transition_repository
                    .material(key, version)
                    .map_err(|error| AiRepositoryError::Integrity(error.to_string()))?;
                let download = transition_repository
                    .download(key, version)
                    .map_err(|error| AiRepositoryError::Integrity(error.to_string()))?;
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
                "SELECT id, highlight_run_id, name, export_status, export_progress, output_path, last_error_code, last_error_message, output_width, output_height, version, created_at, updated_at FROM ai_clip_projects WHERE id = ?1",
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
            });
            if let Some(right) = segments.get(index + 1)
                && let Some((boundary_id, material, download)) =
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
                });
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
                   FROM ai_clip_projects p
                   JOIN ai_highlight_candidates c ON c.run_id = p.highlight_run_id
                   WHERE p.id = ?1 AND c.id = ?2 AND c.selected = 1"#,
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
                AiRepositoryError::Integrity("只能追加当前高光运行中已进入待切片的视频".to_owned())
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
        let changed = self.database.connection()?.execute(
            "UPDATE ai_clip_projects SET export_status = ?1, export_progress = ?2, output_path = ?3, last_error_code = ?4, last_error_message = ?5, updated_at = ?6 WHERE id = ?7",
            params![status.as_str(), i64::from(progress), output_path, code, message, Utc::now().to_rfc3339(), clip_project_id],
        )?;
        if changed == 0 {
            return Err(AiRepositoryError::NotFound("剪辑工程"));
        }
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
                   last_error_code = NULL, last_error_message = NULL, updated_at = ?1
               WHERE id = ?2"#,
            params![Utc::now().to_rfc3339(), clip_project_id],
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
        let changed = self.database.connection()?.execute(
            r#"UPDATE ai_clip_projects
               SET export_status = 'cancelled', export_progress = 0, output_path = NULL,
                   last_error_code = 'app_restarted',
                   last_error_message = '上次视频导出因应用退出而取消，可重新导出',
                   updated_at = ?1
               WHERE export_status = 'exporting'"#,
            [Utc::now().to_rfc3339()],
        )?;
        Ok(changed as u64)
    }

    pub fn clip_export_sources(&self, clip_project_id: i64) -> Result<Vec<ClipExportSource>> {
        let detail = self.get_clip_project(clip_project_id)?;
        let connection = self.database.connection()?;
        if detail.project.export_status == AiClipExportStatus::Exporting {
            return Err(AiRepositoryError::InvalidState(
                "剪辑工程正在导出".to_owned(),
            ));
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
                Ok(ClipExportSource {
                    segment,
                    source_path,
                    source_fingerprint,
                    subtitles,
                    bridge_after: None,
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
                model_version, status, created_at, last_used_at
            ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'pending', ?9, ?9)
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
        transaction.commit()?;
        Ok(RecoverySummary {
            projects,
            inputs,
            artifacts,
        })
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

/// v7 为可恢复调度和高光分析增加持久化边界。
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

/// v9 保存逐批候选草稿，使长时间高光分析可以展示真实进度并在中断后继续。
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

/// v12 冻结高光筛选阈值，保证历史运行的推荐和自动选择语义稳定。
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
               model_version, status, duration_ms, language, created_at,
               published_at, last_used_at
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
        status: AiArtifactStatus::parse(&row.8)?,
        duration_ms: row.9.map(|value| value as u64),
        language: row.10,
        created_at: row.11,
        published_at: row.12,
        last_used_at: row.13,
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
            .map_err(|_| AiRepositoryError::Integrity("高光标签快照损坏".to_owned()))?,
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
    Ok(AiClipProject {
        id: row.get(0)?,
        highlight_run_id: row.get(1)?,
        name: row.get(2)?,
        output_width,
        output_height,
        version,
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
            "高光候选必须引用句段且时长在 15 到 90 秒之间".to_owned(),
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
            "高光评分必须在 0 到 100 之间且排名有效".to_owned(),
        ));
    }
    Ok(())
}
