//! 客户端激活状态、心跳和安全埋点生命周期。
//!
//! 激活码与令牌只存在于本模块和 SQLite 内部记录中；前端得到的始终是
//! `ActivationStateView` 摘要。网络调用全部委托给 `api.rs`。

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{Duration as ChronoDuration, Utc};
use serde::Serialize;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::api::{ApiClient, ApiError, TelemetryEvent};
use crate::database::{ClientActivationRecord, Database};

pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(90);
pub const HEARTBEAT_RETRY_INTERVAL: Duration = Duration::from_secs(30);
const TELEMETRY_FLUSH_INTERVAL: Duration = Duration::from_secs(10);
const TELEMETRY_BATCH_SIZE: usize = 20;
const TELEMETRY_QUEUE_CAPACITY: usize = 200;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ActivationStateView {
    pub configured: bool,
    pub active: bool,
    pub status: String,
    pub message: Option<String>,
    pub device_id_hint: String,
    pub last_heartbeat_at: Option<String>,
    pub next_heartbeat_at: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeartbeatOutcome {
    Active,
    Retrying,
    Revoked,
    Missing,
}

#[derive(Clone)]
pub struct ActivationService {
    database: Database,
    api: ApiClient,
    device_id: Arc<String>,
    record: Arc<Mutex<Option<ClientActivationRecord>>>,
    cancellation: CancellationToken,
}

impl ActivationService {
    pub fn new(database: Database, api: ApiClient, app_data_dir: &Path) -> Result<Self, String> {
        let record = database.client_activation().map_err(|error| error.to_string())?;
        let device_id = record
            .as_ref()
            .map(|record| record.device_id.clone())
            .unwrap_or_else(|| stable_device_id(app_data_dir));
        Ok(Self {
            database,
            api,
            device_id: Arc::new(device_id),
            record: Arc::new(Mutex::new(record)),
            cancellation: CancellationToken::new(),
        })
    }

    pub fn api(&self) -> &ApiClient {
        &self.api
    }

    pub fn state(&self) -> ActivationStateView {
        let record = self.record.lock().ok().and_then(|record| record.clone());
        state_from_record(record.as_ref(), &self.device_id)
    }

    pub fn is_active(&self) -> bool {
        self.state().active
    }

    pub async fn activate(&self, code: &str) -> Result<ActivationStateView, String> {
        let code = normalize_activation_code(code)?;
        let response = self
            .api
            .activate(&self.device_id, &code)
            .await
            .map_err(|error| error.safe_message())?;
        let now = Utc::now();
        let record = ClientActivationRecord {
            device_id: self.device_id.as_ref().clone(),
            activate_code: code,
            token: Some(response.token),
            expire_at: Some(response.expire_at),
            grace_sec: Some(response.grace_sec),
            server_time_offset_sec: 0,
            state: "active".to_owned(),
            allow_custom_api_key: false,
            last_heartbeat_at: None,
            next_heartbeat_at: Some((now + heartbeat_duration(HEARTBEAT_INTERVAL)).to_rfc3339()),
            last_error: None,
            updated_at: now.to_rfc3339(),
        };
        self.save_record(record)?;
        Ok(self.state())
    }

    pub async fn heartbeat_once(&self) -> HeartbeatOutcome {
        let Some(mut record) = self.record.lock().ok().and_then(|record| record.clone()) else {
            return HeartbeatOutcome::Missing;
        };
        if !matches!(record.state.as_str(), "active" | "retrying") {
            return HeartbeatOutcome::Revoked;
        }
        match self
            .api
            .heartbeat(&record.device_id, &record.activate_code)
            .await
        {
            Ok(response) if response.revoked || !response.state.eq_ignore_ascii_case("ACTIVE") => {
                record.token = None;
                record.state = "revoked".to_owned();
                record.last_error = Some(reason_message(&response.reason));
                record.last_heartbeat_at = Some(Utc::now().to_rfc3339());
                record.next_heartbeat_at = None;
                record.updated_at = Utc::now().to_rfc3339();
                let _ = self.save_record(record);
                HeartbeatOutcome::Revoked
            }
            Ok(response) => {
                let now = Utc::now();
                if let Some(token) = response.token {
                    record.token = Some(token);
                }
                if let Some(expire_at) = response.expire_at {
                    record.expire_at = Some(expire_at);
                }
                if let Some(grace_sec) = response.grace_sec {
                    record.grace_sec = Some(grace_sec);
                }
                record.server_time_offset_sec = response.server_time - now.timestamp();
                record.allow_custom_api_key = response.allow_custom_api_key == Some(1);
                record.state = "active".to_owned();
                record.last_error = None;
                record.last_heartbeat_at = Some(now.to_rfc3339());
                record.next_heartbeat_at =
                    Some((now + heartbeat_duration(HEARTBEAT_INTERVAL)).to_rfc3339());
                record.updated_at = now.to_rfc3339();
                let _ = self.save_record(record);
                HeartbeatOutcome::Active
            }
            Err(error) => {
                let now = Utc::now();
                record.state = "retrying".to_owned();
                record.last_error = Some(error.safe_message());
                record.next_heartbeat_at =
                    Some((now + heartbeat_duration(HEARTBEAT_RETRY_INTERVAL)).to_rfc3339());
                record.updated_at = now.to_rfc3339();
                let _ = self.save_record(record);
                HeartbeatOutcome::Retrying
            }
        }
    }

    pub fn credentials(&self) -> Option<(String, String)> {
        self.record.lock().ok().and_then(|record| {
            record.as_ref().and_then(|record| {
                matches!(record.state.as_str(), "active" | "retrying")
                    .then(|| (record.device_id.clone(), record.activate_code.clone()))
            })
        })
    }

    pub fn clear(&self) -> Result<ActivationStateView, String> {
        self.database
            .clear_client_activation()
            .map_err(|error| error.to_string())?;
        *self
            .record
            .lock()
            .map_err(|_| "激活状态锁已损坏".to_owned())? = None;
        Ok(self.state())
    }

    pub fn cancellation(&self) -> CancellationToken {
        self.cancellation.clone()
    }

    pub fn shutdown(&self) {
        self.cancellation.cancel();
    }

    fn save_record(&self, record: ClientActivationRecord) -> Result<(), String> {
        self.database
            .save_client_activation(&record)
            .map_err(|error| error.to_string())?;
        *self
            .record
            .lock()
            .map_err(|_| "激活状态锁已损坏".to_owned())? = Some(record);
        Ok(())
    }
}

fn stable_device_id(app_data_dir: &Path) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"dy-screen-device-v1\0");
    hasher.update(app_data_dir.to_string_lossy().as_bytes());
    for key in ["HOSTNAME", "COMPUTERNAME", "USER", "USERNAME"] {
        if let Ok(value) = std::env::var(key) {
            hasher.update(b"\0");
            hasher.update(key.as_bytes());
            hasher.update(b"=");
            hasher.update(value.as_bytes());
        }
    }
    let digest = hex::encode(hasher.finalize());
    format!("DY-{}", &digest[..24])
}

fn normalize_activation_code(code: &str) -> Result<String, String> {
    let code = code.trim();
    if code.len() < 4 || code.len() > 128 || code.chars().any(char::is_control) {
        return Err("请输入有效的激活码".to_owned());
    }
    Ok(code.to_owned())
}

fn heartbeat_duration(duration: Duration) -> ChronoDuration {
    ChronoDuration::seconds(i64::try_from(duration.as_secs()).unwrap_or(i64::MAX))
}

fn state_from_record(
    record: Option<&ClientActivationRecord>,
    device_id: &str,
) -> ActivationStateView {
    let Some(record) = record else {
        return ActivationStateView {
            configured: false,
            active: false,
            status: "missing".to_owned(),
            message: Some("请输入激活码后继续使用".to_owned()),
            device_id_hint: device_hint(device_id),
            last_heartbeat_at: None,
            next_heartbeat_at: None,
        };
    };
    let active = matches!(record.state.as_str(), "active" | "retrying")
        && record.token.as_ref().is_some_and(|token| !token.is_empty());
    ActivationStateView {
        configured: true,
        active,
        status: record.state.clone(),
        message: record.last_error.clone(),
        device_id_hint: device_hint(&record.device_id),
        last_heartbeat_at: record.last_heartbeat_at.clone(),
        next_heartbeat_at: record.next_heartbeat_at.clone(),
    }
}

fn device_hint(device_id: &str) -> String {
    let suffix = device_id
        .chars()
        .rev()
        .take(8)
        .collect::<String>()
        .chars()
        .rev()
        .collect::<String>();
    format!("…{suffix}")
}

fn reason_message(reason: &str) -> String {
    match reason {
        "disabled" => "激活码已停用，请联系管理员".to_owned(),
        "expired" => "激活码已过期，请续费后重新激活".to_owned(),
        "unbound" => "设备绑定已重置，请重新激活".to_owned(),
        _ => "授权已失效，请重新激活".to_owned(),
    }
}

#[derive(Clone)]
pub struct TelemetryQueue {
    sender: mpsc::Sender<TelemetryEvent>,
}

impl TelemetryQueue {
    pub fn spawn(service: ActivationService) -> Self {
        let (sender, mut receiver) = mpsc::channel(TELEMETRY_QUEUE_CAPACITY);
        let cancellation = service.cancellation();
        tokio::spawn(async move {
            let mut batch = Vec::with_capacity(TELEMETRY_BATCH_SIZE);
            let mut interval = tokio::time::interval(TELEMETRY_FLUSH_INTERVAL);
            loop {
                tokio::select! {
                    _ = cancellation.cancelled() => return,
                    _ = interval.tick() => {
                        flush_telemetry(&service, &mut batch).await;
                    }
                    event = receiver.recv() => {
                        let Some(event) = event else { return; };
                        batch.push(event);
                        if batch.len() >= TELEMETRY_BATCH_SIZE {
                            flush_telemetry(&service, &mut batch).await;
                        }
                    }
                }
            }
        });
        Self { sender }
    }

    pub fn track(&self, event: &str, props: Map<String, Value>) {
        if let Some(event) = TelemetryEvent::new(event, props, env!("CARGO_PKG_VERSION")) {
            let _ = self.sender.try_send(event);
        }
    }
}

async fn flush_telemetry(service: &ActivationService, batch: &mut Vec<TelemetryEvent>) {
    if batch.is_empty() {
        return;
    }
    let Some((device_id, activate_code)) = service.credentials() else {
        batch.clear();
        return;
    };
    let events = std::mem::take(batch);
    let _ = service
        .api()
        .telemetry(Some(&device_id), Some(&activate_code), &events)
        .await;
}

pub fn api_error_is_revocation(error: &ApiError) -> bool {
    matches!(error.code(), Some(1004 | 1005 | 2003 | 2004))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_id_is_stable_and_frontend_only_gets_hint() {
        let first = stable_device_id(Path::new("/tmp/dy-screen-test"));
        let second = stable_device_id(Path::new("/tmp/dy-screen-test"));
        assert_eq!(first, second);
        assert!(first.starts_with("DY-"));
        let hint = device_hint(&first);
        assert!(hint.starts_with('…'));
        assert!(!hint.contains(&first));
    }

    #[test]
    fn activation_code_validation_is_bounded() {
        assert!(normalize_activation_code("ABCD-1234").is_ok());
        assert!(normalize_activation_code("abc").is_err());
        assert!(normalize_activation_code(&"x".repeat(129)).is_err());
        assert!(normalize_activation_code("ABC\n123").is_err());
    }

    #[test]
    fn revoked_record_never_reports_active() {
        let record = ClientActivationRecord {
            device_id: "DY-123456789".to_owned(),
            activate_code: "SECRET".to_owned(),
            token: None,
            expire_at: None,
            grace_sec: None,
            server_time_offset_sec: 0,
            state: "revoked".to_owned(),
            allow_custom_api_key: false,
            last_heartbeat_at: None,
            next_heartbeat_at: None,
            last_error: Some("授权已失效".to_owned()),
            updated_at: Utc::now().to_rfc3339(),
        };
        let view = state_from_record(Some(&record), &record.device_id);
        assert!(!view.active);
        let json = serde_json::to_string(&view).unwrap();
        assert!(!json.contains("SECRET"));
    }
}
