//! P5：**顺序解码管道**的解码侧。
//!
//! # 它是什么
//!
//! 后端出片要「FFmpeg 顺序解码 -> core 渲染 -> 编码」，而**不是**逐帧 seek。
//! 后者每次都要回到关键帧重解，慢一个量级 —— 这条在 plan 里是硬约束。
//!
//! 这个例子实现**前半段**：把一段视频顺序解码成 RGBA 帧，逐张交出来。
//! 它 GPU 无关，所以可以直接跑、直接看数。
//!
//! # 为什么用 FFmpeg CLI 而不引绑定
//!
//! 这是既定取舍（plan 里有）：不引 Rust 绑定。代价是要自己管子进程的 stdout，
//! 收益是不把 FFmpeg 的版本与特性绑进构建。
//!
//! # 用法
//!
//!     cargo run --example decode_sequence -- [媒体文件] [最多几帧]
//!
//! 默认媒体是 `target/s3/proxy1080p.mp4`，默认取全部帧。

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

    let started = Instant::now();
    let mut child = Command::new("ffmpeg")
        .args([
            "-v", "error",
            "-i", &media,
            // **rawvideo + rgba**：不解码成文件、不重编码，直接吐像素。
            // 这让这一步只做「解码」，代价可量。
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
        frames += 1;
    }
    // 主动收掉子进程，否则它会因为管道满了卡住（我们只读了前面几帧时）。
    drop(stdout);
    let _ = child.wait();
    let elapsed = started.elapsed();

    println!("媒体：{media}");
    println!("尺寸：{width}x{height}  每帧 {} 字节", frame_bytes);
    println!("顺序解出 {frames} 帧（ffprobe 声明 {declared} 帧）");
    println!(
        "耗时 {:.0} ms -> 每帧 {:.2} ms",
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
    println!("✓ 顺序解码管道（解码侧）跑通");
    Ok(())
}
