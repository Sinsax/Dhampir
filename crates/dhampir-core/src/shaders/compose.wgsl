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
  // 圆角：x = 半径（**源像素**）、y = 抗锯齿斜坡宽度（源像素）。半径 0 = 不裁。
  corner: vec4<f32>,
  // 裁剪形状的参数，都在**源像素**参照系里（换算见 Rust 侧 `clip_params`）：
  //   clip_a = (形状码, 中心 x, 中心 y, 圆的半径)
  //   clip_b = (半宽 或 椭圆 rx, 半高 或 椭圆 ry, 内缩矩形的圆角, 备用)
  clip_a: vec4<f32>,
  clip_b: vec4<f32>,
  // 掩码开关（都是 0/1）：x = 有没有掩码、y = 是否反相、z = 用亮度（否则 alpha）、w = 备用。
  mask_a: vec4<f32>,
  // 染色：rgb 是要换上的颜色、a 是"要不要染"（0/1）。
  //
  // 用途是**投影**：同一层的纹理再画一张时把 rgb 换成阴影色（v1 固定黑），
  // 而 alpha 保持原样 —— 形状还是那个形状。
  // a = 0 时走 `select` 的那一边，输出与不染色**逐字节相同**。
  tint: vec4<f32>,
}

@group(0) @binding(0) var source_texture: texture_2d<f32>;
@group(0) @binding(1) var<uniform> layer: LayerUniform;
// 掩码纹理。没有掩码时绑的是一张 1×1 的白图（`mask_a.x = 0` 会把它整个旁路掉）。
//
// **不用采样器**：与源纹理同一条纪律（滤波精度不许留给实现），所以双线性也自己写。
@group(0) @binding(2) var mask_texture: texture_2d<f32>;

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

/// 取一个掩码纹素，**坐标钳到边缘**（与源那份同理，只是换了一张纹理）。
fn load_mask_clamped(c: vec2<i32>, w: i32, h: i32) -> vec4<f32> {
  let cc = vec2<i32>(clamp(c.x, 0, w - 1), clamp(c.y, 0, h - 1));
  return textureLoad(mask_texture, cc, 0);
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

  // **圆角**：在本层自己的矩形里算一个无分支的覆盖度。
  //
  // 为什么在**源像素**参照系里算：片元手里只有 `src_x/src_y`（逆变换算出来的），
  // 而本层的矩形在源空间里就是 [0,w]×[0,h] —— 不必再传一套局部坐标进来。
  // 半径与斜坡宽度也都由 Rust 侧换算成源像素（见 LayerUniform 的注释）。
  //
  // 为什么没有分支：允许表里 `if (` 只申报了一处（层的越界判定）。这里用 `select`
  // 把「半径 > 0 才裁」压成一个因子；半径 0 时它**精确地**是 0，于是这条路径的输出
  // 与从前逐字节相同（D10 的第一条判据）。
  //
  // 形状用圆角矩形的标准 SDF；覆盖度用一条约 1 像素宽的**线性**斜坡
  // （不用 `smoothstep` —— 它不在允许表里）。
  let half = layer.source_size * 0.5;
  let r = layer.corner.x;
  let p = vec2<f32>(src_x, src_y) - half;
  let q = abs(p) - (half - vec2<f32>(r, r));
  let dist = length(max(q, vec2<f32>(0.0))) + min(max(q.x, q.y), 0.0) - r;
  let coverage = clamp(0.5 - dist / layer.corner.y, 0.0, 1.0);
  let masking = select(0.0, 1.0, r > 0.0);

  // **裁剪形状**：与圆角同一套「每像素覆盖度」，最后两个覆盖度相乘。
  //
  // 形状码在 clip_a.x：0 不裁 / 1 圆 / 2 椭圆 / 3 内缩矩形。分派用 `select` —— 无分支，
  // 且**两个分支都会被求值**（所以在没有裁剪时 Rust 侧给的是安全占位值，不是 0，
  // 免得未选中的那一支算出 0/0）。
  //
  // 椭圆用的是「把圆按半径归一化」那个近似：**零集是精确的**（椭圆内外的判定没错），
  // 差的只是远处的距离值 —— 而距离只用在做抗锯齿的那一圈，够用。
  let cpc = vec2<f32>(src_x, src_y) - layer.clip_a.yz;
  let d_circle = length(cpc) - layer.clip_a.w;
  let d_ellipse = (length(cpc / layer.clip_b.xy) - 1.0) * min(layer.clip_b.x, layer.clip_b.y);
  let cqi = abs(cpc) - (layer.clip_b.xy - vec2<f32>(layer.clip_b.z, layer.clip_b.z));
  let d_inset = length(max(cqi, vec2<f32>(0.0))) + min(max(cqi.x, cqi.y), 0.0) - layer.clip_b.z;
  let shape = layer.clip_a.x;
  let d_none = -1.0e9;
  let d_shape = select(select(select(d_none, d_circle, shape > 0.5), d_ellipse, shape > 1.5), d_inset, shape > 2.5);
  let clip_coverage = clamp(0.5 - d_shape / layer.corner.y, 0.0, 1.0);
  let clip_on = select(0.0, 1.0, shape > 0.5);

  // **掩码**：整张掩码图铺在**图层自己的矩形**上（D12 的第三条决定）——
  // 而片元手里的 `src_x/src_y` 就是源像素坐标，源矩形正是图层矩形，所以归一化坐标
  // 直接由它除出来，不需要再传一套局部坐标。
  //
  // 双线性自己写（与源纹理同一套写法）：掩码图通常与图层不同分辨率，最近邻会在
  // 掩码内部留下块状边界 —— 而 DOM 侧的 CSS `mask-image` 是**滤波**的，两边就对不上了。
  let mask_on = layer.mask_a.x;
  let mask_size = vec2<f32>(textureDimensions(mask_texture, 0));
  let mask_uv = vec2<f32>(src_x / layer.source_size.x, src_y / layer.source_size.y);
  let mask_pos = vec2<f32>(mask_uv.x * mask_size.x - 0.5, mask_uv.y * mask_size.y - 0.5);
  let mask_base_f = vec2<f32>(floor(mask_pos.x), floor(mask_pos.y));
  let mask_base = vec2<i32>(mask_base_f);
  let mask_frac = vec2<f32>(mask_pos.x - mask_base_f.x, mask_pos.y - mask_base_f.y);
  let mask_w = i32(mask_size.x);
  let mask_h = i32(mask_size.y);
  let m00 = load_mask_clamped(vec2<i32>(mask_base.x, mask_base.y), mask_w, mask_h);
  let m10 = load_mask_clamped(vec2<i32>(mask_base.x + 1, mask_base.y), mask_w, mask_h);
  let m01 = load_mask_clamped(vec2<i32>(mask_base.x, mask_base.y + 1), mask_w, mask_h);
  let m11 = load_mask_clamped(vec2<i32>(mask_base.x + 1, mask_base.y + 1), mask_w, mask_h);
  let mask_top = mix(m00, m10, mask_frac.x);
  let mask_bottom = mix(m01, m11, mask_frac.x);
  let mask_texel = mix(mask_top, mask_bottom, mask_frac.y);
  // 通道：alpha 或**亮度**（Rec.709，与 core 的 luma 同一组系数 —— D6 的老规矩）。
  let mask_alpha = mask_texel.a;
  let mask_luma = dot(mask_texel.rgb, vec3<f32>(0.2126, 0.7152, 0.0722));
  let mask_value = select(mask_alpha, mask_luma, layer.mask_a.z > 0.5);
  let mask_applied = select(mask_value, 1.0 - mask_value, layer.mask_a.y > 0.5);

  // **染色**：投影那一张把颜色换成阴影色，形状（alpha）不动。
  // `select` 两支都求值，所以不染色的那条路必须**逐字节同旧** —— 它取的正是 `texel.rgb`。
  let tinted = select(texel.rgb, layer.tint.rgb, layer.tint.a > 0.5);

  // 乘不透明度：转场、淡入淡出、关键帧最终都落到这一个乘法上。
  // 圆角、裁剪、掩码各再乘一个覆盖度 —— 不裁/不掩时三者都是精确的 1.0。
  return vec4<f32>(
    tinted,
    texel.a * layer.opacity * mix(1.0, coverage, masking) * mix(1.0, clip_coverage, clip_on)
      * mix(1.0, mask_applied, mask_on),
  );
}
