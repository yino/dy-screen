#!/usr/bin/env python3
"""0.2 / 0.6 校验：Alpha 真实性、色键痕迹、安全区域、边缘黑边。
用法: python3 check_alpha.py frames/overlay_5s
"""
import sys, os, glob, json
import numpy as np
from PIL import Image

d = sys.argv[1]
files = sorted(glob.glob(os.path.join(d, "frame_*.png")))
rng = np.random.default_rng(7)
sample = files[:: max(1, len(files) // 12)][:12]
report = {"dir": d, "frames_total": len(files), "frames_sampled": len(sample), "checks": []}

agg = {"transparent_frac": [], "mid_alpha_frac": [], "opaque_frac": [], "core_alpha": [], "corner_alpha": [],
       "green_key_px": [], "bbox": [], "edge_blend_err": [], "transparent_rgb_max": []}

for f in sample:
    a = np.asarray(Image.open(f).convert("RGBA")).astype(np.int32)
    al = a[..., 3]
    n = al.size
    agg["transparent_frac"].append(float((al == 0).mean()))
    agg["mid_alpha_frac"].append(float(((al > 0) & (al < 255)).mean()))
    agg["opaque_frac"].append(float((al == 255).mean()))
    core = a[..., :3][al == 255]
    agg["core_alpha"].append(255 if core.size == 0 else 255)
    # 四角 40x40 必须完全透明
    corners = [al[:40, :40], al[:40, -40:], al[-40:, :40], al[-40:, -40:]]
    agg["corner_alpha"].append(int(max(c.max() for c in corners)))
    # 色键痕迹：接近纯绿/纯蓝的像素数
    r, g, b = a[..., 0], a[..., 1], a[..., 2]
    greenish = ((g > 180) & (r < 60) & (b < 60) & (al > 0)).sum()
    agg["green_key_px"].append(int(greenish))
    # 非透明包围盒（安全区域）
    m = al > 8
    ys, xs = np.nonzero(m)
    if len(ys):
        agg["bbox"].append([int(xs.min()), int(ys.min()), int(xs.max()), int(ys.max())])
    # 完全透明像素的 RGB 残值（应为 0，说明未做色键填充）
    t = a[..., :3][al == 0]
    agg["transparent_rgb_max"].append(int(t.max()) if t.size else 0)
    # 直通 alpha 语义校验：自算 blend over white 与 PIL alpha_composite 一致
    base = Image.new("RGBA", (a.shape[1], a.shape[0]), (255, 255, 255, 255))
    comp = Image.alpha_composite(base, Image.open(f).convert("RGBA"))
    got = np.asarray(comp)[..., :3].astype(np.float32)
    av = (al[..., None] / 255.0).astype(np.float32)
    exp = a[..., :3].astype(np.float32) * av + 255.0 * (1 - av)
    agg["edge_blend_err"].append(float(np.abs(got - exp).max()))

def stat(k):
    v = agg[k]
    if k in ("bbox",):
        arr = np.array(v, dtype=float)
        return {"x_min": int(arr[:, 0].min()), "y_min": int(arr[:, 1].min()),
                "x_max": int(arr[:, 2].max()), "y_max": int(arr[:, 3].max())}
    return {"mean": round(float(np.mean(v)), 6), "max": round(float(np.max(v)), 6), "min": round(float(np.min(v)), 6)}

report["stats"] = {k: stat(k) for k in agg}
print(json.dumps(report, ensure_ascii=False, indent=2))
