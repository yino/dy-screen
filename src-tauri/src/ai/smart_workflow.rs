//! 智能成片父任务的持久化状态机。
//!
//! 本模块不拥有 recorder，也不读取媒体。它只接受已经通过 service/command
//! 层验证的受信来源，并以 SQLite 事务隔离任务代次、阶段尝试和草稿所有权。

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use chrono::Utc;
use rusqlite::{OptionalExtension, params};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::database::Database;
use crate::domain::Video;
use crate::transition_matching::{TransitionMatchingError, TransitionMatchingWorkflow};

use super::{
    AiActiveLiveSession, AiInputStatus, AiJobController, AiProjectService, AiProjectStatus,
    AiRepository, AiRepositoryError, AiSmartBatchStatus, AiSmartCandidate, AiSmartCandidateSource,
    AiSmartDraft, AiSmartDraftOwnership, AiSmartDraftStatus, AiSmartReplaySession,
    AiSmartReplaySessionCursor, AiSmartReplaySessionPage, AiSmartStage, AiSmartStageAttempt,
    AiSmartStageAttemptStatus, AiSmartWorkflow, AiSmartWorkflowBatch, AiSmartWorkflowDetail,
    AiSmartWorkflowEvent, AiSmartWorkflowMetric, AiSmartWorkflowMode, AiSmartWorkflowStatus,
    AiTranscriptProjection, AnalysisSegment, ClipTextCorrectionError, ClipTextCorrectionWorkflow,
    HighlightWorkflow, LlmError, NewAiSmartCandidate, NewAiSmartCandidateSource,
    NewAiSmartWorkflow, NewAiSmartWorkflowBatch, Result, SmartWorkflowConfiguration,
    TrustedLocalFile,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SmartStageAttemptStart {
    pub workflow_id: i64,
    pub batch_id: Option<i64>,
    pub draft_generation: Option<u32>,
    pub stage: AiSmartStage,
    pub input_fingerprint: String,
}

#[derive(Clone)]
pub struct SmartWorkflowRepository {
    database: Database,
}

impl SmartWorkflowRepository {
    pub fn new(database: Database) -> Self {
        Self { database }
    }

    pub fn create(&self, input: &NewAiSmartWorkflow) -> Result<AiSmartWorkflowDetail> {
        validate_workflow_input(input)?;
        if let Some(session_id) = input.source_session_id {
            let session = self.database.get_session(session_id)?;
            match input.mode {
                AiSmartWorkflowMode::Live => {
                    if session.ended_at.is_some() || session.status != "recording" {
                        return Err(invalid("只能选择系统当前正在录制的直播会话"));
                    }
                }
                AiSmartWorkflowMode::Replay => {
                    let videos = self.database.list_session_videos(session_id)?;
                    if session.ended_at.is_none() {
                        return Err(invalid("直播回放必须选择已结束的录制会话"));
                    }
                    if videos.is_empty()
                        || videos
                            .iter()
                            .any(|video| video.status != "complete" || video.ended_at.is_none())
                    {
                        return Err(invalid("直播回放必须包含已完成登记的录像分片"));
                    }
                }
                AiSmartWorkflowMode::Local => {
                    return Err(invalid("本地模式不能绑定录制会话"));
                }
            }
        }
        let now = Utc::now().to_rfc3339();
        let connection = self.database.connection()?;
        let live_start_video_id = if input.mode == AiSmartWorkflowMode::Live {
            connection.query_row(
                "SELECT MAX(id) FROM videos WHERE session_id = ?1",
                [input.source_session_id],
                |row| row.get::<_, Option<i64>>(0),
            )?
        } else {
            None
        };
        connection.execute(
            r#"INSERT INTO ai_smart_workflows(
                   name, mode, status, stage, generation, source_session_id,
                   source_summary, provider, model_id, text_scope,
                   configuration_fingerprint, live_start_video_id, created_at, updated_at
               ) VALUES(?1, ?2, 'draft', 'preflight', 1, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10)"#,
            params![
                input.name.trim(),
                input.mode.as_str(),
                input.source_session_id,
                input.source_summary.trim(),
                input.provider.trim(),
                input.model_id.trim(),
                input.text_scope.trim(),
                input.configuration_fingerprint.trim(),
                live_start_video_id,
                now,
            ],
        )?;
        let id = connection.last_insert_rowid();
        drop(connection);
        self.get(id)
    }

    pub fn authorize(
        &self,
        workflow_id: i64,
        expected_generation: u32,
        authorization_digest: &str,
        configuration_fingerprint: &str,
    ) -> Result<AiSmartWorkflowDetail> {
        let digest = required(authorization_digest, "授权摘要")?;
        let fingerprint = required(configuration_fingerprint, "配置指纹")?;
        let now = Utc::now().to_rfc3339();
        let changed = self.database.connection()?.execute(
            r#"UPDATE ai_smart_workflows
               SET authorization_digest = ?1, authorized_at = ?2,
                   status = 'queued', stage = 'preflight', updated_at = ?2,
                   event_sequence = event_sequence + 1
               WHERE id = ?3 AND generation = ?4 AND status IN ('draft', 'paused', 'failed')
                 AND configuration_fingerprint = ?5"#,
            params![digest, now, workflow_id, expected_generation, fingerprint],
        )?;
        if changed == 0 {
            return Err(invalid("任务代次、配置或当前状态已变化，请重新加载后授权"));
        }
        self.get(workflow_id)
    }

    pub fn authorization_is_current(
        &self,
        workflow_id: i64,
        expected_generation: u32,
        configuration_fingerprint: &str,
    ) -> Result<bool> {
        let workflow = self.get(workflow_id)?.workflow;
        Ok(workflow.generation == expected_generation
            && workflow.authorization_digest.is_some()
            && workflow.configuration_fingerprint == configuration_fingerprint
            && !workflow.status.is_terminal())
    }

    pub fn list(&self) -> Result<Vec<AiSmartWorkflow>> {
        let connection = self.database.connection()?;
        let mut statement =
            connection.prepare(&workflow_select("ORDER BY updated_at DESC, id DESC"))?;
        statement
            .query_map([], map_workflow)?
            .map(parse_workflow_row)
            .collect()
    }

    pub fn list_active_live_sessions(&self) -> Result<Vec<AiActiveLiveSession>> {
        let connection = self.database.connection()?;
        let mut statement = connection.prepare(
            r#"SELECT session.id, streamer.name, session.started_at
               FROM recording_sessions session
               JOIN streamers streamer ON streamer.id = session.streamer_id
               WHERE session.ended_at IS NULL AND session.status = 'recording'
                 AND streamer.monitor_enabled = 1 AND streamer.archived = 0
               ORDER BY session.started_at DESC, session.id DESC"#,
        )?;
        Ok(statement
            .query_map([], |row| {
                Ok(AiActiveLiveSession {
                    session_id: row.get(0)?,
                    streamer_name: row.get(1)?,
                    started_at: row.get(2)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn list_replay_sessions(
        &self,
        search: Option<&str>,
        cursor: Option<&AiSmartReplaySessionCursor>,
        limit: usize,
    ) -> Result<AiSmartReplaySessionPage> {
        if let Some(cursor) = cursor
            && (cursor.session_id <= 0
                || chrono::DateTime::parse_from_rfc3339(&cursor.started_at).is_err())
        {
            return Err(invalid("直播回放分页游标无效"));
        }
        let search_pattern = replay_search_pattern(search)?;
        let page_size = limit.clamp(1, 50);
        let connection = self.database.connection()?;
        let mut statement = connection.prepare(
            r#"SELECT session.id, streamer.name, session.started_at,
                      COALESCE(session.ended_at, ''), workflow.id, workflow.status
               FROM recording_sessions session
               JOIN streamers streamer ON streamer.id = session.streamer_id
               LEFT JOIN ai_smart_workflows workflow ON workflow.id = (
                   SELECT candidate.id FROM ai_smart_workflows candidate
                   WHERE candidate.mode = 'replay'
                     AND candidate.source_session_id = session.id
                   ORDER BY candidate.id DESC LIMIT 1
               )
               WHERE session.ended_at IS NOT NULL
                 AND EXISTS (SELECT 1 FROM videos video WHERE video.session_id = session.id)
                 AND (
                     ?1 IS NULL
                     OR lower(streamer.name) LIKE ?1 ESCAPE '\'
                     OR lower(COALESCE(streamer.web_rid, '')) LIKE ?1 ESCAPE '\'
                     OR CAST(session.id AS TEXT) LIKE ?1 ESCAPE '\'
                     OR strftime('%Y-%m-%d %H:%M', session.started_at, 'localtime') LIKE ?1 ESCAPE '\'
                 )
                 AND (
                     ?2 IS NULL OR session.started_at < ?2
                     OR (session.started_at = ?2 AND session.id < ?3)
                 )
               ORDER BY session.started_at DESC, session.id DESC
               LIMIT ?4"#,
        )?;
        let rows = statement
            .query_map(
                params![
                    search_pattern.as_deref(),
                    cursor.map(|value| value.started_at.as_str()),
                    cursor.map(|value| value.session_id),
                    i64::try_from(page_size + 1).unwrap_or(51),
                ],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, Option<i64>>(4)?,
                        row.get::<_, Option<String>>(5)?,
                    ))
                },
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        drop(statement);
        drop(connection);

        let mut items = Vec::with_capacity(rows.len());
        for (session_id, streamer_name, started_at, ended_at, workflow_id, workflow_status) in rows
        {
            let videos = self.database.list_session_videos(session_id)?;
            items.push(AiSmartReplaySession {
                session_id,
                streamer_name,
                started_at,
                ended_at,
                video_count: videos.len(),
                total_duration_ms: videos
                    .iter()
                    .filter_map(|video| video.duration_seconds)
                    .filter_map(|seconds| u64::try_from(seconds).ok())
                    .map(|seconds| seconds.saturating_mul(1_000))
                    .sum(),
                unavailable_video_count: videos
                    .iter()
                    .filter(|video| {
                        video.status != "complete"
                            || video.ended_at.is_none()
                            || video.audio_present == Some(false)
                            || !Path::new(&video.path).is_file()
                    })
                    .count(),
                existing_workflow_id: workflow_id,
                existing_workflow_status: workflow_status
                    .as_deref()
                    .and_then(AiSmartWorkflowStatus::parse),
            });
        }
        let has_more = items.len() > page_size;
        items.truncate(page_size);
        let next_cursor = has_more.then(|| {
            let last = items.last().expect("回放下一页必须有当前页末项");
            AiSmartReplaySessionCursor {
                started_at: last.started_at.clone(),
                session_id: last.session_id,
            }
        });
        Ok(AiSmartReplaySessionPage { items, next_cursor })
    }

    pub fn get(&self, workflow_id: i64) -> Result<AiSmartWorkflowDetail> {
        let connection = self.database.connection()?;
        let workflow = connection
            .query_row(
                &workflow_select("WHERE id = ?1"),
                [workflow_id],
                map_workflow,
            )
            .optional()?
            .ok_or(AiRepositoryError::NotFound("智能成片任务"))
            .and_then(|row| parse_workflow_row(Ok(row)))?;
        let batches = connection
            .prepare(
                r#"SELECT id, workflow_id, position, video_id, source_fingerprint,
                          project_id, highlight_run_id, status, finalized_at,
                          last_error_code, last_error_message, created_at, updated_at
                   FROM ai_smart_workflow_batches WHERE workflow_id = ?1
                   ORDER BY position, id"#,
            )?
            .query_map([workflow_id], map_batch)?
            .map(parse_batch_row)
            .collect::<Result<Vec<_>>>()?;
        let attempts = connection
            .prepare(
                r#"SELECT id, workflow_id, batch_id, draft_generation, stage,
                          input_fingerprint, attempt_generation, status, progress,
                          result_kind, result_id, duration_ms, last_error_code,
                          last_error_message, created_at, updated_at
                   FROM ai_smart_stage_attempts WHERE workflow_id = ?1
                   ORDER BY id"#,
            )?
            .query_map([workflow_id], map_attempt)?
            .map(parse_attempt_row)
            .collect::<Result<Vec<_>>>()?;
        let drafts = connection
            .prepare(
                r#"SELECT id, workflow_id, generation, clip_project_id, ownership,
                          status, automation_project_version, frozen_project_version,
                          first_reviewable_at, created_at, updated_at
                   FROM ai_smart_drafts WHERE workflow_id = ?1
                   ORDER BY generation, id"#,
            )?
            .query_map([workflow_id], map_draft)?
            .map(parse_draft_row)
            .collect::<Result<Vec<_>>>()?;
        let (frozen_input_count, processed_input_count) = connection.query_row(
            r#"SELECT COUNT(input.id),
                      COALESCE(SUM(CASE WHEN input.status IN (
                          'completed', 'skipped', 'cancelled', 'failed'
                      ) THEN 1 ELSE 0 END), 0)
               FROM ai_smart_workflow_batches batch
               JOIN ai_project_inputs input ON input.project_id = batch.project_id
               WHERE batch.workflow_id = ?1"#,
            [workflow_id],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )?;
        Ok(AiSmartWorkflowDetail {
            workflow,
            batches,
            attempts,
            drafts,
            frozen_input_count: non_negative_u64(frozen_input_count, "冻结输入数量")?,
            processed_input_count: non_negative_u64(processed_input_count, "已处理输入数量")?,
        })
    }

    pub fn transition(
        &self,
        workflow_id: i64,
        expected_generation: u32,
        next_status: AiSmartWorkflowStatus,
        next_stage: AiSmartStage,
        error: Option<(&str, &str)>,
    ) -> Result<AiSmartWorkflowDetail> {
        let current = self.get(workflow_id)?.workflow;
        if current.generation != expected_generation {
            return Err(invalid("任务代次已变化"));
        }
        if !current.status.can_transition_to(next_status) {
            return Err(invalid(&format!(
                "智能任务不能从 {} 进入 {}",
                current.status.as_str(),
                next_status.as_str()
            )));
        }
        let (code, message) = sanitize_error(error);
        let now = Utc::now().to_rfc3339();
        let changed = self.database.connection()?.execute(
            r#"UPDATE ai_smart_workflows
               SET status = ?1, stage = ?2, last_error_code = ?3,
                   last_error_message = ?4, updated_at = ?5,
                   event_sequence = event_sequence + 1
               WHERE id = ?6 AND generation = ?7"#,
            params![
                next_status.as_str(),
                next_stage.as_str(),
                code,
                message,
                now,
                workflow_id,
                expected_generation,
            ],
        )?;
        if changed == 0 {
            return Err(invalid("任务代次已变化"));
        }
        self.get(workflow_id)
    }

    pub fn cancel(
        &self,
        workflow_id: i64,
        expected_generation: u32,
    ) -> Result<AiSmartWorkflowDetail> {
        let now = Utc::now().to_rfc3339();
        let mut connection = self.database.connection()?;
        let transaction = connection.transaction()?;
        let changed = transaction.execute(
            r#"UPDATE ai_smart_workflows
               SET status = 'cancelled', generation = generation + 1,
                   event_sequence = event_sequence + 1, updated_at = ?1
               WHERE id = ?2 AND generation = ?3 AND status NOT IN ('completed', 'cancelled')"#,
            params![now, workflow_id, expected_generation],
        )?;
        if changed == 0 {
            return Err(invalid("任务已结束或代次已变化"));
        }
        transaction.execute(
            r#"UPDATE ai_smart_workflow_batches SET status = 'cancelled', updated_at = ?1
               WHERE workflow_id = ?2 AND status IN ('pending', 'queued')"#,
            params![now, workflow_id],
        )?;
        transaction.execute(
            r#"UPDATE ai_smart_stage_attempts SET status = 'cancelled', updated_at = ?1
               WHERE workflow_id = ?2 AND status IN ('pending', 'running')"#,
            params![now, workflow_id],
        )?;
        transaction.commit()?;
        drop(connection);
        self.get(workflow_id)
    }

    pub fn add_batch(
        &self,
        workflow_id: i64,
        input: &NewAiSmartWorkflowBatch,
    ) -> Result<AiSmartWorkflowBatch> {
        let fingerprint = required(&input.source_fingerprint, "批次源指纹")?;
        let workflow_snapshot = self.get(workflow_id)?.workflow;
        if workflow_snapshot.status.is_terminal() {
            return Err(invalid("已结束任务不能追加批次"));
        }
        if workflow_snapshot.mode == AiSmartWorkflowMode::Live {
            let video_id = input
                .video_id
                .ok_or_else(|| invalid("直播批次必须绑定视频"))?;
            let video = self.database.get_video(video_id)?;
            if Some(video.session_id) != workflow_snapshot.source_session_id
                || video.status != "complete"
                || video.ended_at.is_none()
                || video.audio_present != Some(true)
            {
                return Err(invalid("直播批次只接受绑定会话中已完成且有音轨的分片"));
            }
        } else if workflow_snapshot.mode == AiSmartWorkflowMode::Replay && input.video_id.is_some()
        {
            return Err(invalid("直播回放使用整场冻结批次，不能追加单个视频"));
        }
        let mut connection = self.database.connection()?;
        let transaction = connection.transaction()?;
        let workflow = transaction
            .query_row(
                "SELECT mode, source_session_id, status FROM ai_smart_workflows WHERE id = ?1",
                [workflow_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<i64>>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()?
            .ok_or(AiRepositoryError::NotFound("智能成片任务"))?;
        if matches!(workflow.2.as_str(), "completed" | "cancelled") {
            return Err(invalid("已结束任务不能追加批次"));
        }
        if workflow.0 != workflow_snapshot.mode.as_str()
            || workflow.1 != workflow_snapshot.source_session_id
        {
            return Err(invalid("任务来源在批次创建前发生变化"));
        }
        if let Some(existing_id) = transaction
            .query_row(
                r#"SELECT id FROM ai_smart_workflow_batches
                   WHERE workflow_id = ?1 AND (source_fingerprint = ?2 OR (?3 IS NOT NULL AND video_id = ?3))"#,
                params![workflow_id, fingerprint, input.video_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
        {
            transaction.commit()?;
            drop(connection);
            return self.get_batch(existing_id);
        }
        let position = transaction.query_row(
            "SELECT COALESCE(MAX(position) + 1, 0) FROM ai_smart_workflow_batches WHERE workflow_id = ?1",
            [workflow_id],
            |row| row.get::<_, i64>(0),
        )?;
        let now = Utc::now().to_rfc3339();
        transaction.execute(
            r#"INSERT INTO ai_smart_workflow_batches(
                   workflow_id, position, video_id, source_fingerprint, status,
                   finalized_at, created_at, updated_at
               ) VALUES(?1, ?2, ?3, ?4, 'pending', ?5, ?6, ?6)"#,
            params![
                workflow_id,
                position,
                input.video_id,
                fingerprint,
                input.finalized_at,
                now
            ],
        )?;
        let id = transaction.last_insert_rowid();
        if workflow_snapshot.mode == AiSmartWorkflowMode::Live {
            normalize_live_batch_positions(&transaction, workflow_id)?;
        }
        transaction.execute(
            r#"UPDATE ai_smart_workflows
               SET pending_batch_count = pending_batch_count + 1,
                   event_sequence = event_sequence + 1, updated_at = ?1
               WHERE id = ?2"#,
            params![now, workflow_id],
        )?;
        transaction.commit()?;
        drop(connection);
        self.get_batch(id)
    }

    pub fn get_batch_for_video(
        &self,
        workflow_id: i64,
        video_id: i64,
    ) -> Result<Option<AiSmartWorkflowBatch>> {
        let id = self
            .database
            .connection()?
            .query_row(
                r#"SELECT id FROM ai_smart_workflow_batches
                   WHERE workflow_id = ?1 AND video_id = ?2"#,
                params![workflow_id, video_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        id.map(|id| self.get_batch(id)).transpose()
    }

    pub fn advance_live_cursor(&self, workflow_id: i64, video_id: i64) -> Result<()> {
        let changed = self.database.connection()?.execute(
            r#"UPDATE ai_smart_workflows
               SET live_cursor_video_id = CASE
                       WHEN live_cursor_video_id IS NULL OR live_cursor_video_id < ?1 THEN ?1
                       ELSE live_cursor_video_id END,
                   updated_at = ?2
               WHERE id = ?3 AND mode = 'live'"#,
            params![video_id, Utc::now().to_rfc3339(), workflow_id],
        )?;
        if changed == 0 {
            return Err(AiRepositoryError::NotFound("直播智能成片任务"));
        }
        Ok(())
    }

    pub fn resolve_candidate_dedupe_key(
        &self,
        workflow_id: i64,
        semantic_fingerprint: &str,
        stable_segment_ids: &[String],
        session_start_ms: u64,
        session_end_ms: u64,
    ) -> Result<String> {
        let connection = self.database.connection()?;
        let mut statement = connection.prepare(
            r#"SELECT candidate.id, candidate.dedupe_key, candidate.semantic_fingerprint,
                      candidate.session_start_ms, candidate.session_end_ms,
                      source.stable_segment_ids_json
               FROM ai_smart_candidates candidate
               LEFT JOIN ai_smart_candidate_sources source
                 ON source.smart_candidate_id = candidate.id
               WHERE candidate.workflow_id = ?1
               ORDER BY candidate.id, source.source_order"#,
        )?;
        let rows = statement
            .query_map([workflow_id], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, Option<String>>(5)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let incoming_segments = stable_segment_ids
            .iter()
            .collect::<std::collections::HashSet<_>>();
        for (_, key, semantic, start, end, segment_json) in rows {
            let same_core_segment = segment_json
                .as_deref()
                .and_then(|value| serde_json::from_str::<Vec<String>>(value).ok())
                .is_some_and(|segments| {
                    segments
                        .iter()
                        .any(|segment| incoming_segments.contains(segment))
                });
            let overlap_start = session_start_ms.max(u64::try_from(start).unwrap_or_default());
            let overlap_end = session_end_ms.min(u64::try_from(end).unwrap_or_default());
            let overlap = overlap_end.saturating_sub(overlap_start);
            let shorter = (session_end_ms - session_start_ms)
                .min(u64::try_from(end.saturating_sub(start)).unwrap_or_default());
            let same_semantics = semantic == semantic_fingerprint;
            let semantic_overlap = same_semantics
                && shorter > 0
                && overlap.saturating_mul(100) >= shorter.saturating_mul(70);
            let gap = if session_start_ms >= u64::try_from(end).unwrap_or_default() {
                session_start_ms - u64::try_from(end).unwrap_or_default()
            } else if u64::try_from(start).unwrap_or_default() >= session_end_ms {
                u64::try_from(start).unwrap_or_default() - session_end_ms
            } else {
                0
            };
            let semantic_continuation = same_semantics && gap <= 5_000;
            if same_core_segment || semantic_overlap || semantic_continuation {
                return Ok(key);
            }
        }
        let mut hasher = Sha256::new();
        hasher.update(semantic_fingerprint.as_bytes());
        hasher.update((session_start_ms / 5_000).to_le_bytes());
        hasher.update((session_end_ms / 5_000).to_le_bytes());
        for segment_id in stable_segment_ids {
            hasher.update(segment_id.as_bytes());
        }
        Ok(format!("smart_{}", &hex::encode(hasher.finalize())[..32]))
    }

    pub fn attach_batch_project(
        &self,
        batch_id: i64,
        project_id: i64,
        highlight_run_id: Option<i64>,
        next_status: AiSmartBatchStatus,
    ) -> Result<AiSmartWorkflowBatch> {
        let changed = self.database.connection()?.execute(
            r#"UPDATE ai_smart_workflow_batches
               SET project_id = ?1, highlight_run_id = ?2, status = ?3, updated_at = ?4
               WHERE id = ?5"#,
            params![
                project_id,
                highlight_run_id,
                next_status.as_str(),
                Utc::now().to_rfc3339(),
                batch_id,
            ],
        )?;
        if changed == 0 {
            return Err(AiRepositoryError::NotFound("智能成片批次"));
        }
        self.get_batch(batch_id)
    }

    pub fn update_batch_status(
        &self,
        batch_id: i64,
        next_status: AiSmartBatchStatus,
        error: Option<(&str, &str)>,
    ) -> Result<AiSmartWorkflowBatch> {
        let (code, message) = sanitize_error(error);
        let now = Utc::now().to_rfc3339();
        let mut connection = self.database.connection()?;
        let transaction = connection.transaction()?;
        let workflow_id = transaction
            .query_row(
                "SELECT workflow_id FROM ai_smart_workflow_batches WHERE id = ?1",
                [batch_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .ok_or(AiRepositoryError::NotFound("智能成片批次"))?;
        transaction.execute(
            r#"UPDATE ai_smart_workflow_batches
               SET status = ?1, last_error_code = ?2, last_error_message = ?3, updated_at = ?4
               WHERE id = ?5"#,
            params![next_status.as_str(), code, message, now, batch_id],
        )?;
        transaction.execute(
            r#"UPDATE ai_smart_workflows
               SET pending_batch_count = (
                       SELECT COUNT(*) FROM ai_smart_workflow_batches
                       WHERE workflow_id = ?1 AND status IN ('pending', 'queued', 'asr', 'highlight')
                   ), event_sequence = event_sequence + 1, updated_at = ?2
               WHERE id = ?1"#,
            params![workflow_id, now],
        )?;
        transaction.commit()?;
        drop(connection);
        self.get_batch(batch_id)
    }

    pub fn reset_batch_for_retry(
        &self,
        workflow_id: i64,
        batch_id: i64,
    ) -> Result<AiSmartWorkflowBatch> {
        let changed = self.database.connection()?.execute(
            r#"UPDATE ai_smart_workflow_batches
               SET status = 'queued', last_error_code = NULL, last_error_message = NULL,
                   updated_at = ?1
               WHERE id = ?2 AND workflow_id = ?3
                 AND status IN ('pending', 'queued', 'asr', 'highlight', 'failed')"#,
            params![Utc::now().to_rfc3339(), batch_id, workflow_id],
        )?;
        if changed == 0 {
            return Err(invalid("当前批次没有可重试的失败"));
        }
        self.update_batch_status(batch_id, AiSmartBatchStatus::Queued, None)
    }

    pub fn get_batch(&self, batch_id: i64) -> Result<AiSmartWorkflowBatch> {
        self.database
            .connection()?
            .query_row(
                r#"SELECT id, workflow_id, position, video_id, source_fingerprint,
                          project_id, highlight_run_id, status, finalized_at,
                          last_error_code, last_error_message, created_at, updated_at
                   FROM ai_smart_workflow_batches WHERE id = ?1"#,
                [batch_id],
                map_batch,
            )
            .optional()?
            .ok_or(AiRepositoryError::NotFound("智能成片批次"))
            .and_then(|row| parse_batch_row(Ok(row)))
    }

    pub fn start_attempt(&self, input: &SmartStageAttemptStart) -> Result<AiSmartStageAttempt> {
        let fingerprint = required(&input.input_fingerprint, "阶段输入指纹")?;
        let mut connection = self.database.connection()?;
        let transaction = connection.transaction()?;
        if let Some(row) = transaction
            .query_row(
                &format!(
                    "{} WHERE workflow_id = ?1 AND batch_id IS ?2 AND draft_generation IS ?3 AND stage = ?4 AND input_fingerprint = ?5 AND status = 'completed' ORDER BY attempt_generation DESC LIMIT 1",
                    attempt_select()
                ),
                params![
                    input.workflow_id,
                    input.batch_id,
                    input.draft_generation.map(i64::from),
                    input.stage.as_str(),
                    fingerprint,
                ],
                map_attempt,
            )
            .optional()?
        {
            transaction.commit()?;
            return parse_attempt_row(Ok(row));
        }
        let generation = transaction.query_row(
            r#"SELECT COALESCE(MAX(attempt_generation) + 1, 1)
               FROM ai_smart_stage_attempts
               WHERE workflow_id = ?1 AND batch_id IS ?2 AND draft_generation IS ?3 AND stage = ?4"#,
            params![
                input.workflow_id,
                input.batch_id,
                input.draft_generation.map(i64::from),
                input.stage.as_str(),
            ],
            |row| row.get::<_, i64>(0),
        )?;
        transaction.execute(
            r#"UPDATE ai_smart_stage_attempts SET status = 'interrupted', updated_at = ?1
               WHERE workflow_id = ?2 AND batch_id IS ?3 AND draft_generation IS ?4
                 AND stage = ?5 AND status IN ('pending', 'running')"#,
            params![
                Utc::now().to_rfc3339(),
                input.workflow_id,
                input.batch_id,
                input.draft_generation.map(i64::from),
                input.stage.as_str(),
            ],
        )?;
        let now = Utc::now().to_rfc3339();
        transaction.execute(
            r#"INSERT INTO ai_smart_stage_attempts(
                   workflow_id, batch_id, draft_generation, stage, input_fingerprint,
                   attempt_generation, status, progress, created_at, updated_at
               ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, 'running', 0, ?7, ?7)"#,
            params![
                input.workflow_id,
                input.batch_id,
                input.draft_generation.map(i64::from),
                input.stage.as_str(),
                fingerprint,
                generation,
                now,
            ],
        )?;
        let id = transaction.last_insert_rowid();
        transaction.commit()?;
        drop(connection);
        self.get_attempt(id)
    }

    pub fn update_attempt_progress(
        &self,
        attempt_id: i64,
        expected_attempt_generation: u32,
        progress: u8,
    ) -> Result<AiSmartStageAttempt> {
        if progress > 100 {
            return Err(invalid("阶段进度必须在 0 到 100 之间"));
        }
        let changed = self.database.connection()?.execute(
            r#"UPDATE ai_smart_stage_attempts
               SET progress = ?1, updated_at = ?2
               WHERE id = ?3 AND attempt_generation = ?4 AND status = 'running'"#,
            params![
                i64::from(progress),
                Utc::now().to_rfc3339(),
                attempt_id,
                expected_attempt_generation,
            ],
        )?;
        if changed == 0 {
            return Err(invalid("阶段尝试已经结束或代次已变化"));
        }
        self.get_attempt(attempt_id)
    }

    pub fn list_candidates(&self, workflow_id: i64) -> Result<Vec<AiSmartCandidate>> {
        let connection = self.database.connection()?;
        let mut statement = connection.prepare(
            r#"SELECT id FROM ai_smart_candidates WHERE workflow_id = ?1
               ORDER BY session_start_ms, session_end_ms, id"#,
        )?;
        let ids = statement
            .query_map([workflow_id], |row| row.get::<_, i64>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        drop(statement);
        drop(connection);
        ids.into_iter().map(|id| self.get_candidate(id)).collect()
    }

    #[allow(clippy::too_many_arguments)]
    pub fn finish_attempt(
        &self,
        attempt_id: i64,
        expected_attempt_generation: u32,
        status: AiSmartStageAttemptStatus,
        result: Option<(&str, i64)>,
        duration_ms: u64,
        error: Option<(&str, &str)>,
    ) -> Result<AiSmartStageAttempt> {
        if matches!(
            status,
            AiSmartStageAttemptStatus::Pending | AiSmartStageAttemptStatus::Running
        ) {
            return Err(invalid("阶段结束状态无效"));
        }
        let (result_kind, result_id) = result
            .map(|(kind, id)| (Some(kind), Some(id)))
            .unwrap_or((None, None));
        let (code, message) = sanitize_error(error);
        let changed = self.database.connection()?.execute(
            r#"UPDATE ai_smart_stage_attempts
               SET status = ?1, progress = CASE WHEN ?1 = 'completed' THEN 100 ELSE progress END,
                   result_kind = ?2, result_id = ?3, duration_ms = ?4,
                   last_error_code = ?5, last_error_message = ?6, updated_at = ?7
               WHERE id = ?8 AND attempt_generation = ?9 AND status = 'running'"#,
            params![
                status.as_str(),
                result_kind,
                result_id,
                i64::try_from(duration_ms).unwrap_or(i64::MAX),
                code,
                message,
                Utc::now().to_rfc3339(),
                attempt_id,
                expected_attempt_generation,
            ],
        )?;
        if changed == 0 {
            return Err(invalid("阶段尝试已经结束或代次已变化"));
        }
        self.get_attempt(attempt_id)
    }

    pub fn recover_interrupted(&self) -> Result<u64> {
        let now = Utc::now().to_rfc3339();
        let mut connection = self.database.connection()?;
        let transaction = connection.transaction()?;
        let changed = transaction.execute(
            r#"UPDATE ai_smart_stage_attempts
               SET status = 'interrupted', last_error_code = 'app_restarted',
                   last_error_message = '阶段因应用退出而中断，可从该阶段重试', updated_at = ?1
               WHERE status = 'running'"#,
            [now.clone()],
        )?;
        transaction.execute(
            r#"UPDATE ai_smart_workflows
               SET status = 'paused', generation = generation + 1,
                   last_error_code = 'app_restarted',
                   last_error_message = '智能任务因应用退出而暂停，重新验证后可恢复',
                   event_sequence = event_sequence + 1, updated_at = ?1
               WHERE status IN ('queued', 'running')"#,
            [now],
        )?;
        transaction.commit()?;
        Ok(changed as u64)
    }

    pub fn upsert_candidate(
        &self,
        workflow_id: i64,
        input: &NewAiSmartCandidate,
    ) -> Result<AiSmartCandidate> {
        validate_candidate_input(input)?;
        let mut connection = self.database.connection()?;
        let transaction = connection.transaction()?;
        let now = Utc::now().to_rfc3339();
        transaction.execute(
            r#"INSERT INTO ai_smart_candidates(
                   workflow_id, dedupe_key, semantic_fingerprint, canonical_candidate_id,
                   total_score, qualified, selected, session_start_ms, session_end_ms,
                   first_finalized_at, created_at, updated_at
               ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?11)
               ON CONFLICT(workflow_id, dedupe_key) DO UPDATE SET
                   canonical_candidate_id = CASE
                       WHEN excluded.total_score > total_score THEN excluded.canonical_candidate_id
                       ELSE canonical_candidate_id END,
                   total_score = MAX(total_score, excluded.total_score),
                   qualified = MAX(qualified, excluded.qualified),
                   selected = MAX(selected, excluded.selected),
                   session_start_ms = MIN(session_start_ms, excluded.session_start_ms),
                   session_end_ms = MAX(session_end_ms, excluded.session_end_ms),
                   updated_at = excluded.updated_at"#,
            params![
                workflow_id,
                input.dedupe_key.trim(),
                input.semantic_fingerprint.trim(),
                input.canonical_candidate_id,
                i64::from(input.total_score),
                input.qualified,
                input.selected,
                i64::try_from(input.session_start_ms).unwrap_or(i64::MAX),
                i64::try_from(input.session_end_ms).unwrap_or(i64::MAX),
                input.first_finalized_at,
                now,
            ],
        )?;
        let id = transaction.query_row(
            "SELECT id FROM ai_smart_candidates WHERE workflow_id = ?1 AND dedupe_key = ?2",
            params![workflow_id, input.dedupe_key.trim()],
            |row| row.get::<_, i64>(0),
        )?;
        for (source_order, source) in input.sources.iter().enumerate() {
            transaction.execute(
                r#"INSERT OR IGNORE INTO ai_smart_candidate_sources(
                       smart_candidate_id, batch_id, candidate_id, video_id, input_id,
                       stable_segment_ids_json, source_start_ms, source_end_ms,
                       session_start_ms, session_end_ms, source_order, created_at
                   ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)"#,
                params![
                    id,
                    source.batch_id,
                    source.candidate_id,
                    source.video_id,
                    source.input_id,
                    serde_json::to_string(&source.stable_segment_ids)
                        .map_err(|error| AiRepositoryError::Serialization(error.to_string()))?,
                    i64::try_from(source.source_start_ms).unwrap_or(i64::MAX),
                    i64::try_from(source.source_end_ms).unwrap_or(i64::MAX),
                    i64::try_from(source.session_start_ms).unwrap_or(i64::MAX),
                    i64::try_from(source.session_end_ms).unwrap_or(i64::MAX),
                    i64::try_from(source_order).unwrap_or(i64::MAX),
                    now,
                ],
            )?;
        }
        let (candidate_count, selected_count) = transaction.query_row(
            r#"SELECT COUNT(*), COALESCE(SUM(selected), 0)
               FROM ai_smart_candidates WHERE workflow_id = ?1"#,
            [workflow_id],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )?;
        transaction.execute(
            r#"UPDATE ai_smart_workflows SET candidate_count = ?1, selected_count = ?2,
                   event_sequence = event_sequence + 1, updated_at = ?3 WHERE id = ?4"#,
            params![candidate_count, selected_count, now, workflow_id],
        )?;
        transaction.commit()?;
        drop(connection);
        self.get_candidate(id)
    }

    pub fn list_candidate_sources(
        &self,
        smart_candidate_id: i64,
    ) -> Result<Vec<AiSmartCandidateSource>> {
        self.database
            .connection()?
            .prepare(
                r#"SELECT id, smart_candidate_id, batch_id, candidate_id, video_id,
                          input_id, stable_segment_ids_json, source_start_ms, source_end_ms,
                          session_start_ms, session_end_ms, source_order
                   FROM ai_smart_candidate_sources WHERE smart_candidate_id = ?1
                   ORDER BY source_order, id"#,
            )?
            .query_map([smart_candidate_id], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, Option<i64>>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, i64>(8)?,
                    row.get::<_, i64>(9)?,
                    row.get::<_, i64>(10)?,
                    row.get::<_, i64>(11)?,
                ))
            })?
            .map(|row| {
                let row = row?;
                Ok(AiSmartCandidateSource {
                    id: row.0,
                    smart_candidate_id: row.1,
                    batch_id: row.2,
                    candidate_id: row.3,
                    video_id: row.4,
                    input_id: row.5,
                    stable_segment_ids: serde_json::from_str(&row.6).map_err(|_| {
                        AiRepositoryError::Integrity("候选来源句段数据损坏".to_owned())
                    })?,
                    source_start_ms: non_negative_u64(row.7, "候选源开始时间")?,
                    source_end_ms: positive_u64(row.8, "候选源结束时间")?,
                    session_start_ms: non_negative_u64(row.9, "候选会话开始时间")?,
                    session_end_ms: positive_u64(row.10, "候选会话结束时间")?,
                    source_order: non_negative_u32(row.11, "候选来源顺序")?,
                })
            })
            .collect()
    }

    pub fn list_drafts(&self, workflow_id: i64) -> Result<Vec<AiSmartDraft>> {
        self.get(workflow_id).map(|detail| detail.drafts)
    }

    pub fn selected_clip_inputs(
        &self,
        workflow_id: i64,
    ) -> Result<(Vec<super::AiSmartClipSourceInput>, Vec<i64>)> {
        let connection = self.database.connection()?;
        let mut statement = connection.prepare(
            r#"SELECT smart.id, smart.canonical_candidate_id,
                      batch.highlight_run_id, source.batch_id, source.candidate_id,
                      source.session_start_ms, source.session_end_ms
               FROM ai_smart_candidates smart
               JOIN ai_smart_candidate_sources source
                 ON source.smart_candidate_id = smart.id
               JOIN ai_smart_workflow_batches batch ON batch.id = source.batch_id
               JOIN ai_highlight_candidates candidate ON candidate.id = source.candidate_id
               WHERE smart.workflow_id = ?1 AND smart.selected = 1
                 AND candidate.selected = 1 AND batch.highlight_run_id IS NOT NULL
               ORDER BY smart.session_start_ms, smart.id,
                        source.session_start_ms, source.session_end_ms, source.candidate_id"#,
        )?;
        let rows = statement
            .query_map([workflow_id], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        drop(statement);
        drop(connection);

        let mut grouped = Vec::<Vec<(i64, i64, i64, i64, i64, u64, u64)>>::new();
        for (smart_id, canonical_id, highlight_run_id, batch_id, candidate_id, start, end) in rows {
            let start = non_negative_u64(start, "候选会话开始时间")?;
            let end = positive_u64(end, "候选会话结束时间")?;
            if grouped
                .last()
                .and_then(|group| group.first())
                .is_none_or(|row| row.0 != smart_id)
            {
                grouped.push(Vec::new());
            }
            grouped.last_mut().expect("刚创建候选分组").push((
                smart_id,
                canonical_id,
                highlight_run_id,
                batch_id,
                candidate_id,
                start,
                end,
            ));
        }

        let mut selected_rows = Vec::new();
        for mut group in grouped {
            let canonical_id = group[0].1;
            let Some(canonical) = group.iter().find(|row| row.4 == canonical_id).cloned() else {
                return Err(AiRepositoryError::Integrity(
                    "智能候选规范来源缺失".to_owned(),
                ));
            };
            let mut retained = vec![canonical];
            for row in group.drain(..) {
                if row.4 == canonical_id
                    || retained
                        .iter()
                        .any(|kept| ranges_duplicate(row.5, row.6, kept.5, kept.6))
                {
                    continue;
                }
                retained.push(row);
            }
            retained.sort_by_key(|row| (row.5, row.6, row.4));
            selected_rows.extend(retained);
        }
        selected_rows.sort_by_key(|row| (row.5, row.6, row.4));

        let mut source_runs = HashSet::new();
        let mut candidates = HashSet::new();
        let mut sources = Vec::new();
        let mut candidate_ids = Vec::new();
        for (_, _, highlight_run_id, workflow_batch_id, candidate_id, _, _) in selected_rows {
            if source_runs.insert(highlight_run_id) {
                sources.push(super::AiSmartClipSourceInput {
                    highlight_run_id,
                    workflow_batch_id,
                });
            }
            if candidates.insert(candidate_id) {
                candidate_ids.push(candidate_id);
            }
        }
        Ok((sources, candidate_ids))
    }

    fn get_attempt(&self, attempt_id: i64) -> Result<AiSmartStageAttempt> {
        self.database
            .connection()?
            .query_row(
                &format!("{} WHERE id = ?1", attempt_select()),
                [attempt_id],
                map_attempt,
            )
            .optional()?
            .ok_or(AiRepositoryError::NotFound("智能阶段尝试"))
            .and_then(|row| parse_attempt_row(Ok(row)))
    }

    fn get_candidate(&self, candidate_id: i64) -> Result<AiSmartCandidate> {
        let row = self
            .database
            .connection()?
            .query_row(
                r#"SELECT id, workflow_id, canonical_candidate_id, total_score,
                          qualified, selected, session_start_ms, session_end_ms,
                          first_finalized_at
                   FROM ai_smart_candidates WHERE id = ?1"#,
                [candidate_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, bool>(4)?,
                        row.get::<_, bool>(5)?,
                        row.get::<_, i64>(6)?,
                        row.get::<_, i64>(7)?,
                        row.get::<_, String>(8)?,
                    ))
                },
            )
            .optional()?
            .ok_or(AiRepositoryError::NotFound("智能高光候选"))?;
        Ok(AiSmartCandidate {
            id: row.0,
            workflow_id: row.1,
            canonical_candidate_id: row.2,
            total_score: u8::try_from(row.3).map_err(|_| invalid("智能候选分数损坏"))?,
            qualified: row.4,
            selected: row.5,
            session_start_ms: non_negative_u64(row.6, "候选会话开始时间")?,
            session_end_ms: positive_u64(row.7, "候选会话结束时间")?,
            first_finalized_at: row.8,
        })
    }
}

type WorkflowRow = (
    i64,
    String,
    String,
    String,
    String,
    i64,
    Option<i64>,
    String,
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    String,
    Option<i64>,
    Option<i64>,
    Option<i64>,
    i64,
    i64,
    i64,
    i64,
    Option<String>,
    Option<String>,
    String,
    String,
);

fn workflow_select(suffix: &str) -> String {
    format!(
        r#"SELECT id, name, mode, status, stage, generation, source_session_id,
                  source_summary, provider, model_id, text_scope, authorization_digest,
                  authorized_at, configuration_fingerprint, active_draft_generation,
                  live_cursor_video_id, live_start_video_id, event_sequence, candidate_count, selected_count,
                  pending_batch_count, last_error_code, last_error_message, created_at, updated_at
           FROM ai_smart_workflows {suffix}"#
    )
}

fn map_workflow(row: &rusqlite::Row<'_>) -> rusqlite::Result<WorkflowRow> {
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
        row.get(22)?,
        row.get(23)?,
        row.get(24)?,
    ))
}

fn parse_workflow_row(row: rusqlite::Result<WorkflowRow>) -> Result<AiSmartWorkflow> {
    let row = row?;
    Ok(AiSmartWorkflow {
        id: row.0,
        name: row.1,
        mode: AiSmartWorkflowMode::parse(&row.2).ok_or_else(|| invalid("任务模式损坏"))?,
        status: AiSmartWorkflowStatus::parse(&row.3).ok_or_else(|| invalid("任务状态损坏"))?,
        stage: AiSmartStage::parse(&row.4).ok_or_else(|| invalid("任务阶段损坏"))?,
        generation: positive_u32(row.5, "任务代次")?,
        source_session_id: row.6,
        source_summary: row.7,
        provider: row.8,
        model_id: row.9,
        text_scope: row.10,
        authorization_digest: row.11,
        authorized_at: row.12,
        configuration_fingerprint: row.13,
        active_draft_generation: optional_positive_u32(row.14, "活动草稿代次")?,
        live_cursor_video_id: row.15,
        live_start_video_id: row.16,
        event_sequence: non_negative_u64(row.17, "事件序号")?,
        candidate_count: non_negative_u64(row.18, "候选数量")?,
        selected_count: non_negative_u64(row.19, "入选数量")?,
        pending_batch_count: non_negative_u64(row.20, "积压批次")?,
        last_error_code: row.21,
        last_error_message: row.22,
        created_at: row.23,
        updated_at: row.24,
    })
}

type BatchRow = (
    i64,
    i64,
    i64,
    Option<i64>,
    String,
    Option<i64>,
    Option<i64>,
    String,
    String,
    Option<String>,
    Option<String>,
    String,
    String,
);

fn map_batch(row: &rusqlite::Row<'_>) -> rusqlite::Result<BatchRow> {
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
    ))
}

fn parse_batch_row(row: rusqlite::Result<BatchRow>) -> Result<AiSmartWorkflowBatch> {
    let row = row?;
    Ok(AiSmartWorkflowBatch {
        id: row.0,
        workflow_id: row.1,
        position: non_negative_u32(row.2, "批次顺序")?,
        video_id: row.3,
        source_fingerprint: row.4,
        project_id: row.5,
        highlight_run_id: row.6,
        status: AiSmartBatchStatus::parse(&row.7).ok_or_else(|| invalid("批次状态损坏"))?,
        finalized_at: row.8,
        last_error_code: row.9,
        last_error_message: row.10,
        created_at: row.11,
        updated_at: row.12,
    })
}

type AttemptRow = (
    i64,
    i64,
    Option<i64>,
    Option<i64>,
    String,
    String,
    i64,
    String,
    i64,
    Option<String>,
    Option<i64>,
    Option<i64>,
    Option<String>,
    Option<String>,
    String,
    String,
);

fn attempt_select() -> &'static str {
    r#"SELECT id, workflow_id, batch_id, draft_generation, stage, input_fingerprint,
              attempt_generation, status, progress, result_kind, result_id, duration_ms,
              last_error_code, last_error_message, created_at, updated_at
       FROM ai_smart_stage_attempts"#
}

fn map_attempt(row: &rusqlite::Row<'_>) -> rusqlite::Result<AttemptRow> {
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

fn parse_attempt_row(row: rusqlite::Result<AttemptRow>) -> Result<AiSmartStageAttempt> {
    let row = row?;
    Ok(AiSmartStageAttempt {
        id: row.0,
        workflow_id: row.1,
        batch_id: row.2,
        draft_generation: optional_positive_u32(row.3, "尝试草稿代次")?,
        stage: AiSmartStage::parse(&row.4).ok_or_else(|| invalid("阶段类型损坏"))?,
        input_fingerprint: row.5,
        attempt_generation: positive_u32(row.6, "阶段尝试代次")?,
        status: AiSmartStageAttemptStatus::parse(&row.7)
            .ok_or_else(|| invalid("阶段尝试状态损坏"))?,
        progress: u8::try_from(row.8).map_err(|_| invalid("阶段进度损坏"))?,
        result_kind: row.9,
        result_id: row.10,
        duration_ms: optional_non_negative_u64(row.11, "阶段耗时")?,
        last_error_code: row.12,
        last_error_message: row.13,
        created_at: row.14,
        updated_at: row.15,
    })
}

type DraftRow = (
    i64,
    i64,
    i64,
    i64,
    String,
    String,
    i64,
    Option<i64>,
    Option<String>,
    String,
    String,
);

fn map_draft(row: &rusqlite::Row<'_>) -> rusqlite::Result<DraftRow> {
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

fn parse_draft_row(row: rusqlite::Result<DraftRow>) -> Result<AiSmartDraft> {
    let row = row?;
    Ok(AiSmartDraft {
        id: row.0,
        workflow_id: row.1,
        generation: positive_u32(row.2, "草稿代次")?,
        clip_project_id: row.3,
        ownership: AiSmartDraftOwnership::parse(&row.4).ok_or_else(|| invalid("草稿所有权损坏"))?,
        status: AiSmartDraftStatus::parse(&row.5).ok_or_else(|| invalid("草稿状态损坏"))?,
        automation_project_version: positive_u32(row.6, "自动草稿工程版本")?,
        frozen_project_version: optional_positive_u32(row.7, "冻结工程版本")?,
        first_reviewable_at: row.8,
        created_at: row.9,
        updated_at: row.10,
    })
}

fn validate_workflow_input(input: &NewAiSmartWorkflow) -> Result<()> {
    required(&input.name, "任务名称")?;
    required(&input.source_summary, "来源摘要")?;
    required(&input.provider, "Provider")?;
    required(&input.model_id, "模型")?;
    required(&input.text_scope, "文本范围")?;
    required(&input.configuration_fingerprint, "配置指纹")?;
    if (input.mode != AiSmartWorkflowMode::Local) != input.source_session_id.is_some() {
        return Err(invalid("直播模式必须且仅能绑定录制会话"));
    }
    Ok(())
}

fn replay_search_pattern(search: Option<&str>) -> Result<Option<String>> {
    let Some(search) = search.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    if search.chars().count() > 128 || search.chars().any(char::is_control) {
        return Err(invalid("直播回放搜索条件无效"));
    }
    let escaped = search
        .to_lowercase()
        .replace('\\', r"\\")
        .replace('%', r"\%")
        .replace('_', r"\_");
    Ok(Some(format!("%{escaped}%")))
}

fn validate_candidate_input(input: &NewAiSmartCandidate) -> Result<()> {
    required(&input.dedupe_key, "候选去重键")?;
    required(&input.semantic_fingerprint, "候选语义指纹")?;
    if input.session_end_ms <= input.session_start_ms || input.sources.is_empty() {
        return Err(invalid("候选必须包含递增的真实来源区间"));
    }
    for source in &input.sources {
        if source.source_end_ms <= source.source_start_ms
            || source.session_end_ms <= source.session_start_ms
            || source.stable_segment_ids.is_empty()
            || source
                .stable_segment_ids
                .iter()
                .any(|id| id.trim().is_empty())
        {
            return Err(invalid("候选来源区间或稳定句段无效"));
        }
    }
    Ok(())
}

fn required<'a>(value: &'a str, label: &str) -> Result<&'a str> {
    let value = value.trim();
    if value.is_empty() {
        Err(invalid(&format!("{label}不能为空")))
    } else {
        Ok(value)
    }
}

fn sanitize_error(error: Option<(&str, &str)>) -> (Option<String>, Option<String>) {
    error
        .map(|(code, message)| {
            (
                Some(code.trim().chars().take(64).collect()),
                Some(
                    message
                        .trim()
                        .chars()
                        .filter(|character| !character.is_control())
                        .take(256)
                        .collect(),
                ),
            )
        })
        .unwrap_or((None, None))
}

fn positive_u32(value: i64, label: &str) -> Result<u32> {
    u32::try_from(value)
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| invalid(&format!("{label}损坏")))
}

fn non_negative_u32(value: i64, label: &str) -> Result<u32> {
    u32::try_from(value).map_err(|_| invalid(&format!("{label}损坏")))
}

fn optional_positive_u32(value: Option<i64>, label: &str) -> Result<Option<u32>> {
    value.map(|value| positive_u32(value, label)).transpose()
}

fn non_negative_u64(value: i64, label: &str) -> Result<u64> {
    u64::try_from(value).map_err(|_| invalid(&format!("{label}损坏")))
}

fn positive_u64(value: i64, label: &str) -> Result<u64> {
    u64::try_from(value)
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| invalid(&format!("{label}损坏")))
}

fn optional_non_negative_u64(value: Option<i64>, label: &str) -> Result<Option<u64>> {
    value
        .map(|value| non_negative_u64(value, label))
        .transpose()
}

fn invalid(message: &str) -> AiRepositoryError {
    AiRepositoryError::InvalidState(message.to_owned())
}

fn normalize_live_batch_positions(
    transaction: &rusqlite::Transaction<'_>,
    workflow_id: i64,
) -> Result<()> {
    let ids = transaction
        .prepare(
            r#"SELECT batch.id
               FROM ai_smart_workflow_batches batch
               LEFT JOIN videos video ON video.id = batch.video_id
               LEFT JOIN recording_sessions session ON session.id = video.session_id
               WHERE batch.workflow_id = ?1
               ORDER BY COALESCE(video.started_at, session.started_at, batch.finalized_at),
                        COALESCE(video.ended_at, batch.finalized_at), video.id, batch.id"#,
        )?
        .query_map([workflow_id], |row| row.get::<_, i64>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    transaction.execute(
        r#"UPDATE ai_smart_workflow_batches
           SET position = position + 1000000000 WHERE workflow_id = ?1"#,
        [workflow_id],
    )?;
    for (position, id) in ids.into_iter().enumerate() {
        transaction.execute(
            "UPDATE ai_smart_workflow_batches SET position = ?1 WHERE id = ?2",
            params![i64::try_from(position).unwrap_or(i64::MAX), id],
        )?;
    }
    Ok(())
}

fn ranges_duplicate(left_start: u64, left_end: u64, right_start: u64, right_end: u64) -> bool {
    let overlap = left_end
        .min(right_end)
        .saturating_sub(left_start.max(right_start));
    let shorter = left_end
        .saturating_sub(left_start)
        .min(right_end.saturating_sub(right_start));
    shorter > 0 && overlap.saturating_mul(100) >= shorter.saturating_mul(70)
}

#[derive(Debug, Error, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
#[error("{message}")]
pub struct SmartWorkflowError {
    pub code: String,
    pub message: String,
    pub retryable: bool,
}

impl SmartWorkflowError {
    pub fn new(code: &str, message: impl Into<String>, retryable: bool) -> Self {
        Self {
            code: code.to_owned(),
            message: sanitize_public_message(&message.into()),
            retryable,
        }
    }
}

pub type SmartWorkflowResult<T> = std::result::Result<T, SmartWorkflowError>;

pub trait SmartWorkflowGate: Send + Sync {
    fn activation_ready(&self) -> bool;
    fn provider_ready(&self) -> bool;
}

pub trait SmartWorkflowPublisher: Send + Sync {
    fn publish(&self, event: &AiSmartWorkflowEvent);
}

pub trait SmartWorkflowTelemetry: Send + Sync {
    fn record(&self, metric: &AiSmartWorkflowMetric);
}

#[derive(Default)]
pub struct SilentSmartWorkflowTelemetry;

impl SmartWorkflowTelemetry for SilentSmartWorkflowTelemetry {
    fn record(&self, _metric: &AiSmartWorkflowMetric) {}
}

#[derive(Default)]
pub struct SilentSmartWorkflowPublisher;

impl SmartWorkflowPublisher for SilentSmartWorkflowPublisher {
    fn publish(&self, _event: &AiSmartWorkflowEvent) {}
}

#[derive(Clone)]
pub struct SmartClippingWorkflow {
    repository: AiRepository,
    workflows: SmartWorkflowRepository,
    project_service: AiProjectService,
    controller: Arc<dyn AiJobController>,
    highlight: Arc<HighlightWorkflow>,
    correction: ClipTextCorrectionWorkflow,
    transition: TransitionMatchingWorkflow,
    gate: Arc<dyn SmartWorkflowGate>,
    publisher: Arc<dyn SmartWorkflowPublisher>,
    telemetry: Arc<dyn SmartWorkflowTelemetry>,
    tasks: Arc<Mutex<HashMap<i64, SmartWorkflowTask>>>,
}

struct SmartWorkflowTask {
    cancellation: CancellationToken,
    rerun_requested: bool,
}

impl SmartClippingWorkflow {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        database: Database,
        project_service: AiProjectService,
        controller: Arc<dyn AiJobController>,
        highlight: Arc<HighlightWorkflow>,
        correction: ClipTextCorrectionWorkflow,
        transition: TransitionMatchingWorkflow,
        gate: Arc<dyn SmartWorkflowGate>,
    ) -> Self {
        Self {
            repository: AiRepository::new(database.clone()),
            workflows: SmartWorkflowRepository::new(database),
            project_service,
            controller,
            highlight,
            correction,
            transition,
            gate,
            publisher: Arc::new(SilentSmartWorkflowPublisher),
            telemetry: Arc::new(SilentSmartWorkflowTelemetry),
            tasks: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn with_publisher(mut self, publisher: Arc<dyn SmartWorkflowPublisher>) -> Self {
        self.publisher = publisher;
        self
    }

    pub fn with_telemetry(mut self, telemetry: Arc<dyn SmartWorkflowTelemetry>) -> Self {
        self.telemetry = telemetry;
        self
    }

    pub fn repository(&self) -> &SmartWorkflowRepository {
        &self.workflows
    }

    pub async fn create_local(
        &self,
        configuration: SmartWorkflowConfiguration,
        trusted_files: Vec<TrustedLocalFile>,
        authorization_confirmed: bool,
    ) -> SmartWorkflowResult<AiSmartWorkflowDetail> {
        if !authorization_confirmed {
            return Err(SmartWorkflowError::new(
                "smart_authorization_required",
                "必须确认当前任务的来源、Provider 和最小文本发送范围",
                false,
            ));
        }
        validate_configuration(&configuration)?;
        self.validate_gates(true).await?;
        let profile = self.controller.default_profile().map_err(command_error)?;
        let project = self
            .project_service
            .create_draft(&configuration.name, &profile)
            .map_err(service_error)?;
        let imported = self
            .project_service
            .import_local_files(project.id, trusted_files, CancellationToken::new())
            .await
            .map_err(service_error)?;
        if imported.added.is_empty() {
            return Err(SmartWorkflowError::new(
                "smart_no_valid_input",
                "选择的视频均不可用于智能成片",
                false,
            ));
        }
        let source_fingerprint = aggregate_input_fingerprint(
            imported
                .added
                .iter()
                .map(|input| input.source_fingerprint_hash.as_str()),
        );
        let configuration_fingerprint = configuration_fingerprint(
            AiSmartWorkflowMode::Local,
            None,
            &configuration,
            &source_fingerprint,
        )?;
        let created = self
            .workflows
            .create(&NewAiSmartWorkflow {
                name: configuration.name.clone(),
                mode: AiSmartWorkflowMode::Local,
                source_session_id: None,
                source_summary: format!("{} 个本地视频", imported.added.len()),
                provider: configuration.provider.clone(),
                model_id: configuration.model_id.clone(),
                text_scope: configuration.text_scope.clone(),
                configuration_fingerprint: configuration_fingerprint.clone(),
            })
            .map_err(repository_error)?;
        let authorized = self
            .workflows
            .authorize(
                created.workflow.id,
                created.workflow.generation,
                &authorization_digest(created.workflow.id, &configuration_fingerprint),
                &configuration_fingerprint,
            )
            .map_err(repository_error)?;
        let batch = self
            .workflows
            .add_batch(
                created.workflow.id,
                &NewAiSmartWorkflowBatch {
                    video_id: None,
                    source_fingerprint,
                    finalized_at: Utc::now().to_rfc3339(),
                },
            )
            .map_err(repository_error)?;
        self.project_service
            .start_analysis(project.id)
            .await
            .map_err(|error| self.fail_before_spawn(created.workflow.id, error))?;
        self.controller
            .enqueue_project(project.id)
            .await
            .map_err(|error| self.fail_command_before_spawn(created.workflow.id, error))?;
        self.workflows
            .attach_batch_project(batch.id, project.id, None, AiSmartBatchStatus::Queued)
            .map_err(repository_error)?;
        self.publish_current(created.workflow.id, Some(batch.id));
        self.spawn(created.workflow.id, authorized.workflow.generation)?;
        self.workflows
            .get(created.workflow.id)
            .map_err(repository_error)
    }

    pub async fn create_live(
        &self,
        configuration: SmartWorkflowConfiguration,
        session_id: i64,
        authorization_confirmed: bool,
    ) -> SmartWorkflowResult<AiSmartWorkflowDetail> {
        if !authorization_confirmed {
            return Err(SmartWorkflowError::new(
                "smart_authorization_required",
                "必须确认当前直播场次的 Provider 和最小文本发送范围",
                false,
            ));
        }
        validate_configuration(&configuration)?;
        self.validate_gates(true).await?;
        let session = self
            .workflows
            .database
            .get_session(session_id)
            .map_err(|_| {
                SmartWorkflowError::new(
                    "smart_live_session_unavailable",
                    "选择的直播会话不存在或已不可用",
                    false,
                )
            })?;
        if session.ended_at.is_some() || session.status != "recording" {
            return Err(SmartWorkflowError::new(
                "smart_live_session_not_recording",
                "只能选择系统当前正在录制的直播会话",
                false,
            ));
        }
        let configuration_fingerprint = configuration_fingerprint(
            AiSmartWorkflowMode::Live,
            Some(session_id),
            &configuration,
            "live-finalized-segments",
        )?;
        let created = self
            .workflows
            .create(&NewAiSmartWorkflow {
                name: configuration.name,
                mode: AiSmartWorkflowMode::Live,
                source_session_id: Some(session_id),
                source_summary: "1 个正在录制的受信直播会话".to_owned(),
                provider: configuration.provider,
                model_id: configuration.model_id,
                text_scope: configuration.text_scope,
                configuration_fingerprint: configuration_fingerprint.clone(),
            })
            .map_err(repository_error)?;
        let detail = self
            .workflows
            .authorize(
                created.workflow.id,
                created.workflow.generation,
                &authorization_digest(created.workflow.id, &configuration_fingerprint),
                &configuration_fingerprint,
            )
            .map_err(repository_error)?;
        self.publish_current(created.workflow.id, None);
        Ok(detail)
    }

    pub async fn create_replay(
        &self,
        configuration: SmartWorkflowConfiguration,
        session_id: i64,
        authorization_confirmed: bool,
        duplicate_confirmed: bool,
    ) -> SmartWorkflowResult<AiSmartWorkflowDetail> {
        if !authorization_confirmed {
            return Err(SmartWorkflowError::new(
                "smart_authorization_required",
                "必须确认当前直播回放任务的 Provider 和最小文本发送范围",
                false,
            ));
        }
        validate_configuration(&configuration)?;
        self.validate_gates(true).await?;
        let existing = self
            .workflows
            .list()
            .map_err(repository_error)?
            .into_iter()
            .find(|workflow| {
                workflow.mode == AiSmartWorkflowMode::Replay
                    && workflow.source_session_id == Some(session_id)
            });
        if existing.is_some() && !duplicate_confirmed {
            return Err(SmartWorkflowError::new(
                "smart_replay_duplicate_confirmation_required",
                "该场回放已有智能成片任务，确认重新处理后再创建",
                false,
            ));
        }
        let session = self
            .workflows
            .database
            .get_session(session_id)
            .map_err(|_| {
                SmartWorkflowError::new(
                    "smart_replay_session_unavailable",
                    "选择的直播回放不存在或已不可用",
                    false,
                )
            })?;
        if session.ended_at.is_none() {
            return Err(SmartWorkflowError::new(
                "smart_replay_session_still_recording",
                "该场直播仍在录制，请改用正在直播模式",
                false,
            ));
        }
        let frozen_videos = self
            .workflows
            .database
            .list_session_videos(session_id)
            .map_err(|_| {
                SmartWorkflowError::new(
                    "smart_replay_session_unavailable",
                    "无法读取该场直播的本机录像分片",
                    true,
                )
            })?;
        if frozen_videos.is_empty() {
            return Err(SmartWorkflowError::new(
                "smart_replay_session_empty",
                "该场直播没有可用于智能成片的本机录像分片",
                false,
            ));
        }

        let profile = self.controller.default_profile().map_err(command_error)?;
        let project = self
            .project_service
            .create_draft(&configuration.name, &profile)
            .map_err(service_error)?;
        let imported = match self
            .project_service
            .select_completed_session(project.id, session_id, CancellationToken::new())
            .await
        {
            Ok(imported) => imported,
            Err(error) => {
                let _ = self.repository.delete_project(project.id);
                return Err(service_error(error));
            }
        };
        if imported.added.is_empty()
            || imported.unavailable_count != 0
            || imported.added_count != frozen_videos.len()
        {
            let _ = self.repository.delete_project(project.id);
            return Err(SmartWorkflowError::new(
                "smart_replay_source_incomplete",
                "直播回放包含缺失、未完成或无音轨分片，修复录像后再试",
                false,
            ));
        }
        let source_fingerprint = aggregate_input_fingerprint(
            imported
                .added
                .iter()
                .map(|input| input.source_fingerprint_hash.as_str()),
        );
        let configuration_fingerprint = configuration_fingerprint(
            AiSmartWorkflowMode::Replay,
            Some(session_id),
            &configuration,
            &source_fingerprint,
        )?;
        let streamer_name = self
            .workflows
            .database
            .get_streamer(session.streamer_id)
            .map(|streamer| streamer.name)
            .unwrap_or_else(|_| "已结束直播".to_owned());
        let created = match self.workflows.create(&NewAiSmartWorkflow {
            name: configuration.name.clone(),
            mode: AiSmartWorkflowMode::Replay,
            source_session_id: Some(session_id),
            source_summary: format!(
                "{} · 本机直播回放 · {} 个冻结分片",
                streamer_name,
                imported.added.len()
            ),
            provider: configuration.provider.clone(),
            model_id: configuration.model_id.clone(),
            text_scope: configuration.text_scope.clone(),
            configuration_fingerprint: configuration_fingerprint.clone(),
        }) {
            Ok(created) => created,
            Err(error) => {
                let _ = self.repository.delete_project(project.id);
                return Err(repository_error(error));
            }
        };
        let authorized = self
            .workflows
            .authorize(
                created.workflow.id,
                created.workflow.generation,
                &authorization_digest(created.workflow.id, &configuration_fingerprint),
                &configuration_fingerprint,
            )
            .map_err(repository_error)?;
        let batch = self
            .workflows
            .add_batch(
                created.workflow.id,
                &NewAiSmartWorkflowBatch {
                    video_id: None,
                    source_fingerprint,
                    finalized_at: session
                        .ended_at
                        .clone()
                        .unwrap_or_else(|| Utc::now().to_rfc3339()),
                },
            )
            .map_err(repository_error)?;
        self.project_service
            .start_analysis(project.id)
            .await
            .map_err(|error| self.fail_before_spawn(created.workflow.id, error))?;
        self.controller
            .enqueue_project(project.id)
            .await
            .map_err(|error| self.fail_command_before_spawn(created.workflow.id, error))?;
        self.workflows
            .attach_batch_project(batch.id, project.id, None, AiSmartBatchStatus::Queued)
            .map_err(repository_error)?;
        self.publish_current(created.workflow.id, Some(batch.id));
        self.spawn(created.workflow.id, authorized.workflow.generation)?;
        self.workflows
            .get(created.workflow.id)
            .map_err(repository_error)
    }

    pub async fn ingest_finalized_video(&self, video_id: i64) -> SmartWorkflowResult<usize> {
        let video = self.workflows.database.get_video(video_id).map_err(|_| {
            SmartWorkflowError::new("smart_video_unavailable", "完成分片登记不可用", true)
        })?;
        if video.status != "complete"
            || video.ended_at.is_none()
            || video.audio_present != Some(true)
        {
            return Err(SmartWorkflowError::new(
                "smart_video_not_finalized",
                "智能成片只接受已完成登记且有音轨的分片",
                false,
            ));
        }
        let workflows = self
            .workflows
            .list()
            .map_err(repository_error)?
            .into_iter()
            .filter(|workflow| {
                workflow.mode == AiSmartWorkflowMode::Live
                    && workflow.source_session_id == Some(video.session_id)
                    && !workflow.status.is_terminal()
            })
            .collect::<Vec<_>>();
        let mut finalized_videos = self
            .workflows
            .database
            .list_session_videos(video.session_id)
            .map_err(|_| {
                SmartWorkflowError::new(
                    "smart_live_session_unavailable",
                    "无法读取直播会话的已完成分片",
                    true,
                )
            })?
            .into_iter()
            .filter(|candidate| {
                candidate.status == "complete"
                    && candidate.ended_at.is_some()
                    && candidate.audio_present == Some(true)
            })
            .collect::<Vec<_>>();
        let trigger_position = finalized_videos
            .iter()
            .position(|candidate| candidate.id == video.id)
            .ok_or_else(|| {
                SmartWorkflowError::new(
                    "smart_video_not_finalized",
                    "完成分片未出现在会话的稳定登记序列中",
                    true,
                )
            })?;
        finalized_videos.truncate(trigger_position.saturating_add(1));
        let mut ingested = 0;
        for workflow in &workflows {
            self.validate_authorization(workflow).await?;
            for finalized_video in finalized_videos.iter().filter(|candidate| {
                workflow
                    .live_start_video_id
                    .is_none_or(|start_video_id| candidate.id > start_video_id)
            }) {
                if self
                    .workflows
                    .get_batch_for_video(workflow.id, finalized_video.id)
                    .map_err(repository_error)?
                    .is_some()
                {
                    self.workflows
                        .advance_live_cursor(workflow.id, finalized_video.id)
                        .map_err(repository_error)?;
                    continue;
                }
                self.ingest_video_for_workflow(workflow, finalized_video)
                    .await?;
                ingested += 1;
            }
        }
        Ok(ingested)
    }

    async fn ingest_video_for_workflow(
        &self,
        workflow: &AiSmartWorkflow,
        video: &Video,
    ) -> SmartWorkflowResult<()> {
        let profile = self.controller.default_profile().map_err(command_error)?;
        let project = self
            .project_service
            .create_draft(&format!("{} · 分片", workflow.name), &profile)
            .map_err(service_error)?;
        let input = self
            .project_service
            .import_completed_video(project.id, video.id, CancellationToken::new())
            .await
            .map_err(service_error)?;
        let batch = self
            .workflows
            .add_batch(
                workflow.id,
                &NewAiSmartWorkflowBatch {
                    video_id: Some(video.id),
                    source_fingerprint: input.source_fingerprint_hash,
                    finalized_at: video.ended_at.clone().unwrap_or_else(|| {
                        video
                            .started_at
                            .clone()
                            .unwrap_or_else(|| Utc::now().to_rfc3339())
                    }),
                },
            )
            .map_err(repository_error)?;
        if batch.project_id.is_some() {
            let _ = self.repository.delete_project(project.id);
            return Ok(());
        }
        self.project_service
            .start_analysis(project.id)
            .await
            .map_err(service_error)?;
        self.controller
            .enqueue_project(project.id)
            .await
            .map_err(command_error)?;
        self.workflows
            .attach_batch_project(batch.id, project.id, None, AiSmartBatchStatus::Queued)
            .map_err(repository_error)?;
        self.workflows
            .advance_live_cursor(workflow.id, video.id)
            .map_err(repository_error)?;
        self.publish_current(workflow.id, Some(batch.id));
        self.spawn(workflow.id, workflow.generation)?;
        Ok(())
    }

    pub async fn finalize_live_session(&self, session_id: i64) -> SmartWorkflowResult<usize> {
        let videos = self
            .workflows
            .database
            .list_session_videos(session_id)
            .map_err(|_| {
                SmartWorkflowError::new(
                    "smart_live_session_unavailable",
                    "直播结束后无法读取已登记分片",
                    true,
                )
            })?;
        for video in videos
            .into_iter()
            .filter(|video| video.status == "complete" && video.ended_at.is_some())
        {
            let _ = self.ingest_finalized_video(video.id).await;
        }
        let workflows = self
            .workflows
            .list()
            .map_err(repository_error)?
            .into_iter()
            .filter(|workflow| {
                workflow.mode == AiSmartWorkflowMode::Live
                    && workflow.source_session_id == Some(session_id)
                    && !workflow.status.is_terminal()
            })
            .collect::<Vec<_>>();
        for workflow in &workflows {
            self.spawn(workflow.id, workflow.generation)?;
        }
        Ok(workflows.len())
    }

    pub fn get(&self, workflow_id: i64) -> SmartWorkflowResult<AiSmartWorkflowDetail> {
        self.workflows.get(workflow_id).map_err(repository_error)
    }

    pub fn list(&self) -> SmartWorkflowResult<Vec<AiSmartWorkflow>> {
        self.workflows.list().map_err(repository_error)
    }

    pub fn list_active_live_sessions(&self) -> SmartWorkflowResult<Vec<AiActiveLiveSession>> {
        self.workflows
            .list_active_live_sessions()
            .map_err(repository_error)
    }

    pub fn list_replay_sessions(
        &self,
        search: Option<&str>,
        cursor: Option<&AiSmartReplaySessionCursor>,
        limit: usize,
    ) -> SmartWorkflowResult<AiSmartReplaySessionPage> {
        self.workflows
            .list_replay_sessions(search, cursor, limit)
            .map_err(repository_error)
    }

    pub async fn authorize_and_resume(
        &self,
        workflow_id: i64,
        expected_generation: u32,
        configuration_fingerprint: &str,
        authorization_confirmed: bool,
    ) -> SmartWorkflowResult<AiSmartWorkflowDetail> {
        if !authorization_confirmed {
            return Err(SmartWorkflowError::new(
                "smart_authorization_required",
                "必须明确确认当前任务授权",
                false,
            ));
        }
        self.validate_gates(true).await?;
        let detail = self
            .workflows
            .authorize(
                workflow_id,
                expected_generation,
                &authorization_digest(workflow_id, configuration_fingerprint),
                configuration_fingerprint,
            )
            .map_err(repository_error)?;
        if !detail.batches.is_empty() {
            self.spawn(workflow_id, detail.workflow.generation)?;
        }
        self.publish_current(workflow_id, None);
        Ok(detail)
    }

    pub fn open_draft(&self, draft_id: i64) -> SmartWorkflowResult<super::AiClipProjectDetail> {
        let draft = self
            .repository
            .get_smart_draft(draft_id)
            .map_err(repository_error)?;
        self.repository
            .get_clip_project(draft.clip_project_id)
            .map_err(repository_error)
    }

    pub async fn cancel(
        &self,
        workflow_id: i64,
        expected_generation: u32,
    ) -> SmartWorkflowResult<AiSmartWorkflowDetail> {
        if let Some(task) = self
            .tasks
            .lock()
            .map_err(|_| task_state_error())?
            .remove(&workflow_id)
        {
            task.cancellation.cancel();
        }
        let detail = self
            .workflows
            .cancel(workflow_id, expected_generation)
            .map_err(repository_error)?;
        for project_id in detail.batches.iter().filter_map(|batch| batch.project_id) {
            let _ = self.controller.cancel_project(project_id).await;
        }
        self.publish_current(workflow_id, None);
        Ok(detail)
    }

    pub async fn retry_stage(
        &self,
        workflow_id: i64,
        expected_generation: u32,
        stage: AiSmartStage,
        batch_id: Option<i64>,
    ) -> SmartWorkflowResult<AiSmartWorkflowDetail> {
        let detail = self.workflows.get(workflow_id).map_err(repository_error)?;
        if detail.workflow.generation != expected_generation {
            return Err(SmartWorkflowError::new(
                "smart_generation_conflict",
                "任务代次已变化，请刷新后重试",
                false,
            ));
        }
        let retryable = detail.attempts.iter().any(|attempt| {
            attempt.stage == stage
                && (batch_id.is_none() || attempt.batch_id == batch_id)
                && matches!(
                    attempt.status,
                    AiSmartStageAttemptStatus::Failed | AiSmartStageAttemptStatus::Interrupted
                )
        });
        if !retryable {
            return Err(SmartWorkflowError::new(
                "smart_stage_not_retryable",
                "当前阶段没有可重试的失败或中断尝试",
                false,
            ));
        }
        self.validate_authorization(&detail.workflow).await?;
        let retry_batch_id = batch_id.or_else(|| {
            detail
                .attempts
                .iter()
                .rev()
                .find(|attempt| {
                    attempt.stage == stage
                        && matches!(
                            attempt.status,
                            AiSmartStageAttemptStatus::Failed
                                | AiSmartStageAttemptStatus::Interrupted
                        )
                })
                .and_then(|attempt| attempt.batch_id)
        });
        if let Some(batch_id) = retry_batch_id {
            let batch = detail
                .batches
                .iter()
                .find(|batch| batch.id == batch_id)
                .ok_or_else(|| {
                    SmartWorkflowError::new("smart_batch_unavailable", "重试批次不存在", false)
                })?;
            if stage == AiSmartStage::Asr
                && let Some(project_id) = batch.project_id
            {
                let project = self
                    .repository
                    .get_project(project_id)
                    .map_err(repository_error)?;
                for input in project.inputs.iter().filter(|input| {
                    matches!(
                        input.status,
                        AiInputStatus::Failed | AiInputStatus::Cancelled
                    )
                }) {
                    self.controller
                        .retry_input(input.id)
                        .await
                        .map_err(command_error)?;
                }
            }
            self.workflows
                .reset_batch_for_retry(workflow_id, batch_id)
                .map_err(repository_error)?;
        }
        self.workflows
            .transition(
                workflow_id,
                expected_generation,
                AiSmartWorkflowStatus::Queued,
                stage,
                None,
            )
            .map_err(repository_error)?;
        self.spawn(workflow_id, expected_generation)?;
        self.workflows.get(workflow_id).map_err(repository_error)
    }

    pub async fn recover_and_resume(&self) -> SmartWorkflowResult<u64> {
        let interrupted = self
            .workflows
            .recover_interrupted()
            .map_err(repository_error)?;
        for workflow in self.workflows.list().map_err(repository_error)? {
            if workflow.status != AiSmartWorkflowStatus::Paused {
                continue;
            }
            if self.validate_authorization(&workflow).await.is_ok() {
                self.workflows
                    .transition(
                        workflow.id,
                        workflow.generation,
                        AiSmartWorkflowStatus::Queued,
                        workflow.stage,
                        None,
                    )
                    .map_err(repository_error)?;
                self.spawn(workflow.id, workflow.generation)?;
            }
        }
        Ok(interrupted)
    }

    pub async fn run(
        &self,
        workflow_id: i64,
        expected_generation: u32,
        cancellation: CancellationToken,
    ) -> SmartWorkflowResult<AiSmartWorkflowDetail> {
        let initial = self.workflows.get(workflow_id).map_err(repository_error)?;
        if initial.workflow.generation != expected_generation {
            return Err(stale_generation_error());
        }
        self.validate_authorization(&initial.workflow).await?;
        if initial.workflow.status != AiSmartWorkflowStatus::Running {
            self.workflows
                .transition(
                    workflow_id,
                    expected_generation,
                    AiSmartWorkflowStatus::Running,
                    initial.workflow.stage,
                    None,
                )
                .map_err(repository_error)?;
        }
        for batch in initial.batches {
            if matches!(
                batch.status,
                AiSmartBatchStatus::Completed
                    | AiSmartBatchStatus::Failed
                    | AiSmartBatchStatus::Cancelled
            ) {
                continue;
            }
            if cancellation.is_cancelled() {
                return Err(cancelled_error());
            }
            if let Err(error) = self
                .process_batch(
                    workflow_id,
                    expected_generation,
                    &batch,
                    cancellation.child_token(),
                )
                .await
            {
                if error.code != "smart_cancelled" {
                    let _ = self.workflows.update_batch_status(
                        batch.id,
                        AiSmartBatchStatus::Failed,
                        Some((&error.code, &error.message)),
                    );
                    let current = self
                        .workflows
                        .get(workflow_id)
                        .map_err(repository_error)?
                        .workflow;
                    if current.generation == expected_generation && !current.status.is_terminal() {
                        let _ = self.workflows.transition(
                            workflow_id,
                            expected_generation,
                            AiSmartWorkflowStatus::Failed,
                            current.stage,
                            Some((&error.code, &error.message)),
                        );
                    }
                    self.publish_current(workflow_id, Some(batch.id));
                }
                if initial.workflow.mode != AiSmartWorkflowMode::Live {
                    return Err(error);
                }
            }
        }
        let detail = self.workflows.get(workflow_id).map_err(repository_error)?;
        if detail.workflow.mode == AiSmartWorkflowMode::Live {
            self.settle_live_workflow(&detail)?;
        }
        self.workflows.get(workflow_id).map_err(repository_error)
    }

    async fn process_batch(
        &self,
        workflow_id: i64,
        expected_generation: u32,
        batch: &AiSmartWorkflowBatch,
        cancellation: CancellationToken,
    ) -> SmartWorkflowResult<()> {
        let project_id = batch.project_id.ok_or_else(|| {
            SmartWorkflowError::new(
                "smart_batch_project_missing",
                "智能批次尚未建立受信分析项目",
                true,
            )
        })?;
        self.move_to_stage(
            workflow_id,
            expected_generation,
            AiSmartStage::Asr,
            Some(batch.id),
        )?;
        self.workflows
            .update_batch_status(batch.id, AiSmartBatchStatus::Asr, None)
            .map_err(repository_error)?;
        let asr_fingerprint = format!("{}:{}", batch.source_fingerprint, project_id);
        let asr_attempt = self.start_stage(
            workflow_id,
            Some(batch.id),
            None,
            AiSmartStage::Asr,
            &asr_fingerprint,
        )?;
        if asr_attempt.status != AiSmartStageAttemptStatus::Completed {
            let started = Instant::now();
            loop {
                if cancellation.is_cancelled() {
                    let _ = self.controller.cancel_project(project_id).await;
                    self.finish_cancelled(&asr_attempt, started.elapsed().as_millis() as u64);
                    return Err(cancelled_error());
                }
                let detail = self
                    .repository
                    .get_project(project_id)
                    .map_err(repository_error)?;
                let progress = detail.project.progress_percent;
                let _ = self.workflows.update_attempt_progress(
                    asr_attempt.id,
                    asr_attempt.attempt_generation,
                    progress,
                );
                match detail.project.status {
                    AiProjectStatus::Completed | AiProjectStatus::CompletedWithErrors => break,
                    AiProjectStatus::Cancelled => {
                        self.finish_cancelled(&asr_attempt, started.elapsed().as_millis() as u64);
                        return Err(cancelled_error());
                    }
                    AiProjectStatus::Failed => {
                        let error = SmartWorkflowError::new(
                            "smart_asr_failed",
                            detail
                                .project
                                .last_error_message
                                .as_deref()
                                .unwrap_or("本地 ASR 失败"),
                            true,
                        );
                        self.finish_failed(
                            &asr_attempt,
                            &error,
                            started.elapsed().as_millis() as u64,
                        );
                        return Err(error);
                    }
                    _ => tokio::time::sleep(std::time::Duration::from_millis(250)).await,
                }
            }
            self.workflows
                .finish_attempt(
                    asr_attempt.id,
                    asr_attempt.attempt_generation,
                    AiSmartStageAttemptStatus::Completed,
                    Some(("project", project_id)),
                    started.elapsed().as_millis() as u64,
                    None,
                )
                .map_err(repository_error)?;
        }

        let workflow = self
            .workflows
            .get(workflow_id)
            .map_err(repository_error)?
            .workflow;
        self.validate_authorization(&workflow).await?;
        self.move_to_stage(
            workflow_id,
            expected_generation,
            AiSmartStage::Highlight,
            Some(batch.id),
        )?;
        self.workflows
            .update_batch_status(batch.id, AiSmartBatchStatus::Highlight, None)
            .map_err(repository_error)?;
        let highlight_fingerprint = format!(
            "{}:{}:{}",
            workflow.configuration_fingerprint, project_id, asr_fingerprint
        );
        let highlight_attempt = self.start_stage(
            workflow_id,
            Some(batch.id),
            None,
            AiSmartStage::Highlight,
            &highlight_fingerprint,
        )?;
        let run = if highlight_attempt.status == AiSmartStageAttemptStatus::Completed {
            let run_id = highlight_attempt.result_id.ok_or_else(|| {
                SmartWorkflowError::new("smart_cached_result_missing", "高光缓存结果引用缺失", true)
            })?;
            self.repository
                .get_highlight_run(run_id)
                .map_err(repository_error)?
        } else {
            let started = Instant::now();
            let adjacent_context = self.adjacent_context(batch)?;
            match self
                .highlight
                .analyze_with_context(
                    project_id,
                    true,
                    &adjacent_context,
                    cancellation.child_token(),
                )
                .await
            {
                Ok(run) => {
                    self.workflows
                        .finish_attempt(
                            highlight_attempt.id,
                            highlight_attempt.attempt_generation,
                            AiSmartStageAttemptStatus::Completed,
                            Some(("highlight_run", run.id)),
                            started.elapsed().as_millis() as u64,
                            None,
                        )
                        .map_err(repository_error)?;
                    run
                }
                Err(error) => {
                    let error = llm_workflow_error("smart_highlight_failed", error);
                    self.finish_failed(
                        &highlight_attempt,
                        &error,
                        started.elapsed().as_millis() as u64,
                    );
                    return Err(error);
                }
            }
        };
        self.workflows
            .attach_batch_project(
                batch.id,
                project_id,
                Some(run.id),
                AiSmartBatchStatus::Highlight,
            )
            .map_err(repository_error)?;
        let candidates = self
            .repository
            .list_highlight_candidates(run.id)
            .map_err(repository_error)?;
        let project = self
            .repository
            .get_project(project_id)
            .map_err(repository_error)?;
        let session_base_ms = if workflow.mode == AiSmartWorkflowMode::Live {
            batch
                .video_id
                .and_then(|video_id| self.workflows.database.get_video(video_id).ok())
                .and_then(|video| {
                    workflow.source_session_id.and_then(|session_id| {
                        self.workflows
                            .database
                            .get_session(session_id)
                            .ok()
                            .map(|session| {
                                timeline_offset_ms(
                                    video.started_at.as_deref(),
                                    Some(session.started_at.as_str()),
                                )
                            })
                    })
                })
                .unwrap_or_default()
        } else {
            0
        };
        for candidate in &candidates {
            let input = project
                .inputs
                .iter()
                .find(|input| input.id == candidate.input_id)
                .ok_or_else(|| {
                    SmartWorkflowError::new(
                        "smart_candidate_source_invalid",
                        "高光候选无法映射到受信项目输入",
                        false,
                    )
                })?;
            if !candidate.segment_ids.iter().all(|segment_id| {
                self.repository
                    .list_segments_for_input(candidate.input_id)
                    .ok()
                    .is_some_and(|segments| {
                        segments.iter().any(|segment| segment.id == *segment_id)
                    })
            }) {
                return Err(SmartWorkflowError::new(
                    "smart_candidate_segments_invalid",
                    "高光候选引用了未登记的稳定句段",
                    false,
                ));
            }
            let offset = session_base_ms.saturating_add(input.project_offset_ms.unwrap_or(0));
            let session_start_ms = offset.saturating_add(candidate.start_ms);
            let session_end_ms = offset.saturating_add(candidate.end_ms);
            let semantic_fingerprint = semantic_candidate_fingerprint(candidate);
            let dedupe_key = self
                .workflows
                .resolve_candidate_dedupe_key(
                    workflow_id,
                    &semantic_fingerprint,
                    &candidate.segment_ids,
                    session_start_ms,
                    session_end_ms,
                )
                .map_err(repository_error)?;
            self.workflows
                .upsert_candidate(
                    workflow_id,
                    &NewAiSmartCandidate {
                        dedupe_key,
                        semantic_fingerprint,
                        canonical_candidate_id: candidate.id,
                        total_score: candidate.total_score.round().clamp(0.0, 100.0) as u8,
                        qualified: candidate.total_score >= f32::from(run.qualified_score),
                        selected: candidate.selected,
                        session_start_ms,
                        session_end_ms,
                        first_finalized_at: batch.finalized_at.clone(),
                        sources: vec![NewAiSmartCandidateSource {
                            batch_id: batch.id,
                            candidate_id: candidate.id,
                            video_id: batch.video_id,
                            input_id: candidate.input_id,
                            stable_segment_ids: candidate.segment_ids.clone(),
                            source_start_ms: candidate.start_ms,
                            source_end_ms: candidate.end_ms,
                            session_start_ms,
                            session_end_ms,
                        }],
                    },
                )
                .map_err(repository_error)?;
        }
        let (all_sources, selected) = self
            .workflows
            .selected_clip_inputs(workflow_id)
            .map_err(repository_error)?;
        if selected.is_empty() {
            self.workflows
                .update_batch_status(batch.id, AiSmartBatchStatus::Completed, None)
                .map_err(repository_error)?;
            self.workflows
                .transition(
                    workflow_id,
                    expected_generation,
                    AiSmartWorkflowStatus::AwaitingSelection,
                    AiSmartStage::Draft,
                    None,
                )
                .map_err(repository_error)?;
            self.publish_current(workflow_id, Some(batch.id));
            return Ok(());
        }

        self.move_to_stage(
            workflow_id,
            expected_generation,
            AiSmartStage::Draft,
            Some(batch.id),
        )?;
        let current = self.workflows.get(workflow_id).map_err(repository_error)?;
        let current_draft = current.drafts.last().cloned();
        let draft_generation = current_draft.as_ref().map_or(1, |draft| {
            if draft.ownership == AiSmartDraftOwnership::Automation
                && matches!(
                    draft.status,
                    AiSmartDraftStatus::Active | AiSmartDraftStatus::ReviewReady
                )
            {
                draft.generation
            } else {
                draft.generation.saturating_add(1)
            }
        });
        let draft_fingerprint = format!(
            "{}:{}:{}",
            draft_generation,
            all_sources
                .iter()
                .map(|source| source.highlight_run_id.to_string())
                .collect::<Vec<_>>()
                .join(","),
            selected
                .iter()
                .map(i64::to_string)
                .collect::<Vec<_>>()
                .join(",")
        );
        let draft_attempt = self.start_stage(
            workflow_id,
            Some(batch.id),
            Some(draft_generation),
            AiSmartStage::Draft,
            &draft_fingerprint,
        )?;
        let (mut clip, draft) = if draft_attempt.status == AiSmartStageAttemptStatus::Completed {
            let draft_id = draft_attempt.result_id.ok_or_else(|| {
                SmartWorkflowError::new("smart_cached_result_missing", "草稿缓存结果引用缺失", true)
            })?;
            let draft = self
                .repository
                .get_smart_draft(draft_id)
                .map_err(repository_error)?;
            let clip = self
                .repository
                .get_clip_project(draft.clip_project_id)
                .map_err(repository_error)?;
            (clip, draft)
        } else if let Some(draft) = current_draft.filter(|draft| {
            draft.generation == draft_generation
                && draft.ownership == AiSmartDraftOwnership::Automation
                && matches!(
                    draft.status,
                    AiSmartDraftStatus::Active | AiSmartDraftStatus::ReviewReady
                )
        }) {
            let started = Instant::now();
            let existing = self
                .repository
                .get_clip_project(draft.clip_project_id)
                .map_err(repository_error)?;
            let clip = self
                .repository
                .append_smart_clip_sources_and_candidates(
                    draft.clip_project_id,
                    existing.project.version,
                    &all_sources,
                    &selected,
                )
                .map_err(repository_error)?;
            self.workflows
                .finish_attempt(
                    draft_attempt.id,
                    draft_attempt.attempt_generation,
                    AiSmartStageAttemptStatus::Completed,
                    Some(("smart_draft", draft.id)),
                    started.elapsed().as_millis() as u64,
                    None,
                )
                .map_err(repository_error)?;
            (clip, draft)
        } else {
            let started = Instant::now();
            let (clip, draft) = self
                .repository
                .create_smart_clip_project(
                    workflow_id,
                    draft_generation,
                    &format!("{} · 智能草稿", current.workflow.name),
                    &all_sources,
                    &selected,
                )
                .map_err(repository_error)?;
            self.workflows
                .finish_attempt(
                    draft_attempt.id,
                    draft_attempt.attempt_generation,
                    AiSmartStageAttemptStatus::Completed,
                    Some(("smart_draft", draft.id)),
                    started.elapsed().as_millis() as u64,
                    None,
                )
                .map_err(repository_error)?;
            (clip, draft)
        };

        self.move_to_stage(
            workflow_id,
            expected_generation,
            AiSmartStage::Correction,
            Some(batch.id),
        )?;
        let correction_fingerprint = format!("{}:{}", draft.id, clip.project.version);
        let correction_attempt = self.start_stage(
            workflow_id,
            Some(batch.id),
            Some(draft.generation),
            AiSmartStage::Correction,
            &correction_fingerprint,
        )?;
        if correction_attempt.status != AiSmartStageAttemptStatus::Completed {
            let started = Instant::now();
            match self
                .correction
                .correct_smart_project(
                    clip.project.id,
                    clip.project.version,
                    cancellation.child_token(),
                )
                .await
            {
                Ok(summary) => {
                    clip = summary.detail;
                    self.workflows
                        .finish_attempt(
                            correction_attempt.id,
                            correction_attempt.attempt_generation,
                            AiSmartStageAttemptStatus::Completed,
                            Some(("correction_run", summary.run_id)),
                            started.elapsed().as_millis() as u64,
                            None,
                        )
                        .map_err(repository_error)?;
                }
                Err(ClipTextCorrectionError::NoEligibleSubtitles) => {
                    self.workflows
                        .finish_attempt(
                            correction_attempt.id,
                            correction_attempt.attempt_generation,
                            AiSmartStageAttemptStatus::Completed,
                            None,
                            started.elapsed().as_millis() as u64,
                            Some(("smart_correction_skipped", "当前草稿没有可纠错字幕")),
                        )
                        .map_err(repository_error)?;
                }
                Err(error) => {
                    let error =
                        SmartWorkflowError::new("smart_correction_failed", error.to_string(), true);
                    self.finish_failed(
                        &correction_attempt,
                        &error,
                        started.elapsed().as_millis() as u64,
                    );
                    return Err(error);
                }
            }
        }

        clip = self
            .repository
            .get_clip_project(clip.project.id)
            .map_err(repository_error)?;
        self.move_to_stage(
            workflow_id,
            expected_generation,
            AiSmartStage::Transition,
            Some(batch.id),
        )?;
        let transition_fingerprint = format!("{}:{}", draft.id, clip.project.version);
        let transition_attempt = self.start_stage(
            workflow_id,
            Some(batch.id),
            Some(draft.generation),
            AiSmartStage::Transition,
            &transition_fingerprint,
        )?;
        if transition_attempt.status != AiSmartStageAttemptStatus::Completed {
            let started = Instant::now();
            match self
                .transition
                .match_boundaries(clip.project.id, None, cancellation.child_token())
                .await
            {
                Ok(summary) => {
                    self.workflows
                        .finish_attempt(
                            transition_attempt.id,
                            transition_attempt.attempt_generation,
                            AiSmartStageAttemptStatus::Completed,
                            Some(("transition_run", summary.run_id)),
                            started.elapsed().as_millis() as u64,
                            None,
                        )
                        .map_err(repository_error)?;
                }
                Err(
                    TransitionMatchingError::NoBoundaries | TransitionMatchingError::EmptyCatalog,
                ) => {
                    self.workflows
                        .finish_attempt(
                            transition_attempt.id,
                            transition_attempt.attempt_generation,
                            AiSmartStageAttemptStatus::Completed,
                            None,
                            started.elapsed().as_millis() as u64,
                            Some((
                                "smart_transition_skipped",
                                "当前草稿无需转场或素材目录尚未就绪",
                            )),
                        )
                        .map_err(repository_error)?;
                }
                Err(error) => {
                    let error =
                        SmartWorkflowError::new("smart_transition_failed", error.to_string(), true);
                    self.finish_failed(
                        &transition_attempt,
                        &error,
                        started.elapsed().as_millis() as u64,
                    );
                    return Err(error);
                }
            }
        }

        clip = self
            .repository
            .get_clip_project(clip.project.id)
            .map_err(repository_error)?;
        self.repository
            .mark_smart_draft_review_ready(clip.project.id, clip.project.version)
            .map_err(repository_error)?;
        self.workflows
            .update_batch_status(batch.id, AiSmartBatchStatus::Completed, None)
            .map_err(repository_error)?;
        self.workflows
            .transition(
                workflow_id,
                expected_generation,
                AiSmartWorkflowStatus::ReviewReady,
                AiSmartStage::Review,
                None,
            )
            .map_err(repository_error)?;
        self.publish_current(workflow_id, Some(batch.id));
        Ok(())
    }

    fn spawn(&self, workflow_id: i64, generation: u32) -> SmartWorkflowResult<()> {
        let token = CancellationToken::new();
        {
            let mut tasks = self.tasks.lock().map_err(|_| task_state_error())?;
            if let Some(task) = tasks.get_mut(&workflow_id) {
                task.rerun_requested = true;
                return Ok(());
            }
            tasks.insert(
                workflow_id,
                SmartWorkflowTask {
                    cancellation: token.clone(),
                    rerun_requested: false,
                },
            );
        }
        let workflow = self.clone();
        tokio::spawn(async move {
            loop {
                let _ = workflow
                    .run(workflow_id, generation, token.child_token())
                    .await;
                let rerun = workflow.tasks.lock().ok().is_some_and(|mut tasks| {
                    if let Some(task) = tasks.get_mut(&workflow_id)
                        && task.rerun_requested
                        && !task.cancellation.is_cancelled()
                    {
                        task.rerun_requested = false;
                        true
                    } else {
                        tasks.remove(&workflow_id);
                        false
                    }
                });
                if !rerun {
                    break;
                }
            }
        });
        Ok(())
    }

    fn adjacent_context(
        &self,
        batch: &AiSmartWorkflowBatch,
    ) -> SmartWorkflowResult<Vec<AnalysisSegment>> {
        if batch.position == 0 {
            return Ok(Vec::new());
        }
        let detail = self
            .workflows
            .get(batch.workflow_id)
            .map_err(repository_error)?;
        if detail.workflow.mode != AiSmartWorkflowMode::Live {
            return Ok(Vec::new());
        }
        let Some(previous_project_id) = detail
            .batches
            .iter()
            .filter(|candidate| {
                candidate.position < batch.position
                    && candidate.status == AiSmartBatchStatus::Completed
                    && candidate.project_id.is_some()
            })
            .max_by_key(|candidate| candidate.position)
            .and_then(|candidate| candidate.project_id)
        else {
            return Ok(Vec::new());
        };
        let projection = AiTranscriptProjection::load(&self.repository, previous_project_id)
            .map_err(|_| {
                SmartWorkflowError::new(
                    "smart_adjacent_context_unavailable",
                    "前序完整分片上下文不可用，当前分片将独立分析",
                    true,
                )
            })?;
        let mut segments = projection
            .inputs
            .into_iter()
            .flat_map(|input| {
                input.segments.into_iter().map(|segment| AnalysisSegment {
                    stable_id: segment.stable_segment_id,
                    input_id: segment.input_id,
                    start_ms: segment.source_start_ms,
                    end_ms: segment.source_end_ms,
                    text: segment.normalized_text,
                })
            })
            .collect::<Vec<_>>();
        if segments.len() > 12 {
            segments.drain(..segments.len() - 12);
        }
        Ok(segments)
    }

    fn settle_live_workflow(&self, detail: &AiSmartWorkflowDetail) -> SmartWorkflowResult<()> {
        let session_id = detail.workflow.source_session_id.ok_or_else(|| {
            SmartWorkflowError::new("smart_live_session_missing", "直播任务会话绑定缺失", false)
        })?;
        let session = self
            .workflows
            .database
            .get_session(session_id)
            .map_err(|_| {
                SmartWorkflowError::new(
                    "smart_live_session_unavailable",
                    "绑定的直播会话不可用",
                    true,
                )
            })?;
        if detail.batches.iter().any(|batch| {
            matches!(
                batch.status,
                AiSmartBatchStatus::Pending
                    | AiSmartBatchStatus::Queued
                    | AiSmartBatchStatus::Asr
                    | AiSmartBatchStatus::Highlight
            )
        }) {
            return Ok(());
        }
        if let Some(failed) = detail
            .batches
            .iter()
            .find(|batch| batch.status == AiSmartBatchStatus::Failed)
        {
            self.workflows
                .transition(
                    detail.workflow.id,
                    detail.workflow.generation,
                    AiSmartWorkflowStatus::Failed,
                    detail.workflow.stage,
                    Some((
                        failed
                            .last_error_code
                            .as_deref()
                            .unwrap_or("smart_live_batch_failed"),
                        failed
                            .last_error_message
                            .as_deref()
                            .unwrap_or("直播已结束，仍有失败批次需要重试"),
                    )),
                )
                .map_err(repository_error)?;
            self.publish_current(detail.workflow.id, Some(failed.id));
            return Ok(());
        }
        if session.ended_at.is_none() {
            return Ok(());
        }
        self.workflows
            .transition(
                detail.workflow.id,
                detail.workflow.generation,
                AiSmartWorkflowStatus::Completed,
                AiSmartStage::Review,
                None,
            )
            .map_err(repository_error)?;
        self.publish_current(detail.workflow.id, None);
        Ok(())
    }

    async fn validate_gates(&self, require_resources: bool) -> SmartWorkflowResult<()> {
        if !self.gate.activation_ready() {
            return Err(SmartWorkflowError::new(
                "smart_activation_required",
                "客户端未激活，不能启动智能成片",
                false,
            ));
        }
        if !self.gate.provider_ready() {
            return Err(SmartWorkflowError::new(
                "smart_provider_unavailable",
                "LLM Provider 或凭据尚未配置",
                true,
            ));
        }
        if require_resources {
            let diagnostic = self.controller.diagnose().await.map_err(command_error)?;
            if !diagnostic.ready {
                return Err(SmartWorkflowError::new(
                    "smart_resources_unavailable",
                    diagnostic.message,
                    true,
                ));
            }
        }
        Ok(())
    }

    async fn validate_authorization(&self, workflow: &AiSmartWorkflow) -> SmartWorkflowResult<()> {
        self.validate_gates(false).await?;
        if !self
            .workflows
            .authorization_is_current(
                workflow.id,
                workflow.generation,
                &workflow.configuration_fingerprint,
            )
            .map_err(repository_error)?
        {
            return Err(SmartWorkflowError::new(
                "smart_authorization_stale",
                "任务来源或 Provider 配置已变化，需要重新确认授权",
                false,
            ));
        }
        Ok(())
    }

    fn move_to_stage(
        &self,
        workflow_id: i64,
        generation: u32,
        stage: AiSmartStage,
        batch_id: Option<i64>,
    ) -> SmartWorkflowResult<()> {
        self.workflows
            .transition(
                workflow_id,
                generation,
                AiSmartWorkflowStatus::Running,
                stage,
                None,
            )
            .map_err(repository_error)?;
        self.publish_current(workflow_id, batch_id);
        Ok(())
    }

    fn start_stage(
        &self,
        workflow_id: i64,
        batch_id: Option<i64>,
        draft_generation: Option<u32>,
        stage: AiSmartStage,
        fingerprint: &str,
    ) -> SmartWorkflowResult<AiSmartStageAttempt> {
        self.workflows
            .start_attempt(&SmartStageAttemptStart {
                workflow_id,
                batch_id,
                draft_generation,
                stage,
                input_fingerprint: fingerprint.to_owned(),
            })
            .map_err(repository_error)
    }

    fn finish_failed(
        &self,
        attempt: &AiSmartStageAttempt,
        error: &SmartWorkflowError,
        duration_ms: u64,
    ) {
        let _ = self.workflows.finish_attempt(
            attempt.id,
            attempt.attempt_generation,
            AiSmartStageAttemptStatus::Failed,
            None,
            duration_ms,
            Some((&error.code, &error.message)),
        );
    }

    fn finish_cancelled(&self, attempt: &AiSmartStageAttempt, duration_ms: u64) {
        let _ = self.workflows.finish_attempt(
            attempt.id,
            attempt.attempt_generation,
            AiSmartStageAttemptStatus::Cancelled,
            None,
            duration_ms,
            Some(("smart_cancelled", "智能任务已取消")),
        );
    }

    fn publish_current(&self, workflow_id: i64, batch_id: Option<i64>) {
        if let Ok(detail) = self.workflows.get(workflow_id) {
            if let Ok(candidates) = self.workflows.list_candidates(workflow_id) {
                self.telemetry.record(&smart_workflow_metric(
                    &detail,
                    &candidates,
                    std::env::consts::OS,
                ));
            }
            let workflow = detail.workflow;
            self.publisher.publish(&AiSmartWorkflowEvent {
                workflow_id,
                workflow_generation: workflow.generation,
                batch_id,
                stage: workflow.stage,
                sequence: workflow.event_sequence,
                status: workflow.status,
                error_code: workflow.last_error_code,
                error_message: workflow.last_error_message,
            });
        }
    }

    fn fail_before_spawn(
        &self,
        workflow_id: i64,
        error: super::ServiceError,
    ) -> SmartWorkflowError {
        let error = service_error(error);
        self.record_pre_spawn_failure(workflow_id, &error);
        error
    }

    fn fail_command_before_spawn(
        &self,
        workflow_id: i64,
        error: super::AiCommandError,
    ) -> SmartWorkflowError {
        let error = command_error(error);
        self.record_pre_spawn_failure(workflow_id, &error);
        error
    }

    fn record_pre_spawn_failure(&self, workflow_id: i64, error: &SmartWorkflowError) {
        if let Ok(detail) = self.workflows.get(workflow_id) {
            let _ = self.workflows.transition(
                workflow_id,
                detail.workflow.generation,
                AiSmartWorkflowStatus::Failed,
                detail.workflow.stage,
                Some((&error.code, &error.message)),
            );
            self.publish_current(workflow_id, None);
        }
    }
}

fn validate_configuration(configuration: &SmartWorkflowConfiguration) -> SmartWorkflowResult<()> {
    let required = [
        (&configuration.name, "任务名称"),
        (&configuration.provider, "Provider"),
        (&configuration.model_id, "模型"),
        (&configuration.text_scope, "文本范围"),
        (&configuration.output_preference, "输出偏好"),
    ];
    if let Some((_, label)) = required.iter().find(|(value, _)| value.trim().is_empty()) {
        return Err(SmartWorkflowError::new(
            "smart_configuration_invalid",
            format!("{label}不能为空"),
            false,
        ));
    }
    if configuration.text_scope != "selected_clip_subtitles" {
        return Err(SmartWorkflowError::new(
            "smart_text_scope_invalid",
            "智能成片只允许发送入选片段的最小字幕范围",
            false,
        ));
    }
    Ok(())
}

fn configuration_fingerprint(
    mode: AiSmartWorkflowMode,
    session_id: Option<i64>,
    configuration: &SmartWorkflowConfiguration,
    source_fingerprint: &str,
) -> SmartWorkflowResult<String> {
    let payload =
        serde_json::to_vec(&(mode.as_str(), session_id, configuration, source_fingerprint))
            .map_err(|_| {
                SmartWorkflowError::new(
                    "smart_configuration_invalid",
                    "智能任务配置无法生成安全指纹",
                    false,
                )
            })?;
    Ok(hex::encode(Sha256::digest(payload)))
}

fn aggregate_input_fingerprint<'a>(hashes: impl Iterator<Item = &'a str>) -> String {
    let mut hasher = Sha256::new();
    for hash in hashes {
        hasher.update((hash.len() as u64).to_le_bytes());
        hasher.update(hash.as_bytes());
    }
    hex::encode(hasher.finalize())
}

fn authorization_digest(workflow_id: i64, configuration_fingerprint: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(workflow_id.to_le_bytes());
    hasher.update(configuration_fingerprint.as_bytes());
    hasher.update(b"smart-workflow-authorization-v1");
    hex::encode(hasher.finalize())
}

fn semantic_candidate_fingerprint(candidate: &super::AiHighlightCandidate) -> String {
    let mut hasher = Sha256::new();
    let normalized = candidate
        .title
        .chars()
        .filter(|character| !character.is_whitespace() && !character.is_ascii_punctuation())
        .flat_map(char::to_lowercase)
        .collect::<String>();
    hasher.update(normalized.as_bytes());
    hex::encode(hasher.finalize())
}

fn timeline_offset_ms(source_started_at: Option<&str>, session_started_at: Option<&str>) -> u64 {
    let Some(source_started_at) = source_started_at else {
        return 0;
    };
    let Some(session_started_at) = session_started_at else {
        return 0;
    };
    let Ok(source) = chrono::DateTime::parse_from_rfc3339(source_started_at) else {
        return 0;
    };
    let Ok(session) = chrono::DateTime::parse_from_rfc3339(session_started_at) else {
        return 0;
    };
    u64::try_from((source - session).num_milliseconds()).unwrap_or_default()
}

pub fn smart_workflow_metric(
    detail: &AiSmartWorkflowDetail,
    candidates: &[AiSmartCandidate],
    platform: &str,
) -> AiSmartWorkflowMetric {
    let latest_duration = detail
        .attempts
        .iter()
        .filter_map(|attempt| attempt.duration_ms)
        .next_back();
    let first_reviewable = detail
        .drafts
        .iter()
        .filter_map(|draft| draft.first_reviewable_at.as_deref())
        .filter_map(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
        .min();
    let relevant_finalized = candidates
        .iter()
        .filter(|candidate| candidate.selected)
        .filter_map(|candidate| {
            chrono::DateTime::parse_from_rfc3339(&candidate.first_finalized_at).ok()
        })
        .max();
    let first_draft_latency_ms = (detail.workflow.mode == AiSmartWorkflowMode::Live)
        .then_some(first_reviewable.zip(relevant_finalized))
        .flatten()
        .and_then(|(reviewable, finalized)| {
            u64::try_from((reviewable - finalized).num_milliseconds()).ok()
        });
    AiSmartWorkflowMetric {
        mode: detail.workflow.mode.as_str().to_owned(),
        platform: safe_platform(platform),
        stage: detail.workflow.stage.as_str().to_owned(),
        terminal_state: detail.workflow.status.as_str().to_owned(),
        duration_bucket: latest_duration.map(duration_bucket),
        batch_count: detail.batches.len() as u64,
        candidate_count: detail.workflow.candidate_count,
        selected_count: detail.workflow.selected_count,
        first_draft_latency_bucket: first_draft_latency_ms.map(duration_bucket),
        first_draft_within_ten_minutes: first_draft_latency_ms.map(|value| value <= 600_000),
    }
}

fn duration_bucket(duration_ms: u64) -> String {
    match duration_ms {
        0..=999 => "under_1s",
        1_000..=9_999 => "1s_to_10s",
        10_000..=59_999 => "10s_to_1m",
        60_000..=299_999 => "1m_to_5m",
        300_000..=600_000 => "5m_to_10m",
        _ => "over_10m",
    }
    .to_owned()
}

fn safe_platform(platform: &str) -> String {
    match platform {
        "macos" | "windows" | "test" => platform.to_owned(),
        _ => "other".to_owned(),
    }
}

fn repository_error(error: AiRepositoryError) -> SmartWorkflowError {
    SmartWorkflowError::new("smart_repository_failed", error.to_string(), true)
}

fn service_error(error: super::ServiceError) -> SmartWorkflowError {
    SmartWorkflowError::new("smart_source_or_preflight_failed", error.to_string(), true)
}

fn command_error(error: super::AiCommandError) -> SmartWorkflowError {
    SmartWorkflowError::new(&error.code, error.message, error.retryable)
}

fn llm_workflow_error(code: &str, error: LlmError) -> SmartWorkflowError {
    SmartWorkflowError::new(
        code,
        error.to_string(),
        matches!(
            error,
            LlmError::Temporary | LlmError::Provider | LlmError::InvalidResponse
        ),
    )
}

fn task_state_error() -> SmartWorkflowError {
    SmartWorkflowError::new(
        "smart_task_state_unavailable",
        "智能任务运行状态不可用",
        true,
    )
}

fn stale_generation_error() -> SmartWorkflowError {
    SmartWorkflowError::new(
        "smart_generation_conflict",
        "任务代次已变化，旧任务结果已忽略",
        false,
    )
}

fn cancelled_error() -> SmartWorkflowError {
    SmartWorkflowError::new("smart_cancelled", "智能任务已取消", false)
}

fn sanitize_public_message(message: &str) -> String {
    message
        .trim()
        .chars()
        .filter(|character| !character.is_control())
        .take(256)
        .collect()
}

#[cfg(test)]
mod metric_tests {
    use super::*;

    fn detail(reviewable_at: &str) -> AiSmartWorkflowDetail {
        AiSmartWorkflowDetail {
            workflow: AiSmartWorkflow {
                id: 1,
                name: "不会进入指标的任务名".to_owned(),
                mode: AiSmartWorkflowMode::Live,
                status: AiSmartWorkflowStatus::ReviewReady,
                stage: AiSmartStage::Review,
                generation: 1,
                source_session_id: Some(99),
                source_summary: "不会进入指标的主播与直播间".to_owned(),
                provider: "deepseek".to_owned(),
                model_id: "deepseek-chat".to_owned(),
                text_scope: "selected_clip_subtitles".to_owned(),
                authorization_digest: Some("private-content-fingerprint".to_owned()),
                authorized_at: Some("2026-08-14T00:00:00Z".to_owned()),
                configuration_fingerprint: "private-config-fingerprint".to_owned(),
                active_draft_generation: Some(1),
                live_cursor_video_id: Some(7),
                live_start_video_id: Some(4),
                event_sequence: 9,
                candidate_count: 3,
                selected_count: 1,
                pending_batch_count: 0,
                last_error_code: None,
                last_error_message: Some("/private/video.mp4 字幕正文 API_KEY 原始响应".to_owned()),
                created_at: "2026-08-14T00:00:00Z".to_owned(),
                updated_at: reviewable_at.to_owned(),
            },
            batches: Vec::new(),
            attempts: Vec::new(),
            drafts: vec![AiSmartDraft {
                id: 1,
                workflow_id: 1,
                generation: 1,
                clip_project_id: 1,
                ownership: AiSmartDraftOwnership::Automation,
                status: AiSmartDraftStatus::ReviewReady,
                automation_project_version: 1,
                frozen_project_version: None,
                first_reviewable_at: Some(reviewable_at.to_owned()),
                created_at: "2026-08-14T00:00:00Z".to_owned(),
                updated_at: reviewable_at.to_owned(),
            }],
            frozen_input_count: 0,
            processed_input_count: 0,
        }
    }

    fn candidate(finalized_at: &str) -> AiSmartCandidate {
        AiSmartCandidate {
            id: 1,
            workflow_id: 1,
            canonical_candidate_id: 7,
            total_score: 90,
            qualified: true,
            selected: true,
            session_start_ms: 0,
            session_end_ms: 20_000,
            first_finalized_at: finalized_at.to_owned(),
        }
    }

    #[test]
    fn first_draft_latency_has_ten_minute_boundary_and_clock_guards() {
        let at_target = smart_workflow_metric(
            &detail("2026-08-14T00:10:00Z"),
            &[candidate("2026-08-14T00:00:00Z")],
            "test",
        );
        assert_eq!(
            at_target.first_draft_latency_bucket.as_deref(),
            Some("5m_to_10m")
        );
        assert_eq!(at_target.first_draft_within_ten_minutes, Some(true));

        let over_target = smart_workflow_metric(
            &detail("2026-08-14T00:10:00.001Z"),
            &[candidate("2026-08-14T00:00:00Z")],
            "test",
        );
        assert_eq!(
            over_target.first_draft_latency_bucket.as_deref(),
            Some("over_10m")
        );
        assert_eq!(over_target.first_draft_within_ten_minutes, Some(false));

        let clock_rollback = smart_workflow_metric(
            &detail("2026-08-13T23:59:59Z"),
            &[candidate("2026-08-14T00:00:00Z")],
            "test",
        );
        assert!(clock_rollback.first_draft_latency_bucket.is_none());
        assert!(clock_rollback.first_draft_within_ten_minutes.is_none());
    }

    #[test]
    fn metric_serialization_cannot_include_content_or_source_identifiers() {
        let metric = smart_workflow_metric(
            &detail("2026-08-14T00:01:00Z"),
            &[candidate("2026-08-14T00:00:00Z")],
            "unexpected-platform-value",
        );
        let json = serde_json::to_string(&metric).unwrap();
        assert_eq!(metric.platform, "other");
        for forbidden in [
            "任务名",
            "主播",
            "直播间",
            "/private/video.mp4",
            "字幕正文",
            "API_KEY",
            "原始响应",
            "private-content-fingerprint",
            "private-config-fingerprint",
            "workflowId",
            "sessionId",
        ] {
            assert!(!json.contains(forbidden), "指标泄漏了 {forbidden}: {json}");
        }
    }
}
