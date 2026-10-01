//! # dhampir-core
//!
//! 渲染核心：渲染图、内置 WGSL、特效注册表。**这是整个仓库最贵的资产。**
//!
//! ## 唯一一条硬规则：本 crate 内不出现任何 `#[cfg]`
//!
//! 指导文档 §4.4。理由不是审美——是因为一旦这里出现 `#[cfg(target_arch = "wasm32")]`，
//! 你就有了"服务端版渲染"和"浏览器版渲染"两个实现，两端一致性从"架构保证"
//! 降级成"人工纪律"，而人工纪律在半年后一定失效。
//!
//! 平台差异只有两个出口：
//!
//! 1. **帧从哪来、画到哪去** —— 今天真正在用的那一对是
//!    [`render::timeline::SourceResolver`]（帧进）+ [`io::FrameSink`]（帧出）。
//!    两侧各有真实实现：浏览器是 `BoundVideos`，native 是 `DecodingSources`。
//!
//!    ⚠️ [`io::FrameSource`] 是 **M0 的更早写法**，如今只剩 M3 的单视频预览那条路在用
//!    （`dhampir-wasm/src/preview.rs`）—— **别照它去接新宿主**，它不承担"帧号精确"那条
//!    铁律所要求的换算（`SourceResolver::texture_for` 把 `source_frame` 放进参数里，
//!    就是为了不让源内帧号被悄悄忽略）。这段话以前写成"1 号接缝是 FrameSource"，
//!    会把人引到老路上去，所以改掉。
//! 2. `wgpu::Instance` 的创建 —— **唯一允许分叉的地方**，且这个分叉发生在宿主里，
//!    不在本 crate 里（见 [`gpu::NATIVE_BACKENDS`] / [`gpu::BROWSER_BACKENDS`] 的说明）
//!
//! 规则由 CI 与本地脚本共同看守：`scripts/check-core-purity.ps1` 会 grep 本 crate
//! 下的 `#[cfg(` 并让构建失败。
//!
//! ## 依赖
//!
//! 只依赖 [`dhampir_timeline`]（时间用整数帧号）与 `wgpu`。
//! **不依赖 `dhampir-media`**——渲染图不该知道 MP4 长什么样。
//!
//! ## M0 状态
//!
//! 只有一张探针图：[`render::ProbeRenderer`]，一个三色三角形。它不参与任何编辑功能，
//! 存在的唯一目的是把"同一份渲染代码在两个宿主上出图"这件事钉死。
//!
//! ## 关于 [`wgpu`] 的重导出
//!
//! [`pub use wgpu`](wgpu) 不是随手写的：宿主必须用**同一个** wgpu 版本，
//! 否则 `wgpu::Texture` 这种类型在跨 crate 边界时会被当成两个不同的类型。
//! 宿主写 `dhampir_core::wgpu::...` 而不是自己 `use wgpu`，版本就不可能对不上。

pub mod animation;
pub mod cache;
pub mod compose;
pub mod overlay;
pub mod effects;
pub mod gpu;
pub mod io;
pub mod metric;
pub mod readback;
pub mod render;

/// 时间线的重导出。宿主与上层不需要单独依赖 `dhampir-timeline`——
/// 这样"依赖方向表"里 `dhampir-wasm: 依赖 core + media` 就是字面成立的。
pub use dhampir_timeline as timeline;

pub use wgpu;
