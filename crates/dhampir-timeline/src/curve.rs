//! 关键帧求值曲线。**逻辑只有这一份**。
//!
//! # 为什么它住在 timeline，而不是 core
//!
//! 求值要用的东西全在 timeline：`Keyframe`（`schema.rs:176`）、
//! `Easing` 与它的 `apply`（`schema.rs:188` / `:198`）。
//! 依赖方向只能是 core → timeline（`scripts/check-dep-graph.mjs` 钉着），
//! 而剃刀（`edit.rs` 的 `split`）需要"切点那一刻的值"，它也在 timeline。
//!
//! 求值要是留在 core，timeline 就得再抄一份 —— **同一份逻辑两份实现**，
//! 两份会漂，而漂了**没有任何东西会红**。所以求值下沉到这里，
//! core 的 `compose.rs` 用 `pub use` 转发：语义不变、调用路径不变。
//!
//! # 搬过来时逐条照抄的语义
//!
//! 1. 先把键按 `frame` 排一次（契约不要求有序）；
//! 2. `local <= first.frame` → 取 `first.value`；
//! 3. `local >= last.frame` → 取 `last.value`；
//! 4. 否则在**相邻对**里插值，用的是**后一个键**的缓动（`b.easing`），
//!    `span <= 0` 时取 `t = 1.0` 防除零。

use crate::schema::{Frame, Keyframe};

/// 关键帧求值，**按 `target` 取某一条曲线**。
///
/// 这是 `opacity_from` 的泛化：老的那份只认"不透明度"这一个标量，
/// 于是"同时驱动 scale 与 x"（运镜）在契约里表达不出来。逻辑**仍然只有这一份** ——
/// `opacity_from` 是它的特化，不是另写一遍。
///
/// # 逐条照抄的老语义（改这里要连着看 `关键帧不要求有序` 那几条用例）
///
/// 1. 先按 `frame` 排一次（契约不要求有序）；
/// 2. `local <= first.frame` → 取 `first.value`；
/// 3. `local >= last.frame` → 取 `last.value`；
/// 4. 否则在**相邻对**里插值，用**后一个键**的缓动，`span <= 0` 时 `t = 1.0` 防除零。
///
/// # 与老版本唯一的行为差别
///
/// **只挑出 `target` 匹配的那些键**再走上面四步。一个键都不匹配 → 返回 `fallback`
/// （调用方给的静态值）。于是老工程（所有键都是缺省的 `"opacity"`）结果逐字节不变。
pub fn channel_from(fallback: f32, keyframes: &[Keyframe], target: &str, local_frame: Frame) -> f32 {
    let mut keys: Vec<&Keyframe> = keyframes
        .iter()
        .filter(|key| key.target == target)
        .collect();
    if keys.is_empty() {
        return fallback;
    }
    keys.sort_by_key(|key| key.frame);

    let first = keys[0];
    if local_frame <= first.frame {
        return first.value;
    }
    let last = keys[keys.len() - 1];
    if local_frame >= last.frame {
        return last.value;
    }
    for pair in keys.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        if local_frame >= a.frame && local_frame <= b.frame {
            let span = (b.frame - a.frame) as f32;
            // span 为 0 时两个关键帧在同一帧上——取后一个的值，别除零。
            let t = if span <= 0.0 { 1.0 } else { (local_frame - a.frame) as f32 / span };
            let eased = b.easing.apply(t);
            return a.value + (b.value - a.value) * eased;
        }
    }
    fallback
}

/// 关键帧求值。没有关键帧就用片段的静态不透明度。
///
/// 关键帧**不要求有序**——契约里没这么要求，所以这里先排一次。
/// 依赖"用户会按顺序写"是那种只在别人手写的工程上才会炸的假设。
pub fn opacity_from(opacity: f32, keyframes: &[Keyframe], local_frame: Frame) -> f32 {
    // 特化：target = "opacity"。**不是另写一遍逻辑**——那样两份会漂，
    // 而漂了没有任何东西会红（这正是这个模块头那段话的意思）。
    channel_from(opacity, keyframes, "opacity", local_frame)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::Easing;

    fn key(frame: Frame, value: f32, easing: Easing) -> Keyframe {
        Keyframe { frame, target: "opacity".to_string(), value, easing }
    }

    fn keyed(frame: Frame, target: &str, value: f32) -> Keyframe {
        Keyframe { frame, target: target.to_string(), value, easing: Easing::Linear }
    }

    #[test]
    fn 没有关键帧就给静态不透明度() {
        assert_eq!(opacity_from(0.4, &[], 7), 0.4);
    }

    #[test]
    fn 关键帧不要求有序() {
        // 反着写也必须算对：这正是"先排一次"存在的理由。
        let keys = [key(10, 1.0, Easing::Linear), key(0, 0.0, Easing::Linear)];
        assert!((opacity_from(0.0, &keys, 5) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn 范围外取端点不外推() {
        let keys = [key(0, 0.0, Easing::Linear), key(10, 1.0, Easing::Linear)];
        assert_eq!(opacity_from(0.0, &keys, -5), 0.0);
        assert_eq!(opacity_from(0.0, &keys, 999), 1.0);
    }

    #[test]
    fn 同一帧上的两个键不除零() {
        let keys = [key(5, 0.0, Easing::Linear), key(5, 1.0, Easing::Linear)];
        let value = opacity_from(0.0, &keys, 5);
        // 不断言具体值：这一帧会先被端点判定接走，走不到插值分支。
        // 这里要钉的是那个 `span <= 0` 的兜底不会产出 NaN/Inf。
        assert!(value.is_finite(), "不能是 NaN/Inf：{value}");
    }

    #[test]
    fn 插值用的是后一个键的缓动() {
        // 前一个键写 Linear、后一个写 EaseIn：斜率由**后一个**决定。
        let keys = [key(0, 0.0, Easing::Linear), key(10, 1.0, Easing::EaseIn)];
        assert!((opacity_from(0.0, &keys, 5) - 0.25).abs() < 1e-6, "ease_in 是 t*t");
    }

    // ===== T8：按 target 取通道 =====

    #[test]
    fn 只挑出匹配_target_的键() {
        // 同一份 keyframes 里混着两条曲线：opacity 与 scale。
        // 求 opacity 时**不许**被 scale 的键影响 —— 这是泛化最容易出的错。
        let keys = [
            keyed(0, "opacity", 0.0),
            keyed(10, "opacity", 1.0),
            keyed(0, "scale", 2.0),
            keyed(10, "scale", 8.0),
        ];
        assert!((channel_from(1.0, &keys, "opacity", 5) - 0.5).abs() < 1e-6);
        assert!((channel_from(1.0, &keys, "scale", 5) - 5.0).abs() < 1e-6);
    }

    #[test]
    fn 没有匹配的键就用调用方给的静态值() {
        let keys = [keyed(0, "scale", 2.0)];
        // 问的是 rotation：一个键都没有 -> fallback，**不是 0**。
        assert_eq!(channel_from(0.75, &keys, "rotation", 5), 0.75);
    }

    #[test]
    fn 运镜的常见形状能算对() {
        // 参照实现 的「近景」= 从 1.0 推到 1.5 再停住，同时横向移动。
        let keys = [
            keyed(0, "scale", 1.0),
            keyed(60, "scale", 1.5),
            keyed(0, "x", 0.0),
            keyed(60, "x", -0.25),
        ];
        assert!((channel_from(1.0, &keys, "scale", 30) - 1.25).abs() < 1e-6);
        assert!((channel_from(0.0, &keys, "x", 30) + 0.125).abs() < 1e-6);
        // 推到位之后停在那里，不外推。
        assert_eq!(channel_from(1.0, &keys, "scale", 999), 1.5);
    }

    #[test]
    fn 老工程_只写_value_的键被当成_opacity() {
        // 反序列化时没有 target 的键会被补成 "opacity" —— 老工程因此逐字节语义不变。
        let text = r#"{"frame":0,"value":0.25}"#;
        let parsed: Keyframe = serde_json::from_str(text).expect("老形状必须还能读");
        assert_eq!(parsed.target, "opacity");
        assert_eq!(parsed.value, 0.25);
    }

    #[test]
    fn effect_目标串能解析出下标与参数名() {
        let parsed = crate::schema::parse_effect_target("effect.2.radius").expect("该能解析");
        assert_eq!(parsed.index, 2);
        assert_eq!(parsed.param, "radius");

        // 参数名允许含点：按**第一个**点切，末段整体当参数名。
        let dotted = crate::schema::parse_effect_target("effect.0.color.r").expect("该能解析");
        assert_eq!(dotted.index, 0);
        assert_eq!(dotted.param, "color.r");
    }

    #[test]
    fn 不是_effect_形状的串一律不解析() {
        for bad in ["opacity", "effect", "effect.", "effect.x.radius", "effect.0.", "effect.0"] {
            assert!(
                crate::schema::parse_effect_target(bad).is_none(),
                "不该解析成功：{bad}"
            );
        }
    }
}
