//! T5.3 的测量工具：**解码这一段的代价与并发**。
//!
//! # 它回答的两个问题
//!
//! 1. **[A10]** 单线程顺序解码的吞吐上限是多少？—— 报「每读一个源帧多少毫秒」。
//!    并发度数在**代码里**（一次只有一个源在读，见下），这里量的是它的**代价**。
//! 2. **[D11]** 此前那次出片吞吐（plan/measurements.md 第五项）有个洞：
//!    样本工程的四份素材**指向同一个文件**，页缓存会帮忙，所以那个数是**下界**。
//!    这个工具用 `fixtures/four-asset-project.doc.json`（同一条时间线、四份**不同**文件）
//!    把那个洞补上 —— 两个工程逐字段同形，差别只有"文件是四份还是一份"。
//!
//! # 并发数是多少，为什么
//!
//! **1。** 取帧是一条同步循环：每个输出帧里按层序依次 `texture_for`，每一次都是一次
//! 阻塞的 `read_exact`（`SourcePool::read_frame`）。所以**同时最多一个源在解码**，
//! 哪怕开着 N 路解码器（`opened_streams`）。
//!
//! 代价有两项，都是可测的：
//!
//! * **进程与内存**：N 路 = N 个 ffmpeg 子进程 + N 个池子（`pool_slots` 按**源自己的**
//!   尺寸算，上限 192 MiB/源）；
//! * **不重叠**：解码不能与合成/读回重叠，所以 `ms_per_source_read` 是**串行**加的，
//!   源读得越多总耗时越长 —— 这个数由本工具给出。
//!
//! 输出里还专门把 **PNG 编码**那一段单独量了（`png_encode_ms_per_frame`）并从每帧成本里
//! 剔掉（`ms_per_frame_excl_png`）。理由：这一路是 PNG sink，而 debug 构建的 deflate
//! 一点不便宜；不剔掉的话，「每帧多少毫秒」会被当成解码开销，而那是错的。
//!
//! # 它为什么不走出片那条路
//!
//! 它走 `render_frames_png_run`：**同一条**解码 -> 上传 -> 求值 -> 合成 -> 回读的路，
//! 只把 sink 从 mp4 换成 PNG。这样量到的解码账与出片那条**是同一组数**
//! （`rewind` 那条集成测试里两者逐项相等，可对账），而且不需要编码器。
//!
//! # 用法
//!
//!     cargo run -p dhampir-worker --example decode_cost -- <工程文件> [--frames N]
//!
//! 输出 NDJSON：一行一个工程。**ms 是墙钟，含 PNG 落盘**，量纲限制写在字段名旁边。

use std::path::{Path, PathBuf};
use std::time::Instant;

use dhampir_core::compose;
use dhampir_core::overlay::SubtitleTable;
use dhampir_core::timeline::project::{load_doc, ProjectDoc};
use dhampir_worker::pipeline::{
    AudioMode, RenderPlan, SourceTable, frame_bytes, render_frames_png_run,
};

fn repo_root() -> PathBuf {
    // examples/ 与 crate 根的关系是编译期定的，运行期不变。
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("manifest 一定在 crates/<名字> 里")
        .to_path_buf()
}

fn sources_of(doc: &ProjectDoc, root: &Path) -> SourceTable {
    let mut table = SourceTable::new();
    for asset in &doc.assets {
        if asset.id.is_empty() || asset.uri.is_empty() {
            continue;
        }
        table.insert(asset.id.clone(), root.join(&asset.uri));
    }
    table
}

/// PNG 落盘那一项要**单独量出来**：它在 debug 构建下不便宜，而它**不是解码**。
/// 不把它分出来，「每帧多少毫秒」就会被当成解码开销用 —— 那是错的。
///
/// 做法：把**每一帧**已经落盘的 PNG 读回来再编码一次，报**均值**（和最大值）。
/// 为什么不是取前几帧的中位：这一路每帧的内容差别很大（模糊/关键帧/不透明度
/// 都影响可压缩性），前几帧恰好是最好压的那种，用它们会把 PNG 成本**低估一个量级**。
fn png_encode_stats(frames: &[dhampir_worker::pipeline::FramePng], sample: usize) -> (f64, f64) {
    let mut per_encode = Vec::new();
    let step = if sample == 0 || sample >= frames.len() {
        1
    } else {
        frames.len().div_ceil(sample)
    };
    for frame in frames.iter().step_by(step) {
        let Ok(bytes) = std::fs::read(&frame.path) else {
            continue;
        };
        let Ok(image) = dhampir_core::readback::Rgba8Image::decode_png(&bytes) else {
            continue;
        };
        let scratch = frame.path.with_extension("remeasure.png");
        let started = Instant::now();
        if image.write_png(&scratch).is_err() {
            continue;
        }
        per_encode.push(started.elapsed().as_secs_f64() * 1000.0);
        let _ = std::fs::remove_file(&scratch);
    }
    if per_encode.is_empty() {
        return (0.0, 0.0);
    }
    let mean = per_encode.iter().sum::<f64>() / per_encode.len() as f64;
    let max = per_encode.iter().copied().fold(f64::MIN, f64::max);
    (mean, max)
}

/// 量一个工程。
fn measure(project: &str, limit: Option<i64>, png_sample: usize) -> Result<serde_json::Value, String> {
    let root = repo_root();
    let path = root.join(project);
    let text = std::fs::read_to_string(&path).map_err(|e| format!("读不了 {project}：{e}"))?;
    let doc = load_doc(&text).map_err(|e| format!("载不进 {project}：{e}"))?;

    let asset_root = root.join("target/s3");
    let sources = sources_of(&doc, &asset_root);
    let timebases = doc.asset_timebases();
    let from = compose::first_frame_v2(&doc.timeline).ok_or("工程没有帧")?;
    let last = compose::end_frame_v2(&doc.timeline)
        .ok_or("工程没有结束帧")?
        .saturating_sub(1);
    let to = match limit {
        Some(count) => (from + count - 1).min(last),
        None => last,
    };

    // 每个源一帧裸像素多少字节：**按工程自己声明的尺寸**算（与池子算槽数用的是同一个数）。
    let frame_bytes_of: serde_json::Map<String, serde_json::Value> = doc
        .assets
        .iter()
        .map(|asset| {
            let bytes = frame_bytes(asset.width.unwrap_or(0), asset.height.unwrap_or(0));
            (
                asset.id.clone(),
                serde_json::json!({ "width": asset.width, "height": asset.height, "bytes": bytes }),
            )
        })
        .collect();

    let frames: Vec<i64> = (from..=to).collect();
    let dir = root.join("target/t5/decode-cost");
    std::fs::create_dir_all(&dir).map_err(|e| format!("建不了目录：{e}"))?;
    let output = dir.join("frames.png");

    let subtitles = SubtitleTable::new();
    let plan = RenderPlan {
        timeline: &doc.timeline,
        sources: &sources,
        asset_timebases: &timebases,
        from,
        to,
        width: doc.render_hints.width,
        height: doc.render_hints.height,
        sequence: doc.sequence_size(),
        subtitles: &subtitles,
        font_file: None,
        // 这条尺子量的是**解码**，不是音频：音轨一律不走。
        audio: AudioMode::Silent,
        output: &output,
    };

    let started = Instant::now();
    let run = render_frames_png_run(&plan, &frames)?;
    let wall_ms = started.elapsed().as_secs_f64() * 1000.0;

    let frames = run.frames.len();
    let reads = run.decode.frames_read;
    let (png_ms, png_ms_max) = png_encode_stats(&run.frames, png_sample);
    // 把 PNG 那一段剔掉之后，剩下的才是「解码 + 上传 + 合成 + 读回」。
    let render_ms = (wall_ms - png_ms * frames as f64).max(0.0);
    Ok(serde_json::json!({
        "project": project,
        "from": from,
        "to": to,
        "frames": frames,
        // 引用到的源数 = 会开的解码器数（不是"同时在工作"的并行度）。
        "opened_streams": run.opened_streams,
        "distinct_sources": run.opened_streams,
        "decode": run.decode,
        "wall_ms": round2(wall_ms),
        // 每个**输出帧**的墙钟（含 PNG 落盘、解码、合成、读回）。
        "ms_per_frame": round3(ratio(wall_ms, frames)),
        // PNG 编码一帧多少毫秒（debug 构建下不可忽略，所以它是**被减掉的那一项**）。
        "png_encode_ms_per_frame": round3(png_ms),
        "png_encode_ms_per_frame_max": round3(png_ms_max),
        // **剔掉 PNG 之后**每帧多少毫秒 —— 这一项才与出片那条路可比。
        "ms_per_frame_excl_png": round3(ratio(render_ms, frames)),
        // 每个**源帧读**的墙钟（剔掉 PNG）。解码串行，所以这是吞吐上限的直接度量。
        "ms_per_source_read": round3(ratio(render_ms, reads)),
        "source_frame_bytes": frame_bytes_of,
    }))
}

/// 商；分母为 0 时给 0（**不是** Infinity，也不是 panic）。
fn ratio(total: f64, count: usize) -> f64 {
    if count == 0 { 0.0 } else { total / count as f64 }
}

fn round2(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

fn round3(value: f64) -> f64 {
    (value * 1000.0).round() / 1000.0
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut projects: Vec<String> = Vec::new();
    let mut limit: Option<i64> = None;
    let mut png_sample: usize = 0;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--frames" => {
                index += 1;
                match args.get(index).and_then(|value| value.parse::<i64>().ok()) {
                    Some(count) => limit = Some(count),
                    None => {
                        eprintln!("--frames 要一个整数");
                        std::process::exit(2);
                    }
                }
            }
            "--png-sample" => {
                index += 1;
                match args.get(index).and_then(|value| value.parse::<usize>().ok()) {
                    Some(count) => png_sample = count,
                    None => {
                        eprintln!("--png-sample 要一个整数");
                        std::process::exit(2);
                    }
                }
            }
            name => projects.push(name.to_string()),
        }
        index += 1;
    }
    if projects.is_empty() {
        projects = vec![
            "fixtures/sample-project.doc.json".to_string(),
            "fixtures/four-asset-project.doc.json".to_string(),
        ];
    }

    for project in &projects {
        match measure(project, limit, png_sample) {
            Ok(row) => println!("{row}"),
            Err(error) => {
                eprintln!("{project}：{error}");
                std::process::exit(1);
            }
        }
    }
}
