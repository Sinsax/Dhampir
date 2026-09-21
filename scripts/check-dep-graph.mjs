#!/usr/bin/env node
// 依赖方向守卫：架构是**单向无环**的，而且这条不许靠自觉。
//
// ---------------------------------------------------------------------------
// 为什么要守卫
//
// 依赖方向一旦被破坏，破坏它的人**不会**当场看到失败：
//
//   - `dhampir-wasm` 依赖 `dhampir-worker` 是能编译过的，直到你想把 wasm 包
//     发给浏览器，才发现里面塞了一份 native 渲染器的符号。
//   - `dhampir-core` 依赖 `dhampir-media` 也能编译过，但那意味着"渲染图知道 MP4
//     长什么样"，M2 之后每一次改解码契约都会波及最贵的那份资产。
//   - 反向的 `dhampir-timeline → anything` 会让时间轴不再能独立测试，
//     而"帧号换算可以脱离 GPU 单测"是 M0 之后所有里程碑的底座。
//
// 所以这里检查三件事：**允许的边**、**纯层的隔离**、**无环**。
//
// ---------------------------------------------------------------------------
// 为什么是 Node 而不是脚本里的 shell
//
// 与 check-core-purity 同一个理由：本机只有 PowerShell 5.1，它按 ANSI 代码页
// 解析无 BOM 的 .ps1，带中文的脚本直接语法错误。Node 一律按 UTF-8 读源码。
//
// 用法：
//   node scripts/check-dep-graph.mjs
//   node scripts/check-dep-graph.mjs --self-test

import { existsSync, readdirSync, readFileSync } from 'node:fs';
import { dirname, join, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');

// ---------------------------------------------------------------------------
// 期望的架构：一份表。
//
// `dhampir` 是允许的内部依赖；`forbidden` 是不许出现的**任何**依赖名
// （不论是不是内部的）。表写在这里而不是散在注释里——将来有人要加一条边，
// 必须来改这张表，这正好是"我确认过方向"的意思。
// ---------------------------------------------------------------------------
const EXPECTED = new Map([
  [
    'dhampir-timeline',
    {
      allowed: [],
      // 纯数据层。沾上任何一个平台/GPU crate 就意味着它不能再被"脱离显卡"地测。
      forbidden: PLATFORM_CRATES(),
    },
  ],
  [
    'dhampir-media',
    {
      allowed: ['dhampir-timeline'],
      forbidden: PLATFORM_CRATES(),
    },
  ],
  [
    'dhampir-core',
    {
      // 不依赖 dhampir-media：渲染图不该知道 MP4 长什么样。
      allowed: ['dhampir-timeline'],
      forbidden: [],
    },
  ],
  [
    'dhampir-wasm',
    {
      allowed: ['dhampir-core', 'dhampir-media'],
      // 宿主之间互不依赖——它们是同一份 core 的两个出口，不是彼此的一部分。
      forbidden: ['dhampir-worker'],
    },
  ],
  [
    'dhampir-worker',
    {
      allowed: ['dhampir-core', 'dhampir-media'],
      forbidden: ['dhampir-wasm'],
    },
  ],
]);

/** 平台相关的 crate：出现在纯数据/纯契约层里就是架构走样。 */
function PLATFORM_CRATES() {
  return [
    'wgpu', 'web-sys', 'js-sys', 'wasm-bindgen', 'wasm-bindgen-futures',
    'raw-window-handle', 'windows', 'winapi', 'ash', 'metal', 'vulkano',
    'pollster', 'wasm-bindgen-test',
  ];
}

// ---------------------------------------------------------------------------
// 极简 Cargo.toml 解析
//
// 不求完备——只求"不会漏掉一条真实存在的依赖"。漏掉是这里唯一不可接受的错，
// 所以对拿不准的写法一律**算作依赖**（宁可多报，也别放过）。
// ---------------------------------------------------------------------------

/** 去掉一行里的注释。`#` 出现在字符串里时不算注释。 */
function stripComment(line) {
  let out = '';
  let inString = false;
  for (let i = 0; i < line.length; i += 1) {
    const c = line[i];
    if (c === '"' && line[i - 1] !== '\\') inString = !inString;
    if (c === '#' && !inString) break;
    out += c;
  }
  return out;
}

/** 按 `.` 切分，但引号里的 `.` 不算分隔符（`target.'cfg(x)'.dependencies`）。 */
function splitSectionPath(header) {
  const parts = [];
  let current = '';
  let inString = false;
  for (const c of header) {
    if (c === '"' || c === "'") inString = !inString;
    if (c === '.' && !inString) {
      parts.push(current);
      current = '';
    } else {
      current += c;
    }
  }
  parts.push(current);
  return parts.map((p) => p.trim().replace(/^['"]|['"]$/g, '')).filter((p) => p.length > 0);
}

const DEP_SEGMENTS = new Set(['dependencies', 'dev-dependencies', 'build-dependencies']);

/**
 * 解析一份 Cargo.toml，返回 `[{ name, kind, section }]`。
 *
 * `kind` 是 `normal` / `dev` / `build`——分开记是因为"只在测试里依赖它"
 * 仍然是依赖：dev-dependency 一样会把目标 crate 拉进构建图。
 */
export function parseManifest(text) {
  const deps = [];
  const seen = new Set();
  let section = { name: '', kind: 'normal', inlineName: null };

  const push = (name, kind, sectionName) => {
    const clean = name.trim().replace(/^['"]|['"]$/g, '');
    if (clean.length === 0) return;
    const key = `${kind}:${clean}`;
    if (seen.has(key)) return;
    seen.add(key);
    deps.push({ name: clean, kind, section: sectionName });
  };

  for (const rawLine of text.split('\n')) {
    const line = stripComment(rawLine).trim();
    if (line.length === 0) continue;

    if (line.startsWith('[')) {
      const end = line.lastIndexOf(']');
      const header = end > 0 ? line.slice(1, end) : line.slice(1);
      const parts = splitSectionPath(header);
      const index = parts.findIndex((p) => DEP_SEGMENTS.has(p));
      if (index < 0) {
        // `[features]` / `[package]` / `[[bin]]` … 不是依赖表。
        section = { name: header, kind: 'normal', inlineName: null };
        continue;
      }
      const kind = parts[index] === 'dev-dependencies' ? 'dev' : parts[index] === 'build-dependencies' ? 'build' : 'normal';
      const inlineName = parts.length > index + 1 ? parts.slice(index + 1).join('.') : null;
      section = { name: header, kind, inlineName };
      // `[dependencies.serde]` 这种子表写法：依赖名在表名里，不在行里。
      if (inlineName) push(inlineName, kind, header);
      continue;
    }

    if (section.inlineName) continue; // 子表内部的键（version / features / …）
    if (!DEP_SEGMENTS.has(splitSectionPath(section.name).find((p) => DEP_SEGMENTS.has(p)) ?? '')) {
      continue;
    }

    const match = /^("?)([A-Za-z0-9_.-]+)\1\s*=/.exec(line);
    if (match) push(match[2], section.kind, section.name);
  }

  return deps;
}

/** 从 `[workspace] members = [...]` 里取出成员路径。 */
export function parseWorkspaceMembers(text) {
  const start = text.indexOf('members');
  if (start < 0) return [];
  const open = text.indexOf('[', start);
  const close = text.indexOf(']', open);
  if (open < 0 || close < 0) return [];
  const body = text.slice(open + 1, close);
  return [...body.matchAll(/"([^"]+)"/g)].map((m) => m[1].split('\\').join('/'));
}

// ---------------------------------------------------------------------------
// 检查
// ---------------------------------------------------------------------------

/**
 * `graph` 是 `Map<crateName, { deps: [{name, kind}], manifestPath }>`。
 * 返回问题清单（空数组 = 通过）。
 */
export function checkGraph(graph) {
  const problems = [];

  for (const [crate, entry] of graph) {
    const expected = EXPECTED.get(crate);
    if (!expected) {
      problems.push(
        `${crate}: 这是一张新加入 workspace 的 crate，守卫的期望表里没有它。` +
          `请到 scripts/check-dep-graph.mjs 的 EXPECTED 里写下它允许依赖谁——` +
          `这条就是"我确认过方向"的签名。`,
      );
      continue;
    }

    for (const dep of entry.deps) {
      const label = dep.kind === 'normal' ? '' : ` [${dep.kind}]`;
      if (dep.name.startsWith('dhampir-') && !expected.allowed.includes(dep.name)) {
        problems.push(
          `${crate} → ${dep.name}${label}：不在允许的内部依赖表里（允许：` +
            `${expected.allowed.length > 0 ? expected.allowed.join(', ') : '无'}）。`,
        );
      }
      if (expected.forbidden.includes(dep.name)) {
        problems.push(
          `${crate} → ${dep.name}${label}：这一层不许出现平台相关的 crate` +
            `（${crate} 必须能在没有显卡、没有浏览器的环境里被单独测试）。`,
        );
      }
    }
  }

  // 内部边上的环。表本身是无环的，但如果有人新增 crate 又随手连边，
  // 环就是最容易被忽略的那种破坏——它不会让任何一个 crate 编译失败。
  const internal = new Map(
    [...graph].map(([crate, entry]) => [crate, entry.deps.map((d) => d.name).filter((n) => n.startsWith('dhampir-'))]),
  );
  const state = new Map(); // 0 = 正在访问, 1 = 已访问完
  const stack = [];
  const visit = (node) => {
    if (state.get(node) === 1) return;
    if (state.get(node) === 0) {
      const cycle = [...stack.slice(stack.indexOf(node)), node].join(' → ');
      problems.push(`依赖成环：${cycle}。内部依赖必须单向——两台宿主共享一个 core，不是互相嵌套。`);
      return;
    }
    state.set(node, 0);
    stack.push(node);
    for (const next of internal.get(node) ?? []) visit(next);
    stack.pop();
    state.set(node, 1);
  };
  for (const node of internal.keys()) visit(node);

  return problems;
}

// ---------------------------------------------------------------------------
// 自检
// ---------------------------------------------------------------------------

const PARSER_SELF_TESTS = [
  {
    name: '普通依赖表',
    text: '[dependencies]\nfoo = "1"\nbar = { version = "2", features = ["x"] }\n',
    expect: ['foo', 'bar'],
  },
  {
    name: '注释里的赋值不算依赖',
    text: '[dependencies]\n# foo = "1"\nbar = "2" # baz = "3"\n',
    expect: ['bar'],
  },
  {
    name: '子表写法',
    text: '[dependencies.serde]\nversion = "1"\nfeatures = ["derive"]\n',
    expect: ['serde'],
  },
  {
    name: 'target 专属依赖',
    text: "[target.'cfg(target_arch = \"wasm32\")'.dependencies]\nweb-sys = \"0.3\"\n",
    expect: ['web-sys'],
  },
  {
    name: 'target 专属的子表写法',
    text: "[target.'cfg(unix)'.dependencies.libc]\nversion = \"0.2\"\n",
    expect: ['libc'],
  },
  {
    name: 'dev-dependencies 也计入',
    text: '[dev-dependencies]\nwasm-bindgen-test = "0.3"\n',
    expect: ['wasm-bindgen-test'],
  },
  {
    name: 'features 表里的名字不算依赖',
    text: '[features]\ndefault = ["serde"]\nserde = ["dep:serde"]\n[dependencies]\nserde = { version = "1", optional = true }\n',
    expect: ['serde'],
  },
  {
    name: '带引号的依赖名',
    text: '[dependencies]\n"odd.name" = "1"\n',
    expect: ['odd.name'],
  },
];

/** 用一个被篡改的 manifest 确认规则**真的会红**。 */
const RULE_SELF_TESTS = [
  {
    name: 'wasm 依赖 worker 要被抓',
    crate: 'dhampir-wasm',
    deps: [{ name: 'dhampir-worker', kind: 'normal' }],
  },
  {
    name: 'core 依赖 media 要被抓',
    crate: 'dhampir-core',
    deps: [{ name: 'dhampir-media', kind: 'normal' }],
  },
  {
    name: 'timeline 依赖 wgpu 要被抓',
    crate: 'dhampir-timeline',
    deps: [{ name: 'wgpu', kind: 'normal' }],
  },
  {
    name: 'media 在 dev-dependencies 里依赖 web-sys 也要被抓',
    crate: 'dhampir-media',
    deps: [{ name: 'dhampir-timeline', kind: 'normal' }, { name: 'web-sys', kind: 'dev' }],
  },
  {
    name: '未知 crate 要被抓',
    crate: 'dhampir-brandnew',
    deps: [],
  },
];

function runSelfTest() {
  const failures = [];

  for (const testCase of PARSER_SELF_TESTS) {
    const got = parseManifest(testCase.text).map((d) => d.name).sort();
    const want = [...testCase.expect].sort();
    if (got.join(',') !== want.join(',')) {
      failures.push(`解析自检「${testCase.name}」期望 ${want.join(',') || '（空）'}，实际 ${got.join(',') || '（空）'}`);
    }
  }

  for (const testCase of RULE_SELF_TESTS) {
    const graph = new Map([[testCase.crate, { deps: testCase.deps, manifestPath: '<自检>' }]]);
    if (checkGraph(graph).length === 0) {
      failures.push(`规则自检「${testCase.name}」没有被抓出来——这条规则是坏的`);
    }
  }

  // 规则也要能放行：每个真实 crate 都用实际 manifest 跑一遍，不该有误报。
  for (const [crate, entry] of loadGraph()) {
    const problems = checkGraph(new Map([[crate, entry]]));
    if (problems.length > 0) {
      failures.push(`规则自检：${crate} 的真实 manifest 被误报——${problems.join('；')}`);
    }
  }

  return failures;
}

// ---------------------------------------------------------------------------

function loadGraph() {
  const members = parseWorkspaceMembers(readFileSync(join(REPO_ROOT, 'Cargo.toml'), 'utf8'));
  const graph = new Map();
  for (const member of members) {
    const manifestPath = join(REPO_ROOT, member, 'Cargo.toml');
    if (!existsSync(manifestPath)) continue;
    const text = readFileSync(manifestPath, 'utf8');
    const name = /^\s*name\s*=\s*"([^"]+)"/m.exec(text);
    graph.set(name ? name[1] : member, { deps: parseManifest(text), manifestPath: relative(REPO_ROOT, manifestPath) });
  }
  return graph;
}

function diskCrates() {
  const dir = join(REPO_ROOT, 'crates');
  if (!existsSync(dir)) return [];
  return readdirSync(dir, { withFileTypes: true })
    .filter((e) => e.isDirectory() && existsSync(join(dir, e.name, 'Cargo.toml')))
    .map((e) => `crates/${e.name}`)
    .sort();
}

function main() {
  // 参数必须先判死。这条是被一次反向探针逼出来的：那时 unknown 参数被默默忽略、
  // 退出码 0，于是"喂给守卫一个坏参数"竟然算通过——一条永远绿的路径。
  const unknown = process.argv.slice(2).filter((a) => a !== '--self-test' && a !== '-h' && a !== '--help');
  if (unknown.length > 0) {
    console.error(`✗ 不认识的参数：${unknown.join(' ')}`);
    console.error('  用法：node scripts/check-dep-graph.mjs [--self-test]');
    return 2;
  }
  if (process.argv.includes('-h') || process.argv.includes('--help')) {
    console.log('用法：node scripts/check-dep-graph.mjs [--self-test]');
    return 0;
  }

  const selfTestFailures = runSelfTest();
  if (selfTestFailures.length > 0) {
    console.error('✗ 守卫自检失败——先修守卫，别信它的结论：');
    for (const failure of selfTestFailures) console.error(`  - ${failure}`);
    return 2;
  }
  if (process.argv.includes('--self-test')) {
    console.log(
      `✓ 守卫自检通过（${PARSER_SELF_TESTS.length} 条解析用例 + ${RULE_SELF_TESTS.length} 条规则用例 + 真实 manifest 无误报）`,
    );
    return 0;
  }

  const graph = loadGraph();
  if (graph.size === 0) {
    console.error('✗ 一个 workspace 成员都没解析到——根 Cargo.toml 是不是变了？拒绝在空集合上通过。');
    return 2;
  }

  const problems = checkGraph(graph);

  // workspace 成员表与磁盘上的 crate 目录必须一致。少一个成员意味着那个
  // crate 根本不参与构建，而守卫也就永远看不见它——"没被检查"比"检查失败"更糟。
  const onDisk = diskCrates();
  const listed = [...graph.keys()].length;
  const listedPaths = parseWorkspaceMembers(readFileSync(join(REPO_ROOT, 'Cargo.toml'), 'utf8')).sort();
  if (onDisk.join(',') !== listedPaths.join(',')) {
    problems.push(
      `crates/ 下的目录与 workspace members 不一致：\n` +
        `      磁盘：${onDisk.join(', ')}\n` +
        `      成员：${listedPaths.join(', ')}`,
    );
  }

  if (problems.length > 0) {
    console.error(`✗ 依赖方向有问题（检查了 ${listed} 个 crate）：`);
    for (const problem of problems) console.error(`  - ${problem}`);
    return 1;
  }

  const edges = [...graph]
    .map(([crate, entry]) => {
      const internal = entry.deps.filter((d) => d.name.startsWith('dhampir-')).map((d) => d.name);
      return `${crate}${internal.length > 0 ? ` → ${internal.join(', ')}` : '（无内部依赖）'}`;
    })
    .join('\n  ');
  console.log(`✓ 依赖方向正确、无环，${listed} 个 crate：\n  ${edges}`);
  return 0;
}

// 只设 process.exitCode，见 scripts/wasm-test-node-exit-shim.cjs 里记的 libuv 断言坑。
process.exitCode = main();
