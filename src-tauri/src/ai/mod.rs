//! AI 分析项目的领域模型和 SQLite 持久化。
//!
//! 本模块只负责项目、输入、可复用产物和转写句段，不直接依赖 Tauri command 或 React。
//! 这样 repository 与状态机可以在没有 WebView 的环境中完成完整集成测试。

mod clip_export;
mod clip_subtitle;
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
pub(crate) mod tauri_commands;

pub use clip_export::{
    ClipExportFailure, ClipExportPlan, ClipOutputDimensions, build_export_plan, execute_export,
    probe_output_dimensions, select_clip_video_encoder, validate_export_sources,
    validate_export_subtitles,
};
pub use clip_subtitle::{ClipSubtitleAssets, render_clip_subtitle_assets};
pub use commands::{
    AiCommandError, AiCommandService, AiCreateProjectRequest, AiEnvironmentCheckView,
    AiEnvironmentDiagnostic, AiImportBatchView, AiJobController, AiProjectDetailView,
    AiProjectInputView, AiRuntimeComponentDiagnostic, AiRuntimeResourceDiagnostic,
    AiSessionImportView, AiTrustedFileGrant,
};
pub use desktop_runtime::{LocalAsrComponents, LocalAsrEnvironment, LocalAsrRuntime};
pub use domain::{
    AiArtifactStatus, AiClipEffect, AiClipExportStatus, AiClipProject, AiClipProjectDetail,
    AiClipSegment, AiClipSegmentUpdate, AiClipSubtitle, AiHighlightCandidate,
    AiHighlightCandidatePage, AiHighlightChunk, AiHighlightProgress, AiHighlightRun,
    AiHighlightRunStatus, AiInputSourceKind, AiInputStatus, AiProject, AiProjectDetail,
    AiProjectInput, AiProjectStatus, AsrArtifact, NewAiHighlightChunk, NewAiHighlightRun,
    NewAiProjectInput, NewAsrArtifact, RecognitionProfile, RecoverySummary, SourceFingerprint,
    TranscriptSegment, TranscriptSegmentDraft,
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
    FakeHighlightProvider, HighlightAgentProvider, HighlightCandidateDraft,
    HighlightCandidateScore, LlmError, LlmProviderSettings, MAX_AGENT_TURNS, MemoryCredentialStore,
    PROMPT_VERSION, ProviderDiagnostic, RankingAgentOutput, RankingAgentRequest,
    RigDeepSeekProvider, SystemCredentialStore,
};
pub use processor::{
    AiInputProcessor, AiJobEvent, AiJobPublisher, AiProcessOutcome, AiProcessorError,
};
pub use projection::{
    AiProjectionError, AiTranscriptInputProjection, AiTranscriptProjection,
    AiTranscriptSegmentProjection,
};
pub use repository::{
    AiRepository, AiRepositoryError, ClipExportSource, Result, project_clip_subtitles,
};
pub use service::{
    AiPreflight, AiProjectService, AiProjectSummary, AiReplaySessionCursor, AiReplaySessionOption,
    AiReplaySessionPage, AiReplayStreamerCursor, AiReplayStreamerOption, AiReplayStreamerPage,
    AiSessionOption, ImportBatchResult, ImportRejection, PreflightReport, ServiceError,
    SessionImportResult, TrustedLocalFile,
};

pub(crate) use repository::{
    migrate_ai_v4, migrate_ai_v7, migrate_ai_v9, migrate_ai_v12, migrate_ai_v13, migrate_ai_v14,
};
