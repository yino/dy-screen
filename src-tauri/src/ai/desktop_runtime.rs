//! 桌面进程中的真实 FFprobe、FFmpeg、VAD、ASR 和调度器装配。

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use dy_screen::asr::{
    AsrBundleManifest, AsrEngine, AsrError, AsrErrorKind, AsrJobExecutor, AsrResourceResolver,
    CloudFirstAsrEngine, DashScopeAsrClient, DashScopeAsrConfig, EngineResult, FfmpegAudioPreparer,
    FfprobeMediaInspector, MediaInspector, RecordingActivityGate, ResolvedAsrResources,
    ResourcePreflight, SchedulerEvent, SchedulerEventSink, SchedulerJob,
    SharedTemporaryAudioPublisher, TextNormalizer, TranscriptionScheduler,
    UnavailableTemporaryAudioPublisher, UpstreamFileUploadPublisher, VadConfig, WhisperCppEngine,
    WhisperCppVadEngine,
};
use dy_screen::runtime_resources::RuntimeManifest;
use tokio_util::sync::CancellationToken;

use crate::activation::ActivationService;
use crate::api::ApiError;
use crate::database::Database;

use super::processor::build_job_event;
use super::{
    AiCommandError, AiEnvironmentCheckView, AiEnvironmentDiagnostic, AiInputProcessor,
    AiInputStatus, AiJobController, AiJobPublisher, AiLifecycle, AiPreflight, AiProcessorError,
    AiProjectStatus, AiRecoveryReport, AiRepository, AiRuntimeComponentDiagnostic,
    AiRuntimeResourceDiagnostic, PreflightReport, RecognitionProfile,
};

const BUNDLED_MANIFEST: &str = include_str!("../../../resources/asr/manifest.json");
const PROJECT_CANCEL_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone)]
struct ReloadableMediaInspector {
    current: Arc<Mutex<Arc<dyn MediaInspector>>>,
}

#[async_trait]
impl MediaInspector for ReloadableMediaInspector {
    async fn inspect(
        &self,
        source: &dy_screen::asr::FrozenMediaSource,
        cancellation: CancellationToken,
    ) -> EngineResult<dy_screen::asr::MediaInspection> {
        let inspector = self
            .current
            .lock()
            .map(|inspector| inspector.clone())
            .map_err(|_| {
                AsrError::new(
                    AsrErrorKind::Internal,
                    "media_inspector_unavailable",
                    "本地媒体探测器状态不可用",
                    true,
                )
            })?;
        inspector.inspect(source, cancellation).await
    }
}

#[derive(Clone)]
pub struct LocalAsrEnvironment {
    resource_root: PathBuf,
    active_resource_root: Arc<Mutex<Option<PathBuf>>>,
    /// 百炼直连所需的 AI 授权是否已配置；由运行时按实际授权状态维护。
    cloud_configured: Arc<std::sync::atomic::AtomicBool>,
}

impl LocalAsrEnvironment {
    pub fn load(resource_root: PathBuf) -> Self {
        Self {
            resource_root,
            active_resource_root: Arc::new(Mutex::new(None)),
            cloud_configured: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    /// 记录百炼直连凭据是否就绪，供诊断反映真实可用性。
    pub fn set_cloud_configured(&self, configured: bool) {
        self.cloud_configured
            .store(configured, std::sync::atomic::Ordering::Relaxed);
    }

    /// 百炼直连当前是否具备凭据条件。
    pub fn cloud_configured(&self) -> bool {
        self.cloud_configured
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn resource_root(&self) -> &Path {
        &self.resource_root
    }

    pub fn resources(&self) -> Result<ResolvedAsrResources, AsrError> {
        let root = self.active_root();
        let manifest_text = load_asr_manifest_text(&root)?;
        AsrResourceResolver::resolve(&root, &manifest_text)
    }

    fn active_root(&self) -> PathBuf {
        self.active_resource_root
            .lock()
            .ok()
            .and_then(|root| root.clone())
            .unwrap_or_else(|| self.resource_root.clone())
    }

    pub fn activate_resource_root(&self, root: PathBuf) -> Result<(), AsrError> {
        let manifest_text = load_asr_manifest_text(&root)?;
        let _ = AsrResourceResolver::resolve(&root, &manifest_text)?;
        let mut active = self.active_resource_root.lock().map_err(|_| {
            AsrError::new(
                AsrErrorKind::Internal,
                "asr_resource_state_unavailable",
                "本地 ASR 资源状态不可用",
                true,
            )
        })?;
        *active = Some(root);
        Ok(())
    }

    fn runtime_diagnostic(&self) -> Option<AiRuntimeResourceDiagnostic> {
        let text =
            std::fs::read_to_string(self.active_root().join("runtime-manifest.json")).ok()?;
        let manifest = RuntimeManifest::from_json(&text).ok()?;
        Some(AiRuntimeResourceDiagnostic {
            bundle_version: manifest.bundle_version.clone(),
            manifest_sha256: manifest.manifest_sha256().ok()?,
            signature_valid: manifest.verify_signature().is_ok(),
            components: manifest
                .components
                .iter()
                .map(|component| AiRuntimeComponentDiagnostic {
                    id: component.id.clone(),
                    version: component.version.clone(),
                    required: component.required,
                    file_count: component.files.len(),
                    size_bytes: component.files.iter().map(|file| file.size_bytes).sum(),
                })
                .collect(),
        })
    }

    /// 百炼直连、可选本地识别与音频准备三项独立诊断。
    ///
    /// 只输出分类与布尔可用性：不含短期 token、URL、签名地址、本地路径或上游任务 ID。
    fn cloud_checks(&self, local_ready: bool) -> Vec<AiEnvironmentCheckView> {
        let cloud_ready = self.cloud_configured();
        vec![
            AiEnvironmentCheckView {
                code: "cloud_transcription".to_owned(),
                passed: cloud_ready,
                message: if cloud_ready {
                    "百炼直连转写可用".to_owned()
                } else {
                    "缺少可用的 AI 授权，百炼直连转写不可用".to_owned()
                },
            },
            AiEnvironmentCheckView {
                code: "cloud_audio_publisher".to_owned(),
                passed: cloud_ready,
                message: if cloud_ready {
                    "云端音频直传可用".to_owned()
                } else {
                    "云端音频直传不可用".to_owned()
                },
            },
            AiEnvironmentCheckView {
                code: "local_transcription".to_owned(),
                passed: local_ready,
                message: if local_ready {
                    "本地识别资源就绪（用户显式选择时可用）".to_owned()
                } else {
                    "本地识别资源未就绪；不影响百炼直连转写".to_owned()
                },
            },
        ]
    }

    fn default_recognition_profile(&self) -> Result<RecognitionProfile, AiCommandError> {
        let manifest_text = load_asr_manifest_text(&self.active_root())
            .or_else(|_| Ok(BUNDLED_MANIFEST.to_owned()))
            .map_err(command_asr_error)?;
        let manifest = AsrBundleManifest::from_json(&manifest_text).map_err(command_asr_error)?;
        let vad = VadConfig::default();
        // 默认识别路径是百炼直连，因此默认配置的引擎身份必须是**云端引擎**的真实
        // 身份，而不是随包本地 manifest 的身份——否则缓存指纹与实际执行引擎不符，
        // 云端与本地两条路径会互相命中对方的产物。
        // VAD 参数与规范化版本仍取自随包 manifest：它们同样参与指纹。
        let dashscope = DashScopeAsrConfig::default();
        Ok(RecognitionProfile {
            engine_id: CLOUD_ENGINE_ID.to_owned(),
            engine_version: dy_screen::asr::CLOUD_ENGINE_VERSION.to_owned(),
            model_id: dashscope.model.clone(),
            model_version: dashscope.model,
            language_hint: Some("zh".to_owned()),
            vad_model_id: format!("{}@{}", manifest.vad.logical_id, manifest.vad.version),
            vad_threshold_millis: u32::from(vad.threshold_permille),
            vad_padding_ms: vad.speech_padding_ms,
            timestamp_policy: "segment".to_owned(),
            normalization_version: manifest.normalization.version,
            hotwords: Vec::new(),
            provider: Some("cloud".to_owned()),
            // 分块策略在实施分块提交后回填；未分块时不写入，以保持与旧配置一致。
            chunk_strategy: None,
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
                    cloud_asr_available: self.cloud_configured(),
                    runtime: self.runtime_diagnostic(),
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
                // 三项独立诊断：百炼直连转写、可选本地识别与音频准备。
                // 注意：ready 仍只表示本地资源就绪，云端可用性由
                // cloud_asr_available 与 cloud_checks 单独表达，避免资源依赖型
                // 流程在只有云端可用时被误判为就绪。
                let mut checks = checks;
                checks.extend(self.cloud_checks(report.ready));
                AiEnvironmentDiagnostic {
                    ready,
                    platform: report.platform,
                    engine_id: report.identity.engine_id,
                    engine_version: report.identity.engine_version,
                    model_id: report.identity.model_id,
                    model_version: report.identity.model_version,
                    checks,
                    message,
                    cloud_asr_available: self.cloud_configured(),
                    runtime: self.runtime_diagnostic(),
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
                cloud_asr_available: self.cloud_configured(),
                runtime: self.runtime_diagnostic(),
            },
        }
    }
}

fn load_asr_manifest_text(resource_root: &Path) -> Result<String, AsrError> {
    if let Ok(text) = std::fs::read_to_string(resource_root.join("manifest.json")) {
        return Ok(text);
    }
    let runtime_text = std::fs::read_to_string(resource_root.join("runtime-manifest.json"))
        .map_err(|_| {
            AsrError::new(
                AsrErrorKind::EnvironmentUnavailable,
                "asr_manifest_missing",
                "本地 ASR 资源清单缺失，请重新安装应用",
                false,
            )
        })?;
    let runtime = RuntimeManifest::from_json(&runtime_text).map_err(|_| {
        AsrError::new(
            AsrErrorKind::EnvironmentUnavailable,
            "asr_manifest_invalid",
            "本地运行资源清单无效，请重新安装应用",
            false,
        )
    })?;
    let platform = runtime.for_current_platform().map_err(|_| {
        AsrError::new(
            AsrErrorKind::UnsupportedPlatform,
            "unsupported_asr_platform",
            "当前系统不支持本地语音识别",
            false,
        )
    })?;
    let component_file = |id: &str| -> Result<(&str, u64, &str), AsrError> {
        runtime
            .components
            .iter()
            .find(|component| component.id == id)
            .and_then(|component| {
                component
                    .files
                    .first()
                    .map(|file| (file.path.as_str(), file.size_bytes, file.sha256.as_str()))
            })
            .ok_or_else(|| {
                AsrError::new(
                    AsrErrorKind::EnvironmentUnavailable,
                    "asr_manifest_invalid",
                    "运行资源缺少 ASR 组件",
                    false,
                )
            })
    };
    let (model_file, model_size, model_sha) = component_file("asr.model")?;
    let (vad_model_file, vad_size, vad_sha) = component_file("asr.vad-model")?;
    let (normalization_file, normalization_size, normalization_sha) =
        component_file("asr.normalization")?;
    let (_, _, engine_version) = runtime
        .components
        .iter()
        .find(|component| component.id == "asr.whisper")
        .map(|component| {
            (
                component.id.as_str(),
                component.version.as_str(),
                component.version.as_str(),
            )
        })
        .ok_or_else(|| {
            AsrError::new(
                AsrErrorKind::EnvironmentUnavailable,
                "asr_manifest_invalid",
                "运行资源缺少 Whisper 组件",
                false,
            )
        })?;
    let whisper = component_file("asr.whisper")?;
    let vad_sidecar = component_file("asr.vad-sidecar")?;
    let ffmpeg = component_file("media.ffmpeg")?;
    let ffprobe = component_file("media.ffprobe")?;
    let mut integrity = vec![
        serde_json::json!({"file": whisper.0, "sizeBytes": whisper.1, "sha256": whisper.2}),
        serde_json::json!({"file": vad_sidecar.0, "sizeBytes": vad_sidecar.1, "sha256": vad_sidecar.2}),
        serde_json::json!({"file": ffmpeg.0, "sizeBytes": ffmpeg.1, "sha256": ffmpeg.2}),
        serde_json::json!({"file": ffprobe.0, "sizeBytes": ffprobe.1, "sha256": ffprobe.2}),
    ];
    for component in runtime
        .components
        .iter()
        .filter(|component| component.id.starts_with("platform.library."))
    {
        if let Some(file) = component.files.first() {
            integrity.push(serde_json::json!({"file": file.path, "sizeBytes": file.size_bytes, "sha256": file.sha256}));
        }
    }
    let legacy = serde_json::json!({
        "schemaVersion": 1,
        "bundleVersion": runtime.bundle_version,
        "engine": {"id": "whisper.cpp", "version": engine_version, "sourceCommit": "runtime-pack"},
        "model": {"logicalId": "whisper-small-multilingual-q5_1", "version": "runtime-pack", "file": model_file, "sizeBytes": model_size, "sha256": model_sha, "source": "runtime-pack", "license": "MIT"},
        "vad": {"logicalId": "silero-vad", "version": "runtime-pack", "file": vad_model_file, "sizeBytes": vad_size, "sha256": vad_sha, "source": "runtime-pack", "license": "MIT"},
        "normalization": {"logicalId": "opencc", "version": "runtime-pack", "file": normalization_file, "sizeBytes": normalization_size, "sha256": normalization_sha, "source": "runtime-pack", "license": "Apache-2.0"},
        "licenseFiles": runtime.licenses.iter().map(|license| license.path.clone()).collect::<Vec<_>>(),
        "platforms": [{"os": platform.os, "arch": platform.arch, "accelerator": if platform.os == "macos" { "metal" } else { "cpu" }, "sidecar": whisper.0, "vadSidecar": vad_sidecar.0, "ffmpeg": ffmpeg.0, "ffprobe": ffprobe.0, "libraries": runtime.components.iter().filter(|component| component.id.starts_with("platform.library.")).filter_map(|component| component.files.first().map(|file| file.path.clone())).collect::<Vec<_>>(), "minimumMemoryBytes": platform.minimum_memory_bytes, "minimumFreeDiskBytes": platform.minimum_free_disk_bytes, "maximumThreads": 4, "minimumCpuFeatures": [], "runtime": null, "runtimeFile": null, "resourceIntegrity": integrity}]
    });
    serde_json::to_string_pretty(&legacy).map_err(|_| {
        AsrError::new(
            AsrErrorKind::EnvironmentUnavailable,
            "asr_manifest_invalid",
            "无法生成兼容的 ASR 资源清单",
            false,
        )
    })
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
            platform_supported: self.resources().is_ok(),
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
    scheduler: Arc<Mutex<Option<Arc<TranscriptionScheduler>>>>,
    database: Database,
    temporary_root: PathBuf,
    publisher: Arc<dyn AiJobPublisher>,
    token_provider: Option<Arc<dyn dy_screen::asr::TemporaryAiTokenProvider>>,
    inspector_switch: Arc<Mutex<Arc<dyn MediaInspector>>>,
    lifecycle: AiLifecycle,
}

struct ActivationTokenProvider {
    activation: ActivationService,
}

/// 按工程冻结的识别配置在两条一等公民路径之间选择引擎。
///
/// 默认路径是百炼直连；只有工程识别配置显式指向本地引擎时才使用本地
/// `whisper.cpp`。这里不存在"云端失败后自动改用本地"的隐式回退。
struct ProjectSelectableAsrEngine {
    cloud: Arc<dyn AsrEngine>,
    local: Arc<dyn AsrEngine>,
}

/// 云端引擎在识别配置中的稳定标识。
const CLOUD_ENGINE_ID: &str = "dashscope-filetrans";

impl ProjectSelectableAsrEngine {
    fn select(&self, request: &dy_screen::asr::AsrRequest) -> Arc<dyn AsrEngine> {
        // 默认路径是百炼直连；只有显式选择本地引擎时才走本地。
        // 未知取值同样走默认路径，而不是静默落到本地。
        if request.engine_id.as_deref() == Some(crate::ai::processor::LOCAL_ENGINE_ID) {
            return self.local.clone();
        }
        self.cloud.clone()
    }
}

#[async_trait]
impl AsrEngine for ProjectSelectableAsrEngine {
    fn identity(&self) -> dy_screen::asr::AsrEngineIdentity {
        // 默认路径是云端；实际执行身份以 engine_id 显式选择为准。
        self.cloud.identity()
    }

    fn capabilities(&self) -> dy_screen::asr::AsrCapabilities {
        self.cloud.capabilities()
    }

    async fn diagnose(&self) -> EngineResult<dy_screen::asr::AsrEnvironmentReport> {
        self.cloud.diagnose().await
    }

    async fn transcribe(
        &self,
        request: dy_screen::asr::AsrRequest,
        progress: Arc<dyn dy_screen::asr::AsrProgressSink>,
        cancellation: CancellationToken,
    ) -> EngineResult<dy_screen::asr::AsrResult> {
        self.select(&request)
            .transcribe(request, progress, cancellation)
            .await
    }
}

#[cfg(test)]
mod engine_selection_tests {
    use super::*;
    use crate::ai::processor::LOCAL_ENGINE_ID;
    use dy_screen::asr::{
        AsrCapabilities, AsrEngineIdentity, AsrEnvironmentReport, AsrProgressSink, AsrRequest,
        AsrResult, AsrSegment, TimestampPolicy,
    };
    use std::sync::atomic::{AtomicU32, Ordering};

    /// 云端替身：按需产出瞬时故障或取消。
    struct FailingCloud {
        error: AsrError,
    }

    #[async_trait]
    impl AsrEngine for FailingCloud {
        fn identity(&self) -> AsrEngineIdentity {
            AsrEngineIdentity {
                engine_id: CLOUD_ENGINE_ID.into(),
                engine_version: "1".into(),
                model_id: "qwen3-asr-flash-filetrans".into(),
                model_version: "qwen3-asr-flash-filetrans".into(),
            }
        }
        fn capabilities(&self) -> AsrCapabilities {
            AsrCapabilities {
                timestamp_policies: vec![TimestampPolicy::Segment],
                supports_language_hint: true,
                supports_hotwords: true,
                confidence_kind: None,
                maximum_threads: 4,
            }
        }
        async fn diagnose(&self) -> EngineResult<AsrEnvironmentReport> {
            Ok(AsrEnvironmentReport {
                ready: true,
                platform: "test".into(),
                identity: self.identity(),
                checks: Vec::new(),
            })
        }
        async fn transcribe(
            &self,
            _request: AsrRequest,
            _progress: Arc<dyn AsrProgressSink>,
            _cancellation: CancellationToken,
        ) -> EngineResult<AsrResult> {
            Err(self.error.clone())
        }
    }

    /// 本地替身：被调用即失败，用于断言"隐式回退不存在"。
    struct ForbiddenLocal {
        calls: Arc<AtomicU32>,
    }

    #[async_trait]
    impl AsrEngine for ForbiddenLocal {
        fn identity(&self) -> AsrEngineIdentity {
            AsrEngineIdentity {
                engine_id: LOCAL_ENGINE_ID.into(),
                engine_version: "1".into(),
                model_id: "ggml-base".into(),
                model_version: "1".into(),
            }
        }
        fn capabilities(&self) -> AsrCapabilities {
            AsrCapabilities {
                timestamp_policies: vec![TimestampPolicy::Segment],
                supports_language_hint: true,
                supports_hotwords: true,
                confidence_kind: None,
                maximum_threads: 4,
            }
        }
        async fn diagnose(&self) -> EngineResult<AsrEnvironmentReport> {
            Ok(AsrEnvironmentReport {
                ready: true,
                platform: "test".into(),
                identity: self.identity(),
                checks: Vec::new(),
            })
        }
        async fn transcribe(
            &self,
            _request: AsrRequest,
            _progress: Arc<dyn AsrProgressSink>,
            _cancellation: CancellationToken,
        ) -> EngineResult<AsrResult> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(AsrResult {
                request_id: "test".into(),
                identity: self.identity(),
                detected_language: None,
                audio_duration_ms: 1,
                provider: "local".into(),
                fallback_reason: None,
                segments: vec![AsrSegment {
                    start_ms: 0,
                    end_ms: 1,
                    text: "本地".into(),
                    confidence: None,
                }],
                warnings: Vec::new(),
            })
        }
    }

    fn request(engine_id: Option<&str>) -> AsrRequest {
        use dy_screen::asr::{PreparedAudio, PreparedAudioFormat};
        AsrRequest {
            request_id: "req-1".into(),
            audio: PreparedAudio {
                path: std::path::PathBuf::from("/tmp/a.wav"),
                format: PreparedAudioFormat::PcmS16LeWav,
                sample_rate_hz: 16_000,
                channels: 1,
                duration_ms: 1_000,
            },
            language_hint: Some("zh".into()),
            hotwords: Vec::new(),
            speech_regions: Vec::new(),
            timestamp_policy: TimestampPolicy::Segment,
            max_threads: 4,
            engine_id: engine_id.map(str::to_owned),
        }
    }

    async fn run(error: AsrError, engine_id: Option<&str>) -> (EngineResult<AsrResult>, u32) {
        let local_calls = Arc::new(AtomicU32::new(0));
        let engine = ProjectSelectableAsrEngine {
            cloud: Arc::new(FailingCloud { error }),
            local: Arc::new(ForbiddenLocal {
                calls: local_calls.clone(),
            }),
        };
        let progress: Arc<dyn AsrProgressSink> = Arc::new(NoopProgress);
        let result = engine
            .transcribe(request(engine_id), progress, CancellationToken::new())
            .await;
        (result, local_calls.load(Ordering::SeqCst))
    }

    struct NoopProgress;

    impl AsrProgressSink for NoopProgress {
        fn publish(&self, _event: dy_screen::asr::AsrProgressEvent) {}
    }

    /// 任务 8.3：瞬时故障在有界重试后明确失败，且**不调用本地引擎**。
    #[tokio::test]
    async fn transient_failure_never_falls_back_to_local() {
        let (result, local_calls) = run(
            AsrError::new(
                AsrErrorKind::ProcessFailed,
                "cloud_upload_service_unavailable",
                "瞬时故障",
                true,
            ),
            Some(CLOUD_ENGINE_ID),
        )
        .await;
        let error = result.expect_err("必须明确失败");
        assert_eq!(error.code, "cloud_upload_service_unavailable");
        assert_eq!(
            local_calls, 0,
            "瞬时故障不得触发本地识别（隐式回退必须不存在）"
        );
        assert_eq!(
            dy_screen::asr::classify_cloud_failure(&error),
            dy_screen::asr::CloudFailureBranch::Transient
        );
    }

    /// 任务 8.4：取消路径不得触发本地识别。
    #[tokio::test]
    async fn cancellation_never_touches_local_engine() {
        let (result, local_calls) = run(AsrError::cancelled(), Some(CLOUD_ENGINE_ID)).await;
        let error = result.expect_err("取消必须终止");
        assert_eq!(local_calls, 0, "取消不得静默变成一次本地识别");
        assert_eq!(
            dy_screen::asr::classify_cloud_failure(&error),
            dy_screen::asr::CloudFailureBranch::Cancelled
        );
    }

    /// 任务 8.6 的反向断言：只有显式选择本地引擎时才走本地路径。
    #[tokio::test]
    async fn explicit_local_selection_uses_local_engine() {
        let (result, local_calls) = run(
            AsrError::new(
                AsrErrorKind::ProcessFailed,
                "cloud_upload_service_unavailable",
                "瞬时故障",
                true,
            ),
            Some(LOCAL_ENGINE_ID),
        )
        .await;
        let value = result.expect("本地路径必须成功");
        assert_eq!(local_calls, 1, "显式选择本地识别时必须调用本地引擎恰好一次");
        assert_eq!(value.provider, "local");
    }

    /// 未显式选择时默认走云端（百炼直连）。
    #[tokio::test]
    async fn default_path_is_cloud() {
        let (result, local_calls) = run(
            AsrError::new(
                AsrErrorKind::ProcessFailed,
                "cloud_upload_service_unavailable",
                "瞬时故障",
                true,
            ),
            None,
        )
        .await;
        assert!(result.is_err(), "默认路径必须走云端并如实失败");
        assert_eq!(local_calls, 0, "默认路径不得落到本地");
    }
}

#[async_trait]
impl dy_screen::asr::TemporaryAiTokenProvider for ActivationTokenProvider {
    fn is_configured(&self) -> bool {
        self.activation.credentials().is_some()
    }
    async fn temporary_token(&self) -> EngineResult<String> {
        let (device_id, activate_code) = self.activation.credentials().ok_or_else(|| {
            AsrError::new(
                AsrErrorKind::EnvironmentUnavailable,
                "ai_authorization_unavailable",
                "当前没有有效的 AI 授权",
                false,
            )
        })?;
        self.activation
            .api()
            .ai_llm_ak(&device_id, &activate_code)
            .await
            .map(|value| value.ak)
            .map_err(|error| {
                let retryable = matches!(error, ApiError::Transport | ApiError::Timeout);
                AsrError::new(
                    AsrErrorKind::ProcessFailed,
                    format!("ai_token_{}", error.category()),
                    error.safe_message(),
                    retryable,
                )
            })
    }
}

impl LocalAsrRuntime {
    #[allow(clippy::too_many_arguments)]
    pub fn build_with_activation(
        database: Database,
        resource_root: PathBuf,
        fallback_ffprobe: PathBuf,
        temporary_root: PathBuf,
        publisher: Arc<dyn AiJobPublisher>,
        activation: ActivationService,
    ) -> LocalAsrComponents {
        Self::build_internal(
            database,
            resource_root,
            fallback_ffprobe,
            temporary_root,
            publisher,
            Some(Arc::new(ActivationTokenProvider { activation })),
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn build(
        database: Database,
        resource_root: PathBuf,
        fallback_ffprobe: PathBuf,
        temporary_root: PathBuf,
        publisher: Arc<dyn AiJobPublisher>,
    ) -> LocalAsrComponents {
        Self::build_internal(
            database,
            resource_root,
            fallback_ffprobe,
            temporary_root,
            publisher,
            None,
        )
    }

    fn build_internal(
        database: Database,
        resource_root: PathBuf,
        fallback_ffprobe: PathBuf,
        temporary_root: PathBuf,
        publisher: Arc<dyn AiJobPublisher>,
        token_provider: Option<Arc<dyn dy_screen::asr::TemporaryAiTokenProvider>>,
    ) -> LocalAsrComponents {
        let repository = AiRepository::new(database.clone());
        let environment = Arc::new(LocalAsrEnvironment::load(resource_root));
        environment.set_cloud_configured(
            token_provider
                .as_ref()
                .is_some_and(|provider| provider.is_configured()),
        );
        let resources = environment.resources();
        let inspector_path = resources
            .as_ref()
            .map(|resources| resources.ffprobe.clone())
            .unwrap_or(fallback_ffprobe);
        let initial_inspector: Arc<dyn MediaInspector> = Arc::new(FfprobeMediaInspector::new(
            inspector_path,
            Duration::from_secs(10),
        ));
        let inspector_switch = Arc::new(Mutex::new(initial_inspector));
        let inspector: Arc<dyn MediaInspector> = Arc::new(ReloadableMediaInspector {
            current: inspector_switch.clone(),
        });
        let normalization_version = environment
            .default_recognition_profile()
            .map(|profile| profile.normalization_version)
            .unwrap_or_else(|_| "OpenCC ver.1.4.1".to_owned());
        let scheduler = resources.ok().and_then(|resources| {
            build_scheduler(
                database.clone(),
                repository.clone(),
                resources,
                temporary_root.clone(),
                publisher.clone(),
                inspector.clone(),
                &normalization_version,
                token_provider.clone(),
            )
            .ok()
            .map(Arc::new)
        });
        let runtime = Arc::new(Self {
            repository: repository.clone(),
            environment: environment.clone(),
            scheduler: Arc::new(Mutex::new(scheduler)),
            database,
            temporary_root: temporary_root.clone(),
            publisher,
            token_provider,
            inspector_switch,
            lifecycle: AiLifecycle::new(repository, temporary_root),
        });
        LocalAsrComponents {
            runtime,
            inspector,
            preflight: environment,
        }
    }

    /// 下载并校验 Runtime Resource Pack 后，在当前进程热激活 ASR 调度器。
    pub fn activate_resource_root(&self, root: PathBuf) -> Result<(), AiCommandError> {
        self.environment
            .activate_resource_root(root)
            .map_err(command_asr_error)?;
        let resources = self.environment.resources().map_err(command_asr_error)?;
        let ffprobe = resources.ffprobe.clone();
        if let Ok(mut inspector) = self.inspector_switch.lock() {
            *inspector = Arc::new(FfprobeMediaInspector::new(ffprobe, Duration::from_secs(10)));
        }
        let normalization_version = self
            .environment
            .default_recognition_profile()?
            .normalization_version;
        let scheduler = build_scheduler(
            self.database.clone(),
            self.repository.clone(),
            resources,
            self.temporary_root.clone(),
            self.publisher.clone(),
            Arc::new(ReloadableMediaInspector {
                current: self.inspector_switch.clone(),
            }),
            &normalization_version,
            self.token_provider.clone(),
        )
        .map_err(command_asr_error)
        .map(Arc::new)?;
        let mut current = self.scheduler.lock().map_err(|_| {
            AiCommandError::new(
                "asr_scheduler_unavailable",
                "本地 ASR 调度器状态不可用",
                true,
            )
        })?;
        if current.is_none() {
            *current = Some(scheduler);
        }
        Ok(())
    }

    pub fn scheduler_ready(&self) -> bool {
        self.scheduler
            .lock()
            .map(|scheduler| scheduler.is_some())
            .unwrap_or(false)
    }

    /// 授权恢复后重新创建已经停止的本地调度器。资源仍从受控 Runtime
    /// Resource Pack 解析，不接受前端传入可执行路径。
    pub fn resume_after_authorization(&self) -> Result<(), AiCommandError> {
        // 授权状态变化后刷新百炼直连可用性，使诊断反映真实路径。
        self.refresh_cloud_availability();
        if self.scheduler_ready() {
            return Ok(());
        }
        let resources = self.environment.resources().map_err(command_asr_error)?;
        let ffprobe = resources.ffprobe.clone();
        if let Ok(mut inspector) = self.inspector_switch.lock() {
            *inspector = Arc::new(FfprobeMediaInspector::new(ffprobe, Duration::from_secs(10)));
        }
        let normalization_version = self
            .environment
            .default_recognition_profile()?
            .normalization_version;
        let scheduler = Arc::new(
            build_scheduler(
                self.database.clone(),
                self.repository.clone(),
                resources,
                self.temporary_root.clone(),
                self.publisher.clone(),
                Arc::new(ReloadableMediaInspector {
                    current: self.inspector_switch.clone(),
                }),
                &normalization_version,
                self.token_provider.clone(),
            )
            .map_err(command_asr_error)?,
        );
        *self.scheduler.lock().map_err(|_| {
            AiCommandError::new(
                "asr_scheduler_unavailable",
                "本地 ASR 调度器状态不可用",
                true,
            )
        })? = Some(scheduler);
        Ok(())
    }

    pub fn recover_startup(&self) -> Result<AiRecoveryReport, AiCommandError> {
        self.lifecycle
            .recover_startup()
            .map_err(|error| AiCommandError::new("ai_recovery_failed", error.to_string(), true))
    }

    /// 启动恢复后从 SQLite 重建待处理队列；进程内 token 不跨重启持久化。
    pub async fn recover_startup_and_requeue(&self) -> Result<AiRecoveryReport, AiCommandError> {
        let report = self.recover_startup()?;
        let scheduler = self
            .scheduler
            .lock()
            .ok()
            .and_then(|scheduler| scheduler.clone());
        let Some(scheduler) = scheduler else {
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
        if let Some(scheduler) = self
            .scheduler
            .lock()
            .ok()
            .and_then(|scheduler| scheduler.clone())
        {
            self.lifecycle
                .shutdown(&scheduler)
                .await
                .map_err(|error| AiCommandError::new("ai_shutdown_failed", error.to_string(), true))
        } else {
            self.recover_startup()
        }
    }

    /// 授权失效时停止当前 ASR 并取走调度器；SQLite 项目状态由生命周期
    /// 对账，重新激活后可通过 `resume_after_authorization` 重建并恢复队列。
    pub async fn pause_for_authorization(&self) -> Result<AiRecoveryReport, AiCommandError> {
        // 授权失效后云端路径不可用；立即反映到诊断。
        self.refresh_cloud_availability();
        let scheduler = self
            .scheduler
            .lock()
            .ok()
            .and_then(|mut scheduler| scheduler.take());
        if let Some(scheduler) = scheduler {
            self.lifecycle.shutdown(&scheduler).await.map_err(|error| {
                AiCommandError::new("ai_authorization_pause_failed", error.to_string(), true)
            })
        } else {
            self.recover_startup()
        }
    }

    /// 按当前授权状态刷新百炼直连可用性。
    pub fn refresh_cloud_availability(&self) {
        let configured = self
            .token_provider
            .as_ref()
            .is_some_and(|provider| provider.is_configured());
        self.environment.set_cloud_configured(configured);
    }

    fn scheduler(&self) -> Result<Arc<TranscriptionScheduler>, AiCommandError> {
        self.scheduler
            .lock()
            .ok()
            .and_then(|scheduler| scheduler.clone())
            .ok_or_else(|| {
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
        if let Some(scheduler) = self
            .scheduler
            .lock()
            .ok()
            .and_then(|scheduler| scheduler.clone())
        {
            scheduler
                .cancel_project_and_wait(project_id, PROJECT_CANCEL_TIMEOUT)
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
            SchedulerEvent::WaitingForRecording(job) => (
                job,
                "waiting_for_recording",
                "录制优先模式：正在等待录制结束",
            ),
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

#[allow(clippy::too_many_arguments)]
fn build_scheduler(
    database: Database,
    repository: AiRepository,
    resources: ResolvedAsrResources,
    temporary_root: PathBuf,
    publisher: Arc<dyn AiJobPublisher>,
    inspector: Arc<dyn MediaInspector>,
    normalization_version: &str,
    token_provider: Option<Arc<dyn dy_screen::asr::TemporaryAiTokenProvider>>,
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
    let local_engine: Arc<dyn AsrEngine> = Arc::new(WhisperCppEngine::new(
        resources.clone(),
        ResourcePreflight::native(),
        vad_config.clone(),
    ));
    let dashscope_config = DashScopeAsrConfig::default();
    let dashscope = DashScopeAsrClient::new(dashscope_config.clone())?;
    // 三步直传发布器：POST /files 直传 -> GET /files/{id} 取签名临时 URL。
    // 未配置授权时 is_available() 为 false，云端路径保持不可用并显式诊断。
    let audio_publisher: SharedTemporaryAudioPublisher = match token_provider.clone() {
        Some(provider) => Arc::new(UpstreamFileUploadPublisher::new(
            dashscope_config.base_url.clone(),
            provider,
        )?),
        None => Arc::new(UnavailableTemporaryAudioPublisher),
    };
    let engine: Arc<dyn AsrEngine> = Arc::new(CloudFirstAsrEngine::new(
        local_engine.clone(),
        audio_publisher,
        token_provider,
        dashscope,
    ));
    let engine: Arc<dyn AsrEngine> = Arc::new(ProjectSelectableAsrEngine {
        cloud: engine,
        local: local_engine,
    });
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
#[allow(clippy::items_after_test_module)]
mod tests {
    use super::{DatabaseRecordingGate, PROJECT_CANCEL_TIMEOUT};
    use crate::database::Database;
    use crate::domain::NewStreamer;
    use dy_screen::asr::RecordingActivityGate;
    use std::time::Duration;

    #[test]
    fn project_cancel_timeout_is_thirty_seconds() {
        assert_eq!(PROJECT_CANCEL_TIMEOUT, Duration::from_secs(30));
    }

    #[test]
    fn recording_gate_allows_parallel_asr_by_default_and_supports_priority_mode() {
        let database = Database::open_in_memory().expect("打开测试数据库");
        database.migrate().expect("完成测试数据库迁移");
        let streamer = database
            .add_streamer(&NewStreamer::room(
                "并行测试主播",
                "parallel-1",
                "parallel-room",
                true,
            ))
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
