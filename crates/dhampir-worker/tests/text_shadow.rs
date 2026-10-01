//! 文字阴影（B5）**真的落进像素**的真机验证（集成测试）。
//!
//! # 它补的是哪一半
//!
//! 求值层（`dhampir-core::overlay`）只验到「样式里的四个字段透传 + 比例换成像素」，
//! `text_raster` 的单测只验到「参数串里有 `gblur`、画布扩了多少边」。
//! 两样都对了，画面上仍然可能是：**阴影没贴、贴错方向、或者根本没糊**（`gblur` 没生效）。
//! 那三件事只有真起 GPU 与 ffmpeg 才量得出来，所以这个文件里的用例默认 `#[ignore]`。
//!
//! # 判据怎么量
//!
//! 同一份夹具渲染**三**次，差异就是各自的贡献：
//!
//! | 帧 | 字幕 | 量出来的东西 |
//! |---|---|---|
//! | `bg` | 没有 | 底板 |
//! | `text` | 有、**不画阴影** | 字本身的足迹（`diff(bg, text)`） |
//! | `shadow` | 有、**画阴影** | 影子的足迹（`diff(text, shadow)`） |
//!
//! 于是「影子落在偏移方向那一侧」这件事可以**只靠差分**判：影子的足迹必须
//! **往下长出字本身**（`dy = +10`），而上边不许冒出去。这样量出来的结论与背景无关，
//! 也不用假设"影子比背景暗"这类关于颜色的前提。
//!
//! # "带模糊"怎么判（这条最容易被糊弄过去）
//!
//! 帧上判"渐变"会被背景的棋盘格搅浑，所以模糊那一条**直接量那张位图**：
//! 硬阴影（`blur = 0`）的 alpha 只有"没有墨"与"实心"两档（外加字形自己的抗锯齿），
//! 而模糊之后**字形的轮廓之外**必须出现一圈 alpha 渐变 —— 具体是：
//! 从墨迹上边界往上 3 行，硬阴影是**全 0**，模糊的那张必须是**既非 0 也非 255** 的一串值。
//! 这条判据能把「`gblur` 写了但没生效」与「`gblur` 生效了」分开。
//!
//! # 反向：不画阴影时**一个字节都不变**
//!
//! `shadow_color = None` 与"老工程（JSON 里根本没有那几个键）"必须给出**同一份像素**，
//! 而且**只栅格化一次**（不许为阴影多起一次 ffmpeg 进程）。
//!
//! 要跑这一条：cargo test -p dhampir-worker --test text_shadow -- --ignored

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use dhampir_core::overlay::SubtitleTable;
use dhampir_core::readback::Rgba8Image;
use dhampir_core::timeline::layer::{AssetTimebases, TimelineV2};
use dhampir_core::timeline::schema::{Frame, TimebaseDto};
use dhampir_core::timeline::subtitle::parse_srt;
use dhampir_worker::pipeline::{AudioMode, FramePng, RenderPlan, SourceTable, render_frames_png};
use dhampir_worker::text_raster::{TextBitmap, TextRasterKey, bitmap_size, rasterize_line, shadow_key};

/// 渲染目标与文档坐标系（工程的 render_hints）。两者一致是默认路径。
const TARGET: (u32, u32) = (640, 360);
/// 30fps 下第 15 帧 = 500ms，正好落在 [`SRT_HERE`] 那条字幕里。
const FRAME: Frame = 15;
const CLIP: (Frame, Frame) = (FRAME, FRAME + 15);

const SUBTITLE_ASSET: &str = "sub.srt";
const BACKGROUND_ASSET: &str = "bg.png";
const SUBTITLE_TEXT: &str = "第一行中文";
const SRT_HERE: &str = "1\n00:00:00,500 --> 00:00:01,000\n第一行中文\n";

/// 夹具用的阴影参数：**向下偏 10px、模糊 6px、半透明黑**。
///
/// 契约里模糊是**比例**（占文档高），所以 6px 在这套文档坐标系里是 `6/360`。
const SHADOW_DY_PX: f32 = 10.0;
const SHADOW_BLUR_PX: f32 = 6.0;
const SHADOW_BLUR_RATIO: f32 = SHADOW_BLUR_PX / 360.0;

/// `subtitle` 里那三段方括号：老工程一个键都没有 / 显式写零 / 真画阴影。
const SUBTITLE_LEGACY: &str = "";
const SUBTITLE_ZEROED: &str =
    r#","shadow_color":null,"shadow_dx_px":0.0,"shadow_dy_px":0.0,"shadow_blur_ratio":0.0"#;

const RUNNER: &str = "需要真 GPU、PATH 上的 ffmpeg 与一个中文字体；跑：cargo test -p dhampir-worker --test text_shadow -- --ignored";

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
        // 发行版的字体布局不一样：Debian 是 opentype/noto 与 truetype/dejavu，Arch / Fedora
        // 是 noto-cjk 与 TTF/，openSUSE 是 dejavu/。只列一种的话，装了字体也会报「一个候选字体
        // 都没有」—— 那是**环境差异**，不是测试坏了（同一类问题还出现在 Chrome 候选表上）。
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

struct Rendered {
    png: FramePng,
    image: Rgba8Image,
}

struct Fixture {
    dir: PathBuf,
    font: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let dir = repo_root().join("target/t2/text-shadow-tests").join(name);
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

    /// 背景素材：640x360 的棋盘格。**不能是纯色** —— 纯色场上"影子有没有落上去"
    /// 会被"看起来本来就那样"糊过去。
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

/// 时间线：一条背景轨 + 一条字幕轨，`shadow` 是 `subtitle` 里接在后面的那一段。
///
/// 三段（老 / 显式零 / 有阴影）**只有那一段不同**，于是"差在哪儿"这件事是可判的。
fn fixture_timeline(shadow: &str) -> TimelineV2 {
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
                        "outline": false{shadow}
                    }}
                }}
            ]
        }}"#,
        start = CLIP.0,
        end = CLIP.1,
        background = BACKGROUND_ASSET,
        subtitle = SUBTITLE_ASSET,
        shadow = shadow,
    );
    serde_json::from_str(&text).expect("夹具时间线必须能反序列化")
}

/// 真正带阴影的那一段（与 [`SUBTITLE_LEGACY`] / [`SUBTITLE_ZEROED`] 同形，只是有值）。
fn shadow_fragment() -> String {
    format!(
        r#","shadow_color":[0,0,0,180],"shadow_dx_px":0.0,"shadow_dy_px":{dy},"shadow_blur_ratio":{blur}"#,
        dy = SHADOW_DY_PX,
        blur = SHADOW_BLUR_RATIO
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

/// 一张位图里出现过几种不同的 alpha。**模糊的判据就是它**：
/// 硬阴影只有"没有墨"与"实心"两档（外加字形自己的抗锯齿），模糊之后会多出一大片中间值。
fn alpha_levels(bitmap: &TextBitmap) -> BTreeSet<u8> {
    let mut levels = BTreeSet::new();
    for y in 0..bitmap.height {
        for x in 0..bitmap.width {
            if let Some(px) = bitmap.pixel(x, y) {
                if px[3] > 0 {
                    levels.insert(px[3]);
                }
            }
        }
    }
    levels
}

/// 一条测试用的**文字**位图键（阴影键从它派生）。字号与尺寸都走共享几何。
fn text_key(font: &Path, font_px: u32, text: &str) -> TextRasterKey {
    let (width, height) = bitmap_size(640, font_px as f32 * 1.2, font_px);
    TextRasterKey {
        text: text.to_string(),
        x_offset: 0,
        font_px,
        color: [255, 255, 255, 255],
        outline: false,
        stroke_px: 0,
        stroke_color: [0, 0, 0, 255],
        shadow_color: None,
        shadow_blur_px: 0,
        shadow_dx_px: 0,
        shadow_dy_px: 0,
        shadow_pad: 0,
        font_file: font.to_path_buf(),
        width,
        height,
    }
}

#[test]
#[ignore = "需要真 GPU、PATH 上的 ffmpeg 与一个中文字体；跑：cargo test -p dhampir-worker --test text_shadow -- --ignored"]
fn 阴影落在偏移那一侧并且长到字的外面() {
    let fixture = Fixture::new("frame-shadow");
    let line = fixture_timeline(SUBTITLE_LEGACY);
    let shadowed = fixture.render(
        "with-shadow",
        &fixture_timeline(&shadow_fragment()),
        &table(SRT_HERE),
    );
    let text_only = fixture.render("text-only", &line, &table(SRT_HERE));
    let background = fixture.render("background-only", &line, &SubtitleTable::new());

    // 1. 账面上：一行、没问题、**两次栅格化** —— 阴影那张确实也要画一次。
    assert!(
        shadowed.png.issues.is_empty(),
        "画阴影不该有问题：{:?}",
        shadowed.png.issues
    );
    let stats = shadowed.png.overlay;
    assert_eq!(stats.lines_drawn, 1, "这一帧该画一行");
    assert_eq!(stats.lines_failed, 0);
    assert_eq!(
        (stats.cache_hits, stats.cache_misses),
        (0, 2),
        "一行 + 一张阴影：两次未命中（不画阴影时是一次，见反向那条用例）"
    );

    // 2. 字本身的足迹（不带阴影那一帧与底板的差）。
    let (text_pixels, text_bounds) = diff(&background.image, &text_only.image);
    let text_bounds = text_bounds.unwrap_or_else(|| panic!("字幕一个像素都没画"));
    assert!(text_pixels > 30, "只画了 {text_pixels} 个像素，不像一行字");

    // 3. **影子的足迹**：带阴影那一帧与不带的那一帧的差。
    let (shadow_pixels, shadow_bounds) = diff(&text_only.image, &shadowed.image);
    let shadow_bounds =
        shadow_bounds.unwrap_or_else(|| panic!("加了 shadow_color 却一个像素都没变 —— 影子没画"));
    println!(
        "字 {text_bounds:?}（{text_pixels} 像素）；影子足迹 {shadow_bounds:?}（{shadow_pixels} 像素）"
    );

    // **往下长**：dy = +10px，所以影子的足迹必须比字本身更低。
    assert!(
        shadow_bounds.3 > text_bounds.3,
        "影子没有往下长：影子底边 {}、字底边 {}（dy = +10 应当更低）",
        shadow_bounds.3,
        text_bounds.3
    );
    // **往上不许冒出去**：dy = +10、模糊 6px，影子的上边最多回到字的上边附近。
    assert!(
        shadow_bounds.1 + 2 >= text_bounds.1,
        "影子往上冒出去了：影子顶边 {}、字顶边 {} —— 偏移方向反了？",
        shadow_bounds.1,
        text_bounds.1
    );
    // 影子确实比字宽出一圈（模糊的扩散），而不是与字一模一样的一层。
    assert!(
        shadow_pixels > 200,
        "影子只改了 {shadow_pixels} 个像素，太少了（模糊 6px 的影子上百像素起）"
    );

    // 4. **带模糊**（帧上那条）：沿一条竖线看影子的边缘是不是渐变。
    //
    // 取字的横向中心那一列，从「字底边之下」开始往下走：那里的差值就是影子自己的
    // 覆盖度。硬阴影会是一个**台阶**（从 0 直接跳到深色），模糊的是一条**斜坡**
    // （多个不同的差值）。
    let column = ((text_bounds.0 + text_bounds.2) / 2) as usize;
    let mut levels: BTreeSet<i32> = BTreeSet::new();
    for y in (text_bounds.3 + 1)..=shadow_bounds.3 {
        let at = (y as usize * TARGET.0 as usize + column) * 4;
        let before = i32::from(text_only.image.pixels[at]);
        let after = i32::from(shadowed.image.pixels[at]);
        let delta = (after - before).abs();
        if delta > 0 {
            levels.insert(delta);
        }
    }
    println!("字底边之下那一列的差值档数：{}（{levels:?}）", levels.len());
    assert!(
        levels.len() >= 3,
        "影子边缘的亮度只有 {} 档 —— 那是硬台阶，不是模糊（gblur 没生效？）",
        levels.len()
    );
}

/// **模糊那一条直接量位图**：从墨迹上边界往上 3 行，硬阴影是全 0，模糊的是一圈渐变。
///
/// 帧上判会被背景搅浑，而"`gblur` 写了但没生效"恰恰是这一项最可能的坏法 ——
/// 它只改一点点像素，肉眼几乎看不出来（所以它必须有一条**专门的**判据）。
#[test]
#[ignore = "需要真 GPU、PATH 上的 ffmpeg 与一个中文字体；跑：cargo test -p dhampir-worker --test text_shadow -- --ignored"]
fn 阴影位图真的被模糊了() {
    let font = font_file();
    let key = text_key(&font, 40, SUBTITLE_TEXT);
    // 同一行字的两张影子：一张硬（blur = 0）、一张糊（blur = 12）。
    let hard = rasterize_line(&shadow_key(&key, [0, 0, 0, 255], 0, 0, 8)).expect("硬阴影要画得出来");
    let blurred =
        rasterize_line(&shadow_key(&key, [0, 0, 0, 255], 12, 0, 8)).expect("模糊阴影要画得出来");

    let hard_levels = alpha_levels(&hard).len();
    let blurred_levels = alpha_levels(&blurred).len();
    let hard_ink = hard.ink_bounds().expect("硬阴影有墨");
    let blurred_ink = blurred.ink_bounds().expect("模糊阴影有墨");
    println!(
        "硬阴影 {hard_levels} 档 alpha、墨迹 {}x{}；模糊阴影 {blurred_levels} 档 alpha、墨迹 {}x{}（位图 {}x{} vs {}x{}）",
        hard_ink.2 - hard_ink.0 + 1,
        hard_ink.3 - hard_ink.1 + 1,
        blurred_ink.2 - blurred_ink.0 + 1,
        blurred_ink.3 - blurred_ink.1 + 1,
        hard.width,
        hard.height,
        blurred.width,
        blurred.height
    );
    // ⚠️ **别拿"档数变多"当判据**：CJK 字形自己的抗锯齿就有两百多档
    // （实测硬阴影 233 档、模糊 150 档 —— 模糊之后反而更少，因为大片区域被推到两端）。
    // 我第一版就是这么写的，第一次真跑就红。下面的两条才是这件事的判据：
    // **轮廓胖一圈**（模糊往四周糊出去）+ **轮廓之外是渐变**。
    assert!(
        blurred_levels >= 20,
        "模糊阴影只有 {blurred_levels} 档 alpha：不像一条渐变"
    );
    assert!(
        blurred_ink.2 - blurred_ink.0 >= hard_ink.2 - hard_ink.0 + 6,
        "模糊之后轮廓没变宽（{} vs {}）—— 模糊没生效",
        blurred_ink.2 - blurred_ink.0,
        hard_ink.2 - hard_ink.0
    );
    assert!(
        blurred_ink.3 - blurred_ink.1 >= hard_ink.3 - hard_ink.1 + 6,
        "模糊之后轮廓没变高（{} vs {}）—— 模糊没生效",
        blurred_ink.3 - blurred_ink.1,
        hard_ink.3 - hard_ink.1
    );

    // **最关键的一条**：字形轮廓**之外**那几行。
    //
    // 两张位图都把字画在**画布中心**，所以"同一处"要按中心对齐着找：
    // 硬那张的墨迹上边界就是**字形轮廓**（它没有模糊），拿它当基准，
    // 再换成模糊那张里对应的行号。
    let hard_ink = hard.ink_bounds().expect("硬阴影有墨");
    let offset = hard_ink.1 as i32 - hard.height as i32 / 2;
    let glyph_top = offset + blurred.height as i32 / 2; // 模糊那张里的字形上边界
    let row = glyph_top - 3; // 轮廓**之外**（往上 3 行）
    assert!(row >= 0, "画布太小，取不到轮廓之外的那一行");
    let row_of = |bitmap: &TextBitmap, y: u32| -> Vec<u8> {
        (0..bitmap.width)
            .filter_map(|x| bitmap.pixel(x, y).map(|px| px[3]))
            .filter(|alpha| *alpha > 0)
            .collect()
    };
    let hard_row = row_of(&hard, (offset + hard.height as i32 / 2) as u32 - 3);
    let blur_row = row_of(&blurred, row as u32);
    println!(
        "轮廓外那一行：硬 {} 个有墨像素；模糊 {} 个（{:?}）",
        hard_row.len(),
        blur_row.len(),
        blur_row
    );
    assert!(
        hard_row.is_empty(),
        "硬阴影在字形轮廓之外还有 {} 个像素 —— 那不是硬阴影",
        hard_row.len()
    );
    assert!(
        blur_row.len() >= 8,
        "模糊阴影在字形轮廓之外只有 {} 个像素 —— 模糊没生效（gblur 被吞了？）",
        blur_row.len()
    );
    assert!(
        blur_row.iter().all(|alpha| *alpha < 255),
        "轮廓外居然有实心像素：那是没糊的轮廓，不是渐变"
    );
}

/// **反向用例**：不画阴影时与改动前**逐字节相同**，而且不多起一次栅格化。
///
/// 两个"不画"的写法各来一份，因为它们是两条**不同的入口**：
///   * 老工程：JSON 里**根本没有**那几个键（走 `serde(default)`）；
///   * 显式零：`shadow_color: null` + 三个 0（走"字段在、但说的是不画"）。
///
/// 两者必须给出同一份 PNG，也必须与"字号位数一模一样的那种老工程"相同。
#[test]
#[ignore = "需要真 GPU、PATH 上的 ffmpeg 与一个中文字体；跑：cargo test -p dhampir-worker --test text_shadow -- --ignored"]
fn 不画阴影时像素逐字节不变() {
    let fixture = Fixture::new("frame-no-shadow");
    let subtitles = table(SRT_HERE);
    let legacy = fixture.render("legacy", &fixture_timeline(SUBTITLE_LEGACY), &subtitles);
    let zeroed = fixture.render("zeroed", &fixture_timeline(SUBTITLE_ZEROED), &subtitles);

    assert_eq!(
        legacy.png.digest, zeroed.png.digest,
        "「没有那几个键」与「显式写零」必须给出同一份像素（既有工程逐字节不变）"
    );
    println!("不画阴影的两种写法像素摘要一致：{}", legacy.png.digest);

    // 而且**不该多起一次栅格化**：阴影那张位图根本不存在。
    assert_eq!(
        (legacy.png.overlay.cache_hits, legacy.png.overlay.cache_misses),
        (0, 1),
        "一行字只该有一次未命中 —— 多出来的那次就是为阴影起的 ffmpeg"
    );
    assert_eq!(
        (zeroed.png.overlay.cache_hits, zeroed.png.overlay.cache_misses),
        (0, 1),
        "显式写零也走同一条老路"
    );
    assert!(
        legacy.png.issues.is_empty() && zeroed.png.issues.is_empty(),
        "不画阴影不该记问题"
    );
}

/// 与 `tests/overlay.rs` 同一条纪律：`#[ignore]` 的文案里必须带**本文件**的跑法
/// （守卫按文案找用例，文案写错就没人跑得起来）。
#[test]
fn ignore_文案与跑法说明一致() {
    let source =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/text_shadow.rs"))
            .expect("读得回本文件");
    for line in source.lines().filter(|line| line.starts_with("#[ignore")) {
        assert!(
            line.contains("cargo test -p dhampir-worker --test text_shadow -- --ignored"),
            "ignore 文案里没有跑法：{line}"
        );
    }
    assert!(RUNNER.contains("cargo test -p dhampir-worker --test text_shadow -- --ignored"));
}
