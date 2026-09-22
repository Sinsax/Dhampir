//! 离屏纹理 → CPU 像素。**两个宿主共用这一份**。
//!
//! 指导文档的设计要点（§5 设计要点 2）：M2 的双端比对里，wasm 侧也**不从 canvas
//! 抄像素**，而是渲染到离屏纹理再 `copy_texture_to_buffer`。原因有二：
//!
//! 1. canvas 纹理通常没有 `COPY_SRC` 用途，根本拷不出来
//! 2. 正好验证 `FrameSink` 抽象——"预览与导出共用渲染图，只是 sink 不同"
//!
//! 所以这段代码天然属于 core：两端要干的是同一件事，就该是同一份实现。
//! 它也是本仓库里唯一处理**行对齐**这个经典坑的地方。

use crate::wgpu;

/// `copy_texture_to_buffer` 要求 `bytes_per_row` 是 256 的倍数。
///
/// 这是 WebGPU 规范里最经典的一个坑：2560×1440 的 RGBA8 一行是 10240 字节，
/// 正好是 256 的倍数，于是本地开发一切正常；换到 1920×1080（一行 7680 字节，
/// 7680 / 256 = 30，也整除……）——真正会炸的是 1366×768 这类宽度。
/// 所以**不要靠"我这边能跑"来判断**，一律走 [`padded_bytes_per_row`]。
pub const COPY_BYTES_PER_ROW_ALIGNMENT: u32 = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;

/// 把 `unpadded` 向上取整到 `align` 的倍数。
pub const fn padded_bytes_per_row(unpadded: u32, align: u32) -> u32 {
    unpadded.div_ceil(align) * align
}

/// 一张紧密打包（无行填充）的 RGBA8 图像。
///
/// 无行填充是有意的：读回来之后立刻去掉 padding，让下游（PNG 编码、SSIM 比对）
/// 拿到的都是"`width * height * 4` 字节、逐行连续"的东西，不必各自再处理一次对齐。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rgba8Image {
    pub width: u32,
    pub height: u32,
    /// 长度恒为 `width * height * 4`。
    pub pixels: Vec<u8>,
}

impl Rgba8Image {
    /// 取像素，越界返回 `None`。返回 `[r, g, b, a]`。
    pub fn pixel(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let i = ((y * self.width + x) * 4) as usize;
        Some([
            self.pixels[i],
            self.pixels[i + 1],
            self.pixels[i + 2],
            self.pixels[i + 3],
        ])
    }

    /// 解码 PNG 成 [`Rgba8Image`]。
///
/// **为什么解码也在 core**：双端比对要读**另一侧**的产物，而"读得对不对"这件事
/// 同样不该两边各写一遍。用同一个 `png` crate，编解码都在这里，
/// 于是"两边看到的是同一批像素"是代码保证的，不是约定。
///
/// 只接受 8 位 RGBA：比对器认的就是这个格式，悄悄转换会把差异藏进转换里。
pub fn decode_png(bytes: &[u8]) -> Result<Rgba8Image, String> {
    let decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    let mut reader = decoder.read_info().map_err(|e| e.to_string())?;
    let mut buffer = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buffer).map_err(|e| e.to_string())?;
    if info.color_type != png::ColorType::Rgba || info.bit_depth != png::BitDepth::Eight {
        return Err(format!(
            "只支持 8 位 RGBA，得到 {:?}/{:?}",
            info.color_type, info.bit_depth
        ));
    }
    buffer.truncate(info.buffer_size());
    Ok(Rgba8Image {
        width: info.width,
        height: info.height,
        pixels: buffer,
    })
}

/// 把像素编码成 PNG 字节。
    ///
    /// **为什么编码器住在 core**：M2 的比对规则是"在编码后的字节上进行"
    /// （指导文档 §5 设计要点 3）——两端渲染完各自编码，编出来的字节直接比。
    /// 这条规则只有在**两端用同一个编码器**时才成立；如果 native 用 `image`、
    /// wasm 用浏览器 canvas 的 `toBlob`，那比出来的差异里混着两个编码器，
    /// 归因就废了。
    ///
    /// `png` crate 是纯 Rust，wasm32 上原样可用，所以这份实现在两端真的只写一次。
    /// 不返回字节而返回 `Vec<u8>`：调用方决定落盘还是比对。
    pub fn encode_png(&self) -> Result<Vec<u8>, PngError> {
        let mut out = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut out, self.width, self.height);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            // PNG 编码器内部有多线程/压缩级别之类的自由度，全部锁死在默认值上：
            // 这些参数一变，同样的像素会编出不同的字节，而字节相等正是 M2 的判据。
            encoder.set_compression(png::Compression::Default);
            encoder.set_filter(png::FilterType::Sub);
            let mut writer = encoder.write_header().map_err(PngError::Encode)?;
            writer
                .write_image_data(&self.pixels)
                .map_err(PngError::Encode)?;
        }
        Ok(out)
    }

    /// 编码 PNG 并写到 `path`。目录不存在会失败——不做隐式建目录，
    /// 记录文件的目录结构要显式出现在代码里。
    pub fn write_png(&self, path: &std::path::Path) -> Result<(), PngError> {
        let bytes = self.encode_png()?;
        std::fs::write(path, bytes).map_err(PngError::Io)
    }
}

/// PNG 编码 / 写盘失败。
#[derive(Debug)]
pub enum PngError {
    Io(std::io::Error),
    Encode(png::EncodingError),
}

impl core::fmt::Display for PngError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "写 PNG 文件失败：{e}"),
            Self::Encode(e) => write!(f, "PNG 编码失败：{e}"),
        }
    }
}

impl core::error::Error for PngError {}

/// 读回失败。
#[derive(Debug)]
pub enum ReadbackError {
    /// 纹理格式不是 8 位 RGBA/BGRA 家族，本函数不做格式转换。
    UnsupportedFormat(wgpu::TextureFormat),
    /// `map_async` 失败（设备丢失、纹理已被销毁等）。
    MapFailed(String),
    /// 驱动 device 轮询失败。native 上是 `PollError`，wasm 上不该出现。
    PollFailed(String),
    /// 缓冲区大小对不上——内部记账错误，属于 bug 而非环境问题。
    SizeMismatch { expected: u64, actual: u64 },
}

impl core::fmt::Display for ReadbackError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::UnsupportedFormat(got) => {
                write!(f, "读回只支持 Rgba8*/Bgra8* 格式，收到 {got:?}")
            }
            Self::MapFailed(m) => write!(f, "GPU 缓冲区映射失败：{m}"),
            Self::PollFailed(m) => write!(f, "驱动 GPU 设备轮询失败：{m}"),
            Self::SizeMismatch { expected, actual } => {
                write!(
                    f,
                    "读回缓冲区大小不符：期望 {expected} 字节，实际 {actual} 字节"
                )
            }
        }
    }
}

impl core::error::Error for ReadbackError {}

/// 纹理是否是本模块能处理的格式。
///
/// 刻意**不做格式转换**：读回是测量手段，测量手段自己引入一次色彩转换，
/// 后面就没法区分"两端渲染不同"和"转换不同"了。要 RGBA 就在渲染时就渲染成 RGBA。
pub const fn is_readable_format(format: wgpu::TextureFormat) -> bool {
    matches!(
        format,
        wgpu::TextureFormat::Rgba8Unorm
            | wgpu::TextureFormat::Rgba8UnormSrgb
            | wgpu::TextureFormat::Bgra8Unorm
            | wgpu::TextureFormat::Bgra8UnormSrgb
    )
}

/// `map_async` 回调与等待方之间的交接点。
///
/// 两个字段都在同一把锁里：回调写入 `outcome` 时顺便取走 waker，
/// 这样"检查结果"和"注册唤醒"不会交错成丢唤醒的经典竞态。
#[derive(Default)]
struct MapSlot {
    /// 回调结果。`None` 表示回调还没跑。
    outcome: Option<Result<(), wgpu::BufferAsyncError>>,
    waker: Option<std::task::Waker>,
}

impl MapSlot {
    /// 取锁。**中毒不 panic**：唯一持锁的代码是"写结果、唤 waker"，
    /// 里面没有可 panic 的语句；真中毒了也宁可继续走错误路径而不是把宿主进程带走。
    fn lock(slot: &std::sync::Mutex<Self>) -> std::sync::MutexGuard<'_, Self> {
        slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// 等 `map_async` 完成。
///
/// # 为什么手写这个 future，而不用 `wgpu::util::DownloadBuffer`
///
/// wgpu 30 把 `DownloadBuffer::read_buffer` 改成了**回调式**：
/// 它接受一个 `callback`，文档还明确要求"若之后没有别的提交，调用方必须自己反复
/// 调用 `Device::poll()` 直到回调跑完"。既然轮询责任回到了调用方，就自己写这层，
/// 换来一件更重要的事——**两个宿主共用同一份等待逻辑**：
///
/// - **native**：`Device::poll` 是阻塞的，`PollType::wait_indefinitely()` 会一直
///   等到提交执行完**且回调被调用**才返回。所以第一次 `poll` 就有结果，
///   对上层表现为同步调用。
/// - **wasm**：`Device::poll` 是文档化的 no-op（WebGPU 由事件循环驱动，
///   "Devices are automatically polled"）。这里会注册 waker 然后返回 `Pending`，
///   等 `mapAsync` 的 Promise 落地、回调执行时被唤醒重入。
///
/// 也就是说，**这段代码是 core 里唯一一处"同一份代码、两端行为不同"的地方**，
/// 而它依赖的是 wgpu 公开文档化的语义，不是 `#[cfg]`。正因如此它必须只有一份：
/// 一旦让宿主各自实现等待，两端一致就没人保证了。
///
/// 宿主怎么驱动这个 future：native 侧 `pollster`-风格的单线程 executor，
/// wasm 侧 `wasm_bindgen_futures`。core 自己不提供 executor——
/// 那属于运行时，不属于引擎。
struct MapWait<'a> {
    device: &'a wgpu::Device,
    slot: std::sync::Arc<std::sync::Mutex<MapSlot>>,
}

impl std::future::Future for MapWait<'_> {
    type Output = Result<(), ReadbackError>;

    fn poll(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        // 1) 回调已经跑过？直接出结果。
        if let Some(outcome) = MapSlot::lock(&self.slot).outcome.take() {
            return std::task::Poll::Ready(
                outcome.map_err(|e| ReadbackError::MapFailed(format!("{e:?}"))),
            );
        }

        // 2) 先注册 waker，再驱动轮询——顺序反了会丢唤醒。
        MapSlot::lock(&self.slot).waker = Some(cx.waker().clone());

        // 3) native 阻塞驱动到完成为止；wasm 是 no-op。
        if let Err(e) = self.device.poll(wgpu::PollType::wait_indefinitely()) {
            return std::task::Poll::Ready(Err(ReadbackError::PollFailed(format!("{e:?}"))));
        }

        // 4) 步骤 3 在 native 上返回时回调已执行，再查一次能省掉一次调度；
        //    wasm 上必然还是 None，就挂起等唤醒。
        match MapSlot::lock(&self.slot).outcome.take() {
            Some(outcome) => std::task::Poll::Ready(
                outcome.map_err(|e| ReadbackError::MapFailed(format!("{e:?}"))),
            ),
            None => std::task::Poll::Pending,
        }
    }
}

/// 把一张 8 位 RGBA/BGRA 纹理读回成紧密打包的 [`Rgba8Image`]。
///
/// 纹理必须有 `COPY_SRC` 用途。
///
/// **这是 `async` 而不是阻塞函数**：浏览器里没有"阻塞等 GPU"这件事，
/// 写了就挂主线程。native 侧由宿主用单线程 executor 把同一个 future 跑完，
/// 于是"读回"这段逻辑在两个宿主里真的只有一份（见 [`MapWait`]）。
pub async fn read_texture_rgba8(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
) -> Result<Rgba8Image, ReadbackError> {
    let format = texture.format();
    if !is_readable_format(format) {
        return Err(ReadbackError::UnsupportedFormat(format));
    }

    let width = texture.width();
    let height = texture.height();
    let unpadded_bpr = width * 4;
    let padded_bpr = padded_bytes_per_row(unpadded_bpr, COPY_BYTES_PER_ROW_ALIGNMENT);
    let buffer_size = u64::from(padded_bpr) * u64::from(height);

    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("dhampir readback"),
        size: buffer_size,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("dhampir readback encoder"),
    });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded_bpr),
                rows_per_image: Some(height),
            },
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );

    // 顺序是有讲究的：**先提交拷贝，再请求映射**。反过来的话，
    // 缓冲区在"有待执行拷贝"的状态下被 map，会撞上 WebGPU 对 buffer 状态的约束。
    // wgpu 自己的 `DownloadBuffer` 也是这个顺序。
    queue.submit([encoder.finish()]);

    let slot = std::sync::Arc::new(std::sync::Mutex::new(MapSlot::default()));
    {
        let slot = std::sync::Arc::clone(&slot);
        buffer
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |outcome| {
                let mut guard = MapSlot::lock(&slot);
                guard.outcome = Some(outcome);
                if let Some(waker) = guard.waker.take() {
                    waker.wake();
                }
            });
    }

    MapWait { device, slot }.await?;

    let downloaded = {
        let view = buffer
            .get_mapped_range(..)
            .map_err(|e| ReadbackError::MapFailed(format!("{e:?}")))?;

        if view.len() as u64 != buffer_size {
            return Err(ReadbackError::SizeMismatch {
                expected: buffer_size,
                actual: view.len() as u64,
            });
        }

        // 去掉行填充。BGRA 家族在这里顺便通道重排，让下游只认 RGBA。
        let swap_rb = matches!(
            format,
            wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb
        );
        let mut pixels = Vec::with_capacity((unpadded_bpr * height) as usize);
        for row in 0..height as usize {
            let start = row * padded_bpr as usize;
            let row_bytes = &view[start..start + unpadded_bpr as usize];
            if swap_rb {
                for px in row_bytes.chunks_exact(4) {
                    pixels.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
                }
            } else {
                pixels.extend_from_slice(row_bytes);
            }
        }

        // `BufferView` 必须先释放（`unmap` 要求没有存活的映射视图）。
        // 这个显式作用域就是为此存在的——注释在这里，是因为"删掉花括号"看起来无害。
        pixels
    };

    buffer.unmap();

    Ok(Rgba8Image {
        width,
        height,
        pixels: downloaded,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn padding_aligns_up() {
        // 实参是**字节数**，不是像素宽度——差一个 ×4 正好会落进"看起来对"的陷阱：
        // 2560 与 10240 都是 256 的倍数，把宽度当字节数传进去，这条测试照样是绿的。
        // 所以下面每一行都写成 `宽 × 4`，让单位在代码里就看得见。
        assert_eq!(padded_bytes_per_row(2560 * 4, 256), 10240); // 1440p：一行 10240，正好整除
        assert_eq!(padded_bytes_per_row(1920 * 4, 256), 7680); // 1080p：一行 7680，也整除
        assert_eq!(padded_bytes_per_row(1366 * 4, 256), 5632); // 1366 宽：5464 → 5632（真会炸的是这种）
        assert_eq!(padded_bytes_per_row(1, 256), 256); // 0 以外的最小值也要抬到一整行
        assert_eq!(padded_bytes_per_row(0, 256), 0); // 0 保持 0：空缓冲不该被抬成 256
    }

    #[test]
    fn alignment_constant_is_256() {
        assert_eq!(COPY_BYTES_PER_ROW_ALIGNMENT, 256);
    }

    #[test]
    fn pixel_indexing_is_row_major() {
        let img = Rgba8Image {
            width: 2,
            height: 2,
            pixels: vec![0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
        };
        assert_eq!(img.pixel(0, 0), Some([0, 1, 2, 3]));
        assert_eq!(img.pixel(1, 0), Some([4, 5, 6, 7]));
        assert_eq!(img.pixel(0, 1), Some([8, 9, 10, 11]));
        assert_eq!(img.pixel(2, 0), None);
        assert_eq!(img.pixel(0, 2), None);
    }
}
