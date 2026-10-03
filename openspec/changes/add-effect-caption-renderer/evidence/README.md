# 特效字幕沙箱证据（HyperFrames 适用性验证）

本目录保存 `add-effect-caption-renderer` 任务组 0（仓库外沙箱适用性验证）**不可再生的证据**。
沙箱原始位置 `/tmp/caption-sandbox2/`（第二轮）与 `/tmp/caption-sandbox/`（第一轮）**都在系统临时区，重启即失**，因此把关键部分归档到这里。

> 定位：**只读证据与复现基准**。不参与构建、不参与 CI、不参与发行校验，也不是产品依赖。
> 产品侧不允许出现 Node/npm 安装步骤、`/tmp` 路径或开发机路径（任务 10.6）。

## 归档了什么

| 路径 | 内容 |
|---|---|
| `scripts/emit-page-html.mjs` | **工具（本机跑通）**：用真实 `injectInterceptor` 把组合编译成"渲染器实际会服务的页面"，并断言页面内 `<script>` 恰为随包引擎脚本 |
| `scripts/probe-runtime-scripts.mjs` | **工具（已在真实 Chrome 跑通）**：加载上一步的页面，枚举 `document.scripts`、全局符号与插桩痕迹 |
| `runtime-probe-receipt-0.8.96.json` | 上述探测的回执（`ok: true`，页面内 1 个 480,863 B 内联引擎脚本，`__playerReady`/`__renderReady` 均为真） |
| `scripts/render2.mjs` | 第二轮渲染驱动（真实 `hyperframes render --format png-sequence`，Q1） |
| `scripts/bench_hf.sh` | HyperFrames 原生 `png-sequence` 基准脚本 |
| `scripts/run_q2.sh`、`scripts/run_q2_wrap.sh` | 替代方案（只渲染字幕带 + 合成）Q2 驱动 |
| `scripts/compose_band_ffmpeg.sh`、`scripts/compose_band_sharp.mjs` | 带层与底片合成（ffmpeg / sharp 两条实现） |
| `scripts/attrib.mjs`、`scripts/metrics.py`、`scripts/check_alpha.py`、`scripts/verify2.py` | 逐帧属性统计、耗时/体积指标、Alpha 通道与像素核对 |
| `scripts/engine.js` | 沙箱自写的**等价**引擎脚本（`sandbox-engine/1.0.0`），**不是** HyperFrames 真实引擎脚本 |
| `scripts/compositions/*.html` | 四个纯 CSS 组合样本：`baseline` / `emphasis` / `numbers` / `sentence-io` |
| `scripts/package.json` | 沙箱依赖声明（`hyperframes: file:hyperframes-0.8.96.tgz`、`playwright`） |
| `hyperframes-0.8.96-dist-manifest.txt` | 随包渲染器 548 个 dist 文件的 sha256 + 体积清单 |
| `../engine-runtime-inventory.md` | **任务 0.7 交付物**：引擎脚本清单、版本、注入机制与完整性校验设计 |

## 没有归档什么（以及为什么）

| 未归档 | 体积 | 原因 |
|---|---|---|
| `frames/` | 939 MB | 一次性渲染产物，可由脚本重跑再生成 |
| `node_modules/` | 145 MB | 由 `package.json` + `npm pack` 重建 |
| `logs/` | 232 KB | 关键数字已进 `sandbox-feasibility.md` 与 `engine-runtime-inventory.md` |
| `hyperframes-0.8.96.tgz` | 9.6 MB | 可按下方命令重新取得，且 npm 完整性值已记录 |
| `hf-extract/`、`hf-proj/` | 32 MB | 前者是 tarball 解包副本，后者是可重建的渲染工程 |

## 复现

```bash
# 1) 取得并安装渲染器（报告 11.1 记录：约 40 s 下载 + 2 min 安装）
npm_config_cache=.npm-cache npm pack hyperframes@0.8.96 --fetch-timeout=1800000 --fetch-retries=3
npm_config_cache=.npm-cache npm install ./hyperframes-0.8.96.tgz --no-audit --no-fund
./node_modules/.bin/hyperframes --version        # 期望 0.8.96

# 2) 沙箱禁止写 HOME，因此显式指定浏览器与缓存目录
HYPERFRAMES_BROWSER_PATH="/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" \
PUPPETEER_CACHE_DIR=/tmp/caption-sandbox2/.puppeteer-cache \
  ./node_modules/.bin/hyperframes render --format png-sequence ...

# 3) 重建随包清单（约定：每行 sha256␠␠size␠␠path，按 path 排序，行间以 \n 连接后取树哈希）
node --input-type=module -e "
import {createHash} from 'crypto';
import {readFileSync,readdirSync} from 'fs';
import {join,relative} from 'path';
const h=b=>createHash('sha256').update(b).digest('hex');
const root='node_modules/hyperframes', dist=join(root,'dist'), files=[];
(function walk(d){for(const e of readdirSync(d,{withFileTypes:true})){const p=join(d,e.name);
  if(e.isDirectory())walk(p); else if(e.isFile())files.push(p);}})(dist);
files.sort();
const lines=files.map(f=>{const b=readFileSync(f);return h(b)+'  '+String(b.length).padStart(9)+'  '+relative(root,f);});
console.log('files',files.length,'treeSha256',h(lines.join('\n')));
"
# 期望：files 548 treeSha256 f4460819c799f246e7d7089adf79075777917d5ae1a156481a7698ca31f83fd1
# 与 evidence/hyperframes-0.8.96-dist-manifest.txt 逐行 diff 应完全一致（文件头注释行除外）
```

第二轮结论与全部数字见 [../sandbox-feasibility.md](../sandbox-feasibility.md)；本轮归档只是把证据从 `/tmp` 搬到仓库，**没有改动任何结论**。
