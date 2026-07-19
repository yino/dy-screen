use dy_screen_app_lib::database::Database;
use dy_screen_app_lib::domain::{NewStreamer, NewVideo, VideoFilter};
use tempfile::tempdir;

#[test]
fn short_sqlite_write_lock_is_retried_instead_of_failing_immediately() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("busy.sqlite3");
    let database = Database::open(&path).unwrap();
    database.migrate().unwrap();

    let blocker = rusqlite::Connection::open(&path).unwrap();
    blocker.execute_batch("BEGIN IMMEDIATE").unwrap();
    let writer = std::thread::spawn({
        let database = database.clone();
        move || {
            database.add_streamer(&NewStreamer {
                name: "锁等待主播".to_owned(),
                room_url: "https://live.douyin.com/600".to_owned(),
                room_id: "room-600".to_owned(),
                monitor_enabled: true,
            })
        }
    });

    std::thread::sleep(std::time::Duration::from_millis(100));
    blocker.execute_batch("COMMIT").unwrap();
    let streamer = writer
        .join()
        .expect("writer thread")
        .expect("short write lock should be retried");
    assert_eq!(streamer.room_id, "room-600");
}

#[test]
fn migration_is_idempotent_and_creates_defaults() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("client.sqlite3");
    let database = Database::open(&path).unwrap();
    database.migrate().unwrap();
    database.migrate().unwrap();

    let settings = database.get_settings().unwrap();
    assert_eq!(settings.quality, "HD1");
    assert_eq!(settings.protocol, "flv");
    assert_eq!(settings.segment_seconds, 900);
    assert_eq!(settings.max_concurrent_recordings, 4);
    assert!(settings.output_root.ends_with("Downloads/dy-screen"));
}

#[test]
fn duplicate_room_id_is_rejected() {
    let database = Database::open_in_memory().unwrap();
    database.migrate().unwrap();
    let input = NewStreamer {
        name: "主播 A".to_owned(),
        room_url: "https://live.douyin.com/100".to_owned(),
        room_id: "room-100".to_owned(),
        monitor_enabled: true,
    };
    database.add_streamer(&input).unwrap();

    let error = database
        .add_streamer(&NewStreamer {
            name: "主播 B".to_owned(),
            ..input
        })
        .unwrap_err();
    assert!(error.to_string().contains("已经存在"));
}

#[test]
fn monitor_switch_is_persisted() {
    let database = Database::open_in_memory().unwrap();
    database.migrate().unwrap();
    let streamer = database
        .add_streamer(&NewStreamer {
            name: "主播".to_owned(),
            room_url: "https://live.douyin.com/100".to_owned(),
            room_id: "room-100".to_owned(),
            monitor_enabled: true,
        })
        .unwrap();

    database.set_monitor_enabled(streamer.id, false).unwrap();
    let saved = database.list_streamers(false).unwrap();
    assert!(!saved[0].monitor_enabled);
    assert_eq!(saved[0].monitor_status, "paused");
}

#[test]
fn streamer_profile_can_be_edited_and_still_enforces_unique_room() {
    let database = Database::open_in_memory().unwrap();
    database.migrate().unwrap();
    let first = database
        .add_streamer(&NewStreamer {
            name: "主播 A".to_owned(),
            room_url: "https://live.douyin.com/100".to_owned(),
            room_id: "room-100".to_owned(),
            monitor_enabled: true,
        })
        .unwrap();
    database
        .add_streamer(&NewStreamer {
            name: "主播 B".to_owned(),
            room_url: "https://live.douyin.com/200".to_owned(),
            room_id: "room-200".to_owned(),
            monitor_enabled: true,
        })
        .unwrap();

    let updated = database
        .update_streamer(
            first.id,
            &NewStreamer {
                name: "主播 A（已修改）".to_owned(),
                room_url: "https://live.douyin.com/101".to_owned(),
                room_id: "room-101".to_owned(),
                monitor_enabled: false,
            },
        )
        .unwrap();
    assert_eq!(updated.name, "主播 A（已修改）");
    assert_eq!(updated.room_id, "room-101");
    assert!(!updated.monitor_enabled);

    let error = database
        .update_streamer(
            first.id,
            &NewStreamer {
                name: "重复".to_owned(),
                room_url: "https://live.douyin.com/200".to_owned(),
                room_id: "room-200".to_owned(),
                monitor_enabled: true,
            },
        )
        .unwrap_err();
    assert!(error.to_string().contains("已经存在"));
}

#[test]
fn archived_streamer_can_be_restored_instead_of_becoming_permanently_blocked() {
    let database = Database::open_in_memory().unwrap();
    database.migrate().unwrap();
    let streamer = database
        .add_streamer(&NewStreamer {
            name: "原主播".to_owned(),
            room_url: "https://live.douyin.com/400".to_owned(),
            room_id: "room-400".to_owned(),
            monitor_enabled: true,
        })
        .unwrap();
    database.archive_streamer(streamer.id).unwrap();

    let restored = database
        .restore_streamer(
            streamer.id,
            &NewStreamer {
                name: "恢复主播".to_owned(),
                room_url: "https://live.douyin.com/400".to_owned(),
                room_id: "room-400".to_owned(),
                monitor_enabled: true,
            },
        )
        .unwrap();

    assert!(!restored.archived);
    assert!(restored.monitor_enabled);
    assert_eq!(restored.name, "恢复主播");
}

#[test]
fn video_lookup_and_reconciliation_are_not_limited_to_first_page() {
    let database = Database::open_in_memory().unwrap();
    database.migrate().unwrap();
    let streamer = database
        .add_streamer(&NewStreamer {
            name: "大量视频主播".to_owned(),
            room_url: "https://live.douyin.com/200".to_owned(),
            room_id: "room-200".to_owned(),
            monitor_enabled: true,
        })
        .unwrap();
    let session = database.start_session(streamer.id, "/tmp").unwrap();
    for index in 0..51 {
        database
            .add_video(&NewVideo {
                session_id: session.id,
                path: format!("/tmp/dy-screen-missing-{index}.mkv"),
                started_at: None,
                ended_at: None,
                duration_seconds: None,
                size_bytes: 0,
                audio_present: Some(true),
                status: "complete".to_owned(),
            })
            .unwrap();
    }

    let oldest = database.list_videos(None, 2, 50).unwrap().items[0].clone();
    assert_eq!(database.get_video(oldest.id).unwrap().id, oldest.id);

    database.reconcile_startup().unwrap();
    assert_eq!(database.get_video(oldest.id).unwrap().status, "missing");
}

#[test]
fn startup_reconciliation_registers_manifest_segments_from_interrupted_session() {
    let directory = tempdir().unwrap();
    let database = Database::open_in_memory().unwrap();
    database.migrate().unwrap();
    let streamer = database
        .add_streamer(&NewStreamer {
            name: "恢复主播".to_owned(),
            room_url: "https://live.douyin.com/300".to_owned(),
            room_id: "room-300".to_owned(),
            monitor_enabled: true,
        })
        .unwrap();
    database
        .start_session(streamer.id, directory.path().to_str().unwrap())
        .unwrap();

    let recording_dir = directory.path().join("room-300").join("20260718T120000Z");
    std::fs::create_dir_all(&recording_dir).unwrap();
    let segment = recording_dir.join("20260718-120000.mkv");
    std::fs::write(&segment, b"completed").unwrap();
    std::fs::write(
        recording_dir.join("segments.csv"),
        format!("\"{}\",0,6\n", segment.display()),
    )
    .unwrap();

    database.reconcile_startup().unwrap();

    let videos = database.list_videos(Some(streamer.id), 1, 50).unwrap();
    assert_eq!(videos.items.len(), 1);
    assert_eq!(videos.items[0].path, segment.to_string_lossy());
    assert_eq!(videos.items[0].status, "complete");
}

#[test]
fn reconciliation_registers_missing_manifest_segments_from_completed_session() {
    let directory = tempdir().unwrap();
    let database = Database::open_in_memory().unwrap();
    database.migrate().unwrap();
    let streamer = database
        .add_streamer(&NewStreamer {
            name: "已结束会话主播".to_owned(),
            room_url: "https://live.douyin.com/301".to_owned(),
            room_id: "room-301".to_owned(),
            monitor_enabled: false,
        })
        .unwrap();
    let session = database
        .start_session(streamer.id, directory.path().to_str().unwrap())
        .unwrap();
    let recording_dir = directory.path().join("room-301").join("20260718T120000Z");
    std::fs::create_dir_all(&recording_dir).unwrap();
    let segment = recording_dir.join("20260718-120000.mkv");
    std::fs::write(&segment, b"completed").unwrap();
    std::fs::write(
        recording_dir.join("segments.csv"),
        format!("\"{}\",0,6\n", segment.display()),
    )
    .unwrap();
    database
        .finish_session(session.id, "completed", None)
        .unwrap();

    database.reconcile_startup().unwrap();

    let videos = database.list_session_videos(session.id).unwrap();
    assert_eq!(videos.len(), 1);
    assert_eq!(videos[0].path, segment.to_string_lossy());
}

#[test]
fn targeted_streamer_reconciliation_closes_active_session_and_recovers_segments() {
    let directory = tempdir().unwrap();
    let database = Database::open_in_memory().unwrap();
    database.migrate().unwrap();
    let streamer = database
        .add_streamer(&NewStreamer {
            name: "超时清理主播".to_owned(),
            room_url: "https://live.douyin.com/302".to_owned(),
            room_id: "room-302".to_owned(),
            monitor_enabled: true,
        })
        .unwrap();
    let session = database
        .start_session(streamer.id, directory.path().to_str().unwrap())
        .unwrap();
    let recording_dir = directory.path().join("room-302").join("20260718T120000Z");
    std::fs::create_dir_all(&recording_dir).unwrap();
    let segment = recording_dir.join("20260718-120000.mkv");
    std::fs::write(&segment, b"completed").unwrap();
    std::fs::write(
        recording_dir.join("segments.csv"),
        format!("\"{}\",0,6\n", segment.display()),
    )
    .unwrap();

    database.reconcile_streamer_sessions(streamer.id).unwrap();

    assert!(database.current_session(streamer.id).unwrap().is_none());
    assert_eq!(
        database.get_session(session.id).unwrap().status,
        "interrupted"
    );
    assert_eq!(database.list_session_videos(session.id).unwrap().len(), 1);
}

#[test]
fn video_query_separates_current_session_and_applies_server_filters() {
    let directory = tempdir().unwrap();
    let history_path = directory.path().join("history-normal.mkv");
    let current_path = directory.path().join("current-missing.mkv");
    std::fs::write(&history_path, b"history").unwrap();
    std::fs::write(&current_path, b"current").unwrap();
    let database = Database::open_in_memory().unwrap();
    database.migrate().unwrap();
    let streamer = database
        .add_streamer(&NewStreamer {
            name: "筛选主播".to_owned(),
            room_url: "https://live.douyin.com/500".to_owned(),
            room_id: "room-500".to_owned(),
            monitor_enabled: true,
        })
        .unwrap();
    let history = database.start_session(streamer.id, "/tmp").unwrap();
    database
        .add_video(&NewVideo {
            session_id: history.id,
            path: history_path.to_string_lossy().into_owned(),
            started_at: Some("2026-07-17T10:00:00Z".to_owned()),
            ended_at: None,
            duration_seconds: Some(60),
            size_bytes: 10,
            audio_present: Some(true),
            status: "complete".to_owned(),
        })
        .unwrap();
    database
        .finish_session(history.id, "completed", None)
        .unwrap();
    let current = database.start_session(streamer.id, "/tmp").unwrap();
    database
        .add_video(&NewVideo {
            session_id: current.id,
            path: current_path.to_string_lossy().into_owned(),
            started_at: Some("2026-07-18T10:00:00Z".to_owned()),
            ended_at: None,
            duration_seconds: Some(60),
            size_bytes: 10,
            audio_present: Some(true),
            status: "missing".to_owned(),
        })
        .unwrap();

    let current_page = database
        .query_videos(
            Some(streamer.id),
            1,
            50,
            &VideoFilter {
                current_only: true,
                ..VideoFilter::default()
            },
        )
        .unwrap();
    assert_eq!(current_page.items.len(), 1);
    assert!(current_page.items[0].path.contains("current-missing"));

    let filtered = database
        .query_videos(
            Some(streamer.id),
            1,
            50,
            &VideoFilter {
                status: Some("complete".to_owned()),
                search: Some("history-normal".to_owned()),
                from: Some("2026-07-17T00:00:00Z".to_owned()),
                to: Some("2026-07-17T23:59:59Z".to_owned()),
                current_only: false,
            },
        )
        .unwrap();
    assert_eq!(filtered.items.len(), 1);
    assert!(filtered.items[0].path.contains("history-normal"));
}
