use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use clap::{Args, Parser, Subcommand};
use dy_screen::error::Result;
use dy_screen::manager::{EventSink, LiveJobRunner, RecordingManager};
use dy_screen::model::{JobEvent, Protocol, RecordingRequest, redact_url};
use dy_screen::recorder::{FfmpegConfig, FfmpegRecorder, RecordingConfig, check_ffmpeg};
use dy_screen::resolver::{DEFAULT_USER_AGENT, StreamResolver};
use serde::Serialize;
use tokio_util::sync::CancellationToken;

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
    /// Resolve a room and list its available stream qualities.
    Resolve(ResolveArgs),
    /// Record one or more rooms concurrently through FFmpeg.
    Record(RecordArgs),
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

struct CliEventSink {
    json: bool,
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

async fn run(cli: Cli) -> Result<bool> {
    match cli.command {
        Command::Resolve(args) => resolve(args).await,
        Command::Record(args) => record(args).await,
    }
}

async fn resolve(args: ResolveArgs) -> Result<bool> {
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

async fn record(args: RecordArgs) -> Result<bool> {
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
