use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use clap::{Args, Parser, Subcommand, ValueEnum};
use dy_screen::asr::{
    AsrEngine, AsrError, AsrProgressEvent, AsrProgressSink, AsrRequest, AsrResourceResolver,
    AudioPreparationRequest, FfmpegAudioPreparer, FfprobeMediaInspector, FrozenMediaSource,
    MediaAudioPreparer, MediaInspector, PreparedAudioFormat, ResourcePreflight, SpeechRegion,
    TextNormalizer, TimestampPolicy, VadConfig, VadEngine, WhisperCppEngine, WhisperCppVadEngine,
};
use dy_screen::error::RecorderError;
use dy_screen::manager::{EventSink, LiveJobRunner, RecordingManager};
use dy_screen::model::{JobEvent, ProfileInspection, Protocol, RecordingRequest, redact_url};
use dy_screen::profile_resolver::ProfileResolver;
use dy_screen::recorder::{FfmpegConfig, FfmpegRecorder, RecordingConfig, check_ffmpeg};
use dy_screen::resolver::{DEFAULT_USER_AGENT, RoomDiagnostic, StreamResolver};
use serde::Serialize;
use sha2::Digest;
use thiserror::Error;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Error)]
enum CliError {
    #[error(transparent)]
    Recorder(#[from] RecorderError),
    #[error(transparent)]
    Asr(#[from] AsrError),
    #[error("文件系统错误：{0}")]
    Io(#[from] std::io::Error),
}

impl CliError {
    fn safe_message(&self) -> String {
        match self {
            Self::Recorder(error) => error.safe_message(),
            Self::Asr(error) => error.safe_message.clone(),
            Self::Io(_) => "无法读取或写入本地文件".to_owned(),
        }
    }
}

type CliResult<T> = std::result::Result<T, CliError>;

#[derive(Debug, Parser)]
#[command(
    name = "dy-screen",
    version,
    about = "Resolve and record multiple public Douyin live rooms"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// 检查公开抖音个人主页并发现稳定直播入口。
    InspectProfile(ProfileArgs),
    /// 安全检查公开抖音直播间响应，只输出脱敏元信息和分类。
    InspectRoom(RoomArgs),
    /// Resolve a room and list its available stream qualities.
    Resolve(ResolveArgs),
    /// Record one or more rooms concurrently through FFmpeg.
    Record(RecordArgs),
    /// 对一个本地视频执行完整 FFprobe、FFmpeg、VAD 和本地 ASR 阶段测试。
    Asr(AsrArgs),
}

#[derive(Debug, Args)]
struct AsrArgs {
    /// 要识别的本地视频文件。
    video: PathBuf,

    /// 开发/阶段测试使用的受控 ASR 资源根目录；生产 UI 不暴露此参数。
    #[arg(long, env = "ASR_RESOURCE_ROOT")]
    resource_root: Option<PathBuf>,

    /// 语言提示；默认中文，使用 auto 可自动检测。
    #[arg(long, default_value = "zh")]
    language: String,

    /// 主播名、品牌名或商品名热词，可重复传入。
    #[arg(long = "hotword")]
    hotwords: Vec<String>,

    /// 在指定阶段完成后停止，便于单独诊断媒体、音频、VAD 或完整识别。
    #[arg(long, value_enum, default_value_t = AsrStopAfter::Asr)]
    stop_after: AsrStopAfter,

    /// 输出包含原始文本、规范化文本和时间戳的 JSON。
    #[arg(long)]
    json: bool,
}

/// ASR CLI 可以独立验收的流水线停止点；每个阶段仍执行其必要前置步骤。
#[derive(Debug, Clone, Copy, ValueEnum, PartialEq, Eq)]
enum AsrStopAfter {
    /// 只完成 FFprobe 音轨与时长探测。
    Probe,
    /// 完成 FFprobe 和 FFmpeg 标准音频准备。
    Audio,
    /// 完成 FFprobe、FFmpeg 和 Silero VAD。
    Vad,
    /// 执行完整流水线并返回识别文本；这是默认行为。
    Asr,
}

#[derive(Debug, Args)]
struct RoomArgs {
    /// 公开 live.douyin.com 直播间 URL。
    room_url: String,

    /// 输出机器可读的 JSON。
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct ProfileArgs {
    /// 公开 douyin.com/user 个人主页 URL。
    profile_url: String,

    /// 输出机器可读的 JSON。
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct ResolveArgs {
    /// Public live.douyin.com room URL.
    room_url: String,

    /// Select one quality to demonstrate fallback behavior, for example HD1.
    #[arg(long)]
    quality: Option<String>,

    /// Prefer a specific protocol.
    #[arg(long)]
    protocol: Option<Protocol>,

    /// Emit machine-readable JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct RecordArgs {
    /// Public live.douyin.com room URLs. Supply two or more to validate concurrency.
    #[arg(required = true, num_args = 1..)]
    room_urls: Vec<String>,

    /// Requested quality, for example FULL_HD1 or HD1.
    #[arg(long)]
    quality: Option<String>,

    /// Preferred stream protocol; FLV is preferred when omitted.
    #[arg(long)]
    protocol: Option<Protocol>,

    /// Root directory for room/session recording folders.
    #[arg(long, default_value = "recordings")]
    output: PathBuf,

    /// Length of each recoverable MKV segment in seconds.
    #[arg(long, default_value_t = 900)]
    segment_seconds: u64,

    /// FFmpeg executable or Tauri sidecar path.
    #[arg(long, default_value = "ffmpeg")]
    ffmpeg: PathBuf,

    /// FFprobe executable or Tauri sidecar path used to report audio presence.
    #[arg(long, default_value = "ffprobe")]
    ffprobe: PathBuf,

    /// Maximum seconds to wait for FFprobe track detection per completed job.
    #[arg(long, default_value_t = 3)]
    probe_timeout_seconds: u64,

    /// Emit JSON-lines job events and results.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Serialize)]
struct VariantView {
    quality: String,
    protocol: Protocol,
    endpoint: String,
}

#[derive(Debug, Serialize)]
struct ResolveView {
    room_id: String,
    status: Option<i64>,
    default_quality: Option<String>,
    variants: Vec<VariantView>,
    selected: Option<VariantView>,
    fell_back: Option<bool>,
}

#[derive(Debug, Serialize)]
struct ProfileView {
    status: &'static str,
    profile_sec_uid: String,
    display_name: Option<String>,
    web_rid: Option<String>,
    room_url: Option<String>,
    room_id: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AsrCliView {
    video: String,
    engine_id: String,
    engine_version: String,
    model_id: String,
    model_version: String,
    language: Option<String>,
    duration_ms: u64,
    segments: Vec<AsrCliSegment>,
    text: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AsrCliSegment {
    start_ms: u64,
    end_ms: u64,
    raw_text: String,
    normalized_text: String,
    confidence: Option<f32>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AsrProbeCliView {
    stage: &'static str,
    video: String,
    duration_ms: u64,
    audio_present: bool,
    audio_codec: Option<String>,
    audio_sample_rate_hz: Option<u32>,
    audio_channels: Option<u16>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AsrAudioCliView {
    stage: &'static str,
    video: String,
    source_duration_ms: u64,
    format: PreparedAudioFormat,
    sample_rate_hz: u32,
    channels: u16,
    duration_ms: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AsrVadCliView {
    stage: &'static str,
    video: String,
    duration_ms: u64,
    regions: Vec<SpeechRegion>,
}

/// 清理 CLI 为单个输入创建的哈希隔离目录。
///
/// 守卫先于临时音频 lease 创建，因此 Rust 的逆序析构保证 WAV 文件先关闭、目录后删除；
/// 成功、阶段提前返回、识别失败和 Ctrl-C 取消都会走同一清理边界。
struct CliAsrTemporaryDirectory {
    path: PathBuf,
}

impl CliAsrTemporaryDirectory {
    fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

impl Drop for CliAsrTemporaryDirectory {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_dir_all(&self.path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            eprintln!("warning: 无法清理 ASR CLI 临时目录");
        }
    }
}

struct CliEventSink {
    json: bool,
}

struct CliAsrProgress {
    quiet: bool,
}

impl AsrProgressSink for CliAsrProgress {
    fn publish(&self, event: AsrProgressEvent) {
        if !self.quiet {
            eprintln!("[{}] {}", event.completed_units, event.message);
        }
    }
}

impl EventSink for CliEventSink {
    fn emit(&self, event: JobEvent) -> bool {
        if self.json {
            println!(
                "{}",
                serde_json::to_string(&event).expect("job event is serializable")
            );
            return true;
        }
        match event {
            JobEvent::Queued { room_url } => println!("queued {room_url}"),
            JobEvent::Resolving { room_url } => println!("resolving {room_url}"),
            JobEvent::Resolved {
                room_id, selected, ..
            } => println!(
                "resolved room={} quality={} protocol={} fallback={}",
                room_id, selected.quality, selected.protocol, selected.fell_back
            ),
            JobEvent::RecordingStarted {
                room_id,
                output_dir,
                ..
            } => println!("recording room={} output={}", room_id, output_dir.display()),
            JobEvent::SegmentFinalized {
                room_id,
                path,
                duration_seconds,
                audio_present,
                ..
            } => println!(
                "segment room={} path={} duration_seconds={:?} audio_present={:?}",
                room_id,
                path.display(),
                duration_seconds,
                audio_present
            ),
            JobEvent::Finished { result } if result.success => println!(
                "finished room={} segments={} partial_segments={}",
                result.room_id.as_deref().unwrap_or("unknown"),
                result.segments.len(),
                result.partial_segments.len()
            ),
            JobEvent::Finished { result } => eprintln!(
                "failed room={} error={} completed_segments={} partial_segments={}",
                result.room_id.as_deref().unwrap_or("unknown"),
                result.error.as_deref().unwrap_or("unknown error"),
                result.segments.len(),
                result.partial_segments.len()
            ),
        }
        true
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    match run(Cli::parse()).await {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(error) => {
            eprintln!("error: {}", error.safe_message());
            ExitCode::from(1)
        }
    }
}

async fn run(cli: Cli) -> CliResult<bool> {
    match cli.command {
        Command::InspectProfile(args) => inspect_profile(args).await,
        Command::InspectRoom(args) => inspect_room(args).await,
        Command::Resolve(args) => resolve(args).await,
        Command::Record(args) => record(args).await,
        Command::Asr(args) => transcribe_video(args).await,
    }
}

async fn inspect_room(args: RoomArgs) -> CliResult<bool> {
    let diagnostic = StreamResolver::new()?.diagnose(&args.room_url).await;
    print_room_diagnostic(&diagnostic, args.json);
    Ok(diagnostic.classification.is_success())
}

fn print_room_diagnostic(diagnostic: &RoomDiagnostic, json: bool) {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(diagnostic).expect("room diagnostic is serializable")
        );
        return;
    }
    println!(
        "classification={:?} http_status={} content_type={} response_bytes={}",
        diagnostic.classification,
        diagnostic
            .http_status
            .map(|status| status.to_string())
            .as_deref()
            .unwrap_or("none"),
        diagnostic.content_type.as_deref().unwrap_or("unknown"),
        diagnostic.response_bytes,
    );
    println!(
        "markers access_restricted={} pace_payload={} supported_room={}",
        diagnostic.markers.access_restricted,
        diagnostic.markers.pace_payload,
        diagnostic.markers.supported_room,
    );
    if let Some(error) = &diagnostic.error {
        println!("error={error}");
    }
}

async fn inspect_profile(args: ProfileArgs) -> CliResult<bool> {
    let inspection = ProfileResolver::new()?.inspect(&args.profile_url).await?;
    let view = match inspection {
        ProfileInspection::Offline { identity } => ProfileView {
            status: "offline",
            profile_sec_uid: identity.profile_sec_uid,
            display_name: identity.display_name,
            web_rid: None,
            room_url: None,
            room_id: None,
        },
        ProfileInspection::Live { identity, room } => ProfileView {
            status: "live",
            profile_sec_uid: identity.profile_sec_uid,
            display_name: identity.display_name,
            web_rid: Some(room.web_rid),
            room_url: Some(room.room_url),
            room_id: room.room_id,
        },
    };
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&view).expect("profile view is serializable")
        );
    } else {
        println!(
            "status={} profile_sec_uid={} display_name={} web_rid={} room_id={}",
            view.status,
            view.profile_sec_uid,
            view.display_name.as_deref().unwrap_or("unknown"),
            view.web_rid.as_deref().unwrap_or("not-discovered"),
            view.room_id.as_deref().unwrap_or("not-discovered")
        );
        if let Some(room_url) = view.room_url {
            println!("room_url={room_url}");
        }
    }
    Ok(true)
}

async fn resolve(args: ResolveArgs) -> CliResult<bool> {
    let resolver = StreamResolver::new()?;
    let room = resolver.resolve(&args.room_url).await?;
    let selected = if args.quality.is_some() || args.protocol.is_some() {
        Some(room.select(args.quality.as_deref(), args.protocol)?)
    } else {
        None
    };
    let view = ResolveView {
        room_id: room.room_id.clone(),
        status: room.status,
        default_quality: room.default_quality.clone(),
        variants: room
            .variants
            .iter()
            .map(|variant| VariantView {
                quality: variant.quality.clone(),
                protocol: variant.protocol,
                endpoint: redact_url(&variant.url),
            })
            .collect(),
        selected: selected.as_ref().map(|stream| VariantView {
            quality: stream.quality.clone(),
            protocol: stream.protocol,
            endpoint: stream.redacted_url(),
        }),
        fell_back: selected.as_ref().map(|stream| stream.fell_back),
    };

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&view).expect("resolve view is serializable")
        );
    } else {
        println!(
            "room={} status={:?} default_quality={}",
            view.room_id,
            view.status,
            view.default_quality.as_deref().unwrap_or("unknown")
        );
        for variant in &view.variants {
            println!(
                "  quality={} protocol={} endpoint={}",
                variant.quality, variant.protocol, variant.endpoint
            );
        }
        if let Some(selected) = &view.selected {
            println!(
                "selected quality={} protocol={} endpoint={} fallback={}",
                selected.quality,
                selected.protocol,
                selected.endpoint,
                view.fell_back.unwrap_or(false)
            );
        }
    }
    Ok(true)
}

async fn record(args: RecordArgs) -> CliResult<bool> {
    let ffmpeg = FfmpegConfig {
        executable: args.ffmpeg,
        segment_seconds: args.segment_seconds,
        user_agent: DEFAULT_USER_AGENT.to_owned(),
    };
    check_ffmpeg(&ffmpeg.executable).await?;

    let resolver = StreamResolver::new()?;
    let recorder = FfmpegRecorder::new(RecordingConfig {
        ffmpeg,
        ffprobe_executable: args.ffprobe,
        probe_timeout: Duration::from_secs(args.probe_timeout_seconds),
        output_root: args.output,
        shutdown_timeout: Duration::from_secs(5),
    });
    let events: Arc<dyn EventSink> = Arc::new(CliEventSink { json: args.json });
    let runner = Arc::new(LiveJobRunner::with_event_sink(
        resolver,
        recorder,
        events.clone(),
    ));
    let manager = RecordingManager::with_event_sink(runner, events);
    let requests: Vec<RecordingRequest> = args
        .room_urls
        .into_iter()
        .map(|room_url| RecordingRequest {
            room_url,
            quality: args.quality.clone(),
            protocol: args.protocol,
        })
        .collect();

    if !args.json {
        println!(
            "starting {} recording job(s); press Ctrl-C to stop",
            requests.len()
        );
    }

    let cancellation = CancellationToken::new();
    let signal_cancellation = cancellation.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            signal_cancellation.cancel();
        }
    });

    let results = manager.run_all(requests, cancellation).await;
    let all_succeeded = results.iter().all(|result| result.success);
    Ok(all_succeeded)
}

async fn transcribe_video(args: AsrArgs) -> CliResult<bool> {
    let AsrArgs {
        video,
        resource_root,
        language,
        hotwords,
        stop_after,
        json,
    } = args;
    let resource_root = resource_root
        .or_else(default_asr_resource_root)
        .ok_or_else(|| {
            AsrError::new(
                dy_screen::asr::AsrErrorKind::EnvironmentUnavailable,
                "asr_resource_root_missing",
                "找不到随包 ASR 资源，请通过安装包运行或在阶段测试中设置 ASR_RESOURCE_ROOT",
                false,
            )
        })?;
    let manifest = std::fs::read_to_string(resource_root.join("manifest.json")).map_err(|_| {
        AsrError::new(
            dy_screen::asr::AsrErrorKind::EnvironmentUnavailable,
            "asr_manifest_missing",
            "本地 ASR 资源清单缺失，请重新安装应用",
            false,
        )
    })?;
    let resources = AsrResourceResolver::resolve(&resource_root, &manifest)?;
    let vad_config = VadConfig::default();
    let engine = if stop_after == AsrStopAfter::Asr {
        let engine = WhisperCppEngine::new(
            resources.clone(),
            ResourcePreflight::native(),
            vad_config.clone(),
        );
        let environment = engine.diagnose().await?;
        if !environment.ready {
            return Err(AsrError::new(
                dy_screen::asr::AsrErrorKind::EnvironmentUnavailable,
                "asr_environment_not_ready",
                environment
                    .checks
                    .iter()
                    .find(|check| !check.passed)
                    .map(|check| check.message.clone())
                    .unwrap_or_else(|| "本地 ASR 环境未就绪".to_owned()),
                false,
            )
            .into());
        }
        Some(engine)
    } else {
        None
    };

    let source = FrozenMediaSource::from_path(&video)?;
    let cancellation = CancellationToken::new();
    let signal_cancellation = cancellation.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            signal_cancellation.cancel();
        }
    });
    let inspection = FfprobeMediaInspector::new(resources.ffprobe.clone(), Duration::from_secs(10))
        .inspect(&source, cancellation.child_token())
        .await?;
    if stop_after == AsrStopAfter::Probe {
        let view = AsrProbeCliView {
            stage: "probe",
            video: video.to_string_lossy().into_owned(),
            duration_ms: inspection.duration_ms,
            audio_present: inspection.audio_present,
            audio_codec: inspection.audio_codec,
            audio_sample_rate_hz: inspection.audio_sample_rate_hz,
            audio_channels: inspection.audio_channels,
        };
        if json {
            println!(
                "{}",
                serde_json::to_string_pretty(&view).expect("ASR probe view is serializable")
            );
        } else {
            println!(
                "stage=probe duration_ms={} audio_present={} codec={} sample_rate_hz={} channels={}",
                view.duration_ms,
                view.audio_present,
                view.audio_codec.as_deref().unwrap_or("unknown"),
                view.audio_sample_rate_hz
                    .map(|value| value.to_string())
                    .as_deref()
                    .unwrap_or("unknown"),
                view.audio_channels
                    .map(|value| value.to_string())
                    .as_deref()
                    .unwrap_or("unknown")
            );
        }
        return Ok(true);
    }
    if !inspection.audio_present {
        return Err(AsrError::new(
            dy_screen::asr::AsrErrorKind::InvalidInput,
            "no_audio_track",
            "视频没有音轨，无法进行语音识别",
            false,
        )
        .into());
    }

    let task_hash = hex::encode(sha2::Sha256::digest(
        source.path.to_string_lossy().as_bytes(),
    ));
    let temporary_root = std::env::temp_dir()
        .join("dy-screen-asr-cli")
        .join(&task_hash[..16]);
    let _temporary_directory = CliAsrTemporaryDirectory::new(temporary_root.clone());
    let preparation = AudioPreparationRequest {
        task_id: format!("cli-{}", &task_hash[..16]),
        source,
        duration_ms: inspection.duration_ms,
        temporary_root,
    };
    let audio = FfmpegAudioPreparer::new(resources.ffmpeg.clone())
        .prepare_temporary_wav(&preparation, cancellation.child_token())
        .await?;
    if stop_after == AsrStopAfter::Audio {
        let view = AsrAudioCliView {
            stage: "audio",
            video: video.to_string_lossy().into_owned(),
            source_duration_ms: inspection.duration_ms,
            format: audio.audio().format,
            sample_rate_hz: audio.audio().sample_rate_hz,
            channels: audio.audio().channels,
            duration_ms: audio.audio().duration_ms,
        };
        if json {
            println!(
                "{}",
                serde_json::to_string_pretty(&view).expect("ASR audio view is serializable")
            );
        } else {
            println!(
                "stage=audio format={:?} sample_rate_hz={} channels={} duration_ms={}",
                view.format, view.sample_rate_hz, view.channels, view.duration_ms
            );
        }
        return Ok(true);
    }
    let regions = WhisperCppVadEngine::new(
        resources.vad_sidecar.clone(),
        resources.vad_model.clone(),
        resources.maximum_threads,
        false,
    )
    .detect(audio.audio(), &vad_config, cancellation.child_token())
    .await?;
    if stop_after == AsrStopAfter::Vad {
        let view = AsrVadCliView {
            stage: "vad",
            video: video.to_string_lossy().into_owned(),
            duration_ms: inspection.duration_ms,
            regions,
        };
        if json {
            println!(
                "{}",
                serde_json::to_string_pretty(&view).expect("ASR VAD view is serializable")
            );
        } else {
            println!("stage=vad regions={}", view.regions.len());
            for region in &view.regions {
                println!(
                    "[{} --> {}] speech",
                    format_milliseconds(region.start_ms),
                    format_milliseconds(region.end_ms)
                );
            }
        }
        return Ok(true);
    }
    if regions.is_empty() {
        if json {
            println!(
                "{}",
                serde_json::to_string_pretty(&AsrCliView {
                    video: video.to_string_lossy().into_owned(),
                    engine_id: resources.identity.engine_id,
                    engine_version: resources.identity.engine_version,
                    model_id: resources.identity.model_id,
                    model_version: resources.identity.model_version,
                    language: None,
                    duration_ms: inspection.duration_ms,
                    segments: Vec::new(),
                    text: String::new(),
                })
                .expect("ASR CLI view is serializable")
            );
        } else {
            println!("视频中没有检测到有效人声");
        }
        return Ok(true);
    }

    let engine = engine.ok_or_else(|| {
        AsrError::new(
            dy_screen::asr::AsrErrorKind::Internal,
            "asr_engine_not_initialized",
            "语音识别引擎未正确初始化",
            false,
        )
    })?;
    let result = engine
        .transcribe(
            AsrRequest {
                request_id: format!("cli-{}", &task_hash[..16]),
                audio: audio.audio().clone(),
                language_hint: Some(language),
                hotwords,
                speech_regions: regions,
                timestamp_policy: TimestampPolicy::Segment,
                max_threads: resources.maximum_threads,
            },
            Arc::new(CliAsrProgress { quiet: json }),
            cancellation,
        )
        .await?;
    let mapping = std::fs::read_to_string(&resources.normalization_mapping).unwrap_or_default();
    let normalizer = TextNormalizer::with_opencc_characters("zh-normalize-v1", &mapping);
    let segments: Vec<AsrCliSegment> = result
        .segments
        .into_iter()
        .map(|segment| AsrCliSegment {
            start_ms: segment.start_ms,
            end_ms: segment.end_ms,
            normalized_text: normalizer.normalize(&segment.text),
            raw_text: segment.text,
            confidence: segment.confidence,
        })
        .collect();
    let text = segments
        .iter()
        .map(|segment| segment.normalized_text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let view = AsrCliView {
        video: video.to_string_lossy().into_owned(),
        engine_id: result.identity.engine_id,
        engine_version: result.identity.engine_version,
        model_id: result.identity.model_id,
        model_version: result.identity.model_version,
        language: result.detected_language,
        duration_ms: result.audio_duration_ms,
        segments,
        text,
    };
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&view).expect("ASR CLI view is serializable")
        );
    } else {
        for segment in &view.segments {
            println!(
                "[{} --> {}] {}",
                format_milliseconds(segment.start_ms),
                format_milliseconds(segment.end_ms),
                segment.normalized_text
            );
        }
    }
    Ok(true)
}

fn default_asr_resource_root() -> Option<PathBuf> {
    let executable = std::env::current_exe().ok()?;
    let beside_executable = executable.parent()?.join("resources/asr");
    if beside_executable.join("manifest.json").is_file() {
        return Some(beside_executable);
    }
    let development = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources/asr");
    development
        .join("manifest.json")
        .is_file()
        .then_some(development)
}

fn format_milliseconds(value: u64) -> String {
    let total_seconds = value / 1_000;
    format!(
        "{:02}:{:02}:{:02}.{:03}",
        total_seconds / 3_600,
        (total_seconds / 60) % 60,
        total_seconds % 60,
        value % 1_000
    )
}
