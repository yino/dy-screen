import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type {
  AiEnvironmentDiagnostic,
  AiExportResult,
  AiImportBatch,
  AiHighlightCandidate,
  AiHighlightRun,
  AiJobEvent,
  AiProject,
  AiProjectDetail,
  AiProjectSummary,
  AiSessionOption,
  AiSessionImportResult,
  AiTranscriptProjection,
  AiTrustedFileGrant,
  AppSettings,
  BrowserAccessState,
  ClientApi,
  CreateStreamerInput,
  Dashboard,
  EnvironmentStatus,
  RuntimeResourceView,
  RuntimeResourceEvent,
  LlmProviderSettings,
  MonitorEvent,
  PreviewSnapshot,
  ProviderDiagnostic,
  Streamer,
  StreamerPromptContext,
  StreamerTag,
  StreamerTagInput,
  ThumbnailBatch,
  ThumbnailEvent,
  ThumbnailSnapshot,
  VideoPage,
} from "./types";

const defaultSettings: AppSettings = {
  outputRoot: "~/Downloads/dy-screen",
  quality: "HD1",
  protocol: "flv",
  segmentSeconds: 900,
  maxConcurrentRecordings: 4,
  asrDuringRecording: true,
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
  listStreamerTagNameSuggestions: (limit = 20) =>
    invoke<string[]>("list_streamer_tag_name_suggestions", { limit }),
  getStreamerPromptContext: (id) =>
    invoke<StreamerPromptContext>("get_streamer_prompt_context", { id }),
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
  requestVideoThumbnails: (videoIds) =>
    invoke<ThumbnailBatch>("request_video_thumbnails", { videoIds }),
  getVideoThumbnails: (batchId) =>
    invoke<ThumbnailBatch>("get_video_thumbnails", { batchId }),
  releaseVideoThumbnailBatch: (batchId) =>
    invoke<void>("release_video_thumbnail_batch", { batchId }),
  retryVideoThumbnail: (batchId, videoId) =>
    invoke<ThumbnailSnapshot>("retry_video_thumbnail", { batchId, videoId }),
  openVideo: (id) => invoke<void>("open_video", { id }),
  revealVideo: (id) => invoke<void>("reveal_video", { id }),
  deleteVideo: (id) => invoke<void>("delete_video", { id }),
  deleteSession: (sessionId) => invoke<void>("delete_session", { sessionId }),
  openLogs: () => invoke<void>("open_logs"),
  diagnoseEnvironment: () => invoke<EnvironmentStatus>("diagnose_environment"),
  runtimeResourceStatus: () => invoke<RuntimeResourceView>("runtime_resource_status"),
  runtimeResourceManifest: () => invoke<RuntimeResourceView>("runtime_resource_manifest"),
  runtimeResourceDownload: () => invoke<RuntimeResourceView>("runtime_resource_download"),
  runtimeResourceCancel: () => invoke<void>("runtime_resource_cancel"),
  runtimeResourceRecheck: () => invoke<RuntimeResourceView>("runtime_resource_recheck"),
  runtimeResourceSource: () => invoke<string>("runtime_resource_source"),
  subscribeRuntimeResources: async (listener) => {
    const unlisten = await listen<RuntimeResourceEvent>("runtime-resource-event", (event) => listener(event.payload));
    return unlisten;
  },
  getBrowserAccessState: () => invoke<BrowserAccessState>("get_browser_access_state"),
  showDouyinVerification: () => invoke<void>("show_douyin_verification"),
  recheckDouyinAccess: () => invoke<BrowserAccessState>("recheck_douyin_access"),
  clearDouyinSession: (confirmed) =>
    invoke<BrowserAccessState>("clear_douyin_session", { confirmed }),
  listAiProjects: () => invoke<AiProject[]>("ai_list_projects"),
  getAiProject: (projectId) => invoke<AiProjectDetail>("ai_get_project", { projectId }),
  createAiProject: (input) => invoke<AiProject>("ai_create_project", { input }),
  renameAiProject: (projectId, name) => invoke<AiProject>("ai_rename_project", { projectId, name }),
  setAiProjectContext: (projectId, tags, analysisGoal) =>
    invoke<AiProject>("ai_set_project_context", { projectId, tags, analysisGoal }),
  deleteAiProject: (projectId) => invoke<void>("ai_delete_project", { projectId }),
  pickAiLocalVideos: () => invoke<AiTrustedFileGrant[]>("ai_pick_local_videos"),
  importAiLocalGrants: (projectId, grantIds) =>
    invoke<AiImportBatch>("ai_import_local_grants", { projectId, grantIds }),
  listAiCompletedSessions: (limit = 100) =>
    invoke<AiSessionOption[]>("ai_list_completed_sessions", { limit }),
  addAiCompletedSession: (projectId, sessionId) =>
    invoke<AiSessionImportResult>("ai_add_completed_session", { projectId, sessionId }),
  reorderAiInputs: (projectId, orderedIds) =>
    invoke<AiProjectDetail>("ai_reorder_inputs", { projectId, orderedIds }),
  removeAiInput: (projectId, inputId) =>
    invoke<AiProjectDetail>("ai_remove_input", { projectId, inputId }),
  getAiProjectSummary: (projectId) =>
    invoke<AiProjectSummary>("ai_project_summary", { projectId }),
  startAiProject: (projectId) => invoke<AiProject>("ai_start_project", { projectId }),
  cancelAiProject: (projectId) => invoke<AiProject>("ai_cancel_project", { projectId }),
  promoteAiNextInput: (inputId) => invoke<AiProjectDetail>("ai_promote_next_input", { inputId }),
  preemptAiWithInput: (inputId, confirmed) =>
    invoke<AiProjectDetail>("ai_preempt_with_input", { inputId, confirmed }),
  retryAiInput: (inputId) => invoke<AiProjectDetail>("ai_retry_input", { inputId }),
  queryAiTranscript: (projectId) =>
    invoke<AiTranscriptProjection>("ai_query_transcript", { projectId }),
  copyAiSegmentText: (projectId, stableSegmentId) =>
    invoke<string>("ai_copy_segment_text", { projectId, stableSegmentId }),
  copyAiInputText: (projectId, inputId) =>
    invoke<string>("ai_copy_input_text", { projectId, inputId }),
  copyAiProjectText: (projectId) =>
    invoke<string>("ai_copy_project_text", { projectId }),
  exportAiTxt: (projectId) => invoke<AiExportResult>("ai_export_txt", { projectId }),
  exportAiJson: (projectId) => invoke<AiExportResult>("ai_export_json", { projectId }),
  diagnoseAiEnvironment: () =>
    invoke<AiEnvironmentDiagnostic>("ai_diagnose_environment"),
  getAiLlmSettings: () => invoke<LlmProviderSettings>("ai_get_llm_settings"),
  saveAiLlmSettings: (settings, apiKey) =>
    invoke<LlmProviderSettings>("ai_save_llm_settings", { settings, apiKey: apiKey ?? null }),
  clearAiLlmKey: () => invoke<void>("ai_clear_llm_key"),
  diagnoseAiLlmProvider: () => invoke<ProviderDiagnostic>("ai_diagnose_llm_provider"),
  startAiHighlightAnalysis: (projectId, confirmed) =>
    invoke<AiHighlightRun>("ai_start_highlight_analysis", { projectId, confirmed }),
  listAiHighlightCandidates: (runId) =>
    invoke<AiHighlightCandidate[]>("ai_list_highlight_candidates", { runId }),
  selectAiHighlightCandidates: (runId, candidateIds) =>
    invoke<AiHighlightCandidate[]>("ai_select_highlight_candidates", { runId, candidateIds }),
  requestAiInputPreview: (projectId, inputId) =>
    invoke<PreviewSnapshot>("request_ai_input_preview", { projectId, inputId }),
  retryAiInputPreview: (projectId, inputId) =>
    invoke<PreviewSnapshot>("retry_ai_input_preview", { projectId, inputId }),
  requestExit: (force) => invoke<void>("request_exit", { force }),
  subscribe: async (listener) => {
    const unlisten = await listen<MonitorEvent>("monitor-event", (event) => {
      listener(event.payload);
    });
    return unlisten;
  },
  subscribeBrowserAccess: async (listener) => {
    const unlisten = await listen<BrowserAccessState>("browser-access-event", (event) => {
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
  subscribeThumbnail: async (listener) => {
    const unlisten = await listen<ThumbnailEvent>("video-thumbnail-event", (event) => {
      listener(event.payload);
    });
    return unlisten;
  },
  subscribeAi: async (listener) => {
    const unlisten = await listen<AiJobEvent>("ai-job-event", (event) => {
      listener(event.payload);
    });
    return unlisten;
  },
};

export function createBrowserApi(): ClientApi {
  let streamers: Streamer[] = [];
  let settings = defaultSettings;
  const thumbnailBatches = new Map<string, ThumbnailBatch>();
  const desktopAccessUnavailable = (): BrowserAccessState => ({
    status: "session_expired",
    pendingCount: 0,
    activeStreamerId: null,
    currentWebRid: null,
    lastReason: "真实访问验证仅桌面端可用",
    updatedAt: new Date().toISOString(),
  });
  const rejectDesktopAccess = (): Promise<never> =>
    Promise.reject(new Error("真实访问验证仅桌面端可用"));
  try {
    streamers = (JSON.parse(window.localStorage.getItem("dy-screen-streamers") || "[]") as Streamer[])
      .map((item) => ({
        ...item,
        sourceKind: item.sourceKind ?? "room",
        sourceUrl: item.sourceUrl ?? item.roomUrl ?? "",
        profileSecUid: item.profileSecUid ?? null,
        webRid: item.webRid ?? item.roomUrl?.match(/live\.douyin\.com\/(\d+)/)?.[1] ?? null,
        roomUrl: item.roomUrl ?? null,
        roomId: item.roomId ?? null,
        failureCount: Number.isFinite(item.failureCount) ? Math.max(0, item.failureCount) : 0,
        nextRetryAt: item.nextRetryAt ?? null,
        tags: Array.isArray(item.tags)
          ? item.tags.map((tag, index) => ({
              id: Number.isFinite(tag.id) ? tag.id : -(index + 1),
              name: String(tag.name ?? "").trim(),
              promptGuidance: tag.promptGuidance?.trim() || null,
              sortOrder: index,
            })).filter((tag) => tag.name)
          : [],
      }));
    settings = {
      ...defaultSettings,
      ...(JSON.parse(
        window.localStorage.getItem("dy-screen-settings") || "{}",
      ) as Partial<AppSettings>),
    };
  } catch {
    streamers = [];
  }

  const saveStreamers = () => {
    window.localStorage.setItem("dy-screen-streamers", JSON.stringify(streamers));
  };

  const dashboard = (): Dashboard => ({
    streamers: [...streamers],
    activeRecordings: streamers.filter((item) => item.monitorStatus === "recording").length,
    currentVideoCount: streamers.reduce((total, item) => total + item.currentVideoCount, 0),
  });

  const normalizeTags = (tags: StreamerTagInput[]): StreamerTag[] => {
    if (tags.length > 10) throw new Error("每个主播最多可以设置 10 个标签");
    const names = new Set<string>();
    return tags.map((tag, index) => {
      if (tag.name.includes("\n") || tag.name.includes("\r") || tag.promptGuidance?.match(/[\u0000-\u001f\u007f]/)) {
        throw new Error("标签名称和指导不能包含控制字符");
      }
      const name = tag.name.trim();
      const promptGuidance = tag.promptGuidance?.trim() || null;
      if (!name) throw new Error("标签名称不能为空");
      if ([...name].length > 24) throw new Error("标签名称不能超过 24 个字符");
      if (promptGuidance && [...promptGuidance].length > 500) throw new Error("标签指导不能超过 500 个字符");
      const key = name.toLocaleLowerCase();
      if (names.has(key)) throw new Error("同一主播不能设置重复标签");
      names.add(key);
      return {
        id: -(index + 1),
        name,
        promptGuidance,
        sortOrder: index,
      };
    });
  };

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
        failureCount: 0,
        nextRetryAt: null,
        currentVideoCount: 0,
        historyVideoCount: 0,
        tags: normalizeTags(input.tags),
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
        lastError: null,
        failureCount: 0,
        nextRetryAt: null,
        tags: normalizeTags(input.tags),
      };
      streamers = streamers.map((item) => item.id === id ? updated : item);
      saveStreamers();
      return updated;
    },
    listStreamerTagNameSuggestions: async (limit = 20) => {
      const seen = new Set<string>();
      const suggestions: string[] = [];
      for (const streamer of [...streamers].reverse()) {
        for (const tag of streamer.tags) {
          const key = tag.name.toLocaleLowerCase();
          if (seen.has(key)) continue;
          seen.add(key);
          suggestions.push(tag.name);
          if (suggestions.length >= Math.max(0, Math.min(limit, 100))) return suggestions;
        }
      }
      return suggestions;
    },
    getStreamerPromptContext: async (id) => {
      const streamer = streamers.find((item) => item.id === id);
      if (!streamer) throw new Error("找不到主播");
      return {
        streamerId: streamer.id,
        streamerName: streamer.name,
        tags: streamer.tags.map((tag) => ({
          name: tag.name,
          promptGuidance: tag.promptGuidance,
          priority: tag.sortOrder,
        })),
      };
    },
    setMonitorEnabled: async (id, enabled) => {
      streamers = streamers.map((item) =>
        item.id === id
          ? {
              ...item,
              monitorEnabled: enabled,
              monitorStatus: enabled ? "waiting" : "paused",
              lastError: null,
              failureCount: 0,
              nextRetryAt: null,
            }
          : item,
      );
      saveStreamers();
    },
    checkStreamerNow: async (id) => {
      streamers = streamers.map((item) =>
        item.id === id
          ? {
              ...item,
              liveStatus: "offline",
              monitorStatus: item.monitorEnabled ? "waiting" : "paused",
              lastCheckedAt: new Date().toISOString(),
              lastError: null,
              failureCount: 0,
              nextRetryAt: null,
            }
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
      window.localStorage.setItem("dy-screen-settings", JSON.stringify(settings));
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
    requestVideoThumbnails: async (videoIds): Promise<ThumbnailBatch> => {
      const batchId = `browser-thumbnail-${Date.now()}`;
      const batch: ThumbnailBatch = {
        batchId,
        items: videoIds.map((videoId) => ({
          batchId,
          videoId,
          cacheKey: null,
          state: "unavailable",
          media: null,
          errorCode: "browser_thumbnail_unavailable",
          errorMessage: "浏览器演示模式不能读取本地视频首帧",
        })),
      };
      thumbnailBatches.set(batchId, batch);
      return batch;
    },
    getVideoThumbnails: async (batchId) => {
      const batch = thumbnailBatches.get(batchId);
      if (!batch) throw new Error("找不到浏览器演示封面批次");
      return batch;
    },
    releaseVideoThumbnailBatch: async (batchId) => {
      thumbnailBatches.delete(batchId);
    },
    retryVideoThumbnail: async (batchId, videoId): Promise<ThumbnailSnapshot> => {
      const batch = thumbnailBatches.get(batchId);
      const item = batch?.items.find((candidate) => candidate.videoId === videoId);
      if (!item) throw new Error("找不到浏览器演示封面状态");
      return item;
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
    runtimeResourceStatus: async (): Promise<RuntimeResourceView> => ({
      status: "ready",
      ready: true,
      bundleVersion: "browser-demo",
      platform: "browser-demo",
      appMinVersion: "0.2.0",
      manifestSha256: null,
      components: [],
      totalSizeBytes: 0,
      minimumFreeDiskBytes: 0,
      minimumMemoryBytes: 0,
      source: "浏览器演示模式",
      downloadedBytes: 0,
      errorCode: null,
      errorMessage: null,
      updatedAt: new Date().toISOString(),
    }),
    runtimeResourceManifest: async () => (await (createBrowserApi() as ClientApi).runtimeResourceStatus!()),
    runtimeResourceDownload: async () => { throw new Error("浏览器演示模式不下载桌面运行资源"); },
    runtimeResourceCancel: async () => undefined,
    runtimeResourceRecheck: async () => (await (createBrowserApi() as ClientApi).runtimeResourceStatus!()),
    runtimeResourceSource: async () => "浏览器演示模式",
    subscribeRuntimeResources: async () => () => undefined,
    getBrowserAccessState: async () => desktopAccessUnavailable(),
    showDouyinVerification: rejectDesktopAccess,
    recheckDouyinAccess: rejectDesktopAccess,
    clearDouyinSession: async () => rejectDesktopAccess(),
    listAiProjects: async () => [],
    getAiProject: async () => {
      throw new Error("浏览器演示模式没有本地 AI 项目数据库");
    },
    createAiProject: async () => {
      throw new Error("请在 Tauri 桌面客户端中创建本地 AI 项目");
    },
    renameAiProject: async () => {
      throw new Error("浏览器演示模式不能修改本地 AI 项目");
    },
    setAiProjectContext: async () => {
      throw new Error("浏览器演示模式不能修改本地 AI 项目");
    },
    deleteAiProject: async () => undefined,
    pickAiLocalVideos: async () => [],
    importAiLocalGrants: async () => ({ added: [], rejected: [] }),
    listAiCompletedSessions: async () => [],
    addAiCompletedSession: async () => {
      throw new Error("浏览器演示模式不能读取录像会话");
    },
    reorderAiInputs: async () => {
      throw new Error("浏览器演示模式不能修改本地 AI 项目");
    },
    removeAiInput: async () => {
      throw new Error("浏览器演示模式不能修改本地 AI 项目");
    },
    getAiProjectSummary: async () => {
      throw new Error("浏览器演示模式没有本地 AI 项目");
    },
    startAiProject: async () => {
      throw new Error("浏览器演示模式不能执行本地 ASR");
    },
    cancelAiProject: async () => {
      throw new Error("浏览器演示模式没有运行中的 ASR");
    },
    promoteAiNextInput: async () => {
      throw new Error("浏览器演示模式没有本地 ASR 队列");
    },
    preemptAiWithInput: async () => {
      throw new Error("浏览器演示模式没有本地 ASR 队列");
    },
    retryAiInput: async () => {
      throw new Error("浏览器演示模式不能执行本地 ASR");
    },
    queryAiTranscript: async () => {
      throw new Error("浏览器演示模式没有本地转写结果");
    },
    copyAiSegmentText: async () => "",
    copyAiInputText: async () => "",
    copyAiProjectText: async () => "",
    exportAiTxt: async () => ({ saved: false }),
    exportAiJson: async () => ({ saved: false }),
    diagnoseAiEnvironment: async (): Promise<AiEnvironmentDiagnostic> => ({
      ready: false,
      platform: "browser-demo",
      engineId: "whisper.cpp",
      engineVersion: "v1.9.1",
      modelId: "whisper-small-multilingual-q5_1",
      modelVersion: "",
      checks: [{
        code: "desktop_required",
        passed: false,
        message: "浏览器演示模式不能访问随包 ASR 资源",
      }],
      message: "请在 Tauri 桌面客户端中使用本地语音识别",
    }),
    getAiLlmSettings: async (): Promise<LlmProviderSettings> => ({
      provider: "deepseek",
      modelId: "deepseek-chat",
      timeoutMs: 30_000,
      promptVersion: "highlight-v1",
      keyConfigured: false,
      updatedAt: null,
    }),
    saveAiLlmSettings: async () => {
      throw new Error("浏览器演示模式不会保存 API Key");
    },
  clearAiLlmKey: async () => undefined,
  diagnoseAiLlmProvider: async () => ({ ok: false, category: "browser_demo", message: "浏览器演示模式不会调用 DeepSeek" }),
    startAiHighlightAnalysis: async () => {
      throw new Error("浏览器演示模式不会调用 DeepSeek");
    },
    listAiHighlightCandidates: async () => [],
    selectAiHighlightCandidates: async () => [],
    requestAiInputPreview: async (_projectId, inputId): Promise<PreviewSnapshot> => ({
      requestId: `browser-ai-preview-${inputId}`,
      videoId: -inputId,
      state: "failed",
      progressPercent: null,
      message: "浏览器演示模式不能读取本地视频",
      media: null,
      errorCode: "browser_preview_unavailable",
      errorMessage: "请在 Tauri 桌面客户端中使用播放器",
    }),
    retryAiInputPreview: async (_projectId, inputId): Promise<PreviewSnapshot> => ({
      requestId: `browser-ai-preview-${inputId}`,
      videoId: -inputId,
      state: "failed",
      progressPercent: null,
      message: "浏览器演示模式不能读取本地视频",
      media: null,
      errorCode: "browser_preview_unavailable",
      errorMessage: "请在 Tauri 桌面客户端中使用播放器",
    }),
    requestExit: async () => undefined,
    subscribe: async () => () => undefined,
    subscribeBrowserAccess: async () => () => undefined,
    subscribePreview: async () => () => undefined,
    subscribeThumbnail: async () => () => undefined,
    subscribeAi: async () => () => undefined,
  };
}

export const clientApi: ClientApi = isTauriRuntime() ? tauriApi : createBrowserApi();
