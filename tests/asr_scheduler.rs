use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use dy_screen::asr::{
    AsrError, AsrErrorKind, AsrJobExecutor, EngineResult, RecordingActivityGate, SchedulerEvent,
    SchedulerEventSink, SchedulerJob, TranscriptionScheduler,
};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

struct FakeRecordingGate {
    active: AtomicBool,
    changed: Notify,
}

impl FakeRecordingGate {
    fn new(active: bool) -> Self {
        Self {
            active: AtomicBool::new(active),
            changed: Notify::new(),
        }
    }

    fn set_active(&self, active: bool) {
        self.active.store(active, Ordering::SeqCst);
        self.changed.notify_waiters();
    }
}

#[async_trait]
impl RecordingActivityGate for FakeRecordingGate {
    fn is_recording_active(&self) -> bool {
        self.active.load(Ordering::SeqCst)
    }

    async fn wait_until_idle(&self, cancellation: CancellationToken) -> EngineResult<()> {
        while self.is_recording_active() {
            tokio::select! {
                _ = cancellation.cancelled() => return Err(AsrError::cancelled()),
                _ = self.changed.notified() => {}
            }
        }
        Ok(())
    }
}

struct FakeExecutor {
    order: Mutex<Vec<String>>,
    active: AtomicUsize,
    maximum_active: AtomicUsize,
    fail_job: Option<String>,
    duration: Duration,
}

struct IgnoringCancellationExecutor {
    duration: Duration,
}

#[async_trait]
impl AsrJobExecutor for IgnoringCancellationExecutor {
    async fn execute(
        &self,
        _job: SchedulerJob,
        _cancellation: CancellationToken,
    ) -> EngineResult<()> {
        tokio::time::sleep(self.duration).await;
        Ok(())
    }
}

#[async_trait]
impl AsrJobExecutor for FakeExecutor {
    async fn execute(
        &self,
        job: SchedulerJob,
        cancellation: CancellationToken,
    ) -> EngineResult<()> {
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.maximum_active.fetch_max(active, Ordering::SeqCst);
        self.order.lock().unwrap().push(job.id.clone());
        tokio::select! {
            _ = cancellation.cancelled() => {
                self.active.fetch_sub(1, Ordering::SeqCst);
                return Err(AsrError::cancelled());
            }
            _ = tokio::time::sleep(self.duration) => {}
        }
        self.active.fetch_sub(1, Ordering::SeqCst);
        if self.fail_job.as_deref() == Some(&job.id) {
            return Err(AsrError::new(
                AsrErrorKind::ProcessFailed,
                "fake_failure",
                "测试任务失败",
                true,
            ));
        }
        Ok(())
    }
}

#[derive(Default)]
struct EventCollector(Mutex<Vec<SchedulerEvent>>);

impl SchedulerEventSink for EventCollector {
    fn publish(&self, event: SchedulerEvent) {
        self.0.lock().unwrap().push(event);
    }
}

fn job(id: &str) -> SchedulerJob {
    project_job(id, 1)
}

fn project_job(id: &str, project_id: i64) -> SchedulerJob {
    SchedulerJob {
        id: id.to_owned(),
        project_id,
        input_id: id.as_bytes()[0] as i64,
        generation: 1,
    }
}

async fn wait_for_terminal_events(events: &EventCollector, count: usize) {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let terminal = events
                .0
                .lock()
                .unwrap()
                .iter()
                .filter(|event| {
                    matches!(
                        event,
                        SchedulerEvent::Finished(_)
                            | SchedulerEvent::Failed { .. }
                            | SchedulerEvent::Cancelled(_)
                    )
                })
                .count();
            if terminal >= count {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("scheduler reaches terminal events");
}

#[tokio::test]
async fn scheduler_is_fifo_single_concurrency_and_waits_for_recording_without_window_state() {
    let gate = Arc::new(FakeRecordingGate::new(true));
    let executor = Arc::new(FakeExecutor {
        order: Mutex::new(Vec::new()),
        active: AtomicUsize::new(0),
        maximum_active: AtomicUsize::new(0),
        fail_job: None,
        duration: Duration::from_millis(40),
    });
    let events = Arc::new(EventCollector::default());
    let scheduler =
        TranscriptionScheduler::start(executor.clone(), gate.clone(), events.clone(), 8);
    scheduler.enqueue(job("a")).await.unwrap();
    scheduler.enqueue(job("b")).await.unwrap();
    scheduler.enqueue(job("c")).await.unwrap();
    tokio::time::sleep(Duration::from_millis(80)).await;
    assert!(executor.order.lock().unwrap().is_empty());
    assert!(
        events
            .0
            .lock()
            .unwrap()
            .iter()
            .any(|event| matches!(event, SchedulerEvent::WaitingForRecording(_)))
    );

    gate.set_active(false);
    wait_for_terminal_events(&events, 3).await;
    assert_eq!(&*executor.order.lock().unwrap(), &["a", "b", "c"]);
    assert_eq!(executor.maximum_active.load(Ordering::SeqCst), 1);
    scheduler.shutdown().await;
}

#[tokio::test]
async fn cancellation_and_failure_release_the_single_execution_slot() {
    let gate = Arc::new(FakeRecordingGate::new(false));
    let executor = Arc::new(FakeExecutor {
        order: Mutex::new(Vec::new()),
        active: AtomicUsize::new(0),
        maximum_active: AtomicUsize::new(0),
        fail_job: Some("a".to_owned()),
        duration: Duration::from_millis(40),
    });
    let events = Arc::new(EventCollector::default());
    let scheduler = TranscriptionScheduler::start(executor.clone(), gate, events.clone(), 8);
    scheduler.enqueue(job("a")).await.unwrap();
    scheduler.enqueue(job("b")).await.unwrap();
    scheduler.enqueue(job("c")).await.unwrap();
    assert!(scheduler.cancel("b"));
    wait_for_terminal_events(&events, 3).await;
    assert_eq!(&*executor.order.lock().unwrap(), &["a", "c"]);
    assert!(
        events
            .0
            .lock()
            .unwrap()
            .iter()
            .any(|event| matches!(event, SchedulerEvent::Failed { job, .. } if job.id == "a"))
    );
    assert!(
        events
            .0
            .lock()
            .unwrap()
            .iter()
            .any(|event| matches!(event, SchedulerEvent::Cancelled(job) if job.id == "b"))
    );
    assert!(
        events
            .0
            .lock()
            .unwrap()
            .iter()
            .any(|event| matches!(event, SchedulerEvent::Finished(job) if job.id == "c"))
    );
    assert_eq!(executor.maximum_active.load(Ordering::SeqCst), 1);
    scheduler.shutdown().await;
}

#[tokio::test]
async fn shutdown_cancels_the_running_job_before_waiting_for_worker_exit() {
    let gate = Arc::new(FakeRecordingGate::new(false));
    let executor = Arc::new(FakeExecutor {
        order: Mutex::new(Vec::new()),
        active: AtomicUsize::new(0),
        maximum_active: AtomicUsize::new(0),
        fail_job: None,
        duration: Duration::from_secs(10),
    });
    let events = Arc::new(EventCollector::default());
    let scheduler = TranscriptionScheduler::start(executor.clone(), gate, events.clone(), 8);
    scheduler.enqueue(job("a")).await.unwrap();
    tokio::time::timeout(Duration::from_secs(1), async {
        while executor.active.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("fake ASR job starts");

    tokio::time::timeout(Duration::from_secs(1), scheduler.shutdown())
        .await
        .expect("shutdown must not wait for the ten-second fake ASR job");
    assert_eq!(executor.active.load(Ordering::SeqCst), 0);
    assert!(
        events
            .0
            .lock()
            .unwrap()
            .iter()
            .any(|event| matches!(event, SchedulerEvent::Cancelled(job) if job.id == "a"))
    );
}

#[tokio::test]
async fn deleting_the_active_project_removes_its_queue_and_releases_the_next_project() {
    let gate = Arc::new(FakeRecordingGate::new(false));
    let executor = Arc::new(FakeExecutor {
        order: Mutex::new(Vec::new()),
        active: AtomicUsize::new(0),
        maximum_active: AtomicUsize::new(0),
        fail_job: None,
        duration: Duration::from_secs(10),
    });
    let events = Arc::new(EventCollector::default());
    let scheduler = TranscriptionScheduler::start(executor.clone(), gate, events.clone(), 8);
    scheduler.enqueue(project_job("a", 10)).await.unwrap();
    scheduler.enqueue(project_job("b", 10)).await.unwrap();
    scheduler.enqueue(project_job("c", 20)).await.unwrap();

    tokio::time::timeout(Duration::from_secs(1), async {
        while executor.active.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("first project starts");

    scheduler
        .cancel_project_and_wait(10, Duration::from_secs(1))
        .await
        .expect("project cancellation is acknowledged after executor release");
    let snapshot = scheduler.snapshot().await.unwrap();
    assert_ne!(snapshot.active.as_ref().map(|job| job.project_id), Some(10));
    assert!(
        snapshot
            .queued
            .iter()
            .all(|entry| entry.job.project_id != 10)
    );

    tokio::time::timeout(Duration::from_secs(1), async {
        while !executor.order.lock().unwrap().iter().any(|id| id == "c") {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("next project starts after deletion cancellation");
    assert_eq!(&*executor.order.lock().unwrap(), &["a", "c"]);
    assert_eq!(executor.maximum_active.load(Ordering::SeqCst), 1);
    assert!(
        events
            .0
            .lock()
            .unwrap()
            .iter()
            .any(|event| matches!(event, SchedulerEvent::Cancelled(job) if job.id == "b"))
    );
    scheduler.shutdown().await;
}

#[tokio::test]
async fn promote_and_preempt_reorder_without_duplicate_or_concurrent_execution() {
    let gate = Arc::new(FakeRecordingGate::new(false));
    let executor = Arc::new(FakeExecutor {
        order: Mutex::new(Vec::new()),
        active: AtomicUsize::new(0),
        maximum_active: AtomicUsize::new(0),
        fail_job: None,
        duration: Duration::from_millis(80),
    });
    let events = Arc::new(EventCollector::default());
    let scheduler = TranscriptionScheduler::start(executor.clone(), gate, events.clone(), 8);
    scheduler.enqueue(project_job("a", 1)).await.unwrap();
    scheduler.enqueue(project_job("b", 2)).await.unwrap();
    scheduler.enqueue(project_job("c", 3)).await.unwrap();

    tokio::time::timeout(Duration::from_secs(1), async {
        while executor.active.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("first job starts");
    let promoted = scheduler.promote_next("c").await.unwrap();
    assert_eq!(promoted.queued[0].job.id, "c");
    let preempting = scheduler.preempt_with("c").await.unwrap();
    assert_eq!(
        preempting.active.as_ref().map(|job| job.id.as_str()),
        Some("a")
    );

    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let snapshot = scheduler.snapshot().await.unwrap();
            if snapshot.active.is_none() && snapshot.queued.is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("preempted and queued work completes");

    assert_eq!(&*executor.order.lock().unwrap(), &["a", "c", "b", "a"]);
    assert_eq!(executor.maximum_active.load(Ordering::SeqCst), 1);
    assert!(events.0.lock().unwrap().iter().any(|event| {
        matches!(
            event,
            SchedulerEvent::Preempted { previous, requeued }
                if previous.id == "a" && previous.generation == 1 && requeued.generation == 2
        )
    }));
    scheduler.shutdown().await;
}

#[tokio::test]
async fn project_cancellation_timeout_is_bounded_and_scheduler_recovers_after_executor_returns() {
    let gate = Arc::new(FakeRecordingGate::new(false));
    let events = Arc::new(EventCollector::default());
    let scheduler = TranscriptionScheduler::start(
        Arc::new(IgnoringCancellationExecutor {
            duration: Duration::from_millis(120),
        }),
        gate,
        events,
        4,
    );
    scheduler.enqueue(project_job("a", 10)).await.unwrap();
    tokio::time::sleep(Duration::from_millis(10)).await;

    let error = scheduler
        .cancel_project_and_wait(10, Duration::from_millis(10))
        .await
        .unwrap_err();
    assert_eq!(error.code, "asr_cancel_timeout");

    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if scheduler.snapshot().await.unwrap().active.is_none() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("actor remains usable after cancellation caller times out");
    scheduler.shutdown().await;
}
