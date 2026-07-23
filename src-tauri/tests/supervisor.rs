use std::sync::Arc;

use dy_screen_app_lib::database::Database;
use dy_screen_app_lib::domain::NewStreamer;
use dy_screen_app_lib::supervisor::{
    DiskDecision, MonitorLogger, MonitorState, NoopPublisher, RecordingLimiter, Supervisor,
    backoff_seconds, decide_disk, recording_session_status,
};

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
