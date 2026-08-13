//! AI 项目的类型化应用命令、受信文件授权和安全视图模型。
//!
//! 该层把 UI 操作映射到服务层，拒绝由前端直接提交任意媒体或模型路径。

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{
    AiClipProjectDetail, AiClipSegmentUpdate, AiClipSubtitleUpdate, AiHighlightCandidate,
    AiHighlightCandidatePage, AiHighlightProgress, AiHighlightRun, AiHighlightRunStatus,
    AiInputSourceKind, AiInputStatus, AiProject, AiProjectDetail, AiProjectInput, AiProjectService,
    AiProjectStatus, AiProjectSummary, AiReplaySessionCursor, AiReplaySessionPage,
    AiReplayStreamerCursor, AiReplayStreamerPage, AiRepository, AiSessionOption,
    AiTranscriptProjection, CredentialStore, HighlightWorkflow, ImportBatchResult, ImportRejection,
    LlmProviderSettings, ProviderDiagnostic, RecognitionProfile, ServiceError, SessionImportResult,
    TrustedLocalFile,
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AiCommandError {
    pub code: String,
    pub message: String,
    pub retryable: bool,
}

impl AiCommandError {
    pub fn new(code: impl Into<String>, message: impl Into<String>, retryable: bool) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            retryable,
        }
    }
}

impl std::fmt::Display for AiCommandError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for AiCommandError {}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AiCreateProjectRequest {
    pub name: String,
    #[serde(default)]
    pub hotwords: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AiTrustedFileGrant {
    pub grant_id: String,
    pub display_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AiProjectDetailView {
    pub project: AiProject,
    pub inputs: Vec<AiProjectInputView>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AiProjectInputView {
    pub id: i64,
    pub project_id: i64,
    pub position: i64,
    pub source_kind: AiInputSourceKind,
    pub video_id: Option<i64>,
    pub display_name: String,
    pub duration_ms: Option<u64>,
    pub audio_present: Option<bool>,
    pub project_offset_ms: Option<u64>,
    pub status: AiInputStatus,
    pub progress_percent: u8,
    pub artifact_id: Option<i64>,
    pub last_error_code: Option<String>,
    pub last_error_message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AiImportBatchView {
    pub added: Vec<AiProjectInputView>,
    pub rejected: Vec<ImportRejection>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AiSessionImportView {
    pub detail: AiProjectDetailView,
    pub added_count: usize,
    pub duplicate_count: usize,
    pub unavailable_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AiEnvironmentDiagnostic {
    pub ready: bool,
    pub platform: String,
    pub engine_id: String,
    pub engine_version: String,
    pub model_id: String,
    pub model_version: String,
    pub checks: Vec<AiEnvironmentCheckView>,
    pub message: String,
    pub runtime: Option<AiRuntimeResourceDiagnostic>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AiRuntimeResourceDiagnostic {
    pub bundle_version: String,
    pub manifest_sha256: String,
    pub signature_valid: bool,
    pub components: Vec<AiRuntimeComponentDiagnostic>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AiRuntimeComponentDiagnostic {
    pub id: String,
    pub version: String,
    pub required: bool,
    pub file_count: usize,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AiEnvironmentCheckView {
    pub code: String,
    pub passed: bool,
    pub message: String,
}

#[async_trait]
pub trait AiJobController: Send + Sync {
    fn default_profile(&self) -> Result<RecognitionProfile, AiCommandError>;
    async fn enqueue_project(&self, project_id: i64) -> Result<(), AiCommandError>;
    async fn cancel_project(&self, project_id: i64) -> Result<(), AiCommandError>;
    async fn retry_input(&self, input_id: i64) -> Result<(), AiCommandError>;
    async fn diagnose(&self) -> Result<AiEnvironmentDiagnostic, AiCommandError>;
    async fn promote_next_input(&self, input_id: i64) -> Result<(), AiCommandError> {
        let _ = input_id;
        Err(AiCommandError::new(
            "scheduler_control_unavailable",
            "当前运行时不支持调整队列",
            true,
        ))
    }
    async fn preempt_with_input(
        &self,
        input_id: i64,
        confirmed: bool,
    ) -> Result<(), AiCommandError> {
        let _ = input_id;
        if !confirmed {
            return Err(AiCommandError::new(
                "confirmation_required",
                "立即切换必须二次确认",
                false,
            ));
        }
        Err(AiCommandError::new(
            "scheduler_control_unavailable",
            "当前运行时不支持调整队列",
            true,
        ))
    }
}

#[derive(Clone)]
pub struct AiCommandService {
    project_service: AiProjectService,
    repository: AiRepository,
    controller: Arc<dyn AiJobController>,
    grants: Arc<TrustedFileGrantStore>,
    highlight_workflow: Option<Arc<HighlightWorkflow>>,
    highlight_tasks: Arc<Mutex<HashSet<i64>>>,
    credential_store: Option<Arc<dyn CredentialStore>>,
}

impl AiCommandService {
    pub fn new(
        project_service: AiProjectService,
        repository: AiRepository,
        controller: Arc<dyn AiJobController>,
    ) -> Self {
        Self {
            project_service,
            repository,
            controller,
            grants: Arc::new(TrustedFileGrantStore::default()),
            highlight_workflow: None,
            highlight_tasks: Arc::new(Mutex::new(HashSet::new())),
            credential_store: None,
        }
    }

    pub fn with_highlight_workflow(mut self, workflow: Arc<HighlightWorkflow>) -> Self {
        self.highlight_workflow = Some(workflow);
        self
    }

    pub fn with_credential_store(mut self, store: Arc<dyn CredentialStore>) -> Self {
        self.credential_store = Some(store);
        self
    }

    pub fn list_projects(&self) -> Result<Vec<AiProject>, AiCommandError> {
        self.project_service.open_workspace().map_err(service_error)
    }

    pub fn get_project(&self, project_id: i64) -> Result<AiProjectDetailView, AiCommandError> {
        self.repository
            .get_project(project_id)
            .map(sanitize_project_detail)
            .map_err(repository_error)
    }

    pub fn create_project(
        &self,
        request: AiCreateProjectRequest,
    ) -> Result<AiProject, AiCommandError> {
        let mut profile = self.controller.default_profile()?;
        profile.hotwords = normalize_hotwords(request.hotwords);
        self.project_service
            .create_draft(&request.name, &profile)
            .map_err(service_error)
    }

    pub fn rename_project(&self, project_id: i64, name: &str) -> Result<AiProject, AiCommandError> {
        self.repository
            .rename_project(project_id, name)
            .map_err(repository_error)
    }

    pub fn set_project_context(
        &self,
        project_id: i64,
        tags: Vec<String>,
        analysis_goal: Option<String>,
    ) -> Result<AiProject, AiCommandError> {
        self.repository
            .set_project_context(project_id, &tags, analysis_goal.as_deref())
            .map_err(repository_error)
    }

    pub async fn delete_project(&self, project_id: i64) -> Result<(), AiCommandError> {
        let project = self
            .repository
            .get_project(project_id)
            .map_err(repository_error)?;
        if matches!(
            project.project.status,
            AiProjectStatus::Queued | AiProjectStatus::Running
        ) {
            self.repository
                .mark_project_deleting(project_id)
                .map_err(repository_error)?;
            self.controller.cancel_project(project_id).await?;
            self.repository
                .delete_deleting_project(project_id)
                .map_err(repository_error)?;
            return Ok(());
        }
        self.repository
            .delete_project(project_id)
            .map_err(repository_error)
    }

    /// 只能由后端系统文件选择器调用；Tauri invoke 不暴露 PathBuf 注册入口。
    pub fn register_backend_file_selection(
        &self,
        paths: Vec<PathBuf>,
    ) -> Result<Vec<AiTrustedFileGrant>, AiCommandError> {
        self.grants.register(paths)
    }

    pub async fn import_local_grants(
        &self,
        project_id: i64,
        grant_ids: Vec<String>,
    ) -> Result<AiImportBatchView, AiCommandError> {
        let trusted = self.grants.consume(&grant_ids)?;
        self.project_service
            .import_local_files(
                project_id,
                trusted,
                tokio_util::sync::CancellationToken::new(),
            )
            .await
            .map(sanitize_import_batch)
            .map_err(service_error)
    }

    pub fn consume_trusted_file_grants(
        &self,
        grant_ids: &[String],
    ) -> Result<Vec<TrustedLocalFile>, AiCommandError> {
        self.grants.consume(grant_ids)
    }

    pub fn list_completed_sessions(
        &self,
        limit: usize,
    ) -> Result<Vec<AiSessionOption>, AiCommandError> {
        self.project_service
            .list_completed_sessions(limit.min(200))
            .map_err(service_error)
    }

    pub fn list_replay_streamers(
        &self,
        search: Option<&str>,
        cursor: Option<&AiReplayStreamerCursor>,
        limit: usize,
    ) -> Result<AiReplayStreamerPage, AiCommandError> {
        self.project_service
            .list_replay_streamers(search, cursor, limit)
            .map_err(service_error)
    }

    pub fn list_replay_sessions(
        &self,
        streamer_id: i64,
        project_id: i64,
        search: Option<&str>,
        cursor: Option<&AiReplaySessionCursor>,
        limit: usize,
    ) -> Result<AiReplaySessionPage, AiCommandError> {
        self.project_service
            .list_replay_sessions(streamer_id, project_id, search, cursor, limit)
            .map_err(service_error)
    }

    pub async fn add_completed_session(
        &self,
        project_id: i64,
        session_id: i64,
    ) -> Result<AiSessionImportView, AiCommandError> {
        let result = self
            .project_service
            .select_completed_session(
                project_id,
                session_id,
                tokio_util::sync::CancellationToken::new(),
            )
            .await
            .map_err(service_error)?;
        Ok(sanitize_session_import(
            result,
            self.get_project(project_id)?,
        ))
    }

    pub fn reorder_inputs(
        &self,
        project_id: i64,
        ordered_ids: &[i64],
    ) -> Result<AiProjectDetailView, AiCommandError> {
        self.project_service
            .reorder_inputs(project_id, ordered_ids)
            .map_err(service_error)?;
        self.get_project(project_id)
    }

    pub fn remove_input(
        &self,
        project_id: i64,
        input_id: i64,
    ) -> Result<AiProjectDetailView, AiCommandError> {
        self.project_service
            .remove_input(project_id, input_id)
            .map_err(service_error)?;
        self.get_project(project_id)
    }

    pub async fn project_summary(
        &self,
        project_id: i64,
    ) -> Result<AiProjectSummary, AiCommandError> {
        self.project_service
            .summary(project_id)
            .await
            .map_err(service_error)
    }

    pub async fn start_project(&self, project_id: i64) -> Result<AiProject, AiCommandError> {
        let project = self
            .project_service
            .start_analysis(project_id)
            .await
            .map_err(service_error)?;
        self.controller.enqueue_project(project_id).await?;
        Ok(project)
    }

    pub async fn cancel_project(&self, project_id: i64) -> Result<AiProject, AiCommandError> {
        self.controller.cancel_project(project_id).await?;
        self.repository
            .get_project(project_id)
            .map(|detail| detail.project)
            .map_err(repository_error)
    }

    pub async fn promote_next_input(
        &self,
        input_id: i64,
    ) -> Result<AiProjectDetailView, AiCommandError> {
        self.controller.promote_next_input(input_id).await?;
        let input = self
            .repository
            .get_input(input_id)
            .map_err(repository_error)?;
        self.get_project(input.project_id)
    }

    pub async fn preempt_with_input(
        &self,
        input_id: i64,
        confirmed: bool,
    ) -> Result<AiProjectDetailView, AiCommandError> {
        self.controller
            .preempt_with_input(input_id, confirmed)
            .await?;
        let input = self
            .repository
            .get_input(input_id)
            .map_err(repository_error)?;
        self.get_project(input.project_id)
    }

    pub async fn retry_input(&self, input_id: i64) -> Result<AiProjectDetailView, AiCommandError> {
        self.controller.retry_input(input_id).await?;
        let input = self
            .repository
            .get_input(input_id)
            .map_err(repository_error)?;
        self.get_project(input.project_id)
    }

    pub async fn start_highlight_analysis(
        &self,
        project_id: i64,
        confirmed: bool,
    ) -> Result<AiHighlightRun, AiCommandError> {
        let workflow = self.highlight_workflow.as_ref().ok_or_else(|| {
            AiCommandError::new(
                "llm_provider_unavailable",
                "高光分析 Provider 尚未配置",
                true,
            )
        })?;
        let run = workflow.prepare(project_id, confirmed).map_err(|error| {
            AiCommandError::new("highlight_analysis_failed", error.to_string(), true)
        })?;
        if matches!(run.status, AiHighlightRunStatus::Completed) {
            return Ok(run);
        }
        self.ensure_highlight_task(workflow, run.id)?;
        Ok(run)
    }

    pub fn resume_highlight_analysis(&self, run_id: i64) -> Result<AiHighlightRun, AiCommandError> {
        let workflow = self.highlight_workflow.as_ref().ok_or_else(|| {
            AiCommandError::new(
                "llm_provider_unavailable",
                "高光分析 Provider 尚未配置",
                true,
            )
        })?;
        let run = self
            .repository
            .get_highlight_run(run_id)
            .map_err(repository_error)?;
        if !run.user_authorized {
            return Err(AiCommandError::new(
                "highlight_authorization_required",
                "该高光运行没有用户授权，不能自动恢复",
                false,
            ));
        }
        if !matches!(run.status, AiHighlightRunStatus::Completed) {
            self.ensure_highlight_task(workflow, run.id)?;
        }
        Ok(run)
    }

    fn ensure_highlight_task(
        &self,
        workflow: &Arc<HighlightWorkflow>,
        run_id: i64,
    ) -> Result<(), AiCommandError> {
        let should_start = self
            .highlight_tasks
            .lock()
            .map_err(|_| {
                AiCommandError::new(
                    "highlight_task_state_unavailable",
                    "高光分析任务状态不可用",
                    true,
                )
            })?
            .insert(run_id);
        if should_start {
            let workflow = Arc::clone(workflow);
            let tasks = Arc::clone(&self.highlight_tasks);
            tauri::async_runtime::spawn(async move {
                if let Err(error) = workflow
                    .execute(run_id, tokio_util::sync::CancellationToken::new())
                    .await
                {
                    workflow.record_fatal_error(run_id, &error);
                }
                if let Ok(mut active) = tasks.lock() {
                    active.remove(&run_id);
                }
            });
        }
        Ok(())
    }

    pub async fn diagnose_llm_provider(&self) -> Result<ProviderDiagnostic, AiCommandError> {
        let workflow = self.highlight_workflow.as_ref().ok_or_else(|| {
            AiCommandError::new(
                "llm_provider_unavailable",
                "高光分析 Provider 尚未配置",
                true,
            )
        })?;
        workflow
            .diagnose(tokio_util::sync::CancellationToken::new())
            .await
            .map_err(|error| {
                let retryable = matches!(
                    error,
                    super::LlmError::Temporary | super::LlmError::Provider
                );
                AiCommandError::new("llm_diagnosis_failed", error.to_string(), retryable)
            })
    }

    pub fn list_highlight_candidates(
        &self,
        run_id: i64,
    ) -> Result<Vec<AiHighlightCandidate>, AiCommandError> {
        self.repository
            .list_highlight_candidates(run_id)
            .map_err(repository_error)
    }

    pub fn list_qualified_highlight_candidates(
        &self,
        run_id: i64,
        page: u32,
        page_size: u32,
    ) -> Result<AiHighlightCandidatePage, AiCommandError> {
        self.repository
            .list_qualified_highlight_candidates(run_id, page, page_size)
            .map_err(repository_error)
    }

    pub fn list_selected_highlight_candidates(
        &self,
        run_id: i64,
        page: u32,
        page_size: u32,
    ) -> Result<AiHighlightCandidatePage, AiCommandError> {
        self.repository
            .list_selected_highlight_candidates(run_id, page, page_size)
            .map_err(repository_error)
    }

    pub fn latest_highlight_run(
        &self,
        project_id: i64,
    ) -> Result<Option<AiHighlightRun>, AiCommandError> {
        self.repository
            .latest_highlight_run_for_project(project_id)
            .map_err(repository_error)
    }

    pub fn highlight_progress(&self, run_id: i64) -> Result<AiHighlightProgress, AiCommandError> {
        self.repository
            .highlight_progress(run_id)
            .map_err(repository_error)
    }

    pub fn select_highlight_candidates(
        &self,
        run_id: i64,
        candidate_ids: &[i64],
    ) -> Result<Vec<AiHighlightCandidate>, AiCommandError> {
        self.repository
            .select_highlight_candidates(run_id, candidate_ids)
            .map_err(repository_error)
    }

    pub fn set_highlight_candidate_selected(
        &self,
        run_id: i64,
        candidate_id: i64,
        selected: bool,
    ) -> Result<AiHighlightCandidate, AiCommandError> {
        self.repository
            .set_highlight_candidate_selected(run_id, candidate_id, selected)
            .map_err(repository_error)
    }

    pub fn open_clip_project(&self, run_id: i64) -> Result<AiClipProjectDetail, AiCommandError> {
        self.repository
            .get_or_create_clip_project(run_id)
            .map_err(repository_error)
    }

    pub fn get_clip_project(
        &self,
        clip_project_id: i64,
    ) -> Result<AiClipProjectDetail, AiCommandError> {
        self.repository
            .get_clip_project(clip_project_id)
            .map_err(repository_error)
    }

    pub fn update_clip_segment(
        &self,
        clip_project_id: i64,
        segment_id: i64,
        update: AiClipSegmentUpdate,
    ) -> Result<AiClipProjectDetail, AiCommandError> {
        self.repository
            .update_clip_segment(clip_project_id, segment_id, &update)
            .map_err(repository_error)
    }

    pub fn update_clip_subtitle(
        &self,
        clip_project_id: i64,
        subtitle_id: i64,
        update: AiClipSubtitleUpdate,
    ) -> Result<AiClipProjectDetail, AiCommandError> {
        self.repository
            .update_clip_subtitle(clip_project_id, subtitle_id, &update)
            .map_err(repository_error)
    }

    pub fn reset_clip_subtitle(
        &self,
        clip_project_id: i64,
        subtitle_id: i64,
        expected_project_version: u32,
    ) -> Result<AiClipProjectDetail, AiCommandError> {
        self.repository
            .reset_clip_subtitle(clip_project_id, subtitle_id, expected_project_version)
            .map_err(repository_error)
    }

    pub fn insert_clip_candidate(
        &self,
        clip_project_id: i64,
        candidate_id: i64,
        insert_index: u32,
    ) -> Result<AiClipProjectDetail, AiCommandError> {
        self.repository
            .insert_clip_candidate(clip_project_id, candidate_id, insert_index)
            .map_err(repository_error)
    }

    pub fn reorder_clip_segments(
        &self,
        clip_project_id: i64,
        ordered_ids: &[i64],
    ) -> Result<AiClipProjectDetail, AiCommandError> {
        self.repository
            .reorder_clip_segments(clip_project_id, ordered_ids)
            .map_err(repository_error)
    }

    pub fn remove_clip_segment(
        &self,
        clip_project_id: i64,
        segment_id: i64,
    ) -> Result<AiClipProjectDetail, AiCommandError> {
        self.repository
            .remove_clip_segment(clip_project_id, segment_id)
            .map_err(repository_error)
    }

    pub fn get_llm_provider_settings(
        &self,
        key_configured: bool,
    ) -> Result<LlmProviderSettings, AiCommandError> {
        self.repository
            .get_llm_provider_settings(key_configured)
            .map_err(repository_error)
    }

    pub fn llm_key_configured(&self) -> Result<bool, AiCommandError> {
        self.credential_store
            .as_ref()
            .ok_or_else(|| AiCommandError::new("credential_unavailable", "系统凭据库不可用", true))?
            .get()
            .map(|value| value.is_some())
            .map_err(|_| {
                AiCommandError::new("credential_read_failed", "无法读取系统凭据状态", true)
            })
    }

    pub fn save_llm_provider_settings(
        &self,
        mut settings: LlmProviderSettings,
        api_key: Option<String>,
    ) -> Result<LlmProviderSettings, AiCommandError> {
        if let Some(store) = &self.credential_store {
            if let Some(key) = api_key {
                store.set(&key).map_err(|_| {
                    AiCommandError::new("credential_save_failed", "无法保存系统凭据", true)
                })?;
                settings.key_configured = true;
            } else {
                // 由凭据库的真实状态决定返回值，避免前端旧 DTO 覆盖已保存的 Key 状态。
                settings.key_configured = store.get().map(|value| value.is_some()).unwrap_or(false);
            }
        } else if api_key.is_some() {
            return Err(AiCommandError::new(
                "credential_unavailable",
                "系统凭据库不可用，未保存 Key",
                true,
            ));
        }
        self.repository
            .save_llm_provider_settings(&settings)
            .map_err(repository_error)
    }

    pub fn clear_llm_api_key(&self) -> Result<(), AiCommandError> {
        self.credential_store
            .as_ref()
            .ok_or_else(|| AiCommandError::new("credential_unavailable", "系统凭据库不可用", true))?
            .clear()
            .map_err(|_| AiCommandError::new("credential_clear_failed", "无法清除系统凭据", true))
    }

    pub fn query_transcript(
        &self,
        project_id: i64,
    ) -> Result<AiTranscriptProjection, AiCommandError> {
        AiTranscriptProjection::load(&self.repository, project_id).map_err(|error| {
            AiCommandError::new("transcript_projection_failed", error.to_string(), true)
        })
    }

    pub fn copy_segment_text(
        &self,
        project_id: i64,
        stable_segment_id: &str,
    ) -> Result<String, AiCommandError> {
        self.query_transcript(project_id)?
            .copy_segment_text(stable_segment_id)
            .map_err(|error| {
                AiCommandError::new("transcript_segment_not_found", error.to_string(), false)
            })
    }

    pub fn copy_input_text(
        &self,
        project_id: i64,
        input_id: i64,
    ) -> Result<String, AiCommandError> {
        self.query_transcript(project_id)?
            .copy_input_text(input_id)
            .map_err(|error| {
                AiCommandError::new("transcript_input_not_found", error.to_string(), false)
            })
    }

    pub fn copy_project_text(&self, project_id: i64) -> Result<String, AiCommandError> {
        Ok(self.query_transcript(project_id)?.copy_project_text())
    }

    pub fn export_txt(&self, project_id: i64) -> Result<String, AiCommandError> {
        Ok(self.query_transcript(project_id)?.to_txt())
    }

    pub fn export_json(&self, project_id: i64) -> Result<String, AiCommandError> {
        self.query_transcript(project_id)?
            .to_json()
            .map_err(|error| {
                AiCommandError::new("transcript_export_failed", error.to_string(), true)
            })
    }

    pub async fn diagnose(&self) -> Result<AiEnvironmentDiagnostic, AiCommandError> {
        self.controller.diagnose().await
    }
}

struct GrantedPath {
    path: PathBuf,
    expires_at: Instant,
}

struct TrustedFileGrantStore {
    next_id: AtomicU64,
    grants: Mutex<HashMap<String, GrantedPath>>,
    ttl: Duration,
}

impl Default for TrustedFileGrantStore {
    fn default() -> Self {
        Self {
            next_id: AtomicU64::new(1),
            grants: Mutex::new(HashMap::new()),
            ttl: Duration::from_secs(15 * 60),
        }
    }
}

impl TrustedFileGrantStore {
    fn register(&self, paths: Vec<PathBuf>) -> Result<Vec<AiTrustedFileGrant>, AiCommandError> {
        let mut grants = self.grants.lock().map_err(|_| grant_store_error())?;
        grants.retain(|_, grant| grant.expires_at > Instant::now());
        let mut unique = HashSet::new();
        let mut result = Vec::new();
        for path in paths.into_iter().take(100) {
            let canonical = path.canonicalize().map_err(|_| {
                AiCommandError::new(
                    "selected_file_unavailable",
                    "选择的视频不存在或不可访问",
                    false,
                )
            })?;
            if !canonical
                .metadata()
                .map(|metadata| metadata.is_file())
                .unwrap_or(false)
            {
                return Err(AiCommandError::new(
                    "selected_file_not_regular",
                    "选择的视频必须是普通文件",
                    false,
                ));
            }
            if !unique.insert(canonical.clone()) {
                continue;
            }
            let counter = self.next_id.fetch_add(1, Ordering::Relaxed);
            let digest = hex::encode(Sha256::digest(canonical.as_os_str().as_encoded_bytes()));
            let grant_id = format!("ai-file-{counter:016x}-{}", &digest[..12]);
            let display_name = canonical
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("未命名视频")
                .replace(['\r', '\n'], " ");
            grants.insert(
                grant_id.clone(),
                GrantedPath {
                    path: canonical,
                    expires_at: Instant::now() + self.ttl,
                },
            );
            result.push(AiTrustedFileGrant {
                grant_id,
                display_name,
            });
        }
        Ok(result)
    }

    fn consume(&self, grant_ids: &[String]) -> Result<Vec<TrustedLocalFile>, AiCommandError> {
        if grant_ids.is_empty() || grant_ids.len() > 100 {
            return Err(untrusted_grant_error());
        }
        let unique: HashSet<&str> = grant_ids.iter().map(String::as_str).collect();
        if unique.len() != grant_ids.len() {
            return Err(untrusted_grant_error());
        }
        let now = Instant::now();
        let mut grants = self.grants.lock().map_err(|_| grant_store_error())?;
        grants.retain(|_, grant| grant.expires_at > now);
        if grant_ids.iter().any(|id| !grants.contains_key(id)) {
            return Err(untrusted_grant_error());
        }
        Ok(grant_ids
            .iter()
            .filter_map(|id| {
                grants
                    .remove(id)
                    .map(|grant| TrustedLocalFile::new(id, grant.path))
            })
            .collect())
    }
}

fn normalize_hotwords(hotwords: Vec<String>) -> Vec<String> {
    let mut hotwords = hotwords
        .into_iter()
        .map(|word| word.trim().to_owned())
        .filter(|word| !word.is_empty())
        .take(100)
        .collect::<Vec<_>>();
    hotwords.sort();
    hotwords.dedup();
    hotwords
}

fn sanitize_project_detail(detail: AiProjectDetail) -> AiProjectDetailView {
    AiProjectDetailView {
        project: detail.project,
        inputs: detail
            .inputs
            .into_iter()
            .map(sanitize_project_input)
            .collect(),
    }
}

fn sanitize_import_batch(batch: ImportBatchResult) -> AiImportBatchView {
    AiImportBatchView {
        added: batch
            .added
            .into_iter()
            .map(sanitize_project_input)
            .collect(),
        rejected: batch.rejected,
    }
}

fn sanitize_session_import(
    result: SessionImportResult,
    detail: AiProjectDetailView,
) -> AiSessionImportView {
    AiSessionImportView {
        detail,
        added_count: result.added_count,
        duplicate_count: result.duplicate_count,
        unavailable_count: result.unavailable_count,
    }
}

fn sanitize_project_input(input: AiProjectInput) -> AiProjectInputView {
    AiProjectInputView {
        id: input.id,
        project_id: input.project_id,
        position: input.position,
        source_kind: input.source_kind,
        video_id: input.video_id,
        display_name: input.display_name,
        duration_ms: input.duration_ms,
        audio_present: input.audio_present,
        project_offset_ms: input.project_offset_ms,
        status: input.status,
        progress_percent: input.progress_percent,
        artifact_id: input.artifact_id,
        last_error_code: input.last_error_code,
        last_error_message: input.last_error_message,
    }
}

fn service_error(error: ServiceError) -> AiCommandError {
    let code = match &error {
        ServiceError::SessionStillRecording => "session_still_recording",
        ServiceError::NoValidInput => "no_valid_ai_input",
        ServiceError::Preflight(_) => "asr_environment_not_ready",
        ServiceError::Media(_) => "media_inspection_failed",
        ServiceError::Repository(_) => "ai_repository_error",
        ServiceError::Database(_) => "ai_database_error",
        ServiceError::InvalidReplayQuery(_) => "invalid_replay_query",
    };
    AiCommandError::new(code, error.to_string(), true)
}

fn repository_error(error: super::AiRepositoryError) -> AiCommandError {
    let code = match error {
        super::AiRepositoryError::ClipVersionConflict => "clip_version_conflict",
        super::AiRepositoryError::ClipExportInProgress => "clip_export_in_progress",
        super::AiRepositoryError::InvalidClipSubtitle(_) => "invalid_clip_subtitle",
        _ => "ai_repository_error",
    };
    AiCommandError::new(code, error.to_string(), true)
}

fn grant_store_error() -> AiCommandError {
    AiCommandError::new(
        "file_grant_store_unavailable",
        "本地文件授权状态不可用，请重新选择视频",
        true,
    )
}

fn untrusted_grant_error() -> AiCommandError {
    AiCommandError::new(
        "untrusted_file_grant",
        "本地视频必须通过系统文件选择器添加",
        false,
    )
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn tauri_background_tasks_can_start_without_a_current_tokio_reactor() {
        let (sender, receiver) = mpsc::channel();
        tauri::async_runtime::spawn(async move {
            let _ = sender.send(());
        });
        receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("Tauri 全局运行时应执行后台任务");
    }
}
