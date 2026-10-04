//! 动图（GIF / 动画 WebP）的**引擎内整段解码**。
//!
//! # 这是干什么的
//!
//! 宿主把动图的**原始字节**交给引擎，引擎在加载期一次性解码成「RGBA 帧序列 + 逐帧延迟表」。
//! 此后播放期一帧都不再解码 —— 合成只要按（时间线算好的）帧号取一张已经解好的位图。
//! 契约与收益见 `plan/dhampir-gif-native-decode-proposal.md` §4 / §8 / §9。
//!
//! # 为什么住在 core
//!
//! 出片（native）与预览（wasm）必须用**同一份**解码实现 —— 这是本模块唯一的存在理由。
//! 放宿主里就得写两遍，而两遍的实现迟早在 disposal、透明处理、延迟这些地方漂开，
//! 且那种漂移**在单帧里看不出来**（表现是「动图慢了半拍」或「某一帧是空的」）。
//! core 里不允许出现 `#[cfg]`（见 crate 文档），所以这里只有一条代码路径。
//!
//! # 跨端约定（三条）
//!
//! 1. **像素是直通 alpha 的 RGBA8**（非预乘）。GIF 的透明索引落成 alpha = 0；
//!    WebP 那边 `image-webp` 的合成本身就是直通语义（它的 `do_alpha_blending`
//!    注释写着 "assumed to be NOT pre-multiplied"）。上传纹理时按宿主既有位图路径的
//!    同一约定走，不在这里再动一次像素。
//! 2. **延迟表是逐帧毫秒**，长度恒等于帧数。合成侧的「时间 → 帧号」换算
//!    （`dhampir_timeline::layer::source_frame_at_delays`）拿它做前缀和 ——
//!    也就是说第 0 帧的延迟算第 0 帧的展示时长、不参与相位（第 0 帧从层起点起播，方案 §4.3）。
//! 3. **循环不在这里做**。素材是「一个环」这件事由层上的 `loop_source` 表达
//!    （与 `source_frame_looped` 同一套语义），本模块只交出「一圈」。
//!    `loop_count` 是文件里写的循环次数，供诊断与宿主参考，不参与取帧。
//!
//! # 内存
//!
//! 解码结果**全量常驻**（方案 §8.3-3 的定版：LRU 缺帧要整段重放，满足不了
//! 「seek 后首帧即时」）。所以有三道闸：原始字节上限、帧数上限、解码后字节预算。
//! 超限一律**明确失败**，不做静默降级 —— 调用方拿到错误后按「这一层画不出来」处理
//! （保留层、报 issue，不许 panic：出片逐帧路径里任何 panic 都是整块 chunk 失败）。

use std::fmt;
use std::io::Cursor;

use gif::{ColorOutput, DecodeOptions, DisposalMethod, Repeat};

/// 单个动图资产的**原始字节**上限：64MB（方案 §4.5 的超限项）。
pub const MAX_FILE_BYTES: usize = 64 * 1024 * 1024;

/// 解码后 RGBA 总量的上限：256MB。
///
/// 这不是「预计用量」，是**拦截畸形文件的闸**：正常贴纸是 66 帧 × 目标盒子 ≈ 20MB 量级，
/// 四张贴纸 80MB —— 预算留了三倍余量。超了说明要么文件畸形，要么调用方没按目标尺寸解码。
pub const MAX_DECODED_BYTES: u64 = 256 * 1024 * 1024;

/// 帧数上限。贴纸动图是几十帧量级；这个数只用来拦住畸形文件。
pub const MAX_FRAMES: usize = 4096;

/// 动图容器格式。**按 magic 认，不按扩展名**（方案 §9.3-4）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnimFormat {
    Gif,
    WebP,
}

impl AnimFormat {
    /// 契约里用的名字（与工程 JSON 的 `assets[].kind` 无关 —— 那是登记时的猜测）。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Gif => "gif",
            Self::WebP => "webp",
        }
    }
}

/// 解好的一帧。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnimFrame {
    /// 这一帧停留多少毫秒。GIF 文件里存的是 10ms 单位，这里已乘 10。
    ///
    /// **原样保留 0**：合成侧的查表会把零宽的时间格跳过（那一帧在时间上不存在）。
    /// 「浏览器把 0 当成 100ms」是**播放器**的展示策略，而这里的表是**取帧的依据**；
    /// 真要做那个钳制，得先在两端把它定成契约（方案 §9.2）。
    pub delay_ms: u32,
    /// 直通 alpha 的 RGBA8，长度恒为 `width * height * 4`（**整张画布**，不是帧的增量矩形）。
    pub rgba: Vec<u8>,
}

/// 一整张动图的解码结果。加载期一次性产物，播放期只读。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Animation {
    pub format: AnimFormat,
    /// 画布宽。**不是某一帧矩形的宽** —— 动图的每一帧都已是整张画布。
    pub width: u32,
    /// 画布高。
    pub height: u32,
    /// 文件里写的循环次数；**0 = 无限循环**（GIF 与 WebP 同语义）。不参与取帧。
    pub loop_count: u32,
    /// 一圈的总时长（毫秒）＝ 全部帧延迟之和。
    pub total_ms: u64,
    /// 逐帧数据，下标即帧号。
    pub frames: Vec<AnimFrame>,
}

impl Animation {
    pub fn frame_count(&self) -> usize {
        self.frames.len()
    }

    /// 单帧的字节数（RGBA）。
    pub fn frame_bytes(&self) -> usize {
        (self.width as usize) * (self.height as usize) * 4
    }

    /// 解码结果占多少字节 —— 调用方拿它并进内存账。
    pub fn memory_bytes(&self) -> u64 {
        self.frame_bytes() as u64 * self.frames.len() as u64
    }

    /// 逐帧延迟表 —— 就是契约里 `frame_delays_ms` 的真值。
    pub fn delays_ms(&self) -> Vec<u32> {
        self.frames.iter().map(|frame| frame.delay_ms).collect()
    }

    /// 按帧号取一帧。越界返回 `None`（由调用方决定「跳层」还是「停在最后一帧」）。
    pub fn frame(&self, index: usize) -> Option<&AnimFrame> {
        self.frames.get(index)
    }
}

/// 动图解码的失败形态。**每一种都要能被宿主转成一条 issue**，不许 panic。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnimError {
    /// 原始字节超过上限。
    TooLarge { bytes: usize, limit: usize },
    /// 帧数超过上限（畸形文件）。
    TooManyFrames { limit: usize },
    /// 解码后的字节总量超过预算。
    OverBudget { bytes: u64, limit: u64 },
    /// 认出来的容器是**单帧图**：该走静态图那条路，不该来这儿。
    NotAnimation(&'static str),
    /// 容器都认不出来（既不是 GIF 也不是 WebP）。
    UnknownFormat,
    /// 认出来了但解不开：截断、损坏、字段非法。
    BadData(String),
}

impl fmt::Display for AnimError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge { bytes, limit } => {
                write!(f, "动图原始字节 {bytes} 超过上限 {limit}")
            }
            Self::TooManyFrames { limit } => write!(f, "动图帧数超过上限 {limit}"),
            Self::OverBudget { bytes, limit } => {
                write!(f, "动图解码后占 {bytes} 字节，超过预算 {limit}")
            }
            Self::NotAnimation(what) => write!(f, "{what} 是单帧图，不是动图"),
            Self::UnknownFormat => write!(f, "认不出动图格式（既不是 GIF 也不是 WebP）"),
            Self::BadData(message) => write!(f, "动图解不开：{message}"),
        }
    }
}

impl std::error::Error for AnimError {}

/// 按 magic 认格式：`GIF87a` / `GIF89a`，或 `RIFF????WEBP`。
///
/// **不看扩展名**：登记表里的 kind 是录入时的猜测，magic 是事实。
/// 两者不一致时以事实为准并报 warning（方案 §9.3-4）。
pub fn detect_format(bytes: &[u8]) -> Option<AnimFormat> {
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Some(AnimFormat::Gif);
    }
    if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return Some(AnimFormat::WebP);
    }
    None
}

/// 解一张动图（默认的三道闸）。
pub fn decode(bytes: &[u8]) -> Result<Animation, AnimError> {
    decode_with_limits(bytes, MAX_FILE_BYTES, MAX_DECODED_BYTES, MAX_FRAMES)
}

/// 解一张动图，闸门由调用方给。
///
/// 闸门可传参是为了**单测能构造超限**：真拿 64MB 去撞默认闸门，测试会慢得像做贼。
pub fn decode_with_limits(
    bytes: &[u8],
    file_limit: usize,
    byte_budget: u64,
    frame_limit: usize,
) -> Result<Animation, AnimError> {
    if bytes.len() > file_limit {
        return Err(AnimError::TooLarge {
            bytes: bytes.len(),
            limit: file_limit,
        });
    }
    match detect_format(bytes) {
        Some(AnimFormat::Gif) => decode_gif(bytes, byte_budget, frame_limit),
        Some(AnimFormat::WebP) => decode_webp(bytes, byte_budget, frame_limit),
        None => Err(AnimError::UnknownFormat),
    }
}

// ---------------------------------------------------------------------------
// GIF
// ---------------------------------------------------------------------------
//
// `gif` crate 交出来的是**逐帧的增量矩形**（left/top/width/height + 索引/RGBA 缓冲），
// **不带画布**。也就是说 disposal 合成是调用方的事 —— 这正是本模块与
// 「把文件丢给现成库」的区别所在。下面这套状态机就是 GIF 合成语义的全部：
//
//   1. 先执行**上一帧**的处置（它决定这一帧画在什么底色上）
//   2. 本帧若是「处置到上一帧」，在**绘制之前**留底
//   3. 绘制本帧（透明像素不落笔）
//   4. 把整张画布拷成一帧交出去

/// 一帧在画布上占的矩形。
#[derive(Debug, Clone, Copy)]
struct Rect {
    left: u16,
    top: u16,
    width: u16,
    height: u16,
}

fn decode_gif(bytes: &[u8], byte_budget: u64, frame_limit: usize) -> Result<Animation, AnimError> {
    let options = {
        let mut options = DecodeOptions::new();
        // RGBA 而不是索引：调色板查找交给 crate（局部调色板、交错、裁剪那套它有），
        // 但**画布合并不在它里面** —— 见上面那段注释。
        options.set_color_output(ColorOutput::RGBA);
        options
    };
    let mut decoder = options
        .read_info(Cursor::new(bytes))
        .map_err(|error| AnimError::BadData(format!("GIF 头读不出来：{error}")))?;

    let width = u32::from(decoder.width());
    let height = u32::from(decoder.height());
    if width == 0 || height == 0 {
        return Err(AnimError::BadData("GIF 的逻辑屏幕是 0 像素".to_string()));
    }
    // 0（文件里写的）= 无限循环；两个格式同语义，原样落到契约字段里。
    let loop_count = match decoder.repeat() {
        Repeat::Infinite => 0,
        Repeat::Finite(times) => u32::from(times),
    };

    let stride = (width as usize) * (height as usize) * 4;
    let mut canvas = vec![0u8; stride];
    // 画布初始为**全透明**：GIF 的「背景色」在现代用法里几乎总是那个被忽略的索引色
    // （WebP 那边对带 alpha 的动图同样按透明处理）。这是**跨端要一致的选择**，
    // 不是随手写的默认值 —— 方案 §4.1 的对齐项之一。

    let mut pending_dispose = DisposalMethod::Any;
    let mut pending_rect = Rect {
        left: 0,
        top: 0,
        width: 0,
        height: 0,
    };
    let mut saved_for_previous: Option<Vec<u8>> = None;

    let mut frames: Vec<AnimFrame> = Vec::new();
    let mut total_ms: u64 = 0;
    let mut decoded_bytes: u64 = 0;

    while let Some(frame) = decoder.read_next_frame().map_err(|error| {
        AnimError::BadData(format!("GIF 第 {} 帧解不开：{error}", frames.len() + 1))
    })? {
        let delay_ms = u32::from(frame.delay) * 10;
        let dispose = frame.dispose;
        let rect = Rect {
            left: frame.left,
            top: frame.top,
            width: frame.width,
            height: frame.height,
        };
        // 缓冲要拷出来：`frame` 借在 `decoder` 上，下一轮循环还要用 `decoder`。
        let buffer = frame.buffer.to_vec();

        // ① 上一帧的处置
        match pending_dispose {
            DisposalMethod::Background => {
                clear_rect(&mut canvas, width, height, pending_rect);
            }
            DisposalMethod::Previous => {
                if let Some(restored) = saved_for_previous.take() {
                    canvas = restored;
                }
            }
            // `Any` 是「解码器随意」，`Keep` 是「别动」—— 两者都按「不动」做，
            // 这也是浏览器的做法（差异只在畸形文件上体现）。
            DisposalMethod::Any | DisposalMethod::Keep => {}
        }
        // ② 本帧要「处置到上一帧」就先留底（必须在绘制之前）
        if dispose == DisposalMethod::Previous {
            saved_for_previous = Some(canvas.clone());
        }
        // ③ 绘制
        blit_rgba(&mut canvas, width, height, rect, &buffer);

        total_ms += u64::from(delay_ms);
        decoded_bytes += stride as u64;
        if decoded_bytes > byte_budget {
            return Err(AnimError::OverBudget {
                bytes: decoded_bytes,
                limit: byte_budget,
            });
        }
        if frames.len() >= frame_limit {
            return Err(AnimError::TooManyFrames { limit: frame_limit });
        }
        frames.push(AnimFrame {
            delay_ms,
            rgba: canvas.clone(),
        });

        pending_dispose = dispose;
        pending_rect = rect;
    }

    if frames.is_empty() {
        return Err(AnimError::BadData("GIF 里一帧都没有".to_string()));
    }

    Ok(Animation {
        format: AnimFormat::Gif,
        width,
        height,
        loop_count,
        total_ms,
        frames,
    })
}

/// 把一帧的 RGBA 贴到画布上。
///
/// **透明像素（alpha = 0）不落笔**：GIF 的「透明索引」语义就是「这里露出画布上已有的东西」，
/// 覆盖写会把上一帧擦掉 —— 而症状是「动图有的帧少了一块」，看起来像解码器坏了。
///
/// 矩形允许超出画布（`check_frame_consistency` 默认是关的）：超出部分丢掉。
/// 畸形文件的越界矩形不该让整张图失败，但也不该画出越界的内存。
fn blit_rgba(canvas: &mut [u8], canvas_w: u32, canvas_h: u32, rect: Rect, rgba: &[u8]) {
    let frame_w = u32::from(rect.width);
    let frame_h = u32::from(rect.height);
    let origin_x = u32::from(rect.left);
    let origin_y = u32::from(rect.top);
    for row in 0..frame_h {
        let y = origin_y + row;
        if y >= canvas_h {
            break;
        }
        for col in 0..frame_w {
            let x = origin_x + col;
            if x >= canvas_w {
                break;
            }
            let src = ((row * frame_w + col) as usize) * 4;
            if src + 4 > rgba.len() {
                return;
            }
            if rgba[src + 3] == 0 {
                continue;
            }
            let dst = ((y * canvas_w + x) as usize) * 4;
            canvas[dst..dst + 4].copy_from_slice(&rgba[src..src + 4]);
        }
    }
}

/// 把一个矩形抹成透明（`Background` 处置）。
fn clear_rect(canvas: &mut [u8], canvas_w: u32, canvas_h: u32, rect: Rect) {
    let frame_w = u32::from(rect.width);
    let frame_h = u32::from(rect.height);
    let origin_x = u32::from(rect.left);
    let origin_y = u32::from(rect.top);
    for row in 0..frame_h {
        let y = origin_y + row;
        if y >= canvas_h {
            break;
        }
        for col in 0..frame_w {
            let x = origin_x + col;
            if x >= canvas_w {
                break;
            }
            let dst = ((y * canvas_w + x) as usize) * 4;
            canvas[dst..dst + 4].copy_from_slice(&[0, 0, 0, 0]);
        }
    }
}

// ---------------------------------------------------------------------------
// 动画 WebP
// ---------------------------------------------------------------------------
//
// 与 GIF 的分工**正相反**：`image-webp` 自己维护画布，`read_frame` 交出来的就是
// **合成好的整张画布**，顺手把这一帧的时长（毫秒）返回。所以这里没有 disposal 状态机，
// 只需要「循环读、收帧、查账」。
//
// 两个要注意的地方：
//   * `read_frame` 会断言缓冲长度等于 `output_buffer_size()` —— 尺寸必须照它给的来；
//   * **没有 alpha 的动画 WebP 输出 RGB（3 字节/像素）**，而我们的约定是 RGBA8，
//     所以那一支要自己补 alpha = 255。

fn decode_webp(bytes: &[u8], byte_budget: u64, frame_limit: usize) -> Result<Animation, AnimError> {
    let mut decoder = image_webp::WebPDecoder::new(Cursor::new(bytes))
        .map_err(|error| AnimError::BadData(format!("WebP 头读不出来：{error}")))?;

    // 静态 WebP 走静态图那条路：这里明确报出来，让调用方能把它变成一条 issue，
    // 而不是画出一张「只有第一帧」的动图（那种错看起来像动图不动了）。
    if !decoder.is_animated() {
        return Err(AnimError::NotAnimation("WebP"));
    }

    let (width, height) = decoder.dimensions();
    if width == 0 || height == 0 {
        return Err(AnimError::BadData("WebP 画布是 0 像素".to_string()));
    }
    let declared_frames = decoder.num_frames() as usize;
    if declared_frames > frame_limit {
        return Err(AnimError::TooManyFrames { limit: frame_limit });
    }
    let has_alpha = decoder.has_alpha();
    let buffer_size = decoder.output_buffer_size().ok_or_else(|| {
        AnimError::BadData("WebP 报不出输出缓冲大小（尺寸超出实现上限）".to_string())
    })?;
    // WebP 的循环次数也是个两态枚举。**0 = 无限**是我们契约里的写法
    // （与 GIF 文件里那个 0 同义），所以 Forever 落成 0。
    let loop_count = match decoder.loop_count() {
        image_webp::LoopCount::Forever => 0,
        image_webp::LoopCount::Times(times) => u32::from(times.get()),
    };

    let stride = (width as usize) * (height as usize) * 4;
    let mut buffer = vec![0u8; buffer_size];
    let mut frames: Vec<AnimFrame> = Vec::with_capacity(declared_frames);
    let mut total_ms: u64 = 0;
    let mut decoded_bytes: u64 = 0;

    for index in 0..declared_frames {
        // 返回的是**这一帧的时长**（毫秒）—— 契约里的延迟表直接就是它。
        let delay_ms = decoder.read_frame(&mut buffer).map_err(|error| {
            AnimError::BadData(format!("WebP 第 {} 帧解不开：{error}", index + 1))
        })?;
        let rgba = if has_alpha {
            buffer.clone()
        } else {
            rgb_to_rgba(&buffer)
        };

        total_ms += u64::from(delay_ms);
        decoded_bytes += stride as u64;
        if decoded_bytes > byte_budget {
            return Err(AnimError::OverBudget {
                bytes: decoded_bytes,
                limit: byte_budget,
            });
        }
        frames.push(AnimFrame { delay_ms, rgba });
    }

    if frames.is_empty() {
        return Err(AnimError::BadData("WebP 里一帧都没有".to_string()));
    }

    Ok(Animation {
        format: AnimFormat::WebP,
        width,
        height,
        loop_count,
        total_ms,
        frames,
    })
}

/// RGB8 → RGBA8（alpha 恒 255）。没有 alpha 通道的动画 WebP 走这一支。
fn rgb_to_rgba(rgb: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(rgb.len() / 3 * 4);
    for pixel in rgb.chunks_exact(3) {
        out.extend_from_slice(&[pixel[0], pixel[1], pixel[2], 255]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::borrow::Cow;

    // 调色板：0 红 / 1 蓝 / 2 绿 / 3 白。四个色是为了让 GIF 的调色板是 2 的幂
    // （编码器对非 2 的幂的全局调色板会报错）。
    const RED: [u8; 4] = [255, 0, 0, 255];
    const BLUE: [u8; 4] = [0, 0, 255, 255];
    const GREEN: [u8; 4] = [0, 255, 0, 255];
    const WHITE: [u8; 4] = [255, 255, 255, 255];
    const CLEAR: [u8; 4] = [0, 0, 0, 0];

    fn palette() -> Vec<u8> {
        vec![255, 0, 0, 0, 0, 255, 0, 255, 0, 255, 255, 255]
    }

    /// 造一帧。«indices» 是画布上的索引（长度 = width*height）。
    fn frame(
        canvas: (u16, u16),
        rect: (u16, u16, u16, u16),
        indices: &[u8],
        delay_cs: u16,
        dispose: DisposalMethod,
        transparent: Option<u8>,
    ) -> gif::Frame<'static> {
        let frame = gif::Frame {
            width: rect.2,
            height: rect.3,
            left: rect.0,
            top: rect.1,
            buffer: Cow::Owned(indices.to_vec()),
            delay: delay_cs,
            dispose,
            transparent,
            ..gif::Frame::default()
        };
        // «canvas» 只用来提醒这个函数知道逻辑屏幕尺寸；GIF 的帧不携带它。
        let _ = canvas;
        frame
    }

    fn encode(
        canvas: (u16, u16),
        repeat: Option<Repeat>,
        frames: &[gif::Frame<'static>],
    ) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut encoder = gif::Encoder::new(&mut out, canvas.0, canvas.1, &palette())
                .expect("编码器能建起来");
            if let Some(repeat) = repeat {
                encoder.set_repeat(repeat).expect("能设循环");
            }
            for frame in frames {
                encoder.write_frame(frame).expect("写得进一帧");
            }
        }
        out
    }

    fn px(animation: &Animation, index: usize, x: u32, y: u32) -> [u8; 4] {
        let stride = (animation.width as usize) * 4;
        let offset = y as usize * stride + x as usize * 4;
        let frame = &animation.frames[index].rgba;
        [
            frame[offset],
            frame[offset + 1],
            frame[offset + 2],
            frame[offset + 3],
        ]
    }

    #[test]
    fn 嗅探按_magic_认格式() {
        assert_eq!(detect_format(b"GIF87a....."), Some(AnimFormat::Gif));
        assert_eq!(detect_format(b"GIF89a....."), Some(AnimFormat::Gif));
        // 位置 0..4 是 RIFF、8..12 是 WEBP —— 中间四个字节是长度，随便填。
        assert_eq!(detect_format(b"RIFF____WEBPVP8X"), Some(AnimFormat::WebP));
        assert_eq!(detect_format(b"RIFF????WEBP"), Some(AnimFormat::WebP));
        // 认不出来的一律 None，不猜。
        assert_eq!(detect_format(b"GIF"), None);
        assert_eq!(detect_format(b"RIFF????????XX"), None);
        // PNG 的 magic：认不出就得是 None（别把静态图当动图收下）。
        let png_magic = [0x89u8, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        assert_eq!(detect_format(&png_magic), None);
        assert_eq!(detect_format(&[]), None);
    }

    #[test]
    fn 认不出的字节报_unknown_format() {
        assert_eq!(
            decode(b"not an animation at all").unwrap_err(),
            AnimError::UnknownFormat
        );
    }

    #[test]
    fn gif_帧延迟按_10ms_单位换算且总时长是前缀和() {
        // 延迟写的是 10ms 单位：10 / 20 / 3 → 100 / 200 / 30 毫秒。
        let bytes = encode(
            (2, 2),
            None,
            &[
                frame(
                    (2, 2),
                    (0, 0, 2, 2),
                    &[0, 1, 2, 3],
                    10,
                    DisposalMethod::Keep,
                    None,
                ),
                frame(
                    (2, 2),
                    (0, 0, 2, 2),
                    &[1, 1, 1, 1],
                    20,
                    DisposalMethod::Keep,
                    None,
                ),
                frame(
                    (2, 2),
                    (0, 0, 2, 2),
                    &[2, 2, 2, 2],
                    3,
                    DisposalMethod::Keep,
                    None,
                ),
            ],
        );
        let animation = decode(&bytes).expect("解得开");
        assert_eq!(animation.format, AnimFormat::Gif);
        assert_eq!((animation.width, animation.height), (2, 2));
        assert_eq!(animation.delays_ms(), vec![100, 200, 30]);
        assert_eq!(animation.total_ms, 330);
        // 每帧都是**整张画布**，不是增量矩形。
        for index in 0..animation.frame_count() {
            assert_eq!(animation.frames[index].rgba.len(), animation.frame_bytes());
        }
    }

    #[test]
    fn gif_零延迟原样保留不钳制() {
        // 钳制（浏览器把 0 当 100ms）是**播放器**的策略，不是取帧的依据 ——
        // 这里如实保留 0，合成侧的零宽时间格自然被跳过。
        let bytes = encode(
            (2, 2),
            None,
            &[frame(
                (2, 2),
                (0, 0, 2, 2),
                &[0, 0, 0, 0],
                0,
                DisposalMethod::Keep,
                None,
            )],
        );
        let animation = decode(&bytes).expect("解得开");
        assert_eq!(animation.delays_ms(), vec![0]);
        assert_eq!(animation.total_ms, 0);
    }

    #[test]
    fn gif_透明像素不落笔_画布上已画的东西留着() {
        // 第 2 帧把索引 0 声明成透明，只有索引 1（蓝）落笔。
        // 若实现是「整块覆盖写」，(0,0) 与 (0,1) 会变成透明 —— 那正是要钉住的错。
        let bytes = encode(
            (2, 2),
            None,
            &[
                frame(
                    (2, 2),
                    (0, 0, 2, 2),
                    &[0, 1, 2, 3],
                    10,
                    DisposalMethod::Keep,
                    None,
                ),
                frame(
                    (2, 2),
                    (0, 0, 2, 2),
                    &[0, 1, 0, 1],
                    10,
                    DisposalMethod::Keep,
                    Some(0),
                ),
            ],
        );
        let animation = decode(&bytes).expect("解得开");
        assert_eq!(px(&animation, 0, 0, 0), RED);
        assert_eq!(px(&animation, 0, 1, 0), BLUE);
        assert_eq!(px(&animation, 0, 0, 1), GREEN);
        assert_eq!(px(&animation, 0, 1, 1), WHITE);
        // 第 2 帧：只有蓝落笔，红的与绿的留着。
        assert_eq!(px(&animation, 1, 0, 0), RED, "透明像素把上一帧擦掉了");
        assert_eq!(px(&animation, 1, 1, 0), BLUE);
        assert_eq!(px(&animation, 1, 0, 1), GREEN, "透明像素把上一帧擦掉了");
        assert_eq!(px(&animation, 1, 1, 1), BLUE);
    }

    #[test]
    fn gif_background_处置把上一帧的矩形清成透明() {
        // 第 1 帧铺满整张画布、处置 = Background → 第 2 帧到来前画布已清空，
        // 于是第 2 帧里没被画到的地方是**透明**，不是第 1 帧的颜色。
        let bytes = encode(
            (2, 2),
            None,
            &[
                frame(
                    (2, 2),
                    (0, 0, 2, 2),
                    &[0, 1, 2, 3],
                    10,
                    DisposalMethod::Background,
                    None,
                ),
                frame((2, 2), (1, 1, 1, 1), &[2], 10, DisposalMethod::Keep, None),
            ],
        );
        let animation = decode(&bytes).expect("解得开");
        assert_eq!(px(&animation, 1, 1, 1), GREEN);
        assert_eq!(
            px(&animation, 1, 0, 0),
            CLEAR,
            "Background 处置没清掉上一帧"
        );
        assert_eq!(px(&animation, 1, 1, 0), CLEAR);
        assert_eq!(px(&animation, 1, 0, 1), CLEAR);
    }

    #[test]
    fn gif_previous_处置恢复到绘制之前而不是上一帧的样子() {
        // 第 1 帧铺满、处置 = Previous；第 2 帧只画一格。
        // 正确的语义：第 1 帧的处置在「第 2 帧绘制之前」执行，恢复的是第 1 帧**绘制之前**的画布
        // （全透明）—— 而不是第 1 帧的样子。所以第 2 帧里除那一格之外全是透明。
        let bytes = encode(
            (2, 2),
            None,
            &[
                frame(
                    (2, 2),
                    (0, 0, 2, 2),
                    &[0, 1, 2, 3],
                    10,
                    DisposalMethod::Previous,
                    None,
                ),
                frame((2, 2), (0, 0, 1, 1), &[3], 10, DisposalMethod::Keep, None),
                frame((2, 2), (1, 1, 1, 1), &[2], 10, DisposalMethod::Keep, None),
            ],
        );
        let animation = decode(&bytes).expect("解得开");
        assert_eq!(px(&animation, 1, 0, 0), WHITE);
        assert_eq!(
            px(&animation, 1, 1, 0),
            CLEAR,
            "Previous 恢复错了：露出了上一帧的像素"
        );
        assert_eq!(px(&animation, 1, 0, 1), CLEAR);
        assert_eq!(px(&animation, 1, 1, 1), CLEAR);
        // 第 3 帧：第 2 帧处置是 Keep，所以白的与绿的都在。
        assert_eq!(px(&animation, 2, 0, 0), WHITE);
        assert_eq!(px(&animation, 2, 1, 1), GREEN);
    }

    #[test]
    fn gif_循环次数读得出来_零表示无限() {
        let infinite = encode(
            (2, 2),
            Some(Repeat::Infinite),
            &[frame(
                (2, 2),
                (0, 0, 2, 2),
                &[0, 0, 0, 0],
                10,
                DisposalMethod::Keep,
                None,
            )],
        );
        assert_eq!(decode(&infinite).unwrap().loop_count, 0);

        let finite = encode(
            (2, 2),
            Some(Repeat::Finite(3)),
            &[frame(
                (2, 2),
                (0, 0, 2, 2),
                &[0, 0, 0, 0],
                10,
                DisposalMethod::Keep,
                None,
            )],
        );
        assert_eq!(decode(&finite).unwrap().loop_count, 3);
    }

    #[test]
    fn 三道闸都是明确失败而不是静默降级() {
        let bytes = encode(
            (2, 2),
            None,
            &[
                frame(
                    (2, 2),
                    (0, 0, 2, 2),
                    &[0, 1, 2, 3],
                    10,
                    DisposalMethod::Keep,
                    None,
                ),
                frame(
                    (2, 2),
                    (0, 0, 2, 2),
                    &[1, 1, 1, 1],
                    10,
                    DisposalMethod::Keep,
                    None,
                ),
            ],
        );
        // 原始字节上限
        assert_eq!(
            decode_with_limits(&bytes, bytes.len() - 1, MAX_DECODED_BYTES, MAX_FRAMES).unwrap_err(),
            AnimError::TooLarge {
                bytes: bytes.len(),
                limit: bytes.len() - 1
            }
        );
        // 帧数上限
        assert_eq!(
            decode_with_limits(&bytes, MAX_FILE_BYTES, MAX_DECODED_BYTES, 1).unwrap_err(),
            AnimError::TooManyFrames { limit: 1 }
        );
        // 解码后字节预算：单帧 2*2*4 = 16 字节，两帧 32 —— 预算给 20 就超。
        match decode_with_limits(&bytes, MAX_FILE_BYTES, 20, MAX_FRAMES).unwrap_err() {
            AnimError::OverBudget { bytes, limit } => {
                assert!(bytes > limit, "报出来的用量该超过预算：{bytes} vs {limit}");
                assert_eq!(limit, 20);
            }
            other => panic!("该报预算超限，实得 {other:?}"),
        }
    }

    #[test]
    fn 截断的_gif_报_bad_data_而不是_panic() {
        let bytes = encode(
            (2, 2),
            None,
            &[frame(
                (2, 2),
                (0, 0, 2, 2),
                &[0, 1, 2, 3],
                10,
                DisposalMethod::Keep,
                None,
            )],
        );
        // 掐掉尾巴：头还在（magic 认得出），后面解不动。
        let truncated = &bytes[..bytes.len().saturating_sub(12)];
        match decode(truncated) {
            Err(AnimError::BadData(_)) => {}
            other => panic!("截断的 GIF 该报 BadData，实得 {other:?}"),
        }
    }

    /// 动画 WebP 的夹具来自 «image-webp» 自己的测试资产（MIT/Apache-2.0，
    /// 见 tests/fixtures/CREDITS.md）。用真文件而不是自造的字节：
    /// 自造的 WebP 得手写 VP8L 位流，那不是测试该干的事。
    const ANIMATED_WEBP: &[u8] = include_bytes!("../tests/fixtures/animated_lossless.webp");

    #[test]
    fn webp_动画整段解码且与文件自报的账对得上() {
        let animation = decode(ANIMATED_WEBP).expect("解得开");
        assert_eq!(animation.format, AnimFormat::WebP);
        // 与库自报的帧数一致 —— 也顺带钉住「帧数与 num_frames 不是两套账」。
        let probe = image_webp::WebPDecoder::new(Cursor::new(ANIMATED_WEBP)).unwrap();
        assert_eq!(animation.frame_count(), probe.num_frames() as usize);
        assert_eq!((animation.width, animation.height), probe.dimensions());
        assert_eq!(animation.total_ms, probe.loop_duration());

        let stride = animation.frame_bytes();
        for (index, frame) in animation.frames.iter().enumerate() {
            assert_eq!(frame.rgba.len(), stride, "第 {index} 帧不是整张画布");
        }
        // 延迟之和等于总时长，且每一帧都有实际时长。
        let sum: u64 = animation.delays_ms().iter().map(|d| u64::from(*d)).sum();
        assert_eq!(sum, animation.total_ms);
        assert!(
            animation.delays_ms().iter().all(|d| *d > 0),
            "WebP 的逐帧时长不该是 0"
        );
        assert_eq!(
            animation.memory_bytes(),
            stride as u64 * animation.frame_count() as u64
        );
    }

    #[test]
    fn 静态字节流进不来_动图这条路只收动图() {
        // 把 WebP 的 RIFF 头留下、后面接垃圾：认得出容器，解不开 → BadData。
        let mut broken = ANIMATED_WEBP[..12].to_vec();
        broken.extend_from_slice(&[0u8; 32]);
        match decode(&broken) {
            Err(AnimError::BadData(_)) | Err(AnimError::NotAnimation(_)) => {}
            other => panic!("该报 BadData 或 NotAnimation，实得 {other:?}"),
        }
    }
}
