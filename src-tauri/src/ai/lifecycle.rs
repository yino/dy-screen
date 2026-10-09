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
    /// 启动恢复时被**继续完成**的中断删除操作数量。
    ///
    /// 用户删除工程时若进程中断，工程会停在 `deleting` 状态且没人推进。
    /// 此前 `recover_deleting_projects` **没有任何调用点**（零调用），
    /// 于是这些工程会永久卡在 `deleting`。
    pub resumed_deletions: usize,
    /// 按设置保留期回收的特效字幕渲染缓存条目数。
    ///
    /// 此前 `prune_render_cache` 零调用，缓存只增不减。保留期 0 表示**不清理**。
    pub pruned_render_cache_entries: usize,
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
    ///
    /// 另外按设置回收过期渲染缓存。**只在启动时做**，不放在退出路径上——
    /// 退出不该因为磁盘回收而变慢。
    pub fn recover_startup(&self) -> Result<AiRecoveryReport, AiLifecycleError> {
        let mut report = self.recover_after_processes_stopped()?;
        report.pruned_render_cache_entries = self.prune_expired_render_cache()?;
        Ok(report)
    }

    /// 按「设置」里的保留期回收特效字幕渲染缓存。
    ///
    /// 保留期 **0 表示不清理**（用户显式选择保留全部缓存）。读取设置失败时
    /// 保守地**不清理**——宁可不回收，也不要因为读设置出错而删掉用户资产。
    pub fn prune_expired_render_cache(&self) -> Result<usize, AiLifecycleError> {
        let retention_days = match self.repository.render_cache_retention_days() {
            Ok(days) => days,
            Err(_) => return Ok(0),
        };
        if retention_days == 0 {
            return Ok(0);
        }
        let cutoff = (chrono::Utc::now() - chrono::Duration::days(i64::from(retention_days)))
            .to_rfc3339();
        Ok(
            super::effect_caption_cache::prune_render_cache(&self.repository, &cutoff)
                .unwrap_or(0),
        )
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
        // 继续完成中断的删除：工程若停在 `deleting`，说明删除过程被打断，
        // 这里把它删干净，避免留下永远无法清理的记录。
        let pending_deletions = self
            .repository
            .recover_deleting_projects()
            .map_err(AiLifecycleError::from)?;
        let mut resumed_deletions = 0_usize;
        for project_id in pending_deletions {
            if self.repository.delete_project(project_id).is_ok() {
                resumed_deletions += 1;
            }
        }
        let removed_temporary_audio_files =
            cleanup_stale_audio_files(&self.temporary_root, &HashSet::new())?;
        Ok(AiRecoveryReport {
            database,
            removed_temporary_audio_files,
            resumed_deletions,
            pruned_render_cache_entries: 0,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::Database;

    /// 用户删除工程时若进程中断，工程会停在 `deleting`。启动恢复必须**继续完成**，
    /// 否则这些工程永久卡在 `deleting`（`recover_deleting_projects` 此前零调用）。
    #[test]
    fn startup_recovery_resumes_interrupted_deletions() {
        let database = Database::open_in_memory().expect("in-memory database");
        database.migrate().expect("migrate to latest");
        let repository = AiRepository::new(database.clone());
        // 用 SQL 直造工程并直接置为 deleting，模拟"删除进行到一半被中断"。
        // 不用 `create_project` 是为了避免与其 profile 结构耦合。
        {
            let connection = database.connection().expect("connection");
            connection
                .execute(
                    "INSERT INTO ai_projects(id, name, status, recognition_profile_json,
                        recognition_profile_hash, input_frozen, progress_percent,
                        project_tags_json, created_at, updated_at)
                     VALUES(1, '待删除', 'deleting', '{}', 'profile', 0, 0, '[]', 'now', 'now')",
                    [],
                )
                .expect("seed deleting project");
        }

        let temporary_root = std::env::temp_dir().join("dy-screen-lifecycle-test");
        std::fs::create_dir_all(&temporary_root).expect("temp root");
        let lifecycle = AiLifecycle::new(repository.clone(), temporary_root);
        let report = lifecycle.recover_startup().expect("recover startup");

        assert_eq!(report.resumed_deletions, 1, "挂起的删除应被继续完成");
        let remaining: i64 = database
            .connection()
            .expect("connection")
            .query_row(
                "SELECT count(*) FROM ai_projects WHERE status = 'deleting'",
                [],
                |row| row.get(0),
            )
            .expect("count");
        assert_eq!(remaining, 0, "恢复后不应残留 deleting 工程");
    }

    /// 保留期语义：**0 = 不清理**；正数则回收早于该保留期的缓存。
    #[test]
    fn render_cache_retention_zero_disables_pruning() {
        let database = Database::open_in_memory().expect("in-memory database");
        database.migrate().expect("migrate to latest");
        let repository = AiRepository::new(database.clone());

        // 一条"很久以前"的 ready 缓存。frames_dir 用空串，避免测试删真实目录。
        let seed = |created_at: &str, key: &str| {
            database
                .connection()
                .expect("connection")
                .execute(
                    "INSERT INTO ai_effect_caption_renders(
                        project_id, candidate_id, cache_key, composition_version,
                        composition_fingerprint, font_version, browser_version,
                        renderer_version, engine_script_version, width, height, fps,
                        frame_count, frames_dir, status, created_at, updated_at)
                     VALUES(1, 1, ?1, 'v1', 'fp', 'f1', 'b1', 'r1', 's1', 1080, 1920, 30,
                        90, '', 'ready', ?2, ?2)",
                    rusqlite::params![key, created_at],
                )
                .expect("seed render row");
        };
        let count = || -> i64 {
            database
                .connection()
                .expect("connection")
                .query_row(
                    "SELECT count(*) FROM ai_effect_caption_renders",
                    [],
                    |row| row.get(0),
                )
                .expect("count")
        };

        seed("2000-01-01T00:00:00+00:00", "old-key");
        assert_eq!(count(), 1);

        let temporary_root = std::env::temp_dir().join("dy-screen-lifecycle-prune-test");
        std::fs::create_dir_all(&temporary_root).expect("temp root");
        let lifecycle = AiLifecycle::new(repository.clone(), temporary_root);

        // 保留期 0：显式选择"不清理"，必须一条都不动。
        {
            let mut settings = database.get_settings().expect("settings");
            settings.render_cache_retention_days = 0;
            database.save_settings(&settings).expect("save settings");
        }
        assert_eq!(
            lifecycle.prune_expired_render_cache().expect("prune"),
            0,
            "保留期 0 表示不清理"
        );
        assert_eq!(count(), 1, "保留期 0 时缓存必须原样保留");

        // 保留期 3 天：那条 2000 年的记录应被回收。
        {
            let mut settings = database.get_settings().expect("settings");
            settings.render_cache_retention_days = 3;
            database.save_settings(&settings).expect("save settings");
        }
        assert_eq!(
            lifecycle.prune_expired_render_cache().expect("prune"),
            1,
            "超出保留期的缓存应被回收"
        );
        assert_eq!(count(), 0);
    }
}
