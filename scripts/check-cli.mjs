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
//   node scripts/check-cli.mjs [--cli target/debug/dhampir]

import { runToolSync } from './spawn-tool.mjs';
import { copyFileSync, existsSync, mkdirSync, readFileSync, statSync, writeFileSync } from 'node:fs';
import { tryRemove } from './safe-remove.mjs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

export const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const TMP = join(REPO_ROOT, 'target', 'p6', 'cli-contract');
const PROJECT = 'fixtures/sample-project.doc.json';
const ASSET = 'target/s3/proxy1080p.mp4'; // gitignore 的草稿素材：node scripts/make-test-media.mjs 生成

/** CLI 的子命令名单。**必须与 crates/dhampir-worker/src/bin/dhampir.rs 的 COMMANDS 表一致** ——
 * 这是一条真判据：「加了命令但没登记」和「登记了但 --help 没列出来」都要红。
 * 名单长度写进结论文案，所以文案不会自己漂成假的。
 * T7.1 加了五个具名子命令（undo / redo / clip / sequence / batch）。 */
export const SUBCOMMANDS = ['probe', 'info', 'gop', 'frame', 'render', 'import', 'library', 'edit', 'subtitle',
  'undo', 'redo', 'clip', 'sequence', 'batch'];

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
  'render-subtitle-out',
  'render-subtitle-out-format',
  'render-subtitle-out-refused',
  'render-subtitle-out-empty',
  // T7.1：具名子命令是「同一实现的糖」，判据问的就是它到底是不是同一个实现。
  'clip-equivalence',
  'sequence-equivalence',
  'history-alias',
  'clip-dry-run',
  'clip-rejects-bad-usage',
  'batch-matches-edits',
  'batch-atomic',
  'batch-empty',
  // 退出码契约里**最容易漏的一条**：缺一个必给的开关是用法错（2），不是运行期失败（1）。
  // 从前 14 个子命令里有 9 个退 1 —— 而 30 条判据一条都没钉它，所以它活了很久。
  // 本机后端正是靠 0 / 2 区分「成功」与「用户能改的错」，混淆的代价是它去查渲染管线。
  'usage-error-exit-code',
  // frame 从前只出单帧，而多给的 --to 会被静默收下（用户以为出了一段）。
  'frame-range',
  // 只给 `--from` 是「**到工程结尾**」—— `docs/api.md` §2.2 的承诺。
  // 从前实现是 `to = args.to.unwrap_or(from)` ⇒ 只出一帧、退出码还是 0（下游交接单 D2）。
  // 这一条钉的是「参数 -> 工程长度」这段**接线**：单测盖不到它，而 D2 的错正好出在这里。
  'frame-from-only-to-end',
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
  // **不喂 stdin**（见 scripts/spawn-tool.mjs）：CLI 从不读 stdin，
  // 而默认的 stdin 管道在限制管道的环境里会直接创建失败（EBUSY）。
  const result = runToolSync(cli, args, { cwd: REPO_ROOT, maxBuffer: 64 * 1024 * 1024 });
  return {
    code: result.status === null ? -1 : result.status,
    stdout: result.stdout || '',
    stderr: result.stderr || '',
    // **把"起不来"与"退出码就是 -1"分开。** 混在一起的话，30 条判据会在环境
    // 起不了子进程时一起变红，看起来像"CLI 全坏了"—— 那是误导。
    spawnError: result.error === undefined || result.error === null ? null : result.error.code,
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
  // 清不掉不算失败（见 scripts/safe-remove.mjs）：清理失败不该决定守卫的结论。
  tryRemove(TMP);
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

  // ---- 用法错的退出码：**缺一个必给的开关是 2，不是 1** ----
  //
  // 这一条钉的是分类而不是文案：少给开关的人要去补开关，而不是去查 GPU。
  // 判据拿**每一个**子命令的「什么都不给」来跑，所以新加子命令若忘了给这条
  // 留出口，它自己就会掉进这里变红（`unknown-subcommand` 那条只管名字认不认）。
  const bareUsage = SUBCOMMANDS.map((name) => ({ name: name, code: run(cli, [name]).code }));
  const notTwo = bareUsage.filter((row) => row.code !== 2);
  // 例外：`clip` / `sequence` 的第一个位置参数是动作名，缺动作名也是用法错（2），
  // 所以它们**不该**出现在例外名单里 —— 一律要求 2。
  record('usage-error-exit-code', notTwo.length === 0,
    '退的不是 2 的：' + (notTwo.map((row) => row.name + '=' + row.code).join(' ') || '（无）'));

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

  // `frame` 出**一段**：从前 `--to` 会被静默收下（用户以为出了一段，实际只出一帧，
  // 退出码还是 0）。这条钉住三件事：帧数对、文件名逐个落盘、多给 --frame 要退 2。
  // 后一半与前半同样重要 —— 让 --frame 悄悄赢，产出的就不是用户要的那一份。
  const rangeDir = join(TMP, 'frame-range');
  const ranged = run(cli, ['frame', '--project', PROJECT, '--from', '0', '--to', '4', '--out', rangeDir]);
  let rangedBody = null;
  try { rangedBody = JSON.parse(ranged.stdout); } catch (error) { rangedBody = null; }
  const rangeFiles = [0, 1, 2, 3, 4].map((n) => join(rangeDir, 'frame-' + String(n).padStart(4, '0') + '.png'));
  const bothWays = run(cli, ['frame', '--project', PROJECT, '--frame', '1', '--from', '0', '--out', rangeDir]);
  record('frame-range',
    ranged.code === 0 && rangedBody !== null && Array.isArray(rangedBody.frames)
      && rangedBody.frames.length === 5 && rangedBody.count === 5
      && rangeFiles.every((file) => existsSync(file) && statSync(file).size > 1000)
      && bothWays.code === 2,
    'exit=' + ranged.code + ' 帧数=' + (rangedBody === null || !rangedBody.frames ? 'none' : rangedBody.frames.length)
      + ' 落盘=' + rangeFiles.filter((file) => existsSync(file)).length + '/5'
      + ' 混给 --frame 的 exit=' + bothWays.code);

  // 只给 `--from` ⇒ **到工程结尾**（`docs/api.md` §2.2）。判据取工程自己的结尾：
  // 所有**启用的 video 层**的最大 `end`（`end` 左闭右开 ⇒ 最后一帧是 `end - 1`）——
  // 与实现用的是同一条口径（`compose::end_frame_v2`）。取 end-3 起，正好应当出 3 张。
  const fixture = JSON.parse(readFileSync(join(REPO_ROOT, PROJECT), 'utf8'));
  const seqEnd = Math.max(...fixture.timeline.tracks
    .filter((track) => track.kind === 'video')
    .flatMap((track) => track.layers
      .filter((layer) => layer.enabled !== false)
      .map((layer) => layer.end)));
  const tailFrom = seqEnd - 3;
  const tailDir = join(TMP, 'frame-from-only');
  const tail = run(cli, ['frame', '--project', PROJECT, '--from', String(tailFrom), '--out', tailDir]);
  let tailBody = null;
  try { tailBody = JSON.parse(tail.stdout); } catch (error) { tailBody = null; }
  const tailFiles = [tailFrom, tailFrom + 1, tailFrom + 2]
    .map((n) => join(tailDir, 'frame-' + String(n).padStart(4, '0') + '.png'));
  record('frame-from-only-to-end',
    tail.code === 0 && tailBody !== null && Array.isArray(tailBody.frames)
      && tailBody.count === 3 && tailBody.frames.length === 3
      && tailFiles.every((file) => existsSync(file) && statSync(file).size > 1000),
    'exit=' + tail.code + ' 帧数=' + (tailBody === null || !tailBody.frames ? 'none' : tailBody.frames.length)
      + ' 落盘=' + tailFiles.filter((file) => existsSync(file)).length + '/3'
      + '（工程 end=' + seqEnd + '，--from ' + tailFrom + '）');

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

  // ---- 侧挂字幕（--subtitle-out）：文字上屏的第二种口径 ----
  //
  // 它与「烧进画面」是两条独立的路：**不给字体时画面上一个字都没有**（`--font-file` 那条路
  // 会整趟判失败，而这一趟确实判失败），而侧挂文件照写 —— 侧挂回答的是"这段里说过什么"，
  // 不是"像素里有几个字"。所以这里的退出码是 1 而不是 0，这正是那条边界本身。
  //
  // 时间**重定基到这一趟产物**、且裁到出片区间里：第 2 条被切成 3 秒结尾。
  // 那是这份判据里最容易写成"整条照抄"的一处 —— 整条照抄在这里会过得很舒服。
  const sidePath = join(TMP, 'side.srt');
  const side = run(cli, ['render', '--project', subProject, '--asset-root', fixtureRoot,
    '--from', '0', '--to', '89', '--width', '64', '--height', '36',
    '--out', join(TMP, 'side.mp4'), '--subtitle-out', sidePath]);
  const sideDone = events(side.stdout, 'done');
  const sideText = existsSync(sidePath) ? readFileSync(sidePath, 'utf8') : '';
  // 逐字节比对（含结尾的空行与换行）：BOM 与 CR 也在这一条里被判掉。
  const sideExpected = '1\n00:00:00,000 --> 00:00:02,000\n第一行中文'
    + '\n\n2\n00:00:02,000 --> 00:00:03,000\n'
    + 'Mixed 混排 text with a rather long tail that ought to wrap somewhere\n\n';
  record('render-subtitle-out',
    side.code === 1 && sideDone.length === 1 && sideDone[0].failed === true
      && sideDone[0].subtitle_out === sidePath && sideDone[0].subtitle_entries === 2
      && sideText === sideExpected
      && Array.isArray(sideDone[0].issues)
      && sideDone[0].issues.some((issue) => issue.code === 'subtitle_font_missing'),
    'exit=' + side.code + ' entries=' + (sideDone.length === 0 ? 'none' : sideDone[0].subtitle_entries)
      + ' 文本对得上=' + (sideText === sideExpected));

  // 格式：明说的优先（扩展名 `.txt` 认不出来，所以只有 `--format` 能定它），
  // 而**两边打架**（文件名叫 .ass、内容说写 srt）与**都认不出来**都必须退 2 且不写文件 ——
  // 前者是"播放器打开只会说解析失败"的典型，后者是"猜错的方向正好是前者"。
  const assPath = join(TMP, 'side.txt');
  const ass = run(cli, ['render', '--project', subProject, '--asset-root', fixtureRoot,
    '--from', '0', '--to', '89', '--width', '64', '--height', '36',
    '--out', join(TMP, 'ass.mp4'), '--subtitle-out', assPath, '--format', 'ass']);
  const assText = existsSync(assPath) ? readFileSync(assPath, 'utf8') : '';
  const dialogues = assText.split('\n').filter((line) => line.startsWith('Dialogue:'));
  const clashPath = join(TMP, 'clash.ass');
  const clash = run(cli, ['render', '--project', subProject, '--asset-root', fixtureRoot,
    '--out', join(TMP, 'clash.mp4'), '--subtitle-out', clashPath, '--format', 'srt']);
  const weirdPath = join(TMP, 'side.weird');
  const weird = run(cli, ['render', '--project', subProject, '--asset-root', fixtureRoot,
    '--out', join(TMP, 'weird.mp4'), '--subtitle-out', weirdPath]);
  record('render-subtitle-out-format',
    ass.code === 1 && assText.startsWith('[Script Info]\n') && dialogues.length === 2
      && dialogues[1] === 'Dialogue: 0,0:00:02.00,0:00:03.00,Default,,0,0,0,,'
        + 'Mixed 混排 text with a rather long tail that ought to wrap somewhere'
      && clash.code === 2 && !existsSync(clashPath)
      && weird.code === 2 && !existsSync(weirdPath),
    'ass exit=' + ass.code + ' Dialogue=' + dialogues.length
      + ' 打架 exit=' + clash.code + ' 认不出 exit=' + weird.code);

  // 侧挂**先于出片**落地：字体给错时这一趟在开始出片之前就退 2，所以**不该留下文件** ——
  // 留下的那份会让调用方以为"产物与这份时间轴是一对"，而产物根本没出。
  // 这条同时钉住了**顺序**：反过来说，先写文件再检查字体，这个判据就会红。
  const refusedPath = join(TMP, 'refused.srt');
  const refused = run(cli, ['render', '--project', subProject, '--asset-root', fixtureRoot,
    '--from', '0', '--to', '89', '--width', '64', '--height', '36',
    '--out', join(TMP, 'refused.mp4'), '--font-file', 'fixtures/definitely-not-a-font.ttf',
    '--subtitle-out', refusedPath]);
  record('render-subtitle-out-refused', refused.code === 2 && !existsSync(refusedPath),
    'exit=' + refused.code + ' 留下了文件=' + existsSync(refusedPath));

  // 工程里没有字幕：文件**照样写出来**（空的），计数是 0。
  // 「没写文件」与「写了但没内容」在调用方那里是两件事，所以这条要分开判。
  const emptyPath = join(TMP, 'empty.srt');
  const empty = run(cli, ['render', '--project', PROJECT, '--from', '0', '--to', '9',
    '--width', '64', '--height', '36', '--out', join(TMP, 'empty.mp4'), '--subtitle-out', emptyPath]);
  const emptyDone = events(empty.stdout, 'done');
  record('render-subtitle-out-empty',
    empty.code === 0 && emptyDone.length === 1 && emptyDone[0].subtitle_entries === 0
      && existsSync(emptyPath) && statSync(emptyPath).size === 0,
    'exit=' + empty.code + ' entries=' + (emptyDone.length === 0 ? 'none' : emptyDone[0].subtitle_entries)
      + ' 大小=' + (existsSync(emptyPath) ? statSync(emptyPath).size : 'none'));

  // ---- 具名子命令（T7.1）：**同一实现的糖** ----
  //
  // 这一组问的是同一个问题：具名写法与等价的 `edit --op` 到底是不是同一件事。
  // 「同一实现」不许靠读代码相信 —— 这里比的是**产物字节**与 **stdout**，
  // 比"看起来一样"强：summary 文案、issues、缩进格式里任何一处分叉都会红。
  const fixtureText = readFileSync(join(REPO_ROOT, PROJECT), 'utf8');
  const fresh = (name) => {
    const path = join(TMP, name);
    writeFileSync(path, fixtureText, 'utf8');
    return path;
  };
  const same = (one, other) => readFileSync(one, 'utf8') === readFileSync(other, 'utf8');

  const clipPath = fresh('named-clip.json');
  const opPath = fresh('named-op.json');
  const clipRun = run(cli, ['clip', 'split', '--project', clipPath, '--layer', 'c', '--at', '75', '--write']);
  const opRun = run(cli, ['edit', '--project', opPath, '--op', '{"op":"split","layer":"c","at":75}', '--write']);
  record('clip-equivalence',
    clipRun.code === 0 && opRun.code === 0 && same(clipPath, opPath)
      && clipRun.stdout.trim() === opRun.stdout.trim(),
    'exit=' + clipRun.code + '/' + opRun.code + ' 产物相同=' + same(clipPath, opPath)
      + ' stdout相同=' + (clipRun.stdout.trim() === opRun.stdout.trim()));

  const seqPath = fresh('named-seq.json');
  const setPath = fresh('named-set.json');
  const seqRun = run(cli, ['sequence', 'set', '--project', seqPath, '--timebase', '30000/1001', '--write']);
  const setRun = run(cli, ['edit', '--project', setPath, '--op',
    '{"op":"set_sequence","timebase":{"num":30000,"den":1001},"width":0,"height":0}', '--write']);
  record('sequence-equivalence',
    seqRun.code === 0 && setRun.code === 0 && same(seqPath, setPath)
      && seqRun.stdout.trim() === setRun.stdout.trim(),
    'exit=' + seqRun.code + '/' + setRun.code + ' 产物相同=' + same(seqPath, setPath));

  // 撤销的具名写法与 `edit --undo`：连函数都是同一个，所以这条**本来**该是恒真的。
  // 留着它是因为"糖"最容易被写成第二份实现 —— 那正是这条要挡住的事。
  const undPath = fresh('named-undo.json');
  const flagPath = fresh('named-flag.json');
  const undHist = join(TMP, 'named-undo-history.json');
  const flagHist = join(TMP, 'named-flag-history.json');
  run(cli, ['clip', 'split', '--project', undPath, '--layer', 'c', '--at', '75', '--history', undHist, '--write']);
  run(cli, ['clip', 'split', '--project', flagPath, '--layer', 'c', '--at', '75', '--history', flagHist, '--write']);
  const undRun = run(cli, ['undo', '--project', undPath, '--history', undHist, '--write']);
  const flagRun = run(cli, ['edit', '--project', flagPath, '--history', flagHist, '--undo', '--write']);
  record('history-alias',
    undRun.code === 0 && flagRun.code === 0 && same(undPath, flagPath)
      && undRun.stdout.trim() === flagRun.stdout.trim(),
    'exit=' + undRun.code + '/' + flagRun.code + ' 产物相同=' + same(undPath, flagPath));

  // 干跑：不给 --write 就**一个字节都不许动**（连历史文件也不许碰）。
  const dryPath = fresh('named-dry.json');
  const dryBefore = readFileSync(dryPath, 'utf8');
  const dryRun = run(cli, ['clip', 'split', '--project', dryPath, '--layer', 'c', '--at', '75']);
  let dryEdit = null;
  try { dryEdit = JSON.parse(dryRun.stdout); } catch (error) { dryEdit = null; }
  record('clip-dry-run',
    dryRun.code === 0 && dryEdit !== null && dryEdit.written === false
      && readFileSync(dryPath, 'utf8') === dryBefore,
    'exit=' + dryRun.code + ' written=' + (dryEdit === null ? 'none' : dryEdit.written)
      + ' 文件没被动=' + (readFileSync(dryPath, 'utf8') === dryBefore));

  // 用法错一律退 2：不认识的动作名、以及**多给一个开关**。
  // 多给比少给更容易被放过（"反正不用它"），而它的表现是那个开关被静静丢掉。
  const badName = run(cli, ['clip', 'rotate', '--project', PROJECT]);
  const extraFlag = run(cli, ['clip', 'split', '--project', PROJECT, '--layer', 'c', '--at', '75',
    '--length', '2']);
  record('clip-rejects-bad-usage', badName.code === 2 && extraFlag.code === 2,
    'exit=' + badName.code + '/' + extraFlag.code);

  // 批处理 = 一次写、一条历史；**结果必须与逐条 edit 一模一样**。
  const scriptPath = join(TMP, 'named-batch.ndjson');
  writeFileSync(scriptPath,
    '# 两刀\n{"op":"split","layer":"c","at":75}\n\n{"op":"move","layer":"d","to":20,"track":"v2"}\n', 'utf8');
  const batchPath = fresh('named-batch.json');
  const stepPath = fresh('named-steps.json');
  const batchRun = run(cli, ['batch', '--project', batchPath, '--script', scriptPath, '--write']);
  run(cli, ['edit', '--project', stepPath, '--op', '{"op":"split","layer":"c","at":75}', '--write']);
  run(cli, ['edit', '--project', stepPath, '--op', '{"op":"move","layer":"d","to":20,"track":"v2"}', '--write']);
  record('batch-matches-edits',
    batchRun.code === 0 && same(batchPath, stepPath),
    'exit=' + batchRun.code + ' 产物相同=' + same(batchPath, stepPath));

  // 中途有一步不成立：**整份不落盘**（不是"做到哪算哪"），并明说卡在第几行。
  const badScript = join(TMP, 'named-bad.ndjson');
  writeFileSync(badScript,
    '{"op":"split","layer":"c","at":75}\n{"op":"split","layer":"nosuch","at":75}\n', 'utf8');
  const atomicPath = fresh('named-atomic.json');
  const atomicBefore = readFileSync(atomicPath, 'utf8');
  const atomicRun = run(cli, ['batch', '--project', atomicPath, '--script', badScript, '--write']);
  let atomicBody = null;
  try { atomicBody = JSON.parse(atomicRun.stdout); } catch (error) { atomicBody = null; }
  record('batch-atomic',
    atomicRun.code === 2 && readFileSync(atomicPath, 'utf8') === atomicBefore
      && atomicBody !== null && String(atomicBody.summary).includes('第 2 行'),
    'exit=' + atomicRun.code + ' 文件没被动=' + (readFileSync(atomicPath, 'utf8') === atomicBefore)
      + ' summary=' + (atomicBody === null ? 'none' : atomicBody.summary));

  // 空脚本**不算成功**：它会打印 ok 却什么都没做，与"脚本路径写错了"分不开。
  const emptyScript = join(TMP, 'named-empty.ndjson');
  writeFileSync(emptyScript, '# 只有注释\n\n', 'utf8');
  const emptyBatchPath = fresh('named-empty.json');
  const emptyBefore = readFileSync(emptyBatchPath, 'utf8');
  const emptyBatch = run(cli, ['batch', '--project', emptyBatchPath, '--script', emptyScript, '--write']);
  record('batch-empty',
    emptyBatch.code === 2 && readFileSync(emptyBatchPath, 'utf8') === emptyBefore,
    'exit=' + emptyBatch.code + ' 文件没被动=' + (readFileSync(emptyBatchPath, 'utf8') === emptyBefore));

  return observed;
}

function main() {
  const argv = process.argv.slice(2);
  if (argv.includes('--self-test')) { runSelfTest(); return; }
  const index = argv.indexOf('--cli');
  // 默认按**当前平台**的可执行名找（Windows 是 dhampir.exe，Linux/macOS 是 dhampir）。
  // 写死 .exe 的后果是：装对了二进制也报「先跑 cargo build」——那是环境差异，不是契约不成立。
  const cli = index >= 0
    ? argv[index + 1]
    : join(REPO_ROOT, 'target', 'debug', process.platform === 'win32' ? 'dhampir.exe' : 'dhampir');
  if (!existsSync(cli)) {
    console.error('找不到 dhampir 可执行文件：' + cli);
    console.error('先跑：cargo build -p dhampir-worker --bin dhampir');
    process.exitCode = 2;
    return;
  }
  // **先探一次"起不起得来"。**
  // 环境起不了子进程时（status=null），下面 30 条判据会一起变红，看上去像"CLI 全坏了"——
  // 那是误导，会把人送去读根本没坏的 CLI 代码。分开报，并且**照样红**（fail-closed）。
  const probe = run(cli, ['--help']);
  if (probe.code === -1) {
    console.error('起不了 dhampir：' + cli);
    console.error('  spawnError=' + probe.spawnError + '  stderr=' + probe.stderr.trim());
    console.error('这不是"CLI 不满足契约"，是**这个环境起不了子进程**。');
    console.error('实测签名：给子进程开 stdin 管道会 EBUSY（node 侧）/ os error 231（Rust 侧）。');
    console.error('本守卫已经把 stdin 关掉了（scripts/spawn-tool.mjs）；这里仍 -1 就请换普通终端复跑。');
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
  tryRemove(TMP);
}

main();
