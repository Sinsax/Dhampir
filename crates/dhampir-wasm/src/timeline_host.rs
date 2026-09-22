//! 让浏览器按**一份工程**渲染，而不是按一个视频文件（wasm-only）。
//!
//! # 这是 M3 与 M4 的分界
//!
//! M3 的预览宿主只知道"一个 video 元素、一帧"；从这里开始，宿主知道的是
//! **一份 schema v1 工程**：多轨、片段、变换、不透明度、特效、转场、关键帧。
//! 求值仍然在 `dhampir_core::compose` 里（纯函数、两端共用），这一层只做两件事：
//! 把工程 JSON 收进来并校验、把图层清单落到真实的纹理上。
//!
//! # v1 的宿主能力（写清楚，免得当成没有的）
//!
//! **只绑定一路源纹理**：所有图层都采当前这一帧。多源 / 每层各自的源内帧号
//! 需要宿主维护纹理缓存与解码调度——那是 T4.4/M5 的事，不是这一层能糊弄过去的。
//! 对"单片段工程"这个最常见的退化情形，它是**精确**的。

use std::cell::RefCell;

use dhampir_core::compose::{self, Composite};
use dhampir_core::render::SourceResolver;
use dhampir_core::io::FrameSource;
use dhampir_core::render::{Compositor, LayerDraw};
use dhampir_core::timeline::schema::{Project, validate_project_with_effects};
use wasm_bindgen::prelude::*;

use crate::preview::{PREVIEW_FORMAT, VideoFrameSource, element_by_id};
use crate::web::{js_err, new_instance};

thread_local! {
    /// 当前载入的工程。**只有通过校验的工程才会被记住**——
    /// 让一份有问题的工程留在里面，只会让后面每一步都要重新判断"它到底能不能用"。
    static PROJECT: RefCell<Option<Project>> = const { RefCell::new(None) };
}

fn composite_json(composite: &Composite) -> serde_json::Value {
    let layers: Vec<serde_json::Value> = composite
        .layers
        .iter()
        .map(|layer| {
            serde_json::json!({
                "clip_id": layer.clip_id,
                "source": layer.source,
                "source_frame": layer.source_frame,
                "opacity": layer.opacity,
                "frozen_for_transition": layer.frozen_for_transition,
                "transform": {
                    "x": layer.transform.x,
                    "y": layer.transform.y,
                    "scale": layer.transform.scale,
                    "rotation_deg": layer.transform.rotation_deg,
                },
                "effects": layer
                    .effects
                    .iter()
                    .map(|effect| serde_json::json!({ "kind": effect.kind, "params": effect.params }))
                    .collect::<Vec<_>>(),
            })
        })
        .collect();
    serde_json::json!({ "frame": composite.frame, "layers": layers })
}

/// 载入一份工程：解析 + 校验，返回结构化结果。
///
/// 返回形如 `{"parsed":true,"ok":false,"issues":[…]}`。**问题清单直接来自 timeline 的校验**，
/// UI 可以照着渲染成人话——不需要这一层再翻译一遍，翻译两遍就会有两套说法。
#[wasm_bindgen]
pub fn dhampir_project_open(json: &str) -> String {
    let parsed: Result<Project, _> = serde_json::from_str(json);
    match parsed {
        Err(error) => serde_json::json!({
            "parsed": false,
            "ok": false,
            "error": error.to_string(),
        })
        .to_string(),
        Ok(project) => {
            let issues =
                validate_project_with_effects(&project, dhampir_core::effects::REGISTRY);
            let ok = issues.is_empty();
            PROJECT.with(|slot| {
                *slot.borrow_mut() = if ok { Some(project) } else { None };
            });
            serde_json::json!({ "parsed": true, "ok": ok, "issues": issues }).to_string()
        }
    }
}

/// 这一帧要画什么。工程没载入（或没通过校验）时返回带 error 的空清单。
#[wasm_bindgen]
pub fn dhampir_project_frame(frame: i32) -> String {
    PROJECT.with(|slot| {
        let borrowed = slot.borrow();
        match borrowed.as_ref() {
            None => serde_json::json!({
                "frame": frame,
                "layers": [],
                "error": "还没有载入通过校验的工程",
            })
            .to_string(),
            Some(project) => composite_json(&compose::evaluate(project, i64::from(frame))).to_string(),
        }
    })
}

/// 时间线长度（帧）。没载入时返回 -1。
#[wasm_bindgen]
pub fn dhampir_project_end_frame() -> i32 {
    PROJECT.with(|slot| {
        slot.borrow()
            .as_ref()
            .and_then(compose::end_frame)
            .map(|end| i32::try_from(end).unwrap_or(i32::MAX))
            .unwrap_or(-1)
    })
}

/// 第一帧。没载入时返回 -1。
#[wasm_bindgen]
pub fn dhampir_project_first_frame() -> i32 {
    PROJECT.with(|slot| {
        slot.borrow()
            .as_ref()
            .and_then(compose::first_frame)
            .map(|start| i32::try_from(start).unwrap_or(0))
            .unwrap_or(-1)
    })
}

/// 按工程渲染某一帧，返回像素摘要。**给 harness 做程序化验证用。**
///
/// 走的是和预览同一条路（求值 -> 合成 -> 读回），区别只是 sink 是离屏纹理。
/// 这样"时间线驱动两端"这句话才能被逐字节地验，而不是靠看一眼画面对不对。
#[wasm_bindgen]
pub async fn dhampir_project_render_probe(
    video_id: String,
    frame: i32,
    width: u32,
    height: u32,
) -> Result<String, JsValue> {
    let composite = PROJECT.with(|slot| {
        slot.borrow()
            .as_ref()
            .map(|project| compose::evaluate(project, i64::from(frame)))
    })
    .ok_or_else(|| js_err("还没有载入通过校验的工程"))?;

    let video = element_by_id(&video_id, "video")?;
    let instance = new_instance();
    let ctx = dhampir_core::gpu::request_context(&instance, None)
        .await
        .map_err(|e| js_err(e.to_string()))?;

    let mut source = VideoFrameSource::new(&ctx.device, &ctx.queue, video).map_err(js_err)?;
    let source_size = source.size();
    let source_view = source.frame_view(&ctx.device, i64::from(frame));

    let target = ctx.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("dhampir project probe target"),
        size: wgpu::Extent3d {
            width: width.max(1),
            height: height.max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: PREVIEW_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let target_view = target.create_view(&wgpu::TextureViewDescriptor::default());

    // v1：所有图层采同一路源纹理（见模块注释里的能力说明）。
    let draws: Vec<LayerDraw<'_>> = composite
        .layers
        .iter()
        .map(|layer| LayerDraw {
            view: &source_view,
            source_size,
            transform: layer.transform,
            opacity: layer.opacity,
        })
        .collect();

    let compositor = Compositor::new(&ctx.device, PREVIEW_FORMAT);
    let mut encoder = ctx
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("dhampir project probe encoder"),
        });
    compositor.compose(
        &ctx.device,
        &ctx.queue,
        &mut encoder,
        &target_view,
        (width.max(1), height.max(1)),
        &draws,
        wgpu::Color::TRANSPARENT,
    );
    ctx.queue.submit([encoder.finish()]);

    let image = dhampir_core::readback::read_texture_rgba8(&ctx.device, &ctx.queue, &target)
        .await
        .map_err(|e| js_err(e.to_string()))?;
    let digest = dhampir_core::timeline::selfcheck::fnv1a64(&image.pixels);
    Ok(serde_json::json!({
        "frame": frame,
        "width": width,
        "height": height,
        "layers": composite.layers.len(),
        "bytes": image.pixels.len(),
        "digest": format!("{digest:016x}"),
    })
    .to_string())
}

// ---------------------------------------------------------------------------
// 双端比对用的入口：按**同一份合成源**渲染样本工程，返回 PNG 字节。
//
// 为什么源要用合成图而不是 video 元素：比对的结论只有在**两端输入逐字节相同**时
// 才有归因价值。源图由 core 的 synthetic_source_rgba8 + synthetic_seed_for_source_frame
// 生成，native 一侧调的是同一对函数——所以比出来的差异只可能来自渲染与运行时。
// ---------------------------------------------------------------------------

#[wasm_bindgen]
pub async fn dhampir_sample_project_render_png(
    project_json: String,
    frame: i32,
    width: u32,
    height: u32,
) -> Result<Vec<u8>, JsValue> {
    // 刻意**不碰** thread_local 里的工程：这个入口要能被独立调用（驱动直接喂 JSON），
    // 免得比对结果依赖"页面之前打开了什么"。
    let project: Project = serde_json::from_str(&project_json)
        .map_err(|e| js_err(format!("工程 JSON 解析失败：{e}")))?;
    let issues = validate_project_with_effects(&project, dhampir_core::effects::REGISTRY);
    if !issues.is_empty() {
        return Err(js_err(format!("工程没通过校验：{}", issues.len())));
    }

    let composite = compose::evaluate(&project, i64::from(frame));
    let instance = new_instance();
    let ctx = dhampir_core::gpu::request_context(&instance, None)
        .await
        .map_err(|e| js_err(e.to_string()))?;

    let mut resolver = SyntheticSources {
        device: &ctx.device,
        queue: &ctx.queue,
        cache: std::collections::HashMap::new(),
        size: (width.max(1), height.max(1)),
    };
    let target = ctx.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("dhampir sample probe target"),
        size: wgpu::Extent3d {
            width: width.max(1),
            height: height.max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: PREVIEW_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let target_view = target.create_view(&wgpu::TextureViewDescriptor::default());
    let renderer = dhampir_core::render::TimelineRenderer::new(&ctx.device, PREVIEW_FORMAT);
    let mut encoder = ctx
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    renderer.render_frame(
        &ctx.device,
        &ctx.queue,
        &mut encoder,
        &target_view,
        (width.max(1), height.max(1)),
        &composite,
        &mut resolver,
        wgpu::Color::TRANSPARENT,
    );
    ctx.queue.submit([encoder.finish()]);

    let image = dhampir_core::readback::read_texture_rgba8(&ctx.device, &ctx.queue, &target)
        .await
        .map_err(|e| js_err(e.to_string()))?;
    image
        .encode_png()
        .map_err(|e| js_err(format!("PNG 编码失败：{e}")))
}

/// 与 native 一侧 `render_project` 里那个缓存器**同构**：同一对 core 函数生成源图。
struct SyntheticSources<'a> {
    device: &'a wgpu::Device,
    queue: &'a wgpu::Queue,
    cache: std::collections::HashMap<(String, i64), (wgpu::Texture, wgpu::TextureView)>,
    size: (u32, u32),
}

impl SourceResolver for SyntheticSources<'_> {
    fn texture_for(
        &mut self,
        source: &str,
        source_frame: i64,
    ) -> Option<(wgpu::TextureView, (u32, u32))> {
        let key = (source.to_string(), source_frame);
        if !self.cache.contains_key(&key) {
            let (width, height) = self.size;
            let pixels = dhampir_core::render::synthetic_source_rgba8(
                width,
                height,
                dhampir_core::render::synthetic_seed_for_source_frame(source, source_frame),
            );
            let texture = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("dhampir sample source"),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: PREVIEW_FORMAT,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            self.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &pixels,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(width * 4),
                    rows_per_image: Some(height),
                },
                wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            );
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            self.cache.insert(key.clone(), (texture, view));
        }
        self.cache
            .get(&key)
            .map(|(_, view)| (view.clone(), self.size))
    }
}
