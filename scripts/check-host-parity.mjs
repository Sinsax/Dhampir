#!/usr/bin/env node
// **两个宿主的形状对表**：DOM 宿主的接口清单 vs wasm 宿主承诺过的导出名。
//
//   node scripts/check-host-parity.mjs
//   node scripts/check-host-parity.mjs --self-test
//
// # 为什么需要它
//
// 「两个宿主同一套接口」这句话在阶段 3 之前**只有注释在保证**。注释不会红。
// 这条判据把它变成可判定的：DOM 宿主的每个接口名都要能对应到 docs/api-surface.md
// 里「**底座 API**」那段**承诺过**的名字上 —— 不是内部名字、不是取证工具、更不是编出来的名字。
//
// 另一半是**不许静默缺失**：wasm 宿主那条调用顺序里的每一步，DOM 宿主要么实现，
// 要么在 HOST_UNIMPLEMENTED 里写明为什么不做（理由不许是占位符）。

import { readFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import { HOST_INTERFACE, HOST_UNIMPLEMENTED } from '../web/dom-host.mjs';

const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const SURFACE = join(REPO_ROOT, 'docs', 'api-surface.md');

/**
 * DOM 接口名 → wasm 导出的对应关系。**这张表是判据的一部分**，不藏在注释里。
 *
 * behavior：exact = 语义一样；explicit-error = 叫它会给明确的拒绝（不静默）；
 * dom-only = 这边有、wasm 那边没有对应物（必须给理由）。
 */
export const HOST_MAP = {
  open: { wasm: ['dhampir_project_open'], behavior: 'exact' },
  resize: { wasm: ['dhampir_project_resize'], behavior: 'exact' },
  sources_for: { wasm: ['dhampir_project_sources_for'], behavior: 'exact' },
  clear_bitmaps: { wasm: ['dhampir_project_clear_bitmaps'], behavior: 'exact' },
  set_bitmap: { wasm: ['dhampir_project_set_bitmap'], behavior: 'explicit-error' },
  text_frame: { wasm: ['dhampir_project_text_frame'], behavior: 'exact' },
  draw: { wasm: ['dhampir_project_draw'], behavior: 'exact' },
  describe: { wasm: ['dhampir_host_api_version', 'dhampir_build_id'], behavior: 'exact' },
};

/** wasm 那条**调用顺序**里的每一步（docs/api.md 的「调用顺序契约」）。 */
export const CALL_ORDER_STEPS = ['open', 'resize', 'sources_for', 'clear_bitmaps', 'set_bitmap', 'text_frame', 'draw'];

/** 解析 api-surface.md：按段落（## 标题）收集导出名。 */
export function parseSurface(text) {
  const sections = new Map();
  let current = '(开头)';
  for (const line of text.split(/\r?\n/)) {
    const heading = line.match(/^## (.+)$/);
    if (heading !== null) {
      current = heading[1].trim();
      if (!sections.has(current)) sections.set(current, new Set());
      continue;
    }
    const item = line.match(/^- \u0060(dhampir_[a-z0-9_]+)\u0060/);
    if (item !== null) {
      if (!sections.has(current)) sections.set(current, new Set());
      sections.get(current).add(item[1]);
    }
  }
  return sections;
}

const PLACEHOLDER = ['TODO', 'todo', '以后再说', '待定', '无', '-', ''];

/** 判据本体。纯函数：喂**故意错的**输入就能验证它会红。 */
export function judgeParity(sections, implemented, unimplemented, hostMap = HOST_MAP, callOrder = CALL_ORDER_STEPS) {
  const problems = [];
  const promised = sections.get('底座 API') === undefined ? new Set() : sections.get('底座 API');
  const everNamed = new Set();
  for (const names of sections.values()) for (const name of names) everNamed.add(name);
  if (promised.size === 0) {
    problems.push('api-surface.md 里没解析到「底座 API」段 —— 解析坏了，不许当通过');
  }
  for (const name of implemented) {
    if (hostMap[name] === undefined) problems.push('接口 ' + name + ' 没有登记对应关系（HOST_MAP 里没有它）');
  }
  for (const [name, entry] of Object.entries(hostMap)) {
    for (const wasm of entry.wasm) {
      if (!promised.has(wasm)) {
        problems.push('接口 ' + name + ' 绑到了 ' + wasm + '，但它不在「底座 API」里' +
          (everNamed.has(wasm) ? '（在别的段：内部/取证/弃用 —— 绑错了）' : '（根本没有这个导出）'));
      }
    }
    if (entry.behavior === 'dom-only' && (entry.why === undefined || PLACEHOLDER.includes(String(entry.why).trim()))) {
      problems.push('接口 ' + name + ' 是 dom-only，但没给出非占位的理由');
    }
  }
  for (const name of implemented) {
    if (unimplemented[name] !== undefined) {
      problems.push('接口 ' + name + ' 同时出现在「实现了」与「明确不做」里 —— 状态不许有歧义');
    }
  }
  for (const step of callOrder) {
    const done = implemented.includes(step);
    const told = unimplemented[step] !== undefined;
    if (!done && !told) {
      problems.push('调用顺序里的 ' + step + ' 既没实现、也没写明为什么不做 —— 静默缺失');
      continue;
    }
    if (told && PLACEHOLDER.includes(String(unimplemented[step]).trim())) {
      problems.push(step + ' 的「不做」理由是占位符');
    }
  }
  for (const name of Object.keys(unimplemented)) {
    const known = hostMap[name] !== undefined || promised.has('dhampir_project_' + name);
    if (!known) problems.push('明确不做的 ' + name + ' 对不上任何已知导出名（是编出来的？）');
    // 理由必须**存在且不是占位符** —— 这条对**每一条**都生效，不只是调用顺序里那几步。
    if (PLACEHOLDER.includes(String(unimplemented[name]).trim())) {
      problems.push('明确不做的 ' + name + ' 理由是占位符：' + JSON.stringify(unimplemented[name]));
    }
  }
  return problems;
}

function selfTest(sections) {
  const mutations = [
    ['某一步没登记对应关系', (impl) => impl.push('mystery_step')],
    ['绑到根本不存在的导出', (impl, un, map) => { map.resize = { wasm: ['dhampir_nonexistent_thing'], behavior: 'exact' }; }],
    ['同一步既实现又声明不做', (impl, un) => { un.draw = '不做（有理由）'; }],
    ['调用顺序里的步没表态', (impl, un, map, order) => order.push('mystery_step')],
    ['理由写成占位符', (impl, un) => { un.preroll = 'TODO'; }],
    ['凭空发明的导出名', (impl, un) => { un.invented_thing = '一个很长的、非占位的理由'; }],
  ];
  let caught = 0;
  for (const [name, breakIt] of mutations) {
    const impl = [...HOST_INTERFACE];
    const un = { ...HOST_UNIMPLEMENTED };
    const map = JSON.parse(JSON.stringify(HOST_MAP));
    const order = [...CALL_ORDER_STEPS];
    breakIt(impl, un, map, order);
    const found = judgeParity(sections, impl, un, map, order);
    if (found.length === 0) {
      console.error('  - 自检失败：判据对「' + name + '」视而不见');
      process.exit(1);
    }
    caught += 1;
  }
  // 反向：没改的输入必须是绿的（否则判据本身太吵，红的没人看）
  const clean = judgeParity(sections, HOST_INTERFACE, HOST_UNIMPLEMENTED);
  if (clean.length !== 0) {
    console.error('  - 自检失败：没改的输入本来就红：' + clean[0]);
    process.exit(1);
  }
  console.log('✓ 自检：' + caught + ' 个变异被抓住，且未改动的输入是绿的');
}

function main() {
  const sections = parseSurface(readFileSync(SURFACE, 'utf8'));
  if (process.argv.includes('--self-test')) {
    selfTest(sections);
    return;
  }
  const problems = judgeParity(sections, HOST_INTERFACE, HOST_UNIMPLEMENTED);
  if (problems.length > 0) {
    for (const problem of problems) console.error('  - ' + problem);
    console.error('✗ 两个宿主的形状对不上');
    process.exit(1);
  }
  const promised = sections.get('底座 API');
  console.log('✓ 宿主形状对表：DOM 宿主 ' + HOST_INTERFACE.length + ' 个接口全部对应到「底座 API」的承诺名（' +
    promised.size + ' 个）；调用顺序 ' + CALL_ORDER_STEPS.length + ' 步全部表态；明确不做 ' +
    Object.keys(HOST_UNIMPLEMENTED).length + ' 项都写了非占位理由');
}

main();
