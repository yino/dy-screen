use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use chrono::Utc;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

use crate::error::{RecorderError, Result};
use crate::model::{EventSink, JobEvent, JobResult, SelectedStream, noop_event_sink};
use crate::resolver::DEFAULT_USER_AGENT;

#[derive(Debug, Clone)]
pub struct FfmpegConfig {
    pub executable: PathBuf,
    pub segment_seconds: u64,
    pub user_agent: String,
}

impl Default for FfmpegConfig {
    fn default() -> Self {
        Self {
            executable: PathBuf::from("ffmpeg"),
            segment_seconds: 900,
            user_agent: DEFAULT_USER_AGENT.to_owned(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct RecordingConfig {
    pub ffmpeg: FfmpegConfig,
    pub ffprobe_executable: PathBuf,
    pub probe_timeout: Duration,
    pub output_root: PathBuf,
    pub shutdown_timeout: Duration,
}

impl Default for RecordingConfig {
    fn default() -> Self {
        Self {
            ffmpeg: FfmpegConfig::default(),
            ffprobe_executable: PathBuf::from("ffprobe"),
            probe_timeout: Duration::from_secs(3),
            output_root: PathBuf::from("recordings"),
            shutdown_timeout: Duration::from_secs(5),
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct FfmpegPlan {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub output_dir: PathBuf,
    pub segment_list: PathBuf,
}

impl std::fmt::Debug for FfmpegPlan {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FfmpegPlan")
            .field("program", &self.program)
            .field("arg_count", &self.args.len())
            .field("output_dir", &self.output_dir)
            .field("segment_list", &self.segment_list)
            .finish()
    }
}

pub fn build_ffmpeg_plan(
    config: &FfmpegConfig,
    stream: &SelectedStream,
    output_dir: &Path,
) -> Result<FfmpegPlan> {
    if config.segment_seconds == 0 {
        return Err(RecorderError::InvalidSegmentDuration);
    }

    let output_pattern = output_dir.join("%Y%m%d-%H%M%S.mkv");
    let segment_list = output_dir.join("segments.csv");
    let args = vec![
        "-hide_banner".into(),
        "-loglevel".into(),
        "warning".into(),
        "-rw_timeout".into(),
        "15000000".into(),
        "-user_agent".into(),
        config.user_agent.clone().into(),
        "-headers".into(),
        "Referer: https://live.douyin.com/\r\n".into(),
        "-i".into(),
        stream.url.clone().into(),
        "-map".into(),
        "0:v:0".into(),
        "-map".into(),
        "0:a:0?".into(),
        "-c".into(),
        "copy".into(),
        "-f".into(),
        "segment".into(),
        "-segment_time".into(),
        config.segment_seconds.to_string().into(),
        "-reset_timestamps".into(),
        "1".into(),
        "-strftime".into(),
        "1".into(),
        "-segment_list".into(),
        segment_list.clone().into_os_string(),
        "-segment_list_type".into(),
        "csv".into(),
        output_pattern.into_os_string(),
    ];

    Ok(FfmpegPlan {
        program: config.executable.clone(),
        args,
        output_dir: output_dir.to_path_buf(),
        segment_list,
    })
}

pub async fn check_ffmpeg(executable: &Path) -> Result<()> {
    let status = Command::new(executable)
        .arg("-version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .map_err(|error| RecorderError::FfmpegUnavailable {
            path: executable.to_path_buf(),
            message: error.to_string(),
        })?;

    if !status.success() {
        return Err(RecorderError::FfmpegUnavailable {
            path: executable.to_path_buf(),
            message: format!("version check exited with {status}"),
        });
    }
    Ok(())
}

#[derive(Clone)]
pub struct FfmpegRecorder {
    config: RecordingConfig,
}

impl FfmpegRecorder {
    pub fn new(config: RecordingConfig) -> Self {
        Self { config }
    }

    pub fn config(&self) -> &RecordingConfig {
        &self.config
    }

    pub async fn record_selected(
        &self,
        room_url: String,
        room_id: String,
        stream: SelectedStream,
        cancellation: CancellationToken,
    ) -> JobResult {
        self.record_selected_with_event_sink(
            room_url,
            room_id,
            stream,
            cancellation,
            noop_event_sink(),
        )
        .await
    }

    pub async fn record_selected_with_event_sink(
        &self,
        room_url: String,
        room_id: String,
        stream: SelectedStream,
        cancellation: CancellationToken,
        events: Arc<dyn EventSink>,
    ) -> JobResult {
        if cancellation.is_cancelled() {
            return JobResult {
                room_url,
                room_id: Some(room_id),
                success: false,
                selected: Some(stream.summary()),
                output_dir: None,
                segments: Vec::new(),
                partial_segments: Vec::new(),
                audio_present: None,
                error: Some(RecorderError::Cancelled.safe_message()),
            };
        }
        match self
            .record_selected_inner(&room_url, &room_id, &stream, cancellation, events)
            .await
        {
            Ok(outcome) => JobResult {
                room_url,
                room_id: Some(room_id),
                success: outcome.success,
                selected: Some(stream.summary()),
                output_dir: Some(outcome.output_dir),
                segments: outcome.segments,
                partial_segments: outcome.partial_segments,
                audio_present: outcome.audio_present,
                error: outcome.error,
            },
            Err(error) => JobResult {
                room_url,
                room_id: Some(room_id),
                success: false,
                selected: Some(stream.summary()),
                output_dir: None,
                segments: Vec::new(),
                partial_segments: Vec::new(),
                audio_present: None,
                error: Some(error.safe_message()),
            },
        }
    }

    async fn record_selected_inner(
        &self,
        room_url: &str,
        room_id: &str,
        stream: &SelectedStream,
        cancellation: CancellationToken,
        events: Arc<dyn EventSink>,
    ) -> Result<RecordingOutcome> {
        tokio::select! {
            _ = cancellation.cancelled() => return Err(RecorderError::Cancelled),
            checked = check_ffmpeg(&self.config.ffmpeg.executable) => checked?,
        }

        let output_dir = create_unique_output_dir(&self.config.output_root, room_id).await?;
        let plan = build_ffmpeg_plan(&self.config.ffmpeg, stream, &output_dir)?;

        if cancellation.is_cancelled() {
            return Err(RecorderError::Cancelled);
        }

        let mut child = Command::new(&plan.program)
            .args(&plan.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| RecorderError::FfmpegUnavailable {
                path: plan.program.clone(),
                message: error.to_string(),
            })?;

        events.emit(JobEvent::RecordingStarted {
            room_url: room_url.to_owned(),
            room_id: room_id.to_owned(),
            output_dir: output_dir.clone(),
        });

        let mut child_stdin = child.stdin.take();

        let (status, cancelled) = tokio::select! {
            status = child.wait() => (status?, false),
            _ = cancellation.cancelled() => {
                if let Some(mut stdin) = child_stdin.take() {
                    let _ = stdin.write_all(b"q\n").await;
                    let _ = stdin.flush().await;
                }
                let status = match timeout(self.config.shutdown_timeout, child.wait()).await {
                    Ok(status) => status?,
                    Err(_) => {
                        let _ = child.start_kill();
                        child.wait().await?
                    }
                };
                (status, true)
            }
        };

        let (segments, partial_segments) =
            collect_segments(&output_dir, &plan.segment_list).await?;
        let audio_present = match segments.first() {
            Some(segment) => probe_audio(
                &self.config.ffprobe_executable,
                segment,
                self.config.probe_timeout,
            )
            .await
            .unwrap_or(None),
            None => None,
        };
        for path in &segments {
            events.emit(JobEvent::SegmentFinalized {
                room_url: room_url.to_owned(),
                room_id: room_id.to_owned(),
                path: path.clone(),
                audio_present,
            });
        }
        if cancelled {
            return Ok(RecordingOutcome {
                output_dir,
                segments,
                partial_segments,
                audio_present,
                success: false,
                error: Some(RecorderError::Cancelled.safe_message()),
            });
        }
        if !status.success() {
            return Ok(RecordingOutcome {
                output_dir,
                segments,
                partial_segments,
                audio_present,
                success: false,
                error: Some(
                    RecorderError::FfmpegFailed {
                        code: status.code(),
                    }
                    .safe_message(),
                ),
            });
        }
        Ok(RecordingOutcome {
            output_dir,
            segments,
            partial_segments,
            audio_present,
            success: true,
            error: None,
        })
    }
}

struct RecordingOutcome {
    output_dir: PathBuf,
    segments: Vec<PathBuf>,
    partial_segments: Vec<PathBuf>,
    audio_present: Option<bool>,
    success: bool,
    error: Option<String>,
}

static SESSION_COUNTER: AtomicU64 = AtomicU64::new(0);

async fn create_unique_output_dir(output_root: &Path, room_id: &str) -> Result<PathBuf> {
    let room_dir = output_root.join(sanitize_component(room_id));
    tokio::fs::create_dir_all(&room_dir).await?;
    for _ in 0..100 {
        let counter = SESSION_COUNTER.fetch_add(1, Ordering::Relaxed);
        let timestamp = Utc::now().format("%Y%m%dT%H%M%S%.9fZ");
        let output_dir = room_dir.join(format!("{timestamp}-{counter:04}"));
        match tokio::fs::create_dir(&output_dir).await {
            Ok(()) => return Ok(output_dir),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "could not allocate a unique recording session directory",
    )
    .into())
}

async fn collect_segments(
    output_dir: &Path,
    segment_list: &Path,
) -> Result<(Vec<PathBuf>, Vec<PathBuf>)> {
    let manifest = tokio::fs::read_to_string(segment_list)
        .await
        .unwrap_or_default();
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(false)
        .flexible(true)
        .from_reader(manifest.as_bytes());
    let mut completed = reader
        .records()
        .filter_map(std::result::Result::ok)
        .filter_map(|record| record.get(0).map(str::to_owned))
        .filter(|value| !value.is_empty())
        .map(|value| {
            let path = PathBuf::from(&value);
            if path.is_absolute() || path.starts_with(output_dir) {
                path
            } else {
                output_dir.join(path)
            }
        })
        .filter(|path| path.exists())
        .collect::<Vec<_>>();
    completed.sort();
    completed.dedup();

    let mut entries = tokio::fs::read_dir(output_dir).await?;
    let mut partial = Vec::new();
    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        if path.extension().is_some_and(|extension| extension == "mkv")
            && !completed.contains(&path)
        {
            partial.push(path);
        }
    }
    partial.sort();
    Ok((completed, partial))
}

async fn probe_audio(
    executable: &Path,
    segment: &Path,
    probe_timeout: Duration,
) -> Result<Option<bool>> {
    let mut command = Command::new(executable);
    command
        .args([
            "-v",
            "error",
            "-select_streams",
            "a:0",
            "-show_entries",
            "stream=index",
            "-of",
            "csv=p=0",
        ])
        .arg(segment)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let output = match timeout(probe_timeout, command.output()).await {
        Ok(output) => output,
        Err(_) => return Ok(None),
    }
    .map_err(|error| RecorderError::FfmpegUnavailable {
        path: executable.to_path_buf(),
        message: error.to_string(),
    })?;
    Ok(Some(
        output.status.success() && !String::from_utf8_lossy(&output.stdout).trim().is_empty(),
    ))
}

fn sanitize_component(value: &str) -> String {
    let sanitized: String = value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect();
    if sanitized.is_empty() {
        "unknown-room".to_owned()
    } else {
        sanitized
    }
}
