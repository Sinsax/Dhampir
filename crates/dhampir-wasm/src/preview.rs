//! M3 预览链路：源帧怎么进 GPU、画到哪去。
//!
//! 只有 wasm32 会编译这个模块（Cargo.toml 的 target-specific 依赖负责这件事）。
//!
//! # 这里落的就是 S3.1 的结论
//!
//! 浏览器侧的源帧**先拷进一张普通 texture_2d**，不是零拷贝外部纹理。两条实测理由
//! （见 plan/s3.1-source-frame-sampling.md §2.2）：
//!
//!   1. texture_external 没有 textureLoad 重载（实测编译失败：no matching call）；
//!   2. 《WGSL 可移植性子集》禁隐式 LOD 采样（textureSample(），而外部纹理只能那样采。
//!
//! 于是 frame_view() 里是一次 copy_external_image_to_texture；之后渲染图拿到的就是一张
//! 普通纹理——与 native 侧同一种绑定、同一份 WGSL。这正是底座要的那条线。

use std::cell::RefCell;

use dhampir_core::gpu::{self, GpuContext};
use dhampir_core::io::{FrameSink, FrameSource};
use dhampir_core::render::BlitRenderer;
use dhampir_core::{readback, wgpu};

use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use web_sys::{HtmlCanvasElement, HtmlVideoElement};

use crate::web::{js_err, new_instance};

/// 源纹理与 canvas 的格式。用线性 Rgba8Unorm：拷贝进来的就是源帧的字节，
/// 不做任何色彩转换——转换留在渲染图里显式做，别藏在管线的格式里。
pub const PREVIEW_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

// ---------------------------------------------------------------------------
// 接缝一：帧从哪来
// ---------------------------------------------------------------------------

/// 浏览器侧的帧来源：一个 video 元素当前的帧。
///
/// 持有的 Queue 是 Clone 来的句柄：copy_external_image_to_texture 是 Queue 的方法，
/// 而 FrameSource::frame_view 只收 &Device——所以队列随源一起带。
pub struct VideoFrameSource {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    queue: wgpu::Queue,
    video: HtmlVideoElement,
    size: (u32, u32),
}

impl VideoFrameSource {
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        video: HtmlVideoElement,
    ) -> Result<Self, String> {
        let width = video.video_width().max(1);
        let height = video.video_height().max(1);
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("dhampir preview source"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: PREVIEW_FORMAT,
            // COPY_DST：拷贝的落点；TEXTURE_BINDING：渲染图要采它。
            //
            // RENDER_ATTACHMENT **不是**可选的：Dawn 要求 copyExternalImageToTexture 的目标纹理
            // 同时带这个用途，少了它不是报错而是**静默失败**——纹理保持未初始化，画出来是一张
            // 未定义的图。S3.1 在 JS 侧先踩过一次（见 plan/s3.1-source-frame-sampling.md §3），
            // 这里是从 Rust 又踩了一次。
            usage: wgpu::TextureUsages::COPY_DST
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        Ok(Self {
            texture,
            view,
            queue: queue.clone(),
            video,
            size: (width, height),
        })
    }

    pub fn size(&self) -> (u32, u32) {
        self.size
    }
}

impl FrameSource for VideoFrameSource {
    fn frame_view(&mut self, _device: &wgpu::Device, _frame: i64) -> wgpu::TextureView {
        // frame 参数在这里不参与定位：video 元素自己持有播放位置，调用方负责 seek。
        // 记录里那句话（"帧是全局帧号，源内偏移由实现方换算"）等 T3.2 接了真解码器再兑现。
        self.queue.copy_external_image_to_texture(
            &wgpu::wgt::CopyExternalImageSourceInfo {
                source: wgpu::wgt::ExternalImageSource::HTMLVideoElement(self.video.clone()),
                origin: wgpu::wgt::Origin2d::ZERO,
                flip_y: false,
            },
            wgpu::wgt::CopyExternalImageDestInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
                color_space: wgpu::wgt::PredefinedColorSpace::Srgb,
                premultiplied_alpha: false,
            },
            wgpu::Extent3d {
                width: self.size.0,
                height: self.size.1,
                depth_or_array_layers: 1,
            },
        );
        self.view.clone()
    }
}

// ---------------------------------------------------------------------------
// 接缝二：画到哪去
// ---------------------------------------------------------------------------

/// 浏览器侧的帧去向：canvas surface。
///
/// SurfaceTexture 必须活到 present，所以 acquire 把它存进 self，finish 再吐出去。
pub struct CanvasFrameSink {
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    // wgpu 30 起「呈现」是 Queue 的动作（queue.present(frame)），不是 frame 自己的方法。
    queue: wgpu::Queue,
    current: Option<wgpu::SurfaceTexture>,
}

impl CanvasFrameSink {
    /// surface 由调用方先建好：`request_context` 需要拿到 surface 才能选到能用它的 adapter，
    /// 所以「建 surface → 要 context → 配 surface」这个顺序不能反过来。
    pub fn new(
        surface: wgpu::Surface<'static>,
        adapter: &wgpu::Adapter,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        size: (u32, u32),
    ) -> Result<Self, String> {
        let (width, height) = size;
        let caps = surface.get_capabilities(adapter);
        let mut config = surface
            .get_default_config(adapter, width, height)
            .ok_or_else(|| "该 adapter 不支持这个 canvas surface".to_string())?;
        // 优先 sRGB：两端要"看起来一致"，色彩空间差异先在预览侧消掉。
        if let Some(srgb) = caps.formats.iter().copied().find(wgpu::TextureFormat::is_srgb) {
            config.format = srgb;
        }
        // Fifo：预览是给人看的，撕裂会干扰"看起来对不对"的判断。
        config.present_mode = wgpu::PresentMode::Fifo;
        surface.configure(device, &config);
        Ok(Self {
            surface,
            config,
            queue: queue.clone(),
            current: None,
        })
    }

    /// canvas 尺寸变了之后重新配置 surface。
    ///
    /// 先把 current 丢掉再 configure：SurfaceTexture 还活着时重配会在某些后端报错，
    /// 而那个错误要到 present 时才显形，很难归因。
    pub fn resize(&mut self, device: &wgpu::Device, size: (u32, u32)) -> Result<(), String> {
        let (width, height) = size;
        if width == 0 || height == 0 {
            return Err("canvas 尺寸不能为 0".to_string());
        }
        self.current = None;
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(device, &self.config);
        Ok(())
    }

    /// canvas surface 的格式。BlitRenderer 必须用**同一个**格式建管线。
    pub fn format(&self) -> wgpu::TextureFormat {
        self.config.format
    }

    /// 当前这张 canvas 纹理（`acquire` 里存下来的那张）。
    ///
    /// 预渲染缓存（候选 ⑤）命中时要拿它当**呈现目标**：不必重新合成，只要把缓存纹理
    /// **blit** 到这张上来（BlitRenderer 就在这个文件里，格式也已经对齐 —— 见上面那条）。
    ///
    /// ⚠️ **不要把它当 `copy_texture_to_texture` 的源或目的**：画布表面纹理通常只有
    /// `RENDER_ATTACHMENT`，没有 `COPY_DST` —— 拷贝会被拒绝。所以路径只能是
    /// "缓存纹理 →（BlitRenderer）→ 这张"。
    ///
    /// 没 `acquire` 过时是 `None`（那时本来也没有可呈现的目标）。
    pub fn texture(&self) -> Option<&wgpu::Texture> {
        self.current.as_ref().map(|frame| &frame.texture)
    }
}


// ---------------------------------------------------------------------------
// 宿主：把两个接缝接起来
// ---------------------------------------------------------------------------

/// 预览宿主：一个上下文 + 一个源 + 一个 sink + 一条搬运管线。
pub struct PreviewHost {
    ctx: GpuContext,
    source: VideoFrameSource,
    sink: CanvasFrameSink,
    renderer: BlitRenderer,
}

impl PreviewHost {
    /// 画一帧：源 → 搬运管线 → canvas。
    pub fn draw(&mut self, frame: i64) {
        let source_view = self.source.frame_view(&self.ctx.device, frame);
        let sink_view = self.sink.acquire(&self.ctx.device);
        let mut encoder = self.ctx.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("dhampir preview encoder"),
        });
        self.renderer
            .render(&self.ctx.device, &mut encoder, &source_view, &sink_view);
        self.ctx.queue.submit([encoder.finish()]);
        self.sink.finish(frame);
    }
}

pub(crate) fn element_by_id<T: JsCast>(id: &str, what: &str) -> Result<T, JsValue> {
    let window = web_sys::window().ok_or_else(|| js_err("没有 window 对象"))?;
    let document = window
        .document()
        .ok_or_else(|| js_err("没有 document 对象"))?;
    let element = document.get_element_by_id(id).ok_or_else(|| {
        js_err(format!("页面上找不到 id={id} 的元素"))
    })?;
    element
        .dyn_into::<T>()
        .map_err(|_| js_err(format!("id={id} 的元素不是{what}")))
}

thread_local! {
    static PREVIEW_HOST: RefCell<Option<PreviewHost>> = const { RefCell::new(None) };
}

/// 初始化预览宿主（幂等）。返回 adapter 信息 JSON，便于记录"用了哪块卡"。
#[wasm_bindgen]
pub async fn dhampir_preview_init(canvas_id: String, video_id: String) -> Result<String, JsValue> {
    if PREVIEW_HOST.with(|h| h.borrow().is_some()) {
        return Ok(String::from("{\"already\":true}"));
    }
    let canvas: HtmlCanvasElement = element_by_id(&canvas_id, "canvas")?;
    let video: HtmlVideoElement = element_by_id(&video_id, "video")?;
    let size = (canvas.width().max(1), canvas.height().max(1));

    let instance = new_instance();
    // 显式 SurfaceTarget::Canvas，不走 Into——见 web.rs 里同一处的说明。
    let surface = instance
        .create_surface(wgpu::SurfaceTarget::Canvas(canvas))
        .map_err(|e| js_err(format!("无法从 canvas 创建 surface：{e:?}")))?;
    let ctx = gpu::request_context(&instance, Some(&surface))
        .await
        .map_err(|e| js_err(e.to_string()))?;

    let sink = CanvasFrameSink::new(surface, &ctx.adapter, &ctx.device, &ctx.queue, size)
        .map_err(js_err)?;
    // 管线必须用 sink 的**格式**建：格式不匹配 wgpu 会在校验时报错。
    let renderer = BlitRenderer::new(&ctx.device, sink.format());
    let source = VideoFrameSource::new(&ctx.device, &ctx.queue, video).map_err(js_err)?;

    // 只报两栏：浏览器侧本来也问不出型号（GPUAdapterInfo 的 name/description 是空串，
    // 见 run-browser-corpus.mjs 的说明）。要"用了哪块卡"得靠宿主侧从 CDP 读设备表。
    let info = ctx.adapter.get_info();
    let json = format!(
        "{{\"name\":\"{}\",\"backend\":\"{:?}\",\"size\":\"{}x{}\"}}",
        info.name, info.backend, size.0, size.1
    );
    PREVIEW_HOST.with(|h| *h.borrow_mut() = Some(PreviewHost { ctx, source, sink, renderer }));
    Ok(json)
}

/// 画一帧到 canvas。
#[wasm_bindgen]
pub fn dhampir_preview_draw(frame: i32) -> Result<(), JsValue> {
    PREVIEW_HOST.with(|h| {
        let mut borrowed = h.borrow_mut();
        let host = borrowed
            .as_mut()
            .ok_or_else(|| js_err("预览宿主尚未初始化"))?;
        host.draw(i64::from(frame));
        Ok(())
    })
}

/// 离屏走一遍同一条路，返回像素摘要——**给harness做程序化验证用**。
///
/// 与上面画 canvas 的唯一区别是 sink：这里画进离屏纹理，于是能读回来算摘要。
/// 它证的是"Rust 侧的拷贝 + 搬运"与 JS 侧算出来的是同一批字节。
#[wasm_bindgen]
pub async fn dhampir_preview_probe_digest(video_id: String) -> Result<String, JsValue> {
    let video: HtmlVideoElement = element_by_id(&video_id, "video")?;
    let instance = new_instance();
    let ctx = gpu::request_context(&instance, None)
        .await
        .map_err(|e| js_err(e.to_string()))?;

    let mut source =
        VideoFrameSource::new(&ctx.device, &ctx.queue, video).map_err(js_err)?;
    let (width, height) = source.size();
    let target = ctx.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("dhampir preview probe target"),
        size: wgpu::Extent3d {
            width,
            height,
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

    let renderer = BlitRenderer::new(&ctx.device, PREVIEW_FORMAT);
    let source_view = source.frame_view(&ctx.device, 0);
    let mut encoder = ctx.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("dhampir preview probe encoder"),
    });
    renderer.render(&ctx.device, &mut encoder, &source_view, &target_view);
    ctx.queue.submit([encoder.finish()]);

    let image = readback::read_texture_rgba8(&ctx.device, &ctx.queue, &target)
        .await
        .map_err(|e| js_err(e.to_string()))?;
    // 用仓里已有的 FNV-1a 64（dhampir-timeline 的 selfcheck），与守卫/JS 侧同参数，
    // 这样摘要可以直接跨语言比。
    let digest = dhampir_core::timeline::selfcheck::fnv1a64(&image.pixels);
    Ok(format!(
        "{{\"width\":{},\"height\":{},\"bytes\":{},\"digest\":\"{:016x}\"}}",
        image.width,
        image.height,
        image.pixels.len(),
        digest
    ))
}
impl FrameSink for CanvasFrameSink {
    fn acquire(&mut self, _device: &wgpu::Device) -> wgpu::TextureView {
        // wgpu 30 起 get_current_texture 不再返回 Result，而是把各种失败列成枚举。
        // Success 与 Suboptimal 都是能画的帧（后者只是配置不再最优）。
        // 其余情形这里只能 panic：FrameSink::acquire 的签名没有回错误的余地，
        // 而「拿不到帧」是宿主级故障，不该被悄悄画成一张空图。
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame) => frame,
            wgpu::CurrentSurfaceTexture::Suboptimal(frame) => frame,
            other => panic!("取 canvas 纹理失败：{other:?}"),
        };
        let view = frame.texture.create_view(&wgpu::TextureViewDescriptor::default());
        self.current = Some(frame);
        view
    }

    fn finish(&mut self, _frame: i64) {
        // 提交与呈现是两步：submit 已由调用方做完，这里把它交给合成器。
        if let Some(frame) = self.current.take() {
            self.queue.present(frame);
        }
    }
}
