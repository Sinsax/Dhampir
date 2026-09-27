//! 微基准：「把一帧源纹理搬进去」这一步在 CPU 软件实现上有多贵。
//!
//! 跑：
//!   cargo run -q -p dhampir-worker --example upload_cost -- cpu 1920 1080 20 4
//!   cargo run -q -p dhampir-worker --example upload_cost -- gpu 1920 1080 20 4
//!
//! # 已经量出来的（Windows + WARP / RTX 4070）
//!
//! ```text
//!                                  WARP        GPU
//!   create_texture                 0.05 ms    0.54 ms
//!   write_texture + 同步等完成     30.9 ms    0.73 ms
//!   池化（4 张轮流写）             31.6 ms    1.38 ms
//!   纯 memcpy 同尺寸（物理下限）    0.31 ms    0.35 ms
//!   显式 staging + copy          1221.1 ms    4.87 ms   <- 我自己试的"优化"，差 40 倍
//! ```
//!
//! 两条值得记住的：
//!
//! 1. **WARP 上 `write_texture` 是 memcpy 下限的 100 倍**（31ms vs 0.31ms）。
//!    同一个操作在真 GPU 上只要 0.73ms —— 非常接近下限。
//! 2. **"自己管一块 staging buffer 再显式 copy" 是错的方向**：在 WARP 上 1221ms，
//!    比现状差 40 倍。教训不是"这个想法不对"，而是**在这里猜的代价很大** ——
//!    先量再改。
//!
//! # 但这个例子**没有**解释整帧的 240ms
//!
//! 逐帧剖出来的分项加起来（管道读 8.9 + 上传 ~31 + pass ~6 + 读回 2.4 ≈ 48ms）
//! **远小于**在管线里实测的 240ms/帧。而且**同一个上传操作在管线里测只有 2ms**、
//! 隔离测是 31ms —— **两处对不上，说明其中一次测量是错的，且没有找到错在哪**。
//!
//! 所以这个例子现在的用途是：**留作复现起点**，以及**记录那条 100 倍的下限差**
//! （那一半是可信的、可复现的）。要定位根因得换真 profiler（WPA/ETW 或 perf），
//! 而不是继续手插桩。

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let which = args.get(1).map(String::as_str).unwrap_or("cpu");
    let width: u32 = args.get(2).and_then(|v| v.parse().ok()).unwrap_or(1920);
    let height: u32 = args.get(3).and_then(|v| v.parse().ok()).unwrap_or(1080);
    let frames: usize = args.get(4).and_then(|v| v.parse().ok()).unwrap_or(30);

    let backends = dhampir_core::gpu::NATIVE_BACKENDS;
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends,
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
        info.name,
        info.backend,
        info.device_type
    );
    println!("源尺寸 {width}x{height}，{frames} 帧\n");

    let bytes: Vec<u8> = vec![0x80; (width * height * 4) as usize];

    // --- 1. 只新建纹理，不上传 ---
    let t = std::time::Instant::now();
    let mut textures = Vec::new();
    for _ in 0..frames {
        textures.push(device.create_texture(&wgpu::TextureDescriptor {
            label: Some("bench"),
            size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        }));
    }
    let create_ms = t.elapsed().as_secs_f64() * 1000.0 / frames as f64;
    drop(textures);

    // --- 2. 新建 + 上传，每帧同步等一次（把 GPU 侧真的做的活儿也计进来）---
    let mut total = 0.0f64;
    let mut alloc = 0.0f64;
    let mut copy = 0.0f64;
    for _ in 0..frames {
        let t0 = std::time::Instant::now();
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("bench"),
            size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        alloc += t0.elapsed().as_secs_f64() * 1000.0;

        let t1 = std::time::Instant::now();
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &bytes,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width * 4),
                rows_per_image: Some(height),
            },
            wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        );
        // **同步等它真的做完** —— 不同步就只是在量"把命令记下来"有多快。
        device.poll(wgpu::PollType::wait_indefinitely()).expect("poll");
        copy += t1.elapsed().as_secs_f64() * 1000.0;
        total += t0.elapsed().as_secs_f64() * 1000.0;

        // 别让纹理活太久：下一轮新建时上一张还在会掩盖分配器的行为。
        drop(texture);
    }
    let n = frames as f64;
    println!("  只 create_texture          {create_ms:8.3} ms/帧");
    println!("  create_texture 那一小段    {:8.3} ms/帧", alloc / n);
    println!("  write_texture + 同步等完成 {:8.3} ms/帧   <- 真正把 {:.1} MB 搬进纹理",
             copy / n, bytes.len() as f64 / 1048576.0);
    println!("  合计                       {:8.3} ms/帧", total / n);

    // --- 3. **池化**：预建 K 张，轮流写，不做每帧新建/销毁 ---
    //
    // 这一段是拿来做对照的：出片链路里源纹理是**每帧新建**的，
    // 而池子里同时还留着几张（`slots`）—— 于是分配器每帧都在
    // 「建一张新的 + 释放一张旧的」之间来回，而隔离测出来的上传只要 25ms/帧。
    // 如果是分配/释放的锅，这一段的数会明显低。
    let keep: usize = std::env::args().nth(5).and_then(|v| v.parse().ok()).unwrap_or(4);
    let mut pool: Vec<wgpu::Texture> = (0..keep)
        .map(|_| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some("bench-pool"),
                size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            })
        })
        .collect();
    let mut pooled = 0.0f64;
    for i in 0..frames {
        let t = std::time::Instant::now();
        let texture = &pool[i % keep];
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &bytes,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width * 4),
                rows_per_image: Some(height),
            },
            wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        );
        device.poll(wgpu::PollType::wait_indefinitely()).expect("poll");
        pooled += t.elapsed().as_secs_f64() * 1000.0;
    }
    println!();
    println!("  **池化**（{keep} 张轮流写） {:.3} ms/帧   <- 不建不销毁", pooled / n);
    pool.clear();

    // --- 4. 三种搬法对比 ---
    //
    // 嫌疑：`queue.write_texture` 每次调用都会**新要一块 staging buffer**，
    // 而软件实现上那块 buffer 的分配 + 拷贝可能才是真正的开销。
    // 对照：自己维护一块 staging buffer，用 `copy_buffer_to_texture` 显式拷。
    // 还有一条基准线：同样大小的**纯 memcpy**，看数据的物理下限在哪。
    let row_bytes = width * 4;
    let mut plain = 0.0f64;
    let mut staging_total = 0.0f64;
    {
        let mut dst = vec![0u8; bytes.len()];
        for _ in 0..frames {
            let t = std::time::Instant::now();
            dst.copy_from_slice(&bytes);
            plain += t.elapsed().as_secs_f64() * 1000.0;
        }
        std::hint::black_box(&dst);
    }
    {
        // 固定一块 staging buffer，反复写它，再显式拷进纹理。
        // 注意 `bytes_per_row` 要 256 对齐 —— 1920*4=7680 本来就是。
        let staging = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("bench-staging"),
            size: bytes.len() as u64,
            usage: wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("bench-staging-tex"),
            size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        for _ in 0..frames {
            let t = std::time::Instant::now();
            queue.write_buffer(&staging, 0, &bytes);
            let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
            enc.copy_buffer_to_texture(
                wgpu::TexelCopyBufferInfo {
                    buffer: &staging,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(row_bytes),
                        rows_per_image: Some(height),
                    },
                },
                wgpu::TexelCopyTextureInfo {
                    texture: &texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            );
            queue.submit([enc.finish()]);
            device.poll(wgpu::PollType::wait_indefinitely()).expect("poll");
            staging_total += t.elapsed().as_secs_f64() * 1000.0;
        }
    }
    println!();
    println!("  纯 memcpy 同尺寸           {:.3} ms/帧   <- 物理下限", plain / n);
    println!("  显式 staging + copy        {:.3} ms/帧   <- 自己管 staging buffer", staging_total / n);
}
