// 最小交互式剪辑界面。**没有框架**——就是 DOM 操作。
//
// 业务规则一概不在这里：校验问 Rust（engine.open），求值问 Rust（engine.evaluate），
// 渲染问 Rust（engine.seek）。这一层只负责"把工程画成能点的东西"和"把点击写回工程"。

import { loadEngine } from "/engine.js";
import { exportPngSequence } from "/export/png-sequence.js";
import { activeBackendFrom } from "/backend.js";
import { createHttpBackend } from "/export/http.js";

// 工程与素材**一律问 backend**，页面不许自己知道它们从哪来。
// 默认是降级模式（同一源下的固定 URL，不依赖服务）；
// 带 ?backend=local&port=... 就切到本机后端。
const backend = activeBackendFrom(location.search);
const FRAME_W = 640;
const FRAME_H = 360;

const state = { engine: null, project: null, selected: null, frame: 0, issues: [] };

const $ = (id) => document.getElementById(id);
const log = (message) => { $("issues").innerHTML = message; };

// --- 源：每个 source 一个 <video> -------------------------------------------------
// 同一帧上不同图层可能是不同源、不同源内帧，共用一个 <video> 做不到。
async function bindAllSources(engine, project, mediaUrl) {
  const sources = new Set();
  for (const track of project.tracks) for (const clip of track.clips) sources.add(clip.source);
  const host = $("videos");
  let index = 0;
  for (const source of sources) {
    const id = "src" + index;
    index += 1;
    const video = document.createElement("video");
    video.id = id;
    video.muted = true;
    video.playsInline = true;
    video.preload = "auto";
    video.src = mediaUrl;
    host.appendChild(video);
    await new Promise((resolve, reject) => {
      video.addEventListener("loadeddata", resolve, { once: true });
      video.addEventListener("error", () => reject(new Error("素材加载失败：" + source)), { once: true });
    });
    video.pause();
    engine.bindSource(source, id);
  }
}

// --- 时间线视图 -------------------------------------------------------------------
// **视图由工程算出来**：片段条的左边界与宽度都是 track_at / duration 的函数。
// 反过来"拖动条子改数据"要等视图稳定之后再接（v1 先用数字输入改）。
function renderTimeline() {
  const project = state.project;
  const end = Math.max(1, state.engine.endFrame());
  const host = $("timeline");
  host.textContent = "";
  project.tracks.forEach((track, trackIndex) => {
    const row = document.createElement("div");
    row.className = "track";
    const label = document.createElement("span");
    label.className = "track-label";
    label.textContent = track.id + " (" + track.kind + ")";
    row.appendChild(label);
    track.clips.forEach((clip, clipIndex) => {
      const bar = document.createElement("div");
      bar.className = "clip";
      if (state.selected !== null && state.selected.trackIndex === trackIndex && state.selected.clipIndex === clipIndex) {
        bar.className += " selected";
      }
      bar.style.left = (clip.track_at / end * 100) + "%";
      bar.style.width = Math.max(1.5, clip.duration / end * 100) + "%";
      bar.textContent = clip.id;
      if (clip.transition_in) {
        const overlay = document.createElement("div");
        overlay.className = "tr";
        overlay.style.left = "0";
        overlay.style.width = (clip.transition_in.duration / clip.duration * 100) + "%";
        bar.appendChild(overlay);
      }
      bar.addEventListener("click", () => {
        state.selected = { trackIndex, clipIndex };
        renderTimeline();
        renderInspector();
      });
      row.appendChild(bar);
    });
    host.appendChild(row);
  });
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

function renderInspector() {
  const host = $("props");
  host.textContent = "";
  if (state.selected === null) { host.textContent = "（未选中片段）"; return; }
  const { trackIndex, clipIndex } = state.selected;
  const clip = state.project.tracks[trackIndex].clips[clipIndex];
  const apply = (mutate) => {
    mutate(clip);
    // 每次改动都过一遍 Rust 校验：问题立刻显示，而不是等导出时才炸。
    const result = state.engine.validate(state.project);
    state.issues = result.issues || [];
    renderIssues();
    renderTimeline();
  };
  host.appendChild(numberField("起始帧 track_at", clip.track_at, (v) => apply((c) => { c.track_at = v; })));
  host.appendChild(numberField("时长 duration", clip.duration, (v) => apply((c) => { c.duration = v; })));
  host.appendChild(numberField("源内起始 source_in", clip.source_in, (v) => apply((c) => { c.source_in = v; })));
  host.appendChild(numberField("不透明度", clip.opacity, (v) => apply((c) => { c.opacity = v; })));
  host.appendChild(numberField("缩放 scale", clip.transform.scale, (v) => apply((c) => { c.transform.scale = v; })));
  host.appendChild(numberField("平移 x", clip.transform.x, (v) => apply((c) => { c.transform.x = v; })));
  host.appendChild(numberField("旋转 度", clip.transform.rotation_deg, (v) => apply((c) => { c.transform.rotation_deg = v; })));

  const blurRow = document.createElement("label");
  const blurSpan = document.createElement("span");
  blurSpan.textContent = "模糊半径";
  const blurInput = document.createElement("input");
  blurInput.type = "number";
  const existing = clip.effects.find((effect) => effect.kind === "gaussian_blur");
  blurInput.value = String(existing === undefined ? 0 : existing.params.radius);
  blurInput.addEventListener("change", () => apply((c) => {
    // 半径 0 就**移除**这个特效，而不是留个空壳：空壳会进图层清单，让"这一帧有几层"不可信。
    c.effects = c.effects.filter((effect) => effect.kind !== "gaussian_blur");
    const radius = Number(blurInput.value);
    if (radius > 0) c.effects.push({ kind: "gaussian_blur", params: { radius } });
  }));
  blurRow.appendChild(blurSpan);
  blurRow.appendChild(blurInput);
  host.appendChild(blurRow);

  const keyRow = document.createElement("label");
  const keySpan = document.createElement("span");
  keySpan.textContent = "不透明度关键帧";
  const keyButton = document.createElement("button");
  keyButton.textContent = clip.keyframes.length === 0 ? "加一对" : "清掉";
  keyButton.addEventListener("click", () => apply((c) => {
    c.keyframes = c.keyframes.length === 0
      ? [{ frame: 0, value: 0, easing: "linear" }, { frame: Math.max(1, c.duration - 1), value: 1, easing: "ease_in_out" }]
      : [];
  }));
  keyRow.appendChild(keySpan);
  keyRow.appendChild(keyButton);
  host.appendChild(keyRow);
}

// --- 问题清单 ---------------------------------------------------------------------
// 直接渲染 Rust 给的结构化 Issue：code + path + message 三段都在，**不在这里翻译**。
function renderIssues() {
  const host = $("issues");
  if (state.issues.length === 0) {
    host.innerHTML = "<span class=\"ok\">✓ 工程通过校验</span>";
    return;
  }
  host.innerHTML = state.issues.map(function (issue) {
    return "<div class=\"bad\">[" + issue.code + "] " + issue.path + " — " + issue.message + "</div>";
  }).join("");
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
// 两条路，**如实区分**：
//   png-sequence：逐帧渲染成 PNG，帧精确，交给 FFmpeg 编码（默认）；
//   http        ：把工程交给服务端出片（A 模式；本仓库只有 echo 假后端）。
// 提交前预检。返回 true 表示**已被拦住**，不该继续提交。
//
// 没有这一步，用户要等**分钟级任务跑完**才被告知"某一条对端不支持"。
// 而能力声明从哪来、规则是什么，都不在这个文件里：
// 前者问 backend，后者由 Rust 给出 —— 这里只负责把它们接上并显示。
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

async function precheckBeforeExport() {
  let capabilities = null;
  try {
    capabilities = await backend.capabilities();
  } catch (error) {
    // 拿不到能力声明**不是**拦截理由：降级模式本来就没有后端。
    // 但要让人看得见这件事，而不是静默跳过 —— 静默跳过会让"没预检"
    // 和"预检通过"看起来一模一样。
    $("issues").innerHTML =
      "<div>拿不到对端能力声明，本次跳过预检：" + String(error) + "</div>";
    await reportPrecheck("error", []);
    return false;
  }
  if (capabilities === null) {
    // 降级模式：对端不渲染也不出片，无从预检。
    await reportPrecheck("skipped", []);
    return false;
  }
  const issues = state.engine.precheck(capabilities);
  if (issues.length === 0) {
    $("issues").innerHTML = "<div class=\"ok\">提交前预检通过</div>";
    await reportPrecheck("passed", []);
    return false;
  }
  const lines = issues.map(function (issue) {
    return "<div class=\"bad\">[" + issue.code + "] " + issue.path + " — " + issue.message + "</div>";
  }).join("");
  $("issues").innerHTML =
    "<div class=\"bad\">提交前预检拦下了这次导出（对端做不了这些）：</div>" + lines;
  await reportPrecheck("blocked", issues);
  return true;
}

async function runExport() {
  const only = document.getElementById("frameOnly");
  const from = Number(only.dataset.from);
  const to = Number(only.dataset.to);

  // **提交前**预检：把对端做不了的东西现在就指出来，而不是等任务跑完。
  if (await precheckBeforeExport()) return;
  try {
    log("逐帧渲染 " + from + ".." + to + " …");
    await exportPngSequence(state.engine, state.project, { from, to }, async (frame, bytes, total) => {
      await fetch("/frame-png?frame=" + frame, { method: "POST", body: bytes });
      if ((frame - from) % 10 === 0) log("已渲染 " + (frame - from + 1) + "/" + total + " 帧");
    });
    log("<span class=\"ok\">帧序列已交给驱动，等 FFmpeg 编码。</span>");
    await fetch("/export-done", { method: "POST", body: JSON.stringify({ from, to }) });
  } catch (error) {
    log("<span class=\"bad\">导出失败：" + String(error && error.message ? error.message : error) + "</span>");
    await fetch("/export-failed", { method: "POST", body: String(error && error.message ? error.message : error) });
  }
}

// **启动里程碑回报。**
//
// 为什么需要它：--local 卡住的那几轮，页面**不抛错也不进展** ——
// 没有栈、没有消息，外部只能看到「什么都没发生」。
// 卡住比失败难查，所以要在每一步留一个脚印，把「卡在哪」变成可观测的事实。
function mark(name) {
  try {
    navigator.sendBeacon("/page-error", "里程碑:" + name);
  } catch (error) {
    // 观测手段失败不能影响启动。
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
  if (opened.ok !== true) { log("<span class=\"bad\">样本工程没通过校验</span>"); return; }
  // 复制一份给 UI 改：engine 里那份是"最后一份通过校验的"，两者职责不同。
  state.project = JSON.parse(JSON.stringify(engine.project));

  const canvas = $("preview");
  await engine.attach("preview");
  mark("已上屏到 canvas");
  await bindAllSources(engine, state.project, await backend.mediaUrlFor("a.mp4"));
  mark("视频源已绑定");

  const end = engine.endFrame();
  $("frame").max = String(Math.max(0, end - 1));
  const only = document.getElementById("frameOnly");
  only.dataset.from = "0";
  only.dataset.to = String(Math.max(0, end - 1));
  renderTimeline();
  renderInspector();
  renderIssues();
  await seekTo(0);
  mark("首帧已上屏");

  $("first").addEventListener("click", () => seekTo(0));
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
  // 这是"给测试用的入口"，不是产品功能——所以只在显式带上参数时才生效。
  if (new URLSearchParams(location.search).get("export") === "1") {
    await runExport();
  }
}

main().catch((error) => {
  log("<span class=\"bad\">启动失败：" + String(error && error.message ? error.message : error) + "</span>");
});

// 给程序化验收用：driver 通过这个钩子驱动界面，而不是去模拟鼠标。
window.dhampir = {
  state,
  seekTo,
  renderTimeline,
  renderInspector,
  renderIssues,
  select: (trackIndex, clipIndex) => {
    state.selected = { trackIndex, clipIndex };
    renderInspector();
    return state.project.tracks[trackIndex].clips[clipIndex];
  },
};
