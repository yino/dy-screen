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
            _ => Err(AiRepositoryError::Integrity("精彩运行状态无效".to_owned())),
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
                // 音频准备完成后可以直接进入转写：云端路径不执行 VAD，
                // 因此不会经过 DetectingSpeech；本地路径仍按原顺序经过它。
                Self::PreparingAudio,
                Self::DetectingSpeech | Self::Transcribing | Self::Cancelled | Self::Failed
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
    /// 实际调用路径，例如 `cloud` 或 `local`。
    ///
    /// `skip_serializing_if` 是**兼容性要求**：升级前写入的识别配置没有该字段，
    /// 反序列化为 `None` 后必须序列化回完全相同的 JSON，否则既有工程的全部缓存
    /// 指纹都会变化并被误判为需要重新识别。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// 分块策略标识；`None` 表示未分块。
    ///
    /// 与 `provider` 同为兼容字段：参与指纹但不得改变旧配置的指纹。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chunk_strategy: Option<String>,
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
    /// 实际使用的识别 provider：`local` | `aliyun`。
    pub provider: String,
    /// 云端不可用时的脱敏回退原因；直接走本地时为空。
    pub fallback_reason: Option<String>,
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
    /// 实际使用的识别 provider：`local` | `aliyun`。
    pub provider: String,
    /// 云端不可用时的脱敏回退原因。
    pub fallback_reason: Option<String>,
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
    /// 文案纠正（**片段级前置**）。
    ///
    /// 位于 `Draft` **之前**：校正在拼合辑之前作用于转写句段，合辑字幕快照生成时
    /// 自动采用纠正后的文本（见 `migrate_ai_v32`）。这样用户在看合辑之前就能拿到
    /// 纠正过的文案，而不是先拼合辑再回头纠正。
    Correction,
    Draft,
    Transition,
    /// 表现形式增强（特效字幕）：位于转场匹配之后、评审之前，且**不阻塞**基础草稿。
    Enhancement,
    Review,
}

impl AiSmartStage {
    pub const ALL: [Self; 8] = [
        Self::Preflight,
        Self::Asr,
        Self::Highlight,
        Self::Correction,
        Self::Draft,
        Self::Transition,
        Self::Enhancement,
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
            Self::Enhancement => "enhancement",
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
    /// 生成式动画单元（`insert` 合成方式）：与桥接同样占用时间轴、增加工程总时长。
    Animation,
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
    /// 生成资产 ID（仅 `Animation` 单元有值）。
    pub generated_asset_id: Option<i64>,
    /// 合成方式（仅 `Animation` 单元有值）。
    pub composition: Option<AiGeneratedAnimationComposition>,
    /// 资产校验状态（仅 `Animation` 单元有值）。
    pub asset_status: Option<AiGeneratedAnimationStatus>,
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
    /// 重启后需要向上游核对状态的付费任务数量（见 `pending_remote_jobs`）。
    ///
    /// 这些任务已提交但未确认终态，恢复时必须先查询上游，MUST NOT 直接重新提交。
    #[serde(default)]
    pub awaiting_remote_reconcile: usize,
}

/// 高光表现形式增强的方式（步骤 3 任务 1.1）。
///
/// `video_animation` 在本 change 中只保留枚举位，不实现生成路径；未知取值必须被拒绝，
/// 以免上游新增类型时被静默当成已知方式处理。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AiEnhancementKind {
    EffectCaption,
    VideoAnimation,
}

impl AiEnhancementKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::EffectCaption => "effect_caption",
            Self::VideoAnimation => "video_animation",
        }
    }

    pub(crate) fn parse(value: &str) -> Result<Self> {
        match value {
            "effect_caption" => Ok(Self::EffectCaption),
            "video_animation" => Ok(Self::VideoAnimation),
            _ => Err(AiRepositoryError::Integrity("增强方式无效".to_owned())),
        }
    }
}

/// 增强结果的代次状态（步骤 3 任务 1.5）。
///
/// 用户修改工程后到达的旧代次结果只能停在 `suggested`，不得覆盖人工编辑。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AiEnhancementStatus {
    Suggested,
    Applied,
    Rejected,
    Superseded,
}

impl AiEnhancementStatus {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Suggested => "suggested",
            Self::Applied => "applied",
            Self::Rejected => "rejected",
            Self::Superseded => "superseded",
        }
    }

    pub(crate) fn parse(value: &str) -> Result<Self> {
        match value {
            "suggested" => Ok(Self::Suggested),
            "applied" => Ok(Self::Applied),
            "rejected" => Ok(Self::Rejected),
            "superseded" => Ok(Self::Superseded),
            _ => Err(AiRepositoryError::Integrity("增强状态无效".to_owned())),
        }
    }
}

/// 被特效字幕命中的时间区间。区间 MUST 由工程已发布句段确定，不得采用模型给出的时间。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AiSuppressionRange {
    pub start_ms: u64,
    pub end_ms: u64,
}

/// 一条高光的表现形式增强记录。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AiHighlightEnhancement {
    pub id: i64,
    pub project_id: i64,
    pub candidate_id: i64,
    pub highlight_run_id: Option<i64>,
    pub generation: i64,
    pub kind: AiEnhancementKind,
    pub status: AiEnhancementStatus,
    pub start_ms: u64,
    pub end_ms: u64,
    pub emphasis_words: Vec<String>,
    pub composition_version: Option<String>,
    pub composition_fingerprint: Option<String>,
    pub render_cache_key: Option<String>,
    /// 生成该建议时所依据的工程已发布内容指纹；与当前指纹不符即为过期代次。
    pub source_fingerprint: String,
    pub suppression_ranges: Vec<AiSuppressionRange>,
    pub rejection_code: Option<String>,
    pub rejection_detail: Option<String>,
    /// 该高光已产生的上游成本（分）：只有按次付费的生成分支会累加，字幕分支与无增强为 0。
    pub cost_minor: u64,
    pub created_at: String,
    pub updated_at: String,
}

/// 写入增强记录所需的字段。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct NewAiHighlightEnhancement {
    pub project_id: i64,
    pub candidate_id: i64,
    pub highlight_run_id: Option<i64>,
    pub generation: i64,
    pub kind: AiEnhancementKind,
    pub status: AiEnhancementStatus,
    pub start_ms: u64,
    pub end_ms: u64,
    pub emphasis_words: Vec<String>,
    pub composition_version: Option<String>,
    pub composition_fingerprint: Option<String>,
    pub render_cache_key: Option<String>,
    pub source_fingerprint: String,
    pub suppression_ranges: Vec<AiSuppressionRange>,
    pub rejection_code: Option<String>,
    pub rejection_detail: Option<String>,
    /// 该高光已产生的上游成本（分）。
    pub cost_minor: u64,
}

/// 生成式动画的合成方式（步骤 4 任务 1.1）。
///
/// `replace` 等长替换高光内的源画面范围，工程总时长不变；`insert` 作为独立时间轴
/// 单元插入边界，增加工程总时长并让右侧片段与其字幕顺延。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AiGeneratedAnimationComposition {
    Replace,
    Insert,
}

impl AiGeneratedAnimationComposition {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Replace => "replace",
            Self::Insert => "insert",
        }
    }

    pub(crate) fn parse(value: &str) -> Result<Self> {
        match value {
            "replace" => Ok(Self::Replace),
            "insert" => Ok(Self::Insert),
            _ => Err(AiRepositoryError::Integrity("生成动画合成方式无效".to_owned())),
        }
    }
}

/// 生成式动画资产的校验状态（步骤 4 任务 1.1 / 任务组 3）。
///
/// 只有 `Validated` 才允许进入合成与导出；`Invalidated` 专指 `insert` 因左右邻接
/// 关系改变而失效，必须重新审阅且 MUST NOT 自动重新生成。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AiGeneratedAnimationStatus {
    /// 已提交上游，等待生成与下载。
    Pending,
    /// 已下载并通过哈希、尺寸、帧率与实际时长校验。
    Validated,
    /// 生成失败或媒体校验未通过，转入降级。
    Rejected,
    /// 邻接绑定失效（仅 `insert`），需重新审阅。
    Invalidated,
}

impl AiGeneratedAnimationStatus {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Validated => "validated",
            Self::Rejected => "rejected",
            Self::Invalidated => "invalidated",
        }
    }

    pub(crate) fn parse(value: &str) -> Result<Self> {
        match value {
            "pending" => Ok(Self::Pending),
            "validated" => Ok(Self::Validated),
            "rejected" => Ok(Self::Rejected),
            "invalidated" => Ok(Self::Invalidated),
            _ => Err(AiRepositoryError::Integrity("生成动画状态无效".to_owned())),
        }
    }
}

/// `replace` 的目标源时间范围（高光来源区间内，不得跨来源片段）。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AiGeneratedAnimationReplaceRange {
    pub start_ms: u64,
    pub end_ms: u64,
}

/// `insert` 的稳定邻接绑定：左片段、右片段与边界三元组，归属左侧高光。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AiGeneratedAnimationInsertBinding {
    pub left_segment_id: i64,
    pub right_segment_id: i64,
    pub boundary_id: i64,
}

/// 生成式动画资产（步骤 4 任务 1.1）。
///
/// 生成资产是单工程单高光的产物：它 MUST NOT 写入目录素材表、不产生 `renderMode`、
/// 不被边界召回作为候选。媒体属性在下载校验通过后才有值。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AiGeneratedAnimationAsset {
    pub id: i64,
    pub project_id: i64,
    pub candidate_id: i64,
    pub highlight_run_id: Option<i64>,
    pub generation: i64,
    /// 复用键：分镜内容 + 提示词 + 目标时长 + 生成规格，全部一致才复用。
    pub reuse_key: String,
    pub composition: AiGeneratedAnimationComposition,
    pub status: AiGeneratedAnimationStatus,
    /// `replace` 的目标源范围；`insert` 时为 `None`。
    pub replace_range: Option<AiGeneratedAnimationReplaceRange>,
    /// `insert` 的邻接绑定；`replace` 时为 `None`。
    pub insert_binding: Option<AiGeneratedAnimationInsertBinding>,
    /// 上游业务任务 ID（不透明，非凭据、非下载地址）。
    pub upstream_task_id: Option<String>,
    /// 校验通过后写入的媒体属性。
    pub sha256: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub fps: Option<u32>,
    pub duration_ms: Option<u64>,
    pub rejection_code: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// 写入生成式动画资产所需的字段。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct NewAiGeneratedAnimationAsset {
    pub project_id: i64,
    pub candidate_id: i64,
    pub highlight_run_id: Option<i64>,
    pub generation: i64,
    pub reuse_key: String,
    pub composition: AiGeneratedAnimationComposition,
    pub status: AiGeneratedAnimationStatus,
    pub replace_range: Option<AiGeneratedAnimationReplaceRange>,
    pub insert_binding: Option<AiGeneratedAnimationInsertBinding>,
    pub upstream_task_id: Option<String>,
    pub sha256: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub fps: Option<u32>,
    pub duration_ms: Option<u64>,
    pub rejection_code: Option<String>,
}

/// 生成资产的上游任务类型（用于本地幂等与记账分类）。
pub const GENERATED_ANIMATION_IMAGE_TASK_TYPE: &str = "wanx_text2image";
/// 图生视频任务类型。
pub const GENERATED_ANIMATION_VIDEO_TASK_TYPE: &str = "wanx_image2video";

/// 生成资产复用键的输入：分镜内容、提示词、目标时长与生成规格。
///
/// 全部字段一致才复用；任一变化都视为新任务（可能产生新的上游费用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GeneratedAnimationReuseInput<'a> {
    /// 分镜内容（画面主体、构图、镜头运动等结构化描述）。
    pub storyboard: &'a str,
    /// 参考图（文生图）提示词。
    pub image_prompt: &'a str,
    /// 图生视频提示词。
    pub video_prompt: &'a str,
    /// 目标时长毫秒。
    pub target_duration_ms: u64,
    /// 生成规格指纹（模型、尺寸、帧率等冻结取值）。
    pub spec_fingerprint: &'a str,
}

/// 计算生成资产复用键：由分镜内容、提示词、目标时长与生成规格共同构成。
///
/// 每个字段先写长度前缀再写内容，避免不同字段组合产生相同拼接串。
pub fn generated_animation_reuse_key(input: &GeneratedAnimationReuseInput<'_>) -> String {
    let mut hasher = Sha256::new();
    for part in [
        input.storyboard,
        input.image_prompt,
        input.video_prompt,
        input.spec_fingerprint,
    ] {
        hasher.update((part.len() as u64).to_le_bytes());
        hasher.update(part.as_bytes());
    }
    hasher.update(input.target_duration_ms.to_le_bytes());
    hex::encode(hasher.finalize())
}

/// 由复用键派生稳定请求标识：同一复用键必须得到同一 `requestId`。
///
/// 该标识是本地幂等的承担者（上游不具备调用方幂等能力），必须在付费提交前落盘。
pub fn generated_animation_request_id(reuse_key: &str) -> String {
    let digest = Sha256::digest(reuse_key.as_bytes());
    format!("wanx-{}", hex::encode(&digest[..16]))
}

/// 冻结的生成时长容差：5%（千分比 50）。
pub const GENERATED_ANIMATION_DURATION_TOLERANCE_PERMILLE: u16 = 50;

/// 生成资产缓存目录名（步骤 4 任务 3.1）。
///
/// **本地路径不入库**：由应用数据目录 + 资产 ID 推导文件名，因此清理缓存既不影响
/// 已发布工程，也不会让数据库留下失效路径（重新导出时按需重新下载）。
pub const GENERATED_ANIMATION_CACHE_DIR: &str = "generated-animations";

/// 生成资产在缓存目录中的文件名约定。
pub fn generated_animation_file_name(asset_id: i64) -> String {
    format!("animation-{asset_id}.mp4")
}

/// 高光内一个可用来源区间（由工程已发布句段在该来源片段内确定）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReplaceSourceRange {
    pub start_ms: u64,
    pub end_ms: u64,
}

/// `replace` 目标源范围的计算结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplaceTargetRange {
    /// 命中一个来源区间；范围必须完整落在该单区间内。
    Matched(AiGeneratedAnimationReplaceRange),
    /// 没有任何单区间能在冻结容差内匹配生成时长，调用方必须转入降级。
    Unmatched(&'static str),
}

/// 依据来源区间与生成时长反向确定 `replace` 的目标源范围（决策一）。
///
/// 规则：
/// - 目标范围 MUST 完整落在**单个**来源区间内，不得跨来源片段；
/// - 优先取等于生成时长的一段；区间略短但差异仍在 5% 容差内时退化为整个区间；
/// - 差异超过容差即 `Unmatched`（调用方转入降级，MUST NOT 补帧凑时长）；
/// - MUST NOT 采用模型给出的时间戳。
pub fn resolve_replace_target_range(
    ranges: &[ReplaceSourceRange],
    generated_duration_ms: u64,
) -> ReplaceTargetRange {
    if generated_duration_ms == 0 {
        return ReplaceTargetRange::Unmatched("generated_duration_missing");
    }
    let tolerance = u64::from(GENERATED_ANIMATION_DURATION_TOLERANCE_PERMILLE);
    for range in ranges {
        if range.end_ms <= range.start_ms {
            continue;
        }
        let available = range.end_ms - range.start_ms;
        let target_len = generated_duration_ms.min(available);
        let difference = generated_duration_ms.abs_diff(target_len);
        // 千分比比较，避免浮点：difference / duration <= tolerance / 1000
        if difference.saturating_mul(1_000) > generated_duration_ms.saturating_mul(tolerance) {
            continue;
        }
        return ReplaceTargetRange::Matched(AiGeneratedAnimationReplaceRange {
            start_ms: range.start_ms,
            end_ms: range.start_ms + target_len,
        });
    }
    ReplaceTargetRange::Unmatched("no_source_range_within_tolerance")
}

/// 判断 `insert` 的邻接绑定在当前片段顺序下是否仍然成立（步骤 4 任务 5.7）。
///
/// 任一重排、移除或追加导致左右邻接变化即失效：该动画必须重新审阅，
/// MUST NOT 自动重新生成（重新生成会产生新的付费调用）。
pub fn insert_binding_is_valid(
    binding: &AiGeneratedAnimationInsertBinding,
    segment_ids: &[i64],
) -> bool {
    segment_ids.windows(2).any(|pair| {
        pair[0] == binding.left_segment_id && pair[1] == binding.right_segment_id
    })
}

/// 冻结的受限放慢上限：最多 1.03x（1030‰）；超出即按超容差处理，
/// MUST NOT 用大幅变速掩盖时长不匹配。
pub const GENERATED_ANIMATION_MAX_SLOWDOWN_PERMILLE: u16 = 1_030;

/// 生成时长与目标范围的适配方案（步骤 4 任务组 6 / 决策四）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeneratedDurationAdaptation {
    /// 生成不短于目标：裁剪多余帧，精确对齐目标。
    Trim,
    /// 生成略短于目标：在受限范围内放慢，`slowdown_permille` 为放慢比例千分比。
    SlowDown { slowdown_permille: u16 },
    /// 差异超容差或所需变速超受限范围：失败并降级，MUST NOT 补黑帧或长时间冻结画面。
    Rejected(&'static str),
}

/// 依据 5% 容差与冻结的受限变速范围适配生成时长（决策四）。
///
/// - 生成不短于目标且差异在容差内 → 裁剪多余帧；
/// - 生成短于目标且放慢比例在受限范围内 → 施加受限变速；
/// - 其余情况（超容差 / 超受限变速）→ `Rejected`，调用方必须转入降级。
pub fn adapt_generated_duration(
    generated_duration_ms: u64,
    target_duration_ms: u64,
) -> GeneratedDurationAdaptation {
    if generated_duration_ms == 0 || target_duration_ms == 0 {
        return GeneratedDurationAdaptation::Rejected("invalid_duration");
    }
    let tolerance = u64::from(GENERATED_ANIMATION_DURATION_TOLERANCE_PERMILLE);
    let difference = generated_duration_ms.abs_diff(target_duration_ms);
    if difference.saturating_mul(1_000) > target_duration_ms.saturating_mul(tolerance) {
        return GeneratedDurationAdaptation::Rejected("over_tolerance");
    }
    if generated_duration_ms >= target_duration_ms {
        return GeneratedDurationAdaptation::Trim;
    }
    let slowdown_permille =
        ((target_duration_ms as f64 / generated_duration_ms as f64) * 1_000.0).round() as u32;
    if slowdown_permille > u32::from(GENERATED_ANIMATION_MAX_SLOWDOWN_PERMILLE) {
        return GeneratedDurationAdaptation::Rejected("slowdown_out_of_range");
    }
    GeneratedDurationAdaptation::SlowDown {
        slowdown_permille: u16::try_from(slowdown_permille).unwrap_or(u16::MAX),
    }
}

/// 校验同一高光最终只保留一种"可导出"的增强方式（步骤 4 任务组 7 / 决策五）。
///
/// 返回第一个同时存在 `effect_caption` 与 `video_animation` 两条 `applied` 记录的候选；
/// 其他状态（`suggested` / `rejected` / `superseded`）不是终态，不参与冲突判定。
/// MUST NOT 让两条分支同时产出可导出内容。
pub fn find_enhancement_kind_conflict(
    enforcements: &[AiHighlightEnhancement],
) -> Option<i64> {
    for (index, left) in enforcements.iter().enumerate() {
        if left.status != AiEnhancementStatus::Applied {
            continue;
        }
        for right in enforcements.iter().skip(index + 1) {
            if right.status == AiEnhancementStatus::Applied
                && right.candidate_id == left.candidate_id
                && right.kind != left.kind
            {
                return Some(left.candidate_id);
            }
        }
    }
    None
}

/// 从候选集合中剔除已分配 `video_animation` 的高光（任务 7.1）。
///
/// 增强方式互斥：动画高光 MUST NOT 生成或渲染特效字幕，普通 ASR 字幕仍按既有规则显示。
pub fn candidates_eligible_for_effect_caption(
    candidate_ids: &[i64],
    enforcements: &[AiHighlightEnhancement],
) -> Vec<i64> {
    candidate_ids
        .iter()
        .copied()
        .filter(|candidate_id| {
            !enforcements.iter().any(|enhancement| {
                enhancement.candidate_id == *candidate_id
                    && enhancement.kind == AiEnhancementKind::VideoAnimation
            })
        })
        .collect()
}

/// 单条高光的增强审阅摘要（步骤 4 任务 9.1）。
///
/// 只含结构化字段：不含 Prompt 正文、资产下载地址或 Provider 原始响应。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AiEnhancementReviewItem {
    pub candidate_id: i64,
    /// 最终唯一的增强方式；尚无结果时为 `None`（进行中）。
    pub kind: Option<AiEnhancementKind>,
    pub status: Option<AiEnhancementStatus>,
    /// 合成方式（仅 `video_animation` 有值）。
    pub composition: Option<AiGeneratedAnimationComposition>,
    /// 生成资产校验状态（仅 `video_animation` 有值）。
    pub asset_status: Option<AiGeneratedAnimationStatus>,
    /// 降级原因码。
    pub rejection_code: Option<String>,
    /// 已产生成本（分）。
    pub cost_minor: u64,
}

/// 组装按高光的增强审阅摘要（步骤 4 任务 9.1 / 9.2 / 9.3）。
///
/// 已就绪的高光展示最终增强方式、合成方式、资产校验状态、降级原因与已产生成本；
/// 尚无结果的高光以 `kind = None` 表示"进行中"，**不阻塞**其他高光的审阅。
pub fn enhancement_review_items(
    candidate_ids: &[i64],
    enforcements: &[AiHighlightEnhancement],
    assets: &[AiGeneratedAnimationAsset],
) -> Vec<AiEnhancementReviewItem> {
    candidate_ids
        .iter()
        .map(|candidate_id| {
            let enforcement = enforcements
                .iter()
                .find(|item| {
                    item.candidate_id == *candidate_id
                        && item.status == AiEnhancementStatus::Applied
                })
                .or_else(|| {
                    enforcements
                        .iter()
                        .find(|item| item.candidate_id == *candidate_id)
                });
            match enforcement {
                None => AiEnhancementReviewItem {
                    candidate_id: *candidate_id,
                    kind: None,
                    status: None,
                    composition: None,
                    asset_status: None,
                    rejection_code: None,
                    cost_minor: 0,
                },
                Some(enforcement) => {
                    let asset = assets
                        .iter()
                        .find(|asset| asset.candidate_id == *candidate_id);
                    AiEnhancementReviewItem {
                        candidate_id: *candidate_id,
                        kind: Some(enforcement.kind),
                        status: Some(enforcement.status),
                        composition: asset.map(|asset| asset.composition),
                        asset_status: asset.map(|asset| asset.status),
                        rejection_code: enforcement.rejection_code.clone(),
                        cost_minor: enforcement.cost_minor,
                    }
                }
            }
        })
        .collect()
}

/// 渲染尝试状态（步骤 3 任务组 9）。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AiRenderAttemptStatus {
    Rendering,
    Ready,
    Failed,
}

impl AiRenderAttemptStatus {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Rendering => "rendering",
            Self::Ready => "ready",
            Self::Failed => "failed",
        }
    }

    pub(crate) fn parse(value: &str) -> Result<Self> {
        match value {
            "rendering" => Ok(Self::Rendering),
            "ready" => Ok(Self::Ready),
            "failed" => Ok(Self::Failed),
            _ => Err(AiRepositoryError::Integrity("渲染尝试状态无效".to_owned())),
        }
    }
}

/// 一次特效字幕渲染尝试。`cache_key` 相同即表示帧序列可以复用。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AiEffectCaptionRenderAttempt {
    pub id: i64,
    pub project_id: i64,
    pub candidate_id: i64,
    pub cache_key: String,
    pub composition_version: String,
    pub composition_fingerprint: String,
    pub font_version: String,
    pub browser_version: String,
    pub renderer_version: String,
    pub engine_script_version: String,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub frame_count: u64,
    pub frames_dir: String,
    pub status: AiRenderAttemptStatus,
    pub failure_code: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// 写入渲染尝试所需的字段。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct NewAiEffectCaptionRenderAttempt {
    pub project_id: i64,
    pub candidate_id: i64,
    pub cache_key: String,
    pub composition_version: String,
    pub composition_fingerprint: String,
    pub font_version: String,
    pub browser_version: String,
    pub renderer_version: String,
    pub engine_script_version: String,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub frames_dir: String,
}

#[cfg(test)]
mod generated_animation_reuse_tests {
    use super::*;

    fn reuse_input<'a>(
        storyboard: &'a str,
        image_prompt: &'a str,
        video_prompt: &'a str,
        duration: u64,
        spec: &'a str,
    ) -> GeneratedAnimationReuseInput<'a> {
        GeneratedAnimationReuseInput {
            storyboard,
            image_prompt,
            video_prompt,
            target_duration_ms: duration,
            spec_fingerprint: spec,
        }
    }

    #[test]
    fn reuse_key_is_stable_and_changes_on_any_field() {
        let base = reuse_input("分镜A", "图像提示", "视频提示", 5_000, "spec-v1");
        let key = generated_animation_reuse_key(&base);
        assert_eq!(key, generated_animation_reuse_key(&base), "同输入必须稳定");
        // 任一字段变化都必须产生不同的键（可能产生新费用）。
        for changed in [
            reuse_input("分镜B", "图像提示", "视频提示", 5_000, "spec-v1"),
            reuse_input("分镜A", "图像提示2", "视频提示", 5_000, "spec-v1"),
            reuse_input("分镜A", "图像提示", "视频提示2", 5_000, "spec-v1"),
            reuse_input("分镜A", "图像提示", "视频提示", 6_000, "spec-v1"),
            reuse_input("分镜A", "图像提示", "视频提示", 5_000, "spec-v2"),
        ] {
            assert_ne!(key, generated_animation_reuse_key(&changed));
        }
    }

    #[test]
    fn reuse_key_uses_length_prefixes_to_avoid_concatenation_ambiguity() {
        // ("ab","c") 与 ("a","bc") 直接拼接会相同，长度前缀必须让它们不同。
        let left = reuse_input("ab", "c", "x", 1_000, "s");
        let right = reuse_input("a", "bc", "x", 1_000, "s");
        assert_ne!(
            generated_animation_reuse_key(&left),
            generated_animation_reuse_key(&right)
        );
    }

    #[test]
    fn request_id_is_derived_from_reuse_key_and_stable() {
        let key = generated_animation_reuse_key(&reuse_input(
            "分镜A", "图像提示", "视频提示", 5_000, "spec-v1",
        ));
        let request_id = generated_animation_request_id(&key);
        assert_eq!(
            request_id,
            generated_animation_request_id(&key),
            "同一复用键必须得到同一 requestId"
        );
        assert!(request_id.starts_with("wanx-"));
        assert_ne!(
            request_id,
            generated_animation_request_id(&format!("{key}-x"))
        );
    }

    #[test]
    fn replace_target_range_prefers_a_single_source_range_never_crossing() {
        // 区间比生成时长长：取等于生成时长的一段，差异为 0。
        assert_eq!(
            resolve_replace_target_range(
                &[ReplaceSourceRange {
                    start_ms: 1_000,
                    end_ms: 20_000,
                }],
                5_000,
            ),
            ReplaceTargetRange::Matched(AiGeneratedAnimationReplaceRange {
                start_ms: 1_000,
                end_ms: 6_000,
            })
        );
        // 区间略短但差异在 5% 容差内：退化为整个区间。
        assert_eq!(
            resolve_replace_target_range(
                &[ReplaceSourceRange {
                    start_ms: 0,
                    end_ms: 4_900,
                }],
                5_000,
            ),
            ReplaceTargetRange::Matched(AiGeneratedAnimationReplaceRange {
                start_ms: 0,
                end_ms: 4_900,
            })
        );
        // 两个短区间相加够长，但任一单区间都不够：必须拒绝，不得跨片段拼接。
        assert_eq!(
            resolve_replace_target_range(
                &[
                    ReplaceSourceRange {
                        start_ms: 0,
                        end_ms: 3_000,
                    },
                    ReplaceSourceRange {
                        start_ms: 10_000,
                        end_ms: 13_000,
                    },
                ],
                5_000,
            ),
            ReplaceTargetRange::Unmatched("no_source_range_within_tolerance")
        );
    }

    #[test]
    fn replace_target_range_rejects_over_tolerance_and_missing_duration() {
        // 区间过短（差异 50%）：拒绝并转入降级。
        assert_eq!(
            resolve_replace_target_range(
                &[ReplaceSourceRange {
                    start_ms: 0,
                    end_ms: 2_500,
                }],
                5_000,
            ),
            ReplaceTargetRange::Unmatched("no_source_range_within_tolerance")
        );
        // 生成时长缺失：拒绝。
        assert_eq!(
            resolve_replace_target_range(
                &[ReplaceSourceRange {
                    start_ms: 0,
                    end_ms: 9_000,
                }],
                0,
            ),
            ReplaceTargetRange::Unmatched("generated_duration_missing")
        );
        // 空区间：跳过。
        assert_eq!(
            resolve_replace_target_range(
                &[ReplaceSourceRange {
                    start_ms: 5_000,
                    end_ms: 5_000,
                }],
                5_000,
            ),
            ReplaceTargetRange::Unmatched("no_source_range_within_tolerance")
        );
    }

    #[test]
    fn replace_target_range_falls_through_to_a_later_range() {
        // 第一个区间太短被跳过，第二个命中：证明是"逐个单区间"匹配。
        assert_eq!(
            resolve_replace_target_range(
                &[
                    ReplaceSourceRange {
                        start_ms: 0,
                        end_ms: 1_000,
                    },
                    ReplaceSourceRange {
                        start_ms: 8_000,
                        end_ms: 20_000,
                    },
                ],
                5_000,
            ),
            ReplaceTargetRange::Matched(AiGeneratedAnimationReplaceRange {
                start_ms: 8_000,
                end_ms: 13_000,
            })
        );
    }

    #[test]
    fn insert_binding_invalidates_when_left_right_adjacency_changes() {
        let binding = AiGeneratedAnimationInsertBinding {
            left_segment_id: 11,
            right_segment_id: 12,
            boundary_id: 7,
        };
        // 邻接成立。
        assert!(insert_binding_is_valid(&binding, &[10, 11, 12, 13]));
        // 重排后 11 与 12 不再相邻：失效。
        assert!(!insert_binding_is_valid(&binding, &[10, 11, 13, 12]));
        // 移除右片段：失效。
        assert!(!insert_binding_is_valid(&binding, &[10, 11, 13]));
        // 追加片段改变邻接：失效。
        assert!(!insert_binding_is_valid(&binding, &[11, 99, 12]));
        // 单片段工程不可能成立邻接。
        assert!(!insert_binding_is_valid(&binding, &[11]));
    }

    #[test]
    fn duration_adaptation_trims_when_generated_is_not_shorter() {
        assert_eq!(
            adapt_generated_duration(5_100, 5_000),
            GeneratedDurationAdaptation::Trim
        );
        assert_eq!(
            adapt_generated_duration(5_000, 5_000),
            GeneratedDurationAdaptation::Trim
        );
    }

    #[test]
    fn duration_adaptation_slows_down_within_the_limited_range() {
        assert_eq!(
            adapt_generated_duration(4_900, 5_000),
            GeneratedDurationAdaptation::SlowDown {
                slowdown_permille: 1_020
            }
        );
    }

    #[test]
    fn duration_adaptation_rejects_over_tolerance_without_padding_or_freezing() {
        assert_eq!(
            adapt_generated_duration(4_500, 5_000),
            GeneratedDurationAdaptation::Rejected("over_tolerance")
        );
        assert_eq!(
            adapt_generated_duration(5_500, 5_000),
            GeneratedDurationAdaptation::Rejected("over_tolerance")
        );
    }

    #[test]
    fn duration_adaptation_rejects_slowdown_beyond_the_frozen_range() {
        assert_eq!(
            adapt_generated_duration(4_850, 5_000),
            GeneratedDurationAdaptation::Rejected("slowdown_out_of_range")
        );
    }

    #[test]
    fn duration_adaptation_rejects_missing_duration() {
        assert_eq!(
            adapt_generated_duration(0, 5_000),
            GeneratedDurationAdaptation::Rejected("invalid_duration")
        );
        assert_eq!(
            adapt_generated_duration(5_000, 0),
            GeneratedDurationAdaptation::Rejected("invalid_duration")
        );
    }

    fn enhancement(
        candidate_id: i64,
        kind: AiEnhancementKind,
        status: AiEnhancementStatus,
    ) -> AiHighlightEnhancement {
        AiHighlightEnhancement {
            id: candidate_id,
            project_id: 1,
            candidate_id,
            highlight_run_id: None,
            generation: 1,
            kind,
            status,
            start_ms: 0,
            end_ms: 4_000,
            emphasis_words: Vec::new(),
            composition_version: None,
            composition_fingerprint: None,
            render_cache_key: None,
            source_fingerprint: "fp".to_owned(),
            suppression_ranges: Vec::new(),
            rejection_code: None,
            rejection_detail: None,
            cost_minor: 0,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    #[test]
    fn enhancement_kinds_are_mutually_exclusive_per_candidate() {
        // 同一候选两条 applied（不同 kind）：冲突，不得同时产出可导出内容。
        let conflict = vec![
            enhancement(
                42,
                AiEnhancementKind::EffectCaption,
                AiEnhancementStatus::Applied,
            ),
            enhancement(
                42,
                AiEnhancementKind::VideoAnimation,
                AiEnhancementStatus::Applied,
            ),
        ];
        assert_eq!(find_enhancement_kind_conflict(&conflict), Some(42));

        // 非终态不参与判定：suggested 的动画不影响 applied 的字幕分支。
        let safe = vec![
            enhancement(
                42,
                AiEnhancementKind::EffectCaption,
                AiEnhancementStatus::Applied,
            ),
            enhancement(
                42,
                AiEnhancementKind::VideoAnimation,
                AiEnhancementStatus::Suggested,
            ),
        ];
        assert_eq!(find_enhancement_kind_conflict(&safe), None);

        // 不同候选各自一条：不冲突。
        let distinct = vec![
            enhancement(
                1,
                AiEnhancementKind::EffectCaption,
                AiEnhancementStatus::Applied,
            ),
            enhancement(
                2,
                AiEnhancementKind::VideoAnimation,
                AiEnhancementStatus::Applied,
            ),
        ];
        assert_eq!(find_enhancement_kind_conflict(&distinct), None);
    }

    #[test]
    fn animated_highlights_are_excluded_from_effect_caption() {
        let enforcements = vec![
            enhancement(
                42,
                AiEnhancementKind::VideoAnimation,
                AiEnhancementStatus::Applied,
            ),
            enhancement(
                43,
                AiEnhancementKind::EffectCaption,
                AiEnhancementStatus::Applied,
            ),
        ];
        // 候选 42 已分配动画：不得再生成特效字幕；43 与 44 允许。
        assert_eq!(
            candidates_eligible_for_effect_caption(&[42, 43, 44], &enforcements),
            vec![43, 44]
        );
    }

    fn animation_asset(
        candidate_id: i64,
        status: AiGeneratedAnimationStatus,
    ) -> AiGeneratedAnimationAsset {
        AiGeneratedAnimationAsset {
            id: candidate_id,
            project_id: 1,
            candidate_id,
            highlight_run_id: None,
            generation: 1,
            reuse_key: "rk".to_owned(),
            composition: AiGeneratedAnimationComposition::Replace,
            status,
            replace_range: Some(AiGeneratedAnimationReplaceRange {
                start_ms: 0,
                end_ms: 5_000,
            }),
            insert_binding: None,
            upstream_task_id: Some("task".to_owned()),
            sha256: Some("a".repeat(64)),
            width: Some(768),
            height: Some(512),
            fps: Some(30),
            duration_ms: Some(5_000),
            rejection_code: None,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    #[test]
    fn enhancement_review_items_show_final_kind_composition_and_cost() {
        let enforcements = vec![{
            let mut item = enhancement(
                42,
                AiEnhancementKind::VideoAnimation,
                AiEnhancementStatus::Applied,
            );
            item.cost_minor = 250;
            item
        }];
        let assets = vec![animation_asset(42, AiGeneratedAnimationStatus::Validated)];
        let items = enhancement_review_items(&[42, 43], &enforcements, &assets);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].kind, Some(AiEnhancementKind::VideoAnimation));
        assert_eq!(
            items[0].composition,
            Some(AiGeneratedAnimationComposition::Replace)
        );
        assert_eq!(
            items[0].asset_status,
            Some(AiGeneratedAnimationStatus::Validated)
        );
        assert_eq!(items[0].cost_minor, 250);
        // 高光 43 尚无结果：以 kind=None 表示进行中，但不阻塞列表内其他高光。
        assert_eq!(items[1].kind, None);
        assert_eq!(items[1].cost_minor, 0);
    }

    #[test]
    fn enhancement_review_item_is_redacted_by_construction() {
        let enforcements = vec![{
            let mut item = enhancement(
                42,
                AiEnhancementKind::VideoAnimation,
                AiEnhancementStatus::Rejected,
            );
            item.rejection_code = Some("over_tolerance".to_owned());
            item.rejection_detail =
                Some("原始响应 https://oss.example/x?Signature=secret".to_owned());
            item
        }];
        let items = enhancement_review_items(&[42], &enforcements, &[]);
        let json = serde_json::to_string(&items).unwrap();
        // 只暴露原因码，不暴露 rejection_detail 正文或任何地址。
        assert!(json.contains("over_tolerance"));
        assert!(!json.contains("Signature"));
        assert!(!json.contains("https://"));
        assert!(!json.contains("rejectionDetail"));
    }
}
