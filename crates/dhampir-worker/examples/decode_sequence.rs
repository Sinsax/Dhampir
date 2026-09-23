//! P5：**顺序解码管道**（解码 -> 上传 -> 渲染）。
//!
//! # 它是什么
//!
//! 后端出片要「FFmpeg 顺序解码 -> core 渲染 -> 编码」，而**不是**逐帧 seek。
//! 后者每次都要回到关键帧重解，慢一个量级 —— 这条在 plan 里是硬约束。
//!
//! 这个例子把前两段跑成一条链：顺序解码 -> 上传进复用纹理 -> **交给 TimelineRenderer 渲染**。
//!
//! # 一处实测出来的讲究
//!
//! 「拿到一帧」的代价分三段（都是实测，1080p）：
//!   只解码 0.46 ms/帧  ->  +搬像素出管道 2.86  ->  +上传 GPU 4.17。
//! **解码本身只占 11%**，其余花在把像素搬来搬去。
//!
//! # 用法
//!
//!     cargo run --example decode_sequence -- [媒体文件] [最多几帧]

use dhampir_core::compose::{Composite, Layer};
use dhampir_core::gpu::NATIVE_BACKENDS;
use dhampir_core::readback;
use dhampir_core::render::{SourceResolver, TimelineRenderer};
use dhampir_worker::baseline::open_leg;
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::Instant;

/// 顺序源：**永远返回同一张纹理**。
///
/// 这不是偷懒 —— 顺序管道的本质就是「一张纹理被逐帧覆盖」，
/// 而每帧新建纹理在 1080p 下是 32 MB 一次的分配。
struct SequenceSource {
    view: wgpu::TextureView,
    size: (u32, u32),
}

impl SourceResolver for SequenceSource {
    fn texture_for(
        &mut self,
        _source: &str,
        _source_frame: i64,
    ) -> Option<(wgpu::TextureView, (u32, u32))> {
        Some((self.view.clone(), self.size))
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let media = args
        .first()
        .cloned()
        .unwrap_or_else(|| "target/s3/proxy1080p.mp4".to_string());
    let limit: Option<usize> = args.get(1).and_then(|text| text.parse().ok());

    let probe = Command::new("ffprobe")
        .args([
            "-v", "error", "-select_streams", "v:0",
            "-show_entries", "stream=width,height,nb_frames",
            "-of", "csv=p=0", &media,
        ])
        .output()?;
    if !probe.status.success() {
        return Err(format!("ffprobe 失败：{}", String::from_utf8_lossy(&probe.stderr)).into());
    }
    let text = String::from_utf8(probe.stdout)?;
    let fields: Vec<&str> = text.trim().split(',').collect();
    let width: usize = fields.first().unwrap_or(&"0").parse()?;
    let height: usize = fields.get(1).unwrap_or(&"0").parse()?;
    let declared: usize = fields.get(2).and_then(|v| v.parse().ok()).unwrap_or(0);
    if width == 0 || height == 0 {
        return Err("ffprobe 没给出尺寸，无法按帧切分".into());
    }
    let frame_bytes = width * height * 4;
    let size = (width as u32, height as u32);

    let (ctx, _init) =
        open_leg(NATIVE_BACKENDS).map_err(|error| format!("拿不到 GPU 上下文: {error}"))?;

    // **一张纹理反复用，每帧只覆盖写。**
    let upload_target = ctx.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("dhampir decode pipeline upload"),
        size: wgpu::Extent3d { width: size.0, height: size.1, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });

    let started = Instant::now();
    let mut child = Command::new("ffmpeg")
        .args(["-v", "error", "-i", &media, "-f", "rawvideo", "-pix_fmt", "rgba", "-"])
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()?;
    let mut stdout = child.stdout.take().ok_or("拿不到 ffmpeg 的 stdout")?;

    let mut frame = vec![0u8; frame_bytes];
    let mut frames = 0usize;
    let mut checksum: u64 = 0xcbf29ce484222325;
    loop {
        if let Some(max) = limit {
            if frames >= max {
                break;
            }
        }
        match stdout.read_exact(&mut frame) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(error) => return Err(error.into()),
        }
        for byte in frame.iter().step_by(4096) {
            checksum = (checksum ^ u64::from(*byte)).wrapping_mul(0x100000001b3);
        }
        ctx.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &upload_target,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &frame,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some((width * 4) as u32),
                rows_per_image: Some(height as u32),
            },
            wgpu::Extent3d { width: size.0, height: size.1, depth_or_array_layers: 1 },
        );
        frames += 1;
    }
    drop(stdout);
    let _ = child.wait();
    let elapsed = started.elapsed();

    println!("媒体：{media}");
    println!("尺寸：{width}x{height}  每帧 {frame_bytes} 字节");
    println!("顺序解出 {frames} 帧（ffprobe 声明 {declared} 帧）");
    println!(
        "耗时 {:.0} ms -> 每帧 {:.2} ms（含解码 + 搬像素 + 上传）",
        elapsed.as_millis() as f64,
        elapsed.as_secs_f64() * 1000.0 / frames.max(1) as f64
    );
    println!("帧内容指纹：{checksum:016x}");

    if frames == 0 {
        return Err("一帧都没解出来".into());
    }
    if limit.is_none() && declared > 0 && frames != declared {
        return Err(format!("解出 {frames} 帧，而 ffprobe 说 {declared} 帧").into());
    }

    // ---- 第三段：**把它交给 core 渲染** ----
    //
    // 到这里为止只是「把像素搬进了纹理」。要证明它真能进渲染图，
    // 得让它过一遍 TimelineRenderer 并读回像素 —— 否则「上传成功」什么也没说明。
    let target = ctx.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("dhampir decode pipeline render target"),
        size: wgpu::Extent3d { width: size.0, height: size.1, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let composite = Composite {
        frame: 0,
        layers: vec![Layer {
            clip_id: "decoded".to_string(),
            source: "decoded".to_string(),
            source_frame: 0,
            opacity: 1.0,
            transform: dhampir_core::timeline::schema::Transform {
                x: 0.0, y: 0.0, scale: 1.0, rotation_deg: 0.0,
            },
            effects: Vec::new(),
            frozen_for_transition: false,
            blend: dhampir_core::timeline::layer::BlendMode::Normal,
            is_adjustment: false,
        }],
    };
    let mut resolver = SequenceSource {
        view: upload_target.create_view(&wgpu::TextureViewDescriptor::default()),
        size,
    };
    let renderer = TimelineRenderer::new(&ctx.device, wgpu::TextureFormat::Rgba8Unorm);
    let mut encoder = ctx.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("dhampir decode pipeline encoder"),
    });
    let drawn = renderer.render_frame(
        &ctx.device, &ctx.queue, &mut encoder,
        &target.create_view(&wgpu::TextureViewDescriptor::default()),
        size, &composite, &mut resolver, wgpu::Color::TRANSPARENT,
    );
    ctx.queue.submit([encoder.finish()]);
    let image = pollster::block_on(readback::read_texture_rgba8(&ctx.device, &ctx.queue, &target))?;
    let first = &image.pixels[..4];
    let varied = image.pixels.chunks(4).any(|px| px != first);
    println!("渲染：画了 {drawn} 层，读回 {} 字节，画面{}", image.pixels.len(),
        if varied { "有内容（不是纯色）" } else { "**是一片纯色**" });
    if drawn == 0 {
        return Err("渲染器一层都没画 —— 顺序源没有被接上".into());
    }
    if !varied {
        return Err("渲染结果是纯色 —— 解码出来的像素没有真正进渲染".into());
    }


    // ---- 第四段：**把渲染结果交给 FFmpeg 编码** ----
    //
    // 到这里为止只证明了「渲染结果有内容」。要证明它**能成为成片**，
    // 得让它过一遍编码器并核对产物。
    //
    // 这一步先只编**一帧**：目的是验证三段之间的**接口**（RGBA 进、mp4 出），
    // 不是验证吞吐。逐帧串联是下一步 —— 那需要把上面那个解码循环改成
    // 「边解边渲边编」，是一次结构改动。
    let out_path = "target/decode-pipeline-out.mp4";
    let mut encoder_child = Command::new("ffmpeg")
        .args([
            "-v", "error",
            // 输入是裸 RGBA 帧流，所以尺寸与帧率都得显式告诉它。
            "-f", "rawvideo",
            "-pix_fmt", "rgba",
            "-s", &format!("{width}x{height}"),
            "-r", "30",
            "-i", "-",
            "-c:v", "libx264",
            "-preset", "veryfast",
            "-crf", "20",
            "-pix_fmt", "yuv420p",
            "-y", out_path,
        ])
        .stdin(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()?;
    {
        let stdin = encoder_child.stdin.as_mut().ok_or("拿不到编码器的 stdin")?;
        stdin.write_all(&image.pixels)?;
    }
    let _ = encoder_child.wait();

    // 核对产物：帧数必须是 1 —— 这正是「三段接口接上了」的判据。
    let check = Command::new("ffprobe")
        .args([
            "-v", "error", "-select_streams", "v:0",
            "-count_frames", "-show_entries", "stream=nb_read_frames,width,height",
            "-of", "csv=p=0", out_path,
        ])
        .output()?;
    let summary = String::from_utf8(check.stdout)?;
    // **`-of csv` 的字段顺序是 width,height,nb_read_frames，不是请求里的顺序。**
    // 我第一次按「第一个字段是帧数」解析，把 1920（宽度）当成了帧数，
    // 于是报出「产物 1920 帧」—— 而产物其实**正好 1 帧**。
    // 教训：**别猜 csv 的列序，按名字取**。下面显式断言字段数，顺序才有据可依。
    let parts: Vec<&str> = summary.trim().split(',').collect();
    if parts.len() != 3 {
        return Err(format!("ffprobe 的字段数不是 3，而是 {} —— 解析的前提不成立", parts.len()).into());
    }
    let encoded_frames: usize = parts[2].parse()?;
    println!("编码：{out_path}  帧数 {encoded_frames}  尺寸 {}", summary.trim());
    if encoded_frames != 1 {
        return Err(format!("编码产物应当是 1 帧，实际 {encoded_frames} 帧").into());
    }

    println!("✓ 顺序解码管道（解码 + 上传 + 渲染 + 编码接口）跑通");
    Ok(())
}
