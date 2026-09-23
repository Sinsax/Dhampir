//! 按时间线把一帧画出来 —— **两个宿主都调它**。
//!
//! # 为什么这里必须有「一个入口」
//!
//! 双端比对的结论，只有在**两边跑的是同一份代码**时才有意义。
//! 如果 wasm 宿主与 worker 各写一遍「先模糊再合成」的调度，那比出来的差异
//! 既可能是运行时差异、也可能是两份驱动的写法差异——后者是纯粹的浪费。
//! 所以调度（谁先谁后、中间纹理从哪来）放在这里；宿主只提供两样东西：
//! 一块源纹理、一个能画的目标。
//!
//! # v1 的做法
//!
//! 1. 先给带特效的图层做模糊，各自画进一块临时纹理；
//! 2. 再把所有图层按顺序交给合成节点叠一次。
//!
//! 第 1 步每帧每层新建两块临时纹理——**这是刻意选笨的**：
//! 纹理池是宿主的资源策略（显存预算、LRU），不该埋在这一层里。
//! 等两端都跑通了再把它提到宿主去，那时换的是宿主，不是这里的语义。

use dhampir_timeline::schema::Effect;

use crate::compose::Composite;
use crate::render::blur::BlurRenderer;
use crate::render::compose::{Compositor, LayerDraw};
use crate::wgpu;

/// 源纹理的提供者。宿主实现它——浏览器那边是 video 元素，native 那边是解码器或文件。
pub trait SourceResolver {
    /// 给出这个 source 在**这一帧**上对应的纹理与尺寸。给不出来就返回 None（该层会被跳过）。
    ///
    /// `source_frame` **必须**在参数里：这是「帧号精确」这条铁律在宿主接缝上的兑现。
    /// 只给 source 名字的话，宿主只能按名字返回一张静态纹理——
    /// 于是源内帧号被悄悄忽略，而画面看起来完全正常。
    /// （这个洞是被样本工程比出来的：不同帧算出的摘要一模一样。）
    fn texture_for(
        &mut self,
        source: &str,
        source_frame: i64,
    ) -> Option<(wgpu::TextureView, (u32, u32))>;
}

/// 图层要用的模糊半径。0 表示这层不需要模糊。
///
/// 半径会被夹到 [`crate::render::BLUR_MAX_RADIUS`]：登记表已经限制了取值范围，
/// 这里再兜一次，免得一个绕过校验的工程让着色器索引越界。
pub fn blur_radius(effects: &[Effect]) -> u32 {
    let mut radius = 0.0_f32;
    for effect in effects {
        if effect.kind != "gaussian_blur" {
            continue;
        }
        if let Some(value) = effect.params.get("radius") {
            if value.is_finite() && *value > radius {
                radius = *value;
            }
        }
    }
    // u32 -> f32 没有 From 实现，只能 as；这里范围远小于 2^24，转换是精确的。
    radius.max(0.0).round().min(crate::render::BLUR_MAX_RADIUS as f32) as u32
}

/// 时间线渲染器：合成 + 特效的调度。构造一次、每帧复用。
pub struct TimelineRenderer {
    compositor: Compositor,
    blur: BlurRenderer,
    format: wgpu::TextureFormat,
}

/// 固定源解析器：永远返回同一张纹理。
///
/// 分段合成末尾要把结果从中间纹理搬到 target，而 compose_layers 的纹理来自解析器 ——
/// 给它一个「永远是这张」的解析器，就得到一次搬运。
struct FixedSource<'a> {
    view: &'a wgpu::TextureView,
    size: (u32, u32),
}

impl SourceResolver for FixedSource<'_> {
    fn texture_for(
        &mut self,
        _source: &str,
        _source_frame: i64,
    ) -> Option<(wgpu::TextureView, (u32, u32))> {
        Some((self.view.clone(), self.size))
    }
}

/// 一次「原样搬运」用的图层：没有变换、完全不透明。
fn identity_layer() -> crate::compose::Layer {
    crate::compose::Layer {
        clip_id: String::new(),
        source: String::new(),
        source_frame: 0,
        opacity: 1.0,
        transform: dhampir_timeline::schema::Transform { x: 0.0, y: 0.0, scale: 1.0, rotation_deg: 0.0 },
        effects: Vec::new(),
        frozen_for_transition: false,
        blend: dhampir_timeline::layer::BlendMode::Normal,
        is_adjustment: false,
    }
}

impl TimelineRenderer {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        Self {
            compositor: Compositor::new(device, format),
            blur: BlurRenderer::new(device, format),
            format,
        }
    }

    /// 画一帧，返回**实际画了几层**。
    ///
    /// 源解析不出来时跳过那一层而不是整帧失败：一行轨道的素材暂时没准备好，
    /// 不该让整帧变黑——预览里那表现为"闪一下"，比少一层更烦人。
    /// 画一帧，返回**实际画了几层**。
    ///
    /// 源解析不出来时跳过那一层而不是整帧失败：一行轨道的素材暂时没准备好，
    /// 不该让整帧变黑 —— 预览里那表现为「闪一下」，比少一层更烦人。
    ///
    /// **它是薄包装**：真正的合成在 compose_layers 里。
    /// 分段合成（下一步）会直接调 compose_layers 并传中间纹理，
    /// 所以那个函数一出生就有两个调用方，不会成为「写了没人用」的代码。
    #[allow(clippy::too_many_arguments)]
    pub fn render_frame(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        target_size: (u32, u32),
        composite: &Composite,
        resolver: &mut dyn SourceResolver,
        clear: wgpu::Color,
    ) -> usize {
        // **先看分段计划。** 没有调整图层就走原来那条单 pass 路（行为逐字节不变）；
        // 有的话要「先合成一段 -> 对结果跑特效 -> 再继续」，那需要中间纹理。
        let plan = plan_steps(&composite.layers);
        if !plan.iter().any(|step| matches!(step, Step::Adjust { .. })) {
            return self.compose_layers(
                device,
                queue,
                encoder,
                target,
                target_size,
                &composite.layers,
                resolver,
                Some(clear),
            );
        }
        self.render_segmented(
            device, queue, encoder, target, target_size, composite, resolver, clear, &plan,
        )
    }

    /// 分段合成：调整图层要求「先合成一段 -> 对结果跑特效 -> 再继续」。
    ///
    /// **纹理全部存进 Vec，用下标指代「当前底」** —— 不用引用。
    /// 用引用的话，循环里新建的视图会与「当前底」的借用冲突，而绕开它需要
    /// 泄漏或索引体操；存进 Vec 之后借用关系是自然的，代价只是多几张纹理。
    /// **先要正确性，纹理池化留到后面。**
    #[allow(clippy::too_many_arguments)]
    fn render_segmented(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        target_size: (u32, u32),
        composite: &Composite,
        resolver: &mut dyn SourceResolver,
        clear: wgpu::Color,
        plan: &[Step],
    ) -> usize {
        let extent = wgpu::Extent3d {
            width: target_size.0.max(1),
            height: target_size.1.max(1),
            depth_or_array_layers: 1,
        };
        let mut textures: Vec<wgpu::Texture> = Vec::new();
        let mut views: Vec<wgpu::TextureView> = Vec::new();
        // **写成显式传参的函数，不用闭包。**
        // 闭包会在整个作用域内独占 views/textures 的可变借用，
        // 而循环里还要读 views（拿当前底）—— 那样根本编译不过。
        fn allocate(
            device: &wgpu::Device,
            format: wgpu::TextureFormat,
            extent: wgpu::Extent3d,
            label: &'static str,
            textures: &mut Vec<wgpu::Texture>,
            views: &mut Vec<wgpu::TextureView>,
        ) -> usize {
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: extent,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: format,
                usage: wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            });
            views.push(texture.create_view(&wgpu::TextureViewDescriptor::default()));
            textures.push(texture);
            views.len() - 1
        }

        let mut current: Option<usize> = None;
        let mut drawn = 0usize;

        for step in plan {
            match step {
                Step::Draw(indices) => {
                    let layers: Vec<crate::compose::Layer> = indices
                        .iter()
                        .map(|index| composite.layers[*index].clone())
                        .collect();
                    let dest = match current {
                        Some(index) => index,
                        None => allocate(device, self.format, extent, "dhampir segment first", &mut textures, &mut views),
                    };
                    let base = views[dest].clone();
                    let fresh = current.is_none();
                    drawn += self.compose_layers(
                        device,
                        queue,
                        encoder,
                        &base,
                        target_size,
                        &layers,
                        resolver,
                        if fresh { Some(clear) } else { None },
                    );
                    current = Some(dest);
                }
                Step::Adjust { effects, .. } => {
                    let radius = blur_radius(effects);
                    let Some(from) = current else { continue };
                    if radius == 0 {
                        continue;
                    }
                    // blur_separable 需要一张中间纹理与一张输出纹理（它自己是一横一纵两趟）。
                    let middle = allocate(
                        device, self.format, extent, "dhampir adjust middle", &mut textures, &mut views,
                    );
                    let out = allocate(
                        device, self.format, extent, "dhampir adjust out", &mut textures, &mut views,
                    );
                    let source = views[from].clone();
                    let mid = views[middle].clone();
                    let to = views[out].clone();
                    self.blur.blur_separable(
                        device, queue, encoder, &source, &mid, &to, target_size, radius,
                    );
                    current = Some(out);
                }
            }
        }

        // **把结果落回 target。** 用现成的 compose_layers 加一个固定源解析器即可 ——
        // 为一次搬运引入 blit 渲染器不值得。
        let Some(result) = current else {
            return 0;
        };
        let result_view = views[result].clone();
        let mut fixed = FixedSource {
            view: &result_view,
            size: target_size,
        };
        let blit = [identity_layer()];
        drawn += self.compose_layers(
            device, queue, encoder, target, target_size, &blit, &mut fixed, Some(clear),
        );
        drop(textures);
        drawn
    }

    /// 把这几层合成到 dest，返回**实际画了几层**。
    ///
    /// **这是分段合成的可复用入口**：现有路径传 target 与 Some(clear)；
    /// 分段路径（下一步）传中间纹理与 None。
    #[allow(clippy::too_many_arguments)]
    fn compose_layers(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        dest: &wgpu::TextureView,
        dest_size: (u32, u32),
        layers: &[crate::compose::Layer],
        resolver: &mut dyn SourceResolver,
        clear: Option<wgpu::Color>,
    ) -> usize {
        // **先看分段计划。** 调整图层要求「先合成一部分 -> 对结果跑特效 -> 再继续」，
        // 那需要中间纹理与多次 pass，而这里现在只有一次 pass。
        //
        // 在把那段实现出来之前，遇到调整图层就**整帧不画并返回 0**：
        // 返回 0 是调用方看得见的信号（画面空），好过悄悄画一张
        // 「特效没生效但看不出哪里不对」的图。
        //
        // 这正是本项目一以贯之的取舍：**明确失败优于静默降级。**
        let plan = plan_steps(layers);
        if plan.iter().any(|step| matches!(step, Step::Adjust { .. })) {
            // **不要用 debug_assert**：那会让这条路径在 debug 下 panic、在 release 下返回 0，
            // 同一条路径两种行为。而 wasm-pack --dev 就是 debug 构建 ——
            // 于是浏览器里会直接崩掉一整个模块，而不是「这一帧没画」。
            // 统一返回 0：调用方看得见（画面空），且两种构建下一致。
            return 0;
        }

        // 先把每层要采的纹理备好：需要模糊的层先画进临时纹理。
        // 临时纹理与视图都要活到 compose 之后，所以放在这两个 Vec 里。
        let mut keep_alive: Vec<wgpu::Texture> = Vec::new();
        // **把图层与它的纹理配成一对**，而不是分两个 Vec 靠下标对齐。
        //
        // 分两个 Vec 会有一个很安静的错：源解析不出来的层会被 continue 掉，
        // 于是 prepared 比 composite.layers 短，下面那个 zip 就**错位**了 ——
        // 结果是 **B 层的纹理配上 A 层的变换**。画面会错，但不崩、也不报错。
        // 配成对之后，错位在类型上就不可能发生。
        #[allow(clippy::type_complexity)]
        let mut prepared: Vec<(&crate::compose::Layer, wgpu::TextureView, (u32, u32))> =
            Vec::new();

        for layer in layers {
            let Some((view, size)) = resolver.texture_for(&layer.source, layer.source_frame) else {
                continue;
            };
            let radius = blur_radius(&layer.effects);
            if radius == 0 {
                prepared.push((layer, view, size));
                continue;
            }
            let blurred = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("dhampir timeline blur target"),
                size: wgpu::Extent3d {
                    width: size.0.max(1),
                    height: size.1.max(1),
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: self.format,
                usage: wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            });
            let intermediate = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("dhampir timeline blur intermediate"),
                size: wgpu::Extent3d {
                    width: size.0.max(1),
                    height: size.1.max(1),
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: self.format,
                usage: wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            });
            let blurred_view = blurred.create_view(&wgpu::TextureViewDescriptor::default());
            let intermediate_view =
                intermediate.create_view(&wgpu::TextureViewDescriptor::default());
            self.blur.blur_separable(
                device,
                queue,
                encoder,
                &view,
                &intermediate_view,
                &blurred_view,
                size,
                radius,
            );
            keep_alive.push(blurred);
            keep_alive.push(intermediate);
            prepared.push((layer, blurred_view, size));
        }

        // 不再 zip 两个序列 —— 直接从成对的 prepared 来，错位不可能发生。
        let draws: Vec<LayerDraw<'_>> = prepared
            .iter()
            .map(|(layer, view, size)| LayerDraw {
                view,
                source_size: *size,
                transform: layer.transform,
                opacity: layer.opacity,
                blend: layer.blend,
            })
            .collect();

        self.compositor.compose(
            device,
            queue,
            encoder,
            dest,
            dest_size,
            &draws,
            clear,
        );

        // 让编译器和读者都看得见这些纹理活到了这里。
        drop(keep_alive);
        draws.len()
    }
}

/// 由源标识推出一个稳定的 seed。
///
/// 两端对同一个 source 名字必须生成**同一张**源图，所以这个映射必须是纯函数，
/// 而且不能依赖任何运行时的哈希实现（Rust 的 DefaultHasher 每次进程启动都可能不同）。
/// 这里用 FNV-1a 32 位——写死算法，换版本也不会变。
pub fn synthetic_seed_for_source(source: &str) -> u32 {
    let mut hash = 0x811c_9dc5_u32;
    for byte in source.as_bytes() {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

/// 源内**帧号**也要参与 seed：否则同一素材的每一帧长得一模一样，
/// 「帧号精确」这件事在比对里就完全压不到（样本工程第一次跑出来的
/// 不同帧摘要相同，就是这个原因）。
pub fn synthetic_seed_for_source_frame(source: &str, frame: i64) -> u32 {
    synthetic_seed_for_source(source) ^ (frame as u32).wrapping_mul(0x9e37_79b9)
}

/// 确定性源图：两端调**同一个函数**生成源像素，于是输入逐字节相同。
///
/// 它同时是"--dump-raw"思路的极简版：要隔离"渲染差异"，就必须先让**输入**相同。
/// 在高频方块上叠一层水平渐变，是为了同时压住两种错误：
/// 采样错位（方块边界会立刻暴露）与缩放/滤波偷懒（渐变会暴露）。
pub fn synthetic_source_rgba8(width: u32, height: u32, seed: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            let checker = if ((x / 8) + (y / 8) + seed) % 2 == 0 { 220 } else { 40 };
            let gradient = ((x * 255) / width.max(1)) as u8;
            let vertical = ((y * 255) / height.max(1)) as u8;
            let r = checker;
            let g = gradient;
            let b = vertical;
            let a = if (x / 16 + y / 16) % 3 == 0 { 255 } else { 200 };
            out.extend_from_slice(&[r, g, b, a]);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn effect(kind: &str, params: &[(&str, f32)]) -> Effect {
        Effect {
            kind: kind.to_string(),
            params: params.iter().map(|(key, value)| (key.to_string(), *value)).collect(),
        }
    }

    #[test]
    fn 没有特效就没有模糊() {
        assert_eq!(blur_radius(&[]), 0);
        assert_eq!(blur_radius(&[effect("sharpen", &[("radius", 8.0)])]), 0, "不认识的特效不该触发模糊");
        assert_eq!(blur_radius(&[effect("gaussian_blur", &[("sigma", 8.0)])]), 0, "没有 radius 参数就当 0");
    }

    #[test]
    fn 半径被夹到上限且四舍五入() {
        assert_eq!(blur_radius(&[effect("gaussian_blur", &[("radius", 3.0)])]), 3);
        assert_eq!(blur_radius(&[effect("gaussian_blur", &[("radius", 2.6)])]), 3);
        assert_eq!(
            blur_radius(&[effect("gaussian_blur", &[("radius", 999.0)])]),
            crate::render::BLUR_MAX_RADIUS,
            "超过上限要夹住，不能把索引交给着色器"
        );
        assert_eq!(blur_radius(&[effect("gaussian_blur", &[("radius", -5.0)])]), 0);
        assert_eq!(blur_radius(&[effect("gaussian_blur", &[("radius", f32::NAN)])]), 0, "NaN 不能穿过去");
    }

    #[test]
    fn 多个特效取最大的半径() {
        let effects = vec![
            effect("gaussian_blur", &[("radius", 2.0)]),
            effect("gaussian_blur", &[("radius", 6.0)]),
        ];
        assert_eq!(blur_radius(&effects), 6);
    }

    #[test]
    fn 源图确定且不退化() {
        let a = synthetic_source_rgba8(64, 32, 1);
        let b = synthetic_source_rgba8(64, 32, 1);
        assert_eq!(a, b, "同一个 seed 必须逐字节相同——否则双端比的就不是渲染差异");
        assert_eq!(a.len(), 64 * 32 * 4);
        assert_ne!(a, synthetic_source_rgba8(64, 32, 2), "换 seed 该变");
        let first = &a[..4];
        assert!(a.chunks(4).any(|px| px != first), "源图不能是纯色，否则比不出采样错误");
        // 透明度也要有变化：不然"合成"这件事根本没被压到
        let first_alpha = a[3];
        assert!(a.chunks(4).any(|px| px[3] != first_alpha), "源图的不透明度应当有变化");
    }

    #[test]
    fn 尺寸为零时返回空() {
        assert!(synthetic_source_rgba8(0, 0, 0).is_empty());
    }
}

/// 渲染图的**分段计划**。
///
/// # 为什么需要它
///
/// 调整图层要求「先合成一部分 → 对结果跑特效 → 再继续」，而**一次 render pass 表达不了**
/// 这件事。所以必须先把图层清单切成段：
///
/// ```text
/// 原来： [层1][层2][调整A][层3]
/// 改成： Draw(0,1) → Adjust(2) → Draw(3)
///        段1 合成到中间纹理 t1
///        t1 过 A 的特效 → t2
///        t2 + 层3 → 最终
/// ```
///
/// # 为什么是「下标」而不是复制图层
///
/// 计划是**纯数据**：只描述「按什么顺序做什么」，不持有任何 GPU 资源。
/// 渲染时才根据它去建纹理、切 pass。这样计划本身可以脱离 GPU 单测。
#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    /// 把这几层依次画到当前底上（下标指向原清单）。
    Draw(Vec<usize>),
    /// 对**当前已经画好的结果**跑这一层（调整图层）的特效。
    Adjust {
        layer: usize,
        effects: Vec<dhampir_timeline::schema::Effect>,
    },
}

/// 把图层清单切成渲染步骤。
///
/// 语义上有一处刻意的地方：**调整图层永远不会出现在 `Draw` 里**。
/// 它没有素材、不产生像素，只对「已经画上去的东西」施加影响 ——
/// 如果把它塞进 Draw，渲染器就得在 `Draw` 里再判断一次「这层到底要不要采素材」，
/// 而那正是「用 source 是否为空来区分」那种隐晦写法的来源。
pub fn plan_steps(layers: &[crate::compose::Layer]) -> Vec<Step> {
    let mut steps = Vec::new();
    let mut pending: Vec<usize> = Vec::new();

    for (index, layer) in layers.iter().enumerate() {
        if layer.is_adjustment {
            if !pending.is_empty() {
                steps.push(Step::Draw(std::mem::take(&mut pending)));
            }
            steps.push(Step::Adjust {
                layer: index,
                effects: layer.effects.clone(),
            });
        } else {
            pending.push(index);
        }
    }
    if !pending.is_empty() {
        steps.push(Step::Draw(pending));
    }
    steps
}

#[cfg(test)]
mod plan_tests {
    use super::*;
    use dhampir_timeline::layer::BlendMode;
    use dhampir_timeline::schema::{Effect, Transform};

    fn layer(id: &str, adjustment: bool) -> crate::compose::Layer {
        crate::compose::Layer {
            clip_id: id.to_string(),
            source: if adjustment { String::new() } else { format!("{id}.mp4") },
            source_frame: 0,
            opacity: 1.0,
            transform: Transform { x: 0.0, y: 0.0, scale: 1.0, rotation_deg: 0.0 },
            effects: if adjustment {
                vec![Effect { kind: "gaussian_blur".to_string(), params: Default::default() }]
            } else {
                Vec::new()
            },
            frozen_for_transition: false,
            blend: BlendMode::Normal,
            is_adjustment: adjustment,
        }
    }

    #[test]
    fn 没有调整图层时就是一个_draw() {
        let layers = vec![layer("a", false), layer("b", false)];
        assert_eq!(plan_steps(&layers), vec![Step::Draw(vec![0, 1])]);
    }

    #[test]
    fn 调整图层把清单切成三段() {
        let layers = vec![layer("a", false), layer("b", false), layer("adj", true), layer("c", false)];
        let steps = plan_steps(&layers);
        assert_eq!(steps.len(), 3);
        assert_eq!(steps[0], Step::Draw(vec![0, 1]));
        match &steps[1] {
            Step::Adjust { layer: index, effects } => {
                assert_eq!(*index, 2);
                assert_eq!(effects.len(), 1, "调整图层的特效要跟着计划走");
            }
            other => panic!("第二段应当是 Adjust，得到 {other:?}"),
        }
        assert_eq!(steps[2], Step::Draw(vec![3]));
    }

    #[test]
    fn 调整图层在最后时它影响的是前面全部() {
        let layers = vec![layer("a", false), layer("b", false), layer("adj", true)];
        let steps = plan_steps(&layers);
        assert_eq!(steps[0], Step::Draw(vec![0, 1]));
        assert!(matches!(steps[1], Step::Adjust { layer: 2, .. }));
    }

    #[test]
    fn 调整图层在最前时下面是空的() {
        // 退化情形：它下面什么都没有。计划照常给出 Adjust，
        // 渲染器要对**空底**跑特效 —— 不崩、也不该凭空产生内容。
        let layers = vec![layer("adj", true), layer("a", false)];
        let steps = plan_steps(&layers);
        assert!(matches!(steps[0], Step::Adjust { layer: 0, .. }));
        assert_eq!(steps[1], Step::Draw(vec![1]));
    }

    #[test]
    fn 每一层恰好被计划一次() {
        // **这条是最有价值的不变量**：漏一层 = 画面少东西，
        // 多一次 = 同一层被画两遍（叠加会变浓）。两种都不会崩，只会画错。
        let layers = vec![
            layer("a", false),
            layer("adj1", true),
            layer("b", false),
            layer("c", false),
            layer("adj2", true),
            layer("d", false),
        ];
        let steps = plan_steps(&layers);
        let mut seen: Vec<usize> = Vec::new();
        for step in &steps {
            match step {
                Step::Draw(indices) => seen.extend(indices),
                Step::Adjust { layer, .. } => seen.push(*layer),
            }
        }
        seen.sort_unstable();
        assert_eq!(seen, vec![0, 1, 2, 3, 4, 5], "每一层都要恰好出现一次");
    }

    #[test]
    fn 调整图层绝不出现在_draw_里() {
        // 它没有素材、不产生像素。塞进 Draw 会逼渲染器在 Draw 里再判断一次
        // 「这层要不要采素材」—— 那正是隐晦写法的来源。
        let layers = vec![layer("a", false), layer("adj", true), layer("b", false)];
        for step in plan_steps(&layers) {
            if let Step::Draw(indices) = step {
                for index in indices {
                    assert!(!layers[index].is_adjustment, "第 {index} 层是调整图层，不该进 Draw");
                }
            }
        }
    }
}
