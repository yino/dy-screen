use std::sync::Arc;

use async_trait::async_trait;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

pub use crate::model::EventSink;
use crate::model::{JobEvent, JobResult, RecordingRequest, noop_event_sink};
use crate::recorder::FfmpegRecorder;
use crate::resolver::StreamResolver;

#[async_trait]
pub trait JobRunner: Send + Sync {
    async fn run_job(
        &self,
        request: RecordingRequest,
        cancellation: CancellationToken,
    ) -> JobResult;
}

pub struct RecordingManager<R: JobRunner> {
    runner: Arc<R>,
    events: Arc<dyn EventSink>,
}

impl<R: JobRunner + 'static> RecordingManager<R> {
    pub fn new(runner: Arc<R>) -> Self {
        Self {
            runner,
            events: noop_event_sink(),
        }
    }

    pub fn with_event_sink(runner: Arc<R>, events: Arc<dyn EventSink>) -> Self {
        Self { runner, events }
    }

    pub async fn run_all(
        &self,
        requests: Vec<RecordingRequest>,
        cancellation: CancellationToken,
    ) -> Vec<JobResult> {
        let mut jobs = JoinSet::new();
        for (index, request) in requests.into_iter().enumerate() {
            self.events.emit(JobEvent::Queued {
                room_url: request.room_url.clone(),
            });
            let runner = self.runner.clone();
            let job_cancellation = cancellation.child_token();
            jobs.spawn(async move {
                let result = runner.run_job(request, job_cancellation).await;
                (index, result)
            });
        }

        let mut results = Vec::new();
        while let Some(joined) = jobs.join_next().await {
            match joined {
                Ok((index, result)) => {
                    self.events.emit(JobEvent::Finished {
                        result: result.clone(),
                    });
                    results.push((index, result));
                }
                Err(error) => {
                    let result = JobResult::failure(
                        "<recording-task>",
                        format!("recording task failed: {error}"),
                    );
                    self.events.emit(JobEvent::Finished {
                        result: result.clone(),
                    });
                    results.push((usize::MAX, result));
                }
            }
        }
        results.sort_by_key(|(index, _)| *index);
        results.into_iter().map(|(_, result)| result).collect()
    }
}

#[derive(Clone)]
pub struct LiveJobRunner {
    resolver: StreamResolver,
    recorder: FfmpegRecorder,
    events: Arc<dyn EventSink>,
}

impl LiveJobRunner {
    pub fn new(resolver: StreamResolver, recorder: FfmpegRecorder) -> Self {
        Self {
            resolver,
            recorder,
            events: noop_event_sink(),
        }
    }

    pub fn with_event_sink(
        resolver: StreamResolver,
        recorder: FfmpegRecorder,
        events: Arc<dyn EventSink>,
    ) -> Self {
        Self {
            resolver,
            recorder,
            events,
        }
    }
}

#[async_trait]
impl JobRunner for LiveJobRunner {
    async fn run_job(
        &self,
        request: RecordingRequest,
        cancellation: CancellationToken,
    ) -> JobResult {
        if cancellation.is_cancelled() {
            return JobResult::failure(request.room_url, "recording was cancelled");
        }
        self.events.emit(JobEvent::Resolving {
            room_url: request.room_url.clone(),
        });
        let room = match tokio::select! {
            _ = cancellation.cancelled() => {
                return JobResult::failure(request.room_url, "recording was cancelled");
            }
            resolved = self.resolver.resolve(&request.room_url) => resolved
        } {
            Ok(room) => room,
            Err(error) => {
                return JobResult::failure(request.room_url, error.safe_message());
            }
        };
        let selected = match room.select(request.quality.as_deref(), request.protocol) {
            Ok(selected) => selected,
            Err(error) => {
                let mut result = JobResult::failure(request.room_url, error.safe_message());
                result.room_id = Some(room.room_id);
                return result;
            }
        };

        self.events.emit(JobEvent::Resolved {
            room_url: request.room_url.clone(),
            room_id: room.room_id.clone(),
            selected: selected.summary(),
        });

        if cancellation.is_cancelled() {
            return JobResult::failure(request.room_url, "recording was cancelled");
        }

        self.recorder
            .record_selected_with_event_sink(
                request.room_url,
                room.room_id,
                selected,
                cancellation,
                self.events.clone(),
            )
            .await
    }
}
