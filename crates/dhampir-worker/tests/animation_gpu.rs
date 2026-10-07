//! 动图纹理缓存的**真机**验证：上传、逐帧取、越界钳制、预算闸。
//!
//! **为什么在 worker**：core 不开任何 wgpu 后端 feature（开了会把 wasm32 打编译死），
//! 所以 core 里拿不到 adapter。缓存本身在 core，但要**真机验证它上传了什么**
//! 就只能放到能建 Instance 的宿主里。
//!
//! 要跑：cargo test -p dhampir-worker --test animation_gpu -- --ignored

use dhampir_core::animation::{AnimFormat, AnimFrame, Animation};
use dhampir_core::gpu::NATIVE_BACKENDS;
use dhampir_core::readback;
use dhampir_core::render::AnimationTextures;
use dhampir_core::wgpu;
use dhampir_worker::baseline::open_leg;

const SIZE: u32 = 4;

/// 造一张「每一帧整体纯色、颜色按帧号变」的动图。
///
/// 每帧**整张画布一个颜色**是刻意的：读回一个像素就能认出是哪一帧，
/// 不必去比整张图的摘要 —— 判据越短，错的时候越好归因。
fn solid_animation(frame_count: usize, delay_ms: u32) -> Animation {
    let mut frames = Vec::with_capacity(frame_count);
    for index in 0..frame_count {
        let shade = (index as u8).wrapping_mul(40).wrapping_add(20);
        let mut rgba = Vec::with_capacity((SIZE * SIZE * 4) as usize);
        for _ in 0..(SIZE * SIZE) {
            rgba.extend_from_slice(&[shade, shade, shade, 255]);
        }
        frames.push(AnimFrame { delay_ms, rgba });
    }
    Animation {
        format: AnimFormat::Gif,
        width: SIZE,
        height: SIZE,
        loop_count: 0,
        total_ms: u64::from(delay_ms) * frame_count as u64,
        frames,
    }
}

/// 把一张纹理读回一个像素（左上角）。
fn top_left(ctx: &dhampir_core::gpu::GpuContext, view: &wgpu::TextureView) -> Vec<u8> {
    // 读回要一张**自己拥有**的纹理：TextureView 拿不到 CopySrc 的用法位，
    // 所以这里按 view 的尺寸重建一张、把 view 的内容画进去。
    let target = ctx.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("dhampir animation test readback"),
        size: wgpu::Extent3d {
            width: SIZE,
            height: SIZE,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let target_view = target.create_view(&wgpu::TextureViewDescriptor::default());
    let blit =
        dhampir_core::render::BlitRenderer::new(&ctx.device, wgpu::TextureFormat::Rgba8Unorm);
    let mut encoder = ctx
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("dhampir animation test encoder"),
        });
    blit.render(&ctx.device, &mut encoder, view, &target_view);
    ctx.queue.submit([encoder.finish()]);
    let image = pollster::block_on(readback::read_texture_rgba8(
        &ctx.device,
        &ctx.queue,
        &target,
    ))
    .expect("读回失败");
    image.pixels[..4].to_vec()
}

#[test]
#[ignore = "需要真 GPU；跑：cargo test -p dhampir-worker --test animation_gpu -- --ignored"]
fn 上传后按帧号取到的是不同的帧() {
    let (ctx, _init) = open_leg(NATIVE_BACKENDS).expect("拿不到 GPU 上下文");
    let mut cache = AnimationTextures::new(ctx.device.clone(), ctx.queue.clone(), 0);
    let animation = solid_animation(4, 30);
    cache.upload("sticker", &animation).expect("上传失败");

    assert_eq!(cache.frame_count("sticker"), Some(4));
    assert_eq!(cache.frame_count("sticker"), Some(animation.frames.len()));
    assert_eq!(
        cache.delays_ms("sticker"),
        Some(animation.delays_ms().as_slice())
    );
    assert_eq!(cache.memory_bytes(), u64::from(SIZE * SIZE * 4) * 4);

    // 每一帧的颜色都不同 —— 这是「帧号真的被用上了」的证据。
    let mut seen = Vec::new();
    for index in 0..4i64 {
        let (view, size) = cache.texture_for("sticker", index).expect("取不到帧");
        assert_eq!(size, (SIZE, SIZE));
        seen.push(top_left(&ctx, &view));
    }
    for (index, pixel) in seen.iter().enumerate() {
        assert_eq!(pixel[3], 255, "第 {index} 帧 alpha 不对");
    }
    let mut unique = seen.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), 4, "四帧读回的颜色有重复：帧号没有被用上");
}

#[test]
#[ignore = "需要真 GPU；跑：cargo test -p dhampir-worker --test animation_gpu -- --ignored"]
fn 越界停在最后一帧而未知资产取不到() {
    let (ctx, _init) = open_leg(NATIVE_BACKENDS).expect("拿不到 GPU 上下文");
    let mut cache = AnimationTextures::new(ctx.device.clone(), ctx.queue.clone(), 0);
    let animation = solid_animation(3, 30);
    cache.upload("sticker", &animation).expect("上传失败");

    let last = cache.texture_for("sticker", 2).expect("取不到最后一帧");
    let beyond = cache
        .texture_for("sticker", 99)
        .expect("越界应当停在最后一帧");
    assert_eq!(
        top_left(&ctx, &last.0),
        top_left(&ctx, &beyond.0),
        "越界没有停在最后一帧"
    );
    let negative = cache
        .texture_for("sticker", -5)
        .expect("负帧号应当落第零帧");
    let first = cache.texture_for("sticker", 0).expect("取不到第零帧");
    assert_eq!(
        top_left(&ctx, &negative.0),
        top_left(&ctx, &first.0),
        "负帧号没有落第零帧"
    );

    // 没登记过的资产必须取不到 —— 否则「引擎供帧」与「宿主供帧」会同时成立。
    assert!(cache.texture_for("nope", 0).is_none());
    assert!(!cache.contains("nope"));
    assert!(cache.remove("sticker"));
    assert!(cache.texture_for("sticker", 0).is_none());
}

#[test]
#[ignore = "需要真 GPU；跑：cargo test -p dhampir-worker --test animation_gpu -- --ignored"]
fn 超预算明确拒绝而不是悄悄吃显存() {
    let (ctx, _init) = open_leg(NATIVE_BACKENDS).expect("拿不到 GPU 上下文");
    // 预算只给 3 帧的字节：第 4 帧那次上传必须失败，而且**已经上传的保持不动**。
    let one_frame = u64::from(SIZE * SIZE * 4);
    let mut cache = AnimationTextures::new(ctx.device.clone(), ctx.queue.clone(), one_frame * 3);
    assert_eq!(cache.budget_bytes(), one_frame * 3);

    cache
        .upload("ok", &solid_animation(3, 30))
        .expect("三帧应当放得下");
    let error = cache
        .upload("too_big", &solid_animation(4, 30))
        .expect_err("四帧应当被拒");
    let message = error.to_string();
    assert!(
        message.contains("预算"),
        "拒绝理由没说是预算问题：{message}"
    );
    assert_eq!(
        cache.memory_bytes(),
        one_frame * 3,
        "被拒的那次不该动已上传的账"
    );
    assert!(!cache.contains("too_big"));

    // 替换成更小的应当成功，并且账目按替换后的算（不是累加）。
    cache
        .upload("ok", &solid_animation(2, 30))
        .expect("换小应当成功");
    assert_eq!(cache.memory_bytes(), one_frame * 2);
    assert_eq!(cache.frame_count("ok"), Some(2));
}

#[test]
#[ignore = "需要真 GPU；跑：cargo test -p dhampir-worker --test animation_gpu -- --ignored"]
fn 帧字节数与画布对不上时报错而不是传错位像素() {
    let (ctx, _init) = open_leg(NATIVE_BACKENDS).expect("拿不到 GPU 上下文");
    let mut cache = AnimationTextures::new(ctx.device.clone(), ctx.queue.clone(), 0);
    let mut animation = solid_animation(2, 30);
    animation.frames[1].rgba.truncate(4);
    let error = cache
        .upload("broken", &animation)
        .expect_err("对不上应当被拒");
    assert!(error.to_string().contains("对不上"), "理由不对：{error}");
    assert!(!cache.contains("broken"));
}

/// **真素材规模的形状判据**：所有帧进**一张** `D2Array` 纹理，而不是每帧一张。
///
/// # 为什么这条判据必须存在
///
/// 上传曾经是逐帧 `create_texture` + `write_texture`。两种宿主下**同一份 core 代码**：
///
/// | 500x500 x 77 帧 (73.4 MiB) | 耗时 |
/// |---|---|
/// | native（Vulkan 真机） | 6 ms |
/// | **浏览器（wasm/WebGPU）** | **1699 ms** |
///
/// 差 283 倍 —— 因为 WebGPU 后端每次 `write_texture` 都要跨 wasm↔JS 边界拷一趟，
/// 77 帧就是 77 趟。改成一张数组纹理后浏览器实测 **194 ms（8.8x）**，
/// 五张真贴纸合计 **4994 ms -> 461 ms（10.8x）**。
///
/// ⚠️ 这条跑在 native 上，**抓不住浏览器那 283 倍**（native 两种写法都快）。
/// 它抓的是**形状**：一旦有人把上传改回"每帧一张纹理"，帧号取值/预算/替换这几条
/// 会跟着一起漂，而那是共享的契约。浏览器侧的耗时证据在
/// `scripts/bench-animation-upload.mjs`（真素材，需 WebGPU）。
///
/// 尺寸取 500x500 x 77 = 真素材里最大的一张（一张 500x500、77 帧的 GIF），
/// 不是 `SIZE = 4` 那种玩具规模 —— 上面那个 218 ms 的旧数字就是被玩具规模掩盖的。
#[test]
#[ignore = "需要真 GPU；跑：cargo test -p dhampir-worker --test animation_gpu -- --ignored"]
fn 真素材规模的上传走一张数组纹理() {
    let (ctx, _init) = open_leg(NATIVE_BACKENDS).expect("拿不到 GPU 上下文");
    let (side, fc) = (500u32, 77usize);
    let mut frames = Vec::with_capacity(fc);
    for index in 0..fc {
        let shade = (index as u8).wrapping_mul(3).wrapping_add(20);
        let mut rgba = Vec::with_capacity((side * side * 4) as usize);
        for _ in 0..(side * side) {
            rgba.extend_from_slice(&[shade, shade, shade, 255]);
        }
        frames.push(AnimFrame { delay_ms: 30, rgba });
    }
    let animation = Animation {
        format: AnimFormat::Gif,
        width: side,
        height: side,
        loop_count: 0,
        total_ms: fc as u64 * 30,
        frames,
    };
    let mut cache = AnimationTextures::new(ctx.device.clone(), ctx.queue.clone(), 0);
    cache.upload("big", &animation).expect("上传失败");

    // 账目按帧数×画布算（不是按纹理张数）
    assert_eq!(cache.frame_count("big"), Some(fc));
    assert_eq!(cache.memory_bytes(), u64::from(side * side * 4) * fc as u64);

    // 帧号仍然各取各的：抽查首/中/尾三帧的颜色互不相同（证明层号真的接上了）
    let mut seen = Vec::new();
    for index in [0i64, (fc / 2) as i64, (fc - 1) as i64] {
        let (view, size) = cache.texture_for("big", index).expect("取不到帧");
        assert_eq!(size, (side, side));
        seen.push(top_left(&ctx, &view));
    }
    assert_ne!(
        seen[0], seen[1],
        "第 0 帧与中间帧是同一个像素 —— 层号没接上"
    );
    assert_ne!(
        seen[1], seen[2],
        "中间帧与最后一帧是同一个像素 —— 层号没接上"
    );

    // 越界仍然停在最后一帧（不与上面那条重复：这里走的是钳制那条路）
    let (last, _) = cache
        .texture_for("big", 9999)
        .expect("越界应当钳到最后一帧");
    assert_eq!(top_left(&ctx, &last), seen[2]);
}
