//! 文字的**逐帧动效**：弹入缩放（T1）/ 逐条强调（T3）/ 抖动（T4）。
//!
//! # 为什么单独一个文件，而不是塞进 `layer.rs`
//!
//! 这个模块是 2026-10-11 那趟的产物，起因值得留在这里 —— **两个任务并行改了
//! 同一个 `layer.rs`，后写的把先写的整份覆盖掉了**（`grep` 出来符号凭空消失，
//! 没有提交、没有报错，只在下游留下"函数找不到"）。
//!
//! 现在的分工是**按"谁动 `layer.rs`"切的**：
//!
//! * `layer.rs` 只留 `SubtitleStyle` 上的 **6 行字段声明 + 6 行 `Default`**
//!   （另一项工作也要往同一个结构体加字段，那是最窄的接触面）；
//! * **逻辑全在这里** —— 曲线、抖动相位表、包络求值。
//!
//! 于是"整份写回 `layer.rs`"这种事故的代价从 ~180 行降到 12 行。
//! **往后往 `SubtitleStyle` 加字段，逻辑请放本模块或各自的文件，别继续堆进 `layer.rs`。**
//!
//! # 三条口径（与 `layer.rs` 上那六个字段的文档注释是同一套）
//!
//! 1. **`None` = 用默认行为**。六个字段全是 `Option`，于是"没写这个键"
//!    与"写了默认值"分得开 —— 老工程重写**一个键都不多**，
//!    而用户显式写的值（哪怕恰好等于默认值）**永远不被吃掉**。
//! 2. **`None` 时求值结果必须是"单位变换"**：缩放恒 1.0、抖动恒 0。
//!    这是"存量零回归"的执行处 —— 老工程走到底与改动前逐字节相同。
//! 3. **纯函数、无累积状态**。抖动尤其重要：靠"上一帧在哪"累加的话，
//!    `seek` 过去与顺序播放会分叉，而预览是 `seek` 的、成片是顺序的
//!    （本仓 `handheld_sway` 已立过这条规矩）。

use crate::layer::SubtitleStyle;

/// 抖动频率的默认值（Hz）—— 这一组里**唯一的**非零默认。
///
/// ⚠️ 它**不是** `shake_hz` 的 serde 默认（那个字段是 `Option`、缺省即 `None`），
/// 而是**求值处** [`SubtitleStyle::shake_hz_value`] 的落点。两者不要混：把字段写成
/// `#[serde(default = "default_shake_hz")]` 会让"没写这个键"的老工程读出一个 `18.0`，
/// 于是它与其他五个字段**形状不一致**，而更要命的是
/// "没写"与"写了 18.0"从此分不出来 —— 那正是这一组用 `Option` 要避免的事。
pub const DEFAULT_SHAKE_HZ: f32 = 18.0;

/// [`DEFAULT_SHAKE_HZ`] 的函数形式（给 `unwrap_or_else` 用）。
pub fn default_shake_hz() -> f32 {
    DEFAULT_SHAKE_HZ
}

/// `back_out(t, s = 3)` —— 参照实现 的 `E.back_out(p, 3)`。
///
/// ```text
/// function(t, s) { s = s || 1.70158; return --t * t * ((s + 1) * t + s) + 1; }
/// ```
///
/// # 为什么重写一份、不用 [`crate::easing::EasingForm::BackOut`]
///
/// 那个枚举的系数**写死 1.70158**（经典值，峰值 ≈ **1.1000**），
/// 而参照实现 的贴纸弹入用的是 **`s = 3`**（峰值 **1.2500**）。
/// 两者差 0.15 的过冲量，是"弹得脆"与"弹得软"的区别。
///
/// 契约的 [`crate::easing::Easing`] 是不带参数的枚举、装不下 `s`，
/// 所以这一条曲线按公式算。**必须与 YekiTrim 侧 `backOut3` 逐值一致**
/// （那边的守卫 `check-yekitrim-translator.mjs` 钉着同一条公式）——
/// 两边不一致的症状是"预览弹的幅度与成片不同"，而两边各自看都正常。
///
/// # ⚠️ 峰值是 **1.25**，而 `scale_in_peak` 是**端点**不是峰值
///
/// 这两件事很容易混，写清楚：`back_out3(1) == 1`，所以
/// `from + (peak - from)·back_out3(p)` 的**末值是 `peak`**；
/// 而曲线在中点 `p = 0.5` 冲到 `1.25`，于是**实际最大值**是
/// `from + (peak - from)·1.25`。`from = 0.7` / `peak = 1.15` 时它是
/// **1.2625**，不是 1.15。
///
/// 判据在 `弹入窗口内先小后过冲再回落` 里逐值量过。
pub fn back_out3(t: f32) -> f32 {
    const S: f32 = 3.0;
    let t = t.clamp(0.0, 1.0);
    let u = t - 1.0;
    u * u * ((S + 1.0) * u + S) + 1.0
}

/// 一条文字**这一帧**的抖动偏移（像素，文档坐标系；正为向右/向下）。
///
/// # 相位表（两个正弦叠加）
///
/// ```text
/// dx = shake_px * ( 0.62·sin(2π·hz·t     + phase)
///                 + 0.38·sin(2π·hz·2.7·t + phase·1.9) )
/// dy = shake_px * ( 0.62·sin(2π·hz·1.3·t + phase + 1.1)
///                 + 0.38·sin(2π·hz·3.1·t + phase·0.6 + 2.3) )
/// ```
///
/// 第二条的频率是第一条的 **2.7× / 3.1×**（**不是**整数倍）：整数倍会让叠加
/// 在每个基频周期严格重复，看着像机械抖动；非整数倍才有手抖的观感。
/// 两条都乘 `shake_px` ⇒ **幅度线性于 `shake_px`**（判据之一）。
///
/// 两个方向用的频率也**不同**（`2.7` vs `3.1`、`1.3` vs `1.0`）——
/// 都相同的话轨迹退化成一条直线（对角往返），而不是抖。
///
/// # 三个"关"的分支都要显式挡
///
/// - `shake_px <= 0` 或非有限 ⇒ `(0, 0)`。「关」与「开着但幅度 0」是同一件事。
/// - `shake_hz <= 0` ⇒ **也要挡**。0 Hz 代入公式**不会崩**：`sin(phase)` 是个
///   常数，于是整条字**偏移一个固定量** —— 那不是"不抖"，是"错位"，
///   而且错位量随相位不同，同屏多条会散在各自的位置上。
/// - `shake_phase` 非有限 ⇒ 当 0 用（不把 NaN 传进 `sin` 污染整条链）。
/// - `t_sec` 非有限 ⇒ `(0, 0)`（`NaN` 会顺着 `sin` 传染到落点上）。
pub fn text_shake_px(t_sec: f32, shake_px: f32, shake_hz: f32, shake_phase: f32) -> (f32, f32) {
    if !(shake_px > 0.0) || !shake_px.is_finite() {
        return (0.0, 0.0);
    }
    if !(shake_hz > 0.0) || !shake_hz.is_finite() || !t_sec.is_finite() {
        return (0.0, 0.0);
    }
    let phase = if shake_phase.is_finite() {
        shake_phase
    } else {
        0.0
    };
    let w = std::f32::consts::TAU * shake_hz;
    let dx = shake_px
        * (0.62 * (w * t_sec + phase).sin() + 0.38 * (w * 2.7 * t_sec + phase * 1.9).sin());
    let dy = shake_px
        * (0.62 * (w * 1.3 * t_sec + phase + 1.1).sin()
            + 0.38 * (w * 3.1 * t_sec + phase * 0.6 + 2.3).sin());
    (dx, dy)
}

/// 一条字幕**这一帧**的三样：`(不透明度, 纵向偏移, 缩放倍率)`。
///
/// 前两样的口径与 [`crate::layer::text_envelope`] **逐字相同**（那一份是
/// "不关心缩放"的薄壳，内部就走这里）。第三样是 T1 弹入：
///
/// ```text
/// scale_in_ms == 0                         ->  scale 恒 1.0（不缩 = 改动前的行为）
/// scale_in_ms >  0 且 local < scale_in_ms  ->  p = local / scale_in_ms
///                                             scale = from + (peak - from)·back_out3(p)
/// 其余                                      ->  scale 恒 1.0
/// ```
///
/// # ⚠️ 窗口**过了就恒为 1.0**，不是停在 `peak`
///
/// `back_out3(1) == 1`，所以 `from + (peak-from)·1 == peak` —— 曲线**终点是 `peak`**。
/// 要的是"弹过头再回落"，所以窗口外**必须显式回到 1.0**。若写成"窗口外也给 `peak`"，
/// 文字会**一直比正常大 15%**，而那看着像"字号配错了"，不像"弹入没结束"。
///
/// # 与淡入的关系：**各走各的窗口**
///
/// 缩放窗口 `scale_in_ms` 与淡入窗口 `fade_in_ms` 是两个独立时长
/// （参照的两个值恰好都在 180ms 附近，但契约不绑死它们）。
/// 不共用一条 `p`：共用会让"只想弹、不想淡"在契约上表达不出来。
///
/// # `span_ms == 0`
///
/// 与 [`crate::layer::text_envelope`] 同款：**没有时长就没有包络**，
/// 给 `(1.0, 0.0, 1.0)`（完全不透明、不位移、不缩），而不是"算不出来"。
#[allow(clippy::too_many_arguments)]
pub fn text_envelope_scaled(
    local_ms: u64,
    span_ms: u64,
    fade_in_ms: u64,
    fade_out_ms: u64,
    rise_in_px: f32,
    rise_out_px: f32,
    scale_in_from: f32,
    scale_in_peak: f32,
    scale_in_ms: u64,
) -> (f32, f32, f32) {
    let (opacity, offset) = crate::layer::text_envelope_plain(
        local_ms,
        span_ms,
        fade_in_ms,
        fade_out_ms,
        rise_in_px,
        rise_out_px,
    );
    // **窗口内才动，窗口外恒 1.0**（见上面那段"过了就恒为 1.0"）。
    let scale = if span_ms > 0 && scale_in_ms > 0 && local_ms < scale_in_ms {
        let p = local_ms as f32 / scale_in_ms as f32;
        scale_in_from + (scale_in_peak - scale_in_from) * back_out3(p)
    } else {
        1.0
    };
    (opacity, offset, scale)
}

/// 从轨道样式取"这一帧的缩放端点"，`None` 落到单位值。
///
/// 抽成函数是为了让**求值处只有一处 `unwrap_or`** —— 六个字段散在调用点里
/// 各写一遍 `unwrap_or`，就一定会有一处漏掉，而漏掉的那一处
/// 在"字段写了但没生效"之前**不会有任何症状**。
pub fn scale_in_params(style: &SubtitleStyle) -> (f32, f32, u64) {
    (
        style.scale_in_from.unwrap_or(1.0),
        style.scale_in_peak.unwrap_or(1.0),
        style.scale_in_ms.unwrap_or(0),
    )
}

/// 周期缩放曲线上的一个点：`(毫秒, 倍率)`。
///
/// ⚠️ 单独一个结构而**不是**元组：它进的是工程文件（`.doc.json`），
/// `[[0,1.05],[120,1.0]]` 这种位置数组在报告与排错里谁也读不出来。
#[derive(Debug, Clone, Copy, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct ScalePoint {
    /// 相对**周期起点**的毫秒。
    pub ms: u32,
    /// 这一点的缩放倍率（1.0 = 基线）。
    pub scale: f32,
}

/// 从轨道样式取 `(点列, 周期毫秒)`。`None` = 不启用（走 `scale_in_*` 老路）。
///
/// 与 `scale_in_params` / `shake_params` 同一条理由：启用判断**只在这里出现一次** ——
/// 散在调用点就会有一处漏掉，而漏掉的那处在"字段写了但没生效"之前**没有任何症状**。
pub fn scale_curve_params(style: &SubtitleStyle) -> Option<(&[ScalePoint], u32)> {
    let cycle = style.scale_cycle_ms?;
    let points = style.scale_curve.as_deref()?;
    // < 2 个点画不出"上去再回来"；周期 0 没法取模。两者都当"不启用"（不 panic、不猜）。
    if cycle == 0 || points.len() < 2 {
        return None;
    }
    Some((points, cycle))
}

/// **周期缩放曲线**在 `local_ms` 处的倍率（心跳）。见 `SubtitleStyle::scale_curve`。
///
/// 语义：取 `local_ms % cycle_ms`，落在相邻两点之间就用 `back_out3` 插值
/// （每段各自起落 —— 于是"冲上去"和"弹回来"两端都带一点过冲，手感就是心跳）。
/// 末点之后到周期结束返回 1.0（**停顿**；没有停顿就成抖动不是心跳）。
///
/// ⚠️ **纯函数**（只吃 `local_ms`，无累积状态）—— 与 T4 抖动同纪律：
/// 预览要能任意 `seek`，带状态的曲线一 seek 就与成片分叉。
pub fn scale_curve_at(local_ms: u64, cycle_ms: u32, points: &[ScalePoint]) -> f32 {
    if cycle_ms == 0 || points.len() < 2 {
        return 1.0;
    }
    let t = (local_ms % cycle_ms as u64) as u32;
    let mut prev = points[0];
    for p in &points[1..] {
        if t <= p.ms {
            let span = p.ms.saturating_sub(prev.ms);
            if span == 0 {
                return p.scale;
            }
            let u = (t.saturating_sub(prev.ms)) as f32 / span as f32;
            return prev.scale + (p.scale - prev.scale) * back_out3(u);
        }
        prev = *p;
    }
    // 末点之后 ⇒ 本周期剩余时间**停在基线**（这就是"停顿"）。
    1.0
}

/// 从轨道样式取"这一帧的抖动参数"，`None` 落到（0.0, 18.0, 0.0）。
///
/// 与 [`scale_in_params`] 同一条理由：`unwrap_or` 只在这里出现一次。
pub fn shake_params(style: &SubtitleStyle) -> (f32, f32, f32) {
    (
        style.shake_px.unwrap_or(0.0),
        style.shake_hz.unwrap_or(DEFAULT_SHAKE_HZ),
        style.shake_phase.unwrap_or(0.0),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

// =====================================================================
    // 周期缩放（心跳，2026-10-18）
    // =====================================================================

    fn heartbeat_points() -> Vec<ScalePoint> {
        vec![
            // ⚠️ **首点是基线（1.0）而不是峰值**：曲线的语义是"从上一个点过渡到下一个点"，
            //    写 `1.05 → 1.0` 就成了"一开始就最大、然后缩小"（那是收尾，不是心跳）。
            ScalePoint { ms: 0, scale: 1.0 },
            ScalePoint { ms: 60, scale: 1.05 },
            ScalePoint { ms: 140, scale: 1.0 },
            ScalePoint { ms: 220, scale: 1.02 },
            ScalePoint { ms: 320, scale: 1.0 },
        ]
    }

    /// 形状：主峰起、回落、小峰起、回落、然后停顿到周期末尾。
    ///
    /// 变异自证：把末点之后那句 1.0 改成"返回最后一点的 scale" ⇒ 停顿那条断言红
    /// （停顿消失、读起来就成抖动了）。
    #[test]
    fn scale_curve_shape_is_two_peaks_then_a_pause() {
        let pts = heartbeat_points();
        let cyc = 1000u32;
        let at = |ms: u64| scale_curve_at(ms, cyc, &pts);
        assert!((at(0) - 1.0).abs() < 1e-6, "t=0 应当是基线，实际 {}", at(0));
        assert!((at(60) - 1.05).abs() < 1e-6, "t=60 是主峰，实际 {}", at(60));
        assert!((at(140) - 1.0).abs() < 1e-6, "t=140 回到基线，实际 {}", at(140));
        assert!((at(220) - 1.02).abs() < 1e-6, "t=220 是小峰，实际 {}", at(220));
        assert!(at(30) > 1.0, "主峰之前应当在往上走，实际 {}", at(30));
        for t in [320u64, 500, 700, 999] {
            assert!((at(t) - 1.0).abs() < 1e-6, "t={t} 应当停在基线，实际 {}", at(t));
        }
    }

    /// 周期性：t 与 t+cycle 逐值相同；非法参数一律 1.0（不 panic、不猜）。
    #[test]
    fn scale_curve_is_cyclic_and_safe() {
        let pts = heartbeat_points();
        for t in [0u64, 60, 120, 250, 700] {
            assert_eq!(scale_curve_at(t, 1000, &pts), scale_curve_at(t + 1000, 1000, &pts));
            assert_eq!(scale_curve_at(t, 1000, &pts), scale_curve_at(t + 3000, 1000, &pts));
        }
        assert_eq!(scale_curve_at(100, 0, &pts), 1.0, "周期 0 就不启用");
        assert_eq!(scale_curve_at(100, 1000, &[]), 1.0, "没有点就不启用");
        assert_eq!(
            scale_curve_at(100, 1000, &[ScalePoint { ms: 0, scale: 1.5 }]),
            1.0,
            "只有一个点画不出上去再回来"
        );
    }

    /// scale_curve_params：缺一个字段就不启用 —— 启用判断只在这里出现一次。
    #[test]
    fn scale_curve_params_requires_both_fields() {
        let mut style = SubtitleStyle::default();
        assert!(scale_curve_params(&style).is_none(), "两个都没写就不启用");
        style.scale_curve = Some(heartbeat_points());
        assert!(scale_curve_params(&style).is_none(), "只有点列、没有周期，仍不启用");
        style.scale_cycle_ms = Some(1000);
        let (pts, cyc) = scale_curve_params(&style).expect("两个都有才启用");
        assert_eq!(cyc, 1000);
        assert_eq!(pts.len(), 5);
        style.scale_cycle_ms = Some(0);
        assert!(scale_curve_params(&style).is_none(), "周期 0 就不启用");
    }

    // =====================================================================
    // T1 弹入
    // =====================================================================

    /// 参照的三个值：`0.7 → 1.15 → 1.0`，窗口 180ms。
    const FROM: f32 = 0.7;
    const PEAK: f32 = 1.15;
    const MS: u64 = 180;

    #[test]
    fn 弹入窗口内先小后过冲再回落() {
        // p = 0 时应当**恰好**是 from（起点，没有过冲）。
        let (_, _, at0) = text_envelope_scaled(0, 1000, 0, 0, 0.0, 0.0, FROM, PEAK, MS);
        assert!((at0 - FROM).abs() < 1e-5, "p=0 应当是起点 {FROM}，实际 {at0}");

        // 峰值必须**真的超过** 1.0 —— 这是"过冲"的定义，
        // 也是"曲线系数必须是 s=3 而不是经典 1.70158"的判据：
        // 经典系数下 `peak=1.15` 是**到不了**的（最多 1.10），
        // 而这里按下限断言 —— 只要过冲真的发生。
        // 峰值。
        //
        // ⚠️ **`scale_in_peak` 是曲线的"终点"，不是"实际最大值"** ——
        // 这条判据量的是**实际最大值**，所以它与字段值**不相等**：
        //
        //     scale(p) = from + (peak - from)·back_out3(p)
        //              = 0.7 + 0.45·back_out3(p)
        //
        // `back_out3` 自己在中点 `p = 0.5` 冲到 **1.25**（s=3 的固有峰），
        // 于是实际最大值 = `0.7 + 0.45×1.25 = 1.2625`。
        // 实测确认过：`back_out3 peak = 1.250000 at t=0.5000`。
        //
        // 这不是 bug，是"过冲曲线 + 线性插值端点"的必然结果，而它必须
        // **被写下来** —— 否则下一个人会照着 `peak` 的名字以为峰值就是 1.15，
        // 然后"修"成他以为的样子。
        let mut peak_seen = f32::MIN;
        for ms in 0..MS {
            let (_, _, s) = text_envelope_scaled(ms, 1000, 0, 0, 0.0, 0.0, FROM, PEAK, MS);
            peak_seen = peak_seen.max(s);
        }
        assert!(
            peak_seen > 1.0 + 0.10,
            "峰值要明显超过 1.0（过冲），实际 {peak_seen}"
        );
        assert!(
            (peak_seen - 1.2625).abs() < 0.02,
            "实际峰值应当是 0.7 + 0.45*1.25 = 1.2625（**不是** peak=1.15），实际 {peak_seen}"
        );

        // 末帧（p→1）回到 `peak` —— `back_out3(1) == 1` 的直接后果。
        // 这一条才是"`peak` 是端点"的口径。
        let (_, _, last) = text_envelope_scaled(MS - 1, 1000, 0, 0, 0.0, 0.0, FROM, PEAK, MS);
        assert!(
            (last - PEAK).abs() < 0.02,
            "窗口末帧应当接近 peak={PEAK}（端点的口径），实际 {last}"
        );
    }

    /// ⚠️ **窗口过了就恒为 1.0**，不是停在 `peak`。
    ///
    /// 这条是"回落"的判据。写成"窗口外也给 peak"的话，文字会一直大 15%，
    /// 而那看着像"字号配错了" —— 没人会往"弹入没结束"想。
    #[test]
    fn 弹入窗口外恒为一() {
        for ms in [MS, MS + 1, 500, 999, 1000] {
            let (_, _, s) = text_envelope_scaled(ms, 1000, 0, 0, 0.0, 0.0, FROM, PEAK, MS);
            assert!(
                (s - 1.0).abs() < 1e-6,
                "过了窗口（{ms}ms >= {MS}ms）缩放必须恒为 1.0，实际 {s}"
            );
        }
    }

    /// `scale_in_ms == 0` ⇒ **不缩**（既有工程逐字节不变的那条路）。
    #[test]
    fn 窗口为零时不缩() {
        for ms in [0, 1, 90, 179, 500] {
            let (_, _, s) = text_envelope_scaled(ms, 1000, 0, 0, 0.0, 0.0, FROM, PEAK, 0);
            assert!(
                (s - 1.0).abs() < 1e-6,
                "scale_in_ms=0 必须恒为 1.0，实际 {s}"
            );
        }
    }

    /// `from == peak` ⇒ 曲线**退化但不变**（全程那一个值）。
    ///
    /// 这条钉的是"别把退化情况写崩"：`from == peak == 1.0` 正是"字段没写"
    /// 时求值处的输入（`unwrap_or(1.0)`），它必须给出恒 1.0。
    #[test]
    fn 端点相同则曲线退化为常数() {
        for ms in [0, 90, 179] {
            let (_, _, s) = text_envelope_scaled(ms, 1000, 0, 0, 0.0, 0.0, 1.0, 1.0, MS);
            assert!(
                (s - 1.0).abs() < 1e-6,
                "from == peak == 1.0 时必须恒 1.0，实际 {s}"
            );
        }
    }

    /// **缩放与淡入各走各的窗口**（两个时长互不影响）。
    #[test]
    fn 缩放窗口与淡入窗口互不干扰() {
        // 淡入 180ms、缩放窗口 0 ⇒ 有淡入、**不缩**。
        let (o1, _, s1) = text_envelope_scaled(0, 1000, 180, 0, 0.0, 0.0, FROM, PEAK, 0);
        assert!(o1 < 1.0, "淡入应当生效，实际 opacity={o1}");
        assert!((s1 - 1.0).abs() < 1e-6, "缩放窗口为 0 ⇒ 不缩，实际 {s1}");

        // 缩放 180ms、淡入 0 ⇒ 缩、**不淡**。
        let (o2, _, s2) = text_envelope_scaled(0, 1000, 0, 0, 0.0, 0.0, FROM, PEAK, MS);
        assert!((o2 - 1.0).abs() < 1e-6, "没写淡入 ⇒ 不透明，实际 {o2}");
        assert!((s2 - FROM).abs() < 1e-5, "缩放应当已生效，实际 {s2}");
    }

    // =====================================================================
    // T4 抖动
    // =====================================================================

    /// 抖动是**纯 `t` 的函数** —— 同一个 `t` 永远给同一个答案。
    ///
    /// 这条钉的是"无累积状态"：若实现靠"上一帧在哪"累加，则**乱序求值**
    /// 会给出与顺序求值不同的结果，而预览是 `seek`（乱序）、成片是顺序的。
    #[test]
    fn 抖动是无状态纯函数() {
        let ts: Vec<f32> = (0..64).map(|i| i as f32 * 0.017).collect();
        let forward: Vec<(f32, f32)> =
            ts.iter().map(|&t| text_shake_px(t, 8.0, 18.0, 0.0)).collect();
        // 倒序再算一遍，必须**逐值**相同。
        let mut backward: Vec<(f32, f32)> = ts
            .iter()
            .rev()
            .map(|&t| text_shake_px(t, 8.0, 18.0, 0.0))
            .collect();
        backward.reverse();
        assert_eq!(forward, backward, "乱序求值与顺序求值必须逐值相同");
    }

    /// 幅度**线性于 `shake_px`**（相位表那两条都乘它）。
    #[test]
    fn 抖动幅度线性于幅度参数() {
        let mut seen_nonzero = false;
        for i in 0..40 {
            let t = i as f32 * 0.011;
            let (dx1, dy1) = text_shake_px(t, 4.0, 18.0, 0.0);
            let (dx2, dy2) = text_shake_px(t, 8.0, 18.0, 0.0);
            assert!(
                (dx2 - dx1 * 2.0).abs() < 1e-5 && (dy2 - dy1 * 2.0).abs() < 1e-5,
                "t={t}: 幅度翻倍应当位移翻倍，实际 ({dx1},{dy1}) -> ({dx2},{dy2})"
            );
            if dx1.abs() > 1e-3 || dy1.abs() > 1e-3 {
                seen_nonzero = true;
            }
        }
        assert!(seen_nonzero, "判据空转：全程都没有非零位移，比不出线性");
    }

    /// ⚠️ **两个方向必须是两条不同的轨迹** —— 不能 `dy ≡ dx`。
    ///
    /// # 这条是被变异台打出来的
    ///
    /// 下面那条 `抖动双向变号且不漂移` **抓不住**"把 `dy` 的相位表抄成 `dx` 的"
    /// 这个变异：两条轨迹重合之后，每个轴**自己**照样正负都出现、均值照样是 0，
    /// 于是那条判据仍然全绿。而画面上的症状是**文字沿 45° 直线来回跑**
    /// （不是"抖"，是"擦"），比不抖更难看，却没有任何判据盯着。
    ///
    /// 所以补这一条：两条轨迹**必须在绝大多数时刻不相等**。
    #[test]
    fn 抖动两个方向是不同轨迹() {
        let n = 400;
        let mut differing = 0;
        for i in 0..n {
            let t = i as f32 / 60.0;
            let (dx, dy) = text_shake_px(t, 6.0, 18.0, 0.0);
            if (dx - dy).abs() > 1e-3 {
                differing += 1;
            }
        }
        assert!(
            differing > n * 9 / 10,
            "dx 与 dy 必须是两条不同的轨迹（重合 = 沿 45° 直线擦，不是抖）；\
             只有 {differing}/{n} 帧不同"
        );
    }

    /// 位移**随时间变号**、且**不是单向漂移** —— 这是"抖"与"飘走"的区别。
    ///
    /// ⚠️ 这条**单独不足以**证明两个方向不同（见上面 `抖动两个方向是不同轨迹`），
    /// 两条一起才盖住"轨迹"这件事。
    #[test]
    fn 抖动双向变号且不漂移() {
        let n = 600;
        let mut dx_signs = std::collections::BTreeSet::new();
        let mut dy_signs = std::collections::BTreeSet::new();
        let mut sum_dx = 0.0_f32;
        let mut max_abs = 0.0_f32;
        for i in 0..n {
            let t = i as f32 / 60.0; // 10 秒 @60fps
            let (dx, dy) = text_shake_px(t, 6.0, 18.0, 0.0);
            if dx > 1e-4 {
                dx_signs.insert(1);
            } else if dx < -1e-4 {
                dx_signs.insert(-1);
            }
            if dy > 1e-4 {
                dy_signs.insert(1);
            } else if dy < -1e-4 {
                dy_signs.insert(-1);
            }
            sum_dx += dx;
            max_abs = max_abs.max(dx.abs()).max(dy.abs());
        }
        assert_eq!(dx_signs.len(), 2, "横向必须正负都出现过（不然是单向漂移）");
        assert_eq!(dy_signs.len(), 2, "纵向必须正负都出现过（不然是单向漂移）");
        // 均值为 0 ≈ 不漂移。10 秒的零均值正弦，取整误差留一点余量。
        let mean = sum_dx / n as f32;
        assert!(mean.abs() < 0.5, "横向均值应当接近 0（不漂移），实际 {mean}");
        // 峰值不能超过幅度上限（两条各 0.62 + 0.38 = 1.0，正好是 shake_px）。
        assert!(
            max_abs <= 6.0 + 1e-3,
            "位移峰值不能超过 shake_px=6.0，实际 {max_abs}"
        );
    }

    /// 关的分支：`shake_px <= 0`、`hz <= 0`、非有限输入**一律给零**。
    ///
    /// ⚠️ 其中 `hz == 0` 这条最值得有：0 Hz 代入公式**不会崩**，
    /// `sin(phase)` 是个常数 ⇒ 整条字**偏移一个固定量** ——
    /// 那是"错位"不是"不抖"，而且错位量随相位不同。
    #[test]
    fn 抖动关闭与非法输入都给零() {
        // shake_px <= 0
        for px in [0.0, -1.0, -0.001] {
            assert_eq!(text_shake_px(1.0, px, 18.0, 0.0), (0.0, 0.0), "px={px}");
        }
        // hz <= 0 —— **必须显式挡住**（理由见上）
        for hz in [0.0, -5.0] {
            assert_eq!(text_shake_px(1.0, 8.0, hz, 1.3), (0.0, 0.0), "hz={hz}");
        }
        // 非有限
        assert_eq!(text_shake_px(f32::NAN, 8.0, 18.0, 0.0), (0.0, 0.0), "t=NaN");
        assert_eq!(
            text_shake_px(f32::INFINITY, 8.0, 18.0, 0.0),
            (0.0, 0.0),
            "t=inf"
        );
        assert_eq!(text_shake_px(1.0, f32::NAN, 18.0, 0.0), (0.0, 0.0), "px=NaN");
        assert_eq!(text_shake_px(1.0, 8.0, f32::NAN, 0.0), (0.0, 0.0), "hz=NaN");
        // 相位非有限 ⇒ 当 0 用（**不是**给零位移：幅度与频率都合法，它在抖）
        let with_nan_phase = text_shake_px(0.3, 8.0, 18.0, f32::NAN);
        let with_zero_phase = text_shake_px(0.3, 8.0, 18.0, 0.0);
        assert_eq!(
            with_nan_phase, with_zero_phase,
            "相位 NaN 应当按 0 处理，而不是把 NaN 传进 sin"
        );
        assert!(
            with_nan_phase != (0.0, 0.0),
            "相位非法时**仍然要抖**（只有相位那一项退化成 0）"
        );
    }

    /// 不同相位给出**不同**的轨迹 —— 同屏多条要能错开。
    #[test]
    fn 不同相位给出不同轨迹() {
        let a = text_shake_px(0.25, 8.0, 18.0, 0.0);
        let b = text_shake_px(0.25, 8.0, 18.0, 1.9);
        assert_ne!(a, b, "相位不同必须给出不同位移（不然同屏多条会整体同拍）");
    }

    /// `back_out3` 的端点与峰值（与 YekiTrim 侧 `backOut3` 同一条公式）。
    #[test]
    fn back_out3_端点与峰值() {
        assert!((back_out3(0.0) - 0.0).abs() < 1e-6, "起点必须是 0");
        assert!((back_out3(1.0) - 1.0).abs() < 1e-6, "终点必须是 1");
        // 过冲峰值 ≈ 1.25（s=3）。经典 s=1.70158 只有 ≈1.10。
        let peak = (0..=1000)
            .map(|i| back_out3(i as f32 / 1000.0))
            .fold(f32::MIN, f32::max);
        assert!(
            peak > 1.20,
            "s=3 的峰值应当 ≈1.25（明显高于经典的 1.10），实际 {peak}"
        );
        // 夹取：越界输入不许外推。
        assert!((back_out3(-5.0) - 0.0).abs() < 1e-6, "负输入要夹到 0");
        assert!((back_out3(5.0) - 1.0).abs() < 1e-6, "超界输入要夹到 1");
    }

    // =====================================================================
    // R4：默认值必须逐字节等价于"改动前"
    // =====================================================================

    /// **渲染层 R4**：`SubtitleStyle::default()`（即老工程）求值出来
    /// 必须是**单位变换** —— 缩放恒 1.0、抖动恒 0。
    ///
    /// 这条是"存量零回归"在**求值层**的判据（JSON 键那一层在 `layer.rs` 的
    /// `v2_tests` 里）。两层都要有：JSON 那条抓"多写了键"，
    /// 这条抓"默认值走岔了"（比如把 `shake_px` 的默认写成 1.0）。
    #[test]
    fn 默认样式求值出来是单位变换() {
        let style = SubtitleStyle::default();

        let (from, peak, ms) = scale_in_params(&style);
        assert_eq!((from, peak, ms), (1.0, 1.0, 0), "默认缩放参数必须是单位值");
        let (px, hz, phase) = shake_params(&style);
        assert_eq!(px, 0.0, "默认不抖");
        // ⚠️ **必须写死字面量 18.0，不能比 `DEFAULT_SHAKE_HZ`。**
        //
        // 写成 `assert_eq!(hz, DEFAULT_SHAKE_HZ)` 是**自指**的：把常量改成 0.0
        // 之后两边仍然相等 ⇒ 判据照样绿。变异台实测抓到过这条
        // （`M6` 那一格：`DEFAULT_SHAKE_HZ 18.0 -> 0.0` 跑出 GREEN）。
        //
        // 判据要钉的是**契约上的那个数**，不是"实现与它自己一致"。
        assert_eq!(hz, 18.0, "默认频率取 18.0（写死字面量，别跟常量比）");
        assert_eq!(phase, 0.0, "默认相位 0");

        // 整条包络走一遍：任何一帧都必须是单位缩放。
        for ms_local in [0, 1, 50, 179, 180, 500, 999] {
            let (_, _, s) = text_envelope_scaled(
                ms_local, 1000, 0, 0, 0.0, 0.0, from, peak, ms,
            );
            assert!(
                (s - 1.0).abs() < 1e-6,
                "默认样式在 {ms_local}ms 处必须是 1.0，实际 {s}"
            );
        }
        // 抖动恒 0。
        for i in 0..100 {
            let t = i as f32 * 0.03;
            assert_eq!(
                text_shake_px(t, px, hz, phase),
                (0.0, 0.0),
                "默认样式必须恒不抖（t={t}）"
            );
        }
    }

    /// **`text_envelope`（老签名）与 `text_envelope_scaled(…, 1.0, 1.0, 0)` 等价。**
    ///
    /// 这条钉的是"弹幕那条路没有因为这次改动而变"：它走的是老壳子，
    /// 而老壳子内部委托给新函数 —— 两者前两个返回值必须**逐值相同**。
    #[test]
    fn 老包络与新包络的前两项逐值相同() {
        for local in [0u64, 1, 30, 179, 180, 400, 999, 1000, 1500] {
            for (fi, fo, ri, ro) in [
                (0u64, 0u64, 0.0_f32, 0.0_f32),
                (180, 0, 20.0, 0.0),
                (0, 200, 0.0, 8.0),
                (180, 200, 20.0, 8.0),
                (100, 100, 5.0, 3.0),
            ] {
                let old = crate::layer::text_envelope(local, 1000, fi, fo, ri, ro);
                let new = text_envelope_scaled(local, 1000, fi, fo, ri, ro, 1.0, 1.0, 0);
                assert_eq!(
                    (old.0, old.1),
                    (new.0, new.1),
                    "local={local} fade=({fi},{fo}) rise=({ri},{ro}): 老壳子必须与新函数前两项一致"
                );
            }
        }
    }

    /// `span_ms == 0` ⇒ `(1.0, 0.0, 1.0)`（与老包络同款）。
    #[test]
    fn 零时长给单位值() {
        let (o, dy, s) = text_envelope_scaled(0, 0, 180, 200, 20.0, 8.0, 0.7, 1.15, 180);
        assert_eq!((o, dy, s), (1.0, 0.0, 1.0), "没有时长就没有包络");
    }
}
