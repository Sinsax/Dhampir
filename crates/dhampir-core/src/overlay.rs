//! 文字覆盖层：某一帧上要画的**字幕**结构。
//!
//! # 为什么它是独立的一份结果，而不是塞进 Composite
//!
//! 图层清单说的是「哪几张纹理、用哪个变换叠上去」，而文字**没有纹理** ——
//! 它要先由宿主栅格化成位图。把两者塞进一个结构里会让「谁负责栅格化」变得含糊，
//! 而含糊的代价是两端各自决定，结构与像素一起分叉。
//!
//! 所以评估层出两份结果：图层清单（Composite）与文字覆盖层（这里）。
//! 两份都是**纯函数**算出来的；宿主不许自己再算一遍。
//!
//! # 为什么在 core 而不是在宿主
//!
//! 结构必须一致：几行、每行什么、占哪个归一化矩形。这条由 dhampir_timeline::text_layout
//! 算，core 负责「这一帧哪条字幕活着」。宿主只做一件事：把文字**画**进给定的矩形里。
//!
//! # 时间口径
//!
//! SRT/ASS 的时间戳是**绝对时间**（毫秒），与素材帧率无关 ——
//! 所以毫秒换算成帧号用**时间线**的时间基，不用字幕素材自己的时间基。
//! 一条字幕素材登记了什么时间基都不影响它什么时候出现。
//!
//! # 与文档坐标系的关系
//!
//! 归一化矩形依赖文档坐标系：字号是按**高度**的比例算的，而宽度是按**宽度**归一化的，
//! 所以同一段文字在 4:3 与 16:9 里的归一化宽度不同。这与 T1 确立的口径一致 ——
//! 归一化不是「与一切无关」，而是「与渲染目标尺寸无关」。
//!
//! # 弹幕走同一条路
//!
//! 弹幕与字幕在这层**没有分家**：同一张表（素材 id -> 已解析的条），同一个入口
//! （[`evaluate_overlay`]），出同一个结构。理由是两者对下游是同一种东西 ——
//! 「要画的一行字 + 它占的归一化矩形」，宿主那一侧的画字路径一个字都不用改。
//!
//! 差异只在两处，都放在 [`TextOverlay`] 的 `danmaku` 那半边：
//!
//! * 弹幕的矩形**随帧变化**（从右滚到左），由 `dhampir_timeline::danmaku::rect_at` 现算；
//! * 弹幕的泳道与在屏区间是**结构**，由 `dhampir_timeline::danmaku::layout` 分配 ——
//!   两端要报出同一张表，所以分配只能有一份实现。
//!
//! **颜色与描边两者共用**：`DanmakuSpec` 没有颜色字段（要分开就得动契约）。
//! 只有弹幕的工程看不出这件事（默认白字带描边）。

use dhampir_timeline::danmaku::{layout as layout_danmaku, rect_at};
use dhampir_timeline::layer::TimelineV2;
use dhampir_timeline::schema::{Frame, TimebaseDto, TrackKind};
use dhampir_timeline::subtitle::{Cue, frame_at_ms};
use dhampir_timeline::text_layout::{NormalizedRect, layout};
use std::collections::BTreeMap;

/// 一行要画的文字：内容 + 它占的**行盒**（归一化，相对文档坐标系）+ 这一帧的淡入淡出。
#[derive(Debug, Clone, PartialEq)]
pub struct TextItem {
    pub text: String,
    pub rect: NormalizedRect,
    /// 这一帧的不透明度（淡入淡出算出来的）。**由契约层的 [`text_envelope`] 给**，
    /// 宿主只照用 —— 见那个函数的说明。
    pub opacity: f32,
    /// 这一帧的纵向偏移（**文档像素**，正为向下）。同样是契约层算好的。
    pub dy_px: f32,
    /// **这一条的颜色**（已解析）。
    ///
    /// 与"轨道样式里的颜色"的关系：轨道给的是**默认**，一条 cue 自己带的
    /// （ASS 的 `\c&HBBGGRR&`）覆盖它。**在求值层就解析掉**，
    /// 于是宿主只看到"这一条是什么颜色"，不用自己判断该用哪一个 ——
    /// 两端各判一次就会在"有的条有颜色、有的没有"的工程上分叉。
    pub color: [u8; 4],
    /// **字号**（相对序列高的比例）。**布局算出来的事实**，跟着条目走 ——
    /// 宿主不该从行盒反推（行高可配之后那个反推是错的，见
    /// `dhampir_timeline::text_layout::TextLine::font_ratio`）。
    pub font_ratio: f32,
    /// **这一条被整体缩了多少**（1.0 = 没缩）。
    ///
    /// 参照的 `swEff = sw * wrapped.scale` —— **描边要跟着缩**，
    /// 而描边是宿主按 `stroke_px` 画的。与 `font_ratio` 同一条理由：
    /// 布局算出来的事实，跟着条目走。
    pub scale: f32,
}

/// 一条要画的弹幕：内容 + **这一帧**的矩形 + 泳道与在屏区间 + 这一帧的淡入淡出。
///
/// 矩形与另外几个字段放一起，是为了让「两端给出同一张表」这件事**能逐字段对账**：
/// 只比矩形的话，泳道被分配错了（两条换了位置）在单帧里可能看不出来。
#[derive(Debug, Clone, PartialEq)]
pub struct DanmakuTextItem {
    pub text: String,
    /// 这一帧的归一化矩形（滚动位置是帧的函数）。
    pub rect: NormalizedRect,
    pub lane: u32,
    pub enter: Frame,
    pub exit: Frame,
    /// 这一帧的不透明度（含 `DanmakuSpec.opacity` 这个基础值）。
    pub opacity: f32,
    /// 这一帧的纵向偏移（文档像素）。
    pub dy_px: f32,
    /// **这一条的颜色**（已解析）：cue 自带（ASS 的 `\c`）覆盖轨道默认。
    pub color: [u8; 4],
    /// **字号**（相对序列高的比例）。同 [`TextItem::font_ratio`]。
    pub font_ratio: f32,
}

/// 一类文字的画法（颜色 + 描边）。
///
/// # 为什么字幕与弹幕**各一套**
///
/// 这个结构以前是没有的：`TextOverlay` 上只有一份 `color` + `outline` 给两者共用，
/// 模块文档还写明了"要分开就得动契约"。而 V-Trim 的字幕是暖色 `#dcbda0`、
/// 弹幕是白色 `#ffffff` —— 共用一份时**必然有一个错**，
/// 而"两边的字都能看见"这件事让人以为没问题。
#[derive(Debug, Clone, PartialEq)]
pub struct TextStyle {
    pub color: [u8; 4],
    /// **字号**（相对序列高的比例）。布局**本来就知道**它，带着走，
    /// 宿主就不必再从行盒反推（行高可配之后那个反推是错的）。
    pub font_ratio: f32,
    pub outline: bool,
    /// 描边宽度（**文档像素**；由 `stroke_ratio * 目标高` 换算而来，
    /// 换算在 `evaluate_overlay` 里做，因为只有它知道目标尺寸）。
    ///
    /// **口径是"外侧宽度"**（见 `SubtitleStyle::stroke_ratio` 的说明）：
    /// CSS 的居中描边是一半，ffmpeg 的 `borderw` 是全部 —— 契约挑了后者。
    pub stroke_px: f32,
    pub stroke_color: [u8; 4],
    /// 字体族名。宿主**从你给的字体目录里按这个名字找**，找不到要**报出来**
    /// （不是悄悄换一个字体画）。
    pub family: Option<String>,
    /// 字重（CSS 刻度）。宿主据此在字体目录里挑一个粗体文件。
    pub weight: u32,
}

impl Default for TextStyle {
    fn default() -> Self {
        Self {
            color: [255, 255, 255, 255],
            font_ratio: 0.0,
            outline: true,
            stroke_px: 0.0,
            stroke_color: [0, 0, 0, 255],
            family: None,
            weight: 400,
        }
    }
}

/// 某一帧的文字覆盖层。
#[derive(Debug, Clone, PartialEq)]
pub struct TextOverlay {
    /// 按轨道顺序（先画的在前），轨内按行。
    pub items: Vec<TextItem>,
    /// 字幕之外还有弹幕：同一帧里活着的弹幕条，按轨道顺序、轨内按分配顺序。
    ///
    /// 与 `items` 分开而不是混在一起：两者的**落点规则不同**（字幕居中于整条目标宽，
    /// 弹幕按自己的宽度左对齐），宿主据此选不同的画法。混在一起就得在每个元素上带一个
    /// 种类标签，而那与"这里是纯结构"的定位冲突。
    pub danmaku: Vec<DanmakuTextItem>,
    /// **字幕**的画法。与弹幕的那一套分开（见 `TextStyle` 的说明）。
    pub subtitle_style: TextStyle,
    /// **弹幕**的画法。
    pub danmaku_style: TextStyle,
    /// 因为超过 max_lines 被丢弃的**行数**（所有字幕轨加起来）。
    ///
    /// 与 text_layout 的口径一致：丢弃必须计数，否则「字幕只显示了一半」
    /// 看起来和「字幕就是这样」一样。
    pub dropped_lines: usize,
    /// 因为泳道排不下被丢弃的**弹幕条数**（所有弹幕轨加起来）。
    ///
    /// 与 `dropped_lines` 同款：少的几条看起来和「素材里就那几条」一样。
    pub dropped_danmaku: usize,
}

impl TextOverlay {
    pub fn is_empty(&self) -> bool {
        self.items.is_empty() && self.danmaku.is_empty()
    }
}

/// 字幕素材 id -> 已解析的字幕条。
///
/// **由宿主提供**：core 是纯函数，不做 I/O，读文件与解析都由宿主做完再交进来。
/// 这与 AssetTimebases 是同一个缝。
pub type SubtitleTable = BTreeMap<String, Vec<Cue>>;

/// 算某一帧的文字覆盖层。没有字幕/弹幕轨、或者这一帧没有活着的字时返回 None。
///
/// 返回 None 而不是空结构，是为了让调用方**不必区分**「没有字」与
/// 「有字但这一帧是空的」—— 两种情况的处理完全一样：什么都不画。
///
/// 判据是「**字幕与弹幕两半都空**才 None」：只有弹幕的一帧照样要画，
/// 只有字幕的一帧也照样要画。没有任何弹幕轨的工程走的是与 T2 完全相同的分支
/// （弹幕那半边恒空），所以无弹幕工程的输出逐字节不变。
pub fn evaluate_overlay(
    timeline: &TimelineV2,
    frame: Frame,
    sequence: (u32, u32),
    subtitles: Option<&SubtitleTable>,
) -> Option<TextOverlay> {
    let table = subtitles?;
    if table.is_empty() {
        return None;
    }

    let timebase: TimebaseDto = timeline.timebase;
    let mut items: Vec<TextItem> = Vec::new();
    let mut danmaku: Vec<DanmakuTextItem> = Vec::new();
    let mut dropped_lines = 0_usize;
    let mut dropped_danmaku = 0_usize;
    // 两类文字**各一套**画法（见 `TextStyle` 的说明：以前共用一份，必错一个）。
    let mut subtitle_style = TextStyle::default();
    let mut danmaku_style = TextStyle::default();
    let mut styled = false;
    let mut danmaku_styled = false;
    // 描边宽度是"目标高 * 比例" —— 换算需要目标尺寸，所以在这里做一次。
    let target_h = sequence.1 as f32;

    for track in &timeline.tracks {
        if track.kind != TrackKind::Subtitle {
            continue;
        }
        // 轨道级的样式：一条字幕轨 = 一个字幕素材 + 一套样式。
        // `SubtitleStyle` 现在带 `Option<String>`（`font_family`），所以**不是 Copy**
        // —— 这里借它，不搬它。
        let Some(style) = track.subtitle.as_ref() else {
            continue;
        };
        // 活着的元素至多一个（轨内不许重叠，由校验保证）。
        let Some(element) = track.layers.iter().find(|layer| layer.covers(frame)) else {
            continue;
        };
        if !element.enabled {
            continue;
        }
        let Some(source) = element.source.as_ref() else {
            continue;
        };
        let Some(cues) = table.get(&source.asset_id) else {
            continue;
        };

        for cue in cues {
            if !cue_visible_at(cue, frame, &timebase) {
                continue;
            }
            // 这一帧在 cue 内的位置（毫秒）与 cue 总长（毫秒）。
            let (Some(local_ms), Some(span_ms)) = (
                ms_at_frame_milli(frame, &timebase).map(|now| now.saturating_sub(cue.start_ms)),
                cue.end_ms.checked_sub(cue.start_ms),
            ) else {
                continue;
            };
            // **淡入淡出由契约层算**（`text_envelope`），宿主只照用。
            let (opacity, dy_px) = dhampir_timeline::layer::text_envelope(
                local_ms,
                span_ms,
                style.fade_in_ms,
                style.fade_out_ms,
                style.rise_in_px,
                style.rise_out_px,
            );
            let laid: dhampir_timeline::text_layout::TextLayout =
                layout(&cue.text, &style, sequence);
            dropped_lines += laid.dropped_lines;
            // **这一条的颜色**：cue 自己带的覆盖轨道默认。
            let item_color = cue.style.color.unwrap_or(style.color);
            for line in laid.lines {
                items.push(TextItem {
                    text: line.text,
                    rect: line.rect,
                    opacity,
                    dy_px,
                    color: item_color,
                    font_ratio: line.font_ratio,
                    scale: line.scale,
                });
            }
            subtitle_style = TextStyle {
                color: style.color,
                font_ratio: style.font_ratio,
                outline: style.outline,
                stroke_px: style.stroke_ratio * target_h,
                stroke_color: style.stroke_color,
                family: style.font_family.clone(),
                weight: style.font_weight,
            };
            styled = true;
        }
    }

    for track in &timeline.tracks {
        if track.kind != TrackKind::Danmaku {
            continue;
        }
        // 泳道参数在轨道级（一条弹幕轨 = 一份素材 + 一套泳道参数）。
        let Some(spec) = track.danmaku.as_ref() else {
            continue;
        };
        // 与字幕同一条：轨内活着的元素至多一个，且要 enabled。
        let Some(element) = track.layers.iter().find(|layer| layer.covers(frame)) else {
            continue;
        };
        if !element.enabled {
            continue;
        }
        let Some(source) = element.source.as_ref() else {
            continue;
        };
        let Some(cues) = table.get(&source.asset_id) else {
            continue;
        };
        // 泳道分配**整条素材算一次**（与帧无关），再挑这一帧活着的那些。
        // 每帧重算是纯函数的天性；素材几百条时这是 O(条数 × 泳道数)，写进边界。
        let laid: dhampir_timeline::danmaku::DanmakuLayout = layout_danmaku(cues, spec, &timebase);
        dropped_danmaku += laid.dropped;
        for item in laid.items {
            if frame < item.enter || frame > item.exit {
                continue;
            }
            let Some(rect) = rect_at(&item, frame, spec, sequence) else {
                continue;
            };
            // 在屏区间是**闭**的，所以总长要多算一帧的时长。
            let span_ms = ms_between_frames(item.enter, item.exit.saturating_add(1), &timebase);
            let local_ms = ms_between_frames(item.enter, frame, &timebase);
            let (fade, dy_px) = dhampir_timeline::layer::text_envelope(
                local_ms,
                span_ms,
                spec.fade_in_ms,
                spec.fade_out_ms,
                0.0,
                0.0,
            );
            // **基础不透明度乘在淡入淡出之上** —— V-Trim 的弹幕是 `0.9 * fade`。
            danmaku.push(DanmakuTextItem {
                text: item.text,
                rect,
                lane: item.lane,
                enter: item.enter,
                exit: item.exit,
                opacity: spec.opacity.clamp(0.0, 1.0) * fade,
                dy_px,
                // **这条自带的颜色**覆盖轨道默认。
                color: item.color.unwrap_or(spec.color),
                font_ratio: spec.font_ratio,
            });
        }
        danmaku_style = TextStyle {
            color: spec.color,
            font_ratio: spec.font_ratio,
            outline: spec.outline,
            stroke_px: spec.stroke_ratio * target_h,
            stroke_color: spec.stroke_color,
            family: spec.font_family.clone(),
            weight: spec.font_weight,
        };
        danmaku_styled = true;
    }

    // 两半都空才是 None —— 只有弹幕的一帧不能被当成"没有字"。
    let has_subtitle = styled && !items.is_empty();
    let has_danmaku = danmaku_styled && !danmaku.is_empty();
    if !has_subtitle && !has_danmaku {
        return None;
    }
    Some(TextOverlay {
        items,
        danmaku,
        subtitle_style,
        danmaku_style,
        dropped_lines,
        dropped_danmaku,
    })
}

/// 两个帧号之间相差多少毫秒（`to - from`，负数取 0）。
///
/// **用同一份时间基换算**（`ms_at_frame`），不在这里重写一遍除法 ——
/// 重写一遍就是给"两处算法慢慢分叉"另一个机会。
fn ms_between_frames(from: Frame, to: Frame, timebase: &TimebaseDto) -> u64 {
    if to <= from {
        return 0;
    }
    // 单帧毫秒 = den/num * 1000；乘以帧数，全程整数化以免累积浮点误差。
    let num = timebase.num.max(1) as u128;
    let den = timebase.den as u128;
    let frames = (to - from) as u128;
    let ms = frames.saturating_mul(den).saturating_mul(1000) / num;
    ms.min(u64::MAX as u128) as u64
}

/// 这一帧从时间线起点算起过了多少毫秒。
fn ms_at_frame_milli(frame: Frame, timebase: &TimebaseDto) -> Option<u64> {
    if frame < 0 {
        return Some(0);
    }
    Some(ms_between_frames(0, frame, timebase))
}

/// 这条字幕在这一帧显示吗。
///
/// 区间取**闭开** [start_frame, end_frame)：SRT 的结束时间是「什么时候消失」。
/// 短于一帧的字幕（end <= start）仍然在 start 那一帧显示 —— 否则一条都不显示，
/// 而「一条都不显示」看起来和「这条字幕不存在」一样，属于最难查的那类。
///
/// 与 examples/layout_subtitles.rs 里那段判定是**同一套规则**（那边是参照输出）：
/// 两处只要有一处改了规则，参照就不参照了。
pub fn cue_visible_at(cue: &Cue, frame: Frame, timebase: &TimebaseDto) -> bool {
    let Some(start) = frame_at_ms(cue.start_ms, timebase) else {
        return false;
    };
    let Some(end) = frame_at_ms(cue.end_ms, timebase) else {
        return false;
    };
    if end > start {
        frame >= start && frame < end
    } else {
        frame == start
    }
}

/// 这条字幕条活着的**帧区间**（闭区间）。
///
/// 与 [`cue_visible_at`] 是同一套规则（区间闭开、短于一帧仍算一帧）——
/// 只是一个按帧问、一个按区间答。**两边各写一份是会漂的**，
/// 所以有一条测试逐帧比对两者（`帧区间与逐帧判定一致`）。
pub fn cue_frames(cue: &Cue, timebase: &TimebaseDto) -> Option<(Frame, Frame)> {
    let start = frame_at_ms(cue.start_ms, timebase)?;
    let end = frame_at_ms(cue.end_ms, timebase)?;
    Some(if end > start { (start, end - 1) } else { (start, start) })
}

/// 一条要写进**侧挂字幕文件**的字幕条：文本 + 它在这段出片区间里出现的帧区间。
///
/// 帧号是**绝对**的（时间线坐标系，不是产物坐标系）：重定基是调用方的事，
/// 因为「产物的第 0 帧是时间线的哪一帧」只有调用方知道。
#[derive(Debug, Clone, PartialEq)]
pub struct OverlaySpan {
    /// 字幕素材 id。**一份侧挂文件只装得下一份素材** —— 哪条来自哪份素材要跟着条目走，
    /// 否则调用方无从发现「这次出片用到了两份」。
    pub asset_id: String,
    pub text: String,
    /// 第一次出现的帧（已经裁到出片区间里）。
    pub first: Frame,
    /// 最后一次出现的帧（含）。
    pub last: Frame,
}

/// 出片区间 `[from, to]` 里，画面上出现过哪些字幕条、各占哪几帧。
///
/// # 与 [`evaluate_overlay`] 的关系
///
/// 同一件事的两种问法：那边问「这一帧画什么」，这边问「这段区间里哪些条目出现过」。
/// 侧挂导出要的是后者 —— 逐帧问也能拼出来，但拼的人自己就得再实现一遍合并规则，
/// 而「哪两帧属于同一条」正是会漂的那部分。
///
/// 两边的规则**必须一致**，所以这里的可见性判定走同一套 [`cue_visible_at`] 的口径
/// （经由 [`cue_frames`]）、元素的覆盖走同一个 `Layer::covers`、
/// 「没样式的字幕轨不参与」也是同一条。测试里逐帧比对两者的结论。
///
/// # 已知边界（不假装）
///
/// * **轨内元素重叠**时这里会把两个元素的条目都列出来，而 `evaluate_overlay`
///   只画第一个覆盖该帧的元素 —— 重叠是校验禁止的（`layer_overlap`），
///   走到这里说明工程已经在校验那关被拦下了。
/// * 不看轨道/元素是不是**被静音**（`enabled` 在元素上，见上：它是参与的）。
/// * 不看 `max_lines`：被丢掉的行在侧挂文件里**仍然在** —— 侧挂文件是「这份素材在这段
///   区间里说了什么」，不是「这一帧的像素里有几个字」。
pub fn overlay_spans(
    timeline: &TimelineV2,
    from: Frame,
    to: Frame,
    subtitles: &SubtitleTable,
) -> Vec<OverlaySpan> {
    let timebase = timeline.timebase;
    let mut spans: Vec<OverlaySpan> = Vec::new();

    for track in &timeline.tracks {
        if track.kind != TrackKind::Subtitle {
            continue;
        }
        if track.subtitle.is_none() {
            // 与 evaluate_overlay 同一条：没有样式的字幕轨不参与（位置与字号都无从谈起）。
            continue;
        }
        for element in &track.layers {
            if !element.enabled {
                continue;
            }
            let Some(source) = element.source.as_ref() else {
                continue;
            };
            let Some(cues) = subtitles.get(&source.asset_id) else {
                continue;
            };
            // 元素自己的覆盖区间（左闭右开）先裁到出片区间里 —— 元素不覆盖的帧，
            // 那条字幕没被画过，就不该出现在侧挂文件里。
            let low = element.start.max(from);
            let high = element.end.saturating_sub(1).min(to);
            if high < low {
                continue;
            }
            for cue in cues {
                let Some((first, last)) = cue_frames(cue, &timebase) else {
                    continue;
                };
                let first = first.max(low);
                let last = last.min(high);
                if last < first {
                    continue;
                }
                spans.push(OverlaySpan {
                    asset_id: source.asset_id.clone(),
                    text: cue.text.clone(),
                    first,
                    last,
                });
            }
        }
    }
    spans
}

#[cfg(test)]
mod tests {
    use super::*;

    const SEQUENCE: (u32, u32) = (640, 360);

    fn timeline(json: &str) -> TimelineV2 {
        serde_json::from_str(json).expect("测试用的时间线 JSON 必须能反序列化")
    }

    /// 一条字幕轨 + 一个覆盖 [0, 120) 的字幕元素。时间基 30/1。
    fn subtitle_timeline() -> TimelineV2 {
        timeline(
            r#"{
                "schema": 3,
                "timebase": { "num": 30, "den": 1 },
                "tracks": [{
                    "id": "sub",
                    "kind": "subtitle",
                    "layers": [{
                        "id": "cue",
                        "start": 0,
                        "end": 120,
                        "source": { "asset_id": "sub.srt", "source_in": 0 }
                    }],
                    "subtitle": {
                        "font_ratio": 0.055,
                        "bottom_margin": 0.06,
                        "max_lines": 2,
                        "color": [255, 240, 200, 255],
                        "outline": false
                    }
                }]
            }"#,
        )
    }

    fn cues() -> Vec<Cue> {
        vec![
            Cue { start_ms: 0, end_ms: 2000, text: "第一行中文".to_string(), style: Default::default() },
            Cue { start_ms: 2000, end_ms: 4000, text: "第二句".to_string(), style: Default::default() },
        ]
    }

    fn table() -> SubtitleTable {
        let mut table = SubtitleTable::new();
        table.insert("sub.srt".to_string(), cues());
        table
    }

    /// **字幕轨 + 弹幕轨同时存在**，且两者的颜色刻意不同。
    ///
    /// 这是拆开 `TextOverlay.color` 的那条用例的前提：共用一个字段时，
    /// 这两个颜色**不可能同时成立**。
    fn both_timeline() -> TimelineV2 {
        timeline(
            r#"{
                "schema": 3,
                "timebase": { "num": 30, "den": 1 },
                "tracks": [
                    {
                        "id": "sub",
                        "kind": "subtitle",
                        "layers": [{
                            "id": "cue",
                            "start": 0,
                            "end": 120,
                            "source": { "asset_id": "sub.srt", "source_in": 0 }
                        }],
                        "subtitle": {
                            "font_ratio": 0.055,
                            "bottom_margin": 0.06,
                            "max_lines": 2,
                            "color": [255, 240, 200, 255],
                            "outline": false
                        }
                    },
                    {
                        "id": "dm",
                        "kind": "danmaku",
                        "layers": [{
                            "id": "shots",
                            "start": 0,
                            "end": 120,
                            "source": { "asset_id": "dm.ass", "source_in": 0 }
                        }],
                        "danmaku": {
                            "asset_id": "dm.ass",
                            "lanes": 4,
                            "duration_ms": 2000,
                            "font_ratio": 0.04,
                            "color": [0, 255, 0, 255]
                        }
                    }
                ]
            }"#,
        )
    }

    fn both_table() -> SubtitleTable {
        let mut table = SubtitleTable::new();
        table.insert("sub.srt".to_string(), cues());
        // 弹幕复用同一批 cue：这条用例只看颜色，不看文本内容。
        table.insert("dm.ass".to_string(), cues());
        table
    }

    #[test]
    fn 没有字幕表时没有覆盖层() {
        assert!(evaluate_overlay(&subtitle_timeline(), 15, SEQUENCE, None).is_none());
    }

    #[test]
    fn 空表时没有覆盖层() {
        let empty = SubtitleTable::new();
        assert!(evaluate_overlay(&subtitle_timeline(), 15, SEQUENCE, Some(&empty)).is_none());
    }

    #[test]
    fn 活着的字幕给出覆盖层() {
        // 第 15 帧 = 500ms，落在第一条字幕 [0, 2000) 里。
        let overlay = evaluate_overlay(&subtitle_timeline(), 15, SEQUENCE, Some(&table()))
            .expect("这一帧应当有字幕");
        assert_eq!(overlay.items.len(), 1);
        assert_eq!(overlay.items[0].text, "第一行中文");
    }

    /// **结构必须与参照输出逐字段一致。**
    ///
    /// 参照是 dhampir_timeline::text_layout 本身（examples/layout_subtitles.rs 打的也是它）。
    /// 这条测试的意义是：core 只是「挑出这一帧的字幕」，**不许**对结构做任何再加工。
    #[test]
    fn 结构与共享布局逐字段一致() {
        let overlay = evaluate_overlay(&subtitle_timeline(), 15, SEQUENCE, Some(&table())).unwrap();
        let style = dhampir_timeline::layer::SubtitleStyle {
            color: [255, 240, 200, 255],
            outline: false,
            ..Default::default()
        };
        let reference = layout("第一行中文", &style, SEQUENCE);
        assert_eq!(overlay.items.len(), reference.lines.len());
        for (item, line) in overlay.items.iter().zip(reference.lines.iter()) {
            assert_eq!(item.text, line.text);
            assert_eq!(item.rect, line.rect, "矩形必须逐字段相同");
        }
    }

    #[test]
    fn 颜色与描边来自轨道样式() {
        let overlay = evaluate_overlay(&subtitle_timeline(), 15, SEQUENCE, Some(&table())).unwrap();
        assert_eq!(overlay.subtitle_style.color, [255, 240, 200, 255]);
        assert!(!overlay.subtitle_style.outline, "轨道样式写了不描边");
    }

    #[test]
    fn 没写描边比例的工程描边宽度与升级前一致() {
        // **这条钉的是"既有工程逐字节不变"。**
        //
        // `subtitle_timeline()` 没写 `stroke_ratio`，于是它取默认值 0；
        // 而 `stroke_px` 算出来必须是 **0**（= "别用契约的值，让栅格器按字号推"）。
        //
        // 我第一版把默认值写成 `12/1080`（V-Trim 的实际值），于是这里会算出
        // `12/1080 * 360 = 4`，而老行为是 `border_px(20) = 1` —— 描边粗了 4 倍，
        // 既有工程的产物全变。默认值是**契约的一部分**，不是风格偏好。
        let overlay = evaluate_overlay(&subtitle_timeline(), 15, SEQUENCE, Some(&table())).unwrap();
        assert_eq!(overlay.subtitle_style.stroke_px, 0.0, "默认必须是 0（走字号推的老路径）");

        // 显式写一个比例时才算，且按**目标高**换算。
        let mut styled = subtitle_timeline();
        if let Some(style) = styled.tracks[0].subtitle.as_mut() {
            style.stroke_ratio = 12.0 / 1080.0;
        }
        let overlay = evaluate_overlay(&styled, 15, SEQUENCE, Some(&table())).unwrap();
        let expected = 12.0 / 1080.0 * SEQUENCE.1 as f32;
        assert!(
            (overlay.subtitle_style.stroke_px - expected).abs() < 1e-4,
            "写了的比例要按目标高换算：期望 {expected}，实得 {}",
            overlay.subtitle_style.stroke_px
        );
    }

    #[test]
    fn 字幕与弹幕各有一套颜色() {
        // **这是拆开 `color` 的理由**：V-Trim 的字幕是暖色、弹幕是白色，
        // 共用一份时必然有一个错 —— 而"两边的字都看得见"让人以为没问题。
        let overlay = evaluate_overlay(&both_timeline(), 15, SEQUENCE, Some(&both_table())).unwrap();
        assert_eq!(overlay.subtitle_style.color, [255, 240, 200, 255], "字幕用字幕的颜色");
        assert_eq!(overlay.danmaku_style.color, [0, 255, 0, 255], "弹幕用弹幕的颜色");
        assert_ne!(
            overlay.subtitle_style.color, overlay.danmaku_style.color,
            "两者必须能不同 —— 这正是共用一个字段时做不到的事"
        );
    }

    #[test]
    fn 这一帧没有字幕时没有覆盖层() {
        // 第 10 帧 = 333ms 有字幕；这里挑一条**不在范围内**的：
        // 2000ms 之后需要 frame >= 60，所以 59 帧仍是第一条；换一条表来验空。
        let mut table = SubtitleTable::new();
        table.insert(
            "sub.srt".to_string(),
            vec![Cue { start_ms: 20_000, end_ms: 22_000, text: "很久以后".to_string(), style: Default::default() }],
        );
        assert!(evaluate_overlay(&subtitle_timeline(), 15, SEQUENCE, Some(&table)).is_none());
    }

    #[test]
    fn 字幕区间是闭开的() {
        let timebase = TimebaseDto { num: 30, den: 1 };
        let cue = Cue { start_ms: 2000, end_ms: 4000, text: "x".to_string(), style: Default::default() };
        // 30fps：2000ms -> 第 60 帧，4000ms -> 第 120 帧。
        assert!(!cue_visible_at(&cue, 59, &timebase), "还没到");
        assert!(cue_visible_at(&cue, 60, &timebase), "起点包含");
        assert!(cue_visible_at(&cue, 119, &timebase), "最后一帧还在");
        assert!(!cue_visible_at(&cue, 120, &timebase), "终点不包含");
    }

    #[test]
    fn 短于一帧的字幕仍然显示一帧() {
        let timebase = TimebaseDto { num: 30, den: 1 };
        // 3ms 的间隔在 30fps 下取整之后是同一帧 —— 不能一条都不显示。
        let cue = Cue { start_ms: 1000, end_ms: 1003, text: "x".to_string(), style: Default::default() };
        assert!(cue_visible_at(&cue, 30, &timebase));
    }

    #[test]
    fn 超出最大行数时丢弃并计数() {
        let mut table = SubtitleTable::new();
        table.insert(
            "sub.srt".to_string(),
            vec![Cue {
                start_ms: 0,
                end_ms: 2000,
                text: "一\n二\n三\n四\n五".to_string(),
                style: Default::default(),
            }],
        );
        let overlay = evaluate_overlay(&subtitle_timeline(), 15, SEQUENCE, Some(&table)).unwrap();
        assert_eq!(overlay.items.len(), 2, "max_lines 是 2");
        assert_eq!(overlay.dropped_lines, 3, "丢了多少行必须如实计数");
    }

    #[test]
    fn 视频轨上的同名字幕素材不参与() {
        // 把轨的 kind 换成 video：同一个表、同一帧，不该给出覆盖层。
        let video = timeline(
            r#"{
                "schema": 3,
                "timebase": { "num": 30, "den": 1 },
                "tracks": [{
                    "id": "v",
                    "kind": "video",
                    "layers": [{
                        "id": "c",
                        "start": 0,
                        "end": 120,
                        "source": { "asset_id": "sub.srt", "source_in": 0 }
                    }]
                }]
            }"#,
        );
        assert!(evaluate_overlay(&video, 15, SEQUENCE, Some(&table())).is_none());
    }

    #[test]
    fn 没有样式的字幕轨不参与() {
        // 轨上没写 subtitle 样式：位置与字号都无从谈起，所以不画，而不是猜一套默认值。
        let no_style = timeline(
            r#"{
                "schema": 3,
                "timebase": { "num": 30, "den": 1 },
                "tracks": [{
                    "id": "sub",
                    "kind": "subtitle",
                    "layers": [{
                        "id": "cue",
                        "start": 0,
                        "end": 120,
                        "source": { "asset_id": "sub.srt", "source_in": 0 }
                    }]
                }]
            }"#,
        );
        assert!(evaluate_overlay(&no_style, 15, SEQUENCE, Some(&table())).is_none());
    }

    #[test]
    fn 同样输入给同样输出() {
        let a = evaluate_overlay(&subtitle_timeline(), 15, SEQUENCE, Some(&table()));
        let b = evaluate_overlay(&subtitle_timeline(), 15, SEQUENCE, Some(&table()));
        assert_eq!(a, b, "评估必须确定性，否则两端结构谈不上一致");
    }

    #[test]
    fn 不同宽高比下归一化宽度不同() {
        // 字号按高度算、宽度按宽度归一化 —— 所以 4:3 里同一段字占比更宽。
        let wide = evaluate_overlay(&subtitle_timeline(), 15, (640, 360), Some(&table())).unwrap();
        let tall = evaluate_overlay(&subtitle_timeline(), 15, (480, 360), Some(&table())).unwrap();
        assert!(
            tall.items[0].rect.width > wide.items[0].rect.width,
            "4:3 里同一行的归一化宽度应当更大：{:?} vs {:?}",
            tall.items[0].rect.width,
            wide.items[0].rect.width
        );
    }

    // ---- 侧挂导出（T2.7）：两种问法必须说的是同一件事 ----

    fn cue(start_ms: u64, end_ms: u64, text: &str) -> Cue {
        Cue { start_ms, end_ms, text: text.to_string(), style: Default::default() }
    }

    /// 一个带样式的字幕轨（两个紧邻、不重叠的元素）、一条**没样式**的轨、
    /// 以及一个**关掉**的元素。后两者是反向用例：它们都不该出现在侧挂导出里。
    fn spans_timeline() -> TimelineV2 {
        timeline(
            r#"{
                "schema": 3,
                "timebase": { "num": 30, "den": 1 },
                "tracks": [
                    { "id": "sub", "kind": "subtitle",
                      "layers": [
                        { "id": "e1", "start": 0, "end": 120, "source": { "asset_id": "sub.srt", "source_in": 0 } },
                        { "id": "e2", "start": 120, "end": 240, "source": { "asset_id": "sub.srt", "source_in": 0 } }
                      ],
                      "subtitle": { "font_ratio": 0.055, "bottom_margin": 0.06, "max_lines": 2 } },
                    { "id": "nos", "kind": "subtitle",
                      "layers": [{ "id": "n1", "start": 0, "end": 240, "source": { "asset_id": "other.srt", "source_in": 0 } }] },
                    { "id": "off", "kind": "subtitle",
                      "layers": [{ "id": "o1", "start": 0, "end": 240, "enabled": false,
                                   "source": { "asset_id": "other.srt", "source_in": 0 } }],
                      "subtitle": { "font_ratio": 0.055, "bottom_margin": 0.06, "max_lines": 2 } }
                ]
            }"#,
        )
    }

    /// 四条**单字**字幕（不会换行、不会被 max_lines 丢 —— 于是
    /// `evaluate_overlay` 某一帧的 items 正好是「那一刻活着的条」，可以逐帧对账）
    /// 加上另一份素材的字幕。
    fn spans_table() -> SubtitleTable {
        let mut table = SubtitleTable::new();
        table.insert(
            "sub.srt".to_string(),
            vec![
                cue(0, 2000, "甲"),
                cue(2000, 4000, "乙"),
                cue(4000, 6000, "丙"),
                cue(6000, 8000, "丁"),
            ],
        );
        table.insert("other.srt".to_string(), vec![cue(0, 8000, "别的")]);
        table
    }

    /// **这一条才是把两种问法钉在一起的东西。** 逐帧问 `evaluate_overlay`
    /// （画面里出现的字），与按区间问 `overlay_spans`（侧挂导出要写的字）对账。
    ///
    /// 「没样式的轨不参与」「关掉的元素不参与」都由这条顺手盯住 ——
    /// 那两条规则只要有一边改了口径，对账立刻红。
    #[test]
    fn 侧挂导出的区间与画面里出现的字一致() {
        let timeline = spans_timeline();
        let table = spans_table();
        let (from, to) = (0, 300);
        let spans = overlay_spans(&timeline, from, to, &table);
        assert!(!spans.is_empty(), "这份时间线上有字幕，不该一条都算不出来");

        for frame in from..=to {
            let mut want: Vec<&str> = spans
                .iter()
                .filter(|span| span.first <= frame && frame <= span.last)
                .map(|span| span.text.as_str())
                .collect();
            want.sort_unstable();
            let overlay = evaluate_overlay(&timeline, frame, SEQUENCE, Some(&table));
            let mut got: Vec<&str> = overlay
                .as_ref()
                .map(|overlay| overlay.items.iter().map(|item| item.text.as_str()).collect())
                .unwrap_or_default();
            got.sort_unstable();
            assert_eq!(want, got, "第 {frame} 帧：侧挂导出与画面必须说的是同一件事");
        }
        assert!(
            spans.iter().all(|span| span.asset_id == "sub.srt"),
            "没样式的轨与关掉的元素都不该进来：{spans:?}"
        );
    }

    #[test]
    fn 出片区间外的条不进侧挂导出() {
        let timeline = spans_timeline();
        let table = spans_table();
        // 只出「乙」那一段：侧挂导出里就只该有它。
        let spans = overlay_spans(&timeline, 60, 119, &table);
        assert_eq!(spans.len(), 1, "{spans:?}");
        assert_eq!((spans[0].text.as_str(), spans[0].first, spans[0].last), ("乙", 60, 119));
        // 区间把一条从中间切开时，报的是它**在这段区间里**出现的帧，不是整条的帧。
        let spans = overlay_spans(&timeline, 30, 89, &table);
        assert_eq!(spans.len(), 2, "{spans:?}");
        assert_eq!((spans[0].text.as_str(), spans[0].first, spans[0].last), ("甲", 30, 59));
        assert_eq!((spans[1].text.as_str(), spans[1].first, spans[1].last), ("乙", 60, 89));
    }

    #[test]
    fn 元素没覆盖的帧不算它出现过() {
        let timeline = timeline(
            r#"{
                "schema": 3,
                "timebase": { "num": 30, "den": 1 },
                "tracks": [{
                    "id": "sub", "kind": "subtitle",
                    "layers": [{ "id": "e", "start": 0, "end": 30, "source": { "asset_id": "sub.srt", "source_in": 0 } }],
                    "subtitle": { "font_ratio": 0.055, "bottom_margin": 0.06, "max_lines": 2 }
                }]
            }"#,
        );
        let table = spans_table();
        let spans = overlay_spans(&timeline, 0, 239, &table);
        // 「甲」到第 59 帧才结束，但元素只覆盖 [0,30) —— 第 30 帧起画面上没有它。
        assert_eq!(spans.len(), 1, "{spans:?}");
        assert_eq!((spans[0].text.as_str(), spans[0].first, spans[0].last), ("甲", 0, 29));
        assert!(evaluate_overlay(&timeline, 30, SEQUENCE, Some(&table)).is_none());
    }

    #[test]
    fn 短于一帧的条在侧挂导出里仍占一帧() {
        let mut table = SubtitleTable::new();
        table.insert("sub.srt".to_string(), vec![cue(1000, 1003, "闪一下")]);
        let spans = overlay_spans(&spans_timeline(), 0, 239, &table);
        // 1000..1003ms 在 30fps 下取整是同一帧 —— 不能一条都不出现。
        assert_eq!(spans.len(), 1, "{spans:?}");
        assert_eq!((spans[0].first, spans[0].last), (30, 30));
    }

    #[test]
    fn 空表时侧挂导出也是空的() {
        assert!(overlay_spans(&spans_timeline(), 0, 239, &SubtitleTable::new()).is_empty());
    }

    #[test]
    fn 时间基坏掉时不算出现过() {
        // num 为 0 的工程是坏的（校验会拦），但这里也不能装作算得出来。
        let mut bad = spans_timeline();
        bad.timebase = TimebaseDto { num: 0, den: 1 };
        assert!(overlay_spans(&bad, 0, 239, &spans_table()).is_empty());
    }

    #[test]
    fn 帧区间与逐帧判定一致() {
        // 约 29.97fps：两种问法在非整数帧率下最容易分叉。
        let timebase = TimebaseDto { num: 30000, den: 1001 };
        for (start, end) in [(0_u64, 2000_u64), (1000, 1003), (500, 500), (33, 34)] {
            let item = cue(start, end, "x");
            let (first, last) = cue_frames(&item, &timebase).expect("时间基合法");
            for frame in (first - 5)..=(last + 5) {
                assert_eq!(
                    cue_visible_at(&item, frame, &timebase),
                    frame >= first && frame <= last,
                    "cue {start}..{end} 在第 {frame} 帧：逐帧判定与帧区间必须同答"
                );
            }
        }
    }
}
