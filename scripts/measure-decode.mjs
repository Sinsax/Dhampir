#!/usr/bin/env node
// P5 补测：**解码分量**。
//
// # 为什么必须补它
//
// 前三项测量（渲染吞吐、GPU 并发、素材 IO）**都不含解码** ——
// worker 用的是 core 的合成源，不吃视频。于是「1 分钟 1080p 素材要多久」
// 那个问题一直答不了：渲染分量有，解码分量没有。
//
// # 为什么用 FFmpeg 而不用自己写
//
// 这是既定取舍（plan 里写过）：**用 FFmpeg CLI，不引 Rust 绑定**。
// 后端出片要走「顺序解码管道」，而不是像前端那样逐帧 seek ——
// 逐帧 seek 每次都要回到关键帧重解，慢一个量级。
// 这里量的正是**顺序解码**的代价。
//
// # 它量的与不量的
//
// 量：FFmpeg 顺序解码这份文件的吞吐（解码 + 解封装，输出丢进 null）。
// 不量：渲染（那有单独一项）、编码、真实网络。
//
// 用法：
//   node scripts/measure-decode.mjs
//   node scripts/measure-decode.mjs --media target/s3/proxy1080p.mp4

import { spawnSync } from 'node:child_process';
import { existsSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

export const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');

function probe(media) {
  const out = spawnSync('ffprobe', [
    '-v', 'error', '-select_streams', 'v:0',
    '-show_entries', 'stream=nb_frames,avg_frame_rate,width,height',
    '-of', 'json', media,
  ], { encoding: 'utf8' });
  const stream = JSON.parse(out.stdout).streams[0];
  const [num, den] = String(stream.avg_frame_rate).split('/').map(Number);
  return {
    frames: Number(stream.nb_frames) || 0,
    fps: den ? num / den : 0,
    width: stream.width,
    height: stream.height,
  };
}

function main() {
  const argv = process.argv.slice(2);
  const mediaIndex = argv.indexOf('--media');
  const relative = mediaIndex >= 0 ? argv[mediaIndex + 1] : 'target/s3/proxy1080p.mp4';
  const media = join(REPO_ROOT, relative);
  if (!existsSync(media)) {
    console.error('找不到素材：' + relative);
    process.exitCode = 1;
    return;
  }

  const info = probe(media);
  console.log('素材：' + relative + '  ' + info.width + 'x' + info.height
    + '  ' + info.frames + ' 帧 @' + info.fps + 'fps');

  // **-f null -**：解码全部帧但不写输出，量的就是解码本身。
  const started = performance.now();
  const run = spawnSync('ffmpeg', ['-v', 'error', '-i', media, '-f', 'null', '-'], {
    encoding: 'utf8', cwd: REPO_ROOT,
  });
  const elapsedMs = performance.now() - started;

  if (run.status !== 0) {
    console.error('ffmpeg 解码失败：' + (run.stderr || '').slice(-800));
    process.exitCode = 1;
    return;
  }
  if (!(info.frames > 0)) {
    console.log('');
    console.log('测不出来：ffprobe 没给出帧数，无法算每帧成本。');
    process.exitCode = 1;
    return;
  }

  const perFrameMs = elapsedMs / info.frames;
  console.log('');
  console.log('顺序解码全部 ' + info.frames + ' 帧：' + elapsedMs.toFixed(0) + ' ms');
  console.log('  每帧 ' + perFrameMs.toFixed(2) + ' ms  ->  ' + (1000 / perFrameMs).toFixed(0) + ' fps');
  console.log('  （含进程启动，所以帧数越少这个数越偏高；这份文件帧数够多，影响可忽略）');

  // **把两段合起来**：这才是「1 分钟素材要多久」第一次有依据。
  // 渲染那一段来自 measure-render.mjs 的实测中位数 13.35 ms/帧。
  const RENDER_MS_PER_FRAME = 13.35;
  const oneMinute = 1800;
  const renderSeconds = (RENDER_MS_PER_FRAME * oneMinute) / 1000;
  const decodeSeconds = (perFrameMs * oneMinute) / 1000;
  console.log('');
  console.log('外推到 1800 帧（1080p 素材 60 秒 @30fps）：');
  console.log('  解码：约 ' + decodeSeconds.toFixed(1) + ' 秒');
  console.log('  渲染：约 ' + renderSeconds.toFixed(1) + ' 秒（来自 measure-render.mjs 的实测）');
  console.log('  合计：约 ' + (decodeSeconds + renderSeconds).toFixed(1) + ' 秒（**不含编码**）');

  console.log('');
  console.log('⚠️ 边界：');
  console.log('  * 这是**解码 + 渲染**两段；**编码没算**，它通常是同一个量级；');
  console.log('  * 走的是**本机文件**，不含真实网络与对象存储的取回时间；');
  console.log('  * 素材是 8 秒的合成片（testsrc2），**码率与真实素材不同** ——');
  console.log('    真实素材解码更贵，所以这个数是**偏乐观**的下界。');
}

main();
