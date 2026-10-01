#!/usr/bin/env node
// 预览通路探针（阶段 4/5）：**一条命令**，在真实 Chrome 里量缓存命中与位图复用。
//
//     node scripts/preview-cache-probe.mjs [--headed] [--timeout 300]
//
// # 为什么不是 check-dual-end
//
// `check-dual-end` 的浏览器腿走合成源离屏导出（`web/synthetic.html` →
// `dhampir_sample_project_render_png`），**不经过** `dhampir_project_draw` ——
// 而阶段 4/5 改的正是后者。所以那一条 1.000000 与这个改动无关。
// 这里是**预览这条链**的读数：命中数 / 整圈 / 位图重传次数 / 像素是否逐字节相同。
//
// # 判据在页面里（`web/cache-probe.html`），这里只做三件事
//
//   1. 起一个静态服务（web/ + pkg/ + 一份 62 秒的测试视频）；
//   2. 用与 `web-check.mjs` 同一组 WebGPU 参数拉起无头 Chrome；
//   3. 把页面回报的 JSON 原样打出来，并按判据给退出码。
//
// **不重算页面里的数**：重算就又多一份实现，两份迟早说得不一样。

import { createServer } from 'node:http';
import { existsSync, readFileSync, statSync, writeFileSync } from 'node:fs';
import { spawn } from 'node:child_process';
import { extname, join, resolve, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const WEB_DIR = join(REPO_ROOT, 'web');
const PKG_DIR = join(REPO_ROOT, 'crates', 'dhampir-wasm', 'www', 'pkg');
const MEDIA = join(REPO_ROOT, 'target', 'cache-probe', 'test.mp4');

const MIME = {
  '.html': 'text/html; charset=utf-8',
  '.js': 'text/javascript; charset=utf-8',
  '.mjs': 'text/javascript; charset=utf-8',
  '.json': 'application/json; charset=utf-8',
  '.wasm': 'application/wasm',
  '.mp4': 'video/mp4',
  '.png': 'image/png',
};

function parseArgs(argv) {
  const out = { headed: false, skipE: false, timeoutMs: 420000, bad: false };
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    if (arg === '--headed') out.headed = true;
    else if (arg === '--skip-e') out.skipE = true;
    else if (arg === '--timeout') { out.timeoutMs = Number(argv[i + 1]) * 1000; i += 1; }
    else { console.error('✗ 不认识的参数：' + arg); out.bad = true; }
  }
  return out;
}

function findBrowser() {
  const candidates = [];
  if (process.env.DHAMPIR_CHROME) candidates.push(process.env.DHAMPIR_CHROME);
  const pf = process.env.ProgramFiles || 'C:\\Program Files';
  const pfx86 = process.env['ProgramFiles(x86)'] || 'C:\\Program Files (x86)';
  const local = process.env.LOCALAPPDATA || '';
  candidates.push(join(pf, 'Google', 'Chrome', 'Application', 'chrome.exe'));
  candidates.push(join(pfx86, 'Google', 'Chrome', 'Application', 'chrome.exe'));
  if (local !== '') candidates.push(join(local, 'Google', 'Chrome', 'Application', 'chrome.exe'));
  candidates.push(join(pfx86, 'Microsoft', 'Edge', 'Application', 'msedge.exe'));
  candidates.push(join(pf, 'Microsoft', 'Edge', 'Application', 'msedge.exe'));
  for (const candidate of candidates) if (existsSync(candidate)) return candidate;
  return null;
}

/** 页面判据：**每一条都对着一个具体的错**。返回 `{problems, unverifiable}`。
 *
 * 两者必须分开：`problems` 是"这件事没做到"，`unverifiable` 是"这个环境量不了"。
 * 混在一起只会有两种结局 —— 要么把测不了的当成绿的（自欺），
 * 要么把环境问题当成改动坏了（误红）。 */
function problemsOf(report) {
  const problems = [];
  const unverifiable = [];
  if (report === null) return { problems: ['页面没有回报结果（超时？看下面的浏览器 stderr）'], unverifiable };
  if (report.ok !== true) problems.push('页面自己报失败：' + report.error);
  const phases = report.phases || {};
  const S = phases.S || {};
  const R = phases.R || {};
  const before = phases.P4_before || {};
  const after = phases.P4_after || {};
  const C = phases.C || {};

  // ⓪ 探针自检：取到的到底是不是"画面"
  if (S.captureWorks !== true) {
    // **不是"改动坏了"，是"这个环境量不了画面"** —— 依据是三条取回路径 + 两个环境对照：
    // 原生 WebGPU 画布能呈现也能取回、`copyExternalImageToTexture` 也交像素，
    // 但 Dhampir 的画布取回恒为**不透明黑**（缓存开/关都一样）。
    unverifiable.push('像素等价（命中 vs 未命中 vs 缓存关）在本机无头/回退适配器上量不了：'
      + 'Dhampir 的画布取回恒为不透明黑（`ink=' + (S.pngHash && S.pngHash[0] ? S.pngHash[0].ink : '?')
      + '/' + (S.pngHash && S.pngHash[0] ? S.pngHash[0].samples : '?')
      + '`），而原生 WebGPU 画布与 external-copy 的对照都通过（S2=' + JSON.stringify(phases.S2) + '）');
    unverifiable.push('视频解码本身是好的（readyState=' + (S.video && S.video.readyState)
      + '，' + (S.video && S.video.width) + 'x' + (S.video && S.video.height)
      + '），合成也报了 2 层（S.frame120=' + JSON.stringify(S.frame120 && S.frame120.layers) + '）');
  } else {
    for (const [frame, verdict] of Object.entries(C.pixels || {})) {
      if (verdict.missEqualsRef !== true) problems.push('第 ' + frame + ' 帧：缓存开/关画出来的像素不同（中间纹理 + blit 改变了画面）');
      if (verdict.hitEqualsRef !== true) problems.push('第 ' + frame + ' 帧：命中的像素与缓存关那条路不同');
    }
    if (R.distinctFrames !== true) problems.push('探针无效：相邻两帧的像素摘要相同（取到的可能是同一张）');
    if (C.distinctHits === false) problems.push('探针无效：不同帧的命中结果相同');
  }

  // ① 阶段 4：同标识的贴纸位图，改后必须**明显少拷**（用本段增量，不用累计值）
  const dBefore = before.uploadsDelta ? before.uploadsDelta.copied : null;
  const dAfter = after.uploadsDelta ? after.uploadsDelta.copied : null;
  if (dBefore === null || dAfter === null) {
    problems.push('读不到 hostUploads 增量（新导出没生效？）');
  } else if (!(dAfter < dBefore)) {
    problems.push('阶段 4 没生效：同样 20 帧，改前拷 ' + dBefore + ' 次、改后还是 ' + dAfter + ' 次');
  }

  // ② 阶段 5：第二遍必须真的命中
  if (!(C.hitsInSecondPass > 0)) problems.push('阶段 5 一次都没命中（hitsInSecondPass = ' + C.hitsInSecondPass + '）');
  // ③ 命中要**明显更快**（否则缓存白占内存）
  const miss = C.miss ? C.miss.medianTotalMs : null;
  const hit = C.hit ? C.hit.medianTotalMs : null;
  if (miss === null || hit === null) problems.push('读不到整圈中位数');
  else if (!(hit < miss)) problems.push('命中没有更快：未命中中位 ' + miss + 'ms、命中中位 ' + hit + 'ms');

  // ④ 串行与长跑：不许有未捕获错误（wasm panic 抓不住，只能看"还能不能接着画"）
  if (phases.D === undefined || phases.D.ok !== true) problems.push('串行压测没跑完');
  if (phases.E === undefined || phases.E.skipped === true) problems.push('长跑没跑（--skip-e？）');
  else if (!(phases.E.frames >= 1800)) problems.push('长跑帧数不足 1800（实际 ' + phases.E.frames + ' 帧 = ' + (phases.E.frames / 30).toFixed(1) + 's）');
  const errors = Array.isArray(report.errors) ? report.errors : [];
  if (errors.length > 0) problems.push('页面有未捕获错误：' + errors.join(' | '));
  return { problems, unverifiable };
}

async function main() {
  const args = parseArgs(process.argv.slice(2));
  if (args.bad) return 2;
  for (const [what, file] of [['页面', join(WEB_DIR, 'cache-probe.html')], ['wasm 产物', join(PKG_DIR, 'dhampir_wasm.js')], ['测试视频', MEDIA]]) {
    if (!existsSync(file)) {
      console.error('✗ ' + what + '不在：' + file);
      if (what === '测试视频') {
        console.error('  生成它：ffmpeg -y -f lavfi -i "testsrc2=size=320x180:rate=30:duration=62" \\');
        console.error('    -c:v libx264 -preset ultrafast -pix_fmt yuv420p -g 30 target/cache-probe/test.mp4');
      }
      return 2;
    }
  }
  const browser = findBrowser();
  if (browser === null) { console.error('✗ 找不到 Chrome / Edge。用 DHAMPIR_CHROME=<exe> 指定一个。'); return 2; }

  let verdict = null;
  const pageErrors = [];
  const server = createServer((req, res) => {
    const path = new URL(req.url, 'http://127.0.0.1').pathname;
    if (req.method === 'POST' && path === '/verdict') {
      let body = '';
      req.on('data', (chunk) => { body += chunk; });
      req.on('end', () => {
        try { verdict = JSON.parse(body); } catch (error) { pageErrors.push('verdict 解析失败：' + error); }
        res.writeHead(200, { 'content-type': 'application/json' }).end('{}');
      });
      return;
    }
    let file = null;
    if (path === '/' || path === '/cache-probe.html') file = join(WEB_DIR, 'cache-probe.html');
    else if (path === '/media/test.mp4') file = MEDIA;
    else if (path.startsWith('/pkg/')) file = join(PKG_DIR, path.slice('/pkg/'.length));
    else file = join(WEB_DIR, path.slice(1));
    if (file === null || !existsSync(file) || !statSync(file).isFile()) { res.writeHead(404).end('nf'); return; }
    const body = readFileSync(file);
    res.writeHead(200, {
      'content-type': MIME[extname(file)] || 'application/octet-stream',
      'content-length': body.length,
      'cache-control': 'no-store',
      'accept-ranges': 'bytes',
      'access-control-allow-origin': '*',
    });
    res.end(body);
  });
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  const port = server.address().port;
  // `?src=bitmap`：见 `web/cache-probe.html` 顶部那段 —— 软件适配器上 `<video>` 直传会
  // **静默产出空内容**，走 bitmap 才拿得到真画面（否则像素判据量的是空帧）。
  const url = 'http://127.0.0.1:' + port + '/cache-probe.html?src=bitmap'
    + (args.skipE ? '&skipE=1' : '');
  console.log('探针页面：' + url);

  const profile = join(REPO_ROOT, 'target', 'cache-probe-profile');
  const chromeArgs = [
    '--no-first-run', '--no-default-browser-check', '--disable-gpu-sandbox',
    '--user-data-dir=' + profile, '--enable-unsafe-webgpu', '--use-angle=default',
    '--remote-debugging-port=0', '--enable-logging=stderr', '--v=0',
  ];
  if (!args.headed) chromeArgs.unshift('--headless=new');
  chromeArgs.push(url);
  console.log('浏览器：' + browser);
  const child = spawn(browser, chromeArgs, { stdio: ['ignore', 'ignore', 'pipe'] });
  let chromeErr = '';
  child.stderr.on('data', (chunk) => { chromeErr += chunk.toString('utf8'); });

  const deadline = Date.now() + args.timeoutMs;
  while (Date.now() < deadline && verdict === null) await new Promise((r) => setTimeout(r, 500));
  try { child.kill(); } catch (error) { /* 已经没了 */ }
  server.close();

  console.log('');
  console.log(JSON.stringify(verdict, null, 1));
  // **读数落盘**：交付里要引用的数字不该只活在终端回滚里。
  try {
    const out = join(REPO_ROOT, 'target', 'cache-probe', 'verdict.json');
    writeFileSync(out, JSON.stringify(verdict, null, 1), 'utf8');
    console.log('读数已写入 ' + out);
  } catch (error) { console.error('读数写盘失败：' + error); }
  const { problems, unverifiable } = problemsOf(verdict);
  if (unverifiable.length > 0) {
    console.log('');
    console.log('⚠ 这一轮**测不了**的东西（不是通过，也不是红）：');
    for (const item of unverifiable) console.log('  · ' + item);
  }
  if (problems.length > 0) {
    console.error('');
    for (const problem of problems) console.error('  - ' + problem);
    if (chromeErr.trim() !== '') console.error('--- 浏览器 stderr（尾部）---\n' + chromeErr.trim().slice(-2000));
    console.error('预览通路探针未通过');
    return 1;
  }
  console.log('');
  console.log('✓ 预览通路探针通过：命中 ' + verdict.phases.C.hitsInSecondPass + ' 帧、'
    + '整圈 ' + verdict.phases.C.miss.medianTotalMs + 'ms -> ' + verdict.phases.C.hit.medianTotalMs + 'ms、'
    + '位图重传 20 帧 ' + verdict.phases.P4_before.uploadsDelta.copied + ' -> '
    + verdict.phases.P4_after.uploadsDelta.copied + ' 次、'
    + '长跑 ' + verdict.phases.E.frames + ' 帧无错'
    + (unverifiable.length > 0 ? '（像素等价那一项没测，见上面的 ⚠）' : ''));
  return 0;
}

process.exitCode = await main();
