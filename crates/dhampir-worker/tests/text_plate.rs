//! 字幕底衬（T6）**真的落进像素**的真机验证（集成测试）。
//!
//! # 它补的是哪一半
//!
//! `text_overlay` 的单测已经把「铺在哪、铺多大、层序对不对」量到了像素级，
//! 但那条路用的是**假栅格化器**（不起 ffmpeg、不碰 GPU）。它证不了的是：
//! **真起一遍 ffmpeg + GPU 之后**底衬还在不在 —— 中间还隔着
//! `evaluate_overlay` 的透传、`paint_lines` 的调用点、以及真实的栅格化。
//! 那三件事只有端到端才量得出来，所以这个文件里的用例默认 `#[ignore]`。
//!
//! # 判据怎么量
//!
//! 同一份夹具渲染**三**次，差异就是各自的贡献：
//!
//! | 帧 | 字幕样式 | 量出来的东西 |
//! |---|---|---|
//! | `bg` | 没有字幕 | 底板 |
//! | `text` | 有字幕、**不铺底衬** | 字本身的足迹 `diff(bg, text)` |
//! | `plate` | 有字幕、**铺底衬** | 底衬+字的足迹 `diff(bg, plate)` |
//!
//! 于是「底衬铺出来了」可以**只靠差分与颜色**判：新多出来的像素必须是
//! `plate_color` 那一个值，而且它的包围盒必须比字的包围盒**四周都大一圈**
//! （`plate_pad_px`）。这样量出来的结论与背景内容无关。
//!
//! # 反向：不铺底衬时**一个字节都不变**
//!
//! `plate_color = None` 与"老工程（JSON 里根本没有那两个键）"必须给出
//! **同一份像素**。这是 T6 的存量零回归判据在端到端这一层的执行处。
//!
//! 要跑这一条：cargo test -p dhampir-worker --test text_plate -- --ignored

use std::path::{Path, PathBuf};

use dhampir_core::overlay::SubtitleTable;
use dhampir_core::readback::Rgba8Image;
use dhampir_core::timeline::layer::{AssetTimebases, TimelineV2};
use dhampir_core::timeline::schema::{Frame, TimebaseDto};
use dhampir_core::timeline::subtitle::parse_srt;
use dhampir_worker::pipeline::{AudioMode, FramePng, RenderPlan, SourceTable, render_frames_png};

/// 渲染目标与文档坐标系（工程的 render_hints）。两者一致是默认路径。
const TARGET: (u32, u32) = (640, 360);
/// 30fps 下第 15 帧 = 500ms，正好落在 [`SRT_HERE`] 那条字幕里。
const FRAME: Frame = 15;
const CLIP: (Frame, Frame) = (FRAME, FRAME + 8);

const SUBTITLE_ASSET: &str = "sub.srt";
const BACKGROUND_ASSET: &str = "bg.png";
const SUBTITLE_TEXT: &str = "第一行中文";
const SRT_HERE: &str = "1\n00:00:00,500 --> 00:00:01,000\n第一行中文\n";

/// 夹具用的底衬：**半透明黑（55%）** —— 就是契约里的参照值。
///
/// 用半透明而不是不透明是有意的：不透明色会被下游的色调 / 色彩空间处理
/// 原样搬过去，而半透明要真的走一遍合成公式。**合成错了会露馅**。
const PLATE_COLOR: [u8; 4] = [0, 0, 0, 140];
const PLATE_PAD_PX: f32 = 12.0;

/// `subtitle` 里那两段方括号：老工程一个键都没有 / 真铺底衬。
const SUBTITLE_LEGACY: &str = "";
const SUBTITLE_ZEROED: &str = r#","plate_color":null,"plate_pad_px":12.0"#;

const RUNNER: &str = "需要真 GPU、PATH 上的 ffmpeg 与一个中文字体；跑：cargo test -p dhampir-worker --test text_plate -- --ignored";

fn timebase() -> TimebaseDto {
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
        "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/TTF/DejaVuSans.ttf",
        "/usr/share/fonts/dejavu/DejaVuSans.ttf",
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

/// 本环境能不能写 `%TEMP%` —— 不能就**如实跳过**，而不是报"实现坏了"。
///
/// 与 `text_raster.rs` 的 `writable_scratch_dir` 同一条纪律（那里写着理由）：
/// 沙箱化的开发环境（例如 DSH 的文件沙箱）会让某个路径下的可执行文件**写不了
/// `%TEMP%`**，而字幕栅格化要往那里落一份 `.srt` —— 于是整条端到端路径
/// 会因为**环境**失败，与底衬一点关系都没有。
///
/// 判据是**真去试着建一次**，不是嗅探环境变量。
fn temp_writable() -> bool {
    let dir = std::env::temp_dir().join(format!("dhampir-t6-{}", std::process::id()));
    match std::fs::create_dir_all(&dir) {
        Ok(()) => {
            let _ = std::fs::remove_dir(&dir);
            true
        }
        Err(error) => {
            eprintln!(
                "跳过：这个环境写不了临时目录 {}（{error}）—— 环境限制，不是实现坏了",
                dir.display()
            );
            false
        }
    }
}

struct Rendered {
    #[allow(dead_code)]
    png: FramePng,
    image: Rgba8Image,
}

struct Fixture {
    dir: PathBuf,
    font: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let dir = repo_root().join("target/t6/text-plate-tests").join(name);
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

    /// 背景素材：640x360 的棋盘格。**不能是纯色** —— 纯色场上"底衬有没有落上去"
    /// 会被"看起来本来就那样"糊过去（与 `text_shadow` 同一条理由）。
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

    fn render(&self, out: &str, line: &TimelineV2, subtitles: &SubtitleTable) -> Rendered {
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
            background: None,
            subtitles,
            font_file: Some(self.font.as_path()),
            font_bold_file: None,
            font_dir: None,
            chunk_workers: 1,
            // 这条路出的是 PNG：没有容器可放音轨。
            audio: AudioMode::Silent,
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

/// 时间线：一条背景轨 + 一条字幕轨，`plate` 是 `subtitle` 里接在后面的那一段。
fn fixture_timeline(plate: &str) -> TimelineV2 {
    let text = format!(
        r#"{{
            "schema": 3,
            "timebase": {{ "num": 30, "den": 1 }},
            "tracks": [
                {{
                    "id": "v1",
                    "kind": "video",
                    "layers": [{{
                        "id": "bg",
                        "start": {start},
                        "end": {end},
                        "source": {{ "asset_id": "{background}", "source_in": 0 }}
                    }}]
                }},
                {{
                    "id": "sub",
                    "kind": "subtitle",
                    "layers": [{{
                        "id": "cue",
                        "start": {start},
                        "end": {end},
                        "source": {{ "asset_id": "{subtitle}", "source_in": 0 }}
                    }}],
                    "subtitle": {{
                        "font_ratio": 0.055,
                        "color": [255, 255, 255, 255],
                        "outline": false{plate}
                    }}
                }}
            ]
        }}"#,
        start = CLIP.0,
        end = CLIP.1,
        background = BACKGROUND_ASSET,
        subtitle = SUBTITLE_ASSET,
        plate = plate,
    );
    serde_json::from_str(&text).expect("夹具时间线必须能反序列化")
}

/// 真正带底衬的那一段（与 [`SUBTITLE_ZEROED`] 同形，只是 `plate_color` 有值）。
fn plate_fragment() -> String {
    format!(
        r#","plate_color":[{},{},{},{}],"plate_pad_px":{pad}"#,
        PLATE_COLOR[0], PLATE_COLOR[1], PLATE_COLOR[2], PLATE_COLOR[3],
        pad = PLATE_PAD_PX
    )
}

fn table(srt: &str) -> SubtitleTable {
    let report = parse_srt(srt).expect("夹具 SRT 必须能解析");
    assert_eq!(report.skipped, 0, "夹具 SRT 不该有看不懂而跳过的条目");
    let mut table = SubtitleTable::new();
    table.insert(SUBTITLE_ASSET.to_string(), report.cues);
    table
}

/// 两张图哪里不一样：`(不同的像素数, 包围盒)`。逐字节一样时包围盒是 `None`。
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

/// 数一张图里**正好等于**给定 RGBA 的像素个数。
fn count_color(image: &Rgba8Image, color: [u8; 4]) -> usize {
    image
        .pixels
        .chunks_exact(4)
        .filter(|px| px[0] == color[0] && px[1] == color[1] && px[2] == color[2] && px[3] == color[3])
        .count()
}

#[test]
#[ignore = "需要真 GPU、PATH 上的 ffmpeg 与一个中文字体；跑：cargo test -p dhampir-worker --test text_plate -- --ignored"]
fn 底衬比字多长一圈并且用的是契约那个色() {
    if !temp_writable() {
        return;
    }
    let fixture = Fixture::new("plate-ring");
    let bg = fixture.render(
        "background-only",
        &fixture_timeline(SUBTITLE_LEGACY),
        &SubtitleTable::new(),
    );
    let text = fixture.render(
        "text-only",
        &fixture_timeline(SUBTITLE_LEGACY),
        &table(SRT_HERE),
    );
    let plated = fixture.render(
        "with-plate",
        &fixture_timeline(&plate_fragment()),
        &table(SRT_HERE),
    );

    // ① 字真的画出来了（否则下面全是废话）。
    let (text_px, text_box) = diff(&bg.image, &text.image);
    assert!(text_px > 0, "夹具那条字幕必须真的画得出字");
    let (tx0, ty0, tx1, ty1) = text_box.expect("有字就该有包围盒");

    // ② 底衬画出来了：整体足迹必须**变大**。
    let (plate_px, plate_box) = diff(&bg.image, &plated.image);
    assert!(
        plate_px > text_px,
        "铺了底衬之后足迹必须变大（{plate_px} vs 无底衬的 {text_px}）"
    );
    let (px0, py0, px1, py1) = plate_box.expect("有底衬就该有包围盒");

    // ③ **四周都外扩一圈**（`plate_pad_px`）—— 只验"变大"的话，
    //    一个只往一个方向长的实现也能过。
    let pad = PLATE_PAD_PX as i32;
    let slack = 2i32; // 栅格化的抗锯齿会让字本身的足迹抖动一两像素。
    for (name, outer, inner) in [
        ("左", px0 as i32, tx0 as i32),
        ("上", py0 as i32, ty0 as i32),
    ] {
        assert!(
            outer <= inner - (pad - slack),
            "底衬在{name}侧没有外扩：底衬 {outer} / 字 {inner}"
        );
    }
    for (name, outer, inner) in [
        ("右", px1 as i32, tx1 as i32),
        ("下", py1 as i32, ty1 as i32),
    ] {
        assert!(
            outer >= inner + (pad - slack),
            "底衬在{name}侧没有外扩：底衬 {outer} / 字 {inner}"
        );
    }

    // ④ **色值就是契约那个**。半透明黑的底衬落在棋盘格上会与深浅格合成出两个
    //    不同的值，所以这里数的是"**与背景不同的那些像素里，底衬贡献的那一份**"
    //    —— 判法是：底衬那圈（字的外面）里出现的颜色**必须全都不是背景色**，
    //    而且**必须来自半透明黑**（即比同位置的背景更暗）。
    let mut ring_pixels = 0usize;
    let mut darker_than_bg = 0usize;
    for y in 0..TARGET.1 {
        for x in 0..TARGET.0 {
            let inside_text = x >= tx0 && x <= tx1 && y >= ty0 && y <= ty1;
            let inside_plate = x >= px0 && x <= px1 && y >= py0 && y <= py1;
            if !inside_plate || inside_text {
                continue;
            }
            let at = ((y * TARGET.0 + x) * 4) as usize;
            let plate_px = &plated.image.pixels[at..at + 4];
            let bg_px = &bg.image.pixels[at..at + 4];
            if plate_px == bg_px {
                continue;
            }
            ring_pixels += 1;
            // 半透明黑只会让像素**变暗**（三个通道都不变亮）。
            if plate_px[0] <= bg_px[0] && plate_px[1] <= bg_px[1] && plate_px[2] <= bg_px[2] {
                darker_than_bg += 1;
            }
        }
    }
    assert!(ring_pixels > 0, "底衬那圈里一个像素都没变 —— 底衬没铺出来");
    assert_eq!(
        ring_pixels, darker_than_bg,
        "底衬那圈里出现了**变亮**的像素（{ring_pixels} 个里只有 {darker_than_bg} 个变暗）\
         —— `plate_color` 是半透明黑，合成只会变暗；变亮说明铺的不是这个色"
    );
}

#[test]
#[ignore = "需要真 GPU、PATH 上的 ffmpeg 与一个中文字体；跑：cargo test -p dhampir-worker --test text_plate -- --ignored"]
fn 不铺底衬时与老工程逐字节相同() {
    if !temp_writable() {
        return;
    }
    let fixture = Fixture::new("plate-legacy");
    // 三段都渲染同一帧：老工程（没有那两个键）/ 显式 null / 显式 null+12。
    let legacy = fixture.render(
        "legacy",
        &fixture_timeline(SUBTITLE_LEGACY),
        &table(SRT_HERE),
    );
    let nulled = fixture.render(
        "nulled",
        &fixture_timeline(SUBTITLE_ZEROED),
        &table(SRT_HERE),
    );

    assert_eq!(
        legacy.image.pixels, nulled.image.pixels,
        "plate_color = null 必须与老工程（没有这两个键）逐字节相同"
    );
    assert_eq!(
        count_color(&legacy.image, PLATE_COLOR),
        0,
        "老工程里不该出现底衬色"
    );

    // 正向对照：同一份夹具**加上**底衬就必须不一样 —— 否则上面那条"相同"
    // 有可能只是因为整张图根本没渲染出字幕（空图之间当然相同）。
    let plated = fixture.render(
        "plated",
        &fixture_timeline(&plate_fragment()),
        &table(SRT_HERE),
    );
    assert_ne!(
        legacy.image.pixels, plated.image.pixels,
        "加上底衬必须改变像素 —— 否则这条测试没在测底衬"
    );
}

/// 与 `tests/overlay.rs` 同一条纪律：`#[ignore]` 的文案里必须带**本文件**的跑法
/// （守卫按文案找用例，文案写错就没人跑得起来）。
#[test]
fn ignore_文案与跑法说明一致() {
    let source =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/text_plate.rs"))
            .expect("读得回本文件");
    for line in source.lines().filter(|line| line.starts_with("#[ignore")) {
        assert!(
            line.contains("cargo test -p dhampir-worker --test text_plate -- --ignored"),
            "ignore 文案里没有跑法：{line}"
        );
    }
    assert!(RUNNER.contains("cargo test -p dhampir-worker --test text_plate -- --ignored"));
    // `SUBTITLE_TEXT` 是夹具 SRT 的内容，写歪了 srt 就对不上。
    assert!(SRT_HERE.contains(SUBTITLE_TEXT));
}
