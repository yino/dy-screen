export type LiveStatus = "checking" | "offline" | "live" | "error";

export type MonitorStatus =
  | "paused"
  | "waiting"
  | "waiting_resource"
  | "recording"
  | "retrying"
  | "recording_error";

export interface Streamer {
  id: number;
  name: string;
  roomUrl: string;
  roomId: string;
  monitorEnabled: boolean;
  archived: boolean;
  liveStatus: LiveStatus;
  monitorStatus: MonitorStatus;
  lastCheckedAt: string | null;
  lastError: string | null;
  currentVideoCount: number;
  historyVideoCount: number;
}

export interface Dashboard {
  streamers: Streamer[];
  activeRecordings: number;
  currentVideoCount: number;
}

export interface CreateStreamerInput {
  name: string;
  roomUrl: string;
  monitorEnabled: boolean;
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

export interface ClientApi {
  getDashboard(): Promise<Dashboard>;
  createStreamer(input: CreateStreamerInput): Promise<Streamer>;
  updateStreamer(id: number, input: CreateStreamerInput): Promise<Streamer>;
  setMonitorEnabled(id: number, enabled: boolean): Promise<void>;
  checkStreamerNow(id: number): Promise<void>;
  archiveStreamer(id: number): Promise<void>;
  stopRecording(id: number): Promise<void>;
  listVideos(streamerId?: number, page?: number, pageSize?: number, filters?: VideoFilters): Promise<VideoPage>;
  listCurrentVideos(streamerId: number): Promise<VideoPage>;
  getSettings(): Promise<AppSettings>;
  saveSettings(settings: AppSettings): Promise<void>;
  openVideo(id: number): Promise<void>;
  revealVideo(id: number): Promise<void>;
  deleteVideo(id: number): Promise<void>;
  deleteSession(sessionId: number): Promise<void>;
  openLogs(): Promise<void>;
  diagnoseEnvironment(): Promise<EnvironmentStatus>;
  requestExit(force: boolean): Promise<void>;
  subscribe(listener: (event: MonitorEvent) => void): Promise<() => void>;
}
