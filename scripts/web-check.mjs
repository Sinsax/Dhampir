#!/usr/bin/env node
// web/ 的最小静态服务器 + 程序化验收驱动。
//
// **为什么不用 Vite / webpack / pnpm**：
//   1. wasm-pack --target web 的产物本身就是 ES module，浏览器可直接 import，不需要打包；
//   2. 本机 pnpm 是坏的（WinGet shim 指向缺失的包目录），引 workspace 会平白多一个"第一天就红"；
//   3. 少一层构建就少一层"改了没生效"——调试预览问题时这很值钱。
//
// 用法：
//   node scripts/web-check.mjs --serve            只起服务，人工看
//   node scripts/web-check.mjs                    起服务 + 无头 Chrome 跑程序化验收

import { createServer } from 'node:http';
import { createReadStream, existsSync, readFileSync, statSync } from 'node:fs';
import { spawn } from 'node:child_process';
import { extname, join, resolve } from 'node:path';
import { dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const HERE = dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = resolve(HERE, '..');
const WEB_DIR = join(REPO_ROOT, 'web');
const PKG_DIR = join(REPO_ROOT, 'crates', 'dhampir-wasm', 'www', 'pkg');

const MIME = {
  '.html': 'text/html; charset=utf-8',
  '.js': 'text/javascript; charset=utf-8',
  '.mjs': 'text/javascript; charset=utf-8',
  '.json': 'application/json; charset=utf-8',
  '.wasm': 'application/wasm',
  '.mp4': 'video/mp4',
  '.css': 'text/css; charset=utf-8',
};

function findChrome() {
  const candidates = [
    'C:/Program Files/Google/Chrome/Application/chrome.exe',
    'C:/Program Files (x86)/Google/Chrome/Application/chrome.exe',
    '/usr/bin/google-chrome',
  ];
  for (const path of candidates) if (existsSync(path)) return path;
  throw new Error('找不到 Chrome');
}

const args = { serveOnly: process.argv.includes('--serve'), media: 'target/s3/proxy1080p.mp4', timeoutMs: 90000 };

const server = createServer((req, res) => {
  const url = new URL(req.url, 'http://127.0.0.1');
  const path = url.pathname;

  if (req.method === 'POST' && path === '/result') {
    let body = '';
    req.on('data', (chunk) => { body += chunk; });
    req.on('end', () => {
      res.writeHead(204).end();
      if (settle !== null) settle(body);
    });
    return;
  }

  let file = null;
  if (path === '/' || path === '/index.html') file = join(WEB_DIR, 'index.html');
  else if (path === '/probe.html') file = join(WEB_DIR, 'probe.html');
  else if (path.startsWith('/pkg/')) file = join(PKG_DIR, path.slice('/pkg/'.length));
  else if (path === '/sample-project.json') file = join(REPO_ROOT, 'fixtures', 'sample-project.json');
  else if (path === '/media/proxy.mp4') file = join(REPO_ROOT, args.media);

  if (file === null || !existsSync(file) || !statSync(file).isFile()) {
    res.writeHead(404).end('not found: ' + path);
    return;
  }
  res.writeHead(200, { 'content-type': MIME[extname(file)] || 'application/octet-stream' });
  createReadStream(file).pipe(res);
});

let settle = null;
const result = new Promise((resolveResult) => { settle = resolveResult; });

await new Promise((resolveListen) => server.listen(0, '127.0.0.1', resolveListen));
const port = server.address().port;
const url = 'http://127.0.0.1:' + port + '/probe.html';
console.log('→ ' + url);

if (args.serveOnly) {
  console.log('（只起服务；Ctrl+C 结束）');
} else {
  const profile = join(REPO_ROOT, 'target', 'web-check-profile');
  const child = spawn(findChrome(), [
    '--headless=new', '--disable-gpu-sandbox', '--no-first-run', '--no-default-browser-check',
    '--user-data-dir=' + profile, '--enable-unsafe-webgpu', '--use-angle=default', url,
  ], { stdio: ['ignore', 'ignore', 'pipe'] });
  let stderr = '';
  child.stderr.on('data', (chunk) => { stderr += chunk; });

  const timer = setTimeout(() => {
    try { child.kill(); } catch {}
    console.error('✗ 等结果超时（' + args.timeoutMs + 'ms）');
    console.error(stderr.slice(-600));
    process.exitCode = 1;
    server.close();
  }, args.timeoutMs);

  const body = await result;
  clearTimeout(timer);
  try { child.kill(); } catch {}
  const report = JSON.parse(body);
  const problems = validate(report);
  if (problems.length > 0) {
    for (const problem of problems) console.error('  - ' + problem);
    console.error('原始结果：' + body);
    process.exitCode = 1;
  } else {
    console.log(JSON.stringify(report, null, 2));
    console.log('');
    console.log('✓ 工程预览上屏验收通过');
  }
  server.close();
}

/** 验收判据。**故意不要求 canvas 与离屏逐字节相等**——两者目标格式不同
 *  （canvas 走 sRGB、离屏走 Rgba8Unorm 线性），逐字节相等本来就不该成立。
 *  逐字节相等留在"同一格式、两个运行时"那条上（见 T4.5 的双端比对）。 */
function validate(report) {
  const problems = [];
  if (report.ok !== true) problems.push('页面没报 ok=true：' + JSON.stringify(report.error));
  if (report.projectOk !== true) problems.push('样本工程没通过校验：' + JSON.stringify(report.issues));
  const attach = report.attach === undefined ? {} : report.attach;
  if (attach.already === true) problems.push('attach 报 already=true，宿主没被真正建起来');
  if (typeof attach.size !== 'string' || !attach.size.includes('x')) problems.push('attach 没返回尺寸：' + JSON.stringify(attach));
  if (report.drawError !== null && report.drawError !== undefined) problems.push('draw 抛错：' + report.drawError);
  if (!Array.isArray(report.frames) || report.frames.length < 2) problems.push('至少要验两帧');
  else {
    for (const row of report.frames) {
      if (!Array.isArray(row.sources)) problems.push('第 ' + row.frame + ' 帧没返回 sources');
      if (row.sources.length === 0) problems.push('第 ' + row.frame + ' 帧一个源都没有——说明工程没驱动到');
      if (!(row.pngLength > 0)) problems.push('第 ' + row.frame + ' 帧的 canvas 读回来是空的');
    }
    // 两帧选出的 (source, source_frame) 必须不同：证明画面确实是按帧号算出来的
    const key = (row) => row.sources.map((s) => s.source + '@' + s.source_frame).sort().join(',');
    if (key(report.frames[0]) === key(report.frames[1])) {
      problems.push('两帧选出的源完全相同，时间线没有真正驱动画面');
    }
  }
  return problems;
}
