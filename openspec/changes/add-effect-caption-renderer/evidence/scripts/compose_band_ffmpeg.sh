#!/bin/bash
# Q2 合成：字幕带 PNG 序列 → 全分辨率 1080x1920 RGBA PNG 序列（ffmpeg 单趟 overlay）
# 用法: bash compose_band_ffmpeg.sh <band_dir> <y0> <frames> <out_dir> [compression_level]
set -u
cd /tmp/caption-sandbox2
BAND=$1; Y0=$2; FRAMES=$3; OUT=$4; CL=${5:-3}; TAG=${6:-band}
mkdir -p "$OUT" overlay
rm -f "$OUT"/*.png
if [ ! -f overlay/base_transparent_1080x1920.png ]; then
  ffmpeg -y -v error -f lavfi -i "color=c=black@0.0:s=1080x1920:r=30,format=rgba" -frames:v 1 \
    -pix_fmt rgba overlay/base_transparent_1080x1920.png
fi
S=$(python3 -c 'import time;print(int(time.time()*1000))')
ffmpeg -y -v error -loop 1 -framerate 30 -i overlay/base_transparent_1080x1920.png \
  -framerate 30 -start_number 0 -i "$BAND/frame_%05d.png" \
  -filter_complex "[0:v][1:v]overlay=0:${Y0}:format=auto" \
  -frames:v "$FRAMES" -start_number 0 -pix_fmt rgba -compression_level "$CL" "$OUT/frame_%05d.png"
RC=$?
E=$(python3 -c 'import time;print(int(time.time()*1000))')
WALL=$((E-S))
N=$(ls "$OUT"/*.png 2>/dev/null | wc -l | tr -d ' ')
KB=$(du -sk "$OUT" | cut -f1)
python3 - "$BAND" "$Y0" "$FRAMES" "$OUT" "$CL" "$WALL" "$RC" "$N" "$KB" "$TAG" <<'PY'
import json,sys,os
band,y0,frames,out,cl,wall,rc,n,kb,tag=sys.argv[1],int(sys.argv[2]),int(sys.argv[3]),sys.argv[4],int(sys.argv[5]),int(sys.argv[6]),int(sys.argv[7]),int(sys.argv[8]),int(sys.argv[9]),sys.argv[10]
rec={'method':'ffmpeg-overlay','tag':tag,'band_dir':band,'band_y0':y0,'frames':frames,'compression_level':cl,
     'composite_wall_ms':wall,'rc':rc,'out_frames':n,'out_dir_kb':kb,
     'composite_ms_per_frame':round(wall/n,2) if n else None,'out_bytes_per_frame':round(kb*1024/n) if n else None}
json.dump(rec,open(f'/tmp/caption-sandbox2/results/composite_ffmpeg_{tag}_cl{cl}_y{y0}_{n}.json','w'),indent=2,ensure_ascii=False)
print(json.dumps(rec,ensure_ascii=False))
PY
