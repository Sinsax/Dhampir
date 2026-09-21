//! # dhampir-worker
//!
//! 服务端宿主。**native-only**——它依赖 `wgpu` 的 Vulkan / DX12 / Metal 后端，
//! 这些在 wasm32 上根本不存在。
//!
//! ## 它和 `dhampir-wasm` 的关系
//!
//! 两者**互不依赖**，是同一份 [`dhampir_core`] 的两个宿主。整个仓库里唯一的
//! "双份"之处就是 [`dhampir_core::gpu::NATIVE_BACKENDS`] 与
//! [`dhampir_core::gpu::BROWSER_BACKENDS`] 这两个常量——真正的
//! `wgpu::Instance::new` 调用发生在各自的入口里。
//!
//! 除了那一次调用，两个宿主的初始化路径完全相同，都走
//! [`dhampir_core::gpu::request_context`]。
//!
//! ## M0 内容
//!
//! - [`offscreen`]：离屏渲染探针三角形 → 读回像素 → 写 PNG 与 `adapter.json`
//! - `dhampir-render` 二进制：把上面这些串成一条命令行
//!
//! M1 会在这个骨架上长出 corpus 场景集、`--scene` 参数与计时表。

// 明确的守卫：如果谁在 wasm32 上编译本 crate，让他看到一句人话，
// 而不是一堆 `ash` 的报错。
#[cfg(target_arch = "wasm32")]
compile_error!(
    "dhampir-worker 是 native-only 宿主（依赖 Vulkan/DX12/Metal 后端）。\
     浏览器侧请编译 dhampir-wasm：`cargo check -p dhampir-wasm --target wasm32-unknown-unknown`\
     ——注意加 `-p`，不要用 `--workspace`。"
);

pub mod offscreen;

pub use dhampir_core::timeline::{probe_digest, probe_report};
