# 宿主 API

Version: 6

下游宿主读这一份就够：**形状在 `crates/dhampir-timeline/src/host_api.rs`，
版本问 `dhampir_host_api_version`，导出名单在文末**。

这份文档本身也被钉着（`scripts/api-surface.mjs`）：下面的 `Version:` 行与名单
跟代码不一致就红。**说明是人写的** —— 生成器只动那两样，别的一个字节都不碰，
所以这里的话（哪些算承诺、形状从哪查）不会被下一次 `--write` 抹掉。

## 为什么有版本号，却没有 version 键

返回体里**没有** `version` 字段。往每个形状里塞一个，每升一次版本就要动所有形状，
而这里的形状是钉死的（每条都有键集断言）—— 启动时问一次，记住就够了。

* 对端要判断对面是哪个版本，调 `dhampir_host_api_version`：返回整数，与
  `host_api::HOST_API_VERSION` 是同一个数（现在等于 6）。
* **但版本号答不了"这是哪一次构建"**：它只在导出面/形状变化时才 +1，
  一次纯实现修复（例如把动图上传从逐帧改成一张数组纹理）**不改它**。
  这种时候问 `dhampir_build_id` —— 返回一句话 `git=<短 sha> api=<n>`，
  sha 由 `scripts/package.mjs` 在构建时经 `DHAMPIR_GIT_SHA` 注入
  （开发树直接 `cargo build` 时是 `git=unknown`）。
  **它不是契约**：形状不承诺，只为"浏览器里跑的到底是哪份产物"这一个问题存在。
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

## v2 -> v3 变了什么

`overlay` 这个形状里多了两个键：`danmaku` 与 `dropped_danmaku`。

* `danmaku` 是弹幕条目数组，每条形状是
  `{ text, rect: { x, y, width, height }, lane, enter, exit }`。
* **`rect` 是这一帧的滚动位置**，不是像字幕那样一份固定的居中矩形：弹幕按自己文本
  宽度**左对齐**并横向滚动，同一个条目的 `rect.x` 随帧变化（进入帧在右边缘外，
  离开帧在左边缘外）。要「这条弹幕最终停在哪个泳道、活在哪几帧」看 `lane`
  与 `enter` / `exit`（闭区间，帧号）。
* `dropped_danmaku` 是这一帧评估中被丢弃的弹幕条数（泳道排不下就丢并计数，
  不允许叠在一起）。它与 `dropped_lines` 一样，是结论的一部分，不是日志。
* **字幕与弹幕分成两个键，不合并进 `items`**：行的 `rect` 是居中的固定矩形，
  弹幕的 `rect` 是逐帧滑动的左对齐矩形，落在一起就得在下游按形状分叉。键名
  （`text`/`rect`/`x`/`y`/`width`/`height`）与另外两处（字幕 `items`、CLI 的
  `subtitle` 子命令）**逐字段同名**，读的人不必记两套叫法。

**已知边界（不假装）**：弹幕的颜色与描边**复用所属轨道的 `SubtitleStyle`**，
`DanmakuSpec` 自己没有颜色字段 —— 所以 `color` / `outline` 这两个键对字幕与弹幕
是同一份值，不能按弹幕单独调色。

## v3 -> v4 变了什么

**形状一个都没动**，多的是**一对导出**：`dhampir_project_undo` 与
`dhampir_project_redo`。

* 两个都是**无参**的：历史本身在宿主里（`dhampir-timeline::history`），
  与 CLI `edit --undo/--redo --history <文件>` 走的是**同一份规则**。
* 返回体沿用编辑那一套 `{ ok, summary, issues }` —— 所以这一版升的是
  **对端能看到的导出面**，不是任何形状。`summary` 形如 `撤销：<被退掉的那一步>`；
  退不动时 `ok:false` 且 `issues[0].code` 是 `nothing_to_undo` / `nothing_to_redo`，
  **宿主里的工程与历史一个字节都不动**。
* `dhampir_project_open` 成功时历史会**清空**（换一份工程就是换一条历史）；
  编辑失败**不占一步**（失败的那一步没有可退的东西）。
* 没有载入工程时给 `no_project`。

## v4 -> v5 变了什么

**形状面**：`overlay` 里的 `subtitle_style` / `danmaku_style` 多了四个键 ——
**文字阴影**。**导出面一个都没动。**

| 键 | 含义 |
|---|---|
| `shadow_color` | 阴影颜色（RGBA 四字节）。`null` = **不画阴影**（默认，老工程就是这个形状） |
| `shadow_dx_px` | 水平偏移（文档像素，与 `transform.x/y` 同一坐标系） |
| `shadow_dy_px` | 垂直偏移（文档像素，**正数向下**） |
| `shadow_blur_px` | **模糊半径（像素）**。契约里是 `shadow_blur_ratio`（占高的比例），求值层按高换算过来 |

* **为什么四个都能缺省还要升版本**：规矩是「**wasm 返回体加字段即视为 API 变更**」。
  对端拿到的形状变了就是破坏性改动，不管变的是谁"觉得"重要的字段 —— 与 v3 -> v4
  正好是同一句话的两半（那边形状没变、导出面变了，照样升）。
* **老工程一个键都不多**：四个字段都带 `skip_serializing_if`，缺省值不进返回体，
  所以"没写过阴影的工程"拿到的形状与 v4 **逐字节相同**。
* **弹幕那半边恒为不画**（`shadow_color: null`）：`DanmakuSpec` 里没有阴影字段，
  跟着字幕走会让给字幕配的阴影莫名其妙地出现在弹幕上。
* **量纲**：偏移与模糊都是**像素**，且与 `stroke_px` 同一条口径（求值层拿到的那个尺寸
  —— 默认路径上就是导出尺寸）。这一层**不**再做一次"文档 -> 目标"的换算：
  那需要两个尺寸，而 `evaluate_overlay` 只有前者。
* **两端只保证观感近似**：浏览器是 canvas 的 `shadowBlur`，CLI 是 ffmpeg 的
  `gblur`（σ = 模糊半径 / 2）。**不保证逐像素一致** —— 与"字形像素允许不同"同一条。

**已知边界（不假装）**：浏览器那一侧，阴影是画在**与文字同一张画布**里的，
所以模糊/偏移超出那一行的位图范围时会被裁掉；CLI 那一侧会为阴影多开一张扩过边的位图。
两端因此只在"影子没超出那一行"时严格近似。字形的位图尺寸是**契约给的**
（`placements[].bitmap_width/height`），不能为了影子改它。

## v5 -> v6 变了什么

**形状面一个键都没动**；多的是**一对导出**，让引擎自己解码动图：
`dhampir_asset_load_animation` 与 `dhampir_asset_animation_info`。

在此之前，动图（GIF）是由宿主自己解帧、每帧 `set_bitmap` 喂进来的 —— 于是
「浏览器解出来的帧」与「CLI 解出来的帧」是两份实现，而贴纸的相位差一帧
**单帧根本看不出来**。这一版把解码搬进引擎：两个宿主用**同一份** core 解码器
（`dhampir_core::animation`），于是「预览所见 = 成片所得」成了结构上的保证，
而不是一条要靠对齐维护的纪律。动画 WebP 走同一条路，只是魔数不同。

### `dhampir_asset_load_animation(asset_id, bytes)`

* `bytes` 由**宿主负责取**（浏览器 = `fetch`）。**格式按魔数认**，不看扩展名：
  登记表里的 `kind` 是录入时的猜测，字节是事实，不一致时以字节为准。
* 引擎把整段解成帧序列常驻显存，此后**播放期零请求、零逐帧上传**。
* `asset_id` 必须是这一层在求值里用的那个源标识（= 工程里的 asset_id）——
  引擎按它接管这一路的纹理。
* **重复调用同一个 id 是替换**（幂等）：编辑里换了素材文件时重新 load 必须生效。
* **失败不改路由**：没解开的资产照旧走宿主位图那条路（也就是"这一层画不出来"），
  而不是把工程弄坏。

返回体两种形态，**每个形态只出现自己那几个键**（与 `open` 同一条口径）：

| 形态 | 键 |
|---|---|
| 成功 | `parsed` `ok` `info` |
| 失败 | `parsed` `ok` `error` |

`parsed` 表示「魔数认出来了」，`ok` 表示「整张图解完并且传上 GPU 了」。
**认得出但解不开**（截断、超帧数上限）是 `parsed:true, ok:false` —— 这两件事
分开报，是为了让调用方能分辨"这不是动图"与"这份动图坏了"。

### `dhampir_asset_animation_info(asset_id)`

**只查，不加载、不传字节**。页面重放或宿主重建之后，调用方想知道要不要重新 load，
而不想为了确认这件事再传一遍几十 MB。返回体：

| 形态 | 键 |
|---|---|
| 引擎手里有 | `loaded` `info` |
| 引擎手里没有 | `loaded` |

`loaded:true` 而没有 `info`（带 `error`）是**过渡态**：时间真值在、纹理不在
（attach 之前 load 过，或宿主被重建）。这时宿主**应当重新 load**。

### `info` 的字段

| 键 | 含义 |
|---|---|
| `format` | `"gif"` 或 `"webp"` —— **按魔数认出来的** |
| `width` / `height` | 画布尺寸（动图每一帧都是整张画布） |
| `frame_count` | 帧数 |
| `loop_count` | 文件里写的循环次数；**0 = 无限循环**（两个格式同语义） |
| `total_ms` | 一圈总时长（毫秒） |
| `frame_delays_ms` | **逐帧延迟表**，长度等于 `frame_count` |
| `bytes` | 上传后占多少字节（估算，不含驱动对齐） |

### 两个必须照做的约定

1. **`frame_delays_ms` 是时间真值**。工程 JSON 里也有一份（录入时探测的），
   两份不一致时**以返回的这一份为准** —— 它是解码器从文件本身读出来的。
   宿主拿到后应当把它写回资产表。映射用一张表、像素用另一张表，
   表现就是动图**越播越偏**。
2. **这一路不再 `set_bitmap`**。引擎自己供帧之后，宿主再喂位图等于把贴纸钉死在
   某一帧上（画面看起来只是"动图不动了"）。`sources_for` 里它照旧出现 ——
   求值是同一份，变的只是**谁提供像素**。

**已知边界（不假装）**：

* **滤镜类特效照旧**（模糊、色彩、扭曲都由引擎管线跑）；但**逐帧的宿主侧加工
  （比如宿主自己的字形/贴纸贴图合成）在这一路上没有位置** —— 引擎供的是源纹理。
* 这一版**不做显存预算的 LRU 驱逐**：整段常驻。超预算时 `load` **明确失败**
  （`ok:false`），而不是悄悄换出一部分帧。理由是 GIF 没有关键帧，换出再换回
  要重播整段 —— 那会让"seek 后第一帧在一个帧间隔内出图"这条承诺失效。
  预算上限见 `dhampir_core::animation::MAX_DECODED_BYTES`（256 MiB）。
* 解码按**素材原生尺寸**，缩放交给 GPU 采样（与视频源同路）。

## 那几行字是谁画的

**宿主画**。`overlay` 只说「画哪几行字（字幕与弹幕）、各占哪个矩形」：栅格化（字体、字号、
描边怎么落成位图）是实现侧的事，所以同一个 `overlay` 在两个宿主上允许出现
不同的像素 —— 比对的是**结构**（项数/文本/矩形/泳道/帧区间），不是字形的逐像素。

## 名单之外的那些导出

同一个模块里还有几个**只给本仓库验收用**的导出（离屏探针、sample project 出图这类）。
它们也在下面的名单里（守卫按模块扫，不按用途猜），但**不承诺兼容** ——
真要长期依赖，先把它们从探针改成契约。`docs/api-surface.md` 是全部导出的分类表，
那上面标着哪些模块算承诺。

## 导出名单（`crates/dhampir-wasm/src/timeline_host.rs`）

- `dhampir_asset_animation_info`
- `dhampir_asset_load_animation`
- `dhampir_build_id`
- `dhampir_host_api_version`
- `dhampir_project_attach`
- `dhampir_project_begin_frame`
- `dhampir_project_bind_source`
- `dhampir_project_clear_bitmaps`
- `dhampir_project_doc`
- `dhampir_project_draw`
- `dhampir_project_edit`
- `dhampir_project_end_frame`
- `dhampir_project_first_frame`
- `dhampir_project_frame`
- `dhampir_project_open`
- `dhampir_project_precheck`
- `dhampir_project_preroll`
- `dhampir_project_redo`
- `dhampir_project_render_probe`
- `dhampir_project_resize`
- `dhampir_project_set_bitmap`
- `dhampir_project_set_mask_image`

- `dhampir_project_set_bitmap_mode`
- `dhampir_project_set_danmaku_bitmap`
- `dhampir_project_set_subtitles`
- `dhampir_project_set_text_bitmap`
- `dhampir_project_sources_for`
- `dhampir_project_text_frame`
- `dhampir_project_text_probe`
- `dhampir_project_undo`
- `dhampir_sample_project_render_png`


### `dhampir_project_set_mask_image(asset_id, bitmap)`

**把一张掩码图交给预览宿主**（浏览器那条腿）。预览宿主没有素材表、取不到掩码图，
所以由 JS 侧负责"取 + 解码"（`fetch` → `createImageBitmap`），再把**位图对象**交进来 ——
**与源位图那条路同一个形状**（`dhampir_project_set_bitmap`），一个 `copy_external_image_to_texture` 就上了 GPU。

尺寸为 0（多半是解码失败）直接报错。**没有交进来的掩码会让 `draw` 报错** ——
而不是静默画一张没有掩码的图（这正是这条通路存在的理由）。
`web/engine.js` 的 `async uploadMasks()` 就是调用方：在 `open()` 之后 await 一次。