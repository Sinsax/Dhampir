//! # dhampir-media
//!
//! 解码 / 编码的**纯契约**。这一层只有 trait 与它们共享的词汇表，没有任何实现，
//! 也**不依赖 wgpu**——GPU 资源不在这里出现。
//!
//! ## 当前状态：**目标接口，零实现**
//!
//! 5 个 trait（`Demuxer` / `VideoDecoder` / `VideoEncoder` / `AudioEncoder` / `Muxer`）
//! **一个实现都没有**，全仓也没有任何地方引用它们。
//!
//! 这不是遗漏，是**刻意的分工**：解码与编码归宿主 ——
//! 浏览器走 WebCodecs（在 JS 侧），服务端走它自己的解码器。
//! 底座只规定「帧怎么从容器里出来」这个抽象面长什么样。
//!
//! **但必须说清一件事**：打算照这些 trait 实现一条链路的话，请注意
//! **至今没有任何一端跑通过它**。空契约比没有契约更容易误导 ——
//! 它看起来像一条已经铺好的路。真正的实现要由用到它的那一端来做；
//! 做完之后应当把这句话删掉，并让 `scripts/check-media-status.mjs` 一起改。
//!
//! ## 为什么和 core 的 `FrameSource` 分开
//!
//! 指导文档 §4.4 把 `FrameSource` / `FrameSink` 放在 `dhampir-core`，它们的签名
//! 里有 `wgpu::Device` 与 `wgpu::TextureView`。那两个 trait 描述的是"**帧怎么上
//! GPU**"。而本 crate 描述的是"**帧怎么从容器里出来**"——封装格式、像素格式、
//! 时间戳、seek。后者跟 GPU 无关，是 FFmpeg 与 WebCodecs 共同的抽象面。
//!
//! 分开的收益很实际：契约层可以脱离 GPU 单测，而 GPU 层不必知道 MP4 长什么样。
//!
//! ## 两端各自实现
//!
//! | | native (`dhampir-worker`) | wasm (`dhampir-wasm`) |
//! |---|---|---|
//! | 解复用 / 解码 | FFmpeg | WebCodecs |
//! | 编码 | FFmpeg | WebCodecs（暂不需要） |
//!
//! 两边实现的只是这些 trait，上游渲染代码看不到差别。
//!
//! ## M0 状态
//!
//! **这里只有签名与词汇表，没有任何实现。** 具体实现在 M4 落地；M0–M2 用合成帧，
//! 完全不需要解码。之所以现在就把契约定下来，是因为 `dhampir-timeline` 的 JSON 契约
//! 要引用像素格式与色彩信息，等到 M4 再定会让时间线 JSON 改版。

use core::fmt;

/// 像素格式。**只列出引擎真正会遇到的**，不做 FFmpeg 那样的全覆盖枚举。
///
/// 命名沿用 GPU 侧习惯（`*8Unorm` = 8 位归一化整数），因为跨过这个边界之后
/// 数据就是要当纹理用的。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum PixelFormat {
    /// 8 位 RGBA，每通道归一化。sRGB 传输函数（视频最常见）。
    Rgba8UnormSrgb,
    /// 8 位 RGBA，线性。合成中间结果。
    Rgba8Unorm,
    /// 8 位 BGRA，sRGB。Windows / DirectX 侧的默认顺序，FFmpeg 的 `bgra`。
    Bgra8UnormSrgb,
    /// 8 位 YUV 4:2:0 平面格式，limited range，BT.709。解码器吐出的原始形态。
    Yuv420p,
    /// 8 位 NV12（Y 平面 + 交错 UV），limited range。硬解与 WebCodecs 常见输出。
    Nv12,
}

impl PixelFormat {
    /// 每像素字节数。平面格式返回单像素平均字节数（4:2:0 为 1.5 → 这里不适用，
    /// 返回 `None`，逼调用方按平面去算，别在这里糊一个近似值）。
    pub const fn bytes_per_pixel(&self) -> Option<u32> {
        match self {
            Self::Rgba8UnormSrgb | Self::Rgba8Unorm | Self::Bgra8UnormSrgb => Some(4),
            // 4:2:0 与 NV12 不是打包格式，"每像素字节数"没有定义。
            Self::Yuv420p | Self::Nv12 => None,
        }
    }

    /// 是否是 RGB 打包格式（可以直接当纹理上传，不需要色彩转换）。
    pub const fn is_packed_rgb(&self) -> bool {
        matches!(
            self,
            Self::Rgba8UnormSrgb | Self::Rgba8Unorm | Self::Bgra8UnormSrgb
        )
    }
}

impl fmt::Display for PixelFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Rgba8UnormSrgb => "rgba8unorm-srgb",
            Self::Rgba8Unorm => "rgba8unorm",
            Self::Bgra8UnormSrgb => "bgra8unorm-srgb",
            Self::Yuv420p => "yuv420p",
            Self::Nv12 => "nv12",
        };
        f.write_str(s)
    }
}

/// 色彩空间。**这是"画面发灰"类 bug 的唯一源头**，所以必须进契约。
///
/// 指导文档 §4.3④：VideoFrame 带 `colorSpace` 元数据（BT.709 limited range 最常见），
/// 转换矩阵配错会让画面发灰或过饱和。这里把三个决定性的分量都显式写出来，
/// 不留"默认值"的余地。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ColorSpace {
    /// 传递函数（transfer characteristics）。
    pub transfer: TransferFunction,
    /// 原色（primaries）与矩阵系数（matrix coefficients）。
    pub primaries: Primaries,
    /// 量化范围。
    pub range: ColorRange,
}

impl ColorSpace {
    /// BT.709 limited range —— 1080p 及以下的绝对主流。
    pub const BT709_LIMITED: Self = Self {
        transfer: TransferFunction::Bt709,
        primaries: Primaries::Bt709,
        range: ColorRange::Limited,
    };

    /// BT.709 full range —— 屏幕录制、游戏捕获常见。
    pub const BT709_FULL: Self = Self {
        transfer: TransferFunction::Bt709,
        primaries: Primaries::Bt709,
        range: ColorRange::Full,
    };

    /// sRGB full range —— 图片素材与浏览器 canvas。
    pub const SRGB_FULL: Self = Self {
        transfer: TransferFunction::Srgb,
        primaries: Primaries::Bt709,
        range: ColorRange::Full,
    };
}

/// 传递函数。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum TransferFunction {
    /// BT.709 OETF。
    Bt709,
    /// sRGB EOTF/OETF。
    Srgb,
    /// 线性光。
    Linear,
    /// PQ（HDR10）。
    Pq,
    /// HLG。
    Hlg,
}

/// 色域 / 矩阵系数。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Primaries {
    /// BT.709 / sRGB 共用同一组原色。
    Bt709,
    /// BT.601，SD 素材。
    Bt601,
    /// BT.2020，HDR。
    Bt2020,
}

/// 量化范围。
///
/// 这不是微观差别：limited range 的 8 位黑点是 16、白点是 235。把 limited 当 full
/// 解释，画面会发灰且对比度塌掉——正是指导文档点名的那类 bug。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ColorRange {
    /// 视频范围（黑 16 / 白 235）。
    Limited,
    /// 全范围（黑 0 / 白 255）。
    Full,
}

/// 视频轨的几何与时基信息。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VideoInfo {
    pub width: u32,
    pub height: u32,
    /// 这是**素材自身**的帧率，与时间线 timebase 可能不同。
    /// 素材帧率 ≠ 时间线帧率时如何处理（重复帧 / 丢帧 / 光流）是 M4 的设计问题。
    pub timebase: dhampir_timeline::Timebase,
    pub pixel_format: PixelFormat,
    pub color_space: ColorSpace,
    /// 是否含 alpha（ProRes 4444、VP9 alpha 等）。
    pub has_alpha: bool,
}

/// 音频轨信息。
///
/// 音频**不做逐样本比对**（指导文档：SSIM 那套判据不适用于音频），但参数语义必须
/// 与视频侧一致——尤其是"用整数表示时间"：采样率是整数，采样点是整数。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AudioInfo {
    pub sample_rate: u32,
    pub channels: u16,
    pub sample_format: SampleFormat,
}

/// 音频采样格式。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SampleFormat {
    /// 32 位浮点，平面。FFmpeg 内部处理格式，也是混音的首选。
    F32Planar,
    /// 16 位整数，交错。
    S16Interleaved,
}

/// 媒体层的错误。
///
/// 变体刻意粗粒度：契约层不该假装知道 FFmpeg 或 WebCodecs 的错误分类，
/// 具体信息放 `message`。等到 M4 真接上实现，再按实际遇到的类别细分。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MediaError {
    /// 容器打不开或格式不认识。
    UnsupportedContainer(String),
    /// 请求了容器里没有的流。
    StreamNotFound(String),
    /// seek 的目标超出范围。
    SeekOutOfRange {
        requested_frame: i64,
        available_frames: Option<i64>,
    },
    /// 解码器吐出的东西与声明的 [`VideoInfo`] 不符。
    FrameMismatch(String),
    /// 编码器拒绝输入。
    EncodeFailed(String),
    /// 其他，附原文。
    Other(String),
}

impl fmt::Display for MediaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedContainer(m) => write!(f, "不支持的容器：{m}"),
            Self::StreamNotFound(m) => write!(f, "流不存在：{m}"),
            Self::SeekOutOfRange {
                requested_frame,
                available_frames,
            } => match available_frames {
                Some(total) => write!(f, "seek 到第 {requested_frame} 帧，但素材只有 {total} 帧"),
                None => write!(f, "seek 到第 {requested_frame} 帧，超出可 seek 范围"),
            },
            Self::FrameMismatch(m) => write!(f, "解码输出与声明的视频信息不符：{m}"),
            Self::EncodeFailed(m) => write!(f, "编码失败：{m}"),
            Self::Other(m) => f.write_str(m),
        }
    }
}

impl core::error::Error for MediaError {}

// ---------------------------------------------------------------------------
// 契约
//
// 下面这些 trait 的实现分散在两个宿主里。**签名一旦冻结就不要随手改**：
// 时间线 JSON 契约与测试语料都建在它们之上。
// ---------------------------------------------------------------------------

/// 解复用器：打开容器、报告轨道信息、按帧 seek。
///
/// 注意 `seek` 的单位是**帧号**而不是时间戳。指导文档 §5.1 的铁律：时间用整数帧号。
/// "第 N 帧"在容器里未必有精确的时间戳（尤其是 VFR 素材），换算责任在实现方。
pub trait Demuxer {
    /// 视频轨信息。无视频轨返回 `None`。
    fn video_info(&self) -> Option<&VideoInfo>;

    /// 音频轨信息。无音频轨返回 `None`。
    fn audio_info(&self) -> Option<&AudioInfo>;

    /// 视频总帧数。未知返回 `None`（流式容器不算罕见）。
    fn video_frame_count(&self) -> Option<i64>;

    /// 定位到第 `frame` 帧。之后的 [`VideoDecoder::decode_frame`] 从它开始。
    fn seek_to_frame(&mut self, frame: i64) -> Result<(), MediaError>;
}

/// 视频解码器：把第 N 帧解成 CPU 侧的像素。
///
/// 返回值故意是借用而不是所有权：解码器自己持有帧缓冲池，
/// 调用方要在拿到数据后立刻把它拷进 GPU 纹理或编码器输入队列。
/// 想跨帧持有请显式 `to_owned`——这样"什么时候多了一份拷贝"在代码里看得见。
pub trait VideoDecoder {
    fn info(&self) -> &VideoInfo;

    /// 解出第 `frame` 帧。实现方负责按需 seek，但**同一帧重复调用应该命中缓存**，
    /// 因为 M5 的分片渲染会反复请求同一个 GOP 的首帧。
    fn decode_frame(&mut self, frame: i64) -> Result<DecodedVideoFrame<'_>, MediaError>;
}

/// 一帧 CPU 侧的像素数据。
///
/// 平面格式（YUV420p / NV12）用 `planes` 表达；打包格式只有一个平面。
/// 不提供"帮我转成 RGBA"的便捷方法——色彩转换是渲染图里的一等节点，
/// 藏进媒体层会让"画面发灰"变得无从追查。
#[derive(Debug)]
pub struct DecodedVideoFrame<'a> {
    /// 帧号。**这是引擎内部的时间标识**，不是容器里的 PTS。
    pub frame: i64,
    pub info: VideoInfo,
    pub planes: &'a [Plane<'a>],
    /// 该帧的有效期语义：`true` 表示下一次 `decode_frame` 调用后数据即失效。
    /// 硬解路径常见，调用方必须据此决定何时拷贝。
    pub borrowed: bool,
}

/// 一个像素平面。
#[derive(Debug)]
pub struct Plane<'a> {
    pub data: &'a [u8],
    /// 行跨度（字节）。可能大于 `width * bytes_per_pixel`——解码器为了对齐会留 padding。
    pub stride: usize,
    /// 平面宽度（像素）。色度平面是亮度的一半。
    pub width: u32,
    /// 平面高度（像素）。
    pub height: u32,
}

/// 已编码的一段码流。
#[derive(Clone, Debug)]
pub struct EncodedPacket {
    /// 该包对应的帧号。
    pub frame: i64,
    pub data: Vec<u8>,
    /// 是否是关键帧（IDR）。分片渲染要靠它切 GOP 边界。
    pub keyframe: bool,
}

/// 视频编码器。
///
/// `encode` 是**有状态**的：编码器内部维持前向参考帧，调用顺序必须与帧号顺序一致。
/// 这条约束在 M5 分片时会变成实际约束（每个分片需要独立的关键帧起点）。
pub trait VideoEncoder {
    fn encode(&mut self, frame: &DecodedVideoFrame<'_>) -> Result<(), MediaError>;

    /// 取出已经攒下的包。编码器会缓冲，直到攒够一个 GOP 才吐。
    fn drain(&mut self) -> Vec<EncodedPacket>;

    /// 冲刷，把缓冲里剩下的全部吐出。**必须调用**，否则最后一个 GOP 会丢。
    fn flush(&mut self) -> Result<(), MediaError>;
}

/// 频道用不上但契约里要有：音频编码器不做分片（音频一次编码到底，绕开全部边界问题）。
pub trait AudioEncoder {
    fn encode(&mut self, frame: &AudioFrame) -> Result<(), MediaError>;
    fn flush(&mut self) -> Result<(), MediaError>;
}

/// 一块音频样本。`start_sample` 以采样点为单位——整数，不是秒。
#[derive(Clone, Debug)]
pub struct AudioFrame {
    pub start_sample: i64,
    pub info: AudioInfo,
    pub samples: Vec<f32>,
}

/// 复用器：把视频与音频码流写进容器。
///
/// 调用顺序契约：先 `add_video_packet` 灌满视频，再 `add_audio_packet` 灌音频，
/// 最后 `finalize`。音频不参与分片，所以它的包一定是完整的一段。
pub trait Muxer {
    fn add_video_packet(&mut self, packet: EncodedPacket) -> Result<(), MediaError>;
    fn add_audio_packet(&mut self, packet: EncodedPacket) -> Result<(), MediaError>;
    fn finalize(self) -> Result<(), MediaError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packed_formats_report_bytes_per_pixel() {
        assert_eq!(PixelFormat::Rgba8UnormSrgb.bytes_per_pixel(), Some(4));
        assert!(PixelFormat::Bgra8UnormSrgb.is_packed_rgb());
    }

    #[test]
    fn planar_formats_refuse_to_guess() {
        // 4:2:0 的"每像素字节数"是 1.5，不是整数。这里必须返回 None 而不是糊一个 2，
        // 否则调用方会按错误的步长算缓冲大小。
        assert_eq!(PixelFormat::Yuv420p.bytes_per_pixel(), None);
        assert_eq!(PixelFormat::Nv12.bytes_per_pixel(), None);
        assert!(!PixelFormat::Nv12.is_packed_rgb());
    }

    #[test]
    fn bt709_presets_are_distinct() {
        assert_ne!(ColorSpace::BT709_LIMITED, ColorSpace::BT709_FULL);
        assert_eq!(ColorSpace::BT709_LIMITED.range, ColorRange::Limited);
        assert_eq!(ColorSpace::SRGB_FULL.transfer, TransferFunction::Srgb);
    }
}
