#!/usr/bin/env node
// dhampir-media 的**状态守卫**。
//
// 为什么需要它：那个 crate 定义了 5 个 trait，但**零实现、全仓零引用**。
// 下游看到 trait 会以为有一条铺好的路 —— **空契约比没有契约更容易误导**。
// 光在文档里写一句不够：一重构就没了。所以让守卫盯着它。
//
// 判据：
//   * 仍然没有任何实现 -> lib.rs **必须**含状态声明；
//   * 一旦有了实现 -> 守卫**提醒删掉**声明（过时的声明和缺失的声明一样有害）。
//
// 用法：
//   node scripts/check-media-status.mjs             检查
//   node scripts/check-media-status.mjs --self-test 只跑守卫自检

import { existsSync, readFileSync, readdirSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

export const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');

/** 契约里声明的 trait。守卫要确认它们**还在** —— 否则这条检查就成了空转。 */
export const CONTRACT_TRAITS = ['Demuxer', 'VideoDecoder', 'VideoEncoder', 'AudioEncoder', 'Muxer'];

/** 声明里必须出现的一句（防止被改写成含糊的说法）。 */
export const REQUIRED_PHRASE = '目标接口，零实现';

/** 数一数每个契约 trait 各有多少个实现。 */
export function countImplementations(sources) {
  const counts = new Map(CONTRACT_TRAITS.map((name) => [name, 0]));
  for (const text of sources) {
    for (const line of text.split('\n')) {
      const trimmed = line.trim();
      if (!trimmed.startsWith('impl')) continue;
      for (const name of CONTRACT_TRAITS) {
        // 只认 `impl Trait for Type`（带 for）。故意不写正则：
        // 反斜杠在多层字符串里过一遍就变味，而这里根本用不上那么强的表达力。
        if (trimmed.startsWith('impl ' + name + ' for ')) counts.set(name, counts.get(name) + 1);
        else if (trimmed.startsWith('impl<') && trimmed.includes(' ' + name + ' for ')) {
          counts.set(name, counts.get(name) + 1);
        }
      }
    }
  }
  return counts;
}

/** 返回问题清单（空 = 通过）。 */
export function judge(libText, sources) {
  const problems = [];
  const counts = countImplementations(sources);
  let total = 0;
  for (const value of counts.values()) total += value;
  const declared = CONTRACT_TRAITS.filter((name) => libText.includes('pub trait ' + name)).length;

  if (declared !== CONTRACT_TRAITS.length) {
    problems.push(
      '契约里只剩 ' + declared + ' 个 trait（期望 ' + CONTRACT_TRAITS.length +
      '）—— 要么契约改了，要么这条守卫该改'
    );
  }

  if (total === 0) {
    if (!libText.includes(REQUIRED_PHRASE)) {
      problems.push(
        '零实现，但 lib.rs 里没有状态声明（缺「' + REQUIRED_PHRASE + '」）——' +
        '下游会以为这是一条铺好的路'
      );
    }
  } else {
    if (libText.includes(REQUIRED_PHRASE)) {
      problems.push(
        '已经有 ' + total + ' 个实现，但 lib.rs 仍写着「' + REQUIRED_PHRASE + '」——' +
        '声明过时了，请连同这条守卫一起更新'
      );
    }
  }
  return problems;
}

function mediaSources() {
  const dir = join(REPO_ROOT, 'crates', 'dhampir-media', 'src');
  if (!existsSync(dir)) return [];
  return readdirSync(dir)
    .filter((name) => name.endsWith('.rs'))
    .map((name) => readFileSync(join(dir, name), 'utf8'));
}

function runSelfTest() {
  let passed = 0;
  const expect = (name, problems, shouldBeEmpty) => {
    if ((problems.length === 0) !== shouldBeEmpty) {
      throw new Error('自检失败：' + name + ' -> ' + JSON.stringify(problems));
    }
    passed += 1;
  };
  const traits = CONTRACT_TRAITS.map((name) => 'pub trait ' + name + ' {}').join('\n');
  const withNotice = traits + '\n//! ## 当前状态：' + REQUIRED_PHRASE + '\n';
  const noImpl = [''];
  const withImpl = ['impl VideoDecoder for Thing {}'];

  expect('零实现 + 有声明 -> 通过', judge(withNotice, noImpl), true);
  expect('零实现 + 无声明 -> 必须红', judge(traits, noImpl), false);
  expect('有实现 + 声明还在 -> 必须红（过时声明同样有害）', judge(withNotice, withImpl), false);
  expect('有实现 + 声明已删 -> 通过', judge(traits, withImpl), true);
  expect('trait 少了 -> 必须红（防止守卫空转）', judge('pub trait Demuxer {}', noImpl), false);
  // 反向：`impl Demuxer {` 这种固有实现块**不该**被数成 trait 实现。
  expect('固有 impl 块不算实现', judge(withNotice, ['impl Demuxer {']), true);

  console.log('OK media 状态守卫自检通过（' + passed + ' 条断言）');
}

function main() {
  if (process.argv.includes('--self-test')) { runSelfTest(); return; }
  const libPath = join(REPO_ROOT, 'crates', 'dhampir-media', 'src', 'lib.rs');
  if (!existsSync(libPath)) {
    console.error('  - 找不到 crates/dhampir-media/src/lib.rs');
    console.error('media 状态守卫无法执行');
    process.exitCode = 1;
    return;
  }
  const problems = judge(readFileSync(libPath, 'utf8'), mediaSources());
  if (problems.length > 0) {
    for (const problem of problems) console.error('  - ' + problem);
    console.error('dhampir-media 的状态声明与实际不符');
    process.exitCode = 1;
    return;
  }
  const counts = countImplementations(mediaSources());
  let total = 0;
  for (const value of counts.values()) total += value;
  console.log('OK dhampir-media 状态声明与实现情况一致（当前实现数 ' + total + '）');
}

main();
