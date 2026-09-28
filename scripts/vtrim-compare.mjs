// V-Trim ↔ Dhampir 逐步对比：**一步**跑完整条回路，并按窗口出表。
//
// # 这个脚本存在的理由：它把两个已经踩过的坑堵死了
//
// 我在手工搭这条回路时犯了两个错，**两次都产出了"看起来有据"的假结论**：
//
//   甲、**把源时间当输出时间取样。**
//       V-Trim 的 `polish.toml` 里事件时间是**源时间**，而 `mute_ranges` 会把中间
//       几段**掐掉**（本样例共 17.50 秒）。所以同一个事件在成片里出现的位置是
//       `源时间 - 被掐掉的长度`。我按源时间直接去 `-ss` 取帧，
//       于是量的是**完全不相干的时刻** —— 第一批结论（"blur 很好、flash 很差"）
//       整个是反的。
//       -> 本脚本**先算映射再取样**，并把映射表打出来。
//
//   乙、**在结论写完之前就把产物清理了。**
//       我第二次追一个"21.83 的差异"，回头要复核时发现基准文件已经被自己删了。
//       -> 本脚本把两边成片留在 `--out` 目录里，**报告写完才提示可以删**；
//          不传 `--clean` 就不动它。
//
// 另外它还有一条纪律：**用户工程必须恢复**。整个流程包在 try/finally 里，
// 无论中途哪一步失败（V-Trim 崩了、渲染失败、Ctrl-C），`polish.toml` 都还原。
//
// # 用法
//
//     node scripts/vtrim-compare.mjs \
//       --clip "E:/media/<项目>/output/clips/<片段>" \
//       --events out/fx-events.json \
//       --out out/vtrim-run
//
// `--events` 是一个 JSON 数组，描述**要插进 polish.toml 的测试事件**（源时间）：
//
//     [
//       { "type": "blur",    "time": 66.2 },
//       { "type": "stutter", "time": 68.2, "intensity": 30 },
//       { "type": "shake",   "time": 70.2, "intensity": 20 }
//     ]
//
// 不传 `--events` 就是"不插任何东西，只比现有工程"（那是**基线**，
// 用来确认两边本来就一致）。
//
// # 它怎么判
//
// 对每个事件，在**输出时间轴**上取它的窗口，量三样：
//
//   * **像素差**：整帧平均绝对差。要跟**窗口外的基线**比，不是跟 0 比 ——
//     字幕的字形栅格化（Chrome/Skia vs ffmpeg/FreeType）本身就带来 5~13 的底噪。
//   * **高频比**：本仓的拉普拉斯能量 / 参照的。`1.0` = 同锐利。
//     这个量能把"糊"和"锯齿"分开：**码率不够会变低，点采样会变高**。
//   * **最佳纵向位移**：能让差异最小的那个像素位移。`+0` 之外的值
//     基本都指向"位置不对"而不是"效果不对"。
//
// # 环境
//
// * `ffmpeg` / `ffprobe` 在 PATH 上
// * `dhampir.exe`（`--dhampir`，默认 `target/debug/dhampir.exe`）
// * `vtrim.exe`（`--vtrim`，不传就自己找 `.workbuddy/perf/bin/vtrim.exe`）
// * 转译器（`--translator` / `VTRIM_TRANSLATOR`，默认找 `<V-Trim>/tools/…`）——
//   **它在 V-Trim 那一侧**，见下面"转译器住在哪"
//
// **转译器不属于这个仓库**（它是 V-Trim 那一侧的东西，见 `docs/vtrim-integration.md`），
// 所以路径可配 —— 等它搬到 V-Trim 去了，这里传个路径就行。

import { execFileSync } from 'node:child_process';
import { copyFileSync, existsSync, mkdirSync, readFileSync, writeFileSync, rmSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';

const REPO_ROOT = resolve(new URL('..', import.meta.url).pathname.replace(/^\/([A-Za-z]:)/, '$1'));

// ---------------------------------------------------------------------------
// 参数
// ---------------------------------------------------------------------------

function parseArgs(argv) {
  const out = { events: null, clean: false, sampleStep: 0.05 };
  for (let i = 0; i < argv.length; i += 1) {
    const a = argv[i];
    if (a === '--clip') out.clip = argv[++i];
    else if (a === '--events') out.events = argv[++i];
    else if (a === '--out') out.out = argv[++i];
    else if (a === '--dhampir') out.dhampir = argv[++i];
    else if (a === '--vtrim') out.vtrim = argv[++i];
    else if (a === '--translator') out.translator = argv[++i];
    else if (a === '--font-file') out.fontFile = argv[++i];
    else if (a === '--clean') out.clean = true;
    else if (a === '--self-test') out.selfTest = true;
    else if (a === '--help' || a === '-h') out.help = true;
    else throw new Error(`不认识的参数：${a}`);
  }
  return out;
}

const USAGE = `用法：node scripts/vtrim-compare.mjs --clip <clip 目录> [选项]

  --clip <目录>         V-Trim 的片段目录（里面有 polish.toml / clip.mp4）
  --events <json>       要插进去的测试事件（JSON 数组，源时间）。不给 = 只比现有工程
  --out <目录>          运行目录（默认 out/vtrim-compare）
  --translator <路径>   转译器（默认找 ../V-Trim/tools/polish-to-dhampir.mjs；
                        也可用环境变量 VTRIM_TRANSLATOR）
  --dhampir <路径>      dhampir 可执行（默认 target/debug/dhampir.exe）
  --vtrim <路径>        vtrim 可执行（默认自动找）
  --font-file <路径>    字幕字体（默认 C:/Windows/Fonts/msyh.ttc）
  --clean               跑完删掉运行目录（**默认保留**，见文件头"坑乙"）
  --self-test           只跑自检（验证映射与插入，不碰工程）
`;

// ---------------------------------------------------------------------------
// 小工具
// ---------------------------------------------------------------------------

const log = (msg) => console.log(msg);
const step = (msg) => console.log(`\n=== ${msg} ===`);

function run(cmd, args, opts = {}) {
  return execFileSync(cmd, args, { encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'], ...opts });
}

/// ffprobe 的 `-of default=nw=1` 是 key=value —— **不要用 `csv=p=0`**：
/// 它是位置式的，字段一多一少就静默串位（本项目已因它错过三次）。
function probeVideo(path) {
  const text = run('ffprobe', [
    '-v', 'error', '-select_streams', 'v',
    '-show_entries', 'stream=nb_frames,r_frame_rate,duration,width,height',
    '-of', 'default=nw=1', path,
  ]);
  const fields = {};
  for (const line of text.split('\n')) {
    const at = line.indexOf('=');
    if (at > 0) fields[line.slice(0, at)] = line.slice(at + 1).trim();
  }
  const [num, den] = (fields.r_frame_rate || '0/1').split('/').map(Number);
  return {
    frames: Number(fields.nb_frames),
    fps: den ? num / den : 0,
    duration: Number(fields.duration),
    width: Number(fields.width),
    height: Number(fields.height),
  };
}

// ---------------------------------------------------------------------------
// polish.toml：极简读 + 单点写
// ---------------------------------------------------------------------------

/// 读出 `[[mute_ranges]]` 的区间（**源秒**）。这些段是**掐掉**而不是静音。
function readMuteRanges(toml) {
  const ranges = [];
  const re = /\[\[mute_ranges\]\]([\s\S]*?)(?=\n\[|$)/g;
  let m;
  while ((m = re.exec(toml)) !== null) {
    const start = /start\s*=\s*([\d.]+)/.exec(m[1]);
    const end = /end\s*=\s*([\d.]+)/.exec(m[1]);
    if (start && end) ranges.push([Number(start[1]), Number(end[1])]);
  }
  return ranges.sort((a, b) => a[0] - b[0]);
}

/// **源秒 -> 输出秒。** 减去在它之前被掐掉的总长度。
///
/// 这就是"坑甲"的那个函数。手工搭回路时我漏了它，于是量错了时刻。
function makeRemap(muteRanges) {
  return (sourceSec) => {
    let cut = 0;
    for (const [a, b] of muteRanges) {
      if (sourceSec <= a) break;
      cut += Math.min(sourceSec, b) - a;
    }
    return sourceSec - cut;
  };
}

/// 把测试事件插进**覆盖它那个源时间**的 segment 里。
///
/// 返回 `{ text, insertedAt }`。找不到就抛 —— **不静默丢**。
function insertEvents(toml, events) {
  const lines = toml.split('\n');
  // 每个 [[segments]] 的起止行与 start/end
  const segments = [];
  for (let i = 0; i < lines.length; i += 1) {
    if (lines[i].trim() !== '[[segments]]') continue;
    let start = null;
    let end = null;
    let stop = lines.length;
    for (let j = i + 1; j < lines.length; j += 1) {
      if (lines[j].trim() === '[[segments]]') { stop = j; break; }
      const s = lines[j].trim();
      const ms = /^start\s*=\s*([\d.]+)/.exec(s);
      const me = /^end\s*=\s*([\d.]+)/.exec(s);
      if (ms) start = Number(ms[1]);
      if (me) end = Number(me[1]);
    }
    segments.push({ head: i, stop, start, end });
  }
  // 从后往前插，行号才不会位移
  const sorted = [...events].sort((a, b) => b.time - a.time);
  const at = [];
  for (const ev of sorted) {
    const seg = segments.find((s) => s.start !== null && s.end !== null && ev.time >= s.start && ev.time < s.end);
    if (!seg) throw new Error(`事件 ${ev.type}@${ev.time}s 不落在任何 segment 里（源时间 ${segments.map((s) => `${s.start}-${s.end}`).join(', ')}）`);
    const body = [`[[segments.events]]`, `type = "${ev.type}"`, `time = ${ev.time}`];
    for (const [k, v] of Object.entries(ev)) {
      if (k === 'type' || k === 'time') continue;
      body.push(typeof v === 'string' ? `${k} = "${v}"` : `${k} = ${v}`);
    }
    body.push('');
    lines.splice(seg.stop, 0, ...body);
    at.push({ type: ev.type, sourceSec: ev.time, segment: `${seg.start}-${seg.end}` });
    // 插完之后各 segment 的行号都变了 -> 重新解析（事件不多，代价可忽略）
    return { text: lines.join('\n'), insertedAt: at };
  }
  return { text: toml, insertedAt: at };
}

// 逐条插（每条都重新解析一次行号，避免"插完一次后行号全错"）
function insertEventsOneByOne(toml, events) {
  let text = toml;
  const at = [];
  for (const ev of events) {
    const r = insertEvents(text, [ev]);
    text = r.text;
    at.push(...r.insertedAt);
  }
  return { text, insertedAt: at };
}

// ---------------------------------------------------------------------------
// 取帧与度量
// ---------------------------------------------------------------------------

/// 取一帧。**取不到就抛** —— 不要留一个"文件不存在"给下游去猜。
///
/// ffmpeg 在 `-ss` 超出片长时可能**退 0 但不写文件**，于是下游会拿到一个
/// 莫名其妙的 `FileNotFoundError`（我第一次真跑就是这样，报的是 Python 读不到 PNG，
/// 而真正的原因是"本仓只渲了 48 秒"）。
function grab(video, seconds, outPath) {
  try {
    run('ffmpeg', ['-v', 'error', '-ss', String(seconds), '-i', video, '-frames:v', '1', '-y', outPath]);
  } catch (error) {
    throw new Error(`取帧失败 ${video} @${seconds}s：${String(error.stderr || error.message).slice(0, 200)}`);
  }
  if (!existsSync(outPath)) {
    const info = probeVideo(video);
    throw new Error(
      `取帧没产出文件：${video} @${seconds}s（该视频只有 ${info.duration.toFixed(2)}s / ${info.frames} 帧）`,
    );
  }
  return outPath;
}

/// 度量交给一个 Python 片段（numpy 读 PNG 最省事；本仓的 scripts 一律零 npm 依赖）。
///
/// **每次调用只跑一次 Python、把整张表算完** —— 逐帧起进程会让整个回路慢一个数量级。
function measure(plan) {
  const py = `
import json, sys
import numpy as np
from PIL import Image
from numpy.lib.stride_tricks import sliding_window_view
plan = json.load(open(sys.argv[1], encoding='utf-8'))
def L(p):
    return np.asarray(Image.open(p).convert('RGB')).astype(float)
def lap(a):
    g = a[200:900].mean(axis=2)
    k = np.array([[0,1,0],[1,-4,1],[0,1,0]], float)
    return float((sliding_window_view(g,(3,3))*k).sum(axis=(2,3)).var())
def best_dy(a, r):
    A, R = a[200:900], r[200:900]
    best = None
    for dy in range(-24, 25):
        sc = float(np.abs(np.roll(A, dy, axis=0)[24:-24] - R[24:-24]).mean())
        if best is None or sc < best[0]: best = (sc, dy)
    return best
out = []
for row in plan:
    a = L(row['mine']); r = L(row['ref'])
    sc, dy = best_dy(a, r)
    out.append({'pixel': float(np.abs(a-r).mean()), 'ratio': lap(a)/max(lap(r), 1e-9), 'dy': dy, 'residual': sc})
print(json.dumps(out))
`;
  const tmpPy = join(plan.dir, '_measure.py');
  const tmpJson = join(plan.dir, '_plan.json');
  writeFileSync(tmpPy, py, 'utf8');
  writeFileSync(tmpJson, JSON.stringify(plan.rows), 'utf8');
  const text = run('python', [tmpPy, tmpJson]);
  return JSON.parse(text.trim().split('\n').pop());
}

// ---------------------------------------------------------------------------
// 自检：**验证那两个坑真的被堵住了**
// ---------------------------------------------------------------------------
//
// 一个只在"跑得通"时好看的脚本没有意义 —— 这一轮我犯的两个错都发生在
// "脚本看起来在正常工作"的时候。所以这里按**逐值**钉：
//
//   1. `remap` 对多段 `mute_ranges` 的映射（坑甲）
//   2. `insertEventsOneByOne` 在**多个事件落到不同 segment** 时行号不串（插一次
//      会让后面所有 segment 的行号位移，一次性从后往前插才对，逐条插则必须重解析）

function selfTest() {
  let checks = 0;
  const eq = (actual, expected, what) => {
    checks += 1;
    const a = typeof actual === 'number' ? Number(actual.toFixed(6)) : actual;
    const b = typeof expected === 'number' ? Number(expected.toFixed(6)) : expected;
    if (a !== b) throw new Error(`${what}：期望 ${b}，实得 ${a}`);
  };

  // ---- 1. 源 -> 输出映射 ----
  //
  // 掐掉 [10,12) 与 [20,23)：共 5 秒。
  //   源 5   （在第一段之前）      -> 5
  //   源 11  （在第一段里面）      -> 11 - 1 = 10
  //   源 12  （正好在掐掉段之后）  -> 12 - 2 = 10
  //   源 15                        -> 13
  //   源 21  （在第二段里面）      -> 21 - 2 - 1 = 18
  //   源 25                        -> 20
  const r = makeRemap([[10, 12], [20, 23]]);
  eq(r(5), 5, 'remap 在掐掉段之前');
  eq(r(11), 10, 'remap 在掐掉段里面');
  eq(r(12), 10, 'remap 正好在掐掉段之后');
  eq(r(15), 13, 'remap 在两段之间');
  eq(r(21), 18, 'remap 在第二个掐掉段里面');
  eq(r(25), 20, 'remap 在全部掐掉之后');
  // **核心**：把"减去掐掉长度"这一步去掉会得到什么 —— 必须与上面不同，
  // 否则这个自检没有分辨力。
  const identity = (s) => s;
  if (identity(25) === r(25)) throw new Error('自检失效：恒等映射与 remap 同值，说明用例选错了点');

  // 读 mute_ranges
  const tomlSample = [
    '[project]', 'name = "x"', '',
    '[[mute_ranges]]', 'start = 10.0', 'end = 12.0', '',
    '[[mute_ranges]]', 'start = 20.0', 'end = 23.0', '',
    '[stage]', 'blur_px = 28', '',
  ].join('\n');
  const ranges = readMuteRanges(tomlSample);
  eq(ranges.length, 2, 'mute_ranges 条数');
  eq(ranges[0][0], 10, 'mute_ranges[0].start');
  eq(ranges[1][1], 23, 'mute_ranges[1].end');
  // 不能把 `[stage] blur_px` 之类误当成区间
  eq(readMuteRanges('[stage]\nblur_px = 28\n').length, 0, '没有 mute_ranges 时应当是 0 条');

  // ---- 2. 插事件：多个事件落到不同 segment，行号不能串 ----
  const toml2 = [
    '[[segments]]', 'start = 0.0', 'end = 10.0', 'note = "a"', '',
    '[[segments]]', 'start = 10.0', 'end = 20.0', 'note = "b"', '',
    '[[segments]]', 'start = 20.0', 'end = 30.0', 'note = "c"', '',
  ].join('\n');
  const inserted = insertEventsOneByOne(toml2, [
    { type: 'blur', time: 2.0 },        // -> segment 0
    { type: 'stutter', time: 12.0, intensity: 30 }, // -> segment 1
    { type: 'shake', time: 25.0, intensity: 20 },   // -> segment 2
  ]);
  eq(inserted.insertedAt.length, 3, '三个事件都要插进去');
  eq(inserted.insertedAt.filter((x) => x.segment === '0-10').length, 1, 'segment 0 收到 1 条');
  eq(inserted.insertedAt.filter((x) => x.segment === '10-20').length, 1, 'segment 1 收到 1 条');
  eq(inserted.insertedAt.filter((x) => x.segment === '20-30').length, 1, 'segment 2 收到 1 条');
  // 每个事件块必须落在**它自己那个** segment 里（行号串了就会串到别的段）
  const blocks = inserted.text.split('[[segments]]');
  const inSeg = (i, ty) => blocks[i].includes(`type = "${ty}"`);
  if (!inSeg(1, 'blur')) throw new Error('blur 没落在第 1 个 segment 里（行号串了）');
  if (!inSeg(2, 'stutter')) throw new Error('stutter 没落在第 2 个 segment 里（行号串了）');
  if (!inSeg(3, 'shake')) throw new Error('shake 没落在第 3 个 segment 里（行号串了）');
  checks += 3;
  // 而且**段内顺序**不能把别段的 header 吃掉
  eq(blocks.length, 4, 'segment 头数量不变');

  // 事件落不进任何 segment 时必须**抛**，不能静默丢
  checks += 1;
  let threw = false;
  try {
    insertEventsOneByOne(toml2, [{ type: 'blur', time: 99.0 }]);
  } catch {
    threw = true;
  }
  if (!threw) throw new Error('事件落在所有 segment 之外时应当抛，实得静默通过');

  console.log(`✓ vtrim-compare 自检：${checks} 项全绿`);
  console.log('  钉住的是「源->输出映射」与「多事件插入不串行号」——这一轮我犯的两个错');
}

// ---------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------

function main() {
  const args = parseArgs(process.argv.slice(2));
  if (args.selfTest) {
    selfTest();
    return;
  }
  if (args.help || !args.clip) {
    console.log(USAGE);
    process.exit(args.help ? 0 : 2);
  }
  const clip = resolve(args.clip);
  const tomlPath = join(clip, 'polish.toml');
  if (!existsSync(tomlPath)) throw new Error(`找不到 ${tomlPath}`);

  const runDir = resolve(args.out || join(REPO_ROOT, 'out', 'vtrim-compare'));
  // 转译器**住在 V-Trim 那一侧**（见 docs/vtrim-integration.md 第十节）。
  //
  // 与 `vtrim` 同一个办法：候选列表 + 环境变量，全找不到就**大声报错并列出找过哪里**。
  // 「指一个相对路径」在这里是错的 —— Dhampir 与 V-Trim 是两个仓库，
  // `<Dhampir>/tools/…` 这个位置已经**不存在**了（那份是重复的，已删）。
  const translatorCandidates = [
    args.translator,
    process.env.VTRIM_TRANSLATOR,
    join(REPO_ROOT, '..', 'V-Trim', 'tools', 'polish-to-dhampir.mjs'),
    join(REPO_ROOT, '..', '..', 'V-Trim', 'tools', 'polish-to-dhampir.mjs'),
  ].filter(Boolean).map((p) => resolve(p));
  const translator = translatorCandidates.find((p) => existsSync(p));
  const dhampir = resolve(args.dhampir || join(REPO_ROOT, 'target', 'debug', 'dhampir.exe'));
  const fontFile = args.fontFile || 'C:/Windows/Fonts/msyh.ttc';
  // vtrim 可执行：按**候选列表**找，而不是猜一个相对路径。
  //
  // 我第一版按 `clip/../../../../..` 往上找五层 —— 那依赖片段在磁盘上的深度，
  // 换个项目就找不到（第一次真跑就是这么失败的）。候选列表把常见位置与环境变量
  // 都列出来，全找不到才退到 PATH，并在报错里**说清找过哪些地方**。
  const vtrimCandidates = [
    args.vtrim,
    process.env.VTRIM_EXE,
    join(REPO_ROOT, '..', 'V-Trim', '.workbuddy', 'perf', 'bin', 'vtrim.exe'),
    join(REPO_ROOT, '..', '..', 'V-Trim', '.workbuddy', 'perf', 'bin', 'vtrim.exe'),
  ].filter(Boolean).map((p) => resolve(p));
  const vtrim = vtrimCandidates.find((p) => existsSync(p)) || 'vtrim';

  if (!translator) {
    throw new Error(
      '找不到转译器（`polish-to-dhampir.mjs`）。它住在 V-Trim 那一侧，\n' +
      '  用 `--translator <路径>` 或环境变量 `VTRIM_TRANSLATOR` 显式指一个。找过：\n' +
      translatorCandidates.map((p) => `    ${p}`).join('\n'),
    );
  }
  if (!existsSync(dhampir)) throw new Error(`找不到 dhampir：${dhampir}`);
  log(`转译器：${translator}`);
  // `vtrim` 落回 PATH 时这里不报错（让它自己 spawn 失败），但**列清楚找过什么**。
  log(`vtrim：${existsSync(vtrim) ? vtrim : `${vtrim}（不在候选里，试 PATH）`}`);
  if (!existsSync(vtrim)) {
    log(`  找过：\n${vtrimCandidates.map((p) => `    ${p}`).join('\n')}`);
  }

  mkdirSync(runDir, { recursive: true });
  const originalToml = readFileSync(tomlPath, 'utf8');
  const backupPath = join(runDir, 'polish.toml.orig');
  const workingPath = join(runDir, 'polish.toml.test');

  const events = args.events ? JSON.parse(readFileSync(resolve(args.events), 'utf8')) : [];
  const muteRanges = readMuteRanges(originalToml);
  const remap = makeRemap(muteRanges);

  log(`片段：${clip}`);
  log(`运行目录：${runDir}`);
  log(`要插的测试事件：${events.length} 个`);

  // ---- 映射表：**先打出来**（坑甲）----
  step('源时间 -> 输出时间映射');
  log(`  mute_ranges（掐掉，不是静音）：${muteRanges.map(([a, b]) => `${a}-${b}`).join('  ') || '（无）'}`);
  const totalCut = muteRanges.reduce((s, [a, b]) => s + (b - a), 0);
  log(`  共掐掉 ${totalCut.toFixed(2)} 秒`);
  if (events.length) {
    log('  测试事件的落点：');
    for (const ev of events) {
      log(`     ${String(ev.type).padEnd(12)} 源 ${String(ev.time).padStart(7)}  ->  输出 ${remap(ev.time).toFixed(2)}`);
    }
  }

  let report = [];
  let failed = null;
  try {
    // ---- 准备 toml ----
    writeFileSync(backupPath, originalToml, 'utf8');
    let testToml = originalToml;
    if (events.length) {
      const r = insertEventsOneByOne(originalToml, events);
      testToml = r.text;
      for (const a of r.insertedAt) log(`  已插入 ${a.type}@${a.sourceSec}s 到 segment ${a.segment}`);
    }
    writeFileSync(workingPath, testToml, 'utf8');
    copyFileSync(workingPath, tomlPath);

    // ---- 转译 ----
    step('转译');
    const projectPath = join(runDir, 'project.json');
    const translateLog = run('node', [translator, clip, '--out', projectPath]);
    for (const line of translateLog.trim().split('\n').slice(0, 5)) log(`  ${line}`);

    // ---- 两边渲染 ----
    const info = probeVideo(join(clip, 'clip.mp4'));
    step(`渲染（源 ${info.width}x${info.height} ${info.fps}fps）`);
    const vtrimOut = join(clip, 'vtuber-clip_1.mp4');
    if (existsSync(vtrimOut)) rmSync(vtrimOut);
    log('  V-Trim（chrome 后端，会自动先跑 process）…');
    run(vtrim, ['polish', 'render', clip, '--renderer', 'chrome']);
    if (!existsSync(vtrimOut)) throw new Error(`V-Trim 跑完了却没产出 ${vtrimOut}`);
    const refPath = join(runDir, 'ref.mp4');
    copyFileSync(vtrimOut, refPath);
    rmSync(vtrimOut); // 工程目录立刻恢复干净
    const refInfo = probeVideo(refPath);
    log(`    参照 ${refInfo.frames} 帧 / ${refInfo.duration.toFixed(3)}s / ${refInfo.fps}fps  -> ${refPath}`);

    const minePath = join(runDir, 'mine.mp4');
    // **`--to` 要按本仓自己的帧率算，不能用参照的帧数。**
    //
    // 我第一版写的是 `refInfo.frames - 1` —— 参照是 **30fps**、本仓是 **60fps**，
    // 于是只渲了一半（到手 2907 帧 / 48.45s，而它该是 5812 帧 / 96.87s）。
    // 这是"两个宿主帧率不同"最基础的一处，而我恰恰写错了。
    const timeline = JSON.parse(readFileSync(projectPath, 'utf8')).timeline;
    const tb = timeline.timebase;
    const mineFps = tb.num / tb.den;
    // 工程的**总帧数**没有单列字段，由各层 `start + end` 的最大值决定。
    // 拿它跟"参照时长 × 本仓帧率"取小 —— 否则会比工程多出两三帧
    // （实测 5814 vs 工程的 5812），虽然不影响窗口内的取样，但会让报告里的
    // 帧数看着对不上，而"看着对不上"正是这份工具要消灭的东西。
    let projectFrames = 0;
    for (const track of timeline.tracks) {
      for (const layer of track.layers ?? []) {
        // `end` 是**绝对帧号**（不是长度）—— 我第一版写成 `start + end`，
        // 于是算出 11320 = 2 x 5660，钳位完全失效。
        projectFrames = Math.max(projectFrames, Number(layer.end));
      }
    }
    const byDuration = Math.round(refInfo.duration * mineFps);
    const lastFrame = Math.max(1, (projectFrames > 0 ? Math.min(byDuration, projectFrames) : byDuration) - 1);
    log(`  工程共 ${projectFrames} 帧，参照时长折合 ${byDuration} 帧 -> 出 0..${lastFrame}`);
    log(`  本仓时间基 ${tb.num}/${tb.den} = ${mineFps}fps -> 出 0..${lastFrame} 帧（参照 ${refInfo.frames} 帧 @${refInfo.fps}fps）`);
    run(dhampir, [
      'render', '--project', projectPath, '--from', '0', '--to', String(lastFrame),
      '--out', minePath, '--asset-root', clip, '--font-file', fontFile,
    ]);
    const mineInfo = probeVideo(minePath);
    log(`    本仓 ${mineInfo.frames} 帧 / ${mineInfo.duration.toFixed(3)}s / ${mineInfo.fps}fps  -> ${minePath}`);

    // ---- 采样 ----
    //
    // **一律用输出时间**（坑甲）。基线取窗口之外的几处。
    step('逐窗口度量');
    const frameDir = join(runDir, 'frames');
    mkdirSync(frameDir, { recursive: true });
    const rows = [];
    const sample = (tag, t) => {
      const m = join(frameDir, `${tag}-m-${t.toFixed(3)}.png`);
      const r = join(frameDir, `${tag}-r-${t.toFixed(3)}.png`);
      grab(minePath, t, m);
      grab(refPath, t, r);
      return { mine: m, ref: r };
    };

    // 基线：在事件窗口之外、且离得够远的时刻
    const winOf = (ev) => {
      const widths = { blur: 0.80, stutter: 0.12, shake: 0.20, flash: 0.44, hue_shift: 0.95,
        noise: 0.80, zoom_bounce: 0.30, pulse: 0.48, split: 0.12, color_shift: 1.00 };
      const outStart = remap(ev.time);
      const w = widths[ev.type] ?? 0.5;
      return { outStart, outEnd: outStart + w, width: w };
    };
    const busy = events.map((ev) => winOf(ev));
    const baselineTimes = [];
    for (let t = 2; t < refInfo.duration - 2 && baselineTimes.length < 6; t += 7.3) {
      if (busy.some((b) => t > b.outStart - 1.0 && t < b.outEnd + 1.0)) continue;
      baselineTimes.push(Number(t.toFixed(2)));
    }
    for (const [i, t] of baselineTimes.entries()) rows.push({ ...sample(`base${i}`, t), group: '基线', t });

    for (const [i, ev] of events.entries()) {
      const w = winOf(ev);
      // 窗口内均匀取 5 点（含两端略内缩，避开边界帧）
      const ts = [];
      for (let k = 0; k < 5; k += 1) {
        ts.push(Number((w.outStart + (w.width * (k + 0.5)) / 5).toFixed(3)));
      }
      for (const [k, t] of ts.entries()) rows.push({ ...sample(`ev${i}-${k}`, t), group: `${ev.type}@${ev.time}`, t });

      // -----------------------------------------------------------------
      // **局部基线**：紧邻窗口的几点（前后各两点）。
      //
      // 这一条是查 blur 时现出来的：全局基线 7.02 看着很干净，
      // 但 blur 的窗口（输出 60.90）**正好落在一条弹幕的滚动区间里**
      // （`56.13-67.65`），而那条弹幕本来就有一条已知差异
      // （逐条滚动时长 vs 轨道级平均，见 DROP/CLAMP 清单）。
      // 于是量到 26.17 全部记在 blur 头上 —— **而归因是错的**。
      //
      // 局部基线让"底噪本身就在变"这件事显形：判据是**跟邻居比**，
      // 而不是跟"全片的平均值"比。
      // -----------------------------------------------------------------
      const local = [w.outStart - 1.2, w.outStart - 0.6, w.outEnd + 0.6, w.outEnd + 1.2]
        .filter((x) => x > 0.5 && x < refInfo.duration - 0.5);
      for (const [k, x] of local.entries()) {
        rows.push({ ...sample(`loc${i}-${k}`, Number(x.toFixed(3))), group: `${ev.type}@${ev.time}#邻居`, t: Number(x.toFixed(3)) });
      }
    }

    const measured = measure({ dir: runDir, rows });
    const byGroup = new Map();
    rows.forEach((row, i) => {
      if (!byGroup.has(row.group)) byGroup.set(row.group, []);
      byGroup.get(row.group).push({ ...row, ...measured[i] });
    });

    const avg = (list, key) => list.reduce((s, x) => s + x[key], 0) / Math.max(list.length, 1);
    const base = byGroup.get('基线') || [];
    const basePixel = avg(base, 'pixel');
    const baseRatio = avg(base, 'ratio');

    step('结果');
    // **基线也要打 dy** —— 它是 dy 那一列的参照点。
    // 没有它，"blur 位移 -21px" 这句话就没法判：那是**本来就有的位置差**，
    // 还是**这个特效引入的**？（我第一版只打了像素差与高频比，于是拿到
    // -21 这个数时不知道该往哪查。）
    const baseDy = Math.round(avg(base, 'dy'));
    log(`  基线（窗口外 ${base.length} 处）：像素差 ${basePixel.toFixed(2)}   高频比 ${baseRatio.toFixed(2)}   位移 ${baseDy > 0 ? '+' : ''}${baseDy}px`);
    log('');
    log('  ' + '特效'.padEnd(20) + '输出窗口'.padEnd(16) + '像素差'.padStart(8) + '邻居'.padStart(9) + '高频比'.padStart(8) + '位移'.padStart(6) + '  判定');
    for (const ev of events) {
      const key = `${ev.type}@${ev.time}`;
      const g = byGroup.get(key);
      if (!g) continue;
      const w = winOf(ev);
      const pixel = avg(g, 'pixel');
      const ratio = avg(g, 'ratio');
      const dy = Math.round(avg(g, 'dy'));
      // **判据跟邻居比，不跟全片平均比**（见上面"局部基线"的理由）。
      const neighbour = byGroup.get(`${key}#邻居`) || [];
      const ref = neighbour.length >= 2 ? avg(neighbour, 'pixel') : basePixel;
      const bad = pixel > ref * 1.5 + 2;
      // **邻居自己也脏的时候，判据没力气** —— 要明说。
      //
      // 不说的话，"✓"会被读成"这个特效是对的"，而它实际只说明
      // "这个窗口跟它周围一样差"。两者的区别在**换一批测试时刻**时就显形了。
      const noisy = ref > basePixel * 2;
      const verdict = bad ? '✗ 高于邻居' : noisy ? '⚠ 底噪太脏，判据无效' : '✓';
      const sharp = ratio > 1.25 ? ' 锯齿' : ratio < 0.8 ? ' 偏糊' : '';
      const shift = dy !== 0 ? ` 位移${dy > 0 ? '+' : ''}${dy}px` : '';
      log('  ' + key.padEnd(20) + `${w.outStart.toFixed(2)}-${w.outEnd.toFixed(2)}`.padEnd(16)
        + pixel.toFixed(2).padStart(8) + ref.toFixed(2).padStart(9) + ratio.toFixed(2).padStart(8) + String(dy).padStart(6)
        + `  ${verdict}${sharp}${shift}`);
    }

    report = [...byGroup.entries()].map(([group, list]) => ({
      group, n: list.length, pixel: avg(list, 'pixel'), ratio: avg(list, 'ratio'), dy: Math.round(avg(list, 'dy')),
    }));
    writeFileSync(join(runDir, 'report.json'), JSON.stringify({
      clip, muteRanges, totalCut, events: events.map((e) => ({ ...e, outTime: remap(e.time) })),
      baseline: { pixel: basePixel, ratio: baseRatio }, groups: report,
      ref: refPath, mine: minePath, project: projectPath,
    }, null, 2) + '\n', 'utf8');
    log(`\n  报告：${join(runDir, 'report.json')}`);
  } catch (error) {
    failed = error;
  } finally {
    // **无论成败都恢复用户的 polish.toml。**
    try {
      writeFileSync(tomlPath, originalToml, 'utf8');
      log('\n  polish.toml 已恢复');
    } catch (restoreError) {
      console.error(`\n  ✗✗ polish.toml 恢复失败：${restoreError.message}`);
      console.error(`  原始内容在：${backupPath}`);
    }
    // 本仓自己的产物不要留在用户的片段目录里
    for (const stray of ['vtuber-clip_1.mp4']) {
      const p = join(clip, stray);
      if (existsSync(p)) { rmSync(p); log(`  已删 ${stray}（V-Trim 的产物，工程目录要干净）`); }
    }
  }

  if (failed) {
    console.error(`\n✗ 失败：${failed.message}`);
    process.exit(1);
  }
  if (args.clean) {
    rmSync(runDir, { recursive: true, force: true });
    log(`  已删运行目录 ${runDir}（--clean）`);
  } else {
    log('  **运行目录保留**（坑乙）：结论要复核时还得看它。确认无误后再删。');
  }
}

main();
