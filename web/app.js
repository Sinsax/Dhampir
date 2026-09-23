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
  only.dataset.ready = "1";
  canvas.dataset.ready = "1";
  window.dhampirReady = true;
  mark("启动完成");

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
  select: (trackIndex, layerIndex) => {
    state.selected = { trackIndex: trackIndex, layerIndex: layerIndex };
    renderInspector();
    return state.doc.timeline.tracks[trackIndex].layers[layerIndex];
  },
};
