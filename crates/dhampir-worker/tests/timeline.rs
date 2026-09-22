//! 时间线渲染器的真机验证（集成测试）。
//!
//! **为什么在 worker**：core 按设计不开任何 wgpu 后端 feature（开了会把 wasm32 打编译死），
//! 所以 core 里拿不到 adapter。要真机验证就得放到能建 Instance 的宿主里。
//!
//! 要跑：cargo test -p dhampir-worker --test timeline -- --ignored

use std::collections::BTreeMap;

use dhampir_core::compose::{Composite, Layer};
use dhampir_core::gpu::NATIVE_BACKENDS;
use dhampir_core::readback;
use dhampir_core::render::{SourceResolver, TimelineRenderer, synthetic_source_rgba8};
use dhampir_core::timeline::schema::{Effect, Transform};
use dhampir_core::wgpu;
use dhampir_worker::baseline::open_leg;

const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
const SIZE: u32 = 32;

/// 只认一个源的解析器：这条测试要压的是**调度**，不是多源查找。
struct OneSource {
    view: wgpu::TextureView,
    size: (u32, u32),
}

impl SourceResolver for OneSource {
    fn texture_for(&mut self, _source: &str) -> Option<(wgpu::TextureView, (u32, u32))> {
        Some((self.view.clone(), self.size))
    }
}

fn blur_effect(radius: f32) -> Effect {
    let mut params = BTreeMap::new();
    params.insert("radius".to_string(), radius);
    Effect { kind: "gaussian_blur".to_string(), params }
}

fn layer(clip_id: &str, opacity: f32, scale: f32, effects: Vec<Effect>) -> Layer {
    Layer {
        clip_id: clip_id.to_string(),
        source: "synthetic".to_string(),
        source_frame: 0,
        opacity,
        transform: Transform { x: 0.0, y: 0.0, scale, rotation_deg: 0.0 },
        effects,
        frozen_for_transition: false,
    }
}

/// 渲染一份 composite 并读回。
fn render(ctx: &dhampir_core::gpu::GpuContext, source: &wgpu::Texture, composite: &Composite) -> Vec<u8> {
    let target = ctx.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("dhampir timeline test target"),
        size: wgpu::Extent3d { width: SIZE, height: SIZE, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let target_view = target.create_view(&wgpu::TextureViewDescriptor::default());
    let mut resolver = OneSource {
        view: source.create_view(&wgpu::TextureViewDescriptor::default()),
        size: (SIZE, SIZE),
    };
    let renderer = TimelineRenderer::new(&ctx.device, FORMAT);
    let mut encoder = ctx.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("dhampir timeline test encoder"),
    });
    renderer.render_frame(
        &ctx.device,
        &ctx.queue,
        &mut encoder,
        &target_view,
        (SIZE, SIZE),
        composite,
        &mut resolver,
        wgpu::Color::TRANSPARENT,
    );
    ctx.queue.submit([encoder.finish()]);
    let image = pollster::block_on(readback::read_texture_rgba8(&ctx.device, &ctx.queue, &target))
        .expect("读回失败");
    image.pixels
}

fn make_source(ctx: &dhampir_core::gpu::GpuContext) -> wgpu::Texture {
    let pixels = synthetic_source_rgba8(SIZE, SIZE, 7);
    let texture = ctx.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("dhampir timeline test source"),
        size: wgpu::Extent3d { width: SIZE, height: SIZE, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    ctx.queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &pixels,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(SIZE * 4),
            rows_per_image: Some(SIZE),
        },
        wgpu::Extent3d { width: SIZE, height: SIZE, depth_or_array_layers: 1 },
    );
    texture
}

#[test]
#[ignore = "需要真 GPU；跑：cargo test -p dhampir-worker --test timeline -- --ignored"]
fn 多轨合成加模糊能被调度起来() {
    let (ctx, _init) = open_leg(NATIVE_BACKENDS).expect("拿不到 GPU 上下文");
    let source = make_source(&ctx);

    // 两层：底层不透明、顶层半透明且缩小 —— 合成与变换都被压到；
    // 顶层再挂一个模糊，特效调度也被压到。
    let plain = Composite {
        frame: 0,
        layers: vec![
            layer("bottom", 1.0, 1.0, Vec::new()),
            layer("top", 0.5, 0.7, Vec::new()),
        ],
    };
    let blurred = Composite {
        frame: 0,
        layers: vec![
            layer("bottom", 1.0, 1.0, Vec::new()),
            layer("top", 0.5, 0.7, vec![blur_effect(3.0)]),
        ],
    };

    let plain_pixels = render(&ctx, &source, &plain);
    let blurred_pixels = render(&ctx, &source, &blurred);
    let again = render(&ctx, &source, &blurred);

    // 1. 确定性：同一份输入与 composite，两次必须逐字节相同。
    //    这是"两端一致"能成立的前提——本端都不自洽就没什么可比。
    assert_eq!(blurred_pixels, again, "同一帧渲染两次结果不同，渲染不是确定性的");

    // 2. 不退化：画面不能是一片纯色（纯色图什么结论都撑不起来）
    let first = &plain_pixels[..4];
    assert!(
        plain_pixels.chunks(4).any(|px| px != first),
        "合成结果是一片纯色，说明图层根本没画上去"
    );

    // 3. **模糊真的跑了**：挂上模糊与不挂，结果必须不同。
    //    这条才是"特效调度被接上了"的证据——只测"能出图"是测不出来的。
    assert_ne!(
        plain_pixels, blurred_pixels,
        "挂上 gaussian_blur 之后结果没变，说明特效没有被调度"
    );
}

#[test]
#[ignore = "需要真 GPU；跑：cargo test -p dhampir-worker --test timeline -- --ignored"]
fn 源解析不出来时跳过该层而不是整帧失败() {
    struct Nothing;
    impl SourceResolver for Nothing {
        fn texture_for(&mut self, _source: &str) -> Option<(wgpu::TextureView, (u32, u32))> {
            None
        }
    }
    let (ctx, _init) = open_leg(NATIVE_BACKENDS).expect("拿不到 GPU 上下文");
    let target = ctx.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("dhampir timeline test nothing"),
        size: wgpu::Extent3d { width: SIZE, height: SIZE, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let composite = Composite { frame: 0, layers: vec![layer("a", 1.0, 1.0, Vec::new())] };
    let renderer = TimelineRenderer::new(&ctx.device, FORMAT);
    let mut encoder = ctx.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    let drawn = renderer.render_frame(
        &ctx.device, &ctx.queue, &mut encoder,
        &target.create_view(&wgpu::TextureViewDescriptor::default()),
        (SIZE, SIZE), &composite, &mut Nothing, wgpu::Color::TRANSPARENT,
    );
    assert_eq!(drawn, 0, "解析不出源就不该画任何一层");
    ctx.queue.submit([encoder.finish()]);
    // 不 panic、能提交，就算过：这条钉的是"少一层素材不该让整帧失败"。
}
