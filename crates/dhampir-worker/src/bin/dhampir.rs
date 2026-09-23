//! 底座的命令行：dhampir probe | info | gop | frame | render。
//!
//! # 它为什么属于底座
//!
//! 在此之前，后端的能力只活在 examples/ 里 —— 例子是给人看的，没有任何稳定性承诺，
//! 也不会被任何守卫盯着。而「远端模式」与「本机模式」的差别**只是后端在哪**，
//! 所以后端真正需要的那个东西，就是一个能被任何宿主张起来的**确定性入口**：
//!
//! > 本机后端（scripts/dhampir-local.mjs）就是这么用它的：
//! > HTTP 层在 Node，渲染在 Rust，两边都不重复实现对方的活。
//!
//! # 输出契约
//!
//! * 需要读结构的子命令（probe / info / gop / frame）在 stdout 上打**一个 JSON 文档**；
//! * render 在 stdout 上打 **NDJSON**（一行一个对象）：先 start，再若干 progress，末行 done。
//!   带进度是因为出片是分钟级的活，调用方要能显示进度而不能干等。
//! * 诊断、警告、被忽略的音轨一律走 **stderr** —— stdout 只给机器读。
//!
//! # 退出码（契约的一部分，被 scripts/cli-contract.mjs 钉住）
//!
//! | 码 | 含义 |
//! |---|---|
//! | 0 | 成功 |
//! | 2 | 用法错，或工程校验有 error（**用户能改的**） |
//! | 1 | 运行期失败（GPU、ffmpeg、写盘、渲染报告判失败） |
//!
//! **不认识的参数一律退出 2**，不静默丢掉 —— 静默丢参数会让 --width 打错字变成
//! 「用默认尺寸跑完了」，而人以为已经生效。

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use dhampir_core::compose;
use dhampir_core::effects::REGISTRY;
use dhampir_core::timeline::host_api::{AssetInfoView, SampleView, gop_slices};
use dhampir_core::timeline::edit::{EditOp, apply as apply_edit};
use dhampir_core::timeline::project::{
    Asset, AssetKind, ProjectDoc, asset_reference_counts, load_doc, validate_project_doc,
};
use dhampir_core::timeline::schema::{TimebaseDto, TrackKind};
use dhampir_worker::pipeline::{RenderPlan, SourceTable, render_frames_png, render_plan};

const USAGE: &str = "\
用法：dhampir <子命令> [选项]

子命令：
  probe   --project <文件>                        解析并校验工程，打印 DocIssues
  info    --asset <文件>                          打印素材信息（尺寸/帧数/时间基/GOP 长度）
  gop     --asset <文件>                          打印 GOP 切片表
  frame   --project <文件> --frame <N> --out <目录>
                                                  出第 N 帧的 PNG（文件名 frame-<N>.png）
  render  --project <文件> --from <N> --to <N> --out <文件.mp4>
                                                  出片（stdout 是 NDJSON 进度）

公共选项：
  import  --project <文件> --file <素材> [--id <id>] [--replace] [--write]
                         把一个文件登记成资产（ffprobe 自动填尺寸/帧数/时间基）。
                         不给 --write 就是**干跑**：只打印将要写入的那一条
  library --project <文件>
                         列出素材库：每个资产被引用了多少次
  edit    --project <文件> --op <JSON> [--write]
                         执行一次编辑操作（与浏览器走的是同一份实现）。
                         形状是一个带 op 字段的 JSON 对象，六个操作：
                         insert / trim / split / move / remove / set_sequence
                         （split 的形状：op=split, layer=c, at=75）
                         **不给 --write 就只在内存里做一遍并打印结果**

公共选项：
  --asset-root <目录>   工程文件里 asset.uri 的相对根（默认 target/s3）
  --asset-map <文件>     兜底资产登记表（形状：assets.<id>.file）。
                         **只补工程文件没登记的 id**：工程文件里的位置永远优先
  --width <像素>        输出宽度（默认取工程文件里的 render_hints.width）
  --height <像素>       输出高度（默认取工程文件里的 render_hints.height）
  -h, --help            显示本帮助

退出码：0 成功 / 2 用法或校验错 / 1 运行期失败";

/// 解析出来的选项。**用显式字段而不是一张 HashMap** ——
/// 拼错的名字要在解析阶段就变成错误，而不是到用的时候才发现取不到。
#[derive(Debug, Default, PartialEq)]
struct Args {
    command: String,
    project: Option<String>,
    asset: Option<String>,
    out: Option<String>,
    asset_root: Option<String>,
    asset_map: Option<String>,
    file: Option<String>,
    id: Option<String>,
    op: Option<String>,
    write: bool,
    replace: bool,
    from: Option<i64>,
    to: Option<i64>,
    frame: Option<i64>,
    width: Option<u32>,
    height: Option<u32>,
    help: bool,
}

/// 认得的**带值**选项。不在表里的一律报错。
const KNOWN_VALUE_FLAGS: [&str; 12] = [
    "--project", "--asset", "--out", "--asset-root", "--asset-map", "--file", "--id", "--op",
    "--from", "--to", "--width", "--height",
];
/// 认得的**不带值**选项。
const KNOWN_FLAGS: [&str; 5] = ["--frame", "--write", "--replace", "-h", "--help"];
/// 认得的子命令。
const COMMANDS: [&str; 8] =
    ["probe", "info", "gop", "frame", "render", "import", "library", "edit"];

/// 解析。**纯函数**，所以能脱离命令行单测。
fn parse(argv: &[String]) -> Result<Args, String> {
    let mut args = Args::default();
    let mut index = 0;
    while index < argv.len() {
        let token = argv[index].as_str();
        if token == "-h" || token == "--help" {
            args.help = true;
            index += 1;
            continue;
        }
        if token == "--write" || token == "--replace" {
            if token == "--write" {
                args.write = true;
            } else {
                args.replace = true;
            }
            index += 1;
            continue;
        }
        if !token.starts_with('-') {
            if args.command.is_empty() {
                if !COMMANDS.contains(&token) {
                    return Err(format!("不认识的子命令：{token}"));
                }
                args.command = token.to_string();
                index += 1;
                continue;
            }
            return Err(format!("多余的位置参数：{token}"));
        }
        if !KNOWN_VALUE_FLAGS.contains(&token) && !KNOWN_FLAGS.contains(&token) {
            return Err(format!("不认识的选项：{token}"));
        }
        let value = argv
            .get(index + 1)
            .ok_or_else(|| format!("{token} 后面要跟一个值"))?
            .clone();
        match token {
            "--project" => args.project = Some(value),
            "--asset" => args.asset = Some(value),
            "--out" => args.out = Some(value),
            "--asset-root" => args.asset_root = Some(value),
            "--asset-map" => args.asset_map = Some(value),
            "--file" => args.file = Some(value),
            "--id" => args.id = Some(value),
            "--op" => args.op = Some(value),
            "--from" => args.from = Some(parse_int(&token, &value)?),
            "--to" => args.to = Some(parse_int(&token, &value)?),
            "--frame" => args.frame = Some(parse_int(&token, &value)?),
            "--width" => args.width = Some(parse_uint(&token, &value)?),
            "--height" => args.height = Some(parse_uint(&token, &value)?),
            other => return Err(format!("不认识的选项：{other}")),
        }
        index += 2;
    }
    Ok(args)
}

fn parse_int(name: &str, value: &str) -> Result<i64, String> {
    value.parse().map_err(|_| format!("{name} 要一个整数，得到 {value}"))
}

fn parse_uint(name: &str, value: &str) -> Result<u32, String> {
    let parsed: u32 = value
        .parse()
        .map_err(|_| format!("{name} 要一个非负整数，得到 {value}"))?;
    if parsed == 0 {
        return Err(format!("{name} 不能是 0"));
    }
    Ok(parsed)
}

// ---------------------------------------------------------------------------
// ffprobe 的两件事：流信息与包列表
// ---------------------------------------------------------------------------

fn ffprobe_json(args: &[&str], file: &Path) -> Result<serde_json::Value, String> {
    let output = Command::new("ffprobe")
        .args(args)
        .arg(file)
        .output()
        .map_err(|error| format!("起不了 ffprobe：{error}（PATH 里有 ffprobe 吗？）"))?;
    if !output.status.success() {
        return Err(format!(
            "ffprobe 读不了 {}：{}",
            file.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("ffprobe 的输出不是 JSON：{error}"))
}

/// 解析 ffprobe 给的有理数字符串（如 30/1）。
fn parse_rate(text: &str) -> Option<(u32, u32)> {
    let mut parts = text.split('/');
    let num: u32 = parts.next()?.trim().parse().ok()?;
    let den: u32 = parts.next().unwrap_or("1").trim().parse().ok()?;
    if den == 0 {
        return None;
    }
    Some((num, den))
}

/// 从 ffprobe 的一个字段里取文本。
///
/// **它给的 JSON 里类型是混的**：pos/size/flags 是字符串，dts/duration 是数字。
/// 只认字符串的话，duration 会被静默当成 0 —— 那正是"错得很安静"的那一类。
fn field_text(item: &serde_json::Value, name: &str) -> Option<String> {
    match item.get(name)? {
        serde_json::Value::String(text) => Some(text.clone()),
        serde_json::Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

/// 包列表 -> SampleView 列表。返回的第二个值是**被减掉的时间戳原点**。
///
/// # 为什么要归零
///
/// 契约里的 SampleView.dts 是 **u64**，而真实 MP4 的 dts **可以是负的** ——
/// 带 B 帧或 edit list 的片子第一个包的 dts 就是负的（实测
/// target/s3/proxy1080p.mp4 是 -512）。
///
/// 三条路里只有一条是对的：
///
/// * 报错 —— 于是 info/gop 在**真实素材上从来跑不通**（第一版就是这么写的，实测踩到）；
/// * 夹成 0 —— 多个包会撞到同一个时间戳，而且"时间戳从 0 开始"变成一句没说的谎；
/// * **减去首包 dts 并把这个偏移报出去** —— 时间戳变成相对量，且偏移是可见的。
///
/// 选第三条。它不影响 GOP 切分（那只看 is_sync 与字节范围），
/// 而调用方若需要绝对时间戳，拿 origin 就能还原。
///
/// 键名是**单字母**（o/s/d/u/k），那是契约层定下的，别改 ——
/// 这里只是把 ffprobe 的字段搬进去，不重新发明一套。
pub fn samples_from_packets(
    packets: &serde_json::Value,
) -> Result<(Vec<SampleView>, i64), String> {
    let list = packets
        .get("packets")
        .and_then(serde_json::Value::as_array)
        .ok_or("ffprobe 的输出里没有 packets 数组")?;
    let mut raw = Vec::with_capacity(list.len());
    for (index, packet) in list.iter().enumerate() {
        let offset: usize = field_text(packet, "pos")
            .ok_or_else(|| format!("第 {index} 个包没有 pos"))?
            .parse()
            .map_err(|_| format!("第 {index} 个包的 pos 不是数"))?;
        let size: usize = field_text(packet, "size")
            .ok_or_else(|| format!("第 {index} 个包没有 size"))?
            .parse()
            .map_err(|_| format!("第 {index} 个包的 size 不是数"))?;
        let dts_text = field_text(packet, "dts")
            .ok_or_else(|| format!("第 {index} 个包没有 dts"))?;
        let dts: i64 = dts_text
            .parse()
            .map_err(|_| format!("第 {index} 个包的 dts 不是数：{dts_text}"))?;
        let duration: u32 = field_text(packet, "duration")
            .unwrap_or_else(|| "0".to_string())
            .parse()
            .map_err(|_| format!("第 {index} 个包的 duration 不是数"))?;
        let flags = field_text(packet, "flags").unwrap_or_default();
        raw.push((offset, size, dts, duration, flags.contains('K')));
    }
    // 原点取所有包 dts 的最小值（不只是首包：edit list 下首包未必最小）。
    let origin = raw.iter().map(|row| row.2).min().unwrap_or(0);
    let samples = raw
        .into_iter()
        .map(|(offset, size, dts, duration, is_sync)| SampleView {
            offset,
            size,
            // origin <= 每个 dts，所以这个减法不会出负数。
            dts: dts.saturating_sub(origin) as u64,
            duration,
            is_sync,
        })
        .collect();
    Ok((samples, origin))
}

const PACKET_ARGS: [&str; 9] = [
    "-v", "error",
    "-select_streams", "v:0",
    "-show_packets",
    "-show_entries", "packet=pos,size,dts,duration,flags",
    "-of", "json",
];

/// 素材信息。**形状就是契约里的 AssetInfoView**，不另造一套。
fn asset_info(file: &Path) -> Result<AssetInfoView, String> {
    let stream = ffprobe_json(
        &[
            "-v", "error",
            "-select_streams", "v:0",
            "-count_frames",
            "-show_entries", "stream=nb_read_frames,width,height,avg_frame_rate",
            "-of", "json",
        ],
        file,
    )?;
    let stream = stream
        .get("streams")
        .and_then(serde_json::Value::as_array)
        .and_then(|list| list.first())
        .ok_or_else(|| format!("{} 里没有视频流", file.display()))?;
    // **同样要按混合类型读**：实测 ffprobe 的 width/height/nb_read_frames 是字符串，
    // 但不同版本/不同格式下可能是数字。只认一种就会静默给出 0 —— 上面那个
    // "width: 0" 就是这么来的，它是**错的**而不是"未知"。
    let text_field = |name: &str| field_text(stream, name);
    let width: u32 = text_field("width")
        .and_then(|text| text.parse().ok())
        .ok_or_else(|| "ffprobe 没给出宽度".to_string())?;
    let height: u32 = text_field("height")
        .and_then(|text| text.parse().ok())
        .ok_or_else(|| "ffprobe 没给出高度".to_string())?;
    let frame_count: i64 = text_field("nb_read_frames")
        .and_then(|text| text.parse().ok())
        .ok_or_else(|| "ffprobe 没给出帧数（-count_frames 没生效？）".to_string())?;
    let (num, den) = text_field("avg_frame_rate")
        .and_then(|text| parse_rate(&text))
        .ok_or_else(|| "ffprobe 没给出可解析的帧率".to_string())?;

    let packets = ffprobe_json(&PACKET_ARGS, file)?;
    let (samples, dts_origin) = samples_from_packets(&packets)?;
    if dts_origin != 0 {
        // 归零是**可见的**：不写出来就成了"时间戳从 0 开始"这句没说的谎。
        eprintln!("时间戳已按首包归零（原 dts 最小值 {dts_origin}，流时间基内）。");
    }
    let slices = gop_slices(&samples);
    // GOP 长度**由切片实测**，不写死也不猜：只有至少两段时才谈得上"间隔"。
    // 0 表示未知 —— 契约里明说此时不能按 GOP 切。
    let gop_length = if slices.len() >= 2 {
        u32::try_from(slices[0].sample_count).unwrap_or(0)
    } else {
        0
    };

    Ok(AssetInfoView {
        id: file
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_default(),
        kind: "video".to_string(),
        frame_count,
        timebase: TimebaseDto { num, den },
        width,
        height,
        gop_length,
        // 本 CLI 不做代理生成 —— 如实说没有，而不是给一个好看的 true。
        proxy_available: false,
    })
}

// ---------------------------------------------------------------------------
// 工程
// ---------------------------------------------------------------------------

fn read_doc(path: &str) -> Result<ProjectDoc, String> {
    let text =
        std::fs::read_to_string(path).map_err(|error| format!("读不了工程 {path}：{error}"))?;
    load_doc(&text).map_err(|error| format!("工程 {path} 载入失败：{error}"))
}

/// 载入工程；失败就打一行人话并**退 2**。
///
/// 文件不在、JSON 不合法、字段不符，都是**用户能改的**错 ——
/// 报成运行期失败（1）会让人以为"程序坏了"，而去查不该查的地方。
fn load_project_or_usage(path: &str) -> Result<ProjectDoc, ExitCode> {
    match read_doc(path) {
        Ok(doc) => Ok(doc),
        Err(error) => {
            eprintln!("{error}");
            Err(ExitCode::from(2))
        }
    }
}

/// 读兜底资产登记表。形状与 fixtures/local-assets.json 一致：
/// {"assets":{"<id>":{"file":"...","kind":"..."}}}，file 相对 asset_root 解析。
pub fn load_asset_map(path: &str, asset_root: &Path) -> Result<Vec<(String, PathBuf)>, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("读不了兜底登记表 {path}：{error}"))?;
    let value: serde_json::Value =
        serde_json::from_str(&text).map_err(|error| format!("兜底登记表不是 JSON：{error}"))?;
    let table = value
        .get("assets")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| format!("兜底登记表 {path} 里没有 assets 对象"))?;
    let mut rows = Vec::new();
    for (id, entry) in table {
        let Some(file) = entry.get("file").and_then(serde_json::Value::as_str) else {
            continue;
        };
        if file.is_empty() {
            continue;
        }
        let raw = Path::new(file);
        rows.push((
            id.clone(),
            if raw.is_absolute() { raw.to_path_buf() } else { asset_root.join(raw) },
        ));
    }
    Ok(rows)
}

/// 素材表：id -> 文件。**位置由宿主解释**，所以相对 uri 要挂到 --asset-root 上。
///
/// 优先级：**工程文件的 assets 先来，兜底表只补缺**。
/// 反过来的话，兜底表会悄悄盖掉工程文件里写的真实位置，而用户看不到。
fn build_sources(doc: &ProjectDoc, asset_root: &Path, fallback: &[(String, PathBuf)]) -> SourceTable {
    let mut table = SourceTable::new();
    for asset in &doc.assets {
        if asset.uri.is_empty() {
            continue;
        }
        let raw = Path::new(&asset.uri);
        let file = if raw.is_absolute() {
            raw.to_path_buf()
        } else {
            asset_root.join(raw)
        };
        table.insert(asset.id.clone(), file);
    }
    for (id, file) in fallback {
        if table.file_for(id).is_none() {
            table.insert(id.clone(), file.clone());
        }
    }
    table
}

/// 解析兜底表；给了路径但读不了就**报错而不是忽略** ——
/// 静默忽略会让"兜底没生效"和"兜底不需要"看起来一样。
fn resolve_fallback(args: &Args, asset_root: &Path) -> Result<Vec<(String, PathBuf)>, String> {
    match args.asset_map.as_ref() {
        None => Ok(Vec::new()),
        Some(path) => load_asset_map(path, asset_root),
    }
}

/// 输出尺寸：命令行 > 工程文件里的 render_hints。
fn resolve_size(args: &Args, doc: &ProjectDoc) -> (u32, u32) {
    (
        args.width.unwrap_or(doc.render_hints.width),
        args.height.unwrap_or(doc.render_hints.height),
    )
}

/// 帧区间：没给就取整个工程（左闭右开换算成闭区间）。
fn resolve_range(args: &Args, doc: &ProjectDoc) -> Result<(i64, i64), String> {
    let first = compose::first_frame_v2(&doc.timeline).unwrap_or(0);
    let end = compose::end_frame_v2(&doc.timeline).unwrap_or(0);
    let from = args.from.unwrap_or(first);
    let to = args.to.unwrap_or(end.saturating_sub(1));
    if to < from {
        return Err(format!("帧区间是空的：from={from} to={to}"));
    }
    Ok((from, to))
}

/// 校验不通过就打印清单并返回退出码 2。**校验只有一份实现**，在契约层。
fn gate(doc: &ProjectDoc) -> Option<ExitCode> {
    let issues = validate_project_doc(doc, REGISTRY);
    if issues.is_ok() {
        return None;
    }
    eprintln!("工程没通过校验，先修它：");
    eprintln!(
        "{}",
        serde_json::to_string_pretty(&issues).unwrap_or_default()
    );
    Some(ExitCode::from(2))
}

fn print_json<T: serde::Serialize>(value: &T) -> Result<ExitCode, String> {
    println!(
        "{}",
        serde_json::to_string_pretty(value).map_err(|error| error.to_string())?
    );
    Ok(ExitCode::SUCCESS)
}

// ---------------------------------------------------------------------------
// 子命令
// ---------------------------------------------------------------------------

fn cmd_probe(args: &Args) -> Result<ExitCode, String> {
    let project = args.project.as_ref().ok_or("probe 要 --project <文件>")?;
    let doc = match load_project_or_usage(project) {
        Ok(doc) => doc,
        Err(code) => return Ok(code),
    };
    let issues = validate_project_doc(&doc, REGISTRY);
    // 校验有 error 就让退出码说话 —— 调用方不该去解析 JSON 才知道失败了。
    // 但 stdout 上仍然给完整清单：**退出码与内容是两件事**。
    let code = if issues.is_ok() { ExitCode::SUCCESS } else { ExitCode::from(2) };
    print_json(&issues)?;
    Ok(code)
}

fn cmd_info(args: &Args) -> Result<ExitCode, String> {
    let asset = args.asset.as_ref().ok_or("info 要 --asset <文件>")?;
    let info = asset_info(Path::new(asset))?;
    print_json(&info)
}

fn cmd_gop(args: &Args) -> Result<ExitCode, String> {
    let asset = args.asset.as_ref().ok_or("gop 要 --asset <文件>")?;
    let packets = ffprobe_json(&PACKET_ARGS, Path::new(asset))?;
    let (samples, dts_origin) = samples_from_packets(&packets)?;
    let slices = gop_slices(&samples);
    print_json(&serde_json::json!({
        "asset": asset,
        "samples": samples.len(),
        "sync_samples": samples.iter().filter(|sample| sample.is_sync).count(),
        // 样本表里的 dts 是**相对量**：这个偏移已经在每个 dts 上减掉了。
        // 写出来，需要绝对时间戳的调用方才能还原。
        "dts_origin": dts_origin,
        "slices": slices,
    }))
}

/// 这份工程里**会被出片忽略**的音轨 id。
///
/// 抽成纯函数有两个理由：能脱离 GPU 单测；以及"哪些东西没被渲染"这件事
/// 应该是**列出来的**，而不是一句笼统的"不支持音频"。
fn audio_track_ids(timeline: &dhampir_core::timeline::layer::TimelineV2) -> Vec<String> {
    timeline
        .tracks
        .iter()
        .filter(|track| track.kind == TrackKind::Audio)
        .map(|track| track.id.clone())
        .collect()
}

/// 帧区间取到的**工程量**，给调用方在真去解码之前知道要处理多少帧。
fn project_frames(doc: &ProjectDoc) -> usize {
    let end = compose::end_frame_v2(&doc.timeline).unwrap_or(0);
    let first = compose::first_frame_v2(&doc.timeline).unwrap_or(0);
    usize::try_from(end.saturating_sub(first).max(0)).unwrap_or(0)
}

fn cmd_frame(args: &Args) -> Result<ExitCode, String> {
    let project = args.project.as_ref().ok_or("frame 要 --project <文件>")?;
    let out = args.out.as_ref().ok_or("frame 要 --out <目录>")?;
    let frame = args.frame.ok_or("frame 要 --frame <N>")?;
    let doc = match load_project_or_usage(project) {
        Ok(doc) => doc,
        Err(code) => return Ok(code),
    };
    if let Some(code) = gate(&doc) {
        return Ok(code);
    }
    let (width, height) = resolve_size(args, &doc);
    let root = PathBuf::from(args.asset_root.clone().unwrap_or_else(|| "target/s3".to_string()));
    let sources = build_sources(&doc, &root, &resolve_fallback(args, &root)?);

    // 文件名固定按帧号，调用方给的是**目录** —— 这样同一帧重跑一定落在同一个路径上。
    let output = PathBuf::from(out).join("frame.png");
    // **把资产时间基带上。** 少了它就会退回恒等换算（素材帧率按时间线算），
    // 而 60fps 素材放进 30fps 工程的表现是**半速播放**。
    let asset_timebases = doc.asset_timebases();
    let plan = RenderPlan {
        timeline: &doc.timeline,
        sources: &sources,
        asset_timebases: &asset_timebases,
        from: frame,
        to: frame,
        width,
        height,
        sequence: doc.sequence_size(),
        output: &output,
    };
    let written = render_frames_png(&plan, &[frame])?;
    let (_, path, digest) = written.first().ok_or("一帧都没出")?;
    print_json(&serde_json::json!({
        "frame": frame,
        "path": path.display().to_string(),
        "digest": digest,
        "width": width,
        "height": height,
        "project_frames": project_frames(&doc),
    }))
}

fn cmd_render(args: &Args) -> Result<ExitCode, String> {
    let project = args.project.as_ref().ok_or("render 要 --project <文件>")?;
    let out = args.out.as_ref().ok_or("render 要 --out <文件.mp4>")?;
    let doc = match load_project_or_usage(project) {
        Ok(doc) => doc,
        Err(code) => return Ok(code),
    };
    if let Some(code) = gate(&doc) {
        return Ok(code);
    }
    // **音轨不渲染**：明说，不给一份「看起来很成功」的哑片。
    let audio_tracks = audio_track_ids(&doc.timeline);
    if !audio_tracks.is_empty() {
        eprintln!(
            "注意：本次出片**不渲染音频**（音轨：{}）。产物是无声的，这是当前实现的边界。",
            audio_tracks.join(", ")
        );
    }

    let (from, to) = resolve_range(args, &doc)?;
    let (width, height) = resolve_size(args, &doc);
    let root = PathBuf::from(args.asset_root.clone().unwrap_or_else(|| "target/s3".to_string()));
    let sources = build_sources(&doc, &root, &resolve_fallback(args, &root)?);
    let output = PathBuf::from(out);

    let total = (to - from + 1) as usize;
    println!(
        "{}",
        serde_json::json!({
            "event": "start",
            "from": from,
            "to": to,
            "total": total,
            "width": width,
            "height": height,
            "assets": sources.len(),
        })
    );

    let asset_timebases = doc.asset_timebases();
    let plan = RenderPlan {
        timeline: &doc.timeline,
        sources: &sources,
        asset_timebases: &asset_timebases,
        from,
        to,
        width,
        height,
        sequence: doc.sequence_size(),
        output: &output,
    };
    let report = render_plan(&plan, |done, total| {
        // 每帧一行，调用方自己决定要不要节流 —— 这里不替它做决定。
        println!("{}", serde_json::json!({"event": "progress", "done": done, "total": total}));
    })?;

    let failed = report.failed();
    println!(
        "{}",
        serde_json::json!({
            "event": "done",
            "output": report.output.display().to_string(),
            "frames": report.frames,
            "encoded_frames": report.encoded_frames,
            "width": report.width,
            "height": report.height,
            "fps": report.encoder_fps,
            "seconds": report.seconds,
            "elapsed_ms": report.elapsed_ms,
            "opened_streams": report.opened_streams,
            "empty_frames": report.empty_frames,
            "issues": report.issues,
            "failed": failed,
        })
    );
    if failed {
        eprintln!("出片报告判定为失败（问题清单或空帧不为空，或帧数对不上）—— 见 done 那一行。");
        return Ok(ExitCode::from(1));
    }
    Ok(ExitCode::SUCCESS)
}

// ---------------------------------------------------------------------------
// 素材库：导入与清点
// ---------------------------------------------------------------------------

/// 按扩展名判素材种类。
///
/// **判不出来就按 video 处理**：ffprobe 会立刻给出答案（拿不到视频流就报错），
/// 比在这里堆一张越来越长的扩展名表可靠。
fn infer_kind(path: &Path) -> AssetKind {
    let extension = path
        .extension()
        .and_then(|text| text.to_str())
        .map(|text| text.to_ascii_lowercase());
    match extension.as_deref() {
        Some("srt") | Some("ass") | Some("ssa") | Some("vtt") => AssetKind::Subtitle,
        Some("wav") | Some("mp3") | Some("aac") | Some("m4a") | Some("flac") => AssetKind::Audio,
        Some("png") | Some("jpg") | Some("jpeg") | Some("webp") | Some("bmp") => AssetKind::Image,
        _ => AssetKind::Video,
    }
}

/// 把文件路径写成 uri：**在资产根下面就用相对路径，否则原样写**。
///
/// 不硬塞一个假的相对路径：那是把"这个素材不归我管"这件事藏起来，
/// 而藏起来的结果是换台机器就找不到素材、且没人知道为什么。
fn relativize_uri(path: &Path, asset_root: &Path) -> String {
    let normalized = |value: &Path| value.to_string_lossy().replace('\\', "/");
    let file = normalized(path);
    let root = normalized(asset_root);
    let root = root.trim_end_matches('/');
    let prefix = format!("{root}/");
    match file.strip_prefix(&prefix) {
        Some(rest) => rest.to_string(),
        None => file,
    }
}

fn cmd_import(args: &Args) -> Result<ExitCode, String> {
    let project = args.project.as_ref().ok_or("import 要 --project <文件>")?;
    let file = args.file.as_ref().ok_or("import 要 --file <素材>")?;
    let path = Path::new(file);
    if !path.exists() {
        eprintln!("文件不在：{file}");
        return Ok(ExitCode::from(2));
    }
    let asset_root =
        PathBuf::from(args.asset_root.clone().unwrap_or_else(|| "target/s3".to_string()));
    let id = match args.id.clone() {
        Some(explicit) => explicit,
        None => path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .ok_or("这个路径没有文件名，请用 --id 指定")?,
    };
    let kind = infer_kind(path);
    let mut built = Asset {
        id: id.clone(),
        kind,
        name: id.clone(),
        uri: relativize_uri(path, &asset_root),
        frame_count: None,
        timebase: None,
        width: None,
        height: None,
        content_hash: None,
        tags: std::collections::BTreeMap::new(),
        note: String::new(),
    };
    // 只有视频流才有尺寸与帧数 —— 字幕/音频/图片问了也是白问。
    if kind == AssetKind::Video {
        let info = asset_info(path)?;
        built.frame_count = Some(info.frame_count);
        built.timebase = Some(info.timebase.clone());
        built.width = Some(info.width);
        built.height = Some(info.height);
    }

    let mut doc = match load_project_or_usage(project) {
        Ok(doc) => doc,
        Err(code) => return Ok(code),
    };
    match doc.assets.iter().position(|asset| asset.id == id) {
        Some(index) => {
            if !args.replace {
                eprintln!("资产 id 已存在：{id}（要覆盖就加 --replace）");
                return Ok(ExitCode::from(2));
            }
            doc.assets[index] = built.clone();
        }
        None => doc.assets.push(built.clone()),
    }

    let issues = validate_project_doc(&doc, REGISTRY);
    if args.write {
        // 写回：pretty + 结尾换行。**migrated_from 是 serde(skip) 的，不会进文件。**
        let text = serde_json::to_string_pretty(&doc).map_err(|error| error.to_string())?;
        std::fs::write(project, format!("{text}\n"))
            .map_err(|error| format!("写不回工程 {project}：{error}"))?;
    }
    print_json(&serde_json::json!({
        "asset": built,
        "written": args.write,
        "asset_count": doc.assets.len(),
        "issues": issues,
    }))?;
    Ok(if issues.is_ok() { ExitCode::SUCCESS } else { ExitCode::from(2) })
}

fn cmd_library(args: &Args) -> Result<ExitCode, String> {
    let project = args.project.as_ref().ok_or("library 要 --project <文件>")?;
    let doc = match load_project_or_usage(project) {
        Ok(doc) => doc,
        Err(code) => return Ok(code),
    };
    let counts = asset_reference_counts(&doc);
    let mut rows = Vec::with_capacity(doc.assets.len());
    let mut unused = Vec::new();
    for (index, asset) in doc.assets.iter().enumerate() {
        let references = counts.get(&asset.id).copied().unwrap_or(0);
        if references == 0 && !asset.id.is_empty() {
            unused.push(asset.id.clone());
        }
        rows.push(serde_json::json!({
            "index": index,
            "id": asset.id,
            "kind": asset.kind,
            "uri": asset.uri,
            "frame_count": asset.frame_count,
            "timebase": asset.timebase,
            "width": asset.width,
            "height": asset.height,
            "references": references,
        }));
    }
    print_json(&serde_json::json!({
        "total": doc.assets.len(),
        "used": doc.assets.len() - unused.len(),
        // **未被引用的列出来**：它们是 unused_asset 警告的来源，
        // 而"库里有 12 条"和"库里 12 条里有 3 条没人用"是两件事。
        "unused": unused,
        "assets": rows,
    }))
}

fn cmd_edit(args: &Args) -> Result<ExitCode, String> {
    let project = args.project.as_ref().ok_or("edit 要 --project <文件>")?;
    let op_text = args.op.as_ref().ok_or("edit 要 --op <JSON>")?;
    let doc = match load_project_or_usage(project) {
        Ok(doc) => doc,
        Err(code) => return Ok(code),
    };
    let op: EditOp = match serde_json::from_str(op_text) {
        Ok(op) => op,
        Err(error) => {
            eprintln!("--op 不是合法的编辑操作：{error}");
            return Ok(ExitCode::from(2));
        }
    };
    let outcome = apply_edit(&doc, REGISTRY, &op);
    if args.write && outcome.is_ok() {
        let text = serde_json::to_string_pretty(&outcome.doc).map_err(|error| error.to_string())?;
        std::fs::write(project, format!("{text}\n"))
            .map_err(|error| format!("写不回工程 {project}：{error}"))?;
    }
    print_json(&serde_json::json!({
        "ok": outcome.is_ok(),
        "summary": outcome.summary,
        "written": args.write && outcome.is_ok(),
        "issues": outcome.issues,
    }))?;
    Ok(if outcome.is_ok() { ExitCode::SUCCESS } else { ExitCode::from(2) })
}

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let args = match parse(&argv) {
        Ok(args) => args,
        Err(error) => {
            eprintln!("{error}");
            eprintln!();
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    if args.help || args.command.is_empty() {
        println!("{USAGE}");
        // 没给子命令**不是**错误：dhampir 与 dhampir --help 一样是"问怎么用"。
        return ExitCode::SUCCESS;
    }
    let result = match args.command.as_str() {
        "probe" => cmd_probe(&args),
        "info" => cmd_info(&args),
        "gop" => cmd_gop(&args),
        "frame" => cmd_frame(&args),
        "render" => cmd_render(&args),
        "import" => cmd_import(&args),
        "library" => cmd_library(&args),
        "edit" => cmd_edit(&args),
        other => Err(format!("不认识的子命令：{other}")),
    };
    match result {
        Ok(code) => code,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::from(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| item.to_string()).collect()
    }

    #[test]
    fn 子命令与选项能被解析出来() {
        let args = parse(&argv(&[
            "render", "--project", "p.json", "--from", "0", "--to", "9", "--out", "o.mp4",
        ]))
        .expect("合法");
        assert_eq!(args.command, "render");
        assert_eq!(args.project.as_deref(), Some("p.json"));
        assert_eq!(args.from, Some(0));
        assert_eq!(args.to, Some(9));
        assert_eq!(args.out.as_deref(), Some("o.mp4"));
    }

    #[test]
    fn 不认识的选项要报错而不是静默丢掉() {
        // 这条正是「参数打错字被静默忽略」的防线。
        let error = parse(&argv(&["render", "--wdith", "640"])).expect_err("应当报错");
        assert!(error.contains("--wdith"), "{error}");
    }

    #[test]
    fn 不认识的子命令要报错() {
        let error = parse(&argv(&["transcode"])).expect_err("应当报错");
        assert!(error.contains("transcode"), "{error}");
    }

    #[test]
    fn 选项少给值要报错() {
        let error = parse(&argv(&["probe", "--project"])).expect_err("应当报错");
        assert!(error.contains("要跟一个值"), "{error}");
    }

    #[test]
    fn 多给一个位置参数要报错() {
        let error = parse(&argv(&["probe", "extra.json"])).expect_err("应当报错");
        assert!(error.contains("多余的位置参数"), "{error}");
    }

    #[test]
    fn 带值选项表里每一条都真的被处理() {
        // 表里列了却不处理的选项会落到 other 分支报错 —— 而它已经在表里，
        // 说明有人加了选项却没写分支。这条把它变成红。
        for flag in KNOWN_VALUE_FLAGS {
            let parsed = parse(&argv(&["probe", flag, "1"]))
                .unwrap_or_else(|error| panic!("表里的选项 {flag} 解析不过：{error}"));
            assert_eq!(parsed.command, "probe");
        }
    }

    #[test]
    fn 宽度为零要被拒() {
        let error = parse(&argv(&["render", "--width", "0"])).expect_err("应当报错");
        assert!(error.contains("不能是 0"), "{error}");
    }

    #[test]
    fn 帮助与空参数都算成功路径() {
        let args = parse(&argv(&["--help"])).expect("合法");
        assert!(args.help);
        let args = parse(&[]).expect("合法");
        assert!(args.command.is_empty());
    }

    #[test]
    fn 有理数帧率解析() {
        assert_eq!(parse_rate("30/1"), Some((30, 1)));
        assert_eq!(parse_rate("30000/1001"), Some((30000, 1001)));
        // 单独一个数按 den=1 处理（ffprobe 偶尔这么给）。
        assert_eq!(parse_rate("25"), Some((25, 1)));
        // 分母为 0 不是「无穷帧率」，是坏数据。
        assert_eq!(parse_rate("30/0"), None);
        assert_eq!(parse_rate(""), None);
    }

    #[test]
    fn 包列表解析出样本表并认出关键帧() {
        let packets = serde_json::json!({
            "packets": [
                {"pos": "48", "size": "100", "dts": "0", "duration": "512", "flags": "K__"},
                {"pos": "148", "size": "90", "dts": "512", "duration": "512", "flags": "___"},
            ]
        });
        let (samples, origin) = samples_from_packets(&packets).expect("能解析");
        assert_eq!(samples.len(), 2);
        assert_eq!(origin, 0);
        assert!(samples[0].is_sync);
        assert!(!samples[1].is_sync);
        assert_eq!(samples[1].offset, 148);
        assert_eq!(samples[1].dts, 512);
    }

    #[test]
    fn 字段类型混着来也要读对() {
        // ffprobe 实测：pos/size/flags 是字符串，dts/duration 是数字。
        // 只认字符串的话 duration 会被静默当成 0 —— 那正是"错得很安静"的那一类。
        let packets = serde_json::json!({
            "packets": [{"pos": "48", "size": 33751, "dts": 0, "duration": 256, "flags": "K__"}]
        });
        let (samples, _) = samples_from_packets(&packets).expect("能解析");
        assert_eq!(samples[0].size, 33751);
        assert_eq!(samples[0].duration, 256);
        assert_eq!(samples[0].offset, 48);
    }

    #[test]
    fn 负的_dts_按最小值归零并把偏移报出来() {
        // 真实素材（target/s3/proxy1080p.mp4）第一个包的 dts 就是 -512。
        // 报错 -> info/gop 在真实素材上从来跑不通；夹成 0 -> 多个包撞同一个时间戳。
        let packets = serde_json::json!({
            "packets": [
                {"pos": "48", "size": "33751", "dts": -512, "duration": 256, "flags": "K__"},
                {"pos": "33799", "size": "21119", "dts": -256, "duration": 256, "flags": "___"},
                {"pos": "54918", "size": "12530", "dts": 0, "duration": 256, "flags": "___"},
            ]
        });
        let (samples, origin) = samples_from_packets(&packets).expect("能解析");
        assert_eq!(origin, -512);
        assert_eq!(samples.iter().map(|s| s.dts).collect::<Vec<_>>(), vec![0, 256, 512]);
    }

    #[test]
    fn 素材表把相对_uri_挂到_asset_root_上() {
        let doc = load_doc(
            r#"{"project_schema":1,"timeline":{"schema":2,"timebase":{"num":30,"den":1},"tracks":[]},
                "assets":[{"id":"a.mp4","kind":"video","uri":"proxy.mp4"},
                          {"id":"b.mp4","kind":"video","uri":"C:/abs/b.mp4"},
                          {"id":"c.mp4","kind":"video","uri":""}]}"#,
        )
        .expect("能载入");
        let table = build_sources(&doc, Path::new("target/s3"), &[]);
        // 相对 uri 挂到根上。
        assert_eq!(
            table.file_for("a.mp4").map(|path| path.to_string_lossy().replace('\\', "/")),
            Some("target/s3/proxy.mp4".to_string())
        );
        // 绝对 uri 原样。
        assert_eq!(
            table.file_for("b.mp4").map(|path| path.to_string_lossy().replace('\\', "/")),
            Some("C:/abs/b.mp4".to_string())
        );
        // uri 为空的不进表 —— 位置未知就不假装知道。
        assert!(table.file_for("c.mp4").is_none());
    }

    #[test]
    fn 兜底登记表只补工程文件没登记的_id() {
        let doc = load_doc(
            r#"{"project_schema":1,"timeline":{"schema":2,"timebase":{"num":30,"den":1},"tracks":[]},
                "assets":[{"id":"a.mp4","kind":"video","uri":"real.mp4"}]}"#,
        )
        .expect("能载入");
        let fallback = vec![
            // 同一个 id：工程文件里的位置必须赢。
            ("a.mp4".to_string(), PathBuf::from("target/s3/wrong.mp4")),
            ("b.mp4".to_string(), PathBuf::from("target/s3/fallback.mp4")),
        ];
        let table = build_sources(&doc, Path::new("target/s3"), &fallback);
        assert_eq!(
            table.file_for("a.mp4").map(|p| p.to_string_lossy().replace('\\', "/")),
            Some("target/s3/real.mp4".to_string())
        );
        assert_eq!(
            table.file_for("b.mp4").map(|p| p.to_string_lossy().replace('\\', "/")),
            Some("target/s3/fallback.mp4".to_string())
        );
    }

    #[test]
    fn 输出尺寸缺省取工程文件的渲染提示() {
        let doc = load_doc(
            r#"{"project_schema":1,"timeline":{"schema":2,"timebase":{"num":30,"den":1},"tracks":[]},
                "render_hints":{"width":1280,"height":720,"format":"mp4"}}"#,
        )
        .expect("能载入");
        let empty = Args::default();
        assert_eq!(resolve_size(&empty, &doc), (1280, 720));
        let explicit = Args { width: Some(640), ..Args::default() };
        // 命令行只给了一个：另一个仍取提示值，不强行配对。
        assert_eq!(resolve_size(&explicit, &doc), (640, 720));
    }

    #[test]
    fn 音轨要被列出来而不是被笼统地说成不支持() {
        let doc = load_doc(
            r#"{"project_schema":1,"timeline":{"schema":2,"timebase":{"num":30,"den":1},
                "tracks":[{"id":"v1","kind":"video","layers":[]},
                          {"id":"a1","kind":"audio","layers":[]},
                          {"id":"a2","kind":"audio","layers":[]}]}}"#,
        )
        .expect("能载入");
        assert_eq!(audio_track_ids(&doc.timeline), vec!["a1".to_string(), "a2".to_string()]);
    }

    #[test]
    fn 空时间线的帧区间是空的() {
        let doc = load_doc(
            r#"{"project_schema":1,"timeline":{"schema":2,"timebase":{"num":30,"den":1},"tracks":[]}}"#,
        )
        .expect("能载入");
        // 空工程 -> end=0 -> to = -1 < from = 0，要报错而不是"出 0 帧"。
        assert!(resolve_range(&Args::default(), &doc).is_err());
        // 显式给区间就照给。
        let args = Args { from: Some(0), to: Some(89), ..Args::default() };
        assert_eq!(resolve_range(&args, &doc).expect("合法"), (0, 89));
    }
}
