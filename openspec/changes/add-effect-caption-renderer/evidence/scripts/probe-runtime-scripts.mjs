#!/usr/bin/env node
// 在真实浏览器里枚举被渲染页面的 <script>，补齐任务 0.7 的"运行时观测"那一半。
//
// ⚠️ 本脚本**在当前会话中未被执行过**：DSH 文件沙箱不允许 Chrome 初始化
//    （`sandbox initialization failed: Operation not permitted`，Crashpad 需写
//    ~/Library/Application Support）。请在有完整权限的终端运行，并回报回执。
//
// 前置：
//   1) 先用 emit-page-html.mjs 生成"渲染器实际会服务的页面"：
//        node emit-page-html.mjs <composition.html> <page.html> --package <hyperframes 包目录>
//   2) 需要一个可用的 Chromium（系统 Chrome 即可）与 playwright：
//        HYPERFRAMES_BROWSER_PATH="/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" \
//        node probe-runtime-scripts.mjs <page.html>
//
// 断言范围（诚实说明）：
//   - 本脚本证明的是**页面内 <script> 元素**的清单。
//   - 渲染器另有通过 CDP `Page.addScriptToEvaluateOnNewDocument` 注入的插桩代码，
//     它们**不会**出现在 document.scripts 中，其清单来自静态穷举（见
//     ../engine-runtime-inventory.md 第 3.3 节）。本脚本只把它们的运行时痕迹作为
//     informational 输出，不作断言。
//
// 退出码：0 = 断言通过；1 = 断言失败；2 = 用法/环境错误。

import { createHash } from "node:crypto";
import { existsSync, readFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { pathToFileURL } from "node:url";

const EXPECTED_RUNTIME_BYTES = 480863;
const EXPECTED_RUNTIME_SHA256 =
  "38592478aad3f515e33b69ecfc11898e338c28a54799bebcd933c18906decd06";
const AUDIO_FX_BYTES = 38874;
const AUDIO_FX_SHA256 = "eec4e4349b87ee1542b88a32a4a5dd1428a2bc50fda7ad124c27083d126eb426";
const BOOTSTRAP_ATTR = "data-hyperframes-preview-runtime";

function die(code, message) {
  console.error(message);
  process.exit(code);
}

const argv = process.argv.slice(2);
const positional = [];
let allowAudioFx = false;
let waitMs = 8000;
let playwrightDir = null;
for (let index = 0; index < argv.length; index += 1) {
  if (argv[index] === "--allow-audio-fx") allowAudioFx = true;
  else if (argv[index] === "--wait-ms") waitMs = Number(argv[++index]);
  else if (argv[index] === "--playwright") playwrightDir = argv[++index];
  else positional.push(argv[index]);
}
if (positional.length !== 1) {
  die(
    2,
    "用法：node probe-runtime-scripts.mjs <page.html> [--playwright <playwright 包目录>] [--allow-audio-fx] [--wait-ms N]",
  );
}
const pagePath = resolve(positional[0]);
const browserPath = process.env.HYPERFRAMES_BROWSER_PATH;
if (!browserPath) die(2, "必须设置 HYPERFRAMES_BROWSER_PATH 指向 Chromium 可执行文件");

// playwright 不在本仓库依赖里：按 --playwright → 当前工作目录 node_modules → 裸包名 的顺序解析。
async function loadPlaywright() {
  const candidates = [];
  if (playwrightDir) candidates.push(resolve(playwrightDir));
  candidates.push(resolve(process.cwd(), "node_modules", "playwright"));
  for (const candidate of candidates) {
    for (const entry of ["index.mjs", "index.js"]) {
      const file = join(candidate, entry);
      if (existsSync(file)) return import(pathToFileURL(file).href);
    }
  }
  try {
    return await import("playwright");
  } catch {
    die(
      2,
      `找不到 playwright。请用 --playwright <包目录> 指定，或在已安装它的工程目录下运行（当前 cwd=${process.cwd()}）`,
    );
  }
}

// 页面文件里的内联脚本哈希由 Node 侧计算，避免依赖页面内的加密能力。
const sha256 = (value) => createHash("sha256").update(value).digest("hex");
const pageHtml = readFileSync(pagePath, "utf8");
const inlineScripts = [...pageHtml.matchAll(/<script\b([^>]*)>([\s\S]*?)<\/script>/gi)].map(
  (match) => ({ attributes: match[1], body: match[2] }),
);

const { chromium } = await loadPlaywright();

let browser;
try {
  browser = await chromium.launch({
    executablePath: browserPath,
    args: ["--no-sandbox", "--autoplay-policy=no-user-gesture-required"],
    timeout: 30_000,
  });
} catch (error) {
  die(2, `无法启动浏览器（${browserPath}）：${String(error).split("\n")[0]}`);
}
const failures = [];
let receipt;
try {
  const page = await browser.newPage();
  await page.goto(pathToFileURL(pagePath).href, { waitUntil: "domcontentloaded" });
  // 组合没有时间轴时 __playerReady 可能不出现，因此只等待不强制。
  await page
    .waitForFunction("window.__playerReady === true || window.__renderReady === true", {
      timeout: waitMs,
    })
    .catch(() => {});

  const observed = await page.evaluate(() => ({
    scripts: Array.from(document.scripts).map((script) => ({
      src: script.src || null,
      type: script.type || null,
      hasBootstrapAttr: script.hasAttribute("data-hyperframes-preview-runtime"),
      textLength: script.textContent.length,
      head: script.textContent.slice(0, 40),
      tail: script.textContent.slice(-40),
    })),
    globals: {
      hyperframes: typeof window.__hyperframes,
      timelines: typeof window.__timelines,
      playerReady: window.__playerReady === true,
      renderReady: window.__renderReady === true,
      bootstrapped: window.__hyperframeRuntimeBootstrapped === true,
    },
    instrumentation: {
      hfIoTargets: typeof window.__hf_io_targets,
      hfRootProps: typeof window.__HF_ROOT_PROPS__,
      canvasGetContextWrapped: /__hf|hf_/i.test(
        String(HTMLCanvasElement.prototype.getContext).slice(0, 200),
      ),
    },
  }));

  for (const [index, script] of observed.scripts.entries()) {
    const matchesRuntime =
      script.hasBootstrapAttr &&
      script.src === null &&
      script.textLength > 0 &&
      inlineScripts.some((entry) => Buffer.byteLength(entry.body, "utf8") === script.textLength);
    const isAudioFx = allowAudioFx && script.src === null && !script.hasBootstrapAttr;
    if (!matchesRuntime && !isAudioFx) {
      failures.push(`script[${index}] 不是随包引擎脚本：${JSON.stringify(script).slice(0, 200)}`);
    }
    if (script.src !== null) {
      failures.push(`script[${index}] 是外链 src=${script.src}，违反"只用随包代码"`);
    }
  }

  const inlineAuthoritative = inlineScripts.map((script) => {
    const bytes = Buffer.byteLength(script.body, "utf8");
    return {
      hasBootstrapAttr: script.attributes.includes(BOOTSTRAP_ATTR),
      bytes,
      sha256: sha256(script.body),
    };
  });
  if (inlineAuthoritative.length !== observed.scripts.length) {
    failures.push(
      `页面文件含 ${inlineAuthoritative.length} 个 <script>，浏览器里观察到 ${observed.scripts.length} 个（运行时是否动态插入了脚本？）`,
    );
  }
  const runtimeEntry = inlineAuthoritative.find(
    (script) => script.bytes === EXPECTED_RUNTIME_BYTES,
  );
  if (!runtimeEntry || runtimeEntry.sha256 !== EXPECTED_RUNTIME_SHA256) {
    failures.push("页面文件里找不到与随包引擎脚本哈希一致的 480,863 B 内联脚本");
  }

  receipt = {
    page: pagePath,
    browser: browserPath,
    pageInlineScripts: inlineAuthoritative,
    observed,
    expected: {
      runtimeBytes: EXPECTED_RUNTIME_BYTES,
      runtimeSha256: EXPECTED_RUNTIME_SHA256,
      audioFxBytes: AUDIO_FX_BYTES,
      audioFxSha256: AUDIO_FX_SHA256,
      allowAudioFx,
    },
    ok: failures.length === 0,
    failures,
  };
} finally {
  await browser.close();
}

console.log(JSON.stringify(receipt, null, 2));
if (!receipt.ok) die(1, "运行时脚本清单断言失败");
