//! 文字**倾斜 / 非等比压扁**真的落进像素 —— T5 真机出片验证（集成测试）。
//!
//! # 它补的是哪一半
//!
//! T1（契约层）、T2（CPU 仿射）、T3（YekiTrim 转译）、T4（前端预览）各有判据，
//! 但**没有任何一条证明"形变真的改变了成片像素"**：
//!
//! | 层 | 已有判据 | 证明的是 |
//! |---|---|---|
//! | T1 | 字段被填进 `TextItem` | 值**到达**了求值层 |
//! | T2 | `blit_affine` 的函数级单测 | **函数**行为对 |
//! | T4 | `check-text-tilt.mjs` | 预览 CSS 矩阵 == **公式** |
//!
//! ⚠️ 三段合起来**仍可能"出片里文字一动不动"** —— 只要接线处漏了，
//! 三个判据全绿而屏幕上没变化。`text_enter.rs` 头部记的就是 2026-10-11
//! 那次真实漏检：函数全对、出片不动，是**手工出 21 张 PNG** 才看见的。
//!
//! ⇒ **本文件把"形变落到像素"钉死**，与那份同一条思路：
//! 量**墨迹的包围盒与二阶矩**，不量"函数返回了什么"。
//!
//! # 判据怎么量
//!
//! | 量什么 | 怎么算 | 倾斜 | 压扁 |
//! |---|---|---|---|
//! | 主轴倾角 | 二阶中心矩 `θ = ½·atan2(2μ11, μ20−μ02)` | ≈ 设定角 | ≈ 0 |
//! | 包围盒高度 | `y1 − y0` | 变大 | **变小** |
//! | 包围盒宽度 | `x1 − x0` | 变大 | **基本不变** |
//!
//! ★ 为什么不能只看"包围盒变了"：倾斜与压扁**都会**改变包围盒，
//! 那一条分不出是哪种。**倾角**是方向性判据，**宽度不变**是"非等比"的判据 ——
//! 少了宽度那条，"压扁"与"整条等比缩小"分不开（而那是另一个效果，`scale`）。
//!
//! # 反向：恒等时**逐字节不变**（R4 在端到端这一层的执行处）
//!
//! # 怎么跑
//!
//! ```text
//! TMP=F:\para\Code\Dhampir\.workbuddy-tmp \
//! cargo test -p dhampir-worker --test text_tilt -- --ignored
//! ```
//!
//! ⚠️ **`TMP` 不是可选的**：Windows 上 Rust 的 `std::env::temp_dir()` 读 `TMP`，
//! **忽略 `TMPDIR` 与 `TEMP`**。给错了不会报错，而是走"跳过"分支报
//! **`0 passed` 且 `0.00s`** —— 一条都没跑却看着像绿的。
//! **唯一一眼能分辨的是耗时**：真跑是秒级。

use std::path::{Path, PathBuf};

use dhampir_core::overlay::SubtitleTable;
use dhampir_core::readback::Rgba8Image;
use dhampir_core::timeline::layer::{AssetTimebases, TimelineV2};
use dhampir_core::timeline::schema::{Frame, TimebaseDto};
use dhampir_core::timeline::subtitle::parse_srt;
use dhampir_worker::pipeline::{
    AudioMode, FramePng, RenderPlan, SourceTable, render_frames_png,
};

const TARGET: (u32, u32) = (640, 360);
const BACKGROUND_ASSET: &str = "bg.png";
const SUBTITLE_ASSET: &str = "sub.srt";

/// 一帧里"有墨"的判据：与**无字幕**基线逐像素比，差得够多就算画上去的。
///
/// 与 `text_enter.rs` 的 `INK_DELTA` 同值、同一条理由（背景与白字 alpha 都是 255，
/// 只能靠"与基线不同"分开）。
const INK_DELTA: u32 = 24;

/// 字幕覆盖的帧区间。形变是**静态**的，一帧就够。
const SPAN: (Frame, Frame) = (0, 59);

/// 取这一帧：`text_enter.rs` 的夹具里进场动效窗口约 10 帧，
/// 取 40 帧（0.67s）保证**稳态** —— 否则量到的变化里混着进场缩放，归因就不唯一了。
const STEADY_FRAME: Frame = 40;

/// 用的那段字。**够宽**（倾斜/压扁才有可测面积）且全是全角汉字
/// —— 半角混排的逻辑宽是估算（`text_layout.rs::measure_em` 的已知残差），
/// 这里不去碰那条残差。
const LINE: &str = "一二三四五六七八";

fn timebase() -> TimebaseDto {
    TimebaseDto { num: 60, den: 1 }
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("manifest 一定在 crates/<名字> 里")
        .to_path_buf()
}

/// 测试用的字体：挑本机常见的那些。**这不是产品默认值**（与 `text_plate` 同款）。
///
/// ⚠️ 少了字体，`render_frames_png` 会报 `subtitle_font_missing` 并**故意不画字幕**
/// —— 那时量出来是"墨迹 0 像素"，看着像"形变没生效"，其实是**根本没画**。
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

/// 本环境能不能写临时目录 —— 不能就**如实跳过**，而不是报"实现坏了"。
/// 判据是**真去试着建一次**，不是嗅探环境变量。
fn temp_writable() -> bool {
    let dir = std::env::temp_dir().join(format!("dhampir-t5-tilt-{}", std::process::id()));
    match std::fs::create_dir_all(&dir) {
        Ok(()) => {
            let _ = std::fs::remove_dir(&dir);
            true
        }
        Err(error) => {
            eprintln!(
                "跳过：这个环境写不了临时目录 {}（{error}）—— 环境限制，不是实现坏了。\
                 绕法：**`TMP`** 指一个仓内目录（⚠️ 只给 TMPDIR/TEMP 没用，见文件头）",
                dir.display()
            );
            false
        }
    }
}

// ---------------------------------------------------------------- 夹具

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
        let dir = repo_root().join("target/t5/text-tilt-tests").join(name);
        std::fs::create_dir_all(&dir).expect("建不了草稿目录");
        let fixture = Self {
            dir,
            font: font_file(),
        };
        fixture.write_background();
        fixture
    }

    fn background(&self) -> PathBuf {
        self.dir.join(BACKGROUND_ASSET)
    }

    /// 背景：640×360 棋盘格。**不能是纯色** —— 纯色场上"字有没有落上去"
    /// 会被"看起来本来就那样"糊过去（与 `text_plate` / `text_shadow` 同一条理由）。
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

    /// 渲染**一帧**（稳态那帧）。
    fn render(
        &self,
        out: &str,
        line: &TimelineV2,
        subtitles: &SubtitleTable,
    ) -> Rendered {
        let sources = self.sources();
        let mut timebases = AssetTimebases::new();
        timebases.insert(BACKGROUND_ASSET, timebase());
        let output = self.dir.join(out).join("out.png");
        let plan = RenderPlan {
            timeline: line,
            sources: &sources,
            asset_timebases: &timebases,
            from: STEADY_FRAME,
            to: STEADY_FRAME,
            width: TARGET.0,
            height: TARGET.1,
            sequence: TARGET,
            background: None,
            subtitles,
            font_file: Some(self.font.as_path()),
            font_bold_file: None,
            font_dir: None,
            chunk_workers: 1,
            audio: AudioMode::Silent,
            output: &output,
        };
        let frames: Vec<Frame> = vec![STEADY_FRAME];
        let written = render_frames_png(&plan, &frames).expect("渲染必须成功");
        assert_eq!(written.len(), 1, "要一帧就该写一帧");
        let png = written.into_iter().next().expect("有一帧");
        let bytes = std::fs::read(&png.path).expect("产出的 PNG 要读得回来");
        let image = Rgba8Image::decode_png(&bytes).expect("产出的 PNG 必须是 8 位 RGBA");
        assert_eq!((image.width, image.height), TARGET, "PNG 尺寸不对");
        Rendered { png, image }
    }
}

/// **只有背景轨、没有字幕轨** —— 量墨迹的基线。
///
/// # 为什么必须有它（而不是"拿自己当基线"）
///
/// `ink()` 的判据是"与基线不一样的像素"。若把基线设成**同一张图**，
/// 差值恒为 0 ⇒ 一条墨迹都数不出来，判据**空转**。
/// 基线必须是**同一帧、同一条时间线、但不画字**的那一份。
fn background_only_timeline() -> TimelineV2 {
    let text = format!(
        r#"{{
            "schema": 3,
            "timebase": {{ "num": 60, "den": 1 }},
            "tracks": [{{
                "id": "v1",
                "kind": "video",
                "layers": [{{
                    "id": "bg",
                    "start": {start},
                    "end": {end},
                    "source": {{ "asset_id": "{background}", "source_in": 0 }}
                }}]
            }}]
        }}"#,
        start = SPAN.0,
        end = SPAN.1,
        background = BACKGROUND_ASSET,
    );
    serde_json::from_str(&text).expect("无字幕基线时间线必须能反序列化")
}

/// 时间线：一条背景轨 + 一条字幕轨。`transform` 接在**字幕图层**里。
///
/// ⚠️ 形变写在 `layers[].transform`，**不是** `subtitle`（轨级样式）：
/// `evaluate_overlay` 读的是 element 的 transform。写错地方**不报错、只是不生效**
/// —— 这正是本文件要抓的那类失败。
fn fixture_timeline(transform: &str) -> TimelineV2 {
    let text = format!(
        r#"{{
            "schema": 3,
            "timebase": {{ "num": 60, "den": 1 }},
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
                        "source": {{ "asset_id": "{subtitle}", "source_in": 0 }},
                        "transform": {transform}
                    }}],
                    "subtitle": {{
                        "font_ratio": 0.055,
                        "color": [255, 255, 255, 255],
                        "outline": false
                    }}
                }}
            ]
        }}"#,
        start = SPAN.0,
        end = SPAN.1,
        background = BACKGROUND_ASSET,
        subtitle = SUBTITLE_ASSET,
        transform = transform,
    );
    serde_json::from_str(&text).expect("夹具时间线必须能反序列化")
}

fn table(srt: &str) -> SubtitleTable {
    let report = parse_srt(srt).expect("夹具 SRT 必须能解析");
    assert_eq!(report.skipped, 0, "夹具 SRT 不该有看不懂而跳过的条目");
    let mut table = SubtitleTable::new();
    table.insert(SUBTITLE_ASSET.to_string(), report.cues);
    table
}

fn srt() -> String {
    format!("1\n00:00:00,000 --> 00:00:00,983\n{LINE}\n")
}

// ---------------------------------------------------------------- 量法

/// 墨迹的一阶/二阶矩。
struct Stats {
    x0: u32,
    y0: u32,
    x1: u32,
    y1: u32,
    mu20: f64,
    mu11: f64,
    mu02: f64,
    count: u64,
}

impl Stats {
    fn width(&self) -> u32 {
        self.x1 - self.x0 + 1
    }
    fn height(&self) -> u32 {
        self.y1 - self.y0 + 1
    }
    /// 主轴倾角（度）。`θ = ½·atan2(2μ11, μ20−μ02)`。
    ///
    /// **方向性**判据：包围盒大小对"倾斜"与"压扁"都敏感，
    /// 而倾角只对旋转敏感 —— 这是把两者分开的钥匙。
    fn angle_deg(&self) -> f64 {
        (0.5 * (2.0 * self.mu11).atan2(self.mu20 - self.mu02)).to_degrees()
    }
}

/// 与**无字幕基线**逐像素比，取"画上去的墨"的包围盒与二阶中心矩。
fn stats(image: &Rgba8Image, baseline: &Rgba8Image) -> Option<Stats> {
    assert_eq!(
        (image.width, image.height),
        (baseline.width, baseline.height),
        "两张图尺寸不同，比不了"
    );
    let mut x0 = u32::MAX;
    let mut y0 = u32::MAX;
    let mut x1 = 0u32;
    let mut y1 = 0u32;
    let mut count = 0u64;
    let (mut sx, mut sy) = (0f64, 0f64);

    for y in 0..image.height {
        for x in 0..image.width {
            let at = ((y * image.width + x) * 4) as usize;
            let a = &image.pixels[at..at + 4];
            let b = &baseline.pixels[at..at + 4];
            let delta: u32 = (0..3)
                .map(|i| (a[i] as i32 - b[i] as i32).unsigned_abs())
                .sum();
            if delta > INK_DELTA {
                count += 1;
                sx += x as f64;
                sy += y as f64;
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x);
                y1 = y1.max(y);
            }
        }
    }
    if count == 0 {
        return None;
    }

    let cx = sx / count as f64;
    let cy = sy / count as f64;
    let (mut mu20, mut mu11, mut mu02) = (0f64, 0f64, 0f64);
    for y in 0..image.height {
        for x in 0..image.width {
            let at = ((y * image.width + x) * 4) as usize;
            let a = &image.pixels[at..at + 4];
            let b = &baseline.pixels[at..at + 4];
            let delta: u32 = (0..3)
                .map(|i| (a[i] as i32 - b[i] as i32).unsigned_abs())
                .sum();
            if delta > INK_DELTA {
                let dx = x as f64 - cx;
                let dy = y as f64 - cy;
                mu20 += dx * dx;
                mu11 += dx * dy;
                mu02 += dy * dy;
            }
        }
    }

    Some(Stats {
        x0,
        y0,
        x1,
        y1,
        mu20,
        mu11,
        mu02,
        count,
    })
}

/// 恒等 transform。**显式写全**（含新键 `scale_y`），与 `LEGACY` 分开 ——
/// 两条路都要求逐字节相同。
const IDENTITY: &str =
    r#"{ "x": 0.0, "y": 0.0, "scale": 1.0, "rotation": 0.0, "scale_y": 1.0 }"#;

/// **老工程**：`transform` 里**没有** `scale_y` 这个键。
///
/// ⚠️ `rotation` **必须留着**：它是接线前就有的字段，且**没有 `serde(default)`**
/// （`layer.rs:154` —— 只有 `scale_y` 有，见 `:151`）。省掉它是**反序列化失败**，
/// 不是"老工程"。第一版就是这么写的，红在 `missing field rotation` —— 记在这里。
///
/// ★ 于是这条判据真正测的是：**磁盘上没有 `scale_y` 键的老工程**
/// 与"显式写 `scale_y: 1.0`"给出同一份像素 —— 即 R4 在端到端这一层的执行处。
const LEGACY: &str = r#"{ "x": 0.0, "y": 0.0, "scale": 1.0, "rotation": 0.0 }"#;

// ---------------------------------------------------------------- 判据

/// 恒等形变必须与老工程**逐字节相同**（R4 在端到端这一层的执行处）。
#[test]
#[ignore = "需要真 GPU + 字体；用 --ignored 跑，并设 TMP（见文件头）"]
fn t5_恒等形变与老工程逐字节相同() {
    if !temp_writable() {
        return;
    }
    let fixture = Fixture::new("identity");
    let base = progress();

    let expl = fixture.render("flat", &fixture_timeline(IDENTITY), &table(&srt()));
    let legacy = fixture.render("legacy", &fixture_timeline(LEGACY), &table(&srt()));

    assert_eq!(
        expl.image.pixels, legacy.image.pixels,
        "显式恒等形变与老工程（无新键）必须逐字节相同"
    );

    // 与**无字幕**基线比：确认这一帧真的画了字（否则"逐字节相同"是空转的）。
    let bare = fixture.render("bare", &background_only_timeline(), &table(&srt()));
    let ink = stats(&expl.image, &bare.image).expect("这一帧必须画出了字");
    eprintln!(
        "t5 恒等：显式 vs 老工程逐字节相同（{} 字节）；\
         墨迹 {}×{} / {} 像素；耗时 {:.1}s",
        expl.image.pixels.len(),
        ink.width(),
        ink.height(),
        ink.count,
        base.elapsed().as_secs_f64()
    );
}

/// ★ **倾斜真的落进像素**：主轴倾角 ≈ 设定角（方向性判据）。
#[test]
#[ignore = "需要真 GPU + 字体；用 --ignored 跑，并设 TMP（见文件头）"]
fn t5_倾斜真的落进像素() {
    if !temp_writable() {
        return;
    }
    let fixture = Fixture::new("tilt");
    let base = progress();
    let bare = fixture.render("bare", &background_only_timeline(), &table(&srt()));

    let flat = fixture.render("flat", &fixture_timeline(IDENTITY), &table(&srt()));
    let tilted = fixture.render(
        "tilt",
        &fixture_timeline(
            r#"{ "x": 0.0, "y": 0.0, "scale": 1.0, "rotation": 12.0, "scale_y": 1.0 }"#,
        ),
        &table(&srt()),
    );

    let base_ink = stats(&flat.image, &bare.image)
        .expect("恒等那一帧必须画出了字（否则是**字体**或**没画**，不是形变）");
    let got = stats(&tilted.image, &bare.image).expect("倾斜那一帧必须画出了字");

    // ① 反向先立：恒等那份的倾角应接近 0。它若也有明显倾角，说明**量法有偏**，
    //    拿它当基线就不成立 —— 所以这条必须**排在正向之前**。
    let flat_angle = base_ink.angle_deg();
    assert!(
        flat_angle.abs() < 3.0,
        "恒等那一帧的倾角应接近 0，实得 {flat_angle:.2}° —— \
         它若也明显倾斜，说明是**量法有偏**，正向判据不能信"
    );

    // ② 正向：倾角应与设定的 12° 同号、同量级。
    //    容差给得宽 —— 实心字块的二阶矩受笔画分布影响。这条判的是**方向与量级**；
    //    精确性由 T2 的数学单测与 T4 的矩阵判据负责。
    let angle = got.angle_deg();
    assert!(
        angle > 4.0,
        "倾角应显著为正（设定 12°），实得 {angle:.2}° —— \
         0 附近说明**形变没有落进像素**（接线处漏了），而不是'角度算错'"
    );
    assert!(angle < 45.0, "倾角 {angle:.2}° 远超设定值 12°，量级不对");

    // ③ 包围盒：斜着撑开 ⇒ 高度明显变大。
    assert!(
        got.height() > base_ink.height(),
        "倾斜后包围盒高度应变大：恒等 {} → 倾斜 {}",
        base_ink.height(),
        got.height()
    );

    eprintln!(
        "t5 倾斜：设定 12.0° → 反解 {angle:.2}°（恒等基线 {flat_angle:.2}°）；\
         包围盒 {}×{} → {}×{}；墨迹 {} → {} 像素；耗时 {:.1}s",
        base_ink.width(),
        base_ink.height(),
        got.width(),
        got.height(),
        base_ink.count,
        got.count,
        base.elapsed().as_secs_f64()
    );
}

/// ★ **压扁真的落进像素**：高度显著变小、**宽度几乎不变**、倾角不变。
#[test]
#[ignore = "需要真 GPU + 字体；用 --ignored 跑，并设 TMP（见文件头）"]
fn t5_压扁真的落进像素() {
    if !temp_writable() {
        return;
    }
    let fixture = Fixture::new("squash");
    let base = progress();
    let bare = fixture.render("bare", &background_only_timeline(), &table(&srt()));

    let flat = fixture.render("flat", &fixture_timeline(IDENTITY), &table(&srt()));
    let squashed = fixture.render(
        "squash",
        &fixture_timeline(
            r#"{ "x": 0.0, "y": 0.0, "scale": 1.0, "rotation": 0.0, "scale_y": 0.5 }"#,
        ),
        &table(&srt()),
    );

    let base_ink = stats(&flat.image, &bare.image).expect("恒等那一帧必须画出了字");
    let got = stats(&squashed.image, &bare.image).expect("压扁那一帧必须画出了字");

    // ① 高度显著变小。设 0.5 ⇒ 期望约一半；容差留给抗锯齿与阈值边缘。
    let ratio = got.height() as f64 / base_ink.height() as f64;
    assert!(
        ratio < 0.75,
        "压扁 0.5 后高度比应显著小于 1，实得 {ratio:.3}（恒等 {} → 压扁 {}）—— \
         接近 1 说明**形变没有落进像素**（接线处漏了）",
        base_ink.height(),
        got.height()
    );
    assert!(ratio > 0.30, "高度比 {ratio:.3} 过小 —— 压得太狠或量到了别的东西");

    // ② 宽度**基本不变**：这是"非等比"（而非整体缩小）的**唯一**判据。
    //    ⚠️ 少了它，"压扁"与"整条等比变小"分不开 —— 后者是另一个效果（`scale`）。
    let width_ratio = got.width() as f64 / base_ink.width() as f64;
    assert!(
        (0.85..=1.15).contains(&width_ratio),
        "非等比压扁**不该改变宽度**：恒等 {} → 压扁 {}（比 {width_ratio:.3}）",
        base_ink.width(),
        got.width()
    );

    // ③ 倾角不变（纵轴缩放不旋转主轴）。
    let base_angle = base_ink.angle_deg();
    let angle = got.angle_deg();
    assert!(
        (angle - base_angle).abs() < 3.0,
        "压扁不该改变倾角：恒等 {base_angle:.2}° → 压扁 {angle:.2}°"
    );

    eprintln!(
        "t5 压扁：设定 scale_y=0.5 → 高度比 {ratio:.3}、宽度比 {width_ratio:.3}、\
         倾角 {base_angle:.2}° → {angle:.2}°；墨迹 {} → {} 像素；耗时 {:.1}s",
        base_ink.count,
        got.count,
        base.elapsed().as_secs_f64()
    );
}

/// **倾斜 + 压扁同时给**：两个自由度互不吞掉。
///
/// # ⚠️ 这一条第一版判错了 —— 记下来免得下一个人再踩
///
/// 第一版用**填充率**（墨迹像素 / 包围盒面积）当"压扁也生效了"的判据，
/// 理由是"压扁让笔画更密"。**推理错在忽略倾斜对包围盒的放大**：
///
/// ```text
/// 实测恒等 158×20，倾斜 10° 后包围盒涨到约 161×48
/// ⇒ 面积 ×3.9，而压扁只把墨迹 ×0.6 ⇒ 填充率必然**下降**（实测 0.32 → 0.19）
/// ```
///
/// ⇒ 判据方向反了。**正确做法是拿"纯倾斜"当对照**，而不是拿恒等当对照：
/// 叠加档的**包围盒高度**必须显著小于纯倾斜档 —— 这一条**只**由压扁贡献，
/// 因为两档的旋转相同。这样"只生效了旋转"（高度≈纯倾斜）才会被抓到。
#[test]
#[ignore = "需要真 GPU + 字体；用 --ignored 跑，并设 TMP（见文件头）"]
fn t5_倾斜与压扁可叠加() {
    if !temp_writable() {
        return;
    }
    let fixture = Fixture::new("both");
    let base = progress();
    let bare = fixture.render("bare", &background_only_timeline(), &table(&srt()));

    // ★ 三档：恒等 / **纯倾斜**（对照组）/ 倾斜+压扁。
    //   纯倾斜那一档是本条判据的**钥匙** —— 叠加档只多了一个压扁。
    let flat = fixture.render("flat", &fixture_timeline(IDENTITY), &table(&srt()));
    let tilt_only = fixture.render(
        "tilt-only",
        &fixture_timeline(
            r#"{ "x": 0.0, "y": 0.0, "scale": 1.0, "rotation": 10.0, "scale_y": 1.0 }"#,
        ),
        &table(&srt()),
    );
    let both = fixture.render(
        "both",
        &fixture_timeline(
            r#"{ "x": 0.0, "y": 0.0, "scale": 1.0, "rotation": 10.0, "scale_y": 0.6 }"#,
        ),
        &table(&srt()),
    );

    let base_ink = stats(&flat.image, &bare.image).expect("恒等那一帧必须画出了字");
    let tilted = stats(&tilt_only.image, &bare.image).expect("纯倾斜那一帧必须画出了字");
    let got = stats(&both.image, &bare.image).expect("叠加那一帧必须画出了字");

    // ① 旋转那半边：叠加档与纯倾斜档的倾角应当**接近**（同一个 10°）。
    let tilt_angle = tilted.angle_deg();
    let angle = got.angle_deg();
    assert!(
        angle > 3.0,
        "叠加档的倾角应显著为正，实得 {angle:.2}°（纯倾斜档 {tilt_angle:.2}°）"
    );
    assert!(
        (angle - tilt_angle).abs() < 5.0,
        "两档旋转相同（都 10°），倾角该接近：纯倾斜 {tilt_angle:.2}° vs 叠加 {angle:.2}°"
    );

    // ② 压扁那半边：拿**纯倾斜**当对照 —— 高度差只可能来自压扁。
    let height_ratio = got.height() as f64 / tilted.height() as f64;
    assert!(
        height_ratio < 0.80,
        "叠加档高度应显著小于**纯倾斜**档（两档旋转相同，差只来自压扁 0.6）：\
         纯倾斜 {} → 叠加 {}（比 {height_ratio:.3}）—— \
         约等于 1 说明**只生效了旋转、压扁被吞了**",
        tilted.height(),
        got.height()
    );

    // ③ 反向：叠加档的**宽度**仍应与纯倾斜档接近（压扁是纵轴的事）。
    let width_ratio = got.width() as f64 / tilted.width() as f64;
    assert!(
        (0.80..=1.20).contains(&width_ratio),
        "压扁不该改变宽度（对照纯倾斜档）：{} → {}（比 {width_ratio:.3}）",
        tilted.width(),
        got.width()
    );

    eprintln!(
        "t5 叠加：倾角 {}° → {angle:.2}°（同 10°）；高度 纯倾斜 {} → 叠加 {}（比 {height_ratio:.3}）；\
         宽度 {} → {}（比 {width_ratio:.3}）；恒等墨迹 {} 像素；耗时 {:.1}s",
        tilt_angle,
        tilted.height(),
        got.height(),
        tilted.width(),
        got.width(),
        base_ink.count,
        base.elapsed().as_secs_f64()
    );
}

/// 形变**不碰文字带以外**：背景必须一动不动。
///
/// ⚠️ 防的是"实现里顺手动了别的层"（例如把变换矩阵用到整帧坐标上）。
#[test]
#[ignore = "需要真 GPU + 字体；用 --ignored 跑，并设 TMP（见文件头）"]
fn t5_形变不碰文字带以外() {
    if !temp_writable() {
        return;
    }
    let fixture = Fixture::new("scope");
    let base = progress();
    let bare = fixture.render("bare", &background_only_timeline(), &table(&srt()));

    let flat = fixture.render("flat", &fixture_timeline(IDENTITY), &table(&srt()));
    let tilted = fixture.render(
        "tilt",
        &fixture_timeline(
            r#"{ "x": 0.0, "y": 0.0, "scale": 1.0, "rotation": 12.0, "scale_y": 1.0 }"#,
        ),
        &table(&srt()),
    );

    let ink = stats(&flat.image, &bare.image).expect("恒等那一帧必须画出了字");
    let (w, h) = (flat.image.width, flat.image.height);
    let margin = 40u32;
    let (tx0, ty0) = (ink.x0.saturating_sub(margin), ink.y0.saturating_sub(margin));
    let (tx1, ty1) = ((ink.x1 + margin).min(w - 1), (ink.y1 + margin).min(h - 1));

    let mut outside = 0u64;
    for y in 0..h {
        for x in 0..w {
            if (tx0..=tx1).contains(&x) && (ty0..=ty1).contains(&y) {
                continue;
            }
            let at = ((y * w + x) * 4) as usize;
            if flat.image.pixels[at..at + 4] != tilted.image.pixels[at..at + 4] {
                outside += 1;
            }
        }
    }

    assert_eq!(
        outside, 0,
        "文字带 ({tx0},{ty0})-({tx1},{ty1}) **之外**有 {outside} 个像素被改了 —— \
         形变只该影响文字自己"
    );
    eprintln!(
        "t5 范围：文字带外改动像素 = 0（带 = ({tx0},{ty0})-({tx1},{ty1})）；耗时 {:.1}s",
        base.elapsed().as_secs_f64()
    );
}

/// 计时起点。**它是"真跑了还是静默跳过"的唯一一眼判据**：
/// 跳过时这里报 `0.0s`，真跑是秒级（见文件头）。
fn progress() -> std::time::Instant {
    std::time::Instant::now()
}
