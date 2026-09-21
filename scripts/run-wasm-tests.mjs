#!/usr/bin/env node
// 在 wasm32 运行时里跑 dhampir-wasm 的测试，并为里程碑留下一份可复核的记录。
//
// ---------------------------------------------------------------------------
// 为什么不直接用 `wasm-pack test --node`
//
// 因为在本机（Windows 10 + Node v25.5.0）上，那条命令会在**测试全部通过之后**
// 报失败，退出码 1：
//
//     running 4 tests
//     test result: ok. 4 passed; 0 failed
//
//     Assertion failed: !(handle->flags & UV_HANDLE_CLOSING), file src\win\async.c, line 76
//     Error: Node failed with exit_code -1073740791
//
// 所以这个脚本自己做那三件事：cargo 编译 → 逐个 wasm 交给 runner → 自己判结果。
// runner 的 stdout 直接接文件句柄（省得日志和我们的输出搅在一起）。
//
// ---------------------------------------------------------------------------
// 退出码崩溃的根因（实测排除过，别再重复走我走过的弯路）
//
// 第一版脚本以为"把 stdout 从管道换成文件句柄"就解决了。**那个结论是错的**——
// 当时只跑了一次，恰好没崩。第二版以为"把 process.exit 推迟一个 tick"解决了，
// **也是错的**——那是 6 次里恰好没崩。同一二进制各跑 12 次：
//
//   变体                           崩溃    退出 0
//   不给任何垫片                   10/12    2/12
//   推迟一个 tick 再真退           9/12     3/12
//   完全接管退出（改用 exitCode）   0/12    12/12
//   接管 + 嵌套 setImmediate 再真退  3/12     9/12
//
// 结论：**只要真的调用被强制执行的 process.exit()，就可能撞上 libuv 的
// UV_HANDLE_CLOSING 断言**；推迟几个 tick 只降低概率。让 node 自己把事件循环
// 跑干才干净。见 `scripts/wasm-test-node-exit-shim.cjs`。
//
// 代价：接管退出后，若测试壳留下活句柄，node 会**挂住**而不是退出。所以下面给
// runner 设了超时——"挂住"绝不能伪装成"还在跑"。
//
// 因为这代价只为了治 Windows 的缺陷，**垫片只在 win32 上注入**（见 decideShim）。
// CI 跑在 linux 上，那里缺陷不复现，就不该多带一条本机没跑过的退出路径。
// `DHAMPIR_WASM_TEST_SHIM=1|0` 可强制，用来在一种平台上复现另一种平台的行为。
//
// ---------------------------------------------------------------------------
// 为什么不"忽略退出码、只 grep `test result: ok`"
//
// 那是把守卫改成永远绿。assert 崩溃、链接失败、cargo 因磁盘满而中断，日志里都
// 可能**恰好**留着上一次的 ok 字样。接管退出让**退出码重新可信**之后，判据是
//
//   ① 每个目标的 runner 退出码为 0
//   ② 日志里出现 `test result: ok`，且 failed 计数为 0
//   ③ 跑过的测试数 == 二进制自己 `--list` 出来的测试数
//   ④ 源码里的 #[wasm_bindgen_test] 条数 == ③ 那个数
//   ⑤ 日志里没有那个 libuv 崩溃字样（接管退出后不该出现；出现说明环境变了，要红不要忍）
//
// 五条全中才退出 0。③④ 让"4 passed"不再是一个孤立数字：它必须与那个 .wasm 里
// 真实存在的测试数、以及源码里写下的测试数都对得上，截断的运行对不上。
//
// 用法：
//   node scripts/run-wasm-tests.mjs
//   node scripts/run-wasm-tests.mjs --out records/m0
//   node scripts/run-wasm-tests.mjs --self-test
//
// 环境变量 DHAMPIR_WASM_TEST_TIMEOUT_MS 可覆盖单个目标的超时（默认 300000）。

import { closeSync, copyFileSync, existsSync, mkdirSync, openSync, readdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { dirname, join, resolve } from 'node:path';
import { tmpdir } from 'node:os';
import { fileURLToPath } from 'node:url';

const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const TARGET = 'wasm32-unknown-unknown';
const CRATE = 'dhampir-wasm';
const SHIM_SOURCE = fileURLToPath(new URL('./wasm-test-node-exit-shim.cjs', import.meta.url));
// 接管退出之后，"挂住"是唯一的新风险。给每个目标一个超时，别让它伪装成还在跑。
const TARGET_TIMEOUT_MS = Number(process.env.DHAMPIR_WASM_TEST_TIMEOUT_MS ?? 300_000);

// 导出以便自检：参数解析错一次就够丢人的了（第一版就漏了 `i += 1`，
// 于是 `--out records/m0` 报"不认识的参数：records/m0"）。
export function parseArgs(argv) {
  const out = { out: 'records/m0', keepLogs: false };
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    if (arg === '--out') {
      const value = argv[i + 1];
      if (value === undefined || value.startsWith('--')) return { error: '--out 后面缺目录名' };
      out.out = value;
      i += 1; // 吃掉值，别让它下一轮被当成未知参数
    } else if (arg === '--keep-logs') out.keepLogs = true;
    else if (arg === '--print-locked-version') out.printLockedVersion = true;
    else if (arg === '--self-test') out.selfTest = true;
    else if (arg === '-h' || arg === '--help') out.help = true;
    else return { error: `不认识的参数：${arg}` };
  }
  return out;
}

/** 跑一个子进程，把 stdout/stderr 接到文件句柄，返回 { status, error, timedOut }。 */
function runToFile(command, args, { stdoutPath, stderrPath, env, timeout }) {
  const outFd = openSync(stdoutPath, 'w');
  const errFd = stdoutPath === stderrPath ? outFd : openSync(stderrPath, 'w');
  let status = null;
  let error = null;
  let timedOut = false;
  try {
    const result = spawnSync(command, args, {
      cwd: REPO_ROOT,
      stdio: ['ignore', outFd, errFd],
      shell: false,
      ...(timeout ? { timeout } : {}),
      ...(env ? { env } : {}),
    });
    status = result.status;
    error = result.error ?? null;
    // spawnSync 超时被杀时，error.code === 'ETIMEDOUT'
    timedOut = Boolean(result.error && result.error.code === 'ETIMEDOUT');
  } finally {
    closeSync(outFd);
    if (errFd !== outFd) closeSync(errFd);
  }
  return { status, error, timedOut };
}

/** 找个能用的 wasm-bindgen-test-runner。 */
export function findTestRunner() {
  // ① PATH 上先找（`cargo install wasm-bindgen-cli`, 或手动装的）
  const onPath = spawnSync('wasm-bindgen-test-runner', ['--version'], { encoding: 'utf8', shell: false });
  if (!onPath.error && onPath.status === 0) {
    return { path: 'wasm-bindgen-test-runner', version: String(onPath.stdout).trim() };
  }

  // ② wasm-pack 的缓存目录：%LOCALAPPDATA%\.wasm-pack\wasm-bindgen-<hash>\ 与
  //    ~/.cache/.wasm-pack/...。hash 不可预测，所以扫；多个版本时取字典序最大的
  //    （版本号数字位数固定，字典序与版本序一致）。
  const candidates = [];
  const roots = [];
  if (process.env.LOCALAPPDATA) roots.push(join(process.env.LOCALAPPDATA, '.wasm-pack'));
  if (process.env.HOME) roots.push(join(process.env.HOME, '.cache', '.wasm-pack'));
  for (const root of roots) {
    if (!existsSync(root)) continue;
    for (const entry of readdirSync(root, { withFileTypes: true })) {
      if (!entry.isDirectory() || !entry.name.startsWith('wasm-bindgen-')) continue;
      const exe = join(root, entry.name, process.platform === 'win32' ? 'wasm-bindgen-test-runner.exe' : 'wasm-bindgen-test-runner');
      if (existsSync(exe)) candidates.push(exe);
    }
  }
  candidates.sort();
  if (candidates.length === 0) return null;

  const exe = candidates[candidates.length - 1];
  const versionProbe = spawnSync(exe, ['--version'], { encoding: 'utf8', shell: false });
  return { path: exe, version: String(versionProbe.stdout ?? '').trim() };
}

/** Cargo.lock 里钉住的 wasm-bindgen crate 版本。CLI 必须与它一致。 */
export function lockedWasmBindgenVersion(lockText) {
  const match = /^\[\[package\]\]\nname = "wasm-bindgen"\nversion = "([^"]+)"/m.exec(lockText);
  return match ? match[1] : null;
}

/** 从 runner 的日志里数出结果。 */
export function summarizeLog(log) {
  const results = [];
  for (const match of log.matchAll(/^test result: (ok|FAILED)\. (\d+) passed; (\d+) failed/gm)) {
    results.push({ status: match[1], passed: Number(match[2]), failed: Number(match[3]) });
  }
  return { results, noTests: log.includes('no tests to run!') };
}

/** `--list` 的输出 → 测试名。行形如 `name: test`（bench 是 `name: bench`）。 */
export function parseTestList(listing) {
  const names = [];
  for (const line of listing.split('\n')) {
    const trimmed = line.trim();
    if (!trimmed.endsWith(': test')) continue;
    names.push(trimmed.slice(0, -': test'.length));
  }
  return names;
}

/**
 * 数源码里有多少个 `#[wasm_bindgen_test]`。
 *
 * 为什么要这件看起来多余的事：**wasm32 下只有 `#[wasm_bindgen_test]` 注册的测试会跑**。
 * 实测（2026-09-22）：`crates/dhampir-wasm` 的 lib 测试二进制里，5 个普通 `#[test]`
 * 的符号**在 .wasm 里搜得到**（确实编进去了），但 `--list` 一条都不列、运行时报
 * `no tests to run!`。也就是说普通 `#[test]` 在 wasm 上是"编进去但永远不执行"。
 *
 * 所以"跑过 4 个"必须和"源码里写了几个 `#[wasm_bindgen_test]`"对得上：
 * 谁把属性写错、把文件用 `#![cfg]` 关掉、或新加的 wasm 测试没被收集，这里就红。
 * 只认行首（允许缩进），避免把注释和字符串里的字样算进来。
 */
export function countWasmTestAttrs(sourceText) {
  return (sourceText.match(/^[ \t]*#\[wasm_bindgen_test\]/gm) ?? []).length;
}

/** 递归扫一个 crate 目录下所有 .rs，汇总 `#[wasm_bindgen_test]` 条数。 */
export function collectWasmTestAttrs(crateDir) {
  const files = [];
  const walk = (dir) => {
    for (const entry of readdirSync(dir, { withFileTypes: true })) {
      if (entry.name === 'target') continue;
      const path = join(dir, entry.name);
      if (entry.isDirectory()) walk(path);
      else if (entry.name.endsWith('.rs')) files.push(path);
    }
  };
  walk(crateDir);
  files.sort();

  let count = 0;
  const scanned = [];
  for (const file of files) {
    const n = countWasmTestAttrs(readFileSync(file, 'utf8'));
    if (n > 0) scanned.push(`${file.slice(REPO_ROOT.length + 1).replace(/\\/g, '/')}×${n}`);
    count += n;
  }
  return { count, files: scanned.length > 0 ? scanned : ['（没有任何文件写了 #[wasm_bindgen_test]）'] };
}

/**
 * runner 是否报了那个已知的 node 退出崩溃。
 *
 * 注意别把"测试失败"也算进来：正常运行到失败的用例时，runner 打印的是
 *
 *     Error: Node failed with exit_code 1
 *
 * ——那是**正常的失败上报**，不是崩溃。崩溃的签名是 libuv 断言，或者一个负数
 * 退出码（实测是 -1073740791，即 0xC0000409）。这个区分是被一次探针逼出来的：
 * 早期版本只匹配 "Node failed with exit_code"，于是在"测试真的失败"时喊狼来了，
 * 而误报会诱导人删掉守卫。
 */
export function detectNodeCrash(log) {
  if (log.includes('UV_HANDLE_CLOSING')) return true;
  return /Node failed with exit_code -(?!0\b)\d+/.test(log);
}

/**
 * 把退出垫片放到一个**不含空白字符**的路径上。
 * NODE_OPTIONS 不解析带空格的 --require 路径（反斜杠会被整个吃掉），
 * 所以这里宁可换位置，也不悄悄带着一个会失效的路径往下跑。
 */
export function resolveShimPath({ repoRoot, tmp, shimSource }) {
  const preferred = join(repoRoot, 'target', 'wasm-test-run', 'dhampir-wasm-test-defer-exit.cjs');
  if (!/\s/.test(preferred)) return { path: preferred, copy: true };
  const fallback = join(tmp, 'dhampir-wasm-test-defer-exit.cjs');
  if (!/\s/.test(fallback)) return { path: fallback, copy: true, reason: '仓库路径含空格，NODE_OPTIONS 不认' };
  return { path: null, reason: `仓库路径和临时目录都含空格（${repoRoot} / ${tmp}），无法用 NODE_OPTIONS 注入垫片` };
}

/**
 * 要不要注入退出垫片。
 *
 * 垫片治的是 **Windows 上 Node 的 libuv 退出断言**（见 wasm-test-node-exit-shim.cjs）。
 * 别的平台上那个缺陷不复现，注入它反而多带进一个**没在本地跑过**的退出路径——
 * 而"让 node 自己把事件循环跑干"这件事，在测试壳留下了活句柄时会变成挂住。
 * 所以：**没有缺陷的地方，不注入。**
 *
 * 判定抽成纯函数是为了能自检——"在 CI 的 Linux 上到底注没注垫片"这件事，
 * 不该只能靠读代码回答。`DHAMPIR_WASM_TEST_SHIM=1|0` 可强制，用来在一种平台上
 * 复现另一种平台的行为（例如在 Linux 上确认关掉垫片是否真的不需要它）。
 *
 * 返回 `null` 表示取值非法——**不静默降级成默认值**，那会让"我明明设了"变成猜谜。
 */
export function decideShim({ platform, flag }) {
  if (flag === '1') return { apply: true, reason: 'DHAMPIR_WASM_TEST_SHIM=1：强制注入' };
  if (flag === '0') return { apply: false, reason: 'DHAMPIR_WASM_TEST_SHIM=0：强制不注入' };
  if (flag !== undefined && flag !== '') return null;
  if (platform === 'win32') {
    return { apply: true, reason: `${platform}：libuv 退出断言在本机实测复现（12 次里崩 10 次）` };
  }
  return { apply: false, reason: `${platform}：该缺陷未在此平台复现，不引入没跑过的退出路径` };
}

function shimEnv(shimPath) {
  const existing = process.env.NODE_OPTIONS ? `${process.env.NODE_OPTIONS} ` : '';
  return { ...process.env, NODE_OPTIONS: `${existing}--require ${shimPath}` };
}

// ---------------------------------------------------------------------------
// 自检：这个脚本自己也是一道守卫，所以它必须能证明自己不是永远绿的。
// ---------------------------------------------------------------------------
export function selfTest() {
  const cases = [];
  const check = (name, ok) => cases.push({ name, ok });

  // -- parseArgs：第一版就漏了 `i += 1`，所以这几条是被真事故钉住的
  check('--out 吃掉自己的值，不把它当未知参数', parseArgs(['--out', 'records/m0']).out === 'records/m0');
  check('--out 之后的参数仍能被识别', parseArgs(['--out', 'records/m0', '--keep-logs']).keepLogs === true);
  check('--out 缺值要报错，不能默默用默认值', typeof parseArgs(['--out']).error === 'string');
  check('--out 后面跟另一个参数算缺值', typeof parseArgs(['--out', '--keep-logs']).error === 'string');
  check('不认识的参数要报错', typeof parseArgs(['--nope']).error === 'string');
  check('默认输出目录是 records/m0', parseArgs([]).out === 'records/m0');
  check('--help 能被识别', parseArgs(['--help']).help === true);

  // -- summarizeLog
  const okLog = 'running 4 tests\ntest result: ok. 4 passed; 0 failed; 0 ignored\n';
  const failLog = 'running 2 tests\ntest result: FAILED. 1 passed; 1 failed; 0 ignored\n';
  check('数得出全过', summarizeLog(okLog).results[0].passed === 4);
  check('数得出失败', summarizeLog(failLog).results[0].failed === 1);
  check('"没跑"与"跑过了"要区分开', summarizeLog('no tests to run!\n').noTests === true);
  check('空日志不谎报通过', summarizeLog('').results.length === 0);

  // -- parseTestList
  const listing = 'a_test: test\nb_test: test\n   # 忽略这行\nsome_bench: bench\nc test: test\n';
  check('解析 --list 的测试名', JSON.stringify(parseTestList(listing)) === JSON.stringify(['a_test', 'b_test', 'c test']));
  check('--list 空输出得到空清单', parseTestList('').length === 0);

  // -- countWasmTestAttrs
  const src = '#[wasm_bindgen_test]\nfn a() {}\n    #[wasm_bindgen_test]\nfn b() {}\n';
  check('数得出 wasm 测试属性条数', countWasmTestAttrs(src) === 2);
  check('缩进的属性也算', countWasmTestAttrs('  #[cfg(test)]\n  mod t {\n    #[wasm_bindgen_test]\n    fn c() {}\n  }\n') === 1);
  check('注释里的字样不算数', countWasmTestAttrs('// 用 #[wasm_bindgen_test] 标注\n') === 0);
  check('行内的字样不算数', countWasmTestAttrs('let s = "#[wasm_bindgen_test]";\n') === 0);

  // -- detectNodeCrash：这两个字样出现过，绝不能当成"通过"
  check('认得出 libuv 断言', detectNodeCrash('Assertion failed: !(handle->flags & UV_HANDLE_CLOSING)'));
  check('认得出崩溃的负数退出码', detectNodeCrash('Error: Node failed with exit_code -1073740791'));
  check('干净日志不误报', !detectNodeCrash(okLog));
  // 下面这条是被一次真实反向探针逼出来的：测试失败时 runner 也会打
  // "Node failed with exit_code 1"，那是正常的失败上报，不是崩溃。
  check('测试失败（exit_code 1）不算崩溃，别喊狼来了', !detectNodeCrash('running 2 tests\nError: Node failed with exit_code 1\n'));
  check('失败用例的 FAILED 行不算崩溃', !detectNodeCrash(failLog));

  // -- lockedWasmBindgenVersion：版本对齐是"第一天就红"的头号来源
  const lock = '[[package]]\nname = "wasm-bindgen"\nversion = "0.2.128"\n\n[[package]]\nname = "wasm-bindgen"\nversion = "9.9.9"\n';
  check('从 Cargo.lock 里读出 wasm-bindgen 版本（取第一个）', lockedWasmBindgenVersion(lock) === '0.2.128');
  check('lock 里没有 wasm-bindgen 时返回 null', lockedWasmBindgenVersion('[[package]]\nname = "other"\n') === null);

  // -- decideShim：在哪种平台上注入垫片，必须是可被证伪的判定，
  //    而不是埋在 main() 里的一句 if —— CI 跑在 linux 上，那正是没人手动看过的路径。
  const shimWin = decideShim({ platform: 'win32', flag: undefined });
  check('win32 上注入垫片', shimWin.apply === true && typeof shimWin.reason === 'string');
  const shimLinux = decideShim({ platform: 'linux', flag: undefined });
  check('linux 上不注入垫片（缺陷不复现，就别多带一条退出路径）', shimLinux.apply === false);
  check('=1 强制注入（可在非 win32 上复现那边的行为）', decideShim({ platform: 'linux', flag: '1' }).apply === true);
  check('=0 强制不注入', decideShim({ platform: 'win32', flag: '0' }).apply === false);
  check('非法取值返回 null，不静默降级成默认值', decideShim({ platform: 'win32', flag: 'yes' }) === null);
  check('空串等同于没设（shell 里 `export X=` 很常见）', decideShim({ platform: 'linux', flag: '' }).apply === false);
  check('判定不是恒真：两种平台至少有一种不注入', shimWin.apply !== shimLinux.apply);

  // -- resolveShimPath：NODE_OPTIONS 不认带空格的路径，这条实测过
  const noSpace = resolveShimPath({ repoRoot: 'F:\\a\\b', tmp: 'C:\\tmp', shimSource: '' });
  check('路径无空白时就用仓库内的位置', noSpace.path !== null && !/\s/.test(noSpace.path));
  const spaced = resolveShimPath({ repoRoot: 'F:\\my repo', tmp: 'C:\\tmp', shimSource: '' });
  check('仓库路径含空格时退到临时目录', spaced.path !== null && !/\s/.test(spaced.path) && spaced.path.includes('tmp'));
  const bothSpaced = resolveShimPath({ repoRoot: 'F:\\my repo', tmp: 'C:\\my temp', shimSource: '' });
  check('两个位置都含空格时明确失败，不静默降级', bothSpaced.path === null && typeof bothSpaced.reason === 'string');

  const failed = cases.filter((c) => !c.ok);
  for (const c of cases) console.log(`  ${c.ok ? '✓' : '✗'} ${c.name}`);
  if (failed.length > 0) {
    console.error(`\n✗ 自检未通过：${failed.length} / ${cases.length}`);
    return 1;
  }
  console.log(`\n✓ 自检通过（${cases.length} 条用例）`);
  return 0;
}

function main() {
  const args = parseArgs(process.argv.slice(2));
  if (args.help) {
    console.log('用法：node scripts/run-wasm-tests.mjs [--out <dir>] [--keep-logs] [--self-test]');
    return 0;
  }
  if (args.error) {
    console.error(`参数错误：${args.error}`);
    return 2;
  }
  if (args.selfTest) return selfTest();

  // -- 供 CI 用：把 Cargo.lock 里钉住的版本原样打出来，一行。
  //
  // 为什么要有这个出口：CI 需要"装一个与 crate 版本相同的 CLI"，而版本号只该有
  // 一处真相。让工作流自己写正则去啃 Cargo.lock，等于把 lockedWasmBindgenVersion
  // 抄了第二遍——漂移的那天正好是最不该有漂移的那天。
  if (args.printLockedVersion) {
    const locked = lockedWasmBindgenVersion(readFileSync(join(REPO_ROOT, 'Cargo.lock'), 'utf8'));
    if (!locked) {
      console.error('✗ Cargo.lock 里没有 wasm-bindgen —— 这个仓库的依赖表坏了，不是参数问题');
      return 2;
    }
    console.log(locked);
    return 0;
  }

  const workDir = join(REPO_ROOT, 'target', 'wasm-test-run');
  mkdirSync(workDir, { recursive: true });

  const runner = findTestRunner();
  if (!runner) {
    console.error(
      '✗ 找不到 wasm-bindgen-test-runner。\n' +
        '  装法一：cargo install wasm-bindgen-cli --version <与 Cargo.lock 里的 wasm-bindgen 相同>\n' +
        '  装法二：随便跑一次 `wasm-pack test --node`，wasm-pack 会把它下到自己的缓存里。',
    );
    return 2;
  }

  const shimDecision = decideShim({ platform: process.platform, flag: process.env.DHAMPIR_WASM_TEST_SHIM });
  if (!shimDecision) {
    console.error(`✗ DHAMPIR_WASM_TEST_SHIM 只认 1 或 0，收到：${process.env.DHAMPIR_WASM_TEST_SHIM}`);
    return 2;
  }

  let shimPath = null;
  if (shimDecision.apply) {
    const shim = resolveShimPath({ repoRoot: REPO_ROOT, tmp: tmpdir(), shimSource: SHIM_SOURCE });
    if (!shim.path) {
      console.error(`✗ 无法放置退出垫片：${shim.reason}`);
      return 2;
    }
    if (shim.copy) {
      mkdirSync(dirname(shim.path), { recursive: true });
      copyFileSync(SHIM_SOURCE, shim.path);
    }
    shimPath = shim.path;
  }
  const env = shimPath ? shimEnv(shimPath) : process.env;

  // CLI 与 crate 版本必须一致，否则 wasm-bindgen 会拒绝生成胶水代码。
  // 这是"第一天就红"的头号来源，所以在真正跑测试之前先挡住。
  const lockedVersion = lockedWasmBindgenVersion(readFileSync(join(REPO_ROOT, 'Cargo.lock'), 'utf8'));
  const cliVersion = /(\d+\.\d+\.\d+(?:[-+][\w.-]+)?)/.exec(runner.version)?.[1] ?? null;
  if (lockedVersion && cliVersion && lockedVersion !== cliVersion) {
    console.error(
      `✗ wasm-bindgen CLI 版本与 Cargo.lock 不一致：CLI=${cliVersion} crate=${lockedVersion}。\n` +
        `  ${runner.path}\n` +
        '  版本不一致时 wasm-bindgen 会拒绝生成胶水代码，报的是"linked against a different\n' +
        '  version of wasm-bindgen"——看起来像构建坏了，其实是版本没对齐。',
    );
    return 2;
  }

  // ---- 1. 编译测试目标（不运行） ------------------------------------------
  const buildOut = join(workDir, 'cargo-stdout.json');
  const buildErr = join(workDir, 'cargo-stderr.txt');
  const build = runToFile(
    'cargo',
    ['test', '--target', TARGET, '-p', CRATE, '--no-run', '--message-format', 'json', '--color', 'never'],
    { stdoutPath: buildOut, stderrPath: buildErr },
  );

  if (build.error) {
    console.error(`✗ 起不来 cargo：${build.error.message}`);
    return 2;
  }
  if (build.status !== 0) {
    console.error(`✗ 编译 wasm 测试目标失败（退出码 ${build.status}）：\n${readFileSync(buildErr, 'utf8')}`);
    return 1;
  }

  const executables = [];
  for (const line of readFileSync(buildOut, 'utf8').split('\n')) {
    const trimmed = line.trim();
    if (!trimmed.startsWith('{')) continue;
    let message;
    try {
      message = JSON.parse(trimmed);
    } catch {
      continue;
    }
    // profile.test 为真才是测试可执行文件；cdylib 产物也有 .wasm，别混进来。
    if (message.reason !== 'compiler-artifact' || message.profile?.test !== true) continue;
    if (typeof message.executable !== 'string' || !message.executable.endsWith('.wasm')) continue;
    executables.push({
      path: message.executable,
      name: message.target?.name ?? '<未知>',
    });
  }

  if (executables.length === 0) {
    console.error('✗ 没有编译出任何 wasm 测试目标——拒绝在空集合上通过。');
    return 2;
  }

  // ---- 2. 逐个目标：先问清单，再跑 --------------------------------------
  const targets = [];
  const problems = [];
  for (const executable of executables) {
    const slug = executable.name.replace(/[^\w.-]/g, '_');
    const listLog = join(workDir, `${slug}.list.txt`);
    const runLog = join(workDir, `${slug}.log`);

    // ②-a 二进制里到底有哪些测试 —— 顺带证明它确实是个测试可执行文件
    const listing = runToFile(runner.path, [executable.path, '--list'], {
      stdoutPath: listLog,
      stderrPath: listLog,
      env,
      timeout: TARGET_TIMEOUT_MS,
    });
    const listed = parseTestList(readFileSync(listLog, 'utf8')).sort();

    // ②-b 正式跑
    const run = runToFile(runner.path, [executable.path], {
      stdoutPath: runLog,
      stderrPath: runLog,
      env,
      timeout: TARGET_TIMEOUT_MS,
    });
    const log = readFileSync(runLog, 'utf8').replace(/\r\n/g, '\n');
    const summary = summarizeLog(log);
    const passed = summary.results.reduce((sum, r) => sum + r.passed, 0);
    const failed = summary.results.reduce((sum, r) => sum + r.failed, 0);
    const crashed = detectNodeCrash(log);

    targets.push({
      target: executable.name,
      wasm: executable.path.replace(/\\/g, '/'),
      listed_tests: listed,
      exit_code: run.status,
      passed,
      failed,
      ran: summary.results.length > 0,
      // 0 条不代表出错：lib 目标的单测是普通 #[test]，wasm 上编得进去但不会跑
      // （见 countWasmTestAttrs 的注释）。这里如实记下来，别让它悄悄过去。
      wasm_runnable: listed.length > 0,
      timed_out: run.timedOut || listing.timedOut,
      node_crash_text_seen: crashed,
      log: log.trim().split('\n'),
    });

    if (listing.status !== 0) {
      problems.push(`${executable.name}: --list 退出码 ${listing.status}，这个文件可能根本不是测试目标`);
    }
    // 接管退出后，"挂住"是唯一的新风险，必须单独判死。
    if (run.timedOut || listing.timedOut) {
      problems.push(
        `${executable.name}: 超过 ${TARGET_TIMEOUT_MS} ms 还没退出，被强杀了。` +
          '接管退出（只设 exitCode、不真退）之后，如果测试壳留下活句柄，node 会挂住。' +
          '这不是"跑得慢"，是红。',
      );
    }
    if (run.status !== 0) {
      problems.push(`${executable.name}: runner 退出码 ${run.status}`);
    }
    if (failed > 0) {
      problems.push(`${executable.name}: ${failed} 个测试失败`);
    }
    if (!summary.noTests && summary.results.length === 0) {
      problems.push(`${executable.name}: 日志里没有 "test result:" 行——运行被提前打断了`);
    }
    // 数对不上：截断的运行、陈旧的二进制、漏跑的测试都会在这里露头。
    if (summary.results.length > 0 && passed !== listed.length) {
      problems.push(`${executable.name}: 跑了 ${passed} 个测试，但二进制里列出来的是 ${listed.length} 个`);
    }
    if (crashed) {
      problems.push(
        `${executable.name}: 日志里出现了 node 的 libuv 退出崩溃。` +
          '接管退出（scripts/wasm-test-node-exit-shim.cjs）本该挡住它——出现了说明环境变了，去查，别忽略。',
      );
    }
  }

  const passed = targets.reduce((sum, t) => sum + t.passed, 0);
  const failed = targets.reduce((sum, t) => sum + t.failed, 0);
  if (passed === 0) {
    problems.push('一个 wasm 测试都没跑起来——"没跑"和"跑过了"必须区分开');
  }

  // ---- 2c. 与源码对账 ----------------------------------------------------
  // wasm32 下普通 #[test] 编得进去却永不执行，所以"跑了几条"必须和源码里
  // 写了几个 #[wasm_bindgen_test] 对得上。这条独立于运行日志。
  const sourceTests = collectWasmTestAttrs(join(REPO_ROOT, 'crates', CRATE));
  const totalListed = targets.reduce((sum, t) => sum + t.listed_tests.length, 0);
  if (sourceTests.count !== totalListed) {
    problems.push(
      `源码里写了 ${sourceTests.count} 个 #[wasm_bindgen_test]，但 wasm 侧只列出并跑了 ${totalListed} 个。` +
        `（wasm32 下普通 #[test] 不会执行，只有 #[wasm_bindgen_test] 会——属性写错或文件被 #![cfg] 关掉都会走到这里）` +
        ` 扫过的文件：${sourceTests.files.join(', ')}`,
    );
  }

  // ---- 3. 记录 ----------------------------------------------------------
  const record = {
    schema: 1,
    milestone: 'M0',
    kind: 'wasm-tests',
    target: TARGET,
    crate: CRATE,
    // 如实写明我们没有走 wasm-pack：将来别人复现时，照着这里写的做就行。
    runner: { path: runner.path, version: runner.version },
    node: process.version,
    wasm_bindgen_locked: lockedVersion,
    // 本机 node 退出路径的缺陷 + 绕过方式，写进记录而不是只留在注释里。
    // `applied` 必须如实写：记录读起来要能回答"这一次到底注没注垫片"，
    // 否则将来在别的平台上看到一份绿记录，会以为垫片也参与了。
    node_exit_shim: {
      applied: shimPath !== null,
      platform: process.platform,
      reason: shimDecision.reason,
      shim: shimPath ? shimPath.replace(/\\/g, '/') : null,
      why: 'Windows + Node 只要真的调用 process.exit()，就可能撞上 libuv 的 UV_HANDLE_CLOSING 断言，退出码变成 -1073740791，与测试结果无关。垫片接管 process.exit，只设 process.exitCode，让 node 自然退出。12 次实测：无垫片崩 10 次、接管后崩 0 次。非 win32 上该缺陷不复现，故不注入。',
      nota_bene: '垫片只改退出路径，不改退出码——测试失败仍然是非零退出；"挂住"由超时判死。',
      override: 'DHAMPIR_WASM_TEST_SHIM=1|0 可强制（在一种平台上复现另一种平台的行为）',
      timeout_ms: TARGET_TIMEOUT_MS,
    },
    command: 'node scripts/run-wasm-tests.mjs',
    // 源码里写了几个 #[wasm_bindgen_test] —— 与跑过的条数必须相等
    source_wasm_bindgen_tests: sourceTests,
    targets,
    listed_total: totalListed,
    passed,
    failed,
    exit_code: problems.length === 0 ? 0 : 1,
  };

  const outDir = resolve(REPO_ROOT, args.out);
  mkdirSync(outDir, { recursive: true });
  const recordPath = join(outDir, 'wasm-tests.json');
  writeFileSync(recordPath, `${JSON.stringify(record, null, 2)}\n`);

  // ---- 4. 结论 ----------------------------------------------------------
  for (const target of targets) {
    for (const line of target.log) console.log(`  ${line}`);
  }

  if (problems.length > 0) {
    console.error('\n✗ wasm 测试没通过：');
    for (const problem of problems) console.error(`  - ${problem}`);
    console.error(`  记录 → ${recordPath}`);
    return 1;
  }

  const skipped = targets.filter((t) => !t.wasm_runnable).map((t) => t.target);
  if (!args.keepLogs) rmSync(workDir, { recursive: true, force: true });

  console.log(`\n✓ wasm32 运行时上 ${passed} 个测试全过（${targets.length} 个目标，退出码全为 0）`);
  console.log(`  与源码对账：${sourceTests.count} 个 #[wasm_bindgen_test] == ${totalListed} 条实际列出并跑过`);
  if (skipped.length > 0) {
    console.log(`  无 wasm 可跑测试的目标（普通 #[test] 在 wasm 上不执行，如实记下）：${skipped.join(', ')}`);
  }
  console.log(`  记录 → ${recordPath}`);
  return 0;
}

// 本进程（父）同样不调 process.exit()：本机 Node/Windows 上真被执行
// process.exit() 会概率性撞 libuv 断言，把退出码变成负数——而"退出码可信"
// 正是这个脚本存在的理由。所有子进程都用 spawnSync 跑完了，没有待 drain 的句柄，
// 设 process.exitCode 后自然退出即刻发生。
process.exitCode = main();
