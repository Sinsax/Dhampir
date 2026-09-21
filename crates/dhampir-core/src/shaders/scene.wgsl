// dhampir · M1 corpus 场景着色器（一份文本、两个宿主）
//
// 这份 WGSL 被 `include_str!` 编进 dhampir-core：native 下由 naga 编成
// SPIR-V / HLSL / MSL，wasm 下把**同一份原文**交给浏览器的 Tint。
// 所以它必须待在 WebGPU 能力下限里（指导文档 §4.3①），与 probe.wgsl 同一套约束：
//
//   - 不用导数（fwidth / dpdx / dpdy）：结果允许因实现而异
//   - 不用隐式 LOD 采样（textureSample）：它要导数；采样一律走 textureLoad
//   - 不用循环：累加顺序必须由**文本**决定，不能由编译器的展开顺序决定
//   - 只用 f32 / u32 / i32，不用任何精度限定的隐式行为
//
// 「确定性」在这份文件里的具体含义：同一份 WGSL + 同一组 Params + 同一个目标尺寸
// → 逐字节相同的像素。帧号只参与**整数**运算（% / & / 整除），不读时钟、不用随机数。
// 整数运算不会因为编译器或驱动的差异而变化，浮点会——这正是要守住的那条线。
//
// 每个场景「考什么」写在各自入口前的注释里；判定在 render/scene.rs（纯函数）。

struct Params {
    // 目标尺寸（像素）。用来把帧坐标换算成 uv。
    size: vec2<f32>,
    // 帧号。只以整数方式参与运算——见文件头的「确定性」。
    frame: u32,
    // 对齐填充（uniform 里的结构体按 16 字节对齐），同时留给 M2 的 seed 之类参数。
    pad: u32,
};

@group(0) @binding(0) var<uniform> params: Params;

// 中间纹理。程序化场景（gradient / checker / srgb_linear / alpha_stack）不读它，
// 但**绑定组布局是全场景共用的**：多一个未使用的绑定不影响任何结果，
// 而"每个场景一套布局、各自一张空表"只会让 M2 的归因多一层变量。
// blur 场景真的读它（它的输入就是上一趟写进这张纹理的图案）。
@group(0) @binding(1) var src: texture_2d<f32>;

// ---------------------------------------------------------------------------
// 全屏三角形
//
// 不需要顶点缓冲、没有顶点属性：三个顶点铺满视口，多余的部分被裁掉。
// 顶点坐标的选法与 probe.wgsl 同一个理由（见 render/probe.rs 关于"别落在像素
// 中心、也别落在两像素正中间"的说明）：(-1,-1) (3,-1) (-1,3) 的斜边是
// x + y = 2，而视口内 x + y ≤ 2 - (1/W + 1/H) < 2，所以每个像素中心都**严格**
// 落在三角形内部，覆盖判定没有 tie，也就没有"实现自由"的余地。
// ---------------------------------------------------------------------------
@vertex
fn vs_fullscreen(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    var corners = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0),
    );
    return vec4<f32>(corners[index], 0.0, 1.0);
}

// alpha_stack 用：位置来自全屏三角形（同上），颜色来自逐层顶点数据，
// 于是"第几层画的是什么颜色"完全由宿主传进来的顶点缓冲决定，着色器不猜。
struct LayerOut {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) color: vec4<f32>,
};

@vertex
fn vs_layer(@builtin(vertex_index) index: u32, @location(0) color: vec4<f32>) -> LayerOut {
    var corners = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0),
    );
    var out: LayerOut;
    out.clip_position = vec4<f32>(corners[index], 0.0, 1.0);
    // 颜色原样传下去：alpha 参与混合，由固定功能混合单元做，不在着色器里做。
    // 手写混合方程与固定功能混合各算一次，是"两端不一致"的经典来源之一。
    out.color = color;
    return out;
}

// ---------------------------------------------------------------------------
// 共用小工具
// ---------------------------------------------------------------------------

// 像素坐标 → 纹素坐标。frag 一定 ≥ 0，所以截断就是向下取整。
fn texel_of(frag: vec2<f32>) -> vec2<i32> {
    return vec2<i32>(i32(frag.x), i32(frag.y));
}

// 越界处钳到边缘（clamp-to-edge）。**不用采样器、不用 textureSample**：
// textureLoad 按整型坐标精确取纹素，既不需要导数，也不涉及滤波精度——
// WebGPU 允许实现在滤波时降精度，那会让"累加顺序"这条线索被滤波噪声盖住。
fn load_texel(texel: vec2<i32>, dim: vec2<i32>) -> vec3<f32> {
    let clamped = clamp(texel, vec2<i32>(0), dim - vec2<i32>(1));
    return textureLoad(src, clamped, 0).rgb;
}

// sRGB(非线性) → linear。用规范里的分段公式，**写除法而不是乘倒数**：
// 1/12.92 与 1/1.055 在二进制里都不是精确值，乘倒数会引入一次额外舍入，
// 而"往返"这条判据要看的恰恰是最后几个最低位。
fn srgb_to_linear(s: vec3<f32>) -> vec3<f32> {
    let lo = s / 12.92;
    let hi = pow((s + vec3<f32>(0.055)) / 1.055, vec3<f32>(2.4));
    return select(hi, lo, s <= vec3<f32>(0.04045));
}

// ---------------------------------------------------------------------------
// 1. gradient —— 全范围渐变（考插值与 8 位量化精度）
//
// 目标格式是 Rgba8UnormSrgb，着色器输出的是**线性光**，编码由硬件在存储时做。
// 三通道各有各的形状，避免"三通道一起偏"掩盖掉单通道的错误：
//   r = 斜坡本身            —— 看整体是否单调、端点是否到位
//   g = 反向斜坡            —— 通道顺序写反了会立刻显形
//   b = 8 周期的锯齿        —— 插值或量化上差一个最低位，相位就会整段错开，
//                             比"斜坡看着对不对"容易判定得多
//
// 帧号给出 1/16 的循环平移（整数运算 → 两端同一个平移量）。
// ---------------------------------------------------------------------------
@fragment
fn fs_gradient(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let shift = f32(params.frame % 16u) / 16.0;
    let x = fract(frag.x / params.size.x + shift);
    return vec4<f32>(x, 1.0 - x, fract(x * 8.0), 1.0);
}

// ---------------------------------------------------------------------------
// 2. checker —— 像素级棋盘（考光栅化坐标的精确性）
//
// 格子边长 4 / 8 / 12 像素，随帧循环。奇偶用**整数**算：
// 浮点取模会让"格子的边界"变成一个模糊的判断，而边界正是这里要钉死的东西。
//
// 颜色刻意选成既不黑也不白、也不相等的两个值：这样"是否经过 sRGB 编码"
// 无法被"黑/白在哪个域都一样"掩盖——三通道的读数必须同时对上。
// ---------------------------------------------------------------------------
@fragment
fn fs_checker(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let cell_px = 4.0 + f32((params.frame % 3u) * 4u);
    let ix = i32(floor(frag.x / cell_px));
    let iy = i32(floor(frag.y / cell_px));
    let odd = ((ix + iy) & 1) == 1;
    let even_color = vec3<f32>(0.85, 0.15, 0.35);
    let odd_color = vec3<f32>(0.05, 0.55, 0.95);
    return vec4<f32>(select(even_color, odd_color, odd), 1.0);
}

// ---------------------------------------------------------------------------
// 3. srgb_linear —— sRGB ↔ linear 往返（考传输函数）
//
// 8 条竖条，第 k 条**以线性光**输出 srgb_to_linear(k/8)。硬件的 sRGB 编码在
// 存储时把它编回 k/8 对应的 8 位值，于是"读回来的字节 ≈ round(255·k/8)"
// 直接验证了往返。若谁漏了一次解码（把 k/8 当线性值直接输出），读数会系统性偏亮；
// 若多用了一次解码，会系统性偏暗——两种错误的方向不同，都能一眼归因。
//
// 帧号把条带整体循环平移（整数条带位移），平移量是精确的。
// ---------------------------------------------------------------------------
@fragment
fn fs_srgb_linear(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let stripes = 8.0;
    let shift = f32(params.frame % 8u) / stripes;
    let shifted = fract(frag.x / params.size.x + shift);
    let k = floor(shifted * stripes);
    let s = k / stripes;
    return vec4<f32>(srgb_to_linear(vec3<f32>(s)), 1.0);
}

// ---------------------------------------------------------------------------
// 4. alpha_stack —— 多层半透明叠加（考混合顺序）
//
// 每层是一个铺满视口的三角形，颜色与 alpha 来自顶点数据（见 render/scene.rs 的
// SCENE_LAYERS），绘制顺序就是层的顺序，混合用固定功能的 "over"：
//     dst = src·srcAlpha + dst·(1 - srcAlpha)
// 层数随帧增长（2..5 层）：层数不同 → 结果的落点不同，而**顺序**才是判据要抓的。
//
// 判据（见 scene.rs）不是"数字必须等于某个值"，而是两条更硬的性质：
//   ① 实测值必须离"按顺序合成的模型预测"比离"逆序合成的预测"更近
//      —— 顺序反了会被直接抓住，而具体混合域（线性还是 sRGB）的差异不影响这个比较
//   ② 实测值不能等于任何单层的"完全不透明"颜色——那是 alpha 被忽略的症状
// ---------------------------------------------------------------------------
@fragment
fn fs_layer(in: LayerOut) -> @location(0) vec4<f32> {
    return in.color;
}

// ---------------------------------------------------------------------------
// 5. blur —— 可分离高斯 + 物理上分离的高光块（考多趟与浮点累加顺序）
//
// 三趟：图案 → 横向模糊 → 纵向模糊（最后一趟直接写目标纹理）。
// 中间纹理是 Rgba16Float，不是 8 位：8 位存储会把每一趟的差异压到 1/255 以下，
// 于是"累加顺序不同"这件事在结果里根本看不见——那这条场景就白设了。
//
// 源的图案：8 像素棋盘（0.0 / 0.6 线性光）+ 一个 3×3 的高光块（1.0）。
//
// **这一场景刻意不用帧号。** 它的判据是"实测值对上模型卷积"，而模型必须在
// 采样点上成立；图案一旦随帧平移，"采样点到底落在图案的哪一格"就变成每帧都要
// 重算的东西——M0 已经因为采样坐标和几何对不上吃过一次亏（见 render/probe.rs
// 模块文档里那段）。帧号的确定性由其它四个场景覆盖，这里宁可少一个变量。
//
// 高光块 [40,43) × [40,43) 落在**偶格**里（格 5+5 = 10），所以它是 1.0 压在
// 0.0 上，对比度拉满；若放在奇格（0.6）上，对比度只有 0.4，判据的余量会小一半。
//
// 核：σ = 1.5、半径 3 的高斯，已归一化，**按 6 位小数写死**：
//     exp(-i²/(2σ²))，i = 0..3 → 1、0.800737402917、0.411112290507、0.135335283237
//     归一化因子 = 1 + 2·(0.800737402917 + 0.411112290507 + 0.135335283237) = 3.694369953321
//     w = [0.270682, 0.216745, 0.111281, 0.036633]（各自的 6 位四舍五入）
//     按字面量相加 Σw = 1.000000
//
// **为什么权重必须是非二进制精确值**：`fround(0.270682) != 0.270682`——上面这四个
// 小数在 f32 下没有一个能精确表示。若改成 1/16 这种二进制可精确表示的数，无论编译器
// 怎么重排加法、怎么融合乘加，结果都逐位相同，"抽头顺序由文本决定"这条守卫就成了空话。
//
// 诚实的边界：这个场景**不声称**能通过 8 位目标观测出累加顺序。中间纹理是 Rgba16Float，
// 它带来的量化误差（约 5e-4，线性）折算到 8 位输出约 0.1 LSB；而 f32 重排加法的误差
// 约 1e-7，折算是 2e-5 LSB。两者都远在 8 位量化台阶（1/255）之下，读回里看不见。
// 所以这里钉住的是**核的取值、clamp 语义与三趟结构**（这些都能被观测），
// 而"抽头顺序"是靠**文本**（展开写、无循环）固定的，不是靠这条场景验出来的。
//
// 这些字面量与 render/scene.rs 的 BLUR_WEIGHTS 由一条测试钉着不许漂移。
//
// 抽头**展开写、不用循环**：累加顺序必须由文本决定。循环一旦被编译器展开，
// 顺序就交给编译器了，而这个场景要测的正是顺序。
// ---------------------------------------------------------------------------
@fragment
fn fs_blur_source(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let ix = i32(floor(frag.x / 8.0));
    let iy = i32(floor(frag.y / 8.0));
    let odd = ((ix + iy) & 1) == 1;
    let base = select(0.0, 0.6, odd);
    let impulse = (frag.x >= 40.0) && (frag.x < 43.0) && (frag.y >= 40.0) && (frag.y < 43.0);
    return vec4<f32>(vec3<f32>(select(base, 1.0, impulse)), 1.0);
}

@fragment
fn fs_blur_h(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let texel = texel_of(frag.xy);
    let dim = vec2<i32>(textureDimensions(src));
    var acc = 0.036633 * load_texel(texel + vec2<i32>(-3, 0), dim);
    acc = acc + 0.111281 * load_texel(texel + vec2<i32>(-2, 0), dim);
    acc = acc + 0.216745 * load_texel(texel + vec2<i32>(-1, 0), dim);
    acc = acc + 0.270682 * load_texel(texel, dim);
    acc = acc + 0.216745 * load_texel(texel + vec2<i32>(1, 0), dim);
    acc = acc + 0.111281 * load_texel(texel + vec2<i32>(2, 0), dim);
    acc = acc + 0.036633 * load_texel(texel + vec2<i32>(3, 0), dim);
    return vec4<f32>(acc, 1.0);
}

@fragment
fn fs_blur_v(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let texel = texel_of(frag.xy);
    let dim = vec2<i32>(textureDimensions(src));
    var acc = 0.036633 * load_texel(texel + vec2<i32>(0, -3), dim);
    acc = acc + 0.111281 * load_texel(texel + vec2<i32>(0, -2), dim);
    acc = acc + 0.216745 * load_texel(texel + vec2<i32>(0, -1), dim);
    acc = acc + 0.270682 * load_texel(texel, dim);
    acc = acc + 0.216745 * load_texel(texel + vec2<i32>(0, 1), dim);
    acc = acc + 0.111281 * load_texel(texel + vec2<i32>(0, 2), dim);
    acc = acc + 0.036633 * load_texel(texel + vec2<i32>(0, 3), dim);
    return vec4<f32>(acc, 1.0);
}
