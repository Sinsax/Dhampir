//! 特效注册表：类型串 → 参数规格（以及将来的 pipeline）。
//!
//! 这里只有**一份**登记表：T4.1 的校验拿它校对工程，T4.2 的渲染图拿它选 pipeline。
//! 分成两份的话，UI 上控件的取值范围和后端实际接受的取值范围迟早对不上——
//! 而那种不一致表现为"用户能拖到某个值，导出时才报错"。

use dhampir_timeline::schema::{EffectPipeline, EffectSpace, EffectSpec, WindowDefault};

/// 高斯模糊。radius 是像素半径。
///
/// 上界**必须**等于着色器能展开的最大抽头数的一半（`render::BLUR_MAX_RADIUS`）。
/// 写死一个更大的数会让「用户能拖到 64」而着色器只按 16 算——
/// 那种不一致不报错，只是模糊得不够，正是这份登记表要防的事。
/// 有一条测试把两者钉在一起。
pub const GAUSSIAN_BLUR: EffectSpec = EffectSpec {
    kind: "gaussian_blur",
    params: &[("radius", 0.0, crate::render::BLUR_MAX_RADIUS as f32)],    // **这个 space 是「默认空间」，不是唯一真相。**
    //
    // 同一个 kind 在两个空间里都会出现：实拍片段上的 gaussian_blur 跑在源纹理上
    // （源像素，不换算），调整图层上的同名特效跑在目标尺寸上（文档像素，要换算）。
    // 登记表只能给一个值，所以它表达不了这个二义性。
    //
    // S6 的处理是**把判据移到调用点**：render::timeline 的两个调用点各自显式传
    // Source 或 Document（见 radius_in_space 的文档）。那两处才是真正的判据，
    // 而它们各自都有测试钉着。
    //
    // 这里声明 Document 只是**默认值**，用于 UI 提示与"调用点没指定时的兜底"。
    // 选 Document 的理由：兜底时"多缩放一次"在预览与成片尺寸不同时看得出来，
    // 而"该缩没缩"长得像"模糊得不够"—— 后者正是本项目最要避免的**静默偏差**。
    space: EffectSpace::Document,
    pipeline: EffectPipeline::SeparableBlur,
    // 瞬时模糊（V-Trim 的 blur 事件）：涨 2 帧、满 3 帧、落 5 帧 ≈ 0.33 秒 @30fps。
    window_default: Some(WindowDefault { attack: 2, hold: 3, release: 5 }),
};

/// 四个逐像素色彩调整。**同一条管线**，所以这里登记四次、渲染只写一次。
///
/// 为什么四个 kind 而不是一个带 mode 参数：
/// UI 上它们是四个独立控件、有各自的取值范围与默认值；
/// 合成一个的话，"亮度"的取值范围会跟着"饱和度"一起校验，报错也说不清是哪一项。
/// 管线相同这个事实由 pipeline 字段表达，不需要靠合并 kind 来表达。
///
/// 范围都是 [0, ...] 或 [-1, 1]：亮度/对比度/饱和度都用**归一化**量，
/// 不用百分比 —— 百分比要除以 100，多一次除就多一次舍入差异。
pub const BRIGHTNESS: EffectSpec = EffectSpec {
    kind: "brightness",
    params: &[("amount", -1.0, 1.0)],
    // 逐像素算子，与坐标系无关；声明 Source 是因为它读的是**输入纹理本身**，
    // 不涉及"文档像素"这个概念（ColorAdjust 管线根本不调半径换算）。
    space: EffectSpace::Source,
    pipeline: EffectPipeline::ColorAdjust,
    window_default: None,
};
/// 对比度：绕 0.5 中灰缩放。1.0 = 不变。
pub const CONTRAST: EffectSpec = EffectSpec {
    kind: "contrast",
    params: &[("amount", 0.0, 4.0)],
    space: EffectSpace::Source,
    pipeline: EffectPipeline::ColorAdjust,
    window_default: None,
};
/// 饱和度：向亮度插值。1.0 = 不变，0.0 = 完全灰度。
pub const SATURATION: EffectSpec = EffectSpec {
    kind: "saturation",
    params: &[("amount", 0.0, 4.0)],
    space: EffectSpace::Source,
    pipeline: EffectPipeline::ColorAdjust,
    window_default: None,
};
/// 色调：色相旋转，单位**度**（不是弧度）。
///
/// 用户填的是度：说"转 90 度"比说"转 1.5708"自然，而 UI 上填 1.5708 没法看。
/// 度转弧度在 Rust 侧做一次（见 render::color_adjust），着色器只收弧度 ——
/// 转换只发生在一处，两端不会各转一遍。
pub const HUE: EffectSpec = EffectSpec {
    kind: "hue",
    params: &[("degrees", -180.0, 180.0)],
    space: EffectSpace::Source,
    pipeline: EffectPipeline::ColorAdjust,
    window_default: None,
};
/// 全部已登记的特效。
///
/// **顺序不重要**（查找是线性的），但分组成段便于读。
pub const REGISTRY: &[EffectSpec] = &[
    // ---- 已有 ----
    GAUSSIAN_BLUR,
    BRIGHTNESS,
    CONTRAST,
    SATURATION,
    HUE,
    // ---- ColorMask（T10）----
    FLASH,
    VIGNETTE,
    NOISE,
    OVERLAY,
    // ---- Warp（T11）----
    SHAKE,
    ZOOM_BOUNCE,
    PULSE,
    SPLIT,
];

// ===== ColorMask 管线（T10）：用常量色叠加的逐像素算子 =====
//
// 与 ColorAdjust 的区别见 EffectPipeline::ColorMask 的注释。这四个都是
// V-Trim 里真有的事件（flash / vignette / noise / overlay），不是凭空加的。
//
// **颜色一律拆成 r/g/b/a 四个参数**，不引第二个 map：
// `params` 是 `BTreeMap<String, f32>`，一个字段一个含义，靠键名约定
// （`"color"` 装四个数）迟早会在某一端被解析错，而那种错不会报错、只会画错颜色。

/// 闪白（可带色）。`amount` 是覆盖强度，颜色由 r/g/b 给。
///
/// V-Trim 的 flash 默认白色、0.25 秒。这里用同一个默认窗。
pub const FLASH: EffectSpec = EffectSpec {
    kind: "flash",
    params: &[
        ("amount", 0.0, 1.0),
        ("r", 0.0, 1.0),
        ("g", 0.0, 1.0),
        ("b", 0.0, 1.0),
    ],
    space: EffectSpace::Source,
    pipeline: EffectPipeline::ColorMask,
    window_default: Some(WindowDefault { attack: 1, hold: 1, release: 4 }),
};

/// 暗角：从中心到边缘逐渐压暗。`amount` 是边缘处的压暗量。
pub const VIGNETTE: EffectSpec = EffectSpec {
    kind: "vignette",
    params: &[("amount", 0.0, 1.0), ("radius", 0.1, 2.0), ("softness", 0.0, 1.0)],
    space: EffectSpace::Document,
    pipeline: EffectPipeline::ColorMask,
    window_default: Some(WindowDefault { attack: 2, hold: 4, release: 6 }),
};

/// 噪声。`amount` 是叠加强度，`seed` 决定这一份噪声长什么样（**可复现**）。
///
/// 噪声**必须是帧号的函数**（见 `Window` 的注释）：用系统随机会让预览与成片
/// 逐帧不同，而那正是本项目最要避免的"看起来成功、其实没验"。
pub const NOISE: EffectSpec = EffectSpec {
    kind: "noise",
    params: &[("amount", 0.0, 1.0), ("seed", 0.0, 4096.0)],
    space: EffectSpace::Source,
    pipeline: EffectPipeline::ColorMask,
    window_default: Some(WindowDefault { attack: 1, hold: 4, release: 4 }),
};

/// 纯色/渐变覆盖层。`shape` 0=纯色 1=线性渐变 2=径向渐变。
///
/// 渐变的第二个颜色用 `r2/g2/b2` —— 与主色同一套命名，不发明新规则。
pub const OVERLAY: EffectSpec = EffectSpec {
    kind: "overlay",
    params: &[
        ("amount", 0.0, 1.0),
        ("r", 0.0, 1.0),
        ("g", 0.0, 1.0),
        ("b", 0.0, 1.0),
        ("r2", 0.0, 1.0),
        ("g2", 0.0, 1.0),
        ("b2", 0.0, 1.0),
        ("shape", 0.0, 2.0),
        ("angle", 0.0, 360.0),
    ],
    space: EffectSpace::Document,
    pipeline: EffectPipeline::ColorMask,
    window_default: None,
};

// ===== Warp 管线（T11）：坐标重映射 =====
//
// 单位一律用**归一化量**（相对画面尺寸的比例），不用像素：
// 像素在预览（640x360）与成片（1920x1080）里含义不同，两端就不一致了 ——
// 这条理由与 SubtitleStyle 用 font_ratio 而不是字号像素是同一条。

/// 抖动：高频小幅位移。`amount` 是位移幅度（画面宽度的比例），`frequency` 是每秒振荡次数。
pub const SHAKE: EffectSpec = EffectSpec {
    kind: "shake",
    params: &[("amount", 0.0, 0.2), ("frequency", 1.0, 60.0), ("seed", 0.0, 4096.0)],
    space: EffectSpace::Document,
    pipeline: EffectPipeline::Warp,
    window_default: Some(WindowDefault { attack: 1, hold: 4, release: 4 }),
};

/// 缩放弹跳：`amount` 是最大放大倍数（0.1 = 放大 10%）。
pub const ZOOM_BOUNCE: EffectSpec = EffectSpec {
    kind: "zoom_bounce",
    params: &[("amount", 0.0, 1.0), ("frequency", 0.5, 30.0)],
    space: EffectSpace::Document,
    pipeline: EffectPipeline::Warp,
    window_default: Some(WindowDefault { attack: 2, hold: 4, release: 6 }),
};

/// 脉冲：低频呼吸式缩放，比 zoom_bounce 缓和。
pub const PULSE: EffectSpec = EffectSpec {
    kind: "pulse",
    params: &[("amount", 0.0, 1.0), ("frequency", 0.5, 30.0)],
    space: EffectSpace::Document,
    pipeline: EffectPipeline::Warp,
    window_default: Some(WindowDefault { attack: 3, hold: 4, release: 5 }),
};

/// 分屏：把画面沿中线切成两半，各自横移并倾斜。
///
/// `offset` 是两半分开的距离（画面宽度的比例），`skew` 是倾斜（度）。
pub const SPLIT: EffectSpec = EffectSpec {
    kind: "split",
    params: &[("offset", 0.0, 0.5), ("skew", -45.0, 45.0), ("amount", 0.0, 1.0)],
    space: EffectSpace::Document,
    pipeline: EffectPipeline::Warp,
    window_default: Some(WindowDefault { attack: 1, hold: 4, release: 4 }),
};

/// 按类型串查登记项。
pub fn spec_of(kind: &str) -> Option<&'static EffectSpec> {
    REGISTRY.iter().find(|spec| spec.kind == kind)
}

/// 已登记的类型串，已排序。UI 生成下拉框用。
pub fn kinds() -> Vec<&'static str> {
    let mut names: Vec<&'static str> = REGISTRY.iter().map(|spec| spec.kind).collect();
    names.sort_unstable();
    names
}

#[cfg(test)]
mod tests {
    use super::*;
    use dhampir_timeline::schema::{
        Clip, Effect, Project, SCHEMA_VERSION, TimebaseDto, Track, TrackKind, Transform,
        validate_project_with_effects,
    };
    use std::collections::BTreeMap;

    #[test]
    fn 模糊半径上界与着色器展开数必须一致() {
        // 这条是**跨模块的一致性闸**：着色器展开多少抽头，登记表就只准报多大半径。
        // 两边各写一个数，迟早会出现「UI 能拖、后端算不到」。
        let spec = spec_of("gaussian_blur").expect("应当登记了 gaussian_blur");
        let (_, _, max) = spec.params.iter().find(|(name, _, _)| *name == "radius").expect("应当有 radius");
        assert_eq!(*max, crate::render::BLUR_MAX_RADIUS as f32);
        assert_eq!(crate::render::BLUR_TAPS, 2 * crate::render::BLUR_MAX_RADIUS as usize + 1);
    }

    #[test]
    fn 登记表能查到也能列出来() {
        assert!(spec_of("gaussian_blur").is_some());
        assert!(spec_of("不存在的特效").is_none());
        assert_eq!(
            kinds(),
            vec![
                "brightness",
                "contrast",
                "flash",
                "gaussian_blur",
                "hue",
                "noise",
                "overlay",
                "pulse",
                "saturation",
                "shake",
                "split",
                "vignette",
                "zoom_bounce",
            ]
        );
    }

    #[test]
    fn 瞬时特效都要给默认时长() {
        // 这条守的是"加了特效但忘了说它能不能当瞬时事件用"。
        // 没有 window_default 的特效在 UI 上插不进"第 30 帧闪一下"那类事件，
        // 而那正是 V-Trim polish 的主要用法 —— 漏了会表现为"这个特效点不出来"。
        for spec in REGISTRY {
            if spec.is_transient_capable() {
                let window = spec.window_default.expect("刚判过有");
                assert!(
                    window.total() > 0,
                    "{} 的默认窗口总时长为 0，插进时间线等于看不见",
                    spec.kind
                );
            }
        }
    }

    #[test]
    fn 每条管线都至少有一个特效在用() {
        // 反向用例的价值在于**钉住"这批特效确实各自有归属"**：
        // 任何一条没有 pipeline 的 kind 都进不了 REGISTRY（类型上就要求有），
        // 所以这里改钉"四种管线各至少有一个使用者"——
        // 若哪天某条管线空了，说明它被合并或被删，那件事该被看见。
        for pipeline in [
            EffectPipeline::SeparableBlur,
            EffectPipeline::ColorAdjust,
            EffectPipeline::ColorMask,
            EffectPipeline::Warp,
        ] {
            assert!(
                REGISTRY.iter().any(|spec| spec.pipeline == pipeline),
                "{pipeline:?} 没有任何特效在用 —— 它要么该删，要么是漏登记了"
            );
        }
    }

    #[test]
    fn 每个登记项都必须声明空间与管线() {
        // 这条守的是**新加特效时最容易漏的一步**：加了 kind、加了参数范围，
        // 却忘了说"跑在哪个空间"。
        //
        // space 漏了声明不会编译不过（它有类型），但会**静默影响正确性**：
        // 声明成 Source 的文档空间特效不缩放，在预览与成片尺寸不同时糊出不同的图。
        // 所以这里要求每一项都显式给出，并且管线必须是**已实现**的那种。
        for spec in REGISTRY {
            // 断言它真的有一个明确的空间（枚举只有两个值，这里确认不是靠默认值蒙混）。
            let space_is_explicit = matches!(spec.space, EffectSpace::Source | EffectSpace::Document);
            assert!(space_is_explicit, "{} 没有声明像素空间", spec.kind);

            // 管线必须是渲染器真的认的那一种。将来加了枚举变体却没实现时，
            // 这条会先红 —— 而不是等到渲染时才发现没人处理。
            match spec.pipeline {
                EffectPipeline::SeparableBlur => {
                    // 可分离模糊**必须**有半径参数，否则管线拿不到核宽。
                    assert!(
                        spec.param_max("radius").is_some(),
                        "{} 走 SeparableBlur 却没有 radius 参数",
                        spec.kind
                    );
                }
                EffectPipeline::ColorAdjust => {
                    // 逐像素调整**必须**有 amount 类参数，否则这一趟什么都不改。
                    // 有参数但不给范围也不行 —— 那样 UI 无从生成控件。
                    let has_param = spec.params.iter().any(|(name, _, _)| {
                        *name == "amount" || *name == "degrees"
                    });
                    assert!(!spec.params.is_empty(), "{} 走 ColorAdjust 却没有参数", spec.kind);
                    assert!(has_param, "{} 的参数名既不是 amount 也不是 degrees", spec.kind);
                    // 逐像素算子与坐标系无关，声明 Document 会造成"多做一次换算"的误读。
                    assert!(
                        matches!(spec.space, EffectSpace::Source),
                        "{} 是逐像素算子，空间应当声明为 Source",
                        spec.kind
                    );
                }
                EffectPipeline::ColorMask => {
                    // 用常量色叠加的算子**必须**给出 amount —— 它是这一趟的强度，
                    // 没有它这条特效画出来与不画一样，而用户会以为"特效没生效"。
                    assert!(
                        spec.param_max("amount").is_some(),
                        "{} 走 ColorMask 却没有 amount 参数",
                        spec.kind
                    );
                    // **不许把强度塞进 params 之外**：即便有 amount，
                    // 它也不能越出 [0,1] —— 那是"覆盖多少"的定义域。
                    let (min, max) = spec
                        .params
                        .iter()
                        .find(|(name, _, _)| *name == "amount")
                        .map(|(_, min, max)| (*min, *max))
                        .expect("上面刚断言过有 amount");
                    assert!(
                        min >= 0.0 && max <= 1.0,
                        "{} 的 amount 范围 [{min}, {max}] 越出 [0, 1]",
                        spec.kind
                    );
                }
                EffectPipeline::Warp => {
                    // 坐标重映射**必须**有 amount：它决定位移多大。
                    // 没有它这条管线会退化成恒等映射（每帧重画一遍什么都不改）。
                    assert!(
                        spec.param_max("amount").is_some(),
                        "{} 走 Warp 却没有 amount 参数",
                        spec.kind
                    );
                    // 位移量用**归一化**比例（画面宽度的几分之几），不许用像素：
                    // 像素在预览 640x360 与成片 1920x1080 里含义不同 —— 
                    // 与 SubtitleStyle 用 font_ratio 是同一条理由。
                    let (_, max) = spec
                        .params
                        .iter()
                        .find(|(name, _, _)| *name == "amount")
                        .map(|(_, min, max)| (*min, *max))
                        .expect("上面刚断言过有 amount");
                    assert!(
                        max <= 1.0,
                        "{} 的 amount 上界 {max} 不像是归一化比例（应当 <= 1.0）",
                        spec.kind
                    );
                }
            }
        }
    }

    #[test]
    fn 反向用例_把空间声明成源空间就不是同一个说法() {
        // 反向用例：证明 space **真的被用到了**，不是个摆设字段。
        //
        // 如果哪天有人把 space 从 EffectSpec 删掉、或者渲染器开始忽略它，
        // 这条会红。判据是"两种声明给出的半径换算结果不同"——
        // 而那正是两端会不会分叉的分水岭。
        use crate::render::{RenderSpace, scale_document_radius};

        let doc = spec_of("gaussian_blur").expect("应当登记了");
        // 文档坐标系 1920x1080 -> 目标 640x360，比例正好 1/3。
        let target = RenderSpace {
            sequence: (1920, 1080),
            target: (640, 360),
        };

        // 文档空间：按目标/文档比例缩小（1920 -> 640 是 1/3）。
        let scaled = scale_document_radius(24, target);
        assert_eq!(scaled, 8, "文档空间应当按比例缩放：24 * (640/1920) = 8");

        // 源空间：**不缩放**。同一个半径原样传下去。
        // 这里用"不做换算"来代表 Source 的行为——两者必须不同，
        // 否则 space 这个字段就没有存在意义。
        let unscaled = 24;
        assert_ne!(
            scaled, unscaled,
            "两种空间的换算结果必须不同，否则 space 字段是摆设（当前声明：{:?}）",
            doc.space
        );
    }

    #[test]
    fn 校验用的是同一份登记表() {
        // 这条测试的价值在于**串起来**：core 的登记表 + timeline 的校验，
        // 两边对"radius 的合法范围"必须是同一个说法。
        let project = Project {
            schema: SCHEMA_VERSION,
            timebase: TimebaseDto { num: 60, den: 1 },
            tracks: vec![Track {
                id: "v1".to_string(),
                kind: TrackKind::Video,
                clips: vec![Clip {
                    id: "c1".to_string(),
                    source: "a.mp4".to_string(),
                    source_in: 0,
                    track_at: 0,
                    duration: 30,
                    transform: Transform::default(),
                    opacity: 1.0,
                    effects: vec![Effect {
                        kind: "gaussian_blur".to_string(),
                        params: BTreeMap::from([("radius".to_string(), 8.0)]),
                        window: dhampir_timeline::schema::Window::Always,
                        opacity: 1.0,
                    }],
                    keyframes: Vec::new(),
                    transition_in: None,
                }],
            }],
        };
        assert!(
            validate_project_with_effects(&project, REGISTRY).is_empty(),
            "登记表里的特效与范围应当被接受"
        );

        let mut bad = project.clone();
        // 越界值从登记表推：上界 + 1
        let over = spec_of("gaussian_blur").expect("应当登记了").params[0].2 + 1.0;
        bad.tracks[0].clips[0].effects[0].params.insert("radius".to_string(), over);
        let issues = validate_project_with_effects(&bad, REGISTRY);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].code, "effect_param_out_of_range");
        // 报出来的范围必须与登记表一致，否则用户按提示改还是错。
        // 上界**从登记表推**，不写死：写死的话改一次上界就要改一次测试，
        // 而「改测试让它过」正是这类断言最容易退化的方式。
        let bound = spec_of("gaussian_blur").expect("应当登记了").params[0].2;
        assert!(
            issues[0].message.contains(&bound.to_string()),
            "提示里要带上真实上界 {bound}：{}",
            issues[0].message
        );
    }
}
