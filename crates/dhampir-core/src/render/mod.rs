//! 渲染图。
//!
//! M0 只有一张**探针图**：[`ProbeRenderer`]，一个三色三角形。它不参与任何编辑
//! 功能，存在的唯一目的是把"同一份渲染代码在两个宿主上出图"这件事钉死——
//! 出不了图的话，后面所有的特效、合成、分片都无从谈起。
//!
//! 真正的渲染图（节点、连接、求值顺序、特效注册表）在 M1/M2 长出来，
//! 但它的形状已经从 [`ProbeRenderer`] 定下了：**构造要 `&wgpu::Device` 与目标格式，
//! 渲染要 `&mut wgpu::CommandEncoder` 与一个 `&wgpu::TextureView`**。
//! 中间不出现 `Instance`、不出现 surface、不出现任何平台判断。

mod probe;

pub use probe::{
    PROBE_CLEAR_COLOR, PROBE_TARGET_SIZE, PROBE_VERTICES, ProbeRenderer, ProbeSample, ProbeVertex,
    SampleExpectation, VERTEX_INSET, probe_samples, render_probe_frame,
};
