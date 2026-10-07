//! corpus 场景的**纯数值模型**：一堆 `f64` 算术，不碰 GPU、不碰 `wgpu`。
//!
//! 与 `render/scene.rs` 的分工：那里写"**声明**"（场景注册表、入口点名、采样表、
//! 判据、容差），这里写"**应当算出什么**"的独立答案。
//!
//! # 为什么需要一份独立答案
//!
//! M2 的核心动作是"同一份 WGSL，两个运行时渲染同一帧，比较结果"。如果只有
//! 两端互相比，不一致时能说的只有"它们不一样"——说不出**谁对**，也说不清
//! 差的是插值、是传输函数、还是多趟的顺序。有了模型，差异立刻落到具体的一条上：
//! "blur 的边缘采样点读的是 154，模型按 clamp 给的是 189，按 zero 给的是 154
//! —— 边界语义变成 zero 了"。
//!
//! 模型还必须**先于**测量存在，否则它就不是独立答案，而是对测量结果的复述。
//!
//! # 为什么是 `f64` 而不是 `f32`
//!
//! 模型要做两件事：算值、判"这个值离 8 位台阶的边界有多远"。后者需要比被观察对象
//! 更高的精度：`f32` 自己的舍入误差与我们要判断的那点余量同量级。所以模型用 `f64`
//! 算完再落成字节，并在测试里逐条钉住"每一条判据的余量"。
//!
//! # 这份模型**不能**证明什么（诚实的边界）
//!
//! 四类差异在 8 位观测里根本看不见，模型也就不可能验它们：
//!
//! | 差异 | 量级 | 折成 8 位输出 |
//! |---|---|---|
//! | `f32` 加法重排（"抽头顺序"） | 约 `1e-7`（线性） | 约 `2e-5` LSB |
//! | `Rgba16Float` 中间纹理的量化 | 约 `5e-4`（线性） | 约 `0.1` LSB |
//! | 硬件 sRGB 编码的近似 | 规范允许的近似 | **最多 1 LSB** |
//! | 混合单元的内部精度 | 每层存回 8 位 | 1 LSB（模型按字节量化模拟） |
//!
//! 前两条远在 8 位台阶（`1/255`）之下：`blur` 场景钉住的是**核的取值、clamp 语义
//! 与三趟结构**，而"抽头顺序由文本决定"是靠 WGSL 展开写、无循环来保证的，
//! 不是靠这个场景验出来的。
//!
//! 第三条正是判据容差取 1 字节的理由（不是拍出来的，见
//! `render/scene.rs` 的容差说明与那条"缺陷模型必须远超容差"的测试）。

// ---------------------------------------------------------------------------
// sRGB 传输函数
// ---------------------------------------------------------------------------

/// 8 位通道的量化电平数。`1/255` 就是那个"台阶"。
pub const BYTE_LEVELS: f64 = 255.0;

/// sRGB(非线性) 域的分段点：以下是线性段。
///
/// 这是规范里四舍五入到 7 位小数的十进制，精确交点是 `0.040448236…`；所以两个分支在
/// 这个数上并不逐位相接，实测差 2.33e-9（`srgb_segments_meet_at_the_thresholds`）。
pub const SRGB_DECODE_THRESHOLD: f64 = 0.04045;

/// 线性域的分段点：以下是线性段。**不等于**上面那个数——两者互为反函数的分段点，不能
/// 互用：把编码阈值错写成 `0.04045`，往返最坏会差 74 个字节（线性 0.01 处差 8）。
///
/// 它同样是四舍五入到 7 位小数的十进制，精确交点是 `0.003130668442…`；两个分支在这个数
/// 上实测差 2.85e-8，即 8 位输出上的 7.3e-6 个台阶——判据容差取 1 字节与它无关。
pub const SRGB_ENCODE_THRESHOLD: f64 = 0.0031308;

/// 夹到 `[0, 1]`。NaN 原样返回（不做"把 NaN 变成 0"的偷偷修正——NaN 应当在下游
/// 表现为一条能看见的判据失败，而不是一个看起来正常的 0）。
pub fn clamp01(v: f64) -> f64 {
    v.clamp(0.0, 1.0)
}

/// sRGB(非线性) → 线性光。与 `scene.wgsl` 的 `srgb_to_linear` 同一个分段公式。
///
/// 写除法而不是乘倒数：`1/12.92` 与 `1/1.055` 都不是二进制精确值，乘倒数会多一次
/// 舍入。往返判据看的恰恰是最后几个最低位，所以这一处不能"顺手优化"。
pub fn srgb_decode(srgb: f64) -> f64 {
    let c = clamp01(srgb);
    if c <= SRGB_DECODE_THRESHOLD {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// 线性光 → sRGB(非线性)。
///
/// 真实的编码发生在**硬件存储时**（目标格式是 `Rgba8UnormSrgb`，着色器输出线性光）。
/// 这里写出同一个公式，是为了能独立算出"读回来应该是多少"。
pub fn srgb_encode(linear: f64) -> f64 {
    let c = clamp01(linear);
    if c <= SRGB_ENCODE_THRESHOLD {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

/// 线性光 → 8 位字节（sRGB 编码后四舍五入）。
pub fn byte_of_linear_light(linear: f64) -> u8 {
    (BYTE_LEVELS * srgb_encode(linear))
        .round()
        .clamp(0.0, BYTE_LEVELS) as u8
}

/// 线性 alpha → 8 位字节。alpha 通道**不做** sRGB 编码（unorm 就是线性的）。
pub fn byte_of_linear_alpha(alpha: f64) -> u8 {
    (BYTE_LEVELS * clamp01(alpha))
        .round()
        .clamp(0.0, BYTE_LEVELS) as u8
}

/// 线性 RGBA → 8 位 RGBA。三个颜色通道编码，alpha 不编码。
pub fn bytes_of_linear_rgba(linear: [f64; 4]) -> [u8; 4] {
    [
        byte_of_linear_light(linear[0]),
        byte_of_linear_light(linear[1]),
        byte_of_linear_light(linear[2]),
        byte_of_linear_alpha(linear[3]),
    ]
}

/// 8 位字节 → 线性光。混合判据要在**线性域**比较（附件是 sRGB，混合在线性域发生）。
pub fn linear_light_of_byte(byte: u8) -> f64 {
    srgb_decode(f64::from(byte) / BYTE_LEVELS)
}

/// 8 位 alpha → 线性 alpha。
pub fn linear_alpha_of_byte(byte: u8) -> f64 {
    f64::from(byte) / BYTE_LEVELS
}

/// 线性光对应的**理想字节值**（不四舍五入）。
///
/// 只在测试里用来算"离舍入边界还有多远"：`187.516` 这种值就是危险值——
/// 硬件编码只要差千分之几，读数就会跳到另一个字节上。
pub fn ideal_light_byte(linear: f64) -> f64 {
    BYTE_LEVELS * srgb_encode(linear)
}

/// 理想字节值离最近的舍入边界（`x.5`）有多远：`0.5` = 正落在台阶中央（最安全），
/// `0` = 正落在边界上（**舍入规则**决定结果，观测本身不再能判断对错）。
pub fn rounding_margin(linear: f64) -> f64 {
    let fract = ideal_light_byte(linear).fract();
    0.5 - (fract - 0.5).abs()
}

/// 两个 RGBA 之间的**最大单通道差**（字节）。判据用的就是这个距离。
pub fn distance_bytes(a: [u8; 4], b: [u8; 4]) -> u8 {
    a.into_iter()
        .zip(b)
        .map(|(left, right)| left.abs_diff(right))
        .max()
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// 片元坐标约定
// ---------------------------------------------------------------------------

/// 像素索引 → 片元坐标（`@builtin(position).xy`）。
///
/// `frag` 是**像素中心**：像素索引 `p` 覆盖窗口区间 `[p, p + 1]`，中心在 `p + 0.5`。
/// 这份约定必须在模型与采样表里一致——M0 踩过一次"采样坐标与几何对不上"的坑
/// （见 `render/probe.rs` 的模块文档），当时的症状是断言指着渲染，而渲染是对的。
pub fn frag_center(index: u32) -> f64 {
    f64::from(index) + 0.5
}

/// WGSL 的 `fract`：`x - floor(x)`，结果恒在 `[0, 1)`。
///
/// Rust 的 `f64::fract` 对负数是"朝零方向"的小数部分（`(-0.25).fract() == -0.25`），
/// 与 WGSL 不同。这里的实现必须跟 WGSL 走，否则采样点一旦落在负数一侧，
/// 模型与着色器就会悄悄分开。
pub fn wrap01(v: f64) -> f64 {
    v - v.floor()
}

// ---------------------------------------------------------------------------
// 1. gradient
// ---------------------------------------------------------------------------

/// 帧号的循环周期：`shift = (frame % 16) / 16`。
pub const GRADIENT_SHIFT_PERIOD: u32 = 16;

/// 锯齿的周期数：`b = fract(t * 8)`。8 个周期铺满宽度，一个周期 32 像素。
pub const GRADIENT_SAWTOOTH_PERIODS: f64 = 8.0;

/// `render/scene.rs` 注册表里名为 `gradient` 的那个场景的线性光预测。
///
/// 与 `fs_gradient` 逐行对应：`t = fract(frag.x / size.x + shift)`，
/// 输出 `(t, 1 - t, fract(t * 8), 1)`。
///
/// `y` 不参与——这正是"横向渐变"的含义；参数留着是为了所有场景模型同一个签名，
/// 免得调用方在某个场景上少传一个坐标。
pub fn gradient_linear(size: (u32, u32), frame: u32, x: u32, _y: u32) -> [f64; 4] {
    let width = f64::from(size.0);
    let shift = f64::from(frame % GRADIENT_SHIFT_PERIOD) / f64::from(GRADIENT_SHIFT_PERIOD);
    let t = wrap01(frag_center(x) / width + shift);
    [t, 1.0 - t, wrap01(t * GRADIENT_SAWTOOTH_PERIODS), 1.0]
}

// ---------------------------------------------------------------------------
// 2. checker
// ---------------------------------------------------------------------------

/// 棋盘格边长的基数与步长（像素），以及帧号的循环周期：`4 + (frame % 3) * 4`。
pub const CHECKER_CELL_BASE: f64 = 4.0;
pub const CHECKER_CELL_STEP: f64 = 4.0;
pub const CHECKER_CELL_PERIOD: u32 = 3;

/// 偶格（`ix + iy` 为偶数）的线性颜色。
pub const CHECKER_EVEN_LINEAR: [f64; 3] = [0.85, 0.15, 0.35];

/// 奇格的颜色。
///
/// 两个颜色都不能是黑或白：那样"是否经过 sRGB 编码"会被掩盖——黑与白在两个域里
/// 是同一个值。三通道同时对上才算过。
pub const CHECKER_ODD_LINEAR: [f64; 3] = [0.05, 0.55, 0.95];

/// 当前帧的格子边长（像素）。
pub fn checker_cell_px(frame: u32) -> f64 {
    CHECKER_CELL_BASE + f64::from(frame % CHECKER_CELL_PERIOD) * CHECKER_CELL_STEP
}

/// `fs_checker` 的线性光预测。奇偶用整数算（与着色器一致），不走浮点取模。
pub fn checker_linear(frame: u32, x: u32, y: u32) -> [f64; 4] {
    let cell = checker_cell_px(frame);
    let ix = (frag_center(x) / cell).floor() as i64;
    let iy = (frag_center(y) / cell).floor() as i64;
    let odd = (ix + iy) % 2 != 0;
    let color = if odd {
        CHECKER_ODD_LINEAR
    } else {
        CHECKER_EVEN_LINEAR
    };
    [color[0], color[1], color[2], 1.0]
}

// ---------------------------------------------------------------------------
// 3. srgb_linear
// ---------------------------------------------------------------------------

/// 条纹数：8 条竖条铺满宽度，`shift = (frame % 8) / 8` 每帧平移一条。
pub const TRANSFER_STRIPES: u32 = 8;

/// `fs_srgb_linear` 里第 `k` 条条纹的**线性光**值：`srgb_to_linear(k / 8)`。
///
/// 着色器输出的是"线性光"，硬件的 sRGB 编码会把它编回 `k / 8` 对应的 8 位值。
/// 所以读回来的字节应当 ≈ `round(255 · k / 8)`——这条往返就是本场景的全部内容。
pub fn transfer_linear_for_stripe(k: u32) -> f64 {
    srgb_decode(f64::from(k) / f64::from(TRANSFER_STRIPES))
}

/// 某像素落在第几条条纹上（整数运算，与着色器同一套）。
pub fn transfer_stripe_at(size: (u32, u32), frame: u32, x: u32) -> u32 {
    let shift = f64::from(frame % TRANSFER_STRIPES) / f64::from(TRANSFER_STRIPES);
    let t = wrap01(frag_center(x) / f64::from(size.0) + shift);
    (t * f64::from(TRANSFER_STRIPES)).floor() as u32
}

/// `fs_srgb_linear` 的线性光预测。
pub fn transfer_linear(size: (u32, u32), frame: u32, x: u32) -> [f64; 4] {
    let value = transfer_linear_for_stripe(transfer_stripe_at(size, frame, x));
    [value, value, value, 1.0]
}

// ---------------------------------------------------------------------------
// 4. alpha_stack
// ---------------------------------------------------------------------------

/// 层数基数与周期：`layer_count = 2 + frame % 4`。
pub const LAYER_COUNT_BASE: u32 = 2;
pub const LAYER_COUNT_PERIOD: u32 = 4;

/// 层数上限 = [`LAYER_COUNT_BASE`] + [`LAYER_COUNT_PERIOD`] - 1。
pub const LAYER_COUNT_MAX: usize = 5;

/// 各层的**线性**颜色与 alpha。顺序就是绘制顺序（第一项最先画）。
///
/// 挑这些数的理由：alpha 各不相同（0.5 / 0.5 / 0.25 / 0.75 / 0.5）、颜色三通道
/// 大小关系互不相同，于是"层序反了"与"某一层被漏了"都会在结果里显形，
/// 而且**离任何单层的完全不透明颜色都足够远**（见判据的第三条）。
pub const LAYERS_LINEAR: [[f64; 4]; LAYER_COUNT_MAX] = [
    [0.80, 0.10, 0.10, 0.50],
    [0.10, 0.70, 0.20, 0.50],
    [0.35, 0.25, 0.80, 0.25],
    [0.75, 0.65, 0.10, 0.75],
    [0.15, 0.45, 0.60, 0.50],
];

/// 当前帧要画几层。
pub fn layer_count(frame: u32) -> u32 {
    LAYER_COUNT_BASE + frame % LAYER_COUNT_PERIOD
}

/// 合成顺序。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StackOrder {
    /// 绘制顺序（第一层最先画）。
    Forward,
    /// 逆序——**这是反例**，用来证明"顺序"这条判据不是恒绿的。
    Reverse,
}

/// 按 [`StackOrder`] 展开出的层下标。
pub fn stack_indices(frame: u32, order: StackOrder) -> Vec<usize> {
    let mut indices: Vec<usize> = (0..layer_count(frame) as usize).collect();
    if order == StackOrder::Reverse {
        indices.reverse();
    }
    indices
}

/// 第 `index` 层的**完全不透明**颜色（alpha = 1）编成字节。判据第三条的靶子。
pub fn opaque_layer_bytes(index: usize) -> [u8; 4] {
    let layer = LAYERS_LINEAR[index];
    bytes_of_linear_rgba([layer[0], layer[1], layer[2], 1.0])
}

/// 不量化的理想合成（`f64` 一路到底，不落 8 位）。
///
/// 真实硬件每画完一层就把结果存回 8 位附件，所以它**不是**硬件的行为，而是
/// 一条参照线：与 [`alpha_stack_bytes`] 的差就是"逐层量化"带来的量，测试拿它
/// 来证明容差 2 够用。
pub fn alpha_stack_ideal(frame: u32, order: StackOrder) -> [f64; 4] {
    let mut acc = [0.0_f64; 4];
    for index in stack_indices(frame, order) {
        let [r, g, b, a] = LAYERS_LINEAR[index];
        acc = [
            r * a + acc[0] * (1.0 - a),
            g * a + acc[1] * (1.0 - a),
            b * a + acc[2] * (1.0 - a),
            // alpha 通道的源因子是 **1**（不是 srcAlpha）：直通 alpha 的 source-over。
            // 与 `render/scene.rs` 的 `layer_blend_state` 必须是同一个方程，
            // 写错这里会让模型与硬件差出几十个字节。
            a + acc[3] * (1.0 - a),
        ];
    }
    acc
}

/// `alpha_stack` 的字节预测：**逐层都落回 8 位**地模拟硬件。
///
/// 每层的过程：从上一层的 8 位值线性化 → 用 source-over 混合 → 编码回 8 位。
/// 少做这一步量化，模型会与硬件差 1~2 个字节（见 [`alpha_stack_ideal`]）。
pub fn alpha_stack_bytes(frame: u32, order: StackOrder) -> [u8; 4] {
    let mut dst = [0_u8; 4];
    for index in stack_indices(frame, order) {
        let [r, g, b, a] = LAYERS_LINEAR[index];
        let dst_source = [
            linear_light_of_byte(dst[0]) * (1.0 - a),
            linear_light_of_byte(dst[1]) * (1.0 - a),
            linear_light_of_byte(dst[2]) * (1.0 - a),
        ];
        let dst_alpha = linear_alpha_of_byte(dst[3]) * (1.0 - a);
        dst = [
            byte_of_linear_light(r * a + dst_source[0]),
            byte_of_linear_light(g * a + dst_source[1]),
            byte_of_linear_light(b * a + dst_source[2]),
            byte_of_linear_alpha(a + dst_alpha),
        ];
    }
    dst
}

// ---------------------------------------------------------------------------
// 5. blur
// ---------------------------------------------------------------------------

/// 高斯核的 σ。半径 3 时 σ = 1.5 是"权重衰减得刚好"的常见取值。
pub const BLUR_SIGMA: f64 = 1.5;

/// 抽头半径（像素）。核在 `[0, 3.5σ]` 之外已经很轻，3 之外再往外加没有意义。
pub const BLUR_RADIUS: i32 = 3;

/// **写进 `scene.wgsl` 的权重字面量**，下标是抽头距离 `|i|`。
///
/// 它们是 `gaussian_exact(i) / 归一化因子` 的 6 位四舍五入值，且按字面量相加
/// 正好是 `1.000000`（测试钉住）。为什么必须是非二进制精确值（`0.270682` 在 `f32`
/// 下就不能精确表示）：若换成 `1/16` 这种二进制可表示的数，加法怎么重排结果都一样，
/// "抽头顺序由文本决定"这条守卫会变成空话。
pub const BLUR_WEIGHTS: [f64; 4] = [0.270682, 0.216745, 0.111281, 0.036633];

/// 源图案的格子边长（像素）。
pub const BLUR_CELL_PX: u32 = 8;

/// 奇格的亮度（线性）。偶数格是 0.0。
pub const BLUR_ODD_LEVEL: f64 = 0.6;

/// 高光块的半开区间 `[min, max)`，两轴相同。
///
/// 它落在偶格（`floor(40 / 8) + floor(40 / 8) = 10`，偶数）里：`1.0` 压在 `0.0` 上，
/// 对比度拉满。若压在奇格（0.6）上，判据余量会小一半。
pub const BLUR_IMPULSE_RANGE: (u32, u32) = (40, 43);

/// 未归一化的高斯取值：`exp(-i² / (2σ²))`。
pub fn gaussian_exact(offset: i32) -> f64 {
    let i = f64::from(offset);
    (-(i * i) / (2.0 * BLUR_SIGMA * BLUR_SIGMA)).exp()
}

/// 归一化因子 `1 + 2·(g₁ + g₂ + g₃)`。
pub fn gaussian_normalization() -> f64 {
    1.0 + 2.0 * (gaussian_exact(1) + gaussian_exact(2) + gaussian_exact(3))
}

/// 精确权重 `g(i) / 归一化因子`（连续值，不是字面量）。
pub fn gaussian_weight_exact(offset: i32) -> f64 {
    gaussian_exact(offset) / gaussian_normalization()
}

/// 权重**字面量**与精确值的最大偏差。
///
/// 字面量是模型与 WGSL 共用的那份，所以它必须足够接近精确值：偏差 `≤ 5e-7`
/// （6 位小数的舍入误差上界）在 8 位输出上折不到 `0.01` 个字节。
pub fn blur_weight_literal_error() -> f64 {
    (0..=BLUR_RADIUS)
        .map(|offset| (BLUR_WEIGHTS[offset as usize] - gaussian_weight_exact(offset)).abs())
        .fold(0.0_f64, f64::max)
}

/// 字面量的和 `Σ w`（含对称项）。
pub fn blur_weight_sum() -> f64 {
    BLUR_WEIGHTS[0] + 2.0 * (BLUR_WEIGHTS[1] + BLUR_WEIGHTS[2] + BLUR_WEIGHTS[3])
}

/// 源图案在像素 `(x, y)` 上的值。
pub fn blur_source_pixel(x: u32, y: u32) -> f64 {
    let (impulse_min, impulse_max) = BLUR_IMPULSE_RANGE;
    if x >= impulse_min && x < impulse_max && y >= impulse_min && y < impulse_max {
        return 1.0;
    }
    let checker = (x / BLUR_CELL_PX) + (y / BLUR_CELL_PX);
    if checker % 2 == 1 {
        BLUR_ODD_LEVEL
    } else {
        0.0
    }
}

/// 越界纹素的取法。`Clamp` 是着色器的语义，另两个是**反例模型**：
/// 判据要能区分它们，否则"clamp 语义被验到了"就是一句空话。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EdgeMode {
    /// 钳到边缘（`load_texel` 的行为）。
    Clamp,
    /// 越界算 0（"零填充"卷积）。
    Zero,
    /// 环绕。
    Wrap,
}

/// 在某一轴上取下标；`None` 表示"这一轴上越界，且语义是零填充"。
fn resolve_index(coord: i32, dim: u32, mode: EdgeMode) -> Option<u32> {
    let last = dim as i32 - 1;
    match mode {
        EdgeMode::Clamp => Some(coord.clamp(0, last) as u32),
        EdgeMode::Zero => {
            if coord < 0 || coord > last {
                None
            } else {
                Some(coord as u32)
            }
        }
        EdgeMode::Wrap => Some(coord.rem_euclid(dim as i32) as u32),
    }
}

/// 按给定边界语义取源图案的一纹素（两轴分别判定；任一轴取不到就当 0）。
fn blur_source_sample(size: (u32, u32), x: i32, y: i32, mode: EdgeMode) -> f64 {
    match (
        resolve_index(x, size.0, mode),
        resolve_index(y, size.1, mode),
    ) {
        (Some(sx), Some(sy)) => blur_source_pixel(sx, sy),
        _ => 0.0,
    }
}

/// 横向一趟在 `(x, y)` 上的结果。
///
/// 抽头从 `-3` 到 `+3` **按文本顺序**累加，`BLUR_WEIGHTS` 用 `|offset|` 取——
/// 与 `fs_blur_h` 展开写的七行一一对应。顺序在 `f64` 下只影响 1e-16，
/// 但"模型与文本同序"这件事本身值得保持：它是人能对着两段代码逐行核对的前提。
fn blur_h_at(size: (u32, u32), x: i32, y: i32, mode: EdgeMode, quantize: bool) -> f64 {
    let mut acc = 0.0_f64;
    for offset in -BLUR_RADIUS..=BLUR_RADIUS {
        acc += BLUR_WEIGHTS[offset.unsigned_abs() as usize]
            * blur_source_sample(size, x + offset, y, mode);
    }
    if quantize { round_to_f16(acc) } else { acc }
}

/// 两趟（先横后纵）在 `(x, y)` 上的结果，边界语义由 `mode` 决定。
fn blur_at_mode(size: (u32, u32), x: u32, y: u32, mode: EdgeMode, quantize: bool) -> f64 {
    let (x, y) = (x as i32, y as i32);
    let mut acc = 0.0_f64;
    for offset in -BLUR_RADIUS..=BLUR_RADIUS {
        acc += BLUR_WEIGHTS[offset.unsigned_abs() as usize]
            * blur_h_at(size, x, y + offset, mode, quantize);
    }
    acc
}

/// `blur` 场景的预测值（线性光，clamp 语义，不做中间量化）。
pub fn blur_at(size: (u32, u32), x: u32, y: u32) -> f64 {
    blur_at_mode(size, x, y, EdgeMode::Clamp, false)
}

/// 换一种边界语义的预测值。只用于判据的反例比较。
pub fn blur_at_with_edge(size: (u32, u32), x: u32, y: u32, mode: EdgeMode) -> f64 {
    blur_at_mode(size, x, y, mode, false)
}

/// 把中间纹理的存储量化算进去的预测值：横向那趟的结果按 `Rgba16Float` 存回。
///
/// 中间纹理是 `Rgba16Float`（而不是 8 位）正是为了让**三趟结构**可观测；
/// 代价是它自己会引入约 `5e-4` 的量化误差。这个函数用来量那条误差有多大——
/// 结果是它在 8 位输出上折成 **0 个字节**（测试钉住），也就是判据容差 1 字节
/// 里还剩整整一个字节的余量。
pub fn blur_at_with_intermediate_quantized(size: (u32, u32), x: u32, y: u32) -> f64 {
    blur_at_mode(size, x, y, EdgeMode::Clamp, true)
}

/// 把 `f64` 舍入到最近的半精度（`f16`）可表示值。
///
/// 手写而不是引 `half`：这个函数只在模型里用一次（量化中间值），
/// 而**多一个依赖**是 dhampir-core 不愿意付的代价。精度取正规数尾数 10 位、
/// 次正规数下限 `2^-24`，与 IEEE 754 binary16 一致。
pub fn round_to_f16(value: f64) -> f64 {
    if value == 0.0 || !value.is_finite() {
        return value;
    }
    let magnitude = value.abs();
    let exponent = magnitude.log2().floor();
    let step = 2.0_f64.powf((exponent - 10.0).max(-24.0));
    value.signum() * (magnitude / step).round() * step
}

// ---------------------------------------------------------------------------
// 测试用的对照模型
// ---------------------------------------------------------------------------

/// 半径 3 的**盒式**滤波（平坦核）。测试用：证明 gaussian 的判据不是"随便什么
/// 模糊都能过"。位置不在 `tests` 模块里，是因为 `scene.rs` 的测试也要用它——
/// 放到 `#[cfg(test)] mod tests` 里就没法共用了。
#[cfg(test)]
pub(crate) fn box_blur_at(size: (u32, u32), x: u32, y: u32) -> f64 {
    let (x, y) = (x as i32, y as i32);
    let span = (2 * BLUR_RADIUS + 1) as f64;
    let mut acc = 0.0_f64;
    for dx in -BLUR_RADIUS..=BLUR_RADIUS {
        for dy in -BLUR_RADIUS..=BLUR_RADIUS {
            acc += blur_source_sample(size, x + dx, y + dy, EdgeMode::Clamp);
        }
    }
    acc / (span * span)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 核的**数值**：三个小数位以上的常数在文档里被引用，改动必须是有意的。
    ///
    /// 更关键的是：`BLUR_WEIGHTS` 必须真的等于"精确核的 6 位舍入"。如果谁手滑改了
    /// 一位（当年写错过 `0.216745` → `0.216753`），这条测试立刻红，而不必等某天
    /// 有人盯着一条 8 位输出里的 1 字节差异发愣。
    #[test]
    fn weights_are_the_rounded_exact_gaussian() {
        let expected_exact = [1.0, 0.800737402917, 0.411112290507, 0.135335283237];
        for (offset, exact) in expected_exact.iter().enumerate() {
            let computed = gaussian_exact(offset as i32);
            assert!(
                (computed - exact).abs() < 5e-13,
                "gaussian_exact({offset}) = {computed}，与文档里的 {exact} 不符"
            );
        }

        let normalization = gaussian_normalization();
        assert!(
            (normalization - 3.694369953321).abs() < 5e-13,
            "归一化因子 = {normalization}"
        );

        let expected_literals = [0.270682, 0.216745, 0.111281, 0.036633];
        assert_eq!(BLUR_WEIGHTS, expected_literals, "权重字面量被改动了");
        for (offset, literal) in BLUR_WEIGHTS.iter().enumerate() {
            let rounded = (gaussian_weight_exact(offset as i32) * 1e6).round() / 1e6;
            assert!(
                (rounded - literal).abs() < 1e-12,
                "第 {offset} 项的精确值是 {rounded}（6 位舍入），字面量却是 {literal}"
            );
        }
    }

    /// 字面量之和必须是 `1.000000`：卷积要保持"平场进、平场出"。
    ///
    /// 这不是凑出来的巧合，而是选字面量时的**条件**（当年写成 `0.216753/0.111280/`
    /// `0.036630` 时和是 `1.000008`）。
    #[test]
    fn weights_sum_to_one() {
        let sum = blur_weight_sum();
        assert!((sum - 1.0).abs() < 1e-12, "Σ字面量 = {sum}");
        let error = blur_weight_literal_error();
        assert!(error < 5e-7, "字面量偏离精确核 {error}，超过 6 位舍入的界");
        // 折到 8 位输出上必须远小于一个台阶，否则"用字面量当模型"就不诚实。
        assert!(
            255.0 * error * 3.0 < 0.01,
            "字面量误差在 8 位输出上是 {} 字节，不能忽略",
            255.0 * error * 3.0
        );
    }

    /// 权重必须是**非二进制精确值**。
    ///
    /// 否则编译器怎么重排加法结果都逐位相同，"抽头顺序由文本决定"这条守卫就失去
    /// 对象。这条测试是那句注释的可执行版本。
    #[test]
    fn weights_are_not_binary_exact() {
        for weight in BLUR_WEIGHTS {
            let as_f32 = weight as f32;
            assert!(
                f64::from(as_f32) != weight,
                "{weight} 在 f32 下可以精确表示——换一个权重，别让守卫变成空话"
            );
        }
    }

    /// sRGB 往返：**0 字节误差**，且与 8 位台阶一一对应。
    #[test]
    fn srgb_roundtrip_is_byte_exact() {
        for byte in 0..=255_u8 {
            let back = byte_of_linear_light(linear_light_of_byte(byte));
            assert_eq!(back, byte, "字节 {byte} 往返之后变成了 {back}");
        }
        assert_eq!(byte_of_linear_light(0.0), 0);
        assert_eq!(byte_of_linear_light(1.0), 255);
        // 0.5 是半亮：sRGB 0.735357×255 = 187.5 附近，取 188。
        assert_eq!(byte_of_linear_light(0.5), 188);
        assert_eq!(byte_of_linear_alpha(0.5), 128);
        assert_eq!(byte_of_linear_alpha(0.0), 0);
    }

    /// 分段点两侧必须接得上——但**不是逐位相接**。
    ///
    /// 规范给的阈值是四舍五入到 7 位小数的十进制，精确交点在 `0.003130668442…` /
    /// `0.040448236…`：所以两个分支在文档阈值处天然差 2.85e-8（编码侧）与 2.33e-9（解码
    /// 侧）。这条测试把这两个残差**上下都钉住**：换成精确交点、或者写错常量，都会失败。
    #[test]
    fn srgb_segments_meet_at_the_thresholds() {
        let encode_below = srgb_encode(SRGB_ENCODE_THRESHOLD - 1e-12);
        let encode_above = srgb_encode(SRGB_ENCODE_THRESHOLD + 1e-12);
        let encode_jump = (encode_above - encode_below).abs();
        assert!(
            (1e-9..1e-7).contains(&encode_jump),
            "编码在分段点上的残差变成了 {encode_jump:e}——常量与上面那段说明要一起改"
        );
        // 残差在 8 位输出上必须看不见：两侧落在同一个字节上。
        assert_eq!(
            byte_of_linear_light(encode_below),
            byte_of_linear_light(encode_above),
            "分段点两侧掉进了不同的字节"
        );

        let decode_below = srgb_decode(SRGB_DECODE_THRESHOLD - 1e-12);
        let decode_above = srgb_decode(SRGB_DECODE_THRESHOLD + 1e-12);
        let decode_jump = (decode_above - decode_below).abs();
        assert!(
            (1e-10..1e-8).contains(&decode_jump),
            "解码在分段点上的残差变成了 {decode_jump:e}——常量与上面那段说明要一起改"
        );
    }

    /// `wrap01` 必须与 WGSL 的 `fract` 同语义（负数一侧尤其要看）。
    #[test]
    fn wrap01_matches_wgsl_fract() {
        assert!((wrap01(0.25) - 0.25).abs() < 1e-15);
        assert!((wrap01(1.25) - 0.25).abs() < 1e-15);
        assert!((wrap01(-0.25) - 0.75).abs() < 1e-15);
        assert!((wrap01(-1.25) - 0.75).abs() < 1e-15);
        // 与 Rust 自带的 fract 不同——这正是不能直接用它的原因。
        assert!((-0.25_f64).fract() < 0.0);
    }

    /// 钉住 blur 五个采样点的模型值。
    ///
    /// 这五个数是整条判据的靶子，其中 `(0, 41)` 是唯一能分辨边界语义的点。
    /// 数字变了意味着核、图案或边界语义动了——那必须是**有意**的改动。
    #[test]
    fn blur_model_matches_pinned_samples() {
        let size = (256, 256);
        let cases = [
            ((19, 19), 0.0, 0),
            ((35, 27), 0.6, 203),
            ((38, 41), 0.552_914_359, 196),
            ((41, 41), 0.647_100_744, 210),
            ((0, 41), 0.511_251_600, 189),
        ];
        for ((x, y), linear, byte) in cases {
            let value = blur_at(size, x, y);
            assert!(
                (value - linear).abs() < 1e-9,
                "({x},{y}) 的模型值是 {value}，钉住的是 {linear}"
            );
            assert_eq!(byte_of_linear_light(value), byte, "({x},{y}) 的字节不对");
        }
    }

    /// 边界语义必须**分得开**：clamp / zero / wrap 在 `(0, 41)` 上给出三个不同的
    /// 字节，其中 clamp 与另外两个差 35 / 28 个字节。
    ///
    /// 没有这一条，"clamp 语义被验到了"就无从谈起——三种语义要是给出同一个值，
    /// 判据再严也只是在验别的东西。
    #[test]
    fn edge_modes_are_distinguishable() {
        let size = (256, 256);
        let clamp = byte_of_linear_light(blur_at_with_edge(size, 0, 41, EdgeMode::Clamp));
        let zero = byte_of_linear_light(blur_at_with_edge(size, 0, 41, EdgeMode::Zero));
        let wrap = byte_of_linear_light(blur_at_with_edge(size, 0, 41, EdgeMode::Wrap));
        assert_eq!((clamp, zero, wrap), (189, 154, 161));
        assert!(
            clamp.abs_diff(zero) >= 32,
            "clamp 与 zero 只差 {} 字节",
            clamp.abs_diff(zero)
        );
        assert!(
            clamp.abs_diff(wrap) >= 24,
            "clamp 与 wrap 只差 {} 字节",
            clamp.abs_diff(wrap)
        );

        // 另外四个点上三种语义必须**一样**——那说明它们不在边界附近，
        // 于是"哪个点负责验边界"这件事有唯一答案。
        for (x, y) in [(19, 19), (35, 27), (38, 41), (41, 41)] {
            let values = [EdgeMode::Clamp, EdgeMode::Zero, EdgeMode::Wrap]
                .map(|mode| blur_at_with_edge(size, x, y, mode));
            let spread = values.iter().cloned().fold(f64::MIN, f64::max)
                - values.iter().cloned().fold(f64::MAX, f64::min);
            assert!(spread < 1e-12, "({x},{y}) 上三种边界语义给出了不同的值");
        }
    }

    /// 中间纹理的 `Rgba16Float` 量化在 8 位输出上是 **0 个字节**。
    #[test]
    fn intermediate_quantization_stays_below_one_byte() {
        let size = (256, 256);
        for (x, y) in [(19, 19), (35, 27), (38, 41), (41, 41), (0, 41)] {
            let exact = byte_of_linear_light(blur_at(size, x, y));
            let quantized = byte_of_linear_light(blur_at_with_intermediate_quantized(size, x, y));
            assert!(
                exact.abs_diff(quantized) <= 1,
                "({x},{y}) 量化前后差 {} 字节，容差 1 就不够了",
                exact.abs_diff(quantized)
            );
        }
    }

    /// `round_to_f16` 得真的是半精度：尾数 10 位、次正规数下限对得上。
    #[test]
    fn round_to_f16_matches_binary16() {
        assert!((round_to_f16(1.0) - 1.0).abs() < 1e-15);
        assert!((round_to_f16(0.6) - 0.600_097_656_25).abs() < 1e-15);
        // 0.5 到 1.0 之间的台阶是 2^-11。
        let step = 2.0_f64.powi(-11);
        let value = 0.5 + step;
        assert!((round_to_f16(value) - value).abs() < 1e-15);
        let tiny = 2.0_f64.powi(-24);
        assert!((round_to_f16(tiny) - tiny).abs() < 1e-18);
        // 0 与 NaN 原样返回（不能因为量化把 0 变成 1e-24）。
        assert_eq!(round_to_f16(0.0), 0.0);
        assert!(round_to_f16(f64::NAN).is_nan());
    }

    /// 盒式滤波必须**过不了**高斯判据。判据要是连"模糊了"就算过，那它验的是
    /// "有没有模糊"，而不是"模糊对不对"。
    #[test]
    fn box_filter_is_not_the_gaussian() {
        let size = (256, 256);
        let gaussian = byte_of_linear_light(blur_at(size, 41, 41));
        let boxed = byte_of_linear_light(box_blur_at(size, 41, 41));
        assert!(
            gaussian.abs_diff(boxed) > 2,
            "盒式滤波给出 {boxed}，高斯给出 {gaussian}——判据分不开它们"
        );
    }

    /// `blur` 的采样点在更大的目标上给同样的值。
    ///
    /// 这条是 1080p 计时跑能在**同一个采样表**上做检查的前提：图案是按绝对像素
    /// 坐标画的，五个采样点又都不在受尺寸影响的区域里。
    #[test]
    fn blur_samples_do_not_depend_on_size() {
        for size in [(256, 256), (512, 512), (1920, 1080)] {
            for (x, y) in [(19, 19), (35, 27), (38, 41), (41, 41), (0, 41)] {
                assert_eq!(
                    byte_of_linear_light(blur_at(size, x, y)),
                    byte_of_linear_light(blur_at((256, 256), x, y)),
                    "尺寸 {size:?} 下 ({x},{y}) 的读数变了"
                );
            }
        }
    }

    /// 顺序判据的余量：正序与逆序的字节预测至少差 37。
    #[test]
    fn stack_order_changes_the_result_by_a_wide_margin() {
        for frame in 0..8 {
            let forward = alpha_stack_bytes(frame, StackOrder::Forward);
            let reverse = alpha_stack_bytes(frame, StackOrder::Reverse);
            let gap = distance_bytes(forward, reverse);
            assert!(
                gap >= 16,
                "第 {frame} 帧：正序 {forward:?} 与逆序 {reverse:?} 只差 {gap} 字节"
            );
        }
    }

    /// 逐层量化与理想模型的差 ≤ 1 字节。容忍 2 字节的判据据此成立。
    #[test]
    fn stack_quantization_stays_within_tolerance() {
        for frame in 0..8 {
            let quantized = alpha_stack_bytes(frame, StackOrder::Forward);
            let ideal = bytes_of_linear_rgba(alpha_stack_ideal(frame, StackOrder::Forward));
            let gap = distance_bytes(quantized, ideal);
            assert!(
                gap <= 1,
                "第 {frame} 帧：量化模型 {quantized:?} 与理想模型 {ideal:?} 差 {gap}"
            );
        }
    }

    /// alpha 通道的源因子是 1（不是 srcAlpha）。写错会让 alpha 一路偏暗，
    /// 而三个颜色通道照旧是对的——正是那种"看着没问题"的错误。
    #[test]
    fn stack_alpha_channel_uses_the_over_operator() {
        // 两层、每层 alpha 0.5：结果 alpha 应当是 0.5 + 0 = 0.5（而不是 0.25）。
        let frame = 0;
        assert_eq!(layer_count(frame), 2);
        let alpha = alpha_stack_bytes(frame, StackOrder::Forward)[3];
        assert_eq!(
            alpha, 192,
            "第 0 帧的 alpha 应当是 0.5×255 ≈ 128 之后逐层累积的值"
        );
        // 三层：0.25 + 0.5·(1-0.25) + 0.5·(1-0.25-0.375) 在字节域里的落点。
        assert_eq!(alpha_stack_bytes(1, StackOrder::Forward)[3], 208);
    }

    /// 采样表要能分辨四种有代表性的错法：通道置换、少一次编码、相位差一格、
    /// 层数不随帧变化。逐条给出字节距离，全部远大于容差。
    #[test]
    fn scene_models_separate_the_defects_we_care_about() {
        let size = (256, 256);

        // ① checker 的通道置换（把偶格与奇格的颜色换过来）。
        let even = bytes_of_linear_rgba(checker_linear(0, 1, 1));
        let odd = bytes_of_linear_rgba(checker_linear(0, 5, 1));
        assert!(
            distance_bytes(even, odd) >= 100,
            "棋盘两色太接近，换色看不出来"
        );

        // ② srgb_linear 少一次解码：直接输出 k/8（线性域），会系统性偏亮。
        let mut worst_gap = 0_u8;
        for k in 0..8_u32 {
            let correct = byte_of_linear_light(transfer_linear_for_stripe(k));
            let missing = byte_of_linear_light(f64::from(k) / 8.0);
            worst_gap = worst_gap.max(correct.abs_diff(missing));
        }
        assert!(
            worst_gap >= 16,
            "漏一次解码在 8 位输出上只差 {worst_gap} 字节，判据太软"
        );

        // ③ gradient 相位差一格（1/16）：在锯齿那一侧会整段跳开。
        let t0 = bytes_of_linear_rgba(gradient_linear(size, 0, 48, 8));
        let t1 = bytes_of_linear_rgba(gradient_linear(size, 1, 48, 8));
        assert!(
            distance_bytes(t0, t1) >= 32,
            "渐变相位差 1/16 只差 {} 字节",
            distance_bytes(t0, t1)
        );

        // ④ alpha_stack 层数不随帧变（永远画两层）。
        let layer_frames = (0..4).map(layer_count).collect::<Vec<_>>();
        assert_eq!(layer_frames, vec![2, 3, 4, 5]);
    }
}
