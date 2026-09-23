//! 让浏览器按**一份工程**渲染，而不是按一个视频文件（wasm-only）。
//!
//! # 这是 M3 与 M4 的分界
//!
//! M3 的预览宿主只知道"一个 video 元素、一帧"；从这里开始，宿主知道的是
//! **一份工程文件**（ProjectDoc，timeline 为 v2）：多轨、元素、变换、不透明度、
//! 混合模式、特效、转场、关键帧、调整图层、标记。
//! 求值仍然在 dhampir_core::compose 里（纯函数、两端共用），这一层只做三件事：
//! 把工程 JSON 收进来并校验、按资产表绑源、把图层清单落到真实的纹理上。
//!
//! # 多源（写清楚，免得当成没有的）
//!
//! **一个 source 一个 video 元素**（BoundVideos），每个 source 各自 seek 到
//! 自己那一帧 —— 这是"帧号精确"在宿主接缝上的兑现。同样一份素材被多个元素引用
//! 时，它们各自 seek，互不干扰。
//!
//! # 与后端那条路的边界
//!
//! 后端（dhampir CLI）按 asset 开一路 ffmpeg 顺序解码器。浏览器这侧没有解码器，
//! 用的是 video 元素的 seek。所以两端能对齐的是**形状与图层清单**，
//! 逐像素对齐要两端吃同一份像素 —— 那件事的边界写在 plan/consistency-criteria.md。

use std::cell::RefCell;
use std::collections::HashMap;

use dhampir_core::compose::{self, Composite};
use dhampir_core::io::{FrameSink, FrameSource};
use dhampir_core::render::SourceResolver;
// 宿主 API 的返回体形状：**有名字、有测试钉住**，不再用宏手写。
use dhampir_core::timeline::host_api;
use dhampir_core::wgpu;
use dhampir_core::timeline::project::{ProjectDoc, load_doc, validate_project_doc};
use wasm_bindgen::prelude::*;
use web_sys::{HtmlCanvasElement, HtmlVideoElement};

use crate::preview::{CanvasFrameSink, PREVIEW_FORMAT, VideoFrameSource, element_by_id};
use crate::web::{js_err, new_instance};

thread_local! {
    /// 工程预览宿主。与 PROJECT 分开：工程可以在没有 canvas 时先载入并校验。
    static PROJECT_HOST: RefCell<Option<ProjectHost>> = const { RefCell::new(None) };
}

thread_local! {
    /// 当前载入的工程。**只有通过校验的工程才会被记住**——
    /// 让一份有问题的工程留在里面，只会让后面每一步都要重新判断「它到底能不能用」。
    ///
    /// 宿主持有的是**工程文件**（ProjectDoc，timeline 已是 v2），不是裸契约。
    /// 于是「写入一律写工程文件」这条规矩在浏览器侧也真的落地了，
    /// 而 v1 -> v2 的迁移只发生在 load_doc 一处。
    static PROJECT: RefCell<Option<ProjectDoc>> = const { RefCell::new(None) };
}


// ---------------------------------------------------------------------------
// W0：工程帧上 canvas
//
// 与 preview.rs 那个宿主的区别：那个只认一路 <video> 与一条搬运管线；
// 这个认的是**一份工程**，走求值 + TimelineRenderer（多轨、特效、转场、关键帧）。
//
// # 为什么 seek 在 JS 侧做
//
// <video> 的 seek 是**异步**的：set_current_time 立刻返回，那一帧还没解码出来。
// 而 SourceResolver 是同步接口（渲染循环里不该 await）。所以拆成两步：
//   1. JS 调 dhampir_project_sources_for(frame) 拿到这一帧需要的 (source, 秒数)，
//      逐个 seek 并等 seeked；
//   2. JS 再调 dhampir_project_draw(frame)，此时每个 video 都停在自己的那一帧上。
// 异步的 DOM 舞蹈留在 JS，Rust 侧保持同步——两边都在自己擅长的形态上。

/// 工程预览宿主。
pub struct ProjectHost {
    ctx: dhampir_core::gpu::GpuContext,
    sink: CanvasFrameSink,
    renderer: dhampir_core::render::TimelineRenderer,
    /// source 标识 -> 对应的 <video>。v1 允许多路：一个 source 一个元素。
    videos: HashMap<String, HtmlVideoElement>,
    /// 预览尺寸。**由 canvas 决定**，不由工程决定——schema v1 里没有分辨率字段。
    size: (u32, u32),
}

/// 渲染期的解析器：不 seek，只取「当前停在哪一帧」的纹理。
struct BoundVideos<'a> {
    device: &'a wgpu::Device,
    queue: &'a wgpu::Queue,
    videos: &'a HashMap<String, HtmlVideoElement>,
    format: wgpu::TextureFormat,
    /// 一份源纹理的缓存。缓存的是**纹理**不是像素：每次渲染仍重新拷一次。
    textures: HashMap<String, (wgpu::Texture, wgpu::TextureView, (u32, u32))>,
}

impl SourceResolver for BoundVideos<'_> {
    fn texture_for(
        &mut self,
        source: &str,
        _source_frame: i64,
    ) -> Option<(wgpu::TextureView, (u32, u32))> {
        // 这里**不**按 source_frame 定位：那一帧已经由 JS 侧 seek 好了。
        // source_frame 的意义体现在 sources_for 返回的秒数上。
        let video = self.videos.get(source)?;

        // **这一帧还没有可用画面就跳过这一层。**
        //
        // HAVE_CURRENT_DATA = 2。低于它的时候 video_width() 可能是 0，
        // 而 0 会被 max(1) 兜成 1x1 —— 接着 copy_external_image_to_texture 拿
        // 1920x1080 的源往 1x1 的纹理里拷，**在 wasm 里就是一个 unreachable**，
        // 页面上只剩一句 "启动失败：unreachable executed"，看不出跟素材有关。
        //
        // 契约本来就写着"给不出来就返回 None（该层会被跳过）"：
        // 宁可少画一层，也不能让整页死掉。
        if video.ready_state() < 2 {
            return None;
        }
        let width = video.video_width();
        let height = video.video_height();
        if width == 0 || height == 0 {
            return None;
        }

        // 尺寸与缓存不一致就重建：同一 source 换了素材、或元数据晚到都会走到这里。
        // **不重建的话**，纹理尺寸与源不符 -> 又是上面那个 unreachable。
        let needs_texture = match self.textures.get(source) {
            Some((_, _, size)) => *size != (width, height),
            None => true,
        };
        if needs_texture {
            let texture = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("dhampir project source"),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: self.format,
                // RENDER_ATTACHMENT 不是可选的：Dawn 要求 copyExternalImageToTexture 的目标
                // 同时带这个用途，少了它不报错而是**静默失败**（S3.1 与 preview.rs 都记过）。
                usage: wgpu::TextureUsages::COPY_DST
                    | wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            });
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            self.textures
                .insert(source.to_string(), (texture, view, (width, height)));
        }
        let (texture, view, size) = self.textures.get(source)?;
        self.queue.copy_external_image_to_texture(
            &wgpu::wgt::CopyExternalImageSourceInfo {
                source: wgpu::wgt::ExternalImageSource::HTMLVideoElement(video.clone()),
                origin: wgpu::wgt::Origin2d::ZERO,
                flip_y: false,
            },
            wgpu::wgt::CopyExternalImageDestInfo {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
                color_space: wgpu::wgt::PredefinedColorSpace::Srgb,
                premultiplied_alpha: false,
            },
            wgpu::Extent3d {
                width: size.0,
                height: size.1,
                depth_or_array_layers: 1,
            },
        );
        Some((view.clone(), *size))
    }
}

impl ProjectHost {
    fn draw(&mut self, frame: i64) -> Result<(), String> {
        let composite = PROJECT.with(|slot| {
            slot.borrow()
                .as_ref()
                .map(|doc| compose::evaluate_v2(&doc.timeline, frame))
        });
        let Some(composite) = composite else {
            return Err("还没有载入通过校验的工程".to_string());
        };

        // 拆分借用：四个字段互不相干，解析器只需要其中两个的不可变借用。
        let Self { ctx, sink, renderer, videos, size } = self;
        let (width, height) = *size;
        let sink_format = sink.format();
        let sink_view = sink.acquire(&ctx.device);
        let mut resolver = BoundVideos {
            device: &ctx.device,
            queue: &ctx.queue,
            videos,
            format: sink_format,
            textures: HashMap::new(),
        };
        let mut encoder = ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("dhampir project encoder"),
            });
        renderer.render_frame(
            &ctx.device,
            &ctx.queue,
            &mut encoder,
            &sink_view,
            (width, height),
            &composite,
            &mut resolver,
            wgpu::Color::TRANSPARENT,
        );
        ctx.queue.submit([encoder.finish()]);
        sink.finish(frame);
        Ok(())
    }
}

/// 固定源解析器：所有 source、所有帧都返回**同一张纹理**。
///
/// 存在的理由是让 render_probe 走共用入口而不引入多源能力 ——
/// 多源是 dhampir_project_draw 那条路的事（它用 BoundVideos 按 source 各自 seek）。
struct FixedSource {
    view: wgpu::TextureView,
    size: (u32, u32),
}

impl SourceResolver for FixedSource {
    fn texture_for(
        &mut self,
        _source: &str,
        _source_frame: i64,
    ) -> Option<(wgpu::TextureView, (u32, u32))> {
        Some((self.view.clone(), self.size))
    }
}

fn composite_result(composite: &Composite) -> dhampir_core::timeline::host_api::FrameResult {
    dhampir_core::timeline::host_api::FrameResult {
        frame: composite.frame,
        layers: composite
            .layers
            .iter()
            .map(|layer| dhampir_core::timeline::host_api::LayerView {
                clip_id: layer.clip_id.clone(),
                source: layer.source.clone(),
                source_frame: layer.source_frame,
                opacity: layer.opacity,
                frozen_for_transition: layer.frozen_for_transition,
                transform: dhampir_core::timeline::host_api::TransformView {
                    x: layer.transform.x,
                    y: layer.transform.y,
                    scale: layer.transform.scale,
                    rotation_deg: layer.transform.rotation_deg,
                },
                effects: layer
                    .effects
                    .iter()
                    .map(|effect| dhampir_core::timeline::host_api::EffectView {
                        kind: effect.kind.clone(),
                        params: effect.params.clone(),
                    })
                    .collect(),
            })
            .collect(),
        error: None,
    }
}

/// 载入一份工程：解析 + 校验，返回结构化结果。
///
/// 返回形如 `{"parsed":true,"ok":false,"issues":[…]}`。**问题清单直接来自 timeline 的校验**，
/// UI 可以照着渲染成人话——不需要这一层再翻译一遍，翻译两遍就会有两套说法。
#[wasm_bindgen]
pub fn dhampir_project_open(json: &str) -> String {
    // **三种形态都收**：工程文件 / 裸契约 v1 / 裸契约 v2。判定只在 load_doc 里做一次。
    let doc = match load_doc(json) {
        Ok(doc) => doc,
        Err(error) => return host_api::to_json(&host_api::OpenResult::unparsed(error)),
    };
    let issues = validate_project_doc(&doc, dhampir_core::effects::REGISTRY);
    let ok = issues.is_ok();
    PROJECT.with(|slot| {
        if ok {
            *slot.borrow_mut() = Some(doc);
        }
        // **校验不过时保留上一份可用工程。**
        // 旧实现这里写的是 None，而 engine.js 的注释一直写着"失败时保留上一份"——
        // 实现与注释不一致，后果是"编辑到一半"会让预览直接不再出图。
        // 一次非法编辑不该让整个界面失效：问题显示在清单里就够了。
    });
    host_api::to_json(&host_api::OpenResult::from_doc_issues(&issues))
}

/// 当前工程的**工程文件本体**（壳 + 契约）。
///
/// 前端要它有三件事：按资产表解析素材地址、读 render_hints 作为出片尺寸、
/// 以及把整份工程原样提交给后端。**没有载入时返回 null** ——
/// 不返回一个空壳，因为空壳会被当成"载入了一个空工程"。
#[wasm_bindgen]
pub fn dhampir_project_doc() -> String {
    PROJECT.with(|slot| match slot.borrow().as_ref() {
        None => "null".to_string(),
        Some(doc) => host_api::to_json(doc),
    })
}

/// 这一帧要画什么。工程没载入（或没通过校验）时返回带 error 的空清单。
#[wasm_bindgen]
pub fn dhampir_project_frame(frame: i32) -> String {
    PROJECT.with(|slot| {
        let borrowed = slot.borrow();
        match borrowed.as_ref() {
            None => dhampir_core::timeline::host_api::to_json(&dhampir_core::timeline::host_api::FrameResult {
                frame: i64::from(frame),
                layers: Vec::new(),
                error: Some("还没有载入通过校验的工程".to_string()),
            }),
            Some(doc) => host_api::to_json(&composite_result(&compose::evaluate_v2(
                &doc.timeline,
                i64::from(frame),
            ))),
        }
    })
}

/// 时间线长度（帧）。没载入时返回 -1。
#[wasm_bindgen]
pub fn dhampir_project_end_frame() -> i32 {
    PROJECT.with(|slot| {
        slot.borrow()
            .as_ref()
            .and_then(|doc| compose::end_frame_v2(&doc.timeline))
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
            .and_then(|doc| compose::first_frame_v2(&doc.timeline))
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
            .map(|doc| compose::evaluate_v2(&doc.timeline, i64::from(frame)))
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

    // **走共用的渲染入口**，而不是自己 new 一个 Compositor。
    //
    // 这里原先直接用 Compositor 叠图，于是它成了**旁路**：混合模式与调整图层会加进
    // TimelineRenderer，而这条路不会自动获得 —— 两条路从那时起开始分叉。
    // 现在传一个**固定 resolver**：所有 source 都返回同一张纹理。
    // 这不是偷懒，而是本函数的**退化语义**本身（它只喂一路源，用来验「工程路径能出图」）。
    //
    // ⚠️ 一处**应当变化**的行为：旧旁路完全忽略 effects，新路径会应用特效。
    // 所以对带特效的层，结果**本来就该不同**；对无特效的层必须逐字节相同
    // （基线验收就是拿无特效的工程比）。
    let mut resolver = FixedSource {
        view: source_view.clone(),
        size: source_size,
    };
    let renderer = dhampir_core::render::TimelineRenderer::new(&ctx.device, PREVIEW_FORMAT);
    let mut encoder = ctx
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("dhampir project probe encoder"),
        });
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
    let digest = dhampir_core::timeline::selfcheck::fnv1a64(&image.pixels);
    Ok(host_api::to_json(&host_api::ProbeResult {
        frame: i64::from(frame),
        width,
        height,
        layers: composite.layers.len(),
        bytes: image.pixels.len(),
        digest: format!("{digest:016x}"),
    }))
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
    // 三种形态都收（这里是 v1 裸契约），但**求值一律走 v2** ——
    // 这样浏览器侧只剩一条求值路径。这条路径与 native 的 render_project 一起被
    // check-dual-end.mjs 逐像素盯着：如果迁移改了语义，那边会立刻变红。
    let doc = load_doc(&project_json).map_err(|e| js_err(format!("工程载入失败：{e}")))?;
    let issues = validate_project_doc(&doc, dhampir_core::effects::REGISTRY);
    if !issues.is_ok() {
        return Err(js_err(format!(
            "工程没通过校验：{} 条错误 / {} 条警告",
            issues.errors.len(),
            issues.warnings.len()
        )));
    }

    let composite = compose::evaluate_v2(&doc.timeline, i64::from(frame));
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

// ---------------------------------------------------------------------------
// W0 的四个导出：attach / bind_source / sources_for / draw / resize
// ---------------------------------------------------------------------------

/// 建工程预览宿主：canvas surface + sink + 时间线渲染器。
#[wasm_bindgen]
pub async fn dhampir_project_attach(canvas_id: String) -> Result<String, JsValue> {
    if PROJECT_HOST.with(|h| h.borrow().is_some()) {
        return Ok(String::from("{\"already\":true}"));
    }
    let canvas: HtmlCanvasElement = element_by_id(&canvas_id, "canvas")?;
    let size = (canvas.width().max(1), canvas.height().max(1));
    let instance = new_instance();
    let surface = instance
        .create_surface(wgpu::SurfaceTarget::Canvas(canvas))
        .map_err(|e| js_err(format!("无法从 canvas 创建 surface：{e:?}")))?;
    let ctx = dhampir_core::gpu::request_context(&instance, Some(&surface))
        .await
        .map_err(|e| js_err(e.to_string()))?;
    let sink = CanvasFrameSink::new(surface, &ctx.adapter, &ctx.device, &ctx.queue, size)
        .map_err(js_err)?;
    let renderer = dhampir_core::render::TimelineRenderer::new(&ctx.device, sink.format());
    let info = ctx.adapter.get_info();
    let json = format!(
        "{{\"name\":\"{}\",\"backend\":\"{:?}\",\"size\":\"{}x{}\"}}",
        info.name, info.backend, size.0, size.1
    );
    PROJECT_HOST.with(|h| {
        *h.borrow_mut() = Some(ProjectHost {
            ctx,
            sink,
            renderer,
            videos: HashMap::new(),
            size,
        });
    });
    Ok(json)
}

/// 把一个 source 标识绑定到页面上的一个 video 元素。
///
/// v1 允许多路：一个 source 一个元素。但**每路都要自己 seek 到自己那一帧**——
/// 这是"帧号精确"在宿主接缝上的兑现，少做一步它就又变成假的。
#[wasm_bindgen]
pub fn dhampir_project_bind_source(source: String, video_id: String) -> Result<(), JsValue> {
    let video: HtmlVideoElement = element_by_id(&video_id, "video")?;
    PROJECT_HOST.with(|h| {
        let mut borrowed = h.borrow_mut();
        let host = borrowed
            .as_mut()
            .ok_or_else(|| js_err("工程预览宿主尚未初始化，先调 dhampir_project_attach"))?;
        host.videos.insert(source, video);
        Ok(())
    })
}

/// 这一帧需要哪些源、各自停在**第几秒**。
///
/// 秒数由**整数帧号**与工程的时间基算出（frame * den / num）——
/// 浮点只在这一步出现，而且是从整数推出来的，不是反过来。
#[wasm_bindgen]
pub fn dhampir_project_sources_for(frame: i32) -> String {
    PROJECT.with(|slot| {
        let borrowed = slot.borrow();
        let Some(doc) = borrowed.as_ref() else {
            return host_api::to_json(&host_api::SourcesResult {
                frame: i64::from(frame),
                sources: Vec::new(),
                error: Some("还没有载入通过校验的工程".to_string()),
            });
        };
        let (num, den) = match doc.timeline.timebase.to_timebase() {
            Ok(timebase) => (f64::from(timebase.num), f64::from(timebase.den)),
            Err(error) => {
                return host_api::to_json(&host_api::SourcesResult {
                    frame: i64::from(frame),
                    sources: Vec::new(),
                    error: Some(error.to_string()),
                });
            }
        };
        let composite = compose::evaluate_v2(&doc.timeline, i64::from(frame));
        // 去重：同一个 (source, 帧) 只该 seek 一次。
        let mut seen = std::collections::BTreeSet::new();
        let mut sources = Vec::new();
        for layer in &composite.layers {
            if !seen.insert((layer.source.clone(), layer.source_frame)) {
                continue;
            }
            sources.push(host_api::SourceView {
                source: layer.source.clone(),
                source_frame: layer.source_frame,
                seconds: (layer.source_frame as f64) * den / num,
            });
        }
        host_api::to_json(&host_api::SourcesResult {
            frame: i64::from(frame),
            sources,
            error: None,
        })
    })
}

/// 画一帧到 canvas。**调用前请先用 sources_for 把源 seek 到位。**
#[wasm_bindgen]
pub fn dhampir_project_draw(frame: i32) -> Result<(), JsValue> {
    PROJECT_HOST.with(|h| {
        let mut borrowed = h.borrow_mut();
        let host = borrowed
            .as_mut()
            .ok_or_else(|| js_err("工程预览宿主尚未初始化，先调 dhampir_project_attach"))?;
        host.draw(i64::from(frame)).map_err(js_err)
    })
}

/// canvas 尺寸变了之后重新配置 surface。
///
/// 预览尺寸**由 canvas 决定**，不由工程决定——schema v1 里没有分辨率字段。
#[wasm_bindgen]
pub fn dhampir_project_resize(width: u32, height: u32) -> Result<(), JsValue> {
    PROJECT_HOST.with(|h| {
        let mut borrowed = h.borrow_mut();
        let host = borrowed
            .as_mut()
            .ok_or_else(|| js_err("工程预览宿主尚未初始化，先调 dhampir_project_attach"))?;
        let size = (width.max(1), height.max(1));
        host.sink.resize(&host.ctx.device, size).map_err(js_err)?;
        host.size = size;
        Ok(())
    })
}


/// 出片前的预检：这份工程里有没有**超出对端能力**的东西。
///
/// # 为什么在前端做这件事，但规则不写在前端
///
/// 前端确实要"提交前就知道哪一条不支持"（不然要等分钟级任务跑完才报错）。
/// 但**判定规则只有一个实现，在 Rust 里** —— 这个导出就是那条通道。
/// 在 JS 里重写一遍过滤逻辑，两端就会各自演化，
/// 而「两端说同一种话」正是这个项目最贵的东西。
///
/// 入参是对端的能力声明（就是 /capabilities 返回的那份），
/// 出参是 Issue 数组 —— **复用同一套错误格式**，前端不需要再翻译一次。
///
/// 当前实现先从 v1 契约迁移到 v2 再预检：宿主持有的还是 v1 的 Project。
#[wasm_bindgen]
pub fn dhampir_project_precheck(capabilities_json: &str) -> String {
    let capabilities: host_api::Capabilities = match serde_json::from_str(capabilities_json) {
        Ok(capabilities) => capabilities,
        Err(error) => {
            return host_api::to_json(&host_api::OpenResult::unparsed(format!(
                "能力声明解析失败：{error}"
            )))
        }
    };

    PROJECT.with(|slot| {
        let borrowed = slot.borrow();
        let Some(doc) = borrowed.as_ref() else {
            return host_api::to_json(&host_api::OpenResult::unparsed(
                "还没有载入通过校验的工程".to_string(),
            ));
        };
        // **不再需要迁移**：载入时已经统一成 v2 了。
        // 这段代码以前是"第二处迁移"—— 两份迁移实现迟早会漂，而漂了以后
        // 「预检说能做、渲染却做不了」这类事就只能靠人盯。
        host_api::to_json(&host_api::precheck(&doc.timeline, &capabilities))
    })
}
