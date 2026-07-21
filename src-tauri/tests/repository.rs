use dy_screen_app_lib::database::Database;
use dy_screen_app_lib::domain::{
    DiscoveryBinding, NewStreamer, NewVideo, StreamerSourceKind, VideoFilter,
};
use rusqlite::{Connection, OptionalExtension, params};
use tempfile::tempdir;

fn create_legacy_database(path: &std::path::Path) {
    let connection = Connection::open(path).unwrap();
    connection
        .execute_batch(
            r#"
            PRAGMA foreign_keys = ON;
            CREATE TABLE schema_migrations (
                version INTEGER PRIMARY KEY,
                applied_at TEXT NOT NULL
            );
            INSERT INTO schema_migrations(version, applied_at) VALUES(1, '2026-07-18T00:00:00Z');

            CREATE TABLE streamers (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL,
                room_url TEXT NOT NULL,
                room_id TEXT NOT NULL UNIQUE,
                monitor_enabled INTEGER NOT NULL DEFAULT 1,
                archived INTEGER NOT NULL DEFAULT 0,
                live_status TEXT NOT NULL DEFAULT 'checking',
                monitor_status TEXT NOT NULL DEFAULT 'waiting',
                last_checked_at TEXT,
                last_error TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            CREATE TABLE recording_sessions (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                streamer_id INTEGER NOT NULL REFERENCES streamers(id),
                started_at TEXT NOT NULL,
                ended_at TEXT,
                status TEXT NOT NULL,
                retry_count INTEGER NOT NULL DEFAULT 0,
                output_root TEXT NOT NULL,
                error TEXT
            );
            CREATE TABLE videos (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id INTEGER NOT NULL REFERENCES recording_sessions(id),
                path TEXT NOT NULL UNIQUE,
                started_at TEXT,
                ended_at TEXT,
                duration_seconds INTEGER,
                size_bytes INTEGER NOT NULL DEFAULT 0,
                audio_present INTEGER,
                status TEXT NOT NULL DEFAULT 'complete',
                created_at TEXT NOT NULL
            );
            CREATE TABLE settings (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );

            INSERT INTO streamers(
                id, name, room_url, room_id, monitor_enabled, archived,
                live_status, monitor_status, last_checked_at, last_error,
                created_at, updated_at
            ) VALUES
                (11, '旧主播', 'https://live.douyin.com/236150550962', '7664620130978581282', 1, 0, 'offline', 'waiting', '2026-07-18T01:00:00Z', NULL, '2026-07-18T00:00:00Z', '2026-07-18T01:00:00Z'),
                (12, '异常旧主播', 'https://legacy.example/not-normalized', 'legacy-room-id', 0, 1, 'error', 'paused', NULL, NULL, '2026-07-18T00:00:00Z', '2026-07-18T00:00:00Z');
            INSERT INTO recording_sessions(
                id, streamer_id, started_at, ended_at, status, retry_count, output_root, error
            ) VALUES(21, 11, '2026-07-18T01:00:00Z', '2026-07-18T02:00:00Z', 'completed', 0, '/tmp/legacy', NULL);
            INSERT INTO videos(
                id, session_id, path, size_bytes, status, created_at
            ) VALUES(31, 21, '/tmp/legacy/segment.mkv', 1024, 'complete', '2026-07-18T01:00:00Z');
            "#,
        )
        .unwrap();
}

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
        move || database.add_streamer(&NewStreamer::room("锁等待主播", "600", "room-600", true))
    });

    std::thread::sleep(std::time::Duration::from_millis(100));
    blocker.execute_batch("COMMIT").unwrap();
    let streamer = writer
        .join()
        .expect("writer thread")
        .expect("short write lock should be retried");
    assert_eq!(streamer.room_id.as_deref(), Some("room-600"));
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
fn legacy_migration_preserves_streamers_flags_and_history_relations() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("legacy.sqlite3");
    create_legacy_database(&path);

    let database = Database::open(&path).unwrap();
    database.migrate().unwrap();
    database.migrate().unwrap();

    let connection = Connection::open(&path).unwrap();
    let migrated = connection
        .query_row(
            r#"
            SELECT source_kind, source_url, profile_sec_uid, web_rid, room_url,
                   room_id, monitor_enabled, archived, live_status, monitor_status
            FROM streamers WHERE id = 11
            "#,
            [],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, bool>(6)?,
                    row.get::<_, bool>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, String>(9)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(migrated.0, "room");
    assert_eq!(migrated.1, "https://live.douyin.com/236150550962");
    assert_eq!(migrated.2, None);
    assert_eq!(migrated.3.as_deref(), Some("236150550962"));
    assert_eq!(
        migrated.4.as_deref(),
        Some("https://live.douyin.com/236150550962")
    );
    assert_eq!(migrated.5.as_deref(), Some("7664620130978581282"));
    assert!(migrated.6);
    assert!(!migrated.7);
    assert_eq!(migrated.8, "offline");
    assert_eq!(migrated.9, "waiting");

    let relation = connection
        .query_row(
            r#"
            SELECT rs.streamer_id, v.session_id
            FROM recording_sessions rs
            JOIN videos v ON v.session_id = rs.id
            WHERE rs.id = 21 AND v.id = 31
            "#,
            [],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )
        .unwrap();
    assert_eq!(relation, (11, 21));
    assert_eq!(
        connection
            .query_row("PRAGMA foreign_key_check", [], |_| Ok(true))
            .optional()
            .unwrap(),
        None
    );
}

#[test]
fn legacy_migration_preserves_duplicate_room_urls_without_failing_unique_index() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("legacy-duplicate-room-url.sqlite3");
    create_legacy_database(&path);
    let connection = Connection::open(&path).unwrap();
    connection
        .execute_batch(
            r#"
            INSERT INTO streamers(
                id, name, room_url, room_id, monitor_enabled, archived,
                live_status, monitor_status, last_checked_at, last_error,
                created_at, updated_at
            ) VALUES(
                13, '重复入口旧主播', 'https://live.douyin.com/236150550962',
                '7664620130978581999', 1, 0, 'offline', 'waiting', NULL, NULL,
                '2026-07-18T00:00:00Z', '2026-07-18T01:00:00Z'
            );
            INSERT INTO recording_sessions(
                id, streamer_id, started_at, ended_at, status, retry_count, output_root, error
            ) VALUES(
                22, 13, '2026-07-18T03:00:00Z', '2026-07-18T04:00:00Z',
                'completed', 0, '/tmp/legacy-duplicate', NULL
            );
            INSERT INTO videos(
                id, session_id, path, size_bytes, status, created_at
            ) VALUES(
                32, 22, '/tmp/legacy-duplicate/segment.mkv', 2048,
                'complete', '2026-07-18T03:00:00Z'
            );
            "#,
        )
        .unwrap();
    drop(connection);

    let database = Database::open(&path).unwrap();
    database.migrate().unwrap();

    let connection = Connection::open(&path).unwrap();
    let rows = connection
        .prepare(
            r#"
            SELECT id, web_rid, monitor_enabled, monitor_status, last_error
            FROM streamers
            WHERE source_url = 'https://live.douyin.com/236150550962'
            ORDER BY id
            "#,
        )
        .unwrap()
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, bool>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<String>>(4)?,
            ))
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();

    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].0, 11);
    assert_eq!(rows[0].1.as_deref(), Some("236150550962"));
    assert_eq!(rows[1].0, 13);
    assert_eq!(rows[1].1, None);
    assert!(!rows[1].2);
    assert_eq!(rows[1].3, "paused");
    assert!(
        rows[1]
            .4
            .as_deref()
            .is_some_and(|message| message.contains("稳定直播入口冲突"))
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM recording_sessions rs JOIN videos v ON v.session_id = rs.id WHERE rs.streamer_id = 13 AND v.id = 32",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        1
    );
}

#[test]
fn migration_keeps_unrecognized_legacy_room_url_with_diagnostic_state() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("legacy-invalid.sqlite3");
    create_legacy_database(&path);

    let database = Database::open(&path).unwrap();
    database.migrate().unwrap();

    let connection = Connection::open(&path).unwrap();
    let migrated = connection
        .query_row(
            "SELECT source_kind, source_url, web_rid, room_url, room_id, archived, monitor_enabled, last_error FROM streamers WHERE id = 12",
            [],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, bool>(5)?,
                    row.get::<_, bool>(6)?,
                    row.get::<_, Option<String>>(7)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(migrated.0, "room");
    assert_eq!(migrated.1, "https://legacy.example/not-normalized");
    assert_eq!(migrated.2, None);
    assert_eq!(
        migrated.3.as_deref(),
        Some("https://legacy.example/not-normalized")
    );
    assert_eq!(migrated.4.as_deref(), Some("legacy-room-id"));
    assert!(migrated.5);
    assert!(!migrated.6);
    assert!(migrated.7.unwrap().contains("无法提取稳定直播入口"));
}

#[test]
fn migrated_identity_indexes_are_partial_and_room_id_is_not_unique() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("legacy-indexes.sqlite3");
    create_legacy_database(&path);

    let database = Database::open(&path).unwrap();
    database.migrate().unwrap();

    let connection = Connection::open(&path).unwrap();
    connection
        .execute(
            r#"
            INSERT INTO streamers(
                name, source_kind, source_url, profile_sec_uid, web_rid,
                room_url, room_id, monitor_enabled, archived, live_status,
                monitor_status, created_at, updated_at
            ) VALUES(?1, 'profile', ?2, ?3, NULL, NULL, NULL, 1, 0, 'offline', 'waiting_first_live', ?4, ?4)
            "#,
            params![
                "等待主播",
                "https://www.douyin.com/user/profile-a",
                "profile-a",
                "2026-07-18T00:00:00Z"
            ],
        )
        .unwrap();
    let duplicate_profile = connection.execute(
        r#"
        INSERT INTO streamers(
            name, source_kind, source_url, profile_sec_uid, web_rid,
            room_url, room_id, monitor_enabled, archived, live_status,
            monitor_status, created_at, updated_at
        ) VALUES('重复主页', 'profile', 'https://www.douyin.com/user/profile-a', 'profile-a', NULL, NULL, NULL, 1, 0, 'offline', 'waiting_first_live', ?1, ?1)
        "#,
        ["2026-07-18T00:00:00Z"],
    );
    assert!(duplicate_profile.is_err());

    connection
        .execute(
            r#"
            INSERT INTO streamers(
                name, source_kind, source_url, profile_sec_uid, web_rid,
                room_url, room_id, monitor_enabled, archived, live_status,
                monitor_status, created_at, updated_at
            ) VALUES('新周期', 'room', 'https://live.douyin.com/777', NULL, '777', 'https://live.douyin.com/777', '7664620130978581282', 1, 0, 'offline', 'waiting', ?1, ?1)
            "#,
            ["2026-07-18T00:00:00Z"],
        )
        .expect("相同 room_id 的新稳定入口应允许保存");
    let duplicate_web_rid = connection.execute(
        r#"
        INSERT INTO streamers(
            name, source_kind, source_url, profile_sec_uid, web_rid,
            room_url, room_id, monitor_enabled, archived, live_status,
            monitor_status, created_at, updated_at
        ) VALUES('重复入口', 'room', 'https://live.douyin.com/777', NULL, '777', 'https://live.douyin.com/777', 'new-room-id', 1, 0, 'offline', 'waiting', ?1, ?1)
        "#,
        ["2026-07-18T00:00:00Z"],
    );
    assert!(duplicate_web_rid.is_err());
}

#[test]
fn duplicate_stable_web_rid_is_rejected() {
    let database = Database::open_in_memory().unwrap();
    database.migrate().unwrap();
    let input = NewStreamer::room("主播 A", "100", "room-100", true);
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
        .add_streamer(&NewStreamer::room("主播", "100", "room-100", true))
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
        .add_streamer(&NewStreamer::room("主播 A", "100", "room-100", true))
        .unwrap();
    database
        .add_streamer(&NewStreamer::room("主播 B", "200", "room-200", true))
        .unwrap();

    let updated = database
        .update_streamer(
            first.id,
            &NewStreamer::room("主播 A（已修改）", "101", "room-101", false),
        )
        .unwrap();
    assert_eq!(updated.name, "主播 A（已修改）");
    assert_eq!(updated.room_id.as_deref(), Some("room-101"));
    assert!(!updated.monitor_enabled);

    let error = database
        .update_streamer(
            first.id,
            &NewStreamer::room("重复", "200", "room-200-new", true),
        )
        .unwrap_err();
    assert!(error.to_string().contains("已经存在"));
}

#[test]
fn archived_streamer_can_be_restored_instead_of_becoming_permanently_blocked() {
    let database = Database::open_in_memory().unwrap();
    database.migrate().unwrap();
    let streamer = database
        .add_streamer(&NewStreamer::room("原主播", "400", "room-400", true))
        .unwrap();
    database.archive_streamer(streamer.id).unwrap();

    let restored = database
        .restore_streamer(
            streamer.id,
            &NewStreamer::room("恢复主播", "400", "room-400", true),
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
        .add_streamer(&NewStreamer::room("大量视频主播", "200", "room-200", true))
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
        .add_streamer(&NewStreamer::room("恢复主播", "300", "room-300", true))
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
        .add_streamer(&NewStreamer::room(
            "已结束会话主播",
            "301",
            "room-301",
            false,
        ))
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
        .add_streamer(&NewStreamer::room("超时清理主播", "302", "room-302", true))
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
        .add_streamer(&NewStreamer::room("筛选主播", "500", "room-500", true))
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

#[test]
fn profile_streamer_can_wait_without_room_binding_and_be_found_by_identity() {
    let database = Database::open_in_memory().unwrap();
    database.migrate().unwrap();

    let streamer = database
        .add_streamer(&NewStreamer {
            name: "等待开播主播".to_owned(),
            source_kind: StreamerSourceKind::Profile,
            source_url: "https://www.douyin.com/user/profile-waiting".to_owned(),
            profile_sec_uid: Some("profile-waiting".to_owned()),
            web_rid: None,
            room_url: None,
            room_id: None,
            monitor_enabled: true,
        })
        .unwrap();

    assert_eq!(streamer.source_kind, StreamerSourceKind::Profile);
    assert_eq!(streamer.web_rid, None);
    assert_eq!(streamer.room_url, None);
    assert_eq!(streamer.room_id, None);
    assert_eq!(streamer.monitor_status, "waiting_first_live");
    assert_eq!(
        database
            .find_streamer_by_profile_sec_uid("profile-waiting")
            .unwrap()
            .unwrap()
            .id,
        streamer.id
    );
}

#[test]
fn stable_web_rid_is_unique_while_current_room_id_can_change() {
    let database = Database::open_in_memory().unwrap();
    database.migrate().unwrap();
    let streamer = database
        .add_streamer(&NewStreamer {
            name: "直播间主播".to_owned(),
            source_kind: StreamerSourceKind::Room,
            source_url: "https://live.douyin.com/800".to_owned(),
            profile_sec_uid: None,
            web_rid: Some("800".to_owned()),
            room_url: Some("https://live.douyin.com/800".to_owned()),
            room_id: Some("cycle-1".to_owned()),
            monitor_enabled: true,
        })
        .unwrap();

    let bound = database
        .bind_discovered_room(
            streamer.id,
            "800",
            "https://live.douyin.com/800",
            Some("cycle-2"),
        )
        .unwrap();
    let DiscoveryBinding::Bound(updated) = bound else {
        panic!("同一稳定入口应更新当前直播场次");
    };
    assert_eq!(updated.room_id.as_deref(), Some("cycle-2"));
    assert_eq!(
        database
            .find_streamer_by_web_rid("800")
            .unwrap()
            .unwrap()
            .id,
        streamer.id
    );

    let error = database
        .add_streamer(&NewStreamer {
            name: "重复稳定入口".to_owned(),
            source_kind: StreamerSourceKind::Room,
            source_url: "https://live.douyin.com/800".to_owned(),
            profile_sec_uid: None,
            web_rid: Some("800".to_owned()),
            room_url: Some("https://live.douyin.com/800".to_owned()),
            room_id: Some("another-cycle".to_owned()),
            monitor_enabled: true,
        })
        .unwrap_err();
    assert!(error.to_string().contains("稳定直播入口"));
}

#[test]
fn delayed_discovery_merges_history_free_profile_into_existing_room_streamer() {
    let database = Database::open_in_memory().unwrap();
    database.migrate().unwrap();
    let target = database
        .add_streamer(&NewStreamer {
            name: "已有直播间".to_owned(),
            source_kind: StreamerSourceKind::Room,
            source_url: "https://live.douyin.com/900".to_owned(),
            profile_sec_uid: None,
            web_rid: Some("900".to_owned()),
            room_url: Some("https://live.douyin.com/900".to_owned()),
            room_id: Some("room-900".to_owned()),
            monitor_enabled: true,
        })
        .unwrap();
    let session = database.start_session(target.id, "/tmp").unwrap();
    database
        .finish_session(session.id, "completed", None)
        .unwrap();
    let temporary = database
        .add_streamer(&NewStreamer {
            name: "主页主播".to_owned(),
            source_kind: StreamerSourceKind::Profile,
            source_url: "https://www.douyin.com/user/profile-900".to_owned(),
            profile_sec_uid: Some("profile-900".to_owned()),
            web_rid: None,
            room_url: None,
            room_id: None,
            monitor_enabled: true,
        })
        .unwrap();

    let outcome = database
        .bind_discovered_room(
            temporary.id,
            "900",
            "https://live.douyin.com/900",
            Some("room-900-new"),
        )
        .unwrap();
    assert_eq!(
        outcome,
        DiscoveryBinding::Merged {
            target_streamer_id: target.id,
            removed_streamer_id: temporary.id,
        }
    );
    assert!(database.get_streamer(temporary.id).is_err());
    let merged = database.get_streamer(target.id).unwrap();
    assert_eq!(merged.source_kind, StreamerSourceKind::Profile);
    assert_eq!(merged.profile_sec_uid.as_deref(), Some("profile-900"));
    assert_eq!(merged.room_id.as_deref(), Some("room-900-new"));
    assert_eq!(
        database.get_session(session.id).unwrap().streamer_id,
        target.id
    );
}

#[test]
fn delayed_discovery_with_profile_history_pauses_conflict_without_deleting_records() {
    let database = Database::open_in_memory().unwrap();
    database.migrate().unwrap();
    let target = database
        .add_streamer(&NewStreamer {
            name: "已有入口".to_owned(),
            source_kind: StreamerSourceKind::Room,
            source_url: "https://live.douyin.com/901".to_owned(),
            profile_sec_uid: None,
            web_rid: Some("901".to_owned()),
            room_url: Some("https://live.douyin.com/901".to_owned()),
            room_id: Some("room-901".to_owned()),
            monitor_enabled: true,
        })
        .unwrap();
    let temporary = database
        .add_streamer(&NewStreamer {
            name: "有历史主页".to_owned(),
            source_kind: StreamerSourceKind::Profile,
            source_url: "https://www.douyin.com/user/profile-901".to_owned(),
            profile_sec_uid: Some("profile-901".to_owned()),
            web_rid: None,
            room_url: None,
            room_id: None,
            monitor_enabled: true,
        })
        .unwrap();
    let profile_session = database.start_session(temporary.id, "/tmp").unwrap();
    database
        .finish_session(profile_session.id, "completed", None)
        .unwrap();

    let outcome = database
        .bind_discovered_room(
            temporary.id,
            "901",
            "https://live.douyin.com/901",
            Some("room-901-new"),
        )
        .unwrap();
    assert_eq!(
        outcome,
        DiscoveryBinding::Conflict {
            target_streamer_id: target.id,
        }
    );
    let conflicted = database.get_streamer(temporary.id).unwrap();
    assert!(!conflicted.monitor_enabled);
    assert_eq!(conflicted.monitor_status, "identity_conflict");
    assert!(conflicted.last_error.unwrap().contains("重复稳定直播入口"));
    assert_eq!(
        database
            .get_session(profile_session.id)
            .unwrap()
            .streamer_id,
        temporary.id
    );
}
