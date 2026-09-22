#!/usr/bin/env node
// records/m2（M2「双运行时同帧比对」）的记录守卫。
//
// 为什么要有这个东西：M2 的结论（"两个宿主渲染同一批帧，结果在容差内一致"）不是一段话，
// 是一批文件。文件可以被替换、被误删、被下一次真跑覆盖掉一半，也可以"看着挺全"其实
// 从没被谁复算过。所以这份守卫把记录**重算一遍**：不信任记录里的任何数字，只把它当作
// "待核对的声明"——像素摘要从 PNG 里解出来自己算，整表摘要用自己算出的像素摘要重拼，
// SSIM / PSNR / 差异统计按规格重写一份，档位文件重新解析、重新判定。
//
// 与生成侧的关系（这是这份守卫的立身之本）：两侧 PNG 都是 `dhampir-core` 用 `png` crate
// 编出来的，比对工具 `scripts/dhampir-framediff.mjs` 自写了解码器。守卫**再写一遍**：
// PNG 解码、FNV-1a 64、SHA-256、SSIM/PSNR/差异统计、差异形状、TOML 档位解析、CSV/JSON/
// 报告渲染，全部在这里另起一份。与生产代码（Rust）或工具（JS）共用同一份实现时，
// "实现对了吗"就没有第二个人回答——库错在哪、滤波器理解错在哪，两边会同样地错。
//
// 复算出来的东西分三种地位，红的时候要能一眼归因：
//   ① 整数（字节数、摘要、像素统计、行列集合）→ **逐位相等**；
//   ② 浮点（SSIM / PSNR）→ 按 `sameNumber` 的容差比（浮点乘加不承诺跨实现逐位同）；
//      但记录里那份 `summary.csv` 是**文本**，守卫按自己重算的数渲染出来再逐字节比——
//      于是"浮点等价"被升级回"文本相等"，红了就直接指到某一行某一列；
//   ③ 语义（判定 pass/fail、README 有没有交代清楚）→ 重新判一遍。
//
// 用法：
//   node scripts/check-m2-record.mjs
//   node scripts/check-m2-record.mjs --record records/m2
//   node scripts/check-m2-record.mjs --self-test

import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  readdirSync,
  readFileSync,
  rmSync,
  statSync,
  writeFileSync,
} from 'node:fs';
import { deflateSync, inflateSync } from 'node:zlib';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const DEFAULT_RECORD = join(REPO_ROOT, 'records', 'm2');

// ---------------------------------------------------------------------------
// 这份记录被钉住的形状与锚
//
// 全部是**记录里的口径**，不是"恰好这一次的数"。锚（摘要与字节数）是从记录里抄下来的：
// 它们的作用是让"整份记录被换成了另一批自洽的假数据"也能被抓到——只做自洽性复核的话，
// 换一批同样自洽的数据是查不出来的。改记录就要改这里，并说明为什么。
// ---------------------------------------------------------------------------

export const EXPECTED = {
  // corpus 形状：与 M1 同源（同一份 `dhampir-core` 的表，两个宿主共用）
  scenes: ['gradient', 'checker', 'srgb_linear', 'alpha_stack', 'blur'],
  frameRange: '0..16',
  framesPerScene: 16,
  frames: 80,
  points: 368,
  targetSize: '256x256',
  targetFormat: 'Rgba8UnormSrgb',
  byteTolerance: 1,
  probeDigest: 'c3f0da6b37577e55',
  probeFormatVersion: 1,
  crateVersion: '0.0.1',

  // 两条浏览器腿（本里程碑跑的）
  legs: [
    {
      slug: 'browser',
      label: 'browser-webgpu-nvidia',
      legSlugInRecord: 'm2',
      // 浏览器宿主自己的用法与构建参数：两条腿**相同**（只有 extra_args 不同）
      requestedBackends: 'BROWSER_WEBGPU',
      buildProfile: 'debug',
      wgpuVersion: '30.0.1',
      framesDigest: '71ecc80cade3d73d',
      setDigest: '4bc004b502a1301a',
      pngBytes: 129619,
      readingsLines: 532,
      readingsBytes: 112215,
      findingsFalse: [],
      rerunRepro: null,
      // 浏览器当场报的那块卡（页面里问不出型号，只能报这三样）
      inPage: { architecture: 'lovelace', vendor: 'nvidia', subgroupMaxSize: 32 },
      extraArgs: [],
      // 这条腿画出来的就是 native 那 80 张，所以摘要与期望值一致
      digestMatchesExpect: true,
      // 宿主侧对上的卡（host-gpu.json 的 resolved 栏）
      gpu: { vendor: 'nvidia', vendorId: 4318, deviceId: 10118, device: 'NVIDIA GeForce RTX 4070', driverVersion: '32.0.16.1074' },
    },
    {
      slug: 'browser-amd',
      label: 'browser-webgpu-amd',
      legSlugInRecord: 'm2-amd',
      requestedBackends: 'BROWSER_WEBGPU',
      buildProfile: 'debug',
      wgpuVersion: '30.0.1',
      framesDigest: 'a37b0ab5140b18e6',
      setDigest: 'bc09803eb44beac8',
      pngBytes: 129765,
      readingsLines: 532,
      readingsBytes: 112215,
      // 服务端 4 条 findings 里第 2、3 条为 false、audit.findings 里第 4、5 条为 false——
      // 内容是"这条腿与 native 不一样，差异交给 framediff 判定"。**设计意图，不是失败**。
      findingsFalse: [2, 3],
      auditFindingsFalse: [4, 5],
      rerunRepro: {
        framesDigest: 'a37b0ab5140b18e6',
        pngBytes: 129765,
        readingsBytes: 112215,
        readingsSha256_16: '496a5e3ef2e8e2ba',
      },
      inPage: { architecture: 'rdna-2', vendor: 'amd', subgroupMaxSize: 64 },
      extraArgs: ['--force_low_power_gpu'],
      // 与 native 不同：这台机器上 AMD 的 sRGB 往返在 gradient 上有 1 LSB 的差，
      // 于是整表摘要与 M1 归档的期望值不一样。**这条"不一样"本身是记录的一部分。**
      digestMatchesExpect: false,
      gpu: { vendor: 'amd', vendorId: 4098, deviceId: 5056, device: 'AMD Radeon(TM) Graphics', driverVersion: '32.0.21030.2001' },
    },
  ],

  /**
   * 两条浏览器腿之间的差异（服务端逐字段比出来的形状）。
   *
   * 记这些数不是"记录差异有多大"，而是**把差异钉住**：M2 的结论是"差异只有这些、
   * 且都说得清来历"。哪天多出一条差异路径，就说明有东西变了。
   */
  legDrift: {
    // adapter.json 的差异路径（恰好这 6 条）
    adapterPaths: [
      'backend_slug',
      'in_page.architecture',
      'in_page.subgroup_max_size',
      'in_page.vendor',
      'unix_epoch_millis',
      'unix_epoch_seconds',
    ],
    // readings.txt：38 行不同，首个在第 22 行（0 基）
    readingsDiffLines: 38,
    readingsFirstDiffLine: 22,
    // 截图 json 里必须不同的三项（其余差异都是"当场的事实"，这两项才是判据）
    screenshotMustDiffer: ['browser.extra_args', 'round.frames_digest', 'round.digest_matches_expect'],
    /**
     * 反过来：**不该**变的那几栏必须一样。
     *
     * 逐字段比一遍整份 json 会报出上百条"当场的事实"（时间戳、设备表顺序、耗时），
     * 那些不是判据；这里只挑与"渲染对不对、跑的范围对不对"有关的那几栏。
     * 比较用**结构化**口径（两侧各自 `JSON.stringify` 再比），不是把对象摊成字符串：
     * `round.counts` 是对象、`browser.extra_args` 是数组，摊平之后比法就成了另一种东西。
     */
    screenshotMustEqual: [
      'viewport.width',
      'viewport.height',
      'round.frames_verified',
      'round.counts',
      'round.scene',
      'round.frames_arg',
    ],

    /**
     * 两条腿的 `run.json`：**不是**逐字节相同，差的是这些。
     *
     * 这条口径是被实测逼出来的：早先 README 里写过一句"两份 readings.txt 与 run.json
     * 逐字节相同"，量了一遍才发现是错的——readings.txt 是 browser 与 native 相同
     * （AMD 有 38 行不同），run.json 则是三份都不一样。所以差异被拆成两句、钉成**形状**：
     *
     *   · 与 native 比只有 2 条路径不同：`adapter_name`（浏览器读不出卡名，服务端写 null）
     *     与 `requested`（BROWSER_WEBGPU / DX12）。**80 帧本身逐字节相同**，
     *     差的只是"这是谁跑的"。
     *   · 两条浏览器腿之间 273 条：48 帧的三种摘要（pixel/png/repeat）+ `frames_digest`
     *     + 31 处 `png_bytes` + 97 处采样点字段（`measured` / `distance` / `detail`）。
     *
     * 数这些组不是为了"差异有多大"，是为了把差异**钉住**：多出一组就说明有东西变了，
     * 而"多了 1 组"与"差了 1 LSB"必须分得开。`png_bytes` 那 31 不是 48：长度撞上了
     * （同一个数只是巧合，不是设计），所以它按**实测**钉，不按"应该等于 48"钉。
     */
    runJsonVsNativePaths: ['backends[0].adapter_name', 'backends[0].requested'],
    runJsonDiffGroups: {
      'backends[0].frames[i].pixel_digest': 48,
      'backends[0].frames[i].png_digest': 48,
      'backends[0].frames[i].repeat_pixel_digest': 48,
      'backends[0].frames[i].png_bytes': 31,
      'backends[0].frames[i].points[j].detail': 24,
      'backends[0].frames[i].points[j].distance': 19,
      'backends[0].frames[i].points[j].measured[i]': 54,
      'backends[0].frames_digest': 1,
    },
    // 48 帧的像素摘要不同、32 帧逐字节相同：`checker` 与 `srgb_linear` 在 AMD 上也没变
    differingFrames: 48,
    differingScenes: ['alpha_stack', 'blur', 'gradient'],
    identicalScenes: ['checker', 'srgb_linear'],
    pngBytesTotal: { browser: 129619, 'browser-amd': 129765 },
  },

  // native 腿来自 M1 归档——**不是**本里程碑跑的。M2 的判据正是"这两批字节一样"，
  // 所以这份守卫要读它；它不在就该红（没它就回答不了 M2 的问题）。
  native: {
    dir: 'records/m1/dx12/frames',
    runJson: 'records/m1/dx12/run.json',
    label: 'native-dx12-nvidia',
    framesDigest: '71ecc80cade3d73d',
    setDigest: '4bc004b502a1301a',
    pngBytes: 129619,
    // M2 要拿它当锚的那几栏：帧数、以及"同一份测量文本"的字节数
    frames: 80,
    readingsBytes: 112215,
  },

  // 两份 framediff 记录
  framediff: [
    {
      slug: 'framediff',
      a: 'browser',
      b: 'native',
      thresholds: 'scripts/framediff-thresholds-exact.toml',
      thresholdsDigest: 'fce01cde0b735693',
      shape: false,
      amp: 16,
      diffImages: 0,
      diffPixels: 0,
      reportBytes: 897,
      summaryCsvBytes: 3990,
      verdictJsonBytes: 5139,
    },
    {
      slug: 'framediff-crossvendor',
      a: 'browser-amd',
      b: 'native',
      thresholds: 'scripts/framediff-thresholds.toml',
      thresholdsDigest: '3f9560d1e3d4fb3f',
      shape: true,
      amp: 16,
      diffImages: 48,
      diffPixels: 1105968,
      reportBytes: 1121,
      summaryCsvBytes: 6620,
      verdictJsonBytes: 5543,
      // 字符 271803 / 字节 271845（里面有 128 列的长数组，非 ASCII 会让两者不等——
      // 记这个数时踩过一次：把字符数当成了字节数）
      shapeJsonBytes: 271845,
    },
  ],

  wasmTests: { listedTotal: 10, passed: 10, failed: 0, targets: 2 },

  rootFiles: ['README.md', 'wasm-tests.json'],
  subdirs: ['browser', 'browser-amd', 'framediff', 'framediff-crossvendor'],

  /**
   * README 里**不许**出现的说法（逐字）。
   *
   * 这几句都是这一版亲手写错过、后来按实测量掉重写的：留着它们，读的人就会以为
   * 两条腿的 `readings.txt` 与 `run.json` 是逐字节相同的。写 README 时连"引用旧那句"
   * 都不行——所以它们是**字面量禁令**，不是"别写错"的口号。
   */
  readmeForbidden: [
    '两份 readings.txt 与 run.json 逐字节相同',
    'readings.txt 两份逐字节相同',
    'run.json 也逐字节相同',
  ],

  /** README 不许短于这个字节数：它是这份记录唯一的读入口，掏空了比写错更糟。 */
  readmeMinBytes: 1024,

  /** 独立复核那份报告不许短于这个字节数（"复核过了，没问题"这种一句话不算证据）。 */
  reviewMinBytes: 1024,

  /**
   * 记录根目录里**允许**出现的其它文件。
   *
   * 这不是随手列一下：这份目录是给复核的人读的，"记录就是这些"这句话得能被查。
   * 多出一个没人认领的文件（旧截图、草稿、跑了一半的输出）不该悄悄留着。
   * 允许清单里放的是**后到**的两类产物：
   *
   *   · `acceptance.json` 与 13 份 `<判据 id>.txt`——`record-acceptance.mjs` 生成。
   *     本守卫**不要求**它们在（它们里面有一条判据就是"把本守卫跑一遍"，
   *     要求它存在就成了循环），但一旦在，就必须整份是绿的、id 一条不差。
   *   · `review-independent.md`——独立复核者写的，同样后到、同样不要求存在；
   *     在的话必须不是空壳。
   */
  acceptanceIds: [
    'native-check',
    'native-tests',
    'wasm-check',
    'cross-runtime',
    'guard-core-purity',
    'guard-dep-graph',
    'guard-text-hygiene',
    'guard-m1-record',
    'guard-m1-record-self-test',
    'framediff-self-test',
    'wgsl-census',
    'guard-m2-record',
    'guard-m2-record-self-test',
  ],
  optionalRootFiles: ['acceptance.json', 'review-independent.md'],

  /**
   * README 必须写出来的事实（**逐字**出现在文里才算）。
   *
   * 每一条都是本里程碑结论里的一个数：记录改了而 README 没改，下一个人读到的就是
   * 另一件事。M2 的 README 真写错过一句（"readings.txt 与 run.json 两份逐字节相同"），
   * 所以除了"必须写什么"，`checkRecordHonesty` 还钉了"不许写什么"。
   *
   * 这些字面量在自检里也当反向用例用：抠掉任意一条要红。
   */
  readmeAnchors: [
    '71ecc80cade3d73d', // browser / native 的整表摘要（两个宿主同一批帧）
    'a37b0ab5140b18e6', // AMD 腿的整表摘要（与上面不同，且这正是结论的一部分）
    '4bc004b502a1301a', // 帧**集合**摘要（含文件名与文件字节），browser 与 native 相等
    'fce01cde0b735693', // 严格档（framediff）的档位摘要
    '3f9560d1e3d4fb3f', // 跨厂商档的档位摘要
    '129619', // browser / native 的 PNG 总字节
    '129765', // AMD 腿的 PNG 总字节
    '1105968', // 跨厂商档的差异像素总数
    '38 行', // 两条腿 readings.txt 不同的行数
    '第 22 行', // 首个不同的行（0 基）
    '273 条', // 两条腿 run.json 的差异路径数
    '0.9995', // 跨厂商档的 SSIM 下限
    '--force_low_power_gpu', // AMD 腿多出来的那条启动参数
    '10/10', // wasm 侧双运行时等值报告
    '6 条路径', // 两条腿 adapter.json 的差异路径数
  ],
};

// ---------------------------------------------------------------------------
// 摘要：FNV-1a 64
//
// 与 `dhampir-timeline` 的 `fnv1a64` 同参数（偏移基数 0xcbf29ce484222325、
// 质数 0x100000001b3、8 位一乘）。**故意重写一遍**而不是引 Rust 那份。
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

/** 流式版本：`fnv1a64Update(fnv1a64(a) 的内部值, b)` 与 `fnv1a64(a+b)` 等价。 */
export function fnv1a64State(bytes) {
  let hash = FNV_OFFSET_BASIS;
  for (let i = 0; i < bytes.length; i += 1) {
    hash ^= BigInt(bytes[i]);
    hash = (hash * FNV_PRIME) & MASK64;
  }
  return hash;
}

export function fnv1a64Update(state, bytes) {
  let hash = state;
  for (let i = 0; i < bytes.length; i += 1) {
    hash ^= BigInt(bytes[i]);
    hash = (hash * FNV_PRIME) & MASK64;
  }
  return hash;
}

export function fnv1a64Hex(state) {
  return state.toString(16).padStart(16, '0');
}

// ---------------------------------------------------------------------------
// 摘要：SHA-256
//
// 记录里截图旁证写着 `screenshot.sha256`，生成侧用 `node:crypto` 算的。这里**自己实现**
// 一遍：与被验证的那一侧共用同一份实现，"算对了吗"就没有第二个人回答。自检里有
// 空串 / "abc" / 两段不同长度的已知向量把它们钉住。
// ---------------------------------------------------------------------------

const SHA256_K = new Uint32Array([
  0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
  0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
  0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
  0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
  0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
  0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
  0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
  0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
]);

function rotr32(value, bits) {
  return ((value >>> bits) | (value << (32 - bits))) >>> 0;
}

/** SHA-256 → 64 位小写十六进制。只处理 < 2^32 字节的输入（截图就是这个量级）。 */
export function sha256Hex(bytes) {
  const state = new Uint32Array([
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
  ]);
  const length = bytes.length;
  const padded = Buffer.alloc((((length + 8) >> 6) + 1) << 6);
  bytes.copy(padded);
  padded[length] = 0x80;
  padded.writeUInt32BE(Math.floor((length * 8) / 0x100000000), padded.length - 8);
  padded.writeUInt32BE((length * 8) >>> 0, padded.length - 4);

  const w = new Uint32Array(64);
  for (let offset = 0; offset < padded.length; offset += 64) {
    for (let i = 0; i < 16; i += 1) w[i] = padded.readUInt32BE(offset + i * 4);
    for (let i = 16; i < 64; i += 1) {
      const x = w[i - 15];
      const y = w[i - 2];
      const s0 = rotr32(x, 7) ^ rotr32(x, 18) ^ (x >>> 3);
      const s1 = rotr32(y, 17) ^ rotr32(y, 19) ^ (y >>> 10);
      w[i] = (w[i - 16] + s0 + w[i - 7] + s1) >>> 0;
    }
    let a = state[0];
    let b = state[1];
    let c = state[2];
    let d = state[3];
    let e = state[4];
    let f = state[5];
    let g = state[6];
    let h = state[7];
    for (let i = 0; i < 64; i += 1) {
      const S1 = rotr32(e, 6) ^ rotr32(e, 11) ^ rotr32(e, 25);
      const ch = (e & f) ^ (~e & g);
      const t1 = (h + S1 + ch + SHA256_K[i] + w[i]) >>> 0;
      const S0 = rotr32(a, 2) ^ rotr32(a, 13) ^ rotr32(a, 22);
      const maj = (a & b) ^ (a & c) ^ (b & c);
      const t2 = (S0 + maj) >>> 0;
      h = g;
      g = f;
      f = e;
      e = (d + t1) >>> 0;
      d = c;
      c = b;
      b = a;
      a = (t1 + t2) >>> 0;
    }
    state[0] = (state[0] + a) >>> 0;
    state[1] = (state[1] + b) >>> 0;
    state[2] = (state[2] + c) >>> 0;
    state[3] = (state[3] + d) >>> 0;
    state[4] = (state[4] + e) >>> 0;
    state[5] = (state[5] + f) >>> 0;
    state[6] = (state[6] + g) >>> 0;
    state[7] = (state[7] + h) >>> 0;
  }
  let out = '';
  for (let i = 0; i < 8; i += 1) out += state[i].toString(16).padStart(8, '0');
  return out;
}

// ---------------------------------------------------------------------------
// PNG：只认这一种形态（8 位 RGBA、非隔行），也正是 core 的编码器写出来的形态
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
      return pa <= pb && pa <= pc ? left : pb <= pc ? up : upLeft;
    }
    default:
      throw new Error(`不认识的滤波器类型 ${filter}`);
  }
}

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
export function encodePng(width, height, rgba, filter = 0) {
  const bpp = 4;
  const stride = width * bpp;
  const raw = Buffer.alloc(height * (stride + 1));
  for (let y = 0; y < height; y += 1) {
    raw[y * (stride + 1)] = filter;
    for (let x = 0; x < stride; x += 1) {
      const value = rgba[y * stride + x];
      const left = x >= bpp ? rgba[y * stride + x - bpp] : 0;
      const up = y > 0 ? rgba[(y - 1) * stride + x] : 0;
      const upLeft = y > 0 && x >= bpp ? rgba[(y - 1) * stride + x - bpp] : 0;
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

// ---------------------------------------------------------------------------
// 指标（luma / SSIM / PSNR / 差异统计 / 差异形状）
//
// 算术次序在这里是**契约的一部分**，不是风格：守卫要拿自己重算的数去逐字节复算
// `summary.csv` 与 `verdict.json` 里的浮点文本（`String(double)` 是 17 位有效数字
// 的舍入输出，1 ulp 的差别就换一个字符串）。所以公式、求和次序、窗口卷积的次序
// 都照规格写死——换任何一个次序，浮点就会换一个值，文本就不等了。
//
// 这不算"抄一份实现"：次序与公式是工具对外承诺的**定义**（记录里的数是"这个算法"
// 的结果），而解码器、摘要、TOML 解析、渲染这些**可自由发挥的地方全部另写**。
// 自检里有"两张全等图必须恰好得到 SSIM 1、PSNR inf"和"差 1 LSB 的图必须得到
// 有限值"这两组锚，用来抓"守卫自己把公式写错了"。
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

export function assertSameSize(a, b) {
  if (a.width !== b.width || a.height !== b.height) {
    throw new Error(`尺寸不同：${a.width}×${a.height} vs ${b.width}×${b.height}`);
  }
}

/** 全图 SSIM（luma）。全等的两张图**恰好**是 1——公式的写法保证了这点。 */
export function ssimLuma(a, b) {
  assertSameSize(a, b);
  if (a.width < SSIM_WINDOW || a.height < SSIM_WINDOW) {
    throw new Error(`图像比 SSIM 窗口还小（${a.width}×${a.height} < ${SSIM_WINDOW}×${SSIM_WINDOW}），算不了`);
  }
  const x = lumaPlane(a);
  const y = lumaPlane(b);
  const n = x.length;
  const xx = new Float64Array(n);
  const yy = new Float64Array(n);
  const xy = new Float64Array(n);
  for (let i = 0; i < n; i += 1) {
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
 * 以及**原始 |Δ| 的 RGBA 图**（有差异才建）。
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

// ---------------------------------------------------------------------------
// 差异的空间形状（只出事实、不出结论；判断在 plan/wgsl-portable-subset.md）
//
// 口径（与工具逐字一致，否则 shape.json 逐字节比不起来）：
//   · per_channel / magnitude_at_least_2 / sign_quadrant 数的是**通道实例**；
//   · diff_pixels / full_height_columns / bbox 数的是**像素**；
//   · 有符号差是 a − b。
// ---------------------------------------------------------------------------

const CHANNELS = ['R', 'G', 'B', 'A'];
const QUADRANTS = ['topLeft', 'topRight', 'bottomLeft', 'bottomRight'];

export function pixelDiffShape(a, b) {
  assertSameSize(a, b);
  const { width, height } = a;
  const halfW = Math.floor(width / 2);
  const halfH = Math.floor(height / 2);

  const perChannel = Object.fromEntries(CHANNELS.map((c) => [c, { plus: 0, minus: 0 }]));
  const magnitudeAtLeast2 = Object.fromEntries(CHANNELS.map((c) => [c, 0]));
  const signQuadrant = Object.fromEntries(QUADRANTS.map((q) => [q, { plus: 0, minus: 0 }]));
  const deltaCounts = new Map();
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

  const fullHeightColumns = [];
  for (let col = 0; col < width; col += 1) if (colDiffs[col] === height) fullHeightColumns.push(col);
  const fullWidthRows = [];
  for (let row = 0; row < height; row += 1) if (rowDiffs[row] === width) fullWidthRows.push(row);
  const fullHeightSet = new Set(fullHeightColumns);

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

/** 逐帧形状 → 逐场景汇总。并集与逐帧是两件事，两个都记。 */
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
      sign_quadrant: Object.fromEntries(QUADRANTS.map((q) => [
        q,
        {
          plus: sum((s) => s.sign_quadrant[q].plus),
          minus: sum((s) => s.sign_quadrant[q].minus),
        },
      ])),
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

// ---------------------------------------------------------------------------
// 配对：名字契约、集合相等、帧号连续
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

/** 清单不合规就一次说清（**不做**"取交集继续跑"这种静默调解）。 */
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
  if (pairing.onlyA.length > 0) throw new Error(`${labels.a} 有而 ${labels.b} 没有：${show(pairing.onlyA)}`);
  if (pairing.onlyB.length > 0) throw new Error(`${labels.b} 有而 ${labels.a} 没有：${show(pairing.onlyB)}`);
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

// ---------------------------------------------------------------------------
// 阈值档位（TOML 子集）：未知键与未知节一律红
//
// 拼错的键若被静默忽略，这道闸等于没设。所以这里只吃 `键 = 数` 与两种节，
// 别的一律报错——而且是**自己重新解析**，不引工具的解析器。
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

/** 一个场景的档位 = [default] 打底、[scenario.<名字>] 覆盖；缺两条 SSIM 基线就红。 */
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
// 判定与渲染
//
// 渲染出来的文本是要**逐字节**对账的（summary.csv / verdict.json / shape.json /
// report.txt 都在记录里）。所以这里不只是"算出同样的数"，而是"拼出同样的字节"。
// ---------------------------------------------------------------------------

function num(value) {
  return Number.isFinite(value) ? String(value) : 'inf';
}

function csvField(value) {
  const text = String(value);
  return /[",\n]/.test(text) ? `"${text.replaceAll('"', '""')}"` : text;
}

/** 逐场景汇总（不含判定）。 */
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

/** 每条闸单独判，失败的**点名那一条、那几个数**。 */
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

/** |Δ| × amp、clamp 到 255，alpha 一律 255。 */
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

/**
 * 报告文本。记录里的 `report.txt` 与真跑时屏幕上打印的是**同一份文本**（一个函数出）。
 * 守卫这里也一样：只出这一份，屏幕与比对都用它。
 */
export function buildReport({ analysis, judged, labels, shown, outRel, thresholdsInfo }) {
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
  const diffImages = [...analysis.diffRgbaByName.keys()].sort();
  const extra = diffImages.length > 0 ? `，另 diff/ 下 ${diffImages.length} 张放大差异图` : '';
  lines.push(`  记录 → ${outRel}/summary.csv、${outRel}/verdict.json、${outRel}/report.txt${extra}`);
  if (judged.verdict === 'pass') {
    lines.push(`\n✓ ${judged.scenes.length} 个场景、${analysis.frames.length} 帧全部达标（退出码 0）`);
  } else {
    lines.push(`\n✗ 有场景不达标（退出码 1）——不达标项与档位都在上面的失败行里`);
  }
  return `${lines.join('\n')}\n`;
}

// ---------------------------------------------------------------------------
// 从磁盘读成模型
//
// 所有检查函数都是**纯函数**（输入 = 模型）：真跑时模型从磁盘构造，自检时从合成
// 数据构造——同一条检查代码路径，不会出现"自检测的是另一套逻辑"。M1 的守卫用
// `syntheticLeg()` 是同一个道理。
// ---------------------------------------------------------------------------

const PNG_NAME = /\.png$/i;

function readBytesIfPresent(path) {
  try {
    return readFileSync(path);
  } catch {
    return null;
  }
}

/** JSON 文件 → `{ value }` / `{ error }` / `null`（文件不在）。 */
export function readJsonIfPresent(path) {
  const bytes = readBytesIfPresent(path);
  if (bytes === null) return null;
  try {
    return { value: JSON.parse(bytes.toString('utf8')) };
  } catch (error) {
    return { error: `JSON 解不开：${error.message}` };
  }
}

/**
 * 一条腿（`records/m2/<slug>`，或 `records/m1/dx12` 那样的 native 归档）。
 *
 * 只负责**读**：读不到的东西存 `{ error }` 或 `null`，判定交给检查项——
 * 读的时候就把"读不到"变成"过"，正是守卫最该避免的事。
 */
export function loadLeg(dir, dirRel, slug) {
  const rootFiles = new Set();
  const fileBytes = new Map();
  const frameNames = [];
  const frameBytes = new Map();
  const ignoredFrames = [];

  if (existsSync(dir)) {
    for (const entry of readdirSync(dir, { withFileTypes: true })) {
      if (!entry.isFile()) continue;
      rootFiles.add(entry.name);
      fileBytes.set(entry.name, statSync(join(dir, entry.name)).size);
    }
    const framesDir = join(dir, 'frames');
    if (existsSync(framesDir)) {
      for (const entry of readdirSync(framesDir, { withFileTypes: true })) {
        if (!entry.isFile()) continue;
        if (!PNG_NAME.test(entry.name)) {
          // 非 PNG 的文件照记：比对工具的 `ignored` 一栏要说出"还有谁没参与比对"，
          // 守卫这几栏也得说同一句话（否则复算出来的 verdict.json 少一个字段的来源）。
          ignoredFrames.push(entry.name);
          continue;
        }
        frameNames.push(entry.name);
        frameBytes.set(entry.name, readFileSync(join(framesDir, entry.name)));
      }
      frameNames.sort();
      ignoredFrames.sort();
    }
  }

  return {
    slug,
    dir,
    dirRel,
    rootFiles,
    fileBytes,
    frameNames,
    frameBytes,
    ignoredFrames,
    run: readJsonIfPresent(join(dir, 'run.json')),
    adapter: readJsonIfPresent(join(dir, 'adapter.json')),
    hostGpu: readJsonIfPresent(join(dir, 'host-gpu.json')),
    screenshot: readJsonIfPresent(join(dir, 'screenshot-browser-corpus.json')),
    screenshotPng: readBytesIfPresent(join(dir, 'screenshot-browser-corpus.png')),
    readingsBytes: readBytesIfPresent(join(dir, 'readings.txt')),
    rerunRepro: readJsonIfPresent(join(dir, 'rerun-repro.json')),
    // 这几份的**字节**单独留一份：跨腿判定里有两条是"两份文件必须不同"，
    // 那是关于字节的话，不能拿结构比较代替（两份结构相同、值不同的文件也满足结构比较）。
    runBytes: readBytesIfPresent(join(dir, 'run.json')),
    adapterBytes: readBytesIfPresent(join(dir, 'adapter.json')),
    hostGpuBytes: readBytesIfPresent(join(dir, 'host-gpu.json')),
  };
}

/**
 * 解一帧（同一份字节只解一次）。SSIM 要对 160 个帧对重算，而 native 那 80 张
 * 在两组 framediff 里都要用到——重复解码只是让守卫变慢，不会让它更可信。
 */
const IMAGE_CACHE = new WeakMap();
export function frameImage(leg, name) {
  let cache = IMAGE_CACHE.get(leg);
  if (cache === undefined) {
    cache = new Map();
    IMAGE_CACHE.set(leg, cache);
  }
  if (!cache.has(name)) {
    const bytes = leg.frameBytes.get(name);
    let entry;
    if (bytes === undefined) {
      entry = { error: `${name} 不在这条腿的 frames/ 里` };
    } else {
      try {
        entry = { image: decodePng(bytes) };
      } catch (error) {
        entry = { error: `${name} 解不开：${error.message}` };
      }
    }
    cache.set(name, entry);
  }
  return cache.get(name);
}

/** framediff 的输入是"一批帧"，不是整条腿。 */
export function framesOf(leg) {
  return { dirRel: `${leg.dirRel}/frames`, names: leg.frameNames, bytes: leg.frameBytes };
}

// ---------------------------------------------------------------------------
// 帧集合摘要（corpus 表的整表摘要）
//
// 契约（`dhampir-core` 的 `frames_digest`）：按**表序**拼
// `场景名 + 0x00 + 帧号 LE u32 + 像素摘要 LE u64`，整体再 FNV-1a 64。
// 这里用自己算出来的像素摘要重组，所以"整表摘要"这一栏不是抄的。
// ---------------------------------------------------------------------------

export function framesDigest(entries) {
  let state = FNV_OFFSET_BASIS;
  const scratch = Buffer.alloc(12);
  for (const entry of entries) {
    state = fnv1a64Update(state, Buffer.from(entry.scene, 'utf8'));
    state = fnv1a64Update(state, Buffer.from([0]));
    scratch.writeUInt32LE(entry.frame, 0);
    scratch.writeBigUInt64LE(BigInt(`0x${entry.pixelDigest}`), 4);
    state = fnv1a64Update(state, scratch);
  }
  return fnv1a64Hex(state);
}

// ---------------------------------------------------------------------------
// readings.txt 的逐行渲染
//
// 契约住在生成侧（`crates/dhampir-core/src/render/corpus.rs` 的 `report_text` 与
// `render/scene.rs` 的 `SampleVerdict::report_line`）。这里照着重写一份：只有自己
// 再拼一遍，"给人看的那份读数是真的"才有人回答。
//
// 三个实测抓出来的坑：
//   · Rust 的 `{:<14}` 按 **Unicode 标量**算宽度，JS 的 `String.length` 按 UTF-16
//     码元算。采样点标签是中文，用 `String.length` 会**每一行**都错位——3 万多处不一致，
//     而"差在哪"看不出来。所以填充一律按 `[...s].length`。
//   · 帧标题用 `{:03}`（`f000`），逐点行的帧号用 `{:<3}`（`f0  `）——同一个数、两种填法，
//     不是笔误。
//   · 文件以**两个空行**收尾（最后一帧块尾的空行 + join 出的那个换行）。
//
// 越界 / 未判定两条支路在这份记录里取不到（`counts.out_of_range` 与 `counts.unjudged`
// 都是 0），它们是**照源码写的、没有被逐字节验过**。所以「readings」这一项会另外把
// "这两支确实是 0 条"也报出来：真走到那两支，问题就不是"渲染对不对"了。
// ---------------------------------------------------------------------------

const codePointLength = (text) => [...String(text)].length;

function pad(text, width, align = 'left') {
  const s = String(text);
  const n = codePointLength(s);
  if (n >= width) return s;
  const fill = ' '.repeat(width - n);
  return align === 'right' ? fill + s : s + fill;
}

export function renderReadings(run) {
  const backend = run.backends[0];
  const counts = backend.counts;
  const lines = ['dhampir M1 corpus 逐点读数'];
  lines.push(
    `帧 ${counts.frames}、采样点 ${counts.points}、失败 ${counts.failed}、越界 ${counts.out_of_range}、`
      + `未判定 ${counts.unjudged}；容差 ${run.byte_tolerance} 字节`,
  );
  lines.push('');
  const sceneByName = new Map(run.scenes.map((scene) => [scene.name, scene]));
  const rgba = (values) => [0, 1, 2, 3].map((i) => pad(values[i], 3, 'right')).join(' ');
  for (const frame of backend.frames) {
    const scene = sceneByName.get(frame.scene);
    const repeat = frame.repeat_identical
      ? '逐字节一致'
      : `不一致：${frame.pixel_digest} vs ${frame.repeat_pixel_digest}`;
    lines.push(
      `--- ${frame.scene} f${String(frame.frame).padStart(3, '0')} （${scene.description}；同帧两次渲染${repeat}）`,
    );
    for (const point of frame.points) {
      const head = `${pad(frame.scene, 12)} f${pad(frame.frame, 3)}`
        + ` (${pad(point.x, 3, 'right')},${pad(point.y, 3, 'right')}) ${pad(point.label, 14)}`;
      const measured = point.measured;
      if (measured === null || measured === undefined) {
        lines.push(`${head} 越界——采样表里的坐标落在图像外`);
        continue;
      }
      if (!frame.repeat_identical) {
        lines.push(`${head} 实测 ${rgba(measured)} | 未判定：同帧两次渲染结果不一致`);
        continue;
      }
      lines.push(
        `${head} 实测 ${rgba(measured)} | 模型 ${rgba(point.expected)}`
          + ` | 距离 ${point.distance} 容差 ${point.tolerance} ${point.passed ? '通过' : '失败'} — ${point.detail}`,
      );
    }
    lines.push('');
  }
  return `${lines.join('\n')}\n`;
}

// ---------------------------------------------------------------------------
// 记录形状：键集合
//
// 逐份列全（而不是"有那么几项就行"）：多一个键、少一个键都要红。M2 的浏览器腿与 M1 的
// native 腿产出的文件**故意不一样**（浏览器不出 timing.json / compare.json，
// adapter.json 多出 in_page / gpu_identity / producer 等），所以不能把 M1 的形状往上套：
// 套上去的结果只会是"要么恒红、要么把真的缺口放过"。
// ---------------------------------------------------------------------------

const KEYS = {
  runTop: [
    'artifacts', 'backends', 'byte_tolerance', 'frame_range', 'frames_per_scene', 'kind',
    'milestone', 'nondeterministic_fields', 'scenes', 'schema', 'target_format', 'target_size',
  ],
  backend: ['adapter_name', 'counts', 'frames', 'frames_digest', 'repeat_mismatches', 'requested'],
  frame: [
    'frame', 'pixel_digest', 'png', 'png_bytes', 'png_digest', 'points', 'repeat_identical',
    'repeat_pixel_digest', 'scene',
  ],
  point: ['detail', 'distance', 'expected', 'label', 'measured', 'passed', 'purpose', 'tolerance', 'x', 'y'],
  scene: ['description', 'fragment_entries', 'name', 'passes', 'samples', 'size', 'uses_frame'],
  sample: ['expected', 'label', 'purpose', 'x', 'y'],
  counts: ['clean', 'failed', 'frames', 'out_of_range', 'points', 'unjudged'],
  artifacts: ['adapter', 'frame_count', 'frames_dir', 'readings'],
  adapter: [
    'adapter', 'adapter_name', 'backend_slug', 'build_profile', 'corpus_target_size', 'crate_version',
    'gpu_identity', 'in_page', 'kind', 'milestone', 'naga_version', 'naga_version_note',
    'nondeterministic_fields', 'probe_digest', 'probe_format_version', 'producer',
    'requested_backends', 'schema', 'target_format', 'timing_note', 'timing_record',
    'unix_epoch_millis', 'unix_epoch_seconds', 'wgpu_version',
  ],
  adapterPort: ['backend', 'device', 'device_type', 'driver', 'driver_info', 'name', 'vendor'],
  inPage: [
    'architecture', 'description', 'device', 'is_fallback_adapter', 'note', 'subgroup_max_size',
    'subgroup_min_size', 'vendor',
  ],
  gpuIdentity: ['reason', 'resolved_by', 'resolves_to', 'state'],
  hostGpu: ['source', 'devices', 'resolved', 'note'],
  device: [
    'driverVendor', 'driverVersion', 'deviceId', 'deviceString', 'revision', 'subSysId',
    'vendorId', 'vendorString',
  ],
  resolved: ['device', 'device_id', 'driver_vendor', 'driver_version', 'match_reason', 'vendor', 'vendor_id'],
  hostGpuInShot: ['devices', 'on_disk', 'post', 'read_from', 'source'],
  screenshotTop: [
    'audit', 'browser', 'captured_at', 'host_gpu', 'leg', 'milestone', 'notes', 'page_url',
    'purpose', 'round', 'schema', 'screenshot', 'server_checks', 'server_findings', 'trigger', 'viewport',
  ],
  screenshotLeg: [
    'adapter_name', 'backend_slug', 'build_profile', 'naga_version', 'out_dir', 'requested_backends',
    'slug', 'wgpu_version',
  ],
  screenshotBrowser: ['extra_args', 'headless', 'kind', 'path', 'product', 'revision', 'user_agent'],
  viewport: ['captured_content', 'height', 'width'],
  round: [
    'adapter_bytes', 'adapter_bytes_on_disk', 'counts', 'digest_matches_expect', 'elapsed_ms',
    'expected_frames_digest', 'frames_arg', 'frames_digest', 'frames_on_disk', 'frames_verified',
    'frames_written', 'png_bytes_total', 'readings_bytes', 'readings_bytes_on_disk', 'run_json_bytes',
    'run_json_bytes_on_disk', 'scene',
  ],
  screenshotFile: ['bytes', 'file', 'sha256'],
  checkItem: ['detail', 'ok'],
  audit: ['findings', 'hard', 'ok'],
  trigger: ['autorun_suppressed', 'by', 'why'],
  verdictTop: [
    'amp', 'exit_code', 'inputs', 'kind', 'schema', 'scenes', 'thresholds', 'tool', 'totals',
    'verdict', 'window',
  ],
  verdictInput: ['bytes_total', 'dir', 'frames', 'ignored', 'label', 'set_digest'],
  verdictScene: [
    'diff_pixels', 'failures', 'frames', 'limits', 'max_abs_diff', 'mean_ssim_luma',
    'min_psnr_rgb_db', 'min_ssim_luma', 'scene', 'verdict', 'worst_frames',
  ],
  shapeTop: ['inputs', 'kind', 'note', 'scenes', 'schema', 'signed_delta_definition', 'thresholds', 'tool'],
  shapeInput: ['bytes_total', 'dir', 'frames', 'label', 'set_digest'],
  rerunRepro: ['first', 'frames', 'kind', 'leg', 'made_by', 'not_compared', 'question', 'readings', 'rerun', 'schema'],
  rerunRun: ['frames_digest', 'out_dir', 'png_bytes_total'],
  rerunFrames: ['byte_identical', 'compared', 'mismatches'],
  rerunReadings: ['first_bytes', 'first_sha256_16', 'identical', 'rerun_bytes', 'rerun_sha256_16'],
};

/** 键集合必须**逐字相等**。返回空数组 = 通过。 */
function exactKeys(where, object, expected) {
  if (object === null || typeof object !== 'object' || Array.isArray(object)) {
    return [`${where} 不是对象（${JSON.stringify(object)}）`];
  }
  const actual = Object.keys(object);
  const missing = expected.filter((key) => !actual.includes(key));
  const extra = actual.filter((key) => !expected.includes(key));
  if (missing.length === 0 && extra.length === 0) return [];
  const parts = [];
  if (missing.length > 0) parts.push(`缺 ${missing.join('、')}`);
  if (extra.length > 0) parts.push(`多了 ${extra.join('、')}`);
  return [`${where} 的键不对：${parts.join('；')}`];
}

/**
 * 一条腿的检查项清单。`checkLegModel` 保证**每一项都有一条结论**——
 * 检查被删掉时表现为"项少了"，而不是"项还在但永远绿"。
 */
export const LEG_CHECKS = [
  'required-files',
  'run-shape',
  'frame-set',
  'png-bytes-and-digest',
  'pixels',
  'repeat',
  'frames-digest',
  'counts',
  'points',
  'measured-vs-png',
  'readings',
  'adapter',
  'host-gpu',
  'screenshot',
  'rerun-repro',
];

/** 浏览器腿必须有的东西；以及**必须没有**的东西（M1 的腿有、浏览器腿不出）。 */
const LEG_FILES = [
  'adapter.json',
  'host-gpu.json',
  'readings.txt',
  'run.json',
  'screenshot-browser-corpus.json',
  'screenshot-browser-corpus.png',
];

const LEG_FORBIDDEN_FILES = ['timing.json', 'compare.json'];

/** 一张帧的名字：`<场景>-f<三位帧号>.png`。 */
function frameNameOf(scene, frame) {
  return `${scene}-f${String(frame).padStart(3, '0')}.png`;
}

function expectedFrameNames(spec) {
  const names = [];
  for (const scene of spec.scenes) {
    for (let frame = 0; frame < spec.framesPerScene; frame += 1) names.push(frameNameOf(scene, frame));
  }
  return names;
}

/** 逐帧逐点摊平。检查项里反复要用，且必须与 run.json 的顺序一致。 */
function allPoints(backend) {
  return backend.frames.flatMap((frame) => frame.points.map((point) => ({ frame, point })));
}

const CHECKS = {
  'required-files': (leg) => {
    const messages = [];
    for (const name of LEG_FILES) if (!leg.rootFiles.has(name)) messages.push(`缺 ${name}`);
    for (const name of LEG_FORBIDDEN_FILES) {
      if (leg.rootFiles.has(name)) {
        messages.push(`多出 ${name}——浏览器腿不出这份（adapter.json 的 timing_note 写了理由），出现它说明这条目录被别的轮次污染了`);
      }
    }
    const known = new Set([...LEG_FILES, 'rerun-repro.json']);
    const extra = [...leg.rootFiles].filter((name) => !known.has(name)).sort();
    if (extra.length > 0) messages.push(`根目录多出没见过的文件：${extra.join('、')}`);
    if (leg.frameNames.length === 0) messages.push('frames/ 里一张 PNG 都没有');
    if (leg.ignoredFrames.length > 0) {
      messages.push(`frames/ 里混进非 PNG：${leg.ignoredFrames.slice(0, 4).join('、')}`);
    }
    return messages;
  },

  'run-shape': (leg, spec) => {
    const run = leg.run?.value;
    if (run === undefined) return ['run.json 读不到或解不开'];
    const messages = [...exactKeys('run.json 顶层', run, KEYS.runTop)];
    if (run.schema !== 1) messages.push(`schema=${JSON.stringify(run.schema)}`);
    if (run.kind !== 'corpus') messages.push(`kind=${JSON.stringify(run.kind)}`);
    // milestone 是**记录契约版本**，不是跑的时刻：M2 的浏览器腿照样写 M1，因为表契约没变。
    // 这条要是被当成"写错了"去"修正"，M1 守卫与 M2 守卫就有一份对不上了。
    if (run.milestone !== 'M1') messages.push(`milestone=${JSON.stringify(run.milestone)}（表契约版本，M2 也写 M1）`);
    if (run.frames_per_scene !== spec.framesPerScene) messages.push(`frames_per_scene=${run.frames_per_scene}`);
    if (run.frame_range !== spec.frameRange) messages.push(`frame_range=${JSON.stringify(run.frame_range)}`);
    if (run.target_size !== spec.targetSize) messages.push(`target_size=${JSON.stringify(run.target_size)}`);
    if (run.target_format !== spec.targetFormat) messages.push(`target_format=${JSON.stringify(run.target_format)}`);
    if (run.byte_tolerance !== spec.byteTolerance) messages.push(`byte_tolerance=${run.byte_tolerance}`);
    if (!Array.isArray(run.nondeterministic_fields) || run.nondeterministic_fields.length !== 0) {
      messages.push('nondeterministic_fields 不是空数组——表里只要有一个字段不确定，"两批字节一样"就无从说起');
    }

    messages.push(...exactKeys('run.json 的 artifacts', run.artifacts, KEYS.artifacts));
    if (run.artifacts?.adapter !== 'adapter.json') messages.push(`artifacts.adapter=${JSON.stringify(run.artifacts?.adapter)}`);
    if (run.artifacts?.frame_count !== spec.frames) messages.push(`artifacts.frame_count=${run.artifacts?.frame_count}`);
    if (run.artifacts?.frames_dir !== 'frames') messages.push(`artifacts.frames_dir=${JSON.stringify(run.artifacts?.frames_dir)}`);
    if (run.artifacts?.readings !== 'readings.txt') messages.push(`artifacts.readings=${JSON.stringify(run.artifacts?.readings)}`);

    if (!Array.isArray(run.backends) || run.backends.length !== 1) {
      messages.push(`backends 有 ${run.backends?.length} 条——这份记录是"每条腿一条"，多一条就说明混了别的腿`);
      return messages;
    }
    const backend = run.backends[0];
    messages.push(...exactKeys('backends[0]', backend, KEYS.backend));
    if (backend.requested !== spec.requestedBackends) messages.push(`backends[0].requested=${JSON.stringify(backend.requested)}`);
    // 浏览器读不出适配器名字（adapter.name 是空串），所以这栏只能是 null。
    // 哪天它变成一个真名字，要问的是"谁把它填进去的"。
    if (backend.adapter_name !== null) messages.push(`backends[0].adapter_name=${JSON.stringify(backend.adapter_name)}（浏览器读不出名字，只能是 null）`);

    if (!Array.isArray(run.scenes)) {
      messages.push('run.scenes 不是数组');
      return messages;
    }
    const names = run.scenes.map((scene) => scene.name);
    if (names.join(',') !== spec.scenes.join(',')) {
      messages.push(`场景表是 [${names.join('、')}]，锚是 [${spec.scenes.join('、')}]`);
    }
    for (const scene of run.scenes) {
      messages.push(...exactKeys(`scenes[${scene.name}]`, scene, KEYS.scene));
      if (scene.size !== spec.targetSize) messages.push(`scenes[${scene.name}].size=${JSON.stringify(scene.size)}`);
      if (typeof scene.uses_frame !== 'boolean') messages.push(`scenes[${scene.name}].uses_frame 不是布尔值`);
      if (typeof scene.description !== 'string' || scene.description === '') messages.push(`scenes[${scene.name}].description 是空的`);
      if (typeof scene.passes !== 'number' || scene.passes < 1) messages.push(`scenes[${scene.name}].passes=${JSON.stringify(scene.passes)}`);
      if (!Array.isArray(scene.fragment_entries) || scene.fragment_entries.length !== scene.passes) {
        messages.push(`scenes[${scene.name}] 的 fragment_entries 有 ${scene.fragment_entries?.length} 条，passes 是 ${scene.passes}`);
      }
      if (!Array.isArray(scene.samples) || scene.samples.length === 0) {
        messages.push(`scenes[${scene.name}].samples 是空的`);
        continue;
      }
      for (const sample of scene.samples) {
        messages.push(...exactKeys(`scenes[${scene.name}].samples[${sample.label}]`, sample, KEYS.sample));
      }
    }
    const sampleTotal = run.scenes.reduce((sum, scene) => sum + (scene.samples?.length ?? 0), 0);
    if (sampleTotal !== spec.points / spec.framesPerScene) {
      messages.push(`每帧采样点数 ${sampleTotal} × ${spec.framesPerScene} 帧 ≠ 记录里的 ${spec.points} 个点`);
    }
    return messages;
  },

  'frame-set': (leg, spec) => {
    const messages = [];
    const backend = leg.run?.value?.backends?.[0];
    if (backend === undefined) return ['run.json 里没有 backends[0]，无从对帧清单'];
    const want = expectedFrameNames(spec);
    const actual = leg.frameNames;
    if (actual.length !== want.length) messages.push(`frames/ 里 ${actual.length} 张 PNG，锚是 ${want.length} 张`);
    const missing = want.filter((name) => !actual.includes(name));
    const extra = actual.filter((name) => !want.includes(name));
    if (missing.length > 0) messages.push(`缺 ${missing.length} 张：${missing.slice(0, 6).join('、')}`);
    if (extra.length > 0) messages.push(`多了 ${extra.length} 张：${extra.slice(0, 6).join('、')}`);

    const listed = backend.frames.map((frame) => frame.png);
    const wantListed = want.map((name) => `frames/${name}`);
    if (listed.join(',') !== wantListed.join(',')) {
      const first = listed.findIndex((name, i) => name !== wantListed[i]);
      messages.push(
        `run.json 的帧表与锚对不上（共 ${listed.length} 条，首个不同在第 ${first} 条：`
          + `${JSON.stringify(listed[first])} vs ${JSON.stringify(wantListed[first])}）`,
      );
    }
    // 帧号必须**场景内**从 0 起连续、且与场景表同序——被截断的一批不能伪装成一批。
    const byScene = new Map();
    for (const frame of backend.frames) {
      if (!byScene.has(frame.scene)) byScene.set(frame.scene, []);
      byScene.get(frame.scene).push(frame.frame);
    }
    const sceneOrder = [...byScene.keys()];
    if (sceneOrder.join(',') !== spec.scenes.join(',')) {
      messages.push(`帧表里的场景出现顺序是 [${sceneOrder.join('、')}]，锚是 [${spec.scenes.join('、')}]`);
    }
    for (const [scene, frames] of byScene) {
      if (frames.length !== spec.framesPerScene) {
        messages.push(`场景 ${scene} 有 ${frames.length} 帧，锚是 ${spec.framesPerScene}`);
      }
      const bad = frames.filter((frame, i) => frame !== i);
      if (bad.length > 0) messages.push(`场景 ${scene} 的帧号不从 0 起连续：${bad.slice(0, 6).join(', ')}`);
    }
    return messages;
  },

  'png-bytes-and-digest': (leg, spec) => {
    const messages = [];
    const backend = leg.run?.value?.backends?.[0];
    if (backend === undefined) return ['run.json 里没有 backends[0]'];
    let bytesTotal = 0;
    for (const frame of backend.frames) {
      const name = String(frame.png).replace(/^frames\//, '');
      const bytes = leg.frameBytes.get(name);
      if (bytes === undefined) {
        messages.push(`${name} 不在 frames/ 里`);
        continue;
      }
      bytesTotal += bytes.length;
      if (frame.png_bytes !== bytes.length) {
        messages.push(`${name}：png_bytes 写 ${frame.png_bytes}，盘上是 ${bytes.length}`);
      }
      const digest = fnv1a64(bytes);
      if (frame.png_digest !== digest) {
        messages.push(`${name}：png_digest 写 ${JSON.stringify(frame.png_digest)}，重算是 ${digest}`);
      }
    }
    if (bytesTotal !== spec.pngBytes) {
      messages.push(`80 张 PNG 加起来 ${bytesTotal} 字节，锚是 ${spec.pngBytes} 字节`);
    }
    return messages;
  },

  pixels: (leg) => {
    const messages = [];
    const backend = leg.run?.value?.backends?.[0];
    if (backend === undefined) return ['run.json 里没有 backends[0]'];
    for (const frame of backend.frames) {
      const name = String(frame.png).replace(/^frames\//, '');
      const image = frameImage(leg, name);
      if (image.error !== undefined) {
        messages.push(`${name}：${image.error}`);
        continue;
      }
      // 像素摘要口径：解码后的 **RGBA 字节**（PNG 里存的就是渲染出来的那些字节，
      // 没经过任何线性化）再过一遍 FNV-1a 64。这一条是"整表摘要不是抄来的"的地基。
      const digest = fnv1a64(image.image.pixels);
      if (frame.pixel_digest !== digest) {
        messages.push(`${name}：pixel_digest 写 ${JSON.stringify(frame.pixel_digest)}，从盘上像素重算是 ${digest}`);
      }
    }
    return messages;
  },

  repeat: (leg) => {
    const messages = [];
    const backend = leg.run?.value?.backends?.[0];
    if (backend === undefined) return ['run.json 里没有 backends[0]'];
    const bad = backend.frames.filter((frame) => frame.repeat_identical !== true);
    if (bad.length > 0) {
      messages.push(`有 ${bad.length} 帧声明"同帧两次渲染不一致"：${bad.slice(0, 4).map((f) => f.png).join('、')}`);
    }
    const drift = backend.frames.filter((frame) => frame.repeat_pixel_digest !== frame.pixel_digest);
    if (drift.length > 0) {
      messages.push(`有 ${drift.length} 帧的 repeat_pixel_digest ≠ pixel_digest：${drift.slice(0, 4).map((f) => f.png).join('、')}`);
    }
    if (!Array.isArray(backend.repeat_mismatches) || backend.repeat_mismatches.length !== 0) {
      messages.push(`repeat_mismatches 不是空数组（${JSON.stringify(backend.repeat_mismatches)?.slice(0, 80)}）`);
    }
    return messages;
  },

  'frames-digest': (leg, spec) => {
    const messages = [];
    const backend = leg.run?.value?.backends?.[0];
    if (backend === undefined) return ['run.json 里没有 backends[0]'];
    const recomputed = framesDigest(
      backend.frames.map((frame) => ({ scene: frame.scene, frame: frame.frame, pixelDigest: frame.pixel_digest })),
    );
    if (backend.frames_digest !== recomputed) {
      messages.push(`整表摘要写 ${JSON.stringify(backend.frames_digest)}，用逐帧像素摘要重拼是 ${recomputed}`);
    }
    if (recomputed !== spec.framesDigest) {
      messages.push(`重拼出来的整表摘要是 ${recomputed}，锚是 ${spec.framesDigest}`);
    }
    return messages;
  },

  counts: (leg, spec) => {
    const messages = [];
    const backend = leg.run?.value?.backends?.[0];
    if (backend === undefined) return ['run.json 里没有 backends[0]'];
    const counts = backend.counts;
    messages.push(...exactKeys('backends[0].counts', counts, KEYS.counts));
    if (counts?.clean !== true) messages.push(`counts.clean=${JSON.stringify(counts?.clean)}`);
    if (counts?.frames !== spec.frames) messages.push(`counts.frames=${counts?.frames}`);
    if (counts?.points !== spec.points) messages.push(`counts.points=${counts?.points}`);
    for (const key of ['failed', 'out_of_range', 'unjudged']) {
      if (counts?.[key] !== 0) messages.push(`counts.${key}=${counts?.[key]}（这一份记录里必须是 0）`);
    }
    const actualPoints = backend.frames.reduce((sum, frame) => sum + frame.points.length, 0);
    if (actualPoints !== counts?.points) messages.push(`逐帧点数加起来是 ${actualPoints}，counts.points 是 ${counts?.points}`);
    if (backend.frames.length !== counts?.frames) messages.push(`帧表有 ${backend.frames.length} 帧，counts.frames 是 ${counts?.frames}`);
    return messages;
  },

  points: (leg, spec) => {
    const messages = [];
    const run = leg.run?.value;
    const backend = run?.backends?.[0];
    if (backend === undefined) return ['run.json 里没有 backends[0]'];
    const sceneByName = new Map((run.scenes ?? []).map((scene) => [scene.name, scene]));

    for (const frame of backend.frames) {
      messages.push(...exactKeys(`frames[${frame.png}]`, frame, KEYS.frame));
      const scene = sceneByName.get(frame.scene);
      if (scene === undefined) {
        messages.push(`帧 ${frame.png} 的场景 ${frame.scene} 不在场景表里`);
        continue;
      }
      if (frame.points.length !== scene.samples.length) {
        messages.push(`${frame.png} 有 ${frame.points.length} 个采样点，场景表里是 ${scene.samples.length} 个`);
      }
      for (const point of frame.points) {
        messages.push(...exactKeys(`帧 ${frame.png} 的采样点 ${point.label}`, point, KEYS.point));
      }
    }

    // 第 0 帧的每个点必须**逐字段**等于场景表那一行：表是"应该看到什么"，
    // 帧表是"看到了什么"，两者错开就说明有人在事后改过其中一份。
    for (const scene of run.scenes ?? []) {
      const first = backend.frames.find((frame) => frame.scene === scene.name && frame.frame === 0);
      if (first === undefined) {
        messages.push(`场景 ${scene.name} 没有第 0 帧`);
        continue;
      }
      for (let i = 0; i < scene.samples.length; i += 1) {
        const sample = scene.samples[i];
        const point = first.points[i];
        if (point === undefined) continue;
        for (const key of ['label', 'purpose', 'x', 'y']) {
          if (point[key] !== sample[key]) {
            messages.push(`${scene.name} 第 0 帧第 ${i} 点：${key} 是 ${JSON.stringify(point[key])}，表里是 ${JSON.stringify(sample[key])}`);
          }
        }
        if (JSON.stringify(point.expected) !== JSON.stringify(sample.expected)) {
          messages.push(`${scene.name} 第 0 帧第 ${i} 点：expected 是 ${JSON.stringify(point.expected)}，表里是 ${JSON.stringify(sample.expected)}`);
        }
      }
    }

    // uses_frame 的两半都要能反过来印证：声明"随帧变"的就必须真的变，
    // 声明"不随帧"的就必须 16 帧一个样。只看一半的话，一个恒定的场景
    // 只要把 uses_frame 写成 true 就能蒙过去。
    for (const scene of run.scenes ?? []) {
      const list = (backend.frames ?? []).filter((frame) => frame.scene === scene.name);
      if (list.length === 0) continue;
      const signature = (frame) => JSON.stringify(frame.points.map((point) => point.expected));
      const same = list.every((frame) => signature(frame) === signature(list[0]));
      if (scene.uses_frame === true && same) {
        messages.push(`场景 ${scene.name} 声明 uses_frame=true，可 ${list.length} 帧的 expected 一模一样——随帧变的那一半没被验到`);
      }
      if (scene.uses_frame === false && !same) {
        messages.push(`场景 ${scene.name} 声明 uses_frame=false，可 expected 在 ${list.length} 帧里变了`);
      }
    }

    for (const { frame, point } of allPoints(backend)) {
      if (point.tolerance !== spec.byteTolerance) {
        messages.push(`${frame.png} 的 ${point.label}：tolerance=${JSON.stringify(point.tolerance)}`);
      }
      if (point.passed !== true) messages.push(`${frame.png} 的 ${point.label}：passed=${JSON.stringify(point.passed)}`);
      if (point.passed !== (point.distance <= point.tolerance)) {
        messages.push(`${frame.png} 的 ${point.label}：distance ${point.distance} 与 tolerance ${point.tolerance} 推不出 passed=${JSON.stringify(point.passed)}`);
      }
      if (!Array.isArray(point.measured) || point.measured.length !== 4) {
        messages.push(`${frame.png} 的 ${point.label}：measured 不是 4 个通道`);
      }
      if (!Array.isArray(point.expected) || point.expected.length !== 4) {
        messages.push(`${frame.png} 的 ${point.label}：expected 不是 4 个通道`);
      }
      if (typeof point.detail !== 'string' || point.detail === '') {
        messages.push(`${frame.png} 的 ${point.label}：detail 是空的——判定理由空缺的"通过"不算数`);
      }
    }
    return messages;
  },

  'measured-vs-png': (leg) => {
    const messages = [];
    const backend = leg.run?.value?.backends?.[0];
    if (backend === undefined) return ['run.json 里没有 backends[0]'];
    let checked = 0;
    for (const frame of backend.frames) {
      const name = String(frame.png).replace(/^frames\//, '');
      const image = frameImage(leg, name);
      if (image.error !== undefined) {
        messages.push(`${name}：${image.error}`);
        continue;
      }
      const { width, pixels } = image.image;
      for (const point of frame.points) {
        if (point.x < 0 || point.y < 0 || point.x >= width) {
          messages.push(`${name} 的 ${point.label}：坐标 (${point.x},${point.y}) 落在图像外`);
          continue;
        }
        const at = (point.y * width + point.x) * 4;
        const pixel = [...pixels.subarray(at, at + 4)];
        checked += 1;
        if (JSON.stringify(pixel) !== JSON.stringify(point.measured)) {
          messages.push(
            `${name} 的 ${point.label}：measured 写 ${JSON.stringify(point.measured)}，`
              + `同一张 PNG 在 (${point.x},${point.y}) 是 ${JSON.stringify(pixel)}`,
          );
        }
      }
    }
    // 一面"全过"的镜子也可能是空的：这一项必须能报出"我核了多少个点"，
    // 点数与记录对不上就是这一项自己没跑到位。
    if (checked === 0) messages.push('一个采样点都没核到');
    return messages;
  },

  readings: (leg, spec) => {
    const run = leg.run?.value;
    if (run === undefined) return ['run.json 读不到或解不开'];
    if (leg.readingsBytes === null) return ['readings.txt 不在'];
    const messages = [];
    const want = leg.readingsBytes.toString('utf8');
    let got;
    try {
      got = renderReadings(run);
    } catch (error) {
      return [`渲染 readings 时抛了异常：${error.message}`];
    }
    if (got !== want) {
      let i = 0;
      while (i < want.length && i < got.length && want[i] === got[i]) i += 1;
      messages.push(
        `重渲的 readings.txt 与盘上不一样：首差在第 ${i} 个字符（盘上 ${JSON.stringify(want.slice(i, i + 60))}，`
          + `重渲 ${JSON.stringify(got.slice(i, i + 60))}）`,
      );
    }
    if (leg.readingsBytes.length !== spec.readingsBytes) {
      messages.push(`readings.txt 有 ${leg.readingsBytes.length} 字节，锚是 ${spec.readingsBytes} 字节`);
    }
    const lines = want.split('\n').length;
    if (lines !== spec.readingsLines) messages.push(`readings.txt 分割出 ${lines} 行，锚是 ${spec.readingsLines} 行`);
    // 那两条"照源码写、没被逐字节验过"的支路：这一份记录里必须是 0 条。
    const counts = run.backends?.[0]?.counts;
    for (const key of ['out_of_range', 'unjudged']) {
      if (counts?.[key] !== 0) {
        messages.push(
          `counts.${key}=${counts?.[key]}：越界 / 未判定那两条支路的渲染没被逐字节验过，出现它们要人工看一眼`,
        );
      }
    }
    return messages;
  },

  adapter: (leg, spec) => {
    const adapter = leg.adapter?.value;
    if (adapter === undefined) return ['adapter.json 读不到或解不开'];
    const messages = [...exactKeys('adapter.json 顶层', adapter, KEYS.adapter)];
    if (adapter.schema !== 1) messages.push(`schema=${JSON.stringify(adapter.schema)}`);
    if (adapter.kind !== 'adapter') messages.push(`kind=${JSON.stringify(adapter.kind)}`);
    if (adapter.milestone !== 'M1') messages.push(`milestone=${JSON.stringify(adapter.milestone)}（记录契约版本）`);
    if (adapter.producer !== 'browser-webgpu/wasm32') messages.push(`producer=${JSON.stringify(adapter.producer)}`);
    if (adapter.requested_backends !== spec.requestedBackends) messages.push(`requested_backends=${JSON.stringify(adapter.requested_backends)}`);
    if (adapter.target_format !== spec.targetFormat) messages.push(`target_format=${JSON.stringify(adapter.target_format)}`);
    if (adapter.build_profile !== spec.buildProfile) messages.push(`build_profile=${JSON.stringify(adapter.build_profile)}`);
    if (adapter.corpus_target_size !== spec.targetSize) messages.push(`corpus_target_size=${JSON.stringify(adapter.corpus_target_size)}`);
    if (adapter.crate_version !== spec.crateVersion) messages.push(`crate_version=${JSON.stringify(adapter.crate_version)}`);
    if (adapter.wgpu_version !== spec.wgpuVersion) messages.push(`wgpu_version=${JSON.stringify(adapter.wgpu_version)}`);
    if (adapter.backend_slug !== spec.legSlugInRecord) messages.push(`backend_slug=${JSON.stringify(adapter.backend_slug)}`);
    // 这条锚跨越 M0/M1/M2：probe_digest 是那份纯逻辑探针报告的字节摘要，
    // native 归档（records/m1/dx12/adapter.json）写着同一个数。两边都写它，
    // 说明"两个宿主共用同一份渲染"不是一句口号。
    if (adapter.probe_digest !== spec.probeDigest) {
      messages.push(`probe_digest=${JSON.stringify(adapter.probe_digest)}，锚是 ${spec.probeDigest}`);
    }
    if (adapter.probe_format_version !== spec.probeFormatVersion) messages.push(`probe_format_version=${JSON.stringify(adapter.probe_format_version)}`);
    if (adapter.adapter_name !== null) messages.push(`adapter_name=${JSON.stringify(adapter.adapter_name)}（浏览器读不出，只能是 null）`);
    if (adapter.naga_version !== null) messages.push(`naga_version=${JSON.stringify(adapter.naga_version)}（浏览器里没有 naga）`);
    if (typeof adapter.naga_version_note !== 'string' || adapter.naga_version_note === '') {
      messages.push('naga_version_note 是空的——null 必须带一句"为什么是 null"');
    }
    if (adapter.timing_record !== null) messages.push(`timing_record=${JSON.stringify(adapter.timing_record)}（浏览器腿不出 timing.json）`);
    if (typeof adapter.timing_note !== 'string' || adapter.timing_note === '') {
      messages.push('timing_note 是空的——"这一腿为什么不计时"必须写下来');
    }
    messages.push(...exactKeys('adapter.json 的 adapter 端口', adapter.adapter, KEYS.adapterPort));
    const port = adapter.adapter ?? {};
    if (port.backend !== 'BrowserWebGpu') messages.push(`adapter.backend=${JSON.stringify(port.backend)}`);
    // 下面五栏在浏览器上就是读不出来：它们必须是空 / 零，而不是"看起来像真的"的东西。
    const emptyPort = [
      ['device', '0'], ['device_type', 'Other'], ['driver', ''], ['driver_info', ''], ['name', ''], ['vendor', '0'],
    ];
    for (const [key, want] of emptyPort) {
      if (port[key] !== want) messages.push(`adapter.${key}=${JSON.stringify(port[key])}，浏览器上应该是 ${JSON.stringify(want)}`);
    }
    messages.push(...exactKeys('adapter.json 的 gpu_identity', adapter.gpu_identity, KEYS.gpuIdentity));
    const identity = adapter.gpu_identity ?? {};
    if (identity.state !== 'unresolved') messages.push(`gpu_identity.state=${JSON.stringify(identity.state)}`);
    if (identity.resolved_by !== 'harness') messages.push(`gpu_identity.resolved_by=${JSON.stringify(identity.resolved_by)}`);
    if (identity.resolves_to !== 'host-gpu.json') messages.push(`gpu_identity.resolves_to=${JSON.stringify(identity.resolves_to)}`);
    if (typeof identity.reason !== 'string' || identity.reason === '') messages.push('gpu_identity.reason 是空的');
    messages.push(...exactKeys('adapter.json 的 in_page', adapter.in_page, KEYS.inPage));
    const inPage = adapter.in_page ?? {};
    if (inPage.architecture !== spec.inPage.architecture) messages.push(`in_page.architecture=${JSON.stringify(inPage.architecture)}，锚是 ${JSON.stringify(spec.inPage.architecture)}`);
    if (inPage.vendor !== spec.inPage.vendor) messages.push(`in_page.vendor=${JSON.stringify(inPage.vendor)}，锚是 ${JSON.stringify(spec.inPage.vendor)}`);
    if (inPage.subgroup_max_size !== spec.inPage.subgroupMaxSize) {
      messages.push(`in_page.subgroup_max_size=${JSON.stringify(inPage.subgroup_max_size)}，锚是 ${spec.inPage.subgroupMaxSize}`);
    }
    if (inPage.is_fallback_adapter !== false) messages.push(`in_page.is_fallback_adapter=${JSON.stringify(inPage.is_fallback_adapter)}`);
    if (typeof inPage.note !== 'string' || inPage.note === '') messages.push('in_page.note 是空的');
    if (!Array.isArray(adapter.nondeterministic_fields) || adapter.nondeterministic_fields.length !== 5) {
      messages.push(`nondeterministic_fields 有 ${adapter.nondeterministic_fields?.length} 条，锚是 5 条`);
    }
    const seconds = adapter.unix_epoch_seconds;
    const millis = adapter.unix_epoch_millis;
    if (!Number.isInteger(seconds) || !Number.isInteger(millis)) {
      messages.push('unix_epoch_* 不是整数');
    } else if (Math.floor(millis / 1000) !== seconds) {
      messages.push(`unix_epoch_seconds ${seconds} 与 unix_epoch_millis ${millis} 不是同一时刻`);
    }
    return messages;
  },

  'host-gpu': (leg, spec) => {
    const messages = [];
    const hostGpu = leg.hostGpu?.value;
    if (hostGpu === undefined) return ['host-gpu.json 读不到或解不开'];
    messages.push(...exactKeys('host-gpu.json 顶层', hostGpu, KEYS.hostGpu));
    if (typeof hostGpu.source !== 'string' || hostGpu.source === '') messages.push('host-gpu.json 的 source 是空的');
    if (!Array.isArray(hostGpu.devices) || hostGpu.devices.length === 0) {
      return [...messages, 'host-gpu.json 的 devices 是空的——没有设备表就没有"对上了哪块卡"这件事'];
    }
    for (const [index, device] of hostGpu.devices.entries()) {
      messages.push(...exactKeys(`host-gpu.json 的 devices[${index}]`, device, KEYS.device));
    }
    messages.push(...exactKeys('host-gpu.json 的 resolved', hostGpu.resolved, KEYS.resolved));
    const resolved = hostGpu.resolved ?? {};
    if (resolved.vendor !== spec.gpu.vendor) messages.push(`resolved.vendor=${JSON.stringify(resolved.vendor)}，锚是 ${JSON.stringify(spec.gpu.vendor)}`);
    if (resolved.vendor_id !== spec.gpu.vendorId) messages.push(`resolved.vendor_id=${JSON.stringify(resolved.vendor_id)}，锚是 ${spec.gpu.vendorId}`);
    if (resolved.device_id !== spec.gpu.deviceId) messages.push(`resolved.device_id=${JSON.stringify(resolved.device_id)}，锚是 ${spec.gpu.deviceId}`);
    if (resolved.device !== spec.gpu.device) messages.push(`resolved.device=${JSON.stringify(resolved.device)}，锚是 ${JSON.stringify(spec.gpu.device)}`);
    if (resolved.driver_version !== spec.gpu.driverVersion) {
      messages.push(`resolved.driver_version=${JSON.stringify(resolved.driver_version)}，锚是 ${JSON.stringify(spec.gpu.driverVersion)}`);
    }
    // 这块卡必须真的在设备表里，而且是**同一块**（型号 + 驱动版本 + PCI id 都对得上）。
    // 只写 resolved 不对表，"对上了"就只是一句自述。
    const hit = hostGpu.devices.find(
      (device) => device.deviceString === resolved.device && device.driverVersion === resolved.driver_version,
    );
    if (hit === undefined) {
      messages.push(`resolved 指着 ${JSON.stringify(resolved.device)} / ${JSON.stringify(resolved.driver_version)}，设备表里没有对应的一行`);
    } else {
      if (hit.vendorId !== resolved.vendor_id) messages.push(`设备表里这块卡的 vendorId ${hit.vendorId}，resolved 写 ${resolved.vendor_id}`);
      if (hit.deviceId !== resolved.device_id) messages.push(`设备表里这块卡的 deviceId ${hit.deviceId}，resolved 写 ${resolved.device_id}`);
    }
    // 注意：**不**断言 resolved.driver_vendor === 那一行的 driverVendor。AMD 腿的设备表里
    // NVIDIA 那块卡的 driverVendor 实测是 "AMD"（原样抄录，不做修正），这块卡本身也不参与
    // 匹配——拿一个"应该是什么"去覆盖"抄到了什么"，等于把记录改成了记忆里的样子。
    if (typeof resolved.match_reason !== 'string' || resolved.match_reason === '') {
      messages.push('resolved.match_reason 是空的——"怎么就认定是这块卡"必须写下来');
    }
    const shot = leg.screenshot?.value;
    if (shot !== undefined && shot.host_gpu !== undefined) {
      if (JSON.stringify(shot.host_gpu.on_disk) !== JSON.stringify(hostGpu)) {
        messages.push('截图 json 里的 host_gpu.on_disk 与 host-gpu.json 不是同一份');
      }
      if (JSON.stringify(shot.host_gpu.devices) !== JSON.stringify(hostGpu.devices)) {
        messages.push('截图 json 里的 host_gpu.devices 与 host-gpu.json 的 devices 不是同一份');
      }
    }
    return messages;
  },

  screenshot: (leg, spec) => {
    const shot = leg.screenshot?.value;
    if (shot === undefined) return ['screenshot-browser-corpus.json 读不到或解不开'];
    const messages = [...exactKeys('截图 json 顶层', shot, KEYS.screenshotTop)];
    if (shot.schema !== 1) messages.push(`schema=${JSON.stringify(shot.schema)}`);
    // 这份的 milestone 是**运行时间**（M2），与 run.json 的 M1（表契约版本）不是一个东西。
    if (shot.milestone !== 'M2') messages.push(`milestone=${JSON.stringify(shot.milestone)}（这份是运行时间，M2）`);
    for (const key of ['captured_at', 'page_url', 'purpose']) {
      if (typeof shot[key] !== 'string' || shot[key] === '') messages.push(`${key} 是空的`);
    }
    messages.push(...exactKeys('截图 json 的 leg', shot.leg, KEYS.screenshotLeg));
    const legInfo = shot.leg ?? {};
    // 截图 json 里的 `leg.slug` 是**记录内的腿名**（m2 / m2-amd），不是目录名；
    // 目录名由 `spec.slug` 表示（下面 `out_dir` 那一栏查的就是它）。两者用 `legSlugInRecord` 区分。
    // 实测：records/m2/browser/screenshot-browser-corpus.json 的 leg.slug 是 m2。
    if (legInfo.slug !== spec.legSlugInRecord) messages.push(`leg.slug=${JSON.stringify(legInfo.slug)}，锚是 ${JSON.stringify(spec.legSlugInRecord)}`);
    if (legInfo.backend_slug !== spec.legSlugInRecord) messages.push(`leg.backend_slug=${JSON.stringify(legInfo.backend_slug)}`);
    if (typeof legInfo.out_dir !== 'string' || !legInfo.out_dir.endsWith(`records/m2/${spec.slug}`)) {
      messages.push(`leg.out_dir=${JSON.stringify(legInfo.out_dir)}，应当指向 records/m2/${spec.slug}`);
    }
    if (legInfo.requested_backends !== spec.requestedBackends) messages.push(`leg.requested_backends=${JSON.stringify(legInfo.requested_backends)}`);
    if (legInfo.adapter_name !== null) messages.push(`leg.adapter_name=${JSON.stringify(legInfo.adapter_name)}（浏览器读不出）`);
    if (legInfo.build_profile !== spec.buildProfile) messages.push(`leg.build_profile=${JSON.stringify(legInfo.build_profile)}`);
    if (legInfo.wgpu_version !== spec.wgpuVersion) messages.push(`leg.wgpu_version=${JSON.stringify(legInfo.wgpu_version)}`);
    if (legInfo.naga_version !== null) messages.push(`leg.naga_version=${JSON.stringify(legInfo.naga_version)}（浏览器里没有 naga）`);

    messages.push(...exactKeys('截图 json 的 browser', shot.browser, KEYS.screenshotBrowser));
    const browser = shot.browser ?? {};
    if (browser.headless !== true) messages.push(`browser.headless=${JSON.stringify(browser.headless)}`);
    for (const key of ['kind', 'path', 'product', 'user_agent']) {
      if (typeof browser[key] !== 'string' || browser[key] === '') messages.push(`browser.${key} 是空的`);
    }
    // CDP 的 `Browser.getVersion().revision` 是**字符串**（形如 @792bf67…），采集脚本原样落盘。
    // 判「非空字符串」而不是「整数」——按产出脚本的实际契约判。
    if (typeof browser.revision !== 'string' || browser.revision === '') {
      messages.push(`browser.revision=${JSON.stringify(browser.revision)}（CDP 给的是非空字符串）`);
    }
    // 这条是两条腿之间**该不一样**的地方之一：AMD 腿多一个 --force_low_power_gpu。
    if (JSON.stringify(browser.extra_args) !== JSON.stringify(spec.extraArgs)) {
      messages.push(`browser.extra_args=${JSON.stringify(browser.extra_args)}，锚是 ${JSON.stringify(spec.extraArgs)}`);
    }

    messages.push(...exactKeys('截图 json 的 viewport', shot.viewport, KEYS.viewport));
    const viewport = shot.viewport ?? {};
    if (!Number.isInteger(viewport.width) || !Number.isInteger(viewport.height)) messages.push('viewport 的宽高不是整数');
    if (!Number.isInteger(viewport.captured_content?.height) || viewport.captured_content.height <= viewport.height) {
      messages.push('captured_content.height 不比视口高——"整页截图"这件事没被记下来');
    }

    const round = shot.round ?? {};
    messages.push(...exactKeys('截图 json 的 round', round, KEYS.round));
    for (const key of ['frames_written', 'frames_verified', 'frames_on_disk']) {
      if (round[key] !== spec.frames) messages.push(`round.${key}=${JSON.stringify(round[key])}，锚是 ${spec.frames}`);
    }
    if (round.scene !== 'all') messages.push(`round.scene=${JSON.stringify(round.scene)}`);
    if (round.frames_arg !== spec.frameRange) messages.push(`round.frames_arg=${JSON.stringify(round.frames_arg)}`);
    if (round.frames_digest !== spec.framesDigest) messages.push(`round.frames_digest=${JSON.stringify(round.frames_digest)}，锚是 ${spec.framesDigest}`);
    if (round.digest_matches_expect !== spec.digestMatchesExpect) {
      messages.push(`round.digest_matches_expect=${JSON.stringify(round.digest_matches_expect)}，锚是 ${JSON.stringify(spec.digestMatchesExpect)}`);
    }
    // 期望值那一栏与 native 归档的摘要对齐：AMD 腿对不上（实测差异），所以这里只查它
    // 与 native 锚的关系，不查它与自己那一栏的关系。
    if (round.expected_frames_digest !== EXPECTED.native.framesDigest) {
      messages.push(`round.expected_frames_digest=${JSON.stringify(round.expected_frames_digest)}，锚（native 归档）是 ${EXPECTED.native.framesDigest}`);
    }
    if ((round.frames_digest === round.expected_frames_digest) !== (round.digest_matches_expect === true)) {
      messages.push('round.digest_matches_expect 与两个摘要对不对得上不一致');
    }
    const backend = leg.run?.value?.backends?.[0];
    if (backend !== undefined && JSON.stringify(round.counts) !== JSON.stringify(backend.counts)) {
      messages.push('round.counts 与 run.json 的 counts 不是同一份');
    }
    const disk = {
      readings_bytes: leg.rootFiles.has('readings.txt') ? leg.fileBytes.get('readings.txt') : undefined,
      run_json_bytes: leg.rootFiles.has('run.json') ? leg.fileBytes.get('run.json') : undefined,
      adapter_bytes: leg.rootFiles.has('adapter.json') ? leg.fileBytes.get('adapter.json') : undefined,
    };
    for (const [key, bytes] of Object.entries(disk)) {
      if (round[key] !== bytes) messages.push(`round.${key}=${JSON.stringify(round[key])}，盘上是 ${bytes} 字节`);
      const onDiskKey = `${key}_on_disk`;
      if (round[onDiskKey] !== bytes) messages.push(`round.${onDiskKey}=${JSON.stringify(round[onDiskKey])}，盘上是 ${bytes} 字节`);
    }
    const pngTotal = backend === undefined
      ? undefined
      : backend.frames.reduce((sum, frame) => sum + frame.png_bytes, 0);
    if (pngTotal !== undefined && round.png_bytes_total !== pngTotal) {
      messages.push(`round.png_bytes_total=${formatNumber(round.png_bytes_total)}，逐帧加起来是 ${formatNumber(pngTotal)}`);
    }
    if (!Number.isInteger(round.elapsed_ms) || round.elapsed_ms <= 0) messages.push(`round.elapsed_ms=${JSON.stringify(round.elapsed_ms)}`);

    messages.push(...exactKeys('截图 json 的 screenshot', shot.screenshot, KEYS.screenshotFile));
    const file = shot.screenshot ?? {};
    if (file.file !== 'screenshot-browser-corpus.png') messages.push(`screenshot.file=${JSON.stringify(file.file)}`);
    if (leg.screenshotPng === null) {
      messages.push('screenshot-browser-corpus.png 不在');
    } else {
      if (file.bytes !== leg.screenshotPng.length) messages.push(`screenshot.bytes=${formatNumber(file.bytes)}，盘上 ${formatNumber(leg.screenshotPng.length)} 字节`);
      const digest = sha256Hex(leg.screenshotPng);
      if (file.sha256 !== digest) messages.push(`screenshot.sha256=${JSON.stringify(file.sha256)}，重算是 ${digest}`);
    }

    if (!Array.isArray(shot.server_checks) || shot.server_checks.length === 0) {
      messages.push('server_checks 是空的——服务端一条都没核');
    } else {
      for (const [index, item] of shot.server_checks.entries()) {
        messages.push(...exactKeys(`server_checks[${index}]`, item, KEYS.checkItem));
        if (item.ok !== true) messages.push(`server_checks[${index}] 是红的：${item.detail}`);
        if (typeof item.detail !== 'string' || item.detail === '') messages.push(`server_checks[${index}] 没写理由`);
      }
    }
    const okPattern = (list, wantFalse) => list.map((item, index) => (item.ok === false ? index : null)).filter((index) => index !== null).join(',') === wantFalse.join(',');
    if (!Array.isArray(shot.server_findings) || shot.server_findings.length === 0) {
      messages.push('server_findings 是空的');
    } else {
      for (const [index, item] of shot.server_findings.entries()) {
        messages.push(...exactKeys(`server_findings[${index}]`, item, KEYS.checkItem));
      }
      // 这两条 false 是**设计意图**：内容是"这条腿与 native 不一样，差异交给 framediff"。
      // 它们红着、而 audit.ok 还是 true，才说明这套判定分得清"不同"与"出错"。
      const falseIndexes = spec.findingsFalse;
      if (!okPattern(shot.server_findings, falseIndexes)) {
        messages.push(
          `server_findings 里 ok=false 的是第 ${shot.server_findings.map((item, i) => (item.ok === false ? i : null)).filter((i) => i !== null).join('、') || '（没有）'} 条，`
            + `锚是第 ${spec.findingsFalse.join('、') || '（没有）'} 条`,
        );
      }
    }
    messages.push(...exactKeys('截图 json 的 audit', shot.audit, KEYS.audit));
    const audit = shot.audit ?? {};
    if (audit.ok !== true) messages.push(`audit.ok=${JSON.stringify(audit.ok)}——findings 里有 false 不代表 audit 不通过，但 audit.ok 必须是 true`);
    if (!Array.isArray(audit.hard) || audit.hard.length !== 0) messages.push(`audit.hard=${JSON.stringify(audit.hard)}`);
    if (!Array.isArray(audit.findings) || audit.findings.length === 0) {
      messages.push('audit.findings 是空的');
    } else {
      for (const [index, item] of audit.findings.entries()) {
        messages.push(...exactKeys(`audit.findings[${index}]`, item, KEYS.checkItem));
      }
      const want = spec.auditFindingsFalse ?? [];
      if (!okPattern(audit.findings, want)) {
        messages.push(
          `audit.findings 里 ok=false 的是第 ${audit.findings.map((item, i) => (item.ok === false ? i : null)).filter((i) => i !== null).join('、') || '（没有）'} 条，`
            + `锚是第 ${want.join('、') || '（没有）'} 条`,
        );
      }
    }
    messages.push(...exactKeys('截图 json 的 trigger', shot.trigger, KEYS.trigger));
    if (!Array.isArray(shot.notes) || shot.notes.length === 0 || shot.notes.some((note) => typeof note !== 'string' || note === '')) {
      messages.push('notes 是空数组或有空条目');
    }
    return messages;
  },

  'rerun-repro': (leg, spec) => {
    const repro = leg.rerunRepro;
    if (spec.rerunRepro === null) {
      // 反过来说：这条腿本来**不该**有复跑佐证，出现它就得解释。
      return repro === null
        ? []
        : ['多出 rerun-repro.json——锚说这条腿没有复跑佐证，出现它要说明为什么'];
    }
    if (repro === null) return ['缺 rerun-repro.json——这条腿的"重跑一遍还是这些字节"没有佐证'];
    if (repro.error !== undefined) return [repro.error];
    const value = repro.value;
    const messages = [...exactKeys('rerun-repro.json 顶层', value, KEYS.rerunRepro)];
    if (value.schema !== 1) messages.push(`schema=${JSON.stringify(value.schema)}`);
    if (value.kind !== 'browser-leg-rerun-repro') messages.push(`kind=${JSON.stringify(value.kind)}`);
    if (value.leg !== spec.legSlugInRecord) messages.push(`leg=${JSON.stringify(value.leg)}`);
    // `question` 是这份佐证要回答的那句话（产出脚本写的），空着就等于没说要证明什么。
    if (typeof value.question !== 'string' || value.question === '') messages.push('question 是空的');
    messages.push(...exactKeys('rerun-repro.json 的 first', value.first, KEYS.rerunRun));
    messages.push(...exactKeys('rerun-repro.json 的 rerun', value.rerun, KEYS.rerunRun));
    const want = spec.rerunRepro;
    if (value.first?.frames_digest !== want.framesDigest) messages.push(`first.frames_digest=${JSON.stringify(value.first?.frames_digest)}，锚是 ${want.framesDigest}`);
    if (value.rerun?.frames_digest !== want.framesDigest) messages.push(`rerun.frames_digest=${JSON.stringify(value.rerun?.frames_digest)}`);
    if (value.first?.png_bytes_total !== want.pngBytes) messages.push(`first.png_bytes_total=${value.first?.png_bytes_total}，锚是 ${want.pngBytes}`);
    if (value.rerun?.png_bytes_total !== want.pngBytes) messages.push(`rerun.png_bytes_total=${value.rerun?.png_bytes_total}`);
    // 与本次记录对齐：first 指的就是这条腿自己
    if (value.first?.out_dir !== `records/m2/${spec.slug}`) messages.push(`first.out_dir=${JSON.stringify(value.first?.out_dir)}`);
    if (value.rerun?.out_dir === value.first?.out_dir) messages.push('rerun.out_dir 与 first.out_dir 是同一个目录——那不算复跑');
    messages.push(...exactKeys('rerun-repro.json 的 frames', value.frames, KEYS.rerunFrames));
    if (value.frames?.compared !== spec.frames) messages.push(`frames.compared=${value.frames?.compared}，锚是 ${spec.frames}`);
    if (value.frames?.byte_identical !== spec.frames) messages.push(`frames.byte_identical=${value.frames?.byte_identical}，锚是 ${spec.frames}`);
    if (!Array.isArray(value.frames?.mismatches) || value.frames.mismatches.length !== 0) {
      messages.push(`frames.mismatches 不是空数组：${JSON.stringify(value.frames?.mismatches)?.slice(0, 80)}`);
    }
    messages.push(...exactKeys('rerun-repro.json 的 readings', value.readings, KEYS.rerunReadings));
    if (value.readings?.identical !== true) messages.push(`readings.identical=${JSON.stringify(value.readings?.identical)}`);
    if (value.readings?.first_sha256_16 !== want.readingsSha256_16) {
      messages.push(`readings.first_sha256_16=${JSON.stringify(value.readings?.first_sha256_16)}，锚是 ${want.readingsSha256_16}`);
    }
    if (value.readings?.rerun_sha256_16 !== want.readingsSha256_16) messages.push(`readings.rerun_sha256_16=${JSON.stringify(value.readings?.rerun_sha256_16)}`);
    for (const key of ['first_bytes', 'rerun_bytes']) {
      if (value.readings?.[key] !== want.readingsBytes) messages.push(`readings.${key}=${value.readings?.[key]}，锚是 ${want.readingsBytes}`);
    }
    // 自己再核一遍：记录里那个 sha256 前 16 位是不是真的等于盘上那份 readings.txt 的
    if (leg.readingsBytes !== null) {
      const digest = sha256Hex(leg.readingsBytes).slice(0, 16);
      if (digest !== want.readingsSha256_16) {
        messages.push(`盘上 readings.txt 的 sha256 前 16 位是 ${digest}，锚是 ${want.readingsSha256_16}`);
      }
    }
    if (value.not_compared?.run_json === undefined) messages.push('not_compared 里没写 run.json 为什么没参与比对');
    return messages;
  },
};

/**
 * 判一条腿。返回**每一项都有一条结论**（顺序与 `LEG_CHECKS` 一致）的数组。
 *
 * 每项 `detail` 最多列 6 条，免得一条系统性错误刷出几百行把第一因埋掉。
 */
export function checkLegModel(leg, spec) {
  return LEG_CHECKS.map((id) => {
    let messages;
    try {
      messages = CHECKS[id](leg, spec);
    } catch (error) {
      messages = [`检查自己抛了异常：${error.message}`];
    }
    const shown = messages.slice(0, 6);
    if (messages.length > shown.length) shown.push(`…另有 ${messages.length - shown.length} 处`);
    return { id, ok: messages.length === 0, detail: shown.join('；') };
  });
}

/** 千分位——只为了让"哪个数大"一眼能看出来，不参与任何判定。 */
function formatNumber(value) {
  return typeof value === 'number' ? value.toLocaleString('en-US') : JSON.stringify(value);
}

// ---------------------------------------------------------------------------
// native 腿（records/m1/dx12）：M2 的答案里有一半在这份归档里
//
// 它已经由 `check-m1-record.mjs` 整份查过一遍，所以这里不重复那 14 项——这里只回答
// M2 自己需要的那三件事：**它是不是我们以为的那份归档**、**它的帧摘要能不能重算出来**、
// **它的 readings 与浏览器腿是不是同一份字节**。少查了东西，就会出现"M2 的结论
// 建在一份没被任何人核过的目录上"。
// ---------------------------------------------------------------------------

export const NATIVE_CHECKS = ['archive', 'frames-digest', 'readings'];

export function checkNativeLeg(leg, spec) {
  const messages = {
    archive: [],
    'frames-digest': [],
    readings: [],
  };
  const wanted = ['adapter.json', 'compare.json', 'readings.txt', 'run.json', 'timing.json'];
  for (const name of wanted) if (!leg.rootFiles.has(name)) messages.archive.push(`缺 ${name}`);
  if (leg.frameNames.length !== spec.frames) messages.archive.push(`frames/ 里 ${leg.frameNames.length} 张 PNG，锚是 ${spec.frames} 张`);
  const missing = expectedFrameNames(spec).filter((name) => !leg.frameNames.includes(name));
  if (missing.length > 0) messages.archive.push(`缺 ${missing.length} 张帧：${missing.slice(0, 6).join('、')}`);
  // 锚：这份归档是 M1 那次提交留下的，probe_digest 与帧摘要都写死在 EXPECTED 里
  if (leg.adapter?.value?.probe_digest !== EXPECTED.probeDigest) {
    messages.archive.push(`adapter.json 的 probe_digest=${JSON.stringify(leg.adapter?.value?.probe_digest)}，锚是 ${EXPECTED.probeDigest}`);
  }
  if (leg.run?.value?.backends?.[0]?.frames_digest !== spec.framesDigest) {
    messages['frames-digest'].push(
      `run.json 的整表摘要=${JSON.stringify(leg.run?.value?.backends?.[0]?.frames_digest)}，锚是 ${spec.framesDigest}`,
    );
  }

  const backend = leg.run?.value?.backends?.[0];
  if (backend === undefined) {
    messages['frames-digest'].push('run.json 里没有 backends[0]');
  } else {
    const recomputed = framesDigest(
      backend.frames.map((frame) => ({ scene: frame.scene, frame: frame.frame, pixelDigest: frame.pixel_digest })),
    );
    if (recomputed !== spec.framesDigest) {
      messages['frames-digest'].push(`用逐帧像素摘要重拼是 ${recomputed}，锚是 ${spec.framesDigest}`);
    }
    for (const frame of backend.frames) {
      const name = String(frame.png).replace(/^frames\//, '');
      const image = frameImage(leg, name);
      if (image.error !== undefined) {
        messages['frames-digest'].push(`${name}：${image.error}`);
        continue;
      }
      const digest = fnv1a64(image.image.pixels);
      if (digest !== frame.pixel_digest) messages['frames-digest'].push(`${name}：像素摘要写 ${frame.pixel_digest}，重算是 ${digest}`);
    }
  }

  if (leg.readingsBytes === null) {
    messages.readings.push('readings.txt 不在');
  } else {
    if (leg.readingsBytes.length !== spec.readingsBytes) {
      messages.readings.push(`readings.txt 有 ${leg.readingsBytes.length} 字节，锚是 ${spec.readingsBytes} 字节`);
    }
    if (leg.run?.value === undefined) {
      messages.readings.push('run.json 读不到，无法复算');
    } else {
      const got = renderReadings(leg.run.value);
      if (got !== leg.readingsBytes.toString('utf8')) messages.readings.push('重渲的 readings.txt 与盘上不一样');
    }
  }

  return NATIVE_CHECKS.map((id) => {
    const list = messages[id];
    const shown = list.slice(0, 6);
    if (list.length > shown.length) shown.push(`…另有 ${list.length - shown.length} 处`);
    return { id: `native-${id}`, ok: list.length === 0, detail: shown.join('；') };
  });
}

// ---------------------------------------------------------------------------
// 跨腿：M2 的两句话都在这里
//
// 第一句：浏览器那条腿画出来的**就是** native 那 80 张（逐字节）。
// 第二句：AMD 那条腿与 native 不一样，差异**恰好**是这些，且都说得清来历。
//
// 两句都要在，缺哪一句 M2 都答不完：只有第一句，说明挑了一条正好一样的腿；
// 只有第二句，说明"一样"没被验过。
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// 两处必须说明白的"事实"（写在这里而不是写进判据，因为它们是**证据**不是**结论**）
//
// ① `adapter.json`：两条腿的差异路径恰好 6 条，但**文件字节**确实不同——
//    每条路径的值长短不一样（`lovelace` vs `rdna-2`、`m2` vs `m2-amd`、两个时间戳），
//    于是 2398 / 2397 字节。所以"差异只有 6 条路径"与"两份文件不同"同时成立，
//    谁也不能代替谁：只看路径数会漏掉"值改了但结构没变"，只看字节数会漏掉"改了哪一栏"。
// ② `host-gpu.json`：两条腿的**字节也不同**（resolved 指着两块不同的卡、设备表不同），
//    而截图 json 里那份 `host_gpu.on_disk` 是这条腿自己的拷贝——所以 `on_disk` 两腿
//    **不相等**，它该相等的是"与本腿的 host-gpu.json 相等"（那条判定在纯 `host-gpu` 项里）。
//    把跨腿的 `on_disk` 判成"必须相等"是错的；把它的字节数当"该一样"也是错的。
// ---------------------------------------------------------------------------

/**
 * 跨腿这几段的 id（顺序与下面返回的一致）。
 *
 * 写成常量不是为了让代码好读：自检要断言"这一族恰好是这 6 段、且每段都有反向用例"，
 * 靠的就是它。哪天有人在 `checkCrossLegs` 里删了一段，自检会当场报"项少了"。
 */
export const CROSS_CHECKS = [
  'browser-vs-native-bytes',
  'amd-vs-native-readings',
  'adapter-drift',
  'screenshot-drift',
  'run-json-drift',
  'leg-distinctness',
];

export function checkCrossLegs({ browser, amd, native }) {
  const results = [];

  // ---- ① 浏览器 / NVIDIA 与 native / NVIDIA：逐字节相同 ----
  {
    const messages = [];
    const names = browser.frameNames;
    if (names.length !== native.frameNames.length) {
      messages.push(`帧数不同：${names.length} vs ${native.frameNames.length}`);
    }
    const onlyBrowser = names.filter((name) => !native.frameNames.includes(name));
    const onlyNative = native.frameNames.filter((name) => !names.includes(name));
    if (onlyBrowser.length > 0) messages.push(`只有浏览器腿有：${onlyBrowser.slice(0, 6).join('、')}`);
    if (onlyNative.length > 0) messages.push(`只有 native 有：${onlyNative.slice(0, 6).join('、')}`);
    let drift = 0;
    const first = [];
    for (const name of names) {
      const left = browser.frameBytes.get(name);
      const right = native.frameBytes.get(name);
      if (right === undefined) continue;
      if (!left.equals(right)) {
        drift += 1;
        if (first.length < 4) first.push(`${name}（${left.length} vs ${right.length} 字节）`);
      }
    }
    if (drift > 0) messages.push(`有 ${drift} 张 PNG 的文件字节不同：${first.join('、')}`);
    if (browser.setDigest !== EXPECTED.legs[0].setDigest) {
      messages.push(`浏览器腿的集合摘要是 ${browser.setDigest}，锚是 ${EXPECTED.legs[0].setDigest}`);
    }
    if (browser.setDigest !== native.setDigest) {
      messages.push(`浏览器腿的集合摘要 ${browser.setDigest} ≠ native 的 ${native.setDigest}`);
    }
    results.push({ id: 'browser-vs-native-bytes', ok: messages.length === 0, detail: messages.slice(0, 6).join('；') });
  }

  // ---- ② AMD 与 native：不一样，而且差异是**钉住的那一些** ----
  {
    const messages = [];
    const left = browser.readingsBytes?.toString('utf8');
    const right = amd.readingsBytes?.toString('utf8');
    const nativeText = native.readingsBytes?.toString('utf8');
    if (left === undefined || right === undefined || nativeText === undefined) {
      messages.push('有腿的 readings.txt 读不到');
    } else {
      if (left !== nativeText) messages.push('浏览器腿的 readings.txt 与 native 不是同一份字节——上一条"逐字节相同"就没落地');
      const a = left.split('\n');
      const b = right.split('\n');
      const diffLines = [];
      for (let i = 0; i < Math.max(a.length, b.length); i += 1) if (a[i] !== b[i]) diffLines.push(i);
      if (diffLines.length !== EXPECTED.legDrift.readingsDiffLines) {
        messages.push(`两条浏览器腿的 readings.txt 有 ${diffLines.length} 行不同，锚是 ${EXPECTED.legDrift.readingsDiffLines} 行`);
      }
      if (diffLines[0] !== EXPECTED.legDrift.readingsFirstDiffLine) {
        messages.push(`首个不同的行是第 ${diffLines[0]} 行（0 基），锚是第 ${EXPECTED.legDrift.readingsFirstDiffLine} 行`);
      }
      if (browser.readingsBytes.length !== amd.readingsBytes.length) {
        messages.push(`两条腿的 readings.txt 长度不同：${browser.readingsBytes.length} vs ${amd.readingsBytes.length}——差 1 LSB 不该改变字节数`);
      }
    }
    results.push({ id: 'amd-vs-native-readings', ok: messages.length === 0, detail: messages.slice(0, 6).join('；') });
  }

  // ---- ③ 两条腿之间的差异形状：adapter.json 恰好 6 处、且就是那 6 条 ----
  {
    const messages = [];
    const left = browser.adapter?.value;
    const right = amd.adapter?.value;
    if (left === undefined || right === undefined) {
      messages.push('有腿的 adapter.json 读不到');
    } else {
      const paths = diffPathList(left, right).map((entry) => entry.path).sort();
      const want = [...EXPECTED.legDrift.adapterPaths].sort();
      if (paths.join(',') !== want.join(',')) {
        const extra = paths.filter((path) => !want.includes(path));
        const missing = want.filter((path) => !paths.includes(path));
        const parts = [];
        if (extra.length > 0) parts.push(`多出 ${extra.join('、')}`);
        if (missing.length > 0) parts.push(`少了 ${missing.join('、')}`);
        messages.push(`两条腿的 adapter.json 差异路径不是那 6 条：${parts.join('；')}`);
      }
    }
    results.push({ id: 'adapter-drift', ok: messages.length === 0, detail: messages.slice(0, 6).join('；') });
  }

  // ---- ④ 两条腿的截图 json：该一样的维度必须一样，该不一样的必须不一样 ----
  {
    const messages = [];
    const left = browser.screenshot?.value;
    const right = amd.screenshot?.value;
    if (left === undefined || right === undefined) {
      messages.push('有腿的截图 json 读不到');
    } else {
      for (const path of EXPECTED.legDrift.screenshotMustDiffer) {
        const a = pluck(left, path);
        const b = pluck(right, path);
        if (!a.found || !b.found) {
          messages.push(`${path} 有一侧根本没有这一栏（${a.found ? 'b' : 'a'} 侧缺）——缺栏不能算"该不一样"`);
          continue;
        }
        if (JSON.stringify(a.value) === JSON.stringify(b.value)) {
          messages.push(`${path} 两条腿一模一样（${JSON.stringify(a.value)}），可这一栏本来该不一样`);
        }
      }
      for (const path of EXPECTED.legDrift.screenshotMustEqual) {
        const a = pluck(left, path);
        const b = pluck(right, path);
        if (!a.found || !b.found) {
          messages.push(`${path} 有一侧读不到`);
          continue;
        }
        if (JSON.stringify(a.value) !== JSON.stringify(b.value)) {
          messages.push(`${path} 两条腿不一样：${JSON.stringify(a.value)} vs ${JSON.stringify(b.value)}`);
        }
      }
      for (const shot of [left, right]) {
        if (shot.milestone !== 'M2') messages.push(`截图 json 的 milestone=${JSON.stringify(shot.milestone)}`);
        if (shot.audit?.ok !== true) messages.push('audit.ok 不是 true');
      }
    }
    results.push({ id: 'screenshot-drift', ok: messages.length === 0, detail: messages.slice(0, 6).join('；') });
  }

  // ---- ⑤ 两条腿的 run.json：不是逐字节相同，差的是钉住的那几组 ----
  {
    const messages = [];
    const left = browser.run?.value;
    const right = amd.run?.value;
    const nativeRun = native.run?.value;
    if (left === undefined || right === undefined || nativeRun === undefined) {
      messages.push('有腿的 run.json 读不到');
    } else {
      const vsNative = diffPathList(left, nativeRun).map((entry) => entry.path).sort();
      const want = [...EXPECTED.legDrift.runJsonVsNativePaths].sort();
      if (vsNative.join(',') !== want.join(',')) {
        const extra = vsNative.filter((path) => !want.includes(path));
        const missing = want.filter((path) => !vsNative.includes(path));
        const parts = [];
        if (extra.length > 0) parts.push(`多出 ${extra.slice(0, 6).join('、')}`);
        if (missing.length > 0) parts.push(`少了 ${missing.join('、')}`);
        messages.push(`浏览器腿与 native 的 run.json 差异路径不是那 2 条：${parts.join('；')}`);
      }
      if (JSON.stringify(left.backends?.[0]?.adapter_name) !== JSON.stringify(right.backends?.[0]?.adapter_name)) {
        // 两条腿都读不出卡名（null），"读不出"在记录里只有一种写法
        messages.push('两条腿的 backends[0].adapter_name 不是同一个值——浏览器两条腿都读不出卡名，应当都是 null');
      }
      const paths = diffPathList(left, right).map((entry) => entry.path);
      const groups = new Map();
      for (const path of paths) {
        const key = path.replace(/frames\[\d+\]/g, 'frames[i]').replace(/points\[\d+\]/g, 'points[j]').replace(/measured\[\d+\]/g, 'measured[i]');
        groups.set(key, (groups.get(key) ?? 0) + 1);
      }
      const wantGroups = EXPECTED.legDrift.runJsonDiffGroups;
      for (const key of new Set([...Object.keys(wantGroups), ...groups.keys()])) {
        const got = groups.get(key) ?? 0;
        const pinned = wantGroups[key] ?? 0;
        if (got !== pinned) messages.push(`两条腿 run.json 的差异里 ${key} 有 ${got} 处，锚是 ${pinned} 处`);
      }
      const changedFrames = [];
      const framesA = left.backends?.[0]?.frames ?? [];
      const framesB = right.backends?.[0]?.frames ?? [];
      for (let i = 0; i < Math.max(framesA.length, framesB.length); i += 1) {
        if (framesA[i]?.pixel_digest !== framesB[i]?.pixel_digest) changedFrames.push(framesA[i]?.scene ?? framesB[i]?.scene);
      }
      if (changedFrames.length !== EXPECTED.legDrift.differingFrames) {
        messages.push(`两条腿有 ${changedFrames.length} 帧的像素摘要不同，锚是 ${EXPECTED.legDrift.differingFrames} 帧`);
      }
      const changedScenes = [...new Set(changedFrames)].sort();
      const wantScenes = [...EXPECTED.legDrift.differingScenes].sort();
      if (changedScenes.join(',') !== wantScenes.join(',')) {
        messages.push(`摘要不同的场景是 ${changedScenes.join('、') || '（没有）'}，锚是 ${wantScenes.join('、')}`);
      }
      const sameScenes = [...new Set(
        framesA.filter((frame, i) => frame.pixel_digest === framesB[i]?.pixel_digest).map((frame) => frame.scene),
      )].sort();
      const wantSame = [...EXPECTED.legDrift.identicalScenes].sort();
      if (sameScenes.join(',') !== wantSame.join(',')) {
        messages.push(`逐帧像素摘要相同的场景是 ${sameScenes.join('、') || '（没有）'}，锚是 ${wantSame.join('、')}`);
      }
    }
    results.push({ id: 'run-json-drift', ok: messages.length === 0, detail: messages.slice(0, 6).join('；') });
  }

  // ---- ⑥ 两条腿必须是**两次不同的运行**（同一份记录被拷成两份是最容易漏的一种假） ----
  {
    const messages = [];
    const adapterA = browser.adapter?.value;
    const adapterB = amd.adapter?.value;
    const shotA = browser.screenshot?.value;
    const shotB = amd.screenshot?.value;
    if (adapterA === undefined || adapterB === undefined) {
      messages.push('有腿的 adapter.json 读不到');
    } else {
      // 字节不同——这一句与"差异路径恰好 6 条"是两件事，都要说
      if (browser.adapterBytes !== null && amd.adapterBytes !== null
        && browser.adapterBytes.equals(amd.adapterBytes)) {
        messages.push('两条腿的 adapter.json 逐字节相同——那要么是同一个目录被拷了两份，要么时间戳没写进去');
      }
      const pairs = [
        ['unix_epoch_seconds', adapterA.unix_epoch_seconds, adapterB.unix_epoch_seconds],
        ['backend_slug', adapterA.backend_slug, adapterB.backend_slug],
        ['in_page.vendor', adapterA.in_page?.vendor, adapterB.in_page?.vendor],
        ['in_page.architecture', adapterA.in_page?.architecture, adapterB.in_page?.architecture],
        ['in_page.subgroup_max_size', adapterA.in_page?.subgroup_max_size, adapterB.in_page?.subgroup_max_size],
      ];
      for (const [name, a, b] of pairs) {
        if (JSON.stringify(a) === JSON.stringify(b)) {
          messages.push(`${name} 两条腿相同（${JSON.stringify(a)}）——这两份记录必须是两次不同的运行、两块不同的卡`);
        }
      }
      const gpuA = browser.hostGpu?.value?.resolved;
      const gpuB = amd.hostGpu?.value?.resolved;
      if (gpuA?.device === gpuB?.device) {
        messages.push(`两条腿的 host-gpu.json 指着同一块卡（${JSON.stringify(gpuA?.device)}）——块卡不同是这条腿存在的理由`);
      }
      if (browser.hostGpuBytes !== null && amd.hostGpuBytes !== null
        && browser.hostGpuBytes.equals(amd.hostGpuBytes)) {
        messages.push('两条腿的 host-gpu.json 逐字节相同——设备表与 resolved 该不一样');
      }
      for (const [slug, leg, want] of [['browser', browser, EXPECTED.legDrift.pngBytesTotal.browser], ['browser-amd', amd, EXPECTED.legDrift.pngBytesTotal['browser-amd']]]) {
        const total = leg.frameBytes.size > 0
          ? [...leg.frameBytes.values()].reduce((sum, bytes) => sum + bytes.length, 0)
          : null;
        if (total !== want) messages.push(`${slug} 的 PNG 总字节是 ${total}，锚是 ${want}`);
      }
      if (shotA?.browser?.extra_args?.length !== 0) messages.push(`浏览器腿的 extra_args 不是空的：${JSON.stringify(shotA?.browser?.extra_args)}`);
      if (JSON.stringify(shotB?.browser?.extra_args) !== JSON.stringify(['--force_low_power_gpu'])) {
        messages.push(`AMD 腿的 extra_args=${JSON.stringify(shotB?.browser?.extra_args)}——它靠这一条才会被派到 iGPU 上`);
      }
    }
    results.push({ id: 'leg-distinctness', ok: messages.length === 0, detail: messages.slice(0, 6).join('；') });
  }

  return results;
}

/** 按 `a.b[2].c` 这样的路径取值（返回 `{ found, value }`；不存在的路径不当成 `undefined` 值）。 */
export function pluck(object, path) {
  const steps = path.split('.');
  let current = object;
  for (const step of steps) {
    const match = /^([^\[\]]*)((?:\[\d+\])*)$/.exec(step);
    const key = match[1];
    if (key !== '') {
      if (current === null || typeof current !== 'object' || !(key in current)) return { found: false };
      current = current[key];
    }
    for (const index of match[2].matchAll(/\[(\d+)\]/g)) {
      if (!Array.isArray(current) || current.length <= Number(index[1])) return { found: false };
      current = current[Number(index[1])];
    }
  }
  return { found: true, value: current };
}

/** 逐字段差异路径（只用于**形状**，不用于判定数值）。 */
export function diffPathList(left, right, path = '', out = []) {
  if (left === right) return out;
  if (Array.isArray(left) && Array.isArray(right)) {
    for (let i = 0; i < Math.max(left.length, right.length); i += 1) {
      diffPathList(left[i], right[i], `${path}[${i}]`, out);
    }
    return out;
  }
  if (left !== null && right !== null && typeof left === 'object' && typeof right === 'object') {
    const keys = [...new Set([...Object.keys(left), ...Object.keys(right)])];
    for (const key of keys) diffPathList(left[key], right[key], path === '' ? key : `${path}.${key}`, out);
    return out;
  }
  out.push({ path, left, right });
  return out;
}

// ---------------------------------------------------------------------------
// 重算一次比对
//
// 记录里的 `report.txt` / `summary.csv` / `verdict.json` / `shape.json` 是工具**渲染**出来的。
// 这里不看它们写了什么：拿两条腿的 PNG 重算指标、重新解析档位、重新判定，再按同一口径
// **重新渲染**，最后逐字节对。上面那套指标与渲染都是本文件里另写的一份（不引工具），
// 所以"记录里的数被第二个人算出来过"这句话才成立。
// ---------------------------------------------------------------------------

/**
 * 一条腿自己的帧集合摘要：按配对顺序拼 `文件名 + 0x00 + 文件字节`，整体 FNV-1a 64。
 *
 * 工具的 `analyzePair` 对两侧各算一次；这里单独给一条腿算，是为了让跨腿检查能拿它当锚
 * （"两侧的帧集摘要同为 `4bc004b502a1301a`"这句话里的那个数就是这么来的）。
 */
export function frameSetDigest(leg) {
  const pairing = pairNames(leg.frameNames, leg.frameNames, { a: 'A', b: 'B' });
  let state = FNV_OFFSET_BASIS;
  for (const pair of pairing.pairs) {
    state = fnv1a64Update(state, Buffer.from(pair.name, 'utf8'));
    state = fnv1a64Update(state, Buffer.from([0]));
    state = fnv1a64Update(state, leg.frameBytes.get(pair.name));
  }
  return fnv1a64Hex(state);
}

/**
 * 两条腿 → 一次比对的完整分析（与工具 `analyzePair` 同口径）。
 *
 * 结构不对（名字不合契约、两侧缺胳膊少腿、尺寸不同）就**抛**：调用方把它变成一条红的
 * 检查项。这里不做任何"取交集继续跑"的静默调解——被截断的一批不能伪装成一批。
 */
export function analyzeLegPair({ legA, legB, labelA, labelB, shape = false }) {
  const labels = { a: labelA, b: labelB };
  const pairing = pairNames(legA.frameNames, legB.frameNames, labels);
  assertPairing(pairing, labels);

  const frames = [];
  const diffRgbaByName = new Map();
  let bytesTotalA = 0;
  let bytesTotalB = 0;
  let digestA = FNV_OFFSET_BASIS;
  let digestB = FNV_OFFSET_BASIS;

  for (const pair of pairing.pairs) {
    const bytesA = legA.frameBytes.get(pair.name);
    const bytesB = legB.frameBytes.get(pair.name);
    if (bytesA === undefined || bytesB === undefined) {
      throw new Error(
        `${pair.name} 有一侧不在盘上（${labelA} ${bytesA === undefined ? '缺' : '有'}，`
          + `${labelB} ${bytesB === undefined ? '缺' : '有'}）`,
      );
    }
    bytesTotalA += bytesA.length;
    bytesTotalB += bytesB.length;
    digestA = fnv1a64Update(digestA, Buffer.from(pair.name, 'utf8'));
    digestA = fnv1a64Update(digestA, Buffer.from([0]));
    digestA = fnv1a64Update(digestA, bytesA);
    digestB = fnv1a64Update(digestB, Buffer.from(pair.name, 'utf8'));
    digestB = fnv1a64Update(digestB, Buffer.from([0]));
    digestB = fnv1a64Update(digestB, bytesB);

    const imageA = frameImage(legA, pair.name);
    const imageB = frameImage(legB, pair.name);
    if (imageA.error !== undefined) throw new Error(`${labelA} 侧：${imageA.error}`);
    if (imageB.error !== undefined) throw new Error(`${labelB} 侧：${imageB.error}`);
    assertSameSize(imageA.image, imageB.image);

    const diff = pixelDiffSummary(imageA.image, imageB.image);
    if (diff.rgba !== null) {
      diffRgbaByName.set(pair.name, {
        width: imageA.image.width,
        height: imageA.image.height,
        rgba: diff.rgba,
      });
    }
    let shapeOfFrame = null;
    if (shape) {
      shapeOfFrame = pixelDiffShape(imageA.image, imageB.image);
      // 同一件事的两份实现必须说同一句话：`diff_pixels` 与 `max_abs_diff` 在这里被算了
      // 两次（一次为了差异图、一次为了形状）。与其挑一个信，不如当场停住——
      // 对不上的时候，"形状"这一整份记录都失去了立足点。
      if (shapeOfFrame.diff_pixels !== diff.diffPixels || shapeOfFrame.max_abs_diff !== diff.maxAbsDiff) {
        throw new Error(
          `${pair.name}：本文件里两份实现算出的差异对不上——`
            + `pixelDiffSummary 说 ${diff.diffPixels} 像素/max|Δ| ${diff.maxAbsDiff}，`
            + `pixelDiffShape 说 ${shapeOfFrame.diff_pixels} 像素/max|Δ| ${shapeOfFrame.max_abs_diff}`,
        );
      }
    }
    frames.push({
      scene: pair.scene,
      frame: pair.frame,
      file: pair.name,
      width: imageA.image.width,
      height: imageA.image.height,
      ssim_luma: ssimLuma(imageA.image, imageB.image),
      psnr_rgb_db: psnrRgb(imageA.image, imageB.image),
      max_abs_diff: diff.maxAbsDiff,
      diff_pixels: diff.diffPixels,
      shape: shapeOfFrame,
    });
  }

  return {
    labelA,
    labelB,
    bytesTotalA,
    bytesTotalB,
    setDigestA: fnv1a64Hex(digestA),
    setDigestB: fnv1a64Hex(digestB),
    ignoredA: legA.ignoredFrames,
    ignoredB: legB.ignoredFrames,
    frames,
    scenes: summarizeScenes(frames),
    diffRgbaByName,
  };
}

/**
 * 档位那一栏：`{ path, digest, resolved }`。
 *
 * `resolved` 是**逐场景解出来的档**（不是 [default] 的原文）——记录里那一栏的可信度
 * 全靠它："这个场景是按哪几条闸判的"必须写在判定旁边，不能让人回去读 TOML。
 */
export function thresholdsInfoOf(pathRel, text, judged) {
  return {
    path: pathRel,
    digest: fnv1a64(Buffer.from(text, 'utf8')),
    resolved: Object.fromEntries(judged.scenes.map((scene) => [scene.scene, scene.limits])),
  };
}

// ---------------------------------------------------------------------------
// 装载整份记录
//
// 检查项全部是**纯函数**（输入 = 这里的模型）：真跑时模型从磁盘构造，自检时从合成数据
// 构造——同一条检查代码路径，不会出现"自检测的是另一套逻辑"。
// ---------------------------------------------------------------------------

/** 目录清单：名字全集（文件 + 目录）、文件字节数、子目录名。 */
export function listEntries(dir) {
  const names = new Set();
  const files = new Set();
  const dirs = new Set();
  const bytes = new Map();
  if (existsSync(dir)) {
    for (const entry of readdirSync(dir, { withFileTypes: true })) {
      names.add(entry.name);
      if (entry.isDirectory()) {
        dirs.add(entry.name);
      } else {
        files.add(entry.name);
        bytes.set(entry.name, statSync(join(dir, entry.name)).size);
      }
    }
  }
  return { names, files, dirs, bytes };
}

export function readTextIfPresent(path) {
  const bytes = readBytesIfPresent(path);
  return bytes === null ? null : bytes.toString('utf8');
}

/** 一份比对记录（`records/m2/framediff*`）：盘上有什么，就原样读成模型。 */
export function loadFramediffDir(dir, dirRel, spec) {
  const listing = listEntries(dir);
  const diffNames = [];
  const diffBytes = new Map();
  const diffDir = join(dir, 'diff');
  if (existsSync(diffDir)) {
    for (const entry of readdirSync(diffDir, { withFileTypes: true })) {
      if (!entry.isFile() || !PNG_NAME.test(entry.name)) continue;
      diffNames.push(entry.name);
      diffBytes.set(entry.name, readFileSync(join(diffDir, entry.name)));
    }
    diffNames.sort();
  }
  const text = new Map();
  for (const name of ['report.txt', 'summary.csv', 'verdict.json', 'shape.json']) {
    const value = readTextIfPresent(join(dir, name));
    if (value !== null) text.set(name, value);
  }
  return {
    spec,
    path: dir,
    dirRel,
    listing,
    diffNames,
    diffBytes,
    text,
    thresholdsText: readTextIfPresent(join(REPO_ROOT, spec.thresholds)),
    reportBytes: listing.bytes.get('report.txt'),
    summaryBytes: listing.bytes.get('summary.csv'),
    verdictBytes: listing.bytes.get('verdict.json'),
    shapeBytes: listing.bytes.get('shape.json'),
  };
}

/**
 * 整份记录的模型。
 *
 *   · `legs`       —— `{ browser, 'browser-amd' }`（本里程碑跑的）
 *   · `native`     —— `records/m1/dx12`（M1 归档；M2 的判据要用它）
 *   · `framediff`  —— 两份比对记录
 *   · `entries`    —— 根目录清单（含目录名）
 */
export function loadRecord(recordDir, dirRel) {
  const listing = listEntries(recordDir);
  const legs = {};
  for (const spec of EXPECTED.legs) {
    const leg = loadLeg(join(recordDir, spec.slug), `${dirRel}/${spec.slug}`, spec.slug);
    leg.setDigest = frameSetDigest(leg);
    legs[spec.slug] = leg;
  }
  const nativeDir = join(REPO_ROOT, dirname(EXPECTED.native.runJson));
  const native = loadLeg(nativeDir, dirname(EXPECTED.native.runJson), 'native');
  native.setDigest = frameSetDigest(native);

  return {
    dirRel,
    dir: recordDir,
    listing,
    readmeText: readTextIfPresent(join(recordDir, 'README.md')),
    readmeBytes: readBytesIfPresent(join(recordDir, 'README.md')),
    wasmTests: readJsonIfPresent(join(recordDir, 'wasm-tests.json')),
    acceptance: readJsonIfPresent(join(recordDir, 'acceptance.json')),
    acceptanceBytes: readBytesIfPresent(join(recordDir, 'acceptance.json')),
    // 独立复核报告：**后到**的产物（复核者读完全部记录才写），所以不要求存在；
    // 在的话必须不是"我看过了，没问题"这种一句话。
    reviewText: readTextIfPresent(join(recordDir, 'review-independent.md')),
    reviewBytes: readBytesIfPresent(join(recordDir, 'review-independent.md')),
    legs,
    native,
    framediff: EXPECTED.framediff.map((spec) => (
      loadFramediffDir(join(recordDir, spec.slug), `${dirRel}/${spec.slug}`, spec)
    )),
  };
}

/** 记录里 `a` / `b` 两个键指的是哪条腿、哪份帧目录。 */
export const LEG_SOURCES = {
  browser: {
    legKey: 'browser',
    label: EXPECTED.legs[0].label,
    frames: `records/m2/${EXPECTED.legs[0].slug}/frames`,
    key: 'legs',
    setDigest: EXPECTED.legs[0].setDigest,
    pngBytes: EXPECTED.legs[0].pngBytes,
  },
  'browser-amd': {
    legKey: 'browser-amd',
    label: EXPECTED.legs[1].label,
    frames: `records/m2/${EXPECTED.legs[1].slug}/frames`,
    key: 'legs',
    setDigest: EXPECTED.legs[1].setDigest,
    pngBytes: EXPECTED.legs[1].pngBytes,
  },
  native: {
    legKey: 'native',
    label: EXPECTED.native.label,
    frames: EXPECTED.native.dir,
    key: 'native',
    setDigest: EXPECTED.native.setDigest,
    pngBytes: EXPECTED.native.pngBytes,
  },
};

function pickLeg(model, source) {
  return source.key === 'native' ? model.native : model.legs[source.legKey];
}

// ---------------------------------------------------------------------------
// 重算一份比对记录
//
// 检查分两层：
//
//   · **重算**：拿两条腿的 PNG 重新算指标、重新解析档位、重新判定，再按同一口径重渲，
//     与盘上那四个文件逐个对字节——这一层说的是"记录里的数被第二个人算出来过"；
//   · **锚**：记录里的数是不是我们钉住的那几个（摘要、字节数、差异像素）——这一层说的是
//     "这份记录就是当时那一份"，而不是"另一批同样自洽的数据"。
//
// 两层缺一不可：只有重算，换一批自洽的假数据照样全绿；只有锚，记录内部的算法漂了看不出来。
// ---------------------------------------------------------------------------

/**
 * 记录在仓库里的规范位置。
 *
 * 重渲时用的是**这个**路径，不是命令行传进来的目录名：`report.txt` 里
 * `记录 → records/m2/framediff/summary.csv` 那一行是当时跑出来的事实，
 * 把记录拷到别处再复核时，那一行不该跟着变（否则拷贝一份就没法核了）。
 */
const RECORD_REL = 'records/m2';

export const FRAMEDIFF_CHECKS = [
  'dir-listing',
  'inputs',
  'thresholds',
  'summary-csv',
  'verdict-json',
  'report-txt',
  'shape-json',
  'diff-images',
];

/** `report.txt` 的正文行数（不含结尾那个换行）。两份记录一致。 */
const REPORT_LINES = 11;

/**
 * 重算一份比对记录——**只算，不与盘上比**。
 *
 * 结构不成立（腿不在、帧配不上、档位读不出）就抛；调用方把它记成红项，并且不再逐项
 * 编故事：依赖这一步的那几项会一律报"无从判定"，而不是装作绿。
 */
export function recomputeFramediff(fd, model) {
  const sourceA = LEG_SOURCES[fd.spec.a];
  const sourceB = LEG_SOURCES[fd.spec.b];
  if (sourceA === undefined) throw new Error(`a = ${fd.spec.a} 不在 LEG_SOURCES 里`);
  if (sourceB === undefined) throw new Error(`b = ${fd.spec.b} 不在 LEG_SOURCES 里`);

  const analysis = analyzeLegPair({
    legA: pickLeg(model, sourceA),
    legB: pickLeg(model, sourceB),
    labelA: sourceA.label,
    labelB: sourceB.label,
    shape: fd.spec.shape,
  });
  if (fd.thresholdsText === null) throw new Error(`档位文件不在：${fd.spec.thresholds}`);
  const judged = judge(analysis, parseThresholds(fd.thresholdsText));
  const thresholdsInfo = thresholdsInfoOf(fd.spec.thresholds, fd.thresholdsText, judged);
  const shown = { a: sourceA.frames, b: sourceB.frames };
  const labels = { a: sourceA.label, b: sourceB.label };
  const outRel = `${RECORD_REL}/${fd.spec.slug}`;

  return {
    sources: { a: sourceA, b: sourceB },
    analysis,
    judged,
    thresholdsInfo,
    shown,
    labels,
    outRel,
    csv: renderSummaryCsv(analysis),
    verdict: `${JSON.stringify(buildVerdictJson({ analysis, judged, amp: fd.spec.amp, thresholdsInfo, shown }), null, 2)}\n`,
    // 形状只在 `--shape` 那一轮才有：没给 `--shape` 时每帧的 `shape` 是 null，
    // 拼 shape.json 会当场炸（工具那边也是靠"--shape 必须给 --out"挡住这条路）。
    shape: fd.spec.shape
      ? `${JSON.stringify(buildShapeJson({ analysis, thresholdsInfo, shown }), null, 2)}\n`
      : null,
    report: buildReport({ analysis, judged, labels, shown, outRel, thresholdsInfo }),
    diffImages: [...analysis.diffRgbaByName.keys()].sort(),
  };
}

/** 一个场景的档位该长什么样：键集与数值都钉住（多一个 `psnr_db_min` 也是变了）。 */
const THRESHOLD_SHAPE = {
  framediff: {
    keys: ['max_abs_diff_max', 'mean_ssim_min', 'min_ssim_min'],
    default: { mean_ssim_min: 1, min_ssim_min: 1, max_abs_diff_max: 0 },
    strict: [],
  },
  'framediff-crossvendor': {
    keys: ['max_abs_diff_max', 'mean_ssim_min', 'min_ssim_min'],
    default: { mean_ssim_min: 0.9995, min_ssim_min: 0.9995, max_abs_diff_max: 1 },
    // 这两个场景单独收紧到"逐字节相等"。**唯一不许的做法**就是为让某次记录变绿去动它们，
    // 所以这里逐值钉死：档位文件改一个字节，摘要先变、这条也跟着变。
    strict: ['checker', 'srgb_linear'],
  },
};

const FRAMEDIFF_JUDGES = {
  /** 这个目录里该有什么、不该有什么。 */
  'dir-listing': ({ fd }) => {
    const messages = [];
    const want = ['report.txt', 'summary.csv', 'verdict.json'];
    if (fd.spec.shape) want.push('shape.json');
    const have = [...fd.listing.files].sort();
    const missing = want.filter((name) => !have.includes(name));
    const extra = have.filter((name) => !want.includes(name));
    if (missing.length > 0) messages.push(`缺 ${missing.join('、')}`);
    if (extra.length > 0) messages.push(`多出没见过的文件：${extra.join('、')}`);
    const wantDiff = fd.spec.diffImages > 0;
    if (fd.listing.dirs.has('diff') !== wantDiff) {
      messages.push(
        wantDiff
          ? '没有 diff/ 目录——48 张差异图是这份记录的一半证据'
          : '多出 diff/ 目录——没有差异就不该写黑图充数',
      );
    }
    const extraDirs = [...fd.listing.dirs].filter((name) => name !== 'diff');
    if (extraDirs.length > 0) messages.push(`多出子目录：${extraDirs.join('、')}`);
    return messages;
  },

  /** 两份输入：帧数、帧**集合**摘要、PNG 总字节都要是锚上那两条腿。 */
  inputs: ({ fd, model, recomputed }) => {
    if (recomputed.error !== undefined) return [`重算没成（${recomputed.error}），这一项无从判定`];
    const messages = [];
    for (const side of ['a', 'b']) {
      const source = recomputed.sources[side];
      const leg = pickLeg(model, source);
      const digest = frameSetDigest(leg);
      const bytes = [...leg.frameBytes.values()].reduce((sum, buffer) => sum + buffer.length, 0);
      if (leg.frameNames.length !== EXPECTED.frames) {
        messages.push(`${side} = ${source.legKey} 有 ${leg.frameNames.length} 张 PNG，锚是 ${EXPECTED.frames} 张`);
      }
      if (digest !== source.setDigest) messages.push(`${side} = ${source.legKey} 的帧集合摘要是 ${digest}，锚是 ${source.setDigest}`);
      if (bytes !== source.pngBytes) messages.push(`${side} = ${source.legKey} 的 PNG 总字节是 ${bytes}，锚是 ${source.pngBytes}`);
      if (leg.ignoredFrames.length > 0) messages.push(`${side} = ${source.legKey} 的 frames/ 里混进非 PNG：${leg.ignoredFrames.slice(0, 4).join('、')}`);
    }
    // 判定用的两条腿必须就是上面这两条——重算时拿错一份模型，这里要说话
    if (recomputed.analysis.frames.length !== EXPECTED.frames) {
      messages.push(`重算的帧对数是 ${recomputed.analysis.frames.length}，锚是 ${EXPECTED.frames}`);
    }
    return messages;
  },

  /** 档位：文件摘要 + 逐场景解出来的闸值（两边都要对得上）。 */
  thresholds: ({ fd, recomputed }) => {
    const messages = [];
    if (fd.thresholdsText === null) return [`档位文件不在：${fd.spec.thresholds}`];
    const digest = fnv1a64(Buffer.from(fd.thresholdsText, 'utf8'));
    if (digest !== fd.spec.thresholdsDigest) {
      messages.push(`档位文件 ${fd.spec.thresholds} 的摘要是 ${digest}，锚是 ${fd.spec.thresholdsDigest}`);
    }
    if (recomputed.error === undefined && recomputed.thresholdsInfo.digest !== digest) {
      messages.push(`判定时用的档位摘要 ${recomputed.thresholdsInfo.digest} 与盘上读出来的 ${digest} 不是同一份`);
    }
    let table = null;
    try {
      table = parseThresholds(fd.thresholdsText);
    } catch (error) {
      messages.push(`档位文件解不开：${error.message}`);
      return messages;
    }
    const shape = THRESHOLD_SHAPE[fd.spec.slug];
    for (const scene of EXPECTED.scenes) {
      let limits = null;
      try {
        limits = resolveThresholds(table, scene);
      } catch (error) {
        messages.push(`场景 ${scene} 解不出档位：${error.message}`);
        continue;
      }
      messages.push(...exactKeys(`场景 ${scene} 的档位`, limits, shape.keys));
      const want = shape.strict.includes(scene) ? { ...shape.default, max_abs_diff_max: 0 } : shape.default;
      for (const key of shape.keys) {
        if (key in limits && limits[key] !== want[key]) {
          messages.push(`场景 ${scene} 的 ${key} 是 ${JSON.stringify(limits[key])}，锚是 ${JSON.stringify(want[key])}`);
        }
      }
    }
    const known = new Set(EXPECTED.scenes);
    const extra = [...table.scenarios.keys()].filter((scene) => !known.has(scene)).sort();
    if (extra.length > 0) messages.push(`档位文件里多出场景节：${extra.join('、')}——没跑过的场景不该有档`);
    return messages;
  },

  'summary-csv': ({ fd, recomputed }) => {
    if (recomputed.error !== undefined) return [`重算没成（${recomputed.error}），这一项无从判定`];
    const messages = [];
    const have = fd.text.get('summary.csv');
    if (have === undefined) return ['summary.csv 不在'];
    if (have !== recomputed.csv) {
      messages.push(`重算重渲的 summary.csv 与盘上不是同一份字节（盘上 ${Buffer.byteLength(have)}，重算 ${Buffer.byteLength(recomputed.csv)}）`);
    }
    if (fd.summaryBytes !== fd.spec.summaryCsvBytes) {
      messages.push(`summary.csv 有 ${fd.summaryBytes} 字节，锚是 ${fd.spec.summaryCsvBytes}`);
    }
    return messages;
  },

  'verdict-json': ({ fd, recomputed }) => {
    if (recomputed.error !== undefined) return [`重算没成（${recomputed.error}），这一项无从判定`];
    const messages = [];
    const have = fd.text.get('verdict.json');
    if (have === undefined) return ['verdict.json 不在'];
    if (have !== recomputed.verdict) messages.push('重算重渲的 verdict.json 与盘上不是同一份字节');
    if (fd.verdictBytes !== fd.spec.verdictJsonBytes) {
      messages.push(`verdict.json 有 ${fd.verdictBytes} 字节，锚是 ${fd.spec.verdictJsonBytes}`);
    }
    const verdict = readJsonIfPresent(join(fd.path, 'verdict.json'));
    if (verdict?.value === undefined) {
      messages.push(`verdict.json 解不开：${verdict?.error ?? '（读不到）'}`);
      return messages;
    }
    const value = verdict.value;
    messages.push(...exactKeys('verdict.json 顶层', value, KEYS.verdictTop));
    if (value.schema !== 1) messages.push(`schema=${JSON.stringify(value.schema)}`);
    if (value.kind !== 'framediff') messages.push(`kind=${JSON.stringify(value.kind)}`);
    if (value.tool !== 'scripts/dhampir-framediff.mjs') messages.push(`tool=${JSON.stringify(value.tool)}`);
    if (value.amp !== fd.spec.amp) messages.push(`amp=${JSON.stringify(value.amp)}，锚是 ${fd.spec.amp}`);
    // SSIM 的窗口口径：记录里写了就必须与守卫这几个常数是同一套（改了窗口，数值全变）
    const window = { space: 'luma', size: SSIM_WINDOW, sigma: SSIM_SIGMA, k1: SSIM_K1, k2: SSIM_K2, range: SSIM_RANGE };
    messages.push(...exactKeys('verdict.json 的 window', value.window, Object.keys(window)));
    for (const [key, want] of Object.entries(window)) {
      if (value.window?.[key] !== want) messages.push(`window.${key}=${JSON.stringify(value.window?.[key])}，锚是 ${JSON.stringify(want)}`);
    }
    for (const side of ['a', 'b']) {
      const source = recomputed.sources[side];
      const input = value.inputs?.[side];
      messages.push(...exactKeys(`verdict.json 的 inputs.${side}`, input, ['bytes_total', 'dir', 'frames', 'ignored', 'label', 'set_digest']));
      if (input?.label !== source.label) messages.push(`inputs.${side}.label=${JSON.stringify(input?.label)}，锚是 ${source.label}`);
      if (input?.dir !== source.frames) messages.push(`inputs.${side}.dir=${JSON.stringify(input?.dir)}，锚是 ${source.frames}`);
      if (input?.frames !== EXPECTED.frames) messages.push(`inputs.${side}.frames=${input?.frames}`);
      if (input?.bytes_total !== source.pngBytes) messages.push(`inputs.${side}.bytes_total=${input?.bytes_total}，锚是 ${source.pngBytes}`);
      if (input?.set_digest !== source.setDigest) messages.push(`inputs.${side}.set_digest=${JSON.stringify(input?.set_digest)}，锚是 ${source.setDigest}`);
      if (Array.isArray(input?.ignored) && input.ignored.length > 0) messages.push(`inputs.${side}.ignored 不是空的：${input.ignored.slice(0, 4).join('、')}`);
    }
    messages.push(...exactKeys('verdict.json 的 totals', value.totals, ['diff_images', 'diff_pixels', 'frames', 'scenes']));
    if (value.totals?.scenes !== EXPECTED.scenes.length) messages.push(`totals.scenes=${value.totals?.scenes}`);
    if (value.totals?.frames !== EXPECTED.frames) messages.push(`totals.frames=${value.totals?.frames}`);
    if (value.totals?.diff_pixels !== fd.spec.diffPixels) messages.push(`totals.diff_pixels=${value.totals?.diff_pixels}，锚是 ${fd.spec.diffPixels}`);
    if (value.totals?.diff_images !== fd.spec.diffImages) messages.push(`totals.diff_images=${value.totals?.diff_images}，锚是 ${fd.spec.diffImages}`);
    if (value.verdict !== 'pass') messages.push(`verdict=${JSON.stringify(value.verdict)}——这份记录该是全达标`);
    if (value.exit_code !== 0) messages.push(`exit_code=${JSON.stringify(value.exit_code)}`);
    if (!Array.isArray(value.scenes) || value.scenes.length !== EXPECTED.scenes.length) {
      messages.push(`场景数不是 ${EXPECTED.scenes.length}：${value.scenes?.length}`);
    } else {
      const names = value.scenes.map((scene) => scene.scene);
      if (names.join(',') !== [...EXPECTED.scenes].sort().join(',')) {
        messages.push(`场景表是 [${names.join('、')}]，锚是 [${[...EXPECTED.scenes].sort().join('、')}]`);
      }
      for (const scene of value.scenes) {
        if (scene.frames !== EXPECTED.framesPerScene) messages.push(`场景 ${scene.scene} 记了 ${scene.frames} 帧`);
        if (scene.verdict !== 'pass') messages.push(`场景 ${scene.scene} 记的是 ${JSON.stringify(scene.verdict)}`);
        if ((scene.failures ?? []).length > 0) messages.push(`场景 ${scene.scene} 记了失败行：${scene.failures.join('；')}`);
      }
    }
    const thresholds = fd.spec.thresholds;
    if (value.thresholds?.path !== thresholds) messages.push(`thresholds.path=${JSON.stringify(value.thresholds?.path)}`);
    if (value.thresholds?.digest !== fd.spec.thresholdsDigest) {
      messages.push(`thresholds.digest=${JSON.stringify(value.thresholds?.digest)}，锚是 ${fd.spec.thresholdsDigest}`);
    }
    return messages;
  },

  'report-txt': ({ fd, recomputed }) => {
    if (recomputed.error !== undefined) return [`重算没成（${recomputed.error}），这一项无从判定`];
    const messages = [];
    const have = fd.text.get('report.txt');
    if (have === undefined) return ['report.txt 不在'];
    if (have !== recomputed.report) messages.push('重算重渲的 report.txt 与盘上不是同一份字节');
    if (fd.reportBytes !== fd.spec.reportBytes) messages.push(`report.txt 有 ${fd.reportBytes} 字节，锚是 ${fd.spec.reportBytes}`);
    const lines = have.split('\n').length - 1;
    if (lines !== REPORT_LINES) messages.push(`report.txt 有 ${lines} 行，锚是 ${REPORT_LINES} 行`);
    if (!have.endsWith('\n')) messages.push('report.txt 结尾没有换行');
    // 屏幕上那份与文件里那份是同一个函数出的（工具如此，守卫也如此）：报告里该写着它落在哪
    if (!have.includes(`记录 → ${recomputed.outRel}/summary.csv、${recomputed.outRel}/verdict.json、${recomputed.outRel}/report.txt`)) {
      messages.push(`report.txt 里没写出记录落在哪（该是 ${recomputed.outRel}/…）`);
    }
    return messages;
  },

  'shape-json': ({ fd, recomputed }) => {
    if (recomputed.error !== undefined) return [`重算没成（${recomputed.error}），这一项无从判定`];
    const messages = [];
    const have = fd.text.get('shape.json');
    if (!fd.spec.shape) {
      if (have !== undefined) messages.push('严格档多了 shape.json——那次没给 --shape，多出来的就是别的轮次留下的');
      return messages;
    }
    if (have === undefined) return ['shape.json 不在——这份记录的形状证据全靠它'];
    if (have !== recomputed.shape) messages.push('重算重渲的 shape.json 与盘上不是同一份字节');
    if (fd.shapeBytes !== fd.spec.shapeJsonBytes) {
      messages.push(`shape.json 有 ${fd.shapeBytes} 字节，锚是 ${fd.spec.shapeJsonBytes} 字节（不是 ${codePointLength(have)} 个字符——里面有 128 列的长数组）`);
    }
    const shape = readJsonIfPresent(join(fd.path, 'shape.json'));
    if (shape?.value === undefined) {
      messages.push(`shape.json 解不开：${shape?.error ?? '（读不到）'}`);
      return messages;
    }
    if (shape.value.kind !== 'framediff-shape') messages.push(`kind=${JSON.stringify(shape.value.kind)}`);
    if (shape.value.signed_delta_definition !== 'a - b') {
      messages.push(`signed_delta_definition=${JSON.stringify(shape.value.signed_delta_definition)}`);
    }
    return messages;
  },

  'diff-images': ({ fd, recomputed }) => {
    if (recomputed.error !== undefined) return [`重算没成（${recomputed.error}），这一项无从判定`];
    const messages = [];
    const names = recomputed.diffImages;
    if (fd.diffNames.join(',') !== names.join(',')) {
      const extra = fd.diffNames.filter((name) => !names.includes(name));
      const missing = names.filter((name) => !fd.diffNames.includes(name));
      const parts = [];
      if (extra.length > 0) parts.push(`多出 ${extra.slice(0, 4).join('、')}`);
      if (missing.length > 0) parts.push(`少了 ${missing.slice(0, 4).join('、')}`);
      messages.push(`diff/ 里的名字与重算出来的不是一个集合${parts.length > 0 ? `（${parts.join('；')}）` : '（顺序不同）'}`);
    }
    if (fd.diffNames.length !== fd.spec.diffImages) {
      messages.push(`diff/ 里有 ${fd.diffNames.length} 张，锚是 ${fd.spec.diffImages} 张`);
    }
    let different = 0;
    const first = [];
    for (const name of names) {
      const entry = recomputed.analysis.diffRgbaByName.get(name);
      const onDisk = fd.diffBytes.get(name);
      if (onDisk === undefined) continue;
      const encoded = encodePng(entry.width, entry.height, ampRgb(entry.rgba, fd.spec.amp));
      if (!encoded.equals(onDisk)) {
        different += 1;
        if (first.length < 3) first.push(`${name}（盘上 ${onDisk.length} vs 重编码 ${encoded.length} 字节）`);
      }
    }
    if (different > 0) messages.push(`${different} 张差异图重编码后与盘上不是同一份字节：${first.join('、')}`);
    return messages;
  },
};

/** 逐项判一份比对记录。返回**每一项都有一条结论**的数组（顺序与 `FRAMEDIFF_CHECKS` 一致）。 */
export function judgeFramediff({ fd, model, recomputed }) {
  return FRAMEDIFF_CHECKS.map((id) => {
    let messages;
    try {
      messages = FRAMEDIFF_JUDGES[id]({ fd, model, recomputed });
    } catch (error) {
      messages = [`检查自己抛了异常：${error.message}`];
    }
    const shown = messages.slice(0, 6);
    if (messages.length > shown.length) shown.push(`…另有 ${messages.length - shown.length} 处`);
    return { id, ok: messages.length === 0, detail: shown.join('；') };
  });
}

/** 真跑走的就是这条路：先重算一次，再逐项判。 */
export function checkFramediffDir(fd, model) {
  let recomputed;
  try {
    recomputed = recomputeFramediff(fd, model);
  } catch (error) {
    recomputed = { error: error.message };
  }
  return judgeFramediff({ fd, model, recomputed });
}

// ---------------------------------------------------------------------------
// 诚实性层：记录里写给人看的那几句，对不对
//
// 上面几层判的全是数字。这一层判的是**记录怎么说自己**：README 的结论、wasm 侧那份
// 10/10、验收快照、独立复核报告。它们最容易变成"当时对、后来改了记录忘了改文字"——
// 数字层会红，文字层没人查。M2 的 README 就真写错过一句（把两条腿说成逐字节相同），
// 所以这一层除了"必须写什么"，还钉了"不许写什么"。
//
// 与数字层同一条规矩：**不要求文字"看起来像证据"，只要求说出口的话都查得到。**
// 查不到时要能点名是**哪一条**，所以锚是逐字字面量，不是关键词。
// ---------------------------------------------------------------------------

export const HONESTY_CHECKS = [
  'root-listing',
  'readme-claims',
  'wasm-tests',
  'acceptance',
  'review-independent',
];

/** `wasm-tests.json` 的键集合（逐字）。 */
const WASM_KEYS = {
  top: [
    'command', 'crate', 'exit_code', 'failed', 'kind', 'listed_total', 'milestone', 'node',
    'node_exit_shim', 'passed', 'runner', 'schema', 'source_wasm_bindgen_tests', 'target',
    'targets', 'wasm_bindgen_locked',
  ],
  runner: ['path', 'version'],
  shim: ['applied', 'nota_bene', 'override', 'platform', 'reason', 'shim', 'timeout_ms', 'why'],
  source: ['count', 'files'],
  target: [
    'exit_code', 'failed', 'listed_tests', 'log', 'node_crash_text_seen', 'passed', 'ran',
    'target', 'timed_out', 'wasm', 'wasm_runnable',
  ],
};

/**
 * `wasm-tests.json` 那 10 条测试**是从哪两个文件里数出来的**。
 *
 * `prefix` 是记录里给该文件里的测试加的模块前缀（`corpus.rs` 在 `mod tests` 里，
 * 所以列出来带 `corpus::tests::`；`cross_runtime.rs` 是集成测试，直接用函数名）。
 */
const WASM_SOURCE_FILES = [
  { path: 'crates/dhampir-wasm/src/corpus.rs', prefix: 'corpus::tests::' },
  { path: 'crates/dhampir-wasm/tests/cross_runtime.rs', prefix: '' },
];

/**
 * 从源码里**数**出 `#[wasm_bindgen_test]` 函数名。
 *
 * 这里是本守卫的独立重写之一：不读记录里的 `count`，而是自己扫一遍源码。
 * 两个坑都踩过：
 *
 *   · 注释里的 `#[wasm_bindgen_test]`（`corpus.rs` 里有一段解释"为什么用它不用 #[test]"
 *     的注释，逐字包含这个属性）——**跳过整行注释**，否则数出来是 8 不是 6；
 *   · 属性与 `fn` 之间可能夹着别的属性（`#[cfg(...)]` 之类）——一直往下找到第一行
 *     真正的代码行再取名，而不是只看下一行。
 */
export function wasmTestFns(text) {
  const lines = String(text).split('\n');
  const names = [];
  for (let i = 0; i < lines.length; i += 1) {
    const trimmed = lines[i].trim();
    if (trimmed.startsWith('//')) continue;
    if (!/^#\[wasm_bindgen_test(\s*\([^)]*\))?\]$/.test(trimmed)) continue;
    for (let j = i + 1; j < lines.length; j += 1) {
      const next = lines[j].trim();
      if (next.length === 0 || next.startsWith('//') || next.startsWith('#[')) continue;
      const match = /^(?:pub\s+)?(?:async\s+)?fn\s+([A-Za-z_][A-Za-z0-9_]*)/.exec(next);
      if (match) names.push(match[1]);
      break;
    }
  }
  return names;
}

/** `acceptance.json` 顶层键（逐字），与 `record-acceptance.mjs` 落盘的那一份对齐。 */
const ACCEPTANCE_KEYS = [
  'commit', 'criteria', 'dirty', 'exit_code', 'generated_at', 'green', 'milestone', 'node',
  'schema', 'source', 'title',
];

/** 每条判据**必须**有这几栏；`note` / `tests` 是可选的补充。 */
const ACCEPTANCE_ITEM_REQUIRED = ['command', 'exit_code', 'id', 'ok', 'says', 'seconds'];
const ACCEPTANCE_ITEM_OPTIONAL = ['note', 'tests'];

/**
 * `records/` 里那几份给人读的文本，也得守"LF + 无 BOM"。
 *
 * 全仓的 `check-text-hygiene.mjs` **跳过 `records/`**（那里面是证据，不是源码），
 * 于是这块没人管——而它恰恰是最容易踩 BOM 的地方：一次 `Set-Content` 就带上 EF BB BF，
 * 文件照样能读、diff 里只多三个看不见的字节。
 */
export function recordByteHygiene(where, bytes) {
  if (bytes === null || bytes === undefined) return [];
  const messages = [];
  if (bytes.length >= 3 && bytes[0] === 0xef && bytes[1] === 0xbb && bytes[2] === 0xbf) {
    messages.push(`${where} 带 UTF-8 BOM（EF BB BF）——` + '`records/` 不在 text-hygiene 的管辖里，这里得自己管');
  }
  if (bytes.includes(0x0d)) {
    messages.push(`${where} 里有 CR（0x0D）——这份记录该是纯 LF`);
  }
  return messages;
}

/**
 * 把一段 Markdown 逐字符**摊成只剩下的字**：去掉强调与行内代码的记号，空白折成一个空格。
 *
 * 为什么不能直接 `text.includes(anchor)`：README 里那句话实际长这样——
 *
 *     `readings.txt` 两份**逐字节相同**（同一份测量文本），`run.json` 也逐字节相同
 *
 * 逐字找 `readings.txt 两份逐字节相同` 是**找不到**的（中间夹了反引号与 `**`），
 * 于是"字面量禁令"会悄悄变成一条永真检查——这正是它要防的那种失效。
 * 摊平之后两边都只剩字，记号怎么排都拦得住，而且**折行**也拦得住
 * （文档按 100 列折行，`38 行` 有可能被折成 `38\n行`）。
 *
 * 返回 `{ flat, lineOf }`：`lineOf[i]` 是 `flat[i]` 在**原文**里的行号（1 基）。
 * 摊平会吃掉换行，行号只能边走边记——报出来的行号得能在编辑器里对得上。
 */
export function flattenProse(text) {
  const raw = String(text);
  let flat = '';
  const lineOf = [];
  let pendingSpace = false;
  let line = 1;
  for (let i = 0; i < raw.length; i += 1) {
    const ch = raw[i];
    if (ch === '\n') {
      line += 1;
      pendingSpace = true;
      continue;
    }
    if (ch === '`' || ch === '*' || ch === '_' || ch === '~') continue;
    if (ch === ' ' || ch === '\t' || ch === '\r') {
      pendingSpace = true;
      continue;
    }
    if (pendingSpace && flat.length > 0) {
      flat += ' ';
      lineOf.push(line);
    }
    pendingSpace = false;
    flat += ch;
    lineOf.push(line);
  }
  return { flat, lineOf };
}

/** 只要摊平后的那串字。 */
export function normalizeProse(text) {
  return flattenProse(text).flat;
}

const HONESTY_JUDGES = {

  'root-listing': ({ model }) => {
    const messages = [];
    const have = model.listing;
    const required = new Set([...EXPECTED.rootFiles, ...EXPECTED.subdirs]);
    const allowed = new Set([...required, ...EXPECTED.optionalRootFiles]);
    // 验收快照那 13 份 `<判据 id>.txt` 也是允许的（后到，不要求存在）。
    for (const id of EXPECTED.acceptanceIds) allowed.add(`${id}.txt`);

    const missing = [...required].filter((name) => !have.names.has(name)).sort();
    if (missing.length > 0) messages.push(`少了 ${missing.length} 项：${missing.slice(0, 4).join('、')}`);
    const extra = [...have.names].filter((name) => !allowed.has(name)).sort();
    if (extra.length > 0) {
      messages.push(`多出 ${extra.length} 项没人认领的：${extra.slice(0, 4).join('、')}` + '——旧截图、草稿、跑了一半的输出不该悄悄留着');
    }
    for (const name of EXPECTED.rootFiles) {
      if (have.dirs.has(name)) {
        messages.push(`${name} 是目录，锚里它是个文件`);
        continue;
      }
      if (have.names.has(name) && have.bytes.get(name) === 0) messages.push(`${name} 是 0 字节`);
    }
    for (const name of EXPECTED.subdirs) {
      if (have.files.has(name)) messages.push(`${name} 是文件，锚里它是个目录`);
    }
    const readmeBytes = have.bytes.get('README.md');
    if (readmeBytes !== undefined && readmeBytes < EXPECTED.readmeMinBytes) {
      messages.push(`README.md 只有 ${readmeBytes} 字节，短于锚 ${EXPECTED.readmeMinBytes}——它是这份记录唯一的读入口`);
    }
    return messages;
  },

  'readme-claims': ({ model }) => {
    const messages = [];
    if (model.readmeText === null) return ['README.md 不在——这份记录没有读入口'];
    messages.push(...recordByteHygiene('README.md', model.readmeBytes));
    const { flat, lineOf } = flattenProse(model.readmeText);
    for (const anchor of EXPECTED.readmeAnchors) {
      if (!flat.includes(normalizeProse(anchor))) messages.push(`README.md 里找不到 ${JSON.stringify(anchor)}`);
    }
    for (const banned of EXPECTED.readmeForbidden) {
      const at = flat.indexOf(normalizeProse(banned));
      if (at >= 0) {
        messages.push(`README.md 第 ${lineOf[at]} 行还留着已被实测推翻的说法：${JSON.stringify(banned)}`);
      }
    }
    return messages;
  },

  'wasm-tests': ({ model }) => {
    const got = model.wasmTests;
    if (got === null) return ['wasm-tests.json 不在——wasm 侧那 10/10 没有原文'];
    if (got.value === undefined) return [`wasm-tests.json 解不开：${got.error}`];
    const value = got.value;
    const messages = [];
    messages.push(...exactKeys('wasm-tests.json 顶层', value, WASM_KEYS.top));
    if (value.schema !== 1) messages.push(`schema=${JSON.stringify(value.schema)}`);
    if (value.kind !== 'wasm-tests') messages.push(`kind=${JSON.stringify(value.kind)}`);
    // 这里的 `M0` 是**记录契约版本**（谁写这份形状），不是"哪个里程碑跑的"：
    // 同一条 wasm 判据 M0 立的、M1 与 M2 各重跑过一遍，形状没变过。
    if (value.milestone !== 'M0') {
      messages.push(`milestone=${JSON.stringify(value.milestone)}——这栏是记录形状的版本（那份形状 M0 立的），不是"哪个里程碑跑的"`);
    }
    if (value.target !== 'wasm32-unknown-unknown') messages.push(`target=${JSON.stringify(value.target)}`);
    if (value.crate !== 'dhampir-wasm') messages.push(`crate=${JSON.stringify(value.crate)}`);
    messages.push(...exactKeys('wasm-tests.json 的 runner', value.runner, WASM_KEYS.runner));
    const shim = value.node_exit_shim;
    messages.push(...exactKeys('wasm-tests.json 的 node_exit_shim', shim, WASM_KEYS.shim));
    // win32 上这份记录的前提：不经垫片，"跑过了"与"被 libuv 断言吃掉"分不开。
    if (shim?.applied !== true) {
      messages.push(`node_exit_shim.applied=${JSON.stringify(shim?.applied)}——win32 上没接管进程退出，这份记录说明不了测试真跑过`);
    }

    const listed = [];
    const targets = value.targets;
    if (!Array.isArray(targets) || targets.length !== EXPECTED.wasmTests.targets) {
      messages.push(`targets 不是 ${EXPECTED.wasmTests.targets} 个：${targets?.length ?? '（不是数组）'}`);
    } else {
      for (const target of targets) {
        const where = `wasm-tests.json 的 targets[${JSON.stringify(target?.target)}]`;
        messages.push(...exactKeys(where, target, WASM_KEYS.target));
        if (target.exit_code !== 0) messages.push(`${target.target} 退出码 ${JSON.stringify(target.exit_code)}`);
        if (target.failed !== 0) messages.push(`${target.target} 有 ${target.failed} 条失败`);
        if (target.ran !== true) messages.push(`${target.target} 没跑起来（ran=${JSON.stringify(target.ran)}）`);
        if (target.wasm_runnable !== true) {
          messages.push(`${target.target} 的 wasm 不是可运行的（wasm_runnable=${JSON.stringify(target.wasm_runnable)}）`);
        }
        if (target.timed_out !== false) messages.push(`${target.target} timed_out=${JSON.stringify(target.timed_out)}`);
        if (target.node_crash_text_seen !== false) {
          messages.push(`${target.target} 的日志里出现了 node 崩溃文本——那不是测试失败，是没结论`);
        }
        if (!Array.isArray(target.listed_tests)) {
          messages.push(`${target.target} 的 listed_tests 不是数组`);
        } else {
          if (target.listed_tests.length !== target.passed) {
            messages.push(`${target.target} 列了 ${target.listed_tests.length} 条却报 passed=${target.passed}——列出来的每一条都该是跑过的`);
          }
          listed.push(...target.listed_tests);
        }
      }
    }
    if (value.listed_total !== EXPECTED.wasmTests.listedTotal) {
      messages.push(`listed_total=${JSON.stringify(value.listed_total)}，锚是 ${EXPECTED.wasmTests.listedTotal}`);
    }
    if (value.passed !== EXPECTED.wasmTests.passed) messages.push(`passed=${JSON.stringify(value.passed)}`);
    if (value.failed !== EXPECTED.wasmTests.failed) messages.push(`failed=${JSON.stringify(value.failed)}`);
    if (value.exit_code !== 0) messages.push(`exit_code=${JSON.stringify(value.exit_code)}`);
    if (listed.length !== EXPECTED.wasmTests.listedTotal) {
      messages.push(`两份 targets 加起来列了 ${listed.length} 条测试，锚是 ${EXPECTED.wasmTests.listedTotal} 条`);
    }

    // ---- 把 10 条测试**重新数一遍** ----
    // 记录里的 `count: 10` 是抄的；这里自己扫源码里的 `#[wasm_bindgen_test]`。
    // 谁改了一个属性、或者注释掉一条测试而没重跑，都会在这里露出来。
    const source = value.source_wasm_bindgen_tests;
    messages.push(...exactKeys('wasm-tests.json 的 source_wasm_bindgen_tests', source, WASM_KEYS.source));
    const files = Array.isArray(source?.files) ? source.files : [];
    const derived = [];
    for (const spec of WASM_SOURCE_FILES) {
      const text = readTextIfPresent(join(REPO_ROOT, spec.path));
      if (text === null) {
        messages.push(`源码不在：${spec.path}`);
        continue;
      }
      const names = wasmTestFns(text);
      for (const name of names) derived.push(`${spec.prefix}${name}`);
      const entry = `${spec.path}×${names.length}`;
      if (!files.includes(entry)) {
        messages.push(`source_wasm_bindgen_tests.files 里没有 ${entry}（源码里数出来就是 ${names.length} 个 #[wasm_bindgen_test]）`);
      }
    }
    if (source?.count !== derived.length) {
      messages.push(`source_wasm_bindgen_tests.count=${JSON.stringify(source?.count)}，源码里数出来是 ${derived.length}`);
    }
    const notInSource = listed.filter((name) => !derived.includes(name));
    if (notInSource.length > 0) {
      messages.push(`记录里列了 ${notInSource.length} 条源码里没有的测试：${notInSource.slice(0, 4).join('、')}`);
    }
    const notListed = derived.filter((name) => !listed.includes(name));
    if (notListed.length > 0) {
      messages.push(`源码里有 ${notListed.length} 条 #[wasm_bindgen_test] 没被列出来：${notListed.slice(0, 4).join('、')}`);
    }
    return messages;
  },

  'acceptance': ({ model }) => {
    const messages = [];
    const txts = EXPECTED.acceptanceIds.map((id) => `${id}.txt`);
    const present = txts.filter((name) => model.listing.names.has(name));
    const got = model.acceptance;
    if (got === null) {
      // **不要求它在**：13 条判据里有一条就是"把本守卫跑一遍"，要求它存在就成了循环。
      // 但"跑了一半"必须看得出来——有判据原文却没有总表，读的人不知道那是不是全部。
      if (present.length > 0) {
        messages.push(`有 ${present.length} 份判据原文（${present.slice(0, 3).join('、')}…）却没有 acceptance.json——半份记录`);
      }
      return messages;
    }
    if (got.value === undefined) return [`acceptance.json 解不开：${got.error}`];
    const value = got.value;
    messages.push(...recordByteHygiene('acceptance.json', model.acceptanceBytes));
    messages.push(...exactKeys('acceptance.json 顶层', value, ACCEPTANCE_KEYS));
    if (value.schema !== 1) messages.push(`schema=${JSON.stringify(value.schema)}`);
    if (value.milestone !== 'm2') messages.push(`milestone=${JSON.stringify(value.milestone)}`);
    if (value.green !== true) messages.push(`green=${JSON.stringify(value.green)}——这份快照没全绿，它就不能算验收`);
    if (value.exit_code !== 0) messages.push(`exit_code=${JSON.stringify(value.exit_code)}`);
    // `dirty: null` 是"未知"（git 不可用），与 `false` 是两件事——只有 `true` 判红。
    if (value.dirty === true) {
      messages.push('dirty=true——记录是在一棵脏树上跑的，`commit` 那一栏代表不了记录内容');
    }

    const criteria = Array.isArray(value.criteria) ? value.criteria : [];
    const ids = criteria.map((item) => item?.id);
    const notRun = EXPECTED.acceptanceIds.filter((id) => !ids.includes(id));
    const extraIds = ids.filter((id) => !EXPECTED.acceptanceIds.includes(id));
    if (notRun.length > 0) messages.push(`${notRun.length} 条判据没跑：${notRun.slice(0, 4).join('、')}`);
    if (extraIds.length > 0) messages.push(`多出 ${extraIds.length} 条判据：${extraIds.slice(0, 4).join('、')}`);
    for (const item of criteria) {
      const missing = ACCEPTANCE_ITEM_REQUIRED.filter((key) => !(key in (item ?? {})));
      if (missing.length > 0) {
        messages.push(`判据 ${JSON.stringify(item?.id)} 缺栏：${missing.join('、')}`);
        continue;
      }
      const unknown = Object.keys(item).filter(
        (key) => !ACCEPTANCE_ITEM_REQUIRED.includes(key) && !ACCEPTANCE_ITEM_OPTIONAL.includes(key),
      );
      if (unknown.length > 0) messages.push(`判据 ${item.id} 多出没认领的栏：${unknown.slice(0, 4).join('、')}`);
      if (item.ok !== true) messages.push(`判据 ${item.id} 记的是 ok=${JSON.stringify(item.ok)}`);
      if (item.exit_code !== 0) messages.push(`判据 ${item.id} 记的是 exit_code=${JSON.stringify(item.exit_code)}`);
    }
    // 逐条判据都要有**原样输出**：只有一句"✓"的验收表是没法复核的。
    for (const id of EXPECTED.acceptanceIds) {
      if (!model.listing.names.has(`${id}.txt`)) {
        messages.push(`${id}.txt 不在——判据认的是退出码，原样输出得留下来`);
        continue;
      }
      messages.push(...recordByteHygiene(`${id}.txt`, readBytesIfPresent(join(model.dir, `${id}.txt`))));
    }
    const extras = [...model.listing.names]
      .filter((name) => name.endsWith('.txt') && !txts.includes(name))
      .sort();
    if (extras.length > 0) messages.push(`多出 ${extras.length} 份没人认领的 .txt：${extras.slice(0, 4).join('、')}`);
    return messages;
  },

  'review-independent': ({ model }) => {
    const bytes = model.reviewBytes;
    // 后到的产物：复核者读完全部记录才写，**不要求存在**。
    if (bytes === null) return [];
    const messages = [];
    messages.push(...recordByteHygiene('review-independent.md', bytes));
    if (bytes.length < EXPECTED.reviewMinBytes) {
      messages.push(`review-independent.md 只有 ${bytes.length} 字节，短于锚 ${EXPECTED.reviewMinBytes}——"我看过了，没问题"不算复核`);
    }
    const text = model.reviewText ?? bytes.toString('utf8');
    // 一份复核报告至少得提到**一个本记录里钉住的值**：没提到就说明它没拿记录里的数字当证据。
    const anchors = [
      ...EXPECTED.legs.map((leg) => leg.framesDigest),
      ...EXPECTED.legs.map((leg) => leg.setDigest),
      EXPECTED.native.framesDigest,
      EXPECTED.native.setDigest,
      ...EXPECTED.framediff.map((spec) => spec.thresholdsDigest),
      String(EXPECTED.framediff[1].diffPixels),
    ];
    if (!anchors.some((anchor) => text.includes(anchor))) {
      messages.push(`复核报告里一个本记录钉住的值都没提到（比如 ${anchors[0]}）——没拿记录里的数字当证据`);
    }
    if (!/PASS|FAIL|通过|不通过/.test(text)) {
      messages.push('复核报告里找不到结论（PASS / FAIL / 通过 / 不通过）');
    }
    return messages;
  },
};

/** 逐项判"记录怎么说自己"。返回**每一项都有一条结论**的数组（顺序与 `HONESTY_CHECKS` 一致）。 */
export function judgeHonesty({ model }) {
  return HONESTY_CHECKS.map((id) => {
    let messages;
    try {
      messages = HONESTY_JUDGES[id]({ model });
    } catch (error) {
      messages = [`检查自己抛了异常：${error.message}`];
    }
    const shown = messages.slice(0, 6);
    if (messages.length > shown.length) shown.push(`…另有 ${messages.length - shown.length} 处`);
    return { id, ok: messages.length === 0, detail: shown.join('；') };
  });
}

/** 真跑走的也是这条路：不重算，只读记录（重算那部分在 `checkFramediffDir` 里）。 */
export function checkRecordHonesty(model) {
  return judgeHonesty({ model });
}


// ===========================================================================
// 自检与命令行
//
// 为什么自检的**基线是真记录**，而不是合成一份自洽的假模型：
// 上面检查实现里有 79 处 `EXPECTED.*`，锚是真实跑出来的摘要与字节数
// （4bc004b502a1301a / 129619 / 1105968 / 38 行 / 第 22 行 …）。
// 合成数据**不可能**满足这些锚——换一批自洽的假数据正是这些锚要抓的东西。
// 所以：基线 = 真记录；反向用例 = 对真模型做**定向改动**，断言"指定的那一项变红"。
// ===========================================================================

/** 深拷贝：Buffer / Set / Map / 数组 / 普通对象。模型全靠它。 */
function deepClone(value) {
  if (value === null || typeof value !== 'object') return value;
  if (Buffer.isBuffer(value)) return Buffer.from(value);
  if (value instanceof Set) return new Set([...value].map(deepClone));
  if (value instanceof Map) {
    const out = new Map();
    for (const [key, item] of value) out.set(key, deepClone(item));
    return out;
  }
  if (Array.isArray(value)) return value.map(deepClone);
  const out = {};
  for (const key of Object.keys(value)) out[key] = deepClone(value[key]);
  return out;
}

/** 检查项的完整清单（37 项：15 + 3 + 8 + 6 + 5）。自检按它实数上报覆盖。 */
export const ALL_CHECK_IDS = [
  ...LEG_CHECKS,
  ...NATIVE_CHECKS.map((id) => 'native-' + id),
  ...FRAMEDIFF_CHECKS,
  ...CROSS_CHECKS,
  ...HONESTY_CHECKS,
];

/** 检查项的 spec 是**两层合起来**的：语料级（scenes/frames/frameRange…）+ 腿级（pngBytes/inPage…）。 */
function legSpecOf(spec) {
  return { ...EXPECTED, ...spec };
}

function nativeSpec() {
  return { ...EXPECTED, ...EXPECTED.native };
}

/** 按分组跑一组检查。自检只跑与改动相关的那一组，避免 37 次全量重算。 */
function runGroup(group, model, scope) {
  if (group === 'leg') {
    const spec = EXPECTED.legs.find((item) => item.slug === scope);
    if (spec === undefined) throw new Error('未知腿 ' + scope);
    return checkLegModel(model.legs[scope], legSpecOf(spec));
  }
  if (group === 'native') return checkNativeLeg(model.native, nativeSpec());
  if (group === 'cross') {
    return checkCrossLegs({
      browser: model.legs['browser'],
      amd: model.legs['browser-amd'],
      native: model.native,
    });
  }
  if (group === 'fd') return checkFramediffDir(model.framediff[scope], model);
  if (group === 'honesty') return checkRecordHonesty(model);
  throw new Error('未知分组 ' + group);
}

/**
 * 反向用例。
 *
 * 每条只改**一处**，并指名它该被哪一项抓到。`scope`：leg 用腿名、fd 用下标、其余为 null。
 */
const MUTATIONS = [
  // ---- 腿（15）----
  { name: '少一份 host-gpu.json', group: 'leg', scope: 'browser', expect: 'required-files',
    mutate: (m) => { m.legs.browser.rootFiles.delete('host-gpu.json'); } },
  { name: 'run.json 少一个顶层键', group: 'leg', scope: 'browser', expect: 'run-shape',
    mutate: (m) => { delete m.legs.browser.run.value.byte_tolerance; } },
  { name: '少一张 PNG', group: 'leg', scope: 'browser', expect: 'frame-set',
    mutate: (m) => { m.legs.browser.frameNames.splice(3, 1); } },
  { name: '改一帧的 png_bytes', group: 'leg', scope: 'browser', expect: 'png-bytes-and-digest',
    mutate: (m) => { m.legs.browser.run.value.backends[0].frames[7].png_bytes += 1; } },
  { name: '改一帧的 pixel_digest（文件没动）', group: 'leg', scope: 'browser', expect: 'pixels',
    mutate: (m) => { m.legs.browser.run.value.backends[0].frames[7].pixel_digest = '0000000000000000'; } },
  { name: '一帧的同帧两次渲染不一致', group: 'leg', scope: 'browser', expect: 'repeat',
    mutate: (m) => { m.legs.browser.run.value.backends[0].frames[9].repeat_identical = false; } },
  { name: '改整表摘要', group: 'leg', scope: 'browser', expect: 'frames-digest',
    mutate: (m) => { m.legs.browser.run.value.backends[0].frames_digest = '0000000000000000'; } },
  { name: '改 counts.points', group: 'leg', scope: 'browser', expect: 'counts',
    mutate: (m) => { m.legs.browser.run.value.backends[0].counts.points -= 1; } },
  { name: '把一个采样点判成未通过', group: 'leg', scope: 'browser', expect: 'points',
    mutate: (m) => { m.legs.browser.run.value.backends[0].frames[4].points[0].passed = false; } },
  { name: '采样点的 measured 与 PNG 对不上', group: 'leg', scope: 'browser', expect: 'measured-vs-png',
    mutate: (m) => { m.legs.browser.run.value.backends[0].frames[0].points[0].measured[0] += 1; } },
  { name: 'readings.txt 多一个字节', group: 'leg', scope: 'browser', expect: 'readings',
    mutate: (m) => { m.legs.browser.readingsBytes = Buffer.concat([m.legs.browser.readingsBytes, Buffer.from('x')]); } },
  { name: '改 adapter.json 的 probe_digest', group: 'leg', scope: 'browser', expect: 'adapter',
    mutate: (m) => { m.legs.browser.adapter.value.probe_digest = '0000000000000000'; } },
  { name: 'host-gpu.json 的 resolved 清空', group: 'leg', scope: 'browser', expect: 'host-gpu',
    mutate: (m) => { m.legs.browser.hostGpu.value.resolved = null; } },
  { name: '截图 json 的 frames_verified 改掉', group: 'leg', scope: 'browser', expect: 'screenshot',
    mutate: (m) => { m.legs.browser.screenshot.value.round.frames_verified = 1; } },
  { name: '复跑佐证的比较帧数改掉', group: 'leg', scope: 'browser-amd', expect: 'rerun-repro',
    mutate: (m) => { m.legs['browser-amd'].rerunRepro.value.frames.compared -= 1; } },

  // ---- native 归档（3）----
  { name: 'native 的 probe_digest 改掉', group: 'native', scope: null, expect: 'native-archive',
    mutate: (m) => { m.native.adapter.value.probe_digest = '0000000000000000'; } },
  { name: 'native 的整表摘要改掉', group: 'native', scope: null, expect: 'native-frames-digest',
    mutate: (m) => { m.native.run.value.backends[0].frames_digest = '0000000000000000'; } },
  { name: 'native 的 readings 字节数改掉', group: 'native', scope: null, expect: 'native-readings',
    mutate: (m) => { m.native.readingsBytes = Buffer.from('x'); } },

  // ---- 跨腿（6）----
  { name: '浏览器腿的帧集摘要改掉', group: 'cross', scope: null, expect: 'browser-vs-native-bytes',
    mutate: (m) => { m.legs.browser.setDigest = '0000000000000000'; } },
  { name: '把 AMD 的 readings 换成与 native 相同', group: 'cross', scope: null, expect: 'amd-vs-native-readings',
    mutate: (m) => { m.legs['browser-amd'].readingsBytes = Buffer.from(m.legs.browser.readingsBytes); } },
  { name: '把浏览器腿的 adapter 换成 AMD 那份', group: 'cross', scope: null, expect: 'adapter-drift',
    mutate: (m) => { m.legs.browser.adapter.value = deepClone(m.legs['browser-amd'].adapter.value); } },
  { name: '截图里某项该不同却改成相同', group: 'cross', scope: null, expect: 'screenshot-drift',
    mutate: (m) => { m.legs.browser.screenshot.value.browser.extra_args = ['--force_low_power_gpu']; } },
  { name: 'run.json 多出一条与 native 的差异路径', group: 'cross', scope: null, expect: 'run-json-drift',
    mutate: (m) => { m.legs.browser.run.value.byte_tolerance = 999; } },
  { name: '两条腿的 adapter.json 字节改成一样', group: 'cross', scope: null, expect: 'leg-distinctness',
    mutate: (m) => { m.legs.browser.adapterBytes = Buffer.from(m.legs['browser-amd'].adapterBytes); } },

  // ---- framediff（8）----
  { name: '比对目录少一个文件', group: 'fd', scope: 0, expect: 'dir-listing',
    mutate: (m) => { m.framediff[0].listing.files.delete('report.txt'); } },
  { name: '输入腿少一张 PNG', group: 'fd', scope: 0, expect: 'inputs',
    mutate: (m) => { m.legs.browser.frameNames.pop(); } },
  { name: '档位文本读不出来', group: 'fd', scope: 0, expect: 'thresholds',
    mutate: (m) => { m.framediff[0].thresholdsText = ''; } },
  { name: 'summary.csv 改一个字节', group: 'fd', scope: 0, expect: 'summary-csv',
    mutate: (m) => { m.framediff[0].text.set('summary.csv', 'x'); } },
  { name: 'verdict.json 改一个字节', group: 'fd', scope: 0, expect: 'verdict-json',
    mutate: (m) => { m.framediff[0].text.set('verdict.json', 'x'); } },
  { name: 'report.txt 改一个字节', group: 'fd', scope: 0, expect: 'report-txt',
    mutate: (m) => { m.framediff[0].text.set('report.txt', 'x'); } },
  { name: '跨厂商 shape.json 改一个字节', group: 'fd', scope: 1, expect: 'shape-json',
    mutate: (m) => { m.framediff[1].text.set('shape.json', 'x'); } },
  { name: '跨厂商少一张差异图', group: 'fd', scope: 1, expect: 'diff-images',
    mutate: (m) => { m.framediff[1].diffNames.pop(); } },

  // ---- 诚实性（5）----
  { name: '根目录多一个没人认领的文件', group: 'honesty', scope: null, expect: 'root-listing',
    mutate: (m) => { m.listing.names.add('stray.txt'); } },
  { name: 'README 抠掉一个结论数字', group: 'honesty', scope: null, expect: 'readme-claims',
    mutate: (m) => { m.readmeText = String(m.readmeText).replace('71ecc80cade3d73d', '0000000000000000'); } },
  { name: 'wasm-tests 的 passed 改掉', group: 'honesty', scope: null, expect: 'wasm-tests',
    mutate: (m) => { m.wasmTests.value.passed = 9; } },
  { name: '验收快照只有半份', group: 'honesty', scope: null, expect: 'acceptance',
    mutate: (m) => { m.acceptance = { value: { ok: true } }; } },
  { name: '独立复核报告是空壳', group: 'honesty', scope: null, expect: 'review-independent',
    mutate: (m) => { m.reviewBytes = Buffer.from('ok'); m.reviewText = 'ok'; } },
];

/**
 * 把**后到的、自指的**产物从模型里摘掉，得到自检基线。
 *
 * 为什么必须摘：`acceptance.json` 与 13 份判据原文里有一条判据就是"把本守卫跑一遍"，
 * 而 `acceptance` 那一项又要求"在就必须整份绿"。于是**只要盘上留着一份红的快照**，
 * 体检基线就永远不绿 → 自检失败 → 守卫退 2 → 判据永远补不绿 = 死锁。
 *
 * 摘掉不等于不查：真跑（`--record`）照样按原样加载、红的照红；自检里那两条反向用例
 * 也是**主动塞进**坏快照来验证判据会红（见 `MUTATIONS`）。这里只是让"基线"测的是记录本体，
 * 而不是一个依赖守卫自身结论的产物。
 */
function stripOptionalArtifacts(model) {
  model.acceptance = null;
  model.acceptanceBytes = null;
  model.reviewText = null;
  model.reviewBytes = null;
  for (const name of [...model.listing.names]) {
    if (name === 'acceptance.json' || name === 'review-independent.md' || name.endsWith('.txt')) {
      model.listing.names.delete(name);
      model.listing.files.delete(name);
      model.listing.bytes.delete(name);
    }
  }
  return model;
}

/**
 * 自检。返回 `{ failures, count, covered, total }`。
 *
 * 与 M1 同形：`expect(name, condition, detail)` **真数断言条数**——守卫报的每个数都是结论的一部分。
 */
export function runSelfTest() {
  const failures = [];
  let count = 0;
  const expect = (name, condition, detail) => {
    count += 1;
    if (!condition) failures.push(name + '：' + detail);
  };

  if (!existsSync(DEFAULT_RECORD)) {
    failures.push('自检基线不在 ' + DEFAULT_RECORD + '——反向用例以真记录为基线，没有它就无从自检');
    return { failures, count, covered: 0, total: ALL_CHECK_IDS.length };
  }
  const base = stripOptionalArtifacts(loadRecord(DEFAULT_RECORD, 'records/m2'));

  // ---- ① 基线必须全绿。基线不绿说明记录或守卫本身有问题，后面的"因为那一条红"就没有意义 ----
  const baselineGroups = [
    ['leg', 'browser', LEG_CHECKS.length],
    ['leg', 'browser-amd', LEG_CHECKS.length],
    ['native', null, NATIVE_CHECKS.length],
    ['cross', null, CROSS_CHECKS.length],
    ['fd', 0, FRAMEDIFF_CHECKS.length],
    ['fd', 1, FRAMEDIFF_CHECKS.length],
    ['honesty', null, HONESTY_CHECKS.length],
  ];
  for (const [group, scope, want] of baselineGroups) {
    const label = group + (scope === null ? '' : '/' + scope);
    let results;
    try {
      results = runGroup(group, base, scope);
    } catch (error) {
      expect('基线 ' + label + ' 能跑', false, '抛了异常 ' + error.message);
      continue;
    }
    expect('基线 ' + label + ' 项数', results.length === want, '得到 ' + results.length + '，应为 ' + want);
    const red = results.filter((result) => !result.ok);
    expect('基线 ' + label + ' 全绿', red.length === 0, red.map((result) => result.id + '(' + result.detail + ')').join(' | '));
  }

  // ---- ② 传 undefined spec 必须**抛异常→红**，不能静默通过 ----
  const noSpec = checkLegModel(base.legs.browser, undefined);
  const noSpecRed = noSpec.filter((result) => !result.ok).length;
  // 不要求 15 项全红（有几项本来就不读 spec），但**必须**有项红：
  // 漏传 spec 而全绿，说明这些检查项对参数不敏感。
  expect('漏传 spec 必须变红而不是静默过', noSpecRed > 0,
    '15 项全部通过了——说明没有一项真的在读 spec');

  // ---- ③ 每条反向用例：改一处，必须**因为指定的那一项**红 ----
  for (const mutation of MUTATIONS) {
    const model = deepClone(base);
    let results;
    try {
      mutation.mutate(model);
      results = runGroup(mutation.group, model, mutation.scope);
    } catch (error) {
      expect('改动「' + mutation.name + '」能跑完', false, '抛了异常 ' + error.message);
      continue;
    }
    const red = results.filter((result) => !result.ok).map((result) => result.id);
    expect(
      '改动「' + mutation.name + '」必须被 ' + mutation.expect + ' 抓到',
      red.includes(mutation.expect),
      '红的是 ' + (red.length === 0 ? '（什么都没红）' : red.join('、')),
    );
  }

  // ---- ④ 覆盖：37 个检查项每一项都要有反向用例，数量实数上报 ----
  const covered = new Set(MUTATIONS.map((mutation) => mutation.expect));
  const uncovered = ALL_CHECK_IDS.filter((id) => !covered.has(id));
  expect('每个检查项都有反向用例', uncovered.length === 0, '没有反向用例的是 ' + uncovered.join('、'));

  return { failures, count, covered: covered.size, total: ALL_CHECK_IDS.length };
}

const USAGE = '用法：node scripts/check-m2-record.mjs [--record records/m2] [--self-test]';

function main() {
  const argv = process.argv.slice(2);
  let recordDir = DEFAULT_RECORD;
  let dirRel = 'records/m2';
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
      dirRel = value;
      recordDir = resolve(REPO_ROOT, value);
      i += 1;
    } else {
      console.error('✗ 不认识的参数：' + arg);
      console.error('  ' + USAGE);
      return 2;
    }
  }
  if (help) {
    console.log(USAGE);
    return 0;
  }

  const selfResult = runSelfTest();
  if (selfResult.failures.length > 0) {
    console.error('✗ 守卫自检失败——先修守卫，别信它的结论：');
    for (const failure of selfResult.failures) console.error('  - ' + failure);
    return 2;
  }
  if (selfTest) {
    console.log(
      '✓ 守卫自检通过（' + selfResult.count + ' 条断言；'
      + selfResult.covered + '/' + selfResult.total + ' 个检查项各一条反向用例）',
    );
    return 0;
  }

  // ---- 记录在不在、空不空 ----
  if (!existsSync(recordDir)) {
    console.error('✗ 没有 ' + recordDir + ' 这个目录');
    return 2;
  }
  for (const spec of EXPECTED.legs) {
    if (!existsSync(join(recordDir, spec.slug))) {
      console.error('✗ 缺 ' + spec.slug + '/ 这条腿');
      return 2;
    }
  }
  let model;
  try {
    model = loadRecord(recordDir, dirRel);
  } catch (error) {
    console.error('✗ 记录读不进来：' + error.message);
    return 2;
  }

  // 空文件集**绝不允许**通过：一条腿一张 PNG 都没有时，"全绿"只是"什么都没查"。
  const pngTotal = EXPECTED.legs.reduce((sum, spec) => sum + model.legs[spec.slug].frameNames.length, 0);
  if (pngTotal === 0) {
    console.error('✗ frames/ 里一张 PNG 都没有——空文件集绝不允许通过');
    return 2;
  }

  const sections = [
    ['腿 ' + EXPECTED.legs[0].slug, 'leg', EXPECTED.legs[0].slug],
    ['腿 ' + EXPECTED.legs[1].slug, 'leg', EXPECTED.legs[1].slug],
    ['native 归档（' + EXPECTED.native.label + '）', 'native', null],
    ['跨腿', 'cross', null],
    ['比对记录 ' + EXPECTED.framediff[0].slug, 'fd', 0],
    ['比对记录 ' + EXPECTED.framediff[1].slug, 'fd', 1],
    ['诚实性', 'honesty', null],
  ];
  let redTotal = 0;
  let allTotal = 0;
  for (const [title, group, scope] of sections) {
    let results;
    try {
      results = runGroup(group, model, scope);
    } catch (error) {
      console.error('✗ ' + title + '：检查跑不起来 —— ' + error.message);
      return 2;
    }
    const red = results.filter((result) => !result.ok);
    redTotal += red.length;
    allTotal += results.length;
    console.log((red.length === 0 ? '✓' : '✗') + ' ' + title + '：' + (results.length - red.length) + '/' + results.length);
    for (const result of red) console.log('    ✗ ' + result.id + ' — ' + result.detail);
  }
  console.log('—'.repeat(60));
  console.log('合计 ' + (allTotal - redTotal) + '/' + allTotal + ' 项绿' + (redTotal > 0 ? '，' + redTotal + ' 项红' : ''));
  if (redTotal > 0) return 1;
  console.log('✓ ' + dirRel + ' 复核通过：' + pngTotal + ' 张 PNG 的像素摘要逐张重算、'
    + '整表摘要与档位判定重拼、比对记录逐字节对账，全部一致');
  return 0;
}

process.exitCode = main();



