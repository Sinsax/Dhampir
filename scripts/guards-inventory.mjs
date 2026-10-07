#!/usr/bin/env node
// 「**判据家底**」表 —— 每个守卫管什么、自检几个变异、能不能红。
//
//   node scripts/guards-inventory.mjs
//
// # 为什么是生成而不是手写
//
// 「守卫会红」是这个仓库最核心的承诺（"守卫若不会红，就不是守卫"）。
// 手写一张"谁有几个变异"的表一定会过期 —— 所以这里**逐个跑 `--self-test`**，
// 从它们自己的输出里读数字。表因此永远等于现实。
//
// 顺带报出每条守卫**现在**是否通过（跑不动的那种要写明是环境问题）。

import { spawnSync } from 'node:child_process';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');

// 与 scripts/run-guards.mjs 同一份清单（那边跑正检、这边跑自检）。
const GUARDS = [
  'check-core-purity.mjs',
  'check-dep-graph.mjs',
  'check-text-hygiene.mjs',
  'check-linux-portability.mjs',
  'check-sequential-decode.mjs',
  'check-web-invariants.mjs',
  'check-waapi2doc.mjs',
  'check-anim-eval.mjs',
  'check-host-parity.mjs',
  'check-capabilities.mjs',
  'check-dom-css.mjs',
  'check-dom-differences.mjs',
  'check-backend-seam.mjs',
  'check-defects.mjs',
  'check-preview-parity.mjs',
  'check-overlay-plumbing.mjs',
  'check-m1-record.mjs',
  'check-m2-record.mjs',
  'check-media-status.mjs',
  'check-effect-registry.mjs',
  'check-local-backend.mjs',
  'check-same-origin-proxy.mjs',
  'api-surface.mjs',
  'timeline-contract.mjs',
  'check-cli.mjs',
  'check-dual-end.mjs',
];

/** 从守卫的自检输出里读"几个变异/几个错误映射器"。读不到就返回 null（= 它没有自检）。 */
// **别只认一种说法。** 本仓各守卫的措辞五花八门：`N 个变异全部被抓住`、`N 条断言`、
// `N 条用例`、`N 个错误映射器`，还有 `8 条解析用例 + 5 条规则用例 + …` 这种**加和**形式。
// 第 64 轮我第一版只认前两种，于是把"26 条里 6 条有自检"当成结论写进了 plan ✗ ——
// 真相是 **26 条全都有**。所以这里把所有能认出来的计数**加起来**；一条都认不出才返回 null
// （诚实地说"读不出来"，而不是替它下结论）。
function mutatedFrom(text) {
  const numbers = [...text.matchAll(/(\d+)\s*(?:条[^，。、）)]*?用例|条断言|条变异|个变异|个错误映射器)/g)].map(
    (match) => Number(match[1]),
  );
  if (numbers.length === 0) return null;
  return numbers.reduce((sum, value) => sum + value, 0);
}

const rows = [];
for (const guard of GUARDS) {
  const passed = spawnSync('node', [join(REPO_ROOT, 'scripts', guard)], { encoding: 'utf8', cwd: REPO_ROOT });
  const selfTest = spawnSync('node', [join(REPO_ROOT, 'scripts', guard), '--self-test'], { encoding: 'utf8', cwd: REPO_ROOT });
  const combined = String(selfTest.stdout || '') + String(selfTest.stderr || '');
  const mutated = mutatedFrom(combined);
  rows.push({
    guard,
    ok: passed.status === 0,
    selfTestOk: selfTest.status === 0,
    mutated,
  });
}

console.log('| 守卫 | 正检 | 自检 | 自检几个变异 |');
console.log('|---|---|---|---|');
for (const row of rows) {
  const mutated = row.mutated === null ? '**没有自检**' : String(row.mutated) + ' 个';
  console.log('| `' + row.guard + '` | ' + (row.ok ? 'green' : 'red') + ' | ' + (row.selfTestOk ? 'green' : 'red') + ' | ' + mutated + ' |');
}
const withSelfTest = rows.filter((row) => row.mutated !== null).length;
const totalMutations = rows.reduce((sum, row) => sum + (row.mutated === null ? 0 : row.mutated), 0);
console.log('');
console.log(rows.length + ' 条守卫里 ' + withSelfTest + ' 条带自检，共 ' + totalMutations + ' 个变异。');
console.log('没有自检的那几条，靠的是\\"正检本身够具体\\"—— 要不要补，看它们的失效模式。');
