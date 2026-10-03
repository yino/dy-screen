#!/usr/bin/env node
/**
 * Q2 合成（备选实现）：字幕带 PNG 序列 → 全分辨率 RGBA PNG 序列，用 libvips（hyperframes 依赖中的 sharp）。
 * 用法: node compose_band_sharp.mjs --band frames/band_15s --y0 1380 --frames 450 --out frames/comp_sharp --compression 3
 */
import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';
import { fileURLToPath } from 'node:url';
import { createRequire } from 'node:module';

const require = createRequire(import.meta.url);
const sharp = require('/tmp/caption-sandbox2/node_modules/sharp');

const ROOT = path.dirname(fileURLToPath(import.meta.url));
const argv = process.argv.slice(2);
const arg = (n, d) => { const i = argv.indexOf('--' + n); return i >= 0 ? argv[i + 1] : d; };
const band = path.resolve(ROOT, arg('band'));
const y0 = Number(arg('y0', 1380));
const frames = Number(arg('frames'));
const out = path.resolve(ROOT, arg('out'));
const compression = Number(arg('compression', 3));
fs.mkdirSync(out, { recursive: true });

const base = await sharp({ create: { width: 1080, height: 1920, channels: 4, background: { r: 0, g: 0, b: 0, alpha: 0 } } })
  .png({ compressionLevel: 3 }).toBuffer();

const t0 = process.hrtime.bigint();
let bytes = 0;
const sha = [];
for (let i = 0; i < frames; i += 1) {
  const name = `frame_${String(i).padStart(5, '0')}.png`;
  const buf = await sharp(base)
    .composite([{ input: path.join(band, name), top: y0, left: 0 }])
    .png({ compressionLevel: compression })
    .toBuffer();
  fs.writeFileSync(path.join(out, name), buf);
  bytes += buf.length;
  sha.push(`${crypto.createHash('sha256').update(buf).digest('hex')}  ${name}`);
}
const wall = Number(process.hrtime.bigint() - t0) / 1e6;
fs.writeFileSync(path.join(out, 'SHA256SUMS'), sha.join('\n') + '\n');
const rec = {
  method: 'sharp(libvips)-composite', band, band_y0: y0, frames, compression_level: compression,
  composite_wall_ms: Number(wall.toFixed(1)), out_frames: frames,
  composite_ms_per_frame: Number((wall / frames).toFixed(2)),
  out_bytes_per_frame: Math.round(bytes / frames), out_bytes_per_second: Math.round(bytes / (frames / 30)),
};
fs.writeFileSync(path.resolve(ROOT, arg('json', 'results/composite_sharp.json')), JSON.stringify(rec, null, 2));
console.log(JSON.stringify(rec));
