import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type {
  AppSettings,
  ClientApi,
  CreateStreamerInput,
  Dashboard,
  EnvironmentStatus,
  MonitorEvent,
  PreviewSnapshot,
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
  requestVideoPreview: (id) => invoke<PreviewSnapshot>("request_video_preview", { id }),
  retryVideoPreview: (id) => invoke<PreviewSnapshot>("retry_video_preview", { id }),
  getVideoPreview: (requestId) => invoke<PreviewSnapshot>("get_video_preview", { requestId }),
  retainVideoPreview: (requestId) => invoke<void>("retain_video_preview", { requestId }),
  releaseVideoPreview: (requestId) => invoke<void>("release_video_preview", { requestId }),
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
  subscribePreview: async (listener) => {
    const unlisten = await listen<PreviewSnapshot>("video-preview-event", (event) => {
      listener(event.payload);
    });
    return unlisten;
  },
};

function createBrowserApi(): ClientApi {
  let streamers: Streamer[] = [];
  let settings = defaultSettings;
  try {
    streamers = (JSON.parse(localStorage.getItem("dy-screen-streamers") || "[]") as Streamer[])
      .map((item) => ({
        ...item,
        sourceKind: item.sourceKind ?? "room",
        sourceUrl: item.sourceUrl ?? item.roomUrl ?? "",
        profileSecUid: item.profileSecUid ?? null,
        webRid: item.webRid ?? item.roomUrl?.match(/live\.douyin\.com\/(\d+)/)?.[1] ?? null,
        roomUrl: item.roomUrl ?? null,
        roomId: item.roomId ?? null,
      }));
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
      const sourceUrl = input.sourceUrl.trim();
      const profileMatch = sourceUrl.match(/(?:www\.)?douyin\.com\/user\/([^/?#]+)/);
      const roomMatch = sourceUrl.match(/live\.douyin\.com\/(\d+)/);
      if (!profileMatch && !roomMatch) throw new Error("请输入有效的个人主页或直播间链接");
      if (profileMatch && streamers.some((item) => item.profileSecUid === profileMatch[1])) {
        throw new Error("该个人主页已经存在");
      }
      if (roomMatch && streamers.some((item) => item.webRid === roomMatch[1])) {
        throw new Error("该稳定直播入口已经存在");
      }
      if (roomMatch && !input.name.trim()) throw new Error("直播间链接必须填写主播名称");
      const sourceKind = profileMatch ? "profile" : "room";
      const normalizedSourceUrl = profileMatch
        ? `https://www.douyin.com/user/${profileMatch[1]}`
        : `https://live.douyin.com/${roomMatch![1]}`;
      const streamer: Streamer = {
        id: Date.now(),
        name: input.name.trim() || "个人主页主播（演示）",
        sourceKind,
        sourceUrl: normalizedSourceUrl,
        profileSecUid: profileMatch?.[1] ?? null,
        webRid: roomMatch?.[1] ?? null,
        roomUrl: roomMatch ? normalizedSourceUrl : null,
        roomId: roomMatch?.[1] ?? null,
        monitorEnabled: input.monitorEnabled,
        archived: false,
        liveStatus: profileMatch ? "offline" : "checking",
        monitorStatus: input.monitorEnabled
          ? profileMatch ? "waiting_first_live" : "waiting"
          : "paused",
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
      const sourceUrl = input.sourceUrl.trim();
      const profileMatch = sourceUrl.match(/(?:www\.)?douyin\.com\/user\/([^/?#]+)/);
      const roomMatch = sourceUrl.match(/live\.douyin\.com\/(\d+)/);
      if (!profileMatch && !roomMatch) throw new Error("请输入有效的个人主页或直播间链接");
      if (roomMatch && !input.name.trim()) throw new Error("直播间链接必须填写主播名称");
      const current = streamers.find((item) => item.id === id);
      if (!current) throw new Error("找不到主播");
      const sourceKind = profileMatch ? "profile" : "room";
      const normalizedSourceUrl = profileMatch
        ? `https://www.douyin.com/user/${profileMatch[1]}`
        : `https://live.douyin.com/${roomMatch![1]}`;
      const updated: Streamer = {
        ...current,
        name: input.name.trim() || current.name,
        sourceKind,
        sourceUrl: normalizedSourceUrl,
        profileSecUid: profileMatch?.[1] ?? null,
        webRid: roomMatch?.[1] ?? null,
        roomUrl: roomMatch ? normalizedSourceUrl : null,
        roomId: roomMatch?.[1] ?? null,
        monitorEnabled: input.monitorEnabled,
        liveStatus: profileMatch ? "offline" : "checking",
        monitorStatus: input.monitorEnabled
          ? profileMatch ? "waiting_first_live" : "waiting"
          : "paused",
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
    requestVideoPreview: async (id): Promise<PreviewSnapshot> => ({
      requestId: `browser-preview-${id}`,
      videoId: id,
      state: "failed",
      progressPercent: null,
      message: "浏览器演示模式不能读取本地视频",
      media: null,
      errorCode: "browser_preview_unavailable",
      errorMessage: "请在 Tauri 桌面客户端中使用内置视频预览",
    }),
    retryVideoPreview: async (id): Promise<PreviewSnapshot> => ({
      requestId: `browser-preview-${id}`,
      videoId: id,
      state: "failed",
      progressPercent: null,
      message: "浏览器演示模式不能读取本地视频",
      media: null,
      errorCode: "browser_preview_unavailable",
      errorMessage: "请在 Tauri 桌面客户端中使用内置视频预览",
    }),
    getVideoPreview: async () => {
      throw new Error("浏览器演示模式没有预览任务");
    },
    retainVideoPreview: async () => undefined,
    releaseVideoPreview: async () => undefined,
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
    subscribePreview: async () => () => undefined,
  };
}

export const clientApi: ClientApi = isTauriRuntime() ? tauriApi : createBrowserApi();
