import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";

import { DEFAULT_LLM_MODEL_ID } from "./types";

/** 读取生产源码：兼容 vitest 以仓库根或 `ui/` 为工作目录两种运行方式。 */
function readUiSource(file: string): string {
  const candidates = [
    resolve(process.cwd(), "ui/src", file),
    resolve(process.cwd(), "src", file),
  ];
  for (const candidate of candidates) {
    try {
      return readFileSync(candidate, "utf8");
    } catch {
      continue;
    }
  }
  throw new Error(`找不到 ${file}（尝试过 ${candidates.join(", ")}）`);
}

/**
 * LLM 默认模型的回归护栏。
 *
 * 背景（2026-10-08 真实运行发现）：客户端只走"直连百炼"这一条调用路径，
 * 但前端 fallback 曾写死为 `"deepseek-chat"`，UI 会把它存进设置，
 * 随后请求被上游以 400（模型不存在）拒绝，表现为含糊的「Provider 请求失败」。
 */
describe("LLM 默认模型", () => {
  it("默认模型必须与当前调用端点（百炼兼容模式）兼容", () => {
    expect(DEFAULT_LLM_MODEL_ID).toBe("qwen-plus");
  });

  it("生产代码的 fallback 不得写死其他厂商的模型名", () => {
    for (const file of ["AiWorkspace.tsx", "OneClickProductPage.tsx", "api.ts"]) {
      const source = readUiSource(file);
      expect(
        source.includes('"deepseek-chat"'),
        `${file} 把 fallback 写死成了 deepseek-chat：请求会被百炼端点以 400 拒绝`,
      ).toBe(false);
    }
  });

  it("provider fallback 必须与数据库 CHECK 约束保持一致", () => {
    // `llm_provider_settings` 用 `CHECK(provider = 'deepseek')` 锁死了这个旧标识
    // （它承载调用路径，不是厂商）。前端 fallback 若写别的值，写库会直接违反约束。
    for (const file of ["AiWorkspace.tsx", "OneClickProductPage.tsx"]) {
      const source = readUiSource(file);
      expect(
        source.includes('provider: llmSettings?.provider ?? "deepseek"'),
        `${file} 的 provider fallback 与数据库约束不一致`,
      ).toBe(true);
    }
  });
});

/**
 * 阶段条的渲染顺序取自 `smartStageLabels` 的**键插入顺序**，而不是后端
 * `AiSmartStage::ALL`。二者一旦漂移，界面会把阶段排错（曾经如此：纠错已经
 * 前置到合辑之前，界面仍显示"合辑草稿 → 文本纠错"）。
 *
 * 这里直接从 Rust 源里解析 `AiSmartStage::ALL` 的顺序，并断言前端字面量一致。
 */
function readRepositoryFile(relative: string): string {
  const candidates = [
    resolve(process.cwd(), relative),
    resolve(process.cwd(), "..", relative),
  ];
  for (const candidate of candidates) {
    try {
      return readFileSync(candidate, "utf8");
    } catch {
      continue;
    }
  }
  throw new Error(`找不到 ${relative}（尝试过 ${candidates.join(", ")}）`);
}

function backendStageOrder(): string[] {
  const source = readRepositoryFile("src-tauri/src/ai/domain.rs");
  const start = source.indexOf("pub const ALL: [Self; 8] = [");
  expect(start, "未找到 AiSmartStage::ALL").toBeGreaterThan(-1);
  const body = source.slice(start, source.indexOf("];", start));
  return [...body.matchAll(/Self::(\w+)/g)].map((match) => match[1].toLowerCase());
}

function frontendStageOrder(): string[] {
  const source = readUiSource("AiWorkspace.tsx");
  const start = source.indexOf("const smartStageLabels: Record<AiSmartStage, string> = {");
  expect(start, "未找到 smartStageLabels").toBeGreaterThan(-1);
  const body = source.slice(start, source.indexOf("};", start));
  return [...body.matchAll(/^\s{2}(\w+):/gm)].map((match) => match[1]);
}

/**
 * 前端与后端的枚举 / 联合类型必须**同名同值**。
 *
 * 二者是各自手写的，没有代码生成，因此极易漂移。历史上 `AiSmartStage` 就漂过：
 * 后端把纠错前置到合辑之前，前端仍是旧顺序；前端还整整漏了 `enhancement`。
 * 界面会因此显示错误的阶段条与状态。
 *
 * 这里逐字段比对两边定义，任何一侧多值、少值、或顺序不同都会失败。
 */
function parseFrontendEnums(): Record<string, string[]> {
  const source = readUiSource("types.ts");
  const result: Record<string, string[]> = {};
  const pattern = /export type (Ai\w+) =([^;]+);/g;
  for (const match of source.matchAll(pattern)) {
    const values = [...match[2].matchAll(/"([a-z_0-9]+)"/g)].map((item) => item[1]);
    if (values.length > 0) result[match[1]] = values;
  }
  return result;
}

function toSnakeCase(value: string): string {
  return value.replace(/(?<!^)(?=[A-Z])/g, "_").toLowerCase();
}

function parseBackendEnums(): Record<string, string[]> {
  const source = readRepositoryFile("src-tauri/src/ai/domain.rs");
  const result: Record<string, string[]> = {};
  const pattern = /pub enum (Ai\w+) \{/g;
  for (const match of source.matchAll(pattern)) {
    let depth = 1;
    let index = match.index + match[0].length;
    while (index < source.length && depth > 0) {
      if (source[index] === "{") depth += 1;
      else if (source[index] === "}") depth -= 1;
      index += 1;
    }
    const body = source.slice(match.index + match[0].length, index);
    const values = [...body.matchAll(/^ {4}([A-Z]\w*)\s*[,(]/gm)].map((item) =>
      toSnakeCase(item[1]),
    );
    if (values.length > 0) result[match[1]] = values;
  }
  return result;
}

describe("前后端枚举一致性", () => {
  it("同名枚举必须取值与顺序完全一致", () => {
    const frontend = parseFrontendEnums();
    const backend = parseBackendEnums();
    const shared = Object.keys(frontend).filter((name) => name in backend);
    expect(shared.length, "至少应解析出若干同名枚举").toBeGreaterThan(5);
    for (const name of shared) {
      expect(frontend[name], `${name} 与后端不一致`).toEqual(backend[name]);
    }
  });
});
