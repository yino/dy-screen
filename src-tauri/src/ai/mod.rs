//! AI 分析项目的领域模型和 SQLite 持久化。
//!
//! 本模块只负责项目、输入、可复用产物和转写句段，不直接依赖 Tauri command 或 React。
//! 这样 repository 与状态机可以在没有 WebView 的环境中完成完整集成测试。

mod clip_export;
mod clip_subtitle;
mod clip_text_correction;
mod commands;
mod desktop_runtime;
mod domain;
mod highlight;
mod lifecycle;
mod llm;
mod processor;
mod projection;
mod repository;
mod service;
mod smart_workflow;
pub(crate) mod tauri_commands;

pub use clip_export::{
    ClipExportFailure, ClipExportPlan, ClipOutputDimensions, build_export_plan, execute_export,
    probe_output_dimensions, select_clip_video_encoder, validate_export_sources,
    validate_export_subtitles,
};
pub use clip_subtitle::{
    ClipSubtitleAssets, build_clip_subtitle_frames, render_clip_subtitle_assets,
};
pub use clip_text_correction::{
    CLIP_TEXT_CORRECTION_PROMPT_VERSION, ClipTextCorrectionError, ClipTextCorrectionWorkflow,
};
pub use commands::{
    AiCommandError, AiCommandService, AiCreateProjectRequest, AiEnvironmentCheckView,
    AiEnvironmentDiagnostic, AiImportBatchView, AiJobController, AiProjectDetailView,
    AiProjectInputView, AiRuntimeComponentDiagnostic, AiRuntimeResourceDiagnostic,
    AiSessionImportView, AiTrustedFileGrant,
};
pub use desktop_runtime::{LocalAsrComponents, LocalAsrEnvironment, LocalAsrRuntime};
pub use domain::{
    AiActiveLiveSession, AiArtifactStatus, AiClipEffect, AiClipExportStatus, AiClipProject,
    AiClipProjectDetail, AiClipProjectSource, AiClipSegment, AiClipSegmentUpdate, AiClipSubtitle,
    AiClipSubtitleFrame, AiClipSubtitleUpdate, AiClipTimelineUnit, AiClipTimelineUnitKind,
    AiHighlightCandidate, AiHighlightCandidatePage, AiHighlightChunk, AiHighlightProgress,
    AiHighlightRun, AiHighlightRunStatus, AiInputSourceKind, AiInputStatus, AiProject,
    AiProjectDetail, AiProjectInput, AiProjectStatus, AiSmartBatchStatus, AiSmartCandidate,
    AiSmartCandidateSource, AiSmartClipSourceInput, AiSmartDraft, AiSmartDraftOwnership,
    AiSmartDraftStatus, AiSmartReplaySession, AiSmartReplaySessionCursor, AiSmartReplaySessionPage,
    AiSmartStage, AiSmartStageAttempt, AiSmartStageAttemptStatus, AiSmartWorkflow,
    AiSmartWorkflowBatch, AiSmartWorkflowDetail, AiSmartWorkflowEvent, AiSmartWorkflowMetric,
    AiSmartWorkflowMode, AiSmartWorkflowStatus, AsrArtifact, AuthorizeSmartWorkflowInput,
    ClipTextCorrectionSummary, CreateLiveSmartWorkflowInput, CreateLocalSmartWorkflowInput,
    CreateReplaySmartWorkflowInput, NewAiHighlightChunk, NewAiHighlightRun, NewAiProjectInput,
    NewAiSmartCandidate, NewAiSmartCandidateSource, NewAiSmartWorkflow, NewAiSmartWorkflowBatch,
    NewAsrArtifact, RecognitionProfile, RecoverySummary, RetrySmartWorkflowStageInput,
    SmartWorkflowConfiguration, SourceFingerprint, TranscriptSegment, TranscriptSegmentDraft,
};
pub use highlight::{
    AnalysisChunk, AnalysisFingerprintConfig, AnalysisSegment, COMEDY_PAYOFF, ECOMMERCE_CONVERSION,
    GENERIC_HOOK, HighlightSkill, HighlightWorkflow, KNOWLEDGE_DENSITY, STORY_EMOTION,
    analysis_fingerprint, chunk_segments, select_skills,
};
pub use lifecycle::{AiLifecycle, AiLifecycleError, AiRecoveryReport};
pub use llm::{
    CandidateAgentOutput, CandidateAgentRequest, CredentialError, CredentialStore,
    DEEPSEEK_BASE_URL, DEFAULT_EXCELLENT_SCORE, DEFAULT_MODEL_ID, DEFAULT_QUALIFIED_SCORE,
    DEFAULT_TRANSITION_AUTO_APPLY_SCORE, FakeHighlightProvider, HighlightAgentProvider,
    HighlightCandidateDraft, HighlightCandidateScore, LlmError, LlmProviderSettings,
    MAX_AGENT_TURNS, MemoryCredentialStore, PROMPT_VERSION, ProviderDiagnostic, RankingAgentOutput,
    RankingAgentRequest, RigDeepSeekProvider, SubtitleCorrectionItem, SubtitleCorrectionOutput,
    SubtitleCorrectionProvider, SubtitleCorrectionRequest, SystemCredentialStore,
    TransitionAgentCandidate, TransitionAgentMatch, TransitionAgentOutput, TransitionAgentProvider,
    TransitionAgentRequest, TransitionAgentScore, TransitionScoreOutput, TransitionScoreRequest,
};
pub use processor::{
    AiInputProcessor, AiJobEvent, AiJobPublisher, AiProcessOutcome, AiProcessorError,
};
pub use projection::{
    AiProjectionError, AiTranscriptInputProjection, AiTranscriptProjection,
    AiTranscriptSegmentProjection,
};
pub use repository::{
    AiRepository, AiRepositoryError, ClipExportBridge, ClipExportSource, ClipTextCorrectionPlan,
    ClipTextCorrectionTarget, ClipTextCorrectionUpdate, Result, project_clip_subtitles,
};
pub use service::{
    AiPreflight, AiProjectService, AiProjectSummary, AiReplaySessionCursor, AiReplaySessionOption,
    AiReplaySessionPage, AiReplayStreamerCursor, AiReplayStreamerOption, AiReplayStreamerPage,
    AiSessionOption, ImportBatchResult, ImportRejection, PreflightReport, ServiceError,
    SessionImportResult, TrustedLocalFile,
};
pub use smart_workflow::{
    SilentSmartWorkflowPublisher, SilentSmartWorkflowTelemetry, SmartClippingWorkflow,
    SmartStageAttemptStart, SmartWorkflowError, SmartWorkflowGate, SmartWorkflowPublisher,
    SmartWorkflowRepository, SmartWorkflowResult, SmartWorkflowTelemetry,
};

pub(crate) use repository::{
    migrate_ai_v4, migrate_ai_v7, migrate_ai_v9, migrate_ai_v12, migrate_ai_v13, migrate_ai_v14,
    migrate_ai_v16, migrate_ai_v21, migrate_ai_v22, migrate_ai_v23, migrate_ai_v24,
};
