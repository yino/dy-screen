use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use dy_screen::asr::FrozenMediaSource;
use dy_screen::resolver::StreamResolver;
use tauri::menu::{MenuBuilder, MenuItem, MenuItemBuilder};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter, Manager, RunEvent, State, WindowEvent};
use tauri_plugin_autostart::ManagerExt as AutostartExt;
use tauri_plugin_notification::NotificationExt;
use tauri_plugin_opener::OpenerExt;

use crate::ai::tauri_commands::*;
use crate::ai::{
    AiCommandService, AiJobEvent, AiJobPublisher, AiProjectService, AiRepository,
    HighlightWorkflow, LocalAsrRuntime, RigDeepSeekProvider, SourceFingerprint,
    SystemCredentialStore,
};
use crate::app_lifecycle::{
    InstanceLock, LifecycleEvent, ShutdownGate, ShutdownReason, log_lifecycle,
};
use crate::app_support::{delete_recording_session, delete_recording_video, validate_settings};
use crate::database::Database;
use crate::domain::{
    AppSettings, BrowserAccessState, BrowserAccessStatus, CommandError, CreateStreamerRequest,
    Dashboard, EnvironmentStatus, MonitorEvent, Streamer, StreamerPromptContext, VideoFilter,
    VideoPage,
};
use crate::preview::{
    PreviewCache, PreviewFailure, PreviewPublisher, PreviewRequest, PreviewService, PreviewSnapshot,
};
use crate::runtime_resource_state::{
    RuntimeResourceEvent, RuntimeResourceState, RuntimeResourceView,
};
use crate::room_resolution::{RoomResolutionPublisher, RoomResolutionService};
use crate::streamer_service::{PublicSourceInspector, create_streamer_with, update_streamer_with};
use crate::supervisor::{MonitorLogger, MonitorPublisher, Supervisor};
use crate::tauri_browser::TauriBrowserPageDriver;
use crate::thumbnail::{
    FfmpegThumbnailExecutor, ThumbnailBatch, ThumbnailCache, ThumbnailEvent, ThumbnailFailure,
    ThumbnailPublisher, ThumbnailRequest, ThumbnailService, ThumbnailSnapshot,
    trusted_thumbnail_request,
};

type TrayStatus = Arc<Mutex<Option<MenuItem<tauri::Wry>>>>;

struct AppState {
    _instance_lock: InstanceLock,
    database: Database,
    supervisor: Supervisor,
    room_resolution: Arc<RoomResolutionService>,
    preview: PreviewService,
    thumbnail: ThumbnailService,
    ai_runtime: Arc<LocalAsrRuntime>,
    runtime_resources: RuntimeResourceState,
    log_dir: PathBuf,
    shutdown_gate: ShutdownGate,
}

#[derive(Clone)]
struct DesktopRoomResolutionPublisher {
    app: AppHandle,
    database: Database,
    logger: MonitorLogger,
    verification_notified: Arc<AtomicBool>,
}

impl RoomResolutionPublisher for DesktopRoomResolutionPublisher {
    fn publish_access_state(&self, state: &BrowserAccessState) {
        let _ = self.app.emit("browser-access-event", state);
        if should_notify_verification(&self.verification_notified, state.status)
            && self
                .database
                .get_settings()
                .is_ok_and(|settings| settings.notifications_enabled)
        {
            let _ = self
                .app
                .notification()
                .builder()
                .title("需要访问验证")
                .body("请打开直播管家的抖音验证窗口")
                .show();
        }
    }

    fn publish_diagnostic(&self, entry: &dy_screen::access::AccessDiagnosticEntry) {
        self.logger.log_access(entry);
    }
}

fn should_notify_verification(notified: &AtomicBool, status: BrowserAccessStatus) -> bool {
    if status == BrowserAccessStatus::VerificationRequired {
        !notified.swap(true, Ordering::SeqCst)
    } else {
        notified.store(false, Ordering::SeqCst);
        false
    }
}

#[derive(Clone)]
struct DesktopAiPublisher {
    app: AppHandle,
}

impl AiJobPublisher for DesktopAiPublisher {
    fn publish(&self, event: AiJobEvent) {
        let _ = self.app.emit("ai-job-event", event);
    }
}

#[derive(Clone)]
struct DesktopPublisher {
    app: AppHandle,
    database: Database,
    tray_status: TrayStatus,
}

#[async_trait]
impl MonitorPublisher for DesktopPublisher {
    async fn publish(&self, event: MonitorEvent) {
        let _ = self.app.emit("monitor-event", event);
        if let Ok(dashboard) = self.database.dashboard()
            && let Ok(status) = self.tray_status.lock()
            && let Some(item) = status.as_ref()
        {
            let _ = item.set_text(format!("正在录制：{} 路", dashboard.active_recordings));
        }
    }

    async fn notify(&self, title: &str, body: &str) {
        let enabled = self
            .database
            .get_settings()
            .map(|settings| settings.notifications_enabled)
            .unwrap_or(false);
        if enabled {
            let _ = self
                .app
                .notification()
                .builder()
                .title(title)
                .body(body)
                .show();
        }
    }
}

#[derive(Clone)]
struct DesktopPreviewPublisher {
    app: AppHandle,
}

impl PreviewPublisher for DesktopPreviewPublisher {
    fn publish(&self, snapshot: &PreviewSnapshot) {
        authorize_preview_media(&self.app, snapshot);
        let _ = self.app.emit("video-preview-event", snapshot);
    }
}

#[derive(Clone)]
struct DesktopThumbnailPublisher {
    app: AppHandle,
    cache: ThumbnailCache,
}

impl ThumbnailPublisher for DesktopThumbnailPublisher {
    fn publish(&self, event: &ThumbnailEvent) {
        authorize_thumbnail_item_with_cache(&self.app, &self.cache, &event.item);
        let _ = self.app.emit("video-thumbnail-event", event);
    }
}

#[tauri::command]
fn get_dashboard(state: State<'_, AppState>) -> Result<Dashboard, String> {
    state
        .database
        .dashboard()
        .map_err(|error| error.to_string())
}

fn require_runtime_ready(state: &AppState) -> Result<(), String> {
    require_runtime_ready_flag(state.runtime_resources.view().ready)
}

fn require_runtime_ready_flag(ready: bool) -> Result<(), String> {
    ready
        .then_some(())
        .ok_or_else(|| "运行资源尚未准备完成，请先完成资源下载和校验".to_owned())
}

#[tauri::command]
fn runtime_resource_status(state: State<'_, RuntimeResourceState>) -> RuntimeResourceView {
    state.view()
}

#[tauri::command]
fn runtime_resource_manifest(state: State<'_, RuntimeResourceState>) -> RuntimeResourceView {
    state.view()
}

#[tauri::command]
async fn runtime_resource_download(
    app: AppHandle,
    state: State<'_, RuntimeResourceState>,
    app_state: State<'_, AppState>,
) -> Result<RuntimeResourceView, String> {
    let runtime = state.inner().clone();
    let ai_runtime = app_state.ai_runtime.clone();
    let supervisor = app_state.supervisor.clone();
    let event_app = app.clone();
    let result = runtime
        .download(move |view, progress| {
            let _ = event_app.emit(
                "runtime-resource-event",
                RuntimeResourceEvent {
                    status: view,
                    progress,
                },
            );
        })
        .await;
    if let Err(error) = &result {
        runtime.fail_download(error);
    }
    runtime.finish_download();
    let result = result.map_err(|error| error.to_string())?;
    let resource_root = runtime
        .current_root()
        .ok_or_else(|| "资源已下载但未找到原子安装目录".to_owned())?;
    ai_runtime
        .activate_resource_root(resource_root)
        .map_err(|error| error.to_string())?;
    ai_runtime
        .recover_startup_and_requeue()
        .await
        .map_err(|error| error.to_string())?;
    supervisor.restore().await?;
    let _ = app.emit("runtime-resource-event", &result);
    Ok(result)
}

#[tauri::command]
fn runtime_resource_cancel(state: State<'_, RuntimeResourceState>) {
    state.cancel();
}

#[tauri::command]
fn runtime_resource_recheck(state: State<'_, RuntimeResourceState>) -> RuntimeResourceView {
    state.refresh()
}

#[tauri::command]
fn runtime_resource_source(state: State<'_, RuntimeResourceState>) -> String {
    state.fixed_source()
}

#[tauri::command]
fn list_streamer_tag_name_suggestions(
    limit: Option<usize>,
    state: State<'_, AppState>,
) -> Result<Vec<String>, String> {
    state
        .database
        .list_streamer_tag_name_suggestions(limit.unwrap_or(20))
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn get_streamer_prompt_context(
    id: i64,
    state: State<'_, AppState>,
) -> Result<StreamerPromptContext, String> {
    state
        .database
        .streamer_prompt_context(id)
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn create_streamer(
    input: CreateStreamerRequest,
    state: State<'_, AppState>,
) -> Result<Streamer, CommandError> {
    require_runtime_ready(state.inner()).map_err(|message| CommandError::new("resource_not_ready", message))?;
    let inspector = PublicSourceInspector::new(
        state.room_resolution.clone(),
        state.room_resolution.public_request_gate(),
    )?;
    create_streamer_with(&state.database, &inspector, &state.supervisor, input).await
}

#[tauri::command]
async fn update_streamer(
    id: i64,
    input: CreateStreamerRequest,
    state: State<'_, AppState>,
) -> Result<Streamer, CommandError> {
    require_runtime_ready(state.inner()).map_err(|message| CommandError::new("resource_not_ready", message))?;
    let inspector = PublicSourceInspector::new(
        state.room_resolution.clone(),
        state.room_resolution.public_request_gate(),
    )?;
    update_streamer_with(&state.database, &inspector, &state.supervisor, id, input).await
}

#[tauri::command]
async fn set_monitor_enabled(
    id: i64,
    enabled: bool,
    state: State<'_, AppState>,
) -> Result<(), String> {
    require_runtime_ready(state.inner())?;
    if enabled {
        state.supervisor.resume(id).await
    } else {
        state.supervisor.stop(id).await
    }
}

#[tauri::command]
fn check_streamer_now(id: i64, state: State<'_, AppState>) -> Result<(), String> {
    require_runtime_ready(state.inner())?;
    state.supervisor.check_now(id)
}

#[tauri::command]
fn get_browser_access_state(state: State<'_, AppState>) -> BrowserAccessState {
    state.room_resolution.access_state()
}

#[tauri::command]
async fn show_douyin_verification(state: State<'_, AppState>) -> Result<(), String> {
    state
        .room_resolution
        .show_verification()
        .await
        .map_err(|error| error.safe_message())
}

#[tauri::command]
async fn recheck_douyin_access(state: State<'_, AppState>) -> Result<BrowserAccessState, String> {
    let access = state
        .room_resolution
        .recheck()
        .await
        .map_err(|error| error.safe_message())?;
    state.supervisor.check_all_now()?;
    Ok(access)
}

#[tauri::command]
async fn clear_douyin_session(
    confirmed: bool,
    state: State<'_, AppState>,
) -> Result<BrowserAccessState, String> {
    if !confirmed {
        return Err("需要确认后才能清除抖音浏览器会话".to_owned());
    }
    state
        .room_resolution
        .clear_session()
        .await
        .map_err(|error| error.safe_message())?;
    Ok(state.room_resolution.access_state())
}

#[tauri::command]
async fn archive_streamer(id: i64, state: State<'_, AppState>) -> Result<(), String> {
    require_runtime_ready(state.inner())?;
    state.supervisor.stop(id).await?;
    state
        .database
        .archive_streamer(id)
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn stop_recording(id: i64, state: State<'_, AppState>) -> Result<(), String> {
    require_runtime_ready(state.inner())?;
    state.supervisor.stop_recording(id).await;
    Ok(())
}

#[tauri::command]
fn list_videos(
    streamer_id: Option<i64>,
    page: u32,
    page_size: u32,
    filter: Option<VideoFilter>,
    state: State<'_, AppState>,
) -> Result<VideoPage, String> {
    require_runtime_ready(state.inner())?;
    let page = state
        .database
        .query_videos(streamer_id, page, page_size, &filter.unwrap_or_default())
        .map_err(|error| error.to_string())?;
    attach_preview_cache_availability(page, &state.preview)
}

#[tauri::command]
fn list_current_videos(streamer_id: i64, state: State<'_, AppState>) -> Result<VideoPage, String> {
    require_runtime_ready(state.inner())?;
    let page = state
        .database
        .query_videos(
            Some(streamer_id),
            1,
            50,
            &VideoFilter {
                current_only: true,
                ..VideoFilter::default()
            },
        )
        .map_err(|error| error.to_string())?;
    attach_preview_cache_availability(page, &state.preview)
}

fn attach_preview_cache_availability(
    mut page: VideoPage,
    preview: &PreviewService,
) -> Result<VideoPage, String> {
    let cached_video_ids = preview.cached_video_ids().map_err(|error| error.message)?;
    for video in &mut page.items {
        video.has_preview_cache = cached_video_ids.contains(&video.id);
    }
    Ok(page)
}

#[tauri::command]
fn get_settings(state: State<'_, AppState>) -> Result<AppSettings, String> {
    state
        .database
        .get_settings()
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn save_settings(
    settings: AppSettings,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    validate_settings(&settings)?;
    let output_root = expand_home(&settings.output_root);
    ensure_writable_directory(&output_root)?;
    let mut normalized = settings;
    normalized.output_root = output_root.to_string_lossy().into_owned();

    if normalized.autostart_enabled {
        app.autolaunch()
            .enable()
            .map_err(|error| error.to_string())?;
    } else {
        app.autolaunch()
            .disable()
            .map_err(|error| error.to_string())?;
    }
    state
        .database
        .save_settings(&normalized)
        .map_err(|error| error.to_string())?;
    state
        .supervisor
        .set_max_concurrent(normalized.max_concurrent_recordings);
    Ok(())
}

#[tauri::command]
async fn request_video_preview(
    id: i64,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<PreviewSnapshot, PreviewFailure> {
    require_runtime_ready(state.inner())
        .map_err(|message| PreviewFailure::new("resource_not_ready", message))?;
    let snapshot = state.preview.request(preview_request(id, &state)?).await?;
    authorize_preview_media(&app, &snapshot);
    Ok(snapshot)
}

#[tauri::command]
async fn retry_video_preview(
    id: i64,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<PreviewSnapshot, PreviewFailure> {
    require_runtime_ready(state.inner())
        .map_err(|message| PreviewFailure::new("resource_not_ready", message))?;
    let snapshot = state.preview.retry(preview_request(id, &state)?).await?;
    authorize_preview_media(&app, &snapshot);
    Ok(snapshot)
}

#[tauri::command]
async fn request_ai_input_preview(
    project_id: i64,
    input_id: i64,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<PreviewSnapshot, PreviewFailure> {
    require_runtime_ready(state.inner())
        .map_err(|message| PreviewFailure::new("resource_not_ready", message))?;
    let snapshot = state
        .preview
        .request(ai_input_preview_request(project_id, input_id, &state)?)
        .await?;
    authorize_preview_media(&app, &snapshot);
    Ok(snapshot)
}

#[tauri::command]
async fn retry_ai_input_preview(
    project_id: i64,
    input_id: i64,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<PreviewSnapshot, PreviewFailure> {
    require_runtime_ready(state.inner())
        .map_err(|message| PreviewFailure::new("resource_not_ready", message))?;
    let snapshot = state
        .preview
        .retry(ai_input_preview_request(project_id, input_id, &state)?)
        .await?;
    authorize_preview_media(&app, &snapshot);
    Ok(snapshot)
}

#[tauri::command]
fn get_video_preview(
    request_id: String,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<PreviewSnapshot, PreviewFailure> {
    require_runtime_ready(state.inner())
        .map_err(|message| PreviewFailure::new("resource_not_ready", message))?;
    let snapshot = state
        .preview
        .get(&request_id)
        .ok_or_else(|| PreviewFailure::new("preview_not_found", "找不到视频预览任务"))?;
    authorize_preview_media(&app, &snapshot);
    Ok(snapshot)
}

#[tauri::command]
fn retain_video_preview(request_id: String, state: State<'_, AppState>) {
    state.preview.retain_playback(&request_id);
}

#[tauri::command]
fn release_video_preview(request_id: String, state: State<'_, AppState>) {
    state.preview.release_playback(&request_id);
}

#[tauri::command]
async fn request_video_thumbnails(
    video_ids: Vec<i64>,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<ThumbnailBatch, ThumbnailFailure> {
    require_runtime_ready(state.inner())
        .map_err(|message| ThumbnailFailure::new("resource_not_ready", message))?;
    if video_ids.len() > crate::thumbnail::THUMBNAIL_MAX_BATCH_SIZE {
        return Err(ThumbnailFailure::new(
            "batch_too_large",
            "单次最多请求 50 个视频封面",
        ));
    }
    let (ffmpeg_path, ffprobe_path) = state
        .runtime_resources
        .media_tools()
        .map_err(|_| ThumbnailFailure::new("resource_not_ready", "受控媒体资源尚未准备完成"))?;
    let mut requests = Vec::with_capacity(video_ids.len());
    for video_id in video_ids {
        requests.push(thumbnail_request(
            video_id,
            &state,
            &ffmpeg_path.to_string_lossy(),
            &ffprobe_path.to_string_lossy(),
        )?);
    }
    let batch = state.thumbnail.request_batch(requests).await?;
    authorize_thumbnail_batch(&app, &state.thumbnail, &batch)?;
    Ok(batch)
}

#[tauri::command]
fn get_video_thumbnails(
    batch_id: String,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<ThumbnailBatch, ThumbnailFailure> {
    require_runtime_ready(state.inner())
        .map_err(|message| ThumbnailFailure::new("resource_not_ready", message))?;
    let batch = state.thumbnail.get_batch(&batch_id)?;
    authorize_thumbnail_batch(&app, &state.thumbnail, &batch)?;
    Ok(batch)
}

#[tauri::command]
fn release_video_thumbnail_batch(batch_id: String, state: State<'_, AppState>) {
    state.thumbnail.release_batch(&batch_id);
}

#[tauri::command]
async fn retry_video_thumbnail(
    batch_id: String,
    video_id: i64,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<ThumbnailSnapshot, ThumbnailFailure> {
    require_runtime_ready(state.inner())
        .map_err(|message| ThumbnailFailure::new("resource_not_ready", message))?;
    let (ffmpeg_path, ffprobe_path) = state
        .runtime_resources
        .media_tools()
        .map_err(|_| ThumbnailFailure::new("resource_not_ready", "受控媒体资源尚未准备完成"))?;
    let request = thumbnail_request(
        video_id,
        &state,
        &ffmpeg_path.to_string_lossy(),
        &ffprobe_path.to_string_lossy(),
    )?;
    let snapshot = state.thumbnail.retry(&batch_id, request).await?;
    authorize_thumbnail_item(&app, &state.thumbnail, &snapshot)?;
    Ok(snapshot)
}

#[tauri::command]
fn open_video(id: i64, app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    require_runtime_ready(state.inner())?;
    let video = state
        .database
        .get_video(id)
        .map_err(|error| error.to_string())?;
    if !Path::new(&video.path).is_file() {
        let _ = state.database.mark_video_status(id, "missing");
        return Err("视频文件已被移动或删除".to_owned());
    }
    app.opener()
        .open_path(video.path, None::<&str>)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn reveal_video(id: i64, app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    require_runtime_ready(state.inner())?;
    let video = state
        .database
        .get_video(id)
        .map_err(|error| error.to_string())?;
    if !Path::new(&video.path).exists() {
        let _ = state.database.mark_video_status(id, "missing");
        return Err("视频文件已被移动或删除".to_owned());
    }
    app.opener()
        .reveal_item_in_dir(&video.path)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn delete_video(id: i64, state: State<'_, AppState>) -> Result<(), String> {
    require_runtime_ready(state.inner())?;
    state
        .database
        .get_video(id)
        .map_err(|error| error.to_string())?;
    state.thumbnail.begin_eviction(&[id]);
    let delete_result = delete_recording_video(&state.database, id);
    let committed = state.database.get_video(id).is_err();
    if !committed {
        state.thumbnail.rollback_eviction(&[id]);
        return delete_result;
    }

    let preview_error = state
        .preview
        .evict_video(id)
        .err()
        .map(|error| format!("视频已删除，但预览缓存清理失败：{}", error.message));
    let thumbnail_error = state
        .thumbnail
        .commit_eviction(&[id])
        .err()
        .map(|error| format!("视频已删除，但封面缓存清理失败：{}", error.message));
    delete_result.and_then(|_| preview_error.or(thumbnail_error).map_or(Ok(()), Err))
}

#[tauri::command]
fn delete_session(session_id: i64, state: State<'_, AppState>) -> Result<(), String> {
    require_runtime_ready(state.inner())?;
    let video_ids = state
        .database
        .list_session_videos(session_id)
        .map_err(|error| error.to_string())?
        .into_iter()
        .map(|video| video.id)
        .collect::<Vec<_>>();
    state.thumbnail.begin_eviction(&video_ids);
    let delete_result = delete_recording_session(&state.database, session_id);
    let committed = state.database.get_session(session_id).is_err();
    if !committed {
        state.thumbnail.rollback_eviction(&video_ids);
        return delete_result;
    }
    let mut cache_error = None;
    for video_id in &video_ids {
        state
            .preview
            .evict_video(*video_id)
            .unwrap_or_else(|error| {
                cache_error.get_or_insert_with(|| {
                    format!("录制会话已删除，但预览缓存清理失败：{}", error.message)
                });
            });
    }
    if let Err(error) = state.thumbnail.commit_eviction(&video_ids) {
        cache_error.get_or_insert_with(|| {
            format!("录制会话已删除，但封面缓存清理失败：{}", error.message)
        });
    }
    delete_result.and_then(|_| cache_error.map_or(Ok(()), Err))
}

#[tauri::command]
fn open_logs(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    std::fs::create_dir_all(&state.log_dir).map_err(|error| error.to_string())?;
    app.opener()
        .open_path(state.log_dir.to_string_lossy(), None::<&str>)
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn diagnose_environment(state: State<'_, AppState>) -> Result<EnvironmentStatus, String> {
    let Some((ffmpeg_path, ffprobe_path)) = state.runtime_resources.media_tools().ok() else {
        return Ok(EnvironmentStatus {
            ffmpeg: false,
            ffprobe: false,
        });
    };
    let (ffmpeg, ffprobe) = tokio::join!(
        executable_works(ffmpeg_path),
        executable_works(ffprobe_path)
    );
    Ok(EnvironmentStatus { ffmpeg, ffprobe })
}

#[tauri::command]
fn request_exit(force: bool, app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    let active_recordings = state
        .database
        .dashboard()
        .map_err(|error| error.to_string())?
        .active_recordings;
    if active_recordings > 0 && !force {
        return Err("当前仍有活动录制，需要确认后才能退出".to_owned());
    }
    begin_shutdown(&app, state.inner(), ShutdownReason::UserRequest);
    Ok(())
}

pub fn run() {
    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .invoke_handler(tauri::generate_handler![
            runtime_resource_status,
            runtime_resource_manifest,
            runtime_resource_download,
            runtime_resource_cancel,
            runtime_resource_recheck,
            runtime_resource_source,
            get_dashboard,
            list_streamer_tag_name_suggestions,
            get_streamer_prompt_context,
            create_streamer,
            update_streamer,
            set_monitor_enabled,
            check_streamer_now,
            get_browser_access_state,
            show_douyin_verification,
            recheck_douyin_access,
            clear_douyin_session,
            archive_streamer,
            stop_recording,
            list_videos,
            list_current_videos,
            get_settings,
            save_settings,
            request_video_preview,
            retry_video_preview,
            request_ai_input_preview,
            retry_ai_input_preview,
            get_video_preview,
            retain_video_preview,
            release_video_preview,
            request_video_thumbnails,
            get_video_thumbnails,
            release_video_thumbnail_batch,
            retry_video_thumbnail,
            open_video,
            reveal_video,
            delete_video,
            delete_session,
            open_logs,
            diagnose_environment,
            ai_list_projects,
            ai_get_project,
            ai_create_project,
            ai_rename_project,
            ai_set_project_context,
            ai_delete_project,
            ai_pick_local_videos,
            ai_import_local_grants,
            ai_list_completed_sessions,
            ai_add_completed_session,
            ai_reorder_inputs,
            ai_remove_input,
            ai_project_summary,
            ai_start_project,
            ai_cancel_project,
            ai_promote_next_input,
            ai_preempt_with_input,
            ai_retry_input,
            ai_query_transcript,
            ai_copy_segment_text,
            ai_copy_input_text,
            ai_copy_project_text,
            ai_export_txt,
            ai_export_json,
            ai_diagnose_environment,
            ai_get_llm_settings,
            ai_save_llm_settings,
            ai_clear_llm_key,
            ai_diagnose_llm_provider,
            ai_start_highlight_analysis,
            ai_get_latest_highlight_run,
            ai_get_highlight_progress,
            ai_resume_highlight_analysis,
            ai_list_highlight_candidates,
            ai_select_highlight_candidates,
            request_exit
        ])
        .setup(|app| {
            let app_data_dir = app.path().app_data_dir()?;
            let app_cache_dir = app.path().app_cache_dir()?;
            let log_dir = app.path().app_log_dir()?;
            std::fs::create_dir_all(&app_data_dir)?;
            let instance_lock = match InstanceLock::acquire(&app_data_dir) {
                Ok(lock) => {
                    log_lifecycle(LifecycleEvent::InstanceLockAcquired, None);
                    lock
                }
                Err(_) => {
                    log_lifecycle(LifecycleEvent::InstanceRejected, None);
                    app.handle().exit(0);
                    return Ok(());
                }
            };
            std::fs::create_dir_all(&app_cache_dir)?;
            std::fs::create_dir_all(&log_dir)?;
            cleanup_old_logs(&log_dir, 30);

            let database = Database::open(&app_data_dir.join("dy-screen.sqlite3"))?;
            database.migrate()?;
            database.reconcile_startup()?;
            let runtime_resource_state = RuntimeResourceState::new(
                database.clone(),
                app_data_dir.clone(),
                desktop_asr_resource_root(app.path().resource_dir()?),
            );
            let _ = runtime_resource_state.refresh();
            app.manage(runtime_resource_state);
            let settings = database.get_settings()?;

            let asr_temporary_root = app_cache_dir.join("asr-audio");
            std::fs::create_dir_all(&asr_temporary_root)?;
            let packaged_asr_root = desktop_asr_resource_root(app.path().resource_dir()?);
            let asr_resource_root = app
                .state::<RuntimeResourceState>()
                .current_root()
                .unwrap_or(packaged_asr_root);
            let asr_ffprobe = app
                .state::<RuntimeResourceState>()
                .media_tools()
                .ok()
                .map(|(_, ffprobe)| ffprobe)
                .unwrap_or_else(|| {
                    #[cfg(debug_assertions)]
                    {
                        PathBuf::from(settings.ffprobe_path.clone())
                    }
                    #[cfg(not(debug_assertions))]
                    {
                        PathBuf::new()
                    }
                });
            let ai_components = tauri::async_runtime::block_on(async {
                LocalAsrRuntime::build(
                    database.clone(),
                    asr_resource_root,
                    asr_ffprobe,
                    asr_temporary_root,
                    Arc::new(DesktopAiPublisher {
                        app: app.handle().clone(),
                    }),
                )
            });
            tauri::async_runtime::block_on(ai_components.runtime.recover_startup_and_requeue())
                .map_err(|error| std::io::Error::other(error.to_string()))?;
            let ai_project_service = AiProjectService::new(
                database.clone(),
                ai_components.inspector,
                ai_components.preflight,
            );
            let credential_store: Arc<dyn crate::ai::CredentialStore> =
                Arc::new(SystemCredentialStore::default());
            let highlight_workflow = Arc::new(HighlightWorkflow::new(
                AiRepository::new(database.clone()),
                Arc::new(RigDeepSeekProvider),
                credential_store.clone(),
            ));
            let ai_commands = AiCommandService::new(
                ai_project_service,
                crate::ai::AiRepository::new(database.clone()),
                ai_components.runtime.clone(),
            )
            .with_highlight_workflow(highlight_workflow)
            .with_credential_store(credential_store);
            app.manage(AiDesktopState::new(ai_commands));

            let tray_status_item = MenuItemBuilder::with_id("recording_status", "正在录制：0 路")
                .enabled(false)
                .build(app)?;
            let tray_status = Arc::new(Mutex::new(Some(tray_status_item.clone())));
            let publisher = Arc::new(DesktopPublisher {
                app: app.handle().clone(),
                database: database.clone(),
                tray_status: tray_status.clone(),
            });
            let max_concurrent = database.get_settings()?.max_concurrent_recordings;
            let monitor_logger = MonitorLogger::file(log_dir.clone());
            let browser_driver = Arc::new(
                TauriBrowserPageDriver::new(
                    app.handle().clone(),
                    app_data_dir.join("douyin-browser-session"),
                )
                .map_err(|error| std::io::Error::other(error.safe_message()))?,
            );
            let room_resolution_publisher = Arc::new(DesktopRoomResolutionPublisher {
                app: app.handle().clone(),
                database: database.clone(),
                logger: monitor_logger.clone(),
                verification_notified: Arc::new(AtomicBool::new(false)),
            });
            let native_room_resolver = Arc::new(
                StreamResolver::new()
                    .map_err(|error| std::io::Error::other(error.safe_message()))?,
            );
            let room_resolution = Arc::new(RoomResolutionService::new(
                native_room_resolver,
                browser_driver,
                room_resolution_publisher,
            ));
            let mut supervisor = Supervisor::new_with_room_discovery(
                database.clone(),
                publisher,
                max_concurrent,
                room_resolution.clone(),
                room_resolution.public_request_gate(),
                monitor_logger,
            )
            .map_err(std::io::Error::other)?;
            supervisor.set_runtime_resources(
                app.state::<RuntimeResourceState>().inner().clone(),
            );
            let preview_cache_dir = app_cache_dir.join("video-preview");
            std::fs::create_dir_all(&preview_cache_dir)?;
            app.asset_protocol_scope()
                .allow_directory(&preview_cache_dir, true)?;
            let preview = PreviewService::with_executor_and_publisher(
                PreviewCache::new(preview_cache_dir),
                Arc::new(crate::preview::FfmpegPreviewExecutor),
                Arc::new(DesktopPreviewPublisher {
                    app: app.handle().clone(),
                }),
            )
            .map_err(std::io::Error::other)?;
            let thumbnail_cache = ThumbnailCache::new(app_cache_dir.join("video-thumbnails"));
            let thumbnail = ThumbnailService::with_executor_and_publisher(
                thumbnail_cache.clone(),
                Arc::new(FfmpegThumbnailExecutor),
                Arc::new(DesktopThumbnailPublisher {
                    app: app.handle().clone(),
                    cache: thumbnail_cache,
                }),
            )
            .map_err(std::io::Error::other)?;
            let shutdown_gate = ShutdownGate::default();

            app.manage(AppState {
                _instance_lock: instance_lock,
                database,
                supervisor: supervisor.clone(),
                room_resolution,
                preview,
                thumbnail,
                ai_runtime: ai_components.runtime,
                runtime_resources: app.state::<RuntimeResourceState>().inner().clone(),
                log_dir,
                shutdown_gate,
            });
            install_shutdown_signal_handlers(app.handle().clone());

            let tray_menu = MenuBuilder::new(app)
                .text("show", "打开主窗口")
                .item(&tray_status_item)
                .separator()
                .text("pause_all", "暂停全部监听")
                .text("resume_all", "恢复全部监听")
                .separator()
                .text("quit", "退出直播管家")
                .build()?;
            let mut tray_builder = TrayIconBuilder::new()
                .menu(&tray_menu)
                .tooltip("直播管家")
                .show_menu_on_left_click(true)
                .on_menu_event(|app, event| handle_tray_event(app, event.id().0.as_str()));
            if let Some(icon) = app.default_window_icon() {
                tray_builder = tray_builder.icon(icon.clone());
            }
            tray_builder.build(app)?;

            if app
                .state::<RuntimeResourceState>()
                .view()
                .ready
            {
                tauri::async_runtime::spawn(async move {
                    let _ = supervisor.restore().await;
                });
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        });

    let app = builder
        .build(tauri::generate_context!())
        .expect("无法创建直播管家应用");
    app.run(|app, event| match event {
        RunEvent::ExitRequested { api, .. } => {
            if let Some(state) = app.try_state::<AppState>()
                && !state.shutdown_gate.is_started()
            {
                api.prevent_exit();
                let active_recordings = state
                    .database
                    .dashboard()
                    .map(|dashboard| dashboard.active_recordings)
                    .unwrap_or_default();
                if active_recordings > 0 {
                    show_main_window(app);
                    let _ = app.emit(
                        "monitor-event",
                        MonitorEvent {
                            kind: "exit_confirmation_requested".to_owned(),
                            streamer_id: None,
                        },
                    );
                } else {
                    begin_shutdown(app, state.inner(), ShutdownReason::TauriExitRequested);
                }
            }
        }
        RunEvent::Resumed => {
            if let Some(state) = app.try_state::<AppState>()
                && !state.shutdown_gate.is_started()
            {
                let _ = state.supervisor.check_all_now();
            }
        }
        _ => {}
    });
}

fn handle_tray_event(app: &AppHandle, id: &str) {
    match id {
        "show" => {
            show_main_window(app);
        }
        "pause_all" => {
            let supervisor = app.state::<AppState>().supervisor.clone();
            tauri::async_runtime::spawn(async move {
                let _ = supervisor.pause_all().await;
            });
        }
        "resume_all" => {
            let supervisor = app.state::<AppState>().supervisor.clone();
            tauri::async_runtime::spawn(async move {
                let _ = supervisor.resume_all().await;
            });
        }
        "quit" => {
            let state = app.state::<AppState>();
            let active_recordings = state
                .database
                .dashboard()
                .map(|dashboard| dashboard.active_recordings)
                .unwrap_or_default();
            if active_recordings > 0 {
                show_main_window(app);
                let _ = app.emit(
                    "monitor-event",
                    MonitorEvent {
                        kind: "exit_confirmation_requested".to_owned(),
                        streamer_id: None,
                    },
                );
            } else {
                begin_shutdown(app, state.inner(), ShutdownReason::Tray);
            }
        }
        _ => {}
    }
}

fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.set_focus();
    }
}

fn begin_shutdown(app: &AppHandle, state: &AppState, reason: ShutdownReason) {
    if !state.shutdown_gate.try_begin() {
        log_lifecycle(LifecycleEvent::ShutdownDeduplicated, Some(reason));
        return;
    }
    log_lifecycle(LifecycleEvent::ShutdownStarted, Some(reason));
    let app = app.clone();
    let supervisor = state.supervisor.clone();
    let room_resolution = state.room_resolution.clone();
    let preview = state.preview.clone();
    let thumbnail = state.thumbnail.clone();
    let ai_runtime = state.ai_runtime.clone();
    tauri::async_runtime::spawn(async move {
        let shutdown = async {
            let (_, _, _, _, _) = tokio::join!(
                preview.shutdown(),
                thumbnail.shutdown(),
                supervisor.shutdown(),
                room_resolution.shutdown(),
                ai_runtime.shutdown()
            );
        };
        let event = if tokio::time::timeout(Duration::from_secs(20), shutdown)
            .await
            .is_ok()
        {
            LifecycleEvent::ShutdownCompleted
        } else {
            LifecycleEvent::ShutdownTimedOut
        };
        log_lifecycle(event, Some(reason));
        app.exit(0);
    });
}

#[cfg(unix)]
fn install_shutdown_signal_handlers(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        use tokio::signal::unix::{SignalKind, signal};

        let Ok(mut sigterm) = signal(SignalKind::terminate()) else {
            log_lifecycle(LifecycleEvent::SignalRegistrationFailed, None);
            return;
        };
        let Ok(mut sigint) = signal(SignalKind::interrupt()) else {
            log_lifecycle(LifecycleEvent::SignalRegistrationFailed, None);
            return;
        };
        let reason = tokio::select! {
            _ = sigterm.recv() => ShutdownReason::Sigterm,
            _ = sigint.recv() => ShutdownReason::Sigint,
        };
        let state = app.state::<AppState>();
        begin_shutdown(&app, state.inner(), reason);
    });
}

#[cfg(not(unix))]
fn install_shutdown_signal_handlers(_app: AppHandle) {}

fn desktop_asr_resource_root(_packaged_resource_dir: PathBuf) -> PathBuf {
    #[cfg(debug_assertions)]
    {
        if let Some(override_root) = std::env::var_os("ASR_RESOURCE_ROOT") {
            return PathBuf::from(override_root);
        }
        let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")));
        let staged = workspace_root.join("resources/asr-stage");
        if staged.join("manifest.json").is_file() {
            return staged;
        }
        workspace_root.join("resources/asr")
    }
    #[cfg(not(debug_assertions))]
    _packaged_resource_dir.join("resources/asr")
}

fn preview_request(id: i64, state: &State<'_, AppState>) -> Result<PreviewRequest, PreviewFailure> {
    let mut video = state
        .database
        .get_video(id)
        .map_err(|_| PreviewFailure::new("video_not_found", "找不到视频记录"))?;
    if !Path::new(&video.path).is_file() {
        let _ = state.database.mark_video_status(id, "missing");
        video.status = "missing".to_owned();
    }
    let (ffmpeg_path, ffprobe_path) = state
        .runtime_resources
        .media_tools()
        .map_err(|_| PreviewFailure::new("resource_not_ready", "受控媒体资源尚未准备完成"))?;
    Ok(PreviewRequest {
        video_id: video.id,
        source_path: video.path,
        source_status: video.status,
        ffmpeg_path: ffmpeg_path.to_string_lossy().into_owned(),
        ffprobe_path: ffprobe_path.to_string_lossy().into_owned(),
    })
}

fn thumbnail_request(
    id: i64,
    state: &State<'_, AppState>,
    ffmpeg_path: &str,
    ffprobe_path: &str,
) -> Result<ThumbnailRequest, ThumbnailFailure> {
    let video = state
        .database
        .get_video(id)
        .map_err(|_| ThumbnailFailure::new("video_not_found", "找不到视频记录"))?;
    let preview_media = state
        .preview
        .cached_media_for_video(id)
        .map_err(|_| ThumbnailFailure::new("preview_cache_unavailable", "无法读取视频预览缓存"))?;
    Ok(trusted_thumbnail_request(
        &video,
        preview_media.as_deref(),
        ffmpeg_path,
        ffprobe_path,
    ))
}

fn ai_input_preview_request(
    project_id: i64,
    input_id: i64,
    state: &State<'_, AppState>,
) -> Result<PreviewRequest, PreviewFailure> {
    let input = AiRepository::new(state.database.clone())
        .get_input(input_id)
        .map_err(|_| PreviewFailure::new("ai_input_not_found", "找不到 AI 项目视频"))?;
    if input.project_id != project_id {
        return Err(PreviewFailure::new(
            "ai_input_project_mismatch",
            "AI 项目视频归属无效",
        ));
    }
    let source = FrozenMediaSource::from_path(Path::new(&input.source_path))
        .map_err(|error| PreviewFailure::new(error.code, error.safe_message))?;
    let current = SourceFingerprint {
        normalized_path: source.path.to_string_lossy().into_owned(),
        size_bytes: source.size_bytes,
        modified_at_ms: i64::try_from(source.modified_at_ms).map_err(|_| {
            PreviewFailure::new("media_timestamp_unavailable", "无法读取视频修改时间")
        })?,
        video_id: input.video_id,
    };
    if current != input.source_fingerprint {
        return Err(PreviewFailure::new(
            "media_changed",
            "视频文件在项目创建后发生变化，无法可靠联动时间戳",
        ));
    }
    let (ffmpeg_path, ffprobe_path) = state
        .runtime_resources
        .media_tools()
        .map_err(|_| PreviewFailure::new("resource_not_ready", "受控媒体资源尚未准备完成"))?;
    Ok(PreviewRequest {
        // AI 输入使用负数命名空间，避免和视频库正整数 ID 的预览缓存冲突。
        video_id: input_id.saturating_neg(),
        source_path: input.source_path,
        source_status: "complete".to_owned(),
        ffmpeg_path: ffmpeg_path.to_string_lossy().into_owned(),
        ffprobe_path: ffprobe_path.to_string_lossy().into_owned(),
    })
}

fn authorize_preview_media(app: &AppHandle, snapshot: &PreviewSnapshot) {
    if let Some(media) = &snapshot.media {
        let _ = app.asset_protocol_scope().allow_file(&media.path);
    }
}

fn authorize_thumbnail_batch(
    app: &AppHandle,
    service: &ThumbnailService,
    batch: &ThumbnailBatch,
) -> Result<(), ThumbnailFailure> {
    for item in &batch.items {
        authorize_thumbnail_item(app, service, item)?;
    }
    Ok(())
}

fn authorize_thumbnail_item(
    app: &AppHandle,
    service: &ThumbnailService,
    snapshot: &ThumbnailSnapshot,
) -> Result<(), ThumbnailFailure> {
    if let Some(media) = &snapshot.media {
        let path = service.authorize_media(snapshot.video_id, Path::new(&media.path))?;
        app.asset_protocol_scope()
            .allow_file(path)
            .map_err(|_| ThumbnailFailure::new("thumbnail_asset_denied", "无法授权视频封面资源"))?;
    }
    Ok(())
}

fn authorize_thumbnail_item_with_cache(
    app: &AppHandle,
    cache: &ThumbnailCache,
    snapshot: &ThumbnailSnapshot,
) {
    if let Some(media) = &snapshot.media
        && let Ok(path) = cache.validate_ready_media(snapshot.video_id, Path::new(&media.path))
    {
        let _ = app.asset_protocol_scope().allow_file(path);
    }
}

fn expand_home(path: &str) -> PathBuf {
    if path == "~" {
        return std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(path));
    }
    if let Some(rest) = path.strip_prefix("~/")
        && let Some(home) = std::env::var_os("HOME")
    {
        return PathBuf::from(home).join(rest);
    }
    PathBuf::from(path)
}

fn ensure_writable_directory(path: &Path) -> Result<(), String> {
    std::fs::create_dir_all(path).map_err(|error| format!("无法创建录像目录：{error}"))?;
    let metadata = std::fs::metadata(path).map_err(|error| format!("无法读取录像目录：{error}"))?;
    if !metadata.is_dir() {
        return Err("录像保存位置不是目录".to_owned());
    }
    let nonce = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let probe = path.join(format!(
        ".dy-screen-write-test-{}-{nonce}",
        std::process::id()
    ));
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
        .map_err(|error| format!("录像目录不可写：{error}"))?;
    std::fs::remove_file(probe).map_err(|error| format!("录像目录写入检查失败：{error}"))?;
    Ok(())
}

async fn executable_works(executable: PathBuf) -> bool {
    tokio::process::Command::new(executable)
        .arg("-version")
        .kill_on_drop(true)
        .output()
        .await
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn cleanup_old_logs(directory: &Path, retention_days: u64) {
    let retention = Duration::from_secs(retention_days * 24 * 60 * 60);
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        let modified = metadata.modified().unwrap_or(SystemTime::now());
        if modified.elapsed().is_ok_and(|age| age > retention) && metadata.is_file() {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn access_verification_notification_is_deduplicated_until_session_recovers() {
        let notified = AtomicBool::new(false);

        assert!(should_notify_verification(
            &notified,
            BrowserAccessStatus::VerificationRequired
        ));
        assert!(!should_notify_verification(
            &notified,
            BrowserAccessStatus::VerificationRequired
        ));
        assert!(!should_notify_verification(
            &notified,
            BrowserAccessStatus::SessionReady
        ));
        assert!(should_notify_verification(
            &notified,
            BrowserAccessStatus::VerificationRequired
        ));
    }

    #[test]
    fn runtime_gate_blocks_business_commands_until_resources_are_ready() {
        let error = require_runtime_ready_flag(false).expect_err("资源未就绪必须锁定业务命令");
        assert!(error.contains("资源尚未准备完成"));
        assert!(require_runtime_ready_flag(true).is_ok());
    }
}
