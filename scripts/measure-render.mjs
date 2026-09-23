#!/usr/bin/env node
// P5 的第一项测量：**渲染吞吐**。
//
// # 方法上的一处讲究
//
// render_project 每次运行都含 **进程启动 + wgpu 初始化**，小样本会被它主导。
// 所以这里测**两个样本量**，用差值算**边际每帧成本**，而不是拿总时间直接除帧数 ——
// 后者会把固定开销摊进每帧里，得出一个"每帧很贵"的假结论。
//
// # 这个数能说明什么、不能说明什么
//
// 能：**渲染**这一段的边际成本。
// 不能：**解码**成本 —— worker 用的是 core 的合成源，不解码任何视频。
//       所以它**不是**"1 分钟 1080p 素材要多久"的答案，只是那个答案的渲染分量。
//       真正的答案要等后端接上顺序解码管道（P5 未做项）。
//
// 用法：
//   node scripts/measure-render.mjs
//   node scripts/measure-render.mjs --frames 4,16 --size 320x180

import { spawnSync } from 'node:child_process';
import { mkdirSync, rmSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

export const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');

/** 跑一次渲染，返回毫秒。**用 performance.now 包住整个子进程**。 */
export function timeRender(frameCount, outDir) {
  rmSync(outDir, { recursive: true, force: true });
  mkdirSync(outDir, { recursive: true });
  const frames = Array.from({ length: frameCount }, (_, index) => String(index));
  const started = performance.now();
  const result = spawnSync('cargo', [
    'run', '--quiet', '--example', 'render_project', '--',
    'fixtures/sample-project.json', outDir, ...frames,
  ], { cwd: REPO_ROOT, encoding: 'utf8', env: process.env });
  const elapsed = performance.now() - started;
  return { ms: elapsed, status: result.status, stderr: result.stderr || '' };
}

/** 由两个样本算**边际**每帧成本（毫秒）。
 *
 * 固定开销 = (小样本时间 * 大帧数 - 大样本时间 * 小帧数) / (大帧数 - 小帧数)，
 * 这里只关心边际斜率：(大 - 小) / (大帧数 - 小帧数)。 */
export function marginalMsPerFrame(small, large) {
  const frameDelta = large.frames - small.frames;
  if (frameDelta <= 0) return null;
  return (large.ms - small.ms) / frameDelta;
}

function main() {
  const argv = process.argv.slice(2);
  const framesIndex = argv.indexOf('--frames');
  const pair = framesIndex >= 0 ? argv[framesIndex + 1].split(',').map(Number) : [30, 150];
  if (pair.length !== 2 || !(pair[1] > pair[0])) {
    console.error('--frames 要给两个递增的数，例如 --frames 4,16');
    process.exitCode = 1;
    return;
  }

  console.log('先构建一次（构建时间不该算进测量里）…');
  const build = spawnSync('cargo', ['build', '--quiet', '--example', 'render_project'], {
    cwd: REPO_ROOT, encoding: 'utf8', env: process.env,
  });
  if (build.status !== 0) {
    console.error('构建失败');
    console.error((build.stderr || '').slice(-1500));
    process.exitCode = 1;
    return;
  }

  const samples = [];
  for (const frames of pair) {
    const run = timeRender(frames, join(REPO_ROOT, 'target', 'measure-frames'));
    if (run.status !== 0) {
      console.error('渲染 ' + frames + ' 帧失败');
      console.error(run.stderr.slice(-1500));
      process.exitCode = 1;
      return;
    }
    samples.push({ frames, ms: run.ms });
    console.log('  ' + String(frames).padStart(3) + ' 帧：' + run.ms.toFixed(0) + ' ms（含进程启动与 wgpu 初始化）');
  }

  const perFrame = marginalMsPerFrame(samples[0], samples[1]);
  // **先判这次测量到底有没有分辨出信号。**
  //
  // 实测同一对样本（4/16 帧）两次跑分别得到 -8.62 与 +14.89 ms/帧 ——
  // **符号都反了**。这说明差异被「进程启动 + wgpu 初始化」的抖动完全淹没。
  // 负的每帧成本不是「渲染很快」，是「这个测法分辨不出来」。
  // **把垃圾当结果打印，比不测量更糟：它会被人当成数字用。**
  if (!(perFrame > 0)) {
    console.log("");
    console.log("测不出来：边际每帧 " + perFrame.toFixed(2) + " ms 为非正数，抖动大于信号。");
    console.log("加大帧数再试，或把计时挪进进程内部。");
    process.exitCode = 1;
    return;
  }
  console.log('');
  console.log('边际每帧（渲染，合成源、不解码）：' + perFrame.toFixed(2) + ' ms');
  console.log('  —— 由 ' + samples[0].frames + ' 帧与 ' + samples[1].frames + ' 帧两个样本的差值算出，');
  console.log('     这样固定开销不会摊进每帧里。');
  console.log('');
  const oneMinute = 1800;
  console.log('外推到 1800 帧（1080p 素材 60 秒 @30fps）的**渲染部分**：约 '
    + (perFrame * oneMinute / 1000).toFixed(1) + ' 秒');
  console.log('');
  console.log('**这个数不包含解码** —— 也正因如此，它离「1 分钟素材要多久」还很远。');
  console.log('要回答那个问题，需要后端接上顺序解码管道（目前不存在）。');
}

main();
