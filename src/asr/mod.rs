//! 与具体供应商无关的本地语音识别核心。
//!
//! 模块放在不依赖 Tauri 的根 crate 中，使桌面后台任务、命令行阶段测试和未来其他
//! 客户端都依赖同一套契约。供应商适配器不得把命令行参数或资源路径泄漏到业务类型。

mod contract;
mod error;
mod evidence;
mod media;
mod normalize;
mod quality;
mod resources;
mod runtime;
mod scheduler;
mod transcript;
mod vad;
mod whisper_cpp;

pub use contract::{
    AsrCapabilities, AsrConfidenceKind, AsrEngine, AsrEngineIdentity, AsrEnvironmentCheck,
    AsrEnvironmentReport, AsrProgressEvent, AsrProgressSink, AsrProgressStage, AsrRequest,
    AsrResult, AsrSegment, AsrWarning, PreparedAudio, PreparedAudioFormat, SpeechRegion,
    TimestampPolicy,
};
pub use error::{AsrError, AsrErrorKind, AsrResult as EngineResult};
pub use evidence::{
    AsrCompletionEvidenceCheck, AsrCompletionEvidencePaths, AsrCompletionEvidenceReport,
    AsrCompletionTaskStatus, audit_asr_completion_evidence,
};
pub use media::{
    AudioPreparationCapabilities, AudioPreparationRequest, FfmpegAudioPreparer,
    FfprobeMediaInspector, FrozenMediaSource, MediaAudioPreparer, MediaInspection, MediaInspector,
    PcmPipeProcess, PreparedAudioLease, cleanup_stale_audio_files, prepare_and_detect_speech,
};
pub use normalize::TextNormalizer;
pub use quality::{
    AsrQualityAnchor, AsrQualityCollectionEvidence, AsrQualityDataset, AsrQualityError,
    AsrQualityOutput, AsrQualityReport, AsrQualitySample, AsrQualitySegment, AsrQualityTerm,
    AsrQualityTerms, AsrReferenceRegion, PerformanceMetrics, RecallMetrics, SampleQualityReport,
    SilenceMetrics, TimestampMetrics, evaluate_quality_results, render_quality_markdown,
};
pub use resources::{
    AsrBundleManifest, AsrEngineManifest, AsrFileIntegrityManifest, AsrModelManifest,
    AsrNormalizationManifest, AsrPlatformManifest, AsrVadManifest,
};
pub use runtime::{
    AsrResourceResolver, NativeSystemResourceProbe, ResolvedAsrPlatformResource,
    ResolvedAsrResources, ResourcePreflight, SystemResourceProbe,
};
pub use scheduler::{
    AsrJobExecutor, RecordingActivityGate, SchedulerEvent, SchedulerEventSink, SchedulerJob,
    SchedulerQueueEntry, SchedulerSnapshot, TranscriptionScheduler,
};
pub use transcript::{
    AssembledTranscript, ProjectTimelineGap, ProjectTranscriptSegment, TranscriptAssembler,
    TranscriptInput, TranscriptInputSource, TranscriptSourceSegment,
};
pub use vad::{VadConfig, VadEngine, WhisperCppVadEngine};
pub use whisper_cpp::WhisperCppEngine;
