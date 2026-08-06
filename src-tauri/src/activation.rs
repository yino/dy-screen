//! 客户端激活状态、心跳和安全埋点生命周期。
//!
//! 完整设备号、激活码与令牌只存在于本模块和系统安全凭据存储中；前端
//! 得到的始终是 `ActivationStateView` 摘要。网络调用全部委托给 `api.rs`。

use std::path::Path;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{Duration as ChronoDuration, Utc};
use serde::Serialize;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use tokio::sync::{Mutex as AsyncMutex, mpsc};
use tokio_util::sync::CancellationToken;

use crate::activation_secret::{
    ActivationSecretStore, ActivationSecrets, SystemActivationSecretStore,
};
use crate::api::{ApiClient, ApiError, TelemetryEvent};
use crate::database::{ClientActivationRecord, Database};
use crate::domain::DEFAULT_MAX_SCREEN_LIMIT;
use crate::transition_materials::TransitionCatalogCoordinator;

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
    ContractError,
    Missing,
}

#[derive(Clone)]
pub struct ServerRecordingLimit {
    current: Arc<AtomicUsize>,
}

impl Default for ServerRecordingLimit {
    fn default() -> Self {
        Self {
            current: Arc::new(AtomicUsize::new(DEFAULT_MAX_SCREEN_LIMIT)),
        }
    }
}

impl ServerRecordingLimit {
    pub fn current(&self) -> usize {
        self.current.load(Ordering::Acquire)
    }

    pub fn apply(&self, value: Option<i64>, source: &'static str) -> Option<usize> {
        let value = value?;
        let Ok(value) = usize::try_from(value) else {
            log_invalid_server_limit(source);
            return None;
        };
        if value == 0 {
            log_invalid_server_limit(source);
            return None;
        }
        self.current.store(value, Ordering::Release);
        Some(value)
    }
}

fn log_invalid_server_limit(source: &str) {
    eprintln!(
        "{}",
        serde_json::json!({
            "component": "client_api",
            "event": "invalid_max_screen_limit",
            "source": source,
        })
    );
}

#[derive(Clone)]
pub struct ActivationService {
    database: Database,
    api: ApiClient,
    device_id: Arc<String>,
    record: Arc<Mutex<Option<ClientActivationRecord>>>,
    secrets: Arc<Mutex<Option<ActivationSecrets>>>,
    secret_store: Arc<dyn ActivationSecretStore>,
    credential_error: Arc<Mutex<Option<String>>>,
    operation_lock: Arc<AsyncMutex<()>>,
    generation: Arc<AtomicU64>,
    cancellation: CancellationToken,
    recording_limit: ServerRecordingLimit,
    transition_catalog: Arc<Mutex<Option<TransitionCatalogCoordinator>>>,
}

impl ActivationService {
    pub fn new(database: Database, api: ApiClient, app_data_dir: &Path) -> Result<Self, String> {
        Self::new_with_secret_store(
            database,
            api,
            app_data_dir,
            Arc::new(SystemActivationSecretStore::for_app_data_dir(app_data_dir)),
        )
    }

    pub fn new_with_secret_store(
        database: Database,
        api: ApiClient,
        app_data_dir: &Path,
        secret_store: Arc<dyn ActivationSecretStore>,
    ) -> Result<Self, String> {
        let mut record = database
            .client_activation()
            .map_err(|error| error.to_string())?;
        let (secrets, migration_device_id, credential_error) =
            load_and_migrate_secrets(&database, secret_store.as_ref())?;
        if let (Some(record), Some(error)) = (&mut record, &credential_error) {
            record.state = "invalid".to_owned();
            record.last_error = Some(error.clone());
            record.next_heartbeat_at = None;
            record.updated_at = Utc::now().to_rfc3339();
            database
                .save_client_activation(record)
                .map_err(|error| error.to_string())?;
        }
        let device_id = secrets
            .as_ref()
            .map(|secrets| secrets.device_id.clone())
            .or(migration_device_id)
            .unwrap_or_else(|| stable_device_id(app_data_dir));
        Ok(Self {
            database,
            api,
            device_id: Arc::new(device_id),
            record: Arc::new(Mutex::new(record)),
            secrets: Arc::new(Mutex::new(secrets)),
            secret_store,
            credential_error: Arc::new(Mutex::new(credential_error)),
            operation_lock: Arc::new(AsyncMutex::new(())),
            generation: Arc::new(AtomicU64::new(0)),
            cancellation: CancellationToken::new(),
            recording_limit: ServerRecordingLimit::default(),
            transition_catalog: Arc::new(Mutex::new(None)),
        })
    }

    pub fn api(&self) -> &ApiClient {
        &self.api
    }

    pub fn max_screen_limit(&self) -> usize {
        self.recording_limit.current()
    }

    pub fn apply_app_start_limit(&self, value: Option<i64>) -> Option<usize> {
        self.recording_limit.apply(value, "app_start")
    }

    pub fn set_transition_catalog_coordinator(
        &self,
        coordinator: TransitionCatalogCoordinator,
    ) -> Result<(), String> {
        *self
            .transition_catalog
            .lock()
            .map_err(|_| "素材目录协调器状态锁已损坏".to_owned())? = Some(coordinator);
        Ok(())
    }

    pub fn state(&self) -> ActivationStateView {
        let record = self.record.lock().ok().and_then(|record| record.clone());
        let secrets = self.secrets.lock().ok().and_then(|secrets| secrets.clone());
        let credential_error = self
            .credential_error
            .lock()
            .ok()
            .and_then(|error| error.clone());
        state_from_record(
            record.as_ref(),
            secrets.as_ref(),
            &self.device_id,
            credential_error.as_deref(),
        )
    }

    pub fn is_active(&self) -> bool {
        self.state().active
    }

    pub async fn activate(&self, code: &str) -> Result<ActivationStateView, String> {
        let _operation = self.operation_lock.lock().await;
        let generation = self.generation.fetch_add(1, Ordering::AcqRel) + 1;
        let code = normalize_activation_code(code)?;
        let (activation, retried_protocol) = self
            .activate_with_protocol_retry(&self.device_id, &code)
            .await;
        let response = match activation {
            Ok(response) => response,
            Err(error) => {
                let disposition = activation_error_disposition(&error, retried_protocol);
                let (state, message, delete_secret) = error_state(&error, disposition);
                let record = self
                    .record
                    .lock()
                    .ok()
                    .and_then(|record| record.clone())
                    .unwrap_or_else(|| empty_activation_record(&self.device_id));
                self.lock_authorization(record, state, message.clone(), delete_secret)?;
                return Err(message);
            }
        };
        self.ensure_generation(generation)?;
        let now = Utc::now();
        let verifying = ClientActivationRecord {
            device_id_hint: device_hint(&self.device_id),
            expire_at: Some(response.expire_at),
            grace_sec: Some(response.grace_sec),
            server_time_offset_sec: 0,
            state: "verifying".to_owned(),
            allow_custom_api_key: false,
            last_heartbeat_at: None,
            next_heartbeat_at: None,
            last_error: Some("激活码已接受，正在验证授权状态".to_owned()),
            updated_at: now.to_rfc3339(),
        };
        self.save_record(verifying.clone())?;
        self.set_credential_error(None)?;

        let (heartbeat, retried_protocol) = self
            .heartbeat_with_protocol_retry(&self.device_id, &code)
            .await;
        self.ensure_generation(generation)?;
        let heartbeat = match heartbeat {
            Ok(heartbeat)
                if heartbeat_requires_reactivation(heartbeat.revoked, &heartbeat.state) =>
            {
                let message = reason_message(&heartbeat.reason);
                self.lock_authorization(verifying, "revoked", message.clone(), true)?;
                return Err(message);
            }
            Ok(heartbeat) => heartbeat,
            Err(error) => {
                let disposition = heartbeat_error_disposition(&error, retried_protocol);
                let (state, message, delete_secret) = error_state(&error, disposition);
                self.lock_authorization(verifying, state, message.clone(), delete_secret)?;
                return Err(message);
            }
        };

        let token = heartbeat.token.clone().unwrap_or(response.token);
        let secrets = ActivationSecrets {
            device_id: self.device_id.as_ref().clone(),
            activate_code: code,
            token: Some(token),
        };
        self.persist_secrets(&secrets)?;
        let active = self.active_record_from_heartbeat(verifying, &heartbeat);
        self.apply_heartbeat_side_effects(&heartbeat, &secrets);
        self.save_record(active)?;
        Ok(self.state())
    }

    pub async fn heartbeat_once(&self) -> HeartbeatOutcome {
        let _operation = self.operation_lock.lock().await;
        let generation = self.generation.load(Ordering::Acquire);
        let Some(record) = self.record.lock().ok().and_then(|record| record.clone()) else {
            return HeartbeatOutcome::Missing;
        };
        let Some(secrets) = self.secrets.lock().ok().and_then(|secrets| secrets.clone()) else {
            return HeartbeatOutcome::Missing;
        };
        if !record_has_valid_local_authorization(&record, Some(&secrets), Utc::now().timestamp()) {
            let _ = self.lock_authorization(
                record,
                "invalid",
                "本地授权已超过有效期和离线宽限期，请重新输入激活码".to_owned(),
                true,
            );
            return HeartbeatOutcome::Revoked;
        }
        let (result, retried_protocol) = self
            .heartbeat_with_protocol_retry(&secrets.device_id, &secrets.activate_code)
            .await;
        if self.generation.load(Ordering::Acquire) != generation {
            return HeartbeatOutcome::Missing;
        }
        match result {
            Ok(response) if heartbeat_requires_reactivation(response.revoked, &response.state) => {
                let _ = self.lock_authorization(
                    record,
                    "revoked",
                    reason_message(&response.reason),
                    true,
                );
                HeartbeatOutcome::Revoked
            }
            Ok(response) => {
                let mut updated_secrets = secrets;
                if let Some(token) = response.token.clone() {
                    updated_secrets.token = Some(token);
                }
                if self.persist_secrets(&updated_secrets).is_err() {
                    let _ = self.lock_authorization(
                        record,
                        "invalid",
                        "无法更新系统安全凭据，请重新激活".to_owned(),
                        false,
                    );
                    return HeartbeatOutcome::Revoked;
                }
                let active = self.active_record_from_heartbeat(record, &response);
                self.apply_heartbeat_side_effects(&response, &updated_secrets);
                if self.save_record(active).is_err() {
                    return HeartbeatOutcome::Retrying;
                }
                HeartbeatOutcome::Active
            }
            Err(error) => {
                let disposition = heartbeat_error_disposition(&error, retried_protocol);
                let (state, message, delete_secret) = error_state(&error, disposition);
                let _ = self.lock_authorization(record, state, message, delete_secret);
                match disposition {
                    HeartbeatErrorDisposition::Contract => HeartbeatOutcome::ContractError,
                    HeartbeatErrorDisposition::Reactivate => HeartbeatOutcome::Revoked,
                    HeartbeatErrorDisposition::Retry => HeartbeatOutcome::Retrying,
                }
            }
        }
    }

    /// 手动素材同步先刷新心跳版本信号，但不会改变客户端授权状态或阻塞
    /// 监听、录制和本地 AI 任务。
    pub async fn refresh_transition_catalog(&self, force: bool) -> Result<bool, String> {
        let record = self
            .record
            .lock()
            .map_err(|_| "激活状态锁已损坏".to_owned())?
            .clone();
        let secrets = self
            .secrets
            .lock()
            .map_err(|_| "激活凭据状态锁已损坏".to_owned())?
            .clone();
        let (_record, secrets) = record
            .zip(secrets)
            .filter(|(record, secrets)| {
                record_has_valid_local_authorization(record, Some(secrets), Utc::now().timestamp())
            })
            .ok_or_else(|| "尚未保存可用于同步素材的激活凭据".to_owned())?;
        let response = self
            .api
            .heartbeat(&secrets.device_id, &secrets.activate_code)
            .await
            .map_err(|error| error.safe_message())?;
        if response.revoked || !response.state.eq_ignore_ascii_case("ACTIVE") {
            return Err("当前激活状态不允许同步转场素材".to_owned());
        }
        let signal = response
            .transition_materials
            .filter(|signal| signal.is_valid())
            .ok_or_else(|| "服务端心跳未返回有效的转场素材版本".to_owned())?;
        let coordinator = self
            .transition_catalog()
            .ok_or_else(|| "素材目录同步服务尚未初始化".to_owned())?;
        let queued = coordinator.notify(signal, &secrets.device_id, &secrets.activate_code);
        if force && !queued {
            coordinator.retry().map_err(|error| error.to_string())?;
            return Ok(true);
        }
        Ok(queued)
    }

    pub fn credentials(&self) -> Option<(String, String)> {
        let record = self.record.lock().ok()?.clone()?;
        let secrets = self.secrets.lock().ok()?.clone()?;
        record_has_valid_local_authorization(&record, Some(&secrets), Utc::now().timestamp())
            .then_some((secrets.device_id, secrets.activate_code))
    }

    fn transition_catalog(&self) -> Option<TransitionCatalogCoordinator> {
        self.transition_catalog
            .lock()
            .ok()
            .and_then(|coordinator| coordinator.clone())
    }

    pub async fn clear(&self) -> Result<ActivationStateView, String> {
        let _operation = self.operation_lock.lock().await;
        self.generation.fetch_add(1, Ordering::AcqRel);
        let device_id_hint = device_hint(&self.device_id);
        let now = Utc::now();
        let mut record = ClientActivationRecord {
            device_id_hint,
            expire_at: None,
            grace_sec: None,
            server_time_offset_sec: 0,
            state: "invalid".to_owned(),
            allow_custom_api_key: false,
            last_heartbeat_at: None,
            next_heartbeat_at: None,
            last_error: Some("请输入激活码后继续使用".to_owned()),
            updated_at: now.to_rfc3339(),
        };
        if self.secret_store.delete().is_err() {
            record.last_error = Some("无法清除系统安全凭据，请重新提交激活码覆盖旧凭据".to_owned());
        }
        *self
            .secrets
            .lock()
            .map_err(|_| "激活凭据状态锁已损坏".to_owned())? = None;
        self.save_record(record)?;
        self.set_credential_error(None)?;
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

    fn persist_secrets(&self, secrets: &ActivationSecrets) -> Result<(), String> {
        self.secret_store
            .store(secrets)
            .map_err(|_| "无法写入系统安全凭据，请检查系统权限后重试".to_owned())?;
        let persisted = self
            .secret_store
            .load()
            .map_err(|_| "无法读取系统安全凭据，请检查系统权限后重试".to_owned())?;
        if persisted.as_ref() != Some(secrets) {
            return Err("系统安全凭据回读校验失败，请重试".to_owned());
        }
        *self
            .secrets
            .lock()
            .map_err(|_| "激活凭据状态锁已损坏".to_owned())? = persisted;
        self.set_credential_error(None)
    }

    fn set_credential_error(&self, value: Option<String>) -> Result<(), String> {
        *self
            .credential_error
            .lock()
            .map_err(|_| "激活凭据错误状态锁已损坏".to_owned())? = value;
        Ok(())
    }

    fn ensure_generation(&self, generation: u64) -> Result<(), String> {
        (self.generation.load(Ordering::Acquire) == generation)
            .then_some(())
            .ok_or_else(|| "本次激活已被新的授权操作替代".to_owned())
    }

    async fn heartbeat_with_protocol_retry(
        &self,
        device_id: &str,
        activate_code: &str,
    ) -> (Result<crate::api::HeartbeatResponse, ApiError>, bool) {
        let first = self.api.heartbeat(device_id, activate_code).await;
        if first
            .as_ref()
            .err()
            .and_then(ApiError::code)
            .is_some_and(|code| matches!(code, 9101..=9105))
        {
            return (self.api.heartbeat(device_id, activate_code).await, true);
        }
        (first, false)
    }

    async fn activate_with_protocol_retry(
        &self,
        device_id: &str,
        activate_code: &str,
    ) -> (Result<crate::api::ActivationResponse, ApiError>, bool) {
        let first = self.api.activate(device_id, activate_code).await;
        if first
            .as_ref()
            .err()
            .and_then(ApiError::code)
            .is_some_and(|code| matches!(code, 9101..=9105))
        {
            return (self.api.activate(device_id, activate_code).await, true);
        }
        (first, false)
    }

    fn active_record_from_heartbeat(
        &self,
        mut record: ClientActivationRecord,
        response: &crate::api::HeartbeatResponse,
    ) -> ClientActivationRecord {
        let now = Utc::now();
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
        record
    }

    fn apply_heartbeat_side_effects(
        &self,
        response: &crate::api::HeartbeatResponse,
        secrets: &ActivationSecrets,
    ) {
        self.recording_limit
            .apply(response.max_screen_limit, "heartbeat");
        let Some(signal) = response.transition_materials.clone() else {
            return;
        };
        if !signal.is_valid() {
            eprintln!(
                "{}",
                serde_json::json!({
                    "component": "transition_materials",
                    "event": "invalid_catalog_signal",
                })
            );
            return;
        }
        if let Some(coordinator) = self.transition_catalog() {
            coordinator.notify(signal, &secrets.device_id, &secrets.activate_code);
        }
    }

    fn lock_authorization(
        &self,
        mut record: ClientActivationRecord,
        state: &str,
        message: String,
        delete_secret: bool,
    ) -> Result<(), String> {
        let now = Utc::now();
        record.state = state.to_owned();
        record.last_error = Some(message);
        record.last_heartbeat_at = Some(now.to_rfc3339());
        record.next_heartbeat_at = (state == "retrying")
            .then(|| (now + heartbeat_duration(HEARTBEAT_RETRY_INTERVAL)).to_rfc3339());
        record.updated_at = now.to_rfc3339();
        if delete_secret {
            let _ = self.secret_store.delete();
            *self
                .secrets
                .lock()
                .map_err(|_| "激活凭据状态锁已损坏".to_owned())? = None;
        }
        self.save_record(record)
    }
}

type LoadedActivationSecrets = (Option<ActivationSecrets>, Option<String>, Option<String>);

fn load_and_migrate_secrets(
    database: &Database,
    secret_store: &dyn ActivationSecretStore,
) -> Result<LoadedActivationSecrets, String> {
    let legacy = database
        .legacy_client_activation()
        .map_err(|error| error.to_string())?;
    let stored = match secret_store.load() {
        Ok(stored) => stored,
        Err(_) => {
            return Ok((
                None,
                legacy.as_ref().map(|record| record.device_id.clone()),
                Some("无法读取系统安全凭据，请检查系统权限后重新激活".to_owned()),
            ));
        }
    };

    let Some(legacy) = legacy else {
        database
            .finalize_client_activation_secret_migration()
            .map_err(|error| error.to_string())?;
        return Ok((stored, None, None));
    };
    let legacy_secrets = ActivationSecrets {
        device_id: legacy.device_id.clone(),
        activate_code: legacy.activate_code,
        token: legacy.token,
    };
    if legacy_secrets.validate().is_err() {
        return Ok((
            stored,
            Some(legacy.device_id),
            Some("旧版授权凭据内容无效，请重新输入激活码".to_owned()),
        ));
    }

    if let Some(stored) = stored {
        if stored != legacy_secrets {
            return Ok((
                Some(stored),
                Some(legacy.device_id),
                Some("系统安全凭据与旧版授权记录不一致，请重新激活".to_owned()),
            ));
        }
        database
            .finalize_client_activation_secret_migration()
            .map_err(|error| error.to_string())?;
        return Ok((Some(legacy_secrets), None, None));
    }

    if secret_store.store(&legacy_secrets).is_err() {
        return Ok((
            None,
            Some(legacy.device_id),
            Some("无法迁移旧版授权凭据到系统安全存储，请检查系统权限".to_owned()),
        ));
    }
    match secret_store.load() {
        Ok(Some(persisted)) if persisted == legacy_secrets => {
            database
                .finalize_client_activation_secret_migration()
                .map_err(|error| error.to_string())?;
            Ok((Some(persisted), None, None))
        }
        _ => Ok((
            None,
            Some(legacy.device_id),
            Some("旧版授权凭据迁移回读校验失败，请重试".to_owned()),
        )),
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

fn empty_activation_record(device_id: &str) -> ClientActivationRecord {
    ClientActivationRecord {
        device_id_hint: device_hint(device_id),
        expire_at: None,
        grace_sec: None,
        server_time_offset_sec: 0,
        state: "missing".to_owned(),
        allow_custom_api_key: false,
        last_heartbeat_at: None,
        next_heartbeat_at: None,
        last_error: None,
        updated_at: Utc::now().to_rfc3339(),
    }
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
    secrets: Option<&ActivationSecrets>,
    device_id: &str,
    credential_error: Option<&str>,
) -> ActivationStateView {
    if let Some(message) = credential_error {
        return ActivationStateView {
            configured: record.is_some(),
            active: false,
            status: "invalid".to_owned(),
            message: Some(message.to_owned()),
            device_id_hint: record
                .map(|record| record.device_id_hint.clone())
                .unwrap_or_else(|| device_hint(device_id)),
            last_heartbeat_at: record.and_then(|record| record.last_heartbeat_at.clone()),
            next_heartbeat_at: None,
        };
    }
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
    let active = record_has_valid_local_authorization(record, secrets, Utc::now().timestamp());
    let has_token = secrets
        .and_then(|secrets| secrets.token.as_deref())
        .is_some_and(|token| !token.is_empty());
    let locally_expired =
        matches!(record.state.as_str(), "active" | "retrying") && has_token && !active;
    ActivationStateView {
        configured: true,
        active,
        status: if locally_expired {
            "invalid".to_owned()
        } else {
            record.state.clone()
        },
        message: if locally_expired {
            Some("本地授权已超过有效期和离线宽限期，请重新输入激活码".to_owned())
        } else {
            record.last_error.clone()
        },
        device_id_hint: record.device_id_hint.clone(),
        last_heartbeat_at: record.last_heartbeat_at.clone(),
        next_heartbeat_at: record.next_heartbeat_at.clone(),
    }
}

fn record_has_valid_local_authorization(
    record: &ClientActivationRecord,
    secrets: Option<&ActivationSecrets>,
    local_now_timestamp: i64,
) -> bool {
    if !matches!(record.state.as_str(), "active" | "retrying")
        || !secrets
            .and_then(|secrets| secrets.token.as_deref())
            .is_some_and(|token| !token.is_empty())
    {
        return false;
    }
    let (Some(expire_at), Some(grace_sec)) = (record.expire_at, record.grace_sec) else {
        return false;
    };
    let server_now = local_now_timestamp.saturating_add(record.server_time_offset_sec);
    let valid_until = expire_at.saturating_add(grace_sec.max(0));
    server_now <= valid_until
}

fn device_hint(device_id: &str) -> String {
    if device_id.chars().count() <= 8 {
        return "…本地设备".to_owned();
    }
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

fn heartbeat_requires_reactivation(revoked: bool, state: &str) -> bool {
    revoked || !state.trim().eq_ignore_ascii_case("ACTIVE")
}

#[derive(Clone)]
pub struct TelemetryQueue {
    sender: mpsc::Sender<TelemetryEvent>,
}

impl TelemetryQueue {
    pub fn spawn(service: ActivationService) -> Self {
        let (sender, mut receiver) = mpsc::channel(TELEMETRY_QUEUE_CAPACITY);
        let cancellation = service.cancellation();
        // Tauri 的 setup 回调在 macOS UI 主线程执行，那里没有当前 Tokio
        // reactor；使用 Tauri 全局运行时可安全地从同步初始化路径启动队列。
        tauri::async_runtime::spawn(async move {
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
    heartbeat_error_disposition(error, true) == HeartbeatErrorDisposition::Reactivate
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HeartbeatErrorDisposition {
    Contract,
    Reactivate,
    Retry,
}

fn heartbeat_error_disposition(
    error: &ApiError,
    protocol_already_retried: bool,
) -> HeartbeatErrorDisposition {
    match error.code() {
        Some(1001 | 2001) => HeartbeatErrorDisposition::Contract,
        Some(1002..=1006 | 2002..=2004) => HeartbeatErrorDisposition::Reactivate,
        Some(9101..=9105) if protocol_already_retried => HeartbeatErrorDisposition::Reactivate,
        Some(1007 | 9000 | 9101..=9105) | None => HeartbeatErrorDisposition::Retry,
        Some(_) => HeartbeatErrorDisposition::Retry,
    }
}

fn activation_error_disposition(
    error: &ApiError,
    protocol_already_retried: bool,
) -> HeartbeatErrorDisposition {
    match error.code() {
        Some(1001..=1006 | 2001..=2004) => HeartbeatErrorDisposition::Reactivate,
        Some(9101..=9105) if protocol_already_retried => HeartbeatErrorDisposition::Reactivate,
        Some(1007 | 9000 | 9101..=9105) | None => HeartbeatErrorDisposition::Retry,
        Some(_) => HeartbeatErrorDisposition::Retry,
    }
}

fn error_state(
    error: &ApiError,
    disposition: HeartbeatErrorDisposition,
) -> (&'static str, String, bool) {
    match disposition {
        HeartbeatErrorDisposition::Contract => (
            "contract_error",
            format!(
                "{}；客户端已发送标准和兼容请求头，请确认 Nginx 已启用 underscores_in_headers on",
                error.safe_message()
            ),
            true,
        ),
        HeartbeatErrorDisposition::Reactivate => ("invalid", error.safe_message(), true),
        HeartbeatErrorDisposition::Retry => ("retrying", error.safe_message(), false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::activation_secret::MemoryActivationSecretStore;
    use crate::api::{ApiConfig, PlainJsonCodec};

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
            device_id_hint: "…23456789".to_owned(),
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
        let view = state_from_record(Some(&record), None, "DY-123456789", None);
        assert!(!view.active);
        let json = serde_json::to_string(&view).unwrap();
        assert!(!json.contains("DY-123456789"));
    }

    #[test]
    fn local_authorization_expires_after_the_server_deadline_and_grace_period() {
        let record = ClientActivationRecord {
            device_id_hint: "…23456789".to_owned(),
            expire_at: Some(1_000),
            grace_sec: Some(300),
            server_time_offset_sec: 20,
            state: "retrying".to_owned(),
            allow_custom_api_key: false,
            last_heartbeat_at: None,
            next_heartbeat_at: None,
            last_error: Some("网络连接失败".to_owned()),
            updated_at: Utc::now().to_rfc3339(),
        };
        let secrets = ActivationSecrets {
            device_id: "DY-123456789".to_owned(),
            activate_code: "TEST-CODE".to_owned(),
            token: Some("TEST-TOKEN".to_owned()),
        };

        assert!(record_has_valid_local_authorization(
            &record,
            Some(&secrets),
            1_280
        ));
        assert!(!record_has_valid_local_authorization(
            &record,
            Some(&secrets),
            1_281
        ));
        let view = state_from_record(Some(&record), Some(&secrets), "DY-123456789", None);
        assert!(!view.active);
        assert_eq!(view.status, "invalid");
        assert!(view.message.unwrap().contains("离线宽限期"));
    }

    #[test]
    fn incomplete_or_empty_local_authorization_is_never_active() {
        let mut record = ClientActivationRecord {
            device_id_hint: "…23456789".to_owned(),
            expire_at: None,
            grace_sec: Some(300),
            server_time_offset_sec: 0,
            state: "active".to_owned(),
            allow_custom_api_key: false,
            last_heartbeat_at: None,
            next_heartbeat_at: None,
            last_error: None,
            updated_at: Utc::now().to_rfc3339(),
        };
        let mut secrets = ActivationSecrets {
            device_id: "DY-123456789".to_owned(),
            activate_code: "TEST-CODE".to_owned(),
            token: Some("TEST-TOKEN".to_owned()),
        };

        assert!(!record_has_valid_local_authorization(
            &record,
            Some(&secrets),
            1_000
        ));
        record.expire_at = Some(2_000);
        secrets.token = Some(String::new());
        assert!(!record_has_valid_local_authorization(
            &record,
            Some(&secrets),
            1_000
        ));
        assert!(!record_has_valid_local_authorization(&record, None, 1_000));
    }

    #[test]
    fn heartbeat_revoked_flag_or_non_active_state_requires_reactivation() {
        assert!(!heartbeat_requires_reactivation(false, "ACTIVE"));
        assert!(!heartbeat_requires_reactivation(false, " active "));
        assert!(heartbeat_requires_reactivation(true, "ACTIVE"));
        assert!(heartbeat_requires_reactivation(false, "INACTIVE"));
        assert!(heartbeat_requires_reactivation(false, "DISABLED"));
        assert!(heartbeat_requires_reactivation(false, "EXPIRED"));
    }

    #[test]
    fn heartbeat_business_codes_use_the_documented_disposition_matrix() {
        for code in [1001, 2001] {
            let error = ApiError::Business {
                code,
                message: "TEST-MESSAGE".to_owned(),
            };
            assert_eq!(
                heartbeat_error_disposition(&error, false),
                HeartbeatErrorDisposition::Contract
            );
        }
        for code in [1002, 1003, 1004, 1005, 1006, 2002, 2003, 2004] {
            let error = ApiError::Business {
                code,
                message: "TEST-MESSAGE".to_owned(),
            };
            assert_eq!(
                heartbeat_error_disposition(&error, false),
                HeartbeatErrorDisposition::Reactivate
            );
        }
        for code in [1007, 9000, 9101, 9102, 9103, 9104, 9105] {
            let error = ApiError::Business {
                code,
                message: "TEST-MESSAGE".to_owned(),
            };
            assert_eq!(
                heartbeat_error_disposition(&error, false),
                HeartbeatErrorDisposition::Retry
            );
        }
        for code in 9101..=9105 {
            let error = ApiError::Business {
                code,
                message: "TEST-MESSAGE".to_owned(),
            };
            assert_eq!(
                heartbeat_error_disposition(&error, true),
                HeartbeatErrorDisposition::Reactivate
            );
        }
    }

    #[test]
    fn activation_business_codes_use_the_documented_disposition_matrix() {
        for code in [1001, 1002, 1003, 1004, 1005, 1006, 2001, 2002, 2003, 2004] {
            let error = ApiError::Business {
                code,
                message: "TEST-MESSAGE".to_owned(),
            };
            assert_eq!(
                activation_error_disposition(&error, false),
                HeartbeatErrorDisposition::Reactivate
            );
        }
        for code in [1007, 9000, 9101, 9102, 9103, 9104, 9105] {
            let error = ApiError::Business {
                code,
                message: "TEST-MESSAGE".to_owned(),
            };
            assert_eq!(
                activation_error_disposition(&error, false),
                HeartbeatErrorDisposition::Retry
            );
        }
        for code in 9101..=9105 {
            let error = ApiError::Business {
                code,
                message: "TEST-MESSAGE".to_owned(),
            };
            assert_eq!(
                activation_error_disposition(&error, true),
                HeartbeatErrorDisposition::Reactivate
            );
        }
    }

    #[test]
    fn telemetry_queue_can_start_without_a_current_tokio_reactor() {
        assert!(tokio::runtime::Handle::try_current().is_err());
        let directory = tempfile::tempdir().unwrap();
        let database = Database::open(&directory.path().join("activation.sqlite3")).unwrap();
        database.migrate().unwrap();
        let api = ApiClient::new(
            ApiConfig {
                base_url: "http://127.0.0.1/api".to_owned(),
                timeout: Duration::from_millis(50),
                client_version: "test".to_owned(),
                platform: "mac".to_owned(),
            },
            Arc::new(PlainJsonCodec),
        )
        .unwrap();
        let service = ActivationService::new_with_secret_store(
            database,
            api,
            Path::new("/tmp/dy-screen-test"),
            Arc::new(MemoryActivationSecretStore::new()),
        )
        .unwrap();
        let state = service.state();
        assert!(!state.active);
        assert_eq!(state.status, "missing");

        let queue = TelemetryQueue::spawn(service.clone());
        queue.track("app_open", Map::new());
        service.shutdown();
    }
}
