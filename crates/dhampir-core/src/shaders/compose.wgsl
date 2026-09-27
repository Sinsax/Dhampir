// 把一层画进目标纹理：变换 + 不透明度 + alpha 混合。
//
// 三条约束都来自实测与既有约定，不是口味：
//   1. **采样走 `textureLoad` + 自己做双线性**（见下）——不用采样器；
//   2. **不用 discard**：越界返回全透明，靠混合方程让目标保持不变。
//      discard 会引入一个"片段被丢弃"的状态，而透明混合是纯算术，两端更容易算得一模一样；
//   3. 变换用**逆矩阵**在片元着色器里回算源坐标，而不是在顶点阶段摆四边形——
//      这样顶点只有全屏三角形一个形状，变换也不受光栅化插值影响。
//
// # 为什么要自己做双线性（这一条是修出来的）
//
// 先前这里是 `textureLoad(source_texture, vec2<i32>(floor(src)), 0)` —— **点采样**。
// 运镜放大（1.3~1.5 倍）时一个源纹素被摊成 1.3~1.5 个目标像素，边缘就成了阶梯；
// 而参照（Chrome canvas 的 `drawImage`）是**双线性**的。
//
// 实测判据（拉普拉斯能量，衡量高频）：
//
//     时刻     本仓     参照    比值
//      5.0s    713.0    335.8   2.12
//     16.0s    597.3    175.2   3.41
//     50.0s    864.5    321.3   2.69
//     平均     685.5    262.9   **2.61**
//
// **高频多出 1.6 倍 = 锯齿**。反过来说：如果是码率不够，高频会**变低**（糊），
// 所以用户报的"像素感"不是码率问题。
//
// 为什么不用采样器：`plan/wgsl-portable-subset.md` 写明了理由 ——
// **滤波精度允许实现降精度**，那会给两端引入一块不由我们控制的自由度。
// 自己写 `mix` 就没有那块自由度，且逐值确定（`mix` 是定义好的算式）。

struct LayerUniform {
  // 输出像素 -> 源像素 的两行仿射系数（第三列是平移）。
  inv_row0: vec4<f32>,
  inv_row1: vec4<f32>,
  // 源纹理尺寸，用来判越界。
  source_size: vec2<f32>,
  opacity: f32,
  _padding: f32,
}

@group(0) @binding(0) var source_texture: texture_2d<f32>;
@group(0) @binding(1) var<uniform> layer: LayerUniform;

@vertex
fn vs_fullscreen(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
  var positions = array<vec2<f32>, 3>(
    vec2<f32>(-1.0, -1.0),
    vec2<f32>(3.0, -1.0),
    vec2<f32>(-1.0, 3.0),
  );
  return vec4<f32>(positions[index], 0.0, 1.0);
}

/// 取一个纹素，**坐标钳到边缘**。
///
/// 钳制自己写（而不是让采样器做）：越界的那几个抽头要重复边缘像素，
/// 双线性在贴图边缘才会平滑收住，而不是掉到透明。
fn load_clamped(c: vec2<i32>, w: i32, h: i32) -> vec4<f32> {
  let cc = vec2<i32>(clamp(c.x, 0, w - 1), clamp(c.y, 0, h - 1));
  return textureLoad(source_texture, cc, 0);
}

@fragment
fn fs_layer(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
  // position.xy 是目标纹理上的像素中心坐标。
  let src_x = layer.inv_row0.x * position.x + layer.inv_row0.y * position.y + layer.inv_row0.z;
  let src_y = layer.inv_row1.x * position.x + layer.inv_row1.y * position.y + layer.inv_row1.z;
  let width = i32(layer.source_size.x);
  let height = i32(layer.source_size.y);

  // 越界 = 这一层没盖到这里。返回全透明，混合方程会让目标原样留着。
  if (src_x < 0.0 || src_y < 0.0 || src_x >= layer.source_size.x || src_y >= layer.source_size.y) {
    return vec4<f32>(0.0, 0.0, 0.0, 0.0);
  }

  // **双线性**。纹素 `i` 的中心在 `i + 0.5`，所以先把坐标挪进"以纹素中心为整数"的参照系，
  // 再取相邻四个抽头、按小数部分两次 `mix`。
  //
  // 先拼成 vec2<f32> 再整体转 i32：`blit.wgsl` 里验证过这条路，
  // 而 `vec2<i32>(标量, 标量)` 会被 naga 拒掉（Composing 0's component type is not expected）。
  let center = vec2<f32>(src_x - 0.5, src_y - 0.5);
  let base_f = vec2<f32>(floor(center.x), floor(center.y));
  let base = vec2<i32>(base_f);
  let frac = vec2<f32>(center.x - base_f.x, center.y - base_f.y);

  let c00 = load_clamped(vec2<i32>(base.x, base.y), width, height);
  let c10 = load_clamped(vec2<i32>(base.x + 1, base.y), width, height);
  let c01 = load_clamped(vec2<i32>(base.x, base.y + 1), width, height);
  let c11 = load_clamped(vec2<i32>(base.x + 1, base.y + 1), width, height);

  let top = mix(c00, c10, frac.x);
  let bottom = mix(c01, c11, frac.x);
  let texel = mix(top, bottom, frac.y);

  // 乘不透明度：转场、淡入淡出、关键帧最终都落到这一个乘法上。
  return vec4<f32>(texel.rgb, texel.a * layer.opacity);
}
