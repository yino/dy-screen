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
    SchedulerJob {
        id: id.to_owned(),
        project_id: 1,
        input_id: id.as_bytes()[0] as i64,
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
