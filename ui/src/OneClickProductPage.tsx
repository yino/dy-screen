import { convertFileSrc } from "@tauri-apps/api/core";
import {
  AlertTriangle,
  Check,
  CheckCircle2,
  Download,
  FileVideo2,
  FolderOpen,
  LoaderCircle,
  Pencil,
  Play,
  Radio,
  RefreshCw,
  RotateCcw,
  ShieldCheck,
  Sparkles,
  Square,
  Video,
  X,
} from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { ClipEditor } from "./AiWorkspace";
import { SearchableCombobox, type SearchableComboboxOption } from "./SearchableCombobox";
import { recoverSmartWorkflows, type SmartWorkflowSnapshot } from "./smartWorkflowRecovery";
import type {
  AiActiveLiveSession,
  AiClipProjectDetail,
  AiEnvironmentDiagnostic,
  AiReplayStreamerCursor,
  AiReplayStreamerOption,
  AiSmartReplaySession,
  AiSmartReplaySessionCursor,
  AiSmartStage,
  AiSmartWorkflowDetail,
  AiSmartWorkflowMode,
  ClientApi,
  LlmProviderSettings,
  PreviewSnapshot,
} from "./types";

const productStages = [
  { key: "preflight", label: "准备素材" },
  { key: "asr", label: "识别字幕" },
  { key: "highlight", label: "提取精彩" },
  { key: "draft", label: "生成合辑" },
  { key: "correction", label: "校正文案" },
  { key: "transition", label: "匹配包装" },
] as const satisfies ReadonlyArray<{ key: Exclude<AiSmartStage, "review">; label: string }>;

const workflowStatusLabels: Record<AiSmartWorkflowDetail["workflow"]["status"], string> = {
  draft: "待授权",
  queued: "排队中",
  running: "处理中",
  awaiting_selection: "等待选择精彩",
  review_ready: "可预览",
  paused: "已暂停",
  completed: "已完成",
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

function formatDuration(milliseconds: number): string {
  const totalSeconds = Math.max(0, Math.round(milliseconds / 1_000));
  const hours = Math.floor(totalSeconds / 3_600);
  const minutes = Math.floor((totalSeconds % 3_600) / 60);
  const seconds = totalSeconds % 60;
  return hours > 0
    ? `${String(hours).padStart(2, "0")}:${String(minutes).padStart(2, "0")}:${String(seconds).padStart(2, "0")}`
    : `${String(minutes).padStart(2, "0")}:${String(seconds).padStart(2, "0")}`;
}

function mediaUrl(path: string): string {
  return "__TAURI_INTERNALS__" in window ? convertFileSrc(path) : path;
}

function latestAttempt(detail: AiSmartWorkflowDetail, stage: AiSmartStage) {
  return [...detail.attempts]
    .filter((attempt) => attempt.stage === stage)
    .sort((left, right) => right.updatedAt.localeCompare(left.updatedAt) || right.id - left.id)[0] ?? null;
}

function workflowProgress(detail: AiSmartWorkflowDetail | null): number {
  if (!detail) return 0;
  if (detail.workflow.stage === "review" || ["review_ready", "completed"].includes(detail.workflow.status)) return 100;
  const index = productStages.findIndex((stage) => stage.key === detail.workflow.stage);
  if (index < 0) return 0;
  const attempt = latestAttempt(detail, detail.workflow.stage);
  const stageProgress = attempt?.progress ?? (detail.workflow.status === "queued" ? 0 : 12);
  return Math.min(99, Math.round(((index + stageProgress / 100) / productStages.length) * 100));
}

const activeStageTitles: Record<(typeof productStages)[number]["key"], string> = {
  preflight: "正在准备素材",
  asr: "正在识别字幕",
  highlight: "正在提取精彩",
  draft: "正在生成合辑",
  correction: "正在校正文案",
  transition: "正在匹配包装",
};

function workflowProgressTitle(detail: AiSmartWorkflowDetail | null): string {
  if (!detail) return "等待创建成品";
  if (detail.workflow.status === "awaiting_selection") return "等待确认精彩";
  if (detail.workflow.status === "review_ready") return "成品已准备就绪";
  if (detail.workflow.status === "completed") return "成品已完成";
  if (detail.workflow.status === "cancelled") return "生成已停止";
  if (detail.workflow.status === "draft") return "等待任务授权";
  const stage = productStages.find((item) => item.key === detail.workflow.stage);
  if (detail.workflow.status === "failed") return `${stage?.label ?? "当前阶段"}处理失败`;
  if (detail.workflow.status === "paused") return `${stage?.label ?? "当前阶段"}已暂停`;
  return stage ? activeStageTitles[stage.key] : "正在准备预览";
}

function workflowProgressTone(detail: AiSmartWorkflowDetail | null): "idle" | "active" | "warning" | "failed" | "complete" {
  if (!detail) return "idle";
  if (["review_ready", "completed"].includes(detail.workflow.status)) return "complete";
  if (["failed", "cancelled"].includes(detail.workflow.status)) return "failed";
  if (["awaiting_selection", "paused"].includes(detail.workflow.status)) return "warning";
  return ["queued", "running"].includes(detail.workflow.status) ? "active" : "idle";
}

function stageState(
  detail: AiSmartWorkflowDetail | null,
  stage: typeof productStages[number]["key"],
): "pending" | "working" | "completed" | "failed" | "blocked" {
  if (!detail) return "pending";
  const stageIndex = productStages.findIndex((item) => item.key === stage);
  const currentIndex = detail.workflow.stage === "review"
    ? productStages.length
    : productStages.findIndex((item) => item.key === detail.workflow.stage);
  if (stageIndex < currentIndex || detail.workflow.stage === "review") return "completed";
  if (stageIndex > currentIndex) return "pending";
  if (detail.workflow.status === "failed" || detail.workflow.status === "paused") return "failed";
  if (detail.workflow.status === "awaiting_selection") return "blocked";
  const attempt = latestAttempt(detail, stage);
  if (attempt?.status === "completed") return "completed";
  return "working";
}

function projectIdForDraft(detail: AiSmartWorkflowDetail, draft: AiClipProjectDetail): number {
  return detail.batches.find((batch) => batch.highlightRunId === draft.project.highlightRunId)?.projectId
    ?? detail.batches.find((batch) => batch.projectId !== null)?.projectId
    ?? 0;
}

interface OneClickProductPageProps {
  api: ClientApi;
}

export function OneClickProductPage({ api }: OneClickProductPageProps) {
  const [mode, setMode] = useState<AiSmartWorkflowMode>("local");
  const [grants, setGrants] = useState<Array<{ grantId: string; displayName: string }>>([]);
  const [liveSessions, setLiveSessions] = useState<AiActiveLiveSession[]>([]);
  const [liveSessionId, setLiveSessionId] = useState<number | null>(null);
  const [replayStreamers, setReplayStreamers] = useState<AiReplayStreamerOption[]>([]);
  const [selectedReplayStreamer, setSelectedReplayStreamer] = useState<AiReplayStreamerOption | null>(null);
  const [replayStreamerSearch, setReplayStreamerSearch] = useState("");
  const [replayStreamerCursor, setReplayStreamerCursor] = useState<AiReplayStreamerCursor | null>(null);
  const [replayStreamerLoading, setReplayStreamerLoading] = useState(false);
  const [replayStreamerLoadingMore, setReplayStreamerLoadingMore] = useState(false);
  const [replayStreamerError, setReplayStreamerError] = useState<string | null>(null);
  const [replaySessions, setReplaySessions] = useState<AiSmartReplaySession[]>([]);
  const [replaySessionId, setReplaySessionId] = useState<number | null>(null);
  const [replaySessionSearch, setReplaySessionSearch] = useState("");
  const [replayCursor, setReplayCursor] = useState<AiSmartReplaySessionCursor | null>(null);
  const [replayLoading, setReplayLoading] = useState(false);
  const [replayLoadingMore, setReplayLoadingMore] = useState(false);
  const [replayError, setReplayError] = useState<string | null>(null);
  const [sourceLoading, setSourceLoading] = useState(false);
  const [sourceError, setSourceError] = useState<string | null>(null);
  const [authorized, setAuthorized] = useState(false);
  const [duplicateConfirmed, setDuplicateConfirmed] = useState(false);
  const [environment, setEnvironment] = useState<AiEnvironmentDiagnostic | null>(null);
  const [llmSettings, setLlmSettings] = useState<LlmProviderSettings | null>(null);
  const [snapshot, setSnapshot] = useState<SmartWorkflowSnapshot>({ workflows: [], details: new Map() });
  const [selectedWorkflowId, setSelectedWorkflowId] = useState<number | null>(null);
  const [busy, setBusy] = useState(false);
  const [highlightDecisionBusy, setHighlightDecisionBusy] = useState<"continue" | "stop" | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [message, setMessage] = useState<string | null>(null);
  const [draftDetail, setDraftDetail] = useState<AiClipProjectDetail | null>(null);
  const [draftProjectId, setDraftProjectId] = useState(0);
  const [draftLoading, setDraftLoading] = useState(false);
  const [preview, setPreview] = useState<PreviewSnapshot | null>(null);
  const [editor, setEditor] = useState<AiClipProjectDetail | null>(null);
  const replayStreamerRequestRef = useRef(0);
  const replaySessionRequestRef = useRef(0);
  const draftRequestRef = useRef(0);
  const progressTrackRef = useRef<HTMLOListElement>(null);

  useEffect(() => {
    let disposed = false;
    void api.diagnoseAiEnvironment()
      .then((next) => { if (!disposed) setEnvironment(next); })
      .catch((failure) => { if (!disposed) setError(safeError(failure, "无法检测本地识别资源")); });
    if (api.getAiLlmSettings) {
      void api.getAiLlmSettings()
        .then((next) => { if (!disposed) setLlmSettings(next); })
        .catch(() => { if (!disposed) setLlmSettings(null); });
    }
    return () => { disposed = true; };
  }, [api]);

  useEffect(() => {
    let disposed = false;
    let recovery: { dispose(): void } | undefined;
    void recoverSmartWorkflows(api, (next) => {
      if (disposed) return;
      setSnapshot(next);
      setSelectedWorkflowId((current) => current !== null && next.details.has(current)
        ? current
        : next.workflows[0]?.id ?? null);
    })
      .then((value) => {
        if (disposed) value.dispose();
        else recovery = value;
      })
      .catch((failure) => { if (!disposed) setError(safeError(failure, "无法恢复一键成品任务")); });
    return () => {
      disposed = true;
      recovery?.dispose();
    };
  }, [api]);

  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;
    void api.subscribePreview((next) => {
      if (disposed) return;
      setPreview((current) => current?.requestId === next.requestId ? next : current);
    }).then((handler) => {
      if (disposed) handler();
      else unlisten = handler;
    });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [api]);

  const loadLiveSessions = useCallback(async () => {
    setSourceLoading(true);
    setSourceError(null);
    try {
      const items = await api.listActiveLiveSessions();
      setLiveSessions(items);
      setLiveSessionId((current) => items.some((item) => item.sessionId === current)
        ? current
        : items[0]?.sessionId ?? null);
    } catch (failure) {
      setSourceError(safeError(failure, "无法读取正在录制的直播间"));
    } finally {
      setSourceLoading(false);
    }
  }, [api]);

  const loadReplayStreamers = useCallback(async (
    search: string,
    cursor: AiReplayStreamerCursor | null,
    append: boolean,
  ) => {
    const requestId = ++replayStreamerRequestRef.current;
    if (append) setReplayStreamerLoadingMore(true);
    else setReplayStreamerLoading(true);
    setReplayStreamerError(null);
    try {
      const page = await api.listAiReplayStreamers(search, cursor, 20);
      if (requestId !== replayStreamerRequestRef.current) return;
      setReplayStreamers((current) => append
        ? [...current, ...page.items.filter((item) => !current.some((existing) => existing.streamerId === item.streamerId))]
        : page.items);
      setReplayStreamerCursor(page.nextCursor);
    } catch (failure) {
      if (requestId === replayStreamerRequestRef.current) setReplayStreamerError(safeError(failure, "无法读取回放主播"));
    } finally {
      if (requestId === replayStreamerRequestRef.current) {
        setReplayStreamerLoading(false);
        setReplayStreamerLoadingMore(false);
      }
    }
  }, [api]);

  const loadReplaySessions = useCallback(async (
    streamerId: number,
    search: string,
    cursor: AiSmartReplaySessionCursor | null,
    append: boolean,
  ) => {
    const requestId = ++replaySessionRequestRef.current;
    if (append) setReplayLoadingMore(true);
    else setReplayLoading(true);
    setReplayError(null);
    try {
      const page = await api.listSmartReplaySessions(streamerId, search, cursor, 20);
      if (requestId !== replaySessionRequestRef.current) return;
      setReplaySessions((current) => append
        ? [...current, ...page.items.filter((item) => !current.some((existing) => existing.sessionId === item.sessionId))]
        : page.items);
      setReplayCursor(page.nextCursor);
    } catch (failure) {
      if (requestId === replaySessionRequestRef.current) setReplayError(safeError(failure, "无法读取该主播的直播回放"));
    } finally {
      if (requestId === replaySessionRequestRef.current) {
        setReplayLoading(false);
        setReplayLoadingMore(false);
      }
    }
  }, [api]);

  useEffect(() => {
    if (mode === "live") void loadLiveSessions();
  }, [loadLiveSessions, mode]);

  useEffect(() => {
    if (mode !== "replay") return;
    void loadReplayStreamers("", null, false);
  }, [loadReplayStreamers, mode]);

  useEffect(() => {
    if (mode !== "replay" || !selectedReplayStreamer) return;
    setReplaySessions([]);
    setReplaySessionId(null);
    setReplayCursor(null);
    setReplaySessionSearch("");
    setReplayError(null);
    setAuthorized(false);
    setDuplicateConfirmed(false);
    replaySessionRequestRef.current += 1;
    void loadReplaySessions(selectedReplayStreamer.streamerId, "", null, false);
  }, [loadReplaySessions, mode, selectedReplayStreamer?.streamerId]);

  const selectedDetail = selectedWorkflowId === null ? null : snapshot.details.get(selectedWorkflowId) ?? null;
  const selectedReplay = replaySessions.find((session) => session.sessionId === replaySessionId) ?? null;
  const replayStreamerOptions = useMemo<SearchableComboboxOption[]>(() => replayStreamers.map((streamer) => ({
    value: String(streamer.streamerId),
    label: streamer.name,
    description: [
      streamer.tags.join("、"),
      streamer.webRid ? `直播间 ${streamer.webRid}` : "未绑定直播间",
      `${streamer.replayCount} 场回放`,
    ].filter(Boolean).join(" · "),
    status: streamer.liveStatus === "live" || streamer.monitorStatus === "recording"
      ? "正在直播"
      : streamer.archived ? "已归档" : !streamer.monitorEnabled ? "已暂停" : "可选择",
  })), [replayStreamers]);
  const replaySessionOptions = useMemo<SearchableComboboxOption[]>(() => replaySessions.map((session) => ({
    value: String(session.sessionId),
    label: `${formatSessionTime(session.startedAt)}–${formatSessionTime(session.endedAt)}`,
    description: [
      `会话 ${session.sessionId}`,
      formatDuration(session.totalDurationMs),
      `${session.videoCount} 个分片`,
      session.unavailableVideoCount > 0 ? `${session.unavailableVideoCount} 个不可用` : "",
    ].filter(Boolean).join(" · "),
    status: session.unavailableVideoCount > 0
      ? "文件失效"
      : session.existingWorkflowStatus
        ? `已有任务 · ${workflowStatusLabels[session.existingWorkflowStatus]}`
        : "已结束",
    disabled: session.unavailableVideoCount > 0,
  })), [replaySessions]);
  const resultDraft = useMemo(() => {
    if (!selectedDetail) return null;
    return [...selectedDetail.drafts]
      .filter((draft) => draft.firstReviewableAt !== null || ["review_ready", "exporting", "exported", "frozen"].includes(draft.status))
      .sort((left, right) => right.generation - left.generation)[0] ?? null;
  }, [selectedDetail]);

  useEffect(() => {
    const requestId = ++draftRequestRef.current;
    setDraftDetail(null);
    setDraftProjectId(0);
    setPreview(null);
    if (!selectedDetail || !resultDraft) return;
    setDraftLoading(true);
    void api.openSmartDraft(resultDraft.id)
      .then(async (detail) => {
        if (requestId !== draftRequestRef.current) return;
        const projectId = projectIdForDraft(selectedDetail, detail);
        setDraftDetail(detail);
        setDraftProjectId(projectId);
        const firstSegment = detail.segments[0];
        if (!firstSegment || projectId <= 0) return;
        try {
          const next = await api.requestAiInputPreview(projectId, firstSegment.inputId);
          if (requestId === draftRequestRef.current) setPreview(next);
        } catch (failure) {
          if (requestId === draftRequestRef.current) {
            setPreview({
              requestId: `one-click-preview-${resultDraft.id}`,
              videoId: firstSegment.inputId,
              state: "failed",
              progressPercent: null,
              message: "成片预览准备失败",
              media: null,
              errorCode: "preview_request_failed",
              errorMessage: safeError(failure, "无法准备成片预览"),
            });
          }
        }
      })
      .catch((failure) => {
        if (requestId === draftRequestRef.current) {
          setPreview({
            requestId: `one-click-draft-${resultDraft.id}`,
            videoId: 0,
            state: "failed",
            progressPercent: null,
            message: "无法读取成片草稿",
            media: null,
            errorCode: "draft_open_failed",
            errorMessage: safeError(failure, "无法读取成片草稿"),
          });
        }
      })
      .finally(() => { if (requestId === draftRequestRef.current) setDraftLoading(false); });
  }, [api, resultDraft?.id, selectedDetail?.workflow.id]);

  const sourceReady = mode === "local"
    ? grants.length > 0
    : mode === "live"
      ? liveSessionId !== null
      : selectedReplayStreamer !== null && selectedReplay !== null && selectedReplay.unavailableVideoCount === 0;
  const gatesReady = Boolean(environment?.ready && llmSettings?.keyConfigured);
  const duplicateReady = mode !== "replay" || selectedReplay?.existingWorkflowId === null || duplicateConfirmed;
  const canCreate = sourceReady && gatesReady && duplicateReady && authorized && !busy;
  const progress = workflowProgress(selectedDetail);
  const progressTitle = workflowProgressTitle(selectedDetail);
  const progressTone = workflowProgressTone(selectedDetail);
  const progressStageIndex = selectedDetail?.workflow.stage === "review"
    ? productStages.length - 1
    : productStages.findIndex((stage) => stage.key === selectedDetail?.workflow.stage);
  const progressStageLabel = progressStageIndex >= 0
    ? `${String(progressStageIndex + 1).padStart(2, "0")} / ${String(productStages.length).padStart(2, "0")} · ${productStages[progressStageIndex].label}`
    : "智能成片管线";
  const fallbackCandidateCount = Math.min(3, selectedDetail?.workflow.candidateCount ?? 0);
  const failedAttempt = selectedDetail
    ? [...selectedDetail.attempts].reverse().find((attempt) => attempt.status === "failed" || attempt.status === "interrupted") ?? null
    : null;
  const previewSegment = draftDetail?.segments[0] ?? null;

  useEffect(() => {
    const track = progressTrackRef.current;
    if (!track || progressStageIndex < 0 || track.scrollWidth <= track.clientWidth || typeof track.scrollTo !== "function") return;
    const activeStage = track.children.item(progressStageIndex) as HTMLElement | null;
    if (!activeStage) return;
    const reduceMotion = window.matchMedia?.("(prefers-reduced-motion: reduce)").matches ?? false;
    track.scrollTo({
      left: activeStage.offsetLeft - (track.clientWidth - activeStage.clientWidth) / 2,
      behavior: reduceMotion ? "auto" : "smooth",
    });
  }, [progressStageIndex, selectedDetail?.workflow.id]);

  const setSourceMode = (next: AiSmartWorkflowMode) => {
    setMode(next);
    setAuthorized(false);
    setDuplicateConfirmed(false);
    setSourceError(null);
  };

  const pickLocal = async () => {
    setBusy(true);
    setError(null);
    try {
      setGrants(await api.pickAiLocalVideos());
      setAuthorized(false);
    } catch (failure) {
      setError(safeError(failure, "系统文件选择器不可用"));
    } finally {
      setBusy(false);
    }
  };

  const create = async () => {
    if (!canCreate) return;
    setBusy(true);
    setError(null);
    const configuration = {
      name: mode === "local" ? "本地一键成品" : mode === "live" ? "直播首版成品" : "直播回放成品",
      provider: llmSettings?.provider ?? "deepseek",
      modelId: llmSettings?.modelId ?? "deepseek-chat",
      textScope: "selected_clip_subtitles" as const,
      outputPreference: "reviewable_compilation",
    };
    try {
      const detail = mode === "local"
        ? await api.createLocalSmartWorkflow({
            configuration,
            grantIds: grants.map((grant) => grant.grantId),
            authorizationConfirmed: true,
          })
        : mode === "live"
          ? await api.createLiveSmartWorkflow({ configuration, sessionId: liveSessionId!, authorizationConfirmed: true })
          : await api.createReplaySmartWorkflow({
              configuration,
              sessionId: replaySessionId!,
              authorizationConfirmed: true,
              duplicateConfirmed,
            });
      setSnapshot((current) => {
        const details = new Map(current.details);
        details.set(detail.workflow.id, detail);
        return {
          details,
          workflows: [detail.workflow, ...current.workflows.filter((item) => item.id !== detail.workflow.id)],
        };
      });
      setSelectedWorkflowId(detail.workflow.id);
      setGrants([]);
      setAuthorized(false);
      setDuplicateConfirmed(false);
      if (mode === "replay") {
        setReplaySessions((current) => current.map((session) => session.sessionId === replaySessionId
          ? { ...session, existingWorkflowId: detail.workflow.id, existingWorkflowStatus: detail.workflow.status }
          : session));
      }
    } catch (failure) {
      setError(safeError(failure, "一键成品任务创建失败"));
    } finally {
      setBusy(false);
    }
  };

  const retry = async () => {
    if (!selectedDetail || !failedAttempt) return;
    setBusy(true);
    setError(null);
    try {
      const updated = await api.retrySmartWorkflowStage({
        workflowId: selectedDetail.workflow.id,
        expectedGeneration: selectedDetail.workflow.generation,
        stage: failedAttempt.stage,
        batchId: failedAttempt.batchId,
      });
      setSnapshot((current) => {
        const details = new Map(current.details);
        details.set(updated.workflow.id, updated);
        return {
          details,
          workflows: current.workflows.map((workflow) => workflow.id === updated.workflow.id ? updated.workflow : workflow),
        };
      });
    } catch (failure) {
      setError(safeError(failure, "阶段重试失败"));
    } finally {
      setBusy(false);
    }
  };

  const cancel = async (successMessage?: string) => {
    if (!selectedDetail) return;
    setBusy(true);
    if (successMessage) setHighlightDecisionBusy("stop");
    setError(null);
    setMessage(null);
    try {
      const updated = await api.cancelSmartWorkflow(selectedDetail.workflow.id, selectedDetail.workflow.generation);
      setSnapshot((current) => {
        const details = new Map(current.details);
        details.set(updated.workflow.id, updated);
        return {
          details,
          workflows: current.workflows.map((workflow) => workflow.id === updated.workflow.id ? updated.workflow : workflow),
        };
      });
      if (successMessage) setMessage(successMessage);
    } catch (failure) {
      setError(safeError(failure, "无法取消当前任务"));
    } finally {
      setHighlightDecisionBusy(null);
      setBusy(false);
    }
  };

  const confirmHighlightFallback = async () => {
    if (!selectedDetail || fallbackCandidateCount === 0) return;
    setBusy(true);
    setHighlightDecisionBusy("continue");
    setError(null);
    setMessage(null);
    try {
      const updated = await api.confirmSmartHighlightFallback(
        selectedDetail.workflow.id,
        selectedDetail.workflow.generation,
      );
      setSnapshot((current) => {
        const details = new Map(current.details);
        details.set(updated.workflow.id, updated);
        return {
          details,
          workflows: current.workflows.map((workflow) => workflow.id === updated.workflow.id ? updated.workflow : workflow),
        };
      });
      setMessage(`已采用评分最高的 ${fallbackCandidateCount} 个候选，继续生成成品`);
    } catch (failure) {
      setError(safeError(failure, "无法确认默认精彩"));
    } finally {
      setHighlightDecisionBusy(null);
      setBusy(false);
    }
  };

  const openEditor = async () => {
    if (!resultDraft) return;
    if (draftDetail) {
      setEditor(draftDetail);
      return;
    }
    setBusy(true);
    try {
      const detail = await api.openSmartDraft(resultDraft.id);
      setDraftProjectId(selectedDetail ? projectIdForDraft(selectedDetail, detail) : 0);
      setDraftDetail(detail);
      setEditor(detail);
    } catch (failure) {
      setError(safeError(failure, "无法打开成片编辑器"));
    } finally {
      setBusy(false);
    }
  };

  const exportDraft = async () => {
    if (!resultDraft || !api.startAiClipExport) return;
    setBusy(true);
    setError(null);
    try {
      await api.startAiClipExport(resultDraft.clipProjectId);
      setMessage("已选择导出位置，正在生成视频");
    } catch (failure) {
      setError(safeError(failure, "无法开始导出"));
    } finally {
      setBusy(false);
    }
  };

  if (editor) {
    return <ClipEditor
      api={api}
      initial={editor}
      projectId={draftProjectId}
      llmSettings={llmSettings}
      backLabel="一键成品"
      onBack={() => setEditor(null)}
    />;
  }

  return (
    <div className="page-content one-click-page">
      {(error || message) && (
        <div className={`one-click-notice ${error ? "error" : "success"}`} role={error ? "alert" : "status"}>
          {error ? <AlertTriangle size={16} /> : <CheckCircle2 size={16} />}
          <span>{error ?? message}</span>
          <button aria-label="关闭提示" onClick={() => { setError(null); setMessage(null); }}><X size={15} /></button>
        </div>
      )}

      <section className="one-click-composer" aria-labelledby="one-click-new-title">
        <header>
          <div>
            <p className="section-kicker">NEW PRODUCT</p>
            <h2 id="one-click-new-title">新建成品</h2>
          </div>
          <span><ShieldCheck size={15} />当前任务单独授权</span>
        </header>

        <div className="one-click-mode" role="group" aria-label="成片来源">
          <button className={mode === "local" ? "active" : ""} aria-pressed={mode === "local"} onClick={() => setSourceMode("local")}><FolderOpen size={17} />本地视频</button>
          <button className={mode === "replay" ? "active" : ""} aria-pressed={mode === "replay"} onClick={() => setSourceMode("replay")}><Video size={17} />直播回放</button>
          <button className={mode === "live" ? "active" : ""} aria-pressed={mode === "live"} onClick={() => setSourceMode("live")}><Radio size={17} />正在直播</button>
        </div>

        <div className="one-click-source-row">
          {mode === "local" && (
            <div className="one-click-local-source">
              <button className="secondary-button" disabled={busy} onClick={() => void pickLocal()}><FileVideo2 size={16} />选择视频</button>
              <div aria-live="polite">
                {grants.length === 0
                  ? <span>尚未选择</span>
                  : <><strong>{grants.length} 个视频</strong><span>{grants.map((grant) => grant.displayName).join(" · ")}</span></>}
              </div>
            </div>
          )}

          {mode === "live" && (
            <div className="one-click-source-select">
              <label htmlFor="one-click-live-session">直播间</label>
              <select
                id="one-click-live-session"
                value={liveSessionId ?? ""}
                disabled={sourceLoading || liveSessions.length === 0}
                onChange={(event) => { setLiveSessionId(Number(event.target.value)); setAuthorized(false); }}
              >
                {liveSessions.length === 0 && <option value="">当前没有正在录制的直播间</option>}
                {liveSessions.map((session) => <option key={session.sessionId} value={session.sessionId}>{session.streamerName} · {formatSessionTime(session.startedAt)}</option>)}
              </select>
              <button className="icon-button" aria-label="刷新正在直播" title="刷新正在直播" disabled={sourceLoading} onClick={() => void loadLiveSessions()}><RefreshCw className={sourceLoading ? "spin" : ""} size={16} /></button>
            </div>
          )}

          {mode === "replay" && (
            <div className="one-click-replay-source">
              <SearchableCombobox
                ariaLabel="选择回放主播"
                placeholder="先选择主播"
                searchPlaceholder="搜索主播、标签或直播间 ID"
                value={selectedReplayStreamer ? String(selectedReplayStreamer.streamerId) : null}
                selectedLabel={selectedReplayStreamer?.name}
                options={replayStreamerOptions}
                loading={replayStreamerLoading}
                loadingMore={replayStreamerLoadingMore}
                error={replayStreamerError}
                hasMore={replayStreamerCursor !== null}
                emptyMessage="没有包含历史回放的主播"
                onChange={(value) => {
                  const streamer = replayStreamers.find((item) => item.streamerId === Number(value)) ?? null;
                  setSelectedReplayStreamer(streamer);
                }}
                onOpen={() => void loadReplayStreamers("", null, false)}
                onSearchChange={(search) => {
                  setReplayStreamerSearch(search);
                  void loadReplayStreamers(search, null, false);
                }}
                onLoadMore={() => void loadReplayStreamers(replayStreamerSearch, replayStreamerCursor, true)}
                onRetry={() => void loadReplayStreamers(replayStreamerSearch, null, false)}
              />
              <SearchableCombobox
                ariaLabel="选择直播回放"
                placeholder="再选择直播回放"
                searchPlaceholder="搜索日期、时间或会话 ID"
                value={replaySessionId === null ? null : String(replaySessionId)}
                selectedLabel={replaySessionId === null ? undefined : replaySessionOptions.find((option) => option.value === String(replaySessionId))?.label}
                options={replaySessionOptions}
                disabled={!selectedReplayStreamer}
                loading={replayLoading}
                loadingMore={replayLoadingMore}
                error={replayError}
                hasMore={replayCursor !== null}
                emptyMessage="该主播没有可用的已结束回放"
                onChange={(value) => {
                  setReplaySessionId(Number(value));
                  setAuthorized(false);
                  setDuplicateConfirmed(false);
                }}
                onOpen={() => {
                  if (selectedReplayStreamer && replaySessions.length === 0 && !replayError && !replayLoading) {
                    void loadReplaySessions(selectedReplayStreamer.streamerId, "", null, false);
                  }
                }}
                onSearchChange={(search) => {
                  if (search === replaySessionSearch && replayError) return;
                  setReplaySessionSearch(search);
                  if (selectedReplayStreamer) void loadReplaySessions(selectedReplayStreamer.streamerId, search, null, false);
                }}
                onLoadMore={() => {
                  if (selectedReplayStreamer) void loadReplaySessions(selectedReplayStreamer.streamerId, replaySessionSearch, replayCursor, true);
                }}
                onRetry={() => {
                  if (selectedReplayStreamer) void loadReplaySessions(selectedReplayStreamer.streamerId, replaySessionSearch, null, false);
                }}
              />
            </div>
          )}

          {mode === "live" && sourceLoading && <span className="one-click-source-state" role="status"><LoaderCircle className="spin" size={14} />正在读取来源</span>}
          {mode === "live" && sourceError && <span className="one-click-source-state error" role="alert"><AlertTriangle size={14} />{sourceError}</span>}
        </div>

        <footer className="one-click-authorization">
          <div className="one-click-gates" aria-label="运行条件">
            <span className={environment?.ready ? "ready" : "blocked"}>{environment?.ready ? <Check size={13} /> : <X size={13} />}ASR</span>
            <span className={llmSettings?.keyConfigured ? "ready" : "blocked"}>{llmSettings?.keyConfigured ? <Check size={13} /> : <X size={13} />}Agent</span>
            <span className={sourceReady ? "ready" : "blocked"}>{sourceReady ? <Check size={13} /> : <X size={13} />}来源</span>
          </div>
          <label>
            <input type="checkbox" checked={authorized} onChange={(event) => setAuthorized(event.target.checked)} />
            我确认仅为当前{mode === "live" ? "直播场次" : mode === "replay" ? "回放任务及冻结分片" : "任务"}授权自动精彩、文案校正与包装匹配
          </label>
          {mode === "replay" && selectedReplay?.existingWorkflowId != null && (
            <label className="one-click-duplicate">
              <input type="checkbox" checked={duplicateConfirmed} onChange={(event) => setDuplicateConfirmed(event.target.checked)} />
              已有一键成品任务，确认重新处理
            </label>
          )}
          <button className="primary-button" disabled={!canCreate} onClick={() => void create()}>{busy ? <LoaderCircle className="spin" size={16} /> : <Sparkles size={16} />}开始生成</button>
        </footer>
      </section>

      <section className="one-click-workbench" aria-labelledby="one-click-progress-title">
        <header className="one-click-taskbar">
          <div>
            <p className="section-kicker">CURRENT PRODUCT</p>
            <h2 id="one-click-progress-title">当前成品</h2>
          </div>
          {snapshot.workflows.length > 0 && (
            <div className="one-click-task-switcher">
              <select aria-label="切换当前任务" value={selectedWorkflowId ?? ""} onChange={(event) => setSelectedWorkflowId(Number(event.target.value))}>
                {snapshot.workflows.map((workflow) => <option key={workflow.id} value={workflow.id}>{workflow.name} · {workflowStatusLabels[workflow.status]}</option>)}
              </select>
              {selectedDetail && !["completed", "cancelled"].includes(selectedDetail.workflow.status) && <button className="icon-button" aria-label="取消当前任务" title="取消当前任务" disabled={busy} onClick={() => void cancel()}><Square size={15} /></button>}
            </div>
          )}
        </header>

        <section className={`one-click-progress-console ${progressTone}`} aria-label="当前生成状态">
          <div className="one-click-progress-stage">
            <span>{progressStageLabel}</span>
            <strong>{progressTitle}</strong>
            <small title={selectedDetail?.workflow.sourceSummary ?? undefined}>
              {selectedDetail?.workflow.sourceSummary ?? "选择素材并授权后开始生成"}
            </small>
          </div>
          <div className="one-click-progress-readout" aria-hidden="true">
            <b>{progress}<span>%</span></b>
            <small>{selectedDetail ? workflowStatusLabels[selectedDetail.workflow.status] : "尚未开始"}</small>
          </div>
          <div
            className="one-click-progress-rail"
            role="progressbar"
            aria-label={`${selectedDetail?.workflow.mode === "live" ? "首版" : "成品"}生成总进度`}
            aria-valuemin={0}
            aria-valuemax={100}
            aria-valuenow={progress}
            aria-valuetext={`${progressTitle}，${progress}%`}
          >
            <span className="one-click-progress-fill" style={{ width: `${progress}%` }}><i /></span>
          </div>
        </section>

        <ol ref={progressTrackRef} className="one-click-track" aria-label={`${selectedDetail?.workflow.mode === "live" ? "首版" : "成品"}生成进度 ${progress}%`}>
          {productStages.map((stage, index) => {
            const state = stageState(selectedDetail, stage.key);
            const attempt = selectedDetail ? latestAttempt(selectedDetail, stage.key) : null;
            return (
              <li key={stage.key} className={state} data-stage={stage.key}>
                <span>{String(index + 1).padStart(2, "0")}</span>
                <strong>{stage.label}</strong>
                <i aria-hidden="true">{state === "completed" ? <Check size={12} /> : state === "failed" ? <X size={12} /> : null}</i>
                {state === "working" && <small>{attempt?.progress ?? 0}%</small>}
              </li>
            );
          })}
        </ol>

        {selectedDetail?.workflow.status === "awaiting_selection" && (
          <div className="one-click-stage-action warning" role="alert" aria-labelledby="one-click-highlight-confirmation">
            <AlertTriangle size={17} />
            <span>
              <strong id="one-click-highlight-confirmation">确认默认精彩</strong>
              {fallbackCandidateCount > 0
                ? `当前没有达到自动入选阈值的候选。是否采用评分最高的 ${fallbackCandidateCount} 个候选继续？`
                : "当前没有可用的精彩候选，无法继续生成。"}
            </span>
            <div className="one-click-stage-actions">
              <button className="secondary-button" disabled={busy} onClick={() => void cancel("已停止生成，后续阶段不会执行")}>
                {highlightDecisionBusy === "stop" && <LoaderCircle className="spin" size={15} />}
                停止生成
              </button>
              {fallbackCandidateCount > 0 && (
                <button className="primary-button" disabled={busy} onClick={() => void confirmHighlightFallback()}>
                  {highlightDecisionBusy === "continue" ? <LoaderCircle className="spin" size={15} /> : <Check size={15} />}
                  采用前 {fallbackCandidateCount} 个并继续
                </button>
              )}
            </div>
          </div>
        )}

        {selectedDetail && ["failed", "paused"].includes(selectedDetail.workflow.status) && (
          <div className="one-click-stage-action failed" role="alert">
            <AlertTriangle size={17} />
            <span><strong>{selectedDetail.workflow.lastErrorCode ?? "当前阶段失败"}</strong>{selectedDetail.workflow.lastErrorMessage ?? "处理已暂停，已完成阶段不会重跑。"}</span>
            <button className="secondary-button" disabled={busy || !failedAttempt} onClick={() => void retry()}><RotateCcw size={15} />重试此阶段</button>
          </div>
        )}

        {resultDraft && (
          <section className="one-click-result" aria-labelledby="one-click-result-title">
            <div className="one-click-preview">
              {preview?.state === "ready" && preview.media
                ? <>
                    <video
                      controls
                      src={mediaUrl(preview.media.path)}
                      aria-label="首个精彩片段预览"
                      onLoadedMetadata={(event) => {
                        if (previewSegment) event.currentTarget.currentTime = previewSegment.sourceStartMs / 1_000;
                      }}
                      onTimeUpdate={(event) => {
                        if (!previewSegment || event.currentTarget.currentTime * 1_000 < previewSegment.sourceEndMs) return;
                        event.currentTarget.pause();
                        event.currentTarget.currentTime = previewSegment.sourceStartMs / 1_000;
                      }}
                    />
                    <span className="one-click-preview-badge">片段预览 1/{draftDetail?.segments.length ?? 1}</span>
                  </>
                : <div className="one-click-preview-placeholder">
                    {draftLoading || preview?.state === "queued" || preview?.state === "probing" || preview?.state === "remuxing" || preview?.state === "transcoding"
                      ? <LoaderCircle className="spin" size={30} />
                      : preview?.state === "failed" ? <AlertTriangle size={30} /> : <Play size={30} />}
                    <strong>{preview?.state === "failed" ? "预览准备失败" : draftLoading ? "正在准备预览" : "成片已就绪"}</strong>
                    <span>{preview?.errorMessage ?? (draftDetail ? `${draftDetail.segments.length} 个精彩片段 · ${formatDuration(draftDetail.projectDurationMs ?? 0)}` : `草稿 v${resultDraft.generation}`)}</span>
                  </div>}
            </div>
            <div className="one-click-result-meta">
              <div>
                <p className="section-kicker">READY TO REVIEW</p>
                <h3 id="one-click-result-title">{selectedDetail?.workflow.mode === "live" ? `首版成片 v${resultDraft.generation}` : `成片 v${resultDraft.generation}`}</h3>
                <p>{resultDraft.ownership === "automation" ? "自动版本" : "人工编辑版本"} · {resultDraft.status === "exported" ? "已导出" : resultDraft.status === "exporting" || resultDraft.status === "frozen" ? "导出中已冻结" : "可编辑、可导出"}</p>
              </div>
              {selectedDetail?.workflow.mode === "live" && <span className="one-click-live-note"><Radio size={14} />直播继续时自动准备下一版</span>}
              <div className="one-click-result-actions">
                <button className="secondary-button" disabled={busy} onClick={() => void openEditor()}><Pencil size={16} />编辑</button>
                <button className="primary-button" disabled={busy || !api.startAiClipExport} onClick={() => void exportDraft()}><Download size={16} />导出</button>
              </div>
            </div>
          </section>
        )}
      </section>
    </div>
  );
}
