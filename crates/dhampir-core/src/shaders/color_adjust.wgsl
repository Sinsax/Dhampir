// 逐像素色彩调整：亮度 / 对比度 / 饱和度 / 色调。
//
// # 为什么是一个着色器而不是四个
//
// 这四种调整都是**同一条公式链**上的系数：先动亮度（加），再动对比度（绕中灰缩放），
// 最后动饱和度（向亮度插值）。合成一个着色器有三个好处：
//
//   1. 只申报一次构造（普查的允许表不用为四个特效各加一行）；
//   2. 参数为恒等值时**整条链退化成原样**，逐像素可验证；
//   3. 用户同时挂亮度和饱和度时不必走两趟 —— 少一趟就少一次纹理往返。
//
// # 为什么没有分支
//
// 《WGSL 可移植性子集》禁掉分支，因为带副作用的分支会让两个编译器产生不同结果。
// 这里用**恒等值**代替分支：不想要的那一项，它的系数就传 1.0 或 0.0，
// 公式照常算完，结果不变。这样文本里一个 `if` 都没有。
//
// # 取值边界
//
// 渲染色是**非预乘**的 RGBA。调整只作用于 RGB，alpha **原样透传** ——
// 动 alpha 会让叠加结果在两端分叉（合成用的是预乘域，两边舍入不同）。

struct ColorAdjustUniform {
  // 亮度：加性偏移，单位是 [-1, 1] 的归一化色值。
  brightness: f32,
  // 对比度：绕 0.5 中灰缩放。1.0 = 不变。
  contrast: f32,
  // 饱和度：向亮度灰度插值。1.0 = 不变，0.0 = 完全灰度。
  saturation: f32,
  // 色调：色相旋转角，单位**弧度**。0.0 = 不变。
  hue: f32,
}

@group(0) @binding(0) var color_source: texture_2d<f32>;
@group(0) @binding(1) var<uniform> adjust: ColorAdjustUniform;

@vertex
fn vs_fullscreen(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
  var positions = array<vec2<f32>, 3>(
    vec2<f32>(-1.0, -1.0),
    vec2<f32>(3.0, -1.0),
    vec2<f32>(-1.0, 3.0),
  );
  return vec4<f32>(positions[index], 0.0, 1.0);
}

// 亮度权重（Rec.709）。与 core 里算灰度用的是同一组系数 ——
// 两端各写一份迟早在某个颜色上差一档。
fn luma(c: vec3<f32>) -> f32 {
  return dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
}

@fragment
fn fs_color_adjust(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
  let at = vec2<i32>(position.xy);
  let src = textureLoad(color_source, at, 0);
  let rgb = src.rgb;

  // 1) 亮度：加性偏移。
  let lit = rgb + vec3<f32>(adjust.brightness);

  // 2) 对比度：绕 0.5 缩放。contrast=1 时 (lit-0.5)*1+0.5 == lit，恒等。
  let gray = vec3<f32>(0.5);
  let contrasted = (lit - gray) * adjust.contrast + gray;

  // 3) 饱和度：向亮度插值。sat=1 时原样，sat=0 时完全灰度。
  let l = luma(contrasted);
  let saturated = mix(vec3<f32>(l), contrasted, adjust.saturation);

  // 4) 色调：绕灰度轴旋转 RGB。hue=0 时 cos=1、sin=0，结果恒等。
  //    这是 YIQ 空间里的标准色相旋转矩阵，展开写以避免分支。
  let c = cos(adjust.hue);
  let s = sin(adjust.hue);
  let m = mat3x3<f32>(
    vec3<f32>(0.299 + 0.701 * c + 0.168 * s, 0.587 - 0.587 * c + 0.330 * s, 0.114 - 0.114 * c - 0.497 * s),
    vec3<f32>(0.299 - 0.299 * c - 0.328 * s, 0.587 + 0.413 * c + 0.035 * s, 0.114 - 0.114 * c + 0.292 * s),
    vec3<f32>(0.299 - 0.300 * c + 1.250 * s, 0.587 - 0.588 * c - 1.050 * s, 0.114 + 0.886 * c - 0.203 * s),
  );
  let hued = m * saturated;

  // alpha 原样透传：调整不动 alpha。
  return vec4<f32>(hued, src.a);
}
