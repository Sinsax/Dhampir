//! 文字叠加：把**已经栅格化好的行位图**按共享布局给出的落点叠进目标帧。
//!
//! # 三件事分开，各有各的归属
//!
//!   1. **这一帧有哪几行、各占哪个归一化矩形** —— 评估层（[`crate::overlay`]），纯函数，
//!      两端必须给出同一份结构；
//!   2. **字形像素长什么样** —— 宿主（CLI 走 ffmpeg drawtext，浏览器走 canvas），
//!      两端**允许**不同，证据里也不比这个；
//!   3. **位图放在哪** —— 共享布局（[`place_line`](dhampir_timeline::text_layout::place_line)）
//!      给出目标像素里的落点，本模块只负责把它画上去。
//!
//! 三条里只有第 3 条会被两端比对（两端各写一份「位图放在哪」的算术，迟早会漂，
//! 而漂了以后两端各自都是自洽的）。所以它不能有第二份实现 —— 这一层存在的理由就是这个。
//!
//! # 为什么在 core 而不是在宿主
//!
//! 「把一张带 alpha 的位图按左上角落点叠到目标上」是一个原语，而原语的正确性
//! （1:1、一次重采样都不做、alpha 不预乘）值得只被证明一次。宿主各写一份的代价
//! 不是重复几十行代码，而是**两端的落点会漂**。
//!
//! # 与 worker 的 CPU 叠加不是同一件事
//!
//! worker 出片那条路已经有 `text_overlay::blit`：那是把位图按行拷进一张**已经读回内存**
//! 的 RGBA8 图（CPU 逐像素）。这里是把纹理画进纹理（GPU 一次 draw）。
//! 换一条路线就得换一份实现，但**落点的来源是同一个**（`place_line`）—— 那才是要共享的东西。
//!
//! # 判定与观测
//!
//! [`ink_report`] 是给宿主用的：把「加字之前」与「加字之后」两张读回来的图比一下，
//! 得到墨迹的像素数与包围盒。包围盒与 `place_line` 给的落点对不上，就是
//! 「字放上去了，但位置不对」——那是**判据**，不是观测（见 `plan/roadmap.md` T2 的验收口径）。

use dhampir_timeline::layer::BlendMode;
use dhampir_timeline::schema::Transform;
use dhampir_timeline::text_layout::LinePlacement;

use super::compose::{Compositor, LayerDraw, RenderSpace};
use crate::readback::Rgba8Image;

/// 要叠上的一行：位图纹理 + 它在目标帧里的落点。
pub struct OverlayItem<'a> {
    /// 栅格化好的行位图，**直排 alpha**（不预乘）。
    ///
    /// 不预乘是硬要求：合成按 `SrcAlpha / OneMinusSrcAlpha` 走，也就是着色器先拿直排的
    /// `rgb`、再由混合方程乘 alpha。给一张预乘的位图不会报错，只会让边缘发暗 ——
    /// 症状看起来像「字体渲染得不太好」，而不像「叠加算错了」。
    pub view: &'a wgpu::TextureView,
    /// 这张纹理**实际**的尺寸。
    ///
    /// 必须等于 `placement.bitmap_width/height`。两者不等说明宿主把位图与落点配错了对，
    /// 硬画上去就是一次静默缩放（字糊一点、位置还差不多）。数出来、不画。
    pub bitmap_size: (u32, u32),
    /// 目标像素里的落点，由 `place_line` 给出。
    pub placement: LinePlacement,
}

/// 叠加的结果。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OverlayReport {
    /// 真正画上去的行数。
    pub drawn: usize,
    /// 被拦住没画的行数：声明尺寸与实际纹理不符，或尺寸为 0。**正常恒为 0**。
    pub size_mismatch: usize,
}

impl OverlayReport {
    /// 什么都没画、也没有被拦住的（空清单的常见情形）。
    pub fn is_silent(&self) -> bool {
        self.drawn == 0 && self.size_mismatch == 0
    }
}

/// 落点 -> 合成变换。
///
/// 变换的语义（[`inverse_affine`](super::compose::inverse_affine) 里写死的定义）是
/// 「把源图的**中心**搬到 目标中心 + `(x, y)`」，所以落点（左上角）要先换算成中心：
///
/// ```text
/// x = 落点.x + 位图宽 / 2 - 目标宽 / 2
/// y = 落点.y + 位图高 / 2 - 目标高 / 2
/// ```
///
/// 为什么从 `placement.x/y` 反推、而不是从行盒中心重算一遍：判定要比的就是
/// `placements[].x` 与墨迹包围盒，两条路必须是**同一个数**（落点先四舍五入成整数像素，
/// 再从它反推；从行盒中心另算一次会差半个像素以内，而「半个像素」正是最难查的那种）。
///
/// `scale` 恒为 1、`rotation` 恒为 0：位图本来就是按目标像素栅格化的，
/// 这条路上**一次重采样都不该有**。要缩放或旋转文字是另一个功能（届时改的是这里）。
pub fn placement_transform(placement: LinePlacement, target: (u32, u32)) -> Transform {
    Transform {
        x: placement.x as f32 + placement.bitmap_width as f32 / 2.0 - target.0 as f32 / 2.0,
        y: placement.y as f32 + placement.bitmap_height as f32 / 2.0 - target.1 as f32 / 2.0,
        scale: 1.0,
        rotation_deg: 0.0,
    }
}

/// 把 `items` 按给定顺序（从下往上）叠进 `target`。
///
/// # 绝不清屏
///
/// 它叠在**已经建立好的底**上：清屏是
/// [`TimelineRenderer::render_frame`](super::TimelineRenderer::render_frame) 的 `clear`
/// 参数的事。这里再给一个 clear 参数只会多出「同一帧清了两次」这种错，而它的症状
/// （底没了）看起来像「字画错了」。
///
/// # 坐标系
///
/// `target_size` **必须与算落点时用的目标尺寸是同一个**：落点已经是目标像素，
/// 这里再乘一次比例就是重复换算。所以交给合成器的坐标系是 [`RenderSpace::square`] ——
/// 位图与帧 1:1，这条路上没有文档坐标系可换算（文档坐标系在 `place_line` 之前就用过了：
/// 归一化矩形是按文档尺寸算出来的）。
///
/// 目标尺寸为 0 时直接返回：那种帧根本不该走到渲染，静默返回比在这里建一个
/// 0 尺寸的 pass 好（后者会在 wgpu 校验里报一句与真实原因无关的话）。
pub fn compose_overlay(
    compositor: &Compositor,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    encoder: &mut wgpu::CommandEncoder,
    target: &wgpu::TextureView,
    target_size: (u32, u32),
    items: &[OverlayItem<'_>],
) -> OverlayReport {
    let mut report = OverlayReport::default();
    if target_size.0 == 0 || target_size.1 == 0 {
        return report;
    }

    let mut layers = Vec::with_capacity(items.len());
    for item in items {
        let declared = (item.placement.bitmap_width, item.placement.bitmap_height);
        if declared.0 == 0 || declared.1 == 0 || declared != item.bitmap_size {
            // 两类都拦：**尺寸为 0** 的位图过不了绑定校验（`place_line` 只会对
            // 「没有可画的东西」给出 `None`，走到这里说明宿主自己造了张空图）；
            // **尺寸不符** 的位图硬画就是一次静默缩放。两者都不是「画出来了但难看」，
            // 而是「画出来的东西不是算出来的那个」，所以不画、并让宿主记一条问题。
            report.size_mismatch += 1;
            continue;
        }
        layers.push(LayerDraw {
            view: item.view,
            source_size: declared,
            transform: placement_transform(item.placement, target_size),
            // 不透明度恒为 1：字幕的整体浓淡由颜色里的 alpha 表达（评估层的事），
            // 叠加层不再乘一次 —— 两处都能调透明度，就会有两处都调的那一天。
            opacity: 1.0,
            // 普通 source-over：文字与画面之间不做任何混合花样。
            blend: BlendMode::Normal,
        });
    }
    report.drawn = layers.len();
    if layers.is_empty() {
        // 一行都没画就**不碰 encoder**：一个「Load 之后什么都不画」的 pass 是空操作，
        // 但它会出现在抓帧里，让「这一帧到底有没有走叠加」变得看不出来。
        return report;
    }

    compositor.compose(device, queue, encoder, target, RenderSpace::square(target_size), &layers, None);
    report
}

/// 墨迹落在哪个矩形里（目标像素，左上角闭区间）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InkBounds {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// 两张同尺寸的 RGBA8：后一张比前一张**多了什么墨迹**。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InkReport {
    /// 变了的像素数。
    pub pixels: u64,
    /// 变了的像素落在哪个矩形里。一个都没变时是 `None`。
    pub bounds: Option<InkBounds>,
}

/// 比较底图与叠加后的图，数出**加字加出来的墨迹**。
///
/// # 为什么逐字节比较，而不是给个阈值
///
/// 两件事让「完全相同」是精确的、不是近似：
///
///   1. 这条路上没有重采样：位图与帧 1:1，叠加 pass 也不重新采一遍底图；
///   2. 位图空白处的 alpha 是 0，混合方程给的是 `dst` 本身 —— 精确相等，不是「接近」。
///
/// 于是「值变了」与「这里有字」是同一件事。反过来给一个容差会漏掉**低 alpha 的
/// 抗锯齿边缘**，而那正是「字只画了一半」最先显形的地方。
///
/// 这是**判据的一半**：包围盒与 `place_line` 给的落点对不上，就是「字放上去了但位置不对」
/// （预乘过的位图、算错的落点都会在这里显形）。另一半（几个像素才算有字、容差多大）
/// 由宿主的判据表定，见 `plan/roadmap.md` T2 的验收口径。
pub fn ink_report(base: &Rgba8Image, overlaid: &Rgba8Image) -> Result<InkReport, String> {
    if base.width != overlaid.width || base.height != overlaid.height {
        return Err(format!(
            "两张图尺寸不一致：{}x{} 与 {}x{} —— 不是同一帧，比出来的墨迹没有意义",
            base.width, base.height, overlaid.width, overlaid.height
        ));
    }
    let expected = base.width as usize * base.height as usize * 4;
    if base.pixels.len() != expected || overlaid.pixels.len() != expected {
        return Err(format!(
            "像素长度与尺寸不符：{}x{} 期望 {expected} 字节，得到 {} / {}",
            base.width,
            base.height,
            base.pixels.len(),
            overlaid.pixels.len()
        ));
    }

    let mut pixels: u64 = 0;
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (u32::MAX, u32::MAX, 0u32, 0u32);
    for y in 0..base.height {
        for x in 0..base.width {
            let at = ((y as usize * base.width as usize + x as usize) * 4) as usize;
            if base.pixels[at..at + 4] != overlaid.pixels[at..at + 4] {
                pixels += 1;
                min_x = min_x.min(x);
                min_y = min_y.min(y);
                max_x = max_x.max(x);
                max_y = max_y.max(y);
            }
        }
    }

    let bounds = if pixels == 0 {
        None
    } else {
        Some(InkBounds { x: min_x, y: min_y, width: max_x - min_x + 1, height: max_y - min_y + 1 })
    };
    Ok(InkReport { pixels, bounds })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::compose::inverse_affine;
    use dhampir_timeline::text_layout::{NormalizedRect, place_line};

    fn placement(x: i32, y: i32, bitmap_width: u32, bitmap_height: u32) -> LinePlacement {
        LinePlacement { x, y, bitmap_width, bitmap_height, font_px: 16 }
    }

    /// 位图的四个角在**源像素**坐标里的对应点（用生产那份逆变换算）。
    ///
    /// 问的是「目标像素 p 落在源图哪里」，所以给 `p = 落点` 应当得到 `0`，
    /// 给 `p = 落点 + 位图宽` 应当得到 `位图宽`。**中间不夹任何测试自己写的变换** ——
    /// 这条测试要与 GPU 走同一段算术，否则测的是测试自己的算术。
    fn source_at(placement: LinePlacement, target: (u32, u32), at: (f32, f32)) -> (f32, f32) {
        let (row0, row1) = inverse_affine(
            placement_transform(placement, target),
            (placement.bitmap_width, placement.bitmap_height),
            target,
        );
        (
            row0[0] * at.0 + row0[1] * at.1 + row0[2],
            row1[0] * at.0 + row1[1] * at.1 + row1[2],
        )
    }

    fn close(actual: f32, expected: f32) -> bool {
        (actual - expected).abs() < 1e-4
    }

    #[test]
    fn 位图左上角恰好落在落点上() {
        // 这条是 T2.5 的关键：宿主报的 `placements[].x/y` 与 GPU 真正画的位置
        // 必须是同一个数（判定要比它们）。差一个像素也算错。
        for case in [
            placement(0, 0, 640, 30),
            placement(10, 20, 640, 30),
            placement(319, 326, 640, 30),
            placement(-12, -8, 640, 30),
            placement(7, 3, 640, 24),
        ] {
            let target = (640, 360);
            let corner = source_at(case, target, (case.x as f32, case.y as f32));
            assert!(close(corner.0, 0.0) && close(corner.1, 0.0), "{case:?} 左上角落在 {corner:?}");
            let far = source_at(
                case,
                target,
                (case.x as f32 + case.bitmap_width as f32, case.y as f32 + case.bitmap_height as f32),
            );
            assert!(
                close(far.0, case.bitmap_width as f32) && close(far.1, case.bitmap_height as f32),
                "{case:?} 右下角落在 {far:?} —— 位图被缩放了",
            );
        }
    }

    #[test]
    fn 位图中心与行盒中心重合() {
        // 落点那一侧由共享布局算（`place_line` 的规则是「位图中心对准行盒中心」），
        // 这一侧由 `placement_transform` 送到 GPU。两边都别自己改规则。
        let target = (640, 360);
        for rect in [
            // 常见的字幕位置：底部居中。
            NormalizedRect { x: 0.25, y: 0.8, width: 0.5, height: 0.055 },
            // 居中、更大的行盒（字号反推出来的字号应当是 30）。
            NormalizedRect { x: 0.3, y: 0.45, width: 0.4, height: 0.1 },
        ] {
            let placed = place_line(rect, target).expect("行盒有高度，应当给得出落点");
            // 行盒中心（目标像素）—— 用契约给的方法算，不在这里重写一遍归一化算术。
            let line_box_center = (
                rect.center_x() * target.0 as f32,
                (rect.y + rect.height / 2.0) * target.1 as f32,
            );
            let at = source_at(placed, target, line_box_center);
            let expected = (placed.bitmap_width as f32 / 2.0, placed.bitmap_height as f32 / 2.0);
            assert!(
                (at.0 - expected.0).abs() <= 0.5 && (at.1 - expected.1).abs() <= 0.5,
                "行盒中心 {line_box_center:?} 该落在位图中心 {expected:?} 附近，得到 {at:?}",
            );
        }
    }

    #[test]
    fn 每一行各自换算不被上一行带跑() {
        // 合成器曾经踩过一次「所有层共用一块 uniform」：同一 pass 里的每次 draw
        // 都读到最后一次写入的值（实测症状是两个颜色都用了蓝色的参数）。
        // 这条钉住**换算本身**不是从某个共享状态来的。
        let target = (1280, 720);
        let first = placement(0, 600, 1280, 40);
        let second = placement(300, 20, 1280, 60);
        let t1 = placement_transform(first, target);
        let t2 = placement_transform(second, target);
        assert_ne!(t1, t2);
        assert!(close(t1.y, 600.0 + 20.0 - 360.0) && close(t2.y, 20.0 + 30.0 - 360.0));
    }

    #[test]
    fn 目标尺寸为零时不产生_nan() {
        // 尺寸为 0 的帧由 `compose_overlay` 直接拦住，但换算本身也不该给出
        // NaN/Inf —— 一个 NaN 变换会让整帧变成垃圾，而不是「这一帧没画」。
        let t = placement_transform(placement(0, 0, 640, 30), (0, 0));
        assert!(t.x.is_finite() && t.y.is_finite());
        assert_eq!(t.scale, 1.0);
        assert_eq!(t.rotation_deg, 0.0);
    }

    #[test]
    fn 墨迹包围盒只算变了的像素() {
        let mut base = Rgba8Image { width: 4, height: 3, pixels: vec![0; 4 * 3 * 4] };
        // 底图铺成不透明黑，便于「变了」＝「有墨」。
        for chunk in base.pixels.chunks_mut(4) {
            chunk[3] = 255;
        }
        let clean = ink_report(&base, &base).expect("同尺寸");
        assert_eq!(clean.pixels, 0);
        assert_eq!(clean.bounds, None, "一个像素都没变时不该报一个空矩形");

        let mut overlaid = base.clone();
        for at in [(1usize, 1usize), (2, 1), (2, 2)] {
            let index = (at.1 * 4 + at.0) * 4;
            overlaid.pixels[index] = 255;
        }
        let report = ink_report(&base, &overlaid).expect("同尺寸");
        assert_eq!(report.pixels, 3);
        assert_eq!(
            report.bounds,
            Some(InkBounds { x: 1, y: 1, width: 2, height: 2 }),
            "包围盒该是真正变了的范围，而不是整张图",
        );
    }

    #[test]
    fn 尺寸不一致时拒绝回答() {
        let a = Rgba8Image { width: 4, height: 3, pixels: vec![0; 4 * 3 * 4] };
        let b = Rgba8Image { width: 5, height: 3, pixels: vec![0; 5 * 3 * 4] };
        let error = ink_report(&a, &b).expect_err("尺寸不一致不该给结论");
        assert!(error.contains("尺寸不一致"), "报错要指出原因，得到 {error}");
        // 尺寸说是这么大、像素却不够长：这属于「图本身坏了」，同样不回答。
        let broken = Rgba8Image { width: 4, height: 3, pixels: vec![0; 8] };
        assert!(ink_report(&a, &broken).is_err());
    }
}
