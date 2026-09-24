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
import { copyFileSync, createReadStream, existsSync, mkdirSync, readFileSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { spawn, spawnSync } from 'node:child_process';
import { dirname, extname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { firstDifference, firstSubsetDifference } from './verdict-compare.mjs';

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

/**
 * 归一化矩形的容差。**判定口径的一部分，写死在这里。**
 *
 * 两端跑的是同一份 `dhampir_timeline::text_layout`（f32 起步），所以正常情况下逐位相同；
 * 留一点余地的唯一理由是数值要过一趟十进制打印/解析。
 * 1e-6 比一个字宽（font_ratio 0.055、360 行高时约 2e-2 归一化单位）小四个数量级 ——
 * 大到不会因为末位抖动误红，小到落点真错了必红。
 *
 * **放在文件顶部**：用它的函数在模块末尾，而调它们的那一段在中间 ——
 * 声明留在后面就是一处 TDZ 陷阱（第一版正是这么红的）。
 */
const SUBTITLE_TOLERANCE = 1e-6;

const MIME = {
  '.html': 'text/html; charset=utf-8',
  '.js': 'text/javascript; charset=utf-8',
  '.json': 'application/json; charset=utf-8',
  '.wasm': 'application/wasm',
  '.mp4': 'video/mp4',
  '.css': 'text/css; charset=utf-8',
  // 字幕文本。静态白名单里暂时不服务它们（页面的字幕走后端 /assets/<id>/media），
  // 但 MIME 表与 scripts/dhampir-local.mjs 那张对齐 —— 两张表不一致，
  // 今天只是少一个 content-type、明天就是"同一条路两种行为"。
  '.srt': 'text/plain; charset=utf-8',
  '.ass': 'text/plain; charset=utf-8',
  '.ssa': 'text/plain; charset=utf-8',
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
    // **把 fixture 里的字幕与弹幕摆到后端的 asset root 下。** 后端按「asset root + uri」
    // 解析素材位置（工程文件的 assets 表优先），而这份仓库里素材本来就分两处：
    // 视频在 target/s3（本机素材，默认跑法早已依赖它），字幕/弹幕在 fixtures/（跟踪目录，
    // 判定的对照半边也要读它）。一个 asset root 装不下两边，所以把这两份拷过去。
    // 不做这一步的失败形状是 `/assets/sub.srt/media` 回 404 asset_file_missing，
    // 而页面上的表现只是"字幕登记不上"—— 看起来像判定逻辑写错了。
    //
    // **弹幕也要摆**：弹幕素材在资产表里同样是 `kind: subtitle` 的一份，
    // 少摆它这一路就登记不上，而症状与"工程里没有弹幕轨"一模一样。
    for (const name of ['sample-subtitle.srt', 'sample-subtitle.ass']) {
      const fixturePath = join(REPO_ROOT, 'fixtures', name);
      if (existsSync(fixturePath)) copyFileSync(fixturePath, join(REPO_ROOT, 'target', 's3', name));
    }
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

// 查询串拼一次、三种模式共用。**手工看的时候也要带上后端参数** ——
// 只起页面不起后端（或反过来）会让人自己拼 URL，而拼错的表现是"页面能用但没连上后端"。
// --verdict <name>：让页面跑一次验收判定并**主动回传**，驱动只读后端拿结果。
// 这是 --exec 坏掉之后的替代通道（见文件头「观测」一节）。
// 它是一条**独立的流程**：与自动导出互斥 —— 两者都要抢那个「这一轮结束了」的信号。
const verdictName = valueOf('--verdict', null);
if (verdictName !== null && backendMode === null) {
  console.error('--verdict 需要后端（--local 或 --remote）：判定是页面 POST 到后端、驱动再读回来的');
  process.exit(2);
}
// 判定跑哪份工程。**按判定名给默认值，但要说出来** —— 判定名偷偷决定"在测什么"，
// 报告里就只剩一句"通过了"，看不出验的是哪一份工程。
const PROJECT_FOR_VERDICT = {
  'trim-parity': 'sample-project.doc',
  subtitle: 'sample-subtitle.doc',
  'undo-drag': 'sample-project.doc',
};
const projectId = valueOf('--project', null)
  || (verdictName !== null && PROJECT_FOR_VERDICT[verdictName] !== undefined
    ? PROJECT_FOR_VERDICT[verdictName]
    : 'sample-project.doc');
const params = [];
if (mode === 'app' && !argv.includes('--no-export') && verdictName === null) params.push('export=1');
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
// --src video|bitmap：强制源模式。**用来比较两条路的输出是否逐字节相同** ——
// 两条路都该给出同一张画面，不同就说明其中一条错了。
const srcValue = valueOf('--src', null);
if (srcValue !== null) params.push('src=' + srcValue);
if (verdictName !== null) params.push('verdict=' + encodeURIComponent(verdictName));
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
/** --exec 在页面里的执行结果（**要在杀掉浏览器之前拿到**）。 */
let execResult = null;

if (mode === 'serve') {
  console.log('（手工看：打开上面那个 URL。Ctrl+C 结束；带了 --local/--remote 时后端也已起来）');
} else {
  // 超时可调：诊断时用短超时让它**自己超时并打印现场**，而不是干等五分钟什么也看不到。
  const timeoutMs = Number(valueOf('--timeout-ms', mode === 'app' ? '300000' : '90000'));
  const profile = join(REPO_ROOT, 'target', 'web-check-profile');
  rmSync(profile, { recursive: true, force: true });
  // **换浏览器要换一整套启动参数。** --browser 让"这段代码在别的浏览器里行不行"
  // 变成一条命令，而不是把 URL 拷出去手工试 —— 后者正是这次踩坑的方式。
  const browser = valueOf('--browser', null) || findChrome();
  const isFirefox = /firefox/i.test(browser);
  if (isFirefox) {
    // 无头 Firefox 默认不开 WebGPU。写一份 user.js 把它打开 ——
    // **不改用户自己的配置**，profile 是本仓库 target/ 下的一次性目录。
    mkdirSync(profile, { recursive: true });
    writeFileSync(join(profile, 'user.js'), [
      'user_pref("dom.webgpu.enabled", true);',
      'user_pref("dom.webgpu.force-enabled", true);',
      'user_pref("gfx.webrender.all", true);',
      'user_pref("gfx.webrender.software", false);',
      'user_pref("media.hardware-video-decoding.force-enabled", true);',
    ].join(String.fromCharCode(10)) + String.fromCharCode(10), 'utf8');
  }
  const browserArgs = isFirefox
    // Firefox 没有 CDP。读不到脚印时靠页面自己的 beacon，
    // 所以这一条照样能用，只是「页面卡点」那一行会显示读不到。
    ? ['--headless', '--no-remote', '--profile', profile, url]
    : [
        '--headless=new', '--disable-gpu-sandbox', '--no-first-run', '--no-default-browser-check',
        '--user-data-dir=' + profile, '--enable-unsafe-webgpu', '--use-angle=default',
        // **把调试端口开起来**：卡住的时候要靠它读页面里的脚印，而不是等页面自己上报。
        '--remote-debugging-port=0',
        // **把页面的 console 转到 stderr** —— 没有它，页面里抛的错在外面看不到，
        // 而表现是「什么都没发生」。
        '--enable-logging=stderr', '--v=0', url,
      ];
  console.log('  浏览器：' + browser);
  const child = spawn(browser, browserArgs, { stdio: ['ignore', 'ignore', 'pipe'] });
  let stderr = '';
  child.stderr.on('data', (chunk) => { stderr += chunk; });

  const debugPort = isFirefox ? null : await readDebugPort(profile);
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
    // --exec **必须在杀掉浏览器之前跑** —— 它要连页面的调试端口。
    // （第一次写的时候放在报告那一段，那时 Chrome 已经没了，拿到的是 fetch failed。）
    const script = valueOf('--exec', null);
    if (script !== null && pageMarks !== null && pageMarks.ready === true) {
      try {
        const targets = await (await fetch('http://127.0.0.1:' + debugPort + '/json/list')).json();
        const page = targets.find((target) => target.type === 'page');
        if (page === undefined) {
          execResult = null;
        } else {
          // **两步走**：先让页面把结果存到 window 上，再读回来。
          // 一步到位（awaitPromise + returnByValue）在本机 Chrome 上给回的是 {}，
          // 而那看起来像"表达式没返回"—— 诊断工具不该有这种歧义。
          await cdpEvaluate(
            page.webSocketDebuggerUrl,
            '(async () => { window.__dshExec = await (' + script + '); return 1; })()'
          );
          execResult = await cdpEvaluate(page.webSocketDebuggerUrl, 'window.__dshExec');
        }
      } catch (error) {
        execResult = '执行失败：' + String(error && error.message ? error.message : error);
      }
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
    if (readyOnly) {
      // --exec：页面起来之后在页面里跑一段 JS 并把结果打出来。
      // **这是"能验到界面"的唯一入口** —— 没有它，前端的编辑路径就只能靠肉眼看，
      // 而"看起来点了有反应"证明不了 doc 真的变了。
      if (execResult !== null) {
        // **拿不到就直说。** 这个入口在本机 Chrome 上不稳定（awaitPromise 与
        // returnByValue 一起用时给回的是 {}），而一个"返回空对象"的诊断工具
        // 比没有更坏 —— 下一个人会以为"表达式没返回"，然后去查页面。
        const empty = execResult === undefined
          || (typeof execResult === 'object' && execResult !== null
              && Object.keys(execResult).length === 0);
        if (empty) {
          console.log('页面执行结果: （拿不到 —— 这个入口在本机 Chrome 上不可靠，别拿它下结论）');
        } else {
          const text = typeof execResult === 'string' ? execResult : JSON.stringify(execResult);
          console.log('页面执行结果: ' + text);
        }
      }
      reportReady(stderr);
    }
    else if (mode === 'probe') reportProbe();
    else if (mode === 'app') {
      if (backendMode === null) reportApp(stderr);
      // 判定回传要**在后端还活着的时候读** —— 收尾那一段会把后端杀掉。
      else if (verdictName !== null) await reportVerdict(verdictName);
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
        // awaitPromise：页面里常要 await 一次 fetch / 一次编辑操作，
        // 不开这个的话拿到的是一个 Promise 对象（序列化成 {}），看起来像"没返回"。
        params: { expression: expression, returnByValue: true, awaitPromise: true },
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

// ---------------------------------------------------------------------------
// 判定回传（页面主动送出来，驱动只读后端）
// ---------------------------------------------------------------------------
//
// **为什么不用 --exec**：CDP 的 Runtime.evaluate（awaitPromise 与 returnByValue
// 同用）在本机 Chrome 上给回空对象，而「返回空对象」和「什么都没发生」长得一样。
// 改成页面把判定 POST 到本机后端、驱动读后端：**拿不到就是没拿到，不算通过。**

/** 找 dhampir 可执行文件。找不到返回 null —— 调用方要如实报「没法对照」，不许静默跳过。 */
function findCli() {
  const explicit = valueOf('--cli', null);
  // **显式给了就用那个，不许偷偷换一个。** 静默回退会让人以为验的是 A，其实跑的是 B。
  if (explicit !== null) {
    return existsSync(explicit)
      ? { cli: explicit, error: null }
      : { cli: null, error: '显式指定的 --cli 不存在：' + explicit };
  }
  const fileName = process.platform === 'win32' ? 'dhampir.exe' : 'dhampir';
  for (const dir of ['debug', 'release']) {
    const candidate = join(REPO_ROOT, 'target', dir, fileName);
    if (existsSync(candidate)) return { cli: candidate, error: null };
  }
  return { cli: null, error: '找不到 dhampir 可执行文件（先 cargo build -p dhampir-worker --bin dhampir）' };
}

/**
 * 判据：**同一次编辑，预览与 CLI 给出的工程必须逐字段相同。**
 *
 * 从 fixture 拷一份工程给 CLI 改（同一个 op），改完与页面回传的 after 比。
 * 起点也要对一次：页面那份必须**覆盖** fixture 写下的每个字段，否则两边改的
 * 根本不是同一份工程，比出来的结论没有意义。
 */
function runCliParity(value) {
  const found = findCli();
  if (found.cli === null) {
    return { ok: false, detail: found.error + ' —— 没法对照' };
  }
  const cli = found.cli;
  const dir = join(REPO_ROOT, 'target', 'verdict');
  mkdirSync(dir, { recursive: true });
  const projectFile = join(dir, 'trim-parity.json');
  const original = readFileSync(join(REPO_ROOT, 'fixtures', 'sample-project.doc.json'), 'utf8');
  writeFileSync(projectFile, original);

  let started = null;
  try { started = JSON.parse(original); } catch (error) {
    return { ok: false, detail: 'fixture 不是 JSON：' + String(error && error.message ? error.message : error) };
  }
  const baseDifference = firstSubsetDifference(started, value.before, '$');
  if (baseDifference !== null) return { ok: false, detail: '页面那份工程与 fixture 对不上：' + baseDifference };

  const result = spawnSync(cli, ['edit', '--project', projectFile, '--write', '--op', JSON.stringify(value.op)], {
    cwd: REPO_ROOT, encoding: 'utf8',
  });
  const stderrText = String(result.stderr || '').trim();
  if (result.status !== 0) {
    return { ok: false, detail: 'CLI 编辑退出 ' + result.status + '：' + (stderrText || String(result.stdout || '').trim()) };
  }
  let after = null;
  try { after = JSON.parse(readFileSync(projectFile, 'utf8')); } catch (error) {
    return { ok: false, detail: 'CLI 写出来的工程不是 JSON：' + String(error && error.message ? error.message : error) };
  }
  const difference = firstDifference(after, value.after, '$');
  if (difference !== null) return { ok: false, detail: '同一次编辑，预览与 CLI 的工程不同：' + difference };
  return { ok: true, detail: '预览与 CLI 对同一个 op 给出逐字段相同的工程' };
}

// ---------------------------------------------------------------------------
// 拖拽 / 撤销 / 重做判定（T4.3）：页面回传四份工程，这里把它们钉在 fixture 与 CLI 上
// ---------------------------------------------------------------------------

/** 在工程里按 id 找一个元素。**不能按下标**：`move` 会把那条轨道按起点重排。 */
function findLayerById(doc, id) {
  const tracks = doc !== null && typeof doc === 'object' && doc.timeline !== undefined
    ? doc.timeline.tracks : [];
  for (const track of Array.isArray(tracks) ? tracks : []) {
    for (const layer of Array.isArray(track.layers) ? track.layers : []) {
      if (layer.id === id) return { track: track.id, layer: layer };
    }
  }
  return null;
}

/**
 * 判据（T4.3）：**落点吸到别人的边界上、撤销逐字段回到拖动前、重做原样放回来。**
 *
 * 页面回传的是四份工程（拖动前/拖动后/撤销后/重做后）与拖拽现场的九个事实。这里：
 *   1. 起点对一次：页面那份必须**覆盖** fixture 写下的每个字段（否则比的是另一份工程）；
 *   2. 吸附**真的发生了**：原始落点不是任何边界、落地必须是**别人的**一条边界、两者相差
 *      不超过吸附半径 —— 半径按页面给的总帧数/量宽**重算**，不是采信页面报的那个数；
 *   3. 用 CLI 拿同一条 `move` 复算一遍：改出来的工程与页面「拖动后」逐字段相同 ——
 *      这一条把「拖拽只生成已有的 move」从口头承诺变成可复算的事实；
 *   4. 撤销 == 拖动前、重做 == 拖动后（都是逐字段）。
 */
function runUndoDragParity(value) {
  const found = findCli();
  if (found.cli === null) return { ok: false, detail: found.error + ' —— 没法对照' };
  const drag = value.drag;
  if (drag === null || typeof drag !== 'object') {
    return { ok: false, detail: '页面没回传拖拽的现场（drag）' };
  }
  for (const field of ['layer', 'from', 'boundary', 'candidate', 'preview', 'landed',
    'snapFrames', 'end', 'trackWidth']) {
    if (typeof drag[field] !== 'number' && typeof drag[field] !== 'string') {
      return { ok: false, detail: '拖拽现场缺一项：' + field };
    }
  }
  const fixturePath = join(REPO_ROOT, 'fixtures', projectId + '.json');
  if (!existsSync(fixturePath)) return { ok: false, detail: '对照用的工程不在：' + fixturePath };
  const original = readFileSync(fixturePath, 'utf8');
  let fixture = null;
  try { fixture = JSON.parse(original); } catch (error) {
    return { ok: false, detail: 'fixture 不是 JSON：' + String(error && error.message ? error.message : error) };
  }
  const baseDifference = firstSubsetDifference(fixture, value.before, '$');
  if (baseDifference !== null) {
    return { ok: false, detail: '页面那份工程与 fixture 对不上：' + baseDifference };
  }
  const picked = findLayerById(fixture, drag.layer);
  if (picked === null) return { ok: false, detail: 'fixture 里没有这个元素：' + drag.layer };
  if (picked.layer.start !== drag.from) {
    return { ok: false, detail: '拖动前它在第 ' + picked.layer.start + ' 帧，页面说第 ' + drag.from + ' 帧' };
  }
  const boundaries = [];
  for (const track of fixture.timeline.tracks) {
    for (const layer of track.layers) {
      if (layer.id === drag.layer) continue;
      boundaries.push(layer.start, layer.end);
    }
  }
  const radius = Math.max(0, Math.round(6 * Number(drag.end) / Number(drag.trackWidth)));
  if (radius <= 0) {
    return { ok: false, detail: '按 总帧数 ' + drag.end + ' / 量宽 ' + drag.trackWidth
      + ' 重算出来的吸附半径是 0 帧 —— 这一趟没验到吸附' };
  }
  if (radius !== Number(drag.snapFrames)) {
    return { ok: false, detail: '吸附半径：页面报 ' + drag.snapFrames + ' 帧、按 总帧数/量宽 重算是 ' + radius + ' 帧' };
  }
  if (boundaries.includes(Number(drag.preview))) {
    return { ok: false, detail: '原始落点第 ' + drag.preview + ' 帧本身就是一条边界 —— 那样看不出吸附有没有生效' };
  }
  if (!boundaries.includes(Number(drag.landed))) {
    return { ok: false, detail: '落地第 ' + drag.landed + ' 帧不是别人的任何一条边界' };
  }
  if (Number(drag.landed) === Number(drag.preview)) {
    return { ok: false, detail: '落地与原始落点都是第 ' + drag.preview + ' 帧 —— 没吸附' };
  }
  if (Math.abs(Number(drag.landed) - Number(drag.preview)) > radius) {
    return { ok: false, detail: '落地离原始落点 ' + Math.abs(Number(drag.landed) - Number(drag.preview))
      + ' 帧，超过半径 ' + radius + ' 帧' };
  }
  if (Number(drag.landed) !== Number(drag.boundary)) {
    return { ok: false, detail: '页面说落地是第 ' + drag.landed + ' 帧、场景里挑的边界是第 ' + drag.boundary + ' 帧' };
  }
  if (Number(drag.landed) === Number(drag.from)) {
    return { ok: false, detail: '拖动之后起点没变（都是第 ' + drag.from + ' 帧）—— 这一拖什么也没验到' };
  }
  const after = findLayerById(value.dragged, drag.layer);
  if (after === null) return { ok: false, detail: '「拖动后」那份工程里没有 ' + drag.layer + ' 了' };
  if (after.layer.start !== Number(drag.landed)) {
    return { ok: false, detail: '拖动之后它在第 ' + after.layer.start + ' 帧，不是落地第 ' + drag.landed + ' 帧' };
  }
  if (after.layer.end - after.layer.start !== picked.layer.end - picked.layer.start) {
    return { ok: false, detail: '这一拖把长度改了：' + (picked.layer.end - picked.layer.start) + ' 帧 → '
      + (after.layer.end - after.layer.start) + ' 帧' };
  }
  // CLI 复算：同一条 move 打在同一份 fixture 上，两边必须逐字段相同。
  const dir = join(REPO_ROOT, 'target', 'verdict');
  mkdirSync(dir, { recursive: true });
  const projectFile = join(dir, 'undo-drag.json');
  writeFileSync(projectFile, original);
  const op = { op: 'move', layer: drag.layer, to: Number(drag.landed) };
  const result = spawnSync(found.cli, ['edit', '--project', projectFile, '--write', '--op', JSON.stringify(op)], {
    cwd: REPO_ROOT, encoding: 'utf8',
  });
  const stderrText = String(result.stderr || '').trim();
  if (result.status !== 0) {
    return { ok: false, detail: 'CLI 用同一条 move 复算退出 ' + result.status + '：'
      + (stderrText || String(result.stdout || '').trim()) };
  }
  let cliAfter = null;
  try { cliAfter = JSON.parse(readFileSync(projectFile, 'utf8')); } catch (error) {
    return { ok: false, detail: 'CLI 写出来的工程不是 JSON：' + String(error && error.message ? error.message : error) };
  }
  const mirror = firstDifference(cliAfter, value.dragged, '$');
  if (mirror !== null) {
    return { ok: false, detail: '同一条 move：CLI 改出来的工程与页面「拖动后」不同：' + mirror };
  }
  const undone = firstDifference(value.before, value.undone, '$');
  if (undone !== null) return { ok: false, detail: '撤销之后与拖动前不同：' + undone };
  const redone = firstDifference(value.dragged, value.redone, '$');
  if (redone !== null) return { ok: false, detail: '重做之后与拖动后不同：' + redone };
  const hints = value.hints !== null && typeof value.hints === 'object' ? value.hints : {};
  const said = ['drag', 'undo', 'redo'].every((key) => typeof hints[key] === 'string' && hints[key].length > 0)
    ? '；引擎自己给的说明：' + [hints.drag, hints.undo, hints.redo].join(' → ')
    : '';
  return {
    ok: true,
    detail: '拖 ' + drag.layer + '：原始落点第 ' + drag.preview + ' 帧（不是任何边界）→ 吸到第 '
      + drag.landed + ' 帧（别人的边界；半径 round(6 × ' + drag.end + ' / ' + drag.trackWidth + ') = '
      + radius + ' 帧）；同一条 move 由 CLI 复算后与「拖动后」逐字段相同；'
      + '撤销逐字段回到拖动前、重做原样放回来' + said,
  };
}

// ---------------------------------------------------------------------------
// 字幕判定（T2.5）：两端结构一致 + 页内墨迹自洽
// ---------------------------------------------------------------------------

/**
 * 一列数字（字色这类小数组）逐一比。**只给数组用** —— 布尔/字符串用 `!==` 直接比
 * （第一版把 `outline` 也塞进来，于是每一帧都报「形状不同（CLI true、页面 true）」，
 * 一条分不清对错的判据比没有更坏）。null 只和 null 相等。容差见文件顶部的 SUBTITLE_TOLERANCE。
 */
function compareNumberList(label, left, right) {
  if (left === null || left === undefined || right === null || right === undefined) {
    return (left === null || left === undefined) && (right === null || right === undefined)
      ? []
      : [label + '：一端没有、另一端有（CLI ' + JSON.stringify(left ?? null) + '、页面 ' + JSON.stringify(right ?? null) + '）'];
  }
  if (!Array.isArray(left) || !Array.isArray(right) || left.length !== right.length) {
    return [label + '：形状不同（CLI ' + JSON.stringify(left) + '、页面 ' + JSON.stringify(right) + '）'];
  }
  const problems = [];
  for (let index = 0; index < left.length; index += 1) {
    if (typeof left[index] !== 'number' || typeof right[index] !== 'number'
      || Math.abs(left[index] - right[index]) > SUBTITLE_TOLERANCE) {
      problems.push(label + '[' + index + ']：CLI ' + left[index] + '、页面 ' + right[index]);
    }
  }
  return problems;
}

/** 清单对清单：项数、文本（逐字节）、归一化矩形（容差内）。 */
function compareSubtitleManifest(frame, cli, manifest) {
  const where = '帧 ' + frame;
  const problems = [];
  for (const key of ['subtitle_assets', 'dropped_lines']) {
    if (cli[key] !== manifest[key]) {
      problems.push(where + '：' + key + ' 不同（CLI ' + cli[key] + '、页面 ' + manifest[key] + '）');
    }
  }
  problems.push(...compareNumberList(where + '：字色', cli.color, manifest.color));
  if (cli.outline !== manifest.outline) {
    problems.push(where + '：描边不同（CLI ' + cli.outline + '、页面 ' + manifest.outline + '）');
  }

  const left = Array.isArray(cli.items) ? cli.items : [];
  const right = Array.isArray(manifest.items) ? manifest.items : [];
  if (left.length !== right.length) {
    problems.push(where + '：项数不同（CLI ' + left.length + '、页面 ' + right.length + '）');
    return problems;
  }
  for (let index = 0; index < left.length; index += 1) {
    const label = where + ' 第 ' + index + ' 项';
    if (left[index].text !== right[index].text) {
      problems.push(label + '：文本不同（CLI ' + JSON.stringify(left[index].text)
        + '、页面 ' + JSON.stringify(right[index].text) + '）');
    }
    const a = left[index].rect;
    const b = right[index].rect;
    if (a === undefined || b === undefined) {
      problems.push(label + '：一端没有 rect');
      continue;
    }
    for (const key of ['x', 'y', 'width', 'height']) {
      if (typeof a[key] !== 'number' || typeof b[key] !== 'number') {
        problems.push(label + '：rect.' + key + ' 不是数（CLI ' + a[key] + '、页面 ' + b[key] + '）');
      } else if (Math.abs(a[key] - b[key]) > SUBTITLE_TOLERANCE) {
        problems.push(label + '：rect.' + key + ' 差 ' + Math.abs(a[key] - b[key])
          + '（CLI ' + a[key] + '、页面 ' + b[key] + '）');
      }
    }
  }
  return problems;
}

/**
 * 弹幕对弹幕：**结构逐字段比**（T3 的验收口径）。
 *
 * 比的是 `(text, lane, enter, exit)` 四样。**`lane` 一定要比**：只比这一帧的矩形的话，
 * 「泳道被分配错了」（两条换了位置）在单帧里可能完全看不出来 —— 而那正是两端最容易
 * 漂的地方（分配算法一分为二就会漂，且两边各自的表都自洽）。
 *
 * 矩形也一并比：它是 `danmaku::rect_at` 这个**时间的函数**算出来的，是同一条判据的
 * 另一半。它随帧变化，所以按容差比；而 `lane`/`enter`/`exit` 是整数，严格比。
 *
 * **丢弃数（`dropped_danmaku`）不是日志，是结论的一部分**：少的那几条看起来和
 * 「素材里就那几条」一模一样。
 */
function compareSubtitleDanmaku(frame, cli, manifest) {
  const where = '帧 ' + frame;
  const problems = [];
  if (cli.dropped_danmaku !== manifest.dropped_danmaku) {
    problems.push(where + '：丢弃的弹幕条数不同（CLI ' + cli.dropped_danmaku
      + '、页面 ' + manifest.dropped_danmaku + '）');
  }
  const left = Array.isArray(cli.danmaku) ? cli.danmaku : [];
  const right = Array.isArray(manifest.danmaku) ? manifest.danmaku : [];
  if (left.length !== right.length) {
    problems.push(where + '：弹幕条数不同（CLI ' + left.length + '、页面 ' + right.length + '）');
    return problems;
  }
  for (let index = 0; index < left.length; index += 1) {
    const label = where + ' 第 ' + index + ' 条弹幕';
    for (const key of ['lane', 'enter', 'exit']) {
      if (left[index][key] !== right[index][key]) {
        problems.push(label + '：' + key + ' 不同（CLI ' + left[index][key]
          + '、页面 ' + right[index][key] + '）');
      }
    }
    if (left[index].text !== right[index].text) {
      problems.push(label + '：文本不同（CLI ' + JSON.stringify(left[index].text)
        + '、页面 ' + JSON.stringify(right[index].text) + '）');
    }
    const a = left[index].rect;
    const b = right[index].rect;
    if (a === undefined || b === undefined) {
      problems.push(label + '：一端没有 rect');
      continue;
    }
    for (const key of ['x', 'y', 'width', 'height']) {
      if (typeof a[key] !== 'number' || typeof b[key] !== 'number') {
        problems.push(label + '：rect.' + key + ' 不是数（CLI ' + a[key] + '、页面 ' + b[key] + '）');
      } else if (Math.abs(a[key] - b[key]) > SUBTITLE_TOLERANCE) {
        problems.push(label + '：rect.' + key + ' 差 ' + Math.abs(a[key] - b[key])
          + '（CLI ' + a[key] + '、页面 ' + b[key] + '）');
      }
    }
  }
  return problems;
}

/**
 * 这份工程里有弹幕轨吗。读不了就当没有 —— **读不了本身会被别的判据抓住**
 * （CLI 那一半跑不起来、工程不存在），这里不重复报一遍。
 */
function projectHasDanmakuTrack(file) {
  try {
    const doc = JSON.parse(readFileSync(file, 'utf8'));
    const tracks = doc !== null && doc.timeline !== undefined && Array.isArray(doc.timeline.tracks)
      ? doc.timeline.tracks
      : [];
    return tracks.some((track) => track !== null && track.kind === 'danmaku');
  } catch (error) {
    return false;
  }
}

/**
 * 墨迹报告（只有宿主这一侧有）：**不是和 CLI 比，而是页内自洽**。
 *
 * 三件事：有字就得有墨迹（否则这一行贴了个空）、没有字的帧不能有墨迹
 * （两趟渲染之间除了文字不该有别的差别）、逐行减出来的像素之和必须等于整帧的
 * （不等就是两行压在一起了）。宿主自己报的问题（`subtitle_*`）也一并搬上来 ——
 * 它在报告里，不在这里复述一遍就等于没看。
 */
function compareSubtitleProbe(frame, manifest, probe) {
  const where = '帧 ' + frame;
  const problems = [];
  if (probe === null || probe === undefined || typeof probe !== 'object') {
    return [where + '：没有墨迹报告'];
  }
  const issues = Array.isArray(probe.issues) ? probe.issues : null;
  if (issues === null) problems.push(where + '：墨迹报告里没有 issues');
  else {
    for (const issue of issues) {
      problems.push(where + '：宿主报了 ' + issue.code + '（' + issue.path + '）：' + issue.message);
    }
  }
  const lines = Array.isArray(probe.lines) ? probe.lines : [];
  const placements = Array.isArray(manifest.placements) ? manifest.placements : [];
  if (lines.length !== placements.length) {
    problems.push(where + '：清单 ' + placements.length + ' 行、墨迹报告 ' + lines.length + ' 行 —— 不是同一份');
    return problems;
  }
  const ink = probe.ink;
  if (ink === null || ink === undefined || typeof ink !== 'object') {
    problems.push(where + '：墨迹报告里没有 ink');
    return problems;
  }
  if (ink.lines_overlap === true) {
    problems.push(where + '：逐行墨迹之和（' + ink.lines_pixels + '）与整帧墨迹（' + ink.pixels + '）不等 —— 两行压在一起了');
  }
  if (probe.subtitle_assets !== manifest.subtitle_assets) {
    problems.push(where + '：字幕素材数两处不同（清单 ' + manifest.subtitle_assets + '、报告 ' + probe.subtitle_assets + '）');
  }
  for (let index = 0; index < lines.length; index += 1) {
    const line = lines[index];
    const placement = placements[index];
    const label = where + ' 第 ' + index + ' 行';
    if (line.text !== placement.text) {
      problems.push(label + '：两次导出的文本不同（清单 ' + JSON.stringify(placement.text)
        + '、报告 ' + JSON.stringify(line.text) + '）—— 中间清单被重算过');
    }
    // 落点两处同源，必须逐字段相同；不同就说明这一趟判的不是那份清单。
    for (const key of ['x', 'y', 'bitmap_width', 'bitmap_height', 'font_px']) {
      if (line.placement === undefined || line.placement[key] !== placement[key]) {
        problems.push(label + '：落点 ' + key + ' 两处不同（清单 ' + placement[key]
          + '、报告 ' + (line.placement === undefined ? '没有 placement' : line.placement[key]) + '）');
      }
    }
    // 归一化矩形也是两处都有（清单一项一份、报告一行一份）。它是两端比对的那份数字，
    // 页内先自比一次：两处不同就说明中间清单被重算过。
    for (const key of ['x', 'y', 'width', 'height']) {
      const a = placement.rect === undefined ? undefined : placement.rect[key];
      const b = line.rect === undefined ? undefined : line.rect[key];
      if (typeof a !== 'number' || typeof b !== 'number' || Math.abs(a - b) > SUBTITLE_TOLERANCE) {
        problems.push(label + '：rect.' + key + ' 两处不同（清单 ' + a + '、报告 ' + b + '）');
      }
    }
    const lineInk = line.ink === undefined ? null : line.ink;
    if (placement.visible === true) {
      if (line.bitmap === null || line.bitmap === undefined) problems.push(label + '：这一行可见却没有位图');
      if (lineInk === null || lineInk.pixels === 0) problems.push(label + '：这一行可见却没有留下墨迹');
    } else if (lineInk !== null && lineInk.pixels !== 0) {
      problems.push(label + '：这一行是空白却留下了 ' + lineInk.pixels + ' 个像素');
    }
  }
  // 这一帧一个字都没有：整帧墨迹必须是 0（两趟渲染的底必须一模一样）。
  if (placements.length === 0 && ink.pixels !== 0) {
    problems.push(where + '：这一帧没有字，却读出 ' + ink.pixels + ' 个像素的墨迹');
  }
  return problems;
}

/** 一帧的实测事实（判定成立时才打出来）：几行、墨迹多少像素、位图多大落在哪；弹幕几条、丢了几条。 */
function subtitleFrameNote(frame, manifest, probe) {
  const placements = Array.isArray(manifest.placements) ? manifest.placements : [];
  const lines = probe !== null && probe !== undefined && Array.isArray(probe.lines) ? probe.lines : [];
  const per = lines.map((line) => (line.ink === undefined ? 0 : line.ink.pixels));
  const target = probe !== null && probe !== undefined && Array.isArray(probe.target)
    ? '，画布 ' + probe.target[0] + 'x' + probe.target[1]
    : '';
  const first = placements.length > 0 ? placements[0] : null;
  const at = first === null
    ? ''
    : '；头一行位图 ' + first.bitmap_width + 'x' + first.bitmap_height
      + ' @(' + first.x + ',' + first.y + ') 字号 ' + first.font_px;
  const ink = probe === null || probe === undefined || probe.ink === undefined ? '（没有报告）' : probe.ink.pixels;
  // 弹幕那一半的事实：这一帧几条、各在哪个泳道/哪几帧活着、整条素材被丢了几条。
  // **不看墨迹**：弹幕不进 probe（判定路径只判字幕行，见 compareSubtitleProbe 的说明）。
  const danmaku = Array.isArray(manifest.danmaku) ? manifest.danmaku : [];
  const lanes = danmaku.map((item) => item.text + '(泳道 ' + item.lane + ' ' + item.enter + '..' + item.exit + ')');
  const shots = danmaku.length === 0
    ? ''
    : '；弹幕 ' + danmaku.length + ' 条：' + lanes.join('、')
      + '（整条素材丢 ' + manifest.dropped_danmaku + ' 条）';
  return '帧 ' + frame + '：' + placements.length + ' 行' + target + '，墨迹 ' + ink
    + ' px（逐行 ' + per.join('/') + '）' + at + shots;
}

/**
 * 判据：**同一帧，浏览器宿主与 CLI 给出的清单必须一致**（T2.5 的验收口径；弹幕那一半是 T3.4）。
 *
 * 两端跑的是同一份 `text_layout` 与同一份 `danmaku`（泳道分配 + `rect_at`），
 * 所以这里比的是结构：项数、文本、归一化矩形、字色/描边；弹幕再多比
 * `lane`/`enter`/`exit` 与 `dropped_danmaku`。
 * **字形不在判据里** —— 浏览器那边用系统字体（sans-serif）、CLI 用 --font-file，
 * 像素本来就允许不同（plan/roadmap.md 的 T2 验收：结构一致、字形允许不同）。
 *
 * CLI 那一半读的是 fixtures/ 下这份工程（跟踪目录）—— 与页面读到的是同一个文件：
 * web-check 起本机后端之前把 fixture 里的 .srt/.ass 摆到了 target/s3 下（后端只有一个
 * asset root）。
 *
 * # 为什么还要一条「非空白」检查
 *
 * 两端都一条弹幕也没算出来时，上面每一条逐字段判据都成立 —— 而那样的结论是**白说的**。
 * 所以工程里**有**弹幕轨时，这一趟必须真的见到条目、也真的见到被丢的条目：
 * 前者证明结构那条路被走通，后者证明「丢弃数一致」这条判据不是空过。
 * 工程里没有弹幕轨时这两条不判（那种工程本来就没有这一半可验）。
 */
function runSubtitleParity(value) {
  const found = findCli();
  if (found.cli === null) return { ok: false, detail: found.error + ' —— 没法对照' };
  const manifests = Array.isArray(value.frames) ? value.frames : [];
  const probes = Array.isArray(value.probes) ? value.probes : [];
  if (manifests.length === 0) return { ok: false, detail: '页面没回传任何一帧的清单' };
  if (probes.length !== manifests.length) {
    return { ok: false, detail: '清单 ' + manifests.length + ' 帧、墨迹 ' + probes.length + ' 帧 —— 对不上' };
  }
  const projectFile = join(REPO_ROOT, 'fixtures', projectId + '.json');
  if (!existsSync(projectFile)) return { ok: false, detail: '对照用的工程不在：' + projectFile };
  const hasDanmaku = projectHasDanmakuTrack(projectFile);

  const problems = [];
  const frames = [];
  const notes = [];
  let danmakuSeen = 0;
  let danmakuDropped = 0;
  for (let index = 0; index < manifests.length; index += 1) {
    const manifest = manifests[index];
    const frame = Number(manifest.frame);
    frames.push(frame);
    const result = spawnSync(found.cli, ['subtitle', '--project', projectFile, '--frame', String(frame),
      '--asset-root', 'fixtures'], { cwd: REPO_ROOT, encoding: 'utf8' });
    const stderrText = String(result.stderr || '').trim();
    if (result.status !== 0) {
      problems.push('帧 ' + frame + '：CLI subtitle 退出 ' + result.status + '：'
        + (stderrText || String(result.stdout || '').trim()));
      continue;
    }
    let cli = null;
    try { cli = JSON.parse(result.stdout); } catch (error) {
      problems.push('帧 ' + frame + '：CLI 的输出不是 JSON：' + String(error && error.message ? error.message : error));
      continue;
    }
    problems.push(...compareSubtitleManifest(frame, cli, manifest));
    problems.push(...compareSubtitleDanmaku(frame, cli, manifest));
    problems.push(...compareSubtitleProbe(frame, manifest, probes[index]));
    notes.push(subtitleFrameNote(frame, manifest, probes[index]));
    const shots = Array.isArray(manifest.danmaku) ? manifest.danmaku.length : 0;
    danmakuSeen += shots;
    // 丢弃数按帧累加会重复计数（整条素材算一次、每帧都报同一个数），所以取最大值 ——
    // 这里要的是「有没有丢过」，不是「丢了几次」。
    danmakuDropped = Math.max(danmakuDropped, Number(manifest.dropped_danmaku) || 0);
  }
  if (hasDanmaku && danmakuSeen === 0) {
    problems.push('这份工程有弹幕轨，但这些帧里一条都没见到 —— 弹幕那一半等于什么也没验');
  }
  if (hasDanmaku && danmakuDropped === 0) {
    problems.push('这份工程有弹幕轨，但一帧都没丢过条 —— 「丢弃数一致」这条判据没被走到');
  }
  if (problems.length > 0) {
    const shown = problems.slice(0, 6);
    const more = problems.length > shown.length ? '；…还有 ' + (problems.length - shown.length) + ' 条' : '';
    return { ok: false, detail: shown.join('；') + more };
  }
  // 弹幕那一半的实测事实**写进结论**：只说"一致"的话，看不出这一趟到底验到了什么
  // （「一条都没见到」与「见到了 9 条」是同一句话）。
  const danmakuFact = hasDanmaku
    ? '；弹幕结构一致（' + danmakuSeen + ' 条·泳道/进入/离开帧 + 丢弃 ' + danmakuDropped + ' 条）'
    : '；这份工程没有弹幕轨，弹幕那一半没内容可验';
  return {
    ok: true,
    detail: '帧 ' + frames.join('/') + '：两端清单一致（文本逐字节、矩形容差 ' + SUBTITLE_TOLERANCE
      + '）' + danmakuFact
      + '；墨迹逐行自洽（可见的行都有像素、没有字的帧为 0、行间不重叠）',
    notes: notes,
  };
}

/** 读后端上的判定并下结论。**拿不到就是没拿到，不算通过。** */
async function reportVerdict(name) {
  console.log('判定回传：' + name);
  let payload = null;
  let readError = null;
  try {
    const response = await fetch(backendUrl + '/verdict?name=' + encodeURIComponent(name));
    if (!response.ok) readError = '后端 /verdict 返回 ' + response.status;
    else payload = await response.json();
  } catch (error) {
    readError = '读 /verdict 失败：' + String(error && error.message ? error.message : error);
  }
  const items = payload !== null && Array.isArray(payload.items) ? payload.items : [];
  if (items.length === 0) {
    if (readError !== null) console.log('  - ' + readError);
    console.log('  - 没拿到页面回传的判定 —— 这不是通过（通道只在单机/分离模式下通）');
    process.exitCode = 1;
    return;
  }
  const value = items[items.length - 1].value;
  if (value === null || typeof value !== 'object' || value.ok !== true) {
    const reason = value !== null && typeof value === 'object' ? value.reason : '回传的值不是对象';
    console.log('  - 页面自己说这次编辑没成立：' + String(reason));
    process.exitCode = 1;
    return;
  }
  // **按 kind 分派。** 判定名只选跑哪一份判定，判据按回传的形状走 ——
  // 加一条判定要动的是这里加一支，而不是在别的判定里加 if。
  if (value.kind === 'subtitle') {
    const parity = runSubtitleParity(value);
    console.log((parity.ok ? '  ✓ ' : '  - ') + parity.detail);
    // 成功时把每帧的实测事实一并打出来：**结论之外要有事实**，
    // 不然「✓」这一行既看不出墨迹是多少，也看不出画布是不是与契约同尺寸。
    for (const note of parity.notes || []) console.log('  · ' + note);
    if (!parity.ok) process.exitCode = 1;
    return;
  }
  if (value.kind === 'undo-drag') {
    const parity = runUndoDragParity(value);
    console.log((parity.ok ? '  ✓ ' : '  - ') + parity.detail);
    if (!parity.ok) process.exitCode = 1;
    return;
  }
  console.log('  op：' + JSON.stringify(value.op));
  const parity = runCliParity(value);
  console.log((parity.ok ? '  ✓ ' : '  - ') + parity.detail);
  if (!parity.ok) process.exitCode = 1;
}

