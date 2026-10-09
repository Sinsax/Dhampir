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
use dhampir_core::timeline::layer::{MaskChannel, MaskSpec};
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
        backdrop_effects: Vec::new(),
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
        corner_radius: 0.0,
        clip: None,
        mask: None,
        shadow: None,
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

/// 提供源 + **可选掩码**的解析器：`mask = None` 就是"宿主还没接掩码通路"那种宿主。
struct SourceWithMask {
    view: wgpu::TextureView,
    size: (u32, u32),
    mask: Option<wgpu::TextureView>,
}

impl SourceResolver for SourceWithMask {
    fn texture_for(
        &mut self,
        _source: &str,
        _source_frame: i64,
    ) -> Option<(wgpu::TextureView, (u32, u32))> {
        Some((self.view.clone(), self.size))
    }

    fn mask_texture_for(&mut self, _source: &str) -> Option<(wgpu::TextureView, (u32, u32))> {
        self.mask.clone().map(|view| (view, (2, 2)))
    }
}

/// 造一张 2×2 的掩码（行优先：左上、右上、左下、右下）。
fn mask_2x2(ctx: &dhampir_core::gpu::GpuContext, texels: [[u8; 4]; 4]) -> wgpu::Texture {
    let texture = ctx.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("dhampir timeline test mask"),
        size: wgpu::Extent3d {
            width: 2,
            height: 2,
            depth_or_array_layers: 1,
        },
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
        wgpu::Extent3d {
            width: 2,
            height: 2,
            depth_or_array_layers: 1,
        },
    );
    texture
}

/// 造一张「左半不透明、右半全透明」的源。
///
/// 为什么要它：**整体不透明的源做模糊是恒等变换**（数学上就该如此）——
/// 拿它去验模糊只会得到"看不出变化"，那验不了任何东西。有 alpha 边界才有的可糊。
fn half_source(ctx: &dhampir_core::gpu::GpuContext) -> wgpu::Texture {
    let texture = ctx.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("dhampir timeline test half source"),
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
    let mut pixels = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    for _y in 0..SIZE {
        for x in 0..SIZE {
            // 左半：不透明的红；右半：全透明。
            pixels.extend_from_slice(if x < SIZE / 2 {
                &[255u8, 0, 0, 255]
            } else {
                &[0u8, 0, 0, 0]
            });
        }
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

/// 用给定的解析器渲一帧并读回像素。
fn render_with(
    ctx: &dhampir_core::gpu::GpuContext,
    resolver: &mut dyn SourceResolver,
    composite: &Composite,
) -> Vec<u8> {
    let target = ctx.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("dhampir timeline test mask target"),
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
    let renderer = TimelineRenderer::new(&ctx.device, FORMAT);
    let mut encoder = ctx
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("dhampir timeline test mask encoder"),
        });
    renderer.render_frame(
        &ctx.device,
        &ctx.queue,
        &mut encoder,
        &target_view,
        RenderSpace::square((SIZE, SIZE)),
        composite,
        resolver,
        wgpu::Color::TRANSPARENT,
    );
    ctx.queue.submit([encoder.finish()]);
    pollster::block_on(readback::read_texture_rgba8(
        &ctx.device,
        &ctx.queue,
        &target,
    ))
    .expect("读回失败")
    .pixels
}

/// **掩码的通路判据（D12 第 3.5 步）**：解析器给不出掩码时必须**拒绝整帧**，不许画成没掩码的样子。
///
/// 为什么这条必须真机：拒绝发生在**求值层之后的准备循环**里（那一层才看得见"这一层要不要画"），
/// 而那条路径只有在能拿到 GPU 上下文时才走得通。
///
/// 期望的是**响亮的 panic**：debug 构建里最后一道防线就是 `debug_assert`（release 里它退化成
/// "这一帧一个像素都不画"）。上游本该先报 —— 宿主的 `mask_texture_for` 返回 `None` 之前,
/// 它自己会记 `unknown_asset` / `source_decode_failed`（worker 就是这么做的）。
#[test]
#[should_panic(expected = "掩码素材解析不出来")]
#[ignore = "需要真 GPU；跑：cargo test -p dhampir-worker --test timeline -- --ignored"]
fn 掩码解析不出来时拒绝整帧而不是画成没掩码() {
    let (ctx, _init) = open_leg(NATIVE_BACKENDS).expect("拿不到 GPU 上下文");
    let source = make_opaque_source(&ctx);
    let source_view = source.create_view(&wgpu::TextureViewDescriptor::default());
    let masked = Composite {
        frame: 0,
        layers: vec![Layer {
            mask: Some(MaskSpec {
                gradient: None,
                asset_id: "m.png".to_string(),
                channel: MaskChannel::Alpha,
                invert: false,
            }),
            ..layer("with-mask", 1.0, 1.0, Vec::new())
        }],
    };

    // 宿主"还没接掩码通路"：一个像素都不该画。
    let mut no_mask = SourceWithMask {
        view: source_view.clone(),
        size: (SIZE, SIZE),
        mask: None,
    };
    let refused = render_with(&ctx, &mut no_mask, &masked);
    assert!(
        refused.iter().all(|byte| *byte == 0),
        "掩码解析不出来时必须拒绝整帧（一个像素都不画），却画出了东西",
    );
}

/// 掩码通路通了之后：**渲染路径上**四象限与反相都对（合成级那条 GPU 用例之外的第二道）。
#[test]
#[ignore = "需要真 GPU；跑：cargo test -p dhampir-worker --test timeline -- --ignored"]
fn 掩码从解析器来_渲染路径上四象限与反相都对() {
    let (ctx, _init) = open_leg(NATIVE_BACKENDS).expect("拿不到 GPU 上下文");
    let source = make_opaque_source(&ctx);
    let source_view = source.create_view(&wgpu::TextureViewDescriptor::default());
    let mask = mask_2x2(
        &ctx,
        [
            [255, 255, 255, 255],
            [0, 0, 0, 0],
            [0, 0, 0, 0],
            [255, 255, 255, 255],
        ],
    );
    let mask_view = mask.create_view(&wgpu::TextureViewDescriptor::default());

    let with = |invert: bool| Composite {
        frame: 0,
        layers: vec![Layer {
            mask: Some(MaskSpec {
                gradient: None,
                asset_id: "m.png".to_string(),
                channel: MaskChannel::Alpha,
                invert,
            }),
            ..layer("with-mask", 1.0, 1.0, Vec::new())
        }],
    };
    let alpha_at = |pixels: &[u8], x: u32, y: u32| pixels[((y * SIZE + x) * 4 + 3) as usize];

    let mut resolver = SourceWithMask {
        view: source_view.clone(),
        size: (SIZE, SIZE),
        mask: Some(mask_view.clone()),
    };
    let normal = render_with(&ctx, &mut resolver, &with(false));
    // 两档而不是精确值：2×2 铺 32 时没有像素中心落在纹素中心上（实测 240 / 15）。
    assert!(
        alpha_at(&normal, 8, 8) >= 200,
        "左上应当留下，得到 {}",
        alpha_at(&normal, 8, 8)
    );
    assert!(alpha_at(&normal, 24, 24) >= 200, "右下应当留下");
    assert!(
        alpha_at(&normal, 24, 8) <= 40,
        "右上应当被切掉，得到 {}",
        alpha_at(&normal, 24, 8)
    );
    assert!(alpha_at(&normal, 8, 24) <= 40, "左下应当被切掉");

    let mut inverted = SourceWithMask {
        view: source_view,
        size: (SIZE, SIZE),
        mask: Some(mask_view),
    };
    let flipped = render_with(&ctx, &mut inverted, &with(true));
    assert!(alpha_at(&flipped, 8, 8) <= 40, "反相后左上应当被切掉");
    assert!(alpha_at(&flipped, 24, 24) <= 40, "反相后右下应当被切掉");
    assert!(alpha_at(&flipped, 24, 8) >= 200, "反相后右上应当留下");
    assert!(alpha_at(&flipped, 8, 24) >= 200, "反相后左下应当留下");
}

/// **多边形裁剪（D11 第 24 轮）**：栅格化成掩码纹理之后，只留下三角形那一半。
#[test]
#[ignore = "需要真 GPU；跑：cargo test -p dhampir-worker --test timeline -- --ignored"]
fn 多边形裁剪只留下三角形那一半() {
    let (ctx, _init) = open_leg(NATIVE_BACKENDS).expect("拿不到 GPU 上下文");
    let source = make_opaque_source(&ctx);
    let source_view = source.create_view(&wgpu::TextureViewDescriptor::default());
    // 顶点是**图层框内的归一化坐标**，而 (0,0) 是**左上角**（y 向下）——
    // 所以 `[0,0] [1,0] [0,1]` 盖住的是 x+y ≤ 1 的**左上那一半**。
    // （这一版测试我先把方向想反过一次，断言全反 —— 记在这儿，免得上第二次当。）
    let composite = Composite {
        frame: 0,
        layers: vec![Layer {
            clip: Some(dhampir_core::timeline::layer::ClipShape::Polygon {
                points: vec![[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]],
            }),
            ..layer("poly", 1.0, 1.0, Vec::new())
        }],
    };
    let mut resolver = SourceWithMask {
        view: source_view,
        size: (SIZE, SIZE),
        mask: None,
    };
    let pixels = render_with(&ctx, &mut resolver, &composite);
    let alpha_at = |x: u32, y: u32| pixels[((y * SIZE + x) * 4 + 3) as usize];
    assert!(
        alpha_at(6, 6) >= 200,
        "左上角在三角形里，得到 {}",
        alpha_at(6, 6)
    );
    assert!(
        alpha_at(26, 2) >= 200,
        "贴着上边、斜边之内，得到 {}",
        alpha_at(26, 2)
    );
    assert!(
        alpha_at(2, 26) >= 200,
        "贴着左边、斜边之内，得到 {}",
        alpha_at(2, 26)
    );
    assert!(
        alpha_at(12, 12) >= 200,
        "斜边内侧，得到 {}",
        alpha_at(12, 12)
    );
    assert!(
        alpha_at(26, 26) <= 40,
        "右下角在斜边外，得到 {}",
        alpha_at(26, 26)
    );
    assert!(
        alpha_at(26, 16) <= 40,
        "右中也在斜边外，得到 {}",
        alpha_at(26, 16)
    );
    assert!(
        alpha_at(16, 26) <= 40,
        "下中也在斜边外，得到 {}",
        alpha_at(16, 26)
    );
}

/// **路径裁剪（D11 第 25 轮）**：`path()` 先细分成折线、按**图层框的文档像素**归一化，再栅格化。
#[test]
#[ignore = "需要真 GPU；跑：cargo test -p dhampir-worker --test timeline -- --ignored"]
fn 路径裁剪的直线与曲线都对() {
    let (ctx, _init) = open_leg(NATIVE_BACKENDS).expect("拿不到 GPU 上下文");
    let source = make_opaque_source(&ctx);
    let source_view = source.create_view(&wgpu::TextureViewDescriptor::default());
    // 坐标是**文档像素**、原点在图层框左上角（y 向下）—— 框是 32×32。
    let with_path = |data: &str| Composite {
        frame: 0,
        layers: vec![Layer {
            clip: Some(dhampir_core::timeline::layer::ClipShape::Path {
                data: data.to_string(),
            }),
            ..layer("path", 1.0, 1.0, Vec::new())
        }],
    };
    let render_path = |data: &str| {
        let mut resolver = SourceWithMask {
            view: source_view.clone(),
            size: (SIZE, SIZE),
            mask: None,
        };
        render_with(&ctx, &mut resolver, &with_path(data))
    };
    let alpha_at = |pixels: &[u8], x: u32, y: u32| pixels[((y * SIZE + x) * 4 + 3) as usize];

    // ① 直线三角形：盖住 x+y ≤ 32 的左上那一半。
    let straight = render_path("M 0 0 L 32 0 L 0 32 Z");
    assert!(alpha_at(&straight, 6, 6) >= 200, "左上角在三角形里");
    assert!(alpha_at(&straight, 26, 2) >= 200, "上边之内");
    assert!(alpha_at(&straight, 2, 26) >= 200, "左边之内");
    assert!(alpha_at(&straight, 26, 26) <= 40, "右下在斜边外");

    // ② 曲线：`M 0 0 C 0 32 32 32 32 0 Z` 是一条向下鼓的透镜 ——
    //    中点 (16,24) 在下边界上，所以 (16,16) 在内、(16,30) 在外。
    //    （这两条是**从曲线方程算出来的**，不是"看着像"。）
    let curved = render_path("M 0 0 C 0 32 32 32 32 0 Z");
    assert!(
        alpha_at(&curved, 16, 16) >= 200,
        "透镜内部应当留下，得到 {}",
        alpha_at(&curved, 16, 16)
    );
    assert!(
        alpha_at(&curved, 16, 30) <= 40,
        "曲线下方应当被切掉，得到 {}",
        alpha_at(&curved, 16, 30)
    );
}

/// **投影（D13 第 28 轮）**：同一层多画一张 —— 偏移、染色、模糊、浓淡，四条都钉。
#[test]
#[ignore = "需要真 GPU；跑：cargo test -p dhampir-worker --test timeline -- --ignored"]
fn 投影在下面_偏移染色模糊浓淡都对() {
    let (ctx, _init) = open_leg(NATIVE_BACKENDS).expect("拿不到 GPU 上下文");
    let source = make_opaque_source(&ctx);
    let source_view = source.create_view(&wgpu::TextureViewDescriptor::default());
    // 层缩到一半：quad 只盖中间 16×16（[8,24)²）；投影再往右下挪 4 ⇒ [12,28)²。
    let with_shadow = |shadow: Option<dhampir_core::timeline::layer::ShadowSpec>| Composite {
        frame: 0,
        layers: vec![Layer {
            shadow,
            ..layer("drop", 1.0, 0.5, Vec::new())
        }],
    };
    let spec = |blur_sigma: f32, opacity: f32| dhampir_core::timeline::layer::ShadowSpec {
        offset_x: 4.0,
        offset_y: 4.0,
        blur_sigma,
        opacity,
    };
    let render_shadow = |shadow: Option<dhampir_core::timeline::layer::ShadowSpec>| {
        let mut resolver = SourceWithMask {
            view: source_view.clone(),
            size: (SIZE, SIZE),
            mask: None,
        };
        render_with(&ctx, &mut resolver, &with_shadow(shadow))
    };
    let at = |pixels: &[u8], x: u32, y: u32| {
        let index = ((y * SIZE + x) * 4) as usize;
        [
            pixels[index],
            pixels[index + 1],
            pixels[index + 2],
            pixels[index + 3],
        ]
    };

    let plain = render_shadow(None);
    let sharp = render_shadow(Some(spec(0.0, 1.0)));

    // ① 偏移 + 染色：只在投影里、（本层之外）的那个像素应当**是黑的**。
    //    (26,14) 在阴影的 [12,28)² 里，而在本层的 [8,24)² 之外。
    let shadow_only = at(&sharp, 26, 14);
    assert!(
        shadow_only[0] <= 8 && shadow_only[1] <= 8 && shadow_only[2] <= 8,
        "阴影应当是黑的，得到 {shadow_only:?}"
    );
    assert!(
        shadow_only[3] >= 200,
        "阴影应当是实的，得到 {shadow_only:?}"
    );
    assert_eq!(at(&plain, 26, 14)[3], 0, "没有投影时那里应当是空的");

    // ② 画在**下面**：本层盖住了的地方，像素与没投影时**逐字节相同**。
    assert_eq!(
        at(&sharp, 16, 16),
        at(&plain, 16, 16),
        "本层压在阴影上，自己的像素不该变"
    );

    // ③ 模糊**确实跑了**：拿一张「左半不透明、右半透明」的源 —— 模糊会把左半的 alpha
    //    铺进右半的不透明边界之外（换成整体不透明的源就是恒等变换，验不了任何东西）。
    //
    //    **已知限制**：模糊跑在**源纹理**上，所以它只能在纹理内部扩散；图层矩形之外、
    //    以及**掩码边界**之外都扩散不到（掩码是模糊之后才乘的）。CSS 的 `drop-shadow`
    //    扩散到轮廓之外。要改对得先「把轮廓渲染进一张带透明留白的中间纹理再模糊」——
    //    那是下一步，登记表里这条能力因此记 partial。
    let half = half_source(&ctx);
    let render_half = |shadow: Option<dhampir_core::timeline::layer::ShadowSpec>| {
        let mut resolver = SourceWithMask {
            view: half.create_view(&wgpu::TextureViewDescriptor::default()),
            size: (SIZE, SIZE),
            mask: None,
        };
        render_with(&ctx, &mut resolver, &with_shadow(shadow))
    };
    let half_sharp = render_half(Some(spec(0.0, 1.0)));
    let half_blurred = render_half(Some(spec(8.0, 1.0)));
    // 不透明边界落在目标 x=20（纹理 x=16，quad 走 [12,28)）。
    assert_eq!(at(&half_sharp, 22, 14)[3], 0, "不模糊时右边应当是空的");
    assert!(
        at(&half_blurred, 22, 14)[3] > 10,
        "模糊之后应当铺到右边，得到 {:?}",
        at(&half_blurred, 22, 14)
    );

    // ④ 浓淡：0.5 的投影，alpha 应当在一半附近（而不是 0 或满）。
    let faint = render_shadow(Some(spec(0.0, 0.5)));
    let alpha = at(&faint, 26, 14)[3];
    assert!(
        (100..=160).contains(&alpha),
        "浓淡 0.5 的阴影 alpha 应当在一半附近，得到 {alpha}"
    );

    // ⑤ **向外扩散**（第 29 轮的正题）：大模糊时阴影要铺到**本层矩形之外**。
    //
    //    几何先写下来再写数（这是本项目里第三次栽在"想当然的坐标"上）：
    //    本层盖 [8,24)²；阴影再偏移 +4 ⇒ 轮廓落在 [12,28)²；
    //    模糊半径 16 源像素、scale 0.5 ⇒ 扩散只有 **8 个目标像素**，即左边界到 x=4。
    //    所以判据点取 (5,16)：在扩散里、在本层框外。
    assert_eq!(at(&sharp, 5, 16)[3], 0, "不模糊时框外不该有东西");
    let spread = render_shadow(Some(spec(8.0, 1.0)));
    assert!(
        at(&spread, 5, 16)[3] > 0,
        "大模糊应当扩散到本层矩形之外，得到 {:?}",
        at(&spread, 5, 16),
    );
    // 再往外就没有了：扩散是**有界**的（半径 × scale），不是漫无边际。
    assert_eq!(at(&spread, 2, 16)[3], 0, "扩散之外不该有东西");
}

/// 纯色源（读回型混合的期望值要能手算，就不能用带图案的合成源）。
fn solid_source(ctx: &dhampir_core::gpu::GpuContext, rgba: [u8; 4], label: &str) -> wgpu::Texture {
    let texture = ctx.device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
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
    let mut pixels = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    for _ in 0..(SIZE * SIZE) {
        pixels.extend_from_slice(&rgba);
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

/// **读回型混合（D13，第 31 轮）**：5 条公式逐条对**手算的期望值**。
///
/// 为什么期望值能手算：源与底都是纯色，且上面那一层**不透明**（αs = 1）——
/// 于是 W3C 的 `Cs' = (1−αb)·Cs + αb·B` 退化成 `B` 本身，结果是纯粹的公式值。
/// （半透明那一路也有覆盖：`Cs'` 与 αb 的插值要单独验，见下面第 ⑥ 条。）
#[test]
#[ignore = "需要真 GPU；跑：cargo test -p dhampir-worker --test timeline -- --ignored"]
fn 读回型混合的五行公式逐条对得上() {
    use dhampir_core::timeline::layer::BlendMode;
    let (ctx, _init) = open_leg(NATIVE_BACKENDS).expect("拿不到 GPU 上下文");
    let base = solid_source(&ctx, [100, 150, 200, 255], "dhampir blend base");
    let top = solid_source(&ctx, [200, 50, 100, 255], "dhampir blend top");

    let render_mode = |mode: BlendMode, opacity: f32| -> [u8; 4] {
        let composite = Composite {
            frame: 0,
            layers: vec![
                Layer {
                    source: "base".to_string(),
                    ..layer("base", 1.0, 1.0, Vec::new())
                },
                Layer {
                    source: "top".to_string(),
                    blend: mode,
                    ..layer("top", opacity, 1.0, Vec::new())
                },
            ],
        };
        // 两个名字各给一张：用两张纹理的解析器。
        struct Two<'a> {
            base: &'a wgpu::TextureView,
            top: &'a wgpu::TextureView,
        }
        impl SourceResolver for Two<'_> {
            fn texture_for(
                &mut self,
                source: &str,
                _frame: i64,
            ) -> Option<(wgpu::TextureView, (u32, u32))> {
                let view = if source == "top" { self.top } else { self.base };
                Some((view.clone(), (SIZE, SIZE)))
            }
        }
        let base_view = base.create_view(&wgpu::TextureViewDescriptor::default());
        let top_view = top.create_view(&wgpu::TextureViewDescriptor::default());
        let mut resolver = Two {
            base: &base_view,
            top: &top_view,
        };
        let pixels = render_with(&ctx, &mut resolver, &composite);
        let index = (((SIZE / 2) * SIZE + SIZE / 2) * 4) as usize;
        [
            pixels[index],
            pixels[index + 1],
            pixels[index + 2],
            pixels[index + 3],
        ]
    };
    let near = |got: [u8; 4], want: [u8; 3]| {
        for channel in 0..3 {
            let delta = (got[channel] as i32 - want[channel] as i32).abs();
            assert!(
                delta <= 3,
                "通道 {channel}：得到 {got:?}，期望 {want:?}（容差 3）"
            );
        }
        assert_eq!(got[3], 255, "底是不透明的，结果也该是");
    };

    // 五条期望值按 W3C 的公式手算（s = [200,50,100]、d = [100,150,200]）。
    near(render_mode(BlendMode::Darken, 1.0), [100, 50, 100]);
    near(render_mode(BlendMode::Lighten, 1.0), [200, 150, 200]);
    near(render_mode(BlendMode::Difference, 1.0), [100, 100, 100]);
    near(render_mode(BlendMode::Overlay, 1.0), [157, 86, 188]);
    near(render_mode(BlendMode::SoftLight, 1.0), [134, 112, 191]);
    // ⑥ 半透明的上面那一层：αs = 0.5 ⇒ 结果 = 0.5·B + 0.5·d（底不透明）。
    //    darken 的 B = [100,50,100] ⇒ 0.5·B + 0.5·d = [100,100,150]。
    near(render_mode(BlendMode::Darken, 0.5), [100, 100, 150]);
}

/// **固定方程那 4 条（D13 判据 ④）**：走合成器的老路，值必须还是那几条经典公式。
///
/// 为什么要专门钉：第 31 轮给合成器加了一条**读回型**的新回路，而"新路没弄坏老路"这件事
/// 只能靠判据说话。这里用与读回型**同一套**纯色 + 手算期望值的办法，把 4 条一起按住。
///
/// （诚实说明：这 4 条的**改动前 digest** 当时没有留 —— 所以这条钉的是"值仍是那几条公式"，
///   而不是"与改动前逐字节相同"。新增的基线记在 evidence 里，供下一轮对照。）
#[test]
#[ignore = "需要真 GPU；跑：cargo test -p dhampir-worker --test timeline -- --ignored"]
fn 固定方程的四条混合值仍是经典公式() {
    use dhampir_core::timeline::layer::BlendMode;
    let (ctx, _init) = open_leg(NATIVE_BACKENDS).expect("拿不到 GPU 上下文");
    let base = solid_source(&ctx, [100, 150, 200, 255], "dhampir fixed base");
    let top = solid_source(&ctx, [200, 50, 100, 255], "dhampir fixed top");
    let render_mode = |mode: BlendMode| -> [u8; 4] {
        let composite = Composite {
            frame: 0,
            layers: vec![
                Layer {
                    source: "base".to_string(),
                    ..layer("base", 1.0, 1.0, Vec::new())
                },
                Layer {
                    source: "top".to_string(),
                    blend: mode,
                    ..layer("top", 1.0, 1.0, Vec::new())
                },
            ],
        };
        struct Two<'a> {
            base: &'a wgpu::TextureView,
            top: &'a wgpu::TextureView,
        }
        impl SourceResolver for Two<'_> {
            fn texture_for(
                &mut self,
                source: &str,
                _frame: i64,
            ) -> Option<(wgpu::TextureView, (u32, u32))> {
                let view = if source == "top" { self.top } else { self.base };
                Some((view.clone(), (SIZE, SIZE)))
            }
        }
        let base_view = base.create_view(&wgpu::TextureViewDescriptor::default());
        let top_view = top.create_view(&wgpu::TextureViewDescriptor::default());
        let mut resolver = Two {
            base: &base_view,
            top: &top_view,
        };
        let pixels = render_with(&ctx, &mut resolver, &composite);
        let index = (((SIZE / 2) * SIZE + SIZE / 2) * 4) as usize;
        [
            pixels[index],
            pixels[index + 1],
            pixels[index + 2],
            pixels[index + 3],
        ]
    };
    let near = |got: [u8; 4], want: [u8; 3], what: &str| {
        for channel in 0..3 {
            let delta = (got[channel] as i32 - want[channel] as i32).abs();
            assert!(
                delta <= 3,
                "{what} 通道 {channel}：得到 {got:?}，期望 {want:?}（容差 3）"
            );
        }
    };
    // s = [200,50,100]、d = [100,150,200]、顶层不透明 ⇒ 结果就是那条公式本身。
    near(render_mode(BlendMode::Normal), [200, 50, 100], "normal");
    // add：逐通道相加并夹到 255。
    near(render_mode(BlendMode::Add), [255, 200, 255], "add");
    // multiply：s·d/255。
    near(render_mode(BlendMode::Multiply), [78, 29, 78], "multiply");
    // screen：255 − (255−s)(255−d)/255。
    near(render_mode(BlendMode::Screen), [222, 171, 222], "screen");
}

/// 双色源：左半不透明红、右半不透明蓝（用来验「背景被真的取来滤波」）。
fn two_color_source(ctx: &dhampir_core::gpu::GpuContext) -> wgpu::Texture {
    colored_source(ctx, "dhampir backdrop test source", |x| {
        if x < SIZE / 2 {
            [255, 0, 0, 255]
        } else {
            [0, 0, 255, 255]
        }
    })
}

/// 全透明源：给「玻璃」那一层用 —— 它自己的像素不该盖住被滤波的背景。
fn clear_source(ctx: &dhampir_core::gpu::GpuContext) -> wgpu::Texture {
    colored_source(ctx, "dhampir backdrop test clear", |_x| [0, 0, 0, 0])
}

fn colored_source(
    ctx: &dhampir_core::gpu::GpuContext,
    label: &str,
    pick: impl Fn(u32) -> [u8; 4],
) -> wgpu::Texture {
    let texture = ctx.device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
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
    let mut pixels = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    for _y in 0..SIZE {
        for x in 0..SIZE {
            pixels.extend_from_slice(&pick(x));
        }
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

/// **背景滤镜（D13 第二个用户，第 36 轮）**：只作用在**这一层的矩形**里，且真的读了身后的内容。
///
/// 第 35 轮这条判据抓到过一个真错：滤波结果在**目标分辨率**，我却用这一层**缩放过的**四边形
/// 去贴它 ⇒ 结果被再缩放一次（蓝只漏进来 6/255）。现在的做法是 **1:1 贴 + 矩形覆盖度掩码**。
#[test]
#[ignore = "需要真 GPU；跑：cargo test -p dhampir-worker --test timeline -- --ignored"]
fn 背景滤镜只在层的矩形里生效_而且真的读了身后() {
    let (ctx, _init) = open_leg(NATIVE_BACKENDS).expect("拿不到 GPU 上下文");
    let base = two_color_source(&ctx);
    let glass_source = clear_source(&ctx);
    let base_view = base.create_view(&wgpu::TextureViewDescriptor::default());
    let glass_view = glass_source.create_view(&wgpu::TextureViewDescriptor::default());
    struct Two<'a> {
        base: &'a wgpu::TextureView,
        glass: &'a wgpu::TextureView,
    }
    impl SourceResolver for Two<'_> {
        fn texture_for(
            &mut self,
            source: &str,
            _frame: i64,
        ) -> Option<(wgpu::TextureView, (u32, u32))> {
            let view = if source == "glass" {
                self.glass
            } else {
                self.base
            };
            Some((view.clone(), (SIZE, SIZE)))
        }
    }
    let render_with_glass = |backdrop: bool| -> Vec<u8> {
        let mut glass = layer("glass", 1.0, 0.5, Vec::new());
        glass.source = "glass".to_string();
        if backdrop {
            glass.backdrop_effects = vec![Effect {
                kind: "gaussian_blur".to_string(),
                params: std::collections::BTreeMap::from([("radius".to_string(), 8.0_f32)]),
                ..Default::default()
            }];
        }
        let composite = Composite {
            frame: 0,
            layers: vec![
                Layer {
                    source: "base".to_string(),
                    ..layer("base", 1.0, 1.0, Vec::new())
                },
                glass,
            ],
        };
        let mut resolver = Two {
            base: &base_view,
            glass: &glass_view,
        };
        render_with(&ctx, &mut resolver, &composite)
    };
    let with = render_with_glass(true);
    let without = render_with_glass(false);
    let at = |pixels: &[u8], x: u32, y: u32| {
        let index = ((y * SIZE + x) * 4) as usize;
        [
            pixels[index],
            pixels[index + 1],
            pixels[index + 2],
            pixels[index + 3],
        ]
    };

    // ① **矩形外逐字节不变**：玻璃层只盖 [8,24)²。
    for (x, y) in [(2u32, 2u32), (30, 30), (2, 30), (30, 2)] {
        assert_eq!(
            at(&with, x, y),
            at(&without, x, y),
            "({x},{y}) 在玻璃矩形之外，不该被动过"
        );
    }

    // ② 矩形内**真的读了身后**：双色边界在 x=16，模糊把蓝带进左侧、红带进右侧。
    assert_eq!(
        at(&without, 12, 16)[2],
        0,
        "没挂背景滤镜时，红半区的蓝分量应当是 0"
    );
    assert!(
        at(&with, 12, 16)[2] > 40,
        "挂了之后应当被蓝半区染上，得到 {:?}",
        at(&with, 12, 16)
    );
    // 阈值只要能"证明混了"就够（`without` 在那里红分量是 0）；具体多少取决于核的形状。
    assert!(
        at(&with, 20, 16)[0] > 20,
        "蓝半区靠近边界处应当被红染上，得到 {:?}",
        at(&with, 20, 16)
    );
}

/// **乘性亮度（第 42 轮）**：CSS `brightness()` 那一种（`c·k`，保黑）。
///
/// 判据的重点不是"能跑"，而是**它与加性那条不是同一个函数** —— 黑附近的像素是分水岭：
/// 乘性把 10 变成 5（×0.5），加性会把 10 抬到 15（+0.05）—— 方向都相反。
#[test]
#[ignore = "需要真 GPU；跑：cargo test -p dhampir-worker --test timeline -- --ignored"]
fn 乘性亮度保黑_与加性那条不是同一个函数() {
    let (ctx, _init) = open_leg(NATIVE_BACKENDS).expect("拿不到 GPU 上下文");
    let source = solid_source(&ctx, [10, 100, 200, 255], "dhampir brightness source");
    let view = source.create_view(&wgpu::TextureViewDescriptor::default());
    let at = |pixels: &[u8]| {
        let index = (((SIZE / 2) * SIZE + SIZE / 2) * 4) as usize;
        [
            pixels[index],
            pixels[index + 1],
            pixels[index + 2],
            pixels[index + 3],
        ]
    };
    let render_effect = |kind: &str, name: &str, value: f32| -> [u8; 4] {
        struct One<'a> {
            view: &'a wgpu::TextureView,
        }
        impl SourceResolver for One<'_> {
            fn texture_for(
                &mut self,
                _source: &str,
                _frame: i64,
            ) -> Option<(wgpu::TextureView, (u32, u32))> {
                Some((self.view.clone(), (SIZE, SIZE)))
            }
        }
        // **调色特效要挂在调整层上**：有素材的层在 `Draw` 里只认跑在源纹理上的模糊，
        // 别的特效挂上去会被校验拦下（本仓那条"写好了没接上"的规矩）。所以这里用"底 + 调整层"。
        let composite = Composite {
            frame: 0,
            layers: vec![
                Layer {
                    source: "s".to_string(),
                    ..layer("s", 1.0, 1.0, Vec::new())
                },
                Layer {
                    is_adjustment: true,
                    ..layer(
                        "adj",
                        1.0,
                        1.0,
                        vec![Effect {
                            kind: kind.to_string(),
                            params: std::collections::BTreeMap::from([(name.to_string(), value)]),
                            ..Default::default()
                        }],
                    )
                },
            ],
        };
        let mut resolver = One { view: &view };
        at(&render_with(&ctx, &mut resolver, &composite))
    };
    // ×1.5：10→15、100→150、200→255（截断）。
    let up = render_effect("brightness_multiply", "factor", 1.5);
    assert!(up[0].abs_diff(15) <= 2, "10 × 1.5 应当是 15，得到 {up:?}");
    assert!(
        up[1].abs_diff(150) <= 2,
        "100 × 1.5 应当是 150，得到 {up:?}"
    );
    // ×0.5：10→5 —— 加性那条在这里会**抬亮**（+0.05 ⇒ 10→15），方向相反。
    let down = render_effect("brightness_multiply", "factor", 0.5);
    assert!(down[0].abs_diff(5) <= 2, "10 × 0.5 应当是 5，得到 {down:?}");
    let added = render_effect("brightness", "amount", 0.05);
    assert!(
        added[0] > 10,
        "加性亮度会把 10 抬亮（这正是它与 CSS 不同的地方），得到 {added:?}"
    );
}

/// **规范色相旋转（第 44 轮）**：CSS/SVG 那套系数与 YIQ 那条不是一回事。
///
/// 判据取**纯红转 90°** 并**手算**（θ=90 ⇒ cos=0、sin=1，矩阵取自规范 MathML）：
/// - 规范：`R′=0.213−0.213=0`、`G′=0.213+0.143=0.356`、`B′=0.213−0.787<0` ⇒ 约 `(0, 91, 0)`
/// - YIQ ：`R′=0.299+0.168=0.467`、`G′=0.587+0.330=0.917`、`B′≈−0.383` ⇒ 约 `(119, 234, 0)`
#[test]
#[ignore = "需要真 GPU；跑：cargo test -p dhampir-worker --test timeline -- --ignored"]
fn 规范色相旋转与_yiq_那条明显不同() {
    let (ctx, _init) = open_leg(NATIVE_BACKENDS).expect("拿不到 GPU 上下文");
    let source = solid_source(&ctx, [255, 0, 0, 255], "dhampir hue source");
    let view = source.create_view(&wgpu::TextureViewDescriptor::default());
    let render_effect = |kind: &str, degrees: f32| -> [u8; 4] {
        struct One<'a> {
            view: &'a wgpu::TextureView,
        }
        impl SourceResolver for One<'_> {
            fn texture_for(
                &mut self,
                _source: &str,
                _frame: i64,
            ) -> Option<(wgpu::TextureView, (u32, u32))> {
                Some((self.view.clone(), (SIZE, SIZE)))
            }
        }
        let composite = Composite {
            frame: 0,
            layers: vec![
                Layer {
                    source: "s".to_string(),
                    ..layer("s", 1.0, 1.0, Vec::new())
                },
                Layer {
                    is_adjustment: true,
                    ..layer(
                        "adj",
                        1.0,
                        1.0,
                        vec![Effect {
                            kind: kind.to_string(),
                            params: std::collections::BTreeMap::from([(
                                "degrees".to_string(),
                                degrees,
                            )]),
                            ..Default::default()
                        }],
                    )
                },
            ],
        };
        let mut resolver = One { view: &view };
        let pixels = render_with(&ctx, &mut resolver, &composite);
        let index = (((SIZE / 2) * SIZE + SIZE / 2) * 4) as usize;
        [
            pixels[index],
            pixels[index + 1],
            pixels[index + 2],
            pixels[index + 3],
        ]
    };
    // ⓪ 0° 必须**逐位**是原色 —— 这一步同时证明"整步被跳过"没有副作用。
    assert_eq!(
        render_effect("hue_rotate_css", 0.0),
        [255, 0, 0, 255],
        "0° 应当逐位不变"
    );
    // ① 规范矩阵：约 (0, 91, 0)；② YIQ 那条：约 (119, 234, 0)。
    let spec = render_effect("hue_rotate_css", 90.0);
    let yiq = render_effect("hue", 90.0);
    assert!(spec[0] <= 8, "规范矩阵下红分量应当几乎为零，得到 {spec:?}");
    assert!(
        spec[1].abs_diff(91) <= 8,
        "规范矩阵的绿分量应当约 91，得到 {spec:?}"
    );
    assert!(spec[2] <= 8, "规范矩阵下蓝分量被截断为零，得到 {spec:?}");
    assert!(
        yiq[0].abs_diff(119) <= 8,
        "YIQ 那条的红分量应当约 119，得到 {yiq:?}"
    );
    assert!(
        yiq[1].abs_diff(234) <= 8,
        "YIQ 那条的绿分量应当约 234，得到 {yiq:?}"
    );
    assert!(
        spec[1].abs_diff(yiq[1]) > 100,
        "两条必须在明显不同的地方：spec={spec:?} yiq={yiq:?}"
    );
}

/// **规范权重饱和度（第 45 轮）**：灰度权重用 CSS/SVG 那组取整值（0.213/0.715/0.072）。
///
/// 手算（源 `(255,128,0)` = `(1.0, 0.502, 0.0)`、`amount = 0.5`）：
/// `luma = 0.213 + 0.715×0.502 = 0.5719`，`mix(luma, rgb, 0.5)` ⇒ `(0.786, 0.537, 0.286)` ⇒ 约 `(200, 137, 73)`。
#[test]
#[ignore = "需要真 GPU；跑：cargo test -p dhampir-worker --test timeline -- --ignored"]
fn 规范权重饱和度_与_rec709_那条几乎一样但本源不同() {
    let (ctx, _init) = open_leg(NATIVE_BACKENDS).expect("拿不到 GPU 上下文");
    let at = |pixels: &[u8]| {
        let index = (((SIZE / 2) * SIZE + SIZE / 2) * 4) as usize;
        [
            pixels[index],
            pixels[index + 1],
            pixels[index + 2],
            pixels[index + 3],
        ]
    };
    let render = |rgba: [u8; 4], kind: &str, amount: f32| -> [u8; 4] {
        let source = solid_source(&ctx, rgba, "dhampir saturation source");
        let view = source.create_view(&wgpu::TextureViewDescriptor::default());
        struct One<'a> {
            view: &'a wgpu::TextureView,
        }
        impl SourceResolver for One<'_> {
            fn texture_for(
                &mut self,
                _source: &str,
                _frame: i64,
            ) -> Option<(wgpu::TextureView, (u32, u32))> {
                Some((self.view.clone(), (SIZE, SIZE)))
            }
        }
        let composite = Composite {
            frame: 0,
            layers: vec![
                Layer {
                    source: "s".to_string(),
                    ..layer("s", 1.0, 1.0, Vec::new())
                },
                Layer {
                    is_adjustment: true,
                    ..layer(
                        "adj",
                        1.0,
                        1.0,
                        vec![Effect {
                            kind: kind.to_string(),
                            params: std::collections::BTreeMap::from([(
                                "amount".to_string(),
                                amount,
                            )]),
                            ..Default::default()
                        }],
                    )
                },
            ],
        };
        let mut resolver = One { view: &view };
        at(&render_with(&ctx, &mut resolver, &composite))
    };
    // ⓪ amount = 1.0 必须**逐位**是原色（整步 skipped）。
    assert_eq!(
        render([255, 128, 0, 255], "saturation_css", 1.0),
        [255, 128, 0, 255]
    );
    // ① 规范权重：手算 (200, 137, 73)。
    let spec = render([255, 128, 0, 255], "saturation_css", 0.5);
    for (channel, want) in [200u8, 137, 73].iter().enumerate() {
        assert!(
            spec[channel].abs_diff(*want) <= 2,
            "通道 {channel} 应当约 {want}，得到 {spec:?}"
        );
    }
    // ② 两条权重**几乎一样**：这是实测，代替原来那条"极小偏差"的文字容差登记。
    //    逐通道差应当 ≤ 1 —— 也就是说"看得见的差别"确实没有；但不等于同一个函数。
    for rgba in [[255, 128, 0, 255], [0, 255, 0, 255], [32, 200, 180, 255]] {
        let a = render(rgba, "saturation_css", 0.5);
        let b = render(rgba, "saturation", 0.5);
        for channel in 0..3 {
            assert!(
                a[channel].abs_diff(b[channel]) <= 1,
                "{rgba:?} 通道 {channel}：规范 {a:?} vs Rec.709 {b:?} 差超过了 1 档"
            );
        }
    }
}

/// **渐变遮罩（第 47 轮）**：程序化渐变在**这一层自己的尺寸**上栅格化。
///
/// 层缩到一半（盖 `[8,24)²`），源不透明红；渐变 90°（朝右）、断点 `(0,0) → (1,1)`：
/// 于是层的**左边缘**覆盖度≈0（看不见）、**右边缘**≈1（全见）、**正中间**≈0.5。
///
/// 这条同时钉住了「算在哪个尺寸上」：若错误地按**目标**尺寸算（32 宽），
/// x=10 的覆盖度会是 `10.5/32 ≈ 0.33`（α≈84），而不是层自己那 16 宽上的 `2.5/16 ≈ 0.16`（α≈40）。
#[test]
#[ignore = "需要真 GPU；跑：cargo test -p dhampir-worker --test timeline -- --ignored"]
fn 渐变遮罩按层自己的尺寸铺开_左右与反相都对() {
    use dhampir_core::timeline::layer::{GradientStop, LinearGradient, MaskChannel, MaskSpec};
    let (ctx, _init) = open_leg(NATIVE_BACKENDS).expect("拿不到 GPU 上下文");
    let source = solid_source(&ctx, [255, 0, 0, 255], "dhampir gradient mask source");
    let view = source.create_view(&wgpu::TextureViewDescriptor::default());
    struct One<'a> {
        view: &'a wgpu::TextureView,
    }
    impl SourceResolver for One<'_> {
        fn texture_for(
            &mut self,
            _source: &str,
            _frame: i64,
        ) -> Option<(wgpu::TextureView, (u32, u32))> {
            Some((self.view.clone(), (SIZE, SIZE)))
        }
    }
    let render_masked = |invert: bool| -> Vec<u8> {
        let mut masked = layer("g", 1.0, 0.5, Vec::new());
        masked.source = "s".to_string();
        masked.mask = Some(MaskSpec {
            asset_id: String::new(),
            gradient: Some(LinearGradient {
                angle_deg: 90.0,
                stops: vec![
                    GradientStop {
                        at: 0.0,
                        coverage: 0.0,
                    },
                    GradientStop {
                        at: 1.0,
                        coverage: 1.0,
                    },
                ],
            }),
            channel: MaskChannel::Alpha,
            invert,
        });
        let composite = Composite {
            frame: 0,
            layers: vec![masked],
        };
        let mut resolver = One { view: &view };
        render_with(&ctx, &mut resolver, &composite)
    };
    let at = |pixels: &[u8], x: u32, y: u32| {
        let index = ((y * SIZE + x) * 4) as usize;
        pixels[index + 3]
    };
    let masked = render_masked(false);
    assert!(
        at(&masked, 10, 16) <= 50,
        "左边缘覆盖度≈0，alpha 应当很小，得到 {}",
        at(&masked, 10, 16)
    );
    assert!(
        at(&masked, 22, 16) >= 200,
        "右边缘覆盖度≈1，alpha 应当很大，得到 {}",
        at(&masked, 22, 16)
    );
    let middle = at(&masked, 16, 16) as i32;
    assert!(
        (middle - 127).abs() <= 30,
        "正中间应当是半透明，得到 {middle}"
    );
    // 层框之外完全不该有东西。
    assert_eq!(at(&masked, 2, 16), 0, "层框之外不该有像素");
    // 反相：左右对调。
    let inverted = render_masked(true);
    assert!(
        at(&inverted, 10, 16) >= 200,
        "反相后左边缘应当几乎全见，得到 {}",
        at(&inverted, 10, 16)
    );
    assert!(
        at(&inverted, 22, 16) <= 50,
        "反相后右边缘应当几乎不可见，得到 {}",
        at(&inverted, 22, 16)
    );
}

/// **引擎侧的抗锯齿斜坡：量出来，而不是断言**（第 52 轮）。
///
/// 台账 `geometry.edge_antialias` 的 note 里写着"引擎是 1 目标像素宽的线性斜坡"——
/// 那是一句**论断**。这条判据把它变成**实测**：把圆角层**转过 30°**（让边缘落在像素内部，
/// 逼出真正的抗锯齿），然后在一行里数"半透明像素"有几个：
///
/// - 一个都没有 ⇒ **没有抗锯齿**（硬边）✗
/// - 有很多（比如 > 4）⇒ 那是**模糊**，不是 1 像素斜坡 ✗
/// - 1..3 个 ⇒ 与"1 目标像素宽的斜坡"一致 ✓
#[test]
#[ignore = "需要真 GPU；跑：cargo test -p dhampir-worker --test timeline -- --ignored"]
fn 圆角层的边缘是窄抗锯齿_不是硬边也不是模糊() {
    let (ctx, _init) = open_leg(NATIVE_BACKENDS).expect("拿不到 GPU 上下文");
    // **判决要能归因**：把机器打出来，数字才不是浮在空中的。
    println!("GPU 适配器：{:?}", ctx.adapter_info);
    let source = solid_source(&ctx, [255, 255, 255, 255], "dhampir aa source");
    let view = source.create_view(&wgpu::TextureViewDescriptor::default());
    struct One<'a> {
        view: &'a wgpu::TextureView,
    }
    impl SourceResolver for One<'_> {
        fn texture_for(
            &mut self,
            _source: &str,
            _frame: i64,
        ) -> Option<(wgpu::TextureView, (u32, u32))> {
            Some((self.view.clone(), (SIZE, SIZE)))
        }
    }
    let mut rotated = layer("r", 1.0, 1.0, Vec::new());
    rotated.source = "s".to_string();
    rotated.corner_radius = 6.0;
    // 转 30°：边缘不再与像素网格对齐，抗锯齿必须自己发生。
    rotated.transform.rotation_deg = 30.0;
    let composite = Composite {
        frame: 0,
        layers: vec![rotated],
    };
    let mut resolver = One { view: &view };
    let pixels = render_with(&ctx, &mut resolver, &composite);
    // **不能只看一行**：中间那行整行都在形状内部（α 全是 255）—— 第 52 轮第一版就栽在这。
    //
    // 改成量**全局**：半透明像素的总数 ÷ 边缘总长 ≈ "斜坡有多少像素宽"。
    // 这个量与几何无关，而且两头都能分辨：
    //   · 硬边（不做抗锯齿）⇒ 半透明像素 **0** 个；
    //   · 模糊（不是 1 像素斜坡）⇒ 比值会成倍变大（>2）。
    let mut intermediate = 0usize;
    let mut min_alpha = 255u8;
    let mut max_alpha = 0u8;
    for y in 0..SIZE {
        for x in 0..SIZE {
            let alpha = pixels[((y * SIZE + x) * 4 + 3) as usize];
            if alpha > 0 && alpha < 255 {
                intermediate += 1;
                min_alpha = min_alpha.min(alpha);
                max_alpha = max_alpha.max(alpha);
            }
        }
    }
    let perimeter = 4.0 * SIZE as f32;
    let ramp = intermediate as f32 / perimeter;
    println!(
        "AA 实测：半透明像素 {intermediate} 个，边缘总长 {perimeter}，斜坡 ≈ {ramp:.3} 像素宽（α 区间 {min_alpha}..{max_alpha}）"
    );
    assert!(
        intermediate > 0,
        "转 30° 的边缘必须有抗锯齿（一个半透明像素都没有 ⇒ 硬边）"
    );
    // 下界取 0.05 而不是"约 1"：第 52 轮实测是 **0.22 像素宽** ——
    // 台账原先那句"1 目标像素宽的线性斜坡"是**论断**，实测把它推翻了（斜坡更窄）。
    // 下界只用来抓"几乎没有抗锯齿"，不把当初的猜测写死。
    assert!(
        (0.05..=2.0).contains(&ramp),
        "斜坡应当又窄又是真抗锯齿，实测 ≈ {ramp:.3}（半透明 {intermediate} 个 / 周长 {perimeter}）"
    );
}

/// **加法混合：`add` 与 CSS `plus-lighter` 的关系（第 53 轮，把"未必"变成精确）**。
///
/// 两边的定义（都写下来，才谈得上"关系"）：
/// - 本仓 `add`：颜色分量 `src·1 + dst·1` —— **rgb 不看源 alpha**；
/// - CSS `plus-lighter`：`αs·Cs + Cb`（**预乘后**相加），`αo = min(1, αs + αb)`。
///
/// ⇒ **相同当且仅当 `αs = 1`**；`αs < 1` 时本仓多加了 `(1−αs)·Cs`。
///
/// 这条判据钉的正是那个"当且仅当"的另一半：**同一个加法层，opacity 从 1.0 调到 0.5，
/// rgb 一字不变**（因为我们的 rgb 根本不看 αs）——而 CSS 那边会变成 `0.5·Cs + Cb`。
#[test]
#[ignore = "需要真 GPU；跑：cargo test -p dhampir-worker --test timeline -- --ignored"]
fn 加法混合_rgb_不看源_alpha_而_css_会看() {
    use dhampir_core::timeline::layer::BlendMode;
    let (ctx, _init) = open_leg(NATIVE_BACKENDS).expect("拿不到 GPU 上下文");
    let base = solid_source(&ctx, [50, 50, 50, 255], "dhampir add base");
    let top = solid_source(&ctx, [100, 20, 10, 255], "dhampir add top");
    let base_view = base.create_view(&wgpu::TextureViewDescriptor::default());
    let top_view = top.create_view(&wgpu::TextureViewDescriptor::default());
    struct Two<'a> {
        base: &'a wgpu::TextureView,
        top: &'a wgpu::TextureView,
    }
    impl SourceResolver for Two<'_> {
        fn texture_for(
            &mut self,
            source: &str,
            _frame: i64,
        ) -> Option<(wgpu::TextureView, (u32, u32))> {
            let view = if source == "top" { self.top } else { self.base };
            Some((view.clone(), (SIZE, SIZE)))
        }
    }
    let render = |opacity: f32| -> [u8; 4] {
        let composite = Composite {
            frame: 0,
            layers: vec![
                Layer {
                    source: "base".to_string(),
                    ..layer("base", 1.0, 1.0, Vec::new())
                },
                Layer {
                    source: "top".to_string(),
                    blend: BlendMode::Add,
                    ..layer("top", opacity, 1.0, Vec::new())
                },
            ],
        };
        let mut resolver = Two {
            base: &base_view,
            top: &top_view,
        };
        let pixels = render_with(&ctx, &mut resolver, &composite);
        let index = (((SIZE / 2) * SIZE + SIZE / 2) * 4) as usize;
        [
            pixels[index],
            pixels[index + 1],
            pixels[index + 2],
            pixels[index + 3],
        ]
    };
    let opaque = render(1.0);
    let half = render(0.5);
    // ① αs = 1：两边**相同**（都是 Cs + Cb = (150, 70, 60)）。
    for (channel, want) in [150u8, 70, 60].iter().enumerate() {
        assert!(
            opaque[channel].abs_diff(*want) <= 2,
            "αs=1 时应当是 {want}，得到 {opaque:?}"
        );
    }
    // ② αs = 0.5：本仓**一字不变**（rgb 不看 αs）—— CSS 那边会是 0.5·Cs + Cb = (100, 60, 55)。
    assert_eq!(
        half, opaque,
        "本仓的 rgb 不看源 alpha（这正是与 plus-lighter 的差别所在）：opacity=0.5 给 {half:?}、opacity=1.0 给 {opaque:?}"
    );
    // 把"差多少"也算出来写进断言消息里（不靠注释）：差 = (1−αs)·Cs = (50, 10, 5)。
    let css_expected = [100i32, 60, 55];
    for channel in 0..3 {
        let delta = opaque[channel] as i32 - css_expected[channel];
        let want = ((1.0 - 0.5) * [100.0f32, 20.0, 10.0][channel]).round() as i32;
        assert!(
            (delta - want).abs() <= 2,
            "通道 {channel}：与 plus-lighter 的差应当等于 (1−αs)·Cs = {want}，得到 {delta}"
        );
    }
}

/// **掩码插值核：引擎侧实测**（第 55 轮）。
///
/// 台账 `mask.interpolation_kernel` 说"两边都滤波，但核不同" —— 那是关于**差异**的话。
/// 这一条量的是**我们自己那半边**：把 2×2 的掩码（左上亮、其余全暗）铺到 32×32 上，
/// 2 个纹素中心落在 x=8 与 x=24 ⇒ **50/50 点应当在 x=16**。
///
/// 判据（三条一起才叫"是自己的双线性"）：
/// 1. 单调不增；
/// 2. x=16 处约 127（半亮）—— nearest 采样在这里只会给 0 或 255；
/// 3. 斜坡上**逐像素增量基本恒定**（线性），差的极差 ≤ 3。
#[test]
#[ignore = "需要真 GPU；跑：cargo test -p dhampir-worker --test timeline -- --ignored"]
fn 掩码采样是自己写的双线性_斜坡线性且五十点在半亮() {
    let (ctx, _init) = open_leg(NATIVE_BACKENDS).expect("拿不到 GPU 上下文");
    println!("GPU 适配器：{:?}", ctx.adapter_info);
    let source = solid_source(&ctx, [255, 255, 255, 255], "dhampir mask source");
    // 决定性对照：**全亮**掩码。若第一个纹素仍读不到 255，那是坐标映射偏了半个纹素；
    // 若读到 255，才说明先前那个 120 是"与相邻暗纹素的混入"。
    let all_bright = false;
    let mask = if all_bright {
        mask_2x2(&ctx, [[255, 255, 255, 255]; 4])
    } else {
        mask_2x2(
            &ctx,
            [
                [255, 255, 255, 255],
                [0, 0, 0, 0],
                [0, 0, 0, 0],
                [0, 0, 0, 0],
            ],
        )
    };
    let view = source.create_view(&wgpu::TextureViewDescriptor::default());
    let mask_view = mask.create_view(&wgpu::TextureViewDescriptor::default());
    let mut masked = layer("m", 1.0, 1.0, Vec::new());
    masked.source = "s".to_string();
    masked.mask = Some(MaskSpec {
        asset_id: "mask.png".to_string(),
        gradient: None,
        channel: MaskChannel::Alpha,
        invert: false,
    });
    let composite = Composite {
        frame: 0,
        layers: vec![masked],
    };
    let mut resolver = SourceWithMask {
        view: view.clone(),
        size: (SIZE, SIZE),
        mask: Some(mask_view),
    };
    let pixels = render_with(&ctx, &mut resolver, &composite);
    // **行要选在纹素中心上，不能选在纹素分界上**：2×2 掩码铺到 32×32，
    // 两行纹素的分界正好在 y=16 —— 第 55 轮我读了那一行，得到 120，
    // 于是误报了一条缺陷（D17）。真相是：**那一行本来就该是亮暗各半**，
    // 我们自己的双线性是对的。现在读 y=4（第 0 行纹素的中心）。
    let row = SIZE / 8;
    let profile: Vec<u8> = (0..SIZE)
        .map(|x| pixels[((row * SIZE + x) * 4 + 3) as usize])
        .collect();
    println!("掩码剖面（行 {row}）：{profile:?}");
    // 1. 单调不增
    for window in profile.windows(2) {
        assert!(window[0] >= window[1], "剖面应当单调不增：{profile:?}");
    }
    // 2. **边缘没有被压暗**（读纹素中心那一行时，第一个纹素应当精确是 255）。
    //    第 55 轮我误报了 D17：那次读的是 y=16 —— 正好是两行纹素的**分界**，
    //    本来就该读到亮暗各半（120）。**量之前先想清楚自己在量哪一行**，已撤回那一条。
    assert_eq!(
        profile[0], 255,
        "纹素中心那一行读到的第一个纹素应当精确是全亮，得到 {}：{profile:?}",
        profile[0]
    );
    // 3. 斜坡线性：逐像素增量应当恒定（这就是"自己的双线性"的定义）。
    let ramp: Vec<i32> = profile
        .iter()
        .map(|v| *v as i32)
        .filter(|v| *v > 5 && *v < 250)
        .collect();
    assert!(
        ramp.len() >= 12,
        "斜坡应当有足够多的取样点，得到 {} 个：{profile:?}",
        ramp.len()
    );
    let deltas: Vec<i32> = ramp.windows(2).map(|w| w[0] - w[1]).collect();
    let spread = deltas.iter().max().unwrap() - deltas.iter().min().unwrap();
    assert!(
        spread <= 3,
        "双线性应当是线性的（逐像素增量恒定），得到增量极差 {spread}：{deltas:?}"
    );
}
