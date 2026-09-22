// 把一层画进目标纹理：变换 + 不透明度 + alpha 混合。
//
// 三条约束都来自实测与既有约定，不是口味：
//   1. 采样只用 textureLoad（整型坐标 + mip 0）——子集禁掉隐式 LOD 采样（见 blit.wgsl 的注释）；
//   2. **不用 discard**：越界返回全透明，靠混合方程让目标保持不变。
//      discard 会引入一个"片段被丢弃"的状态，而透明混合是纯算术，两端更容易算得一模一样；
//   3. 变换用**逆矩阵**在片元着色器里回算源坐标，而不是在顶点阶段摆四边形——
//      这样顶点只有全屏三角形一个形状，变换也不受光栅化插值影响。

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

@fragment
fn fs_layer(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
  // position.xy 是目标纹理上的像素中心坐标。
  let src_x = layer.inv_row0.x * position.x + layer.inv_row0.y * position.y + layer.inv_row0.z;
  let src_y = layer.inv_row1.x * position.x + layer.inv_row1.y * position.y + layer.inv_row1.z;
  // 先拼成 vec2<f32> 再整体转 i32：blit.wgsl 里验证过 vec2<i32>(vec2<f32>) 这条路，
  // 而 vec2<i32>(标量, 标量) 会被 naga 拒掉（Composing 0's component type is not expected）。
  let src = vec2<f32>(floor(src_x), floor(src_y));
  let coords = vec2<i32>(src);
  let width = i32(layer.source_size.x);
  let height = i32(layer.source_size.y);
  if (coords.x < 0 || coords.y < 0 || coords.x >= width || coords.y >= height) {
    // 越界 = 这一层没盖到这里。返回全透明，混合方程会让目标原样留着。
    return vec4<f32>(0.0, 0.0, 0.0, 0.0);
  }
  let texel = textureLoad(source_texture, coords, 0);
  // 乘不透明度：转场、淡入淡出、关键帧最终都落到这一个乘法上。
  return vec4<f32>(texel.rgb, texel.a * layer.opacity);
}
