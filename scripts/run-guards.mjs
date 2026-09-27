#!/usr/bin/env node
// 守卫总目录（guard runner）：把**全部**守卫一次跑完，每条都先 `--self-test` 再正跑。
//
// # 为什么要有这个文件（这是一个交付缺口）
//
// 这份清单原来只活在 `target/t2/run-guards.cjs` —— 而 `target/` 是 gitignore 的。
// 于是"到底有哪些守卫"这件事**根本没交付**：新克隆的人只看到 `scripts/` 下一堆
// `check-*.mjs`，不知道哪些算数、哪条要带参数、总共该有几条。
// 清单不落仓库，守卫就会在无人察觉中少掉一条 —— 而"少一条"永远不会自己变红。
//
// # 两条纪律
//
//   * **清单是显式的，并且与实际文件对表**：`scripts/` 里多出一条没登记的守卫、
//     或者登记了却不存在，都判红，并且**拒绝继续跑**。凭空少一条守卫，
//     比一条守卫报红严重得多（"全绿"会变成一句不负责任的话）。
//   * **fail-closed**：起不来的（spawn 出错、拿不到退出码）算红，不算跳过 ——
//     "跳过"是守卫消失最常见的形式。
//
// # 关于本会话（agent 工作区）里的假红 —— 两类，都不是坏代码
//
// 在 WorkBuddy agent 会话里直接跑这份清单会有几条红。两类原因**都不是仓库的毛病**，
// 但表现完全不同，分清楚才能不白跑一趟。
//
// ## 甲类：这个环境**不给子进程开 stdin 管道**（挡的是能力，不是代码）
//
// 实测签名：**只有 stdin 管道创建不了**，stdout / stderr 管道一切正常。
//
//   node 侧：spawnSync 返回 status=null，error.code='EBUSY'
//   Rust 侧：os error 231（ERROR_PIPE_BUSY，"所有的管道范例都在使用中"）
//
// 最小复现（纯标准库、无依赖、与本仓代码无关；子进程只要一条立刻退出的命令就够）：
//
//   stdin=null   -> OK
//   stdin=piped  -> ERR os error 231
//   stdout=pipe  -> OK
//
// **已经修掉的一半**：不喂 stdin 的调用本来就不该要那条管道。于是有了
// `scripts/spawn-tool.mjs`（`stdin: 'ignore'`）。这不叫垫片 —— 垫片会让
// "真的起不了子进程"也变绿；这里是**如实声明这次调用不喂 stdin**，起不来照样 status=null、照样红。
//
// **仍然红的一半**：渲染那条路**必须**往编码器 stdin 里写帧
// （crates/dhampir-worker/src/pipeline.rs），那条管道在 JS 这层够不着。
// 表现是 `dhampir` 报 `os error 231`，于是：
//
//   * `check-cli.mjs`           -> 27 / 30（红的 3 条都要真出片）
//   * `check-local-backend.mjs` -> 出片 + 下载那几条红
//   * `check-dual-end.mjs`      -> **绿**（走逐帧 PNG 那条路，不碰编码器 stdin）
//
// ## 乙类：本机 CLI 给 fs.rmSync 套了**按 turn 计数**的安全删除（挡的是删除次数）
//
// 计数超过阈值就直接抛，而跑一遍守卫套件本身要删掉几百个临时路径：
//
//   Error: [safe-delete][SAFE_DELETE_BULK_CONFIRM_REQUIRED]
//          {"count":549,"threshold":50,"scope":"turn","targets":[...]}
//       at wrappedRmSync (node-safe-delete-shim.cjs)
//       at collect (check-cli.mjs:154)
//
// 于是**越靠后的守卫越容易被"清理"绊倒**，而报告上它长得像一条契约不成立 ——
// 这一次真的被它骗过：`check-cli` 从 `27 / 30` 变成直接崩在 `collect` 的第一行。
// 已经修掉：与判据无关的清理统一走 `scripts/safe-remove.mjs`（删不掉只警告，不动退出码）。
//
// ## 结论
//
// **对照（这是关键的一行）**：把这条环境限制拿掉之后（沙箱豁免获批的那一次），
// 同一份清单是 **18 / 18 条全绿**。所以"16 / 18"是**这个环境**的读数，不是仓库的读数 ——
// 两条红的根因只有一个，而且不在仓库里。
//
// 要拿**真实**数字，去**普通终端**（不经 agent 的进程包装）里跑。
// **不要**为此把垫片提交进仓库，也不要让它进任何守卫的默认路径 ——
// 那会把"真的起不了子进程"也一起变成绿的。
//
// 用法：
//   node scripts/run-guards.mjs                 跑全部
//   node scripts/run-guards.mjs --list          只打印清单与对表结果
//   node scripts/run-guards.mjs --self-test     只跑本文件的自检

import { spawnSync } from 'node:child_process';
import { readdirSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

export const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');

/** 名字长这样的一律算守卫。 */
export const GUARD_FILE_RE = /^check-.*\.mjs$/;

/** 名字不像守卫、但确实是守卫的两个（它们是契约检查，不是 check-* 命名）。 */
export const ALSO_GUARDS = ['api-surface.mjs', 'timeline-contract.mjs'];

/**
 * 红的那条要回印多少行。
 *
 * 绿的两行够了；**红的必须多印** —— 只留最后两行时，一次 Node 崩溃栈会被截成
 * 「…run_main…」加「Node.js v22」，恰好把最有用的那行吞掉。
 * 这不是假设：排查 T7 收口时就被截过一次，白跑了两趟。
 */
export const RED_TAIL_LINES = 14;

/**
 * 显式清单。**顺序有意义**：先跑快而广的静态检查，重的记录检查放后面，
 * 这样前几条报红时能早一点看见，不必等最后一条跑完。
 *
 * `args` 是**正跑**时要带的参数（自检一律只有 `--self-test`）。
 */
export const GUARDS = [
  { script: 'check-core-purity.mjs', args: [] },
  { script: 'check-dep-graph.mjs', args: [] },
  { script: 'check-text-hygiene.mjs', args: [] },
  { script: 'check-linux-portability.mjs', args: [] },
  { script: 'check-sequential-decode.mjs', args: [] },
  { script: 'check-web-invariants.mjs', args: [] },
  { script: 'check-backend-seam.mjs', args: [] },
  { script: 'check-defects.mjs', args: [] },
  { script: 'check-preview-parity.mjs', args: [] },
  { script: 'check-overlay-plumbing.mjs', args: [] },
  { script: 'check-m1-record.mjs', args: ['--record', 'records/m1'] },
  { script: 'check-m2-record.mjs', args: ['--record', 'records/m2'] },
  { script: 'check-media-status.mjs', args: [] },
  { script: 'check-effect-registry.mjs', args: [] },
  { script: 'check-vtrim-translator.mjs', args: [] },
  { script: 'check-local-backend.mjs', args: [] },
  // 同源代理那条路：它坏了不会有别的守卫变红（check-local-backend 直接打后端，
  // 不经过转发），而产品路径的最后一跳正是从它上面走的。
  { script: 'check-same-origin-proxy.mjs', args: [] },
  { script: 'api-surface.mjs', args: [] },
  { script: 'timeline-contract.mjs', args: [] },
  { script: 'check-cli.mjs', args: [] },
  { script: 'check-dual-end.mjs', args: [] },
];

/** `scripts/` 里**实际存在**的守卫文件名（已排序）。 */
export function presentGuards(dir = join(REPO_ROOT, 'scripts')) {
  return readdirSync(dir)
    .filter((name) => GUARD_FILE_RE.test(name) || ALSO_GUARDS.includes(name))
    .sort();
}

/**
 * 对表。纯函数，便于喂**故意坏的**输入验证它真的会红。
 *
 * 返回问题列表：空 = 清单与磁盘一致。
 */
export function inventoryProblems(present, registered) {
  const problems = [];
  const p = new Set(present);
  const r = new Set(registered);
  for (const name of present) {
    if (!r.has(name)) {
      problems.push('scripts/' + name + ' 存在但**没登记**在这份清单里 —— ' +
        '新加的守卫会被悄悄漏跑，请把它加进 GUARDS');
    }
  }
  for (const name of registered) {
    if (!p.has(name)) {
      problems.push('清单里登记了 ' + name + '，但 scripts/ 下**找不到它** —— ' +
        '守卫是不是被删掉/改名了？少一条守卫必须当场可见');
    }
  }
  return problems;
}

/** 起一条守卫。fail-closed：拿不到退出码一律当红。 */
export function runOne(script, args) {
  const result = spawnSync(process.execPath, [join(REPO_ROOT, 'scripts', script), ...args], {
    cwd: REPO_ROOT,
    encoding: 'utf8',
    maxBuffer: 1 << 28,
    // stdin 显式 ignore：本机 node 给子进程开 stdin 管道会 EBUSY（见文件头的说明）。
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  const text = (result.stdout || '') + (result.stderr || '');
  const lines = text.split(/\r?\n/).filter((line) => line.trim() !== '');
  const code = result.error ? -1 : result.status;
  return { code, lines, spawnError: result.error ? String(result.error.message) : null };
}

const SELF_TEST_CASES = [
  { name: '一致 -> 过', present: ['check-a.mjs', 'api-surface.mjs'], registered: ['api-surface.mjs', 'check-a.mjs'], expect: 0 },
  { name: '**多出没登记的守卫** -> 必须红', present: ['check-a.mjs', 'check-new.mjs'], registered: ['check-a.mjs'], expect: 1 },
  { name: '**登记了却不存在** -> 必须红', present: ['check-a.mjs'], registered: ['check-a.mjs', 'check-gone.mjs'], expect: 1 },
  { name: '两边都错 -> 两条都要报', present: ['check-new.mjs'], registered: ['check-gone.mjs'], expect: 2 },
];

export function selfTestFailures() {
  const failures = [];
  for (const testCase of SELF_TEST_CASES) {
    const got = inventoryProblems(testCase.present, testCase.registered).length;
    if (got !== testCase.expect) {
      failures.push('自检「' + testCase.name + '」期望 ' + testCase.expect + ' 条，实际 ' + got + ' 条');
    }
  }
  return failures;
}

function main() {
  const argv = process.argv.slice(2);
  if (argv.includes('--help')) {
    console.log('用法：node scripts/run-guards.mjs [--list] [--self-test]');
    return 0;
  }

  const failures = selfTestFailures();
  if (failures.length > 0) {
    console.error('守卫目录自检失败 —— 先修它，别信它的结论：');
    for (const failure of failures) console.error('  - ' + failure);
    return 2;
  }
  if (argv.includes('--self-test')) {
    console.log('✓ 守卫目录自检通过（' + SELF_TEST_CASES.length + ' 条用例）');
    return 0;
  }

  const present = presentGuards();
  const registered = GUARDS.map((guard) => guard.script);
  const problems = inventoryProblems(present, registered);
  if (problems.length > 0) {
    console.error('守卫清单与 scripts/ 对不上，**拒绝继续跑**：');
    for (const problem of problems) console.error('  - ' + problem);
    console.error('清单对不上时，跑出来的"全绿"少跑了守卫，不能当结论。');
    return 1;
  }

  if (argv.includes('--list')) {
    console.log('共 ' + GUARDS.length + ' 条守卫（与 scripts/ 对表一致）：');
    for (const guard of GUARDS) {
      console.log('  ' + guard.script + (guard.args.length > 0 ? ' ' + guard.args.join(' ') : ''));
    }
    return 0;
  }

  let bad = 0;
  const reds = [];
  for (const guard of GUARDS) {
    const selfTest = runOne(guard.script, ['--self-test']);
    const plain = runOne(guard.script, guard.args);
    const ok = selfTest.code === 0 && plain.code === 0;
    if (!ok) {
      bad += 1;
      reds.push(guard.script);
    }
    console.log((ok ? 'OK  ' : 'RED ') + guard.script + '  自检=' + selfTest.code + ' 正跑=' + plain.code +
      (selfTest.spawnError ? '  起不来：' + selfTest.spawnError : ''));
    // 绿的多印两行够了；**红的要多印**——只留最后两行时，一次 Node 崩溃栈会被截成
    // "…run_main…" 加 "Node.js v22"，恰恰把最有用的那行吞掉（这次排查就吃过这个亏）。
    const keep = ok ? 2 : RED_TAIL_LINES;
    for (const line of [...selfTest.lines, ...plain.lines].slice(-keep)) console.log('      ' + line);
  }

  console.log('');
  console.log(bad === 0
    ? GUARDS.length + ' / ' + GUARDS.length + ' 条守卫全绿'
    : (GUARDS.length - bad) + ' / ' + GUARDS.length + ' 条全绿，红的是：' + reds.join(' '));
  if (bad > 0) {
    console.log('红的这几条通常不是坏代码 —— 先按文件头那两类（甲：环境不给开 stdin 管道；' +
      '乙：安全删除按 turn 计数）逐条核过再下结论。');
  }
  return bad === 0 ? 0 : 1;
}

// 只设 process.exitCode，不调 process.exit()——见 scripts/wasm-test-node-exit-shim.cjs 里的说明：
// 在本机 Node/Windows 上真正执行的 process.exit() 可能撞 libuv 断言，退出码变成负数，
// 而退出码是这份报告唯一的结论。
process.exitCode = main();
