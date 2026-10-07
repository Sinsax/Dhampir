#!/usr/bin/env node
// 「**现在还需要人做的事**」清单 —— 从我这边跑不动的判据里**生成**出来。
//
//   node scripts/web-parity-pending.mjs
//
// # 为什么要"生成"而不是写死
//
// 写死的清单一定会过期（本会话反复吃过：陈旧断言、引用不存在的用例、说假话的条目）。
// 所以这里不手写：**未实测的那几条直接从台账 `scripts/dom-parity-differences.toml` 读**，
// 台账改了、这里就跟着变 —— 结构上不可能与台账不一致。
//
// 清单分三段：① 台账要求你量的；② 环境判据（我这边跑不动、你那边一条命令的事）；
// ③ 等你拍板的两件（不是量测问题，是取舍）。

import { readFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');

/** 台账的极简解析：只认 `[节]` / `status = "…"` / `method = """…"""` / `note = "…"`。 */
export function parseLedger(text) {
  const out = [];
  let current = null;
  const lines = text.split('\n');
  let inMethod = false;
  let buffer = [];
  for (const line of lines) {
    const section = line.match(/^\[([^\]]+)\]/);
    if (section !== null && !inMethod) {
      current = { name: section[1], status: '', method: '', note: '' };
      out.push(current);
      continue;
    }
    if (current === null) continue;
    if (inMethod) {
      if (line.trim() === '"""') {
        inMethod = false;
        current.method = buffer.join(' ').replace(/\s+/g, ' ').trim();
        buffer = [];
      } else {
        buffer.push(line.trim());
      }
      continue;
    }
    // `method = """…"""` 两种写法都要认：**跨行**与**单行**。
    // （第 66 轮自查：我自己刚因为"只认一种措辞"把一名守卫误判成没有自检 ✗ ——
    //   同一个坑不该在后面的解析器里再踩一次。）
    const singleLineMethod = line.match(/^method = """(.*)"""$/);
    if (singleLineMethod !== null) {
      current.method = singleLineMethod[1].replace(/\s+/g, ' ').trim();
      continue;
    }
    if (/^method = """/.test(line)) {
      inMethod = true;
      continue;
    }
    const key = line.match(/^(status|method|note) = "(.*)"$/);
    if (key !== null) current[key[1]] = key[2];
  }
  return out;
}

/** 自检：拿**合成台账**把解析的边界钉住（不依赖真台账当前长什么样）。 */
function selfTest() {
  const problems = [];
  const sample = [
    '# 注释不算条目',
    '[filter.some.thing]',
    'status = "未实测"',
    'method = """',
    '跨行量法第一句。',
    '第二句。',
    '"""',
    'note = "带「引号」的说明"',
    '',
    '[other.one]',
    'status = "已实测"',
    'method = """单行量法"""',
    'max_abs_diff_max = 3',
    '',
    '[accepted.one]',
    'status = "明确接受"',
    'note = "理由"',
  ].join('\n');
  const parsed = parseLedger(sample);
  if (parsed.length !== 3) problems.push('应当解析出 3 条，得到 ' + parsed.length);
  if (parsed[0] === undefined || parsed[0].method !== '跨行量法第一句。 第二句。') {
    problems.push('跨行 method 没拼对：' + JSON.stringify(parsed[0] === undefined ? null : parsed[0].method));
  }
  if (parsed[1] === undefined || parsed[1].method !== '单行量法') {
    problems.push('单行 method 没认出来：' + JSON.stringify(parsed[1] === undefined ? null : parsed[1].method));
  }
  const pending = parsed.filter((entry) => entry.status === '未实测');
  if (pending.length !== 1 || pending[0].name !== 'filter.some.thing') {
    problems.push('未实测的筛法不对：' + JSON.stringify(pending.map((entry) => entry.name)));
  }
  return problems;
}

if (process.argv.includes('--self-test')) {
  const problems = selfTest();
  if (problems.length > 0) {
    for (const problem of problems) console.error('  - ' + problem);
    console.error('✗ 待办清单生成器的自检不过');
    process.exit(1);
  }
  console.log('✓ 待办清单生成器的自检通过（跨行/单行 method、引号、三档筛选）');
  process.exit(0);
}
/** 每条「未实测」的**操作步骤**（键在节名上）。
 *
 * 为什么要有它：台账的 `method` 说的是"量什么"，而人上手要的是"点哪里"。
 * 键在节名上 ⇒ 台账里改了节名，这里就会**报出来**（不会静默漏一条待办）。
 */
const STEPS = {
  'geometry.edge_antialias': [
    '① 引擎出图：cargo run -q -p dhampir-worker --bin dhampir -- frame --project target/corner/radius.doc.json --frame 15 --out target/aa/engine',
    '② 浏览器打开 web/dom-host.html，载入同一个工程，把画布缩放到与那张 PNG **完全同尺寸**后截图（截整个画布）',
    '③ 比：node scripts/dhampir-framediff.mjs target/aa/engine/frame-0015.png <你的截图.png>',
    '④ 把"边缘像素占比"与"这些像素上的最大通道差"填进台账这一节的 `edge_max_diff_max`，status 改 `已实测`',
    '注意：**只比边缘那一圈**（整帧均值会把这圈差异稀释成看起来没事，而肉眼恰恰在边缘上）',
  ],
  'blend.add_plus_lighter': [
    '① 造一个用了 `add` 混合的工程，**上面那层的不透明度设成 0.5**（这是两者分叉的地方），引擎出图',
    '② DOM 宿主同尺寸截图',
    '③ 比**亮部**的最大通道差（加法在亮部最容易分叉）',
    '预期：差**不为零** —— 两边不是同一个函数（本仓 rgb 不看源 alpha；CSS 是 αs·Cs + Cb）',
    '若你看到差为零，先查上面那层的不透明度是不是 1.0（那种情形两边本来就相同）',
  ],
  'mask.interpolation_kernel': [
    '① 造一个**掩码图能从 uri 直接取到**（http / blob / data）的工程 —— 相对文件名不行，那种 DOM 侧刻意不设（见 mask.availability）',
    '② 把掩码图缩放到**非整数倍**（让纹素边界落在像素中心之间），引擎出图',
    '③ DOM 宿主同尺寸截图，比**掩码内部**那一圈（不是边缘）',
    '预期：两边都滤波、核不同 ⇒ 会差；差多少由这一量定',
  ],
};
const ledger = parseLedger(readFileSync(join(REPO_ROOT, 'scripts', 'dom-parity-differences.toml'), 'utf8'));
const pending = ledger.filter((entry) => entry.status === '未实测');

console.log('一、台账要求你量的（' + pending.length + ' 条，全部需要浏览器）');
console.log('');
for (const entry of pending) {
  console.log('  [' + entry.name + ']');
  console.log('    量法：' + entry.method);
  const steps = STEPS[entry.name];
  if (steps === undefined) {
    console.log('    ⚠ 这条**还没有操作步骤** —— 补进本脚本的 STEPS（别让它静默漏掉）');
  } else {
    console.log('    怎么做：');
    for (const step of steps) console.log('      ' + step);
  }
  if (entry.note !== '') console.log('    备注：' + entry.note);
  console.log('');
}
const orphanSteps = Object.keys(STEPS).filter((name) => !ledger.some((entry) => entry.name === name));
if (orphanSteps.length > 0) {
  console.log('  ⚠ 这些操作步骤在台账里已经没有对应条目了（节名改了？）：' + orphanSteps.join(' / '));
  console.log('');
}

console.log('二、环境判据（我这边跑不动，你那边一条命令）');
console.log('');
console.log('  1. wasm 重建（会让 check-web-invariants 转绿）');
console.log('     cd crates/dhampir-wasm && wasm-pack build --dev --target web --out-dir www/pkg');
console.log('  2. 管道类判据（timeline-contract / check-local-backend / check-cli）：在普通终端里直接跑');
console.log('     node scripts/run-guards.mjs     ← 一次跑全 26 条');
console.log('  3. 双端那条（check-dual-end）：要先有第 1 步的 wasm pkg，再让它跑');
console.log('  4. 浏览器逐值缓动对照（19 条，含 3 条 linear()）');
console.log('     node scripts/easing-reference.mjs && cargo test -p dhampir-timeline --lib -- --ignored');
console.log('  5. 打开 web/dom-host.html 看画面（能画的都该画出来；不能画的应在页面上报出来）');
console.log('');
console.log('三、等你拍板的两件（不是量测问题，是取舍）');
console.log('');
  console.log('  A.（已不必你拍板，第 70 轮）同向重播的跳变 / 非整数相位：保持"明说不支持"。');
  console.log('     代价已算清（badge 4→37 键 = +825%；2 秒 5 通道约 +52 KB），三个选项与"何时翻案"写在 criteria 的 D7。');
console.log('  B. crates/dhampir-worker 里那 2 条既有红（asset.uri 的书写形态判定）修不修？');
console.log('     （要修得同时改 scripts/dhampir-local.mjs 的 isAbsoluteUri）');
console.log('');
console.log('跑完第一、二段之后，5 条环境红应当全部转绿；台账那 ' + pending.length + ' 条可以据此从"未实测"改成"已实测"。');
