#!/usr/bin/env node
// 把一个组合 HTML 变成"渲染器实际会服务的页面"，并列出页面内的全部可执行内容。
//
// 用途：任务 0.7「验证默认渲染页面中的可执行内容只有这些随包引擎脚本」的 HTML 层证据。
//
// 原理：`injectInterceptor` 没有导出，所以在系统临时目录里复制一份渲染器包并追加
// `export`，再调用真实的注入函数。**不改动已安装的包**，也不复制 node_modules 之外的东西。
//
// 用法：
//   node emit-page-html.mjs <composition.html> <out-page.html> [--package <hyperframes 包目录>]
//
// 退出码：0 = 断言通过（页面里恰为随包引擎脚本）；1 = 断言失败；2 = 用法/环境错误。

import { createHash } from "node:crypto";
import { cpSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { pathToFileURL } from "node:url";

const EXPECTED_RUNTIME_SHA256 =
  "38592478aad3f515e33b69ecfc11898e338c28a54799bebcd933c18906decd06";
const BOOTSTRAP_ATTR = "data-hyperframes-preview-runtime";

function die(code, message) {
  console.error(message);
  process.exit(code);
}

const argv = process.argv.slice(2);
const positional = [];
let packageDir = null;
for (let index = 0; index < argv.length; index += 1) {
  if (argv[index] === "--package") {
    packageDir = argv[++index];
  } else {
    positional.push(argv[index]);
  }
}
if (positional.length !== 2) {
  die(2, "用法：node emit-page-html.mjs <composition.html> <out-page.html> [--package <dir>]");
}
const [compositionPath, outPath] = positional;
packageDir = resolve(packageDir ?? "node_modules/hyperframes");

const sha256 = (value) => createHash("sha256").update(value).digest("hex");
const countScripts = (html) => (html.match(/<script\b/gi) ?? []).length;

function listScripts(html) {
  const scripts = [];
  const pattern = /<script\b([^>]*)>([\s\S]*?)<\/script>/gi;
  for (let match = pattern.exec(html); match; match = pattern.exec(html)) {
    const attributes = match[1];
    const body = match[2];
    scripts.push({
      attributes: attributes.trim(),
      src: /src\s*=\s*"([^"]*)"/i.exec(attributes)?.[1] ?? null,
      inlineBytes: Buffer.byteLength(body, "utf8"),
      inlineSha256: body.length > 0 ? sha256(body) : null,
    });
  }
  return scripts;
}

// 渲染器一旦读到 HYPERFRAME_RUNTIME_URL，页面就会变成外链脚本，随包哈希不再是唯一来源。
if ((process.env.HYPERFRAME_RUNTIME_URL ?? "").trim() !== "") {
  die(2, `HYPERFRAME_RUNTIME_URL 必须为空，当前为 ${process.env.HYPERFRAME_RUNTIME_URL}`);
}

const composition = readFileSync(compositionPath, "utf8");
// 探针副本必须与安装包同级（即 node_modules 内），否则包自身的依赖（postcss 等）无法解析。
const probeRoot = mkdtempSync(join(dirname(packageDir), "hyperframes-probe-"));
let injected;
let runtime;
try {
  const probePackage = join(probeRoot, "hyperframes");
  cpSync(packageDir, probePackage, { recursive: true });
  const compilerChunk = join(probePackage, "dist", "chunk-E7APSJCK.js");
  writeFileSync(
    compilerChunk,
    `${readFileSync(compilerChunk, "utf8")}\nexport { injectInterceptor, getRuntimeScriptUrl };\n`,
  );
  const compiler = await import(pathToFileURL(compilerChunk).href);
  const runtimeChunk = await import(
    pathToFileURL(join(probePackage, "dist", "chunk-DUILJXNH.js")).href
  );
  runtime = runtimeChunk.getHyperframeRuntimeScript();
  injected = compiler.injectInterceptor(composition, "inline");
} finally {
  rmSync(probeRoot, { recursive: true, force: true });
}

writeFileSync(outPath, injected);

const before = countScripts(composition);
const after = listScripts(injected);
const runtimeBytes = Buffer.byteLength(runtime, "utf8");
const runtimeSha256 = sha256(runtime);
const failures = [];

if (before !== 0) {
  failures.push(`组合文件本身含 ${before} 个 <script>，组 3 必须先拒绝（本函数不做白名单校验）`);
}
if (after.length !== 1) {
  failures.push(`注入后页面含 ${after.length} 个 <script>，期望恰为 1 个随包引擎脚本`);
}
for (const [index, script] of after.entries()) {
  if (script.src !== null) failures.push(`script[${index}] 是外链 src=${script.src}，违反"只用随包代码"`);
  if (!script.attributes.includes(BOOTSTRAP_ATTR)) {
    failures.push(`script[${index}] 缺少 ${BOOTSTRAP_ATTR} 标记：${script.attributes}`);
  }
  if (script.inlineBytes !== runtimeBytes) {
    failures.push(`script[${index}] 内联 ${script.inlineBytes} B，期望 ${runtimeBytes} B`);
  }
}
const receipt = {
  composition: resolve(compositionPath),
  page: resolve(outPath),
  compositionScripts: before,
  pageScripts: after,
  expectedRuntimeBytes: runtimeBytes,
  expectedRuntimeSha256: runtimeSha256,
  runtimeMatchesExpectedProfile: runtimeSha256 === EXPECTED_RUNTIME_SHA256,
  pageBytes: Buffer.byteLength(injected, "utf8"),
  ok: failures.length === 0 && runtimeSha256 === EXPECTED_RUNTIME_SHA256,
  failures,
};
console.log(JSON.stringify(receipt, null, 2));
if (!receipt.ok) die(1, "页面可执行内容断言失败");
