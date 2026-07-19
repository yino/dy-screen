import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type {
  AppSettings,
  ClientApi,
  CreateStreamerInput,
  Dashboard,
  EnvironmentStatus,
  MonitorEvent,
  Streamer,
  VideoPage,
} from "./types";

const defaultSettings: AppSettings = {
  outputRoot: "~/Downloads/dy-screen",
  quality: "HD1",
  protocol: "flv",
  segmentSeconds: 900,
  maxConcurrentRecordings: 4,
  ffmpegPath: "ffmpeg",
  ffprobePath: "ffprobe",
  notificationsEnabled: true,
  autostartEnabled: false,
};

function isTauriRuntime(): boolean {
  return "__TAURI_INTERNALS__" in window;
}

const tauriApi: ClientApi = {
  getDashboard: () => invoke<Dashboard>("get_dashboard"),
  createStreamer: (input) => invoke<Streamer>("create_streamer", { input }),
  updateStreamer: (id, input) => invoke<Streamer>("update_streamer", { id, input }),
  setMonitorEnabled: (id, enabled) =>
    invoke<void>("set_monitor_enabled", { id, enabled }),
  checkStreamerNow: (id) => invoke<void>("check_streamer_now", { id }),
  archiveStreamer: (id) => invoke<void>("archive_streamer", { id }),
  stopRecording: (id) => invoke<void>("stop_recording", { id }),
  listVideos: (streamerId, page = 1, pageSize = 50, filters = {}) =>
    invoke<VideoPage>("list_videos", { streamerId, page, pageSize, filter: filters }),
  listCurrentVideos: (streamerId) =>
    invoke<VideoPage>("list_current_videos", { streamerId }),
  getSettings: () => invoke<AppSettings>("get_settings"),
  saveSettings: (settings) => invoke<void>("save_settings", { settings }),
  openVideo: (id) => invoke<void>("open_video", { id }),
  revealVideo: (id) => invoke<void>("reveal_video", { id }),
  deleteVideo: (id) => invoke<void>("delete_video", { id }),
  deleteSession: (sessionId) => invoke<void>("delete_session", { sessionId }),
  openLogs: () => invoke<void>("open_logs"),
  diagnoseEnvironment: () => invoke<EnvironmentStatus>("diagnose_environment"),
  requestExit: (force) => invoke<void>("request_exit", { force }),
  subscribe: async (listener) => {
    const unlisten = await listen<MonitorEvent>("monitor-event", (event) => {
      listener(event.payload);
    });
    return unlisten;
  },
};

function createBrowserApi(): ClientApi {
  let streamers: Streamer[] = [];
  let settings = defaultSettings;
  try {
    streamers = JSON.parse(localStorage.getItem("dy-screen-streamers") || "[]") as Streamer[];
    settings = JSON.parse(
      localStorage.getItem("dy-screen-settings") || JSON.stringify(defaultSettings),
    ) as AppSettings;
  } catch {
    streamers = [];
  }

  const saveStreamers = () => {
    localStorage.setItem("dy-screen-streamers", JSON.stringify(streamers));
  };

  const dashboard = (): Dashboard => ({
    streamers: [...streamers],
    activeRecordings: streamers.filter((item) => item.monitorStatus === "recording").length,
    currentVideoCount: streamers.reduce((total, item) => total + item.currentVideoCount, 0),
  });

  return {
    getDashboard: async () => dashboard(),
    createStreamer: async (input: CreateStreamerInput) => {
      const roomMatch = input.roomUrl.match(/live\.douyin\.com\/(\d+)/);
      if (!roomMatch) {
        throw new Error("请输入有效的抖音公开直播间链接");
      }
      if (streamers.some((item) => item.roomUrl === input.roomUrl)) {
        throw new Error("该直播间已经存在");
      }
      const streamer: Streamer = {
        id: Date.now(),
        name: input.name.trim(),
        roomUrl: input.roomUrl.trim(),
        roomId: roomMatch[1],
        monitorEnabled: input.monitorEnabled,
        archived: false,
        liveStatus: "checking",
        monitorStatus: input.monitorEnabled ? "waiting" : "paused",
        lastCheckedAt: null,
        lastError: null,
        currentVideoCount: 0,
        historyVideoCount: 0,
      };
      streamers = [...streamers, streamer];
      saveStreamers();
      return streamer;
    },
    updateStreamer: async (id, input) => {
      const roomMatch = input.roomUrl.match(/live\.douyin\.com\/(\d+)/);
      if (!roomMatch) throw new Error("请输入有效的抖音公开直播间链接");
      const current = streamers.find((item) => item.id === id);
      if (!current) throw new Error("找不到主播");
      const updated: Streamer = {
        ...current,
        name: input.name.trim(),
        roomUrl: input.roomUrl.trim(),
        roomId: roomMatch[1],
        monitorEnabled: input.monitorEnabled,
        liveStatus: "checking",
        monitorStatus: input.monitorEnabled ? "waiting" : "paused",
      };
      streamers = streamers.map((item) => item.id === id ? updated : item);
      saveStreamers();
      return updated;
    },
    setMonitorEnabled: async (id, enabled) => {
      streamers = streamers.map((item) =>
        item.id === id
          ? {
              ...item,
              monitorEnabled: enabled,
              monitorStatus: enabled ? "waiting" : "paused",
            }
          : item,
      );
      saveStreamers();
    },
    checkStreamerNow: async (id) => {
      streamers = streamers.map((item) =>
        item.id === id
          ? { ...item, liveStatus: "offline", lastCheckedAt: new Date().toISOString() }
          : item,
      );
      saveStreamers();
    },
    archiveStreamer: async (id) => {
      streamers = streamers.filter((item) => item.id !== id);
      saveStreamers();
    },
    stopRecording: async () => undefined,
    listVideos: async (_streamerId, page = 1, pageSize = 50): Promise<VideoPage> => ({
      items: [],
      total: 0,
      page,
      pageSize,
    }),
    listCurrentVideos: async (_streamerId): Promise<VideoPage> => ({
      items: [],
      total: 0,
      page: 1,
      pageSize: 50,
    }),
    getSettings: async () => settings,
    saveSettings: async (nextSettings) => {
      settings = nextSettings;
      localStorage.setItem("dy-screen-settings", JSON.stringify(settings));
    },
    openVideo: async () => undefined,
    revealVideo: async () => undefined,
    deleteVideo: async () => undefined,
    deleteSession: async () => undefined,
    openLogs: async () => undefined,
    diagnoseEnvironment: async (): Promise<EnvironmentStatus> => ({
      ffmpeg: false,
      ffprobe: false,
    }),
    requestExit: async () => undefined,
    subscribe: async () => () => undefined,
  };
}

export const clientApi: ClientApi = isTauriRuntime() ? tauriApi : createBrowserApi();
