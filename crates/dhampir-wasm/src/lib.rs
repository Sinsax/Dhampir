//! # dhampir-wasm
//!
//! 浏览器宿主。它与 [`dhampir_worker`] **互不依赖**——两者是同一份
//! [`dhampir_core`] 的两个宿主，这就是"双血统"的字面含义。
//!
//! ## 结构
//!
//! - [`probe`]：**目标无关**。这一份在 native 与 wasm32 上都会被编译，
//!   所以 `cargo check --workspace` 本身就已经在证明"同一份源码"
//! - [`web`]：`wasm32` 专属。`wasm-bindgen` 导出、canvas surface、WebGPU 后端
//! - [`corpus`]：`wasm32` 专属。把 M1 那张表在浏览器里重画一遍，产出**可落盘的字节**
//!   （记录形状全在 `dhampir_core::render::corpus`，这里只做浏览器专属的三件事，
//!   见该模块文档）。M2 的"双运行时同帧比对"就建立在这一份之上
//!
//! native 下这个 crate 几乎是空的——这是**有意的**，因为 M0 验收第 1 条要求
//! `cargo check --workspace` 在 native 上通过。用 target-specific 依赖
//! （见 Cargo.toml）而不是在代码里堆 `#[cfg]`，让 Cargo 来表达平台差异。
//!
//! ## 唯一的分叉
//!
//! 整个 crate 里只有一行代码"和 native 长得不一样"：创建 `wgpu::Instance` 时
//! 传的后端常量。见 [`web`] 里的 `new_instance`。

pub mod demux;
pub mod probe;

#[cfg(target_arch = "wasm32")]
pub mod cache_wasm;

#[cfg(target_arch = "wasm32")]
pub mod corpus;

#[cfg(target_arch = "wasm32")]
pub mod demux_wasm;

#[cfg(target_arch = "wasm32")]
pub mod preview;

#[cfg(target_arch = "wasm32")]
pub mod timeline_host;

#[cfg(target_arch = "wasm32")]
pub mod web;
