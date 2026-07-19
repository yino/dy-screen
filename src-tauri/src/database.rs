use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use chrono::Utc;
use rusqlite::{Connection, ErrorCode, OptionalExtension, params};
use thiserror::Error;

use crate::domain::{
    AppSettings, Dashboard, NewStreamer, NewVideo, RecordingSession, Streamer, Video, VideoFilter,
    VideoPage,
};

#[derive(Debug, Error)]
pub enum DatabaseError {
    #[error("数据库错误：{0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("文件系统错误：{0}")]
    Io(#[from] std::io::Error),
    #[error("该直播间已经存在")]
    DuplicateStreamer,
    #[error("找不到记录：{0}")]
    NotFound(&'static str),
    #[error("数据库锁已损坏")]
    Poisoned,
}

pub type Result<T> = std::result::Result<T, DatabaseError>;

#[derive(Clone)]
pub struct Database {
    connection: Arc<Mutex<Connection>>,
}

struct SessionManifestScope {
    id: i64,
    output_root: String,
    started_at: String,
    ended_at: Option<String>,
    room_id: String,
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

    fn connection(&self) -> Result<MutexGuard<'_, Connection>> {
        self.connection.lock().map_err(|_| DatabaseError::Poisoned)
    }

    pub fn migrate(&self) -> Result<()> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        transaction.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS schema_migrations (
                version INTEGER PRIMARY KEY,
                applied_at TEXT NOT NULL
            );
            "#,
        )?;
        let applied = transaction
            .query_row(
                "SELECT 1 FROM schema_migrations WHERE version = 1",
                [],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if !applied {
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
        }
        transaction.commit()?;
        drop(connection);
        self.ensure_default_settings()
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
        let now = Utc::now().to_rfc3339();
        let monitor_status = if input.monitor_enabled {
            "waiting"
        } else {
            "paused"
        };
        let connection = self.connection()?;
        let inserted = connection.execute(
            r#"
            INSERT INTO streamers(
                name, room_url, room_id, monitor_enabled, archived,
                live_status, monitor_status, created_at, updated_at
            ) VALUES(?1, ?2, ?3, ?4, 0, 'checking', ?5, ?6, ?6)
            "#,
            params![
                input.name.trim(),
                input.room_url,
                input.room_id,
                input.monitor_enabled,
                monitor_status,
                now
            ],
        );
        match inserted {
            Ok(_) => {
                let id = connection.last_insert_rowid();
                drop(connection);
                self.get_streamer(id)
            }
            Err(error) if error.sqlite_error_code() == Some(ErrorCode::ConstraintViolation) => {
                Err(DatabaseError::DuplicateStreamer)
            }
            Err(error) => Err(error.into()),
        }
    }

    pub fn get_streamer(&self, id: i64) -> Result<Streamer> {
        let connection = self.connection()?;
        connection
            .query_row(&streamer_select("WHERE s.id = ?1"), [id], map_streamer)
            .optional()?
            .ok_or(DatabaseError::NotFound("主播"))
    }

    pub fn find_streamer_by_room_id(&self, room_id: &str) -> Result<Option<Streamer>> {
        let connection = self.connection()?;
        connection
            .query_row(
                &streamer_select("WHERE s.room_id = ?1"),
                [room_id],
                map_streamer,
            )
            .optional()
            .map_err(Into::into)
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
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn set_monitor_enabled(&self, id: i64, enabled: bool) -> Result<()> {
        let status = if enabled { "waiting" } else { "paused" };
        let changed = self.connection()?.execute(
            "UPDATE streamers SET monitor_enabled = ?1, monitor_status = ?2, updated_at = ?3 WHERE id = ?4 AND archived = 0",
            params![enabled, status, Utc::now().to_rfc3339(), id],
        )?;
        if changed == 0 {
            return Err(DatabaseError::NotFound("主播"));
        }
        Ok(())
    }

    pub fn update_streamer(&self, id: i64, input: &NewStreamer) -> Result<Streamer> {
        let monitor_status = if input.monitor_enabled {
            "waiting"
        } else {
            "paused"
        };
        let connection = self.connection()?;
        let updated = connection.execute(
            r#"
            UPDATE streamers
            SET name = ?1, room_url = ?2, room_id = ?3, monitor_enabled = ?4,
                live_status = 'checking', monitor_status = ?5, last_error = NULL,
                updated_at = ?6
            WHERE id = ?7 AND archived = 0
            "#,
            params![
                input.name.trim(),
                input.room_url,
                input.room_id,
                input.monitor_enabled,
                monitor_status,
                Utc::now().to_rfc3339(),
                id
            ],
        );
        match updated {
            Ok(0) => Err(DatabaseError::NotFound("主播")),
            Ok(_) => {
                drop(connection);
                self.get_streamer(id)
            }
            Err(error) if error.sqlite_error_code() == Some(ErrorCode::ConstraintViolation) => {
                Err(DatabaseError::DuplicateStreamer)
            }
            Err(error) => Err(error.into()),
        }
    }

    pub fn restore_streamer(&self, id: i64, input: &NewStreamer) -> Result<Streamer> {
        let monitor_status = if input.monitor_enabled {
            "waiting"
        } else {
            "paused"
        };
        let connection = self.connection()?;
        let changed = connection.execute(
            r#"
            UPDATE streamers
            SET name = ?1, room_url = ?2, room_id = ?3, monitor_enabled = ?4,
                archived = 0, live_status = 'checking', monitor_status = ?5,
                last_checked_at = NULL, last_error = NULL, updated_at = ?6
            WHERE id = ?7 AND archived = 1
            "#,
            params![
                input.name.trim(),
                input.room_url,
                input.room_id,
                input.monitor_enabled,
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
                last_error = ?4, updated_at = ?3
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

    pub fn archive_streamer(&self, id: i64) -> Result<()> {
        self.connection()?.execute(
            "UPDATE streamers SET archived = 1, monitor_enabled = 0, monitor_status = 'paused', updated_at = ?1 WHERE id = ?2",
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

    pub fn update_session_retry(&self, id: i64, retry_count: i64) -> Result<()> {
        self.connection()?.execute(
            "UPDATE recording_sessions SET retry_count = ?1, status = 'retrying' WHERE id = ?2",
            params![retry_count, id],
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

    pub fn add_video(&self, video: &NewVideo) -> Result<()> {
        self.connection()?.execute(
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
        Ok(())
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
        for path in recover_manifest_segments(
            &session.output_root,
            &session.room_id,
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
        })
    }
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

fn streamer_select(suffix: &str) -> String {
    format!(
        r#"
        SELECT s.id, s.name, s.room_url, s.room_id, s.monitor_enabled, s.archived,
               s.live_status, s.monitor_status, s.last_checked_at, s.last_error,
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
    Ok(Streamer {
        id: row.get(0)?,
        name: row.get(1)?,
        room_url: row.get(2)?,
        room_id: row.get(3)?,
        monitor_enabled: row.get(4)?,
        archived: row.get(5)?,
        live_status: row.get(6)?,
        monitor_status: row.get(7)?,
        last_checked_at: row.get(8)?,
        last_error: row.get(9)?,
        current_video_count: row.get(10)?,
        history_video_count: row.get(11)?,
    })
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
