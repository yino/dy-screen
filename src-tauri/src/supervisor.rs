use std::collections::HashMap;
use std::future::Future;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use chrono::Utc;
use dy_screen::error::RecorderError;
use dy_screen::model::{
    EventSink, JobEvent, ProfileInspection, Protocol, RoomStreams, SelectedStream,
};
use dy_screen::profile_resolver::ProfileResolver;
use dy_screen::recorder::{FfmpegConfig, FfmpegRecorder, RecordingConfig};
use dy_screen::resolver::{RoomInspection, StreamResolver};
use fs2::available_space;
use serde::Serialize;
use tokio::sync::{Mutex as AsyncMutex, Notify, broadcast, mpsc};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::database::Database;
use crate::domain::{DiscoveryBinding, MonitorEvent, NewVideo, Streamer, StreamerSourceKind};

const PUBLIC_PAGE_REQUEST_INTERVAL: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonitorState {
    pub live_status: String,
    pub monitor_status: String,
}

impl MonitorState {
    pub fn waiting() -> Self {
        Self {
            live_status: "offline".to_owned(),
            monitor_status: "waiting".to_owned(),
        }
    }

    pub fn paused(&self) -> Self {
        Self {
            live_status: self.live_status.clone(),
            monitor_status: "paused".to_owned(),
        }
    }
}

pub fn backoff_seconds(failure_count: usize) -> u64 {
    [30, 60, 120, 300]
        .get(failure_count)
        .copied()
        .unwrap_or(300)
}

pub fn profile_backoff_seconds(failure_count: usize) -> u64 {
    [60, 120, 300].get(failure_count).copied().unwrap_or(300)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiskDecision {
    Continue,
    Warn,
    BlockNew,
    StopActive,
}

pub fn decide_disk(bytes_available: u64, recording_active: bool) -> DiskDecision {
    const GIB: u64 = 1024 * 1024 * 1024;
    if recording_active && bytes_available < GIB {
        DiskDecision::StopActive
    } else if bytes_available < 2 * GIB {
        DiskDecision::BlockNew
    } else if bytes_available < 10 * GIB {
        DiskDecision::Warn
    } else {
        DiskDecision::Continue
    }
}

pub fn recording_session_status(cancelled: bool, success: bool) -> &'static str {
    if cancelled {
        "cancelled"
    } else if success {
        "completed"
    } else {
        "error"
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ResolveFailure {
    Offline,
    Retryable(String, Option<u16>),
    AccessRestricted(String, Option<u16>),
    LayoutChanged(String, Option<u16>),
    EntryInvalid(String, Option<u16>),
}

fn classify_resolve_error(error: &RecorderError) -> ResolveFailure {
    match error {
        RecorderError::RoomUnavailable => ResolveFailure::Offline,
        RecorderError::RoomAccessRestricted => {
            ResolveFailure::AccessRestricted(error.safe_message(), None)
        }
        RecorderError::UnsupportedPageLayout => {
            ResolveFailure::LayoutChanged(error.safe_message(), None)
        }
        RecorderError::InvalidRoomUrl { .. } | RecorderError::UnsupportedRoomUrl { .. } => {
            ResolveFailure::EntryInvalid(error.safe_message(), None)
        }
        RecorderError::RoomHttpStatus { status: 404 | 410 } => {
            let status = room_error_http_status(error);
            ResolveFailure::EntryInvalid(
                format!("直播入口已失效（HTTP {}）", status.unwrap_or_default()),
                status,
            )
        }
        RecorderError::RoomHttpStatus { .. } => {
            ResolveFailure::Retryable(error.safe_message(), room_error_http_status(error))
        }
        RecorderError::PageRequest(source)
            if source
                .status()
                .is_some_and(|status| matches!(status.as_u16(), 404 | 410)) =>
        {
            ResolveFailure::EntryInvalid(
                "直播入口已失效".to_owned(),
                source.status().map(|status| status.as_u16()),
            )
        }
        _ => ResolveFailure::Retryable(error.safe_message(), room_error_http_status(error)),
    }
}

fn room_error_http_status(error: &RecorderError) -> Option<u16> {
    match error {
        RecorderError::RoomHttpStatus { status } => Some(*status),
        RecorderError::PageRequest(source) => source.status().map(|status| status.as_u16()),
        _ => None,
    }
}

fn retry_at(wait_seconds: u64) -> String {
    (Utc::now() + chrono::Duration::seconds(wait_seconds as i64)).to_rfc3339()
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RoomCheckLogEntry<'a> {
    timestamp: String,
    streamer_id: i64,
    web_rid: Option<&'a str>,
    classification: &'a str,
    http_status: Option<u16>,
    failure_count: usize,
    next_retry_at: Option<&'a str>,
}

#[derive(Clone, Default)]
pub struct MonitorLogger {
    directory: Option<Arc<PathBuf>>,
    write_lock: Arc<Mutex<()>>,
}

impl MonitorLogger {
    pub fn file(directory: PathBuf) -> Self {
        Self {
            directory: Some(Arc::new(directory)),
            write_lock: Arc::new(Mutex::new(())),
        }
    }

    pub fn log_room_check(
        &self,
        streamer_id: i64,
        web_rid: Option<&str>,
        classification: &str,
        http_status: Option<u16>,
        failure_count: usize,
        next_retry_at: Option<&str>,
    ) {
        let safe_web_rid = web_rid.filter(|value| {
            !value.is_empty() && value.chars().all(|character| character.is_ascii_digit())
        });
        let safe_classification = match classification {
            "live" | "offline" | "access_restricted" | "layout_changed" | "entry_invalid"
            | "retryable_error" => classification,
            _ => "unknown",
        };
        let safe_next_retry_at =
            next_retry_at.filter(|value| chrono::DateTime::parse_from_rfc3339(value).is_ok());
        let entry = RoomCheckLogEntry {
            timestamp: Utc::now().to_rfc3339(),
            streamer_id,
            web_rid: safe_web_rid,
            classification: safe_classification,
            http_status,
            failure_count,
            next_retry_at: safe_next_retry_at,
        };
        let Ok(line) = serde_json::to_string(&entry) else {
            return;
        };
        #[cfg(debug_assertions)]
        eprintln!("{line}");

        let Some(directory) = self.directory.as_deref() else {
            return;
        };
        let Ok(_guard) = self.write_lock.lock() else {
            return;
        };
        if let Err(_error) = append_monitor_log(directory, &line) {
            #[cfg(debug_assertions)]
            eprintln!("监听诊断日志写入失败：{_error}");
        }
    }
}

fn append_monitor_log(directory: &std::path::Path, line: &str) -> std::io::Result<()> {
    std::fs::create_dir_all(directory)?;
    let path = directory.join(format!("dy-screen-{}.jsonl", Utc::now().format("%Y-%m-%d")));
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    file.write_all(line.as_bytes())?;
    file.write_all(b"\n")?;
    file.flush()
}

async fn cancellable_delay(cancellation: &CancellationToken, duration: Duration) -> bool {
    tokio::select! {
        _ = cancellation.cancelled() => false,
        _ = tokio::time::sleep(duration) => true,
    }
}

async fn after_cancellable_delay<T, F>(
    cancellation: &CancellationToken,
    duration: Duration,
    operation: F,
) -> Option<T>
where
    F: Future<Output = T>,
{
    if !cancellable_delay(cancellation, duration).await {
        return None;
    }
    Some(operation.await)
}

struct PublicRequestGate {
    next_allowed: AsyncMutex<Instant>,
    minimum_interval: Duration,
}

impl PublicRequestGate {
    fn new(minimum_interval: Duration) -> Self {
        Self {
            next_allowed: AsyncMutex::new(Instant::now()),
            minimum_interval,
        }
    }

    async fn run<T, F>(&self, cancellation: &CancellationToken, operation: F) -> Option<T>
    where
        F: Future<Output = T>,
    {
        let mut next_allowed = tokio::select! {
            _ = cancellation.cancelled() => return None,
            guard = self.next_allowed.lock() => guard,
        };
        let wait = next_allowed.saturating_duration_since(Instant::now());
        if !wait.is_zero() && !cancellable_delay(cancellation, wait).await {
            return None;
        }
        *next_allowed = Instant::now() + self.minimum_interval;
        tokio::select! {
            _ = cancellation.cancelled() => None,
            result = operation => Some(result),
        }
    }
}

enum ResolveAttempt {
    Live(RoomStreams),
    Offline { room_id: Option<String> },
    Retryable(String, Option<u16>),
    AccessRestricted(String, Option<u16>),
    LayoutChanged(String, Option<u16>),
    EntryInvalid(String, Option<u16>),
    Cancelled,
}

enum SessionEnd {
    Completed,
    Cancelled,
    Error(String),
}

pub struct RecordingLimiter {
    limit: AtomicUsize,
    active: AtomicUsize,
    notify: Notify,
}

impl RecordingLimiter {
    pub fn new(limit: usize) -> Self {
        Self {
            limit: AtomicUsize::new(limit.max(1)),
            active: AtomicUsize::new(0),
            notify: Notify::new(),
        }
    }

    pub async fn acquire(self: &Arc<Self>) -> RecordingPermit {
        loop {
            let notified = self.notify.notified();
            let active = self.active.load(Ordering::Acquire);
            let limit = self.limit.load(Ordering::Acquire);
            if active < limit
                && self
                    .active
                    .compare_exchange(active, active + 1, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
            {
                return RecordingPermit {
                    limiter: self.clone(),
                };
            }
            notified.await;
        }
    }

    pub fn set_limit(&self, limit: usize) {
        self.limit.store(limit.max(1), Ordering::Release);
        self.notify.notify_waiters();
    }

    pub fn active(&self) -> usize {
        self.active.load(Ordering::Acquire)
    }
}

pub struct RecordingPermit {
    limiter: Arc<RecordingLimiter>,
}

impl Drop for RecordingPermit {
    fn drop(&mut self) {
        self.limiter.active.fetch_sub(1, Ordering::AcqRel);
        self.limiter.notify.notify_one();
    }
}

#[async_trait]
pub trait MonitorPublisher: Send + Sync {
    async fn publish(&self, event: MonitorEvent);
    async fn notify(&self, title: &str, body: &str);
}

#[derive(Default)]
pub struct NoopPublisher;

#[async_trait]
impl MonitorPublisher for NoopPublisher {
    async fn publish(&self, _event: MonitorEvent) {}
    async fn notify(&self, _title: &str, _body: &str) {}
}

#[async_trait]
pub trait ProfileDiscovery: Send + Sync {
    async fn inspect(&self, source_url: &str) -> dy_screen::Result<ProfileInspection>;
}

#[async_trait]
impl ProfileDiscovery for ProfileResolver {
    async fn inspect(&self, source_url: &str) -> dy_screen::Result<ProfileInspection> {
        ProfileResolver::inspect(self, source_url).await
    }
}

#[async_trait]
pub trait RoomDiscovery: Send + Sync {
    async fn inspect(&self, room_url: &str) -> dy_screen::Result<RoomInspection>;
}

#[async_trait]
impl RoomDiscovery for StreamResolver {
    async fn inspect(&self, room_url: &str) -> dy_screen::Result<RoomInspection> {
        StreamResolver::inspect(self, room_url).await
    }
}

pub trait JitterSource: Send + Sync {
    fn profile_seconds(&self) -> u64;
}

struct SystemJitter;

impl JitterSource for SystemJitter {
    fn profile_seconds(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| u64::from(duration.subsec_nanos()) % 11)
            .unwrap_or_default()
    }
}

#[async_trait]
pub trait DelayStrategy: Send + Sync {
    async fn sleep(&self, duration: Duration);
}

struct TokioDelay;

#[async_trait]
impl DelayStrategy for TokioDelay {
    async fn sleep(&self, duration: Duration) {
        tokio::time::sleep(duration).await;
    }
}

struct Worker {
    generation: u64,
    cancellation: CancellationToken,
    task: JoinHandle<()>,
    wake: mpsc::UnboundedSender<()>,
}

#[derive(Clone)]
pub struct Supervisor {
    database: Database,
    publisher: Arc<dyn MonitorPublisher>,
    profile_discovery: Arc<dyn ProfileDiscovery>,
    room_discovery: Arc<dyn RoomDiscovery>,
    jitter: Arc<dyn JitterSource>,
    delay: Arc<dyn DelayStrategy>,
    workers: Arc<Mutex<HashMap<i64, Worker>>>,
    worker_generation: Arc<AtomicU64>,
    recording_tokens: Arc<Mutex<HashMap<i64, CancellationToken>>>,
    recording_limiter: Arc<RecordingLimiter>,
    public_request_gate: Arc<PublicRequestGate>,
    monitor_logger: MonitorLogger,
    shutdown: CancellationToken,
    changes: broadcast::Sender<MonitorEvent>,
}

impl Supervisor {
    pub fn new(
        database: Database,
        publisher: Arc<dyn MonitorPublisher>,
        max_concurrent: usize,
    ) -> Result<Self, String> {
        let profile_discovery = ProfileResolver::new().map_err(|error| error.safe_message())?;
        let room_discovery = StreamResolver::new().map_err(|error| error.safe_message())?;
        let mut supervisor = Self::with_dependencies(
            database,
            publisher,
            max_concurrent,
            Arc::new(profile_discovery),
            Arc::new(room_discovery),
            Arc::new(SystemJitter),
            Arc::new(TokioDelay),
        );
        supervisor.public_request_gate =
            Arc::new(PublicRequestGate::new(PUBLIC_PAGE_REQUEST_INTERVAL));
        Ok(supervisor)
    }

    pub fn new_with_log_dir(
        database: Database,
        publisher: Arc<dyn MonitorPublisher>,
        max_concurrent: usize,
        log_dir: PathBuf,
    ) -> Result<Self, String> {
        let mut supervisor = Self::new(database, publisher, max_concurrent)?;
        supervisor.monitor_logger = MonitorLogger::file(log_dir);
        Ok(supervisor)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn with_dependencies(
        database: Database,
        publisher: Arc<dyn MonitorPublisher>,
        max_concurrent: usize,
        profile_discovery: Arc<dyn ProfileDiscovery>,
        room_discovery: Arc<dyn RoomDiscovery>,
        jitter: Arc<dyn JitterSource>,
        delay: Arc<dyn DelayStrategy>,
    ) -> Self {
        let (changes, _) = broadcast::channel(128);
        Self {
            database,
            publisher,
            profile_discovery,
            room_discovery,
            jitter,
            delay,
            workers: Arc::new(Mutex::new(HashMap::new())),
            worker_generation: Arc::new(AtomicU64::new(0)),
            recording_tokens: Arc::new(Mutex::new(HashMap::new())),
            recording_limiter: Arc::new(RecordingLimiter::new(max_concurrent)),
            public_request_gate: Arc::new(PublicRequestGate::new(Duration::ZERO)),
            monitor_logger: MonitorLogger::default(),
            shutdown: CancellationToken::new(),
            changes,
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<MonitorEvent> {
        self.changes.subscribe()
    }

    pub async fn restore(&self) -> Result<(), String> {
        let streamers = self
            .database
            .list_streamers(false)
            .map_err(|error| error.to_string())?;
        for streamer in streamers {
            if streamer.monitor_enabled {
                self.start(streamer.id)?;
            }
        }
        Ok(())
    }

    pub fn start(&self, streamer_id: i64) -> Result<(), String> {
        let mut workers = self.workers.lock().map_err(|_| "监听锁已损坏")?;
        if let Some(worker) = workers.get(&streamer_id) {
            let _ = worker.wake.send(());
            return Ok(());
        }
        let cancellation = self.shutdown.child_token();
        let task_cancellation = cancellation.clone();
        let (wake, wake_receiver) = mpsc::unbounded_channel();
        let generation = self.worker_generation.fetch_add(1, Ordering::Relaxed) + 1;
        let supervisor = self.clone();
        let task = tokio::spawn(async move {
            supervisor
                .worker_loop(streamer_id, task_cancellation, wake_receiver)
                .await;
            if let Ok(mut workers) = supervisor.workers.lock() {
                let is_current = workers
                    .get(&streamer_id)
                    .is_some_and(|worker| worker.generation == generation);
                if is_current {
                    workers.remove(&streamer_id);
                }
            }
        });
        workers.insert(
            streamer_id,
            Worker {
                generation,
                cancellation,
                task,
                wake,
            },
        );
        Ok(())
    }

    pub fn check_now(&self, streamer_id: i64) -> Result<(), String> {
        self.start(streamer_id)
    }

    pub fn check_all_now(&self) -> Result<(), String> {
        let streamers = self
            .database
            .list_streamers(false)
            .map_err(|error| error.to_string())?;
        for streamer in streamers {
            if streamer.monitor_enabled {
                self.start(streamer.id)?;
            }
        }
        Ok(())
    }

    pub fn worker_count(&self) -> usize {
        self.workers
            .lock()
            .map(|workers| workers.len())
            .unwrap_or_default()
    }

    pub fn set_max_concurrent(&self, limit: usize) {
        self.recording_limiter.set_limit(limit);
    }

    pub async fn stop(&self, streamer_id: i64) -> Result<(), String> {
        self.database
            .set_monitor_enabled(streamer_id, false)
            .map_err(|error| error.to_string())?;
        if let Ok(mut recording_tokens) = self.recording_tokens.lock()
            && let Some(token) = recording_tokens.remove(&streamer_id)
        {
            token.cancel();
        }
        let worker = self
            .workers
            .lock()
            .map_err(|_| "监听锁已损坏")?
            .remove(&streamer_id);
        if let Some(worker) = worker {
            worker.cancellation.cancel();
            if wait_for_worker(worker).await {
                self.database
                    .reconcile_streamer_sessions(streamer_id)
                    .map_err(|error| error.to_string())?;
            }
        }
        self.emit("streamer_changed", Some(streamer_id)).await;
        Ok(())
    }

    pub async fn resume(&self, streamer_id: i64) -> Result<(), String> {
        self.database
            .set_monitor_enabled(streamer_id, true)
            .map_err(|error| error.to_string())?;
        self.start(streamer_id)?;
        self.emit("streamer_changed", Some(streamer_id)).await;
        Ok(())
    }

    pub async fn stop_recording(&self, streamer_id: i64) {
        if let Ok(mut tokens) = self.recording_tokens.lock()
            && let Some(token) = tokens.remove(&streamer_id)
        {
            token.cancel();
        }
    }

    pub async fn pause_all(&self) -> Result<(), String> {
        let ids = self
            .database
            .list_streamers(false)
            .map_err(|error| error.to_string())?
            .into_iter()
            .filter(|streamer| streamer.monitor_enabled)
            .map(|streamer| streamer.id)
            .collect::<Vec<_>>();
        for id in ids {
            self.stop(id).await?;
        }
        Ok(())
    }

    pub async fn resume_all(&self) -> Result<(), String> {
        let streamers = self
            .database
            .list_streamers(false)
            .map_err(|error| error.to_string())?;
        for streamer in streamers {
            if !streamer.monitor_enabled {
                self.resume(streamer.id).await?;
            }
        }
        Ok(())
    }

    pub async fn shutdown(&self) {
        self.shutdown.cancel();
        let tokens = self
            .recording_tokens
            .lock()
            .map(|mut tokens| tokens.drain().map(|(_, token)| token).collect::<Vec<_>>())
            .unwrap_or_default();
        for token in tokens {
            token.cancel();
        }
        let workers = self
            .workers
            .lock()
            .map(|mut workers| {
                workers
                    .drain()
                    .map(|(_, worker)| worker)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        for worker in &workers {
            worker.cancellation.cancel();
        }
        let _ = wait_for_workers(workers, Duration::from_secs(10)).await;
        let _ = self.database.reconcile_startup();
    }

    async fn worker_loop(
        &self,
        streamer_id: i64,
        cancellation: CancellationToken,
        mut wake_receiver: mpsc::UnboundedReceiver<()>,
    ) {
        let mut room_failures = 0usize;
        let mut profile_failures = 0usize;
        let mut entry_invalid_count = 0usize;
        let mut restored_failure_count = false;
        loop {
            if cancellation.is_cancelled() {
                break;
            }
            let mut streamer = match self.database.get_streamer(streamer_id) {
                Ok(streamer) if streamer.monitor_enabled && !streamer.archived => streamer,
                _ => break,
            };
            if !restored_failure_count {
                let saved_failures = streamer.failure_count.max(0) as usize;
                match streamer.monitor_status.as_str() {
                    "profile_error" => profile_failures = saved_failures,
                    "entry_invalid" => entry_invalid_count = saved_failures,
                    "access_restricted" | "layout_changed" | "retrying" => {
                        room_failures = saved_failures
                    }
                    _ => {}
                }
                restored_failure_count = true;
            }

            if streamer.source_kind == StreamerSourceKind::Profile
                && (streamer.web_rid.is_none() || streamer.room_url.is_none())
            {
                let profile_status = if streamer.monitor_status == "rediscovering" {
                    "rediscovering"
                } else {
                    "discovering"
                };
                let _ = self.database.update_streamer_status(
                    streamer_id,
                    "offline",
                    profile_status,
                    None,
                );
                self.emit("streamer_changed", Some(streamer_id)).await;
                let Some(inspection) = self
                    .public_request_gate
                    .run(
                        &cancellation,
                        self.profile_discovery.inspect(&streamer.source_url),
                    )
                    .await
                else {
                    break;
                };
                match inspection {
                    Ok(ProfileInspection::Offline { .. }) => {
                        profile_failures = 0;
                        let _ = self.database.update_streamer_status(
                            streamer_id,
                            "offline",
                            "waiting_first_live",
                            None,
                        );
                        self.emit("streamer_changed", Some(streamer_id)).await;
                        let wait = 60 + self.jitter.profile_seconds().min(10);
                        if !self
                            .wait_for_next(
                                Duration::from_secs(wait),
                                &cancellation,
                                &mut wake_receiver,
                            )
                            .await
                        {
                            break;
                        }
                        continue;
                    }
                    Ok(ProfileInspection::Live { room, .. }) => {
                        profile_failures = 0;
                        match self.database.bind_discovered_room(
                            streamer_id,
                            &room.web_rid,
                            &room.room_url,
                            room.room_id.as_deref(),
                        ) {
                            Ok(DiscoveryBinding::Bound(updated)) => {
                                streamer = *updated;
                                self.emit("streamer_changed", Some(streamer_id)).await;
                            }
                            Ok(DiscoveryBinding::Merged {
                                target_streamer_id, ..
                            }) => {
                                self.emit("streamer_merged", Some(target_streamer_id)).await;
                                let _ = self.start(target_streamer_id);
                                break;
                            }
                            Ok(DiscoveryBinding::Conflict { target_streamer_id }) => {
                                self.emit("streamer_changed", Some(streamer_id)).await;
                                self.emit("streamer_conflict", Some(target_streamer_id))
                                    .await;
                                break;
                            }
                            Err(error) => {
                                profile_failures += 1;
                                let safe_error = error.to_string();
                                let wait =
                                    profile_backoff_seconds(profile_failures.saturating_sub(1));
                                let _ = self.database.update_streamer_failure(
                                    streamer_id,
                                    "error",
                                    "profile_error",
                                    &safe_error,
                                    profile_failures,
                                    &retry_at(wait),
                                );
                                self.emit("streamer_changed", Some(streamer_id)).await;
                                if !self
                                    .wait_for_next(
                                        Duration::from_secs(wait),
                                        &cancellation,
                                        &mut wake_receiver,
                                    )
                                    .await
                                {
                                    break;
                                }
                                continue;
                            }
                        }
                    }
                    Err(error) => {
                        profile_failures += 1;
                        let safe_error = error.safe_message();
                        let wait = profile_backoff_seconds(profile_failures.saturating_sub(1));
                        let _ = self.database.update_streamer_failure(
                            streamer_id,
                            "error",
                            "profile_error",
                            &safe_error,
                            profile_failures,
                            &retry_at(wait),
                        );
                        self.emit("streamer_changed", Some(streamer_id)).await;
                        if !self
                            .wait_for_next(
                                Duration::from_secs(wait),
                                &cancellation,
                                &mut wake_receiver,
                            )
                            .await
                        {
                            break;
                        }
                        continue;
                    }
                }
            }

            let Some(room_url) = streamer.room_url.clone() else {
                let _ = self.database.update_streamer_status(
                    streamer_id,
                    "error",
                    "entry_invalid",
                    Some("直播间来源缺少可用入口"),
                );
                self.monitor_logger.log_room_check(
                    streamer_id,
                    streamer.web_rid.as_deref(),
                    "entry_invalid",
                    None,
                    1,
                    None,
                );
                break;
            };
            let _ = self
                .database
                .update_streamer_status(streamer_id, "checking", "waiting", None);
            self.emit("streamer_changed", Some(streamer_id)).await;

            let wait_seconds = match self.resolve_room(&room_url, &cancellation).await {
                ResolveAttempt::Live(room) => {
                    room_failures = 0;
                    entry_invalid_count = 0;
                    if let Some(web_rid) = streamer.web_rid.as_deref()
                        && let Ok(DiscoveryBinding::Bound(updated)) =
                            self.database.bind_discovered_room(
                                streamer_id,
                                web_rid,
                                &room_url,
                                Some(&room.room_id),
                            )
                    {
                        streamer = *updated;
                    }
                    let _ = self.database.update_streamer_status(
                        streamer_id,
                        "live",
                        "waiting_resource",
                        None,
                    );
                    self.emit("streamer_changed", Some(streamer_id)).await;
                    self.monitor_logger.log_room_check(
                        streamer_id,
                        streamer.web_rid.as_deref(),
                        "live",
                        None,
                        0,
                        None,
                    );
                    match self.record_live(streamer, cancellation.clone()).await {
                        Ok(SessionEnd::Completed) => {
                            let _ = self.database.update_streamer_status(
                                streamer_id,
                                "offline",
                                "waiting",
                                None,
                            );
                        }
                        Ok(SessionEnd::Cancelled) => {}
                        Ok(SessionEnd::Error(error)) | Err(error) => {
                            let _ = self.database.update_streamer_status(
                                streamer_id,
                                "live",
                                "recording_error",
                                Some(&error),
                            );
                            self.publisher.notify("录制异常", &error).await;
                        }
                    }
                    30
                }
                ResolveAttempt::Offline { room_id } => {
                    room_failures = 0;
                    entry_invalid_count = 0;
                    if let (Some(web_rid), Some(room_id)) =
                        (streamer.web_rid.as_deref(), room_id.as_deref())
                    {
                        let _ = self.database.bind_discovered_room(
                            streamer_id,
                            web_rid,
                            &room_url,
                            Some(room_id),
                        );
                    }
                    let _ = self.database.update_streamer_status(
                        streamer_id,
                        "offline",
                        "waiting",
                        None,
                    );
                    self.emit("streamer_changed", Some(streamer_id)).await;
                    self.monitor_logger.log_room_check(
                        streamer_id,
                        streamer.web_rid.as_deref(),
                        "offline",
                        None,
                        0,
                        None,
                    );
                    30
                }
                ResolveAttempt::Retryable(safe_error, http_status) => {
                    entry_invalid_count = 0;
                    room_failures += 1;
                    let wait = backoff_seconds(room_failures.saturating_sub(1));
                    let next_retry_at = retry_at(wait);
                    let _ = self.database.update_streamer_failure(
                        streamer_id,
                        "error",
                        "retrying",
                        &safe_error,
                        room_failures,
                        &next_retry_at,
                    );
                    self.monitor_logger.log_room_check(
                        streamer_id,
                        streamer.web_rid.as_deref(),
                        "retryable_error",
                        http_status,
                        room_failures,
                        Some(&next_retry_at),
                    );
                    self.emit("streamer_changed", Some(streamer_id)).await;
                    wait
                }
                ResolveAttempt::AccessRestricted(safe_error, http_status) => {
                    entry_invalid_count = 0;
                    room_failures += 1;
                    let wait = backoff_seconds(room_failures.saturating_sub(1));
                    let next_retry_at = retry_at(wait);
                    let _ = self.database.update_streamer_failure(
                        streamer_id,
                        "error",
                        "access_restricted",
                        &safe_error,
                        room_failures,
                        &next_retry_at,
                    );
                    self.monitor_logger.log_room_check(
                        streamer_id,
                        streamer.web_rid.as_deref(),
                        "access_restricted",
                        http_status,
                        room_failures,
                        Some(&next_retry_at),
                    );
                    self.emit("streamer_changed", Some(streamer_id)).await;
                    wait
                }
                ResolveAttempt::LayoutChanged(safe_error, http_status) => {
                    entry_invalid_count = 0;
                    room_failures += 1;
                    let wait = backoff_seconds(room_failures.saturating_sub(1));
                    let next_retry_at = retry_at(wait);
                    let _ = self.database.update_streamer_failure(
                        streamer_id,
                        "error",
                        "layout_changed",
                        &safe_error,
                        room_failures,
                        &next_retry_at,
                    );
                    self.monitor_logger.log_room_check(
                        streamer_id,
                        streamer.web_rid.as_deref(),
                        "layout_changed",
                        http_status,
                        room_failures,
                        Some(&next_retry_at),
                    );
                    self.emit("streamer_changed", Some(streamer_id)).await;
                    wait
                }
                ResolveAttempt::EntryInvalid(safe_error, http_status) => {
                    room_failures = 0;
                    entry_invalid_count = entry_invalid_count.saturating_add(1);
                    if streamer.source_kind == StreamerSourceKind::Profile
                        && entry_invalid_count >= 3
                        && self.database.clear_room_binding(streamer_id).is_ok()
                    {
                        self.monitor_logger.log_room_check(
                            streamer_id,
                            streamer.web_rid.as_deref(),
                            "entry_invalid",
                            http_status,
                            entry_invalid_count,
                            None,
                        );
                        self.emit("streamer_changed", Some(streamer_id)).await;
                        entry_invalid_count = 0;
                        continue;
                    }
                    let wait = 30;
                    let next_retry_at = retry_at(wait);
                    let _ = self.database.update_streamer_failure(
                        streamer_id,
                        "error",
                        "entry_invalid",
                        &safe_error,
                        entry_invalid_count,
                        &next_retry_at,
                    );
                    self.monitor_logger.log_room_check(
                        streamer_id,
                        streamer.web_rid.as_deref(),
                        "entry_invalid",
                        http_status,
                        entry_invalid_count,
                        Some(&next_retry_at),
                    );
                    self.emit("streamer_changed", Some(streamer_id)).await;
                    wait
                }
                ResolveAttempt::Cancelled => break,
            };

            if !self
                .wait_for_next(
                    Duration::from_secs(wait_seconds),
                    &cancellation,
                    &mut wake_receiver,
                )
                .await
            {
                break;
            }
        }
    }

    async fn wait_for_next(
        &self,
        duration: Duration,
        cancellation: &CancellationToken,
        wake_receiver: &mut mpsc::UnboundedReceiver<()>,
    ) -> bool {
        tokio::select! {
            _ = cancellation.cancelled() => false,
            _ = self.delay.sleep(duration) => true,
            _ = wake_receiver.recv() => true,
        }
    }

    async fn resolve_room(
        &self,
        room_url: &str,
        cancellation: &CancellationToken,
    ) -> ResolveAttempt {
        let Some(result) = self
            .public_request_gate
            .run(cancellation, self.room_discovery.inspect(room_url))
            .await
        else {
            return ResolveAttempt::Cancelled;
        };
        match result {
            Ok(RoomInspection::Live(room)) => ResolveAttempt::Live(room),
            Ok(RoomInspection::Offline { room_id }) => ResolveAttempt::Offline {
                room_id: Some(room_id),
            },
            Err(error) => match classify_resolve_error(&error) {
                ResolveFailure::Offline => ResolveAttempt::Offline { room_id: None },
                ResolveFailure::Retryable(error, status) => {
                    ResolveAttempt::Retryable(error, status)
                }
                ResolveFailure::AccessRestricted(error, status) => {
                    ResolveAttempt::AccessRestricted(error, status)
                }
                ResolveFailure::LayoutChanged(error, status) => {
                    ResolveAttempt::LayoutChanged(error, status)
                }
                ResolveFailure::EntryInvalid(error, status) => {
                    ResolveAttempt::EntryInvalid(error, status)
                }
            },
        }
    }

    async fn record_live(
        &self,
        streamer: Streamer,
        worker_cancellation: CancellationToken,
    ) -> Result<SessionEnd, String> {
        let room_url = streamer
            .room_url
            .clone()
            .ok_or_else(|| "尚未发现稳定直播入口".to_owned())?;
        let permit = tokio::select! {
            permit = self.recording_limiter.acquire() => permit,
            _ = worker_cancellation.cancelled() => return Ok(SessionEnd::Cancelled),
        };
        let room = match self.resolve_room(&room_url, &worker_cancellation).await {
            ResolveAttempt::Live(room) => room,
            ResolveAttempt::Offline { .. } => return Ok(SessionEnd::Completed),
            ResolveAttempt::Retryable(error, _)
            | ResolveAttempt::AccessRestricted(error, _)
            | ResolveAttempt::LayoutChanged(error, _)
            | ResolveAttempt::EntryInvalid(error, _) => {
                return Err(error);
            }
            ResolveAttempt::Cancelled => return Ok(SessionEnd::Cancelled),
        };
        let room_id = room.room_id.clone();
        self.database
            .update_current_room_id(streamer.id, &room_id)
            .map_err(|error| error.to_string())?;
        let settings = self
            .database
            .get_settings()
            .map_err(|error| error.to_string())?;
        std::fs::create_dir_all(&settings.output_root).map_err(|error| error.to_string())?;
        let available =
            available_space(&settings.output_root).map_err(|error| error.to_string())?;
        match decide_disk(available, false) {
            DiskDecision::BlockNew | DiskDecision::StopActive => {
                return Err("磁盘可用空间不足 2 GB，未启动新录制".to_owned());
            }
            DiskDecision::Warn => {
                self.publisher
                    .notify("磁盘空间警告", "录像磁盘可用空间低于 10 GB")
                    .await;
            }
            DiskDecision::Continue => {}
        }
        let protocol = settings
            .protocol
            .parse::<Protocol>()
            .map_err(|error| error.to_string())?;
        let selected = room
            .select(Some(&settings.quality), Some(protocol))
            .map_err(|error| error.safe_message())?;
        let session = self
            .database
            .start_session(streamer.id, &settings.output_root)
            .map_err(|error| error.to_string())?;
        let token = worker_cancellation.child_token();
        if let Ok(mut recording_tokens) = self.recording_tokens.lock() {
            recording_tokens.insert(streamer.id, token.clone());
        } else {
            let error = "录制锁已损坏".to_owned();
            let _ = self
                .database
                .finish_session(session.id, "error", Some(&error));
            return Err(error);
        }
        let disk_monitor_cancellation = CancellationToken::new();
        let disk_monitor = tokio::spawn(monitor_active_disk(
            PathBuf::from(&settings.output_root),
            token.clone(),
            disk_monitor_cancellation.clone(),
            self.publisher.clone(),
        ));
        let sink = Arc::new(DatabaseEventSink {
            database: self.database.clone(),
            session_id: session.id,
            changes: self.changes.clone(),
            publisher: self.publisher.clone(),
            streamer_id: streamer.id,
        });
        let recorder = FfmpegRecorder::new(RecordingConfig {
            ffmpeg: FfmpegConfig {
                executable: PathBuf::from(settings.ffmpeg_path),
                segment_seconds: settings.segment_seconds,
                ..FfmpegConfig::default()
            },
            ffprobe_executable: PathBuf::from(settings.ffprobe_path),
            output_root: PathBuf::from(settings.output_root),
            ..RecordingConfig::default()
        });
        let _ = self
            .database
            .update_streamer_status(streamer.id, "live", "recording", None);
        self.publisher
            .notify("开始录制", &format!("{} 已开播", streamer.name))
            .await;
        self.emit("streamer_changed", Some(streamer.id)).await;

        let mut retries = 0usize;
        let mut current_selected: SelectedStream = selected;
        let final_end = 'recording: loop {
            let result = recorder
                .record_selected_with_event_sink(
                    room_url.clone(),
                    room_id.clone(),
                    current_selected,
                    token.clone(),
                    sink.clone(),
                )
                .await;
            if token.is_cancelled() {
                break SessionEnd::Cancelled;
            }

            let mut last_error = result
                .error
                .unwrap_or_else(|| "直播流已结束，但直播间仍在线，准备继续录制".to_owned());
            let mut confirmation = self.resolve_room(&room_url, &token).await;
            loop {
                match confirmation {
                    ResolveAttempt::Offline { .. } => break 'recording SessionEnd::Completed,
                    ResolveAttempt::Cancelled => break 'recording SessionEnd::Cancelled,
                    ResolveAttempt::Live(_) => {}
                    ResolveAttempt::Retryable(error, _)
                    | ResolveAttempt::AccessRestricted(error, _)
                    | ResolveAttempt::LayoutChanged(error, _)
                    | ResolveAttempt::EntryInvalid(error, _) => last_error = error,
                }
                if retries >= 3 {
                    break 'recording SessionEnd::Error(last_error);
                }
                retries += 1;
                let _ = self
                    .database
                    .update_session_retry(session.id, retries as i64);
                let _ = self.database.update_streamer_status(
                    streamer.id,
                    "live",
                    "retrying",
                    Some(&last_error),
                );
                self.emit("streamer_changed", Some(streamer.id)).await;
                let Some(refreshed) = after_cancellable_delay(
                    &token,
                    Duration::from_secs(backoff_seconds(retries - 1)),
                    self.resolve_room(&room_url, &token),
                )
                .await
                else {
                    break 'recording SessionEnd::Cancelled;
                };
                match refreshed {
                    ResolveAttempt::Offline { .. } => break 'recording SessionEnd::Completed,
                    ResolveAttempt::Cancelled => break 'recording SessionEnd::Cancelled,
                    ResolveAttempt::Retryable(error, status) => {
                        confirmation = ResolveAttempt::Retryable(error, status);
                    }
                    ResolveAttempt::AccessRestricted(error, status) => {
                        confirmation = ResolveAttempt::AccessRestricted(error, status);
                    }
                    ResolveAttempt::LayoutChanged(error, status) => {
                        confirmation = ResolveAttempt::LayoutChanged(error, status);
                    }
                    ResolveAttempt::EntryInvalid(error, status) => {
                        confirmation = ResolveAttempt::EntryInvalid(error, status);
                    }
                    ResolveAttempt::Live(room) => {
                        current_selected =
                            match room.select(Some(&settings.quality), Some(protocol)) {
                                Ok(selected) => selected,
                                Err(error) => {
                                    confirmation =
                                        ResolveAttempt::Retryable(error.safe_message(), None);
                                    continue;
                                }
                            };
                        continue 'recording;
                    }
                }
            }
        };

        disk_monitor_cancellation.cancel();
        let _ = disk_monitor.await;
        if let Ok(mut recording_tokens) = self.recording_tokens.lock() {
            recording_tokens.remove(&streamer.id);
        }
        let (session_status, final_error) = match &final_end {
            SessionEnd::Completed => ("completed", None),
            SessionEnd::Cancelled => ("cancelled", Some("录制已取消")),
            SessionEnd::Error(error) => ("error", Some(error.as_str())),
        };
        let finish_result = self
            .database
            .finish_session(session.id, session_status, final_error)
            .map_err(|error| error.to_string());
        drop(permit);
        self.publisher
            .notify("录制结束", &format!("{} 本次录制已结束", streamer.name))
            .await;
        finish_result?;
        self.database
            .reconcile_session_manifests(session.id)
            .map_err(|error| error.to_string())?;
        self.emit("session_changed", Some(streamer.id)).await;
        Ok(final_end)
    }

    async fn emit(&self, kind: &str, streamer_id: Option<i64>) {
        let event = MonitorEvent {
            kind: kind.to_owned(),
            streamer_id,
        };
        let _ = self.changes.send(event.clone());
        self.publisher.publish(event).await;
    }
}

async fn wait_for_worker(worker: Worker) -> bool {
    wait_for_workers(vec![worker], Duration::from_secs(10)).await > 0
}

async fn wait_for_workers(mut workers: Vec<Worker>, timeout: Duration) -> usize {
    let deadline = tokio::time::Instant::now() + timeout;
    let mut aborted = 0;
    for worker in &mut workers {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let completed = !remaining.is_zero()
            && tokio::time::timeout(remaining, &mut worker.task)
                .await
                .is_ok();
        if !completed {
            worker.task.abort();
            aborted += 1;
            let _ = (&mut worker.task).await;
        }
    }
    aborted
}

async fn monitor_active_disk(
    output_root: PathBuf,
    recording_token: CancellationToken,
    cancellation: CancellationToken,
    publisher: Arc<dyn MonitorPublisher>,
) {
    let mut warned = false;
    loop {
        tokio::select! {
            _ = cancellation.cancelled() => break,
            _ = recording_token.cancelled() => break,
            _ = tokio::time::sleep(Duration::from_secs(30)) => {
                let Ok(available) = available_space(&output_root) else {
                    continue;
                };
                match decide_disk(available, true) {
                    DiskDecision::StopActive => {
                        publisher
                            .notify("磁盘空间不足", "可用空间低于 1 GB，正在安全停止录制")
                            .await;
                        recording_token.cancel();
                        break;
                    }
                    DiskDecision::Warn if !warned => {
                        warned = true;
                        publisher
                            .notify("磁盘空间警告", "录像磁盘可用空间低于 10 GB")
                            .await;
                    }
                    DiskDecision::Continue => warned = false,
                    DiskDecision::Warn | DiskDecision::BlockNew => {}
                }
            }
        }
    }
}

struct DatabaseEventSink {
    database: Database,
    session_id: i64,
    changes: broadcast::Sender<MonitorEvent>,
    publisher: Arc<dyn MonitorPublisher>,
    streamer_id: i64,
}

impl EventSink for DatabaseEventSink {
    fn emit(&self, event: JobEvent) -> bool {
        if let JobEvent::SegmentFinalized {
            path,
            started_at,
            ended_at,
            duration_seconds,
            audio_present,
            ..
        } = event
        {
            let size_bytes = std::fs::metadata(&path)
                .map(|metadata| metadata.len() as i64)
                .unwrap_or_default();
            if let Err(error) = self.database.add_video(&NewVideo {
                session_id: self.session_id,
                path: path.to_string_lossy().into_owned(),
                started_at,
                ended_at,
                duration_seconds,
                size_bytes,
                audio_present,
                status: "complete".to_owned(),
            }) {
                eprintln!("登记完成分片失败：{error}");
                return false;
            }
            let event = MonitorEvent {
                kind: "video_changed".to_owned(),
                streamer_id: Some(self.streamer_id),
            };
            let _ = self.changes.send(event.clone());
            let publisher = self.publisher.clone();
            tokio::spawn(async move {
                publisher.publish(event).await;
            });
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dy_screen::error::RecorderError;

    #[tokio::test]
    async fn database_event_sink_does_not_publish_when_video_insert_fails() {
        let database = Database::open_in_memory().unwrap();
        database.migrate().unwrap();
        let (changes, mut receiver) = broadcast::channel(4);
        let sink = DatabaseEventSink {
            database,
            session_id: i64::MAX,
            changes,
            publisher: Arc::new(NoopPublisher),
            streamer_id: 42,
        };

        let accepted = sink.emit(JobEvent::SegmentFinalized {
            room_url: "https://live.douyin.com/42".to_owned(),
            room_id: "42".to_owned(),
            path: PathBuf::from("/tmp/missing-session-segment.mkv"),
            started_at: Some("2026-07-18T12:00:00Z".to_owned()),
            ended_at: Some("2026-07-18T12:01:00Z".to_owned()),
            duration_seconds: Some(60),
            audio_present: Some(true),
        });

        assert!(!accepted);
        assert!(matches!(
            receiver.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ));
    }

    #[test]
    fn room_errors_are_classified_for_offline_retry_and_entry_recovery() {
        assert_eq!(
            classify_resolve_error(&RecorderError::RoomUnavailable),
            ResolveFailure::Offline
        );
        assert!(matches!(
            classify_resolve_error(&RecorderError::UnsupportedPageLayout),
            ResolveFailure::LayoutChanged(message, _) if !message.is_empty()
        ));
        assert!(matches!(
            classify_resolve_error(&RecorderError::RoomAccessRestricted),
            ResolveFailure::AccessRestricted(message, _) if !message.is_empty()
        ));
        assert!(matches!(
            classify_resolve_error(&RecorderError::RoomHttpStatus { status: 404 }),
            ResolveFailure::EntryInvalid(message, Some(404)) if !message.is_empty()
        ));
        assert!(matches!(
            classify_resolve_error(&RecorderError::RoomHttpStatus { status: 503 }),
            ResolveFailure::Retryable(message, Some(503)) if !message.is_empty()
        ));
    }

    #[tokio::test]
    async fn retry_delay_is_cancelled_immediately() {
        let cancellation = CancellationToken::new();
        let waiting = tokio::spawn({
            let cancellation = cancellation.clone();
            async move { cancellable_delay(&cancellation, Duration::from_secs(300)).await }
        });
        tokio::task::yield_now().await;
        cancellation.cancel();

        let completed = tokio::time::timeout(Duration::from_millis(100), waiting)
            .await
            .expect("retry delay should observe cancellation")
            .expect("retry task");
        assert!(!completed);
    }

    #[tokio::test]
    async fn multiple_workers_share_one_global_shutdown_deadline() {
        let workers = (0..3)
            .map(|_| {
                let (wake, _) = mpsc::unbounded_channel();
                Worker {
                    generation: 0,
                    cancellation: CancellationToken::new(),
                    task: tokio::spawn(std::future::pending()),
                    wake,
                }
            })
            .collect();
        let started = tokio::time::Instant::now();

        let aborted = wait_for_workers(workers, Duration::from_millis(50)).await;

        assert_eq!(aborted, 3);
        assert!(started.elapsed() < Duration::from_millis(200));
    }

    #[tokio::test]
    async fn retry_operation_is_not_polled_until_delay_finishes() {
        let cancellation = CancellationToken::new();
        let called = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let operation = {
            let called = called.clone();
            async move {
                called.store(true, Ordering::SeqCst);
                42
            }
        };
        let task = tokio::spawn({
            let cancellation = cancellation.clone();
            async move {
                after_cancellable_delay(&cancellation, Duration::from_millis(40), operation).await
            }
        });

        tokio::time::sleep(Duration::from_millis(10)).await;
        assert!(!called.load(Ordering::SeqCst));
        assert_eq!(task.await.unwrap(), Some(42));
        assert!(called.load(Ordering::SeqCst));
    }
}
