//! 生成资产（参考图 / 转场视频）的**受控下载与完整性校验**。
//!
//! 上游返回的资源 URL **只有 24 小时有效期**，因此拿到后必须立即下载落盘。
//! 校验不通过一律**丢弃并删除半成品**——宁可这次不出转场，也不能把坏素材
//! 合成进成片：成片一旦导出，用户看到的是坏画面，而提示只在日志里。
//!
//! 校验口径取自上游实测与官方限制：
//! - 图像：单边 `[240, 8000]` 像素、`≤20MB`、不支持透明通道
//! - 视频：MP4 / 30fps、时长必须与请求一致（越界说明上游没按参数执行）

use std::path::{Path, PathBuf};

use async_trait::async_trait;

use super::wan_generation::GenerationTransport;

/// 探测到的媒体属性。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProbedMedia {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub duration_ms: u64,
}

/// 媒体属性探测抽象：真实实现调用 ffprobe，测试注入假实现（**测试不依赖 ffmpeg**）。
#[async_trait]
pub trait MediaProbe: Send + Sync {
    async fn probe(&self, path: &Path) -> Result<ProbedMedia, String>;
}

/// ffprobe 实现（与 `clip_export` 一样走受控的随应用 ffprobe）。
pub struct FfprobeMediaProbe {
    pub ffprobe_path: PathBuf,
}

#[async_trait]
impl MediaProbe for FfprobeMediaProbe {
    async fn probe(&self, path: &Path) -> Result<ProbedMedia, String> {
        let output = tokio::process::Command::new(&self.ffprobe_path)
            .args([
                "-v",
                "error",
                "-select_streams",
                "v:0",
                "-show_entries",
                "stream=width,height,r_frame_rate:format=duration",
                "-of",
                "json",
            ])
            .arg(path)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .output()
            .await
            .map_err(|error| error.to_string())?;
        if !output.status.success() {
            return Err("ffprobe 读取失败".to_owned());
        }
        parse_ffprobe_json(&String::from_utf8_lossy(&output.stdout))
    }
}

/// 解析 ffprobe 的 JSON 输出。抽成纯函数以便直接测试。
pub fn parse_ffprobe_json(raw: &str) -> Result<ProbedMedia, String> {
    let value: serde_json::Value =
        serde_json::from_str(raw).map_err(|_| "ffprobe 输出不是 JSON".to_owned())?;
    let stream = &value["streams"][0];
    let width = stream["width"].as_u64().unwrap_or(0) as u32;
    let height = stream["height"].as_u64().unwrap_or(0) as u32;
    // `r_frame_rate` 形如 "30/1"；缺失或不合法时按 0 处理，由调用方判定。
    let fps = stream["r_frame_rate"]
        .as_str()
        .and_then(parse_rational)
        .unwrap_or(0);
    let duration_ms = value["format"]["duration"]
        .as_str()
        .and_then(|raw| raw.parse::<f64>().ok())
        .map(|seconds| (seconds * 1000.0).round() as u64)
        .unwrap_or(0);
    if width == 0 || height == 0 {
        return Err("ffprobe 未给出画面尺寸".to_owned());
    }
    Ok(ProbedMedia {
        width,
        height,
        fps,
        duration_ms,
    })
}

fn parse_rational(raw: &str) -> Option<u32> {
    let (numerator, denominator) = raw.split_once('/')?;
    let numerator: f64 = numerator.parse().ok()?;
    let denominator: f64 = denominator.parse().ok()?;
    if denominator == 0.0 {
        return None;
    }
    Some((numerator / denominator).round() as u32)
}

/// 期望值：与请求发给上游的参数一致，用来校验上游**确实照办**了。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AssetExpectation {
    /// 图像与视频的区分（影响尺寸下限与时长校验）。
    pub is_video: bool,
    /// 允许的最大字节数（图像 20MB；视频给更宽的上限）。
    pub max_bytes: u64,
    /// 期望时长（毫秒）。仅视频有意义；允许 `tolerance_ms` 误差。
    pub expected_duration_ms: u64,
    pub tolerance_ms: u64,
    /// 画面单边下限（上游要求图像单边 ≥ 240）。
    pub min_side: u32,
}

impl AssetExpectation {
    pub fn image() -> Self {
        Self {
            is_video: false,
            max_bytes: 20 * 1024 * 1024,
            expected_duration_ms: 0,
            tolerance_ms: 0,
            min_side: 240,
        }
    }

    pub fn video(expected_duration_ms: u64) -> Self {
        Self {
            is_video: true,
            max_bytes: 200 * 1024 * 1024,
            expected_duration_ms,
            // 编码/封装会造成几十毫秒偏差，给 500ms 容差；再大就说明上游没照办。
            tolerance_ms: 500,
            min_side: 240,
        }
    }
}

/// 校验通过后的资产描述。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedAsset {
    pub path: PathBuf,
    pub sha256: String,
    pub bytes: u64,
    pub media: ProbedMedia,
}

/// 可区分的拒收原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssetRejection {
    /// 下载失败（含 24 小时过期后的 404/403）。
    Download(String),
    /// 响应体为空。
    EmptyBody,
    /// 超出体积上限。
    TooLarge { bytes: u64, limit: u64 },
    /// 探测失败（解不出媒体属性）。
    ProbeFailed(String),
    /// 画面太小，无法作为 480P 视频首帧。
    TooSmall { width: u32, height: u32, min: u32 },
    /// 时长与请求不符。
    DurationMismatch { actual_ms: u64, expected_ms: u64 },
    /// 视频轨道没有可用帧率。
    NoFrameRate,
}

impl AssetRejection {
    /// 稳定的错误码（脱敏、可直接入库与展示）。
    pub fn code(&self) -> &'static str {
        match self {
            Self::Download(_) => "generated_asset_download_failed",
            Self::EmptyBody => "generated_asset_empty",
            Self::TooLarge { .. } => "generated_asset_too_large",
            Self::ProbeFailed(_) => "generated_asset_probe_failed",
            Self::TooSmall { .. } => "generated_asset_too_small",
            Self::DurationMismatch { .. } => "generated_asset_duration_mismatch",
            Self::NoFrameRate => "generated_asset_no_framerate",
        }
    }
}

/// 下载 + 校验。
///
/// 任何一步失败都会**删除已写下的半成品**——磁盘上不留垃圾，调用方也拿不到
/// "看起来存在但其实没用"的文件。
pub async fn download_and_validate<T: GenerationTransport, P: MediaProbe>(
    transport: &T,
    probe: &P,
    url: &str,
    destination: &Path,
    expectation: AssetExpectation,
) -> Result<ValidatedAsset, AssetRejection> {
    let bytes = transport
        .download(url)
        .await
        .map_err(AssetRejection::Download)?;
    if bytes.is_empty() {
        return Err(AssetRejection::EmptyBody);
    }
    let bytes_len = bytes.len() as u64;
    if bytes_len > expectation.max_bytes {
        return Err(AssetRejection::TooLarge {
            bytes: bytes_len,
            limit: expectation.max_bytes,
        });
    }

    if let Some(parent) = destination.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|error| AssetRejection::Download(error.to_string()))?;
    }
    tokio::fs::write(destination, &bytes)
        .await
        .map_err(|error| AssetRejection::Download(error.to_string()))?;

    let sha256 = sha256_hex(&bytes);

    // 从这里开始的任何失败都要清掉刚写下的文件。
    let media = match probe.probe(destination).await {
        Ok(media) => media,
        Err(error) => {
            discard(destination).await;
            return Err(AssetRejection::ProbeFailed(error));
        }
    };

    if media.width.min(media.height) < expectation.min_side {
        discard(destination).await;
        return Err(AssetRejection::TooSmall {
            width: media.width,
            height: media.height,
            min: expectation.min_side,
        });
    }

    if expectation.is_video {
        if media.fps == 0 {
            discard(destination).await;
            return Err(AssetRejection::NoFrameRate);
        }
        let drift = media.duration_ms.abs_diff(expectation.expected_duration_ms);
        if drift > expectation.tolerance_ms {
            discard(destination).await;
            return Err(AssetRejection::DurationMismatch {
                actual_ms: media.duration_ms,
                expected_ms: expectation.expected_duration_ms,
            });
        }
    }

    Ok(ValidatedAsset {
        path: destination.to_path_buf(),
        sha256,
        bytes: bytes_len,
        media,
    })
}

/// 删除半成品；删除失败不重要（下次回收会处理），但不能因此把成功当失败。
async fn discard(path: &Path) {
    let _ = tokio::fs::remove_file(path).await;
}

/// 十六进制小写 SHA-256。
pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::sync::Mutex;

    struct FakeTransport {
        body: Result<Vec<u8>, String>,
    }

    #[async_trait]
    impl GenerationTransport for FakeTransport {
        async fn post(
            &self,
            _url: &str,
            _token: &str,
            _asynchronous: bool,
            _body: &Value,
        ) -> Result<(u16, String), String> {
            Err("unused".to_owned())
        }
        async fn get(&self, _url: &str, _token: &str) -> Result<(u16, String), String> {
            Err("unused".to_owned())
        }
        async fn download(&self, _url: &str) -> Result<Vec<u8>, String> {
            self.body.clone()
        }
    }

    struct FakeProbe {
        result: Result<ProbedMedia, String>,
        calls: Mutex<usize>,
    }

    #[async_trait]
    impl MediaProbe for FakeProbe {
        async fn probe(&self, _path: &Path) -> Result<ProbedMedia, String> {
            *self.calls.lock().unwrap() += 1;
            self.result.clone()
        }
    }

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "dy-screen-asset-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn ok_probe() -> ProbedMedia {
        ProbedMedia {
            width: 1024,
            height: 1024,
            fps: 30,
            duration_ms: 2000,
        }
    }

    #[tokio::test]
    async fn accepts_valid_asset_and_records_hash() {
        let dir = temp_dir();
        let path = dir.join("ok.png");
        let transport = FakeTransport {
            body: Ok(vec![1, 2, 3, 4, 5]),
        };
        let probe = FakeProbe {
            result: Ok(ok_probe()),
            calls: Mutex::new(0),
        };
        let asset = download_and_validate(
            &transport,
            &probe,
            "https://x/a.png",
            &path,
            AssetExpectation::image(),
        )
        .await
        .expect("should accept");
        assert_eq!(asset.bytes, 5);
        assert!(path.exists());
        // 与独立计算的结果一致，确保入库的哈希可用作完整性凭据。
        assert_eq!(asset.sha256, sha256_hex(&[1, 2, 3, 4, 5]));
        assert_eq!(asset.media, ok_probe());
    }

    #[tokio::test]
    async fn rejects_and_removes_file_when_probe_fails() {
        let dir = temp_dir();
        let path = dir.join("probe-fail.png");
        let _ = std::fs::remove_file(&path);
        let transport = FakeTransport { body: Ok(vec![9]) };
        let probe = FakeProbe {
            result: Err("boom".to_owned()),
            calls: Mutex::new(0),
        };
        let error = download_and_validate(
            &transport,
            &probe,
            "https://x/a.png",
            &path,
            AssetExpectation::image(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.code(), "generated_asset_probe_failed");
        assert!(!path.exists(), "校验失败必须删除半成品");
    }

    #[tokio::test]
    async fn rejects_oversized_body_without_writing_anything() {
        let dir = temp_dir();
        let path = dir.join("huge.png");
        let _ = std::fs::remove_file(&path);
        let transport = FakeTransport {
            body: Ok(vec![0; 32]),
        };
        let probe = FakeProbe {
            result: Ok(ok_probe()),
            calls: Mutex::new(0),
        };
        let mut expectation = AssetExpectation::image();
        expectation.max_bytes = 16;
        let error = download_and_validate(&transport, &probe, "https://x/a.png", &path, expectation)
            .await
            .unwrap_err();
        assert!(matches!(error, AssetRejection::TooLarge { .. }));
        assert!(!path.exists());
        assert_eq!(*probe.calls.lock().unwrap(), 0, "超限应在探测前就拒绝");
    }

    #[tokio::test]
    async fn rejects_video_whose_duration_does_not_match_the_request() {
        let dir = temp_dir();
        let path = dir.join("wrong-duration.mp4");
        let _ = std::fs::remove_file(&path);
        // 请求 2 秒，上游给了 10 秒——按秒计费，这是真金白银的偏差，必须拒收。
        let probe = FakeProbe {
            result: Ok(ProbedMedia {
                duration_ms: 10_000,
                ..ok_probe()
            }),
            calls: Mutex::new(0),
        };
        let transport = FakeTransport { body: Ok(vec![7]) };
        let error = download_and_validate(
            &transport,
            &probe,
            "https://x/a.mp4",
            &path,
            AssetExpectation::video(2_000),
        )
        .await
        .unwrap_err();
        assert_eq!(error.code(), "generated_asset_duration_mismatch");
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn rejects_too_small_frame_for_480p_first_frame() {
        let dir = temp_dir();
        let path = dir.join("tiny.png");
        let _ = std::fs::remove_file(&path);
        let probe = FakeProbe {
            result: Ok(ProbedMedia {
                width: 64,
                height: 64,
                fps: 0,
                duration_ms: 0,
            }),
            calls: Mutex::new(0),
        };
        let transport = FakeTransport { body: Ok(vec![3]) };
        let error = download_and_validate(
            &transport,
            &probe,
            "https://x/a.png",
            &path,
            AssetExpectation::image(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.code(), "generated_asset_too_small");
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn download_failure_is_reported_as_download_failed() {
        let dir = temp_dir();
        let path = dir.join("nope.png");
        let transport = FakeTransport {
            body: Err("404".to_owned()),
        };
        let probe = FakeProbe {
            result: Ok(ok_probe()),
            calls: Mutex::new(0),
        };
        let error = download_and_validate(
            &transport,
            &probe,
            "https://x/gone.png",
            &path,
            AssetExpectation::image(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.code(), "generated_asset_download_failed");
    }

    #[test]
    fn parses_ffprobe_json_including_rational_frame_rate() {
        let raw = r#"{"streams":[{"width":854,"height":480,"r_frame_rate":"30/1"}],"format":{"duration":"2.033333"}}"#;
        let media = parse_ffprobe_json(raw).expect("parse");
        assert_eq!(media.width, 854);
        assert_eq!(media.height, 480);
        assert_eq!(media.fps, 30);
        assert_eq!(media.duration_ms, 2033);

        // 缺尺寸必须报错，而不是当成 0×0 通过。
        assert!(parse_ffprobe_json(r#"{"streams":[{}]}"#).is_err());
    }
}
