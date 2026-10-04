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

use crate::animation::{AnimError, AnimFormat, Animation};
use crate::wgpu;

/// 一张动图上传后的账目与句柄。
struct Entry {
    /// **一张 `D2Array` 纹理装下所有帧**（第 N 层 = 第 N 帧），不是每帧一张纹理。
    ///
    /// 为什么合成一张：`queue.write_texture` 在 **wasm/WebGPU** 宿主上要跨
    /// wasm↔JS 边界拷贝 —— 每帧一次的话，77 帧的贴纸就是 77 趟往返。
    /// 实测（500x500x77，73.4 MiB，同一台机器/同一批素材/同一个无头 Chrome，
    /// 判据脚本 `scripts/bench-animation-upload.mjs`）：
    ///
    /// | | 合计（5 张真贴纸） |
    /// |---|---|
    /// | 逐帧上传（改前） | 747 ms |
    /// | 一张数组纹理（改后） | 542 ms |
    ///
    /// ⚠️ **这里只快约 1.4x，不是最初以为的 10.8x。** 最初那次对照是拿一个**过期的
    /// 8 MB 未优化产物**当旧版比出来的，那个数字**不成立**，已在交接文档 §五之二 更正。
    /// 收益的方向是对的（少 76 次跨边界拷贝），但**量级远小于当时的结论**。
    /// 一次 `create_texture` + 一次 `write_texture` 把跨边界次数从"帧数"降到 1。
    ///
    /// 保住纹理对象：`TextureView` 不持有它的生命周期，视图全丢了纹理才能释放。
    ///
    /// 它**没有读点**（视图已经够用），这是刻意的 —— 见下面 `#[allow]` 的说明。
    #[allow(dead_code)]
    texture: wgpu::Texture,
    /// 逐层的视图（与帧号一一对应）。层号 = 帧号。
    views: Vec<wgpu::TextureView>,
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
        self.views.len()
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
        let frame_bytes = (animation.width as u64) * (animation.height as u64) * 4;
        let incoming = frame_bytes * animation.frame_count() as u64;
        // 先在**不碰 GPU** 的前提下查账：超了就直接拒绝，一张纹理都不建。
        let replaced = self.entries.get(asset_id).map(Entry::bytes).unwrap_or(0);
        let after = self.memory_bytes() - replaced + incoming;
        if after > self.budget_bytes {
            return Err(AnimError::OverBudget {
                bytes: after,
                limit: self.budget_bytes,
            });
        }
        if animation.frames.is_empty() || animation.width == 0 || animation.height == 0 {
            return Err(AnimError::BadData("动图没有可上传的帧".to_string()));
        }

        let stride = (animation.width as usize) * 4;
        let frame_len = stride * animation.height as usize;

        // 先把所有帧的长度校验完 —— 有一帧对不上就**一张纹理都不建**。
        // 对不上说明解出来的不是"整张画布"，传上去是错位的像素（比失败更难查）。
        for (index, frame) in animation.frames.iter().enumerate() {
            if frame.rgba.len() != frame_len {
                return Err(AnimError::BadData(format!(
                    "动图第 {} 帧的像素数与画布对不上：{} vs {}",
                    index + 1,
                    frame.rgba.len(),
                    frame_len
                )));
            }
        }

        let layers = animation.frame_count() as u32;

        // ⚠️ **层数上限**：WebGPU 的 `maxTextureArrayLayers` 默认下限是 **256**，
        // 而帧数上限 `MAX_FRAMES` 是 4096 ⇒ 大动图会撞上，`create_texture` 直接报验证错。
        // 撞了就**明确拒绝**（与预算闸同一种处置：宁可报"这张画不出来"，
        // 也不要建出一张静默截断的纹理 —— 后者表现为"动图后半段不动了"）。
        // 判据取自设备限制而不是写死 256：不同实现可以给得更高。
        let max_layers = self.device.limits().max_texture_array_layers;
        if layers > max_layers {
            return Err(AnimError::BadData(format!(
                "动图有 {layers} 帧，超过这个设备能用的纹理数组层数上限 {max_layers} \
                 —— 请改用更少帧的素材"
            )));
        }

        // **一次创建、一次写入**（见 `Entry` 上那段说明）：
        // 所有帧拼进一张 `D2Array` 纹理，第 N 层就是第 N 帧。
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("dhampir animation frames"),
            size: wgpu::Extent3d {
                width: animation.width,
                height: animation.height,
                depth_or_array_layers: layers,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: FRAME_FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });

        // 帧数据拼成一段连续缓冲：**一次** `write_texture` 写完所有层。
        //
        // ⚠️ 这一步在 wasm 宿主上也是跨边界拷贝，但**只拷一趟**（原来是帧数趟）。
        // 缓冲的层间排布就是 `D2Array` 要求的"每层按序紧挨着"。
        let mut packed = Vec::with_capacity(frame_len * animation.frame_count());
        for frame in &animation.frames {
            // 每一层的行距已经是紧凑的（stride == width*4），所以直接首尾相接即可。
            packed.extend_from_slice(&frame.rgba);
        }
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &packed,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                // ⚠️ **必须是包含所有层的总排布**，不是单帧的：
                // `bytes_per_row` 是一行跨多少字节，`rows_per_image` 是"每层多少行"，
                // 两者合起来决定一层的步长；层与层由 extent 的 depth 推。
                bytes_per_row: Some(stride as u32),
                rows_per_image: Some(animation.height),
            },
            wgpu::Extent3d {
                width: animation.width,
                height: animation.height,
                depth_or_array_layers: layers,
            },
        );

        // 逐层建视图（**不新建纹理**）：层号 = 帧号，与 D2Array 的约定一致。
        let mut views = Vec::with_capacity(animation.frame_count());
        for layer in 0..layers {
            views.push(texture.create_view(&wgpu::TextureViewDescriptor {
                label: Some("dhampir animation frame"),
                format: Some(FRAME_FORMAT),
                dimension: Some(wgpu::TextureViewDimension::D2),
                base_array_layer: layer,
                array_layer_count: Some(1),
                ..Default::default()
            }));
        }

        self.entries.insert(
            asset_id.to_string(),
            Entry {
                texture,
                views,
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
        let view = entry.views.get(index)?;
        Some((view.clone(), (entry.width, entry.height)))
    }

    /// 这个资产是不是引擎自己在供帧（宿主据此**不再**为它 `set_bitmap`）。
    pub fn contains(&self, asset_id: &str) -> bool {
        self.entries.contains_key(asset_id)
    }

    /// 逐帧延迟表（契约里 `frame_delays_ms` 的真值）。宿主用它写回资产表。
    pub fn delays_ms(&self, asset_id: &str) -> Option<&[u32]> {
        self.entries
            .get(asset_id)
            .map(|entry| entry.delays_ms.as_slice())
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
