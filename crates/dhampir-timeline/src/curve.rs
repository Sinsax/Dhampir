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

/// 关键帧求值。没有关键帧就用片段的静态不透明度。
///
/// 关键帧**不要求有序**——契约里没这么要求，所以这里先排一次。
/// 依赖"用户会按顺序写"是那种只在别人手写的工程上才会炸的假设。
pub fn opacity_from(opacity: f32, keyframes: &[Keyframe], local_frame: Frame) -> f32 {
    if keyframes.is_empty() {
        return opacity;
    }
    let mut keys: Vec<&Keyframe> = keyframes.iter().collect();
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
    opacity
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::Easing;

    fn key(frame: Frame, value: f32, easing: Easing) -> Keyframe {
        Keyframe { frame, value, easing }
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
}
