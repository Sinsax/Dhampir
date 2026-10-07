#!/usr/bin/env node
// 「HTML 宿主与引擎之间**允许的差异**」台账的判据。
//
//   node scripts/check-dom-differences.mjs
//   node scripts/check-dom-differences.mjs --self-test
//
// # 它管什么
//
// scripts/dom-parity-differences.toml 是**允许的差异**的真值。它自己不会红，所以要有这条判据：
//
//   R1 每条都要有 status，且在三档里（未实测 / 已实测 / 明确接受）；
//   R2 **未实测的条目不许带数字** —— 没量过就把"待定"写成一个像门槛的数，
//      是这份仓库最反对的那种假数（那一份 framediff 表里同样的规矩是"按实测定档"）；
//   R3 未实测必须写清**量法**（method），否则下一个人不知道该怎么量；
//   R4 已实测必须有数字；明确接受必须写理由；
//   R5 不许有不认识的键（拼错的键被静默忽略，这份台账就等于没设）；
//   R6 每一条都要在**别处**被引用（docs/ 或能力登记表）—— 免得它变成没人读的清单。

import { existsSync, readdirSync, readFileSync, statSync } from 'node:fs';

/** 这个路径存在吗（不存在返回 false，不抛）。 */
function statSyncSafe(path) {
  return existsSync(path);
}
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const LEDGER = join(REPO_ROOT, 'scripts', 'dom-parity-differences.toml');

const STATUSES = ['未实测', '已实测', '明确接受'];
const NUMERIC_KEYS = ['max_abs_diff_max', 'mean_ssim_min', 'min_ssim_min', 'psnr_db_min', 'edge_max_diff_max'];
const KNOWN_KEYS = new Set(['status', 'method', 'note', ...NUMERIC_KEYS]);

/** 解析这份台账用到的 TOML 子集：`[节]`、`键 = "值"`、`键 = """多行"""`、`#` 注释。 */
export function parseLedger(text) {
  const sections = new Map();
  let current = null;
  const lines = String(text).split(/\r?\n/);
  for (let i = 0; i < lines.length; i += 1) {
    const raw = lines[i];
    const line = raw.trim();
    if (line === '' || line.startsWith('#')) continue;
    const section = line.match(/^\[([A-Za-z0-9_.-]+)\]$/);
    if (section !== null) {
      if (sections.has(section[1])) throw new Error('重复的节：' + section[1]);
      current = section[1];
      sections.set(current, new Map());
      continue;
    }
    const kv = line.match(/^([A-Za-z0-9_]+)\s*=\s*(.*)$/);
    if (kv === null) throw new Error('第 ' + (i + 1) + ' 行认不出来：' + raw);
    if (current === null) throw new Error('第 ' + (i + 1) + ' 行的键不在任何节里');
    const key = kv[1];
    let value = kv[2].trim();
    if (value === '"""') {
      const parts = [];
      i += 1;
      while (i < lines.length && lines[i].trim() !== '"""') {
        parts.push(lines[i]);
        i += 1;
      }
      if (i >= lines.length) throw new Error('多行值没有收尾的三引号：' + key);
      value = parts.join('\n').trim();
    } else if (value.startsWith('"') && value.endsWith('"')) {
      value = value.slice(1, -1);
    }
    if (sections.get(current).has(key)) throw new Error('同一个键写两遍：' + current + '.' + key);
    sections.get(current).set(key, value);
  }
  return sections;
}

const PLACEHOLDER = ['TODO', 'todo', '待定', '以后再说', '无', '-', ''];

/** 判据本体。纯函数：喂故意错的输入就能验证它会红。 */
export function judgeLedger(sections, references) {
  const problems = [];
  if (sections.size === 0) problems.push('台账是空的 —— 空文件集不算通过');
  for (const [name, keys] of sections) {
    const status = keys.get('status');
    if (!STATUSES.includes(status)) problems.push(name + ' 的 status 不在三档里：' + JSON.stringify(status));
    const note = String(keys.get('note') === undefined ? '' : keys.get('note')).trim();
    if (note.length < 8 || PLACEHOLDER.includes(note)) problems.push(name + ' 没写说明（或太短）');
    const numbers = [...keys.keys()].filter((key) => NUMERIC_KEYS.includes(key));
    if (status === '未实测') {
      if (numbers.length > 0) {
        problems.push(name + ' 是「未实测」却带着数字（' + numbers.join('、') + '）—— 没量过不许写成数');
      }
      const method = String(keys.get('method') === undefined ? '' : keys.get('method')).trim();
      // **量法里点名的脚本必须真的存在**（R7，第 62 轮加）。
    //
    // `method` 是给下一个人**照着做**的 —— 它点名一个已被改名/删掉的脚本，那份量法就成了装饰。
    // 本会话在别处已经吃过几次同类亏（陈旧断言、引用不存在的用例、说假话的条目）。
    for (const script of method.match(/scripts\/[a-z0-9-]+\.mjs/g) || []) {
      if (!statSyncSafe(join(REPO_ROOT, script))) {
        problems.push(name + ' 的量法点名了 ' + script + '，但仓库里没有这个脚本');
      }
    }
    if (method.length < 20 || PLACEHOLDER.includes(method)) problems.push(name + ' 是「未实测」，必须写清量法（method）');
    }
    if (status === '已实测' && numbers.length === 0) problems.push(name + ' 说「已实测」，却没有数字');
    for (const key of keys.keys()) if (!KNOWN_KEYS.has(key)) problems.push(name + ' 有不认识的键：' + key);
    if (!references.has(name)) {
      problems.push(name + ' 在别处（docs/ 或能力登记表）没有被引用 —— 会变成一份没人读的清单');
    }
  }
  return problems;
}

/** 收集别处对这些节名的引用。 */
function collectReferences() {
  const found = new Set();
  const visit = (dir) => {
    for (const entry of readdirSync(dir, { withFileTypes: true })) {
      const full = join(dir, entry.name);
      if (entry.isDirectory()) {
        if (['target', 'node_modules', '.git'].includes(entry.name)) continue;
        visit(full);
        continue;
      }
      if (!/\.(md|json)$/.test(entry.name)) continue;
      if (full === LEDGER) continue;
      const text = readFileSync(full, 'utf8');
      // 名字可以有多段（`filter.drop-shadow.spread`）—— 原先只认两段，
      // 于是三段名会被当成"没人引用"，那是**守卫自己的洞**（第 28 轮撞上）。
      for (const match of text.matchAll(/\b([a-z][a-z0-9_]*(?:\.[a-z0-9_-]+)+)\b/g)) found.add(match[1]);
    }
  };
  visit(join(REPO_ROOT, 'plan'));
  visit(join(REPO_ROOT, 'docs'));
  visit(join(REPO_ROOT, 'web'));
  return found;
}

function selfTest(sections, references) {
  const clean = judgeLedger(sections, references);
  if (clean.length !== 0) {
    console.error('  - 自检失败：没改的输入本来就红：' + clean[0]);
    process.exit(1);
  }
  const clone = () => new Map([...sections].map(([name, keys]) => [name, new Map(keys)]));
  const mutations = [
    ['status 不在三档里', (s) => { s.get('geometry.edge_antialias').set('status', 'maybe'); }],
    ['未实测却带着数字', (s) => { s.get('geometry.edge_antialias').set('edge_max_diff_max', '2'); }],
    ['未实测却没写量法', (s) => { s.get('geometry.edge_antialias').delete('method'); }],
    ['已实测却没有数字', (s) => { s.get('blend.add_plus_lighter').set('status', '已实测'); }],
    ['不认识的键', (s) => { s.get('lottie.vector').set('tolerance', '0.01'); }],
    ['说明太短', (s) => { s.get('lottie.vector').set('note', '无'); }],
    ['量法点了一个不存在的脚本', (s) => {
      // 注意用**ASCII 脚本名**：规则的正则只认这种（真实脚本名都是 ASCII）。
      s.get('geometry.edge_antialias').set('method', '用 scripts/no-such-script.mjs 去量这一圈边缘');
    }],
  ];
  let caught = 0;
  for (const [name, breakIt] of mutations) {
    const s = clone();
    breakIt(s);
    if (judgeLedger(s, references).length === 0) {
      console.error('  - 自检失败：判据对「' + name + '」视而不见');
      process.exit(1);
    }
    caught += 1;
  }
  // 没人引用也要红
  const orphan = clone();
  if (judgeLedger(orphan, new Set()).length === 0) {
    console.error('  - 自检失败：一条引用都没有时判据视而不见');
    process.exit(1);
  }
  caught += 1;
  console.log('✓ 自检：' + caught + ' 个变异被抓住，且没改的输入是绿的');
}

function main() {
  const sections = parseLedger(readFileSync(LEDGER, 'utf8'));
  const references = collectReferences();
  if (process.argv.includes('--self-test')) {
    selfTest(sections, references);
    return;
  }
  const problems = judgeLedger(sections, references);
  if (problems.length > 0) {
    for (const problem of problems) console.error('  - ' + problem);
    console.error('✗ 允许的差异台账不过');
    process.exit(1);
  }
  const counts = {};
  for (const keys of sections.values()) {
    const status = keys.get('status');
    counts[status] = (counts[status] === undefined ? 0 : counts[status]) + 1;
  }
  console.log('✓ 允许的差异台账：' + sections.size + ' 条（' +
    Object.entries(counts).map(([k, v]) => k + ' ' + v).join(' / ') + '），每条都有引用、状态与数字自洽');
}

main();
