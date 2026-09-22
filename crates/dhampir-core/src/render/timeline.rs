//! 按时间线把一帧画出来 —— **两个宿主都调它**。
//!
//! # 为什么这里必须有「一个入口」
//!
//! 双端比对的结论，只有在**两边跑的是同一份代码**时才有意义。
//! 如果 wasm 宿主与 worker 各写一遍「先模糊再合成」的调度，那比出来的差异
//! 既可能是运行时差异、也可能是两份驱动的写法差异——后者是纯粹的浪费。
//! 所以调度（谁先谁后、中间纹理从哪来）放在这里；宿主只提供两样东西：
//! 一块源纹理、一个能画的目标。
//!
//! # v1 的做法
//!
//! 1. 先给带特效的图层做模糊，各自画进一块临时纹理；
//! 2. 再把所有图层按顺序交给合成节点叠一次。
//!
//! 第 1 步每帧每层新建两块临时纹理——**这是刻意选笨的**：
//! 纹理池是宿主的资源策略（显存预算、LRU），不该埋在这一层里。
//! 等两端都跑通了再把它提到宿主去，那时换的是宿主，不是这里的语义。

use dhampir_timeline::schema::Effect;

use crate::compose::Composite;
use crate::render::blur::BlurRenderer;
use crate::render::compose::{Compositor, LayerDraw};
use crate::wgpu;

/// 源纹理的提供者。宿主实现它——浏览器那边是 video 元素，native 那边是解码器或文件。
pub trait SourceResolver {
    /// 给出这个 source 标识对应的纹理与尺寸。给不出来就返回 None（该层会被跳过）。
    fn texture_for(&mut self, source: &str) -> Option<(wgpu::TextureView, (u32, u32))>;
}

/// 图层要用的模糊半径。0 表示这层不需要模糊。
///
/// 半径会被夹到 [`crate::render::BLUR_MAX_RADIUS`]：登记表已经限制了取值范围，
/// 这里再兜一次，免得一个绕过校验的工程让着色器索引越界。
pub fn blur_radius(effects: &[Effect]) -> u32 {
    let mut radius = 0.0_f32;
    for effect in effects {
        if effect.kind != "gaussian_blur" {
            continue;
        }
        if let Some(value) = effect.params.get("radius") {
            if value.is_finite() && *value > radius {
                radius = *value;
            }
        }
    }
    // u32 -> f32 没有 From 实现，只能 as；这里范围远小于 2^24，转换是精确的。
    radius.max(0.0).round().min(crate::render::BLUR_MAX_RADIUS as f32) as u32
}

/// 时间线渲染器：合成 + 特效的调度。构造一次、每帧复用。
pub struct TimelineRenderer {
    compositor: Compositor,
    blur: BlurRenderer,
    format: wgpu::TextureFormat,
}

impl TimelineRenderer {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        Self {
            compositor: Compositor::new(device, format),
            blur: BlurRenderer::new(device, format),
            format,
        }
    }

    /// 画一帧，返回**实际画了几层**。
    ///
    /// 源解析不出来时跳过那一层而不是整帧失败：一行轨道的素材暂时没准备好，
    /// 不该让整帧变黑——预览里那表现为"闪一下"，比少一层更烦人。
    #[allow(clippy::too_many_arguments)]
    pub fn render_frame(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        target_size: (u32, u32),
        composite: &Composite,
        resolver: &mut dyn SourceResolver,
        clear: wgpu::Color,
    ) -> usize {
        // 先把每层要采的纹理备好：需要模糊的层先画进临时纹理。
        // 临时纹理与视图都要活到 compose 之后，所以放在这两个 Vec 里。
        let mut keep_alive: Vec<wgpu::Texture> = Vec::new();
        let mut prepared: Vec<(wgpu::TextureView, (u32, u32))> = Vec::new();

        for layer in &composite.layers {
            let Some((view, size)) = resolver.texture_for(&layer.source) else {
                continue;
            };
            let radius = blur_radius(&layer.effects);
            if radius == 0 {
                prepared.push((view, size));
                continue;
            }
            let blurred = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("dhampir timeline blur target"),
                size: wgpu::Extent3d {
                    width: size.0.max(1),
                    height: size.1.max(1),
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: self.format,
                usage: wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            });
            let intermediate = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("dhampir timeline blur intermediate"),
                size: wgpu::Extent3d {
                    width: size.0.max(1),
                    height: size.1.max(1),
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: self.format,
                usage: wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            });
            let blurred_view = blurred.create_view(&wgpu::TextureViewDescriptor::default());
            let intermediate_view =
                intermediate.create_view(&wgpu::TextureViewDescriptor::default());
            self.blur.blur_separable(
                device,
                queue,
                encoder,
                &view,
                &intermediate_view,
                &blurred_view,
                size,
                radius,
            );
            keep_alive.push(blurred);
            keep_alive.push(intermediate);
            prepared.push((blurred_view, size));
        }

        let draws: Vec<LayerDraw<'_>> = composite
            .layers
            .iter()
            .zip(prepared.iter())
            .map(|(layer, (view, size))| LayerDraw {
                view,
                source_size: *size,
                transform: layer.transform,
                opacity: layer.opacity,
            })
            .collect();

        self.compositor.compose(
            device,
            queue,
            encoder,
            target,
            target_size,
            &draws,
            clear,
        );

        // 让编译器和读者都看得见这些纹理活到了这里。
        drop(keep_alive);
        draws.len()
    }
}

/// 确定性源图：两端调**同一个函数**生成源像素，于是输入逐字节相同。
///
/// 它同时是"--dump-raw"思路的极简版：要隔离"渲染差异"，就必须先让**输入**相同。
/// 在高频方块上叠一层水平渐变，是为了同时压住两种错误：
/// 采样错位（方块边界会立刻暴露）与缩放/滤波偷懒（渐变会暴露）。
pub fn synthetic_source_rgba8(width: u32, height: u32, seed: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            let checker = if ((x / 8) + (y / 8) + seed) % 2 == 0 { 220 } else { 40 };
            let gradient = ((x * 255) / width.max(1)) as u8;
            let vertical = ((y * 255) / height.max(1)) as u8;
            let r = checker;
            let g = gradient;
            let b = vertical;
            let a = if (x / 16 + y / 16) % 3 == 0 { 255 } else { 200 };
            out.extend_from_slice(&[r, g, b, a]);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn effect(kind: &str, params: &[(&str, f32)]) -> Effect {
        Effect {
            kind: kind.to_string(),
            params: params.iter().map(|(key, value)| (key.to_string(), *value)).collect(),
        }
    }

    #[test]
    fn 没有特效就没有模糊() {
        assert_eq!(blur_radius(&[]), 0);
        assert_eq!(blur_radius(&[effect("sharpen", &[("radius", 8.0)])]), 0, "不认识的特效不该触发模糊");
        assert_eq!(blur_radius(&[effect("gaussian_blur", &[("sigma", 8.0)])]), 0, "没有 radius 参数就当 0");
    }

    #[test]
    fn 半径被夹到上限且四舍五入() {
        assert_eq!(blur_radius(&[effect("gaussian_blur", &[("radius", 3.0)])]), 3);
        assert_eq!(blur_radius(&[effect("gaussian_blur", &[("radius", 2.6)])]), 3);
        assert_eq!(
            blur_radius(&[effect("gaussian_blur", &[("radius", 999.0)])]),
            crate::render::BLUR_MAX_RADIUS,
            "超过上限要夹住，不能把索引交给着色器"
        );
        assert_eq!(blur_radius(&[effect("gaussian_blur", &[("radius", -5.0)])]), 0);
        assert_eq!(blur_radius(&[effect("gaussian_blur", &[("radius", f32::NAN)])]), 0, "NaN 不能穿过去");
    }

    #[test]
    fn 多个特效取最大的半径() {
        let effects = vec![
            effect("gaussian_blur", &[("radius", 2.0)]),
            effect("gaussian_blur", &[("radius", 6.0)]),
        ];
        assert_eq!(blur_radius(&effects), 6);
    }

    #[test]
    fn 源图确定且不退化() {
        let a = synthetic_source_rgba8(64, 32, 1);
        let b = synthetic_source_rgba8(64, 32, 1);
        assert_eq!(a, b, "同一个 seed 必须逐字节相同——否则双端比的就不是渲染差异");
        assert_eq!(a.len(), 64 * 32 * 4);
        assert_ne!(a, synthetic_source_rgba8(64, 32, 2), "换 seed 该变");
        let first = &a[..4];
        assert!(a.chunks(4).any(|px| px != first), "源图不能是纯色，否则比不出采样错误");
        // 透明度也要有变化：不然"合成"这件事根本没被压到
        let first_alpha = a[3];
        assert!(a.chunks(4).any(|px| px[3] != first_alpha), "源图的不透明度应当有变化");
    }

    #[test]
    fn 尺寸为零时返回空() {
        assert!(synthetic_source_rgba8(0, 0, 0).is_empty());
    }
}
