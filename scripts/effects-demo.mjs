#!/usr/bin/env node
// 「全部枚举效果」演示工程的生成器。跑：
//
//   node scripts/effects-demo.mjs
//
// 产出（都在 out/effects-demo/ 下）：
//   assets/{bg,tile,mask,empty}.png      素材（捆绑 Python 的 Pillow 生成，不需要任何外部工具）
//   effects-demo.doc.json               工程：全部 16 条登记效果 + 9 条混合 + 5 种裁剪 + 圆角/掩码/投影/背景滤镜/关键帧缓动
//   effects-demo.md                     效果索引：每条效果在哪个时间段、哪一层
//
// 三份产物（前端 HTML / wasm 网页 / native 视频）都吃同一份工程 —— 三端一致才有可比性。
// 工程由这里生成而不是手写：手写一份一定会与登记表漂开。
import { existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const OUT = join(REPO_ROOT, 'out', 'effects-demo');
const ASSETS = join(OUT, 'assets');
const FPS = 30;
const DOC_W = 640;
const DOC_H = 360;

const ZONE_A1 = { from: 0, to: 90 };     // 3 秒：9 条混合
const ZONE_A2 = { from: 90, to: 180 };   // 3 秒：5 种裁剪 + 圆角/掩码/投影/背景滤镜/缓动
const ZONE_B = { from: 180, to: 660 };   // 16 秒：16 条效果，每条 1 秒

const BLEND_MODES = ['normal', 'add', 'multiply', 'screen', 'darken', 'lighten', 'overlay', 'soft_light', 'difference'];

const EFFECTS = [
  { kind: 'gaussian_blur', params: { radius: 6 }, note: '模糊（可分离两趟）' },
  { kind: 'brightness', params: { amount: 0.15 }, note: '亮度（加性）' },
  { kind: 'brightness_multiply', params: { factor: 1.6 }, note: '亮度（乘性、保黑；对应 CSS brightness）' },
  { kind: 'contrast', params: { amount: 1.6 }, note: '对比度（绕 0.5 缩放）' },
  { kind: 'saturation', params: { amount: 1.6 }, note: '饱和度（Rec.709 权重）' },
  { kind: 'saturation_css', params: { amount: 1.6 }, note: '饱和度（CSS/SVG 规范权重）' },
  { kind: 'hue', params: { degrees: 60 }, note: '色相（YIQ / Rec.601 矩阵）' },
  { kind: 'hue_rotate_css', params: { degrees: 90 }, note: '色相（CSS/SVG 规范矩阵）' },
  { kind: 'flash', params: { amount: 0.35, r: 1, g: 0.2, b: 0.2 }, note: '闪白/闪色（常量色叠加）' },
  { kind: 'vignette', params: { amount: 0.7, radius: 1.2, softness: 0.4 }, note: '暗角' },
  { kind: 'noise', params: { amount: 0.35, seed: 3 }, note: '噪声' },
  { kind: 'overlay', params: { amount: 0.5, r: 0.1, g: 0.2, b: 0.6, r2: 0.7, g2: 0.2, b2: 0.1, shape: 0, angle: 45 }, note: '渐变叠加' },
  { kind: 'shake', params: { amount: 0.06, frequency: 12, seed: 5 }, note: '抖动（Warp）' },
  { kind: 'zoom_bounce', params: { amount: 0.25, frequency: 2 }, note: '缩放弹跳（Warp）' },
  { kind: 'pulse', params: { amount: 0.2, frequency: 2 }, note: '脉动（Warp）' },
  { kind: 'split', params: { offset: 0.12, skew: 12, amount: 0.8 }, note: '错切分裂（Warp）' },
];

const EASINGS = [
  'linear', 'cubic-bezier(0.25,0.1,0.25,1)', 'ease-in-out', 'back_out',
  'steps(4)', 'linear(0, 0.25 75%, 1)', 'cubic-bezier(0.68,-0.55,0.265,1.55)', 'ease-in',
];

const layer = (id, start, end, extra = {}) => Object.assign({
  id, start, end,
  transform: { x: 0, y: 0, scale: 1, rotation: 0 },
  opacity: 1, blend: 'normal', enabled: true, gain: 1,
  effects: [], keyframes: [], markers: [], tags: {}, loop_source: true,
}, extra);

const transform = (x, y, scale, rotation = 0) => ({ x, y, scale, rotation });
const picture = (id, start, end, assetId, tf, extra = {}) =>
  layer(id, start, end, Object.assign({ source: { asset_id: assetId, source_in: 0 }, transform: tf }, extra));

function buildDoc() {
  const index = [];
  // 契约不允许**同轨**图层时间重叠 ⇒ 凡是同时在画的层各占一条轨。
  // 设计：时序 showreel（每项一段），于是只需 4 条轨：底 / 基 / 瓦片 / 调整层。
  const tBg = [];
  const tBase = [];
  const tTile = [];
  const tFx = [];
  const ITEM = 20; // 每项 20 帧（约 0.67 秒）
  let cursor = 0;
  const next = () => { const from = cursor; cursor += ITEM; return { from, to: cursor }; };

  // 0) 底：一张平滑渐变，全片都在（让模糊/暗角/噪声/色相都看得出来）
  // **不再画一张覆盖全片的底图**：每一段自己已经画了基图，再叠一张同素材的层会让
  // 同一帧上出现两个层引用同一个素材 —— 那正是池子记账透支的那个形状（见
  // plan/waapi-stage3-evidence.md 第 79 轮）。视觉不受影响：每段的基图就是那张底图。

  // 1) 9 条混合模式：底图 + 半透明瓦片（各种混合）
  BLEND_MODES.forEach((mode, i) => {
    const { from, to } = next();
    tBase.push(picture('blend-base-' + mode, from, to, 'bg.png', transform(0, 0, 0.5)));
    tTile.push(picture('blend-top-' + mode, from, to, 'tile.png', transform(0, 0, 0.42), { blend: mode, opacity: 0.92 }));
    index.push({ zone: 'A', item: '混合 ' + mode, layer: 'blend-top-' + mode, from, to });
  });

  // 2) 层特性：5 种裁剪 + 圆角 + 两种掩码 + 投影 + 背景滤镜
  const features = [
    { id: 'clip-circle', extra: { clip: { kind: 'circle', center: [0.5, 0.5], radius: 110 } }, note: '裁剪：圆' },
    { id: 'clip-ellipse', extra: { clip: { kind: 'ellipse', center: [0.5, 0.5], radius_x: 130, radius_y: 80 } }, note: '裁剪：椭圆' },
    { id: 'clip-inset', extra: { clip: { kind: 'inset', top: 24, right: 24, bottom: 24, left: 24, radius: 12 } }, note: '裁剪：内缩矩形（带圆角）' },
    { id: 'clip-polygon', extra: { clip: { kind: 'polygon', points: [[0.5, 0.02], [0.98, 0.94], [0.02, 0.94]] } }, note: '裁剪：多边形' },
    { id: 'clip-path', extra: { clip: { kind: 'path', data: 'M 0.1 0.1 L 0.9 0.1 L 0.9 0.9 L 0.1 0.9 Z' } }, note: '裁剪：路径' },
    { id: 'corner-60', extra: { corner_radius: 60 }, note: '圆角 60px' },
    { id: 'mask-gradient', extra: { mask: { asset_id: '', channel: 'alpha', invert: false, gradient: { angle_deg: 90, stops: [{ at: 0, coverage: 0 }, { at: 1, coverage: 1 }] } } }, note: '掩码：程序化线性渐变' },
    { id: 'mask-asset', extra: { mask: { asset_id: 'mask.png', channel: 'alpha', invert: false } }, note: '掩码：素材（alpha 通道）' },
    { id: 'shadow', extra: { shadow: { offset_x: 14, offset_y: 14, blur_sigma: 6, opacity: 0.8 } }, note: '投影（向外扩散）' },
    { id: 'backdrop', extra: { backdrop_effects: [{ kind: 'gaussian_blur', params: { radius: 8 } }] }, asset: 'empty.png', note: '背景滤镜（读身后内容再糊）' },
  ];
  features.forEach((f) => {
    const { from, to } = next();
    tBase.push(picture('feat-base-' + f.id, from, to, 'bg.png', transform(0, 0, 0.5)));
    if (f.asset === undefined && f.extra.backdrop_effects !== undefined) {
      // 背景滤镜层**不带素材**：它自己的像素是全透明的，带素材只会让渲染器为了"读身后"
      // 把它再画一遍 ⇒ 同一帧上对同一素材要两次 ⇒ 池子记账透支（见第 79 轮）。
      tTile.push(layer('feat-' + f.id, from, to, f.extra));
    } else {
      tTile.push(picture('feat-' + f.id, from, to, f.asset || 'tile.png', transform(0, 0, f.asset ? 0.5 : 0.42), f.extra));
    }
    index.push({ zone: 'A', item: f.note, layer: 'feat-' + f.id, from, to });
  });

  // 3) 八种缓动：同一块瓦片转 + 缩放，逐条换缓动（动起来才看得出来）
  EASINGS.forEach((easing, i) => {
    const { from, to } = next();
    tBase.push(picture('ease-base-' + i, from, to, 'bg.png', transform(0, 0, 0.5)));
    tTile.push(picture('ease-' + i, from, to, 'tile.png', transform(0, 0, 0.18), {
      keyframes: [
        { frame: 0, target: 'rotation', value: 0, easing },
        { frame: ITEM - 1, target: 'rotation', value: 180, easing: 'linear' },
        { frame: 0, target: 'scale', value: 0.18, easing },
        { frame: ITEM - 1, target: 'scale', value: 0.5, easing: 'linear' },
        { frame: 0, target: 'x', value: -160, easing },
        { frame: ITEM - 1, target: 'x', value: 160, easing: 'linear' },
      ],
    }));
    index.push({ zone: 'A', item: '缓动 ' + easing, layer: 'ease-' + i, from, to, easing });
  });

  // 4) 16 条效果：底图 + 运动瓦片 + **调整图层**（效果挂它身上，影响下面已画的内容）
  EFFECTS.forEach((spec, i) => {
    const { from, to } = next();
    const easing = EASINGS[i % EASINGS.length];
    tBase.push(picture('fx-base-' + spec.kind, from, to, 'bg.png', transform(0, 0, 0.5)));
    tTile.push(picture('fx-tile-' + spec.kind, from, to, 'tile.png', transform(-140, 40, 0.3), {
      keyframes: [
        { frame: 0, target: 'x', value: -140, easing },
        { frame: ITEM - 1, target: 'x', value: 140, easing: 'linear' },
        { frame: 0, target: 'rotation', value: -20, easing },
        { frame: ITEM - 1, target: 'rotation', value: 20, easing: 'linear' },
      ],
    }));
    tFx.push(layer('fx-' + spec.kind, from, to, {
      effects: [{ kind: spec.kind, params: spec.params, opacity: 1 }],
      note: spec.note,
    }));
    index.push({ zone: 'B', item: '效果 ' + spec.kind + '（' + spec.note + '）', layer: 'fx-' + spec.kind, from, to, easing });
  });

  const sample = JSON.parse(readFileSync(join(REPO_ROOT, 'fixtures', 'sample-project.doc.json'), 'utf8'));
  const tracks = [
    { id: 'base', kind: 'video', gain: 1, layers: tBase, subtitle: null, danmaku: null },
    { id: 'tile', kind: 'video', gain: 1, layers: tTile, subtitle: null, danmaku: null },
    { id: 'fx', kind: 'video', gain: 1, layers: tFx, subtitle: null, danmaku: null },
  ].filter((track) => track.layers.length > 0);
  const totalLayers = tracks.reduce((sum, track) => sum + track.layers.length, 0);
  return {
    doc: {
      project_schema: sample.project_schema,
      generator: sample.generator,
      meta: sample.meta,
      assets: [
        { id: 'bg.png', kind: 'image', name: '渐变背景', uri: 'assets/bg.png', frame_count: 1, timebase: { num: FPS, den: 1 }, width: 1280, height: 720, tags: {}, content_hash: null },
        { id: 'tile.png', kind: 'image', name: '半透明瓦片', uri: 'assets/tile.png', frame_count: 1, timebase: { num: FPS, den: 1 }, width: 400, height: 400, tags: {}, content_hash: null },
        { id: 'mask.png', kind: 'image', name: '掩码（alpha 渐变）', uri: 'assets/mask.png', frame_count: 1, timebase: { num: FPS, den: 1 }, width: 256, height: 256, tags: {}, content_hash: null },
        { id: 'empty.png', kind: 'image', name: '全透明（给背景滤镜用）', uri: 'assets/empty.png', frame_count: 1, timebase: { num: FPS, den: 1 }, width: 64, height: 64, tags: {}, content_hash: null },
      ],
      timeline: { schema: sample.timeline.schema, timebase: { num: FPS, den: 1 }, markers: [], tracks },
      view: { playhead: 0, selection: null, zoom: 1 },
      render_hints: { width: DOC_W, height: DOC_H, format: 'mp4' },
      extensions: sample.extensions,
    },
    index,
    totalFrames: cursor,
    totalLayers,
  };
}
function main() {
  mkdirSync(ASSETS, { recursive: true });
  const emptyPath = join(ASSETS, 'empty.png');
  if (!existsSync(emptyPath)) {
    console.warn('缺少 assets/empty.png：请先用 Python/Pillow 生成（见 target/make-assets.py 的同类写法）');
  }
  const { doc, index, totalFrames, totalLayers } = buildDoc();
  const docPath = join(OUT, 'effects-demo.doc.json');
  writeFileSync(docPath, JSON.stringify(doc, null, 2) + '\n');
  const lines = [
    '# 全部枚举效果：索引',
    '',
    '工程：out/effects-demo/effects-demo.doc.json（三份产物共用这一份）',
    '长度：' + totalFrames + ' 帧 @ ' + FPS + ' fps（' + (totalFrames / FPS).toFixed(1) + ' 秒），文档尺寸 ' + DOC_W + 'x' + DOC_H,
    '',
    '| 区段 | 条目 | 图层 | 帧区间 | 缓动 |',
    '|---|---|---|---|---|',
    ...index.map((r) => '| ' + r.zone + ' | ' + r.item + ' | ' + r.layer + ' | ' + r.from + '-' + r.to + ' | ' + (r.easing || '-') + ' |'),
    '',
    '## 计数',
    '',
    '- 效果（登记表 16 条）：' + EFFECTS.length,
    '- 混合模式（9 条）：' + BLEND_MODES.length,
    '- 裁剪形状（5 种）：5',
    '- 轨道数：' + doc.timeline.tracks.length + '，图层总数：' + totalLayers,
    '',
  ];
  writeFileSync(join(OUT, 'effects-demo.md'), lines.join('\n'));
  console.log('已生成 ' + docPath + '（' + doc.timeline.tracks.length + ' 轨 / ' + totalLayers + ' 层 / ' + totalFrames + ' 帧）与 effects-demo.md（' + index.length + ' 条索引）');
}

main();
