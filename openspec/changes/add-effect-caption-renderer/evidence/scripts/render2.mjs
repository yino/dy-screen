#!/usr/bin/env node
/**
 * caption-sandbox2 逐帧渲染驱动（第二轮复测）
 * 在第一轮 render.mjs 基础上增加：
 *   1) `--clip x,y,w,h`：只渲染（捕获）字幕带区域——最终合成仍产出全分辨率 RGBA 帧（见 compose_band.sh）
 *   2) `--sha`：为每帧写 sha256 清单，用于「逐帧确定（同一输入可重现）」验证
 *   3) 每帧成本三段归因（seek / 捕获+PNG 编码 / 落盘），与第一轮 attrib.mjs 同口径
 *
 * 用法：
 *   node render2.mjs --comp compositions/sentence-io.html --width 1080 --height 1920 \
 *        --fps 30 --duration-ms 15000 --out frames/full_15s --json results/full_15s.json [--clip 0,1380,1080,400] [--sha]
 */
import { chromium } from 'playwright';
import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';
import { fileURLToPath } from 'node:url';

const ROOT = path.dirname(fileURLToPath(import.meta.url));
const argv = process.argv.slice(2);
const arg = (n, d) => { const i = argv.indexOf('--' + n); return i >= 0 ? argv[i + 1] : d; };
const has = (n) => argv.includes('--' + n) || argv.some((a) => a.startsWith('--' + n + '='));

const comp = arg('comp', 'compositions/sentence-io.html');
const width = Number(arg('width', 1080));
const height = Number(arg('height', 1920));
const fps = Number(arg('fps', 30));
const durationMs = Number(arg('duration-ms', 15000));
const outDir = path.resolve(ROOT, arg('out', 'frames/out'));
const jsonPath = arg('json', null);
const maxFrames = arg('max-frames', null) ? Number(arg('max-frames')) : Infinity;
const keyframes = (arg('keyframes', '') || '').split(',').filter(Boolean).map(Number);
const shotsDir = path.resolve(ROOT, arg('shots', 'shots'));
const writeSha = has('sha');
const rafSync = (() => { const i = argv.indexOf('--raf-sync'); if (i < 0) return 0; const v = Number(argv[i + 1]); return Number.isFinite(v) && v > 0 ? v : 2; })();
const clipRaw = arg('clip', null);
const clip = clipRaw ? (() => { const p = clipRaw.split(',').map(Number); return { x: p[0], y: p[1], width: p[2], height: p[3] }; })() : null;
// 变体 B：真正的「小画面」捕获——视口高度只等于字幕带高，组合整体位移到带的位置（等效于全画布的该条带）
const wrapTop = arg('wrap-top', null) ? Number(arg('wrap-top')) : null;
const cutHeight = arg('cut-height', null) ? Number(arg('cut-height')) : null;

const CHROME_BIN = process.env.CHROME_BIN ||
  path.join(process.env.HOME, 'Library/Caches/ms-playwright/chromium_headless_shell-1234/chrome-headless-shell-mac-arm64/chrome-headless-shell');

fs.mkdirSync(outDir, { recursive: true });
if (jsonPath) fs.mkdirSync(path.dirname(path.resolve(ROOT, jsonPath)), { recursive: true });
if (keyframes.length) fs.mkdirSync(shotsDir, { recursive: true });

const frameCount = Math.min(Math.ceil((durationMs / 1000) * fps), maxFrames);

const browser = await chromium.launch({
  executablePath: CHROME_BIN,
  args: ['--no-sandbox', '--disable-lcd-text', '--force-color-profile=srgb', '--hide-scrollbars',
    ...(process.env.CHROME_EXTRA_ARGS ? process.env.CHROME_EXTRA_ARGS.split(' ').filter(Boolean) : [])],
});
const chromeVersion = browser.version();
const viewport = cutHeight ? { width, height: cutHeight } : { width, height };
const page = await (await browser.newContext({ viewport, deviceScaleFactor: 1 })).newPage();

const tStart = Date.now();
await page.goto('file://' + path.resolve(ROOT, comp), { waitUntil: 'load' });
if (cutHeight !== null) {
  // 把组合包进一个 1080x1920 的绝对定位层，并整体上移 wrapTop：
  // 布局与全画布渲染完全一致，只是 Chrome 的捕获画面只有带高（小 surface）。
  await page.evaluate(({ w, fullH, top }) => {
    const wrap = document.createElement('div');
    wrap.id = '__hf_band_wrap__';
    wrap.style.cssText = `position:absolute;left:0;top:${-top}px;width:${w}px;height:${fullH}px`;
    while (document.body.firstChild) wrap.appendChild(document.body.firstChild);
    document.body.appendChild(wrap);
    document.documentElement.style.width = `${w}px`;
    document.documentElement.style.height = `${fullH}px`;
    document.documentElement.style.overflow = 'hidden';
    document.body.style.margin = '0';
    document.body.style.width = `${w}px`;
    document.body.style.height = `${fullH}px`;
    document.body.style.overflow = 'hidden';
  }, { w: width, fullH: height, top: wrapTop ?? 0 });
}
await page.addScriptTag({ path: path.join(ROOT, 'engine.js') });
await page.evaluate(() => window.__ENGINE__.ready());
const loadMs = Date.now() - tStart;

const seekMs = [], shotMs = [], writeMs = [];
const shaLines = [];
let bytes = 0;
const t0 = process.hrtime.bigint();
for (let i = 0; i < frameCount; i += 1) {
  const tMs = Math.round((i * 1000) / fps);
  const name = `frame_${String(i).padStart(5, '0')}.png`;
  const p = path.join(outDir, name);
  let s = process.hrtime.bigint();
  await page.evaluate((t) => window.__ENGINE__.seek(t), tMs);
  if (rafSync > 0) {
    // 等 N 帧 rAF：确保 seek 后的这一帧已被合成器完整提交，再截图（消除光栅/绘制竞态）
    await page.evaluate((n) => new Promise((res) => {
      let k = n; const step = () => { k -= 1; if (k <= 0) res(true); else requestAnimationFrame(step); };
      requestAnimationFrame(step);
    }), rafSync);
  }
  seekMs.push(Number(process.hrtime.bigint() - s) / 1e6);
  s = process.hrtime.bigint();
  const opts = { omitBackground: true, type: 'png' };
  if (clip) opts.clip = clip;
  await page.screenshot({ ...opts, path: p });
  shotMs.push(Number(process.hrtime.bigint() - s) / 1e6);
  s = process.hrtime.bigint();
  const buf = fs.readFileSync(p);
  bytes += buf.length;
  if (writeSha) shaLines.push(`${crypto.createHash('sha256').update(buf).digest('hex')}  ${name}`);
  writeMs.push(Number(process.hrtime.bigint() - s) / 1e6);
  if (keyframes.includes(i)) fs.copyFileSync(p, path.join(shotsDir, `${path.basename(comp, '.html')}_f${String(i).padStart(5, '0')}.png`));
}
const wallMs = Number(process.hrtime.bigint() - t0) / 1e6;
const animCount = await page.evaluate(() => (document.getAnimations ? document.getAnimations().length : -1));
await browser.close();

if (writeSha) fs.writeFileSync(path.join(outDir, 'SHA256SUMS'), shaLines.join('\n') + '\n');

const mean = (v) => Number((v.reduce((a, b) => a + b, 0) / v.length).toFixed(2));
const sum = (v) => Number(v.reduce((a, b) => a + b, 0).toFixed(1));
const sorted = [...shotMs].sort((a, b) => a - b);
const result = {
  composition: comp, width, height, fps,
  clip: clip ? `${clip.x},${clip.y},${clip.width},${clip.height}` : null,
  capture_w: clip ? clip.width : width, capture_h: clip ? clip.height : height,
  capture_area_ratio: clip ? Number(((clip.width * clip.height) / (width * height)).toFixed(4))
    : (cutHeight ? Number((cutHeight / height).toFixed(4)) : 1),
  mode: cutHeight !== null ? 'band-viewport' : (clip ? 'band-clip' : 'full-frame'),
  raf_sync: rafSync,
  viewport: `${viewport.width}x${viewport.height}`,
  wrap_top: wrapTop,
  duration_ms_rendered: Math.round((frameCount / fps) * 1000),
  frame_count: frameCount,
  wall_ms_total: Number(wallMs.toFixed(1)),
  wall_ms_load: loadMs,
  ratio_wall_over_highlight: Number((wallMs / durationMs).toFixed(3)),
  wall_fps_effective: Number((frameCount / (wallMs / 1000)).toFixed(2)),
  seek_ms_mean: mean(seekMs), screenshot_ms_mean: mean(shotMs), screenshot_ms_p95: Number(sorted[Math.floor(sorted.length * 0.95)].toFixed(2)),
  write_ms_mean: mean(writeMs), total_ms_per_frame: mean([...seekMs.map((v, i) => v + shotMs[i] + writeMs[i])]),
  capture_only_fps: Number((1000 / mean(shotMs)).toFixed(2)),
  bytes_total: bytes, bytes_per_frame_mean: Math.round(bytes / frameCount),
  bytes_per_second: Math.round(bytes / (frameCount / fps)),
  animations_seeked: animCount,
  chrome_bin: CHROME_BIN, chrome_version: chromeVersion,
  sha_manifest: writeSha ? path.join(path.relative(ROOT, outDir), 'SHA256SUMS') : null,
};
if (jsonPath) fs.writeFileSync(path.resolve(ROOT, jsonPath), JSON.stringify(result, null, 2));
console.log(JSON.stringify(result));
