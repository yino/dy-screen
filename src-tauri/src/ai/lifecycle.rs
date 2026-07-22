//! 应用启动恢复和显式退出时的 ASR 状态、子进程与临时文件治理。

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use dy_screen::asr::{AsrError, TranscriptionScheduler, cleanup_stale_audio_files};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::{AiRepository, AiRepositoryError, RecoverySummary};

#[derive(Debug, Error)]
pub enum AiLifecycleError {
    #[error(transparent)]
    Repository(#[from] AiRepositoryError),
    #[error(transparent)]
    Asr(#[from] AsrError),
}

/// 启动或退出恢复的可审计结果，不包含媒体路径和模型 stderr。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AiRecoveryReport {
    pub database: RecoverySummary,
    pub removed_temporary_audio_files: usize,
}

/// 统一管理 ASR 数据库状态与临时音频目录的生命周期边界。
#[derive(Clone)]
pub struct AiLifecycle {
    repository: AiRepository,
    temporary_root: PathBuf,
}

impl AiLifecycle {
    pub fn new(repository: AiRepository, temporary_root: PathBuf) -> Self {
        Self {
            repository,
            temporary_root,
        }
    }

    pub fn temporary_root(&self) -> &Path {
        &self.temporary_root
    }

    /// 异常重启后，不信任任何进程内状态：恢复输入、废弃半成品并递归清理临时 WAV/part。
    pub fn recover_startup(&self) -> Result<AiRecoveryReport, AiLifecycleError> {
        self.recover_after_processes_stopped()
    }

    /// 显式退出先取消调度器中的 FFmpeg/ASR 子进程，再执行与异常重启相同的持久化恢复。
    pub async fn shutdown(
        &self,
        scheduler: &TranscriptionScheduler,
    ) -> Result<AiRecoveryReport, AiLifecycleError> {
        scheduler.shutdown().await;
        self.recover_after_processes_stopped()
    }

    fn recover_after_processes_stopped(&self) -> Result<AiRecoveryReport, AiLifecycleError> {
        let database = self.repository.recover_interrupted()?;
        let removed_temporary_audio_files =
            cleanup_stale_audio_files(&self.temporary_root, &HashSet::new())?;
        Ok(AiRecoveryReport {
            database,
            removed_temporary_audio_files,
        })
    }
}
