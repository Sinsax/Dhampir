#!/usr/bin/env node
// 底座 CLI 的契约检查：**每个子命令的入参、出参、退出码**（名单见 SUBCOMMANDS）。
//
// # 为什么它值得单独存在
//
// dhampir CLI 是本机后端（甚至任何下游后端）的渲染实现。
// 它的「退出码 0/2/1」与「stdout 是 NDJSON」不是内部约定，而是**调用方要依赖的契约**：
// 本机后端就是靠 0 与 2 区分「出片成功」和「工程本身有问题」的。
// 契约一旦漂，表现是"看起来跑完了但结果不对"，而那种错最难发现。
//
// # 为什么判据要写在名单里
//
// judge() 要求观察的**名字一条不差**：少一条（某段代码提前 return 跳过了）
// 和多一条（有人加了判据没登记）都红。静默跳过一条判据和那条判据通过，
// 在外面看起来是一样的 —— 这正是要防的。
//
// 用法：
//   node scripts/check-cli.mjs
//   node scripts/check-cli.mjs --self-test
//   node scripts/check-cli.mjs --cli target/debug/dhampir.exe

import { spawnSync } from 'node:child_process';
import { copyFileSync, existsSync, mkdirSync, readFileSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

export const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const TMP = join(REPO_ROOT, 'target', 'p6', 'cli-contract');
const PROJECT = 'fixtures/sample-project.doc.json';
const ASSET = 'target/s3/proxy1080p.mp4';

/** CLI 的子命令名单。**必须与 crates/dhampir-worker/src/bin/dhampir.rs 的 COMMANDS 表一致** ——
 * 这是一条真判据：「加了命令但没登记」和「登记了但 --help 没列出来」都要红。
 * 名单长度写进结论文案，所以文案不会自己漂成假的。 */
export const SUBCOMMANDS = ['probe', 'info', 'gop', 'frame', 'render', 'import', 'library', 'edit', 'subtitle'];

/** 每一条判据的名字。**改这里就必须改采集端**。 */
export const EXPECTED = [
  'help',
  'no-args',
  'unknown-flag',
  'unknown-subcommand',
  'probe-clean',
  'probe-broken',
  'probe-missing-file',
  'info',
  'gop',
  'frame',
  'render',
  'render-broken',
  'import-dry',
  'import-write',
  'library',
  'import-duplicate',
  'subtitle',
  'subtitle-blank',
];

/**
 * 判定。抽成纯函数是为了能喂**故意坏的**观察进来验证它真的会红。
 */
export function judge(observed) {
  const problems = [];
  if (!Array.isArray(observed) || observed.length === 0) {
    problems.push('一条观察都没有 —— 检查拒绝在空集上通过（采集段提前退出了吗？）');
    return problems;
  }
  const seen = new Map();
  for (const row of observed) {
    if (seen.has(row.name)) problems.push('判据重复：' + row.name);
    seen.set(row.name, row);
  }
  for (const name of EXPECTED) {
    if (!seen.has(name)) problems.push('判据缺失：' + name + '（这段代码被跳过了？）');
  }
  for (const name of seen.keys()) {
    if (!EXPECTED.includes(name)) problems.push('名单里没有这条判据：' + name);
  }
  for (const row of observed) {
    if (!row.ok) {
      problems.push(row.name + (row.detail === undefined || row.detail === '' ? '' : '：' + row.detail));
    }
  }
  return problems;
}

function runSelfTest() {
  let passed = 0;
  const expect = (name, condition) => {
    if (!condition) throw new Error('自检失败：' + name);
    passed += 1;
  };
  const good = EXPECTED.map((name) => ({ name: name, ok: true, detail: '' }));
  expect('完整观察通过', judge(good).length === 0);
  expect('空集必须红', judge([]).length > 0);
  expect('undefined 必须红', judge(undefined).length > 0);
  const oneFalse = good.map((row) => (row.name === 'info' ? { ...row, ok: false, detail: 'x' } : row));
  const falseProblems = judge(oneFalse);
  expect('一条 false 必须红且点名', falseProblems.length === 1 && falseProblems[0].startsWith('info'));
  expect('少一条必须红', judge(good.filter((row) => row.name !== 'gop')).some((p) => p.includes('判据缺失')));
  expect('多一条必须红', judge(good.concat([{ name: 'smuggled', ok: true }])).some((p) => p.includes('smuggled')));
  expect('重复必须红', judge(good.concat([{ name: EXPECTED[0], ok: true }])).some((p) => p.includes('判据重复')));
  console.log('✓ CLI 契约检查自检通过（' + passed + ' 条断言）');
}

/** 跑一次 CLI 并收全输出。 */
function run(cli, args) {
  const result = spawnSync(cli, args, { cwd: REPO_ROOT, encoding: 'utf8', maxBuffer: 64 * 1024 * 1024 });
  return {
    code: result.status === null ? -1 : result.status,
    stdout: result.stdout || '',
    stderr: result.stderr || '',
  };
}

/** 从 NDJSON 里取某一类事件。 */
export function events(text, kind) {
  const found = [];
  for (const line of String(text).split('\n')) {
    const trimmed = line.trim();
    if (trimmed.length === 0) continue;
    let event = null;
    try { event = JSON.parse(trimmed); } catch (error) { continue; }
    if (event !== null && event.event === kind) found.push(event);
  }
  return found;
}

function collect(cli) {
  const observed = [];
  const record = (name, ok, detail) => {
    observed.push({ name: name, ok: !!ok, detail: detail === undefined ? '' : String(detail) });
  };
  rmSync(TMP, { recursive: true, force: true });
  mkdirSync(TMP, { recursive: true });

  // ---- 帮助与用法错 ----
  const help = run(cli, ['--help']);
  const allListed = SUBCOMMANDS.every((name) => help.stdout.includes(name));
  record('help', help.code === 0 && allListed,
    'exit=' + help.code + ' ' + SUBCOMMANDS.length + ' 个子命令都在=' + allListed);
  record('no-args', run(cli, []).code === 0, 'exit=' + run(cli, []).code);
  const unknownFlag = run(cli, ['render', '--wdith', '640']);
  record('unknown-flag', unknownFlag.code === 2, 'exit=' + unknownFlag.code);
  record('unknown-subcommand', run(cli, ['transcode']).code === 2, '');

  // ---- probe ----
  const probe = run(cli, ['probe', '--project', PROJECT]);
  let probeBody = null;
  try { probeBody = JSON.parse(probe.stdout); } catch (error) { probeBody = null; }
  record('probe-clean',
    probe.code === 0 && probeBody !== null && Array.isArray(probeBody.errors) && probeBody.errors.length === 0
      && Array.isArray(probeBody.warnings),
    'exit=' + probe.code + ' ' + JSON.stringify(probeBody).slice(0, 160));

  // 坏工程：**校验有 error 就退 2**，而 stdout 上仍然给完整清单。
  const brokenPath = join(TMP, 'broken.json');
  const doc = JSON.parse(readFileSync(join(REPO_ROOT, PROJECT), 'utf8'));
  doc.timeline.tracks[0].layers[0].end = -5;
  writeFileSync(brokenPath, JSON.stringify(doc), 'utf8');
  const broken = run(cli, ['probe', '--project', brokenPath]);
  let brokenBody = null;
  try { brokenBody = JSON.parse(broken.stdout); } catch (error) { brokenBody = null; }
  record('probe-broken', broken.code === 2 && brokenBody !== null && brokenBody.errors.length > 0,
    'exit=' + broken.code + ' errors=' + (brokenBody === null ? 'none' : brokenBody.errors.length));

  // 文件不在 —— **也是用户能改的错，退 2 而不是 1**。
  record('probe-missing-file', run(cli, ['probe', '--project', 'fixtures/definitely-not-here.json']).code === 2, '');

  // ---- info ----
  const info = run(cli, ['info', '--asset', ASSET]);
  let infoBody = null;
  try { infoBody = JSON.parse(info.stdout); } catch (error) { infoBody = null; }
  record('info',
    info.code === 0 && infoBody !== null && infoBody.width === 1920 && infoBody.height === 1080
      && infoBody.frame_count === 480 && infoBody.gop_length === 60,
    'exit=' + info.code + ' ' + JSON.stringify(infoBody));

  // ---- gop ----
  const gop = run(cli, ['gop', '--asset', ASSET]);
  let gopBody = null;
  try { gopBody = JSON.parse(gop.stdout); } catch (error) { gopBody = null; }
  // dts_origin 必须在：样本表的 dts 是**相对量**，不写出来调用方无从还原绝对时间戳。
  record('gop',
    gop.code === 0 && gopBody !== null && Array.isArray(gopBody.slices) && gopBody.slices.length > 1
      && typeof gopBody.dts_origin === 'number',
    'exit=' + gop.code + ' slices=' + (gopBody === null ? 'none' : gopBody.slices.length));

  // ---- frame ----
  const frameDir = join(TMP, 'frames');
  const frame = run(cli, ['frame', '--project', PROJECT, '--frame', '30', '--out', frameDir]);
  let frameBody = null;
  try { frameBody = JSON.parse(frame.stdout); } catch (error) { frameBody = null; }
  const png = join(frameDir, 'frame-0030.png');
  record('frame',
    frame.code === 0 && frameBody !== null && /^[0-9a-f]{16}$/.test(String(frameBody.digest))
      && existsSync(png) && statSync(png).size > 1000,
    'exit=' + frame.code + ' ' + JSON.stringify(frameBody));

  // ---- render ----
  const outPath = join(TMP, 'out.mp4');
  const render = run(cli, ['render', '--project', PROJECT, '--from', '0', '--to', '9',
    '--width', '320', '--height', '180', '--out', outPath]);
  const starts = events(render.stdout, 'start');
  const progresses = events(render.stdout, 'progress');
  const dones = events(render.stdout, 'done');
  // **stdout 只能是 NDJSON**：混进人话输出，调用方的解析就会悄悄少几条。
  const nonJsonLines = String(render.stdout).split('\n')
    .filter((line) => line.trim().length > 0)
    .filter((line) => { try { JSON.parse(line); return false; } catch (error) { return true; } });
  record('render',
    render.code === 0 && starts.length === 1 && progresses.length === 10 && dones.length === 1
      && dones[0].frames === 10 && dones[0].failed === false && nonJsonLines.length === 0
      && existsSync(outPath),
    'exit=' + render.code + ' start=' + starts.length + ' progress=' + progresses.length
      + ' done=' + dones.length + ' 非 JSON 行=' + nonJsonLines.length);

  const brokenRender = run(cli, ['render', '--project', brokenPath, '--out', join(TMP, 'never.mp4')]);
  record('render-broken', brokenRender.code === 2 && !existsSync(join(TMP, 'never.mp4')),
    'exit=' + brokenRender.code);

  // ---- import / library ----
  // **在副本上真写**：检查工具不该改动 fixtures 下的交付物。
  const workPath = join(TMP, 'lib-work.json');
  copyFileSync(join(REPO_ROOT, PROJECT), workPath);
  const original = readFileSync(join(REPO_ROOT, PROJECT), 'utf8');

  const dry = run(cli, ['import', '--project', workPath, '--file', ASSET, '--id', 'extra']);
  let dryBody = null;
  try { dryBody = JSON.parse(dry.stdout); } catch (error) { dryBody = null; }
  record('import-dry',
    dry.code === 0 && dryBody !== null && dryBody.written === false
      && dryBody.asset !== undefined && dryBody.asset.frame_count === 480
      && dryBody.asset.uri === 'proxy1080p.mp4'
      && readFileSync(workPath, 'utf8') === original,
    'exit=' + dry.code + ' 文件未被改动=' + (readFileSync(workPath, 'utf8') === original));

  const wrote = run(cli, ['import', '--project', workPath, '--file', ASSET, '--id', 'extra', '--write']);
  let wroteBody = null;
  try { wroteBody = JSON.parse(wrote.stdout); } catch (error) { wroteBody = null; }
  record('import-write',
    wrote.code === 0 && wroteBody !== null && wroteBody.written === true && wroteBody.asset_count === 5,
    'exit=' + wrote.code + ' asset_count=' + (wroteBody === null ? 'none' : wroteBody.asset_count));

  const library = run(cli, ['library', '--project', workPath]);
  let libraryBody = null;
  try { libraryBody = JSON.parse(library.stdout); } catch (error) { libraryBody = null; }
  record('library',
    library.code === 0 && libraryBody !== null && libraryBody.total === 5
      && Array.isArray(libraryBody.unused) && libraryBody.unused.includes('extra'),
    'exit=' + library.code + ' ' + (libraryBody === null ? 'none' : JSON.stringify(libraryBody.unused)));

  const duplicate = run(cli, ['import', '--project', workPath, '--file', ASSET, '--id', 'extra']);
  record('import-duplicate', duplicate.code === 2, 'exit=' + duplicate.code);

  // ---- subtitle：文字覆盖层（评估层的直接出口）----
  //
  // 它不需要 GPU 也不需要 ffmpeg，所以**进默认关卡**。
  // 判的不是一串魔数，而是三条**结构不变量** —— 居中、底边、行高。
  // 那三个关系正是两端必须一致的东西；写死坐标只会让测试跟着实现一起漂。
  const subProject = join(TMP, 'sub-project.json');
  const subDoc = JSON.parse(readFileSync(join(REPO_ROOT, 'fixtures', 'sample-project.doc.json'), 'utf8'));
  subDoc.assets.push({ id: 'sub.srt', kind: 'subtitle', uri: 'sample-subtitle.srt' });
  subDoc.timeline.tracks.push({
    id: 'sub',
    kind: 'subtitle',
    layers: [{ id: 'cue', start: 0, end: 100000, source: { asset_id: 'sub.srt', source_in: 0 } }],
    subtitle: {
      font_ratio: 0.055, bottom_margin: 0.06, max_lines: 2,
      color: [255, 255, 255, 255], outline: true,
    },
  });
  writeFileSync(subProject, JSON.stringify(subDoc), 'utf8');
  const fixtureRoot = join(REPO_ROOT, 'fixtures');

  const parseOverlay = (result) => {
    try { return JSON.parse(result.stdout); } catch (error) { return null; }
  };

  const shown = run(cli, ['subtitle', '--project', subProject, '--asset-root', fixtureRoot, '--frame', '15']);
  const overlay = parseOverlay(shown);
  const item = overlay !== null && Array.isArray(overlay.items) ? overlay.items[0] : null;
  record('subtitle',
    shown.code === 0 && item !== null
      && item.text === '第一行中文'
      && Math.abs(item.rect.x + item.rect.width / 2 - 0.5) < 1e-4
      && Math.abs(item.rect.y + item.rect.height - (1 - 0.06)) < 1e-4
      && Math.abs(item.rect.height - 0.055 * 1.2) < 1e-4,
    'exit=' + shown.code + ' item=' + JSON.stringify(item));

  // **这一帧没有字幕**：空数组 + 素材确实被解析过（subtitle_assets=1）。
  // 少了后半句，这条就会在「字幕素材根本没读进来」时也通过 —— 那正是它要抓的东西。
  const blank = run(cli, ['subtitle', '--project', subProject, '--asset-root', fixtureRoot, '--frame', '6000']);
  const blankOverlay = parseOverlay(blank);
  record('subtitle-blank',
    blank.code === 0 && blankOverlay !== null
      && Array.isArray(blankOverlay.items) && blankOverlay.items.length === 0
      && blankOverlay.subtitle_assets === 1,
    'exit=' + blank.code + ' subtitle_assets=' + (blankOverlay === null ? 'null' : blankOverlay.subtitle_assets));

  return observed;
}

function main() {
  const argv = process.argv.slice(2);
  if (argv.includes('--self-test')) { runSelfTest(); return; }
  const index = argv.indexOf('--cli');
  const cli = index >= 0 ? argv[index + 1] : join(REPO_ROOT, 'target', 'debug', 'dhampir.exe');
  if (!existsSync(cli)) {
    console.error('找不到 dhampir 可执行文件：' + cli);
    console.error('先跑：cargo build -p dhampir-worker --bin dhampir');
    process.exitCode = 2;
    return;
  }
  const observed = collect(cli);
  const problems = judge(observed);
  console.log('CLI 契约：' + observed.filter((row) => row.ok).length + ' / ' + EXPECTED.length + ' 条判据通过');
  if (problems.length > 0) {
    for (const problem of problems) console.error('  - ' + problem);
    console.error('✗ dhampir CLI 不满足契约');
    process.exitCode = 1;
    return;
  }
  console.log('✓ dhampir CLI 契约成立（' + SUBCOMMANDS.length
    + ' 个子命令 / stdout 是 NDJSON / 退出码 0-2-1）');
  rmSync(TMP, { recursive: true, force: true });
}

main();
