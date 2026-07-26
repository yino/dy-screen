//! 可选录制优先、全局单并发、可观察且可重排的 ASR 后台调度器。

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use super::{AsrError, AsrErrorKind, EngineResult};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchedulerJob {
    pub id: String,
    pub project_id: i64,
    pub input_id: i64,
    pub generation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchedulerQueueEntry {
    pub job: SchedulerJob,
    pub position: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SchedulerSnapshot {
    pub active: Option<SchedulerJob>,
    pub queued: Vec<SchedulerQueueEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SchedulerEvent {
    Queued(SchedulerJob),
    WaitingForRecording(SchedulerJob),
    Started(SchedulerJob),
    Finished(SchedulerJob),
    Failed {
        job: SchedulerJob,
        code: String,
    },
    Cancelled(SchedulerJob),
    Preempted {
        previous: SchedulerJob,
        requeued: SchedulerJob,
    },
    QueueChanged(SchedulerSnapshot),
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

enum SchedulerCommand {
    Enqueue {
        job: SchedulerJob,
        response: oneshot::Sender<EngineResult<()>>,
    },
    CancelJob {
        job_id: String,
    },
    CancelProject {
        project_id: i64,
        response: oneshot::Sender<EngineResult<()>>,
    },
    PromoteNext {
        job_id: String,
        response: oneshot::Sender<EngineResult<SchedulerSnapshot>>,
    },
    PreemptWith {
        job_id: String,
        response: oneshot::Sender<EngineResult<SchedulerSnapshot>>,
    },
    Snapshot {
        response: oneshot::Sender<SchedulerSnapshot>,
    },
    Shutdown,
}

struct ActiveJob {
    job: SchedulerJob,
    cancellation: CancellationToken,
    requeue_after_preempt: bool,
}

struct JobCompletion {
    id: String,
    generation: u64,
    result: EngineResult<()>,
}

/// 单 actor 拥有队列和当前任务，命令与 executor 完成回调不会并发修改调度状态。
pub struct TranscriptionScheduler {
    sender: mpsc::UnboundedSender<SchedulerCommand>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl TranscriptionScheduler {
    pub fn start(
        executor: Arc<dyn AsrJobExecutor>,
        recording_gate: Arc<dyn RecordingActivityGate>,
        events: Arc<dyn SchedulerEventSink>,
        queue_capacity: usize,
    ) -> Self {
        let (sender, receiver) = mpsc::unbounded_channel();
        let worker = tokio::spawn(run_scheduler(
            receiver,
            executor,
            recording_gate,
            events,
            queue_capacity.max(1),
        ));
        Self {
            sender,
            worker: Mutex::new(Some(worker)),
        }
    }

    pub async fn enqueue(&self, job: SchedulerJob) -> EngineResult<()> {
        validate_job(&job)?;
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(SchedulerCommand::Enqueue { job, response })
            .map_err(|_| scheduler_state_error())?;
        receiver.await.map_err(|_| scheduler_state_error())?
    }

    /// 兼容单任务取消入口；返回值只表示取消命令已被调度器接受。
    pub fn cancel(&self, job_id: &str) -> bool {
        !job_id.trim().is_empty()
            && self
                .sender
                .send(SchedulerCommand::CancelJob {
                    job_id: job_id.to_owned(),
                })
                .is_ok()
    }

    pub async fn cancel_project_and_wait(
        &self,
        project_id: i64,
        timeout: Duration,
    ) -> EngineResult<()> {
        if project_id <= 0 {
            return Err(AsrError::invalid_input(
                "invalid_project_id",
                "AI 项目标识无效",
            ));
        }
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(SchedulerCommand::CancelProject {
                project_id,
                response,
            })
            .map_err(|_| scheduler_state_error())?;
        tokio::time::timeout(timeout, receiver)
            .await
            .map_err(|_| scheduler_cancel_timeout())?
            .map_err(|_| scheduler_state_error())?
    }

    pub async fn promote_next(&self, job_id: &str) -> EngineResult<SchedulerSnapshot> {
        self.reorder(job_id, false).await
    }

    pub async fn preempt_with(&self, job_id: &str) -> EngineResult<SchedulerSnapshot> {
        self.reorder(job_id, true).await
    }

    async fn reorder(&self, job_id: &str, preempt: bool) -> EngineResult<SchedulerSnapshot> {
        if job_id.trim().is_empty() {
            return Err(AsrError::invalid_input(
                "invalid_scheduler_job",
                "ASR 调度任务标识无效",
            ));
        }
        let (response, receiver) = oneshot::channel();
        let command = if preempt {
            SchedulerCommand::PreemptWith {
                job_id: job_id.to_owned(),
                response,
            }
        } else {
            SchedulerCommand::PromoteNext {
                job_id: job_id.to_owned(),
                response,
            }
        };
        self.sender
            .send(command)
            .map_err(|_| scheduler_state_error())?;
        receiver.await.map_err(|_| scheduler_state_error())?
    }

    pub async fn snapshot(&self) -> EngineResult<SchedulerSnapshot> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(SchedulerCommand::Snapshot { response })
            .map_err(|_| scheduler_state_error())?;
        receiver.await.map_err(|_| scheduler_state_error())
    }

    pub async fn shutdown(&self) {
        let _ = self.sender.send(SchedulerCommand::Shutdown);
        let worker = self.worker.lock().ok().and_then(|mut worker| worker.take());
        if let Some(handle) = worker {
            let _ = handle.await;
        }
    }
}

async fn run_scheduler(
    mut commands: mpsc::UnboundedReceiver<SchedulerCommand>,
    executor: Arc<dyn AsrJobExecutor>,
    recording_gate: Arc<dyn RecordingActivityGate>,
    events: Arc<dyn SchedulerEventSink>,
    capacity: usize,
) {
    let (completion_sender, mut completions) = mpsc::unbounded_channel::<JobCompletion>();
    let mut pending = VecDeque::<SchedulerJob>::new();
    let mut active: Option<ActiveJob> = None;
    let mut cancelling_projects = HashSet::<i64>::new();
    let mut cancellation_waiters = HashMap::<i64, Vec<oneshot::Sender<EngineResult<()>>>>::new();
    let mut shutting_down = false;

    loop {
        if active.is_none()
            && !shutting_down
            && let Some(job) = pending.pop_front()
        {
            let cancellation = CancellationToken::new();
            spawn_job(
                job.clone(),
                cancellation.clone(),
                executor.clone(),
                recording_gate.clone(),
                events.clone(),
                completion_sender.clone(),
            );
            active = Some(ActiveJob {
                job,
                cancellation,
                requeue_after_preempt: false,
            });
            publish_snapshot(&events, &active, &pending);
        }

        if shutting_down && active.is_none() {
            finish_all_waiters(&mut cancellation_waiters, Ok(()));
            break;
        }

        tokio::select! {
            command = commands.recv() => {
                let Some(command) = command else {
                    shutting_down = true;
                    cancel_pending(&events, &mut pending);
                    if let Some(active) = &active {
                        active.cancellation.cancel();
                    }
                    continue;
                };
                match command {
                    SchedulerCommand::Enqueue { job, response } => {
                        let result = if shutting_down {
                            Err(scheduler_state_error())
                        } else if cancelling_projects.contains(&job.project_id) {
                            Err(AsrError::new(
                                AsrErrorKind::InvalidInput,
                                "project_is_cancelling",
                                "AI 项目正在取消或删除",
                                false,
                            ))
                        } else if active.as_ref().is_some_and(|current| current.job.id == job.id)
                            || pending.iter().any(|queued| queued.id == job.id)
                        {
                            Err(AsrError::invalid_input(
                                "duplicate_scheduler_job",
                                "ASR 任务已经在队列中",
                            ))
                        } else if pending.len() + usize::from(active.is_some()) >= capacity {
                            Err(AsrError::new(
                                AsrErrorKind::Internal,
                                "asr_queue_full",
                                "ASR 任务队列已满",
                                true,
                            ))
                        } else {
                            pending.push_back(job.clone());
                            events.publish(SchedulerEvent::Queued(job));
                            publish_snapshot(&events, &active, &pending);
                            Ok(())
                        };
                        let _ = response.send(result);
                    }
                    SchedulerCommand::CancelJob { job_id } => {
                        if let Some(position) = pending.iter().position(|job| job.id == job_id) {
                            if let Some(job) = pending.remove(position) {
                                events.publish(SchedulerEvent::Cancelled(job));
                            }
                        } else if let Some(current) = active.as_mut().filter(|current| current.job.id == job_id) {
                            current.requeue_after_preempt = false;
                            current.cancellation.cancel();
                        }
                        publish_snapshot(&events, &active, &pending);
                    }
                    SchedulerCommand::CancelProject { project_id, response } => {
                        cancelling_projects.insert(project_id);
                        let removed = remove_project_jobs(&mut pending, project_id);
                        for job in removed {
                            events.publish(SchedulerEvent::Cancelled(job));
                        }
                        if let Some(current) = active.as_mut().filter(|current| current.job.project_id == project_id) {
                            current.requeue_after_preempt = false;
                            current.cancellation.cancel();
                            cancellation_waiters.entry(project_id).or_default().push(response);
                        } else {
                            cancelling_projects.remove(&project_id);
                            let _ = response.send(Ok(()));
                        }
                        publish_snapshot(&events, &active, &pending);
                    }
                    SchedulerCommand::PromoteNext { job_id, response } => {
                        let result = promote(&mut pending, &job_id)
                            .map(|()| snapshot_of(&active, &pending));
                        if result.is_ok() {
                            publish_snapshot(&events, &active, &pending);
                        }
                        let _ = response.send(result);
                    }
                    SchedulerCommand::PreemptWith { job_id, response } => {
                        let result = promote(&mut pending, &job_id).map(|()| {
                            if let Some(current) = active.as_mut() {
                                current.requeue_after_preempt = true;
                                current.cancellation.cancel();
                            }
                            snapshot_of(&active, &pending)
                        });
                        if result.is_ok() {
                            publish_snapshot(&events, &active, &pending);
                        }
                        let _ = response.send(result);
                    }
                    SchedulerCommand::Snapshot { response } => {
                        let _ = response.send(snapshot_of(&active, &pending));
                    }
                    SchedulerCommand::Shutdown => {
                        shutting_down = true;
                        cancel_pending(&events, &mut pending);
                        if let Some(current) = active.as_mut() {
                            current.requeue_after_preempt = false;
                            current.cancellation.cancel();
                        }
                        publish_snapshot(&events, &active, &pending);
                    }
                }
            }
            completion = completions.recv(), if active.is_some() => {
                let Some(completion) = completion else {
                    shutting_down = true;
                    continue;
                };
                let is_current = active.as_ref().is_some_and(|current| {
                    current.job.id == completion.id && current.job.generation == completion.generation
                });
                if !is_current {
                    continue;
                }
                let completed = active.take().expect("active job checked above");
                let project_id = completed.job.project_id;
                if completed.requeue_after_preempt && !shutting_down && !cancelling_projects.contains(&project_id) {
                    let previous = completed.job;
                    let mut requeued = previous.clone();
                    requeued.generation = requeued.generation.saturating_add(1);
                    pending.push_back(requeued.clone());
                    events.publish(SchedulerEvent::Preempted { previous, requeued });
                } else {
                    publish_completion(&events, completed.job, completion.result);
                }
                if let Some(waiters) = cancellation_waiters.remove(&project_id) {
                    cancelling_projects.remove(&project_id);
                    for waiter in waiters {
                        let _ = waiter.send(Ok(()));
                    }
                }
                publish_snapshot(&events, &active, &pending);
            }
        }
    }
}

fn spawn_job(
    job: SchedulerJob,
    cancellation: CancellationToken,
    executor: Arc<dyn AsrJobExecutor>,
    recording_gate: Arc<dyn RecordingActivityGate>,
    events: Arc<dyn SchedulerEventSink>,
    completion_sender: mpsc::UnboundedSender<JobCompletion>,
) {
    tokio::spawn(async move {
        if recording_gate.is_recording_active() {
            events.publish(SchedulerEvent::WaitingForRecording(job.clone()));
        }
        let result = tokio::select! {
            _ = cancellation.cancelled() => Err(AsrError::cancelled()),
            result = recording_gate.wait_until_idle(cancellation.clone()) => result,
        };
        let result = if result.is_ok() && !cancellation.is_cancelled() {
            events.publish(SchedulerEvent::Started(job.clone()));
            executor.execute(job.clone(), cancellation).await
        } else {
            Err(AsrError::cancelled())
        };
        let _ = completion_sender.send(JobCompletion {
            id: job.id,
            generation: job.generation,
            result,
        });
    });
}

fn validate_job(job: &SchedulerJob) -> EngineResult<()> {
    if job.id.trim().is_empty() || job.project_id <= 0 || job.input_id <= 0 || job.generation == 0 {
        return Err(AsrError::invalid_input(
            "invalid_scheduler_job",
            "ASR 调度任务标识无效",
        ));
    }
    Ok(())
}

fn promote(pending: &mut VecDeque<SchedulerJob>, job_id: &str) -> EngineResult<()> {
    let position = pending
        .iter()
        .position(|job| job.id == job_id)
        .ok_or_else(|| {
            AsrError::invalid_input("scheduler_job_not_pending", "只能调整待处理的 ASR 视频")
        })?;
    let job = pending.remove(position).ok_or_else(scheduler_state_error)?;
    pending.push_front(job);
    Ok(())
}

fn remove_project_jobs(pending: &mut VecDeque<SchedulerJob>, project_id: i64) -> Vec<SchedulerJob> {
    let mut kept = VecDeque::with_capacity(pending.len());
    let mut removed = Vec::new();
    while let Some(job) = pending.pop_front() {
        if job.project_id == project_id {
            removed.push(job);
        } else {
            kept.push_back(job);
        }
    }
    *pending = kept;
    removed
}

fn cancel_pending(events: &Arc<dyn SchedulerEventSink>, pending: &mut VecDeque<SchedulerJob>) {
    while let Some(job) = pending.pop_front() {
        events.publish(SchedulerEvent::Cancelled(job));
    }
}

fn publish_completion(
    events: &Arc<dyn SchedulerEventSink>,
    job: SchedulerJob,
    result: EngineResult<()>,
) {
    match result {
        Ok(()) => events.publish(SchedulerEvent::Finished(job)),
        Err(error) if error.kind == AsrErrorKind::Cancelled => {
            events.publish(SchedulerEvent::Cancelled(job));
        }
        Err(error) => events.publish(SchedulerEvent::Failed {
            job,
            code: error.code,
        }),
    }
}

fn snapshot_of(active: &Option<ActiveJob>, pending: &VecDeque<SchedulerJob>) -> SchedulerSnapshot {
    SchedulerSnapshot {
        active: active.as_ref().map(|active| active.job.clone()),
        queued: pending
            .iter()
            .enumerate()
            .map(|(position, job)| SchedulerQueueEntry {
                job: job.clone(),
                position: position + 1,
            })
            .collect(),
    }
}

fn publish_snapshot(
    events: &Arc<dyn SchedulerEventSink>,
    active: &Option<ActiveJob>,
    pending: &VecDeque<SchedulerJob>,
) {
    events.publish(SchedulerEvent::QueueChanged(snapshot_of(active, pending)));
}

fn finish_all_waiters(
    waiters: &mut HashMap<i64, Vec<oneshot::Sender<EngineResult<()>>>>,
    result: EngineResult<()>,
) {
    for (_, project_waiters) in waiters.drain() {
        for waiter in project_waiters {
            let response = result.as_ref().map(|_| ()).map_err(Clone::clone);
            let _ = waiter.send(response);
        }
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

fn scheduler_cancel_timeout() -> AsrError {
    AsrError::new(
        AsrErrorKind::ProcessFailed,
        "asr_cancel_timeout",
        "ASR 任务未能在期限内停止，请稍后重试",
        true,
    )
}
