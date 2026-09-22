//! 源帧 blit 的真机验证（集成测试）。
//!
//! **为什么这条测试在 worker 而不是 core**：core 按设计不开任何 wgpu 后端 feature
//! （开了会把 wasm32 编译打死），所以 core 里**拿不到 adapter**。要真机验证就得放到一个
//! 能建 Instance 的宿主里——native 宿主就是 worker。
//!
//! **为什么默认 #[ignore]**：整个测试套件不该依赖一台有 GPU 的机器。M2 的 native-tests
//! 判据跑的是 cargo test --workspace，那条在无 GPU 的机器上也该过。
//! 要跑这一条：cargo test -p dhampir-worker --test blit -- --ignored

use dhampir_core::gpu::NATIVE_BACKENDS;
use dhampir_core::readback;
use dhampir_core::render::BlitRenderer;
use dhampir_core::wgpu;
use dhampir_worker::baseline::open_leg;

/// 目标格式用 **Rgba8Unorm**（线性、不做 sRGB 转换）而不是 Srgb 变体：
/// 这条要证的是「逐纹素恒等搬运」，任何色彩转换都会把结论变成「转换的往返是否精确」。
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

fn make_texture(
    device: &wgpu::Device,
    size: u32,
    label: &str,
    usage: wgpu::TextureUsages,
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

#[test]
#[ignore = "需要真 GPU；跑：cargo test -p dhampir-worker --test blit -- --ignored"]
fn blit_is_identity_when_sizes_match() {
    const SIZE: u32 = 16;
    let (ctx, _init) = open_leg(NATIVE_BACKENDS).expect("拿不到 GPU 上下文");

    // 每个像素都不一样：图案有变化，「恒等」这件事才可证伪（全黑图也能「相等」）。
    let mut pattern = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    for y in 0..SIZE {
        for x in 0..SIZE {
            pattern.extend_from_slice(&[(x * 8) as u8, (y * 8) as u8, ((x ^ y) * 8) as u8, 255]);
        }
    }

    let source = make_texture(
        &ctx.device,
        SIZE,
        "dhampir blit test source",
        wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
    );
    ctx.queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &source,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &pattern,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(SIZE * 4),
            rows_per_image: Some(SIZE),
        },
        wgpu::Extent3d {
            width: SIZE,
            height: SIZE,
            depth_or_array_layers: 1,
        },
    );

    let sink = make_texture(
        &ctx.device,
        SIZE,
        "dhampir blit test sink",
        wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
    );

    let renderer = BlitRenderer::new(&ctx.device, FORMAT);
    let mut encoder = ctx
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("dhampir blit test encoder"),
        });
    renderer.render(
        &ctx.device,
        &mut encoder,
        &source.create_view(&wgpu::TextureViewDescriptor::default()),
        &sink.create_view(&wgpu::TextureViewDescriptor::default()),
    );
    ctx.queue.submit([encoder.finish()]);

    let image = pollster::block_on(readback::read_texture_rgba8(&ctx.device, &ctx.queue, &sink))
        .expect("读回失败");
    let first_mismatch = image
        .pixels
        .iter()
        .zip(pattern.iter())
        .position(|(got, want)| got != want);
    assert_eq!(first_mismatch, None, "blit 不是恒等搬运");
}
