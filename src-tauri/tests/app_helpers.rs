use dy_screen::error::RecorderError;
use dy_screen::model::RoomStreams;
use dy_screen::resolver::RoomInspection;
use dy_screen_app_lib::app_support::{
    NormalizedStreamerSource, delete_recording_session, parse_room_identity, parse_streamer_source,
    validate_room_access, validate_settings,
};
use dy_screen_app_lib::database::Database;
use dy_screen_app_lib::domain::{AppSettings, NewStreamer, NewVideo};

#[test]
fn public_douyin_url_is_normalized_and_room_key_is_extracted() {
    let (url, room_key) =
        parse_room_identity("https://live.douyin.com/452086788686?anchor_id=123#fragment").unwrap();
    assert_eq!(url, "https://live.douyin.com/452086788686");
    assert_eq!(room_key, "452086788686");
}

#[test]
fn unsupported_host_is_rejected_without_exposing_query_values() {
    let error = parse_room_identity("https://example.com/1?signature=secret").unwrap_err();
    assert!(error.contains("抖音公开直播间"));
    assert!(!error.contains("secret"));
}

#[test]
fn source_url_classification_accepts_profiles_and_rooms() {
    let profile =
        parse_streamer_source("https://douyin.com/user/profile-sec-uid?from=share#works").unwrap();
    assert_eq!(
        profile,
        NormalizedStreamerSource::Profile {
            source_url: "https://www.douyin.com/user/profile-sec-uid".to_owned(),
            profile_sec_uid: "profile-sec-uid".to_owned(),
        }
    );

    let room = parse_streamer_source("https://live.douyin.com/452086788686?anchor_id=123#fragment")
        .unwrap();
    assert_eq!(
        room,
        NormalizedStreamerSource::Room {
            source_url: "https://live.douyin.com/452086788686".to_owned(),
            web_rid: "452086788686".to_owned(),
        }
    );
}

#[test]
fn unsupported_source_url_is_rejected_without_exposing_tracking_values() {
    let error =
        parse_streamer_source("https://example.com/user/demo?token=source-secret").unwrap_err();
    assert!(error.contains("个人主页或直播间"));
    assert!(!error.contains("source-secret"));
}

#[test]
fn recording_settings_reject_unsafe_values() {
    let mut settings = AppSettings::defaults();
    settings.segment_seconds = 10;
    assert!(
        validate_settings(&settings)
            .unwrap_err()
            .contains("分片时长")
    );

    settings.segment_seconds = 900;
    settings.max_concurrent_recordings = 0;
    assert!(validate_settings(&settings).unwrap_err().contains("并发"));
}

#[test]
fn room_access_accepts_live_or_offline_rooms_and_rejects_unknown_pages() {
    let room_id = validate_room_access(Ok(RoomInspection::Live(RoomStreams {
        room_id: "room-1".to_owned(),
        status: Some(2),
        default_quality: None,
        variants: Vec::new(),
    })))
    .unwrap();
    assert_eq!(room_id, "room-1");
    assert_eq!(
        validate_room_access(Ok(RoomInspection::Offline {
            room_id: "offline-room".to_owned(),
        }))
        .unwrap(),
        "offline-room"
    );

    let error = validate_room_access(Err(RecorderError::UnsupportedPageLayout)).unwrap_err();
    assert!(error.contains("无法识别"));
}

#[test]
fn deleting_a_session_removes_all_files_and_database_records() {
    let directory = tempfile::tempdir().unwrap();
    let video_path = directory.path().join("segment.mkv");
    std::fs::write(&video_path, b"video").unwrap();
    let (database, session_id) = session_with_video(&video_path);

    delete_recording_session(&database, session_id).unwrap();

    assert!(!video_path.exists());
    assert!(database.get_session(session_id).is_err());
}

#[test]
fn deleting_a_session_restores_video_status_when_file_removal_fails() {
    let directory = tempfile::tempdir().unwrap();
    let invalid_video_path = directory.path().join("not-a-file.mkv");
    std::fs::create_dir(&invalid_video_path).unwrap();
    let (database, session_id) = session_with_video(&invalid_video_path);

    let error = delete_recording_session(&database, session_id).unwrap_err();

    assert!(error.contains("删除录制会话失败"));
    assert_eq!(
        database.get_session(session_id).unwrap().status,
        "completed"
    );
    let videos = database.list_session_videos(session_id).unwrap();
    assert_eq!(videos.len(), 1);
    assert_eq!(videos[0].status, "complete");
}

#[test]
fn deleting_a_session_restores_earlier_files_when_a_later_file_fails() {
    let directory = tempfile::tempdir().unwrap();
    let first_path = directory.path().join("first.mkv");
    let invalid_second_path = directory.path().join("second.mkv");
    std::fs::write(&first_path, b"first").unwrap();
    std::fs::create_dir(&invalid_second_path).unwrap();
    let database = Database::open_in_memory().unwrap();
    database.migrate().unwrap();
    let streamer = database
        .add_streamer(&NewStreamer::room("回滚主播", "701", "room-701", false))
        .unwrap();
    let session = database.start_session(streamer.id, "/tmp").unwrap();
    for path in [&first_path, &invalid_second_path] {
        database
            .add_video(&NewVideo {
                session_id: session.id,
                path: path.to_string_lossy().into_owned(),
                started_at: None,
                ended_at: None,
                duration_seconds: Some(60),
                size_bytes: 5,
                audio_present: Some(true),
                status: "complete".to_owned(),
            })
            .unwrap();
    }
    database
        .finish_session(session.id, "completed", None)
        .unwrap();

    delete_recording_session(&database, session.id).unwrap_err();

    assert!(first_path.is_file(), "earlier file must be restored");
    assert_eq!(database.list_session_videos(session.id).unwrap().len(), 2);
}

fn session_with_video(path: &std::path::Path) -> (Database, i64) {
    let database = Database::open_in_memory().unwrap();
    database.migrate().unwrap();
    let streamer = database
        .add_streamer(&NewStreamer::room("会话主播", "700", "room-700", false))
        .unwrap();
    let session = database.start_session(streamer.id, "/tmp").unwrap();
    database
        .add_video(&NewVideo {
            session_id: session.id,
            path: path.to_string_lossy().into_owned(),
            started_at: None,
            ended_at: None,
            duration_seconds: Some(60),
            size_bytes: 5,
            audio_present: Some(true),
            status: "complete".to_owned(),
        })
        .unwrap();
    database
        .finish_session(session.id, "completed", None)
        .unwrap();
    (database, session.id)
}
