# dhampir · 底座架构（实测整理）

> 本文是**实测**的架构整理，不是设想：依赖关系取自 `Cargo.toml` 并由
> `scripts/check-dep-graph.mjs` 复核，接口签名取自当前源码。
> 整理日期 2026-09-22 ｜ 对应 HEAD `63782435ffa2e0c2bf923f503919b60da45a7f5a`
> 交付边界见 [video-editor-plan.md](./video-editor-plan.md) §2「范围」。

---

## 1. 一句话

底座 = **一份渲染图 + 两个宿主**。

浏览器一侧做 **preview**，native 一侧做 **render**，两边跑**同一个 `dhampir-core`**，
产出**可比的帧**。除此之外的一切（网关、队列、存储、部署、发布）都不在底座里。

---

## 2. 交付边界

| | 内容 |
|---|---|
| **底座（本仓库交付）** | `dhampir-timeline` / `dhampir-media` / `dhampir-core` + 两个宿主 `dhampir-wasm` / `dhampir-worker` |
| **下游（本仓库不做）** | API 网关 / 任务队列 / 对象存储 / 容器与部署 / 分片编排 / 影子环境与发布 / 编辑 UI 产品化 / FFmpeg 编解码的具体实现 |

**约束**：契约里**不预设网络、不预设进程边界**——这样下游既能接成客户端-服务端分离，
也能接成本地预览 + 渲染的合并处理。

---

## 3. 分层与依赖方向（实测）

```
        dhampir-timeline          （纯数据：整数帧号 / 有理数时间基 / 时间码）
           ↑          ↑
   dhampir-media   dhampir-core    （CPU 编解码契约 ‖ 渲染图 + WGSL）
           ↑          ↑             ← 两者是**兄弟**，互不依赖
           └────┬─────┘
        dhampir-wasm   dhampir-worker
       （wasm32 宿主）    （native 宿主）   ← 两个宿主互不依赖
```

`scripts/check-dep-graph.mjs` 实测输出（EXIT=0）：

```
dhampir-core → dhampir-timeline
dhampir-media → dhampir-timeline
dhampir-timeline（无内部依赖）
dhampir-wasm → dhampir-core, dhampir-media
dhampir-worker → dhampir-core, dhampir-media
```

### 3.1 ⚠️ 文档里曾有三处把这张图写错

本轮核对时发现的**事实性错误**，已就地修正。错法都是"以为 core 依赖 media"：

| 位置 | 错误写法 | 实际 |
|---|---|---|
| 根 `README.md` crate 地图 | 链式 `timeline ← media ← core` | core 与 media 是**兄弟** |
| 本文档来源：指导文档「crate 家族命名」 | `dhampir-media 无内部依赖` | media **依赖 timeline** |
| `plan/remaining-work.md` §3.1 | `timeline ← media ← core` | **core 不依赖 media** |

**core 不依赖 media 是刻意的**，不是疏漏——渲染图不该知道 MP4 长什么样。
守卫里写死了这条：`dhampir-core` 的 `allowed` 只有 `['dhampir-timeline']`。

---

## 4. 两个接缝：架构的钱花在哪

底座的全部平台差异都收在**两个出口**，其余代码平台中立。

### 4.1 接缝一：GPU 帧交接 —— `dhampir-core::io`

```rust
pub trait FrameSource {
    /// 第 frame 帧的纹理视图。frame 是**全局帧号**（时间线坐标），不是素材内帧号。
    fn frame_view(&mut self, device: &wgpu::Device, frame: i64) -> wgpu::TextureView;
}

pub trait FrameSink {
    /// 取本帧要画进去的纹理视图（canvas surface 实现一帧只能调一次）
    fn acquire(&mut self, device: &wgpu::Device) -> wgpu::TextureView;
    /// 本帧提交完成：surface 在这里 present()，离屏实现在这里记账
    fn finish(&mut self, frame: i64);
}
```

**为什么这就是架构支点**：渲染图只认"给我第 N 帧"，不关心帧来自 WebCodecs 还是 FFmpeg。
**preview 与 render 共用同一张渲染图，区别只在这两个实现上**。

### 4.2 接缝二：CPU 编解码契约 —— `dhampir-media`

五个纯 trait，**零实现**（两端各自实现）：

| trait | 职责 | 关键约定 |
|---|---|---|
| `Demuxer` | 开容器、报轨道、按帧 seek | `seek` 单位是**帧号**，不是时间戳 |
| `VideoDecoder` | 第 N 帧 → CPU 像素 | 返回借用；同帧重复调用应命中缓存 |
| `VideoEncoder` | 像素 → 码流 | **有状态**，调用顺序必须与帧号顺序一致 |
| `AudioEncoder` | 音频编码 | 音频不分片，一次编到底 |
| `Muxer` | 码流 → 容器 | 先视频包、再音频包、最后 `finalize` |

配套词汇表：`PixelFormat` / `ColorSpace`（含 `BT709_LIMITED`/`SRGB_FULL` 等常量）/
`TransferFunction` / `Primaries` / `ColorRange` / `VideoInfo` / `AudioInfo` / `MediaError`。

`DecodedVideoFrame` 里有一个重要字段：`borrowed: bool`——**明说这帧数据什么时候失效**，
硬解路径常见。调用方据此决定何时拷贝，而不是让"多了一次全帧拷贝"藏在暗处。

### 4.3 两个接缝为什么必须分开

| | `core::io` | `media` |
|---|---|---|
| 层次 | **GPU 侧**（`wgpu::TextureView`） | **CPU 侧**（`&[u8]` 平面） |
| 依赖 | timeline（+ wgpu） | 只有 timeline，**不依赖 wgpu** |
| 谁实现 | 两个宿主 | 两个宿主 |
| 不知道什么 | 不知道编解码格式 | **不知道 GPU 的存在** |

这条切分让 `dhampir-media` 能编译进**任何** Rust 程序（包括没有 GPU 的环境），
也让 core 保持"零 `#[cfg]`"。

**支撑零 `#[cfg]` 的机制**：wgpu 在 workspace 层**不开任何后端 feature**
（`default-features = false, features = ["std", "wgsl"]`），后端由宿主各自追加
（wasm → `webgpu`；worker → `vulkan`/`dx12`/`metal`）。
一旦从 core 里开后端，`ash`/`d3d12` 这类 native-only 依赖就会把 wasm32 编译打死。

---

## 5. 两条对称的数据流

| 阶段 | preview（wasm32 宿主） | render（native 宿主） |
|---|---|---|
| 输入 | WebCodecs `VideoFrame` | FFmpeg 解码 → CPU 平面 |
| 进 GPU | `importExternalTexture`（零拷贝，**S3.1 未决**）或 `copyExternalImageToTexture` | 上传纹理 |
| 接缝一 | 实现 `FrameSource` | 实现 `FrameSource` |
| **渲染** | `dhampir-core` 渲染图（**同一份 WGSL**） | `dhampir-core` 渲染图（**同一份 WGSL**） |
| 接缝二 | `FrameSink` = canvas surface | `FrameSink` = 离屏 texture |
| 输出 | `present()` 给用户看 | `readback` → PNG / 交给编码器 |
| 目的地 | 屏幕 | 文件（编码 / mux 属下游） |

**唯一允许分叉的地方共两处**：`Instance` 的后端选择（`BROWSER_BACKENDS` vs
`NATIVE_BACKENDS`），以及上面两个接缝的实现。

---

## 6. 不变量（动了就动摇验收）

- **整数帧号**，不用浮点秒；**有理数帧率**（`Timebase` 是 num/den，不是 f64）
- **声明式特效**（类型 + 参数），**不做可上传 shader**
- 按 **WebGPU 能力下限**写：无导数、无循环、无隐式 LOD、只用 f32
- **`dhampir-core` 零 `#[cfg]`**（唯一例外：独占一行的 `#[cfg(test)]`）
- **core 只用 `std` + `wgsl`**（见 §4.3）
- `FrameSource` 拿到的是**全局帧号**；源内偏移由实现方换算，渲染图不该知道"这段从第 300 帧开始"
- **解码器必须独立于生成器**：守卫各自重写 FNV-1a / PNG 解码 / SHA-256 / 指标算法，不共享实现

---

## 7. 代码体积：底座 vs 取证脚手架（值得注意）

按文件归属粗算（字节）：

| 类别 | 内容 | 约计 |
|---|---|---|
| **平台中立核心** | `gpu.rs` 15K、`io.rs` 3.6K、`readback.rs` 16K、`shaders/*` 15.7K、`wgsl_subset.rs` 5.8K | **~56 KB** |
| **混合** | `render/scene.rs` 93K（渲染器与场景规格写在一起） | ~93 KB |
| **取证脚手架** | `render/corpus.rs` 52K、`render/scene_model.rs` 38K、`render/probe.rs` 28K | ~118 KB |
| **宿主（native）** | `dhampir-worker` 全部 153K（timing / 对齐探针 / 场景选择 / 记录） | ~153 KB |
| **宿主（wasm）** | `corpus.rs` 41K、`probe.rs` 6.5K、`www/*.html` 31K | ~79 KB |
| **契约与数据** | `dhampir-timeline` 36K、`dhampir-media` 14.5K | ~50 KB |

**结论**：这个仓库里**过半体积是取证脚手架**，不是渲染能力本身。
它拖进了 `serde_json` / `png` 这类与渲染无关的依赖（`corpus.rs` 就是 `use serde_json::{Value, json}`）。

**这是当前最值得规划的一次重构**，但**不是现在做**——理由见 §9 R2。

---

## 8. 未决与已知缺口

| # | 项 | 影响 |
|---|---|---|
| 1 | **S3.1：`FrameSource` 的返回类型装不下外部纹理** | 零拷贝路线可能需要给 trait 加"外部纹理"分支；退到 `copyExternalImageToTexture` 则每帧多一次全帧拷贝。**这是底座唯一未决的接口级决策** |
| 2 | `dhampir-media` 五个 trait **零实现** | 契约未经验证；且注释里引用已移出的 M5 作为设计理由 |
| 3 | M2 记录守卫 `scripts/check-m2-record.mjs` **无 `main()`** | 跑它零输出 EXIT=0 → 验收快照会假绿。**调研结论的"可自证性"缺最后一环** |
| 4 | `scene.rs` 里渲染器与场景规格混住 | 想抽脚手架时会牵动生产代码 |

---

## 9. 建议（按优先级）

**R1 —— 先定 S3.1，再写 M3 的任何代码。**
它是唯一的接口级未决项，决定底座上限（能不能零拷贝）。按 plan 原定口径：
实测 1080p 下"拷贝成本 vs 约束成本"，结论落 `plan/` 下的决策文件。别猜。

**R2 —— 不要现在拆取证脚手架。**
虽然 §7 显示过半体积是脚手架，但它**正在承载 M1/M2 已验收的证据**。
现在拆会动摇记录的可复现性，而 M2 还没收官。**顺序：M2 收官 → 记录冻结 → 再拆**。
届时建议拆成 `dhampir-evidence`（或 feature-gate），让下游拿到干净的 core。

**R3 —— 把"唯一跨边界契约"钉死为 timeline JSON。**
"客户端-服务端分离"要成立，必须明确：**跨进程只传 timeline JSON**；
`FrameSource`/`FrameSink` **不跨进程**（它们是同进程 trait）。
`dhampir-timeline` 已有 `serde`（default 开），契约基础是现成的。把这条写进文档，
下游才不会去序列化一个 `TextureView`。

**R4 —— 给 `dhampir-media` 标注"未验证契约"。**
五个 trait 零实现。保留是对的（下游各自实现），但要显式标注状态，
并把引用 M5 的注释改成"下游分片渲染会需要"。

**R5 —— 补上 M2 守卫的 `main()`。**
调研结论要经得起独立复核，这是最后一环。纯本地、不依赖任何外部决策。

**R6 —— `scene.rs` 的渲染器与场景规格先加注释分界。**
不必现在拆文件，但要标清哪部分是生产渲染器、哪部分是取证规格，为 R2 铺路。

**R7 —— 建议的推进顺序。**
```
1. 修文档依赖错误（本轮已做）
2. M2 收官：守卫 main() → 验收快照 → 提交 → 独立复核
3. 冻结 M2 记录 → 再评估 R2 的脚手架拆分
4. M3 第一件事：S3.1 决策落文件
```
