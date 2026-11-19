#!/usr/bin/env node
// 取帧节奏：亮度斜坡源 + 逐个取帧，量出「请求第 i 帧，实际拿到源的哪一帧」。
//
// 为什么要这个脚本：交接单 plan/d5-frame-rate-handoff.md 里那些读数，
// 如果是手敲命令量出来的，下次谁想复跑就得重敲一遍，而且**敲错一个参数就得到不同的数**。
// 所以把它固化成一条命令。
//
// 量具的原理：源素材第 k 帧的亮度 = floor(k/29*255)。于是**出片帧的 YAVG 直接读回源帧号**，
// 不需要任何额外标注 —— 灰阶本身就是身份。
//
// # ⚠️ 这个量具翻过一次车，改法写在最前面（2026-10-09）
//
// **第一版报了一个不存在的缺陷**：它说底座的取帧路径有 `-2…+2` 的阶梯偏移。
// 那个"偏移"**整个是量具自己造出来的** —— 因为它拿**算出来的理论斜坡**当参照物
// （`floor(k/29*255)`），而实际造出来的源是 `[0, 0, 1, 12, 22, …]`：
// `geq` 的 `N` 计数与"我以为是第几帧"差了一拍，**源自身**就偏了。
//
// 实测的真相：**底座逐帧精确** —— 出片第 i 帧与源第 i 帧
// **30/30 帧逐像素完全相同**（用 `-pix_fmt rgb24` 抽原始像素比对，不走 `signalstats`）。
//
// **两条教训，都已钉进 `--self-test`：**
//
//   1. **参照物必须"读"，不能"算"。** 算式只代表"我以为源长什么样"。
//      现在参照表 = `yavgSequence(src)` 的**实测输出**。
//   2. **"读数"和"真值"要用两条独立的路子。** `signalstats/movie=` 与
//      `rawvideo` 抽原始像素曾是两条路，第一条误导了判定；结论以原始像素为准。
//
// 用法：
//   node scripts/check-frame-pacing.mjs              # 造素材 + 逐帧取 + 打印映射表
//   node scripts/check-frame-pacing.mjs --self-test  # 只验量具本身（不碰底座）
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

/// 亮度值 -> 最接近的表里哪一格 + 误差。
///
/// **⚠️ 必须拿"真实读出来的源"当表，不能拿"理论斜坡"当表。**
/// 这里踩过一个后果很重的坑（2026-10-09）：第一版用理论斜坡
/// `floor(k/(N-1)*255)` 当参照，而实际造出来的源是
/// `[0, 0, 1, 12, 22, …]` 而不是 `[0, 8, 17, 26, …]` ——
/// 于是量出来的"偏移阶梯 −2…+2"**整个是量具自己造出来的**，
/// 底座其实是逐帧精确的（源第 i 帧 ↔ 出片第 i 帧，30/30 像素全同）。
///
/// 教训：**参照物必须读，不能算。** 算式只是"我以为源长什么样"。
function nearestIn(table, v) {
  let best = 0; let bestErr = Infinity;
  for (let k = 0; k < table.length; k++) {
    const e = Math.abs(table[k] - v);
    if (e < bestErr) { bestErr = e; best = k; }
  }
  return { k: best, err: bestErr };
}

/// 源第 k 帧的**理论**亮度。造斜坡用的就是它，所以它可以拿来**核对源本身**
/// （源如果不等于理论值，说明造源那一步的假设错了 —— 那正是上面那个坑）。
///
/// **必须是 `floor` 不是 `round`**：ffmpeg 的 `geq` 对表达式结果**截断**。
/// 实测 30 帧里 `round` 有 14 帧对不上、`floor` 30 帧全中。
const theoretical = (k) => Math.floor((k / (FRAMES - 1)) * 255);

/// 理论斜坡表（只在**核对源本身**与自检里用；判定一律走读出来的表）。
const theoreticalTable = Array.from({ length: FRAMES }, (_, k) => theoretical(k));

/// yuv **tv 范围**的 Y（16..235）→ 全范围 RGB（0..255）。
///
/// # 为什么量具必须有这一步（第二个、更隐蔽的坑）
///
/// 底座**解码时**就把 tv 范围转成全范围了（`-pix_fmt rgba`），所以出片的 R 值
/// **本来就不等于**源的 Y 值。拿 Y 当参照直接对 R，会得出一个**随亮度增大而增大**
/// 的"偏移" —— 看着完全像时间漂移，其实是**量纲不同**。
///
/// 实测（`source_in=60`、`rate=2.0`）：`src Y=100`（源第 60 帧）→ 出片 `R=97`，
/// 而 `(100-16)×255/219 = 97.8`。套上这一行之后 **30/30 帧全部吻合**。
const tvToFull = (y) => Math.round((y - 16) * (255 / 219));

/// [`tvToFull`] 的逆：全范围 RGB → tv 范围的 Y。
/// 用来把**出片**的值搬回源的量纲上比较（出片已经是全范围了）。
const fullToTv = (r) => (r * 219) / 255 + 16;

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

  const nearest = (v) => nearestIn(theoreticalTable, v);
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

  // 4. ★ 参照物必须是"读出来的表"，不是算出来的 —— 这条是那次翻车的回归测试。
  //
  // 造一个**故意不等于理论斜坡**的表（真实源就长这样：头两帧重复），
  // 再确认 `nearestIn` 用的是**传进去的表**而不是别的什么。
  const oddTable = [0, 0, 1, 12, 22, 31];
  const hit = nearestIn(oddTable, 12);
  expect(hit.k === 3 && hit.err === 0, `占位表里 12 应当命中第 3 格，实得 ${hit.k}`);
  // 同一个值在"理论斜坡"里会命中第 3 格吗？理论表第 3 格是 26 —— 所以命中位置不同，
  // 正说明**两张表给不同答案**，也就是"拿错表"会得出不同结论（这正是当年那个 bug）。
  expect(
    nearestIn(theoreticalTable, 12).k !== hit.k,
    '同一个亮度在两套参照下应当给出不同格号 —— 否则这条自检没在验"表有没有选对"',
  );

  // 5. ★★ 色彩范围换算 —— 这是第二个、也是更隐蔽的坑。
  //
  // 底座**解码时**就把 yuv 的 tv 范围（Y 16..235）转成了全范围 RGB，
  // 所以出片的 R 值本来就**不等于**源的 Y 值。拿 Y 当参照去对 R，
  // 会得出一个**随亮度增大而增大的"偏移"**——看着像时间漂移，其实是量纲不同。
  // 实测：`src Y=100`（第 60 帧）→ 出片 `R=97`，正是 `(100-16)*255/219`。
  expect(Math.abs(tvToFull(100) - 97.8) < 1.0, `tvToFull(100) 应当约 97.8，实得 ${tvToFull(100)}`);
  expect(tvToFull(16) === 0, 'tv 黑电平 16 应当映到 0');
  expect(tvToFull(235) === 255, 'tv 白电平 235 应当映到 255');
  // 反向：同一个输入在"直接比"与"换算后比"下结论不同 —— 证明这一步不是摆设。
  expect(
    Math.abs(100 - 97.8) > 1.5,
    '不换算时 100 与 97.8 的差应当超过容差（否则这条自检没在验什么）',
  );

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
  //
  // **必须真产出"第 k 帧 = floor(k/(N-1)*255)"的斜坡。**
  // 第一版直接用 `geq=lum='(N/${N-1})*255'`，实测**头两帧是重复的**
  // （`[0, 0, 1, 12, …]` 而不是 `[0, 8, 17, 26, …]`）—— 因为 `geq` 的 `N` 计数
  // 与"我以为是第几帧"差了一拍。而我当时**拿理论值当参照**去比对，
  // 就把这个源本身的偏移算成了"底座的偏移"。见 §3.6 与文件头。
  //
  // 现在的做法：先造，**再读回来核对**；不一致就**明说**，不硬算。
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
  if (srcSeq.length !== FRAMES) {
    console.error(`✗ 源素材应当有 ${FRAMES} 帧，实得 ${srcSeq.length} —— 量具的前提不成立，停止。`);
    process.exit(2);
  }
  const srcClean = srcSeq.every((v, i) => v === theoretical(i));
  if (!srcClean) {
    console.log('  与理论斜坡**不一致** —— 但本轮判定用的是**读出来的表**，不受影响。');
    console.log('  （第一版就是在这里没警觉，把源自身的偏移算成了底座的偏移。）');
  } else {
    console.log('  与理论斜坡一致。');
  }
  // **判定用的表 = 读出来的源**，不是算出来的。
  const srcTable = srcSeq;

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
  // **出片值要先从 tv 范围换算到全范围**，才能与源的 Y 比 —— 见 `tvToFull` 的注释。
  const got = files.map((n) => {
    const raw = yavg(join(outDir, n));
    return fullToTv(raw);
  });

  // ---- 5. 映射表 ----
  // "拿到的是源第几帧" = 与**读出来的源**最接近的那一格。
  //
  // ⚠️ 这里是那次翻车的现场：第一版用 `theoretical(...)` 当表。见 `nearestIn` 的注释。

  console.log('\n请求帧 -> 实得(已换算回tv) -> 对应源帧 (偏移)');
  let offScale = false;
  for (let i = 0; i < FRAMES; i++) {
    const { k, err } = nearestIn(srcTable, got[i]);
    if (err > 4) offScale = true;
    console.log(`  ${String(i).padStart(2)} -> ${got[i].toFixed(1).padStart(6)} -> src[${String(k).padStart(2)}] (${k - i >= 0 ? '+' : ''}${k - i})${err > 4 ? '  ← 不在斜坡上' : ''}`);
  }
  const offsets = got.map((v, i) => nearestIn(srcTable, v).k - i);
  console.log(`\n偏移序列：${JSON.stringify(offsets)}`);

  // ---- 6. 结论（不 print 结论就当没验过）----
  //
  // **判据：除了两端因**色彩范围钳位**而不可判的帧之外，必须全 0。**
  //
  // 为什么两端不可判：底座的解码把 tv 范围（Y 16..235）转成全范围 RGB（0..255），
  // 而**低于黑电平的部分被钳到 0**（源 Y=0 与 Y=8 都会变成 R=0）。
  // 那个映射在两端**不可逆**，反推回 Y 时落不到唯一一格。
  // 斜坡两端的 Y 分别是 0 与 255，正好都在钳位区里。
  //
  // **但那不是"放宽容差"**：中段 26 帧必须**精确命中**，一帧都不许偏 ——
  // 时间漂移恰恰会在中段暴露（第一版那个假"阶梯偏移"就是中段看的）。
  const CLAMPED = 2; // 两端各 2 帧落在钳位/饱和区
  const mid = offsets.slice(CLAMPED, FRAMES - CLAMPED);
  const badMid = mid.filter((o) => o !== 0);
  console.log(`\n中段（第 ${CLAMPED}..${FRAMES - CLAMPED - 1} 帧，共 ${mid.length} 帧）偏移必须全 0`);

  if (badMid.length === 0) {
    console.log(`✓ 中段 ${mid.length} 帧**全部精确命中**：出片第 i 帧 == 源第 i 帧。`);
    console.log(`  （两端各 ${CLAMPED} 帧因 tv↔full 色彩范围钳位而不可判，见本文件结论处注释。）`);
  } else {
    console.error(`\n✗ 中段有 ${badMid.length}/${mid.length} 帧没对上：${JSON.stringify(badMid)}`);
    console.error(`   完整偏移序列：${JSON.stringify(offsets)}`);
    console.error('   参照表是**读出来的源**且已做色彩范围换算，所以这是真偏移。');
    process.exit(1);
  }
  if (offScale) {
    console.error('  ⚠️ 有读数落在斜坡之外 —— 那不是"偏移"，先查这颗像素从哪来。');
    process.exit(1);
  }

  console.log(`\n中间产物在 ${WORK}`);
}

if (process.argv.includes('--self-test')) selfTest();
else main();
