#!/usr/bin/env node
// web/ 的静态服务器 + 程序化验收驱动。
//
// **为什么不用 Vite / webpack / pnpm**：
//   1. wasm-pack --target web 的产物本身就是 ES module，浏览器可直接 import，不需要打包；
//   2. 本机 pnpm 是坏的（WinGet shim 指向缺失的包目录）；
//   3. 少一层构建就少一层"改了没生效"——调试预览问题时这很值钱。
//
// 用法：
//   node scripts/web-check.mjs --serve     只起服务，人工看
//   node scripts/web-check.mjs --probe     W0 验收：工程帧能不能上 canvas
//   node scripts/web-check.mjs             app 验收：逐帧导出 + FFmpeg 编码成 mp4

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
const VIDEO_PATH = join(REPO_ROOT, 'milestones', 'edited-milestone.mp4');

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
const MEDIA = 'target/s3/proxy1080p.mp4';

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

const state = { frames: 0, done: false, failed: null, settle: null, diag: [] };
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
  if (req.method === 'POST' && path === '/diag') {
    readBody((body) => {
      state.diag.push(body.toString());
      res.writeHead(204).end();
    });
    return;
  }
  if (req.method === 'POST' && path === '/export-done') {
    readBody((body) => {
      state.done = true;
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
  else if (path.startsWith('/pkg/')) file = join(PKG_DIR, path.slice('/pkg/'.length));
  else if (path === '/sample-project.json') file = join(REPO_ROOT, 'fixtures', 'sample-project.json');
  else if (path === '/media/proxy.mp4') file = join(REPO_ROOT, MEDIA);
  else if (path === '/app.js' || path === '/engine.js') file = join(WEB_DIR, path.slice(1));
  else if (path.startsWith('/export/')) file = join(WEB_DIR, path.slice(1));

  if (file === null || !existsSync(file) || !statSync(file).isFile()) {
    res.writeHead(404).end('not found: ' + path);
    return;
  }
  res.writeHead(200, { 'content-type': MIME[extname(file)] || 'application/octet-stream' });
  createReadStream(file).pipe(res);
});

await new Promise((resolveListen) => server.listen(0, '127.0.0.1', resolveListen));
const port = server.address().port;
const suffix = mode === 'probe' ? '/probe.html' : mode === 'app' ? '/?export=1' : '/';
const url = 'http://127.0.0.1:' + port + suffix;
console.log('→ ' + url);

if (mode === 'serve') {
  console.log('（只起服务；Ctrl+C 结束）');
} else {
  const timeoutMs = mode === 'app' ? 300000 : 90000;
  const profile = join(REPO_ROOT, 'target', 'web-check-profile');
  const child = spawn(findChrome(), [
    '--headless=new', '--disable-gpu-sandbox', '--no-first-run', '--no-default-browser-check',
    '--user-data-dir=' + profile, '--enable-unsafe-webgpu', '--use-angle=default', url,
  ], { stdio: ['ignore', 'ignore', 'pipe'] });
  let stderr = '';
  child.stderr.on('data', (chunk) => { stderr += chunk; });
  const timer = setTimeout(() => { try { child.kill(); } catch {} state.settle(); }, timeoutMs);

  await finished;
  clearTimeout(timer);
  try { child.kill(); } catch {}
  server.close();

  if (mode === 'probe') {
    reportProbe();
  } else if (mode === 'app') {
    reportApp(stderr);
  }
}

/** W0 验收：工程帧能不能上 canvas（判据见 plan 里 W0 那一段）。 */
function reportProbe() {
  if (state.result === undefined) {
    console.error('✗ 页面没有回报结果（超时？）');
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
  if (problems.length > 0) { for (const p of problems) console.error('  - ' + p); process.exitCode = 1; return; }
  console.log(JSON.stringify(report, null, 2));
  console.log('');
  console.log('✓ W0 验收通过（工程帧能上 canvas，且由时间线驱动）');
}

/** app 验收：逐帧导出 → FFmpeg 编码 → ffprobe 核对帧数 → 出里程碑视频。 */
function reportApp(stderr) {
  if (state.failed !== null) { console.error('✗ 页面报导出失败：' + state.failed); process.exitCode = 1; return; }
  if (!state.done) { console.error('✗ 等导出完成超时（收到 ' + state.frames + ' 帧）'); console.error(stderr.slice(-500)); process.exitCode = 1; return; }

  const expected = expectedFrames();
  console.log('收到帧数 ' + state.frames + '，期望 ' + expected);

  // 用工程的时间基定帧率：整数帧号 -> 有理数帧率 -> 编码器要的浮点，只在这一步换算。
  const fps = frameRate();
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

  const probe = spawnSync('ffprobe', [
    '-v', 'error', '-select_streams', 'v:0', '-count_frames',
    '-show_entries', 'stream=nb_read_frames,width,height,avg_frame_rate',
    '-of', 'json', VIDEO_PATH,
  ], { encoding: 'utf8' });
  const info = JSON.parse(probe.stdout).streams[0];
  const encodedFrames = Number(String(info.nb_read_frames).trim());
  const bytes = statSync(VIDEO_PATH).size;

  const problems = [];
  if (state.frames !== expected) problems.push('收到帧数 ' + state.frames + ' 与工程长度 ' + expected + ' 不符');
  if (encodedFrames !== expected) problems.push('编码后帧数 ' + encodedFrames + ' 与期望 ' + expected + ' 不符');
  if (!(bytes > 0)) problems.push('成片字节数为 0');

  const summary = {
    video: VIDEO_PATH.replace(REPO_ROOT + '\\', ''),
    bytes,
    frames: encodedFrames,
    expected,
    size: info.width + 'x' + info.height,
    fps: info.avg_frame_rate,
    frameRate: fps,
  };
  if (problems.length > 0) {
    for (const p of problems) console.error('  - ' + p);
    console.error(JSON.stringify(summary, null, 2));
    process.exitCode = 1;
    return;
  }
  console.log(JSON.stringify(summary, null, 2));
  console.log('');
  console.log('✓ app 验收通过：逐帧导出 -> FFmpeg 编码 -> 帧数与工程一致');
}

/** 样本工程的时间线长度 = 末位帧号 + 1（帧区间左闭右开）。 */
function expectedFrames() {
  const project = JSON.parse(readFileSync(join(REPO_ROOT, 'fixtures', 'sample-project.json'), 'utf8'));
  let end = 0;
  for (const track of project.tracks) for (const clip of track.clips) {
    end = Math.max(end, clip.track_at + clip.duration);
  }
  return end;
}

function frameRate() {
  const project = JSON.parse(readFileSync(join(REPO_ROOT, 'fixtures', 'sample-project.json'), 'utf8'));
  return project.timebase.num / project.timebase.den;
}
