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
  requestExit(force: boolean): Promise<void>;
  subscribe(listener: (event: MonitorEvent) => void): Promise<() => void>;
  subscribePreview(listener: (snapshot: PreviewSnapshot) => void): Promise<() => void>;
}
