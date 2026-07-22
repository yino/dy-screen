use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use dy_screen::error::{RecorderError, Result as RecorderResult};
use dy_screen::model::{
    ProfileIdentity, ProfileInspection, ProfileRoom, Protocol, RoomStreams, StreamVariant,
};
use dy_screen::resolver::RoomInspection;
use dy_screen_app_lib::database::Database;
use dy_screen_app_lib::domain::{
    AppSettings, MonitorEvent, NewStreamer, StreamerSourceKind, StreamerTagInput,
};
use dy_screen_app_lib::supervisor::{
    DelayStrategy, JitterSource, MonitorPublisher, NoopPublisher, ProfileDiscovery, RoomDiscovery,
    Supervisor, profile_backoff_seconds,
};
use tokio::sync::Semaphore;

#[derive(Clone)]
enum ProfileReply {
    Offline(&'static str),
    Live {
        profile_sec_uid: &'static str,
        web_rid: &'static str,
        room_id: &'static str,
    },
    Error,
}

#[derive(Clone)]
struct FakeProfileDiscovery {
    replies: Arc<Mutex<VecDeque<ProfileReply>>>,
    calls: Arc<Mutex<Vec<String>>>,
}

impl FakeProfileDiscovery {
    fn new(replies: impl IntoIterator<Item = ProfileReply>) -> Self {
        Self {
            replies: Arc::new(Mutex::new(replies.into_iter().collect())),
            calls: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

#[async_trait]
impl ProfileDiscovery for FakeProfileDiscovery {
    async fn inspect(&self, source_url: &str) -> RecorderResult<ProfileInspection> {
        self.calls.lock().unwrap().push(source_url.to_owned());
        match self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(ProfileReply::Offline("profile-default"))
        {
            ProfileReply::Offline(profile_sec_uid) => Ok(ProfileInspection::Offline {
                identity: ProfileIdentity {
                    profile_sec_uid: profile_sec_uid.to_owned(),
                    display_name: Some("测试主播".to_owned()),
                },
            }),
            ProfileReply::Live {
                profile_sec_uid,
                web_rid,
                room_id,
            } => Ok(ProfileInspection::Live {
                identity: ProfileIdentity {
                    profile_sec_uid: profile_sec_uid.to_owned(),
                    display_name: Some("测试主播".to_owned()),
                },
                room: ProfileRoom {
                    web_rid: web_rid.to_owned(),
                    room_url: format!("https://live.douyin.com/{web_rid}"),
                    room_id: Some(room_id.to_owned()),
                },
            }),
            ProfileReply::Error => Err(RecorderError::UnsupportedPageLayout),
        }
    }
}

#[derive(Clone)]
enum RoomReply {
    Live(&'static str),
    Offline(&'static str),
    EntryInvalid,
    Retryable,
}

#[derive(Clone)]
struct FakeRoomDiscovery {
    replies: Arc<Mutex<VecDeque<RoomReply>>>,
    calls: Arc<Mutex<Vec<String>>>,
    binding_observations: Arc<Mutex<Vec<bool>>>,
    database: Option<Database>,
    streamer_id: Option<i64>,
}

impl FakeRoomDiscovery {
    fn new(replies: impl IntoIterator<Item = RoomReply>) -> Self {
        Self {
            replies: Arc::new(Mutex::new(replies.into_iter().collect())),
            calls: Arc::new(Mutex::new(Vec::new())),
            binding_observations: Arc::new(Mutex::new(Vec::new())),
            database: None,
            streamer_id: None,
        }
    }

    fn observing(mut self, database: Database, streamer_id: i64) -> Self {
        self.database = Some(database);
        self.streamer_id = Some(streamer_id);
        self
    }
}

#[async_trait]
impl RoomDiscovery for FakeRoomDiscovery {
    async fn inspect(&self, room_url: &str) -> RecorderResult<RoomInspection> {
        self.calls.lock().unwrap().push(room_url.to_owned());
        if let (Some(database), Some(streamer_id)) = (&self.database, self.streamer_id) {
            self.binding_observations.lock().unwrap().push(
                database
                    .get_streamer(streamer_id)
                    .ok()
                    .and_then(|streamer| streamer.web_rid)
                    .is_some(),
            );
        }
        match self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(RoomReply::Offline("room-default"))
        {
            RoomReply::Live(room_id) => Ok(RoomInspection::Live(RoomStreams {
                room_id: room_id.to_owned(),
                status: Some(2),
                default_quality: Some("HD1".to_owned()),
                variants: vec![StreamVariant {
                    quality: "HD1".to_owned(),
                    protocol: Protocol::Flv,
                    url: "https://pull.example/live.flv?auth_key=fixture-secret".to_owned(),
                }],
            })),
            RoomReply::Offline(room_id) => Ok(RoomInspection::Offline {
                room_id: room_id.to_owned(),
            }),
            RoomReply::EntryInvalid => Err(RecorderError::UnsupportedPageLayout),
            RoomReply::Retryable => Err(RecorderError::ProfileHttpStatus { status: 503 }),
        }
    }
}

#[derive(Clone)]
struct FixedJitter(u64);

impl JitterSource for FixedJitter {
    fn profile_seconds(&self) -> u64 {
        self.0
    }
}

#[derive(Clone)]
struct ControlledDelay {
    durations: Arc<Mutex<Vec<Duration>>>,
    permits: Arc<Semaphore>,
}

impl Default for ControlledDelay {
    fn default() -> Self {
        Self {
            durations: Arc::new(Mutex::new(Vec::new())),
            permits: Arc::new(Semaphore::new(0)),
        }
    }
}

impl ControlledDelay {
    fn advance(&self) {
        self.permits.add_permits(1);
    }
}

#[async_trait]
impl DelayStrategy for ControlledDelay {
    async fn sleep(&self, duration: Duration) {
        self.durations.lock().unwrap().push(duration);
        let permit = self.permits.acquire().await.unwrap();
        permit.forget();
    }
}

struct BlockingFirstPublisher {
    blocked_once: AtomicBool,
    entered: Semaphore,
    release: Semaphore,
}

impl Default for BlockingFirstPublisher {
    fn default() -> Self {
        Self {
            blocked_once: AtomicBool::new(false),
            entered: Semaphore::new(0),
            release: Semaphore::new(0),
        }
    }
}

#[async_trait]
impl MonitorPublisher for BlockingFirstPublisher {
    async fn publish(&self, _event: MonitorEvent) {
        if self
            .blocked_once
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            self.entered.add_permits(1);
            let permit = self.release.acquire().await.unwrap();
            permit.forget();
        }
    }

    async fn notify(&self, _title: &str, _body: &str) {}
}

fn waiting_profile(database: &Database, profile_sec_uid: &str, enabled: bool) -> i64 {
    database
        .add_streamer(&NewStreamer {
            name: "主页主播".to_owned(),
            source_kind: StreamerSourceKind::Profile,
            source_url: format!("https://www.douyin.com/user/{profile_sec_uid}"),
            profile_sec_uid: Some(profile_sec_uid.to_owned()),
            web_rid: None,
            room_url: None,
            room_id: None,
            monitor_enabled: enabled,
            tags: Vec::new(),
        })
        .unwrap()
        .id
}

async fn wait_until(mut condition: impl FnMut() -> bool) {
    for _ in 0..100 {
        if condition() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("等待测试条件超时");
}

#[test]
fn profile_retry_backoff_uses_sixty_one_twenty_and_three_hundred_seconds() {
    assert_eq!(profile_backoff_seconds(0), 60);
    assert_eq!(profile_backoff_seconds(1), 120);
    assert_eq!(profile_backoff_seconds(2), 300);
    assert_eq!(profile_backoff_seconds(20), 300);
}

#[tokio::test]
async fn restore_selects_profile_or_room_phase_and_skips_paused_streamer() {
    let database = Database::open_in_memory().unwrap();
    database.migrate().unwrap();
    let waiting_id = waiting_profile(&database, "profile-waiting", true);
    let room = database
        .add_streamer(&NewStreamer::room("直播间主播", "401", "room-401", true))
        .unwrap();
    let paused_id = waiting_profile(&database, "profile-paused", false);
    let profile = Arc::new(FakeProfileDiscovery::new([ProfileReply::Offline(
        "profile-waiting",
    )]));
    let room_discovery = Arc::new(FakeRoomDiscovery::new([RoomReply::Offline("room-401")]));
    let delay = Arc::new(ControlledDelay::default());
    let supervisor = Supervisor::with_dependencies(
        database.clone(),
        Arc::new(NoopPublisher),
        4,
        profile.clone(),
        room_discovery.clone(),
        Arc::new(FixedJitter(0)),
        delay,
    );

    supervisor.restore().await.unwrap();
    wait_until(|| {
        profile.calls.lock().unwrap().len() == 1 && room_discovery.calls.lock().unwrap().len() == 1
    })
    .await;

    assert_eq!(supervisor.worker_count(), 2);
    assert_eq!(
        database.get_streamer(waiting_id).unwrap().monitor_status,
        "waiting_first_live"
    );
    assert_eq!(
        database.get_streamer(room.id).unwrap().monitor_status,
        "waiting"
    );
    assert_eq!(
        database.get_streamer(paused_id).unwrap().monitor_status,
        "paused"
    );
    supervisor.shutdown().await;
}

#[tokio::test]
async fn exiting_old_worker_does_not_remove_a_resumed_worker_for_the_same_streamer() {
    let database = Database::open_in_memory().unwrap();
    database.migrate().unwrap();
    let streamer_id = waiting_profile(&database, "profile-worker-generation", true);
    let publisher = Arc::new(BlockingFirstPublisher::default());
    let supervisor = Supervisor::with_dependencies(
        database,
        publisher.clone(),
        4,
        Arc::new(FakeProfileDiscovery::new([
            ProfileReply::Offline("profile-worker-generation"),
            ProfileReply::Offline("profile-worker-generation"),
        ])),
        Arc::new(FakeRoomDiscovery::new([])),
        Arc::new(FixedJitter(0)),
        Arc::new(ControlledDelay::default()),
    );

    supervisor.start(streamer_id).unwrap();
    let entered = publisher.entered.acquire().await.unwrap();
    entered.forget();

    let stop_task = tokio::spawn({
        let supervisor = supervisor.clone();
        async move { supervisor.stop(streamer_id).await }
    });
    wait_until(|| supervisor.worker_count() == 0).await;

    supervisor.resume(streamer_id).await.unwrap();
    assert_eq!(supervisor.worker_count(), 1);
    publisher.release.add_permits(1);
    stop_task.await.unwrap().unwrap();
    tokio::time::sleep(Duration::from_millis(20)).await;

    assert_eq!(supervisor.worker_count(), 1);
    supervisor.shutdown().await;
}

#[tokio::test]
async fn offline_profile_uses_sixty_seconds_plus_jitter() {
    let database = Database::open_in_memory().unwrap();
    database.migrate().unwrap();
    let streamer_id = waiting_profile(&database, "profile-jitter", true);
    let delay = Arc::new(ControlledDelay::default());
    let supervisor = Supervisor::with_dependencies(
        database.clone(),
        Arc::new(NoopPublisher),
        4,
        Arc::new(FakeProfileDiscovery::new([ProfileReply::Offline(
            "profile-jitter",
        )])),
        Arc::new(FakeRoomDiscovery::new([])),
        Arc::new(FixedJitter(7)),
        delay.clone(),
    );

    supervisor.start(streamer_id).unwrap();
    wait_until(|| !delay.durations.lock().unwrap().is_empty()).await;

    assert_eq!(delay.durations.lock().unwrap()[0], Duration::from_secs(67));
    assert_eq!(
        database.get_streamer(streamer_id).unwrap().live_status,
        "offline"
    );
    assert_eq!(
        database.get_streamer(streamer_id).unwrap().monitor_status,
        "waiting_first_live"
    );
    supervisor.shutdown().await;
}

#[tokio::test]
async fn first_profile_discovery_persists_binding_before_immediate_room_check() {
    let database = Database::open_in_memory().unwrap();
    database.migrate().unwrap();
    let streamer_id = waiting_profile(&database, "profile-first-live", true);
    let profile = Arc::new(FakeProfileDiscovery::new([ProfileReply::Live {
        profile_sec_uid: "profile-first-live",
        web_rid: "501",
        room_id: "room-501-a",
    }]));
    let room = Arc::new(
        FakeRoomDiscovery::new([RoomReply::Offline("room-501-a")])
            .observing(database.clone(), streamer_id),
    );
    let delay = Arc::new(ControlledDelay::default());
    let supervisor = Supervisor::with_dependencies(
        database.clone(),
        Arc::new(NoopPublisher),
        4,
        profile.clone(),
        room.clone(),
        Arc::new(FixedJitter(0)),
        delay,
    );

    supervisor.start(streamer_id).unwrap();
    wait_until(|| !room.calls.lock().unwrap().is_empty()).await;

    let saved = database.get_streamer(streamer_id).unwrap();
    assert_eq!(saved.web_rid.as_deref(), Some("501"));
    assert_eq!(
        saved.room_url.as_deref(),
        Some("https://live.douyin.com/501")
    );
    assert_eq!(saved.room_id.as_deref(), Some("room-501-a"));
    assert_eq!(room.binding_observations.lock().unwrap().as_slice(), [true]);
    supervisor.shutdown().await;
}

#[tokio::test]
async fn three_entry_invalid_results_clear_binding_and_return_to_profile_discovery() {
    let database = Database::open_in_memory().unwrap();
    database.migrate().unwrap();
    let streamer = database
        .add_streamer(&NewStreamer {
            name: "失效入口主播".to_owned(),
            source_kind: StreamerSourceKind::Profile,
            source_url: "https://www.douyin.com/user/profile-rediscover".to_owned(),
            profile_sec_uid: Some("profile-rediscover".to_owned()),
            web_rid: Some("601".to_owned()),
            room_url: Some("https://live.douyin.com/601".to_owned()),
            room_id: Some("room-601".to_owned()),
            monitor_enabled: true,
            tags: Vec::new(),
        })
        .unwrap();
    let profile = Arc::new(FakeProfileDiscovery::new([ProfileReply::Offline(
        "profile-rediscover",
    )]));
    let room = Arc::new(FakeRoomDiscovery::new([
        RoomReply::EntryInvalid,
        RoomReply::EntryInvalid,
        RoomReply::EntryInvalid,
    ]));
    let delay = Arc::new(ControlledDelay::default());
    let supervisor = Supervisor::with_dependencies(
        database.clone(),
        Arc::new(NoopPublisher),
        4,
        profile.clone(),
        room.clone(),
        Arc::new(FixedJitter(0)),
        delay.clone(),
    );

    supervisor.start(streamer.id).unwrap();
    for expected_calls in 1..=3 {
        wait_until(|| room.calls.lock().unwrap().len() == expected_calls).await;
        if expected_calls < 3 {
            delay.advance();
        }
    }
    wait_until(|| profile.calls.lock().unwrap().len() == 1).await;

    let saved = database.get_streamer(streamer.id).unwrap();
    assert_eq!(saved.web_rid, None);
    assert_eq!(saved.room_url, None);
    assert_eq!(saved.room_id, None);
    assert_eq!(saved.monitor_status, "waiting_first_live");
    supervisor.shutdown().await;
}

#[tokio::test]
async fn offline_room_updates_current_room_id_without_changing_stable_identity() {
    let database = Database::open_in_memory().unwrap();
    database.migrate().unwrap();
    let streamer = database
        .add_streamer(&NewStreamer::room("周期主播", "701", "old-room-id", true))
        .unwrap();
    let delay = Arc::new(ControlledDelay::default());
    let supervisor = Supervisor::with_dependencies(
        database.clone(),
        Arc::new(NoopPublisher),
        4,
        Arc::new(FakeProfileDiscovery::new([])),
        Arc::new(FakeRoomDiscovery::new([RoomReply::Offline("new-room-id")])),
        Arc::new(FixedJitter(0)),
        delay,
    );

    supervisor.start(streamer.id).unwrap();
    wait_until(|| {
        database
            .get_streamer(streamer.id)
            .unwrap()
            .room_id
            .as_deref()
            == Some("new-room-id")
    })
    .await;

    let saved = database.get_streamer(streamer.id).unwrap();
    assert_eq!(saved.web_rid.as_deref(), Some("701"));
    assert_eq!(saved.room_id.as_deref(), Some("new-room-id"));
    supervisor.shutdown().await;
}

#[tokio::test]
async fn profile_errors_follow_bounded_backoff_and_keep_one_cancellable_worker() {
    let database = Database::open_in_memory().unwrap();
    database.migrate().unwrap();
    let streamer_id = waiting_profile(&database, "profile-errors", true);
    let profile = Arc::new(FakeProfileDiscovery::new([
        ProfileReply::Error,
        ProfileReply::Error,
        ProfileReply::Error,
    ]));
    let delay = Arc::new(ControlledDelay::default());
    let supervisor = Supervisor::with_dependencies(
        database.clone(),
        Arc::new(NoopPublisher),
        4,
        profile.clone(),
        Arc::new(FakeRoomDiscovery::new([])),
        Arc::new(FixedJitter(0)),
        delay.clone(),
    );

    supervisor.start(streamer_id).unwrap();
    for (index, expected) in [60, 120, 300].into_iter().enumerate() {
        wait_until(|| delay.durations.lock().unwrap().len() > index).await;
        assert_eq!(
            delay.durations.lock().unwrap()[index],
            Duration::from_secs(expected)
        );
        assert_eq!(supervisor.worker_count(), 1);
        if index < 2 {
            delay.advance();
        }
    }
    assert_eq!(
        database.get_streamer(streamer_id).unwrap().monitor_status,
        "profile_error"
    );
    supervisor.shutdown().await;
}

#[tokio::test]
async fn retryable_room_failure_and_single_entry_invalid_do_not_clear_binding() {
    let database = Database::open_in_memory().unwrap();
    database.migrate().unwrap();
    let streamer = database
        .add_streamer(&NewStreamer {
            name: "稳定入口主播".to_owned(),
            source_kind: StreamerSourceKind::Profile,
            source_url: "https://www.douyin.com/user/profile-stable".to_owned(),
            profile_sec_uid: Some("profile-stable".to_owned()),
            web_rid: Some("702".to_owned()),
            room_url: Some("https://live.douyin.com/702".to_owned()),
            room_id: Some("room-702".to_owned()),
            monitor_enabled: true,
            tags: Vec::new(),
        })
        .unwrap();
    let room = Arc::new(FakeRoomDiscovery::new([
        RoomReply::Retryable,
        RoomReply::EntryInvalid,
    ]));
    let profile = Arc::new(FakeProfileDiscovery::new([]));
    let delay = Arc::new(ControlledDelay::default());
    let supervisor = Supervisor::with_dependencies(
        database.clone(),
        Arc::new(NoopPublisher),
        4,
        profile.clone(),
        room.clone(),
        Arc::new(FixedJitter(0)),
        delay.clone(),
    );

    supervisor.start(streamer.id).unwrap();
    wait_until(|| room.calls.lock().unwrap().len() == 1).await;
    assert_eq!(
        database
            .get_streamer(streamer.id)
            .unwrap()
            .web_rid
            .as_deref(),
        Some("702")
    );
    delay.advance();
    wait_until(|| room.calls.lock().unwrap().len() == 2).await;
    assert_eq!(
        database
            .get_streamer(streamer.id)
            .unwrap()
            .web_rid
            .as_deref(),
        Some("702")
    );
    assert!(profile.calls.lock().unwrap().is_empty());
    supervisor.shutdown().await;
}

#[tokio::test]
async fn delayed_duplicate_discovery_emits_target_and_removes_temporary_worker_record() {
    let database = Database::open_in_memory().unwrap();
    database.migrate().unwrap();
    let target = database
        .add_streamer(&NewStreamer::room("已有直播间", "803", "room-803", false))
        .unwrap();
    database
        .replace_streamer_tags(
            target.id,
            &[StreamerTagInput {
                name: "带货".to_owned(),
                prompt_guidance: None,
            }],
        )
        .unwrap();
    let temporary_id = waiting_profile(&database, "profile-merge-worker", true);
    database
        .replace_streamer_tags(
            temporary_id,
            &[StreamerTagInput {
                name: "搞笑".to_owned(),
                prompt_guidance: Some("关注幽默表达".to_owned()),
            }],
        )
        .unwrap();
    let profile = Arc::new(FakeProfileDiscovery::new([ProfileReply::Live {
        profile_sec_uid: "profile-merge-worker",
        web_rid: "803",
        room_id: "room-803-new",
    }]));
    let delay = Arc::new(ControlledDelay::default());
    let supervisor = Supervisor::with_dependencies(
        database.clone(),
        Arc::new(NoopPublisher),
        4,
        profile,
        Arc::new(FakeRoomDiscovery::new([])),
        Arc::new(FixedJitter(0)),
        delay,
    );
    let mut events = supervisor.subscribe();

    supervisor.start(temporary_id).unwrap();
    let merged = loop {
        let event = tokio::time::timeout(Duration::from_secs(1), events.recv())
            .await
            .unwrap()
            .unwrap();
        if event.kind == "streamer_merged" {
            break event;
        }
    };

    assert_eq!(merged.streamer_id, Some(target.id));
    assert!(database.get_streamer(temporary_id).is_err());
    let saved_target = database.get_streamer(target.id).unwrap();
    assert_eq!(
        saved_target.profile_sec_uid.as_deref(),
        Some("profile-merge-worker")
    );
    assert_eq!(
        saved_target
            .tags
            .iter()
            .map(|tag| tag.name.as_str())
            .collect::<Vec<_>>(),
        ["带货", "搞笑"]
    );
    wait_until(|| supervisor.worker_count() == 1).await;
    supervisor.shutdown().await;
}

#[tokio::test]
async fn restart_resumes_waiting_profile_and_first_live_creates_session_through_room_resolver() {
    let directory = tempfile::tempdir().unwrap();
    let database = Database::open_in_memory().unwrap();
    database.migrate().unwrap();
    let mut settings = AppSettings::defaults();
    settings.output_root = directory.path().to_string_lossy().into_owned();
    settings.ffmpeg_path = "/usr/bin/true".to_owned();
    settings.ffprobe_path = "/usr/bin/true".to_owned();
    database.save_settings(&settings).unwrap();
    let streamer_id = waiting_profile(&database, "profile-e2e", true);

    let first_delay = Arc::new(ControlledDelay::default());
    let first = Supervisor::with_dependencies(
        database.clone(),
        Arc::new(NoopPublisher),
        4,
        Arc::new(FakeProfileDiscovery::new([ProfileReply::Offline(
            "profile-e2e",
        )])),
        Arc::new(FakeRoomDiscovery::new([])),
        Arc::new(FixedJitter(0)),
        first_delay,
    );
    first.restore().await.unwrap();
    wait_until(|| {
        database.get_streamer(streamer_id).unwrap().monitor_status == "waiting_first_live"
    })
    .await;
    first.shutdown().await;

    let profile = Arc::new(FakeProfileDiscovery::new([ProfileReply::Live {
        profile_sec_uid: "profile-e2e",
        web_rid: "904",
        room_id: "room-904-live",
    }]));
    let room = Arc::new(FakeRoomDiscovery::new([
        RoomReply::Live("room-904-live"),
        RoomReply::Live("room-904-live"),
        RoomReply::Offline("room-904-live"),
    ]));
    let second = Supervisor::with_dependencies(
        database.clone(),
        Arc::new(NoopPublisher),
        4,
        profile.clone(),
        room.clone(),
        Arc::new(FixedJitter(0)),
        Arc::new(ControlledDelay::default()),
    );

    second.restore().await.unwrap();
    wait_until(|| {
        database
            .get_session(1)
            .is_ok_and(|session| session.ended_at.is_some())
    })
    .await;

    let saved = database.get_streamer(streamer_id).unwrap();
    assert_eq!(saved.web_rid.as_deref(), Some("904"));
    assert_eq!(saved.room_id.as_deref(), Some("room-904-live"));
    assert_eq!(profile.calls.lock().unwrap().len(), 1);
    assert!(room.calls.lock().unwrap().len() >= 3);
    assert_eq!(database.get_session(1).unwrap().streamer_id, streamer_id);
    second.shutdown().await;
}

#[tokio::test]
async fn recording_revalidation_persists_and_uses_the_latest_room_id() {
    let directory = tempfile::tempdir().unwrap();
    let database = Database::open_in_memory().unwrap();
    database.migrate().unwrap();
    let mut settings = AppSettings::defaults();
    settings.output_root = directory.path().to_string_lossy().into_owned();
    settings.ffmpeg_path = "/usr/bin/true".to_owned();
    settings.ffprobe_path = "/usr/bin/true".to_owned();
    database.save_settings(&settings).unwrap();
    let streamer = database
        .add_streamer(&NewStreamer {
            name: "跨场次主播".to_owned(),
            source_kind: StreamerSourceKind::Profile,
            source_url: "https://www.douyin.com/user/profile-room-refresh".to_owned(),
            profile_sec_uid: Some("profile-room-refresh".to_owned()),
            web_rid: Some("905".to_owned()),
            room_url: Some("https://live.douyin.com/905".to_owned()),
            room_id: Some("room-A".to_owned()),
            monitor_enabled: true,
            tags: Vec::new(),
        })
        .unwrap();
    let room = Arc::new(FakeRoomDiscovery::new([
        RoomReply::Live("room-A"),
        RoomReply::Live("room-B"),
        RoomReply::Offline("room-B"),
    ]));
    let supervisor = Supervisor::with_dependencies(
        database.clone(),
        Arc::new(NoopPublisher),
        4,
        Arc::new(FakeProfileDiscovery::new([])),
        room,
        Arc::new(FixedJitter(0)),
        Arc::new(ControlledDelay::default()),
    );

    supervisor.start(streamer.id).unwrap();
    wait_until(|| {
        database
            .get_session(1)
            .is_ok_and(|session| session.ended_at.is_some())
    })
    .await;

    assert_eq!(
        database
            .get_streamer(streamer.id)
            .unwrap()
            .room_id
            .as_deref(),
        Some("room-B")
    );
    assert!(directory.path().join("room-B").is_dir());
    assert!(!directory.path().join("room-A").exists());
    supervisor.shutdown().await;
}
