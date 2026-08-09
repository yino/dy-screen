import { describe, expect, it } from "vitest";
import { buildTextDiff } from "./textDiff";

describe("buildTextDiff", () => {
  it("标出中文同音错字的删除与新增", () => {
    expect(buildTextDiff("这个衣服是纯绵的", "这个衣服是纯棉的")).toEqual([
      { kind: "unchanged", text: "这个衣服是纯" },
      { kind: "added", text: "棉" },
      { kind: "removed", text: "绵" },
      { kind: "unchanged", text: "的" },
    ]);
  });

  it("标出补充的标点", () => {
    expect(buildTextDiff("欢迎来到直播间", "欢迎来到直播间。")).toEqual([
      { kind: "unchanged", text: "欢迎来到直播间" },
      { kind: "added", text: "。" },
    ]);
  });

  it("相同文本只返回未变化片段", () => {
    expect(buildTextDiff("保持原文", "保持原文")).toEqual([
      { kind: "unchanged", text: "保持原文" },
    ]);
  });
});
