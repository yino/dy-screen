use std::time::Duration;

use scraper::{Html, Selector};
use serde_json::{Map, Value};
use url::Url;

use crate::error::{RecorderError, Result};
use crate::flight::decode_pace_payload;
use crate::model::{Protocol, RoomStreams, StreamVariant};

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
        let url = validate_room_url(room_url)?;
        let page = self
            .client
            .get(url)
            .header(reqwest::header::REFERER, "https://live.douyin.com/")
            .send()
            .await?
            .error_for_status()?
            .text()
            .await?;
        parse_room_inspection(&page)
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

pub fn parse_room_page(page: &str) -> Result<RoomStreams> {
    match parse_room_inspection(page)? {
        RoomInspection::Live(room) => Ok(room),
        RoomInspection::Offline { .. } => Err(RecorderError::RoomUnavailable),
    }
}

pub fn parse_room_inspection(page: &str) -> Result<RoomInspection> {
    let document = Html::parse_document(page);
    let selector = Selector::parse("script").expect("static script selector");
    let mut offline_room_id = None;

    for script in document.select(&selector) {
        let text = script.text().collect::<String>();
        if !text.contains("self.__pace_f.push") {
            continue;
        }
        let Some(payload) = decode_pace_payload(&text) else {
            continue;
        };

        if let Some(room) = find_room_object(&payload, true) {
            return normalize_room(room).map(RoomInspection::Live);
        }
        if let Some(room) = find_room_object(&payload, false)
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

fn find_room_object(value: &Value, require_stream: bool) -> Option<&Map<String, Value>> {
    match value {
        Value::Object(object) => {
            let is_room = if require_stream {
                object.get("stream_url").is_some_and(Value::is_object)
            } else {
                object.contains_key("id_str") && object.contains_key("status")
            };
            if is_room {
                return Some(object);
            }
            object
                .values()
                .find_map(|child| find_room_object(child, require_stream))
        }
        Value::Array(values) => values
            .iter()
            .find_map(|child| find_room_object(child, require_stream)),
        _ => None,
    }
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
