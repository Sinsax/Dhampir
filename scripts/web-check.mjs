#!/usr/bin/env node
// web/ 的静态服务器 + 程序化验收驱动。
//
// **为什么不用 Vite / webpack / pnpm**：
//   1. wasm-pack --target web 的产物本身就是 ES module，浏览器可直接 import，不需要打包；
//   2. 本机 pnpm 是坏的（WinGet shim 指向缺失的包目录）；
//   3. 少一层构建就少一层"改了没生效"——调试预览问题时这很值钱。
//
// # 四种跑法
//
//   node scripts/web-check.mjs --serve       只起服务，人工看
//   node scripts/web-check.mjs --probe       W0 验收：工程帧能不能上 canvas
//   node scripts/web-check.mjs               app 验收（**降级模式**）：逐帧导出 -> FFmpeg 编码成 mp4
//   node scripts/web-check.mjs --local       本机模式：走**产品导出路径**（提交 -> 轮询 -> 下载）
//   node scripts/web-check.mjs --remote      分离模式**代码路径**：后端在别处，URL 由宿主给
//   node scripts/web-check.mjs --synthetic N 合成源导出（双端比对用）
//
// 另有 --frames-only（只出帧，不编码不写里程碑）、--canvas WxH、--timeout-ms N。
// **--ready-only** 只验"页面启动完成"，不做任何导出 —— 页面起不来的那类问题先跑它。
//
// # 为什么 --local 与默认跑法要分开
//
// 默认那条路验的是"浏览器逐帧渲染的**帧**对不对"（sink 是驱动侧的 /frame-png）；
// --local 验的是"浏览器把工程**交给后端**、后端出片、能下载"。
// 两条路的终点都是一个 mp4，但中间完全不是一回事 —— 混在一个检查里，
// 失败时只说得出"失败了"，说不出是渲染错了还是后端没接上。
//
// # 观测：为什么这里要连 CDP
//
// --local 卡住的那几轮，页面**不抛错也不进展**：没有栈、没有消息，
// 外面只能看到"什么都没发生"。加一个 window.onerror 出口之后知道的仍然只是"没报错"。
// 所以这里再补一手**不依赖页面主动上报**的读法：连上 Chrome 的调试端口，
// 直接把页面里的 window.__dhampirMarks 读出来。卡住比失败难查，观测要先做对。

import { createServer } from 'node:http';
import { createReadStream, existsSync, mkdirSync, readFileSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { spawn, spawnSync } from 'node:child_process';
import { dirname, extname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const HERE = dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = resolve(HERE, '..');
const WEB_DIR = join(REPO_ROOT, 'web');
const PKG_DIR = join(REPO_ROOT, 'crates', 'dhampir-wasm', 'www', 'pkg');
const OUT_DIR = join(REPO_ROOT, 'target', 'export');
const FRAMES_DIR = join(OUT_DIR, 'frames');
// 里程碑文件放**非忽略目录**：它是这一段的交付物，不该躺在 target/ 里被清掉。
// **只有降级模式（PNG 逐帧）那条路会写它** —— 产品路径的产物归后端，
// 落在 target/p6/ 下，验收工具不该冒充交付物。
const VIDEO_PATH = join(REPO_ROOT, 'milestones', 'edited-milestone.mp4');
const BACKEND_EXPORT_PATH = join(REPO_ROOT, 'target', 'p6', 'webcheck-export.mp4');
const MEDIA = 'target/s3/proxy1080p.mp4';

const MIME = {
  '.html': 'text/html; charset=utf-8',
  '.js': 'text/javascript; charset=utf-8',
  '.json': 'application/json; charset=utf-8',
  '.wasm': 'application/wasm',
  '.mp4': 'video/mp4',
  '.css': 'text/css; charset=utf-8',
};

const argv = process.argv.slice(2);
const mode = argv.includes('--serve') ? 'serve' : argv.includes('--probe') ? 'probe' : 'app';
/** null / 'local' / 'remote'。'remote' 也覆盖 --remote-url（指向真实远端）。 */
const backendMode = argv.includes('--local') ? 'local'
  : (argv.includes('--remote') || argv.includes('--remote-url')) ? 'remote'
  : null;

function valueOf(name, fallback) {
  const index = argv.indexOf(name);
  return index >= 0 ? argv[index + 1] : fallback;
}

function findChrome() {
  const candidates = [
    'C:/Program Files/Google/Chrome/Application/chrome.exe',
    'C:/Program Files (x86)/Google/Chrome/Application/chrome.exe',
    '/usr/bin/google-chrome',
  ];
  for (const path of candidates) if (existsSync(path)) return path;
  throw new Error('找不到 Chrome');
}

rmSync(OUT_DIR, { recursive: true, force: true });
mkdirSync(FRAMES_DIR, { recursive: true });
mkdirSync(dirname(VIDEO_PATH), { recursive: true });

const state = {
  frames: 0, done: false, doneBody: null, failed: null,
  settle: null, diag: [], precheck: [], pageErrors: [],
};
const finished = new Promise((resolveFinished) => { state.settle = resolveFinished; });

const server = createServer((req, res) => {
  const url = new URL(req.url, 'http://127.0.0.1');
  const path = url.pathname;

  const readBody = (callback) => {
    const chunks = [];
    req.on('data', (chunk) => chunks.push(chunk));
    req.on('end', () => callback(Buffer.concat(chunks)));
  };

  if (req.method === 'POST' && path === '/frame-png') {
    const frame = Number(url.searchParams.get('frame'));
    readBody((body) => {
      writeFileSync(join(FRAMES_DIR, 'frame-' + String(frame).padStart(4, '0') + '.png'), body);
      state.frames += 1;
      res.writeHead(204).end();
    });
    return;
  }
  if (req.method === 'POST' && path === '/page-error') {
    readBody((body) => { state.pageErrors.push(body.toString()); res.writeHead(204).end(); });
    return;
  }
  if (req.method === 'POST' && path === '/precheck-result') {
    readBody((body) => { state.precheck.push(body.toString()); res.writeHead(204).end(); });
    return;
  }
  if (req.method === 'POST' && path === '/diag') {
    readBody((body) => { state.diag.push(body.toString()); res.writeHead(204).end(); });
    return;
  }
  if (req.method === 'POST' && path === '/export-done') {
    readBody((body) => {
      state.done = true;
      // **留住回执体**：产品路径的胜负全在里面（下载地址就在这一行里）。
      const text = body.toString();
      try { state.doneBody = JSON.parse(text); } catch (error) { state.doneBody = { raw: text }; }
      res.writeHead(204).end();
      state.settle();
    });
    return;
  }
  if (req.method === 'POST' && path === '/export-failed') {
    readBody((body) => {
      state.failed = body.toString();
      res.writeHead(204).end();
      state.settle();
    });
    return;
  }
  if (req.method === 'POST' && path === '/result') {
    readBody((body) => {
      state.result = body.toString();
      res.writeHead(204).end();
      if (state.settle !== null) state.settle();
    });
    return;
  }

  let file = null;
  if (path === '/' || path === '/index.html') file = join(WEB_DIR, 'index.html');
  else if (path === '/probe.html') file = join(WEB_DIR, 'probe.html');
  else if (path === '/synthetic.html') file = join(WEB_DIR, 'synthetic.html');
  else if (path.startsWith('/pkg/')) file = join(PKG_DIR, path.slice('/pkg/'.length));
  else if (path === '/sample-project.json') file = join(REPO_ROOT, 'fixtures', 'sample-project.json');
  // 工程文件形态（带资产表、v2 元素）。**页面默认要的是这一份** ——
  // 裸契约没有资产表，页面读不到 assets，多素材就无从解析。
  else if (path === '/sample-project.doc.json') file = join(REPO_ROOT, 'fixtures', 'sample-project.doc.json');
  else if (path === '/media/proxy.mp4') file = join(REPO_ROOT, MEDIA);
  // web/ 下的前端模块一律照原样服务。写死清单会在加文件时静默 404 ——
  // 而 404 的表现是「页面白屏」，不是「少一个文件」，很难查。
  else if (path === '/app.js' || path === '/engine.js' || path === '/backend.js') file = join(WEB_DIR, path.slice(1));
  else if (path.startsWith('/export/')) file = join(WEB_DIR, path.slice(1));

  if (file === null || !existsSync(file) || !statSync(file).isFile()) {
    res.writeHead(404).end('not found: ' + path);
    return;
  }
  res.writeHead(200, {
    'content-type': MIME[extname(file)] || 'application/octet-stream',
    // **不许缓存。** 这是个验收入口：改了 app.js / engine.js 之后，
    // 浏览器拿缓存里的旧版本会让"改了没生效"，而那种表现和"改错了"一模一样。
    // 手工看的时候（--serve）这一条尤其重要 —— 少一次 Ctrl+Shift+R 的猜谜。
    'cache-control': 'no-store',
  });
  createReadStream(file).pipe(res);
});

await new Promise((resolveListen) => server.listen(0, '127.0.0.1', resolveListen));
const pagePort = server.address().port;

// ---- 后端（本机/分离模式才起） ------------------------------------------------

let backendProcess = null;
let backendPort = 0;
let backendUrl = null;
// --probe 不需要后端（它只验工程帧能不能上 canvas）。其余模式都要 ——
// 包括 --serve：**手工看的时候也该自动起后端**，否则要人手拼 URL 才能用上本机模式。
if (backendMode !== null && mode !== 'probe') {
  backendUrl = valueOf('--remote-url', null);
  if (backendUrl === null) {
    backendPort = Number(valueOf('--backend-port', '8802'));
    backendProcess = spawn(process.execPath, ['scripts/dhampir-local.mjs', '--port', String(backendPort)], {
      cwd: REPO_ROOT, stdio: ['ignore', 'ignore', 'inherit'],
    });
    // unref 让这个子进程**不阻止**父进程退出。
    backendProcess.unref();
    backendUrl = 'http://127.0.0.1:' + backendPort;
    // **等后端真的开始监听再开页面** —— spawn 返回不代表端口已经能连。
    // 不等就会出现竞态：页面在 loadProject 阶段失败，而表现是「什么都没发生」。
    let ready = false;
    for (let attempt = 0; attempt < 80; attempt += 1) {
      try {
        const probe = await fetch(backendUrl + '/health');
        if (probe.ok) { ready = true; break; }
      } catch (error) { /* 还没起来，继续等 */ }
      await new Promise((resolve) => setTimeout(resolve, 100));
    }
    if (!ready) {
      console.error('本机后端在 8 秒内没起来，' + backendMode + ' 无法继续');
      process.exit(1);
    }
  }
}

const projectId = 'sample-project.doc';
// 查询串拼一次、三种模式共用。**手工看的时候也要带上后端参数** ——
// 只起页面不起后端（或反过来）会让人自己拼 URL，而拼错的表现是"页面能用但没连上后端"。
const params = [];
if (mode === 'app' && !argv.includes('--no-export')) params.push('export=1');
if (backendMode === 'local') {
  params.push('backend=local');
  params.push('port=' + backendPort);
  params.push('project=' + projectId);
} else if (backendMode === 'remote') {
  params.push('backend=remote');
  params.push('url=' + encodeURIComponent(backendUrl));
  params.push('project=' + projectId);
}
const canvasValue = argv.indexOf('--canvas') >= 0 ? argv[argv.indexOf('--canvas') + 1] : null;
if (canvasValue !== null) params.push('canvas=' + canvasValue);
const query = params.length > 0 ? '?' + params.join('&') : '';

let suffix;
if (argv.includes('--synthetic')) {
  suffix = '/synthetic.html?frames=' + (valueOf('--synthetic', '0')) + '&width=320&height=180';
} else if (mode === 'probe') {
  suffix = '/probe.html';
} else {
  suffix = '/' + query;
}
process.on('exit', () => { if (backendProcess) backendProcess.kill(); });
const url = 'http://127.0.0.1:' + pagePort + suffix;
console.log('→ ' + url);
if (backendUrl !== null) console.log('  后端：' + backendUrl + '（页面用 backend=' + backendMode + '）');

let pageMarks = null;

if (mode === 'serve') {
  console.log('（手工看：打开上面那个 URL。Ctrl+C 结束；带了 --local/--remote 时后端也已起来）');
} else {
  // 超时可调：诊断时用短超时让它**自己超时并打印现场**，而不是干等五分钟什么也看不到。
  const timeoutMs = Number(valueOf('--timeout-ms', mode === 'app' ? '300000' : '90000'));
  const profile = join(REPO_ROOT, 'target', 'web-check-profile');
  rmSync(profile, { recursive: true, force: true });
  const child = spawn(findChrome(), [
    '--headless=new', '--disable-gpu-sandbox', '--no-first-run', '--no-default-browser-check',
    '--user-data-dir=' + profile, '--enable-unsafe-webgpu', '--use-angle=default',
    // **把调试端口开起来**：卡住的时候要靠它读页面里的脚印，而不是等页面自己上报。
    '--remote-debugging-port=0',
    // **把页面的 console 转到 stderr** —— 没有它，页面里抛的错在外面看不到，
    // 而表现是「什么都没发生」。
    '--enable-logging=stderr', '--v=0', url,
  ], { stdio: ['ignore', 'ignore', 'pipe'] });
  let stderr = '';
  child.stderr.on('data', (chunk) => { stderr += chunk; });

  const debugPort = await readDebugPort(profile);
  console.log('  调试端口：' + (debugPort === null ? '（读不到 DevToolsActivePort）' : debugPort));
  const timer = setTimeout(() => { try { child.kill(); } catch (error) { /* 已经没了 */ } state.settle(); }, timeoutMs);

  const readyOnly = argv.includes('--ready-only');
  if (readyOnly) {
    // **只问一句"页面起来了没有"。**
    // 这一条不依赖任何导出路径 —— 导出跑不通的时候，最先要知道的就是
    // 页面本身到底起没起来、停在哪一步。
    const deadline = Date.now() + timeoutMs;
    while (Date.now() < deadline) {
      pageMarks = await readPageMarks(debugPort);
      if (pageMarks !== null && pageMarks.ready === true) break;
      if (state.pageErrors.some((text) => text.includes('启动失败'))) break;
      await new Promise((resolve) => setTimeout(resolve, 500));
    }
  } else {
    await finished;
  }
  clearTimeout(timer);
  // **收尾之前先把页面里的脚印读出来** —— 杀掉 Chrome 之后就读不到了。
  if (!readyOnly) pageMarks = await readPageMarks(debugPort);
  try { child.kill(); } catch (error) { /* 已经没了 */ }
  server.close();

  // **报告要在杀掉后端之前跑完。** 产品路径的最后一步是下载产物，
  // 而后端一停就下载不了 —— 上一次就是这样拿到了 ECONNRESET。
  try {
    if (readyOnly) reportReady(stderr);
    else if (mode === 'probe') reportProbe();
    else if (mode === 'app') {
      if (backendMode === null) reportApp(stderr);
      else await reportBackendExport(stderr);
    }
  } finally {
    // **跑完就杀，不能只靠 process.on('exit')** —— 子进程自己会让事件循环活着，
    // 于是 Node 永不退出、管道永不刷出，看起来像浏览器卡住。
    if (backendProcess) { backendProcess.kill(); backendProcess = null; }
  }
}

/**
 * 只验「页面启动完成」。**不依赖导出** ——
 * 页面起不来的时候导出路径根本走不到，而那时最需要知道的恰恰是"它停在哪一步"。
 */
function reportReady(browserStderr) {
  printPageSignals();
  const ready = pageMarks !== null && pageMarks.ready === true;
  if (!ready) {
    // 页面没起来时，**浏览器的输出才是第一手材料**：
    // wasm 的 panic 消息（"panicked at ..."）只打在那里，页面上只看到一句 trap。
    const dump = join(REPO_ROOT, 'target', 'p6', 'web-check-browser-stderr.txt');
    mkdirSync(dirname(dump), { recursive: true });
    writeFileSync(dump, browserStderr, 'utf8');
    console.error('✗ 页面没有进入 ready 状态（window.dhampirReady 不是 true）');
    console.error('  浏览器全量输出：' + dump);
    const panic = String(browserStderr).split(String.fromCharCode(10))
      .filter((line) => line.includes('panicked at'));
    for (const line of panic.slice(0, 5)) console.error('  ' + line.trim());
    process.exitCode = 1;
    return;
  }
  console.log('✓ 页面启动完成（ready）');
}

// ---------------------------------------------------------------------------
// CDP：不依赖页面主动上报的观测
// ---------------------------------------------------------------------------

/** 等 Chrome 把调试端口写进 user-data-dir/DevToolsActivePort。 */
async function readDebugPort(profileDir, attempts) {
  const file = join(profileDir, 'DevToolsActivePort');
  for (let attempt = 0; attempt < (attempts || 100); attempt += 1) {
    if (existsSync(file)) {
      try {
        const port = Number(readFileSync(file, 'utf8').split('\n')[0].trim());
        if (Number.isFinite(port) && port > 0) return port;
      } catch (error) { /* 还在写 */ }
    }
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  return null;
}

/** 一次 Runtime.evaluate。用 Node 内置的 WebSocket（v22+ 全局可用）。 */
function cdpEvaluate(webSocketDebuggerUrl, expression, timeoutMs) {
  return new Promise((resolvePromise) => {
    let settled = false;
    let socket = null;
    const finish = (value) => {
      if (settled) return;
      settled = true;
      try { if (socket !== null) socket.close(); } catch (error) { /* 已经关了 */ }
      resolvePromise(value);
    };
    const timer = setTimeout(() => finish(null), timeoutMs || 6000);
    try {
      socket = new WebSocket(webSocketDebuggerUrl);
    } catch (error) {
      clearTimeout(timer);
      finish(null);
      return;
    }
    socket.addEventListener('error', () => { clearTimeout(timer); finish(null); });
    socket.addEventListener('open', () => {
      socket.send(JSON.stringify({
        id: 1,
        method: 'Runtime.evaluate',
        params: { expression: expression, returnByValue: true },
      }));
    });
    socket.addEventListener('message', (event) => {
      clearTimeout(timer);
      try {
        const message = JSON.parse(String(event.data));
        const result = message.result && message.result.result ? message.result.result.value : null;
        finish(result);
      } catch (error) {
        finish(null);
      }
    });
  });
}

async function readPageMarks(debugPort) {
  if (debugPort === null) return null;
  try {
    const targets = await (await fetch('http://127.0.0.1:' + debugPort + '/json/list')).json();
    const page = targets.find((target) => target.type === 'page');
    if (!page || !page.webSocketDebuggerUrl) return null;
    const text = await cdpEvaluate(
      page.webSocketDebuggerUrl,
      'JSON.stringify({marks: window.__dhampirMarks || [], title: document.title, ready: window.dhampirReady === true})'
    );
    return text === null ? null : JSON.parse(text);
  } catch (error) {
    return null;
  }
}

/** 页面信号那一块：三种跑法都要打，**没有它"跑过"与"跳过"在外面看起来一样**。 */
function printPageSignals() {
  console.log('页面信号: ' + (state.pageErrors.length ? state.pageErrors.join(' || ') : '（无）'));
  console.log('预检回报: ' + (state.precheck.length ? state.precheck.join(' | ') : '（页面没有回报 —— 说明根本没走到预检）'));
  console.log('页面卡点: ' + describeMarks());
}

function describeMarks() {
  if (pageMarks === null) return '（CDP 读不到 —— 页面可能没起来，或调试端口没开）';
  const marks = Array.isArray(pageMarks.marks) ? pageMarks.marks : [];
  const ready = pageMarks.ready === true ? 'ready' : 'NOT-ready';
  return '[' + ready + '] ' + (marks.length ? marks.join(' -> ') : '（一条脚印都没有）');
}

// ---------------------------------------------------------------------------
// 报告
// ---------------------------------------------------------------------------

/** W0 验收：工程帧能不能上 canvas（判据见 plan 里 W0 那一段）。 */
function reportProbe() {
  if (state.result === undefined) {
    console.error('✗ 页面没有回报结果（超时？）');
    console.error('  页面卡点: ' + describeMarks());
    process.exitCode = 1;
    return;
  }
  const report = JSON.parse(state.result);
  const problems = [];
  if (report.ok !== true) problems.push('页面没报 ok=true：' + JSON.stringify(report.error));
  if (!Array.isArray(report.frames) || report.frames.length < 2) problems.push('至少要验两帧');
  else {
    const key = (row) => row.sources.map((s) => s.source + '@' + s.source_frame).sort().join(',');
    if (key(report.frames[0]) === key(report.frames[1])) problems.push('两帧选出的源完全相同，时间线没有驱动画面');
    for (const row of report.frames) if (!(row.pngLength > 0)) problems.push('第 ' + row.frame + ' 帧读回为空');
  }
  if (problems.length > 0) {
    for (const problem of problems) console.error('  - ' + problem);
    process.exitCode = 1;
    return;
  }
  console.log(JSON.stringify(report, null, 2));
  console.log('');
  console.log('✓ W0 验收通过（工程帧能上 canvas，且由时间线驱动）');
}

/** app 验收（降级模式）：逐帧导出 -> FFmpeg 编码 -> ffprobe 核对帧数 -> 出里程碑视频。 */
function reportApp(stderr) {
  printPageSignals();
  if (state.failed !== null) {
    console.error('✗ 页面报导出失败：' + state.failed);
    process.exitCode = 1;
    return;
  }
  if (!state.done) {
    console.error('✗ 等导出完成超时（收到 ' + state.frames + ' 帧）');
    console.error('--- 页面与浏览器输出（尾部 4000 字符）---');
    console.error(stderr.slice(-4000));
    process.exitCode = 1;
    return;
  }

  const expected = expectedFrames();
  console.log('收到帧数 ' + state.frames + '，期望 ' + expected);

  // 用工程的时间基定帧率：整数帧号 -> 有理数帧率 -> 编码器要的浮点，只在这一步换算。
  const fps = frameRate();
  // **--frames-only：只出帧，不编码、不写里程碑。**
  // 双端比对的编排器会调这个脚本，而验收工具**不该改动交付物** ——
  // 否则跑一次比对就把 milestones/edited-milestone.mp4 覆盖了。
  if (argv.includes('--frames-only')) {
    // **合成模式只出请求的那几帧**，所以不能按整段长度检查 ——
    // 该不该完整出片是调用方的事，这个脚本只负责把帧交出去。
    const synthetic = argv.includes('--synthetic');
    console.log('--frames-only：跳过编码与里程碑写入');
    if (!synthetic && state.frames !== expected) {
      console.error('帧数与工程长度不符');
      process.exitCode = 1;
      return;
    }
    console.log('帧已就绪（未写里程碑）');
    return;
  }
  const encode = spawnSync('ffmpeg', [
    '-y', '-framerate', String(fps),
    '-i', join(FRAMES_DIR, 'frame-%04d.png'),
    '-c:v', 'libx264', '-preset', 'veryfast', '-crf', '20', '-pix_fmt', 'yuv420p',
    '-movflags', '+faststart', VIDEO_PATH,
  ], { encoding: 'utf8' });
  if (state.diag.length > 0) console.log('诊断：' + state.diag.join(' | '));
  if (encode.status !== 0) {
    console.error('✗ FFmpeg 编码失败（exit ' + encode.status + '）');
    console.error((encode.stderr || '').slice(-800));
    process.exitCode = 1;
    return;
  }

  const info = probeVideo(VIDEO_PATH);
  const bytes = statSync(VIDEO_PATH).size;

  const problems = [];
  if (state.frames !== expected) problems.push('收到帧数 ' + state.frames + ' 与工程长度 ' + expected + ' 不符');
  if (info === null) problems.push('ffprobe 读不了产物');
  else if (info.frames !== expected) problems.push('编码后帧数 ' + info.frames + ' 与期望 ' + expected + ' 不符');
  if (!(bytes > 0)) problems.push('成片字节数为 0');

  const summary = {
    video: VIDEO_PATH.replace(REPO_ROOT + '\\', ''),
    bytes: bytes,
    frames: info === null ? null : info.frames,
    expected: expected,
    size: info === null ? null : info.width + 'x' + info.height,
    fps: info === null ? null : info.fps,
  };
  if (problems.length > 0) {
    for (const problem of problems) console.error('  - ' + problem);
    console.error(JSON.stringify(summary, null, 2));
    process.exitCode = 1;
    return;
  }
  console.log(JSON.stringify(summary, null, 2));
  console.log('');
  console.log('✓ app 验收通过：逐帧导出 -> FFmpeg 编码 -> 帧数与工程一致');
}

/**
 * 产品导出路径（本机 / 分离模式）：浏览器把工程交给后端，后端出片，页面给出下载地址。
 *
 * 这里**不经过 /frame-png** —— 那正是"走的是不是产品路径"的判据。
 */
async function reportBackendExport(stderr) {
  printPageSignals();
  const problems = [];

  if (state.failed !== null) {
    console.error('✗ 页面报导出失败：' + state.failed);
    process.exitCode = 1;
    return;
  }
  if (!state.done) {
    console.error('✗ 等导出完成超时（页面一帧都没交出来）');
    // **全量落盘再打尾部。** 只打尾部会把 panic 的头几行（真正的原因）截掉 ——
    // 这一次就是这样：栈在尾部，而"panicked at ..."在更前面。
    const dump = join(REPO_ROOT, 'target', 'p6', 'web-check-browser-stderr.txt');
    mkdirSync(dirname(dump), { recursive: true });
    writeFileSync(dump, stderr, 'utf8');
    console.error('--- 浏览器输出尾部 4000 字符（全量见 ' + dump + '）---');
    console.error(stderr.slice(-4000));
    process.exitCode = 1;
    return;
  }

  // **走的是产品路径，不是 PNG 序列。** 这一条要显式判：默认跑法的那条路
  // 也会走到 /export-done，只看到 done 是分不清两条路的。
  if (state.frames !== 0) {
    problems.push('收到 ' + state.frames + ' 张 /frame-png —— 走的是 PNG 序列而不是后端出片');
  }
  const body = state.doneBody === null ? {} : state.doneBody;
  const downloadUrl = body.download_url;
  if (typeof downloadUrl !== 'string' || downloadUrl.length === 0) {
    problems.push('页面回报里没有 download_url：' + JSON.stringify(body));
  }

  let info = null;
  let bytes = 0;
  if (typeof downloadUrl === 'string' && downloadUrl.length > 0) {
    // 下载失败**要判红，不是抛异常**：网络类失败很常见（后端没起、连接断了），
    // 而抛异常会让整个驱动崩掉，连"哪一条判据失败了"都看不到。
    try {
      const response = await fetch(downloadUrl);
      if (!response.ok) {
        problems.push('下载成片失败：HTTP ' + response.status + ' ' + downloadUrl);
      } else {
        const buffer = Buffer.from(await response.arrayBuffer());
        bytes = buffer.length;
        mkdirSync(dirname(BACKEND_EXPORT_PATH), { recursive: true });
        writeFileSync(BACKEND_EXPORT_PATH, buffer);
        info = probeVideo(BACKEND_EXPORT_PATH);
      }
    } catch (error) {
      problems.push('下载成片时连接失败：' + String(error && error.message ? error.message : error)
        + '（' + downloadUrl + '）');
    }
  }

  const expected = expectedFrames();
  const fps = frameRate();
  if (info === null) problems.push('拿不到可读的成片（下载失败或 ffprobe 读不了）');
  else {
    if (info.frames !== expected) problems.push('成片帧数 ' + info.frames + ' 与工程长度 ' + expected + ' 不符');
    // **帧数对不能证明时长对。** 这条修错过一次：90 帧被编成 60fps / 1.5 秒，
    // 而帧数一模一样。所以时长要单独判。
    const wantSeconds = (expected / fps).toFixed(6);
    if (info.seconds === null) problems.push('ffprobe 没给出时长');
    else if (info.seconds !== wantSeconds) {
      problems.push('成片时长 ' + info.seconds + ' 与期望 ' + wantSeconds + ' 秒不符');
    }
  }

  const summary = {
    mode: backendMode,
    backend: backendUrl,
    video: BACKEND_EXPORT_PATH.replace(REPO_ROOT + '\\', ''),
    bytes: bytes,
    frames: info === null ? null : info.frames,
    expected: expected,
    size: info === null ? null : info.width + 'x' + info.height,
    seconds: info === null ? null : info.seconds,
    pngFramesReceived: state.frames,
  };
  if (problems.length > 0) {
    for (const problem of problems) console.error('  - ' + problem);
    console.error(JSON.stringify(summary, null, 2));
    process.exitCode = 1;
    return;
  }
  console.log(JSON.stringify(summary, null, 2));
  console.log('');
  console.log('✓ ' + backendMode + ' 模式通过：页面提交工程 -> 后端出片 -> 下载核对形状');
  if (backendMode === 'remote') {
    // 如实划界：这条验的是**代码路径**（跨源 + 宿主给 URL），不是真实远端部署。
    console.log('  边界：后端仍在 127.0.0.1 上，只是换了个端口（因此确实是跨源）。');
    console.log('  真实远端部署要由下游验证 —— 本机没有第二台机器，这里不声称验过。');
  }
}

/** ffprobe 核对一个视频的形状。读不了返回 null。 */
function probeVideo(path) {
  const probe = spawnSync('ffprobe', [
    '-v', 'error', '-select_streams', 'v:0', '-count_frames',
    '-show_entries', 'stream=nb_read_frames,width,height,avg_frame_rate,duration',
    '-of', 'json', path,
  ], { encoding: 'utf8' });
  if (probe.status !== 0) return null;
  let stream = null;
  try { stream = JSON.parse(probe.stdout).streams[0]; } catch (error) { return null; }
  if (!stream) return null;
  const frames = Number(stream.nb_read_frames);
  // ffprobe 的 duration 是秒的十进制字符串；缺了就说缺了，不编一个。
  const seconds = typeof stream.duration === 'string' ? stream.duration : null;
  return {
    frames: Number.isFinite(frames) ? frames : null,
    width: Number(stream.width),
    height: Number(stream.height),
    fps: stream.avg_frame_rate,
    seconds: seconds,
  };
}

/** 样本工程的时间线长度 = 末位帧号 + 1（帧区间左闭右开）。 */
function expectedFrames() {
  const project = JSON.parse(readFileSync(join(REPO_ROOT, 'fixtures', 'sample-project.doc.json'), 'utf8'));
  let end = 0;
  for (const track of project.timeline.tracks) {
    if (track.kind !== 'video') continue;
    for (const layer of track.layers) end = Math.max(end, layer.end);
  }
  return end;
}

function frameRate() {
  const project = JSON.parse(readFileSync(join(REPO_ROOT, 'fixtures', 'sample-project.doc.json'), 'utf8'));
  return project.timeline.timebase.num / project.timeline.timebase.den;
}
