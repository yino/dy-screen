use dy_screen::error::RecorderError;
use dy_screen::model::{Protocol, RoomStreams};
use dy_screen::resolver::{parse_room_page, validate_room_url};

const LIVE_PAGE: &str = include_str!("fixtures/live_room.html");
const OFFLINE_PAGE: &str = include_str!("fixtures/offline_room.html");

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
