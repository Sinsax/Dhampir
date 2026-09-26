// 常量色叠加：闪白 / 暗角 / 噪声 / 纯色·渐变覆盖（T10 的 ColorMask 管线）。
//
// # 为什么四个特效合成一个着色器
//
// 与 color_adjust.wgsl 同一条理由：它们都是**逐像素**算子，且各自都是一个
// "往结果里混多少某个颜色"的问题。合成一个着色器意味着：
//
//   1. 只申报一次构造（普查的允许表不用为四个特效各加一行）；
//   2. 四项的强度都为 0 时**整条链退化成原样**（恒等可验证）；
//   3. 用户同时挂闪白与暗角时不必走两趟 —— 少一趟就少一次纹理往返。
//
// # 为什么没有分支
//
// 《WGSL 可移植性子集》禁掉分支。这里用**强度 0 代替分支**：
// 不想要的那一项，它的 amount 传 0，公式照常算完，结果不变。
//
// # 为什么噪声不用伪随机函数
//
// 噪声必须是**帧号的纯函数**（见 schema::Window 的注释）：用系统随机或
// 时间累积会让"预览跳到中间某帧"与"成片顺序播放到那一帧"给出不同的图，
// 而那种差异只在成片里看得出来。这里用一个整数哈希：
// 同样的 (x, y, frame, seed) 永远同样的值，且两端位运算语义一致。
//
// # 取值边界
//
// 颜色一律**非预乘** RGBA。三项叠加都只作用于 RGB，alpha **原样透传** ——
// 动 alpha 会让结果在预乘域的合成里与另一端分叉。

struct ColorMaskUniform {
  // 闪白：amount 0..1，颜色由 flash_rgb 给。
  flash_amount: f32,
  flash_r: f32,
  flash_g: f32,
  flash_b: f32,
  // 暗角：amount 是边缘压暗量，radius 是暗角起始半径（归一化），softness 是过渡宽度。
  vignette_amount: f32,
  vignette_radius: f32,
  vignette_softness: f32,
  // 噪声：amount 是叠加强度，seed 决定这一份噪声长什么样。
  noise_amount: f32,
  noise_seed: f32,
  // 覆盖层：amount 是覆盖强度，主色（覆盖色）与副色（渐变另一端）。
  overlay_amount: f32,
  overlay_r: f32,
  overlay_g: f32,
  overlay_b: f32,
  overlay_r2: f32,
  overlay_g2: f32,
  overlay_b2: f32,
  // shape: 0=纯色 1=线性渐变 2=径向渐变；angle 是线性渐变的方向（弧度）。
  overlay_shape: f32,
  overlay_angle: f32,
  // 画面尺寸（像素），算暗角的归一化坐标用。
  width: f32,
  height: f32,
  // 当前帧号。噪声要它才"每一帧不同、同一帧可复现"。
  frame: f32,
}

@group(0) @binding(0) var mask_source: texture_2d<f32>;
@group(0) @binding(1) var<uniform> mask: ColorMaskUniform;

@vertex
fn vs_fullscreen(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
  var positions = array<vec2<f32>, 3>(
    vec2<f32>(-1.0, -1.0),
    vec2<f32>(3.0, -1.0),
    vec2<f32>(-1.0, 3.0),
  );
  return vec4<f32>(positions[index], 0.0, 1.0);
}

// 整数哈希：同样的输入永远同样的输出。
//
// **故意不用 fract(sin(x)*43758.5453)** —— 那个经典写法的结果依赖 sin 的
// 精度，两个编译器可以对同一个中值输入给出不同的最后几位，于是"同一帧
// 在预览与成片里噪声不同"。整数运算是精确的。
fn hash_u32(value: u32) -> f32 {
  var x = value;
  x = x ^ (x >> 16u);
  x = x * 0x7feb352du;
  x = x ^ (x >> 15u);
  x = x * 0x846ca68bu;
  x = x ^ (x >> 16u);
  // 取低 24 位映射到 [0, 1)：除以 2^24 是精确的（二进制可表示）。
  return f32(x & 0x00ffffffu) / 16777216.0;
}

// 一像素上的噪声值，落在 [0, 1)。
fn noise_at(x: i32, y: i32, frame: i32, seed: i32) -> f32 {
  // 把四个坐标混进一个整数。乘法用大奇数，避免相邻坐标撞在一起。
  // **全部先转 u32 再算**：`i32 * u32` 在 WGSL 里没有重载，
  // 混着用会被 naga 拒绝（这条是编译器抓出来的，不是猜的）。
  let ux = u32(x) * 73856093u;
  let uy = u32(y) * 19349663u;
  let uf = u32(frame) * 83492791u;
  let us = u32(seed) * 2654435761u;
  return hash_u32(ux ^ uy ^ uf ^ us);
}

@fragment
fn fs_color_mask(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
  let at = vec2<i32>(position.xy);
  let src = textureLoad(mask_source, at, 0);
  var rgb = src.rgb;

  // ---- 1) 闪白：向一个常量色整体插值。amount=0 时恒等。----
  let flash_color = vec3<f32>(mask.flash_r, mask.flash_g, mask.flash_b);
  rgb = mix(rgb, flash_color, mask.flash_amount);

  // ---- 2) 暗角：越靠边压得越暗。amount=0 时恒等。----
  //
  // 用**归一化的椭圆坐标**（中心 0、角落约 1），不是像素距离 ——
  // 像素距离在预览 640x360 与成片 1920x1080 里含义不同，两端就不一致了。
  let uv = vec2<f32>(position.xy / vec2<f32>(mask.width, mask.height)) - vec2<f32>(0.5);
  let dist = length(uv) * 1.41421356;
  // softness=0 时是硬边（step），用除法形式统一表达以避开分支。
  let edge = clamp((dist - mask.vignette_radius) / max(mask.vignette_softness, 1e-4), 0.0, 1.0);
  let vignette = 1.0 - edge * mask.vignette_amount;
  rgb = rgb * vignette;

  // ---- 3) 噪声：加性灰度噪声，逐像素、逐帧不同、同一帧可复现。----
  let n = noise_at(at.x, at.y, i32(mask.frame), i32(mask.noise_seed));
  rgb = rgb + vec3<f32>((n - 0.5) * mask.noise_amount);

  // ---- 4) 覆盖层：纯色 / 线性渐变 / 径向渐变。----
  //
  // shape 用**权重**表达而不是分支：
  //   solid  = 1
  //   linear = 投影到 angle 方向
  //   radial = 到中心的距离
  // 三者各自算完，按"是不是这个 shape"加权 —— 没有 if。
  let is_solid = select(0.0, 1.0, mask.overlay_shape < 0.5);
  let is_linear = select(0.0, 1.0, mask.overlay_shape >= 0.5 && mask.overlay_shape < 1.5);
  let is_radial = select(0.0, 1.0, mask.overlay_shape >= 1.5);
  let dir = vec2<f32>(cos(mask.overlay_angle), sin(mask.overlay_angle));
  let linear_t = clamp(dot(uv, dir) + 0.5, 0.0, 1.0);
  let radial_t = clamp(length(uv) * 2.0, 0.0, 1.0);
  let grad_t = is_solid + is_linear * linear_t + is_radial * radial_t;
  let color_a = vec3<f32>(mask.overlay_r, mask.overlay_g, mask.overlay_b);
  let color_b = vec3<f32>(mask.overlay_r2, mask.overlay_g2, mask.overlay_b2);
  let overlay_color = mix(color_a, color_b, clamp(grad_t, 0.0, 1.0));
  rgb = mix(rgb, overlay_color, mask.overlay_amount);

  // alpha 原样透传：常量色叠加不动 alpha。
  return vec4<f32>(rgb, src.a);
}
