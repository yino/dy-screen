//! 暴露给 React WebView 的 Tauri command 薄适配层。
//!
//! 业务规则、路径授权和长任务生命周期分别由 command service 与 desktop runtime 掌握。

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, State};
use tauri_plugin_dialog::DialogExt;

use super::{
    AiClipProjectDetail, AiClipSegmentUpdate, AiCommandError, AiCommandService,
    AiCreateProjectRequest, AiEnvironmentDiagnostic, AiHighlightCandidate,
    AiHighlightCandidatePage, AiHighlightProgress, AiHighlightRun, AiImportBatchView, AiProject,
    AiProjectDetailView, AiProjectSummary, AiReplaySessionCursor, AiReplaySessionPage,
    AiReplayStreamerCursor, AiReplayStreamerPage, AiSessionImportView, AiSessionOption,
    AiTranscriptProjection, AiTrustedFileGrant, LlmProviderSettings, ProviderDiagnostic,
};

pub struct AiDesktopState {
    commands: AiCommandService,
}

impl AiDesktopState {
    pub fn new(commands: AiCommandService) -> Self {
        Self { commands }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AiExportResult {
    pub saved: bool,
}

#[tauri::command]
pub(crate) fn ai_list_projects(
    state: State<'_, AiDesktopState>,
) -> Result<Vec<AiProject>, AiCommandError> {
    state.commands.list_projects()
}

#[tauri::command]
pub(crate) fn ai_get_project(
    project_id: i64,
    state: State<'_, AiDesktopState>,
) -> Result<AiProjectDetailView, AiCommandError> {
    state.commands.get_project(project_id)
}

#[tauri::command]
pub(crate) fn ai_create_project(
    input: AiCreateProjectRequest,
    state: State<'_, AiDesktopState>,
) -> Result<AiProject, AiCommandError> {
    state.commands.create_project(input)
}

#[tauri::command]
pub(crate) fn ai_rename_project(
    project_id: i64,
    name: String,
    state: State<'_, AiDesktopState>,
) -> Result<AiProject, AiCommandError> {
    state.commands.rename_project(project_id, &name)
}

#[tauri::command]
pub(crate) fn ai_set_project_context(
    project_id: i64,
    tags: Vec<String>,
    analysis_goal: Option<String>,
    state: State<'_, AiDesktopState>,
) -> Result<AiProject, AiCommandError> {
    state
        .commands
        .set_project_context(project_id, tags, analysis_goal)
}

#[tauri::command]
pub(crate) async fn ai_delete_project(
    project_id: i64,
    state: State<'_, AiDesktopState>,
) -> Result<(), AiCommandError> {
    state.commands.delete_project(project_id).await
}

#[tauri::command]
pub(crate) async fn ai_pick_local_videos(
    app: AppHandle,
    state: State<'_, AiDesktopState>,
) -> Result<Vec<AiTrustedFileGrant>, AiCommandError> {
    let selected = tokio::task::spawn_blocking(move || {
        app.dialog()
            .file()
            .set_title("选择要识别的本地视频")
            .add_filter(
                "视频文件",
                &["mp4", "mkv", "mov", "avi", "webm", "m4v", "ts"],
            )
            .blocking_pick_files()
    })
    .await
    .map_err(|_| {
        AiCommandError::new("file_dialog_failed", "系统文件选择器异常退出，请重试", true)
    })?;
    let Some(selected) = selected else {
        return Ok(Vec::new());
    };
    let paths = selected
        .into_iter()
        .filter_map(|path| path.into_path().ok())
        .collect::<Vec<_>>();
    state.commands.register_backend_file_selection(paths)
}

#[tauri::command]
pub(crate) async fn ai_import_local_grants(
    project_id: i64,
    grant_ids: Vec<String>,
    state: State<'_, AiDesktopState>,
) -> Result<AiImportBatchView, AiCommandError> {
    state
        .commands
        .import_local_grants(project_id, grant_ids)
        .await
}

#[tauri::command]
pub(crate) fn ai_list_completed_sessions(
    limit: Option<usize>,
    state: State<'_, AiDesktopState>,
) -> Result<Vec<AiSessionOption>, AiCommandError> {
    state.commands.list_completed_sessions(limit.unwrap_or(100))
}

#[tauri::command]
pub(crate) fn ai_list_replay_streamers(
    search: Option<String>,
    cursor: Option<AiReplayStreamerCursor>,
    limit: Option<usize>,
    state: State<'_, AiDesktopState>,
) -> Result<AiReplayStreamerPage, AiCommandError> {
    state
        .commands
        .list_replay_streamers(search.as_deref(), cursor.as_ref(), limit.unwrap_or(20))
}

#[tauri::command]
pub(crate) fn ai_list_replay_sessions(
    streamer_id: i64,
    project_id: i64,
    search: Option<String>,
    cursor: Option<AiReplaySessionCursor>,
    limit: Option<usize>,
    state: State<'_, AiDesktopState>,
) -> Result<AiReplaySessionPage, AiCommandError> {
    state.commands.list_replay_sessions(
        streamer_id,
        project_id,
        search.as_deref(),
        cursor.as_ref(),
        limit.unwrap_or(20),
    )
}

#[tauri::command]
pub(crate) async fn ai_add_completed_session(
    project_id: i64,
    session_id: i64,
    state: State<'_, AiDesktopState>,
) -> Result<AiSessionImportView, AiCommandError> {
    state
        .commands
        .add_completed_session(project_id, session_id)
        .await
}

#[tauri::command]
pub(crate) fn ai_reorder_inputs(
    project_id: i64,
    ordered_ids: Vec<i64>,
    state: State<'_, AiDesktopState>,
) -> Result<AiProjectDetailView, AiCommandError> {
    state.commands.reorder_inputs(project_id, &ordered_ids)
}

#[tauri::command]
pub(crate) fn ai_remove_input(
    project_id: i64,
    input_id: i64,
    state: State<'_, AiDesktopState>,
) -> Result<AiProjectDetailView, AiCommandError> {
    state.commands.remove_input(project_id, input_id)
}

#[tauri::command]
pub(crate) async fn ai_project_summary(
    project_id: i64,
    state: State<'_, AiDesktopState>,
) -> Result<AiProjectSummary, AiCommandError> {
    state.commands.project_summary(project_id).await
}

#[tauri::command]
pub(crate) async fn ai_start_project(
    project_id: i64,
    state: State<'_, AiDesktopState>,
) -> Result<AiProject, AiCommandError> {
    state.commands.start_project(project_id).await
}

#[tauri::command]
pub(crate) async fn ai_cancel_project(
    project_id: i64,
    state: State<'_, AiDesktopState>,
) -> Result<AiProject, AiCommandError> {
    state.commands.cancel_project(project_id).await
}

#[tauri::command]
pub(crate) async fn ai_promote_next_input(
    input_id: i64,
    state: State<'_, AiDesktopState>,
) -> Result<AiProjectDetailView, AiCommandError> {
    state.commands.promote_next_input(input_id).await
}

#[tauri::command]
pub(crate) async fn ai_preempt_with_input(
    input_id: i64,
    confirmed: bool,
    state: State<'_, AiDesktopState>,
) -> Result<AiProjectDetailView, AiCommandError> {
    state.commands.preempt_with_input(input_id, confirmed).await
}

#[tauri::command]
pub(crate) async fn ai_retry_input(
    input_id: i64,
    state: State<'_, AiDesktopState>,
) -> Result<AiProjectDetailView, AiCommandError> {
    state.commands.retry_input(input_id).await
}

#[tauri::command]
pub(crate) fn ai_query_transcript(
    project_id: i64,
    state: State<'_, AiDesktopState>,
) -> Result<AiTranscriptProjection, AiCommandError> {
    state.commands.query_transcript(project_id)
}

#[tauri::command]
pub(crate) fn ai_copy_segment_text(
    project_id: i64,
    stable_segment_id: String,
    state: State<'_, AiDesktopState>,
) -> Result<String, AiCommandError> {
    state
        .commands
        .copy_segment_text(project_id, &stable_segment_id)
}

#[tauri::command]
pub(crate) fn ai_copy_input_text(
    project_id: i64,
    input_id: i64,
    state: State<'_, AiDesktopState>,
) -> Result<String, AiCommandError> {
    state.commands.copy_input_text(project_id, input_id)
}

#[tauri::command]
pub(crate) fn ai_copy_project_text(
    project_id: i64,
    state: State<'_, AiDesktopState>,
) -> Result<String, AiCommandError> {
    state.commands.copy_project_text(project_id)
}

#[tauri::command]
pub(crate) async fn ai_export_txt(
    project_id: i64,
    app: AppHandle,
    state: State<'_, AiDesktopState>,
) -> Result<AiExportResult, AiCommandError> {
    let content = state.commands.export_txt(project_id)?;
    let project = state.commands.get_project(project_id)?.project;
    save_export(
        app,
        export_name(&project.name, "txt"),
        "TXT 文本",
        "txt",
        content,
    )
    .await
}

#[tauri::command]
pub(crate) async fn ai_export_json(
    project_id: i64,
    app: AppHandle,
    state: State<'_, AiDesktopState>,
) -> Result<AiExportResult, AiCommandError> {
    let content = state.commands.export_json(project_id)?;
    let project = state.commands.get_project(project_id)?.project;
    save_export(
        app,
        export_name(&project.name, "json"),
        "JSON 数据",
        "json",
        content,
    )
    .await
}

#[tauri::command]
pub(crate) async fn ai_diagnose_environment(
    state: State<'_, AiDesktopState>,
) -> Result<AiEnvironmentDiagnostic, AiCommandError> {
    state.commands.diagnose().await
}

#[tauri::command]
pub(crate) fn ai_get_llm_settings(
    state: State<'_, AiDesktopState>,
) -> Result<LlmProviderSettings, AiCommandError> {
    let key_configured = state.commands.llm_key_configured()?;
    state.commands.get_llm_provider_settings(key_configured)
}

#[tauri::command]
pub(crate) fn ai_save_llm_settings(
    settings: LlmProviderSettings,
    api_key: Option<String>,
    state: State<'_, AiDesktopState>,
) -> Result<LlmProviderSettings, AiCommandError> {
    state.commands.save_llm_provider_settings(settings, api_key)
}

#[tauri::command]
pub(crate) fn ai_clear_llm_key(state: State<'_, AiDesktopState>) -> Result<(), AiCommandError> {
    state.commands.clear_llm_api_key()
}

#[tauri::command]
pub(crate) async fn ai_start_highlight_analysis(
    project_id: i64,
    confirmed: bool,
    state: State<'_, AiDesktopState>,
) -> Result<AiHighlightRun, AiCommandError> {
    state
        .commands
        .start_highlight_analysis(project_id, confirmed)
        .await
}

#[tauri::command]
pub(crate) fn ai_get_latest_highlight_run(
    project_id: i64,
    state: State<'_, AiDesktopState>,
) -> Result<Option<AiHighlightRun>, AiCommandError> {
    state.commands.latest_highlight_run(project_id)
}

#[tauri::command]
pub(crate) fn ai_get_highlight_progress(
    run_id: i64,
    state: State<'_, AiDesktopState>,
) -> Result<AiHighlightProgress, AiCommandError> {
    state.commands.highlight_progress(run_id)
}

#[tauri::command]
pub(crate) fn ai_resume_highlight_analysis(
    run_id: i64,
    state: State<'_, AiDesktopState>,
) -> Result<AiHighlightRun, AiCommandError> {
    state.commands.resume_highlight_analysis(run_id)
}

#[tauri::command]
pub(crate) async fn ai_diagnose_llm_provider(
    state: State<'_, AiDesktopState>,
) -> Result<ProviderDiagnostic, AiCommandError> {
    state.commands.diagnose_llm_provider().await
}

#[tauri::command]
pub(crate) fn ai_list_highlight_candidates(
    run_id: i64,
    state: State<'_, AiDesktopState>,
) -> Result<Vec<AiHighlightCandidate>, AiCommandError> {
    state.commands.list_highlight_candidates(run_id)
}

#[tauri::command]
pub(crate) fn ai_list_qualified_highlight_candidates(
    run_id: i64,
    page: Option<u32>,
    page_size: Option<u32>,
    state: State<'_, AiDesktopState>,
) -> Result<AiHighlightCandidatePage, AiCommandError> {
    state.commands.list_qualified_highlight_candidates(
        run_id,
        page.unwrap_or(0),
        page_size.unwrap_or(50),
    )
}

#[tauri::command]
pub(crate) fn ai_list_selected_highlight_candidates(
    run_id: i64,
    page: Option<u32>,
    page_size: Option<u32>,
    state: State<'_, AiDesktopState>,
) -> Result<AiHighlightCandidatePage, AiCommandError> {
    state.commands.list_selected_highlight_candidates(
        run_id,
        page.unwrap_or(0),
        page_size.unwrap_or(50),
    )
}

#[tauri::command]
pub(crate) fn ai_select_highlight_candidates(
    run_id: i64,
    candidate_ids: Vec<i64>,
    state: State<'_, AiDesktopState>,
) -> Result<Vec<AiHighlightCandidate>, AiCommandError> {
    state
        .commands
        .select_highlight_candidates(run_id, &candidate_ids)
}

#[tauri::command]
pub(crate) fn ai_set_highlight_candidate_selected(
    run_id: i64,
    candidate_id: i64,
    selected: bool,
    state: State<'_, AiDesktopState>,
) -> Result<AiHighlightCandidate, AiCommandError> {
    state
        .commands
        .set_highlight_candidate_selected(run_id, candidate_id, selected)
}

#[tauri::command]
pub(crate) fn ai_open_clip_project(
    run_id: i64,
    state: State<'_, AiDesktopState>,
) -> Result<AiClipProjectDetail, AiCommandError> {
    state.commands.open_clip_project(run_id)
}

#[tauri::command]
pub(crate) fn ai_get_clip_project(
    clip_project_id: i64,
    state: State<'_, AiDesktopState>,
) -> Result<AiClipProjectDetail, AiCommandError> {
    state.commands.get_clip_project(clip_project_id)
}

#[tauri::command]
pub(crate) fn ai_update_clip_segment(
    clip_project_id: i64,
    segment_id: i64,
    update: AiClipSegmentUpdate,
    state: State<'_, AiDesktopState>,
) -> Result<AiClipProjectDetail, AiCommandError> {
    state
        .commands
        .update_clip_segment(clip_project_id, segment_id, update)
}

#[tauri::command]
pub(crate) fn ai_insert_clip_candidate(
    clip_project_id: i64,
    candidate_id: i64,
    insert_index: u32,
    state: State<'_, AiDesktopState>,
) -> Result<AiClipProjectDetail, AiCommandError> {
    state
        .commands
        .insert_clip_candidate(clip_project_id, candidate_id, insert_index)
}

#[tauri::command]
pub(crate) fn ai_reorder_clip_segments(
    clip_project_id: i64,
    ordered_ids: Vec<i64>,
    state: State<'_, AiDesktopState>,
) -> Result<AiClipProjectDetail, AiCommandError> {
    state
        .commands
        .reorder_clip_segments(clip_project_id, &ordered_ids)
}

#[tauri::command]
pub(crate) fn ai_remove_clip_segment(
    clip_project_id: i64,
    segment_id: i64,
    state: State<'_, AiDesktopState>,
) -> Result<AiClipProjectDetail, AiCommandError> {
    state
        .commands
        .remove_clip_segment(clip_project_id, segment_id)
}

async fn save_export(
    app: AppHandle,
    file_name: String,
    filter_name: &'static str,
    extension: &'static str,
    content: String,
) -> Result<AiExportResult, AiCommandError> {
    let destination = tokio::task::spawn_blocking(move || {
        app.dialog()
            .file()
            .set_title("导出只读转写结果")
            .set_file_name(file_name)
            .add_filter(filter_name, &[extension])
            .blocking_save_file()
    })
    .await
    .map_err(|_| {
        AiCommandError::new(
            "export_dialog_failed",
            "系统保存对话框异常退出，请重试",
            true,
        )
    })?;
    let Some(destination) = destination.and_then(|path| path.into_path().ok()) else {
        return Ok(AiExportResult { saved: false });
    };
    tokio::fs::write(destination, content)
        .await
        .map_err(|_| AiCommandError::new("export_write_failed", "无法写入导出文件", true))?;
    Ok(AiExportResult { saved: true })
}

fn export_name(project_name: &str, extension: &str) -> String {
    let safe = project_name
        .chars()
        .map(|character| {
            if character.is_alphanumeric() || matches!(character, ' ' | '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    format!("{}-转写.{}", safe.trim(), extension)
}

#[cfg(test)]
mod tests {
    use super::export_name;

    #[test]
    fn export_file_name_does_not_allow_path_components() {
        assert_eq!(export_name("../主播/直播", "txt"), "___主播_直播-转写.txt");
    }
}
