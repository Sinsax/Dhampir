// 坐标重映射：抖动 / 缩放弹跳 / 脉冲 / 分屏（T11 的 Warp 管线）。
//
// # 这一类与前两类的区别
//
// - color_adjust / color_mask 是**逐像素**：输出只看同一坐标的输入；
// - warp 是**邻域**：输出要看**别处**的输入（按一个位移场去取）。
//
// 所以它必须约束采样坐标的边界（见 `at` 的 clamp）—— 不约束的话，
// 位移把采样点推出纹理之外，`textureLoad` 的越界行为在不同后端上不一致。
//
// # 为什么位移场是"纯坐标的函数"
//
// 与 color_mask 的噪声同一条纪律：位移必须由**帧号 + 像素坐标**唯一决定。
// 用累积时间或随机数会让"预览跳到这一帧"与"成片播到这一帧"差几个像素，
// 而那正是最难归因的一类差异。
//
// # 为什么不采样（不用 sampler）
//
// 位移之后坐标通常落在两个纹素之间。用滤波采样器会把"选哪个 mip、
// 用什么滤波"交给实现（可移植子集里 `sampler` 不在允许表）。
// 这里**取最近的整数纹素**（floor），两端必然一致 —— 代价是位移不是亚像素级，
// 而那几个特效用的是"看着在抖"而不是"精确位移"。
//
// # 没有分支
//
// 四项各自算完，按"这一项的强度是不是 0"加权相加。与 color_mask 同法。

struct WarpUniform {
  // 抖动：amount 是位移幅度（归一化），frequency 是每秒振荡次数，seed 错开相位。
  shake_amount: f32,
  shake_frequency: f32,
  shake_seed: f32,
  // 缩放弹跳：amount 是最大放大倍数，frequency 是每秒弹跳次数。
  bounce_amount: f32,
  bounce_frequency: f32,
  // 脉冲：低频呼吸缩放。
  pulse_amount: f32,
  pulse_frequency: f32,
  // 分屏：offset 是两半分开的距离（归一化），skew 是倾斜（弧度）。
  split_offset: f32,
  split_skew: f32,
  split_amount: f32,
  // 画面尺寸与时间（秒）。位移场是它们的纯函数。
  width: f32,
  height: f32,
  seconds: f32,
}

@group(0) @binding(0) var warp_source: texture_2d<f32>;
@group(0) @binding(1) var<uniform> warp: WarpUniform;

@vertex
fn vs_fullscreen(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
  var positions = array<vec2<f32>, 3>(
    vec2<f32>(-1.0, -1.0),
    vec2<f32>(3.0, -1.0),
    vec2<f32>(-1.0, 3.0),
  );
  return vec4<f32>(positions[index], 0.0, 1.0);
}

// 把采样坐标夹进纹理内。**必须做**：位移会把点推出画面，
// 而越界 `textureLoad` 的行为不在可移植子集里。
fn clamp_texel(at: vec2<i32>, dim: vec2<i32>) -> vec2<i32> {
  let max = dim - vec2<i32>(1, 1);
  return vec2<i32>(
    clamp(at.x, 0, max.x),
    clamp(at.y, 0, max.y),
  );
}

@fragment
fn fs_warp(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
  let dim = vec2<i32>(i32(warp.width), i32(warp.height));
  let frag = vec2<f32>(position.xy);
  // 归一化到 [-0.5, 0.5]：位移与缩放都用它算，与像素尺寸无关。
  let uv = frag / vec2<f32>(warp.width, warp.height) - vec2<f32>(0.5);

  // ---- 1) 抖动：两路不同频率的正弦，纵横各一（不成整数比，避免机械感）。----
  let t = warp.seconds;
  let phase = warp.shake_seed * 1.7;
  let shake = vec2<f32>(
    sin(t * warp.shake_frequency * 6.2831853 + phase),
    sin(t * warp.shake_frequency * 4.7123890 + phase + 1.3),
  ) * warp.shake_amount;

  // ---- 2) 缩放弹跳：1 + amount * |sin| —— 用绝对值让它在 1 处"触底"。----
  let bounce = abs(sin(t * warp.bounce_frequency * 6.2831853)) * warp.bounce_amount;

  // ---- 3) 脉冲：低频正弦，可正可负（向外胀/向内收）。----
  let pulse = sin(t * warp.pulse_frequency * 6.2831853) * warp.pulse_amount;

  // 缩放合成：三者的效果都是"把采样点从中心缩放出去"。
  // zoom>1 表示**放大画面**，即采样点向中心收 —— 所以取倒数。
  let zoom = 1.0 + bounce + pulse;

  // ---- 4) 分屏：左右两半各自向两侧移开，并绕中线倾斜。----
  // 用 select 表达"在哪一半"，而不是 if：两边都算完，选一个。
  let is_right = select(0.0, 1.0, uv.x >= 0.0);
  let side = select(-1.0, 1.0, uv.x >= 0.0);
  let split_shift = vec2<f32>(side * warp.split_offset * warp.split_amount, 0.0);
  // 倾斜：离中线越远，纵向偏移越大（绕中线转）。
  let split_shear = vec2<f32>(0.0, uv.x * tan(warp.split_skew) * warp.split_amount);
  // 左半与右半各自平移的量对消，所以这里加上各自的一半。
  let split_total = split_shift * (0.5 + is_right * 0.0) + split_shear;

  // 合成采样点：先缩再移再抖。顺序固定（写在文本里），两端一致。
  var sample_at = uv / zoom + shake + split_total;
  let texel = sample_at * vec2<f32>(warp.width, warp.height) + vec2<f32>(warp.width, warp.height) * 0.5;

  let at = clamp_texel(vec2<i32>(texel), dim);
  return textureLoad(warp_source, at, 0);
}
