use dy_screen::error::RecorderError;
use dy_screen::model::{ProfileDiscoveryErrorKind, ProfileInspection, StreamerSourceKind};
use dy_screen::profile_resolver::{ProfileResolver, parse_profile_page, validate_profile_url};

const LIVE_PROFILE: &str = include_str!("fixtures/live_profile.html");
const LIVE_PROFILE_WITH_UNRELATED_RECOMMENDATION: &str =
    include_str!("fixtures/live_profile_with_unrelated_recommendation.html");
const OFFLINE_PROFILE: &str = include_str!("fixtures/offline_profile.html");
const TEXT_ONLY_PROFILE: &str = include_str!("fixtures/text_only_profile.html");
const LINK_FALLBACK_PROFILE: &str = include_str!("fixtures/link_fallback_profile.html");
const UNSUPPORTED_PROFILE: &str = include_str!("fixtures/unsupported_profile.html");
const ACCESS_RESTRICTED_PROFILE: &str = include_str!("fixtures/access_restricted_profile.html");

const PROFILE_SEC_UID: &str = "MS4wLjABAAAAdUzdkD-hjRb0rWmS8d02sHzajJrlII40rcefrLK7Oug";

#[test]
fn validates_and_normalizes_public_profile_urls() {
    let profile = validate_profile_url(&format!(
        "https://douyin.com/user/{PROFILE_SEC_UID}?from=share#works"
    ))
    .expect("应接受公开个人主页");

    assert_eq!(profile.source_kind, StreamerSourceKind::Profile);
    assert_eq!(profile.profile_sec_uid, PROFILE_SEC_UID);
    assert_eq!(
        profile.source_url,
        format!("https://www.douyin.com/user/{PROFILE_SEC_UID}")
    );
}

#[test]
fn rejects_unsupported_profile_urls_without_echoing_query_values() {
    for input in [
        "ftp://www.douyin.com/user/abc",
        "https://example.com/user/abc?token=secret",
        "https://www.douyin.com/video/123",
        "https://www.douyin.com/user/",
    ] {
        let error = validate_profile_url(input).unwrap_err();
        assert!(matches!(
            error,
            RecorderError::InvalidProfileUrl { .. } | RecorderError::UnsupportedProfileUrl
        ));
        assert!(!error.to_string().contains("secret"));
    }
}

#[test]
fn parses_live_profile_identity_and_room_binding() {
    let profile_url =
        validate_profile_url(&format!("https://www.douyin.com/user/{PROFILE_SEC_UID}")).unwrap();

    let inspection = parse_profile_page(LIVE_PROFILE, profile_url).expect("应识别正在直播主页");

    let ProfileInspection::Live { identity, room } = inspection else {
        panic!("预期正在直播结果");
    };
    assert_eq!(identity.profile_sec_uid, PROFILE_SEC_UID);
    assert_eq!(identity.display_name.as_deref(), Some("小猫充电器"));
    assert_eq!(room.web_rid, "236150550962");
    assert_eq!(room.room_id.as_deref(), Some("7664620130978581282"));
    assert_eq!(room.room_url, "https://live.douyin.com/236150550962");
}

#[test]
fn ignores_unrelated_recommended_live_room_before_the_profile_room() {
    let profile_url =
        validate_profile_url(&format!("https://www.douyin.com/user/{PROFILE_SEC_UID}")).unwrap();

    let inspection = parse_profile_page(LIVE_PROFILE_WITH_UNRELATED_RECOMMENDATION, profile_url)
        .expect("应只绑定目标主页的直播对象");

    let ProfileInspection::Live { room, .. } = inspection else {
        panic!("预期正在直播结果");
    };
    assert_eq!(room.web_rid, "236150550962");
    assert_eq!(room.room_id.as_deref(), Some("7664620130978581282"));
    assert!(
        !serde_json::to_string(&room)
            .unwrap()
            .contains("unrelated-secret")
    );
}

#[test]
fn preserves_profile_identity_when_streamer_is_offline() {
    let profile_url =
        validate_profile_url(&format!("https://www.douyin.com/user/{PROFILE_SEC_UID}")).unwrap();

    let inspection = parse_profile_page(OFFLINE_PROFILE, profile_url).expect("应识别离线主页");

    let ProfileInspection::Offline { identity } = inspection else {
        panic!("预期未开播结果");
    };
    assert_eq!(identity.profile_sec_uid, PROFILE_SEC_UID);
    assert_eq!(identity.display_name.as_deref(), Some("小猫充电器"));
}

#[test]
fn falls_back_to_a_unique_live_room_link() {
    let profile_url =
        validate_profile_url(&format!("https://www.douyin.com/user/{PROFILE_SEC_UID}")).unwrap();

    let inspection =
        parse_profile_page(LINK_FALLBACK_PROFILE, profile_url).expect("应使用唯一直播链接");

    let ProfileInspection::Live { room, .. } = inspection else {
        panic!("预期直播链接回退结果");
    };
    assert_eq!(room.web_rid, "236150550962");
    assert_eq!(room.room_id, None);
    assert_eq!(room.room_url, "https://live.douyin.com/236150550962");
}

#[test]
fn live_text_alone_never_fabricates_room_identity() {
    let profile_url =
        validate_profile_url(&format!("https://www.douyin.com/user/{PROFILE_SEC_UID}")).unwrap();

    let inspection =
        parse_profile_page(TEXT_ONLY_PROFILE, profile_url).expect("文字不应破坏主页身份");

    assert!(matches!(inspection, ProfileInspection::Offline { .. }));
}

#[test]
fn rejects_pages_without_confirmed_profile_identity() {
    let profile_url =
        validate_profile_url(&format!("https://www.douyin.com/user/{PROFILE_SEC_UID}")).unwrap();

    let error = parse_profile_page(UNSUPPORTED_PROFILE, profile_url).unwrap_err();

    assert!(matches!(error, RecorderError::UnsupportedPageLayout));
    assert!(!error.to_string().contains("page-secret"));
}

#[test]
fn classifies_public_http_challenge_as_access_restricted_without_exposing_tokens() {
    let profile_url =
        validate_profile_url(&format!("https://www.douyin.com/user/{PROFILE_SEC_UID}")).unwrap();

    let error = parse_profile_page(ACCESS_RESTRICTED_PROFILE, profile_url).unwrap_err();

    assert!(matches!(error, RecorderError::ProfileAccessRestricted));
    assert!(!error.safe_message().contains("sanitized-signature"));
    assert!(!format!("{error:?}").contains("sanitized-nonce"));
}

#[test]
fn serialized_profile_result_contains_only_normalized_identity() {
    let profile_url =
        validate_profile_url(&format!("https://www.douyin.com/user/{PROFILE_SEC_UID}")).unwrap();
    let inspection = parse_profile_page(LIVE_PROFILE, profile_url).unwrap();

    let json = serde_json::to_string(&inspection).unwrap();

    assert!(json.contains("236150550962"));
    assert!(!json.contains("auth_key"));
    assert!(!json.contains("page-secret"));
    assert!(!json.contains("from=homepage"));
}

#[test]
fn classifies_profile_errors_for_retry_policy() {
    assert_eq!(
        RecorderError::ProfileHttpStatus { status: 503 }.profile_discovery_kind(),
        Some(ProfileDiscoveryErrorKind::Retryable)
    );
    assert_eq!(
        RecorderError::ProfileAccessRestricted.profile_discovery_kind(),
        Some(ProfileDiscoveryErrorKind::AccessRestricted)
    );
    assert_eq!(
        RecorderError::UnsupportedPageLayout.profile_discovery_kind(),
        Some(ProfileDiscoveryErrorKind::UnsupportedPageLayout)
    );
}

#[tokio::test]
async fn invalid_profile_is_rejected_before_any_public_page_request() {
    let resolver = ProfileResolver::new().unwrap();

    let error = resolver
        .inspect("https://example.com/user/abc?token=request-secret")
        .await
        .unwrap_err();

    assert!(matches!(error, RecorderError::UnsupportedProfileUrl));
    assert!(!error.safe_message().contains("request-secret"));
}

#[tokio::test]
async fn profile_request_errors_do_not_format_urls_or_tokens() {
    let source = reqwest::Client::builder()
        .no_proxy()
        .build()
        .unwrap()
        .get("http://127.0.0.1:9/user/demo?token=request-secret")
        .send()
        .await
        .unwrap_err();
    let error = RecorderError::ProfilePageRequest { source };

    assert!(!error.safe_message().contains("request-secret"));
    assert!(!format!("{error:?}").contains("request-secret"));
    assert!(!format!("{error:?}").contains("127.0.0.1"));
}
