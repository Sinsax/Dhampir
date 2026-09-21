//! M0 探针图：一个三色三角形。
//!
//! # 为什么是三角形而不是纯色清屏
//!
//! 清屏只测到"command buffer 提交成功了"，测不到光栅化。而 M2 的双运行时比对
//! 里最先出问题、最难归因的正是**光栅化的边界行为**。三角形覆盖：顶点变换、
//! 图元装配、光栅化、属性插值、输出编码（linear → sRGB）。这几步也是特效管线
//! 将来会踩的全部地基，先用最小实现把它们跑通。
//!
//! # 坐标是刻意挑的，不要"顺手改整齐"
//!
//! 顶点坐标不能落在像素中心上，也不能正好落在两个像素的正中间：
//!
//! - 落在**像素中心**上：该点是否覆盖由 `<` 还是 `<=` 决定，是实现自由
//! - 落在**正中间**：tie-breaking（top-left rule）决定归属，也是实现自由
//!
//! 两种情况下，M2 的比对都会冒出"结构性差异"——边缘整列像素错位。而那是**假阳性**：
//! 不是两端渲染不一致，是测试用例自己没避开规范留白。查这种问题非常贵，
//! 所以坐标从一开始就选在离像素中心与正中间都足够远的地方。
//!
//! 具体数值见 [`PROBE_VERTICES`] 的注释。

use bytemuck::{Pod, Zeroable};
use wgpu::util::DeviceExt as _;

use crate::wgpu;

/// 探针渲染的目标尺寸。
///
/// 两个宿主必须用**同一个**尺寸：顶点坐标在 NDC 里，但"哪个像素被覆盖"依赖
/// 分辨率。分辨率不同，同一个三角形覆盖的像素也不同——那样 M2 就没法比对。
pub const PROBE_TARGET_SIZE: (u32, u32) = (256, 256);

/// 探针的清屏色，**线性光**（渲染目标的格式是 `Rgba8UnormSrgb`，
/// 硬件的 sRGB 编码在这里自动发生）。
///
/// 0.05 是个不满不空的值：既不是 0（分不清"渲染成功但全黑"和"没渲染"），
/// 也不是 0.5（那样三角形插值出的颜色和背景对比不够，肉眼也看不出问题）。
pub const PROBE_CLEAR_COLOR: wgpu::Color = wgpu::Color {
    r: 0.05,
    g: 0.05,
    b: 0.05,
    a: 1.0,
};

/// 探针顶点：位置（NDC）+ 颜色（线性光）。
///
/// `repr(C)` + `Pod`：这块数据要直接当 vertex buffer 用，中间不能有 padding。
/// `[f32; 2]` 8 字节 + `[f32; 3]` 12 字节 = 20 字节，4 字节对齐，无填充。
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct ProbeVertex {
    pub position: [f32; 2],
    pub color: [f32; 3],
}

impl ProbeVertex {
    /// 顶点在 buffer 里的字节跨度。20 字节——是 4 的倍数，合法。
    pub const STRIDE: wgpu::BufferAddress = size_of::<Self>() as wgpu::BufferAddress;
}

/// 三个顶点。位置经过设计，避开像素中心与中点（见模块文档）。
///
/// 换算关系先写清楚，因为下面这张表就是用这两个式子算出来的（测试里也是同一份）：
/// 像素索引 `p` 占的窗口坐标是 `[p, p + 1]`，中心在 `p + 0.5`；NDC 与窗口坐标的
/// 关系是 `window = (ndc + 1) * 128`（`w = h = 256`），于是
///
/// ```text
/// p = (ndc + 1) * 128 - 0.5
/// ```
///
/// 坐标是这么挑的：先把想要的像素索引定在**四分位**上——离像素中心（`.0`）
/// 和两像素正中间（`.5`）都约 0.25 像素。0.25 是这两条约束能做到的**最好值**
/// （`min(frac, 1-frac, |frac-0.5|)` 的上界），所以不是"随便挑远了点"：
///
/// | 顶点 | x → 像素索引 | y → 像素索引 |
/// |---|---|---|
/// | 0 | 128.2552 | 40.7544 |
/// | 1 | 48.2552 | 180.2488 |
/// | 2 | 208.2552 | 180.2488 |
///
/// 六个坐标距中心与正中间都不少于 **0.2448** 像素（测试守着 0.2 这条线）。
///
/// 底边**是水平的**（顶点 1 与 2 的 y 相同）。这不是为了好看：一条近乎水平的边
/// 会以极小的斜率扫过整行像素，在某些列上离像素角只有千分之几像素，那片薄片
/// 归谁由 fill rule 决定——正是两端会分叉的地方。水平边的归属是**整行一致**的，
/// 规避得最干净。等腰也只是"挑四方"的副产品，对称本身不构成问题。
///
/// 颜色是**纯**红绿蓝（线性）。纯色有两个好处：肉眼就能看出插值对不对；
/// 而 sRGB 编码后正好是 255，任何精度损失都会立刻跌破 254，瞒不住。
pub const PROBE_VERTICES: [ProbeVertex; 3] = [
    ProbeVertex {
        position: [0.0059, 0.6777],
        color: [1.0, 0.0, 0.0],
    },
    ProbeVertex {
        position: [-0.6191, -0.4121],
        color: [0.0, 1.0, 0.0],
    },
    ProbeVertex {
        position: [0.6309, -0.4121],
        color: [0.0, 0.0, 1.0],
    },
];

impl ProbeVertex {
    /// 顶点在**像素栅格**里的位置（图像坐标：原点左上、`y` 向下）。
    ///
    /// 换算关系（`w`/`h` 是目标尺寸）：
    ///
    /// ```text
    /// 窗口坐标 x = (ndc_x + 1) * 0.5 * w
    /// 窗口坐标 y = (1 - ndc_y) * 0.5 * h
    /// ```
    ///
    /// 返回的是**像素索引**而不是窗口坐标：像素 `p` 覆盖窗口区间 `[p, p + 1]`，
    /// 所以索引 = 窗口坐标 - 0.5（它的中心正好落在 `p + 0.5`）。
    ///
    /// 用 `f64` 算：这几个数要参与"离边界多远"的判断，`f32` 的精度不够，
    /// 而"差一点点"正好会让边界归属变成实现自由——那正是要避免的东西。
    pub fn pixel_position(&self, size: (u32, u32)) -> [f64; 2] {
        let (w, h) = (f64::from(size.0), f64::from(size.1));
        [
            (f64::from(self.position[0]) + 1.0) * 0.5 * w - 0.5,
            (1.0 - f64::from(self.position[1])) * 0.5 * h - 0.5,
        ]
    }
}

/// 顶点内侧的采样点从顶点朝重心走多少比例。
///
/// 0.10 是权衡出来的：往外靠，颜色更纯、断言更硬，但离三角形边界更近；
/// 往里靠，边界余量更大，但另外两个通道混进来的颜色更多。这个比例下顶点
/// 自己的重心权重是 `1 - 2/3 * 0.10 ≈ 0.933`，于是 sRGB 8 位下主导通道约
/// 247、另两个约 51——主导关系有 190 以上的余量，同时离最近边界还有约 4.6 像素。
pub const VERTEX_INSET: f64 = 0.10;

/// 采样点应当表现出的性质。
///
/// **断言与记录共用这一份声明。** 两边各写一份的话，就会出现"记录下来的点"
/// 和"断言的点"悄悄不是同一个点——而那正是本文件先前踩过的坑（见
/// [`probe_samples`] 的文档）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SampleExpectation {
    /// 清屏色：中性灰。
    Background,
    /// 某个颜色通道必须占优。`0` = 红、`1` = 绿、`2` = 蓝。
    Dominant(u8),
}

/// 一个采样点。
#[derive(Clone, Copy, Debug)]
pub struct ProbeSample {
    /// 稳定的名字。它会出现在 `run.json` 里被长期留存、跨版本比较，**改名等于
    /// 让历史记录失去可比性**。
    pub name: &'static str,
    /// 像素坐标（图像坐标：原点左上、`y` 向下）。
    pub x: u32,
    pub y: u32,
    /// 这个点上必须成立的性质。
    pub expect: SampleExpectation,
}

/// 探针的采样点集合，由目标尺寸算出。
///
/// # 为什么坐标必须由 [`PROBE_VERTICES`] 推出来
///
/// 这里原先手写过一组**与顶点无关**的比例（`(w/2)+2, h/6`、`w/4, 6h/7` 之类）。
/// 后果不是"差一点"，而是三个"顶点附近"的点**全部落在三角形外面**：
/// 于是 native 侧的断言报的是"顶点数据或插值有问题"，而 PNG 里三角形画得
/// 好好的。一个**测试用例的坐标**算错，伪装成了渲染实现的 bug——这是最难查的
/// 一类，因为你会先去怀疑 shader、怀疑管线、怀疑读回，而真凶在采样点自己身上。
///
/// 所以现在坐标只从顶点算：顶点内侧的点取"顶点 → 重心"这条线段上
/// [`VERTEX_INSET`] 处的那个像素。这条线段对任何非退化的三角形都在内部，
/// 且重心坐标**与分辨率和宽高比无关**（仿射不变），所以换尺寸也不会失效。
///
/// 光靠"这次算对了"不够，[`every_sample_point_lands_where_it_claims_to`]
/// 把"这个点确实在三角形里、且声明的通道权重确实最大"变成断言。
///
/// `256x256` 下的坐标：
///
/// | 采样点 | 像素坐标 | 重心权重（红/绿/蓝） |
/// |---|---|---|
/// | `near_vertex_red` | (128, 50) | ≈ 0.934 / 0.033 / 0.033 |
/// | `near_vertex_green` | (56, 176) | ≈ 0.033 / 0.934 / 0.033 |
/// | `near_vertex_blue` | (200, 176) | ≈ 0.033 / 0.033 / 0.934 |
pub fn probe_samples(size: (u32, u32)) -> Vec<ProbeSample> {
    let (w, h) = size;
    let [v0, v1, v2] = PROBE_VERTICES.map(|v| v.pixel_position(size));
    let centroid = [(v0[0] + v1[0] + v2[0]) / 3.0, (v0[1] + v1[1] + v2[1]) / 3.0];

    // 顶点 → 重心 的线段上取 [`VERTEX_INSET`] 处，再落到最近的像素。
    let inset = |v: [f64; 2]| -> (u32, u32) {
        let x = v[0] + VERTEX_INSET * (centroid[0] - v[0]);
        let y = v[1] + VERTEX_INSET * (centroid[1] - v[1]);
        (x.round() as u32, y.round() as u32)
    };
    let (red, green, blue) = (inset(v0), inset(v1), inset(v2));

    vec![
        // 清屏色。线性 0.05 → sRGB 编码 → 8 位。这个值能暴露 sRGB 编码有没有生效。
        // 取两个角而不是一个：只取一个的话，"画面整块平移"这类错误会被漏掉。
        ProbeSample {
            name: "background_topleft",
            x: 2,
            y: 2,
            expect: SampleExpectation::Background,
        },
        ProbeSample {
            name: "background_bottomright",
            x: w - 3,
            y: h - 3,
            expect: SampleExpectation::Background,
        },
        // 三个顶点各自内侧一点。纯红/绿/蓝在 sRGB 编码后应当贴着 255 / 0，
        // 任何精度损失都会让"贴着"变成"差一点"。
        ProbeSample {
            name: "near_vertex_red",
            x: red.0,
            y: red.1,
            expect: SampleExpectation::Dominant(0),
        },
        ProbeSample {
            name: "near_vertex_green",
            x: green.0,
            y: green.1,
            expect: SampleExpectation::Dominant(1),
        },
        ProbeSample {
            name: "near_vertex_blue",
            x: blue.0,
            y: blue.1,
            expect: SampleExpectation::Dominant(2),
        },
    ]
}

/// 探针用的 WGSL。`include_str!` 编进 crate——**两端拿到的是同一份字节**。
///
/// 不用运行时加载：文件系统在 wasm 里不存在，任何"两端各自读文件"的方案都会
/// 引入"读到的到底是不是同一份"这个额外的怀疑对象。
pub const PROBE_WGSL: &str = include_str!("../shaders/probe.wgsl");

/// 探针渲染管线。构造一次、复用——真实渲染图也必须是这个形状，
/// 没人会在每帧重建 pipeline。
pub struct ProbeRenderer {
    pipeline: wgpu::RenderPipeline,
    vertex_buffer: wgpu::Buffer,
}

impl ProbeRenderer {
    /// `target_format` 必须与 [`render`](Self::render) 传入的纹理格式一致，
    /// 否则 wgpu 会在校验时报错（这是好事：格式不匹配的 bug 不该靠肉眼发现）。
    pub fn new(device: &wgpu::Device, target_format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("dhampir probe shader"),
            source: wgpu::ShaderSource::Wgsl(PROBE_WGSL.into()),
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("dhampir probe pipeline layout"),
            // 空数组：探针不绑任何资源。
            bind_group_layouts: &[],
            // wgpu 30 用 `immediate_size` 取代了 `push_constant_ranges`——
            // 也就是 WebGPU 的 `var<immediate>`。要 0 字节：探针不传任何参数，
            // 而且非零就得开 `Features::IMMEDIATES`，那会踩到"两端能力不一致"。
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("dhampir probe pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                // 每一项都是 `Option`：wgpu 30 用 `None` 表达"这个 slot 空着"，
                // 而不是靠数组长度少一项。探针只有一个顶点缓冲，所以是 `[Some(..)]`。
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: ProbeVertex::STRIDE,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &[
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x2,
                            offset: 0,
                            shader_location: 0,
                        },
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x3,
                            offset: size_of::<[f32; 2]>() as wgpu::BufferAddress,
                            shader_location: 1,
                        },
                    ],
                })],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: target_format,
                    // 不做混合：探针要测的是"三角形本身的颜色对不对"，
                    // 混进来一个混合方程就多一个差异来源。
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                // 不剔除任何面。三角形绕序在两端应当一致，但那是 M2 该测的东西，
                // 不是 M0 该假设的东西——先让它全都画出来，出问题再看。
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("dhampir probe vertices"),
            contents: bytemuck::cast_slice(&PROBE_VERTICES),
            usage: wgpu::BufferUsages::VERTEX,
        });

        Self {
            pipeline,
            vertex_buffer,
        }
    }

    /// 把探针画进 `target`。
    ///
    /// `load` 决定是清屏还是叠加到已有内容上。默认用 [`wgpu::LoadOp::Clear`] +
    /// [`PROBE_CLEAR_COLOR`]，见 [`render_probe_frame`]。
    pub fn render(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        load: wgpu::LoadOp<wgpu::Color>,
    ) {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("dhampir probe pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                // 3D 视图才需要切片索引；我们永远是 2D 目标。显式 `None` 而不是
                // 靠默认值——wgpu 30 起这是必填字段，将来若有 3D 用法会在这里编译失败。
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });

        pass.set_pipeline(&self.pipeline);
        pass.set_vertex_buffer(0, self.vertex_buffer.slice(..));
        pass.draw(0..PROBE_VERTICES.len() as u32, 0..1);
    }
}

/// 一帧到位的探针渲染：建管线、画、提交。
///
/// **这是探针与测试专用的便捷函数**——它每帧重建管线，真实渲染图不能这么干。
/// 之所以留着，是因为"M0 的验收只有一句话：能不能出图"，
/// 而 `render_probe_frame(device, queue, target, fmt)` 是这句话最短的写法。
pub fn render_probe_frame(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    target: &wgpu::TextureView,
    target_format: wgpu::TextureFormat,
) {
    let renderer = ProbeRenderer::new(device, target_format);
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("dhampir probe frame"),
    });
    renderer.render(&mut encoder, target, wgpu::LoadOp::Clear(PROBE_CLEAR_COLOR));
    queue.submit([encoder.finish()]);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vertex_stride_has_no_padding() {
        // 若有人往 ProbeVertex 里加了字段却忘了改 vertex layout，
        // 症状会是"画面整个歪掉"，非常难查。这条测试把它挡在前面。
        assert_eq!(ProbeVertex::STRIDE, 20);
        assert_eq!(size_of::<ProbeVertex>(), 20);
    }

    #[test]
    fn vertices_avoid_pixel_centres_and_midpoints() {
        // 把 [`PROBE_VERTICES`] 文档里那张表变成断言。改坐标时必须重算那张表，
        // 否则 M2 会以"结构性差异"的形式给出假阳性，而那种失败最难归因。
        //
        // 阈值 0.2：可达上限是 0.25（四分位），当前坐标实测 ≥ 0.2448。
        // 卡在 0.2 上，是让"将来顺手把坐标改整齐"的人立刻撞墙——0.05 也能过，
        // 但那点余量挡不住"离像素角千分之几像素"那种薄片。
        const MIN_MARGIN: f64 = 0.2;
        for (i, v) in PROBE_VERTICES.iter().enumerate() {
            // 走 [`ProbeVertex::pixel_position`] 而不是在这里再推一遍公式：
            // 采样点与这条断言必须认同一套换算，否则改了公式只有一半跟着改。
            let [px, py] = v.pixel_position(PROBE_TARGET_SIZE);
            for (axis, value) in [("x", px), ("y", py)] {
                let frac = value - value.floor();
                assert!(
                    (frac - 0.5).abs() > MIN_MARGIN,
                    "顶点 {i} 的 {axis} 落在像素正中间附近（{value:.4}），光栅化 tie-breaking 会成为实现自由"
                );
                assert!(
                    frac.min(1.0 - frac) > MIN_MARGIN,
                    "顶点 {i} 的 {axis} 落在像素中心附近（{value:.4}），覆盖判定会成为实现自由"
                );
            }
        }
    }

    /// 采样点几何自检用的向量运算。故意写得笨一点——这里要的是"看得懂"，不是性能。
    mod geometry {
        pub fn sub(a: [f64; 2], b: [f64; 2]) -> [f64; 2] {
            [a[0] - b[0], a[1] - b[1]]
        }

        pub fn cross(a: [f64; 2], b: [f64; 2]) -> f64 {
            a[0] * b[1] - a[1] * b[0]
        }

        pub fn norm(a: [f64; 2]) -> f64 {
            (a[0] * a[0] + a[1] * a[1]).sqrt()
        }

        /// 三角形在**窗口坐标**里（像素索引 + 0.5 就是像素中心）。
        ///
        /// 用窗口坐标而不是 NDC：光栅化的覆盖判定发生在窗口坐标里，
        /// 而"这个点会不会被画到"正是要验的东西。
        pub fn window_triangle(size: (u32, u32)) -> [[f64; 2]; 3] {
            super::PROBE_VERTICES.map(|v| {
                let p = v.pixel_position(size);
                [p[0] + 0.5, p[1] + 0.5]
            })
        }

        /// 采样点覆盖的那个像素中心（窗口坐标）。
        ///
        /// 用**像素中心**而不是左上角：光栅化的覆盖判据是"像素中心是否在三角形内"。
        pub fn sample_window_point(sample: &super::ProbeSample) -> [f64; 2] {
            [f64::from(sample.x) + 0.5, f64::from(sample.y) + 0.5]
        }

        /// 点相对三角形的三个重心权重，顺序与 `PROBE_VERTICES` 一致。
        /// 任一分量为负 = 点在三角形外。退化三角形返回 `None`。
        pub fn barycentric(tri: &[[f64; 2]; 3], p: [f64; 2]) -> Option<[f64; 3]> {
            let [a, b, c] = *tri;
            let (v0, v1, v2) = (sub(b, a), sub(c, a), sub(p, a));
            let den = cross(v0, v1);
            if den.abs() < 1e-12 {
                return None;
            }
            let u = cross(v2, v1) / den;
            let v = cross(v0, v2) / den;
            Some([1.0 - u - v, u, v])
        }

        /// 点到三条边所在直线的距离里最小的那个（像素）。
        ///
        /// 这是"这个点离边界有多远"的度量。贴着边界的点，归属由 fill rule 决定，
        /// 而 fill rule 是实现自由——所以采样点自己先离边界远一点，
        /// M2 才不至于把规范留白读成"两端渲染不一致"。
        pub fn distance_to_nearest_edge(tri: &[[f64; 2]; 3], p: [f64; 2]) -> f64 {
            let mut best = f64::INFINITY;
            for i in 0..3 {
                let (a, b) = (tri[i], tri[(i + 1) % 3]);
                let edge = sub(b, a);
                best = best.min(cross(edge, sub(p, a)).abs() / norm(edge));
            }
            best
        }
    }

    /// **这条测试就是为了一次真实的误报而写的。**
    ///
    /// 采样点坐标原先手写在与顶点无关的地方，三个"顶点附近"的点全落在三角形
    /// 外面，于是 native 的断言报"顶点数据或插值有问题"，而 PNG 里三角形画得
    /// 好好的。当时没有任何一条测试能指出"是你的采样点在三角形外面"——
    /// 只能靠人肉去看图、去算坐标。这条测试把它变成一句话。
    ///
    /// 覆盖多个尺寸而不是只测 `256x256`：坐标是参数化推出来的，只测默认尺寸
    /// 会让"其实只是对 256 调对了"这种实现蒙混过关。
    #[test]
    fn every_sample_point_lands_where_it_claims_to() {
        const MIN_EDGE_GAP: f64 = 1.0;
        /// 主导通道自己的权重下限。低于它，"某通道占优"这条断言就退化成
        /// "某通道稍微大一点"，失去判别力——那正是 M0 断言存在的理由。
        const MIN_DOMINANT_WEIGHT: f64 = 0.8;

        for size in [PROBE_TARGET_SIZE, (512, 512), (384, 216), (300, 700)] {
            let tri = geometry::window_triangle(size);
            for sample in probe_samples(size) {
                let p = geometry::sample_window_point(&sample);
                let w = geometry::barycentric(&tri, p).expect("探针三角形不该退化");
                let gap = geometry::distance_to_nearest_edge(&tri, p);
                let where_ = format!("{} @ {size:?} = ({}, {})", sample.name, sample.x, sample.y);

                // 越界检查放在最前面：越界的点取不到像素，症状会变成
                // "读回的值是 None"这种和几何无关的报错。
                assert!(
                    sample.x < size.0 && sample.y < size.1,
                    "采样点越界：{where_}"
                );

                match sample.expect {
                    SampleExpectation::Background => {
                        assert!(
                            w.iter().any(|k| *k < 0.0),
                            "采样点 {where_} 声明是清屏色，却落在三角形**内部**（重心坐标 {w:?}）"
                        );
                        assert!(
                            gap > MIN_EDGE_GAP,
                            "背景点 {where_} 离边界只有 {gap:.3} 像素"
                        );
                    }
                    SampleExpectation::Dominant(channel) => {
                        let i = usize::from(channel);
                        assert!(i < 3, "采样点 {where_} 的主导通道下标越界：{channel}");
                        assert!(
                            w.iter().all(|k| *k > 0.0),
                            "采样点 {where_} 声明在三角形**内部**，实际在外面（重心坐标 {w:?}）"
                        );
                        assert!(
                            gap > MIN_EDGE_GAP,
                            "内部采样点 {where_} 离边界只有 {gap:.3} 像素"
                        );
                        assert!(
                            w[i] > MIN_DOMINANT_WEIGHT,
                            "采样点 {where_} 声明通道 {i} 占优，但它的权重只有 {:.4}——颜色太混，断言没有判别力",
                            w[i]
                        );
                        for j in 0..3 {
                            assert!(
                                j == i || w[i] > w[j],
                                "采样点 {where_} 声明通道 {i} 占优，但权重 {i} = {:.4} 不大于 {j} = {:.4}",
                                w[i],
                                w[j]
                            );
                        }
                    }
                }
            }
        }
    }

    /// 钉死 `256x256` 下的采样坐标，包括 [`probe_samples`] 文档里那张表。
    ///
    /// 为什么值得钉：这些坐标会带着名字写进 `records/` 里的 `run.json`，
    /// 被长期留存、跨版本比较——坐标悄悄变了，历史记录就失去可比性，
    /// 而这种变化不会有任何症状。改了 [`VERTEX_INSET`] 或顶点坐标就必须
    /// 回来改这张表，**这是设计意图，不是阻碍**。
    #[test]
    fn documented_sample_coordinates_are_the_real_ones() {
        let coords: Vec<(&str, u32, u32)> = probe_samples(PROBE_TARGET_SIZE)
            .into_iter()
            .map(|s| (s.name, s.x, s.y))
            .collect();
        assert_eq!(
            coords,
            vec![
                ("background_topleft", 2, 2),
                ("background_bottomright", 253, 253),
                ("near_vertex_red", 128, 50),
                ("near_vertex_green", 56, 176),
                ("near_vertex_blue", 200, 176),
            ]
        );
    }

    /// 剥掉 `//` 行注释与 `/* */` 块注释。
    ///
    /// 为什么必须剥：要扫的是**代码**，不是散文。模块头的说明里正好写着
    /// "没有导数（`fwidth` / `dpdx` / `dpdy`）"——不剥注释的话，守卫会去举报
    /// 自己的文档，而下一个人的修法通常是"把守卫删掉"。守卫被删掉比守卫误报更糟。
    fn strip_wgsl_comments(src: &str) -> String {
        let mut out = String::with_capacity(src.len());
        let mut chars = src.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '/' && chars.peek() == Some(&'/') {
                // 行注释：连行尾的换行一起吃掉。
                for c in chars.by_ref() {
                    if c == '\n' {
                        break;
                    }
                }
            } else if c == '/' && chars.peek() == Some(&'*') {
                chars.next();
                while let Some(c) = chars.next() {
                    if c == '*' && chars.peek() == Some(&'/') {
                        chars.next();
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    #[test]
    fn comment_stripping_leaves_code_alone() {
        // 剥注释这件事本身也要被守住：如果它哪天变成了"整份都吃掉"，
        // 下面那条可移植性子集检查会瞬间变成永远为真的空话。
        let src = "// fwidth\nlet a = 1; /* dpdx */ let b = 2; // loop\n";
        assert_eq!(strip_wgsl_comments(src), "let a = 1;  let b = 2; ");
    }

    #[test]
    fn wgsl_is_embedded_and_stays_in_the_portable_subset() {
        let code = strip_wgsl_comments(PROBE_WGSL);

        // "编进来了"：两个入口都在。这里只证明文本存在；
        // "这份 WGSL 真能编译"是靠 GPU 路径证明的（宿主建 shader module 时会过编译器），
        // 不在这里假装——本 crate 没有 CPU 侧的 WGSL 编译器。
        for marker in ["@vertex", "@fragment", "fn vs_main", "fn fs_main"] {
            assert!(code.contains(marker), "探针 WGSL 里找不到 {marker}");
        }

        // 能力下限（指导文档 §4.3①）：这些构造的结果允许因实现而异，
        // 基准用例里一个都不能出现，否则 M2 的双运行时比对测的是实现差异，
        // 不是我们的代码差异。扫描对象是**去注释后**的代码。
        for forbidden in ["fwidth", "dpdx", "dpdy", "textureSample", "loop", "atomic"] {
            assert!(
                !code.contains(forbidden),
                "探针 WGSL 的代码里出现了 {forbidden}"
            );
        }
    }
}
