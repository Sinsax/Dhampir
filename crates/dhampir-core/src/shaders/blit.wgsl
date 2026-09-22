// dhampir · 源帧 blit（渲染图的第一级）
//
// 把**源帧纹理**原样画进 sink。它是渲染图里最短的一条：一个节点，输入是帧、输出是目标。
// 预览与出片共用它——区别只在 FrameSink 的实现（canvas surface vs 离屏纹理）。
//
// 与 scene.wgsl 同一套约束（WebGPU 能力下限），理由见 plan/wgsl-portable-subset.md：
//   - 采样一律 textureLoad：按整型坐标 + 显式 mip 0 取纹素，不涉及导数、不涉及滤波精度。
//     这不只是风格：**texture_external 没有 textureLoad 重载**（实测编译失败），
//     而子集禁掉了隐式 LOD 采样（textureSample）。两者合起来就是
//     plan/s3.1-source-frame-sampling.md 选「先拷进 texture_2d」的原因。
//   - 不用分支、不用循环、不用 sampler。

@group(0) @binding(0) var source_texture: texture_2d<f32>;

@vertex
fn vs_fullscreen(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    var corners = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0),
    );
    return vec4<f32>(corners[index], 0.0, 1.0);
}

@fragment
fn fs_source(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    // 目标像素中心 = 源纹素中心，逐纹素恒等搬运（不缩放、不翻转）。
    // 下标由 @builtin(position) 截断而来：像素中心本来就是 x.5，截断得到的正是 x。
    return textureLoad(source_texture, vec2<i32>(position.xy), 0);
}
