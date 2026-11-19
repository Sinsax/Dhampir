# 宿主 API

Version: 7

形状的真值在 `crates/dhampir-timeline/src/host_api.rs`；版本问 `dhampir_host_api_version`；导出名单在文末。本文件由 `scripts/api-surface.mjs` 钉住 `Version:` 行与文末名单，其余说明由人写。

## 版本与构建

`dhampir_host_api_version` 返回整数，与 `host_api::HOST_API_VERSION` 同值（现在 7）；`dhampir_build_id` 返回 `git=<短 sha> api=<n>`（sha 由 `scripts/package.mjs` 经 `DHAMPIR_GIT_SHA` 注入，开发树是 `git=unknown`；**形状不承诺**）。返回体里**没有** `version` 键：启动时问一次就够。**字段增减都要 +1** —— 形状变了就是破坏性改动。

## v1 -> v2 变了什么

`dhampir_project_frame` 的返回体多了 `overlay`（**没有字幕时这个键根本不出现**）：`{ items: [{ text, rect: { x, y, width, height } }], color, outline, dropped_lines }`。`items` 按轨道顺序（先画的在前）、轨内按行；`rect` 是**归一化矩形**（0..1，文档坐标系），不是像素落点。已知边界：所有行都超 `max_lines` 时评估层返回「没有可画的东西」，`overlay` 缺席、`dropped_lines` 拿不到。

## v2 -> v3 变了什么

`overlay` 多了 `danmaku` 与 `dropped_danmaku`：每条 `{ text, rect: { x, y, width, height }, lane, enter, exit }`，`rect` 是**这一帧的滚动位置**（左对齐、随帧变），`lane` 与 `enter`/`exit`（闭区间帧号）定泳道与存活帧，`dropped_danmaku` 是这一帧丢掉的条数（排不下就丢，不叠）；键名与字幕 `items`、CLI `subtitle` 逐字段同名。已知边界：弹幕的颜色与描边**复用所属轨道的 `SubtitleStyle`**，不能单独调。

## v3 -> v4 变了什么

形状一个都没动，多一对**无参**导出：`dhampir_project_undo` / `dhampir_project_redo`，返回体沿用 `{ ok, summary, issues }`（`summary` 形如 `撤销：<被退掉的那一步>`）。退不动时 `ok:false`、`issues[0].code` 为 `nothing_to_undo` / `nothing_to_redo`（工程与历史一个字节都不动），没载入工程给 `no_project`；`dhampir_project_open` 成功会清空历史，编辑失败不占一步。

## v4 -> v5 变了什么

`overlay` 里的 `subtitle_style` / `danmaku_style` 多了四个键（文字阴影）；**导出面一个都没动**。

| 键 | 含义 |
|---|---|
| `shadow_color` | 阴影颜色（RGBA 四字节）。`null` = **不画阴影**（默认，老工程就是这个形状） |
| `shadow_dx_px` | 水平偏移（文档像素，与 `transform.x/y` 同一坐标系） |
| `shadow_dy_px` | 垂直偏移（文档像素，**正数向下**） |
| `shadow_blur_px` | **模糊半径（像素）**；契约里是 `shadow_blur_ratio`（占高的比例），求值层按高换算 |

四个字段都带 `skip_serializing_if`，缺省不进返回体（没写过阴影的工程与 v4 逐字节相同）；弹幕那半边恒为不画（`DanmakuSpec` 没有阴影字段）；两端只保证观感近似（canvas `shadowBlur` vs ffmpeg `gblur`，σ = 模糊半径 / 2）。已知边界：浏览器阴影画在**与文字同一张画布**里，超出该行位图范围会被裁；CLI 会多开一张扩过边的位图。

## v5 -> v6 变了什么

形状面一个键都没动；多一对导出，让引擎自己解码动图（GIF / 动画 WebP，只是魔数不同），两个宿主共用 `dhampir_core::animation`。

### `dhampir_asset_load_animation(asset_id, bytes)`

- `bytes` 由**宿主负责取**（浏览器 = `fetch`），**格式按魔数认**、不看扩展名。
- 整段解成帧序列常驻显存（播放期零请求、零逐帧上传）；`asset_id` 必须是求值里用的那个源标识（= 工程里的 asset_id）；**重复 load 同一个 id 是替换**（幂等）；**失败不改路由**（照旧走宿主位图那条路）。

返回：成功 `parsed` `ok` `info`，失败 `parsed` `ok` `error`；`parsed` = 魔数认出来了，`ok` = 整张图解完并传上 GPU（认得出但解不开，如截断、超帧数上限，是 `parsed:true, ok:false`）。

### `dhampir_asset_animation_info(asset_id)`

只查，不加载、不传字节：有则返回 `loaded` `info`，无则只返回 `loaded`；`loaded:true` 而没有 `info`（带 `error`）是**过渡态**（时间真值在、纹理不在），宿主**应当重新 load**。

`info` 的字段：`format`（`"gif"` / `"webp"`，按魔数）、`width`/`height`（画布尺寸）、`frame_count`、`loop_count`（文件里写的次数，**0 = 无限循环**）、`total_ms`（一圈总时长）、`frame_delays_ms`（逐帧延迟表，长度 = `frame_count`，**时间真值** —— 与工程 JSON 不一致时以它为准并写回资产表）、`bytes`（上传后估算字节）。

**约定与边界**：这一路不再 `set_bitmap`（`sources_for` 里照旧出现，变的只是谁提供像素）；滤镜类特效照旧，但**宿主侧逐帧加工没有位置**；整段常驻、不做 LRU 驱逐，超预算时 `load` **明确失败**（`ok:false`），上限见 `dhampir_core::animation::MAX_DECODED_BYTES`（256 MiB）；解码按**素材原生尺寸**，缩放交给 GPU 采样。

## v6 -> v7 变了什么

`overlay.subtitle_style` 多一个键 `anchor_bottom`：**文字块底边的归一化 y**（`0` = 画面顶边，`1` = 画面底边，缺省 `1.0` = 贴底）。左右安全宽改由 `safe_width_ratio` 显式接管后，`bottom_margin` 才回到「距底边多远」的本职；`anchor_bottom` 是归一化坐标，不是那个比例。缺省 1.0 不进返回体、也不进工程文件；**弹幕恒为 `1.0`**（`DanmakuSpec` 没有这个字段）。

## 那几行字是谁画的

**宿主画**：`overlay` 只说画哪几行、各占哪个矩形；栅格化是实现侧的事，所以两个宿主允许像素不同 —— 比对的是**结构**（项数/文本/矩形/泳道/帧区间），不是字形像素。

## 名单之外的那些导出

同一个模块里还有几个**只给本仓库验收用**的导出（离屏探针、sample project 出图）：它们在下面的名单里（守卫按模块扫），但**不承诺兼容**。分类表见 `docs/api-surface.md`。

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

**把一张掩码图交给预览宿主**：JS 侧 `fetch` → `createImageBitmap`，把**位图对象**交进来（与 `dhampir_project_set_bitmap` 同一个形状）。尺寸为 0 直接报错；**没有交进来的掩码会让 `draw` 报错**。调用方是 `web/engine.js` 的 `uploadMasks()`，在 `open()` 之后 await 一次。
