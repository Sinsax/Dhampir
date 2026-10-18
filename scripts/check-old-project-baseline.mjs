#!/usr/bin/env node
// **老工程逐字节不变** —— 一条命令的判据。
//
//   node scripts/check-old-project-baseline.mjs
//   node scripts/check-old-project-baseline.mjs --frames 0,100,650
//   node scripts/check-old-project-baseline.mjs --self-test
//
// # 它管什么
//
// 这条不变量的口径此前**只有一句台账里的话**：`frame(plain) sha256 == 8b7d9e18a7320ee7`。
// 而那句话**复现不出来** —— 没记工程、没记命令、没记帧号（第 80 轮复核时逐条试都对不上）。
// 一条复现不出来的判据比没有更坏：它让人以为自己验过了。
//
// 所以这里把它变成一条命令，口径是**可比对的两份二进制**：
//
//   ① 当次构建的 `dhampir`（默认 target/debug 或 target/release 里较新的那个）
//   ② 已发布产物里的 `dhampir`（默认 dist/ 里版本号最大的 `dhampir-<ver>-win32-x64.zip`，
//      解压前先按同目录的 `.sha256.txt` 校验 —— 产物本身被动过就该先说，而不是拿它当基线）
//
// 两者渲染同一份工程、同一批帧，**逐帧比 PNG 的字节**。全同才算过。
//
// # 退出码
//
//   0  全部逐字节相同
//   1  有帧不同（**这就是回归**：老工程渲染变了）
//   2  跑不了（缺工程 / 缺产物 / 起不来）—— **不静默通过**，空文件集不算判据
//
// # 依赖（所以它在 CI 之外）
//
// 它要一台**能跑出片**的机器（GPU / wgpu）、`dist/` 里的历史产物、以及
// `out/effects-demo` 那份带素材的示范工程 —— 三样都在仓库里的话它才成立。
// 正因如此它的依赖档是 `cli`：`scripts/check-ci-guards.mjs` 会拦住"把它塞进 CI"。

import { spawnSync } from 'node:child_process';
import { closeSync, existsSync, mkdirSync, openSync, readFileSync, readdirSync, statSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

export const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');

/** 默认跑哪些帧。**不是随手挑的**：effects-demo 的关键帧落在这些位置附近，
 *  而"整段起止 + 中间若干"才能在秒级内覆盖到起始/结束/中间三种状态。 */
export const DEFAULT_FRAMES = [0, 45, 100, 135, 200, 250, 300, 350, 400, 450, 500, 550, 600, 650];

/** 默认工程：示范工程里有特效/混合/文字/遮罩，改一处就会在这批帧上显形。 */
export const DEFAULT_PROJECT = 'out/effects-demo/effects-demo.doc.json';

/** `--frames 0,45,650` -> 数字数组。认不出来就报错（不许悄悄少跑几帧）。 */
export function parseFrames(text) {
  const problems = [];
  const frames = [];
  for (const token of String(text).split(',')) {
    const trimmed = token.trim();
    if (trimmed === '') continue;
    if (!/^[0-9]+$/.test(trimmed)) {
      problems.push('认不出的帧号：' + JSON.stringify(trimmed));
      continue;
    }
    frames.push(Number(trimmed));
  }
  if (frames.length === 0) problems.push('--frames 一帧都没给 —— 空文件集不算判据');
  return { frames, problems };
}

/** dist/ 里 `dhampir-<x.y.z>-win32-x64.zip` 挑版本最大的那个。 */
export function pickNewestRelease(names) {
  const versioned = [];
  for (const name of names) {
    const match = /^dhampir-([0-9]+)\.([0-9]+)\.([0-9]+)-win32-x64\.zip$/.exec(name);
    if (match === null) continue;
    versioned.push({ name, parts: [Number(match[1]), Number(match[2]), Number(match[3])] });
  }
  if (versioned.length === 0) return null;
  versioned.sort((a, b) => {
    for (let i = 0; i < 3; i += 1) {
      if (a.parts[i] !== b.parts[i]) return b.parts[i] - a.parts[i];
    }
    return 0;
  });
  return versioned[0].name;
}

/** `.sha256.txt` 的一行：`<64 位十六进制>  <文件名>`。 */
export function parseShaLine(text) {
  const match = /^\s*([0-9a-f]{64})\b/m.exec(String(text));
  return match === null ? null : match[1].toLowerCase();
}

/** 逐帧比摘要。返回问题列表（空 = 全同）。 */
export function compareFrames(baseline, current) {
  const problems = [];
  if (baseline.size === 0 || current.size === 0) {
    problems.push('有一侧一张帧都没有 —— 空文件集不算判据');
    return problems;
  }
  for (const [frame, digest] of baseline) {
    if (!current.has(frame)) {
      problems.push('帧 ' + frame + ' 只有基线侧有 —— 当次构建没出这一帧');
      continue;
    }
    if (current.get(frame) !== digest) {
      problems.push('帧 ' + frame + ' 与基线**不相同**：基线 ' + digest.slice(0, 16) + ' / 当次 ' +
        current.get(frame).slice(0, 16));
    }
  }
  for (const frame of current.keys()) {
    if (!baseline.has(frame)) problems.push('帧 ' + frame + ' 只有当次构建有 —— 基线里没有这一帧');
  }
  return problems;
}

/** 文件摘要（读不到返回 null）。 */
function sha256Of(path) {
  try {
    return createHash('sha256').update(readFileSync(path)).digest('hex');
  } catch {
    return null;
  }
}

/** 起一个进程，输出重定向到文件（**不走管道**：受限环境里建不出命名管道）。 */
function run(exe, args, logDir, tag) {
  mkdirSync(logDir, { recursive: true });
  const outPath = join(logDir, tag + '.out');
  const errPath = join(logDir, tag + '.err');
  const outFd = openSync(outPath, 'w');
  const errFd = openSync(errPath, 'w');
  const result = spawnSync(exe, args, { cwd: REPO_ROOT, windowsHide: true, stdio: ['ignore', outFd, errFd] });
  closeSync(outFd);
  closeSync(errFd);
  const readOr = (path) => {
    try {
      return readFileSync(path, 'utf8');
    } catch {
      return '';
    }
  };
  return {
    code: result.error === undefined || result.error === null ? result.status : null,
    spawnError: result.error === undefined || result.error === null ? null : String(result.error.message),
    stdout: readOr(outPath),
    stderr: readOr(errPath),
  };
}

/** 解压 dist 里的产物（返回 bin/dhampir.exe 的路径或 null）。 */
function extractRelease(zipPath, destDir, logDir) {
  const exe = process.platform === 'win32' ? 'powershell.exe' : 'unzip';
  const args = process.platform === 'win32'
    ? ['-NoProfile', '-Command', 'Expand-Archive -LiteralPath "' + zipPath + '" -DestinationPath "' + destDir + '" -Force']
    : ['-o', zipPath, '-d', destDir];
  const result = run(exe, args, logDir, 'extract');
  if (result.code !== 0) {
    console.error('✗ 解压失败（' + exe + ' 退出码 ' + String(result.code) + '）' +
      (result.spawnError === null ? '' : '：' + result.spawnError));
    if (result.stderr.trim() !== '') console.error(result.stderr.trim().slice(-500));
    return null;
  }
  const bin = join(destDir, 'bin', process.platform === 'win32' ? 'dhampir.exe' : 'dhampir');
  return existsSync(bin) ? bin : null;
}

/** 当次构建的二进制：--current-exe，或 target/{debug,release} 里较新的那个。 */
function currentBinary(explicit) {
  if (explicit !== undefined) {
    return existsSync(explicit) ? explicit : null;
  }
  const exeName = process.platform === 'win32' ? 'dhampir.exe' : 'dhampir';
  const candidates = [join(REPO_ROOT, 'target', 'debug', exeName), join(REPO_ROOT, 'target', 'release', exeName)]
    .filter((path) => existsSync(path));
  if (candidates.length === 0) return null;
  candidates.sort((a, b) => statSync(b).mtimeMs - statSync(a).mtimeMs);
  return candidates[0];
}

function selfTest() {
  const failures = [];
  let checks = 0;
  const check = (name, ok) => {
    checks += 1;
    if (!ok) failures.push('自检「' + name + '」不成立');
  };
  check('帧号解析', parseFrames('0, 45,650').frames.length === 3 && parseFrames('0,45,650').problems.length === 0);
  check('帧号解析：垃圾要报', parseFrames('0,x').problems.length === 1);
  check('帧号解析：空要报', parseFrames('').problems.length === 1);
  check('版本挑选', pickNewestRelease(['dhampir-0.3.0-win32-x64.zip', 'dhampir-0.10.0-win32-x64.zip', 'junk.zip']) === 'dhampir-0.10.0-win32-x64.zip');
  check('版本挑选：没有产物返回 null', pickNewestRelease(['junk.zip']) === null);
  check('sha 行解析', parseShaLine('AB'.toLowerCase() + '0'.repeat(62) + '  x.zip') === 'ab' + '0'.repeat(62));
  check('sha 行解析：垃圾返回 null', parseShaLine('not a hash') === null);
  const same = new Map([[0, 'aa'], [1, 'bb']]);
  check('相同 -> 无问题', compareFrames(same, new Map(same)).length === 0);
  check('不同 -> 报', compareFrames(new Map([[0, 'aa']]), new Map([[0, 'cc']])).length === 1);
  check('少一帧 -> 报', compareFrames(new Map([[0, 'aa'], [1, 'bb']]), new Map([[0, 'aa']])).length === 1);
  check('空文件集 -> 报', compareFrames(new Map(), new Map([[0, 'aa']])).length >= 1);
  check('默认帧表非空且不重复', DEFAULT_FRAMES.length > 0 && new Set(DEFAULT_FRAMES).size === DEFAULT_FRAMES.length);
  check('默认工程是仓库相对路径', !DEFAULT_PROJECT.startsWith('/') && !/^[A-Za-z]:/.test(DEFAULT_PROJECT));
  if (failures.length > 0) {
    for (const failure of failures) console.error('  - ' + failure);
    console.error('✗ 自检不过');
    process.exit(1);
  }
  console.log('✓ 自检：' + checks + ' 条断言（帧表 / 版本挑选 / sha 行 / 逐帧比对）');
}

function main() {
  const argv = process.argv.slice(2);
  if (argv.includes('--self-test')) {
    selfTest();
    return 0;
  }
  const valueOf = (flag) => {
    const at = argv.indexOf(flag);
    return at < 0 ? undefined : argv[at + 1];
  };

  const project = resolve(REPO_ROOT, valueOf('--project') === undefined ? DEFAULT_PROJECT : valueOf('--project'));
  const assetRoot = valueOf('--asset-root') === undefined ? dirname(project) : resolve(REPO_ROOT, valueOf('--asset-root'));
  const framesArg = valueOf('--frames');
  const { frames, problems: frameProblems } = framesArg === undefined
    ? { frames: DEFAULT_FRAMES, problems: [] }
    : parseFrames(framesArg);

  if (frameProblems.length > 0) {
    for (const problem of frameProblems) console.error('  - ' + problem);
    return 2;
  }
  if (!existsSync(project)) {
    console.error('✗ 工程不存在：' + project);
    console.error('  这条判据要一份**带素材的老工程**（默认 out/effects-demo）—— 它在仓库外，缺了就只能说"这次没验"，不许说"通过"。');
    return 2;
  }

  const current = currentBinary(valueOf('--current-exe'));
  if (current === null) {
    console.error('✗ 找不到当次构建的 dhampir（target/debug 与 target/release 都没有）—— 先 cargo build');
    return 2;
  }

  const logDir = join(REPO_ROOT, 'target', 'old-project-baseline', 'logs');
  const framesRoot = join(REPO_ROOT, 'target', 'old-project-baseline', 'frames');

  // 基线：显式给的优先，否则从 dist/ 里挑版本最大的产物。
  let baseline = valueOf('--baseline-exe') === undefined ? null : resolve(REPO_ROOT, valueOf('--baseline-exe'));
  let baselineTag = 'baseline-exe';
  if (baseline === null) {
    const distDir = join(REPO_ROOT, 'dist');
    const names = existsSync(distDir) ? readdirSync(distDir) : [];
    const zipName = pickNewestRelease(names);
    if (zipName === null) {
      console.error('✗ dist/ 里没有可比对的已发布产物（dhampir-<ver>-win32-x64.zip）—— 没有基线就是没验，不许当通过');
      return 2;
    }
    const zipPath = join(distDir, zipName);
    const shaFile = join(distDir, zipName.replace(/\.zip$/, '.zip.sha256.txt'));
    if (existsSync(shaFile)) {
      const expected = parseShaLine(readFileSync(shaFile, 'utf8'));
      const actual = sha256Of(zipPath);
      if (expected === null || actual !== expected) {
        console.error('✗ 产物摘要与 ' + shaFile.replace(REPO_ROOT + '\\', '') + ' 不一致 —— 拿被动过的产物当基线没有意义');
        console.error('  期望 ' + String(expected) + '，实际 ' + String(actual));
        return 2;
      }
    }
    const stem = zipName.replace(/\.zip$/, '');
    const destDir = join(REPO_ROOT, 'target', 'old-project-baseline', stem);
    const binName = process.platform === 'win32' ? 'dhampir.exe' : 'dhampir';
    const cached = join(destDir, 'bin', binName);
    baseline = existsSync(cached) ? cached : extractRelease(zipPath, destDir, logDir);
    if (baseline === null) {
      console.error('✗ 解压出来的产物里没有 bin/' + binName);
      return 2;
    }
    baselineTag = stem;
  } else if (!existsSync(baseline)) {
    console.error('✗ --baseline-exe 给的文件不存在：' + baseline);
    return 2;
  }

  /** 帧 PNG 的路径。**别猜文件名**：CLI 实际写的是 `frame-0000.png`（四位补零），
   *  而它的帮助里写着 `frame-<N>.png` —— 第 80 轮我就是照帮助写的，于是"渲染成功但找不到文件"。
   *  所以这里按三种可能找，找不到再扫目录认数字。 */
  const pngForFrame = (outDir, frame) => {
    for (const name of ['frame-' + String(frame).padStart(4, '0') + '.png', 'frame-' + frame + '.png']) {
      const path = join(outDir, name);
      if (existsSync(path)) return path;
    }
    try {
      for (const name of readdirSync(outDir)) {
        const match = /^frame-0*([0-9]+)\.png$/.exec(name);
        if (match !== null && Number(match[1]) === frame) return join(outDir, name);
      }
    } catch {
      // 目录还不存在 = 还没渲染
    }
    return null;
  };

  const render = (exe, tag) => {
    const outDir = join(framesRoot, tag);
    const digests = new Map();
    for (const frame of frames) {
      const existing = pngForFrame(outDir, frame);
      const needRender = tag.startsWith('current') || existing === null;
      if (needRender) {
        mkdirSync(outDir, { recursive: true });
        const result = run(exe, ['frame', '--project', project, '--asset-root', assetRoot, '--frame', String(frame), '--out', outDir],
          logDir, tag + '-frame-' + frame);
        if (result.code !== 0) {
          console.error('✗ 渲染帧 ' + frame + ' 失败（' + tag + '，退出码 ' + String(result.code) + '）' +
            (result.spawnError === null ? '' : '：' + result.spawnError));
          if (result.stderr.trim() !== '') console.error(result.stderr.trim().slice(-500));
          return null;
        }
      }
      const png = pngForFrame(outDir, frame);
      const digest = png === null ? null : sha256Of(png);
      if (digest === null) {
        console.error('✗ 渲染帧 ' + frame + ' 之后在 ' + outDir + ' 里找不到它（' + tag + '）—— CLI 的产出名变了？');
        return null;
      }
      digests.set(frame, digest);
    }
    return digests;
  };

  console.log('当次构建：' + current);
  console.log('基线产物：' + baseline);
  console.log('工程：' + project);
  console.log('帧：' + frames.join(','));
  console.log('');

  const baselineDigests = render(baseline, baselineTag);
  if (baselineDigests === null) return 2;
  const currentDigests = render(current, 'current');
  if (currentDigests === null) return 2;

  const problems = compareFrames(baselineDigests, currentDigests);
  for (const frame of frames) {
    const same = baselineDigests.get(frame) === currentDigests.get(frame);
    console.log((same ? '  同  ' : '  异  ') + '帧 ' + String(frame).padStart(4, ' ') + '  ' +
      String(baselineDigests.get(frame)).slice(0, 16) + '  ' + String(currentDigests.get(frame)).slice(0, 16));
  }
  console.log('');
  if (problems.length > 0) {
    for (const problem of problems) console.error('  - ' + problem);
    console.error('✗ 老工程渲染变了（' + problems.length + ' 条）');
    return 1;
  }
  console.log('✓ 老工程逐字节不变：' + frames.length + ' / ' + frames.length + ' 帧与已发布产物相同（当次构建 vs ' +
    baselineTag + '）');
  return 0;
}

// 入口守卫：被 import 时不执行 CLI。
if (process.argv[1] !== undefined && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  process.exitCode = main();
}
