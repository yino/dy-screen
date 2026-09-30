# 特效字幕渲染后端沙盒适用性验证（任务组 0 进入条件）

> 本文件对应 `tasks.md` 任务组 0「适用性验证（进入条件，仓库外沙盒）」。
> 沙盒位于仓库之外：`/tmp/caption-sandbox/`（**仓库内只有本文件与 `tasks.md` 任务组 0 的复选框被修改**）。
> 全部数字为本机实测原始值，复现命令随文给出。

---

## 0. 结论速览

| 项 | 判据 | 实测 | 裁决 |
|---|---|---|---|
| A 表现力 | 纯 CSS 能否明显优于现有固定样式逐字字幕 | 3 类组合的动能量/色相跨度/布局位移均为基线的 **1.6–4.5× / ∞（基线为 0）/ 1.3–32.9×** | **通过** |
| B 渲染耗时 | 渲染耗时 < 高光时长（不得慢于实时） | 1080x1920@30fps 实测比值 **1.015 – 1.061**（>1.0）；720p 为 0.852–0.955；60fps 为 **2.09** | **不通过** |
| C 帧序列体积 | 8 GB 目标设备磁盘基线内 | 1080p **2.46–2.80 MB/s**；90 秒约 **222 MB**（重负载组合上界约 4.0 MB/s） | **可接受** |

**进入条件裁决：不通过。** 触发 `design.md`「迁移计划第 2 步」的退路：**改用替代方案 2（Rust 声明式模板，扩展现有 `fontdue` → RGBA PNG → `ffconcat` → `overlay` 链路）**；同时因为 A 项通过，`design.md` 决策二（表现力不足时才放宽引擎能力）的放宽评审**不需要**触发。是否直接退回替代方案 2 还是先做一轮「捕获路径优化」再复测，见第 8 节的待裁决点。

### 任务组 0 复选框勾选状态（只勾确实做过的）

| 项 | 勾选 | 原因 |
|---|---|---|
| 0.1 | `- [ ]` | 组合渲染、无脚本/无外部引用、页面可执行内容仅引擎脚本**均已验证**（见第 6 节 0.1），但**文本不是真实工程导出的高光文本**（沙盒无工程数据），按字面未满足 |
| 0.2 | `- [x]` | 完整完成（ffprobe `rgba` + Alpha/色键/安全区域逐帧核对） |
| 0.3 | `- [ ]` | **已实测但不达标**：1080x1920@30fps 比值 1.024（>1.0） |
| 0.4 | `- [x]` | 已实测（90 s = 221.8 MB，判为可接受）；「8 GB 设备实际可用磁盘」未直接实测（见 R5/8.2-4） |
| 0.5 | `- [x]` | 720p/1080p 两档耗时与体积曲线均已实测 |
| 0.6 | `- [ ]` | 合成链路、音频 MD5 一致、无损边缘偏差 2/255 **均已验证**，但底片是 `testsrc2` 合成源，**不是真实高光切片**（见 R3） |
| 0.7 | `- [ ]` | 沙盒等价引擎脚本与 npm 元数据（版本/Apache-2.0/`sha512-…`）已记录，但**真实渲染器的引擎脚本清单与哈希未取得**（包体安装超时，见 R1/R6） |
| 0.8 | `- [x]` | 结论已记录：A 通过、B 不通过、C 可接受 → 按迁移计划第 2 步回到替代方案 2 |


---

## 1. 环境与版本

| 项目 | 值 | 获取方式 |
|---|---|---|
| OS | macOS 26.6.2（Build 25G83），Apple M3 Pro，36 GB 内存 | `sw_vers`；`sysctl -n hw.memsize` = 38654705664 |
| Node / npm | v26.0.0 / 11.12.1 | `node -v`、`npm -v` |
| 测试机上 Playwright | 1.63.0（`npm install playwright --no-audit --no-fund`，`PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD=1`） | `node -e "require('playwright/package.json').version"` |
| 无头浏览器 | `chrome-headless-shell 151.0.7922.34`（来自既有缓存 `~/Library/Caches/ms-playwright/chromium_headless_shell-1234/`，**未重新下载浏览器**） | `chrome-headless-shell --version`；Playwright `browser.version()` |
| 备选浏览器（已存在，未使用） | Google Chrome for Testing 151.0.7922.34（`chromium-1234`）、系统 Google Chrome 154.0.8037.58（`/Applications/Google Chrome.app/Contents/MacOS/Google Chrome`，Playwright 可启动） | 启动探针 |
| FFmpeg / ffprobe | 8.1.2（`/opt/homebrew/bin/ffmpeg`） | `ffmpeg -version` |
| 指标计算 Python | 内置运行时 python3 + numpy 2.3.5 + Pillow 12.3.0 | `load_workspace_dependencies` |
| 磁盘 | `/System/Volumes/Data` 剩余 20 GiB（98% 已用） | `df -h`；沙盒总计 223 MB |

### 渲染器选择与「HyperFrames 未能装入」的说明

- HyperFrames 目标后端在 npm 上可达：`hyperframes@0.8.96`，`license=Apache-2.0`，`engines.node>=22`，
  `dist.integrity=sha512-5MIWpawK9JT4Qmv4tI2Mwx31QEVMqClf8Yi30v0ZSmsDKK6J/GznyxrCvc/iDE94nkb9U/6TGxLw4FwXv4gG3w==`
  （`npm view hyperframes@0.8.96 --json`，命令见第 9 节）。
- **tarball 安装失败**：`npm install hyperframes@0.8.96` 报 `EIDLETIMEOUT ... registry.npmjs.org:443`（约 7 分钟后放弃）。按任务「不要长时间反复重试」的要求，未重试。
- 因此 B/C 的耗时与体积使用**与 HyperFrames 相同的原语**测量：headless Chrome（同一内核系列）+ 逐帧确定性 seek + `page.screenshot(omitBackground:true)` 输出 RGBA PNG 序列。HyperFrames 的 `png-sequence` 输出走的也是「逐帧合成快照 + PNG 编码」，所以**每帧成本量级可信**；但严格说，这不等于对 HyperFrames 二进制本身的实测（见第 8 节风险 R1）。

### 沙盒结构（仓库外）

```
/tmp/caption-sandbox/
├── engine.js                 # 渲染器注入的引擎脚本（拥有时间轴，按毫秒 seek）
├── compositions/             # 组合：emphasis / numbers / sentence-io / baseline
├── render.mjs                # 逐帧渲染驱动（seek → 截图 → 落盘，输出 JSON 指标）
├── attrib.mjs                # 每帧成本归因（seek / 截图编码 / 落盘）
├── metrics.py                # A 项量化指标
├── check_alpha.py            # Alpha 真实性 / 色键 / 安全区域校验
├── run_bc.sh                 # B/C 全量测量脚本
├── shots/                    # 关键帧截图（7 帧/组合，共 28 张）
├── frames/                   # 帧序列（测量后按配置删除）
├── overlay/                  # ffmpeg 合成产物
└── results/*.json            # 全部原始数据
```

---

## 2. 测试方法

**渲染循环（`render.mjs`，与 HyperFrames 的「引擎脚本拥有时间轴」口径一致）**

1. 加载组合 `file://.../compositions/<x>.html`（**组合是纯 HTML/CSS，不含任何脚本**）；
2. 由渲染器注入 `engine.js`（`addScriptTag`），其 `seek(tMs)` 对 `document.getAnimations()` 逐个
   `pause()` 并设 `currentTime = tMs` —— 逐帧确定性，不依赖真实时钟、不用 sleep；
3. `page.screenshot({omitBackground:true, type:'png'})` 输出透明背景 RGBA PNG；
4. 记录每帧墙钟、总墙钟、字节数、页面发出的全部请求、DOM 中 `<script>` 数量。

**A 项额外**：15 fps × 6 s = 90 帧/组合，1080x1920；用 `metrics.py` 计算非透明包围盒、Alpha 面积、逐帧平均绝对差（motion energy）、颜色多样性、色相跨度。
**B/C 项**：30 fps（另测一档 60 fps），1080x1920 与 720x1280，时长 5/15/30/40/90 s；每配置渲染完记录 JSON 后立即删除帧序列。

---

## 3. A 项：纯 CSS 组合的表现力上限

### 3.1 方法
4 个组合同一驱动、同一分辨率（1080x1920）、同一帧率（15 fps）、同一 6 s 时间轴：

| 组合 | 类型 | 说明 |
|---|---|---|
| `emphasis.html` | 强调词缩放/位移 | 整句逐字入场 + 强调词「，冲」放大 1.42×、上移 26px、颜色由白→黄→红、下划线扫入 + 无限呼吸光晕 |
| `numbers.html` | 数字高亮 | 「99」放大到 1.46×、渐变文字填充、环形描边弹出、「限时」角标旋转入场 |
| `sentence-io.html` | 逐句进出场 | 4 句 12 s 循环，分别左滑/上浮/缩放/字距展开进出，含模糊与斜切 |
| `baseline.html` | **基线**：现有固定样式逐字字幕 | 复刻 `src-tauri/src/ai/clip_subtitle.rs`：白色填充 + 4px 黑描边 + 阴影，字号固定（`video_height*0.045` clamp 64px），逐字出现，无缩放/位移/颜色/模糊 |

### 3.2 原始数据（`results/A_metrics.json`）

| 指标 | emphasis | numbers | sentence-io | **baseline** |
|---|---|---|---|---|
| 帧数 | 90 | 90 | 90 | 90 |
| 包围盒高度均值 (px) | 226.7 | 240.9 | 189.0 | 70.7 |
| 包围盒高度标准差 | 13.7 | 87.3 | 62.9 | 7.8 |
| **包围盒高 极差比 (max/min)** | **1.27** | **3.89** | **32.88** | 1.09 |
| Alpha 面积 变异系数 | 0.122 | 0.385 | 0.380 | 0.311 |
| Alpha 面积 极差比 | 2.27 | 10.78 | 15.32 | 5.37（仅逐字出现） |
| **motion energy 均值** | **6.42** | **17.83** | **17.22** | **3.94** |
| motion energy 峰值 | 53.2 | 101.0 | 163.3 | 136.7 |
| **唯一量化色数均值** | **67.9** | **80.6** | **47.4** | **7.9** |
| 唯一量化色数峰值 | 98 | 114 | 61 | 8 |
| **色相跨度均值** | **112.96** | **79.15** | **72.91** | **0.00** |
| 色相跨度标准差 | 24.25 | 37.22 | 34.86 | 0.00 |

### 3.3 关键帧截图（`/tmp/caption-sandbox/shots/`，共 28 张）

- `emphasis_f00000/15/30/45/60/75/89.png`：f15–f45 可见强调词由原位放大上移、下划线扫入、色相由黄转红；f60 后回落收束。
- `numbers_f00015/30/45.png`：数字放大 + 环形描边出现；f45–f60「限时」角标旋入。
- `sentence-io_f00000/15/30/45/60/75.png`：每 3 s 换句，可见左滑/上浮/缩放/字距展开四种不同进场，句间有明显空档（对应进出场留白）。
- `baseline_f00000/15/30/45/60/75/89.png`：字号、位置、颜色全程不变，只有字符逐个出现；f00 与 f89 除字数外无差异。

### 3.4 结论（A 项：通过）

- 三类纯 CSS 组合在**颜色多样性（7.9 → 47–81 种量化色）**、**色相变化（0 → 73–113）**、**布局位移（包围盒高极差比 1.09 → 1.27/3.89/32.88）**、**动能量（3.94 → 6.42/17.22/17.83）** 上一致且显著优于基线；基线在色相与颜色维度上是退化的（纯白 + 黑描边），特效组合引入了基线结构上做不到的「缩放 + 位移 + 色相 + 描边/角标 + 模糊进出场」。
- 三种类型（强调词、数字、逐句进出场）均可在**无脚本**条件下用 CSS `@keyframes` + 变换实现，且可被外部按毫秒精确 seek（确定性渲染的前提成立）。
- 因此 `design.md` 决策二「表现力不足才放宽引擎能力」的放宽评审**不触发**。
- 局限：样例文本是代表性抖音高光文案（「这个价格真的绷不住了」「到手只要 99 元」「库存只剩最后一批」「点下面直接抢」），**不是**从某个真实工程数据库导出的高光文本（沙盒无工程数据）；组合类型覆盖 3 类，未覆盖粒子/3D/逐笔迹等高阶效果。

---

## 4. B 项：单条高光的渲染耗时与高光时长比值

### 4.1 原始数据（`results/BC_*.json`、`results/attrib.json`）

| 配置 | 帧数 | 总墙钟 (s) | 每帧均值 (ms) | 每帧 p95 (ms) | 有效 fps | **比值 = 墙钟/高光时长** | 每秒体积 |
|---|---|---|---|---|---|---|---|
| 720x1280 @30fps 5s | 150 | 4.77 | 31.80 | 42.27 | 31.4 | 0.955 | 2.60 MB/s |
| 720x1280 @30fps 15s | 450 | 13.40 | 29.75 | 42.01 | 33.6 | 0.893 | 2.34 MB/s |
| 720x1280 @30fps 40s | 1200 | 34.08 | 28.38 | 41.45 | 35.2 | 0.852 | 2.27 MB/s |
| **1080x1920 @30fps 5s** | 150 | 5.31 | 35.36 | 42.99 | 28.3 | **1.061** | 2.80 MB/s |
| **1080x1920 @30fps 15s** | 450 | 15.76 | 35.01 | 42.60 | 28.6 | **1.051** | 2.56 MB/s |
| **1080x1920 @30fps 30s** | 900 | 30.93 | 34.35 | 42.50 | 29.1 | **1.031** | 2.49 MB/s |
| **1080x1920 @30fps 40s** | 1200 | 40.94 | 34.10 | 42.55 | 29.3 | **1.024** | 2.49 MB/s |
| **1080x1920 @30fps 90s** | 2700 | 91.36 | 33.82 | 42.61 | 29.6 | **1.015** | 2.46 MB/s |
| 1080x1920 @**60fps** 15s | 900 | 31.36 | 34.82 | 42.73 | 28.7 | **2.090** | 5.13 MB/s |
| 1080x1920 @30fps 5s（emphasis，重负载） | 150 | 6.31 | 42.04 | 49.82 | 23.8 | **1.262** | 4.01 MB/s |
| 1080x1920 @30fps 15s（emphasis，重负载） | 450 | 15.18 | 33.71 | 49.07 | 29.7 | **1.012** | 2.55 MB/s |

每帧成本归因（1080x1920@30fps，150 帧，`results/attrib.json`）：

| 阶段 | 每帧均值 |
|---|---|
| `engine.seek()`（设置所有动画 currentTime） | **0.56 ms** |
| `page.screenshot()`（合成器捕获 + PNG 编码，含取回 Buffer） | **36.04 ms** |
| `fs.writeFileSync`（落盘） | **0.31 ms** |
| 合计 | 36.91 ms |

分辨率敏感性：540x960 ≈ 24.4 ms/帧、720x1280 ≈ 28–32 ms/帧、1080x1920 ≈ 34–36 ms/帧 —— **耗时随像素数增长远低于线性**，说明成本主要由「每帧一次的截图/编码固定开销」支配，而不是填充率。

### 4.2 结论（B 项：不通过）

- 判据是「渲染耗时 < 高光时长」。1080x1920@30fps 下实测比值 **1.015 – 1.061**，即**恰好慢于实时**；60 fps 下比值 **2.09**（吞吐上限约 28–30 fps，与请求帧率无关）。
- 重负载组合（多层光晕 + 模糊）5 s 档达到 **1.262**。
- 只有 720x1280 档（0.852–0.955）勉强快于实时，但余量很小，且 1080p 预览/导出规格是本 change 的目标规格。
- 本机是 **M3 Pro / 36 GB**，明显强于目标基线（8 GB 设备，可能是 Intel Mac 或 Windows 笔记本）。因此目标设备上的比值**只会更差**，此结论是乐观侧的上界。
- 每帧 36 ms 中，seek 只占 0.56 ms，**97.6% 花在 Chrome 的截图 + PNG 编码**上；落盘可忽略（0.31 ms）。这意味着优化空间在捕获/编码路径，而不在组合复杂度。

---

## 5. C 项：全分辨率 RGBA 帧序列体积

### 5.1 原始数据

| 规格 | 每秒字节 | 15 s 总占用（实测/外推） | 30 s | 45 s | 60 s | 90 s |
|---|---|---|---|---|---|---|
| 1080x1920 @30fps（实测 5/15/30/40/90 s） | **2.46 – 2.80 MB/s** | 38.5 MB（实测） | 74.6 MB（实测） | ≈112 MB | ≈149 MB | **221.8 MB（实测）** |
| 720x1280 @30fps | 2.27 – 2.60 MB/s | 35.2 MB（实测） | ≈78 MB | ≈105 MB | ≈140 MB | ≈211 MB |
| 1080x1920 @30fps（emphasis 重负载） | 2.55 – 4.01 MB/s | 38.2 MB（实测） | ≈115 MB | ≈160 MB | ≈210 MB | ≈**235 – 360 MB** |

单帧平均字节：1080p 约 82–98 KB（emphasis 约 133 KB，baseline 约 39 KB）。

### 5.2 结论（C 项：可接受）

- 1080p RGBA PNG 帧序列约 **2.5 MB/s**；15–90 秒区间总占用 **约 38 MB → 222 MB**（重负载组合上界约 235–360 MB）。
- 相对 8 GB 设备（macOS 常留 20–40 GB 可用磁盘）单条高光的峰值占用在 **百 MB 量级**，配合 `tasks.md` 7.5/9.3 的分段渲染与用后即清策略可接受；**不建议**同时保留多条高光的全量帧序列。
- 注意：720p 与 1080p 的每秒体积几乎相同（2.3–2.6 MB/s）——PNG 体积由文字/渐变细节而非分辨率支配，所以**降 preview 分辨率省不了多少磁盘**，但能省时间（见 4.1）。
- 本机磁盘当时 98% 已用（剩余 20 GiB），90 s 档实测写入 221.8 MB 未出现压力；**未**在真实 8 GB 设备上验证可用磁盘下限。

---

## 6. 其它验收点

### 0.1 组合不含脚本、页面可执行内容只有引擎脚本
- 静态扫描：`grep -nE "<script|on[a-z]+=|src=|href=|url\(|@import|http://|https://|//cdn" compositions/*.html` → **无匹配**。
- 运行时：渲染期间页面发出的请求只有 `file:///private/tmp/caption-sandbox/compositions/<x>.html`（引擎脚本经 `addScriptTag` 注入，不产生网络请求）；`document.querySelectorAll('script').length = 1`（即注入的引擎脚本本身）。
- 可播放性：帧序列交给 FFmpeg 合成后产出可播放 MP4（见 0.6）。

### 0.2 RGBA 真实 Alpha、不依赖色键（`results/alpha_check.json`，抽样 12 帧）
- `ffprobe`：`codec_name=png, width=1080, height=1920, pix_fmt=rgba`。
- 完全透明像素占 **96.25%**；中间 Alpha（0<a<255）占 **3.73%** —— 存在真实抗锯齿过渡带，不是二值抠图。
- 四角 40x40 区域最大 Alpha = **0**；完全透明像素的 RGB 最大残值 = **0**。
- 近纯绿像素（色键痕迹）计数 = **0**。
- 直通 Alpha 语义校验：用 PIL `alpha_composite` 覆盖到白底，与手算 `rgb*a/255 + 255*(1-a/255)` 的最大偏差 **0.49/255**。
- 安全区域：文字包围盒 x∈[137, 987]、y∈[1466, 1699]，位于 1080x1920 的安全范围内（左右各留 >90 px，距底 221 px）。

### 0.5 720p 与 1080p 曲线（预览规格取值输入）

| 分辨率 | 每帧均值 | 有效 fps | 40 s 比值 | 每秒体积 |
|---|---|---|---|---|
| 720x1280 | 28.4 ms | 35.2 | 0.852 | 2.27 MB/s |
| 1080x1920 | 34.1 ms | 29.3 | 1.024 | 2.49 MB/s |

- 时间收益：720p 比 1080p 快约 **17%**（28.4 vs 34.1 ms/帧）；体积几乎无收益（−9%）。
- 因为固定开销占主导，**降低预览分辨率不能把渲染拉回实时**（720p 也只有 0.85–0.96 的比值）。

### 0.6 FFmpeg 覆盖合成（`overlay/`）
- 底片：`testsrc2=1080x1920@30`（5 s）+ 440 Hz 正弦音轨 → H.264 `yuv420p`。
  **说明**：沙盒内没有真实高光切片，底片用合成源替代（限制见风险 R3）。
- 合成：`overlay=0:0:format=auto` → `out_effect_caption.mp4`；`ffprobe`：视频 h264 1080x1920 `yuv420p` 30fps `nb_frames=150`，音频 aac，`duration=5.000000`。
- 音画同步：输出与底片的音频流 MD5 **完全一致**（`09e396fd3aa35a2463d333cd209ba4b3`），音频被原样保留。
- 透明边缘无黑边：无损编码（`-qp 0 -pix_fmt yuv444p`）下，输出帧与「PNG 按直通 Alpha 覆盖在解码底帧上」的期望值**最大偏差 2/255**（含 67830 个抗锯齿边缘像素）。有损 H.264 下边缘带平均偏差 5.1/255、偏暗与偏亮比例对称（9.8% vs 7.2%），属编码振铃，**不是黑边**。
- 中文字形：由系统 `PingFang SC` 渲染（本 change 5.1 要求的「随包锁定字体」尚未纳入，见风险 R4）。

### 0.7 引擎脚本清单与完整性校验方式
- 沙盒等价物的引擎脚本：`engine.js`，版本字符串 `sandbox-engine/1.0.0`，
  `sha256 = f3aa69d57bd34271455644c08b085a2a5db6d3207877a56a77dbcdf240c56389`。
- 组合哈希（供复现核对）：`baseline.html c020fabe…`、`emphasis.html d0cdc3b0…`、`numbers.html 3e0e3d4a…`、`sentence-io.html e5fbc87b…`（完整值见第 9 节命令输出）。
- 真实渲染器侧：`hyperframes@0.8.96`、`license=Apache-2.0`、`engines.node>=22`、
  完整性 `sha512-5MIWpawK9JT4Qmv4tI2Mwx31QEVMqClf8Yi30v0ZSmsDKK6J/GznyxrCvc/iDE94nkb9U/6TGxLw4FwXv4gG3w==`（npm registry 元数据，可作钉版本与校验输入）。
- **未完成**：HyperFrames 包体未能下载（`EIDLETIMEOUT`），因此**没有**取得其实际注入的引擎脚本清单与哈希，也没有验证生产渲染页面中的可执行内容清单。任务 4.7 的完整性校验设计需要在这一步补齐后才能定稿。

---

## 7. 三个组合样例源码

以下三个文件为完整组合（纯声明式 HTML/CSS，**无脚本、无外部引用**）。基线 `baseline.html` 见 3.1 描述，源码同目录。
渲染器注入的引擎脚本 `engine.js` 不计入组合。

### 7.1 `compositions/emphasis.html`（强调词缩放/位移）

```html
<!DOCTYPE html>
<html lang="zh-CN">
<head>
<meta charset="utf-8"><title>emphasis</title>
<style>
  :root { --ink:#ffffff; --hot:#ffe14d; --accent:#ff3b6b; }
  html,body { margin:0; padding:0; width:100%; height:100%; background:transparent; }
  body { font-family:"PingFang SC","Hiragino Sans GB","Heiti SC",sans-serif; overflow:hidden; }
  .stage { position:absolute; left:0; right:0; bottom:230px; display:flex; justify-content:center; align-items:flex-end; }
  .line { position:relative; display:flex; align-items:baseline; gap:0; font-size:74px; font-weight:800;
          letter-spacing:2px; color:var(--ink); text-shadow:0 6px 18px rgba(0,0,0,.55);
          animation: line-in 6000ms cubic-bezier(.22,.9,.28,1) both; }
  .ch { display:inline-block; animation: ch-in 240ms cubic-bezier(.2,.9,.3,1) both; }
  .c1{animation-delay:0ms}   .c2{animation-delay:70ms}  .c3{animation-delay:140ms}
  .c4{animation-delay:210ms} .c5{animation-delay:280ms} .c6{animation-delay:350ms}
  .c7{animation-delay:420ms} .c8{animation-delay:490ms} .c9{animation-delay:560ms}
  .c10{animation-delay:630ms}
  .emph { position:relative; display:inline-block; margin-left:6px; color:var(--ink);
          animation: emph-pop 6000ms cubic-bezier(.2,1.2,.25,1) both, emph-hue 6000ms linear both; }
  .emph::after { content:""; position:absolute; left:-2%; right:-2%; bottom:-14px; height:14px;
          background:linear-gradient(90deg,var(--accent),var(--hot)); transform-origin:left center;
          border-radius:8px; animation: sweep 6000ms cubic-bezier(.2,.9,.3,1) both; }
  .glow { position:absolute; left:50%; bottom:150px; width:760px; height:360px; transform:translateX(-50%);
          background:radial-gradient(closest-side, rgba(255,59,107,.55), rgba(255,59,107,0) 70%);
          filter:blur(6px); animation: glow-pulse 2400ms ease-in-out infinite; }
  @keyframes line-in { 0%{opacity:0;transform:translateY(60px) scale(.94);} 8%{opacity:1;transform:translateY(0) scale(1);}
          92%{opacity:1;transform:translateY(0) scale(1);} 100%{opacity:0;transform:translateY(-40px) scale(.97);} }
  @keyframes ch-in { 0%{opacity:0;transform:translateY(38px) rotate(6deg);filter:blur(6px);}
          100%{opacity:1;transform:translateY(0) rotate(0deg);filter:blur(0);} }
  @keyframes emph-pop { 0%,28%{transform:scale(1) translateY(0);} 36%{transform:scale(1.42) translateY(-26px);}
          44%{transform:scale(1.18) translateY(-10px);} 52%{transform:scale(1.30) translateY(-18px);}
          62%,100%{transform:scale(1.06) translateY(-4px);} }
  @keyframes emph-hue { 0%,28%{color:#ffffff;text-shadow:0 6px 18px rgba(0,0,0,.55);}
          38%{color:#ffe14d;text-shadow:0 0 34px rgba(255,225,77,.95),0 8px 22px rgba(0,0,0,.6);}
          70%{color:#ff3b6b;text-shadow:0 0 26px rgba(255,59,107,.8),0 8px 22px rgba(0,0,0,.6);}
          100%{color:#ffd7e0;text-shadow:0 6px 18px rgba(0,0,0,.55);} }
  @keyframes sweep { 0%,30%{transform:scaleX(0);opacity:0;} 40%{transform:scaleX(1);opacity:1;}
          88%{transform:scaleX(1);opacity:1;} 100%{transform:scaleX(0);opacity:0;} }
  @keyframes glow-pulse { 0%,100%{opacity:.35;transform:translateX(-50%) scale(.94);}
          50%{opacity:.85;transform:translateX(-50%) scale(1.06);} }
</style>
</head>
<body>
  <div class="glow"></div>
  <div class="stage">
    <div class="line">
      <span class="ch c1">这</span><span class="ch c2">个</span><span class="ch c3">价</span>
      <span class="ch c4">格</span><span class="ch c5">真</span><span class="ch c6">的</span>
      <span class="ch c7">绷</span><span class="ch c8">不</span><span class="ch c9">住</span>
      <span class="ch c10">了</span>
      <span class="emph">，冲</span>
    </div>
  </div>
</body>
</html>
```

### 7.2 `compositions/numbers.html`（数字高亮）

```html
<!DOCTYPE html>
<html lang="zh-CN">
<head>
<meta charset="utf-8"><title>numbers</title>
<style>
  html,body { margin:0; padding:0; width:100%; height:100%; background:transparent; }
  body { font-family:"PingFang SC","Hiragino Sans GB","Heiti SC",sans-serif; overflow:hidden; }
  .stage { position:absolute; left:0; right:0; bottom:230px; display:flex; justify-content:center; align-items:flex-end; }
  .line { display:flex; align-items:baseline; gap:0; font-size:76px; font-weight:800; color:#fff;
          letter-spacing:2px; text-shadow:0 6px 18px rgba(0,0,0,.55);
          animation: pre 6000ms ease-out both; }
  .ch { display:inline-block; animation: kin 6000ms cubic-bezier(.2,.9,.3,1) both; }
  .n1{animation-delay:0ms} .n2{animation-delay:90ms} .n3{animation-delay:180ms}
  .n4{animation-delay:270ms} .n5{animation-delay:360ms}
  .num { position:relative; display:inline-block; margin:0 8px; font-size:112px; line-height:1;
         background:linear-gradient(180deg,#fff3b0,#ffb020 55%,#ff5a2b);
         -webkit-background-clip:text; background-clip:text; color:transparent;
         animation: num-pop 6000ms cubic-bezier(.2,1.25,.25,1) both; }
  .num .ring { position:absolute; inset:-26px -30px -18px -30px; border-radius:32px;
         border:6px solid rgba(255,225,77,.95); box-shadow:0 0 42px rgba(255,176,32,.75);
         animation: ring 6000ms cubic-bezier(.2,.9,.3,1) both; }
  .num .badge { position:absolute; right:-64px; top:-58px; padding:8px 20px 10px; border-radius:999px;
         background:linear-gradient(135deg,#ff3b6b,#ff8a2b); color:#fff; font-size:38px; font-weight:800;
         letter-spacing:1px; box-shadow:0 10px 26px rgba(0,0,0,.45);
         animation: badge 6000ms cubic-bezier(.2,1.2,.25,1) both; }
  .unit { animation: unit 6000ms cubic-bezier(.2,.9,.3,1) both; }
  @keyframes pre { 0%{opacity:0;transform:translateY(52px);} 9%{opacity:1;transform:translateY(0);}
          90%{opacity:1;transform:translateY(0);} 100%{opacity:0;transform:translateY(-34px);} }
  @keyframes kin { 0%{opacity:0;transform:translateY(26px);} 100%{opacity:1;transform:translateY(0);} }
  @keyframes num-pop { 0%,22%{transform:scale(1) rotate(0deg);filter:brightness(1);}
          32%{transform:scale(1.46) rotate(-4deg);filter:brightness(1.35);}
          42%{transform:scale(1.20) rotate(2deg);filter:brightness(1.1);}
          56%,100%{transform:scale(1.28) rotate(0deg);filter:brightness(1.15);} }
  @keyframes ring { 0%,20%{opacity:0;transform:scale(.55);} 32%{opacity:1;transform:scale(1.08);}
          44%{opacity:1;transform:scale(1);} 88%{opacity:.9;transform:scale(1);}
          100%{opacity:0;transform:scale(1.03);} }
  @keyframes badge { 0%,40%{opacity:0;transform:translate(26px,26px) scale(.4) rotate(18deg);}
          52%{opacity:1;transform:translate(0,0) scale(1.12) rotate(-4deg);}
          62%{opacity:1;transform:translate(0,0) scale(1) rotate(0deg);}
          100%{opacity:0;transform:translate(0,-18px) scale(.96) rotate(0deg);} }
  @keyframes unit { 0%,44%{opacity:.5;transform:scale(.96);} 60%,100%{opacity:1;transform:scale(1);} }
</style>
</head>
<body>
  <div class="stage">
    <div class="line">
      <span class="ch n1">到</span><span class="ch n2">手</span><span class="ch n3">只</span><span class="ch n4">要</span>
      <span class="num">99<span class="ring"></span><span class="badge">限时</span></span>
      <span class="unit">元</span>
      <span class="ch n5">起</span>
    </div>
  </div>
</body>
</html>
```

### 7.3 `compositions/sentence-io.html`（逐句进出场，12 s 无限循环）

```html
<!DOCTYPE html>
<html lang="zh-CN">
<head>
<meta charset="utf-8"><title>sentence-io</title>
<style>
  html,body { margin:0; padding:0; width:100%; height:100%; background:transparent; }
  body { font-family:"PingFang SC","Hiragino Sans GB","Heiti SC",sans-serif; overflow:hidden; }
  .stage { position:absolute; left:0; right:0; bottom:250px; display:flex; justify-content:center; }
  .wrap { position:relative; width:980px; height:220px; }
  .s { position:absolute; left:0; right:0; bottom:0; text-align:center; font-size:70px; font-weight:800;
       color:#fff; text-shadow:0 6px 20px rgba(0,0,0,.6); letter-spacing:2px;
       animation-duration:12000ms; animation-timing-function:cubic-bezier(.2,.9,.25,1);
       animation-iteration-count:infinite; animation-fill-mode:both; }
  .s1 { animation-name:io-a; } .s2 { animation-name:io-b; }
  .s3 { animation-name:io-c; } .s4 { animation-name:io-d; }
  .s .hl { color:#ffe14d; text-shadow:0 0 30px rgba(255,225,77,.85),0 6px 20px rgba(0,0,0,.6); }
  .bar { position:absolute; left:50%; bottom:196px; width:760px; height:8px; border-radius:6px;
         transform:translateX(-50%);
         background:linear-gradient(90deg,rgba(255,59,107,.9),rgba(255,225,77,.9));
         animation: bar 4000ms ease-in-out infinite; }
  @keyframes io-a { 0%{opacity:0;transform:translateX(-160px) skewX(9deg);filter:blur(14px);}
          8%{opacity:1;transform:translateX(0) skewX(0);filter:blur(0);}
          22%{opacity:1;transform:translateX(0) scale(1);}
          25%{opacity:0;transform:translateX(120px) scale(.92);filter:blur(10px);}
          100%{opacity:0;transform:translateX(120px) scale(.92);filter:blur(10px);} }
  @keyframes io-b { 0%,25%{opacity:0;transform:translateY(90px) scale(.9);filter:blur(12px);}
          33%{opacity:1;transform:translateY(0) scale(1.04);filter:blur(0);}
          44%{opacity:1;transform:translateY(0) scale(1);}
          50%{opacity:0;transform:translateY(-70px);filter:blur(12px);}
          100%{opacity:0;transform:translateY(-70px);filter:blur(12px);} }
  @keyframes io-c { 0%,50%{opacity:0;transform:scale(1.5) rotate(-3deg);filter:blur(16px);}
          58%{opacity:1;transform:scale(1) rotate(0);filter:blur(0);}
          69%{opacity:1;transform:scale(1);}
          75%{opacity:0;transform:scale(.86) rotate(2deg);filter:blur(10px);}
          100%{opacity:0;transform:scale(.86) rotate(2deg);filter:blur(10px);} }
  @keyframes io-d { 0%,75%{opacity:0;transform:translateX(150px);filter:blur(10px);letter-spacing:14px;}
          83%{opacity:1;transform:translateX(0);filter:blur(0);letter-spacing:2px;}
          92%{opacity:1;transform:translateX(0);}
          100%{opacity:0;transform:translateX(-120px);filter:blur(10px);} }
  @keyframes bar { 0%,100%{opacity:.25;transform:translateX(-50%) scaleX(.9);}
          50%{opacity:.9;transform:translateX(-50%) scaleX(1);} }
</style>
</head>
<body>
  <div class="stage">
    <div class="wrap">
      <div class="s s1">这个价格 <span class="hl">真的</span> 绷不住了</div>
      <div class="s s2">到手只要 <span class="hl">99 元</span></div>
      <div class="s s3">库存只剩 <span class="hl">最后一批</span></div>
      <div class="s s4">点下面 <span class="hl">直接抢</span></div>
      <div class="bar"></div>
    </div>
  </div>
</body>
</html>
```

### 7.4 引擎脚本 `engine.js`（渲染器侧，不属于组合）

```js
(function () {
  window.__ENGINE__ = {
    version: 'sandbox-engine/1.0.0',
    async ready() {
      if (document.fonts && document.fonts.ready) await document.fonts.ready;
      await new Promise((r) => requestAnimationFrame(() => r()));
      return true;
    },
    seek(tMs) {
      const list = document.getAnimations ? document.getAnimations() : [];
      for (const a of list) { try { a.pause(); a.currentTime = tMs; } catch (e) {} }
      document.documentElement.setAttribute('data-seek-ms', String(tMs));
      return list.length;
    },
  };
})();
```

---

## 8. 进入条件裁决与待 Lead 裁决点

### 8.1 裁决：**不通过**

依据 `design.md`「迁移计划第 2 步」与 `tasks.md` 0.8：

1. **B 不达标（决定性）**：目标规格 1080x1920@30fps 实测渲染耗时/高光时长 = **1.015 – 1.061 > 1.0**，即慢于实时；60 fps 为 2.09；重负载组合 5 s 档 1.262。测试机为 M3 Pro/36 GB，目标设备为 8 GB 基线，结论只会更差。
2. **A 达标**：纯 CSS 在颜色、位移、时序上明显优于现有固定样式逐字字幕，故 `design.md` 决策二的「表现力不足才放宽」评审**不触发**；`design.md` 中「目标视觉质量依赖自由编排的时序与合成，声明式模板存在上限」这一未采纳理由**已被本次实测削弱**。
3. **C 达标**：15–90 s 全分辨率 RGBA 帧序列约 38–222 MB（重负载上界约 235–360 MB），配合分段渲染/即用即清可接受。

→ **建议改用替代方案 2：Rust 声明式模板路线**（扩展现有 `fontdue` → RGBA PNG → `ffconcat` → `overlay` 链路），因为：
- 它已经在本仓库内、逐帧确定性、零额外运行时、不推翻归档决策；
- 本次实测表明「纯 CSS 的表现力优势」真实存在（A 通过），但**不足以抵消「慢于实时」这一硬约束**；
- 慢于实时是**每帧固定开销**（Chrome 截图 + PNG 编码 36 ms/帧）造成的，与组合复杂度几乎无关，降预览分辨率也无法回到实时。

### 8.2 需要 Lead 裁决的点

1. **是否直接退回替代方案 2，还是先做一轮「捕获路径优化」复测**（B 的差距只有 1.5–6%）。可选优化：改用 CDP 原始帧/流式捕获、把 PNG 编码移出关键路径、`--disable-gpu`/SwiftShader 对比、用 HyperFrames 二进制实测（本次未装入）。**建议**：若愿意付出 0.5–1 天，值得做一次「HyperFrames 真机 + 替代捕获路径」的窄口径复测；否则直接按替代方案 2 走。
2. **`tasks.md` 任务组 0 的项数与 task-3 描述不一致**：task-3 说「7 行复选框」，实际文件里是 **8 项（0.1–0.8）**。本报告按文件中的 8 项处理。
3. **A 项样例文本来源**：本次用代表性高光文案，不是从真实工程数据库导出。若要求「真实工程文本」，需要提供一个真实工程快照再复跑 A 项（预计 <10 分钟）。
4. **8 GB 目标设备的磁盘/内存基线未直接实测**：本机 36 GB，内存采样还受沙盒限制（`ps` 被拒，只能用 `os.freemem()` 的系统级下限，噪声大，不构成渲染器常驻内存结论）。若 B/C 需要设备级结论，需在目标机型上复跑。

---

## 9. 复现命令

```bash
# 0) 环境
node -v; npm -v; sw_vers; sysctl -n hw.memsize
~/Library/Caches/ms-playwright/chromium_headless_shell-1234/chrome-headless-shell-mac-arm64/chrome-headless-shell --version

# 1) 沙盒（仓库外；不使用 playwright install，直接复用既有浏览器缓存）
mkdir -p /tmp/caption-sandbox && cd /tmp/caption-sandbox && npm init -y
npm_config_cache=/tmp/caption-sandbox/.npm-cache PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD=1 \
  npm install playwright --no-audit --no-fund

# 2) A 项（4 个组合 × 90 帧，1080x1920@15fps）
for c in emphasis numbers sentence-io baseline; do
  node render.mjs --comp compositions/$c.html --width 1080 --height 1920 --fps 15 \
    --duration-ms 6000 --out frames/A_$c --json results/A_$c.json --keyframes 0,15,30,45,60,75,89
done
PY=/Users/yino/.dsh/dsh-runtimes/dsh-primary-runtime/dependencies/python/bin/python3
$PY metrics.py frames/A_emphasis frames/A_numbers frames/A_sentence-io frames/A_baseline

# 3) B/C 项（全量时长档与分辨率档；逐配置记录后删除帧序列）
bash run_bc.sh

# 4) 每帧成本归因
node attrib.mjs --comp compositions/sentence-io.html --width 1080 --height 1920 --fps 30 --frames 150

# 5) Alpha / 色键 / 安全区域
node render.mjs --comp compositions/sentence-io.html --width 1080 --height 1920 --fps 30 \
  --duration-ms 5000 --out frames/overlay_5s --json results/overlay_5s.json
ffprobe -v error -select_streams v:0 -show_entries stream=codec_name,pix_fmt,width,height \
  -of default=nw=1 frames/overlay_5s/frame_00045.png        # -> png / rgba / 1080 / 1920
$PY check_alpha.py frames/overlay_5s

# 6) FFmpeg 覆盖合成 + 无损边缘校验
ffmpeg -y -v error -f lavfi -i "testsrc2=size=1080x1920:rate=30:duration=5" \
  -f lavfi -i "sine=frequency=440:duration=5" -c:v libx264 -preset veryfast -pix_fmt yuv420p \
  -c:a aac -shortest overlay/base.mp4
ffmpeg -y -v error -i overlay/base.mp4 -framerate 30 -i frames/overlay_5s/frame_%05d.png \
  -filter_complex "[0:v][1:v]overlay=0:0:format=auto" -c:v libx264 -preset veryfast \
  -pix_fmt yuv420p -c:a copy -movflags +faststart overlay/out_effect_caption.mp4
ffmpeg -v error -i overlay/base.mp4 -map 0:a -f md5 -      # 与下一行必须一致
ffmpeg -v error -i overlay/out_effect_caption.mp4 -map 0:a -f md5 -
ffmpeg -y -v error -i overlay/base.mp4 -framerate 30 -i frames/overlay_5s/frame_%05d.png \
  -filter_complex "[0:v][1:v]overlay=0:0:format=auto" -c:v libx264 -qp 0 -pix_fmt yuv444p \
  -c:a copy overlay/out_lossless.mp4

# 7) 组合静态扫描与哈希
grep -nE "<script|on[a-z]+=|src=|href=|url\(|@import|http://|https://|//cdn" compositions/*.html || echo "无匹配"
shasum -a 256 engine.js compositions/*.html

# 8) HyperFrames 元数据（tarball 安装因 EIDLETIMEOUT 未完成）
npm view hyperframes@0.8.96 name version license dist.integrity dist.tarball engines --json
```

---

## 10. 未验证的遗留项与风险

- **R1（重要）**：B/C 用的是「headless Chrome + 逐帧截图」原语，**不是 HyperFrames 二进制实测**（`npm install hyperframes` 因 `EIDLETIMEOUT` 失败）。若 HyperFrames 使用了更快的捕获/编码路径，B 的结论可能偏悲观；必须在真实渲染器上复测后才能最终定案。
- **R2**：B 的差距很小（1.5–6%），且 5 s 档存在启动/预热噪声（重负载组合 5 s=1.262 与 15 s=1.012 的差异主要来自进程启动与首帧代价，实测已含约 25–50 ms 的页面加载）。判「不通过」依据的是 30–90 s 长档的稳定读数（1.015–1.031），结论稳健但仍属临界。
- **R3**：0.6 的底片是 `testsrc2` 合成源，**不是真实高光切片**；音画同步以「音频流 MD5 不变」证明，未做真实素材的 A/V 逐帧对齐检验。
- **R4**：中文字形用系统 `PingFang SC`，未使用随包锁定字体；本 change 5.1 要求的字体锁定与跨设备字形一致性问题**未在本沙盒验证**。
- **R5**：内存基线不可得：沙盒禁止 `ps`，只有 `os.freemem()` 的系统级下限（波动极大，本机 36 GB，采样值 77–1400 MB 区间），**不能**据此判断 8 GB 设备的渲染内存；另**未**测量浏览器进程的常驻峰值。
- **R6**：`tasks.md` 0.7 只完成了沙盒等价物与 npm 元数据（版本/Apache-2.0/`sha512-…` 完整性值）；**真实引擎脚本清单与哈希未取得**，任务 4.7 的完整性校验设计仍需补齐。
- **R7**：确定性（同一输入逐帧一致、随机源固定）与「模型不得生成脚本」的白名单校验路径**不在本次验证范围**（属任务组 2/3/5）。
- **R8**：仅覆盖 3 类纯 CSS 效果；粒子、3D 变换、逐笔迹描边等未验证，A 项结论限定在这 3 类之内。

---

## 11. 复测：原生 png-sequence 与字幕带优化（第二轮）

> 本轮沙盒：`/tmp/caption-sandbox2/`（**与第一轮 `/tmp/caption-sandbox/` 完全隔离**）。
> 固定输入与第一轮相同：同一个纯 CSS 组合（`sentence-io`，无脚本、无外部引用）、同样 1080x1920、同样 15 s / 30 s 时长档、30 fps。
> 全部数字为本机实测原始值；脚本与 JSON 见 11.7 复现命令。

### 11.0 本轮修正的三个方法缺陷

1. **第一轮用的是 Playwright `page.screenshot()` 通用截图，不是渲染器自身的 `png-sequence` 输出路径**。本轮把 HyperFrames 真正装起来，用 `hyperframes render --format png-sequence` 实测（Q1），不再用等价物替代。
2. **第一轮因网速放弃安装。** 本轮改用 `npm pack` 先取 tarball 再本地安装，**一次成功**（11.1）。
3. **第一轮未测「只渲染字幕带 + 合成」。** 本轮实测两种带实现（Q2），这是本轮最有价值的增量。

### 11.1 安装尝试与结果（逐条原始记录）

| 步骤 | 命令 | 结果 |
|---|---|---|
| 元数据 | `npm_config_cache=…/.npm-cache npm view hyperframes version dist-tags license engines dist.tarball dist.integrity --json` | 成功；`0.8.96`、`Apache-2.0`、`node>=22`、`sha512-5MIWpawK…xLw4FwXv4gG3w==`（与第一轮 npm 元数据**逐字节一致**） |
| **下载 tarball（本轮新增尝试）** | `npm pack hyperframes@0.8.96 --fetch-timeout=1800000 --fetch-retries=3 --fetch-retry-maxtimeout=600000` | **成功**：`hyperframes-0.8.96.tgz` 9,644,262 B，`shasum ba69691dd4a879d1b484054efd7d11b210eb40e5`，约 40 s |
| **本地安装** | `npm install ./hyperframes-0.8.96.tgz --no-audit --no-fund --fetch-timeout=1800000` | **成功**：`added 75 packages, and changed 2 packages in 2m`，CLI 版本 `0.8.96` |

**结论**：第一轮的 `EIDLETIMEOUT` 是瞬时/安装期解析问题，**不能**作为「HyperFrames 装不上」的依据。同一网络下 `npm pack` + 本地安装可用。

运行时前置条件（不满足会失败或白等，必须记录）：

| 现象 | 原因 | 处理 |
|---|---|---|
| `EPERM mkdir '/Users/yino/.cache/hyperframes'` / `~/.hyperframes` | 沙盒禁止写 HOME | 不下载浏览器，改用系统 Chrome：`HYPERFRAMES_BROWSER_PATH="/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"`、`PUPPETEER_CACHE_DIR=/tmp/caption-sandbox2/.puppeteer-cache`（**未执行** `playwright install`，**未**下载任何浏览器） |
| 首次渲染白等 45 s：`sub_timeline_readiness_timeout`（`[initSession:screenshot] pollSubCompositionTimelines complete (timeout) (49745ms)`） | 纯 CSS 动画组合不会注册 `window.__timelines[id]` | 组合根节点加 `data-no-timeline="true"`（渲染器日志本身给出的解法） |
| `Composition not found` | `-c` 是**相对工程目录**的路径 | 用 `-c compositions/sentence-io-15s.html`，工程目录 `hf-proj/` |

本轮真实渲染命令（示例）：

```bash
cd /tmp/caption-sandbox2
HYPERFRAMES_SKIP_SKILLS=1 HYPERFRAMES_BROWSER_PATH="/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" \
PUPPETEER_CACHE_DIR=/tmp/caption-sandbox2/.puppeteer-cache ./node_modules/.bin/hyperframes \
  render hf-proj -c compositions/sentence-io-15s.html --format png-sequence -o pngsmoke/OUT -f 30 --workers 1
```

组合骨架（保留第一轮全部 CSS，仅外套 HyperFrames 必需属性）：`<div id="root" data-composition-id="main" data-no-timeline="true" data-start="0" data-duration="{15|30}" data-width="1080" data-height="1920">` + 原 `<div class="stage">…`。产物为 `frame_000001.png` 起（**1 基序号**）的 1080x1920 RGBA PNG。

### 11.2 Q1：HyperFrames 自身 `png-sequence` 路径的耗时（1080x1920@30fps）

| 配置 | 帧数 | 总墙钟 (s) | 每帧 (ms) | **比值 = 墙钟/高光时长** | captureMode | workers |
|---|---|---|---|---|---|---|
| HF png-sequence 15 s | 450 | **72.15** | 160.3 | **4.810** | screenshot | 1 |
| HF png-sequence 30 s | 900 | **94.15** | 104.6 | **3.138** | screenshot | 1 |
| HF png-sequence 15 s | 450 | 78.28 | 174.0 | **5.219** | screenshot | 4（auto，12 核） |
| HF png-sequence 30 s | 900 | 88.60 | 98.5 | **2.953** | screenshot | 4 |
| HF png-sequence 5 s（确定性复跑 ×2） | 150 | 58.29 / 58.39 | 388.6 / 389.2 | **11.659 / 11.677** | screenshot | 1 |
| **同口径对比列**：第一轮 Playwright 全屏直捕 15 s | 450 | 15.76 | 35.01 | **1.051** | — | 1 |
| **同口径对比列**：第一轮 Playwright 全屏直捕 30 s | 900 | 30.93 | 34.35 | **1.031** | — | 1 |

- **Q1 结论：不通过，且远低于实时**（2.95–5.22，是实时的 3–5 倍慢）。第一轮用 `page.screenshot` 得到的 1.015–1.061 不是「HyperFrames 的真实性能」，但它也**不是乐观偏差**——渲染器自身的 alpha 捕获路径反而更慢。
- 机制（读包内实现，`dist/chunk-427VCI2W.js`）：HyperFrames 的 alpha 捕获 `captureScreenshotWithAlpha` / `pageScreenshotCapture` 走 CDP `Page.captureScreenshot`，并**强制 `captureBeyondViewport: true` + `optimizeForSpeed: false`**，另外每帧前设置 `Emulation.setDefaultBackgroundColorOverride` 透明背景与透明背景样式注入。多一次 beyond-viewport 全量合成，正好解释了「比 Playwright 默认路径更慢」。
- **宣称 ~2× 的 `--experimental-fast-capture`（Chrome `drawElementImage`）在本机所有运行中都没有启用**：所有 runtrace 均为 `"captureMode":"screenshot"`（含默认 auto 与显式 `--workers auto`）。因此 Q1 的可选优化在本次环境里未生效。
- 多 worker（4）**没有收益反而更慢**（15 s：78.3 s vs 72.2 s），capture 阶段本身就是 CPU 瓶颈。
- 5 s 档的 388 ms/帧异常偏高（compile 阶段 37.8 s），短档的固定/预热开销被摊到更少帧上；本报告以 15 s/30 s 档为准。

### 11.3 Q2：只渲染字幕带 + 合成（本轮核心增量）

两种带实现（字幕带 = 全画布 y∈[1400,1800)，即 **1080x400 = 全画布面积的 20.83%**）：

| 代号 | 捕获方式 | 说明 |
|---|---|---|
| `band-clip` | 视口仍 1080x1920，`page.screenshot({clip:{x:0,y:1400,width:1080,height:400}})` | 布局完全不变，只截取子区域 |
| `band-viewport` | 视口高度 = 400，组合整体包进绝对定位的 1080x1920 层并上移 1400px | Chrome 的**捕获 surface 本身**只有 1080x400；实测与 `band-clip` **逐像素一致**（frame_00045 四通道 max diff = 0），证明布局等价 |

合成：ffmpeg 单趟 `overlay=0:1400:format=auto`，把带帧贴回透明 1080x1920 画布，输出**全分辨率 RGBA PNG 序列**。

| 配置 | 帧数 | 捕获墙钟 (s) | 捕获每帧 (ms) | 合成墙钟 (s) | **端到端墙钟 (s)** | **端到端比值** | 捕获面积比 |
|---|---|---|---|---|---|---|---|
| `band-clip` 15 s | 450 | 14.16 | 31.46 | 1.88 | 16.04 | **1.070** | 20.8% |
| `band-clip` 30 s | 900 | 27.12 | 30.13 | 3.61 | 30.73 | **1.024** | 20.8% |
| **`band-viewport` 15 s** | 450 | 11.15 | **24.76** | 1.85 | 13.00 | **0.867** | 20.8% |
| **`band-viewport` 30 s** | 900 | 21.78 | **24.19** | 3.64 | 25.41 | **0.847** | 20.8% |
| `band-viewport` 15 s + 1 帧 rAF 绘制同步 | 450 | 13.88 | 30.84 | 1.86 | 15.74 | **1.049** | 20.8% |
| **同口径对比列**：第一轮全屏直捕 15 s / 30 s | 450 / 900 | 15.76 / 30.93 | 35.01 / 34.35 | — | — | **1.051 / 1.031** | 100% |
| **同口径对比列**：本轮 driver 全屏直捕 15 s / 30 s | 450 / 900 | 16.23 / 30.98 | 36.05 / 34.42 | — | — | **1.082 / 1.033** | 100% |

- **字幕带路径确实把比值压到 1.0 以下，但只有「小 surface」变体成立**：`band-viewport` 15 s/30 s = **0.867 / 0.847**（余量 13–15%）；`band-clip` 仍 ≥1.0（1.024–1.070）。
- **面积降到 20.8%，耗时只省 12–33%**，再次印证第一轮「每帧固定开销支配成本」的判断：全屏 34.4–36.1 ms/帧 → clip 29.5–30.8 ms/帧（省 12–15%）→ 小 surface 23.6–24.8 ms/帧（省 30–33%）。**不是面积线性收益**，所以带宽再缩也换不来数量级改善。
- 合成成本很低：ffmpeg 单进程 **4.0–4.2 ms/帧**（`-compression_level 1`，输出 135–138 KB/帧、约 4.1 MB/s，比第一轮 2.5 MB/s 高，因为现在带真实 Alpha 的全画布压缩率更低）。备选 libvips/sharp 为 7.8–9.0 ms/帧，**更慢**，不采用。
- 合成实现必须显式 `-start_number 0`：ffmpeg image2 输出默认从 `frame_00001` 起，会让整条序列**错一帧**（本轮踩到并修正；修正前「合成帧 vs 全屏直捕」平均偏差 0.41/255，修正后 0.00042/255）。

### 11.4 Q3：两条路径的产物合规性

| 检查项 | Q1 HyperFrames 原生 png-sequence | Q2 字幕带 + 合成（band-viewport + ffmpeg） |
|---|---|---|
| 真实 Alpha | `png / rgba`；透明 95.21%、中间 Alpha 4.79%、四角 0；无绿色色键 | `png / rgba`；透明 **96.68%**、中间 Alpha **3.32%**、四角 40x40 最大 Alpha **0**、色键像素 **0**（与全屏直捕同值） |
| 全分辨率 | 1080x1920 | 1080x1920（合成后仍是全画布，未缩放） |
| 逐帧确定（同输入重现） | **不位精确**：5 s/150 帧两次运行有 **1 帧** 不同（`frame_000083`，6987 px，max diff 127） | **不位精确（无同步）**：同一配置两次运行 clip 2/450 帧、band-viewport 2/450 帧、`--disable-threaded-animation` 4/450 帧不同，单帧最大 diff 255、数千像素（如 frame 193 有 5893 px） |
| 加绘制同步后 | 未测（HF 无该开关） | 每帧 seek 后等 **1 帧 rAF**：**3 次运行 × 450 帧位精确一致**；代价是 seek 0.5→5.35 ms/帧，比值 0.926（捕获）/ **1.049**（端到端） |
| 带覆盖正确性 | — | 全 450 帧字幕像素并集在带内 (1,66)–(1012,354)，折合全画布 y∈[1466,1754]；四边均无 Alpha 触边，**0 帧被裁切** |
| 合成保真 | — | 450/450 帧与全屏直捕逐像素比对：**平均绝对偏差 0.00042/255**，仅 7–9 帧因两次独立运行各自的光栅竞态出现局部差异 |

- **Alpha 与全分辨率：两条路径都满足。**
- **逐帧确定：两条路径都不是位精确。** 差异不是随机动画状态，而是**合成器绘制竞态**（同一输入、同一时间点，个别帧截到了未完整提交的画面），加 1 帧 rAF 同步即可消除，但每帧多约 5 ms。
- 需要注意：`--disable-threaded-animation` 也不能消除该竞态（4/450 帧仍不同），说明竞态在合成提交侧而非动画线程。

### 11.5 Q4 裁决

**进入条件 B：不通过（严格口径）。** 依据：

1. **原生路径（Q1）决定性不达标**：HyperFrames 自身 `png-sequence` 在 1080x1920@30fps 下 **2.95–5.22**，是实时的 3–5 倍慢；`--experimental-fast-capture` 在本机未启用。第一轮「用错路径」的方法缺陷成立，但**修正后结论更差**，因此第一轮的「B 不通过」结论保持不变，且不再依赖于 `page.screenshot` 这一方法瑕疵。
2. **字幕带路径（Q2）是本轮唯一能压到 <1.0 的路径**：`band-viewport`（小 surface）+ ffmpeg overlay = **0.867（15 s）/ 0.847（30 s）**，余量 13–15%。但 `band-clip` 变体不达标（1.024–1.070）——**只有把 Chrome 的捕获 surface 本身缩到带高才有效**。
3. **「<1.0」与「逐帧位精确可重现」不可同时满足**：唯一 <1.0 的配置存在 2–4/450 帧的可见非确定性；把它修成位精确（1 帧 rAF 同步）后端到端比值变为 **1.049**，重新高于实时。
   → 因此在「必须 <1.0 **且** 必须逐帧位精确」的合取判据下，**没有任何被测配置通过**。

**条件性替代裁决（请 Lead 裁决，本报告不擅自放宽判据）**：若「逐帧确定」按「逐帧动画状态由输入唯一确定、不含随机源」解释，并接受「重跑有约 0.4–0.9% 的帧存在像素级光栅差异」（HyperFrames 自身与第一轮路径属同一类问题），则 **B 通过**，必须记录的路径与约束：

| 约束 | 值 |
|---|---|
| 路径 | **band-viewport（视口高度=带高的小 surface 捕获）+ ffmpeg `overlay` 合成全分辨率 RGBA 帧**；**不可**用 band-clip |
| 带区域 | 1080x400（=全画布高 **20.8%**），覆盖字幕包围盒 y∈[1466,1754]，上下留白 66 / 42 px；**组合必须保证字幕不越出该带**（长文本换行、放大动效、多行字幕都会越界，需在 design 中作为硬约束） |
| 分辨率 / 帧率 | 1080x1920 @30fps 实测 0.847–0.867；**60 fps 未测**（按帧数外推 ≥1.7，不可用）；720p 未测本条路径 |
| 合成 | ffmpeg 单进程 `overlay=0:<bandTop>:format=auto`，4.0–4.2 ms/帧，必须 `-start_number 0`；输出 135–138 KB/帧（约 4.1 MB/s），比第一轮全屏直捕（2.5 MB/s）大 60% |
| 确定性 | 若要求位精确，必须每帧 +1 帧 rAF 绘制同步（比值升到 1.049）；否则接受 2–4/450 帧的光栅差异 |
| 渲染器改动 | HyperFrames 的 `captureScreenshotWithAlpha(page, width, height)` 目前把 clip 硬编码为全画布且 `captureBeyondViewport: true`；走带路径需扩展为「带 clip / 带视口捕获 + 外部合成」，并把 `data-no-timeline` 与 captureMode 行为一并验证 |

### 11.6 剩余不确定性

- **U1**：60 fps、720p/其他分辨率下带路径未测；本报告只在 1080x1920@30fps、15 s/30 s 档给出 <1.0。
- **U2**：测试机是 M3 Pro / 36 GB，目标设备 8 GB 未实测；带路径省的是捕获面积，弱机上的相对收益可能更大也可能更小（每帧固定开销占比会变化），**不能外推**。
- **U3**：带路径未在真实高光文本/多行/超长文本上验证，「字幕不越出 1080x400」这一前提只在当前样例上成立。
- **U4**：合成并行化（多进程 ffmpeg 切段）未测，理论上还能再省约 3 ms/帧，但**不改变** 11.5 的确定性结论。
- **U5**：HyperFrames 5 s 档 388 ms/帧的异常未定位（compile 阶段 37.8 s），短档结论不可用。
- **U6**：仍未使用真实工程高光文本与真实高光底片（沿用第一轮 R3/11.0 的限制）；`--experimental-fast-capture` 为何未启用未查清（可能与 `canvas-draw-element` flag / 组合不含 VFX 画布有关）。
- **U7**：`openspec validate add-effect-caption-renderer --strict` 通过；本轮**未修改** 任何 delta 规格与 `tasks.md` 复选框。

### 11.7 复现命令（脚本均在 `/tmp/caption-sandbox2/`，仓库外）

```bash
# 0) 安装（不要 playwright install；浏览器用系统 Chrome）
cd /tmp/caption-sandbox2
npm_config_cache=.npm-cache npm pack hyperframes@0.8.96 --fetch-timeout=1800000 --fetch-retries=3
npm_config_cache=.npm-cache npm install ./hyperframes-0.8.96.tgz --no-audit --no-fund
HYPERFRAMES_SKIP_SKILLS=1 ./node_modules/.bin/hyperframes --version        # 0.8.96

# 1) Q1：HyperFrames 原生 png-sequence（15s/30s × workers 1/auto，另含 5s 确定性复跑）
bash bench_hf.sh 15 1    q1_w1_15s
bash bench_hf.sh 30 1    q1_w1_30s
bash bench_hf.sh 15 auto q1_auto_15s
bash bench_hf.sh 30 auto q1_auto_30s
bash bench_hf.sh 5  1    q1_det_a ; bash bench_hf.sh 5 1 q1_det_b
#     日志 logs/hf_*.log，结果 results/hf_*.json（含 capture_ms / captureMode / worker_count）

# 2) Q2：全屏直捕对照 + 两种字幕带 + 合成（15s/30s）
bash run_q2.sh 30                       # band-clip 变体 + 合成
bash run_q2_wrap.sh 15 ; bash run_q2_wrap.sh 30    # band-viewport 变体 + 合成
#     单条命令（driver）：node render2.mjs --comp compositions/sentence-io.html --width 1080 --height 1920 \
#       --fps 30 --duration-ms 15000 --out frames/wrap_15s --json results/wrap_15s.json --sha \
#       --wrap-top 1400 --cut-height 400          # ← 小 surface 带；去掉后两个参数即全屏直捕
#       （--raf-sync 1 加 1 帧 rAF 绘制同步；--clip 0,1400,1080,400 即 band-clip 变体）
bash compose_band_ffmpeg.sh frames/wrap_15s 1400 450 frames/wcomp_cl1_15s 1 wrap

# 3) Q3：Alpha / 带覆盖 / 合成保真 / 确定性
PY=/Users/yino/.dsh/dsh-runtimes/dsh-primary-runtime/dependencies/python/bin/python3
$PY verify2.py --band frames/wrap_15s --composed frames/wcomp_cl1_15s --full frames/full_15s \
   --frames 450 --y0 1400 --band-h 400 --json results/verify_wrap_15s.json
diff <(cut -d' ' -f1 frames/wrapraf1_15s_r1/SHA256SUMS) <(cut -d' ' -f1 frames/wrapraf1_15s_r2/SHA256SUMS)  # rAF=1 三次运行全部一致

# 4) 汇总
cat results/final_summary.json
```

关键帧属性（媒体不入库，原始帧留在沙盒 `/tmp/caption-sandbox2/`）：Q2 合成路径第 40 帧，1080x1920 `rgba`，Alpha 0–241，非透明包围盒 (132,1466)–(941,1690)。
