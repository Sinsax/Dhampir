//! 合成节点的真机验证（集成测试）。
//!
//! **为什么在 worker**：core 按设计不开任何 wgpu 后端 feature（开了会把 wasm32 打编译死），
//! 所以 core 里拿不到 adapter。要真机验证就得放到能建 Instance 的宿主里——native 宿主是 worker。
//!
//! **为什么默认 #[ignore]**：整套测试不该依赖一台有 GPU 的机器。
//! 要跑这一条：cargo test -p dhampir-worker --test compose -- --ignored

use dhampir_core::gpu::{GpuContext, NATIVE_BACKENDS};
use dhampir_core::readback;
use dhampir_core::render::{
    Compositor, InkBounds, LayerDraw, MaskInput, OverlayItem, RenderSpace, compose_overlay, ink_report,
};
use dhampir_core::timeline::schema::Transform;
use dhampir_core::timeline::text_layout::{LinePlacement, NormalizedRect, place_line};
use dhampir_core::wgpu;
use dhampir_worker::baseline::open_leg;

/// 用 Rgba8Unorm（线性、不做 sRGB 转换）而不是 Srgb 变体：
/// 这条要证的混合是纯算术，任何色彩转换都会把结论变成「转换的往返是否精确」。
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

fn solid(device: &wgpu::Device, queue: &wgpu::Queue, size: u32, rgba: [u8; 4], label: &str) -> wgpu::Texture {
    solid_rect(device, queue, size, size, rgba, label)
}

/// 非方形的一块实心纹理：文字位图就是这种形状（宽取整条目标宽、高只有几十像素）。
fn solid_rect(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    width: u32,
    height: u32,
    rgba: [u8; 4],
    label: &str,
) -> wgpu::Texture {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let mut pixels = Vec::with_capacity((width * height * 4) as usize);
    for _ in 0..(width * height) {
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
            bytes_per_row: Some(width * 4),
            rows_per_image: Some(height),
        },
        wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
    );
    texture
}

fn target(device: &wgpu::Device, size: u32) -> wgpu::Texture {
    target_rect(device, size, size)
}

fn target_rect(device: &wgpu::Device, width: u32, height: u32) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("dhampir compose test target"),
        size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
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
        RenderSpace::square((SIZE, SIZE)),
        &[
            LayerDraw { view: &red_view, source_size: (SIZE, SIZE), transform: Transform::default(), opacity: 1.0, blend: dhampir_core::timeline::layer::BlendMode::Normal, corner_radius: 0.0, clip: None, mask: None, tint: None, extra_offset: (0.0, 0.0) },
            LayerDraw { view: &blue_view, source_size: (SIZE, SIZE), transform: Transform::default(), opacity: 0.5, blend: dhampir_core::timeline::layer::BlendMode::Normal, corner_radius: 0.0, clip: None, mask: None, tint: None, extra_offset: (0.0, 0.0) },
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
        RenderSpace::square((SIZE, SIZE)),
        &[LayerDraw {
            view: &red_view,
            source_size: (SIZE, SIZE),
            // 缩到一半：只盖住中心，四周应当留着背景（清屏色）
            transform: Transform { x: 0.0, y: 0.0, scale: 0.5, rotation_deg: 0.0 },
            opacity: 1.0,
            blend: dhampir_core::timeline::layer::BlendMode::Normal,
            corner_radius: 0.0,
            clip: None,
            mask: None,
            tint: None,
            extra_offset: (0.0, 0.0),
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

// ---------------------------------------------------------------------------
// 文字叠加（T2.5）：core 的 render::overlay 原语在真 GPU 上的行为。
//
// 浏览器那半的验收走 `node scripts/web-check.mjs --verdict text`（canvas 栅格化 +
// 一次 GPU 叠加 + 读回像素）。这里证的是**叠加原语本身**、而且不需要浏览器：
// 落点与行盒逐个像素一致、空清单不改动一个字节、尺寸不符时拦住不画。
// ---------------------------------------------------------------------------

/// 文字叠加测试用的目标尺寸：**非方形**，而位图宽度取整条目标宽。
const TEXT_TARGET: (u32, u32) = (64, 32);
/// 字号（相对序列高的比例）。**布局算出来的事实**，落点这边只收着用 ——
/// 早先是拿行盒高除以常量反推的，行高可配之后就错了。
const TEXT_FONT_RATIO: f32 = 0.055;

/// 三条位置不同的行盒（归一化，文档坐标）。**故意重叠**：重叠处同时验「后一行盖住前一行」。
const TEXT_RECTS: [(f32, f32, f32, f32); 3] = [
    (0.0, 0.0, 1.0, 0.25),   // 贴顶、整条宽：落点 y 会是负数
    (0.5, 0.3, 0.25, 0.12),  // 中间偏右
    (0.25, 0.55, 0.5, 0.15), // 底部居中
];

const TEXT_COLORS: [[u8; 4]; 3] = [[255, 0, 0, 255], [0, 255, 0, 255], [0, 0, 255, 255]];

/// 这个目标像素在不在落点里（落点可以有一部分在画面外）。
fn inside(placed: LinePlacement, x: u32, y: u32) -> bool {
    let (px, py) = (x as i64, y as i64);
    px >= placed.x as i64
        && px < placed.x as i64 + placed.bitmap_width as i64
        && py >= placed.y as i64
        && py < placed.y as i64 + placed.bitmap_height as i64
}

/// 若干落点裁到画面内之后的并集（包围盒）。一个像素都不剩时给 `None`。
fn union_bounds(placements: &[LinePlacement], target: (u32, u32)) -> Option<InkBounds> {
    let (mut x0, mut y0, mut x1, mut y1) = (i64::MAX, i64::MAX, i64::MIN, i64::MIN);
    for placed in placements {
        let left = (placed.x as i64).max(0);
        let top = (placed.y as i64).max(0);
        let right = (placed.x as i64 + placed.bitmap_width as i64).min(target.0 as i64);
        let bottom = (placed.y as i64 + placed.bitmap_height as i64).min(target.1 as i64);
        if right <= left || bottom <= top {
            continue;
        }
        x0 = x0.min(left);
        y0 = y0.min(top);
        x1 = x1.max(right - 1);
        y1 = y1.max(bottom - 1);
    }
    if x0 > x1 || y0 > y1 {
        return None;
    }
    Some(InkBounds {
        x: x0 as u32,
        y: y0 as u32,
        width: (x1 - x0 + 1) as u32,
        height: (y1 - y0 + 1) as u32,
    })
}

#[test]
#[ignore = "需要真 GPU；跑：cargo test -p dhampir-worker --test compose -- --ignored"]
fn 文字的落点与行盒逐个像素一致() {
    let (ctx, _init) = open_leg(NATIVE_BACKENDS).expect("拿不到 GPU 上下文");
    let (width, height) = TEXT_TARGET;
    let out = target_rect(&ctx.device, width, height);
    let compositor = Compositor::new(&ctx.device, FORMAT);
    let target_view = out.create_view(&wgpu::TextureViewDescriptor::default());

    // 落点走**共享布局**，测试不自己算：算错了这里就对不上像素。
    let placements: Vec<LinePlacement> = TEXT_RECTS
        .iter()
        .map(|(x, y, box_width, box_height)| {
            let rect = NormalizedRect { x: *x, y: *y, width: *box_width, height: *box_height };
            place_line(rect, TEXT_TARGET, TEXT_FONT_RATIO).expect("行盒有高度，应当给得出落点")
        })
        .collect();
    // 位图**照落点声明的尺寸造**、整张填满：这一条要证的是落点，不是字形。
    let bitmaps: Vec<wgpu::Texture> = placements
        .iter()
        .zip(TEXT_COLORS)
        .enumerate()
        .map(|(index, (placed, color))| {
            solid_rect(
                &ctx.device,
                &ctx.queue,
                placed.bitmap_width,
                placed.bitmap_height,
                color,
                &format!("dhampir text bitmap {index}"),
            )
        })
        .collect();
    let views: Vec<wgpu::TextureView> = bitmaps
        .iter()
        .map(|texture| texture.create_view(&wgpu::TextureViewDescriptor::default()))
        .collect();
    let items: Vec<OverlayItem<'_>> = views
        .iter()
        .zip(&placements)
        .map(|(view, placed)| OverlayItem {
            view,
            bitmap_size: (placed.bitmap_width, placed.bitmap_height),
            placement: *placed,
        })
        .collect();

    let mut encoder = ctx.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("dhampir text overlay encoder"),
    });
    // 先按常规路径建立底（清屏），再叠文字 —— 这正是宿主那两行的顺序。
    compositor.compose(
        &ctx.device,
        &ctx.queue,
        &mut encoder,
        &target_view,
        RenderSpace::square(TEXT_TARGET),
        &[],
        Some(wgpu::Color::TRANSPARENT),
    );
    let report = compose_overlay(
        &compositor,
        &ctx.device,
        &ctx.queue,
        &mut encoder,
        &target_view,
        TEXT_TARGET,
        &items,
    );
    assert_eq!(report.drawn, items.len(), "三行都该画上去");
    assert_eq!(report.size_mismatch, 0, "位图尺寸与落点必须一一对应");
    ctx.queue.submit([encoder.finish()]);

    let image = pollster::block_on(readback::read_texture_rgba8(&ctx.device, &ctx.queue, &out))
        .expect("读回失败");

    // 逐个像素对照：**后一行盖住前一行**（清单顺序即叠放顺序，与合成器一致）。
    for y in 0..height {
        for x in 0..width {
            let mut expected = [0, 0, 0, 0];
            for (placed, color) in placements.iter().zip(TEXT_COLORS) {
                if inside(*placed, x, y) {
                    expected = color;
                }
            }
            let got = pixel(&image, width, x, y);
            assert!(near(got, expected, 2), "({x},{y}) 应当是 {expected:?}，得到 {got:?}");
        }
    }

    // 再数一遍墨迹：包围盒必须**恰好**是落点的并集 —— 宿主判的就是这个数。
    let empty = dhampir_core::readback::Rgba8Image {
        width,
        height,
        pixels: vec![0; (width * height * 4) as usize],
    };
    let ink = ink_report(&empty, &image).expect("两张同尺寸");
    assert_eq!(ink.bounds, union_bounds(&placements, TEXT_TARGET), "墨迹包围盒与落点对不上");
}

#[test]
#[ignore = "需要真 GPU；跑：cargo test -p dhampir-worker --test compose -- --ignored"]
fn 没有可叠的行时目标一个字节都不改() {
    // 「无文字工程逐字节不变」在渲染侧的写法：没有字要画时，叠加这一步
    // **不进任何绘制命令**（也不清屏）。空目标上的「没变」没有意义，所以先铺一层底。
    let (ctx, _init) = open_leg(NATIVE_BACKENDS).expect("拿不到 GPU 上下文");
    let (width, height) = TEXT_TARGET;
    let out = target_rect(&ctx.device, width, height);
    let compositor = Compositor::new(&ctx.device, FORMAT);
    let target_view = out.create_view(&wgpu::TextureViewDescriptor::default());

    let base = solid_rect(&ctx.device, &ctx.queue, width, height, [40, 80, 120, 255], "dhampir text base");
    let base_view = base.create_view(&wgpu::TextureViewDescriptor::default());
    let mut encoder = ctx.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("dhampir text base encoder"),
    });
    compositor.compose(
        &ctx.device,
        &ctx.queue,
        &mut encoder,
        &target_view,
        RenderSpace::square(TEXT_TARGET),
        &[LayerDraw {
            view: &base_view,
            source_size: TEXT_TARGET,
            transform: Transform::default(),
            opacity: 1.0,
            blend: dhampir_core::timeline::layer::BlendMode::Normal,
            corner_radius: 0.0,
            clip: None,
            mask: None,
            tint: None,
            extra_offset: (0.0, 0.0),
        }],
        Some(wgpu::Color::TRANSPARENT),
    );
    ctx.queue.submit([encoder.finish()]);
    let before = pollster::block_on(readback::read_texture_rgba8(&ctx.device, &ctx.queue, &out))
        .expect("读回失败");

    let mut encoder = ctx.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("dhampir text overlay empty encoder"),
    });
    let report = compose_overlay(
        &compositor,
        &ctx.device,
        &ctx.queue,
        &mut encoder,
        &target_view,
        TEXT_TARGET,
        &[],
    );
    assert!(report.is_silent(), "空清单不该画、也不该拦：{report:?}");
    ctx.queue.submit([encoder.finish()]);
    let after = pollster::block_on(readback::read_texture_rgba8(&ctx.device, &ctx.queue, &out))
        .expect("读回失败");

    let changed = before
        .pixels
        .iter()
        .zip(after.pixels.iter())
        .filter(|(a, b)| a != b)
        .count();
    assert_eq!(changed, 0, "空清单的叠加改动了目标纹理：{changed} 个字节");
}

#[test]
#[ignore = "需要真 GPU；跑：cargo test -p dhampir-worker --test compose -- --ignored"]
fn 位图尺寸与落点不符时一行都不画() {
    // 宿主把位图与落点配错了对，硬画上去就是一次静默缩放（字糊一点、位置还差不多），
    // 那种错只有把像素读回来比才发现。所以拦住、报数、不画。
    let (ctx, _init) = open_leg(NATIVE_BACKENDS).expect("拿不到 GPU 上下文");
    let (width, height) = TEXT_TARGET;
    let out = target_rect(&ctx.device, width, height);
    let compositor = Compositor::new(&ctx.device, FORMAT);
    let target_view = out.create_view(&wgpu::TextureViewDescriptor::default());

    let placed = place_line(
        NormalizedRect { x: 0.25, y: 0.6, width: 0.5, height: 0.15 },
        TEXT_TARGET,
        TEXT_FONT_RATIO,
    )
    .expect("行盒有高度");
    // 故意造一张比落点宽的位图，并**如实**报它的实际尺寸。
    let actual = (placed.bitmap_width + 2, placed.bitmap_height);
    let bitmap = solid_rect(
        &ctx.device,
        &ctx.queue,
        actual.0,
        actual.1,
        [255, 255, 255, 255],
        "dhampir text bitmap wrong size",
    );
    let bitmap_view = bitmap.create_view(&wgpu::TextureViewDescriptor::default());

    // 底铺满整张（清屏色与文字色不同）：否则「没画」会被「底色一样」蒙过去。
    let base = solid_rect(&ctx.device, &ctx.queue, width, height, [10, 20, 30, 255], "dhampir text base");
    let base_view = base.create_view(&wgpu::TextureViewDescriptor::default());
    let mut encoder = ctx.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("dhampir text base encoder"),
    });
    compositor.compose(
        &ctx.device,
        &ctx.queue,
        &mut encoder,
        &target_view,
        RenderSpace::square(TEXT_TARGET),
        &[LayerDraw {
            view: &base_view,
            source_size: TEXT_TARGET,
            transform: Transform::default(),
            opacity: 1.0,
            blend: dhampir_core::timeline::layer::BlendMode::Normal,
            corner_radius: 0.0,
            clip: None,
            mask: None,
            tint: None,
            extra_offset: (0.0, 0.0),
        }],
        Some(wgpu::Color::TRANSPARENT),
    );
    ctx.queue.submit([encoder.finish()]);
    let before = pollster::block_on(readback::read_texture_rgba8(&ctx.device, &ctx.queue, &out))
        .expect("读回失败");

    let mut encoder = ctx.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("dhampir text overlay mismatch encoder"),
    });
    let report = compose_overlay(
        &compositor,
        &ctx.device,
        &ctx.queue,
        &mut encoder,
        &target_view,
        TEXT_TARGET,
        &[OverlayItem { view: &bitmap_view, bitmap_size: actual, placement: placed }],
    );
    assert_eq!(report.drawn, 0, "尺寸不符的位图不许画");
    assert_eq!(report.size_mismatch, 1, "尺寸不符必须数出来");
    ctx.queue.submit([encoder.finish()]);
    let after = pollster::block_on(readback::read_texture_rgba8(&ctx.device, &ctx.queue, &out))
        .expect("读回失败");

    let changed = before
        .pixels
        .iter()
        .zip(after.pixels.iter())
        .filter(|(a, b)| a != b)
        .count();
    assert_eq!(changed, 0, "尺寸不符时一个字节都不该改：{changed} 个字节");
}

/// **圆角（D10）的第二条判据**：半径 > 0 时只切四个角，半径 0 时一个像素都不动。
///
/// 为什么值得一条 GPU 用例：圆角是**每像素**的事，而「看起来有圆角」什么都证明不了 ——
/// 那可能是把整层缩小了，也可能是把四条边都切了。这里逐点钉死：
/// 四角透明、四条边的中点仍是红的、中心仍是红的；半径 0 再来一遍（四角也必须红）。
#[test]
#[ignore = "需要真 GPU；跑：cargo test -p dhampir-worker --test compose -- --ignored"]
fn 圆角只切四个角_半径为零时一个像素都不动() {
    const SIZE: u32 = 32;
    let (ctx, _init) = open_leg(NATIVE_BACKENDS).expect("拿不到 GPU 上下文");
    let renderer = Compositor::new(&ctx.device, FORMAT);

    let render_once = |radius: f32| -> dhampir_core::readback::Rgba8Image {
        let red = solid(&ctx.device, &ctx.queue, SIZE, [255, 0, 0, 255], "dhampir corner red");
        let out = target(&ctx.device, SIZE);
        let red_view = red.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = ctx.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("dhampir corner test encoder"),
        });
        renderer.compose(
            &ctx.device,
            &ctx.queue,
            &mut encoder,
            &out.create_view(&wgpu::TextureViewDescriptor::default()),
            RenderSpace::square((SIZE, SIZE)),
            &[LayerDraw {
                view: &red_view,
                source_size: (SIZE, SIZE),
                transform: Transform::default(),
                opacity: 1.0,
                blend: dhampir_core::timeline::layer::BlendMode::Normal,
                corner_radius: radius,
                clip: None,
                mask: None,
                tint: None,
                extra_offset: (0.0, 0.0),
            }],
            Some(wgpu::Color::TRANSPARENT),
        );
        ctx.queue.submit([encoder.finish()]);
        pollster::block_on(readback::read_texture_rgba8(&ctx.device, &ctx.queue, &out)).expect("读回失败")
    };

    let corners = [(0, 0), (SIZE - 1, 0), (0, SIZE - 1), (SIZE - 1, SIZE - 1)];

    // 半径 0：整层都是红的（这条钉的是「圆角只在半径 > 0 时才动手」）。
    let flat = render_once(0.0);
    for (x, y) in corners {
        let got = pixel(&flat, SIZE, x, y);
        assert!(near(got, [255, 0, 0, 255], 1), "半径 0 时 ({x},{y}) 应当是红的，得到 {got:?}");
    }

    // 半径 = SIZE/4：四角透明，边中点与中心照旧。
    let rounded = render_once((SIZE / 4) as f32);
    for (x, y) in corners {
        let got = pixel(&rounded, SIZE, x, y);
        assert!(got[3] <= 8, "圆角时 ({x},{y}) 应当基本透明，得到 {got:?}");
    }
    let untouched = [
        (SIZE / 2, 0),
        (SIZE / 2, SIZE - 1),
        (0, SIZE / 2),
        (SIZE - 1, SIZE / 2),
        (SIZE / 2, SIZE / 2),
    ];
    for (x, y) in untouched {
        let got = pixel(&rounded, SIZE, x, y);
        assert!(near(got, [255, 0, 0, 255], 2), "圆角不该动 ({x},{y})，得到 {got:?}");
    }
}

/// **裁剪形状（mark 1.6）的 GPU 判据**：圆 / 椭圆 / 内缩矩形各自切对地方，且形状互相可区分。
///
/// 三条断言都有**判别力**，不是"看起来对"：
///   · 圆 → 四条边的中点也透明（圆角只会切四个角，这条就是它俩的判别点）；
///   · 椭圆 → 在 y 轴上取 (rx+ry)/2 那个点**必须透明** —— 若把椭圆画成半径 rx 的圆，它会红；
///   · 内缩矩形 → 边界上一圈透明、往里 5 像素处仍红（证明是"内缩"而不是"缩小"）。
#[test]
#[ignore = "需要真 GPU；跑：cargo test -p dhampir-worker --test compose -- --ignored"]
fn 裁剪形状_圆与椭圆与内缩矩形各切对地方() {
    const SIZE: u32 = 32;
    let (ctx, _init) = open_leg(NATIVE_BACKENDS).expect("拿不到 GPU 上下文");
    let renderer = Compositor::new(&ctx.device, FORMAT);

    let render_with = |clip: Option<dhampir_core::timeline::layer::ClipShape>| -> dhampir_core::readback::Rgba8Image {
        let red = solid(&ctx.device, &ctx.queue, SIZE, [255, 0, 0, 255], "dhampir clip red");
        let out = target(&ctx.device, SIZE);
        let red_view = red.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = ctx.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("dhampir clip test encoder"),
        });
        renderer.compose(
            &ctx.device,
            &ctx.queue,
            &mut encoder,
            &out.create_view(&wgpu::TextureViewDescriptor::default()),
            RenderSpace::square((SIZE, SIZE)),
            &[LayerDraw {
                view: &red_view,
                source_size: (SIZE, SIZE),
                transform: Transform::default(),
                opacity: 1.0,
                blend: dhampir_core::timeline::layer::BlendMode::Normal,
                corner_radius: 0.0,
                clip,
                mask: None,
                tint: None,
                extra_offset: (0.0, 0.0),
            }],
            Some(wgpu::Color::TRANSPARENT),
        );
        ctx.queue.submit([encoder.finish()]);
        pollster::block_on(readback::read_texture_rgba8(&ctx.device, &ctx.queue, &out)).expect("读回失败")
    };
    let red_at = |image: &dhampir_core::readback::Rgba8Image, x: u32, y: u32| near(pixel(image, SIZE, x, y), [255, 0, 0, 255], 2);
    let clear_at = |image: &dhampir_core::readback::Rgba8Image, x: u32, y: u32| pixel(image, SIZE, x, y)[3] <= 8;

    // ① 不裁：到处都是红的（这条钉的是"没有裁剪时那个覆盖度精确是 1"）。
    let none = render_with(None);
    for (x, y) in [(0, 0), (SIZE - 1, 0), (0, SIZE - 1), (SIZE - 1, SIZE - 1), (SIZE / 2, 0), (0, SIZE / 2)] {
        assert!(red_at(&none, x, y), "不裁时 ({x},{y}) 应当是红的，得到 {:?}", pixel(&none, SIZE, x, y));
    }

    // ② 圆：四角与**四条边中点**都透明（圆角只会切四角 —— 这就是判别点）。
    let circle = render_with(Some(dhampir_core::timeline::layer::ClipShape::Circle {
        radius: 10.0,
        center: None,
    }));
    for (x, y) in [(0, 0), (SIZE - 1, 0), (0, SIZE - 1), (SIZE - 1, SIZE - 1), (SIZE / 2, 0), (SIZE / 2, SIZE - 1), (0, SIZE / 2), (SIZE - 1, SIZE / 2)] {
        assert!(clear_at(&circle, x, y), "圆外 ({x},{y}) 应当透明，得到 {:?}", pixel(&circle, SIZE, x, y));
    }
    assert!(red_at(&circle, SIZE / 2, SIZE / 2), "圆心必须是红的");
    assert!(red_at(&circle, 8, 16), "半径内 2 像素处应当是红的");

    // ③ 椭圆 rx=10 ry=6：y 轴 (rx+ry)/2 = 8 处**必须透明** —— 画成半径 10 的圆的话这里会是红的。
    let ellipse = render_with(Some(dhampir_core::timeline::layer::ClipShape::Ellipse {
        radius_x: 10.0,
        radius_y: 6.0,
        center: None,
    }));
    assert!(clear_at(&ellipse, SIZE / 2, SIZE / 2 + 8), "椭圆在 y 轴 8 像素处应当透明（判别点）");
    assert!(red_at(&ellipse, SIZE / 2, SIZE / 2 + 4), "椭圆内 y 轴 4 像素处应当是红的");
    assert!(red_at(&ellipse, SIZE / 2 + 8, SIZE / 2), "椭圆内 x 轴 8 像素处应当是红的");
    assert!(clear_at(&ellipse, SIZE / 2 + 12, SIZE / 2), "椭圆外 x 轴 12 像素处应当透明");

    // ④ 内缩 4 像素：边界一圈透明，往里 5 像素处仍红。
    let inset = render_with(Some(dhampir_core::timeline::layer::ClipShape::Inset {
        top: 4.0,
        right: 4.0,
        bottom: 4.0,
        left: 4.0,
        radius: 0.0,
    }));
    for (x, y) in [(1, 1), (SIZE / 2, 1), (1, SIZE / 2), (SIZE - 2, SIZE / 2)] {
        assert!(clear_at(&inset, x, y), "内缩矩形外 ({x},{y}) 应当透明，得到 {:?}", pixel(&inset, SIZE, x, y));
    }
    assert!(red_at(&inset, 5, 5), "内缩边界内应当是红的");
    assert!(red_at(&inset, SIZE / 2, SIZE / 2), "内缩矩形中心必须是红的");

    // ⑤ 中心是**归一化比例**：圆心挪到宽度的 1/4 处，半径 6 像素。
    //    判别力：原来的正中（16,16）距新圆心 8 像素 > 6 ⇒ 必须透明；
    //    若实现把 `center` 当成了"相对中心的像素偏移"（旧口径），这两条会正好反过来。
    let moved = render_with(Some(dhampir_core::timeline::layer::ClipShape::Circle {
        radius: 6.0,
        center: Some([0.25, 0.5]),
    }));
    assert!(red_at(&moved, SIZE / 4, SIZE / 2), "圆心应当在宽度的 1/4 处（红）");
    assert!(clear_at(&moved, SIZE / 2, SIZE / 2), "原来的正中应当变成圆外（透明）");
    assert!(clear_at(&moved, 0, 0), "左上角在圆外，应当透明");
}

/// **掩码（D12）的 GPU 判据**：2×2 的掩码铺在 32×32 的层上，四个象限各自对；反相与通道切换都要真的起作用。
///
/// 为什么用 2×2 铺 32×32：32 = 2×16，四个象限的**中心**（8,8）(24,8)… 正好落在四个纹素的**中心**上，
/// 双线性在那里取到的就是那个纹素本身 —— 于是断言是精确的，不掺插值误差。
#[test]
#[ignore = "需要真 GPU；跑：cargo test -p dhampir-worker --test compose -- --ignored"]
fn 掩码按图层矩形铺开_四象限与反相与通道都对() {
    const SIZE: u32 = 32;
    let (ctx, _init) = open_leg(NATIVE_BACKENDS).expect("拿不到 GPU 上下文");
    let renderer = Compositor::new(&ctx.device, FORMAT);

    // 2×2：左上、右下不透明；右上、左下透明 —— 用来测 **alpha 通道**。
    let alpha_mask = pixels_2x2(&ctx, [[255, 255, 255, 255], [0, 0, 0, 0], [0, 0, 0, 0], [255, 255, 255, 255]], "dhampir mask alpha");
    // 2×2：左上、右下是白；右上、左下是黑，alpha 全满 —— 用来测 **亮度通道**。
    let luma_mask = pixels_2x2(&ctx, [[255, 255, 255, 255], [0, 0, 0, 255], [0, 0, 0, 255], [255, 255, 255, 255]], "dhampir mask luma");

    let render_with = |mask: Option<MaskInput<'_>>| -> dhampir_core::readback::Rgba8Image {
        let red = solid(&ctx.device, &ctx.queue, SIZE, [255, 0, 0, 255], "dhampir mask red");
        let out = target(&ctx.device, SIZE);
        let red_view = red.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = ctx.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("dhampir mask test encoder"),
        });
        renderer.compose(
            &ctx.device,
            &ctx.queue,
            &mut encoder,
            &out.create_view(&wgpu::TextureViewDescriptor::default()),
            RenderSpace::square((SIZE, SIZE)),
            &[LayerDraw {
                view: &red_view,
                source_size: (SIZE, SIZE),
                transform: Transform::default(),
                opacity: 1.0,
                blend: dhampir_core::timeline::layer::BlendMode::Normal,
                corner_radius: 0.0,
                clip: None,
                mask,
                tint: None,
                extra_offset: (0.0, 0.0),
            }],
            Some(wgpu::Color::TRANSPARENT),
        );
        ctx.queue.submit([encoder.finish()]);
        pollster::block_on(readback::read_texture_rgba8(&ctx.device, &ctx.queue, &out)).expect("读回失败")
    };
    let alpha_at = |image: &dhampir_core::readback::Rgba8Image, x: u32, y: u32| pixel(image, SIZE, x, y)[3];
    // **两档而不是精确值**：2×2 铺 32×32 时没有哪个像素中心正好落在纹素中心上
    // （实测"留"的那两档是 240、"切"的是 15 —— 差的正是双线性在 3% 处的混合）。
    // 断言要求精确值会变成"测试双线性的小数位"，那是另一件事。
    let kept = |image: &dhampir_core::readback::Rgba8Image, x: u32, y: u32| alpha_at(image, x, y) >= 200;
    let cut = |image: &dhampir_core::readback::Rgba8Image, x: u32, y: u32| alpha_at(image, x, y) <= 40;

    // ① 不挂掩码：到处都不透明（兜底图 + `mask_a.x = 0` 的旁路必须精确）。
    let none = render_with(None);
    for (x, y) in [(8, 8), (24, 8), (8, 24), (24, 24)] {
        assert_eq!(alpha_at(&none, x, y), 255, "没挂掩码时 ({x},{y}) 不该被改");
    }

    // ② alpha 通道：不透明的两个象限留着，另外两个被切掉。
    let alpha_view = alpha_mask.create_view(&wgpu::TextureViewDescriptor::default());
    let alpha = render_with(Some(MaskInput {
        view: &alpha_view,
        channel: dhampir_core::timeline::layer::MaskChannel::Alpha,
        invert: false,
    }));
    assert!(kept(&alpha, 8, 8), "左上是掩码的不透明象限，得到 {}", alpha_at(&alpha, 8, 8));
    assert!(kept(&alpha, 24, 24), "右下是掩码的不透明象限，得到 {}", alpha_at(&alpha, 24, 24));
    assert!(cut(&alpha, 24, 8), "右上是掩码的透明象限，得到 {}", alpha_at(&alpha, 24, 8));
    assert!(cut(&alpha, 8, 24), "左下是掩码的透明象限，得到 {}", alpha_at(&alpha, 8, 24));

    // ③ 反相：正好反过来。
    let inverted = render_with(Some(MaskInput {
        view: &alpha_view,
        channel: dhampir_core::timeline::layer::MaskChannel::Alpha,
        invert: true,
    }));
    assert!(cut(&inverted, 8, 8), "反相后左上应当被切掉，得到 {}", alpha_at(&inverted, 8, 8));
    assert!(cut(&inverted, 24, 24), "反相后右下应当被切掉，得到 {}", alpha_at(&inverted, 24, 24));
    assert!(kept(&inverted, 24, 8), "反相后右上应当留下，得到 {}", alpha_at(&inverted, 24, 8));
    assert!(kept(&inverted, 8, 24), "反相后左下应当留下，得到 {}", alpha_at(&inverted, 8, 24));

    // ④ 通道切换：同一张图，按 **alpha** 看是"全留"，按**亮度**看是四个象限。
    let luma_view = luma_mask.create_view(&wgpu::TextureViewDescriptor::default());
    let as_alpha = render_with(Some(MaskInput {
        view: &luma_view,
        channel: dhampir_core::timeline::layer::MaskChannel::Alpha,
        invert: false,
    }));
    assert!(kept(&as_alpha, 24, 8) && alpha_at(&as_alpha, 24, 8) == 255, "这张图 alpha 全满 ⇒ 按 alpha 看应当一个像素都不切（精确 255）");
    let as_luma = render_with(Some(MaskInput {
        view: &luma_view,
        channel: dhampir_core::timeline::layer::MaskChannel::Luminance,
        invert: false,
    }));
    assert!(kept(&as_luma, 8, 8), "按亮度看：左上白 ⇒ 留下，得到 {}", alpha_at(&as_luma, 8, 8));
    assert!(cut(&as_luma, 24, 8), "按亮度看：右上黑 ⇒ 切掉，得到 {}", alpha_at(&as_luma, 24, 8));
}

/// 造一张 2×2 的纹理（行优先：`[左上, 右上, 左下, 右下]`）。
fn pixels_2x2(ctx: &GpuContext, texels: [[u8; 4]; 4], label: &str) -> wgpu::Texture {
    let texture = ctx.device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d { width: 2, height: 2, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let mut pixels = Vec::with_capacity(16);
    for texel in texels {
        pixels.extend_from_slice(&texel);
    }
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
            bytes_per_row: Some(8),
            rows_per_image: Some(2),
        },
        wgpu::Extent3d { width: 2, height: 2, depth_or_array_layers: 1 },
    );
    texture
}
