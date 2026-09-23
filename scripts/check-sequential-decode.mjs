#!/usr/bin/env node
// P5.1 的守卫：**后端出片不许逐帧 seek。**
//
// # 为什么这条值得有守卫
//
// plan 把它列为硬约束，理由是量化的：逐帧 seek 每次都要回到关键帧重解，
// 比顺序解码慢一个量级（后端实测顺序解码 2164 fps）。
//
// 但它此前**只写在注释里**。注释拦不住人 ——
// 而「加一个 -ss 让它支持任意起点」看起来是个无害的小改动，
// 直到出片时间翻十倍。
//
// # 判据
//
//   * 后端代码里的 ffmpeg 参数**不得出现 -ss / -seek_timestamp**；
//   * 解码路径必须显式声明它是顺序的（-f rawvideo 那条管道）。
//
// 用法：
//   node scripts/check-sequential-decode.mjs             检查
//   node scripts/check-sequential-decode.mjs --self-test 只跑守卫自检

import { existsSync, readFileSync, readdirSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

export const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');

/** 会被当成「跳到某一帧」的 ffmpeg 参数。 */
export const SEEK_FLAGS = ['"-ss"', "'-ss'", '"-seek_timestamp"', "'-seek_timestamp'"];

/** 后端里允许出现 ffmpeg 调用的目录。 */
export function backendFiles() {
  const roots = [
    join(REPO_ROOT, 'crates', 'dhampir-worker', 'examples'),
    join(REPO_ROOT, 'crates', 'dhampir-worker', 'src'),
    join(REPO_ROOT, 'crates', 'dhampir-worker', 'tests'),
  ];
  const files = [];
  for (const root of roots) {
    if (!existsSync(root)) continue;
    const walk = (dir) => {
      for (const entry of readdirSync(dir, { withFileTypes: true })) {
        const path = join(dir, entry.name);
        if (entry.isDirectory()) walk(path);
        else if (entry.name.endsWith('.rs')) files.push(path);
      }
    };
    walk(root);
  }
  return files;
}

/** 判定。抽成纯函数，便于喂**故意坏的**输入验证它真的会红。 */
export function judge(sources) {
  const problems = [];
  let sawFfmpeg = 0;
  let sawSequential = 0;

  for (const [name, text] of sources) {
    if (!text.includes('Command::new("ffmpeg")') && !text.includes("Command::new(\"ffmpeg\")")) {
      continue;
    }
    sawFfmpeg += 1;
    for (const flag of SEEK_FLAGS) {
      if (text.includes(flag)) {
        problems.push(
          name + ' 里的 ffmpeg 出现了 ' + flag + ' —— **后端出片不许逐帧 seek**：' +
          '每次 seek 都要回到关键帧重解，比顺序解码慢一个量级。'
        );
      }
    }
    // 顺序管道的标志：把裸帧从 stdout 接出来。
    if (text.includes('rawvideo')) sawSequential += 1;
  }

  if (sawFfmpeg === 0) {
    // **拒绝在空集上通过** —— 与仓库其他守卫同一纪律。
    problems.push('一个 ffmpeg 调用都没扫到 —— 守卫拒绝在空集上通过（路径改了吗？）');
  }
  if (sawFfmpeg > 0 && sawSequential === 0) {
    problems.push('有 ffmpeg 调用，但没有任何一处走顺序管道（rawvideo）—— 判据的前提不成立');
  }
  return problems;
}

function runSelfTest() {
  let passed = 0;
  const expect = (name, problems, shouldBeEmpty) => {
    if ((problems.length === 0) !== shouldBeEmpty) {
      throw new Error('自检失败：' + name + ' -> ' + JSON.stringify(problems));
    }
    passed += 1;
  };

  const good = [['a.rs', 'Command::new("ffmpeg").args(["-i", "in.mp4", "-f", "rawvideo", "-pix_fmt", "rgba", "-"]);']];
  expect('顺序管道 -> 通过', judge(good), true);

  const seek = [['a.rs', 'Command::new("ffmpeg").args(["-ss", "1.0", "-i", "in.mp4", "-f", "rawvideo", "-"]);']];
  expect('出现 -ss -> 必须红', judge(seek), false);

  const seek2 = [['a.rs', 'Command::new("ffmpeg").args(["-seek_timestamp", "1", "-i", "in.mp4", "-f", "rawvideo"]);']];
  expect('出现 -seek_timestamp -> 必须红', judge(seek2), false);

  const noPipe = [['a.rs', 'Command::new("ffmpeg").args(["-i", "in.mp4", "out.png"]);']];
  expect('有 ffmpeg 但没走顺序管道 -> 必须红', judge(noPipe), false);

  expect('空集 -> 必须红（不能空转）', judge([]), false);
  expect('没有 ffmpeg 的文件 -> 必须红（同为空集）', judge([['a.rs', 'fn main() {}']]), false);

  console.log('✓ 顺序解码守卫自检通过（' + passed + ' 条断言）');
}

function main() {
  if (process.argv.includes('--self-test')) { runSelfTest(); return; }
  const files = backendFiles();
  if (files.length === 0) {
    console.error('  - 后端的 rs 文件一个都没找到（路径改了吗？）');
    process.exitCode = 1;
    return;
  }
  const sources = files.map((path) => [path.replace(REPO_ROOT + '\\', ''), readFileSync(path, 'utf8')]);
  const problems = judge(sources);
  if (problems.length > 0) {
    for (const problem of problems) console.error('  - ' + problem);
    console.error('后端解码路径违反了「顺序解码、不许逐帧 seek」');
    process.exitCode = 1;
    return;
  }
  console.log('✓ 后端解码路径是顺序的（扫描 ' + sources.length + ' 个文件，无 -ss / -seek_timestamp）');
}

main();
