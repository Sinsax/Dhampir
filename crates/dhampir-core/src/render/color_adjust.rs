//! 逐像素色彩调整：亮度 / 对比度 / 饱和度 / 色调。
//!
//! # 与模糊的分工
//!
//! 模糊是**邻域**算子（要看周围像素），必须两趟、必须中间纹理；
//! 色彩调整是**逐像素**算子（只看自己），所以一趟就够、可以直接写到目标。
//! 两者在 Step 序列里是不同的步骤 —— 这个区别不是实现细节，是它们的数学性质。
//!
//! # 为什么不在这里算，而是把系数打包进 uniform
//!
//! 与高斯核同理：**一份来源**。系数在 Rust 里算好，着色器只做加法和乘法，
//! 两端不可能对同一组参数得出不同的调整强度。
//!
//! # 恒等值
//!
//! 每个参数都有"什么都不做"的值：亮度 0、对比度 1、饱和度 1、色调 0。
//! 一个工程没挂这类特效时，调用方**根本不走这条管线**（见 Step 序列的构造），
//! 所以这里的恒等值只用于"挂了但某项没填"的情形。

use bytemuck;

/// 逐像素色彩调整的着色器。
pub const COLOR_ADJUST_WGSL: &str = include_str!("../shaders/color_adjust.wgsl");

/// 传给着色器的系数。
///
/// 字段顺序与 WGSL 里的 `ColorAdjustUniform` **必须一致** ——
/// 这是裸内存布局，改一边不改另一边会静默错位。
#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct ColorAdjustParams {
    pub brightness: f32,
    pub contrast: f32,
    pub saturation: f32,
    pub hue: f32,
}

impl ColorAdjustParams {
    /// 什么都不做的那一组值。
    ///
    /// 用它跑一遍，输出应当与输入**逐像素相同**（有测试钉着）。
    pub const IDENTITY: Self = Self {
        brightness: 0.0,
        contrast: 1.0,
        saturation: 1.0,
        hue: 0.0,
    };

    /// 是否恒等。恒等时调用方可以整条跳过 —— 少一趟就少一次纹理往返。
    pub fn is_identity(&self) -> bool {
        self.brightness == 0.0
            && self.contrast == 1.0
            && self.saturation == 1.0
            && self.hue == 0.0
    }
}

/// 逐像素色彩调整管线。构造一次、每帧复用。
pub struct ColorAdjustRenderer {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    uniform: wgpu::Buffer,
}

impl ColorAdjustRenderer {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("dhampir color adjust shader"),
            source: wgpu::ShaderSource::Wgsl(COLOR_ADJUST_WGSL.into()),
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("dhampir color adjust bind group layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        // 与 blur 一致：**不用滤波采样器**，自己按整数坐标 textureLoad。
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
            label: Some("dhampir color adjust pipeline layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("dhampir color adjust pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_fullscreen"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_color_adjust"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    // 不混合：调整是算完覆盖上去，混进一个混合方程就多一个差异来源。
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
            label: Some("dhampir color adjust uniform"),
            size: std::mem::size_of::<ColorAdjustParams>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self { pipeline, bind_group_layout, uniform }
    }

    /// 跑一趟逐像素调整：`source` -> `target`。
    ///
    /// 恒等参数时**直接返回**（不写 target）：那意味着调用方可以拿 source 当结果用，
    /// 少一趟往返。调用方需要自己处理"没写 target"这个情形 —— 见 Step 序列的构造。
    pub fn apply(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        source: &wgpu::TextureView,
        target: &wgpu::TextureView,
        params: ColorAdjustParams,
    ) {
        if params.is_identity() {
            return;
        }
        queue.write_buffer(&self.uniform, 0, bytemuck::bytes_of(&params));

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("dhampir color adjust bind group"),
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
            label: Some("dhampir color adjust pass"),
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
    use dhampir_timeline::schema::{EffectPipeline, EffectSpace};

    #[test]
    fn 恒等参数被认出来() {
        // 恒等意味着调用方可以整条跳过。若这个判断错了（比如把 1.0 当恒等亮度），
        // 就会白跑一趟，或者更糟：把不该改的画面改掉。
        assert!(ColorAdjustParams::IDENTITY.is_identity());
        for mutate in [
            ColorAdjustParams { brightness: 0.01, ..ColorAdjustParams::IDENTITY },
            ColorAdjustParams { contrast: 0.99, ..ColorAdjustParams::IDENTITY },
            ColorAdjustParams { saturation: 1.01, ..ColorAdjustParams::IDENTITY },
            ColorAdjustParams { hue: 0.01, ..ColorAdjustParams::IDENTITY },
        ] {
            assert!(!mutate.is_identity(), "动了一个字段就不该再是恒等：{mutate:?}");
        }
    }

    #[test]
    fn uniform_布局与着色器一致() {
        // 裸内存布局：四个 f32，共 16 字节。
        // 改结构体而忘了改 WGSL 会**静默错位** —— 亮度会跑进对比度里，
        // 而这种错不报错、只是画面不对，最难归因。所以把大小钉住。
        assert_eq!(std::mem::size_of::<ColorAdjustParams>(), 16);
        assert_eq!(std::mem::align_of::<ColorAdjustParams>(), 4);
    }

    #[test]
    fn 着色器里没有分支也没有循环() {
        // 《WGSL 可移植性子集》禁掉带副作用的分支与循环。
        // 这条在 wgsl_subset.rs 里也有通用版本，但那是扫**全部**着色器；
        // 这里钉这一份，让"加分支"在改这个文件时立刻红。
        assert!(!COLOR_ADJUST_WGSL.contains("if ("), "色相/亮度用恒等值代替分支");
        assert!(!COLOR_ADJUST_WGSL.contains("for ("), "不要循环");
        assert!(!COLOR_ADJUST_WGSL.contains("while ("), "不要循环");
        // 采样器把滤波精度交给实现 —— 那是两端不一致的一个来源。
        assert!(!COLOR_ADJUST_WGSL.contains("sampler"), "不要采样器，用 textureLoad");
    }

    #[test]
    fn 登记表里四个色彩特效都走同一条管线() {
        // 这条钉住"四个 kind、一条管线"这个设计：加第五个色彩特效时
        // 应当只加一格登记项，**不该**再写一个渲染器。
        let kinds = ["brightness", "contrast", "saturation", "hue"];
        for kind in kinds {
            let spec = crate::effects::spec_of(kind).unwrap_or_else(|| panic!("{kind} 没登记"));
            assert_eq!(spec.pipeline, EffectPipeline::ColorAdjust, "{kind} 该走 ColorAdjust");
            assert_eq!(spec.space, EffectSpace::Source, "{kind} 是逐像素算子，不该声明 Document");
            assert!(!spec.params.is_empty(), "{kind} 没有参数范围，UI 无从生成控件");
        }
    }

    #[test]
    fn 度转弧度只做一次且范围正确() {
        // 用户填度、着色器收弧度。转换必须在**一处**发生。
        // 两边各转一遍的话，90 度会变成 90 弧度再转一次 —— 结果是随机的方向。
        let spec = crate::effects::spec_of("hue").expect("登记了 hue");
        let (name, min, max) = spec.params[0];
        assert_eq!(name, "degrees", "参数名必须说明单位是度");
        assert_eq!((min, max), (-180.0, 180.0));
    }
}
