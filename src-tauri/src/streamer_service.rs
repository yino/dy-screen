use async_trait::async_trait;
use dy_screen::error::{RecorderError, Result as RecorderResult};
use dy_screen::model::ProfileInspection;
use dy_screen::profile_resolver::ProfileResolver;
use dy_screen::resolver::{RoomInspection, StreamResolver};

use crate::app_support::{NormalizedStreamerSource, parse_streamer_source};
use crate::database::{Database, DatabaseError};
use crate::domain::{
    CommandError, CreateStreamerRequest, NewStreamer, Streamer, StreamerSourceKind,
};
use crate::supervisor::Supervisor;

#[async_trait]
pub trait SourceInspector: Send + Sync {
    async fn inspect_profile(&self, source_url: &str) -> RecorderResult<ProfileInspection>;
    async fn inspect_room(&self, source_url: &str) -> RecorderResult<RoomInspection>;
}

pub struct PublicSourceInspector {
    profile_resolver: ProfileResolver,
    room_resolver: StreamResolver,
}

impl PublicSourceInspector {
    pub fn new() -> Result<Self, CommandError> {
        Ok(Self {
            profile_resolver: ProfileResolver::new().map_err(map_profile_error)?,
            room_resolver: StreamResolver::new().map_err(map_room_error)?,
        })
    }
}

#[async_trait]
impl SourceInspector for PublicSourceInspector {
    async fn inspect_profile(&self, source_url: &str) -> RecorderResult<ProfileInspection> {
        self.profile_resolver.inspect(source_url).await
    }

    async fn inspect_room(&self, source_url: &str) -> RecorderResult<RoomInspection> {
        self.room_resolver.inspect(source_url).await
    }
}

#[async_trait]
pub trait WorkerControl: Send + Sync {
    async fn start(&self, streamer_id: i64) -> Result<(), String>;
    async fn stop(&self, streamer_id: i64) -> Result<(), String>;
}

#[async_trait]
impl WorkerControl for Supervisor {
    async fn start(&self, streamer_id: i64) -> Result<(), String> {
        self.resume(streamer_id).await
    }

    async fn stop(&self, streamer_id: i64) -> Result<(), String> {
        Supervisor::stop(self, streamer_id).await
    }
}

pub async fn create_streamer_with(
    database: &Database,
    inspector: &dyn SourceInspector,
    worker: &dyn WorkerControl,
    input: CreateStreamerRequest,
) -> Result<Streamer, CommandError> {
    let prepared = prepare_new_streamer(&input, inspector).await?;

    if let Some(profile_sec_uid) = prepared.profile_sec_uid.as_deref()
        && let Some(existing) = database
            .find_streamer_by_profile_sec_uid(profile_sec_uid)
            .map_err(map_database_error)?
    {
        if !existing.archived {
            return Err(duplicate_error(
                "duplicate_profile",
                "该个人主页已经存在",
                existing.id,
            ));
        }
        let restored = database
            .restore_streamer(existing.id, &prepared)
            .map_err(map_database_error)?;
        start_if_enabled(worker, &restored).await?;
        return Ok(restored);
    }

    if let Some(web_rid) = prepared.web_rid.as_deref()
        && let Some(existing) = database
            .find_streamer_by_web_rid(web_rid)
            .map_err(map_database_error)?
    {
        if existing.archived {
            let restored = database
                .restore_streamer(existing.id, &prepared)
                .map_err(map_database_error)?;
            start_if_enabled(worker, &restored).await?;
            return Ok(restored);
        }
        if prepared.source_kind == StreamerSourceKind::Profile && existing.profile_sec_uid.is_none()
        {
            let mut attached = prepared;
            attached.monitor_enabled |= existing.monitor_enabled;
            let updated = database
                .update_streamer(existing.id, &attached)
                .map_err(map_database_error)?;
            start_if_enabled(worker, &updated).await?;
            return Ok(updated);
        }
        return Err(duplicate_error(
            "duplicate_web_rid",
            "该稳定直播入口已经存在",
            existing.id,
        ));
    }

    let streamer = database
        .add_streamer(&prepared)
        .map_err(map_database_error)?;
    start_if_enabled(worker, &streamer).await?;
    Ok(streamer)
}

pub async fn update_streamer_with(
    database: &Database,
    inspector: &dyn SourceInspector,
    worker: &dyn WorkerControl,
    streamer_id: i64,
    input: CreateStreamerRequest,
) -> Result<Streamer, CommandError> {
    let current = database
        .get_streamer(streamer_id)
        .map_err(map_database_error)?;
    let source = parse_streamer_source(&input.source_url)
        .map_err(|message| map_source_parse_error(&input.source_url, message))?;

    if normalized_source_url(&source) == current.source_url {
        return update_without_source_change(database, worker, current, input).await;
    }

    if current.monitor_enabled {
        worker.stop(streamer_id).await.map_err(map_worker_error)?;
    }
    let update_result =
        update_changed_source(database, inspector, worker, streamer_id, input, source).await;
    if update_result.is_err() && current.monitor_enabled {
        let _ = worker.start(streamer_id).await;
    }
    update_result
}

async fn update_changed_source(
    database: &Database,
    inspector: &dyn SourceInspector,
    worker: &dyn WorkerControl,
    streamer_id: i64,
    input: CreateStreamerRequest,
    source: NormalizedStreamerSource,
) -> Result<Streamer, CommandError> {
    let prepared = prepare_normalized_streamer(&input, source, inspector).await?;
    if let Some(profile_sec_uid) = prepared.profile_sec_uid.as_deref()
        && let Some(existing) = database
            .find_streamer_by_profile_sec_uid(profile_sec_uid)
            .map_err(map_database_error)?
        && existing.id != streamer_id
    {
        return Err(duplicate_error(
            "duplicate_profile",
            "该个人主页已经存在",
            existing.id,
        ));
    }
    if let Some(web_rid) = prepared.web_rid.as_deref()
        && let Some(existing) = database
            .find_streamer_by_web_rid(web_rid)
            .map_err(map_database_error)?
        && existing.id != streamer_id
    {
        return Err(duplicate_error(
            "duplicate_web_rid",
            "该稳定直播入口已经存在",
            existing.id,
        ));
    }

    let updated = database
        .update_streamer(streamer_id, &prepared)
        .map_err(map_database_error)?;
    start_if_enabled(worker, &updated).await?;
    Ok(updated)
}

async fn update_without_source_change(
    database: &Database,
    worker: &dyn WorkerControl,
    current: Streamer,
    input: CreateStreamerRequest,
) -> Result<Streamer, CommandError> {
    let name = normalized_name(&input.name, Some(&current.name), current.source_kind)?;
    if current.monitor_enabled && !input.monitor_enabled {
        worker.stop(current.id).await.map_err(map_worker_error)?;
    }
    let updated = database
        .update_streamer(
            current.id,
            &NewStreamer {
                name,
                source_kind: current.source_kind,
                source_url: current.source_url,
                profile_sec_uid: current.profile_sec_uid,
                web_rid: current.web_rid,
                room_url: current.room_url,
                room_id: current.room_id,
                monitor_enabled: input.monitor_enabled,
            },
        )
        .map_err(map_database_error)?;
    if !current.monitor_enabled && updated.monitor_enabled {
        worker.start(updated.id).await.map_err(map_worker_error)?;
    }
    Ok(updated)
}

async fn prepare_new_streamer(
    input: &CreateStreamerRequest,
    inspector: &dyn SourceInspector,
) -> Result<NewStreamer, CommandError> {
    let source = parse_streamer_source(&input.source_url)
        .map_err(|message| map_source_parse_error(&input.source_url, message))?;
    prepare_normalized_streamer(input, source, inspector).await
}

async fn prepare_normalized_streamer(
    input: &CreateStreamerRequest,
    source: NormalizedStreamerSource,
    inspector: &dyn SourceInspector,
) -> Result<NewStreamer, CommandError> {
    match source {
        NormalizedStreamerSource::Profile {
            source_url,
            profile_sec_uid,
        } => {
            let inspection = inspector
                .inspect_profile(&source_url)
                .await
                .map_err(map_profile_error)?;
            let (identity, room) = match inspection {
                ProfileInspection::Offline { identity } => (identity, None),
                ProfileInspection::Live { identity, room } => (identity, Some(room)),
            };
            if identity.profile_sec_uid != profile_sec_uid {
                return Err(CommandError::new(
                    "profile_identity_mismatch",
                    "个人主页身份与页面数据不一致",
                )
                .field("sourceUrl"));
            }
            let name = normalized_name(
                &input.name,
                identity.display_name.as_deref(),
                StreamerSourceKind::Profile,
            )?;
            Ok(NewStreamer {
                name,
                source_kind: StreamerSourceKind::Profile,
                source_url,
                profile_sec_uid: Some(profile_sec_uid),
                web_rid: room.as_ref().map(|room| room.web_rid.clone()),
                room_url: room.as_ref().map(|room| room.room_url.clone()),
                room_id: room.and_then(|room| room.room_id),
                monitor_enabled: input.monitor_enabled,
            })
        }
        NormalizedStreamerSource::Room {
            source_url,
            web_rid,
        } => {
            let name = normalized_name(&input.name, None, StreamerSourceKind::Room)?;
            let inspection = inspector
                .inspect_room(&source_url)
                .await
                .map_err(map_room_error)?;
            Ok(NewStreamer {
                name,
                source_kind: StreamerSourceKind::Room,
                source_url: source_url.clone(),
                profile_sec_uid: None,
                web_rid: Some(web_rid),
                room_url: Some(source_url),
                room_id: Some(inspection.room_id().to_owned()),
                monitor_enabled: input.monitor_enabled,
            })
        }
    }
}

fn normalized_name(
    requested: &str,
    fallback: Option<&str>,
    source_kind: StreamerSourceKind,
) -> Result<String, CommandError> {
    let name = if requested.trim().is_empty() {
        fallback.unwrap_or_default().trim()
    } else {
        requested.trim()
    };
    if name.is_empty() {
        let message = if source_kind == StreamerSourceKind::Profile {
            "请输入主播名称，当前个人主页未提供昵称"
        } else {
            "直播间链接必须填写主播名称"
        };
        return Err(CommandError::new("name_required", message).field("name"));
    }
    if name.chars().count() > 80 {
        return Err(CommandError::new("name_too_long", "主播名称不能超过 80 个字符").field("name"));
    }
    Ok(name.to_owned())
}

fn normalized_source_url(source: &NormalizedStreamerSource) -> &str {
    match source {
        NormalizedStreamerSource::Profile { source_url, .. }
        | NormalizedStreamerSource::Room { source_url, .. } => source_url,
    }
}

async fn start_if_enabled(
    worker: &dyn WorkerControl,
    streamer: &Streamer,
) -> Result<(), CommandError> {
    if streamer.monitor_enabled {
        worker.start(streamer.id).await.map_err(map_worker_error)?;
    }
    Ok(())
}

fn duplicate_error(code: &str, message: &str, streamer_id: i64) -> CommandError {
    CommandError::new(code, format!("{message}，主播 ID 为 {streamer_id}"))
        .field("sourceUrl")
        .existing_streamer(streamer_id)
}

fn map_source_parse_error(input: &str, message: String) -> CommandError {
    let normalized = input.trim().to_ascii_lowercase();
    let code = if normalized.contains("douyin.com/user") {
        "invalid_profile_url"
    } else if normalized.contains("live.douyin.com") {
        "invalid_room_url"
    } else {
        "invalid_source_url"
    };
    CommandError::new(code, message).field("sourceUrl")
}

fn map_profile_error(error: RecorderError) -> CommandError {
    match error {
        RecorderError::InvalidProfileUrl { .. } | RecorderError::UnsupportedProfileUrl => {
            CommandError::new("invalid_profile_url", "请输入有效的公开抖音个人主页链接")
                .field("sourceUrl")
        }
        RecorderError::ProfilePageRequest { .. } | RecorderError::ProfileHttpStatus { .. } => {
            CommandError::new("profile_unavailable", "暂时无法访问该个人主页，请稍后重试")
                .field("sourceUrl")
        }
        RecorderError::ProfileAccessRestricted => CommandError::new(
            "profile_access_restricted",
            "该个人主页当前需要登录、验证码或额外访问权限",
        )
        .field("sourceUrl"),
        RecorderError::UnsupportedPageLayout => CommandError::new(
            "profile_layout_changed",
            "无法识别该个人主页，页面结构可能已经变化",
        )
        .field("sourceUrl"),
        _ => CommandError::new("profile_check_failed", "个人主页校验失败，请稍后重试")
            .field("sourceUrl"),
    }
}

fn map_room_error(error: RecorderError) -> CommandError {
    match error {
        RecorderError::InvalidRoomUrl { .. } | RecorderError::UnsupportedRoomUrl { .. } => {
            CommandError::new("invalid_room_url", "请输入有效的抖音公开直播间链接")
                .field("sourceUrl")
        }
        RecorderError::PageRequest(_) => CommandError::new(
            "room_unavailable",
            "无法访问直播间，请检查网络或确认链接仍然有效",
        )
        .field("sourceUrl"),
        RecorderError::UnsupportedPageLayout => CommandError::new(
            "room_layout_changed",
            "无法识别该直播间页面，请确认它是公开抖音直播间",
        )
        .field("sourceUrl"),
        _ => CommandError::new("room_check_failed", "直播间访问校验失败，请稍后重试")
            .field("sourceUrl"),
    }
}

fn map_database_error(error: DatabaseError) -> CommandError {
    match error {
        DatabaseError::DuplicateProfile => {
            CommandError::new("duplicate_profile", "该个人主页已经存在").field("sourceUrl")
        }
        DatabaseError::DuplicateWebRid | DatabaseError::DuplicateStreamer => {
            CommandError::new("duplicate_web_rid", "该稳定直播入口已经存在").field("sourceUrl")
        }
        DatabaseError::NotFound(entity) => {
            CommandError::new("not_found", format!("找不到记录：{entity}"))
        }
        other => CommandError::new("database_error", other.to_string()),
    }
}

fn map_worker_error(message: String) -> CommandError {
    CommandError::new("worker_control_failed", message)
}
