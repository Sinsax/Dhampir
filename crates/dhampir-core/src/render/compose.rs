//! 渲染图的合成节点：把若干**层**按顺序画进目标纹理。
//!
//! # 这一层为什么值得单独写
//!
//! 预览与出片走的都是这里。所以它的行为必须是**可算清的算术**，而不是「看起来差不多」：
//! 一层的最终像素只由 (源texel, 逆变换, 不透明度, 混合方程) 决定，没有隐含状态。
//!
//! # 与求值层的关系
//!
//! crate::compose 决定「这一帧该画哪几层、每层多透」；这里只管「怎么把它们叠起来」。
//! 转场、关键帧、淡入淡出在求值层就已经归约成**每层一个不透明度**，
//! 所以这里不需要知道「转场」这个概念——差异来源越少越好。
//!
//! # 不做 discard
//!
//! 层没盖到的地方返回全透明，交给混合方程。discard 会引入「片段被丢弃」这个状态，
//! 而透明混合是纯算术——两端更容易算得一模一样。

use dhampir_timeline::schema::Transform;

pub const COMPOSE_WGSL: &str = include_str!("../shaders/compose.wgsl");

/// 传给着色器的一层参数。用 vec4 打包是为了避开 uniform 的对齐坑——
/// 看起来浪费几个字节，比「在某些驱动上偏移算错」便宜得多。
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct LayerUniform {
    inv_row0: [f32; 4],
    inv_row1: [f32; 4],
    source_size: [f32; 2],
    opacity: f32,
    _padding: f32,
}

/// 渲染空间：**文档坐标系**与**实际目标尺寸**是两件事。
///
/// # 为什么必须分开
///
/// 契约里的像素量（现在只有 transform.x/y）是以**文档坐标系**度量的，
/// 而「这一帧渲染到多大」是宿主的事：预览渲染到画布后备尺寸，出片渲染到导出尺寸。
/// 两者不等时，直接把 transform.x 当目标像素用，会让同一个工程在不同尺寸下
/// **位移的相对位置不同** —— 也就是「预览所见 != 成片所得」。
///
/// 修法是把两者显式分开，并在目标 != 文档时按比例缩放像素量。
/// **相等时比例是 1.0，那条路上的行为与从前逐字节一致** —— 出片的默认路径
/// （导出尺寸取 render_hints）就走在那条路上。
///
/// 度量归属（实核过，不是猜的）：
///   * transform.x / transform.y —— **文档像素**，按比例缩放；
///   * 每层的 gaussian_blur.radius —— **源纹理像素**（模糊跑在源尺寸的纹理上），不缩放；
///   * 调整图层的模糊半径 —— **文档像素**（模糊跑在目标尺寸的中间纹理上），要缩放。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RenderSpace {
    /// 文档坐标系（工程声明的序列分辨率）。
    pub sequence: (u32, u32),
    /// 这一帧实际渲染到的尺寸。
    pub target: (u32, u32),
}

impl RenderSpace {
    /// 文档坐标系 == 目标尺寸。
    ///
    /// **合成源、探针、语料这类「没有工程」的路径用它**：那种情况下没有文档坐标系可言，
    /// 目标尺寸就是它自己的坐标系，于是比例是 1.0，行为与引入本结构之前一致。
    pub fn square(size: (u32, u32)) -> Self {
        Self { sequence: size, target: size }
    }

    /// 目标/文档 的比例。1.0 表示两者相同。
    ///
    /// 尺寸为 0 时退化成 1.0：除零会让整帧变成 NaN 垃圾，
    /// 而尺寸为 0 本来就该由调用方拦住（这里只兜底）。
    pub fn pixel_scale(&self) -> (f32, f32) {
        let sx = if self.sequence.0 == 0 {
            1.0
        } else {
            self.target.0 as f32 / self.sequence.0 as f32
        };
        let sy = if self.sequence.1 == 0 {
            1.0
        } else {
            self.target.1 as f32 / self.sequence.1 as f32
        };
        (sx, sy)
    }

    /// 把文档像素量换算成目标像素量。
    pub fn offset(&self, transform: Transform) -> (f32, f32) {
        let (sx, sy) = self.pixel_scale();
        (transform.x * sx, transform.y * sy)
    }
}

/// 要画的一层。
pub struct LayerDraw<'a> {
    pub view: &'a wgpu::TextureView,
    pub source_size: (u32, u32),
    /// 与求值层给出来的那一个（compose::Layer::transform）。
    pub transform: Transform,
    /// 已经乘过转场权重与关键帧的**最终**不透明度。
    pub opacity: f32,
    /// 这一层怎么与下面合。**必须是已实现的模式** —— 调用方负责预筛，见 `compose`。
    pub blend: dhampir_timeline::layer::BlendMode,
}

/// 把混合模式映射成**固定的混合方程**。
///
/// 返回 `None` 表示**这个模式做不到** —— 它要算 f(src, dst)，
/// 而同一 pass 的片元着色器读不到目标纹理。要做得走 ping-pong。
///
/// **调用方必须先问再走，不许静默按 normal 画** —— 静默降级正是这个项目最要避免的。
///
/// 方程表（color 的 src/dst 因子）：
///
/// | 模式     | src            | dst                 |
/// |----------|----------------|---------------------|
/// | normal   | SrcAlpha       | OneMinusSrcAlpha    |
/// | add      | One            | One                 |
/// | multiply | Dst            | OneMinusSrcAlpha    |
/// | screen   | One            | OneMinusSrc         |
pub fn blend_state(mode: dhampir_timeline::layer::BlendMode) -> Option<wgpu::BlendState> {
    use dhampir_timeline::layer::BlendMode;
    // alpha 通道**一律**按普通叠加走。这些模式说的是「颜色怎么合」，
    // 覆盖度不该跟着变 —— 否则半透明层连不透明度都会变味。
    let alpha = wgpu::BlendComponent {
        src_factor: wgpu::BlendFactor::One,
        dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
        operation: wgpu::BlendOperation::Add,
    };
    let add = wgpu::BlendOperation::Add;
    let color = match mode {
        BlendMode::Normal => wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::SrcAlpha,
            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
            operation: add,
        },
        BlendMode::Add => wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::One,
            dst_factor: wgpu::BlendFactor::One,
            operation: add,
        },
        BlendMode::Multiply => wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::Dst,
            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
            operation: add,
        },
        BlendMode::Screen => wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::One,
            // WebGPU 的因子集里**没有** `OneMinusSrcColor`，所以用 `OneMinusSrc`。
            // 对**不透明**的源这恰好是 screen（src + dst*(1-src)）；
            // 源的 alpha < 1 时它不按覆盖度加权 —— 这是**已知的近似**，不是疏漏。
            // 两端用的是同一个方程，所以一致性不受影响；但画质上要知道这个边界。
            dst_factor: wgpu::BlendFactor::OneMinusSrc,
            operation: add,
        },
        // 以下五种需要读目标像素 —— 同一 pass 内拿不到。
        BlendMode::Darken
        | BlendMode::Lighten
        | BlendMode::Overlay
        | BlendMode::SoftLight
        | BlendMode::Difference => return None,
    };
    Some(wgpu::BlendState { color, alpha })
}

/// 一层的仿射逆变换：输出像素 -> 源像素。
///
/// 定义（写死在这里，两端照抄）：
///   正向  p_out = R(rot) * ((p_src - c_src) * scale) + c_out + offset
///   于是  p_src = R(-rot) * (p_out - c_out - offset) / scale + c_src
///
/// **入参一定是输出像素**：文档坐标系与目标尺寸不一致时，比例换算由调用方
/// 用 RenderSpace 做完（见那里的说明）。这个函数只做纯几何，不听两套单位。
pub fn inverse_affine(
    transform: Transform,
    source_size: (u32, u32),
    target_size: (u32, u32),
) -> ([f32; 4], [f32; 4]) {
    let radians = transform.rotation_deg.to_radians();
    let (sin, cos) = radians.sin_cos();
    // 缩放必须为正：契约校验已经拦过，这里再兜一次，避免除零把整帧变成垃圾。
    let scale = if transform.scale.is_finite() && transform.scale > 0.0 {
        transform.scale
    } else {
        1.0
    };

    let c_src_x = source_size.0 as f32 / 2.0;
    let c_src_y = source_size.1 as f32 / 2.0;
    let c_out_x = target_size.0 as f32 / 2.0;
    let c_out_y = target_size.1 as f32 / 2.0;

    let offset_x = c_out_x + transform.x;
    let offset_y = c_out_y + transform.y;

    let a = cos / scale;
    let b = sin / scale;
    let row0 = [a, b, -offset_x * a - offset_y * b + c_src_x, 0.0];

    let c = -sin / scale;
    let d = cos / scale;
    let row1 = [c, d, -offset_x * c - offset_y * d + c_src_y, 0.0];

    (row0, row1)
}

/// 合成管线。构造一次、每帧复用。
pub struct Compositor {
    /// **每种已实现的混合模式一条 pipeline** —— 固定混合方程是在 pipeline 里写死的，
    /// 所以没法在 draw 之间改，只能按模式各建一条、画的时候切。
    pipelines: Vec<(dhampir_timeline::layer::BlendMode, wgpu::RenderPipeline)>,
    bind_group_layout: wgpu::BindGroupLayout,
}

impl Compositor {
    pub fn new(device: &wgpu::Device, target_format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("dhampir compose shader"),
            source: wgpu::ShaderSource::Wgsl(COMPOSE_WGSL.into()),
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("dhampir compose bind group layout"),
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
            label: Some("dhampir compose pipeline layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });

        // 按模式各建一条。只遍历**已实现**的模式 —— blend_state 返回 None 的
        // 那些根本进不来（这是「做不到」在渲染侧的显式落点）。
        let mut pipelines = Vec::new();
        for mode in dhampir_timeline::layer::BlendMode::ALL
            .into_iter()
            .filter(|mode| mode.is_implemented())
        {
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("dhampir compose pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_fullscreen"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_layer"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: target_format,
                    // 方程**只在 blend_state 里写一份**，这里不重复。
                    // （用直通 alpha 而不是预乘：求值层给的不透明度是「这一层多透」，
                    //   预乘会把它算两遍 —— 这条在选择 SrcAlpha/OneMinusSrcAlpha 时定下。）
                    blend: blend_state(mode),
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

        // 这里刻意**不建共用 uniform**：见 compose() 里的说明。
        pipelines.push((mode, pipeline));
        }

        Self { pipelines, bind_group_layout }
    }

    /// 把 layers **按给定顺序**（从下往上）叠进 target。
    ///
    /// 整个列表只用一次 render pass：清屏一次、之后每层叠加。
    /// 每层一个 pass 会让 load 语义有机会出错，也慢。
    #[allow(clippy::too_many_arguments)]
    /// 这个模式的 pipeline。`None` = 未实现 —— 不该走到渲染。
    fn pipeline_for(
        &self,
        mode: dhampir_timeline::layer::BlendMode,
    ) -> Option<&wgpu::RenderPipeline> {
        self.pipelines
            .iter()
            .find(|(candidate, _)| *candidate == mode)
            .map(|(_, pipeline)| pipeline)
    }

    pub fn compose(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        space: RenderSpace,
        layers: &[LayerDraw<'_>],
        // **None = 不清屏，叠到已有内容上。**
        //
        // 分段合成要求「把第 2 段画到第 1 段已经画好的底上」，
        // 而这个函数原先**每次都会清屏** —— 直接套用会把前一段擦掉，
        // 而症状是「调整图层之前的内容不见了」，很容易被误判成纹理绑定错。
        clear: Option<wgpu::Color>,
    ) {
        // **先整帧查一遍**：有任何一个未实现的模式就一帧都不画。
        //
        // 这不是「不许静默降级」的反面，而是**最后一道防线** ——
        // 契约层（unimplemented_blends / precheck）与能力声明本该在提交前就拦住它。
        // 走到这里说明上游漏了；此时「少画几层的图」比「悄悄换了混合方程的图」安全：
        // 前者一眼看得见，后者要拿两端的字节比才能发现。
        if let Some(missing) = layers
            .iter()
            .find(|layer| self.pipeline_for(layer.blend).is_none())
        {
            debug_assert!(false, "未实现的混合模式漏到渲染层：{:?}", missing.blend);
            return;
        }

        // **每层一块 uniform**，而不是共用一块、边画边写。
        //
        // 共用一块是错的，而且错得很安静：queue.write_buffer 写的是「提交时那一块内存」，
        // 而同一个 pass 里的多次 draw 执行时读到的**都是最后那次写入的值**。
        // 实测症状：红(1.0) 上叠蓝(0.5) 出来是 (64,0,127,191) 而不是 (128,0,128,255)
        // ——两层都用上了蓝色那组的参数。这条由 tests/compose.rs 的叠加测试抓出来。
        //
        // 每帧每层建一块 48 字节的 buffer 有点浪费；正确的优化是 dynamic offset 环。
        // 但先把正确性做对：48 字节 × 层数的分配，比一个只在某些驱动上才显形的错误便宜。
        let mut bind_groups = Vec::with_capacity(layers.len());
        for layer in layers {
            // **像素量按文档坐标系度量，这里换算成目标像素。**
            // 相同尺寸时 offset 与 transform.x/y 逐位相等，所以那条路逐字节不变。
            let mut documented = layer.transform;
            let (offset_x, offset_y) = space.offset(layer.transform);
            documented.x = offset_x;
            documented.y = offset_y;
            let (row0, row1) = inverse_affine(documented, layer.source_size, space.target);
            let uniform = LayerUniform {
                inv_row0: row0,
                inv_row1: row1,
                source_size: [layer.source_size.0 as f32, layer.source_size.1 as f32],
                opacity: layer.opacity,
                _padding: 0.0,
            };
            let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("dhampir compose layer uniform"),
                size: std::mem::size_of::<LayerUniform>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            queue.write_buffer(&buffer, 0, bytemuck::bytes_of(&uniform));
            bind_groups.push(device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("dhampir compose bind group"),
                layout: &self.bind_group_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(layer.view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: buffer.as_entire_binding(),
                    },
                ],
            }));
        }

        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("dhampir compose pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: match clear {
                        Some(color) => wgpu::LoadOp::Clear(color),
                        None => wgpu::LoadOp::Load,
                    },
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        // **按层切 pipeline**：固定混合方程写在 pipeline 里，没法在 draw 之间改。
        for (layer, bind_group) in layers.iter().zip(&bind_groups) {
            let pipeline = self
                .pipeline_for(layer.blend)
                .expect("上面已经整帧查过：未实现的模式走不到这里");
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity() -> Transform {
        Transform::default()
    }

    #[test]
    fn 恒等变换把源像素映到自己() {
        let (row0, row1) = inverse_affine(identity(), (100, 50), (100, 50));
        assert!((row0[0] - 1.0).abs() < 1e-6);
        assert!(row0[1].abs() < 1e-6);
        assert!(row0[2].abs() < 1e-6, "恒等时不该有平移：{}", row0[2]);
        assert!(row1[0].abs() < 1e-6);
        assert!((row1[1] - 1.0).abs() < 1e-6);
        assert!(row1[2].abs() < 1e-6);
    }

    #[test]
    fn 中心平移把源中心映到目标中心() {
        let t = Transform { x: 10.0, y: -4.0, scale: 1.0, rotation_deg: 0.0 };
        let (row0, row1) = inverse_affine(t, (100, 50), (100, 50));
        let cx = 50.0_f32;
        let cy = 25.0_f32;
        let src_x = row0[0] * cx + row0[1] * cy + row0[2];
        let src_y = row1[0] * cx + row1[1] * cy + row1[2];
        // 目标中心画出来的是源上 (cx - 10, cy + 4) 处的内容
        assert!((src_x - 40.0).abs() < 1e-4, "得到 {src_x}");
        assert!((src_y - 29.0).abs() < 1e-4, "得到 {src_y}");
    }

    #[test]
    fn 放大两倍让源坐标落在中心四分之一() {
        let t = Transform { x: 0.0, y: 0.0, scale: 2.0, rotation_deg: 0.0 };
        let (row0, row1) = inverse_affine(t, (100, 100), (100, 100));
        let src_x = row0[2];
        let src_y = row1[2];
        // 目标左上角 (0,0) 对应源上偏离中心 50/2 = 25 像素处
        assert!((src_x - 25.0).abs() < 1e-4, "得到 {src_x}");
        assert!((src_y - 25.0).abs() < 1e-4, "得到 {src_y}");
    }

    #[test]
    fn 旋转九十度把坐标换轴() {
        let t = Transform { x: 0.0, y: 0.0, scale: 1.0, rotation_deg: 90.0 };
        let (row0, row1) = inverse_affine(t, (100, 100), (100, 100));
        assert!(row0[0].abs() < 1e-5, "得到 {}", row0[0]);
        assert!((row0[1] - 1.0).abs() < 1e-5, "得到 {}", row0[1]);
        assert!((row1[0] + 1.0).abs() < 1e-5, "得到 {}", row1[0]);
        assert!(row1[1].abs() < 1e-5, "得到 {}", row1[1]);
    }

    #[test]
    fn 非法缩放退回一比一而不是除零() {
        let t = Transform { x: 0.0, y: 0.0, scale: 0.0, rotation_deg: 0.0 };
        let (row0, _) = inverse_affine(t, (10, 10), (10, 10));
        assert!(row0.iter().all(|v| v.is_finite()), "缩放为 0 不能让矩阵变成 Inf/NaN");
    }

    #[test]
    fn 平移到画面外时源坐标也落在外面() {
        // 这一条对应着色器里的越界分支：层被推出画面后不应还画出东西。
        let t = Transform { x: 1000.0, y: 0.0, scale: 1.0, rotation_deg: 0.0 };
        let (row0, _) = inverse_affine(t, (100, 100), (100, 100));
        let src_x = row0[0] * 0.0 + row0[1] * 0.0 + row0[2];
        assert!(src_x < 0.0 || src_x >= 100.0, "目标左上角应当落在源纹理之外，得到 {src_x}");
    }

    #[test]
    fn 四种混合模式都有方程_五种没有() {
        use dhampir_timeline::layer::BlendMode;
        let normal = blend_state(BlendMode::Normal).expect("normal 必须有");
        assert_eq!(normal.color.src_factor, wgpu::BlendFactor::SrcAlpha);
        assert_eq!(normal.color.dst_factor, wgpu::BlendFactor::OneMinusSrcAlpha);

        let add = blend_state(BlendMode::Add).unwrap();
        assert_eq!(add.color.src_factor, wgpu::BlendFactor::One);
        assert_eq!(add.color.dst_factor, wgpu::BlendFactor::One);

        let multiply = blend_state(BlendMode::Multiply).unwrap();
        assert_eq!(multiply.color.src_factor, wgpu::BlendFactor::Dst);

        let screen = blend_state(BlendMode::Screen).unwrap();
        assert_eq!(screen.color.dst_factor, wgpu::BlendFactor::OneMinusSrc);

        for mode in [
            BlendMode::Darken,
            BlendMode::Lighten,
            BlendMode::Overlay,
            BlendMode::SoftLight,
            BlendMode::Difference,
        ] {
            assert!(blend_state(mode).is_none(), "需要读目标像素，不该有方程");
        }
    }

    #[test]
    fn 方程的有无必须与_is_implemented_完全一致() {
        use dhampir_timeline::layer::BlendMode;
        // 这条是本步最重要的一致性约束：「能做」有两个出处
        // （契约层的谓词、渲染器的方程表），它们一旦对不上，
        // 就会出现「说能做但画不出来」或者「画得出来但契约说不行」。
        for mode in BlendMode::ALL {
            assert_eq!(
                blend_state(mode).is_some(),
                mode.is_implemented(),
                "方程有无与 is_implemented() 不一致"
            );
        }
    }

    #[test]
    fn alpha_通道一律按普通叠加走() {
        use dhampir_timeline::layer::BlendMode;
        // 这些模式说的是「颜色怎么合」，覆盖度不该跟着变 ——
        // 否则半透明层连不透明度都会变味。
        for mode in [BlendMode::Normal, BlendMode::Add, BlendMode::Multiply, BlendMode::Screen] {
            let state = blend_state(mode).unwrap();
            assert_eq!(state.alpha.src_factor, wgpu::BlendFactor::One);
            assert_eq!(state.alpha.dst_factor, wgpu::BlendFactor::OneMinusSrcAlpha);
        }
    }

    // ---- RenderSpace：文档坐标系与目标尺寸是两件事 ----

    #[test]
    fn 相同尺寸时比例是_1_且不改变任何像素量() {
        let space = RenderSpace::square((640, 360));
        assert_eq!(space.pixel_scale(), (1.0, 1.0));
        let mut transform = Transform::default();
        transform.x = 160.0;
        transform.y = 90.0;
        assert_eq!(space.offset(transform), (160.0, 90.0));
        let same = RenderSpace { sequence: (640, 360), target: (640, 360) };
        assert_eq!(same.offset(transform), space.offset(transform));
    }

    #[test]
    fn 文档尺寸为零时兜到_1_而不是除零() {
        let space = RenderSpace { sequence: (0, 0), target: (640, 360) };
        assert_eq!(space.pixel_scale(), (1.0, 1.0));
        assert!(space.pixel_scale().0.is_finite());
    }

    /// **这条就是 D1 的判据。**
    ///
    /// 「预览所见 != 成片所得」在数学上就是：同一个工程在不同目标尺寸下，
    /// 一层的**归一化**落点不一样。这里把四种目标尺寸跑一遍，要求归一化落点完全相同，
    /// 且等于 0.5 + transform.x / sequence.x。
    #[test]
    fn 位移的归一化落点与目标尺寸无关() {
        let sequence = (640_u32, 360_u32);
        let source = (1920_u32, 1080_u32);
        let mut transform = Transform::default();
        transform.x = 160.0;
        transform.y = 90.0;

        let expected = (
            0.5 + transform.x / sequence.0 as f32,
            0.5 + transform.y / sequence.1 as f32,
        );

        let mut first: Option<(f32, f32)> = None;
        for target in [(640_u32, 360_u32), (320, 180), (1280, 720), (960, 540)] {
            let space = RenderSpace { sequence, target };
            // 与 Compositor::compose 里做的换算**同一段代码路径**：
            // 先把文档像素换成目标像素，再交给 inverse_affine 这个纯几何函数。
            let (offset_x, offset_y) = space.offset(transform);
            let mut documented = transform;
            documented.x = offset_x;
            documented.y = offset_y;
            let (row0, row1) = inverse_affine(documented, source, space.target);

            // inverse_affine 给的是「输出像素 -> 源像素」。把源中心代进去反解输出位置，
            // 就得到这一层中心实际落在输出的哪里。
            //
            // **旋转为 0 时两行各自退化成「对角元 + 常数项」**：
            // row0 的 x 系数在下标 0（cos/scale），row1 的 y 系数在下标 1（cos/scale）——
            // 写成 row1[0] 会拿到 -sin/scale == 0，除出来是 inf（这条测试自己踩过）。
            let source_centre = (source.0 as f32 / 2.0, source.1 as f32 / 2.0);
            let out_x = (source_centre.0 - row0[2]) / row0[0];
            let out_y = (source_centre.1 - row1[2]) / row1[1];
            let normalized = (out_x / target.0 as f32, out_y / target.1 as f32);

            assert!(
                (normalized.0 - expected.0).abs() < 1e-5 && (normalized.1 - expected.1).abs() < 1e-5,
                "目标 {target:?} 的归一化落点是 {normalized:?}，期望 {expected:?}"
            );
            if let Some(previous) = first {
                assert!(
                    (normalized.0 - previous.0).abs() < 1e-5
                        && (normalized.1 - previous.1).abs() < 1e-5,
                    "不同目标尺寸给出了不同的归一化落点：{previous:?} 与 {normalized:?}"
                );
            } else {
                first = Some(normalized);
            }
        }
    }

    /// 反向验证：**不换算**就会掉进「预览所见 != 成片所得」。
    /// 这个测试自己造那个错误，要求它必须被抓出来 —— 否则上面那条判据可能是恒真的。
    #[test]
    fn 不换算文档像素时归一化落点会随目标尺寸变() {
        let sequence = (640_u32, 360_u32);
        let source = (1920_u32, 1080_u32);
        let mut transform = Transform::default();
        transform.x = 160.0;

        let mut normalized = Vec::new();
        for target in [(640_u32, 360_u32), (320, 180)] {
            // 故意不换算：这就是修复前的行为。
            let space = RenderSpace { sequence, target };
            let (row0, _) = inverse_affine(transform, source, space.target);
            let out_x = (source.0 as f32 / 2.0 - row0[2]) / row0[0];
            normalized.push(out_x / target.0 as f32);
        }
        assert!(
            (normalized[0] - normalized[1]).abs() > 1e-3,
            "不换算时两种目标尺寸给出了相同的归一化落点（{normalized:?}）—— 那说明这条判据抓不到 bug"
        );
        // 而且错的正是「小的目标里位移占比更大」这个方向。
        assert!(normalized[1] > normalized[0], "640 宽的位移在 320 宽里应该占更大比例");
    }
}


