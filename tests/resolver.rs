use dy_screen::error::RecorderError;
use dy_screen::model::{Protocol, RoomStreams};
use dy_screen::resolver::{
    RoomDiagnosticClassification, RoomInspection, classify_room_http_status,
    diagnose_room_response, parse_room_inspection, parse_room_page, validate_room_url,
};
use reqwest::StatusCode;

const LIVE_PAGE: &str = include_str!("fixtures/live_room.html");
const OFFLINE_PAGE: &str = include_str!("fixtures/offline_room.html");
const ACCESS_RESTRICTED_PAGE: &str = include_str!("fixtures/access_restricted_room.html");
const CAPTCHA_INTERSTITIAL_PAGE: &str = include_str!("fixtures/captcha_interstitial_room.html");

#[test]
fn validates_douyin_live_urls() {
    let url = validate_room_url("https://live.douyin.com/292895634635?anchor_id=1")
        .expect("supported room URL");
    assert_eq!(url.host_str(), Some("live.douyin.com"));

    let error = validate_room_url("https://example.com/292895634635").unwrap_err();
    assert!(matches!(error, RecorderError::UnsupportedRoomUrl { .. }));
}

#[test]
fn parses_react_flight_stream_data() {
    let room = parse_room_page(LIVE_PAGE).expect("room stream data");

    assert_eq!(room.room_id, "292895634635");
    assert_eq!(room.status, Some(2));
    assert_eq!(room.default_quality.as_deref(), Some("HD1"));
    assert_eq!(room.variants.len(), 4);
    assert!(
        room.variants
            .iter()
            .any(|variant| { variant.quality == "FULL_HD1" && variant.protocol == Protocol::Flv })
    );
}

#[test]
fn reports_offline_room_without_exposing_payload_data() {
    let error = parse_room_page(OFFLINE_PAGE).unwrap_err();
    assert!(matches!(error, RecorderError::RoomUnavailable));
    assert!(!error.to_string().contains("auth_key"));
}

#[test]
fn inspects_canonical_identity_for_offline_room() {
    let inspection = parse_room_inspection(OFFLINE_PAGE).expect("offline room identity");
    assert_eq!(
        inspection,
        RoomInspection::Offline {
            room_id: "offline-room".to_owned()
        }
    );
}

#[test]
fn rejects_unsupported_payload_shape() {
    let error =
        parse_room_page(r#"<script>self.__pace_f.push([1,"not-json-after-prefix"]);</script>"#)
            .unwrap_err();
    assert!(matches!(error, RecorderError::UnsupportedPageLayout));
}

#[test]
fn classifies_decodable_but_unrecognized_layout_as_unsupported() {
    let page = r#"<script>self.__pace_f.push([1,"c:[{\"state\":{\"newRoomShape\":{\"online\":true}}}]"]);</script>"#;
    let error = parse_room_page(page).unwrap_err();
    assert!(matches!(error, RecorderError::UnsupportedPageLayout));
}

#[test]
fn distinguishes_access_restriction_from_layout_change() {
    let restricted = parse_room_inspection(ACCESS_RESTRICTED_PAGE).unwrap_err();
    assert!(matches!(restricted, RecorderError::RoomAccessRestricted));

    let unknown = r#"<html><script>window.__NEXT_DATA__={"newRoomShape":true};</script></html>"#;
    let changed = parse_room_inspection(unknown).unwrap_err();
    assert!(matches!(changed, RecorderError::UnsupportedPageLayout));
}

#[test]
fn classifies_http_200_captcha_interstitial_as_access_restricted() {
    let error = parse_room_inspection(CAPTCHA_INTERSTITIAL_PAGE).unwrap_err();
    assert!(matches!(error, RecorderError::RoomAccessRestricted));

    let diagnostic = diagnose_room_response(
        "https://live.douyin.com/559686664524",
        StatusCode::OK,
        Some("text/html"),
        CAPTCHA_INTERSTITIAL_PAGE,
    );
    assert_eq!(
        diagnostic.classification,
        RoomDiagnosticClassification::AccessRestricted
    );
    assert!(diagnostic.markers.access_restricted);
    assert!(!diagnostic.markers.pace_payload);
    assert!(!diagnostic.markers.supported_room);

    let serialized = serde_json::to_string(&diagnostic).unwrap();
    assert!(!serialized.contains("验证码中间页"));
    assert!(!serialized.contains("sec_sdk_build"));
    assert!(!serialized.contains("slide"));
}

#[test]
fn only_explicit_http_statuses_mark_the_entry_invalid() {
    assert!(matches!(
        classify_room_http_status(StatusCode::NOT_FOUND),
        Err(RecorderError::RoomHttpStatus { status: 404 })
    ));
    assert!(matches!(
        classify_room_http_status(StatusCode::GONE),
        Err(RecorderError::RoomHttpStatus { status: 410 })
    ));
    assert!(matches!(
        classify_room_http_status(StatusCode::FORBIDDEN),
        Err(RecorderError::RoomAccessRestricted)
    ));
    assert!(matches!(
        classify_room_http_status(StatusCode::INTERNAL_SERVER_ERROR),
        Err(RecorderError::RoomHttpStatus { status: 500 })
    ));
}

#[test]
fn diagnostics_only_expose_safe_metadata_and_markers() {
    let diagnostic = diagnose_room_response(
        "https://live.douyin.com/452086788686?anchor_id=secret",
        StatusCode::OK,
        Some("text/html; charset=utf-8"),
        ACCESS_RESTRICTED_PAGE,
    );

    assert_eq!(
        diagnostic.classification,
        RoomDiagnosticClassification::AccessRestricted
    );
    assert_eq!(diagnostic.room_url, "https://live.douyin.com/452086788686");
    assert_eq!(diagnostic.http_status, Some(200));
    assert_eq!(
        diagnostic.content_type.as_deref(),
        Some("text/html; charset=utf-8")
    );
    assert_eq!(diagnostic.response_bytes, ACCESS_RESTRICTED_PAGE.len());
    assert!(diagnostic.markers.access_restricted);
    assert!(!diagnostic.markers.pace_payload);

    let serialized = serde_json::to_string(&diagnostic).unwrap();
    assert!(!serialized.contains("fixture-nonce-must-not-leak"));
    assert!(!serialized.contains("fixture-signature-must-not-leak"));
    assert!(!serialized.contains("anchor_id"));
    assert!(!serialized.contains("<html"));
}

#[test]
fn diagnostics_classify_unknown_supported_response_as_layout_changed() {
    let page = "<html><body>unknown public page</body></html>";
    let diagnostic = diagnose_room_response(
        "https://live.douyin.com/452086788686",
        StatusCode::OK,
        Some("text/html"),
        page,
    );

    assert_eq!(
        diagnostic.classification,
        RoomDiagnosticClassification::LayoutChanged
    );
    assert_eq!(
        diagnostic.error.as_deref(),
        Some("抖音直播间页面结构已变化，当前版本暂时无法解析")
    );
}

#[test]
fn selects_requested_variant_and_redacts_signed_url() {
    let room = parse_room_page(LIVE_PAGE).expect("room stream data");
    let selected = room
        .select(Some("HD1"), Some(Protocol::Hls))
        .expect("selected HLS stream");

    assert_eq!(selected.quality, "HD1");
    assert_eq!(selected.protocol, Protocol::Hls);
    assert!(!selected.fell_back);
    assert_eq!(selected.redacted_url(), "https://pull.example/hd.m3u8");
    assert!(!selected.redacted_url().contains("top-secret"));
    assert!(!format!("{selected:?}").contains("top-secret"));
    assert!(!format!("{room:?}").contains("top-secret"));
}

#[test]
fn falls_back_to_default_quality_then_protocol() {
    let room = parse_room_page(LIVE_PAGE).expect("room stream data");
    let selected = room
        .select(Some("MISSING"), Some(Protocol::Flv))
        .expect("fallback stream");

    assert_eq!(selected.quality, "HD1");
    assert_eq!(selected.protocol, Protocol::Flv);
    assert!(selected.fell_back);

    let hls_only = RoomStreams {
        variants: room
            .variants
            .into_iter()
            .filter(|variant| variant.protocol == Protocol::Hls)
            .collect(),
        ..room
    };
    let protocol_fallback = hls_only
        .select(Some("HD1"), Some(Protocol::Flv))
        .expect("HLS fallback");
    assert_eq!(protocol_fallback.protocol, Protocol::Hls);
    assert!(protocol_fallback.fell_back);
}
