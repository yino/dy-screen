#!/bin/bash
# Q2 变体 B：视口高度=字幕带高（真正的小 surface 捕获）+ 合成 → 全分辨率 RGBA 帧序列
# 用法: bash run_q2_wrap.sh <duration_s>
set -u
cd /tmp/caption-sandbox2
DUR=$1; FRAMES=$((DUR*30)); BAND_Y0=1400; BAND_H=400
mkdir -p results logs frames
echo "=== Q2-wrap ${DUR}s: 小 surface 捕获 1080x${BAND_H}（wrap-top=${BAND_Y0}） ==="
rm -rf frames/wrap_${DUR}s
node render2.mjs --comp compositions/sentence-io.html --width 1080 --height 1920 --fps 30 \
  --duration-ms $((DUR*1000)) --out frames/wrap_${DUR}s --json results/wrap_${DUR}s.json --sha \
  --wrap-top ${BAND_Y0} --cut-height ${BAND_H} > logs/wrap_${DUR}s.log 2>&1
cat results/wrap_${DUR}s.json | python3 -c "import json,sys;d=json.load(sys.stdin);print(json.dumps({k:d[k] for k in ['mode','frame_count','wall_ms_total','ratio_wall_over_highlight','screenshot_ms_mean','total_ms_per_frame','capture_area_ratio','capture_only_fps','bytes_per_second']}))"
for CL in 3 1; do
  bash compose_band_ffmpeg.sh frames/wrap_${DUR}s ${BAND_Y0} ${FRAMES} frames/wcomp_cl${CL}_${DUR}s $CL wrap
done
rm -rf frames/wcompsharp_${DUR}s
node compose_band_sharp.mjs --band frames/wrap_${DUR}s --y0 ${BAND_Y0} --frames ${FRAMES} \
  --out frames/wcompsharp_${DUR}s --compression 3 --json results/wcomposite_sharp_${DUR}s.json
python3 - "$DUR" "$FRAMES" "$BAND_H" <<'PY'
import json,sys
dur,frames,bh=[int(x) for x in sys.argv[1:4]]
def L(p):
    try: return json.load(open(p))
    except Exception: return None
w=L(f'results/wrap_{dur}s.json'); hl=dur*1000
rows={'wrap_capture_only':{'wall_ms':w['wall_ms_total'],'ratio':w['ratio_wall_over_highlight'],
      'per_frame_ms':w['total_ms_per_frame'],'capture_area_ratio':w['capture_area_ratio'],
      'screenshot_ms_mean':w['screenshot_ms_mean'],'capture_only_fps':w['capture_only_fps']}}
for name,p in (('ffmpeg_cl3',f'results/composite_ffmpeg_wrap_cl3_y1400_{frames}.json'),
               ('ffmpeg_cl1',f'results/composite_ffmpeg_wrap_cl1_y1400_{frames}.json'),
               ('sharp_cl3',f'results/wcomposite_sharp_{dur}s.json')):
    c=L(p)
    if not c: continue
    rows['wrap_plus_'+name]={'band_wall_ms':w['wall_ms_total'],'composite_wall_ms':c['composite_wall_ms'],
        'total_wall_ms':round(w['wall_ms_total']+c['composite_wall_ms'],1),
        'ratio':round((w['wall_ms_total']+c['composite_wall_ms'])/hl,3),
        'per_frame_ms':round(w['total_ms_per_frame']+c['composite_ms_per_frame'],2),
        'out_bytes_per_frame':c['out_bytes_per_frame'],'out_bytes_per_second':c.get('out_bytes_per_second')}
json.dump(rows,open(f'results/q2wrap_summary_{dur}s.json','w'),ensure_ascii=False,indent=2)
print(json.dumps(rows,ensure_ascii=False,indent=2))
PY
echo "=== Q2-wrap ${DUR}s DONE ==="
