use chrono::Utc;
use dy_screen::access::{
    AccessChannel, AccessClassification, AccessDiagnosticEntry, AccessMarkers, AccessNextAction,
    AccessStage,
};
use dy_screen::browser_snapshot::{
    BrowserPageSnapshot, MAX_BROWSER_SNAPSHOT_SCRIPTS, MAX_BROWSER_SNAPSHOT_TOTAL_BYTES,
    parse_browser_snapshot, parse_browser_snapshot_for_web_rid,
};
use dy_screen::error::RecorderError;
use dy_screen::resolver::{RoomInspection, parse_room_scripts};

const LIVE_PAGE: &str = include_str!("fixtures/live_room.html");
const OFFLINE_PAGE: &str = include_str!("fixtures/offline_room.html");

fn pace_script(page: &str) -> String {
    let start = page
        .find("self.__pace_f.push")
        .expect("fixture contains Flight data");
    let end = page[start..]
        .find("</script>")
        .map(|offset| start + offset)
        .expect("fixture closes script");
    page[start..end].trim().to_owned()
}

fn snapshot_json(url: &str, title: &str, access_restricted: bool, scripts: &[String]) -> String {
    serde_json::json!({
        "url": url,
        "title": title,
        "readyState": "complete",
        "markers": {
            "accessRestricted": access_restricted,
            "pacePayload": !scripts.is_empty()
        },
        "scripts": scripts
    })
    .to_string()
}

#[test]
fn parses_live_browser_snapshot_with_the_shared_flight_parser() {
    let json = snapshot_json(
        "https://live.douyin.com/292895634635?verifyFp=must-not-leak#room",
        "脱敏直播间",
        false,
        &[pace_script(LIVE_PAGE)],
    );
    let snapshot = BrowserPageSnapshot::from_json(&json).expect("valid browser snapshot");
    let inspection = parse_browser_snapshot(&snapshot).expect("live room");

    let RoomInspection::Live(room) = inspection else {
        panic!("expected live inspection");
    };
    assert_eq!(snapshot.url(), "https://live.douyin.com/292895634635");
    assert!(snapshot.matches_room_url("https://live.douyin.com/292895634635?anchor_id=ignored"));
    assert!(!snapshot.matches_room_url("https://live.douyin.com/168376497175"));
    assert_eq!(room.room_id, "292895634635");
    assert_eq!(room.variants.len(), 4);
    assert!(matches!(
        parse_browser_snapshot_for_web_rid(&snapshot, "different-room"),
        Err(RecorderError::UnsupportedPageLayout)
    ));

    let debug = format!("{snapshot:?}");
    assert!(!debug.contains("auth_key"));
    assert!(!debug.contains("top-secret"));
    assert!(!debug.contains("verifyFp"));
}

#[test]
fn parses_offline_browser_snapshot_and_direct_script_collection() {
    let script = pace_script(OFFLINE_PAGE);
    let snapshot = BrowserPageSnapshot::from_json(&snapshot_json(
        "https://live.douyin.com/offline-room",
        "已结束",
        false,
        std::slice::from_ref(&script),
    ))
    .expect("valid browser snapshot");

    assert_eq!(
        parse_browser_snapshot(&snapshot).expect("offline room"),
        RoomInspection::Offline {
            room_id: "offline-room".to_owned()
        }
    );
    assert_eq!(
        parse_room_scripts([script.as_str()]).expect("shared script parser"),
        RoomInspection::Offline {
            room_id: "offline-room".to_owned()
        }
    );
}

#[test]
fn browser_challenge_requires_manual_verification_without_leaking_page_data() {
    let json = snapshot_json(
        "https://live.douyin.com/703940802949",
        "验证码中间页 secret-title",
        true,
        &[],
    );
    let snapshot = BrowserPageSnapshot::from_json(&json).expect("valid challenge snapshot");
    let error = parse_browser_snapshot(&snapshot).unwrap_err();

    assert!(matches!(
        error,
        RecorderError::RoomAccessVerificationRequired
    ));
    assert!(!error.safe_message().contains("secret-title"));
    assert!(!format!("{snapshot:?}").contains("secret-title"));
}

#[test]
fn browser_snapshot_rejects_unknown_layout_and_untrusted_or_oversized_input() {
    let unknown = BrowserPageSnapshot::from_json(&snapshot_json(
        "https://live.douyin.com/703940802949",
        "普通页面",
        false,
        &[],
    ))
    .expect("valid empty snapshot");
    assert!(matches!(
        parse_browser_snapshot(&unknown),
        Err(RecorderError::UnsupportedPageLayout)
    ));

    let external = BrowserPageSnapshot::from_json(&snapshot_json(
        "https://example.com/703940802949",
        "伪造页面",
        false,
        &[pace_script(LIVE_PAGE)],
    ))
    .unwrap_err();
    assert!(matches!(external, RecorderError::InvalidBrowserSnapshot));

    let too_many = vec![pace_script(LIVE_PAGE); MAX_BROWSER_SNAPSHOT_SCRIPTS + 1];
    assert!(matches!(
        BrowserPageSnapshot::from_json(&snapshot_json(
            "https://live.douyin.com/703940802949",
            "超限页面",
            false,
            &too_many,
        )),
        Err(RecorderError::InvalidBrowserSnapshot)
    ));

    let oversized = format!(
        "self.__pace_f.push([1,\"{}\"]);",
        "x".repeat(MAX_BROWSER_SNAPSHOT_TOTAL_BYTES)
    );
    assert!(matches!(
        BrowserPageSnapshot::from_json(&snapshot_json(
            "https://live.douyin.com/703940802949",
            "超限页面",
            false,
            &[oversized],
        )),
        Err(RecorderError::InvalidBrowserSnapshot)
    ));
}

#[test]
fn access_diagnostic_serializes_only_safe_whitelisted_fields() {
    let entry = AccessDiagnosticEntry {
        timestamp: Utc::now(),
        streamer_id: Some(6),
        web_rid: Some("703940802949".to_owned()),
        request_id: "room-6-42".to_owned(),
        channel: AccessChannel::Browser,
        stage: AccessStage::Probe,
        classification: AccessClassification::VerificationRequired,
        http_status: None,
        content_type: Some("text/html".to_owned()),
        response_bytes: 6297,
        markers: AccessMarkers {
            access_restricted: true,
            pace_payload: false,
            supported_room: false,
        },
        duration_ms: 15_000,
        failure_count: 1,
        next_action: AccessNextAction::WaitForUser,
        next_retry_at: None,
    };

    let serialized = serde_json::to_string(&entry).expect("serialize diagnostic");
    assert!(serialized.contains("verification_required"));
    assert!(serialized.contains("wait_for_user"));
    assert!(!serialized.contains("cookie"));
    assert!(!serialized.contains("streamUrl"));
    assert!(!serialized.contains("pageBody"));
}
