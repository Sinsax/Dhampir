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

use dhampir_timeline::layer::{SubtitleStyle, TimelineV2};
use dhampir_timeline::schema::{Frame, TimebaseDto, TrackKind};
use dhampir_timeline::subtitle::{Cue, frame_at_ms};
use dhampir_timeline::text_layout::{NormalizedRect, layout};
use std::collections::BTreeMap;

/// 一行要画的文字：内容 + 它占的**行盒**（归一化，相对文档坐标系）。
#[derive(Debug, Clone, PartialEq)]
pub struct TextItem {
    pub text: String,
    pub rect: NormalizedRect,
}

/// 某一帧的文字覆盖层。
#[derive(Debug, Clone, PartialEq)]
pub struct TextOverlay {
    /// 按轨道顺序（先画的在前），轨内按行。
    pub items: Vec<TextItem>,
    /// 文字颜色，RGBA。来自轨道样式。
    pub color: [u8; 4],
    /// 是否描边。来自轨道样式。
    pub outline: bool,
    /// 因为超过 max_lines 被丢弃的**行数**（所有字幕轨加起来）。
    ///
    /// 与 text_layout 的口径一致：丢弃必须计数，否则「字幕只显示了一半」
    /// 看起来和「字幕就是这样」一样。
    pub dropped_lines: usize,
}

impl TextOverlay {
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

/// 字幕素材 id -> 已解析的字幕条。
///
/// **由宿主提供**：core 是纯函数，不做 I/O，读文件与解析都由宿主做完再交进来。
/// 这与 AssetTimebases 是同一个缝。
pub type SubtitleTable = BTreeMap<String, Vec<Cue>>;

/// 算某一帧的文字覆盖层。没有字幕轨、或者这一帧没有活着的字幕时返回 None。
///
/// 返回 None 而不是空结构，是为了让调用方**不必区分**「没有字幕」与
/// 「有字幕但这一帧是空的」—— 两种情况的处理完全一样：什么都不画。
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
    let mut dropped_lines = 0_usize;
    // 默认值只是为了「一条轨都没命中」时结构上仍是确定的；命中时会被轨道样式覆盖。
    let mut color = [255_u8, 255, 255, 255];
    let mut outline = true;
    let mut styled = false;

    for track in &timeline.tracks {
        if track.kind != TrackKind::Subtitle {
            continue;
        }
        // 轨道级的样式：一条字幕轨 = 一个字幕素材 + 一套样式。
        // 类型写出来是为了让顶上的 import 有实际用处 —— 也顺带说明这不是随便一个结构。
        let style: SubtitleStyle = match track.subtitle {
            Some(style) => style,
            None => continue,
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
            let laid: dhampir_timeline::text_layout::TextLayout =
                layout(&cue.text, &style, sequence);
            dropped_lines += laid.dropped_lines;
            for line in laid.lines {
                items.push(TextItem { text: line.text, rect: line.rect });
            }
            color = style.color;
            outline = style.outline;
            styled = true;
        }
    }

    if !styled || items.is_empty() {
        return None;
    }
    Some(TextOverlay { items, color, outline, dropped_lines })
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
        let style = SubtitleStyle { color: [255, 240, 200, 255], outline: false, ..SubtitleStyle::default() };
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
        assert_eq!(overlay.color, [255, 240, 200, 255]);
        assert!(!overlay.outline, "轨道样式写了不描边");
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
