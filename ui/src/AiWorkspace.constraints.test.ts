import source from "./AiWorkspace.tsx?raw";
import styles from "./styles.css?raw";
import { describe, expect, it } from "vitest";

describe("AI 工作区约束", () => {
  it("同时提供宽屏左右布局和窄窗口上下布局，并处理长文本", () => {
    expect(styles).toContain(".ai-result-main { min-width: 0; display: grid; grid-template-columns:");
    expect(styles).toContain("@media (max-width: 1180px)");
    expect(styles).toContain(".ai-result-main { grid-template-columns: 1fr; }");
    expect(styles).toContain("@media (max-width: 580px)");
    expect(styles).toContain("overflow-wrap: anywhere");
  });

  it("第一版源码不提供编辑、字幕文件、图形化时间轴或视频渲染入口", () => {
    for (const unsupported of [
      "编辑转写",
      "拆分句段",
      "合并句段",
      "SRT",
      "ASS",
      "字幕样式",
      "波形",
      "多轨道",
      "裁剪视频",
      "渲染视频",
    ]) {
      expect(source).not.toContain(unsupported);
    }
  });
});
