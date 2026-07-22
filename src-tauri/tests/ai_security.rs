use std::sync::Arc;

use dy_screen_app_lib::ai::{
    AiInputStatus, AiJobController, AiJobEvent, AiJobPublisher, AiProjectStatus, LocalAsrRuntime,
};
use dy_screen_app_lib::database::Database;
use dy_screen_app_lib::domain::AppSettings;
use tempfile::tempdir;

struct IgnoreEvents;

impl AiJobPublisher for IgnoreEvents {
    fn publish(&self, _event: AiJobEvent) {}
}

#[test]
fn production_settings_and_tauri_command_surface_do_not_accept_asr_executable_or_model_paths() {
    let settings = serde_json::to_string(&AppSettings::defaults()).unwrap();
    for forbidden in [
        "whisperPath",
        "asrModelPath",
        "vadModelPath",
        "asrResourceRoot",
    ] {
        assert!(!settings.contains(forbidden));
    }

    let commands = include_str!("../src/ai/tauri_commands.rs");
    assert!(commands.contains("fn ai_pick_local_videos"));
    assert!(commands.contains("fn ai_import_local_grants"));
    for forbidden in [
        "source_path: PathBuf",
        "model_path: PathBuf",
        "sidecar_path: PathBuf",
        "ffmpeg_path: PathBuf",
    ] {
        assert!(!commands.contains(forbidden));
    }
}

#[test]
fn recoverable_ai_events_only_expose_stable_ids_status_and_safe_messages() {
    let event = AiJobEvent {
        project_id: 7,
        input_id: 9,
        project_status: AiProjectStatus::Running,
        project_progress_percent: 37,
        input_status: AiInputStatus::Transcribing,
        input_progress_percent: 82,
        stage: "transcribing".to_owned(),
        message: "正在本机识别语音".to_owned(),
    };
    let json = serde_json::to_string(&event).unwrap();
    assert!(!json.contains("/Users/"));
    assert!(!json.contains("C:\\"));
    assert!(!json.contains("stderr"));
    assert!(!json.contains("modelPath"));
}

#[tokio::test]
async fn missing_resource_diagnostic_is_pathless_and_keeps_locked_default_profile_available() {
    let directory = tempdir().unwrap();
    let missing_root = directory.path().join("private-resource-location");
    let database = Database::open_in_memory().unwrap();
    database.migrate().unwrap();
    let components = LocalAsrRuntime::build(
        database,
        missing_root.clone(),
        "ffprobe".into(),
        directory.path().join("temporary"),
        Arc::new(IgnoreEvents),
    );
    let diagnostic = components.runtime.diagnose().await.unwrap();
    assert!(!diagnostic.ready);
    assert_eq!(diagnostic.engine_id, "whisper.cpp");
    assert!(
        !diagnostic
            .message
            .contains(missing_root.to_string_lossy().as_ref())
    );
    let profile = components.runtime.default_profile().unwrap();
    assert_eq!(profile.engine_id, "whisper.cpp");
    assert!(profile.model_id.contains("small"));
}
