//! 预览侧的帧缓存**策略**：LRU + 显存 / 内存上限。
//!
//! 这里只有策略，不碰 GPU、不碰解码器——所以它在没有显卡的机器上也能被测。
//! 真正持有纹理与 VideoFrame 的宿主只需要问它两件事：
//! 「我要放这一帧，该淘汰谁」和「这一帧还在不在」。
//!
//! # 为什么必须有上限
//!
//! 见 plan/s3.3-videoframe-lifetime.md 第 3 节：1080p 一帧 YUV420 是 3.11 MB、
//! RGBA8 是 8.29 MB；300 帧就是 933 MB / 2.49 GB。「不及时释放」不是卫生问题，
//! 是几百 MB 到 GB 级的泄漏，长视频 + 多轨会更快。
//!
//! # 参考口径与它的问题
//!
//! plan 里引用的参考实现用了两个数：显存 300 张纹理、内存 900 帧。
//! 但**按张数配预算是危险的**——300 张 1080p RGBA8 纹理是 2.49 GB，
//! 很多适配器根本给不了。所以这里只按**字节**记账，由宿主机按自己的
//! 显存预算换算成张数（例如 512 MB 预算 = 61 张 1080p RGBA8），
//! 别把 300 这个数直接抄进代码。

use std::collections::HashMap;

/// 帧的键：全局帧号。整数帧号是项目的铁律，缓存也照这个来。
pub type FrameKey = i64;

/// 1080p RGBA8 纹理一帧的字节数（拷进纹理之后的形态）。
pub const FRAME_RGBA8_1080P_BYTES: usize = 1920 * 1080 * 4;

/// 1080p YUV420 一帧的字节数（解码器输出的常见形态）。
pub const FRAME_YUV420_1080P_BYTES: usize = 1920 * 1080 * 3 / 2;

/// 一层缓存的账。显存与内存各一份，互不顶账。
#[derive(Debug)]
struct Tier {
    label: &'static str,
    budget: usize,
    used: usize,
    /// LRU 顺序：**最近使用的在末尾**。
    ///
    /// 用 Vec + HashMap 而不是引一张有序表：容量是几百，线性扫描不值一提，
    /// 而少一个依赖就少一个「两端版本对不上」的地方。
    order: Vec<FrameKey>,
    bytes: HashMap<FrameKey, usize>,
}

impl Tier {
    fn new(label: &'static str, budget: usize) -> Self {
        Self {
            label,
            budget,
            used: 0,
            order: Vec::new(),
            bytes: HashMap::new(),
        }
    }

    /// 把这一帧挪到「最近使用」的一端。不存在的键不动。
    fn touch(&mut self, key: FrameKey) {
        if let Some(index) = self.order.iter().position(|k| *k == key) {
            self.order.remove(index);
            self.order.push(key);
        }
    }

    /// 记一帧的账，返回被淘汰的帧（最久未用的在前）。
    ///
    /// 一条不变量：**刚放进来这一帧一定留下**。所以单帧超过预算时不会空转——
    /// 它会把别的都挤掉，然后如实报出 used > budget，由调用方决定要不要降质。
    fn insert(&mut self, key: FrameKey, bytes: usize) -> Vec<FrameKey> {
        if let Some(old) = self.bytes.remove(&key) {
            self.used -= old;
        }
        if let Some(index) = self.order.iter().position(|k| *k == key) {
            self.order.remove(index);
        }
        self.bytes.insert(key, bytes);
        self.used += bytes;
        self.order.push(key);

        let mut evicted = Vec::new();
        while self.used > self.budget && self.order.len() > 1 {
            let victim = self.order.remove(0);
            if let Some(freed) = self.bytes.remove(&victim) {
                self.used -= freed;
            }
            evicted.push(victim);
        }
        // 只剩这一帧却仍然超预算：预算为 0，或者这一帧本身比预算还大。
        // 后者留一帧是刻意的（画得出来总比什么都不画好），但账不能瞒——
        // used 会如实大于 budget，调用方查 used_over_budget() 就知道。
        if self.budget == 0 {
            let victim = self.order.remove(0);
            self.bytes.remove(&victim);
            self.used = 0;
            evicted.push(victim);
        }
        evicted
    }

    /// 主动移除一帧，返回它原先占的字节数。
    fn remove(&mut self, key: FrameKey) -> Option<usize> {
        let freed = self.bytes.remove(&key)?;
        self.used -= freed;
        if let Some(index) = self.order.iter().position(|k| *k == key) {
            self.order.remove(index);
        }
        Some(freed)
    }

    fn contains(&self, key: FrameKey) -> bool {
        self.bytes.contains_key(&key)
    }

    fn len(&self) -> usize {
        self.bytes.len()
    }

    fn used_over_budget(&self) -> bool {
        self.used > self.budget
    }
}

/// 两级帧缓存：显存（纹理）与内存（解码帧）各一份 LRU。
#[derive(Debug)]
pub struct FrameCache {
    vram: Tier,
    ram: Tier,
}

impl FrameCache {
    /// 两份预算都按**字节**给。宿主自己换算成张数。
    pub fn new(vram_bytes: usize, ram_bytes: usize) -> Self {
        Self {
            vram: Tier::new("vram", vram_bytes),
            ram: Tier::new("ram", ram_bytes),
        }
    }

    /// 记一帧纹理的账。返回被淘汰的帧号（最久未用的在前）。
    pub fn insert_vram(&mut self, frame: FrameKey, bytes: usize) -> Vec<FrameKey> {
        self.vram.insert(frame, bytes)
    }

    /// 记一帧解码帧的账。返回被淘汰的帧号。
    pub fn insert_ram(&mut self, frame: FrameKey, bytes: usize) -> Vec<FrameKey> {
        self.ram.insert(frame, bytes)
    }

    /// 这一帧刚被用到（渲染、拖动预览都算），把它的位置挪到最近使用端。
    pub fn touch_vram(&mut self, frame: FrameKey) {
        self.vram.touch(frame);
    }

    pub fn touch_ram(&mut self, frame: FrameKey) {
        self.ram.touch(frame);
    }

    pub fn remove_vram(&mut self, frame: FrameKey) -> Option<usize> {
        self.vram.remove(frame)
    }

    pub fn remove_ram(&mut self, frame: FrameKey) -> Option<usize> {
        self.ram.remove(frame)
    }

    pub fn has_vram(&self, frame: FrameKey) -> bool {
        self.vram.contains(frame)
    }

    pub fn has_ram(&self, frame: FrameKey) -> bool {
        self.ram.contains(frame)
    }

    pub fn vram_bytes(&self) -> usize {
        self.vram.used
    }

    pub fn ram_bytes(&self) -> usize {
        self.ram.used
    }

    pub fn vram_len(&self) -> usize {
        self.vram.len()
    }

    /// 显存预算是多少字节（宿主用它换算「该留几张纹理」）。
    pub fn vram_budget(&self) -> usize {
        self.vram.budget
    }

    /// 内存预算是多少字节。
    pub fn ram_budget(&self) -> usize {
        self.ram.budget
    }

    pub fn ram_len(&self) -> usize {
        self.ram.len()
    }

    /// 显存记账是否已经超过预算（单帧比预算还大时会为真）。
    pub fn vram_over_budget(&self) -> bool {
        self.vram.used_over_budget()
    }

    pub fn ram_over_budget(&self) -> bool {
        self.ram.used_over_budget()
    }

    /// 释放全部记账。宿主在销毁纹理 / 帧之后调它。
    pub fn clear(&mut self) {
        self.vram = Tier::new(self.vram.label, self.vram.budget);
        self.ram = Tier::new(self.ram.label, self.ram.budget);
    }
}

/// 按字节预算算「能放几张 w×h 的 RGBA8 纹理」。
///
/// 给宿主用：别把参考实现那个「300 张」直接抄进代码，按自己的预算换算。
pub fn rgba8_texture_capacity(budget_bytes: usize, width: u32, height: u32) -> usize {
    let per = (width as usize) * (height as usize) * 4;
    if per == 0 {
        return 0;
    }
    budget_bytes / per
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 不超预算时不淘汰() {
        let mut cache = FrameCache::new(300, 300);
        assert!(cache.insert_vram(0, 100).is_empty());
        assert!(cache.insert_vram(1, 100).is_empty());
        assert_eq!(cache.vram_len(), 2);
        assert_eq!(cache.vram_bytes(), 200);
    }

    #[test]
    fn 超预算时淘汰最久未用的() {
        let mut cache = FrameCache::new(250, 250);
        cache.insert_vram(0, 100);
        cache.insert_vram(1, 100);
        // 第 3 帧进来会把第 0 帧挤掉（它最久没用）
        let evicted = cache.insert_vram(2, 100);
        assert_eq!(evicted, vec![0]);
        assert!(!cache.has_vram(0));
        assert!(cache.has_vram(1) && cache.has_vram(2));
        assert_eq!(cache.vram_bytes(), 200);
    }

    #[test]
    fn touch_改变淘汰顺序() {
        let mut cache = FrameCache::new(250, 250);
        cache.insert_vram(0, 100);
        cache.insert_vram(1, 100);
        cache.touch_vram(0); // 现在 1 才是最久没用的
        let evicted = cache.insert_vram(2, 100);
        assert_eq!(evicted, vec![1]);
        assert!(cache.has_vram(0) && cache.has_vram(2));
    }

    #[test]
    fn 重复放同一帧只记一次账() {
        let mut cache = FrameCache::new(300, 300);
        cache.insert_vram(7, 100);
        cache.insert_vram(7, 100);
        assert_eq!(cache.vram_len(), 1);
        assert_eq!(cache.vram_bytes(), 100);
    }

    #[test]
    fn 更新一帧的字节数要重算账() {
        let mut cache = FrameCache::new(1000, 1000);
        cache.insert_vram(7, 100);
        cache.insert_vram(7, 250);
        assert_eq!(cache.vram_bytes(), 250);
        assert_eq!(cache.vram_len(), 1);
    }

    #[test]
    fn 单帧超过预算不会空转() {
        // 预算 150，但这一帧要 400：留下它、如实报超支，而不是反复淘汰自己。
        let mut cache = FrameCache::new(150, 150);
        let evicted = cache.insert_vram(0, 400);
        assert!(evicted.is_empty());
        assert!(cache.has_vram(0));
        assert_eq!(cache.vram_bytes(), 400);
        assert!(cache.vram_over_budget());
    }

    #[test]
    fn 单帧超预算时别的帧会被挤掉() {
        let mut cache = FrameCache::new(150, 150);
        cache.insert_vram(0, 100);
        let evicted = cache.insert_vram(1, 400);
        assert_eq!(evicted, vec![0]);
        assert!(cache.has_vram(1));
        assert_eq!(cache.vram_len(), 1);
    }

    #[test]
    fn 预算为零时不保留() {
        let mut cache = FrameCache::new(0, 0);
        let evicted = cache.insert_vram(3, 10);
        assert_eq!(evicted, vec![3]);
        assert!(!cache.has_vram(3));
        assert_eq!(cache.vram_bytes(), 0);
        assert_eq!(cache.vram_len(), 0);
    }

    #[test]
    fn 显存与内存各记各的账() {
        let mut cache = FrameCache::new(1000, 10);
        cache.insert_vram(0, 500);
        cache.insert_ram(0, 100);
        // 内存那一层预算只有 10，单帧 100 就超了，但显存那层不该受影响。
        assert_eq!(cache.vram_bytes(), 500);
        assert!(cache.has_vram(0));
        assert_eq!(cache.ram_bytes(), 100);
        assert!(cache.ram_over_budget());
    }

    #[test]
    fn 移除后账目归零() {
        let mut cache = FrameCache::new(1000, 1000);
        cache.insert_vram(0, 300);
        assert_eq!(cache.remove_vram(0), Some(300));
        assert_eq!(cache.vram_bytes(), 0);
        assert_eq!(cache.remove_vram(0), None);
    }

    #[test]
    fn clear_把两层都清空() {
        let mut cache = FrameCache::new(1000, 1000);
        cache.insert_vram(0, 300);
        cache.insert_ram(1, 200);
        cache.clear();
        assert_eq!(cache.vram_bytes(), 0);
        assert_eq!(cache.ram_bytes(), 0);
        assert_eq!(cache.vram_len(), 0);
        assert_eq!(cache.ram_len(), 0);
    }

    #[test]
    fn 张数换算按字节而不是按参考实现那个数() {
        // 512 MB 预算下，1080p RGBA8 能放多少张
        let capacity = rgba8_texture_capacity(512 * 1024 * 1024, 1920, 1080);
        assert_eq!(capacity, 512 * 1024 * 1024 / FRAME_RGBA8_1080P_BYTES);
        assert_eq!(capacity, 64);
        // 参考实现那个「300 张」= 2.49 GB，别直接抄
        assert_eq!(300 * FRAME_RGBA8_1080P_BYTES, 2_488_320_000);
        assert_eq!(900 * FRAME_YUV420_1080P_BYTES, 2_799_360_000);
    }

    #[test]
    fn 宽高为零时容量为零() {
        assert_eq!(rgba8_texture_capacity(1024, 0, 1080), 0);
    }
}
