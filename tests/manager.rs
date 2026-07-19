use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

use async_trait::async_trait;
use dy_screen::manager::{EventSink, JobRunner, RecordingManager};
use dy_screen::model::{JobEvent, JobResult, RecordingRequest};
use dy_screen::recorder::{FfmpegRecorder, RecordingConfig};
use dy_screen::resolver::StreamResolver;
use std::sync::Mutex;
use tokio_util::sync::CancellationToken;

struct FakeRunner {
    active: Arc<AtomicUsize>,
    max_active: Arc<AtomicUsize>,
}

#[derive(Default)]
struct CapturingSink {
    events: Mutex<Vec<JobEvent>>,
}

impl EventSink for CapturingSink {
    fn emit(&self, event: JobEvent) -> bool {
        self.events.lock().expect("event lock").push(event);
        true
    }
}

#[async_trait]
impl JobRunner for FakeRunner {
    async fn run_job(
        &self,
        request: RecordingRequest,
        _cancellation: CancellationToken,
    ) -> JobResult {
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_active.fetch_max(active, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(40)).await;
        self.active.fetch_sub(1, Ordering::SeqCst);

        if request.room_url.contains("fails") {
            JobResult::failure(request.room_url, "synthetic failure")
        } else {
            JobResult::success(request.room_url)
        }
    }
}

#[tokio::test]
async fn runs_rooms_concurrently_and_isolates_failures() {
    let active = Arc::new(AtomicUsize::new(0));
    let max_active = Arc::new(AtomicUsize::new(0));
    let manager = RecordingManager::new(Arc::new(FakeRunner {
        active,
        max_active: max_active.clone(),
    }));
    let requests = vec![
        RecordingRequest::new("https://live.douyin.com/works"),
        RecordingRequest::new("https://live.douyin.com/fails"),
    ];

    let results = manager.run_all(requests, CancellationToken::new()).await;

    assert_eq!(results.len(), 2);
    assert!(max_active.load(Ordering::SeqCst) >= 2);
    assert_eq!(results.iter().filter(|result| result.success).count(), 1);
    assert_eq!(results.iter().filter(|result| !result.success).count(), 1);
}

#[tokio::test]
async fn emits_serializable_queued_and_finished_events() {
    let sink = Arc::new(CapturingSink::default());
    let manager = RecordingManager::with_event_sink(
        Arc::new(FakeRunner {
            active: Arc::new(AtomicUsize::new(0)),
            max_active: Arc::new(AtomicUsize::new(0)),
        }),
        sink.clone(),
    );
    manager
        .run_all(
            vec![RecordingRequest::new("https://live.douyin.com/room")],
            CancellationToken::new(),
        )
        .await;

    let events = sink.events.lock().expect("event lock");
    assert!(matches!(events.first(), Some(JobEvent::Queued { .. })));
    assert!(matches!(events.last(), Some(JobEvent::Finished { .. })));
    for event in events.iter() {
        serde_json::to_string(event).expect("serializable core event");
    }
}

#[tokio::test]
async fn already_cancelled_live_job_never_starts_resolution_or_ffmpeg() {
    let runner = dy_screen::manager::LiveJobRunner::new(
        StreamResolver::new().expect("resolver"),
        FfmpegRecorder::new(RecordingConfig::default()),
    );
    let cancellation = CancellationToken::new();
    cancellation.cancel();

    let result = runner
        .run_job(
            RecordingRequest::new("https://live.douyin.com/room"),
            cancellation,
        )
        .await;

    assert!(!result.success);
    assert_eq!(result.error.as_deref(), Some("recording was cancelled"));
    assert!(result.output_dir.is_none());
}
