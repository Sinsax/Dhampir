#!/usr/bin/env node
// P5 第三项测量：**素材取用的 IO 与延迟**。
//
// # 为什么测这两个数
//
// 远端模式的预览体验由两件事决定：
//   1. **吞吐**：连续播放时要取多少个 GOP 每秒；
//   2. **单个分片的请求延迟**：拖动一次要等多久。
// 前者决定带宽够不够，后者决定拖动顺不顺手 —— 而后者此前只有"猜"。
//
// # 它不能说明什么
//
// 走的是 **loopback**（127.0.0.1），所以：
//   * 它是**上限**，不是真实网络的数字；
//   * 它**不含**真实网络的 RTT、丢包、带宽限制。
// 真实网络下把这两项加上去才成立。
//
// 用法：
//   node scripts/measure-asset-io.mjs
//   node scripts/measure-asset-io.mjs --asset a.mp4 --chunk-bytes 600000

import { spawn } from 'node:child_process';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

export const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');

/** 一次完整下载，返回字节数与毫秒数。 */
export async function downloadOnce(url) {
  const started = performance.now();
  const response = await fetch(url);
  const bytes = (await response.arrayBuffer()).byteLength;
  return { ms: performance.now() - started, bytes };
}

/** 一次 Range 请求（模拟"取一个 GOP 分片"）。 */
export async function fetchRange(url, length) {
  const started = performance.now();
  const response = await fetch(url, { headers: { Range: 'bytes=0-' + (length - 1) } });
  const bytes = (await response.arrayBuffer()).byteLength;
  return { ms: performance.now() - started, bytes, status: response.status };
}

function main() {
  return run();
}

async function run() {
  const argv = process.argv.slice(2);
  const assetIndex = argv.indexOf('--asset');
  const asset = assetIndex >= 0 ? argv[assetIndex + 1] : 'a.mp4';
  const chunkIndex = argv.indexOf('--chunk-bytes');
  const chunkBytes = chunkIndex >= 0 ? Number(argv[chunkIndex + 1]) : 600000;

  const port = 8798;
  const child = spawn(process.execPath, ['scripts/dhampir-local.mjs', '--port', String(port)], {
    cwd: REPO_ROOT, stdio: ['ignore', 'ignore', 'inherit'],
  });
  const base = 'http://127.0.0.1:' + port + '/assets/' + asset + '/media';

  const settle = async () => {
    for (let attempt = 0; attempt < 50; attempt += 1) {
      try { const response = await fetch('http://127.0.0.1:' + port + '/health'); if (response.ok) return true; } catch (error) { /* 还没起来 */ }
      await new Promise((done) => setTimeout(done, 100));
    }
    return false;
  };

  try {
    if (!(await settle())) {
      console.error('本机后端没起来');
      process.exitCode = 1;
      return;
    }

    // 预热一次：第一次请求要建连接，不该算进测量。
    await downloadOnce(base);

    const fullRuns = [];
    for (let index = 0; index < 10; index += 1) fullRuns.push(await downloadOnce(base));
    const bytes = fullRuns[0].bytes;
    const meanMs = fullRuns.reduce((sum, run) => sum + run.ms, 0) / fullRuns.length;
    const mbPerSecond = (bytes / (1024 * 1024)) / (meanMs / 1000);
    console.log('整段下载（' + (bytes / (1024 * 1024)).toFixed(2) + ' MB）10 次：');
    console.log('  平均 ' + meanMs.toFixed(1) + ' ms/次  ->  ' + mbPerSecond.toFixed(1) + ' MB/s');
    console.log('  单次最快 ' + Math.min(...fullRuns.map((run) => run.ms)).toFixed(1)
      + ' ms，最慢 ' + Math.max(...fullRuns.map((run) => run.ms)).toFixed(1) + ' ms');

    const rangeRuns = [];
    for (let index = 0; index < 20; index += 1) rangeRuns.push(await fetchRange(base, chunkBytes));
    const sorted = rangeRuns.map((run) => run.ms).sort((a, b) => a - b);
    const p50 = sorted[Math.floor(sorted.length * 0.5)];
    const p95 = sorted[Math.min(sorted.length - 1, Math.floor(sorted.length * 0.95))];
    console.log('');
    console.log('单个分片 Range 请求（' + (chunkBytes / 1024).toFixed(0) + ' KB）20 次：');
    console.log('  p50 ' + p50.toFixed(1) + ' ms，p95 ' + p95.toFixed(1) + ' ms，全部状态码 '
      + (rangeRuns.every((run) => run.status === 206) ? '206' : '**有非 206**'));
    console.log('  分片大小按 S3.2 的 -g 60 估算（1080p proxy 约 600 KB/GOP）');

    console.log('');
    console.log('⚠️ 这是 **loopback**，所以：');
    console.log('   * 它是**上限**，不是真实网络的数字；');
    console.log('   * 不含真实网络的 RTT、丢包与带宽限制。');
    console.log('   远端模式下要把那几项加上去，这两个数才成立。');
  } finally {
    child.kill();
  }
}

main().catch((error) => { console.error('测量失败：' + error.message); process.exitCode = 1; });
