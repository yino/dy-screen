//! 桌面进程中的真实 FFprobe、FFmpeg、VAD、ASR 和调度器装配。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use dy_screen::asr::{
    AsrBundleManifest, AsrEngine, AsrError, AsrErrorKind, AsrJobExecutor, AsrResourceResolver,
    EngineResult, FfmpegAudioPreparer, FfprobeMediaInspector, MediaInspector,
    RecordingActivityGate, ResolvedAsrResources, ResourcePreflight, SchedulerEvent,
    SchedulerEventSink, SchedulerJob, TextNormalizer, TranscriptionScheduler, VadConfig,
    WhisperCppEngine, WhisperCppVadEngine,
};
use tokio_util::sync::CancellationToken;

use crate::database::Database;

use super::processor::build_job_event;
use super::{
    AiCommandError, AiEnvironmentCheckView, AiEnvironmentDiagnostic, AiInputProcessor,
    AiInputStatus, AiJobController, AiJobPublisher, AiLifecycle, AiPreflight, AiProcessorError,
    AiProjectStatus, AiRecoveryReport, AiRepository, PreflightReport, RecognitionProfile,
};

const BUNDLED_MANIFEST: &str = include_str!("../../../resources/asr/manifest.json");

#[derive(Clone)]
pub struct LocalAsrEnvironment {
    resource_root: PathBuf,
    resources: Result<ResolvedAsrResources, AsrError>,
    manifest: Result<AsrBundleManifest, AsrError>,
}

impl LocalAsrEnvironment {
    pub fn load(resource_root: PathBuf) -> Self {
        let manifest_text =
            std::fs::read_to_string(resource_root.join("manifest.json")).map_err(|_| {
                AsrError::new(
                    AsrErrorKind::EnvironmentUnavailable,
                    "asr_manifest_missing",
                    "本地 ASR 资源清单缺失，请重新安装应用",
                    false,
                )
            });
        let manifest = manifest_text
            .as_deref()
            .map_err(Clone::clone)
            .and_then(AsrBundleManifest::from_json);
        let resources = manifest_text
            .as_deref()
            .map_err(Clone::clone)
            .and_then(|text| AsrResourceResolver::resolve(&resource_root, text));
        Self {
            resource_root,
            resources,
            manifest,
        }
    }

    pub fn resource_root(&self) -> &Path {
        &self.resource_root
    }

    pub fn resources(&self) -> Result<ResolvedAsrResources, AsrError> {
        self.resources.clone()
    }

    fn default_recognition_profile(&self) -> Result<RecognitionProfile, AiCommandError> {
        let manifest = self
            .manifest
            .clone()
            .or_else(|_| AsrBundleManifest::from_json(BUNDLED_MANIFEST))
            .map_err(command_asr_error)?;
        let vad = VadConfig::default();
        Ok(RecognitionProfile {
            engine_id: manifest.engine.id,
            engine_version: manifest.engine.version,
            model_id: manifest.model.logical_id,
            model_version: manifest.model.version,
            language_hint: Some("zh".to_owned()),
            vad_model_id: format!("{}@{}", manifest.vad.logical_id, manifest.vad.version),
            vad_threshold_millis: u32::from(vad.threshold_permille),
            vad_padding_ms: vad.speech_padding_ms,
            timestamp_policy: "segment".to_owned(),
            normalization_version: manifest.normalization.version,
            hotwords: Vec::new(),
        })
    }

    async fn diagnostic(&self) -> AiEnvironmentDiagnostic {
        let resources = match self.resources() {
            Ok(resources) => resources,
            Err(error) => {
                return AiEnvironmentDiagnostic {
                    ready: false,
                    platform: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
                    engine_id: self
                        .default_recognition_profile()
                        .map(|profile| profile.engine_id)
                        .unwrap_or_else(|_| "whisper.cpp".to_owned()),
                    engine_version: self
                        .default_recognition_profile()
                        .map(|profile| profile.engine_version)
                        .unwrap_or_default(),
                    model_id: self
                        .default_recognition_profile()
                        .map(|profile| profile.model_id)
                        .unwrap_or_default(),
                    model_version: self
                        .default_recognition_profile()
                        .map(|profile| profile.model_version)
                        .unwrap_or_default(),
                    checks: vec![AiEnvironmentCheckView {
                        code: error.code,
                        passed: false,
                        message: error.safe_message.clone(),
                    }],
                    message: error.safe_message,
                };
            }
        };
        let probe_resources = resources.clone();
        let report = tokio::task::spawn_blocking(move || {
            ResourcePreflight::native().diagnose(&probe_resources)
        })
        .await
        .ok()
        .and_then(Result::ok);
        match report {
            Some(report) => {
                let checks = report
                    .checks
                    .into_iter()
                    .map(|check| AiEnvironmentCheckView {
                        code: check.code,
                        passed: check.passed,
                        message: check.message,
                    })
                    .collect::<Vec<_>>();
                let ready = checks.iter().all(|check| check.passed);
                let message = checks
                    .iter()
                    .find(|check| !check.passed)
                    .map(|check| check.message.clone())
                    .unwrap_or_else(|| "本地 ASR 环境就绪，识别过程不会上传视频".to_owned());
                AiEnvironmentDiagnostic {
                    ready,
                    platform: report.platform,
                    engine_id: report.identity.engine_id,
                    engine_version: report.identity.engine_version,
                    model_id: report.identity.model_id,
                    model_version: report.identity.model_version,
                    checks,
                    message,
                }
            }
            None => AiEnvironmentDiagnostic {
                ready: false,
                platform: resources.platform,
                engine_id: resources.identity.engine_id,
                engine_version: resources.identity.engine_version,
                model_id: resources.identity.model_id,
                model_version: resources.identity.model_version,
                checks: vec![AiEnvironmentCheckView {
                    code: "asr_diagnostic_failed".to_owned(),
                    passed: false,
                    message: "本地 ASR 环境检查异常退出，请重新检测".to_owned(),
                }],
                message: "本地 ASR 环境检查异常退出，请重新检测".to_owned(),
            },
        }
    }
}

#[async_trait]
impl AiPreflight for LocalAsrEnvironment {
    async fn check(&self) -> Result<PreflightReport, AsrError> {
        let diagnostic = self.diagnostic().await;
        let passed = |codes: &[&str]| {
            codes.iter().all(|code| {
                diagnostic
                    .checks
                    .iter()
                    .find(|check| check.code == *code)
                    .is_some_and(|check| check.passed)
            })
        };
        Ok(PreflightReport {
            ready: diagnostic.ready,
            engine_id: diagnostic.engine_id,
            engine_version: diagnostic.engine_version,
            model_id: diagnostic.model_id,
            model_version: diagnostic.model_version,
            platform_supported: self.resources.is_ok(),
            sidecars_ready: passed(&[
                "whisper_sidecar",
                "vad_sidecar",
                "ffmpeg",
                "ffprobe",
                "engine_version",
            ]),
            models_ready: passed(&["asr_model", "vad_model", "normalization_mapping"]),
            memory_ready: passed(&["total_memory", "available_memory"]),
            disk_ready: passed(&["available_disk"]),
            message: diagnostic.message,
        })
    }
}

pub struct LocalAsrComponents {
    pub runtime: Arc<LocalAsrRuntime>,
    pub inspector: Arc<dyn MediaInspector>,
    pub preflight: Arc<dyn AiPreflight>,
}

#[derive(Clone)]
pub struct LocalAsrRuntime {
    repository: AiRepository,
    environment: Arc<LocalAsrEnvironment>,
    scheduler: Option<Arc<TranscriptionScheduler>>,
    lifecycle: AiLifecycle,
}

impl LocalAsrRuntime {
    #[allow(clippy::too_many_arguments)]
    pub fn build(
        database: Database,
        resource_root: PathBuf,
        fallback_ffprobe: PathBuf,
        temporary_root: PathBuf,
        publisher: Arc<dyn AiJobPublisher>,
    ) -> LocalAsrComponents {
        let repository = AiRepository::new(database.clone());
        let environment = Arc::new(LocalAsrEnvironment::load(resource_root));
        let resources = environment.resources();
        let inspector_path = resources
            .as_ref()
            .map(|resources| resources.ffprobe.clone())
            .unwrap_or(fallback_ffprobe);
        let inspector: Arc<dyn MediaInspector> = Arc::new(FfprobeMediaInspector::new(
            inspector_path,
            Duration::from_secs(10),
        ));
        let normalization_version = environment
            .default_recognition_profile()
            .map(|profile| profile.normalization_version)
            .unwrap_or_else(|_| "OpenCC ver.1.4.1".to_owned());
        let scheduler = resources.ok().and_then(|resources| {
            build_scheduler(
                database,
                repository.clone(),
                resources,
                temporary_root.clone(),
                publisher,
                inspector.clone(),
                &normalization_version,
            )
            .ok()
            .map(Arc::new)
        });
        let runtime = Arc::new(Self {
            repository: repository.clone(),
            environment: environment.clone(),
            scheduler,
            lifecycle: AiLifecycle::new(repository, temporary_root),
        });
        LocalAsrComponents {
            runtime,
            inspector,
            preflight: environment,
        }
    }

    pub fn recover_startup(&self) -> Result<AiRecoveryReport, AiCommandError> {
        self.lifecycle
            .recover_startup()
            .map_err(|error| AiCommandError::new("ai_recovery_failed", error.to_string(), true))
    }

    /// 启动恢复后从 SQLite 重建待处理队列；进程内 token 不跨重启持久化。
    pub async fn recover_startup_and_requeue(&self) -> Result<AiRecoveryReport, AiCommandError> {
        let report = self.recover_startup()?;
        let Some(scheduler) = &self.scheduler else {
            return Ok(report);
        };
        for project in self
            .repository
            .list_projects()
            .map_err(repository_command_error)?
        {
            if !matches!(
                project.status,
                AiProjectStatus::Queued | AiProjectStatus::Running
            ) {
                continue;
            }
            let detail = self
                .repository
                .get_project(project.id)
                .map_err(repository_command_error)?;
            for input in detail
                .inputs
                .into_iter()
                .filter(|input| input.status == AiInputStatus::Pending)
            {
                let _ = scheduler
                    .enqueue(SchedulerJob {
                        id: job_id(project.id, input.id),
                        project_id: project.id,
                        input_id: input.id,
                        generation: input.scheduler_generation,
                    })
                    .await;
            }
        }
        Ok(report)
    }

    pub async fn shutdown(&self) -> Result<AiRecoveryReport, AiCommandError> {
        if let Some(scheduler) = &self.scheduler {
            self.lifecycle
                .shutdown(scheduler)
                .await
                .map_err(|error| AiCommandError::new("ai_shutdown_failed", error.to_string(), true))
        } else {
            self.recover_startup()
        }
    }

    fn scheduler(&self) -> Result<&Arc<TranscriptionScheduler>, AiCommandError> {
        self.scheduler.as_ref().ok_or_else(|| {
            AiCommandError::new(
                "asr_environment_not_ready",
                "本地 ASR 环境未就绪，请重新检测或修复安装",
                true,
            )
        })
    }
}

#[async_trait]
impl AiJobController for LocalAsrRuntime {
    fn default_profile(&self) -> Result<RecognitionProfile, AiCommandError> {
        self.environment.default_recognition_profile()
    }

    async fn enqueue_project(&self, project_id: i64) -> Result<(), AiCommandError> {
        let scheduler = self.scheduler()?;
        let detail = self
            .repository
            .get_project(project_id)
            .map_err(repository_command_error)?;
        if !matches!(
            detail.project.status,
            AiProjectStatus::Queued | AiProjectStatus::Running
        ) {
            return Err(AiCommandError::new(
                "project_not_queued",
                "项目不处于可排队状态",
                false,
            ));
        }
        let jobs = detail
            .inputs
            .into_iter()
            .filter(|input| input.status == AiInputStatus::Pending)
            .collect::<Vec<_>>();
        if jobs.is_empty() {
            return Err(AiCommandError::new(
                "project_has_no_pending_input",
                "项目没有可执行的待处理视频",
                false,
            ));
        }
        for input in jobs {
            scheduler
                .enqueue(SchedulerJob {
                    id: job_id(project_id, input.id),
                    project_id,
                    input_id: input.id,
                    generation: input.scheduler_generation,
                })
                .await
                .map_err(command_asr_error)?;
        }
        Ok(())
    }

    async fn cancel_project(&self, project_id: i64) -> Result<(), AiCommandError> {
        let is_deleting = self
            .repository
            .get_project(project_id)
            .map(|detail| detail.project.status == AiProjectStatus::Deleting)
            .map_err(repository_command_error)?;
        if let Some(scheduler) = &self.scheduler {
            scheduler
                .cancel_project_and_wait(project_id, Duration::from_secs(15))
                .await
                .map_err(command_asr_error)?;
        }
        if is_deleting {
            return Ok(());
        }
        self.repository
            .cancel_project(project_id)
            .map(|_| ())
            .map_err(repository_command_error)
    }

    async fn retry_input(&self, input_id: i64) -> Result<(), AiCommandError> {
        let scheduler = self.scheduler()?;
        let input = self
            .repository
            .prepare_input_retry(input_id)
            .map_err(repository_command_error)?;
        scheduler
            .enqueue(SchedulerJob {
                id: job_id(input.project_id, input.id),
                project_id: input.project_id,
                input_id: input.id,
                generation: input.scheduler_generation,
            })
            .await
            .map_err(command_asr_error)
    }

    async fn diagnose(&self) -> Result<AiEnvironmentDiagnostic, AiCommandError> {
        Ok(self.environment.diagnostic().await)
    }

    async fn promote_next_input(&self, input_id: i64) -> Result<(), AiCommandError> {
        let scheduler = self.scheduler()?;
        let input = self
            .repository
            .get_input(input_id)
            .map_err(repository_command_error)?;
        if input.status != AiInputStatus::Pending {
            return Err(AiCommandError::new(
                "scheduler_job_not_pending",
                "只能调整待处理的视频",
                false,
            ));
        }
        scheduler
            .promote_next(&job_id(input.project_id, input.id))
            .await
            .map(|_| ())
            .map_err(command_asr_error)
    }

    async fn preempt_with_input(
        &self,
        input_id: i64,
        confirmed: bool,
    ) -> Result<(), AiCommandError> {
        if !confirmed {
            return Err(AiCommandError::new(
                "confirmation_required",
                "立即切换必须二次确认",
                false,
            ));
        }
        let scheduler = self.scheduler()?;
        let input = self
            .repository
            .get_input(input_id)
            .map_err(repository_command_error)?;
        if input.status != AiInputStatus::Pending {
            return Err(AiCommandError::new(
                "scheduler_job_not_pending",
                "只能切换到待处理的视频",
                false,
            ));
        }
        scheduler
            .preempt_with(&job_id(input.project_id, input.id))
            .await
            .map(|_| ())
            .map_err(command_asr_error)
    }
}

struct ProcessorExecutor {
    processor: AiInputProcessor,
}

#[async_trait]
impl AsrJobExecutor for ProcessorExecutor {
    async fn execute(
        &self,
        job: SchedulerJob,
        cancellation: CancellationToken,
    ) -> EngineResult<()> {
        self.processor
            .process_input_with_generation(
                job.project_id,
                job.input_id,
                job.generation,
                cancellation,
            )
            .await
            .map(|_| ())
            .map_err(|error| match error {
                AiProcessorError::Asr(error) => error,
                AiProcessorError::Repository(_) => AsrError::new(
                    AsrErrorKind::Internal,
                    "ai_repository_error",
                    "无法保存语音识别任务状态",
                    true,
                ),
            })
    }
}

struct DatabaseRecordingGate {
    database: Database,
}

#[async_trait]
impl RecordingActivityGate for DatabaseRecordingGate {
    fn is_recording_active(&self) -> bool {
        self.database
            .get_settings()
            .map(|settings| !settings.asr_during_recording)
            .unwrap_or(true)
            && self
                .database
                .dashboard()
                .map(|dashboard| dashboard.active_recordings > 0)
                .unwrap_or(true)
    }

    async fn wait_until_idle(&self, cancellation: CancellationToken) -> EngineResult<()> {
        loop {
            if cancellation.is_cancelled() {
                return Err(AsrError::cancelled());
            }
            if !self.is_recording_active() {
                return Ok(());
            }
            tokio::select! {
                _ = cancellation.cancelled() => return Err(AsrError::cancelled()),
                _ = tokio::time::sleep(Duration::from_millis(500)) => {}
            }
        }
    }
}

struct RuntimeSchedulerEvents {
    repository: AiRepository,
    publisher: Arc<dyn AiJobPublisher>,
}

impl SchedulerEventSink for RuntimeSchedulerEvents {
    fn publish(&self, event: SchedulerEvent) {
        let (job, stage, message) = match event {
            SchedulerEvent::Queued(job) => (job, "queued", "已加入本地语音识别队列"),
            SchedulerEvent::WaitingForRecording(job) => {
                (job, "waiting_for_recording", "录制优先模式：正在等待录制结束")
            }
            SchedulerEvent::Started(job) => {
                if self
                    .repository
                    .get_project(job.project_id)
                    .is_ok_and(|detail| detail.project.status == AiProjectStatus::Queued)
                {
                    let _ = self
                        .repository
                        .transition_project(job.project_id, AiProjectStatus::Running);
                }
                (job, "started", "开始处理视频")
            }
            SchedulerEvent::Finished(job) => (job, "finished", "视频处理完成"),
            SchedulerEvent::Failed { job, code } => {
                mark_scheduler_terminal(
                    &self.repository,
                    &job,
                    AiInputStatus::Failed,
                    &code,
                    "语音识别调度失败",
                );
                (job, "failed", "视频处理失败")
            }
            SchedulerEvent::Cancelled(job) => {
                mark_scheduler_terminal(
                    &self.repository,
                    &job,
                    AiInputStatus::Cancelled,
                    "asr_cancelled",
                    "语音识别已取消",
                );
                (job, "cancelled", "视频处理已取消")
            }
            SchedulerEvent::Preempted { requeued, .. } => (requeued, "requeued", "视频已重新排队"),
            SchedulerEvent::QueueChanged(_) => return,
        };
        let progress = self
            .repository
            .get_input(job.input_id)
            .map(|input| input.progress_percent)
            .unwrap_or(0);
        if let Some(snapshot) = build_job_event(
            &self.repository,
            job.project_id,
            job.input_id,
            stage,
            progress,
            message,
        ) {
            self.publisher.publish(snapshot);
        }
    }
}

fn build_scheduler(
    database: Database,
    repository: AiRepository,
    resources: ResolvedAsrResources,
    temporary_root: PathBuf,
    publisher: Arc<dyn AiJobPublisher>,
    inspector: Arc<dyn MediaInspector>,
    normalization_version: &str,
) -> Result<TranscriptionScheduler, AsrError> {
    let mapping = std::fs::read_to_string(&resources.normalization_mapping).map_err(|_| {
        AsrError::new(
            AsrErrorKind::EnvironmentUnavailable,
            "normalization_mapping_missing",
            "中文规范化资源缺失，请重新安装应用",
            false,
        )
    })?;
    let vad_config = VadConfig::default();
    let engine: Arc<dyn AsrEngine> = Arc::new(WhisperCppEngine::new(
        resources.clone(),
        ResourcePreflight::native(),
        vad_config.clone(),
    ));
    let processor = AiInputProcessor::new(
        repository.clone(),
        inspector,
        Arc::new(FfmpegAudioPreparer::new(resources.ffmpeg.clone())),
        // Apple Silicon 上锁定版本的独立 VAD sidecar 使用 GPU 会崩溃，VAD 固定 CPU。
        Arc::new(WhisperCppVadEngine::new(
            resources.vad_sidecar.clone(),
            resources.vad_model.clone(),
            resources.maximum_threads,
            false,
        )),
        engine,
        TextNormalizer::with_opencc_characters(normalization_version, &mapping),
        vad_config,
        temporary_root,
        publisher.clone(),
    );
    Ok(TranscriptionScheduler::start(
        Arc::new(ProcessorExecutor { processor }),
        Arc::new(DatabaseRecordingGate { database }),
        Arc::new(RuntimeSchedulerEvents {
            repository,
            publisher,
        }),
        256,
    ))
}

fn mark_scheduler_terminal(
    repository: &AiRepository,
    job: &SchedulerJob,
    status: AiInputStatus,
    code: &str,
    message: &str,
) {
    if let Ok(input) = repository.get_input(job.input_id)
        && !matches!(
            input.status,
            AiInputStatus::Completed
                | AiInputStatus::Skipped
                | AiInputStatus::Cancelled
                | AiInputStatus::Failed
        )
    {
        let _ = repository.transition_input(job.input_id, status, Some((code, message)));
    }
    let _ = repository.recompute_project_progress(job.project_id);
}

#[cfg(test)]
mod tests {
    use super::DatabaseRecordingGate;
    use crate::database::Database;
    use crate::domain::NewStreamer;
    use dy_screen::asr::RecordingActivityGate;

    #[test]
    fn recording_gate_allows_parallel_asr_by_default_and_supports_priority_mode() {
        let database = Database::open_in_memory().expect("打开测试数据库");
        database.migrate().expect("完成测试数据库迁移");
        let streamer = database
            .add_streamer(&NewStreamer::room("并行测试主播", "parallel-1", "parallel-room", true))
            .expect("创建测试主播");
        database
            .update_streamer_status(streamer.id, "live", "recording", None)
            .expect("设置录制状态");

        let gate = DatabaseRecordingGate {
            database: database.clone(),
        };
        assert!(!gate.is_recording_active());

        let mut settings = database.get_settings().expect("读取设置");
        settings.asr_during_recording = false;
        database.save_settings(&settings).expect("保存录制优先设置");
        assert!(gate.is_recording_active());
    }
}

fn job_id(project_id: i64, input_id: i64) -> String {
    format!("ai-project-{project_id}-input-{input_id}")
}

fn command_asr_error(error: AsrError) -> AiCommandError {
    AiCommandError::new(error.code, error.safe_message, error.retryable)
}

fn repository_command_error(error: super::AiRepositoryError) -> AiCommandError {
    AiCommandError::new("ai_repository_error", error.to_string(), true)
}
