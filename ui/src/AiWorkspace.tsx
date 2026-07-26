import { convertFileSrc } from "@tauri-apps/api/core";
import {
  AlertTriangle,
  ArrowDown,
  ArrowUp,
  CheckCircle2,
  ChevronLeft,
  ChevronRight,
  Clipboard,
  Download,
  FileJson,
  FileText,
  FolderPlus,
  GripVertical,
  LoaderCircle,
  Play,
  Plus,
  RefreshCw,
  RotateCcw,
  Sparkles,
  Trash2,
  Video,
  X,
} from "lucide-react";
import { FormEvent, useCallback, useEffect, useMemo, useRef, useState } from "react";
import type {
  AiEnvironmentDiagnostic,
  AiHighlightCandidate,
  AiHighlightRun,
  AiInputStatus,
  AiJobEvent,
  AiProject,
  AiProjectDetail,
  AiProjectInput,
  AiProjectStatus,
  AiSessionOption,
  AiTranscriptInput,
  AiTranscriptProjection,
  AiTranscriptSegment,
  ClientApi,
  LlmProviderSettings,
  PreviewSnapshot,
} from "./types";

const segmentPageSize = 200;

const projectStatusLabels: Record<AiProjectStatus, string> = {
  draft: "草稿",
  queued: "排队中",
  running: "分析中",
  deleting: "删除中",
  completed: "已完成",
  completed_with_errors: "部分完成",
  cancelled: "已取消",
  failed: "失败",
};

const inputStatusLabels: Record<AiInputStatus, string> = {
  pending: "等待处理",
  validating: "验证原视频",
  preparing_audio: "提取音频",
  detecting_speech: "检测人声",
  transcribing: "识别语音",
  completed: "已完成",
  skipped: "无人声",
  cancelled: "已取消",
  failed: "失败",
};

function safeError(error: unknown, fallback: string): string {
  if (typeof error === "string" && error.trim()) return error;
  if (error instanceof Error && error.message) return error.message;
  if (typeof error === "object" && error !== null && "message" in error) {
    const message = (error as { message?: unknown }).message;
    if (typeof message === "string" && message.trim()) return message;
  }
  return fallback;
}

function formatDuration(milliseconds: number | null): string {
  if (milliseconds === null || milliseconds < 0) return "时长未知";
  const seconds = Math.floor(milliseconds / 1_000);
  const hours = Math.floor(seconds / 3_600);
  const minutes = Math.floor((seconds % 3_600) / 60);
  const rest = seconds % 60;
  return hours > 0
    ? `${hours}:${String(minutes).padStart(2, "0")}:${String(rest).padStart(2, "0")}`
    : `${minutes}:${String(rest).padStart(2, "0")}`;
}

function formatSessionTime(value: string): string {
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return "时间未知";
  return date.toLocaleString("zh-CN", {
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
    hour12: false,
  });
}

function formatTimestamp(milliseconds: number): string {
  const hours = Math.floor(milliseconds / 3_600_000);
  const minutes = Math.floor((milliseconds % 3_600_000) / 60_000);
  const seconds = Math.floor((milliseconds % 60_000) / 1_000);
  const millis = milliseconds % 1_000;
  return hours > 0
    ? `${String(hours).padStart(2, "0")}:${String(minutes).padStart(2, "0")}:${String(seconds).padStart(2, "0")}.${String(millis).padStart(3, "0")}`
    : `${String(minutes).padStart(2, "0")}:${String(seconds).padStart(2, "0")}.${String(millis).padStart(3, "0")}`;
}

function mediaUrl(path: string): string {
  return "__TAURI_INTERNALS__" in window ? convertFileSrc(path) : path;
}

function currentSegmentAt(
  segments: AiTranscriptSegment[],
  currentTimeMs: number,
): AiTranscriptSegment | null {
  let low = 0;
  let high = segments.length - 1;
  let candidate: AiTranscriptSegment | null = null;
  while (low <= high) {
    const middle = Math.floor((low + high) / 2);
    const segment = segments[middle];
    if (segment.sourceStartMs <= currentTimeMs) {
      candidate = segment;
      low = middle + 1;
    } else {
      high = middle - 1;
    }
  }
  return candidate && currentTimeMs < candidate.sourceEndMs ? candidate : null;
}

async function copyText(text: string): Promise<void> {
  if (navigator.clipboard?.writeText) {
    await navigator.clipboard.writeText(text);
    return;
  }
  const textarea = document.createElement("textarea");
  textarea.value = text;
  textarea.style.position = "fixed";
  textarea.style.opacity = "0";
  document.body.appendChild(textarea);
  textarea.select();
  document.execCommand("copy");
  textarea.remove();
}

export function AiWorkspace({ api }: { api: ClientApi }) {
  const [projects, setProjects] = useState<AiProject[]>([]);
  const [selectedProjectId, setSelectedProjectId] = useState<number | null>(null);
  const [detail, setDetail] = useState<AiProjectDetail | null>(null);
  const [environment, setEnvironment] = useState<AiEnvironmentDiagnostic | null>(null);
  const [sessions, setSessions] = useState<AiSessionOption[]>([]);
  const [selectedSessionId, setSelectedSessionId] = useState("");
  const [transcript, setTranscript] = useState<AiTranscriptProjection | null>(null);
  const [currentInputId, setCurrentInputId] = useState<number | null>(null);
  const [preview, setPreview] = useState<PreviewSnapshot | null>(null);
  const [currentSegmentId, setCurrentSegmentId] = useState<string | null>(null);
  const [followPlayback, setFollowPlayback] = useState(true);
  const [showSubtitles, setShowSubtitles] = useState(true);
  const [portraitVideo, setPortraitVideo] = useState(false);
  const [segmentPage, setSegmentPage] = useState(0);
  const [createOpen, setCreateOpen] = useState(false);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState<string | null>(null);
  const [latestEvent, setLatestEvent] = useState<AiJobEvent | null>(null);
  const [llmSettings, setLlmSettings] = useState<LlmProviderSettings | null>(null);
  const [analysisRun, setAnalysisRun] = useState<AiHighlightRun | null>(null);
  const [highlightCandidates, setHighlightCandidates] = useState<AiHighlightCandidate[]>([]);
  const [analysisBusy, setAnalysisBusy] = useState(false);
  const [projectTags, setProjectTags] = useState("");
  const [analysisGoal, setAnalysisGoal] = useState("");
  const videoRef = useRef<HTMLVideoElement | null>(null);
  const selectedProjectRef = useRef<number | null>(null);
  const currentSegmentRef = useRef<string | null>(null);
  const refreshTimerRef = useRef<number | null>(null);
  selectedProjectRef.current = selectedProjectId;
  currentSegmentRef.current = currentSegmentId;

  const loadProject = useCallback(async (projectId: number) => {
    const [nextDetail, nextTranscript] = await Promise.all([
      api.getAiProject(projectId),
      api.queryAiTranscript(projectId),
    ]);
    setDetail(nextDetail);
    setTranscript(nextTranscript);
    setCurrentInputId((current) => {
      if (current && nextDetail.inputs.some((input) => input.id === current)) return current;
      return nextDetail.inputs[0]?.id ?? null;
    });
    setProjects((current) => current.map((project) =>
      project.id === nextDetail.project.id ? nextDetail.project : project));
  }, [api]);

  const refreshProjects = useCallback(async (preferredProjectId?: number | null) => {
    const nextProjects = await api.listAiProjects();
    setProjects(nextProjects);
    const nextId = preferredProjectId
      ?? selectedProjectRef.current
      ?? nextProjects[0]?.id
      ?? null;
    const validId = nextProjects.some((project) => project.id === nextId)
      ? nextId
      : nextProjects[0]?.id ?? null;
    setSelectedProjectId(validId);
    if (validId !== null) await loadProject(validId);
    else {
      setDetail(null);
      setTranscript(null);
      setCurrentInputId(null);
    }
  }, [api, loadProject]);

  const refreshEnvironment = useCallback(async () => {
    setEnvironment(await api.diagnoseAiEnvironment());
  }, [api]);

  useEffect(() => {
    let disposed = false;
    void Promise.all([
      refreshProjects(),
      refreshEnvironment(),
      api.listAiCompletedSessions().then(setSessions),
    ])
      .catch((error) => !disposed && setMessage(safeError(error, "无法打开 AI 工作区")))
      .finally(() => !disposed && setLoading(false));
    return () => {
      disposed = true;
    };
  }, [api, refreshEnvironment, refreshProjects]);

  useEffect(() => {
    if (!api.getAiLlmSettings) return;
    void api.getAiLlmSettings().then(setLlmSettings).catch(() => setLlmSettings(null));
  }, [api]);

  useEffect(() => {
    let disposed = false;
    let unsubscribe: (() => void) | undefined;
    void api.subscribeAi((event) => {
      setLatestEvent(event);
      if (refreshTimerRef.current !== null) window.clearTimeout(refreshTimerRef.current);
      refreshTimerRef.current = window.setTimeout(() => {
        void refreshProjects(event.projectId).catch((error) => {
          setMessage(safeError(error, "无法恢复最新 AI 状态"));
        });
      }, 120);
    }).then((handler) => {
      if (disposed) handler();
      else unsubscribe = handler;
    });
    return () => {
      disposed = true;
      unsubscribe?.();
      if (refreshTimerRef.current !== null) window.clearTimeout(refreshTimerRef.current);
    };
  }, [api, refreshProjects]);

  useEffect(() => {
    let disposed = false;
    let unsubscribe: (() => void) | undefined;
    void api.subscribePreview((snapshot) => {
      setPreview((current) => current?.requestId === snapshot.requestId ? snapshot : current);
    }).then((handler) => {
      if (disposed) handler();
      else unsubscribe = handler;
    });
    return () => {
      disposed = true;
      unsubscribe?.();
    };
  }, [api]);

  const currentInput = detail?.inputs.find((input) => input.id === currentInputId) ?? null;
  const selectedSession = sessions.find(
    (session) => String(session.sessionId) === selectedSessionId,
  ) ?? null;
  const currentTranscript = transcript?.inputs.find((input) => input.inputId === currentInputId) ?? null;
  const segments = currentTranscript?.segments ?? [];
  const currentSegment = segments.find((segment) => segment.stableSegmentId === currentSegmentId) ?? null;

  useEffect(() => {
    setCurrentSegmentId(null);
    setShowSubtitles(true);
    setPortraitVideo(false);
    setSegmentPage(0);
    setPreview(null);
    if (!currentInput || !["completed", "skipped"].includes(currentInput.status)) return;
    let disposed = false;
    void api.requestAiInputPreview(currentInput.projectId, currentInput.id)
      .then((snapshot) => !disposed && setPreview(snapshot))
      .catch((error) => !disposed && setPreview({
        requestId: `ai-preview-failed-${currentInput.id}`,
        videoId: -currentInput.id,
        state: "failed",
        progressPercent: null,
        message: "预览不可用",
        media: null,
        errorCode: "preview_unavailable",
        errorMessage: safeError(error, "无法准备视频预览，仍可浏览转写文本"),
      }));
    return () => {
      disposed = true;
    };
  }, [api, currentInput?.id, currentInput?.projectId, currentInput?.status]);

  useEffect(() => {
    if (preview?.state !== "ready") return;
    void api.retainVideoPreview(preview.requestId);
    return () => {
      void api.releaseVideoPreview(preview.requestId);
    };
  }, [api, preview?.requestId, preview?.state]);

  useEffect(() => {
    const video = videoRef.current;
    if (!video) return;
    const updateOrientation = () => {
      if (video.videoWidth > 0 && video.videoHeight > 0) {
        setPortraitVideo(video.videoHeight > video.videoWidth);
      }
    };
    updateOrientation();
    video.addEventListener("loadedmetadata", updateOrientation);
    return () => video.removeEventListener("loadedmetadata", updateOrientation);
  }, [preview?.media?.path]);

  useEffect(() => {
    if (!followPlayback || !currentSegmentId) return;
    const index = segments.findIndex((segment) => segment.stableSegmentId === currentSegmentId);
    if (index >= 0) setSegmentPage(Math.floor(index / segmentPageSize));
    const row = document.getElementById(`ai-segment-${currentSegmentId}`);
    if (row && "scrollIntoView" in row) row.scrollIntoView({ block: "nearest" });
  }, [currentSegmentId, followPlayback, segments]);

  const run = async (operation: () => Promise<void>, success?: string) => {
    setBusy(true);
    try {
      await operation();
      if (success) setMessage(success);
    } catch (error) {
      setMessage(safeError(error, "AI 项目操作失败"));
    } finally {
      setBusy(false);
    }
  };

  const selectProject = (projectId: number) => {
    setSelectedProjectId(projectId);
    setMessage(null);
    void loadProject(projectId).catch((error) => setMessage(safeError(error, "读取项目失败")));
  };

  const addLocalVideos = () => run(async () => {
    if (!detail) return;
    const grants = await api.pickAiLocalVideos();
    if (grants.length === 0) return;
    const result = await api.importAiLocalGrants(
      detail.project.id,
      grants.map((grant) => grant.grantId),
    );
    await loadProject(detail.project.id);
    if (result.rejected.length > 0) {
      setMessage(`${result.added.length} 个视频已添加，${result.rejected.length} 个被拒绝：${result.rejected[0].message}`);
    } else {
      setMessage(`已添加 ${result.added.length} 个本地视频`);
    }
  });

  const addSession = () => run(async () => {
    if (!detail || !selectedSessionId) return;
    const result = await api.addAiCompletedSession(detail.project.id, Number(selectedSessionId));
    setDetail(result.detail);
    setProjects((current) => current.map((project) =>
      project.id === result.detail.project.id ? result.detail.project : project));
    setMessage(
      `新增 ${result.addedCount} 个分片，跳过 ${result.duplicateCount} 个重复，${result.unavailableCount} 个不可用`,
    );
  });

  const reorder = (inputId: number, direction: -1 | 1) => run(async () => {
    if (!detail) return;
    const index = detail.inputs.findIndex((input) => input.id === inputId);
    const target = index + direction;
    if (index < 0 || target < 0 || target >= detail.inputs.length) return;
    const ordered = detail.inputs.map((input) => input.id);
    [ordered[index], ordered[target]] = [ordered[target], ordered[index]];
    setDetail(await api.reorderAiInputs(detail.project.id, ordered));
  });

  const dropInput = (draggedId: number, targetId: number) => run(async () => {
    if (!detail || draggedId === targetId) return;
    const ordered = detail.inputs.map((input) => input.id);
    const from = ordered.indexOf(draggedId);
    const to = ordered.indexOf(targetId);
    if (from < 0 || to < 0) return;
    ordered.splice(to, 0, ordered.splice(from, 1)[0]);
    setDetail(await api.reorderAiInputs(detail.project.id, ordered));
  });

  const removeInput = (input: AiProjectInput) => run(async () => {
    if (!detail) return;
    setDetail(await api.removeAiInput(detail.project.id, input.id));
  }, "已从 AI 项目移除视频，原视频保持不变");

  const startProject = () => run(async () => {
    if (!detail) return;
    await api.startAiProject(detail.project.id);
    await refreshProjects(detail.project.id);
  }, "项目已加入本地识别队列");

  const cancelProject = () => {
    if (!detail || !window.confirm("取消后会停止当前本地识别并保留已完整发布的结果，确认继续？")) return;
    void run(async () => {
      await api.cancelAiProject(detail.project.id);
      await refreshProjects(detail.project.id);
    }, "AI 项目已取消，原视频未被修改");
  };

  const deleteProject = () => {
    if (!detail || !window.confirm("只删除 AI 项目、转写和关联数据，绝不会删除原始视频。确认继续？")) return;
    void run(async () => {
      await api.deleteAiProject(detail.project.id);
      await refreshProjects(null);
    }, "AI 项目已删除，原视频仍然保留");
  };

  const retryInput = (inputId: number) => run(async () => {
    await api.retryAiInput(inputId);
    if (detail) await loadProject(detail.project.id);
  }, "失败输入已重新加入队列");

  const promoteInput = (inputId: number) => {
    if (!api.promoteAiNextInput) return;
    void run(async () => {
      const next = await api.promoteAiNextInput!(inputId);
      setDetail(next);
    }, "已设为下一个处理");
  };

  const preemptInput = (inputId: number) => {
    if (!api.preemptAiWithInput || !window.confirm("立即切换会取消当前识别并将其以新代次重新排队，确认继续？")) return;
    void run(async () => {
      const next = await api.preemptAiWithInput!(inputId, true);
      setDetail(next);
    }, "已请求立即切换，当前任务会在释放执行许可后重新排队");
  };

  const startHighlightAnalysis = async () => {
    if (!detail || !api.startAiHighlightAnalysis) return;
    if (!llmSettings?.keyConfigured) {
      setMessage("请先在设置中配置 DeepSeek API Key");
      return;
    }
    if (!window.confirm("将只发送规范化转写、稳定句段时间和标签到 DeepSeek，确认开始高光分析？")) return;
    setAnalysisBusy(true);
    try {
      const run = await api.startAiHighlightAnalysis(detail.project.id, true);
      setAnalysisRun(run);
      if (api.listAiHighlightCandidates) setHighlightCandidates(await api.listAiHighlightCandidates(run.id));
      setMessage("高光分析已完成，候选结果可人工选择");
    } catch (error) {
      setMessage(safeError(error, "高光分析失败，ASR 结果已保留"));
    } finally {
      setAnalysisBusy(false);
    }
  };

  const saveHighlightSelection = async () => {
    if (!analysisRun || !api.selectAiHighlightCandidates) return;
    const selected = highlightCandidates.filter((candidate) => candidate.selected).map((candidate) => candidate.id);
    try {
      setHighlightCandidates(await api.selectAiHighlightCandidates(analysisRun.id, selected));
      setMessage("高光候选选择已保存，当前版本不会生成视频文件");
    } catch (error) {
      setMessage(safeError(error, "保存高光选择失败"));
    }
  };

  const seekSegment = (segment: AiTranscriptSegment) => {
    if (preview?.state !== "ready" || !preview.media || !videoRef.current) return;
    videoRef.current.currentTime = segment.sourceStartMs / 1_000;
    setCurrentSegmentId(segment.stableSegmentId);
  };

  const updateCurrentSegment = () => {
    const video = videoRef.current;
    if (!video) return;
    const segment = currentSegmentAt(segments, Math.round(video.currentTime * 1_000));
    const nextId = segment?.stableSegmentId ?? null;
    if (nextId !== currentSegmentRef.current) setCurrentSegmentId(nextId);
  };

  const copy = (loader: () => Promise<string>, success: string) => run(async () => {
    await copyText(await loader());
  }, success);

  const draft = detail?.project.status === "draft";
  const running = detail && ["queued", "running"].includes(detail.project.status);
  const canStart = Boolean(
    draft
      && environment?.ready
      && detail?.inputs.some((input) => input.status === "pending" && input.audioPresent === true),
  );
  const totalDuration = detail?.inputs.reduce((total, input) => total + (input.durationMs ?? 0), 0) ?? 0;
  const unavailableCount = detail?.inputs.filter((input) => input.status === "failed" || input.audioPresent === false).length ?? 0;
  const pageCount = Math.max(1, Math.ceil(segments.length / segmentPageSize));
  const safePage = Math.min(segmentPage, pageCount - 1);
  const visibleSegments = segments.slice(
    safePage * segmentPageSize,
    (safePage + 1) * segmentPageSize,
  );
  const previewReady = preview?.state === "ready" && Boolean(preview.media);

  useEffect(() => {
    setProjectTags(detail?.project.projectTags?.join("、") ?? "");
    setAnalysisGoal(detail?.project.analysisGoal ?? "");
  }, [detail?.project.id, detail?.project.projectTags, detail?.project.analysisGoal]);

  const saveProjectContext = () => {
    if (!detail || !api.setAiProjectContext) return;
    void api.setAiProjectContext(
      detail.project.id,
      projectTags.split(/[、,，\n]/).map((tag) => tag.trim()).filter(Boolean),
      analysisGoal.trim() || null,
    ).then((project) => setDetail((current) => current ? { ...current, project } : current))
      .catch((error) => setMessage(safeError(error, "保存项目标签失败")));
  };

  if (loading) {
    return <div className="page-content ai-page"><div className="ai-loading"><LoaderCircle className="spin" />正在恢复本地 AI 项目状态</div></div>;
  }

  return (
    <div className="page-content ai-page ai-workspace">
      {message && <div className="ai-message" role="status"><span>{message}</span><button aria-label="关闭 AI 提示" onClick={() => setMessage(null)}><X size={15} /></button></div>}

      <section className={`ai-environment ${environment?.ready ? "ready" : "unavailable"}`}>
        <div className="ai-environment-icon">{environment?.ready ? <CheckCircle2 size={22} /> : <AlertTriangle size={22} />}</div>
        <div>
          <strong>{environment?.ready ? "视频和转写全程保留在本机" : "本地 ASR 环境暂不可用"}</strong>
          <p>{environment?.message ?? "正在检测随包本地识别资源"}</p>
          {environment && <div className="ai-environment-details"><span>{environment.platform}</span><span>{environment.engineId} {environment.engineVersion}</span><span>{environment.modelId}</span>{environment.checks.map((check) => <span key={check.code} className={check.passed ? "ok" : "failed"} title={check.message}>{check.passed ? "✓" : "!"} {check.message}</span>)}</div>}
          {!environment?.ready && <small><span>修复方式：重新安装当前版本应用</span><span>第一版不下载模型，也不会自动改用云端。</span></small>}
        </div>
        <button className="secondary-button" disabled={busy} onClick={() => void run(refreshEnvironment)}><RefreshCw size={15} />重新检测</button>
      </section>

      {projects.length === 0 ? (
        <section className="ai-empty panel">
          <div className="ai-empty-icon"><Sparkles size={34} /></div>
          <h2>还没有 AI 分析项目</h2>
          <p>由你主动创建项目后，再添加多个本地视频或选择一场已结束的直播。打开页面本身不会读取视频或启动 ASR。</p>
          <button className="primary-button" onClick={() => setCreateOpen(true)}><Plus size={17} />创建项目</button>
        </section>
      ) : (
        <div className="ai-shell-grid">
          <aside className="panel ai-project-sidebar">
            <header><div><p className="section-kicker">PROJECTS</p><h2>分析项目</h2></div><button className="icon-button" aria-label="创建项目" onClick={() => setCreateOpen(true)}><Plus size={17} /></button></header>
            <div className="ai-project-list">
              {projects.map((project) => (
                <button key={project.id} className={project.id === selectedProjectId ? "active" : ""} onClick={() => selectProject(project.id)}>
                  <span className={`ai-status-dot ${project.status}`} />
                  <div><strong>{project.name}</strong><small>{projectStatusLabels[project.status]} · {project.progressPercent}%</small></div>
                  <ChevronRight size={15} />
                </button>
              ))}
            </div>
          </aside>

          {detail && (
            <main className="ai-project-main">
              <section className="panel ai-project-header">
                <div className="ai-project-title">
                  <div><p className="section-kicker">LOCAL ASR PROJECT</p><h2>{detail.project.name}</h2><span>{projectStatusLabels[detail.project.status]} · {latestEvent?.projectId === detail.project.id ? latestEvent.message : `${detail.project.progressPercent}%`}</span></div>
                  <div className="ai-project-actions">
                    {running && <button className="secondary-button danger" disabled={busy} onClick={cancelProject}>取消分析</button>}
                    <button className="icon-button danger" aria-label="删除 AI 项目" disabled={busy} onClick={deleteProject}><Trash2 size={16} /></button>
                  </div>
                </div>
                <div className="ai-progress-track" aria-label={`项目进度 ${detail.project.progressPercent}%`}><span style={{ width: `${detail.project.progressPercent}%` }} /></div>
                <div className="ai-summary-row">
                  <span><b>{detail.inputs.length}</b> 个视频</span>
                  <span><b>{formatDuration(totalDuration)}</b> 总时长</span>
                  <span><b>{unavailableCount}</b> 个不可用</span>
                  <span><b>{environment?.engineId ?? detail.project.recognitionProfile.engineId}</b> · {environment?.modelId ?? detail.project.recognitionProfile.modelId}</span>
                </div>
              </section>

              {draft && (
                <section className="panel ai-input-builder">
                  <header><div><p className="section-kicker">INPUTS</p><h3>有序视频输入</h3></div><span>识别只在点击开始后执行</span></header>
                  <div className="ai-project-context"><label>项目标签<input aria-label="项目标签" value={projectTags} onChange={(event) => setProjectTags(event.target.value)} onBlur={saveProjectContext} placeholder="例如：带货、搞笑、知识" /></label><label>分析目标<input aria-label="分析目标" value={analysisGoal} onChange={(event) => setAnalysisGoal(event.target.value)} onBlur={saveProjectContext} placeholder="可选：重点找出价格对比和反转" /></label></div>
                  <div className="ai-input-source-actions">
                    <button className="secondary-button" disabled={busy} onClick={addLocalVideos}><FolderPlus size={16} />添加本地视频</button>
                    <label className="ai-session-picker"><span className="sr-only">选择已结束直播</span><select aria-label="选择已结束直播" value={selectedSessionId} onChange={(event) => setSelectedSessionId(event.target.value)}><option value="">选择已结束直播</option>{sessions.map((session) => <option key={session.sessionId} value={session.sessionId}>{session.streamerName} · {formatSessionTime(session.startedAt)}–{formatSessionTime(session.endedAt)} · {formatDuration(session.totalDurationMs)} · {session.videoCount} 段{session.unavailableVideoCount > 0 ? ` · ${session.unavailableVideoCount} 个不可用` : ""}</option>)}</select>{selectedSession && <small aria-label="已选历史直播详情">{formatSessionTime(selectedSession.startedAt)}–{formatSessionTime(selectedSession.endedAt)} · {selectedSession.videoCount} 个分片 · {formatDuration(selectedSession.totalDurationMs)}{selectedSession.unavailableVideoCount > 0 ? ` · ${selectedSession.unavailableVideoCount} 个不可用` : ""}</small>}</label>
                    <button className="secondary-button" disabled={!selectedSessionId || busy} onClick={addSession}><Video size={15} />添加整场直播</button>
                  </div>
                  {detail.inputs.length === 0 ? <div className="ai-input-empty">先添加一个或多个视频；“上传”仅表示本地导入，不会发生网络上传。</div> : (
                    <div className="ai-draft-inputs">
                      {detail.inputs.map((input, index) => (
                        <article key={input.id} draggable onDragStart={(event) => event.dataTransfer.setData("text/plain", String(input.id))} onDragOver={(event) => event.preventDefault()} onDrop={(event) => { event.preventDefault(); void dropInput(Number(event.dataTransfer.getData("text/plain")), input.id); }}>
                          <GripVertical size={16} aria-hidden="true" />
                          <span className="ai-input-index">{index + 1}</span>
                          <div><strong title={input.displayName}>{input.displayName}</strong><small>{input.sourceKind === "video_library" ? "直播录像" : "本地视频"} · {formatDuration(input.durationMs)} · {input.audioPresent === true ? "有音轨" : input.audioPresent === false ? "无音轨" : "音轨未知"}</small>{input.lastErrorMessage && <em>{input.lastErrorMessage}</em>}</div>
                          <div className="ai-input-order"><button aria-label={`上移 ${input.displayName}`} disabled={index === 0 || busy} onClick={() => void reorder(input.id, -1)}><ArrowUp size={14} /></button><button aria-label={`下移 ${input.displayName}`} disabled={index === detail.inputs.length - 1 || busy} onClick={() => void reorder(input.id, 1)}><ArrowDown size={14} /></button><button aria-label={`移除 ${input.displayName}`} disabled={busy} onClick={() => void removeInput(input)}><X size={14} /></button></div>
                        </article>
                      ))}
                    </div>
                  )}
                  <footer><div><strong>{environment?.engineId} {environment?.engineVersion}</strong><small>{environment?.modelId} · 8 GB 内存基线 · 全局单任务 · 可与录制并行</small></div><button className="primary-button" disabled={!canStart || busy} onClick={startProject}><Play size={16} />开始分析</button></footer>
                </section>
              )}

              {!draft && (
                <section className="ai-result-workspace">
                  {detail.project.status === "completed" || detail.project.status === "completed_with_errors" ? (
                    <section className="panel ai-highlight-panel">
                      <header><div><p className="section-kicker">HIGHLIGHT AGENT</p><h3>高光候选</h3></div><span>{llmSettings?.keyConfigured ? `${llmSettings.modelId} · 仅发送文本` : "未配置 DeepSeek"}</span></header>
                      <div className="ai-highlight-consent"><div><strong>独立于本地 ASR</strong><p>需要显式授权后，候选 Agent 和评分 Agent 才会读取规范化转写。视频、音频、本地路径和 Key 不会发送。</p></div><button className="primary-button" disabled={analysisBusy || !llmSettings?.keyConfigured || !api.startAiHighlightAnalysis} onClick={() => void startHighlightAnalysis()}><Sparkles size={15} />{analysisBusy ? "分析中…" : "开始高光分析"}</button></div>
                      {analysisRun && <div className="ai-highlight-run-meta"><span>状态：{analysisRun.status}</span><span>Skills：{analysisRun.skillsSnapshot.join("、") || "通用"}</span><span>Token：{analysisRun.totalTokens}</span></div>}
                      {highlightCandidates.length > 0 ? <><div className="ai-highlight-candidate-list">{highlightCandidates.map((candidate) => <label key={candidate.id} className="ai-highlight-candidate"><input type="checkbox" checked={candidate.selected} onChange={(event) => setHighlightCandidates((items) => items.map((item) => item.id === candidate.id ? { ...item, selected: event.target.checked } : item))} /><div><strong>{candidate.title}</strong><small>{formatTimestamp(candidate.startMs)}–{formatTimestamp(candidate.endMs)} · {candidate.totalScore.toFixed(0)} 分 · {candidate.matchedTags.join("、") || "通用爆点"}</small><p>{candidate.reason}</p></div></label>)}</div><button className="secondary-button" onClick={() => void saveHighlightSelection()}>保存候选选择</button></> : <div className="ai-highlight-empty">完成分析后，这里会列出默认分数不低于 70 的前 10 个候选。</div>}
                    </section>
                  ) : null}
                  <aside className="panel ai-result-inputs">
                    <header><p className="section-kicker">VIDEOS</p><h3>视频与状态</h3></header>
                    <div className="ai-result-input-list">{detail.inputs.map((input, index) => <div key={input.id} className={`ai-result-input-row ${input.id === currentInputId ? "active" : ""}`}><button className="ai-result-input-select" onClick={() => setCurrentInputId(input.id)}><span>{index + 1}</span><div><strong title={input.displayName}>{input.displayName}</strong><small>{inputStatusLabels[input.status]} · {input.progressPercent}%</small>{input.lastErrorMessage && <em title={input.lastErrorMessage}>{input.lastErrorMessage}</em>}</div></button><div className="ai-queue-actions">{input.status === "pending" && <><button aria-label={`下一个处理 ${input.displayName}`} onClick={() => promoteInput(input.id)}>下一个</button><button aria-label={`立即切换 ${input.displayName}`} onClick={() => preemptInput(input.id)}>立即切换</button></>}{input.status === "failed" && <button className="ai-retry" aria-label={`重试 ${input.displayName}`} onClick={() => void retryInput(input.id)}><RotateCcw size={14} /></button>}</div></div>)}</div>
                  </aside>

                  <div className="ai-result-main">
                    <section className="panel ai-player-panel">
                      <header><div><p className="section-kicker">PLAYER</p><h3>{currentInput?.displayName ?? "选择视频"}</h3></div><div className="ai-player-toggles"><label><input type="checkbox" checked={followPlayback} onChange={(event) => setFollowPlayback(event.target.checked)} />跟随播放</label><label><input type="checkbox" checked={showSubtitles} disabled={!previewReady} onChange={(event) => setShowSubtitles(event.target.checked)} />显示字幕</label></div></header>
                      <div className={`ai-player-stage ${portraitVideo ? "portrait" : ""}`}>
                        {previewReady && preview?.media ? <><video ref={videoRef} controls aria-label="AI 视频播放器" src={mediaUrl(preview.media.path)} onTimeUpdate={updateCurrentSegment} /><div className={`ai-subtitle-overlay ${showSubtitles && currentSegment ? "visible" : ""}`} aria-live="polite">{showSubtitles ? currentSegment?.normalizedText : ""}</div></> : preview?.state === "failed" ? <div className="ai-player-unavailable"><AlertTriangle size={26} /><strong>播放器联动不可用</strong><p>{preview.errorMessage}</p><button className="secondary-button" onClick={() => currentInput && void api.retryAiInputPreview(currentInput.projectId, currentInput.id).then(setPreview)}><RotateCcw size={14} />重试预览</button></div> : currentInput && ["completed", "skipped"].includes(currentInput.status) ? <div className="ai-player-unavailable"><LoaderCircle className="spin" size={25} /><strong>正在准备本地预览</strong><p>预览只服务界面播放，不参与 ASR 缓存指纹。</p></div> : <div className="ai-player-unavailable"><Video size={28} /><strong>等待当前视频处理完成</strong><p>转写和预览都不会修改原视频。</p></div>}
                      </div>
                    </section>

                    <section className="panel ai-transcript-panel">
                      <header><div><p className="section-kicker">TIMESTAMPED TEXT</p><h3>只读时间戳文本</h3></div><div className="ai-result-actions"><button disabled={!currentInput || segments.length === 0} onClick={() => currentInput && void copy(() => api.copyAiInputText(detail.project.id, currentInput.id), "已复制当前视频全文")}><Clipboard size={14} />复制当前视频</button><button disabled={!transcript} onClick={() => void copy(() => api.copyAiProjectText(detail.project.id), "已复制项目全文")}><Clipboard size={14} />复制项目</button><button onClick={() => void run(async () => { const result = await api.exportAiTxt(detail.project.id); if (result.saved) setMessage("TXT 已导出"); })}><FileText size={14} />TXT</button><button onClick={() => void run(async () => { const result = await api.exportAiJson(detail.project.id); if (result.saved) setMessage("JSON 已导出"); })}><FileJson size={14} />JSON</button></div></header>
                      {segments.length === 0 ? <div className="ai-transcript-empty">{currentTranscript?.errorMessage ?? "当前视频还没有可用转写文本"}</div> : <><div className="ai-segment-list" aria-label="转写句段列表">{visibleSegments.map((segment) => <div id={`ai-segment-${segment.stableSegmentId}`} key={segment.stableSegmentId} className={`ai-segment-row ${segment.stableSegmentId === currentSegmentId ? "active" : ""}`}><button className="ai-segment-main" disabled={!previewReady} onClick={() => seekSegment(segment)}><time>{formatTimestamp(segment.sourceStartMs)}<span>– {formatTimestamp(segment.sourceEndMs)}</span></time><p>{segment.normalizedText}</p>{segment.confidence !== null && segment.confidence < 0.55 && <em>低置信</em>}</button><button className="ai-segment-copy" aria-label={`复制句段 ${formatTimestamp(segment.sourceStartMs)}`} onClick={() => void copy(() => api.copyAiSegmentText(detail.project.id, segment.stableSegmentId), "已复制句段")}><Clipboard size={13} /></button></div>)}</div>{pageCount > 1 && <footer className="ai-segment-pagination"><button disabled={safePage === 0} onClick={() => setSegmentPage((page) => Math.max(0, page - 1))}><ChevronLeft size={14} />上一页</button><span>{safePage + 1} / {pageCount} · 共 {segments.length} 句</span><button disabled={safePage >= pageCount - 1} onClick={() => setSegmentPage((page) => Math.min(pageCount - 1, page + 1))}>下一页<ChevronRight size={14} /></button></footer>}</>}
                    </section>
                  </div>
                </section>
              )}
            </main>
          )}
        </div>
      )}

      {createOpen && <CreateProjectDialog api={api} onClose={() => setCreateOpen(false)} onCreated={(project) => { setCreateOpen(false); void refreshProjects(project.id); }} />}
      <div className="ai-scope-note"><Download size={16} /><span>第一版只输出带稳定句段 ID 的文本结果；不会生成字幕文件、修改视频或渲染成品。</span></div>
    </div>
  );
}

function CreateProjectDialog({
  api,
  onClose,
  onCreated,
}: {
  api: ClientApi;
  onClose: () => void;
  onCreated: (project: AiProject) => void;
}) {
  const [name, setName] = useState("");
  const [hotwords, setHotwords] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState(false);
  const submit = async (event: FormEvent) => {
    event.preventDefault();
    if (!name.trim()) {
      setError("请填写项目名称");
      return;
    }
    setSubmitting(true);
    try {
      const project = await api.createAiProject({
        name: name.trim(),
        hotwords: hotwords.split(/[,，\n]/).map((word) => word.trim()).filter(Boolean),
      });
      onCreated(project);
    } catch (reason) {
      setError(safeError(reason, "创建 AI 项目失败"));
    } finally {
      setSubmitting(false);
    }
  };
  return (
    <div className="modal-backdrop" role="presentation">
      <form className="modal ai-create-modal" onSubmit={submit}>
        <header className="modal-header"><div className="modal-title-icon"><Sparkles size={20} /></div><div><h2>创建 AI 分析项目</h2><p>项目只在本机组织视频与转写</p></div><button type="button" className="icon-button" aria-label="关闭创建项目" onClick={onClose}><X size={17} /></button></header>
        <div className="modal-body"><label>项目名称<input aria-label="项目名称" autoFocus value={name} onChange={(event) => setName(event.target.value)} placeholder="例如：7 月 22 日整场直播" /></label><label>识别热词<input aria-label="识别热词" value={hotwords} onChange={(event) => setHotwords(event.target.value)} placeholder="主播名, 品牌名, 商品名" /><small>逗号分隔，将进入本次识别配置指纹。</small></label>{error && <div className="form-error">{error}</div>}</div>
        <footer className="modal-footer"><button type="button" className="secondary-button" onClick={onClose}>取消</button><button className="primary-button" disabled={submitting} type="submit">{submitting && <LoaderCircle className="spin" size={15} />}创建草稿</button></footer>
      </form>
    </div>
  );
}
