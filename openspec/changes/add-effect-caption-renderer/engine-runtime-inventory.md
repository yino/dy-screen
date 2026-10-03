# 渲染器运行时清单与完整性校验设计（任务 0.7）

> 目标：回答"渲染器到底往被渲染页面里放了哪些可执行内容、版本是什么、怎么校验"，
> 作为任务 4.7（渲染器运行时完整性校验）与 5.4（版本进缓存键）的设计输入。
> 全部结论来自对 `hyperframes@0.8.96` 安装产物的静态穷举；**运行时观测部分未完成**，见第 7 节。

## 1. 结论摘要

| 项 | 值 |
|---|---|
| 渲染器身份 | `hyperframes` 0.8.96，Apache-2.0，`engines.node >= 22`（包内 `PROVENANCE_RENDERER_NAME` / `PROVENANCE_VERSION` 亦为 `hyperframes` / `0.8.96`） |
| 引擎脚本文本 | 480,863 B，sha256 `38592478aad3f515e33b69ecfc11898e338c28a54799bebcd933c18906decd06` |
| 承载位置 | **不是独立文件**：内联常量 `RUNTIME_IIFE`，位于 `dist/chunk-DUILJXNH.js`（588,067 B，sha256 `74d69aa337b2c34f6e848f20c26a714f60be83b53eb4c9697c557c4acce422c0`） |
| 注入方式（默认） | `injectInterceptor(html, "inline")` 把引擎脚本**内联**为 `<script data-hyperframes-preview-runtime="1">…</script>`，插到 `</head>` 之前 |
| 第二个注入运行时 | 捕获期 audio FX 脚本 38,874 B，sha256 `eec4e4349b87ee1542b88a32a4a5dd1428a2bc50fda7ad124c27083d126eb426`，由 `page.addScriptTag({content: …})` 注入 |
| 第三类可执行内容 | 11 处 `page.evaluateOnNewDocument(...)`（CDP 预文档插桩，页面加载前执行） |
| 随包 bundle | `dist/` 548 文件 / 32,465,870 B，树哈希 `f4460819c799f246e7d7089adf79075777917d5ae1a156481a7698ca31f83fd1`（约定见 `evidence/hyperframes-0.8.96-dist-manifest.txt` 文件头） |
| 完整性校验单元 | **三层**：bundle 树哈希 → 引擎脚本文本哈希 → 渲染前页面脚本来源断言；见第 5 节 |

## 2. 版本与来源

- npm 元数据（报告 11.1，两轮逐字节一致）：`hyperframes@0.8.96`、`license=Apache-2.0`、`engines.node>=22`、`dist.integrity=sha512-5MIWpawK…xLw4FwXv4gG3w==`。
- 本地 tarball：`hyperframes-0.8.96.tgz` 9,644,262 B，`shasum ba69691dd4a879d1b484054efd7d11b210eb40e5`（**未归档**，可按 `evidence/README.md` 复现）。
- 包内自述常量：`PROVENANCE_RENDERER_NAME = "hyperframes"`、`PROVENANCE_VERSION = "0.8.96"`，另有 `PROVENANCE_RENDERER_TAG` / `PROVENANCE_VERSION_TAG` 用于渲染产物溯源。

> 生产约束（决策二 + 任务 10.6）：这些版本必须**随包固定**并进入渲染缓存键（5.4），发行包中不得出现开发机路径或未声明的可执行文件。

## 3. 注入到被渲染页面的可执行内容（穷举）

### 3.1 引擎脚本（唯一被设计承认的可执行内容）

`dist/chunk-E7APSJCK.js` 中的编译器注入函数（原 `core/dist/compiler/htmlBundler.js`）：

```js
function injectInterceptor(html, runtimeMode = "inline") {
  const sanitized = stripEmbeddedRuntimeScripts(html);          // 先剥掉页面里已有的同名 runtime
  if (sanitized.includes(RUNTIME_BOOTSTRAP_ATTR)) return sanitized; // 幂等
  const runtimeScriptUrl = getRuntimeScriptUrl();               // 只读 env HYPERFRAME_RUNTIME_URL，默认空
  let tag;
  if (runtimeScriptUrl)            tag = `<script ${RUNTIME_BOOTSTRAP_ATTR}="1" src="${escaped}"></script>`;
  else if (runtimeMode === "placeholder") tag = `<script ${RUNTIME_BOOTSTRAP_ATTR}="1" src=""></script>`;
  else tag = `<script ${RUNTIME_BOOTSTRAP_ATTR}="1">${getHyperframeRuntimeScript()}</script>`;
  // 插到 </head> 之前
}
```

要点：

1. **默认路径是内联**（`HYPERFRAME_RUNTIME_URL` 为空）；一旦该环境变量被设置，页面会变成**外链 `src`**，可执行内容就来自网络 —— 生产必须断言该变量为空，否则"只用随包代码"不成立。
2. `RUNTIME_BOOTSTRAP_ATTR = "data-hyperframes-preview-runtime"`，既是幂等标记，也是校验锚点。
3. 注入前会按 marker 剥离页面自带的同名脚本，避免"组合里塞一份假 runtime"绕过校验。
4. 剥离/识别 marker：
   - `RUNTIME_SRC_MARKERS = ["hyperframe.runtime.iife.js", "hyperframes-runtime.modular.inline.js", "hyperframe-runtime.modular-runtime.inline.js", "data-hyperframes-preview-runtime"]`
   - `RUNTIME_INLINE_MARKERS = ["__hyperframeRuntimeBootstrapped", "__hyperframeRuntime", "__hyperframeRuntimeTeardown", "__HF_EXPORT_RENDER_SEEK_CONFIG", "window.__player ="]`
   - 另有一组 `SIMPLE_RUNTIME_FLAG_ASSIGNMENTS` 正则
5. 引擎脚本对外暴露的全局符号（可用于渲染前断言"脚本确实生效"）：`window.__timelines`、`window.__hyperframes`（`assetUrl` / `fitTextFontSize` / `getVariables` / `pretext` / `registerRuntimeDataHandler` / `setRuntimeData` / `clearRuntimeData`）、`window.__player`、`window.__playerReady`、`window.__renderReady`、`window.__hyperframeRuntimeBootstrapped`。
6. 运行时会**重新执行组合里出现的 `<script>`**：`dist/chunk-DUILJXNH.js`（字符偏移 ≈517,110）把组合内的 `SCRIPT` 节点 `createElement("script")` + 复制 `textContent` 后 `appendChild`；偏移 ≈295,590 处另有按 `kind` 注入的分支。**这就是任务组 3 必须显式拒绝 `script`/`on*` 的原因**：白名单是这条边界的唯一防线。

### 3.2 audio FX 运行时（**不在组合页面内**）

- 定义：`getAudioFxRuntimeScript()`；调用点在 `dist/chunk-427VCI2W.js`（字符偏移 ≈536,710）。
- 文本：38,874 B，sha256 `eec4e4349b87ee1542b88a32a4a5dd1428a2bc50fda7ad124c27083d126eb426`，头 `"use strict";(()=>{var Ae="data-fx-chain"…`。
- **注入目标是独立的工作页面，不是组合页面**：渲染器在临时目录写一个空白页 `hf-fx-host-*/audio-fx.html`（`<!doctype html><meta charset=utf-8><title>audio fx</title>`），`page.goto` 后再 `addScriptTag` 注入该运行时，用于离线渲染音频 FX；仅当组合确实有音频 FX 轨道时才走到这里（否则提前返回原音频）。
- 结论：它属"随包渲染器在**自己的辅助页面**里执行"，仍受 bundle 树哈希覆盖，但**不进**"默认渲染页面中的可执行内容"清单。

### 3.3 CDP 预文档插桩（`evaluateOnNewDocument`，共 11 处）

`Page.addScriptToEvaluateOnNewDocument` 在文档开始前注入，典型有：

| 位置 | 作用 |
|---|---|
| `dist/capture-IIZWKNHD.js`（≈16,427） | `window.__hf_io_targets` + `IntersectionObserver` 包装（动画捕获） |
| `dist/capture-IIZWKNHD.js`（≈154,095） | `HTMLCanvasElement.prototype.getContext` 包装（canvas 捕获） |
| `dist/chunk-427VCI2W.js`（≈160,887） | `installMediaRenderIdBridge`（媒体元素渲染 ID 桥） |
| `dist/chunk-427VCI2W.js`（≈193,855 / 194,078 / 194,485） | `__name` 兼容垫片、加速 canvas 插桩、`window.__HF_ROOT_PROPS__` |

这些是**捕获路径**的可执行内容，同样**来自随包渲染器而非组合文件**，随 bundle 整体哈希覆盖，且**不会**出现在 `document.scripts` 里（故第 7.2 节的运行时枚举看不到它们——这与实测一致：本次普通加载中 `__hf_io_targets` / `__HF_ROOT_PROPS__` 均为 `undefined`、canvas 未被包装）。

### 3.4 其它 `addScriptTag` 注入点

共 10 处，其中 8 处不作用于默认渲染的组合页面：
`dist/beats-J2M5BZDO.js`（节拍分析，注入到空白页）、`dist/checkBrowser-FH5HBJZJ.js`（`layout-audit.browser.js` / `motion-sample.browser.js` / `contrast-audit.browser.js`，审计功能）、以及 `dist/chunk-7FLL6XD4.js` 的 `runContrastAudit(page)` 与 `dist/chunk-427VCI2W.js` 的 audio FX（3.2）。
注意：**审计功能被显式启用时会把随包审计脚本注入组合页面**（`runContrastAudit` 先判断 `duration > 0` 再注入）。这些脚本同样随包、可由 L1 树哈希覆盖，但若产品不使用审计功能就不应出现在清单里——4.7 的校验应按"启用了哪些功能"分档。

## 4. 引擎脚本文本的再生成（用于校验与缓存键）

```bash
node --input-type=module -e "
import {createHash} from 'crypto';
const m = await import('/path/to/node_modules/hyperframes/dist/chunk-DUILJXNH.js');
const s = m.getHyperframeRuntimeScript();
console.log(Buffer.byteLength(s,'utf8'), createHash('sha256').update(s).digest('hex'));
"
# 期望：480863 38592478aad3f515e33b69ecfc11898e338c28a54799bebcd933c18906decd06
```

**不要**把 470 KB 引擎脚本文本本身签入仓库：它是 Apache-2.0 第三方产物，可上述命令逐字节再生成；校验只需保存哈希与版本。

## 5. 完整性校验设计（给任务 4.7）

渲染前依次校验，任一不符即**拒绝渲染并返回可区分错误**，不得静默降级：

| 层 | 校验对象 | 方法 | 失败语义 |
|---|---|---|---|
| L1 | 随包渲染器 bundle | 对 `dist/` 重新计算清单（sha256␠␠size␠␠path）并与随包 manifest 的树哈希比对；渲染器版本比对 `PROVENANCE_VERSION` | `renderer_runtime_mismatch` |
| L2 | 引擎脚本文本 | 渲染前用第 4 节方式派生文本并比对 sha256（480,863 B / `38592478…`）；同时校验 audio FX 文本（38,874 B / `eec4e434…`） | `engine_script_hash_mismatch` |
| L3 | 渲染页面 | 断言 `HYPERFRAME_RUNTIME_URL` 为空；断言页面内 `script` 只来自"带 `data-hyperframes-preview-runtime` 的内联引擎脚本 + 可选 audio FX 脚本"，且不含 `src` 外链 | `unexpected_page_script` |
| L4 | 组合文件 | 组 3 的白名单校验显式拒绝 `script`/`on*`/脚本引用/外部 URL（因为运行时会执行组合内 script，见 3.1 第 6 点） | `composition_rejected` |

进缓存键的版本分量（5.4）：渲染器版本 0.8.96 + bundle 树哈希 + 引擎脚本文本哈希 + audio FX 文本哈希 + 浏览器版本 + 字体版本。

## 6. 为什么这条清单必须"按构造"而不是"按过滤"

引擎脚本以**内联文本**进入 HTML，页面内不存在可供哈希的独立文件；而组合文件本身也会被渲染器执行其 `<script>`。因此：

- 可执行内容的来源必须**钉死在"随包渲染器"这一处**（决策二），校验只能建立在 L1/L2 的随包哈希上；
- 任何"模型生成脚本再用正则过滤"的做法都不成立——过滤不是边界，白名单 + 运行时哈希才是。

## 7. 验证状态（含未完成项）

### 7.1 已完成：HTML 层（页面被注入后到底有什么）

用**真实注入函数**（`injectInterceptor(html,"inline")`）把沙箱组合 `compositions/sentence-io.html` 编译成"渲染器实际会服务的页面"，再解析页面内的 `<script>`：

| 观测 | 值 |
|---|---|
| 组合文件注入前的 `<script>` 数 | **0** |
| 注入后页面的 `<script>` 数 | **1** |
| 该脚本 | 内联、带 `data-hyperframes-preview-runtime="1"`、无 `src` |
| 内联字节 | **480,863 B**，sha256 `38592478…decd06`，与 `getHyperframeRuntimeScript()` **逐字节一致** |
| 页面总字节 | 484,674 B |
| `getRuntimeScriptUrl()` | `""`（`HYPERFRAME_RUNTIME_URL` 未设置 → 走内联分支） |

工具（已在本机跑通）：`evidence/scripts/emit-page-html.mjs`。

### 7.2 已完成：运行时层（真实 Chrome 里 `document.scripts` 的实际集合）

在真实 Chrome（`/Applications/Google Chrome.app/...`）里加载 7.1 生成的页面：

```json
{"pageInlineScripts":[{"hasBootstrapAttr":true,"bytes":480863,"sha256":"38592478…decd06"}],
 "observed":{"scripts":[{"src":null,"type":null,"hasBootstrapAttr":true,"textLength":480863,
   "head":"\"use strict\";(()=>{var Bx=Object.create;","tail":"ContentLoaded\",Gb,{once:!0}):Gb();})();\n"}],
  "globals":{"hyperframes":"object","timelines":"object","playerReady":true,"renderReady":true,"bootstrapped":true},
  "instrumentation":{"hfIoTargets":"undefined","hfRootProps":"undefined","canvasGetContextWrapped":false}},
 "ok":true,"failures":[]}
```

即：页面内 `<script>` **恰为 1 个随包引擎脚本**、无外链、脚本真正执行并接管（`__playerReady` / `__renderReady` / `__hyperframeRuntimeBootstrapped` 全为真），且本次普通加载中**未出现**捕获路径的插桩痕迹（与 3.3 的分类一致）。

工具与回执：`evidence/scripts/probe-runtime-scripts.mjs`、`evidence/runtime-probe-receipt-0.8.96.json`。

### 7.3 仍然只有静态结论的部分（诚实记录）

1. **捕获路径的 CDP 插桩未做运行时观测**：它们不产生 `<script>` 元素（因此 7.2 看不到），其清单来自对 548 个随包文件的穷举扫描（3.3 节）。若要运行时确认，需在真实 `hyperframes render` 的捕获会话里读 CDP 会话状态，属可延后的加固项。
2. **浏览器与字体未纳入清单**：固定版本渲染浏览器、锁定字体的版本与哈希属任务 5.1 / 5.4 / 10.1。报告第二轮用的是 `HYPERFRAMES_BROWSER_PATH` 指向系统 Chrome，**那不是**随包固定版本，不能进生产清单。
3. **`SIMPLE_RUNTIME_FLAG_ASSIGNMENTS` 正则组未逐一展开**：仅需在校验实现时按包内常量同步。
4. **审计功能分档未定**：启用对比度/布局/动效审计会向组合页面注入随包审计脚本（3.4），4.7 的清单需按启用的功能分档，本清单暂按"默认渲染不启用审计"记录。

## 8. 证据索引

| 内容 | 位置 |
|---|---|
| 页面注入 + 脚本枚举工具（本机跑通） | `evidence/scripts/emit-page-html.mjs` |
| 真实浏览器运行时探测 + 回执 | `evidence/scripts/probe-runtime-scripts.mjs`、`evidence/runtime-probe-receipt-0.8.96.json` |
| 沙箱脚本与组合样本（只读证据） | `evidence/scripts/` |
| 随包 dist 清单（548 文件） | `evidence/hyperframes-0.8.96-dist-manifest.txt` |
| 归档范围与复现命令 | `evidence/README.md` |
| 两轮适用性结论与全部实测数字 | `sandbox-feasibility.md` |
| 本清单在设计中的作用 | `design.md` 决策一/决策二、任务 4.7 / 5.4 / 10.1 / 10.4 |
