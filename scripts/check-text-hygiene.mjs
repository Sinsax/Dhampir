#!/usr/bin/env node
// 文本卫生守卫：全仓 LF + 无 BOM + 合法 UTF-8。
//
// 为什么需要它：这两条约定**已经**在本机被踩过两次——
//
// 1. `scripts/check-core-purity.ps1` 原来用 PowerShell 5.1 写成带中文的 .ps1，
//    而 PS 5.1 按系统 ANSI 代码页解析**无 BOM** 的 .ps1，于是脚本直接语法错误。
//    换句话说："无 BOM"这条约定本身会让 PS 脚本坏掉——这不是美学问题。
// 2. Windows 上的编辑器与 `Set-Content` 默认写 CRLF/带 BOM。一旦某个 .rs 或
//    golden 文本被它们碰一下，逐字节比对就会在**另一个**平台上红掉，
//    而那时你人在别的机器上，看到的是一个"我什么都没改"的失败。
//
// golden 文件（crates/dhampir-timeline/tests/golden/）尤其经不起这个：
// 它的全部意义就是"逐字节相等"，多一个 0x0D 就整条验收作废。
//
// 用法：
//   node scripts/check-text-hygiene.mjs
//   node scripts/check-text-hygiene.mjs --self-test

import { readdirSync, readFileSync, mkdirSync, mkdtempSync, writeFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, relative, resolve, extname, basename } from 'node:path';
import { fileURLToPath } from 'node:url';

const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');

// 只看**应当**是文本的文件。二进制产物（PNG/wasm/视频）不在管辖范围内——
// 对它们说"不许有 0x0D"是无意义的。
const TEXT_EXTENSIONS = new Set([
  '.rs', '.toml', '.md', '.json', '.mjs', '.js', '.cjs', '.ps1', '.sh',
  '.txt', '.yml', '.yaml', '.html', '.css', '.ts', '.tsx', '.svg', '.gitignore',
]);

// 没有扩展名但仍然是文本的约定文件。
const TEXT_BASENAMES = new Set(['.gitignore', '.gitattributes', '.editorconfig', 'rust-toolchain.toml']);

const SKIP_DIRS = new Set(['target', 'node_modules', '.git', '.tools', 'dist', 'pkg', 'records']);

const BOM = [0xef, 0xbb, 0xbf];
const CR = 0x0d;

/**
 * 严格 UTF-8 解码器。`fatal: true` 让非法字节抛异常，而不是替换成 U+FFFD。
 *
 * 为什么要查这一条：README 里一直写着"全仓 LF + 无 BOM + 合法 UTF-8"，但守卫
 * 一开始只查前两条——**文档比守卫多说了话**。一个 Latin-1 存的中文注释既没有 CR
 * 也没有 BOM，会一路绿到某天有人用 UTF-8 打开它、看到一串问号。
 * 只对已经在管的文本扩展名生效，所以不会误伤 PNG/wasm。
 */
const UTF8_STRICT = new TextDecoder('utf-8', { fatal: true });

function isTextFile(name) {
  if (TEXT_BASENAMES.has(name)) return true;
  return TEXT_EXTENSIONS.has(extname(name).toLowerCase());
}

/**
 * 检查一份字节流。返回问题列表（空数组 = 干净）。
 *
 * 导出出来是为了自检——守卫自己的判定逻辑必须能被单独验证。
 */
export function inspectBytes(bytes) {
  const problems = [];

  if (bytes.length >= 3 && bytes[0] === BOM[0] && bytes[1] === BOM[1] && bytes[2] === BOM[2]) {
    problems.push('开头有 UTF-8 BOM（EF BB BF）');
  }

  const firstCr = bytes.indexOf(CR);
  if (firstCr >= 0) {
    // 找出这个 '\r' 在第几行，报出来才能直接跳过去修。
    let line = 1;
    for (let i = 0; i < firstCr; i += 1) if (bytes[i] === 0x0a) line += 1;
    const count = bytes.filter((b) => b === CR).length;
    problems.push(`有 ${count} 个 CR（第一个在第 ${line} 行、字节偏移 ${firstCr}）`);
  }

  try {
    UTF8_STRICT.decode(bytes);
  } catch {
    problems.push('不是合法 UTF-8（有非法字节序列）');
  }

  return problems;
}

/**
 * 由「扫到几个文件」+「哪些文件违规」得出退出码。空集合**不是**全绿。
 *
 * 为什么把它抽成可导出的函数：这段判定就是守卫的全部结论，而它最危险的失效
 * 模式是「在空集合上宣布全绿」——那样路径写错、扫描器坏掉、目录改名，都会
 * 得到一条 ✓。逻辑留在 main() 里就没法单测（和 scripts/record-acceptance.mjs
 * 的 judgeResult 抽出来是同一个理由）。
 *
 * 退出码口径（根 README §守卫脚本：三者的共同纪律）：
 *   2 = 环境/输入错——"没扫到"跟"扫过了没问题"是两件事，拒绝通过；
 *   1 = 扫到了文件，但内容违规（CR / BOM / 非法 UTF-8）；
 *   0 = 扫过，且干净。
 *
 * 注意：`files.length === 0` 在**本仓库**里几乎走不到，因为本脚本自己是 .mjs、
 * 自己也算"可查文件"。这条分支的价值不在本仓库当场触发，而在于"扫描根被人改
 * 到别处"时不会静默变绿——所以它必须有单测（见 SCAN_SELF_TESTS）。
 */
export function decideScan(files, offenders) {
  if (files.length === 0) return { exitCode: 2, verdict: 'empty' };
  if (offenders.length > 0) return { exitCode: 1, verdict: 'violations' };
  return { exitCode: 0, verdict: 'clean' };
}

// ---------------------------------------------------------------------------
// 自检
// ---------------------------------------------------------------------------

export const SELF_TEST_CASES = [
  { name: '纯 LF 无 BOM 通过', bytes: Buffer.from('a\nb\n', 'utf8'), expect: 0 },
  { name: 'CRLF 报出来', bytes: Buffer.from('a\r\nb\r\n', 'utf8'), expect: 1 },
  { name: '孤立 CR 报出来', bytes: Buffer.from('a\rb', 'utf8'), expect: 1 },
  { name: 'BOM 报出来', bytes: Buffer.concat([Buffer.from([0xef, 0xbb, 0xbf]), Buffer.from('a\n')]), expect: 1 },
  { name: 'BOM + CRLF 报两处', bytes: Buffer.concat([Buffer.from([0xef, 0xbb, 0xbf]), Buffer.from('a\r\n')]), expect: 2 },
  { name: '空文件通过', bytes: Buffer.alloc(0), expect: 0 },
  // 这条原本写的是 `EF 41 0A`——0xEF 是 3 字节序列的开头，后面跟 0x41 本来就非法。
  // 加了 UTF-8 校验之后它立刻变红，说明**样本自己**有问题，不是守卫有问题。
  // 改成 `EF 80 80 0A`（U+F000，合法且以 0xEF 开头），继续钉住"别把 0xEF 当成 BOM"。
  { name: '以 0xEF 开头但不是 BOM 不误报', bytes: Buffer.from([0xef, 0x80, 0x80, 0x0a]), expect: 0 },
  { name: '中文字节不误报', bytes: Buffer.from('中文\n', 'utf8'), expect: 0 },
  // UTF-8 合法性：这两条是"文档比守卫多说了一句话"的直接后果，补上才有资格写进 README。
  { name: '孤立续字节 0x80 报出来', bytes: Buffer.from([0x41, 0x80, 0x0a]), expect: 1 },
  { name: '被截断的多字节序列报出来', bytes: Buffer.from([0xe4, 0xb8]), expect: 1 },
  { name: 'Latin-1 存的"中文"（无 CR 无 BOM）报出来', bytes: Buffer.from([0xd6, 0xd0, 0xce, 0xc4, 0x0a]), expect: 1 },
  { name: '合法的 4 字节序列不误报', bytes: Buffer.from('🎬\n', 'utf8'), expect: 0 },
];

/**
 * 退出码判定本身的自检。
 *
 * 这一组是 README「不在空文件集上通过」那句承诺的**唯一**钉子：去掉它，判定就可
 * 以被悄悄改成"空集合也算绿"而没人发现。
 */
export const SCAN_SELF_TESTS = [
  { name: '空集合判 2（拒绝通过，不是全绿）', files: [], offenders: [], expect: 2 },
  { name: '扫到文件且干净判 0', files: ['README.md'], offenders: [], expect: 0 },
  {
    name: '扫到文件但有违规判 1（与空集合的 2 区分开）',
    files: ['README.md'],
    offenders: [{ file: 'README.md', problems: ['有 1 个 CR'] }],
    expect: 1,
  },
];

function runSelfTest() {
  const failures = [];
  let diskCases = 0;
  for (const testCase of SELF_TEST_CASES) {
    const problems = inspectBytes(testCase.bytes);
    if (problems.length !== testCase.expect) {
      failures.push(`自检「${testCase.name}」期望 ${testCase.expect} 处，实际 ${problems.length} 处：${problems.join('；')}`);
    }
  }

  for (const testCase of SCAN_SELF_TESTS) {
    const { exitCode } = decideScan(testCase.files, testCase.offenders);
    if (exitCode !== testCase.expect) {
      failures.push(`退出码自检「${testCase.name}」期望 ${testCase.expect}，实际 ${exitCode}`);
    }
  }

  // 再验一次"读文件"这条路径——上面都是内存里的字节流。
  const dir = mkdtempSync(join(tmpdir(), 'dhampir-hygiene-'));
  try {
    const crlfPath = join(dir, 'dirty.txt');
    writeFileSync(crlfPath, 'a\r\nb\r\n');
    const problems = inspectBytes(readFileSync(crlfPath));
    diskCases += 1;
    if (problems.length !== 1) {
      failures.push(`自检「从磁盘读 CRLF 文件」期望 1 处，实际 ${problems.length} 处`);
    }

    // UTF-8 那条也得走一遍磁盘：它和 CRLF 一样，是"文件里多/少了几个字节"的事，
    // 只在内存里验，就漏掉了"读回来的 Buffer 和写进去的不一样"这类错。
    const latin1Path = join(dir, 'dirty-latin1.txt');
    writeFileSync(latin1Path, Buffer.from([0xd6, 0xd0, 0xce, 0xc4, 0x0a]));
    const latin1Problems = inspectBytes(readFileSync(latin1Path));
    diskCases += 1;
    if (latin1Problems.length !== 1 || !latin1Problems[0].includes('UTF-8')) {
      failures.push(`自检「从磁盘读 Latin-1 文件」期望 1 处且理由是 UTF-8，实际 ${latin1Problems.length} 处：${latin1Problems.join('；')}`);
    }

    // 空集合这条判定要走一遍**真的**收集路径：一个文本文件都不放的目录，
    // collectFiles 必须给出空数组，decideScan 必须给出 2。
    // 只测 decideScan([]) 会漏掉"收集器把目录也算成文件"这类错。
    const emptyRoot = join(dir, 'empty-root');
    mkdirSync(emptyRoot);
    const noFiles = collectFiles(emptyRoot, []);
    const emptyVerdict = decideScan(noFiles, []);
    diskCases += 1;
    if (noFiles.length !== 0 || emptyVerdict.exitCode !== 2) {
      failures.push(
        `自检「空目录上拒绝通过」期望 0 个文件 / 退出码 2，实际 ${noFiles.length} 个 / 退出码 ${emptyVerdict.exitCode}`,
      );
    }

    // 反过来：同一个收集器在有脏文件时必须能红。否则上面那条"空目录判 2"可能只是
    // 因为收集器永远返回空数组——两条一起才说明收集器真的在工作。
    const dirtyRoot = join(dir, 'dirty-root');
    mkdirSync(dirtyRoot);
    writeFileSync(join(dirtyRoot, 'probe.txt'), 'a\r\n');
    const dirtyFiles = collectFiles(dirtyRoot, []);
    const dirtyOffenders = dirtyFiles
      .map((file) => ({ file, problems: inspectBytes(readFileSync(file)) }))
      .filter((entry) => entry.problems.length > 0);
    const dirtyVerdict = decideScan(dirtyFiles, dirtyOffenders);
    diskCases += 1;
    if (dirtyFiles.length !== 1 || dirtyOffenders.length !== 1 || dirtyVerdict.exitCode !== 1) {
      failures.push(
        `自检「磁盘上真放一个 CRLF 文件」期望 1 个文件 / 1 个违规 / 退出码 1，实际 ${dirtyFiles.length} 个 / ${dirtyOffenders.length} 个 / 退出码 ${dirtyVerdict.exitCode}`,
      );
    }
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }

  return { failures, diskCases };
}

// ---------------------------------------------------------------------------

function collectFiles(dir, found) {
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const full = join(dir, entry.name);
    if (entry.isDirectory()) {
      if (SKIP_DIRS.has(entry.name)) continue;
      collectFiles(full, found);
    } else if (entry.isFile() && isTextFile(entry.name)) {
      found.push(full);
    }
  }
  return found;
}

function main() {
  // 参数必须先判死。这条不是洁癖：反向探针里 `node scripts/check-text-hygiene.mjs
  // --随便什么` 曾经以退出码 0 报"全绿"——即"喂给守卫一个坏参数"算通过，
  // 是一条永远绿的路径。守卫宁可拒绝，也不能假装看懂了。
  const unknown = process.argv.slice(2).filter((a) => a !== '--self-test' && a !== '-h' && a !== '--help');
  if (unknown.length > 0) {
    console.error(`✗ 不认识的参数：${unknown.join(' ')}`);
    console.error('  用法：node scripts/check-text-hygiene.mjs [--self-test]');
    return 2;
  }
  if (process.argv.includes('-h') || process.argv.includes('--help')) {
    console.log('用法：node scripts/check-text-hygiene.mjs [--self-test]');
    return 0;
  }

  const selfTest = runSelfTest();
  if (selfTest.failures.length > 0) {
    console.error('✗ 守卫自检失败——先修守卫，别信它的结论：');
    for (const failure of selfTest.failures) console.error(`  - ${failure}`);
    return 2;
  }
  if (process.argv.includes('--self-test')) {
    console.log(`✓ 守卫自检通过（${SELF_TEST_CASES.length} 条内存用例 + ${SCAN_SELF_TESTS.length} 条退出码用例 + ${selfTest.diskCases} 条磁盘用例）`);
    return 0;
  }

  const files = collectFiles(REPO_ROOT, []);
  const offenders = [];
  for (const file of files) {
    const problems = inspectBytes(readFileSync(file));
    if (problems.length > 0) {
      offenders.push({ file: relative(REPO_ROOT, file).split('\\').join('/'), problems });
    }
  }

  // 判定本身在 decideScan 里（并被自检钉住），main() 只负责把话说清楚。
  const { exitCode, verdict } = decideScan(files, offenders);

  // 空集合上宣布"全绿"是自欺：多半是根目录算错了——"没扫到"不等于"扫过了没问题"。
  if (verdict === 'empty') {
    console.error(`✗ ${REPO_ROOT} 下没有找到任何文本文件——路径是不是变了？拒绝在空集合上通过。`);
    console.error('  （扫描器坏了 / 根目录算错了也是这个症状，它不等于"全绿"。）');
    return exitCode;
  }

  if (verdict === 'violations') {
    console.error(`✗ 有 ${offenders.length} 个文件违反 LF + 无 BOM + 合法 UTF-8（共扫了 ${files.length} 个）：`);
    for (const o of offenders) console.error(`  ${o.file}: ${o.problems.join('；')}`);
    console.error('');
    console.error('  改法：让编辑器对本仓库使用 LF 与 UTF-8（无 BOM）；不要用 PowerShell 的');
    console.error('  Set-Content / Out-File 默认编码写源码文件。');
    return exitCode;
  }

  // 报数时把**口径**一起写出来："57 个"这样的整树数字与"被跟踪面有几个"不是
  // 一回事（未跟踪/被忽略的文本文件也算在内）。不写清楚，读记录的人会把前者
  // 当成提交的属性——独立复核就为此在记录里专门解释过一遍。
  console.log(
    `✓ 文本卫生：${files.length} 个文本文件全是 LF、无 BOM、合法 UTF-8` +
      '（整树口径：含未跟踪与被忽略的文件；records/、target/ 等 SKIP_DIRS 在外）',
  );
  return exitCode;
}

// 只设 process.exitCode，不调 process.exit()——理由同 scripts/check-core-purity.mjs
// 末尾那段：本机 Node/Windows 上真被执行 process.exit() 可能撞 libuv 断言，
// 而守卫的退出码就是它的结论。
process.exitCode = main();
