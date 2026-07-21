use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use dy_screen::error::{RecorderError, Result as RecorderResult};
use dy_screen::model::{ProfileIdentity, ProfileInspection, ProfileRoom, RoomStreams};
use dy_screen::resolver::RoomInspection;
use dy_screen_app_lib::database::Database;
use dy_screen_app_lib::domain::{CreateStreamerRequest, NewStreamer, StreamerSourceKind};
use dy_screen_app_lib::streamer_service::{
    SourceInspector, WorkerControl, create_streamer_with, update_streamer_with,
};

const PROFILE_UID: &str = "profile-service-a";

#[derive(Clone)]
enum ProfileReply {
    Live {
        profile_sec_uid: String,
        web_rid: String,
        room_id: String,
    },
    Offline {
        profile_sec_uid: String,
    },
    Unsupported,
}

#[derive(Clone)]
struct FakeInspector {
    profile: ProfileReply,
    calls: Arc<Mutex<Vec<String>>>,
}

impl FakeInspector {
    fn live() -> Self {
        Self {
            profile: ProfileReply::Live {
                profile_sec_uid: PROFILE_UID.to_owned(),
                web_rid: "236150550962".to_owned(),
                room_id: "7664620130978581282".to_owned(),
            },
            calls: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn offline() -> Self {
        Self {
            profile: ProfileReply::Offline {
                profile_sec_uid: PROFILE_UID.to_owned(),
            },
            calls: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn unsupported(profile_sec_uid: &str) -> Self {
        Self {
            profile: ProfileReply::Unsupported,
            calls: Arc::new(Mutex::new(vec![format!("expected:{profile_sec_uid}")])),
        }
    }
}

#[async_trait]
impl SourceInspector for FakeInspector {
    async fn inspect_profile(&self, source_url: &str) -> RecorderResult<ProfileInspection> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("profile:{source_url}"));
        match &self.profile {
            ProfileReply::Live {
                profile_sec_uid,
                web_rid,
                room_id,
            } => Ok(ProfileInspection::Live {
                identity: ProfileIdentity {
                    profile_sec_uid: profile_sec_uid.clone(),
                    display_name: Some("主页昵称".to_owned()),
                },
                room: ProfileRoom {
                    web_rid: web_rid.clone(),
                    room_url: format!("https://live.douyin.com/{web_rid}"),
                    room_id: Some(room_id.clone()),
                },
            }),
            ProfileReply::Offline { profile_sec_uid } => Ok(ProfileInspection::Offline {
                identity: ProfileIdentity {
                    profile_sec_uid: profile_sec_uid.clone(),
                    display_name: Some("主页昵称".to_owned()),
                },
            }),
            ProfileReply::Unsupported => Err(RecorderError::UnsupportedPageLayout),
        }
    }

    async fn inspect_room(&self, source_url: &str) -> RecorderResult<RoomInspection> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("room:{source_url}"));
        Ok(RoomInspection::Live(RoomStreams {
            room_id: "direct-room-id".to_owned(),
            status: Some(2),
            default_quality: None,
            variants: Vec::new(),
        }))
    }
}

#[derive(Clone, Default)]
struct FakeWorker {
    calls: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl WorkerControl for FakeWorker {
    async fn start(&self, streamer_id: i64) -> Result<(), String> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("start:{streamer_id}"));
        Ok(())
    }

    async fn stop(&self, streamer_id: i64) -> Result<(), String> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("stop:{streamer_id}"));
        Ok(())
    }
}

fn profile_request(profile_sec_uid: &str, name: &str) -> CreateStreamerRequest {
    CreateStreamerRequest {
        name: name.to_owned(),
        source_url: format!("https://www.douyin.com/user/{profile_sec_uid}"),
        monitor_enabled: true,
    }
}

#[tokio::test]
async fn create_live_profile_persists_three_identity_layers_and_starts_worker() {
    let database = Database::open_in_memory().unwrap();
    database.migrate().unwrap();
    let inspector = FakeInspector::live();
    let worker = FakeWorker::default();

    let streamer = create_streamer_with(
        &database,
        &inspector,
        &worker,
        profile_request(PROFILE_UID, ""),
    )
    .await
    .unwrap();

    assert_eq!(streamer.name, "主页昵称");
    assert_eq!(streamer.source_kind, StreamerSourceKind::Profile);
    assert_eq!(streamer.profile_sec_uid.as_deref(), Some(PROFILE_UID));
    assert_eq!(streamer.web_rid.as_deref(), Some("236150550962"));
    assert_eq!(
        streamer.room_url.as_deref(),
        Some("https://live.douyin.com/236150550962")
    );
    assert_eq!(streamer.room_id.as_deref(), Some("7664620130978581282"));
    assert_eq!(
        worker.calls.lock().unwrap().as_slice(),
        [format!("start:{}", streamer.id)]
    );
}

#[tokio::test]
async fn create_offline_profile_uses_nickname_and_saves_nullable_room_fields() {
    let database = Database::open_in_memory().unwrap();
    database.migrate().unwrap();

    let streamer = create_streamer_with(
        &database,
        &FakeInspector::offline(),
        &FakeWorker::default(),
        profile_request(PROFILE_UID, ""),
    )
    .await
    .unwrap();

    assert_eq!(streamer.name, "主页昵称");
    assert_eq!(streamer.web_rid, None);
    assert_eq!(streamer.room_url, None);
    assert_eq!(streamer.room_id, None);
    assert_eq!(streamer.monitor_status, "waiting_first_live");
}

#[tokio::test]
async fn direct_room_still_requires_a_name() {
    let database = Database::open_in_memory().unwrap();
    database.migrate().unwrap();
    let request = CreateStreamerRequest {
        name: "".to_owned(),
        source_url: "https://live.douyin.com/123".to_owned(),
        monitor_enabled: true,
    };

    let error = create_streamer_with(
        &database,
        &FakeInspector::live(),
        &FakeWorker::default(),
        request,
    )
    .await
    .unwrap_err();

    assert_eq!(error.code, "name_required");
    assert_eq!(error.field.as_deref(), Some("name"));
}

#[tokio::test]
async fn malformed_profile_path_has_a_distinct_field_error_code() {
    let database = Database::open_in_memory().unwrap();
    database.migrate().unwrap();
    let request = CreateStreamerRequest {
        name: "主页主播".to_owned(),
        source_url: "https://www.douyin.com/user/?token=profile-secret".to_owned(),
        monitor_enabled: true,
    };

    let error = create_streamer_with(
        &database,
        &FakeInspector::live(),
        &FakeWorker::default(),
        request,
    )
    .await
    .unwrap_err();

    assert_eq!(error.code, "invalid_profile_url");
    assert_eq!(error.field.as_deref(), Some("sourceUrl"));
    assert!(!error.message.contains("profile-secret"));
}

#[tokio::test]
async fn rename_only_preserves_identity_without_stopping_worker_or_rechecking_source() {
    let database = Database::open_in_memory().unwrap();
    database.migrate().unwrap();
    let current = database
        .add_streamer(&NewStreamer::room("原名称", "880", "room-880", true))
        .unwrap();
    let inspector = FakeInspector::unsupported("unused");
    inspector.calls.lock().unwrap().clear();
    let worker = FakeWorker::default();

    let updated = update_streamer_with(
        &database,
        &inspector,
        &worker,
        current.id,
        CreateStreamerRequest {
            name: "新名称".to_owned(),
            source_url: current.source_url.clone(),
            monitor_enabled: true,
        },
    )
    .await
    .unwrap();

    assert_eq!(updated.name, "新名称");
    assert_eq!(updated.web_rid, current.web_rid);
    assert_eq!(updated.room_id, current.room_id);
    assert!(inspector.calls.lock().unwrap().is_empty());
    assert!(worker.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn source_conflict_restores_original_worker_and_record() {
    let database = Database::open_in_memory().unwrap();
    database.migrate().unwrap();
    let current = database
        .add_streamer(&NewStreamer::room("原主播", "881", "room-881", true))
        .unwrap();
    database
        .add_streamer(&NewStreamer {
            name: "目标主页".to_owned(),
            source_kind: StreamerSourceKind::Profile,
            source_url: "https://www.douyin.com/user/profile-conflict".to_owned(),
            profile_sec_uid: Some("profile-conflict".to_owned()),
            web_rid: None,
            room_url: None,
            room_id: None,
            monitor_enabled: false,
        })
        .unwrap();
    let inspector = FakeInspector {
        profile: ProfileReply::Offline {
            profile_sec_uid: "profile-conflict".to_owned(),
        },
        calls: Arc::new(Mutex::new(Vec::new())),
    };
    let worker = FakeWorker::default();

    let error = update_streamer_with(
        &database,
        &inspector,
        &worker,
        current.id,
        profile_request("profile-conflict", "新来源"),
    )
    .await
    .unwrap_err();

    assert_eq!(error.code, "duplicate_profile");
    assert_eq!(database.get_streamer(current.id).unwrap(), current);
    assert_eq!(
        worker.calls.lock().unwrap().as_slice(),
        [
            format!("stop:{}", current.id),
            format!("start:{}", current.id)
        ]
    );
}

#[tokio::test]
async fn source_validation_failure_restores_original_worker() {
    let database = Database::open_in_memory().unwrap();
    database.migrate().unwrap();
    let current = database
        .add_streamer(&NewStreamer::room("原主播", "882", "room-882", true))
        .unwrap();
    let worker = FakeWorker::default();

    let error = update_streamer_with(
        &database,
        &FakeInspector::unsupported("profile-layout"),
        &worker,
        current.id,
        profile_request("profile-layout", "新来源"),
    )
    .await
    .unwrap_err();

    assert_eq!(error.code, "profile_layout_changed");
    assert_eq!(database.get_streamer(current.id).unwrap(), current);
    assert_eq!(
        worker.calls.lock().unwrap().as_slice(),
        [
            format!("stop:{}", current.id),
            format!("start:{}", current.id)
        ]
    );
}

#[tokio::test]
async fn source_change_to_offline_profile_replaces_identity_and_restarts_discovery_worker() {
    let database = Database::open_in_memory().unwrap();
    database.migrate().unwrap();
    let current = database
        .add_streamer(&NewStreamer::room("原主播", "883", "room-883", true))
        .unwrap();
    let worker = FakeWorker::default();

    let updated = update_streamer_with(
        &database,
        &FakeInspector::offline(),
        &worker,
        current.id,
        profile_request(PROFILE_UID, "新主页"),
    )
    .await
    .unwrap();

    assert_eq!(updated.source_kind, StreamerSourceKind::Profile);
    assert_eq!(updated.profile_sec_uid.as_deref(), Some(PROFILE_UID));
    assert_eq!(updated.web_rid, None);
    assert_eq!(updated.room_url, None);
    assert_eq!(updated.room_id, None);
    assert_eq!(updated.monitor_status, "waiting_first_live");
    assert_eq!(
        worker.calls.lock().unwrap().as_slice(),
        [
            format!("stop:{}", current.id),
            format!("start:{}", current.id)
        ]
    );
}

#[tokio::test]
async fn duplicate_profile_returns_existing_streamer_id_without_starting_second_worker() {
    let database = Database::open_in_memory().unwrap();
    database.migrate().unwrap();
    let existing = database
        .add_streamer(&NewStreamer {
            name: "已有主页".to_owned(),
            source_kind: StreamerSourceKind::Profile,
            source_url: format!("https://www.douyin.com/user/{PROFILE_UID}"),
            profile_sec_uid: Some(PROFILE_UID.to_owned()),
            web_rid: None,
            room_url: None,
            room_id: None,
            monitor_enabled: false,
        })
        .unwrap();
    let worker = FakeWorker::default();

    let error = create_streamer_with(
        &database,
        &FakeInspector::offline(),
        &worker,
        profile_request(PROFILE_UID, "重复主页"),
    )
    .await
    .unwrap_err();

    assert_eq!(error.code, "duplicate_profile");
    assert_eq!(error.existing_streamer_id, Some(existing.id));
    assert!(worker.calls.lock().unwrap().is_empty());
    assert_eq!(database.list_streamers(true).unwrap().len(), 1);
}
