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
  currentVideoCount: number;
  historyVideoCount: number;
  tags: StreamerTag[];
}

export interface Dashboard {
  streamers: Streamer[];
  activeRecordings: number;
  currentVideoCount: number;
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

export type ActivationStatus = "missing" | "active" | "retrying" | "revoked" | "invalid" | "development_bypass";

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

export interface LlmProviderSettings {
  provider: "deepseek";
  modelId: string;
  timeoutMs: number;
  promptVersion: string;
  qualifiedScore: number;
  excellentScore: number;
  keyConfigured: boolean;
  updatedAt: string | null;
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

export type AiClipEffect = "none" | "fade_in" | "fade_out" | "fade_in_out" | "flash" | "black";
export type AiClipExportStatus = "idle" | "exporting" | "completed" | "cancelled" | "failed";

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
  stableSegmentId: string;
  clipSegmentId: number;
  inputId: number;
  normalizedText: string;
  sourceStartMs: number;
  sourceEndMs: number;
  projectStartMs: number;
  projectEndMs: number;
}

export interface AiClipProjectDetail {
  project: AiClipProject;
  segments: AiClipSegment[];
  subtitles: AiClipSubtitle[];
  subtitlesComplete: boolean;
}

export interface AiClipSegmentUpdate {
  volumePercent: number;
  effect: AiClipEffect;
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
  copyAiSegmentText(projectId: number, stableSegmentId: string): Promise<string>;
  copyAiInputText(projectId: number, inputId: number): Promise<string>;
  copyAiProjectText(projectId: number): Promise<string>;
  exportAiTxt(projectId: number): Promise<AiExportResult>;
  exportAiJson(projectId: number): Promise<AiExportResult>;
  diagnoseAiEnvironment(): Promise<AiEnvironmentDiagnostic>;
  getAiLlmSettings?(): Promise<LlmProviderSettings>;
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
  insertAiClipCandidate?(clipProjectId: number, candidateId: number, insertIndex: number): Promise<AiClipProjectDetail>;
  reorderAiClipSegments?(clipProjectId: number, orderedIds: number[]): Promise<AiClipProjectDetail>;
  removeAiClipSegment?(clipProjectId: number, segmentId: number): Promise<AiClipProjectDetail>;
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
  subscribeActivation(listener: (state: ActivationState) => void): Promise<() => void>;
}

export interface ProviderDiagnostic {
  ok: boolean;
  category: string;
  message: string;
}
