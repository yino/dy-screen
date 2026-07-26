import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { AiWorkspace } from "./AiWorkspace";
import type {
  AiEnvironmentDiagnostic,
  AiHighlightCandidate,
  AiHighlightRun,
  AiProject,
  AiProjectDetail,
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

beforeEach(() => {
  vi.restoreAllMocks();
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
    const session = {
      sessionId: 88,
      streamerName: "正在直播主播的历史回放",
      startedAt: "2026-07-21T20:00:00Z",
      endedAt: "2026-07-21T22:00:00Z",
      videoCount: 3,
      totalDurationMs: 7_200_000,
      unavailableVideoCount: 1,
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
      subscribeAi: vi.fn().mockResolvedValue(() => undefined),
      subscribePreview: vi.fn().mockResolvedValue(() => undefined),
      pickAiLocalVideos,
      addAiCompletedSession,
      removeAiInput,
    } as unknown as ClientApi;
    render(<AiWorkspace api={api} />);

    await user.selectOptions(await screen.findByLabelText("选择已结束直播"), "88");
    expect(screen.getByRole("option", { name: /正在直播主播的历史回放/ })).toBeInTheDocument();
    expect(screen.getByLabelText("已选历史直播详情")).toHaveTextContent("3 个分片");
    expect(screen.getByLabelText("已选历史直播详情")).toHaveTextContent("1 个不可用");

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

  it("展示播放器与只读时间戳文本，点击句段跳转并用 WebView 元素显示临时字幕", async () => {
    const user = userEvent.setup();
    const api = createAiApi();
    render(<AiWorkspace api={api} />);

    expect(await screen.findByText("欢迎来到直播间。")).toBeInTheDocument();
    expect(screen.getAllByText(/Windows 中文目录/).length).toBeGreaterThan(0);
    expect(api.requestAiInputPreview).toHaveBeenCalledWith(project.id, completedInput.id);
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
    expect(screen.getByText("1 个视频 · 2 个句段")).toBeInTheDocument();
    expect(screen.getByText(/按视频分批生成候选/)).toBeInTheDocument();
    expect(screen.queryByText(/完成分析后/)).not.toBeInTheDocument();

    completeAnalysis(completedHighlightRun);

    await waitFor(() => expect(screen.queryByRole("progressbar", { name: "高光分析进行中" })).not.toBeInTheDocument());
    expect(screen.getByText("状态：已完成")).toBeInTheDocument();
    expect(screen.getByText("范围：1 批 · 2 句")).toBeInTheDocument();
    expect(screen.getByText("模型消耗：320 Token")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "刷新分析结果" })).toBeEnabled();
    expect(screen.getByText("分析已完成，但模型没有返回可用候选。")).toBeInTheDocument();
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

  it("将高光候选与 ASR 整合，并且一次只展开当前候选详情", async () => {
    const user = userEvent.setup();
    const api = createAiApi();
    api.getLatestAiHighlightRun = vi.fn().mockResolvedValue(completedHighlightRun);
    api.listAiHighlightCandidates = vi.fn().mockResolvedValue(highlightCandidates);
    render(<AiWorkspace api={api} />);

    expect(await screen.findByText("2 个评分候选")).toBeInTheDocument();
    expect(screen.getByText("1 个达到 70 分 · 1 个参考候选")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "全文" })).toHaveAttribute("aria-pressed", "true");
    expect(screen.getByRole("button", { name: "高光候选 2" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "已选择 0" })).toBeInTheDocument();
    expect(screen.queryByText(highlightCandidates[0].reason)).not.toBeInTheDocument();
    expect(screen.queryByText(highlightCandidates[1].reason)).not.toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "高光候选 2" }));

    const navigation = screen.getByLabelText("高光候选导航");
    expect(within(navigation).getByRole("button", { name: "查看候选 价格反转，82 分" })).toBeInTheDocument();
    expect(within(navigation).getByRole("button", { name: "查看候选 普通互动，64 分" })).toBeInTheDocument();
    expect(within(navigation).getAllByRole("button")[0]).toHaveAccessibleName("查看候选 价格反转，82 分");
    expect(screen.getByLabelText("当前高光候选")).toHaveTextContent("价格反转");
    expect(screen.getByLabelText("价格反转 评分明细")).toHaveTextContent("86吸引力");
    expect(screen.getByLabelText("价格反转 评分明细")).toHaveTextContent("91标签相关");
    expect(screen.getByText(highlightCandidates[0].reason)).toBeInTheDocument();
    expect(screen.queryByLabelText("普通互动 评分明细")).not.toBeInTheDocument();
    expect(screen.getAllByLabelText("句段包含 2 个高光候选")).toHaveLength(2);
    expect(screen.queryByRole("button", { name: "保存候选选择" })).not.toBeInTheDocument();

    await user.selectOptions(screen.getByLabelText("候选排序"), "time");
    expect(within(navigation).getAllByRole("button")[0]).toHaveAccessibleName("查看候选 普通互动，64 分");
    await user.click(within(navigation).getByRole("button", { name: "查看候选 普通互动，64 分" }));
    expect(screen.getByLabelText("当前高光候选")).toHaveTextContent("普通互动");
    expect(screen.getByLabelText("普通互动 评分明细")).toHaveTextContent("65传播性");
    expect(screen.queryByText(highlightCandidates[0].reason)).not.toBeInTheDocument();
  });

  it("重新打开项目时恢复最近一次高光运行和评分候选", async () => {
    const api = createAiApi();
    api.getLatestAiHighlightRun = vi.fn().mockResolvedValue(completedHighlightRun);
    api.listAiHighlightCandidates = vi.fn().mockResolvedValue(highlightCandidates);
    render(<AiWorkspace api={api} />);

    expect(await screen.findByText("2 个评分候选")).toBeInTheDocument();
    await userEvent.setup().click(screen.getByRole("button", { name: "高光候选 2" }));
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

    await user.click(await screen.findByRole("button", { name: "高光候选 3" }));
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

  it("即时保存加入待切片状态，并在失败时保留后端已确认选择", async () => {
    const user = userEvent.setup();
    const api = createAiApi();
    api.getLatestAiHighlightRun = vi.fn().mockResolvedValue(completedHighlightRun);
    api.listAiHighlightCandidates = vi.fn().mockResolvedValue(highlightCandidates);
    api.selectAiHighlightCandidates = vi.fn()
      .mockResolvedValueOnce(highlightCandidates.map((candidate) => ({
        ...candidate,
        selected: candidate.id === highlightCandidates[0].id,
      })))
      .mockRejectedValueOnce(new Error("选择保存失败"));
    render(<AiWorkspace api={api} />);

    await user.click(await screen.findByRole("button", { name: "高光候选 2" }));
    const selection = screen.getByRole("checkbox", { name: "加入待切片" });
    await user.click(selection);

    await waitFor(() => expect(api.selectAiHighlightCandidates).toHaveBeenCalledWith(
      completedHighlightRun.id,
      [highlightCandidates[0].id],
    ));
    expect(selection).toBeChecked();
    expect(screen.getByRole("button", { name: "已选择 1" })).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "已选择 1" }));
    expect(within(screen.getByLabelText("高光候选导航")).getAllByRole("button")).toHaveLength(1);
    expect(screen.getByLabelText("当前高光候选")).toHaveTextContent("价格反转");
    await user.click(screen.getByRole("button", { name: "高光候选 2" }));
    await user.click(screen.getByRole("button", { name: "查看候选 普通互动，64 分" }));
    const secondSelection = screen.getByRole("checkbox", { name: "加入待切片" });
    await user.click(secondSelection);

    await waitFor(() => expect(screen.getByText("选择保存失败")).toBeInTheDocument());
    expect(secondSelection).not.toBeChecked();
    expect(screen.getByRole("button", { name: "已选择 1" })).toBeInTheDocument();
    expect(api.selectAiHighlightCandidates).toHaveBeenLastCalledWith(
      completedHighlightRun.id,
      [highlightCandidates[0].id, highlightCandidates[1].id],
    );
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
    expect(within(inputRange).getAllByLabelText("普通互动 候选整体评分 64 分")).toHaveLength(2);
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
