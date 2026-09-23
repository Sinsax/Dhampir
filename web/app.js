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
import { exportPngSequence } from "/export/png-sequence.js";
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
 * 验收判据（T2.5）：**画面上的字，两端必须落在同一个地方、看着同一份清单。**
 *
 * 页面只做两件事：
 *   1. seek 那一帧 —— 宿主算出清单（哪几行、各占哪个归一化矩形、各落在哪几个
 *      目标像素），这里把每一行栅格化（canvas -> createImageBitmap -> 交回宿主）；
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
  for (const frame of frames) {
    await state.engine.seek(frame);
    manifests.push(state.engine.textManifest);
    probes.push(await state.engine.textProbe(frame));
  }
  for (const manifest of manifests) unplaced += Number(manifest.unplaced_lines) || 0;
  const ok = subtitles.failed.length === 0 && probes.length === frames.length && unplaced === 0;
  // 理由里把三个数都写出来：只说"没成立"的话，看的人还得回去数一遍。
  const reason = ok ? "" : ("字幕判定没成立：" + subtitles.failed.length + " 路没登记上、"
    + unplaced + " 行算不出落点、拿到 " + probes.length + "/" + frames.length + " 份墨迹报告");
  return report(ok, reason, { subtitles: subtitles, frames: manifests, probes: probes });
}

/** 判定按**名字**选路。表在这里，规则在各判定函数里。 */
const VERDICTS = {
  "trim-parity": runTrimParity,
  subtitle: runSubtitleVerdict,
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
    if (declaredAsset(assetId) === null) {
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
      state.engine.bindSource(assetId, id);
      loaded.push(assetId);
    } catch (error) {
      notice("素材 " + assetId + " 取不到（" + video.src + "）—— 这一路不会被画出来。");
    }
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
    host.textContent = "（未连接后端 —— 加素材请用 dhampir import 或后端的 POST /assets）";
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
  if (state.library.assets.length === 0) host.textContent = "（库里什么都没有）";
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
  state.issues = [];
  state.warnings = [];
  renderTimeline();
  renderInspector();
  renderIssues();
  await seekTo(state.frame);
  log((label || "编辑") + "：" + result.summary);
  await loadLibrary();
  renderLibrary();
}

// --- 时间线视图 -------------------------------------------------------------------

function renderTimeline() {
  const host = $("timeline");
  host.textContent = "";
  const model = timeline();
  if (model === null) {
    host.textContent = "（还没有载入工程）";
    return;
  }
  const end = Math.max(1, state.engine.endFrame());
  model.tracks.forEach((track, trackIndex) => {
    const row = document.createElement("div");
    row.className = "track";
    const label = document.createElement("span");
    label.className = "track-label";
    label.textContent = track.id + " (" + track.kind + ")";
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
      row.appendChild(bar);
    });
    host.appendChild(row);
  });
  renderMarkers(model, end);
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
    host.textContent = "（未选中元素）";
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

// --- 播放头 -----------------------------------------------------------------------

async function seekTo(frame) {
  const end = state.engine.endFrame();
  state.frame = Math.max(0, Math.min(frame, Math.max(0, end - 1)));
  $("frame").value = String(state.frame);
  $("frameLabel").textContent = String(state.frame);
  await state.engine.seek(state.frame);
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

  $("first").addEventListener("click", () => seekTo(engine.firstFrame()));
  $("prev").addEventListener("click", () => seekTo(state.frame - 1));
  $("next").addEventListener("click", () => seekTo(state.frame + 1));
  $("last").addEventListener("click", () => seekTo(end - 1));
  $("frame").addEventListener("input", (event) => seekTo(Number(event.target.value)));
  $("export").addEventListener("click", runExport);
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
  // 判定回传：页面自己把结果送出去，而不是让驱动钻进来取。
  reportVerdict: reportVerdict,
  runTrimParity: runTrimParity,
  runSubtitleVerdict: runSubtitleVerdict,
  loadProjectSubtitles: loadProjectSubtitles,
  loadLibrary: loadLibrary,
  select: (trackIndex, layerIndex) => {
    state.selected = { trackIndex: trackIndex, layerIndex: layerIndex };
    renderInspector();
    return state.doc.timeline.tracks[trackIndex].layers[layerIndex];
  },
};
