// 生成 / 核对**测试素材**：`target/s3/*.mp4`（gitignore 的草稿区，不进仓库）。
//
// # 为什么要有这个脚本
//
// `fixtures/*.doc.json` 与若干守卫（`check-cli` / `check-local-backend` / `web-check`）
// 都指着 `target/s3/proxy1080p.mp4`，而**仓库里既没有这个文件、也没有它的生成命令** ——
// 2026-09-30 复查时实测：换台机器或清过 `target/` 之后，`check-cli` 直接红在 `info` 那一条上，
// 而报出来的形状像"CLI 契约坏了"。**口径只该有一处**，所以钉在这里。
//
// 命令来源：`plan/s3.1-source-frame-sampling.md`（合成源）与 `plan/s3.2-proxy-spec.md`（代理）。
// 除了一句 `-v error`（免得刷屏），**参数与那两份文档逐字相同**。
//
// # 用法
//
//     node scripts/make-test-media.mjs              # 生成到 target/s3（缺哪个补哪个）
//     node scripts/make-test-media.mjs --check      # 只核对现有文件，不生成
//     node scripts/make-test-media.mjs --out <目录> # 换输出目录
//     node scripts/make-test-media.mjs --force      # 已存在也重生成
//     node scripts/make-test-media.mjs --self-test  # 只跑自检（不碰 ffmpeg、不写盘）
//
// 退出码：**0** 成功 / **1** 核对不上 / **2** 用法错、或 ffmpeg/ffprobe 不在 PATH 上。
// 判据一律看退出码，不要 grep 日志 —— 与本仓别的脚本同一条纪律。

import { execFileSync } from 'node:child_process';
import { copyFileSync, existsSync, mkdirSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const REPO = resolve(fileURLToPath(new URL('..', import.meta.url)));

// ---------------------------------------------------------------- 口径（唯一真相）
// `args` 是**不含程序名、不含输出路径**的参数；`from` 给定时，输入是同一目录里的另一个产物。
// `width`/`height`/`frames`/`gop` 是**核对用的期望值** —— 它们不是"大概"，核对不上就退 1。
const SPECS = [
  {
    name: 'source1080p.mp4',
    note: 'S3 的合成源（testsrc2，1080p60，8s，关键帧每 60 帧）',
    args: ['-f', 'lavfi', '-i', 'testsrc2=size=1920x1080:rate=60', '-t', '8',
      '-c:v', 'libx264', '-preset', 'veryfast', '-g', '60', '-keyint_min', '60',
      '-sc_threshold', '0', '-crf', '23', '-pix_fmt', 'yuv420p'],
    width: 1920, height: 1080, frames: 480, gop: 60,
  },
  {
    name: 'proxy1080p.mp4',
    note: 'fixture 与守卫引用的那个名字；与 source1080p 同一份（直接复制，不重编）',
    from: 'source1080p.mp4',
    width: 1920, height: 1080, frames: 480, gop: 60,
  },
  {
    name: 'proxy720p.mp4',
    note: '720p 代理（s3.2 的规格）',
    from: 'source1080p.mp4',
    vf: 'scale=-2:720',
    args: ['-c:v', 'libx264', '-preset', 'veryfast', '-g', '60', '-keyint_min', '60',
      '-sc_threshold', '0', '-crf', '23', '-an'],
    width: 1280, height: 720, frames: 480, gop: 60,
  },
  {
    name: 'sparse1080p.mp4',
    note: '稀疏关键帧源（GOP 250）—— 顺序解码的回退量化用它',
    args: ['-f', 'lavfi', '-i', 'testsrc2=size=1920x1080:rate=60', '-t', '8',
      '-c:v', 'libx264', '-preset', 'veryfast', '-g', '250', '-sc_threshold', '40',
      '-crf', '23', '-pix_fmt', 'yuv420p'],
    width: 1920, height: 1080, frames: 480, gop: 250,
  },
  {
    name: 'sparse-proxy720p.mp4',
    note: '稀疏源的 720p 代理',
    from: 'sparse1080p.mp4',
    vf: 'scale=-2:720',
    args: ['-c:v', 'libx264', '-preset', 'veryfast', '-g', '60', '-keyint_min', '60',
      '-sc_threshold', '0', '-crf', '23', '-an'],
    width: 1280, height: 720, frames: 480, gop: 60,
  },
];

// ---------------------------------------------------------------- 参数
const argv = process.argv.slice(2);
function arg(name, fallback) {
  const i = argv.indexOf(name);
  return i >= 0 && argv[i + 1] && !argv[i + 1].startsWith('--') ? argv[i + 1] : fallback;
}
const has = (name) => argv.includes(name);

const USAGE = `用法：node scripts/make-test-media.mjs [选项]

  --out <目录>   输出目录（默认 target/s3）
  --check        只核对现有文件，不生成
  --force        已存在也重生成
  --self-test    只跑自检（不碰 ffmpeg、不写盘）
`;

if (has('--help') || has('-h')) { console.log(USAGE); process.exit(0); }
const known = new Set(['--out', '--check', '--force', '--self-test', '--help', '-h']);
for (let i = 0; i < argv.length; i += 1) {
  if (!known.has(argv[i])) { console.error(`✗ 不认识的参数：${argv[i]}\n\n${USAGE}`); process.exit(2); }
  if (argv[i] === '--out') i += 1;
}

// ---------------------------------------------------------------- 命令构造（可被冻住）
/** 生成某一件素材的**完整 argv**（不含程序名）。 */
export function buildArgs(spec, outDir) {
  const out = join(outDir, spec.name);
  if (spec.from) {
    const input = join(outDir, spec.from);
    return spec.vf
      ? ['-v', 'error', '-y', '-i', input, '-vf', spec.vf, ...spec.args, out]
      : ['-v', 'error', '-y', '-i', input, '-c', 'copy', out];
  }
  return ['-v', 'error', '-y', ...spec.args, out];
}

/** 核对一件素材的 ffprobe argv（键=值 出口，**不按位置解 CSV**）。 */
export function buildProbeArgs(file) {
  return ['-v', 'error', '-select_streams', 'v:0', '-count_frames',
    '-show_entries', 'stream=width,height,r_frame_rate,nb_read_frames', '-of', 'default=nw=1', file];
}

// ---------------------------------------------------------------- 自检
function selfTest() {
  let checks = 0;
  const fail = [];
  const expect = (ok, label) => { checks += 1; if (!ok) fail.push(label); };

  // 名字唯一：重名会让"缺哪个补哪个"这句话失效
  expect(new Set(SPECS.map((s) => s.name)).size === SPECS.length, 'SPECS 里有重名');

  // 冻结 argv：改了参数就必须同时改这里（否则"口径唯一"只是一句话）
  const src = SPECS.find((s) => s.name === 'source1080p.mp4');
  expect(JSON.stringify(buildArgs(src, 'target/s3')) === JSON.stringify([
    '-v', 'error', '-y', '-f', 'lavfi', '-i', 'testsrc2=size=1920x1080:rate=60', '-t', '8',
    '-c:v', 'libx264', '-preset', 'veryfast', '-g', '60', '-keyint_min', '60',
    '-sc_threshold', '0', '-crf', '23', '-pix_fmt', 'yuv420p',
    join('target/s3', 'source1080p.mp4'),
  ]), 'source1080p 的 argv 与冻结值不一致');

  // 有 from 的必须是"复制"或"带 -vf 的重编"，不许悄悄换了输入
  for (const s of SPECS) {
    if (!s.from) continue;
    expect(SPECS.some((o) => o.name === s.from), `${s.name} 的 from 指向不存在的产物`);
    const a = buildArgs(s, 'target/s3');
    expect(a.includes(join('target/s3', s.from)), `${s.name} 的输入不是 ${s.from}`);
    expect(a[a.length - 1] === join('target/s3', s.name), `${s.name} 的输出不是自己`);
  }

  // 核对用的期望值必须是正数（0 会让核对恒真）
  for (const s of SPECS) {
    for (const k of ['width', 'height', 'frames', 'gop']) {
      expect(Number.isInteger(s[k]) && s[k] > 0, `${s.name} 的 ${k} 不是正整数`);
    }
  }
  expect(buildProbeArgs('x.mp4').includes('default=nw=1'), 'probe 出口不是键=值');

  if (fail.length > 0) {
    console.error('✗ 自检失败（先修脚本，别信它的结论）：');
    for (const f of fail) console.error(`    - ${f}`);
    process.exit(2);
  }
  console.log(`✓ make-test-media 自检：${checks} 项全绿`);
}

// ---------------------------------------------------------------- 核对
function probe(file) {
  const text = execFileSync('ffprobe', buildProbeArgs(file), { encoding: 'utf8' });
  const facts = {};
  for (const line of text.split('\n')) {
    const eq = line.indexOf('=');
    if (eq > 0) facts[line.slice(0, eq)] = line.slice(eq + 1).trim();
  }
  return {
    width: Number(facts.width),
    height: Number(facts.height),
    rate: facts.r_frame_rate,
    frames: Number(facts.nb_read_frames),
  };
}

function keyframeCount(file) {
  const text = execFileSync('ffprobe', ['-v', 'error', '-select_streams', 'v:0',
    '-skip_frame', 'nokey', '-show_entries', 'frame=pts_time', '-of', 'csv=p=0', file],
    { encoding: 'utf8', maxBuffer: 64 * 1024 * 1024 });
  return text.split('\n').filter((l) => l.trim() !== '').length;
}

function check(spec, outDir) {
  const file = join(outDir, spec.name);
  if (!existsSync(file)) return `不在：${file}`;
  const got = probe(file);
  const problems = [];
  if (got.width !== spec.width) problems.push(`宽 ${got.width} != ${spec.width}`);
  if (got.height !== spec.height) problems.push(`高 ${got.height} != ${spec.height}`);
  if (got.frames !== spec.frames) problems.push(`帧数 ${got.frames} != ${spec.frames}`);
  if (got.rate !== '60/1') problems.push(`帧率 ${got.rate} != 60/1`);
  const keys = keyframeCount(file);
  const wantKeys = Math.floor((spec.frames - 1) / spec.gop) + 1;
  if (keys !== wantKeys) problems.push(`关键帧 ${keys} 个 != ${wantKeys} 个（GOP ${spec.gop}）`);
  return problems.length > 0 ? problems.join('；') : null;
}

// ---------------------------------------------------------------- main
if (has('--self-test')) { selfTest(); process.exit(0); }

const outDir = resolve(arg('--out', join(REPO, 'target', 's3')));
for (const bin of ['ffmpeg', 'ffprobe']) {
  try { execFileSync(bin, ['-version'], { stdio: 'ignore' }); }
  catch { console.error(`✗ PATH 上没有 ${bin} —— 先装 ffmpeg（README 的"装什么"那张表）。`); process.exit(2); }
}

mkdirSync(outDir, { recursive: true });
const onlyCheck = has('--check');
let bad = 0;

for (const spec of SPECS) {
  const file = join(outDir, spec.name);
  if (onlyCheck) {
    const problem = check(spec, outDir);
    if (problem) { console.error(`✗ ${spec.name}：${problem}`); bad += 1; }
    else console.log(`✓ ${spec.name} 符合口径（${spec.width}x${spec.height} / ${spec.frames} 帧 / GOP ${spec.gop}）`);
    continue;
  }
  if (existsSync(file) && !has('--force')) {
    const problem = check(spec, outDir);
    if (!problem) { console.log(`= ${spec.name} 已在且符合口径（要重生成加 --force）`); continue; }
    console.log(`! ${spec.name} 已在但不符合口径（${problem}）→ 重新生成`);
  }
  if (spec.from && !spec.vf) {
    copyFileSync(join(outDir, spec.from), file);
    console.log(`✓ ${spec.name}（复制自 ${spec.from}）`);
  } else {
    try {
      execFileSync('ffmpeg', buildArgs(spec, outDir), { stdio: ['ignore', 'ignore', 'inherit'] });
    } catch (error) {
      console.error(`✗ 生成 ${spec.name} 失败：${error.message}`);
      process.exit(1);
    }
    console.log(`✓ ${spec.name}（${spec.note}）`);
  }
  const problem = check(spec, outDir);
  if (problem) { console.error(`✗ 生成出来的 ${spec.name} 不符合口径：${problem}`); bad += 1; }
}

if (bad > 0) {
  console.error(`\n✗ ${bad} 件不符合口径（口径见本脚本头部的 SPECS）`);
  process.exit(1);
}
console.log(`\n✓ 测试素材齐了：${outDir}`);
console.log('  引用它的：check-cli.mjs / check-local-backend.mjs / web-check.mjs / fixtures/*.doc.json');
