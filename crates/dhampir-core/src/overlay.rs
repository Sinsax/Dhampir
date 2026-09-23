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
}
