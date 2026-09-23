#!/usr/bin/env node
// 底座调用面清单 + 宿主 API 契约：**两份都从代码生成**，再用守卫钉住不许漂。
//
// 为什么需要它：wasm 侧有几十个导出，其中一部分是**内部取证工具**
// （M0/M2 的 corpus/probe），另一部分才是下游会依赖的 API。
// 下游要读几十个函数才知道从哪进 —— 这本身就是桥接效率的敌人；
// 更糟的是**每个导出都像一份长期兼容承诺**，而其中大半并不承诺兼容。
//
// 办法：按模块分类，生成一份清单，并让守卫盯着「清单 == 代码」。
// 手写清单一定会漂，所以它必须是生成的。
//
// # 两份文档，两种口径（T2.7）
//
// * `docs/api-surface.md` —— **整份**是生成的：它只有一张分类表和一份名单，
//   没有一句得留着的人话，所以重写它没有代价。
// * `docs/host-api.md` —— 说明由人写，机器只拥有**两样东西**：
//   版本行（整行的 `Version: N`）与导出名单（`- \`dhampir_*\`` 那些行）。
//   所以 `--write` 只动这两样，**别处一个字节都不碰** —— 生成器一旦"重写整份"，
//   人写的口径（形状从哪查、哪些导出只是取证工具）会被一次次抹掉，
//   而抹掉之后没人会发现自己读到的是生成器的默认话术。
//
// # 版本号为什么不写在这里
//
// 版本的真值在 Rust 源码里（`pub const HOST_API_VERSION: u32 = N;`）。
// 这里**扫源码**，不存第二份数字：写死的话，下一次升版本的第一件事就变成改守卫，
// 而那样守卫守的是它自己的影子。
//
// 用法：
//   node scripts/api-surface.mjs --write     重新生成 docs/api-surface.md；更新 docs/host-api.md 的版本行
//   node scripts/api-surface.mjs             检查（不清不写）
//   node scripts/api-surface.mjs --self-test 只跑守卫自检
//   node scripts/api-surface.mjs --help

import { existsSync, mkdirSync, readFileSync, readdirSync, writeFileSync } from 'node:fs';
import { dirname, join, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

export const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');

export const USAGE = [
  '用法：node scripts/api-surface.mjs [--write | --self-test | --help]',
  '  --write      重新生成 docs/api-surface.md；只更新 docs/host-api.md 的版本行',
  '  --self-test  只跑守卫自检',
  '  --help       这一页',
].join('\n');

/** 模块 -> 分类。**分类是人的判断，所以写在这里；函数清单是从代码扫的。** */
export const MODULE_CLASS = {
  'timeline_host.rs': ['底座 API', '下游会依赖：工程载入/校验/求值/上屏/素材绑定。**这部分才承诺兼容。**'],
  'demux_wasm.rs': ['底座 API', '按帧号定位同步样本（帧精确的前提）。'],
  'cache_wasm.rs': ['宿主内部', '帧缓存记账。宿主自己用，**不是渲染契约的一部分**。'],
  'preview.rs': ['弃用', 'M3 的单片段预览，已被 timeline_host 取代。**新代码不要用。**'],
  'web.rs': ['取证工具', 'M0–M2 的探针与 corpus。**不承诺兼容**，供本仓库验收用。'],
  'corpus.rs': ['取证工具', 'M2 的语料与记录。**不承诺兼容。**'],
};

/** 宿主 API 文档、它钉住的模块、以及版本常量的来源（都是仓库相对路径）。 */
export const HOST_API_DOC = 'docs/host-api.md';
export const HOST_API_MODULE = 'timeline_host.rs';
export const HOST_API_SOURCE = 'crates/dhampir-timeline/src/host_api.rs';

/** 版本常量那一行、文档里的版本行、名单里的列表项。**都按整行认，不认行内提及。** */
const VERSION_DECL = /pub const HOST_API_VERSION: u32 = (\d+);/;
const VERSION_LINE = /^Version: (\d+)$/m;
const LIST_ITEM = /^- `(dhampir_[A-Za-z0-9_]+)`$/;

/** 从源码里读版本常量。读不到就返回 null —— **不猜，也不退回默认值**。 */
export function readHostApiVersion(sourceText) {
  const match = sourceText.match(VERSION_DECL);
  return match ? Number(match[1]) : null;
}

/** 文档里写的版本（整行 `Version: N`）。没有就返回 null。 */
export function docVersion(docText) {
  const match = docText.match(VERSION_LINE);
  return match ? Number(match[1]) : null;
}

/** 文档名单里列的导出名。只看列表项：正文里提一句不算进名单。 */
export function listedNames(docText) {
  const names = [];
  for (const line of docText.split('\n')) {
    const match = line.match(LIST_ITEM);
    if (match) names.push(match[1]);
  }
  return names;
}

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

/** 判断：调用面清单与代码是否一致。 */
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
    const match = line.match(LIST_ITEM);
    if (!match) continue;
    const exists = [...exports.values()].some((names) => names.includes(match[1]));
    if (!exists) problems.push('清单里的 ' + match[1] + ' 在代码里找不到（删了导出？重新生成）');
  }
  return problems;
}

/**
 * 判断：宿主 API 文档与源码是否一致。
 *
 * 三条都做过真实反向验证（见 plan/t2-evidence.md 的 T2.7 段）：
 * 版本行缺失 / 版本与源码不同 / 名单与导出对不上。**名单按双向对** ——
 * 只查「文档里写的名字是否存在」，会让**新导出悄悄不进文档**，
 * 而"文档落后于代码"与"文档就是全部"从读的人那侧看起来一模一样。
 */
export function judgeHostApi(docText, exports, version) {
  if (docText === null) {
    return ['缺少 ' + HOST_API_DOC + '（`--write` 只能替你补版本行，说明与名单得人写）'];
  }
  const problems = [];
  const written = docVersion(docText);
  if (written === null) {
    problems.push(HOST_API_DOC + ' 里没有整行的 `Version: N` —— 对端照着它查形状，没有它就只能猜');
  } else if (written !== version) {
    problems.push(
      HOST_API_DOC + ' 写的是 Version: ' + written + '，源码里的 HOST_API_VERSION 是 ' + version + '（升了版本忘改文档）',
    );
  }
  const names = listedNames(docText);
  if (names.length === 0) {
    problems.push(HOST_API_DOC + ' 一条导出都没列 —— 拒绝在空名单上通过');
  }
  const known = new Set(exports.get(HOST_API_MODULE) || []);
  for (const name of names) {
    if (!known.has(name)) {
      problems.push('文档里的 ' + name + ' 在 ' + HOST_API_MODULE + ' 里找不到（删了导出？重新生成名单）');
    }
  }
  for (const name of known) {
    if (!names.includes(name)) problems.push('导出 ' + name + ' 没有出现在 ' + HOST_API_DOC + ' 的名单里');
  }
  return problems;
}

/** 把版本行改对：有就只改数字，没有就插在第一个标题下面。**别处一个字节都不动。** */
export function updateVersionLine(docText, version) {
  if (VERSION_LINE.test(docText)) return docText.replace(VERSION_LINE, 'Version: ' + version);
  const lines = docText.split('\n');
  const title = lines.findIndex((line) => line.startsWith('#'));
  lines.splice(title >= 0 ? title + 1 : 0, 0, '', 'Version: ' + version);
  return lines.join('\n');
}

/**
 * `--write` 的动作范围：**只有这两份文档**，而且宿主那份只动版本行。
 *
 * 返回要写的 `[{ path, text, note }]`；已经一致的不进清单（那才是"没动别处"的证明）。
 * 宿主文档不存在时只写一行版本 —— 说明和名单守卫不会替人编。
 */
export function writeActions({ exports, version, surfaceText, hostText }) {
  const actions = [];
  const rendered = renderSurface(exports);
  const surface = rendered.endsWith('\n') ? rendered : rendered + '\n';
  if (surfaceText !== surface) {
    actions.push({ path: 'docs/api-surface.md', text: surface, note: '整份重新生成' });
  }
  if (hostText === null) {
    actions.push({
      path: HOST_API_DOC,
      text: '# 宿主 API\n\nVersion: ' + version + '\n',
      note: '只写了版本行：说明与导出名单得人写（名单不齐守卫会红）',
    });
  } else {
    const wanted = updateVersionLine(hostText, version);
    if (wanted !== hostText) actions.push({ path: HOST_API_DOC, text: wanted, note: '只改了版本行' });
  }
  return actions;
}

/** 守卫自己的自检。守卫要是坏的，"全绿"就没有意义。 */
function runSelfTest() {
  const failures = [];
  let passed = 0;
  const expect = (name, problems, shouldBeEmpty) => {
    if ((problems.length === 0) !== shouldBeEmpty) {
      failures.push(name + ' -> ' + JSON.stringify(problems));
      return;
    }
    passed += 1;
  };
  const ok = (name, condition, detail) => {
    if (!condition) {
      failures.push(name + (detail === undefined ? '' : ' -> ' + detail));
      return;
    }
    passed += 1;
  };

  // --- 调用面清单 ---
  const surfaceExports = new Map([['timeline_host.rs', ['dhampir_project_open']]]);
  const good = '- `dhampir_project_open`';
  expect('调用面：一致 -> 通过', judge(good, surfaceExports), true);
  expect('调用面：漏了导出 -> 红', judge('（空）', surfaceExports), false);
  expect('调用面：清单里有代码里没有的 -> 红', judge(good + '\n- `dhampir_gone`', surfaceExports), false);
  expect('调用面：模块没分类 -> 红', judge(good, new Map([['unknown.rs', ['dhampir_x']]])), false);

  // --- 宿主 API 文档（T2.7）---
  const hostExports = new Map([
    ['timeline_host.rs', ['dhampir_host_api_version', 'dhampir_project_open']],
  ]);
  const hostDoc =
    '# 宿主 API\n\nVersion: 2\n\n先问版本，再照形状对。\n\n## 交付形状的导出\n\n- `dhampir_project_open`\n- `dhampir_host_api_version`\n';
  expect('宿主 API：版本与名单都一致 -> 通过', judgeHostApi(hostDoc, hostExports, 2), true);
  expect('宿主 API：没有版本行 -> 红', judgeHostApi(hostDoc.replace('Version: 2\n', ''), hostExports, 2), false);
  expect('宿主 API：版本行不是整行 -> 红', judgeHostApi(hostDoc.replace('Version: 2\n', 'Version: 2（随手写的）\n'), hostExports, 2), false);
  expect('宿主 API：版本与源码不同 -> 红', judgeHostApi(hostDoc, hostExports, 3), false);
  expect('宿主 API：文档里有代码里没有的名字 -> 红', judgeHostApi(hostDoc + '- `dhampir_gone`\n', hostExports, 2), false);
  expect(
    '宿主 API：代码里有文档没写的导出 -> 红',
    judgeHostApi(hostDoc, new Map([['timeline_host.rs', ['dhampir_host_api_version', 'dhampir_project_doc', 'dhampir_project_open']]]), 2),
    false,
  );
  expect('宿主 API：空名单 -> 红', judgeHostApi('Version: 2\n', new Map([['timeline_host.rs', []]]), 2), false);
  expect('宿主 API：文档缺文件 -> 红', judgeHostApi(null, hostExports, 2), false);

  // 版本从哪来：只认源码那一行，扫不到就是 null（不猜、不用默认值）。
  ok('读版本：扫到常量', readHostApiVersion('pub const HOST_API_VERSION: u32 = 7;') === 7);
  ok('读版本：没有常量 -> null', readHostApiVersion('pub const HOST_API_VERSION: u32 = 0') === null);
  ok('读版本：写成别的类型不算', readHostApiVersion('pub const HOST_API_VERSION: u8 = 7;') === null);
  ok('读版本：文档版本行只认整行', docVersion('Version: 12\n') === 12 && docVersion('Version: 12 提一句\n') === null);
  ok('名单：只看列表项', listedNames('见 `dhampir_project_open`\n- `dhampir_project_doc`\n').join() === 'dhampir_project_doc');

  // --write 的动作范围：**别处一个字节都不许动**。
  const untouched = '# 宿主 API\n\nVersion: 2\n\n说明。\n';
  const same = writeActions({ exports: hostExports, version: 2, surfaceText: '旧内容'.padEnd(5, 'x'), hostText: untouched });
  ok('--write：版本一致时不产生宿主文档动作', !same.some((action) => action.path === HOST_API_DOC), JSON.stringify(same));
  ok('--write：调用面清单不同步时整份重写', same.some((action) => action.path === 'docs/api-surface.md'));
  const rendered = renderSurface(hostExports);
  const inSync = writeActions({
    exports: hostExports,
    version: 2,
    surfaceText: rendered.endsWith('\n') ? rendered : rendered + '\n',
    hostText: untouched,
  });
  ok('--write：两份都一致时什么都不写', inSync.length === 0, JSON.stringify(inSync));
  const bumped = writeActions({ exports: hostExports, version: 3, surfaceText: rendered + '\n', hostText: untouched })
    .find((action) => action.path === HOST_API_DOC);
  ok('--write：升版本只改那一行的数字', bumped && bumped.text === untouched.replace('Version: 2', 'Version: 3'), JSON.stringify(bumped));
  const inserted = updateVersionLine('# 宿主 API\n\n说明。\n', 4);
  ok('--write：缺版本行时插在标题下面', inserted === '# 宿主 API\n\nVersion: 4\n\n说明。\n', JSON.stringify(inserted));
  ok('--write：插入后其余行逐字节不变', inserted.replace('Version: 4\n\n', '') === '# 宿主 API\n\n说明。\n');

  if (failures.length > 0) {
    console.error('✗ 守卫自检失败——先修守卫，别信它的结论：');
    for (const failure of failures) console.error('  - ' + failure);
    return 2;
  }
  console.log('✓ 调用面清单与宿主 API 守卫自检通过（' + passed + ' 条断言）');
  return 0;
}

function main() {
  // 参数先判死：不认识的参数被默默忽略 = 一条永远绿的路径（照 check-core-purity 的口径）。
  const argv = process.argv.slice(2);
  const unknown = argv.filter((arg) => arg !== '--write' && arg !== '--self-test' && arg !== '-h' && arg !== '--help');
  if (unknown.length > 0) {
    console.error('✗ 不认识的参数：' + unknown.join(' '));
    console.error('  ' + USAGE.split('\n')[0]);
    return 2;
  }
  if (argv.includes('-h') || argv.includes('--help')) {
    console.log(USAGE);
    return 0;
  }
  if (argv.includes('--self-test')) return runSelfTest();

  const srcDir = join(REPO_ROOT, 'crates', 'dhampir-wasm', 'src');
  const exports = scanExports(srcDir);
  if (exports.size === 0) {
    console.error('✗ 一个 wasm 导出都没扫到 —— 路径是不是变了？拒绝在空集合上通过');
    return 2;
  }

  const sourcePath = join(REPO_ROOT, HOST_API_SOURCE);
  if (!existsSync(sourcePath)) {
    console.error('✗ 读不到版本常量的来源 ' + HOST_API_SOURCE + ' —— 版本无从比对，守卫拒绝在不知道版本的情况下通过');
    return 2;
  }
  const version = readHostApiVersion(readFileSync(sourcePath, 'utf8'));
  if (version === null) {
    console.error('✗ ' + HOST_API_SOURCE + ' 里找不到 `pub const HOST_API_VERSION: u32 = N;` —— 同上，拒绝通过');
    return 2;
  }

  const surfacePath = join(REPO_ROOT, 'docs', 'api-surface.md');
  const hostPath = join(REPO_ROOT, HOST_API_DOC);
  const surfaceText = existsSync(surfacePath) ? readFileSync(surfacePath, 'utf8') : null;
  const hostText = existsSync(hostPath) ? readFileSync(hostPath, 'utf8') : null;

  if (argv.includes('--write')) {
    const actions = writeActions({ exports, version, surfaceText, hostText });
    for (const action of actions) {
      const target = join(REPO_ROOT, action.path);
      mkdirSync(dirname(target), { recursive: true });
      writeFileSync(target, action.text);
      console.log('已写入 ' + relative(REPO_ROOT, target).split('\\').join('/') + '：' + action.note);
    }
    if (actions.length === 0) console.log('两份文档都已是最新（没有要写的）');
    return 0;
  }

  const problems = [];
  if (surfaceText === null) {
    problems.push('缺少 docs/api-surface.md（跑 --write 生成）');
  } else {
    problems.push(...judge(surfaceText, exports));
    const rendered = renderSurface(exports);
    if (surfaceText !== (rendered.endsWith('\n') ? rendered : rendered + '\n')) {
      problems.push('清单与代码不同步（跑 --write 重新生成）');
    }
  }
  problems.push(...judgeHostApi(hostText, exports, version));

  if (problems.length > 0) {
    console.error('✗ 调用面/宿主 API 与代码不一致（' + HOST_API_MODULE + ' 的 ' + (exports.get(HOST_API_MODULE) || []).length + ' 个导出 / 版本 ' + version + '）：');
    for (const problem of problems) console.error('  - ' + problem);
    return 1;
  }
  let total = 0;
  for (const names of exports.values()) total += names.length;
  console.log(
    '✓ 调用面清单与宿主 API 都与代码一致（' + exports.size + ' 个模块 / ' + total + ' 个导出；版本 ' + version + '，' +
      HOST_API_DOC + ' 列了 ' + listedNames(hostText).length + ' 个导出）',
  );
  return 0;
}

// 只设 process.exitCode，不调 process.exit()：在本机的 Node/Windows 上，
// 真正被执行的 process.exit() 可能撞上 libuv 的 UV_HANDLE_CLOSING 断言，
// 退出码变成负数（-1073740791）。守卫的退出码是它的唯一结论。
process.exitCode = main();
