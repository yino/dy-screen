use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct NewStreamer {
    pub name: String,
    pub room_url: String,
    pub room_id: String,
    pub monitor_enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CreateStreamerRequest {
    pub name: String,
    pub room_url: String,
    pub monitor_enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Streamer {
    pub id: i64,
    pub name: String,
    pub room_url: String,
    pub room_id: String,
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
