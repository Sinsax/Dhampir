#!/usr/bin/env node
// 缓动逐值参考：**一条命令**，起一个无头 Chrome 跑 web/easing-probe.html，
// 把页面量到的逐值结果落盘 target/easing-reference.json。
//
//   node scripts/easing-reference.mjs
//   node scripts/easing-reference.mjs --chrome <exe> --timeout-ms 60000
//   node scripts/easing-reference.mjs --check          # 不起浏览器，只按同一套判据校验已有产物
//
// # 产出的形状（**别往顶层加键**）
//
// target/easing-reference.json 的顶层就是「缓动串 -> 条目」：
//
//   { "linear": { "values": [21 个], "unclamped": [21 个] },
//     "steps(4,jump-both)": { "error": "…" } }
//
// 消费者是 crates/dhampir-timeline/src/easing.rs 里那个 ignored 测试，它**按顶层键遍历**：
// 顶层多一个"自己用的"键（ok / ua / t 之类），它就会被当成一条"浏览器认了、本仓却不认"
// 的缓动串而直接 panic。所以说明一律另放 target/easing-reference.meta.json。
//
// # 为什么要有这条命令
//
// 阶段 1 的验收是「每条缓动取控制点与浏览器的 Easing 逐值比对」
// （见 plan/web-animation-criteria.md 的 D2）。那份"浏览器的值"必须是**浏览器自己**给的：
// 手工开页面抄数又慢又容易抄错，而抄错的那一份会让下游的比对红在一个与原因无关的地方。
//
// # 起浏览器的方式与 scripts/web-check.mjs 同一套
//
// 没有任何依赖：Chrome 由 spawn 起（同一组启动开关），页面由本进程用 node:http 服务，
// 结果由页面 fetch POST 回本进程（同 scripts/preview-cache-probe.mjs 的 /verdict 那条路）。
// **不装 puppeteer / playwright**：这里只需要"起一个浏览器、收一段 JSON"，
// 多一层依赖只会多一个"到底是谁起的浏览器、谁在什么时候把它关掉"的问题。
//
// # 起不了浏览器时
//
// 明确报错 + 打印 file:// 路径与手工步骤，并且**不写任何文件**。
// 静默产出一个空文件比报错更坏：空文件会一路绿到下游，而下游拿它算出来的"误差"没有意义。

import { createServer } from 'node:http';
import { spawn } from 'node:child_process';
import { existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { tryRemove } from './safe-remove.mjs';

const HERE = dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = resolve(HERE, '..');
const PAGE = join(REPO_ROOT, 'web', 'easing-probe.html');
const DEFAULT_OUT = join(REPO_ROOT, 'target', 'easing-reference.json');
const PROFILE = join(REPO_ROOT, 'target', 'easing-probe-profile');
const POINTS = 21;

const USAGE = '用法：node scripts/easing-reference.mjs [--chrome <exe>] [--out <file>] ' +
  '[--timeout-ms N] [--check]';

/** 本脚本要求的缓动串。**参考数据里的键就是页面的名单**（没有第二份可漂移），
 * 这里再写一份是因为**判据必须独立于被测对象**：名单在页面里漏了一条时，
 * 参考数据里就是"什么都没有"（不是错，是缺），只对着数据本身看永远发现不了。
 * 两份不一致时这里会红，而不是留下"以谁为准"这种说不清的状态 —— judge 里两个方向都查。 */
const REQUIRED_EASINGS = [
  'linear',
  'ease',
  'ease-in',
  'ease-out',
  'ease-in-out',
  'step-start',
  'step-end',
  'cubic-bezier(0.25,0.1,0.25,1)',
  'cubic-bezier(0.42,0,0.58,1)',
  'cubic-bezier(0.68,-0.55,0.265,1.55)',
  'steps(1)',
  'steps(4)',
  'steps(4,start)',
  'steps(4,end)',
  'steps(4,jump-none)',
  'steps(4,jump-both)',
  // 第 37 轮加的：CSS `linear()` 断点表（本仓现在也支持了）。
  // 有它才能证明"三处（浏览器 / 本仓解析器 / 转译器透传）算的是同一条折线"。
  'linear(0, 0.25 75%, 1)',
  'linear(0, 0.5, 1)',
  'linear(0, 1.2 50%, 1)',
];

function valueOf(argv, name, fallback) {
  const index = argv.indexOf(name);
  return index >= 0 && index + 1 < argv.length ? argv[index + 1] : fallback;
}

function sleep(ms) {
  return new Promise((resolveSleep) => setTimeout(resolveSleep, ms));
}

/** 说明文件：与参考数据同目录、同前缀。**分成两份文件不是洁癖** —— 见文件头那段：
 * 参考数据的顶层只许有缓动串，多一个键就会让下游按"缓动串"去解析它。 */
function metaPathOf(outPath) {
  return outPath.endsWith('.json') ? outPath.slice(0, -5) + '.meta.json' : outPath + '.meta.json';
}

/** 找 Chrome。候选表与 scripts/web-check.mjs 那张**一致** —— 两个脚本各写一张，
 * "为什么这条命令起得来、那条起不来"就会变成每次都要重新查的事。
 * 这里返回 null 而不是抛异常：调用方要拿它去走降级用法，异常会把降级那一步跳过去。 */
function findChrome(explicit) {
  const candidates = [
    explicit,
    process.env.DHAMPIR_CHROME || null,
    'C:/Program Files/Google/Chrome/Application/chrome.exe',
    'C:/Program Files (x86)/Google/Chrome/Application/chrome.exe',
    '/usr/bin/google-chrome',
    '/usr/bin/chromium',
    '/usr/bin/chromium-browser',
  ].filter((path) => typeof path === 'string' && path.length > 0);
  for (const path of candidates) if (existsSync(path)) return path;
  return null;
}

/** 一条逐值序列：**必须**是 21 个有限数。少一个点、或中间出现 NaN，
 * 后面所有的"逐值比对"都会静默地少比一个点 —— 那是最难发现的一类漏。 */
function isSeries(value) {
  return Array.isArray(value) && value.length === POINTS &&
    value.every((entry) => typeof entry === 'number' && Number.isFinite(entry));
}

/**
 * 判据。**只返回问题清单**（空数组 = 通过），不在这里决定退出码、更不写盘 ——
 * 写盘那一句必须只在一个地方（main 末尾），否则"什么时候允许写文件"会有两种答案。
 *
 * 入参是**参考数据本身**（缓动串 -> 条目），不是页面回传的那个外层信封。
 */
function judge(table) {
  const problems = [];
  const rejected = [];
  if (table === null || typeof table !== 'object' || Array.isArray(table)) {
    return { problems: ['参考数据不是「缓动串 -> 条目」的对象'], rejected: rejected };
  }

  // t 是等距的 21 个点，**由点数推出来**，参考数据里不存它：
  // 存一份就是第二个说法，而消费者本来就是按 index/(n-1) 算的（见 easing.rs 那个 ignored 测试）。
  const t = [];
  for (let i = 0; i < POINTS; i += 1) t.push(i / (POINTS - 1));

  // 名单互查。**两个方向都要查**：少一条是"漏取"，多一条是"判据过时"，
  // 两者的处置完全不一样，而只看一边只会得到"不一致"这一句无用的话。
  const declared = Object.keys(table);
  for (const easing of REQUIRED_EASINGS) {
    if (declared.indexOf(easing) < 0) {
      problems.push('参考数据里没有「' + easing + '」——少取了一条，下游会静静地少比一条');
    }
  }
  for (const easing of declared) {
    if (REQUIRED_EASINGS.indexOf(easing) < 0) {
      problems.push('参考数据里多了一条本判据不认识的串「' + easing + '」——判据该更新了');
    }
  }

  for (const easing of REQUIRED_EASINGS) {
    const entry = table[easing];
    if (entry === undefined || entry === null) continue; // 上面已经报过"少了一条"
    if (typeof entry !== 'object' || Array.isArray(entry)) {
      problems.push(easing + '：条目不是对象');
      continue;
    }
    if (entry.error !== undefined) {
      // 浏览器拒绝的串**不是**问题：如实记下来即完成任务（伪造数值才是问题）。
      rejected.push(easing);
      continue;
    }
    if (!isSeries(entry.values)) problems.push(easing + '：values 不是 ' + POINTS + ' 个有限数');
    if (!isSeries(entry.unclamped)) problems.push(easing + '：unclamped 不是 ' + POINTS + ' 个有限数');
  }

  // 自检 1：linear 必须逐点等于 t。它判的是**取样通路通不通** ——
  // 动画没生效时 linear 会是常数 1（读回了元素底层值），currentTime 没被认时是常数 0，
  // 两种情况都被这一条抓住。有了它，"这份参考对不对"就不必靠人眼看。
  const linear = table['linear'];
  if (linear !== undefined && linear !== null && isSeries(linear.values)) {
    for (let i = 0; i < POINTS; i += 1) {
      if (!(Math.abs(linear.values[i] - t[i]) <= 1e-6)) {
        problems.push('自检失败：linear 在 t=' + t[i] + ' 处取到 ' + linear.values[i] +
          '，应当等于 t —— 取样通路不通（动画没生效？currentTime 没被认？）');
        break;
      }
    }
  }
  // 自检 2：缓动**真的被应用了**吗。这条不能省：拿到的若正好是 linear 的值，
  // 数值上看毫无异常，而它会把下游实现里所有真正的错误都判成"通过"。
  // ease-in 在中点必须明显慢于 linear，ease-out 必须明显快 —— 方向反了或没应用都会红。
  const middle = (POINTS - 1) / 2;
  const checkApplied = (easing, direction) => {
    const entry = table[easing];
    if (entry === undefined || entry === null || !isSeries(entry.values)) return;
    const value = entry.values[middle];
    if (direction < 0 ? !(value < t[middle] - 0.05) : !(value > t[middle] + 0.05)) {
      problems.push('自检失败：' + easing + ' 在 t=' + t[middle] + ' 处取到 ' + value +
        '，应当' + (direction < 0 ? '小于' : '大于') + ' ' + t[middle] + ' —— 缓动串像是没被应用');
    }
  };
  checkApplied('ease-in', -1);
  checkApplied('ease-out', 1);

  return { problems: problems, rejected: rejected };
}

/** 把结论打出来。**被浏览器拒绝的串单独列一份** —— 那是这份交付的内容之一，
 * 不能混进"通过 / 不通过"里，否则它会被当成噪音划过去。 */
function report(table, meta, problems, rejected) {
  if (table !== null && typeof table === 'object' && !Array.isArray(table)) {
    console.log('取样条数：' + Object.keys(table).length + '；每条点数：' + POINTS);
  }
  if (meta !== null && typeof meta === 'object') {
    if (typeof meta.ua === 'string') console.log('浏览器：' + meta.ua);
    // unclamped 那一列是拿 computed 的十进制字符串读回来的，**它的分辨率是量出来的**：
    // 这个数要跟着结论一起露出来，否则下游会以为那一列精确到 1e-15。
    const probes = meta.transform_read_back !== undefined &&
      Array.isArray(meta.transform_read_back.probes) ? meta.transform_read_back.probes : null;
    if (probes !== null && probes.length > 0) {
      let worst = 0;
      for (const row of probes) if (Number.isFinite(row.delta_px)) worst = Math.max(worst, Math.abs(row.delta_px));
      console.log('unclamped 那一列的读回精度：最坏差 ' + worst + ' px（换算成缓动值要除以页面里的位移量）');
    }
  }
  if (rejected.length === 0) {
    console.log('被浏览器拒绝的缓动串：无');
  } else {
    console.log('被浏览器拒绝的缓动串（参考数据里如实记为 error，没有伪造数值）：');
    for (const easing of rejected) {
      console.log('  - ' + easing + ' -> ' + JSON.stringify(table[easing].error));
    }
  }
  if (problems.length === 0) {
    console.log('✓ 判据通过');
    return;
  }
  for (const problem of problems) console.error('  - ' + problem);
  console.error('✗ 判据未通过');
}

/** 起不了浏览器时的**降级用法**。四步都要写全：只给一个 file:// 路径，
 * 拿到的人不知道下一步该把它变成什么。最后一句是承诺的一部分 —— 这一轮什么都没写。 */
function manualSteps(outPath, why) {
  console.error('');
  console.error('✗ ' + why);
  console.error('');
  console.error('降级用法（不起浏览器也能拿到这份参考）：');
  console.error('  1. 用任意浏览器打开这个页面：' + pathToFileURL(PAGE).href);
  console.error('  2. 页面算完会显示两段 JSON（也挂在 window.__easingReference / window.__easingMeta 上）：');
  console.error('     用页面上的「下载」按钮存下来，或者全选那段文本复制。');
  console.error('  3. **第一段**存成：' + outPath + '（第二段是说明，可另存 ' + metaPathOf(outPath) + '，不影响判据）');
  console.error('  4. 让本脚本按同一套判据校验它（不起浏览器）：');
  console.error('     node scripts/easing-reference.mjs --check' +
    (outPath === DEFAULT_OUT ? '' : ' --out ' + outPath));
  console.error('');
  console.error('  本次**没有**写 ' + outPath + ' —— 静默产出一个空文件比报错更坏。');
}

/** 退出码与日志像不像"进程 / 管道被限制"（**而不是**页面坏了）。
 *
 * 两个签名都是实测到的：受限环境里 Chrome 连自己的子进程都建不起来 ——
 *   mojo\public\cpp\platform\platform_channel.cc:112 Check failed: . : 拒绝访问 (0x5)   ← 命名管道建不了
 *   crashpad_client_win.cc: OpenProcess: 拒绝访问 (0x5)                                ← 进程句柄拿不到
 * 这时报一句"页面取样失败"会把人带到错的方向（去查页面）。 */
function looksLikeProcessSandbox(stderr, exitInfo) {
  const text = typeof stderr === 'string' ? stderr : '';
  if (text.indexOf('platform_channel') >= 0 || text.indexOf('crashpad') >= 0) return true;
  if (exitInfo === null) return false;
  // 0xFFFF8001 / 0x80000003：Windows 上被强行或异常终止的退出码。
  return exitInfo.code === 4294930433 || exitInfo.code === 2147483651;
}

/** 起浏览器。**先按本仓的同一套 stdio 起**（stdout 不要、stderr 留着当现场）。
 *
 * 管道建不了的环境（本机 agent 沙箱实测 EPERM，同步抛出）只换这一条路：
 * 改成 inherit，浏览器输出直接透到终端。**这不是把错误糊过去** ——
 * 浏览器真起不来的话，下面照样会超时 / 退出并报出来；这里换掉的只是
 * "输出流到哪儿"，并且如实说一句。 */
function launchBrowser(browser, args) {
  try {
    return { child: spawn(browser, args, { stdio: ['ignore', 'ignore', 'pipe'] }), captured: true };
  } catch (error) {
    if (error === null || error === undefined || error.code !== 'EPERM') throw error;
    console.log('  · 建不了 stdout/stderr 管道（EPERM）—— 改用 inherit，浏览器输出直接透到终端');
    return { child: spawn(browser, args, { stdio: ['ignore', 'ignore', 'inherit'] }), captured: false };
  }
}

/** --check：不起浏览器，按**同一套判据**校验一份已经存在的产物。
 * 手工存下来的那一份也要过这一关 —— 否则"手工路"就是一条没有判据的路。 */
function checkExisting(outPath) {
  if (!existsSync(outPath)) {
    manualSteps(outPath, '没有 ' + outPath + ' 可以校验');
    return 2;
  }
  let table = null;
  try {
    table = JSON.parse(readFileSync(outPath, 'utf8'));
  } catch (error) {
    console.error('✗ ' + outPath + ' 不是合法 JSON：' + error.message);
    return 1;
  }
  let meta = null;
  const metaPath = metaPathOf(outPath);
  if (existsSync(metaPath)) {
    try { meta = JSON.parse(readFileSync(metaPath, 'utf8')); } catch (error) { meta = null; }
  }
  const { problems, rejected } = judge(table);
  report(table, meta, problems, rejected);
  return problems.length === 0 ? 0 : 1;
}

async function main() {
  const argv = process.argv.slice(2);
  const known = new Set(['--check', '--chrome', '--out', '--timeout-ms']);
  const withValue = new Set(['--chrome', '--out', '--timeout-ms']);
  for (let i = 0; i < argv.length; i += 1) {
    if (!known.has(argv[i])) {
      // 参数先判死。喂错了还照跑，等于给出一条"永远绿"的路。
      console.error('✗ 不认识的参数：' + argv[i]);
      console.error(USAGE);
      return 2;
    }
    if (withValue.has(argv[i])) i += 1;
  }
  const outPath = valueOf(argv, '--out', DEFAULT_OUT);
  const timeoutMs = Number(valueOf(argv, '--timeout-ms', '60000'));
  if (!Number.isFinite(timeoutMs) || timeoutMs <= 0) {
    console.error('✗ --timeout-ms 要给一个正数，收到：' + JSON.stringify(valueOf(argv, '--timeout-ms', null)));
    return 2;
  }
  if (argv.includes('--check')) return checkExisting(outPath);

  if (!existsSync(PAGE)) {
    console.error('✗ 页面不在：' + PAGE);
    return 2;
  }
  const browser = findChrome(valueOf(argv, '--chrome', null));
  if (browser === null) {
    manualSteps(outPath, '找不到 Chrome（装了别的发行版的浏览器就用 --chrome <exe> 或 DHAMPIR_CHROME 指定）');
    return 2;
  }

  // **页面回传的是一个信封**（ok / table / meta）：参考数据与说明在源头就分开，
  // 后面落盘时才能一份原样交出去、一份另放。收到的第一个包就是结果，之后的不再改写。
  let posted = null;
  const server = createServer((req, res) => {
    const url = new URL(req.url, 'http://127.0.0.1');
    if (req.method === 'POST' && url.pathname === '/easing-reference') {
      const chunks = [];
      req.on('data', (chunk) => chunks.push(chunk));
      req.on('end', () => {
        const body = Buffer.concat(chunks).toString('utf8');
        if (posted !== null) { res.writeHead(204).end(); return; }
        try {
          posted = JSON.parse(body);
        } catch (error) {
          // 回传体坏了也要变成"页面报错"，不能让外面看成"什么都没收到"。
          posted = { ok: false, error: '回传体不是合法 JSON：' + error.message };
        }
        res.writeHead(204).end();
      });
      return;
    }
    // 只服务这一个页面：它是自包含的（不 import pkg/、不读素材），
    // 所以不需要 web-check.mjs 那套"整个 web/ 目录"的路由。
    if (req.method === 'GET' && (url.pathname === '/' || url.pathname === '/easing-probe.html')) {
      res.writeHead(200, { 'content-type': 'text/html; charset=utf-8', 'cache-control': 'no-store' });
      res.end(readFileSync(PAGE));
      return;
    }
    res.writeHead(404).end('not found: ' + url.pathname);
  });
  await new Promise((resolveListen) => server.listen(0, '127.0.0.1', resolveListen));
  const url = 'http://127.0.0.1:' + server.address().port + '/easing-probe.html';
  console.log('页面：' + url);
  console.log('浏览器：' + browser);

  tryRemove(PROFILE);
  const chromeArgs = [
    '--headless=new', '--disable-gpu-sandbox', '--no-first-run', '--no-default-browser-check',
    // **同一组开关**（与 scripts/web-check.mjs 一致）。本页其实不需要 WebGPU，
    // 但"起浏览器的方式只有一套"比省掉两个开关值钱：
    // 开着它就不会出现"为什么 web-check 起得来、这条起不来"这种要重新查一遍的事。
    '--user-data-dir=' + PROFILE, '--enable-unsafe-webgpu', '--use-angle=default',
    '--remote-debugging-port=0', '--enable-logging=stderr', '--v=0',
    url,
  ];
  const launched = launchBrowser(browser, chromeArgs);
  const child = launched.child;
  let stderr = launched.captured ? '' : null;
  if (launched.captured) child.stderr.on('data', (chunk) => { stderr += chunk.toString('utf8'); });
  let exitInfo = null;
  child.on('exit', (code, signal) => { exitInfo = { code: code, signal: signal }; });

  const deadline = Date.now() + timeoutMs;
  while (posted === null && exitInfo === null && Date.now() < deadline) await sleep(100);
  // 浏览器刚退出时回传可能还在路上：给 300ms，别把"差一点"报成"没有结果"。
  if (posted === null && exitInfo !== null) await sleep(300);
  try { child.kill(); } catch (error) { /* 已经没了 */ }
  server.close();

  if (posted === null) {
    const why = exitInfo !== null
      ? '浏览器在拿到结果之前就退出了（exit code ' + exitInfo.code + '，signal ' + exitInfo.signal + '）'
      : '等了 ' + timeoutMs + 'ms 也没等到页面回传结果';
    // 浏览器的输出就是现场，**不吞**：受限环境里"管道建不了"与"页面真的坏了"
    // 在终端上长得一样，只有把原文打出来才分得开。
    if (stderr === null) {
      console.error('（本机建不了管道，浏览器的输出已经直接打在上面，这里截不到第二份）');
    } else if (stderr.trim() !== '') {
      console.error('--- 浏览器输出（尾部 2000 字符）---');
      console.error(stderr.trim().slice(-2000));
    }
    if (looksLikeProcessSandbox(stderr, exitInfo)) {
      console.error('  上面 platform_channel / crashpad 加「拒绝访问 (0x5)」是**进程与管道被限制**的签名：');
      console.error('  浏览器在这台机器上起不来，与这个页面无关。');
    }
    manualSteps(outPath, why);
    return 2;
  }
  if (posted.ok === false) {
    // 页面自己把失败说清楚了：**不写文件**，也不要把它伪装成"判据未通过"。
    console.error('✗ 页面报错：' + posted.error);
    console.error('  （**没有**写 ' + outPath + '）');
    return 1;
  }

  const table = posted.table;
  const meta = posted.meta === undefined ? null : posted.meta;
  const { problems, rejected } = judge(table);
  if (problems.length > 0) {
    report(table, meta, problems, rejected);
    console.error('✗ 结果没通过判据，**没有**写 ' + outPath);
    return 1;
  }
  // 写盘只在这一处。**校验通过才写**：先写再校验，等于让一份没判过的文件短暂地存在过。
  mkdirSync(dirname(outPath), { recursive: true });
  writeFileSync(outPath, JSON.stringify(table, null, 2) + '\n', 'utf8');
  if (meta !== null) writeFileSync(metaPathOf(outPath), JSON.stringify(meta, null, 2) + '\n', 'utf8');
  console.log('✓ 已写入 ' + outPath + (meta === null ? '（没有说明段）' : ' 与 ' + metaPathOf(outPath)));
  report(table, meta, problems, rejected);
  return 0;
}

process.exitCode = await main();
