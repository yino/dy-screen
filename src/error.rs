use std::fmt;
use std::path::PathBuf;

use thiserror::Error;

use crate::model::ProfileDiscoveryErrorKind;

pub type Result<T> = std::result::Result<T, RecorderError>;

#[derive(Error)]
pub enum RecorderError {
    #[error("invalid room URL: {reason}")]
    InvalidRoomUrl { reason: String },

    #[error("unsupported room URL host; expected live.douyin.com")]
    UnsupportedRoomUrl { url: String },

    #[error("个人主页 URL 无效：{reason}")]
    InvalidProfileUrl { reason: String },

    #[error("仅支持公开抖音个人主页 douyin.com/user/{{profile_sec_uid}}")]
    UnsupportedProfileUrl,

    #[error("访问公开抖音个人主页失败")]
    ProfilePageRequest {
        #[source]
        source: reqwest::Error,
    },

    #[error("公开抖音个人主页暂时无法访问（HTTP {status}）")]
    ProfileHttpStatus { status: u16 },

    #[error("公开抖音个人主页需要登录、验证码或额外访问权限")]
    ProfileAccessRestricted,

    #[error("访问公开抖音直播间失败")]
    PageRequest(#[from] reqwest::Error),

    #[error("抖音直播间暂时无法访问（HTTP {status}）")]
    RoomHttpStatus { status: u16 },

    #[error("抖音直播间需要登录、验证码或额外访问权限")]
    RoomAccessRestricted,

    #[error("抖音浏览器会话需要手动完成访问验证")]
    RoomAccessVerificationRequired,

    #[error("浏览器页面快照无效或超过安全限制")]
    InvalidBrowserSnapshot,

    #[error("抖音浏览器会话暂时不可用")]
    BrowserSessionUnavailable,

    #[error("浏览器页面回调已经失效")]
    BrowserRequestSuperseded,

    #[error("the live room is offline or has no usable stream")]
    RoomUnavailable,

    #[error("抖音直播间页面结构已变化，当前版本暂时无法解析")]
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

    pub fn profile_discovery_kind(&self) -> Option<ProfileDiscoveryErrorKind> {
        match self {
            Self::ProfilePageRequest { .. } | Self::ProfileHttpStatus { .. } => {
                Some(ProfileDiscoveryErrorKind::Retryable)
            }
            Self::ProfileAccessRestricted => Some(ProfileDiscoveryErrorKind::AccessRestricted),
            Self::UnsupportedPageLayout => Some(ProfileDiscoveryErrorKind::UnsupportedPageLayout),
            _ => None,
        }
    }
}

impl fmt::Debug for RecorderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("RecorderError")
            .field(&self.safe_message())
            .finish()
    }
}
