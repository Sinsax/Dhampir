#!/usr/bin/env node
// P5 第二项测量：**出片时的 GPU 并发上限**。
//
// # 为什么需要它
//
// 出片是 GPU 密集的，而**一块 GPU 只有一个 device**。
// 并发跑多个出片任务会互相拖慢，而不是线性加速 ——
// 所以"能同时跑几个"不能靠猜，它直接决定出片队列的并发度。
//
// # 方法
//
// 分别测：
//   * **单跑**：一个渲染进程，N 帧；
//   * **并发**：K 个渲染进程**同时**启动，各自也是 N 帧；看整批的墙钟时间。
//
// 判据是比值：并发耗时 / 单跑耗时。
//   ~1.0  -> 几乎完全并行（GPU 还有余量）；
//   ~K    -> 完全串行（加并发没有收益，只是排队）。
//
// # 它不能说明什么
//
// worker 用的是 core 的**合成源**，不解码视频。
// 所以它量的是**渲染**这一段的并发行为，不含解码开销。
//
// 用法：
//   node scripts/measure-concurrency.mjs
//   node scripts/measure-concurrency.mjs --frames 60 --jobs 2

import { spawn } from 'node:child_process';
import { mkdirSync, rmSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

export const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');

/** 起一个渲染进程，返回一个在它退出时兑现的 promise（兑现墙钟毫秒）。 */
export function startRender(frames, outDir) {
  rmSync(outDir, { recursive: true, force: true });
  mkdirSync(outDir, { recursive: true });
  const list = Array.from({ length: frames }, (_, index) => String(index));
  const started = performance.now();
  return new Promise((settle) => {
    const child = spawn('cargo', [
      'run', '--quiet', '--example', 'render_project', '--',
      'fixtures/sample-project.json', outDir, ...list,
    ], { cwd: REPO_ROOT, stdio: ['ignore', 'ignore', 'ignore'] });
    child.on('exit', (code) => settle({ ms: performance.now() - started, code: code }));
  });
}

/** 判定比值该怎么说。抽成纯函数，便于自检。 */
export function describeRatio(ratio, jobs) {
  if (!(ratio > 0)) return '测不出来：比值非正数 —— 这次没量到有效数据';
  if (ratio < 1.2) return '接近完全并行：' + jobs + ' 个任务同时跑，整批耗时与单跑相当';
  if (ratio < jobs * 0.6) return '部分并行：加并发有收益，但不是线性的';
  return '基本串行：加并发只是排队，排队长度不带来吞吐';
}

async function main() {
  const argv = process.argv.slice(2);
  const framesIndex = argv.indexOf('--frames');
  const frames = framesIndex >= 0 ? Number(argv[framesIndex + 1]) : 60;
  const jobsIndex = argv.indexOf('--jobs');
  const jobs = jobsIndex >= 0 ? Number(argv[jobsIndex + 1]) : 2;

  const build = spawn('cargo', ['build', '--quiet', '--example', 'render_project'], { cwd: REPO_ROOT, stdio: 'ignore' });
  await new Promise((settle) => build.on('exit', settle));

  console.log('单跑（' + frames + ' 帧）…');
  const solo = await startRender(frames, join(REPO_ROOT, 'target', 'measure-solo'));
  if (solo.code !== 0) {
    console.error('单跑失败（退出码 ' + solo.code + '）');
    process.exitCode = 1;
    return;
  }
  console.log('  ' + solo.ms.toFixed(0) + ' ms');

  console.log('并发 ' + jobs + ' 个（每个同样 ' + frames + ' 帧）…');
  const batchStarted = performance.now();
  const batch = await Promise.all(
    Array.from({ length: jobs }, (_, index) =>
      startRender(frames, join(REPO_ROOT, 'target', 'measure-jobs', 'job-' + index)))
  );
  const batchMs = performance.now() - batchStarted;
  if (batch.some((run) => run.code !== 0)) {
    console.error('并发里有任务失败');
    process.exitCode = 1;
    return;
  }
  console.log('  整批 ' + batchMs.toFixed(0) + ' ms');
  for (let index = 0; index < batch.length; index += 1) {
    console.log('    job-' + index + '：' + batch[index].ms.toFixed(0) + ' ms');
  }

  const ratio = batchMs / solo.ms;
  console.log('');
  console.log('比值（并发整批 / 单跑）= ' + ratio.toFixed(2) + '（' + jobs + ' 个并发）');
  console.log(describeRatio(ratio, jobs));
  console.log('');
  console.log('**它量的是渲染这一段**：worker 用合成源、不解码视频，所以不含解码开销。');
}

main();
