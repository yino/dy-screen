import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { App } from "./App";
import type {
  AiEnvironmentDiagnostic,
  AiProject,
  AiProjectDetail,
  ActivationState,
  BrowserAccessState,
  ClientApi,
  Streamer,
  Video,
} from "./types";

const streamer: Streamer = {
  id: 1,
  name: "小鱼直播间",
  sourceKind: "room",
  sourceUrl: "https://live.douyin.com/452086788686",
  profileSecUid: null,
  webRid: "452086788686",
  roomUrl: "https://live.douyin.com/452086788686",
  roomId: "7663779905489947411",
  monitorEnabled: true,
  archived: false,
  liveStatus: "offline",
  monitorStatus: "waiting",
  lastCheckedAt: "2026-07-18T20:00:00Z",
  lastError: null,
  failureCount: 0,
  nextRetryAt: null,
  currentVideoCount: 0,
  historyVideoCount: 3,
  tags: [],
};

const completedVideo: Video = {
  id: 31,
  sessionId: 8,
  streamerId: streamer.id,
  streamerName: streamer.name,
  path: "/tmp/preview-source.mkv",
  startedAt: "2026-07-18T20:00:00Z",
  endedAt: "2026-07-18T20:01:00Z",
  durationSeconds: 60,
  sizeBytes: 1024,
  audioPresent: true,
  status: "complete",
};

const aiProject: AiProject = {
  id: 101,
  name: "直播转写项目",
  status: "draft",
  recognitionProfile: {
    engineId: "whisper.cpp",
    engineVersion: "v1.9.1",
    modelId: "whisper-small-multilingual-q5_1",
    modelVersion: "small-q5_1",
    languageHint: "zh",
    vadModelId: "silero-v6.2.0",
    vadThresholdMillis: 500,
    vadPaddingMs: 500,
    timestampPolicy: "segment",
    normalizationVersion: "OpenCC ver.1.4.1",
    hotwords: [],
  },
  recognitionProfileHash: "profile-hash",
  inputFrozen: false,
  progressPercent: 0,
  lastErrorCode: null,
  lastErrorMessage: null,
  createdAt: "2026-07-22T00:00:00Z",
  updatedAt: "2026-07-22T00:00:00Z",
};

const emptyAiDetail: AiProjectDetail = { project: aiProject, inputs: [] };

const readyAiEnvironment: AiEnvironmentDiagnostic = {
  ready: true,
  platform: "macos-aarch64",
  engineId: "whisper.cpp",
  engineVersion: "v1.9.1",
  modelId: "whisper-small-multilingual-q5_1",
  modelVersion: "small-q5_1",
  checks: [{ code: "asr_model", passed: true, message: "ASR 模型完整" }],
  message: "本地 ASR 环境就绪，识别过程不会上传视频",
};

const nativeAccessState: BrowserAccessState = {
  status: "native",
  pendingCount: 0,
  activeStreamerId: null,
  currentWebRid: null,
  lastReason: null,
  updatedAt: "2026-07-24T00:00:00Z",
};

const activeActivation: ActivationState = {
  configured: true,
  active: true,
  status: "active",
  message: null,
  deviceIdHint: "…12345678",
  lastHeartbeatAt: "2026-07-27T00:00:00Z",
  nextHeartbeatAt: "2026-07-27T00:01:30Z",
};

function createApi(streamers: Streamer[] = [], videos: Video[] = []): ClientApi {
  return {
    getActivationState: vi.fn().mockResolvedValue(activeActivation),
    activateClient: vi.fn().mockResolvedValue(activeActivation),
    clearActivation: vi.fn().mockResolvedValue({
      ...activeActivation,
      configured: false,
      active: false,
      status: "missing",
    }),
    getDashboard: vi.fn().mockResolvedValue({
      streamers,
      activeRecordings: 0,
      currentVideoCount: 0,
    }),
    createStreamer: vi.fn().mockResolvedValue(streamer),
    updateStreamer: vi.fn().mockResolvedValue(streamer),
    listStreamerTagNameSuggestions: vi.fn().mockResolvedValue([]),
    getStreamerPromptContext: vi.fn().mockResolvedValue({
      streamerId: streamer.id,
      streamerName: streamer.name,
      tags: [],
    }),
    setMonitorEnabled: vi.fn().mockResolvedValue(undefined),
    checkStreamerNow: vi.fn().mockResolvedValue(undefined),
    archiveStreamer: vi.fn().mockResolvedValue(undefined),
    stopRecording: vi.fn().mockResolvedValue(undefined),
    listVideos: vi.fn().mockImplementation(async (_streamerId, page = 1, pageSize = 50, filters = {}) => {
      const filtered = videos.filter((video) => !filters.status || video.status === filters.status);
      return { items: filtered, total: filtered.length, page, pageSize };
    }),
    listCurrentVideos: vi.fn().mockResolvedValue({ items: videos, total: videos.length, page: 1, pageSize: 50 }),
    getSettings: vi.fn().mockResolvedValue({
      outputRoot: "~/Downloads/dy-screen",
      quality: "HD1",
      protocol: "flv",
      segmentSeconds: 900,
      maxConcurrentRecordings: 4,
      asrDuringRecording: true,
      ffmpegPath: "ffmpeg",
      ffprobePath: "ffprobe",
      notificationsEnabled: true,
      autostartEnabled: false,
    }),
    saveSettings: vi.fn().mockResolvedValue(undefined),
    requestVideoPreview: vi.fn().mockRejectedValue("测试未配置视频预览"),
    retryVideoPreview: vi.fn().mockRejectedValue("测试未配置视频预览重试"),
    getVideoPreview: vi.fn().mockRejectedValue("测试未配置视频预览查询"),
    retainVideoPreview: vi.fn().mockResolvedValue(undefined),
    releaseVideoPreview: vi.fn().mockResolvedValue(undefined),
    requestVideoThumbnails: vi.fn().mockImplementation(async (videoIds: number[]) => ({
      batchId: "thumbnail-test-batch",
      items: videoIds.map((videoId) => ({
        batchId: "thumbnail-test-batch",
        videoId,
        cacheKey: null,
        state: "unavailable" as const,
        media: null,
        errorCode: "test_thumbnail_unavailable",
        errorMessage: "测试未配置视频封面",
      })),
    })),
    getVideoThumbnails: vi.fn().mockRejectedValue("测试未配置视频封面查询"),
    releaseVideoThumbnailBatch: vi.fn().mockResolvedValue(undefined),
    retryVideoThumbnail: vi.fn().mockRejectedValue("测试未配置视频封面重试"),
    openVideo: vi.fn().mockResolvedValue(undefined),
    revealVideo: vi.fn().mockResolvedValue(undefined),
    deleteVideo: vi.fn().mockResolvedValue(undefined),
    deleteSession: vi.fn().mockResolvedValue(undefined),
    openLogs: vi.fn().mockResolvedValue(undefined),
    diagnoseEnvironment: vi.fn().mockResolvedValue({ ffmpeg: true, ffprobe: true }),
    getBrowserAccessState: vi.fn().mockResolvedValue(nativeAccessState),
    showDouyinVerification: vi.fn().mockResolvedValue(undefined),
    recheckDouyinAccess: vi.fn().mockResolvedValue(nativeAccessState),
    clearDouyinSession: vi.fn().mockResolvedValue({
      ...nativeAccessState,
      status: "session_expired",
      lastReason: "抖音浏览器会话已清除",
    }),
    listAiProjects: vi.fn().mockResolvedValue([]),
    getAiProject: vi.fn().mockResolvedValue(emptyAiDetail),
    createAiProject: vi.fn().mockResolvedValue(aiProject),
    renameAiProject: vi.fn().mockResolvedValue(aiProject),
    deleteAiProject: vi.fn().mockResolvedValue(undefined),
    pickAiLocalVideos: vi.fn().mockResolvedValue([]),
    importAiLocalGrants: vi.fn().mockResolvedValue({ added: [], rejected: [] }),
    listAiCompletedSessions: vi.fn().mockResolvedValue([]),
    addAiCompletedSession: vi.fn().mockResolvedValue({
      detail: emptyAiDetail,
      addedCount: 0,
      duplicateCount: 0,
      unavailableCount: 0,
    }),
    reorderAiInputs: vi.fn().mockResolvedValue(emptyAiDetail),
    removeAiInput: vi.fn().mockResolvedValue(emptyAiDetail),
    getAiProjectSummary: vi.fn().mockResolvedValue({
      totalInputs: 0,
      validInputs: 0,
      unavailableInputs: 0,
      totalDurationMs: 0,
      engineId: "whisper.cpp",
      modelId: "whisper-small-multilingual-q5_1",
      environmentReady: true,
      environmentMessage: "本地 ASR 环境就绪",
    }),
    startAiProject: vi.fn().mockResolvedValue({ ...aiProject, status: "queued" }),
    cancelAiProject: vi.fn().mockResolvedValue({ ...aiProject, status: "cancelled" }),
    retryAiInput: vi.fn().mockResolvedValue(emptyAiDetail),
    queryAiTranscript: vi.fn().mockResolvedValue({ project: aiProject, inputs: [] }),
    copyAiSegmentText: vi.fn().mockResolvedValue(""),
    copyAiInputText: vi.fn().mockResolvedValue(""),
    copyAiProjectText: vi.fn().mockResolvedValue(""),
    exportAiTxt: vi.fn().mockResolvedValue({ saved: true }),
    exportAiJson: vi.fn().mockResolvedValue({ saved: true }),
    diagnoseAiEnvironment: vi.fn().mockResolvedValue(readyAiEnvironment),
    requestAiInputPreview: vi.fn().mockRejectedValue("测试未配置 AI 视频预览"),
    retryAiInputPreview: vi.fn().mockRejectedValue("测试未配置 AI 视频预览重试"),
    requestExit: vi.fn().mockResolvedValue(undefined),
    subscribe: vi.fn().mockResolvedValue(() => undefined),
    subscribeBrowserAccess: vi.fn().mockResolvedValue(() => undefined),
    subscribePreview: vi.fn().mockResolvedValue(() => undefined),
    subscribeThumbnail: vi.fn().mockResolvedValue(() => undefined),
    subscribeAi: vi.fn().mockResolvedValue(() => undefined),
    subscribeActivation: vi.fn().mockResolvedValue(() => undefined),
  };
}

describe("App", () => {
  it("未激活时显示不可关闭的门禁，激活成功后进入主界面", async () => {
    const user = userEvent.setup();
    const api = createApi([streamer]);
    api.getActivationState = vi.fn().mockResolvedValue({
      ...activeActivation,
      configured: false,
      active: false,
      status: "missing",
      message: "请输入激活码后继续使用",
      lastHeartbeatAt: null,
      nextHeartbeatAt: null,
    });
    api.activateClient = vi.fn().mockResolvedValue(activeActivation);

    render(<App api={api} />);

    const dialog = await screen.findByRole("dialog", { name: "客户端激活" });
    expect(dialog).toHaveTextContent("未激活前不会启动监听或录制任务");
    expect(screen.queryByRole("button", { name: "关闭" })).not.toBeInTheDocument();
    await user.type(screen.getByLabelText("激活码"), "TEST-CODE-1234");
    await user.click(screen.getByRole("button", { name: "激活并进入" }));
    await waitFor(() => expect(api.activateClient).toHaveBeenCalledWith("TEST-CODE-1234"));
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "客户端激活" })).not.toBeInTheDocument());
  });

  it("资源未就绪时只显示准备页，下载完成后解锁监控导航", async () => {
    const user = userEvent.setup();
    const readyStatus = {
      status: "ready" as const,
      ready: true,
      bundleVersion: "2026.07.26",
      platform: "macos-aarch64",
      appMinVersion: "0.2.0",
      manifestSha256: "manifest-hash",
      components: [],
      totalSizeBytes: 1024,
      minimumFreeDiskBytes: 2048,
      minimumMemoryBytes: 4096,
      source: "固定 HTTPS 资源服务器",
      downloadedBytes: 1024,
      errorCode: null,
      errorMessage: null,
      updatedAt: "2026-07-26T00:00:00Z",
    };
    const api = createApi([streamer]);
    api.runtimeResourceStatus = vi.fn().mockResolvedValue({
      ...readyStatus,
      status: "failed" as const,
      ready: false,
      downloadedBytes: 0,
      errorCode: "resource_not_checked",
      errorMessage: "正在检查本地运行资源",
    });
    api.runtimeResourceDownload = vi.fn().mockResolvedValue(readyStatus);
    api.runtimeResourceRecheck = vi.fn().mockResolvedValue(readyStatus);
    api.runtimeResourceCancel = vi.fn().mockResolvedValue(undefined);
    api.subscribeRuntimeResources = vi.fn().mockResolvedValue(() => undefined);

    render(<App api={api} />);

    expect(await screen.findByRole("heading", { name: "准备本地运行资源" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "监控中心" })).toBeDisabled();
    await user.click(screen.getByRole("button", { name: "下载并安装资源" }));
    expect(await screen.findByRole("heading", { name: "监控中心" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "监控中心" })).not.toBeDisabled();
  });

  it("需要访问验证时显示全局横幅并由用户主动打开窗口", async () => {
    const user = userEvent.setup();
    const api = createApi([{
      ...streamer,
      monitorStatus: "verification_required",
      liveStatus: "error",
      lastError: "抖音公开页需要完成访问验证",
    }]);
    api.getBrowserAccessState = vi.fn().mockResolvedValue({
      ...nativeAccessState,
      status: "verification_required",
      pendingCount: 2,
      activeStreamerId: streamer.id,
      currentWebRid: streamer.webRid,
      lastReason: "抖音公开页需要完成访问验证",
    });

    render(<App api={api} />);

    const banner = await screen.findByRole("alert", { name: "抖音访问状态" });
    expect(banner).toHaveTextContent("需要访问验证");
    expect(banner).toHaveTextContent("等待处理 2 个直播间");
    expect(banner).toHaveTextContent("抖音公开页需要完成访问验证");
    expect((await screen.findAllByTestId("streamer-row"))[0]).toHaveTextContent("需要访问验证");
    expect(api.showDouyinVerification).not.toHaveBeenCalled();

    await user.click(screen.getAllByRole("button", { name: "立即验证" })[0]);
    expect(api.showDouyinVerification).toHaveBeenCalledTimes(1);
  });

  it("浏览器解析状态展示排队数量和当前主播且会话恢复后刷新", async () => {
    let accessListener: ((state: BrowserAccessState) => void) | undefined;
    const api = createApi([streamer]);
    api.getBrowserAccessState = vi.fn().mockResolvedValue({
      ...nativeAccessState,
      status: "browser_resolving",
      pendingCount: 3,
      activeStreamerId: streamer.id,
      currentWebRid: streamer.webRid,
      lastReason: "原生请求触发访问限制，已切换浏览器解析",
    });
    api.subscribeBrowserAccess = vi.fn().mockImplementation(async (listener) => {
      accessListener = listener;
      return () => undefined;
    });

    render(<App api={api} />);

    expect(await screen.findByRole("status", { name: "抖音访问状态" })).toHaveTextContent("等待处理 3 个直播间");
    expect((await screen.findAllByTestId("streamer-row"))[0]).toHaveTextContent("浏览器解析中");

    await act(async () => {
      accessListener?.({
        ...nativeAccessState,
        status: "session_ready",
        lastReason: "浏览器访问会话已恢复",
      });
    });

    expect(await screen.findByRole("status", { name: "抖音访问状态" })).toHaveTextContent("浏览器会话可用");
    await waitFor(() => expect(api.getDashboard).toHaveBeenCalledTimes(2));
  });

  it("拒绝让延迟返回的初始化查询覆盖更新的浏览器访问事件", async () => {
    let accessListener: ((state: BrowserAccessState) => void) | undefined;
    let resolveInitial: ((state: BrowserAccessState) => void) | undefined;
    const api = createApi([streamer]);
    api.getBrowserAccessState = vi.fn().mockImplementation(() => new Promise((resolve) => {
      resolveInitial = resolve;
    }));
    api.subscribeBrowserAccess = vi.fn().mockImplementation(async (listener) => {
      accessListener = listener;
      return () => undefined;
    });

    render(<App api={api} />);
    await waitFor(() => expect(accessListener).toBeDefined());

    await act(async () => {
      accessListener?.({
        ...nativeAccessState,
        status: "verification_required",
        pendingCount: 1,
        activeStreamerId: streamer.id,
        currentWebRid: streamer.webRid,
        lastReason: "抖音公开页需要完成访问验证",
        updatedAt: "2026-07-24T00:01:00Z",
      });
    });
    expect(await screen.findByRole("alert", { name: "抖音访问状态" })).toHaveTextContent("需要访问验证");

    await act(async () => {
      resolveInitial?.({
        ...nativeAccessState,
        updatedAt: "2026-07-24T00:00:00Z",
      });
    });

    expect(screen.getByRole("alert", { name: "抖音访问状态" })).toHaveTextContent("需要访问验证");
    expect(screen.getByRole("alert", { name: "抖音访问状态" })).toHaveTextContent("等待处理 1 个直播间");
  });

  it("设置页可以检查、重新验证并确认清除浏览器会话", async () => {
    const user = userEvent.setup();
    const api = createApi();
    api.getBrowserAccessState = vi.fn().mockResolvedValue({
      ...nativeAccessState,
      status: "session_ready",
      lastReason: "浏览器访问会话已恢复",
    });
    const confirm = vi.spyOn(window, "confirm").mockReturnValue(true);

    render(<App api={api} />);
    await user.click(screen.getByRole("button", { name: "设置" }));
    expect(await screen.findByText("抖音访问会话")).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "检查访问状态" }));
    expect(api.recheckDouyinAccess).toHaveBeenCalledTimes(1);
    await user.click(screen.getByRole("button", { name: "重新验证" }));
    expect(api.showDouyinVerification).toHaveBeenCalledTimes(1);
    await user.click(screen.getByRole("button", { name: "清除抖音会话" }));
    expect(confirm).toHaveBeenCalledWith(expect.stringContaining("不会删除主播、录像或设置"));
    expect(api.clearDouyinSession).toHaveBeenCalledWith(true);
    confirm.mockRestore();
  });

  it("首次启动时显示添加主播空状态", async () => {
    render(<App api={createApi()} />);
    expect(await screen.findByText("还没有监控主播")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "添加主播" })).toBeInTheDocument();
  });

  it("添加直播间表单提交名称和统一来源链接", async () => {
    const user = userEvent.setup();
    const api = createApi();
    render(<App api={api} />);

    await user.click(await screen.findByRole("button", { name: "添加主播" }));
    await user.type(screen.getByLabelText("主播名称"), "小鱼直播间");
    await user.type(
      screen.getByLabelText("个人主页或直播间链接"),
      "https://live.douyin.com/452086788686",
    );
    await user.click(screen.getByRole("button", { name: "保存并监听" }));

    expect(api.createStreamer).toHaveBeenCalledWith({
      name: "小鱼直播间",
      sourceUrl: "https://live.douyin.com/452086788686",
      monitorEnabled: true,
      tags: [],
    });
  });

  it("个人主页允许名称为空并交给后端使用主页昵称", async () => {
    const user = userEvent.setup();
    const api = createApi();
    render(<App api={api} />);

    await user.click(await screen.findByRole("button", { name: "添加主播" }));
    await user.type(
      screen.getByLabelText("个人主页或直播间链接"),
      "https://www.douyin.com/user/profile-sec-uid",
    );
    await user.click(screen.getByRole("button", { name: "保存并监听" }));

    expect(api.createStreamer).toHaveBeenCalledWith({
      name: "",
      sourceUrl: "https://www.douyin.com/user/profile-sec-uid",
      monitorEnabled: true,
      tags: [],
    });
  });

  it("添加带货和搞笑标签并按调整后的顺序提交", async () => {
    const user = userEvent.setup();
    const api = createApi();
    render(<App api={api} />);

    await user.click(await screen.findByRole("button", { name: "添加主播" }));
    await user.type(screen.getByLabelText("主播名称"), "标签主播");
    await user.type(
      screen.getByLabelText("个人主页或直播间链接"),
      "https://live.douyin.com/452086788686",
    );
    await user.click(screen.getByRole("button", { name: "添加标签" }));
    await user.type(screen.getByLabelText("标签 1 名称"), "带货");
    await user.type(screen.getByLabelText("标签 1 指导"), "重点提取商品卖点");
    await user.click(screen.getByRole("button", { name: "添加标签" }));
    await user.type(screen.getByLabelText("标签 2 名称"), "搞笑");
    await user.click(screen.getByRole("button", { name: "上移标签 2" }));
    await user.click(screen.getByRole("button", { name: "保存并监听" }));

    expect(api.createStreamer).toHaveBeenCalledWith({
      name: "标签主播",
      sourceUrl: "https://live.douyin.com/452086788686",
      monitorEnabled: true,
      tags: [
        { name: "搞笑", promptGuidance: null },
        { name: "带货", promptGuidance: "重点提取商品卖点" },
      ],
    });
  });

  it("可以删除标签并使用已有标签名称建议", async () => {
    const user = userEvent.setup();
    const api = createApi();
    api.listStreamerTagNameSuggestions = vi.fn().mockResolvedValue(["带货", "搞笑"]);
    render(<App api={api} />);

    await user.click(await screen.findByRole("button", { name: "添加主播" }));
    await user.click(await screen.findByRole("button", { name: "使用标签建议 带货" }));
    expect(screen.getByLabelText("标签 1 名称")).toHaveValue("带货");
    await user.click(screen.getByRole("button", { name: "删除标签 1" }));
    expect(screen.queryByLabelText("标签 1 名称")).not.toBeInTheDocument();
  });

  it("标签达到十个后阻止继续添加并展示数量提示", async () => {
    const user = userEvent.setup();
    render(<App api={createApi()} />);

    await user.click(await screen.findByRole("button", { name: "添加主播" }));
    const addTag = screen.getByRole("button", { name: "添加标签" });
    for (let index = 0; index < 10; index += 1) await user.click(addTag);

    expect(addTag).toBeDisabled();
    expect(screen.getByText("已达到每个主播 10 个标签的上限")).toBeInTheDocument();
    expect(screen.getByText(/不生成 Prompt、不调用 LLM/)).toBeInTheDocument();
    expect(screen.getByText(/浏览器演示模式仅保存到 localStorage，不写入 SQLite/)).toBeInTheDocument();
  });

  it("后端拒绝标签时保持弹窗和用户输入", async () => {
    const user = userEvent.setup();
    const api = createApi();
    api.createStreamer = vi.fn().mockRejectedValue({
      code: "streamer_tag_duplicate",
      message: "同一主播不能设置重复标签",
      field: "tags",
      existingStreamerId: null,
    });
    render(<App api={api} />);

    await user.click(await screen.findByRole("button", { name: "添加主播" }));
    await user.type(screen.getByLabelText("主播名称"), "保留内容主播");
    await user.type(
      screen.getByLabelText("个人主页或直播间链接"),
      "https://live.douyin.com/452086788686",
    );
    await user.click(screen.getByRole("button", { name: "添加标签" }));
    await user.type(screen.getByLabelText("标签 1 名称"), "带货");
    await user.click(screen.getByRole("button", { name: "保存并监听" }));

    expect(await screen.findByText("同一主播不能设置重复标签")).toBeInTheDocument();
    expect(screen.getByRole("form", { name: "添加监控主播" })).toBeInTheDocument();
    expect(screen.getByLabelText("标签 1 名称")).toHaveValue("带货");
    expect(screen.getByLabelText("主播名称")).toHaveValue("保留内容主播");
  });

  it("主播表格折叠多余标签并在详情展示完整指导", async () => {
    const tagged: Streamer = {
      ...streamer,
      tags: [
        { id: 1, name: "带货", promptGuidance: "重点提取商品卖点", sortOrder: 0 },
        { id: 2, name: "搞笑", promptGuidance: null, sortOrder: 1 },
        { id: 3, name: "知识", promptGuidance: "关注解释", sortOrder: 2 },
        { id: 4, name: "生活", promptGuidance: null, sortOrder: 3 },
      ],
    };
    render(<App api={createApi([tagged])} />);

    const row = (await screen.findAllByTestId("streamer-row"))[0];
    expect(row).toHaveTextContent("带货");
    expect(row).toHaveTextContent("搞笑");
    expect(row).toHaveTextContent("+2");
    expect(screen.getByText("重点提取商品卖点")).toBeInTheDocument();
    expect(screen.getByText("关注解释")).toBeInTheDocument();
  });

  it("无标签主播详情展示可操作空状态", async () => {
    render(<App api={createApi([streamer])} />);
    expect(await screen.findByText("尚未设置标签")).toBeInTheDocument();
  });

  it("直播间直连仍要求填写主播名称", async () => {
    const user = userEvent.setup();
    const api = createApi();
    render(<App api={api} />);

    await user.click(await screen.findByRole("button", { name: "添加主播" }));
    await user.type(
      screen.getByLabelText("个人主页或直播间链接"),
      "https://live.douyin.com/452086788686",
    );
    await user.click(screen.getByRole("button", { name: "保存并监听" }));

    expect(await screen.findByText("直播间链接必须填写主播名称")).toBeInTheDocument();
    expect(api.createStreamer).not.toHaveBeenCalled();
  });

  it("将录制中的主播排在等待开播之前", async () => {
    const recording = { ...streamer, id: 2, name: "录制主播", liveStatus: "live", monitorStatus: "recording" } as Streamer;
    render(<App api={createApi([streamer, recording])} />);
    const rows = await screen.findAllByTestId("streamer-row");
    expect(rows[0]).toHaveTextContent("录制主播");
  });

  it("AI 剪辑无项目时显示本地分析空状态且不会自动读取视频", async () => {
    const user = userEvent.setup();
    const api = createApi();
    render(<App api={api} />);
    await user.click(screen.getByRole("button", { name: "AI 剪辑" }));
    expect(await screen.findByText("还没有 AI 分析项目")).toBeInTheDocument();
    expect(screen.getByText("视频和转写全程保留在本机")).toBeInTheDocument();
    expect(api.pickAiLocalVideos).not.toHaveBeenCalled();
    expect(api.requestAiInputPreview).not.toHaveBeenCalled();
  });

  it("AI 环境不可用时保留项目浏览并展示重新检测和安装修复说明", async () => {
    const user = userEvent.setup();
    const api = createApi();
    api.listAiProjects = vi.fn().mockResolvedValue([aiProject]);
    api.diagnoseAiEnvironment = vi.fn().mockResolvedValue({
      ...readyAiEnvironment,
      ready: false,
      checks: [{ code: "asr_model", passed: false, message: "ASR 模型损坏" }],
      message: "ASR 模型损坏，请重新安装应用",
    });
    render(<App api={api} />);
    await user.click(screen.getByRole("button", { name: "AI 剪辑" }));
    expect(await screen.findByText("ASR 模型损坏，请重新安装应用")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "重新检测" })).toBeInTheDocument();
    expect(screen.getByText("修复方式：重新安装当前版本应用")).toBeInTheDocument();
  });

  it("AI 工作区可以创建草稿并规范化提交热词", async () => {
    const user = userEvent.setup();
    const api = createApi();
    render(<App api={api} />);
    await user.click(screen.getByRole("button", { name: "AI 剪辑" }));
    await user.click(await screen.findByRole("button", { name: "创建项目" }));
    await user.type(screen.getByLabelText("项目名称"), "  商品直播  ");
    await user.type(screen.getByLabelText("识别热词"), "主播名, 商品名");
    await user.click(screen.getByRole("button", { name: "创建草稿" }));
    expect(api.createAiProject).toHaveBeenCalledWith({
      name: "商品直播",
      hotwords: ["主播名", "商品名"],
    });
  });

  it("AI 草稿通过系统选择器导入多个视频并可选择已结束直播会话", async () => {
    const user = userEvent.setup();
    const api = createApi();
    api.listAiProjects = vi.fn().mockResolvedValue([aiProject]);
    api.getAiProject = vi.fn().mockResolvedValue(emptyAiDetail);
    api.pickAiLocalVideos = vi.fn().mockResolvedValue([
      { grantId: "grant-a", displayName: "第一段.mp4" },
      { grantId: "grant-b", displayName: "第二段.mkv" },
    ]);
    api.listAiCompletedSessions = vi.fn().mockResolvedValue([{
      sessionId: 88,
      streamerName: "小鱼直播间",
      startedAt: "2026-07-21T20:00:00Z",
      endedAt: "2026-07-21T22:00:00Z",
      videoCount: 8,
      totalDurationMs: 7_200_000,
      unavailableVideoCount: 0,
    }]);
    render(<App api={api} />);
    await user.click(screen.getByRole("button", { name: "AI 剪辑" }));
    await user.click(await screen.findByRole("button", { name: "添加本地视频" }));
    await waitFor(() => expect(api.importAiLocalGrants).toHaveBeenCalledWith(
      aiProject.id,
      ["grant-a", "grant-b"],
    ));
    await user.selectOptions(screen.getByLabelText("选择已结束直播"), "88");
    await user.click(screen.getByRole("button", { name: "添加整场直播" }));
    expect(api.addAiCompletedSession).toHaveBeenCalledWith(aiProject.id, 88);
  });

  it("活动录制退出事件确认后请求安全退出", async () => {
    let listener: ((event: { kind: string; streamerId: number | null }) => void) | undefined;
    const api = createApi();
    api.subscribe = vi.fn().mockImplementation(async (callback) => {
      listener = callback;
      return () => undefined;
    });
    vi.spyOn(window, "confirm").mockReturnValue(true);
    render(<App api={api} />);
    await screen.findByText("还没有监控主播");

    await act(async () => {
      listener?.({ kind: "exit_confirmation_requested", streamerId: null });
    });

    await waitFor(() => expect(api.requestExit).toHaveBeenCalledWith(true));
  });

  it("历史视频库可以按文件状态筛选", async () => {
    const user = userEvent.setup();
    const complete: Video = {
      id: 1,
      sessionId: 1,
      streamerId: streamer.id,
      streamerName: streamer.name,
      path: "/tmp/complete.mkv",
      startedAt: "2026-07-18T20:00:00Z",
      endedAt: null,
      durationSeconds: 900,
      sizeBytes: 1024,
      audioPresent: true,
      status: "complete",
    };
    const missing = { ...complete, id: 2, path: "/tmp/missing.mkv", status: "missing" };
    render(<App api={createApi([streamer], [complete, missing])} />);

    await user.click(screen.getByRole("button", { name: "视频库" }));
    await screen.findByText("complete.mkv");
    await user.selectOptions(screen.getByLabelText("按状态筛选"), "missing");

    expect(screen.getByText("missing.mkv")).toBeInTheDocument();
    expect(screen.queryByText("complete.mkv")).not.toBeInTheDocument();
  });

  it("页面导航不会重复注册后端事件监听", async () => {
    const user = userEvent.setup();
    const api = createApi();
    render(<App api={api} />);
    await screen.findByText("还没有监控主播");

    await user.click(screen.getByRole("button", { name: "AI 剪辑" }));
    await user.click(screen.getByRole("button", { name: "监控中心" }));

    expect(api.subscribe).toHaveBeenCalledTimes(1);
  });

  it("AI 工作区切换菜单后保留状态且不会重新加载", async () => {
    const user = userEvent.setup();
    const api = createApi();
    render(<App api={api} />);

    await user.click(screen.getByRole("button", { name: "AI 剪辑" }));
    await user.click(await screen.findByRole("button", { name: "创建项目" }));
    const projectName = screen.getByLabelText("项目名称");
    await user.type(projectName, "保留中的项目");
    await waitFor(() => expect(api.listAiProjects).toHaveBeenCalledTimes(1));

    await user.click(screen.getByRole("button", { name: "监控中心" }));
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "AI 剪辑" }));

    expect(await screen.findByLabelText("项目名称")).toHaveValue("保留中的项目");
    expect(api.listAiProjects).toHaveBeenCalledTimes(1);
  });

  it("历史视频库按每页 50 条翻页", async () => {
    const user = userEvent.setup();
    const api = createApi([streamer]);
    const videoForPage = (page: number): Video => ({
      id: page,
      sessionId: page,
      streamerId: streamer.id,
      streamerName: streamer.name,
      path: "/tmp/page-" + page + ".mkv",
      startedAt: "2026-07-18T20:00:00Z",
      endedAt: null,
      durationSeconds: 900,
      sizeBytes: 1024,
      audioPresent: true,
      status: "complete",
    });
    api.listVideos = vi.fn().mockImplementation(async (_streamerId, page = 1) => ({
      items: [videoForPage(page)],
      total: 51,
      page,
      pageSize: 50,
    }));
    render(<App api={api} />);

    await user.click(screen.getByRole("button", { name: "视频库" }));
    await screen.findByText("page-1.mkv");
    await user.click(screen.getByRole("button", { name: "下一页" }));

    expect(await screen.findByText("page-2.mkv")).toBeInTheDocument();
  });

  it("历史视频按会话分组并可一次删除整个会话", async () => {
    const user = userEvent.setup();
    const videos: Video[] = [1, 2].map((id) => ({
      id,
      sessionId: 9,
      streamerId: streamer.id,
      streamerName: streamer.name,
      path: `/tmp/session-9-${id}.mkv`,
      startedAt: `2026-07-18T20:0${id}:00Z`,
      endedAt: null,
      durationSeconds: 60,
      sizeBytes: 1024,
      audioPresent: true,
      status: "complete",
    }));
    const deleteSession = vi.fn().mockResolvedValue(undefined);
    const api = {
      ...createApi([streamer], videos),
      deleteSession,
    } as ClientApi & { deleteSession(sessionId: number): Promise<void> };
    vi.spyOn(window, "confirm").mockReturnValue(true);
    render(<App api={api} />);

    await user.click(screen.getByRole("button", { name: "视频库" }));
    expect(await screen.findByText("会话 #9")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "删除会话 #9" }));

    expect(deleteSession).toHaveBeenCalledWith(9);
  });

  it("保留 Tauri 返回的中文字符串错误", async () => {
    const user = userEvent.setup();
    const api = createApi();
    api.createStreamer = vi.fn().mockRejectedValue("该直播间已经存在");
    render(<App api={api} />);

    await user.click(await screen.findByRole("button", { name: "添加主播" }));
    await user.type(screen.getByLabelText("主播名称"), "重复主播");
    await user.type(
      screen.getByLabelText("个人主页或直播间链接"),
      "https://live.douyin.com/452086788686",
    );
    await user.click(screen.getByRole("button", { name: "保存并监听" }));

    expect(await screen.findByText("该直播间已经存在")).toBeInTheDocument();
  });

  it("展示后端结构化来源字段错误", async () => {
    const user = userEvent.setup();
    const api = createApi();
    api.createStreamer = vi.fn().mockRejectedValue({
      code: "duplicate_profile",
      message: "该个人主页已经存在，主播 ID 为 8",
      field: "sourceUrl",
      existingStreamerId: 8,
    });
    render(<App api={api} />);

    await user.click(await screen.findByRole("button", { name: "添加主播" }));
    await user.type(
      screen.getByLabelText("个人主页或直播间链接"),
      "https://www.douyin.com/user/profile-duplicate",
    );
    await user.click(screen.getByRole("button", { name: "保存并监听" }));

    expect(await screen.findByText("该个人主页已经存在，主播 ID 为 8")).toBeInTheDocument();
  });

  it("表格展示主页来源和发现阶段，未发现时禁用直播间入口", async () => {
    const waitingProfile = {
      ...streamer,
      id: 9,
      name: "等待主页主播",
      sourceKind: "profile",
      sourceUrl: "https://www.douyin.com/user/profile-waiting",
      profileSecUid: "profile-waiting",
      webRid: null,
      roomUrl: null,
      roomId: null,
      monitorStatus: "waiting_first_live",
    } as Streamer;

    render(<App api={createApi([waitingProfile])} />);

    const row = (await screen.findAllByTestId("streamer-row"))[0];
    expect(row).toHaveTextContent("个人主页");
    expect(row).toHaveTextContent("等待首次开播");
    expect(screen.getByRole("button", { name: "直播间尚未发现" })).toBeDisabled();
  });

  it("表格覆盖正在发现、主页失败、已发现和重新发现状态", async () => {
    const statusStreamers = [
      ["正在发现", "discovering", null],
      ["主页失败", "profile_error", null],
      ["已发现", "waiting", "910"],
      ["重新发现", "rediscovering", null],
    ].map(([name, monitorStatus, webRid], index) => ({
      ...streamer,
      id: 20 + index,
      name,
      sourceKind: "profile",
      sourceUrl: `https://www.douyin.com/user/profile-${index}`,
      profileSecUid: `profile-${index}`,
      webRid,
      roomUrl: webRid ? `https://live.douyin.com/${webRid}` : null,
      roomId: webRid ? `room-${webRid}` : null,
      monitorStatus,
    })) as Streamer[];

    render(<App api={createApi(statusStreamers)} />);

    expect((await screen.findAllByText("正在发现直播间")).length).toBeGreaterThan(0);
    expect(screen.getAllByText("主页检查失败").length).toBeGreaterThan(0);
    expect(screen.getAllByText("等待开播").length).toBeGreaterThan(0);
    expect(screen.getAllByText("重新发现直播间").length).toBeGreaterThan(0);
  });

  it("表格直接展示房间诊断并提供恢复操作", async () => {
    const user = userEvent.setup();
    const diagnosticStreamers = [
      {
        ...streamer,
        id: 31,
        name: "页面变化主播",
        monitorStatus: "layout_changed",
        liveStatus: "error",
        lastError: "抖音直播间页面结构已变化，当前版本暂时无法解析",
        failureCount: 2,
        nextRetryAt: "2026-07-23T01:02:03Z",
      },
      {
        ...streamer,
        id: 32,
        name: "访问受限主播",
        monitorStatus: "access_restricted",
        liveStatus: "error",
        lastError: "抖音直播间需要登录、验证码或额外访问权限",
        failureCount: 1,
        nextRetryAt: "2026-07-23T01:03:03Z",
      },
    ] as Streamer[];
    const api = createApi(diagnosticStreamers);

    render(<App api={api} />);

    const changedRow = (await screen.findAllByText("页面变化主播"))[0].closest("tr");
    const restrictedRow = screen.getAllByText("访问受限主播")[0].closest("tr");
    expect(changedRow).toHaveTextContent("页面结构变化");
    expect(changedRow).toHaveTextContent("抖音直播间页面结构已变化，当前版本暂时无法解析");
    expect(changedRow).toHaveTextContent("连续 2 次");
    expect(changedRow).toHaveTextContent("预计重试");
    expect(restrictedRow).toHaveTextContent("访问受限");
    expect(restrictedRow).not.toHaveTextContent("直播入口失效");

    await user.click(screen.getByRole("button", { name: "页面变化主播操作" }));
    expect(screen.getByRole("link", { name: "从操作菜单打开直播间" })).toHaveAttribute(
      "href",
      "https://live.douyin.com/452086788686",
    );
    await user.click(screen.getByRole("button", { name: "打开日志目录" }));
    expect(api.openLogs).toHaveBeenCalledTimes(1);
  });

  it("已发现主页展示标准化直播间入口", async () => {
    const discovered = {
      ...streamer,
      sourceKind: "profile",
      sourceUrl: "https://www.douyin.com/user/profile-live",
      profileSecUid: "profile-live",
      webRid: "236150550962",
      roomUrl: "https://live.douyin.com/236150550962",
    } as Streamer;

    render(<App api={createApi([discovered])} />);

    const link = await screen.findByRole("link", { name: /打开直播间/ });
    expect(link).toHaveAttribute("href", "https://live.douyin.com/236150550962");
  });

  it("合并事件刷新列表并定位目标主播", async () => {
    let listener: ((event: { kind: string; streamerId: number | null }) => void) | undefined;
    const temporary = {
      ...streamer,
      id: 10,
      name: "临时主页",
      sourceKind: "profile",
      sourceUrl: "https://www.douyin.com/user/profile-temp",
      profileSecUid: "profile-temp",
      webRid: null,
      roomUrl: null,
      roomId: null,
      monitorStatus: "discovering",
    } as Streamer;
    const target = { ...streamer, id: 11, name: "合并目标" } as Streamer;
    const api = createApi([temporary, target]);
    api.getDashboard = vi
      .fn()
      .mockResolvedValueOnce({ streamers: [temporary, target], activeRecordings: 0, currentVideoCount: 0 })
      .mockResolvedValue({ streamers: [target], activeRecordings: 0, currentVideoCount: 0 });
    api.subscribe = vi.fn().mockImplementation(async (callback) => {
      listener = callback;
      return () => undefined;
    });
    render(<App api={api} />);
    expect((await screen.findAllByText("临时主页")).length).toBeGreaterThan(0);

    await act(async () => {
      listener?.({ kind: "streamer_merged", streamerId: target.id });
    });

    await waitFor(() => expect(screen.queryAllByText("临时主页")).toHaveLength(0));
    expect(screen.getAllByText("合并目标").length).toBeGreaterThan(0);
  });

  it("主播事件刷新为后端返回的最新标签顺序", async () => {
    let listener: ((event: { kind: string; streamerId: number | null }) => void) | undefined;
    const previous: Streamer = {
      ...streamer,
      tags: [
        { id: 1, name: "带货", promptGuidance: null, sortOrder: 0 },
        { id: 2, name: "搞笑", promptGuidance: null, sortOrder: 1 },
      ],
    };
    const latest: Streamer = {
      ...previous,
      tags: [
        { id: 2, name: "搞笑", promptGuidance: null, sortOrder: 0 },
        { id: 1, name: "带货", promptGuidance: null, sortOrder: 1 },
      ],
    };
    const api = createApi([previous]);
    api.getDashboard = vi
      .fn()
      .mockResolvedValueOnce({ streamers: [previous], activeRecordings: 0, currentVideoCount: 0 })
      .mockResolvedValue({ streamers: [latest], activeRecordings: 0, currentVideoCount: 0 });
    api.subscribe = vi.fn().mockImplementation(async (callback) => {
      listener = callback;
      return () => undefined;
    });
    render(<App api={api} />);
    const row = (await screen.findAllByTestId("streamer-row"))[0];
    expect([...row.querySelectorAll(".streamer-tag-badges span")].map((item) => item.textContent)).toEqual(["带货", "搞笑"]);

    await act(async () => {
      listener?.({ kind: "streamer_updated", streamerId: streamer.id });
    });

    await waitFor(() => {
      const refreshedRow = screen.getAllByTestId("streamer-row")[0];
      expect([...refreshedRow.querySelectorAll(".streamer-tag-badges span")].map((item) => item.textContent)).toEqual(["搞笑", "带货"]);
    });
  });

  it("从本次监听视频打开内置播放器", async () => {
    const user = userEvent.setup();
    const requestVideoPreview = vi.fn().mockResolvedValue({
      requestId: "preview-31",
      videoId: 31,
      state: "ready",
      progressPercent: 100,
      message: "视频可以播放",
      media: {
        path: "/tmp/cache/preview-31.mp4",
        mimeType: "video/mp4",
        cacheHit: false,
        generated: true,
        sourceMissing: false,
      },
      errorCode: null,
      errorMessage: null,
    });
    const api = {
      ...createApi([streamer], [completedVideo]),
      requestVideoPreview,
      getVideoPreview: vi.fn(),
      retryVideoPreview: vi.fn(),
      retainVideoPreview: vi.fn().mockResolvedValue(undefined),
      releaseVideoPreview: vi.fn().mockResolvedValue(undefined),
      subscribePreview: vi.fn().mockResolvedValue(() => undefined),
    } as ClientApi;
    render(<App api={api} />);

    await user.click(await screen.findByRole("button", { name: "预览视频" }));

    expect(requestVideoPreview).toHaveBeenCalledWith(31);
    expect(screen.getByRole("dialog", { name: "视频预览" })).toBeInTheDocument();
    expect(screen.getByLabelText("视频播放器")).toBeInTheDocument();
  });

  it("视频库与本次监听视频使用相同预览入口", async () => {
    const user = userEvent.setup();
    const requestVideoPreview = vi.fn().mockResolvedValue({
      requestId: "preview-31",
      videoId: 31,
      state: "queued",
      progressPercent: null,
      message: "预览任务已进入队列",
      media: null,
      errorCode: null,
      errorMessage: null,
    });
    const api = {
      ...createApi([streamer], [completedVideo]),
      requestVideoPreview,
      getVideoPreview: vi.fn(),
      retryVideoPreview: vi.fn(),
      retainVideoPreview: vi.fn().mockResolvedValue(undefined),
      releaseVideoPreview: vi.fn().mockResolvedValue(undefined),
      subscribePreview: vi.fn().mockResolvedValue(() => undefined),
    } as ClientApi;
    render(<App api={api} />);

    await user.click(screen.getByRole("button", { name: "视频库" }));
    await user.click(await screen.findByRole("button", { name: "预览 preview-source.mkv" }));

    expect(requestVideoPreview).toHaveBeenCalledWith(31);
    expect(screen.getAllByText("预览任务已进入队列").length).toBeGreaterThan(0);
  });

  it("预览失败时提供重试和系统播放器打开", async () => {
    const user = userEvent.setup();
    const retryVideoPreview = vi.fn().mockResolvedValue({
      requestId: "retry-31",
      videoId: 31,
      state: "queued",
      progressPercent: null,
      message: "预览任务已进入队列",
      media: null,
      errorCode: null,
      errorMessage: null,
    });
    const api = {
      ...createApi([streamer], [completedVideo]),
      requestVideoPreview: vi.fn().mockResolvedValue({
        requestId: "preview-31",
        videoId: 31,
        state: "failed",
        progressPercent: null,
        message: "视频预览准备失败",
        media: null,
        errorCode: "ffmpeg_missing",
        errorMessage: "无法启动 FFmpeg，请检查应用设置中的路径",
      }),
      getVideoPreview: vi.fn(),
      retryVideoPreview,
      retainVideoPreview: vi.fn().mockResolvedValue(undefined),
      releaseVideoPreview: vi.fn().mockResolvedValue(undefined),
      subscribePreview: vi.fn().mockResolvedValue(() => undefined),
    } as ClientApi;
    render(<App api={api} />);

    await user.click(await screen.findByRole("button", { name: "预览视频" }));
    expect(await screen.findByText("无法启动 FFmpeg，请检查应用设置中的路径")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "重试预览" }));
    expect(retryVideoPreview).toHaveBeenCalledWith(31);
    expect(screen.getByRole("button", { name: "使用系统播放器打开" })).toBeInTheDocument();
  });

  it("通过后端事件把排队状态恢复为可播放状态", async () => {
    const user = userEvent.setup();
    let previewListener: ((snapshot: any) => void) | undefined;
    const api = {
      ...createApi([streamer], [completedVideo]),
      requestVideoPreview: vi.fn().mockResolvedValue({
        requestId: "preview-event-31",
        videoId: 31,
        state: "queued",
        progressPercent: null,
        message: "预览任务已进入队列",
        media: null,
        errorCode: null,
        errorMessage: null,
      }),
      subscribePreview: vi.fn().mockImplementation(async (listener) => {
        previewListener = listener;
        return () => undefined;
      }),
    } as ClientApi;
    render(<App api={api} />);

    await user.click(await screen.findByRole("button", { name: "预览视频" }));
    await act(async () => {
      previewListener?.({
        requestId: "preview-event-31",
        videoId: 31,
        state: "ready",
        progressPercent: 100,
        message: "视频可以播放",
        media: {
          path: "/tmp/cache/preview-event-31.mp4",
          mimeType: "video/mp4",
          cacheHit: true,
          generated: true,
          sourceMissing: false,
        },
        errorCode: null,
        errorMessage: null,
      });
    });

    expect(screen.getByLabelText("视频播放器")).toBeInTheDocument();
    expect(screen.getByText("已使用预览缓存")).toBeInTheDocument();
  });

  it("关闭播放器时释放缓存播放引用", async () => {
    const user = userEvent.setup();
    const retainVideoPreview = vi.fn().mockResolvedValue(undefined);
    const releaseVideoPreview = vi.fn().mockResolvedValue(undefined);
    const api = {
      ...createApi([streamer], [completedVideo]),
      requestVideoPreview: vi.fn().mockResolvedValue({
        requestId: "preview-release-31",
        videoId: 31,
        state: "ready",
        progressPercent: 100,
        message: "视频可以播放",
        media: {
          path: "/tmp/cache/preview-release-31.mp4",
          mimeType: "video/mp4",
          cacheHit: false,
          generated: true,
          sourceMissing: false,
        },
        errorCode: null,
        errorMessage: null,
      }),
      retainVideoPreview,
      releaseVideoPreview,
    } as ClientApi;
    render(<App api={api} />);

    await user.click(await screen.findByRole("button", { name: "预览视频" }));
    await waitFor(() => expect(retainVideoPreview).toHaveBeenCalledWith("preview-release-31"));
    await user.click(screen.getByRole("button", { name: "关闭视频预览" }));

    await waitFor(() => expect(releaseVideoPreview).toHaveBeenCalledWith("preview-release-31"));
  });

  it("源文件缺失且没有缓存时禁用预览入口", async () => {
    const missing = {
      ...completedVideo,
      status: "missing",
      hasPreviewCache: false,
    } as Video & { hasPreviewCache: boolean };

    render(<App api={createApi([streamer], [missing])} />);

    expect(await screen.findByRole("button", { name: "预览视频" })).toBeDisabled();
  });

  it("源文件缺失但缓存有效时仍允许内置预览", async () => {
    const user = userEvent.setup();
    const missing = {
      ...completedVideo,
      status: "missing",
      hasPreviewCache: true,
    } as Video & { hasPreviewCache: boolean };
    const api = createApi([streamer], [missing]);
    api.requestVideoPreview = vi.fn().mockResolvedValue({
      requestId: "cached-missing-31",
      videoId: 31,
      state: "ready",
      progressPercent: 100,
      message: "预览缓存已准备完成",
      media: {
        path: "/tmp/cache/cached-missing-31.mp4",
        mimeType: "video/mp4",
        cacheHit: true,
        generated: true,
        sourceMissing: true,
      },
      errorCode: null,
      errorMessage: null,
    });

    render(<App api={api} />);
    await user.click(await screen.findByRole("button", { name: "预览视频" }));

    expect(api.requestVideoPreview).toHaveBeenCalledWith(31);
    expect(await screen.findByText("原文件缺失，仅播放缓存")).toBeInTheDocument();
  });

  it("忽略同一视频旧请求发出的迟到预览事件", async () => {
    const user = userEvent.setup();
    let previewListener: ((snapshot: any) => void) | undefined;
    const api = createApi([streamer], [completedVideo]);
    api.requestVideoPreview = vi.fn().mockResolvedValue({
      requestId: "current-request-31",
      videoId: 31,
      state: "queued",
      progressPercent: null,
      message: "当前请求仍在排队",
      media: null,
      errorCode: null,
      errorMessage: null,
    });
    api.subscribePreview = vi.fn().mockImplementation(async (listener) => {
      previewListener = listener;
      return () => undefined;
    });

    render(<App api={api} />);
    await user.click(await screen.findByRole("button", { name: "预览视频" }));
    expect(await screen.findAllByText("当前请求仍在排队")).not.toHaveLength(0);

    await act(async () => {
      previewListener?.({
        requestId: "old-request-31",
        videoId: 31,
        state: "ready",
        progressPercent: 100,
        message: "旧请求完成",
        media: {
          path: "/tmp/cache/old-request-31.mp4",
          mimeType: "video/mp4",
          cacheHit: true,
          generated: true,
          sourceMissing: false,
        },
        errorCode: null,
        errorMessage: null,
      });
    });

    expect(screen.queryByLabelText("视频播放器")).not.toBeInTheDocument();
    expect(screen.getAllByText("当前请求仍在排队")).not.toHaveLength(0);
  });

  it("媒体加载失败后可重新准备预览并清除旧错误", async () => {
    const user = userEvent.setup();
    const api = createApi([streamer], [completedVideo]);
    api.requestVideoPreview = vi.fn().mockResolvedValue({
      requestId: "broken-playback-31",
      videoId: 31,
      state: "ready",
      progressPercent: 100,
      message: "视频可以播放",
      media: {
        path: "/tmp/cache/broken-playback-31.mp4",
        mimeType: "video/mp4",
        cacheHit: true,
        generated: true,
        sourceMissing: false,
      },
      errorCode: null,
      errorMessage: null,
    });
    api.retryVideoPreview = vi.fn().mockResolvedValue({
      requestId: "repaired-playback-31",
      videoId: 31,
      state: "ready",
      progressPercent: 100,
      message: "视频可以播放",
      media: {
        path: "/tmp/cache/repaired-playback-31.mp4",
        mimeType: "video/mp4",
        cacheHit: false,
        generated: true,
        sourceMissing: false,
      },
      errorCode: null,
      errorMessage: null,
    });

    render(<App api={api} />);
    await user.click(await screen.findByRole("button", { name: "预览视频" }));
    fireEvent.error(await screen.findByLabelText("视频播放器"));
    expect(await screen.findByText("WebView 无法播放该预览文件，请尝试系统播放器")).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "重新准备预览" }));

    await waitFor(() => expect(api.retryVideoPreview).toHaveBeenCalledWith(31));
    expect(screen.queryByText("WebView 无法播放该预览文件，请尝试系统播放器")).not.toBeInTheDocument();
  });

  it("视频库渐进展示缓存封面并可点击复用现有预览", async () => {
    const user = userEvent.setup();
    const api = createApi([streamer], [completedVideo]);
    api.requestVideoThumbnails = vi.fn().mockResolvedValue({
      batchId: "cover-ready",
      items: [{
        batchId: "cover-ready",
        videoId: completedVideo.id,
        cacheKey: "a".repeat(64),
        state: "ready",
        media: {
          path: "/tmp/cache/cover.jpg",
          mimeType: "image/jpeg",
          width: 480,
          height: 270,
          cacheHit: true,
          sourceKind: "original",
        },
        errorCode: null,
        errorMessage: null,
      }],
    });
    api.requestVideoPreview = vi.fn().mockResolvedValue({
      requestId: "cover-preview",
      videoId: completedVideo.id,
      state: "queued",
      progressPercent: null,
      message: "预览任务已进入队列",
      media: null,
      errorCode: null,
      errorMessage: null,
    });
    render(<App api={api} />);

    await user.click(screen.getByRole("button", { name: "视频库" }));
    expect(await screen.findByAltText("preview-source.mkv 封面")).toHaveAttribute("src", "/tmp/cache/cover.jpg");
    await user.click(screen.getByRole("button", { name: "预览 preview-source.mkv 封面" }));

    expect(api.requestVideoPreview).toHaveBeenCalledWith(completedVideo.id);
  });

  it("封面事件只更新当前批次和当前源版本", async () => {
    const user = userEvent.setup();
    let thumbnailListener: ((event: any) => void) | undefined;
    const api = createApi([streamer], [completedVideo]);
    api.requestVideoThumbnails = vi.fn().mockResolvedValue({
      batchId: "cover-current",
      items: [{
        batchId: "cover-current",
        videoId: completedVideo.id,
        cacheKey: "b".repeat(64),
        state: "queued",
        media: null,
        errorCode: null,
        errorMessage: null,
      }],
    });
    api.subscribeThumbnail = vi.fn().mockImplementation(async (listener) => {
      thumbnailListener = listener;
      return () => undefined;
    });
    render(<App api={api} />);
    await user.click(screen.getByRole("button", { name: "视频库" }));
    expect(await screen.findByLabelText("正在加载封面 preview-source.mkv")).toBeInTheDocument();

    await act(async () => {
      thumbnailListener?.({
        batchId: "cover-current",
        item: {
          batchId: "cover-current",
          videoId: completedVideo.id,
          cacheKey: "old-source".padEnd(64, "0"),
          state: "ready",
          media: { path: "/tmp/cache/old.jpg", mimeType: "image/jpeg", width: 480, height: 270, cacheHit: false, sourceKind: "original" },
          errorCode: null,
          errorMessage: null,
        },
      });
    });
    expect(screen.queryByAltText("preview-source.mkv 封面")).not.toBeInTheDocument();

    await act(async () => {
      thumbnailListener?.({
        batchId: "cover-current",
        item: {
          batchId: "cover-current",
          videoId: completedVideo.id,
          cacheKey: "b".repeat(64),
          state: "ready",
          media: { path: "/tmp/cache/current.jpg", mimeType: "image/jpeg", width: 480, height: 270, cacheHit: false, sourceKind: "original" },
          errorCode: null,
          errorMessage: null,
        },
      });
    });
    expect(await screen.findByAltText("preview-source.mkv 封面")).toHaveAttribute("src", "/tmp/cache/current.jpg");
  });

  it("封面失败可显式重试且离开视频库会释放批次", async () => {
    const user = userEvent.setup();
    const api = createApi([streamer], [completedVideo]);
    api.requestVideoThumbnails = vi.fn().mockResolvedValue({
      batchId: "cover-failed",
      items: [{
        batchId: "cover-failed",
        videoId: completedVideo.id,
        cacheKey: "c".repeat(64),
        state: "failed",
        media: null,
        errorCode: "thumbnail_extract_failed",
        errorMessage: "无法提取视频封面",
      }],
    });
    api.retryVideoThumbnail = vi.fn().mockResolvedValue({
      batchId: "cover-failed",
      videoId: completedVideo.id,
      cacheKey: "c".repeat(64),
      state: "queued",
      media: null,
      errorCode: null,
      errorMessage: null,
    });
    render(<App api={api} />);

    await user.click(screen.getByRole("button", { name: "视频库" }));
    await user.click(await screen.findByRole("button", { name: "重试封面 preview-source.mkv" }));
    expect(api.retryVideoThumbnail).toHaveBeenCalledWith("cover-failed", completedVideo.id);

    await user.click(screen.getByRole("button", { name: "监控中心" }));
    await waitFor(() => expect(api.releaseVideoThumbnailBatch).toHaveBeenCalledWith("cover-failed"));
  });
});
