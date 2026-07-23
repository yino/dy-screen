import { beforeEach, describe, expect, it } from "vitest";
import type { ClientApi } from "./types";

let createBrowserApi: () => ClientApi;

beforeEach(async () => {
  const values = new Map<string, string>();
  Object.defineProperty(window, "localStorage", {
    configurable: true,
    value: {
      get length() { return values.size; },
      clear: () => values.clear(),
      getItem: (key: string) => values.get(key) ?? null,
      key: (index: number) => [...values.keys()][index] ?? null,
      removeItem: (key: string) => values.delete(key),
      setItem: (key: string, value: string) => values.set(key, value),
    } satisfies Storage,
  });
  window.localStorage.clear();
  ({ createBrowserApi } = await import("./api"));
});

describe("浏览器演示标签 API", () => {
  it("迁移没有 tags 字段的旧 localStorage 主播为空标签数组", async () => {
    window.localStorage.setItem("dy-screen-streamers", JSON.stringify([{
      id: 1,
      name: "旧主播",
      sourceKind: "room",
      sourceUrl: "https://live.douyin.com/100",
      webRid: "100",
      roomUrl: "https://live.douyin.com/100",
      roomId: "room-100",
      monitorEnabled: true,
      archived: false,
      liveStatus: "offline",
      monitorStatus: "waiting",
      lastCheckedAt: null,
      lastError: null,
      failureCount: 0,
      nextRetryAt: null,
      currentVideoCount: 0,
      historyVideoCount: 0,
    }]));

    const dashboard = await createBrowserApi().getDashboard();

    expect(dashboard.streamers[0].tags).toEqual([]);
  });

  it("模拟标签创建编辑排序建议和未来上下文", async () => {
    const api = createBrowserApi();
    const created = await api.createStreamer({
      name: "演示主播",
      sourceUrl: "https://live.douyin.com/200",
      monitorEnabled: true,
      tags: [
        { name: " 带货 ", promptGuidance: " 商品卖点 " },
        { name: "搞笑", promptGuidance: null },
      ],
    });
    expect(created.tags.map((tag) => tag.name)).toEqual(["带货", "搞笑"]);

    const updated = await api.updateStreamer(created.id, {
      name: created.name,
      sourceUrl: created.sourceUrl,
      monitorEnabled: true,
      tags: [
        { name: "搞笑", promptGuidance: null },
        { name: "带货", promptGuidance: "商品表达" },
      ],
    });
    expect(updated.tags.map((tag) => tag.name)).toEqual(["搞笑", "带货"]);
    expect(await api.listStreamerTagNameSuggestions()).toEqual(["搞笑", "带货"]);
    expect(await api.getStreamerPromptContext(created.id)).toEqual({
      streamerId: created.id,
      streamerName: "演示主播",
      tags: [
        { name: "搞笑", promptGuidance: null, priority: 0 },
        { name: "带货", promptGuidance: "商品表达", priority: 1 },
      ],
    });
  });

  it("拒绝重复标签且不写入部分主播数据", async () => {
    const api = createBrowserApi();

    await expect(api.createStreamer({
      name: "重复标签主播",
      sourceUrl: "https://live.douyin.com/300",
      monitorEnabled: true,
      tags: [
        { name: "Funny", promptGuidance: null },
        { name: " funny ", promptGuidance: null },
      ],
    })).rejects.toThrow("同一主播不能设置重复标签");
    expect((await api.getDashboard()).streamers).toEqual([]);
  });
});
