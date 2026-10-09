#!/usr/bin/env node
// D5 最小复现：亮度斜坡源 + 逐个取帧，量出「请求第 i 帧，实际拿到源的哪一帧」。
//
// 为什么要这个脚本：交接单 plan/d5-frame-rate-handoff.md 里那些读数，
// 如果是手敲命令量出来的，下次谁想复跑就得重敲一遍，而且**敲错一个参数就得到不同的数**。
// 所以把它固化成一条命令。
//
// 量具的原理：源素材第 k 帧的亮度 = (k/29)*255。于是**出片帧的 YAVG 直接读回源帧号**，
// 不需要任何额外标注 —— 灰阶本身就是身份。
//
// 用法：
//   node scripts/d5-ramp-probe.mjs            # 造素材 + 逐帧取 + 打印映射表
//   node scripts/d5-ramp-probe.mjs --self-test # 只验量具本身（不碰底座）
//
// 前置：需要 `ffmpeg` / `ffprobe` 与已构建的 `target/debug/dhampir`。
import { execFileSync } from 'node:child_process';
import { existsSync, mkdirSync, readdirSync, rmSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const REPO = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const WORK = join(REPO, 'target', 'd5');
const EXE = join(REPO, 'target', 'debug', process.platform === 'win32' ? 'dhampir.exe' : 'dhampir');
const FRAMES = 30;
const FPS = 30;
const SIZE = '320x180';

function run(cmd, args, opts = {}) {
  return execFileSync(cmd, args, { encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'], ...opts });
}

/// 一帧的 YAVG（0..255）。读不到就抛 —— 静默给 0 会让"全黑"与"读失败"长得一样。
function yavg(file) {
  const v = yavgSequence(file)[0];
  if (v === undefined) throw new Error(`读不到 ${file} 的 YAVG`);
  return v;
}

/// 用 `movie=` + `signalstats` 读出每帧的 YAVG。
///
/// **为什么要 cd 到文件所在目录、只传文件名**（2026-10-09 踩到）：
/// lavfi 的 `movie=` **不能吃 Windows 绝对路径** —— `F:\a\b.mp4` 里的 `:` 是 filtergraph
/// 的选项分隔符、`\` 是它自己的转义符，两样都占。实测这些写法**全部失败**：
///
/// | 写法 | 结果 |
/// |---|---|
/// | `movie=F:\a\b.mp4` | `Failed to avformat_open_input 'F'` |
/// | `movie=F\:/a/b.mp4` | `Failed to avformat_open_input 'F;C'` |
/// | `movie='F:/a/b.mp4'` | `Failed to avformat_open_input 'F'` |
/// | **cd 到目录、只写文件名** | ✅ 正常 |
///
/// 相对路径只在 `movie=` 的解析里生效（它按进程 cwd 找），所以把 cwd 交给它。
/// 这样也不用管平台差异 —— 全路径干脆不进 filtergraph。
function yavgSequence(file) {
  const dir = dirname(file);
  const base = file.slice(dir.length + 1);
  return run('ffprobe', [
    '-v', 'error',
    '-f', 'lavfi',
    '-i', `movie=${base},signalstats`,
    '-show_entries', 'frame_tags=lavfi.signalstats.YAVG',
    '-of', 'csv=p=0',
  ], { cwd: dir })
    .split('\n')
    .map((s) => s.trim().replace(/,$/, ''))
    .filter((s) => /^\d+$/.test(s))
    .map(Number);
}

/// 亮度值 -> 最接近的源帧号（0-based）+ 误差。
///
/// 误差是有用的：斜坡每格约 8.8，所以 **err > 4 就意味着这个值根本不在斜坡上**
/// —— 那不是"偏移"，是别的问题（典型是纯黑帧被静默当成 src[0]）。
function nearestSourceFrame(v) {
  let best = 0; let bestErr = Infinity;
  for (let k = 0; k < FRAMES; k++) {
    const e = Math.abs(theoretical(k) - v);
    if (e < bestErr) { bestErr = e; best = k; }
  }
  return { k: best, err: bestErr };
}

/// 源第 k 帧（0-based）的**理论**亮度。斜坡源就是这个式子造的，所以它是真值。
///
/// **必须是 `floor` 不是 `round`**：ffmpeg 的 `geq` 对表达式结果**截断**。
/// 实测 30 帧里 `round` 有 14 帧对不上、`floor` 30 帧全中 —— 用 `round` 会误报
/// "源不合格"，然后后面整张偏移表都不可信。
const theoretical = (k) => Math.floor((k / (FRAMES - 1)) * 255);

/// 量具自检。**为什么必须有**：这个脚本的全部输出都是"读数"，
/// 而量具本身错了的话，读数会**看起来很正常**（一张整齐的偏移表）。
/// 所以先证明"喂进去一个已知输入，它能给出已知答案"。
///
/// 这里不碰底座（不需要 GPU/构建产物），只验三件事：
/// 1. `theoretical` 用 `floor`（用 `round` 会误报源不合格 —— 真踩过）；
/// 2. 最近匹配能找到正确的源帧；
/// 3. **偏离斜坡的读数会被认出来**（否则"全黑帧"会被静默算成"偏移 0"）。
function selfTest() {
  let checks = 0;
  const bad = [];
  const expect = (ok, label) => { checks += 1; if (!ok) bad.push(label); };

  // 1. floor 而不是 round
  expect(theoretical(1) === 8, `theoretical(1) 应为 8（floor），实得 ${theoretical(1)}`);
  expect(theoretical(2) === 17, `theoretical(2) 应为 17（floor），实得 ${theoretical(2)}`);
  expect(theoretical(0) === 0 && theoretical(29) === 255, '两端应恰为 0 与 255');
  // 反向：round 会给 9 —— 如果哪天被改回 round，这条会红
  expect(theoretical(1) !== 9, 'theoretical(1) 不该是 9（那是 round 的结果）');

  const nearest = nearestSourceFrame;
  // 2. 精确命中
  for (const k of [0, 1, 7, 15, 29]) {
    const r = nearest(theoretical(k));
    expect(r.k === k && r.err === 0, `nearest(theoretical(${k})) 应命中 ${k}，实得 ${r.k}（err=${r.err}）`);
  }
  // 3. 斜坡之外要被认出来（err 大于容差）—— 全黑帧就是这种情况
  expect(nearest(0).err === 0, '纯黑应当恰好命中 src[0]');
  expect(nearest(1000).err > 4, '远超斜坡的值应当被判为"不在斜坡上"');
  // 注意：**别断言 `nearest(250).k === 29`** —— 250 离 src[28]=246（err 4）比
  // 离 src[29]=255（err 5）更近，所以正确答案是 28。第一版这里写错成 29，
  // 是这条自检把它抓出来的（这就是自检该干的事）。
  expect(nearest(250).k === 28, `250 应归到 src[28]（err 4），实得 ${nearest(250).k}`);
  expect(nearest(255).k === 29, '满亮应当归到末帧');

  if (bad.length > 0) {
    console.error('✗ 自检失败（先修量具，别信它的读数）：');
    for (const b of bad) console.error(`    - ${b}`);
    process.exit(2);
  }
  console.log(`✓ check-frame-pacing 自检：${checks} 项全绿`);
}

function main() {
  if (!existsSync(EXE)) {
    console.error(`没找到 ${EXE}，先跑：cargo build -p dhampir-worker --bin dhampir`);
    process.exit(2);
  }
  mkdirSync(WORK, { recursive: true });

  // ---- 1. 造素材 ----
  // **无损（-qp 0）**：有损编码会动 YAVG，让"读回来的帧号"带上 ±1 的不确定度。
  const src = join(WORK, 'ramp.mp4');
  rmSync(src, { force: true });
  run('ffmpeg', [
    '-y', '-v', 'error',
    '-f', 'lavfi',
    '-i', `color=c=black:s=${SIZE}:r=${FPS}:d=1,geq=lum='(N/${FRAMES - 1})*255':cb=128:cr=128`,
    '-c:v', 'libx264', '-qp', '0', '-pix_fmt', 'yuv444p',
    src,
  ]);
  const srcSeq = yavgSequence(src);
  console.log(`源素材：${srcSeq.length} 帧`);
  console.log(`  实测 YAVG：${srcSeq.join(' ')}`);
  const srcClean = srcSeq.every((v, i) => v === theoretical(i));
  console.log(`  与理论斜坡一致：${srcClean ? '是' : '否（后面的偏移判定会不准）'}`);

  // ---- 2. 工程（自包含：不依赖 fixtures 里的任何素材）----
  const doc = {
    project_schema: 1,
    generator: { app: 'dhampir', version: '0.2.0' },
    meta: { title: 'D5 最小复现：亮度斜坡源 + 30 帧工程（自包含）', created_at: null, modified_at: null },
    assets: [{
      id: 'ramp.mp4', kind: 'video', name: '亮度斜坡（帧号 = 亮度刻度）',
      uri: 'ramp.mp4', frame_count: FRAMES,
      timebase: { num: FPS, den: 1 }, width: 320, height: 180,
      content_hash: null, tags: {},
      note: '每帧亮度 = 帧号线性换算，所以出片帧的 YAVG 直接读回源帧号。',
    }],
    timeline: {
      // schema 3 = 与 fixtures 里的工程同一代；速率字段**在这一代不存在**。
      schema: 3,
      timebase: { num: FPS, den: 1 },
      markers: [],
      tracks: [{
        id: 'v1', kind: 'video',
        layers: [{
          id: 'ramp', start: 0, end: FRAMES,
          source: { asset_id: 'ramp.mp4', source_in: 0 },
          note: '契约里没有任何"速率"字段 —— 这一条就是 D5 的争点。',
        }],
      }],
    },
    view: { playhead: 0, selection: 'ramp', zoom: 1 },
    render_hints: { width: 320, height: 180, format: 'mp4' },
    extensions: {},
  };
  const docPath = join(WORK, 'd5-ramp.doc.json');
  writeFileSync(docPath, JSON.stringify(doc, null, 2) + '\n');

  // ---- 3. probe：应当 errors/warnings 都空 ----
  const probe = JSON.parse(run(EXE, ['probe', '--project', docPath, '--asset-root', WORK]));
  console.log(`\nprobe：errors=${probe.errors.length} warnings=${probe.warnings.length}`);

  // ---- 4. 逐帧取 ----
  const outDir = join(WORK, 'frames');
  rmSync(outDir, { recursive: true, force: true });
  mkdirSync(outDir, { recursive: true });
  for (let f = 0; f < FRAMES; f++) {
    run(EXE, ['frame', '--project', docPath, '--asset-root', WORK, '--frame', String(f), '--out', outDir]);
  }
  const files = readdirSync(outDir).filter((n) => n.endsWith('.png')).sort();
  if (files.length !== FRAMES) {
    console.error(`应当有 ${FRAMES} 张 PNG，实得 ${files.length}`);
    process.exit(1);
  }
  const got = files.map((n) => yavg(join(outDir, n)));

  // ---- 5. 映射表 ----
  // "拿到的是源第几帧" = 与理论斜坡最接近的那一格（共享的 `nearestSourceFrame`）。

  console.log('\n请求帧 -> 实得 YAVG -> 对应源帧 (偏移)');
  let offScale = false;
  for (let i = 0; i < FRAMES; i++) {
    const { k, err } = nearestSourceFrame(got[i]);
    if (err > 4) offScale = true;
    console.log(`  ${String(i).padStart(2)} -> ${String(got[i]).padStart(3)} -> src[${String(k).padStart(2)}] (${k - i >= 0 ? '+' : ''}${k - i})${err > 4 ? '  ← 不在斜坡上' : ''}`);
  }
  const offsets = got.map((v, i) => nearestSourceFrame(v).k - i);
  console.log(`\n偏移序列：${JSON.stringify(offsets)}`);
  const perfect = offsets.every((o) => o === 0);
  console.log(`1:1 是否精确：${perfect ? '是' : '否'}`);

  // ---- 6. 结论（不 print 结论就当没验过）----
  //
  // **退出码的约定**（D24 未修期间用它把"已知缺陷"与"回归"分开）：
  //   * 漂移**与台账记录的一致** ⇒ exit 0，但**下面会说清它是缺陷**；
  //   * 漂移**变了**（好了或坏了）⇒ exit 1 —— 两种情况都要人来看：
  //     好了说明 D24 修了，该把台账划掉、把这段已知值删掉；
  //     坏了说明又恶化了。
  //
  // 这样做是因为本仓的守卫 runner **没有"允许红"机制**：直接断言"必须 1:1"会让
  // 整套守卫从此常红（而常红的判据等于没有判据）。所以把"当前已知形状"钉成常量，
  // 让判据在**变化**时响 —— 修好那天它会红，提醒把这个常量换成"必须全 0"。
  const KNOWN_OFFSETS = [0, -1, -2, -2, -1, -1, -1, -1, -1, -1, -1, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 1, 1, 2, 2, 2, 2, 2, 1, 0];
  const sameAsKnown = JSON.stringify(offsets) === JSON.stringify(KNOWN_OFFSETS);

  if (perfect) {
    console.log('\n✓ 取帧路径是精确 1:1 的 —— 可以删掉本文件里的 KNOWN_OFFSETS 与 D24 了。');
  } else if (sameAsKnown) {
    console.log('\n⚠️ 有漂移，**与台账 D24 记录的形状一致**（这仍是缺陷，不是通过）:');
    console.log('  这是 D24 的已知表现，不是新回归。判据会在它**变化**时红。');
    console.log('  详见 plan/d5-frame-rate-handoff.md');
    process.exit(0);
  } else {
    console.error('\n✗ 漂移形状**与 D24 记录的不一致** —— 要么变好了、要么恶化了，两种都要人来看：');
    console.error(`   记录: ${JSON.stringify(KNOWN_OFFSETS)}`);
    console.error(`   实得: ${JSON.stringify(offsets)}`);
    if (perfect) console.error('   （实得是精确 1:1 ⇒ D24 可能已修：请划掉台账条目并删掉 KNOWN_OFFSETS）');
    process.exit(1);
  }
  if (offScale) {
    console.log('  ⚠️ 有读数落在斜坡之外 —— 那不是"偏移"，先查这颗像素从哪来。');
  }
  console.log(`\n中间产物在 ${WORK}`);
}

if (process.argv.includes('--self-test')) selfTest();
else main();
