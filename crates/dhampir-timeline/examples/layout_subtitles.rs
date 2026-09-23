//! 把一段字幕按**共享布局**排出来，打印成 JSON。
//!
//! # 它为什么存在
//!
//! T2 的两个宿主都要消费**同一份结构**：这一帧要画几行、每行是什么、每行占哪个
//! 归一化矩形。这份结构由 dhampir_timeline::text_layout 算出来 —— 宿主一旦各算各的，
//! 同一个工程在两个宿主上就是两份不同的字幕，而那正是这个项目最贵的那条不变量。
//!
//! 这个例子是那份结构的**参照输出**：宿主的输出与它不一致，就是宿主错了。
//! 它同时也让那个模块**一出生就有调用方** —— 只写不用的公共 API 比没有更容易误导
//! （dhampir-media 的状态守卫就是为这件事立的）。
//!
//! 用法：
//!
//!     cargo run -p dhampir-timeline --example layout_subtitles -- \
//!         --srt a.srt --fps 30/1 --frame 45 --sequence 640x360
//!
//! 输出一行 JSON 到 stdout；诊断走 stderr，两者不混。

use dhampir_timeline::layer::SubtitleStyle;
use dhampir_timeline::schema::TimebaseDto;
use dhampir_timeline::subtitle::{frame_at_ms, parse_srt};
use dhampir_timeline::text_layout::layout;

fn value_of(args: &[String], name: &str, fallback: Option<&str>) -> Result<String, String> {
    match args.iter().position(|item| item == name) {
        Some(index) => args
            .get(index + 1)
            .cloned()
            .ok_or_else(|| format!("{name} 后面要跟一个值")),
        None => fallback
            .map(|value| value.to_string())
            .ok_or_else(|| format!("缺参数 {name}")),
    }
}

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|item| item == "--help" || item == "-h") {
        eprintln!("用法：layout_subtitles --srt <文件> [--fps 30/1] [--frame 0] [--sequence 640x360] [--font-ratio 0.055] [--max-lines 2]");
        return Ok(());
    }

    let srt_path = value_of(&args, "--srt", None)?;
    let text = std::fs::read_to_string(&srt_path)
        .map_err(|error| format!("读不了 {srt_path}：{error}"))?;

    let fps = value_of(&args, "--fps", Some("30/1"))?;
    let (num, den) = match fps.split_once('/') {
        Some((num, den)) => (
            num.parse::<u32>().map_err(|_| format!("帧率分子不是数：{num}"))?,
            den.parse::<u32>().map_err(|_| format!("帧率分母不是数：{den}"))?,
        ),
        None => return Err(format!("帧率要写成 分子/分母，得到 {fps}")),
    };
    if num == 0 || den == 0 {
        return Err(format!("帧率不能有 0：{fps}"));
    }
    let timebase = TimebaseDto { num, den };

    let frame: i64 = value_of(&args, "--frame", Some("0"))?
        .parse()
        .map_err(|_| "--frame 不是整数".to_string())?;

    let sequence_text = value_of(&args, "--sequence", Some("640x360"))?;
    let (width, height) = match sequence_text.split_once('x') {
        Some((width, height)) => (
            width.parse::<u32>().map_err(|_| format!("宽不是数：{width}"))?,
            height.parse::<u32>().map_err(|_| format!("高不是数：{height}"))?,
        ),
        None => return Err(format!("文档坐标系要写成 宽x高，得到 {sequence_text}")),
    };
    if width == 0 || height == 0 {
        return Err("文档坐标系不能有 0".to_string());
    }

    let mut style = SubtitleStyle::default();
    style.font_ratio = value_of(&args, "--font-ratio", Some("0.055"))?
        .parse()
        .map_err(|_| "--font-ratio 不是数".to_string())?;
    style.max_lines = value_of(&args, "--max-lines", Some("2"))?
        .parse()
        .map_err(|_| "--max-lines 不是整数".to_string())?;

    let report = parse_srt(&text)?;

    let mut active = Vec::new();
    for cue in &report.cues {
        let Some(start) = frame_at_ms(cue.start_ms, &timebase) else {
            continue;
        };
        let Some(end) = frame_at_ms(cue.end_ms, &timebase) else {
            continue;
        };
        // 区间取**闭开** [start, end)：SRT 的结束时间是「什么时候消失」。
        // 短于一帧的字幕（end <= start）仍然在 start 那一帧显示 —— 否则一条都不显示，
        // 而「一条都不显示」看起来和「这条字幕不存在」一样，属于最难查的那类。
        let visible = if end > start {
            frame >= start && frame < end
        } else {
            frame == start
        };
        if !visible {
            continue;
        }
        let laid = layout(&cue.text, &style, (width, height));
        let lines: Vec<serde_json::Value> = laid
            .lines
            .iter()
            .map(|line| {
                serde_json::json!({
                    "text": line.text,
                    "rect": {
                        "x": line.rect.x,
                        "y": line.rect.y,
                        "width": line.rect.width,
                        "height": line.rect.height,
                    },
                })
            })
            .collect();
        active.push(serde_json::json!({
            "start_ms": cue.start_ms,
            "end_ms": cue.end_ms,
            "start_frame": start,
            "end_frame": end,
            "lines": lines,
            "dropped_lines": laid.dropped_lines,
        }));
    }

    println!(
        "{}",
        serde_json::json!({
            "srt": srt_path,
            "frame": frame,
            "timebase": fps,
            "sequence": [width, height],
            "cues_total": report.cues.len(),
            "skipped_blocks": report.skipped,
            "active": active,
        })
    );
    Ok(())
}
