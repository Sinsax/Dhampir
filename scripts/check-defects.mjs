#!/usr/bin/env node
// dhampir 缺陷台账与阶段路线图的守卫。
//
// 为什么需要它：台账这个东西最危险的失效模式是**腐烂**——
// 状态停在 todo 却早就做完了、证据指向不存在的文件、路线图引用了台账里没有的 id。
// 光靠自觉没用，所以让守卫盯着：台账与路线图**必须互相看得住**。
//
// 判据：
//   * 每条条目有合法 id、合法 status、合法 phase，四个字段齐全且非空；
//   * 根因与证据里的路径**真实存在**（可带 :行号），或指向另一个条目 id；
//   * status=done 的条目**必须**给出至少一个存在的证据文件；
//   * 台账引用的 id 集合 与 路线图引用的 id 集合 **互为子集**（谁也不能漏）；
//   * 计数声明行（html 注释 ledger: D=x A=y）必须与实际条数一致（数字不许写死）；
//   * 空台账**不许通过**。
//
// 用法：
//   node scripts/check-defects.mjs                   检查
//   node scripts/check-defects.mjs --self-test       只跑守卫自检
//   node scripts/check-defects.mjs --help

import { existsSync, readFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

export const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');

/** 台账与路线图的路径（相对仓库根）。 */
export const LEDGER_PATH = 'plan/defects.md';
export const ROADMAP_PATH = 'plan/roadmap.md';

export const STATUSES = ['todo', 'doing', 'done', 'wontfix', 'unmeasurable'];
export const PHASES = ['-', 'T0', 'T1', 'T2', 'T3', 'T4', 'T5', 'T6', 'T7'];
export const FIELD_NAMES = ['症状', '根因', '验收', '证据'];

/** 计数声明行：html 注释 ledger: D=16 A=10 */
const COUNTS_RE = /<!--\s*ledger:\s*([^>]*?)\s*-->/;
/** 条目行：- [D1] status=todo phase=T1 */
const ENTRY_RE = /^- \[([A-Z]\d+)\] status=(\S+) phase=(\S+)\s*$/;
/** 字段行：两个空格缩进 */
const FIELD_RE = /^ {2}(症状|根因|验收|证据):\s*(.*)$/;
/** 路径记号末尾的 :行号 或 :起-止 —— 只为取存在性，校验行号没有意义。 */
const LINE_SUFFIX_RE = /:\d+(?:-\d+)?$/;
/** 只认 D 与 A 开头的 id：路线图里满是 T0/T7 这种阶段名，不能当 id 数。 */
const ID_RE = /\b[DA]\d+\b/g;

/** 把计数声明解析成 { D: 16, A: 10 }。解析不了就返回 null。 */
export function parseCounts(text) {
  const found = COUNTS_RE.exec(text);
  if (!found) return null;
  const counts = {};
  for (const piece of found[1].split(/\s+/)) {
    if (piece === '') continue;
    const eq = piece.indexOf('=');
    if (eq <= 0) return null;
    const name = piece.slice(0, eq);
    const value = Number(piece.slice(eq + 1));
    if (!Number.isInteger(value) || value < 0) return null;
    counts[name] = value;
  }
  return counts;
}

/** 纯解析：把台账文本拆成条目，并把**格式类**问题一次报全。 */
export function parseLedger(text) {
  const problems = [];
  const entries = [];
  let current = null;

  const close = () => {
    if (current === null) return;
    for (const name of FIELD_NAMES) {
      const value = current.fields[name];
      if (value === undefined) {
        problems.push('条目 ' + current.id + ' 缺少字段「' + name + '」——四个字段缺一不可');
      } else if (value === '') {
        problems.push('条目 ' + current.id + ' 的字段「' + name + '」是空的');
      }
    }
    entries.push(current);
    current = null;
  };

  const lines = text.split('\n');
  for (let index = 0; index < lines.length; index += 1) {
    const line = lines[index];
    const entryLine = ENTRY_RE.exec(line);
    if (entryLine) {
      close();
      const id = entryLine[1];
      const status = entryLine[2];
      const phase = entryLine[3];
      if (STATUSES.indexOf(status) < 0) {
        problems.push('条目 ' + id + ' 的 status 非法：「' + status + '」（合法：' + STATUSES.join(' / ') + '）');
      }
      if (PHASES.indexOf(phase) < 0) {
        problems.push('条目 ' + id + ' 的 phase 非法：「' + phase + '」（合法：' + PHASES.join(' / ') + '）');
      }
      if (entries.some((entry) => entry.id === id)) {
        problems.push('id 重复：' + id + '（第 ' + (index + 1) + ' 行）');
      }
      current = { id, status, phase, fields: {}, line: index + 1 };
      continue;
    }
    const fieldLine = FIELD_RE.exec(line);
    if (fieldLine) {
      if (current === null) {
        problems.push('第 ' + (index + 1) + ' 行有一个字段但没有归属的条目');
        continue;
      }
      const name = fieldLine[1];
      if (current.fields[name] !== undefined) {
        problems.push('条目 ' + current.id + ' 的字段「' + name + '」写了不止一次');
      }
      current.fields[name] = fieldLine[2].trim();
      continue;
    }
  }
  close();

  return { entries, declared: parseCounts(text), problems };
}

/** 检查一串记号：每个记号要么是存在的路径，要么是存在的条目 id。 */
function checkTokens(entry, field, raw, ids, exists, problems) {
  const tokens = raw.split(/\s+/).filter((token) => token !== '');
  if (tokens.length === 0) {
    problems.push('条目 ' + entry.id + ' 的「' + field + '」是空的');
    return 0;
  }
  let realPaths = 0;
  for (const token of tokens) {
    if (/^[DA]\d+$/.test(token)) {
      if (ids.indexOf(token) < 0) {
        problems.push('条目 ' + entry.id + ' 的「' + field + '」引用了不存在的 id：' + token);
      }
      continue;
    }
    const path = token.replace(LINE_SUFFIX_RE, '');
    if (!exists(path)) {
      problems.push('条目 ' + entry.id + ' 的「' + field + '」引用了不存在的路径：' + path);
    } else {
      realPaths += 1;
    }
  }
  return realPaths;
}

/**
 * 判定。入参：
 *   ledgerText / roadmapText —— 两份文本；
 *   exists —— 可注入的存在性判定（自检用，默认查仓库）。
 * 返回问题清单（空 = 通过）。
 */
export function judge(input) {
  const problems = [];
  const exists = input.exists || ((path) => existsSync(join(REPO_ROOT, path)));

  const ledger = parseLedger(input.ledgerText);
  for (const problem of ledger.problems) problems.push(problem);

  if (ledger.entries.length === 0) {
    problems.push('台账里一条条目都没有 —— 空的台账不许通过（不存在的结论比错的结论更危险）');
    return problems;
  }

  const ids = ledger.entries.map((entry) => entry.id);

  for (const entry of ledger.entries) {
    const root = entry.fields['根因'] === undefined ? '' : entry.fields['根因'];
    // 根因允许只用 id 指路（缺口类条目就是这样），但不能一个实体都不指。
    checkTokens(entry, '根因', root, ids, exists, problems);
    const evidence = entry.fields['证据'] === undefined ? '' : entry.fields['证据'];
    if (evidence === '-') {
      if (entry.status === 'done') {
        problems.push('条目 ' + entry.id + ' 已经是 done，但「证据」还是 - —— 做完的条目必须留下存在的证据文件');
      }
      continue;
    }
    const evidencePaths = checkTokens(entry, '证据', evidence, ids, exists, problems);
    if (evidencePaths === 0) {
      problems.push('条目 ' + entry.id + ' 的「证据」没有给出任何存在的文件');
    }
  }

  // 计数声明：数字必须由实数上报，不许写死。
  const counts = {};
  for (const id of ids) {
    const prefix = id.slice(0, 1);
    counts[prefix] = (counts[prefix] || 0) + 1;
  }
  if (ledger.declared === null) {
    problems.push('台账里没有计数声明行（html 注释 ledger: D=16 A=10）——写死的数字和没有数字一样不可信');
  } else {
    for (const name of Object.keys(ledger.declared)) {
      if (!(name in counts)) {
        problems.push('计数声明里有 ' + name + '，但台账里没有这种前缀的条目');
      } else if (ledger.declared[name] !== counts[name]) {
        problems.push('计数声明写 ' + name + '=' + ledger.declared[name] + '，实际是 ' + counts[name]);
      }
    }
    for (const name of Object.keys(counts)) {
      if (!(name in ledger.declared)) {
        problems.push('台账里有 ' + counts[name] + ' 条 ' + name + '，但计数声明里没写');
      }
    }
  }

  // 台账与路线图必须互相看得住。
  const roadmapIds = input.roadmapText.match(ID_RE) || [];
  const seen = [];
  for (const id of roadmapIds) {
    if (seen.indexOf(id) >= 0) continue;
    seen.push(id);
    if (ids.indexOf(id) < 0) {
      problems.push('路线图引用了台账里不存在的 id：' + id);
    }
  }
  const missing = ids.filter((id) => seen.indexOf(id) < 0);
  if (missing.length > 0) {
    problems.push('台账里这些 id 没有被路线图引用（不归属任何阶段的也要在明确不做里出现）：' + missing.join(' '));
  }

  return problems;
}

function readOrNull(relative) {
  const path = join(REPO_ROOT, relative);
  return existsSync(path) ? readFileSync(path, 'utf8') : null;
}

/** 合成一份台账条目。 */
function block(id, status, phase, root, evidence) {
  return [
    '- [' + id + '] status=' + status + ' phase=' + phase,
    '  症状: 症状文字',
    '  根因: ' + root,
    '  验收: 验收文字',
    '  证据: ' + evidence,
  ].join('\n');
}

function runSelfTest() {
  let passed = 0;
  const failures = [];
  const virtualFiles = ['src/real.rs', 'proof/run.txt'];
  const exists = (path) => virtualFiles.indexOf(path) >= 0;

  const check = (name, shouldPass, ledgerText, roadmapText) => {
    const problems = judge({ ledgerText, roadmapText, exists });
    if ((problems.length === 0) === shouldPass) {
      passed += 1;
      return;
    }
    failures.push(name + ' -> ' + JSON.stringify(problems));
  };

  const header = '<!-- ledger: D=1 A=1 -->\n';
  const good = header + block('D1', 'todo', 'T0', 'src/real.rs', '-')
    + '\n' + block('A1', 'todo', 'T0', 'D1', '-') + '\n';
  const goodRoadmap = 'T0 段处理 D1 与 A1。\n';

  check('合法台账 -> 通过', true, good, goodRoadmap);
  check('status 非法 -> 必须红', false,
    header + block('D1', 'finished', 'T0', 'src/real.rs', '-') + '\n' + block('A1', 'todo', 'T0', 'D1', '-') + '\n',
    goodRoadmap);
  check('phase 非法 -> 必须红', false,
    header + block('D1', 'todo', 'T9', 'src/real.rs', '-') + '\n' + block('A1', 'todo', 'T0', 'D1', '-') + '\n',
    goodRoadmap);
  check('缺字段 -> 必须红', false,
    header + '- [D1] status=todo phase=T0\n  症状: 有\n  根因: src/real.rs\n  证据: -\n'
      + '\n' + block('A1', 'todo', 'T0', 'D1', '-') + '\n',
    goodRoadmap);
  check('字段为空 -> 必须红', false,
    header + '- [D1] status=todo phase=T0\n  症状: 有\n  根因: src/real.rs\n  验收:   \n  证据: -\n'
      + '\n' + block('A1', 'todo', 'T0', 'D1', '-') + '\n',
    goodRoadmap);
  check('根因路径不存在 -> 必须红', false,
    header + block('D1', 'todo', 'T0', 'src/nope.rs', '-') + '\n' + block('A1', 'todo', 'T0', 'D1', '-') + '\n',
    goodRoadmap);
  check('根因引用不存在的 id -> 必须红', false,
    header + block('D1', 'todo', 'T0', 'src/real.rs', '-') + '\n' + block('A1', 'todo', 'T0', 'D99', '-') + '\n',
    goodRoadmap);
  check('done 却没有证据 -> 必须红', false,
    header + block('D1', 'done', 'T0', 'src/real.rs', '-') + '\n' + block('A1', 'todo', 'T0', 'D1', '-') + '\n',
    goodRoadmap);
  check('done 且有存在的证据 -> 通过', true,
    header + block('D1', 'done', 'T0', 'src/real.rs', 'proof/run.txt') + '\n' + block('A1', 'todo', 'T0', 'D1', '-') + '\n',
    goodRoadmap);
  check('证据指向不存在的文件 -> 必须红', false,
    header + block('D1', 'done', 'T0', 'src/real.rs', 'proof/nope.txt') + '\n' + block('A1', 'todo', 'T0', 'D1', '-') + '\n',
    goodRoadmap);
  check('计数声明与实际不符 -> 必须红', false,
    '<!-- ledger: D=1 A=9 -->\n' + block('D1', 'todo', 'T0', 'src/real.rs', '-')
      + '\n' + block('A1', 'todo', 'T0', 'D1', '-') + '\n',
    goodRoadmap);
  check('没有计数声明 -> 必须红', false,
    block('D1', 'todo', 'T0', 'src/real.rs', '-') + '\n' + block('A1', 'todo', 'T0', 'D1', '-') + '\n',
    goodRoadmap);
  check('id 重复 -> 必须红', false,
    header + block('D1', 'todo', 'T0', 'src/real.rs', '-') + '\n' + block('D1', 'todo', 'T0', 'src/real.rs', '-') + '\n',
    'D1\n');
  check('空台账 -> 必须红', false, header, 'D1\n');
  check('路线图引用台账里没有的 id -> 必须红', false, good, 'T0 段提到 D1 与 A1，还提到 D42。\n');
  check('台账里有的 id 路线图没提到 -> 必须红', false, good, 'T0 段只提到 D1。\n');
  check('字段行没有归属条目 -> 必须红', false,
    header + '  症状: 孤儿\n' + block('D1', 'todo', 'T0', 'src/real.rs', '-') + '\n' + block('A1', 'todo', 'T0', 'D1', '-') + '\n',
    goodRoadmap);
  check('字段写两次 -> 必须红', false,
    header + '- [D1] status=todo phase=T0\n  症状: 一\n  症状: 二\n  根因: src/real.rs\n  验收: 有\n  证据: -\n'
      + '\n' + block('A1', 'todo', 'T0', 'D1', '-') + '\n',
    goodRoadmap);

  // 真跑一遍仓库里的两份文件：自检与真跑走同一段代码，这也是本次的判定。
  const live = judge({
    ledgerText: readOrNull(LEDGER_PATH) || '',
    roadmapText: readOrNull(ROADMAP_PATH) || '',
  });
  if (live.length === 0) passed += 1;
  else failures.push('仓库当前台账不通过 -> ' + JSON.stringify(live.slice(0, 3)));

  if (failures.length > 0) {
    console.error('先修守卫，别信它的结论：');
    for (const failure of failures) console.error('  - ' + failure);
    console.error('台账守卫自检失败（' + failures.length + ' 条）');
    process.exitCode = 2;
    return;
  }
  console.log('OK 台账守卫自检通过（' + passed + ' 条断言）');
}

function usage() {
  console.log('用法：node scripts/check-defects.mjs [--self-test | --help]');
}

function main() {
  const args = process.argv.slice(2);
  if (args.indexOf('--self-test') >= 0) {
    runSelfTest();
    return process.exitCode === undefined ? 0 : process.exitCode;
  }
  if (args.indexOf('--help') >= 0 || args.indexOf('-h') >= 0) {
    usage();
    return 0;
  }
  if (args.length > 0) {
    console.error('  - 不认识的参数：' + args.join(' '));
    usage();
    return 2;
  }

  const ledgerText = readOrNull(LEDGER_PATH);
  if (ledgerText === null) {
    console.error('  - 找不到 ' + LEDGER_PATH);
    console.error('台账守卫无法执行');
    return 2;
  }
  const roadmapText = readOrNull(ROADMAP_PATH);
  if (roadmapText === null) {
    console.error('  - 找不到 ' + ROADMAP_PATH + '（台账必须与路线图互相看得住）');
    console.error('台账守卫无法执行');
    return 2;
  }

  const ledger = parseLedger(ledgerText);
  const problems = judge({ ledgerText, roadmapText });
  if (problems.length > 0) {
    const shown = problems.slice(0, 6);
    for (const problem of shown) console.error('  - ' + problem);
    if (problems.length > shown.length) {
      console.error('  - …另有 ' + (problems.length - shown.length) + ' 处');
    }
    console.error('缺陷台账与路线图不一致');
    return 1;
  }

  console.log('OK 缺陷台账自洽（条目 ' + ledger.entries.length + ' 条，路线图引用齐全，证据路径都存在）');
  return 0;
}

process.exitCode = main();
