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
use dhampir_core::overlay::{SubtitleTable, evaluate_overlay};
use dhampir_core::readback::Rgba8Image;
use dhampir_core::render::{
    OverlayItem, RenderSpace, SourceResolver, compose_overlay, ink_report,
};
// 宿主 API 的返回体形状：**有名字、有测试钉住**，不再用宏手写。
use dhampir_core::timeline::host_api;
use dhampir_core::wgpu;
use dhampir_core::timeline::project::{
    AssetKind, ProjectDoc, load_doc, validate_project_doc,
};
use dhampir_core::timeline::schema::Issue;
use dhampir_core::timeline::subtitle::{parse_ass, parse_srt};
use dhampir_core::timeline::text_layout::{border_px, LinePlacement, NormalizedRect, place_line};
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

thread_local! {
    /// 已登记的字幕表：素材 id -> 字幕条。
    ///
    /// # 为什么在这里，而不是在 ProjectHost 里
    ///
    /// 字幕是**工程的属性**，不是 canvas 的属性：载入一份工程、把它的字幕登记进来，
    /// 这件事不需要先有一个 canvas。宿主只是拿它去画。
    ///
    /// # 为什么载入新工程时**不清**
    ///
    /// 素材 id 就是文件名（`sub.srt`），一份页面会话服务的是同一批 fixtures；
    /// 而 `open` 在编辑那条路上会被反复调用（每次应用编辑都重新校验一遍），
    /// 清掉的话症状是「编辑一下字幕忽然不见了」——
    /// 那种错看起来像「字幕本来就只到那儿」，属于最难查的一类。
    static SUBTITLES: RefCell<SubtitleTable> = RefCell::new(SubtitleTable::new());
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

/// 这一帧要画的一行字：内容 + 归一化矩形 + **目标像素落点**。
///
/// 落点只由共享布局（[`place_line`]）给出。宿主**不许**自己算一遍 ——
/// 两端各算一次就会漂，而漂了以后两端各自都自洽，查起来只能靠肉眼。
#[derive(Debug, Clone)]
struct TextLineSpec {
    text: String,
    /// 行盒（归一化，相对文档坐标系）。两端比对的就是它。
    rect: NormalizedRect,
    /// 目标像素里的落点（`place_line` 的结果，JS 照着它去栅格化）。
    placement: LinePlacement,
}

/// 工程预览宿主。
pub struct ProjectHost {
    ctx: dhampir_core::gpu::GpuContext,
    sink: CanvasFrameSink,
    renderer: dhampir_core::render::TimelineRenderer,
    /// source 标识 -> 对应的 <video>。一个 source 一个元素。
    videos: HashMap<String, HtmlVideoElement>,
    /// source 标识 -> JS 预先转好的位图（当前帧）。
    ///
    /// **为什么需要这条路**：不是每个 WebGPU 实现都接受 <video> 作为
    /// copy_external_image_to_texture 的源。实测某个浏览器的
    /// GPUCopyExternalImageSource 联合类型里**没有 HTMLVideoElement**：
    ///
    ///   TypeError: 'source' member of GPUCopyExternalImageSourceInfo could not be
    ///   converted to any of: ImageBitmap, HTMLImageElement, HTMLCanvasElement,
    ///   OffscreenCanvas.
    ///
    /// 而 wgpu 把那个 TypeError unwrap 成 panic —— 整个 wasm 死在那一句上，
    /// 页面上只剩「启动失败：unreachable executed」，看不出跟素材有关。
    /// ImageBitmap 在**每一个**实现的联合类型里都有，所以它是最稳的源。
    bitmaps: HashMap<String, web_sys::ImageBitmap>,
    /// 强制走位图。JS 探测过之后告诉宿主；此时**不许退回 video**（退回去就是 trap）。
    require_bitmap: bool,
    /// 预览尺寸。**由 canvas 决定**，不由工程决定。
    size: (u32, u32),
    /// 这一帧要画的行，由 [`dhampir_project_text_frame`] 算好。
    ///
    /// 存下来而不是画的时候再算一遍：JS 是照着这份清单去栅格化的，
    /// 再算一遍就等于给「栅格化的那一份」与「判定的那一份」两次不一样的机会。
    text_lines: Vec<TextLineSpec>,
    /// `text_lines` 是哪一帧的。**对不上这一帧就不画** ——
    /// 宁可这一帧没有字幕，也不能把上一帧的字按上一帧的落点画上去：
    /// 那种画面看起来完全正常，只是"慢了半拍"。
    text_frame: Option<i64>,
    /// 行号 -> JS 栅格化好的行位图（当前帧）。
    ///
    /// 行号是 [`dhampir_project_text_frame`] 给出的清单里的下标 ——
    /// 于是「JS 栅格化了哪一行」与「宿主画哪一行」是同一个编号，不会错位。
    text_bitmaps: HashMap<u32, web_sys::ImageBitmap>,
}

/// 渲染期的解析器：不 seek，只取「当前停在哪一帧」的纹理。
struct BoundVideos<'a> {
    device: &'a wgpu::Device,
    queue: &'a wgpu::Queue,
    videos: &'a HashMap<String, HtmlVideoElement>,
    bitmaps: &'a HashMap<String, web_sys::ImageBitmap>,
    require_bitmap: bool,
    format: wgpu::TextureFormat,
    /// 一份源纹理的缓存。缓存的是**纹理**不是像素：每次渲染仍重新拷一次。
    textures: HashMap<String, (wgpu::Texture, wgpu::TextureView, (u32, u32))>,
}

impl BoundVideos<'_> {
    /// 保证 source 的纹理存在、且尺寸与源一致。
    ///
    /// **尺寸不符必须重建**：拿尺寸不符的纹理去 copy_external_image_to_texture
    /// 在 wasm 里就是一个 unreachable（整页死），而不是一个可捕获的错误。
    fn ensure_texture(&mut self, source: &str, size: (u32, u32)) {
        let needs = match self.textures.get(source) {
            Some((_, _, current)) => *current != size,
            None => true,
        };
        if !needs {
            return;
        }
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("dhampir project source"),
            size: wgpu::Extent3d {
                width: size.0,
                height: size.1,
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
        self.textures.insert(source.to_string(), (texture, view, size));
    }
}

impl SourceResolver for BoundVideos<'_> {
    fn texture_for(
        &mut self,
        source: &str,
        _source_frame: i64,
    ) -> Option<(wgpu::TextureView, (u32, u32))> {
        // 这里**不**按 source_frame 定位：那一帧已经由 JS 侧 seek 好了。
        // source_frame 的意义体现在 sources_for 返回的秒数上。

        // ---- 位图优先 ----
        // 见 ProjectHost::bitmaps 的说明：有的 WebGPU 实现不接受 <video>，
        // 而传进去的代价不是报错，是整个 wasm 死在那一句上。
        if let Some(bitmap) = self.bitmaps.get(source) {
            let width = bitmap.width();
            let height = bitmap.height();
            if width == 0 || height == 0 {
                return None;
            }
            self.ensure_texture(source, (width, height));
            let (texture, view, size) = self.textures.get(source)?;
            self.queue.copy_external_image_to_texture(
                &wgpu::wgt::CopyExternalImageSourceInfo {
                    source: wgpu::wgt::ExternalImageSource::ImageBitmap(bitmap.clone()),
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
            return Some((view.clone(), *size));
        }

        // JS 探过之后说「这个浏览器不接受 video」时**不许偷偷退回 video** ——
        // 那正好会撞上它不支持的那条路。宁可这一层不画。
        if self.require_bitmap {
            return None;
        }

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

        self.ensure_texture(source, (width, height));
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

// ---------------------------------------------------------------------------
// 文字叠加（T2.5）：评估 -> 落点 -> 栅格化（JS）-> 贴上去（这里）
//
// 三段各有各的归属，这一段只做中间那一段的**调度**：
//   * 这一帧有哪几行、各占哪个归一化矩形 —— evaluate_overlay（纯函数，两端共用）；
//   * 每行落在目标像素的哪儿 —— place_line（纯函数，两端共用）；
//   * 字形像素长什么样 —— JS 的 canvas（浏览器）与 ffmpeg drawtext（CLI）。
// 第 3 条两端**允许**不同（字体就不一样，浏览器没有 --font-file 这种东西），
// 所以证据里不比字形；第 1、2 条必须同源，所以它们不能有第二份实现。
// ---------------------------------------------------------------------------

/// 算这一帧的文字行：`evaluate_overlay` 给的归一化矩形 -> `place_line` 给的目标像素落点。
///
/// 返回的两份东西是同一批行的两种表示：归一化矩形（两端比对的就是它）与落点（宿主照着画）。
/// 落点算不出来的行（目标尺寸为 0、行盒没有高度）**不进清单**，由调用方用
/// `unplaced_lines` 把它数出来 —— 静默少一行，看起来与「这一行本来就没有」一模一样。
fn text_lines(
    doc: &ProjectDoc,
    frame: i64,
    subtitles: &SubtitleTable,
    target: (u32, u32),
) -> (Option<dhampir_core::overlay::TextOverlay>, Vec<TextLineSpec>) {
    let Some(overlay) = evaluate_overlay(&doc.timeline, frame, doc.sequence_size(), Some(subtitles))
    else {
        return (None, Vec::new());
    };
    let mut specs = Vec::with_capacity(overlay.items.len());
    for item in &overlay.items {
        let Some(placement) = place_line(item.rect, target) else {
            continue;
        };
        specs.push(TextLineSpec {
            text: item.text.clone(),
            rect: item.rect,
            placement,
        });
    }
    (Some(overlay), specs)
}

/// 把这一帧的行位图拷进纹理。
///
/// 与 [`BoundVideos`] 同一个口径：**缓存的是纹理不是像素，每帧重新拷一次** ——
/// 一行字的内容在同一帧里就是同一张画，不存在"过期"。
///
/// 尺寸取**位图自己的**（`bitmap.width()`）：与落点声明的尺寸不符时由
/// [`compose_overlay`] 拦下并计数。这里不放大也不裁剪 —— 一张对不上的位图硬画上去
/// 就是一次静默缩放（字糊一点、位置还差不多），那种错没人会当成 bug 报上来。
///
/// 返回与 `lines` **等长**的数组：第 i 项是第 i 行的纹理，宿主没拿到位图的是 `None`
/// （下标不能压缩，否则「哪一行没有位图」就丢了）。
fn upload_text_bitmaps(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    format: wgpu::TextureFormat,
    lines: &[TextLineSpec],
    bitmaps: &HashMap<u32, web_sys::ImageBitmap>,
) -> Vec<Option<(wgpu::Texture, wgpu::TextureView, (u32, u32))>> {
    let mut out = Vec::with_capacity(lines.len());
    for index in 0..lines.len() {
        let Some(bitmap) = bitmaps.get(&(index as u32)) else {
            out.push(None);
            continue;
        };
        let size = (bitmap.width(), bitmap.height());
        if size.0 == 0 || size.1 == 0 {
            out.push(None);
            continue;
        }
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("dhampir project text line"),
            size: wgpu::Extent3d {
                width: size.0,
                height: size.1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            // RENDER_ATTACHMENT 不是可选的：Dawn 要求 copyExternalImageToTexture 的目标
            // 同时带这个用途，少了它不报错而是**静默失败**（见 BoundVideos::ensure_texture）。
            usage: wgpu::TextureUsages::COPY_DST
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        queue.copy_external_image_to_texture(
            &wgpu::wgt::CopyExternalImageSourceInfo {
                source: wgpu::wgt::ExternalImageSource::ImageBitmap(bitmap.clone()),
                origin: wgpu::wgt::Origin2d::ZERO,
                flip_y: false,
            },
            wgpu::wgt::CopyExternalImageDestInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
                color_space: wgpu::wgt::PredefinedColorSpace::Srgb,
                // **直排 alpha**：位图在 JS 侧就是用 `premultiplyAlpha: "none"` 造的，
                // 这里再说一遍「它不是预乘的」。说错不会报错，只会让字的边缘发暗 ——
                // 看起来像「字体渲染得不太好」，而不像「叠加算错了」。
                premultiplied_alpha: false,
            },
            wgpu::Extent3d {
                width: size.0,
                height: size.1,
                depth_or_array_layers: 1,
            },
        );
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        out.push(Some((texture, view, size)));
    }
    out
}

/// （行清单 × 已上传的纹理）-> 叠加项。`skip` 给 `Some(i)` 时跳过第 i 行。
///
/// `skip` 是给判定用的：probe 要分离出**第 i 行贡献的墨迹**，办法就是
/// 「别都画」与「全都画」各渲染一次，两张一比（见 `dhampir_project_text_probe`）。
fn overlay_items<'a>(
    lines: &'a [TextLineSpec],
    uploaded: &'a [Option<(wgpu::Texture, wgpu::TextureView, (u32, u32))>],
    skip: Option<usize>,
) -> Vec<OverlayItem<'a>> {
    let mut items = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        if skip == Some(index) {
            continue;
        }
        let Some((_, view, size)) = uploaded.get(index).and_then(|slot| slot.as_ref()) else {
            continue;
        };
        items.push(OverlayItem {
            view,
            bitmap_size: *size,
            placement: line.placement,
        });
    }
    items
}

/// 一行字的墨迹判定结果（目标像素）。
///
/// `inside` / `on_edge` 是**判据**，不是观测：观测只有 `pixels` 与 `bounds`。
/// 判据的口径与 worker 的 `clip_message` 一致（见 `crates/dhampir-worker/src/text_overlay.rs`）：
/// 墨迹顶到位图的边上就是「被切了」——那时位图边缘那一列/行本来该是空的。
struct InkCheck {
    pixels: u64,
    bounds: Option<dhampir_core::render::InkBounds>,
    /// 墨迹是否整个落在落点方框里。不成立只可能是落点算错了（位图 1:1 贴上去）。
    inside: bool,
    /// 墨迹是否顶到了边（位图边或画面边）。
    on_edge: bool,
}

/// 一行字的墨迹 vs 它自己的落点方框与画面边界。
///
/// 「顶到了边」为什么把**画面边**也算上：位图中心对准行盒中心，而行盒按 margin 排在画面里，
/// 所以正常情况下墨迹离画面边很远。墨迹出现在第 0 行 / 最后一行，只有两种可能 ——
/// 字形被位图切了，或者位图（连同它的 pad）被画面切了。两种都是「被切了」。
fn check_ink(
    report: &dhampir_core::render::InkReport,
    placement: LinePlacement,
    target: (u32, u32),
) -> InkCheck {
    let Some(bounds) = report.bounds else {
        return InkCheck { pixels: 0, bounds: None, inside: true, on_edge: false };
    };
    let right = placement.x + placement.bitmap_width as i32;
    let bottom = placement.y + placement.bitmap_height as i32;
    let left_edge = bounds.x as i32;
    let top_edge = bounds.y as i32;
    let right_edge = (bounds.x + bounds.width) as i32;
    let bottom_edge = (bounds.y + bounds.height) as i32;
    InkCheck {
        pixels: report.pixels,
        bounds: Some(bounds),
        inside: left_edge >= placement.x
            && top_edge >= placement.y
            && right_edge <= right
            && bottom_edge <= bottom,
        // `<=` / `>=` 而不是相等：位图外面没有像素，所以「顶到边」就是
        // 「包围盒碰到了边界那一行/列」。画面边同理（包围盒本来就在画面里）。
        on_edge: left_edge <= placement.x
            || top_edge <= placement.y
            || right_edge >= right
            || bottom_edge >= bottom
            || left_edge <= 0
            || top_edge <= 0
            || right_edge >= target.0 as i32
            || bottom_edge >= target.1 as i32,
    }
}

impl ProjectHost {
    /// 清单与行位图一起作废：画布尺寸变了、字幕换了、清单重算了都要走这里。
    ///
    /// 为什么是**一起**作废而不是各管各的：落点是按目标像素算的、位图是按落点声明的
    /// 尺寸画的，两者只在「同一份清单 + 同一个画布」里互相成立。只作废一半，
    /// 剩下那一半会被当成有效数据继续用 —— 而它看起来完全正常（只是位置/内容是旧的），
    /// 正是这个项目最不想要的那类错。
    fn invalidate_text(&mut self) {
        self.text_lines.clear();
        self.text_frame = None;
        for (_, bitmap) in self.text_bitmaps.drain() {
            bitmap.close();
        }
    }

    fn draw(&mut self, frame: i64) -> Result<(), String> {
        // **顺带把文档坐标系取出来。** 预览的渲染目标是画布，而契约里的像素量
        // （transform.x/y、调整图层的模糊半径）以 render_hints 度量 —— 两者不等时
        // 由 RenderSpace 按比例换算。少了这一步，同一个工程在不同画布尺寸下
        // 位移的相对位置就不一样，也就是「预览所见 != 成片所得」。
        let loaded = PROJECT.with(|slot| {
            slot.borrow().as_ref().map(|doc| {
                let assets = doc.asset_timebases();
                let composite =
                    compose::evaluate_v2_with_assets(&doc.timeline, frame, Some(&assets));
                (composite, doc.sequence_size())
            })
        });
        let Some((composite, sequence)) = loaded else {
            return Err("还没有载入通过校验的工程".to_string());
        };

        // 拆分借用：这些字段互不相干，解析器只需要其中几个的不可变借用。
        let Self {
            ctx,
            sink,
            renderer,
            videos,
            bitmaps,
            require_bitmap,
            size,
            text_lines,
            text_frame,
            text_bitmaps,
        } = self;
        let (width, height) = *size;
        let sink_format = sink.format();
        let sink_view = sink.acquire(&ctx.device);
        let space = RenderSpace { sequence: sequence, target: (width, height) };
        let mut resolver = BoundVideos {
            device: &ctx.device,
            queue: &ctx.queue,
            videos,
            bitmaps,
            require_bitmap: *require_bitmap,
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
            space,
            &composite,
            &mut resolver,
            wgpu::Color::TRANSPARENT,
        );
        // **文字叠在底上，不清屏** —— 清屏是上面那一句的事（compose_overlay 绝不清屏，
        // 见那里的说明：同一帧清两次的症状是「底没了」，看起来像「字画错了」）。
        //
        // 落点清单对不上这一帧就**不画**：宁可这一帧没有字幕，也不能把上一帧的字
        // 按上一帧的落点画上去 —— 那种画面看起来完全正常，只是"慢了半拍"。
        if *text_frame == Some(frame) && !text_lines.is_empty() {
            let uploaded = upload_text_bitmaps(
                &ctx.device,
                &ctx.queue,
                sink_format,
                text_lines,
                text_bitmaps,
            );
            let items = overlay_items(text_lines, &uploaded, None);
            compose_overlay(
                renderer.compositor(),
                &ctx.device,
                &ctx.queue,
                &mut encoder,
                &sink_view,
                (width, height),
                &items,
            );
        }
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

/// 把求值结果转成宿主 API 的形状。
///
/// `overlay` 由调用方算好交进来 —— **不在这里读 `SUBTITLES`**：这个函数只做形状转换，
/// 加一次隐式的全局读，它就不再是「一个可以被单独看懂的转换」了。
fn composite_result(
    composite: &Composite,
    overlay: Option<dhampir_core::timeline::host_api::OverlayView>,
) -> dhampir_core::timeline::host_api::FrameResult {
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
        overlay,
        error: None,
    }
}

/// 这一帧的文字覆盖层，转成宿主 API 的形状。
///
/// # 与 `dhampir_project_text_frame` 的关系
///
/// 两处读的是**同一张表、同一份评估**（`evaluate_overlay`）：这里只做形状转换。
/// 各算一遍就会有两套说法，而「这一帧该画哪几行」正是两端要比对的那件事。
///
/// # 为什么这里不给像素落点
///
/// `place_line` 要目标尺寸，而返回体里给的是**归一化矩形** —— 宿主自己多大就乘多大。
/// 落点是宿主内部的事（栅格化与贴图那一段），不是跨边界的形状。
fn frame_overlay(doc: &ProjectDoc, frame: i64) -> Option<host_api::OverlayView> {
    SUBTITLES.with(|slot| {
        // **借出来用，不 clone** —— 这个函数会被逐帧调（图层面板就靠它）。
        let subtitles = slot.borrow();
        let overlay =
            evaluate_overlay(&doc.timeline, frame, doc.sequence_size(), Some(&subtitles))?;
        Some(host_api::OverlayView {
            items: overlay
                .items
                .iter()
                .map(|item| host_api::TextItemView {
                    text: item.text.clone(),
                    rect: item.rect.into(),
                })
                .collect(),
            color: overlay.color,
            outline: overlay.outline,
            dropped_lines: overlay.dropped_lines,
        })
    })
}

/// 这个宿主实现的**宿主 API 版本**。
///
/// 版本号不进每个返回体（理由见 `dhampir_core::timeline::host_api` 的模块注释）：
/// 对端问一次，记住就够了。查版本时用这个函数，形状对不上时对照 `docs/host-api.md`
/// —— 那份文档与这里的常量由守卫比对，所以「升了常量忘了改文档」不会静默通过。
#[wasm_bindgen]
pub fn dhampir_host_api_version() -> u32 {
    host_api::HOST_API_VERSION
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

/// 执行一次编辑操作。**与 CLI 走同一份实现**（dhampir_core::timeline::edit）。
///
/// 返回 {ok, summary, issues}；成功时新工程会写回宿主，
/// 前端随后调 dhampir_project_doc 拿规范化的那一份。
///
/// 规则不在这里：这一层只是通道。把剪辑规则写进 wasm 导出或前端，
/// 就会出现"CLI 与浏览器对同一次操作结果不同"，而那是这个项目最贵的那条不变量。
#[wasm_bindgen]
pub fn dhampir_project_edit(op_json: &str) -> String {
    use dhampir_core::timeline::schema::Issue;
    let op: dhampir_core::timeline::edit::EditOp = match serde_json::from_str(op_json) {
        Ok(op) => op,
        Err(error) => {
            return host_api::to_json(&serde_json::json!({
                "ok": false,
                "summary": "",
                "issues": [Issue::new("bad_op", "op", format!("不认识的编辑操作：{error}"))],
            }))
        }
    };
    // **先把当前工程克隆出来再改。** PROJECT 是 RefCell：一边 borrow 一边 borrow_mut
    // 会直接 panic，而那句话在 wasm 里就是一个 unreachable（整页死）。
    let current = PROJECT.with(|slot| slot.borrow().clone());
    let Some(doc) = current else {
        return host_api::to_json(&serde_json::json!({
            "ok": false,
            "summary": "",
            "issues": [Issue::new("no_project", "project", "还没有载入通过校验的工程".to_string())],
        }));
    };
    let outcome = dhampir_core::timeline::edit::apply(&doc, dhampir_core::effects::REGISTRY, &op);
    if outcome.is_ok() {
        // **成了才写回。** 没成就让宿主里那份保持原样 —— 半改状态比失败更难查。
        PROJECT.with(|slot| *slot.borrow_mut() = Some(outcome.doc.clone()));
    }
    host_api::to_json(&serde_json::json!({
        "ok": outcome.is_ok(),
        "summary": outcome.summary,
        "issues": outcome.issues,
    }))
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
///
/// 返回体里 `overlay` 是**这一帧的文字覆盖层**（要画哪几行字、各占哪个归一化矩形）；
/// 工程里没有字幕、或者这一帧没有活着的字幕时那个键**根本不出现**。
#[wasm_bindgen]
pub fn dhampir_project_frame(frame: i32) -> String {
    PROJECT.with(|slot| {
        let borrowed = slot.borrow();
        match borrowed.as_ref() {
            None => dhampir_core::timeline::host_api::to_json(&dhampir_core::timeline::host_api::FrameResult {
                frame: i64::from(frame),
                layers: Vec::new(),
                overlay: None,
                error: Some("还没有载入通过校验的工程".to_string()),
            }),
            Some(doc) => {
                let assets = doc.asset_timebases();
                host_api::to_json(&composite_result(
                    &compose::evaluate_v2_with_assets(
                        &doc.timeline,
                        i64::from(frame),
                        Some(&assets),
                    ),
                    frame_overlay(doc, i64::from(frame)),
                ))
            }
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
            .map(|doc| {
                let assets = doc.asset_timebases();
                compose::evaluate_v2_with_assets(&doc.timeline, i64::from(frame), Some(&assets))
            })
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
        RenderSpace::square((width.max(1), height.max(1))),
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

    let assets = doc.asset_timebases();
    let composite =
        compose::evaluate_v2_with_assets(&doc.timeline, i64::from(frame), Some(&assets));
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
        RenderSpace::square((width.max(1), height.max(1))),
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
            bitmaps: HashMap::new(),
            require_bitmap: false,
            size,
            text_lines: Vec::new(),
            text_frame: None,
            text_bitmaps: HashMap::new(),
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

/// 告诉宿主：这个浏览器的 WebGPU 接不接受 <video> 作为 copy 源。
///
/// 由 JS 探测后设置（见 web/engine.js 的 probeVideoCopy）。设成 true 之后
/// BoundVideos **不再尝试 video 那条路** —— 试一次的代价是整页死。
#[wasm_bindgen]
pub fn dhampir_project_set_bitmap_mode(required: bool) {
    PROJECT_HOST.with(|h| {
        if let Some(host) = h.borrow_mut().as_mut() {
            host.require_bitmap = required;
        }
    });
}

/// 清掉上一帧的位图。
///
/// **每帧都要先清。** 不清的话，这一帧不再出现的 source 会拿着上一帧的位图
/// 被画出来 —— 而画面看起来完全正常，只是"慢了半拍"。那种错没人查得出来。
#[wasm_bindgen]
pub fn dhampir_project_clear_bitmaps() {
    PROJECT_HOST.with(|h| {
        if let Some(host) = h.borrow_mut().as_mut() {
            for (_, bitmap) in host.bitmaps.drain() {
                bitmap.close();
            }
        }
    });
}

/// JS 把某个 source **当前帧**转成位图交给宿主。
///
/// 必须在 seek 完成之后做 —— 早了拿到的是上一帧，而画面看起来完全正常。
/// 换掉旧位图时把它 close() 掉：不关会一直占着显存/内存。
#[wasm_bindgen]
pub fn dhampir_project_set_bitmap(source: String, bitmap: web_sys::ImageBitmap) {
    PROJECT_HOST.with(|h| {
        if let Some(host) = h.borrow_mut().as_mut() {
            if let Some(previous) = host.bitmaps.insert(source, bitmap) {
                previous.close();
            }
        }
    });
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
        // 时间线的时间基不合法就直接报错 —— 这一步是**校验**，不是换算。
        if let Err(error) = doc.timeline.timebase.to_timebase() {
            return host_api::to_json(&host_api::SourcesResult {
                frame: i64::from(frame),
                sources: Vec::new(),
                error: Some(error.to_string()),
            });
        }
        let assets = doc.asset_timebases();
        let composite =
            compose::evaluate_v2_with_assets(&doc.timeline, i64::from(frame), Some(&assets));
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
                // **用素材自己的时间基**。以前用的是时间线的 —— 素材帧率不同时
                // 那个秒数就是错的，而表现是"画面看起来正常但慢了/快了一截"。
                seconds: assets
                    .get(&layer.source)
                    .and_then(|timebase| {
                        dhampir_core::timeline::layer::seconds_at_asset_frame(
                            layer.source_frame,
                            timebase,
                        )
                    })
                    .unwrap_or(0.0),
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

// ---------------------------------------------------------------------------
// 文字（T2.5）：清单（Rust）-> 栅格化（JS 的 canvas）-> 位图（Rust 收下）
//
// 分工的理由写在上面那段「文字叠加」的分节里。这一组导出就是那份分工的接线：
//   * `dhampir_project_text_frame` 给清单（含目标像素落点），JS 照着它画；
//   * `dhampir_project_set_subtitles` 收字幕原文（**解析在 Rust**）；
//   * `dhampir_project_set_text_bitmap` 收画好的行位图；
//   * `dhampir_project_text_probe` 是判定入口：把「加字前后」比一比。
// ---------------------------------------------------------------------------

/// `text_frame` 的失败形状：**空数组也照样给**。
///
/// 少一个键，JS 那边就是 `undefined.length` 抛异常 —— 而异常看起来像「页面坏了」，
/// 不像「这一帧没有字幕」。调用方要能无条件地遍历这两个数组。
fn text_frame_error(frame: i64, message: &str) -> String {
    host_api::to_json(&serde_json::json!({
        "frame": frame,
        "error": message,
        "subtitle_assets": 0,
        "items": [],
        "placements": [],
        "color": serde_json::Value::Null,
        "outline": serde_json::Value::Null,
        "dropped_lines": 0,
        "unplaced_lines": 0,
        "issues": [],
    }))
}

/// 工程里登记了、而宿主这里还没有表的字幕素材。
///
/// 「素材登记了、表没有」是**配置错**，不是「这部片子没有字幕」：两者的输出一模一样，
/// 而后者是「看起来成功、其实不对」的典型。
fn unregistered_subtitles<'a>(doc: &'a ProjectDoc, subtitles: &SubtitleTable) -> Vec<&'a str> {
    doc.assets
        .iter()
        .filter(|asset| asset.kind == AssetKind::Subtitle && !subtitles.contains_key(&asset.id))
        .map(|asset| asset.id.as_str())
        .collect()
}

/// 这份工程里**有表**的字幕素材数 —— 与 CLI `subtitle` 子命令的 `subtitle_assets` 同口径
/// （那边是 `table.len()`，而那张表是按工程里 `kind == subtitle` 的素材建的）。
///
/// 不能直接数 `SUBTITLES` 的键数：那是**本页面会话**的表，载入新工程时不清
/// （理由见 `SUBTITLES` 自己的说明），于是它可以带着别的工程留下的素材。
fn registered_subtitles(doc: &ProjectDoc, subtitles: &SubtitleTable) -> usize {
    doc.assets
        .iter()
        .filter(|asset| asset.kind == AssetKind::Subtitle && subtitles.contains_key(&asset.id))
        .count()
}

/// 这一帧要画哪几行字、各落在哪（**目标像素**），给 JS 去栅格化。
///
/// # 为什么由宿主给清单
///
/// 「这一帧哪几条字幕活着」在 `evaluate_overlay` 里，「这一行落在哪」在 `place_line` 里 ——
/// 两份都是共享算术。让 JS 自己算就是把这两份复制到第二处，于是两端迟早会画出
/// 结构不同、位置也不同的两批字。JS 只做一件事：照着 `placements` 里的
/// `bitmap_width/height/font_px` 把这一行画出来。
///
/// # 判据字段与 CLI 的 `subtitle` 子命令**逐字段同名**
///
/// `items[{text,rect}]`、`color`、`outline`、`dropped_lines`、`subtitle_assets` 与
/// `dhampir subtitle --frame` 的输出同名，于是两端比对不需要一张映射表（映射表自己会漂）。
/// 多出来的是 `placements`（像素落点）、`target`（宿主尺寸）、`unplaced_lines`
/// —— CLI 那一侧不画图，所以它没有这三个。
///
/// # 问题码
///
/// 与 worker 同一张判据表（见 `crates/dhampir-worker/src/text_overlay.rs` 顶上的表）。
/// 多一个 `subtitle_unregistered`：工程里登记了字幕素材、宿主却一个字幕表都没有 ——
/// CLI 那一侧这种情况是"读不了文件"的**硬错误**，浏览器这侧没有文件系统，
/// 「读不了」表现为"JS 没交进来"，只能是个问题码。
#[wasm_bindgen]
pub fn dhampir_project_text_frame(frame: i32) -> String {
    let frame = i64::from(frame);
    let doc = PROJECT.with(|slot| slot.borrow().clone());
    let Some(doc) = doc else {
        return text_frame_error(frame, "还没有载入通过校验的工程");
    };
    let subtitles = SUBTITLES.with(|slot| slot.borrow().clone());
    PROJECT_HOST.with(|h| {
        let mut borrowed = h.borrow_mut();
        let Some(host) = borrowed.as_mut() else {
            return text_frame_error(frame, "工程预览宿主尚未初始化，先调 dhampir_project_attach");
        };
        let target = host.size;
        let (overlay, lines) = text_lines(&doc, frame, &subtitles, target);
        let mut issues: Vec<Issue> = Vec::new();
        for asset_id in unregistered_subtitles(&doc, &subtitles) {
            issues.push(Issue::new(
                "subtitle_unregistered",
                "assets",
                format!("字幕素材 {asset_id} 没有交给宿主：先调 dhampir_project_set_subtitles"),
            ));
        }
        let (items, color, outline, dropped_lines) = match &overlay {
            Some(overlay) => (
                overlay
                    .items
                    .iter()
                    .map(|item| text_item_json(&item.text, item.rect))
                    .collect::<Vec<_>>(),
                serde_json::json!(overlay.color),
                serde_json::json!(overlay.outline),
                overlay.dropped_lines,
            ),
            None => (
                Vec::new(),
                serde_json::Value::Null,
                serde_json::Value::Null,
                0,
            ),
        };
        // 有行、却算不出落点的那些：**数出来**。静默少一行，看起来与「这一行本来就没有」一样。
        let placed = lines.len();
        let unplaced = overlay
            .as_ref()
            .map(|o| o.items.len())
            .unwrap_or(0)
            .saturating_sub(placed);
        let sequence = doc.sequence_size();
        let json = host_api::to_json(&serde_json::json!({
            "frame": frame,
            "sequence": [sequence.0, sequence.1],
            "target": [target.0, target.1],
            "subtitle_assets": registered_subtitles(&doc, &subtitles),
            "items": items,
            "placements": lines.iter().map(|line| {
                let mut value = text_item_json(&line.text, line.rect);
                value["x"] = serde_json::json!(line.placement.x);
                value["y"] = serde_json::json!(line.placement.y);
                value["bitmap_width"] = serde_json::json!(line.placement.bitmap_width);
                value["bitmap_height"] = serde_json::json!(line.placement.bitmap_height);
                value["font_px"] = serde_json::json!(line.placement.font_px);
                // 描边宽度也来自共享几何 —— JS 侧照着画就行，不许自己推一遍。
                value["border_px"] = serde_json::json!(border_px(line.placement.font_px));
                // 全是空白字符的行：栅格化出来本来就是空的，判它等于判「空格没有墨迹」。
                value["visible"] = serde_json::json!(is_visible(&line.text));
                value
            }).collect::<Vec<_>>(),
            "color": color,
            "outline": outline,
            "dropped_lines": dropped_lines,
            "unplaced_lines": unplaced,
            "issues": issues,
        }));
        // **算好了才记账。** JS 是照着这份清单去栅格化的，`draw` 只认这一份。
        //
        // 换清单时把旧位图一起丢掉：行号是按位置编的，新清单的 0 号位未必还是上一条字幕
        // —— 留着旧位图，某一行的栅格化万一没成功，`draw` 就会拿**另一条字幕的像素**
        // 贴上去，而画面看起来只是"这一帧的字没变"。
        host.invalidate_text();
        host.text_lines = lines;
        host.text_frame = Some(frame);
        json
    })
}

/// `{text, rect}` —— 与 CLI 的 `cmd_subtitle` 同一形状（逐字段同名）。
fn text_item_json(text: &str, rect: NormalizedRect) -> serde_json::Value {
    serde_json::json!({
        "text": text,
        "rect": {
            "x": rect.x,
            "y": rect.y,
            "width": rect.width,
            "height": rect.height,
        },
    })
}

/// 这一行画出来看得见吗（有非空白字符）。
fn is_visible(text: &str) -> bool {
    text.chars().any(|ch| !ch.is_whitespace())
}

/// 登记一份字幕素材：**解析在 Rust**。
///
/// 浏览器没有文件系统，所以「读」只能由 JS 做（fetch 一段文本）。但「读到的这段文本
/// 是哪几条字幕」必须两端同源 —— 所以解析走 `parse_srt` / `parse_ass`，
/// 与 CLI 的 `load_subtitles` 是同一份实现。
///
/// `format` 是扩展名（`"srt"` / `"ass"` / `"ssa"`）：CLI 认的就是文件名后缀，
/// 让浏览器按内容嗅探就会多出第二套判定。
///
/// 解析失败**不静默**：那样「这部片子没有字幕」与「字幕没读进来」的输出会一模一样。
#[wasm_bindgen]
pub fn dhampir_project_set_subtitles(asset_id: String, text: String, format: String) -> String {
    let parsed = match format.to_ascii_lowercase().as_str() {
        "srt" => parse_srt(&text),
        "ass" | "ssa" => parse_ass(&text),
        other => Err(format!("不认得这个字幕格式：{other}（现在只认 srt/ass/ssa）")),
    };
    let report = match parsed {
        Ok(report) => report,
        Err(error) => {
            return host_api::to_json(&serde_json::json!({
                "ok": false,
                "asset": asset_id,
                "cues": 0,
                "skipped": 0,
                "issues": [Issue::new(
                    "subtitle_parse_failed",
                    "assets",
                    format!("{asset_id} 解析失败：{error}"),
                )],
            }))
        }
    };
    let cues = report.cues.len();
    let skipped = report.skipped;
    SUBTITLES.with(|slot| {
        slot.borrow_mut().insert(asset_id.clone(), report.cues);
    });
    // 字幕换了，照着旧字幕算出来的清单与位图就都不算数了 —— 行号是**按位置**编的，
    // 于是「第 0 行」现在可能是另一条字幕，而旧位图还挂在 0 号上。
    // 不作废的话画面上会出现一条**内容属于上一条字幕**的字，位置却对（编号对得上），
    // 那比多一行少一行难查得多。
    PROJECT_HOST.with(|h| {
        if let Some(host) = h.borrow_mut().as_mut() {
            host.invalidate_text();
        }
    });
    host_api::to_json(&serde_json::json!({
        "ok": true,
        "asset": asset_id,
        "cues": cues,
        "skipped": skipped,
        "issues": [],
    }))
}

/// 清掉上一帧的行位图。
///
/// 与 `dhampir_project_clear_bitmaps` 同一个理由：不清的话，这一帧不再出现的行会拿着
/// 上一帧的位图被画上去 —— 而画面看起来完全正常，只是"慢了半拍"。
#[wasm_bindgen]
pub fn dhampir_project_clear_text_bitmaps() {
    PROJECT_HOST.with(|h| {
        if let Some(host) = h.borrow_mut().as_mut() {
            for (_, bitmap) in host.text_bitmaps.drain() {
                bitmap.close();
            }
        }
    });
}

/// JS 把某一行的位图交给宿主。`index` 是 `dhampir_project_text_frame` 给的清单下标。
///
/// 换掉旧位图时 `close()` 掉：不关会一直占着显存/内存。
///
/// 越界的下标**报错而不是丢掉**：那说明 JS 拿的清单与宿主手上的不是同一份，
/// 而"丢掉"会让那一行静默消失。
#[wasm_bindgen]
pub fn dhampir_project_set_text_bitmap(index: u32, bitmap: web_sys::ImageBitmap) -> Result<(), JsValue> {
    PROJECT_HOST.with(|h| {
        let mut borrowed = h.borrow_mut();
        let host = borrowed
            .as_mut()
            .ok_or_else(|| js_err("工程预览宿主尚未初始化，先调 dhampir_project_attach"))?;
        if index as usize >= host.text_lines.len() {
            return Err(js_err(format!(
                "行号 {index} 超出这一帧的行数（{}）——先调 dhampir_project_text_frame 拿清单",
                host.text_lines.len()
            )));
        }
        if let Some(previous) = host.text_bitmaps.insert(index, bitmap) {
            previous.close();
        }
        Ok(())
    })
}

/// 判定入口：把「加字之前」与「加字之后」两张读回来的图比一比。
///
/// # 为什么逐行分离
///
/// 一次比出来的墨迹里混着所有行 —— 某一行位置不对，报告只能说「这一帧不对」。
/// 所以每行各渲染一次「减去这一行」（其余行都在），与「全都在」比：
/// 差出来的就是**这一行**的墨迹，于是报告能指到行。
/// 渲染次数 = 2 + 行数（无字 / 全都有 / 每行各减一次）。行数是几，不是几十。
///
/// # 判据（是判定，不是观测）
///
///   * 有字要画、这一行却没有位图 -> `subtitle_raster_failed`（JS 那侧没栅格化出来）；
///   * 有位图但尺寸与落点声明不符 -> `subtitle_blit_failed`（`compose_overlay` 会拒绝画它）；
///   * 位图正常、墨迹却为零 -> `subtitle_blit_failed`（这一行没贴上）；
///   * 墨迹顶到位图边或画面边 -> `subtitle_ink_clipped`（字被切了）；
///   * 墨迹跑到落点方框外面 -> `subtitle_ink_clipped`（消息里说清是哪一种）。
///
/// 全是空白字符的行**不判**：它栅格化出来本来就是空的。
#[wasm_bindgen]
pub async fn dhampir_project_text_probe(frame: i32) -> Result<String, JsValue> {
    async fn read_image(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        texture: &wgpu::Texture,
    ) -> Result<Rgba8Image, JsValue> {
        dhampir_core::readback::read_texture_rgba8(device, queue, texture)
            .await
            .map_err(|error| js_err(error.to_string()))
    }

    let frame_number = i64::from(frame);
    let doc = PROJECT.with(|slot| slot.borrow().clone())
        .ok_or_else(|| js_err("还没有载入通过校验的工程"))?;
    let subtitle_assets = SUBTITLES.with(|slot| registered_subtitles(&doc, &slot.borrow()));

    // GPU 的活全在这一段里同步做完（渲染是同步的），读回在借用外面 await：
    // RefCell 的借用不许跨 await 拿在手上（一句 `borrow_mut` 撞上另一句就是 unreachable）。
    let mut prepared = PROJECT_HOST.with(|h| -> Result<PreparedProbe, String> {
        let mut borrowed = h.borrow_mut();
        let Some(host) = borrowed.as_mut() else {
            return Err("工程预览宿主尚未初始化，先调 dhampir_project_attach".to_string());
        };
        // **只判「刚算过的那一份清单」**：`draw` 画的就是它，判另一份等于判了个寂寞。
        if host.text_frame != Some(frame_number) {
            return Err(format!(
                "这一帧的行清单还没算过（现在存的是 {:?}）——先调 dhampir_project_text_frame",
                host.text_frame
            ));
        }
        let (width, height) = host.size;
        let sequence = doc.sequence_size();
        let assets = doc.asset_timebases();
        let composite =
            compose::evaluate_v2_with_assets(&doc.timeline, frame_number, Some(&assets));
        let format = host.sink.format();
        let uploaded =
            upload_text_bitmaps(&host.ctx.device, &host.ctx.queue, format, &host.text_lines, &host.text_bitmaps);

        // 0 = 无字，1 = 全都有，2+i = 减去第 i 行。
        let mut textures = Vec::with_capacity(2 + host.text_lines.len());
        for _ in 0..2 + host.text_lines.len() {
            textures.push(host.ctx.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("dhampir project text probe"),
                size: wgpu::Extent3d {
                    width: width.max(1),
                    height: height.max(1),
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            }));
        }
        let views: Vec<wgpu::TextureView> = textures
            .iter()
            .map(|texture| texture.create_view(&wgpu::TextureViewDescriptor::default()))
            .collect();
        let mut resolver = BoundVideos {
            device: &host.ctx.device,
            queue: &host.ctx.queue,
            videos: &host.videos,
            bitmaps: &host.bitmaps,
            require_bitmap: host.require_bitmap,
            format,
            textures: HashMap::new(),
        };
        let mut encoder = host.ctx.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("dhampir project text probe encoder"),
        });
        let space = RenderSpace { sequence, target: (width, height) };
        for (pass, view) in views.iter().enumerate() {
            // **每一趟都显式清屏**：两趟的底必须是同一张，
            // 不然比出来的「墨迹」里混着两次底的差异。
            host.renderer.render_frame(
                &host.ctx.device,
                &host.ctx.queue,
                &mut encoder,
                view,
                space,
                &composite,
                &mut resolver,
                wgpu::Color::TRANSPARENT,
            );
            let skip = match pass {
                0 => Some(None), // 一趟都不画
                1 => None,       // 全画
                other => Some(Some(other - 2)),
            };
            let items = match skip {
                Some(None) => Vec::new(),
                Some(Some(index)) => overlay_items(&host.text_lines, &uploaded, Some(index)),
                None => overlay_items(&host.text_lines, &uploaded, None),
            };
            compose_overlay(
                host.renderer.compositor(),
                &host.ctx.device,
                &host.ctx.queue,
                &mut encoder,
                view,
                (width, height),
                &items,
            );
        }
        host.ctx.queue.submit([encoder.finish()]);
        Ok(PreparedProbe {
            device: host.ctx.device.clone(),
            queue: host.ctx.queue.clone(),
            width,
            height,
            textures,
            lines: host.text_lines.clone(),
            bitmaps: uploaded
                .iter()
                .map(|slot| slot.as_ref().map(|(_, _, size)| *size))
                .collect(),
            report: serde_json::json!({
                "frame": frame_number,
                "sequence": [sequence.0, sequence.1],
                "target": [width, height],
                "subtitle_assets": subtitle_assets,
            }),
            issues: Vec::new(),
        })
    })
    .map_err(js_err)?;

    let plain = read_image(&prepared.device, &prepared.queue, &prepared.textures[0]).await?;
    let full = read_image(&prepared.device, &prepared.queue, &prepared.textures[1]).await?;
    let total = ink_report(&plain, &full).map_err(js_err)?;

    let mut lines_pixels = 0_u64;
    let mut line_reports = Vec::with_capacity(prepared.lines.len());
    for (index, line) in prepared.lines.iter().enumerate() {
        let without =
            read_image(&prepared.device, &prepared.queue, &prepared.textures[2 + index]).await?;
        let ink = ink_report(&without, &full).map_err(js_err)?;
        let check = check_ink(&ink, line.placement, (prepared.width, prepared.height));
        let visible = is_visible(&line.text);
        let bitmap = prepared.bitmaps[index];
        lines_pixels += check.pixels;
        let declared = (line.placement.bitmap_width, line.placement.bitmap_height);
        // 每个问题的 `path` 都带行号与内容片段：一份报告里出现两条同码问题时，
        // 能看出是哪一行 —— 只报码的话，多行画面里等于没说。
        let path = format!("subtitle[{index}] {}", text_label(&line.text));
        if visible {
            match bitmap {
                None => prepared.issues.push(Issue::new(
                    "subtitle_raster_failed",
                    &path,
                    "这一行没有位图：JS 那侧没把它栅格化出来".to_string(),
                )),
                Some(size) if size != declared => prepared.issues.push(Issue::new(
                    "subtitle_blit_failed",
                    &path,
                    format!(
                        "位图尺寸与落点不符：位图 {}x{}、落点声明 {}x{}（这一行不会被画）",
                        size.0, size.1, declared.0, declared.1
                    ),
                )),
                Some(_) if check.pixels == 0 => prepared.issues.push(Issue::new(
                    "subtitle_blit_failed",
                    &path,
                    "这一行没有留下任何墨迹（位图在画面里吗？）".to_string(),
                )),
                Some(_) if !check.inside => prepared.issues.push(Issue::new(
                    "subtitle_ink_clipped",
                    &path,
                    format!(
                        "墨迹落到落点方框外面：墨迹 {}、落点 {}x{}@({}, {})",
                        bounds_label(check.bounds),
                        declared.0,
                        declared.1,
                        line.placement.x,
                        line.placement.y
                    ),
                )),
                Some(_) if check.on_edge => prepared.issues.push(Issue::new(
                    "subtitle_ink_clipped",
                    &path,
                    "墨迹顶到位图或画面的边界：字被切了".to_string(),
                )),
                Some(_) => {}
            }
        }
        line_reports.push(serde_json::json!({
            "text": line.text,
            "rect": {
                "x": line.rect.x,
                "y": line.rect.y,
                "width": line.rect.width,
                "height": line.rect.height,
            },
            "placement": {
                "x": line.placement.x,
                "y": line.placement.y,
                "bitmap_width": line.placement.bitmap_width,
                "bitmap_height": line.placement.bitmap_height,
                "font_px": line.placement.font_px,
            },
            "bitmap": bitmap.map(|size| serde_json::json!({ "width": size.0, "height": size.1 })),
            "visible": visible,
            "ink": {
                "pixels": check.pixels,
                "bounds": check.bounds.map(bounds_json),
                "inside": check.inside,
                "on_edge": check.on_edge,
            },
        }));
    }

    prepared.report["lines"] = serde_json::json!(line_reports);
    prepared.report["ink"] = serde_json::json!({
        "pixels": total.pixels,
        "bounds": total.bounds.map(bounds_json),
        "lines_pixels": lines_pixels,
        // 两行各自的墨迹不会重叠（行盒是分开的），所以这两个数必须相等：
        // 不等就说明两次渲染之间有东西变了（或两行真的压在一起了）—— 那是结构错。
        "lines_overlap": total.pixels != lines_pixels,
    });
    prepared.report["issues"] = serde_json::json!(prepared.issues);
    Ok(host_api::to_json(&prepared.report))
}

/// 判定用的中间产物：渲染做完了（GPU 侧），读回还没做（要 await）。
struct PreparedProbe {
    device: wgpu::Device,
    queue: wgpu::Queue,
    width: u32,
    height: u32,
    /// 0 = 无字，1 = 全都有，2+i = 减去第 i 行。
    textures: Vec<wgpu::Texture>,
    lines: Vec<TextLineSpec>,
    /// 每行位图的实际尺寸（`None` = 这一行没有位图）。
    bitmaps: Vec<Option<(u32, u32)>>,
    /// 已经填好结构的那半份报告，`lines` / `ink` / `issues` 待填。
    report: serde_json::Value,
    issues: Vec<Issue>,
}

/// 报告里指认一行用的短标签：内容截断成 24 个字符。
///
/// 与 worker 的 `issue_path` 同一个口径（同一段文字在两端要长得一样），
/// 也同一个理由：一条字幕整段贴进消息里，报告就没法读了。
fn text_label(text: &str) -> String {
    let mut label: String = text.chars().take(24).collect();
    if text.chars().count() > 24 {
        label.push('…');
    }
    label
}

/// 墨迹包围盒的 JSON（`None` = 一个像素都没变）。
fn bounds_json(bounds: dhampir_core::render::InkBounds) -> serde_json::Value {
    serde_json::json!({
        "x": bounds.x,
        "y": bounds.y,
        "width": bounds.width,
        "height": bounds.height,
    })
}

/// 消息里描述包围盒用的短标签。
fn bounds_label(bounds: Option<dhampir_core::render::InkBounds>) -> String {
    match bounds {
        None => "（没有）".to_string(),
        Some(bounds) => format!(
            "{}x{}@({}, {})",
            bounds.width, bounds.height, bounds.x, bounds.y
        ),
    }
}

/// canvas 尺寸变了之后重新配置 surface。
///
/// **画布决定的是「渲染到多大」，不是「坐标系是什么」。** 坐标系来自工程的 render_hints
/// （见 ProjectDoc::sequence_size），两者不一致时由 RenderSpace 按比例换算。
/// 早先这里写的是一句「预览尺寸由宿主决定」——那句话本身没错，但它被读成了
/// 「宿主也是坐标系」，于是 transform 的像素量跟着画布尺寸变，预览与成片对不上。
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
        // 落点是**目标像素**：画布一换尺寸，上一份清单与上一批行位图就全部对不上了。
        // 不作废的话它们看起来仍然有效（`text_frame` 还是这一帧），于是字会按旧尺寸贴上去。
        // 作废的代价只是「这一帧要等 JS 重新栅格化」，而那是它本来就要做的事。
        host.invalidate_text();
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
