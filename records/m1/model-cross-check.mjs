// M1：把 scene.rs 测试要钉的东西算出来（一次性脚本，进 target/，不进仓库）。
//
// 这份实现与 scene_model.rs 是**两份**：公式同一个来源（指导文档 + scene.wgsl），
// 但各自独立写。两边算出的字节值必须一致——只钉 Rust 自己的输出证明不了模型是对的。
//
// 输出：target/m1-generated.rs（贴进 scene.rs 的测试模块）+ 屏幕上的缺陷距离。

import { writeFileSync } from "node:fs";

const SIZE = [256, 256];
const clamp01 = (v) => Math.min(1, Math.max(0, v));
const srgbEncode = (l) => {
  const c = clamp01(l);
  return c <= 0.0031308 ? c * 12.92 : 1.055 * Math.pow(c, 1 / 2.4) - 0.055;
};
const srgbDecode = (s) => {
  const c = clamp01(s);
  return c <= 0.04045 ? c / 12.92 : Math.pow((c + 0.055) / 1.055, 2.4);
};
const bL = (l) => Math.round(255 * srgbEncode(l));
const bA = (a) => Math.round(255 * clamp01(a));
const lL = (b) => srgbDecode(b / 255);
const lA = (b) => b / 255;
const rgba = (lin) => [bL(lin[0]), bL(lin[1]), bL(lin[2]), bA(lin[3])];
const frac = (v) => v - Math.floor(v);
const fc = (i) => i + 0.5;
const dist = (a, b) => Math.max(...a.map((v, i) => Math.abs(v - b[i])));

// ---- 逐场景的模型（与 scene_model.rs 对齐） --------------------------------
const gradient = (frame, x) => {
  const t = frac(fc(x) / SIZE[0] + (frame % 16) / 16);
  return [t, 1 - t, frac(t * 8), 1];
};
const gradientHalfPixel = (frame, x) => {
  const t = frac(x / SIZE[0] + (frame % 16) / 16); // 少了那半个像素
  return [t, 1 - t, frac(t * 8), 1];
};
const checker = (frame, x, y) => {
  const cell = 4 + (frame % 3) * 4;
  const odd = (Math.floor(fc(x) / cell) + Math.floor(fc(y) / cell)) % 2 === 1;
  return odd ? [0.05, 0.55, 0.95, 1] : [0.85, 0.15, 0.35, 1];
};
const stripeAt = (frame, x) => Math.floor(frac(fc(x) / SIZE[0] + (frame % 8) / 8) * 8);
const srgbLinear = (frame, x) => {
  const d = srgbDecode(stripeAt(frame, x) / 8);
  return [d, d, d, 1];
};

const LAYERS = [
  [0.8, 0.1, 0.1, 0.5],
  [0.1, 0.7, 0.2, 0.5],
  [0.35, 0.25, 0.8, 0.25],
  [0.75, 0.65, 0.1, 0.75],
  [0.15, 0.45, 0.6, 0.5],
];
const layerCount = (f) => 2 + (f % 4);
function stackIdeal(frame, reverse = false) {
  let idx = [...Array(layerCount(frame)).keys()];
  if (reverse) idx.reverse();
  let acc = [0, 0, 0, 0];
  for (const i of idx) {
    const [r, g, b, a] = LAYERS[i];
    acc = [r * a + acc[0] * (1 - a), g * a + acc[1] * (1 - a), b * a + acc[2] * (1 - a), a + acc[3] * (1 - a)];
  }
  return acc;
}
function stackBytes(frame, reverse = false) {
  let idx = [...Array(layerCount(frame)).keys()];
  if (reverse) idx.reverse();
  let dst = [0, 0, 0, 0];
  for (const i of idx) {
    const [r, g, b, a] = LAYERS[i];
    const d = [lL(dst[0]), lL(dst[1]), lL(dst[2])];
    dst = [bL(r * a + d[0] * (1 - a)), bL(g * a + d[1] * (1 - a)), bL(b * a + d[2] * (1 - a)), bA(a + lA(dst[3]) * (1 - a))];
  }
  return dst;
}
// 两种混合状态的缺陷：颜色源因子用 One（= PREMULTIPLIED_ALPHA_BLENDING）
function stackPremultipliedColor(frame) {
  let dst = [0, 0, 0, 0];
  for (let i = 0; i < layerCount(frame); i++) {
    const [r, g, b, a] = LAYERS[i];
    const d = [lL(dst[0]), lL(dst[1]), lL(dst[2])];
    dst = [bL(r + d[0] * (1 - a)), bL(g + d[1] * (1 - a)), bL(b + d[2] * (1 - a)), bA(a + lA(dst[3]) * (1 - a))];
  }
  return dst;
}
// 缺陷：alpha 通道的源因子也写成 SrcAlpha
function stackAlphaSrcAlpha(frame) {
  let dst = [0, 0, 0, 0];
  for (let i = 0; i < layerCount(frame); i++) {
    const [r, g, b, a] = LAYERS[i];
    const d = [lL(dst[0]), lL(dst[1]), lL(dst[2])];
    dst = [bL(r * a + d[0] * (1 - a)), bL(g * a + d[1] * (1 - a)), bL(b * a + d[2] * (1 - a)), bA(a * a + lA(dst[3]) * (1 - a))];
  }
  return dst;
}

const W = [0.270682, 0.216745, 0.111281, 0.036633];
const R = 3;
const srcAt = (x, y) => {
  if (x >= 40 && x < 43 && y >= 40 && y < 43) return 1.0;
  return (Math.floor(x / 8) + Math.floor(y / 8)) % 2 === 1 ? 0.6 : 0.0;
};
const resolve = (c, dim, mode) => {
  if (c < 0 || c > dim - 1) {
    if (mode === "clamp") return Math.min(dim - 1, Math.max(0, c));
    if (mode === "zero") return null;
    return ((c % dim) + dim) % dim;
  }
  return c;
};
const samp = (x, y, mode) => {
  const cx = resolve(x, SIZE[0], mode);
  const cy = resolve(y, SIZE[1], mode);
  return cx === null || cy === null ? 0.0 : srcAt(cx, cy);
};
const hAt = (x, y, mode, box) => {
  let acc = 0;
  for (let i = -R; i <= R; i++) acc += (box ? 1 / 7 : W[Math.abs(i)]) * samp(x + i, y, mode);
  return acc;
};
const blur = (x, y, mode = "clamp", box = false) => {
  let acc = 0;
  for (let j = -R; j <= R; j++) acc += (box ? 1 / 7 : W[Math.abs(j)]) * hAt(x, y + j, mode, box);
  return acc;
};

// ---- 采样表（与 scene.rs 的静态表逐点一致） --------------------------------
const SCENES = [
  { name: "gradient", span: 16, pts: [[8, 128], [32, 128], [48, 128], [104, 128], [200, 128]] },
  { name: "checker", span: 3, pts: [[3, 1], [4, 1], [7, 1], [8, 1], [11, 1], [12, 1], [255, 255]] },
  { name: "srgb_linear", span: 8, pts: [[16, 128], [80, 128], [144, 128], [240, 128]] },
  { name: "alpha_stack", span: 4, pts: [[128, 128], [8, 248]] },
  { name: "blur", span: 1, pts: [[19, 19], [35, 27], [38, 41], [41, 41], [0, 41]] },
];

function expected(name, frame, x, y) {
  if (name === "gradient") return rgba(gradient(frame, x));
  if (name === "checker") return rgba(checker(frame, x, y));
  if (name === "srgb_linear") return rgba(srgbLinear(frame, x));
  if (name === "alpha_stack") return stackBytes(frame, false);
  if (name === "blur") {
    const v = blur(x, y, "clamp");
    return rgba([v, v, v, 1]);
  }
  throw new Error("未知场景 " + name);
}

// ---- 1. 钉住的整张表 + 摘要 -------------------------------------------------
const rows = [];
const FNV_OFFSET = 0xcbf29ce484222325n;
const FNV_PRIME = 0x100000001b3n;
let hash = FNV_OFFSET;
const feed = (b) => {
  hash = (hash ^ BigInt(b)) & 0xffffffffffffffffn;
  hash = (hash * FNV_PRIME) & 0xffffffffffffffffn;
};
for (const scene of SCENES) {
  for (let frame = 0; frame < scene.span; frame++) {
    scene.pts.forEach(([x, y], index) => {
      const bytes = expected(scene.name, frame, x, y);
      bytes.forEach(feed);
      if (frame < 4 || scene.span === 1) rows.push([scene.name, frame, index, bytes]);
    });
  }
}
const digest = hash.toString(16).padStart(16, "0");
console.log("预测表摘要（FNV-1a 64）= " + digest);
console.log("行数（帧 0..3 全部 + blur）= " + rows.length);

// ---- 2. 缺陷距离 -----------------------------------------------------------
const minOver = (list) => Math.min(...list);
const report = (label, values) => {
  const positives = values.filter((v) => v > 0);
  console.log(`  ${label}: 最小 ${minOver(values)}  可观测样本 ${positives.length}/${values.length}  最小正距离 ${positives.length ? minOver(positives) : "-"}`);
};

console.log("\n== 容差要放过的缺陷：距离 ==");
{
  const perms = [];
  const premul = [];
  const alphaSrc = [];
  const order = [];
  for (const scene of ["gradient", "checker", "alpha_stack"]) {
    const spec = SCENES.find((s) => s.name === scene);
    for (let frame = 0; frame < spec.span; frame++)
      for (const [x, y] of spec.pts) {
        const e = expected(scene, frame, x, y);
        const swapped = [e[2], e[1], e[0], e[3]];
        perms.push(dist(e, swapped));
      }
  }
  report("通道置换 r↔b", perms);
  for (let frame = 0; frame < 4; frame++) {
    premul.push(dist(stackBytes(frame, false), stackPremultipliedColor(frame)));
    alphaSrc.push(dist(stackBytes(frame, false), stackAlphaSrcAlpha(frame)));
    order.push(dist(stackBytes(frame, false), stackBytes(frame, true)));
  }
  report("混合状态：颜色源因子写成 One", premul);
  report("混合状态：alpha 源因子写成 SrcAlpha", alphaSrc);
  report("层序反了", order);
}
{
  const missing = [];
  const spec = SCENES.find((s) => s.name === "srgb_linear");
  for (let frame = 0; frame < spec.span; frame++)
    for (const [x] of spec.pts) {
      const k = stripeAt(frame, x);
      const level = k / 8;
      missing.push(dist(rgba(srgbLinear(frame, x)), rgba([level, level, level, 1])));
    }
  report("漏一次解码（把 sRGB 电平当线性值输出）", missing);
}
{
  const halfPhase = [];
  const rampOnly = [];
  const sawOnly = [];
  const spec = SCENES.find((s) => s.name === "gradient");
  for (let frame = 0; frame < spec.span; frame++)
    for (const [x] of spec.pts) {
      const a = rgba(gradient(frame, x));
      const b = rgba(gradientHalfPixel(frame, x));
      halfPhase.push(dist(a, b));
      rampOnly.push(Math.abs(a[0] - b[0]));
      sawOnly.push(Math.abs(a[2] - b[2]));
    }
  report("半像素相位（floor 与 x+0.5）", halfPhase);
  report("  只看 r 通道（斜坡）", rampOnly);
  report("  只看 b 通道（锯齿）", sawOnly);
}
{
  const boxy = [];
  const spec = SCENES.find((s) => s.name === "blur");
  for (const [x, y] of spec.pts) {
    const g = blur(x, y, "clamp", false);
    const b = blur(x, y, "clamp", true);
    boxy.push(dist(rgba([g, g, g, 1]), rgba([b, b, b, 1])));
  }
  report("盒式滤波替代高斯", boxy);
}
{
  const spec = SCENES.find((s) => s.name === "blur");
  console.log("\n== blur 五点 × 三种边界语义 ==");
  for (const [x, y] of spec.pts) {
    const c = blur(x, y, "clamp");
    const z = blur(x, y, "zero");
    const w = blur(x, y, "wrap");
    console.log(
      `  (${String(x).padStart(3)},${String(y).padStart(3)}) clamp=${bL(c)} zero=${bL(z)} wrap=${bL(w)}  |clamp-zero|=${Math.abs(bL(c) - bL(z))} |clamp-wrap|=${Math.abs(bL(c) - bL(w))}`
    );
  }
}
{
  console.log("\n== 容差本身覆盖的实现自由（不是缺陷） ==");
  const stackQuant = [];
  for (let frame = 0; frame < 4; frame++) stackQuant.push(dist(stackBytes(frame, false), rgba(stackIdeal(frame, false))));
  report("alpha 逐层落回 8 位的量化", stackQuant);
}

// ---- 3. 生成 Rust 片段 ------------------------------------------------------
const lines = [];
lines.push("    /// 帧 0..3（`blur` 只有一帧）的全部采样点预测值，由 `records/m1/model-cross-check.mjs`");
lines.push("    /// 独立算出。三元组是 `(场景, 帧, 采样点下标)`。");
lines.push("    #[rustfmt::skip]");
lines.push("    const PINNED: &[(&str, u32, usize, [u8; 4])] = &[");
for (const [name, frame, index, bytes] of rows) {
  lines.push(`        ("${name}", ${frame}, ${index}, [${bytes.join(", ")}]),`);
}
lines.push("    ];");
lines.push("");
lines.push("    /// 整张预测表（每个场景走完一个周期 × 每个采样点 × 四个通道）的 FNV-1a 64。");
lines.push("    ///");
lines.push("    /// 序列化方式：按 [`SELECTABLE_SCENES`] 的顺序 → 帧 0..周期 → 采样点顺序 → 通道 0..4，");
lines.push("    /// 逐字节喂进 FNV-1a（offset basis `0xcbf29ce484222325`、prime `0x100000001b3`）。");
lines.push(`    const PREDICTION_TABLE_HASH: &str = "${digest}";`);
writeFileSync("target/m1-generated.rs", lines.join("\n") + "\n", "utf8");
console.log("\n已写出 target/m1-generated.rs");
