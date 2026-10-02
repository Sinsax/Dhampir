#!/usr/bin/env node
// **上传耗时的真素材基准**（浏览器腿）。
//
// # 为什么要有这个脚本
//
// 2026-10-02 之前，`AnimationTextures::upload` 是**逐帧** `create_texture` +
// `write_texture`。这件事在 native 上几乎免费，在 **wasm/WebGPU** 上却要每帧跨一次
// wasm↔JS 边界 —— 于是同一份 core 代码，500x500x77 帧（73.4 MiB）：
//
//     native 6 ms  /  浏览器 1699 ms      （差 283 倍）
//
// 而这个差距**在文档 §五 的「218 ms / 4 张」里完全看不见** —— 那个数是在 `SIZE = 4`
// （4×4 像素）的合成 fixture 上测的。规模不对的基准比没有基准更糟：它让人以为已经没问题了。
//
// 所以这条基准**必须用真素材、必须在真浏览器里跑**，而且把 b`ms/MiB` 一起打出来 ——
// 那个数收敛到常数就说明成本只跟字节数走（= 上传），散开才可能是解码。
//
// # 用法
//
//   node scripts/bench-animation-upload.mjs <gif目录> [port]
//
// `gif目录` **必填**：本仓不带这批素材（一张 500x500x77 的 GIF 体积不小，
// 而且它是**别人产出的内容**，不该进本仓）。素材是**只读引用**，一个字节都不拷进来。
//
// 为什么非要真素材：上面那条 283 倍就是因为 fixture 规模不对才被藏了整整一轮。
// 目录里应至少有**一张 500x500、几十帧**的 GIF，否则测的不是同一条路。
//
// 需要 WebGPU（Chrome 无头模式默认给 Dawn/WebGPU；拿不到时试 `--headed`）。
import { spawn } from 'node:child_process';
import { existsSync, mkdtempSync, readFileSync, readdirSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { Cdp, buildBrowserArgs, fetchPageTarget, findBrowser, freePort, sleep, waitForOccupiedPort, withTimeout } from './capture-harness-screenshot.mjs';

const HERE = new URL('.', import.meta.url).pathname.replace(/^\//, '');
const REPO = HERE + '..';
const ARGV = process.argv.slice(2);
const HEADLESS = !ARGV.includes('--headed');
const positional = ARGV.filter((a) => !a.startsWith('--'));
const GIF_DIR = positional[0];
const PORT = Number(positional[1] || 8899);

if (!GIF_DIR) {
  console.error('✗ 缺素材目录。');
  console.error('  用法：node scripts/bench-animation-upload.mjs <gif目录> [port]');
  console.error('  说明：这条基准刻意用真实 GIF（500x500、几十帧），本仓不带这批素材。');
  process.exit(2);
}
if (!existsSync(GIF_DIR)) {
  console.error('✗ 找不到素材目录 ' + GIF_DIR);
  console.error('  用法：node scripts/bench-animation-upload.mjs <gif目录> [port]');
  process.exit(2);
}

// 按帧数挑几张有代表性的（最大、中位、最小各一），而不是把整个库都跑一遍。
function frameCount(p) {
  const b = readFileSync(p);
  let i = 13;
  const fl = b[10];
  if (fl & 0x80) i += 3 * (1 << ((fl & 7) + 1));
  let n = 0;
  while (i < b.length) {
    const c = b[i];
    if (c === 0x21) { i += 2; while (i < b.length && b[i] !== 0) i += 1 + b[i]; i += 1; }
    else if (c === 0x2c) {
      n += 1;
      const lf = b[i + 9];
      let j = i + 10;
      if (lf & 0x80) j += 3 * (1 << ((lf & 7) + 1));
      j += 1;
      while (j < b.length && b[j] !== 0) j += 1 + b[j];
      i = j + 1;
    } else if (c === 0x3b) break;
    else i += 1;
  }
  return n;
}

// ⚠️ **不许把名字小写化之后拿去访问文件系统**（Linux 上大小写敏感，会直接找不到文件）。
// 判后缀时用 `toLowerCase()` 比一下**副本**，真正去访问的永远是 `readdirSync` 给的原名。
const all = readdirSync(GIF_DIR)
  .filter((f) => f.toLowerCase().endsWith('.gif'))
  .map((name) => {
    const path = join(GIF_DIR, name);
    return { name, path, frames: frameCount(path) };
  })
  .filter((g) => g.frames > 0)
  .sort((a, b) => b.frames - a.frames);
if (!all.length) { console.error('✗ ' + GIF_DIR + ' 里没有 GIF'); process.exit(2); }
// 最大 + 中位 + 最小
// `--same-as-app` 用用户报的那五张（而不是按帧数挑），并先 open 一份真工程 ——
// 那是应用的真实顺序（attach -> open -> bindSource -> load_animation）。
const APP_FIVE = ['打招呼_1.gif', '生气.gif', '问号.gif', '思考(认真地).gif', '笑.gif'];
const SAME_AS_APP = ARGV.includes('--same-as-app');
const picks = SAME_AS_APP
  ? APP_FIVE.map((n) => all.find((g) => g.name === n)).filter(Boolean)
  : [all[0], all[Math.floor(all.length / 2)], all[all.length - 1]];
console.log('素材 ' + GIF_DIR);
console.log('挑了 ' + picks.map((p) => p.name + '(' + p.frames + '帧)').join('  '));

// 页面与素材都走**临时目录**：不往本仓写任何东西（`www` 里只留一次性文件，跑完删）。
const stage = mkdtempSync(join(tmpdir(), 'dhampir-bench-'));
const www = join(REPO, 'crates', 'dhampir-wasm', 'www');
const pageName = 'bench-upload.html';
const gifSub = 'bench-gifs';
const { mkdirSync, cpSync } = await import('node:fs');
mkdirSync(join(www, gifSub), { recursive: true });
for (const p of picks) cpSync(p.path, join(www, gifSub, p.name));

let projectJson = null;
if (SAME_AS_APP) {
  const pj = join(REPO, 'fixtures', 'sample-project.json');
  projectJson = readFileSync(pj, 'utf8');
  try { cpSync(pj, join(www, 'bench-project.json')); } catch {}
}
const page = `<!doctype html>
<meta charset="utf-8"><body>
<canvas id="gl" width="640" height="360"></canvas>
<pre id="out">running…</pre>
<script type="module">
import init, { dhampir_project_attach, dhampir_project_open, dhampir_asset_load_animation } from './pkg/dhampir_wasm.js';
const lines = []; const out = document.getElementById('out');
const say = (s) => { lines.push(s); out.textContent = lines.join('\\n'); };
try {
  await init();
  const att = JSON.parse(await dhampir_project_attach('gl'));
  say('适配器 ' + JSON.stringify(att));
  if (${SAME_AS_APP}) {
    const pj = await (await fetch('./bench-project.json')).text();
    const t = performance.now();
    const o = JSON.parse(dhampir_project_open(pj));
    say('open(project) ' + (o.ok ? 'OK' : 'FAIL') + '  ' + Math.round(performance.now() - t) + ' ms');
  }
} catch (e) { say('init/attach 失败: ' + e); }
const FILES = ${JSON.stringify(picks.map((p) => p.name))};
let sum = 0;
for (const name of FILES) {
  const bytes = new Uint8Array(await (await fetch('./bench-gifs/' + encodeURIComponent(name))).arrayBuffer());
  const t0 = performance.now();
  let json;
  try { json = dhampir_asset_load_animation('bench/' + name, bytes); }
  catch (e) { say(name + ' 抛异常: ' + e); continue; }
  const ms = performance.now() - t0;
  const r = JSON.parse(json); const info = r.info || {};
  const mib = (info.bytes || 0) / 1048576; sum += ms;
  say(name + '  ' + (info.width || 0) + 'x' + (info.height || 0) + '  ' + (info.frame_count || 0) + ' 帧  ' +
      mib.toFixed(1) + ' MiB  OK=' + r.ok + '  ' + Math.round(ms) + ' ms  (' +
      (mib ? (ms / mib).toFixed(1) : '?') + ' ms/MiB)' + (r.error ? '  ERR=' + r.error : ''));
}
say('合计 ' + Math.round(sum) + ' ms');
say('DONE');
window.__benchResult = lines.join('\\n'); window.__benchDone = true;
</script></body>`;
writeFileSync(join(www, pageName), page);

const server = spawn(process.execPath, ['scripts/serve-wasm-harness.mjs', '--port', String(PORT), '--out', 'records/bench'], {
  cwd: REPO, stdio: ['ignore', 'pipe', 'pipe'],
});
server.stdout.on('data', () => {});
server.stderr.on('data', () => {});
await waitForOccupiedPort(PORT, 20000);

const browser = findBrowser();
let result = null;
if (!browser) {
  console.error('✗ 找不到 Chrome / Edge，用 DHAMPIR_CHROME=<exe> 指定一个');
} else {
  const profileDir = mkdtempSync(join(tmpdir(), 'dhampir-bench-prof-'));
  const devtoolsPort = await freePort();
  const url = 'http://127.0.0.1:' + PORT + '/' + pageName;
  const child = spawn(browser.path, buildBrowserArgs({ profileDir, url, width: 900, height: 600, headless: HEADLESS, devtoolsPort }), {
    stdio: ['ignore', 'ignore', 'pipe'],
  });
  try {
    const target = await fetchPageTarget(devtoolsPort, url, 30000);
    const ws = new WebSocket(target.webSocketDebuggerUrl);
    await withTimeout(new Promise((res, rej) => {
      ws.addEventListener('open', () => res(undefined), { once: true });
      ws.addEventListener('error', () => rej(new Error('连 DevTools 失败')), { once: true });
    }), 15000, '连 DevTools');
    const cdp = new Cdp(ws);
    await cdp.send('Runtime.enable');
    // ⚠️ 这里**不要**调 `SystemInfo.getInfo`：那是**浏览器级**域，页面级连接发它会被回
    // `-32000 only supported on the browser target`（我第一版就踩了这个，白跑一轮）。
    // 要看宿主设备表得另开一条 `/json/version` 的 WebSocket —— 那条本仓的
    // `run-browser-corpus.mjs` 已经有了，这里不重复。
    // 页面能自报的是 `dhampir_project_attach` 的返回（含 backend），已经打在上面了。
    const deadline = Date.now() + 180000;
    while (Date.now() < deadline) {
      const r = await cdp.send('Runtime.evaluate', {
        expression: 'JSON.stringify({done: !!window.__benchDone, text: window.__benchResult || document.getElementById("out").textContent})',
        returnByValue: true,
      });
      const v = JSON.parse(r.result.value);
      if (v.done) { result = v.text; break; }
      await sleep(500);
    }
    ws.close();
  } finally {
    try { spawn('taskkill', ['/pid', String(child.pid), '/T', '/F'], { stdio: 'ignore' }); } catch {}
    try { rmSync(profileDir, { recursive: true, force: true }); } catch {}
  }
}
try { server.kill(); } catch {}
try { rmSync(join(www, gifSub), { recursive: true, force: true }); } catch {}
try { rmSync(join(www, pageName), { force: true }); } catch {}
try { rmSync(join(www, 'bench-project.json'), { force: true }); } catch {}
try { rmSync(stage, { recursive: true, force: true }); } catch {}

console.log('\n===== 浏览器实测（真素材）=====' + (HEADLESS ? ' [无头]' : ' [有头]'));
console.log(result || '（没等到结果）');
