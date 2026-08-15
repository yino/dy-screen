use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use chrono::Utc;
use dy_screen::runtime_resources::{ResourceStatus, ResourceStatusSnapshot};
use rusqlite::{Connection, ErrorCode, OptionalExtension, Transaction, params, params_from_iter};
use thiserror::Error;

use crate::domain::{
    AppSettings, Dashboard, DiscoveryBinding, NewStreamer, NewVideo, RecordingSession, Streamer,
    StreamerPromptContext, StreamerSourceKind, StreamerTag, StreamerTagInput,
    StreamerTagValidationError, Video, VideoFilter, VideoPage, normalize_streamer_tags,
    streamer_tag_name_key,
};

#[derive(Debug, Error)]
pub enum DatabaseError {
    #[error("数据库错误：{0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("文件系统错误：{0}")]
    Io(#[from] std::io::Error),
    #[error("该直播间已经存在")]
    DuplicateStreamer,
    #[error("该个人主页已经存在")]
    DuplicateProfile,
    #[error("该稳定直播入口已经存在")]
    DuplicateWebRid,
    #[error("找不到记录：{0}")]
    NotFound(&'static str),
    #[error("数据库锁已损坏")]
    Poisoned,
    #[error("数据库迁移完整性检查失败：{0}")]
    MigrationIntegrity(String),
    #[error("录制优先级调整方向无效")]
    InvalidPriorityDirection,
    #[error("{0}")]
    TagValidation(#[from] StreamerTagValidationError),
}

pub type Result<T> = std::result::Result<T, DatabaseError>;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeResourceRecord {
    pub platform: String,
    pub app_min_version: String,
    pub bundle_version: Option<String>,
    pub status: ResourceStatus,
    pub progress_bytes: u64,
    pub total_bytes: u64,
    pub manifest_sha256: Option<String>,
    pub current_component: Option<String>,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub updated_at: String,
}

/// 仅供 Rust 授权服务使用的本地记录。该类型不实现 Serialize，避免激活码或
/// 令牌被 Tauri command、事件或日志意外输出到前端。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientActivationRecord {
    pub device_id_hint: String,
    pub expire_at: Option<i64>,
    pub grace_sec: Option<i64>,
    pub server_time_offset_sec: i64,
    pub state: String,
    pub allow_custom_api_key: bool,
    pub last_heartbeat_at: Option<String>,
    pub next_heartbeat_at: Option<String>,
    pub last_error: Option<String>,
    pub updated_at: String,
}

/// 仅供 Rust 授权服务使用的本地凭据记录。该类型不实现 Serialize，避免完整
/// 设备号、激活码或令牌被 Tauri command、事件或日志意外输出到前端。
#[derive(Clone, PartialEq, Eq)]
pub struct ClientActivationSecretsRecord {
    pub device_id: String,
    pub activate_code: String,
    pub token: Option<String>,
}

/// 仅用于把 v10 明文授权记录迁移到独立的本地凭据表。完成迁移后旧表会被删除。
#[derive(Clone, PartialEq, Eq)]
pub struct LegacyClientActivationRecord {
    pub device_id: String,
    pub activate_code: String,
    pub token: Option<String>,
}

#[derive(Clone)]
pub struct Database {
    connection: Arc<Mutex<Connection>>,
}

struct SessionManifestScope {
    id: i64,
    output_root: String,
    started_at: String,
    ended_at: Option<String>,
    room_id: Option<String>,
}

impl Database {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let connection = Connection::open(path)?;
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
        })
    }

    pub fn open_in_memory() -> Result<Self> {
        let connection = Connection::open_in_memory()?;
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
        })
    }

    pub(crate) fn connection(&self) -> Result<MutexGuard<'_, Connection>> {
        self.connection.lock().map_err(|_| DatabaseError::Poisoned)
    }

    pub fn migrate(&self) -> Result<()> {
        let mut connection = self.connection()?;
        connection.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS schema_migrations (
                version INTEGER PRIMARY KEY,
                applied_at TEXT NOT NULL
            );
            "#,
        )?;
        let applied = connection
            .query_row(
                "SELECT 1 FROM schema_migrations WHERE version = 1",
                [],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if !applied {
            let transaction = connection.transaction()?;
            transaction.execute_batch(
                r#"
                CREATE TABLE streamers (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    name TEXT NOT NULL,
                    room_url TEXT NOT NULL,
                    room_id TEXT NOT NULL UNIQUE,
                    monitor_enabled INTEGER NOT NULL DEFAULT 1,
                    archived INTEGER NOT NULL DEFAULT 0,
                    live_status TEXT NOT NULL DEFAULT 'checking',
                    monitor_status TEXT NOT NULL DEFAULT 'waiting',
                    last_checked_at TEXT,
                    last_error TEXT,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL
                );

                CREATE TABLE recording_sessions (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    streamer_id INTEGER NOT NULL REFERENCES streamers(id),
                    started_at TEXT NOT NULL,
                    ended_at TEXT,
                    status TEXT NOT NULL,
                    retry_count INTEGER NOT NULL DEFAULT 0,
                    output_root TEXT NOT NULL,
                    error TEXT
                );

                CREATE TABLE videos (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    session_id INTEGER NOT NULL REFERENCES recording_sessions(id),
                    path TEXT NOT NULL UNIQUE,
                    started_at TEXT,
                    ended_at TEXT,
                    duration_seconds INTEGER,
                    size_bytes INTEGER NOT NULL DEFAULT 0,
                    audio_present INTEGER,
                    status TEXT NOT NULL DEFAULT 'complete',
                    created_at TEXT NOT NULL
                );

                CREATE TABLE settings (
                    key TEXT PRIMARY KEY,
                    value TEXT NOT NULL
                );

                CREATE INDEX idx_sessions_streamer ON recording_sessions(streamer_id, started_at DESC);
                CREATE INDEX idx_videos_session ON videos(session_id, created_at DESC);
                "#,
            )?;
            transaction.execute(
                "INSERT INTO schema_migrations(version, applied_at) VALUES(1, ?1)",
                [Utc::now().to_rfc3339()],
            )?;
            transaction.commit()?;
        }

        let applied = connection
            .query_row(
                "SELECT 1 FROM schema_migrations WHERE version = 2",
                [],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if !applied {
            connection.pragma_update(None, "foreign_keys", "OFF")?;
            let migration = migrate_streamers_v2(&mut connection);
            let restore_foreign_keys = connection.pragma_update(None, "foreign_keys", "ON");
            migration?;
            restore_foreign_keys?;

            let violations: i64 = connection.query_row(
                "SELECT COUNT(*) FROM pragma_foreign_key_check",
                [],
                |row| row.get(0),
            )?;
            if violations != 0 {
                return Err(DatabaseError::MigrationIntegrity(format!(
                    "发现 {violations} 条外键异常"
                )));
            }
        }
        let applied = connection
            .query_row(
                "SELECT 1 FROM schema_migrations WHERE version = 3",
                [],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if !applied {
            migrate_streamer_tags_v3(&mut connection)?;
        }
        crate::ai::migrate_ai_v4(&mut connection)?;
        crate::ai::migrate_ai_v7(&mut connection)?;
        crate::ai::migrate_ai_v9(&mut connection)?;
        crate::ai::migrate_ai_v12(&mut connection)?;
        crate::ai::migrate_ai_v13(&mut connection)?;
        crate::ai::migrate_ai_v14(&mut connection)?;
        migrate_ai_replay_directory_v15(&mut connection)?;
        crate::ai::migrate_ai_v16(&mut connection)?;
        migrate_recording_priority_v17(&mut connection)?;
        crate::transition_materials::migrate_transition_materials_v18(&mut connection)?;
        crate::ai::migrate_ai_v21(&mut connection)?;
        crate::ai::migrate_ai_v22(&mut connection)?;
        crate::ai::migrate_ai_v23(&mut connection)?;
        crate::ai::migrate_ai_v24(&mut connection)?;

        let applied = connection
            .query_row(
                "SELECT 1 FROM schema_migrations WHERE version = 5",
                [],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if !applied {
            migrate_streamer_diagnostics_v5(&mut connection)?;
        }
        let applied = connection
            .query_row(
                "SELECT 1 FROM schema_migrations WHERE version = 8",
                [],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if !applied {
            migrate_runtime_resources_v8(&mut connection)?;
        }
        let applied = connection
            .query_row(
                "SELECT 1 FROM schema_migrations WHERE version = 10",
                [],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        let activation_table_exists = connection
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'client_activation'",
                [],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if !applied || !activation_table_exists {
            migrate_client_activation_v10(&mut connection)?;
        }
        migrate_client_activation_v19(&mut connection)?;
        migrate_client_activation_secrets_v20(&mut connection)?;
        drop(connection);
        self.ensure_default_settings()
    }

    pub fn client_activation(&self) -> Result<Option<ClientActivationRecord>> {
        self.connection()?
            .query_row(
                r#"SELECT device_id_hint, expire_at, grace_sec, server_time_offset_sec,
                          state, allow_custom_api_key, last_heartbeat_at,
                          next_heartbeat_at, last_error, updated_at
                   FROM client_activation WHERE id = 1"#,
                [],
                |row| {
                    Ok(ClientActivationRecord {
                        device_id_hint: row.get(0)?,
                        expire_at: row.get(1)?,
                        grace_sec: row.get(2)?,
                        server_time_offset_sec: row.get(3)?,
                        state: row.get(4)?,
                        allow_custom_api_key: row.get::<_, i64>(5)? == 1,
                        last_heartbeat_at: row.get(6)?,
                        next_heartbeat_at: row.get(7)?,
                        last_error: row.get(8)?,
                        updated_at: row.get(9)?,
                    })
                },
            )
            .optional()
            .map_err(DatabaseError::from)
    }

    pub fn save_client_activation(&self, record: &ClientActivationRecord) -> Result<()> {
        self.connection()?.execute(
            r#"INSERT INTO client_activation(
                    id, device_id_hint, expire_at, grace_sec, server_time_offset_sec,
                    state, allow_custom_api_key, last_heartbeat_at,
                    next_heartbeat_at, last_error, updated_at
                ) VALUES(1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                ON CONFLICT(id) DO UPDATE SET
                    device_id_hint=excluded.device_id_hint,
                    expire_at=excluded.expire_at,
                    grace_sec=excluded.grace_sec,
                    server_time_offset_sec=excluded.server_time_offset_sec,
                    state=excluded.state,
                    allow_custom_api_key=excluded.allow_custom_api_key,
                    last_heartbeat_at=excluded.last_heartbeat_at,
                    next_heartbeat_at=excluded.next_heartbeat_at,
                    last_error=excluded.last_error,
                    updated_at=excluded.updated_at"#,
            params![
                record.device_id_hint,
                record.expire_at,
                record.grace_sec,
                record.server_time_offset_sec,
                record.state,
                i64::from(record.allow_custom_api_key),
                record.last_heartbeat_at,
                record.next_heartbeat_at,
                record.last_error,
                record.updated_at,
            ],
        )?;
        Ok(())
    }

    pub fn client_activation_secrets(&self) -> Result<Option<ClientActivationSecretsRecord>> {
        self.connection()?
            .query_row(
                r#"SELECT device_id, activate_code, token
                   FROM client_activation_secrets WHERE id = 1"#,
                [],
                |row| {
                    Ok(ClientActivationSecretsRecord {
                        device_id: row.get(0)?,
                        activate_code: row.get(1)?,
                        token: row.get(2)?,
                    })
                },
            )
            .optional()
            .map_err(DatabaseError::from)
    }

    pub fn save_client_activation_secrets(
        &self,
        record: &ClientActivationSecretsRecord,
    ) -> Result<()> {
        self.connection()?.execute(
            r#"INSERT INTO client_activation_secrets(
                    id, device_id, activate_code, token, updated_at
                ) VALUES(1, ?1, ?2, ?3, ?4)
                ON CONFLICT(id) DO UPDATE SET
                    device_id=excluded.device_id,
                    activate_code=excluded.activate_code,
                    token=excluded.token,
                    updated_at=excluded.updated_at"#,
            params![
                record.device_id,
                record.activate_code,
                record.token,
                Utc::now().to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    pub fn clear_client_activation_secrets(&self) -> Result<()> {
        self.connection()?
            .execute("DELETE FROM client_activation_secrets WHERE id = 1", [])?;
        Ok(())
    }

    pub fn legacy_client_activation(&self) -> Result<Option<LegacyClientActivationRecord>> {
        let connection = self.connection()?;
        let exists = connection
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'client_activation_legacy_v10'",
                [],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if !exists {
            return Ok(None);
        }
        connection
            .query_row(
                "SELECT device_id, activate_code, token FROM client_activation_legacy_v10 WHERE id = 1",
                [],
                |row| {
                    Ok(LegacyClientActivationRecord {
                        device_id: row.get(0)?,
                        activate_code: row.get(1)?,
                        token: row.get(2)?,
                    })
                },
            )
            .optional()
            .map_err(DatabaseError::from)
    }

    pub fn finalize_client_activation_secret_migration(&self) -> Result<()> {
        self.connection()?
            .execute_batch("DROP TABLE IF EXISTS client_activation_legacy_v10")?;
        Ok(())
    }

    pub fn clear_client_activation(&self) -> Result<()> {
        self.connection()?
            .execute("DELETE FROM client_activation WHERE id = 1", [])?;
        Ok(())
    }

    pub fn runtime_resource_status(&self) -> Result<RuntimeResourceRecord> {
        let connection = self.connection()?;
        let platform = format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH);
        connection
            .query_row(
                r#"SELECT platform, app_min_version, bundle_version, status,
                          progress_bytes, total_bytes, manifest_sha256, current_component,
                          error_code, error_message, updated_at
                   FROM runtime_resources WHERE platform = ?1"#,
                [platform.clone()],
                |row| {
                    let status: String = row.get(3)?;
                    let status = serde_json::from_str::<ResourceStatus>(&format!("\"{status}\""))
                        .unwrap_or(ResourceStatus::Failed);
                    Ok(RuntimeResourceRecord {
                        platform: row.get(0)?,
                        app_min_version: row.get(1)?,
                        bundle_version: row.get(2)?,
                        status,
                        progress_bytes: row.get::<_, i64>(4)?.max(0) as u64,
                        total_bytes: row.get::<_, i64>(5)?.max(0) as u64,
                        manifest_sha256: row.get(6)?,
                        current_component: row.get(7)?,
                        error_code: row.get(8)?,
                        error_message: row.get(9)?,
                        updated_at: row.get(10)?,
                    })
                },
            )
            .optional()?
            .map_or_else(
                || {
                    Ok(RuntimeResourceRecord {
                        platform,
                        app_min_version: env!("CARGO_PKG_VERSION").to_owned(),
                        bundle_version: None,
                        status: ResourceStatus::Failed,
                        progress_bytes: 0,
                        total_bytes: 0,
                        manifest_sha256: None,
                        current_component: None,
                        error_code: Some("resource_not_checked".to_owned()),
                        error_message: Some("尚未检查本地运行资源".to_owned()),
                        updated_at: Utc::now().to_rfc3339(),
                    })
                },
                Ok,
            )
    }

    pub fn save_runtime_resource_status(&self, snapshot: &ResourceStatusSnapshot) -> Result<()> {
        let connection = self.connection()?;
        let platform = format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH);
        let status = serde_json::to_string(&snapshot.status)
            .map_err(|error| DatabaseError::MigrationIntegrity(error.to_string()))?
            .trim_matches('"')
            .to_owned();
        connection.execute(
            r#"INSERT INTO runtime_resources(
                platform, app_min_version, bundle_version, status, progress_bytes,
                total_bytes, manifest_sha256, current_component, error_code,
                error_message, updated_at
            ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
            ON CONFLICT(platform) DO UPDATE SET
                app_min_version=excluded.app_min_version,
                bundle_version=excluded.bundle_version,
                status=excluded.status,
                progress_bytes=excluded.progress_bytes,
                total_bytes=excluded.total_bytes,
                manifest_sha256=excluded.manifest_sha256,
                current_component=excluded.current_component,
                error_code=excluded.error_code,
                error_message=excluded.error_message,
                updated_at=excluded.updated_at"#,
            params![
                platform,
                env!("CARGO_PKG_VERSION"),
                snapshot.bundle_version,
                status,
                snapshot.downloaded_bytes as i64,
                snapshot.total_bytes as i64,
                snapshot.manifest_sha256,
                snapshot.current_component,
                snapshot.error_code,
                snapshot.error_message,
                Utc::now().to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    fn ensure_default_settings(&self) -> Result<()> {
        let defaults = AppSettings::defaults();
        let values = [
            ("output_root", defaults.output_root),
            ("quality", defaults.quality),
            ("protocol", defaults.protocol),
            ("segment_seconds", defaults.segment_seconds.to_string()),
            (
                "max_concurrent_recordings",
                defaults.max_concurrent_recordings.to_string(),
            ),
            (
                "asr_during_recording",
                defaults.asr_during_recording.to_string(),
            ),
            ("ffmpeg_path", defaults.ffmpeg_path),
            ("ffprobe_path", defaults.ffprobe_path),
            (
                "notifications_enabled",
                defaults.notifications_enabled.to_string(),
            ),
            ("autostart_enabled", defaults.autostart_enabled.to_string()),
        ];
        let connection = self.connection()?;
        for (key, value) in values {
            connection.execute(
                "INSERT OR IGNORE INTO settings(key, value) VALUES(?1, ?2)",
                params![key, value],
            )?;
        }
        Ok(())
    }

    pub fn add_streamer(&self, input: &NewStreamer) -> Result<Streamer> {
        let normalized_tags = normalize_streamer_tags(&input.tags)?;
        let now = Utc::now().to_rfc3339();
        let monitor_status = initial_monitor_status(input);
        let live_status = if input.web_rid.is_some() {
            "checking"
        } else {
            "offline"
        };
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let inserted = transaction.execute(
            r#"
            INSERT INTO streamers(
                name, source_kind, source_url, profile_sec_uid, web_rid,
                room_url, room_id, monitor_enabled, archived,
                live_status, monitor_status, recording_priority, created_at, updated_at
            ) VALUES(
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 0, ?9, ?10,
                (SELECT COALESCE(MAX(recording_priority), 0) + 1 FROM streamers),
                ?11, ?11
            )
            "#,
            params![
                input.name.trim(),
                source_kind_value(input.source_kind),
                input.source_url,
                input.profile_sec_uid,
                input.web_rid,
                input.room_url,
                input.room_id,
                input.monitor_enabled,
                live_status,
                monitor_status,
                now
            ],
        );
        match inserted {
            Ok(_) => {
                let id = transaction.last_insert_rowid();
                insert_streamer_tags_in_transaction(&transaction, id, &normalized_tags)?;
                transaction.commit()?;
                drop(connection);
                self.get_streamer(id)
            }
            Err(error) if error.sqlite_error_code() == Some(ErrorCode::ConstraintViolation) => {
                Err(duplicate_identity_error(input))
            }
            Err(error) => Err(error.into()),
        }
    }

    pub fn get_streamer(&self, id: i64) -> Result<Streamer> {
        let connection = self.connection()?;
        let mut streamer = connection
            .query_row(&streamer_select("WHERE s.id = ?1"), [id], map_streamer)
            .optional()?
            .ok_or(DatabaseError::NotFound("主播"))?;
        attach_tags_to_streamers(&connection, std::slice::from_mut(&mut streamer))?;
        Ok(streamer)
    }

    pub fn find_streamer_by_room_id(&self, room_id: &str) -> Result<Option<Streamer>> {
        let connection = self.connection()?;
        let mut streamer = connection
            .query_row(
                &streamer_select("WHERE s.room_id = ?1 ORDER BY s.id LIMIT 1"),
                [room_id],
                map_streamer,
            )
            .optional()?;
        if let Some(streamer) = streamer.as_mut() {
            attach_tags_to_streamers(&connection, std::slice::from_mut(streamer))?;
        }
        Ok(streamer)
    }

    pub fn find_streamer_by_profile_sec_uid(
        &self,
        profile_sec_uid: &str,
    ) -> Result<Option<Streamer>> {
        let connection = self.connection()?;
        let mut streamer = connection
            .query_row(
                &streamer_select("WHERE s.profile_sec_uid = ?1"),
                [profile_sec_uid],
                map_streamer,
            )
            .optional()?;
        if let Some(streamer) = streamer.as_mut() {
            attach_tags_to_streamers(&connection, std::slice::from_mut(streamer))?;
        }
        Ok(streamer)
    }

    pub fn find_streamer_by_web_rid(&self, web_rid: &str) -> Result<Option<Streamer>> {
        let connection = self.connection()?;
        let mut streamer = connection
            .query_row(
                &streamer_select("WHERE s.web_rid = ?1"),
                [web_rid],
                map_streamer,
            )
            .optional()?;
        if let Some(streamer) = streamer.as_mut() {
            attach_tags_to_streamers(&connection, std::slice::from_mut(streamer))?;
        }
        Ok(streamer)
    }

    pub fn list_streamers(&self, include_archived: bool) -> Result<Vec<Streamer>> {
        let connection = self.connection()?;
        let suffix = if include_archived {
            "ORDER BY s.name COLLATE NOCASE"
        } else {
            "WHERE s.archived = 0 ORDER BY s.name COLLATE NOCASE"
        };
        let sql = streamer_select(suffix);
        let mut statement = connection.prepare(&sql)?;
        let rows = statement.query_map([], map_streamer)?;
        let mut streamers = rows.collect::<std::result::Result<Vec<_>, _>>()?;
        drop(statement);
        attach_tags_to_streamers(&connection, &mut streamers)?;
        Ok(streamers)
    }

    pub fn list_streamers_by_recording_priority(&self) -> Result<Vec<Streamer>> {
        let connection = self.connection()?;
        let sql =
            streamer_select("WHERE s.archived = 0 ORDER BY s.recording_priority ASC, s.id ASC");
        let mut statement = connection.prepare(&sql)?;
        let rows = statement.query_map([], map_streamer)?;
        let mut streamers = rows.collect::<std::result::Result<Vec<_>, _>>()?;
        drop(statement);
        attach_tags_to_streamers(&connection, &mut streamers)?;
        Ok(streamers)
    }

    pub fn move_recording_priority(
        &self,
        id: i64,
        direction: crate::domain::RecordingPriorityDirection,
    ) -> Result<Vec<Streamer>> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        normalize_recording_priorities(&transaction)?;
        let current = transaction
            .query_row(
                "SELECT recording_priority FROM streamers WHERE id = ?1 AND archived = 0",
                [id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .ok_or(DatabaseError::NotFound("主播"))?;
        let neighbor = match direction {
            crate::domain::RecordingPriorityDirection::Up => transaction
                .query_row(
                    r#"SELECT id, recording_priority FROM streamers
                       WHERE archived = 0 AND recording_priority < ?1
                       ORDER BY recording_priority DESC, id DESC LIMIT 1"#,
                    [current],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
                )
                .optional()?,
            crate::domain::RecordingPriorityDirection::Down => transaction
                .query_row(
                    r#"SELECT id, recording_priority FROM streamers
                       WHERE archived = 0 AND recording_priority > ?1
                       ORDER BY recording_priority ASC, id ASC LIMIT 1"#,
                    [current],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
                )
                .optional()?,
        };
        if let Some((neighbor_id, neighbor_priority)) = neighbor {
            let now = Utc::now().to_rfc3339();
            transaction.execute(
                "UPDATE streamers SET recording_priority = ?1, updated_at = ?2 WHERE id = ?3",
                params![neighbor_priority, now, id],
            )?;
            transaction.execute(
                "UPDATE streamers SET recording_priority = ?1, updated_at = ?2 WHERE id = ?3",
                params![current, now, neighbor_id],
            )?;
        }
        transaction.commit()?;
        drop(connection);
        self.list_streamers_by_recording_priority()
    }

    pub fn replace_streamer_tags(
        &self,
        streamer_id: i64,
        tags: &[StreamerTagInput],
    ) -> Result<Vec<StreamerTag>> {
        let normalized = normalize_streamer_tags(tags)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let exists = transaction
            .query_row(
                "SELECT 1 FROM streamers WHERE id = ?1",
                [streamer_id],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if !exists {
            return Err(DatabaseError::NotFound("主播"));
        }
        replace_streamer_tags_in_transaction(&transaction, streamer_id, &normalized)?;
        transaction.commit()?;
        drop(connection);
        Ok(self.get_streamer(streamer_id)?.tags)
    }

    pub fn list_streamer_tag_name_suggestions(&self, limit: usize) -> Result<Vec<String>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            r#"
            SELECT (
                SELECT latest.name
                FROM streamer_tags latest
                WHERE latest.normalized_name = grouped.normalized_name
                ORDER BY latest.updated_at DESC, latest.id DESC
                LIMIT 1
            )
            FROM streamer_tags grouped
            GROUP BY grouped.normalized_name
            ORDER BY MAX(grouped.updated_at) DESC, grouped.normalized_name
            LIMIT ?1
            "#,
        )?;
        let rows = statement.query_map([limit.min(100) as i64], |row| row.get(0))?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn streamer_prompt_context(&self, streamer_id: i64) -> Result<StreamerPromptContext> {
        self.get_streamer(streamer_id)
            .map(|streamer| streamer.prompt_context())
    }

    pub fn set_monitor_enabled(&self, id: i64, enabled: bool) -> Result<()> {
        let status = if enabled { "waiting" } else { "paused" };
        let changed = self.connection()?.execute(
            "UPDATE streamers SET monitor_enabled = ?1, monitor_status = ?2, last_error = NULL, failure_count = 0, next_retry_at = NULL, updated_at = ?3 WHERE id = ?4 AND archived = 0",
            params![enabled, status, Utc::now().to_rfc3339(), id],
        )?;
        if changed == 0 {
            return Err(DatabaseError::NotFound("主播"));
        }
        Ok(())
    }

    pub fn update_streamer(&self, id: i64, input: &NewStreamer) -> Result<Streamer> {
        let normalized_tags = normalize_streamer_tags(&input.tags)?;
        let monitor_status = initial_monitor_status(input);
        let live_status = if input.web_rid.is_some() {
            "checking"
        } else {
            "offline"
        };
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let updated = transaction.execute(
            r#"
            UPDATE streamers
            SET name = ?1, source_kind = ?2, source_url = ?3,
                profile_sec_uid = ?4, web_rid = ?5, room_url = ?6,
                room_id = ?7, monitor_enabled = ?8, live_status = ?9,
                monitor_status = ?10, last_error = NULL, failure_count = 0,
                next_retry_at = NULL, updated_at = ?11
            WHERE id = ?12 AND archived = 0
            "#,
            params![
                input.name.trim(),
                source_kind_value(input.source_kind),
                input.source_url,
                input.profile_sec_uid,
                input.web_rid,
                input.room_url,
                input.room_id,
                input.monitor_enabled,
                live_status,
                monitor_status,
                Utc::now().to_rfc3339(),
                id
            ],
        );
        match updated {
            Ok(0) => Err(DatabaseError::NotFound("主播")),
            Ok(_) => {
                replace_streamer_tags_in_transaction(&transaction, id, &normalized_tags)?;
                transaction.commit()?;
                drop(connection);
                self.get_streamer(id)
            }
            Err(error) if error.sqlite_error_code() == Some(ErrorCode::ConstraintViolation) => {
                Err(duplicate_identity_error(input))
            }
            Err(error) => Err(error.into()),
        }
    }

    pub fn update_streamer_without_source_change(
        &self,
        id: i64,
        name: &str,
        monitor_enabled: bool,
        tags: &[StreamerTagInput],
    ) -> Result<Streamer> {
        let normalized_tags = normalize_streamer_tags(tags)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let changed = transaction.execute(
            r#"
            UPDATE streamers
            SET name = ?1,
                monitor_enabled = ?2,
                monitor_status = CASE
                    WHEN ?2 = 0 THEN 'paused'
                    WHEN monitor_enabled = 0 THEN CASE
                        WHEN source_kind = 'profile' AND web_rid IS NULL
                            THEN 'waiting_first_live'
                        ELSE 'waiting'
                    END
                    ELSE monitor_status
                END,
                last_error = CASE WHEN ?2 = 0 THEN NULL ELSE last_error END,
                failure_count = CASE WHEN ?2 = 0 THEN 0 ELSE failure_count END,
                next_retry_at = CASE WHEN ?2 = 0 THEN NULL ELSE next_retry_at END,
                updated_at = ?3
            WHERE id = ?4 AND archived = 0
            "#,
            params![name.trim(), monitor_enabled, Utc::now().to_rfc3339(), id,],
        )?;
        if changed == 0 {
            return Err(DatabaseError::NotFound("主播"));
        }
        replace_streamer_tags_in_transaction(&transaction, id, &normalized_tags)?;
        transaction.commit()?;
        drop(connection);
        self.get_streamer(id)
    }

    pub fn restore_streamer_snapshot(&self, snapshot: &Streamer) -> Result<Streamer> {
        let snapshot_tags = snapshot
            .tags
            .iter()
            .map(|tag| StreamerTagInput {
                name: tag.name.clone(),
                prompt_guidance: tag.prompt_guidance.clone(),
            })
            .collect::<Vec<_>>();
        let normalized_tags = normalize_streamer_tags(&snapshot_tags)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let changed = transaction.execute(
            r#"
            UPDATE streamers
            SET name = ?1, source_kind = ?2, source_url = ?3,
                profile_sec_uid = ?4, web_rid = ?5, room_url = ?6,
                room_id = ?7, monitor_enabled = ?8, archived = ?9,
                live_status = ?10, monitor_status = ?11,
                last_checked_at = ?12, last_error = ?13, failure_count = ?14,
                next_retry_at = ?15, recording_priority = ?16, updated_at = ?17
            WHERE id = ?18
            "#,
            params![
                snapshot.name,
                source_kind_value(snapshot.source_kind),
                snapshot.source_url,
                snapshot.profile_sec_uid,
                snapshot.web_rid,
                snapshot.room_url,
                snapshot.room_id,
                snapshot.monitor_enabled,
                snapshot.archived,
                snapshot.live_status,
                snapshot.monitor_status,
                snapshot.last_checked_at,
                snapshot.last_error,
                snapshot.failure_count,
                snapshot.next_retry_at,
                snapshot.recording_priority,
                Utc::now().to_rfc3339(),
                snapshot.id,
            ],
        )?;
        if changed == 0 {
            return Err(DatabaseError::NotFound("主播"));
        }
        transaction.execute(
            "DELETE FROM streamer_tags WHERE streamer_id = ?1",
            [snapshot.id],
        )?;
        let now = Utc::now().to_rfc3339();
        for (snapshot_tag, normalized_tag) in snapshot.tags.iter().zip(normalized_tags) {
            transaction.execute(
                r#"
                INSERT INTO streamer_tags(
                    id, streamer_id, name, normalized_name, prompt_guidance,
                    sort_order, created_at, updated_at
                ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)
                "#,
                params![
                    snapshot_tag.id,
                    snapshot.id,
                    normalized_tag.name,
                    streamer_tag_name_key(&normalized_tag.name),
                    normalized_tag.prompt_guidance,
                    snapshot_tag.sort_order,
                    now,
                ],
            )?;
        }
        transaction.commit()?;
        drop(connection);
        self.get_streamer(snapshot.id)
    }

    pub fn restore_streamer(&self, id: i64, input: &NewStreamer) -> Result<Streamer> {
        let monitor_status = initial_monitor_status(input);
        let live_status = if input.web_rid.is_some() {
            "checking"
        } else {
            "offline"
        };
        let connection = self.connection()?;
        let changed = connection.execute(
            r#"
            UPDATE streamers
            SET name = ?1, source_kind = ?2, source_url = ?3,
                profile_sec_uid = ?4, web_rid = ?5, room_url = ?6,
                room_id = ?7, monitor_enabled = ?8, archived = 0,
                live_status = ?9, monitor_status = ?10, last_checked_at = NULL,
                last_error = NULL, failure_count = 0, next_retry_at = NULL,
                updated_at = ?11
            WHERE id = ?12 AND archived = 1
            "#,
            params![
                input.name.trim(),
                source_kind_value(input.source_kind),
                input.source_url,
                input.profile_sec_uid,
                input.web_rid,
                input.room_url,
                input.room_id,
                input.monitor_enabled,
                live_status,
                monitor_status,
                Utc::now().to_rfc3339(),
                id
            ],
        )?;
        if changed == 0 {
            return Err(DatabaseError::NotFound("已归档主播"));
        }
        drop(connection);
        self.get_streamer(id)
    }

    pub fn bind_discovered_room(
        &self,
        streamer_id: i64,
        web_rid: &str,
        room_url: &str,
        room_id: Option<&str>,
    ) -> Result<DiscoveryBinding> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let source = transaction
            .query_row(
                "SELECT source_url, profile_sec_uid, monitor_enabled, recording_priority FROM streamers WHERE id = ?1",
                [streamer_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, bool>(2)?,
                        row.get::<_, i64>(3)?,
                    ))
                },
            )
            .optional()?
            .ok_or(DatabaseError::NotFound("主播"))?;
        let target = transaction
            .query_row(
                "SELECT id, profile_sec_uid, monitor_enabled, recording_priority FROM streamers WHERE web_rid = ?1 AND id <> ?2",
                params![web_rid, streamer_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, bool>(2)?,
                        row.get::<_, i64>(3)?,
                    ))
                },
            )
            .optional()?;

        let Some((target_id, target_profile_sec_uid, target_monitor_enabled, target_priority)) =
            target
        else {
            transaction.execute(
                r#"
                UPDATE streamers
                SET web_rid = ?1, room_url = ?2, room_id = ?3,
                    live_status = 'checking', monitor_status = CASE
                        WHEN monitor_enabled = 1 THEN 'waiting' ELSE 'paused' END,
                    last_error = NULL, failure_count = 0, next_retry_at = NULL,
                    updated_at = ?4
                WHERE id = ?5
                "#,
                params![
                    web_rid,
                    room_url,
                    room_id,
                    Utc::now().to_rfc3339(),
                    streamer_id
                ],
            )?;
            transaction.commit()?;
            drop(connection);
            return self
                .get_streamer(streamer_id)
                .map(Box::new)
                .map(DiscoveryBinding::Bound);
        };

        let source_history_count: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM recording_sessions WHERE streamer_id = ?1",
            [streamer_id],
            |row| row.get(0),
        )?;
        let profile_is_compatible =
            target_profile_sec_uid.is_none() || target_profile_sec_uid == source.1;
        if source_history_count == 0 && profile_is_compatible {
            let target_tags = load_streamer_tag_inputs(&transaction, target_id)?;
            let source_tags = load_streamer_tag_inputs(&transaction, streamer_id)?;
            let (merged_tags, tags_truncated) = merge_streamer_tags(target_tags, source_tags);
            replace_streamer_tags_in_transaction(&transaction, target_id, &merged_tags)?;
            transaction.execute("DELETE FROM streamers WHERE id = ?1", [streamer_id])?;
            let monitor_enabled = source.2 || target_monitor_enabled;
            let retained_priority = source.3.min(target_priority);
            transaction.execute(
                r#"
                UPDATE streamers
                SET source_kind = CASE WHEN ?1 IS NULL THEN source_kind ELSE 'profile' END,
                    source_url = CASE WHEN ?1 IS NULL THEN source_url ELSE ?2 END,
                    profile_sec_uid = COALESCE(profile_sec_uid, ?1),
                    web_rid = ?3, room_url = ?4, room_id = ?5,
                    monitor_enabled = ?6, archived = 0, live_status = 'checking',
                    monitor_status = CASE WHEN ?6 = 1 THEN 'waiting' ELSE 'paused' END,
                    last_checked_at = NULL, last_error = NULL,
                    recording_priority = ?7, updated_at = ?8
                WHERE id = ?9
                "#,
                params![
                    source.1,
                    source.0,
                    web_rid,
                    room_url,
                    room_id,
                    monitor_enabled,
                    retained_priority,
                    Utc::now().to_rfc3339(),
                    target_id
                ],
            )?;
            normalize_recording_priorities(&transaction)?;
            transaction.commit()?;
            if tags_truncated {
                eprintln!("主播标签合并超过 10 个，已按目标优先规则截断");
            }
            return Ok(DiscoveryBinding::Merged {
                target_streamer_id: target_id,
                removed_streamer_id: streamer_id,
            });
        }

        transaction.execute(
            r#"
            UPDATE streamers
            SET monitor_enabled = 0, monitor_status = 'identity_conflict',
                last_error = '发现重复稳定直播入口，已暂停监听', updated_at = ?1
            WHERE id = ?2
            "#,
            params![Utc::now().to_rfc3339(), streamer_id],
        )?;
        transaction.commit()?;
        Ok(DiscoveryBinding::Conflict {
            target_streamer_id: target_id,
        })
    }

    pub fn clear_room_binding(&self, streamer_id: i64) -> Result<()> {
        let changed = self.connection()?.execute(
            r#"
            UPDATE streamers
            SET web_rid = NULL, room_url = NULL, room_id = NULL,
                live_status = 'offline', monitor_status = 'rediscovering',
                last_error = NULL, failure_count = 0, next_retry_at = NULL,
                updated_at = ?1
            WHERE id = ?2 AND source_kind = 'profile'
            "#,
            params![Utc::now().to_rfc3339(), streamer_id],
        )?;
        if changed == 0 {
            return Err(DatabaseError::NotFound("个人主页主播"));
        }
        Ok(())
    }

    pub fn update_current_room_id(&self, streamer_id: i64, room_id: &str) -> Result<()> {
        let changed = self.connection()?.execute(
            "UPDATE streamers SET room_id = ?1, updated_at = ?2 WHERE id = ?3",
            params![room_id, Utc::now().to_rfc3339(), streamer_id],
        )?;
        if changed == 0 {
            return Err(DatabaseError::NotFound("主播"));
        }
        Ok(())
    }

    pub fn update_streamer_status(
        &self,
        id: i64,
        live_status: &str,
        monitor_status: &str,
        error: Option<&str>,
    ) -> Result<()> {
        self.connection()?.execute(
            r#"
            UPDATE streamers
            SET live_status = ?1, monitor_status = ?2, last_checked_at = ?3,
                last_error = ?4,
                failure_count = CASE WHEN ?4 IS NULL THEN 0 ELSE failure_count END,
                next_retry_at = CASE WHEN ?4 IS NULL THEN NULL ELSE next_retry_at END,
                updated_at = ?3
            WHERE id = ?5
            "#,
            params![
                live_status,
                monitor_status,
                Utc::now().to_rfc3339(),
                error,
                id
            ],
        )?;
        Ok(())
    }

    pub fn update_streamer_failure(
        &self,
        id: i64,
        live_status: &str,
        monitor_status: &str,
        error: &str,
        failure_count: usize,
        next_retry_at: &str,
    ) -> Result<()> {
        self.connection()?.execute(
            r#"
            UPDATE streamers
            SET live_status = ?1, monitor_status = ?2, last_checked_at = ?3,
                last_error = ?4, failure_count = ?5, next_retry_at = ?6,
                updated_at = ?3
            WHERE id = ?7
            "#,
            params![
                live_status,
                monitor_status,
                Utc::now().to_rfc3339(),
                error,
                failure_count as i64,
                next_retry_at,
                id,
            ],
        )?;
        Ok(())
    }

    pub fn update_streamer_verification_required(
        &self,
        id: i64,
        error: &str,
        failure_count: usize,
    ) -> Result<()> {
        self.connection()?.execute(
            r#"
            UPDATE streamers
            SET live_status = 'error', monitor_status = 'verification_required',
                last_checked_at = ?1, last_error = ?2, failure_count = ?3,
                next_retry_at = NULL, updated_at = ?1
            WHERE id = ?4
            "#,
            params![Utc::now().to_rfc3339(), error, failure_count as i64, id],
        )?;
        Ok(())
    }

    pub fn archive_streamer(&self, id: i64) -> Result<()> {
        self.connection()?.execute(
            "UPDATE streamers SET archived = 1, monitor_enabled = 0, monitor_status = 'paused', last_error = NULL, failure_count = 0, next_retry_at = NULL, updated_at = ?1 WHERE id = ?2",
            params![Utc::now().to_rfc3339(), id],
        )?;
        Ok(())
    }

    pub fn get_settings(&self) -> Result<AppSettings> {
        let connection = self.connection()?;
        let mut statement = connection.prepare("SELECT key, value FROM settings")?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut values = std::collections::HashMap::new();
        for row in rows {
            let (key, value) = row?;
            values.insert(key, value);
        }
        let defaults = AppSettings::defaults();
        Ok(AppSettings {
            output_root: value_or(&values, "output_root", defaults.output_root),
            quality: value_or(&values, "quality", defaults.quality),
            protocol: value_or(&values, "protocol", defaults.protocol),
            segment_seconds: parse_or(&values, "segment_seconds", defaults.segment_seconds),
            max_concurrent_recordings: parse_or(
                &values,
                "max_concurrent_recordings",
                defaults.max_concurrent_recordings,
            ),
            asr_during_recording: parse_or(
                &values,
                "asr_during_recording",
                defaults.asr_during_recording,
            ),
            ffmpeg_path: value_or(&values, "ffmpeg_path", defaults.ffmpeg_path),
            ffprobe_path: value_or(&values, "ffprobe_path", defaults.ffprobe_path),
            notifications_enabled: parse_or(
                &values,
                "notifications_enabled",
                defaults.notifications_enabled,
            ),
            autostart_enabled: parse_or(&values, "autostart_enabled", defaults.autostart_enabled),
        })
    }

    pub fn save_settings(&self, settings: &AppSettings) -> Result<()> {
        std::fs::create_dir_all(&settings.output_root)?;
        let values = [
            ("output_root", settings.output_root.clone()),
            ("quality", settings.quality.clone()),
            ("protocol", settings.protocol.clone()),
            ("segment_seconds", settings.segment_seconds.to_string()),
            (
                "max_concurrent_recordings",
                settings.max_concurrent_recordings.to_string(),
            ),
            (
                "asr_during_recording",
                settings.asr_during_recording.to_string(),
            ),
            ("ffmpeg_path", settings.ffmpeg_path.clone()),
            ("ffprobe_path", settings.ffprobe_path.clone()),
            (
                "notifications_enabled",
                settings.notifications_enabled.to_string(),
            ),
            ("autostart_enabled", settings.autostart_enabled.to_string()),
        ];
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        for (key, value) in values {
            transaction.execute(
                "INSERT INTO settings(key, value) VALUES(?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![key, value],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn start_session(&self, streamer_id: i64, output_root: &str) -> Result<RecordingSession> {
        let started_at = Utc::now().to_rfc3339();
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO recording_sessions(streamer_id, started_at, status, output_root) VALUES(?1, ?2, 'recording', ?3)",
            params![streamer_id, started_at, output_root],
        )?;
        let id = connection.last_insert_rowid();
        drop(connection);
        self.get_session(id)
    }

    pub fn get_session(&self, id: i64) -> Result<RecordingSession> {
        self.connection()?
            .query_row(
                "SELECT id, streamer_id, started_at, ended_at, status, retry_count, output_root, error FROM recording_sessions WHERE id = ?1",
                [id],
                |row| {
                    Ok(RecordingSession {
                        id: row.get(0)?,
                        streamer_id: row.get(1)?,
                        started_at: row.get(2)?,
                        ended_at: row.get(3)?,
                        status: row.get(4)?,
                        retry_count: row.get(5)?,
                        output_root: row.get(6)?,
                        error: row.get(7)?,
                    })
                },
            )
            .optional()?
            .ok_or(DatabaseError::NotFound("录制会话"))
    }

    pub fn current_session(&self, streamer_id: i64) -> Result<Option<RecordingSession>> {
        self.connection()?
            .query_row(
                "SELECT id, streamer_id, started_at, ended_at, status, retry_count, output_root, error FROM recording_sessions WHERE streamer_id = ?1 AND ended_at IS NULL ORDER BY id DESC LIMIT 1",
                [streamer_id],
                |row| {
                    Ok(RecordingSession {
                        id: row.get(0)?,
                        streamer_id: row.get(1)?,
                        started_at: row.get(2)?,
                        ended_at: row.get(3)?,
                        status: row.get(4)?,
                        retry_count: row.get(5)?,
                        output_root: row.get(6)?,
                        error: row.get(7)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    /// AI 项目选择器只列出已经结束的会话，避免读取仍在写入的录像分片。
    pub fn list_completed_sessions(&self, limit: usize) -> Result<Vec<RecordingSession>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            r#"
            SELECT id, streamer_id, started_at, ended_at, status, retry_count, output_root, error
            FROM recording_sessions
            WHERE ended_at IS NOT NULL
            ORDER BY started_at DESC, id DESC
            LIMIT ?1
            "#,
        )?;
        let rows =
            statement.query_map([i64::try_from(limit.max(1)).unwrap_or(i64::MAX)], |row| {
                Ok(RecordingSession {
                    id: row.get(0)?,
                    streamer_id: row.get(1)?,
                    started_at: row.get(2)?,
                    ended_at: row.get(3)?,
                    status: row.get(4)?,
                    retry_count: row.get(5)?,
                    output_root: row.get(6)?,
                    error: row.get(7)?,
                })
            })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn update_session_retry(&self, id: i64, retry_count: i64) -> Result<()> {
        self.connection()?.execute(
            "UPDATE recording_sessions SET retry_count = ?1, status = 'retrying' WHERE id = ?2",
            params![retry_count, id],
        )?;
        Ok(())
    }

    pub fn mark_session_recording(&self, id: i64) -> Result<()> {
        self.connection()?.execute(
            "UPDATE recording_sessions SET status = 'recording', error = NULL WHERE id = ?1 AND ended_at IS NULL",
            [id],
        )?;
        Ok(())
    }

    pub fn finish_session(&self, id: i64, status: &str, error: Option<&str>) -> Result<()> {
        self.connection()?.execute(
            "UPDATE recording_sessions SET ended_at = ?1, status = ?2, error = ?3 WHERE id = ?4",
            params![Utc::now().to_rfc3339(), status, error, id],
        )?;
        Ok(())
    }

    pub fn add_video(&self, video: &NewVideo) -> Result<i64> {
        let connection = self.connection()?;
        connection.execute(
            r#"
            INSERT OR IGNORE INTO videos(
                session_id, path, started_at, ended_at, duration_seconds,
                size_bytes, audio_present, status, created_at
            ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
            "#,
            params![
                video.session_id,
                video.path,
                video.started_at,
                video.ended_at,
                video.duration_seconds,
                video.size_bytes,
                video.audio_present,
                video.status,
                Utc::now().to_rfc3339()
            ],
        )?;
        connection
            .query_row(
                "SELECT id FROM videos WHERE path = ?1",
                [&video.path],
                |row| row.get(0),
            )
            .map_err(Into::into)
    }

    pub fn list_videos(
        &self,
        streamer_id: Option<i64>,
        page: u32,
        page_size: u32,
    ) -> Result<VideoPage> {
        self.query_videos(streamer_id, page, page_size, &VideoFilter::default())
    }

    pub fn query_videos(
        &self,
        streamer_id: Option<i64>,
        page: u32,
        page_size: u32,
        filter: &VideoFilter,
    ) -> Result<VideoPage> {
        let page = page.max(1);
        let page_size = page_size.clamp(1, 50);
        let offset = i64::from((page - 1) * page_size);
        let (total, mut items) = {
            let connection = self.connection()?;
            let total = connection.query_row(
                r#"
                SELECT COUNT(*)
                FROM videos v
                JOIN recording_sessions rs ON rs.id = v.session_id
                JOIN streamers s ON s.id = rs.streamer_id
                WHERE (?1 IS NULL OR rs.streamer_id = ?1)
                  AND (?2 IS NULL OR v.status = ?2)
                  AND (?3 IS NULL OR lower(s.name || ' ' || v.path) LIKE '%' || lower(?3) || '%')
                  AND (?4 IS NULL OR COALESCE(v.started_at, rs.started_at) >= ?4)
                  AND (?5 IS NULL OR COALESCE(v.started_at, rs.started_at) <= ?5)
                  AND (?6 = 0 OR rs.ended_at IS NULL)
                "#,
                params![
                    streamer_id,
                    filter.status.as_deref(),
                    filter.search.as_deref(),
                    filter.from.as_deref(),
                    filter.to.as_deref(),
                    filter.current_only,
                ],
                |row| row.get(0),
            )?;
            let mut statement = connection.prepare(
                r#"
                SELECT v.id, v.session_id, rs.streamer_id, s.name, v.path,
                       v.started_at, v.ended_at, v.duration_seconds, v.size_bytes,
                       v.audio_present, v.status
                FROM videos v
                JOIN recording_sessions rs ON rs.id = v.session_id
                JOIN streamers s ON s.id = rs.streamer_id
                WHERE (?1 IS NULL OR rs.streamer_id = ?1)
                  AND (?2 IS NULL OR v.status = ?2)
                  AND (?3 IS NULL OR lower(s.name || ' ' || v.path) LIKE '%' || lower(?3) || '%')
                  AND (?4 IS NULL OR COALESCE(v.started_at, rs.started_at) >= ?4)
                  AND (?5 IS NULL OR COALESCE(v.started_at, rs.started_at) <= ?5)
                  AND (?6 = 0 OR rs.ended_at IS NULL)
                ORDER BY COALESCE(v.started_at, rs.started_at) DESC, v.id DESC
                LIMIT ?7 OFFSET ?8
                "#,
            )?;
            let rows = statement.query_map(
                params![
                    streamer_id,
                    filter.status.as_deref(),
                    filter.search.as_deref(),
                    filter.from.as_deref(),
                    filter.to.as_deref(),
                    filter.current_only,
                    page_size,
                    offset,
                ],
                map_video,
            )?;
            (total, rows.collect::<std::result::Result<Vec<_>, _>>()?)
        };
        for video in &mut items {
            if video.status == "complete" && !Path::new(&video.path).is_file() {
                self.mark_video_status(video.id, "missing")?;
                video.status = "missing".to_owned();
            }
        }
        Ok(VideoPage {
            items,
            total,
            page,
            page_size,
        })
    }

    pub fn mark_video_status(&self, id: i64, status: &str) -> Result<()> {
        self.connection()?.execute(
            "UPDATE videos SET status = ?1 WHERE id = ?2",
            params![status, id],
        )?;
        Ok(())
    }

    pub fn delete_video_record(&self, id: i64) -> Result<()> {
        self.connection()?
            .execute("DELETE FROM videos WHERE id = ?1", [id])?;
        Ok(())
    }

    pub fn get_video(&self, id: i64) -> Result<Video> {
        self.connection()?
            .query_row(
                r#"
                SELECT v.id, v.session_id, rs.streamer_id, s.name, v.path,
                       v.started_at, v.ended_at, v.duration_seconds, v.size_bytes,
                       v.audio_present, v.status
                FROM videos v
                JOIN recording_sessions rs ON rs.id = v.session_id
                JOIN streamers s ON s.id = rs.streamer_id
                WHERE v.id = ?1
                "#,
                [id],
                map_video,
            )
            .optional()?
            .ok_or(DatabaseError::NotFound("视频"))
    }

    pub fn list_session_videos(&self, session_id: i64) -> Result<Vec<Video>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            r#"
            SELECT v.id, v.session_id, rs.streamer_id, s.name, v.path,
                   v.started_at, v.ended_at, v.duration_seconds, v.size_bytes,
                   v.audio_present, v.status
            FROM videos v
            JOIN recording_sessions rs ON rs.id = v.session_id
            JOIN streamers s ON s.id = rs.streamer_id
            WHERE v.session_id = ?1
            ORDER BY COALESCE(v.started_at, rs.started_at), v.id
            "#,
        )?;
        let rows = statement.query_map([session_id], map_video)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn set_video_statuses(&self, statuses: &[(i64, String)]) -> Result<()> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        for (id, status) in statuses {
            transaction.execute(
                "UPDATE videos SET status = ?1 WHERE id = ?2",
                params![status, id],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn delete_session_records(&self, session_id: i64) -> Result<()> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        transaction.execute("DELETE FROM videos WHERE session_id = ?1", [session_id])?;
        let changed = transaction.execute(
            "DELETE FROM recording_sessions WHERE id = ?1 AND ended_at IS NOT NULL",
            [session_id],
        )?;
        if changed == 0 {
            return Err(DatabaseError::NotFound("已结束录制会话"));
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn reconcile_startup(&self) -> Result<()> {
        let sessions = self.manifest_scopes(None, None)?;
        let interrupted_at = Utc::now().to_rfc3339();
        self.connection()?.execute(
            "UPDATE recording_sessions SET ended_at = ?1, status = 'interrupted', error = COALESCE(error, '应用异常中断') WHERE ended_at IS NULL",
            [&interrupted_at],
        )?;
        self.connection()?.execute(
            r#"
            UPDATE streamers
            SET monitor_status = CASE
                    WHEN monitor_enabled = 1 AND archived = 0 THEN 'waiting'
                    ELSE 'paused'
                END,
                updated_at = ?1
            WHERE monitor_status IN ('recording', 'waiting_resource')
              AND NOT EXISTS (
                  SELECT 1
                  FROM recording_sessions rs
                  WHERE rs.streamer_id = streamers.id
                    AND rs.ended_at IS NULL
              )
            "#,
            [&interrupted_at],
        )?;
        for mut session in sessions {
            if session.ended_at.is_none() {
                session.ended_at = Some(interrupted_at.clone());
            }
            self.reconcile_manifest_scope(&session)?;
        }
        let videos = {
            let connection = self.connection()?;
            let mut statement = connection.prepare("SELECT id, path FROM videos")?;
            let rows = statement.query_map([], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })?;
            rows.collect::<std::result::Result<Vec<_>, _>>()?
        };
        for (id, path) in videos {
            if !Path::new(&path).exists() {
                self.mark_video_status(id, "missing")?;
            }
        }
        Ok(())
    }

    pub fn reconcile_session_manifests(&self, session_id: i64) -> Result<()> {
        let session = self
            .manifest_scopes(None, Some(session_id))?
            .into_iter()
            .next()
            .ok_or(DatabaseError::NotFound("录制会话"))?;
        self.reconcile_manifest_scope(&session)
    }

    pub fn reconcile_streamer_sessions(&self, streamer_id: i64) -> Result<()> {
        let mut sessions = self.manifest_scopes(Some(streamer_id), None)?;
        let interrupted_at = Utc::now().to_rfc3339();
        self.connection()?.execute(
            "UPDATE recording_sessions SET ended_at = ?1, status = 'interrupted', error = COALESCE(error, '任务清理超时') WHERE streamer_id = ?2 AND ended_at IS NULL",
            params![interrupted_at, streamer_id],
        )?;
        for session in &mut sessions {
            if session.ended_at.is_none() {
                session.ended_at = Some(interrupted_at.clone());
            }
            self.reconcile_manifest_scope(session)?;
        }
        Ok(())
    }

    fn manifest_scopes(
        &self,
        streamer_id: Option<i64>,
        session_id: Option<i64>,
    ) -> Result<Vec<SessionManifestScope>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            r#"
            SELECT rs.id, rs.output_root, rs.started_at, rs.ended_at, s.room_id
            FROM recording_sessions rs
            JOIN streamers s ON s.id = rs.streamer_id
            WHERE (?1 IS NULL OR rs.streamer_id = ?1)
              AND (?2 IS NULL OR rs.id = ?2)
            ORDER BY rs.started_at DESC, rs.id DESC
            "#,
        )?;
        let rows = statement.query_map(params![streamer_id, session_id], |row| {
            Ok(SessionManifestScope {
                id: row.get(0)?,
                output_root: row.get(1)?,
                started_at: row.get(2)?,
                ended_at: row.get(3)?,
                room_id: row.get(4)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    fn reconcile_manifest_scope(&self, session: &SessionManifestScope) -> Result<()> {
        let Some(room_id) = session.room_id.as_deref() else {
            return Ok(());
        };
        for path in recover_manifest_segments(
            &session.output_root,
            room_id,
            &session.started_at,
            session.ended_at.as_deref(),
        ) {
            let size_bytes = std::fs::metadata(&path)
                .map(|metadata| metadata.len() as i64)
                .unwrap_or_default();
            self.add_video(&NewVideo {
                session_id: session.id,
                path: path.to_string_lossy().into_owned(),
                started_at: None,
                ended_at: None,
                duration_seconds: None,
                size_bytes,
                audio_present: None,
                status: "complete".to_owned(),
            })?;
        }
        Ok(())
    }

    pub fn dashboard(&self) -> Result<Dashboard> {
        let streamers = self.list_streamers(false)?;
        let active_recordings = streamers
            .iter()
            .filter(|streamer| streamer.monitor_status == "recording")
            .count() as i64;
        let current_video_count = streamers
            .iter()
            .map(|streamer| streamer.current_video_count)
            .sum();
        Ok(Dashboard {
            streamers,
            active_recordings,
            current_video_count,
            max_screen_limit: crate::domain::DEFAULT_MAX_SCREEN_LIMIT,
        })
    }
}

fn migrate_streamer_tags_v3(connection: &mut Connection) -> Result<()> {
    let transaction = connection.transaction()?;
    transaction.execute_batch(
        r#"
        CREATE TABLE streamer_tags (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            streamer_id INTEGER NOT NULL REFERENCES streamers(id) ON DELETE CASCADE,
            name TEXT NOT NULL,
            normalized_name TEXT NOT NULL,
            prompt_guidance TEXT,
            sort_order INTEGER NOT NULL,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            CHECK(length(trim(name)) > 0),
            CHECK(sort_order >= 0)
        );

        CREATE UNIQUE INDEX idx_streamer_tags_normalized_name
            ON streamer_tags(streamer_id, normalized_name);
        CREATE UNIQUE INDEX idx_streamer_tags_sort_order
            ON streamer_tags(streamer_id, sort_order);
        CREATE INDEX idx_streamer_tags_suggestions
            ON streamer_tags(normalized_name, updated_at DESC, id DESC);
        "#,
    )?;
    transaction.execute(
        "INSERT INTO schema_migrations(version, applied_at) VALUES(3, ?1)",
        [Utc::now().to_rfc3339()],
    )?;
    transaction.commit()?;
    Ok(())
}

fn migrate_recording_priority_v17(connection: &mut Connection) -> Result<()> {
    let applied = connection
        .query_row(
            "SELECT 1 FROM schema_migrations WHERE version = 17",
            [],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    let column_exists = connection.query_row(
        "SELECT COUNT(*) FROM pragma_table_info('streamers') WHERE name = 'recording_priority'",
        [],
        |row| row.get::<_, i64>(0),
    )? == 1;
    if applied && column_exists {
        return Ok(());
    }

    let transaction = connection.transaction()?;
    if !column_exists {
        transaction.execute_batch(
            "ALTER TABLE streamers ADD COLUMN recording_priority INTEGER NOT NULL DEFAULT 0;",
        )?;
    }
    normalize_recording_priorities(&transaction)?;
    transaction.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_streamers_recording_priority ON streamers(recording_priority, id);",
    )?;
    transaction.execute(
        "INSERT OR IGNORE INTO schema_migrations(version, applied_at) VALUES(17, ?1)",
        [Utc::now().to_rfc3339()],
    )?;
    transaction.commit()?;
    Ok(())
}

fn normalize_recording_priorities(transaction: &Transaction<'_>) -> Result<()> {
    let mut statement =
        transaction.prepare("SELECT id FROM streamers ORDER BY recording_priority ASC, id ASC")?;
    let ids = statement
        .query_map([], |row| row.get::<_, i64>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    drop(statement);
    for (index, id) in ids.into_iter().enumerate() {
        transaction.execute(
            "UPDATE streamers SET recording_priority = ?1 WHERE id = ?2",
            params![index as i64 + 1, id],
        )?;
    }
    Ok(())
}

fn migrate_streamer_diagnostics_v5(connection: &mut Connection) -> Result<()> {
    let transaction = connection.transaction()?;
    // Some development builds shipped the same columns under a different
    // migration number. Detect each column independently so those databases
    // can be upgraded without failing on a duplicate-column error.
    let has_failure_count = transaction
        .query_row(
            "SELECT 1 FROM pragma_table_info('streamers') WHERE name = ?1",
            ["failure_count"],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if !has_failure_count {
        transaction.execute(
            "ALTER TABLE streamers ADD COLUMN failure_count INTEGER NOT NULL DEFAULT 0 CHECK(failure_count >= 0)",
            [],
        )?;
    }

    let has_next_retry_at = transaction
        .query_row(
            "SELECT 1 FROM pragma_table_info('streamers') WHERE name = ?1",
            ["next_retry_at"],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if !has_next_retry_at {
        transaction.execute("ALTER TABLE streamers ADD COLUMN next_retry_at TEXT", [])?;
    }
    transaction.execute(
        "INSERT INTO schema_migrations(version, applied_at) VALUES(5, ?1)",
        [Utc::now().to_rfc3339()],
    )?;
    transaction.commit()?;
    Ok(())
}

fn migrate_runtime_resources_v8(connection: &mut Connection) -> Result<()> {
    let transaction = connection.transaction()?;
    transaction.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS runtime_resources (
            platform TEXT PRIMARY KEY,
            app_min_version TEXT NOT NULL,
            bundle_version TEXT,
            status TEXT NOT NULL,
            progress_bytes INTEGER NOT NULL DEFAULT 0 CHECK(progress_bytes >= 0),
            total_bytes INTEGER NOT NULL DEFAULT 0 CHECK(total_bytes >= 0),
            manifest_sha256 TEXT,
            current_component TEXT,
            error_code TEXT,
            error_message TEXT,
            updated_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS runtime_downloads (
            platform TEXT NOT NULL,
            bundle_version TEXT NOT NULL,
            relative_path TEXT NOT NULL,
            part_path TEXT NOT NULL,
            etag TEXT,
            last_modified TEXT,
            downloaded_bytes INTEGER NOT NULL DEFAULT 0 CHECK(downloaded_bytes >= 0),
            total_bytes INTEGER NOT NULL DEFAULT 0 CHECK(total_bytes >= 0),
            status TEXT NOT NULL DEFAULT 'pending',
            updated_at TEXT NOT NULL,
            PRIMARY KEY(platform, bundle_version, relative_path)
        );
        "#,
    )?;
    transaction.execute(
        "INSERT INTO schema_migrations(version, applied_at) VALUES(8, ?1)",
        [Utc::now().to_rfc3339()],
    )?;
    transaction.commit()?;
    Ok(())
}

fn migrate_client_activation_v10(connection: &mut Connection) -> Result<()> {
    let transaction = connection.transaction()?;
    transaction.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS client_activation (
            id INTEGER PRIMARY KEY CHECK(id = 1),
            device_id TEXT NOT NULL,
            activate_code TEXT NOT NULL,
            token TEXT,
            expire_at INTEGER,
            grace_sec INTEGER,
            server_time_offset_sec INTEGER NOT NULL DEFAULT 0,
            state TEXT NOT NULL,
            allow_custom_api_key INTEGER NOT NULL DEFAULT 0 CHECK(allow_custom_api_key IN (0, 1)),
            last_heartbeat_at TEXT,
            next_heartbeat_at TEXT,
            last_error TEXT,
            updated_at TEXT NOT NULL
        );
        "#,
    )?;
    transaction.execute(
        "INSERT OR IGNORE INTO schema_migrations(version, applied_at) VALUES(10, ?1)",
        [Utc::now().to_rfc3339()],
    )?;
    transaction.commit()?;
    Ok(())
}

fn migrate_client_activation_v19(connection: &mut Connection) -> Result<()> {
    let applied = connection
        .query_row(
            "SELECT 1 FROM schema_migrations WHERE version = 19",
            [],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if applied {
        return Ok(());
    }

    let transaction = connection.transaction()?;
    transaction.execute_batch(
        r#"
        ALTER TABLE client_activation RENAME TO client_activation_legacy_v10;

        CREATE TABLE client_activation (
            id INTEGER PRIMARY KEY CHECK(id = 1),
            device_id_hint TEXT NOT NULL,
            expire_at INTEGER,
            grace_sec INTEGER,
            server_time_offset_sec INTEGER NOT NULL DEFAULT 0,
            state TEXT NOT NULL,
            allow_custom_api_key INTEGER NOT NULL DEFAULT 0 CHECK(allow_custom_api_key IN (0, 1)),
            last_heartbeat_at TEXT,
            next_heartbeat_at TEXT,
            last_error TEXT,
            updated_at TEXT NOT NULL
        );

        INSERT INTO client_activation(
            id, device_id_hint, expire_at, grace_sec, server_time_offset_sec,
            state, allow_custom_api_key, last_heartbeat_at, next_heartbeat_at,
            last_error, updated_at
        )
        SELECT
            id,
            CASE
                WHEN length(device_id) > 8 THEN '…' || substr(device_id, length(device_id) - 7)
                ELSE '…迁移设备'
            END,
            expire_at,
            grace_sec,
            server_time_offset_sec,
            state,
            allow_custom_api_key,
            last_heartbeat_at,
            next_heartbeat_at,
            last_error,
            updated_at
        FROM client_activation_legacy_v10;
        "#,
    )?;
    transaction.execute(
        "INSERT INTO schema_migrations(version, applied_at) VALUES(19, ?1)",
        [Utc::now().to_rfc3339()],
    )?;
    transaction.commit()?;
    Ok(())
}

fn migrate_client_activation_secrets_v20(connection: &mut Connection) -> Result<()> {
    let transaction = connection.transaction()?;
    transaction.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS client_activation_secrets (
            id INTEGER PRIMARY KEY CHECK(id = 1),
            device_id TEXT NOT NULL,
            activate_code TEXT NOT NULL,
            token TEXT,
            updated_at TEXT NOT NULL
        );
        "#,
    )?;
    transaction.execute(
        "INSERT OR IGNORE INTO schema_migrations(version, applied_at) VALUES(20, ?1)",
        [Utc::now().to_rfc3339()],
    )?;
    transaction.commit()?;
    Ok(())
}

fn migrate_ai_replay_directory_v15(connection: &mut Connection) -> Result<()> {
    let applied = connection
        .query_row(
            "SELECT 1 FROM schema_migrations WHERE version = 15",
            [],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if applied {
        return Ok(());
    }

    let transaction = connection.transaction()?;
    transaction.execute_batch(
        r#"
        CREATE INDEX IF NOT EXISTS idx_sessions_completed_directory
            ON recording_sessions(streamer_id, ended_at DESC, started_at DESC, id DESC)
            WHERE ended_at IS NOT NULL;
        CREATE INDEX IF NOT EXISTS idx_ai_inputs_project_video
            ON ai_project_inputs(project_id, video_id)
            WHERE video_id IS NOT NULL;
        "#,
    )?;
    transaction.execute(
        "INSERT INTO schema_migrations(version, applied_at) VALUES(15, ?1)",
        [Utc::now().to_rfc3339()],
    )?;
    transaction.commit()?;
    Ok(())
}

fn migrate_streamers_v2(connection: &mut Connection) -> Result<()> {
    let transaction = connection.transaction()?;
    let original_count: i64 =
        transaction.query_row("SELECT COUNT(*) FROM streamers", [], |row| row.get(0))?;
    transaction.execute_batch(
        r#"
        CREATE TABLE streamers_v2 (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL,
            source_kind TEXT NOT NULL CHECK(source_kind IN ('profile', 'room')),
            source_url TEXT NOT NULL,
            profile_sec_uid TEXT,
            web_rid TEXT,
            room_url TEXT,
            room_id TEXT,
            monitor_enabled INTEGER NOT NULL DEFAULT 1,
            archived INTEGER NOT NULL DEFAULT 0,
            live_status TEXT NOT NULL DEFAULT 'checking',
            monitor_status TEXT NOT NULL DEFAULT 'waiting',
            last_checked_at TEXT,
            last_error TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );

        WITH legacy AS (
            SELECT
                streamers.*,
                CASE
                    WHEN room_url LIKE 'https://live.douyin.com/%'
                         AND substr(room_url, 25) <> ''
                         AND substr(room_url, 25) NOT GLOB '*[^0-9]*'
                        THEN substr(room_url, 25)
                    WHEN room_url LIKE 'http://live.douyin.com/%'
                         AND substr(room_url, 24) <> ''
                         AND substr(room_url, 24) NOT GLOB '*[^0-9]*'
                        THEN substr(room_url, 24)
                    ELSE NULL
                END AS extracted_web_rid
            FROM streamers
        ),
        resolved AS (
            SELECT
                legacy.*,
                CASE
                    WHEN extracted_web_rid IS NULL THEN 0
                    WHEN EXISTS (
                        SELECT 1
                        FROM legacy preferred
                        WHERE preferred.extracted_web_rid = legacy.extracted_web_rid
                          AND (
                              preferred.archived < legacy.archived
                              OR (
                                  preferred.archived = legacy.archived
                                  AND preferred.monitor_enabled > legacy.monitor_enabled
                              )
                              OR (
                                  preferred.archived = legacy.archived
                                  AND preferred.monitor_enabled = legacy.monitor_enabled
                                  AND preferred.id < legacy.id
                              )
                          )
                    ) THEN 1
                    ELSE 0
                END AS identity_conflict
            FROM legacy
        )
        INSERT INTO streamers_v2(
            id, name, source_kind, source_url, profile_sec_uid, web_rid,
            room_url, room_id, monitor_enabled, archived, live_status,
            monitor_status, last_checked_at, last_error, created_at, updated_at
        )
        SELECT
            id,
            name,
            'room',
            room_url,
            NULL,
            CASE WHEN identity_conflict = 0 THEN extracted_web_rid ELSE NULL END,
            room_url,
            room_id,
            CASE WHEN identity_conflict = 1 THEN 0 ELSE monitor_enabled END,
            archived,
            live_status,
            CASE WHEN identity_conflict = 1 THEN 'paused' ELSE monitor_status END,
            last_checked_at,
            CASE
                WHEN identity_conflict = 1 THEN
                    CASE
                        WHEN last_error IS NULL OR trim(last_error) = ''
                            THEN '旧版主播存在稳定直播入口冲突，已暂停等待人工处理'
                        ELSE last_error || '；旧版主播存在稳定直播入口冲突，已暂停等待人工处理'
                    END
                WHEN extracted_web_rid IS NOT NULL THEN last_error
                ELSE COALESCE(last_error, '旧版直播间链接无法提取稳定直播入口')
            END,
            created_at,
            updated_at
        FROM resolved;
        "#,
    )?;
    let migrated_count: i64 =
        transaction.query_row("SELECT COUNT(*) FROM streamers_v2", [], |row| row.get(0))?;
    if original_count != migrated_count {
        return Err(DatabaseError::MigrationIntegrity(format!(
            "主播记录数量从 {original_count} 变为 {migrated_count}"
        )));
    }
    transaction.execute_batch(
        r#"
        DROP TABLE streamers;
        ALTER TABLE streamers_v2 RENAME TO streamers;
        CREATE UNIQUE INDEX idx_streamers_profile_sec_uid
            ON streamers(profile_sec_uid) WHERE profile_sec_uid IS NOT NULL;
        CREATE UNIQUE INDEX idx_streamers_web_rid
            ON streamers(web_rid) WHERE web_rid IS NOT NULL;
        "#,
    )?;
    transaction.execute(
        "INSERT INTO schema_migrations(version, applied_at) VALUES(2, ?1)",
        [Utc::now().to_rfc3339()],
    )?;
    transaction.commit()?;
    Ok(())
}

fn recover_manifest_segments(
    output_root: &str,
    room_id: &str,
    started_at: &str,
    ended_at: Option<&str>,
) -> Vec<std::path::PathBuf> {
    let room_directory = Path::new(output_root).join(room_id);
    let session_started = chrono::DateTime::parse_from_rfc3339(started_at)
        .map(|value| value.with_timezone(&Utc) - chrono::Duration::minutes(5))
        .ok();
    let session_ended = ended_at
        .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
        .map(|value| value.with_timezone(&Utc) + chrono::Duration::minutes(5));
    let Ok(entries) = std::fs::read_dir(room_directory) else {
        return Vec::new();
    };
    let mut recovered = Vec::new();
    for entry in entries.flatten() {
        let directory = entry.path();
        if !directory.is_dir() {
            continue;
        }
        if let Ok(modified) = entry.metadata().and_then(|metadata| metadata.modified()) {
            let modified = chrono::DateTime::<Utc>::from(modified);
            if session_started.is_some_and(|started| modified < started)
                || session_ended.is_some_and(|ended| modified > ended)
            {
                continue;
            }
        }
        let Ok(manifest) = std::fs::read_to_string(directory.join("segments.csv")) else {
            continue;
        };
        let mut reader = csv::ReaderBuilder::new()
            .has_headers(false)
            .flexible(true)
            .from_reader(manifest.as_bytes());
        for record in reader.records().flatten() {
            let Some(value) = record.get(0).filter(|value| !value.is_empty()) else {
                continue;
            };
            let manifest_path = std::path::PathBuf::from(value);
            let path = if manifest_path.is_absolute() || manifest_path.exists() {
                manifest_path
            } else {
                directory.join(manifest_path)
            };
            if path.is_file() {
                recovered.push(path);
            }
        }
    }
    recovered.sort();
    recovered.dedup();
    recovered
}

fn replace_streamer_tags_in_transaction(
    transaction: &Transaction<'_>,
    streamer_id: i64,
    tags: &[StreamerTagInput],
) -> Result<()> {
    transaction.execute(
        "DELETE FROM streamer_tags WHERE streamer_id = ?1",
        [streamer_id],
    )?;
    insert_streamer_tags_in_transaction(transaction, streamer_id, tags)
}

fn load_streamer_tag_inputs(
    connection: &Connection,
    streamer_id: i64,
) -> Result<Vec<StreamerTagInput>> {
    let mut statement = connection.prepare(
        r#"
        SELECT name, prompt_guidance
        FROM streamer_tags
        WHERE streamer_id = ?1
        ORDER BY sort_order, id
        "#,
    )?;
    let rows = statement.query_map([streamer_id], |row| {
        Ok(StreamerTagInput {
            name: row.get(0)?,
            prompt_guidance: row.get(1)?,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

fn merge_streamer_tags(
    target_tags: Vec<StreamerTagInput>,
    source_tags: Vec<StreamerTagInput>,
) -> (Vec<StreamerTagInput>, bool) {
    let mut merged = target_tags;
    let mut positions = merged
        .iter()
        .enumerate()
        .map(|(index, tag)| (streamer_tag_name_key(&tag.name), index))
        .collect::<HashMap<_, _>>();
    let mut truncated = false;

    for source_tag in source_tags {
        let key = streamer_tag_name_key(&source_tag.name);
        if let Some(index) = positions.get(&key).copied() {
            if merged[index].prompt_guidance.is_none() && source_tag.prompt_guidance.is_some() {
                merged[index].prompt_guidance = source_tag.prompt_guidance;
            }
            continue;
        }
        if merged.len() >= crate::domain::MAX_STREAMER_TAGS {
            truncated = true;
            continue;
        }
        positions.insert(key, merged.len());
        merged.push(source_tag);
    }

    (merged, truncated)
}

fn insert_streamer_tags_in_transaction(
    transaction: &Transaction<'_>,
    streamer_id: i64,
    tags: &[StreamerTagInput],
) -> Result<()> {
    let now = Utc::now().to_rfc3339();
    for (sort_order, tag) in tags.iter().enumerate() {
        transaction.execute(
            r#"
            INSERT INTO streamer_tags(
                streamer_id, name, normalized_name, prompt_guidance,
                sort_order, created_at, updated_at
            ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?6)
            "#,
            params![
                streamer_id,
                tag.name,
                streamer_tag_name_key(&tag.name),
                tag.prompt_guidance,
                sort_order as i64,
                now,
            ],
        )?;
    }
    Ok(())
}

fn attach_tags_to_streamers(connection: &Connection, streamers: &mut [Streamer]) -> Result<()> {
    if streamers.is_empty() {
        return Ok(());
    }
    let placeholders = std::iter::repeat_n("?", streamers.len())
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        r#"
        SELECT id, streamer_id, name, prompt_guidance, sort_order
        FROM streamer_tags
        WHERE streamer_id IN ({placeholders})
        ORDER BY streamer_id, sort_order, id
        "#
    );
    let streamer_ids = streamers
        .iter()
        .map(|streamer| streamer.id)
        .collect::<Vec<_>>();
    let mut statement = connection.prepare(&sql)?;
    let rows = statement.query_map(params_from_iter(streamer_ids.iter()), |row| {
        Ok((
            row.get::<_, i64>(1)?,
            StreamerTag {
                id: row.get(0)?,
                name: row.get(2)?,
                prompt_guidance: row.get(3)?,
                sort_order: row.get(4)?,
            },
        ))
    })?;
    let mut grouped = HashMap::<i64, Vec<StreamerTag>>::new();
    for row in rows {
        let (streamer_id, tag) = row?;
        grouped.entry(streamer_id).or_default().push(tag);
    }
    for streamer in streamers {
        streamer.tags = grouped.remove(&streamer.id).unwrap_or_default();
    }
    Ok(())
}

fn streamer_select(suffix: &str) -> String {
    format!(
        r#"
        SELECT s.id, s.name, s.source_kind, s.source_url, s.profile_sec_uid,
               s.web_rid, s.room_url, s.room_id, s.monitor_enabled, s.archived,
               s.live_status, s.monitor_status, s.last_checked_at, s.last_error,
               s.failure_count, s.next_retry_at, s.recording_priority,
               (
                   SELECT COUNT(*) FROM videos v
                   JOIN recording_sessions rs ON rs.id = v.session_id
                   WHERE rs.streamer_id = s.id AND rs.ended_at IS NULL
               ) AS current_video_count,
               (
                   SELECT COUNT(*) FROM videos v
                   JOIN recording_sessions rs ON rs.id = v.session_id
                   WHERE rs.streamer_id = s.id
               ) AS history_video_count
        FROM streamers s
        {suffix}
        "#
    )
}

fn map_streamer(row: &rusqlite::Row<'_>) -> rusqlite::Result<Streamer> {
    let source_kind = match row.get::<_, String>(2)?.as_str() {
        "profile" => StreamerSourceKind::Profile,
        _ => StreamerSourceKind::Room,
    };
    Ok(Streamer {
        id: row.get(0)?,
        name: row.get(1)?,
        source_kind,
        source_url: row.get(3)?,
        profile_sec_uid: row.get(4)?,
        web_rid: row.get(5)?,
        room_url: row.get(6)?,
        room_id: row.get(7)?,
        monitor_enabled: row.get(8)?,
        archived: row.get(9)?,
        live_status: row.get(10)?,
        monitor_status: row.get(11)?,
        last_checked_at: row.get(12)?,
        last_error: row.get(13)?,
        failure_count: row.get(14)?,
        next_retry_at: row.get(15)?,
        recording_priority: row.get(16)?,
        current_video_count: row.get(17)?,
        history_video_count: row.get(18)?,
        tags: Vec::new(),
    })
}

fn source_kind_value(source_kind: StreamerSourceKind) -> &'static str {
    match source_kind {
        StreamerSourceKind::Profile => "profile",
        StreamerSourceKind::Room => "room",
    }
}

fn initial_monitor_status(input: &NewStreamer) -> &'static str {
    if !input.monitor_enabled {
        "paused"
    } else if input.source_kind == StreamerSourceKind::Profile && input.web_rid.is_none() {
        "waiting_first_live"
    } else {
        "waiting"
    }
}

fn duplicate_identity_error(input: &NewStreamer) -> DatabaseError {
    if input.profile_sec_uid.is_some() {
        DatabaseError::DuplicateProfile
    } else if input.web_rid.is_some() {
        DatabaseError::DuplicateWebRid
    } else {
        DatabaseError::DuplicateStreamer
    }
}

fn map_video(row: &rusqlite::Row<'_>) -> rusqlite::Result<Video> {
    let audio: Option<i64> = row.get(9)?;
    Ok(Video {
        id: row.get(0)?,
        session_id: row.get(1)?,
        streamer_id: row.get(2)?,
        streamer_name: row.get(3)?,
        path: row.get(4)?,
        started_at: row.get(5)?,
        ended_at: row.get(6)?,
        duration_seconds: row.get(7)?,
        size_bytes: row.get(8)?,
        audio_present: audio.map(|value| value != 0),
        status: row.get(10)?,
        has_preview_cache: false,
    })
}

fn value_or(
    values: &std::collections::HashMap<String, String>,
    key: &str,
    fallback: String,
) -> String {
    values.get(key).cloned().unwrap_or(fallback)
}

fn parse_or<T>(values: &std::collections::HashMap<String, String>, key: &str, fallback: T) -> T
where
    T: std::str::FromStr,
{
    values
        .get(key)
        .and_then(|value| value.parse().ok())
        .unwrap_or(fallback)
}
