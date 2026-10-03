#!/usr/bin/env python3
"""A 项（表现力）量化指标：对每个组合的 RGBA 帧序列计算
 - 非透明包围盒（高度/宽度/面积）随时间的均值、标准差、极差比
 - 逐帧平均绝对差（motion energy，仅 RGB 有效像素归一化）
 - 颜色多样性（量化到 5bit/通道后的唯一色数）与色相跨度
 - 非透明像素数（alpha>0）的均值与变异
用法: python3 metrics.py frames/A_xxx [frames/A_yyy ...]
"""
import sys, os, glob, json
import numpy as np
from PIL import Image

def load(path):
    im = Image.open(path).convert("RGBA")
    return np.asarray(im).astype(np.int16)

def metrics(d):
    files = sorted(glob.glob(os.path.join(d, "frame_*.png")))
    frames = [load(f) for f in files]
    n = len(frames)
    alphas, bboxes, areas, colors, hues = [], [], [], [], []
    diffs = []
    prev = None
    for a in frames:
        al = a[..., 3]
        mask = al > 8
        ys, xs = np.nonzero(mask)
        alphas.append(float(al.mean()))
        if len(ys):
            h = ys.max() - ys.min() + 1
            w = xs.max() - xs.min() + 1
            bboxes.append((w, h))
        else:
            bboxes.append((0, 0))
        areas.append(float(mask.sum()))
        q = (a[..., :3][mask] // 32) if mask.any() else np.zeros((1, 3), dtype=np.int16)
        colors.append(len({tuple(v) for v in q.tolist()}))
        if mask.any():
            px = a[..., :3][mask].astype(np.float32)
            mx = px.max(axis=1); mn = px.min(axis=1)
            hues.append(float((mx - mn).mean()))
        else:
            hues.append(0.0)
        if prev is not None:
            m = (prev[..., 3] > 8) | (a[..., 3] > 8)
            if m.any():
                diffs.append(float(np.abs(a[..., :3].astype(np.float32) - prev[..., :3].astype(np.float32))[m].mean()))
            else:
                diffs.append(0.0)
        prev = a
    bh = np.array([b[1] for b in bboxes], dtype=float)
    bw = np.array([b[0] for b in bboxes], dtype=float)
    ar = np.array(areas, dtype=float)
    hmax, hmin = bh.max(), bh[bh > 0].min() if (bh > 0).any() else 0
    amax, amin = ar.max(), ar[ar > 0].min() if (ar > 0).any() else 0
    return {
        "dir": os.path.basename(d.rstrip("/")),
        "frames": n,
        "bbox_h_mean": round(float(bh.mean()), 1),
        "bbox_h_std": round(float(bh.std()), 2),
        "bbox_h_ratio_max_min": round(float(hmax / max(hmin, 1)), 2),
        "bbox_w_ratio_max_min": round(float(bw.max() / max(bw[bw > 0].min(), 1)), 2) if (bw > 0).any() else 0,
        "alpha_area_mean": int(ar.mean()),
        "alpha_area_cv": round(float(ar.std() / max(ar.mean(), 1)), 4),
        "alpha_area_ratio_max_min": round(float(amax / max(amin, 1)), 2),
        "alpha_mean_mean": round(float(np.mean(alphas)), 3),
        "motion_energy_mean": round(float(np.mean(diffs)), 3),
        "motion_energy_max": round(float(np.max(diffs)), 2),
        "colors_unique_mean": round(float(np.mean(colors)), 1),
        "colors_unique_max": int(np.max(colors)),
        "color_spread_mean": round(float(np.mean(hues)), 2),
        "color_spread_std": round(float(np.std(hues)), 2),
    }

if __name__ == "__main__":
    out = [metrics(d) for d in sys.argv[1:]]
    print(json.dumps(out, ensure_ascii=False, indent=2))
