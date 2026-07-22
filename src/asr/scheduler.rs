//! 录制优先、全局单并发、FIFO 且可取消的 ASR 后台调度器。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use super::{AsrError, AsrErrorKind, EngineResult};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchedulerJob {
    pub id: String,
    pub project_id: i64,
    pub input_id: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SchedulerEvent {
    Queued(SchedulerJob),
    WaitingForRecording(SchedulerJob),
    Started(SchedulerJob),
    Finished(SchedulerJob),
    Failed { job: SchedulerJob, code: String },
    Cancelled(SchedulerJob),
}

pub trait SchedulerEventSink: Send + Sync {
    fn publish(&self, event: SchedulerEvent);
}

#[async_trait]
pub trait RecordingActivityGate: Send + Sync {
    fn is_recording_active(&self) -> bool;

    async fn wait_until_idle(&self, cancellation: CancellationToken) -> EngineResult<()>;
}

#[async_trait]
pub trait AsrJobExecutor: Send + Sync {
    async fn execute(&self, job: SchedulerJob, cancellation: CancellationToken)
    -> EngineResult<()>;
}

/// 单 worker FIFO 调度器。它不复用录制并发许可，开始每个 ASR 输入前必须先通过录制门控。
pub struct TranscriptionScheduler {
    sender: mpsc::Sender<SchedulerJob>,
    cancellations: Arc<Mutex<HashMap<String, CancellationToken>>>,
    shutdown: CancellationToken,
    worker: Mutex<Option<JoinHandle<()>>>,
    events: Arc<dyn SchedulerEventSink>,
}

impl TranscriptionScheduler {
    pub fn start(
        executor: Arc<dyn AsrJobExecutor>,
        recording_gate: Arc<dyn RecordingActivityGate>,
        events: Arc<dyn SchedulerEventSink>,
        queue_capacity: usize,
    ) -> Self {
        let (sender, mut receiver) = mpsc::channel::<SchedulerJob>(queue_capacity.max(1));
        let cancellations = Arc::new(Mutex::new(HashMap::<String, CancellationToken>::new()));
        let worker_cancellations = cancellations.clone();
        let shutdown = CancellationToken::new();
        let worker_shutdown = shutdown.clone();
        let worker_events = events.clone();
        let worker = tokio::spawn(async move {
            loop {
                let job = tokio::select! {
                    _ = worker_shutdown.cancelled() => break,
                    job = receiver.recv() => match job {
                        Some(job) => job,
                        None => break,
                    },
                };
                let cancellation = worker_cancellations
                    .lock()
                    .ok()
                    .and_then(|tokens| tokens.get(&job.id).cloned())
                    .unwrap_or_default();
                if cancellation.is_cancelled() {
                    worker_events.publish(SchedulerEvent::Cancelled(job.clone()));
                    remove_token(&worker_cancellations, &job.id);
                    continue;
                }
                if recording_gate.is_recording_active() {
                    worker_events.publish(SchedulerEvent::WaitingForRecording(job.clone()));
                }
                let gate_result = tokio::select! {
                    _ = worker_shutdown.cancelled() => break,
                    result = recording_gate.wait_until_idle(cancellation.clone()) => result,
                };
                if gate_result.is_err() || cancellation.is_cancelled() {
                    worker_events.publish(SchedulerEvent::Cancelled(job.clone()));
                    remove_token(&worker_cancellations, &job.id);
                    continue;
                }
                worker_events.publish(SchedulerEvent::Started(job.clone()));
                match executor.execute(job.clone(), cancellation.clone()).await {
                    Ok(()) if !cancellation.is_cancelled() => {
                        worker_events.publish(SchedulerEvent::Finished(job.clone()));
                    }
                    Ok(()) => worker_events.publish(SchedulerEvent::Cancelled(job.clone())),
                    Err(error) if error.kind == AsrErrorKind::Cancelled => {
                        worker_events.publish(SchedulerEvent::Cancelled(job.clone()));
                    }
                    Err(error) => worker_events.publish(SchedulerEvent::Failed {
                        job: job.clone(),
                        code: error.code,
                    }),
                }
                remove_token(&worker_cancellations, &job.id);
            }
        });
        Self {
            sender,
            cancellations,
            shutdown,
            worker: Mutex::new(Some(worker)),
            events,
        }
    }

    pub async fn enqueue(&self, job: SchedulerJob) -> EngineResult<()> {
        if job.id.trim().is_empty() {
            return Err(AsrError::invalid_input(
                "invalid_scheduler_job",
                "ASR 调度任务标识无效",
            ));
        }
        let token = CancellationToken::new();
        self.cancellations
            .lock()
            .map_err(|_| scheduler_state_error())?
            .insert(job.id.clone(), token);
        if self.sender.send(job.clone()).await.is_err() {
            remove_token(&self.cancellations, &job.id);
            return Err(scheduler_state_error());
        }
        self.events.publish(SchedulerEvent::Queued(job));
        Ok(())
    }

    pub fn cancel(&self, job_id: &str) -> bool {
        self.cancellations
            .lock()
            .ok()
            .and_then(|tokens| tokens.get(job_id).cloned())
            .is_some_and(|token| {
                token.cancel();
                true
            })
    }

    pub async fn shutdown(&self) {
        self.shutdown.cancel();
        // 必须先取消正在执行的输入，再等待 worker；反过来会让退出被长模型任务无限阻塞。
        if let Ok(tokens) = self.cancellations.lock() {
            for token in tokens.values() {
                token.cancel();
            }
        }
        let worker = self.worker.lock().ok().and_then(|mut worker| worker.take());
        if let Some(handle) = worker {
            let _ = handle.await;
        }
    }
}

fn remove_token(tokens: &Mutex<HashMap<String, CancellationToken>>, job_id: &str) {
    if let Ok(mut tokens) = tokens.lock() {
        tokens.remove(job_id);
    }
}

fn scheduler_state_error() -> AsrError {
    AsrError::new(
        AsrErrorKind::Internal,
        "asr_scheduler_unavailable",
        "ASR 调度器不可用",
        true,
    )
}
