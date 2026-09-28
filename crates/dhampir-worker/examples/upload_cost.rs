//! 微基准：把「每帧把一帧源纹理搬进去」这件事**同步测准** ——
//! 这就是纯 CPU 出片那个瓶颈的**根因**。
//!
//! 跑：
//!   cargo run -q -p dhampir-worker --example upload_cost -- cpu 1920 1080 20
//!   cargo run -q -p dhampir-worker --example upload_cost -- gpu 1920 1080 20
//!
//! # 结论：**`write_texture` 在 WARP 上是 ~118 纳秒/texel**
//!
//! 实测（1920x1080，20 帧，各预热 2 帧）：
//!
//! ```text
//!                                           WARP        GPU
//!   F 纯 memcpy（物理下限）                 0.293 ms    0.303 ms
//!   B write_texture+poll（冲不掉 staging）  25.0 ms     2.92 ms
//!   C write_texture+submit+poll             244.3 ms    0.891 ms   <- 真实代价
//!   E 复用纹理（不每帧新建）                 245.8 ms    0.886 ms
//!   D 自管 staging+copy_buffer_to_texture   827.2 ms    0.918 ms   <- 差 3.4 倍
//!   G 空 submit+poll（什么都不搬）           0.123 ms    0.173 ms
//!   I 只建 encoder 不 submit                 0.003 ms    0.004 ms
//!   J Bgra8Unorm（同尺寸同字节）             250.0 ms
//!   K 加 RENDER_ATTACHMENT                 245.1 ms
//!   L Rgba16Float（**字节 x2**，texel 同）   258.9 ms    <- 只多 6%
//!   M 半尺寸（**texel 1/4**）                64.4 ms    <- 降到 1/3.8
//! ```
//!
//! **判据**：
//!
//! * **C - G = 244.2ms** 才是"搬那 8.3MB"本身的代价 —— 往返只要 0.12ms，
//!   所以**不是同步开销**。
//! * **L 与 C 几乎一样**（字节翻倍只多 6%）-> **不按字节**。
//! * **M ≈ C/3.8**（texel 降到 1/4）-> **按 texel**。
//! * 于是 **244.3ms / 2.0736M texel = 117.8 ns/texel**（M 组回算 124.2，一致）。
//!
//! 换格式（J）、加用途（K）、池化（E）**都没用**；自己管 staging（D）**差 3.4 倍**。
//!
//! # 这条结论解释了先前所有的怪现象
//!
//! 纯 CPU 出片"每帧 240ms、与输出分辨率无关、与内容无关、只跟**源**分辨率走" ——
//! 全部由这一条推出来：**成本 ∝ 源 texel 数**。特效、文字、层数之所以只占 1%，
//! 是因为它们不改变源 texel 数。
//!
//! # 上一版测错了什么（值得记）
//!
//! 上一版的结论是"WARP 上 `write_texture` 30.9ms，是 memcpy 下限的 100 倍"。
//! **那个数只是 CPU 侧的 staging 拷贝**（本版 B 组 = 25.0ms ≈ 它）：
//! `write_texture` 先把数据拷进 staging 缓冲，**真正进纹理的那次拷贝被推迟到
//! 下一次 `submit()`** —— 所以"write 之后立刻 poll"**冲不掉它**。
//!
//! 与管线里的实测正好对上：管线里给 `write_texture` 紧接着插 poll 只有 **2ms**，
//! 而它后面那个 poll（在 `submit` 之后）是 **227ms**。
//! 当时我把那两个数当成"矛盾"，其实是**同一件事的两半**。
//!
//! # 能改什么 / 不能改什么
//!
//! * **能**：减少源 texel 数（按输出尺寸预降采样；texel 降到 1/4 就快 4 倍）。
//! * **不能**：换格式、加用途标志、池化纹理、自己管 staging。
//! * **未验**：这一切都是 **WARP（Windows 的 D3D12 软件实现）**的行为。
//!   目标是 Linux，那边是 **lavapipe**（完全不同的实现）——
//!   **必须在那台机器上重跑本例子**，不能外推。
//!
//! # 各组在做什么
//!
//! | 组 | 做什么 | 强制完成的手段 |
//! |---|---|---|
//! | B | 新建 + `write_texture` | 只 poll（**冲不掉 staging**）|
//! | C | 新建 + `write_texture` | 先 `submit([])` 再 poll（**冲得掉**）|
//! | E | **复用**一张已建好的纹理 | 同 C |
//! | D | 自己管 staging + `copy_buffer_to_texture` | 显式 submit 再 poll |
//! | G | **什么都不搬**，只 submit + poll | —— 往返的代价 |
//! | L/M | 分别改**字节数**与 **texel 数** | 定死"按哪个收费" |

use std::time::Instant;

/// 一次测量。**预热两帧**：D3D12 首帧有管线/JIT 之类的一次性开销，
/// 算进去会把数拉高（上一版没有预热，也没有对照组，所以看不出测错了）。
fn bench(label: &str, frames: usize, mut body: impl FnMut()) {
    for _ in 0..2 {
        body();
    }
    let start = Instant::now();
    for _ in 0..frames {
        body();
    }
    let ms = start.elapsed().as_secs_f64() * 1000.0 / frames as f64;
    println!("  {label:<46}{ms:9.3} ms/帧");
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let which = args.get(1).map(String::as_str).unwrap_or("cpu");
    let width: u32 = args.get(2).and_then(|v| v.parse().ok()).unwrap_or(1920);
    let height: u32 = args.get(3).and_then(|v| v.parse().ok()).unwrap_or(1080);
    let frames: usize = args.get(4).and_then(|v| v.parse().ok()).unwrap_or(20);

    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: dhampir_core::gpu::NATIVE_BACKENDS,
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    });
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: which == "cpu",
        apply_limit_buckets: false,
    }))
    .expect("拿不到 adapter");
    let info = adapter.get_info();
    let (device, queue) =
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
            .expect("拿不到 device");

    println!(
        "adapter: {}  backend={:?}  type={:?}",
        info.name, info.backend, info.device_type
    );
    println!("源尺寸 {width}x{height}，{frames} 帧（各测预热 2 帧）\n");

    let extent = wgpu::Extent3d {
        width,
        height,
        depth_or_array_layers: 1,
    };
    let bytes: Vec<u8> = vec![0x80; (width * height * 4) as usize];
    let tex_desc = |label: &'static str| wgpu::TextureDescriptor {
        label: Some(label),
        size: extent,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    };
    let wait = |device: &wgpu::Device| {
        device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("poll");
    };
    let layout = wgpu::TexelCopyBufferLayout {
        offset: 0,
        bytes_per_row: Some(width * 4),
        rows_per_image: Some(height),
    };
    fn dst(texture: &wgpu::Texture) -> wgpu::TexelCopyTextureInfo<'_> {
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        }
    }

    // --- F. 纯 memcpy 下限 ---
    {
        let src = bytes.clone();
        let mut dst = vec![0u8; bytes.len()];
        bench("F 纯 memcpy（物理下限）", frames, || {
            dst.copy_from_slice(&src);
        });
        std::hint::black_box(&dst);
    }

    // --- B. 新建 + write_texture + 只 poll ---
    bench("B 新建+write_texture+poll（冲不掉 staging）", frames, || {
        let texture = device.create_texture(&tex_desc("b"));
        queue.write_texture(dst(&texture), &bytes, layout, extent);
        wait(&device);
    });

    // --- C. 新建 + write_texture + submit + poll ---
    bench("C 新建+write_texture+submit+poll（**冲得掉**）", frames, || {
        let texture = device.create_texture(&tex_desc("c"));
        queue.write_texture(dst(&texture), &bytes, layout, extent);
        let encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        queue.submit([encoder.finish()]);
        wait(&device);
    });

    // --- E. **复用**一张纹理 + write_texture + submit + poll ---
    {
        let texture = device.create_texture(&tex_desc("e"));
        bench("E **复用**纹理+write_texture+submit+poll", frames, || {
            queue.write_texture(dst(&texture), &bytes, layout, extent);
            let encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
            queue.submit([encoder.finish()]);
            wait(&device);
        });
    }

    // --- D. 自己管 staging + copy_buffer_to_texture + submit + poll ---
    {
        let staging = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("staging"),
            size: bytes.len() as u64,
            usage: wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let texture = device.create_texture(&tex_desc("d"));
        bench("D 自管 staging+copy_buffer_to_texture", frames, || {
            queue.write_buffer(&staging, 0, &bytes);
            let mut encoder =
                device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
            encoder.copy_buffer_to_texture(
                wgpu::TexelCopyBufferInfo {
                    buffer: &staging,
                    layout,
                },
                dst(&texture),
                extent,
            );
            queue.submit([encoder.finish()]);
            wait(&device);
        });
    }

    // --- G. **空转一圈**：什么都不做，只 submit + poll ---
    //
    // 这是最关键的一个对照。如果它本身就接近 240ms，那"上传慢"是假象 ——
    // 贵的是 **WARP 上一次 submit/poll 往返**，与搬多少字节无关。
    bench("G 空 submit+poll（什么都不搬）", frames, || {
        let encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        queue.submit([encoder.finish()]);
        wait(&device);
    });

    // --- H. 连 poll 两次（第二次应当是"已经完了"的一瞬间）---
    bench("H 空 submit+poll+poll", frames, || {
        let encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        queue.submit([encoder.finish()]);
        wait(&device);
        wait(&device);
    });

    // --- I. 只 create_command_encoder + finish（不 submit）---
    bench("I 只建 encoder 不 submit", frames, || {
        let encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        std::hint::black_box(encoder.finish());
    });

    // -------------------------------------------------------------------
    // 扫描：把「按字节」与「按 texel」分开
    // -------------------------------------------------------------------
    //
    // C 的量级是 34 MB/s —— 对一次内存到内存的拷贝来说**慢得离谱**。
    // 下面几组把嫌疑分开：
    //
    //   J 换个 8 位格式（Bgra8Unorm）：texel 数一样、字节数一样
    //   K 加上 RENDER_ATTACHMENT：看用途声明有没有影响
    //   L **Rgba16Float**：texel 数一样，**字节数翻倍**
    //        -> 若耗时也翻倍，说明按**字节**；若不变，说明按 **texel**
    //   M **半尺寸**（1/4 texel）：字节数也降到 1/4
    //        -> 与 L 一起就能定死
    let half_extent = wgpu::Extent3d {
        width: (width / 2).max(1),
        height: (height / 2).max(1),
        depth_or_array_layers: 1,
    };
    // 每个变体带**每 texel 字节数** —— `Rgba16Float` 是 8，别的都是 4。
    // 第一版把它的 `bytes_per_row` 也写成 4 字节，于是校验不过直接崩了。
    let textures_bits: Vec<(&str, wgpu::TextureFormat, wgpu::Extent3d, u32, wgpu::TextureUsages)> = vec![
        ("J Bgra8Unorm（同尺寸同字节）", wgpu::TextureFormat::Bgra8Unorm, extent, 4,
         wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST),
        ("K 加 RENDER_ATTACHMENT", wgpu::TextureFormat::Rgba8Unorm, extent, 4,
         wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST
             | wgpu::TextureUsages::RENDER_ATTACHMENT),
        ("L Rgba16Float（字节x2, texel 同）", wgpu::TextureFormat::Rgba16Float, extent, 8,
         wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST),
        ("M 半尺寸（texel 1/4, 字节 1/4）", wgpu::TextureFormat::Rgba8Unorm, half_extent, 4,
         wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST),
    ];
    for (label, format, ext, texel_bytes, usage) in textures_bits {
        let data: Vec<u8> = vec![0x80; (ext.width * ext.height * texel_bytes) as usize];
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("variant"),
            size: ext,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage,
            view_formats: &[],
        });
        let v_layout = wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(ext.width * texel_bytes),
            rows_per_image: Some(ext.height),
        };
        bench(label, frames, || {
            queue.write_texture(dst(&texture), &data, v_layout, ext);
            let encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
            queue.submit([encoder.finish()]);
            wait(&device);
        });
    }

    println!("\n  读法：C 是「每帧上传一帧源」的真实代价；");
    println!("        C-B 是上一版漏掉的那一段（staging -> 纹理）；");
    println!("        C-E 是每帧新建纹理的代价；C-F 是离物理下限还有多远；");
    println!("        **C-G 才是「搬那 8.3MB」本身的代价** —— 若 G 接近 C，那贵的是往返不是数据。");
}
