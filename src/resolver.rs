use std::time::Duration;

use scraper::{Html, Selector};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use url::Url;

use crate::error::{RecorderError, Result};
use crate::flight::decode_pace_payload;
use crate::model::{Protocol, RoomStreams, StreamVariant};
use crate::profile_resolver::is_access_restricted_page;

pub use crate::access::{
    AccessClassification as RoomDiagnosticClassification, AccessMarkers as RoomDiagnosticMarkers,
};

pub const DEFAULT_USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/138.0 Safari/537.36";

#[derive(Clone)]
pub struct StreamResolver {
    client: reqwest::Client,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoomInspection {
    Live(RoomStreams),
    Offline { room_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoomDiagnostic {
    pub room_url: String,
    pub http_status: Option<u16>,
    pub content_type: Option<String>,
    pub response_bytes: usize,
    pub markers: RoomDiagnosticMarkers,
    pub classification: RoomDiagnosticClassification,
    pub error: Option<String>,
}

#[derive(Debug)]
pub struct RoomInspectionAttempt {
    pub inspection: Result<RoomInspection>,
    pub diagnostic: RoomDiagnostic,
}

impl RoomInspection {
    pub fn room_id(&self) -> &str {
        match self {
            Self::Live(room) => &room.room_id,
            Self::Offline { room_id } => room_id,
        }
    }
}

impl StreamResolver {
    pub fn new() -> Result<Self> {
        let client = reqwest::Client::builder()
            .user_agent(DEFAULT_USER_AGENT)
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .build()?;
        Ok(Self { client })
    }

    pub async fn resolve(&self, room_url: &str) -> Result<RoomStreams> {
        match self.inspect(room_url).await? {
            RoomInspection::Live(room) => Ok(room),
            RoomInspection::Offline { .. } => Err(RecorderError::RoomUnavailable),
        }
    }

    pub async fn inspect(&self, room_url: &str) -> Result<RoomInspection> {
        self.inspect_attempt(room_url).await.inspection
    }

    pub async fn diagnose(&self, room_url: &str) -> RoomDiagnostic {
        self.inspect_attempt(room_url).await.diagnostic
    }

    pub async fn inspect_attempt(&self, room_url: &str) -> RoomInspectionAttempt {
        let url = match validate_room_url(room_url) {
            Ok(url) => url,
            Err(error) => {
                let diagnostic = diagnostic_without_response(
                    room_url,
                    RoomDiagnosticClassification::EntryInvalid,
                    error.safe_message(),
                );
                return RoomInspectionAttempt {
                    inspection: Err(error),
                    diagnostic,
                };
            }
        };
        let safe_url = canonical_room_url(&url);
        let expected_web_rid = room_web_rid(&url);
        let response = match self
            .client
            .get(url)
            .header(reqwest::header::REFERER, "https://live.douyin.com/")
            .send()
            .await
        {
            Ok(response) => response,
            Err(error) => {
                return RoomInspectionAttempt {
                    inspection: Err(RecorderError::PageRequest(error)),
                    diagnostic: diagnostic_without_response(
                        &safe_url,
                        RoomDiagnosticClassification::RetryableError,
                        "访问公开抖音直播间失败".to_owned(),
                    ),
                };
            }
        };
        let status = response.status();
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        match response.bytes().await {
            Ok(body) => {
                let page = String::from_utf8_lossy(&body);
                let diagnostic =
                    diagnose_room_response(&safe_url, status, content_type.as_deref(), &page);
                let inspection = classify_room_http_status(status).and_then(|_| {
                    expected_web_rid.as_deref().map_or_else(
                        || parse_room_inspection(&page),
                        |web_rid| parse_room_inspection_for_web_rid(&page, web_rid),
                    )
                });
                RoomInspectionAttempt {
                    inspection,
                    diagnostic,
                }
            }
            Err(error) => RoomInspectionAttempt {
                inspection: Err(RecorderError::PageRequest(error)),
                diagnostic: RoomDiagnostic {
                    room_url: safe_url,
                    http_status: Some(status.as_u16()),
                    content_type,
                    response_bytes: 0,
                    markers: empty_diagnostic_markers(),
                    classification: RoomDiagnosticClassification::RetryableError,
                    error: Some("读取公开抖音直播间响应失败".to_owned()),
                },
            },
        }
    }
}

pub fn classify_room_http_status(status: reqwest::StatusCode) -> Result<()> {
    if status.is_success() {
        return Ok(());
    }
    match status.as_u16() {
        401 | 403 | 418 | 429 => Err(RecorderError::RoomAccessRestricted),
        status => Err(RecorderError::RoomHttpStatus { status }),
    }
}

pub fn diagnose_room_response(
    room_url: &str,
    status: reqwest::StatusCode,
    content_type: Option<&str>,
    page: &str,
) -> RoomDiagnostic {
    let room_url = Url::parse(room_url)
        .map(|url| canonical_room_url(&url))
        .unwrap_or_else(|_| "<invalid>".to_owned());
    let access_restricted = is_access_restricted_page(page);
    let pace_payload = page.contains("self.__pace_f.push");
    let parsed = if status.is_success() && !access_restricted {
        Url::parse(&room_url)
            .ok()
            .and_then(|url| room_web_rid(&url))
            .map_or_else(
                || parse_room_inspection(page),
                |web_rid| parse_room_inspection_for_web_rid(page, &web_rid),
            )
            .ok()
    } else {
        None
    };
    let markers = RoomDiagnosticMarkers {
        access_restricted,
        pace_payload,
        supported_room: parsed.is_some(),
    };

    let (classification, error) = match classify_room_http_status(status) {
        Err(RecorderError::RoomAccessRestricted) => (
            RoomDiagnosticClassification::AccessRestricted,
            Some(RecorderError::RoomAccessRestricted.safe_message()),
        ),
        Err(RecorderError::RoomHttpStatus { status: 404 | 410 }) => (
            RoomDiagnosticClassification::EntryInvalid,
            Some(format!("直播入口已失效（HTTP {}）", status.as_u16())),
        ),
        Err(error) => (
            RoomDiagnosticClassification::RetryableError,
            Some(error.safe_message()),
        ),
        Ok(()) if access_restricted => (
            RoomDiagnosticClassification::AccessRestricted,
            Some(RecorderError::RoomAccessRestricted.safe_message()),
        ),
        Ok(()) => match parsed {
            Some(RoomInspection::Live(_)) => (RoomDiagnosticClassification::Live, None),
            Some(RoomInspection::Offline { .. }) => (RoomDiagnosticClassification::Offline, None),
            None => (
                RoomDiagnosticClassification::LayoutChanged,
                Some(RecorderError::UnsupportedPageLayout.safe_message()),
            ),
        },
    };

    RoomDiagnostic {
        room_url,
        http_status: Some(status.as_u16()),
        content_type: content_type.map(str::to_owned),
        response_bytes: page.len(),
        markers,
        classification,
        error,
    }
}

fn canonical_room_url(url: &Url) -> String {
    let mut safe = url.clone();
    safe.set_query(None);
    safe.set_fragment(None);
    safe.to_string().trim_end_matches('/').to_owned()
}

fn diagnostic_without_response(
    room_url: &str,
    classification: RoomDiagnosticClassification,
    error: String,
) -> RoomDiagnostic {
    let safe_url = Url::parse(room_url)
        .map(|url| canonical_room_url(&url))
        .unwrap_or_else(|_| "<invalid>".to_owned());
    RoomDiagnostic {
        room_url: safe_url,
        http_status: None,
        content_type: None,
        response_bytes: 0,
        markers: empty_diagnostic_markers(),
        classification,
        error: Some(error),
    }
}

fn empty_diagnostic_markers() -> RoomDiagnosticMarkers {
    RoomDiagnosticMarkers {
        access_restricted: false,
        pace_payload: false,
        supported_room: false,
    }
}

pub fn validate_room_url(input: &str) -> Result<Url> {
    let url = Url::parse(input).map_err(|error| RecorderError::InvalidRoomUrl {
        reason: error.to_string(),
    })?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(RecorderError::InvalidRoomUrl {
            reason: "only http and https are supported".to_owned(),
        });
    }
    if url.host_str() != Some("live.douyin.com") {
        return Err(RecorderError::UnsupportedRoomUrl {
            url: input.to_owned(),
        });
    }
    Ok(url)
}

fn room_web_rid(url: &Url) -> Option<String> {
    url.path_segments()?
        .find(|segment| !segment.is_empty())
        .map(str::to_owned)
}

pub fn parse_room_page(page: &str) -> Result<RoomStreams> {
    match parse_room_inspection(page)? {
        RoomInspection::Live(room) => Ok(room),
        RoomInspection::Offline { .. } => Err(RecorderError::RoomUnavailable),
    }
}

pub fn parse_room_inspection(page: &str) -> Result<RoomInspection> {
    parse_room_inspection_with_identity(page, None)
}

pub fn parse_room_inspection_for_web_rid(
    page: &str,
    expected_web_rid: &str,
) -> Result<RoomInspection> {
    parse_room_inspection_with_identity(page, Some(expected_web_rid))
}

fn parse_room_inspection_with_identity(
    page: &str,
    expected_web_rid: Option<&str>,
) -> Result<RoomInspection> {
    if is_access_restricted_page(page) {
        return Err(RecorderError::RoomAccessRestricted);
    }
    let document = Html::parse_document(page);
    let selector = Selector::parse("script").expect("static script selector");
    let scripts = document.select(&selector).filter_map(|script| {
        let text = script.text().collect::<String>();
        text.contains("self.__pace_f.push").then_some(text)
    });
    parse_room_scripts_with_identity(scripts, expected_web_rid)
}

pub fn parse_room_scripts<I, S>(scripts: I) -> Result<RoomInspection>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    parse_room_scripts_with_identity(scripts, None)
}

pub fn parse_room_scripts_for_web_rid<I, S>(
    scripts: I,
    expected_web_rid: &str,
) -> Result<RoomInspection>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    parse_room_scripts_with_identity(scripts, Some(expected_web_rid))
}

fn parse_room_scripts_with_identity<I, S>(
    scripts: I,
    expected_web_rid: Option<&str>,
) -> Result<RoomInspection>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut offline_room_id = None;

    for text in scripts {
        let Some(payload) = decode_pace_payload(text.as_ref()) else {
            continue;
        };

        if let Some(room) = find_room_object(&payload, true, expected_web_rid) {
            return normalize_room(room).map(RoomInspection::Live);
        }
        if let Some(room) = find_room_object(&payload, false, expected_web_rid)
            && let Some(room_id) = room.get("id_str").and_then(Value::as_str)
        {
            offline_room_id = Some(room_id.to_owned());
        }
    }

    if let Some(room_id) = offline_room_id {
        Ok(RoomInspection::Offline { room_id })
    } else {
        Err(RecorderError::UnsupportedPageLayout)
    }
}

fn find_room_object<'a>(
    value: &'a Value,
    require_stream: bool,
    expected_web_rid: Option<&str>,
) -> Option<&'a Map<String, Value>> {
    find_room_object_with_identity(value, require_stream, expected_web_rid, false)
}

fn find_room_object_with_identity<'a>(
    value: &'a Value,
    require_stream: bool,
    expected_web_rid: Option<&str>,
    ancestor_matches: bool,
) -> Option<&'a Map<String, Value>> {
    match value {
        Value::Object(object) => {
            let identity_matches = ancestor_matches
                || expected_web_rid.is_some_and(|expected| room_matches_web_rid(object, expected));
            let is_room = if require_stream {
                object.get("stream_url").is_some_and(Value::is_object)
            } else {
                object.contains_key("id_str") && object.contains_key("status")
            };
            if is_room && (expected_web_rid.is_none() || identity_matches) {
                return Some(object);
            }
            object.values().find_map(|child| {
                find_room_object_with_identity(
                    child,
                    require_stream,
                    expected_web_rid,
                    identity_matches,
                )
            })
        }
        Value::Array(values) => values.iter().find_map(|child| {
            find_room_object_with_identity(
                child,
                require_stream,
                expected_web_rid,
                ancestor_matches,
            )
        }),
        _ => None,
    }
}

fn room_matches_web_rid(room: &Map<String, Value>, expected_web_rid: &str) -> bool {
    let direct = room.get("web_rid").and_then(Value::as_str);
    let owner = room
        .get("owner")
        .and_then(Value::as_object)
        .and_then(|owner| owner.get("web_rid"))
        .and_then(Value::as_str);
    direct.or(owner) == Some(expected_web_rid)
}

fn normalize_room(room: &Map<String, Value>) -> Result<RoomStreams> {
    let room_id = room
        .get("id_str")
        .and_then(Value::as_str)
        .unwrap_or("unknown-room")
        .to_owned();
    let status = room.get("status").and_then(Value::as_i64);
    let stream_url = room
        .get("stream_url")
        .and_then(Value::as_object)
        .ok_or(RecorderError::RoomUnavailable)?;
    let default_quality = stream_url
        .get("default_resolution")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let mut variants = Vec::new();
    append_variants(&mut variants, stream_url.get("flv_pull_url"), Protocol::Flv);
    append_variants(
        &mut variants,
        stream_url
            .get("hls_pull_url_map")
            .or_else(|| stream_url.get("hls_pull_url")),
        Protocol::Hls,
    );
    if variants.is_empty() {
        return Err(RecorderError::RoomUnavailable);
    }
    Ok(RoomStreams {
        room_id,
        status,
        default_quality,
        variants,
    })
}

fn append_variants(variants: &mut Vec<StreamVariant>, value: Option<&Value>, protocol: Protocol) {
    let Some(map) = value.and_then(Value::as_object) else {
        return;
    };
    for (quality, url) in map {
        if let Some(url) = url.as_str().filter(|url| !url.is_empty()) {
            variants.push(StreamVariant {
                quality: quality.clone(),
                protocol,
                url: url.to_owned(),
            });
        }
    }
}
