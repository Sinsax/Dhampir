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
use dhampir_core::render::{RenderSpace, SourceResolver, TimelineRenderer, synthetic_source_rgba8};
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
    fn texture_for(
        &mut self,
        _source: &str,
        _source_frame: i64,
    ) -> Option<(wgpu::TextureView, (u32, u32))> {
        Some((self.view.clone(), self.size))
    }
}

fn blur_effect(radius: f32) -> Effect {
    let mut params = BTreeMap::new();
    params.insert("radius".to_string(), radius);
    Effect {
        kind: "gaussian_blur".to_string(),
        params,
        ..Default::default()
    }
}

fn layer(clip_id: &str, opacity: f32, scale: f32, effects: Vec<Effect>) -> Layer {
    Layer {
        clip_id: clip_id.to_string(),
        source: "synthetic".to_string(),
        source_frame: 0,
        opacity,
        transform: Transform {
            x: 0.0,
            y: 0.0,
            scale,
            rotation_deg: 0.0,
        },
        effects,
        frozen_for_transition: false,
        // 这个 fixture 测的是渲染，不是混合与调整图层 —— 用恒定默认值。
        blend: dhampir_core::timeline::layer::BlendMode::Normal,
        is_adjustment: false,
    }
}

/// 渲染一份 composite 并读回。
fn render(
    ctx: &dhampir_core::gpu::GpuContext,
    source: &wgpu::Texture,
    composite: &Composite,
) -> Vec<u8> {
    let target = ctx.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("dhampir timeline test target"),
        size: wgpu::Extent3d {
            width: SIZE,
            height: SIZE,
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
    let mut resolver = OneSource {
        view: source.create_view(&wgpu::TextureViewDescriptor::default()),
        size: (SIZE, SIZE),
    };
    let renderer = TimelineRenderer::new(&ctx.device, FORMAT);
    let mut encoder = ctx
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("dhampir timeline test encoder"),
        });
    renderer.render_frame(
        &ctx.device,
        &ctx.queue,
        &mut encoder,
        &target_view,
        RenderSpace::square((SIZE, SIZE)),
        composite,
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
    image.pixels
}

/// 与 make_source 同构，但**把 alpha 全部设成 255**。
///
/// 为什么需要它：synthetic_source_rgba8 造的源**故意是半透明的**
/// （alpha 在 200 与 255 之间变化，core 里还有一条测试断言「不透明度应当有变化」）。
/// 而「调整图层不影响上方」这条判据要求上方**真的铺满** ——
/// 混合用的是**源 alpha**，不是图层 opacity，所以半透明上层会让被模糊的下层透出来，
/// 判据必然失败。
///
/// **第 61 轮查了源的 alpha（timeline.rs:446）才确认这是判据的前提问题，不是实现问题。**
fn make_opaque_source(ctx: &dhampir_core::gpu::GpuContext) -> wgpu::Texture {
    let mut pixels = synthetic_source_rgba8(SIZE, SIZE, 7);
    for pixel in pixels.chunks_mut(4) {
        pixel[3] = 255;
    }
    let texture = ctx.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("dhampir timeline test opaque source"),
        size: wgpu::Extent3d {
            width: SIZE,
            height: SIZE,
            depth_or_array_layers: 1,
        },
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
        wgpu::Extent3d {
            width: SIZE,
            height: SIZE,
            depth_or_array_layers: 1,
        },
    );
    texture
}

fn make_source(ctx: &dhampir_core::gpu::GpuContext) -> wgpu::Texture {
    let pixels = synthetic_source_rgba8(SIZE, SIZE, 7);
    let texture = ctx.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("dhampir timeline test source"),
        size: wgpu::Extent3d {
            width: SIZE,
            height: SIZE,
            depth_or_array_layers: 1,
        },
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
        wgpu::Extent3d {
            width: SIZE,
            height: SIZE,
            depth_or_array_layers: 1,
        },
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
    assert_eq!(
        blurred_pixels, again,
        "同一帧渲染两次结果不同，渲染不是确定性的"
    );

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
        fn texture_for(
            &mut self,
            _source: &str,
            _source_frame: i64,
        ) -> Option<(wgpu::TextureView, (u32, u32))> {
            None
        }
    }
    let (ctx, _init) = open_leg(NATIVE_BACKENDS).expect("拿不到 GPU 上下文");
    let target = ctx.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("dhampir timeline test nothing"),
        size: wgpu::Extent3d {
            width: SIZE,
            height: SIZE,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let composite = Composite {
        frame: 0,
        layers: vec![layer("a", 1.0, 1.0, Vec::new())],
    };
    let renderer = TimelineRenderer::new(&ctx.device, FORMAT);
    let mut encoder = ctx
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    let drawn = renderer.render_frame(
        &ctx.device,
        &ctx.queue,
        &mut encoder,
        &target.create_view(&wgpu::TextureViewDescriptor::default()),
        RenderSpace::square((SIZE, SIZE)),
        &composite,
        &mut Nothing,
        wgpu::Color::TRANSPARENT,
    );
    assert_eq!(drawn, 0, "解析不出源就不该画任何一层");
    ctx.queue.submit([encoder.finish()]);
    // 不 panic、能提交，就算过：这条钉的是"少一层素材不该让整帧失败"。
}

#[test]
#[ignore = "需要真 GPU；跑：cargo test -p dhampir-worker --test timeline -- --ignored"]
fn 遇到调整图层时整帧不画而不是悄悄画错() {
    // **这条钉的是「明确失败优于静默降级」。**
    //
    // 调整图层要求「先合成一部分 -> 对结果跑特效 -> 再继续」，
    // 那需要中间纹理与多次 pass，而渲染器现在只有一次 pass。
    // 所以它必须**整帧不画并返回 0**，而不是把调整图层当普通层画上去 ——
    // 后者会得到一张「特效没生效、但看不出哪里不对」的图。
    //
    // 为什么现在才加：上一轮加了这条拒绝路径，**却没有任何用例覆盖它**
    // （既有 GPU 用例的图层清单里都没有调整图层），
    // 所以那条路径从未被真正执行过。写了不等于跑过。
    struct Nothing;
    impl SourceResolver for Nothing {
        fn texture_for(
            &mut self,
            _source: &str,
            _source_frame: i64,
        ) -> Option<(wgpu::TextureView, (u32, u32))> {
            None
        }
    }

    let (ctx, _init) = open_leg(NATIVE_BACKENDS).expect("拿不到 GPU 上下文");
    let target = ctx.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("dhampir timeline test adjustment"),
        size: wgpu::Extent3d {
            width: SIZE,
            height: SIZE,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });

    // 调整图层：没有素材、只有特效。这里用 is_adjustment 直接标出来。
    let mut adjustment = layer("adj", 1.0, 1.0, Vec::new());
    adjustment.is_adjustment = true;
    adjustment.source = String::new();

    let composite = Composite {
        frame: 0,
        layers: vec![adjustment],
    };
    let renderer = TimelineRenderer::new(&ctx.device, FORMAT);
    let mut encoder = ctx
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    let drawn = renderer.render_frame(
        &ctx.device,
        &ctx.queue,
        &mut encoder,
        &target.create_view(&wgpu::TextureViewDescriptor::default()),
        RenderSpace::square((SIZE, SIZE)),
        &composite,
        &mut Nothing,
        wgpu::Color::TRANSPARENT,
    );
    assert_eq!(
        drawn, 0,
        "分段合成还没实现，就该一帧都不画，而不是画出一张看不出错的图"
    );
    ctx.queue.submit([encoder.finish()]);
}

#[test]
#[ignore = "需要真 GPU；跑：cargo test -p dhampir-worker --test timeline -- --ignored"]
fn 调整图层模糊下方而不影响上方() {
    // **这就是调整图层的定义**：它影响「已经画上去的全部内容」，不影响画在它**之后**的。
    //
    // 判据分两半，缺一不可：
    //   * 下方**真的被改了** —— 否则调整图层根本没生效；
    //   * 上方**一个字都没变** —— 否则它影响的是全图，不是「下方」。
    //
    // 为什么必须两半都有：只测「结果变了」的话，一个把整幅图都模糊掉的错误实现也能通过。
    let (ctx, _init) = open_leg(NATIVE_BACKENDS).expect("拿不到 GPU 上下文");
    // **用不透明的源**：synthetic_source_rgba8 是故意半透明的（alpha 200/255），
    // 而「不影响上方」要求上方真的铺满 —— 混合用的是源 alpha，不是图层 opacity。
    let source = make_opaque_source(&ctx);

    // 调整图层：没有素材、只有特效。
    let mut adjustment = layer("adj", 1.0, 1.0, vec![blur_effect(6.0)]);
    adjustment.is_adjustment = true;
    adjustment.source = String::new();

    let bottom_only = Composite {
        frame: 0,
        layers: vec![layer("bottom", 1.0, 1.0, Vec::new())],
    };
    let adjusted = Composite {
        frame: 0,
        layers: vec![layer("bottom", 1.0, 1.0, Vec::new()), adjustment.clone()],
    };
    // 调整层之上再压一层**完全不透明、铺满**的层：它必须不受调整影响。
    let top = layer("top", 1.0, 1.0, Vec::new());
    let covered = Composite {
        frame: 0,
        layers: vec![
            layer("bottom", 1.0, 1.0, Vec::new()),
            adjustment,
            top.clone(),
        ],
    };
    let only_top = Composite {
        frame: 0,
        layers: vec![top.clone()],
    };

    let plain = render(&ctx, &source, &bottom_only);
    let blurred = render(&ctx, &source, &adjusted);
    let with_top = render(&ctx, &source, &covered);
    let without_adjustment_above = render(&ctx, &source, &only_top);

    // 1. **下方真的被改了。**
    assert_ne!(plain, blurred, "挂上调整图层后下方没变 —— 分段合成没生效");

    // **1.5 归因**：让「只有上层」也走**同一条分段路径**。
    //
    // 原判据拿 covered（分段路径）去比 only_top（**非**分段路径），
    // 两个数来自两条路 —— 差异可能只是那条搬运引入的，而不是模糊泄漏。
    // 这里用一个 radius 为 0 的调整层：它不会真的模糊，
    // 但会让计划里出现 Adjust 步，于是路径与 covered 一致。
    // **这一步是为了分清「实现错」还是「判据错」，不是为了把测试弄绿。**
    let mut noop_adjustment = layer("noop", 1.0, 1.0, vec![blur_effect(0.0)]);
    noop_adjustment.is_adjustment = true;
    noop_adjustment.source = String::new();
    let only_top_segmented = Composite {
        frame: 0,
        layers: vec![noop_adjustment, top.clone()],
    };
    let with_top_segmented = render(&ctx, &source, &only_top_segmented);
    assert_eq!(
        with_top, with_top_segmented,
        "两边都走分段路径后仍不同 —— 差异来自模糊泄漏到上层，是**实现**问题"
    );

    // 2. **上方一个字都没变。**
    //    第二层完全不透明且铺满，所以「底层经调整后 + 上层」应当与「只有上层」逐字节相同。
    //    如果实现把最终结果也模糊了，这两个就会不同 —— 这条断言正是为此而设。
    assert_eq!(
        with_top, without_adjustment_above,
        "调整图层影响了画在它上面的层 —— 那它就不是「影响下方」了"
    );
}

// ============================================================================
// Document space 的 ColorMask：`overlay` 与 `vignette`
// ============================================================================
//
// 为什么补这两条（2026-10-01，处理下游交接单 D1）：
// 下游报「`overlay` 调整层整层不出图」。在**本仓当前 HEAD 复现不出来** ——
// 用交接单那份 doc 出帧，带 overlay 与不带 overlay 的结果**不同**，且逐像素与解析值吻合。
// 但"复现不出来"不是结论：这一族（Document space + ColorMask）此前**没有任何用例**，
// 所以它到底跑没跑过，没人证过。这两条就是把它钉死：
//
//   * `overlay`：逐像素对**解析值** `mix(底, 渐变, amount)`，并单独钉住渐变两端
//     （angle=0 左=color_a、右=color_b；angle=45 左上=color_a、右下=color_b
//      —— 后者正是下游 §5.2 用来核对 `overlay.angle` 换算的那一条）；
//   * `vignette`：中心不动、角落压暗，同样对解析值。
//
// 判据是解析值而不是"与上一版图相同"：后者只能证明"没变"，证明不了"对"。

fn color_mask_effect(kind: &str, params: &[(&str, f32)]) -> Effect {
    let mut map = BTreeMap::new();
    for (key, value) in params {
        map.insert((*key).to_string(), *value);
    }
    Effect {
        kind: kind.to_string(),
        params: map,
        ..Default::default()
    }
}

/// 底图像素：与 `make_opaque_source` 用的是同一份序列（alpha 全 255，混合是全覆盖）。
fn opaque_base_pixels() -> Vec<u8> {
    let mut pixels = synthetic_source_rgba8(SIZE, SIZE, 7);
    for pixel in pixels.chunks_mut(4) {
        pixel[3] = 255;
    }
    pixels
}

fn pixel_at(pixels: &[u8], x: u32, y: u32) -> [f32; 3] {
    let i = ((y * SIZE + x) * 4) as usize;
    [pixels[i] as f32, pixels[i + 1] as f32, pixels[i + 2] as f32]
}

/// `color_mask.wgsl` 第 4 段（overlay）的数学，逐行抄成一份**可算的期望值**。
///
/// **单位**：`color_a/color_b` 是契约里的 0..1 浮点（`overlay` 的 `r/g/b` 就是这么写的），
/// 而读回来的 `base` 是 0..255 —— 这里必须显式 ×255 对齐，否则差的是 255 倍里的一截。
///
/// **分片着色器里 `position.xy` 是像素中心**（`(x+0.5, y+0.5)`），这里必须同口径 ——
/// 差半个像素会让 45° 那条对角判据刚好落在边界上。
///
/// `too_many_arguments`：这是测试里的**参考实现**，8 个参数直接对应着色器 uniform 的
/// 形态（base / 像素坐标 / shape / angle / amount / 两端颜色）。拆结构体只会让
/// "照着着色器读一遍"变得更难，而这函数存在的全部意义就是能对着读。
#[allow(clippy::too_many_arguments)]
fn overlay_expected(
    base: [f32; 3],
    x: u32,
    y: u32,
    shape: f32,
    angle_deg: f32,
    amount: f32,
    color_a: [f32; 3],
    color_b: [f32; 3],
) -> [f32; 3] {
    let size = SIZE as f32;
    let u = (x as f32 + 0.5) / size - 0.5;
    let v = (y as f32 + 0.5) / size - 0.5;
    // `shape=0`（纯色）⇒ `grad_t = 0` ⇒ 取 `color_a`。**这一项 2026-10-01 修过**：
    // 以前写成 `is_solid + …`，纯色会落到 `color_b`（`r2/g2/b2`），与 `OVERLAY` 的文档矛盾。
    let is_linear = if (0.5..1.5).contains(&shape) {
        1.0
    } else {
        0.0
    };
    let is_radial = if shape >= 1.5 { 1.0 } else { 0.0 };
    let angle = angle_deg.to_radians();
    let (dx, dy) = (angle.cos(), angle.sin());
    let linear_t = (u * dx + v * dy + 0.5).clamp(0.0, 1.0);
    let radial_t = ((u * u + v * v).sqrt() * 2.0).clamp(0.0, 1.0);
    let grad_t = (is_linear * linear_t + is_radial * radial_t).clamp(0.0, 1.0);
    let mut out = [0.0f32; 3];
    for c in 0..3 {
        let color = (color_a[c] + (color_b[c] - color_a[c]) * grad_t) * 255.0;
        out[c] = base[c] + (color - base[c]) * amount;
    }
    out
}

/// `color_mask.wgsl` 第 2 段（暗角）的数学。
fn vignette_expected(
    base: [f32; 3],
    x: u32,
    y: u32,
    amount: f32,
    radius: f32,
    softness: f32,
) -> [f32; 3] {
    let size = SIZE as f32;
    let u = (x as f32 + 0.5) / size - 0.5;
    let v = (y as f32 + 0.5) / size - 0.5;
    // 用真的 √2 常量，而不是手写的 `1.414_213_6`（那是它的近似值，
    // clippy 的 approx_constant 指出的就是这里）。着色器那边算的是同一件事。
    let dist = (u * u + v * v).sqrt() * std::f32::consts::SQRT_2;
    let edge = ((dist - radius) / softness.max(1e-4)).clamp(0.0, 1.0);
    let factor = 1.0 - edge * amount;
    [base[0] * factor, base[1] * factor, base[2] * factor]
}

fn assert_close(got: [f32; 3], want: [f32; 3], label: &str, tol: f32) {
    for c in 0..3 {
        assert!(
            (got[c] - want[c]).abs() <= tol,
            "{label}：通道 {c} 实得 {}，期望 {}（容差 {tol}）",
            got[c],
            want[c]
        );
    }
}

#[test]
#[ignore = "需要真 GPU；跑：cargo test -p dhampir-worker --test timeline -- --ignored"]
fn 调整图层的_overlay_逐像素对上解析渐变() {
    // 交接单 D1 的那一组参数（0..1 的浮点色，与 doc 里写的一致）。
    const AMOUNT: f32 = 0.16;
    const A: [f32; 3] = [0.0784, 0.0392, 0.1569]; // == (20, 10, 40) / 255
    const B: [f32; 3] = [0.1569, 0.0784, 0.2353]; // == (40, 20, 60) / 255

    let (ctx, _init) = open_leg(NATIVE_BACKENDS).expect("拿不到 GPU 上下文");
    let source = make_opaque_source(&ctx);
    let base = opaque_base_pixels();

    let bottom = layer("bottom", 1.0, 1.0, Vec::new());
    let plain = Composite {
        frame: 0,
        layers: vec![bottom.clone()],
    };
    let plain_pixels = render(&ctx, &source, &plain);

    for (label, angle) in [("angle=0", 0.0f32), ("angle=45", 45.0f32)] {
        let mut adjustment = layer(
            "adj",
            1.0,
            1.0,
            vec![color_mask_effect(
                "overlay",
                &[
                    ("amount", AMOUNT),
                    ("r", A[0]),
                    ("g", A[1]),
                    ("b", A[2]),
                    ("r2", B[0]),
                    ("g2", B[1]),
                    ("b2", B[2]),
                    ("shape", 1.0),
                    ("angle", angle),
                ],
            )],
        );
        adjustment.is_adjustment = true;
        adjustment.source = String::new();

        let with = render(
            &ctx,
            &source,
            &Composite {
                frame: 0,
                layers: vec![bottom.clone(), adjustment],
            },
        );
        assert_ne!(
            plain_pixels, with,
            "{label}：挂上 overlay 后画面没变 —— Document space 那一趟没跑到"
        );

        // 逐像素对解析值（每 3 个像素采一个，够密也够快）。
        for y in (0..SIZE).step_by(3) {
            for x in (0..SIZE).step_by(3) {
                let want = overlay_expected(pixel_at(&base, x, y), x, y, 1.0, angle, AMOUNT, A, B);
                assert_close(
                    pixel_at(&with, x, y),
                    want,
                    &format!("{label} 像素({x},{y})"),
                    2.0,
                );
            }
        }

        // 单独钉住方向（这两条是**契约**，不是公式的副产品）：
        let left = pixel_at(&with, 1, SIZE / 2);
        let right = pixel_at(&with, SIZE - 2, SIZE / 2);
        if angle == 0.0 {
            // 水平渐变：左右两端各自贴近 color_a / color_b。
            let base_left = pixel_at(&base, 1, SIZE / 2);
            let base_right = pixel_at(&base, SIZE - 2, SIZE / 2);
            for c in 0..3 {
                let want_l = base_left[c] + (A[c] * 255.0 - base_left[c]) * AMOUNT;
                let want_r = base_right[c] + (B[c] * 255.0 - base_right[c]) * AMOUNT;
                assert!(
                    (left[c] - want_l).abs() <= 2.0,
                    "angle=0 左端通道 {c} 不是 color_a 那一端"
                );
                assert!(
                    (right[c] - want_r).abs() <= 2.0,
                    "angle=0 右端通道 {c} 不是 color_b 那一端"
                );
            }
        } else {
            // 45°：按下游 §5.2 的换算，应当是**从左上到右下**。
            let tl = pixel_at(&with, 1, 1);
            let br = pixel_at(&with, SIZE - 2, SIZE - 2);
            let base_tl = pixel_at(&base, 1, 1);
            let base_br = pixel_at(&base, SIZE - 2, SIZE - 2);
            for c in 0..3 {
                let want_tl = base_tl[c] + (A[c] * 255.0 - base_tl[c]) * AMOUNT;
                let want_br = base_br[c] + (B[c] * 255.0 - base_br[c]) * AMOUNT;
                assert!(
                    (tl[c] - want_tl).abs() <= 2.0,
                    "angle=45 左上角通道 {c} 不是 color_a 那一端"
                );
                assert!(
                    (br[c] - want_br).abs() <= 2.0,
                    "angle=45 右下角通道 {c} 不是 color_b 那一端"
                );
            }
        }
    }
    // D4（2026-10-01 修）：`shape=0`（纯色）取的是 **`color_a`（`r/g/b`）**，不是 `color_b`。
    // 参数**故意让 r2≠r**：`r2` 缺省回落到 `r` 时，两种实现看不出区别 —— 那正是它活了这么久的原因。
    let mut solid = layer(
        "adj",
        1.0,
        1.0,
        vec![color_mask_effect(
            "overlay",
            &[
                ("amount", 1.0),
                ("r", 0.8),
                ("g", 0.0),
                ("b", 0.0),
                ("r2", 0.0),
                ("g2", 0.0),
                ("b2", 0.8),
                ("shape", 0.0),
                ("angle", 0.0),
            ],
        )],
    );
    solid.is_adjustment = true;
    solid.source = String::new();
    let solid_px = render(
        &ctx,
        &source,
        &Composite {
            frame: 0,
            layers: vec![bottom.clone(), solid],
        },
    );
    // amount = 1 ⇒ 整幅都该是 color_a = (0.8, 0, 0) × 255 = (204, 0, 0)
    let center = pixel_at(&solid_px, SIZE / 2, SIZE / 2);
    assert!(
        center[0] > 200.0 && center[2] < 5.0,
        "shape=0 应当取 color_a（r/g/b = 204,0,0），实得 {center:?} —— 若取到 color_b 就是 D4 复发"
    );
}

#[test]
#[ignore = "需要真 GPU；跑：cargo test -p dhampir-worker --test timeline -- --ignored"]
fn 调整图层的_vignette_中心不动而角落压暗() {
    const AMOUNT: f32 = 1.0;
    // 半径取得小：这样"中心不动、角落全黑"两半都落在可判的区间里。
    // （半径 1.0 时角落的归一化距离只有 0.707 ⇒ **按定义**本来就不该有暗角，
    //   拿那个参数测会得到一条恒真的假判据。）
    const RADIUS: f32 = 0.2;
    const SOFTNESS: f32 = 0.3;

    let (ctx, _init) = open_leg(NATIVE_BACKENDS).expect("拿不到 GPU 上下文");
    let source = make_opaque_source(&ctx);
    let base = opaque_base_pixels();

    let bottom = layer("bottom", 1.0, 1.0, Vec::new());
    let mut adjustment = layer(
        "adj",
        1.0,
        1.0,
        vec![color_mask_effect(
            "vignette",
            &[
                ("amount", AMOUNT),
                ("radius", RADIUS),
                ("softness", SOFTNESS),
            ],
        )],
    );
    adjustment.is_adjustment = true;
    adjustment.source = String::new();

    let plain = render(
        &ctx,
        &source,
        &Composite {
            frame: 0,
            layers: vec![bottom.clone()],
        },
    );
    let with = render(
        &ctx,
        &source,
        &Composite {
            frame: 0,
            layers: vec![bottom, adjustment],
        },
    );
    assert_ne!(
        plain, with,
        "挂上 vignette 后画面没变 —— Document space 那一趟没跑到"
    );

    for y in (0..SIZE).step_by(3) {
        for x in (0..SIZE).step_by(3) {
            let want = vignette_expected(pixel_at(&base, x, y), x, y, AMOUNT, RADIUS, SOFTNESS);
            assert_close(
                pixel_at(&with, x, y),
                want,
                &format!("vignette 像素({x},{y})"),
                2.0,
            );
        }
    }

    // 两半都钉住：只测"角落变暗"的话，一个把整幅图乘 0 的实现也能通过。
    let center = pixel_at(&with, SIZE / 2, SIZE / 2);
    let base_center = pixel_at(&base, SIZE / 2, SIZE / 2);
    assert_close(center, base_center, "中心不该被压暗", 2.0);
    let corner = pixel_at(&with, 0, 0);
    assert!(
        corner[0] + corner[1] + corner[2] < 3.0,
        "角落应当被压到接近全黑（amount=1 且角落已越过半径+软化带），实得 {corner:?}"
    );
}
