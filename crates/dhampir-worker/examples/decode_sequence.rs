//! P5：**完整顺序解码管道** —— 解码 -> 上传 -> 渲染 -> 读回 -> 编码。
//!
//! # 它是什么
//!
//! 后端出片要「FFmpeg 顺序解码 -> core 渲染 -> 编码」，而**不是**逐帧 seek。
//! 后者每次都要回到关键帧重解，慢一个量级 —— 这条在 plan 里是硬约束。
//!
//! 这个例子把四段串成**一条流**：每解出一帧就上传、渲染、读回、写进编码器，
//! 最后产出一个完整的 mp4。
//!
//! # 为什么之前分了三轮才做到
//!
//! 前三轮分别只做了「解码」「上传」「单帧编码接口」—— 因为把循环改成流式
//! 是**一次结构改动**，不能靠「加一段」达成。前两次我都因为余量不足退回了。
//!
//! # 用法
//!
//!     cargo run --example decode_sequence -- [媒体文件] [最多几帧]
//!
//! 默认媒体 `target/s3/proxy1080p.mp4`，默认取 30 帧（1080p 每帧读回 8 MB，
//! 帧数太大会很慢，而那与「管道通不通」无关）。

use dhampir_core::compose::{self, Composite, Layer};
use dhampir_core::timeline::schema::Project;
use dhampir_core::gpu::NATIVE_BACKENDS;
use dhampir_core::readback;
use dhampir_core::render::{RenderSpace, SourceResolver, TimelineRenderer};
use dhampir_worker::baseline::open_leg;
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::Instant;

/// 顺序源：**永远返回同一张纹理**。
///
/// 顺序管道的本质就是「一张纹理被逐帧覆盖」；
/// 每帧新建纹理在 1080p 下是每次 32 MB 的分配。
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
    let limit: usize = args.get(1).and_then(|text| text.parse().ok()).unwrap_or(30);
    // 第三个参数给了工程文件的话，就**按工程求值**，而不是渲那张硬编码的单层。
    // 这样这条管道才能与浏览器那条（样本工程）比。
    let project: Option<Project> = match args.get(2) {
        Some(path) => {
            let text = std::fs::read_to_string(path)?;
            Some(serde_json::from_str(&text)?)
        }
        None => None,
    };

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
    // **注意 csv 的列序是 width,height,nb_frames**，不是请求里的顺序。
    // 上一轮我按「第一个是帧数」解析，把宽度当成了帧数。
    let width: usize = fields.first().unwrap_or(&"0").parse()?;
    let height: usize = fields.get(1).unwrap_or(&"0").parse()?;
    if width == 0 || height == 0 {
        return Err("ffprobe 没给出尺寸，无法按帧切分".into());
    }
    // **输出帧率要从源取，不能写死。**
    // 源是 60fps 而编码写死 30 的话，480 帧会被编成 16 秒而不是 8 秒 ——
    // 时长就错了，而帧数还是对的，所以只看帧数不会发现。
    // 单独再问一次：单字段的 csv 没有「列序」问题（我在帧数上已经栽过一次）。
    let fps_probe = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0",
               "-show_entries", "stream=avg_frame_rate", "-of", "csv=p=0", &media])
        .output()?;
    let fps_text = String::from_utf8(fps_probe.stdout)?;
    let fps_fields: Vec<&str> = fps_text.trim().split('/').collect();
    let fps_num: f64 = fps_fields.first().and_then(|v| v.parse().ok()).unwrap_or(0.0);
    let fps_den: f64 = fps_fields.get(1).and_then(|v| v.parse().ok()).unwrap_or(1.0);
    let source_fps = if fps_den > 0.0 { fps_num / fps_den } else { 30.0 };
    if !(source_fps > 0.0) {
        return Err("ffprobe 没给出帧率".into());
    }

    // **输出帧率必须跟着「在渲什么」走。**
    //
    // 渲裸视频时它该是源视频的 fps；**渲工程时它必须是工程的 timebase** ——
    // 因为那时我们渲的是**时间线的帧**，而时间线的帧率由 timebase 定义。
    //
    // 这一条我上一轮修错过一次：当时把「从源取帧率」当成了通用修法，
    // 于是 90 帧被编成 60fps / 1.5 秒，而浏览器那条是 30fps / 3.0 秒 ——
    // **帧数一样、时长差一半**。只看帧数发现不了。
    let encoder_fps = match &project {
        Some(loaded) => {
            let timebase = &loaded.timebase;
            if timebase.den == 0 {
                return Err("工程的 timebase 分母为 0".into());
            }
            f64::from(timebase.num) / f64::from(timebase.den)
        }
        None => source_fps,
    };

    let frame_bytes = width * height * 4;
    let size = (width as u32, height as u32);

    let (ctx, _init) =
        open_leg(NATIVE_BACKENDS).map_err(|error| format!("拿不到 GPU 上下文: {error}"))?;

    // **一张上传纹理 + 一个渲染目标，全程复用。**
    let upload_target = ctx.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("dhampir pipeline upload"),
        size: wgpu::Extent3d { width: size.0, height: size.1, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let render_target = ctx.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("dhampir pipeline render target"),
        size: wgpu::Extent3d { width: size.0, height: size.1, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let render_view = render_target.create_view(&wgpu::TextureViewDescriptor::default());
    let renderer = TimelineRenderer::new(&ctx.device, wgpu::TextureFormat::Rgba8Unorm);

    let single_layer = Composite {
        frame: 0,
        layers: vec![Layer {
            backdrop_effects: Vec::new(),
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
            corner_radius: 0.0,
            clip: None,
            mask: None,
            shadow: None,
            is_adjustment: false,
        }],
    };

    // 解码器：吐裸 RGBA。
    let mut decoder = Command::new("ffmpeg")
        .args([
            "-v", "error",
            "-i", &media,
            // **显式声明色彩矩阵。**
            //
            // 不写的话 FFmpeg 从容器元数据里「猜」，而浏览器（WebCodecs）也有一套自己的猜法 ——
            // 两边的默认值不一定相同（BT.601 vs 709），于是同一帧看起来偏色，
            // 而那不是渲染 bug。P5.3 的口径要求「同矩阵、同上采样」，
            // 所以这里把它**写死**，并由 check-sequential-decode 守卫确保它一直显式。
            "-vf", "scale=out_color_matrix=bt709",
            "-f", "rawvideo",
            "-pix_fmt", "rgba",
            "-",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()?;
    let mut decoder_out = decoder.stdout.take().ok_or("拿不到解码器的 stdout")?;

    // 编码器：吃裸 RGBA。尺寸与帧率必须显式告诉它（裸流没有这些信息）。
    let out_path = "target/decode-pipeline-out.mp4";
    let mut encoder = Command::new("ffmpeg")
        .args([
            "-v", "error",
            "-f", "rawvideo", "-pix_fmt", "rgba",
            "-s", &format!("{width}x{height}"), "-r", &format!("{encoder_fps}"),
            "-i", "-",
            "-c:v", "libx264", "-preset", "veryfast", "-crf", "20", "-pix_fmt", "yuv420p",
            "-y", out_path,
        ])
        .stdin(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()?;

    let started = Instant::now();
    let mut frame = vec![0u8; frame_bytes];
    let mut frames = 0usize;
    loop {
        if frames >= limit {
            break;
        }
        match decoder_out.read_exact(&mut frame) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(error) => return Err(error.into()),
        }

        // 1) 上传（覆盖写进复用纹理）
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

        // 2) 渲染（走 core 的同一个入口，与两个宿主一致）
        // **按工程求值**（给了工程的话）：这正是浏览器那条路调的东西，
        // 所以两边的差异只可能来自渲染与运行时，而不是「求值不同」。
        let composite = match &project {
            Some(loaded) => compose::evaluate(loaded, frames as i64),
            None => single_layer.clone(),
        };

        let mut resolver = SequenceSource {
            view: upload_target.create_view(&wgpu::TextureViewDescriptor::default()),
            size,
        };
        let mut command = ctx.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("dhampir pipeline encoder"),
        });
        let drawn = renderer.render_frame(
            &ctx.device, &ctx.queue, &mut command, &render_view,
            RenderSpace::square(size), &composite, &mut resolver, wgpu::Color::TRANSPARENT,
        );
        ctx.queue.submit([command.finish()]);
        if drawn == 0 {
            return Err(format!("第 {frames} 帧渲染器一层都没画").into());
        }

        // 3) 读回（这一步贵，但它是「渲染结果真的出来了」的唯一证据）
        let image = pollster::block_on(readback::read_texture_rgba8(
            &ctx.device, &ctx.queue, &render_target,
        ))?;

        // 4) 写进编码器
        let stdin = encoder.stdin.as_mut().ok_or("拿不到编码器的 stdin")?;
        stdin.write_all(&image.pixels)?;

        frames += 1;
    }
    drop(decoder_out);
    let _ = decoder.wait();
    // **关掉 stdin 编码器才知道流结束了** —— 不关它会一直等。
    drop(encoder.stdin.take());
    let _ = encoder.wait();
    let elapsed = started.elapsed();

    // 核对产物
    let check = Command::new("ffprobe")
        .args([
            "-v", "error", "-select_streams", "v:0",
            "-count_frames", "-show_entries", "stream=nb_read_frames,width,height",
            "-of", "csv=p=0", out_path,
        ])
        .output()?;
    let summary = String::from_utf8(check.stdout)?;
    let parts: Vec<&str> = summary.trim().split(',').collect();
    if parts.len() != 3 {
        return Err(format!("ffprobe 字段数不是 3，而是 {} —— 解析前提不成立", parts.len()).into());
    }
    // **同样是 width,height,nb_read_frames 的列序**：帧数在第三列。
    let encoded: usize = parts[2].parse()?;

    println!("媒体：{media}  {width}x{height} @{source_fps}fps");
    println!("输出帧率 {encoder_fps}（来源：{}）",
        if project.is_some() { "工程 timebase" } else { "源视频" });
    println!("处理 {frames} 帧，耗时 {:.0} ms -> 每帧 {:.2} ms（解码+上传+渲染+读回+编码）",
        elapsed.as_millis() as f64, elapsed.as_secs_f64() * 1000.0 / frames.max(1) as f64);
    println!("产物：{out_path}  编码帧数 {encoded}  尺寸 {}x{}", parts[0], parts[1]);

    if frames == 0 {
        return Err("一帧都没处理".into());
    }
    if encoded != frames {
        return Err(format!("编码出 {encoded} 帧，而处理了 {frames} 帧").into());
    }
    println!("✓ 完整顺序解码管道跑通（解码 -> 上传 -> 渲染 -> 读回 -> 编码）");
    Ok(())
}
