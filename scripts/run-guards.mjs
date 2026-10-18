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
import { closeSync, mkdirSync, openSync, readFileSync, readdirSync } from 'node:fs';
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
  // `deps` = 这条守卫要什么才能跑（见下面的 DEPS 词表）。**它不是注释，是判据**：
  // `scripts/check-ci-guards.mjs` 拿它对着 ci.yml 核，CI 该跑哪几档由 IN_CI 说了算。
  // CI 覆盖对表（第 80 轮）：ci.yml 该跑哪几档，由这里的 deps / IN_CI 说了算。
  // 它自己是 static 档 —— 所以"它也得在 CI 里跑"这件事由它自己判。
  { script: 'check-ci-guards.mjs', args: [], deps: 'static' },
  { script: 'check-core-purity.mjs', args: [], deps: 'static' },
  { script: 'check-dep-graph.mjs', args: [], deps: 'static' },
  { script: 'check-text-hygiene.mjs', args: [], deps: 'static' },
  { script: 'check-linux-portability.mjs', args: [], deps: 'static' },
  { script: 'check-sequential-decode.mjs', args: [], deps: 'static' },
  { script: 'check-web-invariants.mjs', args: [], deps: 'wasm-pkg' },
  // 网页动画 → 工程文件的转译器（阶段 2）：只吃 fixtures 里的样本快照 + 纯数据判据，
  // 不碰 GPU / ffmpeg / 浏览器，所以放前面。
  { script: 'check-waapi2doc.mjs', args: [], deps: 'static' },
  // 网页那份求值（第二实现）与 core 的逐帧数值对照：它漂了必须红。
  { script: 'check-anim-eval.mjs', args: [], deps: 'cargo' },
  // 两个宿主的形状对表（DOM 接口 ↔ 承诺过的 wasm 导出名）：只读两个文件，最轻。
  { script: 'check-host-parity.mjs', args: [], deps: 'static' },
  // 能力登记表（web/capabilities.json）：supported 必须有原语与判据，DOM 用了的必须登记。
  { script: 'check-capabilities.mjs', args: [], deps: 'static' },
  // DOM 侧的特效 → CSS 映射（口径 D5/D6），以及宿主有没有真的用上它。
  { script: 'check-dom-css.mjs', args: [], deps: 'static' },
  // 「允许的差异」台账：未实测的不许写成数，每条都要被引用。
  { script: 'check-dom-differences.mjs', args: [], deps: 'static' },
  // **不登记 `scripts/measure-dom-parity.mjs`（第 83 轮那三条 DOM 差异的量测工具）** —— 它是
  // `measure-*` 那一族（与 measure-render / measure-decode 同类）：**量数**，不判红。守卫的门槛是
  // "能被证明会红"，而它只出读数（判在台账的容差上，判据是 check-dom-differences）。
  // 它的 `--self-test`（28 条）随时可跑，就是不进这份清单 —— 登记了反而会被"清点"判成找不到
  // （`GUARD_FILE_RE` 只认 check-* 那一族），第 83 轮当场吃过这一下。
  // 预览那条腿（canvas）与 CLI 出图的对照（第 83 轮，G2）。要浏览器 + wasm pkg + dhampir，
  // 三样里最稀缺的是浏览器 ⇒ 归 `browser` 档；前置写在它自己的头部。
  { script: 'check-preview-path.mjs', args: [], deps: 'browser' },
  { script: 'check-backend-seam.mjs', args: [], deps: 'static' },
  { script: 'check-defects.mjs', args: [], deps: 'static' },
  // 取帧节奏（D24）：亮度斜坡量出"请求第 i 帧实际拿到源的哪一帧"。
  //
  // 它**需要构建产物 + ffmpeg**，比别的静态检查重。放这里是因为它盯的东西没有别处能盯：
  // 契约函数 `source_frame_at` 的数学**是对的**，漂在**投递路径**上 ——
  // 而那条路径此前一条判据都没有。
  //
  // 注意它的**退出码约定**（D24 未修期间）：漂移形状与台账一致 ⇒ exit 0，但明确说清
  // "这仍是缺陷，不是通过"；形状**变了**（好了或坏了）⇒ exit 1。详见文件头。
  { script: 'check-frame-pacing.mjs', args: [], deps: 'ffmpeg' },
  { script: 'check-preview-parity.mjs', args: [], deps: 'static' },
  { script: 'check-overlay-plumbing.mjs', args: [], deps: 'static' },
  { script: 'check-m1-record.mjs', args: ['--record', 'records/m1'], deps: 'records' },
  { script: 'check-m2-record.mjs', args: ['--record', 'records/m2'], deps: 'records' },
  { script: 'check-media-status.mjs', args: [], deps: 'static' },
  { script: 'check-effect-registry.mjs', args: [], deps: 'static' },
  { script: 'check-local-backend.mjs', args: [], deps: 'cli' },
  // 同源代理那条路：它坏了不会有别的守卫变红（check-local-backend 直接打后端，
  // 不经过转发），而产品路径的最后一跳正是从它上面走的。
  { script: 'check-same-origin-proxy.mjs', args: [], deps: 'static' },
  { script: 'api-surface.mjs', args: [], deps: 'static' },
  { script: 'timeline-contract.mjs', args: [], deps: 'cargo' },
  { script: 'check-cli.mjs', args: [], deps: 'cli' },
  // 老工程逐字节不变（第 80 轮）：这台机器上要比对**已发布产物**渲染同一批帧。
  // 它要 GPU 出片 + dist/ 里的历史产物 + out/effects-demo 那份带素材的工程，
  // 所以是 `cli` 档（CI 上跑不了，也不该假装能跑）。
  { script: 'check-old-project-baseline.mjs', args: [], deps: 'cli' },
  { script: 'check-dual-end.mjs', args: [], deps: 'browser' },
];

/**
 * 依赖档词表。**一条守卫只能属于一档**，档决定它在哪跑：
 *
 * | 档 | 要什么 | 例 |
 * |---|---|---|
 * | `static` | 只读仓库文件（+ 起 node 子进程，输出走文件重定向） | 能力登记表、宿主对表、waapi2doc |
 * | `records` | `records/` 里的取证存档（在仓库里） | m1 / m2 记录复核 |
 * | `cargo` | 本机 `cargo`（会现编一个 example） | timeline-contract、check-anim-eval |
 * | `wasm-pkg` | `crates/dhampir-wasm/www/pkg` 已构建 | check-web-invariants |
 * | `ffmpeg` | 本机 ffmpeg | check-frame-pacing |
 * | `cli` | 已构建的 `dhampir` 可执行文件（+ ffmpeg） | check-cli、check-local-backend |
 * | `browser` | 真浏览器（无头 + CDP） | check-dual-end |
 *
 * **判据**：这七档必须覆盖全部守卫，且 `IN_CI` 那几档必须真的在 ci.yml 里被跑到
 * —— 见 `scripts/check-ci-guards.mjs`。
 */
export const DEPS = ['static', 'records', 'cargo', 'wasm-pkg', 'ffmpeg', 'cli', 'browser'];

/**
 * CI 该覆盖的档。**挪动这里的每一个字都要有理由**：
 * `static` / `records` 只依赖仓库里的字节；`cargo` 那两条本来就是契约判据，
 * runner 上有 rust 却没有 ffmpeg / GPU / 浏览器 / 已构建产物。
 */
export const IN_CI = ['static', 'records', 'cargo'];

/** 取 `--only-deps a,b` 或 `--only-deps=a,b` 的值；没给返回 null。 */
export function onlyDepsArg(argv) {
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    if (arg === '--only-deps') return argv[i + 1] === undefined ? '' : argv[i + 1];
    if (arg.startsWith('--only-deps=')) return arg.slice('--only-deps='.length);
  }
  return null;
}

/** 解析依赖档清单。**认不出来的一律报错**（少跑一档不能被当成"全绿"）。 */
export function parseDeps(text) {
  const problems = [];
  const tokens = String(text).split(',').map((token) => token.trim()).filter((token) => token !== '');
  if (tokens.length === 0) {
    problems.push('--only-deps 后面要跟依赖档，例：--only-deps static,records');
  }
  for (const token of tokens) {
    if (!DEPS.includes(token)) problems.push('认不出的依赖档 ' + token + '（可用：' + DEPS.join(' / ') + '）');
  }
  return { tokens, problems };
}

/** 按依赖档选守卫（返回的一定是 GUARDS 里的对象）。 */
export function guardsForDeps(tokens) {
  const wanted = new Set(tokens);
  return GUARDS.filter((guard) => wanted.has(guard.deps));
}

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
export function runOne(script, args, logDir = join(REPO_ROOT, 'target', 'guards-log')) {
  // **捕获子进程输出走文件，不走管道**（第 80 轮改）。
  //
  // 为什么：受限环境（沙箱、某些 CI 与 Windows 策略）里**建不出命名管道**，
  // 于是 `stdio: 'pipe'` 的 spawn 直接 EPERM —— 本会话就是这样，27 条守卫
  // 全被读成"起不来"，而那是**环境**的读数，不是仓库的。stdin 早就 `'ignore'`
  // 了（同一条道理），stdout/stderr 换成文件重定向之后，这条路径在两边都能走。
  //
  // 它**不是垫片**：真起不来时照样 `result.error` ⇒ code=-1 ⇒ 红。
  mkdirSync(logDir, { recursive: true });
  const outPath = join(logDir, script + '.out');
  const errPath = join(logDir, script + '.err');
  const outFd = openSync(outPath, 'w');
  const errFd = openSync(errPath, 'w');
  const result = spawnSync(process.execPath, [join(REPO_ROOT, 'scripts', script), ...args], {
    cwd: REPO_ROOT,
    windowsHide: true,
    maxBuffer: 1 << 28,
    stdio: ['ignore', outFd, errFd],
  });
  closeSync(outFd);
  closeSync(errFd);
  let text = '';
  for (const path of [outPath, errPath]) {
    try {
      text += readFileSync(path, 'utf8');
    } catch {
      // 读不到就当它没输出 —— 退出码才是结论。
    }
  }
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

/** 自检：返回 { failures, checks }。`checks` 由这里自己数，印出来的数字不会漂。 */
export function selfTestReport() {
  const failures = [];
  let checks = 0;
  const check = (name, ok, detail) => {
    checks += 1;
    if (!ok) failures.push('自检「' + name + '」不成立' + (detail === undefined ? '' : '：' + detail));
  };
  for (const testCase of SELF_TEST_CASES) {
    const got = inventoryProblems(testCase.present, testCase.registered).length;
    check(testCase.name + '（期望 ' + testCase.expect + ' 条）', got === testCase.expect, '实得 ' + got + ' 条');
  }
  // 依赖档那几条：**认不出来必须报**，否则"少跑一档"会伪装成"全绿"。
  check('认不出的依赖档要报', parseDeps('static,nope').problems.length === 1);
  check('空的 --only-deps 要报', parseDeps('').problems.length === 1);
  check('--only-deps=a,b 的等号写法要认', onlyDepsArg(['--only-deps=cargo']) === 'cargo');
  check('--only-deps a,b 的空格写法要认', onlyDepsArg(['--only-deps', 'cargo']) === 'cargo');
  const staticOnly = guardsForDeps(['static']);
  check('只选 static 选出来的必须都是 static', staticOnly.length > 0 && staticOnly.every((guard) => guard.deps === 'static'));
  check('所有依赖档加起来 = 全部守卫', guardsForDeps(DEPS).length === GUARDS.length);
  const noDeps = GUARDS.filter((guard) => !DEPS.includes(guard.deps));
  check('每条守卫都要有合法依赖档', noDeps.length === 0, noDeps.map((guard) => guard.script).join(' '));
  const ciUnknown = IN_CI.filter((dep) => !DEPS.includes(dep));
  check('IN_CI 里不许有认不出的档', ciUnknown.length === 0, ciUnknown.join(' '));
  return { failures, checks };
}

/** 老接口：只要失败列表（自检用例数见 {@link selfTestReport}）。 */
export function selfTestFailures() {
  return selfTestReport().failures;
}

function main() {
  const argv = process.argv.slice(2);
  if (argv.includes('--help')) {
    console.log('用法：node scripts/run-guards.mjs [--list] [--self-test]');
    return 0;
  }

  const report = selfTestReport();
  if (report.failures.length > 0) {
    console.error('守卫目录自检失败 —— 先修它，别信它的结论：');
    for (const failure of report.failures) console.error('  - ' + failure);
    return 2;
  }
  if (argv.includes('--self-test')) {
    console.log('✓ 守卫目录自检通过（' + report.checks + ' 条用例）');
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

  // `--only-deps`：CI 用（那边没有 ffmpeg / GPU / 浏览器 / 已构建产物）。
  // **认不出来就退 2，不许悄悄跑全部** —— 悄悄跑全部在 CI 上必红，那会让"环境"看起来像"回归"。
  const only = onlyDepsArg(argv);
  let selected = GUARDS;
  if (only !== null) {
    const { tokens, problems: depProblems } = parseDeps(only);
    if (depProblems.length > 0) {
      for (const problem of depProblems) console.error('  - ' + problem);
      return 2;
    }
    selected = guardsForDeps(tokens);
    if (selected.length === 0) {
      console.error('--only-deps ' + only + ' 一条守卫都没选中 —— 空文件集不算通过');
      return 2;
    }
  }

  if (argv.includes('--list')) {
    console.log('共 ' + GUARDS.length + ' 条守卫（与 scripts/ 对表一致；依赖档见 deps）：');
    for (const guard of GUARDS) {
      console.log('  ' + guard.script +
        (guard.args.length > 0 ? ' ' + guard.args.join(' ') : '') +
        '   [deps=' + guard.deps + ']');
    }
    console.log('CI 该覆盖的档（IN_CI）：' + IN_CI.join(' / '));
    console.log('只跑某几档：node scripts/run-guards.mjs --only-deps ' + IN_CI.join(','));
    return 0;
  }

  let bad = 0;
  const reds = [];
  for (const guard of selected) {
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
  const scope = only === null ? '全部守卫' : '依赖档 ' + only;
  console.log(bad === 0
    ? selected.length + ' / ' + selected.length + ' 条守卫全绿（' + scope + '）'
    : (selected.length - bad) + ' / ' + selected.length + ' 条全绿（' + scope + '），红的是：' + reds.join(' '));
  if (bad > 0) {
    console.log('红的这几条通常不是坏代码 —— 先按文件头那两类（甲：环境不给开 stdin 管道；' +
      '乙：安全删除按 turn 计数）逐条核过再下结论。');
  }
  return bad === 0 ? 0 : 1;
}

// 入口守卫（与 scripts/timeline-contract.mjs 同一条规矩）：
// **被 import 时不许执行 CLI** —— scripts/guards-inventory.mjs 要直接复用这份清单，
// 没有这道闸的话，它 import 一下就会跑掉整套守卫。
if (process.argv[1] !== undefined && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  // 只设 process.exitCode，不调 process.exit()——见 scripts/wasm-test-node-exit-shim.cjs 里的说明：
  // 在本机 Node/Windows 上真正执行的 process.exit() 可能撞 libuv 断言，退出码变成负数，
  // 而退出码是这份报告唯一的结论。
  process.exitCode = main();
}
