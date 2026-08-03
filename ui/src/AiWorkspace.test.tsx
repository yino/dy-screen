import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { AiWorkspace } from "./AiWorkspace";
import type {
  AiEnvironmentDiagnostic,
  AiClipProjectDetail,
  AiHighlightCandidatePage,
  AiHighlightCandidate,
  AiHighlightRun,
  AiProject,
  AiProjectDetail,
  AiReplayStreamerPage,
  AiTranscriptProjection,
  AiTranscriptSegment,
  ClientApi,
  PreviewSnapshot,
} from "./types";

const project: AiProject = {
  id: 201,
  name: "整场直播转写",
  status: "completed",
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
    hotwords: ["商品名"],
  },
  recognitionProfileHash: "profile-hash",
  inputFrozen: true,
  progressPercent: 100,
  lastErrorCode: null,
  lastErrorMessage: null,
  createdAt: "2026-07-22T00:00:00Z",
  updatedAt: "2026-07-22T00:10:00Z",
};

const completedInput = {
  id: 301,
  projectId: project.id,
  position: 0,
  sourceKind: "local_file" as const,
  videoId: null,
  displayName: "Windows 中文目录 很长很长的商品直播视频.mp4",
  durationMs: 10_000,
  audioPresent: true,
  projectOffsetMs: 0,
  status: "completed" as const,
  progressPercent: 100,
  artifactId: 9,
  lastErrorCode: null,
  lastErrorMessage: null,
};

const segments: AiTranscriptSegment[] = [
  {
    stableSegmentId: "seg-a",
    inputId: completedInput.id,
    videoId: null,
    sourceStartMs: 1_000,
    sourceEndMs: 2_500,
    projectStartMs: 1_000,
    projectEndMs: 2_500,
    rawText: "歡迎來到直播間",
    normalizedText: "欢迎来到直播间。",
    confidence: 0.92,
  },
  {
    stableSegmentId: "seg-b",
    inputId: completedInput.id,
    videoId: null,
    sourceStartMs: 3_000,
    sourceEndMs: 5_000,
    projectStartMs: 3_000,
    projectEndMs: 5_000,
    rawText: "今天價格九十九元",
    normalizedText: "今天价格99元。",
    confidence: 0.48,
  },
];

const detail: AiProjectDetail = { project, inputs: [completedInput] };

const transcript: AiTranscriptProjection = {
  project,
  inputs: [{
    inputId: completedInput.id,
    position: 0,
    sourceKind: "local_file",
    videoId: null,
    displayName: completedInput.displayName,
    durationMs: 10_000,
    projectOffsetMs: 0,
    status: "completed",
    progressPercent: 100,
    errorCode: null,
    errorMessage: null,
    gapDurationMs: null,
    segments,
  }],
};

const environment: AiEnvironmentDiagnostic = {
  ready: true,
  platform: "macos-aarch64",
  engineId: "whisper.cpp",
  engineVersion: "v1.9.1",
  modelId: "whisper-small-multilingual-q5_1",
  modelVersion: "small-q5_1",
  checks: [
    { code: "asr_model", passed: true, message: "ASR 模型完整" },
    { code: "available_memory", passed: true, message: "当前可用内存满足识别要求" },
  ],
  message: "本地 ASR 环境就绪，识别过程不会上传视频",
};

const completedHighlightRun: AiHighlightRun = {
  id: 801,
  projectId: project.id,
  status: "completed",
  modelId: "deepseek-chat",
  promptVersion: "highlight-v1",
  tagsSnapshot: [],
  skillsSnapshot: ["generic-hook@1.0.0"],
  analysisGoal: null,
  analysisFingerprint: "highlight-run-801",
  qualifiedScore: 70,
  excellentScore: 80,
  userAuthorized: true,
  totalSegments: 2,
  totalChars: 18,
  estimatedBatches: 1,
  totalTokens: 320,
  lastErrorCode: null,
  lastErrorMessage: null,
  createdAt: "2026-07-22T00:00:00Z",
  updatedAt: "2026-07-22T00:00:05Z",
};

const highlightCandidates: AiHighlightCandidate[] = [{
  id: 901,
  runId: completedHighlightRun.id,
  chunkId: 1,
  candidateKey: "candidate-qualified",
  title: "价格反转",
  inputId: completedInput.id,
  segmentIds: ["seg-a", "seg-b"],
  startMs: 1_000,
  endMs: 18_000,
  totalScore: 82,
  hookScore: 86,
  informationScore: 78,
  emotionScore: 73,
  tagRelevanceScore: 91,
  completenessScore: 80,
  shareabilityScore: 84,
  reason: "价格信息完整，并且有明确反转。",
  matchedTags: ["带货"],
  rank: 1,
  selected: false,
}, {
  id: 902,
  runId: completedHighlightRun.id,
  chunkId: 1,
  candidateKey: "candidate-reference",
  title: "普通互动",
  inputId: completedInput.id,
  segmentIds: ["seg-a", "seg-b"],
  startMs: 500,
  endMs: 17_500,
  totalScore: 64,
  hookScore: 61,
  informationScore: 58,
  emotionScore: 69,
  tagRelevanceScore: 55,
  completenessScore: 72,
  shareabilityScore: 65,
  reason: "内容完整，但吸引力和标签相关性较弱。",
  matchedTags: [],
  rank: 2,
  selected: false,
}];

const readyPreview: PreviewSnapshot = {
  requestId: "ai-preview-ready",
  videoId: -completedInput.id,
  state: "ready",
  progressPercent: 100,
  message: "视频预览可用",
  media: {
    path: "/tmp/controlled-preview.mp4",
    mimeType: "video/mp4",
    cacheHit: false,
    generated: true,
    sourceMissing: false,
  },
  errorCode: null,
  errorMessage: null,
};

function createAiApi(
  projectDetail: AiProjectDetail = detail,
  projection: AiTranscriptProjection = transcript,
): ClientApi {
  return {
    listAiProjects: vi.fn().mockResolvedValue([projectDetail.project]),
    getAiProject: vi.fn().mockResolvedValue(projectDetail),
    queryAiTranscript: vi.fn().mockResolvedValue(projection),
    diagnoseAiEnvironment: vi.fn().mockResolvedValue(environment),
    listAiCompletedSessions: vi.fn().mockResolvedValue([]),
    listAiReplayStreamers: vi.fn().mockResolvedValue({ items: [], nextCursor: null }),
    listAiReplaySessions: vi.fn().mockResolvedValue({ items: [], nextCursor: null }),
    subscribeAi: vi.fn().mockResolvedValue(() => undefined),
    subscribePreview: vi.fn().mockResolvedValue(() => undefined),
    requestAiInputPreview: vi.fn().mockResolvedValue(readyPreview),
    retryAiInputPreview: vi.fn().mockResolvedValue(readyPreview),
    retainVideoPreview: vi.fn().mockResolvedValue(undefined),
    releaseVideoPreview: vi.fn().mockResolvedValue(undefined),
    copyAiSegmentText: vi.fn().mockResolvedValue("欢迎来到直播间。"),
    copyAiInputText: vi.fn().mockResolvedValue("欢迎来到直播间。\n今天价格99元。"),
    copyAiProjectText: vi.fn().mockResolvedValue("【视频】\n欢迎来到直播间。"),
    exportAiTxt: vi.fn().mockResolvedValue({ saved: true }),
    exportAiJson: vi.fn().mockResolvedValue({ saved: true }),
    cancelAiProject: vi.fn().mockResolvedValue({ ...projectDetail.project, status: "cancelled" }),
    retryAiInput: vi.fn().mockResolvedValue(projectDetail),
    deleteAiProject: vi.fn().mockResolvedValue(undefined),
  } as unknown as ClientApi;
}

function createKeyboardClipFixture() {
  const selectedCandidates: AiHighlightCandidate[] = [{
    ...highlightCandidates[0],
    id: 941,
    candidateKey: "keyboard-first",
    title: "快捷键第一段",
    startMs: 1_000,
    endMs: 3_000,
    selected: true,
  }, {
    ...highlightCandidates[1],
    id: 942,
    candidateKey: "keyboard-second",
    title: "快捷键第二段",
    startMs: 5_000,
    endMs: 9_000,
    selected: true,
  }];
  const candidatePage: AiHighlightCandidatePage = {
    items: selectedCandidates,
    page: 0,
    pageSize: 50,
    totalCandidates: 2,
    qualifiedCandidates: 2,
    selectedCandidates: 2,
  };
  const clip: AiClipProjectDetail = {
    project: {
      id: 741,
      highlightRunId: completedHighlightRun.id,
      name: "快捷键测试工程",
      outputWidth: 1920,
      outputHeight: 1080,
      version: 1,
      exportStatus: "idle",
      exportProgress: 0,
      outputPath: null,
      lastErrorCode: null,
      lastErrorMessage: null,
      createdAt: "2026-08-03T00:00:00Z",
      updatedAt: "2026-08-03T00:00:00Z",
    },
    segments: selectedCandidates.map((candidate, index) => ({
      id: 751 + index,
      clipProjectId: 741,
      candidateId: candidate.id,
      inputId: candidate.inputId,
      position: index,
      title: candidate.title,
      sourceStartMs: candidate.startMs,
      sourceEndMs: candidate.endMs,
      volumePercent: 100,
      effect: "none" as const,
    })),
    subtitles: [{
      id: 761,
      clipProjectId: 741,
      stableSegmentId: "keyboard-subtitle",
      clipSegmentId: 751,
      inputId: selectedCandidates[0].inputId,
      originalText: "快捷键输入保护。",
      text: "快捷键输入保护。",
      hidden: false,
      sourceStartMs: 1_000,
      sourceEndMs: 2_000,
      projectStartMs: 0,
      projectEndMs: 1_000,
    }],
    subtitleFrames: [{
      subtitleId: 761,
      clipSegmentId: 751,
      projectStartMs: 0,
      projectEndMs: 1_000,
      pageText: "快捷键输入保护。",
      visibleText: "快捷键输入保护。",
      hiddenText: "",
    }],
    subtitlesComplete: false,
  };
  const api = createAiApi();
  api.getLatestAiHighlightRun = vi.fn().mockResolvedValue(completedHighlightRun);
  api.listQualifiedAiHighlightCandidates = vi.fn().mockResolvedValue(candidatePage);
  api.listSelectedAiHighlightCandidates = vi.fn().mockResolvedValue(candidatePage);
  api.openAiClipProject = vi.fn().mockResolvedValue(clip);
  api.getAiClipProject = vi.fn().mockResolvedValue(clip);
  return { api, clip };
}

async function openKeyboardClipEditor(api: ClientApi) {
  const user = userEvent.setup();
  vi.spyOn(HTMLMediaElement.prototype, "readyState", "get")
    .mockReturnValue(HTMLMediaElement.HAVE_METADATA);
  const view = render(<AiWorkspace api={api} />);
  await user.click(await screen.findByRole("button", { name: "编辑视频" }));
  const editor = await screen.findByRole("main", { name: "视频剪辑页面" });
  await waitFor(() => expect(editor.querySelector("video")).not.toBeNull());
  const video = editor.querySelector<HTMLVideoElement>("video")!;
  fireEvent.loadedMetadata(video);
  return { ...view, user, editor, video };
}

beforeEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  Object.defineProperty(navigator, "clipboard", {
    configurable: true,
    value: { writeText: vi.fn().mockResolvedValue(undefined) },
  });
});

describe("AiWorkspace", () => {
  it("由用户创建项目、选择本地视频并明确点击开始分析", async () => {
    const user = userEvent.setup();
    const draftProject: AiProject = {
      ...project,
      id: 202,
      name: "待识别视频",
      status: "draft",
      inputFrozen: false,
      progressPercent: 0,
      createdAt: "2026-07-23T00:00:00Z",
      updatedAt: "2026-07-23T00:00:00Z",
    };
    const pendingInput: AiProjectDetail["inputs"][number] = {
      ...completedInput,
      id: 302,
      projectId: draftProject.id,
      displayName: "short_zh.mp4",
      durationMs: 4_000,
      status: "pending",
      progressPercent: 0,
      artifactId: null,
    };
    let projects: AiProject[] = [];
    let currentDetail: AiProjectDetail = { project: draftProject, inputs: [] };
    const api = {
      listAiProjects: vi.fn(async () => projects),
      getAiProject: vi.fn(async () => currentDetail),
      queryAiTranscript: vi.fn(async (): Promise<AiTranscriptProjection> => ({
        project: currentDetail.project,
        inputs: [],
      })),
      diagnoseAiEnvironment: vi.fn().mockResolvedValue(environment),
      listAiCompletedSessions: vi.fn().mockResolvedValue([]),
      subscribeAi: vi.fn().mockResolvedValue(() => undefined),
      subscribePreview: vi.fn().mockResolvedValue(() => undefined),
      createAiProject: vi.fn(async () => {
        projects = [draftProject];
        return draftProject;
      }),
      pickAiLocalVideos: vi.fn().mockResolvedValue([{
        grantId: "selected-video-grant",
        displayName: pendingInput.displayName,
      }]),
      importAiLocalGrants: vi.fn(async () => {
        currentDetail = { project: draftProject, inputs: [pendingInput] };
        return { added: [pendingInput], rejected: [] };
      }),
      startAiProject: vi.fn(async () => {
        const queued = {
          ...draftProject,
          status: "queued" as const,
          inputFrozen: true,
        };
        projects = [queued];
        currentDetail = { project: queued, inputs: [pendingInput] };
        return queued;
      }),
    } as unknown as ClientApi;
    render(<AiWorkspace api={api} />);

    await user.click(await screen.findByRole("button", { name: "创建项目" }));
    await user.type(screen.getByLabelText("项目名称"), draftProject.name);
    await user.click(screen.getByRole("button", { name: "创建草稿" }));
    await user.click(await screen.findByRole("button", { name: "添加本地视频" }));

    expect(api.pickAiLocalVideos).toHaveBeenCalledTimes(1);
    await waitFor(() => expect(api.importAiLocalGrants).toHaveBeenCalledWith(
      draftProject.id,
      ["selected-video-grant"],
    ));
    expect(await screen.findByText(pendingInput.displayName)).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "开始分析" }));
    await waitFor(() => expect(api.startAiProject).toHaveBeenCalledWith(draftProject.id));
  });

  it("主播直播中仍可一键加入历史整场并在导入后手动删除分片", async () => {
    const user = userEvent.setup();
    const draftProject: AiProject = {
      ...project,
      id: 203,
      name: "历史整场",
      status: "draft",
      inputFrozen: false,
      progressPercent: 0,
    };
    const replayStreamer = {
      streamerId: 73,
      name: "正在直播主播的历史回放",
      tags: ["带货"],
      webRid: "7300",
      archived: false,
      monitorEnabled: true,
      liveStatus: "live",
      monitorStatus: "recording",
      replayCount: 1,
      latestEndedAt: "2026-07-21T22:00:00Z",
    };
    const session = {
      sessionId: 88,
      startedAt: "2026-07-21T20:00:00Z",
      endedAt: "2026-07-21T22:00:00Z",
      status: "completed",
      videoCount: 3,
      totalDurationMs: 7_200_000,
      unavailableVideoCount: 1,
      importedVideoCount: 0,
      fullyImported: false,
    };
    const fullyImportedSession = {
      ...session,
      sessionId: 89,
      startedAt: "2026-07-20T20:00:00Z",
      endedAt: "2026-07-20T22:00:00Z",
      importedVideoCount: 3,
      fullyImported: true,
    };
    const sessionInputs: AiProjectDetail["inputs"] = [
      {
        ...completedInput,
        id: 401,
        projectId: draftProject.id,
        position: 0,
        sourceKind: "video_library",
        videoId: 501,
        displayName: "直播-001.mkv",
        status: "pending",
        progressPercent: 0,
        artifactId: null,
      },
      {
        ...completedInput,
        id: 402,
        projectId: draftProject.id,
        position: 1,
        sourceKind: "video_library",
        videoId: 502,
        displayName: "直播-002.mkv",
        status: "pending",
        progressPercent: 0,
        artifactId: null,
      },
      {
        ...completedInput,
        id: 403,
        projectId: draftProject.id,
        position: 2,
        sourceKind: "video_library",
        videoId: 503,
        displayName: "直播-003-缺失.mkv",
        status: "failed",
        progressPercent: 100,
        artifactId: null,
        lastErrorCode: "session_video_unavailable",
        lastErrorMessage: "录像分片缺失或尚未完成",
      },
    ];
    let currentDetail: AiProjectDetail = { project: draftProject, inputs: [] };
    const pickAiLocalVideos = vi.fn().mockResolvedValue([]);
    const addAiCompletedSession = vi.fn(async () => {
      currentDetail = { project: draftProject, inputs: sessionInputs };
      return {
        detail: currentDetail,
        addedCount: 3,
        duplicateCount: 1,
        unavailableCount: 1,
      };
    });
    const removeAiInput = vi.fn(async (_projectId: number, inputId: number) => {
      currentDetail = {
        project: draftProject,
        inputs: currentDetail.inputs.filter((input) => input.id !== inputId),
      };
      return currentDetail;
    });
    const api = {
      listAiProjects: vi.fn().mockResolvedValue([draftProject]),
      getAiProject: vi.fn(async () => currentDetail),
      queryAiTranscript: vi.fn(async (): Promise<AiTranscriptProjection> => ({
        project: draftProject,
        inputs: [],
      })),
      diagnoseAiEnvironment: vi.fn().mockResolvedValue(environment),
      listAiCompletedSessions: vi.fn().mockResolvedValue([session]),
      listAiReplayStreamers: vi.fn().mockResolvedValue({ items: [replayStreamer], nextCursor: null }),
      listAiReplaySessions: vi.fn().mockResolvedValue({ items: [session, fullyImportedSession], nextCursor: null }),
      subscribeAi: vi.fn().mockResolvedValue(() => undefined),
      subscribePreview: vi.fn().mockResolvedValue(() => undefined),
      pickAiLocalVideos,
      addAiCompletedSession,
      removeAiInput,
    } as unknown as ClientApi;
    render(<AiWorkspace api={api} />);

    await user.click(await screen.findByRole("combobox", { name: "选择历史主播" }));
    await user.click(await screen.findByRole("option", { name: /正在直播主播的历史回放/ }));
    await user.click(screen.getByRole("combobox", { name: "选择历史回放" }));
    expect(await screen.findByRole("option", { name: /已全部添加/ })).toBeDisabled();
    const replayOption = await screen.findByRole("option", { name: /会话 88.*3 个分片.*1 个不可用/ });
    await user.click(replayOption);

    await user.click(screen.getByRole("button", { name: "添加整场直播" }));

    expect(addAiCompletedSession).toHaveBeenCalledWith(draftProject.id, session.sessionId);
    expect(pickAiLocalVideos).not.toHaveBeenCalled();
    expect(await screen.findByText("新增 3 个分片，跳过 1 个重复，1 个不可用")).toBeInTheDocument();
    expect(screen.getByText("直播-001.mkv")).toBeInTheDocument();
    expect(screen.getByText("直播-002.mkv")).toBeInTheDocument();
    expect(screen.getByText("直播-003-缺失.mkv")).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "移除 直播-002.mkv" }));
    expect(removeAiInput).toHaveBeenCalledWith(draftProject.id, 402);
    await waitFor(() => expect(screen.queryByText("直播-002.mkv")).not.toBeInTheDocument());
  });

  it("AI 页面隐藏期间今天直播结束后，重新进入无需重启即可出现在回放目录", async () => {
    const user = userEvent.setup();
    const draftProject: AiProject = {
      ...project,
      id: 204,
      name: "今日回放",
      status: "draft",
      inputFrozen: false,
      progressPercent: 0,
    };
    const draftDetail: AiProjectDetail = { project: draftProject, inputs: [] };
    const todayStreamer = {
      streamerId: 91,
      name: "今天刚结束的主播",
      tags: ["知识"],
      webRid: "9100",
      archived: false,
      monitorEnabled: true,
      liveStatus: "offline",
      monitorStatus: "waiting",
      replayCount: 1,
      latestEndedAt: "2026-08-01T12:30:00Z",
    };
    const todayReplay = {
      sessionId: 991,
      startedAt: "2026-08-01T11:30:00Z",
      endedAt: "2026-08-01T12:30:00Z",
      status: "completed",
      videoCount: 4,
      totalDurationMs: 3_600_000,
      unavailableVideoCount: 0,
      importedVideoCount: 0,
      fullyImported: false,
    };
    let streamerItems: typeof todayStreamer[] = [];
    let replayItems: typeof todayReplay[] = [];
    const api = createAiApi(draftDetail, { project: draftProject, inputs: [] });
    api.listAiReplayStreamers = vi.fn(async () => ({ items: streamerItems, nextCursor: null }));
    api.listAiReplaySessions = vi.fn(async () => ({ items: replayItems, nextCursor: null }));
    const { rerender } = render(
      <AiWorkspace api={api} active={false} replayDirectoryVersion={0} />,
    );
    expect((await screen.findAllByText("今日回放")).length).toBeGreaterThan(0);
    expect(api.listAiReplayStreamers).not.toHaveBeenCalled();

    streamerItems = [todayStreamer];
    replayItems = [todayReplay];
    rerender(<AiWorkspace api={api} active={false} replayDirectoryVersion={1} />);
    expect(api.listAiReplayStreamers).not.toHaveBeenCalled();
    rerender(<AiWorkspace api={api} active replayDirectoryVersion={1} />);
    await waitFor(() => expect(api.listAiReplayStreamers).toHaveBeenCalled());

    await user.click(screen.getByRole("combobox", { name: "选择历史主播" }));
    await user.click(await screen.findByRole("option", { name: /今天刚结束的主播/ }));
    await user.click(screen.getByRole("combobox", { name: "选择历史回放" }));
    expect(await screen.findByRole("option", { name: /会话 991/ })).toBeInTheDocument();
  });

  it("快速搜索主播时忽略较晚返回的旧查询结果", async () => {
    const user = userEvent.setup();
    let resolveOld: ((value: { items: Array<{
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
    }>; nextCursor: null }) => void) | null = null;
    const oldStreamer = {
      streamerId: 31,
      name: "旧搜索结果",
      tags: [],
      webRid: "31",
      archived: false,
      monitorEnabled: true,
      liveStatus: "offline",
      monitorStatus: "waiting",
      replayCount: 1,
      latestEndedAt: "2026-07-31T10:00:00Z",
    };
    const newStreamer = { ...oldStreamer, streamerId: 32, name: "新搜索结果", webRid: "32" };
    const api = createAiApi({ ...detail, project: { ...project, status: "draft", inputFrozen: false } });
    api.listAiReplayStreamers = vi.fn((search = "") => {
      if (search === "旧") {
        return new Promise<AiReplayStreamerPage>((resolve) => { resolveOld = resolve; });
      }
      return Promise.resolve({ items: search === "新" ? [newStreamer] : [], nextCursor: null });
    });
    render(<AiWorkspace api={api} />);
    expect((await screen.findAllByText(project.name)).length).toBeGreaterThan(0);
    await user.click(screen.getByRole("combobox", { name: "选择历史主播" }));
    const search = screen.getByLabelText("选择历史主播搜索");
    await user.type(search, "旧");
    await waitFor(() => expect(api.listAiReplayStreamers).toHaveBeenCalledWith("旧", null, 20));
    await user.clear(search);
    await user.type(search, "新");
    expect(await screen.findByRole("option", { name: /新搜索结果/ })).toBeInTheDocument();
    await act(async () => resolveOld?.({ items: [oldStreamer], nextCursor: null }));
    expect(screen.queryByRole("option", { name: /旧搜索结果/ })).not.toBeInTheDocument();
  });

  it("展示播放器与只读时间戳文本，点击句段跳转并用 WebView 元素显示临时字幕", async () => {
    const user = userEvent.setup();
    const api = createAiApi();
    render(<AiWorkspace api={api} />);

    expect(await screen.findByText("欢迎来到直播间。")).toBeInTheDocument();
    expect(screen.getAllByText(/Windows 中文目录/).length).toBeGreaterThan(0);
    await waitFor(() => expect(api.requestAiInputPreview).toHaveBeenCalledWith(
      project.id,
      completedInput.id,
    ));
    const video = await screen.findByLabelText("AI 视频播放器") as HTMLVideoElement;
    Object.defineProperty(video, "videoWidth", { configurable: true, value: 720 });
    Object.defineProperty(video, "videoHeight", { configurable: true, value: 1270 });
    fireEvent.loadedMetadata(video);
    expect(video.closest(".ai-player-stage")).toHaveClass("portrait");
    await user.click(screen.getByText("欢迎来到直播间。").closest("button")!);
    expect(video.currentTime).toBe(1);

    expect(screen.getByLabelText("显示字幕")).toBeChecked();
    Object.defineProperty(video, "currentTime", { configurable: true, value: 1.2, writable: true });
    fireEvent.timeUpdate(video);
    expect(await screen.findByText("欢迎来到直播间。", { selector: ".ai-subtitle-overlay" })).toBeInTheDocument();
    expect(screen.getByText("欢迎来到直播间。", { selector: ".ai-segment-main p" }).closest(".ai-segment-row")).toHaveClass("active");
    expect(api.exportAiTxt).not.toHaveBeenCalled();
  });

  it("跟随播放只滚动转写列表而不推动整个页面", async () => {
    const scrollIntoView = vi.fn();
    Object.defineProperty(window.HTMLElement.prototype, "scrollIntoView", {
      configurable: true,
      value: scrollIntoView,
    });
    const api = createAiApi();
    render(<AiWorkspace api={api} />);

    const video = await screen.findByLabelText("AI 视频播放器") as HTMLVideoElement;
    const segmentList = screen.getByLabelText("转写句段列表");
    const targetRow = screen.getByText("今天价格99元。", { selector: ".ai-segment-main p" })
      .closest(".ai-segment-row") as HTMLElement;
    segmentList.scrollTop = 10;
    vi.spyOn(segmentList, "getBoundingClientRect").mockReturnValue({
      x: 0,
      y: 100,
      top: 100,
      right: 500,
      bottom: 200,
      left: 0,
      width: 500,
      height: 100,
      toJSON: () => ({}),
    });
    vi.spyOn(targetRow, "getBoundingClientRect").mockReturnValue({
      x: 0,
      y: 220,
      top: 220,
      right: 500,
      bottom: 260,
      left: 0,
      width: 500,
      height: 40,
      toJSON: () => ({}),
    });

    Object.defineProperty(video, "currentTime", { configurable: true, value: 3.2, writable: true });
    fireEvent.timeUpdate(video);

    await waitFor(() => expect(targetRow).toHaveClass("active"));
    expect(segmentList.scrollTop).toBe(70);
    expect(scrollIntoView).not.toHaveBeenCalled();
  });

  it("高光分析等待 LLM 返回时显示专用进度并在完成后收起", async () => {
    const user = userEvent.setup();
    vi.spyOn(window, "confirm").mockReturnValue(true);
    let completeAnalysis!: (run: AiHighlightRun) => void;
    const pendingAnalysis = new Promise<AiHighlightRun>((resolve) => {
      completeAnalysis = resolve;
    });
    const api = createAiApi();
    api.getAiLlmSettings = vi.fn().mockResolvedValue({
      provider: "deepseek",
      modelId: "deepseek-chat",
      timeoutMs: 30_000,
      promptVersion: "highlight-v1",
      qualifiedScore: 70,
      excellentScore: 80,
      keyConfigured: true,
      updatedAt: "2026-07-22T00:00:00Z",
    });
    api.startAiHighlightAnalysis = vi.fn(() => pendingAnalysis);
    api.listAiHighlightCandidates = vi.fn().mockResolvedValue([]);
    render(<AiWorkspace api={api} />);

    const startButton = await screen.findByRole("button", { name: "开始高光分析" });
    await waitFor(() => expect(startButton).toBeEnabled());
    await user.click(startButton);

    expect(await screen.findByRole("progressbar", { name: "高光分析进行中" })).toBeInTheDocument();
    expect(screen.getByText("1 个视频 · 2 个句段 · 已发现 0 个候选草稿")).toBeInTheDocument();
    expect(screen.getByText("正在分批生成候选")).toBeInTheDocument();
    expect(screen.queryByText(/完成分析后/)).not.toBeInTheDocument();

    completeAnalysis(completedHighlightRun);

    await waitFor(() => expect(screen.queryByRole("progressbar", { name: "高光分析进行中" })).not.toBeInTheDocument());
    expect(screen.getByText("状态：已完成")).toBeInTheDocument();
    expect(screen.getByText("范围：1 批 · 2 句")).toBeInTheDocument();
    expect(screen.getByText("模型消耗：320 Token")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "刷新分析结果" })).toBeEnabled();
    expect(screen.getByText("分析已完成，但没有生成候选。")).toBeInTheDocument();
  });

  it("恢复长批次后台分析并展示真实完成数和失败批次", async () => {
    const runningRun: AiHighlightRun = {
      ...completedHighlightRun,
      status: "running",
      estimatedBatches: 57,
      totalSegments: 21_663,
      totalTokens: 12_480,
      lastErrorCode: "highlight_provider_timeout",
      lastErrorMessage: "Provider 暂时不可用",
    };
    const api = createAiApi();
    api.getLatestAiHighlightRun = vi.fn().mockResolvedValue(runningRun);
    api.resumeAiHighlightAnalysis = vi.fn().mockResolvedValue(runningRun);
    api.getAiHighlightProgress = vi.fn().mockResolvedValue({
      runId: runningRun.id,
      totalBatches: 57,
      pendingBatches: 36,
      runningBatches: 1,
      completedBatches: 18,
      failedBatches: 2,
      candidateCount: 41,
    });
    api.listAiHighlightCandidates = vi.fn().mockResolvedValue([]);
    render(<AiWorkspace api={api} />);

    const progressbar = await screen.findByRole("progressbar", { name: "高光分析进行中" });
    expect(progressbar).toHaveAttribute("aria-valuenow", "32");
    expect(screen.getByText("已处理 20 / 57 批 · 18 成功 · 2 失败")).toBeInTheDocument();
    expect(screen.getByText("1 个视频 · 2 个句段 · 已发现 41 个候选草稿")).toBeInTheDocument();
    expect(screen.getByText("模型消耗：12480 Token")).toBeInTheDocument();
    expect(screen.getByText("Provider 暂时不可用")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "分析中…" })).toBeDisabled();
    expect(api.resumeAiHighlightAnalysis).toHaveBeenCalledWith(runningRun.id);
  });

  it("相同内容命中历史高光结果时不闪烁运行进度", async () => {
    const user = userEvent.setup();
    vi.spyOn(window, "confirm").mockReturnValue(true);
    const api = createAiApi();
    api.getAiLlmSettings = vi.fn().mockResolvedValue({
      provider: "deepseek",
      modelId: "deepseek-chat",
      timeoutMs: 30_000,
      promptVersion: "highlight-v1",
      qualifiedScore: 70,
      excellentScore: 80,
      keyConfigured: true,
      updatedAt: "2026-07-22T00:00:00Z",
    });
    api.startAiHighlightAnalysis = vi.fn().mockResolvedValue(completedHighlightRun);
    api.listAiHighlightCandidates = vi.fn().mockResolvedValue([]);
    render(<AiWorkspace api={api} />);

    const startButton = await screen.findByRole("button", { name: "开始高光分析" });
    await waitFor(() => expect(startButton).toBeEnabled());
    await user.click(startButton);

    expect(await screen.findByRole("button", { name: "刷新分析结果" })).toBeEnabled();
    await new Promise((resolve) => window.setTimeout(resolve, 350));
    expect(screen.queryByRole("progressbar", { name: "高光分析进行中" })).not.toBeInTheDocument();
    expect(screen.getByText("已读取相同内容的历史高光分析结果")).toBeInTheDocument();
  });

  it("重试接口返回旧的部分完成快照时继续轮询到新候选", async () => {
    const user = userEvent.setup();
    vi.spyOn(window, "confirm").mockReturnValue(true);
    const partialRun: AiHighlightRun = {
      ...completedHighlightRun,
      status: "partial",
      qualifiedScore: 50,
      totalTokens: 4_400,
      lastErrorCode: "highlight_ranking_failed",
      lastErrorMessage: "统一评分失败，已按候选分项分数发布备用排序",
    };
    const finishedRun: AiHighlightRun = {
      ...partialRun,
      status: "completed",
      totalTokens: 4_900,
      lastErrorCode: null,
      lastErrorMessage: null,
      updatedAt: "2026-07-22T00:00:10Z",
    };
    const qualified = [{ ...highlightCandidates[0], totalScore: 85 }];
    const emptyPage: AiHighlightCandidatePage = {
      items: [],
      page: 0,
      pageSize: 50,
      totalCandidates: 8,
      qualifiedCandidates: 0,
      selectedCandidates: 0,
    };
    const finishedPage: AiHighlightCandidatePage = {
      ...emptyPage,
      items: qualified,
      qualifiedCandidates: 5,
    };
    const api = createAiApi();
    api.getAiLlmSettings = vi.fn().mockResolvedValue({
      provider: "deepseek",
      modelId: "deepseek-chat",
      timeoutMs: 30_000,
      promptVersion: "highlight-v1",
      qualifiedScore: 50,
      excellentScore: 80,
      keyConfigured: true,
      updatedAt: "2026-07-22T00:00:00Z",
    });
    api.getLatestAiHighlightRun = vi.fn()
      .mockResolvedValueOnce(partialRun)
      .mockResolvedValue(finishedRun);
    api.startAiHighlightAnalysis = vi.fn().mockResolvedValue(partialRun);
    api.getAiHighlightProgress = vi.fn().mockResolvedValue({
      runId: partialRun.id,
      totalBatches: 3,
      pendingBatches: 0,
      runningBatches: 0,
      completedBatches: 3,
      failedBatches: 0,
      candidateCount: 8,
    });
    api.listQualifiedAiHighlightCandidates = vi.fn()
      .mockResolvedValueOnce(emptyPage)
      .mockResolvedValueOnce(emptyPage)
      .mockResolvedValue(finishedPage);
    api.listSelectedAiHighlightCandidates = vi.fn().mockResolvedValue(emptyPage);
    render(<AiWorkspace api={api} />);

    await user.click(await screen.findByRole("button", { name: "重试未完成批次" }, { timeout: 3_000 }));
    expect(await screen.findByRole("button", { name: "高光候选 1" }, { timeout: 3_000 })).toBeInTheDocument();
    expect(api.getLatestAiHighlightRun).toHaveBeenCalledTimes(2);
    expect(screen.getByText("状态：已完成")).toBeInTheDocument();
    expect(screen.getByText("模型消耗：4900 Token")).toBeInTheDocument();
  });

  it("将高光候选与 ASR 整合，并且一次只展开当前候选详情", async () => {
    const user = userEvent.setup();
    const api = createAiApi();
    const candidatesWithAutomaticSelection = highlightCandidates.map((candidate) => ({
      ...candidate,
      selected: candidate.totalScore >= completedHighlightRun.excellentScore,
    }));
    api.getLatestAiHighlightRun = vi.fn().mockResolvedValue(completedHighlightRun);
    api.listAiHighlightCandidates = vi.fn().mockResolvedValue(candidatesWithAutomaticSelection);
    render(<AiWorkspace api={api} />);

    expect(await screen.findByText("已保存 2 个候选")).toBeInTheDocument();
    expect(screen.getByText("1 个达到合格阈值 · 已选择 1 个 · 优秀候选已自动选中")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "全文" })).toHaveAttribute("aria-pressed", "true");
    expect(screen.getByRole("button", { name: "高光候选 1" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "已选择 1" })).toBeInTheDocument();
    expect(screen.queryByText(highlightCandidates[0].reason)).not.toBeInTheDocument();
    expect(screen.queryByText(highlightCandidates[1].reason)).not.toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "高光候选 1" }));

    const navigation = screen.getByLabelText("高光候选导航");
    expect(within(navigation).getByRole("button", { name: "查看候选 价格反转，82 分" })).toBeInTheDocument();
    expect(within(navigation).getAllByRole("button")[0]).toHaveAccessibleName("查看候选 价格反转，82 分");
    expect(screen.getByLabelText("当前高光候选")).toHaveTextContent("价格反转");
    expect(screen.getByLabelText("价格反转 评分明细")).toHaveTextContent("86吸引力");
    expect(screen.getByLabelText("价格反转 评分明细")).toHaveTextContent("91标签相关");
    expect(screen.getByText(highlightCandidates[0].reason)).toBeInTheDocument();
    expect(screen.queryByLabelText("普通互动 评分明细")).not.toBeInTheDocument();
    expect(screen.getAllByLabelText("句段包含 1 个高光候选")).toHaveLength(2);
    expect(screen.queryByRole("button", { name: "保存候选选择" })).not.toBeInTheDocument();

    await user.selectOptions(screen.getByLabelText("候选排序"), "time");
    expect(within(navigation).getAllByRole("button")[0]).toHaveAccessibleName("查看候选 价格反转，82 分");
  });

  it("顶部候选和编辑入口会定位到 ASR 选择区", async () => {
    const user = userEvent.setup();
    const scrollIntoView = vi.fn();
    Object.defineProperty(HTMLElement.prototype, "scrollIntoView", {
      configurable: true,
      value: scrollIntoView,
    });
    const api = createAiApi();
    api.getLatestAiHighlightRun = vi.fn().mockResolvedValue(completedHighlightRun);
    api.listAiHighlightCandidates = vi.fn().mockResolvedValue(highlightCandidates);
    api.openAiClipProject = vi.fn();
    render(<AiWorkspace api={api} />);

    await user.click(await screen.findByText("在 ASR 中查看和选择"));
    expect(screen.getByRole("button", { name: "高光候选 1" })).toHaveAttribute("aria-pressed", "true");
    await waitFor(() => expect(scrollIntoView).toHaveBeenCalledWith({ behavior: "smooth", block: "start" }));

    await user.click(screen.getByRole("button", { name: "编辑视频" }));
    expect(screen.getByText("请先在高光候选中勾选至少一个片段，再进入视频编辑")).toBeInTheDocument();
    expect(api.openAiClipProject).not.toHaveBeenCalled();
  });

  it("重新打开项目时恢复最近一次高光运行和评分候选", async () => {
    const api = createAiApi();
    api.getLatestAiHighlightRun = vi.fn().mockResolvedValue(completedHighlightRun);
    api.listAiHighlightCandidates = vi.fn().mockResolvedValue(highlightCandidates);
    render(<AiWorkspace api={api} />);

    expect(await screen.findByText("已保存 2 个候选")).toBeInTheDocument();
    await userEvent.setup().click(screen.getByRole("button", { name: "高光候选 1" }));
    expect(screen.getByLabelText("当前高光候选")).toHaveTextContent("82 分");
    expect(screen.getByLabelText("价格反转 评分明细")).toHaveTextContent("91标签相关");
    expect(api.getLatestAiHighlightRun).toHaveBeenCalledWith(project.id);
    expect(api.listAiHighlightCandidates).toHaveBeenCalledWith(completedHighlightRun.id);
  });

  it("点击跨视频候选后定位 ASR 和播放器，并只播放候选时间范围", async () => {
    const user = userEvent.setup();
    const secondInput: AiProjectDetail["inputs"][number] = {
      ...completedInput,
      id: 302,
      position: 1,
      displayName: "第二段直播.mp4",
      durationMs: 20_000,
    };
    const secondSegment: AiTranscriptSegment = {
      ...segments[0],
      stableSegmentId: "seg-second-highlight",
      inputId: secondInput.id,
      sourceStartMs: 6_000,
      sourceEndMs: 9_000,
      projectStartMs: 16_000,
      projectEndMs: 19_000,
      rawText: "第二段高光原文",
      normalizedText: "第二段高光原文。",
    };
    const secondCandidate: AiHighlightCandidate = {
      ...highlightCandidates[0],
      id: 903,
      candidateKey: "candidate-second-video",
      title: "跨视频高光",
      inputId: secondInput.id,
      segmentIds: [secondSegment.stableSegmentId],
      startMs: 6_000,
      endMs: 9_000,
      totalScore: 77,
      rank: 3,
    };
    const multiDetail: AiProjectDetail = { project, inputs: [completedInput, secondInput] };
    const multiTranscript: AiTranscriptProjection = {
      project,
      inputs: [transcript.inputs[0], {
        ...transcript.inputs[0],
        inputId: secondInput.id,
        position: 1,
        displayName: secondInput.displayName,
        durationMs: secondInput.durationMs,
        projectOffsetMs: 10_000,
        segments: [secondSegment],
      }],
    };
    const api = createAiApi(multiDetail, multiTranscript);
    api.getLatestAiHighlightRun = vi.fn().mockResolvedValue(completedHighlightRun);
    api.listAiHighlightCandidates = vi.fn().mockResolvedValue([...highlightCandidates, secondCandidate]);
    api.requestAiInputPreview = vi.fn(async (_projectId, inputId) => ({
      ...readyPreview,
      requestId: `preview-${inputId}`,
      videoId: -inputId,
    }));
    const play = vi.spyOn(window.HTMLMediaElement.prototype, "play").mockResolvedValue(undefined);
    const pause = vi.spyOn(window.HTMLMediaElement.prototype, "pause").mockImplementation(() => undefined);
    render(<AiWorkspace api={api} />);

    await user.click(await screen.findByRole("button", { name: "高光候选 2" }));
    await user.click(screen.getByRole("button", { name: "查看候选 跨视频高光，77 分" }));

    await waitFor(() => expect(api.requestAiInputPreview).toHaveBeenCalledWith(project.id, secondInput.id));
    expect(await screen.findByText(secondSegment.normalizedText, { selector: ".ai-segment-main p" })).toBeInTheDocument();
    const video = await screen.findByLabelText("AI 视频播放器") as HTMLVideoElement;
    await waitFor(() => expect(video.currentTime).toBe(6));

    await user.click(screen.getByRole("button", { name: "播放高光片段" }));
    expect(play).toHaveBeenCalled();
    expect(video.currentTime).toBe(6);
    video.currentTime = 9;
    fireEvent.timeUpdate(video);
    expect(pause).toHaveBeenCalled();
  });

  it("分页候选按单项切换选择，不覆盖其它已选片段", async () => {
    const user = userEvent.setup();
    const secondQualified = {
      ...highlightCandidates[1],
      totalScore: 74,
      shareabilityScore: 75,
      selected: true,
    };
    const qualified = [highlightCandidates[0], secondQualified];
    const page = (items: AiHighlightCandidate[], selectedCandidates: number): AiHighlightCandidatePage => ({
      items,
      page: 0,
      pageSize: 50,
      totalCandidates: 3,
      qualifiedCandidates: 2,
      selectedCandidates,
    });
    const api = createAiApi();
    api.getLatestAiHighlightRun = vi.fn().mockResolvedValue(completedHighlightRun);
    api.listQualifiedAiHighlightCandidates = vi.fn().mockResolvedValue(page(qualified, 1));
    api.listSelectedAiHighlightCandidates = vi.fn().mockResolvedValue(page([secondQualified], 1));
    api.setAiHighlightCandidateSelected = vi.fn().mockResolvedValue({ ...highlightCandidates[0], selected: true });
    render(<AiWorkspace api={api} />);

    await user.click(await screen.findByRole("button", { name: "高光候选 2" }));
    const selection = screen.getByRole("checkbox", { name: "加入待切片" });
    await user.click(selection);

    await waitFor(() => expect(api.setAiHighlightCandidateSelected).toHaveBeenCalledWith(
      completedHighlightRun.id,
      highlightCandidates[0].id,
      true,
    ));
    expect(selection).toBeChecked();
    expect(screen.getByRole("button", { name: "已选择 2" })).toBeInTheDocument();
    expect(api.selectAiHighlightCandidates).toBeUndefined();
  });

  it("候选全部低于阈值时展示真实候选数和冻结阈值", async () => {
    const partialRun: AiHighlightRun = {
      ...completedHighlightRun,
      status: "partial",
      qualifiedScore: 50,
      lastErrorCode: "highlight_ranking_failed",
      lastErrorMessage: "统一评分失败，已按候选分项分数发布备用排序",
    };
    const api = createAiApi();
    api.getLatestAiHighlightRun = vi.fn().mockResolvedValue(partialRun);
    api.listQualifiedAiHighlightCandidates = vi.fn().mockResolvedValue({
      items: [],
      page: 0,
      pageSize: 50,
      totalCandidates: 8,
      qualifiedCandidates: 0,
      selectedCandidates: 0,
    });
    api.listSelectedAiHighlightCandidates = vi.fn().mockResolvedValue({
      items: [],
      page: 0,
      pageSize: 50,
      totalCandidates: 8,
      qualifiedCandidates: 0,
      selectedCandidates: 0,
    });
    render(<AiWorkspace api={api} />);

    expect(await screen.findByText("本次已生成 8 个候选，但没有候选达到 50 分的合格阈值。")).toBeInTheDocument();
  });

  it("在剪辑页用空格切换播放并忽略自动重复", async () => {
    const { api } = createKeyboardClipFixture();
    const { video } = await openKeyboardClipEditor(api);
    let paused = true;
    Object.defineProperty(video, "paused", { configurable: true, get: () => paused });
    const play = vi.spyOn(video, "play").mockImplementation(async () => {
      paused = false;
      fireEvent.play(video);
    });
    const pause = vi.spyOn(video, "pause").mockImplementation(() => {
      paused = true;
      fireEvent.pause(video);
    });

    const playEvent = new KeyboardEvent("keydown", {
      key: " ",
      code: "Space",
      bubbles: true,
      cancelable: true,
    });
    fireEvent(document.body, playEvent);
    expect(play).toHaveBeenCalledTimes(1);
    expect(playEvent.defaultPrevented).toBe(true);

    fireEvent.keyDown(document.body, { key: " ", code: "Space", repeat: true });
    expect(play).toHaveBeenCalledTimes(1);
    expect(pause).not.toHaveBeenCalled();

    fireEvent.keyDown(document.body, { key: " ", code: "Space" });
    expect(pause).toHaveBeenCalledTimes(1);
  });

  it("用左右方向键按三十帧逐帧并跨越片段边界", async () => {
    const { api, clip } = createKeyboardClipFixture();
    const pause = vi.spyOn(HTMLMediaElement.prototype, "pause").mockImplementation(() => undefined);
    const { video } = await openKeyboardClipEditor(api);

    fireEvent.keyDown(document.body, { key: "ArrowRight" });
    expect(video.currentTime).toBeCloseTo(clip.segments[0].sourceStartMs / 1_000 + 1 / 30, 4);
    fireEvent.keyDown(document.body, { key: "ArrowRight", repeat: true });
    expect(video.currentTime).toBeCloseTo(clip.segments[0].sourceStartMs / 1_000 + 2 / 30, 4);
    expect(pause).toHaveBeenCalledTimes(2);

    video.currentTime = clip.segments[0].sourceEndMs / 1_000 - 0.06;
    fireEvent.timeUpdate(video);
    fireEvent.keyDown(document.body, { key: "ArrowRight" });
    fireEvent.keyDown(document.body, { key: "ArrowRight", repeat: true });

    await waitFor(() => expect(screen.getByRole("button", { name: `片段 2：${clip.segments[1].title}` })).toHaveClass("active"));
    const secondVideo = document.querySelector<HTMLVideoElement>(".clip-preview-frame video")!;
    expect(secondVideo).toBe(video);
    await waitFor(() => expect(secondVideo.currentTime).toBeCloseTo(clip.segments[1].sourceStartMs / 1_000 + 0.007, 2));
    fireEvent.keyDown(document.body, { key: "ArrowLeft" });
    await waitFor(() => expect(screen.getByRole("button", { name: `片段 1：${clip.segments[0].title}` })).toHaveClass("active"));
    await waitFor(() => expect(document.querySelector<HTMLVideoElement>(".clip-preview-frame video")?.currentTime).toBeCloseTo(2.973, 2));
  });

  it("用组合方向键和首尾键跳转并保持播放状态", async () => {
    const { api, clip } = createKeyboardClipFixture();
    const play = vi.spyOn(HTMLMediaElement.prototype, "play").mockResolvedValue(undefined);
    vi.spyOn(HTMLMediaElement.prototype, "pause").mockImplementation(() => undefined);
    const { video } = await openKeyboardClipEditor(api);
    fireEvent.play(video);

    fireEvent.keyDown(document.body, { key: "ArrowRight", shiftKey: true });
    await waitFor(() => expect(screen.getByRole("button", { name: `片段 2：${clip.segments[1].title}` })).toHaveClass("active"));
    await waitFor(() => expect(document.querySelector<HTMLVideoElement>(".clip-preview-frame video")?.currentTime).toBe(8));
    let currentVideo = document.querySelector<HTMLVideoElement>(".clip-preview-frame video")!;
    await waitFor(() => expect(play).toHaveBeenCalled());

    fireEvent.keyDown(document.body, { key: "ArrowLeft", shiftKey: true });
    await waitFor(() => expect(screen.getByRole("button", { name: `片段 1：${clip.segments[0].title}` })).toHaveClass("active"));
    await waitFor(() => expect(document.querySelector<HTMLVideoElement>(".clip-preview-frame video")?.currentTime).toBe(1));
    currentVideo = document.querySelector<HTMLVideoElement>(".clip-preview-frame video")!;

    fireEvent.keyDown(document.body, { key: "End" });
    await waitFor(() => expect(screen.getByRole("button", { name: `片段 2：${clip.segments[1].title}` })).toHaveClass("active"));
    await waitFor(() => expect(document.querySelector<HTMLVideoElement>(".clip-preview-frame video")?.currentTime).toBe(9));
    currentVideo = document.querySelector<HTMLVideoElement>(".clip-preview-frame video")!;
    fireEvent.keyDown(document.body, { key: "Home" });
    await waitFor(() => expect(screen.getByRole("button", { name: `片段 1：${clip.segments[0].title}` })).toHaveClass("active"));
    await waitFor(() => expect(document.querySelector<HTMLVideoElement>(".clip-preview-frame video")?.currentTime).toBe(1));
    currentVideo = document.querySelector<HTMLVideoElement>(".clip-preview-frame video")!;
    fireEvent.keyDown(document.body, { key: "ArrowLeft", shiftKey: true });
    expect(currentVideo.currentTime).toBe(1);
  });

  it("用 M 切换静音并用加减键限制时间轴缩放", async () => {
    const { api } = createKeyboardClipFixture();
    const { video } = await openKeyboardClipEditor(api);
    const zoom = screen.getByLabelText("时间轴缩放");

    fireEvent.keyDown(document.body, { key: "m" });
    await waitFor(() => expect(video.muted).toBe(true));
    fireEvent.keyDown(document.body, { key: "m", repeat: true });
    expect(video.muted).toBe(true);
    fireEvent.keyDown(document.body, { key: "M" });
    await waitFor(() => expect(video.muted).toBe(false));

    fireEvent.keyDown(document.body, { key: "+" });
    expect(zoom).toHaveValue("55");
    fireEvent.keyDown(document.body, { key: "=" });
    expect(zoom).toHaveValue("60");
    fireEvent.keyDown(document.body, { key: "-" });
    expect(zoom).toHaveValue("55");
    for (let index = 0; index < 20; index += 1) fireEvent.keyDown(document.body, { key: "+" });
    expect(zoom).toHaveValue("100");
    fireEvent.keyDown(document.body, { key: "+" });
    expect(zoom).toHaveValue("100");
    for (let index = 0; index < 25; index += 1) fireEvent.keyDown(document.body, { key: "-" });
    expect(zoom).toHaveValue("1");

    expect(screen.getByRole("button", { name: "播放视频" })).toHaveAttribute("title", "播放（Space）");
    expect(screen.getByRole("button", { name: "上一帧" })).toHaveAttribute("title", "上一帧（←）");
    expect(screen.getByRole("button", { name: "下一帧" })).toHaveAttribute("title", "下一帧（→）");
    expect(screen.getByRole("button", { name: "静音" })).toHaveAttribute("title", "静音（M）");
    expect(zoom.closest("label")).toHaveAttribute("title", "时间轴缩放（+ / -）");
  });

  it("在输入控件、组合输入和系统修饰键期间不触发快捷键", async () => {
    const { api } = createKeyboardClipFixture();
    const { editor, user, video } = await openKeyboardClipEditor(api);
    const play = vi.spyOn(video, "play").mockResolvedValue(undefined);
    const pause = vi.spyOn(video, "pause").mockImplementation(() => undefined);
    const zoom = screen.getByLabelText("时间轴缩放");

    await user.click(screen.getByRole("button", { name: "字幕属性" }));
    await user.click(screen.getByRole("button", { name: "字幕列表：快捷键输入保护。" }));
    const textarea = screen.getByLabelText("字幕文本");
    const search = screen.getByLabelText("搜索工程字幕");
    const playButton = screen.getByRole("button", { name: "播放视频" });
    const editable = document.createElement("div");
    editable.setAttribute("contenteditable", "true");
    editor.append(editable);

    fireEvent.keyDown(textarea, { key: " ", code: "Space" });
    fireEvent.keyDown(search, { key: "m" });
    fireEvent.keyDown(zoom, { key: "ArrowRight" });
    fireEvent.keyDown(playButton, { key: "ArrowRight" });
    fireEvent.keyDown(editable, { key: "m" });
    const composingEvent = new KeyboardEvent("keydown", { key: "m", bubbles: true, cancelable: true });
    Object.defineProperty(composingEvent, "isComposing", { configurable: true, value: true });
    fireEvent(document.body, composingEvent);
    fireEvent.keyDown(document.body, { key: "m", ctrlKey: true });
    fireEvent.keyDown(document.body, { key: "m", metaKey: true });
    fireEvent.keyDown(document.body, { key: "ArrowRight", altKey: true });

    expect(play).not.toHaveBeenCalled();
    expect(pause).not.toHaveBeenCalled();
    expect(video.muted).toBe(false);
    expect(zoom).toHaveValue("50");
    editable.remove();
  });

  it("离开剪辑页后清理快捷键监听", async () => {
    const { api } = createKeyboardClipFixture();
    const { unmount, video } = await openKeyboardClipEditor(api);
    const play = vi.spyOn(video, "play").mockResolvedValue(undefined);

    unmount();
    fireEvent.keyDown(document.body, { key: " ", code: "Space" });
    expect(play).not.toHaveBeenCalled();
  });

  it("从已选择候选进入独立剪辑页并保存片段、导出和返回", async () => {
    const user = userEvent.setup();
    const play = vi.fn().mockResolvedValue(undefined);
    const pause = vi.fn();
    Object.defineProperty(HTMLMediaElement.prototype, "play", { configurable: true, value: play });
    Object.defineProperty(HTMLMediaElement.prototype, "pause", { configurable: true, value: pause });
    const gain = { gain: { value: 1 }, connect: vi.fn() };
    class MockAudioContext {
      state = "running";
      destination = {};
      createMediaElementSource = vi.fn().mockReturnValue({ connect: vi.fn().mockReturnValue(gain) });
      createGain = vi.fn().mockReturnValue(gain);
      resume = vi.fn().mockResolvedValue(undefined);
      close = vi.fn().mockResolvedValue(undefined);
    }
    vi.stubGlobal("AudioContext", MockAudioContext);
    const selectedCandidates = highlightCandidates.map((candidate, index) => ({
      ...candidate,
      totalScore: index === 0 ? 82 : 74,
      selected: true,
    }));
    const appendableCandidate: AiHighlightCandidate = {
      ...selectedCandidates[1],
      id: 503,
      candidateKey: "candidate-appendable",
      title: "可追加的视频片段",
      startMs: 8_000,
      endMs: 9_500,
      totalScore: 86,
      selected: true,
    };
    const candidatePage: AiHighlightCandidatePage = {
      items: selectedCandidates,
      page: 0,
      pageSize: 50,
      totalCandidates: 2,
      qualifiedCandidates: 2,
      selectedCandidates: 2,
    };
    const selectedCandidatePage: AiHighlightCandidatePage = {
      ...candidatePage,
      items: [...selectedCandidates, appendableCandidate],
      totalCandidates: 3,
      qualifiedCandidates: 3,
      selectedCandidates: 3,
    };
    const clip: AiClipProjectDetail = {
      project: {
        id: 701,
        highlightRunId: completedHighlightRun.id,
        name: "整场直播转写 - 高光剪辑",
        outputWidth: 1920,
        outputHeight: 1080,
        version: 1,
        exportStatus: "idle",
        exportProgress: 0,
        outputPath: null,
        lastErrorCode: null,
        lastErrorMessage: null,
        createdAt: "2026-07-22T00:20:00Z",
        updatedAt: "2026-07-22T00:20:00Z",
      },
      segments: selectedCandidates.map((candidate, index) => ({
        id: 711 + index,
        clipProjectId: 701,
        candidateId: candidate.id,
        inputId: candidate.inputId,
        position: index,
        title: candidate.title,
        sourceStartMs: candidate.startMs,
        sourceEndMs: candidate.endMs,
        volumePercent: 100,
        effect: "none" as const,
      })),
      subtitles: [{
        id: 731,
        clipProjectId: 701,
        stableSegmentId: "clip-subtitle-first",
        clipSegmentId: 711,
        inputId: selectedCandidates[0].inputId,
        originalText: "第一段 ASR 字幕。",
        text: "第一段 ASR 字幕。",
        hidden: false,
        sourceStartMs: 1_000,
        sourceEndMs: 2_000,
        projectStartMs: 0,
        projectEndMs: 1_000,
      }, {
        id: 732,
        clipProjectId: 701,
        stableSegmentId: "clip-subtitle-second",
        clipSegmentId: 712,
        inputId: selectedCandidates[1].inputId,
        originalText: "第二段 ASR 字幕。",
        text: "第二段 ASR 字幕。",
        hidden: false,
        sourceStartMs: 500,
        sourceEndMs: 1_500,
        projectStartMs: 17_000,
        projectEndMs: 18_000,
      }],
      subtitleFrames: [{
        subtitleId: 731,
        clipSegmentId: 711,
        projectStartMs: 0,
        projectEndMs: 400,
        pageText: "第一段 ASR 字幕。",
        visibleText: "第一段",
        hiddenText: " ASR 字幕。",
      }, {
        subtitleId: 731,
        clipSegmentId: 711,
        projectStartMs: 400,
        projectEndMs: 1_000,
        pageText: "第一段 ASR 字幕。",
        visibleText: "第一段 ASR 字幕。",
        hiddenText: "",
      }, {
        subtitleId: 732,
        clipSegmentId: 712,
        projectStartMs: 17_000,
        projectEndMs: 18_000,
        pageText: "第二段 ASR 字幕。",
        visibleText: "第二段 ASR 字幕。",
        hiddenText: "",
      }],
      subtitlesComplete: true,
    };
    const api = createAiApi();
    api.getLatestAiHighlightRun = vi.fn().mockResolvedValue(completedHighlightRun);
    api.listQualifiedAiHighlightCandidates = vi.fn().mockResolvedValue(candidatePage);
    api.listSelectedAiHighlightCandidates = vi.fn().mockResolvedValue(selectedCandidatePage);
    api.openAiClipProject = vi.fn().mockResolvedValue(clip);
    let clipState = clip;
    let rejectNextSubtitleSave = false;
    api.getAiClipProject = vi.fn().mockImplementation(async () => clipState);
    api.updateAiClipSegment = vi.fn().mockImplementation(async (_projectId, segmentId, update) => {
      clipState = { ...clipState, segments: clipState.segments.map((segment) => segment.id === segmentId ? { ...segment, ...update } : segment) };
      return clipState;
    });
    api.insertAiClipCandidate = vi.fn().mockImplementation(async (_projectId, candidateId, insertIndex) => {
      const candidate = selectedCandidatePage.items.find((item) => item.id === candidateId)!;
      const segments = [...clipState.segments];
      segments.splice(insertIndex, 0, {
        id: 713,
        clipProjectId: 701,
        candidateId: candidate.id,
        inputId: candidate.inputId,
        position: insertIndex,
        title: candidate.title,
        sourceStartMs: candidate.startMs,
        sourceEndMs: candidate.endMs,
        volumePercent: 100,
        effect: "none",
      });
      clipState = {
        ...clipState,
        segments: segments.map((segment, index) => ({ ...segment, position: index })),
        subtitles: [...clipState.subtitles, {
          id: 733,
          clipProjectId: 701,
          stableSegmentId: "clip-subtitle-appended",
          clipSegmentId: 713,
          inputId: candidate.inputId,
          originalText: "追加片段 ASR 字幕。",
          text: "追加片段 ASR 字幕。",
          hidden: false,
          sourceStartMs: candidate.startMs,
          sourceEndMs: candidate.endMs,
          projectStartMs: 34_000,
          projectEndMs: 35_500,
        }],
        subtitleFrames: [...clipState.subtitleFrames, {
          subtitleId: 733,
          clipSegmentId: 713,
          projectStartMs: 34_000,
          projectEndMs: 35_500,
          pageText: "追加片段 ASR 字幕。",
          visibleText: "追加片段 ASR 字幕。",
          hiddenText: "",
        }],
        subtitlesComplete: true,
      };
      return clipState;
    });
    api.reorderAiClipSegments = vi.fn().mockImplementation(async (_projectId: number, orderedIds: number[]) => {
      clipState = { ...clipState, segments: orderedIds.map((id, index) => ({ ...clipState.segments.find((segment) => segment.id === id)!, position: index })) };
      return clipState;
    });
    api.updateAiClipSubtitle = vi.fn().mockImplementation(async (_projectId, subtitleId, update) => {
      if (rejectNextSubtitleSave) {
        rejectNextSubtitleSave = false;
        throw { code: "clip_version_conflict", message: "剪辑工程版本已更新，请重新加载后再编辑字幕" };
      }
      const text = update.text.trim();
      clipState = {
        ...clipState,
        project: {
          ...clipState.project,
          version: clipState.project.version + 1,
          exportStatus: "idle",
          outputPath: null,
        },
        subtitles: clipState.subtitles.map((subtitle) => subtitle.id === subtitleId
          ? { ...subtitle, text, hidden: update.hidden }
          : subtitle),
        subtitleFrames: update.hidden
          ? clipState.subtitleFrames.filter((frame) => frame.subtitleId !== subtitleId)
          : clipState.subtitleFrames.map((frame) => frame.subtitleId === subtitleId
            ? { ...frame, pageText: text, visibleText: text, hiddenText: "" }
            : frame),
      };
      return clipState;
    });
    api.resetAiClipSubtitle = vi.fn().mockImplementation(async (_projectId, subtitleId) => {
      const subtitle = clipState.subtitles.find((item) => item.id === subtitleId)!;
      clipState = {
        ...clipState,
        project: { ...clipState.project, version: clipState.project.version + 1 },
        subtitles: clipState.subtitles.map((item) => item.id === subtitleId
          ? { ...item, text: item.originalText, hidden: false }
          : item),
        subtitleFrames: clipState.subtitleFrames.map((frame) => frame.subtitleId === subtitleId
          ? { ...frame, pageText: subtitle.originalText, visibleText: subtitle.originalText, hiddenText: "" }
          : frame),
      };
      return clipState;
    });
    api.startAiClipExport = vi.fn().mockResolvedValue({ ...clip.project, exportStatus: "exporting", exportProgress: 0 });
    api.cancelAiClipExport = vi.fn().mockResolvedValue({ ...clip.project, exportStatus: "cancelled", exportProgress: 0 });
    render(<AiWorkspace api={api} />);

    await user.click(await screen.findByRole("button", { name: "编辑视频" }));
    expect(await screen.findByRole("main", { name: "视频剪辑页面" })).toBeInTheDocument();
    expect(screen.getByText("时间轨道")).toBeInTheDocument();
    expect(screen.getByRole("complementary", { name: "视频与动画素材库" })).toBeInTheDocument();
    expect(await screen.findByRole("button", { name: "追加视频 1" })).toBeEnabled();
    expect(screen.getByLabelText("时间轴缩放")).toHaveValue("50");
    expect(screen.getByRole("button", { name: "播放视频" })).toBeInTheDocument();
    expect(screen.getByLabelText("播放器字幕")).toHaveTextContent("第一段 ASR 字幕。");
    expect(screen.getByText("已关联 2 条工程字幕，可在字幕面板校对")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "字幕：第一段 ASR 字幕。" })).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: `片段 2：${clip.segments[1].title}` }));
    expect(screen.getByLabelText("播放器字幕")).toHaveTextContent("第二段 ASR 字幕。");
    await user.click(screen.getByRole("button", { name: `片段 1：${clip.segments[0].title}` }));
    expect(screen.getByText("输出规格 1920 × 1080 · 工程版本 1")).toBeInTheDocument();
    await waitFor(() => expect(document.querySelector(".clip-preview-frame video")).not.toBeNull());
    const clipPreviewVideo = document.querySelector<HTMLVideoElement>(".clip-preview-frame video")!;
    clipPreviewVideo.currentTime = 3;
    fireEvent.timeUpdate(clipPreviewVideo);
    expect(screen.queryByLabelText("播放器字幕")).not.toBeInTheDocument();
    clipPreviewVideo.currentTime = 1.5;
    fireEvent.timeUpdate(clipPreviewVideo);
    expect(screen.getByLabelText("播放器字幕")).toHaveTextContent("第一段 ASR 字幕。");
    Object.defineProperties(clipPreviewVideo, {
      videoWidth: { configurable: true, value: 1080 },
      videoHeight: { configurable: true, value: 1920 },
    });
    fireEvent.loadedMetadata(clipPreviewVideo);
    expect(clipPreviewVideo.closest(".clip-preview-frame")).toHaveClass("portrait");
    expect(clipPreviewVideo.closest<HTMLElement>(".clip-preview-frame")?.style.getPropertyValue("--clip-preview-aspect")).toBe("0.5625");
    await user.click(screen.getByRole("button", { name: "播放视频" }));
    expect(play).toHaveBeenCalledTimes(1);
    fireEvent.play(clipPreviewVideo);
    expect(screen.getByRole("button", { name: "暂停视频" })).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "静音" }));
    await waitFor(() => expect(clipPreviewVideo.muted).toBe(true));
    await user.click(screen.getByRole("button", { name: "下一帧" }));
    expect(pause).toHaveBeenCalled();
    fireEvent.change(screen.getByLabelText("时间轴缩放"), { target: { value: "80" } });
    expect(screen.getByLabelText("时间轴缩放")).toHaveValue("80");

    await user.click(screen.getByRole("button", { name: "字幕：第一段 ASR 字幕。" }));
    expect(screen.getByRole("button", { name: "字幕属性" })).toHaveAttribute("aria-pressed", "true");
    expect(screen.getByText("1 / 1 条字幕")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "全部字幕" }));
    await user.type(screen.getByLabelText("搜索工程字幕"), "第二段");
    expect(screen.getByText("1 / 2 条字幕")).toBeInTheDocument();
    await user.clear(screen.getByLabelText("搜索工程字幕"));
    await user.click(screen.getByRole("button", { name: "字幕列表：第二段 ASR 字幕。" }));
    expect(screen.getByRole("button", { name: `片段 2：${clip.segments[1].title}` })).toHaveClass("active");
    await user.click(screen.getByRole("button", { name: "字幕列表：第一段 ASR 字幕。" }));

    const subtitleEditor = screen.getByLabelText("字幕文本");
    await user.clear(subtitleEditor);
    await user.type(subtitleEditor, "不会保存的草稿");
    expect(screen.getByText("未保存")).toBeInTheDocument();
    await user.keyboard("{Escape}");
    expect(subtitleEditor).toHaveValue("第一段 ASR 字幕。");
    expect(api.updateAiClipSubtitle).not.toHaveBeenCalled();

    await user.clear(subtitleEditor);
    await user.type(subtitleEditor, "第一段修正字幕。");
    await user.keyboard("{Control>}{Enter}{/Control}");
    await waitFor(() => expect(api.updateAiClipSubtitle).toHaveBeenCalledWith(701, 731, {
      text: "第一段修正字幕。",
      hidden: false,
      expectedProjectVersion: 1,
    }));
    expect(await screen.findByText("已保存")).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "隐藏字幕" }));
    await waitFor(() => expect(api.updateAiClipSubtitle).toHaveBeenLastCalledWith(701, 731, {
      text: "第一段修正字幕。",
      hidden: true,
      expectedProjectVersion: 2,
    }));
    expect(screen.getByRole("button", { name: "恢复显示" })).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "恢复显示" }));
    await waitFor(() => expect(api.updateAiClipSubtitle).toHaveBeenLastCalledWith(701, 731, {
      text: "第一段修正字幕。",
      hidden: false,
      expectedProjectVersion: 3,
    }));
    await user.click(screen.getByRole("button", { name: "恢复 ASR 原文" }));
    await waitFor(() => expect(api.resetAiClipSubtitle).toHaveBeenCalledWith(701, 731, 4));
    expect(screen.getByLabelText("字幕文本")).toHaveValue("第一段 ASR 字幕。");

    rejectNextSubtitleSave = true;
    await user.clear(screen.getByLabelText("字幕文本"));
    await user.type(screen.getByLabelText("字幕文本"), "冲突草稿");
    await user.keyboard("{Control>}{Enter}{/Control}");
    expect(await screen.findByText("版本冲突")).toBeInTheDocument();
    expect(screen.getByLabelText("字幕文本")).toHaveValue("冲突草稿");
    await user.click(screen.getByRole("button", { name: "重新加载最新工程" }));
    expect(screen.getByLabelText("字幕文本")).toHaveValue("第一段 ASR 字幕。");
    await user.click(screen.getByRole("button", { name: "片段属性" }));

    fireEvent.change(screen.getByLabelText("片段音量"), { target: { value: "135" } });
    await waitFor(() => expect(api.updateAiClipSegment).toHaveBeenCalledWith(701, 711, {
      volumePercent: 135,
      effect: "none",
    }));
    await waitFor(() => expect(gain.gain.value).toBe(1.35));
    await user.click(screen.getByRole("button", { name: "淡入内置效果" }));
    await waitFor(() => expect(api.updateAiClipSegment).toHaveBeenLastCalledWith(701, 711, {
      volumePercent: 135,
      effect: "fade_in",
    }));
    expect(Number(document.querySelector<HTMLVideoElement>(".clip-preview-frame video")?.style.opacity)).toBeLessThan(0.2);

    await user.click(screen.getByRole("button", { name: "转场" }));
    expect(screen.queryByRole("button", { name: "淡入内置效果" })).not.toBeInTheDocument();
    const flashMaterial = screen.getByRole("button", { name: "闪白转场内置效果" });
    const secondTimelineSegment = screen.getByRole("button", { name: `片段 2：${clip.segments[1].title}` });
    fireEvent.dragStart(flashMaterial, { dataTransfer: { effectAllowed: "copy" } });
    fireEvent.dragOver(secondTimelineSegment);
    fireEvent.drop(secondTimelineSegment);
    await waitFor(() => expect(api.updateAiClipSegment).toHaveBeenLastCalledWith(701, 712, {
      volumePercent: 100,
      effect: "flash",
    }));
    const firstDropGap = screen.getByRole("button", { name: "拖放到位置 1" });
    fireEvent.dragStart(flashMaterial, { dataTransfer: { effectAllowed: "copy" } });
    fireEvent.dragOver(firstDropGap);
    fireEvent.drop(firstDropGap);
    await waitFor(() => expect(api.updateAiClipSegment).toHaveBeenLastCalledWith(701, 711, {
      volumePercent: 135,
      effect: "flash",
    }));
    await user.click(screen.getByRole("button", { name: "全部" }));

    await user.click(screen.getByRole("button", { name: "片段下移" }));
    expect(api.reorderAiClipSegments).toHaveBeenCalledWith(701, [712, 711]);

    const firstTimelineSegment = screen.getByRole("button", { name: `片段 1：${clip.segments[1].title}` });
    const finalDropGap = screen.getByRole("button", { name: "拖放到位置 3" });
    fireEvent.dragStart(firstTimelineSegment, { dataTransfer: { effectAllowed: "move" } });
    fireEvent.dragOver(finalDropGap);
    fireEvent.drop(finalDropGap);
    await waitFor(() => expect(api.reorderAiClipSegments).toHaveBeenLastCalledWith(701, [711, 712]));

    await user.click(screen.getByRole("button", { name: "视频" }));
    const videoMaterial = await screen.findByRole("button", { name: "追加视频：可追加的视频片段" });
    await user.click(videoMaterial);
    await waitFor(() => expect(api.insertAiClipCandidate).toHaveBeenCalledWith(701, 503, 2));
    expect(await screen.findByRole("button", { name: "片段 3：可追加的视频片段" })).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "导出 MP4" }));
    expect(api.startAiClipExport).toHaveBeenCalledWith(701);
    expect(screen.getByRole("button", { name: "删除选中" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "片段属性" })).toBeEnabled();
    await user.click(screen.getByRole("button", { name: "字幕属性" }));
    expect(screen.getByLabelText("字幕文本")).toBeDisabled();
    await user.click(await screen.findByRole("button", { name: "取消导出 0%" }));
    expect(api.cancelAiClipExport).toHaveBeenCalledWith(701);
    await user.click(screen.getByRole("button", { name: "返回 AI 剪辑" }));
    expect(await screen.findByText("高光评分与候选")).toBeInTheDocument();
  });

  it("任一剪辑片段缺少 ASR 字幕时禁用导出并显示处理方式", async () => {
    const user = userEvent.setup();
    const selectedCandidate = { ...highlightCandidates[0], selected: true };
    const candidatePage: AiHighlightCandidatePage = {
      items: [selectedCandidate],
      page: 0,
      pageSize: 50,
      totalCandidates: 1,
      qualifiedCandidates: 1,
      selectedCandidates: 1,
    };
    const incompleteClip: AiClipProjectDetail = {
      project: {
        id: 702,
        highlightRunId: completedHighlightRun.id,
        name: "缺少字幕的剪辑工程",
        outputWidth: 1920,
        outputHeight: 1080,
        version: 1,
        exportStatus: "idle",
        exportProgress: 0,
        outputPath: null,
        lastErrorCode: null,
        lastErrorMessage: null,
        createdAt: "2026-07-22T00:20:00Z",
        updatedAt: "2026-07-22T00:20:00Z",
      },
      segments: [{
        id: 721,
        clipProjectId: 702,
        candidateId: selectedCandidate.id,
        inputId: selectedCandidate.inputId,
        position: 0,
        title: selectedCandidate.title,
        sourceStartMs: selectedCandidate.startMs,
        sourceEndMs: selectedCandidate.endMs,
        volumePercent: 100,
        effect: "none",
      }],
      subtitles: [],
      subtitleFrames: [],
      subtitlesComplete: false,
    };
    const api = createAiApi();
    api.getLatestAiHighlightRun = vi.fn().mockResolvedValue(completedHighlightRun);
    api.listQualifiedAiHighlightCandidates = vi.fn().mockResolvedValue(candidatePage);
    api.listSelectedAiHighlightCandidates = vi.fn().mockResolvedValue(candidatePage);
    api.openAiClipProject = vi.fn().mockResolvedValue(incompleteClip);
    api.startAiClipExport = vi.fn();
    render(<AiWorkspace api={api} />);

    await user.click(await screen.findByRole("button", { name: "编辑视频" }));
    expect(await screen.findByRole("main", { name: "视频剪辑页面" })).toBeInTheDocument();
    expect(screen.getByText("部分片段没有 ASR 字幕，请补全识别或移除后再导出")).toBeInTheDocument();
    const exportButton = screen.getByRole("button", { name: "导出 MP4" });
    expect(exportButton).toBeDisabled();
    await user.click(exportButton);
    expect(api.startAiClipExport).not.toHaveBeenCalled();
  });

  it("允许审计高光分析的请求、步骤、输入范围和结构化结果", async () => {
    const user = userEvent.setup();
    vi.spyOn(window, "confirm").mockReturnValue(true);
    const api = createAiApi();
    api.getAiLlmSettings = vi.fn().mockResolvedValue({
      provider: "deepseek",
      modelId: "deepseek-chat",
      timeoutMs: 30_000,
      promptVersion: "highlight-v1",
      qualifiedScore: 70,
      excellentScore: 80,
      keyConfigured: true,
      updatedAt: "2026-07-22T00:00:00Z",
    });
    api.startAiHighlightAnalysis = vi.fn().mockResolvedValue(completedHighlightRun);
    api.listAiHighlightCandidates = vi.fn().mockResolvedValue(highlightCandidates);
    render(<AiWorkspace api={api} />);

    const startButton = await screen.findByRole("button", { name: "开始高光分析" });
    await waitFor(() => expect(startButton).toBeEnabled());
    await user.click(startButton);
    await user.click(await screen.findByText("分析详情"));

    expect(screen.getByText("候选发现 Agent")).toBeInTheDocument();
    expect(screen.getByText("全局评分 Agent")).toBeInTheDocument();
    expect(screen.getByLabelText("LLM 请求摘要")).toHaveTextContent("受限的只读高光分析 Agent");
    expect(screen.getByLabelText("LLM 请求摘要")).toHaveTextContent("15 到 90 秒的高光候选");
    expect(screen.getByLabelText("LLM 结构化结果")).toHaveTextContent('"totalScore": 82');
    expect(screen.getByLabelText("LLM 结构化结果")).toHaveTextContent("价格信息完整，并且有明确反转。");
    expect(screen.getByText(/模型未返回的内部推理过程不可读取/)).toBeInTheDocument();

    await user.click(screen.getByText("本次运行使用的规范化文本"));
    const inputRange = screen.getByLabelText("高光分析输入范围");
    expect(within(inputRange).getByText("欢迎来到直播间。")).toBeInTheDocument();
    expect(within(inputRange).getByText("今天价格99元。")).toBeInTheDocument();
    expect(within(inputRange).getAllByLabelText("价格反转 候选整体评分 82 分")).toHaveLength(2);
    expect(within(inputRange).queryByLabelText("普通互动 候选整体评分 64 分")).not.toBeInTheDocument();
  });

  it("播放器不可定位时仍允许复制文本，并提供 TXT 与 JSON 只读导出", async () => {
    const user = userEvent.setup();
    const api = createAiApi();
    api.requestAiInputPreview = vi.fn().mockRejectedValue(new Error("预览时间无法可靠映射"));
    render(<AiWorkspace api={api} />);

    expect(await screen.findByText("预览时间无法可靠映射")).toBeInTheDocument();
    expect(screen.getByText("欢迎来到直播间。").closest("button")).toBeDisabled();
    await user.click(screen.getByRole("button", { name: "复制当前视频" }));
    await waitFor(() => expect(api.copyAiInputText).toHaveBeenCalledWith(project.id, completedInput.id));
    await user.click(screen.getByRole("button", { name: "TXT" }));
    await user.click(screen.getByRole("button", { name: "JSON" }));
    expect(api.exportAiTxt).toHaveBeenCalledWith(project.id);
    expect(api.exportAiJson).toHaveBeenCalledWith(project.id);
  });

  it("长直播句段按 200 条分页，关闭跟随后可自由切换页面", async () => {
    const user = userEvent.setup();
    const longSegments = Array.from({ length: 401 }, (_, index): AiTranscriptSegment => ({
      stableSegmentId: `seg-${index}`,
      inputId: completedInput.id,
      videoId: null,
      sourceStartMs: index * 1_000,
      sourceEndMs: index * 1_000 + 900,
      projectStartMs: index * 1_000,
      projectEndMs: index * 1_000 + 900,
      rawText: `句段 ${index}`,
      normalizedText: `句段 ${index}`,
      confidence: null,
    }));
    const longProjection: AiTranscriptProjection = {
      ...transcript,
      inputs: [{ ...transcript.inputs[0], segments: longSegments }],
    };
    render(<AiWorkspace api={createAiApi(detail, longProjection)} />);

    expect(await screen.findByText("句段 0")).toBeInTheDocument();
    expect(screen.queryByText("句段 250")).not.toBeInTheDocument();
    await user.click(screen.getByLabelText("跟随播放"));
    await user.click(screen.getByRole("button", { name: /下一页/ }));
    expect(await screen.findByText("句段 250")).toBeInTheDocument();
    expect(screen.getByText("2 / 3 · 共 401 句")).toBeInTheDocument();
  });

  it("运行项目支持取消，失败或取消项都可以重新识别，且只删除 AI 数据", async () => {
    const user = userEvent.setup();
    vi.spyOn(window, "confirm").mockReturnValue(true);
    const runningProject = { ...project, status: "running" as const, progressPercent: 75 };
    const failedInput = {
      ...completedInput,
      id: 302,
      status: "failed" as const,
      lastErrorCode: "no_audio_track",
      lastErrorMessage: "视频没有音轨，无法进行语音识别",
    };
    const runningDetail: AiProjectDetail = {
      project: runningProject,
      inputs: [{ ...completedInput, status: "transcribing", progressPercent: 75 }, failedInput],
    };
    const api = createAiApi(runningDetail, {
      project: runningProject,
      inputs: [transcript.inputs[0], { ...transcript.inputs[0], inputId: failedInput.id, status: "failed", segments: [], errorMessage: failedInput.lastErrorMessage }],
    });
    render(<AiWorkspace api={api} />);

    await user.click(await screen.findByRole("button", { name: "取消分析" }));
    expect(api.cancelAiProject).toHaveBeenCalledWith(project.id);
    await user.click(screen.getByRole("button", { name: `重新识别 ${failedInput.displayName}` }));
    expect(api.retryAiInput).toHaveBeenCalledWith(failedInput.id);
    await user.click(screen.getByRole("button", { name: "删除 AI 项目" }));
    expect(window.confirm).toHaveBeenCalledWith(expect.stringContaining("绝不会删除原始视频"));
    expect(api.deleteAiProject).toHaveBeenCalledWith(project.id);
  });

  it("已取消的输入显示重新识别入口", async () => {
    const cancelledProject = { ...project, status: "cancelled" as const, progressPercent: 100 };
    const cancelledInput = {
      ...completedInput,
      id: 303,
      status: "cancelled" as const,
      lastErrorCode: "user_cancelled",
      lastErrorMessage: "用户已取消语音识别",
    };
    const detail: AiProjectDetail = {
      project: cancelledProject,
      inputs: [cancelledInput],
    };
    const api = createAiApi(detail, {
      project: cancelledProject,
      inputs: [{ ...transcript.inputs[0], inputId: cancelledInput.id, status: "cancelled", segments: [], errorMessage: cancelledInput.lastErrorMessage }],
    });
    const user = userEvent.setup();
    render(<AiWorkspace api={api} />);

    const retry = await screen.findByRole("button", { name: `重新识别 ${cancelledInput.displayName}` });
    await user.click(retry);
    expect(api.retryAiInput).toHaveBeenCalledWith(cancelledInput.id);
  });

  it("状态事件丢失后使用主动查询恢复，且第一版不出现编辑与视频渲染入口", async () => {
    let listener: ((event: {
      projectId: number;
      inputId: number;
      projectStatus: "running";
      projectProgressPercent: number;
      inputStatus: "transcribing";
      inputProgressPercent: number;
      stage: string;
      message: string;
    }) => void) | undefined;
    const api = createAiApi();
    api.subscribeAi = vi.fn().mockImplementation(async (callback) => {
      listener = callback;
      return () => undefined;
    });
    render(<AiWorkspace api={api} />);
    await screen.findByText("欢迎来到直播间。");

    await act(async () => {
      listener?.({
        projectId: project.id,
        inputId: completedInput.id,
        projectStatus: "running",
        projectProgressPercent: 82,
        inputStatus: "transcribing",
        inputProgressPercent: 91,
        stage: "transcribing",
        message: "正在识别语音",
      });
      await new Promise((resolve) => window.setTimeout(resolve, 180));
    });
    expect(api.getAiProject).toHaveBeenCalledTimes(2);
    for (const unsupported of ["编辑转写", "SRT", "ASS", "波形", "多轨道", "裁剪视频", "渲染视频"]) {
      expect(screen.queryByRole("button", { name: unsupported })).not.toBeInTheDocument();
    }
  });
});
