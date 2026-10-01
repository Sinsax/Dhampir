//! T2.3b 的测量夹具：**一条字幕的栅格化要多久**。
//!
//! # 这个数为什么必须有
//!
//! 字幕的字形像素来自 ffmpeg 的 drawtext —— 也就是**一条字幕至少要起一次进程**。
//! 一条 3 秒的字幕在 30fps 下要在 90 帧上出现。所以「有没有缓存」不是优化题，是可行性题。
//! 这个夹具把两件事**分开**量，因为它们的量纲完全不同：
//!
//! * **冷**（第一次见到这一行）：写临时文本文件 + 起 ffmpeg + 读回字节 + 染色；
//! * **热**（命中缓存）：一次哈希查找加一次 `Rc` 克隆。
//!
//! 于是「一条字幕在 N 帧上要花多少栅格化时间」= 一次冷 + (N-1) 次热。
//! 这个合成模型也打印出来（`cue` 事件），**别拿冷路径的耗时乘帧数** ——
//! 那是「没有缓存」的世界，而缓存正是这个文件要证的那件事。
//!
//! # 口径与边界（写进输出，别当结论用）
//!
//! * 位图宽度取**整条目标宽**（理由见 `text_raster` 的模块文档），所以 1080p 下一张约 0.8 MB。
//!   冷路径要读这么多字节 —— 那是「按整条宽取」的代价，**不是**这一行的实际墨迹量。
//! * 冷路径的耗时里**大部分是 ffmpeg 的进程启动**。本仓的 Rust 部分是 debug 构建
//!   （`cargo run` 的默认档），ffmpeg 是 release 的外部程序 —— 这个差异只会让
//!   我们的那部分显得更慢，不会把结论往好里说。
//! * 字体由 `--font-file` 给。**本仓不猜系统字体**，所以这个参数是必需的。
//! * 绘制内容固定为「白字 + 黑描边」，样式色由染色那一步上 —— 与产品路径同源。
//!
//! 输出是 NDJSON（每行一个事件），给 `scripts/measure-raster.mjs` 解析。
//! **要数字就解析它，别解析人话** —— 人话会改。
//!
//! 用法：
//!
//! ```text
//! cargo run -p dhampir-worker --example raster_cost -- --font-file C:/Windows/Fonts/msyh.ttc
//! cargo run -p dhampir-worker --example raster_cost -- --font-file ... --cold 12 --warm 50000
//! ```

use dhampir_core::timeline::layer::SubtitleStyle;
use dhampir_core::timeline::text_layout::LINE_HEIGHT_EM;
use dhampir_worker::text_raster::{TextRasterKey, TextRasterizer, bitmap_size, rasterize_line};
use std::path::PathBuf;
use std::time::Instant;

/// 冷路径的取样行。**要的是「真字幕长什么样」**：中文、中英混排、长行、标点。
///
/// 取样行不够时循环使用 —— 冷路径每次调用都真起进程，所以重复的文本
/// 也还是一份**新的**冷成本，不会变成命中。
const LINES: &[&str] = &[
    "第一行中文",
    "Mixed 混排 text with a rather long tail that ought to wrap somewhere",
    "短",
    "标点，。！？；：（）「」——都得能画出来",
    "数字 0 1 2 3 4 5 6 7 8 9 与 % 号",
    "一二三四五六七八九十一二三四五六七八九十",
    "ABC abc 123 ???",
    "（一条带全角括号与省略号的中文长句，用来测行盒够不够）",
];

/// 默认目标尺寸：**文档坐标系那一档**（样本工程）+ 常态出片那一档（1080p）。
const DEFAULT_TARGETS: &[(u32, u32)] = &[(1920, 1080), (640, 360)];

struct Args {
    font_file: PathBuf,
    targets: Vec<(u32, u32)>,
    /// 冷路径取几次样（每次都真起一次进程）。
    cold: usize,
    /// 热路径循环多少次（全部命中缓存）。
    warm: usize,
    /// 合成模型里「一条字幕出现在几帧上」。
    frames: usize,
}

fn usage() -> String {
    "用法：raster_cost --font-file <字体文件> [--target WxH]... [--cold N] [--warm N] [--frames N]"
        .to_string()
}

fn parse_args(argv: Vec<String>) -> Result<Args, String> {
    let mut font_file: Option<PathBuf> = None;
    let mut targets: Vec<(u32, u32)> = Vec::new();
    let mut cold = 8usize;
    let mut warm = 20_000usize;
    let mut frames = 90usize; // 3 秒 @30fps

    let mut index = 0;
    while index < argv.len() {
        let flag = argv[index].as_str();
        let mut next = |name: &str| -> Result<String, String> {
            index += 1;
            argv.get(index)
                .cloned()
                .ok_or_else(|| format!("{name} 后面缺一个值；{}", usage()))
        };
        match flag {
            "--font-file" => font_file = Some(PathBuf::from(next("--font-file")?)),
            "--target" => {
                let raw = next("--target")?;
                targets.push(parse_target(&raw)?);
            }
            "--cold" => {
                cold = next("--cold")?
                    .parse()
                    .map_err(|_| "--cold 要一个正整数".to_string())?
            }
            "--warm" => {
                warm = next("--warm")?
                    .parse()
                    .map_err(|_| "--warm 要一个正整数".to_string())?
            }
            "--frames" => {
                frames = next("--frames")?
                    .parse()
                    .map_err(|_| "--frames 要一个正整数".to_string())?
            }
            other => return Err(format!("不认识参数 `{other}`；{}", usage())),
        }
        index += 1;
    }

    let font_file = font_file.ok_or_else(|| format!("必须给 --font-file；{}", usage()))?;
    // 字体不存在要在**起进程之前**说人话，否则 8 次取样会浪费在 8 条同样的错误上。
    if !font_file.exists() {
        return Err(format!("字体文件不存在：{}", font_file.display()));
    }
    if cold == 0 || warm == 0 || frames == 0 {
        return Err("--cold / --warm / --frames 都要大于 0".to_string());
    }
    if targets.is_empty() {
        targets = DEFAULT_TARGETS.to_vec();
    }
    Ok(Args {
        font_file,
        targets,
        cold,
        warm,
        frames,
    })
}

fn parse_target(raw: &str) -> Result<(u32, u32), String> {
    let (w, h) = raw
        .split_once('x')
        .or_else(|| raw.split_once('X'))
        .ok_or_else(|| format!("--target 要写成 WxH，收到 `{raw}`"))?;
    let w: u32 = w
        .trim()
        .parse()
        .map_err(|_| format!("宽度不是整数：`{raw}`"))?;
    let h: u32 = h
        .trim()
        .parse()
        .map_err(|_| format!("高度不是整数：`{raw}`"))?;
    if w == 0 || h == 0 {
        return Err(format!("尺寸不能为 0：`{raw}`"));
    }
    Ok((w, h))
}

fn emit(value: serde_json::Value) {
    println!("{value}");
}

/// 四舍五入到三位小数，免得打印一堆没意义的尾数。
fn rounded(value: f64) -> f64 {
    (value * 1000.0).round() / 1000.0
}

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    if argv.iter().any(|flag| flag == "--help" || flag == "-h") {
        println!("{}", usage());
        return;
    }
    match run(parse_args(argv)) {
        Ok(()) => {}
        Err(message) => {
            eprintln!("raster_cost: {message}");
            std::process::exit(1);
        }
    }
}

fn run(args: Result<Args, String>) -> Result<(), String> {
    let args = args?;
    // 字号与行高**照产品路径算**：字号 = font_ratio × 高度，行高 = 1.2em。
    // 这两个数一旦和 text_layout 分叉，测出来的就不是产品路径的成本。
    let style = SubtitleStyle::default();

    let mut cold_all: Vec<f64> = Vec::new();
    for &(width, height) in &args.targets {
        let font_px = (style.font_ratio * height as f32).round() as u32;
        let line_box_px = font_px as f32 * LINE_HEIGHT_EM;
        let (bitmap_w, bitmap_h) = bitmap_size(width, line_box_px, font_px);
        let bytes = bitmap_w as u64 * bitmap_h as u64 * 4;

        emit(serde_json::json!({
            "event": "config",
            "target": format!("{width}x{height}"),
            "font_px": font_px,
            "line_box_px": rounded(line_box_px as f64),
            "bitmap": format!("{bitmap_w}x{bitmap_h}"),
            "bytes": bytes,
            "cold_samples": args.cold,
            "warm_ops": args.warm,
        }));

        // --- 冷：每次都真起一次 ffmpeg -------------------------------------
        let mut cold_samples: Vec<f64> = Vec::with_capacity(args.cold);
        let mut touched_edge = 0usize;
        for index in 0..args.cold {
            let text = LINES[index % LINES.len()];
            let key = TextRasterKey {
                x_offset: 0,
                text: text.to_string(),
                font_px,
                color: style.color,
                outline: style.outline,
                stroke_px: 0,
                stroke_color: [0, 0, 0, 255],
                // 这条测量路径量的是**文字位图**：阴影那一张不在它的范围内
                // （它要多一次 ffmpeg 调用，会把"一行字多少钱"这个数搅浑）。
                shadow_color: None,
                shadow_blur_px: 0,
                shadow_dx_px: 0,
                shadow_dy_px: 0,
                shadow_pad: 0,
                font_file: args.font_file.clone(),
                width: bitmap_w,
                height: bitmap_h,
            };
            let started = Instant::now();
            let bitmap = rasterize_line(&key).map_err(|error| {
                format!("栅格化失败（{width}x{height} 第 {index} 次）：{error}")
            })?;
            let ms = started.elapsed().as_secs_f64() * 1000.0;
            // 「字被切了」在测量里也要报出来：它是产品缺陷，不是测量噪声。
            if bitmap.ink_touches_edge() {
                touched_edge += 1;
            }
            cold_samples.push(ms);
            cold_all.push(ms);
            emit(serde_json::json!({
                "event": "cold",
                "target": format!("{width}x{height}"),
                "index": index,
                "text": text,
                "ms": rounded(ms),
                "ink_touches_edge": bitmap.ink_touches_edge(),
            }));
        }
        let cold_stats = stats(&cold_samples);

        // --- 热：同一个键过缓存 -------------------------------------------
        let warm_text = LINES[0];
        let warm_key = TextRasterKey {
            x_offset: 0,
            text: warm_text.to_string(),
            font_px,
            color: style.color,
            outline: style.outline,
            stroke_px: 0,
            stroke_color: [0, 0, 0, 255],
            shadow_color: None,
            shadow_blur_px: 0,
            shadow_dx_px: 0,
            shadow_dy_px: 0,
            shadow_pad: 0,
            font_file: args.font_file.clone(),
            width: bitmap_w,
            height: bitmap_h,
        };
        let mut rasterizer = TextRasterizer::new();
        // 先把它灌进缓存 —— 这一次是未命中，不计进热路径。
        rasterizer
            .rasterize(&warm_key)
            .map_err(|error| format!("预热失败（{width}x{height}）：{error}"))?;
        let started = Instant::now();
        for _ in 0..args.warm {
            rasterizer
                .rasterize(&warm_key)
                .map_err(|error| format!("热路径失败（{width}x{height}）：{error}"))?;
        }
        let warm_total_ms = started.elapsed().as_secs_f64() * 1000.0;
        let warm_ns_per_op = warm_total_ms * 1_000_000.0 / args.warm as f64;

        emit(serde_json::json!({
            "event": "cold_summary",
            "target": format!("{width}x{height}"),
            "count": cold_stats.count,
            "min_ms": cold_stats.min,
            "median_ms": cold_stats.median,
            "max_ms": cold_stats.max,
            "ink_touches_edge": touched_edge,
        }));
        emit(serde_json::json!({
            "event": "warm",
            "target": format!("{width}x{height}"),
            "ops": args.warm,
            "total_ms": rounded(warm_total_ms),
            "ns_per_op": rounded(warm_ns_per_op),
            "hits": rasterizer.hits(),
            "misses": rasterizer.misses(),
            "cached": rasterizer.cached(),
            // 缓存把内存钉在哪：容量 × 一张的字节数（见 text_raster::CACHE_CAPACITY）。
            "resident_bytes": rasterizer.cached() as u64 * bytes,
        }));

        // --- 合成模型：一条字幕在 frames 帧上出现 ---------------------------
        let cue_ms = cold_stats.median
            + (args.frames.saturating_sub(1)) as f64 * (warm_total_ms / args.warm as f64);
        emit(serde_json::json!({
            "event": "cue",
            "target": format!("{width}x{height}"),
            "frames": args.frames,
            "cold_ms": cold_stats.median,
            "one_cold_plus_warm_ms": rounded(cue_ms),
            "note": "1 冷 + (frames-1) 热；这是缓存工作时的模型",
        }));
    }

    let overall = stats(&cold_all);
    emit(serde_json::json!({
        "event": "done",
        "targets": args.targets.len(),
        "cold_count": overall.count,
        "cold_median_ms": overall.median,
    }));
    Ok(())
}

struct Stats {
    count: usize,
    min: f64,
    median: f64,
    max: f64,
}

/// **中位数与两端都给** —— 只给均值会把抖动藏起来（P5 那次的教训）。
fn stats(samples: &[f64]) -> Stats {
    let mut sorted = samples.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    if sorted.is_empty() {
        return Stats {
            count: 0,
            min: 0.0,
            median: 0.0,
            max: 0.0,
        };
    }
    let middle = sorted.len() / 2;
    let median = if sorted.len() % 2 == 1 {
        sorted[middle]
    } else {
        (sorted[middle - 1] + sorted[middle]) / 2.0
    };
    Stats {
        count: sorted.len(),
        min: rounded(sorted[0]),
        median: rounded(median),
        max: rounded(sorted[sorted.len() - 1]),
    }
}
