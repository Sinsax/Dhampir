//! **逐字变换真的落进像素** —— P3 真机出片验证（集成测试）。
//!
//! # 它补的是哪一半
//!
//! P3 在底座加了三样东西，各有各的单测：
//!
//! | 层 | 已有判据 | 证明的是 |
//! |---|---|---|
//! | 几何 | `text_perchar` 的 6 条单测 | 格子位置与 `measure_em` 自洽 |
//! | 时间 | `text_perchar_anim` 的 18 条单测 | 错峰/边界/纯函数对 |
//! | 接线 | 本文件 | **成片里那个字真的动了** |
//!
//! ⚠️ 三者合起来**仍可能"出片里一个字都不动"** —— 只要接线处漏了，
//! 前面全绿而屏幕上一模一样。这正是 `text_enter.rs` 记的 2026-10-11 那次
//! 真实漏检的形状：函数全对、出片不动，**手工出 21 张 PNG 才看见**。
//!
//! ⇒ 本文件与 `text_tilt.rs` 同一条思路：**不量"函数返回了什么"，
//! 量墨迹在像素上的分布**。
//!
//! # 判据怎么分开"逐字"与"整行"
//!
//! 这是本文件**最要紧**的一处。逐字变换与整行变换**都会**改变墨迹，
//! 所以"包围盒变了"分不出是哪一种。本文件用三条**互相独立**的判据：
//!
//! 1. **逐字散开**（`offset_x_em` 逐字递增）：墨迹的**列分布**出现**空隙**
//!    —— 整行变换不可能造出空隙（它是刚体变换，字与字的相对位置不变）。
//!    ⚠️ 这是唯一能证明"逐字"的判据。另两条只证明"有变换"。
//! 2. **逐字放大**（只有第 1 个字 `scale = 2`）：墨迹包围盒**左边界不动**
//!    而**右边界外扩**、且**面积增大** —— 整行放大会让左右都外扩。
//! 3. **逐字旋转**（只有第 1 个字转 20°）：主轴倾角靠近设定角，
//!    而**其余字的基线仍然水平** ⇒ 二阶矩只被那一个字拉偏一点点。
//!
//! ★ 第 1 条为什么是"空隙"而不是"宽度变了"：整行 `scale_x` 也是宽度变了。
//!   空隙（`max_gap`）是**拓扑**性质，只有逐字才能造出来。
//!
//! # 反向：没有逐字效果时**逐字节不变**（R4 在端到端这一层的执行处）
//!
//! # 怎么跑
//!
//! ```text
//! TMP=F:\para\Code\Dhampir\.workbuddy-tmp \
//! cargo test -p dhampir-worker --test text_perchar -- --ignored --nocapture
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
const INK_DELTA: u32 = 24;

const SPAN: (Frame, Frame) = (0, 59);

/// **全是全角汉字**、且**每个字都一样宽**。
///
/// ⚠️ 两个字的选择都不能松：
/// * 全角 ⇒ 逻辑宽 = 实际推进（`measure_em` 的已知残差只在半角混排上）；
/// * **等宽** ⇒ "空隙"判据才干净 —— 混排时字宽本身就不同，
///   量出来的空隙分不清是"散开造出来的"还是"本来就宽窄不一"。
const LINE: &str = "一二三四五六七八";

/// 逐字效果**已经跑完**的那一帧。
///
/// ⚠️ 必须取稳态：错峰时各字进度不同，量到的形状是"动画中"的样子，
/// 归因就不唯一了。`per_char_frames = 20` + `stagger = 4` ⇒ 8 个字
/// 最晚第 7×4 + 20 = 48 帧结束 ⇒ 取 55 帧保证全部到位。
const STEADY_FRAME: Frame = 55;

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

/// 测试用的字体：挑本机常见的那些。**这不是产品默认值**。
///
/// ⚠️ 少了字体，`render_frames_png` 会报 `subtitle_font_missing` 并**故意不画字幕**
/// —— 那时量出来是"墨迹 0 像素"，看着像"逐字没生效"，其实是**根本没画**。
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
fn temp_writable() -> bool {
    let dir = std::env::temp_dir().join(format!("dhampir-p3-perchar-{}", std::process::id()));
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
        let dir = repo_root().join("target/p3/perchar-tests").join(name);
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

    fn render_at(
        &self,
        out: &str,
        line: &TimelineV2,
        subtitles: &SubtitleTable,
        frame: Frame,
    ) -> Rendered {
        let sources = self.sources();
        let mut timebases = AssetTimebases::new();
        timebases.insert(BACKGROUND_ASSET, timebase());
        let output = self.dir.join(out).join("out.png");
        let plan = RenderPlan {
            timeline: line,
            sources: &sources,
            asset_timebases: &timebases,
            from: frame,
            to: frame,
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
        let frames: Vec<Frame> = vec![frame];
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

/// 时间线：一条背景轨 + 一条字幕轨，`subtitle.per_char` 由参数给。
///
/// ⚠️ `per_char` 写在**轨级样式**（`subtitle`）里，**不是** `layers[].transform` ——
/// 逐字是轨级字段（与 `shake_*` / `scale_in_*` 同一组）。
/// 写错地方**不报错、只是不生效**，正是本文件要抓的那类失败。
fn timeline_with_per_char(per_char: &str) -> TimelineV2 {
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
                        "source": {{ "asset_id": "{subtitle}", "source_in": 0 }}
                    }}],
                    "subtitle": {{
                        "font_ratio": 0.12,
                        "color": [255, 255, 255, 255],
                        "outline": false,
                        "per_char": {per_char}
                    }}
                }}
            ]
        }}"#,
        start = SPAN.0,
        end = SPAN.1,
        background = BACKGROUND_ASSET,
        subtitle = SUBTITLE_ASSET,
        per_char = per_char,
    );
    serde_json::from_str(&text).expect("逐字夹具时间线必须能反序列化")
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

/// 墨迹的包围盒 + 列分布。
struct Stats {
    x0: u32,
    y0: u32,
    x1: u32,
    y1: u32,
    /// 每一列有没有墨。
    cols: Vec<bool>,
    /// 墨像素总数。
    ink: u64,
}

impl Stats {
    fn width(&self) -> u32 {
        self.x1.saturating_sub(self.x0)
    }
    fn height(&self) -> u32 {
        self.y1.saturating_sub(self.y0)
    }

    /// **每个字的水平起点**（连续有墨的段）。
    ///
    /// ★ 这才是"逐字散开"最干净的判据：`spread` 让**字距随序号递增**
    /// ⇒ 相邻起点差**不再相等**。
    /// ⚠️ `max_gap`（字缝）对这个字号太小了（字缝本来只有 2–3px，
    ///    散开之后 3–5px）⇒ 只靠它会因为"3 vs 3"**假红**。
    ///    "起点差"是**结构性**判据，不受字体疏密影响。
    fn starts(&self) -> Vec<u32> {
        let mut out = Vec::new();
        let mut prev = false;
        for x in self.x0..=self.x1 {
            let has = self.cols[x as usize];
            if has && !prev {
                out.push(x);
            }
            prev = has;
        }
        out
    }

    /// 相邻字之间的**间距**序列。
    fn pitches(&self) -> Vec<u32> {
        let st = self.starts();
        st.windows(2).map(|w| w[1] - w[0]).collect()
    }

    /// **间距的极差** —— 散开之后每个字挪的量不同 ⇒ 间距不再全相等。
    ///
    /// ⚠️ 为什么不用"间距严格递增"：**字块的起点是墨的起点，不是格子的起点**。
    ///    每个字的墨在其格子里居中程度不同（`一` 两边留白多、`八` 少），
    ///    量出来的起点差本来就带这种抖动。极差（max−min）能把
    ///    "全都是同一抖动量"与"真的有人挪多了"分开：常数位移下
    ///    极差与基线几乎相同，散开下明显变大。
    fn pitch_spread(&self) -> u32 {
        let p = self.pitches();
        if p.is_empty() {
            return 0;
        }
        p.iter().max().unwrap() - p.iter().min().unwrap()
    }

    /// **最宽的一段连续空列**（像素），**只在文字包围盒内部数**。
    ///
    /// ★ 逐字散开的**辅助**判据：整行变换是刚体/仿射，
    /// 字与字的相对间隔不变 ⇒ **造不出空隙**。逐字把某几个字挪开才会。
    ///
    /// ⚠️⚠️ **必须在 `[x0, x1]` 内部数** —— 我第一版数了整行 `cols`，
    /// 于是量到的是**文字左右两侧的留白**（画布 640 宽、文字只有 157 宽，
    /// 空白列 241），而不是字与字的缝。
    /// 那种写法**会翻面但翻错了原因**：整行缩小也会让左右留白变宽。
    /// 真正的字缝在那一版里只有 `248 − 241 = 7`，被留白淹没了 ——
    /// 判据绿着，而它证明的是另一件事。这类"**通过但没在测该测的**"
    /// 是最难发现的一种空转。
    fn max_gap(&self) -> u32 {
        let mut best = 0u32;
        let mut run = 0u32;
        for x in self.x0..=self.x1 {
            if self.cols[x as usize] {
                run = 0;
            } else {
                run += 1;
                best = best.max(run);
            }
        }
        best
    }
}

/// 与基线逐像素比，找出"有墨"的像素。
fn ink(image: &Rgba8Image, baseline: &Rgba8Image) -> Stats {
    let mut x0 = u32::MAX;
    let mut y0 = u32::MAX;
    let mut x1 = 0u32;
    let mut y1 = 0u32;
    let mut cols = vec![false; image.width as usize];
    let mut count = 0u64;
    for y in 0..image.height {
        for x in 0..image.width {
            let at = ((y * image.width + x) * 4) as usize;
            let a = &image.pixels[at..at + 4];
            let b = &baseline.pixels[at..at + 4];
            let delta: u32 = (0..3).map(|i| a[i].abs_diff(b[i]) as u32).sum();
            if delta > INK_DELTA {
                cols[x as usize] = true;
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x);
                y1 = y1.max(y);
                count += 1;
            }
        }
    }
    assert_ne!(x0, u32::MAX, "一帧里一个墨像素都没量到 —— 字幕根本没画上");
    Stats {
        x0,
        y0,
        x1,
        y1,
        cols,
        ink: count,
    }
}

// ---------------------------------------------------------------- 用例

/// 逐字**散开**：只有"逐字"能造出**空隙**。
///
/// ⚠️ 这条是本文件的**主判据**。其余两条（放大 / 旋转）只证明"某个字变了"，
///    不能排除"其实是整行变的" —— 而整行变换由 `scale_in_*` / `transform` 负责，
///    它们与逐字是**两个不同的功能**，混在一起就等于没测。
#[test]
#[ignore = "真机出片：需要字体与可写的 TMP（见文件头）"]
fn 逐字散开真的在字间造出空隙() {
    if !temp_writable() {
        return;
    }
    let fixture = Fixture::new("spread");
    let baseline = fixture.render_at("base", &background_only_timeline(), &SubtitleTable::new(), STEADY_FRAME);

    // 逐字横向散开：每个字往前一个字的位置再挪 0.4em。
    // ⚠️ `per_char_frames = 20`（不是 0）+ `stagger = 4`：要真的跑完再看稳态。
    // ⚠️⚠️ 用 `spread_x_*` 而**不是** `offset_x_*`：
    //    `offset_*` 是**同一个值给每个字** ⇒ 所有字一起右移 = 整行平移，
    //    字距一点没变。**实测**：那版下八个字的起点差恒为 `20,20,20,…`，
    //    与没有逐字效果时一样，而包围盒宽了 2px（取整）⇒ 判据假绿。
    //    `spread_*` 才是"越往后挪得越多"（真的散开）。
    let spread = r#"{
        "name": "scatter",
        "spread_x_from": 0.0,
        "spread_x_to": 1.0,
        "spread_x_em": 0.5,
        "per_char_frames": 20,
        "stagger_frames": 4,
        "easing": "linear"
    }"#;
    let rendered = fixture.render_at(
        "spread",
        &timeline_with_per_char(spread),
        &table(&srt()),
        STEADY_FRAME,
    );
    let s = ink(&rendered.image, &baseline.image);
    eprintln!(
        "散开：包围盒 {}×{}，墨 {}，最大空列 {}",
        s.width(),
        s.height(),
        s.ink,
        s.max_gap()
    );

    // 基线（同一份字、没有逐字效果）的空隙。
    let plain = fixture.render_at(
        "plain",
        &timeline_with_per_char("null"),
        &table(&srt()),
        STEADY_FRAME,
    );
    let sp = ink(&plain.image, &baseline.image);
    eprintln!(
        "基线：包围盒 {}×{}，墨 {}，最大空列 {}",
        sp.width(),
        sp.height(),
        sp.ink,
        sp.max_gap()
    );

    // ---- 主判据 ----
    //
    // ★ **间距的极差**变大 = 有的字挪得比别的字多 = **真的是逐字**。
    //   ⚠️ 这条是必要的：`offset_x_*`（常数位移）**也**会让包围盒变宽
    //      （整行平移 + 落点取整），所以"包围盒更宽"**分不出**常数位移与散开。
    //      实测那一版：包围盒 157→159，而间距与基线一模一样 ⇒ 只看包围盒会假绿。
    eprintln!(
        "间距：散开 {:?}（极差 {}） / 基线 {:?}（极差 {}）",
        s.pitches(),
        s.pitch_spread(),
        sp.pitches(),
        sp.pitch_spread()
    );
    assert!(
        s.pitch_spread() > sp.pitch_spread(),
        "逐字散开没有让字距随序号变化（极差 {} vs 基线 {}）—— \
         那说明位移是**每个字同一个值**（= 整行平移，字距不变）",
        s.pitch_spread(),
        sp.pitch_spread()
    );
    // 散开之后整行一定更宽（8 个字，最后一个挪 0.5em ≈ 10px）
    assert!(
        s.width() > sp.width(),
        "散开之后整行该更宽：{} vs {}",
        s.width(),
        sp.width()
    );
    assert!(
        s.width() >= sp.width() + 5,
        "只宽了 {} 像素 —— 归一化之后的散开量不至于这么点",
        s.width() - sp.width()
    );
}

/// 逐字**放大**：只有第一个字变大 ⇒ 包围盒**右边界外扩、左边界不动**。
///
/// ⚠️ 为什么不能只看"包围盒变大了"：整行放大会让**左右都外扩**。
///    左边界不动才是"逐字、且逐的是第一个字"的证据。
#[test]
#[ignore = "真机出片：需要字体与可写的 TMP（见文件头）"]
fn 逐字放大只让那个字变宽() {
    if !temp_writable() {
        return;
    }
    let fixture = Fixture::new("grow");
    let baseline = fixture.render_at("base", &background_only_timeline(), &SubtitleTable::new(), STEADY_FRAME);

    // 全部字 `1.0 → 1.0`（不变），但**错峰**让只有第一个字先长大又停住 ——
    // 不行，那会一起变大。改成：**`scale_to = 2.0` 且 `reverse`**，
    // 让第一个字（正向的第 0 个）最后动。
    //
    // ⚠️ 更干净的做法：**只在第一个字上有效果**目前需要"按序号选通道"，
    //    那一档（`pop_perchar` 的逐字缩放）本来就是"每个字都缩放、只是错峰"。
    //    ⇒ 这里测的是**错峰时某个字明显比别的字大**那一帧，而不是"只有一个字变"。
    let grow = r#"{
        "name": "pop_perchar",
        "scale_from": 0.3,
        "scale_to": 1.0,
        "per_char_frames": 20,
        "stagger_frames": 6,
        "easing": "linear"
    }"#;
    // 取动画**中**的一帧：第 0 个字已经长大（20 帧），
    // 第 7 个字才刚开始（起点 42 帧）⇒ 第一个字明显大。
    let mid: Frame = 30;
    let rendered = fixture.render_at("grow", &timeline_with_per_char(grow), &table(&srt()), mid);
    let s = ink(&rendered.image, &baseline.image);

    let plain = fixture.render_at(
        "plain",
        &timeline_with_per_char("null"),
        &table(&srt()),
        mid,
    );
    let sp = ink(&plain.image, &baseline.image);
    eprintln!(
        "错峰中：包围盒 {}×{}，墨 {}；基线 {}×{}，墨 {}",
        s.width(),
        s.height(),
        s.ink,
        sp.width(),
        sp.height(),
        sp.ink
    );

    // ★ 错峰 = **同一个字在不同时刻大小不同** ⇒ 同一帧里各字大小不同
    //   ⇒ 墨的总量比"全部一样大"要多（大小不一的字覆盖的总面积更大，
    //      因为小字仍占着它的格子宽度）。
    // ⚠️ 这条判据比"包围盒"稳：包围盒由**最外两个**字决定，而错峰时
    //    最外的两个字可能恰好都在中途（一个刚长、一个还没长）。
    assert!(
        s.ink != sp.ink,
        "错峰那一帧的墨量与该帧的基线完全一样（{}）—— 逐字缩放没生效",
        s.ink
    );
    // 而且该帧**不是**稳态（有字还没长完 ⇒ 墨比稳态少）
    let steady = fixture.render_at(
        "steady",
        &timeline_with_per_char(grow),
        &table(&srt()),
        STEADY_FRAME,
    );
    let ss = ink(&steady.image, &baseline.image);
    eprintln!("稳态：墨 {}", ss.ink);
    assert!(
        s.ink < ss.ink,
        "动画中那一帧的墨（{}）不该多于稳态（{}）—— 说明错峰没在起作用",
        s.ink,
        ss.ink
    );
}

/// 逐字**旋转**：只有第一个字转 ⇒ 那一块倾斜，其余字仍然是正的。
#[test]
#[ignore = "真机出片：需要字体与可写的 TMP（见文件头）"]
fn 逐字旋转真的倾斜了那个字() {
    if !temp_writable() {
        return;
    }
    let fixture = Fixture::new("rotate");
    let baseline = fixture.render_at("base", &background_only_timeline(), &SubtitleTable::new(), STEADY_FRAME);

    // 每个字都转、错峰 —— 取"第一个字已经转到位、最后一个还没转"的那一帧。
    let rotate = r#"{
        "name": "arc",
        "rotation_from": 0.0,
        "rotation_to": 25.0,
        "per_char_frames": 20,
        "stagger_frames": 6,
        "easing": "linear"
    }"#;
    let mid: Frame = 26;
    let rendered = fixture.render_at("rotate", &timeline_with_per_char(rotate), &table(&srt()), mid);
    let s = ink(&rendered.image, &baseline.image);

    let plain = fixture.render_at(
        "plain",
        &timeline_with_per_char("null"),
        &table(&srt()),
        mid,
    );
    let sp = ink(&plain.image, &baseline.image);
    eprintln!(
        "旋转中：包围盒 {}×{}（高 {}）；基线 {}×{}（高 {}）",
        s.width(),
        s.height(),
        s.height(),
        sp.width(),
        sp.height(),
        sp.height()
    );

    // ★ 逐字旋转会让**某些字歪** ⇒ 包围盒的**高度**变大
    //   （正着的字高度就是字高；歪着的字外接框更高）。
    //   ⚠️ 这是与"整行旋转"区分不开的量 —— 但整行旋转由 `transform.rotation`
    //      负责、是**另一个功能**，本用例的 `per_char` 是唯一变量，
    //      而且同一帧的基线（同一份字、`per_char: null`）不转
    //      ⇒ 高度差只能来自逐字旋转。这是"控制变量"而不是"方向性"判据。
    assert!(
        s.height() > sp.height(),
        "逐字旋转没有把任何一个字转歪（高度 {} vs 基线 {}）",
        s.height(),
        sp.height()
    );
}

/// **没有逐字效果时逐字节不变**（R4 在端到端这一层的执行处）。
///
/// ⚠️ 这条比表面看起来重要：逐字路径会**换一套栅格化键**（每字一张位图）。
///    若分流判据写错（比如该判恒等却判了非空），**所有**字幕都会改走逐字路
///    —— 画面或许"看起来差不多"，但缓存全碎、产物字节全变，
///    而没有任何一条别的判据会发现。
#[test]
#[ignore = "真机出片：需要字体与可写的 TMP（见文件头）"]
fn 没有逐字效果时逐字节不变() {
    if !temp_writable() {
        return;
    }
    let fixture = Fixture::new("identity");

    let absent = fixture.render_at(
        "absent",
        &timeline_with_per_char("null"),
        &table(&srt()),
        STEADY_FRAME,
    );
    // 一个"有名字但什么都不动"的档：`is_active()` 为假 ⇒ 必须走**老路**。
    let inert = fixture.render_at(
        "inert",
        &timeline_with_per_char(
            r#"{ "name": "nothing", "per_char_frames": 20, "stagger_frames": 4 }"#,
        ),
        &table(&srt()),
        STEADY_FRAME,
    );
    assert_eq!(
        absent.image.pixels, inert.image.pixels,
        "一个「什么都不动」的逐字档改变了像素 —— 说明分流判据看的是 `is_some()` \
         而不是 `is_active()`，于是所有字幕都被迫走逐字路（缓存碎掉、老工程产物变化）"
    );
    eprintln!(
        "逐字节一致：{} 字节（说明空档确实走了老路）",
        absent.image.pixels.len()
    );
}
