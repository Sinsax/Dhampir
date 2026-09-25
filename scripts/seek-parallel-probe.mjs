#!/usr/bin/env node
// 一次性诊断：多路 seek 并行 vs 串行，在真实 Chrome 里对照。
import { spawn } from 'node:child_process';
import { createServer } from 'node:http';
import { existsSync, readFileSync, statSync } from 'node:fs';
import { extname, join, resolve, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const WEB_DIR = join(REPO_ROOT, 'web');
const MIME = { '.html': 'text/html; charset=utf-8', '.js': 'text/javascript; charset=utf-8',
  '.json': 'application/json; charset=utf-8', '.mp4': 'video/mp4', '.wasm': 'application/wasm' };
const ASSETS = { a: 'proxy1080p.mp4', b: 'proxy720p.mp4', d: 'proxy4k.mp4' };
let verdicts = [];
const server = createServer((req, res) => {
  const path = new URL(req.url, 'http://127.0.0.1').pathname;
  if (req.method === 'POST' && path === '/verdict') {
    let body = '';
    req.on('data', (c) => { body += c; });
    req.on('end', () => { try { verdicts.push(JSON.parse(body)); } catch (e) {} res.writeHead(200).end('{}'); });
    return;
  }
  const m = path.match(/^\/assets\/([abd])\.mp4\/media$/);
  if (m) {
    const file = join(REPO_ROOT, 'target', 's3', ASSETS[m[1]]);
    res.writeHead(200, { 'content-type': 'video/mp4', 'content-length': statSync(file).size,
      'accept-ranges': 'bytes', 'access-control-allow-origin': '*' });
    res.end(readFileSync(file));
    return;
  }
  const file = path === '/' ? join(WEB_DIR, 'seek-probe.html') : join(WEB_DIR, path.slice(1));
  if (!existsSync(file) || !statSync(file).isFile()) { res.writeHead(404).end('nf'); return; }
  res.writeHead(200, { 'content-type': MIME[extname(file)] || 'application/octet-stream', 'cache-control': 'no-store' });
  res.end(readFileSync(file));
});
await new Promise((r) => server.listen(0, '127.0.0.1', r));
const port = server.address().port;
console.log('probe: http://127.0.0.1:' + port + '/');
const chrome = spawn('C:/Program Files/Google/Chrome/Application/chrome.exe',
  ['--headless=new', '--no-first-run', '--user-data-dir=' + join(REPO_ROOT, 'target', 'seek-probe-profile'),
   '--remote-debugging-port=0', 'http://127.0.0.1:' + port + '/'], { stdio: ['ignore', 'pipe', 'pipe'] });
const deadline = Date.now() + 120000;
while (Date.now() < deadline && verdicts.length === 0) await new Promise((r) => setTimeout(r, 500));
console.log(JSON.stringify(verdicts, null, 1));
chrome.kill(); server.close();
if (verdicts.length === 0) process.exitCode = 1;
