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
mod effect_caption;
mod effect_caption_cache;
mod effect_caption_composition;
mod effect_caption_orchestration;
mod effect_caption_render;
mod effect_caption_spec;
mod highlight;
mod lifecycle;
mod llm;
mod model_call;
mod processor;
mod projection;
mod repository;
mod service;
mod smart_workflow;
pub(crate) mod tauri_commands;

pub use clip_export::{
    ClipExportFailure, ClipExportPlan, ClipOutputDimensions, EffectCaptionOverlay,
    ExportResourceBaseline, ExportResourceSummary, TextLayerRange, TransparentFrameSummary,
    build_export_plan, build_export_plan_with_overlays, check_export_resource_baseline,
    cleanup_overlay_layers, execute_export, probe_output_dimensions, select_clip_video_encoder,
    validate_export_sources, validate_export_subtitles, validate_single_text_layer,
    verify_transparent_frame_sequence,
};
pub use clip_subtitle::{
    ClipSubtitleAssets, EffectCaptionAvailability, EffectCaptionWindow, PublishedSubtitleRange,
    TextLayerPlan, build_clip_subtitle_frames, build_clip_subtitle_frames_with_suppression,
    derive_effect_window, plan_text_layers, render_clip_subtitle_assets,
};
pub use clip_text_correction::{
    ActivationCorrectionGate, CLIP_TEXT_CORRECTION_PROMPT_VERSION, ClipCorrectionGate,
    ClipTextCorrectionError, ClipTextCorrectionWorkflow, evaluate_correction_gate,
    map_upstream_business_code,
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
    AiEffectCaptionRenderAttempt, AiEnhancementKind, AiEnhancementStatus, AiHighlightCandidate,
    AiHighlightCandidatePage, AiHighlightChunk, AiHighlightEnhancement, AiHighlightProgress,
    AiHighlightRun, AiHighlightRunStatus, AiInputSourceKind, AiInputStatus, AiProject,
    AiProjectDetail, AiProjectInput, AiProjectStatus, AiRenderAttemptStatus, AiSmartBatchStatus,
    AiSmartCandidate, AiSmartCandidateSource, AiSmartClipSourceInput, AiSmartDraft,
    AiSmartDraftOwnership, AiSmartDraftStatus, AiSmartReplaySession, AiSmartReplaySessionCursor,
    AiSmartReplaySessionPage, AiSmartStage, AiSmartStageAttempt, AiSmartStageAttemptStatus,
    AiSmartWorkflow, AiSmartWorkflowBatch, AiSmartWorkflowDetail, AiSmartWorkflowEvent,
    AiSmartWorkflowMetric, AiSmartWorkflowMode, AiSmartWorkflowStatus, AiSuppressionRange,
    AsrArtifact, AuthorizeSmartWorkflowInput, ClipTextCorrectionSummary,
    CreateLiveSmartWorkflowInput, CreateLocalSmartWorkflowInput, CreateReplaySmartWorkflowInput,
    NewAiEffectCaptionRenderAttempt, NewAiHighlightChunk, NewAiHighlightEnhancement,
    NewAiHighlightRun, NewAiProjectInput, NewAiSmartCandidate, NewAiSmartCandidateSource,
    NewAiSmartWorkflow, NewAiSmartWorkflowBatch, NewAsrArtifact, RecognitionProfile,
    RecoverySummary, RetrySmartWorkflowStageInput, SmartWorkflowConfiguration, SourceFingerprint,
    TranscriptSegment, TranscriptSegmentDraft,
};
pub use effect_caption::{
    BUNDLED_FONT_FAMILIES, CSS_FUNCTION_ALLOWLIST, CSS_PROPERTY_ALLOWLIST, EffectCaptionLimits,
    EffectCaptionRejection, GLOBAL_ATTRIBUTE_ALLOWLIST, ProposedEnhancement,
    PublishedHighlightSegment, RENDERER_CONTRACT_ATTRIBUTES, TAG_ALLOWLIST, VOID_ELEMENTS,
    ValidatedEffectCaption, ValidatedEnhancement, validate_effect_caption,
    validate_enhancement_proposals,
};
pub use effect_caption_cache::{
    RenderCacheOutcome, clear_render_cache, prune_render_cache, render_with_cache,
};
pub use effect_caption_composition::{
    BUNDLED_STYLESHEET, BuiltEffectCaption, CompositionRequest, EffectCaptionCompositionError,
    EffectCaptionCompositionProvider, EffectCaptionCompositionSkill,
    build_effect_caption_composition, composition_prompt, generate_effect_caption_composition,
};
pub use effect_caption_orchestration::{
    EffectCaptionEnhancementRunner, EnhancementHighlight, EnhancementProduct, EnhancementStagePlan,
    EnhancementStageRequest, EnhancementStageSummary, assert_enhancement_stage_position,
    enhancement_highlights, enhancement_stage_plan, published_ranges_from_subtitles,
    run_enhancement_for_batch, run_enhancement_stage,
};
pub use effect_caption_render::{
    AlwaysIdleGate, BaseCapabilityGate, DeterminismPolicy, EffectCaptionRenderer,
    EffectCaptionRuntimeError, FontResource, FontResourceSet, FrameParity, PageScriptAttestation,
    ProcessExit, ProcessLimits, ProcessMemoryProbe, RenderCacheKey, RenderInvocation, RenderLimits,
    RenderOutcome, RenderScheduler, RenderVersionSet, RendererRuntimeManifest, SubprocessRenderer,
    SystemMemoryProbe, VerifiedRendererRuntime, assert_paths_within_roots,
    build_restricted_command, build_restricted_command_with_fonts, cleanup_unpublished_frames,
    compare_frame_sequences, compute_tree_digest, digest_frame_sequence, supervise_process,
    verify_browser, verify_font_resources, verify_page_script_attestation, verify_render_outcome,
    verify_renderer_runtime,
};
pub use effect_caption_spec::{
    CanvasSpec, HighlightRenderWindow, MAX_RENDER_FPS, PREVIEW_FPS_CAP, PREVIEW_SCALE_DIVISOR,
    RenderPurpose, RenderSpec, assert_preview_export_consistent, plan_render_spec,
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
    DEFAULT_TRANSITION_AUTO_APPLY_SCORE, FakeHighlightProvider, FrozenModelCall,
    HighlightAgentProvider, HighlightCandidateDraft, HighlightCandidateScore, LlmError,
    LlmProviderSettings, LlmTokenLease, LlmTokenSource, MODEL_RESOLUTION_INTERVAL_MS,
    MemoryCredentialStore, ModelCallResolver, ModelCallSnapshot, ModelResolutionInputs,
    PROMPT_VERSION, ProviderDiagnostic, RankingAgentOutput, RankingAgentRequest, ResolutionSource,
    RigDeepSeekProvider, SubtitleCorrectionItem, SubtitleCorrectionOutput,
    SubtitleCorrectionProvider, SubtitleCorrectionRequest, SystemCredentialStore,
    TOKEN_SAFETY_MARGIN_SECONDS, TransitionAgentCandidate, TransitionAgentMatch,
    TransitionAgentOutput, TransitionAgentProvider, TransitionAgentRequest, TransitionAgentScore,
    TransitionScoreOutput, TransitionScoreRequest, acquire_llm_token,
    resolve_model_call_with_heartbeat,
};
pub use model_call::{ActivationLlmTokenSource, ModelCallRuntime};
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
    migrate_ai_v16, migrate_ai_v21, migrate_ai_v22, migrate_ai_v23, migrate_ai_v24, migrate_ai_v25,
    migrate_ai_v26, migrate_ai_v27,
};
