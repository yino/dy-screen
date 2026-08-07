use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use dy_screen_app_lib::activation::ActivationService;
use dy_screen_app_lib::activation_secret::{
    ActivationSecretStore, ActivationSecrets, MemoryActivationSecretStore,
    SqliteActivationSecretStore,
};
use dy_screen_app_lib::api::{ApiClient, ApiConfig, PlainJsonCodec};
use dy_screen_app_lib::database::{ClientActivationRecord, Database};
use rusqlite::Connection;

fn test_api() -> ApiClient {
    ApiClient::new(
        ApiConfig {
            base_url: "http://127.0.0.1:9/api".to_owned(),
            timeout: Duration::from_millis(50),
            client_version: "0.3.0".to_owned(),
            platform: "mac".to_owned(),
        },
        Arc::new(PlainJsonCodec),
    )
    .unwrap()
}

fn prepare_v10_database(path: &Path) {
    let database = Database::open(path).unwrap();
    database.migrate().unwrap();
    drop(database);

    let connection = Connection::open(path).unwrap();
    connection
        .execute_batch(
            r#"
            DELETE FROM schema_migrations WHERE version = 19;
            DROP TABLE IF EXISTS client_activation_legacy_v10;
            DROP TABLE client_activation;
            CREATE TABLE client_activation (
                id INTEGER PRIMARY KEY CHECK(id = 1),
                device_id TEXT NOT NULL,
                activate_code TEXT NOT NULL,
                token TEXT,
                expire_at INTEGER,
                grace_sec INTEGER,
                server_time_offset_sec INTEGER NOT NULL DEFAULT 0,
                state TEXT NOT NULL,
                allow_custom_api_key INTEGER NOT NULL DEFAULT 0 CHECK(allow_custom_api_key IN (0, 1)),
                last_heartbeat_at TEXT,
                next_heartbeat_at TEXT,
                last_error TEXT,
                updated_at TEXT NOT NULL
            );
            INSERT INTO client_activation(
                id, device_id, activate_code, token, expire_at, grace_sec,
                server_time_offset_sec, state, allow_custom_api_key,
                last_heartbeat_at, next_heartbeat_at, last_error, updated_at
            ) VALUES(
                1, 'DY-LEGACY-DEVICE-12345678', 'TEST-LEGACY-ACTIVATION-CODE',
                'TEST-LEGACY-OFFLINE-TOKEN', 4102444800, 259200, 0, 'active', 0,
                '2026-08-01T00:00:00Z', '2026-08-01T00:01:30Z', NULL,
                '2026-08-01T00:00:00Z'
            );
            "#,
        )
        .unwrap();
}

fn activation_columns(path: &Path) -> Vec<String> {
    let connection = Connection::open(path).unwrap();
    let mut statement = connection
        .prepare("PRAGMA table_info(client_activation)")
        .unwrap();
    statement
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

fn activation_secret_columns(path: &Path) -> Vec<String> {
    let connection = Connection::open(path).unwrap();
    let mut statement = connection
        .prepare("PRAGMA table_info(client_activation_secrets)")
        .unwrap();
    statement
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

#[test]
fn activation_migration_and_repository_are_idempotent() {
    let database = Database::open_in_memory().expect("打开内存数据库");
    database.migrate().expect("首次迁移");
    database.migrate().expect("重复迁移");
    assert!(database.client_activation().unwrap().is_none());

    let record = ClientActivationRecord {
        device_id_hint: "…34567890".to_owned(),
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

#[test]
fn fresh_database_uses_metadata_only_activation_schema() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("fresh.sqlite3");
    let database = Database::open(&path).unwrap();
    database.migrate().unwrap();
    database.migrate().unwrap();
    drop(database);

    let columns = activation_columns(&path);
    assert!(columns.contains(&"device_id_hint".to_owned()));
    for forbidden in ["device_id", "activate_code", "token", "enc_key"] {
        assert!(!columns.iter().any(|column| column == forbidden));
    }
    assert_eq!(
        activation_secret_columns(&path),
        ["id", "device_id", "activate_code", "token", "updated_at"]
    );
    let migration_count: i64 = Connection::open(&path)
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM schema_migrations WHERE version = 20",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(migration_count, 1);
}

#[test]
fn sqlite_secret_store_supports_restart_replace_and_idempotent_delete() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("sqlite-secrets.sqlite3");
    let database = Database::open(&path).unwrap();
    database.migrate().unwrap();
    let store = SqliteActivationSecretStore::new(database.clone());
    let initial = ActivationSecrets {
        device_id: "DY-SQLITE-DEVICE".to_owned(),
        activate_code: "TEST-SQLITE-ACTIVATION-CODE".to_owned(),
        token: Some("TEST-SQLITE-OFFLINE-TOKEN".to_owned()),
    };

    assert!(store.load().unwrap().is_none());
    store.store(&initial).unwrap();
    assert!(store.load().unwrap().as_ref() == Some(&initial));

    drop(store);
    drop(database);
    let reopened = Database::open(&path).unwrap();
    reopened.migrate().unwrap();
    let restarted_store = SqliteActivationSecretStore::new(reopened);
    assert!(restarted_store.load().unwrap().as_ref() == Some(&initial));

    let replacement = ActivationSecrets {
        activate_code: "TEST-REPLACEMENT-CODE".to_owned(),
        token: None,
        ..initial
    };
    restarted_store.store(&replacement).unwrap();
    assert!(restarted_store.load().unwrap().as_ref() == Some(&replacement));
    restarted_store.delete().unwrap();
    restarted_store.delete().unwrap();
    assert!(restarted_store.load().unwrap().is_none());
}

#[test]
fn production_service_migrates_v10_credentials_into_sqlite() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("legacy-to-sqlite.sqlite3");
    prepare_v10_database(&path);
    let database = Database::open(&path).unwrap();
    database.migrate().unwrap();

    let service = ActivationService::new(database.clone(), test_api(), temporary.path()).unwrap();
    assert!(service.state().active);
    let migrated = SqliteActivationSecretStore::new(database.clone())
        .load()
        .unwrap();
    assert!(
        migrated.as_ref()
            == Some(&ActivationSecrets {
                device_id: "DY-LEGACY-DEVICE-12345678".to_owned(),
                activate_code: "TEST-LEGACY-ACTIVATION-CODE".to_owned(),
                token: Some("TEST-LEGACY-OFFLINE-TOKEN".to_owned()),
            })
    );
    assert!(database.legacy_client_activation().unwrap().is_none());

    drop(service);
    let restarted = ActivationService::new(database, test_api(), temporary.path()).unwrap();
    assert!(restarted.state().active);
}

#[test]
fn legacy_secrets_are_cleared_only_after_secure_store_round_trip() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("legacy.sqlite3");
    prepare_v10_database(&path);
    let database = Database::open(&path).unwrap();
    database.migrate().unwrap();
    assert!(database.legacy_client_activation().unwrap().is_some());

    let store = MemoryActivationSecretStore::new();
    let service = ActivationService::new_with_secret_store(
        database.clone(),
        test_api(),
        temporary.path(),
        Arc::new(store.clone()),
    )
    .unwrap();

    assert!(service.state().active);
    let expected = ActivationSecrets {
        device_id: "DY-LEGACY-DEVICE-12345678".to_owned(),
        activate_code: "TEST-LEGACY-ACTIVATION-CODE".to_owned(),
        token: Some("TEST-LEGACY-OFFLINE-TOKEN".to_owned()),
    };
    assert!(store.load().unwrap().as_ref() == Some(&expected));
    assert!(database.legacy_client_activation().unwrap().is_none());
    drop(service);
    drop(database);

    for forbidden in ["device_id", "activate_code", "token", "enc_key"] {
        assert!(
            !activation_columns(&path)
                .iter()
                .any(|column| column == forbidden)
        );
    }
}

#[test]
fn interrupted_legacy_migration_preserves_the_only_credentials_and_recovers() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("interrupted.sqlite3");
    prepare_v10_database(&path);
    let database = Database::open(&path).unwrap();
    database.migrate().unwrap();
    let store = MemoryActivationSecretStore::new();
    store.fail_store(true);

    let blocked = ActivationService::new_with_secret_store(
        database.clone(),
        test_api(),
        temporary.path(),
        Arc::new(store.clone()),
    )
    .unwrap();
    assert!(!blocked.state().active);
    assert!(blocked.state().message.unwrap().contains("无法迁移"));
    assert!(database.legacy_client_activation().unwrap().is_some());
    assert!(store.load().unwrap().is_none());
    drop(blocked);

    store.fail_store(false);
    let recovered = ActivationService::new_with_secret_store(
        database.clone(),
        test_api(),
        temporary.path(),
        Arc::new(store.clone()),
    )
    .unwrap();
    assert!(store.load().unwrap().is_some());
    assert!(database.legacy_client_activation().unwrap().is_none());
    assert!(
        !recovered.state().active,
        "失败恢复后仍需服务端重新确认授权"
    );
}

#[test]
fn secure_store_read_failure_keeps_the_legacy_record_and_gate_closed() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("load-failure.sqlite3");
    prepare_v10_database(&path);
    let database = Database::open(&path).unwrap();
    database.migrate().unwrap();
    let store = MemoryActivationSecretStore::new();
    store.fail_load(true);

    let service = ActivationService::new_with_secret_store(
        database.clone(),
        test_api(),
        temporary.path(),
        Arc::new(store),
    )
    .unwrap();
    assert!(!service.state().active);
    assert!(
        service
            .state()
            .message
            .unwrap()
            .contains("无法读取本地激活凭据")
    );
    assert!(database.legacy_client_activation().unwrap().is_some());
}

#[tokio::test]
async fn failed_secret_delete_remains_gated_and_can_be_retried_idempotently() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("delete-failure.sqlite3");
    prepare_v10_database(&path);
    let database = Database::open(&path).unwrap();
    database.migrate().unwrap();
    let store = MemoryActivationSecretStore::new();
    let service = ActivationService::new_with_secret_store(
        database,
        test_api(),
        temporary.path(),
        Arc::new(store.clone()),
    )
    .unwrap();
    assert!(service.state().active);

    store.fail_delete(true);
    let blocked = service.clear().await.unwrap();
    assert!(!blocked.active);
    assert!(blocked.message.unwrap().contains("无法清除本地激活凭据"));
    store.fail_delete(false);
    assert!(store.load().unwrap().is_some());

    assert!(!service.clear().await.unwrap().active);
    assert!(!service.clear().await.unwrap().active);
    assert!(store.load().unwrap().is_none());
}
