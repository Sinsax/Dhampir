#!/usr/bin/env node
// measure-dom-parity —— 量 DOM 宿主（浏览器自己的 CSS 合成器）与引擎之间那三条**未实测**的差异。
//
// ===========================================================================
// 这个脚本存在的理由
// ===========================================================================
//
// 台账 scripts/dom-parity-differences.toml 里有三条 status = "未实测"：它们都卡在同一件事上 ——
// **要一个真的浏览器**。没有它，那三行只能写着"知道会有差异、还没量"，而"没量过的数不许写成数"
// 是那份台账的铁规矩（判据：scripts/check-dom-differences.mjs）。
//
// ===========================================================================
// 一个必须先说清的诚实前提
// ===========================================================================
//
// DOM 宿主画的是**占位方块**：它不解码素材（web/dom-host.mjs 的注释写着"这一版不画素材本身，
// 只画一个可见的方块并写上 id"）—— 因为它的职责是"通道值 + CSS 映射"，不是逐像素复刻引擎。
//
// 所以这个量测页（web/dom-measure.html）把那些方块**改成与引擎同色的填充**：
//
//   · 引擎那一侧：一张纯色 PNG 当素材（`assets` 里的一条）；
//   · DOM 那一侧：同一个层，方块尺寸与颜色由页面的 CSS 覆盖成同一个值。
//
// **量的是"浏览器对同一几何/同一混合/同一掩码的处理"**，不是"宿主默认外观像不像引擎"。
// 这个区别写在这里，也写进台账的 note —— 否则读的人会以为"DOM 宿主已经能逐像素复刻引擎了"。
//
// ===========================================================================
// 用法
// ===========================================================================
//
//   node scripts/measure-dom-parity.mjs              # 全跑；打印读数；写 target/dom-parity/
//   node scripts/measure-dom-parity.mjs --case mask.interpolation_kernel
//   node scripts/measure-dom-parity.mjs --self-test  # 统计函数自己的用例（不需要浏览器）
//   node scripts/measure-dom-parity.mjs --keep       # 保留上次的截图与工程（默认也是保留）
//
// 退出码：0 = 三条都量到了；1 = 有一条量不到（缺 Chrome / 缺 dhampir / 页面报错）；2 = 用法错。
//
// 环境变量：CHROME_PATH 可以指定浏览器可执行文件（默认按常见路径找 Chrome / Edge）。

import { spawn, spawnSync } from 'node:child_process';
import { createServer } from 'node:http';
import { existsSync, mkdirSync, readFileSync, statSync, writeFileSync } from 'node:fs';
import { dirname, extname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { deflateSync, inflateSync } from 'node:zlib';
import { decodePng, encodePng } from './dhampir-framediff.mjs';

const SCRIPT_DIR = dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = resolve(SCRIPT_DIR, '..');
const OUT_ROOT = join(REPO_ROOT, 'target', 'dom-parity');
const WEB_DIR = join(REPO_ROOT, 'web');

// ---- 找浏览器 ---------------------------------------------------------------

export function chromeCandidates() {
  const list = [];
  if (process.env.CHROME_PATH !== undefined && process.env.CHROME_PATH !== '') list.push(process.env.CHROME_PATH);
  const programFiles = process.env['ProgramFiles'] === undefined ? 'C:\\Program Files' : process.env['ProgramFiles'];
  const programFilesX86 = process.env['ProgramFiles(x86)'] === undefined ? 'C:\\Program Files (x86)' : process.env['ProgramFiles(x86)'];
  const localAppData = process.env['LOCALAPPDATA'] === undefined ? '' : process.env['LOCALAPPDATA'];
  list.push(join(programFiles, 'Google', 'Chrome', 'Application', 'chrome.exe'));
  list.push(join(programFilesX86, 'Google', 'Chrome', 'Application', 'chrome.exe'));
  if (localAppData !== '') list.push(join(localAppData, 'Google', 'Chrome', 'Application', 'chrome.exe'));
  list.push(join(programFiles, 'Microsoft', 'Edge', 'Application', 'msedge.exe'));
  list.push(join(programFilesX86, 'Microsoft', 'Edge', 'Application', 'msedge.exe'));
  return list;
}

export function findChrome() {
  for (const path of chromeCandidates()) {
    if (existsSync(path)) return path;
  }
  return null;
}

/** 当次构建：target 下较新的那个 dhampir（与 check-old-project-baseline.mjs 同一口径）。 */
export function findDhampir() {
  const candidates = [join(REPO_ROOT, 'target', 'release', 'dhampir.exe'), join(REPO_ROOT, 'target', 'debug', 'dhampir.exe')];
  let best = null;
  for (const path of candidates) {
    if (!existsSync(path)) continue;
    const stamp = statSync(path).mtimeMs;
    if (best === null || stamp > best.stamp) best = { path, stamp };
  }
  return best === null ? null : best.path;
}

// ---- 静态服务（web/ 与本次量测目录） ----------------------------------------

const CONTENT_TYPES = {
  '.html': 'text/html; charset=utf-8',
  '.mjs': 'text/javascript; charset=utf-8',
  '.js': 'text/javascript; charset=utf-8',
  '.json': 'application/json; charset=utf-8',
  '.png': 'image/png',
  '.css': 'text/css; charset=utf-8',
};

/** 起一个只读的静态服务：/case/** 走量测目录，其余走 web/。返回 { port, close }。 */
export function startServer(caseRoot) {
  const server = createServer((request, response) => {
    let pathname = decodeURIComponent(new URL(request.url, 'http://127.0.0.1').pathname);
    let path = null;
    if (pathname.startsWith('/case/')) {
      path = join(caseRoot, pathname.slice('/case/'.length));
    } else {
      path = join(WEB_DIR, pathname === '/' ? 'dom-measure.html' : pathname.slice(1));
    }
    if (!existsSync(path) || statSync(path).isDirectory()) {
      response.writeHead(404, { 'content-type': 'text/plain; charset=utf-8' });
      response.end('404 ' + pathname);
      return;
    }
    response.writeHead(200, {
      'content-type': CONTENT_TYPES[extname(path).toLowerCase()] === undefined ? 'application/octet-stream' : CONTENT_TYPES[extname(path).toLowerCase()],
      'cache-control': 'no-store',
    });
    response.end(readFileSync(path));
  });
  return new Promise((done) => {
    server.listen(0, '127.0.0.1', () => done({ port: server.address().port, close: () => server.close() }));
  });
}

// ---- 跑两边 -----------------------------------------------------------------

/** 引擎那边：dhampir frame → 一张 PNG。 */
export function renderEngine(dhampir, project, assetRoot, outDir) {
  mkdirSync(outDir, { recursive: true });
  const result = spawnSync(dhampir, ['frame', '--project', project, '--asset-root', assetRoot, '--frame', '0', '--out', outDir], {
    encoding: 'utf8',
  });
  if (result.error !== undefined && result.error !== null) return { ok: false, why: '起不来：' + String(result.error.message) };
  if (result.status !== 0) return { ok: false, why: '退出码 ' + String(result.status) + '：' + String(result.stderr === null ? '' : result.stderr).slice(-400) };
  const png = join(outDir, 'frame-0000.png');
  if (!existsSync(png)) return { ok: false, why: '渲染报成功但没有产出 ' + png };
  return { ok: true, png };
}

// ---- CDP：等页面说 ready 之后再截图 ------------------------------------------
//
// 为什么不用 `--screenshot`（第一版就是那么写的，踩了两个坑）：
//   ① 它是"加载完就拍"：这一页要等 fetch 完工程文件、跑完 draw —— 拍到"还没画"的
//      截图和"画出来是空的"长得一模一样（capture-harness-screenshot.mjs 的头部
//      早就把这条写下来了，我第二遍又踩了一次）；
//   ② 配 `--virtual-time-budget` + 页面里有 fetch 时，它**不会自己退出** ——
//      实测 spawnSync 直接 ETIMEDOUT（120 秒）。
// 所以走 CDP：navigate → 轮询 `window.__measureReady` → `Page.captureScreenshot`。

function sleep(ms) {
  return new Promise((done) => setTimeout(done, ms));
}

export function freePort() {
  return new Promise((done, fail) => {
    const server = createServer();
    server.on('error', fail);
    server.listen(0, '127.0.0.1', () => {
      const port = server.address().port;
      server.close(() => done(port));
    });
  });
}

/** 等端口**被占上**（不是"我还能 bind"）—— 后者会在浏览器起来之前就返回。 */
export function waitForOccupiedPort(port, ms = 30000) {
  const deadline = Date.now() + ms;
  return new Promise((done, fail) => {
    const attempt = () => {
      const socket = createServer();
      socket.once('error', () => socket.close(() => done(undefined)));
      socket.once('listening', () => socket.close(() => {
        if (Date.now() > deadline) fail(new Error('端口 ' + String(port) + ' 在 ' + String(ms) + ' ms 内一直没人监听'));
        else setTimeout(attempt, 150);
      }));
      socket.listen(port, '127.0.0.1');
    };
    attempt();
  });
}

export async function fetchPageTarget(devtoolsPort, ms = 30000) {
  const deadline = Date.now() + ms;
  let last = '';
  for (;;) {
    try {
      const list = await fetch('http://127.0.0.1:' + String(devtoolsPort) + '/json/list').then((response) => response.json());
      const target = list.find((item) => item.type === 'page' && typeof item.webSocketDebuggerUrl === 'string');
      if (target !== undefined) return target;
      last = '一个 page 目标都没有';
    } catch (error) {
      last = '还没连上 DevTools：' + String(error.message);
    }
    if (Date.now() > deadline) throw new Error('没等到调试目标。' + last);
    await sleep(250);
  }
}

export function connectCdp(wsUrl) {
  return new Promise((done, fail) => {
    const ws = new WebSocket(wsUrl);
    let nextId = 1;
    const pending = new Map();
    const send = (method, params) => new Promise((resolvePromise, rejectPromise) => {
      const id = nextId;
      nextId += 1;
      pending.set(id, { resolve: resolvePromise, reject: rejectPromise });
      ws.send(JSON.stringify({ id: id, method: method, params: params === undefined ? {} : params }));
    });
    const deliver = (text) => {
      let message = null;
      try {
        message = JSON.parse(text);
      } catch {
        return;
      }
      if (message.id === undefined || !pending.has(message.id)) return;
      const slot = pending.get(message.id);
      pending.delete(message.id);
      if (message.error !== undefined) slot.reject(new Error(JSON.stringify(message.error)));
      else slot.resolve(message.result);
    };
    ws.addEventListener('message', (event) => {
      if (typeof event.data === 'string') deliver(event.data);
      else Promise.resolve(event.data.text()).then(deliver);
    });
    ws.addEventListener('error', () => fail(new Error('连 DevTools WebSocket 失败')), { once: true });
    ws.addEventListener('open', () => {
      done({
        send: send,
        close: () => ws.close(),
        evaluate: async (expression) => {
          const result = await send('Runtime.evaluate', { expression: expression, returnByValue: true, awaitPromise: true });
          return result.result === undefined ? undefined : result.result.value;
        },
      });
    });
  });
}

/** 浏览器那边：开一个 headless、navigate、**等页面说 ready**、按视口截一张 PNG。 */
export async function screenshotDom(chrome, url, width, height, outPng, profileDir) {
  const port = await freePort();
  const child = spawn(chrome, [
    '--headless=new',
    '--disable-extensions',
    '--no-first-run',
    '--no-default-browser-check',
    '--hide-scrollbars',
    '--force-device-scale-factor=1',
    '--window-size=' + String(width) + ',' + String(height),
    '--remote-debugging-port=' + String(port),
    '--user-data-dir=' + profileDir,
    'about:blank',
  ], { stdio: 'ignore' });
  let cdp = null;
  try {
    await waitForOccupiedPort(port);
    const target = await fetchPageTarget(port);
    cdp = await connectCdp(target.webSocketDebuggerUrl);
    await cdp.send('Page.enable');
    // 视口尺寸由这里说了算（窗口边框、DPI 都不再影响"截图 = 画布"这件事）。
    await cdp.send('Emulation.setDeviceMetricsOverride', { width: width, height: height, deviceScaleFactor: 1, mobile: false });
    await cdp.send('Page.navigate', { url: url });
    const deadline = Date.now() + 60000;
    for (;;) {
      const state = await cdp.evaluate('globalThis.__measureReady === true ? "ready" : (globalThis.__measureError === undefined ? "wait" : "error:" + globalThis.__measureError)');
      if (state === 'ready') break;
      if (typeof state === 'string' && state.startsWith('error:')) throw new Error('页面报错：' + state.slice(6));
      if (Date.now() > deadline) throw new Error('页面 60 秒没有 ready（文档标题：' + String(await cdp.evaluate('document.title')) + '）');
      await sleep(150);
    }
    const shot = await cdp.send('Page.captureScreenshot', { format: 'png', fromSurface: true, captureBeyondViewport: false });
    writeFileSync(outPng, Buffer.from(shot.data, 'base64'));
    if (!existsSync(outPng)) throw new Error('截图没落盘：' + outPng);
    await cdp.send('Browser.close').catch(() => {});
    return { ok: true, png: outPng };
  } catch (error) {
    // 第一版这里只打 `error.message`，结果三次全报 "undefined" —— 抓到的不是 Error
    // （CDP 那条路上有可能抛出别的东西）。所以把**能看到的东西全打出来**。
    const detail = error === null || error === undefined
      ? '未知错误（抛出来的是 ' + String(error) + '）'
      : (String(error.message) === 'undefined' ? String(error) : String(error.message));
    const stack = error !== null && error !== undefined && typeof error.stack === 'string' ? '\n' + error.stack.split('\n').slice(0, 4).join('\n') : '';
    return { ok: false, why: detail + stack };
  } finally {
    if (cdp !== null) cdp.close();
    await sleep(200);
    try {
      child.kill();
    } catch {
      // 已经退了
    }
  }
}

/**
 * 解**浏览器截图**那种 PNG（8 位、可能是 RGBA 也可能是**没有 alpha 通道的 RGB**）。
 *
 * 为什么不去改 `dhampir-framediff.mjs` 的解码器：它**故意**只认 8 位 RGBA 非隔行
 * （自检里钉着"拒绝非 RGBA"），那是它自己的口径 —— 它的输入是引擎产出的 PNG，
 * 不该为"浏览器截图"这种旁支放宽。这里只补 Chrome 那一种形态，只在本脚本里用。
 *
 * （第一次跑就是在这里红的：Chrome 截出来的 `colorType=2`，而 framediff 的解码器
 * 明确拒绝它 —— 那个拒绝是对的，缺的是这一侧。）
 */
export function decodeBrowserPng(bytes) {
  const signature = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);
  if (bytes.length < 8 || !bytes.subarray(0, 8).equals(signature)) throw new Error('不是 PNG 签名');
  let width = 0;
  let height = 0;
  let bitDepth = 0;
  let colorType = 0;
  let interlace = 0;
  const idat = [];
  let offset = 8;
  while (offset + 8 <= bytes.length) {
    const length = bytes.readUInt32BE(offset);
    const type = bytes.toString('latin1', offset + 4, offset + 8);
    const data = bytes.subarray(offset + 8, offset + 8 + length);
    if (type === 'IHDR') {
      width = data.readUInt32BE(0);
      height = data.readUInt32BE(4);
      bitDepth = data[8];
      colorType = data[9];
      interlace = data[12];
    } else if (type === 'IDAT') {
      idat.push(Buffer.from(data));
    } else if (type === 'IEND') {
      break;
    }
    offset += 12 + length;
  }
  if (bitDepth !== 8 || interlace !== 0 || (colorType !== 2 && colorType !== 6)) {
    throw new Error('浏览器截图解码器只认 8 位 RGB/RGBA 非隔行，这张是 bitDepth=' + String(bitDepth) + ' colorType=' + String(colorType) + ' interlace=' + String(interlace));
  }
  const channels = colorType === 6 ? 4 : 3;
  const raw = inflateSync(Buffer.concat(idat));
  const stride = width * channels;
  const pixels = Buffer.alloc(width * height * 4);
  let previous = Buffer.alloc(stride);
  let position = 0;
  for (let y = 0; y < height; y += 1) {
    const filter = raw[position];
    position += 1;
    const line = Buffer.from(raw.subarray(position, position + stride));
    position += stride;
    for (let i = 0; i < stride; i += 1) {
      const left = i >= channels ? line[i - channels] : 0;
      const up = previous[i];
      const upLeft = i >= channels ? previous[i - channels] : 0;
      let value = line[i];
      if (filter === 1) value += left;
      else if (filter === 2) value += up;
      else if (filter === 3) value += Math.floor((left + up) / 2);
      else if (filter === 4) {
        const p = left + up - upLeft;
        const pa = Math.abs(p - left);
        const pb = Math.abs(p - up);
        const pc = Math.abs(p - upLeft);
        value += pa <= pb && pa <= pc ? left : (pb <= pc ? up : upLeft);
      }
      line[i] = value & 0xff;
    }
    for (let x = 0; x < width; x += 1) {
      const at = (y * width + x) * 4;
      pixels[at] = line[x * channels];
      pixels[at + 1] = line[x * channels + 1];
      pixels[at + 2] = line[x * channels + 2];
      pixels[at + 3] = channels === 4 ? line[x * channels + 3] : 255;
    }
    previous = line;
  }
  return { width: width, height: height, pixels: pixels };
}

// ---- 统计（这三条是**纯函数**，自检里直接喂合成图） --------------------------

/** 一个像素相对两个参考色（填充色 / 底色）的归类。slack 是每通道容差。 */
export function classifyPixel(rgba, offset, fill, background, slack = 8) {
  let nearFill = true;
  let nearBackground = true;
  for (let i = 0; i < 3; i += 1) {
    const value = rgba[offset + i];
    if (Math.abs(value - fill[i]) > slack) nearFill = false;
    if (Math.abs(value - background[i]) > slack) nearBackground = false;
  }
  if (nearFill) return 'fill';
  if (nearBackground) return 'bg';
  return 'edge';
}

/**
 * 边缘环：**既不是纯填充、也不是纯底色**的像素，再向外扩 `dilate` 圈。
 * 为什么要扩：抗锯齿只在边界那一圈，而两侧的"边界像素"可能落在相邻像素上 ——
 * 只取一侧的边界会漏掉另一侧真正差得最狠的那个像素（第 52 轮量 AA 时踩过这类）。
 */
export function edgeRing(image, fill, background, slack = 8, dilate = 1) {
  const { width, height, pixels } = image;
  const raw = new Uint8Array(width * height);
  for (let p = 0; p < width * height; p += 1) {
    raw[p] = classifyPixel(pixels, p * 4, fill, background, slack) === 'edge' ? 1 : 0;
  }
  let ring = raw;
  for (let round = 0; round < dilate; round += 1) {
    const next = new Uint8Array(width * height);
    for (let y = 0; y < height; y += 1) {
      for (let x = 0; x < width; x += 1) {
        const p = y * width + x;
        let hit = ring[p] === 1;
        if (!hit && x > 0) hit = ring[p - 1] === 1;
        if (!hit && x + 1 < width) hit = ring[p + 1] === 1;
        if (!hit && y > 0) hit = ring[p - width] === 1;
        if (!hit && y + 1 < height) hit = ring[p + width] === 1;
        next[p] = hit ? 1 : 0;
      }
    }
    ring = next;
  }
  return ring;
}

/** 在给定的像素集合上比两张图。返回最大/平均通道差与差异像素数。 */
export function ringStats(a, b, selector) {
  let pixels = 0;
  let maxAbsDiff = 0;
  let sum = 0;
  let diffPixels = 0;
  let worstAt = null;
  for (let p = 0; p < a.width * a.height; p += 1) {
    if (selector !== null && selector[p] !== 1) continue;
    pixels += 1;
    let worst = 0;
    for (let c = 0; c < 4; c += 1) {
      const delta = Math.abs(a.pixels[p * 4 + c] - b.pixels[p * 4 + c]);
      if (delta > worst) worst = delta;
      if (c < 3 && delta > maxAbsDiff) {
        maxAbsDiff = delta;
        worstAt = { x: p % a.width, y: Math.floor(p / a.width) };
      }
    }
    if (c0(worst) === 1) diffPixels += 1;
    sum += worst;
  }
  return { pixels, maxAbsDiff, meanAbsDiff: pixels === 0 ? 0 : sum / pixels, diffPixels, worstAt };
}

function c0(value) {
  return value > 0 ? 1 : 0;
}

/** 一个矩形区域内比两张图（矩形用 [x, y, w, h]，越界的部分裁掉）。 */
export function rectStats(a, b, rect) {
  const selector = new Uint8Array(a.width * a.height);
  for (let y = rect[1]; y < rect[1] + rect[3] && y < a.height; y += 1) {
    for (let x = rect[0]; x < rect[0] + rect[2] && x < a.width; x += 1) {
      if (x >= 0 && y >= 0) selector[y * a.width + x] = 1;
    }
  }
  return { ...ringStats(a, b, selector), selector };
}

/** 一列像素的第 c 个通道（默认 R）。掩码量测要的就是这个剖面。 */
export function columnProfile(image, x, fromY, toY, channel = 0) {
  const out = [];
  for (let y = fromY; y <= toY && y < image.height; y += 1) {
    if (y < 0) continue;
    out.push(image.pixels[(y * image.width + x) * 4 + channel]);
  }
  return out;
}

export function pixelAt(image, x, y) {
  const offset = (y * image.width + x) * 4;
  return [image.pixels[offset], image.pixels[offset + 1], image.pixels[offset + 2], image.pixels[offset + 3]];
}

// ---- 三个案例 ---------------------------------------------------------------

/** 素材：一张纯色 PNG（引擎那侧当"素材"用）。 */
function solidRgba(width, height, color) {
  const rgba = Buffer.alloc(width * height * 4);
  for (let p = 0; p < width * height; p += 1) {
    rgba[p * 4] = color[0];
    rgba[p * 4 + 1] = color[1];
    rgba[p * 4 + 2] = color[2];
    rgba[p * 4 + 3] = color.length > 3 ? color[3] : 255;
  }
  return rgba;
}

function baseDoc(size, background) {
  return {
    project_schema: 1,
    generator: { app: 'measure-dom-parity', version: '0.0.1' },
    meta: { title: 'DOM 差异量测', created_at: null, modified_at: null },
    render_hints: { width: size[0], height: size[1], background: background, format: 'png' },
    assets: [],
    timeline: { schema: 4, timebase: { num: 30, den: 1 }, markers: [], tracks: [{ id: 'v1', kind: 'video', layers: [] }] },
  };
}

export const CASES = [
  {
    name: 'geometry.edge_antialias',
    what: '同一个圆角矩形转 30°：边缘那一圈像素的最大通道差',
    size: [320, 180],
    fill: [{ id: 'aa', w: 200, h: 120, color: [224, 80, 80] }],
    assets: [{ file: 'solid.png', width: 200, height: 120, color: [224, 80, 80] }],
    doc: () => {
      const doc = baseDoc([320, 180], '#000000');
      doc.assets.push({
        id: 'solid',
        kind: 'video',
        name: 'solid',
        uri: 'solid.png',
        width: 200,
        height: 120,
        frame_count: 1,
        timebase: { num: 1, den: 1 },
      });
      doc.timeline.tracks[0].layers.push({
        id: 'aa',
        start: 0,
        end: 30,
        source: { asset_id: 'solid', source_in: 0 },
        corner_radius: 6,
        transform: { x: 0, y: 0, scale: 1, rotation: 30 },
      });
      return doc;
    },
    stat: (engine, dom) => {
      const fill = [224, 80, 80];
      const background = [0, 0, 0];
      const ring = edgeRing(engine, fill, background, 10, 2);
      const stats = ringStats(engine, dom, ring);
      let engineEdge = 0;
      let domEdge = 0;
      for (let p = 0; p < engine.width * engine.height; p += 1) {
        if (classifyPixel(engine.pixels, p * 4, fill, background, 10) === 'edge') engineEdge += 1;
        if (classifyPixel(dom.pixels, p * 4, fill, background, 10) === 'edge') domEdge += 1;
      }
      const perimeter = 2 * (200 + 120);
      return {
        lines: [
          '边缘环像素 ' + String(stats.pixels) + ' 个（引擎侧"既不是填充也不是底色"的像素 ' + String(engineEdge) +
            ' 个、浏览器侧 ' + String(domEdge) + ' 个；形状周长按 2×(200+120) = ' + String(perimeter) + ' 估）',
          '边缘环上的最大通道差 = ' + String(stats.maxAbsDiff) + '（在 ' + JSON.stringify(stats.worstAt) + '）',
          '边缘环上的平均通道差 = ' + stats.meanAbsDiff.toFixed(3) + '；环上不同的像素 ' + String(stats.diffPixels) + '/' + String(stats.pixels),
          '斜坡宽度（半透明类像素 ÷ 周长）≈ 引擎 ' + (engineEdge / perimeter).toFixed(3) + ' 像素、浏览器 ' + (domEdge / perimeter).toFixed(3) + ' 像素',
        ],
        numbers: { edge_max_diff_max: stats.maxAbsDiff },
      };
    },
  },
  {
    name: 'blend.add_plus_lighter',
    what: '加法混合：上层不透明度 0.5 时两边的亮部差',
    size: [320, 180],
    fill: [
      { id: 'bottom', w: 320, h: 180, color: [200, 60, 60] },
      { id: 'top', w: 160, h: 160, color: [60, 60, 200] },
    ],
    assets: [
      { file: 'bottom.png', width: 320, height: 180, color: [200, 60, 60] },
      { file: 'top.png', width: 160, height: 160, color: [60, 60, 200] },
    ],
    doc: () => {
      const doc = baseDoc([320, 180], '#000000');
      doc.assets.push(
        { id: 'bottom', kind: 'video', name: 'bottom', uri: 'bottom.png', width: 320, height: 180, frame_count: 1, timebase: { num: 1, den: 1 } },
        { id: 'top', kind: 'video', name: 'top', uri: 'top.png', width: 160, height: 160, frame_count: 1, timebase: { num: 1, den: 1 } },
      );
      // **两条轨**：同一条轨里的层不许重叠（校验会直接退 2），而这一条要量的正是
      // "上下两层同时在场时的混合" —— 所以底一层放 tracks[0]、加法层放 tracks[1]。
      doc.timeline.tracks[0].layers.push({ id: 'bottom', start: 0, end: 30, source: { asset_id: 'bottom', source_in: 0 } });
      doc.timeline.tracks.push({
        id: 'v2',
        kind: 'video',
        layers: [{ id: 'top', start: 0, end: 30, source: { asset_id: 'top', source_in: 0 }, blend: 'add', opacity: 0.5 }],
      });
      return doc;
    },
    stat: (engine, dom) => {
      const square = [(320 - 160) / 2, (180 - 160) / 2, 160, 160];
      const stats = rectStats(engine, dom, square);
      const center = [160, 90];
      const engineCenter = pixelAt(engine, center[0], center[1]);
      const domCenter = pixelAt(dom, center[0], center[1]);
      const predicted = [Math.min(255, 200 + 60), Math.min(255, 60 + 60), Math.min(255, 60 + 200), 255];
      const cssPredicted = [Math.round(0.5 * 60 + 200), Math.round(0.5 * 60 + 60), Math.round(0.5 * 200 + 60), 255];
      return {
        lines: [
          '中心像素：引擎 ' + JSON.stringify(engineCenter) + '、浏览器 ' + JSON.stringify(domCenter),
          '手算：本仓 add = clamp(src + dst) = ' + JSON.stringify(predicted) + '；CSS plus-lighter(0.5) = alpha_s·Cs + Cb = ' + JSON.stringify(cssPredicted),
          '上层方块内最大通道差 = ' + String(stats.maxAbsDiff) + '（在 ' + JSON.stringify(stats.worstAt) + '）',
          '上层方块内平均通道差 = ' + stats.meanAbsDiff.toFixed(3) + '；不同的像素 ' + String(stats.diffPixels) + '/' + String(stats.pixels),
        ],
        numbers: { max_abs_diff_max: stats.maxAbsDiff },
      };
    },
  },
  {
    name: 'mask.interpolation_kernel',
    what: '掩码内部的插值：3×3 掩码拉伸到 200×120 时两边在同一列的剖面',
    size: [200, 120],
    fill: [{ id: 'm', w: 200, h: 120, color: [255, 255, 255] }],
    assets: [
      { file: 'fill.png', width: 200, height: 120, color: [255, 255, 255] },
      { file: 'mask.png', width: 3, height: 3, color: [255, 255, 255], rows: [255, 128, 0] },
    ],
    doc: (ctx) => {
      const doc = baseDoc([200, 120], '#000000');
      doc.assets.push(
        { id: 'fill', kind: 'video', name: 'fill', uri: 'fill.png', width: 200, height: 120, frame_count: 1, timebase: { num: 1, den: 1 } },
        {
          id: 'mask',
          kind: 'video',
          name: 'mask',
          uri: ctx.domUris ? ctx.base + '/case/' + ctx.name + '/mask.png' : 'mask.png',
          width: 3,
          height: 3,
          frame_count: 1,
          timebase: { num: 1, den: 1 },
        },
      );
      doc.timeline.tracks[0].layers.push({ id: 'm', start: 0, end: 30, source: { asset_id: 'fill', source_in: 0 }, mask: { asset_id: 'mask' } });
      return doc;
    },
    stat: (engine, dom) => {
      const rows = [4, 114];
      const engineProfile = columnProfile(engine, 100, rows[0], rows[1], 0);
      const domProfile = columnProfile(dom, 100, rows[0], rows[1], 0);
      let maxAbsDiff = 0;
      for (let i = 0; i < engineProfile.length; i += 1) {
        maxAbsDiff = Math.max(maxAbsDiff, Math.abs(engineProfile[i] - domProfile[i]));
      }
      const show = (profile) => profile.filter((value, index) => index % 8 === 0).join(',');
      return {
        lines: [
          '第 100 列（每 8 行取一个）：',
          '  引擎  ' + show(engineProfile),
          '  浏览器 ' + show(domProfile),
          '这一列上的最大通道差 = ' + String(maxAbsDiff),
        ],
        numbers: { max_abs_diff_max: maxAbsDiff },
      };
    },
  },
];

/** 素材 PNG：`rows` 给每行的 alpha（掩码那一条要的就是这个），没有就整张一个颜色。 */
export function makeAssetPng(asset) {
  const width = asset.width;
  const height = asset.height;
  const rgba = Buffer.alloc(width * height * 4);
  for (let y = 0; y < height; y += 1) {
    const alpha = asset.rows === undefined ? (asset.color.length > 3 ? asset.color[3] : 255) : asset.rows[y];
    for (let x = 0; x < width; x += 1) {
      const offset = (y * width + x) * 4;
      rgba[offset] = asset.color[0];
      rgba[offset + 1] = asset.color[1];
      rgba[offset + 2] = asset.color[2];
      rgba[offset + 3] = alpha;
    }
  }
  return encodePng(width, height, rgba);
}

// ---- 一个案例 -----------------------------------------------------------------

export async function runCase(kase, env) {
  const dir = join(OUT_ROOT, kase.name);
  mkdirSync(dir, { recursive: true });
  for (const asset of kase.assets) writeFileSync(join(dir, asset.file), makeAssetPng(asset));
  const engineDoc = kase.doc({ base: env.base, name: kase.name, domUris: false });
  const domDoc = kase.doc({ base: env.base, name: kase.name, domUris: true });
  const enginePath = join(dir, 'engine.doc.json');
  const domPath = join(dir, 'dom.doc.json');
  writeFileSync(enginePath, JSON.stringify(engineDoc, null, 2) + '\n');
  writeFileSync(domPath, JSON.stringify(domDoc, null, 2) + '\n');

  const rendered = renderEngine(env.dhampir, enginePath, dir, join(dir, 'engine'));
  if (!rendered.ok) return { name: kase.name, ok: false, why: '引擎那一侧：' + rendered.why };

  const fill = encodeURIComponent(JSON.stringify(kase.fill));
  const url = env.base + '/dom-measure.html?project=' + encodeURIComponent('/case/' + kase.name + '/dom.doc.json') +
    '&frame=0&fill=' + fill;
  // **别忘了 await**：`screenshotDom` 从"同步 spawnSync"改成 CDP 之后是 async 的，
  // 少一个 await 的后果是 `shot.ok` 是 undefined ⇒ 每次都报"浏览器那一侧：undefined"
  // （一个把人引向浏览器、而真相是我自己漏了 await 的错误信息）。
  const shot = await screenshotDom(env.chrome, url, kase.size[0], kase.size[1], join(dir, 'dom.png'), join(OUT_ROOT, 'chrome-profile'));
  if (!shot.ok) return { name: kase.name, ok: false, why: '浏览器那一侧：' + shot.why };

  const engine = decodePng(readFileSync(rendered.png));
  // 浏览器那张用**自己的**解码器：Chrome 截出来常常没有 alpha 通道（colorType=2）。
  const dom = decodeBrowserPng(readFileSync(shot.png));
  if (engine.width !== dom.width || engine.height !== dom.height) {
    return {
      name: kase.name,
      ok: false,
      why: '两边尺寸不同：引擎 ' + engine.width + '×' + engine.height + '、浏览器 ' + dom.width + '×' + dom.height +
        '（截图窗口尺寸与渲染尺寸必须一致）',
    };
  }
  const stat = kase.stat(engine, dom);
  return { name: kase.name, ok: true, what: kase.what, lines: stat.lines, numbers: stat.numbers, dir: dir };
}

// ---- 入口 ---------------------------------------------------------------------

export function parseArgs(argv) {
  const options = { mode: 'run', only: null, problem: null };
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    if (arg === '--self-test') {
      options.mode = 'self-test';
    } else if (arg === '--case') {
      const value = argv[i + 1];
      if (value === undefined) {
        options.problem = '--case 后面要跟一个案例名';
        return options;
      }
      options.only = value;
      i += 1;
    } else if (arg === '--list') {
      options.mode = 'list';
    } else if (arg === '--keep') {
      // 默认就保留（要看截图）；接受它是为了不让人以为传了才有。
    } else {
      options.problem = '不认识的参数：' + arg;
      return options;
    }
  }
  if (options.only !== null && !CASES.some((kase) => kase.name === options.only)) {
    options.problem = '案例名不在表里：' + options.only + '（可选：' + CASES.map((kase) => kase.name).join(' / ') + '）';
  }
  return options;
}

export function selfTest() {
  const problems = [];
  let checks = 0;
  const say = (name, ok) => {
    checks += 1;
    if (!ok) problems.push(name);
  };
  say('填充色像素该归成 fill', classifyPixel(Buffer.from([224, 80, 80, 255]), 0, [224, 80, 80], [0, 0, 0], 8) === 'fill');
  say('底色像素该归成 bg', classifyPixel(Buffer.from([0, 0, 0, 255]), 0, [224, 80, 80], [0, 0, 0], 8) === 'bg');
  say('中间值该归成 edge', classifyPixel(Buffer.from([112, 40, 40, 255]), 0, [224, 80, 80], [0, 0, 0], 8) === 'edge');
  // 合成一张**有边缘像素**的图：4×4 方块，四个角写成中间值（真图里那正是抗锯齿像素）。
  // 第一版自检这几个用例是错的 —— 我画了一张硬边图（一个 edge 像素都没有），
  // 却断言"环里应当有 (2,2)"：环空着是对的，错的是断言。
  const rgba = Buffer.alloc(8 * 8 * 4);
  for (let y = 2; y < 6; y += 1) {
    for (let x = 2; x < 6; x += 1) {
      const offset = (y * 8 + x) * 4;
      rgba[offset] = 224;
      rgba[offset + 1] = 80;
      rgba[offset + 2] = 80;
      rgba[offset + 3] = 255;
    }
  }
  for (const corner of [[2, 2], [5, 2], [2, 5], [5, 5]]) {
    const offset = (corner[1] * 8 + corner[0]) * 4;
    rgba[offset] = 112;
    rgba[offset + 1] = 40;
    rgba[offset + 2] = 40;
  }
  const image = decodePng(encodePng(8, 8, rgba));
  const ring = edgeRing(image, [224, 80, 80], [0, 0, 0], 8, 1);
  say('环里应当有边缘像素 (2,2)', ring[2 * 8 + 2] === 1);
  // 外扩用的是**四邻域**（不是八邻域）：所以 (2,2) 的左邻 (1,2) 在环里，而斜对角的 (1,1) 不在。
  say('环外扩后应当有 (1,2)（四邻域）', ring[2 * 8 + 1] === 1);
  say('斜对角的 (1,1) 不该在环里（四邻域）', ring[1 * 8 + 1] === 0);
  say('方块中心 (3,3) 不该在环里', ring[3 * 8 + 3] === 0);
  say('角落 (7,7) 不该在环里', ring[7 * 8 + 7] === 0);
  const other = decodePng(encodePng(8, 8, rgba));
  const target = 2 * 8 + 2;
  other.pixels[target * 4] = 119;
  const stats = ringStats(image, other, ring);
  say('最大差应当是 7', stats.maxAbsDiff === 7);
  say('差异像素数应当是 1', stats.diffPixels === 1);
  say('最差点坐标应当是 (2,2)', JSON.stringify(stats.worstAt) === JSON.stringify({ x: 2, y: 2 }));
  const whole = rectStats(image, other, [0, 0, 8, 8]);
  say('整帧选择器应当覆盖 64 个像素', whole.pixels === 64);
  const gradient = Buffer.alloc(4 * 4 * 4);
  for (let y = 0; y < 4; y += 1) {
    for (let x = 0; x < 4; x += 1) {
      const offset = (y * 4 + x) * 4;
      gradient[offset] = y * 60;
      gradient[offset + 3] = 255;
    }
  }
  const profile = columnProfile(decodePng(encodePng(4, 4, gradient)), 1, 0, 3);
  say('剖面应当读到 4 个值', profile.length === 4);
  say('剖面应当是 0/60/120/180', profile.join(',') === '0,60,120,180');
  const maskPng = decodePng(makeAssetPng({ width: 3, height: 3, color: [255, 255, 255], rows: [255, 128, 0] }));
  say('掩码素材第 0 行 alpha = 255', maskPng.pixels[3] === 255);
  say('掩码素材第 1 行 alpha = 128', maskPng.pixels[(3 + 0) * 4 + 3] === 128);
  say('掩码素材第 2 行 alpha = 0', maskPng.pixels[(6 + 0) * 4 + 3] === 0);
  // ⑧ 浏览器截图解码器（colorType=2 那一支）：自己拼一张**没有 alpha 通道**的 PNG。
  //    拼它需要 CRC，所以这里有个十行的本地实现 —— 自检要能独立跑，不引别的工具。
  const crcTable = new Int32Array(256);
  for (let n = 0; n < 256; n += 1) {
    let value = n;
    for (let k = 0; k < 8; k += 1) value = (value & 1) === 1 ? 0xedb88320 ^ (value >>> 1) : value >>> 1;
    crcTable[n] = value;
  }
  const crc32 = (bytes) => {
    let value = 0xffffffff;
    for (let i = 0; i < bytes.length; i += 1) value = crcTable[(value ^ bytes[i]) & 0xff] ^ (value >>> 8);
    return (value ^ 0xffffffff) >>> 0;
  };
  const chunk = (type, data) => {
    const body = Buffer.concat([Buffer.from(type, 'latin1'), data]);
    const length = Buffer.alloc(4);
    length.writeUInt32BE(data.length);
    const crc = Buffer.alloc(4);
    crc.writeUInt32BE(crc32(body));
    return Buffer.concat([length, body, crc]);
  };
  const buildRgbPng = (width, height, rows) => {
    const ihdr = Buffer.alloc(13);
    ihdr.writeUInt32BE(width, 0);
    ihdr.writeUInt32BE(height, 4);
    ihdr[8] = 8;
    ihdr[9] = 2;
    return Buffer.concat([
      Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
      chunk('IHDR', ihdr),
      chunk('IDAT', deflateSync(Buffer.concat(rows))),
      chunk('IEND', Buffer.alloc(0)),
    ]);
  };
  const plain = buildRgbPng(2, 2, [
    Buffer.from([0, 255, 0, 0, 0, 255, 0]),
    Buffer.from([0, 0, 0, 255, 255, 255, 255]),
  ]);
  const decodedRgb = decodeBrowserPng(plain);
  say('RGB 截图（滤波器 0）第 (0,0) 个像素应当是红', pixelAt(decodedRgb, 0, 0).join(',') === '255,0,0,255');
  say('RGB 截图第 (1,0) 个像素应当是绿', pixelAt(decodedRgb, 1, 0).join(',') === '0,255,0,255');
  say('RGB 截图第 (0,1) 个像素应当是蓝', pixelAt(decodedRgb, 0, 1).join(',') === '0,0,255,255');
  say('RGB 截图第 (1,1) 个像素应当是白', pixelAt(decodedRgb, 1, 1).join(',') === '255,255,255,255');
  // 第二行用滤波器 2（Up）：差值是相对上一行的
  const filtered = buildRgbPng(2, 2, [
    Buffer.from([0, 255, 0, 0, 0, 255, 0]),
    Buffer.from([2, 1, 0, 255, 255, 0, 255]),
  ]);
  const decodedFiltered = decodeBrowserPng(filtered);
  say('滤波器 2 的行也要能解回来（(0,1) 蓝）', pixelAt(decodedFiltered, 0, 1).join(',') === '0,0,255,255');
  say('滤波器 2 的行也要能解回来（(1,1) 白）', pixelAt(decodedFiltered, 1, 1).join(',') === '255,255,255,255');
  const rgbaRoundtrip = decodeBrowserPng(encodePng(1, 1, Buffer.from([9, 8, 7, 6])));
  say('RGBA 那一支也要能解（colorType=6）', pixelAt(rgbaRoundtrip, 0, 0).join(',') === '9,8,7,6');
  let rejected = false;
  try {
    decodeBrowserPng(Buffer.from('not a png at all'));
  } catch (error) {
    rejected = String(error.message).includes('签名');
  }
  say('非 PNG 要拒绝', rejected);

  const bad = parseArgs(['--case']);
  say('--case 缺参数要报用法错', bad.problem !== null);
  const unknown = parseArgs(['--wat']);
  say('不认识的参数要报用法错', unknown.problem !== null);
  say('--case 给对名字不该报错', parseArgs(['--case', 'geometry.edge_antialias']).problem === null);

  if (problems.length > 0) {
    console.error('✗ 自检失败 ' + String(problems.length) + ' 条：\n  ' + problems.join('\n  '));
    return 1;
  }
  console.log('✓ 自检通过（' + String(checks) + ' 条：归类 / 边缘环 / 环上统计 / 矩形选择 / 剖面 / 素材 alpha / 命令行）');
  return 0;
}

async function main(argv) {
  const options = parseArgs(argv);
  if (options.problem !== null) {
    console.error('✗ ' + options.problem);
    console.error('用法：node scripts/measure-dom-parity.mjs [--case <名>] [--self-test] [--list]');
    return 2;
  }
  if (options.mode === 'self-test') return selfTest();
  if (options.mode === 'list') {
    for (const kase of CASES) console.log(kase.name + '  ' + kase.what);
    return 0;
  }
  const chrome = findChrome();
  const dhampir = findDhampir();
  if (chrome === null) {
    console.error('✗ 找不到浏览器。装了 Chrome / Edge 就能跑；也可以用 CHROME_PATH 指一个。');
    return 1;
  }
  if (dhampir === null) {
    console.error('✗ 找不到 dhampir（先 cargo build 或 cargo build --release）。');
    return 1;
  }
  mkdirSync(OUT_ROOT, { recursive: true });
  const server = await startServer(OUT_ROOT);
  const base = 'http://127.0.0.1:' + String(server.port);
  console.log('浏览器：' + chrome);
  console.log('引擎：' + dhampir);
  console.log('静态服务：' + base + '（/case/** → target/dom-parity，其余 → web/）');
  console.log('');
  const results = [];
  for (const kase of CASES) {
    if (options.only !== null && options.only !== kase.name) continue;
    const result = await runCase(kase, { chrome: chrome, dhampir: dhampir, base: base });
    results.push(result);
    if (result.ok) {
      console.log('✓ ' + result.name + ' —— ' + String(result.what));
      for (const line of result.lines) console.log('    ' + line);
    } else {
      console.log('✗ ' + result.name + ' —— ' + String(result.why));
    }
    console.log('');
  }
  server.close();
  const report = [];
  report.push('measure-dom-parity 读数（浏览器：' + chrome + '）');
  report.push('引擎：' + dhampir);
  report.push('');
  for (const result of results) {
    report.push((result.ok ? '✓ ' : '✗ ') + result.name + (result.ok ? ' —— ' + String(result.what) : ' —— ' + String(result.why)));
    for (const line of result.ok ? result.lines : []) report.push('    ' + line);
    report.push('');
  }
  const reportPath = join(OUT_ROOT, 'report.txt');
  writeFileSync(reportPath, report.join('\n') + '\n');
  writeFileSync(join(OUT_ROOT, 'verdict.json'), JSON.stringify({ chrome: chrome, dhampir: dhampir, cases: results }, null, 2) + '\n');
  console.log('报告：' + reportPath);
  const failed = results.filter((result) => !result.ok).length;
  return failed === 0 ? 0 : 1;
}

// 同 check-preview-path：**不调 process.exit()**（本机 Node/Windows 会撞 libuv 断言）。
const isEntry = process.argv[1] !== undefined && resolve(process.argv[1]) === resolve(fileURLToPath(import.meta.url));
if (isEntry) {
  main(process.argv.slice(2)).then((code) => {
    process.exitCode = code;
  });
}
