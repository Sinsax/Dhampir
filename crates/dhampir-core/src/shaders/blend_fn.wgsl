// 读回型混合：`out = f(src, dst)` —— 固定混合方程表达不了的那 5 条。
//
// 为什么需要一条单独的 pass：`f(src, dst)` 要**同时**知道来源与目标，而一个 pass 里
// 读和写同一张纹理是做不到的。所以先把"底"留在纹理里，再跑这一趟（设计见 D13）。
//
// 公式按 W3C 的混合模式定义（`B(Cb, Cs)`），合成用标准的 source-over ——
// **含 αb 的完整形式**，不只做"底不透明"那种简化：底半透明时混合结果要按 αb 插值，
// 否则一张半透明的底上的 `darken` 会算成"完全混合"（那是另一种画法）。
//
// 无分支：五条全部算出来，用 `select` 按模式码挑（允许表里 `select`/`min`/`max`/`abs`/`mix`/`pow` 都有）。

struct BlendUniform {
  // 模式码：1 暗、2 亮、3 叠加、4 柔光、5 差值。
  mode: f32,
  // **填充写成三个 f32 而不是 `vec3`**：uniform 里 `vec3<f32>` 的对齐是 16，
  // 会把整个结构体撑到 32 字节，而 Rust 那边是按 16 字节算的 —— 两边对不上时
  // 校验层会报 "bound with size 16 where the shader expects 32"。
  _padding_a: f32,
  _padding_b: f32,
  _padding_c: f32,
}

@group(0) @binding(0) var src_texture: texture_2d<f32>;
@group(0) @binding(1) var dst_texture: texture_2d<f32>;
@group(0) @binding(2) var<uniform> params: BlendUniform;

@vertex
fn vs_fullscreen(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
  var positions = array<vec2<f32>, 3>(
    vec2<f32>(-1.0, -1.0),
    vec2<f32>(3.0, -1.0),
    vec2<f32>(-1.0, 3.0),
  );
  return vec4<f32>(positions[index], 0.0, 1.0);
}

@fragment
fn fs_blend(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
  let coord = vec2<i32>(i32(position.x), i32(position.y));
  let src = textureLoad(src_texture, coord, 0);
  let dst = textureLoad(dst_texture, coord, 0);
  // **先拆预乘。** 这条链上的纹理都是**预乘**的（合成器的 `SrcAlpha/OneMinusSrcAlpha`
  // 在写入时就把 alpha 乘进去了），而 W3C 的混合公式是按**直通色**定义的。
  // 把预乘值直接当直通色用会**多乘一次 alpha** —— 症状是"半透明那一层算得太暗"，
  // 而错误的样子看起来完全合理（所以这条由逐通道对期望值的判据抓出来）。
  let ab = dst.a;
  let a_s = src.a;
  let s = src.rgb / max(a_s, 1e-6);
  let d = dst.rgb / max(ab, 1e-6);

  // ---- 五条混合函数 ----
  let darken = min(s, d);
  let lighten = max(s, d);
  let difference = abs(s - d);
  let overlay = select(2.0 * s * d, 1.0 - 2.0 * (1.0 - s) * (1.0 - d), d > vec3<f32>(0.5));
  let soft_light = select(
    d - (1.0 - 2.0 * s) * d * (1.0 - d),
    d + (2.0 * s - 1.0) * (pow(d, vec3<f32>(0.5)) - d),
    s > vec3<f32>(0.5),
  );
  let mode = params.mode;
  let blended = select(
    select(select(select(darken, lighten, mode > 1.5), overlay, mode > 2.5), soft_light, mode > 3.5),
    difference,
    mode > 4.5,
  );

  // ---- W3C 的合成：Cs' = (1 − αb)·Cs + αb·B(Cb, Cs)，再 source-over ----
  let cs = mix(s, blended, ab);
  let ao = a_s + ab * (1.0 - a_s);
  // 输出**也是预乘的**（这一趟不混合，写进去什么就是什么）。
  let co = a_s * cs + ab * d * (1.0 - a_s);
  return vec4<f32>(co, ao);
}
