//! 「同一个工程在两个目标尺寸下，画面里的落点一致」的真机验证（集成测试）。
//!
//! # 它补的是哪一半
//!
//! T1 的判据有两半：
//!   * **数学**：换算之后归一化落点与目标尺寸无关 —— 在 dhampir-core 的单元测试里
//!     （render/compose.rs），那里不需要 GPU，测得快也测得准；
//!   * **端到端**：真的渲一张图出来，量一量那个方块落在哪 —— 就是本文件。
//!
//! 只有前一半绿是不够的：矩阵算对了、纹理绑定或采样错了，一样会得到错的画面。
//! 但反过来也不能只靠这一半 —— 它需要真 GPU，所以默认 #[ignore]，
//! 于是它不能是「默认关卡」。两半各自钉住自己那一层，这是有意的分工。
//!
//! # 夹具为什么要一块有区分度的源
//!
//! 样本工程是一张满帧视频：不管位移怎么算，整帧都被盖住，包围盒永远是整张图，
//! **量不出位置**。所以这里现造一张「中间一块不透明、四周透明」的源 ——
//! 落点一变，包围盒就跟着变。
//!
//! 要跑这一条：
//!   cargo test -p dhampir-worker --test preview_parity -- --ignored

use dhampir_core::gpu::NATIVE_BACKENDS;
use dhampir_core::readback;
use dhampir_core::render::{Compositor, LayerDraw, RenderSpace};
use dhampir_core::timeline::schema::Transform;
use dhampir_core::wgpu;
use dhampir_worker::baseline::open_leg;

/// Rgba8Unorm：不经 sRGB 转换，量到的是算术本身。
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// **文档坐标系**（工程的 render_hints）。
const SEQUENCE: (u32, u32) = (640, 360);
/// 源的分辨率：比目标小，所以它在画面里是一块而不是一整张。
const SOURCE: (u32, u32) = (320, 180);
/// 源里那块不透明的方块（半开区间），居中放。
const BLOCK: (u32, u32, u32, u32) = (120, 70, 200, 110);
/// 这一层的位移（**文档像素**）。
const OFFSET: (f32, f32) = (160.0, 90.0);

/// 造一张「中间一块不透明、四周透明」的源纹理。
fn block_source(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    size: (u32, u32),
    block: (u32, u32, u32, u32),
) -> wgpu::Texture {
    let (width, height) = size;
    let mut pixels = vec![0_u8; (width * height * 4) as usize];
    for y in block.1..block.3 {
        for x in block.0..block.2 {
            let at = ((y * width + x) * 4) as usize;
            pixels[at] = 255;
            pixels[at + 3] = 255;
        }
    }
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("dhampir preview parity source"),
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
            bytes_per_row: Some(width * 4),
            rows_per_image: Some(height),
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    texture
}

/// 非透明像素的包围盒（闭区间）。**一个都没有时返回 None** ——
/// 「什么都没画出来」和「画在了别处」是两件事，不能都当成一个数。
fn alpha_bbox(pixels: &[u8], size: (u32, u32)) -> Option<(u32, u32, u32, u32)> {
    let (width, height) = size;
    let mut found: Option<(u32, u32, u32, u32)> = None;
    for y in 0..height {
        for x in 0..width {
            let alpha = pixels[((y * width + x) * 4 + 3) as usize];
            if alpha == 0 {
                continue;
            }
            found = Some(match found {
                None => (x, y, x, y),
                Some((x0, y0, x1, y1)) => (x0.min(x), y0.min(y), x1.max(x), y1.max(y)),
            });
        }
    }
    found
}

/// 量一次：给定目标尺寸与坐标系，返回包围盒中心的**归一化**落点。
fn 落点(
    ctx: &dhampir_core::gpu::GpuContext,
    target_size: (u32, u32),
    space: RenderSpace,
) -> (f32, f32) {
    let source = block_source(&ctx.device, &ctx.queue, SOURCE, BLOCK);
    let target = ctx.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("dhampir preview parity target"),
        size: wgpu::Extent3d {
            width: target_size.0,
            height: target_size.1,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });

    let renderer = Compositor::new(&ctx.device, FORMAT);
    let mut encoder = ctx
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("dhampir preview parity encoder"),
        });
    let source_view = source.create_view(&wgpu::TextureViewDescriptor::default());
    let transform = Transform {
        x: OFFSET.0,
        y: OFFSET.1,
        ..Transform::default()
    };
    renderer.compose(
        &ctx.device,
        &ctx.queue,
        &mut encoder,
        &target.create_view(&wgpu::TextureViewDescriptor::default()),
        space,
        &[LayerDraw {
            view: &source_view,
            source_size: SOURCE,
            transform,
            opacity: 1.0,
            blend: dhampir_core::timeline::layer::BlendMode::Normal,
        }],
        Some(wgpu::Color::TRANSPARENT),
    );
    ctx.queue.submit([encoder.finish()]);

    let image = pollster::block_on(readback::read_texture_rgba8(
        &ctx.device,
        &ctx.queue,
        &target,
    ))
    .expect("读回失败");
    let bbox = alpha_bbox(&image.pixels, target_size).unwrap_or_else(|| {
        panic!("目标 {target_size:?} 上一个非透明像素都没有 —— 那一层根本没画出来")
    });
    let centre_x = (bbox.0 + bbox.2) as f32 / 2.0;
    let centre_y = (bbox.1 + bbox.3) as f32 / 2.0;
    (
        centre_x / target_size.0 as f32,
        centre_y / target_size.1 as f32,
    )
}

/// **T1 的端到端判据**：同一份工程、同一个位移，换目标尺寸之后归一化落点不变。
#[test]
#[ignore = "需要真 GPU；跑：cargo test -p dhampir-worker --test preview_parity -- --ignored"]
fn 同一工程在不同目标尺寸下的落点一致() {
    let (ctx, _init) = open_leg(NATIVE_BACKENDS).expect("拿不到 GPU 上下文");

    // 期望落点：图层中心 = 目标中心 + 位移（换算成目标像素），
    // 归一化之后就是 0.5 + 位移/文档尺寸 —— **与目标尺寸无关**。
    let expected = (
        0.5 + OFFSET.0 / SEQUENCE.0 as f32,
        0.5 + OFFSET.1 / SEQUENCE.1 as f32,
    );

    let mut measured: Vec<((u32, u32), (f32, f32))> = Vec::new();
    for target_size in [(640_u32, 360_u32), (320, 180), (1280, 720)] {
        let space = RenderSpace {
            sequence: SEQUENCE,
            target: target_size,
        };
        let point = 落点(&ctx, target_size, space);
        assert!(
            (point.0 - expected.0).abs() < 0.02 && (point.1 - expected.1).abs() < 0.02,
            "目标 {target_size:?} 的归一化落点是 {point:?}，期望 {expected:?}"
        );
        if let Some((previous_size, previous)) = measured.first() {
            assert!(
                (point.0 - previous.0).abs() < 0.02 && (point.1 - previous.1).abs() < 0.02,
                "{previous_size:?} 的落点 {previous:?} 与 {target_size:?} 的 {point:?} 不一致"
            );
        }
        measured.push((target_size, point));
    }
}

/// **反向验证**：不把文档坐标系交下去（拿目标尺寸当坐标系），落点必须**明显不同**。
///
/// 没有这一条，上面那个测试可能是恒真的 —— 比如包围盒恰好与位移无关时。
#[test]
#[ignore = "需要真 GPU；跑：cargo test -p dhampir-worker --test preview_parity -- --ignored"]
fn 拿目标尺寸当坐标系时落点会明显不同() {
    let (ctx, _init) = open_leg(NATIVE_BACKENDS).expect("拿不到 GPU 上下文");
    const TARGET: (u32, u32) = (320, 180);

    let correct = 落点(
        &ctx,
        TARGET,
        RenderSpace {
            sequence: SEQUENCE,
            target: TARGET,
        },
    );
    // 修复前的行为：坐标系 == 目标尺寸。
    let broken = 落点(&ctx, TARGET, RenderSpace::square(TARGET));

    assert!(
        (correct.0 - broken.0).abs() > 0.1 || (correct.1 - broken.1).abs() > 0.1,
        "两种坐标系给出了同一个落点（正确 {correct:?}、错误 {broken:?}）—— \
         那说明这个夹具量不出位置，上面那条判据是空转的"
    );
}
