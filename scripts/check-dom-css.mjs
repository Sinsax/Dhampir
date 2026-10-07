#!/usr/bin/env node
// DOM 侧把特效翻成 CSS 的**判据**：映射对不对，以及「不搬的必须报出来」。
//
//   node scripts/check-dom-css.mjs
//   node scripts/check-dom-css.mjs --self-test
//
// # 它为什么重要
//
// 网页动画的「能力迁移」不是把 CSS 有什么就写什么，而是**只搬能证明等价的**：
// 口径在 plan/web-animation-criteria.md 的 D5（模糊）与 D6（色彩）里。
// 这条判据把口径钉住，并且**反向验证** —— 自检拿两个「看起来很合理」的错误映射器
// 跑同一套用例，它们必须红（否则判据等于没有）。

import { readFileSync } from 'node:fs';

import {
  layerFilters,
  layerBlendMode,
  layerCornerRadius,
  layerClip,
  layerMask,
  layerShadowFilter,
  EXACT,
  APPROXIMATE,
  ENGINE_IMPLEMENTED_BLENDS,
  ENGINE_SUPPORTS_CORNER_RADIUS,
} from '../web/dom-css.mjs';

/** 一套用例。喂任何映射函数都能跑，所以能用来反向验证。 */
export function checkCases(mapper) {
  const problems = [];
  const run = (effects) => mapper(effects);
  const expectCss = (name, effects, css) => {
    const got = run(effects);
    if (got.css !== css) problems.push(name + '：CSS 应当是 ' + JSON.stringify(css) + '，得到 ' + JSON.stringify(got.css));
    return got;
  };
  const expectRefused = (name, effects, needle) => {
    const got = run(effects);
    if (got.refused.length !== 1) {
      problems.push(name + '：应当恰好拒绝一条，得到 ' + got.refused.length + ' 条');
      return got;
    }
    if (got.applied.length !== 0) problems.push(name + '：拒绝了却还应用了 ' + got.applied.length + ' 条');
    const why = String(got.refused[0].why === undefined ? '' : got.refused[0].why);
    if (why.trim().length < 8) problems.push(name + '：拒绝理由太短（不许占位）');
    if (needle !== undefined && !why.includes(needle)) {
      problems.push(name + '：拒绝理由里应当点出「' + needle + '」，得到 ' + JSON.stringify(why));
    }
    return got;
  };

  if (run([]).css !== '') problems.push('没有特效时不该产出任何 CSS');
  expectCss('对比度（逐值等价）', [{ kind: 'contrast', params: { amount: 2 } }], 'contrast(2)');
  // D5：CSS 的 blur 参数是 σ，本仓 σ = radius/2 ⇒ radius 8 应当给 blur(4px)（**不是 blur(8px)**）。
  expectCss('模糊（σ = radius/2）', [{ kind: 'gaussian_blur', params: { radius: 8 } }], 'blur(4px)');
  const sat = run([{ kind: 'saturation', params: { amount: 0.5 } }]);
  if (sat.css !== 'saturate(0.5)') problems.push('饱和度：应当是 saturate(0.5)，得到 ' + JSON.stringify(sat.css));
  if (sat.applied.length !== 1 || sat.applied[0].exactness !== APPROXIMATE) {
    problems.push('饱和度必须是 APPROXIMATE（luma 权重取整不同，D6）');
  }
  // 第 42 轮起：**乘性**那条与 CSS 精确等价（都保黑），加性那条仍拒绝（CSS 里没有加性亮度）。
  expectCss('乘性亮度', [{ kind: 'brightness_multiply', params: { factor: 1.5 } }], 'brightness(1.5)');
  expectRefused('加性亮度（乘 vs 加）', [{ kind: 'brightness', params: { amount: -0.5 } }], '乘');
  expectRefused('色相（矩阵不同源）', [{ kind: 'hue', params: { degrees: 90 } }], '矩阵');
  expectRefused('没实现的特效', [{ kind: 'drop-shadow', params: {} }], '还没实现');
  const mixed = expectCss('两条一起（顺序照给）',
    [{ kind: 'contrast', params: { amount: 1.5 } }, { kind: 'gaussian_blur', params: { radius: 4 } }],
    'contrast(1.5) blur(2px)');
  if (mixed.applied.length !== 2 || mixed.refused.length !== 0) problems.push('两条都该被应用且都不该被拒绝');
  return problems;
}

/** 混合模式的用例：8 条逐值交给 CSS，`add` 近似（CSS 没有 add），并报出引擎侧的缺口。 */
export function checkBlendCases(mapper) {
  const problems = [];
  const expectCss = (name, blend, css) => {
    const got = mapper(blend);
    if (got.css !== css) problems.push(name + '：CSS 应当是 ' + JSON.stringify(css) + '，得到 ' + JSON.stringify(got.css));
    return got;
  };
  // 与引擎一致的四条：逐值交给 CSS。
  for (const name of ['multiply', 'screen']) {
    const got = expectCss(name, name, name);
    if (got.engine !== 'implemented') problems.push(name + '：引擎已实现，不该报成缺口');
  }
  if (expectCss('normal 不设样式', 'normal', '').engine !== 'implemented') problems.push('normal 是被实现的');
  // 引擎没实现的五条：CSS 画得出来，但**必须报出引擎缺口**。
  // 第 31 轮起这 5 条引擎也实现了（读回型回路，D13）—— 所以**不该再报缺口**。
  // （报缺口一度是对的：那时引擎确实画不出来。判据跟着事实走，而不是跟着历史。）
  for (const [name, css] of [['darken', 'darken'], ['lighten', 'lighten'], ['overlay', 'overlay'], ['soft_light', 'soft-light'], ['difference', 'difference']]) {
    const got = expectCss(name, name, css);
    if (got.engine !== 'implemented') problems.push(name + '：引擎已经实现了，不该再报成缺口（假消息比不报更坏）');
  }
  // `add`：CSS 没有它，用 plus-lighter **近似**并写明理由。
  const add = expectCss('add 用 plus-lighter', 'add', 'plus-lighter');
  if (add.exactness !== APPROXIMATE) problems.push('add 必须是 APPROXIMATE（CSS 没有 add）');
  if (String(add.why === undefined ? '' : add.why).trim().length < 8) problems.push('add 的近似理由太短');
  // 不认识的名字：拒绝，不许猜。
  const unknown = mapper('multiply_all_the_things');
  if (unknown.refused === undefined) problems.push('不认识的混合模式必须被拒绝');
  return problems;
}

/**
 * **跨文件比对**：`ENGINE_IMPLEMENTED_BLENDS` 必须与 `layer.rs` 的 `is_implemented` 一字不差。
 *
 * 为什么值得单独一条：引擎哪天多实现一种混合模式，DOM 侧那句「引擎还没实现」就会变成假消息 ——
 * 而假消息比不报更坏（用户会为一条根本不缺的能力去改工程）。
 */
export function checkEngineBlendList(readSource) {
  const problems = [];
  const source = readSource();
  const at = source.indexOf('fn is_implemented');
  if (at < 0) {
    problems.push('layer.rs 里找不到 fn is_implemented —— 解析坏了，不许当通过');
    return problems;
  }
  const match = source.slice(at).match(/matches!\s*\(\s*self\s*,\s*([^)]*)\)/);
  if (match === null) {
    problems.push('is_implemented 里没找到 matches! —— 解析坏了，不许当通过');
    return problems;
  }
  const names = [...new Set([...match[1].matchAll(/Self::([A-Za-z]+)/g)].map((item) =>
    item[1].replace(/([a-z0-9])([A-Z])/g, '$1_$2').toLowerCase()))].sort();
  const expected = [...ENGINE_IMPLEMENTED_BLENDS].sort();
  if (JSON.stringify(names) !== JSON.stringify(expected)) {
    problems.push('ENGINE_IMPLEMENTED_BLENDS 与 layer.rs 的 is_implemented 对不上：源码 ' + JSON.stringify(names) + '，清单 ' + JSON.stringify(expected));
  }
  return problems;
}
/**
 * 掩码的用例。
 *
 * 最要紧的一条不是"写成什么"，而是**什么时候刻意不写**：掩码图取不到时浏览器会把整个元素
 * 遮没（拿不到的遮罩 = 全黑），所以相对文件名那种必须**不设**并报出来 —— 否则预览会反而变坏。
 */
export function checkMaskCases(mapper) {
  const problems = [];
  const assets = {
    'm.png': { id: 'm.png', uri: 'm.png' },
    'remote.png': { id: 'remote.png', uri: 'https://example.test/m.png' },
  };
  if (mapper(undefined, assets).css !== '') problems.push('没有掩码时不该设样式');
  // **程序化渐变**（第 47 轮）：看是否写成 CSS 的 linear-gradient，
  // 而且覆盖度落在 **alpha** 上（引擎默认读掩码的 alpha 通道）。
  const gradient = mapper(
    {
      asset_id: '',
      gradient: { angle_deg: 90, stops: [{ at: 0, coverage: 0 }, { at: 1, coverage: 0.5 }] },
      invert: false,
    },
    assets,
  );
  if (gradient.css !== 'linear-gradient(90deg, rgba(0, 0, 0, 0) 0%, rgba(0, 0, 0, 0.5) 100%)') {
    problems.push('渐变掩码应当写成 linear-gradient 且覆盖度落在 alpha 上，得到 ' + JSON.stringify(gradient.css));
  }
  if (gradient.notes !== undefined && gradient.notes.length > 0 && gradient.notes[0].kind !== 'mask') {
    problems.push('渐变掩码的说明不该是别的种类');
  }
  const gradientInverted = mapper(
    { asset_id: '', gradient: { angle_deg: 0, stops: [{ at: 0, coverage: 1 }, { at: 1, coverage: 0 }] }, invert: true },
    assets,
  );
  if (!Array.isArray(gradientInverted.notes) || !/反相/.test(String(gradientInverted.notes[0].why))) {
    problems.push('渐变掩码 + 反相时，必须说明"DOM 侧没画反相"（引擎侧画了）：' + JSON.stringify(gradientInverted.notes));
  }
  const remote = mapper({ asset_id: 'remote.png' }, assets);
  if (remote.css !== 'url(\"https://example.test/m.png\")') {
    problems.push('能直接取的 uri 应当写成 url(…)，得到 ' + JSON.stringify(remote.css));
  }
  if (remote.exactness !== 'unmeasured') problems.push('这条必须自报**未实测**（DOM 不做素材解码）');
  const relative = mapper({ asset_id: 'm.png' }, assets);
  if (relative.css !== '') {
    problems.push('uri 取不到时**不该设** mask-image（浏览器取不到会把整个元素遮没），得到 ' + JSON.stringify(relative.css));
  }
  if (relative.refused === undefined || !String(relative.refused.why).includes('遮没')) {
    problems.push('取不到时必须报出来，并说清「遮没」这个后果');
  }
  const missing = mapper({ asset_id: 'nope.png' }, assets);
  if (missing.css !== '' || missing.refused === undefined) problems.push('素材不在表里时同样不设并报出');
  const inverted = mapper({ asset_id: 'remote.png', invert: true }, assets);
  if ((inverted.notes === undefined ? [] : inverted.notes).length === 0) {
    problems.push('反相在 CSS 的 mask-image 里表达不了 —— 必须报出来');
  }
  return problems;
}

/** 投影的用例：偏移 / 模糊（**×2**）/ 浓淡（用颜色 alpha）三条口径。 */
export function checkShadowCases(mapper) {
  const problems = [];
  if (mapper(undefined) !== '') problems.push('没有投影时不该产出 filter');
  const got = mapper({ offset_x: 4, offset_y: 6, blur_sigma: 3, opacity: 0.5 });
  const want = 'drop-shadow(4px 6px 6px rgb(0 0 0 / 50%))';
  if (got !== want) problems.push('投影应当是 ' + JSON.stringify(want) + '，得到 ' + JSON.stringify(got));
  const sharp = mapper({ offset_x: 0, offset_y: 0, blur_sigma: 0, opacity: 1 });
  if (sharp !== 'drop-shadow(0px 0px 0px rgb(0 0 0 / 100%))') problems.push('零模糊也应当写全：' + JSON.stringify(sharp));
  return problems;
}


/** 裁剪形状的用例：三种形状的 CSS 拼写，以及 polygon **必须被拒绝**（不假装支持）。 */
export function checkClipCases(mapper) {
  const problems = [];
  const expect = (name, clip, css) => {
    const got = mapper(clip);
    if (got.css !== css) problems.push(name + '：应当是 ' + JSON.stringify(css) + '，得到 ' + JSON.stringify(got.css));
    return got;
  };
  expect('不裁', undefined, '');
  expect('圆（居中不写 at）', { kind: 'circle', radius: 30 }, 'circle(30px)');
  expect('圆（显式中心，比例 → 百分比）', { kind: 'circle', radius: 30, center: [0.25, 0.5] }, 'circle(30px at 25% 50%)');
  expect('椭圆', { kind: 'ellipse', radius_x: 30, radius_y: 20 }, 'ellipse(30px 20px)');
  expect('内缩（圆角 0 不写 round）', { kind: 'inset', top: 4, right: 4, bottom: 4, left: 4, radius: 0 }, 'inset(4px 4px 4px 4px)');
  expect('内缩（带圆角）', { kind: 'inset', top: 4, right: 4, bottom: 4, left: 4, radius: 6 }, 'inset(4px 4px 4px 4px round 6px)');
  // 多边形：**归一化坐标 → 百分比**（同一个口径）。
  expect('多边形', { kind: 'polygon', points: [[0, 0], [1, 0], [0, 1]] }, 'polygon(0% 0%, 100% 0%, 0% 100%)');
  // 路径：**原样透传**（两边都是文档像素）。
  expect('路径', { kind: 'path', data: 'M 0 0 L 32 0 L 0 32 Z' }, 'path("M 0 0 L 32 0 L 0 32 Z")');
  const emptyPath = mapper({ kind: 'path', data: '  ' });
  if (emptyPath.refused === undefined) problems.push('空路径必须被拒绝');
  // 顶点不够：拒绝（引擎那边栅格化出来是全 0 = 整层被裁掉，校验层也会报错）。
  const short = mapper({ kind: 'polygon', points: [[0, 0], [1, 1]] });
  if (short.refused === undefined) problems.push('顶点不够的多边形必须被拒绝');
  return problems;
}

/** 圆角的用例：半径 -> `border-radius`；引擎还画不出来的话必须报出来。 */
export function checkCornerRadiusCases(mapper) {
  const problems = [];
  const none = mapper(0);
  if (none.css !== '') problems.push('半径 0 不该设样式，得到 ' + JSON.stringify(none.css));
  const zeroish = mapper(undefined);
  if (zeroish.css !== '') problems.push('缺省半径不该设样式，得到 ' + JSON.stringify(zeroish.css));
  const got = mapper(24);
  if (got.css !== '24px') problems.push('半径 24 应当是 24px，得到 ' + JSON.stringify(got.css));
  // 引擎从第 13 轮起**真的画了**（合成着色器里的无分支 SDF）。所以这里期望的是 implemented ——
  // 若还报 missing，那就是**假消息**，而假消息比不报更坏（会让人为一条不缺的能力去改工程）。
  if (got.engine !== 'implemented') problems.push('引擎已经实现了圆角，不该再报成缺口');
  if (got.why !== undefined && String(got.why).trim().length > 0) problems.push('不该再给缺口理由了');
  return problems;
}

/**
 * **跨文件比对**：DOM 侧那句「引擎还没实现圆角」必须与 Rust 侧一致。
 *
 * 判据读 `crates/dhampir-timeline/src/layer.rs`：那里有 `unimplemented_corner_radius`
 * 就意味着引擎确实画不了。哪天它被删掉（= 真实现了），这条就该红 —— 逼着 DOM 侧一起改，
 * 而不是让它继续说假话。
 */
export function checkCornerRadiusEngineFlag(readSource) {
  const problems = [];
  const source = readSource();
  const rustSaysMissing = source.includes('fn unimplemented_corner_radius');
  if (rustSaysMissing === ENGINE_SUPPORTS_CORNER_RADIUS) {
    problems.push(
      'ENGINE_SUPPORTS_CORNER_RADIUS=' + String(ENGINE_SUPPORTS_CORNER_RADIUS) +
        '，而 layer.rs 里' + (rustSaysMissing ? '有' : '没有') + ' unimplemented_corner_radius —— 两边对不上',
    );
  }
  return problems;
}

/**
 * 最小 DOM 替身。
 *
 * 无浏览器的地方要能验**宿主接线**（属性有没有真的写上、拒绝项有没有报出来），
 * 所以这里只实现本次用到的几个方法。它验的是接线，**不是渲染** —— 渲染得靠真浏览器（见 plan 的阶段 3）。
 */
function makeStub() {
  const makeNode = () => ({
    style: {},
    dataset: {},
    className: '',
    textContent: '',
    children: [],
    replaceChildren() { this.children = []; },
    appendChild(child) { this.children.push(child); },
  });
  return { createElement: () => makeNode() };
}

/** 宿主接线：给一份两层的小工程，看它有没有把映射真的写进样式、把拒绝项报出来。 */
export async function checkHostWiring() {
  const problems = [];
  globalThis.document = makeStub();
  const { createDomHost } = await import('../web/dom-host.mjs');
  const stage = globalThis.document.createElement('div');
  const seen = [];
  const host = createDomHost({ container: stage, onValues: (payload) => seen.push(payload) });
  const doc = {
    assets: [
      { id: 'm.png', uri: 'm.png' },
      { id: 'remote.png', uri: 'https://example.test/m.png' },
    ],
    render_hints: { width: 320, height: 180 },
    timeline: {
      schema: 4,
      timebase: { num: 30, den: 1 },
      tracks: [
        { id: 'a', kind: 'video', layers: [{ id: 'y', start: 0, end: 10, effects: [
          { kind: 'contrast', params: { amount: 2 } },
          { kind: 'gaussian_blur', params: { radius: 8 } },
        ] }] },
        { id: 'b', kind: 'video', layers: [{ id: 'n', start: 0, end: 10, effects: [
          { kind: 'brightness', params: { amount: -0.5 } },
          { kind: 'hue', params: { degrees: 90 } },
        ] }] },
        { id: 'c', kind: 'video', layers: [{ id: 'm', start: 0, end: 10, blend: 'darken' }] },
        { id: 'd', kind: 'video', layers: [{ id: 'r', start: 0, end: 10, corner_radius: 12 }] },
        { id: 'e', kind: 'video', layers: [{ id: 'q', start: 0, end: 10, clip: { kind: 'circle', radius: 8 } }] },
        { id: 'f', kind: 'video', layers: [{ id: 's', start: 0, end: 10, mask: { asset_id: 'remote.png' } }] },
        { id: 'g', kind: 'video', layers: [{ id: 't', start: 0, end: 10, mask: { asset_id: 'm.png' } }] },
      ],
    },
  };
  host.open(doc);
  host.draw(0);
  const filters = stage.children.map((node) => node.style.filter);
  if (filters[0] !== 'contrast(2) blur(4px)') {
    problems.push('第一层（对比度+模糊）的 style.filter 应当是 contrast(2) blur(4px)，得到 ' + JSON.stringify(filters[0]));
  }
  if (filters[1] !== '') {
    problems.push('第二层（亮度+色相）都被拒绝，style.filter 应当是空串，得到 ' + JSON.stringify(filters[1]));
  }
  // 第三层：引擎没实现 darken，而 CSS 有 —— 样式要写上，**缺口要报出来**。
  if (stage.children[2].style.mixBlendMode !== 'darken') {
    problems.push('第三层的 mix-blend-mode 应当是 darken，得到 ' + JSON.stringify(stage.children[2].style.mixBlendMode));
  }
  const payload = seen[seen.length - 1];
  const refusal = payload.css.find((entry) => entry.id === 'n');
  if (refusal === undefined || refusal.refused.length !== 2) {
    problems.push('第二层的两条拒绝必须报出来（债务可见），得到 ' + JSON.stringify(refusal));
  }
  // 第三层（darken）：第 31 轮起引擎也画得出来 ⇒ 不该再有缺口回报。
  const gap = payload.css.find((entry) => entry.id === 'm');
  if (gap !== undefined && gap.refused.length > 0) {
    problems.push('第三层不该再报暗色缺口（引擎已实现），得到 ' + JSON.stringify(gap.refused));
  }
  // 第四层：契约字段已经有了，DOM 侧先画出来；引擎还画不出来，要报出来。
  if (stage.children[3].style.borderRadius !== '12px') {
    problems.push('第四层的 border-radius 应当是 12px，得到 ' + JSON.stringify(stage.children[3].style.borderRadius));
  }
  // 第五层：裁剪形状要真的写进 clip-path。
  if (stage.children[4].style.clipPath !== 'circle(8px)') {
    problems.push('第五层的 clip-path 应当是 circle(8px)，得到 ' + JSON.stringify(stage.children[4].style.clipPath));
  }
  // 第六层：uri 能取 ⇒ 真的写上 mask-image；第七层：取不到 ⇒ **刻意不设**且报出来。
  if (stage.children[5].style.maskImage !== 'url(\"https://example.test/m.png\")') {
    problems.push('第六层的 mask-image 应当是可取的 url(…)，得到 ' + JSON.stringify(stage.children[5].style.maskImage));
  }
  if (stage.children[6].style.maskImage !== '') {
    problems.push('第七层的掩码取不到 ⇒ 必须**不设** mask-image，得到 ' + JSON.stringify(stage.children[6].style.maskImage));
  }
  const maskRefusal = payload.css.find((entry) => entry.id === 't');
  if (maskRefusal === undefined || maskRefusal.refused.length === 0) {
    problems.push('第七层要把「刻意不设」报出来，得到 ' + JSON.stringify(maskRefusal));
  }
  const cornerEntry = payload.css.find((entry) => entry.id === 'r');
  if (cornerEntry !== undefined && cornerEntry.refused.length > 0) {
    problems.push('第四层不该再报圆角缺口（引擎已实现），得到 ' + JSON.stringify(cornerEntry.refused));
  }
  return problems;
}

function selfTest() {
  const clean = checkCases(layerFilters);
  if (clean.length !== 0) {
    console.error('  - 自检失败：真映射器本来就没过用例：' + clean[0]);
    process.exit(1);
  }
  // 两个「看起来很合理」的错误映射器：判据必须抓住。
  const wrongBrightness = (effects) => {
    const out = layerFilters(effects.filter((effect) => effect.kind !== 'brightness'));
    const bright = effects.filter((effect) => effect.kind === 'brightness');
    for (const effect of bright) {
      // 天真写法：CSS 有 brightness，那就直接搬 —— 但本仓那一条是**加性**的。
      out.applied.push({ kind: 'brightness', css: 'brightness(' + (Number(effect.params.amount) + 1) + ')', exactness: EXACT });
    }
    out.css = out.applied.map((item) => item.css).join(' ');
    return out;
  };
  const wrongBlur = (effects) => {
    const out = layerFilters(effects.filter((effect) => effect.kind !== 'gaussian_blur'));
    for (const effect of effects.filter((item) => item.kind === 'gaussian_blur')) {
      // 天真写法：radius 直接当 CSS 的 blur 参数 —— 少除了那个 2（D5）。
      out.applied.push({ kind: 'gaussian_blur', css: 'blur(' + effect.params.radius + 'px)', exactness: EXACT });
    }
    out.css = out.applied.map((item) => item.css).join(' ');
    return out;
  };
  const wrongAdd = (blend) =>
    blend === 'add'
      // 天真写法：本仓叫 add，那就写成 add —— 可 CSS 里没有这个取值，浏览器会整条忽略。
      ? { css: 'add', exactness: EXACT, engine: 'implemented' }
      : layerBlendMode(blend);
  // 反过来谎报「引擎还不支持圆角」：那是**假消息**，会让人为一条不缺的能力去改工程。
  const wrongCorner = (radius) => {
    const out = { ...layerCornerRadius(radius), engine: 'missing' };
    if (out.css !== '') out.why = '（假的）引擎还没实现圆角';
    return out;
  };
  let caught = 0;
  for (const [name, mapper, cases] of [
    ['亮度被天真地搬成 CSS brightness', wrongBrightness, checkCases],
    ['模糊忘了 σ = radius/2', wrongBlur, checkCases],
    ['add 被当成合法的 CSS 取值', wrongAdd, checkBlendCases],
    ['谎报引擎支持圆角', wrongCorner, checkCornerRadiusCases],
    ['掩码取不到也照写 url（会把元素遮没）', (mask, assets) => {
      if (mask === undefined || mask === null) return { css: '' };
      const asset = (assets === undefined ? {} : assets)[String(mask.asset_id)];
      return asset === undefined ? { css: '' } : { css: 'url(' + JSON.stringify(String(asset.uri)) + ')' };
    }, checkMaskCases],
    ['投影的模糊忘了 ×2（σ 当成了半径）',
      (shadow) => (shadow === undefined || shadow === null
        ? ''
        : 'drop-shadow(' + Number(shadow.offset_x) + 'px ' + Number(shadow.offset_y) + 'px ' +
          Number(shadow.blur_sigma) + 'px rgb(0 0 0 / 100%)'),
      checkShadowCases],
    ['多边形写成裸数字而不是百分比', (clip) => (clip !== undefined && clip.kind === 'polygon'
      ? { css: 'polygon(0 0, 1 0, 0 1)' }
      : layerClip(clip)), checkClipCases],
  ]) {
    const found = cases(mapper);
    if (found.length === 0) {
      console.error('  - 自检失败：判据对「' + name + '」视而不见');
      process.exit(1);
    }
    caught += 1;
  }
  console.log('✓ 自检：' + caught + ' 个错误映射器都被抓住，且真映射器是绿的');
}

async function main() {
  if (process.argv.includes('--self-test')) {
    selfTest();
    return;
  }
  const readLayerRs = () => readFileSync(new URL('../crates/dhampir-timeline/src/layer.rs', import.meta.url), 'utf8');
  const problems = checkCases(layerFilters)
    .concat(
      checkBlendCases(layerBlendMode),
      checkEngineBlendList(readLayerRs),
      checkCornerRadiusCases(layerCornerRadius),
      checkClipCases(layerClip),
      checkMaskCases(layerMask),
      checkShadowCases(layerShadowFilter),
      checkShadowCases(layerShadowFilter),
      checkCornerRadiusEngineFlag(readLayerRs),
      await checkHostWiring(),
    );
  if (problems.length > 0) {
    for (const problem of problems) console.error('  - ' + problem);
    console.error('✗ DOM 侧的特效映射不对');
    process.exit(1);
  }
  console.log('✓ DOM 侧映射与宿主接线：对比度/模糊逐值等价；饱和度带近似说明；亮度/色相拒绝并报出；' +
    '混合模式 9/9 可画（add 近似）；圆角两端都已画（引擎 SDF / CSS border-radius），且那句「引擎支不支持」与 layer.rs 一致');
}

main();
