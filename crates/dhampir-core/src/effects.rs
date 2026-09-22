//! 特效注册表：类型串 → 参数规格（以及将来的 pipeline）。
//!
//! 这里只有**一份**登记表：T4.1 的校验拿它校对工程，T4.2 的渲染图拿它选 pipeline。
//! 分成两份的话，UI 上控件的取值范围和后端实际接受的取值范围迟早对不上——
//! 而那种不一致表现为"用户能拖到某个值，导出时才报错"。

use dhampir_timeline::schema::EffectSpec;

/// 高斯模糊。radius 是像素半径。
///
/// 上界**必须**等于着色器能展开的最大抽头数的一半（`render::BLUR_MAX_RADIUS`）。
/// 写死一个更大的数会让「用户能拖到 64」而着色器只按 16 算——
/// 那种不一致不报错，只是模糊得不够，正是这份登记表要防的事。
/// 有一条测试把两者钉在一起。
pub const GAUSSIAN_BLUR: EffectSpec = EffectSpec {
    kind: "gaussian_blur",
    params: &[("radius", 0.0, crate::render::BLUR_MAX_RADIUS as f32)],
};

/// 全部已登记的特效。
pub const REGISTRY: &[EffectSpec] = &[GAUSSIAN_BLUR];

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
        assert_eq!(kinds(), vec!["gaussian_blur"]);
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
