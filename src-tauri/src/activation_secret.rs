//! 客户端授权秘密的系统安全存储边界。
//!
//! 完整设备号、激活码和离线令牌只能通过本模块进出。生产实现使用
//! macOS Keychain 或 Windows Credential Manager，绝不回退到 SQLite 或明文文件。

use std::path::Path;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

const SERVICE_NAME: &str = "com.yino.clip-agent.activation";
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
    #[error("系统安全凭据存储不可用")]
    Unavailable,
    #[error("系统安全凭据内容无效")]
    Invalid,
    #[error("系统安全凭据操作失败")]
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
pub struct SystemActivationSecretStore {
    service: String,
    account: String,
}

impl SystemActivationSecretStore {
    pub fn for_app_data_dir(app_data_dir: &Path) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(b"clip-agent-activation-account-v1\0");
        hasher.update(app_data_dir.to_string_lossy().as_bytes());
        let digest = hex::encode(hasher.finalize());
        Self {
            service: SERVICE_NAME.to_owned(),
            account: format!("primary-{}", &digest[..16]),
        }
    }

    fn encode(secrets: &ActivationSecrets) -> Result<Vec<u8>, ActivationSecretError> {
        secrets.validate()?;
        let payload = serde_json::to_vec(secrets).map_err(|_| ActivationSecretError::Invalid)?;
        if payload.len() > MAX_SECRET_BYTES {
            return Err(ActivationSecretError::Invalid);
        }
        Ok(payload)
    }

    fn decode(payload: &[u8]) -> Result<ActivationSecrets, ActivationSecretError> {
        if payload.is_empty() || payload.len() > MAX_SECRET_BYTES {
            return Err(ActivationSecretError::Invalid);
        }
        let secrets = serde_json::from_slice::<ActivationSecrets>(payload)
            .map_err(|_| ActivationSecretError::Invalid)?;
        secrets.validate()?;
        Ok(secrets)
    }

    #[cfg(target_os = "windows")]
    fn windows_target(&self) -> Vec<u16> {
        format!("{}.{}", self.service, self.account)
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect()
    }

    #[cfg(target_os = "windows")]
    fn windows_account(&self) -> Vec<u16> {
        self.account
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect()
    }
}

impl ActivationSecretStore for SystemActivationSecretStore {
    fn load(&self) -> Result<Option<ActivationSecrets>, ActivationSecretError> {
        #[cfg(target_os = "macos")]
        {
            use security_framework::passwords::get_generic_password;
            use security_framework_sys::base::errSecItemNotFound;

            match get_generic_password(&self.service, &self.account) {
                Ok(payload) => Self::decode(&payload).map(Some),
                Err(error) if error.code() == errSecItemNotFound => Ok(None),
                Err(_) => Err(ActivationSecretError::Operation),
            }
        }
        #[cfg(target_os = "windows")]
        {
            use std::ptr::null_mut;
            use std::slice;
            use windows_sys::Win32::Foundation::{ERROR_NOT_FOUND, GetLastError};
            use windows_sys::Win32::Security::Credentials::{
                CRED_TYPE_GENERIC, CREDENTIALW, CredFree, CredReadW,
            };

            let target = self.windows_target();
            let mut credential: *mut CREDENTIALW = null_mut();
            if unsafe { CredReadW(target.as_ptr(), CRED_TYPE_GENERIC, 0, &mut credential) } == 0 {
                return match unsafe { GetLastError() } {
                    ERROR_NOT_FOUND => Ok(None),
                    _ => Err(ActivationSecretError::Operation),
                };
            }
            if credential.is_null() {
                return Err(ActivationSecretError::Operation);
            }
            let result = unsafe {
                let credential = &*credential;
                if credential.CredentialBlobSize == 0 || credential.CredentialBlob.is_null() {
                    Err(ActivationSecretError::Invalid)
                } else {
                    let payload = slice::from_raw_parts(
                        credential.CredentialBlob,
                        credential.CredentialBlobSize as usize,
                    );
                    Self::decode(payload).map(Some)
                }
            };
            unsafe { CredFree(credential.cast()) };
            return result;
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
            Err(ActivationSecretError::Unavailable)
        }
    }

    fn store(&self, secrets: &ActivationSecrets) -> Result<(), ActivationSecretError> {
        let payload = Self::encode(secrets)?;
        #[cfg(target_os = "macos")]
        {
            use security_framework::passwords::set_generic_password;

            set_generic_password(&self.service, &self.account, &payload)
                .map_err(|_| ActivationSecretError::Operation)
        }
        #[cfg(target_os = "windows")]
        {
            use std::ptr::null_mut;
            use windows_sys::Win32::Security::Credentials::{
                CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC, CREDENTIALW, CredWriteW,
            };

            let mut payload = payload;
            let mut target = self.windows_target();
            let mut account = self.windows_account();
            let credential = CREDENTIALW {
                Type: CRED_TYPE_GENERIC,
                TargetName: target.as_mut_ptr(),
                CredentialBlobSize: payload.len() as u32,
                CredentialBlob: payload.as_mut_ptr(),
                Persist: CRED_PERSIST_LOCAL_MACHINE,
                UserName: account.as_mut_ptr(),
                Comment: null_mut(),
                TargetAlias: null_mut(),
                Attributes: null_mut(),
                ..CREDENTIALW::default()
            };
            return if unsafe { CredWriteW(&credential, 0) } == 0 {
                Err(ActivationSecretError::Operation)
            } else {
                Ok(())
            };
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
            let _ = payload;
            Err(ActivationSecretError::Unavailable)
        }
    }

    fn delete(&self) -> Result<(), ActivationSecretError> {
        #[cfg(target_os = "macos")]
        {
            use security_framework::passwords::delete_generic_password;
            use security_framework_sys::base::errSecItemNotFound;

            match delete_generic_password(&self.service, &self.account) {
                Ok(()) => Ok(()),
                Err(error) if error.code() == errSecItemNotFound => Ok(()),
                Err(_) => Err(ActivationSecretError::Operation),
            }
        }
        #[cfg(target_os = "windows")]
        {
            use windows_sys::Win32::Foundation::{ERROR_NOT_FOUND, GetLastError};
            use windows_sys::Win32::Security::Credentials::{CRED_TYPE_GENERIC, CredDeleteW};

            let target = self.windows_target();
            if unsafe { CredDeleteW(target.as_ptr(), CRED_TYPE_GENERIC, 0) } != 0 {
                return Ok(());
            }
            return match unsafe { GetLastError() } {
                ERROR_NOT_FOUND => Ok(()),
                _ => Err(ActivationSecretError::Operation),
            };
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
            Err(ActivationSecretError::Unavailable)
        }
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
