# 宿主 API

Version: 2

下游宿主读这一份就够：**形状在 `crates/dhampir-timeline/src/host_api.rs`，
版本问 `dhampir_host_api_version`，导出名单在文末**。

这份文档本身也被钉着（`scripts/api-surface.mjs`）：下面的 `Version:` 行与名单
跟代码不一致就红。**说明是人写的** —— 生成器只动那两样，别的一个字节都不碰，
所以这里的话（哪些算承诺、形状从哪查）不会被下一次 `--write` 抹掉。

## 为什么有版本号，却没有 version 键

返回体里**没有** `version` 字段。往每个形状里塞一个，每升一次版本就要动所有形状，
而这里的形状是钉死的（每条都有键集断言）—— 启动时问一次，记住就够了。

* 对端要判断对面是哪个版本，调 `dhampir_host_api_version`：返回整数，与
  `host_api::HOST_API_VERSION` 是同一个数（现在等于 2）。
* 版本的真值在 Rust 源码那一行常量里；这个函数的返回值、这份文档的 `Version:` 行
  与文末名单，都由守卫跟源码比对 —— **「升了常量忘了改文档」不会静默通过**。

**字段增减都要 +1**：对端拿到的形状变了就是破坏性改动，不管变的是谁"觉得"重要的字段。

## v1 -> v2 变了什么

只多了一个键：`dhampir_project_frame` 的返回体多了 `overlay`。

* 有字可画的帧多出 `overlay`，形状是
  `{ items: [{ text, rect: { x, y, width, height } }], color, outline, dropped_lines }`；
  `items` 按轨道顺序（先画的在前）、轨内按行，`color` 是 RGBA 四字节数组。
* **没有字幕时这个键根本不出现**，而不是给一个空数组：「没有字幕」与
  「有字幕但这一帧没字」在评估层就是同一个答案，在这里为它们造出两种形状，
  等于把那个区分又搬回来一次。
* `rect` 是**归一化矩形**（相对文档坐标系，0..1），**不是像素落点** ——
  像素由宿主按当前分辨率算。同一份返回体在 720p 与 1080p 下因此完全一样。
* `frame` / `layers` / `error` 三个键的形状没变。

**已知边界（不假装）**：一条字幕的所有行都超过 `max_lines` 时，评估层按契约返回
「没有可画的东西」，于是 `overlay` 缺席 —— 这份 `dropped_lines` 在中途就丢了，拿不到。

## 那几行字是谁画的

**宿主画**。`overlay` 只说「画哪几行字、各占哪个矩形」：栅格化（字体、字号、
描边怎么落成位图）是实现侧的事，所以同一个 `overlay` 在两个宿主上允许出现
不同的像素 —— 比对的是**结构**（项数/文本/矩形），不是字形的逐像素。

## 名单之外的那些导出

同一个模块里还有几个**只给本仓库验收用**的导出（离屏探针、sample project 出图这类）。
它们也在下面的名单里（守卫按模块扫，不按用途猜），但**不承诺兼容** ——
真要长期依赖，先把它们从探针改成契约。`docs/api-surface.md` 是全部导出的分类表，
那上面标着哪些模块算承诺。

## 导出名单（`crates/dhampir-wasm/src/timeline_host.rs`）

- `dhampir_host_api_version`
- `dhampir_project_attach`
- `dhampir_project_bind_source`
- `dhampir_project_clear_bitmaps`
- `dhampir_project_clear_text_bitmaps`
- `dhampir_project_doc`
- `dhampir_project_draw`
- `dhampir_project_edit`
- `dhampir_project_end_frame`
- `dhampir_project_first_frame`
- `dhampir_project_frame`
- `dhampir_project_open`
- `dhampir_project_precheck`
- `dhampir_project_render_probe`
- `dhampir_project_resize`
- `dhampir_project_set_bitmap`
- `dhampir_project_set_bitmap_mode`
- `dhampir_project_set_subtitles`
- `dhampir_project_set_text_bitmap`
- `dhampir_project_sources_for`
- `dhampir_project_text_frame`
- `dhampir_project_text_probe`
- `dhampir_sample_project_render_png`
