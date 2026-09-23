//! 把帧缓存策略交给页面用（wasm-only）。
//!
//! # 为什么是"账在 Rust、资源在 JS"
//!
//! 策略（LRU 与预算）在 `dhampir_core::cache`，它不碰 GPU，因此能被单测覆盖。
//! 而**被缓存的东西**在浏览器里是 JS 对象（VideoFrame）与 wgpu 纹理——前者只有 JS 拿得到，
//! 后者归这个宿主管。所以这里只开一道账口子：页面告诉它"我要放这一帧、多少字节"，
//! 它回答"该淘汰谁"，页面照单去 close。淘汰决策只有一份实现，不会两边各写一套。

use std::cell::RefCell;

use dhampir_core::cache::FrameCache;
use dhampir_core::timeline::host_api;
use wasm_bindgen::prelude::*;

thread_local! {
    // 这里不用 const 块：FrameCache::new 不是 const fn（HashMap 的关系），
    // 而 thread_local 本来就支持惰性初始化。
    static CACHE: RefCell<FrameCache> = RefCell::new(FrameCache::new(0, 0));
}

fn json_i64_array(values: &[i64]) -> String {
    let mut out = String::from("[");
    for (i, v) in values.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&v.to_string());
    }
    out.push(']');
    out
}

fn stats_json() -> String {
    CACHE.with(|c| {
        let cache = c.borrow();
        host_api::to_json(&host_api::CacheStatsView {
            vram_bytes: cache.vram_bytes(),
            ram_bytes: cache.ram_bytes(),
            vram_len: cache.vram_len(),
            ram_len: cache.ram_len(),
            vram_over: cache.vram_over_budget(),
            ram_over: cache.ram_over_budget(),
        })
    })
}
/// 开一份账。两份预算都按**字节**给（张数由页面自己换算，见 cache.rs 的说明）。
#[wasm_bindgen]
pub fn dhampir_cache_open(vram_bytes: usize, ram_bytes: usize) -> String {
    CACHE.with(|c| *c.borrow_mut() = FrameCache::new(vram_bytes, ram_bytes));
    stats_json()
}

/// 记一帧**解码帧**（VideoFrame）的账，返回该被淘汰的帧号（最久未用的在前）。
#[wasm_bindgen]
// 参数用 i32 而不是 i64：wasm-bindgen 把 i64 映射成 **BigInt**，页面传 number 会当场抛
// 「expected a bigint argument」。帧号用 i32 足够（任何现实片长都放得下）。
pub fn dhampir_cache_note_ram(frame: i32, bytes: usize) -> String {
    let evicted = CACHE.with(|c| c.borrow_mut().insert_ram(i64::from(frame), bytes));
    json_i64_array(&evicted)
}

/// 记一帧**纹理**的账，返回该被淘汰的帧号。
#[wasm_bindgen]
pub fn dhampir_cache_note_vram(frame: i32, bytes: usize) -> String {
    let evicted = CACHE.with(|c| c.borrow_mut().insert_vram(i64::from(frame), bytes));
    json_i64_array(&evicted)
}

/// 这一帧刚被用到（渲染、拖动预览都算），把它的位置挪到最近使用端。
#[wasm_bindgen]
pub fn dhampir_cache_touch_ram(frame: i32) {
    CACHE.with(|c| c.borrow_mut().touch_ram(i64::from(frame)));
}

#[wasm_bindgen]
pub fn dhampir_cache_touch_vram(frame: i32) {
    CACHE.with(|c| c.borrow_mut().touch_vram(i64::from(frame)));
}

#[wasm_bindgen]
pub fn dhampir_cache_remove_ram(frame: i32) {
    CACHE.with(|c| {
        c.borrow_mut().remove_ram(i64::from(frame));
    });
}

#[wasm_bindgen]
pub fn dhampir_cache_remove_vram(frame: i32) {
    CACHE.with(|c| {
        c.borrow_mut().remove_vram(i64::from(frame));
    });
}

/// 当前账目。宿主在 HUD 上显示它，测试也读它。
#[wasm_bindgen]
pub fn dhampir_cache_stats() -> String {
    stats_json()
}

/// 显存预算是多少（页面用来换算"该留几张纹理"）。
#[wasm_bindgen]
pub fn dhampir_cache_vram_budget() -> usize {
    CACHE.with(|c| c.borrow().vram_budget())
}

/// 内存预算是多少。
#[wasm_bindgen]
pub fn dhampir_cache_ram_budget() -> usize {
    CACHE.with(|c| c.borrow().ram_budget())
}

/// 页面用它做一次"按预算换算"自检：给宽高，返回该预算下能放几张 RGBA8 纹理。
#[wasm_bindgen]
pub fn dhampir_cache_texture_capacity(width: u32, height: u32) -> usize {
    let budget = CACHE.with(|c| c.borrow().vram_budget());
    dhampir_core::cache::rgba8_texture_capacity(budget, width, height)
}
