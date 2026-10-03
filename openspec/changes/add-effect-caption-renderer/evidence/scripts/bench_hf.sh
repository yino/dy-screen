#!/bin/bash
# Q1：HyperFrames 自身 png-sequence 路径的渲染耗时测量（仓库外沙盒）
# 用法: bash bench_hf.sh <duration_s> <workers> <tag> [extra args...]
set -u
cd /tmp/caption-sandbox2
DUR=$1; WORKERS=$2; TAG=$3; shift 3
COMP=compositions/sentence-io-${DUR}s.html
OUT=pngsmoke/${TAG}
LOG=logs/hf_${TAG}.log
rm -rf "$OUT"; mkdir -p "$OUT" logs

export HYPERFRAMES_SKIP_SKILLS=1
export HYPERFRAMES_BROWSER_PATH="/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
export PUPPETEER_CACHE_DIR=/tmp/caption-sandbox2/.puppeteer-cache
export PRODUCER_TELEMETRY_DISABLED=1

START=$(python3 -c 'import time;print(int(time.time()*1000))')
./node_modules/.bin/hyperframes render hf-proj -c "$COMP" --format png-sequence \
  -o "$OUT" -f 30 --workers "$WORKERS" "$@" > "$LOG" 2>&1
RC=$?
END=$(python3 -c 'import time;print(int(time.time()*1000))')
WALL=$((END-START))
FRAMES=$(ls "$OUT"/*.png 2>/dev/null | wc -l | tr -d ' ')
BYTES=$(du -sk "$OUT" 2>/dev/null | cut -f1)
echo "TAG=$TAG dur=${DUR}s workers=$WORKERS rc=$RC wall_ms=$WALL frames=$FRAMES dir_kb=$BYTES"
python3 - "$TAG" "$DUR" "$WALL" "$FRAMES" "$BYTES" <<'PY'
import json,sys,re
tag,dur,wall,frames,kb=sys.argv[1],int(sys.argv[2]),int(sys.argv[3]),int(sys.argv[4]),int(sys.argv[5])
log=open(f'/tmp/caption-sandbox2/logs/hf_{tag}.log',encoding='utf8',errors='replace').read()
rec={'tag':tag,'duration_s':dur,'wall_ms':wall,'frames':frames,'dir_kb':kb,
     'ratio_wall_over_highlight':round(wall/(dur*1000),3),
     'per_frame_ms':round(wall/frames,2) if frames else None,
     'frames_per_sec':round(frames/(wall/1000),2) if frames else None}
for m in re.finditer(r'\[Render:trace\] (\{.*?\})\n', log):
    try: d=json.loads(m.group(1))
    except Exception: continue
    if d.get('phase')=='capture_disk' and d.get('status')=='end':
        rec['capture_ms']=d.get('elapsedMs'); rec['capture_mode']=d.get('captureMode')
        rec['worker_count']=d.get('workerCount'); rec['stage_duration_ms']=d.get('durationMs')
    if d.get('phase')=='pipeline' and d.get('status')=='checkpoint':
        rec['pipeline_ms']=d.get('totalElapsedMs')
m=re.search(r'rendered in ([\d.]+)s',log)
if m: rec['reported_total_s']=float(m.group(1))
m=re.search(r'([\d.]+) capture · ([\d.]+) compile · ([\d.]+)s? ?extract',log)
if m: rec['capture_reported_s']=float(m.group(1)); rec['compile_reported_s']=float(m.group(2))
json.dump(rec,open(f'/tmp/caption-sandbox2/results/hf_{tag}.json','w'),indent=2,ensure_ascii=False)
print(json.dumps(rec,ensure_ascii=False))
PY
