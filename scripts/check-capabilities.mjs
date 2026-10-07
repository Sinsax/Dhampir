#!/usr/bin/env node
// 能力登记表的判据。
//
//   node scripts/check-capabilities.mjs
//   node scripts/check-capabilities.mjs --self-test
//
// # 它管什么
//
// web/capabilities.json 是**真值**：每条能力一个状态。它自己不会红，所以要有这条判据：
//
//   R1 每条都有 id/label/status/stage/note，status 与 stage 都在图例里；
//   R2 status=supported **必须**给出底座原语与判据证据（说支持就得有依据）；
//   R3 证据里的每个路径**必须真的存在**（证据不能指向不存在的文件）；
//   R4 DOM 宿主源码里出现的受管 CSS 能力，**必须登记**（用了没登记 = 设计债不可见）；
//   R5 理由/说明不许是占位符；
//   R6 id 唯一。
//
// # 为什么只扫 web/dom-host.mjs
//
// 页面外壳自己的 CSS（grid / 字体 / 配色）与**画面**无关，登记它没有意义。
// 会改变画面的是渲染模块往图层节点上写的那些属性 —— 扫它就够了。

import { existsSync, readFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const REGISTRY = join(REPO_ROOT, 'web', 'capabilities.json');
const DOM_HOST = join(REPO_ROOT, 'web', 'dom-host.mjs');
// 会往画面写 CSS 的**两个**文件：宿主与它的映射层。
const DOM_SOURCES = [DOM_HOST, join(REPO_ROOT, 'web', 'dom-css.mjs')];

/**
 * 受管的 CSS 能力词表。**它独立于登记表**（否则就成了循环论证）：
 * 词表是写死的，扫到的每个都要在登记表里出现。
 */
export const SCAN_VOCAB = [
  'transform', 'opacity', 'filter', 'backdropFilter', 'clipPath', 'maskImage',
  'mixBlendMode', 'boxShadow', 'borderRadius', 'textShadow',
];

const PLACEHOLDER = ['TODO', 'todo', '待定', '以后再说', '无', '-', ''];

/** 判据本体。纯函数：exists 与 domSource 都由调用方给，便于喂故意错的输入。 */
const CRITERIA = join(REPO_ROOT, 'plan', 'web-animation-criteria.md');

/** 设计节的标题集合（`## D12 …` → `D12`）。 */
function readDesignHeadings() {
  const headings = new Set();
  for (const line of readFileSync(CRITERIA, 'utf8').split(/\r?\n/)) {
    const match = line.match(/^##\s+(D[0-9]+)\b/);
    if (match !== null) headings.add(match[1]);
  }
  return headings;
}


/// 扫哪些文件找用例名。**显式列举**而不是全仓递归：
/// 判据要能一眼看懂它扫了什么，也要够快（这条规矩每次跑都会读这些文件）。
const RUST_SOURCES = ['crates/dhampir-timeline/src/easing.rs', 'crates/dhampir-timeline/src/curve.rs', 'crates/dhampir-timeline/src/layer.rs', 'crates/dhampir-core/src/render/blur.rs', 'crates/dhampir-core/src/render/timeline.rs', 'crates/dhampir-core/src/render/compose.rs', 'crates/dhampir-core/src/render/polygon.rs', 'crates/dhampir-worker/tests/timeline.rs', 'crates/dhampir-worker/tests/compose.rs'];

/** 仓库里的 Rust 源文件是否声明了这个用例名（`fn 名字`）。 */
function rustSourceHasTest(name, readText) {
  for (const file of RUST_SOURCES) {
    const text = readText(file);
    if (text !== null && text.includes('fn ' + name)) return true;
  }
  return false;
}

/** 实测索引与 GPU 用例必须**双向一致**（新加/删掉用例而没同步，就该红）。 */
export function judgeMeasurementIndex(readText, indexFile = 'plan/web-engine-measurements.md') {
  const problems = [];
  const index = readText(indexFile);
  if (index === null) {
    return [indexFile + ' 读不到 —— 实测索引不见了']; // 索引没了，先报这个
  }
  const inIndex = new Set([...index.matchAll(/^\| `([^`]+)` \|/gm)].map((match) => match[1]));
  const inCode = new Set();
  for (const file of ['timeline', 'compose']) {
    const text = readText('crates/dhampir-worker/tests/' + file + '.rs');
    if (text === null) continue;
    const lines = text.split('\n');
    for (let i = 0; i < lines.length; i += 1) {
      if (!/^#\[ignore/.test(lines[i].trim())) continue;
      for (let j = i; j < Math.min(i + 6, lines.length); j += 1) {
        const match = lines[j].match(/^fn ([A-Za-z0-9_\u4e00-\u9fa5]+)\(/);
        if (match) { inCode.add(match[1]); break; }
      }
    }
  }
  for (const name of inCode) {
    if (!inIndex.has(name)) problems.push('GPU 用例 `' + name + '` 不在实测索引里 —— 新加的用例要写进 ' + indexFile);
  }
  for (const name of inIndex) {
    if (!inCode.has(name)) problems.push('实测索引里的 `' + name + '` 在代码里找不到 —— 用例被改名或删了，索引要跟着改');
  }
  return problems;
}

export function judgeCapabilities(
  registry,
  domSource,
  exists = (path) => existsSync(join(REPO_ROOT, path)),
  designHeadings = readDesignHeadings(),
  readText = (path) => {
    try {
      return readFileSync(join(REPO_ROOT, path), 'utf8');
    } catch {
      return null;
    }
  },
) {
  const problems = [];
  const list = registry.capabilities === undefined ? [] : registry.capabilities;
  const statuses = Object.keys(registry.status_legend === undefined ? {} : registry.status_legend);
  const stages = Object.keys(registry.stage_legend === undefined ? {} : registry.stage_legend);
  if (list.length === 0) problems.push('登记表是空的 —— 空文件集不算通过');
  if (statuses.length === 0 || stages.length === 0) problems.push('图例缺了（status_legend / stage_legend）');

  const ids = new Set();
  for (const item of list) {
    const at = item.id === undefined ? '(没有 id 的一条)' : item.id;
    if (item.id === undefined || item.id === '') problems.push('有一条没有 id');
    else if (ids.has(item.id)) problems.push('id 重复：' + item.id);
    else ids.add(item.id);
    for (const field of ['label', 'status', 'stage', 'note']) {
      if (item[field] === undefined || String(item[field]).trim() === '') {
        problems.push(at + ' 缺字段 ' + field);
      }
    }
    if (!statuses.includes(item.status)) problems.push(at + ' 的 status 不在图例里：' + String(item.status));
    // **`supported` 必须有一条能跑的判据** —— 光有文档不算。
    //
    // 为什么要这条：能力登记表最大的风险不是"漏登记"，而是"登记成 supported 却没人验过" ——
    // 那种条目读起来最像结论，也最容易在下一次改动里静默过期（本会话已经吃过两次）。
    if (item.status === 'supported') {
      const paths = Array.isArray(item.evidence_paths) ? item.evidence_paths : [];
      const judged = paths.some((path) => {
        if (/scripts\/check-[a-z0-9-]+\.mjs$/.test(path)) return true;
        if (/^crates\/.*\/tests\/.*\.rs$/.test(path)) return true;
        // Rust 源文件里的单测也算判据（本仓大量判据就写在源文件的 `#[cfg(test)]` 里）。
        const text = readText(path);
        return text !== null && /#\[cfg\(test\)\]|#\[test\]/.test(text);
      });
      if (!judged) {
        problems.push(
          at + ' 是 supported，但 evidence_paths 里没有一条**能跑的判据**' +
            '（check-*.mjs / tests/*.rs / 含 #[test] 的文件）—— 光有文档不算',
        );
      }
    }
    if (!stages.includes(item.stage)) problems.push(at + ' 的 stage 不在图例里：' + String(item.stage));
    // 要新原语的，必须写明**属于哪一档基建** —— 「要新原语」太粗，粗到没法排期。
    const needs = Object.keys(registry.needs_legend === undefined ? {} : registry.needs_legend);
    if (item.status === 'needs-primitive') {
      if (!needs.includes(item.needs)) {
        problems.push(at + ' 是 needs-primitive，必须写明档位（' + needs.join(' / ') + '），得到 ' + JSON.stringify(item.needs));
      }
    } else if (item.needs !== undefined) {
      problems.push(at + ' 不是 needs-primitive，却有 needs 字段 —— 状态不许有歧义');
    }
    // **「明确不做」必须写明理由**（非占位）。
    //
    // 为什么这条要紧：本轮之前 `explicitly-not-doing` 只能靠人记得删 —— 第 39 轮就漏了一条
    // （`easing.linear-stops` 还写着"不做"，而那件事已经做完了，登记表在说假话）。
    // 写明理由至少让每条拒绝**有据可查**：下一次读表的人知道当初为什么不做的。
    if (item.status === 'explicitly-not-doing') {
      const why = String(item.why_not === undefined ? '' : item.why_not).trim();
      if (why.length < 20 || ['TODO', '待定', '以后再说', '无', '-'].includes(why)) {
        problems.push(at + ' 是 explicitly-not-doing，必须写明拒绝理由（why_not，非占位，≥20 字）');
      }
    } else if (item.why_not !== undefined) {
      problems.push(at + ' 不是 explicitly-not-doing，却有 why_not 字段 —— 状态不许有歧义');
    }

    // **`partial` 必须写明「差在哪」**（`gap`，非占位）。
    //
    // 三种状态各自要"自证"：`supported` 拿得出能跑的判据、`explicitly-not-doing` 写得出理由、
    // `partial` 说得出**具体差在哪** —— 否则 `partial` 就成了"既不说行也不说不行"的模糊地带，
    // 而本会话第 39/41 轮两次抓到的过期条目，都是栽在这个模糊地带里。
    if (item.status === 'partial') {
      const gap = String(item.gap === undefined ? '' : item.gap).trim();
      if (gap.length < 20 || ['TODO', '待定', '以后再说', '无', '-'].includes(gap)) {
        problems.push(at + ' 是 partial，必须写明「差在哪」（gap，非占位，≥20 字）');
      }
    } else if (item.gap !== undefined) {
      problems.push(at + ' 不是 partial，却有 gap 字段 —— 状态不许有歧义');
    }

    // **`supported` 的 evidence 必须点名一条「真的存在」的判据。**
    //
    // 这条比"路径存在"狠一档：路径存在只说明文件在，而 evidence 里常常点的是
    // **用例名**（「…」）或某个 check 脚本 —— 用例被改名/删掉之后，那句话就成了
    // 无法复核的装饰（本会话吃过多次：陈旧断言、过期条目）。所以这里真的去仓库里找它。
    if (item.status === 'supported') {
      const evidence = String(item.evidence === undefined ? '' : item.evidence);
      const testNames = [...evidence.matchAll(/「([^」]+)」/g)].map((match) => match[1]);
      const scripts = [...evidence.matchAll(/check-[a-z0-9-]+\.mjs/g)].map((match) => match[0]);
      let judged = false;
      for (const name of testNames) {
        if (rustSourceHasTest(name, readText)) {
          judged = true;
          break;
        }
      }
      if (!judged) {
        for (const script of scripts) {
          if (exists(join('scripts', script))) {
            judged = true;
            break;
          }
        }
      }
      if (!judged && testNames.length === 0 && scripts.length === 0) {
        problems.push(at + ' 是 supported，evidence 里既没点名用例（「…」）也没点名 check 脚本');
      } else if (!judged) {
        problems.push(
          at + ' 是 supported，但它点名的那条判据在仓库里**找不到**（用例被改名或删掉了？）：' +
            [...testNames, ...scripts].join(' / '),
        );
      }
    }

    // **实测台账不许变成孤儿文件**（第 69 轮加）。
    //
    // 数字都在 `plan/web-engine-measurements.md` 里，而读登记表的人得能找到它 ——
    // 与差异台账那条"每个 section 都要在别处被引用"同一个精神：没人读的台账迟早烂。
    if (item.status === 'supported' && /实测|GPU 用例|手算|逐值|机器口径/.test(String(item.evidence === undefined ? '' : item.evidence)) &&
        !String(item.evidence === undefined ? '' : item.evidence).includes('web-engine-measurements')) {
      problems.push(at + ' 的 evidence 讲了实测但没指向 `plan/web-engine-measurements.md` —— 数在那里，读表的人得找得到');
    }

    // 「要新原语」不许只是一句话欠着：必须指向 criteria 里**真的存在**的设计节（D12 / D13 …）。
    if (item.status === 'needs-primitive') {
      const design = String(item.design === undefined ? '' : item.design);
      if (!/^D[0-9]+$/.test(design)) {
        problems.push(at + ' 是 needs-primitive，必须写明设计节（design: D<n>）—— 「要新原语」不许只是一句话欠着');
      } else if (!designHeadings.has(design)) {
        problems.push(at + ' 指向的设计节 ' + design + ' 在 plan/web-animation-criteria.md 里不存在');
      }
    }
    const note = String(item.note === undefined ? '' : item.note).trim();
    if (PLACEHOLDER.includes(note)) problems.push(at + ' 的说明是占位符');
    if (item.status === 'supported') {
      if (String(item.dhampir === undefined ? '' : item.dhampir).trim() === '') {
        problems.push(at + ' 声称 supported，却没写底座原语是什么');
      }
      const paths = item.evidence_paths === undefined ? [] : item.evidence_paths;
      if (paths.length === 0) problems.push(at + ' 声称 supported，却没给判据证据（evidence_paths 为空）');
      if (String(item.evidence === undefined ? '' : item.evidence).trim() === '') {
        problems.push(at + ' 声称 supported，却没写证据是什么');
      }
    }
    for (const path of item.evidence_paths === undefined ? [] : item.evidence_paths) {
      if (!exists(path)) problems.push(at + ' 的证据路径不存在：' + path);
    }
  }

  // R4：受管词表里出现在 DOM 宿主源码里的，必须登记。
  const tokens = registry.dom_css_tokens === undefined ? {} : registry.dom_css_tokens;
  for (const [token, value] of Object.entries(tokens)) {
    // 一个 token 可以指向**多条**能力（`filter` 就是这类：它同时对应对比度/饱和度/模糊）。
    for (const id of Array.isArray(value) ? value : [value]) {
      if (!ids.has(id)) problems.push('dom_css_tokens 里 ' + token + ' 指向了不存在的 id：' + id);
    }
  }
  const source = String(domSource);
  for (const token of SCAN_VOCAB) {
    if (!source.includes(token)) continue;
    if (tokens[token] === undefined) {
      problems.push('DOM 宿主用了 ' + token + '，但登记表里没有它 —— 用了没登记就是看不见的债');
    }
  }
  return problems;
}

function selfTest(registry, domSource) {
  const good = judgeCapabilities(registry, domSource);
  if (good.length !== 0) {
    console.error('  - 自检失败：没改的输入本来就红：' + good[0]);
    process.exit(1);
  }
  const clone = () => JSON.parse(JSON.stringify(registry));
  const mutations = [
    ['status 不在图例里', (r) => { r.capabilities[0].status = 'maybe'; }],
    ['supported 却没给证据', (r) => { delete r.capabilities[0].evidence_paths; }],
    ['证据路径不存在', (r) => { r.capabilities[0].evidence_paths = ['nope/not-here.md']; }],
    ['dom_css_tokens 指向不存在的 id', (r) => { r.dom_css_tokens.transform = 'missing-id'; }],
    ['说明是占位符', (r) => { r.capabilities[1].note = 'TODO'; }],
    ['id 重复', (r) => { r.capabilities[1].id = r.capabilities[0].id; }],
    ['缺字段', (r) => { delete r.capabilities[2].label; }],
    ['要新原语却没写档位', (r) => { delete r.capabilities.find((c) => c.status === 'needs-primitive').needs; }],
    ['不是要新原语却写了档位', (r) => { r.capabilities[0].needs = 'geometry'; }],
    ['要新原语却没写设计节', (r) => { delete r.capabilities.find((c) => c.status === 'needs-primitive').design; }],
    ['指向不存在的设计节', (r) => { r.capabilities.find((c) => c.status === 'needs-primitive').design = 'D99'; }],
    ['partial 却没写差在哪', (r) => { delete r.capabilities.find((c) => c.status === 'partial').gap; }],
    ['partial 却写了占位差距', (r) => { r.capabilities.find((c) => c.status === 'partial').gap = '待定'; }],
    ['明确不做却没写理由', (r) => { delete r.capabilities.find((c) => c.status === 'explicitly-not-doing').why_not; }],
    ['明确不做却写了占位理由', (r) => { r.capabilities.find((c) => c.status === 'explicitly-not-doing').why_not = '待定'; }],
    ['讲了实测却没指向实测台账', (r) => {
      const item = r.capabilities.find((c) => c.status === 'supported' && /实测|GPU 用例|手算/.test(String(c.evidence)));
      item.evidence = 'GPU 用例「某某」实测过（但没写指向哪个文件）';
    }],
    ['supported 点名的用例不存在', (r) => {
      const item = r.capabilities.find((c) => c.status === 'supported');
      item.evidence = '「这条用例根本不存在_没有这个测试」';
    }],
    ['supported 的判据路径全是文档', (r) => {
      const item = r.capabilities.find((c) => c.status === 'supported');
      item.evidence_paths = ['plan/web-animation-criteria.md'];
    }],
  ];
  let caught = 0;
  for (const [name, breakIt] of mutations) {
    const r = clone();
    breakIt(r);
    if (judgeCapabilities(r, domSource).length === 0) {
      console.error('  - 自检失败：判据对「' + name + '」视而不见');
      process.exit(1);
    }
    caught += 1;
  }
  // 用了没登记：把 DOM 宿主源码里塞一个没登记的能力（用替身源码，不碰真文件）。
  //
  // **别写死 token 名** —— 这条自检空转过两次：先写死 `filter`（第 9 轮它登记了），
  // 又写死 `clipPath`（第 16 轮它登记了）。两次都是自检自己抓到的，但两次都白跑一轮。
  // 现在改成**运行时挑一个当前没登记的受管 token**；挑不到就判红 —— 不允许悄悄空转。
  const registered = registry.dom_css_tokens === undefined ? {} : registry.dom_css_tokens;
  const free = SCAN_VOCAB.find((token) => !Object.prototype.hasOwnProperty.call(registered, token));
  if (free === undefined) {
    console.error('  - 自检失败：受管 token 全部已登记，这条自检没有可用的替身 —— 要么扩 SCAN_VOCAB，要么换一种自检方式');
    process.exit(1);
  }
  const dirty = domSource + '\nnode.style.' + free + ' = "x";\n';
  const found = judgeCapabilities(clone(), dirty);
  if (found.length === 0) {
    console.error('  - 自检失败：DOM 宿主用了没登记的能力，判据视而不见');
    process.exit(1);
  }
  caught += 1;
  console.log('✓ 自检：' + caught + ' 个变异被抓住，且未改动的输入是绿的');
}

function main() {
  const registry = JSON.parse(readFileSync(REGISTRY, 'utf8'));
  const domSource = DOM_SOURCES.map((path) => readFileSync(path, 'utf8')).join('\n');
  if (process.argv.includes('--self-test')) {
    selfTest(registry, domSource);
    return;
  }
  // 实测索引也要与 GPU 用例双向一致（第 60 轮加的）。
  const readTextForIndex = (path) => {
    try {
      return readFileSync(join(REPO_ROOT, path), 'utf8');
    } catch {
      return null;
    }
  };
  const problems = judgeCapabilities(registry, domSource);
  const indexProblems = judgeMeasurementIndex(readTextForIndex);
  for (const problem of indexProblems) {
    problems.push(problem);
  }
  if (indexProblems.length === 0) {
    const indexText = readTextForIndex('plan/web-engine-measurements.md');
    const rows = [...String(indexText === null ? '' : indexText).matchAll(/^\| `([^`]+)` \|/gm)].length;
    console.log('✓ 实测索引：' + rows + ' 条 GPU 用例，与代码双向一致');
  }
  if (problems.length > 0) {
    for (const problem of problems) console.error('  - ' + problem);
    console.error('✗ 能力登记表不过');
    process.exit(1);
  }
  const counts = {};
  for (const item of registry.capabilities) counts[item.status] = (counts[item.status] === undefined ? 0 : counts[item.status]) + 1;
  console.log('✓ 能力登记表：' + registry.capabilities.length + ' 条（' +
    Object.entries(counts).map(([k, v]) => k + ' ' + v).join(' / ') + '）；受管能力 ' +
    Object.keys(registry.dom_css_tokens).length + ' 个全部登记且 id 有效');
}

main();
