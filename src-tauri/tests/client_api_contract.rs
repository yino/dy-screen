use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread;
use std::time::Duration;

use dy_screen_app_lib::activation::{ActivationService, HeartbeatOutcome, ServerRecordingLimit};
use dy_screen_app_lib::activation_secret::{ActivationSecretStore, MemoryActivationSecretStore};
use dy_screen_app_lib::api::{
    ApiClient, ApiConfig, ApiError, MemoryApiDiagnosticSink, PlainJsonCodec, TelemetryEvent,
};
use dy_screen_app_lib::database::Database;
use serde_json::{Value, json};

#[derive(Debug)]
struct CapturedRequest {
    request_line: String,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

fn spawn_server(responses: Vec<Value>) -> (String, thread::JoinHandle<Vec<CapturedRequest>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("测试 HTTP 监听器应可绑定");
    let address = listener.local_addr().unwrap();
    let handle = thread::spawn(move || {
        let mut captured = Vec::with_capacity(responses.len());
        for response in responses {
            let (mut stream, _) = listener.accept().expect("API 客户端应连接测试服务");
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request_line = String::new();
            reader.read_line(&mut request_line).unwrap();
            let request_line = request_line.trim_end().to_owned();
            let mut headers = HashMap::new();
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let line = line.trim_end_matches(['\r', '\n']);
                if line.is_empty() {
                    break;
                }
                if let Some((name, value)) = line.split_once(':') {
                    headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_owned());
                }
            }
            let content_length = headers
                .get("content-length")
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or_default();
            let mut body = vec![0; content_length];
            reader.read_exact(&mut body).unwrap();
            captured.push(CapturedRequest {
                request_line,
                headers,
                body,
            });

            let response = serde_json::to_vec(&response).unwrap();
            let head = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                response.len()
            );
            stream.write_all(head.as_bytes()).unwrap();
            stream.write_all(&response).unwrap();
        }
        captured
    });
    (format!("http://{address}/api"), handle)
}

fn spawn_gated_server(
    responses: Vec<Value>,
    gated_response_index: usize,
) -> (
    String,
    Receiver<()>,
    Sender<()>,
    thread::JoinHandle<Vec<CapturedRequest>>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("测试 HTTP 监听器应可绑定");
    let address = listener.local_addr().unwrap();
    let (reached_tx, reached_rx) = channel();
    let (release_tx, release_rx) = channel();
    let handle = thread::spawn(move || {
        let mut captured = Vec::with_capacity(responses.len());
        for (index, response) in responses.into_iter().enumerate() {
            let (mut stream, _) = listener.accept().expect("API 客户端应连接测试服务");
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request_line = String::new();
            reader.read_line(&mut request_line).unwrap();
            let request_line = request_line.trim_end().to_owned();
            let mut headers = HashMap::new();
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let line = line.trim_end_matches(['\r', '\n']);
                if line.is_empty() {
                    break;
                }
                if let Some((name, value)) = line.split_once(':') {
                    headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_owned());
                }
            }
            let content_length = headers
                .get("content-length")
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or_default();
            let mut body = vec![0; content_length];
            reader.read_exact(&mut body).unwrap();
            captured.push(CapturedRequest {
                request_line,
                headers,
                body,
            });

            if index == gated_response_index {
                reached_tx.send(()).unwrap();
                release_rx.recv().unwrap();
            }
            let response = serde_json::to_vec(&response).unwrap();
            let head = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                response.len()
            );
            stream.write_all(head.as_bytes()).unwrap();
            stream.write_all(&response).unwrap();
        }
        captured
    });
    (
        format!("http://{address}/api"),
        reached_rx,
        release_tx,
        handle,
    )
}

fn api_client(base_url: String) -> ApiClient {
    api_client_with_timeout(base_url, Duration::from_secs(3))
}

fn api_client_with_timeout(base_url: String, timeout: Duration) -> ApiClient {
    ApiClient::new(
        ApiConfig {
            base_url,
            timeout,
            client_version: "0.3.0".to_owned(),
            platform: "mac".to_owned(),
        },
        Arc::new(PlainJsonCodec),
    )
    .unwrap()
}

fn service_with_memory_store(
    database: Database,
    client: ApiClient,
    app_data_dir: &std::path::Path,
) -> (ActivationService, MemoryActivationSecretStore) {
    let store = MemoryActivationSecretStore::new();
    let service = ActivationService::new_with_secret_store(
        database,
        client,
        app_data_dir,
        Arc::new(store.clone()),
    )
    .unwrap();
    (service, store)
}

#[tokio::test]
async fn client_api_matches_activation_heartbeat_app_start_and_telemetry_contracts() {
    let responses = vec![
        json!({
            "code": 200,
            "data": {"token": "offline-token", "expireAt": 4_102_444_800_i64, "graceSec": 300, "encKey": "future-key"},
            "msg": "激活成功"
        }),
        json!({
            "code": 201,
            "data": {"revoked": true, "state": "DISABLED", "reason": "disabled", "serverTime": 43, "max_screen_limit": 6, "transitionMaterials": {"catalogVersion": 2, "minimumAppVersion": "0.3.0"}},
            "msg": "强制下线"
        }),
        json!({
            "code": 200,
            "data": {"modelId": "model-a", "modelAsr": "asr-a", "version": "1.0.0", "url": "https://cdn.example/app", "signature": null, "minClientVersion": "0.3.0", "force": false, "max_screen_limit": 8},
            "msg": "success"
        }),
        json!({"code": 200, "data": {"accepted": 1, "rejected": 0}, "msg": "已上报"}),
        json!({
            "code": 200,
            "data": {
                "catalogVersion": 2,
                "changed": true,
                "cdnBaseUrl": "https://cdn.example/materials/",
                "materials": [{
                    "assetKey": "tm_example",
                    "assetVersion": 1,
                    "title": "震惊",
                    "description": "用于意外消息",
                    "tags": ["震惊", "反应"],
                    "category": "neutral",
                    "renderMode": "bridge",
                    "videoPath": "https://cdn.example/assets/example.mp4",
                    "previewPath": null,
                    "coverPath": null,
                    "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    "sizeBytes": 1024,
                    "durationMs": 2000,
                    "width": 1280,
                    "height": 720,
                    "fps": 30,
                    "videoCodec": "h264",
                    "hasAudio": true,
                    "sortOrder": 1
                }]
            },
            "msg": "success"
        }),
        json!({
            "code": 200,
            "data": {
                "catalogVersion": 2,
                "changed": false,
                "cdnBaseUrl": "https://cdn.example/materials/"
            },
            "msg": "success"
        }),
    ];
    let (base_url, server) = spawn_server(responses);
    let client = api_client(base_url);

    let activation = client.activate("DY-DEVICE", "ACTIVATE-CODE").await.unwrap();
    assert_eq!(activation.token, "offline-token");
    let heartbeat = client
        .heartbeat("DY-DEVICE", "ACTIVATE-CODE")
        .await
        .unwrap();
    assert!(heartbeat.revoked);
    assert_eq!(heartbeat.max_screen_limit, Some(6));
    assert_eq!(
        heartbeat.transition_materials.unwrap().minimum_app_version,
        "0.3.0"
    );
    let app_start = client.app_start().await.unwrap().unwrap();
    assert_eq!(app_start.model_id, "model-a");
    assert_eq!(app_start.max_screen_limit, Some(8));
    let event = TelemetryEvent::new(
        "feature_use",
        json!({"feature": "monitor", "localPath": "/private/video.mkv"})
            .as_object()
            .unwrap()
            .clone(),
        "0.3.0",
    )
    .unwrap();
    let telemetry = client
        .telemetry(Some("DY-DEVICE"), Some("ACTIVATE-CODE"), &[event])
        .await
        .unwrap();
    assert_eq!(telemetry.accepted, 1);
    let changed = client
        .transition_materials("DY-DEVICE", "ACTIVATE-CODE", 1)
        .await
        .unwrap();
    assert!(changed.changed);
    assert_eq!(changed.materials.unwrap().len(), 1);
    let unchanged = client
        .transition_materials("DY-DEVICE", "ACTIVATE-CODE", 2)
        .await
        .unwrap();
    assert!(!unchanged.changed);
    assert!(unchanged.materials.is_none());

    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 6);
    assert!(
        requests[0]
            .request_line
            .starts_with("POST /api/client/activate ")
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&requests[0].body).unwrap(),
        json!({"device_id": "DY-DEVICE", "activate_code": "ACTIVATE-CODE"})
    );

    assert!(
        requests[1]
            .request_line
            .starts_with("POST /api/client/heartbeat ")
    );
    assert_eq!(requests[1].headers.get("device_id").unwrap(), "DY-DEVICE");
    assert_eq!(requests[1].headers.get("device-id").unwrap(), "DY-DEVICE");
    assert_eq!(
        requests[1].headers.get("activate_code").unwrap(),
        "ACTIVATE-CODE"
    );
    assert_eq!(
        requests[1].headers.get("activate-code").unwrap(),
        "ACTIVATE-CODE"
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&requests[1].body).unwrap(),
        json!({})
    );

    assert!(
        requests[2]
            .request_line
            .starts_with("GET /api/client/app-start ")
    );
    assert_eq!(requests[2].headers.get("platform").unwrap(), "mac");
    assert_eq!(requests[2].headers.get("client_version").unwrap(), "0.3.0");

    assert!(
        requests[3]
            .request_line
            .starts_with("POST /api/client/telemetry ")
    );
    assert_eq!(requests[3].headers.get("device_id").unwrap(), "DY-DEVICE");
    let events = serde_json::from_slice::<Value>(&requests[3].body).unwrap();
    assert_eq!(events.as_array().unwrap().len(), 1);
    assert_eq!(events[0]["event"], "feature_use");
    assert_eq!(events[0]["props"], json!({"feature": "monitor"}));
    assert!(events[0]["props"].get("localPath").is_none());

    for (index, catalog_version) in [(4, 1), (5, 2)] {
        assert!(requests[index].request_line.starts_with(&format!(
            "GET /api/v1/transition-materials?clientVersion={catalog_version} "
        )));
        assert_eq!(
            requests[index].headers.get("device_id").unwrap(),
            "DY-DEVICE"
        );
        assert_eq!(
            requests[index].headers.get("activate_code").unwrap(),
            "ACTIVATE-CODE"
        );
        assert_eq!(
            requests[index].headers.get("activate-code").unwrap(),
            "ACTIVATE-CODE"
        );
    }
}

#[test]
fn server_recording_limit_uses_latest_valid_positive_value_and_preserves_it_otherwise() {
    let limit = ServerRecordingLimit::default();
    assert_eq!(limit.current(), 4);

    assert_eq!(limit.apply(Some(6), "app_start"), Some(6));
    assert_eq!(limit.current(), 6);
    assert_eq!(limit.apply(None, "heartbeat"), None);
    assert_eq!(limit.current(), 6);
    assert_eq!(limit.apply(Some(0), "heartbeat"), None);
    assert_eq!(limit.apply(Some(-1), "app_start"), None);
    assert_eq!(limit.current(), 6);
    assert_eq!(limit.apply(Some(3), "heartbeat"), Some(3));
    assert_eq!(limit.current(), 3);
}

#[tokio::test]
async fn app_start_and_heartbeat_parse_the_optional_limit_independently() {
    let responses = vec![
        json!({
            "code": 200,
            "data": {"modelId": "model-a", "modelAsr": "asr-a", "version": "1.0.0", "url": "https://cdn.example/app", "signature": null, "minClientVersion": "0.2.0", "force": false},
            "msg": "success"
        }),
        json!({
            "code": 200,
            "data": {"revoked": false, "state": "ACTIVE", "reason": "", "serverTime": 43, "max_screen_limit": 5},
            "msg": "success"
        }),
    ];
    let (base_url, server) = spawn_server(responses);
    let client = api_client(base_url);

    assert_eq!(
        client.app_start().await.unwrap().unwrap().max_screen_limit,
        None
    );
    assert_eq!(
        client
            .heartbeat("DY-DEVICE", "ACTIVATE-CODE")
            .await
            .unwrap()
            .max_screen_limit,
        Some(5)
    );
    assert_eq!(server.join().unwrap().len(), 2);
}

#[tokio::test]
async fn synchronous_heartbeat_header_contract_error_never_unlocks_the_client() {
    let responses = vec![
        json!({
            "code": 200,
            "data": {"token": "offline-token", "expireAt": 4_102_444_800_i64, "graceSec": 300, "encKey": "future-key"},
            "msg": "激活成功"
        }),
        json!({"code": 1001, "data": null, "msg": "缺少 activate_code 请求头"}),
    ];
    let (base_url, server) = spawn_server(responses);
    let temporary = tempfile::tempdir().unwrap();
    let database = Database::open(&temporary.path().join("client.sqlite3")).unwrap();
    database.migrate().unwrap();
    let (service, store) =
        service_with_memory_store(database, api_client(base_url), temporary.path());

    let error = service.activate("ACTIVATE-CODE").await.unwrap_err();
    assert!(error.contains("activate_code"));
    let state = service.state();
    assert!(!state.active);
    assert_eq!(state.status, "contract_error");
    assert!(state.message.unwrap().contains("underscores_in_headers"));
    assert!(store.load().unwrap().is_none());
    assert_eq!(server.join().unwrap().len(), 2);
}

#[tokio::test]
async fn activation_in_progress_keeps_the_gate_and_enters_retrying_state() {
    let (base_url, server) = spawn_server(vec![json!({
        "code": 1007,
        "data": null,
        "msg": "激活处理中"
    })]);
    let temporary = tempfile::tempdir().unwrap();
    let database = Database::open(&temporary.path().join("client.sqlite3")).unwrap();
    database.migrate().unwrap();
    let (service, store) =
        service_with_memory_store(database, api_client(base_url), temporary.path());

    let error = service.activate("ACTIVATE-CODE").await.unwrap_err();
    assert!(error.contains("稍后重试"));
    let state = service.state();
    assert!(!state.active);
    assert_eq!(state.status, "retrying");
    assert!(state.next_heartbeat_at.is_some());
    assert!(store.load().unwrap().is_none());
    assert_eq!(server.join().unwrap().len(), 1);
}

#[tokio::test]
async fn synchronous_heartbeat_inactive_response_never_persists_secrets() {
    let responses = vec![
        json!({
            "code": 200,
            "data": {"token": "offline-token", "expireAt": 4_102_444_800_i64, "graceSec": 300, "encKey": "future-key"},
            "msg": "激活成功"
        }),
        json!({
            "code": 201,
            "data": {
                "revoked": true,
                "state": "INACTIVE",
                "reason": "unbound",
                "serverTime": 1_785_943_399_i64
            },
            "msg": "强制下线，请重新登录"
        }),
    ];
    let (base_url, server) = spawn_server(responses);
    let temporary = tempfile::tempdir().unwrap();
    let database = Database::open(&temporary.path().join("client.sqlite3")).unwrap();
    database.migrate().unwrap();
    let (service, store) =
        service_with_memory_store(database.clone(), api_client(base_url), temporary.path());

    assert!(service.activate("ACTIVATE-CODE").await.is_err());
    let state = service.state();
    assert!(!state.active);
    assert_eq!(state.status, "revoked");
    assert!(state.message.unwrap().contains("设备绑定已重置"));
    assert!(store.load().unwrap().is_none());
    let record = database.client_activation().unwrap().unwrap();
    assert_eq!(record.state, "revoked");
    assert_eq!(server.join().unwrap().len(), 2);
}

#[tokio::test]
async fn periodic_heartbeat_force_offline_clears_secrets_and_locks_the_client() {
    let responses = vec![
        json!({
            "code": 200,
            "data": {"token": "TEST-TOKEN-INITIAL", "expireAt": 4_102_444_800_i64, "graceSec": 300},
            "msg": "激活成功"
        }),
        json!({
            "code": 200,
            "data": {"revoked": false, "state": "ACTIVE", "reason": "ok", "serverTime": 1_785_943_399_i64, "token": "TEST-TOKEN-RENEWED"},
            "msg": "续约成功"
        }),
        json!({
            "code": 201,
            "data": {"revoked": true, "state": "DISABLED", "reason": "disabled", "serverTime": 1_785_943_400_i64},
            "msg": "强制下线"
        }),
    ];
    let (base_url, server) = spawn_server(responses);
    let temporary = tempfile::tempdir().unwrap();
    let database = Database::open(&temporary.path().join("client.sqlite3")).unwrap();
    database.migrate().unwrap();
    let (service, store) =
        service_with_memory_store(database, api_client(base_url), temporary.path());

    assert!(service.activate("ACTIVATE-CODE").await.unwrap().active);
    assert_eq!(
        store.load().unwrap().unwrap().token.as_deref(),
        Some("TEST-TOKEN-RENEWED")
    );
    assert_eq!(service.heartbeat_once().await, HeartbeatOutcome::Revoked);
    assert_eq!(service.state().status, "revoked");
    assert!(store.load().unwrap().is_none());
    assert_eq!(server.join().unwrap().len(), 3);
}

#[tokio::test]
async fn synchronous_heartbeat_protocol_error_is_retried_once_before_unlocking() {
    let responses = vec![
        json!({
            "code": 200,
            "data": {"token": "TEST-TOKEN", "expireAt": 4_102_444_800_i64, "graceSec": 300}
        }),
        json!({"code": 9104, "data": null, "msg": "时钟偏差"}),
        json!({
            "code": 200,
            "data": {"revoked": false, "state": "ACTIVE", "reason": "ok", "serverTime": 1_785_943_399_i64}
        }),
    ];
    let (base_url, server) = spawn_server(responses);
    let temporary = tempfile::tempdir().unwrap();
    let database = Database::open(&temporary.path().join("client.sqlite3")).unwrap();
    database.migrate().unwrap();
    let (service, store) =
        service_with_memory_store(database, api_client(base_url), temporary.path());

    assert!(service.activate("ACTIVATE-CODE").await.unwrap().active);
    assert!(store.load().unwrap().is_some());
    assert_eq!(server.join().unwrap().len(), 3);
}

#[tokio::test]
async fn repeated_periodic_protocol_error_requires_reactivation_after_one_retry() {
    let responses = vec![
        json!({
            "code": 200,
            "data": {"token": "TEST-TOKEN", "expireAt": 4_102_444_800_i64, "graceSec": 300}
        }),
        json!({
            "code": 200,
            "data": {"revoked": false, "state": "ACTIVE", "reason": "ok", "serverTime": 1_785_943_399_i64}
        }),
        json!({"code": 9105, "data": null, "msg": "会话失步"}),
        json!({"code": 9105, "data": null, "msg": "会话仍失步"}),
    ];
    let (base_url, server) = spawn_server(responses);
    let temporary = tempfile::tempdir().unwrap();
    let database = Database::open(&temporary.path().join("client.sqlite3")).unwrap();
    database.migrate().unwrap();
    let (service, store) =
        service_with_memory_store(database, api_client(base_url), temporary.path());

    assert!(service.activate("ACTIVATE-CODE").await.unwrap().active);
    assert_eq!(service.heartbeat_once().await, HeartbeatOutcome::Revoked);
    assert_eq!(service.state().status, "invalid");
    assert!(store.load().unwrap().is_none());
    assert_eq!(server.join().unwrap().len(), 4);
}

#[tokio::test]
async fn heartbeat_business_codes_keep_distinct_error_categories() {
    let cases = [
        (1001, "header_contract"),
        (1002, "authorization_invalid"),
        (1003, "authorization_invalid"),
        (1004, "authorization_invalid"),
        (1005, "authorization_invalid"),
        (1006, "authorization_invalid"),
        (1007, "retryable_service"),
        (2001, "header_contract"),
        (2002, "authorization_invalid"),
        (2003, "authorization_invalid"),
        (2004, "authorization_invalid"),
        (9000, "retryable_service"),
        (9101, "protocol_recoverable"),
        (9102, "protocol_recoverable"),
        (9103, "protocol_recoverable"),
        (9104, "protocol_recoverable"),
        (9105, "protocol_recoverable"),
    ];

    for (code, category) in cases {
        let (base_url, server) = spawn_server(vec![json!({
            "code": code,
            "data": null,
            "msg": "TEST-SERVER-MESSAGE-MUST-NOT-PROPAGATE"
        })]);
        let error = api_client(base_url)
            .heartbeat("DY-TEST-DEVICE-12345678", "TEST-ACTIVATION-CODE")
            .await
            .unwrap_err();
        assert_eq!(error.code(), Some(code));
        assert_eq!(error.category(), category);
        assert!(!error.safe_message().contains("MUST-NOT-PROPAGATE"));
        assert_eq!(server.join().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn api_distinguishes_network_failure_and_timeout() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let disconnect_server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        drop(stream);
    });
    let network_error = api_client(format!("http://{address}/api"))
        .heartbeat("DY-TEST-DEVICE", "TEST-ACTIVATION-CODE")
        .await
        .unwrap_err();
    assert!(matches!(network_error, ApiError::Transport));
    disconnect_server.join().unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (_stream, _) = listener.accept().unwrap();
        thread::sleep(Duration::from_millis(250));
    });
    let timeout_error =
        api_client_with_timeout(format!("http://{address}/api"), Duration::from_millis(50))
            .heartbeat("DY-TEST-DEVICE", "TEST-ACTIVATION-CODE")
            .await
            .unwrap_err();
    assert!(matches!(timeout_error, ApiError::Timeout));
    server.join().unwrap();
}

#[tokio::test]
async fn correlation_headers_are_preserved_for_valid_and_invalid_responses() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        for (body, request_id, trace_id) in [
            (
                serde_json::to_vec(&json!({
                    "code": 200,
                    "data": {"token": "TEST-OFFLINE-TOKEN", "expireAt": 4_102_444_800_i64, "graceSec": 300}
                }))
                .unwrap(),
                "request-activate",
                None,
            ),
            (
                serde_json::to_vec(&json!({
                    "code": 200,
                    "data": {"revoked": false, "state": "ACTIVE", "reason": "ok", "serverTime": 42}
                }))
                .unwrap(),
                "request-valid",
                Some("trace-valid"),
            ),
            (b"not-json".to_vec(), "request-invalid", Some("trace-invalid")),
        ] {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            loop {
                line.clear();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" || line.is_empty() {
                    break;
                }
            }
            let trace_header = trace_id
                .map(|value| format!("x-trace-id: {value}\r\n"))
                .unwrap_or_default();
            let head = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\nx-request-id: {request_id}\r\n{trace_header}content-length: {}\r\nconnection: close\r\n\r\n",
                body.len()
            );
            stream.write_all(head.as_bytes()).unwrap();
            stream.write_all(&body).unwrap();
        }
    });
    let diagnostics = MemoryApiDiagnosticSink::default();
    let client = ApiClient::new_with_diagnostics(
        ApiConfig {
            base_url: format!("http://{address}/api"),
            timeout: Duration::from_secs(3),
            client_version: "0.3.0".to_owned(),
            platform: "mac".to_owned(),
        },
        Arc::new(PlainJsonCodec),
        Arc::new(diagnostics.clone()),
    )
    .unwrap();

    client
        .activate("DY-TEST-DEVICE-12345678", "TEST-ACTIVATION-CODE")
        .await
        .unwrap();
    client
        .heartbeat("DY-TEST-DEVICE-12345678", "TEST-ACTIVATION-CODE")
        .await
        .unwrap();
    let invalid = client
        .heartbeat("DY-TEST-DEVICE-12345678", "TEST-ACTIVATION-CODE")
        .await
        .unwrap_err();
    assert_eq!(invalid.request_id(), Some("request-invalid"));
    assert_eq!(invalid.trace_id(), Some("trace-invalid"));

    let events = diagnostics.events();
    let activation_completed = events
        .iter()
        .find(|event| event.operation == "activate" && event.event == "request_completed")
        .unwrap();
    assert_eq!(
        activation_completed.request_id.as_deref(),
        Some("request-activate")
    );
    assert_eq!(
        activation_completed.request_fields,
        vec!["device_id", "activate_code"]
    );
    let completed = events
        .iter()
        .find(|event| event.operation == "heartbeat" && event.event == "request_completed")
        .unwrap();
    assert_eq!(completed.request_id.as_deref(), Some("request-valid"));
    assert_eq!(completed.trace_id.as_deref(), Some("trace-valid"));
    let failed = events
        .iter()
        .find(|event| event.event == "request_failed")
        .unwrap();
    assert_eq!(failed.request_id.as_deref(), Some("request-invalid"));
    assert_eq!(failed.trace_id.as_deref(), Some("trace-invalid"));
    assert_eq!(failed.error_category, Some("invalid_response"));

    let serialized = serde_json::to_string(&events).unwrap();
    for secret in [
        "DY-TEST-DEVICE-12345678",
        "TEST-ACTIVATION-CODE",
        "TEST-OFFLINE-TOKEN",
        "Cookie",
        "signature",
        "/private/test-path",
    ] {
        assert!(
            !serialized.contains(secret),
            "诊断中泄露了测试秘密：{secret}"
        );
    }
    assert!(serialized.contains("…12345678"));
    server.join().unwrap();
}

#[tokio::test]
async fn non_activation_api_only_emits_a_redacted_failure_summary() {
    let (base_url, server) = spawn_server(vec![json!({
        "code": 9000,
        "data": null,
        "msg": "TEST-RAW-SERVER-DETAIL"
    })]);
    let diagnostics = MemoryApiDiagnosticSink::default();
    let client = ApiClient::new_with_diagnostics(
        ApiConfig {
            base_url,
            timeout: Duration::from_secs(3),
            client_version: "0.3.0".to_owned(),
            platform: "mac".to_owned(),
        },
        Arc::new(PlainJsonCodec),
        Arc::new(diagnostics.clone()),
    )
    .unwrap();

    let error = client.app_start().await.unwrap_err();
    assert_eq!(error.code(), Some(9000));
    let events = diagnostics.events();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].operation, "app_start");
    assert_eq!(events[0].event, "request_failed");
    assert_eq!(events[0].path, "client/app-start");
    let serialized = serde_json::to_string(&events).unwrap();
    assert!(!serialized.contains("TEST-RAW-SERVER-DETAIL"));
    assert!(!serialized.contains("http://"));
    assert_eq!(server.join().unwrap().len(), 1);
}

#[tokio::test]
async fn concurrent_activation_requests_are_single_flight_and_the_last_result_wins() {
    let responses = vec![
        json!({
            "code": 200,
            "data": {"token": "TEST-TOKEN-A", "expireAt": 4_102_444_800_i64, "graceSec": 300}
        }),
        json!({
            "code": 200,
            "data": {"revoked": false, "state": "ACTIVE", "reason": "ok", "serverTime": 1_785_943_399_i64}
        }),
        json!({
            "code": 200,
            "data": {"token": "TEST-TOKEN-B", "expireAt": 4_102_444_800_i64, "graceSec": 300}
        }),
        json!({
            "code": 200,
            "data": {"revoked": false, "state": "ACTIVE", "reason": "ok", "serverTime": 1_785_943_400_i64}
        }),
    ];
    let (base_url, server) = spawn_server(responses);
    let temporary = tempfile::tempdir().unwrap();
    let database = Database::open(&temporary.path().join("client.sqlite3")).unwrap();
    database.migrate().unwrap();
    let (service, store) =
        service_with_memory_store(database, api_client(base_url), temporary.path());
    let first_service = service.clone();
    let second_service = service.clone();

    let first = tokio::spawn(async move { first_service.activate("TEST-CODE-A").await });
    let second = tokio::spawn(async move { second_service.activate("TEST-CODE-B").await });
    assert!(first.await.unwrap().unwrap().active);
    assert!(second.await.unwrap().unwrap().active);

    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 4);
    assert!(requests[0].request_line.contains("/client/activate "));
    assert!(requests[1].request_line.contains("/client/heartbeat "));
    assert!(requests[2].request_line.contains("/client/activate "));
    assert!(requests[3].request_line.contains("/client/heartbeat "));
    let last_activation_body = serde_json::from_slice::<Value>(&requests[2].body).unwrap();
    assert_eq!(
        store.load().unwrap().unwrap().activate_code,
        last_activation_body["activate_code"].as_str().unwrap()
    );
    assert!(service.state().active);
}

#[tokio::test]
async fn manual_activation_waits_for_an_in_flight_periodic_heartbeat() {
    let responses = vec![
        json!({
            "code": 200,
            "data": {"token": "TEST-TOKEN-A", "expireAt": 4_102_444_800_i64, "graceSec": 300}
        }),
        json!({
            "code": 200,
            "data": {"revoked": false, "state": "ACTIVE", "reason": "ok", "serverTime": 1_785_943_399_i64}
        }),
        json!({
            "code": 200,
            "data": {"revoked": false, "state": "ACTIVE", "reason": "ok", "serverTime": 1_785_943_400_i64}
        }),
        json!({
            "code": 200,
            "data": {"token": "TEST-TOKEN-B", "expireAt": 4_102_444_800_i64, "graceSec": 300}
        }),
        json!({
            "code": 200,
            "data": {"revoked": false, "state": "ACTIVE", "reason": "ok", "serverTime": 1_785_943_401_i64}
        }),
    ];
    let (base_url, heartbeat_reached, release_heartbeat, server) = spawn_gated_server(responses, 2);
    let temporary = tempfile::tempdir().unwrap();
    let database = Database::open(&temporary.path().join("client.sqlite3")).unwrap();
    database.migrate().unwrap();
    let (service, store) =
        service_with_memory_store(database, api_client(base_url), temporary.path());
    assert!(service.activate("TEST-CODE-A").await.unwrap().active);

    let heartbeat_service = service.clone();
    let heartbeat = tokio::spawn(async move { heartbeat_service.heartbeat_once().await });
    tokio::task::spawn_blocking(move || heartbeat_reached.recv_timeout(Duration::from_secs(3)))
        .await
        .unwrap()
        .unwrap();
    let activation_service = service.clone();
    let activation = tokio::spawn(async move { activation_service.activate("TEST-CODE-B").await });
    tokio::time::sleep(Duration::from_millis(25)).await;
    assert!(!activation.is_finished());

    release_heartbeat.send(()).unwrap();
    assert_eq!(heartbeat.await.unwrap(), HeartbeatOutcome::Active);
    assert!(activation.await.unwrap().unwrap().active);
    assert_eq!(store.load().unwrap().unwrap().activate_code, "TEST-CODE-B");

    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 5);
    assert_eq!(
        requests[2].headers.get("activate_code").map(String::as_str),
        Some("TEST-CODE-A")
    );
    assert!(requests[3].request_line.contains("/client/activate "));
    assert_eq!(
        requests[4].headers.get("activate_code").map(String::as_str),
        Some("TEST-CODE-B")
    );
}
