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
  // **字体族与字重优先用契约给的**（`style.family` / `style.weight`）。
  //
  // 这条链以前是断的：契约里 `TextStyle.family` 有文档（"宿主按这个名字找，
  // 找不到要报出来"），但样式 JSON 不带它 → 这里只能回退到写死的 `sans-serif`，
  // 症状是**字号/位置/颜色都对、只有字形不对**。权重同理（V-Trim 用 700/600，
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
    // 最近一次**算好并交给宿主**的清单（`prepareText` 里写的）。
    //
    // 判定通道读它，而不是再调一次 `textFrame`：那会重算一份新清单并把刚提交的位图
    // 全部作废（宿主那边清单与位图是一起作废的），于是 probe 判的是"没有位图的清单"。
    // 交互路径不读它。
    this.textManifest = null;
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
    }
    return result;
  }

  /** 当前工程的规范化副本（含 assets / view / render_hints）。没载入过是 null。 */
  doc() {
    return this.projectFile;
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
    }
    return result;
  }

  redo() {
    const result = JSON.parse(this.mod.dhampir_project_redo());
    if (result.ok === true) {
      this.projectFile = JSON.parse(this.mod.dhampir_project_doc());
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
    // 源模式：默认**问浏览器**；?src=video / ?src=bitmap 可以强制，
    // 用来比较两条路的画面与速度（两条路的输出应当逐字节相同）。
    const override = new URLSearchParams(location.search).get("src");
    if (override === "video" || override === "bitmap") {
      this.sourceMode = override;
    } else {
      this.sourceMode = (await Engine.probeVideoCopy()) ? "video" : "bitmap";
    }
    this.mod.dhampir_project_set_bitmap_mode(this.sourceMode === "bitmap");
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
    return previous;
  }

  /** 把一个 source 绑到一个 video 元素上。 */
  bindSource(source, videoId) {
    this.mod.dhampir_project_bind_source(source, videoId);
    const video = document.getElementById(videoId);
    if (video !== null) this.videos.set(source, video);
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
  async loadSubtitles(entries) {
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
    this.mod.dhampir_project_clear_bitmaps();
    const t2 = timing ? performance.now() : 0;
    let seekMs = 0, bitmapMs = 0, setMs = 0;
    // 短路命中率：**这个数决定"每帧都在真 seek"还是"大多数帧直接返回"**。
    let hits = 0, misses = 0, maxDeltaMs = 0;
    for (const entry of sources) {
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
      if (this.sourceMode === "bitmap") {
        // **必须在 seek 之后**：早了拿到的是上一帧，而画面看起来完全正常。
        try {
          const bitmap = await createImageBitmap(video);
          const c = timing ? performance.now() : 0;
          bitmapMs += c - b;
          this.mod.dhampir_project_set_bitmap(entry.source, bitmap);
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
    const placed = await this.rasterizePlacements(
      manifest.placements,
      manifest.subtitle_style,
      token,
      (index, bitmap) => this.mod.dhampir_project_set_text_bitmap(index, bitmap),
    );
    // 这一趟过期了就别接着交下一批：宿主手上的清单已经不是这一帧的了。
    if (placed === false) return manifest;
    await this.rasterizePlacements(
      manifest.danmaku_placements,
      manifest.danmaku_style,
      token,
      (index, bitmap) => this.mod.dhampir_project_set_danmaku_bitmap(index, bitmap),
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
  async rasterizePlacements(placements, style, token, submit) {
    for (let index = 0; index < placements.length; index += 1) {
      const line = placements[index];
      // 全是空白字符的行**不做位图**：宿主不判它（栅格化出来本来就是空的），
      // 硬塞一张空的进去只会让"这一行没有位图"那条判据失去意义。
      if (line.visible !== true) continue;

      // **内容没变就不提交** —— 这是 wasm 那边"复用上一帧纹理"能生效的前提。
      //
      // wasm 的 `upload_text_bitmaps` 现在只在**收到新位图**（脏标记）时才重建纹理 + 重传，
      // 而"收到新位图"由这里决定。判据就是上面那个栅格化缓存的 key（**内容级**，不是行号级）——
      // 所以"第 i 行换了字"必然不命中、必然提交，"第 i 行没变"才跳过，不会留下上一行的字。
      //
      // 为什么值得：实测（clip-25，2 行字幕）这段占 3.3ms/帧；双跑证明宿主侧的栅格化缓存
      // 只省掉其中 11% —— 大头正是这次**每帧无条件重建纹理 + 重传**。
      if (RASTER_CACHE.has(rasterKey(line, style))) continue;

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
  async seek(frame, options = {}) {
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
    
    if (this.skipDraw !== true) this.mod.dhampir_project_draw(frame);
    const t2 = performance.now();
    this.lastSeekCost = { prepareMs: t1 - t0, drawMs: t2 - t1, totalMs: t2 - t0 };
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
  }

  /** 把一个 video 元素接到某个 source 上（页面可能已有元素，不必再建）。 */
  registerVideo(source, video) {
    this.videos.set(source, video);
  }
}

/** 载入 wasm 并返回引擎。pkgUrl 由调用方给——**不写死目录结构**，
 *  下游可以换成 npm 包、CDN 或本地路径。 */
export async function loadEngine(pkgUrl) {
  const mod = await import(pkgUrl);
  await mod.default();
  return new Engine(mod);
}
