//! 回退（D3）与同源多帧（D4）的真机验证：**换一种取帧方式，一个像素都不许变**。
//!
//! # 它补的是哪一半
//!
//! `pipeline` 里的池子规则是纯的，单元测试钉住了「谁该留、谁该被挤掉、什么时候重启」。
//! 但**规则对不等于像素对** —— 真起 GPU 与 ffmpeg 之后，「重启这一路解码器再读一遍」
//! 会不会把复用的纹理搞混、会不会少读一帧、重读出来的帧是不是同一帧，
//! 只有真跑一遍才量得出来。而这个文件量的正是这些。
//!
//! # 判据：与「顺序引用版本」逐像素一致
//!
//! 夹具**成对**出现，内容一模一样，差别只在「每一路要不要回退」：
//!
//! * `fixtures/rewind-project.doc.json` —— 一份素材、三个片段在时间线上前后颠倒（`a.mp4`）；
//! * `fixtures/rewind-sequential.doc.json` —— 同样三个片段、同样源内位置，但登记成
//!   三个 id（`a/b/c.mp4` 都指向同一个文件），于是**每一路都只向前读**；
//! * `fixtures/pip-project.doc.json` —— 同一份素材同时当「底」与「画中画」，
//!   两个源内帧相距 100 帧（正是 [D4] 说的那种工程）；
//! * `fixtures/pip-sequential.doc.json` —— 同样两层、同样位置，但小窗那层换了另一个 id。
//!
//! 两两渲染同一批帧，逐帧摘要（`FramePng::digest`，就是落进 PNG 的那些字节）必须相等。
//!
//! **回退那条路是真的被走到，而且是断言出来的**：`render_frames_png_run` 会把解码账
//! （命中 / 向前 / **重启** / 一共读了多少源帧）一并交出来，下面直接断言
//! 「倒序那一趟的重启数 > 0」—— **没被走到的判据等于没有判据**。
//! 道理也说得通：池子的槽数按**源自己的**尺寸算（见 `pool_slots`），源是 1080p，
//! 于是 24 槽；而 rewind 那条工程要同时留住 60 帧
//! （实测见 [plan/measurements.md](../../plan/measurements.md) 的池子表）。
//! 换句话说，**这条用例不是"绕开回退"，而是"回退之后像素仍然一样"**。
//!
//! # 为什么用 PNG 而不是 mp4
//!
//! 判据要的是**像素**，而 mp4 会再套一层有损编码：两边编码器的运行状态不同，
//! 逐字节比较会变成「比较编码器」而不是「比较渲染」。PNG 路走的是**同一条**
//! 解码 -> 求值 -> 合成 -> 回读的路（同一个 `DecodingSources` 与同一个池子），
//! 少的只有「喂编码器」那一跳 —— 而那一跳与源无关。
//!
//! 要跑这一条：`cargo test -p dhampir-worker --test rewind -- --ignored`
//! （要真 GPU、PATH 上的 ffmpeg，以及 `target/s3/proxy1080p.mp4`）

use std::path::{Path, PathBuf};

use dhampir_core::compose;
use dhampir_core::overlay::SubtitleTable;
use dhampir_core::timeline::project::{load_doc, ProjectDoc};
use dhampir_worker::pipeline::{
    AudioMode, FramePng, PoolStats, RenderPlan, SourceTable, render_frames_png_run,
};

const RUNNER: &str = "需要真 GPU、PATH 上的 ffmpeg 与 target/s3/proxy1080p.mp4；跑：cargo test -p dhampir-worker --test rewind -- --ignored";

/// 仓库根：`CARGO_MANIFEST_DIR` 指向 `crates/dhampir-worker`。
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("manifest 一定在 crates/<名字> 里")
        .to_path_buf()
}

/// 素材根。与 CLI 的默认值一致（`--asset-root` 的缺省是 `target/s3`）。
fn asset_root() -> PathBuf {
    repo_root().join("target/s3")
}

/// 读一份工程夹具。**读不到就直接失败**：静默跳过等于把这条判据悄悄删掉。
fn fixture(name: &str) -> ProjectDoc {
    let path = repo_root().join("fixtures").join(name);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("读不了夹具 {}：{error}（{RUNNER}）", path.display()));
    load_doc(&text).unwrap_or_else(|error| panic!("夹具 {} 载不进来：{error}", path.display()))
}

/// 工程文件自己登记的素材位置（相对 uri 挂到素材根上）——
/// **用夹具里写的那一份**，不另外维护一张测试专用的表。
fn sources_of(doc: &ProjectDoc) -> SourceTable {
    let mut table = SourceTable::new();
    for asset in &doc.assets {
        if asset.id.is_empty() || asset.uri.is_empty() {
            continue;
        }
        table.insert(asset.id.clone(), asset_root().join(&asset.uri));
    }
    table
}

/// 渲染一整个工程的结果：每帧的摘要 + **解码侧实际发生的事**。
///
/// 把解码账一起带出来很重要 —— 否则这条用例只能证明"像素一样"，
/// 证明不了"回退那条路真的走过"：**没被走到的判据等于没有判据**。
struct Rendered {
    frames: Vec<FramePng>,
    decode: PoolStats,
    opened_streams: usize,
}

/// 渲染一整个工程（逐帧 PNG），返回每帧的摘要与解码账。
fn render_all(name: &str, out: &str) -> Rendered {
    let doc = fixture(name);
    let sources = sources_of(&doc);
    let timebases = doc.asset_timebases();
    let from = compose::first_frame_v2(&doc.timeline).expect("夹具必须至少有一帧");
    let to = compose::end_frame_v2(&doc.timeline)
        .expect("夹具必须有结束帧")
        .saturating_sub(1);
    assert!(to >= from, "夹具 {name} 的帧区间是空的");

    // 帧号**显式摊开**（而不是靠 from..=to）：渲染器按调用方给的顺序取帧，
    // 这里给的就是顺序号，于是"倒序引用"这件事完全落在池子那一层，
    // 而不是被取的帧号顺序掩盖掉。
    let frames: Vec<i64> = (from..=to).collect();
    let dir = repo_root().join("target/t5/rewind-tests").join(out);
    std::fs::create_dir_all(&dir).expect("建不了草稿目录");
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
        background: None,
        subtitles: &subtitles,
        // 这一条与字体无关：夹具里没有字幕轨，给了字体也不会画字。
        font_file: None,
        font_bold_file: None,
        font_dir: None,
        chunk_workers: 1,
        // 这一条与声音无关：量的是解码池子。
        audio: AudioMode::Silent,
        output: &output,
    };
    let run = render_frames_png_run(&plan, &frames).unwrap_or_else(|error| {
        panic!("渲染 {name} 失败：{error}（{RUNNER}）");
    });
    assert_eq!(run.frames.len(), frames.len(), "{name} 要了几帧就该出几帧");
    // 问题清单必须是空的：`source_rewind` / `source_frame_conflict` 这两条
    // 以前正是在这里把整次出片判失败的。
    for frame in &run.frames {
        assert!(
            frame.issues.is_empty(),
            "{name} 第 {} 帧带问题：{:?}",
            frame.frame,
            frame.issues
        );
    }
    println!(
        "{name}: {} 帧, 解码器 {} 路, 命中 {}, 向前 {}, **重启 {}**, 读源帧 {}",
        run.frames.len(),
        run.opened_streams,
        run.decode.hits,
        run.decode.forward,
        run.decode.replays,
        run.decode.frames_read,
    );
    Rendered {
        frames: run.frames,
        decode: run.decode,
        opened_streams: run.opened_streams,
    }
}

/// 逐帧比对两个工程的摘要，返回第一个不一致的地方（没有就是 None）。
fn first_mismatch(left: &[FramePng], right: &[FramePng]) -> Option<String> {
    if left.len() != right.len() {
        return Some(format!("帧数不同：{} vs {}", left.len(), right.len()));
    }
    for (a, b) in left.iter().zip(right.iter()) {
        if a.frame != b.frame {
            return Some(format!("第 {} 帧与第 {} 帧对上了", a.frame, b.frame));
        }
        if a.digest != b.digest {
            return Some(format!(
                "第 {} 帧像素不同：{} vs {}",
                a.frame, a.digest, b.digest
            ));
        }
    }
    None
}

/// [D3]：素材在时间线上前后颠倒也能出片，且与顺序引用版本**逐像素一致**。
#[test]
#[ignore = "需要真 GPU 与 ffmpeg（run with --ignored）"]
fn 倒序引用同一素材与顺序引用版本逐像素一致() {
    let reverse = render_all("rewind-project.doc.json", "reverse");
    let sequential = render_all("rewind-sequential.doc.json", "sequential");
    assert_eq!(reverse.frames.len(), 90, "rewind 夹具是 90 帧");

    // 先证明**这一趟真的走了回退那条路**：没走到就等于没测。
    assert!(
        reverse.decode.replays > 0,
        "倒序引用这一趟一次回退都没发生 —— 这条用例就没验到 D3：{:?}",
        reverse.decode
    );
    assert_eq!(reverse.opened_streams, 1, "倒序工程只有一个源");
    assert_eq!(
        sequential.decode.replays, 0,
        "顺序引用版本不该回退：{:?}",
        sequential.decode
    );
    assert_eq!(sequential.opened_streams, 3, "顺序版本是三路源");

    if let Some(why) = first_mismatch(&reverse.frames, &sequential.frames) {
        panic!("倒序引用改了像素：{why}");
    }
}

/// [D4]：同一输出帧里同一素材要两个不同源内帧也能出片，且与顺序版本**逐像素一致**。
///
/// 这一条比 D3 更细：它同时钉住「池子按 (源, 源内帧) 键复用」——
/// 如果池子把两层的纹理搞混（比如仍按「一路源一张纹理」给），
/// 画中画里就会出现底色那一帧的内容，摘要立刻不等。
#[test]
#[ignore = "需要真 GPU 与 ffmpeg（run with --ignored）"]
fn 同源画中画与顺序版本逐像素一致() {
    let pip = render_all("pip-project.doc.json", "pip");
    let sequential = render_all("pip-sequential.doc.json", "pip-sequential");
    assert_eq!(pip.frames.len(), 30, "pip 夹具是 30 帧");

    // 同一个源被两个轨道按相距 100 帧的位置同时要着。产品实配（1080p 源 -> 32 槽）
    // 刚好够把这一对留住，于是**第二次要旧帧是命中、一次都不用重读** ——
    // 这正是「按 (源, 源内帧) 键复用」买来的东西。
    assert!(
        pip.decode.hits > 0,
        "画中画这一趟一次命中都没有 —— 池子没在复用：{:?}",
        pip.decode
    );
    assert_eq!(pip.opened_streams, 1, "画中画是一个源、两张纹理");
    assert_eq!(sequential.opened_streams, 2, "顺序版本是两路源");
    // 复用要看得见：同一个源、同一个请求序列，池子那条读的帧**比两路顺序解码更少**。
    // 反过来说，如果池子给错了帧（把两层搞混），下面那条逐帧比较立刻红。
    assert!(
        pip.decode.frames_read < sequential.decode.frames_read,
        "池子那条读的源帧（{}）居然不少于两路顺序解码（{}）—— 复用没生效",
        pip.decode.frames_read,
        sequential.decode.frames_read
    );

    if let Some(why) = first_mismatch(&pip.frames, &sequential.frames) {
        panic!("同源两帧被搞混或错位了：{why}");
    }
}
