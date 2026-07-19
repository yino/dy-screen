import {
  Activity,
  Archive,
  Bot,
  CheckCircle2,
  ChevronRight,
  CircleOff,
  Clock3,
  Download,
  ExternalLink,
  FileVideo2,
  FolderOpen,
  HardDrive,
  LayoutDashboard,
  LoaderCircle,
  Menu,
  MoreHorizontal,
  Pencil,
  Play,
  Plus,
  Radio,
  RefreshCw,
  Save,
  Search,
  Settings,
  Sparkles,
  Square,
  Trash2,
  Video,
  Wifi,
  X,
  Zap,
} from "lucide-react";
import { FormEvent, useCallback, useEffect, useMemo, useRef, useState } from "react";
import type {
  AppSettings,
  ClientApi,
  Dashboard,
  EnvironmentStatus,
  MonitorStatus,
  Streamer,
  Video as VideoItem,
  VideoFilters,
} from "./types";

type Page = "monitor" | "library" | "ai" | "settings";
type HistoryFilters = { search: string; status: string; date: string };

const emptyDashboard: Dashboard = {
  streamers: [],
  activeRecordings: 0,
  currentVideoCount: 0,
};

const monitorPriority: Record<MonitorStatus, number> = {
  recording: 0,
  retrying: 1,
  waiting_resource: 2,
  recording_error: 3,
  waiting: 4,
  paused: 5,
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
      void refreshDashboard();
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
            onOpenVideo={(id) => void action(() => api.openVideo(id))}
            onRevealVideo={(id) => void action(() => api.revealVideo(id))}
          />
        )}

        {page === "library" && (
          <LibraryPage
            videos={historyVideos}
            streamers={dashboard.streamers}
            page={historyPage}
            total={historyTotal}
            filters={historyFilters}
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

        {page === "ai" && <AiPage />}

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
  onOpenVideo,
  onRevealVideo,
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
  onOpenVideo: (id: number) => void;
  onRevealVideo: (id: number) => void;
}) {
  const liveCount = dashboard.streamers.filter((item) => item.liveStatus === "live").length;
  const waitingCount = dashboard.streamers.filter((item) => item.monitorStatus === "waiting").length;
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
              <p>添加公开抖音直播间，应用会自动检查开播并开始录制视频与声音。</p>
              <button className="primary-button" onClick={onAdd}><Plus size={18} />添加主播</button>
            </div>
          ) : (
            <div className="table-wrap">
              <table>
                <thead><tr><th>主播</th><th>直播状态</th><th>监听状态</th><th>最近检查</th><th>视频</th><th><span className="sr-only">操作</span></th></tr></thead>
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

function StreamerRow({ streamer, selected, onSelect, onToggle, onCheck, onEdit, onStop, onArchive }: {
  streamer: Streamer;
  selected: boolean;
  onSelect: () => void;
  onToggle: () => void;
  onCheck: () => void;
  onEdit: () => void;
  onStop: () => void;
  onArchive: () => void;
}) {
  const [menuOpen, setMenuOpen] = useState(false);
  return (
    <tr data-testid="streamer-row" className={selected ? "selected-row" : ""} onClick={onSelect}>
      <td><div className="streamer-cell"><div className="streamer-avatar">{streamer.name.slice(0, 1)}</div><div><strong>{streamer.name}</strong><span>房间 {streamer.roomId.slice(-8)}</span></div></div></td>
      <td><StatusBadge kind={streamer.liveStatus === "live" ? "live" : streamer.liveStatus === "error" ? "error" : "neutral"}>{liveLabels[streamer.liveStatus]}</StatusBadge></td>
      <td><StatusBadge kind={streamer.monitorStatus === "recording" ? "recording" : streamer.monitorStatus === "recording_error" ? "error" : "neutral"}>{monitorLabels[streamer.monitorStatus]}</StatusBadge></td>
      <td><div className="muted-cell"><span>{formatDate(streamer.lastCheckedAt)}</span>{streamer.lastError && <small title={streamer.lastError}>存在异常</small>}</div></td>
      <td><div className="video-count"><strong>{streamer.currentVideoCount}</strong><span>本次</span><i /><strong>{streamer.historyVideoCount}</strong><span>历史</span></div></td>
      <td className="actions-cell" onClick={(event) => event.stopPropagation()}>
        <button className="icon-button" aria-label={streamer.name + "操作"} onClick={() => setMenuOpen(!menuOpen)}><MoreHorizontal size={18} /></button>
        {menuOpen && (
          <div className="action-menu">
            <button onClick={() => { onCheck(); setMenuOpen(false); }}><RefreshCw size={15} />立即检查</button>
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

function SessionPanel({ streamer, videos, onOpen, onReveal }: { streamer: Streamer | null; videos: VideoItem[]; onOpen: (id: number) => void; onReveal: (id: number) => void }) {
  return (
    <aside className="panel session-panel">
      <div className="panel-header compact"><div><p className="section-kicker">CURRENT SESSION</p><h2>本次监听视频</h2></div>{streamer && <StatusBadge kind={streamer.monitorStatus === "recording" ? "recording" : "neutral"}>{monitorLabels[streamer.monitorStatus]}</StatusBadge>}</div>
      {!streamer ? (
        <div className="empty-state"><FileVideo2 size={28} /><h3>请选择主播</h3><p>选择左侧主播后查看本次会话和完成分片。</p></div>
      ) : (
        <>
          <div className="session-streamer"><div className="streamer-avatar large">{streamer.name.slice(0, 1)}</div><div><strong>{streamer.name}</strong><a href={streamer.roomUrl} target="_blank" rel="noreferrer">打开直播间 <ExternalLink size={12} /></a></div></div>
          <div className="session-stats"><div><span>完成分片</span><strong>{videos.length}</strong></div><div><span>累计大小</span><strong>{formatBytes(videos.reduce((total, item) => total + item.sizeBytes, 0))}</strong></div></div>
          <div className="video-list">
            {videos.length === 0 ? (
              <div className="mini-empty"><div className="pulse-ring"><Video size={21} /></div><strong>等待完成分片</strong><p>录制开始后，已正确关闭的 MKV 会显示在这里。</p></div>
            ) : videos.map((video) => (
              <article className="video-item" key={video.id}>
                <div className="video-thumb"><Play size={17} /></div>
                <div className="video-meta"><strong title={video.path}>{fileName(video.path)}</strong><span>{formatDate(video.startedAt)} · {formatBytes(video.sizeBytes)}</span></div>
                <div className="video-actions"><button aria-label="播放视频" onClick={() => onOpen(video.id)}><Play size={15} /></button><button aria-label="定位视频" onClick={() => onReveal(video.id)}><FolderOpen size={15} /></button></div>
              </article>
            ))}
          </div>
          <div className="session-footnote"><CheckCircle2 size={15} /><span>只展示已完成并可安全播放的 MKV 分片</span></div>
        </>
      )}
    </aside>
  );
}

function LibraryPage({ videos, streamers, page, total, filters, onFilter, onFilters, onPage, onOpen, onReveal, onDelete, onDeleteSession }: { videos: VideoItem[]; streamers: Streamer[]; page: number; total: number; filters: HistoryFilters; onFilter: (id?: number) => void; onFilters: (filters: HistoryFilters) => void; onPage: (page: number) => void; onOpen: (id: number) => void; onReveal: (id: number) => void; onDelete: (id: number) => void; onDeleteSession: (sessionId: number) => void }) {
  const totalPages = Math.max(1, Math.ceil(total / 50));
  const sessions = Array.from(videos.reduce((groups, video) => {
    const current = groups.get(video.sessionId) ?? [];
    current.push(video);
    groups.set(video.sessionId, current);
    return groups;
  }, new Map<number, VideoItem[]>()).entries());
  return (
    <div className="page-content">
      <section className="library-hero"><div><p className="section-kicker">RECORDING ARCHIVE</p><h2>所有录制历史都在本机</h2><p>按主播和会话浏览完成分片，使用系统播放器打开 MKV。</p></div><div className="hero-icon"><Download size={29} /></div></section>
      <section className="panel library-panel">
        <div className="filter-bar"><label className="search-field"><Search size={17} /><input value={filters.search} onChange={(event) => onFilters({ ...filters, search: event.target.value })} placeholder="搜索主播或文件名" /></label><select aria-label="按主播筛选" defaultValue="" onChange={(event) => onFilter(event.target.value ? Number(event.target.value) : undefined)}><option value="">全部主播</option>{streamers.map((item) => <option key={item.id} value={item.id}>{item.name}</option>)}</select><select aria-label="按日期筛选" value={filters.date} onChange={(event) => onFilters({ ...filters, date: event.target.value })}><option value="all">全部日期</option><option value="today">今天</option><option value="7days">最近 7 天</option><option value="30days">最近 30 天</option></select><select aria-label="按状态筛选" value={filters.status} onChange={(event) => onFilters({ ...filters, status: event.target.value })}><option value="all">全部状态</option><option value="complete">文件正常</option><option value="missing">文件缺失</option></select><div className="result-count">共 {total} 条</div></div>
        {videos.length === 0 ? (
          <div className="empty-state spacious"><FileVideo2 size={32} /><h3>还没有历史视频</h3><p>主播开播并完成第一个分片后，视频会自动归档到这里。</p></div>
        ) : (
          <div className="library-sessions">{sessions.map(([sessionId, items]) => <section className="library-session" key={sessionId}><header><div><span>{items[0].streamerName}</span><strong>会话 #{sessionId}</strong><small>{items.length} 个分片 · {formatBytes(items.reduce((sum, item) => sum + item.sizeBytes, 0))}</small></div><button className="danger session-delete" aria-label={`删除会话 #${sessionId}`} onClick={() => onDeleteSession(sessionId)}><Trash2 size={15} />删除整个会话</button></header><div className="library-grid">{items.map((video) => <article className="library-card" key={video.id}><div className="library-preview"><Video size={28} /><span className={video.status === "missing" ? "file-state missing" : "file-state"}>{video.status === "missing" ? "文件缺失" : "MKV"}</span></div><div className="library-card-body"><span>{video.streamerName}</span><strong title={video.path}>{fileName(video.path)}</strong><small>{formatDate(video.startedAt)} · {formatBytes(video.sizeBytes)}</small><div><button disabled={video.status === "missing"} onClick={() => onOpen(video.id)}><Play size={15} />打开</button><button onClick={() => onReveal(video.id)}><FolderOpen size={15} />定位</button><button className="danger" onClick={() => onDelete(video.id)}><Trash2 size={15} /></button></div></div></article>)}</div></section>)}</div>
        )}
        {total > 50 && <div className="pagination"><button disabled={page <= 1} onClick={() => onPage(page - 1)}>上一页</button><span>第 {page} / {totalPages} 页</span><button disabled={page >= totalPages} onClick={() => onPage(page + 1)}>下一页</button></div>}
      </section>
    </div>
  );
}

function AiPage() {
  return (
    <div className="page-content ai-page">
      <section className="ai-hero"><div className="ai-orbit"><Bot size={44} /><span /><span /></div><p className="section-kicker">COMING NEXT</p><h2>AI 剪辑功能规划中</h2><p>录制链路稳定后，将基于完成分片构建 ASR、内容理解、高光时刻识别和自动切片流程。</p><div className="ai-tags"><span><Zap size={15} />语音转写</span><span><Sparkles size={15} />高光识别</span><span><Video size={15} />自动切片</span></div></section>
      <section className="workflow-grid"><article><span>01</span><div><strong>完成分片</strong><p>只处理已正确关闭并包含音轨的 MKV。</p></div></article><article><span>02</span><div><strong>ASR 时间轴</strong><p>提取带时间戳的中文转写文本。</p></div></article><article><span>03</span><div><strong>NLP 高光判断</strong><p>识别关键信息、情绪峰值与高互动片段。</p></div></article><article><span>04</span><div><strong>导出短视频</strong><p>把文本时间轴映射回原视频并生成切片。</p></div></article></section>
      <div className="planning-note"><Activity size={19} /><div><strong>本版本不会读取或分析视频内容</strong><p>该入口仅展示规划，不会创建 AI 任务或写入额外业务数据。</p></div></div>
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

function AddStreamerModal({ initial, onClose, onSubmit }: { initial: Streamer | null; onClose: () => void; onSubmit: (input: { name: string; roomUrl: string; monitorEnabled: boolean }) => Promise<void> }) {
  const [name, setName] = useState(initial?.name ?? "");
  const [roomUrl, setRoomUrl] = useState(initial?.roomUrl ?? "");
  const [monitorEnabled, setMonitorEnabled] = useState(initial?.monitorEnabled ?? true);
  const [error, setError] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState(false);
  const submit = async (event: FormEvent) => {
    event.preventDefault();
    setError(null);
    if (!name.trim()) { setError("请输入主播名称"); return; }
    if (!/^https:\/\/live\.douyin\.com\/\d+/.test(roomUrl.trim())) { setError("请输入有效的抖音公开直播间链接"); return; }
    setSubmitting(true);
    try { await onSubmit({ name: name.trim(), roomUrl: roomUrl.trim(), monitorEnabled }); }
    catch (reason) { setError(errorMessage(reason, initial ? "修改主播失败" : "添加主播失败")); setSubmitting(false); }
  };
  return (
    <div className="modal-backdrop" role="presentation" onMouseDown={(event) => { if (event.target === event.currentTarget) onClose(); }}>
      <form className="modal" onSubmit={submit} aria-label={initial ? "编辑监控主播" : "添加监控主播"}>
        <div className="modal-header"><div className="modal-title-icon">{initial ? <Pencil size={21} /> : <Plus size={21} />}</div><div><h2>{initial ? "编辑监控主播" : "添加监控主播"}</h2><p>{initial ? "修改后会重新检查直播状态。" : "保存后会自动执行第一次直播状态检查。"}</p></div><button type="button" className="icon-button" onClick={onClose} aria-label="关闭"><X size={19} /></button></div>
        <div className="modal-body"><label htmlFor="streamer-name">主播名称<input id="streamer-name" aria-label="主播名称" autoFocus value={name} onChange={(event) => setName(event.target.value)} placeholder="例如：小鱼直播间" /></label><label htmlFor="room-url">直播间链接<input id="room-url" aria-label="直播间链接" value={roomUrl} onChange={(event) => setRoomUrl(event.target.value)} placeholder="https://live.douyin.com/452086788686" /><small>仅支持无需登录即可访问的公开直播间。</small></label><label className="switch-row boxed"><div><strong>添加后立即监听</strong><small>未开播时每 30 秒检查一次。</small></div><input type="checkbox" checked={monitorEnabled} onChange={(event) => setMonitorEnabled(event.target.checked)} /></label>{error && <div className="form-error">{error}</div>}</div>
        <div className="modal-footer"><button type="button" className="ghost-button" onClick={onClose}>取消</button><button type="submit" className="primary-button" disabled={submitting}>{submitting ? <LoaderCircle className="spin" size={17} /> : <Radio size={17} />}{initial ? "保存修改" : "保存并监听"}</button></div>
      </form>
    </div>
  );
}
