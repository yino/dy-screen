use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use chrono::{SecondsFormat, Utc};
use fs2::FileExt;
use serde_json::json;
use thiserror::Error;

const INSTANCE_LOCK_FILE: &str = "dy-screen.lock";

#[derive(Debug, Error)]
pub enum InstanceLockError {
    #[error("切片智能体已在运行")]
    AlreadyRunning,
    #[error("无法获取切片智能体实例锁")]
    Io(#[source] io::Error),
}

pub struct InstanceLock {
    file: File,
}

impl InstanceLock {
    pub fn acquire(app_data_dir: &Path) -> Result<Self, InstanceLockError> {
        std::fs::create_dir_all(app_data_dir).map_err(InstanceLockError::Io)?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(app_data_dir.join(INSTANCE_LOCK_FILE))
            .map_err(InstanceLockError::Io)?;
        match FileExt::try_lock_exclusive(&file) {
            Ok(()) => Ok(Self { file }),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                Err(InstanceLockError::AlreadyRunning)
            }
            Err(error) => Err(InstanceLockError::Io(error)),
        }
    }
}

impl Drop for InstanceLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

#[derive(Clone, Default)]
pub struct ShutdownGate {
    started: Arc<AtomicBool>,
}

impl ShutdownGate {
    pub fn try_begin(&self) -> bool {
        !self.started.swap(true, Ordering::SeqCst)
    }

    pub fn is_started(&self) -> bool {
        self.started.load(Ordering::SeqCst)
    }
}

#[derive(Clone, Copy)]
pub enum ShutdownReason {
    UserRequest,
    TauriExitRequested,
    Tray,
    Sigterm,
    Sigint,
}

impl ShutdownReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::UserRequest => "user_request",
            Self::TauriExitRequested => "tauri_exit_requested",
            Self::Tray => "tray",
            Self::Sigterm => "sigterm",
            Self::Sigint => "sigint",
        }
    }
}

#[derive(Clone, Copy)]
pub enum LifecycleEvent {
    InstanceLockAcquired,
    InstanceRejected,
    SignalRegistrationFailed,
    ShutdownStarted,
    ShutdownDeduplicated,
    ShutdownCompleted,
    ShutdownTimedOut,
}

impl LifecycleEvent {
    fn as_str(self) -> &'static str {
        match self {
            Self::InstanceLockAcquired => "instance_lock_acquired",
            Self::InstanceRejected => "instance_rejected",
            Self::SignalRegistrationFailed => "signal_registration_failed",
            Self::ShutdownStarted => "shutdown_started",
            Self::ShutdownDeduplicated => "shutdown_deduplicated",
            Self::ShutdownCompleted => "shutdown_completed",
            Self::ShutdownTimedOut => "shutdown_timed_out",
        }
    }
}

pub fn log_lifecycle(event: LifecycleEvent, reason: Option<ShutdownReason>) {
    log_lifecycle_detail(event, reason, None);
}

/// 在既有生命周期日志上追加一个可选的静态分类字段 `detail`。
///
/// `detail` 只承载预先枚举的安全分类（例如实例锁失败的 `ErrorKind`），不写入路径、
/// 原始错误文案、密钥或报文，因此无需再做脱敏。未提供 `detail` 时输出与旧版本逐字节一致。
pub fn log_lifecycle_detail(
    event: LifecycleEvent,
    reason: Option<ShutdownReason>,
    detail: Option<&str>,
) {
    eprintln!("{}", lifecycle_payload(event, reason, detail));
}

fn lifecycle_payload(
    event: LifecycleEvent,
    reason: Option<ShutdownReason>,
    detail: Option<&str>,
) -> serde_json::Value {
    let mut payload = json!({
        "timestamp": Utc::now().to_rfc3339_opts(SecondsFormat::Micros, true),
        "component": "lifecycle",
        "event": event.as_str(),
        "reason": reason.map(ShutdownReason::as_str),
    });
    if let (Some(detail), Some(object)) = (detail, payload.as_object_mut()) {
        object.insert("detail".to_owned(), json!(detail));
    }
    payload
}

/// 把实例锁失败归类为稳定字符串，用于区分“已有实例在运行”和“锁文件本身不可用”。
pub fn instance_lock_failure_detail(error: &InstanceLockError) -> &'static str {
    match error {
        InstanceLockError::AlreadyRunning => "already_running",
        InstanceLockError::Io(source) => match source.kind() {
            io::ErrorKind::PermissionDenied => "io_permission_denied",
            io::ErrorKind::NotFound => "io_not_found",
            io::ErrorKind::WouldBlock => "io_would_block",
            io::ErrorKind::StorageFull => "io_storage_full",
            _ => "io_other",
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn detail_of(payload: &serde_json::Value) -> Option<&str> {
        payload.get("detail").and_then(serde_json::Value::as_str)
    }

    #[test]
    fn lifecycle_payload_keeps_the_legacy_shape_without_detail() {
        let payload = lifecycle_payload(LifecycleEvent::InstanceLockAcquired, None, None);
        assert_eq!(detail_of(&payload), None, "未提供 detail 时不应新增字段");
        assert_eq!(
            payload.get("event").and_then(serde_json::Value::as_str),
            Some("instance_lock_acquired")
        );
        assert_eq!(
            payload.get("component").and_then(serde_json::Value::as_str),
            Some("lifecycle")
        );
        assert!(
            payload
                .get("timestamp")
                .and_then(serde_json::Value::as_str)
                .is_some()
        );
    }

    #[test]
    fn lifecycle_payload_attaches_static_detail_when_present() {
        let payload = lifecycle_payload(
            LifecycleEvent::InstanceRejected,
            None,
            Some("io_permission_denied"),
        );
        assert_eq!(detail_of(&payload), Some("io_permission_denied"));
        assert_eq!(
            payload.get("event").and_then(serde_json::Value::as_str),
            Some("instance_rejected")
        );
    }

    #[test]
    fn instance_lock_failure_detail_separates_busy_from_unusable_lock() {
        assert_eq!(
            instance_lock_failure_detail(&InstanceLockError::AlreadyRunning),
            "already_running"
        );
        for (kind, expected) in [
            (io::ErrorKind::PermissionDenied, "io_permission_denied"),
            (io::ErrorKind::NotFound, "io_not_found"),
            (io::ErrorKind::WouldBlock, "io_would_block"),
            (io::ErrorKind::StorageFull, "io_storage_full"),
            (io::ErrorKind::TimedOut, "io_other"),
        ] {
            assert_eq!(
                instance_lock_failure_detail(&InstanceLockError::Io(io::Error::from(kind))),
                expected,
                "ErrorKind {kind:?} 必须归类为稳定字符串"
            );
        }
    }
}
