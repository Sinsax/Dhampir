// 两趟可分离高斯模糊：横向一趟、纵向一趟，同一个入口、靠 direction 区分。
//
// # 为什么抽头是**展开写**的
//
// 《WGSL 可移植性子集》禁掉循环的理由是「累加顺序会交给编译器，而顺序必须由文本决定」
// （plan/wgsl-portable-subset.md 的禁止表与替代写法表）。模糊正好是典型的循环累加，
// 所以这里把 33 个抽头逐行写出来——顺序、个数、权重都写在文本里，
// 两个编译器没有发挥余地。
//
// # 权重为什么从 Rust 传进来
//
// 高斯权重需要 exp()。与其在着色器里再实现一遍（还要申报新构造、还要保证两边算得一样），
// 不如在 Rust 里算一次、打包进 uniform：**权重只有一份来源**，两端不可能算出不同的核。
// 超出 radius 的抽头权重是 0，所以不需要在着色器里重新归一。
//
// # 边界
//
// 越界坐标用 clamp 夹到边缘（clamp-to-edge）。夹边之后权重和仍是 1，
// 所以亮度不会被边界改变——这一点由「纯白图模糊后仍是纯白」那条测试钉住。

struct BlurUniform {
  // 33 个权重打包成 9 个 vec4（末尾几个分量是 0）。
  weights: array<vec4<f32>, 9>,
  // 抽头方向：(1,0) 横向、(0,1) 纵向。
  direction: vec2<f32>,
  size: vec2<f32>,
}

@group(0) @binding(0) var blur_source: texture_2d<f32>;
@group(0) @binding(1) var<uniform> blur: BlurUniform;

@vertex
fn vs_fullscreen(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
  var positions = array<vec2<f32>, 3>(
    vec2<f32>(-1.0, -1.0),
    vec2<f32>(3.0, -1.0),
    vec2<f32>(-1.0, 3.0),
  );
  return vec4<f32>(positions[index], 0.0, 1.0);
}

// 取一个抽头，越界夹到边缘。
fn tap(base: vec2<i32>, step: vec2<i32>, offset: i32, limit: vec2<i32>) -> vec4<f32> {
  let at = base + step * offset;
  let clamped = vec2<i32>(clamp(at.x, 0, limit.x), clamp(at.y, 0, limit.y));
  return textureLoad(blur_source, clamped, 0);
}

@fragment
fn fs_blur(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
  let base = vec2<i32>(position.xy);
  let limit = vec2<i32>(i32(blur.size.x) - 1, i32(blur.size.y) - 1);
  let step = vec2<i32>(blur.direction);
  var sum = vec4<f32>(0.0);
  sum = sum + tap(base, step, -16, limit) * blur.weights[0].x;
  sum = sum + tap(base, step, -15, limit) * blur.weights[0].y;
  sum = sum + tap(base, step, -14, limit) * blur.weights[0].z;
  sum = sum + tap(base, step, -13, limit) * blur.weights[0].w;
  sum = sum + tap(base, step, -12, limit) * blur.weights[1].x;
  sum = sum + tap(base, step, -11, limit) * blur.weights[1].y;
  sum = sum + tap(base, step, -10, limit) * blur.weights[1].z;
  sum = sum + tap(base, step, -9, limit) * blur.weights[1].w;
  sum = sum + tap(base, step, -8, limit) * blur.weights[2].x;
  sum = sum + tap(base, step, -7, limit) * blur.weights[2].y;
  sum = sum + tap(base, step, -6, limit) * blur.weights[2].z;
  sum = sum + tap(base, step, -5, limit) * blur.weights[2].w;
  sum = sum + tap(base, step, -4, limit) * blur.weights[3].x;
  sum = sum + tap(base, step, -3, limit) * blur.weights[3].y;
  sum = sum + tap(base, step, -2, limit) * blur.weights[3].z;
  sum = sum + tap(base, step, -1, limit) * blur.weights[3].w;
  sum = sum + tap(base, step, 0, limit) * blur.weights[4].x;
  sum = sum + tap(base, step, 1, limit) * blur.weights[4].y;
  sum = sum + tap(base, step, 2, limit) * blur.weights[4].z;
  sum = sum + tap(base, step, 3, limit) * blur.weights[4].w;
  sum = sum + tap(base, step, 4, limit) * blur.weights[5].x;
  sum = sum + tap(base, step, 5, limit) * blur.weights[5].y;
  sum = sum + tap(base, step, 6, limit) * blur.weights[5].z;
  sum = sum + tap(base, step, 7, limit) * blur.weights[5].w;
  sum = sum + tap(base, step, 8, limit) * blur.weights[6].x;
  sum = sum + tap(base, step, 9, limit) * blur.weights[6].y;
  sum = sum + tap(base, step, 10, limit) * blur.weights[6].z;
  sum = sum + tap(base, step, 11, limit) * blur.weights[6].w;
  sum = sum + tap(base, step, 12, limit) * blur.weights[7].x;
  sum = sum + tap(base, step, 13, limit) * blur.weights[7].y;
  sum = sum + tap(base, step, 14, limit) * blur.weights[7].z;
  sum = sum + tap(base, step, 15, limit) * blur.weights[7].w;
  sum = sum + tap(base, step, 16, limit) * blur.weights[8].x;
  return sum;
}
