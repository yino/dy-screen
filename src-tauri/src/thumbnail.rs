use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::sync::{Notify, Semaphore};
use tokio_util::sync::CancellationToken;

use crate::domain::Video;

pub const THUMBNAIL_PROFILE_VERSION: u32 = 1;
pub const THUMBNAIL_MAX_EDGE: u32 = 480;
pub const THUMBNAIL_MAX_BATCH_SIZE: usize = 50;
pub const THUMBNAIL_CONCURRENCY: usize = 2;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ThumbnailState {
    Queued,
    Ready,
    Failed,
    Unavailable,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ThumbnailSourceKind {
    Original,
    PreviewCache,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ThumbnailFailure {
    pub code: String,
    pub message: String,
}

impl ThumbnailFailure {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ThumbnailFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ThumbnailFailure {}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ThumbnailMedia {
    pub path: String,
    pub mime_type: String,
    pub width: u32,
    pub height: u32,
    pub cache_hit: bool,
    pub source_kind: ThumbnailSourceKind,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ThumbnailSnapshot {
    pub batch_id: String,
    pub video_id: i64,
    pub cache_key: Option<String>,
    pub state: ThumbnailState,
    pub media: Option<ThumbnailMedia>,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ThumbnailBatch {
    pub batch_id: String,
    pub items: Vec<ThumbnailSnapshot>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ThumbnailEvent {
    pub batch_id: String,
    pub item: ThumbnailSnapshot,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
pub struct ThumbnailSourceIdentity {
    pub video_id: i64,
    pub canonical_path: String,
    pub size_bytes: u64,
    pub modified_millis: u128,
    pub source_kind: ThumbnailSourceKind,
    pub profile_version: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ThumbnailCacheManifest {
    pub key: String,
    pub video_id: i64,
    pub source: ThumbnailSourceIdentity,
    pub created_at_millis: u128,
    pub last_accessed_at_millis: u128,
    pub width: u32,
    pub height: u32,
    pub image_file: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThumbnailCachePaths {
    pub image: PathBuf,
    pub temporary_image: PathBuf,
    pub manifest: PathBuf,
    pub temporary_manifest: PathBuf,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ThumbnailCleanupReport {
    pub removed_parts: usize,
    pub removed_entries: usize,
    pub remaining_bytes: u64,
}

#[derive(Debug, Clone)]
pub struct ThumbnailCache {
    root: PathBuf,
    retention: Duration,
    max_bytes: u64,
}

impl ThumbnailCache {
    pub fn new(root: PathBuf) -> Self {
        Self::with_limits(
            root,
            Duration::from_secs(30 * 24 * 60 * 60),
            512 * 1024 * 1024,
        )
    }

    pub fn with_limits(root: PathBuf, retention: Duration, max_bytes: u64) -> Self {
        Self {
            root,
            retention,
            max_bytes,
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn initialize(&self) -> io::Result<()> {
        if let Ok(metadata) = fs::symlink_metadata(&self.root)
            && metadata.file_type().is_symlink()
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "缩略图缓存目录不能是符号链接",
            ));
        }
        fs::create_dir_all(&self.root)?;
        let metadata = fs::symlink_metadata(&self.root)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "缩略图缓存位置不是普通目录",
            ));
        }
        Ok(())
    }

    pub fn validate_key(&self, key: &str) -> io::Result<()> {
        if is_safe_cache_key(key) {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "缩略图缓存键无效",
            ))
        }
    }

    pub fn paths(&self, key: &str) -> ThumbnailCachePaths {
        ThumbnailCachePaths {
            image: self.root.join(format!("{key}.jpg")),
            temporary_image: self.root.join(format!("{key}.part.jpg")),
            manifest: self.root.join(format!("{key}.json")),
            temporary_manifest: self.root.join(format!("{key}.part.json")),
        }
    }

    pub fn publish(
        &self,
        identity: &ThumbnailSourceIdentity,
        width: u32,
        height: u32,
        now_millis: u128,
    ) -> io::Result<ThumbnailCacheManifest> {
        self.initialize()?;
        let key = thumbnail_cache_key(identity);
        self.validate_key(&key)?;
        let paths = self.paths(&key);
        let measured = validate_thumbnail_jpeg(&paths.temporary_image, THUMBNAIL_MAX_EDGE)?;
        if measured != (width, height) {
            remove_if_exists(&paths.temporary_image)?;
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "缩略图尺寸与执行结果不一致",
            ));
        }
        let manifest = ThumbnailCacheManifest {
            key: key.clone(),
            video_id: identity.video_id,
            source: identity.clone(),
            created_at_millis: now_millis,
            last_accessed_at_millis: now_millis,
            width,
            height,
            image_file: format!("{key}.jpg"),
        };
        let bytes = serde_json::to_vec_pretty(&manifest).map_err(io::Error::other)?;
        fs::write(&paths.temporary_manifest, bytes)?;
        replace_file(&paths.temporary_image, &paths.image)?;
        if let Err(error) = replace_file(&paths.temporary_manifest, &paths.manifest) {
            let _ = remove_if_exists(&paths.image);
            return Err(error);
        }
        Ok(manifest)
    }

    pub fn read_manifest(&self, key: &str) -> io::Result<Option<ThumbnailCacheManifest>> {
        if self.validate_key(key).is_err() {
            return Ok(None);
        }
        let paths = self.paths(key);
        let bytes = match fs::read(&paths.manifest) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let Ok(manifest) = serde_json::from_slice::<ThumbnailCacheManifest>(&bytes) else {
            self.remove_key_files(key)?;
            return Ok(None);
        };
        let expected_image_file = format!("{key}.jpg");
        if manifest.key != key
            || manifest.video_id != manifest.source.video_id
            || manifest.image_file != expected_image_file
            || !is_regular_file_without_symlink(&paths.image)
            || validate_thumbnail_jpeg(&paths.image, THUMBNAIL_MAX_EDGE).ok()
                != Some((manifest.width, manifest.height))
        {
            self.remove_key_files(key)?;
            return Ok(None);
        }
        let canonical_root = fs::canonicalize(&self.root)?;
        let canonical_image = fs::canonicalize(&paths.image)?;
        if canonical_image.parent() != Some(canonical_root.as_path()) {
            self.remove_key_files(key)?;
            return Ok(None);
        }
        Ok(Some(manifest))
    }

    pub fn lookup(
        &self,
        identity: &ThumbnailSourceIdentity,
        now_millis: u128,
    ) -> io::Result<Option<ThumbnailCacheManifest>> {
        let key = thumbnail_cache_key(identity);
        let Some(mut manifest) = self.read_manifest(&key)? else {
            return Ok(None);
        };
        if manifest.source != *identity {
            self.remove_entry(&manifest)?;
            return Ok(None);
        }
        manifest.last_accessed_at_millis = now_millis;
        self.write_manifest(&manifest)?;
        Ok(Some(manifest))
    }

    pub fn manifests(&self) -> io::Result<Vec<ThumbnailCacheManifest>> {
        self.initialize()?;
        let mut manifests = Vec::new();
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json")
                || path
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().contains(".part."))
            {
                continue;
            }
            let Some(key) = path.file_stem().and_then(|value| value.to_str()) else {
                remove_if_exists(&path)?;
                continue;
            };
            if let Some(manifest) = self.read_manifest(key)? {
                manifests.push(manifest);
            }
        }
        Ok(manifests)
    }

    pub fn cleanup_at(
        &self,
        now_millis: u128,
        protected_keys: &HashSet<String>,
    ) -> io::Result<ThumbnailCleanupReport> {
        self.initialize()?;
        let mut report = ThumbnailCleanupReport::default();
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            let path = entry.path();
            if entry.file_type()?.is_file()
                && path
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().contains(".part."))
            {
                fs::remove_file(path)?;
                report.removed_parts += 1;
            }
        }

        let manifest_count_before = fs::read_dir(&self.root)?
            .filter_map(Result::ok)
            .filter(|entry| {
                let path = entry.path();
                path.extension().and_then(|value| value.to_str()) == Some("json")
                    && !path
                        .file_name()
                        .is_some_and(|name| name.to_string_lossy().contains(".part."))
            })
            .count();
        let mut manifests = self.manifests()?;
        report.removed_entries += manifest_count_before.saturating_sub(manifests.len());

        let known_images = manifests
            .iter()
            .map(|manifest| self.paths(&manifest.key).image)
            .collect::<HashSet<_>>();
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            let path = entry.path();
            if entry.file_type()?.is_file()
                && path.extension().and_then(|value| value.to_str()) == Some("jpg")
                && !known_images.contains(&path)
            {
                fs::remove_file(path)?;
                report.removed_entries += 1;
            }
        }

        let retention_millis = self.retention.as_millis();
        for manifest in manifests.clone() {
            if protected_keys.contains(&manifest.key) {
                continue;
            }
            if now_millis.saturating_sub(manifest.last_accessed_at_millis) > retention_millis {
                self.remove_entry(&manifest)?;
                report.removed_entries += 1;
            }
        }
        manifests = self.manifests()?;
        manifests.sort_by_key(|manifest| manifest.last_accessed_at_millis);
        let mut total = manifests
            .iter()
            .map(|manifest| self.entry_size(manifest))
            .sum::<u64>();
        for manifest in manifests {
            if total <= self.max_bytes {
                break;
            }
            if protected_keys.contains(&manifest.key) {
                continue;
            }
            let size = self.entry_size(&manifest);
            self.remove_entry(&manifest)?;
            total = total.saturating_sub(size);
            report.removed_entries += 1;
        }
        report.remaining_bytes = total;
        Ok(report)
    }

    pub fn evict_video(&self, video_id: i64) -> io::Result<()> {
        for manifest in self.manifests()? {
            if manifest.video_id == video_id {
                self.remove_entry(&manifest)?;
            }
        }
        Ok(())
    }

    pub fn validate_ready_media(&self, video_id: i64, media_path: &Path) -> io::Result<PathBuf> {
        if !is_regular_file_without_symlink(media_path) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "缩略图资源无效",
            ));
        }
        let canonical_root = fs::canonicalize(&self.root)?;
        let canonical_media = fs::canonicalize(media_path)?;
        if canonical_media.parent() != Some(canonical_root.as_path()) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "缩略图资源不属于缓存目录",
            ));
        }
        let key = canonical_media
            .file_stem()
            .and_then(|value| value.to_str())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "缩略图文件名无效"))?;
        let manifest = self
            .read_manifest(key)?
            .filter(|manifest| manifest.video_id == video_id)
            .ok_or_else(|| io::Error::new(io::ErrorKind::PermissionDenied, "缩略图归属无效"))?;
        if fs::canonicalize(self.paths(&manifest.key).image)
            .ok()
            .as_deref()
            != Some(canonical_media.as_path())
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "缩略图资源路径无效",
            ));
        }
        Ok(canonical_media)
    }

    fn write_manifest(&self, manifest: &ThumbnailCacheManifest) -> io::Result<()> {
        self.validate_key(&manifest.key)?;
        if manifest.image_file != format!("{}.jpg", manifest.key) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "缩略图缓存清单路径无效",
            ));
        }
        let paths = self.paths(&manifest.key);
        let bytes = serde_json::to_vec_pretty(manifest).map_err(io::Error::other)?;
        fs::write(&paths.temporary_manifest, bytes)?;
        replace_file(&paths.temporary_manifest, &paths.manifest)
    }

    fn entry_size(&self, manifest: &ThumbnailCacheManifest) -> u64 {
        let paths = self.paths(&manifest.key);
        fs::metadata(paths.image)
            .map(|value| value.len())
            .unwrap_or_default()
            + fs::metadata(paths.manifest)
                .map(|value| value.len())
                .unwrap_or_default()
    }

    fn remove_entry(&self, manifest: &ThumbnailCacheManifest) -> io::Result<()> {
        self.remove_key_files(&manifest.key)
    }

    fn remove_key_files(&self, key: &str) -> io::Result<()> {
        self.validate_key(key)?;
        let paths = self.paths(key);
        remove_if_exists(&paths.image)?;
        remove_if_exists(&paths.manifest)?;
        remove_if_exists(&paths.temporary_image)?;
        remove_if_exists(&paths.temporary_manifest)
    }
}

pub fn thumbnail_cache_key(identity: &ThumbnailSourceIdentity) -> String {
    let mut hasher = Sha256::new();
    hasher.update(identity.video_id.to_le_bytes());
    hasher.update([0]);
    hasher.update(identity.canonical_path.as_bytes());
    hasher.update([0]);
    hasher.update(identity.size_bytes.to_le_bytes());
    hasher.update(identity.modified_millis.to_le_bytes());
    hasher.update([match identity.source_kind {
        ThumbnailSourceKind::Original => 0,
        ThumbnailSourceKind::PreviewCache => 1,
    }]);
    hasher.update(identity.profile_version.to_le_bytes());
    hex::encode(hasher.finalize())
}

pub fn build_thumbnail_args(input: &Path, output: &Path, seek_to_one_second: bool) -> Vec<String> {
    let mut args = vec![
        "-hide_banner".to_owned(),
        "-loglevel".to_owned(),
        "error".to_owned(),
        "-nostdin".to_owned(),
        "-y".to_owned(),
    ];
    if seek_to_one_second {
        args.extend(["-ss".to_owned(), "1".to_owned()]);
    }
    args.extend([
        "-i".to_owned(),
        input.to_string_lossy().into_owned(),
        "-map".to_owned(),
        "0:v:0".to_owned(),
        "-frames:v".to_owned(),
        "1".to_owned(),
        "-vf".to_owned(),
        "scale='min(480,iw)':'min(480,ih)':force_original_aspect_ratio=decrease".to_owned(),
        "-q:v".to_owned(),
        "3".to_owned(),
        "-an".to_owned(),
        "-sn".to_owned(),
        "-dn".to_owned(),
        output.to_string_lossy().into_owned(),
    ]);
    args
}

pub fn validate_thumbnail_jpeg(path: &Path, max_edge: u32) -> io::Result<(u32, u32)> {
    if !is_regular_file_without_symlink(path) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "缩略图不是普通 JPEG 文件",
        ));
    }
    let metadata = fs::metadata(path)?;
    if metadata.len() == 0 || metadata.len() > 16 * 1024 * 1024 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "缩略图文件大小无效",
        ));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    fs::File::open(path)?.read_to_end(&mut bytes)?;
    let (width, height) = jpeg_dimensions(&bytes)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "缩略图不是有效 JPEG"))?;
    if width == 0 || height == 0 || width.max(height) > max_edge {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "缩略图尺寸超出限制",
        ));
    }
    Ok((width, height))
}

fn jpeg_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 4 || bytes[..2] != [0xff, 0xd8] || bytes[bytes.len() - 2..] != [0xff, 0xd9] {
        return None;
    }
    let mut index = 2;
    while index + 3 < bytes.len() {
        while index < bytes.len() && bytes[index] == 0xff {
            index += 1;
        }
        if index >= bytes.len() {
            return None;
        }
        let marker = bytes[index];
        index += 1;
        if marker == 0xd9 || marker == 0xda {
            break;
        }
        if matches!(marker, 0x01 | 0xd0..=0xd8) {
            continue;
        }
        if index + 2 > bytes.len() {
            return None;
        }
        let length = u16::from_be_bytes([bytes[index], bytes[index + 1]]) as usize;
        if length < 2 || index + length > bytes.len() {
            return None;
        }
        if matches!(marker, 0xc0..=0xc3 | 0xc5..=0xc7 | 0xc9..=0xcb | 0xcd..=0xcf) {
            if length < 7 {
                return None;
            }
            let height = u16::from_be_bytes([bytes[index + 3], bytes[index + 4]]) as u32;
            let width = u16::from_be_bytes([bytes[index + 5], bytes[index + 6]]) as u32;
            return Some((width, height));
        }
        index += length;
    }
    None
}

fn is_safe_cache_key(key: &str) -> bool {
    key.len() == 64 && key.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn is_regular_file_without_symlink(path: &Path) -> bool {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return false;
    };
    metadata.is_file() && !metadata.file_type().is_symlink()
}

fn remove_if_exists(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn replace_file(source: &Path, destination: &Path) -> io::Result<()> {
    remove_if_exists(destination)?;
    fs::rename(source, destination)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThumbnailRequest {
    pub video_id: i64,
    pub source_path: Option<String>,
    pub source_kind: Option<ThumbnailSourceKind>,
    pub ffmpeg_path: String,
    pub ffprobe_path: String,
    pub unavailable_code: Option<String>,
    pub unavailable_message: Option<String>,
}

impl ThumbnailRequest {
    pub fn available(
        video_id: i64,
        source_path: impl Into<String>,
        source_kind: ThumbnailSourceKind,
        ffmpeg_path: impl Into<String>,
        ffprobe_path: impl Into<String>,
    ) -> Self {
        Self {
            video_id,
            source_path: Some(source_path.into()),
            source_kind: Some(source_kind),
            ffmpeg_path: ffmpeg_path.into(),
            ffprobe_path: ffprobe_path.into(),
            unavailable_code: None,
            unavailable_message: None,
        }
    }

    pub fn unavailable(video_id: i64, code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            video_id,
            source_path: None,
            source_kind: None,
            ffmpeg_path: String::new(),
            ffprobe_path: String::new(),
            unavailable_code: Some(code.into()),
            unavailable_message: Some(message.into()),
        }
    }
}

pub fn trusted_thumbnail_request(
    video: &Video,
    preview_media: Option<&Path>,
    ffmpeg_path: &str,
    ffprobe_path: &str,
) -> ThumbnailRequest {
    if !matches!(video.status.as_str(), "complete" | "missing") {
        return ThumbnailRequest::unavailable(
            video.id,
            "video_not_complete",
            "只能为已经完成的视频分片生成封面",
        );
    }
    let original = Path::new(&video.path);
    match fs::symlink_metadata(original) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            return ThumbnailRequest::unavailable(
                video.id,
                "source_not_regular",
                "视频文件不是可读取的普通文件",
            );
        }
        Ok(_) => {
            let extension = original
                .extension()
                .and_then(|value| value.to_str())
                .unwrap_or_default()
                .to_ascii_lowercase();
            if !matches!(extension.as_str(), "mkv" | "mp4")
                || original
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().contains(".partial"))
            {
                return ThumbnailRequest::unavailable(
                    video.id,
                    "unsupported_source",
                    "当前视频文件不能生成封面",
                );
            }
            return ThumbnailRequest::available(
                video.id,
                original.to_string_lossy(),
                ThumbnailSourceKind::Original,
                ffmpeg_path,
                ffprobe_path,
            );
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(_) => {
            return ThumbnailRequest::unavailable(
                video.id,
                "source_unreadable",
                "无法读取视频文件",
            );
        }
    }

    let Some(preview_media) = preview_media else {
        return ThumbnailRequest::unavailable(
            video.id,
            "source_missing",
            "视频原文件和预览缓存均不可用",
        );
    };
    let preview_valid = preview_media
        .extension()
        .and_then(|value| value.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("mp4"))
        && is_regular_file_without_symlink(preview_media);
    if !preview_valid {
        return ThumbnailRequest::unavailable(
            video.id,
            "preview_cache_invalid",
            "已有视频预览缓存不可用",
        );
    }
    ThumbnailRequest::available(
        video.id,
        preview_media.to_string_lossy(),
        ThumbnailSourceKind::PreviewCache,
        ffmpeg_path,
        ffprobe_path,
    )
}

#[derive(Debug, Clone)]
pub struct ThumbnailExecution {
    pub request: ThumbnailRequest,
    pub identity: ThumbnailSourceIdentity,
    pub temporary_output: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThumbnailImage {
    pub width: u32,
    pub height: u32,
}

#[async_trait]
pub trait ThumbnailExecutor: Send + Sync {
    async fn extract(
        &self,
        execution: ThumbnailExecution,
        cancellation: CancellationToken,
    ) -> Result<ThumbnailImage, ThumbnailFailure>;
}

pub trait ThumbnailPublisher: Send + Sync {
    fn publish(&self, event: &ThumbnailEvent);
}

#[derive(Default)]
pub struct NoopThumbnailPublisher;

impl ThumbnailPublisher for NoopThumbnailPublisher {
    fn publish(&self, _event: &ThumbnailEvent) {}
}

#[derive(Debug, Default)]
pub struct FfmpegThumbnailExecutor;

#[async_trait]
impl ThumbnailExecutor for FfmpegThumbnailExecutor {
    async fn extract(
        &self,
        execution: ThumbnailExecution,
        cancellation: CancellationToken,
    ) -> Result<ThumbnailImage, ThumbnailFailure> {
        if cancellation.is_cancelled() {
            return Err(cancelled_failure());
        }
        match probe_has_video(&execution.request, cancellation.child_token()).await? {
            true => {}
            false => {
                return Err(ThumbnailFailure::new(
                    "no_video_track",
                    "视频文件没有可用于封面的画面轨道",
                ));
            }
        }

        let source = Path::new(&execution.identity.canonical_path);
        let mut final_failure =
            ThumbnailFailure::new("thumbnail_extract_failed", "无法提取视频封面");
        for seek_to_one_second in [false, true] {
            if cancellation.is_cancelled() {
                let _ = remove_if_exists(&execution.temporary_output);
                return Err(cancelled_failure());
            }
            let _ = remove_if_exists(&execution.temporary_output);
            let args =
                build_thumbnail_args(source, &execution.temporary_output, seek_to_one_second);
            match run_status(
                &execution.request.ffmpeg_path,
                &args,
                cancellation.child_token(),
            )
            .await
            {
                Ok(true) => {
                    match validate_thumbnail_jpeg(&execution.temporary_output, THUMBNAIL_MAX_EDGE) {
                        Ok((width, height)) => return Ok(ThumbnailImage { width, height }),
                        Err(_) => {
                            final_failure = ThumbnailFailure::new(
                                "thumbnail_invalid_output",
                                "FFmpeg 未生成有效的视频封面",
                            );
                        }
                    }
                }
                Ok(false) => {
                    final_failure =
                        ThumbnailFailure::new("thumbnail_extract_failed", "无法提取视频封面");
                }
                Err(failure) if failure.code == "cancelled" => {
                    let _ = remove_if_exists(&execution.temporary_output);
                    return Err(failure);
                }
                Err(failure) => final_failure = failure,
            }
        }
        let _ = remove_if_exists(&execution.temporary_output);
        Err(final_failure)
    }
}

async fn probe_has_video(
    request: &ThumbnailRequest,
    cancellation: CancellationToken,
) -> Result<bool, ThumbnailFailure> {
    let source = request
        .source_path
        .as_deref()
        .ok_or_else(|| ThumbnailFailure::new("source_unavailable", "视频文件当前不可用"))?;
    let args = vec![
        "-v".to_owned(),
        "error".to_owned(),
        "-select_streams".to_owned(),
        "v:0".to_owned(),
        "-show_entries".to_owned(),
        "stream=index".to_owned(),
        "-of".to_owned(),
        "csv=p=0".to_owned(),
        source.to_owned(),
    ];
    let (success, stdout) = run_with_stdout(&request.ffprobe_path, &args, cancellation).await?;
    if !success {
        return Err(ThumbnailFailure::new(
            "thumbnail_probe_failed",
            "无法检查视频画面轨道",
        ));
    }
    Ok(!String::from_utf8_lossy(&stdout).trim().is_empty())
}

async fn run_status(
    executable: &str,
    args: &[String],
    cancellation: CancellationToken,
) -> Result<bool, ThumbnailFailure> {
    let mut child = Command::new(executable)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| ThumbnailFailure::new("ffmpeg_unavailable", "无法启动 FFmpeg 生成视频封面"))?;
    tokio::select! {
        result = child.wait() => result
            .map(|status| status.success())
            .map_err(|_| ThumbnailFailure::new("thumbnail_extract_failed", "视频封面处理异常终止")),
        _ = cancellation.cancelled() => {
            let _ = child.start_kill();
            let _ = child.wait().await;
            Err(cancelled_failure())
        }
    }
}

async fn run_with_stdout(
    executable: &str,
    args: &[String],
    cancellation: CancellationToken,
) -> Result<(bool, Vec<u8>), ThumbnailFailure> {
    let mut child = Command::new(executable)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| {
            ThumbnailFailure::new("ffprobe_unavailable", "无法启动 FFprobe 检查视频封面")
        })?;
    let mut stdout = child.stdout.take().ok_or_else(|| {
        ThumbnailFailure::new("thumbnail_probe_failed", "无法读取视频轨道检查结果")
    })?;
    let reader = tokio::spawn(async move {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).await.map(|_| bytes)
    });
    let success = tokio::select! {
        result = child.wait() => result
            .map(|status| status.success())
            .map_err(|_| ThumbnailFailure::new("thumbnail_probe_failed", "视频轨道检查异常终止"))?,
        _ = cancellation.cancelled() => {
            let _ = child.start_kill();
            let _ = child.wait().await;
            reader.abort();
            return Err(cancelled_failure());
        }
    };
    let bytes = reader
        .await
        .map_err(|_| ThumbnailFailure::new("thumbnail_probe_failed", "无法读取视频轨道检查结果"))?
        .map_err(|_| ThumbnailFailure::new("thumbnail_probe_failed", "无法读取视频轨道检查结果"))?;
    Ok((success, bytes))
}

fn cancelled_failure() -> ThumbnailFailure {
    ThumbnailFailure::new("cancelled", "视频封面任务已取消")
}

pub fn thumbnail_source_identity(
    request: &ThumbnailRequest,
) -> Result<ThumbnailSourceIdentity, ThumbnailFailure> {
    let source = request
        .source_path
        .as_deref()
        .ok_or_else(|| unavailable_failure(request))?;
    let source_kind = request
        .source_kind
        .ok_or_else(|| unavailable_failure(request))?;
    let source = PathBuf::from(source);
    let metadata = fs::symlink_metadata(&source)
        .map_err(|_| ThumbnailFailure::new("source_missing", "视频文件已被移动或删除"))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(ThumbnailFailure::new(
            "source_not_regular",
            "视频文件不是可读取的普通文件",
        ));
    }
    if source
        .file_name()
        .is_some_and(|name| name.to_string_lossy().contains(".partial"))
    {
        return Err(ThumbnailFailure::new(
            "video_not_complete",
            "正在写入的视频不能生成封面",
        ));
    }
    let extension = source
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let extension_valid = match source_kind {
        ThumbnailSourceKind::Original => matches!(extension.as_str(), "mkv" | "mp4"),
        ThumbnailSourceKind::PreviewCache => extension == "mp4",
    };
    if !extension_valid {
        return Err(ThumbnailFailure::new(
            "unsupported_format",
            "当前只支持从 MKV 或 MP4 视频生成封面",
        ));
    }
    let canonical = fs::canonicalize(&source)
        .map_err(|_| ThumbnailFailure::new("source_missing", "视频文件已被移动或删除"))?;
    let metadata = fs::metadata(&canonical)
        .map_err(|_| ThumbnailFailure::new("source_missing", "视频文件已被移动或删除"))?;
    let modified_millis = metadata
        .modified()
        .ok()
        .and_then(|value| value.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|value| value.as_millis())
        .unwrap_or_default();
    Ok(ThumbnailSourceIdentity {
        video_id: request.video_id,
        canonical_path: canonical.to_string_lossy().into_owned(),
        size_bytes: metadata.len(),
        modified_millis,
        source_kind,
        profile_version: THUMBNAIL_PROFILE_VERSION,
    })
}

fn unavailable_failure(request: &ThumbnailRequest) -> ThumbnailFailure {
    ThumbnailFailure::new(
        request
            .unavailable_code
            .as_deref()
            .unwrap_or("source_unavailable"),
        request
            .unavailable_message
            .as_deref()
            .unwrap_or("视频文件当前不可用"),
    )
}

#[derive(Clone)]
pub struct ThumbnailService {
    inner: Arc<ThumbnailServiceInner>,
}

struct ThumbnailServiceInner {
    cache: ThumbnailCache,
    executor: Arc<dyn ThumbnailExecutor>,
    publisher: Arc<dyn ThumbnailPublisher>,
    batches: Mutex<HashMap<String, ThumbnailBatchRecord>>,
    jobs: Mutex<HashMap<String, ThumbnailJobRecord>>,
    failed_keys: Mutex<HashMap<String, ThumbnailFailure>>,
    evicted_video_ids: Mutex<HashSet<i64>>,
    publish_lock: Mutex<()>,
    semaphore: Arc<Semaphore>,
    shutdown: CancellationToken,
    shutdown_notify: Notify,
    next_batch_id: AtomicU64,
}

struct ThumbnailBatchRecord {
    order: Vec<i64>,
    items: HashMap<i64, ThumbnailSnapshot>,
    requests: HashMap<i64, ThumbnailRequest>,
    keys: HashMap<i64, String>,
}

#[derive(Clone)]
struct ThumbnailJobRecord {
    video_id: i64,
    cancellation: CancellationToken,
    started: Arc<AtomicBool>,
}

struct PendingJob {
    request: ThumbnailRequest,
    identity: ThumbnailSourceIdentity,
    key: String,
    cancellation: CancellationToken,
    started: Arc<AtomicBool>,
}

enum ThumbnailJobOutcome {
    Ready(ThumbnailCacheManifest),
    Failed(ThumbnailFailure),
    Unavailable(ThumbnailFailure),
    Discarded,
}

impl ThumbnailService {
    pub fn new(cache: ThumbnailCache) -> Result<Self, ThumbnailFailure> {
        Self::with_executor_and_publisher(
            cache,
            Arc::new(FfmpegThumbnailExecutor),
            Arc::new(NoopThumbnailPublisher),
        )
    }

    pub fn with_executor(
        cache: ThumbnailCache,
        executor: Arc<dyn ThumbnailExecutor>,
    ) -> Result<Self, ThumbnailFailure> {
        Self::with_executor_and_publisher(cache, executor, Arc::new(NoopThumbnailPublisher))
    }

    pub fn with_executor_and_publisher(
        cache: ThumbnailCache,
        executor: Arc<dyn ThumbnailExecutor>,
        publisher: Arc<dyn ThumbnailPublisher>,
    ) -> Result<Self, ThumbnailFailure> {
        cache
            .initialize()
            .map_err(|_| ThumbnailFailure::new("cache_io", "无法创建视频封面缓存目录"))?;
        cache
            .cleanup_at(now_millis(), &HashSet::new())
            .map_err(|_| ThumbnailFailure::new("cache_io", "无法清理视频封面缓存"))?;
        Ok(Self {
            inner: Arc::new(ThumbnailServiceInner {
                cache,
                executor,
                publisher,
                batches: Mutex::new(HashMap::new()),
                jobs: Mutex::new(HashMap::new()),
                failed_keys: Mutex::new(HashMap::new()),
                evicted_video_ids: Mutex::new(HashSet::new()),
                publish_lock: Mutex::new(()),
                semaphore: Arc::new(Semaphore::new(THUMBNAIL_CONCURRENCY)),
                shutdown: CancellationToken::new(),
                shutdown_notify: Notify::new(),
                next_batch_id: AtomicU64::new(1),
            }),
        })
    }

    pub fn cache_root(&self) -> &Path {
        self.inner.cache.root()
    }

    pub async fn request_batch(
        &self,
        requests: Vec<ThumbnailRequest>,
    ) -> Result<ThumbnailBatch, ThumbnailFailure> {
        if self.inner.shutdown.is_cancelled() {
            return Err(ThumbnailFailure::new(
                "service_stopped",
                "视频封面服务已停止",
            ));
        }
        if requests.len() > THUMBNAIL_MAX_BATCH_SIZE {
            return Err(ThumbnailFailure::new(
                "batch_too_large",
                "单次最多请求 50 个视频封面",
            ));
        }
        self.cleanup_cache()?;
        let batch_id = format!(
            "thumbnail-{}-{}",
            now_millis(),
            self.inner.next_batch_id.fetch_add(1, Ordering::Relaxed)
        );
        let mut order = Vec::with_capacity(requests.len());
        let mut items = HashMap::with_capacity(requests.len());
        let mut request_map = HashMap::with_capacity(requests.len());
        let mut keys = HashMap::with_capacity(requests.len());
        let mut pending = Vec::new();
        let mut seen = HashSet::with_capacity(requests.len());

        for request in requests {
            if request.video_id <= 0 || !seen.insert(request.video_id) {
                return Err(ThumbnailFailure::new(
                    "invalid_video_ids",
                    "视频封面请求包含无效或重复的视频 ID",
                ));
            }
            let video_id = request.video_id;
            order.push(video_id);
            request_map.insert(video_id, request.clone());
            let snapshot = match thumbnail_source_identity(&request) {
                Ok(identity) => {
                    let key = thumbnail_cache_key(&identity);
                    keys.insert(video_id, key.clone());
                    if self
                        .inner
                        .evicted_video_ids
                        .lock()
                        .expect("缩略图驱逐锁已损坏")
                        .contains(&video_id)
                    {
                        unavailable_snapshot(
                            &batch_id,
                            video_id,
                            Some(key),
                            ThumbnailFailure::new("video_evicted", "视频正在删除，封面不可用"),
                        )
                    } else if let Some(manifest) = self
                        .inner
                        .cache
                        .lookup(&identity, now_millis())
                        .map_err(|_| ThumbnailFailure::new("cache_io", "无法读取视频封面缓存"))?
                    {
                        ready_snapshot(&batch_id, &manifest, true, &self.inner.cache)
                    } else if let Some(failure) = self
                        .inner
                        .failed_keys
                        .lock()
                        .expect("缩略图失败状态锁已损坏")
                        .get(&key)
                        .cloned()
                    {
                        failed_snapshot(&batch_id, video_id, key, failure)
                    } else {
                        let snapshot = queued_snapshot(&batch_id, video_id, key.clone());
                        pending.push((request.clone(), identity, key));
                        snapshot
                    }
                }
                Err(failure) => unavailable_snapshot(&batch_id, video_id, None, failure),
            };
            items.insert(video_id, snapshot);
        }

        self.inner
            .batches
            .lock()
            .expect("缩略图批次锁已损坏")
            .insert(
                batch_id.clone(),
                ThumbnailBatchRecord {
                    order,
                    items,
                    requests: request_map,
                    keys,
                },
            );
        for (request, identity, key) in pending {
            if let Some(job) = self.create_job_if_absent(request, identity, key) {
                self.spawn_job(job);
            }
        }
        self.get_batch(&batch_id)
    }

    pub fn get_batch(&self, batch_id: &str) -> Result<ThumbnailBatch, ThumbnailFailure> {
        let batches = self.inner.batches.lock().expect("缩略图批次锁已损坏");
        let record = batches
            .get(batch_id)
            .ok_or_else(|| ThumbnailFailure::new("batch_not_found", "找不到视频封面批次"))?;
        Ok(ThumbnailBatch {
            batch_id: batch_id.to_owned(),
            items: record
                .order
                .iter()
                .filter_map(|video_id| record.items.get(video_id).cloned())
                .collect(),
        })
    }

    pub async fn retry(
        &self,
        batch_id: &str,
        request: ThumbnailRequest,
    ) -> Result<ThumbnailSnapshot, ThumbnailFailure> {
        if self.inner.shutdown.is_cancelled() {
            return Err(ThumbnailFailure::new(
                "service_stopped",
                "视频封面服务已停止",
            ));
        }
        {
            let batches = self.inner.batches.lock().expect("缩略图批次锁已损坏");
            let record = batches
                .get(batch_id)
                .ok_or_else(|| ThumbnailFailure::new("batch_not_found", "找不到视频封面批次"))?;
            if !record.requests.contains_key(&request.video_id) {
                return Err(ThumbnailFailure::new(
                    "video_not_in_batch",
                    "该视频不属于当前封面批次",
                ));
            }
        }
        let video_id = request.video_id;
        let identity = match thumbnail_source_identity(&request) {
            Ok(identity) => identity,
            Err(failure) => {
                let snapshot = unavailable_snapshot(batch_id, video_id, None, failure);
                self.replace_batch_item(batch_id, request, None, snapshot.clone())?;
                return Ok(snapshot);
            }
        };
        let key = thumbnail_cache_key(&identity);
        self.inner
            .failed_keys
            .lock()
            .expect("缩略图失败状态锁已损坏")
            .remove(&key);
        remove_if_exists(&self.inner.cache.paths(&key).temporary_image)
            .map_err(|_| ThumbnailFailure::new("cache_io", "无法清理失败的封面临时文件"))?;
        let snapshot = if let Some(manifest) = self
            .inner
            .cache
            .lookup(&identity, now_millis())
            .map_err(|_| ThumbnailFailure::new("cache_io", "无法读取视频封面缓存"))?
        {
            ready_snapshot(batch_id, &manifest, true, &self.inner.cache)
        } else {
            let snapshot = queued_snapshot(batch_id, video_id, key.clone());
            if let Some(job) = self.create_job_if_absent(request.clone(), identity, key.clone()) {
                self.spawn_job(job);
            }
            snapshot
        };
        self.replace_batch_item(batch_id, request, Some(key), snapshot.clone())?;
        Ok(snapshot)
    }

    pub fn release_batch(&self, batch_id: &str) {
        let removed = self
            .inner
            .batches
            .lock()
            .expect("缩略图批次锁已损坏")
            .remove(batch_id);
        let Some(record) = removed else {
            return;
        };
        let unique_keys = record.keys.into_values().collect::<HashSet<_>>();
        for key in unique_keys {
            if self.key_has_batch_interest(&key) {
                continue;
            }
            if let Some(job) = self
                .inner
                .jobs
                .lock()
                .expect("缩略图任务锁已损坏")
                .get(&key)
                .cloned()
                && !job.started.load(Ordering::SeqCst)
            {
                job.cancellation.cancel();
            }
        }
    }

    pub fn begin_eviction(&self, video_ids: &[i64]) {
        let video_ids = video_ids.iter().copied().collect::<HashSet<_>>();
        self.inner
            .evicted_video_ids
            .lock()
            .expect("缩略图驱逐锁已损坏")
            .extend(video_ids.iter().copied());
        for job in self.inner.jobs.lock().expect("缩略图任务锁已损坏").values() {
            if video_ids.contains(&job.video_id) {
                job.cancellation.cancel();
            }
        }
    }

    pub fn rollback_eviction(&self, video_ids: &[i64]) {
        let mut evicted = self
            .inner
            .evicted_video_ids
            .lock()
            .expect("缩略图驱逐锁已损坏");
        for video_id in video_ids {
            evicted.remove(video_id);
        }
    }

    pub fn commit_eviction(&self, video_ids: &[i64]) -> Result<(), ThumbnailFailure> {
        let ids = video_ids.iter().copied().collect::<HashSet<_>>();
        let _publish_guard = self.inner.publish_lock.lock().expect("缩略图发布锁已损坏");
        for video_id in &ids {
            self.inner
                .cache
                .evict_video(*video_id)
                .map_err(|_| ThumbnailFailure::new("cache_io", "无法清理视频封面缓存"))?;
        }
        let mut batches = self.inner.batches.lock().expect("缩略图批次锁已损坏");
        for record in batches.values_mut() {
            record.order.retain(|video_id| !ids.contains(video_id));
            record.items.retain(|video_id, _| !ids.contains(video_id));
            record
                .requests
                .retain(|video_id, _| !ids.contains(video_id));
            record.keys.retain(|video_id, _| !ids.contains(video_id));
        }
        Ok(())
    }

    pub fn authorize_media(
        &self,
        video_id: i64,
        media_path: &Path,
    ) -> Result<PathBuf, ThumbnailFailure> {
        self.inner
            .cache
            .validate_ready_media(video_id, media_path)
            .map_err(|_| ThumbnailFailure::new("thumbnail_asset_denied", "视频封面资源无效"))
    }

    pub async fn shutdown(&self) {
        self.inner.shutdown.cancel();
        for job in self.inner.jobs.lock().expect("缩略图任务锁已损坏").values() {
            job.cancellation.cancel();
        }
        loop {
            let notified = self.inner.shutdown_notify.notified();
            if self
                .inner
                .jobs
                .lock()
                .expect("缩略图任务锁已损坏")
                .is_empty()
            {
                break;
            }
            notified.await;
        }
    }

    pub fn active_job_count(&self) -> usize {
        self.inner.jobs.lock().expect("缩略图任务锁已损坏").len()
    }

    fn cleanup_cache(&self) -> Result<(), ThumbnailFailure> {
        self.inner
            .cache
            .cleanup_at(now_millis(), &self.protected_keys())
            .map(|_| ())
            .map_err(|_| ThumbnailFailure::new("cache_io", "无法清理视频封面缓存"))
    }

    fn protected_keys(&self) -> HashSet<String> {
        let mut keys = self
            .inner
            .batches
            .lock()
            .expect("缩略图批次锁已损坏")
            .values()
            .flat_map(|record| record.keys.values().cloned())
            .collect::<HashSet<_>>();
        keys.extend(
            self.inner
                .jobs
                .lock()
                .expect("缩略图任务锁已损坏")
                .keys()
                .cloned(),
        );
        keys
    }

    fn create_job_if_absent(
        &self,
        request: ThumbnailRequest,
        identity: ThumbnailSourceIdentity,
        key: String,
    ) -> Option<PendingJob> {
        let mut jobs = self.inner.jobs.lock().expect("缩略图任务锁已损坏");
        if jobs.contains_key(&key) {
            return None;
        }
        let cancellation = self.inner.shutdown.child_token();
        let started = Arc::new(AtomicBool::new(false));
        jobs.insert(
            key.clone(),
            ThumbnailJobRecord {
                video_id: request.video_id,
                cancellation: cancellation.clone(),
                started: started.clone(),
            },
        );
        Some(PendingJob {
            request,
            identity,
            key,
            cancellation,
            started,
        })
    }

    fn spawn_job(&self, job: PendingJob) {
        let service = self.clone();
        tokio::spawn(async move {
            let key = job.key.clone();
            let outcome = service.run_job(job).await;
            service.finish_job(&key);
            service.apply_job_outcome(&key, outcome);
            let _ = service.cleanup_cache();
        });
    }

    async fn run_job(&self, job: PendingJob) -> ThumbnailJobOutcome {
        let permit = tokio::select! {
            permit = self.inner.semaphore.clone().acquire_owned() => match permit {
                Ok(permit) => permit,
                Err(_) => return ThumbnailJobOutcome::Discarded,
            },
            _ = job.cancellation.cancelled() => return ThumbnailJobOutcome::Discarded,
        };
        job.started.store(true, Ordering::SeqCst);
        if job.cancellation.is_cancelled() {
            drop(permit);
            return ThumbnailJobOutcome::Discarded;
        }
        let paths = self.inner.cache.paths(&job.key);
        let result = self
            .inner
            .executor
            .extract(
                ThumbnailExecution {
                    request: job.request.clone(),
                    identity: job.identity.clone(),
                    temporary_output: paths.temporary_image.clone(),
                },
                job.cancellation.child_token(),
            )
            .await;
        drop(permit);
        let image = match result {
            Ok(image) => image,
            Err(failure) if failure.code == "cancelled" => {
                let _ = remove_if_exists(&paths.temporary_image);
                return ThumbnailJobOutcome::Discarded;
            }
            Err(failure) if failure.code == "no_video_track" => {
                let _ = remove_if_exists(&paths.temporary_image);
                return ThumbnailJobOutcome::Unavailable(failure);
            }
            Err(failure) => {
                let _ = remove_if_exists(&paths.temporary_image);
                return ThumbnailJobOutcome::Failed(failure);
            }
        };
        let _publish_guard = self.inner.publish_lock.lock().expect("缩略图发布锁已损坏");
        if self
            .inner
            .evicted_video_ids
            .lock()
            .expect("缩略图驱逐锁已损坏")
            .contains(&job.request.video_id)
            || self.inner.shutdown.is_cancelled()
        {
            let _ = remove_if_exists(&paths.temporary_image);
            return ThumbnailJobOutcome::Discarded;
        }
        if thumbnail_source_identity(&job.request).ok().as_ref() != Some(&job.identity) {
            let _ = remove_if_exists(&paths.temporary_image);
            return ThumbnailJobOutcome::Failed(ThumbnailFailure::new(
                "source_changed",
                "视频文件在生成封面期间发生变化，请重试",
            ));
        }
        match self
            .inner
            .cache
            .publish(&job.identity, image.width, image.height, now_millis())
        {
            Ok(manifest) => ThumbnailJobOutcome::Ready(manifest),
            Err(_) => ThumbnailJobOutcome::Failed(ThumbnailFailure::new(
                "cache_io",
                "无法保存视频封面缓存",
            )),
        }
    }

    fn finish_job(&self, key: &str) {
        self.inner
            .jobs
            .lock()
            .expect("缩略图任务锁已损坏")
            .remove(key);
        self.inner.shutdown_notify.notify_waiters();
    }

    fn apply_job_outcome(&self, key: &str, outcome: ThumbnailJobOutcome) {
        let mut events = Vec::new();
        match &outcome {
            ThumbnailJobOutcome::Failed(failure) => {
                self.inner
                    .failed_keys
                    .lock()
                    .expect("缩略图失败状态锁已损坏")
                    .insert(key.to_owned(), failure.clone());
            }
            ThumbnailJobOutcome::Ready(_) => {
                self.inner
                    .failed_keys
                    .lock()
                    .expect("缩略图失败状态锁已损坏")
                    .remove(key);
            }
            ThumbnailJobOutcome::Unavailable(_) | ThumbnailJobOutcome::Discarded => {}
        }
        if matches!(outcome, ThumbnailJobOutcome::Discarded) {
            return;
        }
        let mut batches = self.inner.batches.lock().expect("缩略图批次锁已损坏");
        for (batch_id, record) in batches.iter_mut() {
            let affected = record
                .keys
                .iter()
                .filter(|(_, item_key)| item_key.as_str() == key)
                .map(|(video_id, _)| *video_id)
                .collect::<Vec<_>>();
            for video_id in affected {
                let snapshot = match &outcome {
                    ThumbnailJobOutcome::Ready(manifest) => {
                        ready_snapshot(batch_id, manifest, false, &self.inner.cache)
                    }
                    ThumbnailJobOutcome::Failed(failure) => {
                        failed_snapshot(batch_id, video_id, key.to_owned(), failure.clone())
                    }
                    ThumbnailJobOutcome::Unavailable(failure) => unavailable_snapshot(
                        batch_id,
                        video_id,
                        Some(key.to_owned()),
                        failure.clone(),
                    ),
                    ThumbnailJobOutcome::Discarded => continue,
                };
                record.items.insert(video_id, snapshot.clone());
                events.push(ThumbnailEvent {
                    batch_id: batch_id.clone(),
                    item: snapshot,
                });
            }
        }
        drop(batches);
        for event in events {
            self.inner.publisher.publish(&event);
        }
    }

    fn replace_batch_item(
        &self,
        batch_id: &str,
        request: ThumbnailRequest,
        key: Option<String>,
        snapshot: ThumbnailSnapshot,
    ) -> Result<(), ThumbnailFailure> {
        let mut batches = self.inner.batches.lock().expect("缩略图批次锁已损坏");
        let record = batches
            .get_mut(batch_id)
            .ok_or_else(|| ThumbnailFailure::new("batch_not_found", "找不到视频封面批次"))?;
        record.requests.insert(request.video_id, request.clone());
        if let Some(key) = key {
            record.keys.insert(request.video_id, key);
        } else {
            record.keys.remove(&request.video_id);
        }
        record.items.insert(request.video_id, snapshot);
        Ok(())
    }

    fn key_has_batch_interest(&self, key: &str) -> bool {
        self.inner
            .batches
            .lock()
            .expect("缩略图批次锁已损坏")
            .values()
            .any(|record| record.keys.values().any(|item_key| item_key == key))
    }
}

fn queued_snapshot(batch_id: &str, video_id: i64, key: String) -> ThumbnailSnapshot {
    ThumbnailSnapshot {
        batch_id: batch_id.to_owned(),
        video_id,
        cache_key: Some(key),
        state: ThumbnailState::Queued,
        media: None,
        error_code: None,
        error_message: None,
    }
}

fn ready_snapshot(
    batch_id: &str,
    manifest: &ThumbnailCacheManifest,
    cache_hit: bool,
    cache: &ThumbnailCache,
) -> ThumbnailSnapshot {
    ThumbnailSnapshot {
        batch_id: batch_id.to_owned(),
        video_id: manifest.video_id,
        cache_key: Some(manifest.key.clone()),
        state: ThumbnailState::Ready,
        media: Some(ThumbnailMedia {
            path: cache
                .paths(&manifest.key)
                .image
                .to_string_lossy()
                .into_owned(),
            mime_type: "image/jpeg".to_owned(),
            width: manifest.width,
            height: manifest.height,
            cache_hit,
            source_kind: manifest.source.source_kind,
        }),
        error_code: None,
        error_message: None,
    }
}

fn failed_snapshot(
    batch_id: &str,
    video_id: i64,
    key: String,
    failure: ThumbnailFailure,
) -> ThumbnailSnapshot {
    ThumbnailSnapshot {
        batch_id: batch_id.to_owned(),
        video_id,
        cache_key: Some(key),
        state: ThumbnailState::Failed,
        media: None,
        error_code: Some(failure.code),
        error_message: Some(failure.message),
    }
}

fn unavailable_snapshot(
    batch_id: &str,
    video_id: i64,
    key: Option<String>,
    failure: ThumbnailFailure,
) -> ThumbnailSnapshot {
    ThumbnailSnapshot {
        batch_id: batch_id.to_owned(),
        video_id,
        cache_key: key,
        state: ThumbnailState::Unavailable,
        media: None,
        error_code: Some(failure.code),
        error_message: Some(failure.message),
    }
}

fn now_millis() -> u128 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default()
}
