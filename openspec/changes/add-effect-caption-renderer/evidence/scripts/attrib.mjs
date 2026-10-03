#!/usr/bin/env node
/**
 * 耗时归因实验：把每帧成本拆成 seek / 截图捕获+PNG 编码 / 落盘 三段。
 * 用法: node attrib.mjs --comp compositions/sentence-io.html --width 1080 --height 1920 --fps 30 --frames 150
 */
import { chromium } from 'playwright';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = path.dirname(fileURLToPath(import.meta.url));
const argv = process.argv.slice(2);
const a = (n, d) => { const i = argv.indexOf('--' + n); return i >= 0 ? argv[i + 1] : d; };
const comp = a('comp', 'compositions/sentence-io.html');
const width = Number(a('width', 1080)), height = Number(a('height', 1920));
const fps = Number(a('fps', 30)), frames = Number(a('frames', 150));
const CHROME_BIN = process.env.CHROME_BIN ||
  path.join(process.env.HOME, 'Library/Caches/ms-playwright/chromium_headless_shell-1234/chrome-headless-shell-mac-arm64/chrome-headless-shell');
const outDir = path.join(ROOT, 'frames/attrib');
fs.mkdirSync(outDir, { recursive: true });

const browser = await chromium.launch({ executablePath: CHROME_BIN, args: ['--no-sandbox', '--force-color-profile=srgb'] });
const page = await (await browser.newContext({ viewport: { width, height }, deviceScaleFactor: 1 })).newPage();
await page.goto('file://' + path.resolve(ROOT, comp), { waitUntil: 'load' });
await page.addScriptTag({ path: path.join(ROOT, 'engine.js') });
await page.evaluate(() => window.__ENGINE__.ready());

const seekMs = [], shotBufMs = [], writeMs = [];
let bytes = 0;
for (let i = 0; i < frames; i += 1) {
  const t = Math.round((i * 1000) / fps);
  let s = process.hrtime.bigint();
  await page.evaluate((x) => window.__ENGINE__.seek(x), t);
  seekMs.push(Number(process.hrtime.bigint() - s) / 1e6);
  s = process.hrtime.bigint();
  const buf = await page.screenshot({ omitBackground: true, type: 'png' });
  shotBufMs.push(Number(process.hrtime.bigint() - s) / 1e6);
  s = process.hrtime.bigint();
  fs.writeFileSync(path.join(outDir, `f_${String(i).padStart(5, '0')}.png`), buf);
  writeMs.push(Number(process.hrtime.bigint() - s) / 1e6);
  bytes += buf.length;
}
await browser.close();
const sum = (v) => Number(v.reduce((x, y) => x + y, 0).toFixed(1));
const mean = (v) => Number((v.reduce((x, y) => x + y, 0) / v.length).toFixed(2));
const res = {
  width, height, fps, frames,
  seek_ms_mean: mean(seekMs),
  screenshot_encode_ms_mean: mean(shotBufMs),
  fs_write_ms_mean: mean(writeMs),
  total_ms_per_frame: mean([...seekMs.map((v, i) => v + shotBufMs[i] + writeMs[i])]),
  total_ms: sum([...seekMs.map((v, i) => v + shotBufMs[i] + writeMs[i])]),
  bytes_per_frame: Math.round(bytes / frames),
};
console.log(JSON.stringify(res, null, 2));
fs.writeFileSync(path.join(ROOT, 'results/attrib.json'), JSON.stringify(res, null, 2));
