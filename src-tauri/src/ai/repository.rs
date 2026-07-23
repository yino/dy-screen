//! AI 项目、输入、ASR 产物和稳定句段的 SQLite 事务边界。
//!
//! 原子发布、状态迁移和引用清理都在此层维护，且永不删除原始媒体或随包模型。

use chrono::Utc;
use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::database::{Database, DatabaseError};

use super::domain::{
    AiArtifactStatus, AiInputSourceKind, AiInputStatus, AiProject, AiProjectDetail, AiProjectInput,
    AiProjectStatus, AsrArtifact, NewAiProjectInput, NewAsrArtifact, RecognitionProfile,
    RecoverySummary, SourceFingerprint, TranscriptSegment, TranscriptSegmentDraft,
};

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
}

pub type Result<T> = std::result::Result<T, AiRepositoryError>;

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

    /// 将用户明确选择的失败输入恢复为待执行；不会自动无限重试。
    pub fn prepare_input_retry(&self, input_id: i64) -> Result<AiProjectInput> {
        let input = self.get_input(input_id)?;
        if input.status != AiInputStatus::Failed {
            return Err(AiRepositoryError::InvalidState(
                "只有失败输入可以重试".to_owned(),
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
            SET status = 'pending', artifact_id = NULL, last_error_code = NULL,
                last_error_message = NULL, updated_at = ?1
            WHERE id = ?2 AND status = 'failed'
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
            SET status = 'pending', last_error_code = 'interrupted',
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
    String,
);

fn project_select(suffix: &str) -> String {
    format!(
        r#"
        SELECT id, name, status, recognition_profile_json, recognition_profile_hash,
               input_frozen, progress_percent, last_error_code, last_error_message,
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
        created_at: row.9,
        updated_at: row.10,
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
);

fn input_select(suffix: &str) -> String {
    format!(
        r#"
        SELECT id, project_id, position, source_kind, video_id, display_name,
               source_path, source_fingerprint_json, source_fingerprint_hash,
               duration_ms, audio_present, project_offset_ms, status, artifact_id,
               last_error_code, last_error_message
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
