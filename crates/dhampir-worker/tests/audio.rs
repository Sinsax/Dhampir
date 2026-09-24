//! T6 的音频判定：**AudioPlan 摊得对**（纯，随时可跑）+ **音轨真的接上去了**（真机）。
//!
//! # 这一条为什么不走出片那条路
//!
//! 出片要 GPU，而且视频编码器吃的是**裸帧 stdin 管道**（`pipeline.rs` 的 `encoder_args`
//! 里那条：`-f` + 裸像素格式 + `-i -`）——
//! 在本机的 agent 会话里给子进程开 stdin 管道会 `ERROR_PIPE_BUSY`（`os error 231`），
//! 于是 `render` 那条路在这里跑不起来（原结论见 `plan/next-steps.md` 坑 19）。
//!
//! 【措辞是刻意的】上面**没有**写那个 ffmpeg 格式名：`check-sequential-decode.mjs`
//! 是按**子串**扫源码的，只要文件里出现那个词，它就要求同一文件里也出现
//! `out_color_matrix`。那条判据针对的是「把视频解成 RGB 却不声明色彩矩阵」，
//! 而这个文件根本不解视频（它只给码流算哈希、给音频解采样点），
//! 所以在这里补一句色彩矩阵是**假声明**。改回那个词会让守卫变红，而红的不是真问题。
//!
//! 但音频这一跳**本来就不需要 stdin 管道**：它读素材文件（stdout 管道，通的）、
//! 写临时文件、再与已经编好的视频复用。所以它可以被**完整地**验：
//!
//!     造一份视频（lavfi，无 stdin 管道）+ 造一段音频
//!       -> 产品自己的 plan_audio / build_audio_track / mux_audio
//!       -> ffprobe 判产物
//!
//! **这一条证不了什么、也证得了什么，必须说清**：它证的是
//! 「音轨按 AudioPlan 拼出来了、时长与视频对得上、视频那一半没被动过」；
//! 它**证不了**「GPU 渲染出来的那 90 帧 + 音轨能出片」—— 那一半要在一个
//! 能起编码器的终端里跑 `node scripts/check-cli.mjs` 才算数。
//! 拿这一条冒充那一条，正是"看起来成功、其实没验"。
//!
//! 跑法：
//!
//!     cargo test -p dhampir-worker --test audio              # 只有纯的那条
//!     cargo test -p dhampir-worker --test audio -- --ignored # 连真机那条

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use dhampir_core::timeline::project::{ProjectDoc, load_doc};
use dhampir_worker::audio::{AUDIO_CHANNELS, AUDIO_SAMPLE_RATE, plan_audio};
use dhampir_worker::pipeline::{SourceTable, build_audio_track, mux_audio};

/// 时间线：30fps、[0, 89] 共 90 帧 = 3 秒。
const FROM: i64 = 0;
const TO: i64 = 89;
/// 3 秒 @ 48kHz。
const EXPECTED_SAMPLES: i64 = 144_000;
/// 一帧是多少秒（30fps）—— "时长与视频一致（±1 帧）"里的那个一帧。
const ONE_FRAME_SECONDS: f64 = 1.0 / 30.0;

/// 仓库根：`CARGO_MANIFEST_DIR` 指向 `crates/dhampir-worker`。
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/dhampir-worker 的上面两层就是仓库根")
        .to_path_buf()
}

fn fixture(name: &str) -> ProjectDoc {
    let path = repo_root().join("fixtures").join(name);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("读不了夹具 {}：{error}", path.display()));
    load_doc(&text).unwrap_or_else(|error| panic!("夹具 {} 解析失败：{error}", path.display()))
}

/// 跑的是一趟**没有音频**的区间：只用来对照"没有音轨时计划是空的"。
fn sources_with(video: &Path, audio: &Path) -> SourceTable {
    let mut table = SourceTable::new();
    table.insert("a.mp4", video.to_path_buf());
    table.insert("tone.m4a", audio.to_path_buf());
    table
}

// ---------------------------------------------------------------------------
// 纯的那一条：计划是从**真工程文件**摊出来的
// ---------------------------------------------------------------------------

#[test]
fn 真工程文件摊出来的音频计划与手算一致() {
    let doc = fixture("audio-project.doc.json");
    // 素材位置在这一条里不重要（plan_audio 只记录它），但必须给全 ——
    // 缺一个就会变成 `audio_source_missing`，而那正是另一条用例要验的。
    let sources = sources_with(
        &repo_root().join("target/s3/proxy1080p.mp4"),
        &repo_root().join("target/s3/tone.m4a"),
    );
    let plan =
        plan_audio(&doc.timeline, &sources, &doc.asset_timebases(), FROM, TO).expect("计划要摊得出来");

    assert!(plan.issues.is_empty(), "夹具本身不该有问题：{:?}", plan.issues);
    assert_eq!(plan.info.sample_rate, AUDIO_SAMPLE_RATE);
    assert_eq!(plan.info.channels, AUDIO_CHANNELS);
    // 总长由**帧区间**算出，与有没有音频段无关 —— 这就是"时长与视频一致"的来处。
    assert_eq!(plan.total_samples, EXPECTED_SAMPLES);
    assert_eq!(plan.start_sample, 0);

    assert_eq!(plan.segments.len(), 2, "夹具里是两段音轨");
    let bed = &plan.segments[0];
    assert_eq!((bed.timeline_start, bed.timeline_end), (0, 30));
    assert_eq!(bed.output_start_sample, 0);
    assert_eq!(bed.output_samples, 48_000, "[0, 30) 共 30 帧 = 1 秒");
    assert_eq!(bed.source_start_sample, 0, "第一段从素材第 0 秒起");

    let tag = &plan.segments[1];
    assert_eq!((tag.timeline_start, tag.timeline_end), (45, 90));
    assert_eq!(tag.output_start_sample, 72_000, "第 45 帧 = 72000 个采样点");
    assert_eq!(tag.output_samples, 72_000, "[45, 90) 共 45 帧 = 1.5 秒");
    assert_eq!(
        tag.source_start_sample, 48_000,
        "第二段 source_in=30 帧 = 素材第 1 秒"
    );

    // 空档只有 [30, 45) 那 15 帧。
    assert_eq!(plan.gap_samples(), 24_000);
    assert_eq!(plan.distinct_assets(), 1, "两段用的是同一份素材");
}

#[test]
fn 音轨与视频用的是同一条选片规则() {
    // 这一条盯着"同源求值"这句话里最容易分叉的那半句：
    // 同一份工程，视频取自 v1 轨、音频取自 a1 轨，两边的**时间线坐标必须对得上**。
    let doc = fixture("audio-project.doc.json");
    let sources = sources_with(
        &repo_root().join("target/s3/proxy1080p.mp4"),
        &repo_root().join("target/s3/tone.m4a"),
    );
    let plan = plan_audio(&doc.timeline, &sources, &doc.asset_timebases(), FROM, TO).unwrap();

    // 音轨第二段在时间线上是 [45, 90) —— 那正好是**视频轨那一帧区间的一段**，
    // 于是"第 45 帧这一帧"在两边的坐标是同一个 45。
    let tag = &plan.segments[1];
    assert_eq!(tag.timeline_start, 45);
    // 从输出轨的采样点倒推回帧号，必须回到 45（而不是 44 或 46）。
    let frame = tag.output_start_sample / (AUDIO_SAMPLE_RATE as i64 / 30);
    assert_eq!(frame, 45);
    // 素材侧同理：source_in=30 帧 @30fps 就是第 1 秒，换算回素材帧号仍是 30。
    assert_eq!(tag.source_start_sample / (AUDIO_SAMPLE_RATE as i64 / 30), 30);
}

// ---------------------------------------------------------------------------
// 真机那一条
// ---------------------------------------------------------------------------

const RUNNER: &str = "需要 PATH 上的 ffmpeg；跑：cargo test -p dhampir-worker --test audio -- --ignored";

/// 起一个**不给 stdin 开管道**的子进程。`Command::output()` 本来就把 stdin 置空，
/// 这里显式写出来是为了让这条约束在代码里看得见（坑 19）。
fn run(program: &str, args: &[&str]) -> String {
    let output = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .unwrap_or_else(|error| {
            panic!("起不了 {program}：{error}（{RUNNER}）");
        });
    assert!(
        output.status.success(),
        "{program} 退出码 {:?}：{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr).trim()
    );
    String::from_utf8_lossy(&output.stdout).to_string()
}

/// 一路**码流**的 SHA256（`-c copy` 之后交给 `-f hash`）。
///
/// 它是"这一路一个字节都没被动过"的判据：复用之后视频仍是同一串包，
/// 所以哈希必须逐字符相同 —— 比"帧数一样"强得多，也比"看着差不多"强得多。
fn stream_hash(path: &Path, map: &str) -> String {
    run(
        "ffmpeg",
        &[
            "-v",
            "error",
            "-i",
            &path.display().to_string(),
            "-map",
            map,
            "-c",
            "copy",
            "-f",
            "hash",
            "-hash",
            "sha256",
            "-",
        ],
    )
    .trim()
    .to_string()
}

fn stream_duration(path: &Path, map: &str) -> f64 {
    let text = run(
        "ffprobe",
        &[
            "-v",
            "error",
            "-select_streams",
            map,
            "-show_entries",
            "stream=duration",
            "-of",
            "csv=p=0",
            &path.display().to_string(),
        ],
    );
    text.trim()
        .parse()
        .unwrap_or_else(|_| panic!("{map} 的时长读不出来：{:?}", text.trim()))
}

fn frame_count(path: &Path) -> usize {
    let text = run(
        "ffprobe",
        &[
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-count_frames",
            "-show_entries",
            "stream=nb_read_frames",
            "-of",
            "csv=p=0",
            &path.display().to_string(),
        ],
    );
    text.trim().parse().expect("帧数不是数")
}

/// 把整段素材解成 `f32le`（与产品同一条口径，但**不裁剪**）。
///
/// 它是上面那些断言的**参考值**：产品裁出来的是什么，拿"整段解出来再在测试里切片"
/// 去比。两条路各自独立，所以这不是"拿产品验产品"。
fn decode_all(path: &Path) -> Vec<u8> {
    let output = Command::new("ffmpeg")
        .args(["-v", "error"])
        .arg("-i")
        .arg(path)
        .args([
            "-vn",
            "-f",
            "f32le",
            "-ac",
            &AUDIO_CHANNELS.to_string(),
            "-ar",
            &AUDIO_SAMPLE_RATE.to_string(),
            "-",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .unwrap_or_else(|error| panic!("解不了素材 {}：{error}（{RUNNER}）", path.display()));
    assert!(
        output.status.success(),
        "解素材 {} 失败：{}",
        path.display(),
        String::from_utf8_lossy(&output.stderr).trim()
    );
    output.stdout
}

/// 从 PCM 字节里按**采样点**切一段出来。
fn pcm_slice(bytes: &[u8], from_sample: i64, samples: i64, bytes_per_sample: usize) -> &[u8] {
    let start = from_sample as usize * bytes_per_sample;
    let end = start + samples as usize * bytes_per_sample;
    &bytes[start..end]
}

#[test]
#[ignore = "真机：要 PATH 上的 ffmpeg"]
fn 音轨接上去了而且时长与视频对得上() {
    let work = repo_root().join("target/t6");
    std::fs::create_dir_all(&work).expect("建不了 target/t6");

    // 1. 造素材。**两件都不经过 stdin 管道**，所以在本机会话里也跑得起来。
    let video = work.join("video.mp4");
    run(
        "ffmpeg",
        &[
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=320x180:rate=30:duration=3",
            "-c:v",
            "libx264",
            "-preset",
            "veryfast",
            "-pix_fmt",
            "yuv420p",
            "-y",
            &video.display().to_string(),
        ],
    );
    let tone = work.join("tone.m4a");
    run(
        "ffmpeg",
        &[
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000:duration=6",
            "-ac",
            "2",
            "-c:a",
            "aac",
            "-b:a",
            "192k",
            "-y",
            &tone.display().to_string(),
        ],
    );
    assert_eq!(frame_count(&video), 90, "造的这份视频要是 90 帧");

    // 2. 走**产品自己的**计划与组装，不是测试里另写一遍。
    let doc = fixture("audio-project.doc.json");
    let sources = sources_with(&video, &tone);
    let plan = plan_audio(&doc.timeline, &sources, &doc.asset_timebases(), FROM, TO).unwrap();
    assert_eq!(plan.total_samples, EXPECTED_SAMPLES);

    let pcm = work.join("audio.f32");
    let stats = build_audio_track(&plan, &pcm).expect("音轨要拼得出来");

    // 3. PCM 的长度是**算出来的**，不是素材有多长就是多长。
    assert_eq!(stats.expected_samples, EXPECTED_SAMPLES);
    assert_eq!(stats.gap_samples, 24_000, "[30, 45) 那 15 帧是空档");
    assert_eq!(
        stats.padded_samples, 0,
        "素材有 6 秒、只用 2.5 秒，不该有补的静音（这个数不为零就说明有内容被截断了）"
    );
    // 两段各 1 秒 / 1.5 秒，合起来正是 2.5 秒的素材 —— 一个采样点都不多不少。
    assert_eq!(
        stats.source_samples_read, 120_000,
        "素材侧读了 2.5 秒：空档不该去素材里拿东西"
    );
    let pcm_bytes = std::fs::read(&pcm).expect("临时 PCM 要读得回来");
    let size = pcm_bytes.len();
    assert_eq!(
        size,
        EXPECTED_SAMPLES as usize * AUDIO_CHANNELS as usize * 4,
        "PCM 必须正好是 total_samples × 声道 × 4 字节"
    );

    // 3b. **内容也要对**：长度对而位置错，是那种要靠耳朵才能发现的错。
    let bytes_per_sample = AUDIO_CHANNELS as usize * 4;
    let reference = decode_all(&tone);
    assert!(
        reference.iter().any(|byte| *byte != 0),
        "参考解码不该全是 0 —— 否则下面那几条是假绿（拿静音比静音当然相等）"
    );
    assert_eq!(
        pcm_slice(&pcm_bytes, 0, 48_000, bytes_per_sample),
        pcm_slice(&reference, 0, 48_000, bytes_per_sample),
        "第一段该是素材的第 0..1 秒（source_in=0）"
    );
    assert!(
        pcm_slice(&pcm_bytes, 48_000, 24_000, bytes_per_sample)
            .iter()
            .all(|byte| *byte == 0),
        "空档 [30, 45) 那 15 帧必须是**静音**，不是素材里的声音"
    );
    assert_eq!(
        pcm_slice(&pcm_bytes, 72_000, 72_000, bytes_per_sample),
        pcm_slice(&reference, 48_000, 72_000, bytes_per_sample),
        "第二段该是素材的第 1..2.5 秒（source_in=30 帧 = 1 秒）"
    );

    // 4. 复用。
    let out = work.join("out.mp4");
    mux_audio(&video, &pcm, &plan, &out).expect("复用要成功");

    // 5. 判产物：产物要有音轨、要能动、视频那一半要一个字节都没变。
    assert_eq!(frame_count(&out), 90, "复用之后视频帧数不能变");
    assert_eq!(
        stream_hash(&out, "0:v:0"),
        stream_hash(&video, "0:v:0"),
        "视频码流的哈希必须与复用前一模一样 —— 说明 -c:v copy 是照搬，没重编"
    );

    let video_seconds = stream_duration(&out, "v:0");
    let audio_seconds = stream_duration(&out, "a:0");
    let delta = (audio_seconds - video_seconds).abs();
    eprintln!(
        "视频 {video_seconds:.6} 秒 / 音轨 {audio_seconds:.6} 秒 / 差 {:.6} 秒（一帧 = {ONE_FRAME_SECONDS:.6} 秒）",
        delta
    );
    assert!(
        delta <= ONE_FRAME_SECONDS,
        "音轨与视频的时长差 {delta:.6} 秒，超过了一帧（{ONE_FRAME_SECONDS:.6} 秒）"
    );

    // 没有音轨的工程：计划是空的，于是**走的是无声那条路**（产物不带 a:0）。
    let doc_silent = fixture("four-asset-project.doc.json");
    let plan_silent = plan_audio(
        &doc_silent.timeline,
        &sources,
        &doc_silent.asset_timebases(),
        FROM,
        TO,
    )
    .unwrap();
    assert!(plan_silent.is_silent(), "没有音轨的工程不该摊出音频段");
    assert_eq!(
        plan_silent.total_samples, EXPECTED_SAMPLES,
        "但时长仍然算得出来（音轨有没有与片子多长是两个问题）"
    );
}
