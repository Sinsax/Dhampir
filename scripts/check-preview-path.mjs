#!/usr/bin/env node
// check-preview-path —— **预览那条腿（canvas）与成片（CLI）到底像不像**。
//
// ===========================================================================
// 为什么必须有这一条（阶段 5 的 G2）
// ===========================================================================
//
// 浏览器这一侧此前**只有一条路**有判据：`render_png` 那条**离屏**出图的路
// （records/m2 的跨厂商比对、check-dual-end 都走它）。而用户在预览里看到的是**另一条路**：
//
//     engine.open → attach('preview') → engine.seek(frame) → dhampir_project_draw → <canvas>
//
// 这条路以前**一次都没量过**。于是"预览与成片一致"是一句话，不是一个读数 ——
// 而阶段 6「切换预览」要的恰恰是这句话成立。这条脚本把它变成读数：
//
//   同一个工程、同一批帧 → CLI 出 PNG（dhampir frame） vs 浏览器 canvas 那一路（CDP 按画布矩形截图）
//   → 用 scripts/dhampir-framediff.mjs 的**同一套指标与档位**判。
//
// 档位用的是 scripts/framediff-thresholds.toml（跨厂商那一档）—— 因为这里比的也是
// "两套编译器/两套驱动画同一段 WGSL"，与那条腿**同性质**（不是引擎 vs CSS 那种算法不同）。
//
// **场景名 `preview` 刻意不写进那份档位文件**（第 83 轮试过又撤了）：那份文件的**原始字节**
// 被 FNV-1a 摘要钉在 records/m2 的两处记录与 4 份文档里 —— 连**加一句注释**都会让摘要变，
// 于是要么改 5 处历史文档、要么重跑记录。场景说明就写在这里与 plan/web-engine-measurements.md，
// 档位本身吃 `default`（1 LSB）已经是对的。
//
// ===========================================================================
// 用法
// ===========================================================================
//
//   node scripts/check-preview-path.mjs                    # 默认帧 0,200,400
//   node scripts/check-preview-path.mjs --frames 0,60,120 --scene preview
//   node scripts/check-preview-path.mjs --debug            # 打印页面自己的步骤日志
//   node scripts/check-preview-path.mjs --self-test
//
// 退出码：0 达标；1 不达标（会把是哪一帧、哪一条闸、差多少点出来）；2 跑不了
// （缺浏览器 / 缺 wasm pkg / 缺 dhampir / 页面报错 / 画布矩形与渲染尺寸不一致）。
//
// 前置：`wasm-pack build crates/dhampir-wasm --target web --out-dir www/pkg --dev`
// 与 `cargo build --release`（或 debug）。

import { spawn, spawnSync } from 'node:child_process';
import { createServer } from 'node:http';
import { existsSync, mkdirSync, readFileSync, readdirSync, renameSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { dirname, extname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { analyzePair, decodePng, encodePng, judge, parseThresholds, resolveThresholds } from './dhampir-framediff.mjs';
import { connectCdp, decodeBrowserPng, fetchPageTarget, findChrome, findDhampir, freePort, waitForOccupiedPort } from './measure-dom-parity.mjs';

const SCRIPT_DIR = dirname(fileURLToPath(import.meta.url));
export const REPO_ROOT = resolve(SCRIPT_DIR, '..');
export const WORK = join(REPO_ROOT, 'target', 'preview-path');
const PROJECT = join(REPO_ROOT, 'out', 'effects-demo', 'effects-demo.doc.json');
const ASSET_ROOT = join(REPO_ROOT, 'out', 'effects-demo');
const PKG_JS = join(REPO_ROOT, 'crates', 'dhampir-wasm', 'www', 'pkg', 'dhampir_wasm.js');
const PAGE = 'effects-demo-wasm.html';
const THRESHOLDS = join(REPO_ROOT, 'scripts', 'framediff-thresholds.toml');

const CONTENT_TYPES = {
  '.html': 'text/html; charset=utf-8',
  '.mjs': 'text/javascript; charset=utf-8',
  '.js': 'text/javascript; charset=utf-8',
  '.json': 'application/json; charset=utf-8',
  '.png': 'image/png',
  '.css': 'text/css; charset=utf-8',
  '.wasm': 'application/wasm',
  '.jpg': 'image/jpeg',
  '.jpeg': 'image/jpeg',
  '.webp': 'image/webp',
};

/**
 * 预览那一页要三处东西：页面本身在 web/、引擎 pkg 在 crates/dhampir-wasm/www/pkg/、
 * 素材在 out/effects-demo/（页面里的 `effects-demo/…` 是**相对页面**的路径）。
 */
export function startPreviewServer() {
  const routes = [
    { prefix: '/pkg/', dir: join(REPO_ROOT, 'crates', 'dhampir-wasm', 'www', 'pkg') },
    { prefix: '/effects-demo/', dir: ASSET_ROOT },
    { prefix: '/', dir: join(REPO_ROOT, 'web') },
  ];
  const server = createServer((request, response) => {
    const pathname = decodeURIComponent(new URL(request.url, 'http://127.0.0.1').pathname);
    const route = routes.find((item) => pathname.startsWith(item.prefix));
    const relative = route.prefix === '/' ? pathname.slice(1) : pathname.slice(route.prefix.length);
    const path = join(route.dir, relative === '' ? PAGE : relative);
    if (!path.startsWith(route.dir) || !existsSync(path) || statSync(path).isDirectory()) {
      response.writeHead(404, { 'content-type': 'text/plain; charset=utf-8' });
      response.end('404 ' + pathname);
      return;
    }
    const type = CONTENT_TYPES[extname(path).toLowerCase()];
    response.writeHead(200, { 'content-type': type === undefined ? 'application/octet-stream' : type, 'cache-control': 'no-store' });
    response.end(readFileSync(path));
  });
  return new Promise((done) => {
    server.listen(0, '127.0.0.1', () => done({ port: server.address().port, close: () => server.close() }));
  });
}

function sleep(ms) {
  return new Promise((done) => setTimeout(done, ms));
}

export function parseArgs(argv) {
  // 场景名**不设开关**：它由文件名（`preview-f000k.png`）决定，判据按同一条路解析档位 ——
  // 让调用方能"换个场景名"就等于让阈值的来源可以漂。
  const options = { mode: 'run', frames: [0, 200, 400], debug: false, problem: null };
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    if (arg === '--self-test') options.mode = 'self-test';
    else if (arg === '--debug') options.debug = true;
    else if (arg === '--frames') {
      const value = argv[i + 1];
      if (value === undefined) { options.problem = '--frames 后面要跟 0,200,400 这样的清单'; return options; }
      const frames = value.split(',').map((piece) => Number(piece.trim()));
      if (frames.length === 0 || frames.some((frame) => !Number.isInteger(frame) || frame < 0)) {
        options.problem = '--frames 只收非负整数清单：' + value;
        return options;
      }
      options.frames = frames;
      i += 1;
    } else {
      options.problem = '不认识的参数：' + arg;
      return options;
    }
  }
  return options;
}

/** CLI 那一侧：一帧一张 PNG。名字统一成 `preview-f000k.png`（framediff 要求帧号从 0 起连续）。 */
export function engineFrameName(index) {
  return 'preview-f' + String(index).padStart(4, '0') + '.png';
}

/** 把 dhampir frame 写出来的那张（名字由 CLI 定，别猜）搬成我们要的名字。 */
export function pickRenderedPng(dir) {
  const names = readdirSync(dir).filter((name) => /^frame-[0-9]+\.png$/.test(name));
  if (names.length === 0) return null;
  names.sort();
  return join(dir, names[names.length - 1]);
}

// ---- 跑 ---------------------------------------------------------------------

async function main(argv) {
  const options = parseArgs(argv);
  if (options.problem !== null) {
    console.error('✗ ' + options.problem);
    console.error('用法：node scripts/check-preview-path.mjs [--frames 0,200,400] [--debug] [--self-test]');
    return 2;
  }
  if (options.mode === 'self-test') return await selfTest();

  const chrome = findChrome();
  if (chrome === null) {
    console.error('✗ 找不到浏览器（Chrome / Edge；或设 CHROME_PATH）。');
    return 2;
  }
  const dhampir = findDhampir();
  if (dhampir === null) {
    console.error('✗ 找不到 dhampir（先 cargo build --release 或 cargo build）。');
    return 2;
  }
  if (!existsSync(PKG_JS)) {
    console.error('✗ 缺 wasm pkg：' + PKG_JS);
    console.error('  先跑：wasm-pack build crates/dhampir-wasm --target web --out-dir www/pkg --dev');
    return 2;
  }
  if (!existsSync(PROJECT)) {
    console.error('✗ 缺工程：' + PROJECT);
    console.error('  先跑：node scripts/effects-demo.mjs');
    return 2;
  }

  const engineDir = join(WORK, 'engine');
  const browserDir = join(WORK, 'browser');
  rmSync(WORK, { recursive: true, force: true });
  mkdirSync(engineDir, { recursive: true });
  mkdirSync(browserDir, { recursive: true });
  console.log('工程：' + PROJECT);
  console.log('帧：' + options.frames.join(',') + '（CLI 一帧一张 PNG，浏览器按画布矩形截图）');
  console.log('');

  // ---- ① CLI 那一侧 ----
  for (let index = 0; index < options.frames.length; index += 1) {
    const frame = options.frames[index];
    const rawDir = join(WORK, 'cli-raw', String(frame));
    mkdirSync(rawDir, { recursive: true });
    const result = spawnSync(dhampir, ['frame', '--project', PROJECT, '--asset-root', ASSET_ROOT, '--frame', String(frame), '--out', rawDir], { encoding: 'utf8' });
    if (result.status !== 0) {
      console.error('✗ CLI 渲染第 ' + String(frame) + ' 帧失败（退出码 ' + String(result.status) + '）：');
      console.error(String(result.stderr === null ? '' : result.stderr).slice(-600));
      return 2;
    }
    const png = pickRenderedPng(rawDir);
    if (png === null) {
      console.error('✗ CLI 说成功，但 ' + rawDir + ' 里没有 frame-*.png');
      return 2;
    }
    const target = join(engineDir, engineFrameName(index));
    renameSync(png, target);
    // 两边必须**都是不透明**才谈得上逐像素比：预览里的画布是浏览器合成结果（没有 alpha 通道），
    // CLI 的 PNG 若带透明像素，那两边的 RGB 说的就不是一回事。这里如实报出来（effects-demo
    // 这一份是零个透明像素 —— 它设了 render_hints.background）。
    const image = decodePng(readFileSync(target));
    let transparent = 0;
    for (let p = 0; p < image.width * image.height; p += 1) {
      if (image.pixels[p * 4 + 3] !== 255) transparent += 1;
    }
    if (transparent > 0) {
      console.log('    ⚠ CLI 这张有 ' + String(transparent) + ' 个非不透明像素 —— 与画布侧的合成结果不是同一件事，读数要按这个前提读');
    }
    console.log('  CLI  第 ' + String(frame).padStart(3) + ' 帧 → ' + engineFrameName(index));
  }
  console.log('');

  // ---- ② 浏览器那一侧（**预览那条腿**：canvas） ----
  const server = await startPreviewServer();
  const base = 'http://127.0.0.1:' + String(server.port);
  const port = await freePort();
  const profile = join(WORK, 'chrome-profile');
  const child = spawn(chrome, [
    '--headless=new',
    '--no-first-run',
    '--disable-extensions',
    '--hide-scrollbars',
    '--window-size=1300,900',
    '--remote-debugging-port=' + String(port),
    '--user-data-dir=' + profile,
    'about:blank',
  ], { stdio: 'ignore' });
  let cdp = null;
  try {
    await waitForOccupiedPort(port);
    const target = await fetchPageTarget(port);
    cdp = await connectCdp(target.webSocketDebuggerUrl);
    await cdp.send('Page.enable');
    await cdp.send('Page.navigate', { url: base + '/' + PAGE });
    // 页面自己会在最后 `await draw()` 画第 0 帧；等它把步骤日志推过 seek:done。
    const deadline = Date.now() + 90000;
    for (;;) {
      const state = await cdp.evaluate('JSON.stringify({ err: globalThis.__err === undefined ? null : globalThis.__err, step: globalThis.__step === undefined ? null : globalThis.__step, drawn: globalThis.__drawn === undefined ? 0 : globalThis.__drawn })');
      const parsed = state === undefined || state === null ? { err: null, step: null, drawn: 0 } : JSON.parse(state);
      if (parsed.err !== null) {
        console.error('✗ 预览页自己报错（' + String(parsed.step) + '）：' + String(parsed.err));
        return 2;
      }
      if (parsed.drawn > 0) break;
      if (Date.now() > deadline) {
        console.error('✗ 预览页 90 秒没有画出第一帧（步骤停在 ' + String(parsed.step) + '）');
        return 2;
      }
      await sleep(250);
    }
    // 画布必须**按 1:1** 出现在视口里：截图裁的就是它的矩形，缩放过就"比的是缩放后的图"。
    const geometry = await cdp.evaluate('(() => { const canvas = document.getElementById("preview"); canvas.style.width = canvas.width + "px"; canvas.style.height = canvas.height + "px"; canvas.scrollIntoView({ block: "start", inline: "start" }); const box = canvas.getBoundingClientRect(); return JSON.stringify({ x: box.x, y: box.y, width: box.width, height: box.height, cw: canvas.width, ch: canvas.height }); })()');
    const box = JSON.parse(geometry);
    if (Math.round(box.width) !== box.cw || Math.round(box.height) !== box.ch) {
      console.error('✗ 画布矩形 ' + String(box.width) + '×' + String(box.height) + ' 与渲染尺寸 ' + String(box.cw) + '×' + String(box.ch) + ' 不一致 —— 那样截出来的图不是这一帧，比了也没意义。');
      return 2;
    }
    console.log('  画布 ' + String(box.cw) + '×' + String(box.ch) + '（1:1，截图裁这个矩形）');

    for (let index = 0; index < options.frames.length; index += 1) {
      const frame = options.frames[index];
      const before = await cdp.evaluate('globalThis.__drawn');
      await cdp.evaluate('(() => { const slider = document.getElementById("frame"); slider.value = ' + JSON.stringify(String(frame)) + '; slider.dispatchEvent(new Event("input")); return slider.value; })()');
      const drawDeadline = Date.now() + 60000;
      for (;;) {
        const drawn = await cdp.evaluate('globalThis.__drawn');
        const err = await cdp.evaluate('globalThis.__err === undefined ? null : globalThis.__err');
        if (err !== null) {
          console.error('✗ 浏览器画第 ' + String(frame) + ' 帧时报错：' + String(err));
          return 2;
        }
        if (typeof drawn === 'number' && drawn > before) break;
        if (Date.now() > drawDeadline) {
          console.error('✗ 浏览器 60 秒没画完第 ' + String(frame) + ' 帧');
          return 2;
        }
        await sleep(200);
      }
      const shot = await cdp.send('Page.captureScreenshot', {
        format: 'png',
        fromSurface: true,
        captureBeyondViewport: false,
        clip: { x: box.x, y: box.y, width: box.cw, height: box.ch, scale: 1 },
      });
      // Chrome 截出来常常**没有 alpha 通道**（colorType=2），而 framediff 的解码器
      // **故意**只认 RGBA 非隔行（自检里钉着"拒绝非 RGBA"）。所以这里无损重编码成 RGBA ——
      // 像素值一个不动，只是补上 alpha=255（画布本来就是不透明的）。
      const shotPng = decodeBrowserPng(Buffer.from(shot.data, 'base64'));
      writeFileSync(join(browserDir, engineFrameName(index)), encodePng(shotPng.width, shotPng.height, shotPng.pixels));
      console.log('  浏览器 第 ' + String(frame).padStart(3) + ' 帧 → ' + engineFrameName(index));
    }
  } finally {
    if (cdp !== null) {
      await cdp.send('Browser.close').catch(() => {});
      cdp.close();
    }
    server.close();
    await sleep(200);
    try {
      child.kill();
    } catch {
      // 已经退了
    }
  }
  console.log('');

  // ---- ③ 判 ----
  const table = parseThresholds(readFileSync(THRESHOLDS, 'utf8'));
  const analysis = analyzePair({ dirA: engineDir, dirB: browserDir, labelA: 'native-cli', labelB: 'browser-preview' });
  const judged = judge(analysis, table);
  for (const scene of judged.scenes) {
    console.log((scene.verdict === 'pass' ? '✓ ' : '✗ ') + scene.scene + '（' + String(scene.frames) + ' 帧）' +
      ' 最大通道差 ' + String(scene.max_abs_diff) + ' / 最差帧 SSIM ' + Number(scene.min_ssim_luma).toFixed(9) +
      ' / 最差帧 PSNR ' + (scene.min_psnr_rgb_db === null ? 'inf' : Number(scene.min_psnr_rgb_db).toFixed(2) + ' dB'));
    for (const failure of scene.failures) console.log('    闸门未过：' + failure);
  }
  writeFileSync(join(WORK, 'verdict.json'), JSON.stringify({ frames: options.frames, thresholds: THRESHOLDS, judged: judged }, null, 2) + '\n');
  console.log('');
  console.log(judged.verdict === 'pass'
    ? '预览那条腿与 CLI 出图在同一档内（档位：scripts/framediff-thresholds.toml 的场景 preview / default）'
    : '✗ 预览那条腿与 CLI 出图**不在**同一档内 —— 见上面逐条闸门');
  console.log('读数与截图：' + WORK);
  return judged.verdict === 'pass' ? 0 : 1;
}

// ---- 自检（不起浏览器、不跑 CLI） --------------------------------------------

export async function selfTest() {
  const problems = [];
  let checks = 0;
  const say = (name, ok) => {
    checks += 1;
    if (!ok) problems.push(name);
  };
  say('--frames 收清单', parseArgs(['--frames', '0,60,120']).frames.join(',') === '0,60,120');
  say('--frames 收负数要报错', parseArgs(['--frames', '0,-1']).problem !== null);
  say('--frames 缺值要报错', parseArgs(['--frames']).problem !== null);
  say('不认识的参数要报错', parseArgs(['--wat']).problem !== null);
  say('默认帧是 0/200/400', parseArgs([]).frames.join(',') === '0,200,400');
  say('帧文件名补足四位', engineFrameName(0) === 'preview-f0000.png' && engineFrameName(12) === 'preview-f0012.png');

  const temp = join(WORK, 'self-test-pick');
  rmSync(temp, { recursive: true, force: true });
  mkdirSync(temp, { recursive: true });
  say('空目录里挑不出东西', pickRenderedPng(temp) === null);
  writeFileSync(join(temp, 'frame-0000.png'), 'x');
  writeFileSync(join(temp, 'frame-0200.png'), 'x');
  say('挑得动多张时取最后一张', (pickRenderedPng(temp) ?? '').endsWith('frame-0200.png'));
  say('别的名字不算数', (() => { rmSync(join(temp, 'frame-0000.png')); rmSync(join(temp, 'frame-0200.png')); writeFileSync(join(temp, 'thumb.png'), 'x'); return pickRenderedPng(temp) === null; })());

  // 档位表：场景没写就是 default 打底 —— 但 mean/min 两条必须有，否则不许混进"全过"。
  const table = parseThresholds(readFileSync(THRESHOLDS, 'utf8'));
  let limits = null;
  try {
    limits = resolveThresholds(table, 'preview');
  } catch (error) {
    limits = null;
  }
  say('档位表能解析出这个场景的档（default 打底）', limits !== null && limits.min_ssim_min !== undefined);
  say('本仓跨厂商那一档 max|Δ| 就是 1 LSB', limits.max_abs_diff_max === 1);

  // 路由：预览页要三处东西（页面 / pkg / 素材），路由错了现场只会看到"页面白屏"。
  const server = await startPreviewServer();
  const base = 'http://127.0.0.1:' + String(server.port);
  try {
    const page = await fetch(base + '/' + PAGE);
    const body = await page.text();
    say('预览页取得到（且是那一页）', page.status === 200 && body.includes('dhampir'));
    const pkg = await fetch(base + '/pkg/dhampir_wasm.js');
    say('pkg 路由指到 crates/dhampir-wasm/www/pkg', pkg.status === (existsSync(PKG_JS) ? 200 : 404));
    const escape = await fetch(base + '/../package.json');
    say('不许走目录逃逸（/../package.json 要 404）', escape.status === 404);
    const missing = await fetch(base + '/nope.png');
    say('不存在的素材要 404', missing.status === 404);
  } finally {
    server.close();
  }

  if (problems.length > 0) {
    console.error('✗ 自检失败 ' + String(problems.length) + ' 条：\n  ' + problems.join('\n  '));
    return 1;
  }
  console.log('✓ 自检通过（' + String(checks) + ' 条：命令行 / 帧名 / 挑 PNG / 档位表 / 路由）');
  return 0;
}

// **不调 process.exit()**：本机 Node/Windows 上真调它会概率性撞 libuv 断言
// （`Assertion failed: !(handle->flags & UV_HANDLE_CLOSING)`，退出码变成 -1073740791）——
// 这正是 scripts/dhampir-framediff.mjs 头部记着的那条。设 exitCode、自然退出。
const isEntry = process.argv[1] !== undefined && resolve(process.argv[1]) === resolve(fileURLToPath(import.meta.url));
if (isEntry) {
  main(process.argv.slice(2)).then((code) => {
    process.exitCode = code;
  });
}
