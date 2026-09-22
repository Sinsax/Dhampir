//! 两趟可分离高斯模糊。
//!
//! # 为什么权重在 Rust 这边算
//!
//! 高斯核需要 exp()。放在着色器里算会带来两件事：一是要在允许表里申报新构造，
//! 二是**两端各算一遍核**——而核只要差一点点，模糊结果就差一点点，
//! 这种差异最后会混进「渲染不一致」里，非常难归因。
//! 所以权重在这里算一次、打包进 uniform：**核只有一份来源**。
//!
//! # 为什么抽头是定长展开
//!
//! 子集禁掉循环（累加顺序必须由文本决定）。所以抽头数写死为 `TAPS`，
//! 超出实际 radius 的那部分权重为 0——既不改长度，也不需要重新归一。

pub const BLUR_WGSL: &str = include_str!("../shaders/blur.wgsl");

/// 单边最大半径。抽头数 = 2 * MAX_RADIUS + 1。
///
/// 这个数不是随便定的：它同时是**着色器里展开的行数**与**特效登记表里 radius 的上界**。
/// 改它要同时改 plan/wgsl-portable-subset.md 的现值（普查会数）。
pub const MAX_RADIUS: u32 = 16;

/// 单方向抽头数。
pub const TAPS: usize = (MAX_RADIUS as usize) * 2 + 1;

/// 权重打包成几个 vec4。
const WEIGHT_VECS: usize = TAPS.div_ceil(4);

/// 一维高斯权重，已归一化。
///
/// **长度恒为 TAPS**：超出 radius 的位置是 0。着色器是定长展开的，不能有可变长度。
/// sigma 取 radius / 2 —— 也就是核覆盖 ±2σ，是常见取法；radius 为 0 时退化成恒等核。
pub fn gaussian_weights_1d(radius: u32) -> [f32; TAPS] {
    let mut out = [0.0_f32; TAPS];
    let center = MAX_RADIUS as usize;
    if radius == 0 {
        out[center] = 1.0;
        return out;
    }
    let radius = radius.min(MAX_RADIUS);
    let sigma = (radius as f32) / 2.0;
    let two_sigma_squared = 2.0 * sigma * sigma;
    let mut sum = 0.0_f32;
    for offset in 0..=(radius as usize) {
        let x = offset as f32;
        let weight = (-x * x / two_sigma_squared).exp();
        out[center + offset] = weight;
        out[center - offset] = weight;
        // 中心只加一次
        sum += if offset == 0 { weight } else { weight * 2.0 };
    }
    for value in out.iter_mut() {
        *value /= sum;
    }
    out
}

/// 传给着色器的模糊参数。
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BlurUniform {
    weights: [[f32; 4]; WEIGHT_VECS],
    direction: [f32; 2],
    size: [f32; 2],
}

/// 两趟可分离模糊管线。构造一次、每帧复用。
pub struct BlurRenderer {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    // **两趟各一块 uniform**，不能共用。见 blur_separable 的说明。
    uniform_horizontal: wgpu::Buffer,
    uniform_vertical: wgpu::Buffer,
}

impl BlurRenderer {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("dhampir blur shader"),
            source: wgpu::ShaderSource::Wgsl(BLUR_WGSL.into()),
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("dhampir blur bind group layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
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
            label: Some("dhampir blur pipeline layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("dhampir blur pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_fullscreen"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_blur"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: format,
                    // 不混合：模糊是把它自己算完覆盖上去。混进一个混合方程就多一个差异来源。
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

        let uniform_size = std::mem::size_of::<BlurUniform>() as u64;
        let make_uniform = |label: &str| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: uniform_size,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        };
        let uniform_horizontal = make_uniform("dhampir blur uniform (horizontal)");
        let uniform_vertical = make_uniform("dhampir blur uniform (vertical)");

        Self {
            pipeline,
            bind_group_layout,
            uniform_horizontal,
            uniform_vertical,
        }
    }

    /// 横一趟、竖一趟。`intermediate` 由调用方提供（显存池策略归宿主，不归这里）。
    ///
    /// **两趟必须各用一块 uniform。** 这里原先错了，而且注释还写反了：
    /// 两趟在**同一个 command encoder** 里，两次 write_buffer 都发生在提交之前，
    /// 于是执行时**两趟读到的都是最后一次写入**（方向都是纵向）。
    /// 症状很隐蔽：纯白图照样是纯白（归一化的模糊对均匀图不变），
    /// 只有「孤立亮点该被摊到邻点」那条测试才把它抓出来。
    /// 与 compose.rs 里那个共用 uniform 的 bug 是同一类。
    #[allow(clippy::too_many_arguments)]
    pub fn blur_separable(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        source: &wgpu::TextureView,
        intermediate: &wgpu::TextureView,
        target: &wgpu::TextureView,
        size: (u32, u32),
        radius: u32,
    ) {
        if radius == 0 {
            return;
        }
        let weights = gaussian_weights_1d(radius);
        let mut packed = [[0.0_f32; 4]; WEIGHT_VECS];
        for (index, value) in weights.iter().enumerate() {
            packed[index / 4][index % 4] = *value;
        }

        let horizontal = BlurUniform {
            weights: packed,
            direction: [1.0, 0.0],
            size: [size.0 as f32, size.1 as f32],
        };
        queue.write_buffer(&self.uniform_horizontal, 0, bytemuck::bytes_of(&horizontal));
        self.one_pass(device, encoder, source, intermediate, &self.uniform_horizontal, "dhampir blur pass (horizontal)");

        let vertical = BlurUniform {
            weights: packed,
            direction: [0.0, 1.0],
            size: [size.0 as f32, size.1 as f32],
        };
        queue.write_buffer(&self.uniform_vertical, 0, bytemuck::bytes_of(&vertical));
        self.one_pass(device, encoder, intermediate, target, &self.uniform_vertical, "dhampir blur pass (vertical)");
    }

    fn one_pass(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        source: &wgpu::TextureView,
        target: &wgpu::TextureView,
        uniform: &wgpu::Buffer,
        label: &str,
    ) {
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("dhampir blur bind group"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(source),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: uniform.as_entire_binding(),
                },
            ],
        });
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some(label),
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

    fn sum(weights: &[f32; TAPS]) -> f32 {
        weights.iter().sum()
    }

    #[test]
    fn 权重归一() {
        for radius in [0, 1, 2, 5, 16] {
            let weights = gaussian_weights_1d(radius);
            assert!((sum(&weights) - 1.0).abs() < 1e-5, "radius {radius} 的权重和是 {}", sum(&weights));
        }
    }

    #[test]
    fn 权重对称且中心最大() {
        let weights = gaussian_weights_1d(8);
        let center = MAX_RADIUS as usize;
        for offset in 1..=8 {
            assert!(
                (weights[center + offset] - weights[center - offset]).abs() < 1e-6,
                "偏移 {offset} 处不对称"
            );
            assert!(weights[center - offset] < weights[center], "离中心越远应当越小");
        }
    }

    #[test]
    fn 半径为零是恒等核() {
        let weights = gaussian_weights_1d(0);
        let center = MAX_RADIUS as usize;
        assert_eq!(weights[center], 1.0);
        assert_eq!(sum(&weights), 1.0);
        for (index, value) in weights.iter().enumerate() {
            if index != center {
                assert_eq!(*value, 0.0, "第 {index} 项应当是 0");
            }
        }
    }

#[test]
    fn 超出半径的抽头权重为零() {
        // 着色器是**定长展开**的：长度永远是 TAPS，靠 0 权重把多余抽头掩掉。
        // 这条测试钉住的就是那个约定——长度变了着色器就崩了。
        let radius = 3_u32;
        let weights = gaussian_weights_1d(radius);
        let center = MAX_RADIUS as usize;
        for offset in (radius as usize + 1)..=center {
            assert_eq!(weights[center + offset], 0.0, "偏移 {offset} 应当在半径外");
            assert_eq!(weights[center - offset], 0.0);
        }
        assert!(weights[center + radius as usize] > 0.0, "半径上的抽头应当非零");
    }

    #[test]
    fn 半径超过上限被夹住而不是越界() {
        // 契约层已经把 radius 限在 0..=MAX_RADIUS，这里再兜一次：
        // 万一有调用方直接传大数，应当是「夹住」而不是数组越界 panic。
        let weights = gaussian_weights_1d(999);
        assert!((sum(&weights) - 1.0).abs() < 1e-5);
        assert!(weights.iter().all(|v| v.is_finite()));
    }

    #[test]
    fn 抽头数与打包数自洽() {
        // 着色器里写的是 array<vec4<f32>, WEIGHT_VECS>，多一个少一个都会编译失败。
        assert_eq!(TAPS, 2 * MAX_RADIUS as usize + 1);
        assert_eq!(WEIGHT_VECS, TAPS.div_ceil(4));
        assert!(WEIGHT_VECS * 4 >= TAPS, "打包不能丢分量");
    }
}

