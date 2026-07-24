use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessChannel {
    Native,
    Browser,
    Monitor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessStage {
    Request,
    Response,
    Navigation,
    Probe,
    Fallback,
    Result,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessClassification {
    Pending,
    Live,
    Offline,
    AccessRestricted,
    VerificationRequired,
    LayoutChanged,
    EntryInvalid,
    RetryableError,
    Cancelled,
    SessionExpired,
}

impl AccessClassification {
    pub fn is_success(self) -> bool {
        matches!(self, Self::Live | Self::Offline)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessNextAction {
    None,
    FallbackToBrowser,
    StartRecording,
    ScheduleNextCheck,
    WaitForUser,
    Retry,
    ResumeWorkers,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccessMarkers {
    pub access_restricted: bool,
    pub pace_payload: bool,
    pub supported_room: bool,
}

/// 单次公开页面访问的白名单诊断记录。
///
/// 该结构有意不提供页面正文、Cookie、脚本或直播流 URL 字段，调用方只能记录排障所需的安全摘要。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccessDiagnosticEntry {
    pub timestamp: DateTime<Utc>,
    pub streamer_id: Option<i64>,
    pub web_rid: Option<String>,
    pub request_id: String,
    pub channel: AccessChannel,
    pub stage: AccessStage,
    pub classification: AccessClassification,
    pub http_status: Option<u16>,
    pub content_type: Option<String>,
    pub response_bytes: usize,
    pub markers: AccessMarkers,
    pub duration_ms: u64,
    pub failure_count: usize,
    pub next_action: AccessNextAction,
    pub next_retry_at: Option<DateTime<Utc>>,
}
