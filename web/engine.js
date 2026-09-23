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

const hann = (resolve) => (event) => resolve(event);

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

  /** 建预览宿主。canvas 尺寸决定预览尺寸——schema v1 里没有分辨率字段。 */
  async attach(canvasId) {
    const info = JSON.parse(await this.mod.dhampir_project_attach(canvasId));
    this.attached = true;
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

  /** 把某一帧所需的源全部 seek 到位。 */
  async prepare(frame) {
    const sources = this.sourcesFor(frame);
    for (const entry of sources) {
      const video = this.videos.get(entry.source);
      if (video === undefined) continue;
      await this.seekVideo(video, entry.seconds);
    }
    return sources;
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
