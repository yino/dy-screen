//! AI 分析项目的领域模型和 SQLite 持久化。
//!
//! 本模块只负责项目、输入、可复用产物和转写句段，不直接依赖 Tauri command 或 React。
//! 这样 repository 与状态机可以在没有 WebView 的环境中完成完整集成测试。

mod commands;
mod desktop_runtime;
mod domain;
mod lifecycle;
mod processor;
mod projection;
mod repository;
mod service;
pub(crate) mod tauri_commands;

pub use commands::{
    AiCommandError, AiCommandService, AiCreateProjectRequest, AiEnvironmentCheckView,
    AiEnvironmentDiagnostic, AiImportBatchView, AiJobController, AiProjectDetailView,
    AiProjectInputView, AiTrustedFileGrant,
};
pub use desktop_runtime::{LocalAsrComponents, LocalAsrEnvironment, LocalAsrRuntime};
pub use domain::{
    AiArtifactStatus, AiInputSourceKind, AiInputStatus, AiProject, AiProjectDetail, AiProjectInput,
    AiProjectStatus, AsrArtifact, NewAiProjectInput, NewAsrArtifact, RecognitionProfile,
    RecoverySummary, SourceFingerprint, TranscriptSegment, TranscriptSegmentDraft,
};
pub use lifecycle::{AiLifecycle, AiLifecycleError, AiRecoveryReport};
pub use processor::{
    AiInputProcessor, AiJobEvent, AiJobPublisher, AiProcessOutcome, AiProcessorError,
};
pub use projection::{
    AiProjectionError, AiTranscriptInputProjection, AiTranscriptProjection,
    AiTranscriptSegmentProjection,
};
pub use repository::{AiRepository, AiRepositoryError, Result};
pub use service::{
    AiPreflight, AiProjectService, AiProjectSummary, AiSessionOption, ImportBatchResult,
    ImportRejection, PreflightReport, ServiceError, TrustedLocalFile,
};

pub(crate) use repository::migrate_ai_v3;
