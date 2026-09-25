// 最小交互式剪辑界面。**没有框架** —— 就是 DOM 操作。
//
// 业务规则一概不在这里：校验问 Rust（engine.open），求值问 Rust（engine.evaluate），
// 渲染问 Rust（engine.seek）。这一层只负责"把工程画成能点的东西"和"把点击写回工程"。
//
// # 这一版认的是**工程文件**（ProjectDoc，timeline 为 v2）
//
// 元素模型 v2 与 v1 的差别是实质的：v2 的元素**自带 start/end**、可以没有素材
// （那就是调整图层）、带 blend 与 markers。页面按 v2 画，于是编辑器能编出
// 渲染器真正支持的东西 —— 而不是"界面上编得出来、出片时才发现不行"。
//
// # 素材地址从哪来
//
// 从**工程文件的资产表**（doc.assets），地址由 backend 按 asset id 给。
// 页面里搜不到任何写死的素材路径 —— 那条不变量由 scripts/check-web-invariants.mjs 盯着。

import { loadEngine } from "/engine.js";
import { exportPngSequence, base64ToBytes } from "/export/png-sequence.js";
import { activeBackendFrom } from "/backend.js";
import { createHttpBackend } from "/export/http.js";

// **观测先于诊断。** 启动的每一步都记一笔，并且同时写进 document.title ——
// 只发 beacon 的话，页面若在某个点之后不再有网络活动，外面就只看到"什么都没发生"。
// 卡住比失败难查，所以脚印要留两条（内存 + 标题，避免单点失效）。
window.__dhampirMarks = [];

const backend = activeBackendFrom(location.search);

// 渲染器实现得了的混合模式。**这是编译期的事实**（core 的 BlendMode::is_implemented），
// 不是对端的能力 —— 对端能不能做由出片前的预检回答。
const IMPLEMENTED_BLENDS = ["normal", "add", "multiply", "screen"];

const state = {
  engine: null,
  // 工程文件（ProjectDoc）。页面编辑的是这一份，提交给后端的也是这一份。
  doc: null,
  selected: null,
  frame: 0,
  issues: [],
  warnings: [],
  capabilities: null,
  // 不阻断使用、但必须看得见的事情（例如某一路素材加载失败）。
  notices: [],
  // 最近一条状态/结果（"已提交给后端出片…"这类）。**只留最新一条** ——
  // 它是状态栏，不是日志；越积越多只会把真正的问题挤下去。
  hint: "",
  // 素材库（来自后端的 dhampir library）。null = 没连后端 / 还没取。
  library: null,
  // 每个素材元素实际拿到的解码尺寸。**记的是 videoWidth（解码后的真实尺寸）**，
  // 不是素材声明的尺寸 —— 两者可能不同，而决定 seek 代价的是前者。
  decodeSizes: [],
};

const $ = (id) => document.getElementById(id);

function mark(name) {
  try { window.__dhampirMarks.push(name); } catch (error) { /* 观测手段不该影响启动 */ }
  try { document.title = "dhampir:" + name; } catch (error) { /* 同上 */ }
  try { navigator.sendBeacon("/page-error", "里程碑:" + name); } catch (error) { /* 同上 */ }
}

function notice(text) {
  state.notices.push(String(text));
  renderIssues();
}

// --- 界面零件（图标 / toast / 空态 / 时码） ---------------------------------------
//
// 设计参照 V-Trim 的 webui：图标内联、控件胶囊化、**三态分明**（载入 / 空 / 读不到）。
// 没有 npm 依赖 —— scripts/check-web-invariants.mjs 盯着 web/node_modules 不许存在 ——
// 所以这里是一张内联 SVG 表，不是图标字体、也不是打包进来的组件库。

/** Lucide 风格 stroke 图标，24x24 viewBox。**只放用得到的**，不搬整套。 */
const ICONS = {
  first: '<path d="M19 20 9 12l10-8v16z"/><path d="M5 19V5"/>',
  prev: '<path d="M15 18l-6-6 6-6"/>',
  play: '<path d="M6 3l14 9-14 9V3z"/>',
  pause: '<rect x="6" y="4" width="4" height="16" rx="1"/><rect x="14" y="4" width="4" height="16" rx="1"/>',
  next: '<path d="M9 18l6-6-6-6"/>',
  last: '<path d="M5 4l10 8-10 8V4z"/><path d="M19 5v14"/>',
  volume: '<path d="M11 5 6 9H2v6h4l5 4z"/><path d="M15.5 8.5a5 5 0 0 1 0 7"/><path d="M19 5a10 10 0 0 1 0 14"/>',
  "volume-x": '<path d="M11 5 6 9H2v6h4l5 4z"/><path d="m23 9-6 6"/><path d="m17 9 6 6"/>',
};

/**
 * 把一张图标放进某个元素。**替换内容而不是追加** ——
 * 追加的话每切一次播放/暂停都会多叠一层看不见的 svg。
 */
function setIcon(id, name) {
  const host = $(id);
  if (host === null) return;
  host.innerHTML = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"'
    + ' stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">'
    + (ICONS[name] === undefined ? "" : ICONS[name]) + "</svg>";
}

/**
 * 短暂反馈。与 #issues 是**分工**而不是重复：
 * 那里是**留着的问题清单**（"它还在那儿"才是它的价值），这里是**说过就走的确认**。
 * 把"已撤销"这类确认也塞进问题清单，真正的问题会被一句句确认挤下去。
 */
function toast(text, kind, ms) {
  const host = $("toasts");
  if (host === null) return;
  const el = document.createElement("div");
  el.className = "toast " + (kind === undefined ? "ok" : kind);
  el.textContent = String(text);
  host.appendChild(el);
  window.setTimeout(() => { el.remove(); }, ms === undefined ? 2600 : ms);
}

/**
 * 画一个空态。**"读不到"与"真的没有"必须是两句不同的话** ——
 * 合成一句「暂无数据」就把"去查连接"和"这里本来就空着"混成了一件事，
 * 而这两件事该做的事情完全相反。
 */
function setEmpty(id, text, options) {
  const host = $(id);
  if (host === null) return;
  const opts = options === undefined ? {} : options;
  host.textContent = "";
  const el = document.createElement("div");
  el.className = "empty" + (opts.error === true ? " error" : "");
  el.textContent = String(text);
  if (typeof opts.why === "string" && opts.why !== "") {
    const why = document.createElement("span");
    why.className = "why";
    why.textContent = opts.why;
    el.appendChild(why);
  }
  host.appendChild(el);
}

/**
 * 秒读数。**帧号才是契约单位**（铁律：编辑一律用整数帧，不用秒），
 * 秒只是给人一眼判断"到哪儿了"。所以两个都给，且秒**绝不参与任何编辑计算** ——
 * 一旦有人拿它去换算，浮点就会回到这条路的中间（那正是铁律要挡的东西）。
 */
function formatTimecode(frame, fps) {
  if (!(fps > 0)) return "00:00.00";
  const seconds = frame / fps;
  const minutes = Math.floor(seconds / 60);
  const rest = seconds - minutes * 60;
  return String(minutes).padStart(2, "0") + ":" + (rest < 10 ? "0" : "") + rest.toFixed(2);
}

// --- 判定回传（程序化验收唯一可靠的出口） -----------------------------------------

/**
 * 把一条判定**主动回传**给后端。
 *
 * 为什么不让驱动钻进页面里取：CDP 的 Runtime.evaluate（awaitPromise 与
 * returnByValue 同用）在本机 Chrome 上给回空对象，而「返回空对象」和
 * 「什么都没发生」长得一样 —— 那种诊断工具比没有更坏。改成页面主动 POST，
 * 驱动只读后端：**拿不到就是没拿到，不会伪装成通过。**
 *
 * 传不出去时**要看得见**（状态栏里出），不许静默 —— 这一条比功能本身重要。
 */
async function reportVerdict(name, value) {
  const base = typeof backend.baseUrl === "string" ? backend.baseUrl : "";
  if (base.length === 0) {
    log("判定没回传（" + name + "）：这个后端没有 baseUrl，回传通道只在单机/分离模式可用");
    return null;
  }
  try {
    const response = await fetch(base + "/verdict", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ name: name, value: value === undefined ? null : value }),
    });
    if (!response.ok) throw new Error("HTTP " + response.status);
    return await response.json();
  } catch (error) {
    const message = String(error && error.message ? error.message : error);
    log("判定没回传（" + name + "）：" + message);
    return null;
  }
}

/**
 * 验收判据：**同一次编辑，预览与 CLI 必须给出同一个工程。**
 *
 * op 由页面自己挑（第一条带元素的视频轨的第一个元素，出点收到中间），
 * 再把 op 与前后两份 doc 一起回传 —— 这样 driver 不必另抄一份「怎么挑元素」，
 * 两边也就不可能挑到不同的元素。
 */
async function runTrimParity(name) {
  const doc = state.doc;
  if (doc === null || doc === undefined) {
    await reportVerdict(name, { ok: false, reason: "页面里还没有工程" });
    return;
  }
  const track = doc.timeline.tracks.find((item) => item.kind === "video" && item.layers.length > 0);
  if (track === undefined) {
    await reportVerdict(name, { ok: false, reason: "没有带元素的视频轨" });
    return;
  }
  const layer = track.layers[0];
  const span = layer.end - layer.start;
  if (!(span > 1)) {
    await reportVerdict(name, { ok: false, reason: "元素太短，出点收不动" });
    return;
  }
  const op = { op: "trim", layer: layer.id, edge: "out", to: layer.start + Math.floor(span / 2) };
  const before = JSON.parse(JSON.stringify(doc));
  await runEdit(op, "验收：剃刀");
  const after = state.doc;
  const changed = JSON.stringify(before) !== JSON.stringify(after);
  await reportVerdict(name, {
    ok: changed,
    reason: changed ? "" : "编辑之后 doc 一个字都没变 —— 那这次验收什么也没验到",
    op: op,
    before: before,
    after: after,
  });
}

/**
 * 验收判据（T2.5 字幕 / T3.4 弹幕）：**画面上的字，两端必须落在同一个地方、看着同一份清单。**
 *
 * 页面只做两件事：
 *   1. seek 那一帧 —— 宿主算出两份清单（哪几行/哪几条、各占哪个归一化矩形、各落在哪几个
 *      目标像素），这里把每一条栅格化（canvas -> createImageBitmap -> 交回宿主）。
 *      字幕与弹幕是**两套清单、两套编号**（弹幕还多 泳道/进入帧/离开帧 三样结构）；
 *   2. 把宿主的两份结果**原样回传**：清单（dhampir_project_text_frame）与墨迹报告
 *      （dhampir_project_text_probe：每次"减去一行"差出来的像素落在哪、有没有被切）。
 *
 * **结论不在这里下**：「两端一致」由驱动拿 CLI 的同一帧对照着判（容差写在驱动里）。
 * 页面自己说"我一致"是没用的 —— 那正是第二份判据。这里只保证通道通、清单与位图
 * 是同一份（所以判定路径**不碰** textFrame：那会重算清单并作废刚提交的位图）。
 *
 * 帧是按样本字幕的节奏挑的（fixtures/sample-subtitle.doc.json：4 条 cue 各 2 秒 @30fps）：
 * 0 = 第一条（单行）、60 = 第二条（按估宽换成两行）、120 = 第三条（单字）、
 * 180 = 第四条（5 行，max_lines=2，丢 3 行）、240 = 全部结束之后（空）。
 * 同一条时间线上的弹幕轨（lanes=2 / duration_ms=2000）在这些帧上依次是
 * 2 / 2 / 1 / 2 / 0 条，其中 180 那一帧两条分属不同泳道 —— 弹幕那一半刻意让
 * 「泳道复用」与「闭区间边界换泳道」都被走到（见 fixtures/sample-subtitle.ass）。
 * 挑错帧不影响判据（两边**同帧**对照），只影响覆盖面。
 */
async function runSubtitleVerdict(name) {
  const report = async (ok, reason, extra) => {
    await reportVerdict(name, Object.assign({ kind: "subtitle", ok: ok, reason: reason }, extra || {}));
  };
  if (state.doc === null || state.doc === undefined) return report(false, "页面里还没有工程");
  if (subtitleAssets().length === 0) return report(false, "这份工程的资产表里没有字幕素材");

  const subtitles = await loadProjectSubtitles();
  const frames = [0, 60, 120, 180, 240];
  const manifests = [];
  const probes = [];
  let unplaced = 0;
  let unplacedDanmaku = 0;
  for (const frame of frames) {
    await state.engine.seek(frame);
    manifests.push(state.engine.textManifest);
    probes.push(await state.engine.textProbe(frame));
  }
  for (const manifest of manifests) {
    unplaced += Number(manifest.unplaced_lines) || 0;
    // 弹幕的「算不出落点」单独数：原因与字幕不同（字幕是行盒没高度/目标为 0，
    // 弹幕还多一种 —— 泳道排到画面外），混成一个数就分不清该去查哪一边。
    unplacedDanmaku += Number(manifest.unplaced_danmaku) || 0;
  }
  const ok = subtitles.failed.length === 0 && probes.length === frames.length
    && unplaced === 0 && unplacedDanmaku === 0;
  // 理由里把四个数都写出来：只说"没成立"的话，看的人还得回去数一遍。
  const reason = ok ? "" : ("字幕判定没成立：" + subtitles.failed.length + " 路没登记上、"
    + unplaced + " 行算不出落点、弹幕 " + unplacedDanmaku + " 条算不出落点、拿到 "
    + probes.length + "/" + frames.length + " 份墨迹报告");
  return report(ok, reason, { subtitles: subtitles, frames: manifests, probes: probes });
}

/** 等到条件成立；超时就返回 false。
 *
 * 判定路径上**不能只 sleep 一段固定时间** —— 那样在慢机器上会把「还没跑完」
 * 记成「没跑」（反过来也一样）。等的是**事实**（状态栏那一句变了），不是时间。 */
async function waitUntil(predicate, timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    let value = false;
    try { value = predicate(); } catch (error) { value = false; }
    if (value === true) return true;
    if (Date.now() > deadline) return false;
    await new Promise((resolve) => { setTimeout(resolve, 25); });
  }
}

/** 在工程里按 id 找一个元素的起点；找不到给 null。
 *
 * **不能按下标找**：`move` 会把那条轨道上的元素按起点重排。 */
function startOfLayer(doc, id) {
  for (const track of doc.timeline.tracks) {
    for (const layer of track.layers) {
      if (layer.id === id) return layer.start;
    }
  }
  return null;
}

/**
 * 验收判据（T4.3）：**拖一下（带着吸附）→ 撤销 → 重做**。
 *
 * 页面做三件事，**结论不在这里下**（驱动拿 CLI 与 fixture 对照着判）：
 *   1. 把某个元素的 bar **真的拖一下** —— 合成分派的 pointer 事件走的是与手拖
 *      同一条 pointerdown/move/up 路；落点刻意停在「别人的边界旁边一帧」，
 *      于是「有没有吸附」在回传的事实里看得见（原始落点不是任何边界、最终落点是）；
 *   2. 把鼠标末位移出来的**预览帧**从 `bar.style.left` 读回来 —— 那一刻
 *      `state.doc` 一个字都还没动，所以读到的只可能是预览；
 *   3. 点那两颗真按钮（撤销 / 重做）各一次，把 拖动前 / 拖动后 / 撤销后 / 重做后
 *      四份工程原样回传，并附上引擎自己给的那三句说明。
 *
 * 换算口径与拖拽实现共用同一份事实：总帧数**从界面上的百分比反解**出来
 * （渲染时用的那个数才是拖拽实现手里的那个数），再与引擎的端帧对一次 ——
 * 对不上就直说「界面上画的与模型对不上」，不猜。
 */
async function runUndoDragVerdict(name) {
  const report = async (ok, reason, extra) => {
    await reportVerdict(name, Object.assign({ kind: "undo-drag", ok: ok, reason: reason }, extra || {}));
  };
  if (state.doc === null || state.doc === undefined) return report(false, "页面里还没有工程");
  const tracks = state.doc.timeline.tracks;
  // 界面上的行：**屏幕上最下面一行才是 tracks[0]**（renderTimeline 倒序铺）。
  // 行与轨道因此不是同一个下标 —— 映射只认 renderTimeline 写下的 dataset，
  // 不在这里倒算（倒算就是第二份实现，而它错的时候不会红）。
  const rowForTrack = (trackIndex) => {
    const rows = Array.from($("timeline").querySelectorAll(".track"));
    const found = rows.find((row) => Number(row.dataset.trackIndex) === trackIndex);
    if (found === undefined) {
      throw new Error("时间线上找不到 tracks[" + trackIndex + "] 对应的行 —— 行序与轨道序的映射断了");
    }
    return found;
  };
  // 行与 bar **每次都现取**：一次成功改动会把时间线整块重画，旧节点随即脱离文档 ——
  // 拖完还攥着同一个节点去读 `style.left`，读到的是被换掉的那一份。
  const barOf = (trackIndex, layerId) => {
    const row = rowForTrack(trackIndex);
    const bar = Array.from(row.querySelectorAll(".layer")).find((item) => item.textContent === layerId);
    return bar === undefined ? null : { row: row, bar: bar };
  };
  // 总帧数**从界面上反解**：挑一条没被最小宽度截断的 bar，把 left / width 两个百分比
  // 与模型里的起点 / 长度联立 —— 渲染时用的那个数才是拖拽实现手里的那个数。
  let end = null;
  let geometry = null;
  for (const track of tracks) {
    const trackIndex = tracks.indexOf(track);
    const row = rowForTrack(trackIndex);
    if (row === undefined) continue;
    const bars = Array.from(row.querySelectorAll(".layer"));
    for (let index = 0; index < track.layers.length; index += 1) {
      const bar = bars[index];
      const layer = track.layers[index];
      if (bar === undefined || layer === undefined) break;
      const widthPct = Number.parseFloat(bar.style.width);
      const leftPct = Number.parseFloat(bar.style.left);
      const length = layer.end - layer.start;
      if (!(widthPct > 1.5) || !(length > 0)) continue;
      const solved = Math.round(length / (widthPct / 100));
      if (!(solved > 0)) continue;
      if (Math.abs(layer.start / solved * 100 - leftPct) > 0.5) {
        return report(false, "界面上的位置与模型对不上：" + layer.id + " 的 left 是 " + bar.style.left
          + "、按 " + solved + " 帧算是 " + (layer.start / solved * 100) + "%");
      }
      end = solved;
      geometry = layer.id + " 的条反解出总帧数 " + solved + "（left " + bar.style.left + "、宽 " + bar.style.width + "）";
      break;
    }
    if (end !== null) break;
  }
  if (end === null) {
    return report(false, "时间线上每个元素都太短（宽度都被最小宽度截断过）—— 反解不出总帧数");
  }
  if (end !== Math.max(1, state.engine.endFrame())) {
    return report(false, "界面上画的总帧数（" + end + "）与引擎的端帧（" + state.engine.endFrame()
      + "）对不上 —— 时间线画的是旧的");
  }
  // 别人的边界：拖谁就按「除它以外」算一份（与拖拽实现里的候选集是同一套算法）。
  const othersBoundaries = (layerId) => {
    const out = [];
    for (const track of tracks) {
      for (const layer of track.layers) {
        if (layer.id === layerId) continue;
        out.push(layer.start, layer.end);
      }
    }
    return out;
  };
  // 场景：把某个元素拖到**别人的边界旁边一帧**上 —— 只要吸附生效，它就该落在那条边界上。
  // 顺序上先挑**只有一层**的轨道：那种轨道上「同轨重叠」这条规则不可能触发
  // （规则还在 Rust 里，这里只是挑个大概率一次就过的场景，不代替它下结论）。
  const single = tracks.filter((track) => track.layers.length === 1);
  const rest = tracks.filter((track) => track.layers.length !== 1);
  const scenes = [];
  const seen = new Set();
  for (const track of single.concat(rest)) {
    const trackIndex = tracks.indexOf(track);
    for (const layer of track.layers) {
      const boundaries = othersBoundaries(layer.id);
      const ordered = boundaries.slice().sort((a, b) => Math.abs(a - layer.start) - Math.abs(b - layer.start));
      for (const boundary of ordered) {
        if (boundary === layer.start) continue;
        for (const candidate of [boundary + 1, boundary - 1]) {
          if (candidate < 0 || candidate === layer.start || boundaries.includes(candidate)) continue;
          const key = layer.id + "@" + boundary + "@" + candidate;
          if (seen.has(key)) continue;
          seen.add(key);
          scenes.push({ trackIndex: trackIndex, layer: layer, boundary: boundary, candidate: candidate });
          break;
        }
      }
    }
  }
  if (scenes.length === 0) {
    return report(false, "这份工程里挑不出「停在边界旁边」的落点 —— 这一趟验不到吸附");
  }
  const before = JSON.parse(JSON.stringify(state.doc));
  const pointer = (type, x, buttons) => new PointerEvent(type, {
    bubbles: true, cancelable: true, button: 0, buttons: buttons,
    clientX: x, clientY: 8, pointerId: 1, pointerType: "mouse", isPrimary: true,
  });
  // 逐场景试放。**收不收由引擎说**（状态栏那句话变了 = 引擎表过态了），
  // 被拒（比如同轨重叠）就换下一个场景 —— 这里不复制任何一条规则、也不预测结果：
  // 预测一份的话，「规则只有一份」这句话在这里就断了。
  const attempts = [];
  let accepted = null;
  for (const scene of scenes) {
    // 每次试放都从干净的界面开始：行与 bar 按 id 现取，不跨场景攥着旧节点。
    renderTimeline();
    await seekTo(state.frame);
    const located = barOf(scene.trackIndex, scene.layer.id);
    if (located === null) {
      attempts.push(scene.layer.id + " 在时间线上找不到");
      continue;
    }
    const trackWidth = located.row.clientWidth;
    if (!(trackWidth > 0)) {
      attempts.push("轨道 " + scene.trackIndex + " 量不到宽度");
      continue;
    }
    const framesPerPixel = end / trackWidth;
    const snapFrames = Math.max(0, Math.round(6 * framesPerPixel));
    // 位移先按像素算，落点再**从界面上读回来**核对：像素↔帧的换算这条路上有两处
    // （这里一份、拖拽实现里一份），只信自己算的那一份会把「没吸上」记成「吸上了」。
    const startX = 120;
    const from = scene.layer.start;
    const wantedX = startX + (scene.candidate - from) / framesPerPixel;
    if (!Number.isFinite(wantedX)) {
      attempts.push("把 " + scene.layer.id + " 挪到第 " + scene.candidate + " 帧的像素换算不成立");
      continue;
    }
    const hintBefore = state.hint;
    const bar = located.bar;
    bar.dispatchEvent(pointer("pointerdown", startX, 1));
    let usedX = wantedX;
    let preview = null;
    for (const nudge of [0, -1, 1, -2, 2, -3, 3, -5, 5, -8, 8, -13, 13]) {
      usedX = wantedX + nudge;
      bar.dispatchEvent(pointer("pointermove", usedX, 1));
      const movedPct = Number.parseFloat(bar.style.left);
      preview = Number.isFinite(movedPct) ? Math.round(movedPct / 100 * end) : null;
      if (preview === scene.candidate) break;
    }
    bar.dispatchEvent(pointer("pointerup", usedX, 0));
    // 等引擎表个态：**成了与拒了都会写一句** —— 所以「没变」的意思是这一拖根本没生成编辑。
    const spoke = await waitUntil(() => state.hint !== hintBefore, 4000);
    if (!spoke) {
      attempts.push("把 " + scene.layer.id + " 挪到第 " + scene.candidate + " 帧：拖了但状态栏没动静");
      continue;
    }
    const dragged = JSON.parse(JSON.stringify(state.doc));
    const landed = startOfLayer(dragged, scene.layer.id);
    // 起点一动没动 = 工程一个字没变（引擎把人拒了）。场景挑边界时就跳过了起点本身，
    // 所以「没动」与「落在场景那条边界上」是互斥的两条路，没有第三种。
    if (landed === scene.layer.start) {
      attempts.push("把 " + scene.layer.id + " 挪到第 " + scene.candidate + " 帧：落地在 " + landed
        + " 帧（引擎说：" + state.hint + "）");
      continue;
    }
    if (landed !== scene.boundary) {
      // 起点**真的变了**，只是没落在场景挑的那条边界上：这一拖生效了，不是「被拒」。
      // 混进「被拒」里继续往下试会把现场冲掉（工程已经改了），所以就地报出去。
      return report(false, "把 " + scene.layer.id + " 挪到第 " + scene.candidate + " 帧：这一拖生效了，"
        + "却落在第 " + landed + " 帧，不是场景挑的第 " + scene.boundary + " 帧（引擎说：" + state.hint + "）",
        { end: end, scene: scene, landed: landed, dragged: dragged, attempts: attempts });
    }
    accepted = {
      scene: scene,
      from: from,
      preview: preview,
      landed: landed,
      dragged: dragged,
      dragHint: state.hint,
      snapFrames: snapFrames,
      trackWidth: trackWidth,
      pixels: Math.round(usedX - startX),
    };
    break;
  }
  if (accepted === null) {
    // 一个场景都没落地就不要报「吸上了」：如实说每一次都被拒了。
    return report(false, "这份工程里每次试放都没拖成（" + attempts.length + " 次）：" + attempts.join("；"),
      { end: end, scenes: scenes.length, attempts: attempts });
  }
  const scene = accepted.scene;
  const layerId = scene.layer.id;
  // 引擎自己会给的那三句说明（runEdit / runHistoryStep 就是这么拼的）。
  const label = "把 " + layerId + " 移到第 " + scene.boundary + " 帧";
  const problems = [];
  if (accepted.preview !== scene.candidate) {
    problems.push("预览停在 " + accepted.preview + " 帧，不是指针指的 " + scene.candidate + " 帧");
  }
  if (accepted.dragHint !== "移动：" + label) {
    problems.push("引擎的说明是 " + JSON.stringify(accepted.dragHint) + "，不是 " + JSON.stringify("移动：" + label));
  }
  // 撤销 / 重做：**点那两颗真按钮**，走的就是用户点的那条路。
  $("undoBtn").click();
  const undoDone = await waitUntil(() => state.hint !== accepted.dragHint, 4000);
  const undoHint = state.hint;
  const undoneDoc = JSON.parse(JSON.stringify(state.doc));
  const undoneStart = startOfLayer(undoneDoc, layerId);
  if (!undoDone) problems.push("撤销之后状态栏没变（还停在 " + JSON.stringify(accepted.dragHint) + "）");
  if (undoneStart !== accepted.from) {
    problems.push("撤销之后 " + layerId + " 在 " + undoneStart + " 帧，不是拖动前的 " + accepted.from + " 帧");
  }
  if (undoHint !== "撤销：" + label) {
    problems.push("撤销的说明是 " + JSON.stringify(undoHint) + "，不是 " + JSON.stringify("撤销：" + label));
  }
  $("redoBtn").click();
  const redoDone = await waitUntil(() => state.hint !== undoHint, 4000);
  const redoHint = state.hint;
  const redoneDoc = JSON.parse(JSON.stringify(state.doc));
  const redoneStart = startOfLayer(redoneDoc, layerId);
  if (!redoDone) problems.push("重做之后状态栏没变（还停在 " + JSON.stringify(undoHint) + "）");
  if (redoneStart !== scene.boundary) {
    problems.push("重做之后 " + layerId + " 在 " + redoneStart + " 帧，不是 " + scene.boundary + " 帧");
  }
  if (redoHint !== "重做：" + label) {
    problems.push("重做的说明是 " + JSON.stringify(redoHint) + "，不是 " + JSON.stringify("重做：" + label));
  }
  return report(problems.length === 0, problems.join("；"), {
    drag: {
      layer: layerId,
      from: accepted.from,
      boundary: scene.boundary,
      candidate: scene.candidate,
      preview: accepted.preview,
      landed: accepted.landed,
      snapFrames: accepted.snapFrames,
      end: end,
      trackWidth: accepted.trackWidth,
      pixels: accepted.pixels,
      geometry: geometry,
      attempts: attempts,
    },
    hints: { drag: accepted.dragHint, undo: undoHint, redo: redoHint },
    before: before,
    dragged: accepted.dragged,
    undone: undoneDoc,
    redone: redoneDoc,
  });
}

/**
 * 验收判据（T2）：**拖手柄真的能改时长，而且改的是 Rust 认的那两个数。**
 *
 * 页面做三件事，结论不在这里下：
 *   1. 找一条能变长的元素，拖它的**右手柄**左移若干个像素；
 *   2. 把拖拽前后两份工程 + 手柄的几何（行宽、总帧数、实际位移像素）回传；
 *   3. 顺带验左手柄：拖入点时 **source_in 必须跟着变**（那是 trim 的核心语义，
 *      也是"前端别自己写 start/end"的理由 —— 直接写数字必然漏掉它）。
 *
 * **拖动必须走真手柄上的真 pointer 事件**：直接调 runEdit 就成了"验 runEdit 能不能用"，
 * 而这一条要验的是**手柄接上了没有** —— 手柄没 append、被过渡标记盖住、
 * pointerdown 被 bar 抢走，这三种毛病都只有真拖一遍才发现得了。
 */
async function runTrimDragVerdict(name) {
  const report = async (ok, reason, extra) => {
    await reportVerdict(name, Object.assign({ kind: "trim-drag", ok: ok, reason: reason }, extra || {}));
  };
  if (state.doc === null || state.doc === undefined) return report(false, "页面里还没有工程");
  const doc = state.doc;
  const tracks = doc.timeline.tracks;
  const end = Math.max(1, state.engine.endFrame());
  if (!(end > 1)) return report(false, "工程只有一帧，修剪验不出东西");

  // 挑一条**视频轨**上"右边还有余量"的元素：拖右手柄左移才缩短得了。
  // 用 dataset 找行 —— 行序与轨道序不是同一个下标（见 renderTimeline 的倒序铺）。
  const rows = Array.from($("timeline").querySelectorAll(".track"));
  // **挑元素要有判据，不能撞上谁算谁。** 这一条要同时验"出点会动"与"入点动时
  // source_in 跟着走"，所以候选必须满足：长度够、**source_in 够大**（不然入点左移会撞素材头，
  // 引擎正确地拒绝，而看起来像"手柄没接上"）。
  // 第一版没写这条，挑中了 source_in = 0 的 a，于是如实报了"验不到入点语义"——
  // 报告是对的，但也说明**这一趟什么也没验到**：探针挑不中场景时不该假装通过。
  const candidates = [];
  for (const track of tracks) {
    const trackIndex = tracks.indexOf(track);
    const row = rows.find((item) => Number(item.dataset.trackIndex) === trackIndex);
    if (row === undefined) continue;
    const bars = Array.from(row.querySelectorAll(".layer"));
    for (let index = 0; index < track.layers.length; index += 1) {
      const layer = track.layers[index];
      const bar = bars[index];
      if (bar === undefined) continue;
      if (!(bar.querySelector(".handle.l") && bar.querySelector(".handle.r"))) continue;
      // 出点要能左移至少 4 帧、source_in 够大，**而且左边要有空间**：
      // 入点左移撞上同轨前一个元素时，引擎会（正确地）回 layer_overlap ——
      // 那是一次**规则生效**，不是手柄坏了。探针要挑能过的场景，否则报出来的
      // "入点没变"会把"规则拦住了"误读成"手柄没接上"。
      const sourceIn = layer.source ? Number(layer.source.source_in) : -1;
      if (!(layer.end - layer.start > 4)) continue;
      if (!(sourceIn > 2)) continue;
      // 左边有多少帧可退：同轨上**结束点不超过它起点**的那些元素里，取最大出点。
      // 没有那样的元素就一路退到 0。**别写成 min**：那会把"左边很空"算成 0。
      let leftEdge = 0;
      for (const other of track.layers) {
        if (other === layer) continue;
        if (other.end <= layer.start && other.end > leftEdge) leftEdge = other.end;
      }
      const leftRoom = layer.start - leftEdge;
      if (!(leftRoom >= 4)) continue;
      const score = Math.min(layer.end - layer.start - 4, sourceIn, leftRoom);
      candidates.push({ trackIndex: trackIndex, row: row, bar: bar, layer: layer, score: score });
    }
  }
  candidates.sort((a, b) => b.score - a.score);
  const picked = candidates.length > 0 ? candidates[0] : null;
  if (picked === null) {
    return report(false, "没有任何元素同时具备左右手柄、可修剪长度、以及够大的 source_in —— 手柄没渲染出来，或这份工程验不到入点语义");
  }
  const trackWidth = picked.row.clientWidth;
  if (!(trackWidth > 0)) return report(false, "量不到轨道宽度");
  const framesPerPixel = end / trackWidth;
  // 位移取**至少 3 帧**，免得被吸附阈值吃成 0 帧（那会被当成"没拖成"，而不是"没生效"）。
  const wantedFrames = Math.max(3, Math.round(6 * framesPerPixel) + 2);
  const dx = -Math.round(wantedFrames / framesPerPixel);

  const pointer = (target, type, x, buttons, y) => target.dispatchEvent(new PointerEvent(type, {
    bubbles: true, cancelable: true, button: 0, buttons: buttons,
    clientX: x, clientY: y, pointerId: 1, pointerType: "mouse", isPrimary: true,
  }));

  const before = JSON.parse(JSON.stringify(state.doc));
  const layerId = picked.layer.id;
  const fromStart = picked.layer.start;
  const fromEnd = picked.layer.end;
  const fromSourceIn = picked.layer.source ? picked.layer.source.source_in : null;

  // ---- 右手柄：改出点 ----
  const rightRect = picked.bar.querySelector(".handle.r").getBoundingClientRect();
  const rx = rightRect.left + rightRect.width / 2;
  const ry = rightRect.top + 2;
  const hintBefore = state.hint;
  pointer(picked.bar.querySelector(".handle.r"), "pointerdown", rx, 1, ry);
  pointer(picked.bar.querySelector(".handle.r"), "pointermove", rx + dx, 1, ry);
  pointer(picked.bar.querySelector(".handle.r"), "pointerup", rx + dx, 0, ry);
  const spoke = await waitUntil(() => state.hint !== hintBefore, 4000);
  if (!spoke) {
    return report(false, "拖了右手柄，但状态栏没动静 —— 手柄上的 pointerdown 没接上（或被 bar 抢走了）",
      { before: before, hint: state.hint, wantedFrames: wantedFrames, dx: dx });
  }
  const afterOut = JSON.parse(JSON.stringify(state.doc));
  const outLayer = findLayerIn(afterOut, layerId);
  if (outLayer === null) return report(false, "拖完之后工程里没有 " + layerId + " 了");
  const outChanged = outLayer.end !== fromEnd;
  if (!outChanged) {
    return report(false, "拖右手柄之后出点没变（还是第 " + fromEnd + " 帧；引擎说：" + state.hint + "）",
      { before: before, after: afterOut, hint: state.hint });
  }
  if (outLayer.start !== fromStart) {
    return report(false, "拖右手柄把**起点**也改了：" + fromStart + " -> " + outLayer.start + "（出点只该动 end）",
      { before: before, after: afterOut, hint: state.hint });
  }
  if (outLayer.source && fromSourceIn !== null && outLayer.source.source_in !== fromSourceIn) {
    return report(false, "拖出点时 source_in 被改了：" + fromSourceIn + " -> " + outLayer.source.source_in
      + "（出点不该碰素材入点）", { before: before, after: afterOut, hint: state.hint });
  }
  const outDuration = outLayer.end - outLayer.start;

  // ---- 左手柄：改入点，source_in 必须跟着走 ----
  const hintBefore2 = state.hint;
  // 重画之后行与条都是**新节点**，按 dataset 的轨道序现取（不跨步攥旧节点）。
  const rows2 = Array.from($("timeline").querySelectorAll(".track"));
  const row2 = rows2.find((item) => Number(item.dataset.trackIndex) === picked.trackIndex);
  const bar2 = row2 === undefined ? null
    : Array.from(row2.querySelectorAll(".layer")).find((item) => item.textContent === layerId);
  if (bar2 === null || bar2 === undefined) return report(false, "重画之后找不到 " + layerId + " 的条");
  const leftRect = bar2.querySelector(".handle.l").getBoundingClientRect();
  const lx = leftRect.left + leftRect.width / 2;
  const ly = leftRect.top + 2;
  const startBeforeIn = outLayer.start;
  const sourceInBefore = outLayer.source ? outLayer.source.source_in : null;
  if (!(sourceInBefore > 2)) {
    // 入点为 0 时左移会撞素材头，验不出"source_in 跟着走" —— 如实说，不硬凑。
    return report(false, "元素的 source_in 是 " + sourceInBefore + "，不够左移 —— 这一趟验不到入点语义",
      { before: before, after: afterOut, outChanged: outChanged });
  }
  // 左移入点 = 往素材前面多要内容 => source_in 变小。
  pointer(bar2.querySelector(".handle.l"), "pointerdown", lx, 1, ly);
  pointer(bar2.querySelector(".handle.l"), "pointermove", lx + dx, 1, ly);
  pointer(bar2.querySelector(".handle.l"), "pointerup", lx + dx, 0, ly);
  const spoke2 = await waitUntil(() => state.hint !== hintBefore2, 4000);
  if (!spoke2) {
    return report(false, "拖了左手柄，但状态栏没动静", { before: before, after: afterOut, hint: state.hint });
  }
  const afterIn = JSON.parse(JSON.stringify(state.doc));
  const inLayer = findLayerIn(afterIn, layerId);
  if (inLayer === null) return report(false, "拖入点之后工程里没有 " + layerId + " 了");
  const inChanged = inLayer.start !== startBeforeIn;
  const sourceInFollowed = inLayer.source ? inLayer.source.source_in !== sourceInBefore : false;
  if (!inChanged) {
    return report(false, "拖左手柄之后入点没变（还是第 " + startBeforeIn + " 帧；引擎说：" + state.hint + "）",
      { before: before, after: afterIn, hint: state.hint });
  }
  if (!sourceInFollowed) {
    return report(false, "入点变了（" + startBeforeIn + " -> " + inLayer.start + "）但 source_in 没跟着变（还是 "
      + sourceInBefore + "）—— 那不是 trim 的语义", { before: before, after: afterIn, hint: state.hint });
  }
  // 入点左移意味着往素材前面多要：source_in 必须**变小**，变大就是把素材放反了。
  if (!(inLayer.source.source_in < sourceInBefore)) {
    return report(false, "入点左移了，source_in 反而变大（" + sourceInBefore + " -> " + inLayer.source.source_in
      + "）—— 方向反了", { before: before, after: afterIn, hint: state.hint });
  }

  return report(true, "", {
    layer: layerId,
    end: end,
    trackWidth: trackWidth,
    snapFrames: Math.max(0, Math.round(6 * framesPerPixel)),
    pixels: dx,
    out: { from: fromEnd, to: outLayer.end, durationBefore: fromEnd - fromStart, durationAfter: outDuration },
    in: { from: startBeforeIn, to: inLayer.start, sourceInBefore: sourceInBefore, sourceInAfter: inLayer.source.source_in },
    hints: { out: state.hint },
    before: before,
    afterOut: afterOut,
    afterIn: afterIn,
  });
}

/** 在任意一份工程里按 id 找一个元素（跨轨道）。找不到给 null。 */
function findLayerIn(doc, id) {
  for (const track of doc.timeline.tracks) {
    for (const layer of track.layers) {
      if (layer.id === id) return layer;
    }
  }
  return null;
}

/**
 * 验收判据（T3）：**播放真的按帧号推进，且到末帧自己停住。**
 *
 * 这一条验的是"有没有第二个时钟"：如果播放是靠 `<video>.play()` 实现的，
 * 帧号就不会随它走（画面动、帧号不动），而那种实现看起来"能播"。
 * 所以判据只看**帧号**：起点、经过若干毫秒之后到了第几帧、末帧之后停没停。
 *
 * 另外验一条反向的：**播放中点别处必须能接管** —— 手动 seek 之后播放要停，
 * 否则用户拖滑块会被播放头拽回去。
 */
async function runPlaybackVerdict(name) {
  const report = async (ok, reason, extra) => {
    await reportVerdict(name, Object.assign({ kind: "playback", ok: ok, reason: reason }, extra || {}));
  };
  if (state.doc === null || state.doc === undefined) return report(false, "页面里还没有工程");
  const engine = state.engine;
  const end = Math.max(1, engine.endFrame());
  if (end < 8) return report(false, "工程只有 " + end + " 帧，播放推进验不出来");

  const fps = sequenceFps();
  // 先确保是停着的、并且在起点。
  pause();
  await seekTo(0);
  const frameBefore = state.frame;
  const playingBefore = isPlaying();

  // ---- 播放：等一段**按帧率算出来**的时间，看帧号有没有跟上 ----
  // 等的是"事实"（帧号变了），不是"睡够多久就一定对" —— 慢机器上固定 sleep 会把
  // "还没跑到"记成"没在跑"。
  play();
  const startedAt = performance.now();
  const wantFrames = 5;
  const budgetMs = wantFrames / fps * 1000 * 12 + 4000;   // 宽裕：慢机器也要能跑到
  const advanced = await waitUntil(() => state.frame >= wantFrames, budgetMs);
  const elapsedMs = performance.now() - startedAt;
  const frameAfter = state.frame;
  const playingDuring = isPlaying();
  pause();
  if (!advanced) {
    return report(false, "播放之后 " + Math.round(budgetMs) + "ms 内帧号只走到 " + frameAfter
      + "（期望至少 " + wantFrames + "）—— 播放头没在推进",
      { fps: fps, end: end, frameBefore: frameBefore, frameAfter: frameAfter });
  }
  if (!playingDuring) {
    return report(false, "帧号推进了，但 isPlaying 已经是 false —— 播放状态没被记着", { frameAfter: frameAfter });
  }
  // 推进速度要**量级正确**：按帧率算，这段时间该走 elapsedMs/1000*fps 帧上下。
  // 允许很宽的区间（rAF 粒度、渲染耗时都算在内），只拦"快了几十倍/慢了几十倍"那种
  // —— 那种说明用的不是同一个帧率。
  const expectedNow = elapsedMs / 1000 * fps;
  const ratio = expectedNow > 0 ? frameAfter / expectedNow : 0;
  if (!(ratio > 0.2 && ratio < 5)) {
    return report(false, "推进速度与序列帧率对不上：经过 " + Math.round(elapsedMs) + "ms（" + fps
      + "fps 该走约 " + expectedNow.toFixed(1) + " 帧），实际走到第 " + frameAfter + " 帧",
      { fps: fps, elapsedMs: Math.round(elapsedMs), frameAfter: frameAfter, expected: expectedNow });
  }

  // ---- 播放中手动 seek：播放必须让位 ----
  pause();
  await seekTo(0);
  play();
  await waitUntil(() => state.frame >= 2, 4000);
  const beforeManual = state.frame;
  await seekTo(end - 2);   // 手动跳近末尾
  // seekTo 本身不暂停（它不是"用户操作"）—— 但**滑块与快捷键会**。
  // 这里验的是"用户那条路"：模拟一次滑块 input。
  const slider = $("frame");
  slider.value = String(Math.max(0, end - 3));
  slider.dispatchEvent(new Event("input", { bubbles: true }));
  const pausedByUser = !isPlaying();
  pause();
  const afterManual = state.frame;
  if (!pausedByUser) {
    return report(false, "拖动滑块之后还在播 —— 用户手动定位会被播放头拽回去",
      { beforeManual: beforeManual, afterManual: afterManual });
  }

  // ---- 末帧：播放到末尾必须自己停 ----
  pause();
  const tailStart = Math.max(0, end - 3);
  await seekTo(tailStart);
  play();
  const reachedEnd = await waitUntil(() => state.frame >= end - 1 && !isPlaying(), 8000);
  const finalFrame = state.frame;
  const stillPlaying = isPlaying();
  pause();
  if (!reachedEnd) {
    return report(false, "从第 " + tailStart + " 帧播到第 " + finalFrame + " 帧之后没有自动停"
      + "（末帧是第 " + (end - 1) + " 帧，isPlaying=" + stillPlaying + "）",
      { end: end, finalFrame: finalFrame, stillPlaying: stillPlaying });
  }
  if (finalFrame !== end - 1) {
    return report(false, "停了，但停在 " + finalFrame + " 帧而不是末帧 " + (end - 1));
  }

  return report(true, "", {
    fps: fps,
    end: end,
    advancedTo: frameAfter,
    elapsedMs: Math.round(elapsedMs),
    expectedFrames: Number(expectedNow.toFixed(2)),
    pausedByUser: pausedByUser,
    stoppedAt: finalFrame,
    // **播放实测**：只报帧号对不对的话，性能好不好这件事在判定里完全看不见。
    // 带上代价分解，"能播"与"播得顺"才是两件分别可查的事。
    stats: playbackStats(),
  });
}

/**
 * 验收判据（T4）：**音轨在界面上看得见、选得中、拖得动。**
 *
 * 用户明确说音频不要特效，只要"能剪辑和基本功能" —— 所以这一条只验编辑面：
 *   1. 音轨**有自己的行**（不是被视频轨吞了，也不是画在屏幕外）；
 *   2. 音轨上的元素**选得中**（点一下要真的变成选中态）；
 *   3. 音轨元素**拖得动**（走与视频同一条 move 路）；
 *   4. 音轨**不参与画面合成** —— 这是既定语义（`compose.rs`），
 *      所以同一帧上"有没有音轨"不该改变画面。
 *
 * 第 4 条是**反向判据**：它盯的是"以后有人图省事把音轨也塞进合成"。
 */
async function runAudioTrackVerdict(name) {
  const report = async (ok, reason, extra) => {
    await reportVerdict(name, Object.assign({ kind: "audio-track", ok: ok, reason: reason }, extra || {}));
  };
  if (state.doc === null || state.doc === undefined) return report(false, "页面里还没有工程");
  const doc = state.doc;
  const tracks = doc.timeline.tracks;
  const audioIndex = tracks.findIndex((track) => track.kind === "audio");
  if (audioIndex < 0) return report(false, "这份工程的轨道里没有 audio 轨");

  // ---- 1. 音轨有自己的行，且行在模型里的顺序与 dataset 一致 ----
  const rows = Array.from($("timeline").querySelectorAll(".track"));
  const audioRow = rows.find((row) => Number(row.dataset.trackIndex) === audioIndex);
  if (audioRow === undefined) {
    return report(false, "音轨 tracks[" + audioIndex + "] 在时间线上没有对应的行",
      { trackKinds: tracks.map((t) => t.kind), domRows: rows.map((r) => r.dataset.trackIndex) });
  }
  const audioTrack = tracks[audioIndex];
  if (audioTrack.layers.length === 0) return report(false, "音轨上一个元素都没有");
  const bars = Array.from(audioRow.querySelectorAll(".layer"));
  if (bars.length !== audioTrack.layers.length) {
    return report(false, "音轨有 " + audioTrack.layers.length + " 个元素，界面上画了 " + bars.length + " 条");
  }
  const label = audioRow.querySelector(".track-label").textContent;
  if (!label.includes(audioTrack.id) || !label.includes("audio")) {
    return report(false, "音轨那一行的标签没写清是哪条轨道：" + JSON.stringify(label));
  }

  // ---- 2. 音轨元素选得中 ----
  const targetLayer = audioTrack.layers[0];
  const targetBar = bars[0];
  targetBar.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true }));
  await new Promise((resolve) => { setTimeout(resolve, 80); });
  const selected = state.selected;
  if (selected === null || selected.trackIndex !== audioIndex || selected.layerIndex !== 0) {
    return report(false, "点了音轨元素之后没选中它（选中态是 " + JSON.stringify(selected) + "）");
  }
  const selectedNow = selectedLayer();
  if (selectedNow === null || selectedNow.id !== targetLayer.id) {
    return report(false, "选中了，但选中的不是 " + targetLayer.id + "（是 "
      + (selectedNow === null ? "null" : selectedNow.id) + "）");
  }

  // ---- 3. 音轨元素拖得动（真发 pointer 事件，走与视频同一条路） ----
  const rows2 = Array.from($("timeline").querySelectorAll(".track"));
  const audioRow2 = rows2.find((row) => Number(row.dataset.trackIndex) === audioIndex);
  const bar2 = Array.from(audioRow2.querySelectorAll(".layer")).find((item) => item.textContent === targetLayer.id);
  if (bar2 === undefined) return report(false, "重画之后找不到音轨上的 " + targetLayer.id);
  const end = Math.max(1, state.engine.endFrame());
  const trackWidth = audioRow2.clientWidth;
  if (!(trackWidth > 0)) return report(false, "量不到音轨行的宽度");
  const framesPerPixel = end / trackWidth;
  const before = JSON.parse(JSON.stringify(state.doc));
  const fromStart = targetLayer.start;
  const startX = 60;
  // 往右挪**至少 3 帧**，免得被吸附吃成 0 帧。
  const wanted = Math.max(3, Math.round(6 * framesPerPixel) + 2);
  const dx = Math.round(wanted / framesPerPixel);
  const pointer = (type, x, buttons) => bar2.dispatchEvent(new PointerEvent(type, {
    bubbles: true, cancelable: true, button: 0, buttons: buttons,
    clientX: x, clientY: 8, pointerId: 1, pointerType: "mouse", isPrimary: true,
  }));
  const hintBefore = state.hint;
  pointer("pointerdown", startX, 1);
  pointer("pointermove", startX + dx, 1);
  pointer("pointerup", startX + dx, 0);
  const spoke = await waitUntil(() => state.hint !== hintBefore, 4000);
  if (!spoke) {
    return report(false, "拖了音轨元素，状态栏没动静 —— 音轨上的元素拖不动",
      { before: before, hint: state.hint });
  }
  const after = JSON.parse(JSON.stringify(state.doc));
  const moved = findLayerIn(after, targetLayer.id);
  if (moved === null) return report(false, "拖完之后工程里没有 " + targetLayer.id + " 了");
  if (moved.start === fromStart) {
    return report(false, "拖了音轨元素但起点没变（还是第 " + fromStart + " 帧；引擎说：" + state.hint + "）",
      { before: before, after: after, hint: state.hint });
  }
  // 移动**不该改长度**（那是两条不同的编辑）。
  if (moved.end - moved.start !== targetLayer.end - targetLayer.start) {
    return report(false, "挪动音轨元素把长度改了：" + (targetLayer.end - targetLayer.start)
      + " -> " + (moved.end - moved.start) + " 帧");
  }
  // 音轨元素仍应是 audio 轨上的（没被挪到别的轨道）。
  const stillAudio = after.timeline.tracks[audioIndex].layers.some((l) => l.id === targetLayer.id);
  if (!stillAudio) return report(false, "挪动之后 " + targetLayer.id + " 不在音轨上了");

  // ---- 4. 音频源真的接上了，且能被播放头驱动 ----
  const wantedIds = audioAssetIdsInUse();
  const bound = Array.from(audioSources.keys());
  if (bound.length === 0) {
    return report(false, "音轨上的素材一个都没挂上 <audio> —— 播放时不会有声音",
      { wanted: wantedIds, bound: bound });
  }
  if (bound.length !== wantedIds.length) {
    return report(false, "音轨引用了 " + wantedIds.length + " 个素材，只挂上 " + bound.length
      + " 路（" + wantedIds.join("/") + " -> " + bound.join("/") + "）");
  }
  // 对齐：跳到某帧时，音频的 currentTime 应当等于该帧换算出的秒数。
  const base = state.doc.timeline.timebase;
  const probeFrame = Math.max(0, Math.min(10, end - 1));
  await seekTo(probeFrame);
  const wantSeconds = probeFrame * base.den / base.num;
  const drifts = [];
  for (const [assetId, audio] of audioSources.entries()) {
    if (!Number.isFinite(audio.duration)) { drifts.push(assetId + " 没有可用时长"); continue; }
    const drift = Math.abs(audio.currentTime - wantSeconds);
    // 容差与 syncAudioToFrame 一致：小于 80ms 是它**故意不纠**的范围。
    if (drift > 0.08 + 0.02) {
      drifts.push(assetId + " 偏离 " + drift.toFixed(3) + "s（应在第 " + wantSeconds.toFixed(3) + "s）");
    }
  }
  if (drifts.length > 0) {
    return report(false, "音频没有跟着播放头对齐：" + drifts.join("；"),
      { probeFrame: probeFrame, wantSeconds: wantSeconds });
  }
  // 停播时不许有声音在放（"按了暂停还在响"是最容易被忽略的一类毛病）。
  const stillPlaying = Array.from(audioSources.values()).filter((audio) => !audio.paused);
  if (stillPlaying.length > 0) {
    return report(false, "没在播放，但有 " + stillPlaying.length + " 路音频还在放");
  }

  return report(true, "", {
    trackId: audioTrack.id,
    layers: audioTrack.layers.map((l) => l.id),
    label: label,
    selected: selectedNow.id,
    moved: { layer: targetLayer.id, from: fromStart, to: moved.start },
    hint: state.hint,
    audio: { assets: wantedIds, bound: bound, probeFrame: probeFrame, wantSeconds: wantSeconds },
    before: before,
    after: after,
  });
}

/**
 * 性能判据（测量用，不是验收）：**把逐帧 seek 的代价拆开，别猜。**
 *
 * # 为什么要有这一条
 *
 * "预览卡"是个感受，不是一个可查的数。拆开之后只有两件事可能出问题：
 * 素材解码（改不了，但能选素材）与这条链路的开销（能改）。
 * 不拆开就只能在两侧同时瞎改 —— 而改错的那一侧会让画面悄悄变差。
 *
 * 它**不回传 ok/reason 那套判定**，只报数字；驱动侧拿它跟帧率预算比。
 * 判据是"数字够不够跑满帧率"，不是"通过/不通过"。
 */
async function runPerfVerdict(name) {
  const engine = state.engine;
  if (state.doc === null || state.doc === undefined) {
    return reportVerdict(name, { kind: "perf", ok: false, reason: "页面里还没有工程" });
  }
  const end = Math.max(1, engine.endFrame());
  const frameMs = 1000 / sequenceFps();
  const samples = Math.min(24, Math.max(4, end - 1));
  const step = Math.max(1, Math.floor((end - 2) / samples));

  // **一个素材一条**：不能只看第 0 帧需要哪些源 —— 那份工程第 0 帧只用到一路，
  // 于是四路分辨率只会印出一路（第一次跑就是这样，差点把结论下错）。
  // 遍历整个序列、把所有出现过的 source 收齐。
  const seenSources = new Map();
  for (let probeFrame = 0; probeFrame < end; probeFrame += 1) {
    for (const entry of engine.sourcesFor(probeFrame)) {
      if (seenSources.has(entry.source)) continue;
      const video = engine.videos.get(entry.source);
      if (video === undefined) continue;
      seenSources.set(entry.source, { source: entry.source, video: video });
    }
  }
  // **逐路自己量**：四路混成一个均值，均值恰好掩盖最慢的那一路 ——
  // 而"要不要换掉这份素材"恰恰取决于最慢的那一路。
  const perSource = [];
  for (const item of seenSources.values()) {
    const video = item.video;
    const probes = 5;
    const times = [];
    for (let i = 0; i < probes; i += 1) {
      // 每次换一个位置（同一个秒数不会重新解码，量不出代价）。
      const seconds = (i + 0.5) / (probes + 1) * Math.min(Number(video.duration) || 1, 2);
      const t = performance.now();
      await engine.seekVideo(video, seconds);
      times.push(Number((performance.now() - t).toFixed(2)));
    }
    const sortedTimes = times.slice().sort((a, b) => a - b);
    // videoWidth 是**解码后的真实尺寸**：设置元素 width/height 之后它会跟着变小，
    // 所以这个数既是证据（上限真的生效了），也是"这一路到底在解多少像素"的答案。
    perSource.push({
      source: item.source,
      width: video.videoWidth,
      height: video.videoHeight,
      duration: Number(video.duration) || 0,
      decodeMs: sortedTimes[Math.floor(sortedTimes.length / 2)],
      decodeMaxMs: sortedTimes[sortedTimes.length - 1],
    });
  }

  const rows = [];
  for (let i = 0; i < samples; i += 1) {
    const frame = 1 + i * step;
    if (frame >= end - 1) break;
    // 三段分开计时：**先量 seek，再量剩下的**。
    // 合在一起计时就只能得出"一帧很慢"，而那不指向任何一处代码。
    const sources = engine.sourcesFor(frame);
    const t0 = performance.now();
    for (const entry of sources) {
      const video = engine.videos.get(entry.source);
      if (video === undefined) continue;
      await engine.seekVideo(video, entry.seconds);
    }
    const t1 = performance.now();
    // **走的就是产品那条路**（engine.seek = prepare + draw）：量一个自己另拼的
    // 近似路径没有意义 —— 那条路快不快不决定用户看到的快不快。
    // 上面那次 seek 已经把源都定位好了，所以这里的 prepare 是一次"已就位"的复跑，
    // 量出来的正是**除解码之外的那部分开销**。
    await engine.seek(frame);
    const t2 = performance.now();
    rows.push({
      frame: frame,
      sources: sources.length,
      // 素材解码：改不了它，但能换素材（这条数决定"要不要用代理"）。
      seekMs: Number((t1 - t0).toFixed(2)),
      // 链路开销：seek 之外的一切（宿主 prepare + 文字 + draw）。
      chainMs: Number((t2 - t1).toFixed(2)),
      totalMs: Number((t2 - t0).toFixed(2)),
    });
  }

  const sorted = rows.map((row) => row.seekMs).slice().sort((a, b) => a - b);
  const total = rows.map((row) => row.totalMs).slice().sort((a, b) => a - b);
  const mean = (list) => (list.length === 0 ? 0 : list.reduce((a, b) => a + b, 0) / list.length);
  const median = (list) => (list.length === 0 ? 0 : list[Math.floor(list.length / 2)]);

  return reportVerdict(name, {
    kind: "perf",
    ok: true,
    reason: "",
    // 报上限，否则"这次测出来的数"归因不到任何一处设置上。
    heavyPixels: PREVIEW_HEAVY_PIXELS,
    decodeSizes: state.decodeSizes,
    frameMs: Number(frameMs.toFixed(2)),
    samples: rows.length,
    sources: perSource,
    seek: {
      meanMs: Number(mean(sorted).toFixed(2)),
      medianMs: Number(median(sorted).toFixed(2)),
      minMs: sorted.length > 0 ? sorted[0] : 0,
      maxMs: sorted.length > 0 ? sorted[sorted.length - 1] : 0,
    },
    total: {
      meanMs: Number(mean(total).toFixed(2)),
      medianMs: Number(median(total).toFixed(2)),
      maxMs: total.length > 0 ? total[total.length - 1] : 0,
    },
    // 链路开销（除 seek 之外的一切）的均值。**这个数才是"精简链路"能改善的部分。**
    overheadMeanMs: Number(mean(rows.map((row) => row.chainMs)).toFixed(2)),
    // 能跑满帧率吗？预算就是序列帧率给的每帧毫秒数。
    budgetOk: median(total) <= frameMs,
    rows: rows,
  });
}

/**
 * 播放全程的**性能实测**（测量用，不是判定）：把整条时间线播一遍，报代价与丢帧。
 *
 * # 为什么与 playback 判定分开
 *
 * `playback` 验的是"帧号推进得对不对"，它只要 5 帧就够，**样本太少**，
 * 拿它谈性能是拿两个点画曲线。性能要的是**整条时间线上的分布**：
 * 中位、均值、最坏，以及"最坏那一帧是谁"。
 *
 * # 为什么必须走真实播放（而不是循环调 seek）
 *
 * 真实播放有节流（rAF 一拍一次）、有 carry 累积、有音频同步。
 * 用一个自己写的高频循环测出来的数**不代表用户看到的速度** ——
 * 那种测量只会得出"很快"，因为它在尽可能快地连着调。
 */
async function runPlaythroughVerdict(name) {
  const engine = state.engine;
  if (state.doc === null || state.doc === undefined) {
    return reportVerdict(name, { kind: "playthrough", ok: false, reason: "页面里还没有工程" });
  }
  const end = Math.max(1, engine.endFrame());
  if (end < 8) {
    return reportVerdict(name, { kind: "playthrough", ok: false, reason: "工程只有 " + end + " 帧，播不出分布" });
  }
  const frameMs = 1000 / sequenceFps();

  pause();
  // **只在测量时打开细粒度计时** —— 它每帧多十几次 performance.now()，
  // 平时不该付这个钱。
  engine.timing = true;
  engine.slowSeeks = [];

  // **同页对照**：新建一个 <video>（引擎没碰过它）跑同样的 seek 序列。
  // 如果它也是 45ms，"慢"就是这个浏览器/这台机器在此环境下的常态；
  // 如果它是 0.1ms，那"慢"是被引擎那条路带出来的 —— 两者指向完全不同的修法。
  // **页面本身的状态**：如果这个页面是后台/被遮挡的，Chrome 会限制它的媒体解码，
  // 而那种限制在测量数字上与"代码慢"完全一样。
  const pageState = {
    visibilityState: typeof document.visibilityState === "string" ? document.visibilityState : "?",
    hidden: document.hidden === true,
    hasFocus: document.hasFocus(),
    devicePixelRatio: window.devicePixelRatio,
  };
  // rAF 到底多久一拍（把"被限制"这件事量出来）
  const rafGaps = [];
  await new Promise((resolve) => {
    let last = performance.now();
    let n = 0;
    const tick = () => {
      const now = performance.now();
      rafGaps.push(now - last);
      last = now;
      n += 1;
      if (n >= 20) resolve();
      else requestAnimationFrame(tick);
    };
    requestAnimationFrame(tick);
  });
  const sortedGaps = rafGaps.slice().sort((a, b) => a - b);
  pageState.rafMedianMs = Number(sortedGaps[Math.floor(sortedGaps.length / 2)].toFixed(2));
  pageState.rafMeanMs = Number((rafGaps.reduce((a, b) => a + b, 0) / rafGaps.length).toFixed(2));

  // 给到"足够播完 + 宽裕"，慢机器也要能跑完。
  const budgetMs = end / sequenceFps() * 1000 * 8 + 8000;
  // **受控对照**：先播一轮**不画**（只 seek），再正常播一轮。
  // 两轮的帧号序列与 seek 位置完全相同，唯一的变量是"要不要上屏"。
  engine.skipDraw = true;
  await seekTo(0);
  await new Promise((r) => setTimeout(r, 150));
  play();
  await waitUntil(() => !isPlaying() || state.frame >= end - 1, budgetMs);
  pause();
  const noDraw = playbackStats();
  engine.skipDraw = false;
  resetPlaybackStats();
  await seekTo(0);
  const startedAt = performance.now();
  const finished = await waitUntil(() => !isPlaying() || state.frame >= end - 1, budgetMs);
  const wallMs = performance.now() - startedAt;
  pause();

  engine.timing = false;
  const stats = playbackStats();
  // **实时倍率**：播完这条时间线实际花了多久 vs 它本来该多久。
  // >1 是慢放（跟不上），约等于 1 是实的。这是用户唯一直接感受到的数。
  const idealMs = (end - 1) / sequenceFps() * 1000;
  const realtimeRatio = idealMs > 0 ? wallMs / idealMs : 0;

  return reportVerdict(name, {
    kind: "playthrough",
    ok: true,
    reason: "",
    frameMs: Number(frameMs.toFixed(2)),
    end: end,
    reachedFrame: state.frame,
    finished: finished,
    wallMs: Math.round(wallMs),
    idealMs: Math.round(idealMs),
    realtimeRatio: Number(realtimeRatio.toFixed(2)),
    stats: stats,
    // 对照：不画那一轮的代价（用来判定"慢"是解码本身的还是被上屏拖住的）。
    noDrawStats: noDraw,
    // 慢 seek 的现场记录（跳了多远、当时的 readyState）。
    slowSeeks: Array.isArray(engine.slowSeeks) ? engine.slowSeeks.slice() : [],
    // 页面自身状态：被限速的页面量出来的数不代表用户环境，这条要能看见。
    pageState: pageState,
  });
}

/**
 * 判定：**预览那一帧与出片那一帧一致吗。**
 *
 * 挑的是**需要多路素材**的那一帧：只走一路的帧两边都简单，一致了也说明不了什么。
 * 报的是**实测的逐通道差**，不是「看起来一样」。
 */
async function runRealFrameVerdict(name) {
  const end = state.engine.endFrame();
  const target = Math.min(12, Math.max(0, end - 1));
  await seekTo(target);
  // **不跟着按钮的开关状态走**：判定要的是"这一帧重新取一次再比"。
  // 页面预检可能已经点过这个按钮，于是这里会撞上"已经展示着"那条路径、
  // 什么都没比就回传 —— 判定必须是可重复的。
  hideRealFrame();
  const result = await showRealFrame();
  if (result === null || result.ok !== true) {
    await reportVerdict(name, { ok: false, reason: result === null ? "没有结果" : result.reason });
    return;
  }
  // **对照：再 seek 一次回来。**
  // 第一遍抄到的是「seek 刚结束」那一刻的画面。用户实际的用法是拖过去再拖回来，
  // 所以第二遍抄的才是他真正看到的。两遍不一样，就说明第一遍**还没稳定** ——
  // 那是「读数太早」，与「两次渲染不一致」要修的东西完全不同。
  let settled = null;
  if (result.compared === true) {
    await seekTo(Math.max(0, result.frame - 1));
    await seekTo(result.frame);
    const again = capturePreviewPixels($("preview"));
    if (!again.uniform) {
      const realPixels = captureRealFramePixels();
      const w = $("preview").width;
      const h = $("preview").height;
      const d2 = diffImageData(again.data, realPixels, w, h);
      settled = { max: d2.maxAbs, mean: d2.meanAbs, block: d2.blockMeanAbs };
    }
  }
  // **对照 2：像导出那样重新 open 一次再 seek。**
  // `exportPngSequence` 每出一帧都先 `engine.open(工程)` 再 `seek`；
  // 而界面上的预览是启动时 open 一次、之后只 seek。
  // 两边的数不一样，就说明**预览会漂**、而导出那条路因为每帧重开所以是对的 ——
  // 这也正好解释了 `check-dual-end` 的 SSIM 是 1.0（它比的是导出，不是预览）。
  let fresh = null;
  if (result.compared === true) {
    state.engine.open(JSON.stringify(state.doc));
    await state.engine.seek(result.frame);
    const reopened = capturePreviewPixels($("preview"));
    if (!reopened.uniform) {
      const realPixels = captureRealFramePixels();
      const w = $("preview").width;
      const h = $("preview").height;
      const d3 = diffImageData(reopened.data, realPixels, w, h);
      fresh = { max: d3.maxAbs, mean: d3.meanAbs, block: d3.blockMeanAbs };
    }
  }
  // **证据要在最后留。**
  // WebGPU 画布被 toDataURL 读一次之后，交换链里的内容就没了，后面再 drawImage 抄到的是空白。
  // 本轮真踩到两次：先留证据，判定就报"预览画布读回来是纯色"。
  // 帧号取 9999：它是**证据**不是产物，用一个不可能撞上的号。
  try {
    const dataUrl = $("preview").toDataURL("image/png");
    const comma = dataUrl.indexOf(",");
    if (comma < 0) throw new Error("toDataURL 没有数据段");
    // **发字节而不是 base64 文本**：驱动那一侧是 writeFileSync 原样落盘，
    // 发文本落下来的就是一个 .png 后缀的 base64 文件 —— 打开时"不是图片"，
    // 而长度看着还挺像样，最容易把人引到别处去。
    await fetch("/frame-png?frame=9999", {
      method: "POST",
      headers: { "content-type": "image/png" },
      body: base64ToBytes(dataUrl.slice(comma + 1)),
    });
  } catch (error) { log("留预览证据失败：" + String(error)); }
  await reportVerdict(name, {
    // kind 决定驱动那一侧怎么读这份判定（按形状分派，不按名字）。
    kind: "realframe",
    ok: true,
    compared: result.compared === true,
    // 没比成时要有一句能看懂的理由 —— 少了它，驱动那侧只会打出 undefined。
    reason: result.reason,
    frame: result.frame,
    max_abs_channel_diff: result.maxAbs,
    mean_abs_channel_diff: result.meanAbs,
    over_threshold_ratio: result.overRatio,
    same_pixel_ratio: result.samePixelRatio,
    channel_mean_diff: result.chanMean,
    block_mean_abs_diff: result.blockMeanAbs,
    render_size: result.width + "x" + result.height,
    preview_size: $("preview").width + "x" + $("preview").height,
    settled_max_abs_channel_diff: settled === null ? null : settled.max,
    settled_mean_abs_channel_diff: settled === null ? null : settled.mean,
    settled_block_mean_abs_diff: settled === null ? null : settled.block,
    fresh_max_abs_channel_diff: fresh === null ? null : fresh.max,
    fresh_mean_abs_channel_diff: fresh === null ? null : fresh.mean,
    fresh_block_mean_abs_diff: fresh === null ? null : fresh.block,
    note: "同一份工程、同一帧：预览走浏览器 wasm，出片帧走 dhampir frame",
  });
}
/**
 * 判定：**界面骨架还在不在。**
 *
 * `web/index.html` 是这个仓库里唯一没有守卫盯着的大件：改版很容易顺手删掉一个 id，
 * 而后果是某个按钮点了没反应 —— 那种坏法在程序化验收里完全看不见
 * （驱动只读 `window.__dhampirMarks` 与 `document.title`）。
 * 所以这里把**契约**摆出来逐个点一遍。
 */
const UI_CONTRACT_IDS = [
  "applyFps", "audios", "download", "export", "first", "fpsLabel", "frame", "frameLabel", "issues", "last", "library", "markers", "muteBtn", "next", "play", "playStats", "prev", "preview", "progress", "progressBar", "progressText", "props", "redoBtn", "removeBtn", "rippleBtn", "seqFps", "splitBtn", "timeline", "undoBtn", "videos", "volume", "frameOnly",
];

async function runUiVerdict(name) {
  const missing = UI_CONTRACT_IDS.filter((id) => $(id) === null);
  // 图标按钮：`setIcon` 是启动末尾才跑的，所以"按钮在"不等于"图标画出来了"。
  const iconHosts = ["first", "prev", "play", "next", "last", "muteBtn"];
  const noIcon = iconHosts.filter((id) => {
    const host = $(id);
    return host === null || host.querySelector("svg") === null;
  });
  const canvas = $("preview");
  await reportVerdict(name, {
    kind: "ui",
    ok: true,
    ready: window.dhampirReady === true,
    missing_ids: missing,
    icons_missing: noIcon,
    canvas: canvas === null ? "没有 canvas" : canvas.width + "x" + canvas.height,
    // 顶栏那行工程摘要也是本轮新加的：它要么是空的，要么说明白是哪份工程。
    project_label: $("projectId") === null ? "(没有 projectId)" : $("projectId").textContent,
  });
}
/** 判定按**名字**选路。表在这里，规则在各判定函数里。 */
const VERDICTS = {
  "trim-parity": runTrimParity,
  subtitle: runSubtitleVerdict,
  "undo-drag": runUndoDragVerdict,
  "trim-drag": runTrimDragVerdict,
  playback: runPlaybackVerdict,
  "audio-track": runAudioTrackVerdict,
  perf: runPerfVerdict,
  playthrough: runPlaythroughVerdict,
  realframe: runRealFrameVerdict,
  ui: runUiVerdict,
};

/** 状态栏：写一条最新的进展/结果。 */
function log(message) {
  state.hint = String(message);
  renderIssues();
}

// --- 工程模型上的小工具 ------------------------------------------------------------

function timeline() {
  return state.doc === null ? null : state.doc.timeline;
}

function selectedLayer() {
  if (state.selected === null || state.doc === null) return null;
  const tracks = state.doc.timeline.tracks;
  const track = tracks[state.selected.trackIndex];
  if (track === undefined) return null;
  return track.layers[state.selected.layerIndex] || null;
}

/** 这一层看起来是不是调整图层。
 *
 * **这只是给界面看的标签**：权威判定在 core 的 Layer::is_adjustment()，
 * 预检与渲染都听它的。两者不一致时以 Rust 为准 —— 这里不参与任何判定。 */
function looksLikeAdjustment(layer) {
  return !layer.source && Array.isArray(layer.effects) && layer.effects.length > 0;
}

function describeLayer(layer) {
  const parts = [];
  parts.push(layer.source ? ("素材 " + layer.source.asset_id) : "无素材");
  if (looksLikeAdjustment(layer)) parts.push("调整图层");
  if (layer.blend && layer.blend !== "normal") parts.push("混合 " + layer.blend);
  const radius = blurRadius(layer);
  if (radius > 0) parts.push("模糊 " + radius);
  return parts.join(" / ");
}

function blurRadius(layer) {
  if (!Array.isArray(layer.effects)) return 0;
  const effect = layer.effects.find((item) => item.kind === "gaussian_blur");
  return effect === undefined ? 0 : Number(effect.params.radius) || 0;
}

function enabledOf(layer) {
  return layer.enabled === undefined ? true : layer.enabled !== false;
}

// --- 素材绑定 ---------------------------------------------------------------------

/** 工程里被引用到的 asset id（去重，保持首次出现的顺序）。 */
function assetIdsInUse() {
  const ids = [];
  const seen = new Set();
  const model = timeline();
  if (model === null) return ids;
  for (const track of model.tracks) {
    for (const layer of track.layers) {
      const id = layer.source && layer.source.asset_id;
      if (typeof id === "string" && id.length > 0 && !seen.has(id)) {
        seen.add(id);
        ids.push(id);
      }
    }
  }
  return ids;
}

function declaredAsset(assetId) {
  const assets = state.doc && Array.isArray(state.doc.assets) ? state.doc.assets : [];
  return assets.find((asset) => asset.id === assetId) || null;
}

/**
 * 预览里"解不动"的素材阈值（像素）。
 *
 * # 这是一个**报告用**的阈值，不是一个能生效的开关
 *
 * 实测（真实 Chrome，同一份 4K 素材四次；那次对照的探针页已随结论一起删掉，
 * 数据留在这一节与 docs/usage.md 6.5）：
 *
 * | 尝试的机制 | videoWidth | 单次 seek |
 * |---|---|---|
 * | 什么都不设 | 3840x2160 | 115.9ms |
 * | width/height 属性 | 3840x2160 | 132.6ms |
 * | CSS 尺寸 | 3840x2160 | 118.8ms |
 * | 属性 + CSS | 3840x2160 | 143.4ms |
 *
 * **没有一种能让 Chrome 少解几个像素。** width/height 属性只是改变元素的
 * 布局尺寸（attrWidth 确实变成了 1920），`videoWidth` 一路都是 3840 ——
 * 解码分辨率不跟着走。所以"设个属性就把 4K 降下来"是**错的**，
 * 这条路在本仓库已经被证伪，不要有人再试一次。
 *
 * 真要降只有两条路，都不在这一层：
 *   * **用低分辨率代理素材**（架构上正确，出片仍用原片）—— 要后端/工程侧配合；
 *   * 换一条不走 <video> 的解码通路（WebCodecs）—— 是另一个量级的改动。
 *
 * # 那这个常量留着干什么
 *
 * 用来**在状态栏说出实话**：这份工程里有素材超出预览能逐帧跟上的量级时，
 * 明确告诉用户"卡是素材的解码量，不是这个页面"。用户能改的东西只有素材，
 * 所以这条提示必须指向素材，而不是含糊地说一句"性能不佳"。
 */
const PREVIEW_HEAVY_PIXELS = 1920 * 1080;

/**
 * 每个被引用的 asset 一个 video 元素。
 *
 * **一个 source 一个元素**：同一帧上不同元素可能是同一素材的不同源内帧，
 * 共用一个 video 是做不到的（这一点在 M3 的 spike 里定过）。
 *
 * 某一路加载失败**不中止启动**：报出来、跳过它，其余照常用。
 * 一路素材坏了就整个界面打不开，那是把"部分可用"降级成"完全不可用"。
 */
async function bindAllSources() {
  const host = $("videos");
  host.textContent = "";
  const ids = assetIdsInUse();
  let index = 0;
  const loaded = [];
  for (const assetId of ids) {
    const asset = declaredAsset(assetId);
    if (asset === null) {
      notice("工程引用了素材 " + assetId + "，但资产表里没有登记它 —— 这一路不会被画出来。");
      continue;
    }
    const id = "src" + index;
    index += 1;
    const video = document.createElement("video");
    video.id = id;
    video.muted = true;
    video.playsInline = true;
    video.preload = "auto";
    // **必须声明 crossorigin。**
    // 本机/分离模式下素材是跨源的，而不带 crossorigin 的 video 是"被污染"的：
    // WebGPU 的 copyExternalImageToTexture 会拒绝它 —— 表现不是报错，
    // 而是 wasm 里一个 unreachable（wgpu 的校验失败变成了 panic），
    // 外面只看到"启动失败：unreachable"。这条路本地跑了很久才定位到这里。
    video.crossOrigin = "anonymous";
    video.src = await backend.mediaUrlFor(assetId);
    host.appendChild(video);
    try {
      await new Promise((resolve, reject) => {
        video.addEventListener("loadeddata", resolve, { once: true });
        video.addEventListener("error", () => reject(new Error("加载失败")), { once: true });
      });
      video.pause();
      // **把"浏览器到底解了多大"记下来。** 这是唯一能知道"这一路在解多少像素"的地方，
      // 而它正是逐帧 seek 快不快的决定因素（见 PREVIEW_HEAVY_PIXELS 那段实测）。
      state.decodeSizes.push({
        asset_id: assetId,
        videoWidth: video.videoWidth,
        videoHeight: video.videoHeight,
      });
      state.engine.bindSource(assetId, id);
      loaded.push(assetId);
    } catch (error) {
      notice("素材 " + assetId + " 取不到（" + video.src + "）—— 这一路不会被画出来。");
    }
  }
  // **解不动的素材要说出来。** 用户看到画面卡，唯一能自己动手改的就是素材；
  // 含糊地说一句"性能不佳"等于什么都没说 —— 要说清楚是**哪一路、多少像素**。
  const heavy = state.decodeSizes.filter((item) =>
    item.videoWidth * item.videoHeight > PREVIEW_HEAVY_PIXELS);
  if (heavy.length > 0) {
    const parts = heavy.map((item) => item.asset_id + "（" + item.videoWidth + "x" + item.videoHeight
      + "，" + (item.videoWidth * item.videoHeight / 1e6).toFixed(1) + "MP）");
    notice("这些素材超出预览能逐帧跟上的量级：" + parts.join("、")
      + "。预览逐帧 seek 的代价与解码像素数成正比，实测 4K 单帧约 150ms、1080p 约 45ms，"
      + "而 30fps 的预算是 33ms —— 卡的是解码量，不是这个页面。"
      + "要跑满帧率得换低分辨率的预览代理（出片仍用原素材）。");
  }
  return loaded;
}

// --- 字幕 -------------------------------------------------------------------------

/** 资产表里登记的字幕素材。**不看谁引用它** —— 引用关系归 Rust 判（library）。 */
function subtitleAssets() {
  const assets = state.doc && Array.isArray(state.doc.assets) ? state.doc.assets : [];
  return assets.filter((asset) => asset.kind === "subtitle");
}

/**
 * 从 uri 的后缀读格式。**不嗅探内容**：
 * CLI 认的就是文件名后缀，嗅探会多出第二套判定，而两套判定迟早不同。
 */
function subtitleFormat(asset) {
  const uri = typeof asset.uri === "string" ? asset.uri : "";
  const dot = uri.lastIndexOf(".");
  return dot < 0 ? "" : uri.slice(dot + 1).toLowerCase();
}

/**
 * 取回并登记这份工程的字幕素材。
 *
 * 位置由 backend 回答（mediaUrlFor），解析在 Rust（parse_srt / parse_ass）——
 * 这里只做一次 fetch 并把两端接上。**失败要看得见**：登记不上的那一路，
 * 宿主随后会在每一帧报 `subtitle_unregistered`，但那时已经看不出原因了。
 */
async function loadProjectSubtitles() {
  const assets = subtitleAssets();
  if (assets.length === 0) return { registered: [], failed: [] };
  const entries = [];
  for (const asset of assets) {
    entries.push({
      assetId: asset.id,
      url: await backend.mediaUrlFor(asset.id),
      format: subtitleFormat(asset),
    });
  }
  const result = await state.engine.loadSubtitles(entries);
  for (const item of result.registered) {
    const skipped = item.skipped > 0 ? "（跳过 " + item.skipped + " 条）" : "";
    log("字幕 " + item.asset + "：读到 " + item.cues + " 条" + skipped);
  }
  for (const failure of result.failed) {
    notice("字幕 " + failure.assetId + " 没登记上（" + failure.error + "）—— 这一路不会被画出来。");
  }
  return result;
}

// --- 素材库 -----------------------------------------------------------------------

/**
 * 取素材库。**引用次数问后端**（它转调 Rust 的 library），不在前端重数 ——
 * 「这个素材有没有被引用」只有一份实现，而两份实现一定会漂。
 */
async function loadLibrary() {
  if (typeof backend.baseUrl !== "string" || backend.baseUrl.length === 0) return;
  try {
    const response = await fetch(backend.baseUrl + "/projects/" + encodeURIComponent(backend.projectId) + "/library");
    if (!response.ok) throw new Error("HTTP " + response.status);
    state.library = await response.json();
  } catch (error) {
    state.library = null;
    notice("取素材库失败：" + String(error && error.message ? error.message : error));
  }
}

function renderLibrary() {
  const host = $("library");
  host.textContent = "";
  if (state.library === null) {
    // **"读不到"要说出下一步做什么。** 只说"未连接后端"的话，用户不知道
    // 是网络问题、是没启动、还是本来就不该有 —— 所以这里把两条出路都写出来。
    setEmpty("library", "素材库读不到", {
      error: true,
      why: "没连上后端。加素材用 dhampir import，或起后端后刷新（POST /assets）。",
    });
    return;
  }
  const unused = new Set(Array.isArray(state.library.unused) ? state.library.unused : []);
  for (const asset of state.library.assets) {
    const row = document.createElement("div");
    row.className = "row";
    const label = document.createElement("span");
    if (unused.has(asset.id)) label.className = "unused";
    label.textContent = asset.id + " · " + asset.kind + " · " + asset.references + " 次";
    label.title = asset.uri;
    const insert = document.createElement("button");
    insert.textContent = "插入";
    insert.addEventListener("click", () => insertFromLibrary(asset.id));
    row.appendChild(label);
    row.appendChild(insert);
    host.appendChild(row);
  }
  // 与上面那句**必须不同**：那里是"读不到"，这里是"读到了、真的是空的"。
  if (state.library.assets.length === 0) {
    setEmpty("library", "工程里还没有素材", { why: "用 dhampir import 登记一个文件，它就会出现在这里。" });
  }
}

/** 往选中的元素所在轨道（没有就第一条视频轨）的当前帧放一个引用。 */
function targetTrackId() {
  const selected = state.selected;
  if (selected !== null && state.doc !== null) {
    const track = state.doc.timeline.tracks[selected.trackIndex];
    if (track !== undefined) return track.id;
  }
  const first = state.doc.timeline.tracks.find((track) => track.kind === "video");
  return first === undefined ? null : first.id;
}

function insertFromLibrary(assetId) {
  const track = targetTrackId();
  if (track === null) {
    notice("没有可用的视频轨道");
    return;
  }
  runEdit({ op: "insert", track: track, asset: assetId, at: state.frame, source_in: 0, length: 60 },
    "插入 " + assetId);
}

// --- 编辑操作 ---------------------------------------------------------------------
//
// **规则一个都不在这里。** 剃刀怎么切、修剪推多少源帧、序列改帧率要重算哪些数，
// 全在 Rust 的 dhampir-timeline::edit 里 —— CLI 与这里调的是同一个函数。
// 这一层只负责把点击翻成一次调用，然后把结果画出来。

async function runEdit(op, label) {
  const result = state.engine.edit(op);
  if (result.ok !== true) {
    // 宿主里那份**一个字都没变**，所以这里也不动本地副本。
    state.issues = result.issues || [];
    renderIssues();
    log((label || "编辑") + " 没生效：" + state.issues.map((issue) => issue.code).join(", "));
    return;
  }
  state.doc = state.engine.doc();
  await refreshAfterEdit();
  log((label || "编辑") + "：" + result.summary);
}

/**
 * 撤销 / 重做一步。
 *
 * **栈在 Rust**（timeline::history），这里只把新工程取回来画一遍 ——
 * 与 CLI 的 `edit --undo` 调的是同一份规则。退不动时 `ok:false` + 一条
 * `nothing_to_undo` / `nothing_to_redo`，宿主里那份**一个字节都没动**。
 */
async function runHistoryStep(which) {
  const verb = which === "undo" ? "撤销" : "重做";
  const result = which === "undo" ? state.engine.undo() : state.engine.redo();
  if (result.ok !== true) {
    state.issues = result.issues || [];
    renderIssues();
    log(verb + " 没生效：" + state.issues.map((issue) => issue.code).join(", "));
    // **退不动要说出来。** 静默什么都不做的话，用户会以为按钮坏了；
    // 而且"没有可撤销的步骤"与"撤销失败了"必须听起来不一样。
    toast(verb + " 没生效：" + (state.issues.map((issue) => issue.code).join(", ") || "没有可撤销的步骤"), "warn");
    return;
  }
  state.doc = state.engine.doc();
  await refreshAfterEdit();
  log(result.summary);
  // 成功也回一声：撤销是"看不见结果"的操作之一（画面可能恰好一样），
  // 没有确认时用户会连按好几次。
  toast(verb + "：" + result.summary, "ok");
}

/** 一次成功改动之后把三个面板与当前帧重新画一遍。
 *
 * 撤销/重做与普通编辑**走的是同一条**——分成两份的话，
 * 「撤销之后画面不刷新」这类漂只有肉眼能发现。 */
async function refreshAfterEdit() {
  state.issues = [];
  state.warnings = [];
  renderTimeline();
  renderInspector();
  renderIssues();
  await seekTo(state.frame);
  await loadLibrary();
  renderLibrary();
}

// --- 时间线视图 -------------------------------------------------------------------

/**
 * 时间线的行序：**屏幕上最下面一行 = tracks[0]**。
 *
 * 这不是审美选择，是 Rust 的语义：`compose.rs` 的「多轨从下往上」把
 * `tracks[0]` 摆在最底层，后画的盖在前面。界面若正序从上往下铺，
 * 用户看到的叠放关系就是**反的** —— 而且反得"看起来很正常"，不会报错。
 *
 * # 为什么要有这两个助手
 *
 * 行序反了之后，"第几行"与"第几条轨道"就不再是同一个数。以前它们恰好相等，
 * 于是各处直接写 `timelineRows()[trackIndex]`。现在若在每一个用到的地方
 * 各自倒算一次，那就是**同一份映射的多份实现** —— 迟早有一处漏掉，
 * 而漏掉的表现是"拖错了行"或"验收探针挑错行"，两种都不红。
 *
 * 所以映射只在这里推导：**画的时候用 trackIndexToRow，读的时候用 rowToTrackIndex**。
 */
function rowIndexForTrack(trackIndex, trackCount) {
  return trackCount - 1 - trackIndex;
}

function trackIndexForRow(rowIndex, trackCount) {
  return trackCount - 1 - rowIndex;
}

/** 当前 DOM 里的行（已按屏幕顺序），以及配套的轨道条数。 */
function timelineRows() {
  const rows = Array.from($("timeline").querySelectorAll(".track"));
  return { rows: rows, trackCount: rows.length };
}

function renderTimeline() {
  const host = $("timeline");
  host.textContent = "";
  const model = timeline();
  if (model === null) {
    host.textContent = "（还没有载入工程）";
    return;
  }
  const end = Math.max(1, state.engine.endFrame());
  const trackCount = model.tracks.length;
  // 倒序铺：屏幕上最后一行是 tracks[0]（最底层）。
  const screenOrder = model.tracks
    .map((track, trackIndex) => ({ track: track, trackIndex: trackIndex }))
    .sort((a, b) => rowIndexForTrack(a.trackIndex, trackCount) - rowIndexForTrack(b.trackIndex, trackCount));
  screenOrder.forEach((entry, rowIndex) => {
    const track = entry.track;
    const trackIndex = entry.trackIndex;
    const row = document.createElement("div");
    row.className = "track";
    // 行序 → 轨道序写进 dataset：**这是界面自己声明的事实**，
    // 验收驱动与调试都从这里读，不必各自倒算一遍。
    row.dataset.trackIndex = String(trackIndex);
    row.dataset.rowIndex = String(rowIndex);
    const label = document.createElement("span");
    label.className = "track-label";
    // 层级提示：用户一眼能看出谁盖着谁（第 1 行是最上层）。
    const layerRank = rowIndex === 0 ? "最上层" : (rowIndex === trackCount - 1 ? "最底层" : "第 " + (rowIndex + 1) + " 层");
    label.textContent = track.id + " (" + track.kind + " · " + layerRank + ")";
    row.appendChild(label);
    track.layers.forEach((layer, layerIndex) => {
      const bar = document.createElement("div");
      let className = "layer";
      if (looksLikeAdjustment(layer)) className += " adjustment";
      if (!enabledOf(layer)) className += " disabled";
      if (state.selected !== null && state.selected.trackIndex === trackIndex
          && state.selected.layerIndex === layerIndex) {
        className += " selected";
      }
      bar.className = className;
      bar.style.left = (layer.start / end * 100) + "%";
      bar.style.width = Math.max(1.5, (layer.end - layer.start) / end * 100) + "%";
      bar.textContent = layer.id;
      bar.title = describeLayer(layer);
      if (layer.transition_in) {
        const overlay = document.createElement("div");
        overlay.className = "tr";
        overlay.style.left = "0";
        overlay.style.width = Math.min(100, layer.transition_in.duration / (layer.end - layer.start) * 100) + "%";
        bar.appendChild(overlay);
      }
      bar.addEventListener("click", () => {
        state.selected = { trackIndex: trackIndex, layerIndex: layerIndex };
        renderTimeline();
        renderInspector();
      });
      attachLayerDrag(bar, row, layer, end);
      // 左右缘：改**时长**（不是位置）。手柄在 bar 之后 append，于是盖在过渡标记之上。
      attachEdgeTrim(bar, row, layer, end, "in");
      attachEdgeTrim(bar, row, layer, end, "out");
      row.appendChild(bar);
    });
    host.appendChild(row);
  });
  renderMarkers(model, end);
}

/**
 * 让一层可以被**拖到别的时间位置**。
 *
 * 一次拖拽只生成**已有的** `{op:"move", layer, to}`，交给 `runEdit` ——
 * 拖拽没有自己的编辑语义，「能不能挪到那儿」永远由 Rust 的 `move` 给结论。
 * 拖动过程中只改这一条 bar 的 `left`（纯视觉预览，`state.doc` 一个字都不动），
 * 落点在 **pointerup** 时才向 Rust 提**一次**：
 * 于是「拖一下」在历史里就是**一步**，撤销正好回到拖动之前。
 */
function attachLayerDrag(bar, row, layer, end) {
  bar.addEventListener("pointerdown", (event) => {
    if (event.button !== 0) return;
    // 手柄上的按下归**改时长**那条路（它自己 stopPropagation；这里是第二道保险，
    // 因为合成事件不一定会冒泡到该到的地方）。
    if (event.target !== null && event.target.classList
        && event.target.classList.contains("handle")) return;
    // 时间线宽度 ↔ 帧数的换算只用**轨道**的宽度：bar 自己有最小宽度（1.5%），
    // 拿它去换算会在短元素上算歪。
    const trackWidth = row.clientWidth;
    if (!(trackWidth > 0) || !(end > 0)) return;
    const framesPerPixel = end / trackWidth;
    const snapFrames = Math.max(0, Math.round(6 * framesPerPixel));
    const startX = event.clientX;
    const originalStart = layer.start;
    const originalLeft = bar.style.left;
    let pending = originalStart;
    const boundaries = [];
    for (const track of (timeline() || { tracks: [] }).tracks) {
      for (const other of track.layers) {
        if (other === layer) continue;
        boundaries.push(other.start, other.end);
      }
    }
    // 指针捕获：不捕获的话，指针一旦离开这条 bar，pointermove / pointerup 就收不到了。
    // **合成事件（验收驱动造的）没有真实指针会抛 NotFoundError**，所以这里要兜住 ——
    // 验收通道不该因为缺少真实指针就把页面打崩。
    try { bar.setPointerCapture(event.pointerId); } catch (error) { /* 合成事件 */ }
    const onMove = (moveEvent) => {
      const delta = Math.round((moveEvent.clientX - startX) * framesPerPixel);
      pending = Math.max(0, originalStart + delta);
      bar.style.left = (pending / end * 100) + "%";
    };
    const finish = () => {
      bar.removeEventListener("pointermove", onMove);
      bar.removeEventListener("pointerup", finish);
      bar.removeEventListener("pointercancel", finish);
      // 先还原成模型里的位置：编辑万一被拒，宿主与界面就还是一致的（不能只在成功时才对）。
      bar.style.left = originalLeft;
      const to = snapFrame(pending, boundaries, snapFrames);
      if (to === originalStart) return;
      runEdit({ op: "move", layer: layer.id, to: to }, "移动").catch((error) => log(String(error)));
    };
    bar.addEventListener("pointermove", onMove);
    bar.addEventListener("pointerup", finish);
    bar.addEventListener("pointercancel", finish);
  });
}

/**
 * 让一层的**左右缘**可以拖动改时长（修剪）。
 *
 * 与 `attachLayerDrag`（整体平移）是两条路，但口径完全一样：
 *   * 拖动过程只改这一条 bar 的 `left` / `width`（纯视觉预览，`state.doc` 一个字不动）；
 *   * 落点在 **pointerup** 时才向 Rust 提**一次** `{op:"trim", layer, edge, to}`；
 *   * 能不能修剪、修剪推多少源帧，**全由 Rust 的 `trim` 判**（`dhampir-timeline::edit`）
 *     —— 这里没有一条自己的规则，连"最短能到几帧"都不复制。
 *
 * # 为什么用 trim 而不是直接改 start/end
 *
 * 直接写数字会把"改入点同时要推 source_in"这件事漏掉 —— 那是 `trim` 的核心语义
 * （入点左移 = 往素材前面多要内容），在前端重写一遍就是第二份实现。
 *
 * @param edge `"in"` 拖左缘（改 start + source_in）、`"out"` 拖右缘（只改 end）。
 */
function attachEdgeTrim(bar, row, layer, end, edge) {
  const handle = document.createElement("div");
  handle.className = "handle " + (edge === "in" ? "l" : "r");
  handle.title = edge === "in" ? "拖我改入点（同时推 source_in）" : "拖我改出点";
  handle.addEventListener("pointerdown", (event) => {
    if (event.button !== 0) return;
    // **别让手柄的按下冒泡到 bar** —— 冒上去就变成"选中/整体平移"，
    // 于是拖右缘会把整条挪走，而这正是改时长最容易出的那种错。
    event.stopPropagation();
    event.preventDefault();
    const trackWidth = row.clientWidth;
    if (!(trackWidth > 0) || !(end > 0)) return;
    const framesPerPixel = end / trackWidth;
    const snapFrames = Math.max(0, Math.round(6 * framesPerPixel));
    const startX = event.clientX;
    const originalStart = layer.start;
    const originalEnd = layer.end;
    const originalLength = originalEnd - originalStart;
    const originalLeft = bar.style.left;
    const originalWidth = bar.style.width;
    let pending = edge === "in" ? originalStart : originalEnd;
    const boundaries = [];
    for (const track of (timeline() || { tracks: [] }).tracks) {
      for (const other of track.layers) {
        if (other === layer) continue;
        boundaries.push(other.start, other.end);
      }
    }
    try { handle.setPointerCapture(event.pointerId); } catch (error) { /* 合成事件没有真实指针 */ }
    const onMove = (moveEvent) => {
      const delta = Math.round((moveEvent.clientX - startX) * framesPerPixel);
      pending = edge === "in" ? originalStart + delta : originalEnd + delta;
      // 预览：入点动 left 与 width，出点只动 width。
      if (edge === "in") {
        const nextStart = Math.max(0, Math.min(pending, originalEnd - 1));
        bar.style.left = (nextStart / end * 100) + "%";
        bar.style.width = ((originalEnd - nextStart) / end * 100) + "%";
      } else {
        const nextEnd = Math.max(originalStart + 1, pending);
        bar.style.width = ((nextEnd - originalStart) / end * 100) + "%";
      }
    };
    const finish = () => {
      handle.removeEventListener("pointermove", onMove);
      handle.removeEventListener("pointerup", finish);
      handle.removeEventListener("pointercancel", finish);
      // 先还原：编辑万一被拒，界面与宿主必须仍然一致（不能只在成功时才对）。
      bar.style.left = originalLeft;
      bar.style.width = originalWidth;
      const to = snapFrame(pending, boundaries, snapFrames);
      if (to === (edge === "in" ? originalStart : originalEnd)) return;
      // 长度没变就什么也别提：拖了一下又回到原地，不该在历史里多出一步。
      const nextLength = edge === "in" ? (originalEnd - to) : (to - originalStart);
      if (nextLength === originalLength) return;
      runEdit({ op: "trim", layer: layer.id, edge: edge, to: to },
        edge === "in" ? "修剪入点" : "修剪出点").catch((error) => log(String(error)));
    };
    handle.addEventListener("pointermove", onMove);
    handle.addEventListener("pointerup", finish);
    handle.addEventListener("pointercancel", finish);
  });
  bar.appendChild(handle);
}

/** 落点的**吸附**：候选帧附近有别的元素边界（阈值内）就吸上去。
 *
 * 这不算业务规则 —— 它不改「能不能移动」，只是把落点对齐；动手的还是 Rust 的 move。 */
function snapFrame(candidate, boundaries, threshold) {
  if (!(threshold > 0)) return candidate;
  let best = candidate;
  let bestDistance = threshold + 1;
  for (const boundary of boundaries) {
    const distance = Math.abs(boundary - candidate);
    if (distance <= threshold && distance < bestDistance) {
      best = boundary;
      bestDistance = distance;
    }
  }
  return best;
}

/** 工程级标记：画在轨道上方的一条细线上。 */
function renderMarkers(model, end) {
  const host = $("markers");
  host.textContent = "";
  const markers = Array.isArray(model.markers) ? model.markers : [];
  for (const marker of markers) {
    const tick = document.createElement("div");
    tick.className = "marker";
    tick.style.left = (marker.frame / end * 100) + "%";
    tick.title = "标记 " + marker.id + " @ " + marker.frame + (marker.name ? "（" + marker.name + "）" : "");
    host.appendChild(tick);
  }
}

// --- 属性面板 ---------------------------------------------------------------------

function numberField(labelText, value, onChange) {
  const wrap = document.createElement("label");
  const span = document.createElement("span");
  span.textContent = labelText;
  const input = document.createElement("input");
  input.type = "number";
  input.value = String(value);
  input.addEventListener("change", () => onChange(Number(input.value)));
  wrap.appendChild(span);
  wrap.appendChild(input);
  return wrap;
}

function selectField(labelText, value, options, onChange) {
  const wrap = document.createElement("label");
  const span = document.createElement("span");
  span.textContent = labelText;
  const select = document.createElement("select");
  for (const option of options) {
    const item = document.createElement("option");
    item.value = option;
    item.textContent = option;
    if (option === value) item.selected = true;
    select.appendChild(item);
  }
  select.addEventListener("change", () => onChange(select.value));
  wrap.appendChild(span);
  wrap.appendChild(select);
  return wrap;
}

function renderInspector() {
  const host = $("props");
  host.textContent = "";
  const layer = selectedLayer();
  if (layer === null) {
    // 空态要说**怎么让它不空**：时间线上点一个片段就行。
    setEmpty("props", "还没有选中元素", { why: "在时间线上点一个片段，这里就会出现它的属性。" });
    return;
  }

  // 每次改动都过一遍 Rust：问题立刻显示，而且**立刻反映到预览上**。
  // engine.open 在工程非法时会保留上一份可用工程，所以这里不会把预览打没。
  const apply = (mutate) => {
    mutate(layer);
    const result = state.engine.open(JSON.stringify(state.doc));
    state.issues = result.issues || [];
    state.warnings = result.warnings || [];
    renderTimeline();
    renderIssues();
    seekTo(state.frame).catch((error) => notice("刷新预览失败：" + error.message));
  };

  const identity = document.createElement("div");
  identity.className = "identity";
  identity.textContent = layer.id + "  ｜  " + describeLayer(layer);
  host.appendChild(identity);

  host.appendChild(numberField("起始帧 start", layer.start, (v) => apply((l) => { l.start = v; })));
  host.appendChild(numberField("结束帧 end（不含）", layer.end, (v) => apply((l) => { l.end = v; })));
  if (layer.source) {
    host.appendChild(numberField("源内起始 source_in", layer.source.source_in,
      (v) => apply((l) => { l.source.source_in = v; })));
  }
  host.appendChild(numberField("不透明度", layer.opacity, (v) => apply((l) => { l.opacity = v; })));
  host.appendChild(numberField("平移 x", layer.transform.x, (v) => apply((l) => { l.transform.x = v; })));
  host.appendChild(numberField("平移 y", layer.transform.y, (v) => apply((l) => { l.transform.y = v; })));
  host.appendChild(numberField("缩放 scale", layer.transform.scale, (v) => apply((l) => { l.transform.scale = v; })));
  host.appendChild(numberField("旋转 度", layer.transform.rotation, (v) => apply((l) => { l.transform.rotation = v; })));

  host.appendChild(selectField("混合模式", layer.blend || "normal", IMPLEMENTED_BLENDS,
    (v) => apply((l) => { l.blend = v; })));

  const blurRow = document.createElement("label");
  const blurSpan = document.createElement("span");
  blurSpan.textContent = "模糊半径";
  const blurInput = document.createElement("input");
  blurInput.type = "number";
  blurInput.value = String(blurRadius(layer));
  blurInput.addEventListener("change", () => apply((l) => {
    // 半径 0 就**移除**这个特效，而不是留个空壳：空壳会进图层清单，
    // 让"这一帧有几层"不可信，也会让"无素材 + 空特效"变成调整图层。
    const kept = (Array.isArray(l.effects) ? l.effects : []).filter((effect) => effect.kind !== "gaussian_blur");
    const radius = Number(blurInput.value);
    l.effects = kept;
    if (radius > 0) l.effects.push({ kind: "gaussian_blur", params: { radius: radius } });
  }));
  blurRow.appendChild(blurSpan);
  blurRow.appendChild(blurInput);
  host.appendChild(blurRow);

  const enabledRow = document.createElement("label");
  const enabledSpan = document.createElement("span");
  enabledSpan.textContent = "启用";
  const enabledInput = document.createElement("input");
  enabledInput.type = "checkbox";
  enabledInput.checked = enabledOf(layer);
  enabledInput.addEventListener("change", () => apply((l) => { l.enabled = enabledInput.checked; }));
  enabledRow.appendChild(enabledSpan);
  enabledRow.appendChild(enabledInput);
  host.appendChild(enabledRow);

  const keyRow = document.createElement("label");
  const keySpan = document.createElement("span");
  keySpan.textContent = "不透明度关键帧";
  const keyButton = document.createElement("button");
  const hasKeys = Array.isArray(layer.keyframes) && layer.keyframes.length > 0;
  keyButton.textContent = hasKeys ? "清掉" : "加一对";
  keyButton.addEventListener("click", () => apply((l) => {
    l.keyframes = Array.isArray(l.keyframes) && l.keyframes.length > 0
      ? []
      : [
        { frame: 0, value: 0, easing: "linear" },
        { frame: Math.max(1, (l.end - l.start) - 1), value: 1, easing: "ease_in_out" },
      ];
  }));
  keyRow.appendChild(keySpan);
  keyRow.appendChild(keyButton);
  host.appendChild(keyRow);

  const markerRow = document.createElement("label");
  const markerSpan = document.createElement("span");
  markerSpan.textContent = "标记（落在这一层起始处）";
  const markerButton = document.createElement("button");
  markerButton.textContent = "加一个";
  markerButton.addEventListener("click", () => apply((l) => {
    if (!Array.isArray(l.markers)) l.markers = [];
    l.markers.push({
      id: l.id + "-m" + (l.markers.length + 1),
      frame: Math.max(0, state.frame - l.start),
      name: "标记 " + (l.markers.length + 1),
    });
  }));
  markerRow.appendChild(markerSpan);
  markerRow.appendChild(markerButton);
  host.appendChild(markerRow);
}

// --- 问题清单 ---------------------------------------------------------------------

function line(className, text) {
  const div = document.createElement("div");
  div.className = className;
  div.textContent = text;
  return div;
}

function renderIssues() {
  const host = $("issues");
  host.textContent = "";
  if (state.hint.length > 0) host.appendChild(line("hint", state.hint));
  for (const text of state.notices) host.appendChild(line("bad", text));
  for (const issue of state.warnings) {
    host.appendChild(line("warn", "[" + issue.code + "] " + issue.path + " — " + issue.message));
  }
  if (state.issues.length > 0) {
    for (const issue of state.issues) {
      host.appendChild(line("bad", "[" + issue.code + "] " + issue.path + " — " + issue.message));
    }
    return;
  }
  if (state.notices.length === 0 && state.hint.length === 0) {
    host.appendChild(line("ok", "✓ 工程通过校验"));
  }
}

// --- 音频 -------------------------------------------------------------------------
//
// **音频不参与合成**（`compose.rs` 的既定语义：音轨只影响时间线长度，不产生画面），
// 所以这一段的职责只有一件：**播放时让音轨出声**，而且与画面**同一个时钟**。
//
// 「同一个时钟」是这里唯一难的地方。做法是让音频**跟着帧号走**：
// 每一拍算出播放头所在的帧，再换算成秒去校准 <audio> 的 currentTime ——
// 而不是"按播放键时同时按下音频播放键"再指望两边不漂。
// 后者在头几秒看不出问题，长片子上一定会漂，而"音画不同步"是最难查的一类。
//
// 用户明确说了音频不要特效，所以这里**没有**淡入淡出、变速、均衡 —— 只有播放/定位/音量。

/** asset id → <audio> 元素。与视频源一样，一个 asset 一个元素。 */
const audioSources = new Map();

/** 工程里被音轨引用到的 asset id（去重）。 */
function audioAssetIdsInUse() {
  const ids = [];
  const seen = new Set();
  const model = timeline();
  if (model === null) return ids;
  for (const track of model.tracks) {
    if (track.kind !== "audio") continue;
    for (const layer of track.layers) {
      const id = layer.source && layer.source.asset_id;
      if (typeof id === "string" && id.length > 0 && !seen.has(id)) {
        seen.add(id);
        ids.push(id);
      }
    }
  }
  return ids;
}

/**
 * 给每个音轨素材挂一个 <audio>。
 *
 * 与视频源同一条规矩：**某一路加载失败不中止启动** —— 报出来、跳过它，其余照常用。
 * 一路音频坏了就让整个界面打不开，那是把"部分可用"降级成"完全不可用"。
 */
async function bindAudioSources() {
  const host = $("audios");
  host.textContent = "";
  audioSources.clear();
  const ids = audioAssetIdsInUse();
  let index = 0;
  const loaded = [];
  for (const assetId of ids) {
    if (declaredAsset(assetId) === null) {
      notice("音轨引用了素材 " + assetId + "，但资产表里没有登记它 —— 这一路不会出声。");
      continue;
    }
    const audio = document.createElement("audio");
    audio.id = "aud" + index;
    index += 1;
    // 与 <video> 同理：不带 crossorigin 的媒体是"被污染"的，本机/分离模式下跨源。
    audio.crossOrigin = "anonymous";
    audio.preload = "auto";
    audio.src = await backend.mediaUrlFor(assetId);
    host.appendChild(audio);
    try {
      await new Promise((resolve, reject) => {
        audio.addEventListener("loadeddata", resolve, { once: true });
        audio.addEventListener("error", () => reject(new Error("加载失败")), { once: true });
      });
      audioSources.set(assetId, audio);
      loaded.push(assetId);
    } catch (error) {
      notice("音频素材 " + assetId + " 取不到（" + audio.src + "）—— 这一路不会出声。");
    }
  }
  return loaded;
}

/** 播放时所有 <audio> 的总音量。1 = 原样，0 = 静音。 */
const audioState = { muted: false, volume: 0.8 };

function applyAudioVolume() {
  for (const audio of audioSources.values()) {
    audio.volume = audioState.muted ? 0 : audioState.volume;
  }
}

/**
 * 把音频**对齐到播放头所在的帧**。
 *
 * 只有一处换算：帧号 → 秒。用的是**工程自己的时间基**（有理数转浮点只在这一步），
 * 与 `sequenceFps()` 同一个来源。
 *
 * `playing` 时逐拍调用；不播时只在 seek 对齐一次（对齐但不播）。
 *
 * **容差**：小于 80ms 的偏差不去动它。每拍都硬写 `currentTime` 会让浏览器反复重新缓冲，
 * 听起来是持续的爆音 —— 那比一点点漂更糟。超过容差才纠。
 */
function syncAudioToFrame(frame, playing) {
  if (audioSources.size === 0) return;
  const base = state.doc === null ? null : state.doc.timeline.timebase;
  if (base === null || !(base.num > 0) || !(base.den > 0)) return;
  // 帧号 → 秒：帧 * den / num。整数帧号是唯一的真相，这里只是给 <audio> 用。
  const seconds = frame * base.den / base.num;
  for (const audio of audioSources.values()) {
    if (!Number.isFinite(audio.duration)) continue;
    if (Math.abs(audio.currentTime - seconds) > 0.08) {
      try { audio.currentTime = Math.max(0, Math.min(seconds, audio.duration)); } catch (error) { /* 还没就绪 */ }
    }
    if (playing === true) {
      if (audio.paused) { audio.play().catch(() => { /* 浏览器可能拒绝自动播放 */ }); }
    } else if (!audio.paused) {
      audio.pause();
    }
  }
}

// --- 播放 -------------------------------------------------------------------------
//
// **播放是"按帧号推进的循环"，不是"让视频自己播"。**
//
// 素材 `<video>` 是被 seek 出来供渲染取帧用的；让它自己播就等于预览有了第二个时钟，
// 而本工程的核心命题是"同一个工程在预览与出片给出可比的帧"。两个时钟一定会漂，
// 漂了之后"预览与出片不一样"就再也说不清是谁的问题。
//
// 所以这里只有一件事：**按序列帧率算出这一拍该到第几帧，然后 seek 过去。**

const playback = {
  playing: false,
  // 上一拍的墙钟时刻（毫秒）。用来算"这一拍该走几帧" ——
  // 每拍固定 +1 的话，慢机器上播放会变成慢动作，而"慢动作"看起来像渲染卡住。
  lastTickMs: 0,
  // **欠账**（毫秒）。每一拍只走整数帧，余下不足一帧的时间必须攒着 ——
  // 直接四舍五入的话，30fps 与 60Hz 的 rAF 之间每拍只有 0.5 帧，
  // 舍掉就永远走不动（第一版正是这样：6 秒只走到第 4 帧）。
  carryMs: 0,
  rafId: 0,
  // --- 丢帧统计 ---------------------------------------------------------------
  //
  // **跟不上的时候允许丢帧，但丢了多少必须说出来。**
  //
  // 预览追不上序列帧率是正常的（素材解码是瓶颈），而"画面在跳"这件事本身
  // 看不出跳了几帧、也看不出是这台机器不行还是片子太重。所以每一次跳过的帧
  // 都记下来，播完/暂停时给出总数与占比。
  //
  // 两处来源分开记 —— 它们的**原因完全不同**，混成一个数就没法诊断：
  //   * `skipped`：按时间推进时一步跨了多帧（解码跟不上，跳着播是对的）；
  //   * `stalled`：单拍耗时超过上限而被**砍掉**的时间折算的帧（切标签页回来、
  //     或一次卡顿很久）。那部分时间没有换算成帧，所以只能**估算**着记。
  dropped: {
    skipped: 0,      // 时间推进跨过的帧数（确定的）
    stalledMs: 0,    // 被上限砍掉的毫秒数（原始事实）
    stalledFrames: 0,// 上面那些毫秒按当前帧率折算的帧数（估算的）
    rendered: 0,     // 真正 seek 并画出来的帧数
    ticks: 0,        // 跑了多少拍
  },
  // **一帧到底花在哪**（播放中实测）。丢帧统计只说"丢了几帧",
  // 说不出"为什么丢" —— 这两个数分开才指向能改的地方。
  cost: {
    seekMs: [],      // seekTo 的整体耗时（就是用户感到的那一段）
    prepareMs: [],   // 其中 prepare（等 <video> 解码 + 文字栅格化）
    drawMs: [],      // 其中 draw（宿主合成上屏）
  },
  // 明显超预算的那些帧（帧号 + 慢在哪几路源）。**用来归因，不只是用来抱怨。**
  slowFrames: [],
  // 慢帧的 prepare 内部拆解（只在明显慢的帧上采样，别把有用的淹掉）。
  prepareBreakdown: [],
};

/** 序列帧率（帧/秒）。时间基是**有理数**，整数帧号 ↔ 秒只在渲染与这里换算。 */
function sequenceFps() {
  const base = state.doc === null || state.doc === undefined ? null : state.doc.timeline.timebase;
  if (base === null || !(base.num > 0) || !(base.den > 0)) return 30;
  return base.num / base.den;
}

function isPlaying() {
  return playback.playing === true;
}

/**
 * 开始播放。
 *
 * 到末帧就**停住**（不回卷、不循环）：回卷会让"导出范围"在播放中莫名其妙地跳，
 * 而循环播放是另一个功能，混进来会让"什么时候停"说不清。
 */
function play() {
  if (playback.playing) return;
  const end = Math.max(1, state.engine.endFrame());
  if (state.frame >= end - 1) {
    // 停在末帧时按播放 = 从头再来（用户意图显然是"再看一遍"，不是"什么都不发生"）。
    seekTo(0).catch((error) => log(String(error)));
  }
  playback.playing = true;
  playback.lastTickMs = performance.now();
  playback.carryMs = 0;   // 上一次播放攒下的欠账不能带进来（否则一按就跳一帧）
  // 统计是"这一次播放"的 —— 不清的话上一次的丢帧会累加到这一次，看起来像越来越糟。
  resetPlaybackStats();
  renderPlaybackStats();
  setIcon("play", "pause");
  $("play").classList.add("playing");
  applyAudioVolume();
  syncAudioToFrame(state.frame, true);
  void pumpPlayback();
}

function pause() {
  if (!playback.playing) return;
  playback.playing = false;
  if (playback.rafId !== 0) {
    cancelAnimationFrame(playback.rafId);
    playback.rafId = 0;
  }
  setIcon("play", "play");
  $("play").classList.remove("playing");
  // 停播时**只是暂停**、不回卷：用户按暂停是想停在这一刻看，不是想回开头。
  syncAudioToFrame(state.frame, false);
  // 播完/暂停时把丢帧如实说出来。**这是这一轮的关键**：
  // 画面在跳是看得见的，"跳了几帧、为什么跳"看不见 —— 那个必须由这里给。
  renderPlaybackStats();
}

/**
 * 把播放统计写进状态栏。
 *
 * 丢帧时用 warn 色 —— **不丢帧是正常态，不该满屏绿**；丢了才需要看见。
 */
function renderPlaybackStats() {
  const host = $("playStats");
  if (host === null) return;
  const stats = playbackStats();
  // 空文本就整个藏起来：一个写着"无丢帧"的胶囊在没播过的时候是噪音，
  // 而"没播过"与"播了且没丢"是两件事。
  if (stats.ticks === 0) { host.textContent = ""; host.hidden = true; return; }
  host.hidden = false;
  host.textContent = stats.text;
  // className 是**整个换掉**的（这里原来就是覆盖写法）：胶囊的底色要跟着丢帧走。
  host.className = stats.droppedTotal > 0 ? "chip a" : "chip g";
}

function togglePlay() {
  if (playback.playing) pause();
  else play();
}

/**
 * 每一拍：**按实际经过的时间**推进，而不是"每拍 +1"。
 *
 * 掉帧时（渲染一帧要 100ms 而帧率是 30fps）"每拍 +1"会让播放变成慢动作 —— 那看起来
 * 像卡住，而真正的信息（这台机器跟不上这个帧率）反而看不出来。按时间推进则**跳帧**，
 * 播放速度始终是真实的；跟不上时画面是跳的，那是能看懂的信号。
 *
 * 但要**限幅**：切标签页回来时 `performance.now()` 会跳很大一截，
 * 不限的话一次算出一大段要跳的帧，于是直接冲到末帧。
 */
async function pumpPlayback() {
  if (!playback.playing) return;
  const end = Math.max(1, state.engine.endFrame());
  const now = performance.now();
  const elapsedMs = Math.max(0, now - playback.lastTickMs);
  playback.lastTickMs = now;
  // 切标签页回来时 performance.now() 会跳一大截：**超出部分直接丢掉**，
  // 不然一次算出一大段要跳的帧，画面会直接冲到末帧。
  //
  // 砍掉多少也**记下来** —— 那是"没画出来的时间"，与"跨了几帧"不是一回事：
  // 这一段根本没进入帧号换算，所以只能说"约等于几帧"（用当前帧率折算）。
  const usableMs = Math.min(elapsedMs, 250);
  if (elapsedMs > usableMs) {
    playback.dropped.stalledMs += elapsedMs - usableMs;
    // 估算：按序列帧率折算。**四舍五入到整数**，因为最终要跟用户说"大约几帧"。
    playback.dropped.stalledFrames += Math.round((elapsedMs - usableMs) / (1000 / sequenceFps()));
  }
  playback.carryMs += usableMs;
  const frameMs = 1000 / sequenceFps();
  // 欠账够一帧才走；**不够就攒着**（这就是 carryMs 存在的理由）。
  const step = Math.floor(playback.carryMs / frameMs);
  if (step > 0) playback.carryMs -= step * frameMs;
  playback.dropped.ticks += 1;
  if (step > 0) {
    // **跨了不止一帧就是丢了 step-1 帧。**
    // 只跨一帧是正常的逐帧播放；跨 n 帧说明中间 n-1 帧没画。
    if (step > 1) playback.dropped.skipped += step - 1;
    const next = state.frame + step;
    if (next >= end - 1) {
      // 末尾那一段：从当前帧到末帧之间的空隙同样是没画的帧。
      playback.dropped.skipped += Math.max(0, (end - 1) - state.frame - 1);
      await seekTo(end - 1);
      playback.dropped.rendered += 1;
      pause();
      return;
    }
    await seekTo(next);
    playback.dropped.rendered += 1;
    // 音频跟着**帧号**对齐（不是"按播放键时各自起跑"）：每拍纠一次，容差见 syncAudioToFrame。
    syncAudioToFrame(next, true);
  }
  playback.rafId = requestAnimationFrame(() => { void pumpPlayback(); });
}

/**
 * 把丢帧统计说成一句人能读的话。
 *
 * **没有丢就说没有** —— 不许因为"看起来还行"就不报，也不许把 0 说成"流畅"
 * （流畅与丢了几帧是两件事，前者是感受，后者是事实）。
 */
function describeDrops(stats) {
  const total = stats.skipped + stats.stalledFrames;
  if (total === 0) {
    return "无丢帧（" + stats.rendered + " 帧全部逐帧画出）";
  }
  const parts = [];
  if (stats.skipped > 0) parts.push("跳过 " + stats.skipped + " 帧");
  if (stats.stalledFrames > 0) {
    parts.push("卡顿丢弃约 " + stats.stalledFrames + " 帧（" + Math.round(stats.stalledMs) + "ms）");
  }
  return "丢帧 " + total + " 帧：" + parts.join("、") + "；实际画出 " + stats.rendered + " 帧";
}

/** 播放统计的快照（给状态栏与验收判定读）。**返回副本**，别把内部对象递出去。 */
/** 一组耗时样本的汇总。空集返回 null —— **没有样本就说没有，不给 0 冒充**。 */
function summarize(list) {
  if (!Array.isArray(list) || list.length === 0) return null;
  const sorted = list.slice().sort((a, b) => a - b);
  const sum = sorted.reduce((a, b) => a + b, 0);
  return {
    n: sorted.length,
    medianMs: Number(sorted[Math.floor(sorted.length / 2)].toFixed(2)),
    meanMs: Number((sum / sorted.length).toFixed(2)),
    maxMs: Number(sorted[sorted.length - 1].toFixed(2)),
  };
}

function playbackStats() {
  const d = playback.dropped;
  return {
    skipped: d.skipped,
    stalledFrames: d.stalledFrames,
    stalledMs: d.stalledMs,
    rendered: d.rendered,
    ticks: d.ticks,
    droppedTotal: d.skipped + d.stalledFrames,
    // 丢帧率：分母是"本该画的帧数" = 画出的 + 丢掉的。
    dropRatio: (d.rendered + d.skipped + d.stalledFrames) > 0
      ? (d.skipped + d.stalledFrames) / (d.rendered + d.skipped + d.stalledFrames)
      : 0,
    text: describeDrops(d),
    // **一帧花在哪**：seekMs 是用户感到的那一段，prepare/draw 是它的两半。
    // 报 null 而不是 0，因为"没采到样"与"耗时为零"是两件事。
    cost: {
      seek: summarize(playback.cost.seekMs),
      prepare: summarize(playback.cost.prepareMs),
      draw: summarize(playback.cost.drawMs),
    },
    // 超预算的帧（归因用）：帧号 + 当时需要哪几路源。
    slowFrames: playback.slowFrames.slice(),
    prepareBreakdown: playback.prepareBreakdown.slice(),
  };
}

/** 清零统计。**开始播放时清** —— 统计是"这一次播放"的，不是历史累计。 */
function resetPlaybackStats() {
  playback.dropped = { skipped: 0, stalledMs: 0, stalledFrames: 0, rendered: 0, ticks: 0 };
  playback.cost = { seekMs: [], prepareMs: [], drawMs: [] };
  playback.slowFrames = [];
  playback.prepareBreakdown = [];
}

// --- 播放头 -----------------------------------------------------------------------

async function seekTo(frame) {
  const end = state.engine.endFrame();
  state.frame = Math.max(0, Math.min(frame, Math.max(0, end - 1)));
  $("frame").value = String(state.frame);
  // 播放头一动，盖着的那张出片帧就**不再对应当前这一帧**了 —— 收回去。
  // 留着它比让人比错更坏：两张不同的帧叠在一起，「看起来不一样」会被当成渲染不一致。
  hideRealFrame();
  // 只动两个子节点的 textContent。**不用 innerHTML** —— 播放中每帧都走这一行，
  // 每帧重建一次 DOM 是在给 GC 制造工作量，而它换不来任何东西。
  $("frameNow").textContent = String(state.frame);
  $("frameTime").textContent = formatTimecode(state.frame, sequenceFps());
  // 不播时把音频**对齐但不播** —— 拖动播放头之后再按播放，声音要从那个位置起，
  // 而不是从上次停下的地方接着走。
  if (!isPlaying()) syncAudioToFrame(state.frame, false);
  // **播放中才记代价**：拖动播放头时的单次耗时是另一件事
  // （用户拖一下等 200ms 是可以接受的，播放中每帧等 200ms 就是幻灯片）。
  // 混在一起统计，两个场景的数会互相污染，谁都不准。
  if (isPlaying()) {
    const t0 = performance.now();
    await state.engine.seek(state.frame);
    const total = performance.now() - t0;
    pushCost("seekMs", total);
    const cost = state.engine.lastSeekCost;
    if (cost !== undefined) {
      // prepare 里既有解码也有文字栅格化；draw 是宿主合成上屏。
      // 两者差一个量级时，该改哪边是清楚的。
      pushCost("prepareMs", cost.prepareMs);
      pushCost("drawMs", cost.drawMs);
    }
    // prepare 内部再拆一层：**"prepare 慢"本身不是一个可行动的结论** ——
    // 要能说出是 seek、createImageBitmap、set_bitmap（拷进 GPU 纹理）还是文字。
    const bd = state.engine.lastPrepareBreakdown;
    if (bd !== undefined && bd.prepareTotalMs > 5) {
      playback.prepareBreakdown.push(bd);
      if (playback.prepareBreakdown.length > 60) playback.prepareBreakdown.shift();
    }
    // **慢的那几帧是谁。** 只报分布的话，"最坏 346ms"这句话无法归因 ——
    // 而"哪一路素材、哪个成帧区间慢"才是能动手的地方。
    // 只留慢过预算两倍的：全记下来会把有用的那几条淹掉。
    if (total > 2 * (1000 / sequenceFps())) {
      const sources = state.engine.sourcesFor(state.frame);
      playback.slowFrames.push({
        frame: state.frame,
        totalMs: Number(total.toFixed(1)),
        // **空标识要起个名字，不能留白。** 调整图层没有素材，source 是空串 ——
        // 原样印出来会变成一个说不清的 " x7"，看的人只会以为是统计坏了。
        sources: sources.map((entry) => (entry.source === ""
          ? "调整图层"
          : entry.source + "@" + entry.source_frame)),
      });
      if (playback.slowFrames.length > 40) playback.slowFrames.shift();
    }
    return;
  }
  await state.engine.seek(state.frame);
}

/** 记一个耗时样本。**只留最近 240 个** —— 统计是给人看的，不是给机器存档案的。 */
function pushCost(bucket, ms) {
  const list = playback.cost[bucket];
  list.push(ms);
  if (list.length > 240) list.shift();
}

// --- 导出 -------------------------------------------------------------------------

/** 把预检结果回报给验收驱动。**没有这一步，「跑过」与「跳过」在外部看起来一样。** */
async function reportPrecheck(outcome, issues) {
  try {
    await fetch("/precheck-result", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ outcome: outcome, issues: issues || [] }),
    });
  } catch (error) {
    // 回报失败**不能**影响导出本身 —— 它是观测手段，不是功能。
  }
}

/**
 * 提交前预检。返回 true 表示**已被拦住**，不该继续提交。
 *
 * 没有这一步，用户要等**分钟级任务跑完**才被告知"某一条对端不支持"。
 * 而能力声明从哪来、规则是什么，都不在这个文件里：
 * 前者问 backend，后者由 Rust 给出 —— 这里只负责把它们接上并显示。
 */
async function precheckBeforeExport() {
  state.capabilities = null;
  try {
    state.capabilities = await backend.capabilities();
  } catch (error) {
    // 拿不到能力声明**不是**拦截理由：降级模式本来就没有后端。
    // 但要让人看得见这件事，而不是静默跳过 —— 静默跳过会让"没预检"
    // 和"预检通过"看起来一模一样。
    notice("拿不到对端能力声明，本次跳过预检：" + String(error));
    await reportPrecheck("error", []);
    return false;
  }
  if (state.capabilities === null) {
    // 降级模式：对端不渲染也不出片，无从预检。
    await reportPrecheck("skipped", []);
    return false;
  }
  const issues = state.engine.precheck(state.capabilities);
  if (issues.length === 0) {
    await reportPrecheck("passed", []);
    return false;
  }
  state.issues = issues;
  renderIssues();
  await reportPrecheck("blocked", issues);
  return true;
}

function showProgress(fraction, phase) {
  const bar = $("progressBar");
  if (fraction === null || fraction === undefined) {
    $("progressText").textContent = "后端出片中…（进度未知）";
    bar.style.width = "100%";
    bar.className = "indeterminate";
    return;
  }
  $("progressText").textContent = "后端出片中… " + Math.round(fraction * 100) + "%";
  bar.className = "";
  bar.style.width = Math.round(fraction * 100) + "%";
  void phase;
}

/** 有编码器的后端：把工程提交过去，轮询到出片，给出下载链接。 */
async function submitToBackend(http, from, to) {
  // 出片尺寸：优先取工程文件里的 render_hints，缺了就取预览画布。
  // **与预览一致**才好比：两边尺寸不同的话，"看起来不一样"永远说不清是谁的问题。
  const hints = state.doc.render_hints || {};
  const canvas = $("preview");
  const options = {
    format: "mp4",
    width: Number(hints.width) > 0 ? Number(hints.width) : canvas.width,
    height: Number(hints.height) > 0 ? Number(hints.height) : canvas.height,
  };
  $("progress").hidden = false;
  $("download").hidden = true;
  log("已提交给后端出片：帧 " + from + ".." + to + "，尺寸 " + options.width + "x" + options.height + " …");
  showProgress(0, "queued");
  try {
    const status = await http.exportProject(state.doc, { from: from, to: to }, showProgress, options);
    const url = http.downloadUrl(status);
    if (url === null) throw new Error("后端说出片成功了，却没给下载地址");
    const link = $("download");
    link.href = url;
    link.hidden = false;
    link.textContent = "下载成片";
    showProgress(1, "succeeded");
    log("成片已就绪。");
    await fetch("/export-done", {
      method: "POST",
      body: JSON.stringify({ from: from, to: to, download_url: url, width: options.width, height: options.height }),
    });
  } catch (error) {
    const message = String(error && error.message ? error.message : error);
    log("后端出片失败：" + message);
    $("progress").hidden = true;
    try { await fetch("/export-failed", { method: "POST", body: message }); } catch (ignore) { /* 观测手段 */ }
  }
}

/** 降级模式：逐帧渲染 PNG，交给驱动侧的编码器。 */
async function renderPngSequence(from, to) {
  try {
    log("逐帧渲染 " + from + ".." + to + " …");
    await exportPngSequence(state.engine, state.doc, { from: from, to: to }, async (frame, bytes, total) => {
      await fetch("/frame-png?frame=" + frame, { method: "POST", body: bytes });
      if ((frame - from) % 10 === 0) log("已渲染 " + (frame - from + 1) + "/" + total + " 帧");
    });
    log("帧序列已交给驱动，等 FFmpeg 编码。");
    await fetch("/export-done", { method: "POST", body: JSON.stringify({ from: from, to: to }) });
  } catch (error) {
    const message = String(error && error.message ? error.message : error);
    log("导出失败：" + message);
    await fetch("/export-failed", { method: "POST", body: message });
  }
}

async function runExport() {
  const only = document.getElementById("frameOnly");
  const from = Number(only.dataset.from);
  const to = Number(only.dataset.to);

  // **提交前**预检：把对端做不了的东西现在就指出来，而不是等任务跑完。
  if (await precheckBeforeExport()) return;

  const capabilities = state.capabilities;
  // **分路的判据是能力声明，不是部署形态。** 谁说自己能编码，就把工程交给谁。
  if (capabilities !== null && capabilities.has_encoder === true && typeof backend.baseUrl === "string") {
    return submitToBackend(createHttpBackend(backend.baseUrl), from, to);
  }
  return renderPngSequence(from, to);
}

// --- 真实出片帧（预览 vs 出片那条路） ---------------------------------------------
//
// **这是本工程那条核心承诺的兑现口**：同一个工程，预览与出片必须给出可比的帧。
// 点一下按钮，后端用 `dhampir frame`（与 render 同一条 Rust 路径）渲染**当前这一帧**，
// 把 PNG 取回来盖在画布上，并报出两边的像素差。
//
// 为什么要报一个**数**：这类「看起来一样」的判断最容易自我安慰 ——
// 分辨率、色深、缩放都可能在骗眼睛。给一个逐通道差的最大值与均值，
// 对了是证据，不对也是证据。

/**
 * 逐像素比两块 ImageData。**尺寸不同就不比** —— 缩放比出来的数没有意义。
 *
 * 除了最大/均值，还算三样能把「为什么不一样」分开的东西：
 *   * 分通道均值 —— 三个通道一起偏，是**色彩换算**（YUV->RGB 的口径不同），不是内容不一致；
 *   * 逐像素全等比例 —— 有多少像素三个通道都几乎一样；
 *   * 4x4 块均值的差 —— **对滤波不敏感**的那一档。全分辨率差得大、块均值差得小，
 *     说明差异集中在前者的**高频**上（缩放的采样滤波器不同），而不是画面内容不同。
 */
function diffImageData(a, b, width, height) {
  let maxAbs = 0;
  let sum = 0;
  let over = 0;
  let same = 0;
  const chan = [0, 0, 0];
  const n = a.length;
  for (let i = 0; i < n; i += 4) {
    let worst = 0;
    for (let c = 0; c < 3; c += 1) {
      const d = Math.abs(a[i + c] - b[i + c]);
      chan[c] += d;
      if (d > worst) worst = d;
      if (d > maxAbs) maxAbs = d;
      sum += d;
      if (d > 2) over += 1;
    }
    if (worst <= 1) same += 1;
  }
  const pixels = n / 4;
  let blockSum = 0;
  let blocks = 0;
  for (let by = 0; by + 4 <= height; by += 4) {
    for (let bx = 0; bx + 4 <= width; bx += 4) {
      let sa = 0;
      let sb = 0;
      for (let y = 0; y < 4; y += 1) {
        for (let x = 0; x < 4; x += 1) {
          const i = ((by + y) * width + (bx + x)) * 4;
          sa += a[i] + a[i + 1] + a[i + 2];
          sb += b[i] + b[i + 1] + b[i + 2];
        }
      }
      blockSum += Math.abs(sa - sb) / 48;
      blocks += 1;
    }
  }
  return {
    maxAbs: maxAbs,
    meanAbs: n > 0 ? sum / n : 0,
    overRatio: n > 0 ? over / n : 0,
    samePixelRatio: pixels > 0 ? same / pixels : 0,
    chanMean: pixels > 0 ? chan.map((v) => v / pixels) : [0, 0, 0],
    blockMeanAbs: blocks > 0 ? blockSum / blocks : 0,
    channels: n,
  };
}

/**
 * 把当前画布抄成一块 ImageData。**必须在同一轮任务里调**（调用处有说明）。
 *
 * 顺带判一下它是不是**纯色**：纯色说明根本没抄到画面，而这时的差异读数不可信 ——
 * 把它报成「渲染不一致」是最坏的一种错，因为方向完全反了。
 */
function capturePreviewPixels(canvas) {
  const scratch = document.createElement("canvas");
  scratch.width = canvas.width;
  scratch.height = canvas.height;
  const ctx = scratch.getContext("2d", { willReadFrequently: true });
  ctx.drawImage(canvas, 0, 0);
  const data = ctx.getImageData(0, 0, canvas.width, canvas.height).data;
  let first = null;
  let uniform = true;
  for (let i = 0; i < data.length; i += 4) {
    if (first === null) { first = [data[i], data[i + 1], data[i + 2]]; continue; }
    if (data[i] !== first[0] || data[i + 1] !== first[1] || data[i + 2] !== first[2]) {
      uniform = false;
      break;
    }
  }
  return { data: data, width: canvas.width, height: canvas.height, uniform: uniform };
}
/** 把盖着的那张后端帧抄成 ImageData（比对要用它的像素）。 */
function captureRealFramePixels() {
  const canvas = $("preview");
  const img = $("realFrame");
  const scratch = document.createElement("canvas");
  scratch.width = canvas.width;
  scratch.height = canvas.height;
  const ctx = scratch.getContext("2d", { willReadFrequently: true });
  ctx.drawImage(img, 0, 0, canvas.width, canvas.height);
  return ctx.getImageData(0, 0, canvas.width, canvas.height).data;
}
/** 收回盖着的那张出片帧。播放头一动它就不再对应当前这一帧了。 */
function hideRealFrame() {
  const img = $("realFrame");
  if (img === null || img.hidden) return;
  img.hidden = true;
  $("stageTag").hidden = true;
  $("realFrameBtn").classList.remove("primary");
}

async function showRealFrame() {
  const button = $("realFrameBtn");
  const img = $("realFrame");
  const tag = $("stageTag");
  const canvas = $("preview");
  if (state.doc === null) { toast("工程还没载入", "warn"); return { ok: false, reason: "工程还没载入" }; }

  // 再点一次 = 收回比对（按钮是开关，不是只能往前）。
  if (!img.hidden) {
    hideRealFrame();
    // 带 reason：这条路径也可能是"判定被跑了第二遍"，
    // 那时只回一个没有理由的 ok 会让驱动打出 undefined，看不出发生了什么。
    return { ok: true, closed: true, compared: false, reason: "这一次没有取（已经处于展示状态）" };
  }

  button.disabled = true;
  button.textContent = "渲染中…";
  const frame = state.frame;
  // **先把预览这一帧抄下来，再去取后端那一帧。**
  // 不能等 fetch 回来再读画布：WebGPU 交换链的内容**不保证跨任务还在**，
  // 中间隔了几个 await 之后 drawImage 很可能拿到空白 ——
  // 而空白的表现是"两边差得离谱"，会被误读成"渲染不一致"，方向正好是反的。
  const preview = capturePreviewPixels(canvas);
  try {
    const response = await fetch("/frame", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ project: state.doc, frame: frame }),
    });
    if (!response.ok) {
      let detail = "HTTP " + response.status;
      try {
        const parsed = await response.json();
        if (parsed && parsed.error && parsed.error.message) detail = parsed.error.message;
      } catch (error) { /* 不是 JSON 就用状态码 */ }
      throw new Error(detail);
    }
    // **确认拿到的就是我要的那一帧**，不是「看起来像」。
    //
    // 注意 `Number(null) === 0`：头不存在时直接 Number() 会得到 0，
    // 于是"头没了"会被报成"后端回的是第 0 帧" —— 一个完全错误的方向。
    // 所以先判 null，再转数。
    const rawFrame = response.headers.get("x-dhampir-frame");
    const got = rawFrame === null ? Number.NaN : Number(rawFrame);
    if (Number.isFinite(got) && got !== frame) {
      throw new Error("后端回的是第 " + got + " 帧，我要的是第 " + frame + " 帧");
    }
    const blob = await response.blob();
    await new Promise((resolveLoad, rejectLoad) => {
      img.onload = () => resolveLoad(undefined);
      img.onerror = () => rejectLoad(new Error("取回来的 PNG 解不开"));
      img.src = URL.createObjectURL(blob);
    });
    img.hidden = false;
    button.classList.add("primary");

    // ---- 比对 ----
    // 出片尺寸与画布尺寸**可以不同**（前者来自 render_hints）。不同就不报差值：
    // 缩放之后逐像素比出来的数不是「渲染差异」，是「缩放差异」，报出来会误导。
    const outW = img.naturalWidth;
    const outH = img.naturalHeight;
    if (outW !== canvas.width || outH !== canvas.height) {
      tag.textContent = "出片 " + outW + "x" + outH + " · 预览 " + canvas.width + "x"
        + canvas.height + " · 尺寸不同，未逐像素比";
      tag.hidden = false;
      toast("出片帧尺寸与预览画布不同（" + outW + "x" + outH + " vs " + canvas.width + "x"
        + canvas.height + "）：尺寸不同就不报像素差", "warn", 5200);
      // **比对没做就是没做**：返回 compared:false，而不是塞一个 maxAbs:0 上去。
      // 0 会被读成"完全一致"，那是这个功能最不该制造的误解。
      return { ok: true, frame: frame, compared: false, width: outW, height: outH,
        reason: "出片帧与预览画布尺寸不同，未逐像素比" };
    }
    // 抄到纯色说明**没抄到画面**（读回时机不对 / 画布没内容）。
    // 这时报差异就是把"我读错了"说成"渲染不一致" —— 宁可不给结论。
    if (preview.uniform) {
      tag.textContent = "预览画布读回来是纯色 —— 读数不可信，不当作渲染差异";
      tag.hidden = false;
      toast("预览画布读回来是纯色：这是**读数**的问题，不是渲染不一致。先别信这次的差值。",
        "warn", 6000);
      return { ok: true, frame: frame, compared: false,
        reason: "预览画布读回来是纯色，读数不可信" };
    }
    const scratch = document.createElement("canvas");
    scratch.width = canvas.width;
    scratch.height = canvas.height;
    const ctx = scratch.getContext("2d", { willReadFrequently: true });
    ctx.drawImage(img, 0, 0, canvas.width, canvas.height);
    const realData = ctx.getImageData(0, 0, canvas.width, canvas.height).data;
    const d = diffImageData(preview.data, realData, canvas.width, canvas.height);
    tag.textContent = "出片路径渲染 · 第 " + frame + " 帧 · 最大通道差 " + d.maxAbs
      + " · 均值 " + d.meanAbs.toFixed(3)
      + " · 超阈值通道 " + (d.overRatio * 100).toFixed(2) + "%";
    tag.hidden = false;
    toast("出片帧已取回：第 " + frame + " 帧，最大通道差 " + d.maxAbs,
      d.maxAbs === 0 ? "ok" : "warn", 4200);
    return { ok: true, frame: frame, compared: true, width: outW, height: outH,
      maxAbs: d.maxAbs, meanAbs: d.meanAbs, overRatio: d.overRatio,
      samePixelRatio: d.samePixelRatio, chanMean: d.chanMean, blockMeanAbs: d.blockMeanAbs };
  } catch (error) {
    const message = String(error && error.message ? error.message : error);
    log("取真实出片帧失败：" + message);
    toast("取真实出片帧失败：" + message, "err", 6000);
    return { ok: false, reason: message };
  } finally {
    button.disabled = false;
    button.textContent = "真实出片帧";
  }
}
// --- 启动 -------------------------------------------------------------------------

async function main() {
  mark("main 进入");
  const engine = await loadEngine("/pkg/dhampir_wasm.js");
  state.engine = engine;
  mark("wasm 已加载");

  const text = await backend.loadProject();
  mark("工程文本已取到");
  const opened = engine.open(text);
  mark("工程已解析");
  if (opened.ok !== true) {
    // **失败也要把原因显示出来**，而不是只写一句"没通过校验"。
    state.issues = opened.issues || [];
    state.warnings = opened.warnings || [];
    renderIssues();
    log("工程没通过校验：" + (opened.error || (state.issues.length + " 条错误")));
    return;
  }
  state.issues = opened.issues || [];
  state.warnings = opened.warnings || [];
  state.doc = engine.doc();
  mark("工程文件已就绪：" + state.doc.timeline.tracks.length + " 条轨道");

  const canvas = $("preview");
  // 画布尺寸是**宿主参数**，不是契约字段 —— 契约里故意没有分辨率，
  // 因为「预览尺寸归宿主」。工程文件里的 render_hints 是**宿主的提示**，不是契约。
  const requestedSize = new URLSearchParams(location.search).get("canvas");
  if (requestedSize) {
    const parts = requestedSize.split("x").map(Number);
    if (parts.length === 2 && parts[0] > 0 && parts[1] > 0) {
      canvas.width = parts[0];
      canvas.height = parts[1];
      mark("画布已设为 " + parts[0] + "x" + parts[1]);
    }
  }
  await engine.attach("preview");
  // 源模式是个**会随浏览器不同而不同**的事实，所以它属于脚印的一部分 ——
  // 出问题时第一眼就该看到它。
  mark("已上屏到 canvas（源模式：" + engine.sourceMode + "）");
  await bindAllSources();
  mark("视频源已绑定");
  // 音频源**在第一次 seek 之前**挂好：`seekTo` 会顺手对齐音频，
  // 那一步没元素就当"这部片子没有声音"——与"音轨没接上"在听感上完全一样。
  await bindAudioSources();
  applyAudioVolume();
  mark("音频源已绑定");
  // 字幕要**在第一次 seek 之前**登记：绘制路径只认宿主手上的那份表，
  // 而"还没登记"与"这部片子没有字幕"在画面上完全一样。
  await loadProjectSubtitles();
  mark("字幕源已登记");

  const end = engine.endFrame();
  mark("endFrame 已返回: " + end);
  $("frame").max = String(Math.max(0, end - 1));
  const only = document.getElementById("frameOnly");
  only.dataset.from = String(Math.max(0, engine.firstFrame()));
  only.dataset.to = String(Math.max(0, end - 1));
  renderTimeline();
  renderInspector();
  renderIssues();
  mark("三个面板已渲染");
  await seekTo(0);
  mark("首帧已上屏");

  // 图标只在启动时铺一次；之后只有播放键与静音键会换脸。
  setIcon("first", "first");
  setIcon("prev", "prev");
  setIcon("play", "play");
  setIcon("next", "next");
  setIcon("last", "last");
  setIcon("muteBtn", audioState.muted ? "volume-x" : "volume");

  // 工程身份写进顶栏。**出片尺寸取自 render_hints 而不是画布** ——
  // 两者可以不同（画布是宿主参数、render_hints 是宿主的提示），
  // 而"预览看到的"与"出片写出来的"尺寸不同时，看的人第一件该知道的就是这件事。
  const hints = state.doc.render_hints || {};
  const outSize = (Number(hints.width) > 0 && Number(hints.height) > 0)
    ? hints.width + "x" + hints.height
    : canvas.width + "x" + canvas.height;
  const tb = state.doc.timeline.timebase;
  $("projectId").textContent = (state.doc.meta && state.doc.meta.title ? state.doc.meta.title : "未命名工程")
    + " · " + state.doc.timeline.tracks.length + " 轨 · 出片 " + outSize
    + " · " + tb.num + "/" + tb.den;

  $("first").addEventListener("click", () => { pause(); seekTo(engine.firstFrame()); });
  $("prev").addEventListener("click", () => { pause(); seekTo(state.frame - 1); });
  $("next").addEventListener("click", () => { pause(); seekTo(state.frame + 1); });
  $("last").addEventListener("click", () => { pause(); seekTo(end - 1); });
  $("play").addEventListener("click", togglePlay);
  $("volume").addEventListener("input", (event) => {
    audioState.volume = Math.max(0, Math.min(1, Number(event.target.value) / 100));
    // 动过音量就等于"我要听声音"：顺手解除静音（否则用户会以为音量坏了）。
    if (audioState.volume > 0) { audioState.muted = false; setIcon("muteBtn", "volume"); }
    applyAudioVolume();
  });
  $("muteBtn").addEventListener("click", () => {
    audioState.muted = !audioState.muted;
    setIcon("muteBtn", audioState.muted ? "volume-x" : "volume");
    applyAudioVolume();
  });
  $("volume").value = String(Math.round(audioState.volume * 100));
  // 拖动滑块 = 手动定位，播放要让位（否则手一松就被播放头拽走，那是"抢方向盘"）。
  $("frame").addEventListener("input", (event) => { pause(); seekTo(Number(event.target.value)); });
  $("fpsLabel").textContent = sequenceFps() + " fps";
  // --- 快捷键 -----------------------------------------------------------------------
  //
  // 一律与按钮**走同一条路**（click() 或同一个函数）：快捷键若有自己的实现，
  // 它与按钮迟早会有两种行为，而"用键盘和用鼠标为什么会不一样"是最难查的那类问题。
  //
  // 输入框里一律让位 —— 否则在数字框里打空格会变成播放，
  // 而那种"快捷键抢输入"的行为用户没法自己发现原因。
  const stepBy = (delta) => { pause(); seekTo(state.frame + delta); };
  // "一秒"按**帧率**换算，不写死 30：29.97 的工程里写死就会每次都差一点。
  const oneSecond = Math.max(1, Math.round(sequenceFps()));
  const setShortcuts = (open) => {
    const panel = $("shortcuts");
    panel.hidden = open === undefined ? !panel.hidden : !open;
  };
  $("shortcutBtn").addEventListener("click", () => setShortcuts());
  // 点背景关掉；点面板本身不关（否则想选文字复制都会被关掉）。
  $("shortcuts").addEventListener("click", (event) => {
    if (event.target === $("shortcuts")) setShortcuts(false);
  });
  window.addEventListener("keydown", (event) => {
    const target = event.target;
    const typing = target !== null && (target.tagName === "INPUT" || target.tagName === "SELECT"
      || target.tagName === "TEXTAREA" || target.isContentEditable === true);
    // 帮助面板开着时：Esc 与 ? 都能关（两个方向都留出口，不必记住是哪一个）。
    if (event.key === "Escape" && !$("shortcuts").hidden) { setShortcuts(false); return; }
    if (event.key === "?" && !typing) { event.preventDefault(); setShortcuts(); return; }
    if (typing) return;
    if ((event.ctrlKey || event.metaKey) && (event.key === "z" || event.key === "Z")) {
      event.preventDefault();
      runHistoryStep(event.shiftKey ? "redo" : "undo").catch((error) => log(String(error)));
      return;
    }
    if (event.code === "Space") {
      event.preventDefault();  // 不拦的话浏览器会把空格当页面滚动
      togglePlay();
      return;
    }
    if (event.key === "m" || event.key === "M") { $("muteBtn").click(); return; }
    if (event.key === "s" || event.key === "S") { $("splitBtn").click(); return; }
    if (event.key === "Delete") { $("removeBtn").click(); return; }
    if (event.key === "ArrowLeft") { stepBy(event.shiftKey ? -oneSecond : -1); }
    else if (event.key === "ArrowRight") { stepBy(event.shiftKey ? oneSecond : 1); }
    // 剪辑台的 J/L 习惯（J 退、L 进）。这里不做变速播放 —— 播放是帧驱动的，
    // 变速意味着"每帧的墙钟预算"跟着变，那是另一个特性，不该顺手塞进快捷键。
    else if (event.key === "," || event.key === "j" || event.key === "J") { stepBy(-1); }
    else if (event.key === "." || event.key === "l" || event.key === "L") { stepBy(1); }
    else if (event.key === "Home") { pause(); seekTo(engine.firstFrame()); }
    else if (event.key === "End") { pause(); seekTo(end - 1); }
  });
  $("realFrameBtn").addEventListener("click", () => {
    showRealFrame().catch((error) => log(String(error)));
  });
  $("export").addEventListener("click", runExport);
  $("undoBtn").addEventListener("click", () => { runHistoryStep("undo").catch((error) => log(String(error))); });
  $("redoBtn").addEventListener("click", () => { runHistoryStep("redo").catch((error) => log(String(error))); });
  $("splitBtn").addEventListener("click", () => {
    const layer = selectedLayer();
    if (layer === null) { log("先选中一个元素再剃刀"); return; }
    runEdit({ op: "split", layer: layer.id, at: state.frame }, "剃刀").catch((error) => log(String(error)));
  });
  $("removeBtn").addEventListener("click", () => {
    const layer = selectedLayer();
    if (layer === null) { log("先选中一个元素再删除"); return; }
    runEdit({ op: "remove", layer: layer.id, ripple: false }, "删除").catch((error) => log(String(error)));
  });
  $("rippleBtn").addEventListener("click", () => {
    const layer = selectedLayer();
    if (layer === null) { log("先选中一个元素再波纹删除"); return; }
    runEdit({ op: "remove", layer: layer.id, ripple: true }, "波纹删除").catch((error) => log(String(error)));
  });
  $("applyFps").addEventListener("click", () => {
    const fps = Number($("seqFps").value);
    if (!(fps > 0)) { log("序列帧率要是一个正数"); return; }
    runEdit({ op: "set_sequence", timebase: { num: fps, den: 1 }, width: 0, height: 0 }, "序列帧率")
      .catch((error) => log(String(error)));
  });
  $("seqFps").value = String(state.doc.timeline.timebase.num / state.doc.timeline.timebase.den);
  await loadLibrary();
  renderLibrary();
  only.dataset.ready = "1";
  canvas.dataset.ready = "1";
  window.dhampirReady = true;
  mark("启动完成");

  // 判定回传：?verdict=<name> 让页面自己跑一次验收并**主动回传**结果。
  // 与 export 一样只在显式带参数时生效 —— 这是给验收用的入口，不是产品功能。
  const verdictName = new URLSearchParams(location.search).get("verdict");
  if (verdictName !== null && verdictName !== "") {
    const run = VERDICTS[verdictName];
    if (run === undefined) {
      // 名字写错也要回传一条 —— 什么都不回传的话，驱动看到的是"没拿到判定"，
      // 那是通道问题（后端没起来/页面没到那一步），与"名字写错了"完全不是一回事。
      await reportVerdict(verdictName, {
        ok: false,
        reason: "没有这个判定：" + verdictName + "（认得的是 " + Object.keys(VERDICTS).join(" / ") + "）",
      });
    } else {
      await run(verdictName);
    }
    // **告诉驱动这一轮结束了。** 不然它只能靠超时收场，而"等超时"看起来和"卡住"一样。
    try { navigator.sendBeacon("/result", JSON.stringify({ verdict: verdictName })); }
    catch (error) { /* 观测手段不该影响结论 */ }
  }

  // 程序化验收用的自动导出钩子：driver 无法点按钮，用查询参数触发。
  // 这是"给测试用的入口"，不是产品功能 —— 所以只在显式带上参数时才生效。
  if (new URLSearchParams(location.search).get("export") === "1") {
    await runExport();
  }
}

main().catch((error) => {
  const message = String(error && error.message ? error.message : error);
  mark("启动失败：" + message);
  log("启动失败：" + message);
});

// 给程序化验收用：driver 通过这个钩子驱动界面，而不是去模拟鼠标。
window.dhampir = {
  state: state,
  seekTo: seekTo,
  renderTimeline: renderTimeline,
  renderInspector: renderInspector,
  renderIssues: renderIssues,
  runExport: runExport,
  // 编辑操作也挂出来：验收驱动靠它把"点一次剃刀"变成可复算的一步。
  runEdit: runEdit,
  // 撤销/重做同理：驱动点不了按钮，但可以让页面自己走一遍同一条路。
  runHistoryStep: runHistoryStep,
  // 判定回传：页面自己把结果送出去，而不是让驱动钻进来取。
  reportVerdict: reportVerdict,
  // 音频：验收要能问"音轨接上了几路""现在是不是静音"。
  audioSources: audioSources,
  audioState: audioState,
  bindAudioSources: bindAudioSources,
  applyAudioVolume: applyAudioVolume,
  // 播放：验收要能问"现在在播吗"，也要能自己起停（驱动点不了按钮）。
  isPlaying: isPlaying,
  play: play,
  pause: pause,
  togglePlay: togglePlay,
  // 丢帧统计：验收要能读"这次播放丢了几帧"，而不是靠看画面猜。
  playbackStats: playbackStats,
  resetPlaybackStats: resetPlaybackStats,
  runTrimParity: runTrimParity,
  runTrimDragVerdict: runTrimDragVerdict,
  runSubtitleVerdict: runSubtitleVerdict,
  loadProjectSubtitles: loadProjectSubtitles,
  loadLibrary: loadLibrary,
  select: (trackIndex, layerIndex) => {
    state.selected = { trackIndex: trackIndex, layerIndex: layerIndex };
    renderInspector();
    return state.doc.timeline.tracks[trackIndex].layers[layerIndex];
  },
};
