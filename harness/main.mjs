#!/usr/bin/env node
// 零依赖 Harness：stdio JSON-RPC 服务端 + Profile 加载 + 声明式 Skill 工具循环 + 受控 tool.invoke。
//
// 只用 node: 内置模块，不引入任何 npm 依赖；供应链面 = 一个 Node 运行时 + 本仓库源码。
// 状态权威在 Rust：本程序只做编排（读 Profile → 跑 Skill 步骤 → 发 tool.invoke → 收结果），
// 不持 token、不触网、不执行任意代码（Skill 是声明式 JSON 步骤，不是脚本）。
"use strict";

import { createInterface } from "node:readline";
import { readFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const PROTOCOL_VERSION = "1";
const MAX_MESSAGE_BYTES = 1 << 20;

const __dirname = dirname(fileURLToPath(import.meta.url));

function parseArgs(argv) {
  const args = { profile: null, skillsDir: null };
  for (let i = 2; i < argv.length; i += 1) {
    if (argv[i] === "--profile") args.profile = argv[++i];
    else if (argv[i] === "--skills-dir") args.skillsDir = argv[++i];
  }
  return args;
}

const cli = parseArgs(process.argv);
if (!cli.profile) {
  process.stderr.write("missing --profile\n");
  process.exit(2);
}

function loadJson(path, label) {
  try {
    return JSON.parse(readFileSync(path, "utf8"));
  } catch (error) {
    process.stderr.write(`invalid ${label}: ${error.message}\n`);
    process.exit(2);
  }
}

// 1) Profile：只用于协议版本与工具白名单（工具二次授权仍在 Rust 侧）。
const profile = loadJson(cli.profile, "profile");
if (profile.protocolVersion !== PROTOCOL_VERSION) {
  process.stderr.write("profile protocol version mismatch\n");
  process.exit(2);
}
const allowedTools = new Set(profile.tools ?? []);

// 2) Skill 规格：声明式步骤，不是代码。目录缺省为主程序旁的 skills/。
const skillsDir = cli.skillsDir ?? join(__dirname, "skills");
const skillCache = new Map();

function loadSkill(skillId) {
  if (skillCache.has(skillId)) return skillCache.get(skillId);
  const path = join(skillsDir, `${skillId}.skill.json`);
  const skill = loadJson(path, "skill");
  if (skill.id !== skillId) {
    throw new Error(`skill id mismatch: ${skill.id} != ${skillId}`);
  }
  for (const tool of skill.tools ?? []) {
    if (!allowedTools.has(tool)) {
      throw new Error(`skill uses non-allowlisted tool: ${tool}`);
    }
  }
  skillCache.set(skillId, skill);
  return skill;
}

// 3) JSON-RPC 双向通道。
let nextChildId = 1;
let pendingTool = null;

function write(value) {
  const line = JSON.stringify(value);
  if (Buffer.byteLength(line) > MAX_MESSAGE_BYTES) {
    process.stderr.write("message exceeds size limit\n");
    return;
  }
  process.stdout.write(line + "\n");
}

function invokeTool(tool, params) {
  const id = nextChildId;
  nextChildId += 1;
  return new Promise((resolve, reject) => {
    pendingTool = { id, resolve, reject };
    write({ jsonrpc: "2.0", id, method: "tool.invoke", params: { tool, ...params } });
  });
}

// 4) 声明式步骤替换：${input.<path>} / ${steps.<i>.<path>}。
function resolveRef(ref, ctx) {
  const parts = ref.split(".");
  if (parts[0] === "input") return getPath(ctx.input, parts.slice(1));
  if (parts[0] === "steps") {
    const index = Number(parts[1]);
    return getPath(ctx.steps[index], parts.slice(2));
  }
  throw new Error(`bad ref: ${ref}`);
}

function getPath(obj, path) {
  let current = obj;
  for (const key of path) current = current?.[key];
  return current;
}

function substitute(value, ctx) {
  if (typeof value === "string") {
    // 整串就是一个引用：直接返回解析值（undefined → 字段被 JSON 丢弃）。
    const wholeRef = value.match(/^\$\{([^}]+)\}$/);
    if (wholeRef) return resolveRef(wholeRef[1], ctx);
    return value.replace(/\$\{([^}]+)\}/g, (_match, ref) => {
      const resolved = resolveRef(ref, ctx);
      return resolved === undefined ? "" : String(resolved);
    });
  }
  if (Array.isArray(value)) return value.map((item) => substitute(item, ctx));
  if (value && typeof value === "object") {
    const out = {};
    for (const [key, item] of Object.entries(value)) out[key] = substitute(item, ctx);
    return out;
  }
  return value;
}

async function runSkill(skillId, input) {
  const skill = loadSkill(skillId);
  const ctx = { input, steps: [] };
  for (const step of skill.steps ?? []) {
    const params = substitute(step.params ?? {}, ctx);
    const result = await invokeTool(step.tool, params);
    ctx.steps.push(result);
  }
  const result = ctx.steps.length > 0 ? ctx.steps[ctx.steps.length - 1] : null;
  return { result, steps: ctx.steps };
}

async function handleRequest(req) {
  if (req.method !== "run_skill") {
    write({ jsonrpc: "2.0", id: req.id, error: { code: -32601, message: "unknown method" } });
    return;
  }
  try {
    const skillId = req.params?.skill;
    const input = req.params?.input ?? req.params ?? {};
    if (!skillId) throw new Error("missing skill");
    const output = await runSkill(skillId, input);
    write({ jsonrpc: "2.0", id: req.id, result: output });
  } catch (error) {
    write({
      jsonrpc: "2.0",
      id: req.id,
      error: { code: -32000, message: String(error.message ?? error) },
    });
  }
}

const reader = createInterface({ input: process.stdin, crlfDelay: Infinity });
reader.on("line", (line) => {
  if (Buffer.byteLength(line) > MAX_MESSAGE_BYTES) {
    process.stderr.write("inbound message exceeds size limit\n");
    return;
  }
  let message;
  try {
    message = JSON.parse(line);
  } catch (_error) {
    return;
  }
  if (message.method !== undefined) {
    // Rust 发来的请求。
    handleRequest(message);
  } else if (pendingTool && message.id === pendingTool.id) {
    // tool.invoke 的响应。
    const pending = pendingTool;
    pendingTool = null;
    if (message.error) pending.reject(new Error(message.error.message));
    else pending.resolve(message.result ?? null);
  }
});
