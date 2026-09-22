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
// 所以这里检查四件事：**允许的边**、**纯层的隔离**、**无环**、**名单对应**。
//
// "名单对应"指 `crates/` 下的每个目录都在 workspace members 里。没登记的目录等于
// 从守卫眼皮底下消失——先前只收"有 Cargo.toml 的目录"，于是 `crates/<dir>`
// （没有清单、也不在 members）在两侧名单里都不存在、静默通过。细则见
// compareDiskAndMembers。
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

import { existsSync, readdirSync, readFileSync, mkdirSync, mkdtempSync, writeFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
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

/**
 * 把根清单里列出的成员路径，对照成"清单真的存在"的成员。
 *
 * 为什么缺清单必须是**错误**而不是跳过：成员目录存在、却没有 Cargo.toml，
 * 意味着那个 crate 根本不参与构建（`cargo` 会直接报错退出），守卫也就永远
 * 看不见它——"没被检查"比"检查失败"更糟。先前这里是 `existsSync → continue`，
 * 于是"少了一个 crate"被静默吞掉，只见一句误导人的"根 Cargo.toml 是不是变了？"。
 *
 * `hasManifest` 注入进来（真实调用传 existsSync），因为这条规则最容易坏成的样子
 * 就是"悄悄少查一个 crate"——必须能用假目录树在自检里钉住。
 *
 * 返回 `{ members: [{ member, manifestPath }], problems: string[] }`。
 */
export function resolveMembers(members, hasManifest) {
  const resolved = [];
  const problems = [];
  for (const member of members) {
    const manifestPath = `${member}/Cargo.toml`;
    if (hasManifest(manifestPath)) {
      resolved.push({ member, manifestPath });
      continue;
    }
    problems.push(
      `workspace 成员 ${member}/ 下没有 Cargo.toml——期望 ${manifestPath}。` +
        `这个 crate 现在既不参与构建、也不被本守卫检查；要么补上清单，要么把它从根 Cargo.toml 的 members 里删掉。`,
    );
  }
  return { members: resolved, problems };
}

/**
 * `crates/` 下的目录清单与 workspace members 必须**逐一对应**，返回问题清单。
 *
 * 为什么单独成函数：先前这段比对只看"有 Cargo.toml 的目录"，于是
 * `crates/<dir>`（既没清单、也不在 members）在磁盘侧与成员侧**都不存在**——
 * 守卫完全跳过它，退出码 0（独立复核抓到的盲区）。这种形态的目录要么是手滑
 * 建出来的、要么是"想加个 crate 但没登记"，两种都该当场报出来。
 *
 * 判据抽成纯函数是为了能用合成输入钉住（见 DISK_MEMBERSHIP_SELF_TESTS）：
 * 一段写坏的比对不会报错，只会让某些目录永远不被检查。
 *
 * `diskEntries` 是 `[{ path: 'crates/名字', hasManifest }]`；`memberPaths` 是
 * 根清单里列出的路径（`crates/名字`）。"成员列了但缺清单"的正常报错路径在
 * readEnvironment / resolveMembers（环境错 → 退出码 2），这里再兜一次底，
 * 好让这个函数单独拿出来也是自洽的。
 */
export function compareDiskAndMembers(diskEntries, memberPaths) {
  const problems = [];
  const members = [...memberPaths].sort();
  const onDisk = new Map(diskEntries.map((entry) => [entry.path, entry.hasManifest]));

  for (const entry of diskEntries) {
    if (members.includes(entry.path)) {
      if (!entry.hasManifest) {
        // 目录在、清单不在：正常路径上 resolveMembers 先报（环境错 → 退出码 2）；
        // 这里兜底，让这个函数单独拿出来也自洽。
        problems.push(`${entry.path} 在 workspace members 里，但没有 ${entry.path}/Cargo.toml——这个 crate 不参与构建。`);
      }
      continue;
    }
    if (entry.hasManifest) {
      problems.push(
        `${entry.path} 有 Cargo.toml，却不在 workspace members 里——它不参与构建，` +
          `守卫也永远看不见它：要么把它登记进根 Cargo.toml，要么删掉这个目录。`,
      );
    } else {
      problems.push(
        `${entry.path} 既没有 Cargo.toml、也不在 workspace members 里——守卫完全看不见它` +
          `（不是 crate，却占着 crates/ 的位置）：补一份清单并登记，或把它移出 crates/。`,
      );
    }
  }

  for (const member of members) {
    if (!onDisk.has(member)) {
      problems.push(`workspace members 列了 ${member}，磁盘上却没有这个目录——两份名单对不上。`);
    }
  }
  return problems;
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

/**
 * 成员清单规则的自检：缺清单要变成"写明目录与期望文件名的错误"，而不是跳过或抛异常。
 */
const MEMBER_SELF_TESTS = [
  {
    name: '成员有 Cargo.toml 时放行',
    members: ['crates/dhampir-core'],
    present: ['crates/dhampir-core/Cargo.toml'],
    expectProblems: 0,
  },
  {
    name: '成员缺 Cargo.toml 要明确报错',
    members: ['crates/dhampir-timeline'],
    present: [],
    expectProblems: 1,
    expectMentions: ['crates/dhampir-timeline/', 'crates/dhampir-timeline/Cargo.toml'],
  },
  {
    name: '多成员里只缺一个，也只报那一个',
    members: ['crates/a', 'crates/b'],
    present: ['crates/a/Cargo.toml'],
    expectProblems: 1,
    expectMentions: ['crates/b/Cargo.toml'],
  },
];

/**
 * 名单对应的自检：`crates/` 的目录清单 vs workspace members。
 *
 * 这一组是独立复核发现 2 的钉子。判据是纯函数（compareDiskAndMembers），
 * 所以每条都用**合成输入**跑，不依赖这棵树当时长什么样。
 */
const DISK_MEMBERSHIP_SELF_TESTS = [
  {
    name: '逐一对应时不报',
    disk: [{ path: 'crates/a', hasManifest: true }],
    members: ['crates/a'],
    expectProblems: 0,
  },
  {
    name: '有清单但没登记到 members 要报',
    disk: [{ path: 'crates/a', hasManifest: true }, { path: 'crates/b', hasManifest: true }],
    members: ['crates/a'],
    expectProblems: 1,
    expectMentions: ['crates/b', 'members'],
  },
  {
    name: '既没清单也没登记（先前的死角）要报',
    disk: [{ path: 'crates/a', hasManifest: true }, { path: 'crates/scratch', hasManifest: false }],
    members: ['crates/a'],
    expectProblems: 1,
    expectMentions: ['crates/scratch', 'Cargo.toml'],
  },
  {
    name: 'members 列了磁盘却没有要报',
    disk: [],
    members: ['crates/a'],
    expectProblems: 1,
    expectMentions: ['crates/a'],
  },
  {
    name: '登记了但清单不在要报',
    disk: [{ path: 'crates/a', hasManifest: false }],
    members: ['crates/a'],
    expectProblems: 1,
    expectMentions: ['Cargo.toml'],
  },
  {
    name: '两侧都空 → 不报（空集合由 main 的成员闸门负责）',
    disk: [],
    members: [],
    expectProblems: 0,
  },
];

function runSelfTest() {
  const failures = [];
  let diskCases = 0;

  for (const testCase of PARSER_SELF_TESTS) {
    const got = parseManifest(testCase.text).map((d) => d.name).sort();
    const want = [...testCase.expect].sort();
    if (got.join(',') !== want.join(',')) {
      failures.push(`解析自检「${testCase.name}」期望 ${want.join(',') || '（空）'}，实际 ${got.join(',') || '（空）'}`);
    }
  }

  for (const testCase of MEMBER_SELF_TESTS) {
    const { members, problems } = resolveMembers(testCase.members, (p) => testCase.present.includes(p));
    const unresolved = testCase.members.filter((m) => !testCase.present.includes(`${m}/Cargo.toml`));
    if (problems.length !== testCase.expectProblems || members.length !== testCase.members.length - unresolved.length) {
      failures.push(
        `成员清单自检「${testCase.name}」期望 ${testCase.expectProblems} 处问题 / ${testCase.members.length - unresolved.length} 个成员，` +
          `实际 ${problems.length} 处 / ${members.length} 个：${problems.join('；')}`,
      );
      continue;
    }
    for (const needle of testCase.expectMentions ?? []) {
      if (!problems.some((p) => p.includes(needle))) {
        failures.push(`成员清单自检「${testCase.name}」的问题里没写清 ${needle}：${problems.join('；')}`);
      }
    }
  }

  for (const testCase of DISK_MEMBERSHIP_SELF_TESTS) {
    const problems = compareDiskAndMembers(testCase.disk, testCase.members);
    if (problems.length !== testCase.expectProblems) {
      failures.push(
        `名单对应自检「${testCase.name}」期望 ${testCase.expectProblems} 处问题，实际 ${problems.length} 处：${problems.join('；')}`,
      );
      continue;
    }
    for (const needle of testCase.expectMentions ?? []) {
      if (!problems.some((p) => p.includes(needle))) {
        failures.push(`名单对应自检「${testCase.name}」的问题里没写清 ${needle}：${problems.join('；')}`);
      }
    }
  }

  for (const testCase of RULE_SELF_TESTS) {
    const graph = new Map([[testCase.crate, { deps: testCase.deps, manifestPath: '<自检>' }]]);
    if (checkGraph(graph).length === 0) {
      failures.push(`规则自检「${testCase.name}」没有被抓出来——这条规则是坏的`);
    }
  }

  // 规则也要能放行：每个真实 crate 都用实际 manifest 跑一遍，不该有误报。
  // `--self-test` 的语义与另外两个守卫一致：只验**守卫自己的逻辑**，不依赖这棵树
  // 完不完整（树不完整由 main() 的环境闸门负责判死，不在这里重复报一次）。
  const environment = readEnvironment();
  const notes = [];
  if (environment.errors.length > 0) {
    notes.push(
      `本树有 ${environment.errors.length} 处环境/输入错（正常跑会退出码 2，与自检结果无关）：${environment.errors.join('；')}`,
    );
  }
  for (const [crate, entry] of environment.graph) {
    const problems = checkGraph(new Map([[crate, entry]]));
    if (problems.length > 0) {
      failures.push(`规则自检：${crate} 的真实 manifest 被误报——${problems.join('；')}`);
    }
  }
  const realManifests = environment.graph.size;

  // 再走一遍真正的磁盘路径。上面那组是注入出来的判定，这里要证的是
  // "合成根里缺清单时，readEnvironment 给出一条写明期望路径的错误、**且不抛异常**"——
  // 先前这条路径会抛出 node:fs 的未捕获异常（退出码 1、栈指向 node 内部），
  // 而且它发生在 runSelfTest 里，连 main() 的空集合检查都到不了。
  const dir = mkdtempSync(join(tmpdir(), 'dhampir-depgraph-'));
  try {
    const memberMissing = join(dir, 'member-no-manifest');
    mkdirSync(join(memberMissing, 'crates', 'dhampir-timeline'), { recursive: true });
    writeFileSync(join(memberMissing, 'Cargo.toml'), '[workspace]\nmembers = ["crates/dhampir-timeline"]\n');
    const memberEnv = readEnvironment(memberMissing);
    diskCases += 1;
    if (memberEnv.errors.length !== 1 || memberEnv.graph.size !== 0 || !memberEnv.errors[0].includes('crates/dhampir-timeline/Cargo.toml')) {
      failures.push(
        `磁盘自检「成员目录缺 Cargo.toml」期望 1 处错误 / 0 个成员且写明期望路径，实际 ` +
          `${memberEnv.errors.length} 处 / ${memberEnv.graph.size} 个：${memberEnv.errors.join('；')}`,
      );
    }

    const noRoot = join(dir, 'no-root-manifest');
    mkdirSync(noRoot, { recursive: true });
    const bareEnv = readEnvironment(noRoot);
    diskCases += 1;
    if (bareEnv.errors.length !== 1 || bareEnv.graph.size !== 0) {
      failures.push(
        `磁盘自检「根 Cargo.toml 不存在」期望 1 处错误 / 0 个成员（且不抛异常），实际 ${bareEnv.errors.length} 处 / ${bareEnv.graph.size} 个`,
      );
    }

    // 空集合这条判定得**可达**：根清单里 members 写成空表时没有环境错，
    // 于是 main() 里"拒绝在空集合上通过"那条检查接手（先前崩得比它早）。
    const emptyMembers = join(dir, 'empty-members');
    mkdirSync(emptyMembers, { recursive: true });
    writeFileSync(join(emptyMembers, 'Cargo.toml'), '[workspace]\nmembers = []\n');
    const emptyEnv = readEnvironment(emptyMembers);
    diskCases += 1;
    if (emptyEnv.errors.length !== 0 || emptyEnv.graph.size !== 0) {
      failures.push(
        `磁盘自检「members 空表」期望 0 处环境错 / 0 个成员（好让空集合检查接手），实际 ${emptyEnv.errors.length} 处 / ${emptyEnv.graph.size} 个`,
      );
    }

    // 名单对应的**磁盘路径**也要真的走一遍：上面那组是注入的表格，这里要证
    // "crates/ 下一个没清单的目录会被 diskEntries 收进来、并对照出问题"——
    // 先前 diskCrates 只收有清单的目录，这个形态的目录压根不会出现在比对里。
    const cratesScan = join(dir, 'crates-scan');
    mkdirSync(join(cratesScan, 'crates', 'dhampir-a'), { recursive: true });
    writeFileSync(join(cratesScan, 'crates', 'dhampir-a', 'Cargo.toml'), '[package]\nname = "dhampir-a"\n');
    mkdirSync(join(cratesScan, 'crates', 'scratch'), { recursive: true });
    const scanned = diskEntries(cratesScan);
    const scanProblems = compareDiskAndMembers(scanned, ['crates/dhampir-a']);
    diskCases += 1;
    if (scanned.length !== 2 || scanProblems.length !== 1 || !scanProblems[0].includes('crates/scratch')) {
      failures.push(
        `磁盘自检「没清单的目录要被抓」期望 2 个目录项 / 1 处问题且点名 crates/scratch，实际 ` +
          `${scanned.length} 个 / ${scanProblems.length} 处：${scanProblems.join('；')}`,
      );
    }
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }

  return { failures, diskCases, notes, realManifests };
}

// ---------------------------------------------------------------------------

/**
 * 读磁盘：根清单 → 成员清单 → 依赖图。
 *
 * **一律不抛异常。** 环境/输入错（根清单读不到、成员目录缺清单）收进 `errors`，
 * 由 main() 统一按"环境错 = 退出码 2"报出。先前这里的 `readFileSync` 在根清单
 * 不存在时抛未捕获异常：退出码落在 1、栈指向 node:fs 内部，把"哪个成员缺清单"
 * 这条最该被看见的信息埋掉了；而且它先在 runSelfTest() 里炸，main() 里那条
 * "拒绝在空集合上通过"根本走不到。
 *
 * `root` 可注入（默认本仓库），这样自检能拿合成目录树跑真路径。
 */
function readEnvironment(root = REPO_ROOT) {
  const errors = [];
  const rootManifestPath = join(root, 'Cargo.toml');
  let rootText;
  try {
    rootText = readFileSync(rootManifestPath, 'utf8');
  } catch (error) {
    return {
      listedPaths: [],
      graph: new Map(),
      errors: [`读不到根清单 ${relative(root, rootManifestPath)}：${error.message}——workspace 根目录是不是算错了？`],
    };
  }

  const listedPaths = parseWorkspaceMembers(rootText).sort();
  const resolved = resolveMembers(listedPaths, (manifestPath) => existsSync(join(root, manifestPath)));
  errors.push(...resolved.problems);

  const graph = new Map();
  for (const { member, manifestPath } of resolved.members) {
    let text;
    try {
      text = readFileSync(join(root, manifestPath), 'utf8');
    } catch (error) {
      errors.push(`读不到 ${manifestPath}：${error.message}`);
      continue;
    }
    const name = /^\s*name\s*=\s*"([^"]+)"/m.exec(text);
    graph.set(name ? name[1] : member, { deps: parseManifest(text), manifestPath });
  }

  // `listedPaths` 原样带出去：main() 要用它跟磁盘上的 crate 目录比对，不必再读一遍根清单。
  return { listedPaths, graph, errors };
}

/**
 * `crates/` 下**每一个**目录项都要交代，不只是"看着像 crate"的那些。
 *
 * 先前这里只收"有 Cargo.toml 的目录"，于是没有清单的目录在磁盘侧与成员侧
 * **都不存在**，守卫静默通过（见 compareDiskAndMembers）。目录与符号链接都算：
 * 一个占着 `crates/<名字>` 的位置、却没人检查的目录，正是这条闸门要拦住的东西。
 *
 * `root` 可注入（默认本仓库），好让自检拿合成目录树跑真路径。
 */
function diskEntries(root = REPO_ROOT) {
  const dir = join(root, 'crates');
  if (!existsSync(dir)) return [];
  return readdirSync(dir, { withFileTypes: true })
    .filter((entry) => entry.isDirectory() || entry.isSymbolicLink())
    .map((entry) => ({
      path: `crates/${entry.name}`,
      hasManifest: existsSync(join(dir, entry.name, 'Cargo.toml')),
    }))
    .sort((a, b) => (a.path < b.path ? -1 : a.path > b.path ? 1 : 0));
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

  const selfTest = runSelfTest();
  if (selfTest.failures.length > 0) {
    console.error('✗ 守卫自检失败——先修守卫，别信它的结论：');
    for (const failure of selfTest.failures) console.error(`  - ${failure}`);
    return 2;
  }
  if (process.argv.includes('--self-test')) {
    console.log(
      `✓ 守卫自检通过（${PARSER_SELF_TESTS.length} 条解析用例 + ${RULE_SELF_TESTS.length} 条规则用例 + ` +
        `${MEMBER_SELF_TESTS.length} 条成员清单用例 + ${selfTest.diskCases} 条磁盘用例 + ` +
        `真实 manifest ${selfTest.realManifests} 个无误报）`,
    );
    // 树本身有问题时如实说出来：自检通过只说明守卫逻辑没问题，不代表这棵树能判。
    for (const note of selfTest.notes) console.error(`⚠ ${note}`);
    return 0;
  }

  const environment = readEnvironment();

  // 环境/输入错 → 退出码 2（README §守卫脚本：不在空文件集上通过）。
  // 先判死：这类情况下图是不完整的，拿它下"依赖方向正确"的结论就是假绿。
  if (environment.errors.length > 0) {
    console.error(`✗ 这份 workspace 读不干净，拒绝下结论（${environment.errors.length} 处环境/输入错）：`);
    for (const error of environment.errors) console.error(`  - ${error}`);
    return 2;
  }

  const graph = environment.graph;
  // 空集合上宣布"全绿"是自欺。现在这条**真的可达**：根 Cargo.toml 里
  // members 没写出任何成员时就是它接手（见 readEnvironment / 磁盘自检）。
  if (graph.size === 0) {
    console.error('✗ 一个 workspace 成员都没解析到——根 Cargo.toml 是不是变了？拒绝在空集合上通过。');
    return 2;
  }

  const problems = checkGraph(graph);

  // `crates/` 下的**每个**目录都要在 workspace members 里，不只是"有 Cargo.toml
  // 的那些"。缺一个成员意味着那个 crate 根本不参与构建，而守卫也就永远看不见它；
  // 一个既没清单又没登记的目录更糟：它在两侧名单里都不存在、被完全跳过
  // （独立复核抓到的盲区，用 compareDiskAndMembers 的合成用例钉住）。
  const onDisk = diskEntries();
  const listed = [...graph.keys()].length;
  problems.push(...compareDiskAndMembers(onDisk, environment.listedPaths));

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
