use serde::{Deserialize, Serialize};

pub use dy_screen::model::StreamerSourceKind;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct NewStreamer {
    pub name: String,
    pub source_kind: StreamerSourceKind,
    pub source_url: String,
    pub profile_sec_uid: Option<String>,
    pub web_rid: Option<String>,
    pub room_url: Option<String>,
    pub room_id: Option<String>,
    pub monitor_enabled: bool,
}

impl NewStreamer {
    pub fn room(
        name: impl Into<String>,
        web_rid: impl Into<String>,
        room_id: impl Into<String>,
        monitor_enabled: bool,
    ) -> Self {
        let web_rid = web_rid.into();
        let room_url = format!("https://live.douyin.com/{web_rid}");
        Self {
            name: name.into(),
            source_kind: StreamerSourceKind::Room,
            source_url: room_url.clone(),
            profile_sec_uid: None,
            web_rid: Some(web_rid),
            room_url: Some(room_url),
            room_id: Some(room_id.into()),
            monitor_enabled,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CreateStreamerRequest {
    pub name: String,
    #[serde(alias = "roomUrl")]
    pub source_url: String,
    pub monitor_enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CommandError {
    pub code: String,
    pub message: String,
    pub field: Option<String>,
    pub existing_streamer_id: Option<i64>,
}

impl CommandError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            field: None,
            existing_streamer_id: None,
        }
    }

    pub fn field(mut self, field: impl Into<String>) -> Self {
        self.field = Some(field.into());
        self
    }

    pub fn existing_streamer(mut self, streamer_id: i64) -> Self {
        self.existing_streamer_id = Some(streamer_id);
        self
    }
}

impl std::fmt::Display for CommandError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for CommandError {}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Streamer {
    pub id: i64,
    pub name: String,
    pub source_kind: StreamerSourceKind,
    pub source_url: String,
    pub profile_sec_uid: Option<String>,
    pub web_rid: Option<String>,
    pub room_url: Option<String>,
    pub room_id: Option<String>,
    pub monitor_enabled: bool,
    pub archived: bool,
    pub live_status: String,
    pub monitor_status: String,
    pub last_checked_at: Option<String>,
    pub last_error: Option<String>,
    pub current_video_count: i64,
    pub history_video_count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DiscoveryBinding {
    Bound(Box<Streamer>),
    Merged {
        target_streamer_id: i64,
        removed_streamer_id: i64,
    },
    Conflict {
        target_streamer_id: i64,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AppSettings {
    pub output_root: String,
    pub quality: String,
    pub protocol: String,
    pub segment_seconds: u64,
    pub max_concurrent_recordings: usize,
    pub ffmpeg_path: String,
    pub ffprobe_path: String,
    pub notifications_enabled: bool,
    pub autostart_enabled: bool,
}

impl AppSettings {
    pub fn defaults() -> Self {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_owned());
        Self {
            output_root: format!("{home}/Downloads/dy-screen"),
            quality: "HD1".to_owned(),
            protocol: "flv".to_owned(),
            segment_seconds: 900,
            max_concurrent_recordings: 4,
            ffmpeg_path: "ffmpeg".to_owned(),
            ffprobe_path: "ffprobe".to_owned(),
            notifications_enabled: true,
            autostart_enabled: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RecordingSession {
    pub id: i64,
    pub streamer_id: i64,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub status: String,
    pub retry_count: i64,
    pub output_root: String,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct NewVideo {
    pub session_id: i64,
    pub path: String,
    pub started_at: Option<String>,
    pub ended_at: Option<String>,
    pub duration_seconds: Option<i64>,
    pub size_bytes: i64,
    pub audio_present: Option<bool>,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Video {
    pub id: i64,
    pub session_id: i64,
    pub streamer_id: i64,
    pub streamer_name: String,
    pub path: String,
    pub started_at: Option<String>,
    pub ended_at: Option<String>,
    pub duration_seconds: Option<i64>,
    pub size_bytes: i64,
    pub audio_present: Option<bool>,
    pub status: String,
    #[serde(default)]
    pub has_preview_cache: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct VideoPage {
    pub items: Vec<Video>,
    pub total: i64,
    pub page: u32,
    pub page_size: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct VideoFilter {
    pub status: Option<String>,
    pub search: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub current_only: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Dashboard {
    pub streamers: Vec<Streamer>,
    pub active_recordings: i64,
    pub current_video_count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentStatus {
    pub ffmpeg: bool,
    pub ffprobe: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MonitorEvent {
    pub kind: String,
    pub streamer_id: Option<i64>,
}
