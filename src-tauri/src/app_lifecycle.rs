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
    #[error("直播管家已在运行")]
    AlreadyRunning,
    #[error("无法获取直播管家实例锁")]
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
    eprintln!(
        "{}",
        json!({
            "timestamp": Utc::now().to_rfc3339_opts(SecondsFormat::Micros, true),
            "component": "lifecycle",
            "event": event.as_str(),
            "reason": reason.map(ShutdownReason::as_str),
        })
    );
}
