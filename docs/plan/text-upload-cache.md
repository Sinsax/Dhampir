# 文字行位图的**上传缓存**（第二阶段 ②，待做）

> 状态：**方案已定死，代码未动**。写在这里而不是对话里，是因为这个改动跨函数签名，
> 半应用是最坏的形态 —— 下一个人（或下一轮）照着这份机械执行即可。

## 要解决的问题（有读数）

`web/engine.js` 的 `prepareText` 每帧把可见的行栅格化并上传。已做的两件事与它们的极限：

| 已做 | 效果 |
|---|---|
| 宿主侧**栅格化缓存**（`RASTER_CACHE`，`canvas` 跨帧复用）| `textMs` **3.7 → 3.3 ms**（双跑实测，clip-25，2 行字幕）|
| — | ⇒ **只省掉 11%** —— 字体渲染**不是**大头 ✗ |

剩下那 3.3ms 在哪：`upload_text_bitmaps()`（`timeline_host.rs:541`）**每帧、每一行**
无条件 `create_texture` + `copy_external_image_to_texture` —— **没有任何"这张没变就跳过"**。
宿主侧的缓存管不到它（位图仍是每帧新建的 `ImageBitmap`）。

**宿主一个人省不掉**：就算宿主不交，wasm 手里还是上一帧那张位图，
`upload_text_bitmaps` 照样重建纹理、照样重传。

## 一个有利的前提（已确认）

- `dhampir_project_clear_bitmaps` 只清 `host.bitmaps`（**源**位图），**不碰**
  `text_bitmaps` / `danmaku_bitmaps`（`timeline_host.rs:1463-1471`）。
- 行位图的清理函数 `dhampir_project_clear_text_bitmaps` **早就因为"一个调用方都没有"
  被删掉了**（见 `timeline_host.rs:101` 的注释）。

⇒ **行位图本来就跨帧保留**。所以这个改动是"给它加个脏标记 + 留住已上传的纹理"，
**不是**改生命周期 —— 比看起来小。

## 改动清单（5 处 + 3 个调用点）

> ⚠️ **必须一次做完，拆不开。** 试过拆法：只加字段 + 初始化会被 `dead_code` 抓
> （字段"从未被读"）；加了写入也一样（仍"从未被读"）。本仓是 0 警告标准，
> 所以**唯一无警告的最小单元就是下面这 5 处 + 3 个调用点一起落地**。
> 想分步验证的话，分的是**判据**（cargo check / SSIM / textcost），不是**改动**。

```rust
// ① ProjectHost 加字段（text_bitmaps 附近，L279/297 之后）
text_dirty: std::collections::HashSet<u32>,
danmaku_dirty: std::collections::HashSet<u32>,
text_uploads: Vec<Option<(wgpu::Texture, wgpu::TextureView, (u32, u32))>>,
danmaku_uploads: Vec<Option<(wgpu::Texture, wgpu::TextureView, (u32, u32))>>,

// ② 初始化处（L1420 附近）补上这四个字段

// ③ dhampir_project_set_text_bitmap（L1938）在 insert 之后：
host.text_dirty.insert(index);

// ④ dhampir_project_set_danmaku_bitmap（L1968）同理

// ⑤ upload_text_bitmaps（L541）改签名，把已上传的纹理跨帧留住：
fn upload_text_bitmaps(
    device, queue, format, lines,
    bitmaps: &HashMap<u32, web_sys::ImageBitmap>,
    cache: &mut Vec<Option<(wgpu::Texture, wgpu::TextureView, (u32, u32))>>,
    dirty: &std::collections::HashSet<u32>,
) -> Vec<Option<(wgpu::Texture, wgpu::TextureView, (u32, u32))>> {
    for index in 0..lines.len() {
        if !dirty.contains(&(index as u32)) {
            out.push(cache.get(index).cloned());   // **复用上一帧那张** ✓
            continue;
        }
        // …原来的 create_texture + copy_external_image_to_texture…
    }
    cache.clone_from(&out);   // 交给下一帧
    out
}
```

**三个调用点跟着改**：`draw` 里两处（`timeline_host.rs:784` / `791`）
与 `text_probe` 一处（`L2031`）。

## 宿主那一半（`web/engine.js`）

**只交变化的行** —— 判据**已经有了**：第 ① 步做的 `RASTER_CACHE` 的 key 就是
"这一行变没变"。命中缓存时**不要调** `set_text_bitmap(index, …)`，
让它待在 `text_bitmaps` 里不动 → wasm 那边 `dirty` 不含它 → 复用纹理。

⚠️ **别忘了**：`RASTER_CACHE` 只覆盖"栅格化"；这里要的是"**别提交**"，
所以判据要在 `rasterizePlacements` 里单独取（不能只看 `createImageBitmap` 是否新建）。

## 怎么验（判据都已就绪）

```text
每步都能单独判：
  cargo check -p dhampir-wasm                     # 改动落地后必须 0 警告
  wasm-pack build crates/dhampir-wasm --target web --out-dir www/pkg --dev
  node scripts/check-dual-end.mjs --frames 0,30,89   # 必须仍是 SSIM 1.000000
  node scripts/run-guards.mjs                        # 18/20（红的两条照旧）
  out/textcost-probe.mjs（clip-25, from=1150, n=200） # textMs 应显著低于 3.3ms
```

**基线已量**：`textMs = 3.3ms`（缓存开着）/ `3.7ms`（缓存关掉，双跑实测）。
所以这个改动**天然可量**。

## 一条操作纪律（这几轮换来的）

要临时改哪儿来量 before/after，**先记 sha256，改完用哈希核验回滚** ——
不许凭"我记得改回来了"。这条是踩过两次之后写下来的：
一次弄丢了别人未提交的文件，一次没有基线哈希。
