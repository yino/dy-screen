use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use dy_screen::resolver::StreamResolver;
use tauri::menu::{MenuBuilder, MenuItem, MenuItemBuilder};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter, Manager, RunEvent, State, WindowEvent};
use tauri_plugin_autostart::ManagerExt as AutostartExt;
use tauri_plugin_notification::NotificationExt;
use tauri_plugin_opener::OpenerExt;

use crate::app_support::{
    delete_recording_session, parse_room_identity, validate_room_access, validate_settings,
};
use crate::database::Database;
use crate::domain::{
    AppSettings, CreateStreamerRequest, Dashboard, EnvironmentStatus, MonitorEvent, NewStreamer,
    Streamer, VideoFilter, VideoPage,
};
use crate::supervisor::{MonitorPublisher, Supervisor};

type TrayStatus = Arc<Mutex<Option<MenuItem<tauri::Wry>>>>;

struct AppState {
    database: Database,
    supervisor: Supervisor,
    log_dir: PathBuf,
    quitting: Arc<AtomicBool>,
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

#[tauri::command]
fn get_dashboard(state: State<'_, AppState>) -> Result<Dashboard, String> {
    state
        .database
        .dashboard()
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn create_streamer(
    input: CreateStreamerRequest,
    state: State<'_, AppState>,
) -> Result<Streamer, String> {
    let name = input.name.trim();
    if name.is_empty() {
        return Err("请输入主播名称".to_owned());
    }
    if name.chars().count() > 80 {
        return Err("主播名称不能超过 80 个字符".to_owned());
    }
    let (room_url, _) = parse_room_identity(&input.room_url)?;
    let resolver = StreamResolver::new().map_err(|_| "无法初始化直播间访问校验".to_owned())?;
    let room_id = validate_room_access(resolver.inspect(&room_url).await)?;
    let existing = state
        .database
        .find_streamer_by_room_id(&room_id)
        .map_err(|error| error.to_string())?;
    if let Some(existing) = existing.as_ref().filter(|streamer| !streamer.archived) {
        return Err(format!("该直播间已经存在，主播 ID 为 {}", existing.id));
    }
    let new_streamer = NewStreamer {
        name: name.to_owned(),
        room_url,
        room_id,
        monitor_enabled: input.monitor_enabled,
    };
    let streamer = if let Some(existing) = existing {
        state.database.restore_streamer(existing.id, &new_streamer)
    } else {
        state.database.add_streamer(&new_streamer)
    }
    .map_err(|error| error.to_string())?;
    if streamer.monitor_enabled {
        state.supervisor.start(streamer.id)?;
    }
    Ok(streamer)
}

#[tauri::command]
async fn update_streamer(
    id: i64,
    input: CreateStreamerRequest,
    state: State<'_, AppState>,
) -> Result<Streamer, String> {
    let name = input.name.trim();
    if name.is_empty() {
        return Err("请输入主播名称".to_owned());
    }
    if name.chars().count() > 80 {
        return Err("主播名称不能超过 80 个字符".to_owned());
    }
    let current = state
        .database
        .get_streamer(id)
        .map_err(|error| error.to_string())?;
    let (room_url, _) = parse_room_identity(&input.room_url)?;
    let resolver = StreamResolver::new().map_err(|_| "无法初始化直播间访问校验".to_owned())?;
    let room_id = validate_room_access(resolver.inspect(&room_url).await)?;
    if let Some(existing) = state
        .database
        .find_streamer_by_room_id(&room_id)
        .map_err(|error| error.to_string())?
        && existing.id != id
    {
        return Err(format!("该直播间已经存在，主播 ID 为 {}", existing.id));
    }
    if current.monitor_enabled {
        state.supervisor.stop(id).await?;
    }
    let updated = state.database.update_streamer(
        id,
        &NewStreamer {
            name: name.to_owned(),
            room_url,
            room_id,
            monitor_enabled: input.monitor_enabled,
        },
    );
    let updated = match updated {
        Ok(streamer) => streamer,
        Err(error) => {
            if current.monitor_enabled {
                let _ = state.supervisor.resume(id).await;
            }
            return Err(error.to_string());
        }
    };
    if updated.monitor_enabled {
        state.supervisor.start(updated.id)?;
    }
    Ok(updated)
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
    state
        .database
        .query_videos(streamer_id, page, page_size, &filter.unwrap_or_default())
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn list_current_videos(streamer_id: i64, state: State<'_, AppState>) -> Result<VideoPage, String> {
    state
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
        .map_err(|error| error.to_string())
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
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn delete_session(session_id: i64, state: State<'_, AppState>) -> Result<(), String> {
    delete_recording_session(&state.database, session_id)
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
    begin_shutdown(&app, state.supervisor.clone(), state.quitting.clone());
    Ok(())
}

pub fn run() {
    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .invoke_handler(tauri::generate_handler![
            get_dashboard,
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
            open_video,
            reveal_video,
            delete_video,
            delete_session,
            open_logs,
            diagnose_environment,
            request_exit
        ])
        .setup(|app| {
            let app_data_dir = app.path().app_data_dir()?;
            let log_dir = app.path().app_log_dir()?;
            std::fs::create_dir_all(&app_data_dir)?;
            std::fs::create_dir_all(&log_dir)?;
            cleanup_old_logs(&log_dir, 14);

            let database = Database::open(&app_data_dir.join("dy-screen.sqlite3"))?;
            database.migrate()?;
            database.reconcile_startup()?;

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
            let quitting = Arc::new(AtomicBool::new(false));

            app.manage(AppState {
                database,
                supervisor: supervisor.clone(),
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
                    begin_shutdown(app, state.supervisor.clone(), state.quitting.clone());
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
                begin_shutdown(app, state.supervisor.clone(), state.quitting.clone());
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

fn begin_shutdown(app: &AppHandle, supervisor: Supervisor, quitting: Arc<AtomicBool>) {
    if quitting.swap(true, Ordering::SeqCst) {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        supervisor.shutdown().await;
        app.exit(0);
    });
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
