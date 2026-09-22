#!/usr/bin/env node
// S3.1「源帧采样策略」实测驱动。
//
// M3 的第一个决策（零拷贝外部纹理 vs copyExternalImageToTexture）按 plan 的口径必须由
// **实测数字**支撑，结论要落文件。手点出来的数字没法复跑、没法被第二个人重算，
// 所以把整条链固定下来：起本地服务 → 无头 Chrome 打开页面 → 页面自己量 → POST 结果
// → 本脚本**先验后信** → 打印 JSON。
//
// 「先验后信」是有意的：页面报什么就信什么，等于让被测者自己出题。这里至少钉住分辨率、
// 帧数、三个基准都在、像素比对覆盖整幅，否则拒绝给结论。
//
// 用法：
//   node scripts/s3-sampling-spike/run.mjs
//   node scripts/s3-sampling-spike/run.mjs --media target/s3/source1080p.mp4 --headed
//   node scripts/s3-sampling-spike/run.mjs --self-test
//
// 退出码：0 实测完成且自洽；1 页面报错或结果不自洽；2 参数/环境问题。

import { createServer } from 'node:http';
import { createReadStream, existsSync, readFileSync, statSync } from 'node:fs';
import { spawn } from 'node:child_process';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const HERE = dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = resolve(HERE, '..', '..');
const PAGE = join(HERE, 'index.html');

const WANT_W = 1920;
const WANT_H = 1080;
const WANT_N = 200;

/** 结果必须自洽才允许拿它出结论。返回问题清单（空 = 通过）。 */
export function validateReport(report, want = { w: WANT_W, h: WANT_H, n: WANT_N }) {
  const problems = [];
  if (report === null || typeof report !== 'object') return ['结果不是对象'];
  if (report.ok !== true) {
    problems.push('页面没有报 ok=true：' + JSON.stringify(report.error === undefined ? '(无 error 字段)' : report.error));
  }
  const v = report.video === undefined ? {} : report.video;
  if (v.videoWidth !== want.w || v.videoHeight !== want.h) {
    problems.push('视频尺寸是 ' + v.videoWidth + 'x' + v.videoHeight + '，要求 ' + want.w + 'x' + want.h);
  }
  for (const key of ['a_import_render', 'b_copy_only', 'b_copy_render']) {
    const s = report[key];
    if (s === undefined || s === null) { problems.push('缺基准 ' + key); continue; }
    if (s.n !== want.n) problems.push(key + ' 的样本数是 ' + s.n + '，要求 ' + want.n);
    if (!(s.mean_ms > 0)) problems.push(key + ' 的 mean_ms 不是正数：' + s.mean_ms);
    if (!(s.p95_ms >= s.median_ms && s.median_ms >= 0)) problems.push(key + ' 的分位数不自洽');
  }
  const p = report.pixels === undefined ? {} : report.pixels;
  if (p.total_bytes !== want.w * want.h * 4) {
    problems.push('像素比对覆盖了 ' + p.total_bytes + ' 字节，要求 ' + (want.w * want.h * 4) + '（整幅 RGBA8）');
  }
  if (typeof p.differing_bytes !== 'number') problems.push('pixels.differing_bytes 不是数');
  // 测量必须**零 WebGPU 错误**：校验失败时渲染会被丢掉，量出来的是"失败调用"的耗时，
  // 而像素上看起来只是"两条路不一样"。这一条是踩过一次才知道要加的。
  const ge = report.gpu_errors;
  if (ge === undefined) problems.push('结果里没有 gpu_errors，无法确认这次测量没有 WebGPU 错误');
  else if (!Array.isArray(ge)) problems.push('gpu_errors 不是数组');
  else if (ge.length > 0) problems.push('这次测量里有 ' + ge.length + ' 条 WebGPU 错误——测的不是有效路径：' + ge[0]);
  return problems;
}

/** 找浏览器：与 capture-harness-screenshot.mjs 同一套口径。 */
export function findBrowser() {
  const pf = process.env.ProgramFiles === undefined ? 'C:\\Program Files' : process.env.ProgramFiles;
  const pfx86 = process.env['ProgramFiles(x86)'] === undefined ? 'C:\\Program Files (x86)' : process.env['ProgramFiles(x86)'];
  const local = process.env.LOCALAPPDATA === undefined ? '' : process.env.LOCALAPPDATA;
  const candidates = [];
  if (process.env.DHAMPIR_CHROME) candidates.push([process.env.DHAMPIR_CHROME, 'env:DHAMPIR_CHROME']);
  candidates.push([join(pf, 'Google', 'Chrome', 'Application', 'chrome.exe'), 'chrome']);
  candidates.push([join(pfx86, 'Google', 'Chrome', 'Application', 'chrome.exe'), 'chrome']);
  if (local !== '') candidates.push([join(local, 'Google', 'Chrome', 'Application', 'chrome.exe'), 'chrome']);
  candidates.push([join(pf, 'Microsoft', 'Edge', 'Application', 'msedge.exe'), 'edge']);
  for (const pair of candidates) if (existsSync(pair[0])) return { path: pair[0], kind: pair[1] };
  return null;
}

function parseArgs(argv) {
  const out = {
    media: join(REPO_ROOT, 'target', 's3', 'source1080p.mp4'),
    headed: false, selfTest: false, timeoutMs: 180000, bad: false,
  };
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    if (arg === '--self-test') out.selfTest = true;
    else if (arg === '--headed') out.headed = true;
    else if (arg === '--media') { out.media = resolve(REPO_ROOT, argv[i + 1] === undefined ? '' : argv[i + 1]); i += 1; }
    else if (arg === '--timeout') { out.timeoutMs = Number(argv[i + 1]) * 1000; i += 1; }
    else { console.error('✗ 不认识的参数：' + arg); out.bad = true; }
  }
  return out;
}

function selfTest() {
  const failures = [];
  let count = 0;
  const expect = (name, cond, detail) => { count += 1; if (!cond) failures.push(name + '：' + detail); };
  const good = {
    ok: true,
    video: { videoWidth: 1920, videoHeight: 1080 },
    a_import_render: { n: 200, mean_ms: 1, median_ms: 1, p95_ms: 1 },
    b_copy_only: { n: 200, mean_ms: 2, median_ms: 2, p95_ms: 2 },
    b_copy_render: { n: 200, mean_ms: 3, median_ms: 3, p95_ms: 3 },
    pixels: { total_bytes: 1920 * 1080 * 4, differing_bytes: 0 },
    gpu_errors: [],
  };
  expect('自洽的结果必须过', validateReport(good).length === 0, validateReport(good).join(' | '));
  expect('页面报错必须被抓', validateReport({ ...good, ok: false, error: 'boom' }).length > 0, '放过了 ok=false');
  expect('分辨率不对必须被抓', validateReport({ ...good, video: { videoWidth: 1280, videoHeight: 720 } }).length > 0, '放过了 720p');
  expect('样本数不够必须被抓', validateReport({ ...good, b_copy_only: { n: 5, mean_ms: 1, median_ms: 1, p95_ms: 1 } }).length > 0, '放过了 n=5');
  expect('像素覆盖不全必须被抓', validateReport({ ...good, pixels: { total_bytes: 16, differing_bytes: 0 } }).length > 0, '放过了 16 字节');
  expect('mean 非正必须被抓', validateReport({ ...good, b_copy_only: { n: 200, mean_ms: 0, median_ms: 0, p95_ms: 0 } }).length > 0, '放过了 mean=0');
  expect('有 WebGPU 错误必须被抓', validateReport({ ...good, gpu_errors: ['boom'] }).length > 0, '放过了 gpu_errors 非空');
  expect('缺 gpu_errors 必须被抓', validateReport({ ...good, gpu_errors: undefined }).length > 0, '放过了缺字段');
  if (failures.length > 0) { for (const f of failures) console.error('  - ' + f); return 1; }
  console.log('✓ 驱动自检通过（' + count + ' 条断言：结果自洽性校验）');
  return 0;
}

async function main() {
  const args = parseArgs(process.argv.slice(2));
  if (args.bad) return 2;
  if (args.selfTest) return selfTest();
  if (!existsSync(PAGE)) { console.error('✗ 页面不在：' + PAGE); return 2; }
  if (!existsSync(args.media)) {
    console.error('✗ 素材不在：' + args.media);
    console.error('  先造一份（1080p、60fps、每 60 帧一个关键帧）：');
    console.error('  ffmpeg -y -f lavfi -i testsrc2=size=1920x1080:rate=60 -t 8 -c:v libx264 -preset veryfast -g 60 -keyint_min 60 -sc_threshold 0 -crf 23 -pix_fmt yuv420p target/s3/source1080p.mp4');
    return 2;
  }
  const browser = findBrowser();
  if (browser === null) { console.error('✗ 找不到 Chrome / Edge。用 DHAMPIR_CHROME=<exe> 指定一个。'); return 2; }

  const html = readFileSync(PAGE);
  const mediaSize = statSync(args.media).size;
  let settle = null;
  const gotResult = new Promise((res) => { settle = res; });

  const server = createServer((req, res) => {
    if (req.method === 'POST' && req.url === '/result') {
      let body = '';
      req.on('data', (chunk) => { body += chunk; });
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
    if (req.url === '/media/source1080p.mp4') {
      res.writeHead(200, { 'content-type': 'video/mp4', 'content-length': String(mediaSize) });
      createReadStream(args.media).pipe(res);
      return;
    }
    res.writeHead(404).end('not found');
  });
  await new Promise((r) => server.listen(0, '127.0.0.1', r));
  const port = server.address().port;
  const url = 'http://127.0.0.1:' + port + '/';

  const profile = join(REPO_ROOT, 'target', 's3', 'chrome-profile');
  const chromeArgs = [];
  if (!args.headed) chromeArgs.push('--headless=new');
  chromeArgs.push('--user-data-dir=' + profile);
  chromeArgs.push('--no-first-run');
  chromeArgs.push('--no-default-browser-check');
  chromeArgs.push('--disable-extensions');
  chromeArgs.push('--window-size=1600,1200');
  chromeArgs.push(url);
  console.log('→ ' + browser.kind + '：' + browser.path);
  console.log('→ ' + url + '（素材 ' + Math.round(mediaSize / 1024) + ' KB）');
  const child = spawn(browser.path, chromeArgs, { stdio: ['ignore', 'ignore', 'pipe'] });
  let chromeErr = '';
  child.stderr.on('data', (chunk) => { chromeErr += chunk.toString('utf8'); });

  const timeout = new Promise((r) => setTimeout(() => r('__timeout__'), args.timeoutMs));
  const report = await Promise.race([gotResult, timeout]);
  try { child.kill(); } catch (e) { /* 已经退了 */ }
  server.close();

  if (report === '__timeout__') {
    console.error('✗ 等结果超时（' + args.timeoutMs + 'ms）。Chrome stderr 末尾：');
    console.error(chromeErr.trim().split('\n').slice(-12).join('\n'));
    return 1;
  }

  const problems = validateReport(report, { w: WANT_W, h: WANT_H, n: WANT_N });
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


