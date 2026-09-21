//! 媒体 I/O 契约：**帧从哪来、画到哪去**。
//!
//! 这两个 trait 是 `dhampir-core` 里唯一的"平台差异出口"（另一个是宿主里的
//! `Instance` 创建）。渲染图本身只认"给我第 N 帧"，不关心帧来自 WebCodecs 还是
//! FFmpeg——这是"一份代码两个运行时"能成立的机制。
//!
//! # M0 状态：只有签名
//!
//! 实现分别在 M2（wasm / 离屏纹理）与 M3（浏览器 / canvas 与 WebCodecs）补。
//! M0 只要求它们**能编译**，因为签名本身就是设计决策：它决定了上游渲染图
//! 能表达什么、不能表达什么。
//!
//! # 未决问题：`frame_view` 的返回类型（S3.1 决策项）
//!
//! 签名按指导文档 §4.4 落成返回 [`wgpu::TextureView`]，但这里有一条**已知的张力**，
//! 已记入 `plan/video-editor-plan.md` §11.5 与 M3 的前置 spike S3.1：
//!
//! WebCodecs 的 `VideoFrame` 走零拷贝路径时，`GPUDevice.importExternalTexture()`
//! 产出的是**外部纹理**（`GPUExternalTexture`）。它在 WGSL 里只能用
//! `texture_external` 类型绑定，只能用 `textureSampleBaseClampToEdge` 采样——
//! 没有 mipmap、没有 `repeat` 寻址（要 repeat 得自己在 shader 里 `fract`），
//! 而且它在**创建它的那个 task 结束时可能失效**。
//!
//! 也就是说：外部纹理**无法伪装成普通 `TextureView`**。零拷贝路线需要给这个
//! trait 增加一个"外部纹理"分支（例如新增 `BindingSource` 枚举或第二个方法），
//! 而"退到 `copyExternalImageToTexture`"则意味着每次采样都多一次全帧拷贝。
//!
//! 两条路的战果不同、代价也不同，所以**现在不猜**：M3 的 S3.1 会实测 1080p
//! 的拷贝成本，把结论写进决策文件，再回来改这个签名。M0 保持与指导文档一致。

use crate::wgpu;

/// 帧的来源。渲染图向它要第 N 帧的纹理视图。
///
/// 生命周期约定（重要）：返回的 [`wgpu::TextureView`] 至少要活到本帧的
/// command buffer 提交为止。实现方自己维护纹理池，调用方不持有所有权——
/// 这样"帧数据什么时候能复用"这件事由最懂它的那一层决定。
pub trait FrameSource {
    /// 第 `frame` 帧的纹理视图。
    ///
    /// - wasm 侧：WebCodecs `VideoFrame`（零拷贝为外部纹理，见模块级文档）
    /// - native 侧：FFmpeg 解码后上传，或 DMA-BUF / 共享句柄导入
    ///
    /// `frame` 是**全局帧号**（时间线坐标），不是源素材内的帧号。源内偏移的
    /// 换算由实现方负责——渲染图不应该知道"这个片段从第 300 帧开始"。
    fn frame_view(&mut self, device: &wgpu::Device, frame: i64) -> wgpu::TextureView;
}

/// 帧的去向。预览与导出共用上游渲染图，区别只在这里。
///
/// 这正是指导文档 §4.4 说的"预览与导出共用渲染图，只是 sink 不同"：
///
/// - wasm 侧：canvas surface，画给用户看
/// - native 侧：离屏 texture，交给 [`crate::readback`] 读回、再交给编码器
pub trait FrameSink {
    /// 取本帧要画进去的纹理视图。
    ///
    /// 对 canvas surface 实现，这里同时负责抓取当前 surface 纹理——
    /// 所以一帧只能调一次，重复调用会拿到失败或过期的纹理。
    fn acquire(&mut self, device: &wgpu::Device) -> wgpu::TextureView;

    /// 本帧提交完成。surface 实现在这里 `present()`，离屏实现在这里做记账。
    fn finish(&mut self, frame: i64);
}
