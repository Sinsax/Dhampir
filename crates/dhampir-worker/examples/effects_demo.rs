//! 「全部枚举效果」演示工程的 **native 渲染**：逐帧出 PNG（外加一份摘要）。
//!
//! 跑：
//!
//!   cargo run -q --release -p dhampir-worker --example effects_demo -- \
//!     out/effects-demo/effects-demo.doc.json target/effects-demo/native 0 860
//!
//! # 为什么不走 CLI 的 frame/render
//!
//! 那两条路用 `dhampir-media` 取素材，而本仓库**没有原生解码器**（解码属下游），
//! 于是含图片素材的工程在那里会**静默跳过每一层**（源解析不出来 -> 跳过该层），
//! 出一张全黑的图。这里改用 `dhampir_core::readback::decode_png` **自己解码**，
//! 于是 native 这一端能吃与 HTML / wasm 两端**完全相同**的素材 —— 三端可比的前提。
//!
//! 它也能走 `flatten_path`/掩码/投影/背景滤镜等全部路径：用的是与 CLI 同一个
//! `TimelineRenderer::render_frame_at`（分段渲染），不是简化版。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use dhampir_core::compose;
use dhampir_core::gpu::NATIVE_BACKENDS;
use dhampir_core::readback;
use dhampir_core::render::{RenderSpace, SourceResolver, TimelineRenderer};
use dhampir_core::timeline::layer::TimelineV2;
use dhampir_core::wgpu;
use dhampir_worker::baseline::open_leg;

const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// 素材表：asset id -> 磁盘上的 PNG。解码一次、上传一次，之后复用纹理。
struct AssetSources<'a> {
    device: &'a wgpu::Device,
    queue: &'a wgpu::Queue,
    /// asset id -> 绝对路径
    files: HashMap<String, PathBuf>,
    cache: HashMap<String, (wgpu::Texture, wgpu::TextureView, (u32, u32))>,
}

impl AssetSources<'_> {
    fn upload(&mut self, asset_id: &str) -> Option<(wgpu::TextureView, (u32, u32))> {
        if !self.cache.contains_key(asset_id) {
            let path = self.files.get(asset_id)?.clone();
            let bytes = std::fs::read(&path).ok()?;
            let image = readback::Rgba8Image::decode_png(&bytes).ok()?;
            let (width, height) = (image.width, image.height);
            let texture = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("dhampir demo asset"),
                size: wgpu::Extent3d {
                    width,
                    height,
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
                &image.pixels,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(width * 4),
                    rows_per_image: Some(height),
                },
                wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
            );
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            self.cache
                .insert(asset_id.to_string(), (texture, view, (width, height)));
        }
        self.cache
            .get(asset_id)
            .map(|(_, view, size)| (view.clone(), *size))
    }
}

impl SourceResolver for AssetSources<'_> {
    fn texture_for(
        &mut self,
        source: &str,
        _frame: i64,
    ) -> Option<(wgpu::TextureView, (u32, u32))> {
        self.upload(source)
    }

    /// 掩码素材也自己解码 —— 这条通路（`mask_texture_for`）在 CLI 里同样依赖上游解码。
    fn mask_texture_for(&mut self, source: &str) -> Option<(wgpu::TextureView, (u32, u32))> {
        self.upload(source)
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let doc_path = args
        .first()
        .ok_or("用法：effects_demo <doc.json> <out-dir> [from] [to]")?;
    let out_dir = args
        .get(1)
        .ok_or("用法：effects_demo <doc.json> <out-dir> [from] [to]")?;
    let from: i64 = args.get(2).and_then(|text| text.parse().ok()).unwrap_or(0);
    let to: i64 = args
        .get(3)
        .and_then(|text| text.parse().ok())
        .unwrap_or(from + 1);

    let doc_text = std::fs::read_to_string(doc_path)?;
    let doc: serde_json::Value = serde_json::from_str(&doc_text)?;
    // **v2 求值器**：v1 的那个（compose::evaluate）把 blend/corner/clip/mask/shadow 全写死成默认值，
    // 用它渲染 v4 工程只会得到一张空图（本会话踩过）。
    let timeline: TimelineV2 = serde_json::from_value(doc["timeline"].clone())?;
    let (width, height) = (
        doc["render_hints"]["width"].as_u64().unwrap_or(640) as u32,
        doc["render_hints"]["height"].as_u64().unwrap_or(360) as u32,
    );
    let base = Path::new(doc_path)
        .parent()
        .unwrap_or(Path::new("."))
        .to_path_buf();
    let mut files: HashMap<String, PathBuf> = HashMap::new();
    if let Some(assets) = doc["assets"].as_array() {
        for asset in assets {
            let (Some(id), Some(uri)) = (asset["id"].as_str(), asset["uri"].as_str()) else {
                continue;
            };
            files.insert(id.to_string(), base.join(uri));
        }
    }
    let (num, den) = (
        timeline.timebase.num.max(1) as f64,
        timeline.timebase.den.max(1) as f64,
    );

    let (ctx, _init) = open_leg(NATIVE_BACKENDS)?;
    let out_dir = PathBuf::from(out_dir);
    std::fs::create_dir_all(&out_dir)?;
    let renderer = TimelineRenderer::new(&ctx.device, FORMAT);
    let space = RenderSpace {
        sequence: (width, height),
        target: (width, height),
    };

    let mut rows: Vec<String> = Vec::new();
    let mut drawn_total = 0usize;
    for frame in from..to {
        let composite = compose::evaluate_v2(&timeline, frame);
        let target = ctx.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("dhampir demo target"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
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
        let mut resolver = AssetSources {
            device: &ctx.device,
            queue: &ctx.queue,
            files: files.clone(),
            cache: HashMap::new(),
        };
        let seconds = (frame as f64) * den / num;
        let drawn = renderer.render_frame_at(
            &ctx.device,
            &ctx.queue,
            &mut encoder,
            &target_view,
            space,
            &composite,
            &mut resolver,
            wgpu::Color::TRANSPARENT,
            seconds as f32,
        );
        ctx.queue.submit([encoder.finish()]);
        drawn_total += drawn;

        let image = pollster::block_on(readback::read_texture_rgba8(
            &ctx.device,
            &ctx.queue,
            &target,
        ))?;
        let path = out_dir.join(format!("frame-{frame:04}.png"));
        image.write_png(&path)?;
        let digest = dhampir_core::timeline::selfcheck::fnv1a64(&image.pixels);
        rows.push(format!(
            "{{\"frame\": {frame}, \"layers\": {drawn}, \"digest\": \"{digest:016x}\"}}"
        ));
    }
    let manifest = format!(
        "{{\n  \"width\": {width},\n  \"height\": {height},\n  \"from\": {from},\n  \"to\": {to},\n  \"drawn_total\": {drawn_total},\n  \"frames\": [\n{}\n  ]\n}}",
        rows.join(",\n")
    );
    std::fs::write(out_dir.join("manifest.json"), format!("{manifest}\n"))?;
    println!(
        "出了 {} 帧（{}x{}），累计绘制层次数 {drawn_total}，清单在 {}",
        to - from,
        width,
        height,
        out_dir.display()
    );
    Ok(())
}
