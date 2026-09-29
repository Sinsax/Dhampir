# 入口接缝：`FrameSource` / `FrameSink` 的现状与缺口

> 这份文件是**一次勘察的结论**，不是计划。它把"两个入口（预览 / 导出）怎么共用一条渲染图"
> 这件事的**现状、缺口、以及缺口为什么还没补**记下来，免得下一个人（或下一轮）重新发现一遍。

## 结论先说

**接缝早就存在，而且被文档当作本架构的 1 号接缝。缺的是 native 侧的实现。**

| 位置 | 内容 |
|---|---|
| `dhampir-core/src/lib.rs:13` | 文档把 `io::FrameSource` / `io::FrameSink` 列为第 1 号接缝 —— **"帧从哪来、画到哪去"** |
| `dhampir-core/src/io.rs:38` | `pub trait FrameSource { fn frame_view(&mut self, device, frame) -> TextureView }` |
| `dhampir-core/src/io.rs:55` | `pub trait FrameSink { fn acquire(&mut self, device) -> TextureView; fn finish(&mut self, frame) }` |
| `dhampir-core/src/render/blit.rs:4` | 共享的 blit：**"区别只在 `FrameSink` 的实现（canvas surface vs 离屏纹理）"** |

`io.rs` 里那两句文档就是设计意图本身：

```text
FrameSink：帧的去向。预览与导出共用上游渲染图，区别只在这里。
  - wasm 侧：canvas surface，画给用户看
  - native 侧：离屏 texture，交给 readback 读回、再交给编码器
```

**两个 trait 只收/返 `wgpu` 类型** —— 平台类型（`web-sys`、`<video>`）渗不进 `dhampir-core`，
这正好满足 core 那条铁律（零 `#[cfg]`、依赖无环）。**这个位置选得是对的，不用改。**

## 实现情况

| 侧 | `FrameSource` | `FrameSink` |
|---|---|---|
| **wasm** | ✅ `crates/dhampir-wasm/src/preview.rs:94` —— `impl FrameSource for VideoFrameSource` | ✅ `preview.rs:336` —— `impl FrameSink for CanvasFrameSink`（`web.rs:198` 的注释称它是"`FrameSink` 的第一个真实实现形态"）|
| **native** | ❌ 无 | ❌ **无** |

native 侧现在**不走这个抽象**：`dhampir-worker/src/pipeline.rs:706` 就地 `create_texture`，
画完直接交给编码器 —— 功能上没问题，但"预览与导出共用渲染图"这句话在 native 侧**没有兑现**。

## 缺口具体是什么

要补的**不是**一对 trait（它们在那儿），而是：

1. **给 `DecodingSources`（`pipeline.rs:783`）实现 `FrameSource`**
   —— 委托现有的取帧/上传路径（`pipeline.rs:705` 的 `upload`），不新建第二套。

2. **新建一个 native 侧的帧槽类型，实现 `FrameSink`**
   —— 这是**唯一需要新写的类型**。它该是"一块离屏纹理"（照 `io.rs` 的文档），
   `acquire` 返回它的视图，`finish` 记账。

   **这里有一个还没定的设计问题**：这块纹理**归谁持有**、生命周期怎么跨帧复用。
   `FrameSource` 的文档把生命周期约定写给了**实现方**（"实现方自己维护纹理池，调用方不持有所有权"），
   所以帧槽这一侧要给出对称的答案 —— 而 `pipeline.rs` 的渲染循环里现在**同时**有分块、
   并行、以及编码器三件事在动，谁在 `acquire`/`finish` 之间跑，需要先想清再落笔。

   **没想清就落笔，正是 `pipeline.rs` 出过事故的同类**（历史上那次是把两趟 uniform 写进同一个
   encoder，两趟读到同一次写入；症状隐蔽到只有一条"孤立亮点该被摊到邻点"的测试才抓得住）。

## 怎么验（补完之后）

```text
cargo test --workspace                        # 670 条
node scripts/run-guards.mjs                   # 20 条，全绿
node scripts/check-dual-end.mjs --frames 0,30,89
    -> 期望仍然是 SSIM 1.000000 / MAE 0.0000（2026-09 实测过一次）
```

外加 `check-core-purity.mjs` / `check-dep-graph.mjs`：补 trait 实现**不该**动 core，
所以它们必须仍然全绿 —— 如果它们红了，说明实现的位置选错了（把平台类型带进了 core）。

## 顺手项（同一片区域，但独立）

`frame` 子命令现在只有 PNG 出口，实测 **1250 ms/帧**（同一段 31 帧走 `render`/h264 只要 ~30 ms/帧）。
加 `--format jpeg|raw` 能把它拉回可用区间。

**但默认必须保持 PNG**：`check-m1-record.mjs` / `check-m2-record.mjs` 那几条记录类守卫
对产物格式敏感，改默认会连带改动已归档的记录。
