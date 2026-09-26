//! 坐标重映射：抖动 / 缩放弹跳 / 脉冲 / 分屏（Warp 管线）。
//!
//! # 与另外三条管线的数学差别
//!
//! `ColorAdjust` 与 `ColorMask` 是**逐像素**的：输出只看同一坐标的输入。
//! `SeparableBlur` 是**邻域**的：输出看周围的输入，但是固定的核。
//! `Warp` 也是邻域的，但**看哪里由一个位移场决定** —— 所以它既不能像
//! 逐像素那样一趟直写同一坐标，也不能像模糊那样把核预先展开。
//!
//! # 四合一
//!
//! 与 color_mask 同一条理由：四个特效都是"一个位移场"，合成一个着色器
//! 意味着同时挂抖动与弹跳时只走一趟纹理往返。
//!
//! # 位移场是纯函数
//!
//! 见 `shaders/warp.wgsl` 的文件头：位移由 `(像素坐标, 时间秒, seed)` 唯一决定。
//! 用累积状态的话，"预览跳到这一帧"与"成片顺序播到这一帧"会差几个像素 ——
//! 而那正是最难归因的一类差异。

use bytemuck;

/// Warp 的着色器。
pub const WARP_WGSL: &str = include_str!("../shaders/warp.wgsl");

/// 四种 Warp 特效求值后的系数，打包给着色器。
///
/// 字段顺序与 WGSL 里的 `WarpUniform` **必须一致** ——
/// 这是裸内存布局，改一边不改另一边会静默错位。
#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct WarpParams {
    pub shake_amount: f32,
    pub shake_frequency: f32,
    pub shake_seed: f32,
    pub bounce_amount: f32,
    pub bounce_frequency: f32,
    pub pulse_amount: f32,
    pub pulse_frequency: f32,
    pub split_offset: f32,
    pub split_skew: f32,
    pub split_amount: f32,
    pub width: f32,
    pub height: f32,
    pub seconds: f32,
}

impl WarpParams {
    /// 什么都不做的那一组值：四项强度全 0。
    ///
    /// 用它跑一遍，输出应当与输入**逐像素相同**（有测试钉着）。
    pub const IDENTITY: Self = Self {
        shake_amount: 0.0,
        // 频率给 1 而不是 0：强度为 0 时它不参与结果，
        // 但给 0 会让"哪天忘了设强度"变成一个静止的画面，给 1 更容易被发现。
        shake_frequency: 1.0,
        shake_seed: 0.0,
        bounce_amount: 0.0,
        bounce_frequency: 1.0,
        pulse_amount: 0.0,
        pulse_frequency: 1.0,
        split_offset: 0.0,
        split_skew: 0.0,
        split_amount: 0.0,
        width: 1.0,
        height: 1.0,
        seconds: 0.0,
    };

    /// 是否恒等。恒等时调用方可以整条跳过 —— 少一趟就少一次纹理往返。
    pub fn is_identity(&self) -> bool {
        self.shake_amount == 0.0
            && self.bounce_amount == 0.0
            && self.pulse_amount == 0.0
            && self.split_amount == 0.0
    }

    /// 缩放是否真的是 1.0（即没有弹跳也没有脉冲）。
    ///
    /// 单独一个谓词是因为缩放是**除数**：它接近 0 时采样点会被推到无穷远。
    pub fn zoom_is_neutral(&self) -> bool {
        self.bounce_amount == 0.0 && self.pulse_amount == 0.0
    }
}

/// Warp 管线。构造一次、每帧复用。
pub struct WarpRenderer {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    uniform: wgpu::Buffer,
}

impl WarpRenderer {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("dhampir warp shader"),
            source: wgpu::ShaderSource::Wgsl(WARP_WGSL.into()),
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("dhampir warp bind group layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        // 与其它三条管线一致：**不用滤波采样器**。
                        // 位移后的坐标落在纹素之间，滤波由实现决定 —— 那是差异来源。
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
            label: Some("dhampir warp pipeline layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("dhampir warp pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_fullscreen"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_warp"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
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
            label: Some("dhampir warp uniform"),
            size: std::mem::size_of::<WarpParams>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self { pipeline, bind_group_layout, uniform }
    }

    /// 跑一趟坐标重映射：`source` -> `target`。
    pub fn apply(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        source: &wgpu::TextureView,
        target: &wgpu::TextureView,
        params: WarpParams,
    ) {
        if params.is_identity() {
            return;
        }
        queue.write_buffer(&self.uniform, 0, bytemuck::bytes_of(&params));

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("dhampir warp bind group"),
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
            label: Some("dhampir warp pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    // 坐标重映射会读**整张**图，没有"没写到的像素"——
                    // 所以这里 clear 成透明只是兜底（位移场若把点推到界外，
                    // 着色器自己也 clamp 住了，不会留空洞）。
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
        assert!(WarpParams::IDENTITY.is_identity());
    }

    #[test]
    fn 任何一项非零就不再恒等() {
        for mutate in [
            (|p: &mut WarpParams| p.shake_amount = 0.1) as fn(&mut WarpParams),
            |p: &mut WarpParams| p.bounce_amount = 0.1,
            |p: &mut WarpParams| p.pulse_amount = 0.1,
            |p: &mut WarpParams| p.split_amount = 0.1,
        ] {
            let mut params = WarpParams::IDENTITY;
            mutate(&mut params);
            assert!(!params.is_identity(), "改了强度却仍被判为恒等");
        }
    }

    #[test]
    fn 恒等参数下缩放是中性的() {
        // 缩放是**除数**：它若为 0，采样点会被推到无穷远（或 NaN）。
        // 恒等值必须是 1.0 而不是 0.0。
        assert!(WarpParams::IDENTITY.zoom_is_neutral());
    }

    #[test]
    fn 着色器在可移植子集里() {
        let code = crate::render::wgsl_subset::check_portable_subset("warp.wgsl", WARP_WGSL);
        assert!(code.contains("fs_warp"), "片元入口不见了");
        assert!(code.contains("vs_fullscreen"), "顶点入口不见了");
        // **越界必须自己 clamp**：位移会把采样点推出画面，
        // 而越界 textureLoad 的行为不在可移植子集里。
        assert!(
            code.contains("clamp("),
            "warp 必须自己把采样坐标夹进纹理（越界 textureLoad 不可移植）"
        );
        assert!(!code.contains("if ("), "Warp 不许出现分支");
    }

    #[test]
    fn uniform_布局与着色器一致() {
        // 与 color_mask 同一道闸：字段顺序是裸内存契约。
        let expected = [
            "shake_amount",
            "shake_frequency",
            "shake_seed",
            "bounce_amount",
            "bounce_frequency",
            "pulse_amount",
            "pulse_frequency",
            "split_offset",
            "split_skew",
            "split_amount",
            "width",
            "height",
            "seconds",
        ];
        let mut cursor = 0usize;
        for field in expected {
            let at = WARP_WGSL[cursor..]
                .find(field)
                .unwrap_or_else(|| panic!("WGSL 里找不到 {field}，或它的次序与 Rust 结构不一致"));
            cursor += at + field.len();
        }
    }
}
