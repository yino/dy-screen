use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use dy_screen::access::{AccessChannel, AccessClassification, AccessNextAction, AccessStage};
use dy_screen::browser_snapshot::BrowserPageSnapshot;
use dy_screen::error::RecorderError;
use dy_screen::model::{Protocol, RoomStreams, StreamVariant};
use dy_screen::resolver::{
    RoomDiagnostic, RoomDiagnosticClassification, RoomDiagnosticMarkers, RoomInspection,
    RoomInspectionAttempt,
};
use dy_screen_app_lib::domain::BrowserAccessStatus;
use dy_screen_app_lib::room_resolution::{
    BrowserPageDriver, NativeRoomResolver, NoopRoomResolutionPublisher, RoomDiscovery,
    RoomResolutionContext, RoomResolutionPolicy, RoomResolutionPublisher, RoomResolutionService,
};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

const LIVE_PAGE: &str = include_str!("../../tests/fixtures/live_room.html");

fn pace_script(page: &str) -> String {
    let start = page.find("self.__pace_f.push").unwrap();
    let end = start + page[start..].find("</script>").unwrap();
    page[start..end].trim().to_owned()
}

fn browser_snapshot(access_restricted: bool) -> BrowserPageSnapshot {
    browser_snapshot_for("292895634635", access_restricted)
}

fn browser_snapshot_for(web_rid: &str, access_restricted: bool) -> BrowserPageSnapshot {
    let scripts = if access_restricted {
        Vec::new()
    } else {
        vec![pace_script(LIVE_PAGE).replace("292895634635", web_rid)]
    };
    BrowserPageSnapshot::from_json(
        &serde_json::json!({
            "url": format!("https://live.douyin.com/{web_rid}"),
            "title": if access_restricted { "验证码中间页" } else { "直播间" },
            "readyState": "complete",
            "markers": {
                "accessRestricted": access_restricted,
                "pacePayload": !scripts.is_empty()
            },
            "scripts": scripts
        })
        .to_string(),
    )
    .unwrap()
}

enum NativeReply {
    Live,
    AccessRestricted,
    Retryable,
}

struct FakeNativeResolver {
    replies: Mutex<VecDeque<NativeReply>>,
    calls: AtomicUsize,
}

struct ConcurrentNativeResolver {
    first_restricted_started: Notify,
    release_restricted: Notify,
    calls: AtomicUsize,
}

impl ConcurrentNativeResolver {
    fn new() -> Self {
        Self {
            first_restricted_started: Notify::new(),
            release_restricted: Notify::new(),
            calls: AtomicUsize::new(0),
        }
    }
}

#[async_trait]
impl NativeRoomResolver for ConcurrentNativeResolver {
    async fn inspect_native(&self, room_url: &str) -> RoomInspectionAttempt {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let access_restricted = room_url.ends_with("/292895634635");
        if access_restricted {
            self.first_restricted_started.notify_one();
            self.release_restricted.notified().await;
        } else {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        RoomInspectionAttempt {
            inspection: if access_restricted {
                Err(RecorderError::RoomAccessRestricted)
            } else {
                Ok(RoomInspection::Live(live_room()))
            },
            diagnostic: RoomDiagnostic {
                room_url: room_url.to_owned(),
                http_status: Some(200),
                content_type: Some("text/html".to_owned()),
                response_bytes: 100,
                markers: RoomDiagnosticMarkers {
                    access_restricted,
                    pace_payload: !access_restricted,
                    supported_room: !access_restricted,
                },
                classification: if access_restricted {
                    RoomDiagnosticClassification::AccessRestricted
                } else {
                    RoomDiagnosticClassification::Live
                },
                error: None,
            },
        }
    }
}

impl FakeNativeResolver {
    fn new(replies: impl IntoIterator<Item = NativeReply>) -> Self {
        Self {
            replies: Mutex::new(replies.into_iter().collect()),
            calls: AtomicUsize::new(0),
        }
    }
}

#[async_trait]
impl NativeRoomResolver for FakeNativeResolver {
    async fn inspect_native(&self, room_url: &str) -> RoomInspectionAttempt {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let reply = self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(NativeReply::Live);
        let (inspection, classification, access_restricted) = match reply {
            NativeReply::Live => (
                Ok(RoomInspection::Live(live_room())),
                RoomDiagnosticClassification::Live,
                false,
            ),
            NativeReply::AccessRestricted => (
                Err(RecorderError::RoomAccessRestricted),
                RoomDiagnosticClassification::AccessRestricted,
                true,
            ),
            NativeReply::Retryable => (
                Err(RecorderError::RoomHttpStatus { status: 503 }),
                RoomDiagnosticClassification::RetryableError,
                false,
            ),
        };
        RoomInspectionAttempt {
            inspection,
            diagnostic: RoomDiagnostic {
                room_url: room_url.to_owned(),
                http_status: Some(200),
                content_type: Some("text/html".to_owned()),
                response_bytes: 6297,
                markers: RoomDiagnosticMarkers {
                    access_restricted,
                    pace_payload: !access_restricted,
                    supported_room: !access_restricted,
                },
                classification,
                error: None,
            },
        }
    }
}

struct FakeBrowserDriver {
    snapshots: Mutex<VecDeque<BrowserPageSnapshot>>,
    navigations: Mutex<Vec<(u64, String)>>,
    snapshot_delay: Duration,
    snapshot_failures: AtomicUsize,
    snapshot_calls: AtomicUsize,
    active_snapshots: AtomicUsize,
    max_active_snapshots: AtomicUsize,
    clear_count: AtomicUsize,
}

impl FakeBrowserDriver {
    fn new(snapshot: BrowserPageSnapshot) -> Self {
        Self {
            snapshots: Mutex::new([snapshot].into_iter().collect()),
            navigations: Mutex::new(Vec::new()),
            snapshot_delay: Duration::ZERO,
            snapshot_failures: AtomicUsize::new(0),
            snapshot_calls: AtomicUsize::new(0),
            active_snapshots: AtomicUsize::new(0),
            max_active_snapshots: AtomicUsize::new(0),
            clear_count: AtomicUsize::new(0),
        }
    }

    fn delayed(snapshot: BrowserPageSnapshot, delay: Duration) -> Self {
        Self {
            snapshot_delay: delay,
            ..Self::new(snapshot)
        }
    }

    fn sequenced(snapshots: impl IntoIterator<Item = BrowserPageSnapshot>) -> Self {
        let snapshots = snapshots.into_iter().collect::<VecDeque<_>>();
        assert!(!snapshots.is_empty());
        Self {
            snapshots: Mutex::new(snapshots),
            navigations: Mutex::new(Vec::new()),
            snapshot_delay: Duration::ZERO,
            snapshot_failures: AtomicUsize::new(0),
            snapshot_calls: AtomicUsize::new(0),
            active_snapshots: AtomicUsize::new(0),
            max_active_snapshots: AtomicUsize::new(0),
            clear_count: AtomicUsize::new(0),
        }
    }

    fn failing_once(snapshot: BrowserPageSnapshot) -> Self {
        let driver = Self::new(snapshot);
        driver.snapshot_failures.store(1, Ordering::SeqCst);
        driver
    }

    fn replace_snapshot(&self, snapshot: BrowserPageSnapshot) {
        *self.snapshots.lock().unwrap() = [snapshot].into_iter().collect();
    }
}

#[async_trait]
impl BrowserPageDriver for FakeBrowserDriver {
    async fn navigate(&self, request_generation: u64, room_url: &str) -> dy_screen::Result<()> {
        self.navigations
            .lock()
            .unwrap()
            .push((request_generation, room_url.to_owned()));
        Ok(())
    }

    async fn snapshot(&self, _request_generation: u64) -> dy_screen::Result<BrowserPageSnapshot> {
        self.snapshot_calls.fetch_add(1, Ordering::SeqCst);
        let active = self.active_snapshots.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_active_snapshots
            .fetch_max(active, Ordering::SeqCst);
        if !self.snapshot_delay.is_zero() {
            tokio::time::sleep(self.snapshot_delay).await;
        }
        self.active_snapshots.fetch_sub(1, Ordering::SeqCst);
        if self
            .snapshot_failures
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                remaining.checked_sub(1)
            })
            .is_ok()
        {
            return Err(RecorderError::BrowserSessionUnavailable);
        }
        let mut snapshots = self.snapshots.lock().unwrap();
        let snapshot = if snapshots.len() > 1 {
            snapshots.pop_front().unwrap()
        } else {
            snapshots.front().unwrap().clone()
        };
        Ok(snapshot)
    }

    async fn show_verification(&self) -> dy_screen::Result<()> {
        Ok(())
    }

    async fn hide_verification(&self) -> dy_screen::Result<()> {
        Ok(())
    }

    async fn clear_session(&self) -> dy_screen::Result<()> {
        self.clear_count.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn shutdown(&self) -> dy_screen::Result<()> {
        Ok(())
    }
}

#[derive(Default)]
struct CapturingPublisher {
    states: Mutex<Vec<dy_screen_app_lib::domain::BrowserAccessState>>,
    diagnostics: Mutex<Vec<dy_screen::access::AccessDiagnosticEntry>>,
}

impl RoomResolutionPublisher for CapturingPublisher {
    fn publish_access_state(&self, state: &dy_screen_app_lib::domain::BrowserAccessState) {
        self.states.lock().unwrap().push(state.clone());
    }

    fn publish_diagnostic(&self, entry: &dy_screen::access::AccessDiagnosticEntry) {
        self.diagnostics.lock().unwrap().push(entry.clone());
    }
}

fn live_room() -> RoomStreams {
    RoomStreams {
        room_id: "292895634635".to_owned(),
        status: Some(2),
        default_quality: Some("HD1".to_owned()),
        variants: vec![StreamVariant {
            quality: "HD1".to_owned(),
            protocol: Protocol::Flv,
            url: "https://pull.example/live.flv?auth_key=secret".to_owned(),
        }],
    }
}

fn short_policy() -> RoomResolutionPolicy {
    RoomResolutionPolicy {
        browser_sticky_for: Duration::from_millis(30),
        browser_probe_timeout: Duration::from_millis(35),
        browser_probe_interval: Duration::from_millis(5),
        verification_poll_interval: Duration::from_millis(5),
    }
}

fn context() -> RoomResolutionContext {
    RoomResolutionContext {
        streamer_id: Some(6),
        web_rid: Some("292895634635".to_owned()),
        failure_count: 0,
        cancellation: CancellationToken::new(),
    }
}

#[tokio::test]
async fn native_success_does_not_touch_browser() {
    let native = Arc::new(FakeNativeResolver::new([NativeReply::Live]));
    let browser = Arc::new(FakeBrowserDriver::new(browser_snapshot(false)));
    let service = RoomResolutionService::with_policy(
        native.clone(),
        browser.clone(),
        Arc::new(NoopRoomResolutionPublisher),
        short_policy(),
    );

    let result = service
        .inspect_with_context("https://live.douyin.com/292895634635", context())
        .await
        .expect("native result");

    assert!(matches!(result, RoomInspection::Live(_)));
    assert_eq!(native.calls.load(Ordering::SeqCst), 1);
    assert!(browser.navigations.lock().unwrap().is_empty());
    assert_eq!(service.access_state().status, BrowserAccessStatus::Native);
}

#[tokio::test]
async fn access_restriction_falls_back_and_stays_browser_sticky_until_one_native_probe() {
    let native = Arc::new(FakeNativeResolver::new([
        NativeReply::AccessRestricted,
        NativeReply::Live,
    ]));
    let browser = Arc::new(FakeBrowserDriver::new(browser_snapshot(false)));
    let publisher = Arc::new(CapturingPublisher::default());
    let service = RoomResolutionService::with_policy(
        native.clone(),
        browser.clone(),
        publisher.clone(),
        short_policy(),
    );

    service
        .inspect_with_context("https://live.douyin.com/292895634635", context())
        .await
        .expect("browser fallback");
    service
        .inspect_with_context("https://live.douyin.com/292895634635", context())
        .await
        .expect("sticky browser result");
    assert_eq!(native.calls.load(Ordering::SeqCst), 1);
    assert_eq!(browser.navigations.lock().unwrap().len(), 2);

    tokio::time::sleep(Duration::from_millis(35)).await;
    service
        .inspect_with_context("https://live.douyin.com/292895634635", context())
        .await
        .expect("recovered native probe");

    assert_eq!(native.calls.load(Ordering::SeqCst), 2);
    assert_eq!(browser.navigations.lock().unwrap().len(), 2);
    assert_eq!(service.access_state().status, BrowserAccessStatus::Native);
    let diagnostics = publisher.diagnostics.lock().unwrap();
    assert!(
        diagnostics.iter().any(
            |entry| entry.next_action == dy_screen::access::AccessNextAction::FallbackToBrowser
        )
    );
}

#[tokio::test]
async fn fallback_logs_request_and_completion_for_each_real_access_channel() {
    let native = Arc::new(FakeNativeResolver::new([NativeReply::AccessRestricted]));
    let browser = Arc::new(FakeBrowserDriver::new(browser_snapshot(false)));
    let publisher = Arc::new(CapturingPublisher::default());
    let service =
        RoomResolutionService::with_policy(native, browser, publisher.clone(), short_policy());

    service
        .inspect_with_context("https://live.douyin.com/292895634635", context())
        .await
        .expect("browser fallback");

    let diagnostics = publisher.diagnostics.lock().unwrap();
    let access_steps = diagnostics
        .iter()
        .map(|entry| {
            (
                entry.channel,
                entry.stage,
                entry.classification,
                entry.next_action,
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        access_steps,
        vec![
            (
                AccessChannel::Native,
                AccessStage::Request,
                AccessClassification::Pending,
                AccessNextAction::None,
            ),
            (
                AccessChannel::Native,
                AccessStage::Response,
                AccessClassification::AccessRestricted,
                AccessNextAction::FallbackToBrowser,
            ),
            (
                AccessChannel::Native,
                AccessStage::Fallback,
                AccessClassification::AccessRestricted,
                AccessNextAction::FallbackToBrowser,
            ),
            (
                AccessChannel::Browser,
                AccessStage::Request,
                AccessClassification::Pending,
                AccessNextAction::None,
            ),
            (
                AccessChannel::Browser,
                AccessStage::Navigation,
                AccessClassification::Pending,
                AccessNextAction::None,
            ),
            (
                AccessChannel::Browser,
                AccessStage::Probe,
                AccessClassification::Live,
                AccessNextAction::StartRecording,
            ),
        ]
    );
}

#[tokio::test]
async fn cancelled_in_flight_native_access_keeps_request_diagnostic_without_fake_response() {
    let native = Arc::new(ConcurrentNativeResolver::new());
    let browser = Arc::new(FakeBrowserDriver::new(browser_snapshot(false)));
    let publisher = Arc::new(CapturingPublisher::default());
    let service = Arc::new(RoomResolutionService::with_policy(
        native.clone(),
        browser,
        publisher.clone(),
        short_policy(),
    ));
    let cancellation = CancellationToken::new();
    let request_started = native.first_restricted_started.notified();
    let task = {
        let service = service.clone();
        let cancellation = cancellation.clone();
        tokio::spawn(async move {
            service
                .inspect_with_context(
                    "https://live.douyin.com/292895634635",
                    RoomResolutionContext {
                        cancellation,
                        ..context()
                    },
                )
                .await
        })
    };

    request_started.await;
    cancellation.cancel();
    assert!(matches!(task.await.unwrap(), Err(RecorderError::Cancelled)));

    let diagnostics = publisher.diagnostics.lock().unwrap();
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].channel, AccessChannel::Native);
    assert_eq!(diagnostics[0].stage, AccessStage::Request);
    assert_eq!(diagnostics[0].classification, AccessClassification::Pending);
    assert_eq!(diagnostics[0].next_action, AccessNextAction::None);
}

#[tokio::test]
async fn stale_concurrent_native_success_cannot_clear_browser_sticky_mode() {
    let native = Arc::new(ConcurrentNativeResolver::new());
    let browser = Arc::new(FakeBrowserDriver::new(browser_snapshot(false)));
    let service = Arc::new(RoomResolutionService::with_policy(
        native.clone(),
        browser.clone(),
        Arc::new(NoopRoomResolutionPublisher),
        RoomResolutionPolicy {
            browser_sticky_for: Duration::from_secs(60),
            ..short_policy()
        },
    ));

    let first_restricted_started = native.first_restricted_started.notified();
    let restricted = {
        let service = service.clone();
        tokio::spawn(async move {
            service
                .inspect_with_context("https://live.douyin.com/292895634635", context())
                .await
        })
    };
    first_restricted_started.await;
    let stale_success = {
        let service = service.clone();
        tokio::spawn(async move {
            let other_context = RoomResolutionContext {
                streamer_id: Some(7),
                web_rid: Some("168376497175".to_owned()),
                ..context()
            };
            service
                .inspect_with_context("https://live.douyin.com/168376497175", other_context)
                .await
        })
    };
    tokio::task::yield_now().await;
    native.release_restricted.notify_one();

    assert!(matches!(
        restricted.await.unwrap(),
        Ok(RoomInspection::Live(_))
    ));
    assert!(matches!(
        stale_success.await.unwrap(),
        Ok(RoomInspection::Live(_))
    ));
    assert_eq!(native.calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        service.access_state().status,
        BrowserAccessStatus::SessionReady
    );

    service
        .inspect_with_context("https://live.douyin.com/292895634635", context())
        .await
        .expect("sticky browser result after stale native success");
    assert_eq!(native.calls.load(Ordering::SeqCst), 2);
    assert_eq!(browser.navigations.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn browser_waits_for_the_requested_document_before_parsing_room_data() {
    let native = Arc::new(FakeNativeResolver::new([NativeReply::AccessRestricted]));
    let browser = Arc::new(FakeBrowserDriver::sequenced([
        browser_snapshot_for("168376497175", false),
        browser_snapshot(false),
    ]));
    let publisher = Arc::new(CapturingPublisher::default());
    let service = RoomResolutionService::with_policy(
        native,
        browser.clone(),
        publisher.clone(),
        short_policy(),
    );

    let result = service
        .inspect_with_context("https://live.douyin.com/292895634635", context())
        .await
        .expect("requested document result");

    assert!(matches!(result, RoomInspection::Live(_)));
    assert_eq!(browser.snapshot_calls.load(Ordering::SeqCst), 2);
    let diagnostics = publisher.diagnostics.lock().unwrap();
    assert!(diagnostics.iter().any(|entry| {
        entry.channel == dy_screen::access::AccessChannel::Browser
            && entry.stage == dy_screen::access::AccessStage::Probe
            && entry.classification == dy_screen::access::AccessClassification::Pending
            && !entry.markers.supported_room
    }));
}

#[tokio::test]
async fn browser_retries_a_snapshot_failure_while_the_webview_is_starting() {
    let native = Arc::new(FakeNativeResolver::new([NativeReply::AccessRestricted]));
    let browser = Arc::new(FakeBrowserDriver::failing_once(browser_snapshot(false)));
    let service = RoomResolutionService::with_policy(
        native,
        browser.clone(),
        Arc::new(NoopRoomResolutionPublisher),
        short_policy(),
    );

    let result = service
        .inspect_with_context("https://live.douyin.com/292895634635", context())
        .await
        .expect("browser recovers after startup snapshot failure");

    assert!(matches!(result, RoomInspection::Live(_)));
    assert_eq!(browser.snapshot_calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn browser_verification_timeout_is_recoverable_after_user_action() {
    let native = Arc::new(FakeNativeResolver::new([
        NativeReply::AccessRestricted,
        NativeReply::AccessRestricted,
    ]));
    let browser = Arc::new(FakeBrowserDriver::new(browser_snapshot(true)));
    let service = RoomResolutionService::with_policy(
        native,
        browser.clone(),
        Arc::new(NoopRoomResolutionPublisher),
        short_policy(),
    );

    let error = service
        .inspect_with_context("https://live.douyin.com/292895634635", context())
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        RecorderError::RoomAccessVerificationRequired
    ));
    assert_eq!(
        service.access_state().status,
        BrowserAccessStatus::VerificationRequired
    );

    browser.replace_snapshot(browser_snapshot(false));
    service.recheck().await.expect("browser session recovered");
    assert_eq!(
        service.access_state().status,
        BrowserAccessStatus::SessionReady
    );
    assert_eq!(browser.navigations.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn manual_recheck_uses_recovered_native_channel_before_reloading_verification_page() {
    let native = Arc::new(FakeNativeResolver::new([
        NativeReply::AccessRestricted,
        NativeReply::Live,
    ]));
    let browser = Arc::new(FakeBrowserDriver::new(browser_snapshot_for(
        "168376497175",
        true,
    )));
    let service = RoomResolutionService::with_policy(
        native.clone(),
        browser.clone(),
        Arc::new(NoopRoomResolutionPublisher),
        short_policy(),
    );

    assert!(matches!(
        service
            .inspect_with_context(
                "https://live.douyin.com/168376497175",
                RoomResolutionContext {
                    streamer_id: Some(8),
                    web_rid: Some("168376497175".to_owned()),
                    ..context()
                }
            )
            .await,
        Err(RecorderError::RoomAccessVerificationRequired)
    ));

    let access = service
        .recheck()
        .await
        .expect("native recovery clears the stale verification target");

    assert_eq!(access.status, BrowserAccessStatus::Native);
    assert_eq!(access.active_streamer_id, None);
    assert_eq!(native.calls.load(Ordering::SeqCst), 2);
    assert_eq!(browser.navigations.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn verification_keeps_the_target_but_allows_native_success_for_another_room() {
    let native = Arc::new(FakeNativeResolver::new([
        NativeReply::AccessRestricted,
        NativeReply::Live,
    ]));
    let browser = Arc::new(FakeBrowserDriver::new(browser_snapshot(true)));
    let service = RoomResolutionService::with_policy(
        native.clone(),
        browser.clone(),
        Arc::new(NoopRoomResolutionPublisher),
        short_policy(),
    );

    assert!(matches!(
        service
            .inspect_with_context("https://live.douyin.com/292895634635", context())
            .await,
        Err(RecorderError::RoomAccessVerificationRequired)
    ));
    let other_context = RoomResolutionContext {
        streamer_id: Some(7),
        web_rid: Some("168376497175".to_owned()),
        ..context()
    };
    let result = service
        .inspect_with_context("https://live.douyin.com/168376497175", other_context)
        .await
        .expect("another room can still use native resolution");
    assert!(matches!(result, RoomInspection::Live(_)));

    let navigations = browser.navigations.lock().unwrap();
    assert_eq!(navigations.len(), 1);
    assert_eq!(navigations[0].1, "https://live.douyin.com/292895634635");
    drop(navigations);
    assert_eq!(native.calls.load(Ordering::SeqCst), 2);
    let access = service.access_state();
    assert_eq!(access.status, BrowserAccessStatus::VerificationRequired);
    assert_eq!(access.active_streamer_id, Some(6));
    assert_eq!(access.current_web_rid.as_deref(), Some("292895634635"));
}

#[tokio::test]
async fn verification_does_not_navigate_away_when_another_room_is_also_restricted() {
    let native = Arc::new(FakeNativeResolver::new([
        NativeReply::AccessRestricted,
        NativeReply::AccessRestricted,
    ]));
    let browser = Arc::new(FakeBrowserDriver::new(browser_snapshot(true)));
    let service = RoomResolutionService::with_policy(
        native.clone(),
        browser.clone(),
        Arc::new(NoopRoomResolutionPublisher),
        short_policy(),
    );

    assert!(matches!(
        service
            .inspect_with_context("https://live.douyin.com/292895634635", context())
            .await,
        Err(RecorderError::RoomAccessVerificationRequired)
    ));
    let other_context = RoomResolutionContext {
        streamer_id: Some(7),
        web_rid: Some("168376497175".to_owned()),
        ..context()
    };
    assert!(matches!(
        service
            .inspect_with_context("https://live.douyin.com/168376497175", other_context)
            .await,
        Err(RecorderError::RoomAccessVerificationRequired)
    ));

    assert_eq!(native.calls.load(Ordering::SeqCst), 2);
    let navigations = browser.navigations.lock().unwrap();
    assert_eq!(navigations.len(), 1);
    assert_eq!(navigations[0].1, "https://live.douyin.com/292895634635");
    drop(navigations);
    let access = service.access_state();
    assert_eq!(access.status, BrowserAccessStatus::VerificationRequired);
    assert_eq!(access.active_streamer_id, Some(6));
    assert_eq!(access.current_web_rid.as_deref(), Some("292895634635"));
}

#[tokio::test]
async fn verification_watcher_deduplicates_unchanged_probe_diagnostics() {
    let native = Arc::new(FakeNativeResolver::new([NativeReply::AccessRestricted]));
    let browser = Arc::new(FakeBrowserDriver::new(browser_snapshot(true)));
    let publisher = Arc::new(CapturingPublisher::default());
    let service =
        RoomResolutionService::with_policy(native, browser, publisher.clone(), short_policy());

    assert!(matches!(
        service
            .inspect_with_context("https://live.douyin.com/292895634635", context())
            .await,
        Err(RecorderError::RoomAccessVerificationRequired)
    ));
    let verification_probe_count = || {
        publisher
            .diagnostics
            .lock()
            .unwrap()
            .iter()
            .filter(|entry| {
                entry.channel == AccessChannel::Browser
                    && entry.stage == AccessStage::Probe
                    && entry.classification == AccessClassification::VerificationRequired
            })
            .count()
    };
    let before_watch = verification_probe_count();
    assert!(before_watch > 0);

    service
        .show_verification()
        .await
        .expect("show verification window");
    tokio::time::sleep(Duration::from_millis(25)).await;

    assert_eq!(verification_probe_count(), before_watch);
}

#[tokio::test]
async fn verification_window_watches_current_page_and_recovers_without_reloading() {
    let native = Arc::new(FakeNativeResolver::new([NativeReply::AccessRestricted]));
    let browser = Arc::new(FakeBrowserDriver::new(browser_snapshot(true)));
    let service = RoomResolutionService::with_policy(
        native,
        browser.clone(),
        Arc::new(NoopRoomResolutionPublisher),
        short_policy(),
    );

    assert!(matches!(
        service
            .inspect_with_context("https://live.douyin.com/292895634635", context())
            .await,
        Err(RecorderError::RoomAccessVerificationRequired)
    ));
    browser.replace_snapshot(browser_snapshot(false));
    service
        .show_verification()
        .await
        .expect("show verification window");

    tokio::time::timeout(Duration::from_millis(100), async {
        loop {
            if service.access_state().status == BrowserAccessStatus::SessionReady {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .expect("automatic verification recovery");
    assert_eq!(browser.navigations.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn browser_requests_are_serialized_and_worker_cancellation_is_honored() {
    let native = Arc::new(FakeNativeResolver::new([NativeReply::AccessRestricted]));
    let browser = Arc::new(FakeBrowserDriver::delayed(
        browser_snapshot(false),
        Duration::from_millis(25),
    ));
    let service = Arc::new(RoomResolutionService::with_policy(
        native,
        browser.clone(),
        Arc::new(NoopRoomResolutionPublisher),
        short_policy(),
    ));
    service
        .inspect_with_context("https://live.douyin.com/292895634635", context())
        .await
        .expect("enable browser mode");

    let first = {
        let service = service.clone();
        tokio::spawn(async move {
            service
                .inspect_with_context("https://live.douyin.com/292895634635", context())
                .await
        })
    };
    let second = {
        let service = service.clone();
        tokio::spawn(async move {
            service
                .inspect_with_context("https://live.douyin.com/292895634635", context())
                .await
        })
    };
    first.await.unwrap().unwrap();
    second.await.unwrap().unwrap();
    assert_eq!(browser.max_active_snapshots.load(Ordering::SeqCst), 1);

    let cancelled = RoomResolutionContext {
        cancellation: CancellationToken::new(),
        ..context()
    };
    cancelled.cancellation.cancel();
    assert!(matches!(
        service
            .inspect_with_context("https://live.douyin.com/333", cancelled)
            .await,
        Err(RecorderError::Cancelled)
    ));
}

#[tokio::test]
async fn clearing_session_cancels_an_active_probe_and_marks_session_expired() {
    let native = Arc::new(FakeNativeResolver::new([NativeReply::AccessRestricted]));
    let browser = Arc::new(FakeBrowserDriver::delayed(
        browser_snapshot(false),
        Duration::from_secs(1),
    ));
    let service = Arc::new(RoomResolutionService::with_policy(
        native,
        browser.clone(),
        Arc::new(NoopRoomResolutionPublisher),
        short_policy(),
    ));
    let task = {
        let service = service.clone();
        tokio::spawn(async move {
            service
                .inspect_with_context("https://live.douyin.com/292895634635", context())
                .await
        })
    };
    tokio::time::sleep(Duration::from_millis(10)).await;
    service.clear_session().await.expect("clear browser data");

    assert!(matches!(task.await.unwrap(), Err(RecorderError::Cancelled)));
    assert_eq!(browser.clear_count.load(Ordering::SeqCst), 1);
    assert_eq!(
        service.access_state().status,
        BrowserAccessStatus::SessionExpired
    );
}

#[tokio::test]
async fn retryable_native_failure_does_not_trigger_browser_fallback() {
    let native = Arc::new(FakeNativeResolver::new([NativeReply::Retryable]));
    let browser = Arc::new(FakeBrowserDriver::new(browser_snapshot(false)));
    let service = RoomResolutionService::with_policy(
        native,
        browser.clone(),
        Arc::new(NoopRoomResolutionPublisher),
        short_policy(),
    );

    assert!(matches!(
        service
            .inspect("https://live.douyin.com/292895634635")
            .await,
        Err(RecorderError::RoomHttpStatus { status: 503 })
    ));
    assert!(browser.navigations.lock().unwrap().is_empty());
}
