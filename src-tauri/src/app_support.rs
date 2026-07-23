use dy_screen::error::RecorderError;
use dy_screen::profile_resolver::validate_profile_url;
use dy_screen::resolver::{RoomInspection, validate_room_url};

use crate::database::Database;
use crate::domain::AppSettings;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NormalizedStreamerSource {
    Profile {
        source_url: String,
        profile_sec_uid: String,
    },
    Room {
        source_url: String,
        web_rid: String,
    },
}

pub fn parse_streamer_source(input: &str) -> Result<NormalizedStreamerSource, String> {
    if let Ok(profile) = validate_profile_url(input.trim()) {
        return Ok(NormalizedStreamerSource::Profile {
            source_url: profile.source_url,
            profile_sec_uid: profile.profile_sec_uid,
        });
    }
    if let Ok((source_url, web_rid)) = parse_room_identity(input) {
        return Ok(NormalizedStreamerSource::Room {
            source_url,
            web_rid,
        });
    }
    Err("请输入有效的个人主页或直播间链接".to_owned())
}

pub fn parse_room_identity(input: &str) -> Result<(String, String), String> {
    let mut url =
        validate_room_url(input.trim()).map_err(|_| "请输入有效的抖音公开直播间链接".to_owned())?;
    let room_key = url
        .path_segments()
        .and_then(|mut segments| segments.find(|segment| !segment.is_empty()))
        .filter(|segment| segment.chars().all(|character| character.is_ascii_digit()))
        .ok_or_else(|| "直播间链接中缺少有效的房间号".to_owned())?
        .to_owned();
    url.set_query(None);
    url.set_fragment(None);
    Ok((url.to_string().trim_end_matches('/').to_owned(), room_key))
}

pub fn validate_room_access(
    result: std::result::Result<RoomInspection, RecorderError>,
) -> Result<String, String> {
    match result {
        Ok(inspection) => Ok(inspection.room_id().to_owned()),
        Err(RecorderError::PageRequest(_)) => {
            Err("无法访问直播间，请检查网络或确认链接仍然有效".to_owned())
        }
        Err(RecorderError::RoomAccessRestricted) => {
            Err("该直播间当前返回验证码或访问验证页面，无法完成公开访问校验".to_owned())
        }
        Err(RecorderError::RoomHttpStatus { status: 404 | 410 }) => {
            Err("直播入口不存在或已失效".to_owned())
        }
        Err(RecorderError::RoomHttpStatus { status }) => {
            Err(format!("直播间暂时无法访问（HTTP {status}）"))
        }
        Err(RecorderError::UnsupportedPageLayout) => {
            Err("无法识别该直播间页面，请确认它是公开抖音直播间".to_owned())
        }
        Err(RecorderError::NoStreamVariant) => Err("直播间当前没有可用的公开音视频流".to_owned()),
        Err(RecorderError::InvalidRoomUrl { .. } | RecorderError::UnsupportedRoomUrl { .. }) => {
            Err("请输入有效的抖音公开直播间链接".to_owned())
        }
        Err(_) => Err("直播间访问校验失败，请稍后重试".to_owned()),
    }
}

pub fn delete_recording_session(database: &Database, session_id: i64) -> Result<(), String> {
    let session = database
        .get_session(session_id)
        .map_err(|error| error.to_string())?;
    if session.ended_at.is_none() {
        return Err("活动录制会话不能删除，请先停止录制".to_owned());
    }
    let videos = database
        .list_session_videos(session_id)
        .map_err(|error| error.to_string())?;
    let original_statuses = videos
        .iter()
        .map(|video| (video.id, video.status.clone()))
        .collect::<Vec<_>>();
    let pending_statuses = videos
        .iter()
        .map(|video| (video.id, "pending_delete".to_owned()))
        .collect::<Vec<_>>();

    for video in &videos {
        let path = std::path::Path::new(&video.path);
        if path.exists() && !path.is_file() {
            return Err(format!("删除录制会话失败：{} 不是普通文件", path.display()));
        }
    }
    database
        .set_video_statuses(&pending_statuses)
        .map_err(|error| error.to_string())?;

    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let mut staged_files = Vec::new();
    for video in &videos {
        let original = std::path::PathBuf::from(&video.path);
        if !original.exists() {
            continue;
        }
        let file_name = original
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("video.mkv");
        let staged = original.with_file_name(format!(
            ".{file_name}.dy-screen-delete-{session_id}-{}-{nonce}",
            video.id
        ));
        if let Err(error) = std::fs::rename(&original, &staged) {
            rollback_staged_files(&staged_files);
            let _ = database.set_video_statuses(&original_statuses);
            return Err(format!("删除录制会话失败：{error}"));
        }
        staged_files.push((original, staged));
    }
    if let Err(error) = database.delete_session_records(session_id) {
        rollback_staged_files(&staged_files);
        let _ = database.set_video_statuses(&original_statuses);
        return Err(error.to_string());
    }
    let cleanup_errors = staged_files
        .iter()
        .filter_map(|(_, staged)| remove_video_file(staged).err())
        .map(|error| error.to_string())
        .collect::<Vec<_>>();
    if !cleanup_errors.is_empty() {
        return Err(format!(
            "录制会话已从视频库删除，但隔离文件清理失败：{}",
            cleanup_errors.join("；")
        ));
    }
    Ok(())
}

pub fn delete_recording_video(database: &Database, video_id: i64) -> Result<(), String> {
    let video = database
        .get_video(video_id)
        .map_err(|error| error.to_string())?;
    database
        .mark_video_status(video_id, "pending_delete")
        .map_err(|error| error.to_string())?;
    let original = std::path::PathBuf::from(&video.path);
    if original.exists() && !original.is_file() {
        let _ = database.mark_video_status(video_id, &video.status);
        return Err("删除视频失败：目标不是普通文件".to_owned());
    }
    let staged = if original.exists() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default();
        let file_name = original
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("video.mkv");
        let staged =
            original.with_file_name(format!(".{file_name}.dy-screen-delete-{video_id}-{nonce}"));
        if let Err(error) = std::fs::rename(&original, &staged) {
            let _ = database.mark_video_status(video_id, &video.status);
            return Err(format!("删除视频失败：{error}"));
        }
        Some(staged)
    } else {
        None
    };
    if let Err(error) = database.delete_video_record(video_id) {
        if let Some(staged) = &staged {
            let _ = std::fs::rename(staged, &original);
        }
        let _ = database.mark_video_status(video_id, &video.status);
        return Err(error.to_string());
    }
    if let Some(staged) = staged
        && let Err(error) = remove_video_file(&staged)
    {
        return Err(format!("视频已从资料库删除，但隔离文件清理失败：{error}"));
    }
    Ok(())
}

fn rollback_staged_files(files: &[(std::path::PathBuf, std::path::PathBuf)]) {
    for (original, staged) in files.iter().rev() {
        if staged.exists() {
            let _ = std::fs::rename(staged, original);
        }
    }
}

fn remove_video_file(path: &std::path::Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

pub fn validate_settings(settings: &AppSettings) -> Result<(), String> {
    if settings.output_root.trim().is_empty() {
        return Err("录像保存目录不能为空".to_owned());
    }
    if settings.segment_seconds < 60 {
        return Err("分片时长不能少于 60 秒".to_owned());
    }
    if !(1..=16).contains(&settings.max_concurrent_recordings) {
        return Err("最大并发录制数必须在 1 到 16 之间".to_owned());
    }
    if !matches!(settings.protocol.as_str(), "flv" | "hls") {
        return Err("录制协议只支持 flv 或 hls".to_owned());
    }
    if !matches!(
        settings.quality.as_str(),
        "FULL_HD1" | "HD1" | "SD1" | "SD2"
    ) {
        return Err("请选择受支持的录制清晰度".to_owned());
    }
    if settings.ffmpeg_path.trim().is_empty() || settings.ffprobe_path.trim().is_empty() {
        return Err("FFmpeg 和 FFprobe 路径不能为空".to_owned());
    }
    Ok(())
}
