use std::collections::BTreeSet;
use std::time::Duration;

use scraper::{Html, Selector};
use serde_json::{Map, Value};
use url::Url;

use crate::error::{RecorderError, Result};
use crate::flight::decode_pace_payload;
use crate::model::{
    ProfileIdentity, ProfileInspection, ProfileRoom, ProfileUrl, StreamerSourceKind,
};
use crate::resolver::DEFAULT_USER_AGENT;

#[derive(Clone)]
pub struct ProfileResolver {
    client: reqwest::Client,
}

impl ProfileResolver {
    pub fn new() -> Result<Self> {
        let client = reqwest::Client::builder()
            .user_agent(DEFAULT_USER_AGENT)
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .build()?;
        Ok(Self { client })
    }

    pub fn from_client(client: reqwest::Client) -> Self {
        Self { client }
    }

    pub async fn inspect(&self, profile_url: &str) -> Result<ProfileInspection> {
        let profile = validate_profile_url(profile_url)?;
        let response = self
            .client
            .get(&profile.source_url)
            .header(reqwest::header::REFERER, "https://www.douyin.com/")
            .send()
            .await
            .map_err(|source| RecorderError::ProfilePageRequest { source })?;

        classify_profile_http_status(response.status())?;

        let page = response
            .text()
            .await
            .map_err(|source| RecorderError::ProfilePageRequest { source })?;
        parse_profile_page(&page, profile)
    }
}

pub fn validate_profile_url(input: &str) -> Result<ProfileUrl> {
    let url = Url::parse(input).map_err(|_| RecorderError::InvalidProfileUrl {
        reason: "URL 格式不正确".to_owned(),
    })?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(RecorderError::InvalidProfileUrl {
            reason: "仅支持 HTTP 或 HTTPS".to_owned(),
        });
    }
    if !matches!(url.host_str(), Some("douyin.com" | "www.douyin.com")) {
        return Err(RecorderError::UnsupportedProfileUrl);
    }

    let mut segments = url
        .path_segments()
        .ok_or(RecorderError::UnsupportedProfileUrl)?;
    if segments.next() != Some("user") {
        return Err(RecorderError::UnsupportedProfileUrl);
    }
    let profile_sec_uid = segments
        .next()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| RecorderError::InvalidProfileUrl {
            reason: "缺少个人主页身份".to_owned(),
        })?;
    if segments.next().is_some() || !is_profile_sec_uid(profile_sec_uid) {
        return Err(RecorderError::InvalidProfileUrl {
            reason: "个人主页身份格式不正确".to_owned(),
        });
    }

    Ok(ProfileUrl {
        source_kind: StreamerSourceKind::Profile,
        source_url: format!("https://www.douyin.com/user/{profile_sec_uid}"),
        profile_sec_uid: profile_sec_uid.to_owned(),
    })
}

pub fn parse_profile_page(page: &str, profile: ProfileUrl) -> Result<ProfileInspection> {
    if is_access_restricted_page(page) {
        return Err(RecorderError::ProfileAccessRestricted);
    }
    let document = Html::parse_document(page);
    let script_selector = Selector::parse("script").expect("固定 script 选择器必须有效");
    let mut payloads = Vec::new();

    for script in document.select(&script_selector) {
        let text = script.text().collect::<String>();
        if !text.contains("self.__pace_f.push") {
            continue;
        }
        let Some(payload) = decode_pace_payload(&text) else {
            continue;
        };
        payloads.push(payload);
    }

    let parsed_identity = payloads
        .iter()
        .find_map(|payload| find_profile_identity(payload, &profile.profile_sec_uid))
        .ok_or(RecorderError::UnsupportedPageLayout)?;
    let structured_room = payloads.iter().find_map(|payload| {
        find_profile_room(
            payload,
            &parsed_identity.identity.profile_sec_uid,
            parsed_identity.current_room_id.as_deref(),
        )
    });
    if let Some(room) = structured_room {
        return Ok(ProfileInspection::Live {
            identity: parsed_identity.identity,
            room,
        });
    }
    if let Some(room) = find_unique_room_link(&document) {
        return Ok(ProfileInspection::Live {
            identity: parsed_identity.identity,
            room,
        });
    }
    Ok(ProfileInspection::Offline {
        identity: parsed_identity.identity,
    })
}

fn is_access_restricted_page(page: &str) -> bool {
    (page.contains("byted_acrawler")
        && (page.contains("__ac_nonce") || page.contains("__ac_signature")))
        || page.contains("captchaBody")
        || page.contains("verify-center")
}

fn is_profile_sec_uid(value: &str) -> bool {
    value
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
}

struct ParsedProfileIdentity {
    identity: ProfileIdentity,
    current_room_id: Option<String>,
}

fn find_profile_identity(value: &Value, expected_sec_uid: &str) -> Option<ParsedProfileIdentity> {
    match value {
        Value::Object(object) => {
            if object.get("sec_uid").and_then(Value::as_str) == Some(expected_sec_uid) {
                return Some(ParsedProfileIdentity {
                    identity: ProfileIdentity {
                        profile_sec_uid: expected_sec_uid.to_owned(),
                        display_name: object
                            .get("nickname")
                            .and_then(Value::as_str)
                            .filter(|value| !value.trim().is_empty())
                            .map(str::to_owned),
                    },
                    current_room_id: object
                        .get("roomIdStr")
                        .or_else(|| object.get("room_id_str"))
                        .and_then(Value::as_str)
                        .filter(|value| *value != "0" && is_numeric_identity(value))
                        .map(str::to_owned),
                });
            }
            object
                .values()
                .find_map(|child| find_profile_identity(child, expected_sec_uid))
        }
        Value::Array(values) => values
            .iter()
            .find_map(|child| find_profile_identity(child, expected_sec_uid)),
        _ => None,
    }
}

fn find_profile_room(
    value: &Value,
    expected_sec_uid: &str,
    expected_room_id: Option<&str>,
) -> Option<ProfileRoom> {
    match value {
        Value::Object(object) => room_from_object(object, expected_sec_uid, expected_room_id)
            .or_else(|| {
                object
                    .values()
                    .find_map(|child| find_profile_room(child, expected_sec_uid, expected_room_id))
            }),
        Value::Array(values) => values
            .iter()
            .find_map(|child| find_profile_room(child, expected_sec_uid, expected_room_id)),
        _ => None,
    }
}

fn room_from_object(
    object: &Map<String, Value>,
    expected_sec_uid: &str,
    expected_room_id: Option<&str>,
) -> Option<ProfileRoom> {
    if !object.get("stream_url").is_some_and(Value::is_object) {
        return None;
    }
    let owner = object.get("owner").and_then(Value::as_object)?;
    let room_id = object
        .get("id_str")
        .or_else(|| object.get("roomIdStr"))
        .and_then(Value::as_str)
        .filter(|value| is_numeric_identity(value));
    let owner_matches = owner
        .get("sec_uid")
        .or_else(|| owner.get("secUid"))
        .and_then(Value::as_str)
        == Some(expected_sec_uid);
    let room_matches = expected_room_id.is_some() && room_id == expected_room_id;
    if !owner_matches && !room_matches {
        return None;
    }
    let web_rid = owner
        .get("web_rid")
        .and_then(Value::as_str)
        .filter(|value| is_numeric_identity(value))?;
    Some(ProfileRoom {
        web_rid: web_rid.to_owned(),
        room_url: format!("https://live.douyin.com/{web_rid}"),
        room_id: room_id.map(str::to_owned),
    })
}

fn find_unique_room_link(document: &Html) -> Option<ProfileRoom> {
    let selector = Selector::parse("a[href]").expect("固定链接选择器必须有效");
    let web_rids = document
        .select(&selector)
        .filter_map(|element| element.value().attr("href"))
        .filter_map(web_rid_from_room_link)
        .collect::<BTreeSet<_>>();
    if web_rids.len() != 1 {
        return None;
    }
    let web_rid = web_rids.into_iter().next()?;
    Some(ProfileRoom {
        room_url: format!("https://live.douyin.com/{web_rid}"),
        web_rid,
        room_id: None,
    })
}

fn web_rid_from_room_link(input: &str) -> Option<String> {
    let url = Url::parse(input).ok()?;
    if url.host_str() != Some("live.douyin.com") {
        return None;
    }
    let mut segments = url.path_segments()?;
    let web_rid = segments.next().filter(|value| is_numeric_identity(value))?;
    if segments.next().is_some() {
        return None;
    }
    Some(web_rid.to_owned())
}

fn is_numeric_identity(value: &str) -> bool {
    !value.is_empty() && value.chars().all(|character| character.is_ascii_digit())
}

fn classify_profile_http_status(status: reqwest::StatusCode) -> Result<()> {
    if matches!(status.as_u16(), 401 | 403 | 429) {
        return Err(RecorderError::ProfileAccessRestricted);
    }
    if !status.is_success() {
        return Err(RecorderError::ProfileHttpStatus {
            status: status.as_u16(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use reqwest::StatusCode;

    use super::classify_profile_http_status;
    use crate::error::RecorderError;

    #[test]
    fn http_status_distinguishes_access_controls_and_retryable_failures() {
        assert!(matches!(
            classify_profile_http_status(StatusCode::FORBIDDEN),
            Err(RecorderError::ProfileAccessRestricted)
        ));
        assert!(matches!(
            classify_profile_http_status(StatusCode::TOO_MANY_REQUESTS),
            Err(RecorderError::ProfileAccessRestricted)
        ));
        assert!(matches!(
            classify_profile_http_status(StatusCode::SERVICE_UNAVAILABLE),
            Err(RecorderError::ProfileHttpStatus { status: 503 })
        ));
        assert!(classify_profile_http_status(StatusCode::OK).is_ok());
    }
}
