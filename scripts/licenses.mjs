// 第三方许可清单：生成 / 核对 / 分发时抽取。
//
// # 为什么要有它
//
// 本仓的许可证是 **Apache-2.0 单许可**（见根 `Cargo.toml` 的 `[workspace.package] license`）。
// Apache-2.0 §4 要求：分发时**随附许可证全文**、保留版权与许可声明、**如实列出第三方组件**。
// 那些组件是 150+ 个 crate，手抄一定会漂 —— 所以清单是**生成的**，并由 `--check` 钉住。
//
// # 用法
//
//     node scripts/licenses.mjs --write            生成 THIRD-PARTY-LICENSES.md
//     node scripts/licenses.mjs --check            核对（漂了就退 1；不写盘）
//     node scripts/licenses.mjs --bundle <目录>    把每个依赖自带的 LICENSE*/NOTICE* 抽到 <目录>/
//     node scripts/licenses.mjs --self-test        只跑自检（不调 cargo、不写盘）
//
// 退出码：**0** 一致 / **1** 漂了或抽不出来 / **2** 用法错。
//
// # 口径
//
// * 清单内容**只来自 `cargo metadata`** 的 `name` / `version` / `license` 三样 —— 那三样由
//   crates.io 的索引决定，**与本机路径无关**，所以换台机器 `--check` 仍然成立。
// * **不把 `manifest_path` 写进清单**：本机绝对路径会让清单在别的机器上"永远漂"。
//   "这个依赖有没有自带许可文本"只在 `--bundle` 里管。
// * 每个 crate 目录下的 `LICENSE` 必须与根 `LICENSE-APACHE` **逐字节相同** —— 由 `--check` 钉住。

import { execFileSync } from 'node:child_process';
import { copyFileSync, existsSync, mkdirSync, readFileSync, readdirSync, statSync, writeFileSync } from 'node:fs';
import { basename, dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const REPO = resolve(fileURLToPath(new URL('..', import.meta.url)));
const OUT_FILE = join(REPO, 'THIRD-PARTY-LICENSES.md');
const ROOT_LICENSE = join(REPO, 'LICENSE-APACHE');
const WORKSPACE_LICENSE = 'Apache-2.0';
const CRATES = readdirSync(join(REPO, 'crates')).map((n) => join(REPO, 'crates', n));

// ---------------------------------------------------------------- 纯函数（可自检）
/** 只取登记在案的依赖（`source` 非空 = 来自 registry），按 (name, version) 去重并排序。 */
export function thirdParty(packages) {
  const seen = new Map();
  for (const p of packages) {
    if (!p.source) continue;
    const key = `${p.name}@${p.version}`;
    if (seen.has(key)) continue;
    seen.set(key, {
      name: p.name,
      version: p.version,
      license: p.license ?? '(未声明)',
      manifestPath: p.manifest_path ?? null,
    });
  }
  return [...seen.values()].sort((a, b) => a.name.localeCompare(b.name) || a.version.localeCompare(b.version));
}

/** 按许可表达式汇总：`license -> 用了它的 crate 数`。 */
export function rollup(list) {
  const m = new Map();
  for (const d of list) m.set(d.license, (m.get(d.license) ?? 0) + 1);
  return [...m.entries()].sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0]));
}

/** 生成清单正文。**给定同一份依赖表，输出必须逐字节相同**（自检盯着这一条）。 */
export function render(list, git = '') {
  const lines = [];
  lines.push('# 第三方许可清单（生成物，不要手改）');
  lines.push('');
  lines.push(`本仓自身的许可证是 **${WORKSPACE_LICENSE}**（见 [\`LICENSE-APACHE\`](LICENSE-APACHE)）。`);
  lines.push('下表是**编译进来的第三方 crate** 的许可声明，来自 `cargo metadata`（即 `Cargo.lock` 钉住的那一套）。');
  lines.push('');
  lines.push('```bash');
  lines.push('node scripts/licenses.mjs --write     # 重新生成（改了 Cargo.toml/Cargo.lock 之后必须重生成）');
  lines.push('node scripts/licenses.mjs --check     # 核对：漂了就退 1');
  lines.push('node scripts/licenses.mjs --bundle <目录>  # 抽取每个依赖自带的 LICENSE*/NOTICE*（分发时用）');
  lines.push('```');
  lines.push('');
  lines.push('分发（例如 `node scripts/package.mjs` 打的产物）时，必须**随附**本文件与 `LICENSE-APACHE`，');
  lines.push('并保留各依赖的版权与许可声明；`--bundle` 就是替你把每个依赖自带的文本抽出来的那一步。');
  lines.push('');
  lines.push(`共 **${list.length}** 个第三方 crate。`);
  lines.push('');
  lines.push('## 按许可表达式汇总');
  lines.push('');
  lines.push('| 许可表达式 | crate 数 |');
  lines.push('|---|---|');
  for (const [lic, n] of rollup(list)) lines.push(`| \`${lic}\` | ${n} |`);
  lines.push('');
  lines.push('## 明细');
  lines.push('');
  lines.push('| crate | 版本 | 许可 |');
  lines.push('|---|---|---|');
  for (const d of list) lines.push(`| \`${d.name}\` | ${d.version} | \`${d.license}\` |`);
  lines.push('');
  if (git) lines.push(`<!-- 生成时 HEAD：${git} -->`);
  lines.push('');
  return lines.join('\n');
}

// ---------------------------------------------------------------- 自检
function selfTest() {
  let checks = 0;
  const bad = [];
  const expect = (ok, label) => { checks += 1; if (!ok) bad.push(label); };

  const fake = [
    { name: 'b', version: '2.0.0', license: 'MIT', source: 'registry+x', manifest_path: '/x/b' },
    { name: 'a', version: '1.0.0', license: 'Apache-2.0', source: 'registry+x', manifest_path: '/x/a' },
    { name: 'a', version: '1.0.0', license: 'Apache-2.0', source: 'registry+x', manifest_path: '/x/a' },
    { name: 'local', version: '0.0.1', license: 'Apache-2.0', source: null, manifest_path: '/repo/crates/local' },
  ];
  const list = thirdParty(fake);
  expect(list.length === 2, `去重/过滤后应为 2，实得 ${list.length}`);
  expect(list.every((d) => d.name !== 'local'), '本仓自己的 crate 不该进第三方表');
  expect(list[0].name === 'a' && list[1].name === 'b', '排序不对');
  expect(JSON.stringify(rollup(list)) === JSON.stringify([['Apache-2.0', 1], ['MIT', 1]]), '汇总不对');
  expect(render(list).includes('| `a` | 1.0.0 | `Apache-2.0` |'), '明细行不对');
  // 反向：换一个许可，输出必须跟着变（否则"钉住"是空转）
  const changed = thirdParty(fake.map((p) => (p.name === 'b' ? { ...p, license: 'ISC' } : p)));
  expect(render(changed) !== render(list), '改了许可却渲染出同一份清单');
  // 同一份输入必须逐字节稳定
  expect(render(list) === render(list), '同一份输入渲染两次不一样');
  // 未声明许可的要如实写出来，不许留空
  expect(render(thirdParty([{ name: 'c', version: '1', license: null, source: 'registry+x' }])).includes('(未声明)'), '未声明许可没被如实标出');

  if (bad.length > 0) {
    console.error('✗ 自检失败（先修脚本，别信它的结论）：');
    for (const b of bad) console.error(`    - ${b}`);
    process.exit(2);
  }
  console.log(`✓ licenses 自检：${checks} 项全绿`);
}

// ---------------------------------------------------------------- cargo metadata
function metadata() {
  try {
    const text = execFileSync('cargo', ['metadata', '--format-version', '1'], {
      cwd: REPO, encoding: 'utf8', maxBuffer: 256 * 1024 * 1024,
    });
    return JSON.parse(text);
  } catch (error) {
    console.error(`✗ 跑不了 cargo metadata：${error.message}`);
    process.exit(2);
  }
}

function gitHead() {
  try { return execFileSync('git', ['rev-parse', '--short', 'HEAD'], { cwd: REPO, encoding: 'utf8' }).trim(); }
  catch { return ''; }
}

/** 每个 crate 目录下的 LICENSE 必须与根 LICENSE-APACHE 逐字节相同。 */
function checkCrateLicenses() {
  const root = readFileSync(ROOT_LICENSE);
  const problems = [];
  for (const dir of CRATES) {
    const p = join(dir, 'LICENSE');
    if (!existsSync(p)) { problems.push(`${basename(dir)}/LICENSE 不在`); continue; }
    if (!readFileSync(p).equals(root)) problems.push(`${basename(dir)}/LICENSE 与根 LICENSE-APACHE 不一致`);
  }
  return problems;
}

function bundle(dir) {
  const meta = metadata();
  const list = thirdParty(meta.packages);
  const out = resolve(dir);
  let copied = 0;
  const missing = [];
  for (const d of list) {
    if (!d.manifestPath) { missing.push(d.name); continue; }
    const src = dirname(d.manifestPath);
    const files = readdirSync(src).filter((f) => /^(LICENSE|LICENCE|COPYING|NOTICE|UNLICENSE)/i.test(f));
    if (files.length === 0) { missing.push(`${d.name}@${d.version}`); continue; }
    const dst = join(out, 'licenses', `${d.name}-${d.version}`);
    mkdirSync(dst, { recursive: true });
    for (const f of files) { copyFileSync(join(src, f), join(dst, f)); copied += 1; }
  }
  copyFileSync(ROOT_LICENSE, join(out, 'LICENSE-APACHE'));
  if (existsSync(OUT_FILE)) copyFileSync(OUT_FILE, join(out, 'THIRD-PARTY-LICENSES.md'));
  console.log(`✓ 已抽取 ${copied} 份许可文本到 ${join(out, 'licenses')}`);
  console.log(`  另有 ${missing.length} 个 crate 的源码目录里没有许可文本（清单里仍在，许可以 SPDX 声明为准）`);
  for (const m of missing.slice(0, 8)) console.log(`    - ${m}`);
  if (missing.length > 8) console.log(`    （其余 ${missing.length - 8} 个略）`);
}

// ---------------------------------------------------------------- main
const argv = process.argv.slice(2);
const USAGE = `用法：node scripts/licenses.mjs [--write | --check | --bundle <目录> | --self-test]

  --write          生成 THIRD-PARTY-LICENSES.md
  --check          核对清单与 crate 里的 LICENSE（漂了就退 1）
  --bundle <目录>   抽取每个依赖自带的 LICENSE*/NOTICE* 到 <目录>/licenses/
  --self-test      只跑自检
`;
if (argv.includes('--help') || argv.includes('-h')) { console.log(USAGE); process.exit(0); }
for (let i = 0; i < argv.length; i += 1) {
  const a = argv[i];
  if (!['--write', '--check', '--bundle', '--self-test', '--help', '-h'].includes(a)) {
    console.error(`✗ 不认识的参数：${a}\n\n${USAGE}`); process.exit(2);
  }
  if (a === '--bundle') i += 1;
}
if (argv.includes('--self-test')) { selfTest(); process.exit(0); }

if (argv.includes('--bundle')) {
  const i = argv.indexOf('--bundle');
  if (!argv[i + 1]) { console.error(`✗ --bundle 要一个目录\n\n${USAGE}`); process.exit(2); }
  if (checkCrateLicenses().length > 0) {
    console.error('✗ crate 目录里的 LICENSE 与根不一致，先修再抽：');
    for (const p of checkCrateLicenses()) console.error(`    - ${p}`);
    process.exit(1);
  }
  bundle(argv[i + 1]);
  process.exit(0);
}

const expected = render(thirdParty(metadata().packages), gitHead());
const licProblems = checkCrateLicenses();

if (argv.includes('--write')) {
  if (licProblems.length > 0) {
    console.error('✗ crate 目录里的 LICENSE 与根不一致（`cp LICENSE-APACHE crates/<crate>/LICENSE`）：');
    for (const p of licProblems) console.error(`    - ${p}`);
    process.exit(1);
  }
  writeFileSync(OUT_FILE, expected, 'utf8');
  console.log(`✓ 已写 ${OUT_FILE}`);
  console.log(`  ${thirdParty(metadata().packages).length} 个第三方 crate；` + (gitHead() ? `HEAD ${gitHead()}` : '（没有 git 信息）'));
  process.exit(0);
}

// 默认与 --check 都是"核对"（默认不写盘，与本仓别的脚本同一条纪律）
let bad = 0;
if (!existsSync(OUT_FILE)) { console.error(`✗ ${OUT_FILE} 不在（跑 --write 生成）`); bad += 1; }
else {
  const actual = readFileSync(OUT_FILE, 'utf8');
  if (actual !== expected) {
    console.error('✗ THIRD-PARTY-LICENSES.md 与 cargo metadata 不一致（跑 --write 重生成）');
    const a = actual.split('\n'); const e = expected.split('\n');
    const first = a.findIndex((l, i) => l !== e[i]);
    console.error(`  第一处不同在第 ${first + 1} 行：`);
    console.error(`    盘上：${(a[first] ?? '(没有这一行)').slice(0, 90)}`);
    console.error(`    期望：${(e[first] ?? '(没有这一行)').slice(0, 90)}`);
    bad += 1;
  }
}
if (licProblems.length > 0) {
  console.error('✗ crate 目录里的 LICENSE 与根不一致：');
  for (const p of licProblems) console.error(`    - ${p}`);
  bad += 1;
}
if (bad > 0) process.exit(1);
console.log(`✓ 第三方清单与 crate 里的 LICENSE 都与代码一致（${thirdParty(metadata().packages).length} 个第三方 crate）`);
