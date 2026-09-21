#!/usr/bin/env node
// dhampir-core 纯净性守卫：本 crate 里不许出现平台分叉。
//
// 规则（指导文档 §4.4，plan §3 T0.2/风险表）：`dhampir-core/src` 下不出现任何
// `#[cfg` / `#[cfg_attr` / `cfg!(`，唯一豁免是 `#[cfg(test)]`。
//
// 理由不是审美：一旦 core 里出现 `#[cfg(target_arch = "wasm32")]`，就有了
// "服务端版渲染"和"浏览器版渲染"两个实现，两端一致性从**架构保证**降级成
// **人工纪律**，而人工纪律在半年后一定失效。
//
// ---------------------------------------------------------------------------
// 为什么是 Node 而不是 PowerShell
//
// Windows PowerShell 5.1 会按系统 ANSI 代码页解析**无 BOM** 的 .ps1 文件。
// 本仓库的约定是全仓 LF + 无 BOM，于是任何带中文的 .ps1 在这台机器上会直接
// **语法错误**（不是显示乱码——是脚本根本跑不起来，连 `--help` 都到不了）。
// Node 一律按 UTF-8 读源码，中文注释与输出都没有这个风险，而且 CI 的
// ubuntu-latest / windows-latest 都自带 Node。
//
// ---------------------------------------------------------------------------
// 为什么必须先剥注释与字符串
//
// 本文件要检查的东西，在源码里**被讨论过**：`dhampir-core/src/lib.rs` 的模块
// 文档里写着"本 crate 内不出现任何 `#[cfg]`"，还举了 `#[cfg(target_arch =
// "wasm32")]` 当反例。不剥注释，守卫会去举报自己的文档；而下一个人的修法通常
// 是"把守则删掉"——守卫被删掉，比守卫误报更糟。
//
// 所以：先把注释与字符串字面量**原地抹成空格**（换行保留），再扫描。
// 抹成空格而不是删除，是为了让行号列号仍然对得上原始文件。
//
// 用法：
//   node scripts/check-core-purity.mjs
//   node scripts/check-core-purity.mjs --self-test   # 只跑守卫自己的自检

import { readdirSync, readFileSync } from 'node:fs';
import { dirname, join, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const CORE_SRC = join(REPO_ROOT, 'crates', 'dhampir-core', 'src');

/** 允许的写法。除了它，任何 cfg 都是分叉。 */
const EXEMPT = '#[cfg(test)]';

/**
 * 把注释与字符串字面量抹成空格，保留下来的换行让行号不变。
 *
 * 支持：双斜杠行注释、斜杠星号块注释（Rust 允许嵌套）、普通字符串、字节字符串、
 * 原始字符串（`r` 或 `br`，后跟若干 `#` 与引号）、字符字面量。
 * 不追求完备的 Rust 词法——只求"不会把注释/字符串里的 cfg 当成代码"。
 */
export function stripNonCode(source) {
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
      let depth = 1;
      let j = i + 2;
      while (j < n && depth > 0) {
        if (source[j] === '/' && source[j + 1] === '*') {
          depth += 1;
          j += 2;
        } else if (source[j] === '*' && source[j + 1] === '/') {
          depth -= 1;
          j += 2;
        } else {
          j += 1;
        }
      }
      blank(i, j);
      i = j;
      continue;
    }

    // 原始字符串：r"..." / r#"..."# / br#"..."#
    if (c === 'r' || c === 'b') {
      let j = c === 'b' ? i + 1 : i;
      if (source[j] === 'r') {
        j += 1;
        let hashes = 0;
        while (source[j] === '#') {
          hashes += 1;
          j += 1;
        }
        if (source[j] === '"') {
          const closer = `"${'#'.repeat(hashes)}`;
          const end = source.indexOf(closer, j + 1);
          j = end < 0 ? n : end + closer.length;
          blank(i, j);
          i = j;
          continue;
        }
      }
    }

    if (c === '"') {
      let j = i + 1;
      while (j < n) {
        if (source[j] === '\\') j += 2;
        else if (source[j] === '"') {
          j += 1;
          break;
        } else j += 1;
      }
      blank(i, j);
      i = j;
      continue;
    }

    // 字符字面量。生命周期 `'a` 没有闭合引号，正则不会命中，也就不会被误抹。
    if (c === "'") {
      const m = /^'(?:\\.|\\u\{[0-9a-fA-F]+\}|[^'\\])'/.exec(source.slice(i, i + 12));
      if (m) {
        blank(i, i + m[0].length);
        i += m[0].length;
        continue;
      }
    }

    i += 1;
  }

  return out.join('');
}

/**
 * 在**已剥注释**的文本里找出违例。
 *
 * 返回 `[{ line, column, text }]`，`line` 从 1 开始，`text` 是原始那一行。
 */
export function findViolations(stripped, originalLines) {
  const violations = [];
  const lines = stripped.split('\n');
  for (let index = 0; index < lines.length; index += 1) {
    const line = lines[index];
    const trimmed = line.trim();
    if (trimmed.length === 0) continue;

    const hasAttr = /#\s*\[\s*cfg\b/.test(line) || /#\s*\[\s*cfg_attr\b/.test(line);
    const hasMacro = /\bcfg!/.test(line);
    if (!hasAttr && !hasMacro) continue;

    // 豁免：这一行（去掉首尾空白后）就是 #[cfg(test)]，没有别的东西。
    const withoutExempt = trimmed.split(EXEMPT).join('').trim();
    if (withoutExempt.length === 0) continue;

    const column = line.search(/\S/);
    violations.push({
      line: index + 1,
      column: column < 0 ? 1 : column + 1,
      text: (originalLines[index] ?? '').trimEnd(),
    });
  }
  return violations;
}

/** 守卫自己的自检。守卫要是坏的，"全绿"就没有意义。 */
export const SELF_TEST_CASES = [
  { name: '行注释里的 cfg 不算', src: 'let a = 1; // #[cfg(target_arch = "wasm32")]\n', expect: 0 },
  { name: '块注释里的 cfg 不算', src: '/* #[cfg(unix)] */\nlet b = 2;\n', expect: 0 },
  { name: '嵌套块注释里的 cfg 不算', src: '/* 外层 /* 内层 #[cfg(unix)] */ 仍在注释里 */\n', expect: 0 },
  { name: '字符串里的 cfg 不算', src: 'const U: &str = "https://example.com/#[cfg]";\n', expect: 0 },
  { name: '原始字符串里的 cfg 不算', src: 'const S: &str = r#"#[cfg(unix)]"#;\n', expect: 0 },
  { name: '字面量里的 cfg 不算', src: 'let x = cfg!(unix); // 注释\n', expect: 1 },
  { name: '真正的属性要报出来', src: '#[cfg(target_arch = "wasm32")]\nfn f() {}\n', expect: 1 },
  { name: 'cfg_attr 也要报出来', src: '#[cfg_attr(feature = "serde", derive(Clone))]\n', expect: 1 },
  { name: '宏形式要报出来', src: 'if cfg!(target_arch = "wasm32") {}\n', expect: 1 },
  { name: '#[cfg(test)] 豁免', src: '#[cfg(test)]\nmod tests {}\n', expect: 0 },
  { name: '豁免只针对这一行', src: '#[cfg(test)]\n#[cfg(unix)]\nmod t {}\n', expect: 1 },
];

function runSelfTest() {
  const failures = [];
  for (const testCase of SELF_TEST_CASES) {
    const stripped = stripNonCode(testCase.src);
    const violations = findViolations(stripped, testCase.src.split('\n'));
    if (violations.length !== testCase.expect) {
      failures.push(
        `自检「${testCase.name}」期望 ${testCase.expect} 处，实际 ${violations.length} 处`,
      );
    }
  }
  return failures;
}

function collectRustFiles(dir) {
  const found = [];
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const full = join(dir, entry.name);
    if (entry.isDirectory()) found.push(...collectRustFiles(full));
    else if (entry.isFile() && entry.name.endsWith('.rs')) found.push(full);
  }
  found.sort();
  return found;
}

function main() {
  // 参数必须先判死。这条是被一次反向探针逼出来的：那时 unknown 参数被默默忽略、
  // 退出码 0，于是"喂给守卫一个坏参数"竟然算通过——一条永远绿的路径。
  const unknown = process.argv.slice(2).filter((a) => a !== '--self-test' && a !== '-h' && a !== '--help');
  if (unknown.length > 0) {
    console.error(`✗ 不认识的参数：${unknown.join(' ')}`);
    console.error('  用法：node scripts/check-core-purity.mjs [--self-test]');
    return 2;
  }
  if (process.argv.includes('-h') || process.argv.includes('--help')) {
    console.log('用法：node scripts/check-core-purity.mjs [--self-test]');
    return 0;
  }

  const selfTestFailures = runSelfTest();
  if (selfTestFailures.length > 0) {
    console.error('✗ 守卫自检失败——先修守卫，别信它的结论：');
    for (const failure of selfTestFailures) console.error(`  - ${failure}`);
    return 2;
  }
  if (process.argv.includes('--self-test')) {
    console.log(`✓ 守卫自检通过（${SELF_TEST_CASES.length} 条用例）`);
    return 0;
  }

  let files;
  try {
    files = collectRustFiles(CORE_SRC);
  } catch (error) {
    console.error(`✗ 读不到 dhampir-core 源码目录 ${CORE_SRC}：${error.message}`);
    return 2;
  }

  // 空集合上宣布"全绿"是自欺：多半是路径写错了。
  if (files.length === 0) {
    console.error(`✗ ${CORE_SRC} 下没有找到任何 .rs 文件——路径是不是变了？拒绝在空集合上通过。`);
    return 2;
  }

  const allViolations = [];
  for (const file of files) {
    const source = readFileSync(file, 'utf8');
    const stripped = stripNonCode(source);
    for (const violation of findViolations(stripped, source.split('\n'))) {
      allViolations.push({ file: relative(REPO_ROOT, file).split('\\').join('/'), ...violation });
    }
  }

  if (allViolations.length > 0) {
    console.error(`✗ dhampir-core 里出现了平台分叉（扫了 ${files.length} 个文件）：`);
    for (const violation of allViolations) {
      console.error(`  ${violation.file}:${violation.line}:${violation.column}: ${violation.text}`);
    }
    console.error('');
    console.error('  core 里只有 #[cfg(test)] 是允许的。平台差异请用 Cargo 的');
    console.error('  target-specific 依赖表达（见 crates/dhampir-wasm/Cargo.toml）。');
    return 1;
  }

  console.log(`✓ dhampir-core 纯净：${files.length} 个文件里没有 #[cfg] / cfg!（豁免仅 #[cfg(test)]）`);
  return 0;
}

// 只设 process.exitCode，不调 process.exit()——见 scripts/wasm-test-node-exit-shim.cjs：
// 在本机的 Node/Windows 上，真正被执行的 process.exit() 可能撞上 libuv 的
// UV_HANDLE_CLOSING 断言，退出码变成负数（-1073740791）。守卫的退出码是它的
// 唯一结论，所以退出码本身不能是个概率事件。自然 drain 退出没有任何代价。
process.exitCode = main();
