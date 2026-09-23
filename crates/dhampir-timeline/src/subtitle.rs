//! 字幕：SRT 与 ASS 的解析与序列化。**纯文本，零依赖。**
//!
//! # 为什么解析在契约层，而不是各宿主自己写
//!
//! 两端都要把字幕展开成"这一帧该显示哪几项"。如果浏览器解析一遍、后端解析一遍，
//! 两边的"哪一帧显示哪一句"迟早会差一帧 —— 而那种差异只在成片里看得出来，
//! 且看起来像"字幕慢了半拍"这种玄学问题。
//!
//! 所以：**解析只有这一份实现**，写在这里（纯数据层，能脱离 GPU 单测）。
//! 宿主只负责把文件读成字符串喂进来，以及把文字画成像素。
//!
//! # 时间用毫秒，帧号只在最后一步换算
//!
//! SRT 的精度本来就是毫秒，ASS 是厘秒。**先原样存毫秒**，到求值时再按序列时间基
//! 换成整数帧。反过来（解析时就用某个帧率换成帧）会让"换序列帧率"变成一件
//! 必须重新解析字幕的事 —— 而字幕文件本来不该知道序列帧率是多少。
//!
//! # 看不懂的行跳过并**计数**，不让整份文件失败
//!
//! ASS 的方言太多（各家工具生成的标签五花八门）。为一行看不懂的标签把整份字幕
//! 拒掉，代价比收益大得多 —— 但**静默跳过**也不行：那会让人以为字幕就这样。
//! 所以跳过的条数报在 ParseReport::skipped 里。

use crate::schema::{Frame, TimebaseDto};

/// 一条字幕。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cue {
    /// 相对字幕文件开头的起点（毫秒）。
    pub start_ms: u64,
    /// 终点（毫秒，**不含** —— 与帧区间左闭右开一致）。
    pub end_ms: u64,
    /// 文本。内部换行用换行符。
    pub text: String,
    pub style: CueStyle,
}

/// 单条字幕的样式覆盖。**没给就用轨道级样式** —— 这样绝大多数文件不用带样式。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CueStyle {
    pub bold: bool,
    pub italic: bool,
    /// RGBA。None = 用轨道样式里的颜色。
    pub color: Option<[u8; 4]>,
}

/// 解析结果。**跳过的条数是结论的一部分**，不是日志。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParseReport {
    pub cues: Vec<Cue>,
    /// 看不懂而跳过的行/条目数。
    pub skipped: usize,
}

impl ParseReport {
    /// 按起点排序（解析不假设文件里的顺序是对的 —— 手写的字幕经常不是）。
    fn sorted(mut self) -> Self {
        self.cues.sort_by_key(|cue| (cue.start_ms, cue.end_ms));
        self
    }
}

/// 毫秒换成**序列帧号**（向下取整）。
///
/// 纯整数：中间量用 i128。用浮点算这一步，0.1+0.2 那类误差会变成
/// "某些字幕早/晚一帧"，而那种错只在成片里看得出来。
pub fn frame_at_ms(ms: u64, timebase: &TimebaseDto) -> Option<Frame> {
    if timebase.num == 0 || timebase.den == 0 {
        return None;
    }
    let numerator = i128::from(ms) * i128::from(timebase.num);
    let denominator = 1000i128 * i128::from(timebase.den);
    let frame = numerator / denominator;
    Some(frame.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64)
}

/// 去掉 BOM 并把行尾统一。
///
/// 这两件事必须一起做：BOM 会让**第一行**永远匹配不上（看错误代码像是在说
/// "序号行格式不对"，而文件其实完全正常）；行尾不统一会让每一行结尾多一个字符。
fn normalize(text: &str) -> String {
    let without_bom = text.strip_prefix('\u{feff}').unwrap_or(text);
    without_bom.replace("\r\n", "\n").replace('\r', "\n")
}

/// 解析 SRT / WebVTT 风格的时间戳：00:00:01,500 或 00:00:01.500。
pub fn parse_srt_time(text: &str) -> Option<u64> {
    let trimmed = text.trim();
    let (clock, fraction) = trimmed.split_once(',').or_else(|| trimmed.split_once('.'))?;
    let mut parts = clock.split(':');
    let hours: u64 = parts.next()?.trim().parse().ok()?;
    let minutes: u64 = parts.next()?.trim().parse().ok()?;
    let seconds: u64 = parts.next()?.trim().parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    // 毫秒位数不固定（1 到 3 位都见过），按位数补零。
    let fraction = fraction.trim();
    if fraction.is_empty() || fraction.len() > 3 || !fraction.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let mut millis: u64 = fraction.parse().ok()?;
    for _ in fraction.len()..3 {
        millis *= 10;
    }
    Some((hours * 3600 + minutes * 60 + seconds) * 1000 + millis)
}

/// SRT 时间戳的文本形式。
pub fn format_srt_time(ms: u64) -> String {
    let hours = ms / 3_600_000;
    let minutes = (ms / 60_000) % 60;
    let seconds = (ms / 1000) % 60;
    let millis = ms % 1000;
    format!("{hours:02}:{minutes:02}:{seconds:02},{millis:03}")
}

/// ASS 的厘秒时间戳：0:00:01.50
pub fn format_ass_time(ms: u64) -> String {
    let hours = ms / 3_600_000;
    let minutes = (ms / 60_000) % 60;
    let seconds = (ms / 1000) % 60;
    let centis = (ms % 1000) / 10;
    format!("{hours}:{minutes:02}:{seconds:02}.{centis:02}")
}

/// 解析 ASS 的时间戳。
pub fn parse_ass_time(text: &str) -> Option<u64> {
    let trimmed = text.trim();
    let (clock, fraction) = trimmed.split_once('.')?;
    let mut parts = clock.split(':');
    let hours: u64 = parts.next()?.trim().parse().ok()?;
    let minutes: u64 = parts.next()?.trim().parse().ok()?;
    let seconds: u64 = parts.next()?.trim().parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    let fraction = fraction.trim();
    if fraction.is_empty() || fraction.len() > 2 || !fraction.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let mut centis: u64 = fraction.parse().ok()?;
    for _ in fraction.len()..2 {
        centis *= 10;
    }
    Some((hours * 3600 + minutes * 60 + seconds) * 1000 + centis * 10)
}

/// 解析 SRT。
///
/// 块之间用空行分隔。序号行**可选**（有的工具不写）。文本可以有多行。
pub fn parse_srt(text: &str) -> Result<ParseReport, String> {
    let normalized = normalize(text);
    let mut report = ParseReport::default();
    let mut blocks: Vec<Vec<&str>> = Vec::new();
    let mut current: Vec<&str> = Vec::new();
    for line in normalized.split('\n') {
        if line.trim().is_empty() {
            if !current.is_empty() {
                blocks.push(std::mem::take(&mut current));
            }
            continue;
        }
        current.push(line);
    }
    if !current.is_empty() {
        blocks.push(current);
    }

    for block in blocks {
        // 找那一行时间戳。它前面最多允许一行序号，所以不能假定是第一行。
        let Some(time_index) = block.iter().position(|line| line.contains("-->")) else {
            report.skipped += 1;
            continue;
        };
        let (left, right) = match block[time_index].split_once("-->") {
            Some(pair) => pair,
            None => {
                report.skipped += 1;
                continue;
            }
        };
        // 右半边可能带位置参数（WebVTT 风格），只取第一个词。
        let right = right.split_whitespace().next().unwrap_or("");
        let (Some(start_ms), Some(end_ms)) = (parse_srt_time(left), parse_srt_time(right)) else {
            report.skipped += 1;
            continue;
        };
        let body = block[time_index + 1..].join("\n");
        if body.trim().is_empty() {
            report.skipped += 1;
            continue;
        }
        report.cues.push(Cue {
            start_ms,
            end_ms,
            text: body.trim_end().to_string(),
            style: CueStyle::default(),
        });
    }
    Ok(report.sorted())
}

/// 写成 SRT。
pub fn to_srt(cues: &[Cue]) -> String {
    let mut out = String::new();
    for (index, cue) in cues.iter().enumerate() {
        out.push_str(&format!("{}\n", index + 1));
        out.push_str(&format!(
            "{} --> {}\n",
            format_srt_time(cue.start_ms),
            format_srt_time(cue.end_ms)
        ));
        out.push_str(&cue.text);
        out.push_str("\n\n");
    }
    out
}

/// 把 ASS 的文本转成纯文本：去掉覆盖标签（花括号里的东西），换行标记换成换行符。
fn ass_text_to_plain(raw: &str) -> String {
    let mut out = String::new();
    let mut depth = 0usize;
    let mut chars = raw.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '{' => depth += 1,
            '}' => depth = depth.saturating_sub(1),
            _ if depth > 0 => {}
            '\\' => match chars.peek() {
                Some('N') | Some('n') => {
                    chars.next();
                    out.push('\n');
                }
                Some('h') => {
                    // 硬空格
                    chars.next();
                    out.push(' ');
                }
                _ => out.push(ch),
            },
            _ => out.push(ch),
        }
    }
    out
}

/// ASS 文件里的默认样式（写的时候用）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssStyle {
    pub font: String,
    pub font_size: u32,
    /// 底部边距（ASS 的 MarginV，单位是脚本分辨率下的像素）。
    pub margin_v: u32,
}

impl Default for AssStyle {
    fn default() -> Self {
        Self { font: "Microsoft YaHei".to_string(), font_size: 48, margin_v: 36 }
    }
}

/// ASS 的 Events 段默认字段序（几乎所有工具都是这一个）。
const ASS_FIELDS: [&str; 10] = [
    "Layer", "Start", "End", "Style", "Name", "MarginL", "MarginR", "MarginV", "Effect", "Text",
];

/// 解析 ASS：只认 Events 段里的 Dialogue，字段序**按 Format 行来**
/// （有的工具字段序与默认不同，写死会读错列）。
pub fn parse_ass(text: &str) -> Result<ParseReport, String> {
    let normalized = normalize(text);
    let mut report = ParseReport::default();
    let mut in_events = false;
    let mut fields: Vec<String> = ASS_FIELDS.iter().map(|name| name.to_string()).collect();

    for line in normalized.split('\n') {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.starts_with('[') {
            in_events = trimmed.eq_ignore_ascii_case("[Events]");
            continue;
        }
        if !in_events {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("Format:") {
            fields = rest.split(',').map(|part| part.trim().to_string()).collect();
            continue;
        }
        let Some(rest) = trimmed.strip_prefix("Dialogue:") else {
            continue;
        };
        // Text 是**最后一个**字段，它自己可以带逗号，所以只切前 n-1 个。
        let parts: Vec<&str> = rest.splitn(fields.len(), ',').collect();
        if parts.len() != fields.len() {
            report.skipped += 1;
            continue;
        }
        let column = |name: &str| -> Option<&str> {
            fields
                .iter()
                .position(|field| field.eq_ignore_ascii_case(name))
                .and_then(|index| parts.get(index).copied())
        };
        let (Some(start), Some(end)) = (
            column("Start").and_then(parse_ass_time),
            column("End").and_then(parse_ass_time),
        ) else {
            report.skipped += 1;
            continue;
        };
        let body = column("Text").map(ass_text_to_plain).unwrap_or_default();
        if body.trim().is_empty() {
            report.skipped += 1;
            continue;
        }
        report.cues.push(Cue { start_ms: start, end_ms: end, text: body, style: CueStyle::default() });
    }
    Ok(report.sorted())
}

/// 写成 ASS（供旁挂导出）。
pub fn to_ass(cues: &[Cue], style: &AssStyle) -> String {
    let mut out = String::new();
    out.push_str("[Script Info]\n");
    out.push_str("ScriptType: v4.00+\n");
    out.push_str("WrapStyle: 2\n");
    out.push_str("ScaledBorderAndShadow: yes\n\n");
    out.push_str("[V4+ Styles]\n");
    out.push_str(&format!("Format: {}\n", ASS_FIELDS.join(", ")));
    out.push_str(&format!(
        "Style: Default,{}, {}, &H00FFFFFF, &H000000FF, &H00000000, &H80000000, 0, 0, 0, 0, 100, 100, 0, 0, 1, 2, 1, 2, 10, 10, {}, 1\n\n",
        style.font, style.font_size, style.margin_v
    ));
    out.push_str("[Events]\n");
    out.push_str(&format!("Format: {}\n", ASS_FIELDS.join(", ")));
    for cue in cues {
        // 文本里的换行在 ASS 里是反斜杠 + N。
        let body = cue.text.replace('\n', "\\N");
        out.push_str(&format!(
            "Dialogue: 0,{},{},Default,,0,0,0,,{}\n",
            format_ass_time(cue.start_ms),
            format_ass_time(cue.end_ms),
            body
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tb(num: u32, den: u32) -> TimebaseDto {
        TimebaseDto { num, den }
    }

    #[test]
    fn 解析一份正常的_srt() {
        let text = "1\n00:00:01,000 --> 00:00:03,500\n第一句\n\n2\n00:00:04,000 --> 00:00:06,000\n第二句\n";
        let report = parse_srt(text).expect("能解析");
        assert_eq!(report.skipped, 0);
        assert_eq!(report.cues.len(), 2);
        assert_eq!(report.cues[0].start_ms, 1000);
        assert_eq!(report.cues[0].end_ms, 3500);
        assert_eq!(report.cues[0].text, "第一句");
        assert_eq!(report.cues[1].start_ms, 4000);
    }

    #[test]
    fn 容忍_bom_与_crlf() {
        // 这两样都在真实文件里极常见，而症状是"第一行永远匹配不上"。
        let text = "\u{feff}1\r\n00:00:00,500 --> 00:00:01,500\r\n喂\r\n";
        let report = parse_srt(text).expect("能解析");
        assert_eq!(report.cues.len(), 1, "BOM 不该让第一块被跳过");
        assert_eq!(report.cues[0].text, "喂");
        assert_eq!(report.cues[0].start_ms, 500);
    }

    #[test]
    fn 毫秒分隔符是点号也认() {
        let text = "00:00:01.250 --> 00:00:02.000\n嗨\n";
        let report = parse_srt(text).expect("能解析");
        assert_eq!(report.cues[0].start_ms, 1250, "一位小数要当十分之一秒");
    }

    #[test]
    fn 多行文本合起来() {
        let text = "1\n00:00:00,000 --> 00:00:01,000\n上半句\n下半句\n";
        let report = parse_srt(text).expect("能解析");
        assert_eq!(report.cues[0].text, "上半句\n下半句");
    }

    #[test]
    fn 畸形时间戳只跳过那一条并计数() {
        // 三个块：第一块好的，后两块各坏在时间戳上（一个右半边不是时间，一个左边不是）。
        let text = "1\n00:00:01,000 --> 00:00:02,000\n好的\n\n2\n99:99:99,999 --> 坏掉了\n坏的\n\n3\n\tx --> y\n又坏\n";
        let report = parse_srt(text).expect("不该整份失败");
        assert_eq!(report.cues.len(), 1);
        assert_eq!(report.skipped, 2, "跳过的条数是结论，必须报出来");
    }

    #[test]
    fn 空文件给零条而不是报错() {
        assert_eq!(parse_srt("").expect("能解析").cues.len(), 0);
        assert_eq!(parse_srt("\n\n  \n").expect("能解析").cues.len(), 0);
        assert_eq!(parse_ass("").expect("能解析").cues.len(), 0);
    }

    #[test]
    fn 顺序乱的块会被排序() {
        let text = "1\n00:00:05,000 --> 00:00:06,000\n后\n\n2\n00:00:01,000 --> 00:00:02,000\n先\n";
        let report = parse_srt(text).expect("能解析");
        assert_eq!(report.cues[0].text, "先");
        assert_eq!(report.cues[1].text, "后");
    }

    #[test]
    fn srt_往返稳定() {
        let text = "1\n00:00:01,000 --> 00:00:03,500\n第一句\n\n2\n00:01:00,000 --> 00:01:02,000\n第二句\n第二行\n";
        let first = parse_srt(text).expect("能解析");
        let written = to_srt(&first.cues);
        let second = parse_srt(&written).expect("能解析");
        assert_eq!(first, second, "写出去再读回来必须一模一样");
        assert_eq!(written, to_srt(&second.cues));
    }

    #[test]
    fn 解析_ass_跳过样式段并去掉覆盖标签() {
        let text = "[Script Info]\nTitle: x\n\n[V4+ Styles]\nFormat: Name, Fontname\nStyle: Default,Arial\n\n[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\nDialogue: 0,0:00:01.00,0:00:03.50,Default,,0,0,0,,{\\an8}上面\\N第二行\nDialogue: 0,0:00:04.00,0:00:06.00,Default,,0,0,0,,普通一句\n";
        let report = parse_ass(text).expect("能解析");
        assert_eq!(report.skipped, 0);
        assert_eq!(report.cues.len(), 2);
        assert_eq!(report.cues[0].start_ms, 1000);
        assert_eq!(report.cues[0].end_ms, 3500);
        assert_eq!(report.cues[0].text, "上面\n第二行", "覆盖标签要去掉，换行标记要还原");
    }

    #[test]
    fn ass_的字段序按_format_行来() {
        // 有的工具把 Start / End 写反 —— 写死列序就会读错。
        let text = "[Events]\nFormat: Layer, End, Start, Style, Name, MarginL, MarginR, MarginV, Effect, Text\nDialogue: 0,0:00:09.00,0:00:02.00,Default,,0,0,0,,正文\n";
        let report = parse_ass(text).expect("能解析");
        assert_eq!(report.cues[0].start_ms, 2000, "Start 要按名字找，不按第几列");
        assert_eq!(report.cues[0].end_ms, 9000);
    }

    #[test]
    fn ass_里坏掉的行只计数() {
        let text = "[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\nDialogue: 字段不够\nDialogue: 0,坏,坏,Default,,0,0,0,,x\nDialogue: 0,0:00:01.00,0:00:02.00,Default,,0,0,0,,好的\n";
        let report = parse_ass(text).expect("不该整份失败");
        assert_eq!(report.cues.len(), 1);
        assert_eq!(report.skipped, 2);
    }

    #[test]
    fn ass_往返稳定() {
        let cues = vec![
            Cue { start_ms: 1000, end_ms: 3500, text: "第一句".to_string(), style: CueStyle::default() },
            Cue { start_ms: 60_000, end_ms: 62_000, text: "两行\n第二行".to_string(), style: CueStyle::default() },
        ];
        let written = to_ass(&cues, &AssStyle::default());
        let back = parse_ass(&written).expect("能解析自己写的");
        assert_eq!(back.cues, cues, "写出去再读回来必须一模一样");
        assert_eq!(back.skipped, 0);
    }

    #[test]
    fn 毫秒换成帧号用序列时间基() {
        // 30fps：1000ms -> 30 帧；333ms -> 9 帧（向下取整）
        assert_eq!(frame_at_ms(1000, &tb(30, 1)), Some(30));
        assert_eq!(frame_at_ms(333, &tb(30, 1)), Some(9));
        assert_eq!(frame_at_ms(0, &tb(30, 1)), Some(0));
        // 30000/1001：一秒是 29.97 帧，向下取整是 29
        assert_eq!(frame_at_ms(1000, &tb(30000, 1001)), Some(29));
        // 坏时间基不猜
        assert_eq!(frame_at_ms(1000, &tb(0, 1)), None);
        assert_eq!(frame_at_ms(1000, &tb(30, 0)), None);
    }

    #[test]
    fn 时间戳格式化与解析互逆() {
        for ms in [0u64, 1, 999, 1000, 61_000, 3_599_999, 3_600_000, 86_399_999] {
            assert_eq!(parse_srt_time(&format_srt_time(ms)), Some(ms), "srt {ms}");
            // ASS 只有厘秒精度，所以要先截到 10ms。
            let truncated = (ms / 10) * 10;
            assert_eq!(parse_ass_time(&format_ass_time(ms)), Some(truncated), "ass {ms}");
        }
    }
}

