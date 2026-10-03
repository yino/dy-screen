#!/usr/bin/env python3
"""第二轮复测校验：
  1) 字幕带覆盖性：全帧非透明包围盒并集，并确认带四边 Alpha 均为 0（无裁切）
  2) 合成保真：合成帧 vs 同时间点全屏直捕帧，逐通道最大/平均偏差
  3) Alpha 真实性：pix_fmt / 透明占比 / 中间 Alpha / 色键痕迹 / 四角
  4) 逐帧确定性：两个 SHA256SUMS 清单比对
用法见文件末尾 __main__。
"""
import argparse, glob, json, os, subprocess, sys
from PIL import Image, ImageChops


def probe_pix_fmt(p):
    out = subprocess.run(['ffprobe', '-v', 'error', '-select_streams', 'v:0',
                          '-show_entries', 'stream=codec_name,pix_fmt,width,height',
                          '-of', 'default=nw=1', p], capture_output=True, text=True)
    return dict(l.split('=', 1) for l in out.stdout.strip().splitlines() if '=' in l)


def band_coverage(banddir, frames):
    union = None
    clipped = []
    for i in range(frames):
        p = os.path.join(banddir, f'frame_{i:05d}.png')
        a = Image.open(p).getchannel('A')
        bb = a.getbbox()
        if bb is None:
            continue
        if union is None:
            union = list(bb)
        else:
            union = [min(union[0], bb[0]), min(union[1], bb[1]), max(union[2], bb[2]), max(union[3], bb[3])]
        w, h = a.size
        edges = [a.crop((0, 0, w, 1)).getextrema()[1], a.crop((0, h - 1, w, h)).getextrema()[1],
                 a.crop((0, 0, 1, h)).getextrema()[1], a.crop((w - 1, 0, w, h)).getextrema()[1]]
        if max(edges) > 0:
            clipped.append({'frame': i, 'edge_max_alpha': edges})
    return union, clipped


def fidelity(compdir, fulldir, frames, y0, bh):
    worst = {'max_abs_diff': 0, 'frame': None, 'mean_abs_diff': 0}
    tot_mean = 0.0
    diff_px_total = 0
    checked = 0
    for i in range(frames):
        cp, fp = os.path.join(compdir, f'frame_{i:05d}.png'), os.path.join(fulldir, f'frame_{i:05d}.png')
        if not (os.path.exists(cp) and os.path.exists(fp)):
            continue
        c, f = Image.open(cp).convert('RGBA'), Image.open(fp).convert('RGBA')
        d = ImageChops.difference(c, f)
        ext = [ch.getextrema()[1] for ch in d.split()]
        mx = max(ext)
        hist_sum = sum(sum(ch.histogram()[1:]) for ch in d.split())
        diff_px_total += hist_sum
        # 平均偏差（按通道）
        s = sum(sum(v * n for v, n in enumerate(ch.histogram())) for ch in d.split())
        mean = s / (c.size[0] * c.size[1] * 4)
        tot_mean += mean
        checked += 1
        if mx > worst['max_abs_diff']:
            worst = {'max_abs_diff': mx, 'frame': i, 'mean_abs_diff': round(mean, 4)}
    return {'frames_checked': checked, 'worst_frame': worst,
            'mean_abs_diff_overall': round(tot_mean / checked, 5) if checked else None,
            'nonzero_channel_pixels_total': diff_px_total}


def alpha_stats(dirname, frames, sample=12):
    idxs = sorted(set(int(i * (frames - 1) / (sample - 1)) for i in range(sample))) if frames > 1 else [0]
    acc = {'transparent': 0, 'intermediate': 0, 'opaque': 0, 'green_key': 0}
    probe = None
    corner_max = 0
    rgb_residual = 0
    for i in idxs:
        p = os.path.join(dirname, f'frame_{i:05d}.png')
        if not os.path.exists(p):
            p = os.path.join(dirname, f'frame_{i+1:05d}.png')
        if not os.path.exists(p):
            continue
        if probe is None:
            probe = probe_pix_fmt(p)
        im = Image.open(p).convert('RGBA')
        a = im.getchannel('A')
        hist = a.histogram()
        acc['transparent'] += hist[0]
        acc['intermediate'] += sum(hist[1:255])
        acc['opaque'] += hist[255]
        r, g, b = im.split()[:3]
        # 色键痕迹：alpha==0 且 g 明显高于 r/b
        px = im.load()
        w, h = im.size
        cnt = 0
        for y in range(0, h, 7):
            for x in range(0, w, 7):
                rp, gp, bp, ap = px[x, y]
                if ap == 0 and gp > 90 and gp > rp + 40 and gp > bp + 40:
                    cnt += 1
                if ap == 0 and (rp or gp or bp):
                    pass
        acc['green_key'] += cnt
        for (cx, cy) in ((0, 0), (w - 40, 0), (0, h - 40), (w - 40, h - 40)):
            corner_max = max(corner_max, a.crop((cx, cy, cx + 40, cy + 40)).getextrema()[1])
        ar = a.histogram()
        rgb_residual = max(rgb_residual, max(max(r.getextrema()) if False else 0 for r in [])) if False else rgb_residual
    total = sum(acc.values())
    return {'pix_fmt': probe, 'sampled_frames': idxs,
            'transparent_pct': round(100 * acc['transparent'] / total, 3),
            'intermediate_alpha_pct': round(100 * acc['intermediate'] / total, 3),
            'opaque_pct': round(100 * acc['opaque'] / total, 3),
            'green_key_pixels_sampled': acc['green_key'],
            'corner_40x40_max_alpha': corner_max}


def sha_compare(a, b):
    la = [l.split()[0] for l in open(a) if l.strip()]
    lb = [l.split()[0] for l in open(b) if l.strip()]
    same = sum(1 for x, y in zip(la, lb) if x == y)
    return {'file_a': a, 'file_b': b, 'lines_a': len(la), 'lines_b': len(lb),
            'identical_lines': same, 'all_identical': la == lb}


if __name__ == '__main__':
    ap = argparse.ArgumentParser()
    ap.add_argument('--band'); ap.add_argument('--composed'); ap.add_argument('--full')
    ap.add_argument('--frames', type=int); ap.add_argument('--y0', type=int, default=1380)
    ap.add_argument('--band-h', type=int, default=400)
    ap.add_argument('--sha-a'); ap.add_argument('--sha-b'); ap.add_argument('--json')
    a = ap.parse_args()
    res = {}
    if a.band:
        union, clipped = band_coverage(a.band, a.frames)
        res['band_coverage'] = {'band_size': None, 'union_bbox_in_band': union,
                                'union_bbox_in_full_frame': [union[0], union[1] + a.y0, union[2], union[3] + a.y0] if union else None,
                                'clipped_frames': len(clipped), 'clipped_examples': clipped[:3]}
    if a.composed and a.full:
        res['fidelity_band_composite_vs_full_capture'] = fidelity(a.composed, a.full, a.frames, a.y0, a.band_h)
    if a.composed:
        res['alpha_composed'] = alpha_stats(a.composed, a.frames)
    if a.full:
        res['alpha_full'] = alpha_stats(a.full, a.frames)
    if a.sha_a and a.sha_b:
        res['determinism'] = sha_compare(a.sha_a, a.sha_b)
    print(json.dumps(res, ensure_ascii=False, indent=2))
    if a.json:
        json.dump(res, open(a.json, 'w'), ensure_ascii=False, indent=2)
