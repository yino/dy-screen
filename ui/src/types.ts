export type LiveStatus = "checking" | "offline" | "live" | "error";

export type MonitorStatus =
  | "paused"
  | "waiting"
  | "discovering"
  | "waiting_first_live"
  | "profile_error"
  | "rediscovering"
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
  ffmpegPath: string;
  ffprobePath: string;
  notificationsEnabled: boolean;
  autostartEnabled: boolean;
}

export interface EnvironmentStatus {
  ffmpeg: boolean;
  ffprobe: boolean;
}

export interface MonitorEvent {
  kind: string;
  streamerId: number | null;
}

export type AiProjectStatus =
  | "draft"
  | "queued"
  | "running"
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

export interface ClientApi {
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
  openVideo(id: number): Promise<void>;
  revealVideo(id: number): Promise<void>;
  deleteVideo(id: number): Promise<void>;
  deleteSession(sessionId: number): Promise<void>;
  openLogs(): Promise<void>;
  diagnoseEnvironment(): Promise<EnvironmentStatus>;
  listAiProjects(): Promise<AiProject[]>;
  getAiProject(projectId: number): Promise<AiProjectDetail>;
  createAiProject(input: { name: string; hotwords: string[] }): Promise<AiProject>;
  renameAiProject(projectId: number, name: string): Promise<AiProject>;
  deleteAiProject(projectId: number): Promise<void>;
  pickAiLocalVideos(): Promise<AiTrustedFileGrant[]>;
  importAiLocalGrants(projectId: number, grantIds: string[]): Promise<AiImportBatch>;
  listAiCompletedSessions(limit?: number): Promise<AiSessionOption[]>;
  addAiCompletedSession(projectId: number, sessionId: number): Promise<AiProjectDetail>;
  reorderAiInputs(projectId: number, orderedIds: number[]): Promise<AiProjectDetail>;
  removeAiInput(projectId: number, inputId: number): Promise<AiProjectDetail>;
  getAiProjectSummary(projectId: number): Promise<AiProjectSummary>;
  startAiProject(projectId: number): Promise<AiProject>;
  cancelAiProject(projectId: number): Promise<AiProject>;
  retryAiInput(inputId: number): Promise<AiProjectDetail>;
  queryAiTranscript(projectId: number): Promise<AiTranscriptProjection>;
  copyAiSegmentText(projectId: number, stableSegmentId: string): Promise<string>;
  copyAiInputText(projectId: number, inputId: number): Promise<string>;
  copyAiProjectText(projectId: number): Promise<string>;
  exportAiTxt(projectId: number): Promise<AiExportResult>;
  exportAiJson(projectId: number): Promise<AiExportResult>;
  diagnoseAiEnvironment(): Promise<AiEnvironmentDiagnostic>;
  requestAiInputPreview(projectId: number, inputId: number): Promise<PreviewSnapshot>;
  retryAiInputPreview(projectId: number, inputId: number): Promise<PreviewSnapshot>;
  requestExit(force: boolean): Promise<void>;
  subscribe(listener: (event: MonitorEvent) => void): Promise<() => void>;
  subscribePreview(listener: (snapshot: PreviewSnapshot) => void): Promise<() => void>;
  subscribeAi(listener: (event: AiJobEvent) => void): Promise<() => void>;
}
