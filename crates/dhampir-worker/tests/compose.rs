//! 合成节点的真机验证（集成测试）。
//!
//! **为什么在 worker**：core 按设计不开任何 wgpu 后端 feature（开了会把 wasm32 打编译死），
//! 所以 core 里拿不到 adapter。要真机验证就得放到能建 Instance 的宿主里——native 宿主是 worker。
//!
//! **为什么默认 #[ignore]**：整套测试不该依赖一台有 GPU 的机器。
//! 要跑这一条：cargo test -p dhampir-worker --test compose -- --ignored

use dhampir_core::gpu::NATIVE_BACKENDS;
use dhampir_core::readback;
use dhampir_core::render::{Compositor, LayerDraw};
use dhampir_core::timeline::schema::Transform;
use dhampir_core::wgpu;
use dhampir_worker::baseline::open_leg;

/// 用 Rgba8Unorm（线性、不做 sRGB 转换）而不是 Srgb 变体：
/// 这条要证的混合是纯算术，任何色彩转换都会把结论变成「转换的往返是否精确」。
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

fn solid(device: &wgpu::Device, queue: &wgpu::Queue, size: u32, rgba: [u8; 4], label: &str) -> wgpu::Texture {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d { width: size, height: size, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let mut pixels = Vec::with_capacity((size * size * 4) as usize);
    for _ in 0..(size * size) {
        pixels.extend_from_slice(&rgba);
    }
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &pixels,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(size * 4),
            rows_per_image: Some(size),
        },
        wgpu::Extent3d { width: size, height: size, depth_or_array_layers: 1 },
    );
    texture
}

fn target(device: &wgpu::Device, size: u32) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("dhampir compose test target"),
        size: wgpu::Extent3d { width: size, height: size, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    })
}

fn pixel(image: &dhampir_core::readback::Rgba8Image, size: u32, x: u32, y: u32) -> [u8; 4] {
    let at = ((y * size + x) * 4) as usize;
    [image.pixels[at], image.pixels[at + 1], image.pixels[at + 2], image.pixels[at + 3]]
}

fn near(got: [u8; 4], want: [u8; 4], tolerance: i32) -> bool {
    got.iter()
        .zip(want.iter())
        .all(|(g, w)| (*g as i32 - *w as i32).abs() <= tolerance)
}

#[test]
#[ignore = "需要真 GPU；跑：cargo test -p dhampir-worker --test compose -- --ignored"]
fn 两层按不透明度叠加() {
    const SIZE: u32 = 8;
    let (ctx, _init) = open_leg(NATIVE_BACKENDS).expect("拿不到 GPU 上下文");

    let red = solid(&ctx.device, &ctx.queue, SIZE, [255, 0, 0, 255], "dhampir compose red");
    let blue = solid(&ctx.device, &ctx.queue, SIZE, [0, 0, 255, 255], "dhampir compose blue");
    let out = target(&ctx.device, SIZE);

    let renderer = Compositor::new(&ctx.device, FORMAT);
    let mut encoder = ctx.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("dhampir compose test encoder"),
    });
    let red_view = red.create_view(&wgpu::TextureViewDescriptor::default());
    let blue_view = blue.create_view(&wgpu::TextureViewDescriptor::default());
    renderer.compose(
        &ctx.device,
        &ctx.queue,
        &mut encoder,
        &out.create_view(&wgpu::TextureViewDescriptor::default()),
        (SIZE, SIZE),
        &[
            LayerDraw { view: &red_view, source_size: (SIZE, SIZE), transform: Transform::default(), opacity: 1.0, blend: dhampir_core::timeline::layer::BlendMode::Normal },
            LayerDraw { view: &blue_view, source_size: (SIZE, SIZE), transform: Transform::default(), opacity: 0.5, blend: dhampir_core::timeline::layer::BlendMode::Normal },
        ],
        Some(wgpu::Color::TRANSPARENT),
    );
    ctx.queue.submit([encoder.finish()]);

    let image = pollster::block_on(readback::read_texture_rgba8(&ctx.device, &ctx.queue, &out))
        .expect("读回失败");

    // 0.5 的蓝叠在不透明的红上：每个通道都应当是 0.5 * 255 ≈ 128。
    // 允许 ±2：GPU 的取整方式（round / round-to-even）不是规范保证的。
    let got = pixel(&image, SIZE, 4, 4);
    assert!(
        near(got, [128, 0, 128, 255], 2),
        "红上叠半透明蓝应当是 (128, 0, 128, 255)，得到 {got:?}"
    );
    // 整张图都应当一致——若只有部分像素对，说明变换/边界有 bug
    for y in 0..SIZE {
        for x in 0..SIZE {
            let got = pixel(&image, SIZE, x, y);
            assert!(
                near(got, [128, 0, 128, 255], 2),
                "({x},{y}) 是 {got:?}，与中心不一致"
            );
        }
    }
}

#[test]
#[ignore = "需要真 GPU；跑：cargo test -p dhampir-worker --test compose -- --ignored"]
fn 缩放把层缩到中心而四周保持背景() {
    const SIZE: u32 = 8;
    let (ctx, _init) = open_leg(NATIVE_BACKENDS).expect("拿不到 GPU 上下文");

    let red = solid(&ctx.device, &ctx.queue, SIZE, [255, 0, 0, 255], "dhampir compose red");
    let out = target(&ctx.device, SIZE);

    let renderer = Compositor::new(&ctx.device, FORMAT);
    let mut encoder = ctx.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("dhampir compose scale encoder"),
    });
    let red_view = red.create_view(&wgpu::TextureViewDescriptor::default());
    renderer.compose(
        &ctx.device,
        &ctx.queue,
        &mut encoder,
        &out.create_view(&wgpu::TextureViewDescriptor::default()),
        (SIZE, SIZE),
        &[LayerDraw {
            view: &red_view,
            source_size: (SIZE, SIZE),
            // 缩到一半：只盖住中心，四周应当留着背景（清屏色）
            transform: Transform { x: 0.0, y: 0.0, scale: 0.5, rotation_deg: 0.0 },
            opacity: 1.0,
            blend: dhampir_core::timeline::layer::BlendMode::Normal,
        }],
        Some(wgpu::Color::TRANSPARENT),
    );
    ctx.queue.submit([encoder.finish()]);

    let image = pollster::block_on(readback::read_texture_rgba8(&ctx.device, &ctx.queue, &out))
        .expect("读回失败");

    let center = pixel(&image, SIZE, 4, 4);
    assert!(near(center, [255, 0, 0, 255], 2), "中心应当是实心红，得到 {center:?}");

    // 四个角必须在层之外：源坐标越界 -> 全透明 -> 目标保持清屏色（全 0）。
    // 这条同时证明了「不用 discard」这条路是通的：透明片段确实没动目标。
    for (x, y) in [(0, 0), (SIZE - 1, 0), (0, SIZE - 1), (SIZE - 1, SIZE - 1)] {
        let got = pixel(&image, SIZE, x, y);
        assert!(
            near(got, [0, 0, 0, 0], 1),
            "角 ({x},{y}) 应当保持背景，得到 {got:?}"
        );
    }
}
