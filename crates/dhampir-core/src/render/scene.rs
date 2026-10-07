//! M1 corpus：五张**确定性**图，与它们的判定（纯函数）。
//!
//! 一份 WGSL（[`SCENE_WGSL`]）在 native 与浏览器两个宿主上各自渲染同一帧。
//! M1 要钉死的是**单端可复现**：同样的代码、同样的参数、同样的尺寸 → 逐字节相同的
//! 像素。两端是否一致是 M2 的事，但 M2 只有在单端先确定的前提下才有意义——
//! 否则差异来自哪里永远说不清。
//!
//! # 五张图各考什么
//!
//! | 场景 | 考什么 | 用的机制 |
//! |---|---|---|
//! | `gradient` | 插值与 8 位量化精度 | 全屏三角形的 `@builtin(position)` |
//! | `checker` | 光栅化坐标的精确性 | 整数运算定的格子边界 |
//! | `srgb_linear` | sRGB ↔ linear 往返 | 硬件的存储编码 |
//! | `alpha_stack` | 混合顺序 | 固定功能混合 + 顶点数据 |
//! | `blur` | 多趟与 clamp 语义 | `Rgba16Float` 中间纹理 |
//!
//! 五个场景**共用同一个绑定组布局**（`@binding(0)` 参数 + `@binding(1)` 中间纹理）。
//! 程序化场景根本用不到 `@binding(1)`，但"每个场景一套布局"会让 M2 的归因多一层变量：
//! 两端不一致时，先要排除的就不只是渲染，还有"布局长什么样"。
//!
//! # 判据为什么是"与模型比"，而不是"与某个魔数比"
//!
//! 每个采样点都有一个**独立答案**（[`super::scene_model`]，纯 `f64`，不碰 GPU），
//! 实测字节与它的距离必须 ≤ [`BYTE_TOLERANCE`]。魔数做不到这件事：魔数是从某一次
//! 运行里抄下来的，它证明不了那次运行为什么是那个值，也说不清差 3 个字节时
//! 到底错在哪一步。模型可以逐行对着 `scene.wgsl` 读，也能算"缺陷实现会偏多少"。
//!
//! # 容差 1 字节是从哪来的（不是拍的）
//!
//! | 来源 | 量级 | 折成 8 位输出 |
//! |---|---|---|
//! | 硬件 sRGB 编码允许近似（规范明文） | 实现自定 | **最多 1 LSB** |
//! | `blur` 的 `Rgba16Float` 中间量化 | 约 `5e-4`（线性） | 实测 0 字节（模型测试钉住） |
//! | `alpha_stack` 逐层落回 8 位 | 每层 1/255 | 实测 ≤ 1（判据按最坏 2 条 1 字节留） |
//!
//! 而我们要抓的缺陷**远超** 1 字节——这些距离由 `tolerance_bites_the_defects_we_care_about`
//! 逐条算出来（都是"整张表里最近的样本"）：
//!
//! | 缺陷 | 最近的样本差 | 说明 |
//! |---|---|---|
//! | 混合状态误用 `PREMULTIPLIED_ALPHA_BLENDING` | 52 | 且 alpha 通道**一模一样**，只有颜色偏 |
//! | alpha 通道源因子误写成 `SrcAlpha` | 78 | 反向的一半错 |
//! | 层序反了 | 37 | |
//! | 漏一次 `srgb_decode` | 17 | `k = 0` 的样本抓不到（0 解码前后都是 0） |
//! | 盒式滤波替代高斯 | 12 | 平场上的两个样本抓不到（两者都等于原值；能看见的三个里最远的是高光块那个，35） |
//! | `blur` 边界语义（clamp / 零填充 / 环绕） | 35 / 28 | 只有 `x = 0` 那个点看得见 |
//! | 通道置换 r ↔ b | 4 | 只在 `r ≠ b` 的样本上量（灰图交换这两个通道是恒等映射）；最近在 `gradient` 第 4 帧 `x = 8`，斜坡与锯齿在那里碰巧交叉 |
//! | 半像素相位（少加 `0.5`） | 2 | 见下 |
//!
//! 最后一行那个 2 值得一提：它由 b 通道那条 8 周期锯齿给出，而且**每个采样点**都给得出
//! ——全部样本都差 2 字节以上。要是指望 r 通道就糟了：斜坡一路只有 5 个样本能单独超过
//! 容差，那 5 个都落在斜坡绕回 0 之后、sRGB 编码斜率最大的地方（两个落进线性段，差 6
//! 字节），剩下 75 个样本只差 0 到 1 字节；`g` 通道（`1 - t`）更是全程 ≤ 1。这就是
//! `gradient` 里那个 `frac(t * 8)` 存在的理由：它把"少加半个像素"这件事在**每个**采样点
//! 上都放大到看得见。
//!
//! 所以容差 1 不是"松"，而是"刚好等于规范允许的实现自由"。
//!
//! # 采样点是挑过的，每个都有用
//!
//! 采样表用**静态坐标**，"这一帧应当是什么颜色"交给模型按帧号算。混着来（坐标随帧
//! 变化）会让"读数对不上"变成两种可能：模型错了，还是采样点挪了？M0 已经因为
//! "采样坐标与几何对不上"吃过一次亏，见 `render/probe.rs` 的模块文档。
//!
//! 唯一一处需要留心的地方写在 [`SamplePoint::purpose`] 里：`srgb_linear` 的 `k=4`
//! 理想字节正好是 `127.5`，**正落在舍入边界上**——那一点的读数是 127 还是 128
//! 属于实现自由，容差正好覆盖它。

use bytemuck::{Pod, Zeroable};
use wgpu::util::DeviceExt as _;

use super::scene_model::{self, EdgeMode, StackOrder};
use crate::wgpu;

/// 编进库里的那**一份** WGSL。native 交给 naga，浏览器交给 Tint。
pub const SCENE_WGSL: &str = include_str!("../shaders/scene.wgsl");

/// corpus 的默认目标尺寸。
///
/// 256×256 是挑的：够大到能放下 8 像素格子与 3×3 高光块的邻域，又小到
/// 逐字节比对、PNG 归档、人工看图都不费劲。每个场景自己声明尺寸
/// （[`SceneSpec::size`]），当前五个都是这个值。
pub const SCENE_TARGET_SIZE: (u32, u32) = (256, 256);

/// 目标纹理格式。着色器输出**线性光**，sRGB 编码由硬件在存储时做。
pub const SCENE_TARGET_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

/// `blur` 的中间纹理格式。
///
/// 必须是 16 位浮点，**不能**是 8 位：8 位存储会把每一趟的差异压到 `1/255` 以下，
/// 于是"三趟结构"与"clamp 语义"这两件能在 8 位下观测到的事还在，但"这个中间值
/// 是不是真的走了浮点"就完全看不见了。代价是引入了约 `5e-4` 的量化误差——
/// 它在 8 位输出上折成 **0 个字节**（模型测试钉住），所以判据不用为它放宽。
pub const BLUR_INTERMEDIATE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

/// 实测字节与模型预测之间允许的**最大单通道差**。
///
/// 取 1 的理由见模块文档那张表：规范允许硬件 sRGB 编码近似，最多 1 LSB。
/// 任何比这更小的值都会把"规范留白"读成"我们的缺陷"，而那种假阳性的代价
/// 是让人去改判据——最终把判据改成恒绿。
pub const BYTE_TOLERANCE: u8 = 1;

/// 场景清屏色：不透明黑。
///
/// 全屏三角形覆盖每个像素，所以这个值在结果里**看不见**；留着它是为了"三角形
/// 没画上去"这种情况有症状——那时读回的是全黑而不是透明，肉眼与字节都能认出来。
const CLEAR_OPAQUE_BLACK: wgpu::Color = wgpu::Color {
    r: 0.0,
    g: 0.0,
    b: 0.0,
    a: 1.0,
};

/// `alpha_stack` 的清屏色：**透明黑**。
///
/// 这一处不能和别的场景一样用不透明黑：混合模型的第一层是"叠在 `[0, 0, 0, 0]` 上"，
/// 底下的 alpha 一旦从 0 变成 1，`dst_alpha = src_a + dst_a·(1 - src_a)` 这条式子
/// 算出来的 alpha 就与实测差出几十个字节——而三通道照旧基本正常，症状会指向"混合
/// 方程写错了"，实际只是清屏色。
const CLEAR_TRANSPARENT: wgpu::Color = wgpu::Color {
    r: 0.0,
    g: 0.0,
    b: 0.0,
    a: 0.0,
};

/// `scene.wgsl` 的 `Params`。**布局必须与 WGSL 侧逐字段一致**。
///
/// WGSL：`vec2<f32>`（偏移 0、8 字节）+ `u32`（偏移 8）+ `u32`（偏移 12）= 16 字节。
/// Rust：`[f32; 2]`（0、8）+ `u32`（8）+ `u32`（12）= 16 字节。测试用
/// `offset_of!` 把偏移逐个钉住——这个结构体是 uniform，错一个字节的表现是
/// "画面整个不对"，而不是编译错误。
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
pub struct Params {
    /// 目标尺寸（像素）。着色器用它把帧坐标换算成 uv。
    pub size: [f32; 2],
    /// 帧号。只以整数方式参与运算。
    pub frame: u32,
    /// 对齐填充，同时留给 M2 的参数。
    pub pad: u32,
}

impl Params {
    /// 某一帧、某个尺寸的参数。
    pub fn for_frame(size: (u32, u32), frame: u32) -> Self {
        Self {
            size: [size.0 as f32, size.1 as f32],
            frame,
            pad: 0,
        }
    }

    /// uniform 缓冲里这块数据的字节数。
    pub const SIZE: wgpu::BufferAddress = size_of::<Self>() as wgpu::BufferAddress;
}

/// `vs_layer` 的顶点：一个线性颜色（含 alpha）。
///
/// 每层三个顶点、颜色相同——`vs_layer` 用 `@builtin(vertex_index)` 取位置，
/// 这个属性只带颜色。插值不会引入差异：三个顶点的值一样，插出来就是那个值。
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
pub struct LayerVertex {
    pub color: [f32; 4],
}

impl LayerVertex {
    /// 顶点在 buffer 里的字节跨度。16 字节，无填充。
    pub const STRIDE: wgpu::BufferAddress = size_of::<Self>() as wgpu::BufferAddress;
}

/// 顶点布局：一个 `vec4<f32>` 颜色，位于 `@location(0)`。
fn layer_vertex_buffer_layout() -> wgpu::VertexBufferLayout<'static> {
    wgpu::VertexBufferLayout {
        array_stride: LayerVertex::STRIDE,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &[wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Float32x4,
            offset: 0,
            shader_location: 0,
        }],
    }
}

/// `alpha_stack` 的混合状态：**直通 alpha 的 source-over**。
///
/// ```text
/// color = src_rgb·src_a + dst_rgb·(1 - src_a)
/// alpha = src_a       + dst_a  ·(1 - src_a)     ← 源因子是 1，不是 src_a
/// ```
///
/// 这就是 `wgpu::BlendState::ALPHA_BLENDING`——**逐字段写出来**，不直接调那个常量：
/// 这一条状态本身就是 `alpha_stack` 的**被测对象**，把它藏在常量名字后面，
/// 文件里就再也看不到"正在被考的是哪条式子"。`alpha_stack_model_matches_the_blend_state`
/// 同时钉住"手写的这个 == 标准常量"与"逐字段都对"。
///
/// **不能**换成 `wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING`：它给颜色通道的源因子
/// 也是 `One`（即 `BlendComponent::OVER`，"颜色已经乘过 alpha"的假设），而
/// `fs_layer` 输出的是**没乘过 alpha** 的颜色。用错之后颜色通道会一路偏亮（差几十个
/// 字节），而 alpha 通道**完全正确**（两个常量的 alpha 分量都是 `OVER`）——
/// 于是"alpha 看着对、颜色偏亮"这种症状会把人引向"着色器写错了"，
/// 而实际错在混合状态。三通道一起错反而不如这种一半对一半错的难查。
const LAYER_BLEND_STATE: wgpu::BlendState = wgpu::BlendState {
    color: wgpu::BlendComponent {
        src_factor: wgpu::BlendFactor::SrcAlpha,
        dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
        operation: wgpu::BlendOperation::Add,
    },
    alpha: wgpu::BlendComponent {
        src_factor: wgpu::BlendFactor::One,
        dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
        operation: wgpu::BlendOperation::Add,
    },
};

/// 一个场景怎么画。
#[derive(Clone, Copy, Debug)]
pub enum SceneDraw {
    /// 一个全屏三角形，无顶点缓冲。
    Fullscreen,
    /// 每层一个顶点缓冲，**绘制顺序就是层序**。
    ///
    /// 带的是模型里的层颜色（`f64`，与 [`scene_model::LAYERS_LINEAR`] 同一个来源），
    /// 渲染时转成 `f32` 装进顶点缓冲。层色只在这里声明一次：模型与顶点数据共用它，
    /// 才谈得上"模型预测的是硬件画的那个东西"。
    Layers(&'static [[f64; 4]]),
}

/// 一个场景有几趟、每趟用哪个片元入口。
#[derive(Clone, Copy, Debug)]
pub enum ScenePasses {
    /// 一趟，直接画进目标纹理。
    Single { fragment: &'static str },
    /// 三趟：图案 → 横向 → 纵向。
    ///
    /// 前两趟写 [`BLUR_INTERMEDIATE_FORMAT`] 中间纹理，最后一趟直接写目标纹理。
    /// 需要**两张**中间纹理（乒乓）：同一趟里既读又写同一张纹理会撞上
    /// "纹理同时是渲染目标与采样源"这条校验，而且那本来就是没定义的行为。
    Blur {
        source: &'static str,
        horizontal: &'static str,
        vertical: &'static str,
    },
}

/// 一个采样点。
///
/// **坐标是静态的**：哪一帧该是什么颜色由模型按帧号算，采样表不跟着帧变。
/// 混着来会让"读数对不上"多一种解释（模型错了还是点挪了），而 M0 已经证明
/// 那种歧义查起来很贵。
#[derive(Clone, Copy, Debug)]
pub struct SamplePoint {
    /// 表里用的短名。
    pub label: &'static str,
    pub x: u32,
    pub y: u32,
    /// 这个点想抓什么。写进记录——"为什么是这几个点"必须能被人读出来。
    pub purpose: &'static str,
}

/// 一个场景的全部声明。**worker 不该知道任何这里没写的东西**。
#[derive(Clone, Copy, Debug)]
pub struct SceneSpec {
    /// `--scene` 接受的名字。
    pub name: &'static str,
    /// 一句话说明考什么（写进记录）。
    pub description: &'static str,
    /// 目标尺寸。worker 必须按它建纹理——尺寸进了 `Params`，也决定光栅化。
    pub size: (u32, u32),
    /// 清屏色。全屏三角形之下看不见，但 `alpha_stack` 的混合底数靠它。
    pub clear: wgpu::Color,
    /// 顶点入口。
    pub vertex_entry: &'static str,
    /// 几趟、每趟用哪个片元入口。
    pub passes: ScenePasses,
    /// 怎么画。
    pub draw: SceneDraw,
    /// 这一场景是否随帧号变化。
    ///
    /// 只用来**说清楚**：`blur` 是唯一 `false` 的场景，它的判据是"实测对上模型卷积"，
    /// 图案一旦随帧平移，"采样点落在图案的哪一格"就每帧都要重算——M0 已经因为
    /// 采样坐标和几何对不上吃过一次亏，这里宁可少一个变量。帧号的确定性由其它四个
    /// 场景覆盖（周期分别是 16 / 3 / 8 / 4 帧，见各场景的采样说明）。
    pub uses_frame: bool,
    /// 采样点。
    pub samples: &'static [SamplePoint],
}

impl SceneSpec {
    /// 这个场景要画几趟。
    pub fn pass_count(&self) -> u32 {
        match self.passes {
            ScenePasses::Single { .. } => 1,
            ScenePasses::Blur { .. } => 3,
        }
    }

    /// 这个场景用到的全部片元入口名。
    pub fn fragment_entries(&self) -> Vec<&'static str> {
        match self.passes {
            ScenePasses::Single { fragment } => vec![fragment],
            ScenePasses::Blur {
                source,
                horizontal,
                vertical,
            } => vec![source, horizontal, vertical],
        }
    }
}

/// gradient 的采样点（`y = 128`，这一场景只看 x）。
///
/// 三通道各有各的形状，所以每个点都能同时问到三件事：`r` 是斜坡（整体是否单调、
/// 端点是否到位）、`g` 是反向斜坡（通道顺序写反会立刻显形）、`b` 是 8 周期锯齿
/// （插值或量化差一个最低位，相位就整段错开——比"斜坡看着对不对"好判得多）。
///
/// `x = 8` 同时是"接近 0 但**不是** 0"的那一点：全黑在分不清"没渲染"与"渲染成黑"
/// 的同时，还会让线性段的公式（`s/12.92`）被跳过而不自知。
static GRADIENT_SAMPLES: [SamplePoint; 5] = [
    SamplePoint {
        label: "斜坡起点",
        x: 8,
        y: 128,
        purpose: "接近 0 但不是 0：线性段公式与「没渲染」都靠它区分",
    },
    SamplePoint {
        label: "斜坡 1/8",
        x: 32,
        y: 128,
        purpose: "r/g 单调性；b 的锯齿第 2 段第一格",
    },
    SamplePoint {
        label: "锯齿 2/8",
        x: 48,
        y: 128,
        purpose: "b 锯齿相位；这一点的 r 通道理想字节离舍入边界只有 0.016",
    },
    SamplePoint {
        label: "锯齿 5/8",
        x: 104,
        y: 128,
        purpose: "b 锯齿第 4 段——与「锯齿 2/8」的 b 应当逐字节相同（周期性的直接证据）",
    },
    SamplePoint {
        label: "斜坡 25/32",
        x: 200,
        y: 128,
        purpose: "高端是否到位；g 通道在这里降到 128 附近",
    },
];

/// checker 的采样点。
///
/// 格子边长随帧在 4 / 8 / 12 之间循环，而采样坐标是静态的——所以这六个点挑了
/// **三种尺寸各自的边界两侧**（`3|4`、`7|8`、`11|12`）：第 0 帧只有第一对分属两个
/// 格子，第 1 帧只有第二对，第 2 帧只有第三对。三个点对合起来，三种格子边长都被
/// 采样到两侧——而"边界落在哪个像素上"正是这一场景要钉死的东西（浮点取模会让它
/// 变成一个模糊的判断，所以着色器里用的是整数除法）。
static CHECKER_SAMPLES: [SamplePoint; 7] = [
    SamplePoint {
        label: "边界 3|4 左",
        x: 3,
        y: 1,
        purpose: "第 0 帧（格 4）边界的左邻",
    },
    SamplePoint {
        label: "边界 3|4 右",
        x: 4,
        y: 1,
        purpose: "第 0 帧边界的右邻——与左邻必须**不同色**",
    },
    SamplePoint {
        label: "边界 7|8 左",
        x: 7,
        y: 1,
        purpose: "第 1 帧（格 8）边界的左邻",
    },
    SamplePoint {
        label: "边界 7|8 右",
        x: 8,
        y: 1,
        purpose: "第 1 帧边界的右邻",
    },
    SamplePoint {
        label: "边界 11|12 左",
        x: 11,
        y: 1,
        purpose: "第 2 帧（格 12）边界的左邻",
    },
    SamplePoint {
        label: "边界 11|12 右",
        x: 12,
        y: 1,
        purpose: "第 2 帧边界的右邻",
    },
    SamplePoint {
        label: "末像素",
        x: 255,
        y: 255,
        purpose: "最后一个像素被覆盖到了（少画一列的经典症状就是它）",
    },
];

/// srgb_linear 的采样点（`y = 128`）。
///
/// 条带随帧整体平移，所以四个静态点在不同帧里读到不同的 `k`。全都取**条带中心**
/// （`k` 的跨度是 32 像素，`x = 16 + 32k`）而不是边界：这一场景要考的是传输函数，
/// 不是边界——边界的账由 checker 付。
static SRGB_LINEAR_SAMPLES: [SamplePoint; 4] = [
    SamplePoint {
        label: "k=0",
        x: 16,
        y: 128,
        purpose: "黑端：往返之后必须是 0（线性段）",
    },
    SamplePoint {
        label: "k=2",
        x: 80,
        y: 128,
        purpose: "线性段与幂函数段之间的读数",
    },
    SamplePoint {
        label: "k=4",
        x: 144,
        y: 128,
        purpose: "理想字节 127.5——**正落在舍入边界上**：读数 127 或 128 都算对，容差正是为这种情况准备的",
    },
    SamplePoint {
        label: "k=7",
        x: 240,
        y: 128,
        purpose: "亮端：往返之后必须是 223 而不是 224 或 255",
    },
];

/// alpha_stack 的采样点。
///
/// 每层都是铺满视口的三角形，所以画面**处处应当是同一个值**——两个点就是为了
/// 把这件事变成一条能看见的断言（而不是"假设它成立"）。
static ALPHA_STACK_SAMPLES: [SamplePoint; 2] = [
    SamplePoint {
        label: "中心",
        x: 128,
        y: 128,
        purpose: "层序判据的主战场",
    },
    SamplePoint {
        label: "左下角",
        x: 8,
        y: 248,
        purpose: "与中心逐字节相同：层是整屏的，任何位置相关的差异都是缺陷",
    },
];

/// blur 的采样点。
///
/// 五个点各司其职，其中只有最后一个能看见边界语义——它的存在理由是
/// `the_left_edge_point_is_the_only_one_that_can_see_the_edge_semantics` 这条测试。
static BLUR_SAMPLES: [SamplePoint; 5] = [
    SamplePoint {
        label: "源 0.0 内部",
        x: 19,
        y: 19,
        purpose: "卷积在平坦区应当**完全不变**：抽头都是 0，任何非 0 都是漏了权重",
    },
    SamplePoint {
        label: "源 0.6 内部",
        x: 35,
        y: 27,
        purpose: "平场进、平场出——权重之和必须是 1（这一点的邻居全是 0.6）",
    },
    SamplePoint {
        label: "高光左外侧",
        x: 38,
        y: 41,
        purpose: "横向抽头部分落在 1.0 上：核的形状（而不只是归一化）在这里显形",
    },
    SamplePoint {
        label: "高光内部",
        x: 41,
        y: 41,
        purpose: "横向与纵向都落在 1.0 上：两趟的**顺序**与对称性",
    },
    SamplePoint {
        label: "左边缘",
        x: 0,
        y: 41,
        purpose: "clamp / 零填充 / 环绕三者可分（189 / 154 / 161）——唯一能验边界语义的点",
    },
];

/// 可选的场景，**顺序即记录顺序**。
///
/// 用 `static` 而不是 `const`：`SceneSpec` 要借用这里面的 `'static` 数据，
/// `const` 每次展开都是一个新临时值，拿不到 `'static` 引用。
pub static SELECTABLE_SCENES: [SceneSpec; 5] = [
    SceneSpec {
        name: "gradient",
        description: "全范围渐变：插值与 8 位量化精度",
        size: SCENE_TARGET_SIZE,
        clear: CLEAR_OPAQUE_BLACK,
        vertex_entry: "vs_fullscreen",
        passes: ScenePasses::Single {
            fragment: "fs_gradient",
        },
        draw: SceneDraw::Fullscreen,
        uses_frame: true,
        samples: &GRADIENT_SAMPLES,
    },
    SceneSpec {
        name: "checker",
        description: "像素级棋盘：光栅化坐标的精确性",
        size: SCENE_TARGET_SIZE,
        clear: CLEAR_OPAQUE_BLACK,
        vertex_entry: "vs_fullscreen",
        passes: ScenePasses::Single {
            fragment: "fs_checker",
        },
        draw: SceneDraw::Fullscreen,
        uses_frame: true,
        samples: &CHECKER_SAMPLES,
    },
    SceneSpec {
        name: "srgb_linear",
        description: "sRGB ↔ linear 往返（8 条竖条）",
        size: SCENE_TARGET_SIZE,
        clear: CLEAR_OPAQUE_BLACK,
        vertex_entry: "vs_fullscreen",
        passes: ScenePasses::Single {
            fragment: "fs_srgb_linear",
        },
        draw: SceneDraw::Fullscreen,
        uses_frame: true,
        samples: &SRGB_LINEAR_SAMPLES,
    },
    SceneSpec {
        name: "alpha_stack",
        description: "多层半透明叠加：混合顺序与 alpha 通道的源因子",
        size: SCENE_TARGET_SIZE,
        // 见 CLEAR_TRANSPARENT 的说明：这一场景的清屏色是混合的底数，不是背景。
        clear: CLEAR_TRANSPARENT,
        vertex_entry: "vs_layer",
        passes: ScenePasses::Single {
            fragment: "fs_layer",
        },
        draw: SceneDraw::Layers(&scene_model::LAYERS_LINEAR),
        uses_frame: true,
        samples: &ALPHA_STACK_SAMPLES,
    },
    SceneSpec {
        name: "blur",
        description: "可分离高斯三趟 + 边缘 clamp 语义",
        size: SCENE_TARGET_SIZE,
        clear: CLEAR_OPAQUE_BLACK,
        vertex_entry: "vs_fullscreen",
        passes: ScenePasses::Blur {
            source: "fs_blur_source",
            horizontal: "fs_blur_h",
            vertical: "fs_blur_v",
        },
        draw: SceneDraw::Fullscreen,
        // 刻意的例外，理由见字段文档。
        uses_frame: false,
        samples: &BLUR_SAMPLES,
    },
];

/// 按 `--scene` 的名字找场景。
pub fn scene_by_name(name: &str) -> Option<&'static SceneSpec> {
    SELECTABLE_SCENES.iter().find(|spec| spec.name == name)
}

/// 全部可选名字，用于错误提示与 `--help`。
pub fn scene_names() -> Vec<&'static str> {
    SELECTABLE_SCENES.iter().map(|spec| spec.name).collect()
}

// ---------------------------------------------------------------------------
// 判定（纯函数：不碰 GPU、不碰文件，因此能在没有显卡的机器上被测试）
// ---------------------------------------------------------------------------

/// 一个采样点的判定结果。
///
/// 除了"过没过"，还带上**期望值、距离、容差**与一句话说明：记录里要能看出
/// "差 1 个字节" 是擦着容差过的还是根本没问题，以及当时模型算的是什么。
#[derive(Clone, Debug)]
pub struct SampleVerdict {
    pub passed: bool,
    /// 模型的字节预测。
    pub expected: [u8; 4],
    /// 实测与预测的最大单通道差。
    pub distance: u8,
    /// 当时的容差（就是 [`BYTE_TOLERANCE`]，带上是让记录自带判据）。
    pub tolerance: u8,
    /// 一句话说明：模型算的是什么，反例模型差多少。
    pub detail: String,
}

impl SampleVerdict {
    /// 写进 `records/` 的一行。格式固定，便于跨版本 `diff`。
    pub fn report_line(
        &self,
        spec: &SceneSpec,
        frame: u32,
        point: SamplePoint,
        measured: [u8; 4],
    ) -> String {
        format!(
            "{:<12} f{:<3} ({:>3},{:>3}) {:<14} 实测 {:>3} {:>3} {:>3} {:>3} | 模型 {:>3} {:>3} {:>3} {:>3} | 距离 {} 容差 {} {} — {}",
            spec.name,
            frame,
            point.x,
            point.y,
            point.label,
            measured[0],
            measured[1],
            measured[2],
            measured[3],
            self.expected[0],
            self.expected[1],
            self.expected[2],
            self.expected[3],
            self.distance,
            self.tolerance,
            if self.passed { "通过" } else { "失败" },
            self.detail,
        )
    }
}

/// 模型对一个采样点的**字节预测**。
///
/// `alpha_stack` 与 `blur` 的预测与坐标无关（前者整屏同色，后者坐标只用来定抽头），
/// 但签名统一带上 `point`：worker 只需要一个函数就能算出记录里那张"期望值"表。
///
/// **尺寸取 `spec.size`。** 用 [`SceneRenderer::new_at`] 在别的尺寸上渲染出来的读数
/// 不能拿这里判——图案是按尺寸归一化重算的，不是缩放（见那里的文档）。
pub fn expected_bytes(spec: &SceneSpec, frame: u32, point: SamplePoint) -> [u8; 4] {
    match spec.name {
        "gradient" => scene_model::bytes_of_linear_rgba(scene_model::gradient_linear(
            spec.size, frame, point.x, point.y,
        )),
        "checker" => {
            scene_model::bytes_of_linear_rgba(scene_model::checker_linear(frame, point.x, point.y))
        }
        "srgb_linear" => scene_model::bytes_of_linear_rgba(scene_model::transfer_linear(
            spec.size, frame, point.x,
        )),
        "alpha_stack" => scene_model::alpha_stack_bytes(frame, StackOrder::Forward),
        "blur" => {
            let value = scene_model::blur_at(spec.size, point.x, point.y);
            scene_model::bytes_of_linear_rgba([value, value, value, 1.0])
        }
        other => panic!("没有为场景 {other} 定义模型预测"),
    }
}

/// 判定一个采样点。
///
/// 判据只有一条：实测与模型预测的距离 ≤ [`BYTE_TOLERANCE`]。
///
/// `alpha_stack` 与 `blur` 的**反例模型**（逆序合成、换一种边界语义）只写进
/// [`SampleVerdict::detail`]，不参与通过与否——因为它们是**被蕴含**的：
/// 与正向模型差 ≤ 1 字节的读数，离逆序模型必然 ≥ 36 字节（层序）或 ≥ 27 字节
/// （边界语义）。把蕴含关系写成断言（而不是写成第二条判据）更好：多一条判据就多
/// 一处可能自己出错的地方，而"蕴含"是可以被测试钉住的。
pub fn judge_sample(
    spec: &SceneSpec,
    frame: u32,
    point: SamplePoint,
    measured: [u8; 4],
) -> SampleVerdict {
    let expected = expected_bytes(spec, frame, point);
    let detail = match spec.name {
        "gradient" => model_detail(scene_model::gradient_linear(
            spec.size, frame, point.x, point.y,
        )),
        "checker" => model_detail(scene_model::checker_linear(frame, point.x, point.y)),
        "srgb_linear" => {
            let k = scene_model::transfer_stripe_at(spec.size, frame, point.x);
            format!(
                "{}；第 {k} 条竖条（s = {k}/8）",
                model_detail(scene_model::transfer_linear(spec.size, frame, point.x))
            )
        }
        "alpha_stack" => alpha_stack_detail(frame, measured),
        "blur" => blur_detail(spec, point),
        other => panic!("没有为场景 {other} 定义判据"),
    };
    let distance = scene_model::distance_bytes(measured, expected);
    SampleVerdict {
        passed: distance <= BYTE_TOLERANCE,
        expected,
        distance,
        tolerance: BYTE_TOLERANCE,
        detail,
    }
}

/// 「模型算的是什么」：线性值 → 理想字节 → 离舍入边界多近。
///
/// 离舍入边界很近的点（比如 `gradient` 在 `x = 48` 的 r 通道只有 0.016）会在这里
/// 露出来：那种点的读数允许在容差内偏 1，**不是**缺陷。
fn model_detail(linear: [f64; 4]) -> String {
    let margin = linear[..3]
        .iter()
        .map(|v| scene_model::rounding_margin(*v))
        .fold(f64::INFINITY, f64::min);
    format!(
        "模型线性 [{:.4}, {:.4}, {:.4}] → 理想字节 [{:.3}, {:.3}, {:.3}]，离舍入边界最近 {:.4} 字节",
        linear[0],
        linear[1],
        linear[2],
        scene_model::ideal_light_byte(linear[0]),
        scene_model::ideal_light_byte(linear[1]),
        scene_model::ideal_light_byte(linear[2]),
        margin,
    )
}

/// `alpha_stack` 的说明：层数、逆序预测、不量化理想、离最近的不透明层。
fn alpha_stack_detail(frame: u32, measured: [u8; 4]) -> String {
    let layers = scene_model::layer_count(frame);
    let reverse = scene_model::alpha_stack_bytes(frame, StackOrder::Reverse);
    let ideal = scene_model::bytes_of_linear_rgba(scene_model::alpha_stack_ideal(
        frame,
        StackOrder::Forward,
    ));
    let opaque_min = (0..layers as usize)
        .map(|index| scene_model::distance_bytes(measured, scene_model::opaque_layer_bytes(index)))
        .min()
        .unwrap_or(0);
    format!(
        "{layers} 层顺序合成；逆序预测 {reverse:?}（距离 {}）；不量化理想 {ideal:?}（距离 {}）；离最近的不透明层 {opaque_min} 字节",
        scene_model::distance_bytes(measured, reverse),
        scene_model::distance_bytes(measured, ideal),
    )
}

/// `blur` 的说明：换一种边界语义会差多少、中间量化会差多少。
fn blur_detail(spec: &SceneSpec, point: SamplePoint) -> String {
    let clamp = scene_model::blur_at(spec.size, point.x, point.y);
    let zero = scene_model::bytes_of_linear_rgba(edge_bytes(spec, point, EdgeMode::Zero));
    let wrap = scene_model::bytes_of_linear_rgba(edge_bytes(spec, point, EdgeMode::Wrap));
    let quantized = scene_model::blur_at_with_intermediate_quantized(spec.size, point.x, point.y);
    format!(
        "{}；零填充预测 {zero:?}、环绕预测 {wrap:?}；中间 Rgba16Float 量化后差 {} 字节",
        model_detail([clamp, clamp, clamp, 1.0]),
        scene_model::distance_bytes(
            scene_model::bytes_of_linear_rgba([clamp, clamp, clamp, 1.0]),
            scene_model::bytes_of_linear_rgba([quantized, quantized, quantized, 1.0]),
        ),
    )
}

/// 换一种边界语义时的线性值（合成一个 RGBA 用）。
fn edge_bytes(spec: &SceneSpec, point: SamplePoint, mode: EdgeMode) -> [f64; 4] {
    let value = scene_model::blur_at_with_edge(spec.size, point.x, point.y, mode);
    [value, value, value, 1.0]
}

// ---------------------------------------------------------------------------
// 渲染图
// ---------------------------------------------------------------------------

/// 一个场景的渲染图：管线 + 两张中间纹理 + 逐层的顶点缓冲。
///
/// 与 [`super::ProbeRenderer`] 同一个形状：构造要 `&wgpu::Device` 与**目标格式**，
/// 渲染要 `&mut wgpu::CommandEncoder` 与一个 `&wgpu::TextureView`。中间不出现
/// `Instance`、不出现 surface、不出现任何平台判断——所以浏览器那边能把同一个
/// 结构体接在 canvas 的 texture view 上。
///
/// 一趟都不碰 `queue.submit`：提交是宿主的事（浏览器上没有"提交"，只有
/// `device.queue.submit` 之后运行时自己决定什么时候真的执行）。
pub struct SceneRenderer {
    spec: &'static SceneSpec,
    /// 实际渲染尺寸。默认就是 [`SceneSpec::size`]，[`SceneRenderer::new_at`] 可以换。
    ///
    /// 独立存一份而不是每次读 `spec.size`：换过尺寸之后，"该用哪个尺寸"只有
    /// 一个答案（这个字段），`render` 与中间纹理都从它取——两处各读一次
    /// `spec.size` 的话，换尺寸时漏改一处就会得到"中间纹理 256×256、目标 1920×1080"
    /// 这种不会报错、只是画面不对的组合。
    size: (u32, u32),
    /// 目标格式。构造时给的那一个，不改。
    ///
    /// 存一份出来是因为宿主需要它：`render_frame`（[`super::corpus`]）要按这个格式
    /// 建离屏纹理。宿主自己写 `SCENE_TARGET_FORMAT` 的话，"这两处什么时候会不一致"
    /// 就成了一道要靠人记住的题。
    format: wgpu::TextureFormat,
    uniform: wgpu::Buffer,
    pipeline: ScenePipeline,
    /// 中间纹理的视图（`blur` 是两张乒乓，其余场景是一张 1×1 占位）。
    ///
    /// 这里**只存视图、不另存 `Texture`**：`wgpu::TextureView` 自己就握着底层纹理的
    /// 句柄（wgpu 30 的 `TextureView` 结构体里有一个 `texture: Texture` 字段），
    /// 视图活着，纹理就不会被释放。另存一份 `Texture` 只会多出一个从不被读的字段，
    /// 而"从不被读"正是下一个人删掉它时最想找的理由——不如一开始就说清楚。
    intermediate_views: Vec<wgpu::TextureView>,
    /// 每一趟的 `@binding(1)` 绑定组（长度 = 趟数）。
    bind_groups: Vec<wgpu::BindGroup>,
    /// `alpha_stack` 的逐层顶点缓冲。下标就是层下标。
    layers: Vec<wgpu::Buffer>,
}

/// 一个场景的管线集合。
enum ScenePipeline {
    Single(wgpu::RenderPipeline),
    Blur {
        source: wgpu::RenderPipeline,
        horizontal: wgpu::RenderPipeline,
        vertical: wgpu::RenderPipeline,
    },
}

impl SceneRenderer {
    /// 建好一个场景需要的全部东西，尺寸取 [`SceneSpec::size`]。
    ///
    /// `target_format` 由宿主给（这里永远是 [`SCENE_TARGET_FORMAT`]，但签名保留它：
    /// M2 的浏览器端要用 canvas 的格式，那时这一处**不该**需要改 core）。
    pub fn new(
        device: &wgpu::Device,
        target_format: wgpu::TextureFormat,
        spec: &'static SceneSpec,
    ) -> Self {
        Self::new_at(device, target_format, spec, spec.size)
    }

    /// 同 [`SceneRenderer::new`]，但按调用方给的尺寸。
    ///
    /// # 什么时候要它
    ///
    /// 1. **计时**：M1 的退出标准点的是"1080p 单帧渲染 ≤ 10ms"，而 corpus 场景
    ///    声明的尺寸是 256×256（那个尺寸是给逐点判据用的，不是给性能用的）。
    /// 2. **行对齐**：`copy_texture_to_buffer` 的 `bytes_per_row` 要 256 字节对齐，
    ///    而 256×256（一行 1024 字节）与 1920×1080（一行 7680 字节）**都**整除 256——
    ///    也就是说这两档尺寸永远走不到"补填充再剥掉"那条路。要验证它，必须故意挑
    ///    一个宽度 ×4 不是 256 倍数的尺寸（例如 1366×768：5464 → 5632）。
    /// 3. M4 的导出分辨率是用户给的，不保证是 256 的倍数。
    ///
    /// # 陷阱：判定用的尺寸不会跟着变
    ///
    /// 换尺寸**不是**把一张图缩放一遍，而是"用另一个尺寸把图案重算一遍"：
    /// `gradient` / `srgb_linear` 按 `params.size` 归一化，`checker` / `blur` 用的是
    /// 绝对像素坐标，两者都会因为尺寸变化而改变每个像素该有的值。
    ///
    /// 所以 [`expected_bytes`] 与 [`judge_sample`] **只对 `spec.size` 成立**——
    /// 它们拿的是 `spec.size`，用它去判另一尺寸的读数会得到"看着像偏差"的错误结论。
    /// 在别的尺寸上判定，必须用 [`SceneRenderer::size`] 把实际尺寸传进
    /// [`scene_model`] 重算：那才是"这一尺寸下应当是什么"的答案。
    pub fn new_at(
        device: &wgpu::Device,
        target_format: wgpu::TextureFormat,
        spec: &'static SceneSpec,
        size: (u32, u32),
    ) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("dhampir scene shader"),
            source: wgpu::ShaderSource::Wgsl(SCENE_WGSL.into()),
        });

        // 全场景共用的绑定组布局：参数 + 中间纹理。
        // 程序化场景不读 `@binding(1)`，但布局照样是这一份——见模块文档。
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("dhampir scene bind group layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(Params::SIZE),
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
            ],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("dhampir scene pipeline layout"),
            // 一个绑定组。`immediate_size: 0` 与探针同一个理由：非零就得开
            // `Features::IMMEDIATES`，那会踩到"两端能力不一致"。
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });

        let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("dhampir scene params"),
            contents: bytemuck::bytes_of(&Params::for_frame(size, 0)),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });

        let (pipeline, intermediate_views, bind_groups) = match spec.passes {
            ScenePasses::Single { fragment } => {
                let buffers: [Option<wgpu::VertexBufferLayout<'_>>; 0] = [];
                let layer_buffers = [Some(layer_vertex_buffer_layout())];
                let vertex_buffers: &[Option<wgpu::VertexBufferLayout<'_>>] = match spec.draw {
                    SceneDraw::Fullscreen => &buffers,
                    SceneDraw::Layers(_) => &layer_buffers,
                };
                let blend = match spec.draw {
                    SceneDraw::Fullscreen => None,
                    SceneDraw::Layers(_) => Some(LAYER_BLEND_STATE),
                };
                let pipeline = build_pipeline(
                    device,
                    &pipeline_layout,
                    &shader,
                    spec.vertex_entry,
                    fragment,
                    target_format,
                    blend,
                    vertex_buffers,
                );
                // 单趟场景也需要 `@binding(1)` 有东西可绑。1×1 就够——着色器不读它，
                // 也不会把它当渲染目标。
                let placeholder = device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("dhampir scene unused source"),
                    size: wgpu::Extent3d {
                        width: 1,
                        height: 1,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: BLUR_INTERMEDIATE_FORMAT,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                    view_formats: &[],
                });
                let view = placeholder.create_view(&wgpu::TextureViewDescriptor::default());
                let group = create_bind_group(device, &bind_group_layout, &uniform, &view);
                (ScenePipeline::Single(pipeline), vec![view], vec![group])
            }
            ScenePasses::Blur {
                source,
                horizontal,
                vertical,
            } => {
                let buffers: [Option<wgpu::VertexBufferLayout<'_>>; 0] = [];
                let source_pipeline = build_pipeline(
                    device,
                    &pipeline_layout,
                    &shader,
                    spec.vertex_entry,
                    source,
                    BLUR_INTERMEDIATE_FORMAT,
                    None,
                    &buffers,
                );
                let horizontal_pipeline = build_pipeline(
                    device,
                    &pipeline_layout,
                    &shader,
                    spec.vertex_entry,
                    horizontal,
                    BLUR_INTERMEDIATE_FORMAT,
                    None,
                    &buffers,
                );
                let vertical_pipeline = build_pipeline(
                    device,
                    &pipeline_layout,
                    &shader,
                    spec.vertex_entry,
                    vertical,
                    target_format,
                    None,
                    &buffers,
                );
                let (a_view, b_view) = (
                    create_intermediate(device, size, "dhampir blur intermediate a"),
                    create_intermediate(device, size, "dhampir blur intermediate b"),
                );
                // 趟 1 写 A（读 B，虽然它不读）；趟 2 读 A 写 B；趟 3 读 B 写目标。
                let group_b = create_bind_group(device, &bind_group_layout, &uniform, &b_view);
                let group_a = create_bind_group(device, &bind_group_layout, &uniform, &a_view);
                let group_b2 = create_bind_group(device, &bind_group_layout, &uniform, &b_view);
                let pipeline = ScenePipeline::Blur {
                    source: source_pipeline,
                    horizontal: horizontal_pipeline,
                    vertical: vertical_pipeline,
                };
                (
                    pipeline,
                    vec![a_view, b_view],
                    vec![group_b, group_a, group_b2],
                )
            }
        };

        // 逐层顶点缓冲：每层三个顶点、颜色相同。
        let layers = match spec.draw {
            SceneDraw::Fullscreen => Vec::new(),
            SceneDraw::Layers(source) => source
                .iter()
                .map(|color| {
                    let vertex = LayerVertex {
                        color: [
                            color[0] as f32,
                            color[1] as f32,
                            color[2] as f32,
                            color[3] as f32,
                        ],
                    };
                    device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some("dhampir layer vertices"),
                        contents: bytemuck::cast_slice(&[vertex; 3]),
                        usage: wgpu::BufferUsages::VERTEX,
                    })
                })
                .collect::<Vec<_>>(),
        };

        Self {
            spec,
            size,
            format: target_format,
            uniform,
            pipeline,
            intermediate_views,
            bind_groups,
            layers,
        }
    }

    /// 实际渲染尺寸。**判定时用这个，不要用 [`SceneSpec::size`]**——
    /// 换过尺寸之后两者会不同，理由见 [`SceneRenderer::new_at`]。
    pub fn size(&self) -> (u32, u32) {
        self.size
    }

    /// 这个渲染器认的是哪个场景。给记录用：写进 `run.json` 的应当是**真的画了**
    /// 的那个场景，而不是调用方以为自己传进去的那个名字。
    pub fn spec(&self) -> &'static SceneSpec {
        self.spec
    }

    /// 构造时给的目标格式。
    ///
    /// 离屏读回（[`super::corpus::render_frame`]）按它建纹理：渲染器是按这个格式
    /// 建的管线，纹理用了别的格式就是"管线与附件不匹配"，而 wgpu 只会在
    /// 提交时才报出来。
    pub fn format(&self) -> wgpu::TextureFormat {
        self.format
    }

    /// 把这一帧画进 `target`。
    ///
    /// `frame` 只进 uniform：整数运算保证了两个宿主算出同一个平移量。
    pub fn render(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        queue: &wgpu::Queue,
        target: &wgpu::TextureView,
        frame: u32,
    ) {
        queue.write_buffer(
            &self.uniform,
            0,
            bytemuck::bytes_of(&Params::for_frame(self.size, frame)),
        );

        match &self.pipeline {
            ScenePipeline::Single(pipeline) => {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("dhampir scene pass"),
                    color_attachments: &[Some(color_attachment(target, self.spec.clear))],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
                pass.set_pipeline(pipeline);
                pass.set_bind_group(0, &self.bind_groups[0], &[]);
                self.draw_layers(&mut pass, frame);
            }
            ScenePipeline::Blur {
                source,
                horizontal,
                vertical,
            } => {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("dhampir blur source pass"),
                    color_attachments: &[Some(color_attachment(
                        &self.intermediate_views[0],
                        CLEAR_TRANSPARENT,
                    ))],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
                pass.set_pipeline(source);
                pass.set_bind_group(0, &self.bind_groups[0], &[]);
                pass.draw(0..3, 0..1);
                drop(pass);

                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("dhampir blur horizontal pass"),
                    color_attachments: &[Some(color_attachment(
                        &self.intermediate_views[1],
                        CLEAR_TRANSPARENT,
                    ))],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
                pass.set_pipeline(horizontal);
                pass.set_bind_group(0, &self.bind_groups[1], &[]);
                pass.draw(0..3, 0..1);
                drop(pass);

                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("dhampir blur vertical pass"),
                    color_attachments: &[Some(color_attachment(target, self.spec.clear))],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
                pass.set_pipeline(vertical);
                pass.set_bind_group(0, &self.bind_groups[2], &[]);
                pass.draw(0..3, 0..1);
            }
        }
    }

    /// 画内容：全屏三角形一次，或者逐层各一次（**顺序就是层序**）。
    fn draw_layers(&self, pass: &mut wgpu::RenderPass<'_>, frame: u32) {
        match self.spec.draw {
            SceneDraw::Fullscreen => pass.draw(0..3, 0..1),
            SceneDraw::Layers(_) => {
                let layers = (scene_model::layer_count(frame) as usize).min(self.layers.len());
                for buffer in self.layers.iter().take(layers) {
                    pass.set_vertex_buffer(0, buffer.slice(..));
                    pass.draw(0..3, 0..1);
                }
            }
        }
    }
}

/// 一个颜色附件。抽出来是因为它有四处调用点，而 `depth_slice: None` 这种
/// "必须显式写出来"的字段（wgpu 30 起必填）最容易在复制粘贴里漏掉一处。
fn color_attachment<'a>(
    target: &'a wgpu::TextureView,
    clear: wgpu::Color,
) -> wgpu::RenderPassColorAttachment<'a> {
    wgpu::RenderPassColorAttachment {
        view: target,
        depth_slice: None,
        resolve_target: None,
        ops: wgpu::Operations {
            load: wgpu::LoadOp::Clear(clear),
            store: wgpu::StoreOp::Store,
        },
    }
}

/// 建一张 `blur` 的中间纹理，返回它的视图。
///
/// 纹理本身不返回：视图握着它（见 [`SceneRenderer`] 的字段说明），返回所有权只会
/// 多一个没用的句柄。
fn create_intermediate(device: &wgpu::Device, size: (u32, u32), label: &str) -> wgpu::TextureView {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width: size.0,
            height: size.1,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: BLUR_INTERMEDIATE_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    texture.create_view(&wgpu::TextureViewDescriptor::default())
}

/// 建一个绑定组：`@binding(0)` 参数、`@binding(1)` 一张纹理。
fn create_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    uniform: &wgpu::Buffer,
    source: &wgpu::TextureView,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("dhampir scene bind group"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(source),
            },
        ],
    })
}

/// 建一条渲染管线。
#[allow(clippy::too_many_arguments)]
fn build_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    shader: &wgpu::ShaderModule,
    vertex_entry: &str,
    fragment_entry: &str,
    format: wgpu::TextureFormat,
    blend: Option<wgpu::BlendState>,
    vertex_buffers: &[Option<wgpu::VertexBufferLayout<'_>>],
) -> wgpu::RenderPipeline {
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("dhampir scene pipeline"),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some(vertex_entry),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            buffers: vertex_buffers,
        },
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some(fragment_entry),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            // 与探针同一个理由：不剔除任何面。绕序一致是 M2 该测的东西，
            // 不是这里该假设的东西。
            cull_mode: None,
            ..Default::default()
        },
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::wgsl_subset::{check_portable_subset, strip_wgsl_comments};
    use std::mem::offset_of;

    /// 每个场景的帧周期（`blur` 不看帧号，所以是 1）。
    ///
    /// 这张表不是"顺手写下的常数"：`the_declared_frame_spans_are_real_periods`
    /// 会走完一个周期再逐个采样点核对回到原样——周期说错了那条测试就红。
    fn frame_span(spec: &SceneSpec) -> u32 {
        match spec.name {
            "gradient" => scene_model::GRADIENT_SHIFT_PERIOD,
            "checker" => scene_model::CHECKER_CELL_PERIOD,
            "srgb_linear" => scene_model::TRANSFER_STRIPES,
            "alpha_stack" => scene_model::LAYER_COUNT_PERIOD,
            "blur" => 1,
            other => panic!("没有为场景 {other} 声明帧周期"),
        }
    }

    /// 从去注释后的着色器文本里读一趟的抽头：`(偏移, 权重字面量)`，按**文本顺序**。
    ///
    /// 读的是文本而不是运行结果：顺序与取值只存在于文本里，而这两件事都要被钉住。
    fn taps_of<'a>(code: &'a str, entry: &str) -> Vec<(i32, &'a str)> {
        let start = code
            .find(&format!("fn {entry}("))
            .expect("入口不在着色器里");
        let body = &code[start..];
        let body = &body[..body.find("\n}").expect("找不到函数结尾")];
        let marker = " * load_texel(texel";
        let mut taps = Vec::new();
        let mut rest = body;
        while let Some(at) = rest.find(marker) {
            let head = &rest[..at];
            // 权重是紧挨在 ` * load_texel` 前面的那个数字（前后都是空格或 `=`）。
            let begin = head
                .rfind(|c: char| !c.is_ascii_digit() && c != '.')
                .map_or(0, |index| index + 1);
            let after = &rest[at + marker.len()..];
            let offset = if let Some(tail) = after.strip_prefix(" + vec2<i32>(") {
                let close = tail.find(')').expect("抽头坐标没有右括号");
                let mut parts = tail[..close].split(',');
                let dx: i32 = parts
                    .next()
                    .expect("坐标少一个分量")
                    .trim()
                    .parse()
                    .expect("dx 不是整数");
                let dy: i32 = parts
                    .next()
                    .expect("坐标少一个分量")
                    .trim()
                    .parse()
                    .expect("dy 不是整数");
                if dx != 0 { dx } else { dy }
            } else if after.starts_with(',') {
                0 // 中心抽头写的是 `load_texel(texel, dim)`
            } else {
                panic!("{entry} 里出现了不认识的 load_texel 调用");
            };
            taps.push((offset, &head[begin..]));
            rest = after;
        }
        taps
    }

    /// **缺陷版**渐变：用 `floor(frag.x)`（少了半个像素）而不是像素中心。
    ///
    /// 故意写得与模型分开——缺陷模型必须独立于被测对象，否则它就只是"把某个开关
    /// 拨到另一边"，而不是"另一种可能的实现"。
    fn half_pixel_linear(spec: &SceneSpec, frame: u32, x: u32) -> [f64; 4] {
        let shift = (frame % scene_model::GRADIENT_SHIFT_PERIOD) as f64
            / scene_model::GRADIENT_SHIFT_PERIOD as f64;
        let t = scene_model::wrap01(x as f64 / spec.size.0 as f64 + shift);
        let _ = spec;
        [
            t,
            1.0 - t,
            scene_model::wrap01(t * scene_model::GRADIENT_SAWTOOTH_PERIODS),
            1.0,
        ]
    }

    /// **缺陷版**合成：颜色通道的源因子也写成 `One`——也就是误用
    /// `wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING`。逐层落回 8 位的那套量化
    /// 与模型保持一致，这样差出来的距离只归因于**混合方程**。
    fn premultiplied_color_bytes(frame: u32) -> [u8; 4] {
        let mut dst = [0u8; 4];
        for index in scene_model::stack_indices(frame, StackOrder::Forward) {
            let src = scene_model::LAYERS_LINEAR[index];
            let a = src[3];
            let back = [
                scene_model::linear_light_of_byte(dst[0]),
                scene_model::linear_light_of_byte(dst[1]),
                scene_model::linear_light_of_byte(dst[2]),
            ];
            dst = scene_model::bytes_of_linear_rgba([
                src[0] + back[0] * (1.0 - a),
                src[1] + back[1] * (1.0 - a),
                src[2] + back[2] * (1.0 - a),
                a + scene_model::linear_alpha_of_byte(dst[3]) * (1.0 - a),
            ]);
        }
        dst
    }

    // -----------------------------------------------------------------------
    // 声明层：布局 / 注册表 / 混合状态 / 入口名
    // -----------------------------------------------------------------------

    #[test]
    fn params_layout_matches_the_wgsl_struct() {
        // uniform 的布局错了不会有编译错误，只会让画面整个不对——所以逐个偏移钉住，
        // 连 WGSL 侧的字段**顺序**也读出来比一遍。
        assert_eq!(size_of::<Params>(), 16);
        assert_eq!(offset_of!(Params, size), 0);
        assert_eq!(offset_of!(Params, frame), 8);
        assert_eq!(offset_of!(Params, pad), 12);
        assert_eq!(Params::SIZE, 16);
        assert_eq!(
            Params::for_frame((256, 256), 7),
            Params {
                size: [256.0, 256.0],
                frame: 7,
                pad: 0,
            }
        );

        let code = strip_wgsl_comments(SCENE_WGSL);
        let start = code.find("struct Params {").expect("着色器里没有 Params");
        let body = &code[start..start + code[start..].find('}').expect("Params 没有右花括号")];
        let fields: Vec<&str> = body
            .lines()
            .skip(1)
            .filter_map(|line| line.trim().split(':').next())
            .filter(|name| !name.is_empty())
            .collect();
        assert_eq!(
            fields,
            vec!["size", "frame", "pad"],
            "Params 的字段顺序与 Rust 侧不一致（uniform 偏移会全变，而画面只会「整个不对」）"
        );
    }

    #[test]
    fn layer_vertex_layout_declares_one_vec4() {
        assert_eq!(size_of::<LayerVertex>(), 16);
        assert_eq!(LayerVertex::STRIDE, 16);
        let layout = layer_vertex_buffer_layout();
        // 跨度与 `size_of` 必须同一个来源：手写 16 的写法在结构体长胖之后会静默地
        // 把第二个顶点读成第一个顶点的一半。
        assert_eq!(layout.array_stride, LayerVertex::STRIDE);
        assert_eq!(layout.step_mode, wgpu::VertexStepMode::Vertex);
        assert_eq!(layout.attributes.len(), 1);
        assert_eq!(layout.attributes[0].format, wgpu::VertexFormat::Float32x4);
        assert_eq!(layout.attributes[0].offset, 0);
        assert_eq!(layout.attributes[0].shader_location, 0);
    }

    #[test]
    fn blend_state_is_alpha_blending_and_not_the_premultiplied_one() {
        // 逐字段写出来，是为了让"正在被考的是哪条式子"看得见；这条断言保证手写的
        // 那份恰好等于标准常量——任何一个字段偏了都会红。
        assert_eq!(LAYER_BLEND_STATE, wgpu::BlendState::ALPHA_BLENDING);
        assert_eq!(
            LAYER_BLEND_STATE.color.src_factor,
            wgpu::BlendFactor::SrcAlpha
        );
        assert_eq!(
            LAYER_BLEND_STATE.color.dst_factor,
            wgpu::BlendFactor::OneMinusSrcAlpha
        );
        assert_eq!(LAYER_BLEND_STATE.color.operation, wgpu::BlendOperation::Add);
        assert_eq!(LAYER_BLEND_STATE.alpha.src_factor, wgpu::BlendFactor::One);
        assert_eq!(
            LAYER_BLEND_STATE.alpha.dst_factor,
            wgpu::BlendFactor::OneMinusSrcAlpha
        );
        assert_eq!(LAYER_BLEND_STATE.alpha.operation, wgpu::BlendOperation::Add);
        // 两个常量**只有颜色通道不同**：这正是"alpha 看着对、颜色偏亮"的来源。
        assert_ne!(
            LAYER_BLEND_STATE.color,
            wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING.color
        );
        assert_eq!(
            LAYER_BLEND_STATE.alpha,
            wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING.alpha
        );
    }

    #[test]
    fn alpha_stack_model_is_the_blend_equation() {
        // 模型里那段折叠与 LAYER_BLEND_STATE 必须是同一条式子——所以在这里按那两个
        // `BlendComponent` 的因子**现场再折一遍**：颜色用 `src_a` / `1 - src_a`，
        // alpha 用 `1` / `1 - src_a`。
        for frame in 0..scene_model::LAYER_COUNT_PERIOD {
            let mut folded = [0.0_f64; 4]; // 底数 = CLEAR_TRANSPARENT
            for index in scene_model::stack_indices(frame, StackOrder::Forward) {
                let src = scene_model::LAYERS_LINEAR[index];
                let a = src[3];
                folded = [
                    src[0] * a + folded[0] * (1.0 - a),
                    src[1] * a + folded[1] * (1.0 - a),
                    src[2] * a + folded[2] * (1.0 - a),
                    a + folded[3] * (1.0 - a),
                ];
            }
            let ideal = scene_model::alpha_stack_ideal(frame, StackOrder::Forward);
            for (channel, (mine, model)) in folded.iter().zip(ideal.iter()).enumerate() {
                assert!(
                    (mine - model).abs() < 1e-12,
                    "第 {frame} 帧通道 {channel}：现场折叠 {mine} 与模型 {model} 对不上"
                );
            }
            assert_eq!(
                scene_model::bytes_of_linear_rgba(folded),
                scene_model::bytes_of_linear_rgba(ideal)
            );
        }
    }

    #[test]
    fn scene_registry_names_and_order_are_pinned() {
        // 顺序 = 记录顺序：改了顺序，`records/` 里按场景分节的表就会前后挪位。
        assert_eq!(
            scene_names(),
            vec!["gradient", "checker", "srgb_linear", "alpha_stack", "blur"]
        );
        for spec in SELECTABLE_SCENES.iter() {
            let found = scene_by_name(spec.name).expect("注册表里的名字必须查得到");
            assert!(
                std::ptr::eq(found, spec),
                "{} 查到的不是同一个 spec",
                spec.name
            );
            assert!(!spec.description.is_empty(), "{} 没有说明", spec.name);
            assert_eq!(
                spec.size, SCENE_TARGET_SIZE,
                "{} 的尺寸不是 corpus 尺寸",
                spec.name
            );
        }
        assert!(scene_by_name("Gradient").is_none(), "名字区分大小写");
        assert!(scene_by_name("").is_none());
        assert!(scene_by_name("blur ").is_none(), "尾随空格不该被容忍");
    }

    #[test]
    fn only_blur_ignores_the_frame() {
        // `uses_frame` 不是备注，是"这个场景考不考帧号"的声明：帧号的确定性由其余
        // 四个场景承担，`blur` 用它换掉了"采样点落在图案哪一格"这个变量。
        let ignores: Vec<&str> = SELECTABLE_SCENES
            .iter()
            .filter(|spec| !spec.uses_frame)
            .map(|spec| spec.name)
            .collect();
        assert_eq!(ignores, vec!["blur"]);
    }

    #[test]
    fn pass_count_and_draw_kind_agree_with_the_entry_list() {
        for spec in SELECTABLE_SCENES.iter() {
            let entries = spec.fragment_entries();
            assert_eq!(
                entries.len() as u32,
                spec.pass_count(),
                "{} 的趟数与入口数对不上",
                spec.name
            );
            assert_eq!(
                spec.pass_count(),
                if matches!(spec.passes, ScenePasses::Blur { .. }) {
                    3
                } else {
                    1
                }
            );
            assert_eq!(
                matches!(spec.draw, SceneDraw::Layers(_)),
                spec.name == "alpha_stack",
                "只有 alpha_stack 逐层绘制"
            );
            if let SceneDraw::Layers(colors) = spec.draw {
                // 顶点数据与模型必须同源：各写一份的话，"模型预测的是硬件画的那个东西"
                // 这句话就不成立了，而且两份色值长得还很像。
                assert_eq!(colors.len(), scene_model::LAYERS_LINEAR.len());
                assert_eq!(colors, &scene_model::LAYERS_LINEAR[..]);
            }
            for entry in entries {
                assert!(entry.starts_with("fs_"), "{entry} 不像片元入口的名字");
            }
        }
    }

    #[test]
    fn every_entry_point_the_registry_names_exists_in_the_shader() {
        let code = check_portable_subset("corpus 场景 WGSL", SCENE_WGSL);
        // 入口名只在两处出现：注册表与着色器。对不上时 WGSL 照样能编过（入口是字符串），
        // 错要到建管线时才炸出来。
        for spec in SELECTABLE_SCENES.iter() {
            assert!(
                code.contains(&format!("fn {}(", spec.vertex_entry)),
                "{} 的顶点入口不在着色器里",
                spec.name
            );
            for entry in spec.fragment_entries() {
                assert!(
                    code.contains(&format!("fn {entry}(")),
                    "{} 的片元入口 {entry} 不在着色器里",
                    spec.name
                );
            }
        }
        // 反向：着色器里**只有**这些入口。多一个没人用的入口，就是一段没人测的代码。
        let named = [
            "vs_fullscreen",
            "vs_layer",
            "fs_gradient",
            "fs_checker",
            "fs_srgb_linear",
            "fs_layer",
            "fs_blur_source",
            "fs_blur_h",
            "fs_blur_v",
        ];
        for name in named {
            assert_eq!(
                code.matches(&format!("fn {name}(")).count(),
                1,
                "{name} 在着色器里出现了一次以上或一次都没有"
            );
        }
        assert_eq!(code.matches("@vertex").count(), 2, "顶点入口数变了");
        assert_eq!(
            code.matches("@fragment").count(),
            7,
            "片元入口数变了（有没人用的入口，或者有入口没被注册表声明）"
        );
    }

    #[test]
    fn blur_taps_in_the_shader_are_the_models_weights() {
        let code = strip_wgsl_comments(SCENE_WGSL);
        for entry in ["fs_blur_h", "fs_blur_v"] {
            let taps = taps_of(&code, entry);
            assert_eq!(taps.len(), 7, "{entry} 的抽头不是 7 个");
            assert_eq!(
                taps.iter().map(|(offset, _)| *offset).collect::<Vec<_>>(),
                vec![-3, -2, -1, 0, 1, 2, 3],
                "{entry} 的抽头偏移不对称（σ = 1.5 的核是对称的）"
            );
            for (offset, weight) in &taps {
                let model = scene_model::BLUR_WEIGHTS[offset.unsigned_abs() as usize];
                assert_eq!(
                    *weight,
                    format!("{model:.6}").as_str(),
                    "{entry} 在偏移 {offset} 处的权重字面量与模型对不上"
                );
            }
            let sum: f64 = taps
                .iter()
                .map(|(_, weight)| weight.parse::<f64>().expect("权重不是字面量"))
                .sum();
            assert!(
                (sum - 1.0).abs() < 1e-9,
                "{entry} 的七个权重之和是 {sum}，不是 1"
            );
        }
    }

    #[test]
    fn documented_sample_coordinates_are_the_real_ones() {
        // 这些坐标会带着名字写进 `records/`，被长期留存、跨版本比较——坐标悄悄变了，
        // 历史记录就失去可比性，而这种变化不会有任何症状。
        let table: Vec<(&str, Vec<(u32, u32)>)> = SELECTABLE_SCENES
            .iter()
            .map(|spec| (spec.name, spec.samples.iter().map(|p| (p.x, p.y)).collect()))
            .collect();
        assert_eq!(
            table,
            vec![
                (
                    "gradient",
                    vec![(8, 128), (32, 128), (48, 128), (104, 128), (200, 128)]
                ),
                (
                    "checker",
                    vec![(3, 1), (4, 1), (7, 1), (8, 1), (11, 1), (12, 1), (255, 255)]
                ),
                (
                    "srgb_linear",
                    vec![(16, 128), (80, 128), (144, 128), (240, 128)]
                ),
                ("alpha_stack", vec![(128, 128), (8, 248)]),
                (
                    "blur",
                    vec![(19, 19), (35, 27), (38, 41), (41, 41), (0, 41)]
                ),
            ]
        );
    }

    #[test]
    fn sample_points_are_static_unique_and_inside_the_target() {
        for spec in SELECTABLE_SCENES.iter() {
            assert!(
                spec.samples.len() >= 2,
                "{} 只有一个采样点：一个点分不清「值对了」与「整屏都是这个值」",
                spec.name
            );
            let mut labels: Vec<&str> = Vec::new();
            for point in spec.samples {
                assert!(
                    point.x < spec.size.0 && point.y < spec.size.1,
                    "{} 的采样点 ({}, {}) 出了画面",
                    spec.name,
                    point.x,
                    point.y
                );
                assert!(
                    !point.label.is_empty() && !point.purpose.is_empty(),
                    "{} 有采样点没写名字或理由",
                    spec.name
                );
                assert!(
                    !labels.contains(&point.label),
                    "{} 里标签 {} 重了",
                    spec.name,
                    point.label
                );
                labels.push(point.label);
            }
        }
    }

    // -----------------------------------------------------------------------
    // 判据：模型预测表 / 缺陷距离 / 判定函数
    // -----------------------------------------------------------------------

    /// 帧 0..3（`blur` 只有一帧）的全部采样点预测值，由 `records/m1/model-cross-check.mjs`
    /// 独立算出。三元组是 `(场景, 帧, 采样点下标)`。
    #[rustfmt::skip]
    const PINNED: &[(&str, u32, usize, [u8; 4])] = &[
        ("gradient", 0, 0, [51, 251, 141, 255]),
        ("gradient", 0, 1, [100, 240, 34, 255]),
        ("gradient", 0, 2, [120, 232, 190, 255]),
        ("gradient", 0, 3, [171, 202, 141, 255]),
        ("gradient", 0, 4, [229, 128, 141, 255]),
        ("gradient", 1, 0, [87, 244, 227, 255]),
        ("gradient", 1, 1, [120, 232, 190, 255]),
        ("gradient", 1, 2, [137, 224, 34, 255]),
        ("gradient", 1, 3, [183, 192, 227, 255]),
        ("gradient", 1, 4, [237, 109, 227, 255]),
        ("gradient", 2, 0, [111, 236, 141, 255]),
        ("gradient", 2, 1, [137, 224, 34, 255]),
        ("gradient", 2, 2, [152, 216, 190, 255]),
        ("gradient", 2, 3, [193, 182, 141, 255]),
        ("gradient", 2, 4, [244, 85, 141, 255]),
        ("gradient", 3, 0, [129, 228, 227, 255]),
        ("gradient", 3, 1, [152, 216, 190, 255]),
        ("gradient", 3, 2, [165, 207, 34, 255]),
        ("gradient", 3, 3, [203, 170, 227, 255]),
        ("gradient", 3, 4, [252, 48, 227, 255]),
        ("checker", 0, 0, [237, 108, 160, 255]),
        ("checker", 0, 1, [63, 196, 249, 255]),
        ("checker", 0, 2, [63, 196, 249, 255]),
        ("checker", 0, 3, [237, 108, 160, 255]),
        ("checker", 0, 4, [237, 108, 160, 255]),
        ("checker", 0, 5, [63, 196, 249, 255]),
        ("checker", 0, 6, [237, 108, 160, 255]),
        ("checker", 1, 0, [237, 108, 160, 255]),
        ("checker", 1, 1, [237, 108, 160, 255]),
        ("checker", 1, 2, [237, 108, 160, 255]),
        ("checker", 1, 3, [63, 196, 249, 255]),
        ("checker", 1, 4, [63, 196, 249, 255]),
        ("checker", 1, 5, [63, 196, 249, 255]),
        ("checker", 1, 6, [237, 108, 160, 255]),
        ("checker", 2, 0, [237, 108, 160, 255]),
        ("checker", 2, 1, [237, 108, 160, 255]),
        ("checker", 2, 2, [237, 108, 160, 255]),
        ("checker", 2, 3, [237, 108, 160, 255]),
        ("checker", 2, 4, [237, 108, 160, 255]),
        ("checker", 2, 5, [63, 196, 249, 255]),
        ("checker", 2, 6, [237, 108, 160, 255]),
        ("srgb_linear", 0, 0, [0, 0, 0, 255]),
        ("srgb_linear", 0, 1, [64, 64, 64, 255]),
        ("srgb_linear", 0, 2, [128, 128, 128, 255]),
        ("srgb_linear", 0, 3, [223, 223, 223, 255]),
        ("srgb_linear", 1, 0, [32, 32, 32, 255]),
        ("srgb_linear", 1, 1, [96, 96, 96, 255]),
        ("srgb_linear", 1, 2, [159, 159, 159, 255]),
        ("srgb_linear", 1, 3, [0, 0, 0, 255]),
        ("srgb_linear", 2, 0, [64, 64, 64, 255]),
        ("srgb_linear", 2, 1, [128, 128, 128, 255]),
        ("srgb_linear", 2, 2, [191, 191, 191, 255]),
        ("srgb_linear", 2, 3, [32, 32, 32, 255]),
        ("srgb_linear", 3, 0, [96, 96, 96, 255]),
        ("srgb_linear", 3, 1, [159, 159, 159, 255]),
        ("srgb_linear", 3, 2, [223, 223, 223, 255]),
        ("srgb_linear", 3, 3, [64, 64, 64, 255]),
        ("alpha_stack", 0, 0, [137, 165, 99, 192]),
        ("alpha_stack", 0, 1, [137, 165, 99, 192]),
        ("alpha_stack", 1, 0, [143, 159, 147, 208]),
        ("alpha_stack", 1, 1, [143, 159, 147, 208]),
        ("alpha_stack", 2, 0, [208, 199, 107, 243]),
        ("alpha_stack", 2, 1, [208, 199, 107, 243]),
        ("alpha_stack", 3, 0, [168, 189, 164, 249]),
        ("alpha_stack", 3, 1, [168, 189, 164, 249]),
        ("blur", 0, 0, [0, 0, 0, 255]),
        ("blur", 0, 1, [203, 203, 203, 255]),
        ("blur", 0, 2, [196, 196, 196, 255]),
        ("blur", 0, 3, [210, 210, 210, 255]),
        ("blur", 0, 4, [189, 189, 189, 255]),
    ];

    /// 整张预测表（每个场景走完一个周期 × 每个采样点 × 四个通道）的 FNV-1a 64。
    ///
    /// 序列化方式：按 [`SELECTABLE_SCENES`] 的顺序 → 帧 0..周期 → 采样点顺序 → 通道 0..4，
    /// 逐字节喂进 FNV-1a（offset basis `0xcbf29ce484222325`、prime `0x100000001b3`）。
    const PREDICTION_TABLE_HASH: &str = "fff8d28ff54c24d8";

    #[test]
    fn pinned_predictions_match_the_independent_reimplementation() {
        // 这张表由**另一份实现**算出（`records/m1/model-cross-check.mjs`，JS：同一套
        // 公式，独立写的一遍）。只钉 Rust 自己的输出，证明不了模型是对的——两份实现
        // 算出同一批数才算。表里的 `frame` 只取 0..3，整周期由下面的摘要覆盖。
        for &(scene, frame, index, bytes) in PINNED {
            let spec = scene_by_name(scene).unwrap_or_else(|| panic!("没有场景 {scene}"));
            let point = spec.samples[index];
            assert_eq!(
                expected_bytes(spec, frame, point),
                bytes,
                "{scene} f{frame} 第 {index} 个采样点（{}）与独立实现算出的不一样",
                point.label
            );
        }
        // 表要真的盖满，不然"全对"可能只是因为它几乎什么都没盖：
        for spec in SELECTABLE_SCENES.iter() {
            for frame in 0..4.min(frame_span(spec)) {
                let rows = PINNED
                    .iter()
                    .filter(|(scene, row_frame, _, _)| *scene == spec.name && *row_frame == frame)
                    .count();
                assert_eq!(
                    rows,
                    spec.samples.len(),
                    "{} f{frame} 在表里只有 {rows} 行",
                    spec.name
                );
            }
        }
    }

    #[test]
    fn the_whole_prediction_table_hashes_to_the_pinned_digest() {
        // 逐点断言只覆盖少数帧；这张摘要把**整周期 × 每个采样点 × 四个通道**压成一个
        // 数（序列化方式见 `PREDICTION_TABLE_HASH` 的文档）。表变了它会变——这是
        // "改模型必须是有意为之"的闸门，也是跨版本比较预测表的最短形式。
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for spec in SELECTABLE_SCENES.iter() {
            for frame in 0..frame_span(spec) {
                for point in spec.samples {
                    for byte in expected_bytes(spec, frame, *point) {
                        hash ^= byte as u64;
                        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
                    }
                }
            }
        }
        assert_eq!(
            format!("{hash:016x}"),
            PREDICTION_TABLE_HASH,
            "整张预测表的摘要变了：模型被改动了，而 records/ 里的预测值还是旧的"
        );
    }

    #[test]
    fn the_declared_frame_spans_are_real_periods() {
        // "周期"不能只是嘴上说说：走完一个周期必须回到原样，逐点逐字节核对。
        for spec in SELECTABLE_SCENES.iter() {
            let span = frame_span(spec);
            for point in spec.samples {
                assert_eq!(
                    expected_bytes(spec, 0, *point),
                    expected_bytes(spec, span, *point),
                    "{} 走完 {span} 帧没有回到原样",
                    spec.name
                );
            }
        }
    }

    #[test]
    fn tolerance_bites_the_defects_we_care_about() {
        // 容差 1 的唯一正当理由是"规范允许实现的 sRGB 编码近似"（见模块文档那张表）。
        // 这条测试算的是另一半：我们真正想抓的缺陷离模型有多远。每个数都由模型自己
        // 算出来，一个字都不抄外部记录——不然判据和被测对象就是同一个来源。
        //
        // 每一项都问"整张表里**最近**的那个样本差多少"：判据不是"某些样本差得远",
        // 而是"没有哪个样本能贴着容差混过去"。

        // ① 通道置换（r ↔ b）。**只在 r 与 b 本来就不相等的样本上算**：`srgb_linear` 与
        //    `blur` 整张图都是灰的，交换这两个通道是恒等映射——那不是容差漏了，而是那两张
        //    图里根本不含这份信息（`srgb_linear` 要抓的缺陷是 ② 那个"漏一次解码"）。
        //    剩下的场景里最近的样本是 gradient 第 4 帧 x = 8：斜坡与锯齿碰巧交叉
        //    （145 对 141）；alpha_stack 上也有 4，checker 上最近的 77。
        let mut permutation: Vec<(u8, &str, u32, &str)> = Vec::new();
        let mut achromatic: Vec<&str> = Vec::new();
        for spec in SELECTABLE_SCENES.iter() {
            let mut can_see_it = false;
            for frame in 0..frame_span(spec) {
                for point in spec.samples {
                    let bytes = expected_bytes(spec, frame, *point);
                    if bytes[0] == bytes[2] {
                        continue;
                    }
                    can_see_it = true;
                    let swapped = [bytes[2], bytes[1], bytes[0], bytes[3]];
                    permutation.push((
                        scene_model::distance_bytes(bytes, swapped),
                        spec.name,
                        frame,
                        point.label,
                    ));
                }
            }
            if !can_see_it {
                achromatic.push(spec.name);
            }
        }
        assert_eq!(
            achromatic,
            vec!["srgb_linear", "blur"],
            "看不见通道置换的场景变了：这两张灰图必须在，其它场景必须能看见"
        );
        for name in &achromatic {
            let spec = scene_by_name(name).expect("名字来自注册表");
            for frame in 0..frame_span(spec) {
                for point in spec.samples {
                    let bytes = expected_bytes(spec, frame, *point);
                    assert!(
                        bytes[0] == bytes[1] && bytes[1] == bytes[2],
                        "{name} 整张表都看不见通道置换，但 {} 不是灰的",
                        point.label
                    );
                }
            }
        }
        let closest = permutation
            .iter()
            .min_by_key(|(distance, ..)| *distance)
            .expect("渐变、棋盘、层叠这三张彩色图必须至少给出一个样本");
        assert_eq!(
            closest.0, 4,
            "通道置换最近的距离从 4 字节变了（{} f{} {}）",
            closest.1, closest.2, closest.3
        );
        assert!(closest.0 > BYTE_TOLERANCE);

        // ② 漏一次解码：把竖条的 sRGB 电平（k/8）当线性值输出。
        let srgb = scene_by_name("srgb_linear").expect("srgb_linear 必须在");
        let mut missing: Vec<(u8, u32, &str)> = Vec::new();
        for frame in 0..frame_span(srgb) {
            for point in srgb.samples {
                let k = scene_model::transfer_stripe_at(srgb.size, frame, point.x);
                let level = k as f64 / scene_model::TRANSFER_STRIPES as f64;
                let defect = scene_model::bytes_of_linear_rgba([level, level, level, 1.0]);
                missing.push((
                    scene_model::distance_bytes(expected_bytes(srgb, frame, *point), defect),
                    frame,
                    point.label,
                ));
            }
        }
        // k = 0 的样本抓不到这个缺陷（0 解码前后都是 0）。那是那个采样点的职责决定的：
        // 它管的是"线性段公式"与"没渲染"的区分。把抓不到的样本数一起钉住，
        // 免得"最近距离"哪天被这种样本悄悄拉低。
        assert_eq!(missing.iter().filter(|(d, ..)| *d == 0).count(), 4);
        let closest = missing
            .iter()
            .filter(|(d, ..)| *d > 0)
            .min_by_key(|(distance, ..)| *distance)
            .expect("总会有 k > 0 的样本");
        assert_eq!(
            closest.0, 17,
            "漏一次解码最近的距离变了（f{} {}）",
            closest.1, closest.2
        );

        // ③ 半像素相位（`floor(frag.x)` 与像素中心的差）。这一项最有说头：抓住它的是
        //    b 通道那条周期 32 像素的锯齿——**每个样本**都差 2 字节以上，整张表最近的
        //    也就是这个 2。而 r 通道那条斜坡几乎全程看不见它：只有 5 个样本能单独超过
        //    容差，其余 ≤ 1 字节。理由是半个像素只有 1/512 那么大，斜坡的可见度全看该处
        //    sRGB 编码的斜率——这 5 个样本都是斜坡绕回 0 之后的那几个像素，t 落进斜率
        //    12.92 的线性段，半个像素被放大成 6 个字节，剩下的只差 2。g 通道（`1 - t`）
        //    更钝：t 贴近 1 处斜率最小，全程 ≤ 1 字节。这就是 gradient 里
        //    `frac(t * 8)` 存在的理由。
        let gradient = scene_by_name("gradient").expect("gradient 必须在");
        let mut sawtooth: Vec<u8> = Vec::new();
        let mut ramp_worst = 0u8;
        let mut flat_worst = 0u8;
        let mut ramp_sees = 0usize;
        let mut saw_sees = 0usize;
        let mut watched = 0usize;
        for frame in 0..frame_span(gradient) {
            for point in gradient.samples {
                let bytes = expected_bytes(gradient, frame, *point);
                let defect =
                    scene_model::bytes_of_linear_rgba(half_pixel_linear(gradient, frame, point.x));
                watched += 1;
                sawtooth.push(scene_model::distance_bytes(bytes, defect));
                let ramp = bytes[0].abs_diff(defect[0]);
                let flat = bytes[1].abs_diff(defect[1]);
                let saw = bytes[2].abs_diff(defect[2]);
                ramp_worst = ramp_worst.max(ramp);
                flat_worst = flat_worst.max(flat);
                if ramp > BYTE_TOLERANCE {
                    ramp_sees += 1;
                }
                if saw > BYTE_TOLERANCE {
                    saw_sees += 1;
                }
            }
        }
        assert_eq!(
            *sawtooth.iter().min().expect("采样点不会一个都没有"),
            2,
            "半像素相位最近的样本从 2 字节变了：锯齿通道是它为 2 的来源"
        );
        assert_eq!(saw_sees, watched, "锯齿通道没能逐个样本抓住半像素相位");
        assert_eq!(ramp_sees, 5, "斜坡通道单独看得见半像素相位的样本数变了");
        assert_eq!(
            ramp_worst, 6,
            "斜坡通道最差的距离变了（绕回 0 之后落进线性段）"
        );
        assert!(
            flat_worst <= BYTE_TOLERANCE,
            "`1 - t` 通道也超过容差了（{flat_worst} 字节）"
        );

        // ④ 盒式滤波替代高斯。平坦区的两个样本抓不到它（盒子与高斯在那里都等于原值：
        //    一个是空白格、一个是满格，7×7 邻域里全是同一个值）。
        let blur = scene_by_name("blur").expect("blur 必须在");
        let mut boxy: Vec<(u8, &str)> = blur
            .samples
            .iter()
            .map(|point| {
                let gaussian = scene_model::blur_at(blur.size, point.x, point.y);
                let mean = scene_model::box_blur_at(blur.size, point.x, point.y);
                (
                    scene_model::distance_bytes(
                        scene_model::bytes_of_linear_rgba([gaussian, gaussian, gaussian, 1.0]),
                        scene_model::bytes_of_linear_rgba([mean, mean, mean, 1.0]),
                    ),
                    point.label,
                )
            })
            .collect();
        assert_eq!(boxy.iter().filter(|(d, _)| *d == 0).count(), 2);
        boxy.sort_by_key(|(distance, _)| *distance);
        assert_eq!(
            boxy.iter()
                .find(|(d, _)| *d > 0)
                .expect("总会有非平坦的样本")
                .0,
            12,
            "盒式滤波最**近**的（能看见的）样本差变了"
        );
        assert!(
            boxy.iter()
                .filter(|(d, _)| *d > 0)
                .all(|(d, _)| *d > BYTE_TOLERANCE)
        );
        // 最远的是高光块里那个样本：1.0 的脉冲在 7×7 均值里被摊平，而高斯给它最大的权重。
        assert_eq!(
            boxy.last().expect("五点都在").0,
            35,
            "盒式滤波最远的样本差变了（高光块里那个）"
        );

        // ⑤ 层序反了。
        let alpha = scene_by_name("alpha_stack").expect("alpha_stack 必须在");
        let orders: Vec<(u8, u32)> = (0..frame_span(alpha))
            .map(|frame| {
                (
                    scene_model::distance_bytes(
                        scene_model::alpha_stack_bytes(frame, StackOrder::Forward),
                        scene_model::alpha_stack_bytes(frame, StackOrder::Reverse),
                    ),
                    frame,
                )
            })
            .collect();
        let closest = orders
            .iter()
            .min_by_key(|(distance, _)| *distance)
            .expect("四帧都在");
        assert!(
            closest.0 >= 37,
            "层序反了在 f{} 只差 {} 字节",
            closest.1,
            closest.0
        );

        // ⑥ 混合状态误用 `PREMULTIPLIED_ALPHA_BLENDING`。它的症状值得单独钉住：
        //    **alpha 通道完全一样**，只有颜色通道偏——"alpha 看着对"正是它难查的原因。
        for frame in 0..frame_span(alpha) {
            let correct = expected_bytes(alpha, frame, alpha.samples[0]);
            let broken = premultiplied_color_bytes(frame);
            assert_eq!(
                correct[3], broken[3],
                "用错常量不该改变 alpha 通道——这正是它难查的原因"
            );
            assert!(
                scene_model::distance_bytes(correct, broken) >= 52,
                "第 {frame} 帧用错混合状态只差 {} 字节",
                scene_model::distance_bytes(correct, broken)
            );
        }
    }

    #[test]
    fn the_left_edge_point_is_the_only_one_that_can_see_the_edge_semantics() {
        // 边界语义（clamp / 零填充 / 环绕）只在贴着边界的采样点上有区别。这条测试
        // 把"哪几个点看得见"变成断言：其余四个点在内陆，三种语义给的是同一个数——
        // 也就是说，删掉左边缘那个点，这一整类缺陷就再也测不出来了。
        let blur = scene_by_name("blur").expect("blur 必须在");
        let mut witness: Vec<(&str, u8, u8)> = Vec::new();
        for point in blur.samples {
            let clamp =
                scene_model::bytes_of_linear_rgba(edge_bytes(blur, *point, EdgeMode::Clamp));
            let zero = scene_model::bytes_of_linear_rgba(edge_bytes(blur, *point, EdgeMode::Zero));
            let wrap = scene_model::bytes_of_linear_rgba(edge_bytes(blur, *point, EdgeMode::Wrap));
            let to_zero = scene_model::distance_bytes(clamp, zero);
            let to_wrap = scene_model::distance_bytes(clamp, wrap);
            if to_zero > BYTE_TOLERANCE || to_wrap > BYTE_TOLERANCE {
                witness.push((point.label, to_zero, to_wrap));
            }
        }
        assert_eq!(
            witness.len(),
            1,
            "看得见边界语义的采样点不止一个或多于一个：{witness:?}"
        );
        let (label, to_zero, to_wrap) = witness[0];
        assert_eq!(label, "左边缘");
        assert!(
            to_zero >= 20 && to_wrap >= 20,
            "距离缩到 {to_zero} / {to_wrap} 了，边界语义要抓不住了"
        );
    }

    #[test]
    fn judge_sample_passes_within_tolerance_and_fails_beyond_it() {
        let spec = scene_by_name("gradient").expect("gradient 必须在");
        let point = spec.samples[0];
        let expected = expected_bytes(spec, 0, point);

        let exact = judge_sample(spec, 0, point, expected);
        assert!(exact.passed);
        assert_eq!(exact.distance, 0);
        assert_eq!(exact.expected, expected);
        assert_eq!(exact.tolerance, BYTE_TOLERANCE);
        assert!(
            exact.detail.contains("模型线性"),
            "说明里要能读出模型算的是什么"
        );

        // 逐通道偏 1（容差内）与偏 2（容差外）。跳过会溢出 255 的通道——
        // 那里 `saturating_add` 不生效，偏 1 会变成"没偏"。
        for channel in 0..4 {
            for delta in 1u8..=2 {
                if expected[channel] as u16 + delta as u16 > 255 {
                    continue;
                }
                let mut measured = expected;
                measured[channel] += delta;
                let verdict = judge_sample(spec, 0, point, measured);
                assert_eq!(
                    verdict.passed,
                    delta <= BYTE_TOLERANCE,
                    "通道 {channel} 偏 {delta} 字节的判定不对"
                );
                assert_eq!(verdict.distance, delta);
            }
        }

        let line = exact.report_line(spec, 0, point, expected);
        assert!(line.contains("gradient"), "记录行里要带场景名：{line}");
        assert!(line.contains("f0"), "记录行里要带帧号：{line}");
        assert!(line.contains(point.label), "记录行里要带采样点标签：{line}");
        assert!(line.contains("通过"), "记录行里要带结论：{line}");
    }

    #[test]
    fn an_unknown_scene_name_is_a_programming_error_not_a_zero() {
        // `expected_bytes` 对没写模型的名字直接 panic：返回全 0 会让"读数全对"变成
        // 恒真的假象——CLI 加了个新场景却忘了写模型，正好会走进这里。
        let bogus = SceneSpec {
            name: "nope",
            description: "没有模型的场景",
            size: SCENE_TARGET_SIZE,
            clear: CLEAR_OPAQUE_BLACK,
            vertex_entry: "vs_fullscreen",
            passes: ScenePasses::Single {
                fragment: "fs_gradient",
            },
            draw: SceneDraw::Fullscreen,
            uses_frame: true,
            samples: &GRADIENT_SAMPLES,
        };
        let point = bogus.samples[0];
        let panicked = std::panic::catch_unwind(|| expected_bytes(&bogus, 0, point)).is_err();
        assert!(panicked, "未知场景名没有让模型预测 panic");
        let panicked =
            std::panic::catch_unwind(|| judge_sample(&bogus, 0, point, [0, 0, 0, 0])).is_err();
        assert!(panicked, "未知场景名没有让判据 panic");
    }
}
