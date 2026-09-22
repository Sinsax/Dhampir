// M2 浏览器腿的本地服务：静态页 + 四个落盘口，**并且是这一轮的判定者**。
//
// 为什么判定在服务端而不是页面里：
//
//  1. 页面自己说"我画对了"不是证据。真正能立住的只有"写进磁盘的字节，与记录里
//     那一行（路径、字节数、FNV-1a 摘要）逐条对上"——这件事必须由**收文件的那一方**
//     做，因为它才有机会在写之前拦住坏数据。
//  2. 完整性是**集合性质**：80 帧一帧不少、没有多余的旧帧混在里面、`counts` 与
//     逐点明细自洽、`readings.txt` 与 `run.json` 描述的是同一次运行。页面看不到
//     整个目录，只有服务端能回答。
//
// 拒收与发现的区别（这个区分是刻意的）：
//
//  - **完整性不过 → 拒收**：不写 `run.json`。一份已经被判定为不可信的记录写进
//    records/ 比没有更糟——它会被后来的人当成证据读。环境那份 `adapter.json`
//    早已落盘，所以"在什么环境里失败的"仍然查得到。
//  - **发现（同帧两次渲染不一致、与 native 的字节是否相同）→ 照写，如实标注**。
//    这些正是 M2 要量出来的事实，不是"记录不可信"。把它们当成拒收条件，等于
//    在最有价值的失败现场拒绝留下证据。
//
// 用法：
//   node scripts/serve-corpus-harness.mjs [--port 8788] [--out records/m2/browser]
//                                        [--leg m2] [--native records/m1/dx12]
//                                        [--expect <16 位十六进制>] [--self-test]
//
// 端口给 0 表示让系统挑一个空闲端口（驱动脚本用这个）。
// 起服务后打开打印出来的 URL；页面会把整轮 POST 回来，服务端校验后落盘。

import { spawn } from 'node:child_process';
import {
  existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, statSync, writeFileSync,
} from 'node:fs';
import { createServer } from 'node:http';
import { dirname, extname, join, normalize, resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';

const HERE = dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = resolve(HERE, '..');
const WWW_ROOT = join(REPO_ROOT, 'crates', 'dhampir-wasm', 'www');

const PNG_SIGNATURE = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);
const MAX_BODY_BYTES = 10 * 1024 * 1024;

const MIME = new Map([
  ['.html', 'text/html; charset=utf-8'],
  ['.js', 'text/javascript; charset=utf-8'],
  ['.mjs', 'text/javascript; charset=utf-8'],
  ['.wasm', 'application/wasm'],
  ['.json', 'application/json; charset=utf-8'],
  ['.css', 'text/css; charset=utf-8'],
  ['.png', 'image/png'],
  ['.map', 'application/json; charset=utf-8'],
]);

// ---------------------------------------------------------------------------
// 与 core 共用的几个常量（这里是**边界上的第二份**，所以每一条都要能被证伪）
// ---------------------------------------------------------------------------
//
// 服务端没法调 Rust，这几个值只能在 JS 里再写一遍。为了避免"写了两遍、
// 漂移的那天正好最不该漂"，`--self-test` 会把它们与**已归档的 native 记录**
// 对一遍：naming 变了、表版本变了、报告抬头变了，自检当天就红。

/** 表的**契约版本**，不是"产生它的里程碑"。同 `corpus::CORPUS_TABLE_MILESTONE`。 */
const CORPUS_TABLE_MILESTONE = 'M1';
/** 同 `corpus::CORPUS_RECORD_KIND`。 */
const CORPUS_RECORD_KIND = 'corpus';
/** 同 `corpus::CORPUS_RECORD_SCHEMA`。 */
const CORPUS_RECORD_SCHEMA = 1;
/** 浏览器宿主唯一可用的后端。同 `gpu::BROWSER_BACKENDS` 的标签形式。 */
const BROWSER_BACKEND_LABEL = 'BROWSER_WEBGPU';
/**
 * 宿主 GPU 表（驱动从 CDP `SystemInfo.getInfo` 读到的设备表）在产物里的文件名。
 *
 * **与 wasm 侧 `unresolved_identity().resolves_to` 是同一个字符串**：记录里那句
 * "答案在这个文件里"指的就是它，两处写法漂开就等于指向一个不存在的文件。服务端是唯一
 * 能真去盘上找这个文件、并在找不到时拒收整轮记录的一侧，所以这条对账放在这里。
 */
const HOST_GPU_FILE = 'host-gpu.json';
/**
 * 宿主设备表的来源声明。表从哪来是**要写进记录的事实**，不是实现细节：换一个来源
 * （别的 CDP 调用、别的 API）意味着换了一套"哪块卡"的判据，那时这份契约得跟着改。
 */
const HOST_GPU_SOURCE = 'CDP SystemInfo.getInfo';
/**
 * 页面 `in_page.vendor`（= JS `GPUAdapterInfo.vendor`）→ PCI 数字 vendor id。
 *
 * 为什么需要这张表：页面给的是小写字符串（实测 Chrome 报 `"nvidia"`），而宿主设备表里
 * **没有可用的字符串厂商名**——`vendorString` 实测全是空串，只有数字 `vendorId`
 * （NVIDIA `4318` = 0x10de、AMD `4098` = 0x1002、Microsoft `5140` = 0x1414）。
 * 于是"字符串 → 数字"这一步必须显式写下来，没有它就只能靠猜。
 *
 * 查不到时**拒绝**，不做相似度匹配：拼错的 vendor 如果被模糊匹配到一块卡，记录里那句
 * "用了哪块卡"就成了编的。表里少一个厂商的正确处理，是把那台机器的 `vendorString` 与
 * `vendorId` 记下来补进这张表，而不是让程序去挑一个"看起来像"的。
 */
const PAGE_VENDOR_IDS = new Map([
  ['nvidia', 0x10de],
  ['amd', 0x1002],
  ['intel', 0x8086],
  ['microsoft', 0x1414],
  ['apple', 0x106b],
  ['qualcomm', 0x5143],
  ['arm', 0x13b5],
  ['imagination', 0x1010],
]);
/** `in_page` 必须有的键。形状由 wasm 侧的 `parse_in_page` 定稿（多给的键在那里就丢掉了）。 */
const IN_PAGE_KEYS = ['vendor', 'architecture', 'device', 'description',
  'is_fallback_adapter', 'subgroup_min_size', 'subgroup_max_size', 'note'];
/** `readings.txt` 的抬头行。同 `corpus::report_text` 的第一行。 */
const READINGS_HEADER = 'dhampir M1 corpus 逐点读数';
/**
 * `frames/<场景>-f<三位帧号>.png`。同 `corpus::frame_rel_path`。
 *
 * 字母表里有 `_`：场景名是 `snake_case`（`srgb_linear` 就在表里）。这条正则的职责是
 * **落盘前的卫生**——把 `/`、`.`、`\` 这些能拼出目录跳转的字符挡在门外——**不是**
 * "哪些场景存在"的权威：那是场景注册表与记录里 `scenes[]` 的事（`crossCheckRun`
 * 会逐帧拿 `row.scene` 去对）。把两件事混成一条正则会造出这种局面：真正存在的场景
 * 送不进来（M2 第二次真跑正是这样：`srgb_linear` 的 16 帧全被拒了），而"什么场景
 * 算存在"还是没人管。
 */
const FRAME_NAME_RE = /^frames\/([a-z0-9_-]+)-f(\d{3})\.png$/;
const DIGEST_RE = /^[0-9a-f]{16}$/;
/**
 * `backends[0].requested` 的形状：core 的 `backend_label` 剥壳之后的写法。
 * 允许位或起来的多后端（`DX12 | VULKAN`），但**不允许** `Backends(DX12)`
 * 那个 wgpu 内部形态（M0 归档里留下的就是它，M1 起不再这么写）。
 */
const BACKEND_LABEL_RE = /^[A-Z][A-Z0-9_]*( \| [A-Z][A-Z0-9_]*)*$/;

const USAGE = `用法：node scripts/serve-corpus-harness.mjs [选项]

选项：
  --port <n>      监听端口（默认 8788；给 0 表示让系统挑一个空闲端口）
  --out <dir>     这条腿的产物目录（默认 records/m2/browser）
  --leg <slug>    期望的腿名（默认 m2）。adapter.json 里的 backend_slug 必须等于它
  --native <dir>  native 腿的归档目录（如 records/m1/dx12）：从它的 run.json 取
                  frames_digest 当期望值，并把两条腿的 readings.txt 摆在一起比
  --expect <hex>  直接给期望的 frames_digest（与 --native 二选一）
  --self-test     只跑本脚本的自检，不启动服务
  -h, --help      显示本帮助`;

// ---------------------------------------------------------------------------
// 纯函数：摘要、路径、记录的形状
// ---------------------------------------------------------------------------

/** FNV-1a 64，与 `dhampir_timeline::fnv1a64` 同算法（16 位小写十六进制）。 */
export function fnv1a64Hex(bytes) {
  const MASK = (1n << 64n) - 1n;
  const PRIME = 0x00000100000001b3n;
  let hash = 0xcbf29ce484222325n;
  for (const byte of bytes) {
    hash = (hash ^ BigInt(byte)) & MASK;
    hash = (hash * PRIME) & MASK;
  }
  return hash.toString(16).padStart(16, '0');
}

/** `(场景, 帧号)` → 记录里那条相对路径。与 core 的 `frame_rel_path` 同一形状。 */
export function framePathFor(scene, frame) {
  return `frames/${scene}-f${String(frame).padStart(3, '0')}.png`;
}

/**
 * 由逐帧明细**复算**整表摘要。字节布局同 `dhampir_core::render::corpus::table_digest`：
 * `场景名 + 0x00 + 帧号(LE u32) + 像素摘要(LE u64)`，逐行拼接后取 FNV-1a 64。
 *
 * 为什么要在这里再实现一遍——这不算"摘要算法有了第二处真相"吗？不算，因为这里复算的是
 * **记录自己声明的那个数**，不是渲染结果：core 那条实现算的是 `frame.digest`（像素的
 * 摘要），这里算的是"把若干帧的像素摘要按记录里的顺序拼起来会得到什么"。两件事要分开：
 * 前者由渲染决定，后者由记录的形状决定。
 *
 * 有它之后，"`frames_digest` 与逐帧明细互相矛盾"就不再是没人查的事：一份记录里两个答案，
 * 说明它至少有一半是错的，而两端的摘要比对是 M2 的命门——浏览器腿算错了摘要会变成
 * 一个查不出来的假差异。形状不对（缺 `pixel_digest`、帧号不是整数）返回 `null`：
 * **复算不了**与"复算出来不一样"是两回事，别混成一个。
 */
export function tableDigestHex(rows) {
  const parts = [];
  for (const row of rows ?? []) {
    if (typeof row?.scene !== 'string' || row.scene === '') return null;
    if (!Number.isInteger(row.frame) || row.frame < 0 || row.frame > 0xffffffff) return null;
    if (!/^[0-9a-f]{16}$/.test(String(row.pixel_digest ?? ''))) return null;
    const frame = Buffer.alloc(4);
    frame.writeUInt32LE(row.frame);
    const pixel = Buffer.alloc(8);
    pixel.writeBigUInt64LE(BigInt(`0x${row.pixel_digest}`));
    // 场景名是 ASCII；用 utf8 编码与 Rust 的 `str::as_bytes` 同义。
    parts.push(Buffer.from(row.scene, 'utf8'), Buffer.from([0]), frame, pixel);
  }
  return fnv1a64Hex(Buffer.concat(parts));
}

/** `"3..19"` → `[3, 19]`；不是这个形状就返回 null。 */
export function parseFrameRange(text) {
  const match = /^(\d+)\.\.(\d+)$/.exec(String(text ?? ''));
  if (!match) return null;
  const start = Number(match[1]);
  const end = Number(match[2]);
  if (end <= start) return null;
  return [start, end];
}

/**
 * `readings.txt` 开头的两行 → 计数。对不上形状就返回 null。
 *
 * 抽出来是为了能**独立复核**：页面送上来的两份文本（readings 与 run.json）都由
 * core 生成，但它们是同一件事的两种呈现——一份写"帧 80、采样点 368"，
 * 另一份写逐帧逐点的明细。这两份如果互相矛盾，说明送来的是两份不同的东西。
 */
export function parseReadingsCounts(text) {
  if (typeof text !== 'string') return null;
  const lines = text.split('\n');
  if (lines[0] !== READINGS_HEADER) return null;
  const match = /^帧 (\d+)、采样点 (\d+)、失败 (\d+)、越界 (\d+)、未判定 (\d+)；容差 (\d+) 字节$/
    .exec(lines[1] ?? '');
  if (!match) return null;
  return {
    frames: Number(match[1]),
    points: Number(match[2]),
    failed: Number(match[3]),
    out_of_range: Number(match[4]),
    unjudged: Number(match[5]),
    tolerance: Number(match[6]),
  };
}

/** 每行明细都能被重新数一遍的那种"明细"，而不是账本自己报的合计。 */
function countPoints(rows) {
  let points = 0;
  let failed = 0;
  let outOfRange = 0;
  let unjudged = 0;
  for (const row of rows) {
    for (const point of row.points ?? []) {
      points += 1;
      if (point.measured === null || point.measured === undefined) outOfRange += 1;
      else if (point.passed === null || point.passed === undefined) unjudged += 1;
      else if (point.passed === false) failed += 1;
    }
  }
  return { points, failed, out_of_range: outOfRange, unjudged };
}

/**
 * **这一轮的可信度判定。** 纯函数：喂记录、盘上已有的帧、期望摘要，出结论。
 *
 * `onDisk`：`Map<相对路径, {bytes, digest}>`。
 *
 * `expectRequested`：这一轮**应该**是哪条腿（`backends[0].requested`）。给了就钉死；
 * 不给只查形状——因为这条键是**腿的属性**，不是记录格式的一部分：同一份记录格式
 * （`kind=corpus`）既装 `DX12` 也装 `BROWSER_WEBGPU`，硬钉其中一个就等于让校验器
 * 只能验一条腿（M1 的 native 归档当"好样本"时会因为这一条必红，连带把旁边那些
 * 反向用例也染红——反向用例因为**别的原因**变红，就等于没测）。
 *
 * `identity`：这一轮的卡身份，`{ adapterName, resolved, why }`——`adapterName` 是
 * `adapter.json` 里那个值（`null` = 浏览器不给名字），`resolved` 是宿主设备表对出来的卡
 * （有名字时为 `null`），`why` 是没解出来时的原因。**不给（`undefined`）不是"跳过"而是红**：
 * 这条检查要同时看 adapter.json 与 run.json，而只有服务端同时握着两份——接线掉了的话，
 * 它就成了"文件都在、没人看过用了哪块卡"。
 *
 * 返回 `{checks, findings, ok}`：
 *  - `checks` 是完整性，任何一条不过 → `ok === false` → 调用方**不许写 run.json**；
 *  - `findings` 是 M2 要量的事实（同帧确定性、与 native 的字节关系），照写、如实标注。
 */
export function crossCheckRun({
  record, onDisk, expectDigest, readingsCounts, expectRequested, identity,
}) {
  const checks = [];
  const findings = [];
  const pass = (detail) => checks.push({ ok: true, detail });
  const fail = (detail) => checks.push({ ok: false, detail });

  const backend = Array.isArray(record?.backends) && record.backends.length === 1
    ? record.backends[0]
    : null;
  if (backend === null) {
    fail(`backends 必须恰好有一条（记录里的 `+`backends`+` 数组长度是 ${record?.backends?.length ?? '缺失'}）`);
    return { checks, findings, ok: false };
  }
  const rows = Array.isArray(backend.frames) ? backend.frames : null;
  if (rows === null || rows.length === 0) {
    fail('backends[0].frames 必须是数组且非空——空集合上不许宣布通过');
    return { checks, findings, ok: false };
  }

  // ---- ① 记录自己的身份 ---------------------------------------------------
  if (record.kind === CORPUS_RECORD_KIND && record.schema === CORPUS_RECORD_SCHEMA
      && record.milestone === CORPUS_TABLE_MILESTONE) {
    pass(`记录身份：kind=${CORPUS_RECORD_KIND} schema=${CORPUS_RECORD_SCHEMA} `
      + `milestone=${CORPUS_TABLE_MILESTONE}（表契约版本）`);
  } else {
    fail(`记录身份不对：kind=${JSON.stringify(record.kind)} `
      + `schema=${JSON.stringify(record.schema)} milestone=${JSON.stringify(record.milestone)}`);
  }
  if (expectRequested === undefined || expectRequested === null) {
    if (BACKEND_LABEL_RE.test(String(backend.requested ?? ''))) {
      pass(`请求的后端：${backend.requested}（只查了形状：这一轮是哪条腿由调用方声明）`);
    } else {
      fail(`requested 不像剥过壳的 wgpu 后端名：${JSON.stringify(backend.requested)}`
        + '——core 的 backend_label 剥掉 `Backends(...)`，记录里不该出现 wgpu 的内部形态');
    }
  } else if (backend.requested === expectRequested) {
    pass(`请求的后端：${backend.requested}`);
  } else {
    fail(`这条腿的 requested 应为 ${JSON.stringify(expectRequested)}，`
      + `收到 ${JSON.stringify(backend.requested)}——记录与它声称的腿不是同一个东西`);
  }
  if (DIGEST_RE.test(String(backend.frames_digest ?? ''))) {
    pass(`frames_digest ${backend.frames_digest}`);
  } else {
    fail(`frames_digest 不是 16 位十六进制：${JSON.stringify(backend.frames_digest)}`);
  }
  if (Array.isArray(record.nondeterministic_fields) && record.nondeterministic_fields.length === 0) {
    pass('记录自报"没有任何一项允许变化"（nondeterministic_fields 为空）');
  } else {
    fail('corpus 记录里的 nondeterministic_fields 必须是空数组：'
      + '这一份的所有字段都被要求可复现，留一项就是给自己留退路');
  }

  // ---- ② 帧区间与场景集合 -------------------------------------------------
  const range = parseFrameRange(record.frame_range);
  if (range === null) {
    fail(`frame_range 形状不对：${JSON.stringify(record.frame_range)}（要 \`a..b\` 且 b > a）`);
    return { checks, findings, ok: false };
  }
  const [start, end] = range;
  const perScene = end - start;
  if (record.frames_per_scene === perScene) {
    pass(`帧区间 ${record.frame_range}（每场景 ${perScene} 帧）`);
  } else {
    fail(`frames_per_scene=${JSON.stringify(record.frames_per_scene)} 与 `
      + `frame_range=${record.frame_range} 算出来的 ${perScene} 不一致`);
  }

  const declaredScenes = new Set(
    (Array.isArray(record.scenes) ? record.scenes : []).map((scene) => scene?.name),
  );
  const byScene = new Map();
  for (const row of rows) {
    if (!byScene.has(row.scene)) byScene.set(row.scene, new Set());
    byScene.get(row.scene).add(row.frame);
  }
  let holes = [];
  for (const [scene, frames] of byScene) {
    for (let frame = start; frame < end; frame += 1) {
      if (!frames.has(frame)) holes.push(framePathFor(scene, frame));
    }
  }
  if (holes.length === 0 && byScene.size * perScene === rows.length) {
    pass(`${byScene.size} 个场景 × ${perScene} 帧，逐场景无缺帧、无重复（共 ${rows.length} 帧）`);
  } else {
    fail(`${byScene.size} 个场景 × ${perScene} 帧 != ${rows.length} 行；`
      + `缺：${holes.slice(0, 8).join('、') || '（无）'}`);
  }
  const undeclared = [...byScene.keys()].filter((name) => !declaredScenes.has(name));
  if (undeclared.length === 0) {
    pass(`帧里的场景都在记录自述的 scenes 表里（${[...declaredScenes].join('、')}）`);
  } else {
    fail(`帧里出现了 scenes 表里没有的场景：${undeclared.join('、')}`);
  }

  // ---- ③ 每一帧都要在盘上，且字节/摘要都对得上 ---------------------------
  const used = new Set();
  const mismatches = [];
  for (const row of rows) {
    const expectedPath = framePathFor(row.scene, row.frame);
    if (row.png !== expectedPath) {
      mismatches.push(`${row.scene} f${row.frame}：记录里写的是 ${row.png}，`
        + `按 naming 应该是 ${expectedPath}`);
      continue;
    }
    const disk = onDisk.get(row.png);
    if (!disk) {
      mismatches.push(`${row.png}：盘上没有这个文件`);
      continue;
    }
    used.add(row.png);
    if (disk.bytes !== row.png_bytes) {
      mismatches.push(`${row.png}：盘上 ${disk.bytes} 字节，记录里写 ${row.png_bytes}`);
      continue;
    }
    if (disk.digest !== row.png_digest) {
      mismatches.push(`${row.png}：盘上摘要 ${disk.digest}，记录里写 ${row.png_digest}`);
    }
  }
  const extra = [...onDisk.keys()].filter((path) => !used.has(path));
  if (mismatches.length === 0 && extra.length === 0) {
    pass(`盘上 ${used.size} 帧全部与 run.json 的路径/字节数/摘要一致，且没有多余文件`);
  } else {
    for (const detail of mismatches.slice(0, 8)) fail(`帧不符：${detail}`);
    if (mismatches.length > 8) fail(`……另有 ${mismatches.length - 8} 帧不符（只列前 8 条）`);
    if (extra.length > 0) {
      fail(`frames/ 里有多余文件（不属于这一轮）：${extra.slice(0, 8).join('、')}`);
    }
  }

  // ---- ④ 账本与明细自洽 ---------------------------------------------------
  const counted = countPoints(rows);
  const counts = backend.counts ?? {};
  const ledgerOk = counts.frames === rows.length
    && counts.points === counted.points
    && counts.failed === counted.failed
    && counts.out_of_range === counted.out_of_range
    && counts.unjudged === counted.unjudged
    && counts.clean === (counted.failed === 0 && counted.out_of_range === 0 && counted.unjudged === 0);
  if (ledgerOk) {
    pass(`账本与明细自洽：帧 ${rows.length}、采样点 ${counted.points}、`
      + `失败 ${counted.failed}、越界 ${counted.out_of_range}、未判定 ${counted.unjudged}`);
  } else {
    fail(`账本与明细不一致：记录写 ${JSON.stringify(counts)}，`
      + `按 tips 数出来 ${JSON.stringify({ ...counted, frames: rows.length })}`);
  }

  // ---- ⑤ 账本摘要能被逐帧明细复算出来 -------------------------------------
  const recomputed = tableDigestHex(rows);
  if (recomputed === null) {
    fail('逐帧明细里缺 pixel_digest（场景名/帧号/像素摘要三种都进摘要），'
      + '账本摘要无从复算——复算不了与"复算出来不一样"不是一回事，但都轮不到说可信');
  } else if (recomputed === backend.frames_digest) {
    pass(`账本摘要能由逐帧明细复算出来（${recomputed}，${rows.length} 行）`);
  } else {
    fail(`frames_digest 与逐帧明细复算的不一样：记录写 ${JSON.stringify(backend.frames_digest)}，`
      + `按"场景名+帧号+像素摘要"复算是 ${recomputed}——同一份记录里两个答案，`
      + '端到端的摘要比对会因此变成一个查不出来的假差异');
  }

  // ---- ⑥ readings.txt 与 run.json 说同一件事 -----------------------------
  if (!readingsCounts) {
    fail('readings.txt 缺失或缺头两行——它必须与 run.json 描述同一次运行');
  } else if (readingsCounts.frames === counts.frames && readingsCounts.points === counts.points
      && readingsCounts.failed === counts.failed
      && readingsCounts.out_of_range === counts.out_of_range
      && readingsCounts.unjudged === counts.unjudged) {
    pass(`readings.txt 的计数与 run.json 一致（帧 ${readingsCounts.frames}、`
      + `采样点 ${readingsCounts.points}）`);
  } else {
    fail(`readings.txt 与 run.json 的计数不一致：readings 写 ${JSON.stringify(readingsCounts)}，`
      + `run.json 写 ${JSON.stringify(counts)}`);
  }

  // ---- ⑦ 卡身份：这份记录答得出"用了哪块卡" ---------------------------------
  //
  // 两件事在这一条里合起来看：两份记录说的是不是同一个名字，以及那个名字（或那个缺口）
  // 是不是真的落到了实处。浏览器腿上 `adapter.name` 是空串，所以"用了哪块卡"只能由
  // `in_page.vendor` + 宿主设备表对出来（见 settleHostGpu）；而这两份记录各在一支手里，
  // 只有服务端同时握着——这条检查只能在这里做。
  if (identity === undefined) {
    fail('调用方没声明这一轮的卡身份（identity）——这一条**不许沉默地跳过**：'
      + '它要同时看 adapter.json 与 run.json，跳过它就等于没人看过"用了哪块卡"');
  } else if (identity.adapterName !== backend.adapter_name) {
    fail(`两份记录说的卡名不是同一个：adapter.json 写 `
      + `${JSON.stringify(identity.adapterName)}，run.json 的 backends[0].adapter_name 写 `
      + `${JSON.stringify(backend.adapter_name)}——同一个名字两个出处，读的人没法互相印证，`
      + '也没法知道哪一份才对');
  } else if (typeof identity.adapterName === 'string' && identity.adapterName !== '') {
    pass(`卡身份由 adapter_name 给出：${identity.adapterName}`);
  } else if (identity.resolved) {
    pass(`卡身份由宿主设备表解出：${identity.resolved.device}`
      + `（vendor ${JSON.stringify(identity.resolved.vendor)} → PCI id `
      + `${identity.resolved.vendor_id}，见 ${HOST_GPU_FILE}）`);
  } else {
    fail(`卡身份没解出来：${identity.why ?? '（没给理由——那本身就是缺陷）'}`
      + '——这份记录说不出"用了哪块卡"，而那是它的全部意义');
  }

  // ---- ⑧ 发现（不影响"这份记录可信"） -------------------------------------
  const repeatBad = rows.filter((row) => row.repeat_identical !== true
    || row.pixel_digest !== row.repeat_pixel_digest);
  if (repeatBad.length === 0) {
    findings.push({ ok: true, detail: `同帧两次渲染：${rows.length} 帧全部逐字节一致` });
  } else {
    findings.push({
      ok: false,
      detail: `同帧两次渲染不一致 ${repeatBad.length} 帧：`
        + `${repeatBad.slice(0, 8).map((row) => `${row.scene} f${row.frame}`).join('、')}`
        + '——这是浏览器运行时的不确定性，M2 要归因（见 T2.5）',
    });
  }
  if (counts.clean === true) {
    findings.push({ ok: true, detail: '表里的采样点全部通过（这就是 M1 冻结的那张表）' });
  } else {
    findings.push({
      ok: false,
      detail: `表里有没过的点：失败 ${counted.failed}、越界 ${counted.out_of_range}、`
        + '未判定 ' + `${counted.unjudged}——结构性差异一律当 bug 修（M2 T2.5）`,
    });
  }
  if (expectDigest) {
    if (backend.frames_digest === expectDigest) {
      findings.push({
        ok: true,
        detail: `与 native 腿逐字节相同（${expectDigest}）：更强的一条已成立，`
          + '逐帧 SSIM（T2.4）仍要跑，作为可复核的对照',
      });
    } else {
      findings.push({
        ok: false,
        detail: `与 native 腿不同：浏览器 ${backend.frames_digest}、native ${expectDigest}`
          + '——交给 T2.4 的逐帧 SSIM 判定差异有多大',
      });
    }
  } else {
    findings.push({ ok: null, detail: '没有给 --native/--expect，未与 native 腿比对摘要' });
  }

  return { checks, findings, ok: checks.every((check) => check.ok) };
}

// ---------------------------------------------------------------------------
// 落盘口
// ---------------------------------------------------------------------------

/** `frames/` 下的现状 → `Map<相对路径, {bytes, digest}>`。目录不存在时是空的。 */
export function readFramesDir(framesDir) {
  const onDisk = new Map();
  if (!existsSync(framesDir)) return onDisk;
  for (const name of readdirSync(framesDir).sort()) {
    const full = join(framesDir, name);
    if (!statSync(full).isFile()) continue;
    const bytes = readFileSync(full);
    onDisk.set(`frames/${name}`, { bytes: bytes.length, digest: fnv1a64Hex(bytes) });
  }
  return onDisk;
}

/**
 * 页面报的 `in_page` 块（`adapter.json` 里的那一块）：形状对不对。**纯函数**。
 *
 * 这是"哪块卡"这条链的第一环，同时也是**唯一**一环：后面所有推断都从 `vendor` 出发，
 * 所以它是空串时整条链没有起点——那不是能靠默认值补上的事。其余几列 Chrome 实测就是空串
 * （`device` / `description`），**允许为空**：把"浏览器不给"当成形状错误，等于让这条腿
 * 永远跑不起来。
 *
 * 不检查"有没有多余的键"：`parse_in_page` 已经把多给的键丢掉了，所以在真记录上这个检查
 * 永远不会红。一条永不触发的守卫比没有守卫更糟——它只提供虚假的安心。
 */
export function validateInPage(inPage) {
  if (inPage === null || typeof inPage !== 'object' || Array.isArray(inPage)) {
    return { ok: false, error: `adapter.json 的 in_page 不是对象：${JSON.stringify(inPage)}` };
  }
  const missing = IN_PAGE_KEYS.filter((key) => !Object.prototype.hasOwnProperty.call(inPage, key));
  if (missing.length > 0) {
    return { ok: false, error: `adapter.json 的 in_page 少了 ${missing.join('、')}：`
      + '这一块的形状由 wasm 侧的 parse_in_page 定稿（浏览器多给的键在那里就丢掉了），'
      + '不是浏览器说了算' };
  }
  for (const key of ['vendor', 'architecture', 'device', 'description', 'note']) {
    if (typeof inPage[key] !== 'string') {
      return { ok: false, error: `adapter.json 的 in_page.${key} 应是字符串，`
        + `收到 ${JSON.stringify(inPage[key])}` };
    }
  }
  if (inPage.vendor === '') {
    return { ok: false, error: 'adapter.json 的 in_page.vendor 是空串：卡身份这条链的第一环就是它，'
      + '后面没有第二环能补上（实测 Chrome 这里给 "nvidia"）' };
  }
  if (inPage.note === '') {
    return { ok: false, error: 'adapter.json 的 in_page.note 是空串：这一块为什么长这样要写进记录，'
      + '否则复核的人只能去读源码' };
  }
  if (typeof inPage.is_fallback_adapter !== 'boolean') {
    return { ok: false, error: 'adapter.json 的 in_page.is_fallback_adapter 应是布尔值，'
      + `收到 ${JSON.stringify(inPage.is_fallback_adapter)}` };
  }
  for (const key of ['subgroup_min_size', 'subgroup_max_size']) {
    const value = inPage[key];
    if (value !== null && !(Number.isInteger(value) && value > 0)) {
      return { ok: false, error: `adapter.json 的 in_page.${key} 应是正整数或 null，`
        + `收到 ${JSON.stringify(value)}` };
    }
  }
  return { ok: true, vendor: inPage.vendor };
}

/**
 * 驱动 POST 上来的宿主设备表：形状对不对。**纯函数**。
 *
 * 设备表的形状是**上游**（CDP）给的，所以这里只钉三件事：来源写着、表非空、
 * 每一块卡的数字 id 与名字可用。**不钉**"表里有几块卡"——那由 [`matchHostGpu`]
 * 按厂商去数，在这里先数一遍只会多一个会漂的第二意见。
 */
export function validateHostGpuText(text) {
  let raw;
  try {
    raw = JSON.parse(text);
  } catch (error) {
    return { ok: false, error: `${HOST_GPU_FILE} 不是 JSON：${error.message}` };
  }
  if (raw === null || typeof raw !== 'object' || Array.isArray(raw)) {
    return { ok: false, error: `${HOST_GPU_FILE} 不是对象` };
  }
  if (raw.source !== HOST_GPU_SOURCE) {
    return { ok: false, error: `宿主设备表的 source 应为 ${JSON.stringify(HOST_GPU_SOURCE)}，`
      + `收到 ${JSON.stringify(raw.source)}——换来源等于换了一套"哪块卡"的判据，`
      + '那时改的是这份契约，不是悄悄换个字符串' };
  }
  if (!Array.isArray(raw.devices) || raw.devices.length === 0) {
    return { ok: false, error: `宿主设备表的 devices 必须是数组且非空（收到 `
      + `${Array.isArray(raw.devices) ? '0 项' : JSON.stringify(raw.devices)}）——`
      + '空表上不可能对出"用了哪块卡"' };
  }
  for (const [index, device] of raw.devices.entries()) {
    if (device === null || typeof device !== 'object' || Array.isArray(device)) {
      return { ok: false, error: `宿主设备表第 ${index} 项不是对象` };
    }
    for (const key of ['vendorId', 'deviceId']) {
      if (!Number.isInteger(device[key]) || device[key] < 0) {
        return { ok: false, error: `宿主设备表第 ${index} 项的 ${key} 应是非负整数，`
          + `收到 ${JSON.stringify(device[key])}——匹配只走数字 id（vendorString 实测是空串）` };
      }
    }
    if (typeof device.deviceString !== 'string' || device.deviceString === '') {
      return { ok: false, error: `宿主设备表第 ${index} 项没有 deviceString：`
        + '这一栏是"用了哪块卡"最终要写进记录的那句话，不能空' };
    }
  }
  return { ok: true, source: raw.source, devices: raw.devices };
}

/**
 * 页面报的 vendor → 宿主设备表里**唯一**一块卡。**纯函数**。
 *
 * 三件事在这里被钉住，每一件都是"猜"与"不猜"的分界：
 *
 *  1. vendor 字符串先要能翻成 PCI 数字 id。翻不出来就红——见 [`PAGE_VENDOR_IDS`]：那是张
 *     显式表，不是相似度匹配。
 *  2. 表里只有**一块**该厂商的卡才算对上。0 块说明页面报的卡与宿主设备表不是同一台机器上
 *     的东西；≥2 块说明"哪块卡"仍有二义性（多卡机器、或同一厂商的核显+独显），这时必须
 *     说明白才配写记录——二义性不是能自动消解的东西。
 *  3. 只认数字 `vendorId`，不认 `vendorString`：实测本机 CDP 的 `vendorString` 全是空串，
 *     按它匹配等于按空串匹配一切。
 */
export function matchHostGpu(inPage, devices) {
  const vendor = inPage?.vendor;
  const pciId = PAGE_VENDOR_IDS.get(String(vendor));
  if (pciId === undefined) {
    return { ok: false, error: `页面报的 in_page.vendor 是 ${JSON.stringify(vendor)}，`
      + `PAGE_VENDOR_IDS 里没有它对应的 PCI vendor id——先在表里登记再跑，不许按名字相似度猜`
      + `（表里现有：${[...PAGE_VENDOR_IDS.keys()].join('、')}）` };
  }
  const hits = devices.filter((device) => device?.vendorId === pciId);
  const table = devices.map((device) => `${device?.deviceString ?? '?'}`
    + `（vendor ${device?.vendorId}）`).join('、');
  if (hits.length === 0) {
    return { ok: false, error: `宿主设备表里没有 vendor id ${pciId}（${vendor}）的卡：`
      + `表里是 ${table}——页面报的那块卡与宿主设备表不是同一台机器上的` };
  }
  if (hits.length > 1) {
    return { ok: false, error: `宿主设备表里有 ${hits.length} 块 ${vendor} 的卡`
      + `（${hits.map((device) => device.deviceString).join('、')}）——`
      + '哪一块在渲染还有二义性，而这份记录的第一件事就是"用了哪块卡"，不能在这一步含糊' };
  }
  return {
    ok: true,
    device: hits[0],
    reason: `in_page.vendor=${vendor} → PCI vendor id ${pciId}，宿主设备表里唯一一块`,
  };
}

/** 适配器那一份：形状对不对。**这是唯一一份"环境"记录**，所以它先写。 */
export function validateAdapterText(text, legSlug) {
  let record;
  try {
    record = JSON.parse(text);
  } catch (error) {
    return { ok: false, error: `adapter.json 不是 JSON：${error.message}` };
  }
  if (record === null || typeof record !== 'object' || Array.isArray(record)) {
    return { ok: false, error: 'adapter.json 不是对象' };
  }
  if (record.kind !== 'adapter') {
    return { ok: false, error: `adapter.json 的 kind 应为 adapter，收到 ${JSON.stringify(record.kind)}` };
  }
  if (record.milestone !== CORPUS_TABLE_MILESTONE) {
    return { ok: false, error: `adapter.json 的 milestone 应为 ${CORPUS_TABLE_MILESTONE}，`
      + `收到 ${JSON.stringify(record.milestone)}` };
  }
  if (record.backend_slug !== legSlug) {
    return { ok: false, error: `adapter.json 的 backend_slug 应为 --leg 给的 ${legSlug}，`
      + `收到 ${JSON.stringify(record.backend_slug)}——它同时是产物目录名，不能漂` };
  }
  if (record.requested_backends !== BROWSER_BACKEND_LABEL) {
    return { ok: false, error: `adapter.json 的 requested_backends 应为 ${BROWSER_BACKEND_LABEL}，`
      + `收到 ${JSON.stringify(record.requested_backends)}` };
  }
  // ---- 身份：`adapter.name` 与 `adapter_name`/`gpu_identity` 必须说同一件事 ---------
  //
  // 浏览器腿上 `adapter.name` 是空串（Chrome 不给，见 wasm 侧的 IN_PAGE_NOTE），所以
  // "用了哪块卡"在这条腿上有**两种**合法答法，而且互斥：
  //   - 名字拿得到：`adapter_name` 写它，`gpu_identity` 不写（那个键的字面意思是"我答不出"）。
  //   - 名字拿不到：`adapter_name` 是 `null`，`gpu_identity` 声明缺口、指向宿主设备表，
  //     而**真的对上哪块卡**是由本脚本写 `host-gpu.json` 时定下来的（见 settleHostGpu）。
  // 两种答法都不成立时，这份记录就答不出"用了哪块卡"——而那是它的全部意义。
  const adapter = record.adapter;
  if (adapter === null || typeof adapter !== 'object' || Array.isArray(adapter)) {
    return { ok: false, error: `adapter.json 的 adapter 不是对象：${JSON.stringify(adapter)}` };
  }
  if (typeof adapter.name !== 'string') {
    return { ok: false, error: 'adapter.json 的 adapter.name 应是字符串（浏览器上就是空串），'
      + `收到 ${JSON.stringify(adapter.name)}` };
  }
  const inPage = validateInPage(record.in_page);
  if (!inPage.ok) return { ok: false, error: inPage.error };

  const derived = adapter.name === '' ? null : adapter.name;
  if (record.adapter_name !== derived) {
    return { ok: false, error: `adapter.json 的 adapter.name 是 ${JSON.stringify(adapter.name)}，`
      + `adapter_name 却是 ${JSON.stringify(record.adapter_name)}——这两个键在 wasm 侧同出`
      + '`adapter_name_of`（空名字写 null），读的人会拿它们互相印证，不许有两种写法' };
  }
  const identityNeeded = derived === null;
  if (!identityNeeded && record.gpu_identity !== undefined) {
    return { ok: false, error: `adapter.name 是 ${JSON.stringify(derived)}（名字拿得到），`
      + '却还挂着 gpu_identity 声明——那个键的意思是"我答不出哪块卡"，'
      + '有名字的腿挂着它，读的人就再也分不清哪条真的答不出了' };
  }
  if (identityNeeded) {
    const identity = record.gpu_identity;
    if (identity === null || typeof identity !== 'object' || Array.isArray(identity)) {
      return { ok: false, error: 'adapter_name 是 null（浏览器不给名字），却没有 gpu_identity 声明：'
        + '一份说不出"用了哪块卡"的浏览器记录，对 M2 毫无用处' };
    }
    if (identity.state !== 'unresolved') {
      return { ok: false, error: `gpu_identity.state 应为 unresolved，`
        + `收到 ${JSON.stringify(identity.state)}` };
    }
    for (const key of ['reason', 'resolved_by', 'resolves_to']) {
      if (typeof identity[key] !== 'string' || identity[key] === '') {
        return { ok: false, error: `gpu_identity 里没有 ${key}：这个声明的全部作用就是`
          + '"说清缺口在哪、由谁补、补到哪个文件"，少一项就读不出来了' };
      }
    }
    if (identity.resolves_to !== HOST_GPU_FILE) {
      return { ok: false, error: `gpu_identity.resolves_to 是 ${JSON.stringify(identity.resolves_to)}，`
        + `应为 ${HOST_GPU_FILE}——那正是驱动 POST 上来的宿主设备表落盘后的名字` };
    }
  }

  if (!text.endsWith('\n') || text.includes('\r')) {
    return { ok: false, error: 'adapter.json 必须是 LF 换行且以换行结尾（记录字节由 core 定稿）' };
  }
  return { ok: true, record, identityNeeded };
}

/** 一帧的请求校验。`body` 是原始字节。 */
export function validateFrameQuery({ path, digest, bytes }, body) {
  const match = FRAME_NAME_RE.exec(String(path ?? ''));
  if (!match) {
    return { ok: false, error: `帧路径不合形状（要 frames/<场景>-f<三位>.png）：${JSON.stringify(path)}` };
  }
  if (!DIGEST_RE.test(String(digest ?? ''))) {
    return { ok: false, error: `摘要不是 16 位十六进制：${JSON.stringify(digest)}` };
  }
  if (!/^\d+$/.test(String(bytes ?? ''))) {
    return { ok: false, error: `字节数不是整数：${JSON.stringify(bytes)}` };
  }
  if (body.length <= PNG_SIGNATURE.length || !body.subarray(0, PNG_SIGNATURE.length).equals(PNG_SIGNATURE)) {
    return { ok: false, error: `${path} 不是 PNG（签名不对，${body.length} 字节）` };
  }
  if (body.length !== Number(bytes)) {
    return { ok: false, error: `${path} 收到 ${body.length} 字节，URL 里声明 ${bytes} 字节` };
  }
  const actual = fnv1a64Hex(body);
  if (actual !== digest) {
    return { ok: false, error: `${path} 摘要不符：收到 ${actual}，URL 里声明 ${digest}` };
  }
  return { ok: true, scene: match[1], frame: Number(match[2]), digest: actual, bytes: body.length };
}

/**
 * 两半都齐了就落盘 `host-gpu.json`，并把解出来的卡带回去。
 *
 * **两半**指：宿主设备表（驱动 POST 的）与页面报的 `in_page`（`adapter.json` 里的）。
 * 顺序可以反——驱动可能在页面送 `adapter.json` 之前或之后 POST 设备表——所以两个落盘口
 * 都调它，还差一半时它什么也不做（`written: false`）：**不写一份没有答案的文件**。
 * `gpu_identity.resolves_to` 指的正是这个文件，指过去却发现里面没有答案，等于把缺口
 * 挪了个地方、还让"文件在"变成了一个假证据。
 *
 * 解不出来（页面报的 vendor 在表里不是唯一一块卡）时返回 `ok: false`，由调用方**拒收**
 * 整轮。这不是苛刻：一份说不出"用了哪块卡"的浏览器记录，对 M2 毫无用处。
 */
function settleHostGpu(outDir, { hostDevices, hostSource, inPage }) {
  if (hostDevices === null || !inPage) {
    return { ok: true, written: false, resolved: null };
  }
  const match = matchHostGpu(inPage, hostDevices);
  if (!match.ok) return { ok: false, error: match.error };
  const resolved = {
    vendor: inPage.vendor,
    vendor_id: match.device.vendorId,
    device_id: match.device.deviceId,
    device: match.device.deviceString,
    driver_vendor: match.device.driverVendor ?? null,
    driver_version: match.device.driverVersion ?? null,
    match_reason: match.reason,
  };
  const file = {
    source: hostSource,
    devices: hostDevices,
    resolved,
    // 写清这份文件为什么能回答"用了哪块卡"：将来的人读到这里，不必再去翻驱动脚本
    // 才知道这几栏是怎么来的、`devices` 里那几列是什么意思。
    note: `本文件由 scripts/serve-corpus-harness.mjs 落盘：devices 是驱动从 ${HOST_GPU_SOURCE}`
      + ' 原样读到的宿主设备表；resolved 是服务端把页面报的 in_page.vendor 对到其中'
      + '**唯一**一块卡的结果（匹配只走数字 vendorId——vendorString 实测是空串）。',
  };
  writeFileSync(join(outDir, HOST_GPU_FILE), `${JSON.stringify(file, null, 2)}\n`);
  return { ok: true, written: true, resolved };
}

/**
 * `GET /__corpus/host` 该回什么错（`null` = 有答案可回）。**三条理由分开说**：页面 ④' 节
 * 把这句原文显示出来，含混的一句会让人去翻服务端的代码才能知道缺的是哪一半。
 */
function hostGpuReadError(state) {
  if (state.hostDevices === null) {
    return `宿主还没 POST 设备表（驱动在 CDP ${HOST_GPU_SOURCE} 那一步）`;
  }
  if (state.identity === undefined || state.identity.adapterName !== null) {
    return '这一轮的卡身份不由宿主设备表回答（adapter.json 还没落盘，'
      + '或它的 adapter_name 不是 null）';
  }
  return state.identity.resolved === null ? (state.identity.why ?? '还没对上') : null;
}

// ---------------------------------------------------------------------------
// HTTP
// ---------------------------------------------------------------------------

function readBody(request) {
  return new Promise((resolvePromise, rejectPromise) => {
    const chunks = [];
    let total = 0;
    request.on('data', (chunk) => {
      total += chunk.length;
      if (total > MAX_BODY_BYTES) {
        rejectPromise(new Error(`请求体超过 ${MAX_BODY_BYTES} 字节`));
        request.destroy();
        return;
      }
      chunks.push(chunk);
    });
    request.on('end', () => resolvePromise(Buffer.concat(chunks)));
    request.on('error', rejectPromise);
  });
}

function json(res, status, body) {
  const text = JSON.stringify(body);
  res.writeHead(status, {
    'content-type': 'application/json; charset=utf-8',
    'content-length': Buffer.byteLength(text),
    'cache-control': 'no-store',
  });
  res.end(text);
}

/** URL 路径 → www 目录内的真实文件；越界一律拒绝。 */
function resolveStatic(urlPath) {
  const decoded = decodeURIComponent(urlPath.split('?')[0]);
  const relative = decoded === '/' ? 'corpus.html' : decoded.replace(/^\/+/, '');
  const target = normalize(join(WWW_ROOT, relative));
  if (target !== WWW_ROOT && !target.startsWith(WWW_ROOT + sep)) return null;
  return target;
}

function serveStatic(res, target) {
  if (target === null || !existsSync(target) || !statSync(target).isFile()) {
    json(res, 404, { ok: false, error: '没有这个文件' });
    return;
  }
  const body = readFileSync(target);
  res.writeHead(200, {
    'content-type': MIME.get(extname(target)) ?? 'application/octet-stream',
    'content-length': body.length,
    // 开发用：别让浏览器缓存住旧的 wasm。
    'cache-control': 'no-store',
  });
  res.end(body);
}

/** 记录目录里的相对路径（打印与写日志用）。 */
function relativeToRepo(fullPath) {
  return fullPath.slice(REPO_ROOT.length + 1).split(sep).join('/');
}

function startServer(options) {
  const { port, outDir, legSlug } = options;
  const framesDir = join(outDir, 'frames');
  mkdirSync(framesDir, { recursive: true });

  /**
   * 这一轮的状态：**页面送来的东西**，重跑一次就覆盖一次。
   *
   * 期望摘要刻意**不进这里**：它来自命令行、整轮不变，与"页面送来了什么"不是一类东西。
   * 两个地方各存一份，就有两个名字指同一样东西，也就有"读错那一个"的可能——M2 第一次
   * 真跑正是栽在这上面：`startServer` 读的是 `options.expectDigest`，而 `state` 里从来
   * 没有这个键。于是横幅打印"未读到"、URL 不带 `&expect=`、判定里 `digest_matches_expect`
   * 一路是 `null`——**与 native 的比对从来没发生过**，而纯函数自检全绿（它直接喂
   * `crossCheckRun` 样本，接线断了它看不见）。现在只有一个来源：`options.expectDigest`。
   */
  const state = {
    adapterText: null,
    // adapter.json 解析后的对象。**身份那几条检查要同时看两份记录**（adapter.json 的
    // `adapter_name` 与 run.json 的 `backends[0].adapter_name`），所以解析结果留在这里，
    // 不在 run 那一支里再解析一遍——两遍就有两个"记录"，它们不一样的那天没人会知道。
    adapterRecord: null,
    // 宿主设备表：驱动 POST 上来的原文（`null` = 这一轮还没送到）。设备表与 in_page 各是
    // 一半，`identity` 是它们对上之后的结果。
    hostDevices: null,
    hostSource: null,
    // 这一轮的卡身份：`{ adapterName, resolved, why }`（见 settleHostGpu）。
    // 初值是 `undefined`——**不是** `null`：`null` 的语义是"这条腿有名字，不需要宿主表"，
    // 而 `undefined` 是"还没人声明过"，两者在 crossCheckRun 的 ⑧ 里待遇不同（后者要红：
    // 一条不许沉默跳过的检查，跳过了必须有人喊）。
    identity: undefined,
    readingsText: null,
    framesWritten: 0,
    pngBytes: 0,
    verdict: null,
  };

  const server = createServer((request, res) => {
    const url = new URL(request.url ?? '/', 'http://127.0.0.1');
    const route = `${request.method} ${url.pathname}`;

    if (request.method === 'POST' && url.pathname.startsWith('/__corpus/')) {
      handlePost({ request, res, url, route, outDir, framesDir, legSlug, state, options })
        .catch((error) => {
          console.error(`✗ ${route} 出错：${error.message}`);
          json(res, 500, { ok: false, error: error.message });
        });
      return;
    }
    // `GET /__corpus/host`：页面 ④' 节来回填"宿主补的卡身份"。它是**读**口，得排在静态
    // 文件那一支前面——那一支会把 `/__corpus/host` 当成 www 里不存在的文件回 404。
    if (request.method === 'GET' && url.pathname === '/__corpus/host') {
      const error = hostGpuReadError(state);
      if (error !== null) {
        json(res, 200, { ok: false, error });
        return;
      }
      json(res, 200, {
        ok: true,
        source: state.hostSource,
        device_count: state.hostDevices.length,
        host_gpu: state.identity.resolved,
      });
      return;
    }
    if (request.method === 'GET' || request.method === 'HEAD') {
      serveStatic(res, resolveStatic(request.url ?? '/'));
      return;
    }
    json(res, 405, { ok: false, error: '只支持 GET / HEAD / POST /__corpus/*' });
  });

  server.on('error', (error) => {
    console.error(`✗ 起不来：${error.message}`);
    process.exitCode = 2;
  });

  server.listen(port, '127.0.0.1', () => {
    // 端口 0 时实际端口由系统给；驱动脚本就是靠这一行知道该连哪里。
    const actualPort = server.address().port;
    console.log('dhampir M2 浏览器腿（corpus 取证）');
    console.log(`  页面    ${WWW_ROOT}`);
    console.log(`  产物    ${outDir}`);
    console.log(`  腿名    ${legSlug}`);
    if (options.nativeDir) {
      console.log(`  对照    ${options.nativeDir}（frames_digest ${options.expectDigest ?? '未读到'}）`);
    }
    console.log(`PORT ${actualPort}`);
    console.log(`URL http://127.0.0.1:${actualPort}/corpus.html?leg=${legSlug}&scene=all&frames=0..16`
      + `&autorun=1${options.expectDigest ? `&expect=${options.expectDigest}` : ''}`);
  });
  return server;
}

async function handlePost({ request, res, url, route, outDir, framesDir, legSlug, state, options }) {
  const body = await readBody(request);

  // ---- 宿主设备表（驱动 POST，不是页面）-----------------------------------
  //
  // 这一口为什么存在：浏览器不给显卡型号（见 wasm 侧的 IN_PAGE_NOTE），而"用了哪块卡"
  // 是这份记录的全部意义。补这一环的只能是宿主——驱动从 CDP 拿到设备表 POST 上来，
  // 服务端把它与页面报的 `in_page.vendor` 对上，写出带答案的 `host-gpu.json`。
  //
  // 与 `adapter.json` 的**先后可以反**：两个入口都调 `settleHostGpu`，谁后到谁触发落盘。
  // 顺序不自由的地方在别处——`adapter.json` 的 `gpu_identity.resolves_to` 指着那个文件，
  // 而它写不出来时这份记录就没有"用了哪块卡"，所以那一半由 run 之前的 ⑧ 顶着。
  if (url.pathname === '/__corpus/host') {
    const verdict = validateHostGpuText(body.toString('utf8'));
    if (!verdict.ok) {
      console.error(`✗ 拒收宿主设备表：${verdict.error}`);
      json(res, 400, { ok: false, error: verdict.error });
      return;
    }
    const settled = settleHostGpu(outDir, {
      hostDevices: verdict.devices,
      hostSource: verdict.source,
      inPage: state.adapterRecord?.in_page ?? null,
    });
    // 先对账再改 `state`：对不上时这一轮就是失败的，盘上不该留下半个证据说它来过。
    if (!settled.ok) {
      const error = `宿主设备表与页面报的卡对不上：${settled.error}`;
      console.error(`✗ 拒收宿主设备表：${error}`);
      json(res, 400, { ok: false, error });
      return;
    }
    state.hostDevices = verdict.devices;
    state.hostSource = verdict.source;
    if (state.identity !== undefined && state.identity.adapterName === null) {
      state.identity = {
        adapterName: null,
        resolved: settled.resolved,
        why: settled.written ? null : '页面报的 in_page 还没到（adapter.json 还没落盘）',
      };
    }
    console.log(`✓ 宿主设备表（${verdict.devices.length} 块卡，来自 ${verdict.source}）`
      + (settled.written
        ? `→ ${HOST_GPU_FILE}：卡身份 ${settled.resolved.device}`
        : `（等页面报的 in_page 到了再对账落盘）`));
    json(res, 200, {
      ok: true,
      devices: verdict.devices.length,
      written: settled.written,
      host_gpu: settled.resolved,
    });
    return;
  }

  // ---- ① adapter.json（先落盘：中途失败时"在什么环境里失败的"要查得到）----
  if (url.pathname === '/__corpus/adapter') {
    const text = body.toString('utf8');
    const verdict = validateAdapterText(text, legSlug);
    if (!verdict.ok) {
      console.error(`✗ 拒收 adapter.json：${verdict.error}`);
      json(res, 400, { ok: false, error: verdict.error });
      return;
    }
    // 身份：**没有名字**的腿（浏览器就是）在这一刻把缺口对上。对上才落盘——`gpu_identity`
    // 里那句"答案在 host-gpu.json 里"必须是句实话，而这份记录正是写它的地方。
    const settled = verdict.identityNeeded
      ? settleHostGpu(outDir, {
        hostDevices: state.hostDevices,
        hostSource: state.hostSource,
        inPage: verdict.record.in_page,
      })
      : { ok: true, written: false, resolved: null };
    if (!settled.ok) {
      const error = `卡身份解不出来：${settled.error}——这份记录说不出"用了哪块卡"，`
        + '而那是它的全部意义';
      console.error(`✗ 拒收 adapter.json：${error}`);
      json(res, 400, { ok: false, error });
      return;
    }
    const file = join(outDir, 'adapter.json');
    writeFileSync(file, body);
    state.adapterText = text;
    state.adapterRecord = verdict.record;
    state.identity = verdict.identityNeeded
      ? {
        adapterName: null,
        resolved: settled.resolved,
        why: settled.written
          ? null
          : `宿主设备表还没送到（驱动要先 POST /__corpus/host，从 ${HOST_GPU_SOURCE} 读）`
            + '——页面报的 vendor 没东西可以对',
      }
      : { adapterName: verdict.record.adapter_name, resolved: null, why: null };
    const relative = relativeToRepo(file);
    console.log(`✓ adapter.json（${body.length} 字节）`
      + `wgpu 报的名字=${JSON.stringify(verdict.record.adapter.name)}，`
      + (settled.written ? `卡身份 ${settled.resolved.device}` : `卡身份待补（${state.identity.why}）`));
    json(res, 200, {
      ok: true,
      file: relative,
      // 这份文件**真写了多少字节**（写它的是这一侧，知道盘上是几字节的也只有这一侧）。
      // 页面那一栏必须抄这个数，不许拿 `text.length` 顶替——那是 UTF-16 码元数，
      // readings.txt 上实测差过 33424 字节（中文一个字 3 字节，`.length` 只算 1）。
      bytes: body.length,
      // 名字这一栏回的是**记录里**那个值（`null` 而不是空串）：页面展示的是记录的事实，
      // 不是 wgpu 那份原文——原文在下面单独一栏里，别让两件事共用一个名字。
      adapter_name: verdict.record.adapter_name,
      wgpu_adapter_name: verdict.record.adapter.name,
      host_gpu: settled.resolved,
      backend_slug: verdict.record.backend_slug,
      requested_backends: verdict.record.requested_backends,
    });
    return;
  }

  // ---- ② 一帧 PNG --------------------------------------------------------
  if (url.pathname === '/__corpus/frame') {
    const verdict = validateFrameQuery({
      path: url.searchParams.get('path'),
      digest: url.searchParams.get('digest'),
      bytes: url.searchParams.get('bytes'),
    }, body);
    if (!verdict.ok) {
      console.error(`✗ 拒收帧：${verdict.error}`);
      json(res, 400, { ok: false, error: verdict.error });
      return;
    }
    const file = join(outDir, verdict.scene === undefined ? '' : 'frames', `${verdict.scene}-f${String(verdict.frame).padStart(3, '0')}.png`);
    writeFileSync(file, body);
    state.framesWritten += 1;
    state.pngBytes += body.length;
    const relative = relativeToRepo(file);
    if (state.framesWritten === 1 || state.framesWritten % 10 === 0) {
      console.log(`· 已落盘 ${state.framesWritten} 帧（${relative}，${body.length} 字节）`);
    }
    json(res, 200, { ok: true, file: relative, bytes: body.length, digest: verdict.digest });
    return;
  }

  // ---- ③ readings.txt ----------------------------------------------------
  if (url.pathname === '/__corpus/readings') {
    const text = body.toString('utf8');
    if (parseReadingsCounts(text) === null) {
      const error = `readings.txt 的头两行不合形状（第一行应为 ${JSON.stringify(READINGS_HEADER)}）`;
      console.error(`✗ 拒收 readings.txt：${error}`);
      json(res, 400, { ok: false, error });
      return;
    }
    if (!text.endsWith('\n') || text.includes('\r')) {
      const error = 'readings.txt 必须是 LF 换行且以换行结尾';
      console.error(`✗ 拒收 readings.txt：${error}`);
      json(res, 400, { ok: false, error });
      return;
    }
    const file = join(outDir, 'readings.txt');
    writeFileSync(file, body);
    state.readingsText = text;
    const relative = relativeToRepo(file);
    console.log(`✓ readings.txt（${body.length} 字节）`);
    json(res, 200, { ok: true, file: relative, bytes: body.length });
    return;
  }

  // ---- ④ run.json：**校验通过才写** ---------------------------------------
  if (url.pathname === '/__corpus/run') {
    const text = body.toString('utf8');
    let record;
    try {
      record = JSON.parse(text);
    } catch (error) {
      const message = `run.json 不是 JSON：${error.message}`;
      console.error(`✗ 拒收 run.json：${message}`);
      json(res, 400, { ok: false, error: message });
      return;
    }
    if (state.adapterText === null) {
      const message = 'adapter.json 还没落盘就送来了 run.json——顺序不是装饰：'
        + '失败时"在什么环境里失败的"必须已经在盘上';
      console.error(`✗ 拒收 run.json：${message}`);
      json(res, 400, { ok: false, error: message });
      return;
    }
    if (state.readingsText === null) {
      const message = 'readings.txt 还没落盘就送来了 run.json——两份文本必须描述同一次运行';
      console.error(`✗ 拒收 run.json：${message}`);
      json(res, 400, { ok: false, error: message });
      return;
    }

    const onDisk = readFramesDir(framesDir);
    const verdict = crossCheckRun({
      record,
      onDisk,
      expectDigest: options.expectDigest,
      readingsCounts: parseReadingsCounts(state.readingsText),
      // 这个服务只写浏览器腿，于是它在这里声明"我在验哪条腿"。
      expectRequested: BROWSER_BACKEND_LABEL,
      // 卡身份：要同时看两份记录（adapter.json 的 `adapter_name` 与 run.json 的
      // `backends[0].adapter_name`），而**只有服务端手里两份都有**——所以对账在这里做（见 ⑧）。
      identity: state.identity,
    });
    const readingsCompare = compareReadingsWithNative(state.readingsText, options.nativeDir);
    if (readingsCompare) verdict.findings.push(readingsCompare);

    for (const check of verdict.checks) console.log(`  ${check.ok ? '✓' : '✗'} ${check.detail}`);
    for (const finding of verdict.findings) {
      console.log(`  ${finding.ok === null ? '·' : finding.ok ? '✓' : '!'} ${finding.detail}`);
    }

    if (!verdict.ok) {
      const failed = verdict.checks.filter((check) => !check.ok).length;
      const message = `${failed} 项完整性检查没通过——**没有写 run.json**：`
        + '一份已被判定不可信的记录写进 records/ 比没有更糟';
      console.error(`✗ ${message}`);
      console.error(`VERDICT ${JSON.stringify({ ok: false, phase: 'run', leg: legSlug, error: message, checks: verdict.checks, findings: verdict.findings })}`);
      json(res, 400, { ok: false, error: message, checks: verdict.checks, findings: verdict.findings });
      return;
    }

    writeFileSync(join(outDir, 'run.json'), body);
    const actualDigest = record.backends[0].frames_digest;
    state.verdict = {
      ok: true,
      phase: 'run',
      leg: legSlug,
      out_dir: relativeToRepo(outDir),
      // 同 adapter 口：run.json 真写了多少字节。页面把这一栏当"我送出去多少字节"的答案
      // （它自己那个 `summary.run_json.length` 是码元数，不是字节）。
      bytes: body.length,
      frames_verified: record.backends[0].frames.length,
      png_bytes_total: state.pngBytes,
      frames_digest: actualDigest,
      counts: record.backends[0].counts,
      expected_frames_digest: options.expectDigest,
      digest_matches_expect: options.expectDigest ? actualDigest === options.expectDigest : null,
      adapter_name: record.backends[0].adapter_name,
      // 这一轮是哪块卡画的。浏览器腿上是宿主设备表对出来的那块（见 host-gpu.json），
      // 有名字的腿就是名字本身——两种都收在 `identity` 里，这里只投影其中一半。
      host_gpu: state.identity?.resolved ?? null,
      checks: verdict.checks,
      findings: verdict.findings,
    };
    console.log(`✓ run.json（${body.length} 字节）——完整性检查全过，已落盘 ${relativeToRepo(join(outDir, 'run.json'))}`);
    console.log(`VERDICT ${JSON.stringify(state.verdict)}`);
    json(res, 200, state.verdict);
    return;
  }

  json(res, 404, { ok: false, error: `没有这个落盘口：${route}` });
}

/** 两条腿的 `readings.txt` 摆在一起比一比：同一份文本就说明逐点读数全等。 */
function compareReadingsWithNative(browserText, nativeDir) {
  if (!nativeDir) return null;
  const nativePath = join(REPO_ROOT, nativeDir, 'readings.txt');
  if (!existsSync(nativePath)) {
    return { ok: null, detail: `${nativeDir}/readings.txt 不在，跳过逐点读数比对` };
  }
  const nativeText = readFileSync(nativePath, 'utf8');
  if (nativeText === browserText) {
    return { ok: true, detail: `readings.txt 与 ${nativeDir} 逐字节相同（逐点实测值全等）` };
  }
  const browserLines = browserText.split('\n');
  const nativeLines = nativeText.split('\n');
  let firstDiff = 0;
  while (firstDiff < browserLines.length && browserLines[firstDiff] === nativeLines[firstDiff]) firstDiff += 1;
  return {
    ok: false,
    detail: `readings.txt 与 ${nativeDir} 不同（浏览器 ${browserLines.length} 行、native `
      + `${nativeLines.length} 行，首个不同在第 ${firstDiff + 1} 行）——交给 T2.4 逐帧 SSIM 定量`,
  };
}

// ---------------------------------------------------------------------------
// 接线自检：真的起服务、真的 POST
// ---------------------------------------------------------------------------

/** 自检用的临时目录根：`target/` 已被 `.gitignore` 忽略，跑完自己删。 */
const SCRATCH_ROOT = join(REPO_ROOT, 'target');
/** 接线自检用的腿名。形状与真跑一致即可，**不写进 records/**。 */
const SELF_TEST_LEG = 'm2';

/**
 * 自检用的宿主设备表：**本机实测的真值**，不是编的——这张表要拿去与样本里的
 * `in_page.vendor` 对账，编出来的话这条自检就只是在验自己。
 *
 * NVIDIA 那一行与 `records/m1/dx12/adapter.json` 的 `vendor`/`device` 逐字相同（M1 那条腿
 * 就是在这块卡上跑的）；AMD 那一行来自本机 CDP `SystemInfo.getInfo` 的实测输出。放两块
 * 厂商的卡是有用的：匹配**不能**靠"表里只有一块卡"通过，必须真的按厂商挑出来。
 */
const SELF_TEST_HOST_DEVICES = [
  { vendorId: 4318, deviceId: 10118, deviceString: 'NVIDIA GeForce RTX 4070',
    driverVendor: 'NVIDIA', driverVersion: '32.0.16.1074' },
  { vendorId: 4098, deviceId: 5056, deviceString: 'AMD Radeon(TM) Graphics',
    driverVendor: 'AMD', driverVersion: '32.0.21030.2001' },
];

/**
 * 浏览器腿 `adapter.json` 的样本：**形状来自 wasm 侧的 `browser_adapter_json`**。
 *
 * 只写出服务端**真会读**的那些键。多写几列（`target_format`、`wgpu_version`、`probe_digest`
 * 之类）等于在 JS 里维护第二份记录形状——它会漂，而且没有任何东西会发现它漂了。
 *
 * 几个值的来历（都不是编的）：
 *
 *  - `adapter` 七列是 core 的 `describe_adapter`；Chrome 上 `name` 是
 *    `GPUAdapterInfo.description()`（空串）、`vendor`/`device` 被 wgpu 的 webgpu 后端写死成
 *    0（`wgpu/src/backend/webgpu.rs::map_adapter_info`），所以那一块在浏览器上**认不出卡**
 *    —— 这正是这份记录需要 `in_page` 与 `gpu_identity` 的原因。服务端只拿 `adapter.name`
 *    去与身份声明对账，其余几列的定稿在 core。
 *  - `in_page` 的形状同 `parse_in_page`；`vendor` / `architecture` / `subgroup_min_size`
 *    是本机实测（RTX 4070 报 `nvidia` / `lovelace` / `32`）。
 *  - `note` 与 `reason` 只要求**非空**（服务端不比对原文）：真记录里那两段话在 Rust 的
 *    `IN_PAGE_NOTE` 与 `unresolved_identity` 里，抄一份到 JS 就是等着它漂开。
 */
function browserAdapterSample() {
  return {
    schema: CORPUS_RECORD_SCHEMA,
    milestone: CORPUS_TABLE_MILESTONE,
    kind: 'adapter',
    adapter: {
      name: '', backend: 'BrowserWebGpu', device_type: 'DiscreteGpu',
      driver: '', driver_info: '', vendor: '0', device: '0',
    },
    adapter_name: null,
    in_page: {
      vendor: 'nvidia',
      architecture: 'lovelace',
      device: '',
      description: '',
      is_fallback_adapter: false,
      subgroup_min_size: 32,
      subgroup_max_size: null,
      note: '（自检样本）浏览器自己报的适配器身份；真记录里这一段的话由 parse_in_page 写。',
    },
    gpu_identity: {
      state: 'unresolved',
      reason: '（自检样本）浏览器不暴露显卡型号；真记录里这一段的话由 unresolved_identity 写。',
      resolved_by: 'harness',
      resolves_to: HOST_GPU_FILE,
    },
    requested_backends: BROWSER_BACKEND_LABEL,
    backend_slug: SELF_TEST_LEG,
  };
}

/** POST 一份 body；返回 `{status, body}`（body 能解析成 JSON 就解析，不能就是原文）。 */
async function postTo(port, path, query, body) {
  const url = `http://127.0.0.1:${port}${path}${query ? `?${query}` : ''}`;
  const response = await fetch(url, { method: 'POST', body });
  const text = await response.text();
  let parsed;
  try {
    parsed = JSON.parse(text);
  } catch {
    parsed = { error: text };
  }
  return { status: response.status, body: parsed };
}

/**
 * 起一个**真的服务进程**，把 M1 归档那一轮原样喂进去，把它的横幅与判定带回来。
 *
 * 为什么非要走"起进程 + HTTP"这一步，而不是把 `startServer` 拿进来直接调：`--native`
 * 那条线要穿过 `parseArgs → resolveExpectDigest → startServer → 横幅 / 判定` 四道手，
 * 而**断的正是这四道手之间的接线**（见 `startServer` 里那段注释：期望摘要曾经被读成
 * 一个不存在的 `state.expectDigest`）。纯函数自检喂 `crossCheckRun` 样本当然全绿——
 * 它绕过整条线。这条自检是唯一能测到"接线掉了"的形态，所以它宁可慢一点、真起进程。
 *
 * 喂进去的帧 / 读数 / 账本**原样来自归档**，只改三处与渲染无关的声明：
 * `requested`、`backend_slug`、`adapter_name`——第三处是因为这条自检喂的是**浏览器腿**
 * 的 adapter 样本（见 `browserAdapterSample`），它答不出卡名，所以整轮记录里那一栏也
 * 必须是 `null`。改别的字节都会让这条自检变成假的。
 *
 * 后面四个参数专门用来造**坏样本**，它们是参数而不是"另一种跑法"：
 *
 *  - `hostDevices`：要 POST 的宿主设备表；`null` = **不** POST（"两半只到一半"那条路）。
 *  - `hostSource`：表里那句来源声明（换掉它等于换了"哪块卡"的判据，契约要求拒收）。
 *  - `hostFirst`：设备表与 adapter.json 谁先到。契约说顺序自由，那就得有两条路都真跑过的证据。
 *  - `adapter`：整个 adapter 样本的覆盖（默认是浏览器腿的真实形状）。
 */
async function driveServer({
  tag, nativeDir, expect, mutateFrame,
  hostDevices = SELF_TEST_HOST_DEVICES,
  hostSource = HOST_GPU_SOURCE,
  hostFirst = true,
  adapter = null,
}) {
  // 样本永远来自归档，**与"服务端被告知要跟谁比"无关**：后者可以是 null（错期望那一轮）。
  const fixtureDir = 'records/m1/dx12';
  mkdirSync(SCRATCH_ROOT, { recursive: true });
  const outDir = mkdtempSync(join(SCRATCH_ROOT, `${tag}-`));
  const argv = [join(HERE, 'serve-corpus-harness.mjs'), '--port', '0',
    '--out', outDir, '--leg', SELF_TEST_LEG];
  // 两个期望值的来源是**二选一**（服务端会拒收同时给的两个）。给目录就够：摘要由
  // `resolveExpectDigest` 从那份 `run.json` 里读——M2 第一次真跑断掉的正是这条支路。
  if (nativeDir) argv.push('--native', nativeDir);
  else if (expect) argv.push('--expect', expect);

  const child = spawn(process.execPath, argv, { cwd: REPO_ROOT, stdio: ['ignore', 'pipe', 'pipe'] });
  let banner = '';
  let stderr = '';
  child.stdout.setEncoding('utf8');
  child.stderr.setEncoding('utf8');
  child.stdout.on('data', (chunk) => { banner += chunk; });
  child.stderr.on('data', (chunk) => { stderr += chunk; });
  const exitCode = new Promise((resolveExit) => child.on('exit', resolveExit));

  const result = {
    outDir, exit: null, status: null, body: null,
    // 送不进去的东西（与"判定不过"是两种红灯，归因完全不同）。**不叫 frameErrors**：
    // 这几个落盘口里只有一个是帧。
    portErrors: [], aborted: false, hostRes: null, adapterRes: null,
  };
  // `banner` / `stderr` 必须**按下标读时才是此刻的样子**。这里原先写的是 `{ banner, stderr, … }`：
  // 字符串按值复制，那一刻横幅还一个字没到，于是"横幅里带上了期望摘要"这条断言读到的永远是
  // 空串——它会**恒定红，且红得毫无理由**（断言本身是对的，坏的是它读的那个副本）。
  // 用 getter 而不是"最后再抄一遍"：GET 之外还有 VERDICT 等后到的行，快照总有漏掉的窗口。
  Object.defineProperties(result, {
    banner: { get: () => banner, enumerable: true },
    stderr: { get: () => stderr, enumerable: true },
  });
  try {
    const deadline = Date.now() + 20000;
    /** 等到累加输出里出现某个形状为止；服务自己退了就立刻停（它的输出里已经写着原因）。 */
    const waitFor = async (pattern, what) => {
      let found = pattern.exec(banner);
      while (found === null && Date.now() < deadline) {
        const raced = await Promise.race([exitCode, new Promise((wake) => setTimeout(wake, 100, null))]);
        if (raced !== null) break;
        found = pattern.exec(banner);
      }
      if (found === null) throw new Error(`20 s 内没有${what}横幅（stderr：${stderr.trim() || '（空）'}）`);
      return found;
    };
    // 服务端把 `PORT <n>` 与 `URL …` 连着打印。**两行都等到**：PORT 是连接信息，URL 是
    // "该开哪个地址"的公开契约（`&expect=` 就在里面）。只等 PORT 会让 URL 那条断言变成赛跑——
    // stdout 是流，看到 PORT 那个 chunk 时 URL 可能还在管道里。
    const port = Number((await waitFor(/^PORT (\d+)$/m, ' PORT '))[1]);
    await waitFor(/^URL /m, ' URL ');

    /**
     * 发一份证据。**非 200 就停下整轮**：后面的口都指望前面那份证据已经在盘上，硬发下去
     * 只会把"送不进去"演成"判定不过"——而这条自检的全部作用就是别把两种红灯混成一个。
     */
    const postPort = async (label, path, query, payload) => {
      const res = await postTo(port, path, query, payload);
      if (res.status !== 200) {
        result.portErrors.push(`${label}：${res.body.error}`);
        result.aborted = true;
      }
      return res;
    };

    const sample = adapter ?? browserAdapterSample();
    const adapterText = `${JSON.stringify({
      ...sample, backend_slug: SELF_TEST_LEG, requested_backends: BROWSER_BACKEND_LABEL,
    }, null, 2)}\n`;
    const hostTable = hostDevices === null ? null
      : `${JSON.stringify({ source: hostSource, devices: hostDevices }, null, 2)}\n`;

    // 谁先到都行（契约如此），所以顺序是个参数——两条路都得真跑过一次才算这条契约有人验过。
    if (hostTable !== null && hostFirst) {
      result.hostRes = await postPort('宿主设备表', '/__corpus/host', '', hostTable);
    }
    // 被拒了就**不再往下送**：这个口红了之后的每一份证据都是给一条已经坏掉的轮次添料，
    // 而"送不进去"与"判定不过"必须一直分得开。
    if (!result.aborted) {
      result.adapterRes = await postPort('adapter.json', '/__corpus/adapter', '', adapterText);
    }
    if (hostTable !== null && !hostFirst && !result.aborted) {
      result.hostRes = await postPort('宿主设备表', '/__corpus/host', '', hostTable);
    }

    const nativeRun = JSON.parse(readFileSync(join(REPO_ROOT, fixtureDir, 'run.json'), 'utf8'));
    const rows = nativeRun.backends[0].frames;
    const framesRoot = join(REPO_ROOT, fixtureDir, 'frames');
    if (!result.aborted) {
      for (const row of rows) {
        let bytes = readFileSync(join(framesRoot, row.png.split('/').pop()));
        if (mutateFrame && mutateFrame(row.png)) {
          bytes = Buffer.from(bytes);
          bytes[bytes.length - 1] ^= 0xff; // 翻最后一字节：签名还在，但这份文件的摘要变了
        }
        const query = `path=${encodeURIComponent(row.png)}&digest=${fnv1a64Hex(bytes)}&bytes=${bytes.length}`;
        await postPort(row.png, '/__corpus/frame', query, bytes);
        if (result.aborted) break;
      }
    }
    if (!result.aborted) {
      const readingsPath = join(REPO_ROOT, fixtureDir, 'readings.txt');
      result.readingsRes = await postPort('readings.txt', '/__corpus/readings', '', readFileSync(readingsPath));
    }
    if (!result.aborted) {
      const record = JSON.parse(JSON.stringify(nativeRun));
      record.backends[0].requested = BROWSER_BACKEND_LABEL;
      // 两份记录说的卡名必须是同一个（⑦ 判的就是它们对不对得上），所以这一栏与 adapter
      // 样本**同源**。要造"对不上"那条红线，改的是这两处之一，不是各写各的。
      record.backends[0].adapter_name = sample.adapter_name;
      const runRes = await postTo(port, '/__corpus/run', '', Buffer.from(`${JSON.stringify(record, null, 2)}\n`));
      result.status = runRes.status;
      result.body = runRes.body;
    }
  } catch (error) {
    result.error = error.message;
  } finally {
    child.kill();
    result.exit = await Promise.race([exitCode, new Promise((wake) => setTimeout(wake, 2000, '（超时未退）'))]);
  }
  return result;
}

/**
 * 接线自检失败时**别只说"没过"**：把服务端说的话带出来。
 *
 * 这一层自检的红灯不常见，一旦出现就是"环境/接线坏了"。只报"××× 没过"会让人去读脚本
 * （而脚本是对的），所以这里把几类证据按顺序拼起来：起不来的原因、停在哪一步、HTTP 的
 * 错、判定里没过的条目、送不进去的东西（落盘口的拒收），最后兜底给它 stderr 的尾巴。
 */
function whyFailed(run) {
  const failedChecks = (run.body?.checks ?? []).filter((item) => !item.ok)
    .map((item) => item.detail).slice(0, 2);
  return [
    run.error,
    run.aborted ? '（这一轮在落盘口就被拒了，根本没走到判定）' : null,
    run.body?.error,
    ...failedChecks,
    run.portErrors.slice(0, 2).join('、'),
    run.stderr.trim().split('\n').slice(-2).join(' / '),
  ].filter(Boolean).join('；') || '（服务端什么也没说）';
}

/** 接线自检的全部用例。返回 `{cases, keep}`：`keep` 是留着给人看的现场。 */
async function wiringSelfTest(check) {
  const nativeDir = 'records/m1/dx12';
  const nativeRun = JSON.parse(readFileSync(join(REPO_ROOT, nativeDir, 'run.json'), 'utf8'));
  const rows = nativeRun.backends[0].frames;
  const expect = nativeRun.backends[0].frames_digest;
  const spotless = [];

  /** 某一轮的断言：红灯时把**服务端说过的话**一起带出来，别只报"没过"。 */
  const legs = (run) => (name, condition) => check(condition ? name : `${name}——服务端说：${whyFailed(run)}`, condition);
  /** 盘上那份 `host-gpu.json`（没有就是 `null`）。 */
  const hostGpuOnDisk = (outDir) => {
    const file = join(outDir, HOST_GPU_FILE);
    return existsSync(file) ? JSON.parse(readFileSync(file, 'utf8')) : null;
  };
  /** 某一轮产物目录里那份文件的字节数（没有就是 `null`）。 */
  const sizeOnDisk = (outDir, name) => {
    const file = join(outDir, name);
    return existsSync(file) ? statSync(file).size : null;
  };
  const nothingOnDisk = (outDir) => !existsSync(join(outDir, HOST_GPU_FILE))
    && !existsSync(join(outDir, 'adapter.json')) && !existsSync(join(outDir, 'run.json'));

  // ---- 正向：好样本（宿主设备表先到） -------------------------------------
  const good = await driveServer({ tag: 'wiring-good', nativeDir, expect });
  if (good.error) {
    check(`（端到端）好样本跑起来了（失败：${good.error}）`, false);
  } else {
    spotless.push(good);
    const ok = legs(good);
    const hostGpu = hostGpuOnDisk(good.outDir);
    // 横幅这一条是**这个缺陷的指纹**：接线断了时它打印"未读到"、URL 少一个 &expect=。
    ok('（端到端）横幅里带上了期望摘要（断线时这里就没有 &expect=）',
      good.banner.includes(`&expect=${expect}`));
    ok('（端到端）好样本：判定全过（HTTP 200）', good.status === 200 && good.body.ok === true);
    ok('（端到端）判定里带上了期望摘要（断线时这里是 null）',
      good.body.expected_frames_digest === expect);
    ok('（端到端）与 native 逐字节相同这条发现成立', good.body.digest_matches_expect === true);
    ok('（端到端）run.json 真落盘了', existsSync(join(good.outDir, 'run.json')));
    ok('（端到端）盘上 80 帧（服务端说自己写了 80，盘上也得是 80）',
      readdirSync(join(good.outDir, 'frames')).filter((name) => name.endsWith('.png')).length === rows.length);

    // ---- 文本产物"写了多少字节"：页面的那一栏只能抄服务端这一侧实测的数 ----------
    //
    // 页面自己算不出这个数：`text.length` 是 UTF-16 码元数，中文一个字在盘上是 3 字节而它只算 1
    // （readings.txt 上实测差过 33424 字节）。所以三个落盘口都得把"真写了多少"回出去，
    // 而回出去的那个数得与**盘上文件的字节数**一致——两个数走的是两条不同的路径
    // （Buffer 长度 vs 文件系统），相等才是结论，不是同一句话说了两遍。
    // （三个都在 `body` 里：`adapterRes`/`readingsRes` 是 `{status, body}`，run 那一口
    //  `driveServer` 只留了 body，所以这里统一取 `body`。）
    for (const [name, body] of [
      ['adapter.json', good.adapterRes?.body], ['readings.txt', good.readingsRes?.body], ['run.json', good.body],
    ]) {
      const onDisk = sizeOnDisk(good.outDir, name);
      ok(`（端到端）回话里带上了 ${name} 写了多少字节`, Number.isInteger(body?.bytes) && body.bytes > 0);
      ok(`（端到端）${name} 的字节数：回话 ${body?.bytes} 与盘上 ${onDisk} 是同一个`,
        body?.bytes !== undefined && body.bytes === onDisk);
    }

    // 卡身份这一条要三处说同一块卡：adapter 口的回话、判定、盘上那份 host-gpu.json。
    // 浏览器腿的 `adapter.name` 是空串，所以"用了哪块卡"只能是宿主设备表对出来的那块——
    // 三处少一处，这份记录就答不出那个问题（而那是它的全部意义）。
    ok('（端到端）adapter 口收下了（浏览器腿的真实形状：adapter_name 是 null 而不是空串）',
      good.adapterRes?.status === 200 && good.adapterRes.body.adapter_name === null
      && good.adapterRes.body.wgpu_adapter_name === '');
    // 设备表**先**到时服务端只收不判：此刻它还看不到页面报的 in_page.vendor，而 vendor 正是
    // 匹配的起点（见 settleHostGpu 第一行）。要钉的不是"这里解出了卡"（那是错的预期，
    // 真那样必是拿别的什么东西猜的），而是"收下、且此刻不宣称解出了卡"——把没解出来的
    // 那一口说成解出来了，比不解出来更坏。
    ok('（端到端）设备表先到：收下、但此刻不下结论（此刻还看不到页面报的 vendor）',
      good.hostRes?.status === 200 && good.hostRes.body.host_gpu === null
      && good.hostRes.body.written === false);
    ok(`（端到端）${HOST_GPU_FILE} 真在盘上（adapter.json 那句 resolves_to 指的就是它）`,
      hostGpu !== null);
    ok(`（端到端）${HOST_GPU_FILE} 里写着"哪块卡"：来源、整张表、解出来的那一行都对得上`,
      hostGpu?.source === HOST_GPU_SOURCE
      && hostGpu.devices?.length === SELF_TEST_HOST_DEVICES.length
      && hostGpu.resolved?.device === 'NVIDIA GeForce RTX 4070'
      && hostGpu.resolved?.vendor === 'nvidia'
      && hostGpu.resolved?.vendor_id === 0x10de
      && hostGpu.resolved?.device_id === 10118
      && typeof hostGpu.resolved?.match_reason === 'string' && hostGpu.resolved.match_reason !== '');
    ok('（端到端）判定里的 host_gpu 是同一块卡（判定、adapter 口、盘上三处同源）',
      good.body.host_gpu?.device === 'NVIDIA GeForce RTX 4070' && good.body.host_gpu?.device_id === 10118
      && good.body.adapter_name === null);
  }

  // ---- 反向：设备表后到（契约说顺序自由，那两条路都得真跑过） --------------
  const lateHost = await driveServer({ tag: 'wiring-late-host', nativeDir, expect, hostFirst: false });
  if (lateHost.error) {
    check(`（端到端）设备表后到的样本跑起来了（失败：${lateHost.error}）`, false);
  } else {
    spotless.push(lateHost);
    const ok = legs(lateHost);
    ok('（端到端）设备表后到：两个口都收下（顺序自由不是一句空话）',
      lateHost.adapterRes?.status === 200 && lateHost.hostRes?.status === 200);
    // 与上面"设备表先到"那一条对着读：同样是这一口，那一次回 `host_gpu: null`（判据还没齐），
    // 这一次回出了那块卡（adapter.json 先到，vendor 已经摆在 state 里了）。两处一起才说明
    // "顺序自由"不是因为服务端两边都不看。
    ok('（端到端）设备表后到：这一口正是在适配器之后解出来的（回 host_gpu，且当场落盘）',
      lateHost.hostRes.body.host_gpu?.device === 'NVIDIA GeForce RTX 4070'
      && lateHost.hostRes.body.written === true);
    ok('（端到端）设备表后到：判定仍然全过，卡身份仍是那块卡',
      lateHost.status === 200 && lateHost.body.ok === true
      && lateHost.body.host_gpu?.device === 'NVIDIA GeForce RTX 4070');
    ok('（端到端）设备表后到：host-gpu.json 照样落盘（谁后到谁触发落盘）',
      hostGpuOnDisk(lateHost.outDir)?.resolved?.device === 'NVIDIA GeForce RTX 4070');
  }

  // ---- 反向：两半只到一半（不 POST 宿主设备表） ---------------------------
  //
  // 这一条钉的是"缺口不许被悄悄咽下去"：adapter.json 自己说得出"我答不出卡名"（所以它
  // 收得下），但整轮记录答不出"用了哪块卡"——那必须落在 run 之前，而不是写出一份少一栏的
  // 记录。反过来也要钉住：**拒收整轮时 adapter.json 要留在盘上**，失败发生在什么环境里
  // 得查得到。
  const noHost = await driveServer({ tag: 'wiring-no-host', nativeDir, expect, hostDevices: null });
  if (noHost.error) {
    check(`（端到端）缺宿主设备表的样本跑起来了（失败：${noHost.error}）`, false);
  } else {
    spotless.push(noHost);
    const ok = legs(noHost);
    const reasons = (noHost.body?.checks ?? []).filter((item) => !item.ok)
      .map((item) => item.detail).join(' | ');
    ok('（端到端）缺设备表：adapter.json 照收、照落盘（它自己说了缺口在哪）',
      noHost.adapterRes?.status === 200 && existsSync(join(noHost.outDir, 'adapter.json')));
    ok(`（端到端）缺设备表：没有写出 ${HOST_GPU_FILE}（不写一份没有答案的文件）`,
      !existsSync(join(noHost.outDir, HOST_GPU_FILE)));
    ok('（端到端）缺设备表：run 被拒（HTTP 400）且没有写 run.json',
      noHost.status === 400 && noHost.body?.ok === false
      && !existsSync(join(noHost.outDir, 'run.json')));
    ok('（端到端）缺设备表：拒收理由是"卡身份没解出来"且点名设备表该从哪来',
      reasons.includes('卡身份没解出来') && reasons.includes(HOST_GPU_SOURCE));
  }

  // ---- 反向：宿主设备表本身不合契约（三种坏法） ---------------------------
  const badSource = await driveServer({ tag: 'wiring-host-source', nativeDir, expect, hostSource: 'CUDA' });
  if (badSource.error) {
    check(`（端到端）来源不对的样本跑起来了（失败：${badSource.error}）`, false);
  } else {
    spotless.push(badSource);
    const ok = legs(badSource);
    ok('（端到端）反向：设备表来源不对 → 拒收（HTTP 400）', badSource.hostRes?.status === 400);
    ok('（端到端）反向：拒收理由点名该写哪个来源（换来源等于换判据）',
      String(badSource.hostRes?.body.error ?? '').includes(HOST_GPU_SOURCE));
    ok('（端到端）反向：停在落盘口，没走到判定（`aborted`——两种红灯不许混成一个）',
      badSource.aborted === true && badSource.status === null);
    ok('（端到端）反向：盘上不留半个证据（连 adapter.json 也没有）', nothingOnDisk(badSource.outDir));
  }

  const namelessDevice = await driveServer({
    tag: 'wiring-host-noname', nativeDir, expect,
    hostDevices: [SELF_TEST_HOST_DEVICES[0], { ...SELF_TEST_HOST_DEVICES[1], deviceString: '' }],
  });
  if (namelessDevice.error) {
    check(`（端到端）设备表缺 deviceString 的样本跑起来了（失败：${namelessDevice.error}）`, false);
  } else {
    spotless.push(namelessDevice);
    const ok = legs(namelessDevice);
    ok('（端到端）反向：某一块卡没有 deviceString → 拒收（HTTP 400）',
      namelessDevice.hostRes?.status === 400);
    ok('（端到端）反向：拒收理由点名 deviceString（那是最终要写进记录的那句话）',
      String(namelessDevice.hostRes?.body.error ?? '').includes('deviceString'));
    ok('（端到端）反向：盘上不留半个证据', nothingOnDisk(namelessDevice.outDir));
  }

  // 二义性：页面报的厂商在表里**不止一块**卡。这条不能用"表里只有一块卡"混过去，
  // 所以样本得真的放两块 NVIDIA（第二块是编的——它只是要占住"同厂商还有一块"这个位置）。
  const ambiguous = await driveServer({
    tag: 'wiring-ambiguous', nativeDir, expect,
    hostDevices: [
      SELF_TEST_HOST_DEVICES[0],
      { vendorId: 0x10de, deviceId: 9351, deviceString: 'NVIDIA GeForce RTX 4090',
        driverVendor: 'NVIDIA', driverVersion: '32.0.16.9999' },
      SELF_TEST_HOST_DEVICES[1],
    ],
  });
  if (ambiguous.error) {
    check(`（端到端）二义性样本跑起来了（失败：${ambiguous.error}）`, false);
  } else {
    spotless.push(ambiguous);
    const ok = legs(ambiguous);
    // 设备表先到时服务端**收得下**它：此刻还不知道页面会报哪个厂商，而 vendor 是匹配的
    // 起点——先收下、等 adapter.json 到了再对账，这不是"漏了一次检查"，是判据本身还没齐。
    ok('（端到端）反向：设备表先到、此刻还判不了（收下，不下结论）', ambiguous.hostRes?.status === 200);
    ok('（端到端）反向：页面报的厂商有两块卡 → adapter.json 被拒（HTTP 400）',
      ambiguous.adapterRes?.status === 400);
    ok('（端到端）反向：拒收理由说出"几块卡"与二义性，而不是挑一块了事',
      String(ambiguous.adapterRes?.body.error ?? '').includes('2 块')
      && String(ambiguous.adapterRes?.body.error ?? '').includes('二义性'));
    ok('（端到端）反向：二义性没解开时不写 host-gpu.json（更不写 adapter.json/run.json）',
      nothingOnDisk(ambiguous.outDir));
  }

  // ---- 反向：改一个字节（判定要红，而且是**因为那一帧**红） ---------------
  // 只断言 400 的话，一个"什么都拒收"的服务端也能过。
  const target = rows[0].png;
  const flipped = await driveServer({
    tag: 'wiring-flip', nativeDir, expect, mutateFrame: (path) => path === target,
  });
  if (flipped.error) {
    check(`（端到端）反向样本跑起来了（失败：${flipped.error}）`, false);
  } else {
    spotless.push(flipped);
    const reasons = (flipped.body?.checks ?? []).filter((item) => !item.ok)
      .map((item) => item.detail).join(' | ');
    const ok = legs(flipped);
    ok('（端到端）反向：改一个字节 → 拒收（HTTP 400）',
      flipped.status === 400 && flipped.body?.ok === false);
    ok('（端到端）反向：**没有**写 run.json（判定不可信的记录写进 records/ 比没有更糟）',
      !existsSync(join(flipped.outDir, 'run.json')));
    ok(`（端到端）反向：拒收理由点名 ${target} 且是"摘要/字节对不上"`,
      reasons.includes(target) && (reasons.includes('盘上摘要') || reasons.includes('盘上 ')));
    ok('（端到端）反向：adapter / 宿主表 / readings / 帧四个口都收下了（红的是判定，不是送不进去）',
      flipped.portErrors.length === 0);
  }

  // ---- 反向：期望值给错（与 native 不同**是发现，不是完整性不过**） -------
  const mismatch = await driveServer({ tag: 'wiring-expect', nativeDir: null, expect: 'deadbeefdeadbeef' });
  if (mismatch.error) {
    check(`（端到端）错期望样本跑起来了（失败：${mismatch.error}）`, false);
  } else {
    spotless.push(mismatch);
    const ok = legs(mismatch);
    ok('（端到端）错期望：判定仍然成立（差异是发现，不是拒收）',
      mismatch.status === 200 && mismatch.body.ok === true);
    ok('（端到端）错期望：digest_matches_expect 是 false，不是 null',
      mismatch.body.digest_matches_expect === false && mismatch.body.expected_frames_digest === 'deadbeefdeadbeef');
    ok('（端到端）错期望：run.json 照写（M2 要找的就是差异）',
      existsSync(join(mismatch.outDir, 'run.json')));
  }

  for (const run of spotless) {
    if (process.env.DHAMPIR_KEEP_SELFTEST) continue;
    rmSync(run.outDir, { recursive: true, force: true });
  }
}

// ---------------------------------------------------------------------------
// 自检
// ---------------------------------------------------------------------------

export async function selfTest() {
  const cases = [];
  const check = (name, ok) => cases.push({ name, ok: Boolean(ok) });

  // ---- 摘要实现必须与 Rust 侧一致（同一组公开向量） ----------------------
  check('fnv1a64("") == 偏移基数', fnv1a64Hex(Buffer.alloc(0)) === 'cbf29ce484222325');
  check('fnv1a64("a") == 公开向量', fnv1a64Hex(Buffer.from('a')) === 'af63dc4c8601ec8c');
  check('fnv1a64("foobar") == 公开向量', fnv1a64Hex(Buffer.from('foobar')) === '85944171f73967e8');
  check('摘要长度固定 16 位', fnv1a64Hex(Buffer.from([0xff, 0xff, 0xff])).length === 16);

  // ---- 路径与区间 --------------------------------------------------------
  check('frame_rel_path 形状', framePathFor('gradient', 7) === 'frames/gradient-f007.png');
  check('区间 "0..16" 解析成 [0,16]', JSON.stringify(parseFrameRange('0..16')) === '[0,16]');
  for (const bad of ['0', '0..', '..16', '16..0', '0..0', 'a..b', '', null]) {
    check(`拒绝区间 ${JSON.stringify(bad)}`, parseFrameRange(bad) === null);
  }

  // ---- 整表摘要的字节布局：与 Rust 那份测试用同一组样本 -------------------
  const u32le = (n) => { const b = Buffer.alloc(4); b.writeUInt32LE(n); return b; };
  const u64le = (n) => { const b = Buffer.alloc(8); b.writeBigUInt64LE(n); return b; };
  const pinnedRows = [
    { scene: 'gradient', frame: 0, pixel_digest: '0102030405060708' },
    { scene: 'blur', frame: 16, pixel_digest: 'ffffffffffffffff' },
  ];
  // 手工拼一遍字节（不调用被测函数），两边对上才算布局一样：
  // `dhampir_core::render::corpus::table_digest` 的测试 `table_digest_layout_is_pinned`
  // 钉的是同一组样本，只是它是从 Rust 侧钉的。
  const pinnedBytes = Buffer.concat([
    Buffer.from('gradient'), Buffer.from([0]), u32le(0), u64le(0x0102030405060708n),
    Buffer.from('blur'), Buffer.from([0]), u32le(16), u64le(0xffffffffffffffffn),
  ]);
  check('整表摘要的字节布局与 Rust 那份测试逐字节相同',
    tableDigestHex(pinnedRows) === fnv1a64Hex(pinnedBytes));
  check('整表摘要对行序敏感（换行序 = 换跑法，摘要必须变）',
    tableDigestHex(pinnedRows) !== tableDigestHex([...pinnedRows].reverse()));
  check('整表摘要看得见像素摘要的每一位',
    tableDigestHex([{ ...pinnedRows[0], pixel_digest: '0102030405060709' }]) !== tableDigestHex([pinnedRows[0]]));
  check('空表的整表摘要是 FNV-1a 的偏移基数', tableDigestHex([]) === 'cbf29ce484222325');
  for (const bad of [
    [{ scene: 'x', frame: 0, pixel_digest: 'ZZZZ' }],
    [{ scene: 'x', frame: 0, pixel_digest: '00000000' }],
    [{ scene: 'x', frame: -1, pixel_digest: '0000000000000000' }],
    [{ scene: '', frame: 0, pixel_digest: '0000000000000000' }],
    [{ scene: 'x', pixel_digest: '0000000000000000' }],
  ]) {
    check(`形状不对的明细复算不出来（返回 null 而不是编一个数）：${JSON.stringify(bad[0])}`,
      tableDigestHex(bad) === null);
  }
  check('复算不出来时返回 null，与"复算出来不一样"分得开',
    tableDigestHex([{ scene: 'x', frame: 0, pixel_digest: 'ffffffffffffffff' }]) !== null);

  // ---- readings 抬头：与 native 归档对一遍（常量不许漂） -----------------
  const nativeDir = join(REPO_ROOT, 'records', 'm1', 'dx12');
  const nativeReadings = existsSync(join(nativeDir, 'readings.txt'))
    ? readFileSync(join(nativeDir, 'readings.txt'), 'utf8')
    : null;
  if (nativeReadings) {
    const counts = parseReadingsCounts(nativeReadings);
    check('从归档的 readings.txt 里读出计数', counts !== null && counts.frames === 80 && counts.points === 368);
    check('抬头常量的第一行与归档一致', nativeReadings.split('\n')[0] === READINGS_HEADER);
  } else {
    check('归档的 readings.txt 在（自检需要它来钉住抬头常量）', false);
  }

  // ---- native 归档反过来钉住其它常量 ------------------------------------
  const nativeRunPath = join(nativeDir, 'run.json');
  if (existsSync(nativeRunPath)) {
    const nativeRun = JSON.parse(readFileSync(nativeRunPath, 'utf8'));
    const nativeCounts = nativeReadings ? parseReadingsCounts(nativeReadings) : null;
    // 这一轮（M1 归档）的卡身份：native 腿有名字，所以"哪块卡"由 `adapter_name` 直接给出，
    // `resolved` 是 null 且不需要宿主设备表——那正是 `identity` 的另一种合法形态。
    // 反向用例通过 `extra` 覆盖它；`'identity' in extra` 而不是 `??`：`??` 只在 `undefined`
    // 时兜底，那样"调用方压根没声明身份"这条红线就会被默认值悄悄盖住，永远测不到。
    const nativeIdentity = { adapterName: nativeRun.backends[0].adapter_name, resolved: null, why: null };
    const runCheck = (record, extra = {}) => crossCheckRun({
      record, onDisk, expectDigest: null, readingsCounts: nativeCounts,
      ...extra,
      identity: 'identity' in extra ? extra.identity : nativeIdentity,
    });
    check('表的契约版本与归档一致', nativeRun.milestone === CORPUS_TABLE_MILESTONE);
    check('记录 kind 与归档一致', nativeRun.kind === CORPUS_RECORD_KIND);
    check('记录 schema 与归档一致', nativeRun.schema === CORPUS_RECORD_SCHEMA);
    // 命名：归档里每一行的 png 路径都要能由 (scene, frame) 复算出来。
    const rows = nativeRun.backends?.[0]?.frames ?? [];
    check('归档里每一帧的路径都能由 (scene, frame) 复算',
      rows.length > 0 && rows.every((row) => row.png === framePathFor(row.scene, row.frame)));
    // 用归档当**好样本**：整条 crossCheckRun 必须放它过去。
    const onDisk = new Map(rows.map((row) => [row.png, {
      bytes: row.png_bytes,
      digest: row.png_digest,
    }]));
    const good = runCheck(nativeRun);
    check(`native 归档能通过同一套完整性检查（${good.checks.length} 条）`, good.ok === true);
    // 归档的账本摘要必须能被它自己的明细复算出来——`71ecc80cade3d73d` 这个数是
    // M2 的比对基准，它要是一句没人复核过的话，整个 M2 都悬空。
    check('归档的 frames_digest 能由它自己的逐帧明细复算出来',
      tableDigestHex(rows) === nativeRun.backends[0].frames_digest);
    check('native 归档那一轮 frame_range 与行数对得上',
      JSON.stringify(parseFrameRange(nativeRun.frame_range)) !== 'null');
    // `requested` 的形状：core 剥壳后的写法放行，wgpu 内部形态不放行。
    check('归档的 requested 是剥过壳的后端名',
      BACKEND_LABEL_RE.test(String(nativeRun.backends?.[0]?.requested ?? '')));
    for (const bad of ['Backends(DX12)', 'dx12', 'DX12 |', '', 'DX12;VULKAN']) {
      check(`requested 形状拒绝 ${JSON.stringify(bad)}`, BACKEND_LABEL_RE.test(bad) === false);
    }
    check('requested 形状放行位或起来的多后端', BACKEND_LABEL_RE.test('DX12 | VULKAN'));

    // 反向：动一个字节、少一帧、多一帧、改一个摘要，都必须红——而且必须
    // **因为那一条**红。只断言 `ok === false` 的话，一个把什么都判红的校验器
    // 也能"通过"全部反向用例。
    const reversed = (fn, keyword, extra = {}) => {
      const record = JSON.parse(JSON.stringify(nativeRun));
      fn(record);
      const result = runCheck(record, extra);
      const reasons = result.checks.filter((item) => !item.ok).map((item) => item.detail).join(' | ');
      return result.ok === false && reasons.includes(keyword);
    };
    check('反向：删掉一帧必须红（且理由是缺帧）',
      reversed((r) => { r.backends[0].frames.pop(); }, `!= ${rows.length - 1} 行`) === true);
    check('反向：改掉一个 png_bytes 必须红（且理由是字节数）',
      reversed((r) => { r.backends[0].frames[0].png_bytes += 1; }, '记录里写') === true);
    check('反向：改掉一个路径必须红（且理由是 naming）',
      reversed((r) => { r.backends[0].frames[0].png = 'frames/gradient-f999.png'; }, 'naming') === true);
    check('反向：改掉 counts 必须红（且理由是账本）',
      reversed((r) => { r.backends[0].counts.points += 1; }, '账本与明细不一致') === true);
    check('反向：把 milestone 改掉必须红（且理由是记录身份）',
      reversed((r) => { r.milestone = 'M2'; }, '记录身份不对') === true);
    check('反向：frames_digest 不是十六进制必须红（且理由是摘要形状）',
      reversed((r) => { r.backends[0].frames_digest = 'x'; }, 'frames_digest 不是 16 位十六进制') === true);
    check('反向：账本摘要与明细对不上必须红（且理由是复算）',
      reversed((r) => {
        r.backends[0].frames_digest = '0123456789abcdef';
      }, '复算的不一样') === true);
    check('反向：明细里少一个 pixel_digest 必须红（且理由是"复算不了"）',
      reversed((r) => {
        delete r.backends[0].frames[0].pixel_digest;
      }, '无从复算') === true);
    check('反向：requested 换成别的腿必须红（且理由是腿不对）',
      reversed((r) => { r.backends[0].requested = 'DX12'; }, 'requested 应为', {
        expectRequested: BROWSER_BACKEND_LABEL,
      }) === true);
    check('反向：requested 写成 wgpu 内部形态必须红（且理由是形状）',
      reversed((r) => { r.backends[0].requested = 'Backends(DX12)'; }, '不像剥过壳的 wgpu 后端名') === true);
    // 卡身份（⑦）：说完"用了哪块卡"只有两种形态，而"没人声明过"是一个必须有人喊出来的洞。
    check('反向：调用方没声明卡身份必须红（这一条不许被沉默地跳过）',
      reversed(() => {}, '不许沉默地跳过', { identity: undefined }) === true);
    check('反向：两份记录的卡名不是同一个必须红（且理由是同一个名字两个出处）',
      reversed((r) => { r.backends[0].adapter_name = 'AMD Radeon(TM) Graphics'; },
        '同一个名字两个出处') === true);
    check('反向：浏览器腿（名字是 null）而宿主设备表没解出卡 → 必须红（且理由是"说不出用了哪块卡"）',
      reversed((r) => { r.backends[0].adapter_name = null; }, '这份记录说不出', {
        identity: { adapterName: null, resolved: null, why: '（自检）宿主设备表还没送到' },
      }) === true);
    // 浏览器腿的**正路**：那一栏是 null，宿主设备表真解出了一块卡。这条必须放行——
    // 光有上面那些反向用例的话，一个"什么都判红"的校验器也能全过。
    const browserPass = runCheck(
      { ...nativeRun, backends: [{ ...nativeRun.backends[0], adapter_name: null }] },
      {
        identity: {
          adapterName: null,
          resolved: { vendor: 'nvidia', vendor_id: 4318, device_id: 10118, device: 'NVIDIA GeForce RTX 4070' },
          why: null,
        },
      },
    );
    check('浏览器腿的正路：run.json 那一栏是 null + 宿主设备表解出唯一一块卡 → 放行',
      browserPass.ok === true);
    // 盘上多一个不属于这一轮的帧：旧一轮的残留混进 records/ 就是脏数据。
    const dirtyDisk = new Map(onDisk);
    dirtyDisk.set('frames/stale-f000.png', { bytes: 1234, digest: '0000000000000000' });
    const dirty = runCheck(nativeRun, { onDisk: dirtyDisk });
    check('反向：frames/ 里有多余文件必须红（且理由是多余文件）',
      dirty.ok === false
      && dirty.checks.some((item) => !item.ok && item.detail.includes('多余文件')));
    // 空集合不许通过。
    const empty = crossCheckRun({
      record: nativeRun, onDisk: new Map(), expectDigest: null, readingsCounts: null,
      identity: nativeIdentity,
    });
    check('反向：盘上什么都没有必须红',
      empty.ok === false
      && empty.checks.some((item) => !item.ok && item.detail.includes('盘上没有这个文件')));
    // 缺 readings：两份文本必须说同一件事，缺一份就不许宣布可信。
    const noReadings = crossCheckRun({
      record: nativeRun, onDisk, expectDigest: null, readingsCounts: null,
      identity: nativeIdentity,
    });
    check('反向：缺 readings.txt 必须红（且理由是读数对不上）',
      noReadings.ok === false
      && noReadings.checks.some((item) => !item.ok && item.detail.includes('readings.txt 缺失')));
  } else {
    check('归档的 run.json 在（自检需要它当样本）', false);
  }

  // ---- adapter 校验：两种合法答法 + 每一种坏法 ---------------------------
  //
  // 样本的形状来自 wasm 侧的 `browser_adapter_json`（见 browserAdapterSample 的注释），
  // **不**复用 `records/m1/dx12/adapter.json`：那一份是 native 腿的，有名字、没有
  // `in_page`——拿它当样本，会把"浏览器腿上答不出卡名"这条最该被测的分支整个绕过去。
  const adapterSample = browserAdapterSample();
  const serialize = (record) => `${JSON.stringify(record, null, 2)}\n`;
  const edited = (fn) => {
    const copy = JSON.parse(JSON.stringify(adapterSample));
    fn(copy);
    return serialize(copy);
  };
  const adapterText = serialize(adapterSample);

  // 两种合法答法，**互斥**：答不出卡名 → 缺口声明指向宿主设备表；答得出 → 直接写名字。
  const browserVerdict = validateAdapterText(adapterText, 'm2');
  check('好 adapter 被接受（浏览器腿：答不出卡名，缺口指向 host-gpu.json）', browserVerdict.ok === true);
  check('浏览器腿这份记录被要求补上卡身份（identityNeeded）',
    browserVerdict.ok === true && browserVerdict.identityNeeded === true);
  const namedVerdict = validateAdapterText(edited((record) => {
    record.adapter.name = 'NVIDIA GeForce RTX 4070';
    record.adapter_name = 'NVIDIA GeForce RTX 4070';
    delete record.gpu_identity;
  }), 'm2');
  check('好 adapter 被接受（有名字的腿：写名字，且不挂 gpu_identity）', namedVerdict.ok === true);
  check('有名字的腿不再要求宿主设备表（缺口声明不是摆设）',
    namedVerdict.ok === true && namedVerdict.identityNeeded === false);
  check('in_page 多给的键不影响判定（多给的键在 parse_in_page 那里就丢掉了）',
    validateAdapterText(edited((record) => { record.in_page.extra = 1; }), 'm2').ok === true);

  // 坏样本：每条**连理由一起断言**。只断言 `ok === false` 的话，一个"什么都拒收"的
  // 校验器也能全过——而这一节的用处正是"每条红线各自在守着什么"。
  const badAdapters = [
    ['不是 JSON', 'not json\n', '不是 JSON'],
    ['kind 不对', edited((r) => { r.kind = 'nope'; }), '的 kind 应为'],
    ['milestone 不对', edited((r) => { r.milestone = 'M0'; }), '的 milestone 应为'],
    ['腿名不对', edited((r) => { r.backend_slug = 'm3'; }), '的 backend_slug 应为'],
    ['requested_backends 不对', edited((r) => { r.requested_backends = 'DX12'; }),
      '的 requested_backends 应为'],
    ['adapter 不是对象', edited((r) => { r.adapter = []; }), '的 adapter 不是对象'],
    ['adapter.name 不是字符串', edited((r) => { r.adapter.name = 0; }), 'adapter.name 应是字符串'],
    ['adapter_name 与 adapter.name 说不上同一件事',
      edited((r) => { r.adapter_name = 'NVIDIA GeForce RTX 4070'; }), 'adapter_name 却是'],
    ['in_page 不是对象', edited((r) => { r.in_page = 'nvidia'; }), '的 in_page 不是对象'],
    ['in_page 少一个键', edited((r) => { delete r.in_page.subgroup_max_size; }), '的 in_page 少了'],
    ['in_page.vendor 是空串', edited((r) => { r.in_page.vendor = ''; }), 'in_page.vendor 是空串'],
    ['in_page.note 是空串', edited((r) => { r.in_page.note = ''; }), 'in_page.note 是空串'],
    ['is_fallback_adapter 不是布尔', edited((r) => { r.in_page.is_fallback_adapter = 'false'; }),
      'is_fallback_adapter 应是布尔值'],
    ['subgroup_min_size 不是正整数', edited((r) => { r.in_page.subgroup_min_size = 0; }),
      'subgroup_min_size 应是正整数'],
    ['答不出卡名却没有 gpu_identity', edited((r) => { delete r.gpu_identity; }),
      '却没有 gpu_identity 声明'],
    ['gpu_identity.resolves_to 指向别的文件',
      edited((r) => { r.gpu_identity.resolves_to = 'gpu.json'; }), 'gpu_identity.resolves_to 是'],
    ['gpu_identity.reason 是空串', edited((r) => { r.gpu_identity.reason = ''; }),
      'gpu_identity 里没有 reason'],
    ['有名字却还挂着 gpu_identity',
      edited((r) => { r.adapter.name = 'X'; r.adapter_name = 'X'; }), '却还挂着 gpu_identity'],
    ['没有结尾换行', adapterText.trimEnd(), '必须是 LF 换行'],
  ];
  for (const [name, text, keyword] of badAdapters) {
    const verdict = validateAdapterText(text, 'm2');
    check(`拒绝 adapter：${name}（且理由是"${keyword}"）`,
      verdict.ok === false && String(verdict.error).includes(keyword));
  }

  // ---- 宿主设备表 → 卡身份：匹配函数的红绿 + 表本身要对得上归档 ----------
  //
  // `PAGE_VENDOR_IDS` 里那个数字不是编的：M1 那条 native 腿就在这块卡上跑，它的
  // `adapter.json` 里 `vendor`/`device` 是 wgpu 报的**十进制数字**（"4318"/"10118"）。
  // 两处对不上就说明这张表填错了——而它填错的直接后果是匹配永远落空，记录里那句
  // "用了哪块卡"于是变成一句没人能反驳的空话。
  if (nativeReadings !== null || existsSync(join(nativeDir, 'adapter.json'))) {
    const m1Adapter = JSON.parse(readFileSync(join(nativeDir, 'adapter.json'), 'utf8'));
    check(`PAGE_VENDOR_IDS 的 nvidia 与 M1 归档那块卡的 vendor 是同一个数字（${m1Adapter.adapter.vendor}）`,
      String(PAGE_VENDOR_IDS.get('nvidia')) === m1Adapter.adapter.vendor);
    check('PAGE_VENDOR_IDS 的 amd 是 PCI 里的 0x1002（实测本机 CDP 报 4098）',
      PAGE_VENDOR_IDS.get('amd') === 0x1002);
  } else {
    check('归档的 adapter.json 在（自检需要它钉住厂商 id 表）', false);
  }
  const matched = matchHostGpu({ vendor: 'nvidia' }, SELF_TEST_HOST_DEVICES);
  check('匹配：nvidia → 表里**唯一**一块 NVIDIA 的卡',
    matched.ok === true && matched.device.deviceString === 'NVIDIA GeForce RTX 4070'
    && matched.device.deviceId === 10118 && matched.reason.includes('唯一一块'));
  const matchRejects = [
    ['厂商查不到（不做相似度匹配）', { vendor: 'nvidai' }, SELF_TEST_HOST_DEVICES, 'PAGE_VENDOR_IDS'],
    ['表里没有该厂商的卡（不是同一台机器）', { vendor: 'intel' }, SELF_TEST_HOST_DEVICES, '不是同一台机器'],
    ['同厂商两块卡（二义性不许自动消解）', { vendor: 'nvidia' },
      [...SELF_TEST_HOST_DEVICES, { ...SELF_TEST_HOST_DEVICES[0], deviceId: 9999,
        deviceString: 'NVIDIA GeForce RTX 4090' }], '2 块'],
  ];
  for (const [name, inPage, devices, keyword] of matchRejects) {
    const verdict = matchHostGpu(inPage, devices);
    check(`匹配拒绝：${name}`, verdict.ok === false && String(verdict.error).includes(keyword));
  }
  check('设备表：devices 空表红（空表上对不出"哪块卡"）',
    validateHostGpuText(`${JSON.stringify({ source: HOST_GPU_SOURCE, devices: [] })}\n`).ok === false);

  // ---- 帧请求：好样本 + 每一条坏法 ---------------------------------------
  const png = Buffer.concat([PNG_SIGNATURE, Buffer.from('fake pixels')]);
  const goodFrame = { path: 'frames/gradient-f000.png', digest: fnv1a64Hex(png), bytes: String(png.length) };
  check('好帧被接受', validateFrameQuery(goodFrame, png).ok === true);
  const badFrames = [
    ['路径越界', { ...goodFrame, path: '../../etc/passwd' }],
    ['路径不在 frames/ 下', { ...goodFrame, path: 'gradient-f000.png' }],
    ['路径大写', { ...goodFrame, path: 'frames/Gradient-f000.png' }],
    ['帧号不是三位', { ...goodFrame, path: 'frames/gradient-f0.png' }],
    ['摘要不是十六进制', { ...goodFrame, digest: 'ZZZZ' }],
    ['字节数不是整数', { ...goodFrame, bytes: '9.5' }],
    ['字节数声明不符', { ...goodFrame, bytes: String(png.length + 1) }],
    ['摘要不符', { ...goodFrame, digest: '0000000000000000' }],
  ];
  for (const [name, query] of badFrames) {
    check(`拒绝帧：${name}`, validateFrameQuery(query, png).ok === false);
  }
  check('拒绝帧：签名不对', validateFrameQuery({ ...goodFrame, digest: fnv1a64Hex(Buffer.from('not a png')) },
    Buffer.from('not a png')).ok === false);
  check('摘要不是恒等函数（改了字节摘要就变）',
    validateFrameQuery(goodFrame, Buffer.concat([PNG_SIGNATURE, Buffer.from('fake pixelz')])).ok === false);

  // ---- 接线：真起服务、真 POST（纯函数测不出"接线掉了"） ------------------
  try {
    await wiringSelfTest(check);
  } catch (error) {
    check(`（端到端）接线自检自己崩了：${error.message}`, false);
  }

  const failed = cases.filter((item) => !item.ok);
  for (const item of cases) console.log(`  ${item.ok ? '✓' : '✗'} ${item.name}`);
  console.log(`\n${cases.length - failed.length}/${cases.length} 项通过`);
  return failed.length === 0 ? 0 : 1;
}

// ---------------------------------------------------------------------------
// 入口
// ---------------------------------------------------------------------------

function parseArgs(argv) {
  const options = {
    port: 8788,
    outDir: join(REPO_ROOT, 'records', 'm2', 'browser'),
    legSlug: 'm2',
    nativeDir: null,
    expect: null,
    selfTest: false,
    help: false,
  };
  const args = [...argv];
  while (args.length > 0) {
    const arg = args.shift();
    switch (arg) {
      case '--port': {
        const value = args.shift();
        if (value === undefined || !/^\d+$/.test(value)) {
          return { error: `--port 后面要跟一个端口号（0 表示让系统挑），收到 ${JSON.stringify(value)}` };
        }
        options.port = Number(value);
        break;
      }
      case '--out': {
        const value = args.shift();
        if (value === undefined || value.trim() === '') {
          return { error: '--out 后面要跟一个目录' };
        }
        options.outDir = resolve(REPO_ROOT, value);
        break;
      }
      case '--leg': {
        const value = args.shift();
        if (value === undefined || !/^[a-z0-9-]{1,64}$/.test(value) || value.startsWith('-') || value.endsWith('-')) {
          return { error: `--leg 只认 [a-z0-9-] 且长度 1..=64（它是目录名），收到 ${JSON.stringify(value)}` };
        }
        options.legSlug = value;
        break;
      }
      case '--native': {
        const value = args.shift();
        if (value === undefined || value.trim() === '') return { error: '--native 后面要跟一个目录' };
        options.nativeDir = value.trim().replace(/\\/g, '/');
        break;
      }
      case '--expect': {
        const value = args.shift();
        if (value === undefined || !DIGEST_RE.test(value)) {
          return { error: `--expect 要 16 位十六进制摘要，收到 ${JSON.stringify(value)}` };
        }
        options.expect = value;
        break;
      }
      case '--self-test':
        options.selfTest = true;
        break;
      case '-h':
      case '--help':
        options.help = true;
        break;
      default:
        return { error: `不认识的参数：${arg}\n\n${USAGE}` };
    }
  }
  if (options.expect && options.nativeDir) {
    return { error: '--expect 与 --native 二选一：两个都给的话，"期望值到底是谁的"就没有唯一答案' };
  }
  return options;
}

/** 期望摘要：显式给的，或从 native 归档的 run.json 里读的。 */
function resolveExpectDigest(options) {
  if (options.expect) return options.expect;
  if (!options.nativeDir) return null;
  const runPath = join(REPO_ROOT, options.nativeDir, 'run.json');
  if (!existsSync(runPath)) {
    console.error(`✗ --native ${options.nativeDir} 里没有 run.json，拿不到期望摘要`);
    return undefined;
  }
  const record = JSON.parse(readFileSync(runPath, 'utf8'));
  const digest = record?.backends?.[0]?.frames_digest;
  if (!DIGEST_RE.test(String(digest ?? ''))) {
    console.error(`✗ ${options.nativeDir}/run.json 里的 frames_digest 形状不对：${JSON.stringify(digest)}`);
    return undefined;
  }
  return digest;
}

async function main(argv) {
  const options = parseArgs(argv);
  if (options.error) {
    console.error(`✗ ${options.error}`);
    return 2;
  }
  if (options.help) {
    console.log(USAGE);
    return 0;
  }
  if (options.selfTest) return selfTest();

  if (!existsSync(join(WWW_ROOT, 'corpus.html'))) {
    console.error(`✗ ${join(WWW_ROOT, 'corpus.html')} 不存在`);
    return 2;
  }
  if (!existsSync(join(WWW_ROOT, 'pkg', 'dhampir_wasm.js'))) {
    console.error('✗ 还没打包 wasm。先跑：\n'
      + '    wasm-pack build crates/dhampir-wasm --target web --out-dir www/pkg --dev');
    return 2;
  }

  const expectDigest = resolveExpectDigest(options);
  if (expectDigest === undefined) return 2;
  startServer({ ...options, expectDigest });
  return 0;
}

// 只设退出码，不调 `process.exit()`：后者会把还没 flush 的 stdout 截断
// （浏览器腿的横幅与判定都在 stdout 上，正是最不该丢的那几行）。
main(process.argv.slice(2))
  .then((code) => { process.exitCode = code; })
  .catch((error) => {
    console.error(`✗ 自己崩了：${error?.stack ?? error?.message ?? error}`);
    process.exitCode = 2;
  });

export { REPO_ROOT, WWW_ROOT, CORPUS_TABLE_MILESTONE, startServer };
