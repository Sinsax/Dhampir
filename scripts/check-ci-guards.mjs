#!/usr/bin/env node
// **CI 有没有把该跑的守卫跑掉** —— 对着 scripts/run-guards.mjs 的依赖档核 .github/workflows/ci.yml。
//
//   node scripts/check-ci-guards.mjs
//   node scripts/check-ci-guards.mjs --self-test
//
// # 它管什么（第 80 轮加）
//
// 在此之前 CI 只跑 4 条守卫（core 纯度 / 依赖方向 / 文本卫生 / 第三方许可），
// 而守卫已经有 27 条 —— 其中一大半（能力登记表、宿主对表、waapi2doc、anim-eval、
// 差异台账、DOM 映射…）**只在这台开发机上跑过**。判据不上 CI 的真实后果不是"少验一点"，
// 而是它会在下一次改动里**悄悄烂掉**：本地谁记得跑谁才知道。
//
// 但也不能反过来"全塞进 CI"：runner 上没 ffmpeg / GPU / 浏览器 / 已构建产物，
// 那些档跑起来只会红，把"环境没装好"伪装成"代码回归"。所以规矩是：
//
//   * 每档守卫**只能有一处真相**（GUARDS[].deps + IN_CI，都在 run-guards.mjs 里）；
//   * ci.yml 用 --only-deps <档,档> 声明它跑哪几档 —— 不逐条抄名字（抄一定会漂）；
//   * 这里判：**漏档**（该跑没跑）、**多档**（跑了跑不了的）、**点名了不存在的脚本**。

import { existsSync, readFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import { GUARDS, DEPS, IN_CI } from './run-guards.mjs';

export const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');
export const CI_PATH = '.github/workflows/ci.yml';

/**
 * 不是守卫、但 CI 里确实要点名的脚本（比如发布件许可清单）。
 * **白名单是显式的**：没进白名单又不在 GUARDS 里的脚本名一律判红 —— 名单漂了就看得见。
 */
export const CI_WHITELIST = ['licenses.mjs', 'run-guards.mjs', 'run-wasm-tests.mjs'];

/** 判据本体。纯函数：喂坏输入的入口在自检里。 */
export function judgeCiConfig(text, options = {}) {
  const guards = options.guards === undefined ? GUARDS : options.guards;
  const inCi = options.inCi === undefined ? IN_CI : options.inCi;
  const deps = options.deps === undefined ? DEPS : options.deps;
  const whitelist = options.whitelist === undefined ? CI_WHITELIST : options.whitelist;
  const exists = options.exists === undefined ? (path) => existsSync(join(REPO_ROOT, path)) : options.exists;
  const problems = [];
  // **注释不算数**：ci.yml 的注释里到处提到脚本名，把注释也算进"点名"会逼着人不敢写注释。
  const body = String(text).split(/\r?\n/).filter((line) => !/^\s*#/.test(line)).join('\n');

  const named = new Set([...body.matchAll(/scripts\/([A-Za-z0-9._-]+\.mjs)/g)].map((match) => match[1]));
  const known = new Set(guards.map((guard) => guard.script));
  for (const name of named) {
    if (known.has(name)) continue;
    if (whitelist.includes(name)) {
      if (!exists(join('scripts', name))) {
        problems.push('ci.yml 点名了白名单里的 ' + name + '，但 scripts/ 下没有这个文件');
      }
      continue;
    }
    problems.push('ci.yml 点名了 ' + name + '：它既不在守卫清单里，也不在白名单里 —— 名单会漂，先决定它算哪一种');
  }

  const listed = [...body.matchAll(/--only-deps[=\s]+([A-Za-z0-9, ]+)/g)]
    .flatMap((match) => match[1].split(',').map((token) => token.trim()).filter((token) => token !== ''));
  if (listed.length === 0) {
    problems.push('ci.yml 里没有 --only-deps —— CI 跑哪几档必须写出来（档位名单的唯一出处是 run-guards.mjs）');
  }
  for (const token of new Set(listed)) {
    if (!deps.includes(token)) problems.push('--only-deps 里有认不出的依赖档：' + token);
  }
  const covered = new Set(listed);
  for (const dep of inCi) {
    if (!covered.has(dep)) {
      problems.push('CI 应当覆盖依赖档 ' + dep + '，但 ci.yml 的 --only-deps 里没有它 —— 那一档的守卫会变成"只有记得跑的人跑过"');
    }
  }
  for (const dep of covered) {
    if (!inCi.includes(dep)) {
      problems.push('ci.yml 跑了 ' + dep + ' 档 —— 那档在 runner 上缺东西（ffmpeg / GPU / 浏览器 / 已构建产物），跑起来只会把"环境没装好"伪装成"回归"');
    }
  }

  for (const name of named) {
    const guard = guards.find((item) => item.script === name);
    if (guard === undefined) continue;
    if (!inCi.includes(guard.deps)) {
      problems.push('ci.yml 直接跑了 ' + name + '（deps=' + guard.deps + '），但那一档不在 IN_CI 里');
    }
  }

  if (!/run-guards\.mjs\s+--self-test/.test(body)) {
    problems.push('ci.yml 没跑 node scripts/run-guards.mjs --self-test —— 「清单少一条/多一条」这件事也得有人盯');
  }
  return problems;
}

const GOOD_CI = [
  'jobs:',
  '  guard:',
  '    steps:',
  '      - name: 守卫目录自检',
  '        run: node scripts/run-guards.mjs --self-test',
  '      - name: 静态档 + 记录档',
  '        run: node scripts/run-guards.mjs --only-deps static,records',
  '      - name: 许可清单',
  '        run: node scripts/licenses.mjs --check',
  '  check-native:',
  '    steps:',
  '      - name: 契约类',
  '        run: node scripts/run-guards.mjs --only-deps cargo',
  '',
].join('\n');

function selfTest() {
  const failures = [];
  let checks = 0;
  const pass = (name, ok) => {
    checks += 1;
    if (!ok) failures.push('自检「' + name + '」不成立');
  };
  const depsOf = (text) => judgeCiConfig(text, { exists: () => true });

  pass('合格的 ci.yml 必须全绿', depsOf(GOOD_CI).length === 0);
  pass('漏一档（static）要判红', depsOf(GOOD_CI.replace('static,records', 'records')).some((p) => p.includes('static')));
  pass('多一档（browser）要判红', depsOf(GOOD_CI + '      - name: 浏览器\n        run: node scripts/run-guards.mjs --only-deps browser\n').some((p) => p.includes('browser')));
  // 名字要用 ASCII：脚本名的字符类里没有汉字，用汉字会**匹配不上**，
  // 于是这条自检看起来"抓不到"，其实是被测输入根本没进判据（第一次就踩了）。
  pass('点名没登记的脚本要判红', depsOf(GOOD_CI + '      - name: 野的\n        run: node scripts/check-not-registered.mjs\n').some((p) => p.includes('既不在守卫清单里')));
  pass('认不出的档要判红', depsOf(GOOD_CI.replace('static,records', 'nope')).some((p) => p.includes('认不出的依赖档')));
  pass('没跑守卫目录自检要判红', depsOf(GOOD_CI.replace('run-guards.mjs --self-test', 'run-guards.mjs --list')).some((p) => p.includes('--self-test')));
  // 这条要用**不存在**的 exists：默认替身是"都存在"，白名单缺失那一支永远走不到。
  pass('白名单里的文件不存在要判红', judgeCiConfig(GOOD_CI.replace('licenses.mjs', 'licenses-gone.mjs'), { exists: () => false })
    .some((p) => p.includes('白名单')));
  pass('注释里的名字不算点名', depsOf('# run: node scripts/check-没登记.mjs\n' + GOOD_CI) .length === 0);
  pass('IN_CI 必须是 DEPS 的子集', IN_CI.every((dep) => DEPS.includes(dep)));
  pass('守卫清单非空', GUARDS.length > 0);
  if (failures.length > 0) {
    for (const failure of failures) console.error('  - ' + failure);
    console.error('✗ 自检不过');
    process.exit(1);
  }
  console.log('✓ 自检：' + checks + ' 条断言（漏档 / 多档 / 未知脚本 / 缺 --only-deps / 注释不算数）');
}

function main() {
  if (process.argv.includes('--self-test')) {
    selfTest();
    return;
  }
  const path = join(REPO_ROOT, CI_PATH);
  if (!existsSync(path)) {
    console.error('✗ 找不到 ' + CI_PATH);
    process.exit(1);
  }
  const problems = judgeCiConfig(readFileSync(path, 'utf8'));
  if (problems.length > 0) {
    for (const problem of problems) console.error('  - ' + problem);
    console.error('✗ CI 与守卫清单对不上');
    process.exit(1);
  }
  const inCiCount = GUARDS.filter((guard) => IN_CI.includes(guard.deps)).length;
  console.log('✓ CI 覆盖：' + inCiCount + ' / ' + GUARDS.length + ' 条守卫（档 ' + IN_CI.join(' / ') +
    '）；其余 ' + (GUARDS.length - inCiCount) + ' 条要本机资产（ffmpeg / GPU / 浏览器 / 已构建产物），不进 CI');
}

if (process.argv[1] !== undefined && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main();
}
