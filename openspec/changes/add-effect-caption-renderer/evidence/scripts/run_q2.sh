#!/bin/bash
# Q2：字幕带（clip 捕获）+ 合成 → 全分辨率 RGBA 帧序列，并与同口径全屏直捕对比
# 用法: bash run_q2.sh <duration_s>
set -u
cd /tmp/caption-sandbox2
DUR=$1
FRAMES=$((DUR*30))
BAND_Y0=1400
BAND_H=400
mkdir -p results logs frames
echo "=== Q2 ${DUR}s: full-frame 直捕（对照，同第一轮口径） ==="
rm -rf frames/full_${DUR}s
node render2.mjs --comp compositions/sentence-io.html --width 1080 --height 1920 --fps 30 \
  --duration-ms $((DUR*1000)) --out frames/full_${DUR}s --json results/full_${DUR}s.json --sha \
  > logs/full_${DUR}s.log 2>&1
cat results/full_${DUR}s.json | python3 -c "import json,sys;d=json.load(sys.stdin);print(json.dumps({k:d[k] for k in ['frame_count','wall_ms_total','ratio_wall_over_highlight','screenshot_ms_mean','total_ms_per_frame','bytes_per_second']}))"

echo "=== Q2 ${DUR}s: 只捕获字幕带 1080x${BAND_H} @ y0=${BAND_Y0} ==="
rm -rf frames/band_${DUR}s
node render2.mjs --comp compositions/sentence-io.html --width 1080 --height 1920 --fps 30 \
  --duration-ms $((DUR*1000)) --out frames/band_${DUR}s --json results/band_${DUR}s.json --sha \
  --clip 0,${BAND_Y0},1080,${BAND_H} \
  > logs/band_${DUR}s.log 2>&1
cat results/band_${DUR}s.json | python3 -c "import json,sys;d=json.load(sys.stdin);print(json.dumps({k:d[k] for k in ['frame_count','wall_ms_total','ratio_wall_over_highlight','screenshot_ms_mean','total_ms_per_frame','capture_area_ratio','bytes_per_second','capture_only_fps']}))"

echo "=== Q2 ${DUR}s: ffmpeg overlay 合成（compression_level 3 / 1） ==="
for CL in 3 1; do
  bash compose_band_ffmpeg.sh frames/band_${DUR}s ${BAND_Y0} ${FRAMES} frames/comp_cl${CL}_${DUR}s $CL band
done

echo "=== Q2 ${DUR}s: sharp(libvips) overlay 合成（compression_level 3） ==="
rm -rf frames/compsharp_${DUR}s
node compose_band_sharp.mjs --band frames/band_${DUR}s --y0 ${BAND_Y0} --frames ${FRAMES} \
  --out frames/compsharp_${DUR}s --compression 3 --json results/composite_sharp_${DUR}s.json

echo "=== Q2 ${DUR}s: 汇总（含合成后的端到端比值） ==="
python3 - "$DUR" "$FRAMES" "$BAND_Y0" "$BAND_H" <<'PY'
import json,sys
dur,frames,y0,bh=[int(x) for x in sys.argv[1:5]]
def L(p):
    try: return json.load(open(p))
    except Exception: return None
full=L(f'results/full_{dur}s.json'); band=L(f'results/band_{dur}s.json')
c3=L(f'results/composite_ffmpeg_band_cl3_y{y0}_{frames}.json'); c1=L(f'results/composite_ffmpeg_band_cl1_y{y0}_{frames}.json')
cs=L(f'results/composite_sharp_{dur}s.json')
hl=dur*1000
rows={
 'full_frame_direct':{'wall_ms':full['wall_ms_total'],'ratio':full['ratio_wall_over_highlight'],'per_frame_ms':full['total_ms_per_frame']},
 'band_capture_only':{'wall_ms':band['wall_ms_total'],'ratio':band['ratio_wall_over_highlight'],'per_frame_ms':band['total_ms_per_frame'],'capture_area_ratio':band['capture_area_ratio']},
}
for name,c in (('ffmpeg_cl3',c3),('ffmpeg_cl1',c1),('sharp_cl3',cs)):
    if not c: continue
    per=c['composite_ms_per_frame']
    rows['band_plus_'+name]={'band_wall_ms':band['wall_ms_total'],'composite_wall_ms':c['composite_wall_ms'],
        'total_wall_ms':round(band['wall_ms_total']+c['composite_wall_ms'],1),
        'ratio':round((band['wall_ms_total']+c['composite_wall_ms'])/hl,3),
        'per_frame_ms':round(band['total_ms_per_frame']+per,2),
        'out_bytes_per_frame':c['out_bytes_per_frame'],'out_bytes_per_second':c.get('out_bytes_per_second')}
json.dump(rows,open(f'results/q2_summary_{dur}s.json','w'),ensure_ascii=False,indent=2)
print(json.dumps(rows,ensure_ascii=False,indent=2))
PY
echo "=== Q2 ${DUR}s DONE ==="
