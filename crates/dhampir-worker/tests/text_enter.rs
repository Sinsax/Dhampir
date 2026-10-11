//! 文字动效（T1 弹入 / T3 强调 / T4 抖动）**真的落进像素**的真机验证（集成测试）。
//!
//! # 它补的是哪一半 —— 这一条是被一次真实的漏检逼出来的
//!
//! 2026-10-11：T1 的函数级判据全绿（`text_motion::text_envelope_scaled` 返回的
//! `scale` 逐值正确），**而出片里文字一帧都不动**。根因在**调用方**：
//!
//! ```text
//! 渲染侧字号来自 place_line(rect, target, style.font_ratio)   ← 轨道级
//! 而逐条的缩放只落在 stroke_px 上（stroke_px * scale）
//! ```
//!
//! 于是 `scale` 被算出来、传下去、然后**在离画面最近的那一步被丢掉**。
//! 当时是手工出 21 张 PNG、逐帧量文字外接框才看见的（全部 102×20，比值恒 1.0000）。
//!
//! ⇒ **这个文件就是把那次手工探查固化下来。** 不固化的话，下次重构
//! 还会以同样的方式悄悄坏掉 —— 而单测**永远发现不了**，因为它们测的是
//! "函数返回了什么"，不是"这个值有没有被画到屏幕上"。
//!
//! # 判据怎么量
//!
//! 每个用例渲染**同一段文字的两份配置**（关 / 开），然后量
//! **文字墨迹的外接框**（alpha 阈值以上的包围盒）与**质心**：
//!
//! | 动效 | 量什么 | 期望 |
//! |---|---|---|
//! | T1 弹入 | 逐帧外接框宽度 | 先 `<1`（起点 0.7）→ `>1`（过冲）→ 回 `1.0` |
//! | T4 抖动 | 逐帧质心位移 | 两轴**各自**变号、幅度有界、**换相位轨迹不同** |
//! | T3 强调 | 外接框高度 | 命中那条**变大**（`cue.scale > 1`） |
//!
//! 用**外接框**而不是整幅差分：背景是棋盘格，按墨迹包围盒量出来的结论
//! 与背景内容无关（与 `text_plate` 同一条思路）。
//!
//! # 反向：全部缺省时**一个字节都不变**
//!
//! `None` 与"老工程（JSON 里根本没有那六个键）"必须给出**同一份像素**。
//! 这是 T1/T3/T4 的存量零回归判据在**端到端**这一层的执行处 ——
//! 与 `layer::v2_tests` 的键集判据（JSON 层）、`text_motion` 的单位变换判据
//! （求值层）合起来，三层都有。
//!
//! # 怎么跑
//!
//! 需要真 GPU、PATH 上的 ffmpeg 与一个中文字体，所以默认 `#[ignore]`：
//!
//! ```text
//! TMP=F:\para\Code\Dhampir\.workbuddy-tmp \
//! cargo test -p dhampir-worker --test text_enter -- --ignored
//! ```
//!
//! ⚠️ **那个环境变量不是可选的**：字幕栅格化要往临时目录落一份 `.srt`，
//! 而沙箱化的环境（例如 DSH 的文件沙箱）会让 `%TEMP%` **写不了**
//! （症状是 `subtitle_raster_failed: ... 拒绝访问。(os error 5)`）——
//! 那时整条端到端会因为**环境**失败，与动效一点关系都没有。
//! 指一个**仓内目录**就绕过去了（见上面的命令）。
//!
//! ## ⚠️⚠️ 必须是 `TMP`，**不能只给 `TMPDIR` / `TEMP`**（2026-10-11 实测更正）
//!
//! 本文件用的是 `std::env::temp_dir()`，而**Rust 在 Windows 上读的是 `TMP`**：
//!
//! | 给什么 | `std::env::temp_dir()` 得到 |
//! |---|---|
//! | `TMPDIR=…` | `C:\Users\…\Temp\` ← **完全忽略**（`TMPDIR` 是 unix 的约定） |
//! | `TEMP=…` | `C:\Users\…\Temp\` ← **也被忽略**（优先级低于 `TMP`） |
//! | `TMP=…` | `…\.workbuddy-tmp\` ✅ |
//! | `TEMP=… TMP=…` | `…\.workbuddy-tmp\` ✅（`TMP` 赢） |
//!
//! ★ **只给 `TMPDIR` 的症状极其难认**：测试**不会报错**，而是走
//! [`temp_writable`] 的"跳过"分支 ⇒ **`4 passed / 0 failed` 而耗时 `0.00s`**。
//! **一条都没真跑，却报全绿** —— 与 `#[ignore]` 那类"跳过"是同一个谎。
//! **唯一一眼能分辨的是耗时**：真跑是**秒级**（本文件约 12~14s），跳过是 `0.00s`。
//!
//! ⚠️ 前几版这里写的是 `TMPDIR=…` 打头（照搬 unix 习惯）—— 照着做**仍然会静默跳过**。
//! 留这段是为了让下一个人不再踩（本仓的规矩：坑要写成能拦住下一个人的样子）。
//!
//! 判据本身在 [`temp_writable`]：**真去试着建一次目录**，不是嗅探环境变量。

use std::path::{Path, PathBuf};

use dhampir_core::overlay::SubtitleTable;
use dhampir_core::readback::Rgba8Image;
use dhampir_core::timeline::layer::{AssetTimebases, TimelineV2};
use dhampir_core::timeline::schema::{Frame, TimebaseDto};
use dhampir_core::timeline::subtitle::parse_srt;
use dhampir_worker::pipeline::{AudioMode, FramePng, RenderPlan, SourceTable, render_frames_png};

const TARGET: (u32, u32) = (640, 360);
const BACKGROUND_ASSET: &str = "bg.png";
const SUBTITLE_ASSET: &str = "sub.srt";

/// 一帧里"有墨"的判据。**不看 RGB 只看 alpha**：字幕是不透明白字，
/// 用 alpha 能把"字"与"棋盘格背景"分开 —— 背景的 alpha 恒 255，
/// 所以这里改成"与背景**不同**"才数得准。见 [`ink_bounds`]。
const INK_DELTA: u32 = 24;

/// 60fps：`scale_in_ms = 180` ⇒ 窗口 10.8 帧。
fn timebase() -> TimebaseDto {
    TimebaseDto { num: 60, den: 1 }
}

/// 字幕覆盖的帧区间。给足长度，好让"窗口内"与"窗口外"都能取到样。
const SPAN: (Frame, Frame) = (0, 119);

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("manifest 一定在 crates/<名字> 里")
        .to_path_buf()
}

/// 测试用的字体：挑本机常见的那些。**这不是产品默认值**（与 `text_plate` 同款）。
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
///
/// 与 `text_plate::temp_writable` 同一条纪律：沙箱化的环境会让字幕栅格化
/// 写不了 `.srt`，那时失败的是**环境**。判据是**真去试着建一次**，不是嗅探变量。
fn temp_writable() -> bool {
    let dir = std::env::temp_dir().join(format!("dhampir-t14-{}", std::process::id()));
    match std::fs::create_dir_all(&dir) {
        Ok(()) => {
            let _ = std::fs::remove_dir(&dir);
            true
        }
        Err(error) => {
            eprintln!(
                "跳过：这个环境写不了临时目录 {}（{error}）—— 环境限制，不是实现坏了。\
                 绕法：**`TMP`** 指一个仓内目录（⚠️ 只给 TMPDIR/TEMP 没用，见本文件头）",
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
        let dir = repo_root().join("target/t14/text-enter-tests").join(name);
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

    /// 渲染 `from..=to` 一串帧。
    fn render_range(
        &self,
        out: &str,
        line: &TimelineV2,
        subtitles: &SubtitleTable,
        from: Frame,
        to: Frame,
    ) -> Vec<Rendered> {
        let sources = self.sources();
        let mut timebases = AssetTimebases::new();
        timebases.insert(BACKGROUND_ASSET, timebase());
        let output = self.dir.join(out).join("out.png");
        let plan = RenderPlan {
            timeline: line,
            sources: &sources,
            asset_timebases: &timebases,
            from,
            to,
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
        let frames: Vec<Frame> = (from..=to).collect();
        let written = render_frames_png(&plan, &frames).expect("渲染必须成功");
        assert_eq!(written.len(), frames.len(), "要了几帧就该写几帧");
        written
            .into_iter()
            .map(|png| {
                let bytes = std::fs::read(&png.path).expect("产出的 PNG 要读得回来");
                let image = Rgba8Image::decode_png(&bytes).expect("产出的 PNG 必须是 8 位 RGBA");
                assert_eq!((image.width, image.height), TARGET, "PNG 尺寸不对");
                Rendered { png, image }
            })
            .collect()
    }
}

/// **只有背景轨、没有字幕轨** —— 量墨迹的基线。
///
/// # 为什么必须有它（而不是"拿自己当基线"）
///
/// [`ink`] 的判据是"与基线不一样的那些像素"。若把基线设成**同一张图**，
/// 差值恒为 0 ⇒ 一条墨迹都数不出来，判据**空转**（第一次跑就是这么红的）。
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

/// 时间线：一条背景轨 + 一条字幕轨。`extra` 接在 `subtitle` 的方括号里。
fn fixture_timeline(extra: &str) -> TimelineV2 {
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
                        "font_ratio": 0.055,
                        "color": [255, 255, 255, 255],
                        "outline": false{extra}
                    }}
                }}
            ]
        }}"#,
        start = SPAN.0,
        end = SPAN.1,
        background = BACKGROUND_ASSET,
        subtitle = SUBTITLE_ASSET,
        extra = extra,
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

/// 墨迹的外接框与质心：与**背景**逐像素比，差得够多就算"这一笔是画的"。
///
/// # 为什么不看 alpha
///
/// 背景与文字**两条路的 alpha 都是 255**（棋盘格不透明、白字不透明），
/// 所以 alpha 分不开它们。判据改成"与同一帧的**无字幕**基线不一样" ——
/// 于是量出来的就是**这一份动效自己画上去的东西**，与背景内容无关。
fn ink(image: &Rgba8Image, baseline: &Rgba8Image) -> Option<(u32, u32, u32, u32, f64, f64)> {
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
    let mut sx = 0f64;
    let mut sy = 0f64;
    for y in 0..image.height {
        for x in 0..image.width {
            let at = ((y * image.width + x) * 4) as usize;
            let a = &image.pixels[at..at + 4];
            let b = &baseline.pixels[at..at + 4];
            let delta: u32 = (0..3).map(|i| (a[i] as i32 - b[i] as i32).unsigned_abs()).sum();
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
    Some((
        x0,
        y0,
        x1,
        y1,
        sx / count as f64,
        sy / count as f64,
    ))
}

fn width_of(bounds: (u32, u32, u32, u32, f64, f64)) -> u32 {
    bounds.2 - bounds.0 + 1
}

/// 一条字幕，覆盖整个 [`SPAN`]（4 秒）。
const SRT_FULL: &str = "1\n00:00:00,000 --> 00:00:01,983\n第一行中文\n";

// =========================================================================
// T1 弹入
// =========================================================================

/// **T1：弹入真的改变逐帧像素** —— 先缩、后过冲、再回到 1.0。
///
/// 这条就是"手工量 21 张 PNG"那次的固化。它盯的是**外接框宽度**：
/// 字号没乘上缩放时，这一列数字会**整列恒等于基线**（历史症状）。
#[test]
#[ignore = "需要真 GPU / ffmpeg / 字体；用 --ignored 跑，并设 TMP（见文件头，只给 TMPDIR 会静默跳过）"]
fn t1_弹入逐帧先缩后过冲再回落() {
    if !temp_writable() {
        return;
    }
    let fx = Fixture::new("t1");
    let table = table(SRT_FULL);

    // **两个参照，不能混**：
    //   `bg`   = 同一帧**没有字幕**（量墨迹的基线）
    //   `base` = 有字幕、但**一个动效字段都不写**（量"正常大小"的参照）
    let bg_tl = background_only_timeline();
    let base_tl = fixture_timeline("");
    // 开启：0.7 -> 1.15 -> 1.0，窗口 180ms @60fps = 10.8 帧。
    let on_tl = fixture_timeline(r#","scale_in_from":0.7,"scale_in_peak":1.15,"scale_in_ms":180"#);

    let from = SPAN.0;
    let to = SPAN.0 + 20;
    let bg = fx.render_range("t1_bg", &bg_tl, &table, from, to);
    let base = fx.render_range("t1_base", &base_tl, &table, from, to);
    let on = fx.render_range("t1_on", &on_tl, &table, from, to);

    let mut widths: Vec<(Frame, u32, u32)> = Vec::new();
    for (i, ((b, o), g)) in base.iter().zip(on.iter()).zip(bg.iter()).enumerate() {
        let frame = from + i as Frame;
        let bb = ink(&b.image, &g.image).expect("基线里必须有字（不然这条判据空转）");
        let ob = ink(&o.image, &g.image).expect("开启后更该有字");
        widths.push((frame, width_of(bb), width_of(ob)));
    }

    let base_w = widths[0].1;
    assert!(base_w > 0, "基线宽度量成了 0，判据空转");

    // ① 起点（第 0 帧）应当**明显小于**基线 —— `scale_in_from = 0.7`。
    let (f0, _, o0) = widths[0];
    let ratio0 = o0 as f64 / base_w as f64;
    assert!(
        ratio0 < 0.85,
        "第 {f0} 帧应当是「缩着」出场的（≈0.7 倍），实测 {o0}/{base_w} = {ratio0:.4}。\
         若恒等于 1.0，说明**缩放没有落进字号**（字号走的是轨道级 font_ratio，不读逐条 scale）"
    );

    // ② 窗口内必须出现过冲（**宽于**基线）。
    let peak = widths
        .iter()
        .take(11)
        .map(|&(_, bw, ow)| ow as f64 / bw as f64)
        .fold(f64::MIN, f64::max);
    assert!(
        peak > 1.05,
        "窗口内必须出现过冲（宽于基线），实测峰值比 {peak:.4}"
    );

    // ③ 窗口过了（>10.8 帧）必须回到**基线宽度**。
    for &(frame, bw, ow) in widths.iter().filter(|&&(f, _, _)| f >= 12) {
        assert_eq!(
            ow, bw,
            "第 {frame} 帧已过窗口，宽度必须回到基线 {bw}，实测 {ow}"
        );
    }
}

// =========================================================================
// T4 抖动
// =========================================================================

/// **T4：抖动真的位移** —— 两轴各自变号、幅度有界、换相位轨迹不同。
#[test]
#[ignore = "需要真 GPU / ffmpeg / 字体；用 --ignored 跑，并设 TMP（见文件头，只给 TMPDIR 会静默跳过）"]
fn t4_抖动两轴各自变号且换相位轨迹不同() {
    if !temp_writable() {
        return;
    }
    let fx = Fixture::new("t4");
    let table = table(SRT_FULL);
    let from = SPAN.0;
    let to = SPAN.0 + 40;

    // `bg` 是同一帧**没有字幕**的那份 —— 墨迹的判据全靠它。
    let bg_tl = background_only_timeline();
    let base_tl = fixture_timeline("");
    let a_tl = fixture_timeline(r#","shake_px":14.0,"shake_hz":18.0,"shake_phase":0.0"#);
    let b_tl = fixture_timeline(r#","shake_px":14.0,"shake_hz":18.0,"shake_phase":1.9"#);

    let bg = fx.render_range("t4_bg", &bg_tl, &table, from, to);
    let base = fx.render_range("t4_base", &base_tl, &table, from, to);
    let a = fx.render_range("t4_a", &a_tl, &table, from, to);
    let b = fx.render_range("t4_b", &b_tl, &table, from, to);

    let mut dxs = Vec::new();
    let mut dys = Vec::new();
    let mut differing = 0usize;
    for (i, ((bs, as_), bs2)) in base.iter().zip(a.iter()).zip(b.iter()).enumerate() {
        let _frame = from + i as Frame;
        let g = &bg[i].image;
        let base_bounds = ink(&bs.image, g).expect("基线里必须有字");
        let ia = ink(&as_.image, g).expect("开抖动后要有字");
        let ib = ink(&bs2.image, g).expect("开抖动后要有字");

        // 位移 = 这一帧的墨迹质心 − 同帧基线的墨迹质心。
        dxs.push(ia.4 - base_bounds.4);
        dys.push(ia.5 - base_bounds.5);

        // 换相位之后轨迹变没变。
        if (ia.4 - ib.4).abs() > 0.5 || (ia.5 - ib.5).abs() > 0.5 {
            differing += 1;
        }
    }

    // ① 两轴各自都要正负都出现过（**单向漂移不是抖**）。
    let has_pos = |v: &[f64]| v.iter().any(|&x| x > 0.5);
    let has_neg = |v: &[f64]| v.iter().any(|&x| x < -0.5);
    assert!(has_pos(&dxs), "横向必须出现过正向位移：{dxs:?}");
    assert!(has_neg(&dxs), "横向必须出现过负向位移：{dxs:?}");
    assert!(has_pos(&dys), "纵向必须出现过正向位移：{dys:?}");
    assert!(has_neg(&dys), "纵向必须出现过负向位移：{dys:?}");

    // ② 幅度有界（抖动是"抖"，不是"飘走"）。
    let max_dx = dxs.iter().fold(0f64, |m, &v| m.max(v.abs()));
    let max_dy = dys.iter().fold(0f64, |m, &v| m.max(v.abs()));
    assert!(
        max_dx <= 14.0 + 2.0 && max_dy <= 14.0 + 2.0,
        "位移不能超过 shake_px=14（正负两条各占一部分，留 2px 取整余量）：dx={max_dx:.1} dy={max_dy:.1}"
    );

    // ③ **换相位必须给出不同的轨迹** —— 相同的话同屏多条会整体同拍。
    assert!(
        differing > 30,
        "两个相位只有 {differing}/{} 帧不同 —— 相位没生效，或同屏多条会一起晃",
        base.len()
    );
}

// =========================================================================
// 存量零回归（端到端这一层）
// =========================================================================

/// **全部缺省 ⇒ 逐字节等于老工程** —— 存量零回归在端到端这一层的执行处。
///
/// 与另外两层合起来构成完整的三层：
/// * `layer::v2_tests` —— **JSON 键**一个都不多；
/// * `text_motion` —— 缺省时求值是**单位变换**；
/// * **这一条** —— 出片**逐字节相同**。
#[test]
#[ignore = "需要真 GPU / ffmpeg / 字体；用 --ignored 跑，并设 TMP（见文件头，只给 TMPDIR 会静默跳过）"]
fn 全部缺省时逐字节等于老工程() {
    if !temp_writable() {
        return;
    }
    let fx = Fixture::new("legacy");
    let table = table(SRT_FULL);
    let from = SPAN.0;
    let to = SPAN.0 + 20;

    // 老工程：那六个键**一个都没有**。
    let legacy = fixture_timeline("");
    // 把六个键**显式写出来**，值取"缺省应当解析到的那些"。
    let explicit = fixture_timeline(
        r#","scale_in_from":1.0,"scale_in_peak":1.0,"scale_in_ms":0,"shake_px":0.0,"shake_hz":18.0,"shake_phase":0.0"#,
    );

    let a = fx.render_range("legacy_absent", &legacy, &table, from, to);
    let b = fx.render_range("legacy_explicit", &explicit, &table, from, to);

    for (i, (x, y)) in a.iter().zip(b.iter()).enumerate() {
        assert_eq!(
            x.image.pixels, y.image.pixels,
            "第 {} 帧：缺省与显式单位值必须给出同一份像素（存量零回归）",
            from + i as Frame
        );
    }
}
