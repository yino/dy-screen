use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use url::Url;

use crate::error::{RecorderError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StreamerSourceKind {
    Profile,
    Room,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileUrl {
    pub source_kind: StreamerSourceKind,
    pub source_url: String,
    pub profile_sec_uid: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileIdentity {
    pub profile_sec_uid: String,
    pub display_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileRoom {
    pub web_rid: String,
    pub room_url: String,
    pub room_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ProfileInspection {
    Offline {
        identity: ProfileIdentity,
    },
    Live {
        identity: ProfileIdentity,
        room: ProfileRoom,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileDiscoveryErrorKind {
    Retryable,
    AccessRestricted,
    UnsupportedPageLayout,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    Flv,
    Hls,
}

impl fmt::Display for Protocol {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Flv => "flv",
            Self::Hls => "hls",
        })
    }
}

impl FromStr for Protocol {
    type Err = String;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        match value.to_ascii_lowercase().as_str() {
            "flv" => Ok(Self::Flv),
            "hls" => Ok(Self::Hls),
            _ => Err("expected flv or hls".to_owned()),
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct StreamVariant {
    pub quality: String,
    pub protocol: Protocol,
    pub url: String,
}

impl fmt::Debug for StreamVariant {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StreamVariant")
            .field("quality", &self.quality)
            .field("protocol", &self.protocol)
            .field("url", &redact_url(&self.url))
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomStreams {
    pub room_id: String,
    pub status: Option<i64>,
    pub default_quality: Option<String>,
    pub variants: Vec<StreamVariant>,
}

impl RoomStreams {
    pub fn select(
        &self,
        requested_quality: Option<&str>,
        requested_protocol: Option<Protocol>,
    ) -> Result<SelectedStream> {
        if self.variants.is_empty() {
            return Err(RecorderError::NoStreamVariant);
        }

        let requested_available = requested_quality.filter(|quality| {
            self.variants
                .iter()
                .any(|variant| variant.quality == *quality)
        });
        let default_available = self.default_quality.as_deref().filter(|quality| {
            self.variants
                .iter()
                .any(|variant| variant.quality == *quality)
        });
        let quality = requested_available
            .or(default_available)
            .or_else(|| highest_quality(&self.variants))
            .ok_or(RecorderError::NoStreamVariant)?;

        let preferred_protocol = requested_protocol.unwrap_or(Protocol::Flv);
        let variant = self
            .variants
            .iter()
            .find(|variant| variant.quality == quality && variant.protocol == preferred_protocol)
            .or_else(|| {
                self.variants
                    .iter()
                    .find(|variant| variant.quality == quality)
            })
            .ok_or(RecorderError::NoStreamVariant)?;

        let fell_back = requested_quality.is_some_and(|requested| requested != variant.quality)
            || requested_protocol.is_some_and(|requested| requested != variant.protocol);

        Ok(SelectedStream {
            quality: variant.quality.clone(),
            protocol: variant.protocol,
            url: variant.url.clone(),
            fell_back,
        })
    }
}

fn highest_quality(variants: &[StreamVariant]) -> Option<&str> {
    const ORDER: &[&str] = &["FULL_HD1", "ORIGIN", "HD1", "SD1", "SD2"];
    ORDER
        .iter()
        .find(|quality| variants.iter().any(|variant| variant.quality == **quality))
        .copied()
        .or_else(|| variants.first().map(|variant| variant.quality.as_str()))
}

#[derive(Clone, PartialEq, Eq)]
pub struct SelectedStream {
    pub quality: String,
    pub protocol: Protocol,
    pub url: String,
    pub fell_back: bool,
}

impl fmt::Debug for SelectedStream {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SelectedStream")
            .field("quality", &self.quality)
            .field("protocol", &self.protocol)
            .field("url", &self.redacted_url())
            .field("fell_back", &self.fell_back)
            .finish()
    }
}

impl SelectedStream {
    pub fn redacted_url(&self) -> String {
        redact_url(&self.url)
    }

    pub fn summary(&self) -> StreamSummary {
        StreamSummary {
            quality: self.quality.clone(),
            protocol: self.protocol,
            endpoint: self.redacted_url(),
            fell_back: self.fell_back,
        }
    }
}

pub fn redact_url(raw: &str) -> String {
    match Url::parse(raw) {
        Ok(mut url) => {
            url.set_query(None);
            url.set_fragment(None);
            url.into()
        }
        Err(_) => "<redacted-stream-url>".to_owned(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamSummary {
    pub quality: String,
    pub protocol: Protocol,
    pub endpoint: String,
    pub fell_back: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordingRequest {
    pub room_url: String,
    pub quality: Option<String>,
    pub protocol: Option<Protocol>,
}

impl RecordingRequest {
    pub fn new(room_url: impl Into<String>) -> Self {
        Self {
            room_url: room_url.into(),
            quality: None,
            protocol: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobResult {
    pub room_url: String,
    pub room_id: Option<String>,
    pub success: bool,
    pub selected: Option<StreamSummary>,
    pub output_dir: Option<PathBuf>,
    pub segments: Vec<PathBuf>,
    pub partial_segments: Vec<PathBuf>,
    pub audio_present: Option<bool>,
    pub error: Option<String>,
}

impl JobResult {
    pub fn success(room_url: impl Into<String>) -> Self {
        Self {
            room_url: room_url.into(),
            room_id: None,
            success: true,
            selected: None,
            output_dir: None,
            segments: Vec::new(),
            partial_segments: Vec::new(),
            audio_present: None,
            error: None,
        }
    }

    pub fn failure(room_url: impl Into<String>, error: impl Into<String>) -> Self {
        Self {
            room_url: room_url.into(),
            room_id: None,
            success: false,
            selected: None,
            output_dir: None,
            segments: Vec::new(),
            partial_segments: Vec::new(),
            audio_present: None,
            error: Some(error.into()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum JobEvent {
    Queued {
        room_url: String,
    },
    Resolving {
        room_url: String,
    },
    Resolved {
        room_url: String,
        room_id: String,
        selected: StreamSummary,
    },
    RecordingStarted {
        room_url: String,
        room_id: String,
        output_dir: PathBuf,
    },
    SegmentFinalized {
        room_url: String,
        room_id: String,
        path: PathBuf,
        started_at: Option<String>,
        ended_at: Option<String>,
        duration_seconds: Option<i64>,
        audio_present: Option<bool>,
    },
    Finished {
        result: JobResult,
    },
}

pub trait EventSink: Send + Sync {
    fn emit(&self, event: JobEvent) -> bool;
}

#[derive(Default)]
pub struct NoopEventSink;

impl EventSink for NoopEventSink {
    fn emit(&self, _event: JobEvent) -> bool {
        true
    }
}

pub fn noop_event_sink() -> Arc<dyn EventSink> {
    Arc::new(NoopEventSink)
}
