#!/usr/bin/env node
// 后端出片的吞吐测量：**真实素材、多源顺序解码、端到端**。
//
// # 它测的和别的 measure-* 有什么不同
//
//   measure-render.mjs      合成源，只测渲染
//   measure-decode.mjs      只测解码，且是合成片
//   measure-export.mjs（这个） **完整出片**：逐源 ffmpeg 顺序解码 -> 上传 -> 求值 -> 合成
//                            -> 读回 -> 编码成 mp4
//
// 也就是说这里的数才是"用户按下导出之后要等多久"的那一类。
//
// # 量纲限制（写进输出，别当成容量数）
//
//   1. 样本工程的四个 asset **指向同一份文件**。四路解码器读同一个文件，
//      页缓存会帮忙 —— 换成四份不同素材会更慢。所以这是**下界**。
//   2. 读回是同步等待（每帧等待 copy_texture_to_buffer 完成），不是流水线吞吐。
//      真实出片可以让读回与编码重叠，边际成本会更低。
//   3. 单机单卡（本机 Windows / 一块 NVIDIA）。别的机器别照搬。
//   4. 输出尺寸影响很大：读回与编码都跟着尺寸走。
//
// 用法：
//   node scripts/measure-export.mjs
//   node scripts/measure-export.mjs --runs 5

import { spawnSync } from 'node:child_process';
import { existsSync, mkdirSync, rmSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const OUT_DIR = join(REPO_ROOT, 'target', 'p6', 'measure');
const PROJECT = 'fixtures/sample-project.doc.json';

function valueOf(name, fallback) {
  const index = process.argv.indexOf(name);
  return index >= 0 ? process.argv[index + 1] : fallback;
}

function findCli() {
  for (const candidate of ['target/debug/dhampir.exe', 'target/debug/dhampir', 'target/release/dhampir.exe']) {
    const full = join(REPO_ROOT, candidate);
    if (existsSync(full)) return full;
  }
  return null;
}

/** 跑一次出片，返回 done 那一行。**解析 NDJSON 而不是看人话输出** —— 人话会改。 */
export function runOnce(cli, args) {
  const result = spawnSync(cli, args, { cwd: REPO_ROOT, encoding: 'utf8', maxBuffer: 64 * 1024 * 1024 });
  if (result.status !== 0) {
    return { ok: false, error: (result.stderr || '').trim() || ('退出码 ' + result.status) };
  }
  const lines = String(result.stdout).split('\n');
  let done = null;
  let frames = 0;
  for (const line of lines) {
    const trimmed = line.trim();
    if (trimmed.length === 0) continue;
    let event = null;
    try { event = JSON.parse(trimmed); } catch (error) { continue; }
    if (event.event === 'done') done = event;
    else if (event.event === 'progress') frames += 1;
  }
  if (done === null) return { ok: false, error: '输出里没有 done 那一行' };
  return { ok: true, done: done, progressLines: frames };
}

/** 一组测量的汇总。**中位数与最坏值都给** —— 只给均值会把抖动藏起来。 */
export function summarise(samples) {
  const sorted = samples.slice().sort((a, b) => a - b);
  const middle = sorted.length === 0 ? null : sorted[Math.floor(sorted.length / 2)];
  return {
    count: sorted.length,
    min: sorted.length === 0 ? null : sorted[0],
    median: middle,
    max: sorted.length === 0 ? null : sorted[sorted.length - 1],
  };
}

function main() {
  if (process.argv.includes('--self-test')) {
    let passed = 0;
    const expect = (name, condition) => {
      if (!condition) throw new Error('自检失败：' + name);
      passed += 1;
    };
    expect('汇总取中位数', summarise([3, 1, 2]).median === 2);
    expect('汇总给最坏值', summarise([3, 1, 2]).max === 3);
    expect('空样本不编数', summarise([]).median === null);
    console.log('✓ 出片测量自检通过（' + passed + ' 条断言）');
    return;
  }

  const cli = findCli();
  if (cli === null) {
    console.error('找不到 dhampir 可执行文件。先跑：cargo build -p dhampir-worker --bin dhampir');
    process.exitCode = 2;
    return;
  }
  const runs = Number(valueOf('--runs', '3'));
  mkdirSync(OUT_DIR, { recursive: true });

  const cases = [
    { label: '640x360  / 90 帧', width: 640, height: 360, from: 0, to: 89 },
    { label: '1920x1080 / 30 帧', width: 1920, height: 1080, from: 0, to: 29 },
  ];

  console.log('后端出片吞吐（真实素材 target/s3/proxy1080p.mp4，四路顺序解码器）');
  console.log('');
  console.log('| 用例 | 帧数 | 每帧 ms（中位） | 最小 | 最大 | 端到端 ms |');
  console.log('|---|---|---|---|---|---|');
  for (const item of cases) {
    const perFrame = [];
    const total = [];
    let frames = 0;
    for (let run = 0; run < runs; run += 1) {
      const output = join(OUT_DIR, 'run-' + item.width + '-' + run + '.mp4');
      const result = runOnce(cli, [
        'render', '--project', PROJECT,
        '--from', String(item.from), '--to', String(item.to),
        '--width', String(item.width), '--height', String(item.height),
        '--out', output,
      ]);
      if (!result.ok) {
        console.error('第 ' + run + ' 次失败：' + result.error);
        process.exitCode = 1;
        return;
      }
      frames = result.done.frames;
      perFrame.push(result.done.elapsed_ms / result.done.frames);
      total.push(result.done.elapsed_ms);
    }
    const stats = summarise(perFrame);
    const totalStats = summarise(total);
    console.log('| ' + item.label + ' | ' + frames + ' | ' + stats.median.toFixed(2) + ' | '
      + stats.min.toFixed(2) + ' | ' + stats.max.toFixed(2) + ' | ' + totalStats.median + ' |');
  }
  console.log('');
  console.log('量纲限制（别把这些数当容量）：');
  console.log('  * 四个 asset 指向**同一份文件** —— 四路解码器读同一个文件，页缓存会帮忙。');
  console.log('    换成四份不同素材会更慢，所以这里是**下界**。');
  console.log('  * 每帧同步等待读回，不是流水线吞吐。真实出片可让读回与编码重叠。');
  console.log('  * 单机单卡；换机器别照搬。');
  console.log('  * 尺寸影响很大：读回与编码都跟着尺寸走。');
  rmSync(OUT_DIR, { recursive: true, force: true });
}

main();
