// HTML 宿主（第二个宿主）：把工程文件渲染成 DOM + CSS。
//
// # 它跟`web/engine.js`（wasm 宿主）是什么关系
//
// 同一个位置：`open / resize / sources_for / clear_bitmaps / set_bitmap / text_frame / draw` ——
// **同一套接口名**，换的是实现：那边把帧画进 WebGPU 画布，这边把同一个工程摊成 DOM，
// 由浏览器自己去排版、上色、画字。
//
// 为什么要它（而不是只有 wasm 宿主）：**设计完整度期要快**。DOM 侧白拿 CSS 的布局、
// 字体渲染、滤镜与混合，不必等引擎把那些原语补齐；等引擎补齐一条，就把它从"HTML 支持"
// 挪到"dhampir 支持"（见 plan/web-animation-parity.md 阶段 4/5）。
//
// # 纪律：它**只吃工程文件**，不许自己发明数据结构
//
// 每个层 = 一个绝对定位的 div；`transform / opacity` 直接吃求值出来的通道值。
// 求值走 `./anim-eval.mjs`（**第二实现**，判据在 scripts/check-anim-eval.mjs）。
// CSS 里画不出来的（读回型混合、几何遮罩、矢量）进能力登记表，不在这里偷偷塞。

import { evaluateFrame } from './anim-eval.mjs';
import {
  layerFilters,
  layerBlendMode,
  layerCornerRadius,
  layerClip,
  layerMask,
  layerShadowFilter,
} from './dom-css.mjs';

/**
 * 与 wasm 宿主同一套接口名 —— **实现了的那些**。
 *
 * 这份清单与下面的 HOST_UNIMPLEMENTED 合起来，必须把「宿主会来调的每一步」都表态完：
 * 要么实现，要么**写明不做及为什么**。第三种状态（静默缺失）由
 * scripts/check-host-parity.mjs 判红 —— 它拿这些名字去对 docs/api-surface.md 里
 * 「底座 API」那段**承诺过的导出名**。
 */
export const HOST_INTERFACE = [
  'open', 'resize', 'sources_for', 'clear_bitmaps', 'set_bitmap', 'text_frame', 'draw', 'describe',
];

/**
 * **明确不做的**：名字 → 为什么。
 *
 * 空理由、占位理由（TODO / 以后再说 / 待定）都不行 —— 判据会红。
 * 理由要能回答一句话：「下游拿着 wasm 宿主的调用顺序来调这边，会看到什么」。
 */
export const HOST_UNIMPLEMENTED = {
  attach: 'DOM 侧没有把画布交给引擎这一步：DOM 树自己就是画面',
  bind_source: '源由浏览器自己管（video/img），不需要引擎绑定',
  set_bitmap_mode: '位图那条链在 DOM 侧不存在',
  begin_frame: '帧计划是给位图复用与缓存用的，DOM 侧没有这一步',
  end_frame: '同上',
  preroll: '预渲染是给 WebGPU 途径省 seek 的，DOM 侧没有 seek',
  set_text_bitmap: '文字由浏览器自己渲染，不需要宿主交位图（HTML 宿主快就快在这里）',
  set_danmaku_bitmap: '同上',
  set_subtitles: '字幕在 DOM 侧就是节点，不需要往引擎里塞',
  first_frame: 'DOM 侧没有第一帧预热这件事',
  frame: '帧号由调用方给 draw；DOM 侧不需要单独探测当前帧',
  precheck: '能力协商的消费端在下游，不在渲染宿主里',
  edit: '编辑面归下游：这里只是渲染宿主，要编辑请调 wasm 宿主或 core',
  undo: '同上',
  redo: '同上',
  doc: 'DOM 宿主不持有工程文件的权威副本；权威在调用方手里',
};

export function createDomHost(options) {
  const container = options.container;
  const onValues = options.onValues === undefined ? () => {} : options.onValues;
  let doc = null;
  let stage = { width: 1920, height: 1080 };
  let elements = new Map();
  let layersById = new Map();
  let assetsById = new Map();
  let currentFrame = -1;

  function open(projectDoc) {
    doc = projectDoc;
    container.replaceChildren();
    elements = new Map();
    assetsById = new Map();
    for (const asset of doc.assets === undefined ? [] : doc.assets) assetsById.set(asset.id, asset);
    currentFrame = -1;
    const hints = doc.render_hints === undefined ? {} : doc.render_hints;
    let order = 0;
    for (const track of doc.timeline.tracks === undefined ? [] : doc.timeline.tracks) {
      if (track.kind !== 'video') continue;
      for (const layer of track.layers === undefined ? [] : track.layers) {
        const node = document.createElement('div');
        node.dataset.layerId = layer.id;
        node.className = 'dom-layer';
        // 层内容：这一版不画素材本身，只画一个可见的方块并写上 id ——
        // **看得见**才能回答"某一层为什么没出现"。（特效 / 混合 / 圆角 / 裁剪 / 掩码
        // 都是靠写样式作用在这个方块上的。）
        node.textContent = layer.id;
        node.style.zIndex = String(order);
        order += 1;
        container.appendChild(node);
        elements.set(layer.id, node);
        layersById.set(layer.id, layer);
      }
    }
    resize(hints.width === undefined ? 1920 : hints.width, hints.height === undefined ? 1080 : hints.height);
    return { ok: true, layers: elements.size };
  }

  /** 与 wasm 宿主同一个语义：舞台尺寸决定"文档像素 → 屏幕像素"的比例。 */
  function resize(width, height) {
    stage = { width, height };
    container.style.width = width + 'px';
    container.style.height = height + 'px';
    return { width, height };
  }

  /**
   * **DOM 侧没有"取源"这一步**：视频元素、贴纸、字形都由浏览器自己管，
   * 不存在"把某一帧的位图交给引擎"这条链。返回空数组是**如实**，不是占位。
   */
  function sources_for() {
    return [];
  }

  function clear_bitmaps() {
    /* 同一件事：位图这条链在 DOM 侧不存在。显式写成空函数，而不是让它看起来"忘了实现"。 */
  }

  function set_bitmap() {
    throw new Error(
      'HTML 宿主不接受位图：字形与素材由浏览器自己管。' +
        '要交位图的那条链走 wasm 宿主（web/engine.js 的 set_bitmap）。',
    );
  }

  function text_frame() {
    return [];
  }

  /** 与 wasm 宿主同名：把这一帧变成屏幕上的样子。 */
  function draw(frame) {
    currentFrame = frame;
    const layers = evaluateFrame(doc.timeline, frame);
    const alive = new Set(layers.map((layer) => layer.id));
    for (const [id, node] of elements) {
      const visible = alive.has(id);
      node.style.display = visible ? 'block' : 'none';
      if (!visible) node.style.opacity = '0';
    }
    // 宽高的一半当作元素中心，让它与"transform 是相对中心的位移"这条口径一致
    // （真正的锚点归 engine；这里是设计完整度期的近似，登记表里记着）。
    const css = [];
    for (const layer of layers) {
      const node = elements.get(layer.id);
      if (node === undefined) continue;
      node.style.opacity = String(layer.opacity);
      node.style.transform =
        'translate(-50%, -50%) translate(' + layer.x + 'px, ' + layer.y + 'px) rotate(' + layer.rotation + 'deg) scale(' +
        layer.scale + ')';
      // 特效 → CSS：只搬能证明等价的（口径在 web/dom-css.mjs 的模块头）。
      // 拒绝的那些**报出去**，由页面显示 —— 静默画一个「看起来差不多」的更坏。
      const source = layersById.get(layer.id);
      const effects = source === undefined || source.effects === undefined ? [] : source.effects;
      const filters = layerFilters(effects);
      const refused = filters.refused.slice();
      const keyed = (source === undefined || source.keyframes === undefined ? [] : source.keyframes)
        .some((key) => String(key.target).startsWith('effect.'));
      if (keyed) {
        refused.push({ kind: '(关键帧驱动)', why: '特效参数被关键帧驱动这一版还没实现：DOM 侧只应用静态参数' });
      }
      // 混合模式：CSS 有 8 条以上，而引擎只实现 4 条 —— 差额必须报出去，
      // 否则用户会以为「预览能看就等于能出片」。
      const source2 = layersById.get(layer.id);
      const source0 = source2;
      // 背景滤镜：CSS 里是 `backdrop-filter`，**滤镜函数本身与上面那套一模一样** ——
      // 所以直接复用同一张映射（口径 D5/D6 不需要重新论证一遍）。
      const backdrop = layerFilters(source0 === undefined ? [] : source0.backdrop_effects);
      node.style.backdropFilter = backdrop.css;
      // 投影也走 `filter`（CSS 里它就是 filter 的一个函数）—— 与特效那份拼在一起。
      const shadowFilter = layerShadowFilter(source2 === undefined ? undefined : source2.shadow);
      node.style.filter = [filters.css, shadowFilter].filter((part) => part !== '').join(' ');
      const blend = layerBlendMode(source2 === undefined ? undefined : source2.blend);
      node.style.mixBlendMode = blend.css;
      // 圆角：契约字段已经有了，DOM 侧先画出来（阶段 4：预览先看见）；
      // 但引擎那边还画不出来 —— 差额必须报出去。
      const corner = layerCornerRadius(source2 === undefined ? 0 : source2.corner_radius);
      node.style.borderRadius = corner.css;
      // 裁剪形状：与圆角**同时生效**（CSS 自己就会把两者一起作用，与引擎相乘覆盖度等价）。
      const clip = layerClip(source2 === undefined ? undefined : source2.clip);
      node.style.clipPath = clip.css;
      if (clip.refused !== undefined) refused.push(clip.refused);
      // 掩码：**取不到就不设**（浏览器取不到遮罩会把整个元素遮没，那比画不出掩码更坏）。
      const mask = layerMask(source2 === undefined ? undefined : source2.mask, Object.fromEntries(assetsById));
      node.style.maskImage = mask.css;
      node.style.webkitMaskImage = mask.css;
      if (mask.refused !== undefined) refused.push(mask.refused);
      for (const note of mask.notes === undefined ? [] : mask.notes) refused.push(note);
      if (corner.engine === 'missing' && corner.css !== '') {
        refused.push({ kind: 'corner_radius', why: corner.why });
      }
      if (blend.engine === 'missing') {
        refused.push({ kind: 'blend:' + String(source2.blend), why: blend.why });
      } else if (blend.refused !== undefined) {
        refused.push(blend.refused);
      } else if (blend.exactness === 'approximate') {
        css.push({ id: layer.id, css: 'mix-blend-mode: ' + blend.css, applied: [], refused: [{ kind: 'blend:add', why: blend.why }] });
      }
      if (filters.css !== '' || refused.length > 0) {
        css.push({ id: layer.id, css: filters.css, applied: filters.applied, refused });
      }
    }
    onValues({ frame, stage, layers, css });
    return layers;
  }

  function describe() {
    return { host: 'dom', interface: HOST_INTERFACE, stage, frame: currentFrame, layers: elements.size };
  }

  return { open, resize, sources_for, clear_bitmaps, set_bitmap, text_frame, draw, describe };
}
