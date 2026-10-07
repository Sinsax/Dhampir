//! 可分离高斯模糊的真机验证（集成测试）。
//!
//! 要跑：cargo test -p dhampir-worker --test blur -- --ignored

use dhampir_core::gpu::NATIVE_BACKENDS;
use dhampir_core::readback;
use dhampir_core::render::BlurRenderer;
use dhampir_core::wgpu;
use dhampir_worker::baseline::open_leg;

const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

fn texture(
    device: &wgpu::Device,
    size: u32,
    usage: wgpu::TextureUsages,
    label: &str,
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width: size,
            height: size,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage,
        view_formats: &[],
    })
}

fn write(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    size: u32,
    pixels: &[u8],
    label: &str,
) -> wgpu::Texture {
    let tex = texture(
        device,
        size,
        wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        label,
    );
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &tex,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        pixels,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(size * 4),
            rows_per_image: Some(size),
        },
        wgpu::Extent3d {
            width: size,
            height: size,
            depth_or_array_layers: 1,
        },
    );
    tex
}

fn pixel(image: &dhampir_core::readback::Rgba8Image, size: u32, x: u32, y: u32) -> [u8; 4] {
    let at = ((y * size + x) * 4) as usize;
    [
        image.pixels[at],
        image.pixels[at + 1],
        image.pixels[at + 2],
        image.pixels[at + 3],
    ]
}

/// 跑一遍两趟模糊并读回。
fn blur_once(
    ctx: &dhampir_core::gpu::GpuContext,
    size: u32,
    source: &wgpu::Texture,
    radius: u32,
) -> dhampir_core::readback::Rgba8Image {
    let intermediate = texture(
        &ctx.device,
        size,
        wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::RENDER_ATTACHMENT,
        "dhampir blur test intermediate",
    );
    let out = texture(
        &ctx.device,
        size,
        wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::RENDER_ATTACHMENT,
        "dhampir blur test out",
    );
    let renderer = BlurRenderer::new(&ctx.device, FORMAT);
    let mut encoder = ctx
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("dhampir blur test encoder"),
        });
    renderer.blur_separable(
        &ctx.device,
        &ctx.queue,
        &mut encoder,
        &source.create_view(&wgpu::TextureViewDescriptor::default()),
        &intermediate.create_view(&wgpu::TextureViewDescriptor::default()),
        &out.create_view(&wgpu::TextureViewDescriptor::default()),
        (size, size),
        radius,
    );
    ctx.queue.submit([encoder.finish()]);
    pollster::block_on(readback::read_texture_rgba8(&ctx.device, &ctx.queue, &out))
        .expect("读回失败")
}

#[test]
#[ignore = "需要真 GPU；跑：cargo test -p dhampir-worker --test blur -- --ignored"]
fn 纯白图模糊后仍然是纯白() {
    // 这条证的是**权重归一**与**夹边不改变亮度**。
    // 两者任一不成立，整张图就会变暗或变亮——而变暗的模糊看起来「也挺像模糊」，
    // 不做这条测试根本发现不了。
    const SIZE: u32 = 8;
    let (ctx, _init) = open_leg(NATIVE_BACKENDS).expect("拿不到 GPU 上下文");

    let mut white = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    for _ in 0..(SIZE * SIZE) {
        white.extend_from_slice(&[255, 255, 255, 255]);
    }
    let source = write(
        &ctx.device,
        &ctx.queue,
        SIZE,
        &white,
        "dhampir blur test white",
    );

    for radius in [1_u32, 3, 8] {
        let image = blur_once(&ctx, SIZE, &source, radius);
        for y in 0..SIZE {
            for x in 0..SIZE {
                let got = pixel(&image, SIZE, x, y);
                let drift = got.iter().map(|v| (*v as i32 - 255).abs()).max().unwrap();
                assert!(
                    drift <= 1,
                    "radius {radius} 时 ({x},{y}) 变成 {got:?}，偏离纯白 {drift}——权重没归一或夹边改了亮度"
                );
            }
        }
    }
}

#[test]
#[ignore = "需要真 GPU；跑：cargo test -p dhampir-worker --test blur -- --ignored"]
fn 孤立亮点会被摊开到邻域() {
    const SIZE: u32 = 9;
    let (ctx, _init) = open_leg(NATIVE_BACKENDS).expect("拿不到 GPU 上下文");

    let mut pixels = vec![0_u8; (SIZE * SIZE * 4) as usize];
    let center = SIZE / 2;
    let at = ((center * SIZE + center) * 4) as usize;
    pixels[at] = 255;
    pixels[at + 1] = 255;
    pixels[at + 2] = 255;
    pixels[at + 3] = 255;
    let source = write(
        &ctx.device,
        &ctx.queue,
        SIZE,
        &pixels,
        "dhampir blur test dot",
    );

    let image = blur_once(&ctx, SIZE, &source, 2);

    let middle = pixel(&image, SIZE, center, center);
    let neighbour = pixel(&image, SIZE, center + 1, center);
    let corner = pixel(&image, SIZE, 0, 0);

    assert!(neighbour[0] > 0, "紧邻的像素应当被摊到，得到 {neighbour:?}");
    assert!(
        middle[0] >= neighbour[0],
        "中心 ({}) 不该比邻域 ({}) 还暗",
        middle[0],
        neighbour[0]
    );
    assert!(
        middle[0] < 255,
        "中心应当被摊薄，仍然是 {} 说明根本没模糊",
        middle[0]
    );
    assert_eq!(
        corner[0], 0,
        "半径 2 够不到角落，角落应当还是 0，得到 {corner:?}"
    );
}
