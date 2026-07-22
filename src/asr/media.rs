//! 原始媒体冻结、FFprobe 探测、FFmpeg 音频准备和临时文件生命周期。
//!
//! 本模块只读原始视频，所有派生音频都受任务目录和取消令牌约束；预览缓存不参与 ASR。

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, UNIX_EPOCH};

use async_trait::async_trait;
use serde::Deserialize;
use tokio::io::AsyncRead;
use tokio::process::{Child, ChildStdout, Command};
use tokio_util::sync::CancellationToken;

use super::{
    AsrError, AsrErrorKind, EngineResult, PreparedAudio, PreparedAudioFormat, SpeechRegion,
    VadConfig, VadEngine,
};

/// 开始分析时冻结的原始媒体身份。预览文件不会进入该身份。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrozenMediaSource {
    pub path: PathBuf,
    pub size_bytes: u64,
    pub modified_at_ms: u128,
}

impl FrozenMediaSource {
    /// 规范化并冻结普通文件的大小与修改时间。
    pub fn from_path(path: &Path) -> EngineResult<Self> {
        let canonical = path.canonicalize().map_err(|_| {
            AsrError::new(
                AsrErrorKind::InvalidInput,
                "media_unavailable",
                "视频文件不存在或不可访问",
                false,
            )
        })?;
        let metadata = canonical.metadata().map_err(|_| {
            AsrError::new(
                AsrErrorKind::InvalidInput,
                "media_unavailable",
                "视频文件不存在或不可访问",
                false,
            )
        })?;
        if !metadata.is_file() {
            return Err(AsrError::invalid_input(
                "media_not_file",
                "视频输入必须是普通文件",
            ));
        }
        Ok(Self {
            path: canonical,
            size_bytes: metadata.len(),
            modified_at_ms: modified_at_ms(&metadata)?,
        })
    }

    /// 执行前后均调用该方法，避免识别仍在写入或已被替换的源文件。
    pub fn verify_unchanged(&self) -> EngineResult<()> {
        let metadata = self.path.metadata().map_err(|_| {
            AsrError::new(
                AsrErrorKind::InvalidInput,
                "media_unavailable",
                "视频文件不存在或不可访问",
                false,
            )
        })?;
        if !metadata.is_file()
            || metadata.len() != self.size_bytes
            || modified_at_ms(&metadata)? != self.modified_at_ms
        {
            return Err(AsrError::new(
                AsrErrorKind::InvalidInput,
                "media_changed",
                "视频文件在任务开始后发生变化",
                false,
            ));
        }
        Ok(())
    }
}

fn modified_at_ms(metadata: &std::fs::Metadata) -> EngineResult<u128> {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis())
        .ok_or_else(|| {
            AsrError::new(
                AsrErrorKind::InvalidInput,
                "media_timestamp_unavailable",
                "无法读取视频文件修改时间",
                false,
            )
        })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaInspection {
    pub duration_ms: u64,
    pub audio_present: bool,
    pub audio_codec: Option<String>,
    pub audio_sample_rate_hz: Option<u32>,
    pub audio_channels: Option<u16>,
}

/// 使用参数数组调用 FFprobe，并在探测前后核对冻结源指纹。
#[derive(Debug, Clone)]
pub struct FfprobeMediaInspector {
    executable: PathBuf,
    timeout: Duration,
}

#[async_trait]
pub trait MediaInspector: Send + Sync {
    async fn inspect(
        &self,
        source: &FrozenMediaSource,
        cancellation: CancellationToken,
    ) -> EngineResult<MediaInspection>;
}

impl FfprobeMediaInspector {
    pub fn new(executable: PathBuf, timeout: Duration) -> Self {
        Self {
            executable,
            timeout,
        }
    }

    pub async fn inspect_media(
        &self,
        source: &FrozenMediaSource,
        cancellation: CancellationToken,
    ) -> EngineResult<MediaInspection> {
        source.verify_unchanged()?;
        let mut command = Command::new(&self.executable);
        command.args([
            "-v",
            "error",
            "-show_entries",
            "format=duration:stream=codec_type,codec_name,sample_rate,channels",
            "-of",
            "json",
        ]);
        command.arg(&source.path);
        command.kill_on_drop(true);
        let output = tokio::select! {
            _ = cancellation.cancelled() => return Err(AsrError::cancelled()),
            result = tokio::time::timeout(self.timeout, command.output()) => {
                result.map_err(|_| AsrError::new(
                    AsrErrorKind::ProcessFailed,
                    "ffprobe_timeout",
                    "视频音轨探测超时",
                    true,
                ))?.map_err(|_| AsrError::new(
                    AsrErrorKind::EnvironmentUnavailable,
                    "ffprobe_unavailable",
                    "FFprobe 不可用，请修复应用安装",
                    false,
                ))?
            }
        };
        if !output.status.success() {
            return Err(AsrError::new(
                AsrErrorKind::ProcessFailed,
                "ffprobe_failed",
                "无法读取视频音轨信息",
                true,
            ));
        }
        source.verify_unchanged()?;
        parse_probe_output(&output.stdout)
    }
}

#[async_trait]
impl MediaInspector for FfprobeMediaInspector {
    async fn inspect(
        &self,
        source: &FrozenMediaSource,
        cancellation: CancellationToken,
    ) -> EngineResult<MediaInspection> {
        self.inspect_media(source, cancellation).await
    }
}

#[derive(Debug, Deserialize)]
struct ProbeOutput {
    #[serde(default)]
    streams: Vec<ProbeStream>,
    format: ProbeFormat,
}

#[derive(Debug, Deserialize)]
struct ProbeStream {
    codec_type: String,
    codec_name: Option<String>,
    sample_rate: Option<String>,
    channels: Option<u16>,
}

#[derive(Debug, Deserialize)]
struct ProbeFormat {
    duration: Option<String>,
}

fn parse_probe_output(output: &[u8]) -> EngineResult<MediaInspection> {
    let probe: ProbeOutput = serde_json::from_slice(output).map_err(|_| {
        AsrError::new(
            AsrErrorKind::MalformedOutput,
            "invalid_ffprobe_output",
            "FFprobe 返回了无法解析的媒体信息",
            false,
        )
    })?;
    let duration_seconds = probe
        .format
        .duration
        .as_deref()
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|value| value.is_finite() && *value > 0.0)
        .ok_or_else(|| {
            AsrError::new(
                AsrErrorKind::InvalidInput,
                "media_duration_unknown",
                "视频缺少可靠时长，无法进行语音识别",
                false,
            )
        })?;
    let audio = probe
        .streams
        .into_iter()
        .find(|stream| stream.codec_type == "audio");
    Ok(MediaInspection {
        duration_ms: (duration_seconds * 1_000.0).round() as u64,
        audio_present: audio.is_some(),
        audio_codec: audio.as_ref().and_then(|stream| stream.codec_name.clone()),
        audio_sample_rate_hz: audio
            .as_ref()
            .and_then(|stream| stream.sample_rate.as_deref())
            .and_then(|value| value.parse().ok()),
        audio_channels: audio.and_then(|stream| stream.channels),
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioPreparationCapabilities {
    pub pcm_pipe: bool,
    pub temporary_wav: bool,
}

#[derive(Debug, Clone)]
pub struct AudioPreparationRequest {
    pub task_id: String,
    pub source: FrozenMediaSource,
    pub duration_ms: u64,
    pub temporary_root: PathBuf,
}

/// 临时 WAV 的生命周期守卫。成功、失败和正常作用域退出都会清理文件。
#[derive(Debug)]
pub struct PreparedAudioLease {
    audio: PreparedAudio,
}

impl PreparedAudioLease {
    /// 供可注入媒体实现返回其拥有的临时音频。调用方必须保证路径位于任务临时目录，
    /// 因为守卫析构时会删除该文件。
    pub fn new(audio: PreparedAudio) -> Self {
        Self { audio }
    }

    pub fn audio(&self) -> &PreparedAudio {
        &self.audio
    }

    #[cfg(test)]
    pub(crate) fn for_test(audio: PreparedAudio) -> Self {
        Self::new(audio)
    }
}

impl Drop for PreparedAudioLease {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.audio.path);
    }
}

/// FFmpeg PCM 管道，持有子进程以避免读取端被丢弃后形成孤儿进程。
pub struct PcmPipeProcess {
    child: Child,
    stdout: Option<ChildStdout>,
}

impl PcmPipeProcess {
    pub fn take_stdout(&mut self) -> Option<impl AsyncRead + Unpin + Send + 'static> {
        self.stdout.take()
    }

    pub async fn wait(mut self) -> EngineResult<()> {
        let status = self.child.wait().await.map_err(|_| {
            AsrError::new(
                AsrErrorKind::ProcessFailed,
                "ffmpeg_pipe_failed",
                "音频解码进程异常退出",
                true,
            )
        })?;
        if !status.success() {
            return Err(AsrError::new(
                AsrErrorKind::ProcessFailed,
                "ffmpeg_pipe_failed",
                "音频解码进程异常退出",
                true,
            ));
        }
        Ok(())
    }

    pub async fn cancel(&mut self) {
        let _ = self.child.start_kill();
        let _ = self.child.wait().await;
    }
}

impl Drop for PcmPipeProcess {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}

#[async_trait]
pub trait MediaAudioPreparer: Send + Sync {
    fn capabilities(&self) -> AudioPreparationCapabilities;

    async fn prepare_temporary_wav(
        &self,
        request: &AudioPreparationRequest,
        cancellation: CancellationToken,
    ) -> EngineResult<PreparedAudioLease>;

    async fn open_pcm_pipe(
        &self,
        request: &AudioPreparationRequest,
        cancellation: CancellationToken,
    ) -> EngineResult<PcmPipeProcess>;
}

#[derive(Debug, Clone)]
pub struct FfmpegAudioPreparer {
    executable: PathBuf,
}

impl FfmpegAudioPreparer {
    pub fn new(executable: PathBuf) -> Self {
        Self { executable }
    }

    fn common_arguments(source: &Path) -> Vec<std::ffi::OsString> {
        vec![
            "-hide_banner".into(),
            "-loglevel".into(),
            "error".into(),
            "-nostdin".into(),
            "-i".into(),
            source.as_os_str().to_owned(),
            "-vn".into(),
            "-ac".into(),
            "1".into(),
            "-ar".into(),
            "16000".into(),
            "-c:a".into(),
            "pcm_s16le".into(),
        ]
    }
}

#[async_trait]
impl MediaAudioPreparer for FfmpegAudioPreparer {
    fn capabilities(&self) -> AudioPreparationCapabilities {
        AudioPreparationCapabilities {
            pcm_pipe: true,
            temporary_wav: true,
        }
    }

    async fn prepare_temporary_wav(
        &self,
        request: &AudioPreparationRequest,
        cancellation: CancellationToken,
    ) -> EngineResult<PreparedAudioLease> {
        validate_task_id(&request.task_id)?;
        request.source.verify_unchanged()?;
        tokio::fs::create_dir_all(&request.temporary_root)
            .await
            .map_err(|_| {
                AsrError::new(
                    AsrErrorKind::ResourceExhausted,
                    "temporary_directory_unavailable",
                    "无法创建语音识别临时目录，请检查磁盘空间",
                    true,
                )
            })?;
        let final_path = request
            .temporary_root
            .join(format!("{}.wav", request.task_id));
        let part_path = request
            .temporary_root
            .join(format!("{}.wav.part", request.task_id));
        let _ = tokio::fs::remove_file(&part_path).await;
        let _ = tokio::fs::remove_file(&final_path).await;

        let mut command = Command::new(&self.executable);
        command.args(Self::common_arguments(&request.source.path));
        command.args(["-f", "wav", "-y"]);
        command.arg(&part_path);
        command.stdout(Stdio::null()).stderr(Stdio::piped());
        command.kill_on_drop(true);
        let mut child = command.spawn().map_err(|_| {
            AsrError::new(
                AsrErrorKind::EnvironmentUnavailable,
                "ffmpeg_unavailable",
                "FFmpeg 不可用，请修复应用安装",
                false,
            )
        })?;
        let status = tokio::select! {
            _ = cancellation.cancelled() => {
                let _ = child.start_kill();
                let _ = child.wait().await;
                let _ = tokio::fs::remove_file(&part_path).await;
                return Err(AsrError::cancelled());
            },
            result = child.wait() => result.map_err(|_| AsrError::new(
                AsrErrorKind::ProcessFailed,
                "ffmpeg_failed",
                "音频解码进程异常退出",
                true,
            ))?,
        };
        if !status.success() {
            let _ = tokio::fs::remove_file(&part_path).await;
            return Err(AsrError::new(
                AsrErrorKind::ProcessFailed,
                "ffmpeg_failed",
                "无法从视频中提取标准音频",
                true,
            ));
        }
        request.source.verify_unchanged()?;
        tokio::fs::rename(&part_path, &final_path)
            .await
            .map_err(|_| {
                AsrError::new(
                    AsrErrorKind::Io,
                    "temporary_audio_publish_failed",
                    "无法发布语音识别临时音频",
                    true,
                )
            })?;
        Ok(PreparedAudioLease::new(PreparedAudio {
            path: final_path,
            format: PreparedAudioFormat::PcmS16LeWav,
            sample_rate_hz: 16_000,
            channels: 1,
            duration_ms: request.duration_ms,
        }))
    }

    async fn open_pcm_pipe(
        &self,
        request: &AudioPreparationRequest,
        cancellation: CancellationToken,
    ) -> EngineResult<PcmPipeProcess> {
        if cancellation.is_cancelled() {
            return Err(AsrError::cancelled());
        }
        request.source.verify_unchanged()?;
        let mut command = Command::new(&self.executable);
        command.args(Self::common_arguments(&request.source.path));
        command.args(["-f", "s16le", "pipe:1"]);
        command.stdout(Stdio::piped()).stderr(Stdio::piped());
        command.kill_on_drop(true);
        let mut child = command.spawn().map_err(|_| {
            AsrError::new(
                AsrErrorKind::EnvironmentUnavailable,
                "ffmpeg_unavailable",
                "FFmpeg 不可用，请修复应用安装",
                false,
            )
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            AsrError::new(
                AsrErrorKind::Internal,
                "ffmpeg_pipe_unavailable",
                "无法建立音频解码管道",
                true,
            )
        })?;
        Ok(PcmPipeProcess {
            child,
            stdout: Some(stdout),
        })
    }
}

fn validate_task_id(task_id: &str) -> EngineResult<()> {
    if task_id.is_empty()
        || !task_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(AsrError::invalid_input(
            "invalid_task_id",
            "语音识别任务标识无效",
        ));
    }
    Ok(())
}

/// 启动恢复时只清理 ASR 临时目录内、且不属于活动任务的 WAV/part 文件。
pub fn cleanup_stale_audio_files(
    temporary_root: &Path,
    active_task_ids: &std::collections::HashSet<String>,
) -> EngineResult<usize> {
    if !temporary_root.exists() {
        return Ok(0);
    }
    let mut removed = 0;
    cleanup_stale_audio_directory(
        temporary_root,
        temporary_root,
        active_task_ids,
        &mut removed,
    )?;
    Ok(removed)
}

fn cleanup_stale_audio_directory(
    temporary_root: &Path,
    directory: &Path,
    active_task_ids: &std::collections::HashSet<String>,
    removed: &mut usize,
) -> EngineResult<()> {
    let entries = std::fs::read_dir(directory).map_err(|_| temporary_cleanup_error())?;
    for entry in entries {
        let entry = entry.map_err(|_| temporary_cleanup_error())?;
        let file_type = entry.file_type().map_err(|_| temporary_cleanup_error())?;
        let path = entry.path();
        // 不跟随符号链接，确保清理边界始终限制在应用 ASR 缓存目录内。
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            cleanup_stale_audio_directory(temporary_root, &path, active_task_ids, removed)?;
            if path != temporary_root
                && std::fs::read_dir(&path)
                    .map_err(|_| temporary_cleanup_error())?
                    .next()
                    .is_none()
            {
                std::fs::remove_dir(&path).map_err(|_| temporary_cleanup_error())?;
            }
            continue;
        }
        if !file_type.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let task_id = name
            .strip_suffix(".wav.part")
            .or_else(|| name.strip_suffix(".wav"));
        let Some(task_id) = task_id else {
            continue;
        };
        if active_task_ids.contains(task_id) {
            continue;
        }
        std::fs::remove_file(&path).map_err(|_| temporary_cleanup_error())?;
        *removed += 1;
    }
    Ok(())
}

fn temporary_cleanup_error() -> AsrError {
    AsrError::new(
        AsrErrorKind::Io,
        "temporary_audio_cleanup_failed",
        "无法清理遗留语音识别临时文件",
        true,
    )
}

/// 组合媒体准备和 VAD 的可测试阶段边界。
pub async fn prepare_and_detect_speech(
    preparer: &dyn MediaAudioPreparer,
    vad: &dyn VadEngine,
    request: &AudioPreparationRequest,
    config: &VadConfig,
    cancellation: CancellationToken,
) -> EngineResult<(PreparedAudioLease, Vec<SpeechRegion>)> {
    if cancellation.is_cancelled() {
        return Err(AsrError::cancelled());
    }
    let lease = preparer
        .prepare_temporary_wav(request, cancellation.clone())
        .await?;
    let regions = vad.detect(lease.audio(), config, cancellation).await?;
    Ok((lease, regions))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use async_trait::async_trait;
    use tempfile::tempdir;
    use tokio_util::sync::CancellationToken;

    use super::{
        AsrError, AsrErrorKind, AudioPreparationCapabilities, AudioPreparationRequest,
        EngineResult, FrozenMediaSource, MediaAudioPreparer, PcmPipeProcess, PreparedAudio,
        PreparedAudioFormat, PreparedAudioLease, SpeechRegion, VadConfig, VadEngine,
        parse_probe_output, prepare_and_detect_speech,
    };

    enum PrepareMode {
        Success,
        NoAudio,
        Failure,
    }

    struct FakePreparer(PrepareMode);

    #[async_trait]
    impl MediaAudioPreparer for FakePreparer {
        fn capabilities(&self) -> AudioPreparationCapabilities {
            AudioPreparationCapabilities {
                pcm_pipe: false,
                temporary_wav: true,
            }
        }

        async fn prepare_temporary_wav(
            &self,
            request: &AudioPreparationRequest,
            cancellation: CancellationToken,
        ) -> EngineResult<PreparedAudioLease> {
            if cancellation.is_cancelled() {
                return Err(AsrError::cancelled());
            }
            match self.0 {
                PrepareMode::Success => {
                    let path = request.temporary_root.join("fake.wav");
                    std::fs::create_dir_all(&request.temporary_root).unwrap();
                    std::fs::write(&path, b"fake pcm").unwrap();
                    Ok(PreparedAudioLease::for_test(PreparedAudio {
                        path,
                        format: PreparedAudioFormat::PcmS16LeWav,
                        sample_rate_hz: 16_000,
                        channels: 1,
                        duration_ms: request.duration_ms,
                    }))
                }
                PrepareMode::NoAudio => Err(AsrError::new(
                    AsrErrorKind::InvalidInput,
                    "no_audio_track",
                    "视频没有音轨",
                    false,
                )),
                PrepareMode::Failure => Err(AsrError::new(
                    AsrErrorKind::ProcessFailed,
                    "fake_prepare_failed",
                    "音频准备失败",
                    true,
                )),
            }
        }

        async fn open_pcm_pipe(
            &self,
            _request: &AudioPreparationRequest,
            _cancellation: CancellationToken,
        ) -> EngineResult<PcmPipeProcess> {
            Err(AsrError::new(
                AsrErrorKind::Internal,
                "fake_pipe_unsupported",
                "测试替身不支持 PCM 管道",
                false,
            ))
        }
    }

    enum VadMode {
        Speech,
        NoSpeech,
        Failure,
    }

    struct FakeVad(VadMode);

    #[async_trait]
    impl VadEngine for FakeVad {
        async fn detect(
            &self,
            audio: &PreparedAudio,
            _config: &VadConfig,
            cancellation: CancellationToken,
        ) -> EngineResult<Vec<SpeechRegion>> {
            if cancellation.is_cancelled() {
                return Err(AsrError::cancelled());
            }
            match self.0 {
                VadMode::Speech => Ok(vec![SpeechRegion {
                    start_ms: 0,
                    end_ms: audio.duration_ms,
                }]),
                VadMode::NoSpeech => Ok(Vec::new()),
                VadMode::Failure => Err(AsrError::new(
                    AsrErrorKind::ProcessFailed,
                    "fake_vad_failed",
                    "VAD 失败",
                    true,
                )),
            }
        }
    }

    fn fake_request(root: PathBuf) -> AudioPreparationRequest {
        let source_path = root.join("source.mp4");
        std::fs::write(&source_path, b"source").unwrap();
        AudioPreparationRequest {
            task_id: "fake-task".to_owned(),
            source: FrozenMediaSource::from_path(&source_path).unwrap(),
            duration_ms: 1_000,
            temporary_root: root.join("temporary"),
        }
    }

    #[test]
    fn ffprobe_parser_rejects_unknown_duration() {
        let error =
            parse_probe_output(br#"{"streams":[],"format":{"duration":"N/A"}}"#).unwrap_err();
        assert_eq!(error.code, "media_duration_unknown");
    }

    #[test]
    fn ffprobe_parser_reports_video_without_audio() {
        let probe = parse_probe_output(
            br#"{"streams":[{"codec_type":"video"}],"format":{"duration":"3.0"}}"#,
        )
        .unwrap();
        assert!(!probe.audio_present);
        assert_eq!(probe.duration_ms, 3_000);
    }

    #[tokio::test]
    async fn injectable_pipeline_covers_success_no_audio_no_speech_failure_and_cancel() {
        let directory = tempdir().unwrap();
        let request = fake_request(directory.path().to_path_buf());
        let config = VadConfig::default();

        let (_, regions) = prepare_and_detect_speech(
            &FakePreparer(PrepareMode::Success),
            &FakeVad(VadMode::Speech),
            &request,
            &config,
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(regions.len(), 1);

        let error = prepare_and_detect_speech(
            &FakePreparer(PrepareMode::NoAudio),
            &FakeVad(VadMode::Speech),
            &request,
            &config,
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, "no_audio_track");

        let (_, regions) = prepare_and_detect_speech(
            &FakePreparer(PrepareMode::Success),
            &FakeVad(VadMode::NoSpeech),
            &request,
            &config,
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert!(regions.is_empty());

        let error = prepare_and_detect_speech(
            &FakePreparer(PrepareMode::Success),
            &FakeVad(VadMode::Failure),
            &request,
            &config,
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, "fake_vad_failed");

        let error = prepare_and_detect_speech(
            &FakePreparer(PrepareMode::Failure),
            &FakeVad(VadMode::Speech),
            &request,
            &config,
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, "fake_prepare_failed");

        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let error = prepare_and_detect_speech(
            &FakePreparer(PrepareMode::Success),
            &FakeVad(VadMode::Speech),
            &request,
            &config,
            cancellation,
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, "asr_cancelled");
    }
}
