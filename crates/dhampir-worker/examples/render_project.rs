//! 样本工程的 native 渲染：出 PNG + 一份摘要清单。
//!
//! 跑：
//!   cargo run -q -p dhampir-worker --example render_project -- \
//!     fixtures/sample-project.json target/s4/worker 0 15 30 45 75
//!
//! # 这是双端比对的 native 一半
//!
//! 源图由 `synthetic_source_rgba8` + `synthetic_seed_for_source` 生成——**两端调同一个函数**。
//! 所以两边的输入像素逐字节相同，比出来的差异只可能来自**渲染与运行时**。
//! 这就是 `--dump-raw` 想达到的效果：先让输入相同，差异才有归因价值。
//!
//! 本仓库没有原生解码器（dhampir-media 是零实现的契约层），所以"含解码"那半属下游；
//! 这一层能保证的是：**给定同样的像素，两端的渲染图一致到什么程度**。

use std::collections::HashMap;
use std::path::PathBuf;

use dhampir_core::compose;
use dhampir_core::effects::REGISTRY;
use dhampir_core::gpu::NATIVE_BACKENDS;
use dhampir_core::readback;
use dhampir_core::render::{
    RenderSpace, SourceResolver, TimelineRenderer, synthetic_seed_for_source_frame,
    synthetic_source_rgba8,
};
use dhampir_core::timeline::schema::{Project, validate_project_with_effects};
use dhampir_core::wgpu;
use dhampir_worker::baseline::open_leg;

const WIDTH: u32 = 320;
const HEIGHT: u32 = 180;
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// 按源名缓存合成出来的源纹理。
///
/// 缓存是必要的：同一个 source 在一个工程里会被多个片段引用，
/// 每帧重建会让"两端的差异"混进分配顺序的差异里。
struct CachedSources<'a> {
    device: &'a wgpu::Device,
    queue: &'a wgpu::Queue,
    // 键要带上帧号：同一素材的不同帧是**不同的**源纹理。
    cache: HashMap<(String, i64), (wgpu::Texture, wgpu::TextureView)>,
}

impl<'a> CachedSources<'a> {
    fn new(device: &'a wgpu::Device, queue: &'a wgpu::Queue) -> Self {
        Self { device, queue, cache: HashMap::new() }
    }
}

impl SourceResolver for CachedSources<'_> {
    fn texture_for(
        &mut self,
        source: &str,
        source_frame: i64,
    ) -> Option<(wgpu::TextureView, (u32, u32))> {
        let key = (source.to_string(), source_frame);
        if !self.cache.contains_key(&key) {
            // 帧号参与 seed：同一素材的每一帧都不同，
            // 「帧号精确」这件事才在比对里被真正压到。
            let pixels = synthetic_source_rgba8(
                WIDTH,
                HEIGHT,
                synthetic_seed_for_source_frame(source, source_frame),
            );
            let texture = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("dhampir sample source"),
                size: wgpu::Extent3d {
                    width: WIDTH,
                    height: HEIGHT,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: FORMAT,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            self.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &pixels,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(WIDTH * 4),
                    rows_per_image: Some(HEIGHT),
                },
                wgpu::Extent3d { width: WIDTH, height: HEIGHT, depth_or_array_layers: 1 },
            );
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            self.cache.insert(key.clone(), (texture, view));
        }
        self.cache
            .get(&key)
            .map(|(_, view)| (view.clone(), (WIDTH, HEIGHT)))
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let project_path = args
        .first()
        .ok_or("用法：render_project <project.json> <out-dir> [帧号...]")?;
    let out_dir = args
        .get(1)
        .ok_or("用法：render_project <project.json> <out-dir> [帧号...]")?;
    let frames: Vec<i64> = if args.len() > 2 {
        args[2..].iter().filter_map(|text| text.parse().ok()).collect()
    } else {
        vec![0, 15, 30, 45, 75]
    };

    let text = std::fs::read_to_string(project_path)?;
    let project: Project = serde_json::from_str(&text)?;
    let issues = validate_project_with_effects(&project, REGISTRY);
    if !issues.is_empty() {
        eprintln!("工程没通过校验：{issues:#?}");
        std::process::exit(2);
    }

    let (ctx, _init) = open_leg(NATIVE_BACKENDS)?;
    let out_dir = PathBuf::from(out_dir);
    std::fs::create_dir_all(&out_dir)?;

    let renderer = TimelineRenderer::new(&ctx.device, FORMAT);
    let mut resolver = CachedSources::new(&ctx.device, &ctx.queue);
    let mut rows: Vec<String> = Vec::new();

    for frame in frames {
        let composite = compose::evaluate(&project, frame);
        let target = ctx.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("dhampir sample target"),
            size: wgpu::Extent3d { width: WIDTH, height: HEIGHT, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let target_view = target.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        let drawn = renderer.render_frame(
            &ctx.device,
            &ctx.queue,
            &mut encoder,
            &target_view,
            RenderSpace::square((WIDTH, HEIGHT)),
            &composite,
            &mut resolver,
            wgpu::Color::TRANSPARENT,
        );
        ctx.queue.submit([encoder.finish()]);

        let image = pollster::block_on(readback::read_texture_rgba8(
            &ctx.device,
            &ctx.queue,
            &target,
        ))?;
        let path = out_dir.join(format!("frame-{frame:04}.png"));
        image.write_png(&path)?;
        let digest = dhampir_core::timeline::selfcheck::fnv1a64(&image.pixels);
        rows.push(format!(
            "    {{\"frame\": {frame}, \"layers\": {drawn}, \"bytes\": {}, \"digest\": \"{digest:016x}\"}}",
            image.pixels.len()
        ));
    }

    let manifest = format!(
        "{{\n  \"width\": {WIDTH},\n  \"height\": {HEIGHT},\n  \"frames\": [\n{}\n  ]\n}}",
        rows.join(",\n")
    );
    std::fs::write(out_dir.join("manifest.json"), format!("{manifest}\n"))?;
    println!("{manifest}");
    Ok(())
}
