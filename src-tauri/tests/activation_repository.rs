use dy_screen_app_lib::database::{ClientActivationRecord, Database};

#[test]
fn activation_migration_and_repository_are_idempotent() {
    let database = Database::open_in_memory().expect("打开内存数据库");
    database.migrate().expect("首次迁移");
    database.migrate().expect("重复迁移");
    assert!(database.client_activation().unwrap().is_none());

    let record = ClientActivationRecord {
        device_id: "DY-1234567890".to_owned(),
        activate_code: "TEST-CODE-ONLY".to_owned(),
        token: Some("offline-token".to_owned()),
        expire_at: Some(1_800_000_000),
        grace_sec: Some(259_200),
        server_time_offset_sec: 3,
        state: "active".to_owned(),
        allow_custom_api_key: false,
        last_heartbeat_at: Some("2026-07-27T00:00:00Z".to_owned()),
        next_heartbeat_at: Some("2026-07-27T00:01:30Z".to_owned()),
        last_error: None,
        updated_at: "2026-07-27T00:00:00Z".to_owned(),
    };
    database
        .save_client_activation(&record)
        .expect("保存激活记录");
    assert_eq!(database.client_activation().unwrap(), Some(record));

    database.clear_client_activation().expect("清除激活记录");
    assert!(database.client_activation().unwrap().is_none());
}
