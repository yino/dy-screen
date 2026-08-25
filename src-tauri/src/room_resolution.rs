use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use chrono::Utc;
use dy_screen::access::{
    AccessChannel, AccessClassification, AccessDiagnosticEntry, AccessMarkers, AccessNextAction,
    AccessStage,
};
use dy_screen::browser_snapshot::{
    BrowserPageSnapshot, BrowserReadyState, parse_browser_snapshot_for_web_rid,
};
use dy_screen::error::{RecorderError, Result};
use dy_screen::resolver::{
    RoomDiagnostic, RoomInspection, RoomInspectionAttempt, StreamResolver, validate_room_url,
};
use tokio::sync::{Mutex as AsyncMutex, Notify, broadcast};
use tokio_util::sync::CancellationToken;

use crate::domain::{BrowserAccessState, BrowserAccessStatus};

pub const DEFAULT_BROWSER_STICKY_DURATION: Duration = Duration::from_secs(30 * 60);
pub const DEFAULT_BROWSER_PROBE_TIMEOUT: Duration = Duration::from_secs(30);
pub const DEFAULT_BROWSER_PROBE_INTERVAL: Duration = Duration::from_millis(500);
pub const DEFAULT_VERIFICATION_POLL_INTERVAL: Duration = Duration::from_secs(1);
pub const DEFAULT_BROWSER_PROBE_LOG_HEARTBEAT: Duration = Duration::from_secs(30);
pub const DEFAULT_PUBLIC_PAGE_REQUEST_INTERVAL: Duration = Duration::from_secs(5);

pub struct PublicPageRequestGate {
    next_allowed: AsyncMutex<Instant>,
    minimum_interval: Duration,
}

impl PublicPageRequestGate {
    pub fn new(minimum_interval: Duration) -> Self {
        Self {
            next_allowed: AsyncMutex::new(Instant::now()),
            minimum_interval,
        }
    }

    pub async fn run<T, F>(&self, cancellation: &CancellationToken, operation: F) -> Option<T>
    where
        F: Future<Output = T>,
    {
        let mut next_allowed = tokio::select! {
            _ = cancellation.cancelled() => return None,
            guard = self.next_allowed.lock() => guard,
        };
        let wait = next_allowed.saturating_duration_since(Instant::now());
        if !wait.is_zero() {
            tokio::select! {
                _ = cancellation.cancelled() => return None,
                _ = tokio::time::sleep(wait) => {}
            }
        }
        *next_allowed = Instant::now() + self.minimum_interval;
        tokio::select! {
            _ = cancellation.cancelled() => None,
            result = operation => Some(result),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct RoomResolutionPolicy {
    pub browser_sticky_for: Duration,
    pub browser_probe_timeout: Duration,
    pub browser_probe_interval: Duration,
    pub verification_poll_interval: Duration,
}

impl Default for RoomResolutionPolicy {
    fn default() -> Self {
        Self {
            browser_sticky_for: DEFAULT_BROWSER_STICKY_DURATION,
            browser_probe_timeout: DEFAULT_BROWSER_PROBE_TIMEOUT,
            browser_probe_interval: DEFAULT_BROWSER_PROBE_INTERVAL,
            verification_poll_interval: DEFAULT_VERIFICATION_POLL_INTERVAL,
        }
    }
}

#[derive(Clone)]
pub struct RoomResolutionContext {
    pub streamer_id: Option<i64>,
    pub web_rid: Option<String>,
    pub failure_count: usize,
    pub cancellation: CancellationToken,
}

impl RoomResolutionContext {
    pub fn detached(room_url: &str) -> Self {
        Self {
            streamer_id: None,
            web_rid: safe_web_rid(None, room_url),
            failure_count: 0,
            cancellation: CancellationToken::new(),
        }
    }
}

#[async_trait]
pub trait NativeRoomResolver: Send + Sync {
    async fn inspect_native(&self, room_url: &str) -> RoomInspectionAttempt;
}

#[async_trait]
impl NativeRoomResolver for StreamResolver {
    async fn inspect_native(&self, room_url: &str) -> RoomInspectionAttempt {
        self.inspect_attempt(room_url).await
    }
}

#[async_trait]
pub trait BrowserPageDriver: Send + Sync {
    async fn navigate(&self, request_generation: u64, room_url: &str) -> Result<()>;
    async fn snapshot(&self, request_generation: u64) -> Result<BrowserPageSnapshot>;
    async fn show_verification(&self) -> Result<()>;
    async fn hide_verification(&self) -> Result<()>;
    async fn clear_session(&self) -> Result<()>;
    async fn shutdown(&self) -> Result<()>;
}

pub trait RoomResolutionPublisher: Send + Sync {
    fn publish_access_state(&self, state: &BrowserAccessState);
    fn publish_diagnostic(&self, entry: &AccessDiagnosticEntry);
    fn publish_verification_cycle_started(&self) {}
}

pub struct NoopRoomResolutionPublisher;

impl RoomResolutionPublisher for NoopRoomResolutionPublisher {
    fn publish_access_state(&self, _state: &BrowserAccessState) {}
    fn publish_diagnostic(&self, _entry: &AccessDiagnosticEntry) {}
}

#[async_trait]
pub trait RoomDiscovery: Send + Sync {
    async fn inspect(&self, room_url: &str) -> Result<RoomInspection>;

    async fn inspect_with_context(
        &self,
        room_url: &str,
        context: RoomResolutionContext,
    ) -> Result<RoomInspection> {
        if context.cancellation.is_cancelled() {
            return Err(RecorderError::Cancelled);
        }
        self.inspect(room_url).await
    }

    fn manages_public_request_gate(&self) -> bool {
        false
    }

    async fn wait_for_access_change(&self, cancellation: &CancellationToken) -> bool {
        tokio::select! {
            _ = cancellation.cancelled() => false,
            _ = tokio::time::sleep(Duration::from_secs(60)) => true,
        }
    }
}

#[async_trait]
impl RoomDiscovery for StreamResolver {
    async fn inspect(&self, room_url: &str) -> Result<RoomInspection> {
        StreamResolver::inspect(self, room_url).await
    }
}

enum ResolutionMode {
    NativePreferred,
    BrowserSticky { until: Instant },
    NativeProbeInFlight,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NativeAttemptPermit {
    Preferred,
    RecoveryProbe,
    VerificationIsolation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BrowserProbeSignature {
    request_generation: u64,
    classification: AccessClassification,
    response_bytes: usize,
    markers: AccessMarkers,
}

struct BrowserProbeLogState {
    signature: BrowserProbeSignature,
    published_at: Instant,
}

struct RuntimeState {
    mode: ResolutionMode,
    access: BrowserAccessState,
    last_room_url: Option<String>,
    verification_target: Option<VerificationTarget>,
    last_browser_probe: Option<BrowserProbeLogState>,
}

#[derive(Clone)]
struct VerificationTarget {
    room_url: String,
    request_generation: u64,
    request_id: String,
    context: RoomResolutionContext,
}

#[derive(Clone)]
pub struct RoomResolutionService {
    native: Arc<dyn NativeRoomResolver>,
    browser: Arc<dyn BrowserPageDriver>,
    publisher: Arc<dyn RoomResolutionPublisher>,
    policy: RoomResolutionPolicy,
    runtime: Arc<Mutex<RuntimeState>>,
    browser_lock: Arc<AsyncMutex<()>>,
    public_request_gate: Arc<PublicPageRequestGate>,
    access_notify: Arc<Notify>,
    request_generation: Arc<AtomicU64>,
    verification_watch_active: Arc<AtomicBool>,
    access_recovered: broadcast::Sender<u64>,
    session_cancellation: Arc<Mutex<CancellationToken>>,
    shutdown: CancellationToken,
}

impl RoomResolutionService {
    pub fn new(
        native: Arc<dyn NativeRoomResolver>,
        browser: Arc<dyn BrowserPageDriver>,
        publisher: Arc<dyn RoomResolutionPublisher>,
    ) -> Self {
        Self::with_shared_gate(
            native,
            browser,
            publisher,
            RoomResolutionPolicy::default(),
            Arc::new(PublicPageRequestGate::new(
                DEFAULT_PUBLIC_PAGE_REQUEST_INTERVAL,
            )),
        )
    }

    pub fn with_policy(
        native: Arc<dyn NativeRoomResolver>,
        browser: Arc<dyn BrowserPageDriver>,
        publisher: Arc<dyn RoomResolutionPublisher>,
        policy: RoomResolutionPolicy,
    ) -> Self {
        Self::with_shared_gate(
            native,
            browser,
            publisher,
            policy,
            Arc::new(PublicPageRequestGate::new(Duration::ZERO)),
        )
    }

    pub fn with_shared_gate(
        native: Arc<dyn NativeRoomResolver>,
        browser: Arc<dyn BrowserPageDriver>,
        publisher: Arc<dyn RoomResolutionPublisher>,
        policy: RoomResolutionPolicy,
        public_request_gate: Arc<PublicPageRequestGate>,
    ) -> Self {
        let (access_recovered, _) = broadcast::channel(8);
        Self {
            native,
            browser,
            publisher,
            policy,
            runtime: Arc::new(Mutex::new(RuntimeState {
                mode: ResolutionMode::NativePreferred,
                access: BrowserAccessState::native(),
                last_room_url: None,
                verification_target: None,
                last_browser_probe: None,
            })),
            browser_lock: Arc::new(AsyncMutex::new(())),
            public_request_gate,
            access_notify: Arc::new(Notify::new()),
            request_generation: Arc::new(AtomicU64::new(0)),
            verification_watch_active: Arc::new(AtomicBool::new(false)),
            access_recovered,
            session_cancellation: Arc::new(Mutex::new(CancellationToken::new())),
            shutdown: CancellationToken::new(),
        }
    }

    pub fn public_request_gate(&self) -> Arc<PublicPageRequestGate> {
        self.public_request_gate.clone()
    }

    pub fn subscribe_access_recovered(&self) -> broadcast::Receiver<u64> {
        self.access_recovered.subscribe()
    }

    pub fn shutdown_token(&self) -> CancellationToken {
        self.shutdown.clone()
    }

    pub fn access_state(&self) -> BrowserAccessState {
        self.runtime
            .lock()
            .map(|runtime| runtime.access.clone())
            .unwrap_or_else(|_| BrowserAccessState {
                status: BrowserAccessStatus::SessionExpired,
                pending_count: 0,
                active_streamer_id: None,
                current_web_rid: None,
                last_reason: Some("浏览器会话状态锁已损坏".to_owned()),
                updated_at: Utc::now().to_rfc3339(),
            })
    }

    pub async fn show_verification(&self) -> Result<()> {
        if self.verification_target().is_some() {
            match self.check_pending_verification().await {
                Ok(()) | Err(RecorderError::BrowserSessionUnavailable) => {}
                Err(error) => return Err(error),
            }
        }
        if self.verification_target().is_none() {
            return Ok(());
        }
        self.browser.show_verification().await?;
        self.start_verification_watch();
        Ok(())
    }

    pub async fn hide_verification(&self) -> Result<()> {
        self.browser.hide_verification().await
    }

    pub async fn recheck(&self) -> Result<BrowserAccessState> {
        if let Some(target) = self.verification_target() {
            if self.try_native_verification_recovery(&target).await? {
                return Ok(self.access_state());
            }
            self.reload_pending_verification().await?;
            return Ok(self.access_state());
        }
        let room_url = self
            .runtime
            .lock()
            .ok()
            .and_then(|runtime| runtime.last_room_url.clone())
            .ok_or(RecorderError::BrowserSessionUnavailable)?;
        let _ = self.inspect(&room_url).await;
        Ok(self.access_state())
    }

    pub async fn clear_session(&self) -> Result<()> {
        if let Ok(mut cancellation) = self.session_cancellation.lock() {
            cancellation.cancel();
            *cancellation = CancellationToken::new();
        }
        self.request_generation.fetch_add(1, Ordering::SeqCst);
        if let Ok(mut runtime) = self.runtime.lock() {
            runtime.mode = ResolutionMode::NativePreferred;
            runtime.verification_target = None;
            runtime.last_browser_probe = None;
            runtime.access.status = BrowserAccessStatus::SessionExpired;
            runtime.access.active_streamer_id = None;
            runtime.access.current_web_rid = None;
            runtime.access.last_reason = Some("抖音浏览器会话已清除".to_owned());
            runtime.access.updated_at = Utc::now().to_rfc3339();
            let state = runtime.access.clone();
            drop(runtime);
            self.publisher.publish_access_state(&state);
        }
        self.access_notify.notify_waiters();
        self.browser.clear_session().await
    }

    pub async fn shutdown(&self) -> Result<()> {
        self.shutdown.cancel();
        if let Ok(cancellation) = self.session_cancellation.lock() {
            cancellation.cancel();
        }
        self.browser.shutdown().await
    }

    fn begin_native_attempt(&self, room_url: &str) -> Option<NativeAttemptPermit> {
        let Ok(mut runtime) = self.runtime.lock() else {
            return Some(NativeAttemptPermit::Preferred);
        };
        if let Some(target) = runtime.verification_target.as_ref() {
            return if same_room(&target.room_url, room_url) {
                None
            } else {
                Some(NativeAttemptPermit::VerificationIsolation)
            };
        }
        match runtime.mode {
            ResolutionMode::NativePreferred => Some(NativeAttemptPermit::Preferred),
            ResolutionMode::BrowserSticky { until } if Instant::now() >= until => {
                runtime.mode = ResolutionMode::NativeProbeInFlight;
                Some(NativeAttemptPermit::RecoveryProbe)
            }
            ResolutionMode::BrowserSticky { .. } | ResolutionMode::NativeProbeInFlight => None,
        }
    }

    fn mark_native_ready(&self, permit: NativeAttemptPermit) {
        if let Ok(mut runtime) = self.runtime.lock() {
            let can_restore = matches!(
                (permit, &runtime.mode),
                (
                    NativeAttemptPermit::Preferred,
                    ResolutionMode::NativePreferred
                ) | (
                    NativeAttemptPermit::RecoveryProbe,
                    ResolutionMode::NativeProbeInFlight
                )
            );
            if !can_restore {
                return;
            }
            runtime.mode = ResolutionMode::NativePreferred;
            runtime.verification_target = None;
            runtime.access.status = BrowserAccessStatus::Native;
            runtime.access.last_reason = None;
            runtime.access.updated_at = Utc::now().to_rfc3339();
            let state = runtime.access.clone();
            drop(runtime);
            self.publisher.publish_access_state(&state);
        }
    }

    fn mark_browser_sticky(&self) {
        if let Ok(mut runtime) = self.runtime.lock() {
            runtime.mode = ResolutionMode::BrowserSticky {
                until: Instant::now() + self.policy.browser_sticky_for,
            };
        }
    }

    async fn try_native_verification_recovery(&self, target: &VerificationTarget) -> Result<bool> {
        let session = self.session_token();
        let generation = self.request_generation.fetch_add(1, Ordering::SeqCst) + 1;
        let request_id = format!(
            "room-{}-{generation}",
            target.context.streamer_id.unwrap_or_default()
        );
        let (started, attempt) = tokio::select! {
            _ = target.context.cancellation.cancelled() => return Err(RecorderError::Cancelled),
            _ = session.cancelled() => return Err(RecorderError::Cancelled),
            _ = self.shutdown.cancelled() => return Err(RecorderError::Cancelled),
            attempt = self.public_request_gate.run(
                &target.context.cancellation,
                async {
                    self.publish_access_started(
                        &target.context,
                        &request_id,
                        AccessChannel::Native,
                    );
                    let started = Instant::now();
                    let attempt = self.native.inspect_native(&target.room_url).await;
                    (started, attempt)
                },
            ) => attempt.ok_or(RecorderError::Cancelled)?,
        };
        let next_action = if matches!(attempt.inspection, Err(RecorderError::RoomAccessRestricted))
        {
            AccessNextAction::FallbackToBrowser
        } else {
            match &attempt.inspection {
                Ok(RoomInspection::Live(_)) => AccessNextAction::StartRecording,
                Ok(RoomInspection::Offline { .. }) => AccessNextAction::ScheduleNextCheck,
                Err(_) => AccessNextAction::Retry,
            }
        };
        self.publish_native_diagnostic(
            &target.context,
            &request_id,
            &attempt.diagnostic,
            started.elapsed(),
            next_action,
        );

        let restore_native = matches!(&attempt.inspection, Ok(RoomInspection::Live(_)));
        match attempt.inspection {
            Ok(_) => {
                self.complete_native_verification(target.request_generation, restore_native)
                    .await;
                Ok(true)
            }
            Err(RecorderError::RoomAccessRestricted) => {
                self.mark_browser_sticky();
                self.publish_diagnostic(
                    &target.context,
                    &request_id,
                    DiagnosticAttempt {
                        channel: AccessChannel::Native,
                        stage: AccessStage::Fallback,
                        classification: AccessClassification::AccessRestricted,
                        http_status: attempt.diagnostic.http_status,
                        content_type: attempt.diagnostic.content_type,
                        response_bytes: attempt.diagnostic.response_bytes,
                        markers: attempt.diagnostic.markers,
                        duration: Duration::ZERO,
                        next_action: AccessNextAction::FallbackToBrowser,
                    },
                );
                Ok(false)
            }
            Err(RecorderError::Cancelled) => Err(RecorderError::Cancelled),
            Err(_) => Ok(false),
        }
    }

    async fn complete_native_verification(&self, request_generation: u64, restore_native: bool) {
        let mut completed = false;
        if let Ok(mut runtime) = self.runtime.lock()
            && runtime
                .verification_target
                .as_ref()
                .is_some_and(|pending| pending.request_generation == request_generation)
        {
            runtime.mode = if restore_native {
                ResolutionMode::NativePreferred
            } else {
                // An offline native result can be genuine while the native channel
                // remains challenge-prone. Keep using the verified browser session
                // for the next worker wake-up instead of immediately re-triggering
                // the same native challenge page.
                ResolutionMode::BrowserSticky {
                    until: Instant::now() + self.policy.browser_sticky_for,
                }
            };
            runtime.verification_target = None;
            runtime.last_browser_probe = None;
            runtime.access.status = if restore_native {
                BrowserAccessStatus::Native
            } else {
                BrowserAccessStatus::SessionReady
            };
            runtime.access.active_streamer_id = None;
            runtime.access.current_web_rid = None;
            runtime.access.last_reason = None;
            runtime.access.updated_at = Utc::now().to_rfc3339();
            let state = runtime.access.clone();
            drop(runtime);
            self.publisher.publish_access_state(&state);
            completed = true;
        }
        if completed {
            let _ = self.browser.hide_verification().await;
            self.access_notify.notify_waiters();
        }
    }

    async fn reload_pending_verification(&self) -> Result<()> {
        let Some(target) = self.verification_target() else {
            return Ok(());
        };
        let session = self.session_token();
        let _guard = tokio::select! {
            _ = session.cancelled() => return Err(RecorderError::Cancelled),
            _ = self.shutdown.cancelled() => return Err(RecorderError::Cancelled),
            guard = self.browser_lock.lock() => guard,
        };
        let Some(current) = self.verification_target() else {
            return Ok(());
        };
        if current.request_generation != target.request_generation {
            return Ok(());
        }

        let request_generation = self.request_generation.fetch_add(1, Ordering::SeqCst) + 1;
        let request_id = format!(
            "room-{}-{request_generation}",
            target.context.streamer_id.unwrap_or_default()
        );
        if let Ok(mut runtime) = self.runtime.lock()
            && let Some(pending) = runtime.verification_target.as_mut()
            && pending.request_generation == target.request_generation
        {
            pending.request_generation = request_generation;
            pending.request_id.clone_from(&request_id);
            runtime.last_browser_probe = None;
        }

        match self
            .probe_browser(
                &target.room_url,
                &target.context,
                &session,
                request_generation,
                &request_id,
            )
            .await
        {
            Ok(_) => self.complete_verification(request_generation).await,
            Err(
                RecorderError::RoomAccessVerificationRequired
                | RecorderError::UnsupportedPageLayout,
            ) => {
                self.mark_verification_required(
                    &target.room_url,
                    request_generation,
                    &request_id,
                    &target.context,
                );
                Ok(())
            }
            Err(error) => Err(error),
        }
    }

    fn session_token(&self) -> CancellationToken {
        self.session_cancellation
            .lock()
            .map(|token| token.clone())
            .unwrap_or_else(|_| {
                let token = CancellationToken::new();
                token.cancel();
                token
            })
    }

    fn verification_target(&self) -> Option<VerificationTarget> {
        self.runtime
            .lock()
            .ok()
            .and_then(|runtime| runtime.verification_target.clone())
    }

    fn mark_verification_required(
        &self,
        room_url: &str,
        request_generation: u64,
        request_id: &str,
        context: &RoomResolutionContext,
    ) {
        if let Ok(mut runtime) = self.runtime.lock() {
            let starts_cycle = runtime.verification_target.is_none();
            runtime.verification_target = Some(VerificationTarget {
                room_url: room_url.to_owned(),
                request_generation,
                request_id: request_id.to_owned(),
                context: RoomResolutionContext {
                    streamer_id: context.streamer_id,
                    web_rid: context.web_rid.clone(),
                    failure_count: context.failure_count,
                    cancellation: CancellationToken::new(),
                },
            });
            runtime.access.status = BrowserAccessStatus::VerificationRequired;
            runtime.access.active_streamer_id = context.streamer_id;
            runtime.access.current_web_rid = safe_web_rid(context.web_rid.as_deref(), room_url);
            runtime.access.last_reason = Some("抖音页面需要手动完成访问验证".to_owned());
            runtime.access.updated_at = Utc::now().to_rfc3339();
            let state = runtime.access.clone();
            drop(runtime);
            self.publisher.publish_access_state(&state);
            if starts_cycle {
                self.publisher.publish_verification_cycle_started();
            }
        }
    }

    fn start_verification_watch(&self) {
        if self
            .verification_watch_active
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return;
        }
        let service = self.clone();
        tokio::spawn(async move {
            service.watch_verification().await;
            service
                .verification_watch_active
                .store(false, Ordering::SeqCst);
        });
    }

    async fn watch_verification(&self) {
        loop {
            if self.verification_target().is_none() || self.shutdown.is_cancelled() {
                return;
            }
            let _ = self.check_pending_verification().await;
            if self.verification_target().is_none() || self.shutdown.is_cancelled() {
                return;
            }
            tokio::select! {
                _ = self.shutdown.cancelled() => return,
                _ = tokio::time::sleep(self.policy.verification_poll_interval) => {}
            }
        }
    }

    async fn check_pending_verification(&self) -> Result<()> {
        let Some(target) = self.verification_target() else {
            return Ok(());
        };
        let session = self.session_token();
        let _guard = tokio::select! {
            _ = session.cancelled() => return Err(RecorderError::Cancelled),
            _ = self.shutdown.cancelled() => return Err(RecorderError::Cancelled),
            guard = self.browser_lock.lock() => guard,
        };
        let Some(current) = self.verification_target() else {
            return Ok(());
        };
        if current.request_generation != target.request_generation {
            return Ok(());
        }

        let started = Instant::now();
        let snapshot = tokio::select! {
            _ = session.cancelled() => return Err(RecorderError::Cancelled),
            _ = self.shutdown.cancelled() => return Err(RecorderError::Cancelled),
            result = tokio::time::timeout(
                self.policy.browser_probe_timeout,
                self.browser.snapshot(target.request_generation),
            ) => result.map_err(|_| RecorderError::BrowserSessionUnavailable)??,
        };
        let markers = snapshot.markers();
        let parsed = if !snapshot.matches_room_url(&target.room_url)
            || snapshot.ready_state() != BrowserReadyState::Complete
        {
            None
        } else {
            let expected_web_rid =
                safe_web_rid(target.context.web_rid.as_deref(), &target.room_url)
                    .ok_or(RecorderError::InvalidBrowserSnapshot)?;
            Some(parse_browser_snapshot_for_web_rid(
                &snapshot,
                &expected_web_rid,
            ))
        };
        let (classification, next_action) = match parsed.as_ref() {
            Some(Ok(RoomInspection::Live(_))) => {
                (AccessClassification::Live, AccessNextAction::StartRecording)
            }
            Some(Ok(RoomInspection::Offline { .. })) => (
                AccessClassification::Offline,
                AccessNextAction::ScheduleNextCheck,
            ),
            Some(Err(RecorderError::RoomAccessVerificationRequired)) => (
                AccessClassification::VerificationRequired,
                AccessNextAction::WaitForUser,
            ),
            Some(Err(RecorderError::UnsupportedPageLayout)) if !markers.access_restricted => {
                (AccessClassification::LayoutChanged, AccessNextAction::Retry)
            }
            Some(Err(RecorderError::UnsupportedPageLayout)) => {
                (AccessClassification::Pending, AccessNextAction::WaitForUser)
            }
            Some(Err(RecorderError::Cancelled)) => {
                (AccessClassification::Cancelled, AccessNextAction::None)
            }
            Some(Err(_)) => (
                AccessClassification::RetryableError,
                AccessNextAction::WaitForUser,
            ),
            None => (AccessClassification::Pending, AccessNextAction::WaitForUser),
        };
        self.publish_browser_probe(
            &target.context,
            &target.request_id,
            target.request_generation,
            DiagnosticAttempt {
                channel: AccessChannel::Browser,
                stage: AccessStage::Probe,
                classification,
                http_status: None,
                content_type: None,
                response_bytes: snapshot.script_bytes(),
                markers: AccessMarkers {
                    access_restricted: markers.access_restricted,
                    pace_payload: markers.pace_payload,
                    supported_room: matches!(parsed.as_ref(), Some(Ok(_))),
                },
                duration: started.elapsed(),
                next_action,
            },
        );

        match parsed {
            Some(Ok(_)) => {
                self.complete_verification(target.request_generation)
                    .await?;
            }
            Some(Err(RecorderError::UnsupportedPageLayout)) if !markers.access_restricted => {
                // A completed target document without a visible challenge cannot be
                // repaired in the verification window. Clear the stale target so
                // monitoring can classify or retry the room normally.
                self.complete_verification(target.request_generation)
                    .await?;
            }
            Some(Err(RecorderError::Cancelled)) => return Err(RecorderError::Cancelled),
            Some(Err(error))
                if !matches!(
                    &error,
                    RecorderError::RoomAccessVerificationRequired
                        | RecorderError::UnsupportedPageLayout
                ) =>
            {
                return Err(error);
            }
            Some(Err(_)) | None => {}
        }
        Ok(())
    }

    async fn complete_verification(&self, request_generation: u64) -> Result<()> {
        let mut completed = false;
        if let Ok(mut runtime) = self.runtime.lock()
            && runtime
                .verification_target
                .as_ref()
                .is_some_and(|pending| pending.request_generation == request_generation)
        {
            runtime.verification_target = None;
            runtime.last_browser_probe = None;
            runtime.access.status = BrowserAccessStatus::SessionReady;
            runtime.access.active_streamer_id = None;
            runtime.access.current_web_rid = None;
            runtime.access.last_reason = None;
            runtime.access.updated_at = Utc::now().to_rfc3339();
            let state = runtime.access.clone();
            completed = true;
            drop(runtime);
            self.publisher.publish_access_state(&state);
        }
        let _ = self.browser.hide_verification().await;
        self.access_notify.notify_waiters();
        if completed {
            let _ = self.access_recovered.send(request_generation);
        }
        Ok(())
    }

    fn update_access_state(
        &self,
        status: BrowserAccessStatus,
        context: &RoomResolutionContext,
        reason: Option<&str>,
    ) {
        if let Ok(mut runtime) = self.runtime.lock() {
            runtime.access.status = status;
            runtime.access.active_streamer_id = context.streamer_id;
            runtime.access.current_web_rid = safe_web_rid(context.web_rid.as_deref(), "");
            runtime.access.last_reason = reason.map(str::to_owned);
            runtime.access.updated_at = Utc::now().to_rfc3339();
            let state = runtime.access.clone();
            drop(runtime);
            self.publisher.publish_access_state(&state);
        }
    }

    fn begin_browser_request(&self, context: &RoomResolutionContext) -> PendingBrowserRequest {
        let mut published = None;
        if let Ok(mut runtime) = self.runtime.lock() {
            runtime.access.pending_count += 1;
            runtime.access.status = BrowserAccessStatus::BrowserResolving;
            runtime.access.active_streamer_id = context.streamer_id;
            runtime.access.current_web_rid = safe_web_rid(context.web_rid.as_deref(), "");
            runtime.access.last_reason = None;
            runtime.access.updated_at = Utc::now().to_rfc3339();
            published = Some(runtime.access.clone());
        }
        if let Some(state) = published {
            self.publisher.publish_access_state(&state);
        }
        PendingBrowserRequest {
            runtime: self.runtime.clone(),
            publisher: self.publisher.clone(),
        }
    }

    fn publish_diagnostic(
        &self,
        context: &RoomResolutionContext,
        request_id: &str,
        attempt: DiagnosticAttempt,
    ) {
        self.publisher.publish_diagnostic(&AccessDiagnosticEntry {
            timestamp: Utc::now(),
            streamer_id: context.streamer_id,
            web_rid: safe_web_rid(context.web_rid.as_deref(), ""),
            request_id: request_id.to_owned(),
            channel: attempt.channel,
            stage: attempt.stage,
            classification: attempt.classification,
            http_status: attempt.http_status,
            content_type: attempt.content_type.and_then(safe_content_type),
            response_bytes: attempt.response_bytes,
            markers: attempt.markers,
            duration_ms: u64::try_from(attempt.duration.as_millis()).unwrap_or(u64::MAX),
            failure_count: context.failure_count,
            next_action: attempt.next_action,
            next_retry_at: None,
        });
    }

    fn publish_browser_probe(
        &self,
        context: &RoomResolutionContext,
        request_id: &str,
        request_generation: u64,
        attempt: DiagnosticAttempt,
    ) {
        let signature = BrowserProbeSignature {
            request_generation,
            classification: attempt.classification,
            response_bytes: attempt.response_bytes,
            markers: attempt.markers,
        };
        let should_publish = self.runtime.lock().map_or(true, |mut runtime| {
            let changed = runtime.last_browser_probe.as_ref().is_none_or(|last| {
                last.signature != signature
                    || last.published_at.elapsed() >= DEFAULT_BROWSER_PROBE_LOG_HEARTBEAT
            });
            if changed {
                runtime.last_browser_probe = Some(BrowserProbeLogState {
                    signature,
                    published_at: Instant::now(),
                });
            }
            changed
        });
        if should_publish {
            self.publish_diagnostic(context, request_id, attempt);
        }
    }

    fn publish_access_started(
        &self,
        context: &RoomResolutionContext,
        request_id: &str,
        channel: AccessChannel,
    ) {
        self.publish_diagnostic(
            context,
            request_id,
            DiagnosticAttempt {
                channel,
                stage: AccessStage::Request,
                classification: AccessClassification::Pending,
                http_status: None,
                content_type: None,
                response_bytes: 0,
                markers: AccessMarkers::default(),
                duration: Duration::ZERO,
                next_action: AccessNextAction::None,
            },
        );
    }

    async fn inspect_browser(
        &self,
        room_url: &str,
        context: &RoomResolutionContext,
        request_generation: u64,
        request_id: &str,
    ) -> Result<RoomInspection> {
        let _pending = self.begin_browser_request(context);
        let session = self.session_token();
        let _guard = tokio::select! {
            _ = context.cancellation.cancelled() => return Err(RecorderError::Cancelled),
            _ = session.cancelled() => return Err(RecorderError::Cancelled),
            _ = self.shutdown.cancelled() => return Err(RecorderError::Cancelled),
            guard = self.browser_lock.lock() => guard,
        };
        if self.verification_target().is_some() {
            self.publish_diagnostic(
                context,
                request_id,
                DiagnosticAttempt {
                    channel: AccessChannel::Browser,
                    stage: AccessStage::Fallback,
                    classification: AccessClassification::VerificationRequired,
                    http_status: None,
                    content_type: None,
                    response_bytes: 0,
                    markers: AccessMarkers {
                        access_restricted: true,
                        ..AccessMarkers::default()
                    },
                    duration: Duration::ZERO,
                    next_action: AccessNextAction::WaitForUser,
                },
            );
            return Err(RecorderError::RoomAccessVerificationRequired);
        }
        if context.cancellation.is_cancelled()
            || session.is_cancelled()
            || self.shutdown.is_cancelled()
        {
            return Err(RecorderError::Cancelled);
        }

        let result = self
            .probe_browser(room_url, context, &session, request_generation, request_id)
            .await;
        match &result {
            Ok(_) => {
                self.update_access_state(BrowserAccessStatus::SessionReady, context, None);
                self.access_notify.notify_waiters();
            }
            Err(RecorderError::RoomAccessVerificationRequired) => {
                self.mark_verification_required(room_url, request_generation, request_id, context)
            }
            Err(RecorderError::BrowserSessionUnavailable) => self.update_access_state(
                BrowserAccessStatus::SessionExpired,
                context,
                Some("抖音浏览器会话暂时不可用"),
            ),
            Err(RecorderError::Cancelled) => {}
            Err(_) => self.update_access_state(
                BrowserAccessStatus::SessionExpired,
                context,
                Some("浏览器解析未获得受支持的直播间数据"),
            ),
        }
        result
    }

    async fn probe_browser(
        &self,
        room_url: &str,
        context: &RoomResolutionContext,
        session: &CancellationToken,
        request_generation: u64,
        request_id: &str,
    ) -> Result<RoomInspection> {
        let (navigation_started, navigation) = tokio::select! {
            _ = context.cancellation.cancelled() => return Err(RecorderError::Cancelled),
            _ = session.cancelled() => return Err(RecorderError::Cancelled),
            _ = self.shutdown.cancelled() => return Err(RecorderError::Cancelled),
            result = self.public_request_gate.run(
                &context.cancellation,
                async {
                    self.publish_access_started(context, request_id, AccessChannel::Browser);
                    let started = Instant::now();
                    let result = self.browser.navigate(request_generation, room_url).await;
                    (started, result)
                },
            ) => result.ok_or(RecorderError::Cancelled)?,
        };
        self.publish_diagnostic(
            context,
            request_id,
            DiagnosticAttempt {
                channel: AccessChannel::Browser,
                stage: AccessStage::Navigation,
                classification: if navigation.is_ok() {
                    AccessClassification::Pending
                } else {
                    AccessClassification::RetryableError
                },
                http_status: None,
                content_type: None,
                response_bytes: 0,
                markers: AccessMarkers::default(),
                duration: navigation_started.elapsed(),
                next_action: if navigation.is_ok() {
                    AccessNextAction::None
                } else {
                    AccessNextAction::Retry
                },
            },
        );
        navigation?;

        let deadline = Instant::now() + self.policy.browser_probe_timeout;
        let mut saw_access_restriction = false;
        let mut saw_target_document = false;
        let mut saw_snapshot_failure = false;
        let expected_web_rid =
            safe_web_rid(None, room_url).ok_or(RecorderError::InvalidBrowserSnapshot)?;
        loop {
            let now = Instant::now();
            if now >= deadline {
                return if saw_access_restriction {
                    Err(RecorderError::RoomAccessVerificationRequired)
                } else if !saw_target_document && saw_snapshot_failure {
                    Err(RecorderError::BrowserSessionUnavailable)
                } else {
                    Err(RecorderError::UnsupportedPageLayout)
                };
            }
            let probe_started = Instant::now();
            let remaining = deadline.saturating_duration_since(now);
            let snapshot_result = tokio::select! {
                _ = context.cancellation.cancelled() => return Err(RecorderError::Cancelled),
                _ = session.cancelled() => return Err(RecorderError::Cancelled),
                _ = self.shutdown.cancelled() => return Err(RecorderError::Cancelled),
                result = tokio::time::timeout(remaining, self.browser.snapshot(request_generation)) => result,
            };
            let snapshot = match snapshot_result {
                Ok(Ok(snapshot)) => snapshot,
                Ok(Err(RecorderError::Cancelled)) => return Err(RecorderError::Cancelled),
                Ok(Err(RecorderError::BrowserRequestSuperseded)) => {
                    return Err(RecorderError::BrowserRequestSuperseded);
                }
                Ok(Err(_)) | Err(_) => {
                    saw_snapshot_failure = true;
                    self.publish_browser_probe(
                        context,
                        request_id,
                        request_generation,
                        DiagnosticAttempt {
                            channel: AccessChannel::Browser,
                            stage: AccessStage::Probe,
                            classification: AccessClassification::RetryableError,
                            http_status: None,
                            content_type: None,
                            response_bytes: 0,
                            markers: AccessMarkers::default(),
                            duration: probe_started.elapsed(),
                            next_action: AccessNextAction::Retry,
                        },
                    );
                    self.wait_for_next_browser_probe(context, session, deadline)
                        .await?;
                    continue;
                }
            };
            let markers = snapshot.markers();
            if !snapshot.matches_room_url(room_url)
                || snapshot.ready_state() != BrowserReadyState::Complete
            {
                saw_snapshot_failure = true;
                self.publish_browser_probe(
                    context,
                    request_id,
                    request_generation,
                    DiagnosticAttempt {
                        channel: AccessChannel::Browser,
                        stage: AccessStage::Probe,
                        classification: AccessClassification::Pending,
                        http_status: None,
                        content_type: None,
                        response_bytes: snapshot.script_bytes(),
                        markers: AccessMarkers {
                            access_restricted: markers.access_restricted,
                            pace_payload: markers.pace_payload,
                            supported_room: false,
                        },
                        duration: probe_started.elapsed(),
                        next_action: AccessNextAction::None,
                    },
                );
                self.wait_for_next_browser_probe(context, session, deadline)
                    .await?;
                continue;
            }
            saw_target_document = true;
            let parsed = parse_browser_snapshot_for_web_rid(&snapshot, &expected_web_rid);
            let (classification, next_action) = match &parsed {
                Ok(RoomInspection::Live(_)) => {
                    (AccessClassification::Live, AccessNextAction::StartRecording)
                }
                Ok(RoomInspection::Offline { .. }) => (
                    AccessClassification::Offline,
                    AccessNextAction::ScheduleNextCheck,
                ),
                Err(RecorderError::RoomAccessVerificationRequired) => (
                    AccessClassification::VerificationRequired,
                    AccessNextAction::WaitForUser,
                ),
                Err(RecorderError::UnsupportedPageLayout) => {
                    (AccessClassification::LayoutChanged, AccessNextAction::Retry)
                }
                Err(RecorderError::Cancelled) => {
                    (AccessClassification::Cancelled, AccessNextAction::None)
                }
                Err(_) => (
                    AccessClassification::RetryableError,
                    AccessNextAction::Retry,
                ),
            };
            self.publish_browser_probe(
                context,
                request_id,
                request_generation,
                DiagnosticAttempt {
                    channel: AccessChannel::Browser,
                    stage: AccessStage::Probe,
                    classification,
                    http_status: None,
                    content_type: None,
                    response_bytes: snapshot.script_bytes(),
                    markers: AccessMarkers {
                        access_restricted: markers.access_restricted,
                        pace_payload: markers.pace_payload,
                        supported_room: parsed.is_ok(),
                    },
                    duration: probe_started.elapsed(),
                    next_action,
                },
            );
            match parsed {
                Ok(inspection) => return Ok(inspection),
                Err(RecorderError::RoomAccessVerificationRequired) => {
                    saw_access_restriction = true;
                }
                Err(RecorderError::UnsupportedPageLayout) => {}
                Err(error) => return Err(error),
            }

            self.wait_for_next_browser_probe(context, session, deadline)
                .await?;
        }
    }

    async fn wait_for_next_browser_probe(
        &self,
        context: &RoomResolutionContext,
        session: &CancellationToken,
        deadline: Instant,
    ) -> Result<()> {
        let delay = self
            .policy
            .browser_probe_interval
            .min(deadline.saturating_duration_since(Instant::now()));
        tokio::select! {
            _ = context.cancellation.cancelled() => Err(RecorderError::Cancelled),
            _ = session.cancelled() => Err(RecorderError::Cancelled),
            _ = self.shutdown.cancelled() => Err(RecorderError::Cancelled),
            _ = tokio::time::sleep(delay) => Ok(()),
        }
    }
}

#[async_trait]
impl RoomDiscovery for RoomResolutionService {
    async fn inspect(&self, room_url: &str) -> Result<RoomInspection> {
        self.inspect_with_context(room_url, RoomResolutionContext::detached(room_url))
            .await
    }

    async fn inspect_with_context(
        &self,
        room_url: &str,
        mut context: RoomResolutionContext,
    ) -> Result<RoomInspection> {
        validate_room_url(room_url)?;
        context.web_rid = safe_web_rid(context.web_rid.as_deref(), room_url);
        if context.cancellation.is_cancelled() || self.shutdown.is_cancelled() {
            return Err(RecorderError::Cancelled);
        }
        if let Ok(mut runtime) = self.runtime.lock() {
            runtime.last_room_url = Some(room_url.to_owned());
        }
        let generation = self.request_generation.fetch_add(1, Ordering::SeqCst) + 1;
        let request_id = format!(
            "room-{}-{generation}",
            context.streamer_id.unwrap_or_default()
        );
        let session = self.session_token();

        if let Some(native_permit) = self.begin_native_attempt(room_url) {
            let (started, attempt) = tokio::select! {
                _ = context.cancellation.cancelled() => return Err(RecorderError::Cancelled),
                _ = session.cancelled() => return Err(RecorderError::Cancelled),
                _ = self.shutdown.cancelled() => return Err(RecorderError::Cancelled),
                attempt = self.public_request_gate.run(
                    &context.cancellation,
                    async {
                        self.publish_access_started(&context, &request_id, AccessChannel::Native);
                        let started = Instant::now();
                        let attempt = self.native.inspect_native(room_url).await;
                        (started, attempt)
                    },
                ) => attempt.ok_or(RecorderError::Cancelled)?,
            };
            let next_action =
                if matches!(attempt.inspection, Err(RecorderError::RoomAccessRestricted)) {
                    AccessNextAction::FallbackToBrowser
                } else {
                    match &attempt.inspection {
                        Ok(RoomInspection::Live(_)) => AccessNextAction::StartRecording,
                        Ok(RoomInspection::Offline { .. }) => AccessNextAction::ScheduleNextCheck,
                        Err(_) => AccessNextAction::Retry,
                    }
                };
            self.publish_native_diagnostic(
                &context,
                &request_id,
                &attempt.diagnostic,
                started.elapsed(),
                next_action,
            );
            match attempt.inspection {
                Ok(inspection) => {
                    self.mark_native_ready(native_permit);
                    return Ok(inspection);
                }
                Err(RecorderError::RoomAccessRestricted) => {
                    self.mark_browser_sticky();
                    self.publish_diagnostic(
                        &context,
                        &request_id,
                        DiagnosticAttempt {
                            channel: AccessChannel::Native,
                            stage: AccessStage::Fallback,
                            classification: AccessClassification::AccessRestricted,
                            http_status: attempt.diagnostic.http_status,
                            content_type: attempt.diagnostic.content_type,
                            response_bytes: attempt.diagnostic.response_bytes,
                            markers: attempt.diagnostic.markers,
                            duration: Duration::ZERO,
                            next_action: AccessNextAction::FallbackToBrowser,
                        },
                    );
                }
                Err(error) => {
                    if native_permit == NativeAttemptPermit::RecoveryProbe {
                        self.mark_browser_sticky();
                    }
                    return Err(error);
                }
            }
        }

        self.inspect_browser(room_url, &context, generation, &request_id)
            .await
    }

    fn manages_public_request_gate(&self) -> bool {
        true
    }

    async fn wait_for_access_change(&self, cancellation: &CancellationToken) -> bool {
        tokio::select! {
            _ = cancellation.cancelled() => false,
            _ = self.shutdown.cancelled() => false,
            _ = self.access_notify.notified() => true,
        }
    }
}

impl RoomResolutionService {
    fn publish_native_diagnostic(
        &self,
        context: &RoomResolutionContext,
        request_id: &str,
        diagnostic: &RoomDiagnostic,
        duration: Duration,
        next_action: AccessNextAction,
    ) {
        self.publish_diagnostic(
            context,
            request_id,
            DiagnosticAttempt {
                channel: AccessChannel::Native,
                stage: AccessStage::Response,
                classification: diagnostic.classification,
                http_status: diagnostic.http_status,
                content_type: diagnostic.content_type.clone(),
                response_bytes: diagnostic.response_bytes,
                markers: diagnostic.markers,
                duration,
                next_action,
            },
        );
    }
}

struct DiagnosticAttempt {
    channel: AccessChannel,
    stage: AccessStage,
    classification: AccessClassification,
    http_status: Option<u16>,
    content_type: Option<String>,
    response_bytes: usize,
    markers: AccessMarkers,
    duration: Duration,
    next_action: AccessNextAction,
}

struct PendingBrowserRequest {
    runtime: Arc<Mutex<RuntimeState>>,
    publisher: Arc<dyn RoomResolutionPublisher>,
}

impl Drop for PendingBrowserRequest {
    fn drop(&mut self) {
        let mut published = None;
        if let Ok(mut runtime) = self.runtime.lock() {
            runtime.access.pending_count = runtime.access.pending_count.saturating_sub(1);
            if let Some((streamer_id, web_rid)) =
                runtime.verification_target.as_ref().map(|target| {
                    (
                        target.context.streamer_id,
                        safe_web_rid(target.context.web_rid.as_deref(), &target.room_url),
                    )
                })
            {
                runtime.access.status = BrowserAccessStatus::VerificationRequired;
                runtime.access.active_streamer_id = streamer_id;
                runtime.access.current_web_rid = web_rid;
            } else if runtime.access.pending_count == 0 {
                runtime.access.active_streamer_id = None;
                runtime.access.current_web_rid = None;
            }
            runtime.access.updated_at = Utc::now().to_rfc3339();
            published = Some(runtime.access.clone());
        }
        if let Some(state) = published {
            self.publisher.publish_access_state(&state);
        }
    }
}

fn safe_web_rid(candidate: Option<&str>, room_url: &str) -> Option<String> {
    candidate
        .filter(|value| {
            !value.is_empty() && value.chars().all(|character| character.is_ascii_digit())
        })
        .map(str::to_owned)
        .or_else(|| {
            validate_room_url(room_url).ok().and_then(|url| {
                url.path_segments()?
                    .find(|segment| {
                        !segment.is_empty()
                            && segment.chars().all(|character| character.is_ascii_digit())
                    })
                    .map(str::to_owned)
            })
        })
}

fn same_room(left: &str, right: &str) -> bool {
    safe_web_rid(None, left)
        .zip(safe_web_rid(None, right))
        .is_some_and(|(left, right)| left == right)
}

fn safe_content_type(value: String) -> Option<String> {
    let normalized = value.trim();
    if normalized.is_empty() || normalized.len() > 128 || normalized.chars().any(char::is_control) {
        None
    } else {
        Some(normalized.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browser_probe_timeout_is_thirty_seconds() {
        assert_eq!(DEFAULT_BROWSER_PROBE_TIMEOUT, Duration::from_secs(30));
    }
}
