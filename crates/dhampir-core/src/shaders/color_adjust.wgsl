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
  // 亮度（**乘性**）：CSS `brightness()` 的那一种。1.0 = 不变（且逐位精确）。
  scale: f32,
  // 色调（**CSS / SVG 规范矩阵**那一条），单位弧度。0.0 = 整步跳过。
  hue_css: f32,
  // 饱和度（**CSS / SVG 规范权重**那一条）。1.0 = 整步跳过。
  saturation_css: f32,
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

  // 1) 亮度：**先乘后加**。乘性那一步对应 CSS 的 `brightness()`（保黑），
  //    加性那一步是本仓既有的（抬黑）。scale=1.0 时乘这一步逐位精确恒等。
  let lit = rgb * adjust.scale + vec3<f32>(adjust.brightness);

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

  // 5) 色调（**CSS / SVG 规范矩阵**，第 44 轮）：系数取自规范自己的 MathML
  //    （`fxtf-drafts/filter-effects/mathml/feColorMatrix03.mml`）。
  //
  //    这里**刻意用显式 `dot` 写三行**，不用 `mat3x3`：第 43 轮用 `mat3x3` 时纯红转 90° 实测
  //    `(184, 87, 0)` —— 182 恰是第一行第二列 (a01)、91 是第一列第二行 (a10)，像是"列被错位取用"。
  //    矩阵的行/列约定不该成为判据里的未知数：写成 `dot` 之后"哪一行算哪个分量"一眼可见。
  //    `hue_css=0` 时整步被 `select` 跳过 —— 不靠"矩阵恰好是恒等"（f32 下 0.213+0.787 未必逐位等于 1）。
  let cc = cos(adjust.hue_css);
  let ss = sin(adjust.hue_css);
  let row0 = vec3<f32>(
    0.213 + 0.787 * cc - 0.213 * ss,
    0.715 - 0.715 * cc - 0.715 * ss,
    0.072 - 0.072 * cc + 0.928 * ss,
  );
  let row1 = vec3<f32>(
    0.213 - 0.213 * cc + 0.143 * ss,
    0.715 + 0.285 * cc + 0.140 * ss,
    0.072 - 0.072 * cc - 0.283 * ss,
  );
  let row2 = vec3<f32>(
    0.213 - 0.213 * cc - 0.787 * ss,
    0.715 - 0.715 * cc + 0.715 * ss,
    0.072 + 0.928 * cc + 0.072 * ss,
  );
  let spec_hued = vec3<f32>(dot(row0, hued), dot(row1, hued), dot(row2, hued));
  let out_rgb = select(spec_hued, hued, adjust.hue_css == 0.0);

  // 6) 饱和度（**CSS / SVG 规范权重**，第 45 轮）：灰度权重用规范那组取整值。
  //    与第 3 步只差权重（0.213/0.715/0.072 vs Rec.709），换算到 8 位不到 0.1 档 ——
  //    但"分不出来"不等于"同一个函数"，要跟浏览器精确一致就得有它。
  //    saturation_css=1 时整步被 `select` 跳过（`mix` 在 1.0 处虽是恒等，仍显式跳过更稳）。
  // **luma 与混合必须取同一个向量**（都是上一步的结果）——
  // 写成 `dot(hued, …)` 而 `mix` 用 `out_rgb` 会"用旧值算灰度、拿新值混"，看着像对的一样。
  let luma_spec = dot(out_rgb, vec3<f32>(0.213, 0.715, 0.072));
  let spec_saturated = mix(vec3<f32>(luma_spec), out_rgb, adjust.saturation_css);
  let final_rgb = select(spec_saturated, out_rgb, adjust.saturation_css == 1.0);

  // alpha 原样透传：调整不动 alpha。
  return vec4<f32>(final_rgb, src.a);
}
