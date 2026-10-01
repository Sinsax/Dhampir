# 动图（GIF / 动画 WebP）原生解码 —— 下游接入交接

**适用产物**：`dist/dhampir-1+2fd7d96`（`project_schema=1`、**`host_api=6`**、git `2fd7d96`）

这份文件只回答一件事：**V-Trim 要把动图交出去、要看它动起来、要保证预览与成片一致，
分别该调什么、注意什么。** 设计理由在 [../plan/dhampir-gif-native-decode-proposal.md](../plan/dhampir-gif-native-decode-proposal.md),
形状权威在 [host-api.md](host-api.md)。

---

## 一、这一版变了什么（一句话）

**动图的解码从 V-Trim 搬到了引擎。** 以前是 V-Trim 自己把 GIF 拆成帧、逐帧栅格化成 PNG、
每帧喂给 wasm；现在是把 **GIF 原始字节**交给引擎，引擎自己解、自己常驻显存、自己按帧号供帧。

对 V-Trim 的意义：

* **贴纸的「分离为帧」这一步可以整个去掉**；
* **出片（CLI）与预览（浏览器）用的是同一份解码器**，所以两边贴纸相位不会各算各的；
* 实测数据量从「逐帧 PNG」降到「GIF 原文件」——**这批素材上是 4.1x**（见 §五）。

### 兼容性

| 面 | 变没变 |
|---|---|
| 工程文件形状（`project_schema = 1`） | **一个键都没动**，老工程照读 |
| `dhampir_host_api_version` 返回值 | **5 → 6** |
| 既有导出（`open` / `draw` / `sources_for` / `set_bitmap` …） | **全都不变** |
| 新增导出 | 两条（见 §二） |

**判据**：V-Trim 只调老接口时，对着新产物**行为与上一版逐字节相同**——
动图这条路是**加法**，不是替换。

---

## 二、API 契约

### 2.1 先问版本

```js
const version = engine.dhampir_host_api_version();   // 期望 6
if (version < 6) { /* 这份产物不支持动图，走老路（逐帧喂位图） */ }
```

### 2.2 交出动图：`dhampir_asset_load_animation(asset_id, bytes)`

* `asset_id`：**必须是求值里用的那个源标识**，也就是工程 `assets[].id`。
  引擎按它接管这一路的纹理。
* `bytes`：**动图原始字节**（JS 侧是 `Uint8Array`）。**格式按魔数认，不看扩展名** ——
  `kind` 是录入时的猜测，字节是事实。
* **同 id 重复调用 = 替换**（幂等）。编辑里换了素材文件时重新 load 会生效。

返回两种形态：

```jsonc
// 成功
{ "parsed": true, "ok": true, "info": { … } }
// 失败
{ "parsed": true, "ok": false, "error": "…" }
```

`parsed` 与 `ok` 分两件事报：**`parsed=false` 是「这不是动图」（静态图 / 视频 / 损坏的魔数），
`parsed=true, ok=false` 是「认得出是动图但解不开」**（截断、超限）。
V-Trim 应当把这两种情况分开提示，它们的处置不一样。

`info` 的字段：

| 键 | 类型 | 含义 |
|---|---|---|
| `format` | `"gif" \| "webp"` | 按魔数认出来的 |
| `width` / `height` | 整数 | 画布尺寸（每一帧都是整张画布） |
| `frame_count` | 整数 | 帧数 |
| `loop_count` | 整数 | 文件里写的循环次数；**0 = 无限** |
| `total_ms` | 整数 | 一圈总时长（毫秒） |
| `frame_delays_ms` | 整数数组 | **逐帧延迟表**，长度 == `frame_count` |
| `bytes` | 整数 | 上传后占多少显存（估算，不含驱动对齐） |

### 2.3 查在不在：`dhampir_asset_animation_info(asset_id)`

**只查，不加载、不传字节。** 页面重放或宿主重建之后用它决定要不要重新 load。

```jsonc
{ "loaded": true,  "info": { … } }   // 引擎手里有
{ "loaded": false }                    // 引擎手里没有 —— 要重新 load
{ "loaded": true,  "error": "…" }      // 过渡态：有时间真值但纹理不在，**要重新 load**
```

---

## 三、V-Trim 侧要改的（就三件）

### 3.1 调用点：把「逐帧喂位图」换成「一次性交字节」

```js
// 现在（动图那条路）
for (const pngBase64 of stickerFrames[name].frames) {
  engine.dhampir_project_set_bitmap(name, await createImageBitmap(blob));
}

// 改成：一次就够 —— 播放期零请求、零上传
const bytes = new Uint8Array(await (await fetch(gifUrl)).arrayBuffer());
const r = JSON.parse(engine.dhampir_asset_load_animation(assetId, bytes));
if (!r.ok) { showProblem(r.parsed ? '动图损坏' : '这不是动图', r.error); }
```

注意两点：

* **「一份动图 = 一次调用」**，不是每帧一次，也不是每个图层一次。同一个 `asset_id` 在时间线里
  被多个图层引用（甚至不同 `source_in`）时**只 load 一次**。
* **这一路不要再调 `set_bitmap`**。引擎自己供帧之后，宿主再喂位图会把贴纸
  **钉死在某一帧**上——画面看起来只是「动图不动了」，很难查。

### 3.2 时间真值：用引擎返回的 `frame_delays_ms` 覆盖你探测的那份

这是**必须照做**的一条。

工程 JSON 里的 `frame_delays_ms` 是 V-Trim 录入时探测的；
引擎返回的这份是**解码器从文件本身读出来的**。两者不一致时**以引擎的为准**。

理由：映射用一张表、像素用另一张表，表现是动图**越播越偏**——
而单帧完全看不出来。

好消息是**这批素材上两份逐值相同**（见 §五），所以这条改动在现有数据上不产生行为变化，
它是**防未来的漂移**。

### 3.3 `sources_for` 的清单里会出现动图层（**要注意的坑**）

`dhampir_project_sources_for` 返回的是**求值出来的源清单**，
而它会**照旧包含动图层**（引擎没有把动图层从清单里过滤掉）。

也就是说：**不能假定「清单里有的源都去 seek 一个 video 元素」**。
对已经 load 过的动图 `asset_id`，V-Trim 应当直接跳过 seek / 栅格化那一步。

一个能用的判据（不需要额外往返）：

```js
const loaded = new Set();            // load 成功的 asset_id
for (const s of sourcesFor) {
  if (loaded.has(s.source)) continue;      // 引擎自己供帧，别 seek
  await seekAndSubmitBitmap(s);
}
```

> 这一条以后可能会改（把动图层从 `sources_for` 里过滤掉），但**改的是形状语义**，
> 要再升一次版本，所以这一版请按上面这样处理。

---

## 四、CLI 侧（出片）

**V-Trim 不需要为动图改任何 CLI 调用。** 出片那条路上的接线在引擎内部：

* 工程里动图素材登记成 `kind: "image_sequence"` + `frame_count` + `loop_source: true`；
* `dhampir render` 会**自己**按魔数认出 GIF、解码、上传、按帧号供帧；
* 时间真值同样以解码结果为准。

一个可用来确认「引擎真的没走 ffmpeg 解码动图」的观测：出片结束的 NDJSON 里有 `opened_streams` ——
**它只数视频流**。一段「1 个背景视频 + N 个动图贴纸」的片子，
`opened_streams` 应当是 **1**，不是 1+N。

---

## 五、实测数据（V-Trim 自己的素材，clip-18）

这批数字来自**你的素材**（`output/clips/clip-18/assets/stickers/`，四个 GIF）:

| 文件 | 画布 | 帧数 | 总时长 | 显存 |
|---|---|---|---|---|
| `叹号.gif` | 500x500 | 38 | 1140 ms | 36.2 MiB |
| `思考(认真地).gif` | 500x500 | 32 | 960 ms | 30.5 MiB |
| `电风扇2.gif` | 500x500 | 32 | 960 ms | 30.5 MiB |
| `笑.gif` | 500x500 | 32 | 960 ms | 30.5 MiB |

**全部是 30 ms 均匀延迟、无零延迟帧、无限循环。**

**时间真值对齐**：引擎读出的延迟表与 `data/sticker_frames.json` 里的 `delays` 数组
**逐值相同**（叹气 38 个 30、其余各 32 个 30）。

**数据量对比**：

| | 现状（逐帧 base64 PNG） | 引擎原生解码 |
|---|---|---|
| 帧数 | 134 | 134（一致） |
| 延迟 | 全 30 ms | 全 30 ms（一致） |
| 数据量 | 46.2 MiB（`sticker_frames.json`） | **8.4 MiB**（GIF 原文件） |
| 膨胀 | **4.1x** | 1x |

**性能**（Vulkan 真机，四张贴纸）：解码 + 上传合计 **218 ms**（平均 55 ms/张）。

---

## 六、两条要注意的边界（别踩）

### 6.1 显存是按**素材原生尺寸**算的

500x500、32 帧的一张 GIF 就是 **30.5 MiB** 显存，四张 = **127.8 MiB**。
当前上限是 256 MiB —— **一个引用 8 张以上贴纸的工程就会撞上限**，
那时 `load_animation` 会**明确失败**（`ok: false`，理由是预算），而不是悄悄换出几帧。

V-Trim 侧要做的：

* **失败要显示出来**，别吞掉——吞掉的症状是「贴纸整个不见了」；
* 如果素材确实很多，考虑**先用较小的图源**（贴纸通常显示得比 500x500 小）
  或在 V-Trim 侧预缩一份再交（引擎按它收到的字节解码）。

### 6.2 贴纸的位置与缩放仍然由 V-Trim 定

引擎**只提供像素**，不管摆在哪、多大。`transform.x/y/scale` 的坐标系是
**文档坐标系**（工程 `render_hints`），引擎**不会替你避免出界**。

实测踩到过：把 500x500 的贴纸按 `scale: 0.62` 放在 `x: 430`（640 宽的画布上），
**贴纸被右边缘切掉了一半**。画面看起来像「贴纸生成失败」，其实是摆位问题。

---

## 七、这一版**没有**验的（诚实清单）

* **浏览器预览侧没有用真素材跑过。** wasm 接线编译通过（含 wasm32 目标）且与 native 逻辑同构，
  §五 的数字是 **native/Vulkan** 出的。V-Trim 接上后**第一次在浏览器里跑真 GIF 时请重点看**：
  动图是否按帧动、以及 `sources_for` 那一条（§3.3）。
* **逐帧加工类特效未覆盖**：模糊 / 色彩 / 扭曲这些引擎特效照常工作，
  但「宿主自己对贴纸做的逐帧加工」在动图这条路上没有位置——引擎供的是源纹理。
* **原生的 WebP 动图没有真实素材验过**：clip-18 里全是 GIF。
  WebP 的解码路径有单测覆盖（合成的动画 WebP fixture），但**没有真实 WebP 素材跑过**。
* **`frame.delay == 0` 的处理**：引擎**保留 0**（解码层不猜）。这批素材没有零延迟帧，
  所以暂时不显现；如果 V-Trim 侧探到过零延迟的素材，**两份实现要对一下口径**。

---

## 八、附：产物内容与接入方式

```
dhampir-1+2fd7d96/
  preview/
    engine.js                 ← 浏览器入口（不变量：不依赖任何前端框架）
    pkg/dhampir_wasm.js       ← wasm-pack 输出
    pkg/dhampir_wasm_bg.wasm
  bin/dhampir.exe             ← 出片用
  VERSION                     ← project_schema=1 / host_api=6 / git / 构建时间 / 平台
  LICENSE  THIRD-PARTY-LICENSES.md  licenses/
```

接入：把整个目录放到 `<程序目录>/dhampir/`，然后设

```
VTEDIT_DHAMPIR_PREVIEW=<程序目录>/dhampir/preview
VTEDIT_DHAMPIR_CLI=<程序目录>/dhampir/bin/dhampir.exe
```

**启动时请校验 `VERSION` 里的 `host_api >= 6`**——
低于 6 的产物没有那两条导出，调用会拿到 `undefined`。
