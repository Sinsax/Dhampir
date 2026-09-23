#!/usr/bin/env node
// 底座调用面清单：**从代码生成**，再用守卫钉住不许漂。
//
// 为什么需要它：wasm 侧有几十个导出，其中一部分是**内部取证工具**
// （M0/M2 的 corpus/probe），另一部分才是下游会依赖的 API。
// 下游要读几十个函数才知道从哪进 —— 这本身就是桥接效率的敌人；
// 更糟的是**每个导出都像一份长期兼容承诺**，而其中大半并不承诺兼容。
//
// 办法：按模块分类，生成一份清单，并让守卫盯着「清单 == 代码」。
// 手写清单一定会漂，所以它必须是生成的。
//
// 用法：
//   node scripts/api-surface.mjs --write     重新生成 docs/api-surface.md
//   node scripts/api-surface.mjs             检查（不清不写）
//   node scripts/api-surface.mjs --self-test 只跑守卫自检

import { existsSync, mkdirSync, readFileSync, readdirSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

export const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');

/** 模块 -> 分类。**分类是人的判断，所以写在这里；函数清单是从代码扫的。** */
export const MODULE_CLASS = {
  'timeline_host.rs': ['底座 API', '下游会依赖：工程载入/校验/求值/上屏/素材绑定。**这部分才承诺兼容。**'],
  'demux_wasm.rs': ['底座 API', '按帧号定位同步样本（帧精确的前提）。'],
  'cache_wasm.rs': ['宿主内部', '帧缓存记账。宿主自己用，**不是渲染契约的一部分**。'],
  'preview.rs': ['弃用', 'M3 的单片段预览，已被 timeline_host 取代。**新代码不要用。**'],
  'web.rs': ['取证工具', 'M0–M2 的探针与 corpus。**不承诺兼容**，供本仓库验收用。'],
  'corpus.rs': ['取证工具', 'M2 的语料与记录。**不承诺兼容。**'],
};

/** 扫出所有 wasm 导出，按模块分组。 */
export function scanExports(srcDir) {
  const found = new Map();
  if (!existsSync(srcDir)) return found;
  for (const name of readdirSync(srcDir).filter((file) => file.endsWith('.rs')).sort()) {
    const text = readFileSync(join(srcDir, name), 'utf8');
    const names = [];
    for (const line of text.split('\n')) {
      const trimmed = line.trim();
      if (!trimmed.startsWith('pub fn ') && !trimmed.startsWith('pub async fn ')) continue;
      const match = trimmed.match(/^pub (?:async )?fn (dhampir_[A-Za-z0-9_]+)/);
      if (match) names.push(match[1]);
    }
    if (names.length > 0) found.set(name, names.sort());
  }
  return found;
}

/** 生成 markdown。 */
export function renderSurface(exports) {
  const lines = [];
  lines.push('# 底座调用面清单');
  lines.push('');
  lines.push('**这份文件是生成的**（`node scripts/api-surface.mjs --write`）。');
  lines.push('手写清单一定会漂，所以由 `scripts/api-surface.mjs` 生成并用守卫钉住。');
  lines.push('');
  lines.push('## 怎么读');
  lines.push('');
  lines.push('| 分类 | 含义 |');
  lines.push('|---|---|');
  lines.push('| **底座 API** | 下游会依赖。**只有这部分承诺兼容**；形状变化要升版本。 |');
  lines.push('| 宿主内部 | 宿主自己用。可用，但不构成渲染契约。 |');
  lines.push('| 弃用 | 已被取代。新代码不要用。 |');
  lines.push('| 取证工具 | 本仓库验收用。**不承诺兼容**。 |');
  lines.push('');
  const order = ['底座 API', '宿主内部', '取证工具', '弃用'];
  for (const category of order) {
    const modules = [...exports.keys()].filter((name) => (MODULE_CLASS[name] || [])[0] === category);
    if (modules.length === 0) continue;
    lines.push('## ' + category);
    lines.push('');
    for (const name of modules) {
      lines.push('### `' + name + '`');
      lines.push('');
      lines.push((MODULE_CLASS[name] || [])[1] || '');
      lines.push('');
      for (const fn of exports.get(name)) lines.push('- `' + fn + '`');
      lines.push('');
    }
  }
  const unknown = [...exports.keys()].filter((name) => !MODULE_CLASS[name]);
  if (unknown.length > 0) {
    lines.push('## 未分类（守卫会报红）');
    lines.push('');
    for (const name of unknown) lines.push('- `' + name + '`（' + exports.get(name).length + ' 个导出）');
    lines.push('');
  }
  return lines.join('\n');
}

/** 判断：文档与代码是否一致。 */
export function judge(docText, exports) {
  const problems = [];
  for (const [module, names] of exports) {
    if (!MODULE_CLASS[module]) {
      problems.push('模块 ' + module + ' 有 ' + names.length + ' 个导出，但没有分类 —— 请先在 MODULE_CLASS 里给它一个定位');
    }
    for (const name of names) {
      if (!docText.includes('`' + name + '`')) problems.push('导出 ' + name + ' 不在清单里');
    }
  }
  // 反向：清单里写了但代码里没有 —— 多半是删了导出忘了重新生成。
  for (const line of docText.split('\n')) {
    const match = line.match(/^- `(dhampir_[A-Za-z0-9_]+)`$/);
    if (!match) continue;
    const exists = [...exports.values()].some((names) => names.includes(match[1]));
    if (!exists) problems.push('清单里的 ' + match[1] + ' 在代码里找不到（删了导出？重新生成）');
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
  const exports = new Map([['timeline_host.rs', ['dhampir_project_open']]]);
  const good = '- `dhampir_project_open`';
  expect('一致 -> 通过', judge(good, exports), true);
  expect('漏了导出 -> 红', judge('（空）', exports), false);
  expect('清单里有代码里没有的 -> 红', judge(good + '\n- `dhampir_gone`', exports), false);
  expect('模块没分类 -> 红', judge(good, new Map([['unknown.rs', ['dhampir_x']]])), false);
  console.log('OK 调用面清单守卫自检通过（' + passed + ' 条断言）');
}

function main() {
  if (process.argv.includes('--self-test')) { runSelfTest(); return; }

  const srcDir = join(REPO_ROOT, 'crates', 'dhampir-wasm', 'src');
  const exports = scanExports(srcDir);
  if (exports.size === 0) {
    console.error('  - 一个 wasm 导出都没扫到 —— 守卫拒绝在空集上通过');
    process.exitCode = 1;
    return;
  }
  const docPath = join(REPO_ROOT, 'docs', 'api-surface.md');
  const rendered = renderSurface(exports);

  if (process.argv.includes('--write')) {
    mkdirSync(dirname(docPath), { recursive: true });
    writeFileSync(docPath, rendered.endsWith('\n') ? rendered : rendered + '\n');
    console.log('已生成 docs/api-surface.md（' + exports.size + ' 个模块）');
    return;
  }

  if (!existsSync(docPath)) {
    console.error('  - 缺少 docs/api-surface.md（跑 --write 生成）');
    process.exitCode = 1;
    return;
  }
  const problems = judge(readFileSync(docPath, 'utf8'), exports);
  if (readFileSync(docPath, 'utf8') !== (rendered.endsWith('\n') ? rendered : rendered + '\n')) {
    problems.push('清单与代码不同步（跑 --write 重新生成）');
  }
  if (problems.length > 0) {
    for (const problem of problems) console.error('  - ' + problem);
    console.error('调用面清单与代码不一致');
    process.exitCode = 1;
    return;
  }
  let total = 0;
  for (const names of exports.values()) total += names.length;
  console.log('OK 调用面清单与代码一致（' + exports.size + ' 个模块 / ' + total + ' 个导出）');
}

main();
