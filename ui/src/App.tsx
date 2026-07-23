import {
  ArrowDown,
  ArrowUp,
  Archive,
  CheckCircle2,
  ChevronRight,
  CircleOff,
  Clock3,
  Download,
  ExternalLink,
  FileVideo2,
  FolderOpen,
  HardDrive,
  ImageOff,
  LayoutDashboard,
  LoaderCircle,
  Maximize2,
  Menu,
  MoreHorizontal,
  Pencil,
  Play,
  Plus,
  Radio,
  RefreshCw,
  RotateCcw,
  Save,
  Search,
  Settings,
  Sparkles,
  Square,
  Tag,
  Trash2,
  Video,
  Wifi,
  X,
} from "lucide-react";
import { convertFileSrc } from "@tauri-apps/api/core";
import { FormEvent, useCallback, useEffect, useMemo, useRef, useState } from "react";
import type {
  AppSettings,
  ClientApi,
  CreateStreamerInput,
  Dashboard,
  EnvironmentStatus,
  MonitorStatus,
  PreviewSnapshot,
  Streamer,
  StreamerTagInput,
  ThumbnailBatch,
  ThumbnailSnapshot,
  Video as VideoItem,
  VideoFilters,
} from "./types";
import { AiWorkspace } from "./AiWorkspace";

type Page = "monitor" | "library" | "ai" | "settings";
type HistoryFilters = { search: string; status: string; date: string };
type ActivePreview = { video: VideoItem; snapshot: PreviewSnapshot };

const emptyDashboard: Dashboard = {
  streamers: [],
  activeRecordings: 0,
  currentVideoCount: 0,
};

const monitorPriority: Record<MonitorStatus, number> = {
  recording: 0,
  retrying: 1,
  waiting_resource: 2,
  discovering: 3,
  rediscovering: 3,
  recording_error: 4,
  profile_error: 4,
  access_restricted: 4,
  layout_changed: 4,
  entry_invalid: 4,
  identity_conflict: 4,
  waiting_first_live: 5,
  waiting: 6,
  paused: 7,
};

const liveLabels = {
  checking: "检查中",
  offline: "未开播",
  live: "直播中",
  error: "检查失败",
};

const monitorLabels = {
  paused: "已暂停",
  waiting: "等待开播",
  discovering: "正在发现直播间",
  waiting_first_live: "等待首次开播",
  profile_error: "主页检查失败",
  rediscovering: "重新发现直播间",
  access_restricted: "访问受限",
  layout_changed: "页面结构变化",
  entry_invalid: "直播入口失效",
  identity_conflict: "身份冲突已暂停",
  waiting_resource: "等待资源",
  recording: "录制中",
  retrying: "正在重试",
  recording_error: "录制异常",
};

function formatDate(value: string | null): string {
  if (!value) return "尚未检查";
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;
  return new Intl.DateTimeFormat("zh-CN", {
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
  }).format(date);
}

function formatBytes(bytes: number): string {
  if (bytes <= 0) return "--";
  if (bytes < 1024 * 1024) return Math.round(bytes / 1024) + " KB";
  if (bytes < 1024 * 1024 * 1024) {
    return (bytes / (1024 * 1024)).toFixed(1) + " MB";
  }
  return (bytes / (1024 * 1024 * 1024)).toFixed(2) + " GB";
}

function fileName(path: string): string {
  return path.split(/[\\/]/).pop() || path;
}

function errorMessage(error: unknown, fallback: string): string {
  if (typeof error === "string" && error.trim()) return error;
  if (error instanceof Error && error.message) return error.message;
  if (typeof error === "object" && error !== null && "message" in error) {
    const message = (error as { message?: unknown }).message;
    if (typeof message === "string" && message.trim()) return message;
  }
  return fallback;
}

function toVideoFilters(filters: HistoryFilters): VideoFilters {
  const days = filters.date === "today" ? 1 : filters.date === "7days" ? 7 : filters.date === "30days" ? 30 : 0;
  return {
    search: filters.search.trim() || undefined,
    status: filters.status === "all" ? undefined : filters.status,
    from: days > 0 ? new Date(Date.now() - days * 24 * 60 * 60 * 1000).toISOString() : undefined,
  };
}

function pendingPreview(videoId: number): PreviewSnapshot {
  return {
    requestId: `pending-${videoId}`,
    videoId,
    state: "queued",
    progressPercent: null,
    message: "正在提交预览任务",
    media: null,
    errorCode: null,
    errorMessage: null,
  };
}

function failedPreview(videoId: number, error: unknown): PreviewSnapshot {
  const errorCode = typeof error === "object" && error !== null && "code" in error
    && typeof (error as { code?: unknown }).code === "string"
    ? (error as { code: string }).code
    : "request_failed";
  return {
    requestId: `failed-${videoId}`,
    videoId,
    state: "failed",
    progressPercent: null,
    message: "视频预览准备失败",
    media: null,
    errorCode,
    errorMessage: errorMessage(error, "无法准备视频预览"),
  };
}

function previewMediaUrl(path: string): string {
  return "__TAURI_INTERNALS__" in window ? convertFileSrc(path) : path;
}

function thumbnailMediaUrl(path: string): string {
  return "__TAURI_INTERNALS__" in window ? convertFileSrc(path) : path;
}

interface AppProps {
  api: ClientApi;
}

export function App({ api }: AppProps) {
  const [page, setPage] = useState<Page>("monitor");
  const [dashboard, setDashboard] = useState<Dashboard>(emptyDashboard);
  const [selectedId, setSelectedId] = useState<number | null>(null);
  const [currentVideos, setCurrentVideos] = useState<VideoItem[]>([]);
  const [historyVideos, setHistoryVideos] = useState<VideoItem[]>([]);
  const [historyTotal, setHistoryTotal] = useState(0);
  const [historyPage, setHistoryPage] = useState(1);
  const [historyStreamerId, setHistoryStreamerId] = useState<number | undefined>();
  const [historyFilters, setHistoryFilters] = useState<HistoryFilters>({ search: "", status: "all", date: "all" });
  const [settings, setSettings] = useState<AppSettings | null>(null);
  const [environment, setEnvironment] = useState<EnvironmentStatus | null>(null);
  const [loading, setLoading] = useState(true);
  const [notice, setNotice] = useState<string | null>(null);
  const [addOpen, setAddOpen] = useState(false);
  const [editingStreamer, setEditingStreamer] = useState<Streamer | null>(null);
  const [mobileNavOpen, setMobileNavOpen] = useState(false);
  const [preview, setPreview] = useState<ActivePreview | null>(null);
  const [thumbnailBatch, setThumbnailBatch] = useState<ThumbnailBatch | null>(null);
  const selectedIdRef = useRef(selectedId);
  const pageRef = useRef(page);
  const historyPageRef = useRef(historyPage);
  const historyStreamerIdRef = useRef(historyStreamerId);
  const historyFiltersRef = useRef(historyFilters);
  selectedIdRef.current = selectedId;
  pageRef.current = page;
  historyPageRef.current = historyPage;
  historyStreamerIdRef.current = historyStreamerId;
  historyFiltersRef.current = historyFilters;

  const refreshDashboard = useCallback(async () => {
    try {
      const next = await api.getDashboard();
      setDashboard(next);
      setSelectedId((current) => {
        if (current && next.streamers.some((item) => item.id === current)) return current;
        return next.streamers[0]?.id ?? null;
      });
    } catch (error) {
      setNotice(errorMessage(error, "读取监控数据失败"));
    } finally {
      setLoading(false);
    }
  }, [api]);

  const refreshCurrentVideos = useCallback(
    async (streamerId: number) => {
      try {
        const result = await api.listCurrentVideos(streamerId);
        setCurrentVideos(result.items);
      } catch (error) {
        setNotice(errorMessage(error, "读取视频列表失败"));
      }
    },
    [api],
  );

  const refreshHistoryVideos = useCallback(
    async (streamerId?: number, requestedPage = 1, filters = historyFiltersRef.current) => {
      try {
        const result = await api.listVideos(streamerId, requestedPage, 50, toVideoFilters(filters));
        setHistoryVideos(result.items);
        setHistoryTotal(result.total);
        setHistoryPage(result.page);
      } catch (error) {
        setNotice(errorMessage(error, "读取视频列表失败"));
      }
    },
    [api],
  );

  useEffect(() => {
    void refreshDashboard();
    void api.getSettings().then(setSettings).catch(() => undefined);
    let disposed = false;
    let unsubscribe: (() => void) | undefined;
    void api.subscribe((event) => {
      if (event.kind === "exit_confirmation_requested") {
        if (window.confirm("当前仍有活动录制。确认安全停止录制并退出应用？")) {
          void api.requestExit(true).catch((error) => {
            setNotice(errorMessage(error, "退出应用失败"));
          });
        }
        return;
      }
      if (event.kind === "streamer_merged" && event.streamerId) {
        void refreshDashboard().then(() => setSelectedId(event.streamerId));
      } else {
        void refreshDashboard();
      }
      if (selectedIdRef.current) void refreshCurrentVideos(selectedIdRef.current);
      if (pageRef.current === "library") {
        void refreshHistoryVideos(
          historyStreamerIdRef.current,
          historyPageRef.current,
          historyFiltersRef.current,
        );
      }
    }).then((handler) => {
      if (disposed) handler();
      else unsubscribe = handler;
    });
    return () => {
      disposed = true;
      unsubscribe?.();
    };
  }, [api, refreshCurrentVideos, refreshDashboard, refreshHistoryVideos]);

  useEffect(() => {
    let disposed = false;
    let unsubscribe: (() => void) | undefined;
    void api.subscribePreview((snapshot) => {
      setPreview((current) => current
        && current.video.id === snapshot.videoId
        && current.snapshot.requestId === snapshot.requestId
        ? { ...current, snapshot }
        : current);
    }).then((handler) => {
      if (disposed) handler();
      else unsubscribe = handler;
    });
    return () => {
      disposed = true;
      unsubscribe?.();
    };
  }, [api]);

  useEffect(() => {
    let disposed = false;
    let unsubscribe: (() => void) | undefined;
    void api.subscribeThumbnail((event) => {
      setThumbnailBatch((current) => {
        if (!current || current.batchId !== event.batchId || event.item.batchId !== current.batchId) {
          return current;
        }
        const existing = current.items.find((item) => item.videoId === event.item.videoId);
        if (!existing) return current;
        if (existing.cacheKey && event.item.cacheKey && existing.cacheKey !== event.item.cacheKey) {
          return current;
        }
        return {
          ...current,
          items: current.items.map((item) => item.videoId === event.item.videoId ? event.item : item),
        };
      });
    }).then((handler) => {
      if (disposed) handler();
      else unsubscribe = handler;
    });
    return () => {
      disposed = true;
      unsubscribe?.();
    };
  }, [api]);

  useEffect(() => {
    let disposed = false;
    let activeBatchId: string | null = null;
    setThumbnailBatch(null);
    if (page !== "library" || historyVideos.length === 0) return undefined;
    void api.requestVideoThumbnails(historyVideos.map((video) => video.id))
      .then((batch) => {
        activeBatchId = batch.batchId;
        if (disposed) {
          void api.releaseVideoThumbnailBatch(batch.batchId);
        } else {
          setThumbnailBatch(batch);
        }
      })
      .catch((error) => {
        if (!disposed) setNotice(errorMessage(error, "无法加载视频封面"));
      });
    return () => {
      disposed = true;
      if (activeBatchId) void api.releaseVideoThumbnailBatch(activeBatchId);
    };
  }, [api, historyVideos, page]);

  useEffect(() => {
    if (!thumbnailBatch?.items.some((item) => item.state === "queued")) return undefined;
    let disposed = false;
    const batchId = thumbnailBatch.batchId;
    const refresh = () => {
      void api.getVideoThumbnails(batchId)
        .then((batch) => {
          if (!disposed) setThumbnailBatch((current) => current?.batchId === batchId ? batch : current);
        })
        .catch(() => undefined);
    };
    const timer = window.setInterval(refresh, 1000);
    return () => {
      disposed = true;
      window.clearInterval(timer);
    };
  }, [api, thumbnailBatch]);

  useEffect(() => {
    if (selectedId) void refreshCurrentVideos(selectedId);
    else setCurrentVideos([]);
  }, [refreshCurrentVideos, selectedId]);

  useEffect(() => {
    if (page === "library") {
      void refreshHistoryVideos(
        historyStreamerIdRef.current,
        historyPageRef.current,
        historyFiltersRef.current,
      );
    }
    if (page === "settings") {
      void api.getSettings().then(setSettings).catch(() => undefined);
      void api.diagnoseEnvironment().then(setEnvironment).catch(() => undefined);
    }
  }, [api, page, refreshHistoryVideos]);

  const sortedStreamers = useMemo(
    () =>
      [...dashboard.streamers].sort((left, right) => {
        const status = monitorPriority[left.monitorStatus] - monitorPriority[right.monitorStatus];
        return status || left.name.localeCompare(right.name, "zh-CN");
      }),
    [dashboard.streamers],
  );

  const selectedStreamer = dashboard.streamers.find((item) => item.id === selectedId) ?? null;

  const navigate = (nextPage: Page) => {
    setPage(nextPage);
    setMobileNavOpen(false);
  };

  const action = async (operation: () => Promise<void>, success?: string) => {
    try {
      await operation();
      if (success) setNotice(success);
      await refreshDashboard();
    } catch (error) {
      setNotice(errorMessage(error, "操作失败"));
    }
  };

  const openPreview = (video: VideoItem) => {
    setPreview({ video, snapshot: pendingPreview(video.id) });
    void api.requestVideoPreview(video.id)
      .then((snapshot) => setPreview((current) => current?.video.id === video.id
        ? { video, snapshot }
        : current))
      .catch((error) => setPreview((current) => current?.video.id === video.id
        ? { video, snapshot: failedPreview(video.id, error) }
        : current));
  };

  const retryPreview = () => {
    if (!preview) return;
    const video = preview.video;
    setPreview({ video, snapshot: pendingPreview(video.id) });
    void api.retryVideoPreview(video.id)
      .then((snapshot) => setPreview((current) => current?.video.id === video.id
        ? { video, snapshot }
        : current))
      .catch((error) => setPreview((current) => current?.video.id === video.id
        ? { video, snapshot: failedPreview(video.id, error) }
        : current));
  };

  const updatePreviewSnapshot = useCallback((snapshot: PreviewSnapshot) => {
    setPreview((current) => current?.video.id === snapshot.videoId
      && current.snapshot.requestId === snapshot.requestId
      ? { ...current, snapshot }
      : current);
  }, []);

  const retryThumbnail = useCallback((videoId: number) => {
    const batchId = thumbnailBatch?.batchId;
    if (!batchId) return;
    void api.retryVideoThumbnail(batchId, videoId)
      .then((snapshot) => {
        setThumbnailBatch((current) => current?.batchId === batchId
          ? {
              ...current,
              items: current.items.map((item) => item.videoId === videoId ? snapshot : item),
            }
          : current);
      })
      .catch((error) => setNotice(errorMessage(error, "视频封面重试失败")));
  }, [api, thumbnailBatch?.batchId]);

  return (
    <div className="app-shell">
      <Sidebar
        page={page}
        open={mobileNavOpen}
        activeRecordings={dashboard.activeRecordings}
        onNavigate={navigate}
        onClose={() => setMobileNavOpen(false)}
      />

      <main className="main-area">
        <header className="topbar">
          <button className="icon-button mobile-menu" onClick={() => setMobileNavOpen(true)} aria-label="打开菜单">
            <Menu size={20} />
          </button>
          <div>
            <p className="eyebrow">DY SCREEN · 本地录制工作台</p>
            <h1>{pageTitle(page)}</h1>
          </div>
          <div className="topbar-actions">
            <div className="health-pill"><span className="health-dot" />后台服务运行中</div>
            {page === "monitor" && dashboard.streamers.length > 0 && (
              <button className="primary-button" onClick={() => { setEditingStreamer(null); setAddOpen(true); }}>
                <Plus size={18} />添加主播
              </button>
            )}
          </div>
        </header>

        {notice && (
          <div className="notice" role="status">
            <span>{notice}</span>
            <button onClick={() => setNotice(null)} aria-label="关闭提示"><X size={16} /></button>
          </div>
        )}

        {page === "monitor" && (
          <MonitorPage
            loading={loading}
            dashboard={dashboard}
            streamers={sortedStreamers}
            selected={selectedStreamer}
            videos={currentVideos}
            onAdd={() => { setEditingStreamer(null); setAddOpen(true); }}
            onSelect={setSelectedId}
            onRefresh={() => void refreshDashboard()}
            onToggle={(streamer) => void action(
              () => api.setMonitorEnabled(streamer.id, !streamer.monitorEnabled),
              streamer.monitorEnabled ? "已暂停监听" : "已恢复监听",
            )}
            onCheck={(id) => void action(() => api.checkStreamerNow(id), "已安排立即检查")}
            onEdit={(streamer) => { setEditingStreamer(streamer); setAddOpen(true); }}
            onStop={(id) => void action(() => api.stopRecording(id), "已请求停止录制")}
            onArchive={(id) => {
              if (window.confirm("归档后会停止监听，但历史视频会保留。确认归档？")) {
                void action(() => api.archiveStreamer(id), "主播已归档");
              }
            }}
            onPreviewVideo={openPreview}
            onOpenVideo={(id) => void action(() => api.openVideo(id))}
            onRevealVideo={(id) => void action(() => api.revealVideo(id))}
            onOpenLogs={() => void action(() => api.openLogs())}
          />
        )}

        {page === "library" && (
          <LibraryPage
            videos={historyVideos}
            streamers={dashboard.streamers}
            page={historyPage}
            total={historyTotal}
            filters={historyFilters}
            thumbnails={thumbnailBatch?.items ?? []}
            onFilter={(id) => {
              setHistoryStreamerId(id);
              setHistoryPage(1);
              void refreshHistoryVideos(id, 1, historyFilters);
            }}
            onFilters={(filters) => {
              setHistoryFilters(filters);
              setHistoryPage(1);
              void refreshHistoryVideos(historyStreamerId, 1, filters);
            }}
            onPage={(nextPage) => void refreshHistoryVideos(historyStreamerId, nextPage, historyFilters)}
            onPreview={openPreview}
            onRetryThumbnail={retryThumbnail}
            onOpen={(id) => void action(() => api.openVideo(id))}
            onReveal={(id) => void action(() => api.revealVideo(id))}
            onDelete={(id) => {
              if (window.confirm("删除后文件无法恢复，确认继续？")) {
                void action(async () => {
                  await api.deleteVideo(id);
                  await refreshHistoryVideos(historyStreamerId, historyPage, historyFilters);
                }, "视频已删除");
              }
            }}
            onDeleteSession={(sessionId) => {
              if (window.confirm("将删除该会话的全部视频文件，操作无法恢复。确认继续？")) {
                void action(async () => {
                  await api.deleteSession(sessionId);
                  await Promise.all([
                    refreshHistoryVideos(historyStreamerId, historyPage, historyFilters),
                    refreshDashboard(),
                  ]);
                }, "录制会话已删除");
              }
            }}
          />
        )}

        {page === "ai" && <AiWorkspace api={api} />}

        {page === "settings" && settings && (
          <SettingsPage
            settings={settings}
            environment={environment}
            onOpenLogs={() => void action(() => api.openLogs())}
            onDiagnose={() => void api.diagnoseEnvironment().then(setEnvironment)}
            onSave={(next) => void action(async () => {
              await api.saveSettings(next);
              setSettings(next);
            }, "设置已保存，新录制会话将使用最新配置")}
          />
        )}
      </main>

      {addOpen && (
        <AddStreamerModal
          api={api}
          initial={editingStreamer}
          onClose={() => { setAddOpen(false); setEditingStreamer(null); }}
          onSubmit={async (input) => {
            const created = editingStreamer
              ? await api.updateStreamer(editingStreamer.id, input)
              : await api.createStreamer(input);
            setAddOpen(false);
            setEditingStreamer(null);
            await refreshDashboard();
            setSelectedId(created.id);
            setNotice(editingStreamer ? "主播信息已更新" : "主播已添加，正在执行首次状态检查");
          }}
        />
      )}

      {preview && (
        <VideoPreviewDialog
          api={api}
          video={preview.video}
          snapshot={preview.snapshot}
          onSnapshot={updatePreviewSnapshot}
          onRetry={retryPreview}
          onOpenSystem={() => void action(() => api.openVideo(preview.video.id))}
          onClose={() => setPreview(null)}
        />
      )}
    </div>
  );
}

function pageTitle(page: Page): string {
  if (page === "monitor") return "监控中心";
  if (page === "library") return "视频资料库";
  if (page === "ai") return "AI 剪辑";
  return "应用设置";
}

function Sidebar({
  page,
  open,
  activeRecordings,
  onNavigate,
  onClose,
}: {
  page: Page;
  open: boolean;
  activeRecordings: number;
  onNavigate: (page: Page) => void;
  onClose: () => void;
}) {
  const items: Array<{ key: Page; label: string; icon: typeof LayoutDashboard }> = [
    { key: "monitor", label: "监控中心", icon: LayoutDashboard },
    { key: "library", label: "视频库", icon: FileVideo2 },
    { key: "ai", label: "AI 剪辑", icon: Sparkles },
    { key: "settings", label: "设置", icon: Settings },
  ];
  return (
    <>
      {open && <button className="sidebar-backdrop" aria-label="关闭菜单" onClick={onClose} />}
      <aside className={open ? "sidebar is-open" : "sidebar"}>
        <div className="brand">
          <div className="brand-mark"><Radio size={22} /></div>
          <div><strong>LivePilot</strong><span>直播管家</span></div>
          <button className="icon-button sidebar-close" onClick={onClose} aria-label="关闭菜单"><X size={18} /></button>
        </div>
        <nav className="nav-list" aria-label="主导航">
          {items.map((item) => {
            const Icon = item.icon;
            return (
              <button
                key={item.key}
                className={page === item.key ? "nav-item active" : "nav-item"}
                onClick={() => onNavigate(item.key)}
              >
                <Icon size={19} />{item.label}
                {item.key === "monitor" && activeRecordings > 0 && <span className="nav-badge">{activeRecordings}</span>}
              </button>
            );
          })}
        </nav>
        <div className="sidebar-card">
          <div className="sidebar-card-icon"><HardDrive size={18} /></div>
          <strong>本地优先</strong>
          <p>录像、数据库与设置仅保存在这台设备。</p>
          <div className="storage-line"><span>默认目录</span><b>Downloads</b></div>
        </div>
        <div className="sidebar-footer">
          <div className="avatar">LP</div>
          <div><strong>直播管家</strong><span>后台自动监听</span></div>
        </div>
      </aside>
    </>
  );
}

function MonitorPage({
  loading,
  dashboard,
  streamers,
  selected,
  videos,
  onAdd,
  onSelect,
  onRefresh,
  onToggle,
  onCheck,
  onEdit,
  onStop,
  onArchive,
  onPreviewVideo,
  onOpenVideo,
  onRevealVideo,
  onOpenLogs,
}: {
  loading: boolean;
  dashboard: Dashboard;
  streamers: Streamer[];
  selected: Streamer | null;
  videos: VideoItem[];
  onAdd: () => void;
  onSelect: (id: number) => void;
  onRefresh: () => void;
  onToggle: (streamer: Streamer) => void;
  onCheck: (id: number) => void;
  onEdit: (streamer: Streamer) => void;
  onStop: (id: number) => void;
  onArchive: (id: number) => void;
  onPreviewVideo: (video: VideoItem) => void;
  onOpenVideo: (id: number) => void;
  onRevealVideo: (id: number) => void;
  onOpenLogs: () => void;
}) {
  const liveCount = dashboard.streamers.filter((item) => item.liveStatus === "live").length;
  const waitingCount = dashboard.streamers.filter((item) =>
    item.monitorStatus === "waiting" || item.monitorStatus === "waiting_first_live"
  ).length;
  return (
    <div className="page-content">
      <section className="summary-grid">
        <SummaryCard icon={Radio} label="监控主播" value={dashboard.streamers.length} hint="已配置的活动主播" tone="green" />
        <SummaryCard icon={Wifi} label="正在直播" value={liveCount} hint="状态每 30 秒刷新" tone="orange" />
        <SummaryCard icon={Video} label="活动录制" value={dashboard.activeRecordings} hint="默认最多同时 4 路" tone="blue" />
        <SummaryCard icon={Clock3} label="等待开播" value={waitingCount} hint="应用关闭窗口后继续" tone="purple" />
      </section>

      <div className="monitor-layout">
        <section className="panel streamer-panel">
          <div className="panel-header">
            <div><p className="section-kicker">AUTOMATIC WATCH</p><h2>主播监听列表</h2></div>
            <button className="secondary-button" onClick={onRefresh}><RefreshCw size={16} />刷新</button>
          </div>
          {loading ? (
            <div className="empty-state"><LoaderCircle className="spin" /><h3>正在读取本地数据</h3></div>
          ) : streamers.length === 0 ? (
            <div className="empty-state spacious">
              <div className="empty-illustration"><Radio size={28} /><span /></div>
              <h3>还没有监控主播</h3>
              <p>添加公开抖音个人主页或直播间，应用会自动发现开播并录制视频与声音。</p>
              <button className="primary-button" onClick={onAdd}><Plus size={18} />添加主播</button>
            </div>
          ) : (
            <div className="table-wrap">
              <table>
                <thead><tr><th>主播 / 来源</th><th>直播状态</th><th>监听状态</th><th>最近检查</th><th>视频</th><th><span className="sr-only">操作</span></th></tr></thead>
                <tbody>
                  {streamers.map((streamer) => (
                    <StreamerRow
                      key={streamer.id}
                      streamer={streamer}
                      selected={selected?.id === streamer.id}
                      onSelect={() => onSelect(streamer.id)}
                      onToggle={() => onToggle(streamer)}
                      onCheck={() => onCheck(streamer.id)}
                      onEdit={() => onEdit(streamer)}
                      onStop={() => onStop(streamer.id)}
                      onArchive={() => onArchive(streamer.id)}
                      onOpenLogs={onOpenLogs}
                    />
                  ))}
                </tbody>
              </table>
            </div>
          )}
        </section>

        <SessionPanel
          streamer={selected}
          videos={videos}
          onPreview={onPreviewVideo}
          onOpen={onOpenVideo}
          onReveal={onRevealVideo}
        />
      </div>
    </div>
  );
}

function SummaryCard({ icon: Icon, label, value, hint, tone }: { icon: typeof Radio; label: string; value: number; hint: string; tone: string }) {
  return (
    <article className="summary-card">
      <div className={"summary-icon " + tone}><Icon size={21} /></div>
      <div><span>{label}</span><strong>{value}</strong><small>{hint}</small></div>
      <ChevronRight size={17} className="summary-arrow" />
    </article>
  );
}

function StreamerRow({ streamer, selected, onSelect, onToggle, onCheck, onEdit, onStop, onArchive, onOpenLogs }: {
  streamer: Streamer;
  selected: boolean;
  onSelect: () => void;
  onToggle: () => void;
  onCheck: () => void;
  onEdit: () => void;
  onStop: () => void;
  onArchive: () => void;
  onOpenLogs: () => void;
}) {
  const [menuOpen, setMenuOpen] = useState(false);
  const sourceLabel = streamer.sourceKind === "profile" ? "个人主页" : "直播间直连";
  const identityLabel = streamer.webRid
    ? `入口 ${streamer.webRid.slice(-8)}`
    : streamer.profileSecUid
      ? `主页 ${streamer.profileSecUid.slice(-8)}`
      : "身份待确认";
  const monitorKind = streamer.monitorStatus === "recording"
    ? "recording"
    : ["recording_error", "profile_error", "access_restricted", "layout_changed", "entry_invalid", "identity_conflict"].includes(streamer.monitorStatus)
      ? "error"
      : "neutral";
  return (
    <tr data-testid="streamer-row" className={selected ? "selected-row" : ""} onClick={onSelect}>
      <td><div className="streamer-cell"><div className="streamer-avatar">{streamer.name.slice(0, 1)}</div><div className="streamer-primary"><strong>{streamer.name}</strong><span>{sourceLabel} · {identityLabel}</span>{streamer.tags.length > 0 && <div className="streamer-tag-badges" aria-label={`${streamer.name}标签`}>{streamer.tags.slice(0, 2).map((tag) => <span key={tag.id}>{tag.name}</span>)}{streamer.tags.length > 2 && <b title={`另有 ${streamer.tags.length - 2} 个标签`}>+{streamer.tags.length - 2}</b>}</div>}</div></div></td>
      <td><StatusBadge kind={streamer.liveStatus === "live" ? "live" : streamer.liveStatus === "error" ? "error" : "neutral"}>{liveLabels[streamer.liveStatus]}</StatusBadge></td>
      <td><StatusBadge kind={monitorKind}>{monitorLabels[streamer.monitorStatus]}</StatusBadge></td>
      <td>
        <div className="muted-cell diagnostic-cell">
          <span>{formatDate(streamer.lastCheckedAt)}</span>
          {streamer.lastError && <small className="diagnostic-error" title={streamer.lastError}>{streamer.lastError}</small>}
          {streamer.failureCount > 0 && (
            <small className="diagnostic-meta">
              连续 {streamer.failureCount} 次
              {streamer.nextRetryAt && <> · 预计重试 {formatDate(streamer.nextRetryAt)}</>}
            </small>
          )}
        </div>
      </td>
      <td><div className="video-count"><strong>{streamer.currentVideoCount}</strong><span>本次</span><i /><strong>{streamer.historyVideoCount}</strong><span>历史</span></div></td>
      <td className="actions-cell" onClick={(event) => event.stopPropagation()}>
        <button className="icon-button" aria-label={streamer.name + "操作"} onClick={() => setMenuOpen(!menuOpen)}><MoreHorizontal size={18} /></button>
        {menuOpen && (
          <div className="action-menu">
            <button onClick={() => { onCheck(); setMenuOpen(false); }}><RefreshCw size={15} />立即检查</button>
            {streamer.roomUrl
              ? <a href={streamer.roomUrl} target="_blank" rel="noreferrer" aria-label="从操作菜单打开直播间" onClick={() => setMenuOpen(false)}><ExternalLink size={15} />打开直播间</a>
              : <button type="button" disabled><ExternalLink size={15} />直播间尚未发现</button>}
            <button onClick={() => { onOpenLogs(); setMenuOpen(false); }}><FolderOpen size={15} />打开日志目录</button>
            <button onClick={() => { onEdit(); setMenuOpen(false); }}><Pencil size={15} />编辑主播</button>
            <button onClick={() => { onToggle(); setMenuOpen(false); }}>{streamer.monitorEnabled ? <Square size={15} /> : <Play size={15} />}{streamer.monitorEnabled ? "暂停监听" : "恢复监听"}</button>
            {streamer.monitorStatus === "recording" && <button onClick={() => { onStop(); setMenuOpen(false); }}><CircleOff size={15} />停止录制</button>}
            <button className="danger" onClick={() => { onArchive(); setMenuOpen(false); }}><Archive size={15} />归档主播</button>
          </div>
        )}
      </td>
    </tr>
  );
}

function StatusBadge({ children, kind }: { children: string; kind: "live" | "recording" | "error" | "neutral" }) {
  return <span className={"status-badge " + kind}><span />{children}</span>;
}

function SessionPanel({ streamer, videos, onPreview, onOpen, onReveal }: { streamer: Streamer | null; videos: VideoItem[]; onPreview: (video: VideoItem) => void; onOpen: (id: number) => void; onReveal: (id: number) => void }) {
  return (
    <aside className="panel session-panel">
      <div className="panel-header compact"><div><p className="section-kicker">CURRENT SESSION</p><h2>本次监听视频</h2></div>{streamer && <StatusBadge kind={streamer.monitorStatus === "recording" ? "recording" : "neutral"}>{monitorLabels[streamer.monitorStatus]}</StatusBadge>}</div>
      {!streamer ? (
        <div className="empty-state"><FileVideo2 size={28} /><h3>请选择主播</h3><p>选择左侧主播后查看本次会话和完成分片。</p></div>
      ) : (
        <>
          <div className="session-streamer"><div className="streamer-avatar large">{streamer.name.slice(0, 1)}</div><div><strong>{streamer.name}</strong>{streamer.roomUrl ? <a href={streamer.roomUrl} target="_blank" rel="noreferrer">打开直播间 <ExternalLink size={12} /></a> : <button type="button" disabled aria-label="直播间尚未发现">直播间尚未发现</button>}</div></div>
          <section className="streamer-tag-details" aria-label="主播标签详情">
            <header><Tag size={15} /><strong>主播标签</strong><span>未来切片上下文</span></header>
            {streamer.tags.length === 0 ? (
              <p className="tag-empty">尚未设置标签</p>
            ) : (
              <div>{streamer.tags.map((tag) => <article key={tag.id}><span>{tag.name}</span>{tag.promptGuidance && <p>{tag.promptGuidance}</p>}</article>)}</div>
            )}
          </section>
          <div className="session-stats"><div><span>完成分片</span><strong>{videos.length}</strong></div><div><span>累计大小</span><strong>{formatBytes(videos.reduce((total, item) => total + item.sizeBytes, 0))}</strong></div></div>
          <div className="video-list">
            {videos.length === 0 ? (
              <div className="mini-empty"><div className="pulse-ring"><Video size={21} /></div><strong>等待完成分片</strong><p>录制开始后，已正确关闭的 MKV 会显示在这里。</p></div>
            ) : videos.map((video) => (
              <article className="video-item" key={video.id}>
                <div className="video-thumb"><Play size={17} /></div>
                <div className="video-meta"><strong title={video.path}>{fileName(video.path)}</strong><span>{formatDate(video.startedAt)} · {formatBytes(video.sizeBytes)}</span></div>
                <div className="video-actions"><button aria-label="预览视频" disabled={video.status === "missing" && video.hasPreviewCache !== true} onClick={() => onPreview(video)}><Play size={15} /></button><button aria-label="使用系统播放器打开" disabled={video.status === "missing"} onClick={() => onOpen(video.id)}><ExternalLink size={15} /></button><button aria-label="定位视频" disabled={video.status === "missing"} onClick={() => onReveal(video.id)}><FolderOpen size={15} /></button></div>
              </article>
            ))}
          </div>
          <div className="session-footnote"><CheckCircle2 size={15} /><span>只展示已完成并可安全播放的 MKV 分片</span></div>
        </>
      )}
    </aside>
  );
}

function LibraryPage({ videos, streamers, page, total, filters, thumbnails, onFilter, onFilters, onPage, onPreview, onRetryThumbnail, onOpen, onReveal, onDelete, onDeleteSession }: { videos: VideoItem[]; streamers: Streamer[]; page: number; total: number; filters: HistoryFilters; thumbnails: ThumbnailSnapshot[]; onFilter: (id?: number) => void; onFilters: (filters: HistoryFilters) => void; onPage: (page: number) => void; onPreview: (video: VideoItem) => void; onRetryThumbnail: (videoId: number) => void; onOpen: (id: number) => void; onReveal: (id: number) => void; onDelete: (id: number) => void; onDeleteSession: (sessionId: number) => void }) {
  const totalPages = Math.max(1, Math.ceil(total / 50));
  const thumbnailsByVideo = new Map(thumbnails.map((item) => [item.videoId, item]));
  const sessions = Array.from(videos.reduce((groups, video) => {
    const current = groups.get(video.sessionId) ?? [];
    current.push(video);
    groups.set(video.sessionId, current);
    return groups;
  }, new Map<number, VideoItem[]>()).entries());
  return (
    <div className="page-content">
      <section className="library-hero"><div><p className="section-kicker">RECORDING ARCHIVE</p><h2>所有录制历史都在本机</h2><p>按主播和会话浏览完成分片，可在应用内预览或使用系统播放器打开原文件。</p></div><div className="hero-icon"><Download size={29} /></div></section>
      <section className="panel library-panel">
        <div className="filter-bar"><label className="search-field"><Search size={17} /><input value={filters.search} onChange={(event) => onFilters({ ...filters, search: event.target.value })} placeholder="搜索主播或文件名" /></label><select aria-label="按主播筛选" defaultValue="" onChange={(event) => onFilter(event.target.value ? Number(event.target.value) : undefined)}><option value="">全部主播</option>{streamers.map((item) => <option key={item.id} value={item.id}>{item.name}</option>)}</select><select aria-label="按日期筛选" value={filters.date} onChange={(event) => onFilters({ ...filters, date: event.target.value })}><option value="all">全部日期</option><option value="today">今天</option><option value="7days">最近 7 天</option><option value="30days">最近 30 天</option></select><select aria-label="按状态筛选" value={filters.status} onChange={(event) => onFilters({ ...filters, status: event.target.value })}><option value="all">全部状态</option><option value="complete">文件正常</option><option value="missing">文件缺失</option></select><div className="result-count">共 {total} 条</div></div>
        {videos.length === 0 ? (
          <div className="empty-state spacious"><FileVideo2 size={32} /><h3>还没有历史视频</h3><p>主播开播并完成第一个分片后，视频会自动归档到这里。</p></div>
        ) : (
          <div className="library-sessions">{sessions.map(([sessionId, items]) => <section className="library-session" key={sessionId}><header><div><span>{items[0].streamerName}</span><strong>会话 #{sessionId}</strong><small>{items.length} 个分片 · {formatBytes(items.reduce((sum, item) => sum + item.sizeBytes, 0))}</small></div><button className="danger session-delete" aria-label={`删除会话 #${sessionId}`} onClick={() => onDeleteSession(sessionId)}><Trash2 size={15} />删除整个会话</button></header><div className="library-grid">{items.map((video) => <article className="library-card" key={video.id}><LibraryThumbnail video={video} thumbnail={thumbnailsByVideo.get(video.id)} onPreview={() => onPreview(video)} onRetry={() => onRetryThumbnail(video.id)} /><div className="library-card-body"><span>{video.streamerName}</span><strong title={video.path}>{fileName(video.path)}</strong><small>{formatDate(video.startedAt)} · {formatBytes(video.sizeBytes)}</small><div><button aria-label={`预览 ${fileName(video.path)}`} disabled={video.status === "missing" && video.hasPreviewCache !== true} onClick={() => onPreview(video)}><Play size={15} />预览</button><button disabled={video.status === "missing"} onClick={() => onOpen(video.id)}><ExternalLink size={15} />系统打开</button><button disabled={video.status === "missing"} onClick={() => onReveal(video.id)}><FolderOpen size={15} />定位</button><button className="danger" onClick={() => onDelete(video.id)}><Trash2 size={15} /></button></div></div></article>)}</div></section>)}</div>
        )}
        {total > 50 && <div className="pagination"><button disabled={page <= 1} onClick={() => onPage(page - 1)}>上一页</button><span>第 {page} / {totalPages} 页</span><button disabled={page >= totalPages} onClick={() => onPage(page + 1)}>下一页</button></div>}
      </section>
    </div>
  );
}

function LibraryThumbnail({ video, thumbnail, onPreview, onRetry }: {
  video: VideoItem;
  thumbnail: ThumbnailSnapshot | undefined;
  onPreview: () => void;
  onRetry: () => void;
}) {
  const [imageFailed, setImageFailed] = useState(false);
  const mediaPath = thumbnail?.state === "ready" ? thumbnail.media?.path ?? null : null;
  useEffect(() => setImageFailed(false), [mediaPath]);
  const label = video.status === "missing"
    ? "文件缺失"
    : fileName(video.path).toLowerCase().endsWith(".mp4") ? "MP4" : "MKV";
  const badge = <span className={video.status === "missing" ? "file-state missing" : "file-state"}>{label}</span>;

  if (mediaPath && !imageFailed) {
    return (
      <button className="library-preview thumbnail-ready" aria-label={`预览 ${fileName(video.path)} 封面`} onClick={onPreview}>
        <img src={thumbnailMediaUrl(mediaPath)} alt={`${fileName(video.path)} 封面`} onError={() => setImageFailed(true)} />
        <span className="thumbnail-play"><Play size={17} /></span>
        {badge}
      </button>
    );
  }
  if (thumbnail?.state === "failed" || imageFailed) {
    return (
      <div className="library-preview thumbnail-placeholder failed" title={thumbnail?.errorMessage ?? "视频封面加载失败"}>
        <ImageOff size={27} />
        <button className="thumbnail-retry" aria-label={`重试封面 ${fileName(video.path)}`} title="重试生成封面" onClick={onRetry}><RotateCcw size={17} /></button>
        {badge}
      </div>
    );
  }
  if (thumbnail?.state === "unavailable") {
    return <div className="library-preview thumbnail-placeholder unavailable" title={thumbnail.errorMessage ?? "视频封面不可用"}><ImageOff size={27} />{badge}</div>;
  }
  return <div className="library-preview thumbnail-placeholder queued" aria-label={`正在加载封面 ${fileName(video.path)}`}><LoaderCircle className="spin" size={25} />{badge}</div>;
}

function VideoPreviewDialog({ api, video, snapshot, onSnapshot, onRetry, onOpenSystem, onClose }: {
  api: ClientApi;
  video: VideoItem;
  snapshot: PreviewSnapshot;
  onSnapshot: (snapshot: PreviewSnapshot) => void;
  onRetry: () => void;
  onOpenSystem: () => void;
  onClose: () => void;
}) {
  const videoRef = useRef<HTMLVideoElement>(null);
  const modalRef = useRef<HTMLElement>(null);
  const [playbackRate, setPlaybackRate] = useState(1);
  const [autoplayBlocked, setAutoplayBlocked] = useState(false);
  const [playbackError, setPlaybackError] = useState<string | null>(null);
  const mediaUrl = snapshot.state === "ready" && snapshot.media
    ? previewMediaUrl(snapshot.media.path)
    : null;
  const sourceUnavailable = video.status === "missing"
    || snapshot.media?.sourceMissing === true
    || snapshot.errorCode === "source_missing";

  useEffect(() => {
    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") onClose();
    };
    window.addEventListener("keydown", handleKeyDown);
    return () => window.removeEventListener("keydown", handleKeyDown);
  }, [onClose]);

  useEffect(() => {
    if (["ready", "failed"].includes(snapshot.state)
      || snapshot.requestId.startsWith("pending-")
      || snapshot.requestId.startsWith("failed-")) return;
    let disposed = false;
    const refresh = async () => {
      try {
        const latest = await api.getVideoPreview(snapshot.requestId);
        if (!disposed && latest) onSnapshot(latest);
      } catch {
        // 状态事件仍会继续更新，短暂查询失败不覆盖当前状态。
      }
    };
    void refresh();
    const timer = window.setInterval(() => void refresh(), 1000);
    return () => {
      disposed = true;
      window.clearInterval(timer);
    };
  }, [api, onSnapshot, snapshot.requestId, snapshot.state]);

  useEffect(() => {
    setPlaybackError(null);
    setAutoplayBlocked(false);
  }, [mediaUrl, snapshot.requestId]);

  useEffect(() => {
    if (!mediaUrl) return;
    void api.retainVideoPreview(snapshot.requestId);
    return () => {
      const player = videoRef.current;
      player?.pause();
      if (player) player.removeAttribute("src");
      void api.releaseVideoPreview(snapshot.requestId);
    };
  }, [api, mediaUrl, snapshot.requestId]);

  const tryAutoplay = () => {
    const player = videoRef.current;
    if (!player || !("__TAURI_INTERNALS__" in window)) return;
    player.play()
      .then(() => setAutoplayBlocked(false))
      .catch(() => setAutoplayBlocked(true));
  };

  const changeRate = (rate: number) => {
    setPlaybackRate(rate);
    if (videoRef.current) videoRef.current.playbackRate = rate;
  };

  const enterFullscreen = () => {
    const target = modalRef.current;
    if (target?.requestFullscreen) void target.requestFullscreen();
  };

  const statusClass = snapshot.state === "failed" ? "failed" : snapshot.state === "ready" ? "ready" : "working";
  return (
    <div className="modal-backdrop preview-backdrop" role="presentation" onMouseDown={(event) => { if (event.target === event.currentTarget) onClose(); }}>
      <section ref={modalRef} className="preview-modal" role="dialog" aria-modal="true" aria-label="视频预览">
        <header className="preview-header">
          <div><p className="section-kicker">IN-APP PREVIEW</p><h2>视频预览</h2><span title={video.path}>{fileName(video.path)}</span></div>
          <button autoFocus className="icon-button" aria-label="关闭视频预览" onClick={onClose}><X size={20} /></button>
        </header>

        <div className="preview-stage">
          {mediaUrl ? (
            <>
              <video
                ref={videoRef}
                aria-label="视频播放器"
                src={mediaUrl}
                controls
                playsInline
                onCanPlay={tryAutoplay}
                onError={() => setPlaybackError("WebView 无法播放该预览文件，请尝试系统播放器")}
              />
              {autoplayBlocked && <button className="preview-play-overlay" onClick={() => void videoRef.current?.play().then(() => setAutoplayBlocked(false))}><Play size={24} />点击播放</button>}
            </>
          ) : snapshot.state === "failed" ? (
            <div className="preview-error"><CircleOff size={37} /><h3>无法准备视频预览</h3><p>{snapshot.errorMessage || "视频预览处理失败"}</p><div><button className="primary-button" onClick={onRetry}><RotateCcw size={16} />重试预览</button><button className="secondary-button" disabled={sourceUnavailable} onClick={onOpenSystem}><ExternalLink size={16} />使用系统播放器打开</button></div></div>
          ) : (
            <div className="preview-preparing"><LoaderCircle className="spin" size={36} /><h3>{snapshot.message}</h3><p>{snapshot.state === "transcoding" ? "源编码与 MP4 不兼容，正在转换为 H.264 + AAC。" : "原始 MKV 会保留，预览缓存准备完成后将自动播放。"}</p>{snapshot.progressPercent !== null && <div className="preview-progress" aria-label={`转换进度 ${Math.round(snapshot.progressPercent)}%`}><span style={{ width: `${snapshot.progressPercent}%` }} /></div>}</div>
          )}
        </div>

        <footer className="preview-footer">
          <div className={`preview-status ${statusClass}`}><span />{snapshot.message}{snapshot.progressPercent !== null && snapshot.state !== "ready" ? ` · ${Math.round(snapshot.progressPercent)}%` : ""}</div>
          {snapshot.media?.cacheHit && <span className="cache-badge">已使用预览缓存</span>}
          {snapshot.media?.sourceMissing && <span className="cache-badge warning">原文件缺失，仅播放缓存</span>}
          {mediaUrl && <label className="preview-rate">倍速<select aria-label="播放倍速" value={playbackRate} onChange={(event) => changeRate(Number(event.target.value))}><option value={0.5}>0.5×</option><option value={1}>1×</option><option value={1.25}>1.25×</option><option value={1.5}>1.5×</option><option value={2}>2×</option></select></label>}
          {mediaUrl && <button className="secondary-button compact" onClick={enterFullscreen}><Maximize2 size={15} />全屏</button>}
          {snapshot.state !== "failed" && <button className="secondary-button compact" disabled={sourceUnavailable} onClick={onOpenSystem}><ExternalLink size={15} />系统打开</button>}
        </footer>
        {playbackError && <div className="preview-playback-error" role="alert"><span>{playbackError}</span><button onClick={onRetry}>重新准备预览</button><button disabled={sourceUnavailable} onClick={onOpenSystem}>使用系统播放器打开</button></div>}
      </section>
    </div>
  );
}

function SettingsPage({ settings, environment, onOpenLogs, onDiagnose, onSave }: { settings: AppSettings; environment: EnvironmentStatus | null; onOpenLogs: () => void; onDiagnose: () => void; onSave: (settings: AppSettings) => void }) {
  const [form, setForm] = useState(settings);
  const submit = (event: FormEvent) => { event.preventDefault(); onSave(form); };
  return (
    <form className="page-content settings-page" onSubmit={submit}>
      <div className="settings-layout">
        <section className="panel settings-card"><div className="panel-header"><div><p className="section-kicker">RECORDING</p><h2>录像设置</h2></div><HardDrive size={22} /></div><label>录像保存目录<input value={form.outputRoot} onChange={(event) => setForm({ ...form, outputRoot: event.target.value })} /><small>默认位于下载目录，修改后只影响新录制。</small></label><div className="form-grid"><label>清晰度<select value={form.quality} onChange={(event) => setForm({ ...form, quality: event.target.value })}><option>FULL_HD1</option><option>HD1</option><option>SD1</option><option>SD2</option></select></label><label>协议<select value={form.protocol} onChange={(event) => setForm({ ...form, protocol: event.target.value })}><option value="flv">FLV</option><option value="hls">HLS</option></select></label><label>分片时长（秒）<input type="number" min="60" value={form.segmentSeconds} onChange={(event) => setForm({ ...form, segmentSeconds: Number(event.target.value) })} /></label><label>最大并发录制<input type="number" min="1" max="16" value={form.maxConcurrentRecordings} onChange={(event) => setForm({ ...form, maxConcurrentRecordings: Number(event.target.value) })} /></label></div></section>
        <section className="panel settings-card"><div className="panel-header"><div><p className="section-kicker">ENVIRONMENT</p><h2>运行环境</h2></div><button type="button" className="secondary-button" onClick={onDiagnose}><RefreshCw size={15} />重新诊断</button></div><label>FFmpeg 路径<input value={form.ffmpegPath} onChange={(event) => setForm({ ...form, ffmpegPath: event.target.value })} /></label><label>FFprobe 路径<input value={form.ffprobePath} onChange={(event) => setForm({ ...form, ffprobePath: event.target.value })} /></label><div className="diagnostic-list"><div><span className={environment?.ffmpeg ? "diagnostic ok" : "diagnostic"}>{environment?.ffmpeg ? <CheckCircle2 size={16} /> : <CircleOff size={16} />}FFmpeg</span><b>{environment?.ffmpeg ? "可用" : "未检测到"}</b></div><div><span className={environment?.ffprobe ? "diagnostic ok" : "diagnostic"}>{environment?.ffprobe ? <CheckCircle2 size={16} /> : <CircleOff size={16} />}FFprobe</span><b>{environment?.ffprobe ? "可用" : "未检测到"}</b></div></div></section>
        <section className="panel settings-card"><div className="panel-header"><div><p className="section-kicker">DESKTOP</p><h2>桌面行为</h2></div><Settings size={22} /></div><label className="switch-row"><div><strong>系统通知</strong><small>开播、录制结束、失败和磁盘不足时提醒。</small></div><input type="checkbox" checked={form.notificationsEnabled} onChange={(event) => setForm({ ...form, notificationsEnabled: event.target.checked })} /></label><label className="switch-row"><div><strong>开机自动启动</strong><small>登录系统后恢复已启用的监听任务。</small></div><input type="checkbox" checked={form.autostartEnabled} onChange={(event) => setForm({ ...form, autostartEnabled: event.target.checked })} /></label><button type="button" className="secondary-button full" onClick={onOpenLogs}><FolderOpen size={16} />打开日志目录</button><p className="tray-note">关闭主窗口后应用会驻留系统托盘；请通过托盘菜单显式退出。</p></section>
      </div>
      <div className="save-bar"><span><CheckCircle2 size={16} />数据库和录像均保存在本地</span><button className="primary-button" type="submit"><Save size={17} />保存设置</button></div>
    </form>
  );
}

function AddStreamerModal({ api, initial, onClose, onSubmit }: { api: ClientApi; initial: Streamer | null; onClose: () => void; onSubmit: (input: CreateStreamerInput) => Promise<void> }) {
  const [name, setName] = useState(initial?.name ?? "");
  const [sourceUrl, setSourceUrl] = useState(initial?.sourceUrl ?? "");
  const [monitorEnabled, setMonitorEnabled] = useState(initial?.monitorEnabled ?? true);
  const [tags, setTags] = useState<StreamerTagInput[]>(
    initial?.tags.map((tag) => ({
      name: tag.name,
      promptGuidance: tag.promptGuidance,
    })) ?? [],
  );
  const [suggestions, setSuggestions] = useState<string[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState(false);

  useEffect(() => {
    let active = true;
    void api.listStreamerTagNameSuggestions(20)
      .then((items) => {
        if (active) setSuggestions(items);
      })
      .catch(() => {
        if (active) setSuggestions([]);
      });
    return () => { active = false; };
  }, [api]);

  const addTag = (tagName = "") => {
    if (tags.length >= 10) return;
    if (tagName && tags.some((tag) => tag.name.trim().toLocaleLowerCase() === tagName.trim().toLocaleLowerCase())) {
      setError("同一主播不能设置重复标签");
      return;
    }
    setTags([...tags, { name: tagName, promptGuidance: null }]);
    setError(null);
  };

  const updateTag = (index: number, value: Partial<StreamerTagInput>) => {
    setTags(tags.map((tag, tagIndex) => tagIndex === index ? { ...tag, ...value } : tag));
    setError(null);
  };

  const removeTag = (index: number) => {
    setTags(tags.filter((_, tagIndex) => tagIndex !== index));
    setError(null);
  };

  const moveTag = (index: number, direction: -1 | 1) => {
    const target = index + direction;
    if (target < 0 || target >= tags.length) return;
    const reordered = [...tags];
    [reordered[index], reordered[target]] = [reordered[target], reordered[index]];
    setTags(reordered);
    setError(null);
  };

  const validatedTags = (): StreamerTagInput[] | null => {
    if (tags.length > 10) {
      setError("每个主播最多可以设置 10 个标签");
      return null;
    }
    const normalizedNames = new Set<string>();
    const normalized: StreamerTagInput[] = [];
    for (const tag of tags) {
      if (/[\u0000-\u001f\u007f]/.test(tag.name) || (tag.promptGuidance && /[\u0000-\u001f\u007f]/.test(tag.promptGuidance))) {
        setError("标签名称和指导不能包含控制字符");
        return null;
      }
      const tagName = tag.name.trim();
      const promptGuidance = tag.promptGuidance?.trim() || null;
      if (!tagName) {
        setError("标签名称不能为空");
        return null;
      }
      if ([...tagName].length > 24) {
        setError("标签名称不能超过 24 个字符");
        return null;
      }
      if (promptGuidance && [...promptGuidance].length > 500) {
        setError("标签指导不能超过 500 个字符");
        return null;
      }
      const key = tagName.toLocaleLowerCase();
      if (normalizedNames.has(key)) {
        setError("同一主播不能设置重复标签");
        return null;
      }
      normalizedNames.add(key);
      normalized.push({ name: tagName, promptGuidance });
    }
    return normalized;
  };

  const submit = async (event: FormEvent) => {
    event.preventDefault();
    setError(null);
    const normalizedSource = sourceUrl.trim();
    const isProfile = /^https?:\/\/(?:www\.)?douyin\.com\/user\/[^/?#]+(?:[?#].*)?$/.test(normalizedSource);
    const isRoom = /^https?:\/\/live\.douyin\.com\/\d+(?:[/?#].*)?$/.test(normalizedSource);
    if (!isProfile && !isRoom) { setError("请输入有效的个人主页或直播间链接"); return; }
    if (isRoom && !name.trim()) { setError("直播间链接必须填写主播名称"); return; }
    const normalizedTags = validatedTags();
    if (!normalizedTags) return;
    setSubmitting(true);
    try { await onSubmit({ name: name.trim(), sourceUrl: normalizedSource, monitorEnabled, tags: normalizedTags }); }
    catch (reason) { setError(errorMessage(reason, initial ? "修改主播失败" : "添加主播失败")); setSubmitting(false); }
  };
  return (
    <div className="modal-backdrop" role="presentation" onMouseDown={(event) => { if (event.target === event.currentTarget) onClose(); }}>
      <form className="modal" onSubmit={submit} aria-label={initial ? "编辑监控主播" : "添加监控主播"}>
        <div className="modal-header"><div className="modal-title-icon">{initial ? <Pencil size={21} /> : <Plus size={21} />}</div><div><h2>{initial ? "编辑监控主播" : "添加监控主播"}</h2><p>{initial ? "修改后会重新检查直播状态。" : "保存后会自动执行第一次直播状态检查。"}</p></div><button type="button" className="icon-button" onClick={onClose} aria-label="关闭"><X size={19} /></button></div>
        <div className="modal-body">
          <label htmlFor="streamer-name">主播名称<input id="streamer-name" aria-label="主播名称" autoFocus value={name} onChange={(event) => setName(event.target.value)} placeholder="个人主页可留空，自动使用主页昵称" /><small>个人主页可留空；直播间直连必须填写。</small></label>
          <label htmlFor="source-url">个人主页或直播间链接<input id="source-url" aria-label="个人主页或直播间链接" value={sourceUrl} onChange={(event) => setSourceUrl(event.target.value)} placeholder="https://www.douyin.com/user/... 或 https://live.douyin.com/..." /><small>仅访问无需登录的公开页面，不使用 Cookie、登录或验证码绕过。</small></label>
          <section className="tag-editor" aria-label="主播标签编辑器">
            <div className="tag-editor-heading">
              <div><strong><Tag size={16} />主播标签</strong><small>最多 10 个；顺序代表未来切片上下文的优先级。</small></div>
              <button type="button" className="secondary-button compact" disabled={tags.length >= 10} onClick={() => addTag()}><Plus size={15} />添加标签</button>
            </div>
            {suggestions.length > 0 && (
              <div className="tag-suggestions"><span>已有名称建议</span>{suggestions.map((suggestion) => <button type="button" key={suggestion.toLocaleLowerCase()} aria-label={`使用标签建议 ${suggestion}`} disabled={tags.length >= 10} onClick={() => addTag(suggestion)}>{suggestion}</button>)}</div>
            )}
            {tags.length === 0 ? (
              <p className="tag-editor-empty">暂未设置标签，可添加“带货”“搞笑”等内容类型。</p>
            ) : (
              <div className="tag-editor-list">
                {tags.map((tag, index) => (
                  <article className="tag-editor-item" key={index}>
                    <div className="tag-priority">{index + 1}</div>
                    <div className="tag-fields">
                      <label>标签名称<input aria-label={`标签 ${index + 1} 名称`} value={tag.name} maxLength={24} onChange={(event) => updateTag(index, { name: event.target.value })} placeholder="例如：带货" /><small>{[...tag.name].length}/24</small></label>
                      <label>Prompt 指导（可选）<textarea aria-label={`标签 ${index + 1} 指导`} value={tag.promptGuidance ?? ""} maxLength={500} onChange={(event) => updateTag(index, { promptGuidance: event.target.value || null })} placeholder="例如：重点提取商品卖点、价格与购买理由" /><small>{[...(tag.promptGuidance ?? "")].length}/500</small></label>
                    </div>
                    <div className="tag-item-actions">
                      <button type="button" aria-label={`上移标签 ${index + 1}`} disabled={index === 0} onClick={() => moveTag(index, -1)}><ArrowUp size={15} /></button>
                      <button type="button" aria-label={`下移标签 ${index + 1}`} disabled={index === tags.length - 1} onClick={() => moveTag(index, 1)}><ArrowDown size={15} /></button>
                      <button type="button" className="danger" aria-label={`删除标签 ${index + 1}`} onClick={() => removeTag(index)}><Trash2 size={15} /></button>
                    </div>
                  </article>
                ))}
              </div>
            )}
            {tags.length >= 10 && <p className="tag-limit">已达到每个主播 10 个标签的上限</p>}
            <p className="tag-reservation-note">标签仅为未来切片上下文预留，本版本不生成 Prompt、不调用 LLM。{"__TAURI_INTERNALS__" in window ? "" : " 浏览器演示模式仅保存到 localStorage，不写入 SQLite。"}</p>
          </section>
          <label className="switch-row boxed"><div><strong>添加后立即监听</strong><small>主页未发现入口时约每 60 秒检查；发现后每 30 秒检查直播间。</small></div><input type="checkbox" checked={monitorEnabled} onChange={(event) => setMonitorEnabled(event.target.checked)} /></label>
          {error && <div className="form-error">{error}</div>}
        </div>
        <div className="modal-footer"><button type="button" className="ghost-button" onClick={onClose}>取消</button><button type="submit" className="primary-button" disabled={submitting}>{submitting ? <LoaderCircle className="spin" size={17} /> : <Radio size={17} />}{initial ? "保存修改" : "保存并监听"}</button></div>
      </form>
    </div>
  );
}
