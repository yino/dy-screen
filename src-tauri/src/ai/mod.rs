//! AI 分析项目的领域模型和 SQLite 持久化。
//!
//! 本模块只负责项目、输入、可复用产物和转写句段，不直接依赖 Tauri command 或 React。
//! 这样 repository 与状态机可以在没有 WebView 的环境中完成完整集成测试。

mod bundled_skills;
mod clip_export;
mod clip_subtitle;
mod clip_text_correction;
mod transcript_text_correction;
mod generated_asset;
mod generated_transition;
mod wan_generation;
mod commands;
mod desktop_runtime;
mod domain;
mod effect_caption;
mod effect_caption_cache;
mod effect_caption_composition;
mod effect_caption_orchestration;
mod effect_caption_render;
mod effect_caption_spec;
mod harness;
mod highlight;
mod lifecycle;
mod llm;
mod media_generation;
mod model_call;
mod model_tool;
mod processor;
mod projection;
mod repository;
mod service;
mod smart_workflow;
pub(crate) mod tauri_commands;

pub use bundled_skills::{
    ALLOWED_PROFILE_TOOLS, BundledSkill, HarnessProfile, SkillLoadError, bundled_skills_directory,
    load_bundled_skills, load_harness_profile, skill_snapshot, skills_fingerprint,
};
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
    TextLayerPlan, TranscriptEffectWindowSource, build_clip_subtitle_frames,
    build_clip_subtitle_frames_with_suppression, derive_effect_window,
    derive_effect_window_from_transcript, plan_text_layers, render_clip_subtitle_assets,
    shift_subtitles_for_inserts, transcript_effect_window_sources,
};
pub use clip_text_correction::{
    ActivationCorrectionGate, CLIP_TEXT_CORRECTION_PROMPT_VERSION, ClipCorrectionGate,
    ClipTextCorrectionError, ClipTextCorrectionWorkflow, evaluate_correction_gate,
    map_upstream_business_code,
};
pub use transcript_text_correction::{
    TranscriptCorrectionSummary, TranscriptCorrectionTarget, TranscriptTextCorrectionWorkflow,
};
pub use generated_transition::{
    GeneratedTransitionFailure, GenerationPoll, GeneratedTransitionOutcome, GeneratedTransitionRequest,
    GeneratedTransitionRunner, GeneratedTransitionTask, TransitionAction, WanGeneratedTransitionRunner,
    base64_encode, data_uri, generated_animation_path, parse_generated_animation_request,
    plan_transition_action, produce_generated_transition,
};
pub use generated_asset::{
    AssetExpectation, AssetRejection, FfprobeMediaProbe, MediaProbe, ProbedMedia, ValidatedAsset,
    download_and_validate, sha256_hex,
};
pub use wan_generation::{
    DASHSCOPE_API_BASE_URL, GeneratedAsset, GenerationClient, GenerationConfig,
    GenerationFailure, GenerationTransport, ReqwestTransport, multimodal_body, text2image_body,
    video_body,
};
pub use commands::{
    AiCommandError, AiCommandService, AiCreateProjectRequest, AiEnvironmentCheckView,
    AiEnvironmentDiagnostic, AiImportBatchView, AiJobController, AiModelStatusView,
    AiProjectDetailView, AiProjectInputView, AiRuntimeComponentDiagnostic,
    AiRuntimeResourceDiagnostic, AiSessionImportView, AiTrustedFileGrant, LlmCallDiagnosticsView,
};
pub use desktop_runtime::{LocalAsrComponents, LocalAsrEnvironment, LocalAsrRuntime};
pub use domain::{
    AiActiveLiveSession, AiArtifactStatus, AiClipEffect, AiClipExportStatus, AiClipProject,
    AiClipProjectDetail, AiClipProjectSource, AiClipSegment, AiClipSegmentUpdate, AiClipSubtitle,
    AiClipSubtitleFrame, AiClipSubtitleUpdate, AiClipTimelineUnit, AiClipTimelineUnitKind,
    AiEffectCaptionRenderAttempt, AiEnhancementKind, AiEnhancementStatus, AiGeneratedAnimationAsset,
    AiGeneratedAnimationComposition, AiGeneratedAnimationInsertBinding,
    AiGeneratedAnimationReplaceRange, AiGeneratedAnimationStatus, AiHighlightCandidate,
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
    NewAiEffectCaptionRenderAttempt, NewAiGeneratedAnimationAsset, NewAiHighlightChunk,
    NewAiHighlightEnhancement,
    NewAiHighlightRun, NewAiProjectInput, NewAiSmartCandidate, NewAiSmartCandidateSource,
    NewAiSmartWorkflow, NewAiSmartWorkflowBatch, NewAsrArtifact, RecognitionProfile,
    RecoverySummary, RetrySmartWorkflowStageInput, SmartWorkflowConfiguration, SourceFingerprint,
    TranscriptSegment, TranscriptSegmentDraft, GENERATED_ANIMATION_CACHE_DIR,
    GENERATED_ANIMATION_DURATION_TOLERANCE_PERMILLE, GENERATED_ANIMATION_IMAGE_TASK_TYPE,
    GENERATED_ANIMATION_MAX_SLOWDOWN_PERMILLE, GENERATED_ANIMATION_VIDEO_TASK_TYPE,
    GeneratedAnimationReuseInput, GeneratedDurationAdaptation, ReplaceSourceRange,
    ReplaceTargetRange, adapt_generated_duration, candidates_eligible_for_effect_caption,
    enhancement_review_items, find_enhancement_kind_conflict, generated_animation_file_name,
    generated_animation_request_id, generated_animation_reuse_key, insert_binding_is_valid,
    resolve_replace_target_range, AiEnhancementReviewItem,
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
pub use harness::{
    HARNESS_MAX_MESSAGE_BYTES, HARNESS_PROTOCOL_VERSION, HarnessError, HarnessLimits,
    HarnessManager, HarnessSidecar, JsonRpcError, JsonRpcRequest, JsonRpcResponse, ToolAllowlist,
    ToolExecutor,
};
pub use highlight::{
    AnalysisChunk, AnalysisFingerprintConfig, AnalysisSegment, COMEDY_PAYOFF, ECOMMERCE_CONVERSION,
    FASHION_BEAUTY, FOOD_APPEAL, GAMING_HIGHLIGHT, GENERIC_HOOK, HighlightSkill, HighlightWorkflow,
    Industry, KNOWLEDGE_DENSITY, MUSIC_PERFORMANCE, STORY_EMOTION, analysis_fingerprint,
    build_industry_prompt, chunk_segments, parse_industry, select_skills,
};
pub use lifecycle::{AiLifecycle, AiLifecycleError, AiRecoveryReport};
pub use llm::{
    CandidateAgentOutput, CandidateAgentRequest, CredentialError, CredentialStore,
    DEFAULT_EXCELLENT_SCORE, DEFAULT_MODEL_ID, DEFAULT_QUALIFIED_SCORE,
    DEFAULT_TRANSITION_AUTO_APPLY_SCORE, FakeHighlightProvider, FrozenModelCall,
    HighlightAgentProvider, HighlightCandidateDraft, HighlightCandidateScore, IndustryAgentRequest,
    IndustryDetectionOutput, LLM_CALL_DIAGNOSTIC_CAPACITY, LlmCallDiagnostic, LlmError,
    LlmProviderSettings, LlmTokenLease,
    LlmTokenSource, MODEL_RESOLUTION_INTERVAL_MS, MemoryCredentialStore, ModelCallHandle,
    ModelCallPath, ModelCallResolver, ModelCallSnapshot, ModelResolutionInputs, PROMPT_VERSION,
    ProviderDiagnostic, RankingAgentOutput, RankingAgentRequest, ResolutionSource,
    RigDeepSeekProvider, RigTextCompleter, SubtitleCorrectionItem, SubtitleCorrectionOutput,
    SubtitleCorrectionProvider, SubtitleCorrectionRequest, SystemCredentialStore,
    TOKEN_SAFETY_MARGIN_SECONDS, TextCompleter, TextCompletion, TransitionAgentCandidate,
    TransitionAgentMatch, TransitionAgentOutput, TransitionAgentProvider, TransitionAgentRequest,
    TransitionAgentScore, TransitionScoreOutput, TransitionScoreRequest, acquire_llm_token,
    llm_call_diagnostics_enabled, llm_call_diagnostics_snapshot, resolve_model_call_with_heartbeat,
    set_llm_call_diagnostics_enabled,
};
pub use media_generation::{
    MediaGenerationError, MediaGenerationKind, MediaGenerationRequest, MediaGenerationResult,
    MediaGenerationTool, MediaQuota, MediaTaskStatus, MediaUpstream, StubMediaUpstream,
};
pub use model_call::{ActivationLlmTokenSource, ModelCallRuntime, TokenStatus};
pub use model_tool::{
    ModelGenerateTextRequest, ModelTextTool, UnifiedCallPointCompositionProvider,
};
pub use processor::{
    AiInputProcessor, AiJobEvent, AiJobPublisher, AiProcessOutcome, AiProcessorError,
};
pub use projection::{
    AiProjectionError, AiTranscriptInputProjection, AiTranscriptProjection,
    AiTranscriptSegmentProjection,
};
pub use repository::{
    AiRepository, AiRepositoryError, ClipExportBridge, ClipExportInsert, ClipExportReplace,
    ClipExportSource, ClipTextCorrectionPlan,
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
    migrate_ai_v26, migrate_ai_v27, migrate_ai_v28, migrate_ai_v29, migrate_ai_v30, migrate_ai_v31,
    migrate_ai_v32,
};
