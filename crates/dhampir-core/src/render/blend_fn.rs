//! 读回型混合的渲染器：`out = f(src, dst)`（设计见 plan/web-animation-criteria.md 的 D13）。
//
//! # 它与 `Compositor` 的分工
//
//! 合成器那 4 条模式用的是**固定混合方程** —— 只靠 draw 的顺序就能表达；
//! 这里这 5 条要**读目标像素**，所以要一趟单独的 pass：两张输入纹理 + 一张输出。
//
//! 调用的形状与 `render_segmented` 里其它中转一致：调用方负责分配纹理、管生命周期。

use crate::wgpu;

/// 五条读回型模式的 WGSL。
pub const BLEND_FN_WGSL: &str = include_str!("../shaders/blend_fn.wgsl");

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BlendUniform {
    mode: f32,
    _padding: [f32; 3],
}

/// 模式 → 着色器里的模式码。**认不出来就是调用方的错**（`is_implemented` 应当先拦下）。
pub fn mode_code(mode: dhampir_timeline::layer::BlendMode) -> f32 {
    use dhampir_timeline::layer::BlendMode;
    match mode {
        BlendMode::Darken => 1.0,
        BlendMode::Lighten => 2.0,
        BlendMode::Overlay => 3.0,
        BlendMode::SoftLight => 4.0,
        BlendMode::Difference => 5.0,
        // 走固定方程的那 4 条不该到这里来。
        other => {
            debug_assert!(false, "{other:?} 走固定方程，不该进读回型回路");
            1.0
        }
    }
}

pub struct BlendFnRenderer {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
}

impl BlendFnRenderer {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("dhampir blend_fn shader"),
            source: wgpu::ShaderSource::Wgsl(BLEND_FN_WGSL.into()),
        });
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("dhampir blend_fn bind group layout"),
            entries: &[
                texture_entry(0),
                texture_entry(1),
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
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
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("dhampir blend_fn pipeline layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("dhampir blend_fn pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_fullscreen"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_blend"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    // **不混合**：这一趟的输出就是最终结果（`f(src,dst)` 已经算完了）。
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
        Self {
            pipeline,
            bind_group_layout,
        }
    }

    /// 跑一趟：`dst` 是已经画好的底、`src` 是这一层单独渲出来的像素，结果写进 `dest`。
    ///
    /// ⚠️ `too_many_arguments`：与 `compose.rs` 的 `compose` 同一处理 —— **allow，不拆结构体**。
    /// 8 个参数里前 4 个（`device` / `queue` / `encoder` / 下文三个视图）是 wgpu 的**调用形状**，
    /// 不能合并；后几个（`dst` / `src` / `mode` / `dest`）是**一张图里的三个不同角色**，
    /// 合并成结构体只会把"哪个是底、哪个是本层、画到哪"藏进字段名里，读调用点反而更绕。
    #[allow(clippy::too_many_arguments)]
    pub fn render(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        dst: &wgpu::TextureView,
        src: &wgpu::TextureView,
        mode: dhampir_timeline::layer::BlendMode,
        dest: &wgpu::TextureView,
    ) {
        let uniform = BlendUniform {
            mode: mode_code(mode),
            _padding: [0.0; 3],
        };
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("dhampir blend_fn uniform"),
            size: std::mem::size_of::<BlendUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&buffer, 0, bytemuck::bytes_of(&uniform));
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("dhampir blend_fn bind group"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(src),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(dst),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: buffer.as_entire_binding(),
                },
            ],
        });
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("dhampir blend_fn pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: dest,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    // **不清屏**：整张输出都由这一趟写满（全屏三角形 + 不混合）。
                    load: wgpu::LoadOp::Load,
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

fn texture_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: false },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}
