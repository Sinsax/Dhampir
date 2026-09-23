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
function rasterizeLine(line, color, outline) {
  const canvas = document.createElement("canvas");
  canvas.width = line.bitmap_width;
  canvas.height = line.bitmap_height;
  const ctx = canvas.getContext("2d");
  ctx.clearRect(0, 0, canvas.width, canvas.height);
  ctx.font = line.font_px + "px " + TEXT_FONT;
  ctx.textAlign = "center";
  ctx.textBaseline = "middle";
  ctx.lineJoin = "round";
  ctx.miterLimit = 2;
  const x = canvas.width / 2;
  const y = canvas.height / 2;
  if (outline === true && line.border_px > 0) {
    // 边宽取两倍：drawtext 的 borderw 是**向外**扩一圈，而 canvas 的描边压在字上。
    // 先描边后填字，内半边被字盖掉，剩下的外半边就是那一圈。
    ctx.lineWidth = line.border_px * 2;
    ctx.strokeStyle = "black";
    ctx.strokeText(line.text, x, y);
  }
  ctx.fillStyle = cssColor(color);
  ctx.fillText(line.text, x, y);
  return canvas;
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

  /** 把一个 video 定位到指定秒数，等它真的 seek 完。 */
  async seekVideo(video, seconds) {
    return new Promise((resolve) => {
      const done = (event) => { video.removeEventListener("seeked", done); resolve(event); };
      video.addEventListener("seeked", done);
      video.currentTime = seconds;
      // 兜底：seek 到同一位置时浏览器可能不派 seeked，不能让预览卡死。
      setTimeout(done, 3000);
    });
  }

  /** 把某一帧所需的源全部 seek 到位（位图模式下顺带把位图做好）。 */
  async prepare(frame) {
    const sources = this.sourcesFor(frame);
    // 位图模式**先清**：不清的话，这一帧不再出现的 source 会拿着上一帧的位图被画出来，
    // 而画面看起来完全正常，只是"慢了半拍"。
    if (this.sourceMode === "bitmap") this.mod.dhampir_project_clear_bitmaps();
    for (const entry of sources) {
      const video = this.videos.get(entry.source);
      if (video === undefined) continue;
      await this.seekVideo(video, entry.seconds);
      if (this.sourceMode === "bitmap") {
        // **必须在 seek 之后**：早了拿到的是上一帧，而画面看起来完全正常。
        try {
          const bitmap = await createImageBitmap(video);
          this.mod.dhampir_project_set_bitmap(entry.source, bitmap);
        } catch (error) {
          // 这一路这一帧没有画面 -> 那一层会被跳过（宿主侧 require_bitmap 会让它返回 None）。
          // **不许退回 video**：那正好会撞上这个浏览器不支持的那条路。
          console.warn("dhampir: 源 " + entry.source + " 这一帧做不出位图：" + error);
        }
      }
    }
    // 文字排在视频之后：上面的 seek 是这一步唯一的长等待，而画字是本地画布上的活。
    // 反过来（先画字再等 seek）只会让"字先到、画面还没到"多出一个中间态。
    await this.prepareText(frame);
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
   */
  async prepareText(frame) {
    const manifest = this.textFrame(frame);
    // **先记清单再栅格化**：`textProbe` 要求判的正是这一份，而它只在
    // 「宿主的清单 == 这一帧」时才肯判（见 Rust 侧的 text_probe）。
    this.textManifest = manifest;
    const token = (this.textToken += 1);
    for (let index = 0; index < manifest.placements.length; index += 1) {
      const line = manifest.placements[index];
      // 全是空白字符的行**不做位图**：宿主不判它（栅格化出来本来就是空的），
      // 硬塞一张空的进去只会让"这一行没有位图"那条判据失去意义。
      if (line.visible !== true) continue;
      let bitmap = null;
      try {
        // **直排 alpha**：宿主用 copy_external_image_to_texture 上传，并且声明
        // premultiplied_alpha = false。两边必须一致 —— 说错不会报错，只会让字的边缘发暗，
        // 而那看起来像"字体没渲染好"，不像"叠加算错了"。
        bitmap = await createImageBitmap(rasterizeLine(line, manifest.color, manifest.outline), {
          premultiplyAlpha: "none",
        });
      } catch (error) {
        // 不静默：宿主那边这一行会被判成 subtitle_raster_failed（有字要画、却没有位图）。
        console.warn("dhampir: 第 " + index + " 行字做不出位图：" + error);
        continue;
      }
      if (token !== this.textToken) { bitmap.close(); return manifest; }
      this.mod.dhampir_project_set_text_bitmap(index, bitmap);
    }
    return manifest;
  }

  /** seek 并渲染到 canvas。 */
  async seek(frame) {
    const sources = await this.prepare(frame);
    this.mod.dhampir_project_draw(frame);
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
