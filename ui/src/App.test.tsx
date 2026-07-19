import { act, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { App } from "./App";
import type { ClientApi, Streamer, Video } from "./types";

const streamer: Streamer = {
  id: 1,
  name: "小鱼直播间",
  roomUrl: "https://live.douyin.com/452086788686",
  roomId: "7663779905489947411",
  monitorEnabled: true,
  archived: false,
  liveStatus: "offline",
  monitorStatus: "waiting",
  lastCheckedAt: "2026-07-18T20:00:00Z",
  lastError: null,
  currentVideoCount: 0,
  historyVideoCount: 3,
};

function createApi(streamers: Streamer[] = [], videos: Video[] = []): ClientApi {
  return {
    getDashboard: vi.fn().mockResolvedValue({
      streamers,
      activeRecordings: 0,
      currentVideoCount: 0,
    }),
    createStreamer: vi.fn().mockResolvedValue(streamer),
    updateStreamer: vi.fn().mockResolvedValue(streamer),
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
      ffmpegPath: "ffmpeg",
      ffprobePath: "ffprobe",
      notificationsEnabled: true,
      autostartEnabled: false,
    }),
    saveSettings: vi.fn().mockResolvedValue(undefined),
    openVideo: vi.fn().mockResolvedValue(undefined),
    revealVideo: vi.fn().mockResolvedValue(undefined),
    deleteVideo: vi.fn().mockResolvedValue(undefined),
    deleteSession: vi.fn().mockResolvedValue(undefined),
    openLogs: vi.fn().mockResolvedValue(undefined),
    diagnoseEnvironment: vi.fn().mockResolvedValue({ ffmpeg: true, ffprobe: true }),
    requestExit: vi.fn().mockResolvedValue(undefined),
    subscribe: vi.fn().mockResolvedValue(() => undefined),
  };
}

describe("App", () => {
  it("首次启动时显示添加主播空状态", async () => {
    render(<App api={createApi()} />);
    expect(await screen.findByText("还没有监控主播")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "添加主播" })).toBeInTheDocument();
  });

  it("添加主播表单提交名称和链接", async () => {
    const user = userEvent.setup();
    const api = createApi();
    render(<App api={api} />);

    await user.click(await screen.findByRole("button", { name: "添加主播" }));
    await user.type(screen.getByLabelText("主播名称"), "小鱼直播间");
    await user.type(
      screen.getByLabelText("直播间链接"),
      "https://live.douyin.com/452086788686",
    );
    await user.click(screen.getByRole("button", { name: "保存并监听" }));

    expect(api.createStreamer).toHaveBeenCalledWith({
      name: "小鱼直播间",
      roomUrl: "https://live.douyin.com/452086788686",
      monitorEnabled: true,
    });
  });

  it("将录制中的主播排在等待开播之前", async () => {
    const recording = { ...streamer, id: 2, name: "录制主播", liveStatus: "live", monitorStatus: "recording" } as Streamer;
    render(<App api={createApi([streamer, recording])} />);
    const rows = await screen.findAllByTestId("streamer-row");
    expect(rows[0]).toHaveTextContent("录制主播");
  });

  it("AI 剪辑页面只显示规划中", async () => {
    const user = userEvent.setup();
    render(<App api={createApi()} />);
    await user.click(screen.getByRole("button", { name: "AI 剪辑" }));
    expect(screen.getByText("AI 剪辑功能规划中")).toBeInTheDocument();
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
      screen.getByLabelText("直播间链接"),
      "https://live.douyin.com/452086788686",
    );
    await user.click(screen.getByRole("button", { name: "保存并监听" }));

    expect(await screen.findByText("该直播间已经存在")).toBeInTheDocument();
  });
});
