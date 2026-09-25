//! 把分离器交给页面用（wasm-only）。
//!
//! # 分工
//!
//! **Rust 负责契约**：MP4 怎么变成样本表、第 N 帧对应哪个样本、要解第 N 帧得从哪个同步样本起。
//! **JS 负责浏览器 API**：VideoDecoder / EncodedVideoChunk 这些是 Web 平台的调用。
//!
//! 为什么不把 VideoDecoder 也搬进 Rust：web-sys 的 VideoDecoder 在 `web_sys_unstable_apis` 之后，
//! 要用它就得给整个构建加一个来路特殊的 cfg。现在这样分工，帧号这条契约仍然落在我们自己的类型上，
//! 浏览器那一层只是执行者。真要收进 dhampir_media::VideoDecoder 时，改的是这一层，不是分离器。
//!
//! # 为什么这里只剩 5 个导出（原 6 个）
//!
//! 收口时全仓 grep 过：`dhampir_demux_gop_slices` **一个调用方都没有**。
//! 唯一的消费者 `scripts/s3-preview-spike/index.html` 用的是 `open` / `description` /
//! `samples` / `sync_start` / `frame_index` 五个。
//!
//! 删它不丢能力：它是一行直通（`host_api::to_json(&host_api::gop_slices(..))`），
//! 而 `gop_slices` 在 `dhampir-timeline/src/host_api.rs` 有 6 条单测，
//! 在 CLI 侧另有真调用（`dhampir.rs` 的 `cmd_gop`，即 `dhampir gop` 子命令）——
//! **远端模式按段取这条路的契约在 CLI 那条腿上活着**，不靠这个没人调的导出撑。
//! 留着的代价和 cache_wasm 那边一样：它看起来像一份兼容承诺，实际没有任何对端。

use std::cell::RefCell;

use wasm_bindgen::prelude::*;

use crate::demux::{self, VideoTrack};
use crate::web::js_err;
use dhampir_core::timeline::host_api;

thread_local! {
    /// 最近一次解析出来的轨。页面只需要同时持有**一条**——预览是单片段路径，
    /// 多片段要等 M4 的时间线。真要并行时这里会换成一张表。
    static TRACK: RefCell<Option<VideoTrack>> = const { RefCell::new(None) };
}

/// 解析一份 MP4 并把它记住，返回给页面看的元信息 JSON。
///
/// 样本表**单独**用 dhampir_demux_samples 取：把"元信息"与"逐样本表"分开，
/// 页面就能先把解码器配起来、再按需喂样本。
#[wasm_bindgen]
pub fn dhampir_demux_open(bytes: &[u8]) -> Result<String, JsValue> {
    let track = demux::parse_video_track(bytes).map_err(|e| js_err(e.to_string()))?;
    let sync_indices: Vec<usize> = track
        .samples
        .iter()
        .enumerate()
        .filter(|(_, s)| s.is_sync)
        .map(|(i, _)| i)
        .collect();
    let json = format!(
        "{{\"width\":{},\"height\":{},\"timescale\":{},\"samples\":{},\"sync_indices\":{},\"constant_rate\":{},\"description_bytes\":{}}}",
        track.width,
        track.height,
        track.timescale,
        track.samples.len(),
        json_usize_array(&sync_indices),
        track.is_constant_rate(),
        track.description.len(),
    );
    TRACK.with(|t| *t.borrow_mut() = Some(track));
    Ok(json)
}

fn json_usize_array(values: &[usize]) -> String {
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

/// avcC 的原始字节：WebCodecs 的 VideoDecoderConfig.description。
#[wasm_bindgen]
pub fn dhampir_demux_description() -> Vec<u8> {
    TRACK.with(|t| {
        t.borrow()
            .as_ref()
            .map(|track| track.description.clone())
            .unwrap_or_default()
    })
}

/// 把分离器里的样本表转成跨边界形状。
///
/// 抽成一个函数是为了与 CLI 那条腿共用同一份形状（`dhampir.rs` 的
/// `cmd_gop` 从 `host_api::gop_slices` 取段），各写一份迟早会漂。
fn sample_views(track: &crate::demux::VideoTrack) -> Vec<host_api::SampleView> {
    track
        .samples
        .iter()
        .map(|s| host_api::SampleView {
            offset: s.offset,
            size: s.size,
            dts: s.dts,
            duration: s.duration,
            is_sync: s.is_sync,
        })
        .collect()
}

/// 逐样本表，按解码顺序。每项 o/s/d/u/k：文件偏移、字节数、解码时间戳、时长、是否同步样本。
#[wasm_bindgen]
pub fn dhampir_demux_samples() -> String {
    TRACK.with(|t| {
        let borrowed = t.borrow();
        let Some(track) = borrowed.as_ref() else {
            return String::from("[]");
        };
        host_api::to_json(&sample_views(track))
    })
}

/// 要解第 frame 帧，得从哪个样本下标开始喂（含）。
///
/// 这是"帧号精确"的兑现处：WebCodecs 只允许从同步样本起解，
/// 所以第 N 帧的代价是"从它前面最近的那个关键帧一路解到 N"。
/// 返回 -1 表示帧号越界或没有可用同步样本。
#[wasm_bindgen]
pub fn dhampir_demux_sync_start(frame: usize) -> i32 {
    TRACK.with(|t| {
        t.borrow()
            .as_ref()
            .and_then(|track| track.sync_sample_at_or_before(frame))
            .map(|i| i as i32)
            .unwrap_or(-1)
    })
}

/// 第 frame 帧在样本表里的下标。越界返回 -1。
#[wasm_bindgen]
pub fn dhampir_demux_frame_index(frame: usize) -> i32 {
    TRACK.with(|t| {
        t.borrow()
            .as_ref()
            .and_then(|track| track.sample_index_of_frame(frame))
            .map(|i| i as i32)
            .unwrap_or(-1)
    })
}
