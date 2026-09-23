#!/usr/bin/env node
// T2.3b 的测量脚本：**一条字幕的栅格化成本**。
//
// # 它测的两段，量纲完全不同
//
//   冷（第一次见到这一行）  写临时文本文件 + 起一次 ffmpeg + 读回字节 + 染色
//   热（命中缓存）         一次哈希查找加一次 Rc 克隆
//
// 于是「一条字幕在 N 帧上要花多少栅格化时间」= 1 冷 + (N-1) 热。
// **别拿冷路径乘帧数** —— 那是"没有缓存"的世界，而缓存正是这个数要证的事。
//
// # 量纲限制（写进输出，别当结论用）
//
//   1. 冷路径里**大部分是 ffmpeg 的进程启动**：那是外部 release 程序，
//      而本仓的 Rust 部分是 debug 构建（cargo run 的默认档）。
//   2. 位图宽度取**整条目标宽**，所以 1080p 下一张约 0.8 MB ——
//      冷路径读的就是这个字节量，**不是**这一行的实际墨迹量。
//   3. 取样行**不经共享布局的换行**（整行直接喂给 drawtext）：小尺寸下长混排行会碰边。
//      那是"模型字宽 vs 真字宽"的差，**不是**产品路径的结论 —— 产品路径先换行再逐行画。
//   4. 单机单字体（--font-file）。换字体、换机器别照搬。
//
// 用法：
//   node scripts/measure-raster.mjs
//   node scripts/measure-raster.mjs --cold 12 --warm 50000 --targets 1920x1080,640x360
//   node scripts/measure-raster.mjs --font-file C:/Windows/Fonts/msyh.ttc

import { spawnSync } from 'node:child_process';
import { existsSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

export const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');

/** 找字体**只是为了让这条命令能开箱即跑**。产品路径上字体由 --font-file 给 —— 本仓不猜系统字体。 */
export const FONT_CANDIDATES = [
  'C:/Windows/Fonts/msyh.ttc',
  'C:/Windows/Fonts/simhei.ttf',
  'C:/Windows/Fonts/simsun.ttc',
  'C:/Windows/Fonts/NotoSansSC-VF.ttf',
  '/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc',
  '/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf',
  '/System/Library/Fonts/PingFang.ttc',
];

function valueOf(name, fallback) {
  const index = process.argv.indexOf(name);
  return index >= 0 ? process.argv[index + 1] : fallback;
}

/** 第一个"存在"的候选。把 exists 当参数传进来，自检才能不碰磁盘。 */
export function pickFont(candidates, exists) {
  for (const candidate of candidates) {
    if (exists(candidate)) return candidate;
  }
  return null;
}

/** NDJSON：能被解析成对象的行才算事件，其余一律忽略（人话与进度行会混进来）。 */
export function parseNdjson(text) {
  const events = [];
  for (const line of String(text).split('\n')) {
    const trimmed = line.trim();
    if (trimmed.length === 0 || trimmed[0] !== '{') continue;
    try {
      const value = JSON.parse(trimmed);
      if (value && typeof value === 'object' && typeof value.event === 'string') events.push(value);
    } catch (error) {
      // 半截 JSON 就是噪声，不是事件。
    }
  }
  return events;
}

export function byEvent(events, name) {
  return events.filter((event) => event.event === name);
}

function mb(bytes) {
  return (bytes / (1024 * 1024)).toFixed(2);
}

function summaryBlock(events) {
  const configs = byEvent(events, 'config');
  const colds = byEvent(events, 'cold_summary');
  const warms = byEvent(events, 'warm');
  const cues = byEvent(events, 'cue');
  console.log('| 目标 | 字号 px | 位图 | 每张 MB | 冷中位 ms | 冷最小 | 冷最大 | 冷碰边 | 热 ns/op | 1 冷 + 89 热 ms |');
  console.log('|---|---|---|---|---|---|---|---|---|---|');
  for (const config of configs) {
    const target = config.target;
    const cold = colds.find((item) => item.target === target) ?? {};
    const warm = warms.find((item) => item.target === target) ?? {};
    const cue = cues.find((item) => item.target === target) ?? {};
    console.log('| ' + [
      target,
      config.font_px,
      config.bitmap,
      mb(config.bytes),
      cold.median_ms ?? '—',
      cold.min_ms ?? '—',
      cold.max_ms ?? '—',
      cold.ink_touches_edge ?? '—',
      warm.ns_per_op ?? '—',
      cue.one_cold_plus_warm_ms ?? '—',
    ].join(' | ') + ' |');
  }
  const resident = warms.reduce((max, warm) => Math.max(max, warm.resident_bytes ?? 0), 0);
  if (resident > 0) {
    console.log('');
    console.log('缓存把内存钉在（本次取样最大的那一份驻留量）：' + mb(resident) + ' MB');
  }
  const done = byEvent(events, 'done')[0];
  if (done) {
    console.log('样本：' + done.cold_count + ' 次冷栅格化，全部走真 ffmpeg。');
  }
}

function main() {
  if (process.argv.includes('--self-test')) {
    let passed = 0;
    const expect = (name, condition) => {
      if (!condition) throw new Error('自检失败：' + name);
      passed += 1;
    };
    expect('选中第一个存在的字体', pickFont(['a', 'b'], (p) => p === 'b') === 'b');
    expect('一个都不存在就给 null', pickFont(['a'], () => false) === null);
    expect(
      'NDJSON 只认事件行',
      parseNdjson('{"event":"cold","ms":1}\n人话一行\n{"event":"done"}\n{半截').length === 2,
    );
    expect('按事件名筛', byEvent(parseNdjson('{"event":"cold"}\n{"event":"warm"}'), 'cold').length === 1);
    console.log('✓ 栅格化测量自检通过（' + passed + ' 条断言）');
    return;
  }

  const font = valueOf('--font-file', null) ?? pickFont(FONT_CANDIDATES, existsSync);
  if (font === null) {
    console.error('没有可用字体。用 --font-file 指定一个，例如：');
    console.error('  node scripts/measure-raster.mjs --font-file C:/Windows/Fonts/msyh.ttc');
    process.exitCode = 2;
    return;
  }
  const cold = valueOf('--cold', '8');
  const warm = valueOf('--warm', '20000');
  const frames = valueOf('--frames', '90');
  const targets = String(valueOf('--targets', '1920x1080,640x360'))
    .split(',')
    .map((item) => item.trim())
    .filter((item) => item.length > 0);
  if (targets.length === 0) {
    console.error('--targets 是空的 —— 没有目标尺寸就没有要测的东西。');
    process.exitCode = 2;
    return;
  }

  console.log('单条字幕的栅格化成本（ffmpeg drawtext → 直排 RGBA8 位图，带缓存）');
  console.log('字体：' + font);
  console.log('');

  const args = [
    'run', '--quiet', '-p', 'dhampir-worker', '--example', 'raster_cost', '--',
    '--font-file', font, '--cold', cold, '--warm', warm, '--frames', frames,
  ];
  for (const target of targets) args.push('--target', target);

  const result = spawnSync('cargo', args, {
    cwd: REPO_ROOT,
    encoding: 'utf8',
    env: process.env,
    maxBuffer: 64 * 1024 * 1024,
  });
  if (result.status !== 0) {
    console.error('夹具失败（退出码 ' + result.status + '）：');
    console.error((result.stderr || '').trim());
    process.exitCode = 1;
    return;
  }

  const events = parseNdjson(result.stdout);
  if (byEvent(events, 'cold').length === 0) {
    console.error('输出里**一条冷样本都没有** —— 这不是"很快"，是**测不出来**。');
    console.error((result.stdout || '').trim().slice(0, 2000));
    process.exitCode = 1;
    return;
  }
  summaryBlock(events);
  console.log('');
  console.log('量纲限制（别把这些数当容量）：');
  console.log('  * 冷路径的耗时里大部分是 **ffmpeg 的进程启动**；本仓的 Rust 部分是 debug 构建。');
  console.log('  * 位图宽取整条目标宽，所以 1080p 一张约 0.8 MB —— 冷路径读的是这个字节量。');
  console.log('  * 取样行不经共享布局的换行：小尺寸下长混排行碰边是"模型字宽 vs 真字宽"的差。');
  console.log('  * 单机单字体。换字体、换机器别照搬。');
}

main();
