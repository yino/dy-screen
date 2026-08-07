//! 客户端授权秘密的本地存储边界。
//!
//! 完整设备号、激活码和离线令牌只能通过本模块进出。生产实现使用独立的
//! SQLite 表，避免这些字段出现在前端状态、事件或日志中。

use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::database::{ClientActivationSecretsRecord, Database};

const MAX_SECRET_BYTES: usize = 16 * 1024;

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ActivationSecrets {
    pub device_id: String,
    pub activate_code: String,
    pub token: Option<String>,
}

impl ActivationSecrets {
    pub fn validate(&self) -> Result<(), ActivationSecretError> {
        validate_secret_field(&self.device_id, 256)?;
        validate_secret_field(&self.activate_code, 512)?;
        if let Some(token) = &self.token {
            validate_secret_field(token, MAX_SECRET_BYTES)?;
        }
        Ok(())
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ActivationSecretError {
    #[error("本地激活凭据存储不可用")]
    Unavailable,
    #[error("本地激活凭据内容无效")]
    Invalid,
    #[error("本地激活凭据操作失败")]
    Operation,
}

pub trait ActivationSecretStore: Send + Sync {
    fn load(&self) -> Result<Option<ActivationSecrets>, ActivationSecretError>;
    fn store(&self, secrets: &ActivationSecrets) -> Result<(), ActivationSecretError>;
    fn delete(&self) -> Result<(), ActivationSecretError>;
}

#[derive(Clone, Default)]
pub struct MemoryActivationSecretStore {
    state: Arc<Mutex<MemoryStoreState>>,
}

#[derive(Default)]
struct MemoryStoreState {
    value: Option<ActivationSecrets>,
    fail_load: bool,
    fail_store: bool,
    fail_delete: bool,
}

impl MemoryActivationSecretStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn fail_load(&self, value: bool) {
        if let Ok(mut state) = self.state.lock() {
            state.fail_load = value;
        }
    }

    pub fn fail_store(&self, value: bool) {
        if let Ok(mut state) = self.state.lock() {
            state.fail_store = value;
        }
    }

    pub fn fail_delete(&self, value: bool) {
        if let Ok(mut state) = self.state.lock() {
            state.fail_delete = value;
        }
    }
}

impl ActivationSecretStore for MemoryActivationSecretStore {
    fn load(&self) -> Result<Option<ActivationSecrets>, ActivationSecretError> {
        let state = self
            .state
            .lock()
            .map_err(|_| ActivationSecretError::Operation)?;
        if state.fail_load {
            return Err(ActivationSecretError::Operation);
        }
        Ok(state.value.clone())
    }

    fn store(&self, secrets: &ActivationSecrets) -> Result<(), ActivationSecretError> {
        secrets.validate()?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| ActivationSecretError::Operation)?;
        if state.fail_store {
            return Err(ActivationSecretError::Operation);
        }
        state.value = Some(secrets.clone());
        Ok(())
    }

    fn delete(&self) -> Result<(), ActivationSecretError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| ActivationSecretError::Operation)?;
        if state.fail_delete {
            return Err(ActivationSecretError::Operation);
        }
        state.value = None;
        Ok(())
    }
}

#[derive(Clone)]
pub struct SqliteActivationSecretStore {
    database: Database,
}

impl SqliteActivationSecretStore {
    pub fn new(database: Database) -> Self {
        Self { database }
    }
}

impl ActivationSecretStore for SqliteActivationSecretStore {
    fn load(&self) -> Result<Option<ActivationSecrets>, ActivationSecretError> {
        let record = self
            .database
            .client_activation_secrets()
            .map_err(|_| ActivationSecretError::Operation)?;
        record
            .map(|record| {
                let secrets = ActivationSecrets {
                    device_id: record.device_id,
                    activate_code: record.activate_code,
                    token: record.token,
                };
                secrets.validate()?;
                Ok(secrets)
            })
            .transpose()
    }

    fn store(&self, secrets: &ActivationSecrets) -> Result<(), ActivationSecretError> {
        secrets.validate()?;
        self.database
            .save_client_activation_secrets(&ClientActivationSecretsRecord {
                device_id: secrets.device_id.clone(),
                activate_code: secrets.activate_code.clone(),
                token: secrets.token.clone(),
            })
            .map_err(|_| ActivationSecretError::Operation)
    }

    fn delete(&self) -> Result<(), ActivationSecretError> {
        self.database
            .clear_client_activation_secrets()
            .map_err(|_| ActivationSecretError::Operation)
    }
}

fn validate_secret_field(value: &str, max_bytes: usize) -> Result<(), ActivationSecretError> {
    if value.is_empty() || value.len() > max_bytes || value.chars().any(char::is_control) {
        return Err(ActivationSecretError::Invalid);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secrets() -> ActivationSecrets {
        ActivationSecrets {
            device_id: "DY-TEST-DEVICE".to_owned(),
            activate_code: "TEST-ACTIVATION-CODE".to_owned(),
            token: Some("TEST-OFFLINE-TOKEN".to_owned()),
        }
    }

    #[test]
    fn memory_store_supports_replace_and_idempotent_delete() {
        let store = MemoryActivationSecretStore::new();
        assert!(store.load().unwrap().is_none());
        store.store(&secrets()).unwrap();
        assert!(store.load().unwrap().is_some());
        store.delete().unwrap();
        store.delete().unwrap();
        assert!(store.load().unwrap().is_none());
    }

    #[test]
    fn memory_store_exposes_failures_without_losing_existing_secret() {
        let store = MemoryActivationSecretStore::new();
        store.store(&secrets()).unwrap();
        store.fail_store(true);
        assert_eq!(
            store.store(&ActivationSecrets {
                token: Some("REPLACEMENT-TOKEN".to_owned()),
                ..secrets()
            }),
            Err(ActivationSecretError::Operation)
        );
        store.fail_store(false);
        assert!(store.load().unwrap().as_ref() == Some(&secrets()));
    }
}
