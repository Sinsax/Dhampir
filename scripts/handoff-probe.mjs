#!/usr/bin/env node
// 交接单自测探针：把 `plan/dhampir-base-handoff.md` 的 **D 组 / B 组**那几条**在底座侧真的量一遍**。
//
//     node scripts/handoff-probe.mjs              跑全部（color-mask / fit / upscale）
//     node scripts/handoff-probe.mjs color-mask   只跑 D1：调整层的 ColorMask 到底画上去了吗
//     node scripts/handoff-probe.mjs fit          只跑 B1：换画布比例时是 contain 还是 cover
//     node scripts/handoff-probe.mjs upscale      只跑 B2：放大走的是双线性还是最近邻
//     node scripts/handoff-probe.mjs --self-test  只跑自检（不碰 GPU / ffmpeg / 不写盘）
//
// # 为什么要有它（而不是让结论活在文档里）
//
// 交接单那几条的症状都是**"画面看着正常、其实没生效"**：`overlay` 不出图、放大偏软、
// 竖屏留黑边。这类结论**不能靠读代码下判断**（本仓已经栽过 5 次"写好了没接上"），
// 也不能靠"跟上一版图比" —— 那只证明"变了"，证明不了"对"。
// 所以这里的判据分两层：
//
//   1. **解析值判据**：把 `color_mask.wgsl` 的数学抄成可算的期望值，逐像素比（不是比哈希）；
//   2. **差分判据**：窗口**内**必须不同、窗口**外**必须逐字节相同。
//
// 读数与 `plan/dhampir-base-handoff.md` §7 是同一组命令、同一份素材，**可照抄重跑**。
//
// # 前置
//
// * `--cli <路径>`（默认 `target/debug/dhampir.exe`）
// * PATH 上的 `ffmpeg` / `ffprobe`
// * 真 GPU（会真的出帧）
// * 测试素材：`node scripts/make-test-media.mjs`（缺了会明确报出来，不退化成"没量"）
//
// 退出码：**0** 判据全过 / **1** 有判据不符 / **2** 用法错或前置缺失。

import { existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { runToolSync } from './spawn-tool.mjs';
import { tryRemove } from './safe-remove.mjs';

const REPO = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const WORK = join(REPO, 'target', 'handoff-probe');
const MEDIA_DIR = join(REPO, 'target', 's3');
const CLIP = 'proxy1080p.mp4';

// ---------------------------------------------------------------- 参数
const argv = process.argv.slice(2);
function arg(name, fallback) {
  const i = argv.indexOf(name);
  return i >= 0 && argv[i + 1] && !argv[i + 1].startsWith('--') ? argv[i + 1] : fallback;
}
const has = (n) => argv.includes(n);
const USAGE = `用法：node scripts/handoff-probe.mjs [color-mask|fit|upscale|shadow] [--cli <路径>] [--self-test]

  （不给子命令 = 四支都跑）
  --cli <路径>    dhampir 可执行（默认 target/debug/dhampir.exe）
  --self-test     只跑自检，不碰 GPU / ffmpeg / 不写盘

  shadow 那一支：契约里还没有 shadow_* 字段时**跳过**（跳过 ≠ 通过）
`;
if (has('--help') || has('-h')) { console.log(USAGE); process.exit(0); }

// ---------------------------------------------------------------- 纯函数（可自检）
/** 一帧工程：一条视频层（`0..end`）+ 可选的调整层（同一条特效，窗口 30..126）。 */
export function buildDoc({ effects = null, end = 480, size = 1920, hints = null } = {}) {
  return {
    project_schema: 1,
    generator: { app: 'dhampir', version: '0.0.1' },
    meta: { title: 'handoff-probe', created_at: null, modified_at: null },
    assets: [{
      id: 'clip', kind: 'video', name: 'clip', uri: CLIP, frame_count: end,
      timebase: { num: 60, den: 1 }, width: size, height: Math.round(size * 9 / 16),
      content_hash: null, tags: {}, note: '',
    }],
    timeline: {
      schema: 3,
      timebase: { num: 60, den: 1 },
      markers: [],
      tracks: [
        { id: 'v1', kind: 'video', layers: [{ id: 'main', start: 0, end,
          source: { asset_id: 'clip', source_in: 0 }, effects: [] }] },
        ...(effects ? [{ id: 'adj', kind: 'video', layers: [{ id: 'adj', start: 30, end: 126, effects }] }] : []),
      ],
    },
    view: { playhead: 0, selection: null, zoom: 1 },
    render_hints: { width: hints?.width ?? 1920, height: hints?.height ?? 1080, format: 'mp4' },
    extensions: {},
  };
}

const OVERLAY = (amount, angle, a, b) => [{
  kind: 'overlay',
  params: {
    amount,
    r: a[0], g: a[1], b: a[2],
    r2: b[0], g2: b[1], b2: b[2],
    shape: 1, angle,
  },
}];

export const VIGNETTE = (amount, radius, softness) =>
  [{ kind: 'vignette', params: { amount, radius, softness } }];

/**
 * `color_mask.wgsl` 第 4 段（overlay）的数学，逐行抄成**可算的期望值**。
 *
 * * `base` 是 0..255 的读回值，而契约里的 `color_a/color_b` 是 **0..1 浮点** ⇒ 必须 ×255；
 * * 分片着色器里 `position.xy` 是**像素中心**（`x+0.5`）⇒ 差半像素会让 45° 那条对角判据落在边界上。
 */
export function overlayExpected(base, x, y, w, h, shape, angleDeg, amount, colorA, colorB) {
  const u = (x + 0.5) / w - 0.5;
  const v = (y + 0.5) / h - 0.5;
  // `shape=0`（纯色）⇒ `grad_t = 0` ⇒ 取 `color_a`（`r/g/b`）。
  // **2026-10-01 修过**：以前是 `is_solid + …`，纯色会取 `color_b`（`r2/g2/b2`）。
  const isLinear = shape >= 0.5 && shape < 1.5 ? 1 : 0;
  const isRadial = shape >= 1.5 ? 1 : 0;
  const angle = (angleDeg * Math.PI) / 180;
  const linearT = Math.min(1, Math.max(0, u * Math.cos(angle) + v * Math.sin(angle) + 0.5));
  const radialT = Math.min(1, Math.max(0, Math.hypot(u, v) * 2));
  const gradT = Math.min(1, Math.max(0, isLinear * linearT + isRadial * radialT));
  return [0, 1, 2].map((c) => {
    const color = (colorA[c] + (colorB[c] - colorA[c]) * gradT) * 255;
    return base[c] + (color - base[c]) * amount;
  });
}

/** `color_mask.wgsl` 第 2 段（暗角）的数学。 */
export function vignetteExpected(base, x, y, w, h, amount, radius, softness) {
  const u = (x + 0.5) / w - 0.5;
  const v = (y + 0.5) / h - 0.5;
  const dist = Math.hypot(u, v) * 1.41421356;
  const edge = Math.min(1, Math.max(0, (dist - radius) / Math.max(softness, 1e-4)));
  const factor = 1 - edge * amount;
  return base.map((c) => c * factor);
}

// ---------------------------------------------------------------- 小工具
function sh(command, args, options = {}) {
  const r = runToolSync(command, args, options);
  if (r.error || r.status === null) {
    throw new Error(`起不了 ${command}：${r.error ? r.error.message : 'status=null'}`
      + (command === 'ffmpeg' ? '（先装 ffmpeg）' : '（--cli 指到了吗？）'));
  }
  return r;
}

function cliPath() {
  return resolve(arg('--cli', join(REPO, 'target', 'debug', process.platform === 'win32' ? 'dhampir.exe' : 'dhampir')));
}

/** 出帧，返回 PNG 路径。 */
function render(cli, docPath, frameNo, outDir, extra = []) {
  mkdirSync(outDir, { recursive: true });
  const r = sh(cli, ['frame', '--project', docPath, '--frame', String(frameNo),
    '--out', outDir, '--asset-root', MEDIA_DIR, ...extra]);
  if (r.status !== 0) throw new Error(`frame 失败(${r.status})：${String(r.stderr).trim().slice(0, 300)}`);
  const name = `frame-${String(frameNo).padStart(4, '0')}.png`;
  const p = join(outDir, name);
  if (!existsSync(p)) throw new Error(`frame 报了成功却没落盘：${p}`);
  return p;
}

const sha = (p) => createHash('sha256').update(readFileSync(p)).digest('hex').slice(0, 16);

/** 取原始 RGBA 像素。 */
function rgba(path, w, h) {
  const r = sh('ffmpeg', ['-v', 'error', '-i', path, '-f', 'rawvideo', '-pix_fmt', 'rgba', '-'],
    { encoding: 'buffer', maxBuffer: Math.max(1 << 26, w * h * 4 + (1 << 20)) });
  if (r.status !== 0) throw new Error(String(r.stderr).slice(0, 200));
  return r.stdout;
}

const px = (buf, w, x, y) => {
  const i = (y * w + x) * 4;
  return [buf[i], buf[i + 1], buf[i + 2]];
};

/** 逐像素对解析值，返回最大偏差。 */
function maxDeviation(got, base, w, h, expected) {
  let worst = 0;
  for (let y = 0; y < h; y += 1) {
    for (let x = 0; x < w; x += 1) {
      const want = expected(px(base, w, x, y), x, y);
      const have = px(got, w, x, y);
      for (let c = 0; c < 3; c += 1) worst = Math.max(worst, Math.abs(have[c] - want[c]));
    }
  }
  return worst;
}

// ---------------------------------------------------------------- 判据收集

const results = [];
const check = (ok, label, detail) => {
  results.push({ ok, label, detail });
  console.log(`  ${ok ? '✓' : '✗'} ${label}${detail ? `　${detail}` : ''}`);
};

function probeColorMask(cli) {
  const W = 64, H = 64; // 小尺寸：逐像素对解析值，4096 个点够密也够快
  const dir = join(WORK, 'color-mask');
  const plainDoc = join(dir, 'plain.doc.json');
  const overlay0Doc = join(dir, 'overlay-0.doc.json');
  const overlay45Doc = join(dir, 'overlay-45.doc.json');
  const vigDoc = join(dir, 'vignette.doc.json');
  mkdirSync(dir, { recursive: true });

  const A = [0.0784, 0.0392, 0.1569];   // 交接单 §1 的那两个颜色（0..1）
  const B = [0.1569, 0.0784, 0.2353];
  const AMOUNT = 0.16;
  writeFileSync(plainDoc, JSON.stringify(buildDoc({}, ), null, 2));
  writeFileSync(overlay0Doc, JSON.stringify(buildDoc({ effects: OVERLAY(AMOUNT, 0, A, B) }), null, 2));
  writeFileSync(overlay45Doc, JSON.stringify(buildDoc({ effects: OVERLAY(AMOUNT, 45, A, B) }), null, 2));
  writeFileSync(vigDoc, JSON.stringify(buildDoc({ effects: VIGNETTE(1, 0.2, 0.3) }), null, 2));

  const inFrame = 60, outFrame = 10; // 窗口是 30..126
  const base60 = render(cli, plainDoc, inFrame, join(dir, 'plain-60'), ['--width', String(W), '--height', String(H)]);
  const base10 = render(cli, plainDoc, outFrame, join(dir, 'plain-10'), ['--width', String(W), '--height', String(H)]);
  const basePx = rgba(base60, W, H);

  // ---- overlay：窗口内必须变、窗口外必须逐字节不变
  for (const [label, doc, angle] of [['angle=0', overlay0Doc, 0], ['angle=45', overlay45Doc, 45]]) {
    const with60 = render(cli, doc, inFrame, join(dir, `${label}-60`), ['--width', String(W), '--height', String(H)]);
    const with10 = render(cli, doc, outFrame, join(dir, `${label}-10`), ['--width', String(W), '--height', String(H)]);
    check(sha(with60) !== sha(base60), `overlay ${label}：窗口内的帧确实变了`,
      `base=${sha(base60)} with=${sha(with60)}`);
    check(sha(with10) === sha(base10), `overlay ${label}：窗口外的帧逐字节不变`, `sha=${sha(with10)}`);

    const got = rgba(with60, W, H);
    const worst = maxDeviation(got, basePx, W, H,
      (b, x, y) => overlayExpected(b, x, y, W, H, 1, angle, AMOUNT, A, B));
    check(worst <= 2, `overlay ${label}：逐像素对解析值（容差 2）`, `最大偏差 ${worst.toFixed(1)}/255`);

    // 方向：这是**契约**，不是公式的副产品
    const mid = H >> 1;
    const corner = angle === 0
      ? [[1, mid, '左端', A], [W - 2, mid, '右端', B]]
      : [[1, 1, '左上', A], [W - 2, H - 2, '右下', B]];
    let ok = true;
    for (const [x, y, name, want] of corner) {
      const base = px(basePx, W, x, y);
      for (let c = 0; c < 3; c += 1) {
        const expect = base[c] + (want[c] * 255 - base[c]) * AMOUNT;
        if (Math.abs(px(got, W, x, y)[c] - expect) > 2) ok = false;
      }
      if (!ok) { check(false, `overlay ${label}：${name}不是 ${want === A ? 'color_a' : 'color_b'} 那一端`); break; }
    }
    if (ok) check(true, `overlay ${label}：${angle === 0 ? '左=color_a、右=color_b' : '左上=color_a、右下=color_b'}`);
  }

  // ---- vignette：中心不动、角落压暗（半径取小，两半才都可判）
  const vig60 = render(cli, vigDoc, inFrame, join(dir, 'vig-60'), ['--width', String(W), '--height', String(H)]);
  const vigPx = rgba(vig60, W, H);
  check(sha(vig60) !== sha(base60), 'vignette：窗口内的帧确实变了');
  const worstVig = maxDeviation(vigPx, basePx, W, H,
    (b, x, y) => vignetteExpected(b, x, y, W, H, 1, 0.2, 0.3));
  check(worstVig <= 2, 'vignette：逐像素对解析值（容差 2）', `最大偏差 ${worstVig.toFixed(1)}/255`);
  const center = px(vigPx, W, W >> 1, H >> 1);
  const cbase = px(basePx, W, W >> 1, H >> 1);
  check(center.every((v, i) => Math.abs(v - cbase[i]) <= 2), 'vignette：中心不被压暗');
  const corner = px(vigPx, W, 0, 0);
  check(corner[0] + corner[1] + corner[2] < 3, 'vignette：角落被压到接近全黑', `角落=${corner.join(',')}`);
}

function probeFit(cli) {
  const dir = join(WORK, 'fit');
  const portrait = { width: 1080, height: 1920 };
  const docPath = join(dir, 'plain.doc.json');
  mkdirSync(dir, { recursive: true });
  writeFileSync(docPath, JSON.stringify(buildDoc(), null, 2));

  const containPng = render(cli, docPath, 60, join(dir, 'contain'),
    ['--width', String(portrait.width), '--height', String(portrait.height)]);
  const buf = rgba(containPng, portrait.width, portrait.height);
  let transparent = 0;
  for (let i = 3; i < buf.length; i += 4) if (buf[i] === 0) transparent += 1;
  const ratio = transparent / (portrait.width * portrait.height);
  check(transparent > 0, 'B1：源比例 ≠ 画布比例时是 contain（有未覆盖区域）',
    `透明 ${(ratio * 100).toFixed(2)}%`);

  // cover：用现成的 transform.scale 表达（源按画布宽高比放大）
  const cover = buildDoc();
  const scale = portrait.width / portrait.height > 16 / 9 ? portrait.width / 1920 : portrait.height / 1080;
  cover.timeline.tracks[0].layers[0].transform = { x: 0, y: 0, scale, rotation: 0 };
  const coverDoc = join(dir, 'cover.doc.json');
  writeFileSync(coverDoc, JSON.stringify(cover, null, 2));
  const coverPng = render(cli, coverDoc, 60, join(dir, 'cover'),
    ['--width', String(portrait.width), '--height', String(portrait.height)]);
  const cb = rgba(coverPng, portrait.width, portrait.height);
  let coverTransparent = 0;
  for (let i = 3; i < cb.length; i += 4) if (cb[i] === 0) coverTransparent += 1;
  check(coverTransparent === 0, `B1：cover 可用现成 transform.scale 表达（scale=${scale.toFixed(4)}）`,
    `透明 ${coverTransparent} 个像素`);
}

function probeUpscale(cli) {
  const dir = join(WORK, 'upscale');
  mkdirSync(dir, { recursive: true });

  // 1 像素棋盘格：最近邻只会给出两种原值，双线性必然产生中间值
  const checker = join(MEDIA_DIR, 'checker1.png');
  sh('ffmpeg', ['-v', 'error', '-y', '-f', 'lavfi', '-i', 'color=c=black:s=1920x1080', '-vf',
    "format=gray,geq=lum='if(mod(X+Y,2),235,16)'", '-frames:v', '1', checker]);

  const doc = buildDoc();
  doc.assets[0] = { ...doc.assets[0], kind: 'image', uri: 'checker1.png', frame_count: 1 };
  doc.timeline.tracks[0].layers[0].end = 1;
  const docPath = join(dir, 'checker.doc.json');
  writeFileSync(docPath, JSON.stringify(doc, null, 2));

  const UP_W = 3413, UP_H = 1920;
  const native = render(cli, docPath, 0, join(dir, 'native'));
  const up = render(cli, docPath, 0, join(dir, 'up'), ['--width', String(UP_W), '--height', String(UP_H)]);
  const refs = {};
  for (const flags of ['bilinear', 'neighbor']) {
    refs[flags] = join(dir, `ref-${flags}.png`);
    sh('ffmpeg', ['-v', 'error', '-y', '-i', native, '-vf',
      `scale=${UP_W}:${UP_H}:flags=${flags}`, refs[flags]]);
  }

  const intermediate = (p) => {
    const b = rgba(p, UP_W, UP_H);
    let mid = 0, total = 0;
    for (let y = 0; y < UP_H; y += 2) {
      for (let x = 0; x < UP_W; x += 2) {
        total += 1;
        const lum = b[(y * UP_W + x) * 4];
        if (Math.abs(lum - 16) > 4 && Math.abs(lum - 235) > 4) mid += 1;
      }
    }
    return mid / total;
  };
  const mine = intermediate(up);
  const bil = intermediate(refs.bilinear);
  const nei = intermediate(refs.neighbor);
  check(Math.abs(mine - bil) < Math.abs(mine - nei),
    'B2：放大走的是**双线性**（不是最近邻搬用）',
    `中间值 底座 ${(mine * 100).toFixed(1)}% / bilinear ${(bil * 100).toFixed(1)}% / neighbor ${(nei * 100).toFixed(1)}%`);

  // 高频能量：**只打印，不当判据**。
  //
  // 实测（2026-10-01）：同一个实现、同一个放大倍数，换内容就换结论 ——
  // testsrc2（照片类）上比 ffmpeg 双线性 **高 2.59×**，1 像素棋盘上 **低到 0.003×**。
  // 所以它是**内容相关的诊断量**，拿它判"软/锐"会得出自相矛盾的结论。
  // 能站住的判据只有上面那条（中间值占比 ⇒ 双线性 vs 最近邻）。
  const laplacian = (p) => {
    const b = rgba(p, UP_W, UP_H);
    const lum = (x, y) => {
      const i = (y * UP_W + x) * 4;
      return 0.299 * b[i] + 0.587 * b[i + 1] + 0.114 * b[i + 2];
    };
    let s = 0, s2 = 0, n = 0;
    for (let y = 1; y < UP_H - 1; y += 1) {
      for (let x = 1; x < UP_W - 1; x += 1) {
        const v = lum(x - 1, y) + lum(x + 1, y) + lum(x, y - 1) + lum(x, y + 1) - 4 * lum(x, y);
        s += v; s2 += v * v; n += 1;
      }
    }
    return s2 / n - (s / n) ** 2;
  };
  const lapMine = laplacian(up);
  const lapRef = laplacian(refs.bilinear);
  console.log(`  · 诊断（**不当判据**，内容相关）：拉普拉斯方差 底座 ${lapMine.toFixed(3)} / bilinear ${lapRef.toFixed(3)}`
    + ` = ${(lapMine / lapRef).toFixed(3)}×`);
}

// ---------------------------------------------------------------- shadow（B5：文字阴影）
//
// 这是**独立**判据：不复用实现方的用例，自己在像素上验"阴影真的画了、落在偏移处、是模糊的"。
// 契约里还没有那几个字段时**明确跳过** —— 跳过不是通过，也不是失败（免得给实现方一个假红）。
function probeShadow(cli) {
  const schemaPath = join(REPO, 'schema', 'timeline-v4.schema.json');
  const hasFields = readFileSync(schemaPath, 'utf8').includes('shadow_color');
  if (!hasFields) {
    console.log('  · 契约里还没有 `shadow_*` 字段（设计见 plan/text-shadow-design.md）—— **跳过**，这不等于通过');
    return;
  }

  // 字体：本仓不猜系统字体，这里按候选列表挑一个**存在**的；一个都没有就跳过（不自作主张）。
  const FONTS = ['C:/Windows/Fonts/msyh.ttc', 'C:/Windows/Fonts/simhei.ttf',
    '/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf'];
  const font = FONTS.find((f) => existsSync(f));
  if (!font) { console.log('  · 找不到可用字体（--font-file 的候选都不在）—— **跳过**'); return; }

  const dir = join(WORK, 'shadow');
  mkdirSync(dir, { recursive: true });
  // 字幕素材在 fixtures/，视频在 target/s3 —— 一个 --asset-root 盖不住两个，先把素材铺过去。
  for (const f of ['sample-subtitle.srt', 'sample-subtitle.ass']) {
    const src = join(REPO, 'fixtures', f);
    if (existsSync(src)) writeFileSync(join(MEDIA_DIR, f), readFileSync(src));
  }

  const FRAME = 60;
  const base = JSON.parse(readFileSync(join(REPO, 'fixtures', 'sample-subtitle.doc.json'), 'utf8'));

  const subTrack = (doc) => doc.timeline.tracks.find((t) => t.kind === 'subtitle');
  const plain = JSON.parse(JSON.stringify(base));
  // ① 字段在场但 `shadow_color: null` ⇒ **必须与老工程逐字节相同**（不画阴影走老路）
  const none = JSON.parse(JSON.stringify(base));
  subTrack(none).subtitle = { shadow_color: null, shadow_dx_px: 8, shadow_dy_px: 12, shadow_blur_ratio: 0.02 };
  // ② 真的画阴影
  const DX = 8, DY = 12;
  const shaded = JSON.parse(JSON.stringify(base));
  subTrack(shaded).subtitle = { shadow_color: [0, 0, 0, 160], shadow_dx_px: DX, shadow_dy_px: DY, shadow_blur_ratio: 0.02 };
  // ③ 把字幕/弹幕轨整条拿掉 ⇒ 这一帧的"文字墨迹"参照
  const noText = JSON.parse(JSON.stringify(base));
  noText.timeline.tracks = noText.timeline.tracks.filter((t) => t.kind !== 'subtitle' && t.kind !== 'danmaku');

  const paths = {};
  for (const [name, doc] of [['plain', plain], ['none', none], ['shaded', shaded], ['notext', noText]]) {
    const p = join(dir, `${name}.doc.json`);
    writeFileSync(p, JSON.stringify(doc, null, 2));
    paths[name] = render(cli, p, FRAME, join(dir, name), ['--font-file', font]);
  }

  // ⚠️ **尺寸要问产物，不能写死**：夹具的 `render_hints` 是 640×360。
  // 第一版这里写死 1920×1080，于是循环越界读到 `NaN`：
  // 差异判据把整幅图都算成"变了"（假绿）、方向判据也靠 NaN 通过。
  // 这正是本仓最怕的那类"看起来有据"——判据自己必须先站得住。
  const pngSize = (p) => {
    const r = sh('ffprobe', ['-v', 'error', '-select_streams', 'v:0',
      '-show_entries', 'stream=width,height', '-of', 'default=nw=1', p]);
    const kv = Object.fromEntries(String(r.stdout).trim().split('\n')
      .filter(Boolean).map((line) => line.split('=')));
    return [Number(kv.width), Number(kv.height)];
  };
  const [W, H] = pngSize(paths.plain);
  const buf = {};
  for (const [name, p] of Object.entries(paths)) {
    buf[name] = rgba(p, W, H);
    if (buf[name].length !== W * H * 4) {
      throw new Error(`${name} 的像素字节数不对：${buf[name].length} != ${W * H * 4}（尺寸 ${W}x${H}）`);
    }
  }
  // ⚠️ **比的是"叠到中性灰底之后的亮度"，不是原始 RGB 亮度**。
  // 第二版这里只取 RGB 亮度，于是**黑色阴影完全看不见**：它的 RGB 恒为 0，
  // 只在 alpha 上留薄纱（实测 `0→69`、`104→107`）。出片那一帧是 RGBA，
  // 阴影就是"加了一层黑 alpha" —— 判据必须同时感受 RGB 与 alpha，否则又是假结论。
  const lum = (b, x, y) => {
    const i = (y * W + x) * 4;
    const a = b[i + 3] / 255;
    const rgb = 0.299 * b[i] + 0.587 * b[i + 1] + 0.114 * b[i + 2];
    return rgb * a + 128 * (1 - a);
  };

  check(sha(paths.none) === sha(paths.plain),
    '阴影字段在场但不给颜色时，与老工程**逐字节相同**', `sha=${sha(paths.plain)}`);
  check(sha(paths.shaded) !== sha(paths.plain), '给了 shadow_color 之后画面确实变了',
    `plain=${sha(paths.plain)} shaded=${sha(paths.shaded)}`);

  // 墨迹与"差异"两类像素
  let inkMaxX = -1, inkMaxY = -1, inkMinX = W, inkMinY = H;
  let diffMaxX = -1, diffMaxY = -1, diffCount = 0;
  let outsideCount = 0, outsideMin = 999, outsideMax = -1;
  const outside = [];
  const ink = new Uint8Array(W * H);
  for (let y = 0; y < H; y += 1) {
    for (let x = 0; x < W; x += 1) {
      if (Math.abs(lum(buf.plain, x, y) - lum(buf.notext, x, y)) > 6) {
        ink[y * W + x] = 1;
        inkMaxX = Math.max(inkMaxX, x); inkMaxY = Math.max(inkMaxY, y);
        inkMinX = Math.min(inkMinX, x); inkMinY = Math.min(inkMinY, y);
      }
    }
  }
  for (let y = 0; y < H; y += 1) {
    for (let x = 0; x < W; x += 1) {
      const d = Math.abs(lum(buf.shaded, x, y) - lum(buf.plain, x, y));
      if (d <= 3) continue;
      diffCount += 1;
      diffMaxX = Math.max(diffMaxX, x); diffMaxY = Math.max(diffMaxY, y);
      // "文字墨迹之外"的那部分就是阴影自己 —— 它必须是**渐变**的，不是一块硬台阶
      const inInk = ink[y * W + x] === 1;
      if (!inInk) {
        outsideCount += 1;
        const l = lum(buf.plain, x, y) - lum(buf.shaded, x, y); // 变暗了多少
        outside.push(l);
        outsideMin = Math.min(outsideMin, l); outsideMax = Math.max(outsideMax, l);
      }
    }
  }

  check(inkMaxX >= 0, '这一帧确实有文字墨迹（参照不空）', `墨迹 bbox=(${inkMinX},${inkMinY})-(${inkMaxX},${inkMaxY})`);
  check(diffCount > 0, '阴影真的画出来了（与不画阴影的同一帧有差异）', `差异像素 ${diffCount}`);
  check(diffMaxX > inkMaxX + DX / 2 && diffMaxY > inkMaxY + DY / 2,
    `阴影落在偏移方向（dx=${DX}, dy=${DY}）`,
    `差异最右/最下 = ${diffMaxX}/${diffMaxY}，墨迹最右/最下 = ${inkMaxX}/${inkMaxY}`);

  // 模糊：墨迹之外的阴影像素应当**变暗程度不一**（硬阴影只有一档）
  if (outsideCount > 0) {
    const spread = outsideMax - outsideMin;
    const distinct = new Set(outside.slice(0, 200000).map((v) => Math.round(v))).size;
    check(spread >= 20 && distinct >= 6,
      '阴影是**模糊**的（墨迹之外有渐变，不是一档硬边）',
      `变暗幅度 ${outsideMin.toFixed(0)}~${outsideMax.toFixed(0)}，档数 ${distinct}`);
  } else {
    check(false, '阴影落在偏移处（墨迹之外应当能看到阴影）', '墨迹之外一处差异都没有 —— 阴影没落出来');
  }
}

// ---------------------------------------------------------------- 自检
function selfTest() {
  let n = 0;
  const bad = [];
  const expect = (ok, label) => { n += 1; if (!ok) bad.push(label); };

  // 解析模型：拿定义直接算几个点，看看是不是那一回事
  const black = [0, 0, 0];
  const white = [255, 255, 255];
  // `shape=0`（纯色）⇒ 取 `color_a`。这条在 2026-10-01 **改的是实现**（以前取 `color_b`）：
  // 证据是下游转译器的纯色把 `r2/g2/b2` 写成与 `r/g/b` 相同（`solidEffect`），所以修正零影响。
  const solidA = overlayExpected(black, 0, 0, 64, 64, 0, 0, 1, [1, 0, 0], [0, 0, 1]);
  expect(Math.abs(solidA[0] - 255) < 1e-6 && solidA[2] < 1e-6, 'shape=0（纯色）应当取 color_a');
  // angle=0 ⇒ 左边是 a、右边是 b
  const left = overlayExpected(black, 0, 32, 64, 64, 1, 0, 1, [1, 0, 0], [0, 0, 1]);
  const right = overlayExpected(black, 63, 32, 64, 64, 1, 0, 1, [1, 0, 0], [0, 0, 1]);
  expect(left[0] > right[0] && right[2] > left[2], 'angle=0 应当是"左 a 右 b"');
  // angle=45 ⇒ 左上更接近 a、右下更接近 b
  const tl = overlayExpected(black, 0, 0, 64, 64, 1, 45, 1, [1, 0, 0], [0, 0, 1]);
  const br = overlayExpected(black, 63, 63, 64, 64, 1, 45, 1, [1, 0, 0], [0, 0, 1]);
  expect(tl[0] > br[0] && br[2] > tl[2], 'angle=45 应当是"左上 a、右下 b"');
  // amount=0 ⇒ 恒等（这是"既有工程逐字节不变"的数学根据）
  const noop = overlayExpected(white, 10, 10, 64, 64, 1, 30, 0, [1, 0, 0], [0, 0, 1]);
  expect(noop.every((v, i) => Math.abs(v - white[i]) < 1e-6), 'amount=0 必须恒等');
  // 暗角半径大于归一化对角距离（0.707）时不该有任何效果 —— 这条防的是"拿 radius=1 当反例"
  const far = vignetteExpected(white, 0, 0, 64, 64, 1, 1, 0.5);
  expect(far.every((v, i) => Math.abs(v - white[i]) < 1e-6), 'radius=1.0 时按定义不该有暗角');
  const near = vignetteExpected(white, 0, 0, 64, 64, 1, 0.2, 0.3);
  expect(near[0] < 1.0, 'radius=0.2 时角落应当被压到接近 0');

  // 工程壳：缺 timeline.schema / meta 会被载入器拒（交接单原文就栽在这）
  const d = buildDoc({ effects: OVERLAY(0.16, 0, [0.1, 0.1, 0.1], [0.2, 0.2, 0.2]) });
  expect(d.timeline.schema === 3, '工程壳必须带 timeline.schema');
  expect(d.meta !== undefined && d.assets[0].frame_count === 480, '工程壳必须带 meta 与 frame_count');
  expect(d.timeline.tracks.length === 2 && d.timeline.tracks[1].layers[0].source === undefined,
    '调整层必须是"没有 source、只有 effects"');
  expect(buildDoc().timeline.tracks.length === 1, '不给特效时不该有调整层');

  if (bad.length > 0) {
    console.error('✗ 自检失败（先修脚本，别信它的结论）：');
    for (const b of bad) console.error(`    - ${b}`);
    process.exit(2);
  }
  console.log(`✓ handoff-probe 自检：${n} 项全绿`);
}

// ---------------------------------------------------------------- main
if (has('--self-test')) { selfTest(); process.exit(0); }

const known = new Set(['color-mask', 'fit', 'upscale', 'shadow', '--cli', '--self-test', '--help', '-h']);
for (let i = 0; i < argv.length; i += 1) {
  if (!known.has(argv[i])) { console.error(`✗ 不认识的参数：${argv[i]}\n\n${USAGE}`); process.exit(2); }
  if (argv[i] === '--cli') i += 1;
}

const cli = cliPath();
for (const [label, path, hint] of [
  ['dhampir CLI', cli, '先 `cargo build -p dhampir-worker --bin dhampir`，或用 --cli 指一个'],
  ['测试素材', join(MEDIA_DIR, CLIP), '先 `node scripts/make-test-media.mjs`'],
]) {
  if (!existsSync(path)) { console.error(`✗ 找不到${label}：${path}\n  ${hint}`); process.exit(2); }
}
for (const bin of ['ffmpeg', 'ffprobe']) {
  const r = runToolSync(bin, ['-version']);
  if (r.error || r.status !== 0) { console.error(`✗ PATH 上没有 ${bin}`); process.exit(2); }
}

const which = argv.filter((a) => ['color-mask', 'fit', 'upscale', 'shadow'].includes(a));
const plan = which.length > 0 ? which : ['color-mask', 'fit', 'upscale', 'shadow'];
tryRemove(WORK);
mkdirSync(WORK, { recursive: true });

for (const name of plan) {
  console.log(`\n=== ${name} ===`);
  if (name === 'color-mask') probeColorMask(cli);
  if (name === 'fit') probeFit(cli);
  if (name === 'upscale') probeUpscale(cli);
  if (name === 'shadow') probeShadow(cli);
}

const failed = results.filter((r) => !r.ok);
console.log(`\n${results.length - failed.length} / ${results.length} 条判据通过`
  + (failed.length > 0 ? `；红的：${failed.map((f) => f.label).join(' | ')}` : ''));
if (failed.length > 0) {
  console.error('✗ 交接单里的某条判据不成立 —— 先按红的那条查，别改判据');
  process.exit(1);
}
console.log(`✓ 全部成立（读数落盘在 ${WORK}）`);
