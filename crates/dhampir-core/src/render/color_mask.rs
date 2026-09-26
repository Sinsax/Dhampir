//! 常量色叠加：闪白 / 暗角 / 噪声 / 覆盖层（ColorMask 管线）。
//!
//! # 与 color_adjust 的分工
//!
//! 两者都是**逐像素**算子，但语义不同：
//! - `color_adjust`：**重新映射现有像素**（同一个像素进去、改动过的同一个像素出来）；
//! - `color_mask`：**引入一个与输入无关的颜色分量**（闪烁、暗角、噪声、渐变）。
//!
//! 分两条管线而不是合并，是因为它们的 uniform 完全不同 ——
//! 合并会让"这条特效需要哪些参数"变成运行时的约定，而不是类型上的事实。
//!
//! # 四合一而不是四个着色器
//!
//! 理由是少走纹理往返：用户同时挂闪白与暗角时，四个独立着色器要四趟。
//! 每项的强度为 0 时**整条链退化成原样**（有测试钉着逐像素恒等）。
//!
//! # 噪声必须是帧号的纯函数
//!
//! 见 `shaders/color_mask.wgsl` 的哈希函数注释：用 `fract(sin(..))` 那类
//! 依赖浮点精度的写法，两个编译器会对同一输入给出不同的最后几位，
//! 于是"预览跳到这一帧"与"成片播到这一帧"噪声不同 —— 而那正是最难查的差异。

use bytemuck;

/// ColorMask 的着色器。
pub const COLOR_MASK_WGSL: &str = include_str!("../shaders/color_mask.wgsl");

/// 四种 ColorMask 特效求值后的系数，打包给着色器。
///
/// 字段顺序与 WGSL 里的 `ColorMaskUniform` **必须一致** ——
/// 这是裸内存布局，改一边不改另一边会静默错位。
///
/// # 为什么把四项合进一个 struct 而不是四项各跑一趟
///
/// 一次 `write_buffer` 写全部：四个独立 uniform 会让"这一帧到底写了哪个"
/// 变成一个可能忘记的状态。合起来就没有"忘写"这回事。
#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct ColorMaskParams {
    pub flash_amount: f32,
    pub flash_r: f32,
    pub flash_g: f32,
    pub flash_b: f32,
    pub vignette_amount: f32,
    pub vignette_radius: f32,
    pub vignette_softness: f32,
    pub noise_amount: f32,
    pub noise_seed: f32,
    pub overlay_amount: f32,
    pub overlay_r: f32,
    pub overlay_g: f32,
    pub overlay_b: f32,
    pub overlay_r2: f32,
    pub overlay_g2: f32,
    pub overlay_b2: f32,
    pub overlay_shape: f32,
    pub overlay_angle: f32,
    pub width: f32,
    pub height: f32,
    pub frame: f32,
}

impl ColorMaskParams {
    /// 什么都不做的那一组值：四项强度全 0。
    ///
    /// 用它跑一遍，输出应当与输入**逐像素相同**（有测试钉着）。
    pub const IDENTITY: Self = Self {
        flash_amount: 0.0,
        flash_r: 0.0,
        flash_g: 0.0,
        flash_b: 0.0,
        vignette_amount: 0.0,
        // 半径 0 + 过渡 1 是中性值：amount 为 0 时它根本不参与结果。
        vignette_radius: 0.0,
        vignette_softness: 1.0,
        noise_amount: 0.0,
        noise_seed: 0.0,
        overlay_amount: 0.0,
        overlay_r: 0.0,
        overlay_g: 0.0,
        overlay_b: 0.0,
        overlay_r2: 0.0,
        overlay_g2: 0.0,
        overlay_b2: 0.0,
        overlay_shape: 0.0,
        overlay_angle: 0.0,
        width: 1.0,
        height: 1.0,
        frame: 0.0,
    };

    /// 是否恒等。恒等时调用方可以整条跳过 —— 少一趟就少一次纹理往返。
    pub fn is_identity(&self) -> bool {
        self.flash_amount == 0.0
            && self.vignette_amount == 0.0
            && self.noise_amount == 0.0
            && self.overlay_amount == 0.0
    }
}

/// 求值后的 `shape` 取值，与 WGSL 里的数值约定一致。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayShape {
    Solid = 0,
    Linear = 1,
    Radial = 2,
}

impl OverlayShape {
    /// 从参数值解析。**认不出来就是 Solid**（最保守：只铺一个颜色，
    /// 不会因为一个坏数值画出乱七八糟的渐变）。
    pub fn from_param(value: f32) -> Self {
        let rounded = value.round();
        if (rounded - 1.0).abs() < f32::EPSILON {
            Self::Linear
        } else if (rounded - 2.0).abs() < f32::EPSILON {
            Self::Radial
        } else {
            Self::Solid
        }
    }
}

/// ColorMask 管线。构造一次、每帧复用。
pub struct ColorMaskRenderer {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    uniform: wgpu::Buffer,
}

impl ColorMaskRenderer {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("dhampir color mask shader"),
            source: wgpu::ShaderSource::Wgsl(COLOR_MASK_WGSL.into()),
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("dhampir color mask bind group layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        // 与 color_adjust / blur 一致：**不用滤波采样器**。
                        // 滤波精度允许实现降精度，那是"两端不一致"的一个来源。
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("dhampir color mask pipeline layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("dhampir color mask pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_fullscreen"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_color_mask"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    // 不混合：算完覆盖上去。混进一个混合方程就多一个差异来源。
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("dhampir color mask uniform"),
            size: std::mem::size_of::<ColorMaskParams>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self { pipeline, bind_group_layout, uniform }
    }

    /// 跑一趟常量色叠加：`source` -> `target`。
    ///
    /// 恒等参数时**直接返回**（不写 target）：调用方可以拿 source 当结果用。
    pub fn apply(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        source: &wgpu::TextureView,
        target: &wgpu::TextureView,
        params: ColorMaskParams,
    ) {
        if params.is_identity() {
            return;
        }
        queue.write_buffer(&self.uniform, 0, bytemuck::bytes_of(&params));

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("dhampir color mask bind group"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(source),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: self.uniform.as_entire_binding(),
                },
            ],
        });

        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("dhampir color mask pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.draw(0..3, 0..1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 恒等参数不写目标() {
        assert!(ColorMaskParams::IDENTITY.is_identity());
    }

    #[test]
    fn 任何一项非零就不再恒等() {
        // 逐项验证：漏掉某一项的判据会让"挂了这个特效却没画"变成静默行为。
        for mutate in [
            (|p: &mut ColorMaskParams| p.flash_amount = 0.5) as fn(&mut ColorMaskParams),
            |p: &mut ColorMaskParams| p.vignette_amount = 0.5,
            |p: &mut ColorMaskParams| p.noise_amount = 0.5,
            |p: &mut ColorMaskParams| p.overlay_amount = 0.5,
        ] {
            let mut params = ColorMaskParams::IDENTITY;
            mutate(&mut params);
            assert!(!params.is_identity(), "改了强度却仍被判为恒等");
        }
    }

    #[test]
    fn shape_解析认不出的落回纯色() {
        assert_eq!(OverlayShape::from_param(0.0), OverlayShape::Solid);
        assert_eq!(OverlayShape::from_param(1.0), OverlayShape::Linear);
        assert_eq!(OverlayShape::from_param(2.0), OverlayShape::Radial);
        // 坏数值不该画出"意外形状"：落回最保守的纯色。
        assert_eq!(OverlayShape::from_param(-5.0), OverlayShape::Solid);
        assert_eq!(OverlayShape::from_param(99.0), OverlayShape::Solid);
        assert_eq!(OverlayShape::from_param(f32::NAN), OverlayShape::Solid);
    }

    #[test]
    fn 着色器在可移植子集里() {
        // 与其它着色器同一道闸：禁词一个都不许出现。
        let code = crate::render::wgsl_subset::check_portable_subset("color_mask.wgsl", COLOR_MASK_WGSL);
        assert!(code.contains("fs_color_mask"), "片元入口不见了");
        assert!(code.contains("vs_fullscreen"), "顶点入口不见了");
        // **不许有分支**：这条管线靠"强度 0 = 恒等"代替分支。
        assert!(!code.contains("if ("), "ColorMask 不许出现分支");
    }

    #[test]
    fn uniform_布局与着色器一致() {
        // 字段顺序是裸内存契约：改了 Rust 结构却忘了同步 WGSL 会**静默错位**
        // （闪白的强度传成了半径之类），而画面只是"看着不对"。
        // 这里按声明顺序核对 WGSL 里的字段名，两边少一个都会红。
        let expected = [
            "flash_amount",
            "flash_r",
            "flash_g",
            "flash_b",
            "vignette_amount",
            "vignette_radius",
            "vignette_softness",
            "noise_amount",
            "noise_seed",
            "overlay_amount",
            "overlay_r",
            "overlay_g",
            "overlay_b",
            "overlay_r2",
            "overlay_g2",
            "overlay_b2",
            "overlay_shape",
            "overlay_angle",
            "width",
            "height",
            "frame",
        ];
        // 逐个按顺序找，确保**次序**也对（只查存在性抓不住错位）。
        let mut cursor = 0usize;
        for field in expected {
            let at = COLOR_MASK_WGSL[cursor..]
                .find(field)
                .unwrap_or_else(|| panic!("WGSL 里找不到 {field}，或它的次序与 Rust 结构不一致"));
            cursor += at + field.len();
        }
    }
}
