// dhampir 预览引擎 —— **框架无关**。
//
// 这一层是「底座」的一部分，不是应用的一部分：
//   * 不 import 任何前端框架（React/Vue/…）——下游换框架不用改这里；
//   * 不写业务规则——校验走 Rust 的 dhampir_project_open，求值走 dhampir_project_frame，
//     渲染走 wasm 的时间线宿主。这里只做「调用 + 把异步的 DOM 舞蹈跳完」。
//
// # seek 为什么在这里
//
// <video> 的 seek 是异步的（set_current_time 立刻返回，那一帧还没解码），
// 而 Rust 侧的 SourceResolver 是同步接口。所以：先问「这一帧需要哪些源、各在第几秒」，
// 逐个 seek 并等 seeked，再让 Rust 画。异步留在 JS，Rust 保持同步。
//
// # 文字为什么也走这一步
//
// 「这一帧要画哪几行字、各落在目标像素的哪儿」是**共享算术**（core 的 evaluate_overlay
// 与契约层的 place_line），由宿主算好给出来（dhampir_project_text_frame）—— 这里不许
// 自己推一遍落点。字**形**像素则只有在浏览器里才拿得到，所以栅格化在这一层做：
// canvas 画一张图，createImageBitmap 之后交回宿主（dhampir_project_set_text_bitmap）。
// 两端允许不同的只有字形（CLI 走 ffmpeg drawtext，浏览器只有系统字体），
// 结构与落点必须同源 —— 判据见 plan/t2-evidence.md。

const hann = (resolve) => (event) => resolve(event);

/**
 * 画字的字体。**这一层没有"字体从哪来"的输入**，因为浏览器没有那种东西。
 *
 * CLI 那一侧是 ffmpeg drawtext，可以给 --font-file；于是两端的字形**本来就不同源**。
 * 判据里比的是"哪几行、各占哪个归一化矩形、墨迹在不在落点方框里"，不比字形。
 */
const TEXT_FONT = "sans-serif";

/**
 * **栅格化缓存：内容 -> 那张 canvas。**
 *
 * # 为什么缓存 canvas，而不是 `ImageBitmap`
 *
 * wasm 侧 `dhampir_project_set_text_bitmap` 在换掉旧位图时会 `previous.close()`。
 * 如果跨帧复用同一个 `ImageBitmap`，第二次提交时 `insert` 返回的 `previous`
 * **就是它自己** —— wasm 会把它 close 掉，于是拿到一张**已关闭的位图**。
 * canvas 是我们自己的东西，wasm 碰不到，所以缓存这一层是安全的；
 * 位图仍然**每帧新建**（`createImageBitmap(canvas)`），与契约完全兼容。
 *
 * # 省掉的是什么
 *
 * 省掉的是**字体渲染**（`fillText` / `measureText`，`textMs` 里的主要部分），
 * 而不是位图创建与上传。原生侧缓存的就是这一层 —— 出片报告里的
 * 「栅格化缓存命中 88 / 未命中 1」量的是同一件事（89 行里 88 行没重新栅格化）。
 *
 * # key 里为什么**不含位置**
 *
 * 位图内容与它贴在哪儿无关 —— 位置由每帧的 manifest 给（`placement.x/y`），
 * wasm 拿着它去摆。把位置算进 key 只会让"同一条字幕、位置每帧微动"永远不命中。
 */
const RASTER_CACHE = new Map();
const RASTER_CACHE_LIMIT = 512;

/** 影响像素的那些字段 —— 位置除外。 */
function rasterKey(line, style) {
  return [
    line.text,
    line.color,
    line.parts,
    line.font_px,
    line.scale,
    line.border_px,
    line.bitmap_width,
    line.bitmap_height,
    // **不透明度必须进 key**：淡入期间它逐帧在变，不进 key 就会一直复用
    // 第一帧那张（alpha 已烤死在位图里）—— 症状是"字幕不淡入、但也不报错"。
    line.opacity,
    style,
  ].map((v) => JSON.stringify(v === undefined ? null : v)).join("|");
}

/**
 * **栅格化入口（带缓存）** —— 调用点看起来与以前一字不差。
 *
 * 为什么做成"包一层"而不是把缓存塞进调用点：`createImageBitmap(rasterizeLine(…))`
 * 那个形状是 `check-overlay-plumbing.mjs` 的锚点（它要在 `createImageBitmap(` 附近
 * 同时看到 `rasterizeLine(` 与 `premultiplyAlpha: "none"`）。守卫守的是真东西 ——
 * "字形位图必须由 rasterizeLine 造、且必须声明直排 alpha" —— 所以**该改的是我**，
 * 不是它。包一层之后锚点原样还在，缓存也在。
 */
function rasterizeLine(line, style) {
  const key = rasterKey(line, style);
  let canvas = RASTER_CACHE.get(key);
  if (canvas === undefined) {
    canvas = rasterizeLineUncached(line, style);
    if (RASTER_CACHE.size >= RASTER_CACHE_LIMIT) RASTER_CACHE.clear();
    RASTER_CACHE.set(key, canvas);
  }
  return canvas;
}

/** `[r,g,b,a]`（各 0-255）-> canvas 认的颜色串。 */
function cssColor(rgba) {
  const parts = Array.isArray(rgba) && rgba.length >= 3 ? rgba : [255, 255, 255, 255];
  const alpha = parts.length > 3 ? parts[3] / 255 : 1;
  return "rgba(" + parts[0] + "," + parts[1] + "," + parts[2] + "," + alpha + ")";
}

/**
 * 把一行字栅格化成**落点声明的那个尺寸**。
 *
 * # 尺寸一个字都不多不少
 *
 * 位图尺寸与落点声明不符时宿主会拒绝画它（compose_overlay 拦下并计数），于是那一行
 * 静默消失。所以这里**不按设备像素比放大** —— 那是把预览尺寸与契约尺寸混在一起，
 * 而契约尺寸是算好的。
 *
 * # 为什么居中画就等于画在行盒上
 *
 * 位图宽 = 整条目标宽（见契约层 text_layout::bitmap_size），而落点的 x 已经把位图
 * 中心对准了行盒中心。所以「在位图里居中」与「落在行盒中心」是同一件事。
 *
 * # 描边与填色为什么不用再染一遍
 *
 * CLI 那一侧是白字 + 黑描边，再由 tint() 把白映射成 style.color、黑保持黑
 * （alpha 只随覆盖度走）。于是这里**直接照着那份结果落笔**：填 style.color、描黑边，
 * 而不是在 JS 里再写一遍逐像素公式（那就是第二份实现，迟早与 tint 漂开）。
 */
function rasterizeLineUncached(line, style) {
  const canvas = document.createElement("canvas");
  canvas.width = line.bitmap_width;
  canvas.height = line.bitmap_height;
  const ctx = canvas.getContext("2d");
  ctx.clearRect(0, 0, canvas.width, canvas.height);
  // **淡入淡出靠 alpha 烤进位图** —— 与 CLI 同一条思路（合成用的混合方程取的是
  // 位图自己的 alpha，`OverlayItem` 上没有 alpha 字段）。清单里的 `opacity`
  // 由宿主算好（`text_envelope`），这里只照用；不给就是 1。
  ctx.globalAlpha = (typeof line.opacity === "number" && line.opacity >= 0) ? line.opacity : 1;
  // **字体族与字重优先用契约给的**（`style.family` / `style.weight`）。
  //
  // 这条链以前是断的：契约里 `TextStyle.family` 有文档（"宿主按这个名字找，
  // 找不到要报出来"），但样式 JSON 不带它 → 这里只能回退到写死的 `sans-serif`，
  // 症状是**字号/位置/颜色都对、只有字形不对**。权重同理（参照实现 用 700/600，
  // canvas 不给就是 400）。`TEXT_FONT` 保留为**兜底**：契约没给时才用它。
  const family = style && style.family ? style.family : TEXT_FONT;
  const weight = style && style.weight ? style.weight : 400;
  ctx.font = weight + " " + line.font_px + "px " + family;
  ctx.textAlign = "center";
  ctx.textBaseline = "middle";
  ctx.lineJoin = "round";
  ctx.miterLimit = 2;
  const x = canvas.width / 2;
  const y = canvas.height / 2;
  // **描边宽度优先用契约给的**（`stroke_px`），0 才退回"从字号推"——
  // 与 CLI 侧 `text_raster.rs` 的同一条分支判据，两边必须一致，
  // 否则预览与成片的描边粗细不同（而那看起来像"字重不一样"）。
  //
  // **再乘这一条的缩放**（`line.scale`）：参照是 `swEff = sw * wrapped.scale`
  // —— 装不下时整体缩字号，描边要跟着缩。不乘的症状是"缩过的字幕描边特别粗"，
  // 而字号与行高都对，只有描边不对，很难一眼看出来。
  const scaled = (line.scale > 0 ? line.scale : 1);
  const strokePx = style.stroke_px > 0
    ? Math.round(style.stroke_px * scaled)
    : Math.round(line.border_px * scaled);

  // 这一套样式的文字阴影（`null` = 不画）。判据与 CLI 侧
  // `text_overlay::shadow_spec` **逐条相同**：颜色给了、且 alpha 不为 0。
  //
  // ⚠️ **不画阴影时这里绝不许碰 `shadow*` 那几个属性** —— canvas 的默认值本来就是
  // "没有阴影"，而把 `shadowBlur` 设成 0、`shadowColor` 设成透明也仍然会走一遍
  // 阴影路径，某些实现下像素会变（投影的合成方式与直接画不一样）。
  // "老工程逐字节不变"在这里就是"一个属性都不设"。
  const shadow = textShadow(style);

  // **有 `.hl` 分段时：逐段各画一次**（参照 `index.html:1985-1994` 就是这么画的）。
  //
  //     line.forEach(p => {
  //       ctx.strokeText(p.text, sx, ly);
  //       ctx.fillStyle = p.hl ? hlColor : color;
  //       ctx.fillText(p.text, sx, ly);
  //       sx += ctx.measureText(p.text).width;
  //     });
  //
  // **浏览器这边偏移是准的** —— `measureText` 与 `fillText` 是同一个引擎，
  // 与参照的条件完全一致。CLI 那边不是（`drawtext` 拿不到别段的宽），
  // 那一处的残差写在 `text_overlay.rs::paint_parts` 的注释里。
  if (Array.isArray(line.parts) && line.parts.length > 0) {
    const widths = line.parts.map((p) => ctx.measureText(p.text).width);
    const total = widths.reduce((a, b) => a + b, 0);
    let sx = x - total / 2;
    for (let i = 0; i < line.parts.length; i += 1) {
      const part = line.parts[i];
      // **先阴影、后文字** —— 与 CLI 侧同一个顺序（那边是先贴阴影位图）。
      if (shadow !== null) drawShadow(ctx, shadow, part.text, sx, y);
      if (style.outline === true && strokePx > 0) {
        ctx.lineWidth = strokePx * 2;
        ctx.strokeStyle = strokeColorCss(line, style);
        ctx.strokeText(part.text, sx, y);
      }
      ctx.fillStyle = cssColor(part.color && part.color.length === 4
        ? part.color
        : lineColor(line, style));
      ctx.fillText(part.text, sx, y);
      sx += widths[i];
    }
    return canvas;
  }

  if (shadow !== null) drawShadow(ctx, shadow, line.text, x, y);
  if (style.outline === true && strokePx > 0) {
    // 边宽取两倍：drawtext 的 borderw 是**向外**扩一圈，而 canvas 的描边压在字上。
    // 先描边后填字，内半边被字盖掉，剩下的外半边就是那一圈。
    ctx.lineWidth = strokePx * 2;
    ctx.strokeStyle = strokeColorCss(line, style);
    ctx.strokeText(line.text, x, y);
  }
  ctx.fillStyle = cssColor(lineColor(line, style));
  ctx.fillText(line.text, x, y);
  return canvas;
}

/**
 * 这一条该用什么颜色。
 *
 * **逐条优先**：`line.color` 是求值层算好的结果（cue 自带覆盖轨道默认），
 * 而 `style.color` 是轨道默认。只在 `line.color` 缺失时才回退 ——
 * 让 JS 自己判"该用哪一个"就是把同一条规矩放到第二处去实现。
 */
function lineColor(line, style) {
  return Array.isArray(line.color) ? line.color : style.color;
}

/**
 * 这一套样式里的**文字阴影**（`null` = 不画）。
 *
 * 判据与 CLI 侧 `text_overlay::shadow_spec` **逐条相同**：
 *   1. `shadow_color` 给了（是四元数组）；
 *   2. 它的 **alpha 不为 0** —— 全透明的阴影 = 看不见的阴影 = 不画。
 * 第二条不是洁癖：为它走一遍阴影路径会**改变像素**（见 `rasterizeLineUncached` 里
 * 那段"不画时一个属性都不设"），而画面上一个像素都不该多。
 *
 * 三个量都是**像素**（求值层已经把契约里的比例换好了），量纲与 `stroke_px` 一致。
 */
function textShadow(style) {
  if (!style || !Array.isArray(style.shadow_color)) return null;
  const color = style.shadow_color;
  if (color.length < 4 || color[3] === 0) return null;
  return {
    color,
    // `shadowBlur` 就是契约给的像素值：canvas 那边它已经等价于"σ 的两倍"，
    // 而 CLI 侧要把它除以 2 才是 `gblur` 的 σ（见 `text_raster::shadow_sigma_px`）
    // —— **那个 2 就是两端"观感近似"而不是逐像素一致的来源**。
    blur: typeof style.shadow_blur_px === "number" && style.shadow_blur_px > 0
      ? style.shadow_blur_px
      : 0,
    dx: typeof style.shadow_dx_px === "number" ? style.shadow_dx_px : 0,
    dy: typeof style.shadow_dy_px === "number" ? style.shadow_dy_px : 0,
  };
}

/**
 * 画一遍"只有阴影"的那一笔。**必须在文字之前调用**（影子在底下）。
 *
 * # 为什么用"填一遍阴影色 + 开阴影"这一招
 *
 * canvas 的阴影是**画一笔就投一次**：把 `shadow*` 装上再 `fillText`，
 * 落下来的东西有两样 —— 影子（按偏移、按模糊）**和这一笔自己**。
 * 于是这里填的颜色是**阴影色**：那一份"自己"与真正要画的字**完全重合**，
 * 随后被描边 + 填字压在下面。好处是这一笔**没有描边**（=`shadow` 只装着
 * 填充那一笔），与 CLI 侧"阴影只取填充轮廓、不参与描边宽度"是同一条口径。
 *
 * # 已知残差（两端同形，写下来不藏）
 *
 * 正文色**半透明**时，会从字里透出下面那一份阴影色的重合笔（CLI 侧也会 ——
 * 那边是模糊回卷过来的阴影），所以它是"影子在字下面"的正常表现，不是叠错。
 * 逐像素两边仍然不同：模糊核本来就不同。
 */
function drawShadow(ctx, shadow, text, x, y) {
  ctx.save();
  ctx.shadowColor = cssColor(shadow.color);
  ctx.shadowBlur = shadow.blur;
  ctx.shadowOffsetX = shadow.dx;
  ctx.shadowOffsetY = shadow.dy;
  ctx.fillStyle = cssColor(shadow.color);
  ctx.fillText(text, x, y);
  ctx.restore();
}

/**
 * 描边颜色：**契约给了就用契约的**，没给（`stroke_px == 0`，老工程）就用 `black`
 * —— 与 CLI 侧那条老路径同色。两边不一致的话，预览的描边颜色与成片不同，
 * 而那是"看起来只是有点脏"的一类错。
 */
function strokeColorCss(line, style) {
  if (style.stroke_px > 0 && Array.isArray(style.stroke_color)) {
    return cssColor(style.stroke_color);
  }
  return "black";
}

export class Engine {
  constructor(mod) {
    this.mod = mod;
    // source 标识 -> video 元素。**每个 source 一个元素**：
    // 同一帧上不同图层可能是不同源、不同源内帧，共用一个 <video> 是做不到的。
    this.videos = new Map();
    // 宿主持有的是**工程文件**（ProjectDoc，timeline 为 v2），不是裸契约。
    // 于是页面只认一种模型，而 v1 -> v2 的迁移只发生在 Rust 的 load_doc 一处。
    //
    // ⚠️ 字段不能叫 this.doc：**实例字段会盖住原型上的同名方法**，
    // 于是 engine.doc() 会变成 "engine.doc is not a function"。
    this.projectFile = null;
    this.attached = false;
    // 源从哪来：'video'（直接拿 <video> 去 copy）或 'bitmap'（JS 先转成位图）。
    // attach 时由 probeVideoCopy 决定，见那里的说明。
    this.sourceMode = "video";
    // 文字清单的**代**。行号是按位置编的，而栅格化是异步的 —— 两次 seek 交叠时，
    // 老的那一趟回来手上指的行可能已经是另一条字幕了（见 prepareText）。
    this.textToken = 0;
    // **"这一路、这一行、这份内容"有没有交过的记录**（字幕一份、弹幕一份）。
    //
    // 用途只有一个：决定要不要**跳过提交** —— 跳过了，wasm 那边就会复用上一帧那张
    // 已经上传好的纹理（省掉 create_texture + 重传，实测这段从 3.3ms 降到 0.2ms）。
    //
    // ⚠️ 判据必须带上**行号**。只按内容判会出错：栅格化缓存是全内容的，而 wasm 是按
    // 行号取纹理的 —— "上一帧 [A,B]、这一帧只剩 [B]"时，第 0 行会被误判成没变，
    // 于是复用第 0 行那张 A 的纹理。`count` 记着上一帧的行数，行数一变整份作废。
    this.textMemo = { keys: new Map(), count: -1 };
    this.danmakuMemo = { keys: new Map(), count: -1 };
    // 最近一次**算好并交给宿主**的清单（`prepareText` 里写的）。
    //
    // 判定通道读它，而不是再调一次 `textFrame`：那会重算一份新清单并把刚提交的位图
    // 全部作废（宿主那边清单与位图是一起作废的），于是 probe 判的是"没有位图的清单"。
    // 交互路径不读它。
    this.textManifest = null;
    // **阶段 4 的"代"**：内容标识里带上它，于是任何"这一路的画面可能换了一份"的事
    // 都会让标识整体变掉 ⇒ 宿主必然重新上传，不会拿旧纹理顶新内容。
    //
    // # 为什么必须要有这一代
    //
    // `open()` 一份新工程时，source 标识与素材帧号**都可能与旧的逐字相同**
    // （同一个 `a.mp4`、同一个 `source_frame`），而内容完全是另一份片子。
    // 少了这一代，宿主的复用判据会把"新工程的第 30 帧"认成"旧工程的第 30 帧"，
    // 于是画面停在上一份片上 —— 而画面**看起来完全正常**，只是"慢了半拍"。
    //
    // 旧口径没有这个责任（那时每帧无条件 `clear_bitmaps`），它是阶段 4 **新引入**的：
    // 一旦开始"跨帧留着"，"什么变了"就必须有人回答。所以这一代不是一个保险，
    // 它是那条判据的一部分。
    this.planGeneration = 0;
    // **阶段 5：单飞队列。** 同一时刻只允许**一条**"这一帧的管线"在跑，后到的排队。
    //
    // # 为什么必须有它（不是优化，是防 panic）
    //
    // 上一轮在这里出过事：同一帧被排了两次续跑、或 pre-roll 与正常 seek 并发 ⇒
    // wasm 侧的 `clear_bitmaps` 重入 ⇒ `RefCell already borrowed` panic。
    // 而 wasm 里的 panic 抓不住，**一次 panic 会把那个 `RefCell` 永久借住** ——
    // 之后任何调用都 panic，页面上只剩"启动失败：unreachable executed"。
    //
    // 所以并发这件事在这里被**结构性地**挡掉：不是靠"小心一点"，也不是靠 RefCell 的借用。
    // 用队列而不是"忙就拒绝"：拒绝会让播放中那一帧被丢掉（症状是画面卡一下），
    // 排队只让它晚一点。
    this.pipeline = Promise.resolve();
    // **阶段 5：预渲染缓存的开关** —— 默认**关**。
    //
    // 打开它每帧要多一次"自有中间纹理 + blit 到画布"（那是白花的钱），只有在
    // "同一帧被要第二次"真的发生时才赚得回来 —— 而这件事由宿主的排帧方式决定，
    // 不是这一层能假设的。所以默认关，由宿主显式打开（`setPreviewCache` / `?cache=1`）。
    //
    // 开关与窗口都**由这一侧给**：wasm 跑在 `wasm32-unknown-unknown` 上，
    // 那一侧没有环境变量（`std::env::var` 恒返回 `Err`，读它等于读死代码）。
    this.previewCache = { enabled: false, seconds: [3, 2], maxMb: 256 };
    /** 待提前渲染的帧（同一个槽位只留最新的一帧）。 */
    this.prerollPending = null;
    /** 提前渲染的预算（毫秒）。宿主给；`null` = 不自动跑（只提供 `schedulePreroll`）。 */
    this.prerollBudgetMs = null;
  }

  /**
   * **打开 / 关掉预渲染缓存**（阶段 5）。返回上一个开关状态。
   *
   * # 为什么把它放在这一层、而不是读环境变量
   *
   * 配置被读的那一侧（wasm）**没有环境** —— `wasm32-unknown-unknown` 上
   * `std::env::var` 恒返回 `Err`。所以"用 `VTEDIT_PREVIEW_CACHE*` 调"是死代码。
   * 能看见配置的只有 JS，于是开关也在这里。
   *
   * # 关掉是**完全**关掉
   *
   * 关掉时下一帧的 `begin_frame` **不带 `cache` 字段** ⇒ 宿主直接丢掉整张表、
   * 回到"合成直接落在画布上"的旧路（不多一次中间纹理 + blit）。
   * 也就是说这个开关的**关**态与阶段 5 之前逐字节相同。
   *
   * @param {boolean} enabled
   * @param {{seconds?: number[], maxMb?: number}} [options] 窗口（前/后秒数）与内存上限
   */
  setPreviewCache(enabled, options = {}) {
    const previous = this.previewCache.enabled === true;
    const seconds = Array.isArray(options.seconds) ? options.seconds : [3, 2];
    const forward = Number(seconds[0]) > 0 ? Number(seconds[0]) : 3;
    const back = Number(seconds[1]) > 0 ? Number(seconds[1]) : 2;
    const maxMb = Number(options.maxMb);
    this.previewCache = {
      enabled: enabled === true,
      seconds: [forward, back],
      maxMb: maxMb > 0 ? maxMb : 256,
    };
    if (enabled !== true) {
      // 关掉时把待渲染的也丢掉 —— 留着会在关掉之后又跑一帧预热，那没有意义。
      this.prerollPending = null;
    }
    return previous;
  }

  /**
   * 换了一份"这一路的画面"就代 +1（见 {@link Engine#planGeneration} 的说明）。
   *
   * 调用点只放**真的可能改掉源画面**的那些：换工程（open / edit / undo / redo）、
   * 给 source 换 video 元素、换源模式。**不放**的：拖动播放头、resize
   * （前者不改内容，后者只改落点体系、由 wasm 那边的尺寸判据管）。
   */
  bumpPlanGeneration() {
    this.planGeneration += 1;
  }

  /**
   * 这个浏览器的 WebGPU 接不接受 <video> 作为 copy_external_image_to_texture 的源？
   *
   * # 为什么要"问"而不是"试"
   *
   * 不支持的时候，wgpu 会把浏览器抛的那个 TypeError **unwrap 成 panic**，
   * 而 wasm 的 panic 抓不住 —— 整个模块死掉，页面上只剩一句
   * "启动失败：unreachable executed"，完全看不出跟素材有关。
   * 所以这里**绕开 wgpu**，直接用浏览器的 WebGPU API 问一句，代价是一次
   * requestAdapter + requestDevice。
   *
   * 判据：联合类型不接受时错误消息里有 "could not be converted"；
   * 接受但内容不可用报的是另一种错（源没有数据），那说明类型这一关已经过了。
   *
   * ⚠️ 这个探测会在浏览器控制台留一条 "Browser fails extracting valid resource
   * from external image" 的警告 —— **那是它故意的**：拿一个空 <video> 去试类型，
   * 内容当然不可用。看到那条警告不代表出错。
   */
  static async probeVideoCopy() {
    if (typeof navigator === "undefined" || !navigator.gpu) return false;
    try {
      const adapter = await navigator.gpu.requestAdapter();
      if (adapter === null) return false;
      const device = await adapter.requestDevice();
      const video = document.createElement("video");
      const texture = device.createTexture({
        size: [2, 2, 1],
        format: "rgba8unorm",
        usage: GPUTextureUsage.COPY_DST | GPUTextureUsage.RENDER_ATTACHMENT,
      });
      try {
        device.queue.copyExternalImageToTexture({ source: video }, { texture: texture }, [2, 2]);
        return true;
      } catch (error) {
        const message = String(error && error.message ? error.message : error);
        return !/could not be converted/i.test(message);
      }
    } catch (error) {
      return false;
    }
  }

  /** 载入并校验一份工程。失败时 Rust 侧**保留上一份可用工程**（UI 不该因为一次非法编辑就崩）。 */
  open(project) {
    const json = typeof project === "string" ? project : JSON.stringify(project);
    const result = JSON.parse(this.mod.dhampir_project_open(json));
    if (result.parsed === true && result.ok === true) {
      // 向 Rust 要一份**规范化**的工程文件，而不是把输入原样存下来 ——
      // 输入可能是不带资产表的裸契约，那样页面读不到 assets。
      this.projectFile = JSON.parse(this.mod.dhampir_project_doc());
      // 换了一份工程 ⇒ 源标识与素材帧号可能一字不差而内容不同，标识必须整体作废。
      this.bumpPlanGeneration();
    }
    return result;
  }

  /** 当前工程的规范化副本（含 assets / view / render_hints）。没载入过是 null。 */
  doc() {
    return this.projectFile;
  }
  /**
   * **把工程用到的掩码图交给宿主**（浏览器那条腿的掩码通路）。
   *
   * 为什么是**独立一步**而不是塞进 `open()`：`open` 是同步的，而取图要 await。
   * 调用方在 `open` 之后 await 这一条即可（与源位图那条链分开：掩码只在换工程时上传一次，不逐帧）。
   *
   * 取不到的掩码**不在这里报错**，而是留给渲染时那条**响亮**的拒绝 ——
   * `draw` 会因为 `missing_mask_assets` 非空而报错，说清楚是哪几个 asset。
   * 那比"静默画一张没有掩码的图"好，也比在这里静默失败好。
   *
   * @returns {Promise<string[]>} 上传成功的 asset id
   */
  async uploadMasks(bitmaps = null) {
    const doc = this.projectFile;
    const assets = doc !== null && Array.isArray(doc.assets) ? doc.assets : [];
    const tracks = doc !== null && doc.timeline !== undefined && Array.isArray(doc.timeline.tracks)
      ? doc.timeline.tracks
      : [];
    const wanted = new Set();
    for (const track of tracks) {
      for (const layer of Array.isArray(track.layers) ? track.layers : []) {
        const mask = layer.mask;
        // 渐变遮罩是程序化生成的，不需要素材 —— 只认 asset_id 那种。
        if (mask !== undefined && mask !== null && typeof mask.asset_id === "string" && mask.asset_id !== "") {
          wanted.add(mask.asset_id);
        }
      }
    }
    const uploaded = [];
    for (const id of wanted) {
      const asset = assets.find((entry) => entry !== null && entry !== undefined && entry.id === id);
      const uri = asset !== undefined && typeof asset.uri === "string" ? asset.uri : "";
      if (uri === "") {
        console.warn("dhampir: 掩码 " + id + " 没有 uri，取不到");
        continue;
      }
      try {
        // 优先用调用方给的位图（file:// 与无网场景下 fetch 会被拒）。
        const given = bitmaps !== null && bitmaps[id] !== undefined ? bitmaps[id] : null;
        const bitmap = given !== null ? given : await createImageBitmap(await (await fetch(uri)).blob());
        this.mod.dhampir_project_set_mask_image(id, bitmap);
        uploaded.push(id);
      } catch (error) {
        console.warn("dhampir: 掩码 " + id + " 取不到或解不开：" + error);
      }
    }
    return uploaded;
  }

  /**
   * 执行一次编辑操作。**规则在 Rust**（dhampir-timeline::edit）——
   * 剃刀、修剪、波纹删除、序列设置都只有那一份实现，CLI 与这里调的是同一个函数。
   *
   * 失败时宿主里那份**一个字都没变**，所以这里也不刷新本地副本。
   */
  edit(op) {
    const result = JSON.parse(this.mod.dhampir_project_edit(JSON.stringify(op)));
    if (result.ok === true) {
      this.projectFile = JSON.parse(this.mod.dhampir_project_doc());
      // 编辑会改掉图层与素材区间 ⇒ 同一个素材帧号可能变成另一幅画面。
      this.bumpPlanGeneration();
    }
    return result;
  }

  /**
   * 撤销 / 重做一步。栈本身在 Rust（dhampir-timeline::history），
   * 这里只是把宿主里那份工程按栈顶换掉，**规则不在 JS**。
   *
   * 退不动时宿主里那份一个字都没变，返回体的 `issues` 里带 `nothing_to_undo`
   * / `nothing_to_redo`——所以这里可以无条件取回工程副本。
   */
  undo() {
    const result = JSON.parse(this.mod.dhampir_project_undo());
    if (result.ok === true) {
      this.projectFile = JSON.parse(this.mod.dhampir_project_doc());
      // 撤回/重做换的是另一份时间线 ⇒ 同上的理由。
      this.bumpPlanGeneration();
    }
    return result;
  }

  redo() {
    const result = JSON.parse(this.mod.dhampir_project_redo());
    if (result.ok === true) {
      this.projectFile = JSON.parse(this.mod.dhampir_project_doc());
      this.bumpPlanGeneration();
    }
    return result;
  }

  /** 重新校验当前工程但不改变它——编辑过程中用来显示问题。 */
  validate(project) {
    const json = typeof project === "string" ? project : JSON.stringify(project);
    return JSON.parse(this.mod.dhampir_project_open(json));
  }

  endFrame() { return this.mod.dhampir_project_end_frame(); }
  firstFrame() { return this.mod.dhampir_project_first_frame(); }

  /** 这一帧的图层清单（纯数据），给时间线与调试用。 */
  evaluate(frame) {
    return JSON.parse(this.mod.dhampir_project_frame(frame));
  }

  /** 建预览宿主。canvas 尺寸决定预览尺寸——契约里没有分辨率字段。 */
  async attach(canvasId) {
    const info = JSON.parse(await this.mod.dhampir_project_attach(canvasId));
    this.attached = true;
    // 源模式：**默认 bitmap**；`?src=video` / `?src=bitmap` 可以强制。
    //
    // ⚠️ 以前默认是"问浏览器"（`probeVideoCopy()`），但那个探测**不可靠**，
    // 而且判错的方式最坏：它拿一个**空 `<video>`** 去试 `copyExternalImageToTexture`，
    // 类型这一关过了就算"支持"（注释也写着"接受但内容不可用是另一种错"）——
    // 可是**类型过关 ≠ 真能画出画面**。在软件 / 回退适配器上（`device_type: Other`），
    // 直传常常**静默产出空内容**：不报错、不 panic，**只是黑屏**。
    // 用户实测：切进 Dhampir 一按播放就黑屏，而工具条上写着 `· video`。
    //
    // 所以默认走 bitmap：每帧 `createImageBitmap` + 上传，慢一点（软件适配器上十几毫秒），
    // 但**一定画得出来**。video 直传留作显式开关，用来比较两条路的画面与速度
    // （两条路的输出应当逐字节相同）。
    const override = new URLSearchParams(location.search).get("src");
 // 默认 video（零拷贝）✓ —— 用户要求：全都用 video，不做 bitmap 自动 fallback ✓
    // 只保留 ?src=bitmap 这一个**人工**旁路（排查用），不参与任何自动逻辑 ✓
    if (override === "bitmap") {
      this.sourceMode = override;
    } else {
      // 用户要求：**默认就用 video（零拷贝）** ✓
      this.sourceMode = "video";
    }
    this.mod.dhampir_project_set_bitmap_mode(this.sourceMode === "bitmap");
    // **阶段 5：预渲染缓存的开关也可以从 URL 给**（`?cache=1`），与 `?src=` 同一个口径 ——
    // 它让"不改宿主代码就能量一次命中收益"成为可能。默认关。
    const query = new URLSearchParams(location.search);
    if (query.get("cache") === "1") {
      const parts = (query.get("cacheSeconds") || "3,2").split(",").map(Number);
      const mb = Number(query.get("cacheMb"));
      this.setPreviewCache(true, {
        seconds: [parts[0], parts[1]],
        maxMb: mb > 0 ? mb : 256,
      });
    }
    this.resetTextMemos();
    // 换了一次 attach（含换 canvas / 重建宿主）⇒ 标识整体作废。
    this.bumpPlanGeneration();
    return info;
  }

  /**
   * 交一张**静态位图**给某个 source —— 贴纸 / 图片序列这类**没有 `<video>`** 的源。
   *
   * # 为什么必须由宿主交
   *
   * `prepare` 只会对 `this.videos` 里的元素做 `createImageBitmap`，所以"没有视频元素"的源
   * 它一个位图都不会生成 —— 那一层就**静默少画**（画面看起来正常，只是少了几层贴纸）。
   * wasm 侧的契约写得很清楚：`dhampir_project_set_bitmap(source, bitmap)` 就是给这种情况准备的
   * （"JS 把某个 source **当前帧**转成位图交给宿主"）。
   *
   * # 时序
   *
   * 契约要求"**seek 完成之后**交"（早了拿到的是上一帧）。而 `seek` 内部先 `clear_bitmaps()`
   * 再 `draw()`，所以宿主在 `seek` **之后**交的位图会在**下一帧**被画出来 —— 晚一帧（16ms），
   * 人眼看不出；要同帧就得拆开 `prepare`/`draw`，那属于后续优化。
   *
   * 传进来的位图**不要自己 close()**：宿主换新的时候会 `previous.close()`（契约如此）。
   */
  setSourceBitmap(source, bitmap) {
    this.mod.dhampir_project_set_bitmap(source, bitmap);
  }

  /**
   * **明确指定源模式**（`"video"` 零拷贝 / `"bitmap"` 每帧 createImageBitmap）。
   *
   * # 为什么需要这个入口
   *
   * `attach()` 的自动判定靠 `probeVideoCopy()`，而那个探针拿的是
   * `document.createElement("video")` —— **一个没有 `src`、`readyState === 0` 的空元素**。
   * 于是它量到的不是"这块设备能不能把 video 拷进纹理"，而是"空 video 抛哪个异常"：
   * 抛 `could not be converted` 就判 false，抛别的（如 `InvalidStateError`）就判 **true**。
   *
   * 实测（同一份 1080p60 工程、同一个引擎、同一台机器）：
   *
   * | 环境 | 探针结果 | 每帧 | 慢 seek |
   * |---|---|---|---|
   * | headless Chrome（本仓的测量脚本） | `video` | 4~6 ms | 0 |
   * | 真窗口 Chrome（宿主页面） | `bitmap` | **191 ms** | **41** |
   *
   * **同一个探针在两个环境给出不同答案。** 与其猜，不如让宿主能明确指定。
   *
   * # ⚠️ 但实测结论是：`"video"` 这条路在浏览器里**走不通**
   *
   * 宿主页面上抓到过真堆栈：
   *
   * ```text
   * panicked at wgpu-30.0.1/src/backend/webgpu.rs:2835:14:
   *   called `Result::unwrap()` on an `Err` value:
   *     JsValue(TypeError: GPUQueue.copyExternalImageToTexture: 'source' member of
   *     GPUCopyExternalImageSourceInfo could not be converted to any of:
   *     ImageBitmap, HTMLImageElement, HTMLCanvasElement, OffscreenCanvas)
   * ```
   *
   * **那张清单里没有 `<video>`** —— 不是设备的怪癖，是 WebGPU 的输入类型就不含视频元素。
   * 所以 `"video"` 模式下**第一次 draw 必 panic**；而且**一次 panic 会把 wasm 侧的
   * `ProjectHost`（一个 `RefCell`）永久借住**，之后任何调用都 `RefCell already borrowed`
   * —— 也就是说"失败后退回 bitmap"这种兜底**同样救不回来**（回退调用自己就 panic）。
   *
   * 结论：**宿主不要指定 `"video"`**；让 `attach()` 的探针决定。`"bitmap"` 是浏览器里
   * 唯一能走通的路（每帧 `createImageBitmap`），它的代价主要落在 GPU 上传上 ——
   * 在软件适配器上实测 ~190ms/帧、在真适配器上 4~6ms。
   *
   * @param {"video"|"bitmap"} mode
   * @returns {string} 上一个模式（方便调用方记账/回退）
   */
  /**
   * **跟随播放**（默认 `false`）。
   *
   * 打开后：`prepare` 发现某个 `<video>` 正在播、且与目标时刻的偏差在
   * {@link Engine#followTolerance} 以内时，**不 seek**，直接取它正在显示的帧。
   *
   * 适用：**预览**（人眼看片 —— 视频按 1× 播，取当前帧就是对的）。
   * 不适用：**帧精确**的任何用途（出片、双端比对、逐帧导出）—— 那些必须逐帧 seek。
   * 所以这是**宿主显式选开**的开关，默认关。
   *
   * 背景：长 GOP 素材上"每帧 seek"是灾难 —— 实测一份 1080p60 素材 GOP > 4 秒，
   * ffmpeg 一次 seek 要 150~183ms，浏览器 212ms/帧（5fps）。
   */
  followPlayback = false;

  /** 跟随播放时允许的偏差（秒）。默认 1/30 —— 一帧级偏差，人眼看不出。 */
  followTolerance = 1 / 30;

  setSourceMode(mode) {
    const next = mode === "bitmap" ? "bitmap" : "video";
    const previous = this.sourceMode;
    this.sourceMode = next;
    // 与 `attach()` 里同一件事：契约侧要跟着切（bitmap 模式每帧会先 clear_bitmaps）。
    this.mod.dhampir_project_set_bitmap_mode(next === "bitmap");
    // 换了源模式 ⇒ 宿主手上那份驻留纹理的来源口径变了，标识整体作废。
    this.bumpPlanGeneration();
    return previous;
  }

  /** 把一个 source 绑到一个 video 元素上。 */
  bindSource(source, videoId) {
    this.mod.dhampir_project_bind_source(source, videoId);
    const video = document.getElementById(videoId);
    if (video !== null) this.videos.set(source, video);
    // 同一个 source 换了元素 ⇒ 同一个素材帧号已经是另一幅画面，标识必须作废。
    this.bumpPlanGeneration();
  }

  /**
   * **有界等待**一段视频可被 `createImageBitmap` 取帧（或超时）。
   *
   * 为什么必须有：`createImageBitmap(<video>)` 在 `readyState < 2` 或正在 `seeking`
   * 时会抛 `InvalidStateError: Passed-in video does not have enough data` ——
   * 而那一帧**整个视频层不画**（症状是黑一下，或"贴纸/字幕都没了"）。
   * 跳转、骑过手动屏蔽区间、刚起播都会撞上，真工程里能稳定复现。
   *
   * 所以这里等 `seeked` / `loadeddata` / `canplay`，**只在这些事件真的把视频变成可读时
   * 才提前结束**，否则等满超时再交给调用方去处理失败 —— 宁可慢这一帧，
   * 也不要交一帧没有画面的。
   */
  static waitVideoReady(video, timeoutMs) {
    const ok = () => video.readyState >= 2 && video.seeking !== true;
    if (ok()) return Promise.resolve(true);
    return new Promise((resolve) => {
      let done = false;
      const finish = (value) => {
        if (done) return;
        done = true;
        video.removeEventListener("seeked", onEvent);
        video.removeEventListener("loadeddata", onEvent);
        video.removeEventListener("canplay", onEvent);
        clearTimeout(timer);
        resolve(value);
      };
      const onEvent = () => { if (ok()) finish(true); };
      const timer = setTimeout(() => finish(ok()), timeoutMs);
      video.addEventListener("seeked", onEvent);
      video.addEventListener("loadeddata", onEvent);
      video.addEventListener("canplay", onEvent);
    });
  }

  /** 这一帧需要哪些源、各停在**第几秒**。秒数由整数帧号经时间基换算。 */
  sourcesFor(frame) {
    return JSON.parse(this.mod.dhampir_project_sources_for(frame)).sources || [];
  }

  /**
   * 取回并登记这份工程的字幕素材。
   *
   * `entries` 是 `[{assetId, url, format}]`：**「素材在哪」不由这里回答** ——
   * 那是部署形态（backend 的 mediaUrlFor），页面把它拼好递进来。
   * `format` 是扩展名（"srt"/"ass"/"ssa"）：CLI 认的就是文件名后缀，
   * 让浏览器按内容嗅探就会多出第二套判定。
   *
   * 「这段文本是哪几条字幕」由 Rust 回答（parse_srt / parse_ass）—— 与 CLI 的
   * load_subtitles 是同一份实现。这里只做一次 fetch。
   *
   * 取是网络、登记是同步的 wasm 调用：所以**先把所有请求一起起起来**再逐个登记，
   * 而不是逐个 await 取（那会白等一个来回）。
   *
   * 一路坏了**不抛**：那一份字幕就登记不上，宿主会在每一帧报 `subtitle_unregistered`。
   * 「这部片子没有字幕」与「字幕没读进来」的输出必须不一样 —— 这里正是那个分岔点。
   */
  /**
   * **把"某一路这一行交过什么"的记录清掉** —— 下一帧会全部重交一遍。
   *
   * 必须和 wasm 侧对齐：`dhampir_project_set_subtitles` 与 `dhampir_project_resize`
   * 会调 `invalidate_text_uploads()`，把已上传的纹理连同脏标记一起扔掉。
   * 宿主如果还记着"这一行交过"，下一帧就会跳过提交 —— 而 wasm 手上已经没有那张纹理了，
   * 结果是**那一行永久不显示**（换字幕/改画布尺寸之后立刻就会看到）。
   *
   * 画布尺寸变化会让 key 自己变（位图宽高在 key 里），所以那条路本来不会漏；
   * 但"重新登记同一份字幕"的 key 与旧的一样，**非清不可**。
   */
  resetTextMemos() {
    this.textMemo = { keys: new Map(), count: -1 };
    this.danmakuMemo = { keys: new Map(), count: -1 };
  }

  async loadSubtitles(entries) {
    // 字幕换了 -> wasm 侧已经丢弃已上传的纹理，宿主这边也得忘掉"交过什么"。
    this.resetTextMemos();
    const list = Array.isArray(entries) ? entries : [];
    const fetched = await Promise.all(list.map(async (entry) => {
      try {
        const response = await fetch(entry.url);
        if (response.ok !== true) return { entry: entry, error: "HTTP " + response.status + " " + entry.url };
        return { entry: entry, text: await response.text() };
      } catch (error) {
        return { entry: entry, error: String(error && error.message ? error.message : error) };
      }
    }));
    const registered = [];
    const failed = [];
    for (const item of fetched) {
      if (item.text === undefined) {
        failed.push({ assetId: item.entry.assetId, error: item.error });
        continue;
      }
      const result = JSON.parse(this.mod.dhampir_project_set_subtitles(item.entry.assetId, item.text, item.entry.format));
      if (result.ok === true) registered.push(result);
      else failed.push({ assetId: item.entry.assetId, error: "解析失败：" + JSON.stringify(result.issues) });
    }
    return { registered: registered, failed: failed };
  }

  /**
   * 这一帧要画哪几行字、各落在哪（**目标像素**）。
   *
   * 清单由宿主给：它同时把宿主手上的清单换成这一帧的，并作废上一帧的行位图
   * （行号是按位置编的，留着旧位图就会拿另一条字幕的像素去贴）。
   * 于是**必须在 draw 之前、就在这一帧上调一次**。
   */
  textFrame(frame) {
    return JSON.parse(this.mod.dhampir_project_text_frame(frame));
  }

  /**
   * 判定入口：把「加字之前 / 加字之后 / 逐行减掉一行」三张图比一比（T2.5）。
   *
   * 判定本身在 Rust —— 这里只是通道。在 JS 里重写一遍判据，两端就会各自演化，
   * 而判据漂开的症状是"两边都自洽、只是结论不同"。
   */
  async textProbe(frame) {
    return JSON.parse(await this.mod.dhampir_project_text_probe(frame));
  }

  /**
   * 把一个 video 定位到指定秒数，等它真的 seek 完。
   *
   * # 为什么先比一次 currentTime（这是预览帧率的关键）
   *
   * 浏览器对**相同的 currentTime** 不会重新解码（实测 0.3ms），但**也不一定派 seeked**
   * —— 于是"设一次、等事件"的写法在值没变时会一直等到兜底超时，白白卡住每一帧。
   *
   * 实测（headless Chrome，640x360 素材）：
   *   * 同值 seek 5 次： 11.2ms（约 2.2ms/次）
   *   * 异值 seek 5 次： 51.8ms（约 10.4ms/次）
   * 而逐帧 seek 的总耗时里 **12–30ms 全在这一步**（sourcesFor / text / draw 合计不到 2ms）。
   * 所以"值没变就直接返回"不是微优化，它决定预览能不能跑到序列帧率。
   *
   * # 兜底定时器为什么必须清
   *
   * 原来每次调用都挂一个 3000ms 的 setTimeout，**用掉了也不清**。播放时一秒 30 帧
   * 就是 30 个挂着的定时器，它们到期后各调一次 done（done 已 removeEventListener，
   * 重复调用无害，但定时器本身一直占着）。清掉它，顺带让 `done` 幂等。
   */
  async seekVideo(video, seconds) {
    // **相同值直接返回**：浏览器不会重新解码，seeked 也未必派发。
    // 用严格相等：currentTime 是双精度，同一个算式重复算出的位模式相同。
    if (video.currentTime === seconds && video.readyState >= 2) {
      if (this.timing) this.lastSeekVideo = { hit: true, deltaMs: 0 };
      return null;
    }
    if (this.timing) {
      this.lastSeekVideo = {
        hit: false,
        // 差值本身是关键事实：**只差一点点**说明短路判据太严，
        // 于是每帧都在做一次完整的 seek，而画面看起来完全正常（只是慢）。
        deltaMs: Math.abs(video.currentTime - seconds) * 1000,
        from: video.currentTime,
        to: seconds,
      };
    }
    return new Promise((resolve) => {
      let settled = false;
      let timer = 0;
      const done = (event) => {
        if (settled) return;
        settled = true;
        video.removeEventListener("seeked", done);
        // **必须清**：不清就是每帧一个 3 秒定时器（见上面那段）。
        if (timer !== 0) clearTimeout(timer);
        resolve(event);
      };
      video.addEventListener("seeked", done);
      try {
        video.currentTime = seconds;
      } catch (error) {
        // 设不进去（还没拿到元数据等）也要收尾，不能让这一帧永远挂着。
        done(null);
        return;
      }
      // 兜底：seek 到同一位置时浏览器可能不派 seeked，不能让预览卡死。
      timer = setTimeout(done, 3000);
    });
  }

  /**
   * **这一帧的来源集合与内容标识**（阶段 4）—— 交给宿主决定哪些位图可以留着。
   *
   * # 标识是什么、为什么是它
   *
   * 标识 = **素材帧号**（`entry.source_frame`）。贴纸（GIF / 图片序列）铺在时间线上时
   * 一个素材帧通常要撑好几个时间线帧；素材帧号没变 ⇒ 那一层的内容逐像素相同 ⇒
   * 宿主可以留着上一帧上传好的纹理，于是 `createImageBitmap` 与那次 GPU 上传**一起省掉**。
   *
   * 判据只能是**调用方给的**：宿主按位图对象 / 尺寸 / 时间戳猜，都会在"新的一帧恰好
   * 长得一样"时把"变了"判成"没变" —— 症状是画面停在上一帧的贴纸上，
   * 看起来完全正常，只是"慢了半拍"。
   *
   * # 什么时候**不给**标识（= 全部照旧重交）
   *
   * `followPlayback`（宿主选开、默认关）下视频自己在往前走 ⇒ 同一个素材帧号对应的
   * **不是**同一幅画面，标识不成立。那时一律不给标识，行为与改前逐字节相同。
   *
   * # 没有新导出时
   *
   * 回落到 `clear_bitmaps()` —— **逐字节就是改前的行为**。产物里 wasm 与 engine.js 是
   * 两个文件，版本错配（旧 wasm + 新 engine.js）时必须退化成旧行为，
   * 而不是抛一句 `dhampir_project_begin_frame is not a function`。
   *
   * @returns {{need: string[], reuse: string[], dropped: number}|null} `null` = 没有复用信息
   */
  beginFramePlan(frame, sources) {
    if (typeof this.mod.dhampir_project_begin_frame !== "function") {
      // 旧 wasm：走的就是改前那一条路，一个字节都没变。
      this.mod.dhampir_project_clear_bitmaps();
      return null;
    }
    // **标识里带"代"**：见 `planGeneration` —— 换工程 / 换元素之后，同一个素材帧号
    // 可能已经是另一幅画面，标识必须整体变掉，否则宿主会拿旧纹理顶新内容。
    const generation = "g" + this.planGeneration + ":";
    const pin = this.followPlayback !== true;
    const payload = { frame: frame, sources: [] };
    for (const entry of sources) {
      if (pin && typeof entry.source_frame === "number") {
        payload.sources.push({ source: entry.source, id: generation + entry.source_frame });
      } else {
        // 裸字符串 = **没有标识** ⇒ 这一项按"必须重交"处理（宿主不许猜）。
        payload.sources.push(entry.source);
      }
    }
    // **阶段 5：缓存开着才带 `cache`**。不带 = 宿主丢掉整张表、走旧路
    // （"默认关"在协议上就是这个意思，不需要第二个开关）。
    if (this.previewCache.enabled === true) {
      payload.cache = {
        enabled: true,
        seconds: this.previewCache.seconds,
        max_mb: this.previewCache.maxMb,
      };
    }
    let plan = null;
    try {
      plan = JSON.parse(this.mod.dhampir_project_begin_frame(JSON.stringify(payload)));
    } catch (error) {
      // 解析不了也走下面的回落 —— 宁可这一帧慢，也不要留着一帧不知道来历的位图。
      plan = null;
    }
    if (plan === null || plan.ok !== true) {
      // 入参形状不对时宿主**一个字段都没改**（解析发生在动宿主状态之前），
      // 所以这里退回旧行为是安全的；不退的话上一帧的位图会留在宿主里被继续画。
      this.mod.dhampir_project_clear_bitmaps();
      return null;
    }
    return plan;
  }

  /**
   * 把某一帧所需的源全部 seek 到位（位图模式下顺带把位图做好）。
   *
   * # 多路 seek 为什么是**串行**的（这是一个试过并证伪的优化）
   *
   * 一眼看去串行 await 是浪费：每一路有自己的 `<video>` 元素、互不共享状态，
   * 一帧要 n 路就要等 n 份解码时间之和，改成 `Promise.all` 就该只剩最慢那一路。
   *
   * **实测不是这样。** 受控对照（同一组 seek 位置、各自先回到同一起点）：
   *
   * | 场景 | 串行中位 | 并行中位 | 加速比 |
   * |---|---|---|---|
   * | 2 路（1080p + 4K） | 154.1ms | 152.2ms | **1.01** |
   * | 3 路（+720p） | 148.7ms | 162.8ms | **0.91** |
   * | 1 路（4K，无并行可言） | 95.7ms | 100.7ms | **0.95** |
   *
   * 第三行是关键对照：只有一路时根本无并行可谈，加速比却是 0.95 ——
   * 说明这个量级的差异是**位置噪声**，不是并行度。三者都落在 1.0 附近，
   * 结论是**浏览器把多路解码排在同一条队列上**，并行只是把排队挪了个位置。
   *
   * 第一版对照还犯过一个错：串行与并行用了不同的 seek 位置（差 0.35s），
   * 于是量出来的是"距离"而不是"并行度"，两个方向都得出过相反的结论。
   * 现在这份结论来自**控住了位置**的那一版，探针见 scripts/seek-parallel-probe.mjs。
   *
   * 所以这里保持串行：它是简单的那个写法，且**没有更慢**。
   * 真正的瓶颈在解码量（见 docs/usage.md「预览性能」），不在这一点上。
   */
  async prepare(frame) {
    // 细粒度计时：**只在被要求时记**（app 侧打开开关），平时一个 performance.now 也不多花。
    const timing = this.timing;
    const t0 = timing ? performance.now() : 0;
    const sources = this.sourcesFor(frame);
    const t1 = timing ? performance.now() : 0;
    // **每帧先清位图 —— 两种模式都要清。**
    //
    // 以前这里只清 bitmap 模式，理由是"video 模式直接走 `<video>`，没有位图这一步"。
    // 那句话对**只**用 `<video>` 的工程成立，但**静态源（贴纸 / 图片序列）没有 video**，
    // 它们的位图由宿主交进来 —— 而 wasm 侧的 resolver 是**位图优先**：
    //
    //     if let Some(bitmap) = self.bitmaps.get(source) { … return … }   // 先查位图
    //
    // 于是 video 模式下不清，宿主交进来的位图就**永不过期**：某个 source 这一帧
    // 已经不该出现（或不该再更新）时，它还在拿旧位图被画出来 —— 正是下面那句注释
    // 怕的"慢了半拍"。实测踩到过：把贴纸 <img> 打断之后画面**逐字节不变**。
    //
    // 多清一次对只用 `<video>` 的工程**无害**：那种工程从不往 bitmaps 里放东西，
    // 清一个空表没有副作用。
    //
    // ---- 阶段 4：把"每帧无条件清空"换成"声明这一帧的来源集合" ----
    //
    // `beginFramePlan` 的内部就是上面这段语义，只是**多了一档复用**：同源、同内容标识
    // 的那些位图留着（连 GPU 上传都省）。它是**放宽式**的 —— 没发标识的源、以及
    // 没有新导出的旧 wasm，行为与改前逐字节相同（判据在 wasm 的 `plan_bitmap` 那边，
    // 有单测钉着）。
    const plan = this.beginFramePlan(frame, sources);
    const reuse = plan === null ? null : new Set(plan.reuse || []);
    // **阶段 5：命中 ⇒ 源这一圈整个跳过。**
    //
    // `begin_frame` 顺手把"这一帧在不在缓存表里"回报回来（`cached`），于是**准备之前**
    // 就知道要不要花这一圈的钱。只在 `draw` 里判是不够的：那时 seek 与栅格化都已经花了，
    // 省下的只有渲染 —— "整圈"不会明显下降，缓存也就看不出有什么用。
    //
    // 命中时跳过的是 **seek**（prepare 里唯一的长等待，实测慢帧里 99% 的时间在它身上）。
    // 文字那一趟**照旧做**：`textProbe` 判的正是"刚算过的那份清单 + 它的位图"，
    // 跳过它判定通道会拿到一份没有位图的清单，于是 probe 给出**错的红** ——
    // 为一趟零点几毫秒的活换一个假红不划算。
    const frameCached = plan !== null && plan.cached === true;
    const t2 = timing ? performance.now() : 0;
    let seekMs = 0, bitmapMs = 0, setMs = 0;
    // 阶段 4 的读数：这一帧**跳过**了几次位图重建、真交了几张。
    let reusedBitmaps = 0, submittedBitmaps = 0;
    // 短路命中率：**这个数决定"每帧都在真 seek"还是"大多数帧直接返回"**。
    let hits = 0, misses = 0, maxDeltaMs = 0;
    for (const entry of sources) {
      // **命中的这一帧不进这一圈**（见上面那段）。
      if (frameCached) continue;
      const video = this.videos.get(entry.source);
      if (video === undefined) continue;
      const a = timing ? performance.now() : 0;
      const before = timing ? { t: video.currentTime, rs: video.readyState } : null;
      // **跟随播放**（宿主选开，默认关）：视频自己在往前走时**不 seek**，
      // 直接拿它**正在显示**的那一帧。
      //
      // 为什么必须：源素材的 GOP 可能长得离谱（实测一份 1080p60 素材
      // **前 250 帧里 0 个关键帧**，GOP > 4 秒），于是每设一次 currentTime
      // 解码器都要退回上一个关键帧再往前解几百帧 —— ffmpeg 实测一次 150~183ms，
      // 浏览器里 212ms/帧，预览只剩 5fps。而**看片不需要帧精确**：视频按 1× 播，
      // 取它当前显示的帧就是对的。帧精确（出片、双端比对）仍然走 seek 那条路，
      // 所以这个开关**默认关** —— 开了它，seek 就不再是帧精确的了。
      // **只要视频在播，就跟它** —— 不比绝对时刻。
      //
      // 为什么不能比：一开始我加了「偏差在 followTolerance 内才跟」，结果**振荡** ——
      // 跟随几帧后偏差涨过阈值 -> seek 一次 200ms -> 那 200ms 又把偏差推得更大 ->
      // 于是几乎每帧都在 seek（实测读数就是 seek 在 0 与 200ms 之间跳）。
      //
      // 正确语义：**播放中，视频自己就是权威**（它按 1× 走，与 DOM 时钟同起点同速率，
      // 偏差只会在毫秒级）。容差只该用在「暂停/拖动后要不要补一次 seek」上。
      const following = this.followPlayback === true && video.paused !== true;
      if (following !== true) await this.seekVideo(video, entry.seconds);
      const b = timing ? performance.now() : 0;
      const spent = b - a;
      seekMs += spent;
      if (timing && this.lastSeekVideo) {
        if (this.lastSeekVideo.hit) hits += 1;
        else {
          misses += 1;
          if (this.lastSeekVideo.deltaMs > maxDeltaMs) maxDeltaMs = this.lastSeekVideo.deltaMs;
        }
      }
      // **慢的那一路要能说出"它当时是什么状态"。** 只报"seek 45ms"无法归因：
      // 是从很远的地方跳过来（真的解码多），还是就从旁边挪一点（那是别的东西在拖）。
      if (timing && spent > 30) {
        this.slowSeeks.push({
          source: entry.source,
          spentMs: Number(spent.toFixed(1)),
          fromT: Number((before ? before.t : 0).toFixed(3)),
          toT: Number(entry.seconds.toFixed(3)),
          // 跳了多远（秒）。**这条是最能分辨原因的数。**
          jumpedSec: Number(Math.abs(entry.seconds - (before ? before.t : 0)).toFixed(3)),
          readyStateBefore: before ? before.rs : -1,
          readyStateAfter: video.readyState,
        });
        if (this.slowSeeks.length > 60) this.slowSeeks.shift();
      }
      // ===== VIDEO_CAPS：**开跑前**用纯 JS 问一次（不碰 wasm ⇒ 失败也不会打死引擎 ✓）=====
      // Firefox 的 GPUCopyExternalImageSourceInfo 名单里**没有 HTMLVideoElement**
      //（实测报错：could not be converted to any of: ImageBitmap, HTMLImageElement,
      //  HTMLCanvasElement, OffscreenCanvas）⇒ 直接把它交给 wasm 会 panic ✗。
      // ⇒ 这里先用 JS 试一次 copy：不抛 ⇒ 保持 video ✓（Edge/Chromium ✓）；抛 TypeError ⇒ 切 bitmap ✓。
      // 注意：这是**开跑前的判定**，不是"出错后回退" ✓ —— 所以永远不会 panic ✓。
      if (this.sourceMode === "video" && this.videoCopyOk === undefined) {
        const probeVideo = this.videos.values().next().value
        if (probeVideo) {
          try {
            const ad = await navigator.gpu.requestAdapter()
            const dev = ad && (await ad.requestDevice())
            if (dev) {
              const tex = dev.createTexture({
                size: [16, 16, 1],
                format: "rgba8unorm",
                usage: GPUTextureUsage.COPY_DST | GPUTextureUsage.RENDER_ATTACHMENT
              })
              dev.queue.copyExternalImageToTexture({ source: probeVideo }, { texture: tex }, [16, 16])
              this.videoCopyOk = true
            }
          } catch (e) {
            this.videoCopyOk = false
            console.warn("[dhampir] 这个浏览器不收 <video> 作为零拷贝源 ⇒ 改用 bitmap ✓（浏览器限制，非代码问题）", String((e && e.message) || e))
          }
          if (this.videoCopyOk === false) this.setSourceMode("bitmap")
        }
      }
      if (this.sourceMode === "bitmap") {
        // **阶段 4：这一帧的内容标识与上一帧一致 ⇒ 宿主手上那张纹理就是这一帧要的。**
        //
        // 于是 `createImageBitmap`（一次全幅回读）与那次 GPU 上传**一起跳过**，
        // `set_bitmap` 也不必调 —— 宿主那边留着的位图与纹理仍然有效。
        //
        // 判据由**宿主**给出（`plan.reuse`），不是我这边猜的：只有它知道自己手上
        // 到底还有没有那张纹理（可能刚被清过、也可能尺寸对不上）。所以这里只消费
        // 它的结论，不重新判断一次 —— 两处各判一次就是给两个答案不一致的机会。
        if (reuse !== null && reuse.has(entry.source)) {
          reusedBitmaps += 1;
          continue;
        }
        // **必须在 seek 之后**：早了拿到的是上一帧，而画面看起来完全正常。
        // 而且**要先等它真的可读**（见 `waitVideoReady` 的说明）——
        // 不等的话，跳转后的那一帧会抛 `InvalidStateError`，整个视频层空掉。
        try {
          await Engine.waitVideoReady(video, 400);
          // **①b 已按"读数判决"撤回**（pre-declared rule：`bitmap` 没变小就撤 ✓）。
          //
          // 你的实测：`bitmap 17ms` —— 与改前的 13~18ms **同一区间** ⇒ 给
          // `createImageBitmap` 加 `resizeWidth/resizeHeight` **没有**省掉那次 1080p 回读
          // （回读发生在缩放之前），所以这里回到不带参数的原样。
          //
          // 保留下来的：`this.targetW/targetH`（`resize` 里记的目标尺寸）—— 它自己没坏处，
          // 而且以后若要在别处按目标尺寸做事还用得上。① 那条路（按窗口尺寸渲染）**不受影响** ✓。
          const bitmap = await createImageBitmap(video);
          const c = timing ? performance.now() : 0;
          bitmapMs += c - b;
          this.mod.dhampir_project_set_bitmap(entry.source, bitmap);
          submittedBitmaps += 1;
          if (timing) setMs += performance.now() - c;
        } catch (error) {
          // 这一路这一帧没有画面 -> 那一层会被跳过（宿主侧 require_bitmap 会让它返回 None）。
          // **不许退回 video**：那正好会撞上这个浏览器不支持的那条路。
          console.warn("dhampir: 源 " + entry.source + " 这一帧做不出位图：" + error);
        }
      }
    }
    const t3 = timing ? performance.now() : 0;
    // 文字排在视频之后：上面的 seek 是这一步唯一的长等待，而画字是本地画布上的活。
    // 反过来（先画字再等 seek）只会让"字先到、画面还没到"多出一个中间态。
    await this.prepareText(frame);
    const t4 = timing ? performance.now() : 0;
    if (timing) {
      this.lastPrepareBreakdown = {
        sourcesForMs: Number((t1 - t0).toFixed(2)),
        clearBitmapsMs: Number((t2 - t1).toFixed(2)),
        seekMs: Number(seekMs.toFixed(2)),
        createBitmapMs: Number(bitmapMs.toFixed(2)),
        setBitmapMs: Number(setMs.toFixed(2)),
        textMs: Number((t4 - t3).toFixed(2)),
        prepareTotalMs: Number((t4 - t0).toFixed(2)),
        sources: sources.length,
        // 短路命中/未命中：未命中多说明"每帧都在真 seek"。
        shortCircuitHits: hits,
        shortCircuitMisses: misses,
        maxMissDeltaMs: Number(maxDeltaMs.toFixed(3)),
        // ---- 阶段 4 的读数 ----
        // `reusedBitmaps` = 这一帧**跳过**了几次 `createImageBitmap` + GPU 上传；
        // `submittedBitmaps` = 真交了几张。两者之和 ≈ 走 bitmap 那条路的源数。
        reusedBitmaps: reusedBitmaps,
        submittedBitmaps: submittedBitmaps,
        // 宿主侧的累计计数（"真的少拷了几次"）。没有新导出时是 null。
        hostUploads: plan === null || plan.uploads === undefined ? null : plan.uploads,
        // ---- 阶段 5 的读数 ----
        // `cacheHit` = 这一帧没花 seek 的钱（画面直接从缓存搬上画布）。
        // `cache` = 宿主的命中/未命中/容量（没有新导出或表没建时是 null）。
        cacheHit: frameCached,
        cache: plan === null || plan.cache === undefined ? null : plan.cache,
      };
    }
    return sources;
  }

  /**
   * 按清单把这一帧的文字栅格化并交给宿主。
   *
   * # 为什么要有"代"
   *
   * 行号是**按位置**编的，而 `createImageBitmap` 是异步的。拖播放头时两次 seek 会交叠：
   * 老的那一趟可能在新清单算好之后才回来，那时它手上的第 i 行已经是**另一条字幕**，
   * 贴上去的症状是"位置对、内容是上一条"。所以每一趟领一个号，`await` 回来之后再比一次
   * —— 比对与提交之间没有 await，所以这一次检查是原子的。
   *
   * （新清单算好的时候宿主已经把旧位图全丢了，所以过期的那一趟什么也不欠。）
   *
   * # 字幕与弹幕为什么要走两趟、两套编号
   *
   * 两份清单在宿主那边是**两个位图集合、两套下标**（各从 0 起）。合成一趟再按"字幕在前
   * 弹幕在后"的偏移量拆开是不行的：一条字幕算不出落点就不会进清单，偏移量随之错位，
   * 于是弹幕的位图会被贴成字幕 —— 而画面看起来只是"有一条字位置偏了"。
   */
  async prepareText(frame) {
    const manifest = this.textFrame(frame);
    // **先记清单再栅格化**：`textProbe` 要求判的正是这一份，而它只在
    // 「宿主的清单 == 这一帧」时才肯判（见 Rust 侧的 text_probe）。
    this.textManifest = manifest;
    const token = (this.textToken += 1);
    // **两类各一套画法**（字幕暖色、弹幕白色 + 各自的描边）——
    // 以前是一份共用的，而共用时"字幕是暖色、弹幕是白色"这件事**必然错一个**。
    //
    // 每类各带一份 memo：它是"这一路、这一帧、这份内容"有没有交过的记录，
    // 决定要不要跳过提交（跳过 = 让 wasm 复用上一帧那张纹理）。**两路不能共用** ——
    // 它们的编号空间与风格都不同，共用会让"字幕第 i 行"与"弹幕第 i 条"互相冒充。
    const placed = await this.rasterizePlacements(
      manifest.placements,
      manifest.subtitle_style,
      token,
      (index, bitmap) => this.mod.dhampir_project_set_text_bitmap(index, bitmap),
      this.textMemo,
    );
    // 这一趟过期了就别接着交下一批：宿主手上的清单已经不是这一帧的了。
    if (placed === false) return manifest;
    await this.rasterizePlacements(
      manifest.danmaku_placements,
      manifest.danmaku_style,
      token,
      (index, bitmap) => this.mod.dhampir_project_set_danmaku_bitmap(index, bitmap),
      this.danmakuMemo,
    );
    return manifest;
  }

  /**
   * 把一份清单逐条栅格化并交给宿主。返回 `false` = 这一趟已经过期（别接着交）。
   *
   * `submit` 决定交到哪个位图集合 —— 两套编号各自从 0 起，所以这里只认下标，
   * 不认"这份清单是字幕还是弹幕"。
   *
   * `style` 是 `{color, outline, stroke_px, stroke_color}`，**来自契约**。
   */
  async rasterizePlacements(placements, style, token, submit, memo) {
    // 行数变了 → wasm 手上那张"按行号的纹理表"长度也变了，整份作废（这一帧全部重交）。
    if (memo.count !== placements.length) {
      memo.keys.clear();
      memo.count = placements.length;
    }
    for (let index = 0; index < placements.length; index += 1) {
      const line = placements[index];
      // 全是空白字符的行**不做位图**：宿主不判它（栅格化出来本来就是空的），
      // 硬塞一张空的进去只会让"这一行没有位图"那条判据失去意义。
      if (line.visible !== true) continue;

      // **这一路、这一行、内容都没变 → 不提交** —— 这是 wasm 那边"复用上一帧纹理"的前提。
      //
      // ⚠️ 判据必须是 **(哪一路, 行号, 内容)** 三元，**不能只按内容去查栅格化缓存**。这是
      // 踩过的坑：那个缓存是**全内容**的（"B" 上一帧出现在第 1 行，它的 key 就在缓存里），
      // 而 wasm 复用纹理是**按行号**取的。于是"上一帧 [A,B]、这一帧只剩 [B]"时，
      // 第 0 行会被误判成"没变"而跳过提交，wasm 照旧复用第 0 行那张 **A** 的纹理 ——
      // 画面显示的成了上一行的字（就是 wasm 注释里怕的"慢了半拍"）。
      //
      // 为什么值得这么绕：实测（clip-25，2 行字幕）这段占 3.3ms/帧；宿主侧的栅格化缓存
      // 双跑只省掉其中 11% —— 大头正是**每帧无条件重建纹理 + 重传**，而那份重传由这一句决定。
      const key = rasterKey(line, style);
      if (memo.keys.get(index) === key) continue;
      memo.keys.set(index, key);

      let bitmap = null;
      try {
        // **直排 alpha**：宿主用 copy_external_image_to_texture 上传，并且声明
        // premultiplied_alpha = false。两边必须一致 —— 说错不会报错，只会让字的边缘发暗，
        // 而那看起来像"字体没渲染好"，不像"叠加算错了"。
        //
        // `rasterizeLine` 内部走栅格化缓存（见 `RASTER_CACHE` 的说明）：
        // **canvas 可以跨帧复用，`ImageBitmap` 不可以**（wasm 换掉旧位图时会 close 它）。
        bitmap = await createImageBitmap(rasterizeLine(line, style), {
          premultiplyAlpha: "none",
        });
      } catch (error) {
        // 不静默：宿主那边这一行会被判成 subtitle_raster_failed（有字要画、却没有位图）。
        console.warn("dhampir: 第 " + index + " 条字做不出位图：" + error);
        continue;
      }
      if (token !== this.textToken) { bitmap.close(); return false; }
      submit(index, bitmap);
    }
    return true;
  }

  /**
   * seek 并渲染到 canvas。
   *
   * 顺带把两段耗时记在 `this.lastSeekCost` 上：**"这一帧慢在哪"是排性能问题
   * 唯一有用的信息** —— 只知道"一帧要 200ms"的话，该改哪里只能靠猜。
   *
   * 记的是**上一次**的值（不累积、不统计）：引擎是底座，攒统计是调用方的事
   * —— 底座一旦开始攒状态，多实例/多画布就会互相污染。
   */
  /**
   * **单飞入口**：同一时刻只允许一条管线在跑（见构造器里 `pipeline` 的说明）。
   *
   * 提前渲染（`options.preroll`）走的也是这条路 ⇒ 它**不可能**与正常 seek 并发。
   * 上一轮那个 `RefCell already borrowed` panic 就是从这里漏出去的。
   */
  async seek(frame, options = {}) {
    const run = () => this.seekImpl(frame, options);
    const next = this.pipeline.then(run, run);
    // 链自己吞掉结果与错误：否则一次失败会把后面**所有**的帧卡死。
    // 调用方拿到的仍是 `next`（错误照旧抛给它）。
    this.pipeline = next.then(() => {}, () => {});
    return next;
  }

  /**
   * 排一帧**提前渲染**（阶段 5）。同一个槽位只留最新的一帧。
   *
   * 预渲染的语义是"把接下来要用的那几帧先算好"；排十几帧没有意义（算完早播过去了），
   * 而且每一帧都要把源重新 seek 一遍 —— 那是在跟播放抢解码器。
   *
   * 返回是否受理：缓存关着就不受理（预热一张不存在的表没有意义）。
   */
  schedulePreroll(frame) {
    if (this.previewCache.enabled !== true) return false;
    this.prerollPending = frame;
    return true;
  }

  /**
   * 现在要不要真的跑那一帧？跑就把它取走，交给**同一条单飞队列**。
   *
   * # 门控为什么**不是** "fps >= 50"
   *
   * 低帧率机器永远到不了那个门限 ⇒ 那条门控等于"这台机器永远不预热"，
   * 而它恰恰是最需要预热的。判据改成**上一帧的实际耗时**：低于预算的一半才预热 ——
   * 预热自己也要花钱，它花掉的时间必须能被后面的命中赚回来。
   *
   * 真正开跑在 `requestIdleCallback` 里（没有就退到 `setTimeout`），
   * 且**不**在这里直接跑：`seek` 的队列保证它与正常 seek 串行。
   */
  pumpPreroll(budgetMs) {
    if (this.prerollPending === null) return false;
    if (this.previewCache.enabled !== true) {
      this.prerollPending = null;
      return false;
    }
    const budget = Number(budgetMs);
    const last = this.lastSeekCost;
    if (last === undefined || !(budget > 0) || !(last.totalMs <= budget * 0.5)) return false;
    const frame = this.prerollPending;
    this.prerollPending = null;
    const start = () => { this.seek(frame, { preroll: true }).catch(() => {}); };
    if (typeof requestIdleCallback === "function") requestIdleCallback(() => start(), { timeout: 200 });
    else setTimeout(start, 0);
    return true;
  }

  /** 真正干活的那条路。**不要直接调它** —— 它没有单飞保护，请用 `seek`。 */
  async seekImpl(frame, options = {}) {
    if (this.timing && !Array.isArray(this.slowSeeks)) this.slowSeeks = [];
    const t0 = performance.now();
    const sources = await this.prepare(frame);
    const t1 = performance.now();
    // **诊断开关**：关掉 draw 之后 seekMs 会不会掉下来 ——
    // 这能判定"慢"是解码本身的，还是被上一帧的 GPU 上屏拖住的。
    // 只在测量时用，正常路径恒为 true。
    // **宿主缝隙：prepare 做完了、还没 draw。**
    //
    // 为什么非要有这个钩子：`prepare` 的第一件事是 `clear_bitmaps()`，所以宿主
    // **在 `seek` 之后**交的静态源位图（贴纸/图片序列），会被**下一次** prepare 清掉，
    // 永远等不到 draw —— 实测就是这个现象：交与不交，画面逐字节相同。
    //
    // 契约原文「JS 把某个 source 当前帧转成位图交给宿主 …… **必须在 seek 完成之后做**」
    // 里的 seek 指的是「把 <video> 定位好」，不是 engine.seek —— 这一点以前会把人绕进去。
    if (typeof options.onPrepared === 'function') await options.onPrepared(frame);
    
    // **pre-roll**：`options.preroll` 时改走 `dhampir_project_preroll` —— 它与 `draw`
    // 做完全一样的事（含**填进预渲染缓存**），**只是最后不 blit 到画布**。
    //
    // 为什么必须是"换导出"而不是"skipDraw = true"：填缓存那一步**就在 draw 里面**，
    // 跳掉整个 draw 就等于什么都没算 ✗（缓存永远是空的）。
    //
    // 顺带解决了一个正确性隐患：pre-roll 走的是**同一条准备路径** ⇒ `sources_for`、
    // 视频 seek、`onPrepared` 交源位图全都照旧 ✓ —— 不会把"空帧"灌进缓存
    //（那种命中时会画出一帧空的，比不缓存更糟）。
    if (options.preroll === true) this.mod.dhampir_project_preroll(frame);
    else if (this.skipDraw !== true) this.mod.dhampir_project_draw(frame);
    const t2 = performance.now();
    this.lastSeekCost = { prepareMs: t1 - t0, drawMs: t2 - t1, totalMs: t2 - t0 };
    // 预算够就顺手排一帧预热（宿主给了 `prerollBudgetMs` 才动，默认 null = 不动）。
    this.pumpPreroll(this.prerollBudgetMs);
    return sources;
  }

  /**
   * 出片前的预检：这份工程里有没有**超出对端能力**的东西。
   *
   * 规则**不在这里** —— 它在 Rust 的 host_api::precheck 里，
   * 这个方法只是通道。在 JS 里重写一遍过滤逻辑，两端就会各自演化。
   */
  precheck(capabilities) {
    return JSON.parse(this.mod.dhampir_project_precheck(JSON.stringify(capabilities)));
  }

  resize(width, height) {
    this.mod.dhampir_project_resize(width, height);
    // ①b 需要知道目标尺寸：取视频帧时按它缩（见下面 createImageBitmap 那处）。
    // 以前这里只转发给 wasm，JS 侧没有这个数 —— 于是每帧都按 1080p 全幅取帧，
    // 而那 13~18ms 的成本大头正是全幅的读回与格式转换。
    this.targetW = width;
    this.targetH = height;
  }

  /** 把一个 video 元素接到某个 source 上（页面可能已有元素，不必再建）。 */
  registerVideo(source, video) {
    this.videos.set(source, video);
    // 与 `bindSource` 同一条理由：换了元素就是换了画面。
    this.bumpPlanGeneration();
  }
}

/** 载入 wasm 并返回引擎。pkgUrl 由调用方给——**不写死目录结构**，
 *  下游可以换成 npm 包、CDN 或本地路径。 */
export async function loadEngine(pkgUrl) {
  const mod = await import(pkgUrl);
  await mod.default();
  return new Engine(mod);
}
