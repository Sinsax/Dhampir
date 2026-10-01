//! 动图（GIF / 动画 WebP）的**纹理缓存**：解好的帧常驻显存，按 (asset_id, 帧号) 取。
//!
//! # 它在接缝上的位置
//!
//! 渲染器按 [`SourceResolver::texture_for`](crate::render::timeline::SourceResolver::texture_for)
//! 要纹理，而这一层的签名恰好就是「(源名, 源内帧号) → 纹理」——
//! 动图缓存是它的**一个实现**，不是第二条渲染路径（方案 §8.3「零新渲染路径」的兑现处）。
//! 两个宿主各持一份，查表顺序都是「动图缓存优先，再落到 video / 解码器」。
//!
//! # 为什么在 core
//!
//! 与解码器同一条理由：两端要同一个实现。放宿主里就会有两份缓存策略，
//! 而「同一张贴纸在预览里是第 30 帧、在出片里是第 31 帧」这种错**单帧看不出来**。
//!
//! # 尺寸：为什么按**原生尺寸**上传
//!
//! 方案 §8.3-3 讨论过「按目标盒子解码」以省内存。这里做的是另一条：
//! **按原生尺寸上传，交给 GPU 采样缩放**（与视频源完全同路）。
//! 理由是**逐帧一致性优先**：两端拿到的源像素因此逐字节相同，
//! 缩放的差异被限制在采样器上，而不是"解码时各缩各的"。代价是显存按原生尺寸算，
//! 所以有 [`AnimationTextures::budget_bytes`] 这道闸 —— 超了就**明确拒绝**，
//! 让宿主把那一层报成"画不出来"，而不是把显存悄悄吃光。
//!
//! # 帧号越界怎么算
//!
//! 求值层给出的帧号**可能超出文件帧数**：不循环的层（`loop_source = false`）走的是
//! `source_in + i`，跑到素材末尾之后就没有对应帧了。这里的口径是**停在最后一帧**
//! （与 `source_frame_at_delays` 在不循环时对时间做的钳制同一套语义）；
//! 负帧号落到第 0 帧。循环层的取模在求值层就做完了，到这里总是合法下标。

use std::collections::BTreeMap;

use crate::animation::{Animation, AnimError, AnimFormat};
use crate::wgpu;

/// 一张动图上传后的账目与句柄。
struct Entry {
    /// 保住纹理对象：`TextureView` 不持有它的生命周期。
    textures: Vec<(wgpu::Texture, wgpu::TextureView)>,
    width: u32,
    height: u32,
    format: AnimFormat,
    loop_count: u32,
    /// 逐帧延迟表 —— 与契约里的 `frame_delays_ms` 是同一份真值（方案 §8.3-2）。
    delays_ms: Vec<u32>,
    total_ms: u64,
}

impl Entry {
    fn frame_count(&self) -> usize {
        self.textures.len()
    }

    fn bytes(&self) -> u64 {
        (self.width as u64) * (self.height as u64) * 4 * self.frame_count() as u64
    }
}

/// 逐帧纹理的缓存。**生命周期跟着工程走**：`open()` 换一份工程就 [`clear`](Self::clear)。
pub struct AnimationTextures {
    device: wgpu::Device,
    queue: wgpu::Queue,
    entries: BTreeMap<String, Entry>,
    budget_bytes: u64,
}

impl AnimationTextures {
    /// `budget_bytes` 是**这一份缓存**的上限（显存预算）。
    ///
    /// 传 0 表示用默认值（[crate::animation::MAX_DECODED_BYTES]）。
    /// 闸门可传参是为了单测能构造超限 —— 真拿 256MB 去撞，测试机上可能真就分配了。
    pub fn new(device: wgpu::Device, queue: wgpu::Queue, budget_bytes: u64) -> Self {
        Self {
            device,
            queue,
            entries: BTreeMap::new(),
            budget_bytes: if budget_bytes == 0 {
                crate::animation::MAX_DECODED_BYTES
            } else {
                budget_bytes
            },
        }
    }

    /// 把一张解好的动图传上 GPU。**同一个 asset_id 再传就是替换**（幂等）。
    ///
    /// 替换而不是"忽略第二次"：宿主在编辑里换了素材文件时，重新 load 必须生效 ——
    /// 留着旧帧的表现是"换了贴纸但画面没变"。
    pub fn upload(&mut self, asset_id: &str, animation: &Animation) -> Result<(), AnimError> {
        let frame_bytes =
            (animation.width as u64) * (animation.height as u64) * 4;
        let incoming = frame_bytes * animation.frame_count() as u64;
        // 先在**不碰 GPU** 的前提下查账：超了就直接拒绝，一张纹理都不建。
        let replaced = self.entries.get(asset_id).map(Entry::bytes).unwrap_or(0);
        let after = self.memory_bytes() - replaced + incoming;
        if after > self.budget_bytes {
            return Err(AnimError::OverBudget { bytes: after, limit: self.budget_bytes });
        }
        if animation.frames.is_empty() || animation.width == 0 || animation.height == 0 {
            return Err(AnimError::BadData("动图没有可上传的帧".to_string()));
        }

        let stride = (animation.width as usize) * 4;
        let mut textures = Vec::with_capacity(animation.frame_count());
        for frame in &animation.frames {
            // 每帧的长度必须与画布对得上 —— 对不上就说明解出来的东西不是"整张画布"，
            // 而那种纹理上传上去是**错位的像素**（比失败更难查）。
            if frame.rgba.len() != (stride * animation.height as usize) {
                return Err(AnimError::BadData(format!(
                    "动图第 {} 帧的像素数与画布对不上：{} vs {}",
                    textures.len() + 1,
                    frame.rgba.len(),
                    stride * animation.height as usize
                )));
            }
            let texture = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("dhampir animation frame"),
                size: wgpu::Extent3d {
                    width: animation.width,
                    height: animation.height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: FRAME_FORMAT,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            self.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &frame.rgba,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(stride as u32),
                    rows_per_image: Some(animation.height),
                },
                wgpu::Extent3d {
                    width: animation.width,
                    height: animation.height,
                    depth_or_array_layers: 1,
                },
            );
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            textures.push((texture, view));
        }

        self.entries.insert(
            asset_id.to_string(),
            Entry {
                textures,
                width: animation.width,
                height: animation.height,
                format: animation.format,
                loop_count: animation.loop_count,
                delays_ms: animation.delays_ms(),
                total_ms: animation.total_ms,
            },
        );
        Ok(())
    }

    /// 按帧号取纹理。没有这个资产（或帧号取不到）就 `None` —— 调用方接着往下找别的源。
    pub fn texture_for(
        &self,
        asset_id: &str,
        source_frame: i64,
    ) -> Option<(wgpu::TextureView, (u32, u32))> {
        let entry = self.entries.get(asset_id)?;
        let index = resolve_frame_index(entry.frame_count(), source_frame);
        let (_, view) = entry.textures.get(index)?;
        Some((view.clone(), (entry.width, entry.height)))
    }

    /// 这个资产是不是引擎自己在供帧（宿主据此**不再**为它 `set_bitmap`）。
    pub fn contains(&self, asset_id: &str) -> bool {
        self.entries.contains_key(asset_id)
    }

    /// 逐帧延迟表（契约里 `frame_delays_ms` 的真值）。宿主用它写回资产表。
    pub fn delays_ms(&self, asset_id: &str) -> Option<&[u32]> {
        self.entries.get(asset_id).map(|entry| entry.delays_ms.as_slice())
    }

    pub fn frame_count(&self, asset_id: &str) -> Option<usize> {
        self.entries.get(asset_id).map(Entry::frame_count)
    }

    pub fn format(&self, asset_id: &str) -> Option<AnimFormat> {
        self.entries.get(asset_id).map(|entry| entry.format)
    }

    pub fn loop_count(&self, asset_id: &str) -> Option<u32> {
        self.entries.get(asset_id).map(|entry| entry.loop_count)
    }

    pub fn total_ms(&self, asset_id: &str) -> Option<u64> {
        self.entries.get(asset_id).map(|entry| entry.total_ms)
    }

    /// 卸掉一张动图（宿主发现资产被换掉/删掉时用）。
    pub fn remove(&mut self, asset_id: &str) -> bool {
        self.entries.remove(asset_id).is_some()
    }

    /// 清空。**换一份工程就该调它** —— 与"换一份工程就是换一条历史"同一时机。
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// 当前占多少显存（纹理字节，估算值，不含驱动的对齐开销）。
    pub fn memory_bytes(&self) -> u64 {
        self.entries.values().map(Entry::bytes).sum()
    }

    pub fn budget_bytes(&self) -> u64 {
        self.budget_bytes
    }

    pub fn asset_count(&self) -> usize {
        self.entries.len()
    }

    /// 已登记的资产 id（按字典序，便于诊断输出稳定）。
    pub fn assets(&self) -> Vec<String> {
        self.entries.keys().cloned().collect()
    }
}

/// 纹理格式。
///
/// **`Rgba8Unorm`（线性）**，与两个宿主的源纹理一致（worker 的 `WORK_FORMAT`
/// 与 wasm 的预览格式都是它）：动图帧是**直通 alpha 的 RGBA8**，
/// 这里不做任何色彩空间转换 —— 转换发生在渲染管线该发生的地方。
pub const FRAME_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// 帧号 → 下标的**唯一**口径（越界钳制，见模块文档）。
///
/// 单独提出来是为了它能脱离 GPU 单测：这条规则一旦两处各写一遍，
/// 症状是"某些贴纸在最后一段停错了帧"，而那种错只出现在素材末尾。
pub fn resolve_frame_index(frame_count: usize, source_frame: i64) -> usize {
    if frame_count == 0 {
        return 0;
    }
    let last = (frame_count - 1) as i64;
    source_frame.clamp(0, last) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 帧号越界停在最后一帧_负号落第零帧() {
        assert_eq!(resolve_frame_index(66, 0), 0);
        assert_eq!(resolve_frame_index(66, 65), 65);
        // 不循环的层跑到素材末尾之后：停在最后一帧（不是回绕 —— 回绕会让贴纸一直动，
        // 而工程说的是"不循环"）。
        assert_eq!(resolve_frame_index(66, 66), 65);
        assert_eq!(resolve_frame_index(66, 9999), 65);
        assert_eq!(resolve_frame_index(66, -1), 0);
        assert_eq!(resolve_frame_index(1, 7), 0);
        // 空动图不该 panic（上传路径会先把它挡掉，这里是兜底）。
        assert_eq!(resolve_frame_index(0, 5), 0);
    }

    #[test]
    fn 纹理格式与两端源纹理一致() {
        // 写死这一条是因为它跨了三个 crate：改这里就得同时改 worker 的 WORK_FORMAT
        // 与 wasm 的预览格式，否则动图与视频在同一帧里会有色彩空间差。
        assert_eq!(FRAME_FORMAT, wgpu::TextureFormat::Rgba8Unorm);
    }
}
