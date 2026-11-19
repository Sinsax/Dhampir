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
  //
  // ⚠️⚠️ **`rgb` 在这里是 0..1，不是 0..255** —— 这是本段唯一要说清的事。
  //
  // `mask_source` 的格式是 `Rgba8Unorm`。**`Unorm` 的 `textureLoad` 返回 0.0..1.0**
  // （硬件把 8 位整数归一化到浮点），**不是裸的 0..255**。
  // 同一函数里第 1 段就是活证据：
  //   * `mix(rgb, flash_color, flash_amount)` —— `flash_color` 由
  //     `param("r").unwrap_or(1.0)` 给出、在 Rust 侧 `clamp(0.0, 1.0)`；若 `rgb` 是 0..255，
  //     这一混会把画面压成 1/255 的黑。**实测定性：`flash` 亮度正常**（峰值帧均值 122→141）。
  //
  // ---- 一段被改错又改回来的历史，别重蹈 ----
  //
  // `67ba95c` 曾在这里加 `const NOISE_FULL_SCALE: f32 = 255.0`，理由是
  // "`textureLoad` 出来是 0..255、旧式增量只有 ±0.5 连一个量化台阶都不到、
  // 所以 `noise` 完全不可见"。**那个前提是错的**，于是"修复"把噪声**放大 255 倍**：
  // `amount = 0.08`（下游实际用法）的增量变成 ±10.2 **在 0..1 空间里**
  // —— 远超满幅，整幅画面被随机值淹没。
  //
  // 真机症状（YekiTrim §20 出片，1920×1080，`amount = 0.08`）：
  // 峰值帧**不是"彩条 + 颗粒"，而是一整张黑白噪点图**，饱和度 **208.8 → 92.5**（-56%）。
  //
  // 那次提交**自己记下了一条对不上的读数**（"实测到 `+171` 的跳变，
  // 超过了单趟满幅上界 10.2……剩下的嫌疑是调整层那趟的 strength 折算，我没跟到底"）。
  // **那条异常就是"上界算错了"的告警**：`10.2` 是 0..255 空间的上界，
  // 而量到的 `+171` 是 0..1 空间的 —— 两者差 255 倍，正好对上。
  // ⇒ **教训：一条"超过理论上界"的读数，要先怀疑上界（量纲），别先怀疑实现。**
  //
  // ---- 那么"完全不可见"到底是不是真的？----
  //
  // 不是。`(n - 0.5) * amount` 在 `amount = 0.08` 时是 ±0.04（0..1 空间）
  // = **±10.2 灰阶** —— 温和但明确可见。"挂上/不挂逐字节相同"那个观察，
  // 现在回看更像是**调整层整层没跑到**（`is_adjustment` 没置位那类问题）——
  // `dhampir-worker` 的噪声用例注释里记着同一个坑："我第一版漏了它们 ——
  // 于是 `adj` 被当成普通图层，`noise` 整条没跑到，症状正好是挂上没挂上一个样"。
  // ⇒ **当时是两个独立缺陷叠在一起：一个是"没跑到"（已由那条用例钉住），
  //    一个是这次的量纲** —— 修好前者之后，后者才暴露成"洗掉画面"。
  let n = noise_at(at.x, at.y, i32(mask.frame), i32(mask.noise_seed));
  rgb = rgb + vec3<f32>((n - 0.5) * mask.noise_amount);

  // ---- 4) 覆盖层：纯色 / 线性渐变 / 径向渐变。----
  //
  // shape 用**权重**表达而不是分支：
  //   solid  = **0**（沿用 `color_a`，即 `r/g/b`）
  //   linear = 投影到 angle 方向（0 → color_a，1 → color_b）
  //   radial = 到中心的距离（同上）
  // 三者各自算完，按"是不是这个 shape"加权 —— 没有 if。
  //
  // ⚠️ **`solid` 不复用 `is_solid` 这个权重**（2026-10-01 修）：它以前写成
  // `grad_t = is_solid + …`，于是纯色落到 `mix` 的 1 端 ⇒ 取的是 **`color_b`（`r2/g2/b2`）**，
  // 与 `OVERLAY` 文档"渐变的第二个颜色用 r2/g2/b2"矛盾。平时看不出来，是因为参数打包让
  // `r2` 缺省回落到 `r`（`color_mask_params`），只有"纯色 + 显式 r2≠r"才露。
  // 实测下游转译器的纯色把两组写成一样（`polish-to-dhampir.mjs` 的 `solidEffect`），
  // 所以这条修正**不改变任何现有工程的画面**。
  let is_linear = select(0.0, 1.0, mask.overlay_shape >= 0.5 && mask.overlay_shape < 1.5);
  let is_radial = select(0.0, 1.0, mask.overlay_shape >= 1.5);
  let dir = vec2<f32>(cos(mask.overlay_angle), sin(mask.overlay_angle));
  let linear_t = clamp(dot(uv, dir) + 0.5, 0.0, 1.0);
  let radial_t = clamp(length(uv) * 2.0, 0.0, 1.0);
  let grad_t = is_linear * linear_t + is_radial * radial_t;
  let color_a = vec3<f32>(mask.overlay_r, mask.overlay_g, mask.overlay_b);
  let color_b = vec3<f32>(mask.overlay_r2, mask.overlay_g2, mask.overlay_b2);
  let overlay_color = mix(color_a, color_b, clamp(grad_t, 0.0, 1.0));
  rgb = mix(rgb, overlay_color, mask.overlay_amount);

  // alpha 原样透传：常量色叠加不动 alpha。
  return vec4<f32>(rgb, src.a);
}
