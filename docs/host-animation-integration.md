# 动图（GIF / 动画 WebP）原生解码 —— 宿主接入指南
**适用产物**：`dhampir-0.2.0+`（`project_schema = 1`、`host_api >= 6`）。产物名 = `dhampir-<版本>-<平台>.zip`；契约号看 `project_schema` / `host_api`，不看名字里的产品版本。
设计理由见 [../plan/dhampir-gif-native-decode-proposal.md](../plan/dhampir-gif-native-decode-proposal.md)，形状权威见 [host-api.md](host-api.md)。
## 一、这一版变了什么
**动图解码从宿主搬到引擎**：交 GIF 原始字节，引擎自己解、自己常驻显存、按帧号供帧，「分离为帧」不再需要。

| 面 | 变没变 |
|---|---|
| 工程文件形状（`project_schema = 1`） | 一个键都没动，老工程照读 |
| `dhampir_host_api_version` | **5 → 6** |
| 导出面 | 既有（`open` / `draw` / `sources_for` / `set_bitmap` …）全都不变；新增两条（§二） |
**判据**：只调老接口时，对着新产物行为与上一版逐字节相同 —— 动图是加法，不是替换。
## 二、API 契约
### 2.1 先问版本
```js
const version = engine.dhampir_host_api_version();   // 期望 >= 6；< 6 = 不支持动图，走老路（逐帧喂位图）
```
### 2.2 交出动图：`dhampir_asset_load_animation(asset_id, bytes)`
* `asset_id` = 工程 `assets[].id`（与求值时用的源标识一致）；`bytes` = 动图原始字节（JS 侧 `Uint8Array`），**按魔数认格式，不看扩展名**；同 id 重复调用 = 替换（幂等）。

返回 `{ "parsed": true, "ok": true, "info": { … } }`，失败 `{ "parsed": true, "ok": false, "error": "…" }`。`parsed=false` = 不是动图（静态图 / 视频 / 坏魔数），`parsed=true, ok=false` = 认得出但解不开（截断、超限）—— 分开提示。

| `info` 键 | 类型 | 含义 |
|---|---|---|
| `format` | `"gif" \| "webp"` | 按魔数认出来的 |
| `width` / `height` | 整数 | 画布尺寸（每一帧都是整张画布） |
| `frame_count` | 整数 | 帧数 |
| `loop_count` | 整数 | 文件里写的循环次数，**0 = 无限** |
| `total_ms` | 整数 | 一圈总时长（毫秒） |
| `frame_delays_ms` | 整数数组 | 逐帧延迟表，长度 == `frame_count` |
| `bytes` | 整数 | 上传后占多少显存（估算，不含驱动对齐） |
### 2.3 查在不在：`dhampir_asset_animation_info(asset_id)`
只查，不加载、不传字节。`{loaded:true, info:{…}}`；`{loaded:false}` 与 `{loaded:true, error:"…"}`（过渡态）都表示**要重新 load**。
## 三、宿主侧要改的三件
### 3.1 调用点：一次性交字节
```js
const bytes = new Uint8Array(await (await fetch(gifUrl)).arrayBuffer());
const r = JSON.parse(engine.dhampir_asset_load_animation(assetId, bytes));
if (!r.ok) { showProblem(r.parsed ? '动图损坏' : '这不是动图', r.error); }
```
**一份动图 = 一次调用**：同一个 `asset_id` 被多个图层 / 多个 `source_in` 引用时只 load 一次；这一路**不要再调 `set_bitmap`**，否则贴纸被钉死在某一帧，画面看起来只是「动图不动了」。
### 3.2 时间真值以引擎的 `frame_delays_ms` 为准
工程 JSON 里那份是宿主录入时探测的，引擎返回的是解码器从文件读出的；不一致时**用引擎的**。映射与像素用两张表会让动图越播越偏，而单帧完全看不出来。
### 3.3 `sources_for` 里仍会出现动图层
`dhampir_project_sources_for` 照旧包含动图层，**不能假定清单里每个源都去 seek 一个 video 元素**；已 load 的 `asset_id` 直接跳过 seek / 栅格化：
```js
for (const s of sourcesFor) {
  if (loaded.has(s.source)) continue;   // load 成功的 asset_id，引擎自己供帧
  await seekAndSubmitBitmap(s);
}
```
## 四、CLI 侧（出片）
宿主不需要改任何 CLI 调用：动图素材登记成 `kind: "image_sequence"` + `frame_count` + `loop_source: true`，`dhampir render` 自己按魔数认 GIF、解码、按帧号供帧，时间真值同样以解码结果为准。观测：结束的 NDJSON 里 `opened_streams` **只数视频流** ——「1 个背景视频 + N 个动图贴纸」应为 **1**，不是 1+N。
## 五、实测数据（V-Trim 的 clip-18）
| 文件 | 画布 | 帧数 | 总时长 | 显存 |
|---|---|---|---|---|
| `叹号.gif` | 500x500 | 38 | 1140 ms | 36.2 MiB |
| `思考(认真地).gif` | 500x500 | 32 | 960 ms | 30.5 MiB |
| `电风扇2.gif` | 500x500 | 32 | 960 ms | 30.5 MiB |
| `笑.gif` | 500x500 | 32 | 960 ms | 30.5 MiB |
全部 30 ms 均匀延迟、无零延迟帧、无限循环；引擎延迟表与 `data/sticker_frames.json` 的 `delays` 逐值相同；数据量 **46.2 MiB（逐帧 base64 PNG）→ 8.4 MiB（GIF 原文件）= 4.1x**。**性能**：`218 ms / 4 张` 出自 `SIZE = 4` 合成 fixture；真素材（500×500）实测 **1.4x**（747 → 542 ms / 5 张，逐帧上传 → 一张数组纹理）。**不要再引 10.8x**。
## 五之二、失败归因复盘（2026-10-02）
* 探针把 `tFetched` 打在 `fetch` 之前 ⇒「取字节 0ms」、`fetch` 被算进解码，结论作废。
* 对照组用了未优化的 `pkg.bak-8MB` 报出 10.8x；用 `git worktree` 重建旧版后是 **1.4x** —— 基线必须来自版本控制。
## 六、两条边界
### 6.1 显存按素材原生尺寸算
500x500、32 帧 = **30.5 MiB**，四张 = **127.8 MiB**；上限 256 MiB ⇒ **8 张以上贴纸的工程会撞上限**，`load_animation` 明确失败（`ok: false`，理由是预算）。失败要显示出来（吞掉的症状是贴纸整个不见了）；素材多时先用较小图源，或宿主侧预缩后再交。
### 6.2 位置与缩放仍由宿主定
引擎只提供像素；`transform.x/y/scale` 用**文档坐标系**（工程 `render_hints`），不替你避免出界。例：500x500 贴纸 `scale: 0.62` 放在 `x: 430`（640 宽画布）会被右边缘切掉一半。
## 七、没有验的
* **浏览器预览侧没用真素材跑过**：§五 的数字是 native/Vulkan 出的；首次在浏览器跑真 GIF 时重点看按帧动与 `sources_for`（§3.3）。
* **逐帧加工类特效未覆盖**；**动 WebP 没有真实素材**（只有合成单测）；**`frame.delay == 0`** 引擎保留 0，宿主若探到零延迟素材要核对口径。
## 八、产物与接入
```text
dhampir-<版本>-<平台>/
  preview/engine.js                 ← 浏览器入口（不依赖任何前端框架）
  preview/pkg/dhampir_wasm.js  preview/pkg/dhampir_wasm_bg.wasm   ← wasm-pack 输出
  bin/dhampir.exe  VERSION  LICENSE  THIRD-PARTY-LICENSES.md
```
`VERSION` 里是 version / project_schema / host_api / git / 构建时间 / 平台。把整个目录放到 `<程序目录>/dhampir/`，再设：
```text
VTEDIT_DHAMPIR_PREVIEW=<程序目录>/dhampir/preview
VTEDIT_DHAMPIR_CLI=<程序目录>/dhampir/bin/dhampir.exe
```
**启动时校验 `VERSION` 里的 `host_api >= 6`** —— 低于 6 的产物没有那两条导出，调用会拿到 `undefined`。
