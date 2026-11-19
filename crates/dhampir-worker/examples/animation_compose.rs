//! 拿**真实素材**跑一遍"动图直接进合成"：解码 -> 上传 -> 合成 -> 读回。
//!
//! 验证的是那句话本身：动图的某一帧**直接作为源纹理进合成**，
//! 中间没有"分离为帧再逐张走宿主"那一步。
//!
//! 要跑：
//!   cargo run --release -p dhampir-worker --example animation_compose -- <gif...>

use std::path::PathBuf;

use dhampir_core::animation;
use dhampir_core::compose::{Composite, Layer};
use dhampir_core::gpu::NATIVE_BACKENDS;
use dhampir_core::readback;
use dhampir_core::render::{AnimationTextures, RenderSpace, TimelineRenderer};
use dhampir_core::timeline::layer::BlendMode;
use dhampir_core::timeline::schema::Transform;
use dhampir_core::wgpu;
use dhampir_worker::baseline::open_leg;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("用法：cargo run -p dhampir-worker --example animation_compose -- <gif>...");
        std::process::exit(2);
    }
    let (ctx, _init) = open_leg(NATIVE_BACKENDS).expect("拿不到 GPU 上下文");
    println!("后端：{:?}", ctx.adapter.get_info().backend);

    // ---- 解码 + 上传：这是"不再分离为帧"的那一步 ----
    let started = std::time::Instant::now();
    let mut cache = AnimationTextures::new(ctx.device.clone(), ctx.queue.clone(), 0);
    let mut first = None;
    for raw in &args {
        let path = PathBuf::from(raw);
        let name = path
            .file_stem()
            .map(|stem| stem.to_string_lossy().to_string())
            .unwrap_or_else(|| raw.clone());
        let bytes = std::fs::read(&path).expect("读不了素材");
        let animation = animation::decode(&bytes).expect("解不开");
        cache.upload(&name, &animation).expect("传不上 GPU");
        println!(
            "  {name}: {} 帧 {}x{} —— 解码并上传完成",
            animation.frame_count(),
            animation.width,
            animation.height
        );
        if first.is_none() {
            first = Some((name, animation));
        }
    }
    let elapsed = started.elapsed();
    println!(
        "四张贴纸解码 + 上传合计：{:.0} ms，显存 {:.1} MiB",
        elapsed.as_secs_f64() * 1000.0,
        cache.memory_bytes() as f64 / 1048576.0
    );

    let (name, animation) = first.expect("至少给一个素材");
    let (width, height) = (animation.width, animation.height);

    // ---- 逐帧合成：每一帧取不同的源内帧号，验证"帧号真的驱动画面" ----
    let target = ctx.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("dhampir animation compose target"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let target_view = target.create_view(&wgpu::TextureViewDescriptor::default());
    let renderer = TimelineRenderer::new(&ctx.device, wgpu::TextureFormat::Rgba8Unorm);

    let mut digests = Vec::new();
    let frames_to_probe: Vec<i64> = vec![0, 1, 10, (animation.frame_count() as i64) - 1];
    for source_frame in &frames_to_probe {
        let composite = Composite {
            frame: *source_frame,
            layers: vec![Layer {
                backdrop_effects: Vec::new(),
                clip_id: "sticker".to_string(),
                source: name.clone(),
                source_frame: *source_frame,
                opacity: 1.0,
                transform: Transform {
                    x: (width as f32) / 2.0,
                    y: (height as f32) / 2.0,
                    scale: 1.0,
                    rotation_deg: 0.0,
                },
                effects: Vec::new(),
                frozen_for_transition: false,
                blend: BlendMode::Normal,
                corner_radius: 0.0,
                clip: None,
                mask: None,
                shadow: None,
                is_adjustment: false,
            }],
        };
        let mut resolver = CacheResolver {
            cache: &cache,
            name: name.clone(),
        };
        let mut encoder = ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("dhampir animation compose encoder"),
            });
        renderer.render_frame(
            &ctx.device,
            &ctx.queue,
            &mut encoder,
            &target_view,
            RenderSpace::square((width, height)),
            &composite,
            &mut resolver,
            wgpu::Color::TRANSPARENT,
        );
        ctx.queue.submit([encoder.finish()]);
        let image = pollster::block_on(readback::read_texture_rgba8(
            &ctx.device,
            &ctx.queue,
            &target,
        ))
        .expect("读回失败");
        let mut hash = 1469598103934665603u64;
        for byte in &image.pixels {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(1099511628211);
        }
        // 非透明像素数：证明"画上去了"而不是一帧空的。
        let opaque = image.pixels.chunks(4).filter(|pixel| pixel[3] > 0).count();
        println!(
            "  源内帧 {source_frame:>3}：摘要 {hash:016x}  非透明像素 {opaque}/{}",
            width as usize * height as usize
        );
        digests.push((*source_frame, hash, opaque));
    }

    // ---- 判据 ----
    let mut unique: Vec<u64> = digests.iter().map(|row| row.1).collect();
    unique.sort();
    unique.dedup();
    println!();
    if digests.iter().all(|row| row.2 > 0) && unique.len() == digests.len() {
        println!(
            "✓ 动图直接进合成成立：{} 个源内帧各自合成出**不同**的画面，且每帧都有内容",
            digests.len()
        );
    } else if digests.iter().any(|row| row.2 == 0) {
        println!("✗ 有帧是空白的 —— 合成没拿到动图纹理");
    } else {
        println!("✗ 不同源内帧合成出了相同画面 —— 帧号没被用上");
    }
}

/// 只用动图缓存供帧的解析器 —— 正是两个宿主里那个"动图优先"的分支。
struct CacheResolver<'a> {
    cache: &'a AnimationTextures,
    name: String,
}

impl dhampir_core::render::SourceResolver for CacheResolver<'_> {
    fn texture_for(
        &mut self,
        source: &str,
        source_frame: i64,
    ) -> Option<(wgpu::TextureView, (u32, u32))> {
        assert_eq!(source, self.name, "合成问的源名与上传时的键对不上");
        self.cache.texture_for(source, source_frame)
    }
}
