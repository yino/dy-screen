use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use dy_screen_app_lib::activation::{ActivationService, HeartbeatOutcome};
use dy_screen_app_lib::api::{ApiClient, ApiConfig, PlainJsonCodec, TelemetryEvent};
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

fn api_client(base_url: String) -> ApiClient {
    ApiClient::new(
        ApiConfig {
            base_url,
            timeout: Duration::from_secs(3),
            client_version: "0.2.0".to_owned(),
            platform: "mac".to_owned(),
        },
        Arc::new(PlainJsonCodec),
    )
    .unwrap()
}

#[tokio::test]
async fn client_api_matches_activation_heartbeat_app_start_and_telemetry_contracts() {
    let responses = vec![
        json!({
            "code": 200,
            "data": {"token": "offline-token", "expireAt": 42, "graceSec": 300, "encKey": "future-key"},
            "msg": "激活成功"
        }),
        json!({
            "code": 201,
            "data": {"revoked": true, "state": "DISABLED", "reason": "disabled", "serverTime": 43},
            "msg": "强制下线"
        }),
        json!({
            "code": 200,
            "data": {"modelId": "model-a", "modelAsr": "asr-a", "version": "1.0.0", "url": "https://cdn.example/app", "signature": null, "minClientVersion": "0.2.0", "force": false},
            "msg": "success"
        }),
        json!({"code": 200, "data": {"accepted": 1, "rejected": 0}, "msg": "已上报"}),
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
    let app_start = client.app_start().await.unwrap().unwrap();
    assert_eq!(app_start.model_id, "model-a");
    let event = TelemetryEvent::new(
        "feature_use",
        json!({"feature": "monitor", "localPath": "/private/video.mkv"})
            .as_object()
            .unwrap()
            .clone(),
        "0.2.0",
    )
    .unwrap();
    let telemetry = client
        .telemetry(Some("DY-DEVICE"), Some("ACTIVATE-CODE"), &[event])
        .await
        .unwrap();
    assert_eq!(telemetry.accepted, 1);

    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 4);
    assert!(requests[0].request_line.starts_with("POST /api/client/activate "));
    assert_eq!(
        serde_json::from_slice::<Value>(&requests[0].body).unwrap(),
        json!({"device_id": "DY-DEVICE", "activate_code": "ACTIVATE-CODE"})
    );

    assert!(requests[1].request_line.starts_with("POST /api/client/heartbeat "));
    assert_eq!(requests[1].headers.get("device_id").unwrap(), "DY-DEVICE");
    assert_eq!(
        requests[1].headers.get("activate_code").unwrap(),
        "ACTIVATE-CODE"
    );
    assert_eq!(serde_json::from_slice::<Value>(&requests[1].body).unwrap(), json!({}));

    assert!(requests[2].request_line.starts_with("GET /api/client/app-start "));
    assert_eq!(requests[2].headers.get("platform").unwrap(), "mac");
    assert_eq!(requests[2].headers.get("client_version").unwrap(), "0.2.0");

    assert!(requests[3].request_line.starts_with("POST /api/client/telemetry "));
    assert_eq!(requests[3].headers.get("device_id").unwrap(), "DY-DEVICE");
    let events = serde_json::from_slice::<Value>(&requests[3].body).unwrap();
    assert_eq!(events.as_array().unwrap().len(), 1);
    assert_eq!(events[0]["event"], "feature_use");
    assert_eq!(events[0]["props"], json!({"feature": "monitor"}));
    assert!(events[0]["props"].get("localPath").is_none());
}

#[tokio::test]
async fn heartbeat_business_revocation_clears_local_authorization() {
    let responses = vec![
        json!({
            "code": 200,
            "data": {"token": "offline-token", "expireAt": 42, "graceSec": 300, "encKey": "future-key"},
            "msg": "激活成功"
        }),
        json!({"code": 1005, "data": null, "msg": "激活码已停用"}),
    ];
    let (base_url, server) = spawn_server(responses);
    let temporary = tempfile::tempdir().unwrap();
    let database = Database::open(&temporary.path().join("client.sqlite3")).unwrap();
    database.migrate().unwrap();
    let service = ActivationService::new(database, api_client(base_url), temporary.path()).unwrap();

    service.activate("ACTIVATE-CODE").await.unwrap();
    assert!(service.state().active);
    assert_eq!(service.heartbeat_once().await, HeartbeatOutcome::Revoked);
    let state = service.state();
    assert!(!state.active);
    assert_eq!(state.status, "revoked");
    assert!(state.message.unwrap().contains("已停用"));
    assert_eq!(server.join().unwrap().len(), 2);
}
