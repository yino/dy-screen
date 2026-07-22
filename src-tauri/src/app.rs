use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use dy_screen::asr::FrozenMediaSource;
use tauri::menu::{MenuBuilder, MenuItem, MenuItemBuilder};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter, Manager, RunEvent, State, WindowEvent};
use tauri_plugin_autostart::ManagerExt as AutostartExt;
use tauri_plugin_notification::NotificationExt;
use tauri_plugin_opener::OpenerExt;

use crate::ai::tauri_commands::*;
use crate::ai::{
    AiCommandService, AiJobEvent, AiJobPublisher, AiProjectService, AiRepository, LocalAsrRuntime,
    SourceFingerprint,
};
use crate::app_support::{delete_recording_session, validate_settings};
use crate::database::Database;
use crate::domain::{
    AppSettings, CommandError, CreateStreamerRequest, Dashboard, EnvironmentStatus, MonitorEvent,
    Streamer, StreamerPromptContext, VideoFilter, VideoPage,
};
use crate::preview::{
    PreviewCache, PreviewFailure, PreviewPublisher, PreviewRequest, PreviewService, PreviewSnapshot,
};
use crate::streamer_service::{PublicSourceInspector, create_streamer_with, update_streamer_with};
use crate::supervisor::{MonitorPublisher, Supervisor};

type TrayStatus = Arc<Mutex<Option<MenuItem<tauri::Wry>>>>;

struct AppState {
    database: Database,
    supervisor: Supervisor,
    preview: PreviewService,
    ai_runtime: Arc<LocalAsrRuntime>,
    log_dir: PathBuf,
    quitting: Arc<AtomicBool>,
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

#[tauri::command]
fn get_dashboard(state: State<'_, AppState>) -> Result<Dashboard, String> {
    state
        .database
        .dashboard()
        .map_err(|error| error.to_string())
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
    let inspector = PublicSourceInspector::new()?;
    create_streamer_with(&state.database, &inspector, &state.supervisor, input).await
}

#[tauri::command]
async fn update_streamer(
    id: i64,
    input: CreateStreamerRequest,
    state: State<'_, AppState>,
) -> Result<Streamer, CommandError> {
    let inspector = PublicSourceInspector::new()?;
    update_streamer_with(&state.database, &inspector, &state.supervisor, id, input).await
}

#[tauri::command]
async fn set_monitor_enabled(
    id: i64,
    enabled: bool,
    state: State<'_, AppState>,
) -> Result<(), String> {
    if enabled {
        state.supervisor.resume(id).await
    } else {
        state.supervisor.stop(id).await
    }
}

#[tauri::command]
fn check_streamer_now(id: i64, state: State<'_, AppState>) -> Result<(), String> {
    state.supervisor.check_now(id)
}

#[tauri::command]
async fn archive_streamer(id: i64, state: State<'_, AppState>) -> Result<(), String> {
    state.supervisor.stop(id).await?;
    state
        .database
        .archive_streamer(id)
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn stop_recording(id: i64, state: State<'_, AppState>) -> Result<(), String> {
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
    let page = state
        .database
        .query_videos(streamer_id, page, page_size, &filter.unwrap_or_default())
        .map_err(|error| error.to_string())?;
    attach_preview_cache_availability(page, &state.preview)
}

#[tauri::command]
fn list_current_videos(streamer_id: i64, state: State<'_, AppState>) -> Result<VideoPage, String> {
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
fn open_video(id: i64, app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
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
    let video = state
        .database
        .get_video(id)
        .map_err(|error| error.to_string())?;
    state
        .database
        .mark_video_status(id, "pending_delete")
        .map_err(|error| error.to_string())?;
    if let Err(error) = remove_video_file(Path::new(&video.path)) {
        let _ = state.database.mark_video_status(id, &video.status);
        return Err(format!("删除视频失败：{error}"));
    }
    state
        .database
        .delete_video_record(id)
        .map_err(|error| error.to_string())?;
    state
        .preview
        .evict_video(id)
        .map_err(|error| format!("视频已删除，但预览缓存清理失败：{}", error.message))
}

#[tauri::command]
fn delete_session(session_id: i64, state: State<'_, AppState>) -> Result<(), String> {
    let video_ids = state
        .database
        .list_session_videos(session_id)
        .map_err(|error| error.to_string())?
        .into_iter()
        .map(|video| video.id)
        .collect::<Vec<_>>();
    delete_recording_session(&state.database, session_id)?;
    for video_id in video_ids {
        state
            .preview
            .evict_video(video_id)
            .map_err(|error| format!("录制会话已删除，但预览缓存清理失败：{}", error.message))?;
    }
    Ok(())
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
    let settings = state
        .database
        .get_settings()
        .map_err(|error| error.to_string())?;
    let (ffmpeg, ffprobe) = tokio::join!(
        executable_works(settings.ffmpeg_path),
        executable_works(settings.ffprobe_path)
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
    begin_shutdown(
        &app,
        state.supervisor.clone(),
        state.preview.clone(),
        state.ai_runtime.clone(),
        state.quitting.clone(),
    );
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
            get_dashboard,
            list_streamer_tag_name_suggestions,
            get_streamer_prompt_context,
            create_streamer,
            update_streamer,
            set_monitor_enabled,
            check_streamer_now,
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
            ai_retry_input,
            ai_query_transcript,
            ai_copy_segment_text,
            ai_copy_input_text,
            ai_copy_project_text,
            ai_export_txt,
            ai_export_json,
            ai_diagnose_environment,
            request_exit
        ])
        .setup(|app| {
            let app_data_dir = app.path().app_data_dir()?;
            let app_cache_dir = app.path().app_cache_dir()?;
            let log_dir = app.path().app_log_dir()?;
            std::fs::create_dir_all(&app_data_dir)?;
            std::fs::create_dir_all(&app_cache_dir)?;
            std::fs::create_dir_all(&log_dir)?;
            cleanup_old_logs(&log_dir, 14);

            let database = Database::open(&app_data_dir.join("dy-screen.sqlite3"))?;
            database.migrate()?;
            database.reconcile_startup()?;
            let settings = database.get_settings()?;

            let asr_temporary_root = app_cache_dir.join("asr-audio");
            std::fs::create_dir_all(&asr_temporary_root)?;
            let asr_resource_root = desktop_asr_resource_root(app.path().resource_dir()?);
            let ai_components = tauri::async_runtime::block_on(async {
                LocalAsrRuntime::build(
                    database.clone(),
                    asr_resource_root,
                    PathBuf::from(settings.ffprobe_path.clone()),
                    asr_temporary_root,
                    Arc::new(DesktopAiPublisher {
                        app: app.handle().clone(),
                    }),
                )
            });
            ai_components
                .runtime
                .recover_startup()
                .map_err(|error| std::io::Error::other(error.to_string()))?;
            let ai_project_service = AiProjectService::new(
                database.clone(),
                ai_components.inspector,
                ai_components.preflight,
            );
            let ai_commands = AiCommandService::new(
                ai_project_service,
                crate::ai::AiRepository::new(database.clone()),
                ai_components.runtime.clone(),
            );
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
            let supervisor = Supervisor::new(database.clone(), publisher, max_concurrent)
                .map_err(std::io::Error::other)?;
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
            let quitting = Arc::new(AtomicBool::new(false));

            app.manage(AppState {
                database,
                supervisor: supervisor.clone(),
                preview,
                ai_runtime: ai_components.runtime,
                log_dir,
                quitting,
            });

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

            tauri::async_runtime::spawn(async move {
                let _ = supervisor.restore().await;
            });
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
            let state = app.state::<AppState>();
            if !state.quitting.load(Ordering::SeqCst) {
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
                    begin_shutdown(
                        app,
                        state.supervisor.clone(),
                        state.preview.clone(),
                        state.ai_runtime.clone(),
                        state.quitting.clone(),
                    );
                }
            }
        }
        RunEvent::Resumed => {
            let _ = app.state::<AppState>().supervisor.check_all_now();
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
                begin_shutdown(
                    app,
                    state.supervisor.clone(),
                    state.preview.clone(),
                    state.ai_runtime.clone(),
                    state.quitting.clone(),
                );
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

fn begin_shutdown(
    app: &AppHandle,
    supervisor: Supervisor,
    preview: PreviewService,
    ai_runtime: Arc<LocalAsrRuntime>,
    quitting: Arc<AtomicBool>,
) {
    if quitting.swap(true, Ordering::SeqCst) {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let (_, _, _) = tokio::join!(
            preview.shutdown(),
            supervisor.shutdown(),
            ai_runtime.shutdown()
        );
        app.exit(0);
    });
}

fn desktop_asr_resource_root(_packaged_resource_dir: PathBuf) -> PathBuf {
    #[cfg(debug_assertions)]
    {
        if let Some(override_root) = std::env::var_os("ASR_RESOURCE_ROOT") {
            return PathBuf::from(override_root);
        }
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")))
            .join("resources/asr")
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
    let settings = state
        .database
        .get_settings()
        .map_err(|_| PreviewFailure::new("settings_unavailable", "无法读取 FFmpeg 设置"))?;
    Ok(PreviewRequest {
        video_id: video.id,
        source_path: video.path,
        source_status: video.status,
        ffmpeg_path: settings.ffmpeg_path,
        ffprobe_path: settings.ffprobe_path,
    })
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
    let settings = state
        .database
        .get_settings()
        .map_err(|_| PreviewFailure::new("settings_unavailable", "无法读取 FFmpeg 设置"))?;
    Ok(PreviewRequest {
        // AI 输入使用负数命名空间，避免和视频库正整数 ID 的预览缓存冲突。
        video_id: input_id.saturating_neg(),
        source_path: input.source_path,
        source_status: "complete".to_owned(),
        ffmpeg_path: settings.ffmpeg_path,
        ffprobe_path: settings.ffprobe_path,
    })
}

fn authorize_preview_media(app: &AppHandle, snapshot: &PreviewSnapshot) {
    if let Some(media) = &snapshot.media {
        let _ = app.asset_protocol_scope().allow_file(&media.path);
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

fn remove_video_file(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

async fn executable_works(executable: String) -> bool {
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
