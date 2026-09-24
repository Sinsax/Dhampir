#!/usr/bin/env node
// 量「逐帧检查点」的代价：把一帧的裸 RGBA 写下去、再读回来，各要多少时间。
//
// 用途只有一个：给 A9（渲染任务的续渲）那个"做不做"的判断提供数字。
// 出片那条路是流式的（GPU 合成 -> 读回像素 -> 写进 ffmpeg 的 stdin），
// MP4 基本流没法从第二个进程接着写，所以"续渲"最多只能是
// **别把已经合成过的帧再合成一遍** —— 也就是把每帧的像素落盘当检查点。
// 这个脚本量的就是那个落盘的代价。
//
// 与渲染成本对照（`plan/measurements.md` 第五/七项，实测 14.56~56.96 ms/帧）：
// 落盘只值不值得，看这两个数在不在一个量级。
//
// 量纲与边界（**这个数不能当"出片会慢多少"用**）：
//   * 量的是**裸 RGBA** 的顺序写/读（`W*H*4` 字节），**不含** PNG 编码；
//     本仓要给人看的"每帧 PNG"另有一次编码开销，比这个贵得多。
//   * 数据用 `os.urandom` 造，是**不可压缩**的 —— 免得量到文件系统压缩，
//     收到一个偏乐观的数。
//   * 写在 `target/` 下（gitignore），跑完删掉；**不是**产物路径上的实测。
//   * 只反映**这台机器的这块盘**（本机实测约 1.1~2.8 GB/s）。
//   * **写这一项单次波动很大**，所以默认跑 3 次取中位数，并把最小/最大一起印出来。
//     只报一次的数是不可依赖的（同参数两次能差 2.4 倍）。
//
// 用法：
//   node scripts/measure-checkpoint-io.mjs
//   node scripts/measure-checkpoint-io.mjs --frames 120
//   node scripts/measure-checkpoint-io.mjs --size 3840x2160
//   node scripts/measure-checkpoint-io.mjs --repeat 5

import { mkdirSync, readFileSync, readdirSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { randomBytes } from 'node:crypto';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');

function parseArgs(argv) {
  const out = { frames: 60, width: 1920, height: 1080, repeat: 3 };
  for (let i = 0; i < argv.length; i += 1) {
    if (argv[i] === '--frames') {
      out.frames = Number(argv[i + 1]);
      i += 1;
    } else if (argv[i] === '--repeat') {
      out.repeat = Number(argv[i + 1]);
      i += 1;
    } else if (argv[i] === '--size') {
      const parts = String(argv[i + 1]).split('x');
      out.width = Number(parts[0]);
      out.height = Number(parts[1]);
      i += 1;
    }
  }
  return out;
}

/** 中位数。偶数个取中间两个的均值 —— 与 plan/measurements.md 其它地方口径一致。 */
function median(values) {
  const sorted = [...values].sort((a, b) => a - b);
  const middle = Math.floor(sorted.length / 2);
  return sorted.length % 2 === 1
    ? sorted[middle]
    : (sorted[middle - 1] + sorted[middle]) / 2;
}

function main() {
  const options = parseArgs(process.argv.slice(2));
  const bytesPerFrame = options.width * options.height * 4;
  if (!Number.isFinite(options.frames) || options.frames <= 0) {
    console.error('--frames 要给正整数');
    return 2;
  }
  if (!Number.isFinite(bytesPerFrame) || bytesPerFrame <= 0) {
    console.error('--size 要给 WxH，例如 1920x1080');
    return 2;
  }

  const dir = join(REPO_ROOT, 'target', 'checkpoint-io');
  rmSync(dir, { recursive: true, force: true });
  mkdirSync(dir, { recursive: true });

  // 一帧的字节只造一次：造数据的开销不进计时。
  const frame = randomBytes(bytesPerFrame);
  if (!Number.isFinite(options.repeat) || options.repeat <= 0) {
    console.error('--repeat 要给正整数');
    rmSync(dir, { recursive: true, force: true });
    return 2;
  }

  const totalBytes = bytesPerFrame * options.frames;
  const perFrameMs = (ns) => (ns / 1e6 / options.frames).toFixed(2);
  const mbPerSecond = (ns) => (totalBytes / 1e6 / (ns / 1e9)).toFixed(0);

  try {
    // **写这一项单次波动很大**（本机实测同样 60 帧能落在 2.98~7.23 ms/帧之间，
    // 即 1.1~2.8 GB/s），所以跑 `repeat` 次取中位数，并把最大最小一起印出来 ——
    // 只报一次的话，读的人会以为那一位有效数字是有意义的。
    const writes = [];
    const reads = [];
    const onDisks = [];
    for (let round = 0; round < options.repeat; round += 1) {
      rmSync(dir, { recursive: true, force: true });
      mkdirSync(dir, { recursive: true });

      const startedWrite = process.hrtime.bigint();
      for (let i = 0; i < options.frames; i += 1) {
        writeFileSync(join(dir, 'frame-' + String(i).padStart(4, '0') + '.bin'), frame);
      }
      const writeNs = Number(process.hrtime.bigint() - startedWrite);

      let readBytes = 0;
      const startedRead = process.hrtime.bigint();
      for (let i = 0; i < options.frames; i += 1) {
        const path = join(dir, 'frame-' + String(i).padStart(4, '0') + '.bin');
        readBytes += readFileSync(path).length;
      }
      const readNs = Number(process.hrtime.bigint() - startedRead);

      if (readBytes !== totalBytes) {
        console.error('读回来的字节数不对：' + readBytes + ' != ' + totalBytes + '，这次测量作废');
        return 2;
      }
      // **别用 `statSync(目录).size`**：在 Windows 上目录项的大小是 0，不是内容之和 ——
      // 早先这里就这么写，于是"落盘占用"永远印 `0.0 MB`，一个看着像实测的假数。
      // 逐文件 stat 才是真的量。
      onDisks.push(readdirSync(dir).reduce((sum, name) => sum + statSync(join(dir, name)).size, 0));
      writes.push(writeNs);
      reads.push(readNs);
    }

    const spread = (values, unit) => '（最小 ' + (Math.min(...values) / 1e6).toFixed(1)
      + ' / 最大 ' + (Math.max(...values) / 1e6).toFixed(1) + ' ms ' + unit + '）';

    console.log('逐帧检查点的代价（裸 RGBA，顺序写/读；中位数取自 ' + options.repeat + ' 次）');
    console.log('  一帧        ：' + options.width + 'x' + options.height + 'x4 = ' + bytesPerFrame + ' 字节');
    console.log('  帧数        ：' + options.frames + '（合计 ' + (totalBytes / 1e6).toFixed(1) + ' MB）');
    console.log('  写          ：' + (median(writes) / 1e6).toFixed(1) + ' ms 总 / ' + perFrameMs(median(writes)) +
      ' ms 每帧 / ' + mbPerSecond(median(writes)) + ' MB/s ' + spread(writes, '总'));
    console.log('  读          ：' + (median(reads) / 1e6).toFixed(1) + ' ms 总 / ' + perFrameMs(median(reads)) +
      ' ms 每帧 / ' + mbPerSecond(median(reads)) + ' MB/s ' + spread(reads, '总'));
    console.log('  落盘占用    ：' + (median(onDisks) / 1e6).toFixed(1) + ' MB'
      + '（应等于合计 ' + (totalBytes / 1e6).toFixed(1) + ' MB）');
    console.log('  对照        ：渲染实测 14.56~56.96 ms/帧（plan/measurements.md 第五/七项）');
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
  return 0;
}

process.exitCode = main();
