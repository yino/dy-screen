//! AI 项目、输入、识别配置、产物和稳定句段的持久化领域类型。

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::repository::{AiRepositoryError, Result};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AiHighlightRunStatus {
    Pending,
    Running,
    Candidates,
    Ranking,
    Completed,
    Partial,
    Cancelled,
    Failed,
}

impl AiHighlightRunStatus {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Candidates => "candidates",
            Self::Ranking => "ranking",
            Self::Completed => "completed",
            Self::Partial => "partial",
            Self::Cancelled => "cancelled",
            Self::Failed => "failed",
        }
    }

    pub(crate) fn parse(value: &str) -> Result<Self> {
        match value {
            "pending" => Ok(Self::Pending),
            "running" => Ok(Self::Running),
            "candidates" => Ok(Self::Candidates),
            "ranking" => Ok(Self::Ranking),
            "completed" => Ok(Self::Completed),
            "partial" => Ok(Self::Partial),
            "cancelled" => Ok(Self::Cancelled),
            "failed" => Ok(Self::Failed),
            _ => Err(AiRepositoryError::Integrity("高光运行状态无效".to_owned())),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AiProjectStatus {
    Draft,
    Queued,
    Running,
    Deleting,
    Completed,
    CompletedWithErrors,
    Cancelled,
    Failed,
}

impl AiProjectStatus {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Deleting => "deleting",
            Self::Completed => "completed",
            Self::CompletedWithErrors => "completed_with_errors",
            Self::Cancelled => "cancelled",
            Self::Failed => "failed",
        }
    }

    pub(crate) fn parse(value: &str) -> Result<Self> {
        match value {
            "draft" => Ok(Self::Draft),
            "queued" => Ok(Self::Queued),
            "running" => Ok(Self::Running),
            "deleting" => Ok(Self::Deleting),
            "completed" => Ok(Self::Completed),
            "completed_with_errors" => Ok(Self::CompletedWithErrors),
            "cancelled" => Ok(Self::Cancelled),
            "failed" => Ok(Self::Failed),
            _ => Err(AiRepositoryError::Integrity("项目状态无效".to_owned())),
        }
    }

    pub(crate) fn can_transition_to(self, next: Self) -> bool {
        matches!(
            (self, next),
            (Self::Draft, Self::Queued)
                | (
                    Self::Queued,
                    Self::Running | Self::Cancelled | Self::Failed | Self::Deleting
                )
                | (
                    Self::Running,
                    Self::Completed
                        | Self::CompletedWithErrors
                        | Self::Cancelled
                        | Self::Failed
                        | Self::Deleting
                )
        )
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AiInputStatus {
    Pending,
    Validating,
    PreparingAudio,
    DetectingSpeech,
    Transcribing,
    Completed,
    Skipped,
    Cancelled,
    Failed,
}

impl AiInputStatus {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Validating => "validating",
            Self::PreparingAudio => "preparing_audio",
            Self::DetectingSpeech => "detecting_speech",
            Self::Transcribing => "transcribing",
            Self::Completed => "completed",
            Self::Skipped => "skipped",
            Self::Cancelled => "cancelled",
            Self::Failed => "failed",
        }
    }

    pub(crate) fn parse(value: &str) -> Result<Self> {
        match value {
            "pending" => Ok(Self::Pending),
            "validating" => Ok(Self::Validating),
            "preparing_audio" => Ok(Self::PreparingAudio),
            "detecting_speech" => Ok(Self::DetectingSpeech),
            "transcribing" => Ok(Self::Transcribing),
            "completed" => Ok(Self::Completed),
            "skipped" => Ok(Self::Skipped),
            "cancelled" => Ok(Self::Cancelled),
            "failed" => Ok(Self::Failed),
            _ => Err(AiRepositoryError::Integrity("输入状态无效".to_owned())),
        }
    }

    pub(crate) fn can_transition_to(self, next: Self) -> bool {
        matches!(
            (self, next),
            (
                Self::Pending,
                Self::Validating | Self::Cancelled | Self::Failed
            ) | (
                Self::Validating,
                Self::PreparingAudio
                    | Self::Completed
                    | Self::Skipped
                    | Self::Cancelled
                    | Self::Failed
            ) | (
                Self::PreparingAudio,
                Self::DetectingSpeech | Self::Cancelled | Self::Failed
            ) | (
                Self::DetectingSpeech,
                Self::Transcribing | Self::Skipped | Self::Cancelled | Self::Failed
            ) | (
                Self::Transcribing,
                Self::Completed | Self::Cancelled | Self::Failed
            ) | (Self::Failed, Self::Pending)
        )
    }

    /// 阶段对应的可恢复进度。细粒度模型进度只用于实时事件，页面重建时以此值为准。
    pub fn progress_percent(self) -> u8 {
        match self {
            Self::Pending => 0,
            Self::Validating => 10,
            Self::PreparingAudio => 25,
            Self::DetectingSpeech => 45,
            Self::Transcribing => 75,
            Self::Completed | Self::Skipped | Self::Cancelled | Self::Failed => 100,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AiArtifactStatus {
    Pending,
    Published,
    Invalidated,
}

impl AiArtifactStatus {
    pub(crate) fn parse(value: &str) -> Result<Self> {
        match value {
            "pending" => Ok(Self::Pending),
            "published" => Ok(Self::Published),
            "invalidated" => Ok(Self::Invalidated),
            _ => Err(AiRepositoryError::Integrity("识别产物状态无效".to_owned())),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AiInputSourceKind {
    LocalFile,
    VideoLibrary,
}

impl AiInputSourceKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::LocalFile => "local_file",
            Self::VideoLibrary => "video_library",
        }
    }

    pub(crate) fn parse(value: &str) -> Result<Self> {
        match value {
            "local_file" => Ok(Self::LocalFile),
            "video_library" => Ok(Self::VideoLibrary),
            _ => Err(AiRepositoryError::Integrity("输入来源无效".to_owned())),
        }
    }
}

/// 参与缓存指纹的完整识别配置。新增影响结果的参数时必须加入本结构。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RecognitionProfile {
    pub engine_id: String,
    pub engine_version: String,
    pub model_id: String,
    pub model_version: String,
    pub language_hint: Option<String>,
    pub vad_model_id: String,
    pub vad_threshold_millis: u32,
    pub vad_padding_ms: u64,
    pub timestamp_policy: String,
    pub normalization_version: String,
    pub hotwords: Vec<String>,
}

impl RecognitionProfile {
    /// 使用规范化 JSON 生成稳定 SHA-256，避免热词输入顺序导致无意义缓存失效。
    pub fn fingerprint(&self) -> Result<String> {
        let mut normalized = self.clone();
        normalized.hotwords = normalized
            .hotwords
            .iter()
            .map(|word| word.trim().to_owned())
            .filter(|word| !word.is_empty())
            .collect();
        normalized.hotwords.sort();
        normalized.hotwords.dedup();
        let json = serde_json::to_vec(&normalized)
            .map_err(|_| AiRepositoryError::Serialization("识别配置无法序列化".to_owned()))?;
        Ok(hex::encode(Sha256::digest(json)))
    }
}

/// 源指纹使用规范路径、大小、修改时间和可选视频库 ID，禁止仅按文件名复用结果。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SourceFingerprint {
    pub normalized_path: String,
    pub size_bytes: u64,
    pub modified_at_ms: i64,
    pub video_id: Option<i64>,
}

impl SourceFingerprint {
    pub fn fingerprint(&self) -> Result<String> {
        let json = serde_json::to_vec(self)
            .map_err(|_| AiRepositoryError::Serialization("源指纹无法序列化".to_owned()))?;
        Ok(hex::encode(Sha256::digest(json)))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AiProject {
    pub id: i64,
    pub name: String,
    pub status: AiProjectStatus,
    pub recognition_profile: RecognitionProfile,
    pub recognition_profile_hash: String,
    pub input_frozen: bool,
    pub progress_percent: u8,
    pub last_error_code: Option<String>,
    pub last_error_message: Option<String>,
    pub project_tags: Vec<String>,
    pub analysis_goal: Option<String>,
    pub deleting_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AiProjectInput {
    pub id: i64,
    pub project_id: i64,
    pub position: i64,
    pub source_kind: AiInputSourceKind,
    pub video_id: Option<i64>,
    pub display_name: String,
    pub source_path: String,
    pub source_fingerprint: SourceFingerprint,
    pub source_fingerprint_hash: String,
    pub duration_ms: Option<u64>,
    pub audio_present: Option<bool>,
    pub project_offset_ms: Option<u64>,
    pub status: AiInputStatus,
    /// 由持久化阶段稳定推导，事件丢失或页面重建后仍可恢复。
    pub progress_percent: u8,
    pub artifact_id: Option<i64>,
    pub last_error_code: Option<String>,
    pub last_error_message: Option<String>,
    pub scheduler_generation: u64,
    pub queue_priority: u32,
    pub queue_sequence: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AiProjectDetail {
    pub project: AiProject,
    pub inputs: Vec<AiProjectInput>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct NewAiProjectInput {
    pub position: i64,
    pub source_kind: AiInputSourceKind,
    pub video_id: Option<i64>,
    pub display_name: String,
    pub source_path: String,
    pub source_fingerprint: SourceFingerprint,
    pub duration_ms: Option<u64>,
    pub audio_present: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct NewAsrArtifact {
    pub source_fingerprint: SourceFingerprint,
    pub recognition_profile_hash: String,
    pub engine_id: String,
    pub engine_version: String,
    pub model_id: String,
    pub model_version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AsrArtifact {
    pub id: i64,
    pub source_fingerprint: SourceFingerprint,
    pub source_fingerprint_hash: String,
    pub recognition_profile_hash: String,
    pub engine_id: String,
    pub engine_version: String,
    pub model_id: String,
    pub model_version: String,
    pub status: AiArtifactStatus,
    pub duration_ms: Option<u64>,
    pub language: Option<String>,
    pub created_at: String,
    pub published_at: Option<String>,
    pub last_used_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptSegmentDraft {
    pub source_start_ms: u64,
    pub source_end_ms: u64,
    pub raw_text: String,
    pub normalized_text: String,
    pub confidence: Option<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptSegment {
    pub id: String,
    pub artifact_id: i64,
    pub ordinal: i64,
    pub source_start_ms: u64,
    pub source_end_ms: u64,
    pub raw_text: String,
    pub normalized_text: String,
    pub confidence: Option<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AiHighlightRun {
    pub id: i64,
    pub project_id: i64,
    pub status: AiHighlightRunStatus,
    pub model_id: String,
    pub prompt_version: String,
    pub tags_snapshot: Vec<String>,
    pub skills_snapshot: Vec<String>,
    pub analysis_goal: Option<String>,
    pub analysis_fingerprint: String,
    pub qualified_score: u8,
    pub excellent_score: u8,
    pub user_authorized: bool,
    pub total_segments: u64,
    pub total_chars: u64,
    pub estimated_batches: u64,
    pub total_tokens: u64,
    pub last_error_code: Option<String>,
    pub last_error_message: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AiHighlightChunk {
    pub id: i64,
    pub run_id: i64,
    pub ordinal: i64,
    pub input_id: i64,
    pub segment_ids: Vec<String>,
    pub context_segment_ids: Vec<String>,
    pub status: String,
    pub candidate_count: u64,
    pub token_usage: u64,
    pub last_error_code: Option<String>,
    pub last_error_message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AiHighlightProgress {
    pub run_id: i64,
    pub total_batches: u64,
    pub pending_batches: u64,
    pub running_batches: u64,
    pub completed_batches: u64,
    pub failed_batches: u64,
    pub candidate_count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AiHighlightCandidate {
    pub id: i64,
    pub run_id: i64,
    pub chunk_id: Option<i64>,
    pub candidate_key: String,
    pub title: String,
    pub input_id: i64,
    pub segment_ids: Vec<String>,
    pub start_ms: u64,
    pub end_ms: u64,
    pub total_score: f32,
    pub hook_score: f32,
    pub information_score: f32,
    pub emotion_score: f32,
    pub tag_relevance_score: f32,
    pub completeness_score: f32,
    pub shareability_score: f32,
    pub reason: String,
    pub matched_tags: Vec<String>,
    pub rank: Option<u32>,
    pub selected: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AiHighlightCandidatePage {
    pub items: Vec<AiHighlightCandidate>,
    pub page: u32,
    pub page_size: u32,
    pub total_candidates: u64,
    pub qualified_candidates: u64,
    pub selected_candidates: u64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AiClipEffect {
    None,
    FadeIn,
    FadeOut,
    FadeInOut,
    Flash,
    Black,
}

impl AiClipEffect {
    pub const ALL: [Self; 6] = [
        Self::None,
        Self::FadeIn,
        Self::FadeOut,
        Self::FadeInOut,
        Self::Flash,
        Self::Black,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::FadeIn => "fade_in",
            Self::FadeOut => "fade_out",
            Self::FadeInOut => "fade_in_out",
            Self::Flash => "flash",
            Self::Black => "black",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|effect| effect.as_str() == value)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AiClipExportStatus {
    Idle,
    Exporting,
    Completed,
    Cancelled,
    Failed,
}

impl AiClipExportStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Exporting => "exporting",
            Self::Completed => "completed",
            Self::Cancelled => "cancelled",
            Self::Failed => "failed",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        [
            Self::Idle,
            Self::Exporting,
            Self::Completed,
            Self::Cancelled,
            Self::Failed,
        ]
        .into_iter()
        .find(|status| status.as_str() == value)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AiClipProject {
    pub id: i64,
    pub highlight_run_id: i64,
    /// 普通工程为空；智能合辑关联其父工作流。
    pub smart_workflow_id: Option<i64>,
    /// 智能合辑的单调草稿代次；普通工程为空。
    pub draft_generation: Option<u32>,
    pub ownership: AiSmartDraftOwnership,
    pub name: String,
    /// 首次导出时冻结，后续重排片段也不会意外改变成品画幅。
    pub output_width: Option<u32>,
    pub output_height: Option<u32>,
    /// 每次工程片段编辑后递增，供恢复和后续编辑能力识别工程快照版本。
    pub version: u32,
    /// 导出开始时冻结的工程版本，只用于审计和代次隔离。
    pub export_frozen_version: Option<u32>,
    pub export_status: AiClipExportStatus,
    pub export_progress: u8,
    pub output_path: Option<String>,
    pub last_error_code: Option<String>,
    pub last_error_message: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AiSmartWorkflowMode {
    Local,
    Replay,
    Live,
}

impl AiSmartWorkflowMode {
    pub const ALL: [Self; 3] = [Self::Local, Self::Replay, Self::Live];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Replay => "replay",
            Self::Live => "live",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|item| item.as_str() == value)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AiSmartWorkflowStatus {
    Draft,
    Queued,
    Running,
    AwaitingSelection,
    ReviewReady,
    Paused,
    Completed,
    Cancelled,
    Failed,
}

impl AiSmartWorkflowStatus {
    pub const ALL: [Self; 9] = [
        Self::Draft,
        Self::Queued,
        Self::Running,
        Self::AwaitingSelection,
        Self::ReviewReady,
        Self::Paused,
        Self::Completed,
        Self::Cancelled,
        Self::Failed,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Queued => "queued",
            Self::Running => "running",
            Self::AwaitingSelection => "awaiting_selection",
            Self::ReviewReady => "review_ready",
            Self::Paused => "paused",
            Self::Completed => "completed",
            Self::Cancelled => "cancelled",
            Self::Failed => "failed",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|item| item.as_str() == value)
    }

    pub fn can_transition_to(self, next: Self) -> bool {
        use AiSmartWorkflowStatus as Status;
        self == next
            || matches!(
                (self, next),
                (Status::Draft, Status::Queued | Status::Cancelled)
                    | (
                        Status::Queued,
                        Status::Running | Status::Paused | Status::Cancelled | Status::Failed
                    )
                    | (
                        Status::Running,
                        Status::AwaitingSelection
                            | Status::ReviewReady
                            | Status::Paused
                            | Status::Completed
                            | Status::Cancelled
                            | Status::Failed
                    )
                    | (
                        Status::AwaitingSelection,
                        Status::Running
                            | Status::ReviewReady
                            | Status::Paused
                            | Status::Cancelled
                            | Status::Failed
                    )
                    | (
                        Status::ReviewReady,
                        Status::Running | Status::Completed | Status::Cancelled | Status::Failed
                    )
                    | (
                        Status::Paused,
                        Status::Queued | Status::Running | Status::Cancelled | Status::Failed
                    )
                    | (
                        Status::Failed,
                        Status::Queued | Status::Running | Status::Cancelled
                    )
            )
    }

    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Cancelled)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AiSmartStage {
    Preflight,
    Asr,
    Highlight,
    Draft,
    Correction,
    Transition,
    Review,
}

impl AiSmartStage {
    pub const ALL: [Self; 7] = [
        Self::Preflight,
        Self::Asr,
        Self::Highlight,
        Self::Draft,
        Self::Correction,
        Self::Transition,
        Self::Review,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Preflight => "preflight",
            Self::Asr => "asr",
            Self::Highlight => "highlight",
            Self::Draft => "draft",
            Self::Correction => "correction",
            Self::Transition => "transition",
            Self::Review => "review",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|item| item.as_str() == value)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AiSmartBatchStatus {
    Pending,
    Queued,
    Asr,
    Highlight,
    Completed,
    Failed,
    Cancelled,
}

impl AiSmartBatchStatus {
    pub const ALL: [Self; 7] = [
        Self::Pending,
        Self::Queued,
        Self::Asr,
        Self::Highlight,
        Self::Completed,
        Self::Failed,
        Self::Cancelled,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Queued => "queued",
            Self::Asr => "asr",
            Self::Highlight => "highlight",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|item| item.as_str() == value)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AiSmartStageAttemptStatus {
    Pending,
    Running,
    Completed,
    Interrupted,
    Cancelled,
    Failed,
}

impl AiSmartStageAttemptStatus {
    pub const ALL: [Self; 6] = [
        Self::Pending,
        Self::Running,
        Self::Completed,
        Self::Interrupted,
        Self::Cancelled,
        Self::Failed,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Interrupted => "interrupted",
            Self::Cancelled => "cancelled",
            Self::Failed => "failed",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|item| item.as_str() == value)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AiSmartDraftOwnership {
    Automation,
    User,
}

impl AiSmartDraftOwnership {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Automation => "automation",
            Self::User => "user",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "automation" => Some(Self::Automation),
            "user" => Some(Self::User),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AiSmartDraftStatus {
    Active,
    ReviewReady,
    Exporting,
    Frozen,
    Exported,
    Failed,
    Superseded,
}

impl AiSmartDraftStatus {
    pub const ALL: [Self; 7] = [
        Self::Active,
        Self::ReviewReady,
        Self::Exporting,
        Self::Frozen,
        Self::Exported,
        Self::Failed,
        Self::Superseded,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::ReviewReady => "review_ready",
            Self::Exporting => "exporting",
            Self::Frozen => "frozen",
            Self::Exported => "exported",
            Self::Failed => "failed",
            Self::Superseded => "superseded",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|item| item.as_str() == value)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AiSmartWorkflow {
    pub id: i64,
    pub name: String,
    pub mode: AiSmartWorkflowMode,
    pub status: AiSmartWorkflowStatus,
    pub stage: AiSmartStage,
    pub generation: u32,
    pub source_session_id: Option<i64>,
    pub source_summary: String,
    pub provider: String,
    pub model_id: String,
    pub text_scope: String,
    pub authorization_digest: Option<String>,
    pub authorized_at: Option<String>,
    pub configuration_fingerprint: String,
    pub active_draft_generation: Option<u32>,
    pub live_cursor_video_id: Option<i64>,
    pub live_start_video_id: Option<i64>,
    pub event_sequence: u64,
    pub candidate_count: u64,
    pub selected_count: u64,
    pub pending_batch_count: u64,
    pub last_error_code: Option<String>,
    pub last_error_message: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AiSmartWorkflowBatch {
    pub id: i64,
    pub workflow_id: i64,
    pub position: u32,
    pub video_id: Option<i64>,
    pub source_fingerprint: String,
    pub project_id: Option<i64>,
    pub highlight_run_id: Option<i64>,
    pub status: AiSmartBatchStatus,
    pub finalized_at: String,
    pub last_error_code: Option<String>,
    pub last_error_message: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AiSmartStageAttempt {
    pub id: i64,
    pub workflow_id: i64,
    pub batch_id: Option<i64>,
    pub draft_generation: Option<u32>,
    pub stage: AiSmartStage,
    pub input_fingerprint: String,
    pub attempt_generation: u32,
    pub status: AiSmartStageAttemptStatus,
    pub progress: u8,
    pub result_kind: Option<String>,
    pub result_id: Option<i64>,
    pub duration_ms: Option<u64>,
    pub last_error_code: Option<String>,
    pub last_error_message: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AiSmartDraft {
    pub id: i64,
    pub workflow_id: i64,
    pub generation: u32,
    pub clip_project_id: i64,
    pub ownership: AiSmartDraftOwnership,
    pub status: AiSmartDraftStatus,
    pub automation_project_version: u32,
    pub frozen_project_version: Option<u32>,
    pub first_reviewable_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AiClipProjectSource {
    pub clip_project_id: i64,
    pub highlight_run_id: i64,
    pub workflow_batch_id: Option<i64>,
    pub source_order: u32,
    pub primary_source: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NewAiSmartWorkflow {
    pub name: String,
    pub mode: AiSmartWorkflowMode,
    pub source_session_id: Option<i64>,
    pub source_summary: String,
    pub provider: String,
    pub model_id: String,
    pub text_scope: String,
    pub configuration_fingerprint: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NewAiSmartWorkflowBatch {
    pub video_id: Option<i64>,
    pub source_fingerprint: String,
    pub finalized_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AiSmartWorkflowDetail {
    pub workflow: AiSmartWorkflow,
    pub batches: Vec<AiSmartWorkflowBatch>,
    pub attempts: Vec<AiSmartStageAttempt>,
    pub drafts: Vec<AiSmartDraft>,
    pub frozen_input_count: u64,
    pub processed_input_count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SmartWorkflowConfiguration {
    pub name: String,
    pub provider: String,
    pub model_id: String,
    pub text_scope: String,
    pub output_preference: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateLocalSmartWorkflowInput {
    pub configuration: SmartWorkflowConfiguration,
    pub grant_ids: Vec<String>,
    pub authorization_confirmed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateLiveSmartWorkflowInput {
    pub configuration: SmartWorkflowConfiguration,
    pub session_id: i64,
    pub authorization_confirmed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateReplaySmartWorkflowInput {
    pub configuration: SmartWorkflowConfiguration,
    pub session_id: i64,
    pub authorization_confirmed: bool,
    pub duplicate_confirmed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AuthorizeSmartWorkflowInput {
    pub workflow_id: i64,
    pub expected_generation: u32,
    pub configuration_fingerprint: String,
    pub authorization_confirmed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RetrySmartWorkflowStageInput {
    pub workflow_id: i64,
    pub expected_generation: u32,
    pub stage: AiSmartStage,
    pub batch_id: Option<i64>,
}

/// 事件只携带可公开的状态定位信息；完整状态始终通过快照命令恢复。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AiSmartWorkflowEvent {
    pub workflow_id: i64,
    pub workflow_generation: u32,
    pub batch_id: Option<i64>,
    pub stage: AiSmartStage,
    pub sequence: u64,
    pub status: AiSmartWorkflowStatus,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
}

/// 只允许发送枚举、桶和聚合计数；结构中刻意不存在正文、路径或来源标识字段。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AiSmartWorkflowMetric {
    pub mode: String,
    pub platform: String,
    pub stage: String,
    pub terminal_state: String,
    pub duration_bucket: Option<String>,
    pub batch_count: u64,
    pub candidate_count: u64,
    pub selected_count: u64,
    pub first_draft_latency_bucket: Option<String>,
    pub first_draft_within_ten_minutes: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AiActiveLiveSession {
    pub session_id: i64,
    pub streamer_name: String,
    pub started_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AiSmartReplaySessionCursor {
    pub started_at: String,
    pub session_id: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AiSmartReplaySession {
    pub session_id: i64,
    pub streamer_name: String,
    pub started_at: String,
    pub ended_at: String,
    pub video_count: usize,
    pub total_duration_ms: u64,
    pub unavailable_video_count: usize,
    pub existing_workflow_id: Option<i64>,
    pub existing_workflow_status: Option<AiSmartWorkflowStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AiSmartReplaySessionPage {
    pub items: Vec<AiSmartReplaySession>,
    pub next_cursor: Option<AiSmartReplaySessionCursor>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AiSmartCandidate {
    pub id: i64,
    pub workflow_id: i64,
    pub canonical_candidate_id: i64,
    pub total_score: u8,
    pub qualified: bool,
    pub selected: bool,
    pub session_start_ms: u64,
    pub session_end_ms: u64,
    pub first_finalized_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AiSmartCandidateSource {
    pub id: i64,
    pub smart_candidate_id: i64,
    pub batch_id: i64,
    pub candidate_id: i64,
    pub video_id: Option<i64>,
    pub input_id: i64,
    pub stable_segment_ids: Vec<String>,
    pub source_start_ms: u64,
    pub source_end_ms: u64,
    pub session_start_ms: u64,
    pub session_end_ms: u64,
    pub source_order: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NewAiSmartCandidateSource {
    pub batch_id: i64,
    pub candidate_id: i64,
    pub video_id: Option<i64>,
    pub input_id: i64,
    pub stable_segment_ids: Vec<String>,
    pub source_start_ms: u64,
    pub source_end_ms: u64,
    pub session_start_ms: u64,
    pub session_end_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NewAiSmartCandidate {
    pub dedupe_key: String,
    pub semantic_fingerprint: String,
    pub canonical_candidate_id: i64,
    pub total_score: u8,
    pub qualified: bool,
    pub selected: bool,
    pub session_start_ms: u64,
    pub session_end_ms: u64,
    pub first_finalized_at: String,
    pub sources: Vec<NewAiSmartCandidateSource>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AiSmartClipSourceInput {
    pub highlight_run_id: i64,
    pub workflow_batch_id: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AiClipSegment {
    pub id: i64,
    pub clip_project_id: i64,
    pub candidate_id: i64,
    pub input_id: i64,
    pub position: u32,
    pub title: String,
    pub source_start_ms: u64,
    pub source_end_ms: u64,
    pub volume_percent: u16,
    pub effect: AiClipEffect,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AiClipSubtitle {
    pub id: i64,
    pub clip_project_id: i64,
    pub stable_segment_id: String,
    pub clip_segment_id: i64,
    pub input_id: i64,
    pub original_text: String,
    pub text: String,
    pub hidden: bool,
    pub source_start_ms: u64,
    pub source_end_ms: u64,
    pub project_start_ms: u64,
    pub project_end_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AiClipSubtitleFrame {
    pub subtitle_id: i64,
    pub clip_segment_id: i64,
    pub project_start_ms: u64,
    pub project_end_ms: u64,
    pub page_text: String,
    pub visible_text: String,
    pub hidden_text: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AiClipTimelineUnitKind {
    Segment,
    Bridge,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AiClipTimelineUnit {
    pub key: String,
    pub kind: AiClipTimelineUnitKind,
    pub project_start_ms: u64,
    pub project_end_ms: u64,
    pub clip_segment_id: Option<i64>,
    pub boundary_id: Option<i64>,
    pub title: String,
    pub asset_key: Option<String>,
    pub asset_version: Option<i64>,
    pub source_status: Option<crate::transition_materials::MaterialDownloadStatus>,
    pub preview_status: Option<crate::transition_materials::MaterialDownloadStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AiClipProjectDetail {
    pub project: AiClipProject,
    pub segments: Vec<AiClipSegment>,
    pub subtitles: Vec<AiClipSubtitle>,
    pub subtitle_frames: Vec<AiClipSubtitleFrame>,
    pub subtitles_complete: bool,
    pub boundaries: Vec<crate::transition_materials::ClipTransitionBoundary>,
    pub project_duration_ms: u64,
    pub timeline_units: Vec<AiClipTimelineUnit>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AiClipSegmentUpdate {
    pub volume_percent: u16,
    pub effect: AiClipEffect,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AiClipSubtitleUpdate {
    pub text: String,
    pub hidden: bool,
    pub expected_project_version: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ClipTextCorrectionSummary {
    pub run_id: i64,
    pub processed: usize,
    pub changed: usize,
    pub unchanged: usize,
    pub skipped_manual: usize,
    pub skipped_hidden: usize,
    pub total_batches: usize,
    pub token_usage: u64,
    pub detail: AiClipProjectDetail,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct NewAiHighlightRun {
    pub project_id: i64,
    pub model_id: String,
    pub prompt_version: String,
    pub tags_snapshot: Vec<String>,
    pub skills_snapshot: Vec<String>,
    pub analysis_goal: Option<String>,
    pub analysis_fingerprint: String,
    pub qualified_score: u8,
    pub excellent_score: u8,
    pub total_segments: u64,
    pub total_chars: u64,
    pub estimated_batches: u64,
    pub user_authorized: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct NewAiHighlightChunk {
    pub ordinal: i64,
    pub input_id: i64,
    pub segment_ids: Vec<String>,
    pub context_segment_ids: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RecoverySummary {
    pub projects: usize,
    pub inputs: usize,
    pub artifacts: usize,
}
