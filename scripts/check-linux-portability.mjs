#!/usr/bin/env node
// D16 的守卫：**Linux 可移植性**。
//
// # 为什么这条值得有守卫
//
// 决定是**不跑** Linux 的两条取证腿（D13，wontfix）——本机没有 docker，WSL 也没有发行版。
// 不跑腿不等于不考虑：它意味着**只能靠静态检查兜住**。
// 而"在 Windows 上写出来的、在 Linux 上会崩的东西"几乎全是同一批模式，
// 它们在这台机器上**永远不会自己变红**：
//
//   * 手写反斜杠当分隔符（`replace('/', '\')`、用 `'\'` 去 join）；
//   * 假设文件系统大小写不敏感（拿小写化的整条路径去 open）；
//   * 假设文件名一定是合法 UTF-8（Linux 上 `OsStr` 可以是非 UTF-8 字节）；
//   * 写 `#[cfg(windows)]` 却没有另一平台的分支 —— 于是在 Linux 上那个东西**根本不存在**。
//
// # 判据
//
//   * R1 不许把 `/` 换成 `\`（把路径写成只有 Windows 认的样子）。
//        反向是**允许且鼓励**的：`replace('\', '/')`、`.split('\').join('/')` 是本仓到处在用的
//        规范化方向（见 scripts/record-acceptance.mjs）。
//   * R2 不许拿小写化的**整条路径**去访问文件系统。
//        对**扩展名 / 格式名**做小写化是允许的，所以判据要求"同一行还有文件系统调用"。
//   * R3 不许假定路径一定是 UTF-8（`.to_str().unwrap()` 那一类 —— Linux 上会 panic）。
//   * R4 出现 `cfg(windows)` 时，同一文件必须有 `cfg(unix)` / `cfg(not(windows))` / `cfg(any(`。
//   * R5 在**解释素材位置**的文件里（提到 asset_root / assetRoot），判定「绝对路径」
//        不许只看平台语义：`Path::is_absolute()` / `path.isAbsolute()` 在 Windows 上
//        认 `C:/x`、在 Linux 上不认 —— 而 asset.uri 是**跨宿主**的（同一份工程、两个宿主）。
//        只看平台会让 Windows 上写的 `C:/abs/b.mp4` 在 Linux 出片时被挂到 --asset-root
//        下面：不是报错，是**换个地方去找**。要按书写形态判，并落在具名 helper 上。
//
// # 两件事决定了这条守卫写起来的手感
//
// 一、**注释先抹掉**（`stripComments`）。本文件自己要讨论上面这些形状 —— 不抹的话守卫会去
//     举报自己的文档，而下一个人的修法通常是"把守则删掉"，守卫被删掉比误报更糟。
//     教训直接抄自 scripts/check-core-purity.mjs。所以：**注释里可以随便举例**，
//     判据字面量与自检样例则一律**拼出来**（字符码 / 跨行拼接）。
//     注意只能抹注释、**不能连着字符串一起抹**：这条守卫的判据本身就活在字符串字面量里。
//
// 二、**R1 按"抹掉空白后"的形状比**（`compact`）。仓库代码都被 prettier / rustfmt 排过，
//     真写错时长的是 `replace('/', '\')`（逗号后有空格），按原样比会**正好漏掉最该抓的一种**。
//
// 行尾统一 LF **不在这里**：它由 `scripts/check-text-hygiene.mjs` 覆盖（roadmap 原文如此）。
// 这条守卫只**交叉声明**那个载体还在，不重复实现 —— 两处各写一份，迟早分叉。
//
// # 这条守卫**不管**什么（写下来免得被当成万能的）
//
//   * **盘符字面量不禁**。本来打算禁，先量了一遍：整棵树有 12 个文件含 `C:/…`，
//     几乎全是**正当**的 —— 字体候选表（`C:/Windows/Fonts/msyh.ttc`）、
//     Chrome 候选表（同时列了 `/usr/bin/google-chrome`）、以及单测里当输入的假路径。
//     禁掉它们会造出一堆假红，而**假红比没有守卫更坏**（人会开始无视它）。
//   * **不判 mtime / 权限 / 符号链接**：那些要真在 Linux 上跑才说得清，属 D13 的范围。
//   * **R5 只管「解释素材位置」的地方**。静态文件的越界校验（`scripts/web-static.mjs`）、
//     记录路径的归属判断（`scripts/dhampir-framediff.mjs`）也在用平台判定，但那是
//     **本机自己的输入**，按平台语义判是对的 —— 一刀切会造出一批假红，而假红比没有守卫更坏。
//   * **不做交叉编译**：能不能装 `x86_64-unknown-linux-gnu` 与这台机器上的链接器有关，
//     能不能装、装了能不能 check，结论记在 plan/t7-evidence.md —— **不在这里假装跑过**。
//   * **词法不求完备**：`stripComments` 不单独处理正则字面量（`/[/*]/` 这种罕见写法会误判），
//     `compact` 理论上能把同一行上不相邻的片段粘成假红。真踩到再收紧，别为它们加机器。
//
// 用法：
//   node scripts/check-linux-portability.mjs             检查
//   node scripts/check-linux-portability.mjs --self-test 只跑守卫自检

import { existsSync, readFileSync, readdirSync, statSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

export const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');

// 反斜杠与两种引号**用字符码构造**。判据里这些形状直写会让守卫把自己判红（它扫的就是这些形状），
// 老规矩有两条：能用字符码就用字符码，剩下的一律**跨行拼**。这是从 scripts/check-core-purity.mjs
// 学来的（`CHECK_TEXT` 那一段）；自检最后一条断言把"守卫自己的源码必须判得干净"钉住。
const BS = String.fromCharCode(92); // \
const SQ = String.fromCharCode(39); // '
const DQ = String.fromCharCode(34); // "

// 正则形式的分隔符片段（`/\//`）。跨行拼出来，免得本文件里出现它。
const SLASH_RE = '/' + BS + '/';

/**
 * 把**注释**抹成空格（换行保留，所以行号不变）；**字符串字面量原样留着**。
 *
 * 为什么必须抹注释：本文件自己要讨论这些形状（上面那段文档就是例证）。不抹的话
 * 守卫会去举报自己的文档，而下一个人的修法通常是"把守则删掉" —— 守卫被删掉比守卫
 * 误报更糟。这条教训直接抄自 scripts/check-core-purity.mjs（见那里的 §为什么必须先剥注释）。
 *
 * 为什么**不能连着字符串一起抹**：这条守卫的判据本身就以字符串字面量为载体
 * （分隔符就是字符串字面量）。抹了字符串，判据就全瞎了。所以这里只认注释，
 * 而且必须认准 —— 得跳过字符串，才知道 `'https://x'` 里的 `//` 不是注释。
 *
 * 词法不求完备：认行注释、块注释、双引号串、单引号串（限同一行，避开 Rust 的 `&'a`）、
 * 反引号串。正则字面量不单独处理（`/[/*]/` 这种罕见写法会误判，写下来免得后面踩）。
 */
export function stripComments(source) {
  const out = Array.from(source);
  const n = source.length;
  const blank = (from, to) => {
    for (let k = from; k < to && k < n; k += 1) {
      if (out[k] !== '\n') out[k] = ' ';
    }
  };
  let i = 0;
  while (i < n) {
    const c = source[i];
    if (c === '/' && source[i + 1] === '/') {
      let j = source.indexOf('\n', i);
      if (j < 0) j = n;
      blank(i, j);
      i = j;
      continue;
    }
    if (c === '/' && source[i + 1] === '*') {
      const end = source.indexOf('*/', i + 2);
      const j = end < 0 ? n : end + 2;
      blank(i, j);
      i = j;
      continue;
    }
    if (c === DQ || c === '`') {
      let j = i + 1;
      while (j < n) {
        if (source[j] === BS) j += 2;
        else if (source[j] === c) {
          j += 1;
          break;
        } else j += 1;
      }
      i = j;
      continue;
    }
    if (c === SQ) {
      const close = source.indexOf(SQ, i + 1);
      const eol = source.indexOf('\n', i + 1);
      // 只认同一行内成对的引号；不成对的（Rust 生命周期）走过去就是。
      if (close >= 0 && (eol < 0 || close < eol)) i = close + 1;
      else i += 1;
      continue;
    }
    i += 1;
  }
  return out.join('');
}

/**
 * 抹掉行内所有空白，再拿去做 R1 的匹配。
 *
 * 起因是个真窟窿：判据原本按**原样字符串**比 `replace('/','\')`，但这仓库的代码
 * 都被 prettier/rustfmt 格式化过 —— 真写错时是 `replace('/', '\')`（逗号后有空格），
 * 于是判据**正好漏掉最该抓的那一种**。抹空白之后两种都抓得到。
 *
 * 代价：同一行上本来不相邻的片段会被粘到一起，理论上能造出假红。真遇到再收紧。
 */
export function compact(line) {
  return line.replace(/\s+/g, '');
}

/** R1：把正斜杠**换成**反斜杠的几种写法。**全部是去掉空白后的形状。** */
export const WINDOWS_ONLY_SEPARATOR = [
  'replace(' + SQ + '/' + SQ + ',' + SQ + BS + BS + SQ + ')',
  'replace(' + DQ + '/' + DQ + ',' + DQ + BS + BS + DQ + ')',
  'replace(' + SLASH_RE + 'g,' + SQ + BS + BS + SQ + ')',
  'replace(' + SLASH_RE + ',' + SQ + BS + BS + SQ + ')',
  'split(' + SQ + '/' + SQ + ').join(' + SQ + BS + BS + SQ + ')',
  'push(' + SQ + BS + BS + SQ + ')',
  'push(' + DQ + BS + BS + DQ + ')',
  'join(' + SQ + BS + BS + SQ + ')',
  'join(' + DQ + BS + BS + DQ + ')',
];

/** R1 的反向：**这是对的**，必须放过。`\` -> `/` 才是可移植的方向。 */
export const PORTABLE_SEPARATOR = [
  'replace(' + SQ + BS + BS + SQ + ',' + SQ + '/' + SQ + ')',
  'replace(' + DQ + BS + BS + DQ + ',' + DQ + '/' + DQ + ')',
  'split(' + SQ + BS + BS + SQ + ').join(' + SQ + '/' + SQ + ')',
];

/** R2：小写化。 */
export const LOWERCASE_TOKENS = ['toLowerCase()', 'to_ascii_lowercase()', 'to_lowercase()'];

/** R2：文件系统访问。与小写化**同一行**出现才算。 */
export const FS_TOKENS = [
  'existsSync',
  'readFileSync',
  'readdirSync',
  'statSync',
  'openSync',
  'std::fs::',
  'File::open',
  'fs::read',
];

// R3：假定路径是 UTF-8。
// 判据字面量**拆开拼**：直写会让守卫把自己判红（它扫的就是这些形状）——
// 与上面 WINDOWS_ONLY_SEPARATOR 用字符码构造是同一个道理。自检最后一条断言把这件事钉住。
const TO_STR = '.to_str()';
export const UTF8_ASSUMPTION = [TO_STR + '.unwrap()', TO_STR + '.expect('];

/** R4：平台条件编译。 */
export const WINDOWS_CFG = 'cfg(windows)';
export const OTHER_PLATFORM_CFG = ['cfg(unix)', 'cfg(not(windows))', 'cfg(any(', 'cfg(target_os'];

/**
 * R5：**解释素材位置**的地方，判定「绝对路径」必须按**书写形态**，不许只看平台语义。
 *
 * 平台语义与书写形态在 Windows 上是重合的，在 Linux 上不是 —— 也正是**在 Windows 上
 * 永远不会自己变红**的那一类。判据限定在「提到 asset_root / assetRoot」的文件里：
 * 别处（静态文件越界校验、记录路径归属判断）用的是**本机自己的输入**，按平台判是对的，
 * 一刀切会造假红，而假红比没有守卫更坏。
 *
 * 记号同样**拆开拼**：直写会让守卫举报自己（它扫的就是 scripts/ 与 crates/）。
 */
export const PLATFORM_ABSOLUTE_TOKENS = ['is' + 'Absolute(', '.is' + '_absolute()'];
export const ASSET_ROOT_TOKENS = ['asset_root', 'assetRoot'];
export const CROSS_PLATFORM_ABSOLUTE_TOKENS = ['is' + '_absolute_uri', 'is' + 'AbsoluteUri'];

/** 要扫哪些文件。**两种语言分开列**，因为判据不完全一样。 */
export function scannedFiles() {
  const rust = [];
  const script = [];
  const walk = (dir, sink, extensions) => {
    for (const entry of readdirSync(dir, { withFileTypes: true })) {
      const path = join(dir, entry.name);
      if (entry.isDirectory()) {
        // 构建产物与依赖不扫：它们不是我们写的。
        if (['node_modules', 'target', 'pkg', '.git'].includes(entry.name)) continue;
        walk(path, sink, extensions);
      } else if (extensions.some((ext) => entry.name.endsWith(ext))) {
        sink.push(path);
      }
    }
  };
  for (const root of ['crates']) {
    const abs = join(REPO_ROOT, root);
    if (existsSync(abs)) walk(abs, rust, ['.rs']);
  }
  for (const root of ['scripts', 'web', 'schema']) {
    const abs = join(REPO_ROOT, root);
    if (existsSync(abs)) walk(abs, script, ['.mjs', '.js', '.cjs', '.ts']);
  }
  return { rust, script };
}

/**
 * 判定。抽成纯函数，便于喂**故意坏的**输入验证它真的会红。
 *
 * 入参形状：`{ rust: [[名字, 文本]], script: [[名字, 文本]] }`。
 */
export function judge(sources) {
  const problems = [];

  for (const [kind, files] of Object.entries(sources)) {
    for (const [name, raw] of files) {
      // 先抹注释。被扫的源码（以及本文件自己）在注释里**讨论**这些形状是合法的。
      const bare = stripComments(raw);
      bare.split('\n').forEach((line, index) => {
        const at = name + ':' + (index + 1);
        const tight = compact(line);
        // --- R1 ---
        for (const bad of WINDOWS_ONLY_SEPARATOR) {
          if (tight.includes(bad)) {
            problems.push(at + ' 把路径分隔符写成了只有 Windows 认的样子（' + bad + '）—— ' +
              '可移植的方向是反过来的：把 ' + BS + BS + ' 规范化成 /');
          }
        }
        // --- R2 ---
        const lower = LOWERCASE_TOKENS.find((token) => line.includes(token));
        const fs = FS_TOKENS.find((token) => line.includes(token));
        if (lower !== undefined && fs !== undefined) {
          problems.push(at + ' 拿小写化的路径去访问文件系统（' + lower + ' + ' + fs + '）—— ' +
            'Linux 的文件系统区分大小写，这样写只会在别的机器上找不到文件');
        }
        // --- R3 ---
        for (const bad of UTF8_ASSUMPTION) {
          if (line.includes(bad)) {
            problems.push(at + ' 假定路径是合法 UTF-8（' + bad + '）—— ' +
              'Linux 上文件名可以是非 UTF-8 字节，这会 panic；请用 to_string_lossy()');
          }
        }
      });
      // --- R4：整份文件看，因为属性与分支常常不相邻 ---
      if (kind === 'rust' && bare.includes(WINDOWS_CFG)) {
        const hasOther = OTHER_PLATFORM_CFG.some((token) => bare.includes(token));
        if (!hasOther) {
          problems.push(name + ' 有 ' + WINDOWS_CFG + ' 却没有另一平台的分支 —— ' +
            '在 Linux 上那个东西**根本不存在**（补 cfg(unix) / cfg(not(windows))）');
        }
      }

      // --- R5：解释素材位置的地方，绝对路径判定要按书写形态 ---
      const platformJudge = PLATFORM_ABSOLUTE_TOKENS.find((token) => bare.includes(token));
      if (platformJudge !== undefined && ASSET_ROOT_TOKENS.some((token) => bare.includes(token))) {
        const hasHelper = CROSS_PLATFORM_ABSOLUTE_TOKENS.some((token) => bare.includes(token));
        if (!hasHelper) {
          problems.push(name + ' 在解释素材位置的地方只用平台语义判「绝对路径」（' + platformJudge +
            '）—— Windows 上写的 C:/abs/a.mp4 在 Linux 上会被当成相对路径、挂到 asset_root 下面；' +
            '要按书写形态判，并落在具名 helper 上（本仓两处：Rust is_absolute_uri / JS isAbsoluteUri）');
        }
      }
    }
  }

  // **拒绝在空集上通过** —— 与仓库其他守卫同一纪律。
  const total = (sources.rust || []).length + (sources.script || []).length;
  if (total === 0) {
    problems.push('一个源文件都没扫到 —— 守卫拒绝在空集上通过（路径改了吗？）');
  }
  return problems;
}

/** 行尾那条判据的载体还在不在（**交叉声明**，不重复实现）。 */
export function hygieneCarrierProblems() {
  const problems = [];
  const path = join(REPO_ROOT, 'scripts', 'check-text-hygiene.mjs');
  if (!existsSync(path)) {
    problems.push('行尾/编码那条判据的载体不见了：scripts/check-text-hygiene.mjs —— ' +
      '它本该覆盖 LF / BOM / UTF-8，这条守卫不重复实现它');
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
  const rust = (text) => ({ rust: [['a.rs', text]], script: [] });
  const script = (text) => ({ rust: [], script: [['a.mjs', text]] });
  const nothing = { rust: [['a.rs', 'fn main() {}']], script: [['a.js', 'const x = 1;']] };

  // 干净的代码 -> 通过（而且不是空集）。
  expect('干净的代码 -> 通过', judge(nothing), true);

  // --- R1 正例（必须红）与反例（必须放过）---
  const toWindows = 'const p = dir.replace(' + SQ + '/' + SQ + ',' + SQ + BS + BS + SQ + ');';
  expect('replace(/ -> \\) -> 必须红', judge(rust(toWindows)), false);
  expect('同上（脚本侧）也必须红', judge(script(toWindows)), false);

  const joinBackslash = 'const p = parts.join(' + SQ + BS + BS + SQ + ');';
  expect('拿反斜杠 join -> 必须红', judge(script(joinBackslash)), false);

  const toPosix = 'const p = dir.replace(' + SQ + BS + BS + SQ + ',' + SQ + '/' + SQ + ');';
  expect('replace(\\ -> /) -> **必须放过**（这才是可移植的方向）', judge(rust(toPosix)), true);

  // 判据必须**容忍格式化器加的空格**。仓库代码都被 prettier / rustfmt 排过，真写错时
  // 长的是 `replace('/', '\')`（逗号后有空格）。原来按原样字符串比，正好漏掉这一种 ——
  // 即"最该抓的那一种"，这是被一次手工试写发现的真窟窿。
  const spaced = 'const p = dir.replace(' + SQ + '/' + SQ + ', ' + SQ + BS + BS + SQ + ');';
  expect('带空格的分隔符替换（格式化后的真实形状）-> 必须红', judge(script(spaced)), false);

  // 正则形式也算（`/\//` -> `\` 同样是只有 Windows 认的写法）。
  const regexForm = 'const p = dir.replace(' + SLASH_RE + 'g, ' + SQ + BS + BS + SQ + ');';
  expect('正则形式的分隔符替换 -> 必须红', judge(script(regexForm)), false);

  // 反向的 `.split('\').join('/')` 是本仓真在用的写法（见 scripts/record-acceptance.mjs）。
  const splitPosix = 'const p = path.split(' + SQ + BS + BS + SQ + ').join(' + SQ + '/' + SQ + ');';
  expect('split(\\).join(/) -> 放过（本仓真在这么用）', judge(script(splitPosix)), true);

  // --- 注释剥除（这条机制本身的用例）---
  const commentOnly = '// dir.replace(' + SQ + '/' + SQ + ',' + SQ + BS + BS + SQ + ');';
  expect('行注释里讨论这些形状 -> 放过（否则守卫会举报自己的文档）', judge(script(commentOnly)), true);
  const blockOnly = '/* ' + 'dir.replace(' + SQ + '/' + SQ + ',' + SQ + BS + BS + SQ + ') */';
  expect('块注释里同理 -> 放过', judge(script(blockOnly)), true);
  // 反过来：字符串里的 `//` **不能**被当成注释，否则同一行的真违规会被它挡瞎。
  const urlThenViolation = 'const u = ' + SQ + 'https://x' + SQ + '; const p = dir.replace(' +
    SQ + '/' + SQ + ',' + SQ + BS + BS + SQ + ');';
  expect('串里的 // 不算注释（判据不能被挡瞎）-> 必须红', judge(script(urlThenViolation)), false);

  // --- R2 ---
  // 样例文本也必须**跨行拼**：否则"小写化 + 文件系统调用"会同现在守卫自己这一行上，
  // 真跑时守卫判自己红（踩过一次）。
  const lowerOpen = 'const p = resolve(base, name.' + 'toLowerCase())' +
    '; if (existsSync(' + 'p)) {}';
  expect('小写化整条路径再 existsSync -> 必须红', judge(script(lowerOpen)), false);
  const lowerExtOnly = "const kind = MIME[extname(file).toLowerCase()];";
  expect('只对扩展名小写化 -> 放过（本仓到处这么用）', judge(script(lowerExtOnly)), true);

  // --- R3 ---
  const utf8 = 'let s = path' + TO_STR + '.unwrap();';
  expect('to_str().unwrap() -> 必须红', judge(rust(utf8)), false);
  const lossy = 'let s = path.to_string_lossy();';
  expect('to_string_lossy() -> 放过', judge(rust(lossy)), true);

  // --- R4 ---
  const loneWindows = '#[cfg(windows)]\nfn console_attach() {}';
  expect('孤立的 cfg(windows) -> 必须红', judge(rust(loneWindows)), false);
  const paired = '#[cfg(windows)]\nfn a() {}\n#[cfg(unix)]\nfn a() {}';
  expect('有配对分支 -> 放过', judge(rust(paired)), true);
  const negated = '#[cfg(windows)]\nfn a() {}\n#[cfg(not(windows))]\nfn a() {}';
  expect('cfg(not(windows)) 也算配对', judge(rust(negated)), true);

  // --- R5 ---
  const assetAbs = 'const p = is' + 'Absolute(uri) ? uri : join(assetRoot, uri);';
  expect('素材位置只用平台语义判绝对 -> 必须红', judge(script(assetAbs)), false);
  const assetAbsHelper = 'export function is' + 'AbsoluteUri(u) { return is' + 'Absolute(u); }\n' + assetAbs;
  expect('同文件里有跨平台 helper -> 放过', judge(script(assetAbsHelper)), true);
  const hostLocal = 'const ok = rel === ' + SQ + SQ + ' || is' + 'Absolute(rel);';
  expect('本机自己的输入（不提 asset_root）-> 放过，别造假红', judge(script(hostLocal)), true);
  const rustAssetAbs = 'if raw.is' + '_absolute() { asset_root.join(raw) }';
  expect('Rust 侧在素材位置只用平台语义 -> 必须红', judge(rust(rustAssetAbs)), false);
  const rustAssetHelper = 'fn is' + '_absolute_uri(p: &Path) -> bool { p.is' + '_absolute() }\n' + rustAssetAbs;
  expect('Rust 侧有跨平台 helper -> 放过', judge(rust(rustAssetHelper)), true);

  // --- 空集纪律 ---
  expect('空集 -> 必须红（不能空转）', judge({ rust: [], script: [] }), false);

  // --- 载体交叉声明 ---
  expect('行尾那条判据的载体在（本仓一定有）', hygieneCarrierProblems(), true);

  // --- 守卫自己必须判得干净 ---
  // 判据字面量与自检样例会装着这些形状，所以必须**拼出来**（字符码 / 跨行拼接）。
  // 注释不受这条约束 —— 判定前注释就被抹掉了，所以文档里可以大大方方举例。
  // 这条断言的价值在于：谁把形状**直写进代码或样例**，自检当场变红，而不是等真跑时
  // 冒出一句莫名其妙的"守卫举报自己"。
  const own = { rust: [], script: [['check-linux-portability.mjs', readFileSync(fileURLToPath(import.meta.url), 'utf8')]] };
  expect('守卫自己的源码必须判得干净（代码与样例里的形状都得拼出来）', judge(own), true);

  console.log('✓ Linux 可移植性守卫自检通过（' + passed + ' 条断言）');
}

function main() {
  const argv = process.argv.slice(2);
  if (argv.includes('--help')) {
    console.log('用法：node scripts/check-linux-portability.mjs [--self-test]');
    return;
  }
  if (argv.includes('--self-test')) {
    try {
      runSelfTest();
    } catch (error) {
      console.error(String(error && error.message ? error.message : error));
      console.error('先修守卫，别信它的结论');
      process.exitCode = 2;
    }
    return;
  }
  const { rust, script } = scannedFiles();
  const read = (list) => list.map((path) => [path.replace(REPO_ROOT + BS, '').split(BS).join('/'), readFileSync(path, 'utf8')]);
  const sources = { rust: read(rust), script: read(script) };
  const problems = judge(sources);
  problems.push(...hygieneCarrierProblems());
  if (problems.length > 0) {
    for (const problem of problems) console.error('  - ' + problem);
    console.error('Linux 可移植性判据不成立');
    process.exitCode = 1;
    return;
  }
  const total = sources.rust.length + sources.script.length;
  console.log('✓ Linux 可移植性判据成立（扫描 ' + total + ' 个源文件：' +
    sources.rust.length + ' rs + ' + sources.script.length + ' js/ts；' +
    '行尾 LF 由 check-text-hygiene 覆盖）');
}

main();
