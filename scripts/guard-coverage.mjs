#!/usr/bin/env node
// 「**改了实现 ⇒ 该跑哪条守卫 / 该改哪份文档**」—— 从守卫自己的题头生成。
//
//   node scripts/guard-coverage.mjs
//
// # 为什么是生成
//
// 手写一份"哪个守卫管什么"一定会过期（本会话反复吃过）。这里读的是**守卫自己的题头**
// 与它源码里出现的仓库路径 —— 守卫改了措辞、换了绑定，这张表跟着变。
//
// 它**只读文件、不 spawn 任何东西** —— 所以在连管道都没有的环境里也能跑。

import { readFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');

// 与 scripts/run-guards.mjs 同一份清单（那份是唯一出处；这里抄名字会漂，所以直接从它读）。
const runner = readFileSync(join(REPO_ROOT, 'scripts', 'run-guards.mjs'), 'utf8');
const block = runner.slice(runner.indexOf('export const GUARDS = ['), runner.indexOf('];', runner.indexOf('export const GUARDS = [')));
const GUARDS = [...block.matchAll(/script: '([^']+)'/g)].map((match) => match[1]);

console.log('| 守卫 | 它管什么（取自它自己的题头） | 绑定的仓库路径 |');
console.log('|---|---|---|');
for (const guard of GUARDS) {
  const text = readFileSync(join(REPO_ROOT, 'scripts', guard), 'utf8');
  const head = text.split('\n').slice(0, 40);
  let summary = '';
  const at = head.findIndex((line) => /它管什么/.test(line));
  if (at >= 0) {
    for (let i = at + 1; i < head.length; i += 1) {
      const line = head[i].replace(/^\/\/\s?/, '').trim();
      if (line !== '' && !line.startsWith('#')) { summary = line; break; }
    }
  }
  if (summary === '') {
    for (const line of head) {
      const trimmed = line.replace(/^\/\/\s?/, '').trim();
      if (trimmed !== '' && !trimmed.startsWith('#') && !line.startsWith('#!')) { summary = trimmed; break; }
    }
  }
  // **必须像路径**：只留纯 ASCII 路径字符。
  //
  // 第一版把注释与消息串也抓了进来（`crates/名字`、`web/node_modules 存在——说明引了 npm 依赖` ✗）——
  // 那是"看起来像表、其实是噪音"，比空着更坏（读的人会照它去跑 ✗）。
  const lookLikePath = (value) => /^[A-Za-z0-9_./-]+$/.test(value);
  const paths = [...new Set(
    [...text.matchAll(/'(docs\/[^']+|plan\/[^']+|scripts\/[^']+|web\/[^']+|crates\/[^']+)'/g)].map((match) => match[1]),
  )].filter(lookLikePath).slice(0, 4);
  console.log('| `' + guard + '` | ' + summary.slice(0, 120) + ' | ' + paths.join(' , ') + ' |');
}
console.log('');
console.log('共 ' + GUARDS.length + ' 条。**要跑哪些**：改到上表某一行的绑定路径，就跑那一行的守卫；不确定就跑全部（`node scripts/run-guards.mjs`）。');
