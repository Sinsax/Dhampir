//! 渲染图的第一级：把**源帧**画进 sink。
//!
//! 它是渲染图里最短的一条——一个节点，输入一张帧纹理、输出目标纹理。预览与出片共用它，
//! 区别只在 [`FrameSink`](crate::io::FrameSink) 的实现（canvas surface vs 离屏纹理）。
//!
//! 两条设计约束都来自实测，不是口味问题（见 `plan/s3.1-source-frame-sampling.md` §2.2）：
//!
//!   1. 源是**普通 texture_2d**，不是外部纹理：texture_external 没有 textureLoad 重载
//!      （实测编译失败：no matching call to textureLoad(texture_external, …)），
//!      而《WGSL 可移植性子集》禁掉了隐式 LOD 采样。
//!   2. 采样走 textureLoad（整型坐标 + mip 0）：不涉及导数，也不把「取哪个纹素」交给滤波。

pub const BLIT_WGSL: &str = include_str!("../shaders/blit.wgsl");

/// 源帧 → sink 的搬运管线。
///
/// 与 [`ProbeRenderer`](super::ProbeRenderer) 同一个形状：**构造一次、每帧复用**；
/// 每帧变的只有绑定组（源纹理换了）。
pub struct BlitRenderer {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
}

impl BlitRenderer {
    /// `target_format` 必须与 [`render`](Self::render) 传入的 sink 纹理格式一致，
    /// 否则 wgpu 会在校验时报错——格式不匹配的 bug 不该靠肉眼发现。
    pub fn new(device: &wgpu::Device, target_format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("dhampir blit shader"),
            source: wgpu::ShaderSource::Wgsl(BLIT_WGSL.into()),
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("dhampir blit bind group layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    // filterable: false —— 走 textureLoad，不需要可滤波采样器。
                    // 它同时把「这里不做滤波」写进类型：想改成 textureSample 会在这里被拦住。
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            }],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("dhampir blit pipeline layout"),
            // wgpu 30 用 `Option<&BindGroupLayout>` 表达「这个 slot 空着」，与 scene.rs 同形。
            bind_group_layouts: &[Some(&bind_group_layout)],
            // 与探针一致：0 字节。非零就得开 Features::IMMEDIATES，那会踩到「两端能力不一致」。
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("dhampir blit pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_fullscreen"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                // 全屏三角形的三个顶点写在着色器里，没有顶点缓冲。
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_source"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: target_format,
                    // 不混合：这是搬运，不是合成。混进来一个混合方程就多一个差异来源。
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

    /// 把 `source` 画进 `sink`。
    ///
    /// 两者尺寸可以不同，但**只有尺寸相同时才是恒等搬运**——放大/缩小会按目标像素中心
    /// 去取最近纹素（textureLoad 不做插值）。缩放策略是 M4 的事，这里不预设。
    pub fn render(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        source: &wgpu::TextureView,
        sink: &wgpu::TextureView,
    ) {
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("dhampir blit bind group"),
            layout: &self.bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(source),
            }],
        });

        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("dhampir blit pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: sink,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    // 全屏覆盖，清不清都无所谓；显式清掉，免得「目标里残留什么」影响判定。
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
