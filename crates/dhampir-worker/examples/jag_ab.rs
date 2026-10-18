//! jag_ab —— 底座文字栅格化的 A/B 夹具（同字号下 旧 drawtext 路 vs 新 libass 路）。
//!
//! 当前工作副本里 drawtext_args() 已经不是 drawtext 了（它现在是 libass 那条串）。
//! 所以"旧路"的 ffmpeg 参数串是照 97c3d8e 的实现**逐字符搬过来的**，
//! 不是调现成函数。那份源码留在 .workbuddy/jag-ab/old-text_raster-97c3d8e.rs。
//!
//! 用法：
//!   cargo run -p dhampir-worker --example jag_ab -- --font-file <ttf> --font-family <族名> \
//!       --font-dir <字体库目录> --out-dir <目录>

use dhampir_worker::text_raster::{
    CoverageProfile, TextBitmap, TextRasterKey, ramp_width, rasterize_line,
};
use std::path::{Path, PathBuf};
use std::process::Command;

fn border_px(font_px: u32) -> u32 {
    (font_px / 16).max(1)
}

fn filter_value(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len() + 4);
    escaped.push('\'');
    for ch in value.chars() {
        match ch {
            '\\' => escaped.push_str("\\\\"),
            ':' => escaped.push_str("\\:"),
            '\'' => escaped.push_str("'\\''"),
            _ => escaped.push(ch),
        }
    }
    escaped.push('\'');
    escaped
}

fn border_color_value(color: [u8; 4]) -> String {
    format!("0x{:02X}{:02X}{:02X}", color[0], color[1], color[2])
}

/// ⚠️ 临时目录由 `--out-dir` 给，不用 `std::env::temp_dir()`：
/// 本机实测 `C:\Users\para\AppData\Local\Temp` 对本进程**拒绝访问**
/// （`os error 5`），而 `drawtext=textfile=` 要的是一份真能读到的文件。
fn write_text_file(dir: &Path, tag: &str, text: &str) -> Result<PathBuf, String> {
    let path = dir.join(format!("jag-ab-text-{tag}.txt"));
    std::fs::write(&path, text.as_bytes())
        .map_err(|error| format!("写不了文本临时文件 {}：{error}", path.display()))?;
    Ok(path)
}

/// 旧路参数串：照 97c3d8e 的 drawtext_args 逐字符重建。
fn drawtext_args_97c3d8e(
    text_file: &Path,
    font_file: &Path,
    font_px: u32,
    width: u32,
    height: u32,
    outline: bool,
    stroke_px: u32,
    stroke_color: [u8; 4],
) -> Vec<String> {
    let source = format!("color=c=black@0.0:s={width}x{height},format=rgba");
    let mut drawtext = format!(
        "drawtext=fontfile={}:textfile={}:fontsize={}:fontcolor=white:expansion=none:x=(w-text_w)/2:y=(h-text_h)/2",
        filter_value(&font_file.to_string_lossy()),
        filter_value(&text_file.to_string_lossy()),
        font_px,
    );
    if outline {
        if stroke_px > 0 {
            drawtext.push_str(&format!(
                ":borderw={}:bordercolor={}",
                stroke_px,
                border_color_value(stroke_color)
            ));
        } else {
            drawtext.push_str(&format!(":borderw={}:bordercolor=black", border_px(font_px)));
        }
    }
    vec![
        "-v".to_string(),
        "error".to_string(),
        "-nostdin".to_string(),
        "-f".to_string(),
        "lavfi".to_string(),
        "-i".to_string(),
        source,
        "-vf".to_string(),
        drawtext,
        "-frames:v".to_string(),
        "1".to_string(),
        "-f".to_string(),
        "rawvideo".to_string(),
        "-pix_fmt".to_string(),
        "rgba".to_string(),
        "-".to_string(),
    ]
}

fn run_ffmpeg_raw(args: &[String], what: &str, expected: usize) -> Result<Vec<u8>, String> {
    let output = Command::new("ffmpeg")
        .args(args)
        .output()
        .map_err(|error| format!("起不了 ffmpeg（{what}）：{error}（PATH 里有 ffmpeg 吗？）"))?;
    if !output.status.success() {
        return Err(format!(
            "ffmpeg 画不出这一行（{what}，退出码 {:?}）：{}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    if output.stdout.len() != expected {
        return Err(format!(
            "ffmpeg 本该吐 {expected} 字节（{what}），实际 {}",
            output.stdout.len()
        ));
    }
    Ok(output.stdout)
}

/// 旧路覆盖度 = 预乘位图的 alpha 通道。
fn coverage_old(premultiplied: &[u8]) -> Vec<u8> {
    premultiplied.chunks_exact(4).map(|px| px[3]).collect()
}

/// 新路覆盖度 = rasterize_line 产物（libass alpha 恒 0，覆盖度已在 RGB 与 alpha 里）。
fn coverage_new(bitmap_pixels: &[u8]) -> Vec<u8> {
    // ⚠️⚠️ 取 **alpha**，不能取 R：终态位图的颜色已经由 tint 染好，
    // R 里放的是**样式色**而不是覆盖度 —— 描边环的 R 恒等于 stroke_color[0]（64）、
    // 填充的 R 恒等于 color[0]（220）。第一版读了 R，量出的"64 台阶占 28.9%"
    // 是夹具自己的假象。
    bitmap_pixels.chunks_exact(4).map(|px| px[3]).collect()
}

/// 把单通道覆盖度包成一张位图（覆盖度写在 alpha 上），
/// 好借用底座自己的 ramp_width（与 CoverageProfile::from_premultiplied 同形态）。
fn cov_to_bitmap(cov: &[u8], width: u32, height: u32) -> Result<TextBitmap, String> {
    let mut pixels = Vec::with_capacity(cov.len() * 4);
    for &c in cov {
        pixels.extend_from_slice(&[c, c, c, c]);
    }
    TextBitmap::new(width, height, pixels)
}

struct Stats {
    json: serde_json::Value,
    ink0: Option<(u32, u32, u32, u32)>,
    ink128: Option<(u32, u32, u32, u32)>,
    ink250: Option<(u32, u32, u32, u32)>,
}

fn bbox(cov: &[u8], width: u32, height: u32, threshold: u8) -> Option<(u32, u32, u32, u32)> {
    let mut x0 = u32::MAX;
    let mut y0 = u32::MAX;
    let mut x1 = 0u32;
    let mut y1 = 0u32;
    let mut any = false;
    for y in 0..height {
        for x in 0..width {
            if cov[(y * width + x) as usize] > threshold {
                any = true;
                if x < x0 { x0 = x; }
                if y < y0 { y0 = y; }
                if x > x1 { x1 = x; }
                if y > y1 { y1 = y; }
            }
        }
    }
    if any { Some((x0, y0, x1, y1)) } else { None }
}

fn median(sorted: &[u32]) -> f64 {
    if sorted.is_empty() { return 0.0; }
    let middle = sorted.len() / 2;
    if sorted.len() % 2 == 1 {
        f64::from(sorted[middle])
    } else {
        (f64::from(sorted[middle - 1]) + f64::from(sorted[middle])) / 2.0
    }
}

fn pct(n: usize, d: usize) -> f64 {
    if d == 0 { 0.0 } else { ((n as f64 / d as f64) * 10000.0).round() / 100.0 }
}

fn ratio(a: f64, b: f64) -> f64 {
    if b == 0.0 { 0.0 } else { ((a / b) * 10000.0).round() / 10000.0 }
}

fn analyze(cov: &[u8], width: u32, height: u32) -> Stats {
    let t: u8 = 128;
    let w = width as usize;
    let h = height as usize;

    // 相邻像素灰度差绝对值
    let mut diffs: Vec<u32> = Vec::new();
    for y in 0..h {
        for x in 0..w {
            let here = cov[y * w + x];
            if x + 1 < w {
                diffs.push(u32::from(here.abs_diff(cov[y * w + x + 1])));
            }
            if y + 1 < h {
                diffs.push(u32::from(here.abs_diff(cov[(y + 1) * w + x])));
            }
        }
    }
    let dirs = diffs.len();
    let strong = diffs.iter().filter(|&&d| d > 20).count();
    let hard = diffs.iter().filter(|&&d| d > 100).count();
    let mut sorted = diffs.clone();
    sorted.sort_unstable();
    let med = median(&sorted);

    // 墨迹量与周长（阈值 128）
    let mut area = 0usize;
    let mut boundary = 0usize;
    for y in 0..h {
        for x in 0..w {
            if cov[y * w + x] <= t { continue; }
            area += 1;
            let mut edge = false;
            if x == 0 || y == 0 || x + 1 == w || y + 1 == h {
                edge = true;
            } else {
                if cov[y * w + x + 1] <= t { edge = true; }
                if cov[y * w + x - 1] <= t { edge = true; }
                if cov[(y + 1) * w + x] <= t { edge = true; }
                if cov[(y - 1) * w + x] <= t { edge = true; }
            }
            if edge { boundary += 1; }
        }
    }
    let ramp_px = cov.iter().filter(|&&c| c > 0 && c < 255).count();

    // 台阶：沿外边界逐行取 x，数相邻行跳变 >= 2px
    let mut left: Vec<Option<usize>> = Vec::with_capacity(h);
    let mut right: Vec<Option<usize>> = Vec::with_capacity(h);
    for y in 0..h {
        let mut l: Option<usize> = None;
        let mut r: Option<usize> = None;
        for x in 0..w {
            if cov[y * w + x] > t {
                if l.is_none() { l = Some(x); }
                r = Some(x);
            }
        }
        left.push(l);
        right.push(r);
    }
    let mut jump_rows = 0usize;
    let mut row_pairs = 0usize;
    for k in 0..h.saturating_sub(1) {
        if let (Some(a), Some(b)) = (left[k], left[k + 1]) {
            row_pairs += 1;
            if a.abs_diff(b) >= 2 { jump_rows += 1; }
        }
        if let (Some(a), Some(b)) = (right[k], right[k + 1]) {
            row_pairs += 1;
            if a.abs_diff(b) >= 2 { jump_rows += 1; }
        }
    }

    let ink0 = bbox(cov, width, height, 0);
    let ink64 = bbox(cov, width, height, 64);
    let ink128 = bbox(cov, width, height, t);
    let ink250 = bbox(cov, width, height, 250);
    let box_json = |b: Option<(u32, u32, u32, u32)>| match b {
        Some((x0, y0, x1, y1)) => serde_json::json!({
            "x0": x0, "y0": y0, "x1": x1, "y1": y1,
            "w": x1 - x0 + 1, "h": y1 - y0 + 1,
        }),
        None => serde_json::Value::Null,
    };

    let json = serde_json::json!({
        "grad_dirs": dirs,
        "strong_gt20": strong,
        "hard_gt100": hard,
        "hard_over_strong_pct": pct(hard, strong),
        "strong_pct_of_dirs": pct(strong, dirs),
        "hard_pct_of_dirs": pct(hard, dirs),
        "grad_median": med,
        "area_gt128": area,
        "area_density": if (w * h) == 0 { 0.0 } else { ((area as f64 / (w * h) as f64) * 100000.0).round() / 100000.0 },
        "boundary_px": boundary,
        "perimeter_over_area": ratio(boundary as f64, area as f64),
        "compactness_p2_over_a": if area == 0 { 0.0 } else { ((boundary * boundary) as f64 / area as f64 * 100.0).round() / 100.0 },
        "ramp_px": ramp_px,
        "ramp_pct_of_area": pct(ramp_px, area),
        "stair_jump_rows": jump_rows,
        "stair_rows": row_pairs,
        "stair_jump_ratio_pct": pct(jump_rows, row_pairs),
        "ink_bbox_0": box_json(ink0),
        "ink_bbox_64": box_json(ink64),
        "ink_bbox_128": box_json(ink128),
        "ink_bbox_250": box_json(ink250),
    });
    Stats {
        json,
        ink0,
        ink128,
        ink250,
    }
}

fn write_pgm(path: &Path, cov: &[u8], width: u32, height: u32) -> Result<(), String> {
    let mut bytes = format!("P5\n{width} {height}\n255\n").into_bytes();
    bytes.extend_from_slice(cov);
    std::fs::write(path, bytes).map_err(|e| format!("写不了 {}：{e}", path.display()))
}

fn pgm_to_png(pgm: &Path, png: &Path, scale: Option<u32>) -> Result<(), String> {
    let mut args: Vec<String> = vec![
        "-v".to_string(),
        "error".to_string(),
        "-y".to_string(),
        "-i".to_string(),
        pgm.to_string_lossy().to_string(),
    ];
    if let Some(k) = scale {
        args.push("-vf".to_string());
        args.push(format!("scale=iw*{k}:ih*{k}:flags=neighbor"));
    }
    args.push("-frames:v".to_string());
    args.push("1".to_string());
    args.push(png.to_string_lossy().to_string());
    let output = Command::new("ffmpeg")
        .args(&args)
        .output()
        .map_err(|e| format!("起不了 ffmpeg（写 PNG）：{e}"))?;
    if !output.status.success() {
        return Err(format!(
            "ffmpeg 写不出 PNG {}：{}",
            png.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(())
}

struct Args {
    font_file: PathBuf,
    font_family: Option<String>,
    font_dir: Option<PathBuf>,
    out_dir: PathBuf,
    font_px: u32,
    /// 旧路单独用的字号：把两路的墨迹高对齐时用（--old-font-px）。
    /// None = 与 font_px 同号（真正的"同一字号"对照）。
    old_font_px: Option<u32>,
    width: u32,
    /// 目标画布高（产品路径里的 `target.1`）。它**不进位图几何**：位图高由行盒推
    /// （见 `run`），这里留着是为了把「目标画布」与「位图」两个量分开看。
    target_height: u32,
    stroke_px: u32,
    outer_stroke_px: u32,
}

fn parse_args(argv: Vec<String>) -> Result<Args, String> {
    let mut font_file: Option<PathBuf> = None;
    let mut font_family: Option<String> = None;
    let mut font_dir: Option<PathBuf> = None;
    let mut out_dir = PathBuf::from(".workbuddy/jag-ab");
    let mut font_px = 72u32;
    let mut old_font_px: Option<u32> = None;
    let mut width = 1920u32;
    let mut target_height = 1080u32;
    let mut stroke_px = 6u32;
    let mut outer_stroke_px = 6u32;
    let mut index = 0usize;
    while index < argv.len() {
        let flag = argv[index].clone();
        let value = |index: &mut usize, name: &str| -> Result<String, String> {
            *index += 1;
            argv.get(*index).cloned().ok_or_else(|| format!("{name} 后面缺一个值"))
        };
        match flag.as_str() {
            "--font-file" => font_file = Some(PathBuf::from(value(&mut index, "--font-file")?)),
            "--font-family" => font_family = Some(value(&mut index, "--font-family")?),
            "--font-dir" => font_dir = Some(PathBuf::from(value(&mut index, "--font-dir")?)),
            "--out-dir" => out_dir = PathBuf::from(value(&mut index, "--out-dir")?),
            "--font-px" => font_px = value(&mut index, "--font-px")?.parse().map_err(|_| "font-px 要整数".to_string())?,
            "--old-font-px" => old_font_px = Some(value(&mut index, "--old-font-px")?.parse().map_err(|_| "old-font-px 要整数".to_string())?),
            "--width" => width = value(&mut index, "--width")?.parse().map_err(|_| "width 要整数".to_string())?,
            "--height" => target_height = value(&mut index, "--height")?.parse().map_err(|_| "height 要整数".to_string())?,
            "--stroke-px" => stroke_px = value(&mut index, "--stroke-px")?.parse().map_err(|_| "stroke-px 要整数".to_string())?,
            "--outer-stroke-px" => outer_stroke_px = value(&mut index, "--outer-stroke-px")?.parse().map_err(|_| "outer-stroke-px 要整数".to_string())?,
            other => return Err(format!("不认识参数 {other}")),
        }
        index += 1;
    }
    let font_file = font_file.ok_or_else(|| "必须给 --font-file".to_string())?;
    if !font_file.is_file() {
        return Err(format!("字体文件不在：{}", font_file.display()));
    }
    Ok(Args {
        font_file,
        font_family,
        font_dir,
        out_dir,
        font_px,
        old_font_px,
        width,
        target_height,
        stroke_px,
        outer_stroke_px,
    })
}

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    if argv.iter().any(|a| a == "--help" || a == "-h") {
        println!("jag_ab --font-file <ttf> [--font-family <族名>] [--font-dir <目录>] [--out-dir <目录>]");
        return;
    }
    if let Err(message) = run(parse_args(argv)) {
        eprintln!("jag_ab: {message}");
        std::process::exit(1);
    }
}

fn run(args: Result<Args, String>) -> Result<(), String> {
    let args = args?;
    std::fs::create_dir_all(&args.out_dir)
        .map_err(|e| format!("建不了 {}：{e}", args.out_dir.display()))?;

    // 位图几何 = 产品路径（宽取整条目标宽，高取行盒 + 上下各一份 pad）。
    let line_box_px = args.font_px as f32 * 1.2;
    let pad = (args.font_px / 3).max(4);
    let bitmap_w = args.width;
    let bitmap_h = line_box_px.round() as u32 + 2 * pad;

    let stroke_color: [u8; 4] = [64, 60, 59, 255];
    let color: [u8; 4] = [220, 189, 160, 255];
    let outline = true;

    println!(
        "{}",
        serde_json::json!({
            "event": "config",
            "font_file": args.font_file.display().to_string(),
            "font_family": args.font_family,
            "font_dir": args.font_dir.as_ref().map(|p| p.display().to_string()),
            "font_px": args.font_px,
            "old_font_px": args.old_font_px.unwrap_or(args.font_px),
            "target_height": args.target_height,
            "line_box_px": line_box_px,
            "bitmap": format!("{bitmap_w}x{bitmap_h}"),
            "stroke_px": args.stroke_px,
            "outer_stroke_px": args.outer_stroke_px,
            "outline": outline,
            "color": color,
            "stroke_color": stroke_color,
            "old_source": "97c3d8e 的 drawtext_args 逐字符重建",
        })
    );

    let texts: Vec<(&str, String)> = vec![
        ("cjk", "很多都土葬，葬山上的嘛".to_string()),
        ("latin", "ABC abc 1234".to_string()),
    ];
    let mut entries: Vec<String> = Vec::new();

    for (tag, text) in &texts {
        let text_file = write_text_file(&args.out_dir, tag, text)?;
        let old_font_px = args.old_font_px.unwrap_or(args.font_px);
        let old_args = drawtext_args_97c3d8e(
            &text_file,
            &args.font_file,
            old_font_px,
            bitmap_w,
            bitmap_h,
            outline,
            args.stroke_px,
            stroke_color,
        );
        let expected = (bitmap_w as usize) * (bitmap_h as usize) * 4;
        let old_raw = run_ffmpeg_raw(&old_args, &format!("旧路 drawtext {tag}"), expected);
        let _ = std::fs::remove_file(&text_file);
        let cov_old = coverage_old(&old_raw?);

        let key = TextRasterKey {
            x_offset: 0,
            text: text.clone(),
            font_px: args.font_px,
            color,
            outline,
            stroke_px: args.stroke_px,
            stroke_color,
            outer_stroke_px: args.outer_stroke_px,
            outer_stroke_color: [255, 255, 255, 255],
            shadow_color: None,
            shadow_blur_px: 0,
            shadow_dx_px: 0,
            shadow_dy_px: 0,
            shadow_pad: 0,
            font_file: args.font_file.clone(),
            font_family: args.font_family.clone(),
            font_dir: args.font_dir.clone(),
            width: bitmap_w,
            height: bitmap_h,
        };
        let bitmap = rasterize_line(&key).map_err(|e| format!("新路栅格化失败（{tag}）：{e}"))?;
        // ⚠️ 把**染色前**的那张也留档（诊断用，与最终 PNG 无关）：
        // 染色会把覆盖度乘上样式色，之后再也看不出"原始覆盖度分布"。
        if let Ok(dir) = std::env::var("JAG_AB_DUMP_RAW") {
            let mut bytes = format!("P5\n{} {}\n255\n", bitmap.width, bitmap.height).into_bytes();
            bytes.extend(bitmap.pixels.chunks_exact(4).map(|px| px[0]));
            let _ = std::fs::write(std::path::Path::new(&dir).join(format!("{tag}-new-raw.pgm")), bytes);
        }
        let cov_new = coverage_new(&bitmap.pixels);

        let mut stats_old: Option<Stats> = None;
        let mut stats_new: Option<Stats> = None;
        for (path_tag, cov) in [("old", &cov_old), ("new", &cov_new)] {
            let stem = format!("{tag}-{path_tag}");
            let pgm = args.out_dir.join(format!("{stem}.pgm"));
            write_pgm(&pgm, cov, bitmap_w, bitmap_h)?;
            pgm_to_png(&pgm, &args.out_dir.join(format!("{stem}.png")), None)?;
            pgm_to_png(&pgm, &args.out_dir.join(format!("{stem}-x5.png")), Some(5))?;
            let stats = analyze(cov, bitmap_w, bitmap_h);
            println!(
                "{}",
                serde_json::json!({
                    "event": "raster",
                    "text_tag": tag,
                    "text": text,
                    "path": path_tag,
                    "png": args.out_dir.join(format!("{stem}.png")).display().to_string(),
                    "png_x5": args.out_dir.join(format!("{stem}-x5.png")).display().to_string(),
                    "pgm": pgm.display().to_string(),
                    "stats": stats.json,
                })
            );
            if path_tag == "old" {
                stats_old = Some(stats);
            } else {
                stats_new = Some(stats);
            }
        }
        let so = stats_old.ok_or_else(|| "旧路没有统计".to_string())?;
        let sn = stats_new.ok_or_else(|| "新路没有统计".to_string())?;

        let dims = |s: &Stats| -> (u64, u64, u64, u64) {
            let d0 = s.ink0.map(|b| ((b.2 - b.0 + 1) as u64, (b.3 - b.1 + 1) as u64)).unwrap_or((0, 0));
            let d1 = s.ink128.map(|b| ((b.2 - b.0 + 1) as u64, (b.3 - b.1 + 1) as u64)).unwrap_or((0, 0));
            (d0.0, d0.1, d1.0, d1.1)
        };
        let (ow0, oh0, ow1, oh1) = dims(&so);
        let (nw0, nh0, nw1, nh1) = dims(&sn);
        let area_old = so.json["area_gt128"].as_u64().unwrap_or(0);
        let area_new = sn.json["area_gt128"].as_u64().unwrap_or(0);
        let sf = |k: &str, s: &Stats| s.json[k].as_f64().unwrap_or(0.0);
        let ramp_of = |cov: &[u8]| -> f32 {
            cov_to_bitmap(cov, bitmap_w, bitmap_h)
                .map(|b| ramp_width(&CoverageProfile::from_premultiplied(&b)))
                .unwrap_or(-1.0)
        };
        let ramp_old = ramp_of(&cov_old);
        let ramp_new = ramp_of(&cov_new);

        println!(
            "{}",
            serde_json::json!({
                "event": "align",
                "text_tag": tag,
                "text": text,
                "old_ink_wh_t0": [ow0, oh0],
                "new_ink_wh_t0": [nw0, nh0],
                "old_ink_wh_t128": [ow1, oh1],
                "new_ink_wh_t128": [nw1, nh1],
                "width_ratio_new_over_old": ratio(nw1 as f64, ow1 as f64),
                "height_ratio_new_over_old": ratio(nh1 as f64, oh1 as f64),
                "same_ink_height_t128": nh1 == oh1,
                "area_old": area_old,
                "area_new": area_new,
                "area_ratio_new_over_old": ratio(area_new as f64, area_old as f64),
                "perimeter_over_area_old": sf("perimeter_over_area", &so),
                "perimeter_over_area_new": sf("perimeter_over_area", &sn),
                "perimeter_ratio_new_over_old": ratio(sf("boundary_px", &sn), sf("boundary_px", &so)),
                "stair_jump_ratio_pct_old": sf("stair_jump_ratio_pct", &so),
                "stair_jump_ratio_pct_new": sf("stair_jump_ratio_pct", &sn),
                "hard_over_strong_pct_old": sf("hard_over_strong_pct", &so),
                "hard_over_strong_pct_new": sf("hard_over_strong_pct", &sn),
                "grad_median_old": sf("grad_median", &so),
                "grad_median_new": sf("grad_median", &sn),
                "ramp_pct_of_area_old": sf("ramp_pct_of_area", &so),
                "ramp_pct_of_area_new": sf("ramp_pct_of_area", &sn),
                "ramp_width_old_px": (ramp_old * 1000.0).round() / 1000.0,
                "ramp_width_new_px": (ramp_new * 1000.0).round() / 1000.0,
                "old_ink_wh_t250": [so.ink250.map(|b| b.2 - b.0 + 1).unwrap_or(0), so.ink250.map(|b| b.3 - b.1 + 1).unwrap_or(0)],
                "new_ink_wh_t250": [sn.ink250.map(|b| b.2 - b.0 + 1).unwrap_or(0), sn.ink250.map(|b| b.3 - b.1 + 1).unwrap_or(0)],
            })
        );
        entries.push(format!("{tag}: old/new"));
    }

    println!("{}", serde_json::json!({ "event": "done", "entries": entries }));
    Ok(())
}

