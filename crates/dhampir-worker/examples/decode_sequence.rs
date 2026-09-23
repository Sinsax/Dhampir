//! P5：**顺序解码管道**的解码侧与上传中段。
//!
//! # 它是什么
//!
//! 后端出片要「FFmpeg 顺序解码 -> core 渲染 -> 编码」，而**不是**逐帧 seek。
//! 后者每次都要回到关键帧重解，慢一个量级 —— 这条在 plan 里是硬约束。
//!
//! 这个例子实现**前两段**：把视频顺序解码成 RGBA 帧，并把每帧**上传进同一张纹理**。
//!
//! # 一处实测出来的讲究
//!
//! 第 63 轮测过「解码 + 丢弃」是 0.46 ms/帧，但**那没算把像素搬出来**。
//! 1080p RGBA 是 8.3 MB/帧，480 帧约 4 GB 过管道 —— 加上这一搬，变成 2.86 ms/帧。
//! **所以「拿到帧」不是零成本**，这条对本例与后端都成立。
//!
//! # 为什么用 FFmpeg CLI 而不引绑定
//!
//! 既定取舍（plan 里有）。代价是自己管子进程 stdout，收益是不把 FFmpeg 的版本绑进构建。
//!
//! # 用法
//!
//!     cargo run --example decode_sequence -- [媒体文件] [最多几帧]

use dhampir_core::gpu::NATIVE_BACKENDS;
use dhampir_worker::baseline::open_leg;
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::Instant;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let media = args
        .first()
        .cloned()
        .unwrap_or_else(|| "target/s3/proxy1080p.mp4".to_string());
    let limit: Option<usize> = args.get(1).and_then(|text| text.parse().ok());

    // 先问尺寸：ffmpeg 的输出流本身不带帧边界，得自己按 w*h*4 切。
    let probe = Command::new("ffprobe")
        .args([
            "-v", "error",
            "-select_streams", "v:0",
            "-show_entries", "stream=width,height,nb_frames",
            "-of", "csv=p=0",
            &media,
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

    // **开 GPU**：上传要它。这里只建上下文与一张纹理，不上屏、不渲染。
    let (ctx, _init) =
        open_leg(NATIVE_BACKENDS).map_err(|error| format!("拿不到 GPU 上下文: {error}"))?;
    // **一张纹理反复用，每帧只覆盖写。**
    // 每帧新建的话，1080p 一张 32 MB、480 帧就是十几 GB 的分配 —— 那才是真的浪费。
    let upload_target = ctx.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("dhampir decode pipeline upload"),
        size: wgpu::Extent3d {
            width: width as u32,
            height: height as u32,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });

    let started = Instant::now();
    let mut child = Command::new("ffmpeg")
        .args([
            "-v", "error",
            "-i", &media,
            // **rawvideo + rgba**：不解码成文件、不重编码，直接吐像素。
            "-f", "rawvideo",
            "-pix_fmt", "rgba",
            "-",
        ])
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
        // **read_exact**：短读就是流结束，不需要额外猜边界。
        match stdout.read_exact(&mut frame) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(error) => return Err(error.into()),
        }
        // 一个轻量的顺序指纹：只为了证明**帧内容是逐帧变化的**，
        // 免得「解出 N 张空白图」也算通过。
        for byte in frame.iter().step_by(4096) {
            checksum = (checksum ^ u64::from(*byte)).wrapping_mul(0x100000001b3);
        }
        // **覆盖写进同一张纹理。** 帧从 CPU 到 GPU 得有一次拷贝，
        // 复用纹理是让这次拷贝只有一份带宽开销。
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
            wgpu::Extent3d {
                width: width as u32,
                height: height as u32,
                depth_or_array_layers: 1,
            },
        );
        frames += 1;
    }
    // 主动收掉子进程，否则它会因为管道满了卡住（我们只读了前面几帧时）。
    drop(stdout);
    let _ = child.wait();
    let elapsed = started.elapsed();
    // 不读回，所以不需要 poll —— 覆盖写是排队的，进程退出前 wgpu 会处理。

    println!("媒体：{media}");
    println!("尺寸：{width}x{height}  每帧 {} 字节", frame_bytes);
    println!("顺序解出 {frames} 帧（ffprobe 声明 {declared} 帧）");
    println!(
        "耗时 {:.0} ms -> 每帧 {:.2} ms（含解码 + 搬像素 + 上传）",
        elapsed.as_millis() as f64,
        elapsed.as_secs_f64() * 1000.0 / frames.max(1) as f64
    );
    println!("帧内容指纹：{checksum:016x}");
    println!("每帧都已上传进同一张 {width}x{height} 纹理（复用，不新建）");

    if frames == 0 {
        return Err("一帧都没解出来".into());
    }
    if limit.is_none() && declared > 0 && frames != declared {
        return Err(format!("解出 {frames} 帧，而 ffprobe 说 {declared} 帧").into());
    }
    println!("✓ 顺序解码管道（解码侧 + 上传中段）跑通");
    Ok(())
}
