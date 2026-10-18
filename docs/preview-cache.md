# 阶段 4/5：位图不必每帧重交 + 预渲染缓存（预览通路）

预览链路（`web/engine.js` + `dhampir-wasm` 的 `timeline_host.rs`）：位图按内容标识复用（阶段 4）、整帧环形缓存（阶段 5）。读数都来自可重跑的命令。
## 1. 阶段 4：位图不必每帧重交（放宽式）
新增导出 `dhampir_project_begin_frame(frame_plan_json)`：
```jsonc
{ "frame": 120,
  "sources": [ { "source": "a.mp4", "id": "g0:120" }, { "source": "stick.png", "id": "g0:4" } ],  // id = 内容标识
  "cache":   { "enabled": true, "seconds": [3, 2], "max_mb": 256 } }
```
* 不在集合里的源：位图关掉、纹理丢掉。
* 标识与手上那份一致：留着 —— 连 `copy_external_image_to_texture` 都省掉。
* 其余（标识变了 / 还没有位图 / **没给标识**）：丢掉手上那张，等调用方重交。
* `clear_bitmaps()` 语义**一个字都没改**，连 `frame_ids` / `bitmap_ids` / `source_uploads` 一起清 ⇒ 没调新导出的旧宿主拿不到标识 ⇒ 每帧照旧重传，行为逐字节相同（单测：`没有声明标识就永远重传`、`来源集合的三种形状都收`）。
## 2. 阶段 5：预渲染缓存（环形槽位表）
* 槽 = `(project_frame, GPU 纹理, valid)`；纹理是 GPU 的（不是 ImageBitmap，wasm 会 `close()`），槽位**惰性分配**。
* 容量 = `min(前后 N 秒, 内存硬上限 256MB)`，绝对上界 4096 槽；窗口默认 **前 +3s / 后 -2s**（`seconds: [3, 2]`，一个数则按 3:2 摊）。
* **失效三维**（写死在两处）：工程编辑 / 载入（`open` / `edit` / `undo` / `redo`）；字幕 / 弹幕变化（`set_subtitles`，经 `invalidate_text_uploads`）；画布尺寸变化（`resize`，`draw` 开头另有「尺寸/格式对不上就整表作废」的兜底）。
* 命中即交缓存：槽位纹理 blit 到画布，**求值 / 渲染 / 文字三样全跳过**；`begin_frame` 顺手回报 `cached`，JS 在 `prepare` 之前就知道要跳过 seek。
* 填充与 present 分开：**pre-roll 命中什么也不做**（尤其是**不上屏**）。
* 配置只能由 JS 给（`engine.setPreviewCache`）：wasm32 侧 `std::env::var` 恒返回 `Err`，`VTEDIT_PREVIEW_CACHE_SECONDS/MB` 在浏览器里是死代码。缓存**默认关**。
## 3. 读数
```powershell
ffmpeg -y -f lavfi -i "testsrc2=size=320x180:rate=30:duration=62" `
  -c:v libx264 -preset ultrafast -pix_fmt yuv420p -g 30 target/cache-probe/test.mp4
node scripts/preview-cache-probe.mjs            # 探针页面 + 判据；读数落 target/cache-probe/verdict.json
node scripts/check-dual-end.mjs --frames 0,30,89
cargo check -p dhampir-wasm --target wasm32-unknown-unknown
node scripts/run-wasm-tests.mjs
```
| 判据 | 读数 |
|---|---|
| 位图 GPU 上传（同样 20 帧、同一份贴纸内容） | **20 次 → 2 次**（复用 18，`submits_skipped` 18） |
| 整圈（同一批 24 帧要第二遍） | 未命中中位 **2.2–2.4ms**（其中 seek 1.3ms）→ 命中中位 **0.3–0.4ms**（seek 0），命中 24/24 |
| 连续播放 1800 帧（60s @30fps）+ 每 3 帧一条 pre-roll | 3.85s 跑完，最坏一帧 8.3ms，宿主命中 647，无 panic、无未捕获错误 |
| 双端一致性 / wasm 单测 / 编译 | 最差 SSIM **1.000000**（`check-dual-end` exit 0）；17/17 通过；0 error（剩原有 1 条 warning） |
## 4. 测不了的、以及为什么（不要当成通过）
* **「命中交的是同一帧」像素级在本机量不了**：`web/cache-probe.html` 三条取回路径都交不出可判读像素（`createImageBitmap` 全透明，另两条不透明黑），缓存开与关都一样；同轮两个环境对照都通过。要在真实浏览器（用户在用的那个）里看。
* `scripts/check-dual-end.mjs` 的浏览器腿走 `web/synthetic.html` → `dhampir_sample_project_render_png`（**离屏合成**），**完全不经过** `dhampir_project_draw` ⇒ 1.000000 只证明共享 core 渲染没退化，证明不了预览这条链。
## 5. 已知风险（代码读出的，未在真机验证）
1. **pre-roll 不调调用方的 `onPrepared`**，而 `begin_frame` 已按预渲染那一帧的标识丢掉贴纸位图 ⇒ 缓存帧里没有贴纸（或留着上一帧的），之后命中会交一帧错的。建议：有「调用方交位图的源」时不要排 pre-roll。
2. **`followPlayback = true` + video 模式**下 `prepare` 不 seek ⇒ 预渲染 `f+1` 存的是当前画面，命中 `f+1` 显示另一帧；`pin` 保护的是位图标识，护不住缓存帧内容。
3. 缓存默认关，下游宿主目前没调 `setPreviewCache` / `schedulePreroll` ⇒ 产品里既没有命中，也没有整圈下降。阶段 4 的收益要 `followPlayback = false`（引擎才会声明内容标识）；`followPlayback = true` 时贴纸仍然每帧重传。
