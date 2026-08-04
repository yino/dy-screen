use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use chrono::Utc;
use dy_screen::access::{
    AccessChannel, AccessClassification, AccessDiagnosticEntry, AccessMarkers, AccessNextAction,
    AccessStage,
};
use dy_screen_app_lib::database::Database;
use dy_screen_app_lib::domain::{NewStreamer, RecordingPriorityDirection};
use dy_screen_app_lib::supervisor::{
    DiskDecision, MonitorLogger, MonitorState, NoopPublisher, RecordingLimiter, Supervisor,
    WorkerRuntime, backoff_seconds, decide_disk, recording_session_status, recording_target_ids,
};

struct RejectingWorkerRuntime;

impl WorkerRuntime for RejectingWorkerRuntime {
    fn spawn(
        &self,
        _task: Pin<Box<dyn Future<Output = ()> + Send + 'static>>,
    ) -> Result<tauri::async_runtime::JoinHandle<()>, String> {
        Err("internal runtime detail".to_owned())
    }
}

#[test]
fn error_backoff_caps_at_five_minutes() {
    assert_eq!(backoff_seconds(0), 30);
    assert_eq!(backoff_seconds(1), 60);
    assert_eq!(backoff_seconds(2), 120);
    assert_eq!(backoff_seconds(3), 300);
    assert_eq!(backoff_seconds(10), 300);
}

#[test]
fn live_and_monitor_states_are_independent() {
    let state = MonitorState::waiting();
    assert_eq!(state.live_status, "offline");
    assert_eq!(state.monitor_status, "waiting");

    let paused = state.paused();
    assert_eq!(paused.live_status, "offline");
    assert_eq!(paused.monitor_status, "paused");
}

#[test]
fn disk_policy_warns_blocks_and_stops_at_thresholds() {
    assert_eq!(
        decide_disk(12 * 1024_u64.pow(3), false),
        DiskDecision::Continue
    );
    assert_eq!(decide_disk(9 * 1024_u64.pow(3), false), DiskDecision::Warn);
    assert_eq!(
        decide_disk(1_500 * 1024_u64.pow(2), false),
        DiskDecision::BlockNew
    );
    assert_eq!(
        decide_disk(900 * 1024_u64.pow(2), true),
        DiskDecision::StopActive
    );
}

#[test]
fn recording_session_outcome_distinguishes_cancel_failure_and_success() {
    assert_eq!(recording_session_status(true, false), "cancelled");
    assert_eq!(recording_session_status(false, true), "completed");
    assert_eq!(recording_session_status(false, false), "error");
}

#[test]
fn structured_monitor_log_only_writes_allowlisted_diagnostic_fields() {
    let directory = tempfile::tempdir().unwrap();
    let logger = MonitorLogger::file(directory.path().to_path_buf());

    logger.log_room_check(
        42,
        Some("703?auth_key=fixture-secret&cookie=session-secret"),
        "layout_changed",
        Some(200),
        2,
        Some("2026-07-23T01:02:03Z"),
    );

    let path = std::fs::read_dir(directory.path())
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let log = std::fs::read_to_string(path).unwrap();
    let value: serde_json::Value = serde_json::from_str(log.trim()).unwrap();
    assert_eq!(value["streamerId"], 42);
    assert_eq!(value["classification"], "layout_changed");
    assert_eq!(value["channel"], "monitor");
    assert_eq!(value["stage"], "result");
    assert_eq!(value["httpStatus"], 200);
    assert_eq!(value["failureCount"], 2);
    assert_eq!(value["nextRetryAt"], "2026-07-23T01:02:03Z");
    assert!(value["timestamp"].is_string());
    assert!(value["webRid"].is_null());
    assert!(!log.contains("fixture-secret"));
    assert!(!log.contains("session-secret"));
    assert!(!log.contains("auth_key"));
    assert!(!log.contains("cookie"));
    assert!(!log.contains("roomUrl"));
    assert!(!log.contains("error"));
}

#[test]
fn access_log_sanitizes_untrusted_identifiers_and_header_values() {
    let directory = tempfile::tempdir().unwrap();
    let logger = MonitorLogger::file(directory.path().to_path_buf());
    logger.log_access(&AccessDiagnosticEntry {
        timestamp: Utc::now(),
        streamer_id: Some(7),
        web_rid: Some("703?auth_key=fixture-secret".to_owned()),
        request_id: "probe?cookie=session-secret".to_owned(),
        channel: AccessChannel::Browser,
        stage: AccessStage::Probe,
        classification: AccessClassification::VerificationRequired,
        http_status: None,
        content_type: Some("text/html\nset-cookie: session-secret".to_owned()),
        response_bytes: 6297,
        markers: AccessMarkers {
            access_restricted: true,
            pace_payload: false,
            supported_room: false,
        },
        duration_ms: 15_000,
        failure_count: 3,
        next_action: AccessNextAction::WaitForUser,
        next_retry_at: None,
    });

    let path = std::fs::read_dir(directory.path())
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let log = std::fs::read_to_string(path).unwrap();
    let value: serde_json::Value = serde_json::from_str(log.trim()).unwrap();
    assert_eq!(value["requestId"], "invalid-request");
    assert!(value["webRid"].is_null());
    assert!(value["contentType"].is_null());
    assert_eq!(value["channel"], "browser");
    assert_eq!(value["stage"], "probe");
    assert_eq!(value["nextAction"], "wait_for_user");
    assert!(!log.contains("fixture-secret"));
    assert!(!log.contains("session-secret"));
    assert!(!log.contains("set-cookie"));
}

#[tokio::test]
async fn one_streamer_only_has_one_worker_and_can_be_woken_immediately() {
    let database = Database::open_in_memory().unwrap();
    database.migrate().unwrap();
    let streamer = database
        .add_streamer(&NewStreamer::room("主播", "1", "room-1", true))
        .unwrap();
    let supervisor = Supervisor::new(database, Arc::new(NoopPublisher), 4).unwrap();

    supervisor.start(streamer.id).unwrap();
    supervisor.start(streamer.id).unwrap();
    supervisor.check_now(streamer.id).unwrap();
    supervisor.check_all_now().unwrap();
    assert_eq!(supervisor.worker_count(), 1);

    supervisor.shutdown().await;
}

#[test]
fn synchronous_start_entrypoints_work_without_a_current_tokio_reactor() {
    let database = Database::open_in_memory().unwrap();
    database.migrate().unwrap();
    let first = database
        .add_streamer(&NewStreamer::room("主播一", "11", "room-11", true))
        .unwrap();
    database
        .add_streamer(&NewStreamer::room("主播二", "12", "room-12", true))
        .unwrap();
    let supervisor = Supervisor::new(database, Arc::new(NoopPublisher), 4).unwrap();

    std::thread::spawn(move || {
        assert!(tokio::runtime::Handle::try_current().is_err());
        supervisor.check_now(first.id).unwrap();
        supervisor.check_all_now().unwrap();
        assert_eq!(supervisor.worker_count(), 2);
        tauri::async_runtime::block_on(supervisor.shutdown());
    })
    .join()
    .expect("普通系统线程启动监听不应 panic");
}

#[test]
fn rejected_worker_spawn_leaves_no_worker_and_preserves_monitor_selection() {
    let database = Database::open_in_memory().unwrap();
    database.migrate().unwrap();
    let streamer = database
        .add_streamer(&NewStreamer::room("主播", "13", "room-13", true))
        .unwrap();
    let supervisor = Supervisor::new(database.clone(), Arc::new(NoopPublisher), 4)
        .unwrap()
        .with_worker_runtime(Arc::new(RejectingWorkerRuntime));

    let error = supervisor.start(streamer.id).unwrap_err();
    assert_eq!(error, "监听任务调度失败，请重试");
    assert_eq!(supervisor.worker_count(), 0);
    assert!(database.get_streamer(streamer.id).unwrap().monitor_enabled);
}

#[tokio::test]
async fn recording_limit_can_be_changed_without_restarting_supervisor() {
    let limiter = Arc::new(RecordingLimiter::new(1));
    let first = limiter.acquire().await;
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(10), limiter.acquire())
            .await
            .is_err()
    );

    limiter.set_limit(2);
    let second = tokio::time::timeout(std::time::Duration::from_millis(50), limiter.acquire())
        .await
        .unwrap();
    assert_eq!(limiter.active(), 2);

    drop(first);
    drop(second);
    assert_eq!(limiter.active(), 0);
}

#[test]
fn priority_targets_are_deterministic_and_offline_streamers_do_not_consume_slots() {
    let database = Database::open_in_memory().unwrap();
    database.migrate().unwrap();
    let mut ids = Vec::new();
    for (index, name) in ["a", "b", "c", "d", "e", "f"].into_iter().enumerate() {
        ids.push(
            database
                .add_streamer(&NewStreamer::room(
                    name,
                    (100 + index).to_string(),
                    format!("room-{index}"),
                    true,
                ))
                .unwrap()
                .id,
        );
    }
    let all_live = ids.iter().copied().collect::<HashSet<_>>();
    let prioritized = database.list_streamers_by_recording_priority().unwrap();
    assert_eq!(recording_target_ids(&prioritized, &all_live, 4), ids[..4]);

    database
        .move_recording_priority(ids[4], RecordingPriorityDirection::Up)
        .unwrap();
    let prioritized = database.list_streamers_by_recording_priority().unwrap();
    assert_eq!(
        recording_target_ids(&prioritized, &all_live, 4),
        vec![ids[0], ids[1], ids[2], ids[4]]
    );

    let without_b = all_live
        .iter()
        .copied()
        .filter(|id| *id != ids[1])
        .collect::<HashSet<_>>();
    assert_eq!(
        recording_target_ids(&prioritized, &without_b, 4),
        vec![ids[0], ids[2], ids[4], ids[3]]
    );
    assert_eq!(recording_target_ids(&prioritized, &all_live, 6).len(), 6);
    assert_eq!(
        recording_target_ids(&prioritized, &all_live, 3),
        vec![ids[0], ids[1], ids[2]]
    );
}

#[test]
fn legacy_local_concurrency_setting_cannot_change_supervisor_limit() {
    let database = Database::open_in_memory().unwrap();
    database.migrate().unwrap();
    let mut settings = database.get_settings().unwrap();
    settings.max_concurrent_recordings = 1;
    database.save_settings(&settings).unwrap();

    let supervisor = Supervisor::new(database, Arc::new(NoopPublisher), 4).unwrap();
    assert_eq!(supervisor.max_screen_limit(), 4);
    supervisor.set_server_recording_limit(6);
    assert_eq!(supervisor.max_screen_limit(), 6);
}
