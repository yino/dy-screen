export type LiveStatus = "checking" | "offline" | "live" | "error";

export type MonitorStatus =
  | "paused"
  | "waiting"
  | "discovering"
  | "waiting_first_live"
  | "profile_error"
  | "rediscovering"
  | "access_restricted"
  | "verification_required"
  | "layout_changed"
  | "entry_invalid"
  | "identity_conflict"
  | "waiting_resource"
  | "recording"
  | "retrying"
  | "recording_error";

export interface StreamerTagInput {
  name: string;
  promptGuidance: string | null;
}

export interface StreamerTag extends StreamerTagInput {
  id: number;
  sortOrder: number;
}

export interface StreamerPromptTag extends StreamerTagInput {
  priority: number;
}

export interface StreamerPromptContext {
  streamerId: number;
  streamerName: string;
  tags: StreamerPromptTag[];
}

export interface Streamer {
  id: number;
  name: string;
  sourceKind: "profile" | "room";
  sourceUrl: string;
  profileSecUid: string | null;
  webRid: string | null;
  roomUrl: string | null;
  roomId: string | null;
  monitorEnabled: boolean;
  archived: boolean;
  liveStatus: LiveStatus;
  monitorStatus: MonitorStatus;
  lastCheckedAt: string | null;
  lastError: string | null;
  failureCount: number;
  nextRetryAt: string | null;
  recordingPriority: number;
  currentVideoCount: number;
  historyVideoCount: number;
  tags: StreamerTag[];
}

export interface Dashboard {
  streamers: Streamer[];
  activeRecordings: number;
  currentVideoCount: number;
  maxScreenLimit: number;
}

export interface CreateStreamerInput {
  name: string;
  sourceUrl: string;
  monitorEnabled: boolean;
  tags: StreamerTagInput[];
}

export interface Video {
  id: number;
  sessionId: number;
  streamerId: number;
  streamerName: string;
  path: string;
  startedAt: string | null;
  endedAt: string | null;
  durationSeconds: number | null;
  sizeBytes: number;
  audioPresent: boolean | null;
  status: string;
  hasPreviewCache?: boolean;
}

export interface VideoPage {
  items: Video[];
  total: number;
  page: number;
  pageSize: number;
}

export interface VideoFilters {
  status?: string;
  search?: string;
  from?: string;
  to?: string;
}

export interface AppSettings {
  outputRoot: string;
  quality: string;
  protocol: string;
  segmentSeconds: number;
  maxConcurrentRecordings: number;
  asrDuringRecording: boolean;
  /** 旧版本字段，生产界面不提供修改入口。 */
  ffmpegPath?: string;
  ffprobePath?: string;
  notificationsEnabled: boolean;
  autostartEnabled: boolean;
  /** 生成式转场的图像模型降级链（逗号分隔，按序尝试）。 */
  generationImageModels: string;
  /** 生成式转场的视频模型降级链（逗号分隔，按序尝试）。 */
  generationVideoModels: string;
  /** 视频分辨率档位：480P / 720P / 1080P。上游默认 1080P，必须显式指定。 */
  generationVideoResolution: string;
  /** 视频时长（秒），上游允许 2–30。 */
  generationVideoSeconds: number;
  /** 是否用 AI 生成转场替换素材库匹配；关闭时回退到免费的素材库匹配。 */
  generationTransitionsEnabled: boolean;
  /** 特效字幕渲染缓存保留天数；0 表示不清理。 */
  renderCacheRetentionDays: number;
}

export interface EnvironmentStatus {
  ffmpeg: boolean;
  ffprobe: boolean;
}

export type RuntimeResourceStatus =
  | "ready"
  | "downloading"
  | "verifying"
  | "failed"
  | "cancelled"
  | "unsupported"
  | "rollback";

export interface RuntimeResourceComponent {
  id: string;
  version: string;
  required: boolean;
  fileCount: number;
  sizeBytes: number;
}

export interface RuntimeResourceView {
  status: RuntimeResourceStatus;
  ready: boolean;
  bundleVersion: string | null;
  platform: string;
  appMinVersion: string;
  manifestSha256: string | null;
  components: RuntimeResourceComponent[];
  totalSizeBytes: number;
  minimumFreeDiskBytes: number;
  minimumMemoryBytes: number;
  source: string;
  downloadedBytes: number;
  errorCode: string | null;
  errorMessage: string | null;
  updatedAt: string;
}

export interface RuntimeResourceProgress {
  phase: string;
  componentId: string | null;
  downloadedBytes: number;
  totalBytes: number;
}

export interface RuntimeResourceEvent {
  status: RuntimeResourceView;
  progress: RuntimeResourceProgress;
}

export interface MonitorEvent {
  kind: string;
  streamerId: number | null;
}

export type BrowserAccessStatus =
  | "native"
  | "browser_resolving"
  | "verification_required"
  | "session_ready"
  | "session_expired";

export interface BrowserAccessState {
  status: BrowserAccessStatus;
  pendingCount: number;
  activeStreamerId: number | null;
  currentWebRid: string | null;
  lastReason: string | null;
  updatedAt: string;
}

export type ActivationStatus =
  | "missing"
  | "verifying"
  | "active"
  | "retrying"
  | "revoked"
  | "invalid"
  | "contract_error";

export interface ActivationState {
  configured: boolean;
  active: boolean;
  status: ActivationStatus;
  message: string | null;
  deviceIdHint: string;
  lastHeartbeatAt: string | null;
  nextHeartbeatAt: string | null;
}

export type AiProjectStatus =
  | "draft"
  | "queued"
  | "running"
  | "deleting"
  | "completed"
  | "completed_with_errors"
  | "cancelled"
  | "failed";

export type AiInputStatus =
  | "pending"
  | "validating"
  | "preparing_audio"
  | "detecting_speech"
  | "transcribing"
  | "completed"
  | "skipped"
  | "cancelled"
  | "failed";

export interface RecognitionProfile {
  engineId: string;
  engineVersion: string;
  modelId: string;
  modelVersion: string;
  languageHint: string | null;
  vadModelId: string;
  vadThresholdMillis: number;
  vadPaddingMs: number;
  timestampPolicy: string;
  normalizationVersion: string;
  hotwords: string[];
}

export interface AiProject {
  id: number;
  name: string;
  status: AiProjectStatus;
  recognitionProfile: RecognitionProfile;
  recognitionProfileHash: string;
  inputFrozen: boolean;
  progressPercent: number;
  lastErrorCode: string | null;
  lastErrorMessage: string | null;
  projectTags?: string[];
  analysisGoal?: string | null;
  deletingAt?: string | null;
  createdAt: string;
  updatedAt: string;
}

export interface AiProjectInput {
  id: number;
  projectId: number;
  position: number;
  sourceKind: "local_file" | "video_library";
  videoId: number | null;
  displayName: string;
  durationMs: number | null;
  audioPresent: boolean | null;
  projectOffsetMs: number | null;
  status: AiInputStatus;
  progressPercent: number;
  artifactId: number | null;
  lastErrorCode: string | null;
  lastErrorMessage: string | null;
  schedulerGeneration?: number;
  queuePriority?: number;
  queueSequence?: number | null;
}

export interface AiProjectDetail {
  project: AiProject;
  inputs: AiProjectInput[];
}

export interface AiTrustedFileGrant {
  grantId: string;
  displayName: string;
}

export interface AiImportRejection {
  displayName: string;
  code: string;
  message: string;
}

export interface AiImportBatch {
  added: AiProjectInput[];
  rejected: AiImportRejection[];
}

export interface AiSessionOption {
  sessionId: number;
  streamerName: string;
  startedAt: string;
  endedAt: string;
  videoCount: number;
  totalDurationMs: number;
  unavailableVideoCount: number;
}

export interface AiReplayStreamerCursor {
  latestEndedAt: string;
  streamerId: number;
}

export interface AiReplaySessionCursor {
  startedAt: string;
  sessionId: number;
}

export interface AiReplayStreamerOption {
  streamerId: number;
  name: string;
  tags: string[];
  webRid: string | null;
  archived: boolean;
  monitorEnabled: boolean;
  liveStatus: string;
  monitorStatus: string;
  replayCount: number;
  latestEndedAt: string;
}

export interface AiReplaySessionOption {
  sessionId: number;
  startedAt: string;
  endedAt: string;
  status: string;
  videoCount: number;
  totalDurationMs: number;
  unavailableVideoCount: number;
  importedVideoCount: number;
  fullyImported: boolean;
}

export interface AiReplayStreamerPage {
  items: AiReplayStreamerOption[];
  nextCursor: AiReplayStreamerCursor | null;
}

export interface AiReplaySessionPage {
  items: AiReplaySessionOption[];
  nextCursor: AiReplaySessionCursor | null;
}

export interface AiSessionImportResult {
  detail: AiProjectDetail;
  addedCount: number;
  duplicateCount: number;
  unavailableCount: number;
}

export interface AiEnvironmentCheck {
  code: string;
  passed: boolean;
  message: string;
}

export interface AiEnvironmentDiagnostic {
  ready: boolean;
  platform: string;
  engineId: string;
  engineVersion: string;
  modelId: string;
  modelVersion: string;
  checks: AiEnvironmentCheck[];
  message: string;
  cloudAsrAvailable?: boolean;
  runtime?: AiRuntimeResourceDiagnostic | null;
}

export interface AiRuntimeResourceDiagnostic {
  bundleVersion: string;
  manifestSha256: string;
  signatureValid: boolean;
  components: AiRuntimeComponentDiagnostic[];
}

export interface AiRuntimeComponentDiagnostic {
  id: string;
  version: string;
  required: boolean;
  fileCount: number;
  sizeBytes: number;
}

export interface AiProjectSummary {
  totalInputs: number;
  validInputs: number;
  unavailableInputs: number;
  totalDurationMs: number;
  engineId: string;
  modelId: string;
  environmentReady: boolean;
  environmentMessage: string;
}

export interface AiTranscriptSegment {
  stableSegmentId: string;
  inputId: number;
  videoId: number | null;
  sourceStartMs: number;
  sourceEndMs: number;
  projectStartMs: number | null;
  projectEndMs: number | null;
  rawText: string;
  normalizedText: string;
  confidence: number | null;
}

export interface AiTranscriptInput {
  inputId: number;
  position: number;
  sourceKind: "local_file" | "video_library";
  videoId: number | null;
  displayName: string;
  durationMs: number | null;
  projectOffsetMs: number | null;
  status: AiInputStatus;
  progressPercent: number;
  errorCode: string | null;
  errorMessage: string | null;
  provider?: "aliyun" | "local" | string;
  fallbackReason?: string | null;
  gapDurationMs: number | null;
  segments: AiTranscriptSegment[];
}

export interface AiTranscriptProjection {
  project: AiProject;
  inputs: AiTranscriptInput[];
}

export interface AiJobEvent {
  projectId: number;
  inputId: number;
  projectStatus: AiProjectStatus;
  projectProgressPercent: number;
  inputStatus: AiInputStatus;
  inputProgressPercent: number;
  stage: string;
  message: string;
}

export type AiSmartWorkflowMode = "local" | "replay" | "live";
export type AiSmartWorkflowStatus =
  | "draft"
  | "queued"
  | "running"
  | "awaiting_selection"
  | "review_ready"
  | "paused"
  | "completed"
  | "cancelled"
  | "failed";
/**
 * 必须与后端 `AiSmartStage::ALL` 保持**同序同名**：
 * 片段级前置下纠错在合辑之前；`enhancement` 位于转场与审阅之间。
 * `ui/src/llmDefaults.test.ts` 有护栏测试直接比对 Rust 源。
 */
export type AiSmartStage =
  | "preflight"
  | "asr"
  | "highlight"
  | "correction"
  | "draft"
  | "transition"
  | "enhancement"
  | "review";
export type AiSmartBatchStatus =
  | "pending"
  | "queued"
  | "asr"
  | "highlight"
  | "completed"
  | "failed"
  | "cancelled";
export type AiSmartStageAttemptStatus =
  | "pending"
  | "running"
  | "completed"
  | "interrupted"
  | "cancelled"
  | "failed";
export type AiSmartDraftOwnership = "automation" | "user";
export type AiSmartDraftStatus =
  | "active"
  | "review_ready"
  | "exporting"
  | "frozen"
  | "exported"
  | "failed"
  | "superseded";

export interface SmartWorkflowConfiguration {
  name: string;
  provider: string;
  modelId: string;
  textScope: "selected_clip_subtitles";
  outputPreference: string;
}

export interface AiSmartWorkflow {
  id: number;
  name: string;
  mode: AiSmartWorkflowMode;
  status: AiSmartWorkflowStatus;
  stage: AiSmartStage;
  generation: number;
  sourceSessionId: number | null;
  sourceSummary: string;
  provider: string;
  modelId: string;
  textScope: string;
  authorizationDigest: string | null;
  authorizedAt: string | null;
  configurationFingerprint: string;
  activeDraftGeneration: number | null;
  liveCursorVideoId: number | null;
  liveStartVideoId: number | null;
  eventSequence: number;
  candidateCount: number;
  selectedCount: number;
  pendingBatchCount: number;
  lastErrorCode: string | null;
  lastErrorMessage: string | null;
  createdAt: string;
  updatedAt: string;
}

export interface AiSmartWorkflowBatch {
  id: number;
  workflowId: number;
  position: number;
  videoId: number | null;
  sourceFingerprint: string;
  projectId: number | null;
  highlightRunId: number | null;
  status: AiSmartBatchStatus;
  finalizedAt: string;
  lastErrorCode: string | null;
  lastErrorMessage: string | null;
  createdAt: string;
  updatedAt: string;
}

export interface AiSmartStageAttempt {
  id: number;
  workflowId: number;
  batchId: number | null;
  draftGeneration: number | null;
  stage: AiSmartStage;
  inputFingerprint: string;
  attemptGeneration: number;
  status: AiSmartStageAttemptStatus;
  progress: number;
  resultKind: string | null;
  resultId: number | null;
  durationMs: number | null;
  lastErrorCode: string | null;
  lastErrorMessage: string | null;
  createdAt: string;
  updatedAt: string;
}

export interface AiSmartDraft {
  id: number;
  workflowId: number;
  generation: number;
  clipProjectId: number;
  ownership: AiSmartDraftOwnership;
  status: AiSmartDraftStatus;
  automationProjectVersion: number;
  frozenProjectVersion: number | null;
  firstReviewableAt: string | null;
  createdAt: string;
  updatedAt: string;
}

export interface AiSmartWorkflowDetail {
  workflow: AiSmartWorkflow;
  batches: AiSmartWorkflowBatch[];
  attempts: AiSmartStageAttempt[];
  drafts: AiSmartDraft[];
  frozenInputCount: number;
  processedInputCount: number;
}

export interface AiSmartWorkflowEvent {
  workflowId: number;
  workflowGeneration: number;
  batchId: number | null;
  stage: AiSmartStage;
  sequence: number;
  status: AiSmartWorkflowStatus;
  errorCode: string | null;
  errorMessage: string | null;
}

export interface AiActiveLiveSession {
  sessionId: number;
  streamerName: string;
  startedAt: string;
}

export interface AiSmartReplaySessionCursor {
  startedAt: string;
  sessionId: number;
}

export interface AiSmartReplaySession {
  sessionId: number;
  streamerName: string;
  startedAt: string;
  endedAt: string;
  videoCount: number;
  totalDurationMs: number;
  unavailableVideoCount: number;
  existingWorkflowId: number | null;
  existingWorkflowStatus: AiSmartWorkflowStatus | null;
}

export interface AiSmartReplaySessionPage {
  items: AiSmartReplaySession[];
  nextCursor: AiSmartReplaySessionCursor | null;
}

export interface LlmProviderSettings {
  /** 兼容列：调用路径由统一调用点决定，界面不再依赖该字段。 */
  provider: string;
  modelId: string;
  timeoutMs: number;
  promptVersion: string;
  qualifiedScore: number;
  excellentScore: number;
  transitionAutoApplyScore: number;
  /** 旧上游凭据是否仍存在（保留但不再使用）。 */
  keyConfigured: boolean;
  updatedAt: string | null;
  /** 当前取值的解析来源（build_default / heartbeat / local_override / run_override）。 */
  modelSource?: string | null;
  resolvedAt?: string | null;
  nextResolutionAt?: string | null;
}

/** 短期 token 的只读状态：不含 token 本体。 */
export interface TokenStatus {
  cached: boolean;
  provider: string | null;
  expiresInSeconds: number | null;
}

/** 一条脱敏的 LLM 调用诊断事件。 */
export interface LlmCallDiagnostic {
  stage: string;
  path: string;
  modelId: string;
  durationMs: number;
  tokenUsage: number | null;
  errorCategory: string | null;
}

/** 调用点诊断面板视图。 */
export interface LlmCallDiagnostics {
  enabled: boolean;
  capacity: number;
  events: LlmCallDiagnostic[];
}

/** 设置页的「当前模型与可用性」视图。 */
export interface AiModelStatus {
  modelId: string;
  modelSource: string | null;
  resolvedAt: string | null;
  nextResolutionAt: string | null;
  token: TokenStatus;
  keyConfigured: boolean;
  callPath: string;
  /** 是否非生产构建；界面据此决定是否显示「立即重新解析」。 */
  nonProductionConfig: boolean;
}

export type AiHighlightRunStatus = "pending" | "running" | "candidates" | "ranking" | "completed" | "partial" | "cancelled" | "failed";

export interface AiHighlightRun {
  id: number;
  projectId: number;
  status: AiHighlightRunStatus;
  modelId: string;
  promptVersion: string;
  tagsSnapshot: string[];
  skillsSnapshot: string[];
  analysisGoal: string | null;
  analysisFingerprint: string;
  qualifiedScore: number;
  excellentScore: number;
  userAuthorized: boolean;
  totalSegments: number;
  totalChars: number;
  estimatedBatches: number;
  totalTokens: number;
  lastErrorCode: string | null;
  lastErrorMessage: string | null;
  createdAt: string;
  updatedAt: string;
}

export interface AiHighlightProgress {
  runId: number;
  totalBatches: number;
  pendingBatches: number;
  runningBatches: number;
  completedBatches: number;
  failedBatches: number;
  candidateCount: number;
}

export interface AiHighlightCandidate {
  id: number;
  runId: number;
  chunkId: number | null;
  candidateKey: string;
  title: string;
  inputId: number;
  segmentIds: string[];
  startMs: number;
  endMs: number;
  totalScore: number;
  hookScore: number;
  informationScore: number;
  emotionScore: number;
  tagRelevanceScore: number;
  completenessScore: number;
  shareabilityScore: number;
  reason: string;
  matchedTags: string[];
  rank: number | null;
  selected: boolean;
}

export interface AiHighlightCandidatePage {
  items: AiHighlightCandidate[];
  page: number;
  pageSize: number;
  totalCandidates: number;
  qualifiedCandidates: number;
  selectedCandidates: number;
}

/**
 * 当前调用端点（百炼兼容模式）支持的默认模型标识。
 *
 * 与后端 `DEFAULT_MODEL_ID` 保持同值：客户端只走"直连百炼"这一条路，
 * 因此 fallback MUST NOT 回落到其他厂商的模型名（例如 `deepseek-chat`），
 * 否则请求会被上游以 400（模型不存在）拒绝。
 */
export const DEFAULT_LLM_MODEL_ID = "qwen-plus";

/** 生成式动画资产（步骤 4 任务组 1）。只含结构化字段：不含下载地址或本地路径。 */
export interface AiGeneratedAnimationAsset {
  id: number;
  projectId: number;
  candidateId: number;
  highlightRunId: number | null;
  generation: number;
  reuseKey: string;
  composition: "replace" | "insert";
  status: "pending" | "validated" | "rejected" | "invalidated";
  replaceRange: { startMs: number; endMs: number } | null;
  insertBinding: { leftSegmentId: number; rightSegmentId: number; boundaryId: number } | null;
  upstreamTaskId: string | null;
  sha256: string | null;
  width: number | null;
  height: number | null;
  fps: number | null;
  durationMs: number | null;
  rejectionCode: string | null;
  createdAt: string;
  updatedAt: string;
}

/** 单条高光的增强审阅项（步骤 4 任务 9.1）。只含结构化字段：不含 Prompt 正文、资产地址或 Provider 原始响应。 */
export interface AiEnhancementReviewItem {
  candidateId: number;
  /** 最终唯一的增强方式；null 表示尚无结果（进行中）。 */
  kind: "effect_caption" | "video_animation" | null;
  status: "suggested" | "applied" | "rejected" | "superseded" | null;
  composition: "replace" | "insert" | null;
  assetStatus: "pending" | "validated" | "rejected" | "invalidated" | null;
  rejectionCode: string | null;
  /** 已产生成本（分）。 */
  costMinor: number;
}

export type AiClipEffect = "none" | "fade_in" | "fade_out" | "fade_in_out" | "flash" | "black";
export type AiClipExportStatus = "idle" | "exporting" | "completed" | "cancelled" | "failed";
export type MaterialDownloadStatus = "missing" | "downloading" | "transcoding" | "ready" | "failed";
export type TransitionCatalogSyncStatus = "idle" | "checking" | "syncing" | "ready" | "upgrade_required" | "failed";
export type BoundarySelectionSource = "none" | "agent" | "manual";

export interface TransitionCatalogState {
  localCatalogVersion: number;
  remoteCatalogVersion: number | null;
  minimumAppVersion: string | null;
  status: TransitionCatalogSyncStatus;
  lastCheckedAt: string | null;
  lastSuccessAt: string | null;
  lastErrorCode: string | null;
  lastErrorMessage: string | null;
}

export interface TransitionMaterialDownload {
  assetKey: string;
  assetVersion: number;
  sourceStatus: MaterialDownloadStatus;
  sourceRelativePath: string | null;
  previewStatus: MaterialDownloadStatus;
  previewRelativePath: string | null;
  validatedSizeBytes: number | null;
  lastErrorCode: string | null;
  lastErrorMessage: string | null;
  updatedAt: string;
}

export interface TransitionMaterial {
  assetKey: string;
  assetVersion: number;
  title: string;
  description: string;
  tags: string[];
  category: string;
  renderMode: "bridge";
  durationMs: number;
  width: number;
  height: number;
  fps: number;
  videoCodec: string;
  hasAudio: boolean;
  sortOrder: number;
  thumbnailAvailable: boolean;
  download: TransitionMaterialDownload;
}

export interface MaterialAssetSnapshot {
  assetKey: string;
  assetVersion: number;
  state: MaterialDownloadStatus;
  mediaUrl: string | null;
  generated: boolean;
  errorCode: string | null;
  errorMessage: string | null;
}

export interface ClipTransitionBoundary {
  id: number;
  clipProjectId: number;
  leftClipSegmentId: number | null;
  rightClipSegmentId: number | null;
  leftStableId: number;
  rightStableId: number;
  assetKey: string | null;
  assetVersion: number | null;
  selectionSource: BoundarySelectionSource;
  confidence: number | null;
  score: number | null;
  sceneScore: number | null;
  continuityScore: number | null;
  rhythmScore: number | null;
  materialScore: number | null;
  reason: string | null;
  suggestedAssetKey: string | null;
  suggestedAssetVersion: number | null;
  suggestionConfidence: number | null;
  suggestionScore: number | null;
  suggestionSceneScore: number | null;
  suggestionContinuityScore: number | null;
  suggestionRhythmScore: number | null;
  suggestionMaterialScore: number | null;
  suggestionReason: string | null;
  suggestionNone: boolean;
  manuallyLocked: boolean;
  stale: boolean;
  active: boolean;
  updatedAt: string;
}

export interface AiClipTimelineUnit {
  key: string;
  kind: "segment" | "bridge" | "animation";
  projectStartMs: number;
  projectEndMs: number;
  clipSegmentId: number | null;
  boundaryId: number | null;
  title: string;
  assetKey: string | null;
  assetVersion: number | null;
  sourceStatus: MaterialDownloadStatus | null;
  previewStatus: MaterialDownloadStatus | null;
  /** 生成资产 ID（仅 animation 单元有值）。 */
  generatedAssetId: number | null;
  /** 合成方式（仅 animation 单元有值）。 */
  composition: "replace" | "insert" | null;
  /** 资产校验状态（仅 animation 单元有值）。 */
  assetStatus: "pending" | "validated" | "rejected" | "invalidated" | null;
}

export interface TransitionMatchSummary {
  runId: number;
  threshold: number;
  matched: number;
  autoApplied: number;
  suggestions: number;
  noneSuggestions: number;
  tokenUsage: number;
  boundaries: ClipTransitionBoundary[];
}

export interface ClipTextCorrectionSummary {
  runId: number;
  processed: number;
  changed: number;
  unchanged: number;
  skippedManual: number;
  skippedHidden: number;
  totalBatches: number;
  tokenUsage: number;
  detail: AiClipProjectDetail;
}

export interface ClipWorkflowProgress {
  workflow: "textCorrection" | "transitionAgent";
  clipProjectId: number;
  runId: number | null;
  stage: "preparing" | "correcting" | "saving" | "matching" | "scoring" | "applying" | "completed" | "cancelled" | "failed";
  completed: number;
  total: number;
  message: string;
}

export interface AiClipProject {
  id: number;
  highlightRunId: number;
  name: string;
  outputWidth: number | null;
  outputHeight: number | null;
  version: number;
  exportStatus: AiClipExportStatus;
  exportProgress: number;
  outputPath: string | null;
  lastErrorCode: string | null;
  lastErrorMessage: string | null;
  createdAt: string;
  updatedAt: string;
}

export interface AiClipSegment {
  id: number;
  clipProjectId: number;
  candidateId: number;
  inputId: number;
  position: number;
  title: string;
  sourceStartMs: number;
  sourceEndMs: number;
  volumePercent: number;
  effect: AiClipEffect;
}

export interface AiClipSubtitle {
  id: number;
  clipProjectId: number;
  stableSegmentId: string;
  clipSegmentId: number;
  inputId: number;
  originalText: string;
  text: string;
  hidden: boolean;
  sourceStartMs: number;
  sourceEndMs: number;
  projectStartMs: number;
  projectEndMs: number;
}

export interface AiClipSubtitleFrame {
  subtitleId: number;
  clipSegmentId: number;
  projectStartMs: number;
  projectEndMs: number;
  pageText: string;
  visibleText: string;
  hiddenText: string;
}

export interface AiClipProjectDetail {
  project: AiClipProject;
  segments: AiClipSegment[];
  subtitles: AiClipSubtitle[];
  subtitleFrames: AiClipSubtitleFrame[];
  subtitlesComplete: boolean;
  boundaries?: ClipTransitionBoundary[];
  projectDurationMs?: number;
  timelineUnits?: AiClipTimelineUnit[];
}

export interface AiClipSegmentUpdate {
  volumePercent: number;
  effect: AiClipEffect;
}

export interface AiClipSubtitleUpdate {
  text: string;
  hidden: boolean;
  expectedProjectVersion: number;
}

export interface AiExportResult {
  saved: boolean;
}

export type PreviewState =
  | "queued"
  | "probing"
  | "remuxing"
  | "transcoding"
  | "ready"
  | "failed";

export interface PreviewMedia {
  path: string;
  mimeType: string;
  cacheHit: boolean;
  generated: boolean;
  sourceMissing: boolean;
}

export interface PreviewSnapshot {
  requestId: string;
  videoId: number;
  state: PreviewState;
  progressPercent: number | null;
  message: string;
  media: PreviewMedia | null;
  errorCode: string | null;
  errorMessage: string | null;
}

export type ThumbnailState = "queued" | "ready" | "failed" | "unavailable";

export type ThumbnailSourceKind = "original" | "preview_cache";

export interface ThumbnailMedia {
  path: string;
  mimeType: "image/jpeg";
  width: number;
  height: number;
  cacheHit: boolean;
  sourceKind: ThumbnailSourceKind;
}

export interface ThumbnailSnapshot {
  batchId: string;
  videoId: number;
  cacheKey: string | null;
  state: ThumbnailState;
  media: ThumbnailMedia | null;
  errorCode: string | null;
  errorMessage: string | null;
}

export interface ThumbnailBatch {
  batchId: string;
  items: ThumbnailSnapshot[];
}

export interface ThumbnailEvent {
  batchId: string;
  item: ThumbnailSnapshot;
}

export interface ClientApi {
  getActivationState(): Promise<ActivationState>;
  activateClient(activationCode: string): Promise<ActivationState>;
  clearActivation(): Promise<ActivationState>;
  getDashboard(): Promise<Dashboard>;
  createStreamer(input: CreateStreamerInput): Promise<Streamer>;
  updateStreamer(id: number, input: CreateStreamerInput): Promise<Streamer>;
  listStreamerTagNameSuggestions(limit?: number): Promise<string[]>;
  getStreamerPromptContext(id: number): Promise<StreamerPromptContext>;
  setMonitorEnabled(id: number, enabled: boolean): Promise<void>;
  checkStreamerNow(id: number): Promise<void>;
  moveStreamerRecordingPriority(id: number, direction: "up" | "down"): Promise<Streamer[]>;
  archiveStreamer(id: number): Promise<void>;
  stopRecording(id: number): Promise<void>;
  listVideos(streamerId?: number, page?: number, pageSize?: number, filters?: VideoFilters): Promise<VideoPage>;
  listCurrentVideos(streamerId: number): Promise<VideoPage>;
  getSettings(): Promise<AppSettings>;
  saveSettings(settings: AppSettings): Promise<void>;
  requestVideoPreview(id: number): Promise<PreviewSnapshot>;
  retryVideoPreview(id: number): Promise<PreviewSnapshot>;
  getVideoPreview(requestId: string): Promise<PreviewSnapshot>;
  retainVideoPreview(requestId: string): Promise<void>;
  releaseVideoPreview(requestId: string): Promise<void>;
  requestVideoThumbnails(videoIds: number[]): Promise<ThumbnailBatch>;
  getVideoThumbnails(batchId: string): Promise<ThumbnailBatch>;
  releaseVideoThumbnailBatch(batchId: string): Promise<void>;
  retryVideoThumbnail(batchId: string, videoId: number): Promise<ThumbnailSnapshot>;
  openVideo(id: number): Promise<void>;
  revealVideo(id: number): Promise<void>;
  deleteVideo(id: number): Promise<void>;
  deleteSession(sessionId: number): Promise<void>;
  openLogs(): Promise<void>;
  diagnoseEnvironment(): Promise<EnvironmentStatus>;
  runtimeResourceStatus?(): Promise<RuntimeResourceView>;
  runtimeResourceManifest?(): Promise<RuntimeResourceView>;
  runtimeResourceDownload?(): Promise<RuntimeResourceView>;
  runtimeResourceCancel?(): Promise<void>;
  runtimeResourceRecheck?(): Promise<RuntimeResourceView>;
  runtimeResourceSource?(): Promise<string>;
  subscribeRuntimeResources?(listener: (event: RuntimeResourceEvent) => void): Promise<() => void>;
  getBrowserAccessState(): Promise<BrowserAccessState>;
  showDouyinVerification(): Promise<void>;
  recheckDouyinAccess(): Promise<BrowserAccessState>;
  clearDouyinSession(confirmed: boolean): Promise<BrowserAccessState>;
  listAiProjects(): Promise<AiProject[]>;
  getAiProject(projectId: number): Promise<AiProjectDetail>;
  createAiProject(input: { name: string; hotwords: string[] }): Promise<AiProject>;
  renameAiProject(projectId: number, name: string): Promise<AiProject>;
  setAiProjectContext?(projectId: number, tags: string[], analysisGoal: string | null): Promise<AiProject>;
  deleteAiProject(projectId: number): Promise<void>;
  pickAiLocalVideos(): Promise<AiTrustedFileGrant[]>;
  createLocalSmartWorkflow(input: {
    configuration: SmartWorkflowConfiguration;
    grantIds: string[];
    authorizationConfirmed: boolean;
  }): Promise<AiSmartWorkflowDetail>;
  listActiveLiveSessions(): Promise<AiActiveLiveSession[]>;
  createLiveSmartWorkflow(input: {
    configuration: SmartWorkflowConfiguration;
    sessionId: number;
    authorizationConfirmed: boolean;
  }): Promise<AiSmartWorkflowDetail>;
  listSmartReplaySessions(
    streamerId: number | null,
    search?: string,
    cursor?: AiSmartReplaySessionCursor | null,
    limit?: number,
  ): Promise<AiSmartReplaySessionPage>;
  createReplaySmartWorkflow(input: {
    configuration: SmartWorkflowConfiguration;
    sessionId: number;
    authorizationConfirmed: boolean;
    duplicateConfirmed: boolean;
  }): Promise<AiSmartWorkflowDetail>;
  authorizeSmartWorkflow(input: {
    workflowId: number;
    expectedGeneration: number;
    configurationFingerprint: string;
    authorizationConfirmed: boolean;
  }): Promise<AiSmartWorkflowDetail>;
  listSmartWorkflows(): Promise<AiSmartWorkflow[]>;
  getSmartWorkflow(workflowId: number): Promise<AiSmartWorkflowDetail>;
  cancelSmartWorkflow(workflowId: number, expectedGeneration: number): Promise<AiSmartWorkflowDetail>;
  confirmSmartHighlightFallback(workflowId: number, expectedGeneration: number): Promise<AiSmartWorkflowDetail>;
  retrySmartWorkflowStage(input: {
    workflowId: number;
    expectedGeneration: number;
    stage: AiSmartStage;
    batchId: number | null;
  }): Promise<AiSmartWorkflowDetail>;
  openSmartDraft(draftId: number): Promise<AiClipProjectDetail>;
  importAiLocalGrants(projectId: number, grantIds: string[]): Promise<AiImportBatch>;
  listAiCompletedSessions(limit?: number): Promise<AiSessionOption[]>;
  listAiReplayStreamers(search?: string, cursor?: AiReplayStreamerCursor | null, limit?: number): Promise<AiReplayStreamerPage>;
  listAiReplaySessions(streamerId: number, projectId: number, search?: string, cursor?: AiReplaySessionCursor | null, limit?: number): Promise<AiReplaySessionPage>;
  addAiCompletedSession(projectId: number, sessionId: number): Promise<AiSessionImportResult>;
  reorderAiInputs(projectId: number, orderedIds: number[]): Promise<AiProjectDetail>;
  removeAiInput(projectId: number, inputId: number): Promise<AiProjectDetail>;
  getAiProjectSummary(projectId: number): Promise<AiProjectSummary>;
  startAiProject(projectId: number): Promise<AiProject>;
  cancelAiProject(projectId: number): Promise<AiProject>;
  promoteAiNextInput?(inputId: number): Promise<AiProjectDetail>;
  preemptAiWithInput?(inputId: number, confirmed: boolean): Promise<AiProjectDetail>;
  retryAiInput(inputId: number): Promise<AiProjectDetail>;
  queryAiTranscript(projectId: number): Promise<AiTranscriptProjection>;
  /** 按高光查询增强审阅项（步骤 4 任务 9.1）。入口是剪辑工程。 */
  queryAiEnhancementReview?(clipProjectId: number): Promise<AiEnhancementReviewItem[]>;
  /** 移除工程里的生成式动画资产（标记失效、需重新审阅）。 */
  removeAiGeneratedAnimation?(clipProjectId: number, assetId: number): Promise<AiGeneratedAnimationAsset>;
  copyAiSegmentText(projectId: number, stableSegmentId: string): Promise<string>;
  copyAiInputText(projectId: number, inputId: number): Promise<string>;
  copyAiProjectText(projectId: number): Promise<string>;
  exportAiTxt(projectId: number): Promise<AiExportResult>;
  exportAiJson(projectId: number): Promise<AiExportResult>;
  diagnoseAiEnvironment(): Promise<AiEnvironmentDiagnostic>;
  getAiLlmSettings?(): Promise<LlmProviderSettings>;
  getAiModelStatus?(): Promise<AiModelStatus>;
  reparseAiModelConfig?(): Promise<AiModelStatus>;
  setLlmDiagnostics?(enabled: boolean): Promise<void>;
  getLlmDiagnostics?(): Promise<LlmCallDiagnostics>;
  saveAiLlmSettings?(settings: LlmProviderSettings, apiKey?: string): Promise<LlmProviderSettings>;
  clearAiLlmKey?(): Promise<void>;
  diagnoseAiLlmProvider?(): Promise<ProviderDiagnostic>;
  startAiHighlightAnalysis?(projectId: number, confirmed: boolean): Promise<AiHighlightRun>;
  getLatestAiHighlightRun?(projectId: number): Promise<AiHighlightRun | null>;
  getAiHighlightProgress?(runId: number): Promise<AiHighlightProgress>;
  resumeAiHighlightAnalysis?(runId: number): Promise<AiHighlightRun>;
  listAiHighlightCandidates?(runId: number): Promise<AiHighlightCandidate[]>;
  listQualifiedAiHighlightCandidates?(runId: number, page?: number, pageSize?: number): Promise<AiHighlightCandidatePage>;
  listSelectedAiHighlightCandidates?(runId: number, page?: number, pageSize?: number): Promise<AiHighlightCandidatePage>;
  selectAiHighlightCandidates?(runId: number, candidateIds: number[]): Promise<AiHighlightCandidate[]>;
  setAiHighlightCandidateSelected?(runId: number, candidateId: number, selected: boolean): Promise<AiHighlightCandidate>;
  openAiClipProject?(runId: number): Promise<AiClipProjectDetail>;
  getAiClipProject?(clipProjectId: number): Promise<AiClipProjectDetail>;
  updateAiClipSegment?(clipProjectId: number, segmentId: number, update: AiClipSegmentUpdate): Promise<AiClipProjectDetail>;
  updateAiClipSubtitle?(clipProjectId: number, subtitleId: number, update: AiClipSubtitleUpdate): Promise<AiClipProjectDetail>;
  resetAiClipSubtitle?(clipProjectId: number, subtitleId: number, expectedProjectVersion: number): Promise<AiClipProjectDetail>;
  insertAiClipCandidate?(clipProjectId: number, candidateId: number, insertIndex: number): Promise<AiClipProjectDetail>;
  reorderAiClipSegments?(clipProjectId: number, orderedIds: number[]): Promise<AiClipProjectDetail>;
  removeAiClipSegment?(clipProjectId: number, segmentId: number): Promise<AiClipProjectDetail>;
  getTransitionCatalogState?(): Promise<TransitionCatalogState>;
  retryTransitionCatalogSync?(): Promise<TransitionCatalogState>;
  subscribeTransitionCatalog?(listener: (state: TransitionCatalogState) => void): Promise<() => void>;
  listTransitionMaterials?(): Promise<TransitionMaterial[]>;
  requestTransitionMaterialPreview?(assetKey: string, assetVersion: number): Promise<MaterialAssetSnapshot>;
  requestTransitionMaterialThumbnail?(assetKey: string, assetVersion: number): Promise<MaterialAssetSnapshot>;
  matchAiClipTransitions?(clipProjectId: number, boundaryId?: number | null): Promise<TransitionMatchSummary>;
  getLatestAiClipTransitionReview?(clipProjectId: number): Promise<TransitionMatchSummary | null>;
  cancelAiClipTransitionAgent?(clipProjectId: number): Promise<void>;
  correctAiClipText?(clipProjectId: number, expectedProjectVersion: number): Promise<ClipTextCorrectionSummary>;
  cancelAiClipTextCorrection?(clipProjectId: number): Promise<void>;
  subscribeClipWorkflow?(listener: (progress: ClipWorkflowProgress) => void): Promise<() => void>;
  applyAiClipTransition?(boundaryId: number, assetKey: string | null, assetVersion: number | null, lockEmpty?: boolean): Promise<ClipTransitionBoundary>;
  unlockAiClipTransition?(boundaryId: number): Promise<ClipTransitionBoundary>;
  startAiClipExport?(clipProjectId: number): Promise<AiClipProject>;
  cancelAiClipExport?(clipProjectId: number): Promise<AiClipProject>;
  requestAiInputPreview(projectId: number, inputId: number): Promise<PreviewSnapshot>;
  retryAiInputPreview(projectId: number, inputId: number): Promise<PreviewSnapshot>;
  requestExit(force: boolean): Promise<void>;
  subscribe(listener: (event: MonitorEvent) => void): Promise<() => void>;
  subscribeBrowserAccess(listener: (state: BrowserAccessState) => void): Promise<() => void>;
  subscribePreview(listener: (snapshot: PreviewSnapshot) => void): Promise<() => void>;
  subscribeThumbnail(listener: (event: ThumbnailEvent) => void): Promise<() => void>;
  subscribeAi(listener: (event: AiJobEvent) => void): Promise<() => void>;
  subscribeSmartWorkflows(listener: (event: AiSmartWorkflowEvent) => void): Promise<() => void>;
  subscribeActivation(listener: (state: ActivationState) => void): Promise<() => void>;
}

export interface ProviderDiagnostic {
  ok: boolean;
  category: string;
  message: string;
}
