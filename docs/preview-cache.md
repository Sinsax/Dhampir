# 阶段 4/5：位图不必每帧重交 + 预渲染缓存（预览通路）

这一页记的是**预览那条链**（`web/engine.js` + `dhampir-wasm` 的 `timeline_host.rs`）上的
两件事：位图按内容标识复用（阶段 4）、整帧环形缓存（阶段 5），以及**它们各自的读数与
测不了的东西**。判定与读数都来自可重跑的命令，不来自"看了一眼"。

## 1. 阶段 4：位图不必每帧重交（放宽式）

新增导出 `dhampir_project_begin_frame(frame_plan_json)`：

```jsonc
{ "frame": 120,
  "sources": [ { "source": "a.mp4", "id": "g0:120" },   // id = 内容标识
               { "source": "stick.png", "id": "g0:4" } ],
  "cache":   { "enabled": true, "seconds": [3, 2], "max_mb": 256 } }
```

* **不在集合里的源**：位图关掉、纹理丢掉；
* **标识与手上那份一致**：留着 —— 连 `copy_external_image_to_texture` 都省掉（一次不拷）；
* 其余（标识变了 / 还没有位图 / **没给标识**）：丢掉手上那张，等调用方重交。

`clear_bitmaps()` 的语义**一个字都没改**，而且它连 `frame_ids` / `bitmap_ids` /
`source_uploads` 一起清干净 —— 于是**没调新导出的旧宿主永远拿不到内容标识**，而标识是
复用纹理的**唯一**判据 ⇒ 每帧照旧重传 ⇒ 行为与改前逐字节相同。
这一条有单测钉着（`没有声明标识就永远重传`、`来源集合的三种形状都收`），
不是靠注释说明。

## 2. 阶段 5：预渲染缓存（环形槽位表）

* 槽 = `(project_frame, GPU 纹理, valid)`，**GPU 纹理**（不是 ImageBitmap：wasm 会 `close()` 它）；
  槽位**惰性分配**（一次申请几百上千张纹理会把第一帧卡住）。
* 容量 = `min(前后 N 秒, 内存硬上限 256MB)`，绝对上界 4096 槽；
  窗口默认 **前 +3s / 后 -2s**（`seconds: [3, 2]`，一个数则按 3:2 摊）。
* **失效三维**（写死在两处）：
  1. 工程编辑 / 载入 —— `open` / `edit` / `undo` / `redo`；
  2. 字幕 / 弹幕变化 —— `set_subtitles`（经 `invalidate_text_uploads`）；
  3. **画布尺寸变化** —— `resize`（同一条路），另外 `draw` 开头还有一道
     "尺寸/格式对不上就整表作废"的兜底 —— 这一维以前出过事（画面被裁切）。
* 命中即交缓存：把槽位纹理 blit 到画布，**求值 / 渲染 / 文字三样全跳过**；
  `begin_frame` 顺手回报 `cached`，于是 JS 在 `prepare` **之前**就知道要跳过 seek。
* 填充与 present 分开：**pre-roll 命中什么也不做**（尤其是**不上屏**，上屏就是"画布闪到那一帧"）。

### 为什么配置由 JS 给，而不是环境变量

`dhampir-wasm` 跑在 `wasm32-unknown-unknown` 上，**那一侧没有环境** ——
`std::env::var` 恒返回 `Err`。所以"用 `VTEDIT_PREVIEW_CACHE_SECONDS/MB` 调"在浏览器里
是**死代码**；开关与窗口只能从看得见配置的那一侧（`engine.setPreviewCache`）递进来。
缓存**默认关**：打开它每帧要多一次"自有中间纹理 + blit 到画布"，只有命中真的发生才赚得回来。

## 3. 读数（都在真实 Chrome 里量，探针见下）

命令：

```powershell
ffmpeg -y -f lavfi -i "testsrc2=size=320x180:rate=30:duration=62" `
  -c:v libx264 -preset ultrafast -pix_fmt yuv420p -g 30 target/cache-probe/test.mp4
node scripts/preview-cache-probe.mjs            # 探针页面 + 判据；读数落 target/cache-probe/verdict.json
node scripts/check-dual-end.mjs --frames 0,30,89
cargo check -p dhampir-wasm --target wasm32-unknown-unknown
node scripts/run-wasm-tests.mjs
```

| 判据 | 改前 | 改后 | 命令 / 退出码 |
|---|---|---|---|
| 位图 GPU 上传（同样 20 帧、同一份贴纸内容） | **20 次** | **2 次**（复用 18 次，`submits_skipped` 18） | `preview-cache-probe` exit 0 |
| 整圈（同一批 24 帧要第二遍） | 未命中中位 **2.2–2.4ms**（其中 seek 1.3ms） | 命中中位 **0.3–0.4ms**（seek 0） | 同上，命中 24/24 |
| 连续播放 1800 帧（60s @30fps）+ 每 3 帧一条 pre-roll | — | 3.85s 跑完，最坏一帧 8.3ms，宿主命中 647 | 同上，**无 panic、无未捕获错误** |
| 并发（不 await 连发 3 次 seek + 1 条 pre-roll） | — | 全部串行跑完，之后仍能继续画 | 同上 |
| 双端一致性（离屏合成路径） | — | 最差 SSIM **1.000000** | `check-dual-end` exit 0 |
| wasm 单测 | — | 17/17 通过（含两条阶段 4 新不变量） | `run-wasm-tests` exit 0 |
| wasm 编译 | — | 0 error（只剩原有 1 条 warning） | `cargo check` exit 0 |

## 4. 测不了的、以及为什么（不要当成通过）

* **"命中交的是同一帧"这一条（像素级）在本机量不了。**
  `web/cache-probe.html` 的三条取回路径（`toDataURL`、`createImageBitmap(canvas)`、
  `toDataURL` → `<img>` 解码）里，`createImageBitmap` 交全透明，另外两条交
  **不透明黑**（`ink` 全画面非零但 RGB 全 0、A=255），而且**缓存开与缓存关都一样** ——
  也就是说这不是阶段 5 那次 blit 造成的。
  同一轮里的两个**环境对照都通过**：原生 WebGPU 画布清成红色能取回 `(255,0,0,255)`；
  `copyExternalImageToTexture`（贴纸那条路用的就是它）能取回 `(0,255,0,255)`。
  视频也确实解码了（`readyState=4`、320×180），合成报了 2 层（`opacity=1`）。
  所以"画布是黑的"这件事我**没有结论**：既不能证明缓存交的帧是错的，也不能证明是对的。
  要在真实浏览器（用户在用的那个）里看这一条。
* `scripts/check-dual-end.mjs` 的浏览器腿走的是 `web/synthetic.html` →
  `dhampir_sample_project_render_png`（**离屏合成**），**完全不经过** `dhampir_project_draw`。
  所以它 1.000000 只能证明"共享 core 渲染没退化"，**证明不了预览这条链** ——
  阶段 4/5 改的恰恰是后者。

## 5. 已知风险（代码读出来的，未在真机上验证）

1. **pre-roll 与调用方的 `onPrepared`**：预渲染某一帧时不会调调用方的
   `onPrepared`（`engine.seek(f, {preroll:true})` 不带这个回调），而
   `begin_frame` 已经按**预渲染那一帧**的标识把贴纸位图丢掉了 ⇒ 预渲染出来的
   缓存帧里**没有贴纸**（或者留着上一帧的贴纸）。之后命中就会交一帧错的。
   建议：工程里有"调用方交位图的源"时**不要排 pre-roll**，或者让调用方给出
   与帧号对应的提交回调。
2. **`followPlayback = true`（下游宿主的默认）+ video 模式**下 `prepare` 不 seek ⇒
   预渲染 `f+1` 时拿到的是视频**当前**那幅画面，却被存成 `f+1` 的缓存帧。
   之后命中 `f+1` 会显示**另一帧**的画面。`pin` 那条判据护住的是"位图标识"，
   护不住"缓存帧的内容"。
3. 缓存**默认关**，而下游宿主目前没有调 `setPreviewCache` / `schedulePreroll` ⇒
   产品里既不会有命中，也不会有"整圈下降"。要看到工具条上的命中数，需要下游宿主侧打开它。
4. 阶段 4 的收益要 `followPlayback = false`（引擎才会声明内容标识）。
   产品里 `followPlayback = true` ⇒ 标识一律不给 ⇒ **产品里贴纸仍然是每帧重传**。
   `pin` 这条规矩本身是对的（跟随播放时 `source_frame` 并不标识内容），
   但"贴纸不必每帧重传"这件事因此要由**调用方**把内容标识交进来
   （例如 `setSourceBitmap(source, bitmap, id)` 用贴纸帧号），这是下游宿主侧的改动。
