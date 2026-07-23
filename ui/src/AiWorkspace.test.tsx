import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { AiWorkspace } from "./AiWorkspace";
import type {
  AiEnvironmentDiagnostic,
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

  it("展示播放器与只读时间戳文本，点击句段跳转并用 WebView 元素显示临时字幕", async () => {
    const user = userEvent.setup();
    const api = createAiApi();
    render(<AiWorkspace api={api} />);

    expect(await screen.findByText("欢迎来到直播间。")).toBeInTheDocument();
    expect(screen.getAllByText(/Windows 中文目录/).length).toBeGreaterThan(0);
    expect(api.requestAiInputPreview).toHaveBeenCalledWith(project.id, completedInput.id);
    const video = await screen.findByLabelText("AI 视频播放器") as HTMLVideoElement;
    await user.click(screen.getByText("欢迎来到直播间。").closest("button")!);
    expect(video.currentTime).toBe(1);

    expect(screen.getByLabelText("显示字幕")).toBeChecked();
    Object.defineProperty(video, "currentTime", { configurable: true, value: 1.2, writable: true });
    fireEvent.timeUpdate(video);
    expect(await screen.findByText("欢迎来到直播间。", { selector: ".ai-subtitle-overlay" })).toBeInTheDocument();
    expect(screen.getByText("欢迎来到直播间。", { selector: ".ai-segment-main p" }).closest(".ai-segment-row")).toHaveClass("active");
    expect(api.exportAiTxt).not.toHaveBeenCalled();
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

  it("运行项目支持取消，部分完成项目支持失败项重试和只删除 AI 数据", async () => {
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
    await user.click(screen.getByRole("button", { name: `重试 ${failedInput.displayName}` }));
    expect(api.retryAiInput).toHaveBeenCalledWith(failedInput.id);
    await user.click(screen.getByRole("button", { name: "删除 AI 项目" }));
    expect(window.confirm).toHaveBeenCalledWith(expect.stringContaining("绝不会删除原始视频"));
    expect(api.deleteAiProject).toHaveBeenCalledWith(project.id);
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
