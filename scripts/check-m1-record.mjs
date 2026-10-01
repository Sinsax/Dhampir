#!/usr/bin/env node
// M1 记录守卫：独立复核 records/m1/ 是不是一份真记录。
//
// ---------------------------------------------------------------------------
// 这份守卫要回答的问题
//
// M1 的退出标准里，有三条是**关于记录本身**的：
//
//   ① 目标环境能跑出 PNG，且重复运行逐字节一致；
//   ② 四种环境（Win/DX12、Win/Vulkan、Linux/GPU、Linux/lavapipe）的 adapter
//      与通过情况全部记录；
//   ③ 1080p 单帧渲染 ≤ 10 ms（起始值，按实测定档）。
//
// 这三条都不是"cargo check 通过"能回答的，也不是"作者说跑过了"能回答的。能回答
// 它们的只有 records/m1/ 里那些字节。所以这份守卫**不读作者的结论，它自己把结论
// 重算一遍**：
//
//   · 每一张归档的 PNG：自己算 FNV-1a 64（文件字节），与 run.json 的 png_digest 比；
//   · 每一张归档的 PNG：**自己解码**（IHDR + IDAT 解压 + 反滤波）拿到 RGBA 像素，
//     再算一次 FNV-1a 64，与 run.json 的 pixel_digest 比；
//   · 用**重算出来的**像素摘要重新组装 frames_digest，与记录里的整表摘要比；
//   · 两条腿的逐帧摘要互相比（跨后端逐字节一致）；
//   · selfcheck-native.txt 的字节自己摘要一次，与 golden 摘要比。
//
// 最后那两步是关键。pixel_digest 是"渲染结果"的摘要，png_digest 是"文件"的摘要。
// 只比后者，等于只证明"文件没被改过"；两者都比、而且整表摘要能由重算结果复现，
// 才证明"这些像素就是那次渲染渲染出来的，并且换个后端也是同一些像素"。
//
// ---------------------------------------------------------------------------
// 为什么自己写 PNG 解码，不引依赖
//
// 引一个第三方 PNG 库来验 PNG，"验"和"生成"就共用同一套别人写的代码：库错在哪里，
// 记录就错在哪里，守卫看不出来——它只是把错误又抄了一遍。这里的解码只用 node:zlib
// （解压本身是 DEFLATE 标准，不是 PNG 知识），块结构、滤波、摘要全部自己实现。
// 代价约 60 行；收益是"PNG 里确实是那 65536 个像素"这句话有了独立证据。
//
// 自定义实现在守卫里可以接受，是因为守卫自带 `--self-test`：自检用一个自写的编码器
// 造出五种滤波器都覆盖到的**真 PNG**，解回来逐字节比。守卫要是错了，自检先红。
//
// ---------------------------------------------------------------------------
// 口径：判定用的那个数
//
// timing.json 里有两个计时数，用途不同：
//   · worst_frame_cpu_ms    —— 纯 CPU 侧编码 + submit，**不含 GPU**；
//   · worst_roundtrip_ms    —— 渲染 + 读回一整趟往返，含 GPU 画完那一段。
//
// 退出标准"单帧渲染 ≤ 10 ms"必须对着后者判：前者不含 GPU，没有能力否证这条预算。
// 本守卫不但检查 `budget_metric` 自述的是哪一个数，还**自己重算一遍 verdict**——
// 如果哪天有人把判据换回 cpu 那个数，verdict 就会与重算结果对不上，这里立刻红。
//
// ---------------------------------------------------------------------------
// 空集合、坏参数、坏路径
//
// 三条都拒绝通过，理由是老规矩：
//   · 目录里一张 PNG 都没有 → 多半是路径写错了，不是"记录很干净"；
//   · 不认识的参数 → 忽略它等于多出一条永远绿的路径；
//   · 自检失败 → 守卫自己的结论不可信，先修守卫。
//
// 用法：
//   node scripts/check-m1-record.mjs
//   node scripts/check-m1-record.mjs --record records/m1
//   node scripts/check-m1-record.mjs --self-test

import { existsSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { deflateSync, inflateSync } from 'node:zlib';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const DEFAULT_RECORD = join(REPO_ROOT, 'records', 'm1');

/** 一条腿一个子目录，名字就是后端 slug。顺序固定：先 DX12 后 Vulkan（与 plan 一致）。 */
export const LEGS = ['dx12', 'vulkan'];

/** slug → 后端标签。两套拼法的说明见 records/m1/README.md。 */
export const BACKEND_LABEL = { dx12: 'DX12', vulkan: 'VULKAN' };

/**
 * 这份记录被钉住的形状。全都是**记录里的口径**，不是"恰好这一次的数"：
 * 换帧区间、换目标尺寸、换容差都应该先改 plan 与 README，再来改这里。
 */
export const EXPECTED = {
  milestone: 'M1',
  scenes: ['gradient', 'checker', 'srgb_linear', 'alpha_stack', 'blur'],
  frameRange: '0..16',
  framesPerScene: 16,
  points: 368,
  targetSize: '256x256',
  targetFormat: 'Rgba8UnormSrgb',
  byteTolerance: 1,
  timingSize: '1920x1080',
  alignSize: '1366x768',
  alignScene: 'checker',
  alignFrame: 0,
  frameBudgetMs: 10,
  probeDigest: 'c3f0da6b37577e55',
  probeLines: 72,
};

/** 每条腿必须有的文件：少一个就说明这条腿没跑完。 */
const REQUIRED_ROOT_FILES = ['adapter.json', 'run.json', 'readings.txt', 'timing.json', 'compare.json'];

/**
 * 退出标准 ② 要的是**四种环境**：Win/DX12、Win/Vulkan、Linux/GPU、Linux/lavapipe。
 *
 * 两条 Windows 腿由 `main()` 硬性要求（缺一条就直接退出码 2，`loadLeg` 还会逐份读文件），
 * 所以剩下唯一可能悄悄缺口的就是 Linux 这两条——`checkHonesty` 盯的就是它。
 */
const LINUX_LEGS = 2;

/**
 * 记录根下每个子目录，按"**完整**的腿 / 空壳"分类。
 *
 * 诚实性检查（checkHonesty）只认完整的腿：独立复核用 `mkdir records/m1/linux`
 * （一个空目录）把"缺 Linux 两条腿"的闸门关掉过一次，所以这里先把"什么才算腿"
 * 查清楚再交出去——必备文件与 `frames/` 里的 PNG 缺一不可。
 */
function inspectLegDirs(recordDir, subdirNames) {
  const complete = [];
  const incomplete = [];
  for (const name of subdirNames) {
    const dir = join(recordDir, name);
    const missing = REQUIRED_ROOT_FILES.filter((file) => !existsSync(join(dir, file)));
    const framesDir = join(dir, 'frames');
    if (!existsSync(framesDir)) missing.push('frames/');
    else if (!readdirSync(framesDir).some((file) => file.endsWith('.png'))) missing.push('frames/ 里的 PNG');
    if (missing.length === 0) complete.push(name);
    else incomplete.push({ name, missing });
  }
  return { complete, incomplete };
}

/**
 * 一条腿的检查项清单。`checkLegModel` 保证**每一项都有一条结论**——
 * 检查被删掉时会出现"项少了"，而不是"项还在但永远绿"。
 */
export const LEG_CHECKS = [
  'required-files',
  'frame-set',
  'png-bytes-and-digest',
  'pixels',
  'repeat',
  'frames-digest',
  'counts',
  'points',
  'readings',
  'timing',
  'alignment',
  'compare',
  'adapter',
  'run-shape',
];

// ---------------------------------------------------------------------------
// 摘要：FNV-1a 64
//
// 与 dhampir-timeline 的 `fnv1a64` 同参数（偏移基数 0xcbf29ce484222325、
// 质数 0x100000001b3、8 位一乘）。**故意重写一遍**而不是引 Rust 那份：
// 守卫与生产代码共用同一个实现时，"实现对了吗"就没有第二个人回答。
// ---------------------------------------------------------------------------

const FNV_OFFSET_BASIS = 0xcbf29ce484222325n;
const FNV_PRIME = 0x100000001b3n;
const MASK64 = (1n << 64n) - 1n;

/** FNV-1a 64，返回 16 位小写十六进制（与记录里的写法一致）。 */
export function fnv1a64(bytes) {
  let hash = FNV_OFFSET_BASIS;
  for (let i = 0; i < bytes.length; i += 1) {
    hash ^= BigInt(bytes[i]);
    hash = (hash * FNV_PRIME) & MASK64;
  }
  return hash.toString(16).padStart(16, '0');
}

// ---------------------------------------------------------------------------
// PNG：只认这一种形态（8 位 RGBA、无隔行），也正是 core 的编码器写出来的形态
// ---------------------------------------------------------------------------

const PNG_SIGNATURE = [137, 80, 78, 71, 13, 10, 26, 10];

/** 解一张 PNG，返回 `{ width, height, pixels }`（pixels 是紧凑的 RGBA8）。 */
export function decodePng(bytes) {
  for (let i = 0; i < PNG_SIGNATURE.length; i += 1) {
    if (bytes[i] !== PNG_SIGNATURE[i]) throw new Error('不是 PNG 签名');
  }

  let ihdr = null;
  const idat = [];
  let offset = 8;
  while (offset + 8 <= bytes.length) {
    const length = bytes.readUInt32BE(offset);
    const type = bytes.toString('latin1', offset + 4, offset + 8);
    const data = bytes.subarray(offset + 8, offset + 8 + length);
    if (type === 'IHDR') {
      ihdr = {
        width: data.readUInt32BE(0),
        height: data.readUInt32BE(4),
        bitDepth: data[8],
        colorType: data[9],
        compression: data[10],
        filter: data[11],
        interlace: data[12],
      };
    } else if (type === 'IDAT') {
      idat.push(Buffer.from(data));
    } else if (type === 'IEND') {
      break;
    }
    offset += 12 + length;
  }

  if (!ihdr) throw new Error('没有 IHDR');
  const shape = `bitDepth=${ihdr.bitDepth} colorType=${ihdr.colorType} interlace=${ihdr.interlace}`;
  if (ihdr.bitDepth !== 8 || ihdr.colorType !== 6 || ihdr.interlace !== 0) {
    throw new Error(`只认 8 位 RGBA 非隔行，这张是 ${shape}`);
  }
  if (idat.length === 0) throw new Error('没有 IDAT');

  const raw = inflateSync(Buffer.concat(idat));
  const bpp = 4;
  const stride = ihdr.width * bpp;
  const pixels = Buffer.alloc(ihdr.height * stride);

  let pos = 0;
  for (let y = 0; y < ihdr.height; y += 1) {
    if (pos >= raw.length) throw new Error(`第 ${y} 行的滤波器字节就没了`);
    const filter = raw[pos];
    pos += 1;
    const src = raw.subarray(pos, pos + stride);
    pos += stride;
    if (src.length !== stride) throw new Error(`第 ${y} 行只剩 ${src.length}/${stride} 字节`);
    const cur = pixels.subarray(y * stride, (y + 1) * stride);
    const prev = y === 0 ? null : pixels.subarray((y - 1) * stride, y * stride);
    for (let x = 0; x < stride; x += 1) {
      const left = x >= bpp ? cur[x - bpp] : 0;
      const up = prev ? prev[x] : 0;
      const upLeft = prev && x >= bpp ? prev[x - bpp] : 0;
      cur[x] = (src[x] + unfilter(filter, left, up, upLeft)) & 0xff;
    }
  }
  if (pos !== raw.length) throw new Error(`IDAT 长度对不上：用了 ${pos}，实际 ${raw.length}`);

  return { width: ihdr.width, height: ihdr.height, pixels };
}

/** PNG 的五个滤波器。全都要实现：编码器换一个默认滤波器，守卫不能跟着瞎。 */
function unfilter(filter, left, up, upLeft) {
  switch (filter) {
    case 0:
      return 0;
    case 1:
      return left;
    case 2:
      return up;
    case 3:
      return (left + up) >> 1;
    case 4: {
      const p = left + up - upLeft;
      const pa = Math.abs(p - left);
      const pb = Math.abs(p - up);
      const pc = Math.abs(p - upLeft);
      const predictor = pa <= pb && pa <= pc ? left : pb <= pc ? up : upLeft;
      return predictor;
    }
    default:
      throw new Error(`不认识的滤波器类型 ${filter}`);
  }
}

// ---------------------------------------------------------------------------
// 一份"腿的记录"模型
//
// 判定逻辑全部写成 `checkLegModel(leg)` 这样的**纯函数**：真跑时喂给它从磁盘读出来
// 的模型，自检时喂给它造出来的模型。两条路走同一段判定代码，自检才有意义。
// ---------------------------------------------------------------------------

function readJsonIfPresent(path) {
  try {
    return JSON.parse(readFileSync(path, 'utf8'));
  } catch (error) {
    return { __error: error.message };
  }
}

/** 从磁盘把一条腿读成一个模型（含每张 PNG 的字节数、文件摘要、解码后的像素摘要）。 */
export function loadLeg(recordDir, slug) {
  const dir = join(recordDir, slug);
  const rootFiles = new Set();
  let framesDirPresent = false;
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    if (entry.isFile()) rootFiles.add(entry.name);
    else if (entry.isDirectory() && entry.name === 'frames') framesDirPresent = true;
  }

  const frameNames = framesDirPresent ? readdirSync(join(dir, 'frames')).sort() : [];
  const pngFiles = new Map();
  for (const name of frameNames) {
    const bytes = readFileSync(join(dir, 'frames', name));
    const info = { size: bytes.length, fileDigest: fnv1a64(bytes) };
    try {
      const image = decodePng(bytes);
      info.pixelDigest = fnv1a64(image.pixels);
      info.width = image.width;
      info.height = image.height;
    } catch (error) {
      info.decodeError = error.message;
    }
    pngFiles.set(`frames/${name}`, info);
  }

  let readings = '';
  try {
    readings = readFileSync(join(dir, 'readings.txt'), 'utf8');
  } catch (error) {
    readings = { __error: error.message };
  }

  return {
    slug,
    rootFiles,
    framesDirPresent,
    frameNames,
    pngFiles,
    readings,
    adapter: readJsonIfPresent(join(dir, 'adapter.json')),
    run: readJsonIfPresent(join(dir, 'run.json')),
    timing: readJsonIfPresent(join(dir, 'timing.json')),
    compare: readJsonIfPresent(join(dir, 'compare.json')),
  };
}

/** 把模型里几份读不动的东西换成空对象，让各检查自己去抱怨。 */
function parts(leg) {
  const ok = (value) => (value && !value.__error ? value : {});
  const run = ok(leg.run);
  const backends = Array.isArray(run.backends) ? run.backends : [];
  return {
    run,
    backend: backends[0] ?? {},
    frames: Array.isArray(backends[0]?.frames) ? backends[0].frames : [],
    scenes: Array.isArray(run.scenes) ? run.scenes : [],
    adapter: ok(leg.adapter),
    timing: ok(leg.timing),
    compare: ok(leg.compare),
  };
}

function parseSize(text) {
  const match = /^(\d+)x(\d+)$/.exec(String(text ?? ''));
  return match ? [Number(match[1]), Number(match[2])] : null;
}

function parseRange(text) {
  const match = /^(\d+)\.\.(\d+)$/.exec(String(text ?? ''));
  return match ? [Number(match[1]), Number(match[2])] : null;
}

/** 记录里用的帧文件名：三位补零，字典序 == 帧号序。 */
function frameFileName(scene, frame) {
  return `frames/${scene}-f${String(frame).padStart(3, '0')}.png`;
}

/**
 * 整表摘要：按帧顺序把 `场景名 + 0x00 + 帧号(LE u32) + 像素摘要(LE u64)` 串起来再摘要一次。
 *
 * 与 `dhampir-worker` 的 `scenes::frames_digest` 同一套规则，同样是**重写一遍**。
 * 顺序敏感是有意的：同样的帧换个顺序也该被看出来。
 */
export function framesDigest(entries) {
  const chunks = [];
  for (const entry of entries) {
    const number = Buffer.alloc(4);
    number.writeUInt32LE(entry.frame >>> 0);
    const digest = Buffer.alloc(8);
    digest.writeBigUInt64LE(BigInt(`0x${entry.pixelDigest}`));
    chunks.push(Buffer.from(entry.scene, 'utf8'), Buffer.from([0]), number, digest);
  }
  return fnv1a64(Buffer.concat(chunks));
}

// ---------------------------------------------------------------------------
// 判定：一条腿
// ---------------------------------------------------------------------------

const CHECKS = {
  'required-files': (leg) => {
    const messages = [];
    for (const name of REQUIRED_ROOT_FILES) if (!leg.rootFiles.has(name)) messages.push(`缺 ${name}`);
    if (!leg.framesDirPresent) messages.push('缺 frames/ 目录');
    for (const [name, value] of [
      ['adapter.json', leg.adapter],
      ['run.json', leg.run],
      ['timing.json', leg.timing],
      ['compare.json', leg.compare],
    ]) {
      if (value && value.__error) messages.push(`${name} 读不动：${value.__error}`);
    }
    if (typeof leg.readings !== 'string' || leg.readings.length === 0) {
      messages.push(`readings.txt 读不动或为空：${leg.readings?.__error ?? '空文件'}`);
    }
    return messages;
  },

  'frame-set': (leg) => {
    const { run, frames, scenes } = parts(leg);
    const messages = [];
    const range = parseRange(run.frame_range);
    if (!range) return [`run.json 的 frame_range 读不动：${JSON.stringify(run.frame_range)}`];
    const [start, end] = range;

    const expected = [];
    for (const scene of scenes) {
      for (let frame = start; frame < end; frame += 1) expected.push(frameFileName(scene.name, frame));
    }

    // frames[] 的声明顺序必须就是"按场景分组、帧号递增"——一格错位都要报。
    frames.forEach((frame, index) => {
      if (frame.png !== expected[index]) {
        messages.push(`frames[${index}].png=${frame.png}，按 scenes×frame_range 应当是 ${expected[index]}`);
      }
    });
    if (frames.length !== expected.length) {
      messages.push(`run.json 声明 ${frames.length} 帧，按 scenes×frame_range 应当是 ${expected.length} 帧`);
    }

    const actual = new Set(leg.frameNames.map((name) => `frames/${name}`));
    const declared = new Set(expected);
    const missing = [...declared].filter((name) => !actual.has(name));
    const extra = [...actual].filter((name) => !declared.has(name));
    if (missing.length > 0) messages.push(`磁盘上少 ${missing.length} 张：${missing.slice(0, 4).join('、')}`);
    if (extra.length > 0) messages.push(`磁盘上多 ${extra.length} 张：${extra.slice(0, 4).join('、')}`);

    if (frames.length > 0) {
      const counted = new Map();
      for (const frame of frames) counted.set(frame.scene, (counted.get(frame.scene) ?? 0) + 1);
      for (const scene of scenes) {
        const n = counted.get(scene.name) ?? 0;
        if (n !== end - start) messages.push(`场景 ${scene.name} 有 ${n} 帧，应当是 ${end - start} 帧`);
      }
    }
    return messages;
  },

  'png-bytes-and-digest': (leg) => {
    const { frames } = parts(leg);
    const messages = [];
    for (const frame of frames) {
      const info = leg.pngFiles.get(frame.png);
      if (!info) {
        messages.push(`${frame.png} 不在磁盘上`);
        continue;
      }
      if (info.size !== frame.png_bytes) {
        messages.push(`${frame.png}：声明 ${frame.png_bytes} 字节，实际 ${info.size} 字节`);
      }
      if (info.fileDigest !== frame.png_digest) {
        messages.push(`${frame.png}：声明 png_digest ${frame.png_digest}，重算 ${info.fileDigest}`);
      }
    }
    return messages;
  },

  pixels: (leg) => {
    const { run, frames } = parts(leg);
    const messages = [];
    const size = parseSize(run.target_size);
    for (const frame of frames) {
      const info = leg.pngFiles.get(frame.png);
      if (!info) continue;
      if (info.decodeError) {
        messages.push(`${frame.png} 解不开：${info.decodeError}`);
        continue;
      }
      if (size && (info.width !== size[0] || info.height !== size[1])) {
        messages.push(`${frame.png}：解码得到 ${info.width}×${info.height}，声明 ${run.target_size}`);
      }
      if (info.pixelDigest !== frame.pixel_digest) {
        messages.push(
          `${frame.png}：声明 pixel_digest ${frame.pixel_digest}，自己解码重算 ${info.pixelDigest}`,
        );
      }
    }
    return messages;
  },

  repeat: (leg) => {
    const { backend, frames } = parts(leg);
    const messages = [];
    const notIdentical = frames.filter((frame) => frame.repeat_identical !== true);
    if (notIdentical.length > 0) {
      messages.push(
        `${notIdentical.length} 帧的同帧两次渲染标记为不一致：${notIdentical
          .slice(0, 4)
          .map((frame) => frame.png)
          .join('、')}`,
      );
    }
    const drifted = frames.filter((frame) => frame.repeat_pixel_digest !== frame.pixel_digest);
    if (drifted.length > 0) {
      messages.push(
        `${drifted.length} 帧的 repeat_pixel_digest ≠ pixel_digest：${drifted
          .slice(0, 4)
          .map((frame) => frame.png)
          .join('、')}`,
      );
    }
    if (Array.isArray(backend.repeat_mismatches) && backend.repeat_mismatches.length > 0) {
      messages.push(`repeat_mismatches 非空：${backend.repeat_mismatches.join('、')}`);
    }
    return messages;
  },

  'frames-digest': (leg) => {
    const { backend, frames } = parts(leg);
    const entries = [];
    for (const frame of frames) {
      const info = leg.pngFiles.get(frame.png);
      if (!info?.pixelDigest) return [`有帧的像素摘要没能重算出来（${frame.png}），整表摘要因此无法复现`];
      // 用**自己重算的**像素摘要，不用 run.json 声明的那个。
      entries.push({ scene: frame.scene, frame: frame.frame, pixelDigest: info.pixelDigest });
    }
    if (entries.length === 0) return ['一帧都没有，整表摘要无从谈起'];
    const recomputed = framesDigest(entries);
    if (recomputed !== backend.frames_digest) {
      return [
        `整表 frames_digest 声明 ${backend.frames_digest}，用重算出的像素摘要重组得到 ${recomputed}`,
      ];
    }
    return [];
  },

  counts: (leg) => {
    const { backend, frames } = parts(leg);
    const messages = [];
    const counts = backend.counts ?? {};
    const points = frames.reduce((n, frame) => n + (Array.isArray(frame.points) ? frame.points.length : 0), 0);
    if (counts.frames !== frames.length) messages.push(`counts.frames=${counts.frames}，实际 ${frames.length} 帧`);
    if (counts.points !== points) messages.push(`counts.points=${counts.points}，逐帧加起来 ${points}`);
    for (const key of ['failed', 'out_of_range', 'unjudged']) {
      if (counts[key] !== 0) messages.push(`counts.${key}=${counts[key]}（必须为 0）`);
    }
    if (counts.clean !== true) messages.push('counts.clean 不是 true');
    if (points !== EXPECTED.points) messages.push(`采样点总数 ${points}，期望 ${EXPECTED.points}`);
    return messages;
  },

  points: (leg) => {
    const { run, frames, scenes } = parts(leg);
    const messages = [];
    const byName = new Map(scenes.map((scene) => [scene.name, scene]));
    for (const frame of frames) {
      const scene = byName.get(frame.scene);
      if (!scene) {
        messages.push(`帧 ${frame.png} 的场景 ${frame.scene} 不在 scenes[] 里`);
        continue;
      }
      const samples = Array.isArray(scene.samples) ? scene.samples : [];
      const points = Array.isArray(frame.points) ? frame.points : [];
      if (points.length !== samples.length) {
        messages.push(`${frame.png}：${points.length} 个采样点，场景声明 ${samples.length} 个`);
      }
      for (let i = 0; i < Math.min(points.length, samples.length); i += 1) {
        const point = points[i];
        const sample = samples[i];
        const where = `${frame.png}[${i}] ${point.label}`;
        if (
          point.x !== sample.x ||
          point.y !== sample.y ||
          point.label !== sample.label ||
          point.purpose !== sample.purpose
        ) {
          messages.push(
            `${where} 与 scenes[] 对不上：(${point.x},${point.y}) vs (${sample.x},${sample.y}) ${sample.label}`,
          );
        }
        if (point.tolerance !== run.byte_tolerance) {
          messages.push(`${where} 的 tolerance=${point.tolerance}，run.json 声明 ${run.byte_tolerance}`);
        }
        if (point.passed !== true) {
          messages.push(
            `${where} 未通过（distance ${point.distance} / 容差 ${point.tolerance}${
              point.passed === null ? '，未判定' : ''
            }）`,
          );
        }
        const measured = Array.isArray(point.measured) ? point.measured : null;
        const expected = Array.isArray(point.expected) ? point.expected : null;
        if (!measured || measured.length !== 4) {
          messages.push(`${where} 的 measured 不是 4 个字节`);
        } else if (expected) {
          const distance = Math.max(...measured.map((v, k) => Math.abs(v - expected[k])));
          if (distance !== point.distance) {
            messages.push(`${where} 的 distance=${point.distance}，按 measured/expected 重算是 ${distance}`);
          }
          if (distance > point.tolerance) {
            messages.push(`${where} 的距离 ${distance} 超过容差 ${point.tolerance}`);
          }
        }
      }
    }

    // 期望值这一列分两个层次看，别混成一个。
    //
    // `scenes[].samples[].expected` 是**第 0 帧**的那一列（worker 的 `scene_json`
    // 用 `expected_bytes(spec, 0, ..)` 写它）。所以"逐帧都等于这张表"是错的判据：
    // 四个 `uses_frame = true` 的场景，别的帧与表不同才是对的。能证伪的规则有三条：
    //
    //   ① 第 0 帧必须锚在表上——表跟记录里的第 0 帧对不上，就有一边在撒谎；
    //   ② `uses_frame = false` 的场景，每一帧都必须等于表（`blur` 只此一家）；
    //   ③ `uses_frame = true` 的场景，至少要有一个采样点在某帧上偏离表，
    //      否则这个声明只是装饰。
    //
    // 第 0 帧以外的帧，守卫**不重算着色器模型**（模型在 core 里，有它自己的测试）；
    // 这里只钉"记录内部自洽"：expected / measured / distance / passed 四者互相说得通。
    const byScene = new Map();
    for (const frame of frames) {
      if (!byName.has(frame.scene)) continue;
      if (!byScene.has(frame.scene)) byScene.set(frame.scene, []);
      byScene.get(frame.scene).push(frame);
    }
    for (const [name, sceneFrames] of byScene) {
      const scene = byName.get(name);
      const samples = Array.isArray(scene.samples) ? scene.samples : [];
      const ordered = [...sceneFrames].sort((a, b) => a.frame - b.frame);
      const anchor = ordered.find((frame) => frame.frame === 0);
      if (!anchor) {
        messages.push(`场景 ${name} 没有第 0 帧——scenes[] 的期望值表锚在第 0 帧上，缺了它没法核`);
        continue;
      }
      if (typeof scene.uses_frame !== 'boolean') {
        messages.push(`场景 ${name} 的 uses_frame 不是布尔值（${JSON.stringify(scene.uses_frame)}）`);
      }
      let varied = false;
      for (const frame of ordered) {
        const points = Array.isArray(frame.points) ? frame.points : [];
        for (let i = 0; i < Math.min(samples.length, points.length); i += 1) {
          const point = points[i];
          const same = JSON.stringify(point.expected) === JSON.stringify(samples[i].expected);
          if (frame.frame === 0) {
            if (!same) {
              messages.push(`${frame.png}[${i}] ${point.label} 的 expected 与 scenes[] 的表对不上——第 0 帧必须锚在表上`);
            }
          } else if (!same) {
            varied = true;
          }
        }
      }
      if (scene.uses_frame === false && varied) {
        messages.push(`场景 ${name} 声明不随帧变化（uses_frame=false），却有帧的期望值与第 0 帧不同`);
      }
      if (scene.uses_frame === true && !varied) {
        messages.push(
          `场景 ${name} 声明 uses_frame=true，但 ${ordered.length} 帧的期望值与第 0 帧一模一样——这个声明没有被证据支持`,
        );
      }
    }
    if (frames.length === 0) messages.push('一帧都没有');
    return messages;
  },

  readings: (leg) => {
    const { run, frames } = parts(leg);
    if (typeof leg.readings !== 'string' || leg.readings.length === 0) {
      return ['readings.txt 是空的——给人读的那一份也是记录的一部分'];
    }
    const messages = [];
    const lines = leg.readings.split('\n');
    if (lines[0] !== 'dhampir M1 corpus 逐点读数') {
      messages.push(`第一行不是标题：${JSON.stringify(lines[0])}`);
    }
    const headers = lines.filter((line) => line.startsWith('--- ')).length;
    const body = lines.filter((line) => /^[a-z_]+ +f\d/.test(line)).length;
    const points = frames.reduce((n, frame) => n + (Array.isArray(frame.points) ? frame.points.length : 0), 0);
    if (headers !== frames.length) {
      messages.push(`readings.txt 有 ${headers} 个帧标题，run.json 声明 ${frames.length} 帧`);
    }
    if (body !== points) messages.push(`readings.txt 有 ${body} 行读数，逐帧应收 ${points} 行`);
    const head = lines.slice(0, 3).join('\n');
    if (!head.includes(`帧 ${frames.length}`) || !head.includes(`采样点 ${points}`)) {
      messages.push('readings.txt 开头的合计行与逐帧对不上');
    }
    if (!head.includes(`容差 ${run.byte_tolerance} 字节`)) {
      messages.push(`readings.txt 的合计行没写「容差 ${run.byte_tolerance} 字节」`);
    }
    return messages;
  },

  timing: (leg) => {
    const { adapter, timing } = parts(leg);
    const messages = [];
    const t = timing.timing ?? {};
    const scenes = Array.isArray(t.scenes) ? t.scenes : [];

    if (timing.kind !== 'timing') messages.push(`timing.json 的 kind=${JSON.stringify(timing.kind)}`);
    if (timing.schema !== 1) messages.push(`timing.json 的 schema=${JSON.stringify(timing.schema)}`);
    if (timing.milestone !== EXPECTED.milestone) {
      messages.push(`timing.json 的 milestone=${JSON.stringify(timing.milestone)}`);
    }
    if (timing.timing_target_size !== EXPECTED.timingSize) {
      messages.push(`timing_target_size=${JSON.stringify(timing.timing_target_size)}，期望 ${EXPECTED.timingSize}`);
    }
    if (t.size !== EXPECTED.timingSize) messages.push(`计时用的尺寸是 ${JSON.stringify(t.size)}`);
    if (!(typeof timing.init_ms === 'number' && timing.init_ms > 0)) {
      messages.push(`init_ms=${JSON.stringify(timing.init_ms)} 不像量出来的数`);
    }
    if (timing.build_profile !== adapter.build_profile) {
      messages.push('timing.json 与 adapter.json 的 build_profile 不是同一个');
    }
    // 两份记录必须同属一次运行：时间戳是唯一能证明这一点的字段。
    if (timing.unix_epoch_millis !== adapter.unix_epoch_millis) {
      messages.push(
        `timing.json 与 adapter.json 的时间戳不同（${timing.unix_epoch_millis} vs ${adapter.unix_epoch_millis}）——它们就不是同一次运行的两份记录`,
      );
    }

    // ---- 判据本身：对着哪个数判，判成什么 ----
    if (t.budget_metric !== 'worst_roundtrip_ms') {
      messages.push(
        `budget_metric=${JSON.stringify(t.budget_metric)}：这条预算必须对着「渲染 + 读回」的往返判`,
      );
    }
    if (typeof t.budget_metric_note !== 'string' || t.budget_metric_note.length === 0) {
      messages.push('没有 budget_metric_note——读的人只能猜那 10 ms 是对着哪个数比的');
    }
    if (t.budget_ms !== EXPECTED.frameBudgetMs) {
      messages.push(`budget_ms=${JSON.stringify(t.budget_ms)}，期望 ${EXPECTED.frameBudgetMs}`);
    }
    if (typeof t.worst_roundtrip_ms !== 'number') {
      messages.push(`worst_roundtrip_ms=${JSON.stringify(t.worst_roundtrip_ms)} 不是个数`);
    }
    if (typeof t.worst_frame_cpu_ms !== 'number') {
      messages.push(`worst_frame_cpu_ms=${JSON.stringify(t.worst_frame_cpu_ms)} 不是个数`);
    }
    if (typeof t.verdict !== 'boolean') {
      messages.push(`verdict=${JSON.stringify(t.verdict)}：本记录应当是 release 构建跑出来的（非 release 只会说"不下结论"）`);
    } else if (typeof t.worst_roundtrip_ms === 'number' && typeof t.budget_ms === 'number') {
      const recomputed = t.worst_roundtrip_ms <= t.budget_ms;
      if (t.verdict !== recomputed) {
        messages.push(
          `verdict=${t.verdict}，而按 worst_roundtrip_ms ${t.worst_roundtrip_ms} vs budget ${t.budget_ms} 重算应当是 ${recomputed}`,
        );
      }
    }

    // ---- 两个"最慢"必须真的是各自那列的最大值 ----
    const medians = (key) => scenes.map((scene) => scene?.[key]?.median_ms);
    const asNumbers = (list) => (list.every((value) => typeof value === 'number') ? list : null);
    const cpu = asNumbers(medians('frame_cpu_ms'));
    const roundtrip = asNumbers(medians('readback_ms'));
    if (!cpu) messages.push('有场景的 frame_cpu_ms.median_ms 不是数');
    else if (t.worst_frame_cpu_ms !== Math.max(...cpu)) {
      messages.push(`worst_frame_cpu_ms=${t.worst_frame_cpu_ms}，各场景 CPU 提交中位数最大的是 ${Math.max(...cpu)}`);
    }
    if (!roundtrip) messages.push('有场景的 readback_ms.median_ms 不是数');
    else if (t.worst_roundtrip_ms !== Math.max(...roundtrip)) {
      messages.push(
        `worst_roundtrip_ms=${t.worst_roundtrip_ms}，各场景往返中位数最大的是 ${Math.max(...roundtrip)}`,
      );
    }

    // ---- 计时的口径 ----
    if (!(typeof t.warmup === 'number' && t.warmup > 0)) messages.push(`warmup=${JSON.stringify(t.warmup)}`);
    if (!(typeof t.repeats === 'number' && t.repeats > 0)) messages.push(`repeats=${JSON.stringify(t.repeats)}`);
    if (typeof t.frame !== 'number') messages.push(`frame=${JSON.stringify(t.frame)} 不是帧号`);
    const names = scenes.map((scene) => scene?.scene).join(',');
    if (names !== EXPECTED.scenes.join(',')) {
      messages.push(`计时覆盖的场景是 ${JSON.stringify(names)}，期望 ${JSON.stringify(EXPECTED.scenes.join(','))}`);
    }
    for (const scene of scenes) {
      for (const key of ['frame_cpu_ms', 'readback_ms']) {
        const bucket = scene?.[key];
        if (!bucket || bucket.n !== t.repeats) {
          messages.push(`${scene?.scene} 的 ${key}.n=${JSON.stringify(bucket?.n)}，期望 ${t.repeats}`);
        }
      }
    }
    return messages;
  },

  alignment: (leg) => {
    const { timing } = parts(leg);
    const messages = [];
    const align = timing.row_alignment ?? {};
    const probe = timing.align_probe ?? {};
    const size = parseSize(EXPECTED.alignSize);
    const unpadded = size[0] * 4;
    const padded = Math.ceil(unpadded / 256) * 256;

    if (align.ok !== true) messages.push(`row_alignment.ok=${JSON.stringify(align.ok)}`);
    if (align.size !== EXPECTED.alignSize) messages.push(`row_alignment.size=${JSON.stringify(align.size)}`);
    if (align.worst_distance !== 0) messages.push(`最差距离 ${JSON.stringify(align.worst_distance)}，期望 0`);
    if (align.pixels_compared !== size[0] * size[1]) {
      messages.push(`比了 ${JSON.stringify(align.pixels_compared)} 个像素，期望 ${size[0] * size[1]}`);
    }
    if (align.pixels_ok !== align.pixels_compared) {
      messages.push(`通过的像素 ${align.pixels_ok} ≠ 比过的 ${align.pixels_compared}`);
    }
    if (align.unpadded_bytes_per_row !== unpadded) {
      messages.push(`unpadded_bytes_per_row=${align.unpadded_bytes_per_row}，按尺寸算应当是 ${unpadded}`);
    }
    if (align.padded_bytes_per_row !== padded) {
      messages.push(`padded_bytes_per_row=${align.padded_bytes_per_row}，按 256 对齐应当是 ${padded}`);
    }
    if (align.byte_tolerance !== EXPECTED.byteTolerance) {
      messages.push(`row_alignment.byte_tolerance=${align.byte_tolerance}`);
    }
    // 这一条是"不许在测不出问题的尺寸上宣布通过"：选一个本来就不需要填充的宽度，
    // 对齐代码就算写错了也照样绿。探针自己会拒绝报通过，守卫必须看它真的拒绝了没有。
    if (align.exercises_padding !== true) {
      messages.push('exercises_padding 不是 true——这个尺寸本来就不需要填充，通过了也证明不了什么');
    }
    if (probe.scene !== EXPECTED.alignScene) messages.push(`align_probe.scene=${JSON.stringify(probe.scene)}`);
    if (probe.frame !== EXPECTED.alignFrame) messages.push(`align_probe.frame=${JSON.stringify(probe.frame)}`);
    if (probe.size !== EXPECTED.alignSize) messages.push(`align_probe.size=${JSON.stringify(probe.size)}`);
    if (probe.unpadded_bytes_per_row !== unpadded || probe.padded_bytes_per_row !== padded) {
      messages.push('align_probe 的行字节数与 row_alignment 对不上');
    }
    return messages;
  },

  compare: (leg) => {
    const { backend, frames, compare } = parts(leg);
    const messages = [];
    const list = Array.isArray(compare.backends) ? compare.backends : [];
    if (compare.other_run_error) {
      messages.push(`没读到被比的那份记录：${compare.other_run_error}`);
    }
    if (compare.identical !== true) messages.push(`compare.identical=${JSON.stringify(compare.identical)}`);
    if (!(typeof compare.backends_compared === 'number' && compare.backends_compared >= 1)) {
      messages.push(`backends_compared=${JSON.stringify(compare.backends_compared)}——一个后端都没比，等于没比`);
    }
    if (compare.backends_not_compared !== 0) {
      messages.push(`backends_not_compared=${JSON.stringify(compare.backends_not_compared)}`);
    }
    if (typeof compare.note !== 'string' || compare.note.length === 0) {
      messages.push('compare.json 没有 note，读的人不知道它是谁跟谁比的');
    }
    if (list.length === 0) messages.push('backends[] 是空的');

    for (const entry of list) {
      const name = entry?.requested ?? '（没写 requested）';
      if (entry.identical !== true) messages.push(`${name}：identical=${JSON.stringify(entry.identical)}`);
      if (entry.frame_count !== frames.length) {
        messages.push(`${name}：frame_count=${entry.frame_count}，本次 ${frames.length} 帧`);
      }
      if (entry.other_frame_count !== frames.length) {
        messages.push(`${name}：other_frame_count=${entry.other_frame_count}，本次 ${frames.length} 帧`);
      }
      if (entry.matched_frames !== frames.length) {
        messages.push(`${name}：比上了 ${entry.matched_frames}/${frames.length} 帧`);
      }
      for (const key of ['pixel_mismatches', 'png_mismatches', 'missing_in_other', 'missing_in_current']) {
        const values = Array.isArray(entry[key]) ? entry[key] : null;
        if (!values) messages.push(`${name}：${key} 不是数组`);
        else if (values.length > 0) messages.push(`${name}：${key} 非空（${values.slice(0, 4).join('、')}）`);
      }
      // 这一格是"跨进程"四个字的落点：另外一个进程算出来的整表摘要，与这份记录
      // 声明的整表摘要必须相等——而且它不能是 null（null 就是"对方没给"）。
      if (typeof entry.other_frames_digest !== 'string') {
        messages.push(`${name}：other_frames_digest=${JSON.stringify(entry.other_frames_digest)}，等于没比`);
      } else if (entry.other_frames_digest !== backend.frames_digest) {
        messages.push(
          `${name}：对方的整表摘要 ${entry.other_frames_digest} ≠ 本份的 ${backend.frames_digest}`,
        );
      }
      if (entry.frames_digest !== backend.frames_digest) {
        messages.push(`${name}：compare 里的 frames_digest 与 run.json 的不是同一个`);
      }
    }
    return messages;
  },

  adapter: (leg) => {
    const { adapter } = parts(leg);
    const messages = [];
    const info = adapter.adapter ?? {};
    if (adapter.kind !== 'adapter') messages.push(`adapter.json 的 kind=${JSON.stringify(adapter.kind)}`);
    if (adapter.schema !== 1) messages.push(`adapter.json 的 schema=${JSON.stringify(adapter.schema)}`);
    if (adapter.milestone !== EXPECTED.milestone) {
      messages.push(`adapter.json 的 milestone=${JSON.stringify(adapter.milestone)}`);
    }
    // 纯逻辑探针的摘要是 M0 就钉住的（同样一份报告在 wasm 侧也跑过）。它要是变了，
    // 要么是探针改了（要改 plan），要么是这份记录来自别的代码。
    if (adapter.probe_digest !== EXPECTED.probeDigest) {
      messages.push(
        `probe_digest=${JSON.stringify(adapter.probe_digest)}，golden 是 ${EXPECTED.probeDigest}`,
      );
    }
    if (adapter.target_format !== EXPECTED.targetFormat) {
      messages.push(`target_format=${JSON.stringify(adapter.target_format)}`);
    }
    if (adapter.corpus_target_size !== EXPECTED.targetSize) {
      messages.push(`corpus_target_size=${JSON.stringify(adapter.corpus_target_size)}`);
    }
    for (const key of ['name', 'backend', 'driver']) {
      if (typeof info[key] !== 'string' || info[key].length === 0) {
        messages.push(`adapter.${key}=${JSON.stringify(info[key])}——记录里必须能看出跑在什么上`);
      }
    }
    for (const key of ['wgpu_version', 'naga_version', 'crate_version', 'requested_backends']) {
      if (typeof adapter[key] !== 'string' || adapter[key].length === 0) {
        messages.push(`adapter.json 的 ${key} 是空的`);
      }
    }
    if (adapter.wgpu_version !== adapter.naga_version) {
      messages.push(`wgpu ${adapter.wgpu_version} 与 naga ${adapter.naga_version} 版本不一致`);
    }
    if (!(typeof adapter.unix_epoch_millis === 'number' && adapter.unix_epoch_millis > 0)) {
      messages.push(`unix_epoch_millis=${JSON.stringify(adapter.unix_epoch_millis)}`);
    } else if (adapter.unix_epoch_seconds !== Math.floor(adapter.unix_epoch_millis / 1000)) {
      messages.push('unix_epoch_seconds 与 unix_epoch_millis 对不上');
    }
    if (!Array.isArray(adapter.nondeterministic_fields) || adapter.nondeterministic_fields.length === 0) {
      messages.push('nondeterministic_fields 是空的——记录得自己说清哪些格子每次都会变');
    }
    return messages;
  },

  'run-shape': (leg) => {
    const { run, backend, frames, scenes, adapter } = parts(leg);
    const messages = [];
    if (run.kind !== 'corpus') messages.push(`run.json 的 kind=${JSON.stringify(run.kind)}`);
    if (run.schema !== 1) messages.push(`run.json 的 schema=${JSON.stringify(run.schema)}`);
    if (run.milestone !== EXPECTED.milestone) messages.push(`run.json 的 milestone=${JSON.stringify(run.milestone)}`);
    if (run.frame_range !== EXPECTED.frameRange) {
      messages.push(`frame_range=${JSON.stringify(run.frame_range)}，期望 ${EXPECTED.frameRange}`);
    }
    if (run.frames_per_scene !== EXPECTED.framesPerScene) {
      messages.push(`frames_per_scene=${JSON.stringify(run.frames_per_scene)}`);
    }
    if (run.target_size !== EXPECTED.targetSize) messages.push(`target_size=${JSON.stringify(run.target_size)}`);
    if (run.target_format !== EXPECTED.targetFormat) {
      messages.push(`target_format=${JSON.stringify(run.target_format)}`);
    }
    if (run.byte_tolerance !== EXPECTED.byteTolerance) {
      messages.push(`byte_tolerance=${JSON.stringify(run.byte_tolerance)}`);
    }
    // corpus 这条路上没有"每次都不一样"的东西：同一份输入就该出同一批字节。
    if (!Array.isArray(run.nondeterministic_fields) || run.nondeterministic_fields.length !== 0) {
      messages.push(
        `nondeterministic_fields=${JSON.stringify(run.nondeterministic_fields)}，corpus 记录里应当是空的`,
      );
    }
    const names = scenes.map((scene) => scene.name).join(',');
    if (names !== EXPECTED.scenes.join(',')) {
      messages.push(`场景集是 ${JSON.stringify(names)}，期望 ${JSON.stringify(EXPECTED.scenes.join(','))}`);
    }
    const backends = Array.isArray(run.backends) ? run.backends : [];
    if (backends.length !== 1) messages.push(`backends[] 有 ${backends.length} 条，一份记录一条腿应当只有 1 条`);
    if (typeof backend.adapter_name !== 'string' || backend.adapter_name.length === 0) {
      messages.push('run.json 里没写 adapter_name');
    } else if (backend.adapter_name !== adapter.adapter_name) {
      messages.push('run.json 与 adapter.json 的 adapter_name 不是同一个');
    }
    if (backend.requested !== BACKEND_LABEL[leg.slug]) {
      messages.push(`backend.requested=${JSON.stringify(backend.requested)}，这条腿应当是 ${BACKEND_LABEL[leg.slug]}`);
    }
    const artifacts = run.artifacts ?? {};
    if (artifacts.frame_count !== frames.length) {
      messages.push(`artifacts.frame_count=${artifacts.frame_count}，实际 ${frames.length}`);
    }
    if (artifacts.frames_dir !== 'frames') messages.push(`artifacts.frames_dir=${JSON.stringify(artifacts.frames_dir)}`);
    if (artifacts.adapter !== 'adapter.json') messages.push(`artifacts.adapter=${JSON.stringify(artifacts.adapter)}`);
    if (artifacts.readings !== 'readings.txt') {
      messages.push(`artifacts.readings=${JSON.stringify(artifacts.readings)}`);
    }
    return messages;
  },
};

/**
 * 判一条腿。返回**每一项都有一条结论**（顺序与 `LEG_CHECKS` 一致）的数组。
 *
 * 每项的 `detail` 最多列 6 条，免得一条系统性错误刷出几百行把真正的第一因埋掉。
 */
export function checkLegModel(leg) {
  return LEG_CHECKS.map((id) => {
    let messages;
    try {
      messages = CHECKS[id](leg);
    } catch (error) {
      messages = [`检查自己抛了异常：${error.message}`];
    }
    const shown = messages.slice(0, 6);
    if (messages.length > shown.length) shown.push(`…另有 ${messages.length - shown.length} 处`);
    return { id, ok: messages.length === 0, detail: shown.join('；') };
  });
}

// ---------------------------------------------------------------------------
// 判定：跨两条腿
// ---------------------------------------------------------------------------

/** 跨后端：同名帧的文件摘要与像素摘要都必须一样，整表摘要也必须一样。 */
export function checkCrossBackend(legs) {
  const messages = [];
  const [first, ...rest] = legs;
  if (!first || rest.length === 0) return ['少于两条腿，无从比跨后端一致性'];
  const framesOf = (leg) => parts(leg).frames;
  const base = framesOf(first);
  for (const leg of rest) {
    const frames = framesOf(leg);
    if (frames.length !== base.length) {
      messages.push(`${leg.slug} 有 ${frames.length} 帧，${first.slug} 有 ${base.length} 帧`);
    }
    const byName = new Map(frames.map((frame) => [frame.png, frame]));
    const fileDrift = [];
    const pixelDrift = [];
    for (const frame of base) {
      const twin = byName.get(frame.png);
      if (!twin) continue;
      const left = first.pngFiles.get(frame.png);
      const right = leg.pngFiles.get(frame.png);
      if (left?.fileDigest !== right?.fileDigest) fileDrift.push(frame.png);
      if (left?.pixelDigest !== right?.pixelDigest) pixelDrift.push(frame.png);
    }
    if (fileDrift.length > 0) {
      messages.push(`${first.slug} 与 ${leg.slug} 有 ${fileDrift.length} 张 PNG 文件字节不同：${fileDrift.slice(0, 4).join('、')}`);
    }
    if (pixelDrift.length > 0) {
      messages.push(`${first.slug} 与 ${leg.slug} 有 ${pixelDrift.length} 张 PNG 像素不同：${pixelDrift.slice(0, 4).join('、')}`);
    }
    const left = parts(first).backend.frames_digest;
    const right = parts(leg).backend.frames_digest;
    if (left !== right) {
      messages.push(`${first.slug} 的整表摘要 ${left} ≠ ${leg.slug} 的 ${right}`);
    }
  }
  return messages;
}

/**
 * 记录是否**诚实地**交代了自己缺哪一腿。
 *
 * M1 的退出标准要求四种环境全部记录。只跑了两条就归档，本身可以接受——但记录里
 * 必须把缺口标出来。悄悄只留两条、README 却写成"四种环境通过"，是这份守卫最该
 * 拦住的东西。
 *
 * 这条检查被独立复核用**一个空目录**绕过过一次：先前只要子目录名以 `linux` 开头
 * 就算"这条腿在"，于是 `mkdir records/m1/linux` + 删掉 README 里全部 ⏳ 仍然全绿。
 * 现在的口径是"**完整**的腿才算腿"（见 inspectLegDirs）：空壳目录本身要红——
 * 它比没有更坏，把缺口伪装成了进展；缺口还必须由 README 里与 Linux **同一行**的
 * ⏳ 标出来，藏在别处的 ⏳ 不算数。
 *
 * 于是这里是两条独立规则：
 *   ① `linux*` 目录存在但不是完整腿 → 红（空壳本身就是问题，标了 ⏳ 也照红）；
 *   ② 完整 Linux 腿不足 `LINUX_LEGS` 条 → README 必须有一行同时写 Linux 与 ⏳。
 * 两条都不数"别的目录有几条"，因为 Windows 那两条腿已经被 `main()` 单独管住
 * （缺 dx12/vulkan 直接退出码 2，且 `loadLeg` 会逐份读文件）。
 */
export function checkHonesty({ readmeText, subdirNames, completeLegDirs = [], incompleteLegDirs = [] }) {
  if (typeof readmeText !== 'string' || readmeText.trim().length === 0) {
    return ['records/m1/README.md 不存在或为空——记录必须有一份人读的说明'];
  }
  const messages = [];
  const isLinux = (name) => /^linux/i.test(name);

  for (const { name, missing } of incompleteLegDirs.filter((entry) => isLinux(entry.name))) {
    messages.push(
      `records/m1/${name}/ 存在，但不是一条完整的腿（缺 ${missing.join('、')}）——` +
        '空壳目录会把"Linux 那两条腿还缺着"伪装成已经补上：要么补全并归档，要么删掉它，' +
        '缺口改用 README 的 ⏳ 说明',
    );
  }

  const completeLinux = completeLegDirs.filter(isLinux);
  if (completeLinux.length < LINUX_LEGS) {
    const marked = readmeText.split('\n').some((line) => /linux/i.test(line) && line.includes('⏳'));
    if (!marked) {
      messages.push(
        `记录里完整的 Linux 腿只有 ${completeLinux.length} 条（要 ${LINUX_LEGS} 条；完整腿共 ` +
          `${completeLegDirs.length} 条，子目录：${subdirNames.join('、') || '（空）'}）；` +
          'README 里必须有一行**同时**写到 Linux 与 ⏳——退出标准要求四种环境全部记录，缺了就要写出来',
      );
    }
  }
  return messages;
}

/**
 * Linux 两条腿的复核口径。**与 Windows 那两条不是一套判据**，三条理由：
 *
 *   1. **不与 Windows 逐字节比**：换了 GPU 与驱动，字节不必相同 —— 本仓实测
 *      RADV 679b249510eea426、lavapipe e2291e1bf32ddef6，而 Windows 两条都是 71ecc80cade3d73d。
 *      退出标准①要的是「**重复运行**逐字节一致」，那由这条腿自己的 compare.json 给出。
 *   2. **不判 10 ms 预算**：lavapipe 是 CPU 软渲染，plan 写明「性能掉一个数量级，只用于链路验证」
 *      —— 拿预算判它等于把设计选择当成回归。数字照旧记在 timing.json 里，只是不拿它当门槛。
 *   3. 判的是**形状与自证**：帧数与场景集对得上、probe 摘要等于 golden、build_profile 是 release
 *      （debug 的计时没有意义）、adapter 能看出跑在什么上、而且 compare.json 必须说 identical。
 *
 * 完整 Linux 腿**不足两条**时返回 applicable=false —— 那种情况归 checkHonesty 的
 * 「README 里必须有同行 ⏳」管，不在这里重复判（否则同一条缺口会报两次）。
 */
export function checkLinuxLegs(recordDir, completeLegDirs) {
  const expected = ['linux-gpu', 'linux-lavapipe'];
  const present = completeLegDirs.filter((name) => /^linux/i.test(name));
  if (present.length < LINUX_LEGS) return { applicable: false, messages: [] };
  const messages = [];
  const wantFrames = EXPECTED.framesPerScene * EXPECTED.scenes.length;
  for (const slug of expected) {
    if (!present.includes(slug)) {
      messages.push('完整的 Linux 腿是 ' + present.join('、') + '，缺 ' + slug +
        '/ —— 四种环境的腿名是约定：linux-gpu（真 GPU）、linux-lavapipe（CPU 软渲染）');
      continue;
    }
    const leg = loadLeg(recordDir, slug);
    const { adapter, run, backend, scenes } = parts(leg);
    if (leg.frameNames.length !== wantFrames) {
      messages.push(slug + ' 有 ' + leg.frameNames.length + ' 张 PNG，期望 ' + wantFrames);
    }
    const sceneNames = scenes.map((scene) => scene.name).join(',');
    if (sceneNames !== EXPECTED.scenes.join(',')) {
      messages.push(slug + ' 的场景集是 ' + JSON.stringify(sceneNames));
    }
    if (adapter.probe_digest !== EXPECTED.probeDigest) {
      messages.push(slug + ' 的 probe_digest=' + JSON.stringify(adapter.probe_digest) +
        '，golden 是 ' + EXPECTED.probeDigest);
    }
    if (adapter.build_profile !== 'release') {
      messages.push(slug + ' 的 build_profile=' + JSON.stringify(adapter.build_profile) +
        ' —— Linux 腿要 release（debug 的计时说明不了任何事）');
    }
    const adapterInfo = adapter.adapter ?? {};
    if (typeof adapterInfo.name !== 'string' || adapterInfo.name.length === 0) {
      messages.push(slug + '/adapter.json 看不出跑在什么适配器上');
    }
    const compare = leg.compare;
    if (!compare || compare.__error !== undefined || compare.identical !== true) {
      messages.push(slug + '/compare.json 没说两次运行逐字节一致（' +
        JSON.stringify(compare && compare.__error ? compare.__error : compare && compare.identical) +
        '）—— 退出标准①要的正是这一条');
    }
    if (typeof backend.frames_digest !== 'string' || backend.frames_digest.length === 0) {
      messages.push(slug + '/run.json 里没有 frames_digest');
    }
  }
  return { applicable: true, messages };
}

// ---------------------------------------------------------------------------
// 自检
// ---------------------------------------------------------------------------

/** CRC-32（PNG 每个块尾部）。只为让自检造出来的 PNG 是**真 PNG**。 */
const CRC_TABLE = (() => {
  const table = new Uint32Array(256);
  for (let n = 0; n < 256; n += 1) {
    let c = n;
    for (let k = 0; k < 8; k += 1) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    table[n] = c >>> 0;
  }
  return table;
})();

function crc32(bytes) {
  let c = 0xffffffff;
  for (let i = 0; i < bytes.length; i += 1) c = CRC_TABLE[(c ^ bytes[i]) & 0xff] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
}

function pngChunk(type, data) {
  const length = Buffer.alloc(4);
  length.writeUInt32BE(data.length);
  const body = Buffer.concat([Buffer.from(type, 'latin1'), data]);
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(crc32(body));
  return Buffer.concat([length, body, crc]);
}

/** 造一张真 PNG。`filter` 指定整张图用哪一种滤波器，用来把解码器的五个分支都走一遍。 */
export function encodePng(width, height, rgba, filter) {
  const bpp = 4;
  const stride = width * bpp;
  const bppBytes = bpp;
  const raw = Buffer.alloc(height * (stride + 1));
  for (let y = 0; y < height; y += 1) {
    raw[y * (stride + 1)] = filter;
    for (let x = 0; x < stride; x += 1) {
      const value = rgba[y * stride + x];
      const left = x >= bppBytes ? rgba[y * stride + x - bppBytes] : 0;
      const up = y > 0 ? rgba[(y - 1) * stride + x] : 0;
      const upLeft = y > 0 && x >= bppBytes ? rgba[(y - 1) * stride + x - bppBytes] : 0;
      raw[y * (stride + 1) + 1 + x] = (value - unfilter(filter, left, up, upLeft)) & 0xff;
    }
  }
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(width, 0);
  ihdr.writeUInt32BE(height, 4);
  ihdr[8] = 8;
  ihdr[9] = 6;
  ihdr[10] = 0;
  ihdr[11] = 0;
  ihdr[12] = 0;
  return Buffer.concat([
    Buffer.from(PNG_SIGNATURE),
    pngChunk('IHDR', ihdr),
    pngChunk('IDAT', deflateSync(raw)),
    pngChunk('IEND', Buffer.alloc(0)),
  ]);
}

/**
 * 造一条**内容自洽**的合成腿。
 *
 * 尺寸：5 个场景 × 16 帧 = 80 帧；采样点 5/5/5/4/4 个，共 23 × 16 = 368 个。
 * 像素摘要与文件摘要都是编的（不用真 PNG），但**彼此必须自洽**——整表摘要是用
 * 同一套规则从同一批编造的像素摘要算出来的。这样"判定逻辑对不对"就能在不跑
 * GPU 的前提下被反复喂。
 */
export function syntheticLeg(slug = 'dx12') {
  const sampleCounts = [5, 5, 5, 4, 4];
  const epoch = 1790025577920;
  const label = BACKEND_LABEL[slug];

  const scenes = EXPECTED.scenes.map((name, index) => ({
    name,
    description: `${name} 的合成描述`,
    fragment_entries: [`fs_${name}`],
    passes: 1,
    // 与真实记录同一个形状：`blur`（最后一个）不随帧变，其余四个随帧变。
    // 自检的"诚实模型全绿"因此同时钉住三条规则的两个方向——判据写成"逐帧等于表"，
    // 这里会先红；判据漏掉锚定，那条反向用例会红。
    uses_frame: index !== EXPECTED.scenes.length - 1,
    samples: Array.from({ length: sampleCounts[index] }, (_, i) => ({
      x: 8 + i * 16,
      y: 128,
      label: `合成点 ${i}`,
      purpose: '自检用',
      expected: [10 + i, 20 + i, 30 + i, 255],
    })),
  }));

  /**
   * 合成腿里一个采样点在第 `frame` 帧的期望值。
   *
   * 第 0 帧等于 `scenes[].samples[].expected`（表就锚在第 0 帧上）；随帧变的场景
   * 从第 1 帧起第一通道加 `frame`，与真实记录"四变一不变"的形状一致。
   */
  const expectedOf = (scene, frame, sample) =>
    sample.expected.map((value, k) => (scene.uses_frame && frame > 0 && k === 0 ? value + frame : value));

  const frames = [];
  const pngFiles = new Map();
  for (const scene of scenes) {
    for (let frame = 0; frame < EXPECTED.framesPerScene; frame += 1) {
      const png = frameFileName(scene.name, frame);
      const pixelDigest = fnv1a64(Buffer.from(`pixel:${scene.name}:${frame}`, 'utf8'));
      const pngBytes = 1024 + frame;
      pngFiles.set(png, {
        size: pngBytes,
        fileDigest: fnv1a64(Buffer.from(`png:${scene.name}:${frame}`, 'utf8')),
        pixelDigest,
        width: 256,
        height: 256,
      });
      frames.push({
        scene: scene.name,
        frame,
        png,
        pixel_digest: pixelDigest,
        repeat_pixel_digest: pixelDigest,
        repeat_identical: true,
        png_digest: pngFiles.get(png).fileDigest,
        png_bytes: pngBytes,
        points: scene.samples.map((sample) => ({
          label: sample.label,
          x: sample.x,
          y: sample.y,
          purpose: sample.purpose,
          // 第 0 帧锚在表上；随帧变的场景从第 1 帧起偏离表（偏离的是第一通道，
          // 与 measured 同步挪，免得顺带把 distance 自洽也弄红——一处改动只该红一处）。
          measured: expectedOf(scene, frame, sample),
          expected: expectedOf(scene, frame, sample),
          distance: 0,
          tolerance: EXPECTED.byteTolerance,
          passed: true,
          detail: '合成',
        })),
      });
    }
  }

  const aggregate = framesDigest(frames.map((frame) => ({ ...frame, pixelDigest: frame.pixel_digest })));
  const cpuMedians = [0.1, 0.11, 0.12, 0.13, 0.14];
  const roundtripMedians = [2.0, 2.1, 2.2, 2.3, 2.4];
  const worstRoundtrip = Math.max(...roundtripMedians);

  const readings = [];
  readings.push('dhampir M1 corpus 逐点读数');
  readings.push(
    `帧 ${frames.length}、采样点 ${EXPECTED.points}、失败 0、越界 0、未判定 0；容差 ${EXPECTED.byteTolerance} 字节`,
  );
  readings.push('');
  for (const frame of frames) {
    readings.push(`--- ${frame.scene} f${String(frame.frame).padStart(3, '0')} （合成）`);
    for (const point of frame.points) {
      readings.push(`${frame.scene}     f${frame.frame}   (${point.x},${point.y}) ${point.label}`);
    }
    readings.push('');
  }

  return {
    slug,
    rootFiles: new Set(REQUIRED_ROOT_FILES),
    framesDirPresent: true,
    frameNames: frames.map((frame) => frame.png.slice('frames/'.length)),
    pngFiles,
    readings: readings.join('\n'),
    adapter: {
      milestone: EXPECTED.milestone,
      kind: 'adapter',
      schema: 1,
      probe_digest: EXPECTED.probeDigest,
      probe_format_version: 1,
      target_format: EXPECTED.targetFormat,
      corpus_target_size: EXPECTED.targetSize,
      adapter_name: 'Synthetic Adapter',
      backend_slug: slug,
      requested_backends: label,
      build_profile: 'release',
      crate_version: '0.0.1',
      wgpu_version: '30.0.1',
      naga_version: '30.0.1',
      unix_epoch_millis: epoch,
      unix_epoch_seconds: Math.floor(epoch / 1000),
      nondeterministic_fields: ['unix_epoch_seconds', 'unix_epoch_millis'],
      adapter: {
        name: 'Synthetic Adapter',
        backend: slug === 'dx12' ? 'Dx12' : 'Vulkan',
        driver: '0.0.0',
        device: '1',
        device_type: 'DiscreteGpu',
        vendor: '1',
        driver_info: '',
      },
    },
    run: {
      schema: 1,
      milestone: EXPECTED.milestone,
      kind: 'corpus',
      frame_range: EXPECTED.frameRange,
      frames_per_scene: EXPECTED.framesPerScene,
      target_size: EXPECTED.targetSize,
      target_format: EXPECTED.targetFormat,
      byte_tolerance: EXPECTED.byteTolerance,
      scenes,
      artifacts: {
        adapter: 'adapter.json',
        readings: 'readings.txt',
        frames_dir: 'frames',
        frame_count: frames.length,
      },
      backends: [
        {
          requested: label,
          adapter_name: 'Synthetic Adapter',
          frames_digest: aggregate,
          counts: {
            frames: frames.length,
            points: EXPECTED.points,
            failed: 0,
            out_of_range: 0,
            unjudged: 0,
            clean: true,
          },
          repeat_mismatches: [],
          frames,
        },
      ],
      nondeterministic_fields: [],
    },
    timing: {
      schema: 1,
      milestone: EXPECTED.milestone,
      kind: 'timing',
      backend_slug: slug,
      requested_backends: label,
      adapter_name: 'Synthetic Adapter',
      build_profile: 'release',
      init_ms: 12.5,
      timing_target_size: EXPECTED.timingSize,
      unix_epoch_millis: epoch,
      unix_epoch_seconds: Math.floor(epoch / 1000),
      nondeterministic_fields: ['unix_epoch_millis', 'init_ms'],
      timing: {
        size: EXPECTED.timingSize,
        frame: 5,
        warmup: 3,
        repeats: 24,
        budget_ms: EXPECTED.frameBudgetMs,
        budget_metric: 'worst_roundtrip_ms',
        budget_metric_note: '合成：判的是渲染 + 读回的往返',
        worst_frame_cpu_ms: Math.max(...cpuMedians),
        worst_roundtrip_ms: worstRoundtrip,
        verdict: worstRoundtrip <= EXPECTED.frameBudgetMs,
        verdict_note: null,
        scenes: EXPECTED.scenes.map((name, index) => ({
          scene: name,
          frame: 5,
          frame_cpu_ms: { n: 24, min_ms: cpuMedians[index] - 0.01, median_ms: cpuMedians[index], max_ms: cpuMedians[index] + 0.01 },
          readback_ms: { n: 24, min_ms: roundtripMedians[index] - 0.1, median_ms: roundtripMedians[index], max_ms: roundtripMedians[index] + 0.1 },
        })),
      },
      row_alignment: {
        size: EXPECTED.alignSize,
        unpadded_bytes_per_row: 1366 * 4,
        padded_bytes_per_row: 5632,
        frame: 0,
        byte_tolerance: EXPECTED.byteTolerance,
        pixels_compared: 1366 * 768,
        pixels_ok: 1366 * 768,
        worst_distance: 0,
        exercises_padding: true,
        examples: [],
        detail: null,
        ok: true,
      },
      align_probe: {
        scene: EXPECTED.alignScene,
        frame: EXPECTED.alignFrame,
        size: EXPECTED.alignSize,
        unpadded_bytes_per_row: 1366 * 4,
        padded_bytes_per_row: 5632,
      },
    },
    compare: {
      schema: 1,
      identical: true,
      backends_compared: 1,
      backends_not_compared: 0,
      note: '合成：第二个进程写出来的比对',
      backends: [
        {
          requested: label,
          adapter_name: 'Synthetic Adapter',
          other_adapter_name: 'Synthetic Adapter',
          frames_digest: aggregate,
          other_frames_digest: aggregate,
          frame_count: frames.length,
          other_frame_count: frames.length,
          matched_frames: frames.length,
          identical: true,
          note: null,
          pixel_mismatches: [],
          png_mismatches: [],
          missing_in_other: [],
          missing_in_current: [],
        },
      ],
    },
  };
}

/** 每一处改动对应一个必须红的检查项。改动只碰一处，所以"红在哪"本身就是证据。 */
const MUTATIONS = [
  { name: '少一份 timing.json', expect: 'required-files', mutate: (leg) => leg.rootFiles.delete('timing.json') },
  { name: '少一张 PNG', expect: 'frame-set', mutate: (leg) => leg.frameNames.splice(3, 1) },
  {
    name: '改一帧的 png_bytes',
    expect: 'png-bytes-and-digest',
    mutate: (leg) => {
      leg.run.backends[0].frames[7].png_bytes += 1;
    },
  },
  {
    name: '改一帧的 pixel_digest（文件没动）',
    expect: 'pixels',
    mutate: (leg) => {
      leg.run.backends[0].frames[7].pixel_digest = '0000000000000000';
    },
  },
  {
    name: '一帧的同帧两次渲染不一致',
    expect: 'repeat',
    mutate: (leg) => {
      leg.run.backends[0].frames[9].repeat_identical = false;
    },
  },
  {
    name: '改整表摘要',
    expect: 'frames-digest',
    mutate: (leg) => {
      leg.run.backends[0].frames_digest = '0000000000000000';
    },
  },
  {
    name: '改 counts.points',
    expect: 'counts',
    mutate: (leg) => {
      leg.run.backends[0].counts.points -= 1;
    },
  },
  {
    name: '把一个采样点判成未通过',
    expect: 'points',
    mutate: (leg) => {
      leg.run.backends[0].frames[4].points[0].passed = false;
    },
  },
  {
    name: '第 0 帧的期望值与 scenes[] 的表错开',
    expect: 'points',
    mutate: (leg) => {
      leg.run.backends[0].frames[0].points[0].expected[0] += 1;
    },
  },
  {
    name: '采样点的 purpose 与表错开',
    expect: 'points',
    mutate: (leg) => {
      leg.run.backends[0].frames[2].points[1].purpose = '换了说法';
    },
  },
  {
    name: '声明随帧变、期望值却 16 帧一个样',
    expect: 'points',
    mutate: (leg) => {
      // blur 是合成腿里唯一不随帧变的场景：把声明改成 true，期望值必须跟着变才算数
      leg.run.scenes[EXPECTED.scenes.length - 1].uses_frame = true;
    },
  },
  {
    name: 'uses_frame 不是布尔值',
    expect: 'points',
    mutate: (leg) => {
      leg.run.scenes[0].uses_frame = 'yes';
    },
  },
  {
    name: 'readings.txt 少一个帧标题',
    expect: 'readings',
    mutate: (leg) => {
      leg.readings = leg.readings.replace(/^--- .*$/m, '');
    },
  },
  {
    name: '判据换成纯 CPU 提交那个数',
    expect: 'timing',
    mutate: (leg) => {
      leg.timing.timing.budget_metric = 'worst_frame_cpu_ms';
    },
  },
  {
    name: '往返超预算却写着通过',
    expect: 'timing',
    mutate: (leg) => {
      leg.timing.timing.worst_roundtrip_ms = EXPECTED.frameBudgetMs + 1;
    },
  },
  {
    name: '行对齐在不需要填充的尺寸上宣布通过',
    expect: 'alignment',
    mutate: (leg) => {
      leg.timing.row_alignment.exercises_padding = false;
    },
  },
  {
    name: '跨进程比对其实不一致',
    expect: 'compare',
    mutate: (leg) => {
      leg.compare.identical = false;
    },
  },
  {
    name: '探针摘要被改',
    expect: 'adapter',
    mutate: (leg) => {
      leg.adapter.probe_digest = '0000000000000000';
    },
  },
  {
    name: 'corpus 记录里出现了非确定项',
    expect: 'run-shape',
    mutate: (leg) => {
      leg.run.nondeterministic_fields = ['init_ms'];
    },
  },
];

/**
 * 自检：造模型 → 判定。返回 `{ failures, count }`。
 *
 * `count` 必须是**真的数出来的**断言条数，不能拿公式凑：守卫报的每个数都是它的
 * 结论的一部分，"跑了 49 条"这句话要是假的，别的数也就没人信了。
 */
function runSelfTest() {
  const failures = [];
  let count = 0;
  const expect = (name, condition, detail) => {
    count += 1;
    if (!condition) failures.push(`${name}：${detail}`);
  };

  // ---- 摘要本身的锚点：FNV-1a 64 的标准测试向量 ----
  const vectors = [
    ['', 'cbf29ce484222325'],
    ['a', 'af63dc4c8601ec8c'],
    ['foobar', '85944171f73967e8'],
  ];
  for (const [text, want] of vectors) {
    const got = fnv1a64(Buffer.from(text, 'utf8'));
    expect(`fnv1a64(${JSON.stringify(text)})`, got === want, `得到 ${got}，标准值 ${want}`);
  }

  // ---- 解码器：五种滤波器都要能解回来，解不动的形态要拒绝 ----
  const width = 3;
  const height = 2;
  const rgba = Buffer.from([
    1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12,
    13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24,
  ]);
  for (let filter = 0; filter <= 4; filter += 1) {
    const png = encodePng(width, height, rgba, filter);
    try {
      const image = decodePng(png);
      expect(
        `解码滤波器 ${filter}`,
        image.width === width && image.height === height && image.pixels.equals(rgba),
        '解回来的像素与原图不一致',
      );
    } catch (error) {
      failures.push(`解码滤波器 ${filter}：抛了异常 ${error.message}`);
    }
  }
  try {
    const bad = Buffer.from(encodePng(width, height, rgba, 0));
    bad[24] = 2; // colorType 改成 RGB，超出本守卫支持的形态
    decodePng(bad);
    failures.push('拒绝非 RGBA 的 PNG：居然解开了');
  } catch {
    /* 期望就是拒绝 */
  }

  // ---- 整表摘要：顺序敏感、内容敏感 ----
  const entries = [
    { scene: 'a', frame: 0, pixelDigest: '0000000000000001' },
    { scene: 'a', frame: 1, pixelDigest: '0000000000000002' },
  ];
  expect('整表摘要顺序敏感', framesDigest(entries) !== framesDigest([...entries].reverse()), '换个顺序摘要没变');
  const tweaked = [{ ...entries[0] }, { ...entries[1], pixelDigest: '0000000000000003' }];
  expect('整表摘要内容敏感', framesDigest(entries) !== framesDigest(tweaked), '改一个像素摘要整表摘要没变');

  // ---- 判定逻辑：诚实的模型必须全绿，每一处改动都必须红 ----
  const honest = syntheticLeg();
  const honestResults = checkLegModel(honest);
  expect('检查项数量', honestResults.length === LEG_CHECKS.length, `得到 ${honestResults.length} 项`);
  const honestBad = honestResults.filter((result) => !result.ok);
  expect(
    '诚实的模型全绿',
    honestBad.length === 0,
    honestBad.map((result) => `${result.id}(${result.detail})`).join(' | '),
  );

  for (const mutation of MUTATIONS) {
    const leg = syntheticLeg();
    mutation.mutate(leg);
    const failed = checkLegModel(leg)
      .filter((result) => !result.ok)
      .map((result) => result.id);
    expect(
      `改动「${mutation.name}」必须被 ${mutation.expect} 抓到`,
      failed.includes(mutation.expect),
      `红的是 ${failed.length === 0 ? '（什么都没红）' : failed.join('、')}`,
    );
  }
  const covered = new Set(MUTATIONS.map((mutation) => mutation.expect));
  const uncovered = LEG_CHECKS.filter((id) => !covered.has(id));
  expect('每个检查项都有反向用例', uncovered.length === 0, `没有反向用例的是 ${uncovered.join('、')}`);

  // ---- 跨后端 ----
  const left = syntheticLeg('dx12');
  const right = syntheticLeg('vulkan');
  expect('两条一样的腿跨后端全绿', checkCrossBackend([left, right]).length === 0, checkCrossBackend([left, right]).join(' | '));
  const drifted = syntheticLeg('vulkan');
  drifted.pngFiles.get('frames/gradient-f000.png').fileDigest = '0000000000000000';
  expect(
    '文件字节不同必须被抓到',
    checkCrossBackend([left, drifted]).some((message) => message.includes('文件字节不同')),
    '没报出文件字节不同',
  );
  const pixelDrift = syntheticLeg('vulkan');
  pixelDrift.run.backends[0].frames[0].scene = 'gradient';
  pixelDrift.pngFiles.get('frames/gradient-f001.png').pixelDigest = '0000000000000000';
  expect(
    '像素不同必须被抓到',
    checkCrossBackend([left, pixelDrift]).some((message) => message.includes('像素不同')),
    '没报出像素不同',
  );
  expect('少于两条腿要拒绝', checkCrossBackend([left]).length > 0, '只有一条腿却说一致');

  // ---- 目录分类：什么才算"一条完整的腿" ----
  // 这一组必须走**真文件系统**：`inspectLegDirs` 的失效模式是"把空壳看成腿"，
  // 而那件事只能拿真目录试出来——独立复核正是用 `mkdir records/m1/linux` 绕过的。
  const probeRoot = mkdtempSync(join(tmpdir(), 'dhampir-m1-legs-'));
  try {
    const completeDir = join(probeRoot, 'linux-gpu');
    mkdirSync(join(completeDir, 'frames'), { recursive: true });
    for (const file of REQUIRED_ROOT_FILES) writeFileSync(join(completeDir, file), '{}\n');
    writeFileSync(join(completeDir, 'frames', 'gradient-f000.png'), Buffer.from(PNG_SIGNATURE));

    const shellDir = join(probeRoot, 'linux');
    mkdirSync(shellDir, { recursive: true });

    const noPngDir = join(probeRoot, 'linux-lavapipe');
    mkdirSync(join(noPngDir, 'frames'), { recursive: true });
    for (const file of REQUIRED_ROOT_FILES) writeFileSync(join(noPngDir, file), '{}\n');

    const inspect = inspectLegDirs(probeRoot, ['linux', 'linux-gpu', 'linux-lavapipe']);
    expect(
      '五份文件齐 + frames/ 里有 PNG 才算完整腿',
      inspect.complete.join(',') === 'linux-gpu',
      `判成完整的：${inspect.complete.join('、') || '（空）'}`,
    );
    expect(
      '空壳目录判成不完整（缺全部必备文件 + frames/）',
      inspect.incomplete.some((entry) => entry.name === 'linux' && entry.missing.length > REQUIRED_ROOT_FILES.length),
      `不完整的：${inspect.incomplete.map((entry) => `${entry.name}(${entry.missing.length})`).join('、') || '（空）'}`,
    );
    expect(
      'frames/ 里没有 PNG 也算不完整',
      inspect.incomplete.some((entry) => entry.name === 'linux-lavapipe' && entry.missing.some((m) => m.includes('PNG'))),
      `不完整的：${inspect.incomplete.map((entry) => entry.name).join('、') || '（空）'}`,
    );
  } finally {
    rmSync(probeRoot, { recursive: true, force: true });
  }

  // ---- 诚实性 ----
  const noLinuxLegs = { completeLegDirs: ['dx12', 'vulkan'] };
  expect(
    '缺 Linux 腿且没标 ⏳ 必须报',
    checkHonesty({ readmeText: '四种环境全部通过', subdirNames: ['dx12', 'vulkan'], ...noLinuxLegs }).length === 1,
    '漏掉了缺口',
  );
  expect(
    '标了 ⏳ 就放行',
    checkHonesty({ readmeText: 'Linux 两条腿 ⏳ 待补', subdirNames: ['dx12', 'vulkan'], ...noLinuxLegs }).length === 0,
    '标了 ⏳ 还在报',
  );
  expect(
    '⏳ 必须与 Linux 同行，藏在下几行不算',
    checkHonesty({ readmeText: 'Linux 两条腿待补\n\n⏳\n', subdirNames: ['dx12', 'vulkan'], ...noLinuxLegs }).length === 1,
    '不同行的 ⏳ 也被认了',
  );
  expect(
    '两条完整的 Linux 腿才不要求 ⏳',
    checkHonesty({
      readmeText: '四种环境都在',
      subdirNames: ['dx12', 'vulkan', 'linux-gpu', 'linux-lavapipe'],
      completeLegDirs: ['dx12', 'vulkan', 'linux-gpu', 'linux-lavapipe'],
    }).length === 0,
    '四条腿齐了还在报',
  );
  expect(
    '只有一条完整 Linux 腿仍要 ⏳（退出标准要的是四条环境）',
    checkHonesty({
      readmeText: 'Linux 都在了',
      subdirNames: ['dx12', 'vulkan', 'linux-gpu'],
      completeLegDirs: ['dx12', 'vulkan', 'linux-gpu'],
    }).length === 1,
    '一条 Linux 腿就放行了',
  );
  expect(
    '空壳 linux/ 目录本身就是红——标了 ⏳ 也照红',
    checkHonesty({
      readmeText: 'Linux 两条腿 ⏳ 待补',
      subdirNames: ['dx12', 'vulkan', 'linux'],
      completeLegDirs: ['dx12', 'vulkan'],
      incompleteLegDirs: [{ name: 'linux', missing: ['adapter.json'] }],
    }).length === 1,
    '空壳目录被放过了',
  );
  expect(
    '非 Linux 命名的空壳目录不掺和这条判定',
    checkHonesty({
      readmeText: 'Linux 两条腿 ⏳ 待补',
      subdirNames: ['dx12', 'vulkan', 'scratch'],
      completeLegDirs: ['dx12', 'vulkan'],
      incompleteLegDirs: [{ name: 'scratch', missing: ['frames/'] }],
    }).length === 0,
    '把只管 Linux 的判定扩大到了别的目录',
  );
  expect('README 空要报', checkHonesty({ readmeText: '', subdirNames: ['dx12'] }).length === 1, '空 README 放行了');

  // ---- Linux 两条腿的复核口径 ----
  expect(
    '不足两条完整 Linux 腿时这条判据不适用（缺口归 ⏳ 那条管）',
    checkLinuxLegs('records/m1', ['dx12', 'vulkan']).applicable === false
      && checkLinuxLegs('records/m1', ['dx12', 'vulkan', 'linux-gpu']).applicable === false,
    '把"还没归档"当成了"归档了但不合格"',
  );
  const linuxProbe = mkdtempSync(join(tmpdir(), 'm1-linux-legs-'));
  try {
    for (const slug of ['linux-gpu', 'linux-lavapipe']) {
      const dir = join(linuxProbe, slug);
      mkdirSync(join(dir, 'frames'), { recursive: true });
      writeFileSync(join(dir, 'frames', 'frame-0000.png'), 'not-a-png');
      writeFileSync(join(dir, 'adapter.json'), JSON.stringify({
        probe_digest: 'deadbeefdeadbeef', build_profile: 'debug', adapter: { name: '' },
      }));
      writeFileSync(join(dir, 'run.json'), JSON.stringify({ scenes: ['gradient'], backends: [{}] }));
      writeFileSync(join(dir, 'readings.txt'), 'x');
      writeFileSync(join(dir, 'timing.json'), '{}');
      writeFileSync(join(dir, 'compare.json'), JSON.stringify({ identical: false }));
    }
    const bad = checkLinuxLegs(linuxProbe, ['dx12', 'vulkan', 'linux-gpu', 'linux-lavapipe']);
    expect('两条完整但内容不合格的 Linux 腿必须逐条报出来', bad.applicable && bad.messages.length > 0,
      '不合格的 Linux 腿被放行了');
    expect('要点名 probe 摘要对不上 golden',
      bad.messages.some((m) => m.includes('probe_digest')), bad.messages.join(' | '));
    expect('要点名 compare 没说逐字节一致',
      bad.messages.some((m) => m.includes('identical') || m.includes('逐字节一致')), bad.messages.join(' | '));
    expect('要点名 build_profile 不是 release',
      bad.messages.some((m) => m.includes('build_profile')), bad.messages.join(' | '));
    expect('腿名换了（linux-x/linux-y）也要报',
      checkLinuxLegs(linuxProbe, ['dx12', 'vulkan', 'linux-x', 'linux-y']).messages.length > 0,
      '腿名不对却放行');
  } finally {
    rmSync(linuxProbe, { recursive: true, force: true });
  }
  if (existsSync(join(DEFAULT_RECORD, 'linux-gpu'))) {
    const real = checkLinuxLegs(DEFAULT_RECORD, ['dx12', 'vulkan', 'linux-gpu', 'linux-lavapipe']);
    expect('真记录里的两条 Linux 腿按自己的口径全绿',
      real.applicable && real.messages.length === 0, real.messages.join(' | '));
  }

  return { failures, count };
}

// ---------------------------------------------------------------------------
// 主流程
// ---------------------------------------------------------------------------

function readmeTextOf(recordDir) {
  const path = join(recordDir, 'README.md');
  if (!existsSync(path)) return null;
  return readFileSync(path, 'utf8');
}

function main() {
  // 参数先判死：忽略一个不认识的参数，等于给出一条永远绿的路径。
  const argv = process.argv.slice(2);
  let recordDir = DEFAULT_RECORD;
  let selfTest = false;
  let help = false;
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    if (arg === '--self-test') selfTest = true;
    else if (arg === '-h' || arg === '--help') help = true;
    else if (arg === '--record') {
      const value = argv[i + 1];
      if (value === undefined || value.startsWith('--')) {
        console.error('✗ --record 后面缺目录');
        return 2;
      }
      recordDir = resolve(REPO_ROOT, value);
      i += 1;
    } else {
      console.error(`✗ 不认识的参数：${arg}`);
      console.error('  用法：node scripts/check-m1-record.mjs [--record records/m1] [--self-test]');
      return 2;
    }
  }
  if (help) {
    console.log('用法：node scripts/check-m1-record.mjs [--record records/m1] [--self-test]');
    return 0;
  }

  const { failures: selfTestFailures, count: selfTestCount } = runSelfTest();
  if (selfTestFailures.length > 0) {
    console.error('✗ 守卫自检失败——先修守卫，别信它的结论：');
    for (const failure of selfTestFailures) console.error(`  - ${failure}`);
    return 2;
  }
  if (selfTest) {
    console.log(
      `✓ 守卫自检通过（${selfTestCount} 条用例：摘要标准向量、五种滤波器往返、`
        + `目录分类、${LEG_CHECKS.length} 个检查项各一条反向用例、跨后端与诚实性）`,
    );
    return 0;
  }

  // ---- 记录在不在、空不空 ----
  if (!existsSync(recordDir)) {
    console.error(`✗ 没有 ${recordDir} 这个目录`);
    return 2;
  }
  // 目录与符号链接都算候选腿：用 `ln -s` 造出来的"腿"同样要按完整腿的标准查一遍。
  const subdirNames = readdirSync(recordDir, { withFileTypes: true })
    .filter((entry) => entry.isDirectory() || entry.isSymbolicLink())
    .map((entry) => entry.name)
    .sort();
  const legs = [];
  for (const slug of LEGS) {
    if (!existsSync(join(recordDir, slug))) {
      console.error(`✗ 缺 ${slug}/ 这条腿`);
      return 2;
    }
    legs.push(loadLeg(recordDir, slug));
  }
  const pngCount = legs.reduce((n, leg) => n + leg.frameNames.length, 0);
  if (pngCount === 0) {
    console.error(`✗ ${recordDir} 下一条腿都没找到 PNG——多半是路径写错了。拒绝在空集合上通过。`);
    return 2;
  }

  // ---- 判 ----
  const results = [];
  for (const leg of legs) {
    for (const result of checkLegModel(leg)) results.push({ ...result, where: leg.slug });
  }

  const crossMessages = checkCrossBackend(legs);
  results.push({
    id: 'cross-backend-bytes',
    where: '两条腿',
    ok: crossMessages.length === 0,
    detail: crossMessages.slice(0, 6).join('；'),
  });

  // 纯逻辑探针的 golden 报告：字节自己摘要一次，与 M0 就钉住的摘要比。
  const probeMessages = [];
  const probePath = join(recordDir, 'selfcheck-native.txt');
  if (!existsSync(probePath)) {
    probeMessages.push('缺 selfcheck-native.txt');
  } else {
    const bytes = readFileSync(probePath);
    const digest = fnv1a64(bytes);
    if (digest !== EXPECTED.probeDigest) {
      probeMessages.push(`重算摘要 ${digest}，golden 是 ${EXPECTED.probeDigest}`);
    }
    const text = bytes.toString('utf8');
    const lines = text.split('\n').length - 1;
    if (!text.startsWith('dhampir-probe v')) probeMessages.push('第一行不是 dhampir-probe 版本行');
    if (lines !== EXPECTED.probeLines) probeMessages.push(`有 ${lines} 行，期望 ${EXPECTED.probeLines} 行`);
    for (const leg of legs) {
      if (leg.adapter?.probe_digest !== digest) {
        probeMessages.push(`${leg.slug}/adapter.json 里的 probe_digest 与这份报告的字节对不上`);
      }
    }
  }
  results.push({
    id: 'native-probe-report',
    where: '记录根',
    ok: probeMessages.length === 0,
    detail: probeMessages.slice(0, 6).join('；'),
  });

  // 诚实性判定要知道"哪些目录算一条完整的腿"（见 inspectLegDirs）——只数目录名
  // 曾被一个空壳 `linux/` 骗过去。
  const { complete: completeLegDirs, incomplete: incompleteLegDirs } = inspectLegDirs(recordDir, subdirNames);
  const honestyMessages = checkHonesty({
    readmeText: readmeTextOf(recordDir),
    subdirNames,
    completeLegDirs,
    incompleteLegDirs,
  });
  results.push({
    id: 'record-honesty',
    where: '记录根',
    ok: honestyMessages.length === 0,
    detail: honestyMessages.slice(0, 6).join('；'),
  });

  // Linux 两条腿归档之后，按**它们自己的口径**复核（见 checkLinuxLegs）。
  const linuxVerdict = checkLinuxLegs(recordDir, completeLegDirs);
  if (linuxVerdict.applicable) {
    results.push({
      id: 'linux-legs',
      where: 'Linux 两条腿',
      ok: linuxVerdict.messages.length === 0,
      detail: linuxVerdict.messages.slice(0, 6).join('；'),
    });
  }

  // ---- 报 ----
  const failed = results.filter((result) => !result.ok);
  if (failed.length > 0) {
    console.error(`✗ records/m1 复核未通过：${failed.length}/${results.length} 项红`);
    for (const result of failed) console.error(`  [${result.where}] ${result.id}: ${result.detail}`);
    return 1;
  }

  const first = parts(legs[0]).backend.frames_digest;
  console.log(
    `✓ records/m1 复核通过：${legs.length} 条腿 × ${legs[0].frameNames.length} 张 PNG（共 ${pngCount} 张），` +
      `每张的 png_digest 与 pixel_digest 都由本守卫自行重算并一致；` +
      `整表摘要 ${first} 由重算结果复现；跨后端逐字节一致；${results.length} 项检查全绿`,
  );
  return 0;
}

// 只设 process.exitCode，不调 process.exit()——见 scripts/wasm-test-node-exit-shim.cjs：
// 在本机的 Node/Windows 上，真正被执行的 process.exit() 可能撞上 libuv 的
// UV_HANDLE_CLOSING 断言，退出码变成负数。守卫的退出码是它的唯一结论。
process.exitCode = main();
