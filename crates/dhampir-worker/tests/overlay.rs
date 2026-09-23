//! 文字覆盖层**真的落进像素**的真机验证（集成测试）。
//!
//! # 它补的是哪一半
//!
//! T2.1 的评估层（`dhampir-core::overlay`）只验到「结构对」：几行、每行什么、占哪个
//! 归一化矩形。而**结构对不等于画得对** —— 矩形算对了、字画到画面外去，一样是错的。
//! 这一段（T2.4）真正的未知量是「宿主把字画在哪、画出来没有」，它只有真起 GPU 与
//! ffmpeg 才量得出来，所以这个文件里的用例默认 `#[ignore]`。
//!
//! # 为什么单趟与分段各来一遍
//!
//! 渲染器有两条路：没有调整图层时一趟画完；有调整图层时要「先合成一段 -> 跑特效 ->
//! 再往下」（`dhampir-core` 的 `render_segmented`）。**叠加的接线要两条都接上** ——
//! 只接一条是最容易漏的一类，而漏掉的那条表现为「某些工程里字幕突然不见了」。
//!
//! 两条用例共用同一份夹具，只在时间线里多一条调整图层，于是差异只剩路径本身。
//!
//! # 判据怎么量
//!
//! 同一份夹具渲染两次：一次带字幕、一次不带。**两帧的差异就是字的像素** ——
//! 这样量出来的东西与背景无关（背景是棋盘格、在分段路径里还被模糊过），
//! 也不用假设「白字比背景亮」这类关于颜色的前提。然后看三件事：
//!
//! * 有墨迹（差异非空，且不是一两个像素）；
//! * 墨迹的中心就是共享布局给的行盒中心 —— 字画在了该在的地方；
//! * 墨迹是**一行字**的大小，不是整张图 —— 字号与位置没有离谱。
//!
//! # 另一半：没有字要画时，像素必须逐字节不变
//!
//! 接线最容易出的另一类错是「顺手改了点东西」：叠加那一阶段对空覆盖层动了像素、
//! 或者「这次带了字体」这件事本身改了行为。所以下面把三种「没有字要画」摆在一起比摘要：
//! 没有字幕轨、有字幕轨但这一帧没有活着的字幕、以及有字体但这帧没有字幕。
//! 三者必须互相同样，也必须与「有字要画、但没给字体」那一次相同 ——
//! 后者画不出来，那就**一个字节都不许动**，另记一条问题。
//!
//! 要跑这一条：cargo test -p dhampir-worker --test overlay -- --ignored

use std::path::{Path, PathBuf};

use dhampir_core::overlay::SubtitleTable;
use dhampir_core::readback::Rgba8Image;
use dhampir_core::timeline::layer::{AssetTimebases, SubtitleStyle, TimelineV2};
use dhampir_core::timeline::schema::{Frame, TimebaseDto};
use dhampir_core::timeline::subtitle::parse_srt;
use dhampir_core::timeline::text_layout;
use dhampir_worker::pipeline::{FramePng, RenderPlan, SourceTable, render_frames_png};
use serde_json::json;

/// 渲染目标与**文档坐标系**（工程的 render_hints）。两者一致是默认路径 ——
/// 这条要验的不是缩放的换算（那在 T1 的用例里），而是「字有没有画上去、画在哪」。
const TARGET: (u32, u32) = (640, 360);
/// 渲染的帧号。30fps 下第 15 帧 = 500ms，正好落在 [`SRT_HERE`] 那条字幕里。
const FRAME: Frame = 15;
/// 背景图层的帧区间。**从 [`FRAME`] 开始**：夹具是一张单帧 PNG，
/// 这样这一帧要的是素材第 0 帧，顺序解码器给得出来。
const CLIP: (Frame, Frame) = (FRAME, FRAME + 15);

/// 字幕轨引用的素材 id。与时间线 JSON 里写的必须一致 ——
/// 表里缺了它，`evaluate_overlay` 会当成「这条轨没有字幕」（那是宿主的错）。
const SUBTITLE_ASSET: &str = "sub.srt";
/// 背景素材 id，同上。
const BACKGROUND_ASSET: &str = "bg.png";
/// 字幕文本。与 [`SRT_HERE`] 里的那一行必须一致：下面按它算期望的落点。
const SUBTITLE_TEXT: &str = "第一行中文";

/// 这一帧（500ms）上有字的那一条。1000ms = 第 30 帧，区间左闭右开。
const SRT_HERE: &str = "1\n00:00:00,500 --> 00:00:01,000\n第一行中文\n";
/// 只在 3000ms 之后有字 —— 第 15 帧上一条都不活着。
const SRT_ELSEWHERE: &str = "1\n00:00:03,000 --> 00:00:03,500\n这一条不在这一帧\n";

const RUNNER: &str = "需要真 GPU、PATH 上的 ffmpeg 与一个中文字体；跑：cargo test -p dhampir-worker --test overlay -- --ignored";

fn timebase() -> TimebaseDto {
    // 与下面时间线 JSON 里的 timebase 一致（30fps）。
    TimebaseDto { num: 30, den: 1 }
}

/// 仓库根：`CARGO_MANIFEST_DIR` 指向 `crates/dhampir-worker`。
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("manifest 一定在 crates/<名字> 里")
        .to_path_buf()
}

/// 测试用的字体：挑本机常见的那些。**这不是产品默认值** ——
/// 产品路径上字体由 `--font-file` 给，本仓不内嵌字体、也不猜系统字体。
fn font_file() -> PathBuf {
    const CANDIDATES: &[&str] = &[
        "C:/Windows/Fonts/msyh.ttc",
        "C:/Windows/Fonts/simhei.ttf",
        "C:/Windows/Fonts/simsun.ttc",
        "/System/Library/Fonts/PingFang.ttc",
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    ];
    for candidate in CANDIDATES {
        let path = PathBuf::from(candidate);
        if path.is_file() {
            return path;
        }
    }
    panic!("本机一个候选字体都没有：{CANDIDATES:?} —— 这条测试要真字体才跑得起来");
}

/// 渲染一帧的结果：报告里的那一项 + 真的落地的像素。
struct Rendered {
    png: FramePng,
    image: Rgba8Image,
}

/// 夹具：一张带结构的背景图、一个字体、一个 gitignored 的草稿目录。
struct Fixture {
    dir: PathBuf,
    font: PathBuf,
}

impl Fixture {
    /// `name` 同时是目录名：**每个用例一个**，因为 cargo 在同一个进程里并行跑它们。
    fn new(name: &str) -> Self {
        let dir = repo_root().join("target/t2/overlay-tests").join(name);
        std::fs::create_dir_all(&dir).expect("建不了草稿目录");
        let fixture = Self {
            dir,
            font: font_file(),
        };
        fixture.write_background();
        fixture
    }

    fn background(&self) -> PathBuf {
        self.dir.join("bg.png")
    }

    /// 背景素材：640x360 的棋盘格。**不能是纯色** —— 纯色场被模糊之后还是那个纯色，
    /// 于是「调整图层真的生效了」那条判据会恒假（它靠模糊前后的差异立着）。
    fn write_background(&self) {
        let mut pixels = vec![0u8; (TARGET.0 * TARGET.1 * 4) as usize];
        for y in 0..TARGET.1 {
            for x in 0..TARGET.0 {
                let dark = ((x / 8) + (y / 8)) % 2 == 0;
                let px = if dark {
                    [24, 24, 32, 255]
                } else {
                    [232, 224, 200, 255]
                };
                let at = ((y * TARGET.0 + x) * 4) as usize;
                pixels[at..at + 4].copy_from_slice(&px);
            }
        }
        Rgba8Image {
            width: TARGET.0,
            height: TARGET.1,
            pixels,
        }
        .write_png(&self.background())
        .expect("写得下背景图");
    }

    fn sources(&self) -> SourceTable {
        let mut table = SourceTable::new();
        table.insert(BACKGROUND_ASSET, self.background());
        table
    }

    /// 渲染一帧、把它读回。`out` 是这一帧的落盘目录名（同一个用例里要跑好几帧，
    /// 各自一个目录，别互相盖）。`font` 为 `None` 就是「宿主没给字体」那种情况。
    fn render(
        &self,
        out: &str,
        line: &TimelineV2,
        subtitles: &SubtitleTable,
        font: Option<&Path>,
    ) -> Rendered {
        let sources = self.sources();
        let mut timebases = AssetTimebases::new();
        timebases.insert(BACKGROUND_ASSET, timebase());
        let output = self.dir.join(out).join("out.png");
        let plan = RenderPlan {
            timeline: line,
            sources: &sources,
            asset_timebases: &timebases,
            from: FRAME,
            to: FRAME,
            width: TARGET.0,
            height: TARGET.1,
            sequence: TARGET,
            subtitles,
            font_file: font,
            output: &output,
        };
        let mut written = render_frames_png(&plan, &[FRAME]).expect("渲染一帧必须成功");
        assert_eq!(written.len(), 1, "要了一帧就该写一帧");
        let png = written.pop().expect("有一帧");
        let bytes = std::fs::read(&png.path).expect("产出的 PNG 要读得回来");
        let image = Rgba8Image::decode_png(&bytes).expect("产出的 PNG 必须是 8 位 RGBA");
        assert_eq!((image.width, image.height), TARGET, "PNG 尺寸不对");
        Rendered { png, image }
    }
}

/// 时间线：一条背景轨 + （可选）一条调整轨 + （可选）一条字幕轨。
///
/// 从 JSON 造而不是手工摆结构体：工程文件本来就是这么来的，
/// 手摆一份会让夹具悄悄长成与真实路径不同的样子。
fn fixture_timeline(adjustment: bool, subtitle_track: bool) -> TimelineV2 {
    let mut tracks = vec![json!({
        "id": "v1",
        "kind": "video",
        "layers": [{
            "id": "bg",
            "start": CLIP.0,
            "end": CLIP.1,
            "source": { "asset_id": BACKGROUND_ASSET, "source_in": 0 }
        }]
    })];
    if adjustment {
        // **这就是「分段路径」的开关**：没有素材、只有特效 = 调整图层，
        // 渲染器遇到它必须切段（先合成到中间纹理、跑特效、再往下）。
        tracks.push(json!({
            "id": "adj",
            "kind": "video",
            "layers": [{
                "id": "adj-blur",
                "start": CLIP.0,
                "end": CLIP.1,
                "effects": [{ "kind": "gaussian_blur", "params": { "radius": 6.0 } }]
            }]
        }));
    }
    if subtitle_track {
        // `subtitle` 给空对象：字段全走默认值。**有样式与没样式是两回事** ——
        // 没有样式的字幕轨按契约不参与（T2.1 的用例盯着那条），这里要的是参与。
        tracks.push(json!({
            "id": "sub",
            "kind": "subtitle",
            "layers": [{
                "id": "cue",
                "start": CLIP.0,
                "end": CLIP.1,
                "source": { "asset_id": SUBTITLE_ASSET, "source_in": 0 }
            }],
            "subtitle": {}
        }));
    }
    let text = json!({
        "schema": 3,
        "timebase": { "num": 30, "den": 1 },
        "tracks": tracks
    })
    .to_string();
    serde_json::from_str(&text).expect("夹具时间线必须能反序列化")
}

/// 把 SRT 解析成字幕表。**走真实解析**，不手摆字幕条 ——
/// 毫秒到帧的换算是这段要验的东西之一，手摆就把那一步跳过去了。
fn table(srt: &str) -> SubtitleTable {
    let report = parse_srt(srt).expect("夹具 SRT 必须能解析");
    assert_eq!(report.skipped, 0, "夹具 SRT 不该有看不懂而跳过的条目");
    let mut table = SubtitleTable::new();
    table.insert(SUBTITLE_ASSET.to_string(), report.cues);
    table
}

/// 两张图哪里不一样：`(不同的像素数, 包围盒)`。逐字节一样时包围盒是 `None`。
///
/// 量「字画在哪」用差异，而不是直接看颜色：背景是棋盘格、在分段路径里还被模糊过，
/// 拿绝对颜色当判据就得先知道背景长什么样 —— 而那是另一个被测的东西。
fn diff(a: &Rgba8Image, b: &Rgba8Image) -> (usize, Option<(u32, u32, u32, u32)>) {
    assert_eq!(
        (a.width, a.height),
        (b.width, b.height),
        "两张图尺寸不同，比不了"
    );
    let mut count = 0usize;
    let mut bounds: Option<(u32, u32, u32, u32)> = None;
    for y in 0..a.height {
        for x in 0..a.width {
            let at = ((y * a.width + x) * 4) as usize;
            if a.pixels[at..at + 4] != b.pixels[at..at + 4] {
                count += 1;
                bounds = Some(match bounds {
                    None => (x, y, x, y),
                    Some((x0, y0, x1, y1)) => (x0.min(x), y0.min(y), x1.max(x), y1.max(y)),
                });
            }
        }
    }
    (count, bounds)
}

/// 共享布局说这一行该落在哪（画面像素坐标）。
///
/// **期望值取自共享布局本身**，不是这串常量：这一段要证的是「宿主画在了结构说的位置上」。
fn expected_centre() -> (f32, f32) {
    let laid = text_layout::layout(SUBTITLE_TEXT, &SubtitleStyle::default(), TARGET);
    assert_eq!(laid.lines.len(), 1, "夹具是一行字");
    assert_eq!(laid.dropped_lines, 0, "夹具这一行不该被丢弃");
    let rect = laid.lines[0].rect;
    (
        rect.center_x() * TARGET.0 as f32,
        (rect.y + rect.height / 2.0) * TARGET.1 as f32,
    )
}

/// 墨迹画在了该在的地方：中心对准行盒中心，大小是一行字的量级。
fn assert_ink_placed(bounds: (u32, u32, u32, u32), label: &str) {
    let (want_x, want_y) = expected_centre();
    let got_x = (bounds.0 + bounds.2) as f32 / 2.0;
    let got_y = (bounds.1 + bounds.3) as f32 / 2.0;
    let width = bounds.2 - bounds.0 + 1;
    let height = bounds.3 - bounds.1 + 1;
    println!(
        "{label}：墨迹 {bounds:?}，中心 ({got_x}, {got_y})，期望 {want_x}/{want_y}，尺寸 {width}x{height}"
    );
    // 容差横向 5px、纵向 6px：位图中心对准行盒中心（place_line 的规则），而字形在字体里
    // 不是严格居中的 —— drawtext 居中的是**字体的行高盒**，墨迹比它偏一点是正常的。
    // 这条挡的是「画到别处去了」那种量级的错（左对齐、整体上移一整行）。
    assert!(
        (got_x - want_x).abs() <= 5.0,
        "{label}：横向中心 {got_x} 离期望 {want_x} 太远"
    );
    assert!(
        (got_y - want_y).abs() <= 6.0,
        "{label}：纵向中心 {got_y} 离期望 {want_y} 太远"
    );
    // 一行字（字号 17px、行盒 23.76px、位图高 30px）：墨迹不该只有几个像素（没画出来），
    // 也不该是一整张图（字号或落点错了）。
    assert!(
        (40..=180).contains(&width),
        "{label}：墨迹宽 {width}，不像一行字"
    );
    assert!(
        (6..=30).contains(&height),
        "{label}：墨迹高 {height}，不像一行字"
    );
}

#[test]
#[ignore = "需要真 GPU、PATH 上的 ffmpeg 与一个中文字体；跑：cargo test -p dhampir-worker --test overlay -- --ignored"]
fn 单趟路径上字幕画进了像素() {
    let fixture = Fixture::new("single-pass");
    let line = fixture_timeline(false, true);
    let with_text = fixture.render(
        "with-text",
        &line,
        &table(SRT_HERE),
        Some(fixture.font.as_path()),
    );
    let empty = SubtitleTable::new();
    let without = fixture.render("without-text", &line, &empty, Some(fixture.font.as_path()));

    // 1. 账面上画了：一行、没失败、没被切、冷缓存一次未命中（真的起了栅格化）。
    assert!(
        with_text.png.issues.is_empty(),
        "字幕不该有问题：{:?}",
        with_text.png.issues
    );
    let stats = with_text.png.overlay;
    assert_eq!(stats.lines_drawn, 1, "这一帧该画一行");
    assert_eq!(stats.lines_failed, 0, "画不出来就不叫画了");
    assert_eq!(stats.lines_clipped, 0, "夹具这一行放得下，不该被切");
    assert_eq!(stats.lines_dropped, 0);
    assert_eq!(
        (stats.cache_hits, stats.cache_misses),
        (0, 1),
        "一帧一行：一次未命中"
    );

    // 2. 像素里真有字。
    let (changed, bounds) = diff(&without.image, &with_text.image);
    let bounds = bounds.unwrap_or_else(|| panic!("字幕一个像素都没改 —— 账上说画了也不算数"));
    assert!(changed > 30, "只改了 {changed} 个像素，不像一行字");
    println!(
        "单趟：改了 {changed} 个像素（画面 {}x{}）",
        TARGET.0, TARGET.1
    );

    // 3. 字画在了结构说的位置上。
    assert_ink_placed(bounds, "单趟");
}

#[test]
#[ignore = "需要真 GPU、PATH 上的 ffmpeg 与一个中文字体；跑：cargo test -p dhampir-worker --test overlay -- --ignored"]
fn 分段路径上字幕画在了同一处() {
    let fixture = Fixture::new("segmented");
    let single = fixture_timeline(false, true);
    let segmented = fixture_timeline(true, true);
    let empty = SubtitleTable::new();

    let with_text = fixture.render(
        "with-text",
        &segmented,
        &table(SRT_HERE),
        Some(fixture.font.as_path()),
    );
    let without = fixture.render(
        "without-text",
        &segmented,
        &empty,
        Some(fixture.font.as_path()),
    );
    // 对照：同一条背景轨走**单趟**路径，用来（a）证明上面那一帧真的走了分段路径、
    // （b）证明两条路径上字落在同一处。
    let plain = fixture.render("single-pass", &single, &empty, Some(fixture.font.as_path()));
    let plain_text = fixture.render(
        "single-text",
        &single,
        &table(SRT_HERE),
        Some(fixture.font.as_path()),
    );

    // 1. **调整图层真的生效了**：模糊改了像素。没有这一条，下面「字画上去了」
    //    可能是在单趟路径上过的 —— 那这条用例就白写了。
    let (changed, backdrop) = diff(&plain.image, &without.image);
    assert!(
        changed > 1000,
        "加了调整图层只改了 {changed} 个像素 —— 分段路径没跑起来，下面的判据是空转的"
    );
    println!("分段：调整图层改了 {changed} 个像素，范围 {backdrop:?}");

    // 2. 账面上画了。
    assert!(
        with_text.png.issues.is_empty(),
        "字幕不该有问题：{:?}",
        with_text.png.issues
    );
    let stats = with_text.png.overlay;
    assert_eq!(stats.lines_drawn, 1, "分段路径上这一帧也该画一行");
    assert_eq!(stats.lines_failed, 0);
    assert_eq!(stats.lines_clipped, 0);

    // 3. 像素里真有字。
    let (changed, bounds) = diff(&without.image, &with_text.image);
    let bounds = bounds.unwrap_or_else(|| panic!("分段路径上字幕一个像素都没画"));
    assert!(changed > 30, "只改了 {changed} 个像素，不像一行字");
    let _ = changed;
    assert_ink_placed(bounds, "分段");

    // 4. **两条路径上字落在同一处**：叠加发生在读回之后，与合成走哪条路无关。
    //    若哪天有人把叠加塞进某一条路径里，这条就会红。
    let (_, plain_bounds) = diff(&plain.image, &plain_text.image);
    let plain_bounds = plain_bounds.unwrap_or_else(|| panic!("单趟路径上字幕一个像素都没画"));
    let centre = |b: (u32, u32, u32, u32)| ((b.0 + b.2) as f32 / 2.0, (b.1 + b.3) as f32 / 2.0);
    let (seg_x, seg_y) = centre(bounds);
    let (plain_x, plain_y) = centre(plain_bounds);
    assert!(
        (seg_x - plain_x).abs() <= 2.0 && (seg_y - plain_y).abs() <= 2.0,
        "两条路径上字的中心不同：分段 ({seg_x}, {seg_y}) vs 单趟 ({plain_x}, {plain_y})"
    );
}

#[test]
#[ignore = "需要真 GPU、PATH 上的 ffmpeg 与一个中文字体；跑：cargo test -p dhampir-worker --test overlay -- --ignored"]
fn 没有字要画时像素逐字节不变() {
    let fixture = Fixture::new("no-text");
    let empty = SubtitleTable::new();
    let font = Some(fixture.font.as_path());

    // 四种「这一帧没有字要画」，像素必须两两相同：
    // 1. 没有字幕轨，给了字体；
    let plain = fixture_timeline(false, false);
    let with_font = fixture.render("plain", &plain, &empty, font);
    // 2. 没有字幕轨，没给字体 —— **带字体这件事本身不许改行为**；
    let no_font = fixture.render("plain-no-font", &plain, &empty, None);
    // 3. 有字幕轨，但这一帧没有活着的字幕（那条字幕在 3000ms 之后）；
    let track = fixture_timeline(false, true);
    let idle = fixture.render("idle-track", &track, &table(SRT_ELSEWHERE), font);
    // 4. **反向**：有字要画、却没给字体 —— 画不出来就一个字节都不许动。
    let missing = fixture.render("missing-font", &track, &table(SRT_HERE), None);

    assert_eq!(
        with_font.png.digest, no_font.png.digest,
        "给了字体却改了像素"
    );
    assert_eq!(
        idle.png.digest, with_font.png.digest,
        "这一帧没有字幕，像素不该变"
    );
    assert_eq!(
        missing.png.digest, with_font.png.digest,
        "画不出来就不许动像素"
    );
    println!(
        "无字三态 + 画不出来的像素摘要一致：{}",
        with_font.png.digest
    );

    // 「没动像素」与「没记问题」是两件事：没字要画时不该有问题，而**画不出来必须出声** ——
    // 静默出一份没有字幕的片子正是这套东西要消灭的失效模式。
    assert!(
        with_font.png.issues.is_empty(),
        "没字要画却记了问题：{:?}",
        with_font.png.issues
    );
    assert!(
        idle.png.issues.is_empty(),
        "没字要画却记了问题：{:?}",
        idle.png.issues
    );
    assert!(
        with_font.png.overlay.is_silent() && idle.png.overlay.is_silent(),
        "没字要画时账上应当是空的"
    );
    assert_eq!(
        missing.png.issues.len(),
        1,
        "画不出来要正好记一条：{:?}",
        missing.png.issues
    );
    assert_eq!(missing.png.issues[0].code, "subtitle_font_missing");
    assert_eq!(
        missing.png.overlay.lines_failed, 1,
        "画不出来的行数要如实计数"
    );
    assert_eq!(missing.png.overlay.lines_drawn, 0);
}

/// 这一条盯着的是**常量与夹具没有走散**：`RUNNER` 那段话写的是本文件的跑法，
/// 而三个用例的 `#[ignore]` 文案必须与它逐字一致（守卫按文案找用例）。
#[test]
fn ignore_文案与跑法说明一致() {
    // `file!()` 是相对路径，而测试的工作目录是 crate 根（`crates/dhampir-worker`），
    // 拼起来读不到 —— 所以从 `CARGO_MANIFEST_DIR` 出发，和其它文件路径一个规矩。
    let source =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/overlay.rs"))
            .expect("读得回本文件");
    for line in source.lines().filter(|line| line.starts_with("#[ignore")) {
        assert!(
            line.contains("cargo test -p dhampir-worker --test overlay -- --ignored"),
            "ignore 文案里没有跑法：{line}"
        );
    }
    assert!(RUNNER.contains("cargo test -p dhampir-worker --test overlay -- --ignored"));
}
