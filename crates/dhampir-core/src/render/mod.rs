//! 渲染图。
//!
//! 两张图：M0 的**探针图**（[`ProbeRenderer`]，一个三色三角形）与 M1 的
//! **corpus 场景**（[`SceneRenderer`]，五张确定性图，见 [`SELECTABLE_SCENES`]）。
//!
//! 它们的形状是同一个：**构造要 `&wgpu::Device` 与目标格式，渲染要
//! `&mut wgpu::CommandEncoder` 与一个 `&wgpu::TextureView`**。
//! 中间不出现 `Instance`、不出现 surface、不出现任何平台判断。
//!
//! 探针图的使命在 M0 就完成了（"能不能出图"这句话最短的写法），留在这里是因为它
//! 的顶点坐标、采样点与判定都带着一段"为什么这么挑"的说明——那些理由比代码值钱。
//!
//! 判定（"这一帧应当是什么样"）在 [`scene_model`]：纯 `f64`、不碰 GPU，因此
//! 没有显卡的机器上也能被测试。这一层独立于测量，才谈得上"独立答案"。

mod animation;
mod blit;
mod blur;
mod color_adjust;
mod color_mask;
mod warp;
mod compose;
mod overlay;
mod probe;
mod timeline;
mod scene;

/// corpus 的公共部分：一帧怎么渲染、怎么判、整表摘要怎么算、**记录长什么样**。
///
/// `pub` 的理由与 [`scene_model`] 一样，而且更强：**两个宿主都要调它**。
/// M2 要证明的是"同一份 WGSL 在两个运行时里画出同样的字节"，如果驱动这件事的
/// 代码——以及"画完之后写成什么表"——在两端各写一遍，那个证明就退化成
/// "两份驱动大致相当"。
pub mod corpus;

/// corpus 场景的**纯数值模型**：一堆 `f64` 算术，不碰 GPU、不碰 `wgpu`。
///
/// `pub` 不是随手写的：worker 用它判定实测值，而复核者要能**站在 crate 外面**
/// 用同一份公式重算表里的数。模型的价值就在于它是独立答案——藏在私有模块里，
/// 复核者只能重读一遍渲染代码，那就不独立了。
pub mod scene_model;

#[cfg(test)]
mod wgsl_subset;

pub use corpus::{
    CORPUS_RECORD_KIND, CORPUS_RECORD_SCHEMA, CORPUS_TARGET_FORMAT, CORPUS_TABLE_MILESTONE, Counts,
    CorpusError, PointReading, RenderPair, SceneFrame, SceneRun, TableRow, epoch_seconds, frame_json,
    frame_rel_path, frames_digest, judge_frame, leg_json, point_json, record_text, render_frame,
    render_frame_pair, render_frame_record, render_run, report_text, scene_json, table_digest,
};
pub use animation::{AnimationTextures, FRAME_FORMAT as ANIMATION_FRAME_FORMAT, resolve_frame_index};
pub use blit::{BLIT_WGSL, BlitRenderer};
pub use blur::{BLUR_WGSL, BlurRenderer, MAX_RADIUS as BLUR_MAX_RADIUS, TAPS as BLUR_TAPS, gaussian_weights_1d};
pub use color_adjust::{COLOR_ADJUST_WGSL, ColorAdjustParams, ColorAdjustRenderer};
pub use color_mask::{
    COLOR_MASK_WGSL, ColorMaskParams, ColorMaskRenderer, OverlayShape,
};
pub use warp::{WARP_WGSL, WarpParams, WarpRenderer};
pub use compose::{COMPOSE_WGSL, Compositor, LayerDraw, RenderSpace, inverse_affine};
pub use overlay::{
    InkBounds, InkReport, OverlayItem, OverlayReport, compose_overlay, ink_report,
    placement_transform,
};
pub use timeline::{
    SourceResolver, TimelineRenderer, blur_radius, scale_document_radius, synthetic_seed_for_source,
    synthetic_seed_for_source_frame, synthetic_source_rgba8,
};
pub use probe::{
    PROBE_CLEAR_COLOR, PROBE_TARGET_SIZE, PROBE_VERTICES, ProbeRenderer, ProbeSample, ProbeVertex,
    SampleExpectation, VERTEX_INSET, probe_samples, render_probe_frame,
};
pub use scene::{
    BLUR_INTERMEDIATE_FORMAT, BYTE_TOLERANCE, LayerVertex, Params, SCENE_TARGET_FORMAT,
    SCENE_TARGET_SIZE, SCENE_WGSL, SELECTABLE_SCENES, SamplePoint, SampleVerdict, SceneDraw,
    ScenePasses, SceneRenderer, SceneSpec, expected_bytes, judge_sample, scene_by_name,
    scene_names,
};
