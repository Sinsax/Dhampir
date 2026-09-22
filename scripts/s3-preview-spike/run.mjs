#!/usr/bin/env node
// M3 预览链路的实测驱动：起本地服务 → 无头 Chrome 打开页面 → 页面自己跑 → POST 结果
// → 本脚本**先验后信** → 打印 JSON。
//
// 页面做的事：Rust 侧（wasm）用 copy_external_image_to_texture + BlitRenderer 渲染一帧并读回算
// FNV-1a 64；JS 侧走同一条路再算一遍；两者摘要必须逐位相同。再加一次 canvas 上屏。
//
// 「先验后信」的理由与 S3.1 的驱动一样：页面报什么就信什么，等于让被测者自己出题。
//
// 用法：
//   node scripts/s3-preview-spike/run.mjs
//   node scripts/s3-preview-spike/run.mjs --self-test
//   node scripts/s3-preview-spike/run.mjs --media target/s3/proxy720p.mp4 --headed
//
// 退出码：0 实测完成且自洽；1 页面报错或结果不自洽；2 参数/环境问题。

import { createServer } from 'node:http';
import { createReadStream, existsSync, readFileSync, statSync } from 'node:fs';
import { spawn } from 'node:child_process';
import { dirname, extname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const HERE = dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = resolve(HERE, '..', '..');
const PAGE = join(HERE, 'index.html');
const PKG_DIR = join(REPO_ROOT, 'crates', 'dhampir-wasm', 'www', 'pkg');

const WANT_W = 1280;
const WANT_H = 720;

const MIME = { '.js': 'text/javascript', '.wasm': 'application/wasm', '.json': 'application/json' };

/** 结果必须自洽才允许拿它出结论。返回问题清单（空 = 通过）。 */
export function validateReport(report, want = { w: WANT_W, h: WANT_H }) {
  const problems = [];
  if (report === null || typeof report !== 'object') return ['结果不是对象'];
  if (report.ok !== true) problems.push('页面没有报 ok=true：' + JSON.stringify(report.error === undefined ? '(无 error 字段)' : report.error));
  if (report.wasm_loaded !== true) problems.push('wasm 模块没加载成功');
  const v = report.video === undefined ? {} : report.video;
  if (v.w !== want.w || v.h !== want.h) problems.push('视频尺寸是 ' + v.w + 'x' + v.h + '，要求 ' + want.w + 'x' + want.h);
  const rust = report.rust === undefined ? {} : report.rust;
  const js = report.js === undefined ? {} : report.js;
  const expectedBytes = want.w * want.h * 4;
  if (rust.bytes !== expectedBytes) problems.push('Rust 侧读了 ' + rust.bytes + ' 字节，要求 ' + expectedBytes);
  if (js.bytes !== expectedBytes) problems.push('JS 侧读了 ' + js.bytes + ' 字节，要求 ' + expectedBytes);
  if (typeof rust.digest !== 'string' || !/^[0-9a-f]{16}$/.test(rust.digest)) problems.push('Rust 侧摘要不像 16 位十六进制：' + JSON.stringify(rust.digest));
  if (report.digests_match !== true) problems.push('两条路的摘要不同：rust=' + rust.digest + ' js=' + js.digest);
  const ge = report.js_gpu_errors;
  if (!Array.isArray(ge)) problems.push('js_gpu_errors 不是数组');
  else if (ge.length > 0) problems.push('JS 侧有 ' + ge.length + ' 条 WebGPU 错误：' + ge[0]);
  if (report.canvas_draw !== 'ok') problems.push('canvas 上屏没成功：' + JSON.stringify(report.canvas_draw));
  // T3.2：分离器与 WebCodecs 解码。
  const d = report.demux === undefined ? {} : report.demux;
  if (d.width !== want.w || d.height !== want.h) problems.push('分离器报的视频尺寸是 ' + d.width + 'x' + d.height);
  if (d.samples !== 480) problems.push('分离器报的样本数是 ' + d.samples + '，要求 480（8s × 60fps）');
  if (d.samples_parsed !== d.samples) problems.push('样本表 JSON 的条数 ' + d.samples_parsed + ' 与元信息 ' + d.samples + ' 不一致');
  if (d.sync_count !== 8) problems.push('同步样本数是 ' + d.sync_count + '，要求 8');
  if (JSON.stringify(d.first_sync) !== JSON.stringify([0, 60, 120])) problems.push('前三个同步样本应当是 [0,60,120]，得到 ' + JSON.stringify(d.first_sync));
  if (d.sync_start_of_130 !== 120) problems.push('第 130 帧应当从样本 120 起解（同步样本回溯），得到 ' + d.sync_start_of_130);
  if (report.decode === undefined) problems.push('缺 decode：解码那一步没跑');
  else {
    if (report.decode.error !== null) problems.push('解码报错：' + JSON.stringify(report.decode.error));
    if (!(report.decode.produced > 0)) problems.push('解码没有产出任何帧');
  }
  if (report.seek === undefined) problems.push('缺 seek：同步样本回溯的等价性没验');
  else {
    if (report.seek.target_frame !== 130) problems.push('seek 目标帧不是 130');
    if (!(report.seek.from_start_frames > report.seek.from_sync_frames)) problems.push('从头解应当比从同步样本解产出更多帧');
    if (report.seek.equivalent !== true) problems.push('「从同步样本解到第 130 帧」与「从头解到第 130 帧」末帧不一致：' + report.seek.from_sync_digest + ' vs ' + report.seek.from_start_digest);
  }
  if (report.frame0_matches_video_path !== true) {
    problems.push('自己分离出来的第 0 帧与 <video> 那条路的第 0 帧不一致：' + JSON.stringify(report.decoded_frame0));
  }
  // T3.4 接线 + T3.5 测量
  if (report.cache === undefined) problems.push('缺 cache：T3.4 的接线没跑');
  else {
    const c = report.cache;
    if (!(c.texture_capacity > 0)) problems.push('按预算换算出的纹理张数为 0');
    if (c.initial === undefined || c.initial.ram_bytes !== 0) problems.push('缓存初始账目不是空的');
    if (!(c.evicted_frames > 0)) problems.push('一帧都没淘汰——LRU 没被真正压到（预算给大了？）');
    if (!(c.evicted_textures > 0)) problems.push('一张纹理都没淘汰');
    if (c.final === undefined) problems.push('缺 cache.final');
    else {
      if (c.final.ram_over === true) problems.push('RAM 记账超预算：LRU 没把账压住');
      if (c.final.vram_over === true) problems.push('VRAM 记账超预算');
      if (!(c.final.ram_bytes <= c.ram_budget)) problems.push('RAM 用度 ' + c.final.ram_bytes + ' 超过预算 ' + c.ram_budget);
    }
  }
  if (report.perf === undefined) problems.push('缺 perf：T3.5 的测量没跑');
  else {
    const perf = report.perf;
    if (perf.decode === undefined || !(perf.decode.n >= 5)) problems.push('解码样本数不足：' + JSON.stringify(perf.decode));
    else if (!(perf.decode.p95 > 0)) problems.push('decode p95 不是正数');
    if (perf.upload_render === undefined || !(perf.upload_render.p95 > 0)) problems.push('upload_render 没量到');
  }
  if (report.playback === undefined) problems.push('缺 playback：流水线播放没跑');
  else {
    const pb = report.playback;
    if (pb.frames !== pb.requested) problems.push('播放丢了帧：产出 ' + pb.frames + ' / 请求 ' + pb.requested);
    if (!(pb.fps > 0)) problems.push('fps 不是正数：' + pb.fps);
    if (!(pb.wall_ms > 0)) problems.push('播放耗时不是正数');
    if (pb.cache_final === undefined || pb.cache_final.vram_over === true) problems.push('播放期间 VRAM 记账超预算');
    if (pb.vram_textures_alive > 0) problems.push('播放结束后还留着 ' + pb.vram_textures_alive + ' 张纹理没释放');
  }
  return problems;
}

export function findBrowser() {
  const pf = process.env.ProgramFiles === undefined ? 'C:\\Program Files' : process.env.ProgramFiles;
  const pfx86 = process.env['ProgramFiles(x86)'] === undefined ? 'C:\\Program Files (x86)' : process.env['ProgramFiles(x86)'];
  const local = process.env.LOCALAPPDATA === undefined ? '' : process.env.LOCALAPPDATA;
  const candidates = [];
  if (process.env.DHAMPIR_CHROME) candidates.push([process.env.DHAMPIR_CHROME, 'env:DHAMPIR_CHROME']);
  candidates.push([join(pf, 'Google', 'Chrome', 'Application', 'chrome.exe'), 'chrome']);
  candidates.push([join(pfx86, 'Google', 'Chrome', 'Application', 'chrome.exe'), 'chrome']);
  if (local !== '') candidates.push([join(local, 'Google', 'Chrome', 'Application', 'chrome.exe'), 'chrome']);
  for (const pair of candidates) if (existsSync(pair[0])) return { path: pair[0], kind: pair[1] };
  return null;
}

function parseArgs(argv) {
  const out = {
    media: join(REPO_ROOT, 'target', 's3', 'proxy720p.mp4'),
    headed: false, selfTest: false, timeoutMs: 600000, bad: false,
    width: WANT_W, height: WANT_H,
  };
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    if (arg === '--self-test') out.selfTest = true;
    else if (arg === '--headed') out.headed = true;
    else if (arg === '--media') { out.media = resolve(REPO_ROOT, argv[i + 1] === undefined ? '' : argv[i + 1]); i += 1; }
    else if (arg === '--timeout') { out.timeoutMs = Number(argv[i + 1]) * 1000; i += 1; }
    else if (arg === '--width') { out.width = Number(argv[i + 1]); i += 1; }
    else if (arg === '--height') { out.height = Number(argv[i + 1]); i += 1; }
    else { console.error('✗ 不认识的参数：' + arg); out.bad = true; }
  }
  return out;
}

function selfTest() {
  const failures = [];
  let count = 0;
  const expect = (name, cond, detail) => { count += 1; if (!cond) failures.push(name + '：' + detail); };
  const bytes = 1280 * 720 * 4;
  const good = {
    ok: true, wasm_loaded: true, video: { w: 1280, h: 720 },
    rust: { width: 1280, height: 720, bytes, digest: '0123456789abcdef' },
    js: { width: 1280, height: 720, bytes, digest: '0123456789abcdef' },
    digests_match: true, js_gpu_errors: [], canvas_draw: 'ok',
    demux: { width: 1280, height: 720, samples: 480, samples_parsed: 480, sync_count: 8, first_sync: [0, 60, 120], sync_start_of_130: 120 },
    decode: { produced: 6, error: null },
    decoded_frame0: { bytes: 3686400, digest: '0123456789abcdef' },
    frame0_matches_video_path: true,
    seek: { target_frame: 130, sync_from: 120, from_sync_frames: 11, from_start_frames: 131, equivalent: true },
    cache: {
      vram_budget: 16777216, ram_budget: 16777216, frame_bytes: 3686400, texture_capacity: 4,
      initial: { ram_bytes: 0, vram_bytes: 0 },
      evicted_frames: 6, evicted_textures: 6, open_frames_after: 4,
      final: { ram_bytes: 14745600, vram_bytes: 14745600, ram_over: false, vram_over: false },
    },
    perf: { targets: 10, decode: { n: 10, p50: 12, p95: 30, max: 40 }, upload_render: { n: 10, p50: 2, p95: 4, max: 6 } },
    playback: {
      frames: 240, requested: 240, wall_ms: 1000, ideal_ms_at_60fps: 4000, fps: 240, drop_ratio: 0,
      heap_before: 1000, heap_peak: 2000, heap_after: 1500, vram_textures_alive: 0,
      cache_final: { vram_over: false, ram_over: false },
    },
  };
  expect('自洽的结果必须过', validateReport(good).length === 0, validateReport(good).join(' | '));
  expect('摘要不同必须被抓', validateReport({ ...good, digests_match: false }).length > 0, '放过了摘要不同');
  expect('wasm 没加载必须被抓', validateReport({ ...good, wasm_loaded: false }).length > 0, '放过了 wasm_loaded=false');
  expect('分辨率不对必须被抓', validateReport({ ...good, video: { w: 1920, h: 1080 } }).length > 0, '放过了 1080p');
  expect('字节数不对必须被抓', validateReport({ ...good, rust: { ...good.rust, bytes: 16 } }).length > 0, '放过了 16 字节');
  expect('canvas 失败必须被抓', validateReport({ ...good, canvas_draw: 'err: boom' }).length > 0, '放过了 canvas 失败');
  expect('WebGPU 错误必须被抓', validateReport({ ...good, js_gpu_errors: ['boom'] }).length > 0, '放过了 gpu 错误');
  expect('第 0 帧不一致必须被抓', validateReport({ ...good, frame0_matches_video_path: false }).length > 0, '放过了第 0 帧不一致');
  expect('同步样本回溯错必须被抓', validateReport({ ...good, demux: { ...good.demux, sync_start_of_130: 0 } }).length > 0, '放过了回溯错');
  expect('末帧不一致必须被抓', validateReport({ ...good, seek: { ...good.seek, equivalent: false } }).length > 0, '放过了末帧不一致');
  expect('一帧没淘汰必须被抓', validateReport({ ...good, cache: { ...good.cache, evicted_frames: 0 } }).length > 0, '放过了零淘汰');
  expect('播放丢帧必须被抓', validateReport({ ...good, playback: { ...good.playback, frames: 239 } }).length > 0, '放过了丢帧');
  expect('播完还留纹理必须被抓', validateReport({ ...good, playback: { ...good.playback, vram_textures_alive: 3 } }).length > 0, '放过了纹理泄漏');
  expect('记账超预算必须被抓', validateReport({ ...good, cache: { ...good.cache, final: { ...good.cache.final, ram_over: true } } }).length > 0, '放过了超预算');
  if (failures.length > 0) { for (const f of failures) console.error('  - ' + f); return 1; }
  console.log('✓ 驱动自检通过（' + count + ' 条断言：结果自洽性校验）');
  return 0;
}

async function main() {
  const args = parseArgs(process.argv.slice(2));
  if (args.bad) return 2;
  if (args.selfTest) return selfTest();
  if (!existsSync(PAGE)) { console.error('✗ 页面不在：' + PAGE); return 2; }
  if (!existsSync(args.media)) { console.error('✗ 素材不在：' + args.media); return 2; }
  if (!existsSync(join(PKG_DIR, 'dhampir_wasm.js'))) {
    console.error('✗ wasm 产物不在：' + PKG_DIR);
    console.error('  先构建：wasm-pack build crates/dhampir-wasm --target web --out-dir www/pkg --dev');
    return 2;
  }
  const browser = findBrowser();
  if (browser === null) { console.error('✗ 找不到 Chrome / Edge。用 DHAMPIR_CHROME=<exe> 指定一个。'); return 2; }

  const html = readFileSync(PAGE);
  const mediaSize = statSync(args.media).size;
  let settle = null;
  let lastProgress = '(页面还没报进度)';
  const gotResult = new Promise((res) => { settle = res; });

  const server = createServer((req, res) => {
    if (req.method === 'POST' && req.url === '/progress') {
      let body = '';
      req.on('data', (c) => { body += c; });
      req.on('end', () => { lastProgress = body; res.writeHead(204).end(); });
      return;
    }
    if (req.method === 'POST' && req.url === '/result') {
      let body = '';
      req.on('data', (c) => { body += c; });
      req.on('end', () => {
        res.writeHead(204).end();
        let parsed = null;
        try { parsed = JSON.parse(body); } catch (e) { parsed = { ok: false, error: '结果不是合法 JSON：' + e.message }; }
        settle(parsed);
      });
      return;
    }
    if (req.url === '/' || req.url.startsWith('/?')) {
      res.writeHead(200, { 'content-type': 'text/html; charset=utf-8' }).end(html);
      return;
    }
    if (req.url === '/media/proxy.mp4') {
      res.writeHead(200, { 'content-type': 'video/mp4', 'content-length': String(mediaSize) });
      createReadStream(args.media).pipe(res);
      return;
    }
    if (req.url.startsWith('/pkg/')) {
      const name = req.url.slice('/pkg/'.length).split('?')[0];
      const path = join(PKG_DIR, name);
      if (!path.startsWith(PKG_DIR) || !existsSync(path)) { res.writeHead(404).end('not found'); return; }
      res.writeHead(200, { 'content-type': MIME[extname(path)] === undefined ? 'application/octet-stream' : MIME[extname(path)] });
      createReadStream(path).pipe(res);
      return;
    }
    res.writeHead(404).end('not found');
  });
  await new Promise((r) => server.listen(0, '127.0.0.1', r));
  const port = server.address().port;
  const url = 'http://127.0.0.1:' + port + '/';

  const profile = join(REPO_ROOT, 'target', 's3', 'preview-chrome-profile');
  const chromeArgs = [];
  if (!args.headed) chromeArgs.push('--headless=new');
  chromeArgs.push('--user-data-dir=' + profile);
  chromeArgs.push('--no-first-run');
  chromeArgs.push('--no-default-browser-check');
  chromeArgs.push('--disable-extensions');
  chromeArgs.push(url);
  console.log('→ ' + browser.kind + '：' + browser.path);
  console.log('→ ' + url + '（素材 ' + Math.round(mediaSize / 1024) + ' KB）');
  const child = spawn(browser.path, chromeArgs, { stdio: ['ignore', 'ignore', 'pipe'] });
  let chromeErr = '';
  child.stderr.on('data', (c) => { chromeErr += c.toString('utf8'); });

  const timeout = new Promise((r) => setTimeout(() => r('__timeout__'), args.timeoutMs));
  const report = await Promise.race([gotResult, timeout]);
  try { child.kill(); } catch (e) { /* 已经退了 */ }
  server.close();

  if (report === '__timeout__') {
    console.error('✗ 等结果超时（' + args.timeoutMs + 'ms）。页面最后报到的进度：' + lastProgress);
    console.error('Chrome stderr 末尾：');
    console.error(chromeErr.trim().split(String.fromCharCode(10)).slice(-10).join(String.fromCharCode(10)));
    return 1;
  }
  const problems = validateReport(report, { w: args.width, h: args.height });
  console.log('');
  if (problems.length > 0) {
    console.error('✗ 结果不自洽，拒绝给结论：');
    for (const p of problems) console.error('  - ' + p);
    console.error('原始结果：' + JSON.stringify(report));
    return 1;
  }
  console.log(JSON.stringify(report, null, 2));
  console.log('');
  console.log('✓ 实测完成且自洽');
  return 0;
}

if (process.argv[1] !== undefined && resolve(process.argv[1]) === resolve(fileURLToPath(import.meta.url))) {
  process.exitCode = await main();
}
