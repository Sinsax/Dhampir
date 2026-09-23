#!/usr/bin/env node
// 「两个宿主真的把文字叠上去了」的结构守卫。
//
// # 它管什么、不管什么
//
// **语义**（这一帧有哪几行、各占哪个归一化矩形、落在哪几个目标像素）由
// `dhampir-core::overlay::evaluate_overlay` 与 `dhampir-timeline::text_layout::place_line`
// 两份纯函数钉住，各自带单测。**像素**由 T2.4 / T2.5 的判定通道钉住
// （本机 `cargo test --test overlay`、浏览器 `--verdict subtitle`，见 plan/t2-evidence.md）。
//
// 这个守卫管的是**接线**：宿主有没有**两个都做** —— 评估（evaluate_overlay）与画上去
// （worker 的 painter.paint / 预览宿主的 compose_overlay）。
//
// 只做一半是这条路最难发现的失效模式，两种都**不会让任何测试变红**：
//
//   * **没评估**：这一帧的字根本不出现。画面看起来完全正常 —— 像"这个工程没有字幕"；
//   * **评估了没画**：结构全对、画面空。看起来像"字体没渲染出来"。
//
// 两者都得靠结构判据：文案比对（两端都比对了"结构"）在这两种情况下都会通过。
//
// # 判据
//
//   1. **评估层只有一份**：`pub fn evaluate_overlay` 只在 core 里，别处不许再有一份实现；
//   2. **通用律（这条就是 T2.6 那句话）**：渲染了帧、又读了时间线的文件必须评估 overlay。
//      今天命中两个宿主，所以**命中数少于两个就算红** —— 改字段名让规则失效，
//      比漏掉一个宿主更隐蔽：它看起来是绿的；
//   3. **出片宿主**（worker/pipeline.rs）：出片与逐帧 PNG 两条渲染路径各评估一次、各画一次，
//      且"评估"后面紧跟着"画"（评估了不画 = 结构对、画面空）；
//   4. **预览宿主**（wasm/timeline_host.rs）：清单从 evaluate_overlay 来（不许自己算）、
//      落点从 place_line 来、贴上去有**两条路**（预览 draw 与判定 text_probe，少一条就
//      要么看不见、要么判不了）、每一次外部位图拷贝都声明直排 alpha；
//   5. **判定入口不许重算清单**：那会把刚提交的行位图全作废（症状是"一行都画不出来"）；
//   6. **CLI 的 subtitle 是两个宿主对照的基准**，它自己也得走评估层 ——
//      基准自己再算一份，"对照"就变成两份实现互相确认；
//   7. **JS 一侧**：用 rasterizeLine 造的字形位图必须写 `premultiplyAlpha: "none"`
//      （说错不报错，只会让字的边缘发暗，看着像"字体没渲染好"）；
//      页面判定路径从 `engine.textManifest` 读清单，**不许**调 textFrame（理由同 5）。
//
// 用法：
//   node scripts/check-overlay-plumbing.mjs
//   node scripts/check-overlay-plumbing.mjs --self-test

import { existsSync, readFileSync, readdirSync } from 'node:fs';
import { dirname, join, relative, resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';

export const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');

const CORE_OVERLAY = 'crates/dhampir-core/src/overlay.rs';
const PIPELINE = 'crates/dhampir-worker/src/pipeline.rs';
const CLI = 'crates/dhampir-worker/src/bin/dhampir.rs';
const WASM_HOST = 'crates/dhampir-wasm/src/timeline_host.rs';
const ENGINE = 'web/engine.js';
const APP = 'web/app.js';

/** 要读的文件。缺了任何一个都判红 —— 守卫找不到证据时不该通过。 */
export const REQUIRED_FILES = [CORE_OVERLAY, PIPELINE, CLI, WASM_HOST, ENGINE, APP];

/** 这两个文件必须被「渲染 + 读时间线」命中：它们是 T2 接上线的两个宿主。 */
export const HOSTS = [PIPELINE, WASM_HOST];

/** 遍历时跳过的目录名（生成的、依赖的、外部的）。 */
const SKIP_DIRS = new Set(['target', 'node_modules', '.git', 'pkg', 'www', 'dist', '.tools']);

const RENDER_RE = /render_frame\(/;
const TIMELINE_RE = /\.timeline\b/;
const EVALUATE_RE = /evaluate_overlay\(/;
const PAINT_RE = /painter\.paint\(/;
const COMPOSE_RE = /compose_overlay\(/;
const PLACE_RE = /place_line\(/;
const COPY_EXTERNAL_RE = /copy_external_image_to_texture\(/;
const PREMULTIPLIED_RE = /premultiplied_alpha:\s*false/;
const CREATE_BITMAP_RE = /createImageBitmap\(/;
const RASTER_RE = /rasterizeLine\(/;
const PREMULTIPLY_ANY_RE = /premultiplyAlpha\s*:/;
const PREMULTIPLY_NONE_RE = /premultiplyAlpha:\s*"none"/;
/** 清单重算的**调用**形式（字段访问 `host.text_lines` 不算）。 */
const TEXT_LINES_CALL_RE = /text_lines\(/;
const TEXT_FRAME_CALL_RE = /\.textFrame\(/;
const PROBE_FN_RE = 'pub async fn dhampir_project_text_probe';
const EVALUATE_DEF_RE = /fn evaluate_overlay/;

/** 行号（1 起）列表：哪些行匹配。正则不带 g，避免 lastIndex 残留。 */
function indicesOf(lines, re) {
  const found = [];
  for (let index = 0; index < lines.length; index += 1) {
    if (re.test(lines[index])) found.push(index);
  }
  return found;
}

/** 从 `start` 这一行起、往后看 `window` 行（含自己）里有没有匹配。 */
function hasWithin(lines, start, window, re) {
  const end = Math.min(start + window, lines.length);
  for (let index = start; index < end; index += 1) {
    if (re.test(lines[index])) return true;
  }
  return false;
}

/**
 * 判定。入参是 { <相对路径>: 文本 }（Rust 源码 + 两个 JS 文件）。
 * 返回问题清单（空 = 通过）。**纯函数**：自检与真跑走同一段代码。
 */
export function overlayProblems(files) {
  const problems = [];
  const text = (path) => {
    const value = files[path];
    if (value === undefined) {
      problems.push('缺少文件：' + path + '（守卫找不到证据，不许通过）');
      return null;
    }
    return value;
  };

  const rustPaths = Object.keys(files).filter((path) => path.endsWith('.rs'));
  if (rustPaths.length === 0) {
    problems.push('一个 .rs 都没扫到 —— 拒绝在空集合上通过（工作目录不对？）');
    return problems;
  }

  // 1. 评估层只有一份。
  const core = text(CORE_OVERLAY);
  if (core !== null && !core.includes('pub fn evaluate_overlay')) {
    problems.push('core 里没有 pub fn evaluate_overlay —— 评估层没有唯一的定义处');
  }
  for (const path of rustPaths) {
    if (path === CORE_OVERLAY) continue;
    if (EVALUATE_DEF_RE.test(files[path])) {
      problems.push(path + ' 里又定义了一份 evaluate_overlay —— 评估层只能有一份：'
        + '第二份与第一份分叉之后，两端各自都自洽，只有把两张画面摆在一起才看得出来');
    }
  }

  // 2. 通用律：渲染了帧、又读了时间线 -> 必须评估 overlay。
  const hits = [];
  for (const path of rustPaths) {
    const source = files[path];
    if (!RENDER_RE.test(source) || !TIMELINE_RE.test(source)) continue;
    hits.push(path);
    if (!EVALUATE_RE.test(source)) {
      problems.push(path + ' 渲染了帧、也读了时间线，却没有评估 overlay —— '
        + '这条路上画出来的帧永远不会带字，而画面看起来完全正常（像"这个工程没有字幕"）');
    }
  }
  for (const host of HOSTS) {
    if (hits.indexOf(host) < 0) {
      problems.push('「渲染 + 读时间线」没有命中 ' + host + ' —— 要么它不再读时间线了，'
        + '要么这条结构规则已经失效（比如字段改名了）。两种情况都要有人来说明，'
        + '不能让规则空转：空转的守卫看着是绿的');
    }
  }

  // 3. 出片宿主：两条渲染路径各评估一次、各画一次，且评估后面紧跟着画。
  const pipeline = text(PIPELINE);
  if (pipeline !== null) {
    const lines = pipeline.split('\n');
    const evaluates = indicesOf(lines, EVALUATE_RE);
    const paints = indicesOf(lines, PAINT_RE);
    if (evaluates.length < 2) {
      problems.push(PIPELINE + ' 只评估了 ' + evaluates.length + ' 次 overlay —— '
        + '出片与逐帧 PNG 是两条渲染路径，漏掉的那条会静默地不画字');
    }
    if (paints.length < 2) {
      problems.push(PIPELINE + ' 只画了 ' + paints.length + ' 次 overlay —— 同上：两条路各要画一次');
    }
    for (const index of evaluates) {
      if (!hasWithin(lines, index, 8, PAINT_RE)) {
        problems.push(PIPELINE + ':' + (index + 1) + ' 评估了 overlay 却没有紧跟着画上去 —— '
          + '结构对、画面空，看起来像"字体没渲染出来"');
      }
    }
  }

  // 4. 预览宿主：评估、落点、两条贴的路、直排 alpha。
  const host = text(WASM_HOST);
  if (host !== null) {
    const lines = host.split('\n');
    if (!EVALUATE_RE.test(host)) {
      problems.push(WASM_HOST + ' 没有评估 overlay —— 预览与判定两边都不会有字');
    }
    const composes = indicesOf(lines, COMPOSE_RE);
    if (composes.length < 2) {
      problems.push(WASM_HOST + ' 只在 ' + composes.length + ' 处把位图贴上去 —— '
        + '预览（draw）与判定（text_probe）各要一处：少的那条要么看不见、要么判不了');
    }
    if (!PLACE_RE.test(host)) {
      problems.push(WASM_HOST + ' 没有用 place_line 算落点 —— '
        + '落点必须来自共享几何，自己乘除出来的落点只有它自己同意');
    }
    const copies = indicesOf(lines, COPY_EXTERNAL_RE);
    if (copies.length === 0) {
      problems.push(WASM_HOST + ' 里一次外部位图拷贝都没有 —— 这条规则找不到证据，不许通过');
    }
    for (const index of copies) {
      // 20 行 = 同一个调用的范围（wgpu 那两个 struct 字面量本身就十几行）。
      if (!hasWithin(lines, index, 20, PREMULTIPLIED_RE)) {
        problems.push(WASM_HOST + ':' + (index + 1) + ' 拷外部位图时没有声明 premultiplied_alpha: false —— '
          + '说错不报错，只会让字的边缘发暗（看着像"字体没渲染好"）');
      }
    }
    // 5. 判定入口不许重算清单。
    const probeAt = lines.findIndex((line) => line.includes(PROBE_FN_RE));
    if (probeAt < 0) {
      problems.push('找不到判定入口 ' + PROBE_FN_RE + ' —— 结构规则失去了锚点，先修守卫');
    } else if (lines.slice(probeAt).some((line) => TEXT_LINES_CALL_RE.test(line))) {
      problems.push('判定入口（text_probe）里重算了行清单 —— 那会把刚提交的行位图全作废，'
        + 'probe 判的就成了"没有位图的清单"（症状：一行都画不出来）。清单读缓存的那一份');
    }
  }

  // 6. CLI 的 subtitle 是两端对照的基准。
  const cli = text(CLI);
  if (cli !== null && !EVALUATE_RE.test(cli)) {
    problems.push(CLI + ' 的 subtitle 子命令没有走评估层 —— 它是两个宿主对照的基准，'
      + '自己再算一份就等于两份实现互相确认');
  }

  // 7. JS 一侧：直排 alpha 与清单来源。
  const engine = text(ENGINE);
  if (engine !== null) {
    const lines = engine.split('\n');
    const bitmaps = indicesOf(lines, CREATE_BITMAP_RE);
    if (bitmaps.length === 0) {
      problems.push(ENGINE + ' 里没有 createImageBitmap —— 浏览器侧的栅格化找不到证据');
    }
    const rasterLines = bitmaps.filter((index) => RASTER_RE.test(lines[index]));
    if (rasterLines.length === 0) {
      problems.push(ENGINE + ' 里没有"用 rasterizeLine 造位图"的 createImageBitmap 调用点 —— '
        + '这条规则失去了锚点（改名了就把规则一起改，别删）');
    }
    for (const index of rasterLines) {
      if (!hasWithin(lines, index, 4, PREMULTIPLY_NONE_RE)) {
        problems.push(ENGINE + ':' + (index + 1) + ' 字形位图没有写 premultiplyAlpha: "none" —— '
          + '宿主是按直排 alpha 上传的（premultiplied_alpha: false），这边预乘之后字边缘会发暗');
      }
    }
    for (const index of indicesOf(lines, PREMULTIPLY_ANY_RE)) {
      if (!PREMULTIPLY_NONE_RE.test(lines[index])) {
        problems.push(ENGINE + ':' + (index + 1) + ' premultiplyAlpha 不是 "none" —— '
          + '说错不会报错，只会让字的边缘发暗');
      }
    }
  }

  const app = text(APP);
  if (app !== null) {
    if (!app.includes('textManifest')) {
      problems.push(APP + ' 没有读 textManifest —— 页面要用宿主刚算过的那一份清单');
    }
    if (TEXT_FRAME_CALL_RE.test(app)) {
      problems.push(APP + ' 调了 textFrame —— 判定路径重算清单会把刚提交的行位图全作废'
        + '（症状：一整帧的字都画不出来）。清单从 textManifest 读');
    }
  }

  return problems;
}

/** 收集 crates 下的所有 .rs（相对路径用 / 分隔，跨平台一致）。 */
function collectRust(dir, found) {
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const full = join(dir, entry.name);
    if (entry.isDirectory()) {
      if (SKIP_DIRS.has(entry.name)) continue;
      collectRust(full, found);
    } else if (entry.isFile() && entry.name.endsWith('.rs')) {
      found.push(full);
    }
  }
  return found;
}

function readSources() {
  const files = {};
  for (const absolute of collectRust(join(REPO_ROOT, 'crates'), [])) {
    const rel = relative(REPO_ROOT, absolute).split(sep).join('/');
    files[rel] = readFileSync(absolute, 'utf8');
  }
  for (const path of REQUIRED_FILES) {
    const absolute = join(REPO_ROOT, path);
    if (existsSync(absolute) && files[path] === undefined) {
      files[path] = readFileSync(absolute, 'utf8');
    }
  }
  return files;
}

function runSelfTest() {
  let passed = 0;
  const failures = [];
  const expect = (name, shouldPass, files, mustInclude) => {
    const problems = overlayProblems(files);
    const ok = (problems.length === 0) === shouldPass
      && (mustInclude === undefined
        || problems.some((problem) => problem.indexOf(mustInclude) >= 0));
    if (ok) {
      passed += 1;
      return;
    }
    failures.push(name + ' -> ' + JSON.stringify(problems));
  };

  // 一份「接线都对」的源码集：两条出片路径 + 预览/判定两条贴图路 + 两侧的 alpha 声明。
  const okCore = 'pub fn evaluate_overlay(\n    timeline: &Timeline,\n    frame: i64,\n) -> Option<TextOverlay> {}\n';
  const okPipeline = [
    'use dhampir_core::overlay::{SubtitleTable, evaluate_overlay};',
    'let mut painter = OverlayPainter::new(plan.font_file);',
    'let drawn = renderer.render_frame(',
    '    &ctx.device, &ctx.queue, &mut encoder, &target_view,',
    '    RenderSpace { sequence: plan.sequence, target: (plan.width, plan.height) },',
    '    &composite, &mut sources, wgpu::Color::TRANSPARENT,',
    ');',
    'let mut overlay_log = IssueLog::new();',
    'if let Some(overlay) =',
    '    evaluate_overlay(plan.timeline, frame, plan.sequence, Some(plan.subtitles))',
    '{',
    '    painter.paint(&mut image, &overlay, (plan.width, plan.height), &mut overlay_log);',
    '}',
    '// 第二条路：逐帧 PNG',
    'renderer.render_frame(&ctx.device, &ctx.queue, &mut encoder, &target_view, space, &composite, &mut sources, clear);',
    'if let Some(overlay) =',
    '    evaluate_overlay(plan.timeline, frame, plan.sequence, Some(plan.subtitles))',
    '{',
    '    painter.paint(&mut image, &overlay, (plan.width, plan.height), &mut overlay_log);',
    '}',
    '',
  ].join('\n');
  const okHost = [
    'use dhampir_core::overlay::{SubtitleTable, evaluate_overlay};',
    'use dhampir_core::render::{compose_overlay, ink_report};',
    'use dhampir_timeline::text_layout::place_line;',
    'fn text_lines(doc: &ProjectDoc, frame: i64, target: (u32, u32)) -> Vec<TextLineSpec> {',
    '    let Some(overlay) = evaluate_overlay(&doc.timeline, frame, doc.sequence_size(), Some(subtitles)) else {',
    '        return Vec::new();',
    '    };',
    '    let Some(placement) = place_line(item.rect, target) else { continue; };',
    '}',
    '    renderer.render_frame(&ctx.device, &ctx.queue, &mut encoder, &sink_view, space, &composite, &mut resolver, clear);',
    '    if *text_frame == Some(frame) && !text_lines.is_empty() {',
    '        compose_overlay(renderer.compositor(), &ctx.device, &ctx.queue, &mut encoder, &sink_view, (width, height), &items);',
    '    }',
    '#[wasm_bindgen]',
    'pub async fn dhampir_project_text_probe(frame: i32) -> Result<String, JsValue> {',
    '    upload_text_bitmaps(&host.ctx.device, &host.ctx.queue, format, &host.text_lines, &host.text_bitmaps);',
    '    queue.copy_external_image_to_texture(',
    '        &info,',
    '        wgpu::wgt::CopyExternalImageDestInfo {',
    '            texture: &texture,',
    '            premultiplied_alpha: false,',
    '        },',
    '        extent,',
    '    );',
    '    compose_overlay(renderer.compositor(), &ctx.device, &ctx.queue, &mut encoder, &view, (width, height), &items);',
    '}',
    '',
  ].join('\n');
  const okCli = 'let overlay = evaluate_overlay(&doc.timeline, frame, sequence, Some(&table));\n';
  const okEngine = [
    'bitmap = await createImageBitmap(rasterizeLine(line, manifest.color, manifest.outline), {',
    '  premultiplyAlpha: "none",',
    '});',
    '',
  ].join('\n');
  const okApp = 'const manifest = engine.textManifest;\n';
  const base = () => ({
    'crates/dhampir-core/src/lib.rs': 'pub mod overlay;\n',
    [CORE_OVERLAY]: okCore,
    [PIPELINE]: okPipeline,
    [CLI]: okCli,
    [WASM_HOST]: okHost,
    [ENGINE]: okEngine,
    [APP]: okApp,
  });

  expect('接线都对的源码集 -> 通过', true, base());

  const missing = base();
  delete missing[WASM_HOST];
  expect('缺文件 -> 必须红', false, missing, '缺少文件');

  const notPublic = base();
  notPublic[CORE_OVERLAY] = 'fn evaluate_overlay(timeline: &Timeline) {}\n';
  expect('core 没有 pub fn evaluate_overlay -> 必须红', false, notPublic, '没有 pub fn evaluate_overlay');

  const copycat = base();
  copycat['crates/dhampir-worker/src/copycat.rs'] = 'fn evaluate_overlay(x: i64) {}\n';
  expect('别处又定义一份评估层 -> 必须红', false, copycat, '又定义了一份 evaluate_overlay');

  const onePath = base();
  onePath[PIPELINE] = okPipeline.split('// 第二条路')[0];
  expect('出片宿主只评估一次 -> 必须红', false, onePath, '只评估了 1 次');

  const evaluatedNotPainted = base();
  evaluatedNotPainted[PIPELINE] = okPipeline.replace(
    '    painter.paint(&mut image, &overlay, (plan.width, plan.height), &mut overlay_log);\n}\n// 第二条路',
    '}\n// 第二条路',
  );
  expect('评估了却不画 -> 必须红', false, evaluatedNotPainted, '评估了 overlay 却没有紧跟着画上去');

  const oneBlit = base();
  oneBlit[WASM_HOST] = okHost.replace(
    '    compose_overlay(renderer.compositor(), &ctx.device, &ctx.queue, &mut encoder, &view, (width, height), &items);\n}',
    '}',
  );
  expect('预览宿主少一处贴图 -> 必须红', false, oneBlit, '只在 1 处把位图贴上去');

  const noPlace = base();
  noPlace[WASM_HOST] = okHost.replace('    let Some(placement) = place_line(item.rect, target) else { continue; };\n', '');
  expect('预览宿主自己推落点 -> 必须红', false, noPlace, '没有用 place_line');

  const noAlpha = base();
  noAlpha[WASM_HOST] = okHost.replace('            premultiplied_alpha: false,\n', '            premultiplied_alpha: true,\n');
  expect('拷位图没声明直排 alpha -> 必须红', false, noAlpha, '没有声明 premultiplied_alpha: false');

  const newHost = base();
  newHost['crates/dhampir-worker/src/newhost.rs'] = 'let c = compose::evaluate_v2_with_assets(&doc.timeline, frame, None);\nrenderer.render_frame(&device, &queue, &mut encoder, &view, space, &c, &mut resolver, clear);\n';
  expect('新宿主读时间线却不评估 overlay -> 必须红', false, newHost, '却没有评估 overlay');

  const renamed = base();
  renamed[PIPELINE] = okPipeline.split('plan.timeline').join('plan.clock');
  expect('规则自己失效（命中不到宿主）-> 必须红', false, renamed, '没有命中');

  const noPremultiply = base();
  noPremultiply[ENGINE] = 'bitmap = await createImageBitmap(rasterizeLine(line, color, outline));\n';
  expect('字形位图没写 premultiplyAlpha -> 必须红', false, noPremultiply, '没有写 premultiplyAlpha');

  const wrongPremultiply = base();
  wrongPremultiply[ENGINE] = 'bitmap = await createImageBitmap(rasterizeLine(line, color, outline), {\n  premultiplyAlpha: "premultiply",\n});\n';
  expect('premultiplyAlpha 写错值 -> 必须红', false, wrongPremultiply, '不是 "none"');

  const recompute = base();
  recompute[WASM_HOST] = okHost.replace(
    'pub async fn dhampir_project_text_probe(frame: i32) -> Result<String, JsValue> {\n',
    'pub async fn dhampir_project_text_probe(frame: i32) -> Result<String, JsValue> {\n'
      + '    let (overlay, lines) = text_lines(&doc, frame, &subtitles, target);\n',
  );
  expect('判定入口重算清单 -> 必须红', false, recompute, '重算了行清单');

  const noProbe = base();
  noProbe[WASM_HOST] = okHost.replace('pub async fn dhampir_project_text_probe', 'pub async fn dhampir_project_ink_check');
  expect('判定入口找不到 -> 必须红', false, noProbe, '找不到判定入口');

  const appReframes = base();
  appReframes[APP] = okApp + 'const frame = engine.textFrame(0);\n';
  expect('页面判定里重算清单 -> 必须红', false, appReframes, '调了 textFrame');

  const appNoManifest = base();
  appNoManifest[APP] = 'const lines = engine.lines;\n';
  expect('页面没有读 textManifest -> 必须红', false, appNoManifest, '没有读 textManifest');

  const empty = base();
  for (const path of Object.keys(empty)) if (path.endsWith('.rs')) delete empty[path];
  expect('一个 .rs 都没扫到 -> 必须红', false, empty, '拒绝在空集合上通过');

  if (failures.length > 0) {
    console.error('先修守卫，别信它的结论：');
    for (const failure of failures) console.error('  - ' + failure);
    console.error('文字叠加接线守卫自检失败（' + failures.length + ' 条）');
    process.exitCode = 2;
    return;
  }
  console.log('✓ 文字叠加接线守卫自检通过（' + passed + ' 条断言）');
}

function main() {
  const args = process.argv.slice(2);
  if (args.indexOf('--self-test') >= 0) {
    runSelfTest();
    return 0;
  }
  if (args.indexOf('--help') >= 0 || args.indexOf('-h') >= 0) {
    console.log('用法：node scripts/check-overlay-plumbing.mjs [--self-test]');
    return 0;
  }
  if (args.length > 0) {
    console.error('  - 不认识的参数：' + args.join(' '));
    return 2;
  }
  const files = readSources();
  const problems = overlayProblems(files);
  if (problems.length > 0) {
    const shown = problems.slice(0, 6);
    for (const problem of shown) console.error('  - ' + problem);
    if (problems.length > shown.length) console.error('  - …另有 ' + (problems.length - shown.length) + ' 处');
    console.error('宿主没有把文字叠加接下来');
    return 1;
  }
  const rust = Object.keys(files).filter((path) => path.endsWith('.rs')).length;
  console.log('✓ 文字叠加接上了：两个宿主都评估、都画（评估层只有一份，'
    + '直排 alpha 与判定路径的清单来源都在判据里）；扫了 ' + rust + ' 个 .rs + 2 个 JS');
  return 0;
}

process.exitCode = main();
