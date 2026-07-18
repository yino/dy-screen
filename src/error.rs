use std::path::PathBuf;

use thiserror::Error;

pub type Result<T> = std::result::Result<T, RecorderError>;

#[derive(Debug, Error)]
pub enum RecorderError {
    #[error("invalid room URL: {reason}")]
    InvalidRoomUrl { reason: String },

    #[error("unsupported room URL host; expected live.douyin.com")]
    UnsupportedRoomUrl { url: String },

    #[error("failed to fetch the Douyin room page: {0}")]
    PageRequest(#[from] reqwest::Error),

    #[error("the live room is offline or has no usable stream")]
    RoomUnavailable,

    #[error("the Douyin page layout is not supported by this Demo")]
    UnsupportedPageLayout,

    #[error("no stream variant is available")]
    NoStreamVariant,

    #[error("segment duration must be greater than zero")]
    InvalidSegmentDuration,

    #[error("FFmpeg is unavailable at {path}: {message}")]
    FfmpegUnavailable { path: PathBuf, message: String },

    #[error("failed to create recording output: {0}")]
    Io(#[from] std::io::Error),

    #[error("FFmpeg exited unsuccessfully with code {code:?}")]
    FfmpegFailed { code: Option<i32> },

    #[error("recording was cancelled")]
    Cancelled,
}

impl RecorderError {
    pub fn safe_message(&self) -> String {
        self.to_string()
    }
}
