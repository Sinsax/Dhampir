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

use dhampir_timeline::schema::{Effect, EffectPipeline};

use crate::compose::Composite;
use crate::render::blur::BlurRenderer;
use crate::render::color_adjust::ColorAdjustRenderer;
use crate::render::color_mask::ColorMaskRenderer;
use crate::render::warp::WarpRenderer;
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
///
/// # 为什么这里查注册表而不是比较字符串
///
/// 原先写的是 `effect.kind != "gaussian_blur"` —— 一个字符串字面量。
/// 那意味着**再加一个走 SeparableBlur 管线的特效时，这里会被静默跳过**：
/// 用户填了半径、校验也过了（登记表里有它），但渲染当没看见。
///
/// 改成按 `spec.pipeline` 认，加特效就只需要在登记表里声明管线，
/// 不必回到渲染主路径里补一个分支 —— 而"忘了补"正是这类硬编码最容易出的错。
pub fn blur_radius(effects: &[Effect]) -> u32 {
    let mut radius = 0.0_f32;
    for effect in effects {
        // 认管线，不认名字。名字认不出来的（没登记的）直接跳过 ——
        // 校验层已经会为"未登记的特效"报错，这里不重复报，只保证不崩。
        let Some(spec) = crate::effects::spec_of(&effect.kind) else {
            continue;
        };
        if spec.pipeline != EffectPipeline::SeparableBlur {
            continue;
        }
        if let Some(value) = effect.params.get("radius") {
            if value.is_finite() && *value > radius {
                radius = *value;
            }
        }
    }
    // 上界**从登记表推**，不写死：写死的话改一次上界就要改这里。
    // 登记表里查不到 radius 时退回着色器的硬上界（有测试盯着登记表必须有它）。
    let max = crate::effects::spec_of("gaussian_blur")
        .and_then(|spec| spec.param_max("radius"))
        .unwrap_or(crate::render::BLUR_MAX_RADIUS as f32);
    // u32 -> f32 没有 From 实现，只能 as；这里范围远小于 2^24，转换是精确的。
    radius.max(0.0).round().min(max) as u32
}

/// 按特效**声明的空间**把半径换算到目标像素。
///
/// # 这个函数在解决什么
///
/// 同一个 `gaussian_blur` 在两个地方跑，而**它们需要的换算相反**：
///
/// | 跑在哪 | 纹素尺寸 | 半径含义 | 要不要换算 |
/// |---|---|---|---|
/// | 实拍片段（`compose_layers`） | 源纹理 | 源像素 | **不换算** |
/// | 调整图层（`Step::Adjust`） | 目标尺寸中间纹理 | 文档像素 | **要换算** |
///
/// 登记表只能给一个 `space`，所以"声明一个值"这件事本身就表达不了这个二义性。
/// 与其让登记表去猜，不如**把空间作为调用点的参数**：调用点自己最清楚
/// 它拿到的纹理是什么尺寸 —— 那是它构造出来的，不是从注册表读来的。
///
/// 登记表的 `space` 因此含义收窄为：**这个特效默认跑在哪个空间**，
/// 用于 UI 提示与"调用点没显式指定时的兜底"。真正的判据由调用点给出。
///
/// 这样声明错的后果也不再是静默分叉：调用点传什么，就按什么算，
/// 而两处调用点各自都有测试钉着（见 blur_radius_for_* 的用例）。
pub fn scale_document_radius(radius: u32, space: crate::render::RenderSpace) -> u32 {
    if radius == 0 {
        return 0;
    }
    let (sx, _) = space.pixel_scale();
    if !sx.is_finite() || sx <= 0.0 {
        return radius;
    }
    // 夹回核表上界：核是**定长展开**的（TAPS 个抽头），超了不会更糊，只会悄悄退化。
    let scaled = (radius as f32 * sx).round();
    scaled.max(0.0).min(crate::render::BLUR_MAX_RADIUS as f32) as u32
}

/// 按**调用点声明的空间**决定要不要换算半径。
///
/// 这是本段（S6）要的那个入口：把"跑在哪个空间"从登记表的单值声明，
/// 变成调用点必须显式回答的问题。两个调用点：
///
/// * 实拍片段那条路传 [`EffectSpace::Source`] —— 纹理就是源尺寸，半径是源像素；
/// * 调整图层那条路传 [`EffectSpace::Document`] —— 纹理是目标尺寸，半径是文档像素。
///
/// 传错的后果是**看得见的**：预览与成片尺寸不同时，模糊程度会明显不一样。
/// 而旧的写法（登记表单值 + 两处硬编码行为）会让其中一处永远错、且无人察觉。
pub fn radius_in_space(
    radius: u32,
    space: dhampir_timeline::schema::EffectSpace,
    render_space: crate::render::RenderSpace,
) -> u32 {
    match space {
        // 源空间：不按比例换算，但**仍然要夹上界**。
        //
        // 这一条是实测发现的：最初这里直接写 => radius，看起来"原样用"很合理，
        // 但它绕过了 scale_document_radius 里那道夹取 —— 于是一个绕过校验的工程
        // （radius 填 999）会让着色器按 999 去索引一张只有 TAPS 个抽头的核。
        // 核是**定长展开**的，越界不会崩，只会读到垃圾或悄悄退化。
        // 结论：**"不换算"不等于"不设防"**，两件事要分开写。
        dhampir_timeline::schema::EffectSpace::Source => {
            radius.min(crate::render::BLUR_MAX_RADIUS)
        }
        // 文档空间：按目标/文档比例换算（内部已含夹取）。
        dhampir_timeline::schema::EffectSpace::Document => {
            scale_document_radius(radius, render_space)
        }
    }
}

/// 从特效表里折出逐像素色彩调整的系数。
///
/// # 为什么是「折叠」而不是「取第一个」
///
/// 用户可能同时挂亮度和饱和度。取第一个会让后一个被静默丢掉 ——
/// 而"我明明挂了饱和度"这种问题很难查。这里选择**相乘/相加累积**：
/// 亮度相加、对比度与饱和度相乘、色调相加，语义上都是可交换的，
/// 所以多个同类特效叠加的结果与顺序无关（这点很重要：顺序无关才不会被
/// 两端各自的遍历顺序影响）。
///
/// 没挂任何色彩特效时返回 [`ColorAdjustParams::IDENTITY`]，调用方据此整条跳过。
pub fn color_params(effects: &[Effect]) -> crate::render::ColorAdjustParams {
    let mut out = crate::render::ColorAdjustParams::IDENTITY;
    for effect in effects {
        // 认管线，不认名字 —— 与 blur_radius 同一套判据。
        let Some(spec) = crate::effects::spec_of(&effect.kind) else {
            continue;
        };
        if spec.pipeline != dhampir_timeline::schema::EffectPipeline::ColorAdjust {
            continue;
        }
        let param = |name: &str| effect.params.get(name).copied().filter(|v| v.is_finite());
        match spec.kind {
            "brightness" => {
                if let Some(v) = param("amount") {
                    out.brightness += v;
                }
            }
            "contrast" => {
                if let Some(v) = param("amount") {
                    out.contrast *= v;
                }
            }
            "saturation" => {
                if let Some(v) = param("amount") {
                    out.saturation *= v;
                }
            }
            "hue" => {
                if let Some(v) = param("degrees") {
                    // **度转弧度只在这一处发生。**
                    // 着色器收的是弧度；两边各转一遍会让 90 度变成 90 弧度再转一次。
                    out.hue += v.to_radians();
                }
            }
            // 走了 ColorAdjust 管线却不在这里 -> 登记表加了新特效但忘了接上。
            // **不静默忽略**：那正是最坏的情形（用户能选中它，画面却不变）。
            other => {
                debug_assert!(false, "ColorAdjust 管线里的 {other} 没有在 color_params 里接上");
            }
        }
    }
    out
}
/// 一条特效 pass 的执行序。**顺序是正确性知识，所以它是数据、有测试。**
///
/// # 为什么顺序不能靠"代码里那么写的"
///
/// 在这条成为函数之前，顺序活在 `Step::Adjust` 分支体里的一段注释 +
/// 两个写死的代码块（"先色彩调整、再模糊"）。那段注释是对的，
/// 但**加一条新管线就要重新论证一次顺序**，而重新论证过的人未必会去读那段注释。
///
/// 现在的规则：**按算子性质分三级**，级内顺序无关（各自可交换）。
///
/// 1. [`PassStage::PerPixel`]：逐像素。输出只看自己，最先做 ——
///    "先决定这张图长什么样，再去动它"。
/// 2. [`PassStage::Warp`]：坐标重映射。它改变"哪个像素在哪"，
///    必须在逐像素调整**之后**（否则调的是被搬动过的像素，用户看到的是位移后的颜色）。
/// 3. [`PassStage::Neighborhood`]：邻域。模糊会把边缘摊开，
///    放最后是为了不让邻域噪声盖住前面两级的判定。
///
/// 为什么模糊不在 Warp 之前：Warp 的位移场是按**目标坐标**算的，
/// 先模糊再位移会连位移场的边界一起糊掉，两端在边界处的差异会被放大。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PassStage {
    /// 逐像素色彩调整与常量色叠加（ColorAdjust + ColorMask）。
    PerPixel = 0,
    /// 坐标重映射（Warp）。
    Warp = 1,
    /// 邻域算子（SeparableBlur）。
    Neighborhood = 2,
}

impl PassStage {
    /// 这条管线属于哪一级。**穷尽 match**：加了管线却不给分级会编译不过，
    /// 而不是静默落进某一级（顺序错了只会表现为"看起来有点怪"，最难查）。
    pub const fn of(pipeline: EffectPipeline) -> Self {
        match pipeline {
            EffectPipeline::ColorAdjust | EffectPipeline::ColorMask => Self::PerPixel,
            EffectPipeline::Warp => Self::Warp,
            EffectPipeline::SeparableBlur => Self::Neighborhood,
        }
    }
}

/// 特效的**执行批次**：按 `PassStage` 分组并在组内保持原有的相对次序。
///
/// 组内保持原序（稳定分区，不是排序）：用户挂了 `brightness` 又挂 `saturation` 时，
/// 两者的相对次序应当与写下的次序一致 —— 虽然这两条数学上可交换，
/// 但**把"可交换"当成"可以随便重排"是两回事**：将来加一条不可交换的
/// 逐像素算子时，稳定分区能保住语义。
pub fn effect_passes(effects: &[Effect]) -> Vec<(PassStage, Vec<usize>)> {
    let mut groups: Vec<(PassStage, Vec<usize>)> = Vec::new();
    for (index, effect) in effects.iter().enumerate() {
        let Some(spec) = crate::effects::spec_of(&effect.kind) else {
            continue;
        };
        let stage = PassStage::of(spec.pipeline);
        match groups.iter_mut().find(|(s, _)| *s == stage) {
            Some((_, indices)) => indices.push(index),
            None => groups.push((stage, vec![index])),
        }
    }
    // 按级的枚举值排 —— 级的定义就是执行序。
    groups.sort_by_key(|(stage, _)| *stage);
    groups
}

/// 把这一批 ColorMask 特效折叠成一组系数。
///
/// # 为什么"折叠"而不是"取第一个"
///
/// 与 `color_params` 同一条理由：用户可能同时挂闪白和暗角。
/// 取第一个会让后一个被静默丢掉 —— 而"我明明挂了暗角"很难查。
///
/// # 逐项的合成方式
///
/// - 闪白与覆盖层是**同类**（都是"向一个颜色插值"），所以按
///   `1 - (1-a)(1-b)` 合成强度（两次覆盖比一次覆盖更强，但不会超过 1）；
/// - 暗角与噪声是**独立通道**，各取所见到的最大值（同种特效挂两条时，
///   更强的那条说了算 —— 叠加两次暗角没有物理意义）。
///
/// `size` 是目标像素尺寸（暗角的椭圆坐标要用），`frame` 是**绝对帧号**
/// （噪声要它才逐帧不同，且同一帧可复现）。
///
/// # 强度来自 `Effect.opacity`
///
/// 时间窗**不在这里算** —— 求值层已经把它折进 `opacity` 了
/// （见 `compose::resolve_effects` 的说明）。所以这里只读 `opacity`，
/// 而它已经是"这一帧这条特效该有多强"。一条特效一个通用字段，
/// 而不是给四套管线各加一个 `envelope` 参数。
pub fn color_mask_params(
    effects: &[Effect],
    size: (u32, u32),
    frame: i64,
) -> crate::render::ColorMaskParams {
    use crate::render::OverlayShape;

    let mut out = crate::render::ColorMaskParams::IDENTITY;
    out.width = size.0.max(1) as f32;
    out.height = size.1.max(1) as f32;
    out.frame = frame as f32;

    /// 两次覆盖的合成：比单次强，但不超过 1。
    fn over(a: f32, b: f32) -> f32 {
        (1.0 - (1.0 - a) * (1.0 - b)).clamp(0.0, 1.0)
    }

    for effect in effects {
        // 认管线，不认名字 —— 与 color_params / blur_radius 同一套判据。
        let Some(spec) = crate::effects::spec_of(&effect.kind) else {
            continue;
        };
        if spec.pipeline != EffectPipeline::ColorMask {
            continue;
        }
        // 这一帧这条特效的整体强度（时间窗已折进来）。
        let strength = effect.opacity.clamp(0.0, 1.0);
        let param = |name: &str| effect.params.get(name).copied().filter(|v| v.is_finite());
        match spec.kind {
            "flash" => {
                if let Some(amount) = param("amount") {
                    // 颜色缺省为白：闪白是最常见的用法，缺省成黑色会让人以为没生效。
                    let r = param("r").unwrap_or(1.0);
                    let g = param("g").unwrap_or(1.0);
                    let b = param("b").unwrap_or(1.0);
                    let weight = amount.clamp(0.0, 1.0) * strength;
                    if weight > out.flash_amount {
                        // 只在**更强**时换颜色：两条闪白同时挂着时，
                        // 更强的那条说了算。弱的那条不该把颜色稀释成两者平均 ——
                        // 那是"谁都看得出来不是用户想要的颜色"，且解释不清。
                        out.flash_r = r.clamp(0.0, 1.0);
                        out.flash_g = g.clamp(0.0, 1.0);
                        out.flash_b = b.clamp(0.0, 1.0);
                        out.flash_amount = weight;
                    }
                }
            }
            "vignette" => {
                if let Some(amount) = param("amount") {
                    // 取更强的那个：两条暗角叠加没有额外语义。
                    // **比较的是已乘过时间窗的强度**：不然一条时间窗已归零的暗角
                    // 会把一条正在生效的挤掉（"更强"要按这一帧的实际强度算）。
                    let weight = amount.clamp(0.0, 1.0) * strength;
                    if weight >= out.vignette_amount {
                        out.vignette_amount = weight;
                        out.vignette_radius = param("radius").unwrap_or(0.7);
                        // softness 兜一个下限：它是除数，0 会让边缘变成硬阶跃，
                        // 而那在预览与成片之间更容易被看出差异。
                        out.vignette_softness = param("softness").unwrap_or(0.35).max(1e-3);
                    }
                }
            }
            "noise" => {
                if let Some(amount) = param("amount") {
                    let weight = amount.clamp(0.0, 1.0) * strength;
                    if weight >= out.noise_amount {
                        out.noise_amount = weight;
                        out.noise_seed = param("seed").unwrap_or(0.0);
                    }
                }
            }
            "overlay" => {
                if let Some(amount) = param("amount") {
                    let weight = amount.clamp(0.0, 1.0) * strength;
                    let r = param("r").unwrap_or(0.0);
                    let g = param("g").unwrap_or(0.0);
                    let b = param("b").unwrap_or(0.0);
                    // 描画层：后一条覆盖前一条的颜色，强度按 over 合成。
                    if weight > 0.0 {
                        out.overlay_r = r;
                        out.overlay_g = g;
                        out.overlay_b = b;
                        out.overlay_r2 = param("r2").unwrap_or(r);
                        out.overlay_g2 = param("g2").unwrap_or(g);
                        out.overlay_b2 = param("b2").unwrap_or(b);
                        out.overlay_shape =
                            OverlayShape::from_param(param("shape").unwrap_or(0.0)) as u32 as f32;
                        // **度转弧度只在这一处发生**（与 hue 同一条纪律）：
                        // 着色器收的是弧度，两边各转一遍会让 90 度变成 90 弧度。
                        out.overlay_angle = param("angle").unwrap_or(0.0).to_radians();
                    }
                    out.overlay_amount = over(out.overlay_amount, weight);
                }
            }
            // 走了 ColorMask 管线却不在这里 -> 登记表加了新特效但忘了接上。
            // **不静默忽略**：那正是最坏的情形（用户能选中它，画面却不变）。
            other => {
                debug_assert!(false, "ColorMask 管线里的 {other} 没有在 color_mask_params 里接上");
            }
        }
    }
    out
}

/// 把这一批 Warp 特效折叠成一组系数。
///
/// # 合成方式
///
/// 四种都是"一个位移场"，语义上**可以叠加**（同时抖和弹跳是想要的），
/// 所以各自的幅度取所见到的最大值（同种特效挂两条时更强的那条说了算），
/// 而**不同类型的位移在着色器里相加** —— 那是它们的物理意义。
///
/// `seconds` 与 `size` 是位移场的自变量：位移必须是
/// `(像素坐标, 时间秒, seed)` 的纯函数，否则跳帧求值与顺序播放会不一致。
///
/// 强度同样来自 `Effect.opacity`（时间窗已由求值层折进去，见 `color_mask_params`）。
pub fn warp_params(effects: &[Effect], size: (u32, u32), seconds: f32) -> crate::render::WarpParams {
    let mut out = crate::render::WarpParams::IDENTITY;
    out.width = size.0.max(1) as f32;
    out.height = size.1.max(1) as f32;
    out.seconds = seconds;

    for effect in effects.iter() {
        let Some(spec) = crate::effects::spec_of(&effect.kind) else {
            continue;
        };
        if spec.pipeline != EffectPipeline::Warp {
            continue;
        }
        // 这一帧这条特效的整体强度（时间窗已折进来）。
        // **幅度乘它、频率不乘** —— 频率是"抖多快"，不是"抖多少"；
        // 乘上去会让窗口边缘的频率突然变化，看起来像卡了一下。
        let strength = effect.opacity.clamp(0.0, 1.0);
        let param = |name: &str| effect.params.get(name).copied().filter(|v| v.is_finite());
        match spec.kind {
            "shake" => {
                if let Some(amount) = param("amount") {
                    let weight = amount.clamp(0.0, 1.0) * strength;
                    if weight >= out.shake_amount {
                        out.shake_amount = weight;
                        // 频率兜一个下限：0 会让位移变成静止的常量偏移，
                        // 看起来像"画面整体歪了"而不是"在抖"。
                        out.shake_frequency = param("frequency").unwrap_or(12.0).max(1e-3);
                        out.shake_seed = param("seed").unwrap_or(0.0);
                    }
                }
            }
            "zoom_bounce" => {
                if let Some(amount) = param("amount") {
                    let weight = amount.clamp(0.0, 1.0) * strength;
                    if weight >= out.bounce_amount {
                        out.bounce_amount = weight;
                        out.bounce_frequency = param("frequency").unwrap_or(3.0).max(1e-3);
                    }
                }
            }
            "pulse" => {
                if let Some(amount) = param("amount") {
                    let weight = amount.clamp(0.0, 1.0) * strength;
                    if weight >= out.pulse_amount {
                        out.pulse_amount = weight;
                        out.pulse_frequency = param("frequency").unwrap_or(1.5).max(1e-3);
                    }
                }
            }
            "split" => {
                if let Some(amount) = param("amount") {
                    let weight = amount.clamp(0.0, 1.0) * strength;
                    if weight >= out.split_amount {
                        out.split_amount = weight;
                        out.split_offset = param("offset").unwrap_or(0.08);
                        // **度转弧度只在这一处发生**（与 hue / overlay 同一条纪律）。
                        out.split_skew = param("skew").unwrap_or(0.0).to_radians();
                    }
                }
            }
            other => {
                debug_assert!(false, "Warp 管线里的 {other} 没有在 warp_params 里接上");
            }
        }
    }
    // **缩放不能是 0**：它是除数。三条缩放项合成后若接近 0（例如弹跳 -脉冲
    // 恰好抵消），着色器会把采样点推到无穷远。夹一个下限，
    // 代价是"极端参数下不再继续缩小"，那比 NaN 好。
    let zoom = 1.0 + out.bounce_amount + out.pulse_amount;
    if zoom < 0.05 {
        // 把脉冲压到刚好让 zoom == 0.05。
        out.pulse_amount = 0.05 - 1.0 - out.bounce_amount;
    }
    out
}

/// 分配一张中间纹理并记进表里，返回它的索引。
///
/// **模块级而不是某个方法里的局部 fn**：`apply_stage` 与主循环都要分配中间纹理，
/// 而局部 fn 出了那个方法就不存在了。
fn alloc_texture(
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
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    views.push(texture.create_view(&wgpu::TextureViewDescriptor::default()));
    textures.push(texture);
    views.len() - 1
}

/// 只在"调用方没给时间"时用的兜底帧率。
///
/// **它不参与任何有正确性含义的量**：所有精确的东西（帧号、源内帧、半径换算）
/// 都不读秒。只有 Warp 的位移场相位用它，而那个是观感。
/// 精确相位请走 [`TimelineRenderer::render_frame_at`]。
pub const DEFAULT_FRAME_RATE: f32 = 30.0;

/// 时间线渲染器：合成 + 特效的调度。构造一次、每帧复用。
pub struct TimelineRenderer {
    compositor: Compositor,
    blur: BlurRenderer,
    color_adjust: ColorAdjustRenderer,
    color_mask: ColorMaskRenderer,
    warp: WarpRenderer,
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

/// **两张**纹理的提供者：按 source 名字("a"/"b")挑一张。
///
/// 专门给"调整图层按不透明度混回原图"那一步用 —— 那一步要把
/// `原图*(1-t) + 结果*t` 画出来，而 `FixedSource` 只有一张纹理。
/// 复用一个图层把结果以 `t` 画在原图之上就够了，不需要新的着色器。
struct PairSource<'a> {
    a: &'a wgpu::TextureView,
    b: &'a wgpu::TextureView,
    size: (u32, u32),
}

impl SourceResolver for PairSource<'_> {
    fn texture_for(
        &mut self,
        source: &str,
        _source_frame: i64,
    ) -> Option<(wgpu::TextureView, (u32, u32))> {
        let view = if source == "b" { self.b } else { self.a };
        Some((view.clone(), self.size))
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
            color_adjust: ColorAdjustRenderer::new(device, format),
            color_mask: ColorMaskRenderer::new(device, format),
            warp: WarpRenderer::new(device, format),
            format,
        }
    }

    /// ColorMask 管线是否已接上渲染器。（守卫与 UI 用它报"这个特效还画不出来"。）
    pub const fn has_color_mask(&self) -> bool {
        true
    }

    /// Warp 管线是否已接上渲染器。
    pub const fn has_warp(&self) -> bool {
        true
    }

    /// 借用里面的合成器：宿主侧要在**同一趟编码**里往目标上叠别的东西（T2.5 的文字行）。
    ///
    /// 为什么不是「宿主自己 new 一个 Compositor」：叠加用的是同一套混合状态与采样器，
    /// 两份管线迟早会在混合状态上分叉，而那时候画面看起来只是「字有点怪」——
    /// 与「叠加算错了」看不出区别。管线建一次、用两次，这条缝就不会存在。
    ///
    /// 它清不了屏、也换不了坐标系：那两件事仍然是 `render_frame` 的。叠加要的是
    /// 「底已经建好了，往上面画」这一个动作（见 [`compose_overlay`](super::compose_overlay)）。
    pub fn compositor(&self) -> &Compositor {
        &self.compositor
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
    /// # 时间
    ///
    /// **没有帧率参数**：这份实现不知道工程时间基（那在 timeline 层）。
    /// Warp 的位移场需要"秒"，所以调用方要用 [`Self::render_frame_at`] 并给出秒；
    /// 这个入口按 **30fps** 估算 —— 只用于"抖动看起来在抖"这种观感，
    /// 而它**不影响任何有正确性含义的量**（那些都不读墙钟、也不读秒）。
    /// 需要精确相位时用 `render_frame_at`。
    #[allow(clippy::too_many_arguments)]
    pub fn render_frame(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        space: crate::render::RenderSpace,
        composite: &Composite,
        resolver: &mut dyn SourceResolver,
        clear: wgpu::Color,
    ) -> usize {
        let seconds = composite.frame as f32 / DEFAULT_FRAME_RATE;
        self.render_frame_at(device, queue, encoder, target, space, composite, resolver, clear, seconds)
    }

    /// 与 [`Self::render_frame`] 相同，但**显式给出这一帧的时间（秒）**。
    ///
    /// 两端（浏览器 / 服务端）都该用它，把各自的时间基换算一次之后传进来 ——
    /// 让渲染器自己去猜帧率会让同一个工程在两个宿主里抖出不同的相位。
    #[allow(clippy::too_many_arguments)]
    pub fn render_frame_at(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        space: crate::render::RenderSpace,
        composite: &Composite,
        resolver: &mut dyn SourceResolver,
        clear: wgpu::Color,
        seconds: f32,
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
                space,
                &composite.layers,
                resolver,
                Some(clear),
            );
        }
        self.render_segmented(
            device, queue, encoder, target, space, composite, resolver, clear, &plan, seconds,
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
        space: crate::render::RenderSpace,
        composite: &Composite,
        resolver: &mut dyn SourceResolver,
        clear: wgpu::Color,
        plan: &[Step],
        seconds: f32,
    ) -> usize {
        let extent = wgpu::Extent3d {
            width: space.target.0.max(1),
            height: space.target.1.max(1),
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
                        space,
                        &layers,
                        resolver,
                        if fresh { Some(clear) } else { None },
                    );
                    current = Some(dest);
                }
                Step::Adjust { effects, opacity, .. } => {
                    let Some(from) = current else { continue };

                    // **按管线分批跑，顺序由 `PassStage` 定**（见那里的注释）。
                    // 这一段以前是"色彩一块、模糊一块"两段写死的代码，加一条管线
                    // 就要重排一次顺序论证。现在加管线**不必碰这里**：
                    // 分级写在对 `EffectPipeline` 的穷尽 match 里，漏了会编译不过。
                    //
                    // `from` 在每一趟之后更新，因为在两个 `continue` 之间它会被
                    // 借用走；这里用带索引的视图表，索引才是真相。
                    let mut cursor = from;
                    for (stage, indices) in effect_passes(effects) {
                        // 这一批只取属于它的那几条，交给对应的求值。
                        let batch: Vec<Effect> = indices
                            .iter()
                            .filter_map(|index| effects.get(*index).cloned())
                            .collect();
                        let Some(next) = self.apply_stage(
                            stage, &batch, cursor, extent, space, composite.frame, seconds,
                            device, queue, encoder, &mut textures, &mut views,
                        ) else {
                            continue;
                        };
                        cursor = next;
                    }
                    // -----------------------------------------------------------------
                    // 按**这一帧上这层的不透明度**把结果混回原图
                    // -----------------------------------------------------------------
                    //
                    // 这是"效果强度"的通用旋钮。参照实现 的 `flash` / `noise` /
                    // `hue_shift` / `vignette` 都是逐帧变化的强度（0.04s 升到峰值、
                    // 再线性落回），所以这个混合不是可有可无的装饰。
                    //
                    // 因为**原图是不透明的**，把结果以 `opacity` 画在原图之上
                    // 恰好就是 `原图*(1-t) + 结果*t` —— 不需要新的着色器。
                    if cursor != from && *opacity < 1.0 {
                        if *opacity <= 0.0 {
                            // 完全不透明的是原图，等于没这一层。
                            cursor = from;
                        } else {
                            let a_view = views[from].clone();
                            let b_view = views[cursor].clone();
                            let dest = alloc_texture(
                                device, self.format, extent,
                                "dhampir adjust blend out", &mut textures, &mut views,
                            );
                            let dest_view = views[dest].clone();
                            let mut resolver = PairSource {
                                a: &a_view,
                                b: &b_view,
                                size: space.target,
                            };
                            let mut under = identity_layer();
                            under.source = "a".to_string();
                            let mut over = identity_layer();
                            over.source = "b".to_string();
                            over.opacity = *opacity;
                            let blend_layers = [under, over];
                            self.compose_layers(
                                device, queue, encoder, &dest_view, space,
                                &blend_layers, &mut resolver,
                                Some(wgpu::Color::TRANSPARENT),
                            );
                            cursor = dest;
                        }
                    }
                    if cursor != from {
                        current = Some(cursor);
                    }
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
            size: space.target,
        };
        let blit = [identity_layer()];
        drawn += self.compose_layers(
            device, queue, encoder, target, space, &blit, &mut fixed, Some(clear),
        );
        drop(textures);
        drawn
    }

    /// 跑**一级**特效，返回结果的纹理索引（这一级什么都没做就给 None）。
    ///
    /// 把"哪一级怎么跑"收在这个函数里，`Step::Adjust` 那边就只剩调度 ——
    /// 加一条管线要改的是这里加一支，而不是回去重排主路径里的顺序论证。
    #[allow(clippy::too_many_arguments)]
    fn apply_stage(
        &self,
        stage: PassStage,
        batch: &[Effect],
        from: usize,
        extent: wgpu::Extent3d,
        space: crate::render::RenderSpace,
        frame: i64,
        seconds: f32,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        textures: &mut Vec<wgpu::Texture>,
        views: &mut Vec<wgpu::TextureView>,
    ) -> Option<usize> {
        match stage {
            PassStage::PerPixel => {
                // 这一级里 ColorAdjust 与 ColorMask 都是逐像素，但**参数结构不同**，
                // 所以各自一趟（顺序：先调整现有像素，再叠常量色 —— 反过来会让
                // 叠加的颜色被后面的调整再改一遍，与"用户调的是原图"的直觉不符）。
                let mut cursor = from;
                let params = color_params(batch);
                if !params.is_identity() {
                    let out = alloc_texture(
                        device, self.format, extent, "dhampir adjust color out",
                        textures, views,
                    );
                    let source = views[cursor].clone();
                    let to = views[out].clone();
                    self.color_adjust.apply(device, queue, encoder, &source, &to, params);
                    cursor = out;
                }
                // ColorMask 与 ColorAdjust 都是逐像素，但**参数结构不同**，各走一趟。
                // 顺序：先调整现有像素，再叠常量色 —— 反过来会让叠加的颜色
                // 被后面的调整再改一遍，与"用户调的是原图"的直觉不符。
                let mask = color_mask_params(batch, space.target, frame);
                if !mask.is_identity() {
                    let out = alloc_texture(
                        device, self.format, extent, "dhampir adjust mask out",
                        textures, views,
                    );
                    let source = views[cursor].clone();
                    let to = views[out].clone();
                    self.color_mask.apply(device, queue, encoder, &source, &to, mask);
                    cursor = out;
                }
                if cursor != from { Some(cursor) } else { None }
            }
            PassStage::Warp => {
                let params = warp_params(batch, space.target, seconds);
                if params.is_identity() {
                    return None;
                }
                let out = alloc_texture(
                    device, self.format, extent, "dhampir adjust warp out", textures, views,
                );
                let source = views[from].clone();
                let to = views[out].clone();
                self.warp.apply(device, queue, encoder, &source, &to, params);
                Some(out)
            }
            PassStage::Neighborhood => {
                // **调整图层的模糊半径是文档像素**：它跑在目标尺寸的中间纹理上，
                // 所以目标尺寸一变，同一个半径看起来就不一样了 —— 必须按比例换算。
                // （每层的 gaussian_blur 不是这个情况：它跑在**源**纹理上，见 compose_layers。）
                let radius = radius_in_space(
                    blur_radius(batch),
                    dhampir_timeline::schema::EffectSpace::Document,
                    space,
                );
                if radius == 0 {
                    return None;
                }
                // blur_separable 需要一张中间纹理与一张输出纹理（它自己是一横一纵两趟）。
                let middle = alloc_texture(
                    device, self.format, extent, "dhampir adjust middle", textures, views,
                );
                let out = alloc_texture(
                    device, self.format, extent, "dhampir adjust out", textures, views,
                );
                let source = views[from].clone();
                let mid = views[middle].clone();
                let to = views[out].clone();
                self.blur.blur_separable(
                    device, queue, encoder, &source, &mid, &to, space.target, radius,
                );
                Some(out)
            }
        }
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
        space: crate::render::RenderSpace,
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
            // **实拍片段：显式声明 Source。**
            // 模糊跑在**源**纹理上（下面两张纹理都是 size = 源尺寸），
            // 所以半径是源像素，与文档坐标系无关。换算它反而会让同一个源在不同导出尺寸下糊得不一样。
            //
            // 传 Source 而不是查登记表：登记表对 gaussian_blur 声明的是 Document
            // （两个空间都用到了，只能声明一个）。**这里以调用点为准** ——
            // 这正是 S6 把「空间」从登记表单值改成调用点参数的原因。
            let radius = radius_in_space(
                blur_radius(&layer.effects),
                dhampir_timeline::schema::EffectSpace::Source,
                space,
            );
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
            space,
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
            window: dhampir_timeline::schema::Window::Always,
            opacity: 1.0,
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
    fn 派发认的是登记表的管线而不是名字() {
        // 这一条是本段（S3）的**结构性判据**：证明 blur_radius 认的是
        // 登记表里的 pipeline，而不是某个写死的字符串。
        //
        // 判法：把所有已登记、且管线是 SeparableBlur 的 kind **从登记表里推出来**，
        // 逐个喂给 blur_radius，都必须识别。这样将来加一个走同一管线的新特效时，
        // 这条测试**自动**覆盖它 —— 不需要有人记得回来加一行。
        let blur_kinds: Vec<&str> = crate::effects::REGISTRY
            .iter()
            .filter(|spec| spec.pipeline == EffectPipeline::SeparableBlur)
            .map(|spec| spec.kind)
            .collect();
        assert!(!blur_kinds.is_empty(), "至少该有一个走 SeparableBlur 的特效");

        for kind in &blur_kinds {
            assert_eq!(
                blur_radius(&[effect(kind, &[("radius", 5.0)])]),
                5,
                "{kind} 声明了 SeparableBlur 管线，blur_radius 就必须认它"
            );
        }

        // 反向：登记表里的特效若**不是** SeparableBlur，就不该触发模糊。
        // 现在还没有这种特效，所以这里用"没登记的 kind"代表这一类 ——
        // 它同样必须被跳过，而不是被当成模糊。
        assert_eq!(
            blur_radius(&[effect("根本没登记过", &[("radius", 5.0)])]),
            0,
            "未登记的 kind 不该触发任何管线"
        );
    }

    #[test]
    fn 四个恒等特效折出来仍然是恒等() {
        // 这条是**实测出来的**：挂上四个色彩特效、参数全填恒等值，
        // 出片必须与完全不挂时逐字节相同（已用 digest 验过）。
        // 这里把那个事实钉进单测，免得以后改折叠逻辑时悄悄破坏它。
        let identity = [
            effect("brightness", &[("amount", 0.0)]),
            effect("contrast", &[("amount", 1.0)]),
            effect("saturation", &[("amount", 1.0)]),
            effect("hue", &[("degrees", 0.0)]),
        ];
        assert!(
            color_params(&identity).is_identity(),
            "四个恒等参数折起来必须还是恒等，否则会白跑一趟甚至改错画面"
        );
    }

    #[test]
    fn 同类色彩特效叠加是可交换的() {
        // 顺序无关很重要：两端各自的遍历顺序若不同，顺序相关就会变成两端不一致。
        // 亮度可加、其余可乘，所以两种顺序必须给出同一个结果。
        let a = [effect("brightness", &[("amount", 0.1)]), effect("brightness", &[("amount", 0.2)])];
        let b = [effect("brightness", &[("amount", 0.2)]), effect("brightness", &[("amount", 0.1)])];
        let pa = color_params(&a);
        let pb = color_params(&b);
        assert!((pa.brightness - pb.brightness).abs() < 1e-6, "亮度叠加必须可交换");

        let c = [effect("saturation", &[("amount", 0.5)]), effect("saturation", &[("amount", 2.0)])];
        let d = [effect("saturation", &[("amount", 2.0)]), effect("saturation", &[("amount", 0.5)])];
        let pc = color_params(&c);
        let pd = color_params(&d);
        assert!((pc.saturation - pd.saturation).abs() < 1e-6, "饱和度叠加必须可交换");
    }

    #[test]
    fn 度转弧度在折叠时发生且只发生一次() {
        // 用户填度、着色器收弧度。转两次会让 90 度变成 90 弧度再转一次。
        let p = color_params(&[effect("hue", &[("degrees", 90.0)])]);
        assert!((p.hue - std::f32::consts::FRAC_PI_2).abs() < 1e-6, "90 度应当是 pi/2 弧度");
    }

    #[test]
    fn 没挂色彩特效时是恒等_调用方据此跳过() {
        assert!(color_params(&[]).is_identity());
        // 只挂模糊时也应当是恒等：模糊不是 ColorAdjust 管线，不该混进来。
        assert!(color_params(&[effect("gaussian_blur", &[("radius", 8.0)])]).is_identity());
    }

    #[test]
    fn 空间由调用点决定而不是登记表说了算() {
        // 这条钉住 S6 的核心改动：半径换算**按调用点传的空间**走，
        // 而不是去读登记表的单值声明。
        //
        // 为什么重要：同一个 gaussian_blur 在两个地方跑，需要的换算相反。
        // 若两者都听登记表的，必然有一处错 —— 而错了不会报错，
        // 只会在预览与成片尺寸不同时糊得不一样。
        use crate::render::RenderSpace;
        use dhampir_timeline::schema::EffectSpace;

        // 文档 1920x1080 -> 目标 640x360，比例 1/3。
        let space = RenderSpace { sequence: (1920, 1080), target: (640, 360) };

        // 半径取 12（在上界 16 之内），这样比的是**换算**而不是夹取。
        // Source：原样。实拍片段那条路走这个。
        assert_eq!(
            radius_in_space(12, EffectSpace::Source, space),
            12,
            "源空间不该换算 —— 半径本来就是源像素"
        );

        // Document：按比例缩。调整图层那条路走这个。
        assert_eq!(
            radius_in_space(12, EffectSpace::Document, space),
            4,
            "文档空间要按 640/1920 缩到 4"
        );

        // 反向用例：两者**必须不同**。
        // 若哪天有人把 radius_in_space 改成恒等函数（或让它去读登记表），
        // 这条会红 —— 而"两处都变成同一个值"正是要防的那个退化。
        assert_ne!(
            radius_in_space(12, EffectSpace::Source, space),
            radius_in_space(12, EffectSpace::Document, space),
            "两种空间必须给出不同结果，否则这个参数没有意义"
        );

        // 尺寸相同时两者相等 —— 换算恒等，且都夹到同一个上界。
        // 用**未超界**的半径（10），否则两边都被夹到同一上界，
        // 会掩盖"换算是否真的发生了"这件事。
        let same = RenderSpace { sequence: (1920, 1080), target: (1920, 1080) };
        assert_eq!(radius_in_space(10, EffectSpace::Source, same), 10);
        assert_eq!(radius_in_space(10, EffectSpace::Document, same), 10);

        // **源空间也要夹上界** —— 这是实测补上的一条。
        // 曾经 Source 分支直接原样返回，绕过校验的 radius=999 会让着色器越界索引。
        assert_eq!(
            radius_in_space(999, EffectSpace::Source, space),
            crate::render::BLUR_MAX_RADIUS,
            "源空间不换算，但必须夹上界"
        );
        assert_eq!(
            radius_in_space(999, EffectSpace::Document, space),
            crate::render::BLUR_MAX_RADIUS,
            "文档空间同样夹上界"
        );
    }

    #[test]
    fn 半径上界跟着登记表走() {
        // 上界原先写死成 BLUR_MAX_RADIUS。现在从登记表推 ——
        // 这条钉住"两者当下相等"，改登记表时若忘了同步着色器上界，它会红。
        let from_registry = crate::effects::spec_of("gaussian_blur")
            .and_then(|spec| spec.param_max("radius"))
            .expect("登记表必须有 radius 上界");
        assert_eq!(
            from_registry, crate::render::BLUR_MAX_RADIUS as f32,
            "登记表上界与着色器硬上界必须一致"
        );
        // 而且真的被用上了：超界值夹到登记表上界，不是别的数。
        assert_eq!(
            blur_radius(&[effect("gaussian_blur", &[("radius", 1.0e9)])]),
            from_registry as u32
        );
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
    ///
    /// # `opacity` 为什么必须在这里
    ///
    /// 这一条是**补上的**：先前 `Adjust` 只带 `effects`，于是调整图层的
    /// `opacity`（以及它的关键帧）**被整个忽略** —— 通过校验、出现在 JSON 里、
    /// 求值层也算出来了，渲染时没人读。
    ///
    /// 而它正是"效果强度"的通用旋钮：参照实现 的 `flash` / `noise` / `hue_shift` /
    /// `vignette` 都是**逐帧变化的强度**（0.04s 升到峰值再线性落回），
    /// 没有它就只能整段满强度，那与参照差得很远。
    ///
    /// 语义：把特效结果与**原图**按 `opacity` 线性混合 ——
    /// 也就是"这个调整图层有多少分量"。因为原图是不透明的，用 alpha 混合画上去
    /// 恰好就是 `原图*(1-t) + 结果*t`。对逐像素效果（色彩、遮罩）这是精确的；
    /// 对模糊这类邻域算子它是"糊与不糊的混合"而不是"半径变小"，属于近似。
    Adjust {
        layer: usize,
        effects: Vec<dhampir_timeline::schema::Effect>,
        opacity: f32,
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
                // **这一帧上这层的不透明度**（关键帧已经在求值层算好了）。
                opacity: layer.opacity.clamp(0.0, 1.0),
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

    /// 造一条特效，给 pass 分批的用例用。
    fn effect(kind: &str, params: &[(&str, f32)]) -> Effect {
        Effect {
            kind: kind.to_string(),
            params: params.iter().map(|(key, value)| (key.to_string(), *value)).collect(),
            window: dhampir_timeline::schema::Window::Always,
            opacity: 1.0,
        }
    }

    fn layer(id: &str, adjustment: bool) -> crate::compose::Layer {
        crate::compose::Layer {
            clip_id: id.to_string(),
            source: if adjustment { String::new() } else { format!("{id}.mp4") },
            source_frame: 0,
            opacity: 1.0,
            transform: Transform { x: 0.0, y: 0.0, scale: 1.0, rotation_deg: 0.0 },
            effects: if adjustment {
                vec![Effect {
                    kind: "gaussian_blur".to_string(),
                    params: Default::default(),
                    window: dhampir_timeline::schema::Window::Always,
                    opacity: 1.0,
                }]
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
            Step::Adjust { layer: index, effects, .. } => {
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

    // ===== T9：pass 顺序是数据，有测试 =====

    /// 各级的执行序必须是 逐像素 -> Warp -> 邻域。
    ///
    /// 这条**就是**原先写在 `Step::Adjust` 注释里的那段论证，
    /// 只是从"读代码才能知道"变成了"跑测试才知道"。
    #[test]
    fn 级的执行序是逐像素_再_warp_再邻域() {
        assert!(PassStage::PerPixel < PassStage::Warp);
        assert!(PassStage::Warp < PassStage::Neighborhood);
    }

    #[test]
    fn 每条管线都归到了某一级() {
        // 穷尽 match 保证编译期不漏；这条钉的是**归属本身**别被改错。
        // 改错的后果是顺序变了而没有任何东西会红 —— 画面只是"有点怪"。
        use EffectPipeline::*;
        assert_eq!(PassStage::of(ColorAdjust), PassStage::PerPixel);
        assert_eq!(PassStage::of(ColorMask), PassStage::PerPixel);
        assert_eq!(PassStage::of(Warp), PassStage::Warp);
        assert_eq!(PassStage::of(SeparableBlur), PassStage::Neighborhood);
    }

    #[test]
    fn 批次按级排序且组内保持原序() {
        // 故意把顺序打乱成"模糊在前、亮度在后"：批次必须把逐像素提到前面，
        // 但**组内**（这里只有一条亮度）次序不能被动过。
        let effects = vec![
            effect("gaussian_blur", &[("radius", 4.0)]),
            effect("brightness", &[("amount", 0.2)]),
            effect("saturation", &[("amount", 1.5)]),
        ];
        let passes = effect_passes(&effects);
        assert_eq!(passes.len(), 2, "两级：逐像素与邻域");

        let (first_stage, first_indices) = &passes[0];
        assert_eq!(*first_stage, PassStage::PerPixel, "逐像素必须最先");
        // 亮度与饱和度**保持它们写下的次序**（1 在 2 之前）。
        assert_eq!(first_indices, &vec![1, 2]);

        let (second_stage, second_indices) = &passes[1];
        assert_eq!(*second_stage, PassStage::Neighborhood);
        assert_eq!(second_indices, &vec![0]);
    }

    #[test]
    fn 认不出的特效不进任何批次() {
        // 没登记过的 kind **不静默当成某一级**：它进不了批次，
        // 于是不会在渲染时被当成"某个已知特效"画错。
        let effects = vec![effect("不存在的特效", &[("amount", 1.0)])];
        assert!(effect_passes(&effects).is_empty());
    }

    #[test]
    fn 加一条新管线的特效不必改调度() {
        // 反向用例：证明 `effect_passes` 是**按注册表**派的，不是按 kind 名字。
        // 拿一条真实登记的 ColorMask 特效（flash）验证它落进 PerPixel，
        // 而 `Step::Adjust` 那边一行都不用动。
        let effects = vec![effect("flash", &[("amount", 1.0)])];
        let passes = effect_passes(&effects);
        assert_eq!(passes.len(), 1);
        assert_eq!(passes[0].0, PassStage::PerPixel);
        assert_eq!(passes[0].1, vec![0]);
    }

    // ===== T10 / T11：两条新管线的求值 =====

    #[test]
    fn 没有_mask_特效时参数是恒等的() {
        // 这条保证"没挂特效的帧"整条跳过 —— 恒等判据错了会让每一帧都多走一趟。
        let params = color_mask_params(&[], (1920, 1080), 0);
        assert!(params.is_identity(), "空清单应当是恒等");
        // 认不出的 kind 也不该改变恒等性。
        let unknown = vec![effect("不存在的特效", &[("amount", 1.0)])];
        assert!(color_mask_params(&unknown, (1920, 1080), 0).is_identity());
    }

    #[test]
    fn 闪白的颜色缺省是白() {
        let effects = vec![effect("flash", &[("amount", 1.0)])];
        let params = color_mask_params(&effects, (1920, 1080), 0);
        assert!(!params.is_identity());
        assert_eq!(params.flash_amount, 1.0);
        assert_eq!((params.flash_r, params.flash_g, params.flash_b), (1.0, 1.0, 1.0));
    }

    #[test]
    fn 两条闪白取更强的那条的颜色() {
        // 更强的说了算，不做平均 —— 平均出来的颜色解释不清，且不是用户填的任何一个。
        let effects = vec![
            effect("flash", &[("amount", 0.2), ("r", 1.0), ("g", 0.0), ("b", 0.0)]),
            effect("flash", &[("amount", 0.9), ("r", 0.0), ("g", 0.0), ("b", 1.0)]),
        ];
        let params = color_mask_params(&effects, (1920, 1080), 0);
        assert_eq!(params.flash_amount, 0.9);
        assert_eq!((params.flash_r, params.flash_g, params.flash_b), (0.0, 0.0, 1.0));
    }

    #[test]
    fn 暗角的_softness_不会变成零() {
        // softness 是着色器里的**除数**：传 0 会让边缘变成硬阶跃，
        // 而硬阶跃在预览与成片之间更容易被看出差异。这里钉住它有下限。
        let effects = vec![effect("vignette", &[("amount", 0.5), ("softness", 0.0)])];
        let params = color_mask_params(&effects, (1920, 1080), 0);
        assert!(params.vignette_softness > 0.0, "softness 不能是 0（它是除数）");
    }

    #[test]
    fn 覆盖层的角度在这里就转成弧度() {
        // **度转弧度只在一处发生**：两边各转一遍会让 90 度变成 90 弧度。
        let effects = vec![effect("overlay", &[("amount", 1.0), ("angle", 180.0), ("shape", 1.0)])];
        let params = color_mask_params(&effects, (1920, 1080), 0);
        assert!(
            (params.overlay_angle - std::f32::consts::PI).abs() < 1e-5,
            "180 度应当变成 PI 弧度，实得 {}",
            params.overlay_angle
        );
        assert_eq!(params.overlay_shape, 1.0, "shape=1 是线性渐变");
    }

    #[test]
    fn 没有_warp_特效时参数是恒等的() {
        assert!(warp_params(&[], (1920, 1080), 0.0).is_identity());
    }

    #[test]
    fn 分屏的斜切在这里就转成弧度() {
        let effects = vec![effect("split", &[("amount", 1.0), ("skew", 45.0)])];
        let params = warp_params(&effects, (1920, 1080), 0.0);
        assert!(
            (params.split_skew - std::f32::consts::FRAC_PI_4).abs() < 1e-5,
            "45 度应当是 PI/4，实得 {}",
            params.split_skew
        );
    }

    #[test]
    fn 抖动的频率不会变成零() {
        // 频率 0 会让位移变成一个静止的常量偏移 —— 看起来像"画面整体歪了"，
        // 而不是"在抖"。兜一个下限。
        let effects = vec![effect("shake", &[("amount", 0.1), ("frequency", 0.0)])];
        let params = warp_params(&effects, (1920, 1080), 0.0);
        assert!(params.shake_frequency > 0.0, "频率不能是 0");
    }

    #[test]
    fn 缩放不会掉到零附近() {
        // 缩放是着色器里的**除数**：三条缩放项抵消到 0 会把采样点推到无穷远。
        // 这里钉住兜底真的生效。
        let effects = vec![effect("pulse", &[("amount", 1.0), ("frequency", 1.0)])];
        let params = warp_params(&effects, (1920, 1080), 0.25);
        let zoom = 1.0 + params.bounce_amount + params.pulse_amount;
        assert!(zoom >= 0.05, "缩放掉到 {zoom}，会把采样点推到无穷远");
    }

    #[test]
    fn 缩放类特效会在多数帧上真的改变采样() {
        // **这条是拿实测抓出来的教训写的单帧用例抓不住它。**
        //
        // 弹跳用 `abs(sin)`、脉冲用 `sin(...)`，它们在固定间隔上**恰好等于 0**
        // （弹跳 t = k/(2f)、脉冲 t = k/f 的整数倍）。只测一帧的话，
        // 恰好撞上零点会得到"缩放 == 1"，看起来像"这个特效没接上"，
        // 而实际上它在别的帧上是好的。
        //
        // 这里扫一个周期：**至少有一帧的缩放必须明显偏离 1**。
        for (kind, params, freq) in [
            ("zoom_bounce", vec![("amount", 0.12), ("frequency", 3.0)], 3.0f32),
            ("pulse", vec![("amount", 0.08), ("frequency", 1.5)], 1.5),
        ] {
            let effects = vec![effect(kind, &params)];
            // 一个完整周期 = 1/f 秒，取 32 个采样点，秒的步长是周期/32。
            let step = 1.0 / freq / 32.0;
            let mut max_deviation: f32 = 0.0;
            for i in 0..32 {
                let seconds = step * i as f32;
                let p = warp_params(&effects, (1920, 1080), seconds);
                let zoom = 1.0 + p.bounce_amount + p.pulse_amount;
                max_deviation = max_deviation.max((zoom - 1.0).abs());
            }
            assert!(
                max_deviation > 0.01,
                "{kind} 在一个周期内的缩放始终贴着 1.0（最大偏离 {max_deviation}）—— \
                 这个特效实际上没接上着色器"
            );
        }
    }

    #[test]
    fn 抖动不会整帧静止() {
        // 抖动是两路正弦（纵横各一），频率不同 —— 着色器里用的是
        // 6.2831853 与 4.7123890 两个**不成整数比**的系数，
        // 所以两路不会同时过零：任何一帧至少有一路在动。
        //
        // 这里只能验到"幅度被传下去了"（相位在着色器里算，
        // Rust 侧看不到两路 sin 的值）—— 所以这条断言的是**参数装配**，
        // 真正的观感由 `缩放类特效会在多数帧上真的改变采样` 那类实测兜。
        let effects = vec![effect("shake", &[("amount", 0.02), ("frequency", 5.0), ("seed", 1.0)])];
        let p = warp_params(&effects, (1920, 1080), 0.0);
        assert_eq!(p.shake_amount, 0.02);
        assert_eq!(p.shake_frequency, 5.0);
        assert_eq!(p.shake_seed, 1.0);
        assert!(!p.is_identity(), "挂了抖动就不该是恒等");
    }
}
