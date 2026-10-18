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
// # 清单也是**读**来的，不是抄的（第 80 轮修）
//
// 这里原来是一份**手抄的 26 条**名单，而 `run-guards.mjs` 已经 27 条
// （漏了 `check-frame-pacing.mjs`）—— 于是这张表少一行、少一行**永远看不出来**。
// 现在直接 `import` 那份唯一清单（`run-guards.mjs` 的入口守卫保证 import 不会顺手跑全套）。
//
// # 三态，不是两态
//
// `正检` 与 `自检` 各有三态：green / red / **起不来**。
// "起不来"（spawn 失败、拿到 null 退出码）**不许**并进 red，也不许并进"没有自检" ——
// 本会话就吃过这个读数：整套守卫在沙箱里起不了子进程，这张表把 27 条全报成
// "正检红 + 零自检"。那是**环境**的读数，不是仓库的结论。

import { spawnSync } from 'node:child_process';
import { closeSync, mkdirSync, openSync, readFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import { GUARDS, DEPS, IN_CI } from './run-guards.mjs';

const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');

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

/** 跑一次。三态：green / red / 起不来（spawn 出错拿不到退出码）。 */
//
// 输出走**文件重定向**而不是管道：受限环境里建不出命名管道，`stdio: 'pipe'`
// 会直接 EPERM，把环境问题读成"27 条全红"。见 run-guards.mjs 里 runOne 的说明。
function runOnce(script, args, logDir = join(REPO_ROOT, 'target', 'guards-log')) {
  mkdirSync(logDir, { recursive: true });
  const outPath = join(logDir, 'inventory-' + script + '.out');
  const errPath = join(logDir, 'inventory-' + script + '.err');
  const outFd = openSync(outPath, 'w');
  const errFd = openSync(errPath, 'w');
  const result = spawnSync(process.execPath, [join(REPO_ROOT, 'scripts', script), ...args], {
    cwd: REPO_ROOT,
    windowsHide: true,
    maxBuffer: 1 << 28,
    stdio: ['ignore', outFd, errFd],
  });
  closeSync(outFd);
  closeSync(errFd);
  if (result.error) return { state: '起不来', detail: String(result.error.message), text: '' };
  let text = '';
  for (const path of [outPath, errPath]) {
    try {
      text += readFileSync(path, 'utf8');
    } catch {
      // 读不到就是没输出
    }
  }
  return { state: result.status === 0 ? 'green' : 'red', detail: null, text };
}

const rows = [];
for (const guard of GUARDS) {
  const selfTest = runOnce(guard.script, ['--self-test']);
  const plain = runOnce(guard.script, guard.args);
  const mutated = selfTest.state === '起不来' ? null : mutatedFrom(selfTest.text);
  rows.push({ guard, selfTest, plain, mutated });
}

console.log('| 守卫 | 依赖档 | 正检 | 自检 | 自检几个变异 |');
console.log('|---|---|---|---|---|');
for (const row of rows) {
  // **"读不出来"与"没有"是两件事**：起不来的那条不许印成"没有自检"。
  const mutated = row.selfTest.state === '起不来'
    ? '读不出来（起不来）'
    : (row.mutated === null ? '**没有自检**' : String(row.mutated) + ' 个');
  console.log('| `' + row.guard.script + '` | `' + row.guard.deps + '` | ' + row.plain.state + ' | ' +
    row.selfTest.state + ' | ' + mutated + ' |');
}

const unspawnable = rows.filter((row) => row.plain.state === '起不来' || row.selfTest.state === '起不来');
const withSelfTest = rows.filter((row) => row.mutated !== null).length;
const totalMutations = rows.reduce((sum, row) => sum + (row.mutated === null ? 0 : row.mutated), 0);

console.log('');
if (unspawnable.length === rows.length) {
  console.log('**一条都没跑起来** —— 自检家底这次读不出来。不印数字：把"读不到"印成"0 个变异"就是撒谎。');
} else {
  console.log(rows.length + ' 条守卫里 ' + withSelfTest + ' 条带自检，共 ' + totalMutations + ' 个变异。');
}
if (unspawnable.length > 0) {
  console.log('');
  console.log('⚠️ 有 ' + unspawnable.length + ' 条**起不来**（spawn 失败）：' +
    unspawnable.map((row) => row.guard.script).join(' '));
  console.log('   这是**环境**的读数，不是仓库的结论 —— 换普通终端复跑再下结论。');
  console.log('   签名示例：' + (unspawnable[0].plain.detail === null ? unspawnable[0].selfTest.detail : unspawnable[0].plain.detail));
} else {
  console.log('没有自检的那几条，靠的是"正检本身够具体"—— 要不要补，看它们的失效模式。');
}
console.log('依赖档：' + DEPS.join(' / ') + '；CI 该覆盖：' + IN_CI.join(' / ') +
  '（`node scripts/run-guards.mjs --only-deps ' + IN_CI.join(',') + '`）');
