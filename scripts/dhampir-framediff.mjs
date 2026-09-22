#!/usr/bin/env node
// dhampir-framediff —— 两组帧目录（PNG）之间的逐帧比对：SSIM + PSNR + 最大绝对差 + 放大差异图。
//
// ===========================================================================
// 为什么是 Node 脚本，而不是 plan 原文里的 Rust 小 crate（tools/dhampir-framediff）
// ===========================================================================
//
// ① **解码器必须独立于生成器**。两侧 PNG 都是 core 用 `png` crate 编出来的；
//    比对工具如果再拿同一个 crate 解回来，"验"与"生成"就共用同一套别人写的代码——
//    库错在哪、滤波器理解错在哪，两侧会同样地错，SSIM 照样报 1.0，工具根本看不出来。
//    这里自己写解码：只借 `node:zlib` 解 DEFLATE（那是压缩标准，不是 PNG 知识），
//    块结构、滤波、CRC 全部自己来——与 `scripts/check-m1-record.mjs` 同一范式。
// ② CI 不新增运行时依赖：Node 本就是守卫套件的依赖；不用 Python、不引 PNG 库。
// ③ 与证据链同族：`scripts/` 下的工具都以 `--self-test` + 退出码 0/1/2 为契约，
//    记录守卫（check-m2-record.mjs）要能独立地再跑一遍它。
//    （位置偏离 plan 原文一处，已回填 plan §5 T2.4。）
//
// ===========================================================================
// 判定口径（写清楚，别让读记录的人猜）
// ===========================================================================
//
// · SSIM：**luma** 上算（Y = 0.2126R + 0.7152G + 0.0722B，取 PNG 里存的 sRGB
//   编码字节，不是线性值）；11×11 高斯窗（σ=1.5，权重归一化），K1=0.01、K2=0.03、
//   L=255；窗口只取 **valid** 区域（不 padding，不补边），逐帧一个均值。
// · PSNR：RGB 三通道的 MSE 上算，10·log10(255²/MSE)；两侧逐字节相同 → 无穷，
//   CSV 写 `inf`、JSON 写 `null`（JSON 没有无穷这个值）。
// · 最大绝对差：**RGBA 四通道**取最大（渲染是 Rgba8UnormSrgb，alpha 漂移一样是缺陷）。
// · 差异像素：四个通道里**任一**不同的像素数。
// · 三个指标都留着的原因：SSIM 会漏窄通道的差（蓝通道权重只有 0.0722），
//   maxdiff 会漏大面积低幅漂移，PSNR 居中。谁替不了谁。
// · 放大差异图：只画 RGB 三通道的 |Δ|，乘 `--amp`（默认 16）后 clamp 到 255，
//   写在 `<out>/diff/<原名>.png`。**有差异才写**——全等的帧不写黑图充数，
//   CSV 的 `diff_image` 列留空（"没有差异"由 `max_abs_diff=0` 说话）。
// · 证据图（差异 PNG 自己）不承诺跨 zlib 版本逐字节稳定；`summary.csv`、`verdict.json`
//   与 `report.txt` **承诺可复现**：同一输入重跑逐字节一致（自检里钉住）。
//   `report.txt` 落的就是屏幕上那份报告（同一个函数出），记录里要的是文件不是口述。
//
// ===========================================================================
// 名字契约与集合检查
// ===========================================================================
//
// · 文件名 `<场景>-f<帧号>.png`（与 core 的 frame_name 同一约定）。
// · 两侧**文件名集合必须相等**——不取交集。取交集会让"某一侧少了一批帧"伪装成全过。
// · 每场景帧号必须从 0 起连续；同一个帧号解析出两次（f2 与 f002 并存）也是红。
// · 本工具只保证"两侧名字集相同且帧号连续"；**两侧同时被截尾**在名字上看不出来，
//   那要靠记录守卫对着 run.json 的 frame_count 抓。
// · 目录里非 PNG 的文件不参与比对，但会被列出并写进 verdict.json（不沉默地丢）。
//
// ===========================================================================
// 退出码（"判红"与"用不了"必须分开，读记录的人才能归因）
// ===========================================================================
//
//   0  全部场景达标
//   1  有场景不达标（判定红）——summary.csv / verdict.json / report.txt 已落盘，照它归因
//   2  工具用不了或输入不成立：参数错、目录缺、名字集对不上、空集、解码失败、阈值档缺
//
// 用法：
//   node scripts/dhampir-framediff.mjs --a <dir> --b <dir> [--label-a <名>] [--label-b <名>]
//        [--out <dir>] [--thresholds <file>] [--amp <1..255>]
//   node scripts/dhampir-framediff.mjs --self-test
//
//   相对路径按仓库根解析（与 run-browser-corpus.mjs 同约定）；不给 --out 就只算不落盘。
//
// summary.csv 的列（固定顺序）：
//   scene,frame,file,width,height,ssim_luma,psnr_rgb_db,max_abs_diff,diff_pixels,diff_image

import { existsSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { deflateSync, inflateSync } from 'node:zlib';
import { tmpdir } from 'node:os';
import { dirname, isAbsolute, join, relative, resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';

const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const DEFAULT_THRESHOLDS = 'scripts/framediff-thresholds.toml';

/**
 * 报告里说落盘位置时用仓库相对路径（一律正斜杠）。
 * 理由：报告承诺"同一输入重跑逐字节一致"——要是写本机绝对路径，
 * 在另一台机器/另一个检出上重跑的复核者会看到不同的文本，把正常差异误当成不一致。
 * 仓库外的路径（比如自检用的临时目录）原样保留，那才是它的真位置。
 */
function relToRepo(path) {
  const rel = relative(REPO_ROOT, path);
  if (rel === '' || rel.startsWith('..') || isAbsolute(rel)) return path;
  return rel.split(sep).join('/');
}
const USAGE = [
  '用法：',
  '  node scripts/dhampir-framediff.mjs --a <dir> --b <dir> [--label-a <名>] [--label-b <名>]',
  '       [--out <dir>] [--thresholds <file>] [--amp <1..255>] [--shape]',
  '  node scripts/dhampir-framediff.mjs --self-test',
  '  相对路径按仓库根解析；不给 --out 就只算不落盘。',
  '  --shape 额外把差异的**空间形状**写成 <out>/shape.json（要求同时给 --out）。',
  '          它只记事实（整高列、符号象限、|Δ|≥2…），可接受与否的判断在子集文档里。',
].join('\n');

// ---------------------------------------------------------------------------
// 参数
// ---------------------------------------------------------------------------

export function parseArgs(argv) {
  const out = {
    a: null,
    b: null,
    labelA: 'A',
    labelB: 'B',
    out: null,
    thresholds: DEFAULT_THRESHOLDS,
    amp: 16,
    selfTest: false,
    shape: false,
    help: false,
  };
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    const takeValue = () => {
      const value = argv[i + 1];
      if (value === undefined || value.startsWith('--')) return { error: `${arg} 后面缺值` };
      i += 1; // 吃掉值，别让它下一轮被当成未知参数
      return { value };
    };
    if (arg === '--a' || arg === '--b' || arg === '--out' || arg === '--thresholds' || arg === '--label-a' || arg === '--label-b') {
      const got = takeValue();
      if (got.error) return { error: got.error };
      if (arg === '--a') out.a = got.value;
      else if (arg === '--b') out.b = got.value;
      else if (arg === '--out') out.out = got.value;
      else if (arg === '--thresholds') out.thresholds = got.value;
      else if (arg === '--label-a') out.labelA = got.value;
      else out.labelB = got.value;
    } else if (arg === '--amp') {
      const got = takeValue();
      if (got.error) return { error: got.error };
      if (!/^\d+$/.test(got.value)) return { error: `--amp 只收整数，收到 ${JSON.stringify(got.value)}` };
      const amp = Number.parseInt(got.value, 10);
      if (amp < 1 || amp > 255) return { error: `--amp 要在 1..255，收到 ${amp}（再大也只是提前被 clamp 掉）` };
      out.amp = amp;
    } else if (arg === '--self-test') out.selfTest = true;
    else if (arg === '--shape') out.shape = true;
    else if (arg === '-h' || arg === '--help') out.help = true;
    else return { error: `不认识的参数：${arg}` };
  }
  return out;
}

// ---------------------------------------------------------------------------
// PNG：只认一种形态（8 位 RGBA、无隔行）——core 的编码器写出来的形态
//
// 为什么自己写解码、不引依赖：见文件头 ①。代价几十行，收益是"PNG 里确实是
// 那批像素"这句话由一套**独立于渲染器**的代码说出来。
// ---------------------------------------------------------------------------

const PNG_SIGNATURE = Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]);

/** 解一张 PNG，返回 `{ width, height, pixels }`（pixels 是紧凑的 RGBA8）。 */
export function decodePng(bytes) {
  if (bytes.length < PNG_SIGNATURE.length || !bytes.subarray(0, PNG_SIGNATURE.length).equals(PNG_SIGNATURE)) {
    throw new Error('不是 PNG 签名');
  }

  let ihdr = null;
  const idat = [];
  let offset = 8;
  while (offset + 8 <= bytes.length) {
    const length = bytes.readUInt32BE(offset);
    const type = bytes.toString('latin1', offset + 4, offset + 8);
    const data = bytes.subarray(offset + 8, offset + 8 + length);
    if (data.length !== length) throw new Error(`块 ${type} 声明 ${length} 字节，文件只剩 ${data.length} 字节`);
    if (type === 'IHDR') {
      ihdr = {
        width: data.readUInt32BE(0),
        height: data.readUInt32BE(4),
        bitDepth: data[8],
        colorType: data[9],
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
  if (ihdr.bitDepth !== 8 || ihdr.colorType !== 6 || ihdr.interlace !== 0) {
    throw new Error(
      `只认 8 位 RGBA 非隔行，这张是 bitDepth=${ihdr.bitDepth} colorType=${ihdr.colorType} interlace=${ihdr.interlace}`,
    );
  }
  if (ihdr.width === 0 || ihdr.height === 0) throw new Error(`宽或高是 0（${ihdr.width}×${ihdr.height}）`);
  if (idat.length === 0) throw new Error('没有 IDAT');

  const raw = inflateSync(Buffer.concat(idat));
  const stride = ihdr.width * 4;
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
      const left = x >= 4 ? cur[x - 4] : 0;
      const up = prev ? prev[x] : 0;
      const upLeft = prev && x >= 4 ? prev[x - 4] : 0;
      cur[x] = (src[x] + unfilter(filter, left, up, upLeft)) & 0xff;
    }
  }
  if (pos !== raw.length) throw new Error(`IDAT 解出来的字节对不上：用了 ${pos}，实际 ${raw.length}`);

  return { width: ihdr.width, height: ihdr.height, pixels };
}

/** PNG 的五个滤波器。全都要实现：编码器换一个默认滤波器时，解码器不能跟着瞎。 */
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

/** CRC-32（PNG 每个块尾部）。为了让写出来的差异图是**真 PNG**，不是为了验别人。 */
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

/** 把 RGBA8 像素编成 PNG。`filter` 指定整张图用哪一种滤波器（自检拿它走遍五个分支）。 */
export function encodePng(width, height, rgba, filter = 0) {
  const stride = width * 4;
  const raw = Buffer.alloc(height * (stride + 1));
  for (let y = 0; y < height; y += 1) {
    raw[y * (stride + 1)] = filter;
    for (let x = 0; x < stride; x += 1) {
      const value = rgba[y * stride + x];
      const left = x >= 4 ? rgba[y * stride + x - 4] : 0;
      const up = y > 0 ? rgba[(y - 1) * stride + x] : 0;
      const upLeft = y > 0 && x >= 4 ? rgba[(y - 1) * stride + x - 4] : 0;
      raw[y * (stride + 1) + 1 + x] = (value - unfilter(filter, left, up, upLeft)) & 0xff;
    }
  }
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(width, 0);
  ihdr.writeUInt32BE(height, 4);
  ihdr[8] = 8; // bit depth
  ihdr[9] = 6; // color type：RGBA
  return Buffer.concat([
    PNG_SIGNATURE,
    pngChunk('IHDR', ihdr),
    pngChunk('IDAT', deflateSync(raw)),
    pngChunk('IEND', Buffer.alloc(0)),
  ]);
}

// ---------------------------------------------------------------------------
// 摘要（FNV-1a 64）——与仓库其它工具同一套参数，写进记录后可跨工具对账
// ---------------------------------------------------------------------------

const FNV_OFFSET = 0xcbf29ce484222325n;
const FNV_PRIME = 0x100000001b3n;

export function fnv1a64Update(hash, bytes) {
  let h = hash;
  for (let i = 0; i < bytes.length; i += 1) {
    h ^= BigInt(bytes[i]);
    h = (h * FNV_PRIME) & 0xffffffffffffffffn;
  }
  return h;
}

export function fnv1a64(bytes) {
  return fnv1a64Update(FNV_OFFSET, bytes).toString(16).padStart(16, '0');
}

// ---------------------------------------------------------------------------
// 指标（全部是纯函数：自检喂合成图，真跑喂从磁盘解出来的图，同一条代码路径）
// ---------------------------------------------------------------------------

export const SSIM_WINDOW = 11;
export const SSIM_SIGMA = 1.5;
const SSIM_K1 = 0.01;
const SSIM_K2 = 0.03;
const SSIM_RANGE = 255;
const SSIM_C1 = (SSIM_K1 * SSIM_RANGE) ** 2;
const SSIM_C2 = (SSIM_K2 * SSIM_RANGE) ** 2;

const SSIM_KERNEL = (() => {
  const half = (SSIM_WINDOW - 1) / 2;
  const kernel = new Float64Array(SSIM_WINDOW);
  let sum = 0;
  for (let i = 0; i < SSIM_WINDOW; i += 1) {
    kernel[i] = Math.exp(-((i - half) ** 2) / (2 * SSIM_SIGMA * SSIM_SIGMA));
    sum += kernel[i];
  }
  for (let i = 0; i < SSIM_WINDOW; i += 1) kernel[i] /= sum;
  return kernel;
})();

/** luma 平面：0.2126R + 0.7152G + 0.0722B，取 PNG 里存的 sRGB 字节（不是线性值）。 */
export function lumaPlane(image) {
  const { width, height, pixels } = image;
  const plane = new Float64Array(width * height);
  for (let i = 0; i < plane.length; i += 1) {
    const base = i * 4;
    plane[i] = 0.2126 * pixels[base] + 0.7152 * pixels[base + 1] + 0.0722 * pixels[base + 2];
  }
  return plane;
}

/** 11×11 高斯窗的可分离卷积；只取 valid 区域，输出尺寸 (w-10)×(h-10)。 */
function windowedSums(plane, width, height) {
  const outW = width - SSIM_WINDOW + 1;
  const outH = height - SSIM_WINDOW + 1;
  const tmp = new Float64Array(height * outW);
  for (let y = 0; y < height; y += 1) {
    const row = y * width;
    for (let x = 0; x < outW; x += 1) {
      let sum = 0;
      for (let k = 0; k < SSIM_WINDOW; k += 1) sum += SSIM_KERNEL[k] * plane[row + x + k];
      tmp[y * outW + x] = sum;
    }
  }
  const out = new Float64Array(outH * outW);
  for (let y = 0; y < outH; y += 1) {
    for (let x = 0; x < outW; x += 1) {
      let sum = 0;
      for (let k = 0; k < SSIM_WINDOW; k += 1) sum += SSIM_KERNEL[k] * tmp[(y + k) * outW + x];
      out[y * outW + x] = sum;
    }
  }
  return out;
}

/** 全图 SSIM（luma）。全等的两张图**恰好**是 1——公式的写法保证了这点，自检钉住。 */
export function ssimLuma(a, b) {
  assertSameSize(a, b);
  if (a.width < SSIM_WINDOW || a.height < SSIM_WINDOW) {
    throw new Error(`图像比 SSIM 窗口还小（${a.width}×${a.height} < ${SSIM_WINDOW}×${SSIM_WINDOW}），算不了`);
  }
  const x = lumaPlane(a);
  const y = lumaPlane(b);
  const xx = new Float64Array(x.length);
  const yy = new Float64Array(y.length);
  const xy = new Float64Array(x.length);
  for (let i = 0; i < x.length; i += 1) {
    xx[i] = x[i] * x[i];
    yy[i] = y[i] * y[i];
    xy[i] = x[i] * y[i];
  }
  const mx = windowedSums(x, a.width, a.height);
  const my = windowedSums(y, b.width, b.height);
  const mxx = windowedSums(xx, a.width, a.height);
  const myy = windowedSums(yy, b.width, b.height);
  const mxy = windowedSums(xy, a.width, a.height);
  let total = 0;
  for (let i = 0; i < mx.length; i += 1) {
    const muX = mx[i];
    const muY = my[i];
    const varX = mxx[i] - muX * muX;
    const varY = myy[i] - muY * muY;
    const cov = mxy[i] - muX * muY;
    const numerator = (2 * muX * muY + SSIM_C1) * (2 * cov + SSIM_C2);
    const denominator = (muX * muX + muY * muY + SSIM_C1) * (varX + varY + SSIM_C2);
    total += numerator / denominator;
  }
  return total / mx.length;
}

/** RGB 三通道的 PSNR；全等 → Infinity。 */
export function psnrRgb(a, b) {
  assertSameSize(a, b);
  const count = a.width * a.height * 3;
  let squared = 0;
  for (let p = 0; p < a.width * a.height; p += 1) {
    const base = p * 4;
    for (let c = 0; c < 3; c += 1) {
      const d = a.pixels[base + c] - b.pixels[base + c];
      squared += d * d;
    }
  }
  const mse = squared / count;
  return mse === 0 ? Infinity : 10 * Math.log10((255 * 255) / mse);
}

/**
 * 逐像素绝对差的汇总：最大绝对差（RGBA 四通道）、差异像素数（任一通道不同），
 * 以及**原始 |Δ| 的 RGBA 图**（有差异才建；放大与编码发生在落盘时）。
 */
export function pixelDiffSummary(a, b) {
  assertSameSize(a, b);
  const pixels = a.width * a.height;
  let maxAbsDiff = 0;
  let diffPixels = 0;
  let rgba = null;
  for (let p = 0; p < pixels; p += 1) {
    const base = p * 4;
    const d0 = Math.abs(a.pixels[base] - b.pixels[base]);
    const d1 = Math.abs(a.pixels[base + 1] - b.pixels[base + 1]);
    const d2 = Math.abs(a.pixels[base + 2] - b.pixels[base + 2]);
    const d3 = Math.abs(a.pixels[base + 3] - b.pixels[base + 3]);
    const worst = Math.max(d0, d1, d2, d3);
    if (worst === 0) continue;
    diffPixels += 1;
    if (worst > maxAbsDiff) maxAbsDiff = worst;
    if (rgba === null) rgba = Buffer.alloc(pixels * 4);
    rgba[base] = d0;
    rgba[base + 1] = d1;
    rgba[base + 2] = d2;
    rgba[base + 3] = d3;
  }
  return { maxAbsDiff, diffPixels, rgba };
}

function assertSameSize(a, b) {
  if (a.width !== b.width || a.height !== b.height) {
    throw new Error(`尺寸不同：${a.width}×${a.height} vs ${b.width}×${b.height}`);
  }
}

// ---------------------------------------------------------------------------
// 差异的**空间形状**（T2.5 归因用）
//
// 为什么"差多少"不够：归因要判的是"这是错位还是噪声"，而这两者在 |Δ| 上可以长得
// 一模一样——gradient 场景整幅图错开一个像素，落在 8 位量化台阶上也就差 1 LSB。
// 所以还要记差异的**位置与符号**：
//   · 整高的窄列（少而贯穿全高）像相位/错位；铺满全图像逐像素舍入
//   · 符号四象限是否对称：单向偏移会让某一侧占优
//   · |Δ| ≥ 2 有没有出现：出现就不是"最低位上的一步"
//
// 这个函数**只出事实、不出结论**。判定"可接受 / 不可接受"是文档（plan/wgsl-portable-subset.md）
// 的活：工具一旦自己下结论，换个场景就得改工具，而"改工具"比"改判断"更像在调数据。
//
// 口径（写清楚，免得两处各算一套）：
//   · per_channel / magnitude_at_least_2 / sign_quadrant 数的是**通道实例**，
//     不是像素——一个像素可能 R、G 两个通道都差，那要记两次。
//     恒等式：四个象限的 (plus+minus) 之和 == 四个通道的 (plus+minus) 之和。
//   · diff_pixels / full_height_columns / bbox 数的是**像素**。
//   · 有符号差的定义是 a − b（谁减谁要写死，否则"符号对称"这句话没有方向）。
// ---------------------------------------------------------------------------

const CHANNELS = ['R', 'G', 'B', 'A'];

export function pixelDiffShape(a, b) {
  assertSameSize(a, b);
  const { width, height } = a;
  const halfW = Math.floor(width / 2);
  const halfH = Math.floor(height / 2);

  const perChannel = Object.fromEntries(CHANNELS.map((c) => [c, { plus: 0, minus: 0 }]));
  const magnitudeAtLeast2 = Object.fromEntries(CHANNELS.map((c) => [c, 0]));
  const signQuadrant = Object.fromEntries(
    ['topLeft', 'topRight', 'bottomLeft', 'bottomRight'].map((q) => [q, { plus: 0, minus: 0 }]),
  );
  const deltaCounts = new Map(); // 有符号差 → 次数（零不进表）
  const colDiffs = new Uint32Array(width);
  const rowDiffs = new Uint32Array(height);
  let diffPixels = 0;
  let maxAbsDiff = 0;
  let minCol = width;
  let maxCol = -1;
  let minRow = height;
  let maxRow = -1;

  for (let p = 0; p < width * height; p += 1) {
    const base = p * 4;
    const col = p % width;
    const row = (p - col) / width;
    const quadrant = `${row < halfH ? 'top' : 'bottom'}${col < halfW ? 'Left' : 'Right'}`;
    let worst = 0;
    for (let i = 0; i < CHANNELS.length; i += 1) {
      const delta = a.pixels[base + i] - b.pixels[base + i];
      const magnitude = Math.abs(delta);
      if (magnitude === 0) continue;
      const name = CHANNELS[i];
      if (magnitude > worst) worst = magnitude;
      if (magnitude >= 2) magnitudeAtLeast2[name] += 1;
      if (delta > 0) {
        perChannel[name].plus += 1;
        signQuadrant[quadrant].plus += 1;
      } else {
        perChannel[name].minus += 1;
        signQuadrant[quadrant].minus += 1;
      }
      deltaCounts.set(delta, (deltaCounts.get(delta) ?? 0) + 1);
    }
    if (worst === 0) continue;
    diffPixels += 1;
    if (worst > maxAbsDiff) maxAbsDiff = worst;
    colDiffs[col] += 1;
    rowDiffs[row] += 1;
    if (col < minCol) minCol = col;
    if (col > maxCol) maxCol = col;
    if (row < minRow) minRow = row;
    if (row > maxRow) maxRow = row;
  }

  // 整高列 / 整宽行：这一列（行）的**每一行（列）**都有差异像素。
  const fullHeightColumns = [];
  for (let col = 0; col < width; col += 1) if (colDiffs[col] === height) fullHeightColumns.push(col);
  const fullWidthRows = [];
  for (let row = 0; row < height; row += 1) if (rowDiffs[row] === width) fullWidthRows.push(row);
  const fullHeightSet = new Set(fullHeightColumns);

  // 落在整高列里的差异像素数：全部等于 diff_pixels 就说明"差异没跑出这些列"。
  let diffPixelsInsideFullHeightColumns = 0;
  if (fullHeightSet.size > 0) {
    for (let p = 0; p < width * height; p += 1) {
      const base = p * 4;
      if (a.pixels[base] === b.pixels[base] && a.pixels[base + 1] === b.pixels[base + 1]
        && a.pixels[base + 2] === b.pixels[base + 2] && a.pixels[base + 3] === b.pixels[base + 3]) continue;
      if (fullHeightSet.has(p % width)) diffPixelsInsideFullHeightColumns += 1;
    }
  }

  return {
    width,
    height,
    diff_pixels: diffPixels,
    max_abs_diff: maxAbsDiff,
    signed_deltas: [...deltaCounts.keys()].sort((x, y) => x - y),
    magnitude_at_least_2: magnitudeAtLeast2,
    per_channel: perChannel,
    sign_quadrant: signQuadrant,
    full_height_columns: fullHeightColumns,
    full_width_rows: fullWidthRows,
    diff_cols_span: maxCol < 0 ? 0 : maxCol - minCol + 1,
    diff_rows_span: maxRow < 0 ? 0 : maxRow - minRow + 1,
    bbox: maxCol < 0 ? null : { min_col: minCol, max_col: maxCol, min_row: minRow, max_row: maxRow },
    diff_pixels_inside_full_height_columns: diffPixelsInsideFullHeightColumns,
  };
}

/** 把逐帧的形状汇总成逐场景。事实的搬运工，不做判断。 */
export function summarizeShape(frames) {
  const byScene = new Map();
  for (const frame of frames) {
    if (!byScene.has(frame.scene)) byScene.set(frame.scene, []);
    byScene.get(frame.scene).push(frame);
  }
  return [...byScene.keys()].sort().map((scene) => {
    const list = byScene.get(scene);
    const shapes = list.map((f) => f.shape);
    const unionColumns = new Set();
    for (const shape of shapes) for (const col of shape.full_height_columns) unionColumns.add(col);
    const firstColumns = JSON.stringify(shapes[0].full_height_columns);
    const sum = (pick) => shapes.reduce((total, shape) => total + pick(shape), 0);
    const sumChannel = (pick) => Object.fromEntries(
      CHANNELS.map((c) => [c, shapes.reduce((total, shape) => total + pick(shape, c), 0)]),
    );
    return {
      scene,
      frames: list.length,
      width: shapes[0].width,
      height: shapes[0].height,
      diff_pixels: sum((s) => s.diff_pixels),
      max_abs_diff: shapes.reduce((max, s) => Math.max(max, s.max_abs_diff), 0),
      signed_deltas: [...new Set(shapes.flatMap((s) => s.signed_deltas))].sort((x, y) => x - y),
      magnitude_at_least_2: sumChannel((s, c) => s.magnitude_at_least_2[c]),
      per_channel: Object.fromEntries(CHANNELS.map((c) => [
        c,
        {
          plus: sum((s) => s.per_channel[c].plus),
          minus: sum((s) => s.per_channel[c].minus),
        },
      ])),
      sign_quadrant: Object.fromEntries(['topLeft', 'topRight', 'bottomLeft', 'bottomRight'].map((q) => [
        q,
        {
          plus: sum((s) => s.sign_quadrant[q].plus),
          minus: sum((s) => s.sign_quadrant[q].minus),
        },
      ])),
      // 并集与逐帧**不是一回事**（并集是"至少一帧里整高的列"，会随帧数变多）。
      // 两个都记，免得引用的人各挑一个。
      full_height_columns_union: [...unionColumns].sort((x, y) => x - y),
      full_height_columns_per_frame: shapes.map((s) => s.full_height_columns.length),
      full_height_columns_identical_across_frames: shapes.every((s) => JSON.stringify(s.full_height_columns) === firstColumns),
      full_width_rows_union: [...new Set(shapes.flatMap((s) => s.full_width_rows))].sort((x, y) => x - y),
      all_diff_pixels_inside_full_height_columns: shapes.every((s) => s.diff_pixels_inside_full_height_columns === s.diff_pixels),
      bbox_union: (() => {
        const boxes = shapes.map((s) => s.bbox).filter(Boolean);
        if (boxes.length === 0) return null;
        return {
          min_col: Math.min(...boxes.map((box) => box.min_col)),
          max_col: Math.max(...boxes.map((box) => box.max_col)),
          min_row: Math.min(...boxes.map((box) => box.min_row)),
          max_row: Math.max(...boxes.map((box) => box.max_row)),
        };
      })(),
      per_frame: list.map((frame) => ({ file: frame.file, frame: frame.frame, ...frame.shape })),
    };
  });
}

/** shape.json：与 verdict.json 绑同一批输入（两侧帧集摘要必须一致才算同一批）。 */
export function buildShapeJson({ analysis, thresholdsInfo, shown }) {
  return {
    schema: 1,
    kind: 'framediff-shape',
    tool: 'scripts/dhampir-framediff.mjs',
    note: '只记事实，不写结论；可接受/不可接受的判断在 plan/wgsl-portable-subset.md',
    signed_delta_definition: 'a - b',
    inputs: {
      a: {
        label: analysis.labelA, dir: shown.a, frames: analysis.frames.length,
        bytes_total: analysis.bytesTotalA, set_digest: analysis.setDigestA,
      },
      b: {
        label: analysis.labelB, dir: shown.b, frames: analysis.frames.length,
        bytes_total: analysis.bytesTotalB, set_digest: analysis.setDigestB,
      },
    },
    thresholds: { path: thresholdsInfo.path, digest: thresholdsInfo.digest },
    scenes: summarizeShape(analysis.frames),
  };
}

// ---------------------------------------------------------------------------
// 配对：名字契约、集合相等、帧号连续（全是纯函数，喂名字清单就能自检）
// ---------------------------------------------------------------------------

const FRAME_NAME = /^([A-Za-z_][A-Za-z0-9_]*)-f(\d+)\.png$/;

export function pairNames(namesA, namesB, labels = { a: 'A', b: 'B' }) {
  const scan = (names) => {
    const seen = new Map();
    const duplicates = [];
    const bad = [];
    for (const name of names) {
      const match = FRAME_NAME.exec(name);
      if (!match) {
        bad.push(name);
        continue;
      }
      const scene = match[1];
      const frame = Number.parseInt(match[2], 10);
      const key = `${scene}\u0000${frame}`;
      if (seen.has(key)) duplicates.push({ scene, frame, names: [seen.get(key), name] });
      else seen.set(key, name);
    }
    return { seen, duplicates, bad };
  };
  const sideA = scan(namesA);
  const sideB = scan(namesB);
  const namesSetA = new Set(namesA);
  const namesSetB = new Set(namesB);
  const onlyA = namesA.filter((name) => !namesSetB.has(name));
  const onlyB = namesB.filter((name) => !namesSetA.has(name));
  const pairs = [];
  for (const [key, name] of sideA.seen) {
    const [scene, frame] = key.split('\u0000');
    pairs.push({ scene, frame: Number.parseInt(frame, 10), name });
  }
  // 场景按码元序（不用 localeCompare：它随环境变）；帧号按数值。
  pairs.sort((p, q) => (p.scene < q.scene ? -1 : p.scene > q.scene ? 1 : p.frame - q.frame));
  return {
    labels,
    emptyA: namesA.length === 0,
    emptyB: namesB.length === 0,
    onlyA,
    onlyB,
    badNames: [
      ...sideA.bad.map((name) => ({ side: 'a', name })),
      ...sideB.bad.map((name) => ({ side: 'b', name })),
    ],
    duplicates: [
      ...sideA.duplicates.map((d) => ({ side: 'a', ...d })),
      ...sideB.duplicates.map((d) => ({ side: 'b', ...d })),
    ],
    pairs,
  };
}

/** 把清单里的不合规一次说清（每条都点名，不做"取交集继续跑"这种静默调解）。 */
export function assertPairing(pairing, labels = { a: 'A', b: 'B' }) {
  const show = (list) => {
    const head = list.slice(0, 6).join('、');
    return list.length > 6 ? `${head} 等 ${list.length} 个` : head;
  };
  if (pairing.emptyA || pairing.emptyB) {
    const sides = [pairing.emptyA ? labels.a : null, pairing.emptyB ? labels.b : null].filter(Boolean).join(' 与 ');
    throw new Error(`${sides} 下连一张 PNG 都没有——空集不准当"全过"`);
  }
  if (pairing.badNames.length > 0) {
    throw new Error(
      `不认得的文件名（契约是 <场景>-f<帧号>.png）：${show(pairing.badNames.map((b) => `${b.name}（侧 ${b.side}）`))}`,
    );
  }
  if (pairing.duplicates.length > 0) {
    const first = pairing.duplicates[0];
    throw new Error(
      `同一个场景里两个文件解析出同一个帧号：${first.scene} f${first.frame}（${first.names.join('、')}，侧 ${first.side}）`,
    );
  }
  if (pairing.onlyA.length > 0) {
    throw new Error(`${labels.a} 有而 ${labels.b} 没有：${show(pairing.onlyA)}`);
  }
  if (pairing.onlyB.length > 0) {
    throw new Error(`${labels.b} 有而 ${labels.a} 没有：${show(pairing.onlyB)}`);
  }
  const byScene = new Map();
  for (const pair of pairing.pairs) {
    if (!byScene.has(pair.scene)) byScene.set(pair.scene, []);
    byScene.get(pair.scene).push(pair.frame);
  }
  for (const [scene, frames] of byScene) {
    const missing = [];
    for (let i = 0; i < frames.length; i += 1) if (!frames.includes(i)) missing.push(i);
    if (missing.length > 0) {
      const head = missing.slice(0, 8).join(', ');
      throw new Error(
        `场景 ${scene} 的帧号不从 0 起连续：共 ${frames.length} 帧，缺 ${head}${missing.length > 8 ? ` 等 ${missing.length} 个` : ''}——被截断的一批不能伪装成一批`,
      );
    }
  }
}

/** 目录里的 PNG（其余文件不参与比对，但要列出来）。 */
export function listPngs(dir) {
  const pngs = [];
  const ignored = [];
  const entries = readdirSync(dir, { withFileTypes: true }).sort((x, y) => (x.name < y.name ? -1 : x.name > y.name ? 1 : 0));
  for (const entry of entries) {
    if (entry.isFile()) {
      if (entry.name.toLowerCase().endsWith('.png')) pngs.push(entry.name);
      else ignored.push(entry.name);
    } else if (entry.isDirectory()) {
      ignored.push(`${entry.name}/`);
    } else {
      ignored.push(`${entry.name}（不是普通文件）`);
    }
  }
  return { pngs, ignored };
}

// ---------------------------------------------------------------------------
// 阈值文件（TOML 子集）：未知键与未知节一律红
//
// 拼错的键如果被静默忽略，这道闸就等于没设——这正是"守卫只认自己理解的东西"
// 该有的样子。解析器只吃 键 = 数值 与两种节，别的一律报错。
// ---------------------------------------------------------------------------

const THRESHOLD_KEYS = new Set(['mean_ssim_min', 'min_ssim_min', 'max_abs_diff_max', 'psnr_db_min']);

export function parseThresholds(text) {
  const defaults = {};
  const scenarios = new Map();
  let current = null;
  let currentName = '(还没有节)';
  const lines = text.split('\n');
  for (let i = 0; i < lines.length; i += 1) {
    const lineNo = i + 1;
    const line = lines[i].split('#')[0].trim();
    if (line === '') continue;
    const section = /^\[([^\]]*)\]$/.exec(line);
    if (section) {
      const name = section[1].trim();
      if (name === 'default') {
        current = defaults;
        currentName = '[default]';
      } else if (name.startsWith('scenario.')) {
        const scene = name.slice('scenario.'.length).trim();
        if (scene === '') throw new Error(`第 ${lineNo} 行：[scenario.] 后面缺场景名`);
        current = scenarios.get(scene) ?? {};
        scenarios.set(scene, current);
        currentName = `[scenario.${scene}]`;
      } else {
        throw new Error(`第 ${lineNo} 行：不认识的节 [${name}]（只认 [default] 与 [scenario.<名字>]）`);
      }
      continue;
    }
    const kv = /^([A-Za-z0-9_]+)\s*=\s*(.*)$/.exec(line);
    if (!kv) throw new Error(`第 ${lineNo} 行看不懂：${JSON.stringify(lines[i])}（只认 键 = 值）`);
    const key = kv[1];
    const raw = kv[2].trim();
    if (current === null) throw new Error(`第 ${lineNo} 行：键 ${key} 写在任何节之前`);
    if (!THRESHOLD_KEYS.has(key)) {
      throw new Error(`第 ${lineNo} 行：不认识的键 ${key}——拼错的键若被静默忽略，这道闸等于没设`);
    }
    if (raw === '') throw new Error(`第 ${lineNo} 行：键 ${key} 没给值`);
    const value = Number(raw);
    if (!Number.isFinite(value)) throw new Error(`第 ${lineNo} 行：${key} 的值 ${JSON.stringify(raw)} 不是数`);
    if (key === 'max_abs_diff_max' && (!Number.isInteger(value) || value < 0 || value > 255)) {
      throw new Error(`第 ${lineNo} 行：max_abs_diff_max 要是 0..255 的整数，收到 ${raw}`);
    }
    if ((key === 'mean_ssim_min' || key === 'min_ssim_min') && (value < 0 || value > 1)) {
      throw new Error(`第 ${lineNo} 行：${key} 要在 0..1，收到 ${raw}`);
    }
    if (key === 'psnr_db_min' && value < 0) throw new Error(`第 ${lineNo} 行：psnr_db_min 不能是负数`);
    if (key in current) throw new Error(`第 ${lineNo} 行：${currentName} 里 ${key} 写了两遍`);
    current[key] = value;
  }
  return { defaults, scenarios };
}

/**
 * 一个场景的档位 = [default] 打底、[scenario.<名字>] 覆盖。
 * mean_ssim_min 与 min_ssim_min 是**必须**能解析出来的两条（plan 的基线）；
 * 解析不出来就红——没设闸门的场景不许混进"全过"。
 */
export function resolveThresholds(table, scene) {
  const merged = { ...table.defaults, ...(table.scenarios.get(scene) ?? {}) };
  if (merged.mean_ssim_min === undefined || merged.min_ssim_min === undefined) {
    throw new Error(
      `场景 ${scene} 没能解析出档位（缺 mean_ssim_min / min_ssim_min）——补 [default] 或 [scenario.${scene}]`,
    );
  }
  return merged;
}

// ---------------------------------------------------------------------------
// 比对（读盘、解码、算三个指标）
// ---------------------------------------------------------------------------

export function analyzePair({ dirA, dirB, labelA, labelB, shape = false }) {
  const listingA = listPngs(dirA);
  const listingB = listPngs(dirB);
  const labels = { a: labelA, b: labelB };
  const pairing = pairNames(listingA.pngs, listingB.pngs, labels);
  assertPairing(pairing, labels);

  const frames = [];
  const diffRgbaByName = new Map();
  let bytesTotalA = 0;
  let bytesTotalB = 0;
  // 集合摘要：按 summary 的行序（场景、帧号）把 文件名 + 0x00 + 文件字节 串起来。
  // 记录守卫可以对着归档目录重算它，证明"这一轮真的用的是这批文件"。
  let digestA = FNV_OFFSET;
  let digestB = FNV_OFFSET;

  for (const pair of pairing.pairs) {
    const fileA = readFileSync(join(dirA, pair.name));
    const fileB = readFileSync(join(dirB, pair.name));
    bytesTotalA += fileA.length;
    bytesTotalB += fileB.length;
    const marker = Buffer.from([0]);
    digestA = fnv1a64Update(digestA, Buffer.from(pair.name, 'utf8'));
    digestA = fnv1a64Update(digestA, marker);
    digestA = fnv1a64Update(digestA, fileA);
    digestB = fnv1a64Update(digestB, Buffer.from(pair.name, 'utf8'));
    digestB = fnv1a64Update(digestB, marker);
    digestB = fnv1a64Update(digestB, fileB);

    const imageA = decodeWith(labelA, pair.name, fileA);
    const imageB = decodeWith(labelB, pair.name, fileB);
    if (imageA.width !== imageB.width || imageA.height !== imageB.height) {
      throw new Error(
        `两侧尺寸不同：${pair.name}：${labelA} ${imageA.width}×${imageA.height} vs ${labelB} ${imageB.width}×${imageB.height}`,
      );
    }
    const ssim = ssimLuma(imageA, imageB);
    const psnr = psnrRgb(imageA, imageB);
    const diff = pixelDiffSummary(imageA, imageB);
    if (diff.rgba !== null) {
      diffRgbaByName.set(pair.name, { width: imageA.width, height: imageA.height, rgba: diff.rgba });
    }
    let shapeOfFrame = null;
    if (shape) {
      shapeOfFrame = pixelDiffShape(imageA, imageB);
      // 同一件事的两份实现必须说同一句话。`diff_pixels` 与 `max_abs_diff` 在这里
      // 被算了两次（一份为了差异图、一份为了形状），数字对不上就说明其中一份错了——
      // 与其挑一个信，不如当场停住。
      if (shapeOfFrame.diff_pixels !== diff.diffPixels || shapeOfFrame.max_abs_diff !== diff.maxAbsDiff) {
        throw new Error(
          `${pair.name}：两份实现算出的差异对不上——`
          + `pixelDiffSummary 说 ${diff.diffPixels} 像素/max|Δ| ${diff.maxAbsDiff}，`
          + `pixelDiffShape 说 ${shapeOfFrame.diff_pixels} 像素/max|Δ| ${shapeOfFrame.max_abs_diff}`,
        );
      }
    }
    frames.push({
      scene: pair.scene,
      frame: pair.frame,
      file: pair.name,
      width: imageA.width,
      height: imageA.height,
      ssim_luma: ssim,
      psnr_rgb_db: psnr,
      max_abs_diff: diff.maxAbsDiff,
      diff_pixels: diff.diffPixels,
      shape: shapeOfFrame,
    });
  }

  return {
    labelA,
    labelB,
    dirA,
    dirB,
    ignoredA: listingA.ignored,
    ignoredB: listingB.ignored,
    bytesTotalA,
    bytesTotalB,
    setDigestA: digestA.toString(16).padStart(16, '0'),
    setDigestB: digestB.toString(16).padStart(16, '0'),
    frames,
    scenes: summarizeScenes(frames),
    diffRgbaByName,
  };
}

function decodeWith(label, name, bytes) {
  try {
    return decodePng(bytes);
  } catch (error) {
    throw new Error(`${label} 的 ${name} 解不开：${error.message}`);
  }
}

/** 逐场景汇总（不含判定，判定在 judge 里对着档位做）。 */
export function summarizeScenes(frames) {
  const byScene = new Map();
  for (const frame of frames) {
    if (!byScene.has(frame.scene)) byScene.set(frame.scene, []);
    byScene.get(frame.scene).push(frame);
  }
  return [...byScene.keys()].sort().map((scene) => {
    const list = byScene.get(scene);
    const mean = list.reduce((sum, f) => sum + f.ssim_luma, 0) / list.length;
    const minSsim = list.reduce((min, f) => Math.min(min, f.ssim_luma), Infinity);
    const minPsnr = list.reduce((min, f) => Math.min(min, f.psnr_rgb_db), Infinity);
    const maxAbsDiff = list.reduce((max, f) => Math.max(max, f.max_abs_diff), 0);
    const diffPixels = list.reduce((sum, f) => sum + f.diff_pixels, 0);
    const worst = list
      .slice()
      .sort(
        (p, q) =>
          p.ssim_luma - q.ssim_luma ||
          q.max_abs_diff - p.max_abs_diff ||
          (p.file < q.file ? -1 : p.file > q.file ? 1 : 0),
      )
      .slice(0, 3)
      .map(({ file, ssim_luma, max_abs_diff }) => ({ file, ssim_luma, max_abs_diff }));
    return {
      scene,
      frames: list.length,
      mean_ssim_luma: mean,
      min_ssim_luma: minSsim,
      min_psnr_rgb_db: minPsnr,
      max_abs_diff: maxAbsDiff,
      diff_pixels: diffPixels,
      worst_frames: worst,
    };
  });
}

// ---------------------------------------------------------------------------
// 判定：每条闸单独判，失败的**点名那一条、那几个数**
// ---------------------------------------------------------------------------

export function judge(analysis, table) {
  const scenes = analysis.scenes.map((scene) => {
    const limits = resolveThresholds(table, scene.scene);
    const failures = [];
    if (scene.mean_ssim_luma < limits.mean_ssim_min) {
      failures.push(`mean_ssim_luma ${num(scene.mean_ssim_luma)} < ${num(limits.mean_ssim_min)}`);
    }
    if (scene.min_ssim_luma < limits.min_ssim_min) {
      failures.push(`min_ssim_luma ${num(scene.min_ssim_luma)} < ${num(limits.min_ssim_min)}`);
    }
    if (limits.max_abs_diff_max !== undefined && scene.max_abs_diff > limits.max_abs_diff_max) {
      failures.push(`max_abs_diff ${scene.max_abs_diff} > ${limits.max_abs_diff_max}`);
    }
    if (limits.psnr_db_min !== undefined && scene.min_psnr_rgb_db < limits.psnr_db_min) {
      failures.push(`min_psnr_rgb_db ${num(scene.min_psnr_rgb_db)} < ${num(limits.psnr_db_min)}`);
    }
    return {
      ...scene,
      limits,
      verdict: failures.length === 0 ? 'pass' : 'fail',
      failures,
    };
  });
  return {
    verdict: scenes.every((scene) => scene.verdict === 'pass') ? 'pass' : 'fail',
    scenes,
  };
}

// ---------------------------------------------------------------------------
// 输出：summary.csv / verdict.json / report.txt / diff/*.png
// ---------------------------------------------------------------------------

function num(value) {
  return Number.isFinite(value) ? String(value) : 'inf';
}

function csvField(value) {
  const text = String(value);
  return /[",\n]/.test(text) ? `"${text.replaceAll('"', '""')}"` : text;
}

export function renderSummaryCsv(analysis) {
  const lines = ['scene,frame,file,width,height,ssim_luma,psnr_rgb_db,max_abs_diff,diff_pixels,diff_image'];
  for (const frame of analysis.frames) {
    lines.push(
      [
        frame.scene,
        frame.frame,
        frame.file,
        frame.width,
        frame.height,
        num(frame.ssim_luma),
        num(frame.psnr_rgb_db),
        frame.max_abs_diff,
        frame.diff_pixels,
        analysis.diffRgbaByName.has(frame.file) ? `diff/${frame.file}` : '',
      ]
        .map(csvField)
        .join(','),
    );
  }
  return `${lines.join('\n')}\n`;
}

/** |Δ| × amp、clamp 到 255，alpha 一律 255（差异图只回答"哪动了、动了多少"）。 */
export function ampRgb(rgba, amp) {
  const out = Buffer.alloc(rgba.length);
  for (let p = 0; p < rgba.length; p += 4) {
    out[p] = Math.min(255, rgba[p] * amp);
    out[p + 1] = Math.min(255, rgba[p + 1] * amp);
    out[p + 2] = Math.min(255, rgba[p + 2] * amp);
    out[p + 3] = 255;
  }
  return out;
}

export function buildVerdictJson({ analysis, judged, amp, thresholdsInfo, shown }) {
  const diffImages = [...analysis.diffRgbaByName.keys()].sort();
  return {
    schema: 1,
    kind: 'framediff',
    tool: 'scripts/dhampir-framediff.mjs',
    amp,
    window: { space: 'luma', size: SSIM_WINDOW, sigma: SSIM_SIGMA, k1: SSIM_K1, k2: SSIM_K2, range: SSIM_RANGE },
    inputs: {
      a: {
        label: analysis.labelA,
        dir: shown.a,
        frames: analysis.frames.length,
        bytes_total: analysis.bytesTotalA,
        set_digest: analysis.setDigestA,
        ignored: analysis.ignoredA,
      },
      b: {
        label: analysis.labelB,
        dir: shown.b,
        frames: analysis.frames.length,
        bytes_total: analysis.bytesTotalB,
        set_digest: analysis.setDigestB,
        ignored: analysis.ignoredB,
      },
    },
    thresholds: thresholdsInfo,
    scenes: judged.scenes.map((scene) => ({
      ...scene,
      min_psnr_rgb_db: Number.isFinite(scene.min_psnr_rgb_db) ? scene.min_psnr_rgb_db : null,
    })),
    totals: {
      scenes: judged.scenes.length,
      frames: analysis.frames.length,
      diff_pixels: analysis.frames.reduce((sum, f) => sum + f.diff_pixels, 0),
      diff_images: diffImages.length,
    },
    verdict: judged.verdict,
    exit_code: judged.verdict === 'pass' ? 0 : 1,
  };
}

export function writeOutputs({ outDir, analysis, judged, amp, thresholdsInfo, shown, labels }) {
  mkdirSync(outDir, { recursive: true });
  writeFileSync(join(outDir, 'summary.csv'), renderSummaryCsv(analysis));
  const verdictText = `${JSON.stringify(buildVerdictJson({ analysis, judged, amp, thresholdsInfo, shown }), null, 2)}\n`;
  writeFileSync(join(outDir, 'verdict.json'), verdictText);
  const diffNames = [...analysis.diffRgbaByName.keys()].sort();
  const diffImages = [];
  if (diffNames.length > 0) {
    const diffDir = join(outDir, 'diff');
    mkdirSync(diffDir, { recursive: true });
    for (const name of diffNames) {
      const { width, height, rgba } = analysis.diffRgbaByName.get(name);
      writeFileSync(join(diffDir, name), encodePng(width, height, ampRgb(rgba, amp)));
      diffImages.push(`diff/${name}`);
    }
  }
  const written = {
    summaryPath: join(outDir, 'summary.csv'),
    verdictPath: join(outDir, 'verdict.json'),
    reportPath: join(outDir, 'report.txt'),
    diffImages,
  };
  // 报告最后一个写：它要说出另外三件落在哪儿（报告自己也在其中，这不矛盾——
  // 位置在写之前就定了）。返回的四个路径全部**已经存在**，调用方不必赌。
  written.reportText = buildReport({ analysis, judged, labels, shown, written, thresholdsInfo });
  writeFileSync(written.reportPath, written.reportText);
  return written;
}

// ---------------------------------------------------------------------------
// 主流程
// ---------------------------------------------------------------------------

/**
 * 报告文本。既打到屏幕、也在给了 `--out` 时落成 `report.txt`——
 * 记录要的是**文件**而不是"当时屏幕上打过什么"。两侧是同一份文本（一个函数出），
 * 不许各写一遍：那样迟早会漂成两个说法。
 */
function buildReport({ analysis, judged, labels, shown, written, thresholdsInfo }) {
  const lines = [];
  const ignoredNote = (ignored) => (ignored.length === 0 ? '' : `，另有 ${ignored.length} 个文件未参与比对：${ignored.slice(0, 4).join('、')}${ignored.length > 4 ? '…' : ''}`);
  lines.push(`  档位：${thresholdsInfo.path}（digest ${thresholdsInfo.digest}）`);
  lines.push(`  a = ${labels.a}：${shown.a}（${analysis.frames.length} 张 PNG，${analysis.bytesTotalA} 字节${ignoredNote(analysis.ignoredA)}）`);
  lines.push(`  b = ${labels.b}：${shown.b}（${analysis.frames.length} 张 PNG，${analysis.bytesTotalB} 字节${ignoredNote(analysis.ignoredB)}）`);
  for (const scene of judged.scenes) {
    lines.push(
      `  ${scene.verdict === 'pass' ? '✓' : '✗'} ${scene.scene}：${scene.frames} 帧，mean SSIM ${num(scene.mean_ssim_luma)}，` +
        `min SSIM ${num(scene.min_ssim_luma)}，min PSNR ${num(scene.min_psnr_rgb_db)}，` +
        `max|Δ| ${scene.max_abs_diff}，差异像素 ${scene.diff_pixels}`,
    );
    for (const failure of scene.failures) lines.push(`      ${failure}`);
  }
  if (written !== null) {
    const extra = written.diffImages.length > 0 ? `，另 diff/ 下 ${written.diffImages.length} 张放大差异图` : '';
    lines.push(
      `  记录 → ${relToRepo(written.summaryPath)}、${relToRepo(written.verdictPath)}、${relToRepo(written.reportPath)}${extra}`,
    );
  } else {
    lines.push('  （没给 --out：只算不落盘）');
  }
  if (judged.verdict === 'pass') {
    lines.push(`\n✓ ${judged.scenes.length} 个场景、${analysis.frames.length} 帧全部达标（退出码 0）`);
  } else {
    lines.push(`\n✗ 有场景不达标（退出码 1）——不达标项与档位都在上面的失败行里`);
  }
  return `${lines.join('\n')}\n`;
}

function main() {
  const args = parseArgs(process.argv.slice(2));
  if (args.help) {
    console.log(USAGE);
    return 0;
  }
  if (args.error) {
    console.error(`参数错误：${args.error}\n${USAGE}`);
    return 2;
  }
  if (args.selfTest) return selfTest();
  if (args.a === null || args.b === null) {
    console.error(`--a 与 --b 都得给。\n${USAGE}`);
    return 2;
  }
  if (args.shape && args.out === null) {
    // 形状证据要落成文件才算证据。只印在屏幕上，下一个人复核时就要重新跑一遍，
    // 而"重新跑"与"当时那一批"是不是同一批，又得从头证一遍。
    console.error(`--shape 要把形状证据写成 <out>/shape.json，所以得同时给 --out。\n${USAGE}`);
    return 2;
  }

  const dirA = resolve(REPO_ROOT, args.a);
  const dirB = resolve(REPO_ROOT, args.b);
  for (const [flag, dir] of [
    ['--a', dirA],
    ['--b', dirB],
  ]) {
    if (!existsSync(dir) || !statSync(dir).isDirectory()) {
      console.error(`✗ ${flag} 不是目录：${dir}`);
      return 2;
    }
  }

  const thresholdsPath = resolve(REPO_ROOT, args.thresholds);
  let thresholdsText = '';
  try {
    thresholdsText = readFileSync(thresholdsPath, 'utf8');
  } catch (error) {
    console.error(`✗ 阈值文件读不到：${thresholdsPath}（${error.message}）`);
    return 2;
  }
  let table = null;
  try {
    table = parseThresholds(thresholdsText);
  } catch (error) {
    console.error(`✗ 阈值文件不成立：${error.message}`);
    return 2;
  }

  let analysis = null;
  try {
    analysis = analyzePair({ dirA, dirB, labelA: args.labelA, labelB: args.labelB, shape: args.shape });
  } catch (error) {
    console.error(`✗ 比对没法开始：${error.message}`);
    return 2;
  }
  let judged = null;
  try {
    judged = judge(analysis, table);
  } catch (error) {
    console.error(`✗ 档位没配好：${error.message}`);
    return 2;
  }

  const thresholdsInfo = {
    path: args.thresholds,
    digest: fnv1a64(Buffer.from(thresholdsText, 'utf8')),
    resolved: Object.fromEntries(judged.scenes.map((scene) => [scene.scene, scene.limits])),
  };

  let written = null;
  if (args.out !== null) {
    try {
      written = writeOutputs({
        outDir: resolve(REPO_ROOT, args.out),
        analysis,
        judged,
        amp: args.amp,
        thresholdsInfo,
        shown: { a: args.a, b: args.b },
        labels: { a: args.labelA, b: args.labelB },
      });
    } catch (error) {
      console.error(`✗ 落盘失败：${error.message}`);
      return 2;
    }
  }

  // 屏幕上的报告与 report.txt 是**同一份文本**：给了 --out 就直接印文件里那份，
  // 而不是再拼一遍——两份各自拼迟早会漂成两个说法。
  const reportText = written === null
    ? buildReport({ analysis, judged, labels: { a: args.labelA, b: args.labelB }, shown: { a: args.a, b: args.b }, written: null, thresholdsInfo })
    : written.reportText;
  process.stdout.write(reportText);

  if (args.shape) {
    // 刻意**不动**上面那份文本：形状是另加的一份证据，不是报告的第二个版本。
    // 报告与 report.txt 仍然逐字节相同（记录里的 digest 因此不受 --shape 影响）。
    const shapePath = join(resolve(REPO_ROOT, args.out), 'shape.json');
    try {
      writeFileSync(shapePath, `${JSON.stringify(buildShapeJson({ analysis, thresholdsInfo, shown: { a: args.a, b: args.b } }), null, 2)}\n`, 'utf8');
    } catch (error) {
      console.error(`✗ 形状证据落盘失败：${error.message}`);
      return 2;
    }
    process.stderr.write(`（形状证据）→ ${relToRepo(shapePath)}\n`);
  }
  return judged.verdict === 'pass' ? 0 : 1;
}

// ---------------------------------------------------------------------------
// 自检（真跑走哪条路，自检就走哪条路；反向用例必须断言"因为那一条红"）
// ---------------------------------------------------------------------------

/** 确定性合成图（自检专用）：LCG 造像素，不用 Math.random。 */
function synthRgba(width, height, seed) {
  const pixels = Buffer.alloc(width * height * 4);
  let state = seed >>> 0;
  for (let p = 0; p < width * height; p += 1) {
    state = (state * 1664525 + 1013904223) >>> 0;
    pixels[p * 4] = state & 0xff;
    pixels[p * 4 + 1] = (state >>> 8) & 0xff;
    pixels[p * 4 + 2] = (state >>> 16) & 0xff;
    pixels[p * 4 + 3] = 255;
  }
  return pixels;
}

function selfTest() {
  const cases = [];
  const check = (name, ok, detail = '') => cases.push({ name, ok: Boolean(ok), detail: String(detail) });
  const throwsLike = (fn, needle) => {
    try {
      fn();
      return { threw: false };
    } catch (error) {
      const message = String(error.message);
      return { threw: true, message, has: message.includes(needle) };
    }
  };
  // 落盘件一律先确认"在"再读内容：文件缺了就 `readFileSync` 抛出去，
  // 会把整个自检崩掉——那样**归因就没了**（反向探针真撞过这一下：
  // 抠掉 report.txt 的写入，屏幕上出现的是一段栈，而不是"哪条用例红了"）。
  const readIfThere = (path) => (existsSync(path) ? readFileSync(path, 'utf8') : null);

  // -- parseArgs
  check('--a 吃掉自己的值', parseArgs(['--a', 'x']).a === 'x');
  check('--a 缺值要报错', typeof parseArgs(['--a']).error === 'string');
  check('--amp 只收 1..255 的整数（0 红）', typeof parseArgs(['--amp', '0']).error === 'string');
  check('--amp 只收 1..255 的整数（256 红）', typeof parseArgs(['--amp', '256']).error === 'string');
  check('--amp 非数字红', typeof parseArgs(['--amp', 'x']).error === 'string');
  check('默认 amp 是 16', parseArgs([]).amp === 16);
  check('不认识的参数要报错', typeof parseArgs(['--nope']).error === 'string');
  // --shape 默认关：默认路径下的记录不该因为装了新版工具就多出一个文件。
  check('--shape 默认关', parseArgs(['--a', 'x', '--b', 'y']).shape === false);
  check('--shape 给了就是开', parseArgs(['--a', 'x', '--b', 'y', '--shape']).shape === true);

  // -- 报告里怎么说落盘位置（仓库相对，好让别的机器上重跑的报告也是同一份）
  check(
    '仓库内的落盘位置写成仓库相对路径（正斜杠）',
    relToRepo(join(REPO_ROOT, 'records', 'm2', 'framediff', 'summary.csv')) === 'records/m2/framediff/summary.csv',
  );
  check('仓库外的落盘位置保持原样', relToRepo(join(REPO_ROOT, '..', 'elsewhere.txt')) === join(REPO_ROOT, '..', 'elsewhere.txt'));

  // -- PNG 编解码
  const sample = synthRgba(13, 12, 7);
  for (let filter = 0; filter <= 4; filter += 1) {
    const back = decodePng(encodePng(13, 12, sample, filter));
    check(`滤波器 ${filter} 编码后能原样解回来`, back.width === 13 && back.height === 12 && back.pixels.equals(sample));
  }
  const good = encodePng(13, 12, sample, 0);
  check('解码器拒绝坏签名', throwsLike(() => decodePng(Buffer.from('not a png at all')), '不是 PNG 签名').has);
  const wrongColor = Buffer.from(good);
  wrongColor[25] = 2;
  check('解码器拒绝非 RGBA', throwsLike(() => decodePng(wrongColor), '只认 8 位 RGBA').has);
  const wrongDepth = Buffer.from(good);
  wrongDepth[24] = 16;
  check('解码器拒绝非 8 位', throwsLike(() => decodePng(wrongDepth), '只认 8 位 RGBA').has);
  const interlaced = Buffer.from(good);
  interlaced[28] = 1;
  check('解码器拒绝隔行', throwsLike(() => decodePng(interlaced), '只认 8 位 RGBA').has);
  const craftPng = (width, height, raw) => {
    const ihdr = Buffer.alloc(13);
    ihdr.writeUInt32BE(width, 0);
    ihdr.writeUInt32BE(height, 4);
    ihdr[8] = 8;
    ihdr[9] = 6;
    return Buffer.concat([
      PNG_SIGNATURE,
      pngChunk('IHDR', ihdr),
      pngChunk('IDAT', deflateSync(raw)),
      pngChunk('IEND', Buffer.alloc(0)),
    ]);
  };
  // 缺口要给成"整行数"才走"滤波器字节就没了"这条路：12 行图只给 11 行份，
  // 第 11 行连滤波器字节都读不到。给成非整行数会先撞上"某一行字节不够"那一条。
  const shortRows = Buffer.alloc(11 * (13 * 4 + 1));
  check(
    '解码器拒绝"行没给全"（并点名第几行）',
    throwsLike(() => decodePng(craftPng(13, 12, shortRows)), '第 11 行的滤波器字节就没了').has,
  );
  const shortOne = Buffer.alloc(13 * 4 + 1 + 10);
  check('解码器拒绝"某一行字节不够"', throwsLike(() => decodePng(craftPng(13, 2, shortOne)), '只剩').has);
  const extra = Buffer.alloc(12 * (13 * 4 + 1) + 8);
  check('解码器拒绝"多出来的字节"', throwsLike(() => decodePng(craftPng(13, 12, extra)), '对不上').has);

  // -- 指标
  const image = { width: 16, height: 16, pixels: synthRgba(16, 16, 11) };
  const same = { width: 16, height: 16, pixels: Buffer.from(image.pixels) };
  check('全等图 SSIM 恰为 1（不是"约等于"）', ssimLuma(image, same) === 1);
  check('全等图 PSNR 是无穷', psnrRgb(image, same) === Infinity);
  const sameDiff = pixelDiffSummary(image, same);
  check('全等图 maxdiff=0、差异像素=0、不建差异图', sameDiff.maxAbsDiff === 0 && sameDiff.diffPixels === 0 && sameDiff.rgba === null);

  const bumped = { width: 16, height: 16, pixels: Buffer.from(image.pixels) };
  bumped.pixels[0] = (bumped.pixels[0] + 1) & 0xff;
  const bumpDiff = pixelDiffSummary(image, bumped);
  check(
    '改 1 个像素 1 个台阶：maxdiff=1、差异像素=1',
    bumpDiff.maxAbsDiff === 1 && bumpDiff.diffPixels === 1 && bumpDiff.rgba !== null,
  );
  check('改 1 个像素：SSIM < 1', ssimLuma(image, bumped) < 1);
  check('改 1 个像素：PSNR 有限', Number.isFinite(psnrRgb(image, bumped)));
  check(
    'PSNR 的定义就是 RGB 三通道 MSE（拿 1/(3·256) 手推）',
    Math.abs(psnrRgb(image, bumped) - 10 * Math.log10((255 * 255) / (1 / (16 * 16 * 3)))) < 1e-9,
  );

  const stripesA = { width: 32, height: 32, pixels: Buffer.alloc(32 * 32 * 4) };
  const stripesB = { width: 32, height: 32, pixels: Buffer.alloc(32 * 32 * 4) };
  for (let y = 0; y < 32; y += 1) {
    for (let x = 0; x < 32; x += 1) {
      const base = (y * 32 + x) * 4;
      const a = (Math.floor(x / 2) % 2) * 255;
      const b = (Math.floor((x + 1) / 2) % 2) * 255;
      stripesA.pixels[base] = a;
      stripesA.pixels[base + 3] = 255;
      stripesB.pixels[base] = b;
      stripesB.pixels[base + 3] = 255;
    }
  }
  check('错位一格的条纹：SSIM 远低于 1（结构性差异看得见）', ssimLuma(stripesA, stripesB) < 0.5);
  check('错位一格的条纹：maxdiff 是 255', pixelDiffSummary(stripesA, stripesB).maxAbsDiff === 255);

  // -- 形状（--shape 落 shape.json 前先在这里把口径钉住：只出事实，不下结论）
  const mkSolid = (w, h, rgba) => {
    const pixels = Buffer.alloc(w * h * 4);
    for (let p = 0; p < w * h; p += 1) pixels.set(rgba, p * 4);
    return { width: w, height: h, pixels };
  };
  const copyImg = (img) => ({ width: img.width, height: img.height, pixels: Buffer.from(img.pixels) });
  const putPixel = (img, x, y, rgba) => { img.pixels.set(rgba, (y * img.width + x) * 4); };
  const channelInstances = (shape) => CHANNELS.reduce((total, c) => total + shape.per_channel[c].plus + shape.per_channel[c].minus, 0);
  const quadrantInstances = (shape) => Object.values(shape.sign_quadrant).reduce((total, q) => total + q.plus + q.minus, 0);

  const flatA = mkSolid(4, 4, [10, 20, 30, 255]);
  const flatShape = pixelDiffShape(flatA, copyImg(flatA));
  check(
    '形状：全等图一切为空（不是填零占位）',
    flatShape.diff_pixels === 0 && flatShape.max_abs_diff === 0 &&
      flatShape.signed_deltas.length === 0 &&
      flatShape.full_height_columns.length === 0 && flatShape.full_width_rows.length === 0 &&
      flatShape.bbox === null && flatShape.diff_cols_span === 0 && flatShape.diff_rows_span === 0 &&
      flatShape.diff_pixels_inside_full_height_columns === 0 &&
      channelInstances(flatShape) === 0 && quadrantInstances(flatShape) === 0,
  );

  // 正例：全图皆差时"每列都整高、每行都整宽"必须真的出现，
  // 否则两条空表检查会变成恒真（实现坏了也看不出来）。
  const allDiffShape = pixelDiffShape(mkSolid(4, 4, [10, 10, 10, 255]), mkSolid(4, 4, [20, 20, 20, 255]));
  check(
    '形状：全图皆差 → 每列整高、每行整宽（正例，防"空表恒真"）',
    allDiffShape.diff_pixels === 16 && allDiffShape.max_abs_diff === 10 &&
      allDiffShape.full_height_columns.join(',') === '0,1,2,3' &&
      allDiffShape.full_width_rows.join(',') === '0,1,2,3' &&
      allDiffShape.diff_pixels_inside_full_height_columns === 16,
  );
  check(
    '形状：全图皆差时 R/G/B 各 16 次 minus、A 不掺和（有符号差 a−b = 10−20）',
    allDiffShape.signed_deltas.join(',') === '-10' &&
      allDiffShape.per_channel.R.plus === 0 && allDiffShape.per_channel.R.minus === 16 &&
      allDiffShape.per_channel.G.minus === 16 && allDiffShape.per_channel.B.minus === 16 &&
      allDiffShape.per_channel.A.plus + allDiffShape.per_channel.A.minus === 0,
  );

  const stripesShape = pixelDiffShape(stripesA, stripesB);
  check(
    '形状：错位一格的条纹 → 16 根整高列（全是奇数列）、没有整宽行',
    stripesShape.full_height_columns.length === 16 &&
      stripesShape.full_height_columns.every((col) => col % 2 === 1) &&
      stripesShape.full_width_rows.length === 0 &&
      stripesShape.diff_cols_span === 31 && stripesShape.diff_rows_span === 32,
  );
  check(
    '形状：错位一格的条纹 → 差异 512 像素、max|Δ| 255、有符号差就是 ±255',
    stripesShape.diff_pixels === 512 && stripesShape.max_abs_diff === 255 &&
      JSON.stringify(stripesShape.signed_deltas) === JSON.stringify([-255, 255]) &&
      stripesShape.diff_pixels_inside_full_height_columns === 512,
  );
  check(
    '形状：per_channel 数的是通道实例（R 512、G/B/A 各 0），且四象限之和 == 四通道之和',
    stripesShape.per_channel.R.plus === 256 && stripesShape.per_channel.R.minus === 256 &&
      stripesShape.per_channel.G.plus + stripesShape.per_channel.G.minus === 0 &&
      stripesShape.per_channel.B.plus + stripesShape.per_channel.B.minus === 0 &&
      stripesShape.per_channel.A.plus + stripesShape.per_channel.A.minus === 0 &&
      quadrantInstances(stripesShape) === channelInstances(stripesShape) &&
      channelInstances(stripesShape) === stripesShape.diff_pixels,
  );
  check(
    '形状：象限对称（±255 各半、四象限各 64/64）',
    ['topLeft', 'topRight', 'bottomLeft', 'bottomRight'].every(
      (q) => stripesShape.sign_quadrant[q].plus === 64 && stripesShape.sign_quadrant[q].minus === 64,
    ),
  );

  // 符号方向必须能分辨：±255 那种"两半对半分"的图对调两侧看不出方向，
  // 所以这条用单像素 +1 的差异来钉（对调两侧必须正负互换，而不是碰巧还是对称的）。
  const oneA = mkSolid(4, 4, [50, 50, 50, 255]);
  const oneB = copyImg(oneA);
  oneB.pixels[0] = 51;
  const oneShape = pixelDiffShape(oneA, oneB);
  const oneShapeFlipped = pixelDiffShape(oneB, oneA);
  check(
    '形状：有符号差写死是 a−b（同一次差异，对调两侧必须正负互换）',
    oneShape.diff_pixels === 1 && oneShape.signed_deltas.join(',') === '-1' &&
      oneShape.per_channel.R.minus === 1 && oneShape.per_channel.R.plus === 0 &&
      oneShape.bbox.min_col === 0 && oneShape.bbox.max_col === 0 &&
      oneShape.bbox.min_row === 0 && oneShape.bbox.max_row === 0 &&
      oneShape.diff_cols_span === 1 && oneShape.diff_rows_span === 1 &&
      oneShapeFlipped.signed_deltas.join(',') === '1' &&
      oneShapeFlipped.per_channel.R.plus === 1 && oneShapeFlipped.per_channel.R.minus === 0,
  );
  // "最低位上的一步"这句话全靠这一栏：|Δ| = 1 不许被算进 ≥ 2。
  check(
    '形状：|Δ| ≥ 2 与 |Δ| = 1 分开数（±255 记 512、±1 记 0、±10 记 16）',
    stripesShape.magnitude_at_least_2.R === 512 &&
      ['G', 'B', 'A'].every((c) => stripesShape.magnitude_at_least_2[c] === 0) &&
      oneShape.magnitude_at_least_2.R === 0 && allDiffShape.magnitude_at_least_2.R === 16,
  );

  // 整高列读的是"差异没跑出这些列"，所以要有一次"跑出去了"的反例。
  const edgeA = mkSolid(4, 4, [0, 0, 0, 255]);
  const edgeB = copyImg(edgeA);
  for (let y = 0; y < 4; y += 1) putPixel(edgeB, 0, y, [7, 0, 0, 255]);
  putPixel(edgeB, 2, 0, [9, 0, 0, 255]);
  const edgeShape = pixelDiffShape(edgeA, edgeB);
  check(
    '形状：整高列之外的差异要被数出来（inside < diff_pixels）',
    edgeShape.full_height_columns.join(',') === '0' &&
      edgeShape.diff_pixels === 5 && edgeShape.diff_pixels_inside_full_height_columns === 4 &&
      edgeShape.diff_pixels_inside_full_height_columns < edgeShape.diff_pixels &&
      edgeShape.bbox.max_col === 2 && edgeShape.diff_cols_span === 3,
  );
  check(
    '形状：尺寸不同的两张图要报错（不静默截断）',
    throwsLike(
      () => pixelDiffShape({ width: 2, height: 2, pixels: Buffer.alloc(16) }, { width: 2, height: 3, pixels: Buffer.alloc(24) }),
      '尺寸不同',
    ).has,
  );

  // -- 汇总：并集与逐帧是两件事，两个都要有，且要能分辨
  const frameOf = (file, frame, shape) => ({ scene: 's', file, frame, shape });
  const colImgA = mkSolid(4, 4, [0, 0, 0, 255]);
  const colImgB0 = copyImg(colImgA);
  const colImgB1 = copyImg(colImgA);
  for (let y = 0; y < 4; y += 1) {
    putPixel(colImgB0, 0, y, [5, 0, 0, 255]);
    putPixel(colImgB1, 1, y, [5, 0, 0, 255]);
  }
  const shapeCol0 = pixelDiffShape(colImgA, colImgB0);
  const shapeCol1 = pixelDiffShape(colImgA, colImgB1);
  const summaryTwo = summarizeShape([frameOf('s-f000.png', 0, shapeCol0), frameOf('s-f001.png', 1, shapeCol1)]);
  check(
    '汇总：并集 ≠ 逐帧（两帧各差一列 → 并集两列、逐帧各一列，两个都记）',
    summaryTwo.length === 1 &&
      summaryTwo[0].full_height_columns_union.join(',') === '0,1' &&
      summaryTwo[0].full_height_columns_per_frame.join(',') === '1,1' &&
      summaryTwo[0].full_height_columns_identical_across_frames === false,
  );
  check(
    '汇总：逐帧相加 vs 取最大不能搞混（diff_pixels 相加 = 8、max_abs_diff 取最大 = 5）',
    summaryTwo[0].diff_pixels === 8 && summaryTwo[0].max_abs_diff === 5 &&
      summaryTwo[0].frames === 2 && summaryTwo[0].width === 4 && summaryTwo[0].height === 4 &&
      summaryTwo[0].bbox_union.min_col === 0 && summaryTwo[0].bbox_union.max_col === 1 &&
      summaryTwo[0].bbox_union.min_row === 0 && summaryTwo[0].bbox_union.max_row === 3 &&
      summaryTwo[0].all_diff_pixels_inside_full_height_columns === true,
  );
  const summarySame = summarizeShape([frameOf('s-f000.png', 0, shapeCol0), frameOf('s-f001.png', 1, shapeCol0)]);
  check(
    '汇总：逐帧同一批整高列时 identical=true、并集就等于那一批',
    summarySame[0].full_height_columns_union.join(',') === '0' &&
      summarySame[0].full_height_columns_per_frame.join(',') === '1,1' &&
      summarySame[0].full_height_columns_identical_across_frames === true,
  );
  const summaryOutside = summarizeShape([frameOf('s-f000.png', 0, shapeCol0), frameOf('s-f001.png', 1, edgeShape)]);
  check(
    '汇总：只要有一帧的差异跑出整高列，all_diff_pixels_inside 就是 false',
    summaryOutside[0].all_diff_pixels_inside_full_height_columns === false,
  );

  check('尺寸不同的两张图要报错', throwsLike(() => ssimLuma({ width: 12, height: 12, pixels: Buffer.alloc(12 * 12 * 4) }, { width: 12, height: 13, pixels: Buffer.alloc(12 * 13 * 4) }), '尺寸不同').has);
  check('比窗口还小的图要报错', throwsLike(() => ssimLuma({ width: 10, height: 10, pixels: Buffer.alloc(10 * 10 * 4) }, { width: 10, height: 10, pixels: Buffer.alloc(10 * 10 * 4) }), '窗口').has);

  // -- 配对
  const labels = { a: 'A侧', b: 'B侧' };
  check('空集要拒绝（空集不准当全过）', throwsLike(() => assertPairing(pairNames([], ['s-f000.png'], labels), labels), '一张 PNG 都没有').has);
  check(
    '只在一侧的文件要点名',
    throwsLike(() => assertPairing(pairNames(['s-f000.png', 'onlyA-f000.png'], ['s-f000.png'], labels), labels), 'onlyA-f000.png').has,
  );
  check(
    '不合契约的文件名要点名',
    throwsLike(() => assertPairing(pairNames(['readme.png'], ['readme.png'], labels), labels), 'readme.png').has,
  );
  check(
    '同一帧号解析出两次要报错',
    throwsLike(() => assertPairing(pairNames(['s-f2.png', 's-f002.png'], ['s-f2.png', 's-f002.png'], labels), labels), '同一个帧号').has,
  );
  check(
    '帧号不连续要报错（并说缺哪个）',
    throwsLike(() => assertPairing(pairNames(['s-f000.png', 's-f002.png'], ['s-f000.png', 's-f002.png'], labels), labels), '缺 1').has,
  );
  const ordered = pairNames(['z-f001.png', 'a-f000.png', 'z-f000.png', 'a-f001.png'], ['z-f001.png', 'a-f000.png', 'z-f000.png', 'a-f001.png']);
  const orderedOk =
    ordered.pairs[0].scene === 'a' && ordered.pairs[0].frame === 0 &&
    ordered.pairs[1].scene === 'a' && ordered.pairs[1].frame === 1 &&
    ordered.pairs[3].scene === 'z' && ordered.pairs[3].frame === 1;
  check('场景按码元序、帧号按数值排', orderedOk);

  // -- 阈值文件
  const table = parseThresholds(
    [
      '# 注释行',
      '',
      '[default]',
      'mean_ssim_min = 0.995',
      'min_ssim_min = 0.98',
      'max_abs_diff_max = 1',
      '',
      '[scenario.blur]',
      'max_abs_diff_max = 2 # 行尾注释',
    ].join('\n'),
  );
  check('解析出 default 与场景档', table.defaults.mean_ssim_min === 0.995 && table.scenarios.get('blur').max_abs_diff_max === 2);
  check('场景档覆盖 default（合并后 blur 用 2）', resolveThresholds(table, 'blur').max_abs_diff_max === 2);
  check('没被覆盖的键沿用 default', resolveThresholds(table, 'blur').min_ssim_min === 0.98);
  check('未列出的场景也能解析出档位', resolveThresholds(table, 'gradient').mean_ssim_min === 0.995);
  check(
    '不认识的键要报错（点名）',
    throwsLike(() => parseThresholds('[default]\nmean_ssim = 0.9\n'), 'mean_ssim').has,
  );
  check('不认识的节要报错', throwsLike(() => parseThresholds('[nope]\n'), '[nope]').has);
  check('键没值要报错', throwsLike(() => parseThresholds('[default]\nmean_ssim_min =\n'), '没给值').has);
  check('键写在节外要报错', throwsLike(() => parseThresholds('mean_ssim_min = 1\n'), '节之前').has);
  // 值必须写成"本身合法"的：不合法会先撞上范围那条，重复键这条根本轮不到——
  // 断言就会出现"实现没修、用例也没红"的假绿。两条路都要各自点到名。
  check(
    '同一个键写两遍要报错',
    throwsLike(() => parseThresholds('[default]\nmin_ssim_min = 0.9\nmin_ssim_min = 0.98\n'), '写了两遍').has,
  );
  check(
    '越界的重复键：先报越界（不静默吞掉）',
    throwsLike(() => parseThresholds('[default]\nmin_ssim_min = 1\nmin_ssim_min = 2\n'), '要在 0..1').has,
  );
  check(
    '场景节里同一个键写两遍也要报错',
    throwsLike(() => parseThresholds('[scenario.blur]\nmax_abs_diff_max = 1\nmax_abs_diff_max = 2\n'), '写了两遍').has,
  );
  check(
    '同一个场景写成两个节、键撞车也要报错',
    throwsLike(
      () => parseThresholds('[scenario.blur]\nmax_abs_diff_max = 1\n[scenario.gradient]\nmean_ssim_min = 1\n[scenario.blur]\nmax_abs_diff_max = 2\n'),
      '写了两遍',
    ).has,
  );
  check(
    '场景没档位要报错（没有 default 打底时）',
    throwsLike(() => resolveThresholds(parseThresholds('[scenario.other]\nmean_ssim_min = 1\nmin_ssim_min = 1\n'), 'gradient'), 'gradient').has,
  );

  // -- 判定：每条闸各自能判红，且理由点名那一条
  const mkScene = (over = {}) => ({
    scene: 'gradient',
    frames: 16,
    mean_ssim_luma: 1,
    min_ssim_luma: 1,
    min_psnr_rgb_db: Infinity,
    max_abs_diff: 0,
    diff_pixels: 0,
    worst_frames: [],
    ...over,
  });
  const tableOf = (extra = {}) => ({
    defaults: { mean_ssim_min: 0.995, min_ssim_min: 0.98, max_abs_diff_max: 1, ...extra },
    scenarios: new Map(),
  });
  check('全达标判绿', judge({ scenes: [mkScene()] }, tableOf()).verdict === 'pass');
  const meanFail = judge({ scenes: [mkScene({ mean_ssim_luma: 0.99 })] }, tableOf());
  check('mean 不够 → 红，且理由点名 mean_ssim_luma', meanFail.verdict === 'fail' && meanFail.scenes[0].failures.some((f) => f.includes('mean_ssim_luma')));
  const minFail = judge({ scenes: [mkScene({ min_ssim_luma: 0.97 })] }, tableOf());
  check('min 不够 → 红，且理由点名 min_ssim_luma', minFail.verdict === 'fail' && minFail.scenes[0].failures.some((f) => f.includes('min_ssim_luma')));
  const diffFail = judge({ scenes: [mkScene({ max_abs_diff: 5 })] }, tableOf());
  check('max|Δ| 超 → 红，且理由点名 max_abs_diff', diffFail.verdict === 'fail' && diffFail.scenes[0].failures.some((f) => f.includes('max_abs_diff 5 > 1')));
  const psnrFail = judge({ scenes: [mkScene({ min_psnr_rgb_db: 25 })] }, tableOf({ psnr_db_min: 30 }));
  check('PSNR 低 → 红，且理由点名 psnr', psnrFail.verdict === 'fail' && psnrFail.scenes[0].failures.some((f) => f.includes('min_psnr_rgb_db')));
  check('只判该判的：没写 psnr_db_min 就不拿 PSNR 说事', judge({ scenes: [mkScene({ min_psnr_rgb_db: 5 })] }, tableOf()).verdict === 'pass');

  // -- 端到端：合成两批图，落盘、复现、差异图、CSV 形状
  const work = mkdtempSync(join(tmpdir(), 'dhampir-framediff-'));
  try {
    const dirA = join(work, 'a');
    const dirB = join(work, 'b');
    mkdirSync(dirA);
    mkdirSync(dirB);
    const framesSpec = [
      { scene: 'beta', frame: 0 },
      { scene: 'beta', frame: 1 },
      { scene: 'alpha', frame: 0 },
    ];
    for (const spec of framesSpec) {
      const name = `${spec.scene}-f${String(spec.frame).padStart(3, '0')}.png`;
      const base = synthRgba(12, 12, 100 + spec.frame);
      writeFileSync(join(dirA, name), encodePng(12, 12, base));
      const other = Buffer.from(base);
      if (spec.scene === 'beta' && spec.frame === 1) other[0] = (other[0] + 3) & 0xff;
      writeFileSync(join(dirB, name), encodePng(12, 12, other));
    }
    const analysis = analyzePair({ dirA, dirB, labelA: 'A侧', labelB: 'B侧' });
    check(
      '端到端：只有该有差异的那一帧进差异图清单',
      analysis.diffRgbaByName.size === 1 && analysis.diffRgbaByName.has('beta-f001.png'),
    );
    check('端到端：场景按名字排（alpha 在前）', analysis.scenes[0].scene === 'alpha' && analysis.scenes[1].scene === 'beta');
    const judged = judge(analysis, { defaults: { mean_ssim_min: 0.5, min_ssim_min: 0.3, max_abs_diff_max: 3 }, scenarios: new Map() });
    check('端到端：判绿（档位够松）', judged.verdict === 'pass');
    const tight = judge(analysis, { defaults: { mean_ssim_min: 0.5, min_ssim_min: 0.3, max_abs_diff_max: 0 }, scenarios: new Map() });
    check('端到端：max|Δ| 刹到 0 就红，exit_code 是 1', tight.verdict === 'fail' && buildVerdictJson({
      analysis,
      judged: tight,
      amp: 16,
      thresholdsInfo: { path: 'x', digest: '0'.repeat(16), resolved: {} },
      shown: { a: 'a', b: 'b' },
    }).exit_code === 1);

    const thresholdsInfo = { path: 'scripts/framediff-thresholds.toml', digest: '0123456789abcdef', resolved: {} };
    const out1 = join(work, 'out1');
    const out2 = join(work, 'out2');
    const written1 = writeOutputs({ outDir: out1, analysis, judged, amp: 16, thresholdsInfo, shown: { a: 'a', b: 'b' }, labels: { a: 'A侧', b: 'B侧' } });
    writeOutputs({ outDir: out2, analysis, judged, amp: 16, thresholdsInfo, shown: { a: 'a', b: 'b' }, labels: { a: 'A侧', b: 'B侧' } });
    // 可复现性这三条比的是**字节**，所以读字节版；文件缺了就是"不可复现"，不是崩溃。
    const bytesIfThere = (path) => (existsSync(path) ? readFileSync(path) : null);
    const csv1 = bytesIfThere(written1.summaryPath);
    const csv2 = bytesIfThere(join(out2, 'summary.csv'));
    check('summary.csv 可复现（同输入两次落盘逐字节一致）', csv1 !== null && csv2 !== null && csv1.equals(csv2));
    const verdict1 = bytesIfThere(written1.verdictPath);
    const verdict2 = bytesIfThere(join(out2, 'verdict.json'));
    check('verdict.json 可复现', verdict1 !== null && verdict2 !== null && verdict1.equals(verdict2));
    const diff1 = bytesIfThere(join(out1, 'diff', 'beta-f001.png'));
    const diff2 = bytesIfThere(join(out2, 'diff', 'beta-f001.png'));
    check('差异图可复现', diff1 !== null && diff2 !== null && diff1.equals(diff2));
    const diffImage = diff1 === null ? null : decodePng(diff1);
    check(
      '差异图内容 = |Δ|×amp、clamp、alpha=255',
      diffImage !== null &&
        diffImage.pixels[0] === 48 &&
        diffImage.pixels[1] === 0 &&
        diffImage.pixels[2] === 0 &&
        diffImage.pixels[3] === 255,
    );
    const csv = readIfThere(written1.summaryPath);
    const reportText = readIfThere(written1.reportPath);
    const verdictRaw = readIfThere(written1.verdictPath);
    check(
      'summary.csv / verdict.json / report.txt 三件都落盘了',
      csv !== null && reportText !== null && verdictRaw !== null,
    );
    check('summary.csv 只有 LF、以换行结尾', csv !== null && !csv.includes('\r') && csv.endsWith('\n'));
    check('summary.csv 的行数对：表头 + 3 帧', csv !== null && csv.trimEnd().split('\n').length === 4);
    const csvRows = (csv ?? '').trimEnd().split('\n').slice(1).map((row) => row.split(','));
    const betaRow = csvRows.find((row) => row[2] === 'beta-f001.png');
    const alphaRow = csvRows.find((row) => row[2] === 'alpha-f000.png');
    check(
      'summary.csv：有差异的帧带 diff/diff 列，没差异的帧留空',
      Boolean(betaRow) &&
        betaRow.length === 10 &&
        betaRow[9] === 'diff/beta-f001.png' &&
        Boolean(alphaRow) &&
        alphaRow.length === 10 &&
        alphaRow[9] === '',
    );
    check(
      'report.txt：落的就是屏幕上那份（点名本次落盘位置）、只有 LF、结尾有换行',
      reportText !== null &&
        !reportText.includes('\r') &&
        reportText.endsWith('\n') &&
        reportText.includes(written1.summaryPath) &&
        reportText.includes(written1.reportPath) &&
        reportText.includes('全部达标（退出码 0）'),
    );
    const verdictJson = verdictRaw === null ? null : JSON.parse(verdictRaw);
    check(
      'verdict.json：全等图那一场景的 PSNR 写成 null（JSON 没有无穷）',
      verdictJson !== null && verdictJson.scenes[0].min_psnr_rgb_db === null && verdictJson.scenes[1].min_psnr_rgb_db !== null,
    );
    check(
      'verdict.json：totals 对得上',
      verdictJson !== null && verdictJson.totals.frames === 3 && verdictJson.totals.diff_images === 1 && verdictJson.totals.diff_pixels === 1,
    );

    // -- --shape 那条路：形状要真的进帧、真的绑住这一批输入，且**不许碰**记录三件
    const analysisShape = analyzePair({ dirA, dirB, labelA: 'A侧', labelB: 'B侧', shape: true });
    check(
      '端到端：--shape 下每帧都带形状，且与差异图那一路的数字一致',
      analysisShape.frames.length === 3 &&
        analysisShape.frames.every((f) => f.shape !== null) &&
        analysisShape.frames.every((f) => f.shape.diff_pixels === f.diff_pixels && f.shape.max_abs_diff === f.max_abs_diff),
    );
    check(
      '端到端：--shape 不改变这一批输入的身份（帧集摘要/字节数与不带 --shape 时相同）',
      analysisShape.setDigestA === analysis.setDigestA && analysisShape.setDigestB === analysis.setDigestB &&
        analysisShape.bytesTotalA === analysis.bytesTotalA && analysisShape.bytesTotalB === analysis.bytesTotalB,
    );
    const judgedShape = judge(analysisShape, { defaults: { mean_ssim_min: 0.5, min_ssim_min: 0.3, max_abs_diff_max: 3 }, scenarios: new Map() });
    // 落**同一个目录**再比：report.txt 里本来就会写本次落盘位置，换个目录那一行必然不同——
    // 那是路径在变（探针真撞上过：第一处不同就是「记录 → …」那行），不是 --shape 在改记录。
    // 比的东西要写成"同一批输入 + 同一个位置"。
    const reportBefore = readIfThere(written1.reportPath);
    const writtenShapeRun = writeOutputs({
      outDir: out1,
      analysis: analysisShape,
      judged: judgedShape,
      amp: 16,
      thresholdsInfo,
      shown: { a: 'a', b: 'b' },
      labels: { a: 'A侧', b: 'B侧' },
    });
    const csvAfter = bytesIfThere(writtenShapeRun.summaryPath);
    const verdictAfter = bytesIfThere(writtenShapeRun.verdictPath);
    check(
      '端到端：--shape 不动记录三件（summary.csv / verdict.json / report.txt 逐字节相同）',
      csv1 !== null && csvAfter !== null && csv1.equals(csvAfter) &&
        verdict1 !== null && verdictAfter !== null && verdict1.equals(verdictAfter) &&
        reportBefore !== null && readIfThere(writtenShapeRun.reportPath) === reportBefore,
    );
    check(
      '端到端：writeOutputs 自己不写 shape.json（形状是 main 另加的一份证据，不是记录的第二个版本）',
      !existsSync(join(out1, 'shape.json')),
    );
    const shapeText1 = JSON.stringify(buildShapeJson({ analysis: analysisShape, thresholdsInfo, shown: { a: 'a', b: 'b' } }), null, 2);
    const shapeText2 = JSON.stringify(
      buildShapeJson({
        analysis: analyzePair({ dirA, dirB, labelA: 'A侧', labelB: 'B侧', shape: true }),
        thresholdsInfo,
        shown: { a: 'a', b: 'b' },
      }),
      null,
      2,
    );
    check('端到端：shape.json 的内容可复现（同一批输入两次生成逐字节一致）', shapeText1 === shapeText2);
    const shapeDoc = JSON.parse(shapeText1);
    check(
      '端到端：shape.json 绑住这一批输入（两侧摘要/字节数/档位摘要）与逐场景形状',
      shapeDoc.kind === 'framediff-shape' &&
        shapeDoc.signed_delta_definition === 'a - b' &&
        shapeDoc.inputs.a.set_digest === analysisShape.setDigestA &&
        shapeDoc.inputs.b.set_digest === analysisShape.setDigestB &&
        shapeDoc.inputs.a.bytes_total === analysisShape.bytesTotalA &&
        shapeDoc.inputs.b.bytes_total === analysisShape.bytesTotalB &&
        shapeDoc.inputs.a.frames === 3 && shapeDoc.inputs.b.frames === 3 &&
        shapeDoc.thresholds.digest === thresholdsInfo.digest &&
        shapeDoc.scenes.length === 2 &&
        shapeDoc.scenes[0].scene === 'alpha' && shapeDoc.scenes[1].scene === 'beta' &&
        shapeDoc.scenes[0].per_frame.length === 1 &&
        shapeDoc.scenes[0].per_frame[0].file === 'alpha-f000.png' &&
        shapeDoc.scenes[1].per_frame.length === 2 &&
        shapeDoc.scenes[1].diff_pixels === 1 && shapeDoc.scenes[1].max_abs_diff === 3,
    );
  } finally {
    rmSync(work, { recursive: true, force: true });
  }

  const failed = cases.filter((c) => !c.ok);
  for (const c of cases) console.log(`  ${c.ok ? '✓' : '✗'} ${c.name}${c.ok || c.detail === '' ? '' : `（${c.detail}）`}`);
  if (failed.length > 0) {
    console.error(`\n✗ 自检失败 ${failed.length}/${cases.length}：`);
    for (const c of failed) console.error(`  - ${c.name}${c.detail === '' ? '' : `：${c.detail}`}`);
    return 1;
  }
  console.log(`\n✓ 自检通过（${cases.length} 条用例）`);
  return 0;
}

// 本进程不调 process.exit()：本机 Node/Windows 上真执行 process.exit() 会
// 概率性撞 libuv 断言，把退出码变成负数——而"退出码可信"正是这个工具的理由。
// 没有子进程、没有待 drain 的句柄，设 process.exitCode 后自然退出即刻发生。
process.exitCode = main();
