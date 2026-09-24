#!/usr/bin/env node
// wasm pkg 的**陈旧判定**与**一次自动重建**。
//
// # 为什么要把它单独放一个文件
//
// 这件事有两个消费者，而它们要做的事**不同**：
//
//   * `scripts/check-web-invariants.mjs`（守卫）—— 只**判**，绝不改工作区。
//     守卫去重建是坏味道：跑一次守卫顺手写一堆文件，之后谁也说不清
//     "现在这棵树是什么"。而且守卫本来就不能修它检查的东西。
//   * `scripts/web-check.mjs`（驱动）—— 要**用**这个 pkg 起页面。
//     它重建是合理的：它本来就是在演"一次真实的运行"。
//
// 两个消费者必须对"什么算陈旧"**完全一致**，否则会出现
// 「守卫说陈旧、驱动说不用重建」这种谁也说不清的状态。所以判据只有这一份。
//
// # 陈旧的后果为什么值得专门做一件事
//
// pkg 是 gitignore 的构建产物，**没有版本号、也没有校验**。
// 改了 Rust 却没重建时，浏览器拿旧 wasm 跑，报出来的错与原因毫无关系：
//
//   * 新导出的函数不存在 -> `engine.doc is not a function`；
//   * 或者更坏：新旧形状对不上，在 wasm 里撞 trap，页面只有一句
//     `启动失败：unreachable executed`。
//
// 判据是 **mtime**，那就得承认它的边界：mtime 只看"谁更新"，
// 不看"内容是否一致"。所以它挡的是"改了 Rust 没重建"这件真事，
// **挡不住**"重建过但内容仍然不对"（那要靠别的判据）。

import { existsSync, readdirSync, statSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const HERE = dirname(fileURLToPath(import.meta.url));
export const REPO_ROOT = resolve(HERE, '..');

/** pkg 目录（构建产物，gitignore）。 */
export const PKG_DIR = join(REPO_ROOT, 'crates', 'dhampir-wasm', 'www', 'pkg');
/** 拿来做 mtime 代表的那个文件。**用 .wasm 而不是目录**：目录的 mtime 在多数文件系统上
 * 只在"增删条目"时变，重新构建（覆盖同名文件）**不会**动它 —— 拿目录判会永远说不陈旧。 */
export const PKG_WASM_PATH = join(PKG_DIR, 'dhampir_wasm_bg.wasm');

/** 什么算"它的源码"。**改这些里面任何一个都该重建 pkg**。 */
export const WASM_SOURCE_PATHS = [
  join(REPO_ROOT, 'crates', 'dhampir-wasm', 'src'),
  join(REPO_ROOT, 'crates', 'dhampir-core', 'src'),
  join(REPO_ROOT, 'crates', 'dhampir-timeline', 'src'),
  join(REPO_ROOT, 'Cargo.toml'),
];

/** 重建命令。**工作目录与坑 1 里那条一字不差** —— 两种写法会在两边各建一份。 */
export const REBUILD = {
  program: 'wasm-pack',
  args: ['build', '--dev', '--target', 'web', '--out-dir', 'www/pkg'],
  cwd: join(REPO_ROOT, 'crates', 'dhampir-wasm'),
  display: 'wasm-pack build --dev --target web --out-dir www/pkg（工作目录 crates/dhampir-wasm）',
};

/**
 * 重建时**必须**关掉增量编译。这不是性能调优，是"能不能成"的问题。
 *
 * 实测（rustc 1.97.0，本机，2026-09-24）：开着增量时，**一次失败的构建会毒化增量缓存**，
 * 之后每次重建都撞 ICE（`rustc_metadata/src/rmeta/encoder.rs:2457: no entry found for key`，
 * 报 `the compiler unexpectedly panicked. This is a bug`），而且**重试也没用** ——
 * 连试三次都是同一个 panic。关掉增量之后同一条命令稳定成功（9.07 秒）。
 *
 * 这条是**做 D12 的过程中撞出来的**：为了验"真的坏了仍然红"，
 * 我故意往 `dhampir-wasm/src/lib.rs` 里塞了一个语法错（随后逐字节还原），
 * 那次失败的构建就是毒源。所以这不是理论风险 —— 它正是"用一次自动重建"这条路上
 * 最常见的一种情形：**上一次构建失败过**。
 *
 * 顺带一个量到的数：关掉增量那次是 9.07 秒；开着增量先成功过一次 22.6 秒。
 * 两个数**不是同一条件下的对照**，所以只用作"关掉它不会更慢"的旁证，不当结论。
 */
export function rebuildEnv(base = process.env) {
  return { ...base, CARGO_INCREMENTAL: '0' };
}

/** 一个目录（或文件）里最新的 mtime。目录递归。 */
export function newestMtime(path) {
  if (!existsSync(path)) return null;
  const stats = statSync(path);
  if (stats.isFile()) return stats.mtimeMs;
  let newest = stats.mtimeMs;
  for (const entry of readdirSync(path, { withFileTypes: true })) {
    const child = join(path, entry.name);
    const childNewest = newestMtime(child);
    if (childNewest !== null && childNewest > newest) newest = childNewest;
  }
  return newest;
}

/** 单个文件的 mtime；不在就是 null。 */
export function fileMtime(path) {
  try {
    return statSync(path).mtimeMs;
  } catch (error) {
    return null;
  }
}

/**
 * pkg 的**源码里最新的那个 mtime**。一个源路径都没有时返回 null。
 *
 * 单独抽出来是因为它要能被测：写死在这里的话，"源码目录搬了家"
 * 会让判据静默退化成一个恒真的检查（谁都比不过 0）。
 */
export function newestSourceMtime(paths = WASM_SOURCE_PATHS) {
  const values = paths.map(newestMtime).filter((value) => value !== null);
  if (values.length === 0) return null;
  return values.reduce((a, b) => Math.max(a, b), 0);
}

/**
 * 现在的状态。`stale` 为真 = **要重建**。
 *
 * 三种"判不了"（pkg 没构建过 / 一个源路径都不在）都返回 `stale: false` ——
 * 它们是"还没到这步"，不是"不变量被破坏"。
 */
export function pkgState() {
  const pkgMtime = fileMtime(PKG_WASM_PATH);
  const sourceMtime = newestSourceMtime();
  return {
    pkgMtime,
    sourceMtime,
    pkgPath: PKG_WASM_PATH,
    stale: pkgMtime !== null && sourceMtime !== null && pkgMtime < sourceMtime,
  };
}

/**
 * 陈旧就**重建一次**，不陈旧就什么都不做。
 *
 * 「**一次**」是要点：重建之后仍然是旧的（或者重建本身失败了），
 * 说明问题不是"忘了重建"，而是更实在的东西（wasm-pack 不在、源码编不过、
 * 源码目录搬了家）。那时**如实报出去，不再重试** ——
 * 重试会把一次编译错误变成一个转不完的循环。
 *
 * 返回 `{ attempted, rebuilt, ok, reason, output }`：
 *
 * * `ok: false` 的意思是**不许继续**（调用方该带着 reason 退非零）；
 * * `attempted: false, ok: true` 是"没什么要做的"，不是失败。
 *
 * `spawn` 可以注入，所以自检能在**不起真进程**的前提下把三条路都走一遍。
 */
export function ensureFreshPkg(options = {}) {
  return ensureFreshPkgWith({
    spawn: options.spawn ?? spawnSync,
    log: options.log ?? (() => {}),
    pkgState,
  });
}

/**
 * `ensureFreshPkg` 的本体：**判据与分支只有这一份**。
 *
 * 它把两件会与外部世界打交道的事收成参数：
 *
 * * `pkgState` —— 真跑走 `pkgState()`，自检走脚本化的假状态
 *   （自检不该去动真实的 pkg：那会写工作区）；
 * * `spawn` —— 真跑起真的 wasm-pack（几十秒），自检起一个假的（返一个对象）。
 *
 * 之所以要有这么一层而不是"在自检里照着抄一遍分支"：抄一遍之后，
 * 测的就是那份复制品了 —— 而复制品与真实现分叉时，自检照样全绿。
 */
function runSelfTest() {
  let passed = 0;
  const expect = (name, condition) => {
    if (!condition) throw new Error('自检失败：' + name);
    passed += 1;
  };
  const state = (pkg, source) => ({ pkgMtime: pkg, sourceMtime: source, pkgPath: 'x.wasm',
    stale: pkg !== null && source !== null && pkg < source });

  // newestSourceMtime：一个源路径都不在时必须是 null，不能是 0 ——
  // 0 会让"pkg 比 0 新"恒成立，于是判据静默退化成一个恒真的检查。
  expect('没有源路径 -> null', newestSourceMtime([join(REPO_ROOT, 'definitely', 'not', 'here')]) === null);
  expect('源路径都有 mtime -> 取最大', newestSourceMtime([REPO_ROOT]) !== null);

  // 注入的 spawn：把三条路走一遍，不起真进程。
  const staleState = { pkgMtime: 100, sourceMtime: 200, pkgPath: 'x.wasm', stale: true };
  const freshState = { pkgMtime: 300, sourceMtime: 200, pkgPath: 'x.wasm', stale: false };

  let calls = 0;
  let lastOptions = null;
  const okRun = (program, args, options) => {
    calls += 1;
    lastOptions = options;
    return { status: 0, stdout: '', stderr: '' };
  };
  const failRun = (program, args, options) => {
    calls += 1;
    lastOptions = options;
    return { status: 101, stdout: '', stderr: 'error: could not compile' };
  };
  const goneRun = (program, args, options) => {
    calls += 1;
    lastOptions = options;
    return { status: null, error: new Error('spawn wasm-pack ENOENT') };
  };

  // 这里直接测 ensureFreshPkg 的分支，所以把它要用的 pkgState 结果**换掉**：
  // 用一组按调用次序返回的假状态（第一次陈旧、第二次新鲜）。
  const scripted = (states) => {
    let index = 0;
    return () => states[Math.min(index++, states.length - 1)];
  };
  const withStates = (states, options) => {
    const seq = scripted(states);
    // 借用真实函数，只把 pkgState 换成脚本化的 —— 保持其余逻辑一字不改。
    const fake = { ...options, pkgState: seq };
    return ensureFreshPkgWith(fake);
  };

  // 不陈旧：一次进程都不该起。
  calls = 0;
  const notStale = withStates([freshState], { spawn: okRun });
  expect('不陈旧 -> 不重建', notStale.attempted === false && calls === 0);

  // pkg 没构建过：也不该起进程，而且**不算失败**（守卫同样不判它）。
  calls = 0;
  const noPkg = withStates([state(null, 200)], { spawn: okRun });
  expect('pkg 不在 -> 不重建也不失败', noPkg.ok === true && calls === 0);

  // 陈旧 + 重建成功：**正好一次**。
  calls = 0;
  const rebuilt = withStates([staleState, freshState], { spawn: okRun });
  expect('陈旧 -> 重建一次', rebuilt.attempted === true && rebuilt.rebuilt === true && calls === 1);

  // 重建时那两个**必须**带上的东西。少了任何一个都会让这条自动重建在真机上莫名其妙地失败：
  // CARGO_INCREMENTAL=0（不然一次失败过的构建会毒化增量缓存，之后次次 ICE）、
  // stdin=ignore（不然在这个 agent 会话里直接 EBUSY，见坑 19）。
  expect('重建关掉增量编译',
    lastOptions !== null && lastOptions.env !== undefined && lastOptions.env.CARGO_INCREMENTAL === '0');
  expect('重建不吃 stdin（坑 19）',
    lastOptions !== null && Array.isArray(lastOptions.stdio) && lastOptions.stdio[0] === 'ignore');
  expect('重建的工作目录是 crates/dhampir-wasm',
    lastOptions !== null && String(lastOptions.cwd).endsWith('dhampir-wasm'));

  // 陈旧 + 编译错误：必须 ok=false，而且**只试一次**。
  calls = 0;
  const failed = withStates([staleState, staleState], { spawn: failRun });
  expect('编译错误 -> 仍然红', failed.ok === false && calls === 1);
  expect('报错里要带重建命令', failed.reason.includes('wasm-pack'));

  // 陈旧 + wasm-pack 不在：也是红，且说清是"起不来"不是"编不过"。
  calls = 0;
  const missing = withStates([staleState, staleState], { spawn: goneRun });
  expect('wasm-pack 不在 -> 红', missing.ok === false && calls === 1);
  expect('说清是起不来', missing.reason.includes('ENOENT'));

  // 重建"成功"但 pkg 仍然旧：不能报通过 —— 那说明判据的前提不成立。
  calls = 0;
  const stillStale = withStates([staleState, staleState], { spawn: okRun });
  expect('重建后仍旧 -> 红（不假装成功）', stillStale.ok === false && calls === 1);

  console.log('✓ 陈旧 pkg 自检通过（' + passed + ' 条断言）');
}

/**
 * `ensureFreshPkg` 的本体见上（`ensureFreshPkgWith`）。这里只剩分支实现。
 */
function ensureFreshPkgWith(options) {
  const state = options.pkgState;
  const run = options.spawn;
  const log = options.log ?? (() => {});
  const before = state();
  if (before.pkgMtime === null) {
    return { attempted: false, rebuilt: false, ok: true,
      reason: 'pkg 还没构建过（守卫也不判它），跳过重建', output: '' };
  }
  if (!before.stale) {
    return { attempted: false, rebuilt: false, ok: true, reason: 'pkg 不比源码旧，不用重建', output: '' };
  }
  log('wasm pkg 比源码旧，重建一次：' + REBUILD.display);
  // **stdin 显式给 `ignore`**：在这个 agent 会话里给子进程开 stdin 管道会
  // `ERROR_PIPE_BUSY`（next-steps.md 坑 19），而 wasm-pack 根本不需要 stdin。
  // 不写这一条，这条自动重建会在这个会话里**必然失败**，且错得看不出原因。
  const result = run(REBUILD.program, REBUILD.args,
    { cwd: REBUILD.cwd, encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'], env: rebuildEnv() });
  const output = String((result && (result.stderr || result.stdout)) || '').trim();
  if (result === null || result === undefined || result.error || result.status !== 0) {
    const why = result && result.error ? String(result.error.message || result.error) : '退出码 ' + (result ? result.status : '无');
    return { attempted: true, rebuilt: false, ok: false,
      reason: '自动重建失败（' + why + '）—— 这一次**不许**当成通过：' + REBUILD.display,
      output, before };
  }
  const after = state();
  if (after.stale) {
    return { attempted: true, rebuilt: false, ok: false, output, before, after,
      reason: '重建跑完了，pkg 仍然比源码旧 —— 说明原因不是"忘了重建"（源码目录搬过家？）' };
  }
  return { attempted: true, rebuilt: true, ok: true, reason: '重建完成，pkg 现在是新的', output, before, after };
}

export { ensureFreshPkgWith };

const invoked = process.argv[1] ? resolve(process.argv[1]) === fileURLToPath(import.meta.url) : false;
if (invoked) {
  const argv = process.argv.slice(2);
  if (argv.includes('--self-test')) {
    runSelfTest();
  } else if (argv.includes('--rebuild')) {
    // 手跑一条：**只有它真的会起 wasm-pack**。
    // 给一条独立可跑的命令，是因为"看到守卫说陈旧、然后想重建一次"这件事
    // 不该逼人去起一整个浏览器验收。
    const result = ensureFreshPkg({ log: (line) => console.log(line) });
    console.log(result.ok ? '✓ ' + result.reason : '✗ ' + result.reason);
    if (!result.ok && result.output) console.error(result.output.split('\n').slice(-20).join('\n'));
    process.exitCode = result.ok ? 0 : 1;
  } else {
    const state = pkgState();
    console.log('pkg      : ' + state.pkgPath);
    console.log('pkg mtime: ' + (state.pkgMtime === null ? '（不在）' : new Date(state.pkgMtime).toISOString()));
    console.log('源 mtime : ' + (state.sourceMtime === null ? '（判不了）' : new Date(state.sourceMtime).toISOString()));
    console.log('陈旧     : ' + state.stale);
    process.exitCode = state.stale ? 1 : 0;
  }
}
