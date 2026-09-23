#!/usr/bin/env node
// 「预览与成片用同一个坐标系」的结构守卫。
//
// # 它管什么、不管什么
//
// **语义**（transform.x 换成目标像素之后，归一化落点与目标尺寸无关）已经由
// dhampir-core 的单元测试钉住（render/compose.rs 的「位移的归一化落点与目标尺寸无关」，
// 外加一条反向用例「不换算文档像素时归一化落点会随目标尺寸变」）。
// 那部分是数学，用 Rust 测才测得准。
//
// 这个守卫管的是**另一半**：宿主有没有把文档坐标系**接上**。
// 光有 core 的算术、宿主却传一个等于目标尺寸的坐标系，功能等于没做 ——
// 而那种错不会让任何测试变红，只会让「预览所见 != 成片所得」悄悄回来。
//
// # 判据
//
//   1. core 里缩放真的在算（pixel_scale 与 space.offset 都在）；
//   2. 契约里有一个**读工程**拿到的文档坐标系（ProjectDoc::sequence_size）；
//   3. 出片那条路把**工程**的坐标系交给渲染器（不是导出尺寸）；
//   4. 预览那条路把**工程**的坐标系交给渲染器（不是画布尺寸）；
//   5. **不许把裸元组当坐标系传**（那正是修复前的写法，编译器现在也拦得住，
//      但这里再钉一次：将来有人加个默认参数就绕过去了）；
//   6. RenderSpace::square 的每一处都要在下面的允许清单里 ——
//      它是「这条路上没有工程」的显式声明，新增一处就该有人说明理由。
//
// 用法：
//   node scripts/check-preview-parity.mjs
//   node scripts/check-preview-parity.mjs --self-test

import { existsSync, readFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

export const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');

/** 允许用 RenderSpace::square 的文件：这些路径上**没有工程**，目标尺寸就是它自己的坐标系。 */
export const SQUARE_ALLOWED = [
  { path: 'crates/dhampir-core/src/render/compose.rs', why: '它自己的单元测试（相同尺寸时比例是 1）' },
  { path: 'crates/dhampir-core/src/render/overlay.rs', why: '文字叠加：行位图的落点已经是目标像素（place_line(rect, target) 的产物），这条路上没有文档坐标系可换算' },
  { path: 'crates/dhampir-worker/examples/decode_sequence.rs', why: '顺序解码的例子，输入是裸帧序列而不是工程' },
  { path: 'crates/dhampir-worker/examples/render_project.rs', why: '渲染例子直接渲契约，没有工程壳' },
  { path: 'crates/dhampir-worker/tests/compose.rs', why: '合成器测试，源是现造的颜色块' },
  { path: 'crates/dhampir-worker/tests/timeline.rs', why: '时间线测试，源是合成纹理' },
  { path: 'crates/dhampir-wasm/src/timeline_host.rs', why: '探针/语料两条路（render_probe 与 sample_project），不读工程壳' },
];

/** 要读的文件。缺了任何一个都判红 —— 守卫找不到证据时不该通过。 */
export const REQUIRED_FILES = [
  'crates/dhampir-core/src/render/compose.rs',
  'crates/dhampir-core/src/render/timeline.rs',
  'crates/dhampir-timeline/src/project.rs',
  'crates/dhampir-worker/src/pipeline.rs',
  'crates/dhampir-worker/src/bin/dhampir.rs',
  'crates/dhampir-wasm/src/timeline_host.rs',
];

/** 调用渲染器的行 —— 这些地方必须出现坐标系。 */
const CALL_RE = /render_frame\(|\.compose\(/;
/** 裸元组当参数：以括号开头、整行就是一个元组。 */
const BARE_TUPLE_RE = /^\s*\([^()]*,[^()]*\),?\s*$/;

/**
 * 判定。入参是 { <相对路径>: 文本 }。
 * 返回问题清单（空 = 通过）。**纯函数**：自检与真跑走同一段代码。
 */
export function plumbingProblems(files) {
  const problems = [];
  const text = (path) => {
    const value = files[path];
    if (value === undefined) {
      problems.push('缺少文件：' + path + '（守卫找不到证据，不许通过）');
      return null;
    }
    return value;
  };

  const compose = text('crates/dhampir-core/src/render/compose.rs');
  if (compose !== null) {
    if (!compose.includes('pub fn pixel_scale')) {
      problems.push('core 的 RenderSpace 里没有 pixel_scale —— 比例没在算，坐标系只是个摆设');
    }
    if (!compose.includes('pub fn offset')) {
      problems.push('core 的 RenderSpace 里没有 offset —— 文档像素没有被换算成目标像素');
    }
    if (!compose.includes('space.offset(')) {
      problems.push('合成本身没有调用 RenderSpace::offset —— 换算写了但没人用');
    }
    if (!compose.includes('space.target')) {
      problems.push('合成本身没有用 space.target —— 目标尺寸可能还是从别处拿的');
    }
  }

  const project = text('crates/dhampir-timeline/src/project.rs');
  if (project !== null && !project.includes('pub fn sequence_size')) {
    problems.push('ProjectDoc 里没有 sequence_size —— 文档坐标系没有唯一的定义处');
  }

  // 出片那条路：坐标系必须来自工程，而不是导出尺寸。
  const pipeline = text('crates/dhampir-worker/src/pipeline.rs');
  if (pipeline !== null) {
    if (!pipeline.includes('pub sequence:')) {
      problems.push('RenderPlan 里没有 sequence —— 出片拿不到文档坐标系');
    }
    if (!pipeline.includes('sequence: plan.sequence')) {
      problems.push('出片调用没有把 plan.sequence 交给渲染器 —— 出片侧退回了「目标尺寸即坐标系」');
    }
  }
  const cli = text('crates/dhampir-worker/src/bin/dhampir.rs');
  if (cli !== null && !cli.includes('doc.sequence_size()')) {
    problems.push('CLI 没有用 doc.sequence_size() 填 RenderPlan.sequence —— 出片的坐标系没人给');
  }

  // 预览那条路：坐标系必须来自工程，而不是画布。
  const host = text('crates/dhampir-wasm/src/timeline_host.rs');
  if (host !== null && !host.includes('doc.sequence_size()')) {
    problems.push('预览宿主没有用 doc.sequence_size() —— 预览会退回「画布尺寸即坐标系」');
  }

  // 不许把裸元组当坐标系传。
  for (const path of Object.keys(files)) {
    const source = files[path];
    if (!path.endsWith('.rs')) continue;
    const lines = source.split('\n');
    for (let index = 0; index < lines.length; index += 1) {
      if (!CALL_RE.test(lines[index])) continue;
      for (let probe = index + 1; probe < Math.min(index + 12, lines.length); probe += 1) {
        if (BARE_TUPLE_RE.test(lines[probe])) {
          problems.push(path + ':' + (probe + 1) + ' 把裸元组当渲染空间传了 —— '
            + '坐标系的形参必须显式给（RenderSpace { sequence, target } 或 RenderSpace::square）');
        }
      }
    }
  }

  // square 的每一处都要在允许清单里，且清单要如实报数。
  const allowed = SQUARE_ALLOWED.map((entry) => entry.path);
  for (const path of Object.keys(files)) {
    if (!files[path].includes('RenderSpace::square')) continue;
    if (allowed.indexOf(path) < 0) {
      problems.push(path + ' 用了 RenderSpace::square，但它不在允许清单里 —— '
        + '要么改用工程的 sequence_size，要么在 scripts/check-preview-parity.mjs 的 SQUARE_ALLOWED 里写明理由');
    }
  }

  return problems;
}

function readSources() {
  const files = {};
  for (const path of REQUIRED_FILES) {
    const full = join(REPO_ROOT, path);
    files[path] = existsSync(full) ? readFileSync(full, 'utf8') : undefined;
    if (files[path] === undefined) delete files[path];
  }
  // 结构检查要扫全部 Rust 源码：新加一个调用点是这件事最容易出错的地方。
  for (const entry of SQUARE_ALLOWED) {
    const full = join(REPO_ROOT, entry.path);
    if (existsSync(full) && files[entry.path] === undefined) {
      files[entry.path] = readFileSync(full, 'utf8');
    }
  }
  return files;
}

function runSelfTest() {
  let passed = 0;
  const failures = [];
  const expect = (name, shouldPass, files) => {
    const problems = plumbingProblems(files);
    if ((problems.length === 0) === shouldPass) {
      passed += 1;
      return;
    }
    failures.push(name + ' -> ' + JSON.stringify(problems));
  };

  // 一份「正确」的合成源码集。
  const okCompose = 'pub fn pixel_scale(&self) {}\npub fn offset(&self) {}\nlet (row0, row1) = inverse_affine(d, size, space.target);\nlet o = space.offset(t);';
  const okPipeline = 'pub sequence: (u32, u32),\nRenderSpace { sequence: plan.sequence, target: (plan.width, plan.height) },';
  const base = () => ({
    'crates/dhampir-core/src/render/compose.rs': okCompose,
    'crates/dhampir-core/src/render/timeline.rs': 'nothing here',
    'crates/dhampir-timeline/src/project.rs': 'pub fn sequence_size(&self) {}',
    'crates/dhampir-worker/src/pipeline.rs': okPipeline,
    'crates/dhampir-worker/src/bin/dhampir.rs': 'sequence: doc.sequence_size(),',
    'crates/dhampir-wasm/src/timeline_host.rs': 'let s = doc.sequence_size();',
  });

  expect('正确的一组 -> 通过', true, base());
  // 反向：每一条都必须**因为那一条**变红。
  const missing = base(); delete missing['crates/dhampir-timeline/src/project.rs'];
  expect('缺文件 -> 必须红', false, missing);
  const noScale = base(); noScale['crates/dhampir-core/src/render/compose.rs'] = 'let o = space.offset(t);\nspace.target';
  expect('core 没有 pixel_scale -> 必须红', false, noScale);
  const unusedScale = base(); unusedScale['crates/dhampir-core/src/render/compose.rs'] = 'pub fn pixel_scale(&self) {}\npub fn offset(&self) {}\nspace.target';
  expect('算了比例却没人用 -> 必须红', false, unusedScale);
  const noSequence = base(); noSequence['crates/dhampir-timeline/src/project.rs'] = 'nothing';
  expect('契约没有 sequence_size -> 必须红', false, noSequence);
  const noPlanField = base(); noPlanField['crates/dhampir-worker/src/pipeline.rs'] = 'RenderSpace { sequence: plan.sequence, target: (plan.width, plan.height) },';
  expect('RenderPlan 没有 sequence -> 必须红', false, noPlanField);
  const noWorkerPass = base(); noWorkerPass['crates/dhampir-worker/src/pipeline.rs'] = 'pub sequence: (u32, u32),';
  expect('出片没把 plan.sequence 交下去 -> 必须红', false, noWorkerPass);
  const noCli = base(); noCli['crates/dhampir-worker/src/bin/dhampir.rs'] = 'sequence: (640, 360),';
  expect('CLI 没用 doc.sequence_size() -> 必须红', false, noCli);
  const noPreview = base(); noPreview['crates/dhampir-wasm/src/timeline_host.rs'] = 'let s = (640, 360);';
  expect('预览没用 doc.sequence_size() -> 必须红', false, noPreview);

  const bareTuple = base();
  bareTuple['crates/dhampir-worker/src/pipeline.rs'] = okPipeline + '\nrenderer.render_frame(\n    &d, &q, &mut e, &view,\n    (plan.width, plan.height),\n    &c, &mut r,\n);';
  expect('把裸元组当渲染空间传 -> 必须红', false, bareTuple);

  const straySquare = base();
  straySquare['crates/dhampir-worker/src/pipeline.rs'] = okPipeline + '\nlet s = RenderSpace::square((640, 360));';
  expect('新文件里冒出 square -> 必须红', false, straySquare);

  const allowedSquare = base();
  allowedSquare['crates/dhampir-worker/tests/compose.rs'] = 'let s = RenderSpace::square((SIZE, SIZE));';
  expect('允许清单里的 square -> 通过', true, allowedSquare);

  if (failures.length > 0) {
    console.error('先修守卫，别信它的结论：');
    for (const failure of failures) console.error('  - ' + failure);
    console.error('预览坐标系守卫自检失败（' + failures.length + ' 条）');
    process.exitCode = 2;
    return;
  }
  console.log('✓ 预览坐标系守卫自检通过（' + passed + ' 条断言）');
}

function main() {
  const args = process.argv.slice(2);
  if (args.indexOf('--self-test') >= 0) { runSelfTest(); return 0; }
  if (args.indexOf('--help') >= 0 || args.indexOf('-h') >= 0) {
    console.log('用法：node scripts/check-preview-parity.mjs [--self-test]');
    return 0;
  }
  if (args.length > 0) {
    console.error('  - 不认识的参数：' + args.join(' '));
    return 2;
  }
  const problems = plumbingProblems(readSources());
  if (problems.length > 0) {
    const shown = problems.slice(0, 6);
    for (const problem of shown) console.error('  - ' + problem);
    if (problems.length > shown.length) console.error('  - …另有 ' + (problems.length - shown.length) + ' 处');
    console.error('预览与成片的坐标系没有被接上');
    return 1;
  }
  console.log('✓ 预览与成片共用同一个文档坐标系（core 在换算、两个宿主都从工程取 sequence）');
  return 0;
}

process.exitCode = main();
