use std::fmt;

use serde::Deserialize;
use url::Url;

use crate::error::{RecorderError, Result};
use crate::resolver::{RoomInspection, parse_room_scripts, parse_room_scripts_for_web_rid};

pub const MAX_BROWSER_SNAPSHOT_SCRIPTS: usize = 32;
pub const MAX_BROWSER_SNAPSHOT_TOTAL_BYTES: usize = 2 * 1024 * 1024;
const MAX_BROWSER_SNAPSHOT_JSON_BYTES: usize = MAX_BROWSER_SNAPSHOT_TOTAL_BYTES + 64 * 1024;
const MAX_BROWSER_SNAPSHOT_TITLE_BYTES: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BrowserReadyState {
    Loading,
    Interactive,
    Complete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserSnapshotMarkers {
    pub access_restricted: bool,
    pub pace_payload: bool,
    #[serde(default)]
    pub room_offline: bool,
    #[serde(default)]
    pub snapshot_overflow: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UntrustedBrowserPageSnapshot {
    url: String,
    title: String,
    ready_state: BrowserReadyState,
    markers: BrowserSnapshotMarkers,
    scripts: Vec<String>,
}

#[derive(Clone)]
pub struct BrowserPageSnapshot {
    url: String,
    #[allow(dead_code)]
    title: String,
    ready_state: BrowserReadyState,
    markers: BrowserSnapshotMarkers,
    scripts: Vec<String>,
    script_bytes: usize,
}

impl BrowserPageSnapshot {
    pub fn from_json(json: &str) -> Result<Self> {
        if json.len() > MAX_BROWSER_SNAPSHOT_JSON_BYTES {
            return Err(RecorderError::InvalidBrowserSnapshot);
        }
        let raw: UntrustedBrowserPageSnapshot =
            serde_json::from_str(json).map_err(|_| RecorderError::InvalidBrowserSnapshot)?;
        if raw.title.len() > MAX_BROWSER_SNAPSHOT_TITLE_BYTES
            || raw.scripts.len() > MAX_BROWSER_SNAPSHOT_SCRIPTS
            || raw.markers.snapshot_overflow
        {
            return Err(RecorderError::InvalidBrowserSnapshot);
        }

        let url = normalize_browser_page_url(&raw.url)?;
        let mut script_bytes = 0usize;
        for script in &raw.scripts {
            if !script.contains("self.__pace_f.push") {
                return Err(RecorderError::InvalidBrowserSnapshot);
            }
            script_bytes = script_bytes
                .checked_add(script.len())
                .ok_or(RecorderError::InvalidBrowserSnapshot)?;
            if script_bytes > MAX_BROWSER_SNAPSHOT_TOTAL_BYTES {
                return Err(RecorderError::InvalidBrowserSnapshot);
            }
        }
        if raw.markers.pace_payload != !raw.scripts.is_empty() {
            return Err(RecorderError::InvalidBrowserSnapshot);
        }

        Ok(Self {
            url,
            title: raw.title,
            ready_state: raw.ready_state,
            markers: raw.markers,
            scripts: raw.scripts,
            script_bytes,
        })
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn ready_state(&self) -> BrowserReadyState {
        self.ready_state
    }

    pub fn matches_room_url(&self, room_url: &str) -> bool {
        normalize_browser_page_url(room_url).is_ok_and(|expected| expected == self.url)
    }

    pub fn markers(&self) -> BrowserSnapshotMarkers {
        self.markers
    }

    pub fn script_count(&self) -> usize {
        self.scripts.len()
    }

    pub fn script_bytes(&self) -> usize {
        self.script_bytes
    }
}

impl fmt::Debug for BrowserPageSnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BrowserPageSnapshot")
            .field("url", &self.url)
            .field("ready_state", &self.ready_state)
            .field("markers", &self.markers)
            .field("script_count", &self.scripts.len())
            .field("script_bytes", &self.script_bytes)
            .finish()
    }
}

pub fn parse_browser_snapshot(snapshot: &BrowserPageSnapshot) -> Result<RoomInspection> {
    match parse_room_scripts(snapshot.scripts.iter().map(String::as_str)) {
        Ok(inspection @ RoomInspection::Offline { .. }) => Ok(inspection),
        Ok(RoomInspection::Live(_)) if snapshot.markers.access_restricted => {
            Err(RecorderError::RoomAccessVerificationRequired)
        }
        Ok(inspection) => Ok(inspection),
        Err(RecorderError::UnsupportedPageLayout) if snapshot.markers.room_offline => {
            Ok(RoomInspection::Offline {
                room_id: snapshot_room_id(snapshot)?,
            })
        }
        Err(RecorderError::UnsupportedPageLayout) if snapshot.markers.access_restricted => {
            Err(RecorderError::RoomAccessVerificationRequired)
        }
        Err(error) => Err(error),
    }
}

pub fn parse_browser_snapshot_for_web_rid(
    snapshot: &BrowserPageSnapshot,
    expected_web_rid: &str,
) -> Result<RoomInspection> {
    match parse_room_scripts_for_web_rid(
        snapshot.scripts.iter().map(String::as_str),
        expected_web_rid,
    ) {
        Ok(inspection @ RoomInspection::Offline { .. }) => Ok(inspection),
        Ok(RoomInspection::Live(_)) if snapshot.markers.access_restricted => {
            Err(RecorderError::RoomAccessVerificationRequired)
        }
        Ok(inspection) => Ok(inspection),
        Err(RecorderError::UnsupportedPageLayout) if snapshot.markers.room_offline => {
            Ok(RoomInspection::Offline {
                room_id: expected_web_rid.to_owned(),
            })
        }
        Err(RecorderError::UnsupportedPageLayout) if snapshot.markers.access_restricted => {
            Err(RecorderError::RoomAccessVerificationRequired)
        }
        Err(error) => Err(error),
    }
}

fn snapshot_room_id(snapshot: &BrowserPageSnapshot) -> Result<String> {
    Url::parse(snapshot.url())
        .ok()
        .and_then(|url| {
            url.path_segments()?
                .find(|segment| !segment.is_empty())
                .map(str::to_owned)
        })
        .ok_or(RecorderError::InvalidBrowserSnapshot)
}

pub fn is_allowed_douyin_page_url(input: &str) -> bool {
    Url::parse(input).is_ok_and(|url| {
        url.scheme() == "https"
            && url
                .host_str()
                .is_some_and(|host| host == "douyin.com" || host.ends_with(".douyin.com"))
    })
}

fn normalize_browser_page_url(input: &str) -> Result<String> {
    let mut url = Url::parse(input).map_err(|_| RecorderError::InvalidBrowserSnapshot)?;
    if !is_allowed_douyin_page_url(input) {
        return Err(RecorderError::InvalidBrowserSnapshot);
    }
    url.set_query(None);
    url.set_fragment(None);
    Ok(url.to_string().trim_end_matches('/').to_owned())
}
