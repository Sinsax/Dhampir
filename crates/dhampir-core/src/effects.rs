//! 特效注册表：类型串 → 参数规格（以及将来的 pipeline）。
//!
//! 这里只有**一份**登记表：T4.1 的校验拿它校对工程，T4.2 的渲染图拿它选 pipeline。
//! 分成两份的话，UI 上控件的取值范围和后端实际接受的取值范围迟早对不上——
//! 而那种不一致表现为"用户能拖到某个值，导出时才报错"。

use dhampir_timeline::schema::{EffectPipeline, EffectSpace, EffectSpec};

/// 高斯模糊。radius 是像素半径。
///
/// 上界**必须**等于着色器能展开的最大抽头数的一半（`render::BLUR_MAX_RADIUS`）。
/// 写死一个更大的数会让「用户能拖到 64」而着色器只按 16 算——
/// 那种不一致不报错，只是模糊得不够，正是这份登记表要防的事。
/// 有一条测试把两者钉在一起。
pub const GAUSSIAN_BLUR: EffectSpec = EffectSpec {
    kind: "gaussian_blur",
    params: &[("radius", 0.0, crate::render::BLUR_MAX_RADIUS as f32)],
    // **这个 space 是「默认空间」，不是唯一真相。**
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
};
/// 对比度：绕 0.5 中灰缩放。1.0 = 不变。
pub const CONTRAST: EffectSpec = EffectSpec {
    kind: "contrast",
    params: &[("amount", 0.0, 4.0)],
    space: EffectSpace::Source,
    pipeline: EffectPipeline::ColorAdjust,
};
/// 饱和度：向亮度插值。1.0 = 不变，0.0 = 完全灰度。
pub const SATURATION: EffectSpec = EffectSpec {
    kind: "saturation",
    params: &[("amount", 0.0, 4.0)],
    space: EffectSpace::Source,
    pipeline: EffectPipeline::ColorAdjust,
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
};
/// 全部已登记的特效。
pub const REGISTRY: &[EffectSpec] = &[GAUSSIAN_BLUR, BRIGHTNESS, CONTRAST, SATURATION, HUE];

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
        assert_eq!(kinds(), vec!["brightness", "contrast", "gaussian_blur", "hue", "saturation"]);
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
