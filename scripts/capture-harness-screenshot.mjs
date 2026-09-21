#!/usr/bin/env node
// 给里程碑记录留一张**浏览器自检页的截图**，外加一份能被机器复核的旁证 JSON。
//
// 为什么截图这件事要写成脚本，而不是"打开浏览器按一下 PrintScreen"：
//
//  1. 手截图没法复跑。半年后有人要问"这张图是哪一次跑出来的、当时的结论是什么"，
//     手截图只能回答"不知道"。这里每次跑都覆盖同一个文件名，旁证 JSON 里记下
//     Chrome 版本、URL、页面状态文案、截图摘要——图能对上 JSON，JSON 能对上页面。
//  2. 用 `--screenshot` 那种"加载完就拍"的做法拍不到结论：页面要等 wasm 初始化、
//     跑完 WebGPU、POST 落盘。拍到「尚未运行」的截图和"没跑"长得一模一样。
//     所以这里走 CDP：**先 `await run()`，再拍**。
//  3. 截图会骗人。因此旁证 JSON 里同时写死这次运行的硬结论（golden 逐字节相等、
//     与 native 侧 PNG 逐字节相同、落盘成功），并且**任一条不成立就不写 PNG**——
//     宁可没有这张图，也不要一张"看起来全绿"的图。
//
// 用法：
//   node scripts/capture-harness-screenshot.mjs
//   node scripts/capture-harness-screenshot.mjs --headed      # 无头下 WebGPU 不可用时试这个
//   node scripts/capture-harness-screenshot.mjs --keep-profile # 失败时留着 Chrome profile 便于查
//   node scripts/capture-harness-screenshot.mjs --self-test
//
// 退出码：0 成功；1 页面/浏览器那边出了问题（会打印原始原因）；2 参数或环境问题。

import { spawn } from 'node:child_process';
import { createHash } from 'node:crypto';
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { createServer } from 'node:net';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');

const USAGE = `用法：node scripts/capture-harness-screenshot.mjs [选项]

选项：
  --out <dir>        记录目录（默认 records/m0）
  --width <n>       窗口宽度（默认 1400）
  --height <n>      窗口高度（默认 1000；整页更高时会整页拍下来）
  --headed          不用无头模式（有头 Chrome 也走同一套 CDP，只是能看见窗口）
  --keep-profile    结束时不删 Chrome profile 目录（排查用）
  --self-test       只跑本脚本的自检，不启动浏览器
  -h, --help        显示本帮助

环境变量：
  DHAMPIR_CHROME    指定浏览器可执行文件（默认按常见路径找 Chrome，再退到 Edge）`;

/** 找浏览器。按顺序试，命中即可——但**记下用的是哪一个**，别让截图说不清是谁拍的。 */
export function findBrowser(exists = existsSync) {
  const programFiles = process.env['ProgramFiles'] ?? 'C:\\Program Files';
  const programFilesX86 = process.env['ProgramFiles(x86)'] ?? 'C:\\Program Files (x86)';
  const localAppData = process.env['LOCALAPPDATA'] ?? '';
  const candidates = [
    ...(process.env.DHAMPIR_CHROME ? [[process.env.DHAMPIR_CHROME, 'env:DHAMPIR_CHROME']] : []),
    [join(programFiles, 'Google\\Chrome\\Application\\chrome.exe'), 'chrome'],
    [join(programFilesX86, 'Google\\Chrome\\Application\\chrome.exe'), 'chrome'],
    ...(localAppData ? [[join(localAppData, 'Google\\Chrome\\Application\\chrome.exe'), 'chrome']] : []),
    [join(programFilesX86, 'Microsoft\\Edge\\Application\\msedge.exe'), 'edge'],
    [join(programFiles, 'Microsoft\\Edge\\Application\\msedge.exe'), 'edge'],
  ];
  for (const [path, kind] of candidates) {
    if (exists(path)) return { path, kind };
  }
  return null;
}

/**
 * 从本地服务的输出里抠出 `?expect=<16 位十六进制>`。
 *
 * 为什么从输出里抠、而不是在这里再算一遍 FNV-1a：**摘要算法只能有一处真相**。
 * `serve-wasm-harness.mjs` 已经算给页面了（而且它自己那份实现被公开测试向量钉着）。
 * 这里再写一遍，就有两个地方可能在 `0xff..ff` 上分叉，而分叉的表现是
 * "两端渲染不一致"——一个假红灯。宁可解析字符串。
 */
export function parseExpectDigest(text) {
  const match = /expect=([0-9a-f]{16})\b/.exec(text);
  return match ? match[1] : null;
}

/**
 * 组装 Chrome 命令行。抽出来是为了自检能验"URL 到底有没有被传进去"。
 *
 * `about:blank` 只在没有 URL 时加：**无头模式只接受一个 target**——同时给
 * `about:blank` 和自检页，Chrome 会直接退出并打
 * "Multiple targets are not supported in headless mode."，
 * 表现出来却是"DevTools 端口一直没人监听"。这条被自检钉住了。
 */
export function buildBrowserArgs({ profileDir, url, width, height, headless, devtoolsPort }) {
  const args = [
    // 无头用 new 模式：旧无头不跑 GPU 进程，WebGPU 拿不到适配器。
    ...(headless ? ['--headless=new'] : []),
    // 端口由调用方先占一个空闲的传进来。之前这里写 `=0`（让 Chrome 自己挑），
    // 却又拿它去连**服务**的端口——两个端口混了，连的是别人的 /json/list。
    `--remote-debugging-port=${devtoolsPort}`,
    `--user-data-dir=${profileDir}`,
    `--window-size=${width},${height}`,
    '--no-first-run',
    '--no-default-browser-check',
    '--disable-extensions',
    '--disable-features=Translate,MediaRouter',
    '--hide-scrollbars',
    '--allow-insecure-localhost',
  ];
  // URL 放最后，且单独一项：它可能是唯一带 `&` 的参数，混在别处容易被引号吃掉。
  args.push(typeof url === 'string' ? url : 'about:blank');
  return args;
}

/**
 * 审这次运行的结论。返回 `{ ok, problems }`。
 *
 * 这是整个脚本的判定核心，所以抽成纯函数：截图工具最危险的失效模式是
 * "拍到了一张全绿的图，而页面其实有一节是 ✗"。页面自己也会算 `failures`，
 * 但那是页面在自证；这里**重新按字段判一遍**，两边都得同意才算。
 */
export function auditReport(report) {
  const problems = [];
  if (report === null || typeof report !== 'object') {
    return { ok: false, problems: ['页面没有返回报告对象'] };
  }
  if (report.milestone !== 'M0') problems.push(`milestone 不是 M0：${JSON.stringify(report.milestone)}`);
  if (report.probe?.golden_check_passed !== true) {
    problems.push(`golden 比对没过：${JSON.stringify(report.probe?.golden_check_error ?? '')}`);
  }
  for (const key of ['init_error', 'canvas_error', 'offscreen_error']) {
    if (report[key] !== undefined) problems.push(`${key}：${report[key]}`);
  }
  if (report.expected_png_match !== true) {
    problems.push(
      report.expected_png_fnv1a64 === undefined
        ? '没拿到 ?expect=，浏览器侧的 PNG 没跟 native 侧比过——这张图证明不了"两端一致"'
        : `浏览器侧 PNG (${report.offscreen_png?.fnv1a64}) 与 native 侧 (${report.expected_png_fnv1a64}) 不同`,
    );
  }
  if (report.sink?.ok !== true) problems.push(`落盘没成功：${JSON.stringify(report.sink ?? null)}`);
  return { ok: problems.length === 0, problems };
}

// ---------------------------------------------------------------------------
// 自检
// ---------------------------------------------------------------------------

export function selfTest() {
  const cases = [];
  const check = (name, ok) => cases.push({ name, ok });

  check('解析出服务打印的 expect 摘要', parseExpectDigest('  http://127.0.0.1:8787/?expect=85bbc2017f35dda9\n') === '85bbc2017f35dda9');
  check('没有 expect 就返回 null，不编一个', parseExpectDigest('http://127.0.0.1:8787/\n') === null);
  check('expect 长度不对不认', parseExpectDigest('?expect=85bbc2\n') === null);
  check('expect 里有非十六进制字符不认', parseExpectDigest('?expect=85bbc2017f35ddzz\n') === null);

  const args = buildBrowserArgs({ profileDir: 'P', url: 'http://x/?autorun=1', width: 1400, height: 1000, headless: true, devtoolsPort: 9222 });
  check('无头参数到位', args.includes('--headless=new'));
  check('调试端口用的是传进来的那个，不是 0', args.includes('--remote-debugging-port=9222'));
  check('URL 被传进去了', args.at(-1) === 'http://x/?autorun=1');
  check(
    '给了 URL 就不能再塞 about:blank（无头只接受一个 target）',
    args.filter((a) => a === 'about:blank' || a.startsWith('http')).length === 1,
  );
  check(
    '没给 URL 时退到 about:blank',
    buildBrowserArgs({ profileDir: 'P', url: null, width: 1, height: 1, headless: true, devtoolsPort: 9222 }).at(-1) === 'about:blank',
  );
  check(
    '有头模式不带 --headless=new',
    !buildBrowserArgs({ profileDir: 'P', url: 'u', width: 1, height: 1, headless: false, devtoolsPort: 9222 }).includes('--headless=new'),
  );

  const good = {
    milestone: 'M0',
    probe: { golden_check_passed: true },
    expected_png_fnv1a64: '85bbc2017f35dda9',
    expected_png_match: true,
    offscreen_png: { fnv1a64: '85bbc2017f35dda9' },
    sink: { ok: true },
  };
  check('全绿的报告判过', auditReport(good).ok === true);
  check('null 判红并给理由', auditReport(null).ok === false && auditReport(null).problems.length === 1);
  check('golden 没过判红', auditReport({ ...good, probe: { golden_check_passed: false } }).ok === false);
  check('canvas 路径炸了判红', auditReport({ ...good, canvas_error: 'no adapter' }).ok === false);
  check('没比过 PNG 判红（不许拿没比过的图充数）', auditReport({ ...good, expected_png_match: undefined }).ok === false);
  check('PNG 不同判红', auditReport({ ...good, expected_png_match: false }).ok === false);
  check('落盘失败判红', auditReport({ ...good, sink: { ok: false, error: 'nope' } }).ok === false);
  // 反向再钉一次：判定不是恒绿。若哪天被改成"只要拿到 report 就算过"，这里先炸。
  check('判定不是恒绿：存在至少一份输入判红', [null, { milestone: 'M0' }, { ...good, sink: {} }].some((r) => auditReport(r).ok === false));

  const browser = findBrowser(() => false);
  check('一个浏览器都找不到时返回 null，不抛异常', browser === null);

  const failed = cases.filter((c) => !c.ok);
  for (const c of cases) console.log(`  ${c.ok ? '✓' : '✗'} ${c.name}`);
  if (failed.length > 0) {
    console.error(`\n✗ 自检未通过：${failed.length} / ${cases.length}`);
    return 1;
  }
  console.log(`\n✓ 自检通过（${cases.length} 条用例）`);
  return 0;
}

// ---------------------------------------------------------------------------
// CDP
// ---------------------------------------------------------------------------

/** 一个够用的 CDP 客户端：够发命令、够收回复，不做订阅。 */
class Cdp {
  constructor(ws) {
    this.ws = ws;
    this.nextId = 1;
    this.pending = new Map();
    ws.addEventListener('message', (event) => {
      const text = typeof event.data === 'string' ? event.data : Buffer.from(event.data).toString('utf8');
      const message = JSON.parse(text);
      const slot = message.id === undefined ? undefined : this.pending.get(message.id);
      if (!slot) return;
      this.pending.delete(message.id);
      if (message.error) slot.reject(new Error(`${message.error.message}（CDP ${message.error.code}）`));
      else slot.resolve(message.result);
    });
  }

  send(method, params = {}) {
    const id = this.nextId++;
    return new Promise((resolvePromise, rejectPromise) => {
      this.pending.set(id, { resolve: resolvePromise, reject: rejectPromise });
      this.ws.send(JSON.stringify({ id, method, params }));
    });
  }

  /** 求值并取回值。页面里的异常会变成 `exceptionDetails`，这里必须当红处理。 */
  async evaluate(expression, { awaitPromise = false } = {}) {
    const result = await this.send('Runtime.evaluate', {
      expression,
      awaitPromise,
      returnByValue: true,
      allowUnsafeEvalBlockedByCSP: false,
    });
    if (result.exceptionDetails) {
      throw new Error(`页面求值抛异常：${result.exceptionDetails.exception?.description ?? result.exceptionDetails.text}`);
    }
    return result.result?.value;
  }
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function withTimeout(promise, ms, what) {
  let timer;
  try {
    return await Promise.race([
      promise,
      new Promise((_, reject) => {
        timer = setTimeout(() => reject(new Error(`等 ${what} 超时（${ms} ms）`)), ms);
      }),
    ]);
  } finally {
    clearTimeout(timer);
  }
}

/** 要一个空闲端口。写死端口在多任务机器上就是"偶尔红一次"。 */
function freePort() {
  return new Promise((resolvePromise, rejectPromise) => {
    const server = createServer();
    server.on('error', rejectPromise);
    server.listen(0, '127.0.0.1', () => {
      const { port } = server.address();
      server.close(() => resolvePromise(port));
    });
  });
}

/**
 * 等某个端口**被占上**（即等一个进程真的监听住了）。
 *
 * 这里踩过一次：原来的写法在自己能 bind 时就 resolve，也就是"端口还空着"就算通过。
 * 于是它在服务/浏览器起来之前就返回，后面连接被拒，报出来的是"fetch failed"——
 * 一个把人引向网络配置、而真相是"根本没等"的错误。名字也一起改掉了，
 * 免得下次又被 `waitForPort` 这个模棱两可的名字骗一遍。
 */
function waitForOccupiedPort(port, ms = 20000) {
  const deadline = Date.now() + ms;
  return new Promise((resolvePromise, rejectPromise) => {
    const attempt = () => {
      const socket = createServer();
      socket.once('error', () => {
        // bind 失败 = 端口被占 = 目标进程已经监听了。
        socket.close(() => resolvePromise(undefined));
      });
      socket.once('listening', () => {
        socket.close(() => {
          if (Date.now() > deadline) rejectPromise(new Error(`端口 ${port} 在 ${ms} ms 内一直没人监听`));
          else setTimeout(attempt, 150);
        });
      });
      socket.listen(port, '127.0.0.1');
    };
    attempt();
  });
}

async function fetchPageTarget(devtoolsPort, urlPrefix, ms = 30000) {
  const deadline = Date.now() + ms;
  let last = '';
  for (;;) {
    try {
      const list = await fetch(`http://127.0.0.1:${devtoolsPort}/json/list`).then((r) => r.json());
      const target = list.find((t) => t.type === 'page' && typeof t.url === 'string' && t.url.startsWith(urlPrefix));
      if (target?.webSocketDebuggerUrl) return target;
      last = `当前 page 目标：${list.filter((t) => t.type === 'page').map((t) => t.url).join('、') || '（一个都没有）'}`;
    } catch (error) {
      last = `还没能连上 DevTools：${error.message}`;
    }
    if (Date.now() > deadline) throw new Error(`没等到自检页的调试目标。${last}`);
    await sleep(250);
  }
}

/** 等 `dhampirHarness.run` 挂上去。挂上去 = 页面脚本求值完了 = `pkg/` 都在。 */
async function waitForHarness(cdp, ms = 60000) {
  const deadline = Date.now() + ms;
  let status = '';
  for (;;) {
    const ready = await cdp.evaluate('typeof globalThis.dhampirHarness?.run === "function"');
    if (ready === true) return;
    status = await cdp.evaluate('document.getElementById("status")?.textContent ?? ""');
    if (Date.now() > deadline) throw new Error(`自检页没把 dhampirHarness 挂上来。状态栏：${status}`);
    await sleep(250);
  }
}

// ---------------------------------------------------------------------------
// 入口
// ---------------------------------------------------------------------------

async function run(args) {
  const browser = findBrowser();
  if (!browser) {
    console.error('✗ 找不到 Chrome / Edge。用 DHAMPIR_CHROME=<exe> 指定一个。');
    return 2;
  }

  const outDir = resolve(REPO_ROOT, args.out);
  const expectSource = join(outDir, 'probe-native-dx12.png');
  if (!existsSync(expectSource)) {
    console.error(
      `✗ 缺少 ${expectSource.slice(REPO_ROOT.length + 1)}——没有它，浏览器侧的 PNG 就无从比对。\n` +
        '  先跑 native 探针：cargo run -p dhampir-worker --bin dhampir-render -- --probe-only --backend dx12 --out records/m0',
    );
    return 2;
  }

  const serverPort = await freePort();
  // 服务端口与 DevTools 端口必须是两个，而且**必须真的不同**：Windows 会在一个
  // 端口刚被释放后把它再发一次，于是两次 freePort() 可能吐出同一个号。撞上时
  // Chrome 绑不上调试端口、悄悄退出，报出来却是"连不上 DevTools"。
  let devtoolsPort = await freePort();
  while (devtoolsPort === serverPort) devtoolsPort = await freePort();
  const server = spawn(process.execPath, ['scripts/serve-wasm-harness.mjs', '--port', String(serverPort), '--out', args.out], {
    cwd: REPO_ROOT,
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  let serverOut = '';
  server.stdout.on('data', (chunk) => {
    serverOut += chunk.toString('utf8');
  });
  server.stderr.on('data', (chunk) => {
    serverOut += chunk.toString('utf8');
    process.stderr.write(chunk);
  });

  const profileDir = mkdtempSync(join(REPO_ROOT, 'target', 'screenshot-profile-'));
  let child;
  let ws;
  let chromeErr = '';
  try {
    await withTimeout(waitForOccupiedPort(serverPort), 20000, `本地服务监听到 ${serverPort}`);
    // 端口一 accept 就读 stdout 是个竞态：`listen` 回调里的那两行还没被数据事件送到。
    // 于是这里轮询等它出现，而不是读一次就下结论"服务没打印"。
    const expectDeadline = Date.now() + 10000;
    let expect = parseExpectDigest(serverOut);
    while (expect === null && Date.now() < expectDeadline) {
      await sleep(100);
      expect = parseExpectDigest(serverOut);
    }
    if (expect === null) {
      console.error('✗ 本地服务没有打印 ?expect= 摘要（它只在 native 探针图存在时打印）。原始输出：\n' + serverOut);
      return 2;
    }
    const url = `http://127.0.0.1:${serverPort}/?autorun=1&expect=${expect}`;
    console.log(`页面  ${url}`);
    console.log(`宿主  ${browser.kind}  ${browser.path}${args.headed ? '（有头）' : '（无头）'}`);

    child = spawn(
      browser.path,
      buildBrowserArgs({ profileDir, url, width: args.width, height: args.height, headless: !args.headed, devtoolsPort }),
      { stdio: ['ignore', 'ignore', 'pipe'] },
    );
    let chromeErrInner = '';
    child.stderr.on('data', (chunk) => {
      chromeErrInner += chunk.toString('utf8');
      chromeErr = chromeErrInner;
    });
    // 先等 DevTools 端口真的被监听住，再去问它有哪些目标。反过来的话，
    // "Chrome 没起来"和"页面还没建目标"会报同一个错，排查要多绕一圈。
    await withTimeout(waitForOccupiedPort(devtoolsPort, 30000), 32000, `Chrome 的 DevTools 端口 ${devtoolsPort}`);

    const target = await fetchPageTarget(devtoolsPort, `http://127.0.0.1:${serverPort}/`);
    ws = new WebSocket(target.webSocketDebuggerUrl);
    await withTimeout(
      new Promise((resolvePromise, rejectPromise) => {
        ws.addEventListener('open', () => resolvePromise(undefined), { once: true });
        ws.addEventListener('error', () => rejectPromise(new Error('连 DevTools WebSocket 失败')), { once: true });
      }),
      15000,
      '连 DevTools',
    );
    const cdp = new Cdp(ws);
    await cdp.send('Page.enable');
    await cdp.send('Runtime.enable');
    const version = await cdp.send('Browser.getVersion').catch(() => ({}));

    await cdp.send('Emulation.setDeviceMetricsOverride', {
      width: args.width,
      height: args.height,
      deviceScaleFactor: 1,
      mobile: false,
    });

    await waitForHarness(cdp);

    // 页面自己会在 `?autorun=1` 时跑一次。这里**不再调 run()**：跑两次就会 POST 两次，
    // 第二份记录覆盖第一份，图上和图下的结论就可能来自不同的两次运行。
    // 只等 autorun 那次结束——判据是状态栏从"等待运行"变成结论。
    const report = await withTimeout(
      cdp.evaluate(
        `(async () => {
           // autorun 已经在跑；这里只轮询状态，不重复触发。
           const deadline = Date.now() + 90000;
           for (;;) {
             const status = document.getElementById('status');
             if (status && status.className === 'ok') return { status: 'ok', text: status.textContent };
             if (status && status.className === 'bad') return { status: 'bad', text: status.textContent };
             if (Date.now() > deadline) return { status: 'timeout', text: status ? status.textContent : '(没有状态栏)' };
             await new Promise((r) => setTimeout(r, 250));
           }
         })()`,
        { awaitPromise: true },
      ),
      120000,
      '等页面跑完自检',
    );

    const status = await cdp.evaluate(
      '({ className: document.getElementById("status").className, text: document.getElementById("status").textContent })',
    );
    // 页面把结论 JSON 原样打进 <pre id="report">，这里读回来复核——不靠状态栏的措辞。
    const pageReport = await cdp.evaluate(
      '(() => { try { return JSON.parse(document.getElementById("report").textContent); } catch { return null; } })()',
    );

    const audit = auditReport(pageReport);

    if (!audit.ok) {
      console.error('✗ 这次运行有硬结论没过，**不写截图**（宁可没有图，也不要一张看着全绿的图）：');
      for (const problem of audit.problems) console.error(`  - ${problem}`);
      console.error(`  页面状态栏：${status?.text ?? '(无)'}`);
      if (chromeErr.trim()) console.error(`  浏览器 stderr：\n${chromeErr.trim()}`);
      return 1;
    }

    const metrics = await cdp.send('Page.getLayoutMetrics');
    const content = metrics.cssContentSize ?? metrics.contentSize;
    const shot = await cdp.send('Page.captureScreenshot', {
      format: 'png',
      captureBeyondViewport: true,
      clip: { x: 0, y: 0, width: Math.ceil(content.width), height: Math.ceil(content.height), scale: 1 },
    });
    const png = Buffer.from(shot.data, 'base64');
    if (!png.subarray(0, 8).equals(Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]))) {
      throw new Error('CDP 返回的截图不是 PNG（签名不对）');
    }

    const pngPath = join(outDir, 'screenshot-browser-harness.png');
    // 记录文件名写死 `-harness`：这个目录里已经有一张 256x256 的 `probe-*.png`，
    // 两张图容易混。名字要能自己说清是哪一张。
    writeFileSync(pngPath, png);

    const sidecar = {
      schema: 1,
      milestone: 'M0',
      purpose: 'M0 浏览器宿主自检页的整页截图，以及这次运行的硬结论——用于人工复核截图里的绿是不是真的。',
      captured_at: new Date().toISOString(),
      page_url: url,
      browser: {
        kind: browser.kind,
        path: browser.path,
        product: version.product ?? null,
        revision: version.revision ?? null,
        user_agent: version.userAgent ?? null,
        headless: !args.headed,
      },
      viewport: {
        width: args.width,
        height: args.height,
        captured_content: { width: content.width, height: content.height },
      },
      screenshot: {
        file: 'screenshot-browser-harness.png',
        bytes: png.length,
        sha256: createHash('sha256').update(png).digest('hex'),
      },
      status_bar: { className: status?.className ?? null, text: status?.text ?? null },
      verdicts: {
        golden_check_passed: pageReport.probe?.golden_check_passed ?? null,
        golden_digest_hex: pageReport.probe?.digest_hex ?? null,
        golden_expected_digest_hex: pageReport.probe?.golden_digest_hex ?? null,
        expected_png_fnv1a64: pageReport.expected_png_fnv1a64 ?? null,
        browser_png_fnv1a64: pageReport.offscreen_png?.fnv1a64 ?? null,
        expected_png_match: pageReport.expected_png_match ?? null,
        sink_ok: pageReport.sink?.ok ?? null,
        sink_files: pageReport.sink?.files ?? null,
      },
      audit: { ok: audit.ok, problems: audit.problems },
      notes: [
        '截图是 CDP Page.captureScreenshot 的整页结果，不是人按的 PrintScreen——同一条命令能再拍一张。',
        '页面在 ?autorun=1 下自己跑一次；本脚本不重复触发 run()，所以图上的结论与 browser-harness.json 来自同一次运行。',
        `注：页面若在 90 s 内没给出状态栏结论，run() 会以 timeout 返回并判红——本次是 ${report.status}。`,
        '`expected_png_fnv1a64` 由本地服务从 records/m0/probe-native-dx12.png 算出，页面拿它比对；两侧的 FNV-1a 各自被公开向量钉着。',
      ],
    };
    writeFileSync(join(outDir, 'screenshot-browser-harness.json'), `${JSON.stringify(sidecar, null, 2)}\n`, 'utf8');

    console.log(`✓ 自检页判定：${status.text}`);
    console.log(`  golden ${sidecar.verdicts.golden_digest_hex}（期望 ${sidecar.verdicts.golden_expected_digest_hex}）`);
    console.log(`  PNG 与 native 侧逐字节相同：${sidecar.verdicts.expected_png_match}`);
    console.log(`  截图 ${png.length} 字节 → records/m0/screenshot-browser-harness.png`);
    console.log(`  旁证 → records/m0/screenshot-browser-harness.json`);
    return 0;
  } catch (error) {
    // 失败时把两边的原始输出一并吐出来。第一版只打 error.message，于是
    // "连不上 DevTools" 这种话背后其实是"Chrome 根本没起来"——白查一轮。
    if (chromeErr.trim()) console.error(`--- 浏览器 stderr ---\n${chromeErr.trim()}`);
    if (serverOut.trim()) console.error(`--- 本地服务输出 ---\n${serverOut.trim()}`);
    throw error;
  } finally {
    try {
      ws?.close();
    } catch {
      /* 关不掉就算了，下面照样杀进程 */
    }
    if (child && child.pid) {
      // Chrome 会开出子进程；只杀父进程会留下一个还在跑 GPU 的孙子。
      spawn('taskkill', ['/pid', String(child.pid), '/T', '/F'], { stdio: 'ignore' });
    }
    server.kill();
    await sleep(400);
    if (!args.keepProfile) {
      try {
        rmSync(profileDir, { recursive: true, force: true, maxRetries: 5 });
      } catch {
        // profile 目录删不掉不该让整个记录失败——它在 target/ 下，本来就没人管。
      }
    } else {
      console.log(`  保留 profile：${profileDir}`);
    }
  }
}

function parseArgs(argv) {
  const out = { out: 'records/m0', width: 1400, height: 1000, headed: false, keepProfile: false, selfTest: false, help: false };
  const args = [...argv];
  while (args.length > 0) {
    const arg = args.shift();
    switch (arg) {
      case '--out': {
        const value = args.shift();
        if (value === undefined || value.trim() === '') {
          console.error('✗ --out 后面要跟一个目录');
          return { error: true };
        }
        out.out = value;
        break;
      }
      case '--width':
      case '--height': {
        const value = args.shift();
        if (value === undefined || !/^\d+$/.test(value) || Number(value) < 320) {
          console.error(`✗ ${arg} 后面要跟一个不小于 320 的整数`);
          return { error: true };
        }
        out[arg === '--width' ? 'width' : 'height'] = Number(value);
        break;
      }
      case '--headed':
        out.headed = true;
        break;
      case '--keep-profile':
        out.keepProfile = true;
        break;
      case '--self-test':
        out.selfTest = true;
        break;
      case '-h':
      case '--help':
        out.help = true;
        break;
      default:
        // 不认识的参数一律判死：静默忽略参数是"看起来全绿"最常见的来源。
        console.error(`✗ 不认识的参数：${arg}\n\n${USAGE}`);
        return { error: true };
    }
  }
  return out;
}

if (import.meta.url === `file://${process.argv[1].replace(/\\/g, '/')}` || process.argv[1].endsWith('capture-harness-screenshot.mjs')) {
  const args = parseArgs(process.argv.slice(2));
  if (args.error) {
    process.exitCode = 2;
  } else if (args.help) {
    console.log(USAGE);
  } else if (args.selfTest) {
    process.exitCode = selfTest();
  } else {
    // 只设 exitCode，不调 process.exit()：理由同别的守卫脚本（Windows + Node 的 libuv 断言）。
    run(args).then(
      (code) => {
        process.exitCode = code;
      },
      (error) => {
        console.error(`✗ 截图失败：${error.message}`);
        process.exitCode = 1;
      },
    );
  }
}
