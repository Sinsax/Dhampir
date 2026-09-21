// dhampir · M0 探针三角形
//
// 这个 shader 的全部意义是"同一份 WGSL 文本"：它被 include_str! 编进 crate，
// native 下由 naga 编译成 SPIR-V / HLSL，wasm 下把原文交给浏览器的 Tint。
//
// 所以这里刻意只用 WebGPU 能力下限里的构造（指导文档 §4.3①）：
//   - 没有纹理采样（`texture_external` 的可用性两端不同）
//   - 没有导数（`fwidth` / `dpdx` / `dpdy`，实现自由度大）
//   - 没有循环、没有长链浮点累加（累加顺序是差异来源）
//   - 只用 f32，不用任何精度限定的隐式行为
//
// 别在这里加"顺手优化"。M2 会把它扩展成 corpus，但"能不用就不用"的原则
// 从这一份开始就得守着——它就是基准。

struct VertexIn {
    @location(0) position: vec2<f32>,
    @location(1) color: vec3<f32>,
};

struct VertexOut {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) color: vec3<f32>,
};

@vertex
fn vs_main(input: VertexIn) -> VertexOut {
    var output: VertexOut;
    // 已经是 NDC，不做任何变换。变换矩阵要到 M2 的 corpus 才引入。
    output.clip_position = vec4<f32>(input.position, 0.0, 1.0);
    output.color = input.color;
    return output;
}

@fragment
fn fs_main(input: VertexOut) -> @location(0) vec4<f32> {
    // 直接输出线性光。目标是 Rgba8UnormSrgb，linear → sRGB 的编码由硬件做，
    // 不由我们做——把手写的转换和硬件的转换叠在一起，就再也分不清是谁的问题。
    return vec4<f32>(input.color, 1.0);
}
