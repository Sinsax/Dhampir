//! 宿主那一半：把共享布局算出来的那几行字**画进帧里**。
//!
//! # 分工（别把这三件事混起来）
//!
//! * 共享布局（`dhampir-timeline::text_layout`）决定**要画什么**：几行、每行什么文本、
//!   占哪个归一化行盒。两端必须一致，所以它在契约层、是纯函数、零依赖。
//! * 栅格化（[`crate::text_raster`]）把一行文字变成一张 RGBA8 位图。两端各做各的：
//!   本机走 ffmpeg drawtext，浏览器走 canvas。**字形像素允许不同**。
//! * 叠上去（本模块）是「把那张位图按行盒放进目标像素」这一步。它同样两端各有一份，
//!   但判据是同一条：位图中心对准行盒中心（见下）。
//!
//! # 几何：规则在契约层，这里只调用
//!
//! 「行盒放在目标像素的哪个位置」**不在这里**：它住在
//! `dhampir_timeline::text_layout`（`place_line` / `LinePlacement`），与浏览器
//! 那半共用一份。两端各写一份落点算术，就一定会漂 —— 而漂了以后两边各自
//! 都是「自洽」的，只有把两张画面摆在一起才看得出来。
//!
//! 规则本身（位图中心对准行盒中心、字号由行盒高反推）的推导写在共享层的文档里，
//! 这里不再抄一遍。
//!
//! # 混合：直排 alpha 的 source-over
//!
//! 位图是直排 RGBA8（`text_raster` 的契约），帧缓冲也是直排（`readback` 的契约），
//! 所以就是教科书式的 source-over：
//!
//! ```text
//! out.rgb = src.rgb × src.a + dst.rgb × (1 - src.a)
//! out.a   = src.a + dst.a × (1 - src.a)
//! ```
//!
//! 全不透明的像素直接搬字节（不做乘除）—— 白字就还是精确的白，不会因为取整差 1。
//!
//! # 判据：三条问题 + 一条事实
//!
//! | 情况 | 落地成什么 | 为什么 |
//! |---|---|---|
//! | 有字要画，宿主没有字体 | 问题 `subtitle_font_missing` | 画不出来就是画不出来。**不能**静默出一份没有字幕的片子 —— 那正是这套东西要消灭的失效模式 |
//! | ffmpeg 画不出这一行 | 问题 `subtitle_raster_failed` | 同上；消息带 ffmpeg 的原文 |
//! | 墨迹被切（顶到位图边界，或有墨像素落在画面外） | 问题 `subtitle_ink_clipped` | 结构说这行放得下、像素说被切了，两者不一致就必须红 |
//! | 超过 max_lines 被丢掉的行 | **事实**：只计数、出声，不判失败 | 它是样式里写明的上限（不是渲染失败），计数在 [`OverlayStats::lines_dropped`] 与 CLI 的 stderr 提示里 |
//!
//! `subtitle_ink_clipped` 值不值得红，是量出来决定的，不是口味：
//! 实测（`plan/measurements.md` 那批字）真字体比共享布局的按字宽分类模型**宽 3%~25%**
//! （CJK +11%、小写拉丁 +5%~25%）。布局按模型的宽度换行，于是「模型说刚好放得下、
//! 真字体放不下」是可达的（模型 88% 宽的行 × 1.25 = 110%）。
//! 那种片子里的字是真的被画面边缘切掉了，属于「看起来成功、其实不对」——
//! 所以判失败，并把数字写进消息里让人能直接改。
//!
//! 反过来，丢行**不**判失败：`max_lines` 是样式里明写的上限，五行的字幕被压到两行
//! 是它自己的规则在起作用。判失败会把「按样式办事」变成失败，而失败一旦变得常见，
//! 人就开始忽略它。
//!
//! # 弹幕：同一条画法，**不同的判据**
//!
//! 弹幕复用上面那张表的前两行 —— 没有字体、画不出来都是问题，**问题代码也共用**
//! （`subtitle_font_missing` / `subtitle_raster_failed`）：原因与修法是同一件事，
//! 与那串字属于字幕还是弹幕无关。共用而不是另起一组，是为了不让调用方去比对两张表。
//!
//! 但弹幕**不做「墨迹被切就判失败」**：它从右滚到左，出来与离开的路上本来就有大半条
//! 在画面外，那不是「结构说这行放得下、像素说被切了」，而是滚动本身的样子。
//! 照搬字幕那条判据，每一条弹幕都会红 —— 而失败一旦变得常见，人就开始忽略它。
//!
//! 所以弹幕的账**单独分一列**（[`OverlayStats`] 的 `danmaku_*`：画了几条、
//! 因为泳道排不下丢了几条、画不出几条）。分列的理由不是好看：两边的**上限来源不同** ——
//! 字幕丢行来自 `max_lines`（样式里写的），弹幕丢条来自泳道耗尽（素材密度 × `lanes`），
//! 该改的地方一个是样式、一个是泳道参数。合成一个数就查不出是哪一个在丢。
//!
//! # 已知缺口（写下来，不藏着）
//!
//! 一条字幕**全部**超过 max_lines 时（例如 max_lines = 0），`evaluate_overlay` 按契约
//! 返回 `None`（它约定「没有可画的东西」与「没有字幕」同形），于是那份丢弃计数在中途
//! 就没了 —— 这里收不到，也就计不出来。改它要动 core 的返回口径（T2.1 的契约），
//! 不在这一段的范围里，记在 plan/t2-evidence.md 的覆盖边界里。
//!
//! 同一个洞对弹幕有两处入口：一条弹幕轨被泳道**全部**丢掉（`lanes = 0`，或素材密到
//! 一条都排不下），以及弹幕素材解析出来就是空的。这时弹幕那半边是空的，字幕半边也空的话
//! 同样落到 `None`。**同一处契约口径，同一条边界** —— 那边改了这里跟着受益，不另修。

use std::path::{Path, PathBuf};
use std::rc::Rc;

use dhampir_core::overlay::TextOverlay;
use dhampir_core::readback::Rgba8Image;

use crate::pipeline::IssueLog;
use crate::text_raster::{TextBitmap, TextRasterKey, TextRasterizer, shadow_key};

// ---------------------------------------------------------------------------
// 几何：行盒 -> 目标像素里的落点
// ---------------------------------------------------------------------------

// 规则在契约层（两端共用一份），这里只把它们带出来 —— 于是既有调用点
// （`paint_lines`、下面的测试）不用改路径。改规则要去
// crates/dhampir-timeline/src/text_layout.rs，**不许在这里重写一份**。
pub use dhampir_core::timeline::text_layout::{LinePlacement, place_line};

// ---------------------------------------------------------------------------
// 叠上去
// ---------------------------------------------------------------------------

/// 一次叠图的账。两个数都是**有墨的像素**（全透明的不算）：
/// 落在画面里的与落在画面外被丢掉的。后者大于 0 就是「字被切了」。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BlitReport {
    pub written: u64,
    pub skipped: u64,
}

/// 8 位通道乘一个 0..255 的系数，四舍五入。
fn mul_alpha(value: u32, factor: u32) -> u32 {
    (value * factor + 127) / 255
}

/// 把一张直排 RGBA8 位图叠到一帧紧密打包的 RGBA8 上（source-over）。
///
/// 越界的部分丢掉并计数，**不 panic、也不回绕**（回绕会把字画到对面去，
/// 那是比丢掉更坏的一类错：看起来有字，位置全错）。
pub fn blit(
    image: &mut Rgba8Image,
    bitmap: &TextBitmap,
    x: i32,
    y: i32,
) -> Result<BlitReport, String> {
    blit_scaled(image, bitmap, x, y, 255)
}

/// 同 [`blit`]，但整张位图的 alpha 再乘一个系数（`0..=255`）。
///
/// 字幕/弹幕的**淡入淡出**走这条路：契约层算出"这一帧多透明"，
/// 宿主把它交给叠图这一步 —— 而不是把透明度烘进位图缓存
/// （那样每一帧都要重新栅格化一次，而栅格化是起一次 ffmpeg 进程）。
///
/// `scale == 255` 时逐值退化成 [`blit`]，所以老工程逐字节不变。
pub fn blit_scaled(
    image: &mut Rgba8Image,
    bitmap: &TextBitmap,
    x: i32,
    y: i32,
    scale: u32,
) -> Result<BlitReport, String> {
    if scale == 0 {
        // 全透明：一个像素都不写，但**帧缓冲大小仍然要校验**
        // （那是调用方的错，不该因为"这一帧恰好全透明"就静默放过）。
        let expected = image.width as usize * image.height as usize * 4;
        if image.pixels.len() != expected {
            return Err(format!(
                "帧缓冲大小对不上：{}x{} 要 {expected} 字节，实际 {}",
                image.width,
                image.height,
                image.pixels.len()
            ));
        }
        return Ok(BlitReport::default());
    }
    let expected = image.width as usize * image.height as usize * 4;
    if image.pixels.len() != expected {
        return Err(format!(
            "帧缓冲大小对不上：{}x{} 要 {expected} 字节，实际 {}",
            image.width,
            image.height,
            image.pixels.len()
        ));
    }
    let width = image.width as i32;
    let height = image.height as i32;
    let mut report = BlitReport::default();

    for row in 0..bitmap.height {
        let target_y = y + row as i32;
        for column in 0..bitmap.width {
            let source = ((row as usize) * (bitmap.width as usize) + column as usize) * 4;
            let raw_alpha = bitmap.pixels[source + 3] as u32;
            if raw_alpha == 0 {
                continue;
            }
            // 淡入淡出的系数**乘在 alpha 上**（而不是乘 RGB）：
            // 乘 RGB 会让"半透明的白字"变成"不透明的灰字"，
            // 而它在亮背景上是看不见的 —— 正是要避免的那种错。
            let alpha = mul_alpha(raw_alpha, scale);
            if alpha == 0 {
                continue;
            }
            let target_x = x + column as i32;
            if target_x < 0 || target_y < 0 || target_x >= width || target_y >= height {
                report.skipped += 1;
                continue;
            }
            let destination =
                ((target_y as usize) * (image.width as usize) + target_x as usize) * 4;
            if alpha == 255 {
                // 全不透明的墨：直接搬字节。省一次乘除，也保证「白字是精确的白」。
                image.pixels[destination..destination + 4]
                    .copy_from_slice(&bitmap.pixels[source..source + 4]);
            } else {
                let inverse = 255 - alpha;
                for channel in 0..3 {
                    let source_channel = bitmap.pixels[source + channel] as u32;
                    let destination_channel = image.pixels[destination + channel] as u32;
                    image.pixels[destination + channel] = (mul_alpha(source_channel, alpha)
                        + mul_alpha(destination_channel, inverse))
                        as u8;
                }
                let destination_alpha = image.pixels[destination + 3] as u32;
                // 上界：alpha + inverse = 255（mul_alpha 的取整不会超过它乘的那个系数）。
                image.pixels[destination + 3] =
                    (alpha + mul_alpha(destination_alpha, inverse)) as u8;
            }
            report.written += 1;
        }
    }
    Ok(report)
}

// ---------------------------------------------------------------------------
// 一帧一帧地画：缓存与计数在这里
// ---------------------------------------------------------------------------

/// 这一趟里文字覆盖层都发生了什么。**是事实，不是判据** —— 判据在 [`IssueLog`] 里。
///
/// 分开写是有意的：「画了几行、切了几行、丢了几行」与「哪些情况算失败」
/// 是两件事。前者进报告，后者进问题清单，而 [`crate::pipeline::RenderReport::failed`]
/// 只读后者 —— 于是「丢弃计数」不会把一次合法的出片判成失败。
///
/// 字幕与弹幕**各一列**（理由见模块文档）：两边都会丢东西，但丢的原因与要改的地方不同。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct OverlayStats {
    /// 真的叠上画面的**字幕行数**。
    pub lines_drawn: usize,
    /// 被切的字幕行数（顶到位图边界，或有墨像素落在画面外）。
    pub lines_clipped: usize,
    /// 因为超过 max_lines 被共享布局丢掉的字幕行数。
    pub lines_dropped: usize,
    /// 没能画出来的字幕行数（没有字体、ffmpeg 画不出、几何对不上）。
    pub lines_failed: usize,
    /// 真的叠上画面的**弹幕条数**。
    ///
    /// 「一条弹幕画了几十帧」在这里算几十条 —— 与 `lines_drawn` 同款：
    /// 它是**逐帧的账累加起来**的数，不是素材里有多少条。
    pub danmaku_drawn: usize,
    /// 因为泳道排不下被共享布局丢掉的弹幕条数（整条素材算一次，与帧无关）。
    pub danmaku_dropped: usize,
    /// 没能画出来的弹幕条数（没有字体、ffmpeg 画不出、几何对不上）。
    pub danmaku_failed: usize,
    /// 栅格化缓存命中次数 —— 「省了多少次进程」是量出来的。
    ///
    /// **字幕与弹幕共用这一个缓存**（键里带文本与尺寸，弹幕的字与字幕的字不会撞），
    /// 所以它不分成两列：分列只会让「省了多少次」这个数变成两个都只对一半的数。
    pub cache_hits: usize,
    pub cache_misses: usize,
}

impl OverlayStats {
    /// 把另一份账并进来（**分块并行**时每块各有一份，最后要汇总）。
    ///
    /// 与 `PoolStats::merge` 同一条理由：这些都是**逐帧累加**的数，
    /// 两块各画了 10 行就是一共画了 20 行。
    pub fn merge(&mut self, other: Self) {
        self.lines_drawn += other.lines_drawn;
        self.lines_clipped += other.lines_clipped;
        self.lines_dropped += other.lines_dropped;
        self.lines_failed += other.lines_failed;
        self.danmaku_drawn += other.danmaku_drawn;
        self.danmaku_dropped += other.danmaku_dropped;
        self.danmaku_failed += other.danmaku_failed;
        self.cache_hits += other.cache_hits;
        self.cache_misses += other.cache_misses;
    }

    /// 这一趟有没有文字的事。CLI 用它决定要不要出声。
    pub fn is_silent(&self) -> bool {
        self.lines_drawn == 0
            && self.lines_clipped == 0
            && self.lines_dropped == 0
            && self.lines_failed == 0
            && self.danmaku_drawn == 0
            && self.danmaku_dropped == 0
            && self.danmaku_failed == 0
    }
}

/// 出片期间的叠加器：**跨帧存活**，因为它带着栅格化缓存。
#[derive(Debug, Default)]
pub struct OverlayPainter {
    rasterizer: TextRasterizer,
    font_file: Option<PathBuf>,
    /// 粗体字体文件（可选）。字重 >= 600 时用它。
    ///
    /// # 为什么是"两个文件"而不是"一个 bold 参数"
    ///
    /// ffmpeg 的 `drawtext` **没有** `bold` 开关 —— 粗体就是**换一个字体文件**。
    /// 所以"字重"在这条链路上的落地方式只能是"宿主给出对应字重的文件"。
    /// 契约里存的是**字重这个意图**（`font_weight`），"哪个文件实现它"是宿主的事。
    bold_file: Option<PathBuf>,
    /// 字体目录（可选）：按 `font_family` 的名字在里面找。
    font_dir: Option<PathBuf>,
    stats: OverlayStats,
}

impl OverlayPainter {
    /// `font_file` 为 `None` 表示这个宿主没给字体。**不是「不要字幕」**：
    /// 真有字要画时会记一条问题（见模块文档的判据表）。
    pub fn new(font_file: Option<&Path>) -> Self {
        Self {
            rasterizer: TextRasterizer::new(),
            font_file: font_file.map(Path::to_path_buf),
            bold_file: None,
            font_dir: None,
            stats: OverlayStats::default(),
        }
    }

    /// 再给一个粗体文件（字重 >= 600 时用）与一个字体目录（按 `font_family` 找）。
    pub fn with_fonts(mut self, bold_file: Option<&Path>, font_dir: Option<&Path>) -> Self {
        self.bold_file = bold_file.map(Path::to_path_buf);
        self.font_dir = font_dir.map(Path::to_path_buf);
        self
    }

    /// 把这一帧的覆盖层叠上去。画不动的地方记问题，不返回 Err ——
    /// 一条字幕画不出来不该让整次出片在这里中断（报告会把这次出片判失败，
    /// 而已经画出来的帧仍然值得看、值得查）。
    pub fn paint(
        &mut self,
        image: &mut Rgba8Image,
        overlay: &TextOverlay,
        target: (u32, u32),
        log: &mut IssueLog,
    ) {
        // **把字段拆开借。**
        //
        // `rasterizer` 要**可变**借、字体解析只要**不可变**借 ——
        // 写成 `self.font_for(..)` 那样的方法会把整个 `self` 借住，两边打架。
        // 拆成几个字段 + 一个自由函数，借用就分开了。
        let Self { rasterizer, font_file, bold_file, font_dir, stats } = self;
        let font_for = |style: &dhampir_core::overlay::TextStyle| {
            pick_font(font_file, bold_file, font_dir, style)
        };
        paint_lines(
            &mut |key| rasterizer.rasterize(key),
            &font_for,
            // **字体目录**带下去：libass 靠它找字体与回退字体。
            font_dir,
            image,
            overlay,
            target,
            log,
            stats,
        );
    }

    /// 累计计数。缓存的命中/未命中从栅格化器现取。
    pub fn stats(&self) -> OverlayStats {
        OverlayStats {
            cache_hits: self.rasterizer.hits(),
            cache_misses: self.rasterizer.misses(),
            ..self.stats
        }
    }
}

/// 这一套样式该用哪个字体文件。
///
/// 顺序：**字体目录里按 `font_family` 找** → 字重 >= 600 时的粗体文件 →
/// `--font-file` 兜底。
///
/// 目录给了、名字也给了、却找不到 —— 这里退到兜底（**不静默换一个相似的字体**：
/// 换了之后"字长得不对"看起来像"字号配错了"）。要让它可见，看
/// `--font-dir` 那条的启动报告。
fn pick_font(
    font_file: &Option<PathBuf>,
    bold_file: &Option<PathBuf>,
    font_dir: &Option<PathBuf>,
    style: &dhampir_core::overlay::TextStyle,
) -> Option<PathBuf> {
    if let (Some(dir), Some(family)) = (font_dir.as_ref(), style.family.as_ref()) {
        if let Some(found) = resolve_family(dir, family, style.weight) {
            return Some(found);
        }
    }
    // 字重 >= 600 且给了粗体文件就用它 —— 这是"字重"在 ffmpeg 那侧的落地方式
    // （`drawtext` 没有 `bold` 开关，粗体就是换一个字体文件）。
    if style.weight >= 600 {
        if let Some(bold) = bold_file.as_ref() {
            return Some(bold.clone());
        }
    }
    font_file.clone()
}

/// 在字体目录里按族名找一个文件。
///
/// 比法是**归一化之后包含**：`"Noto Sans SC"` 与 `NotoSansSC-Regular.ttf`
/// 归一化之后分别是 `lxgwwenkai` 与 `lxgwwenkai...`，能对上。
/// 字重 >= 600 时优先挑文件名里带 `bold` 的那个。
///
/// 找不到返回 `None`（调用方退回 `--font-file`）。
fn resolve_family(dir: &Path, family: &str, weight: u32) -> Option<PathBuf> {
    let want = normalize_family(family);
    if want.is_empty() {
        return None;
    }
    let entries = std::fs::read_dir(dir).ok()?;
    let mut regular = None;
    let mut bold = None;
    for entry in entries.flatten() {
        let path = entry.path();
        let name = path.file_name()?.to_str()?.to_string();
        let lower = name.to_ascii_lowercase();
        let is_font = ["ttf", "otf", "ttc"]
            .iter()
            .any(|ext| lower.ends_with(&format!(".{ext}")));
        if !is_font {
            continue;
        }
        let stem = normalize_family(&name);
        if !stem.contains(&want) {
            continue;
        }
        if lower.contains("bold") || lower.contains("bd") {
            if bold.is_none() {
                bold = Some(path.clone());
            }
        } else if regular.is_none() {
            regular = Some(path.clone());
        }
    }
    if weight >= 600 {
        bold.or(regular)
    } else {
        regular.or(bold)
    }
}

/// 族名/文件名 -> 只留小写字母数字（丢掉空格、连字符、下划线、扩展名）。
fn normalize_family(raw: &str) -> String {
    let stem = raw.rsplit_once('.').map(|(head, _)| head).unwrap_or(raw);
    stem.chars()
        .filter(|ch| ch.is_alphanumeric())
        .flat_map(|ch| ch.to_lowercase())
        .collect()
}

/// 一条文字叠上去的结果。**只回答发生了什么** —— 怎么记账由调用方决定
/// （字幕与弹幕各有各的计数器）。
///
/// 分成三个变体而不是一个 bool：`Nothing`（没有可画的东西）与 `Failed`（画不出来）
/// 在下游是两件事 —— 前者不该计数、也不该记问题，后者要记。合成一个 bool 就会把
/// 「目标尺寸为 0」当成一次渲染失败。
enum Painted {
    /// 叠上去了。`clipped` = 「墨迹被切」那个问题报出来了没有。
    Drawn { clipped: bool },
    /// 画不出来（没字体 / 栅格化失败 / 叠图尺寸对不上）。问题已经记进 `log`。
    Failed,
    /// 没有可画的东西（目标尺寸为 0、行盒没有高度）。**不是失败**。
    Nothing,
}

/// 画一条：落点 → 栅格化 → 叠上去 →（可选）切线判定。
///
/// **字幕与弹幕共用这一份**。共用的前提是两者对得上同一个规则：位图恒取**整条目标宽**、
/// 文字在位图里居中，所以「位图中心对准矩形中心」对字幕（矩形 = 整条可用宽）与弹幕
/// （矩形 = 这条自己的宽度）都给出正确落点。各写一份就会漂 —— 而漂了以后两边各自
/// 都自洽，只有把两张画面摆在一起才看得出来。
///
/// `judge_clip`：要不要把「墨迹被切」判成问题与计数。字幕给 `true`；弹幕给 `false` ——
/// 滚动中越界是常态（见模块文档）。**这个开关在这里而不是在调用方事后过滤**：
/// 事后过滤意味着问题已经记进 `IssueLog` 了，而它没有撤回 —— 弹幕一出画面
/// 整次出片就会被判失败。
#[allow(clippy::too_many_arguments)]
/// **逐段画一行**（`.hl` 分支）。见 [`paint_one`] 里那段说明。
///
/// 每一段各自栅格化（自己的文字、自己的行内偏移、自己的颜色），
/// 然后**都按同一个落点**贴上去 —— 位图恒等于目标宽，
/// 段内的偏移是在 `drawtext` 的 `x` 表达式里加的。
#[allow(clippy::too_many_arguments)]
fn paint_parts(
    rasterize: &mut impl FnMut(&TextRasterKey) -> Result<Rc<TextBitmap>, String>,
    font_file: Option<PathBuf>,
    // **字体目录**：libass 找字体与回退字体的入口（CLI 的 --font-dir）。
    font_dir: Option<PathBuf>,
    image: &mut Rgba8Image,
    target: (u32, u32),
    parts: &[dhampir_core::overlay::TextRun],
    style: &dhampir_core::overlay::TextStyle,
    scale: f32,
    opacity: f32,
    dy_px: f32,
    judge_clip: bool,
    log: &mut IssueLog,
    placement: LinePlacement,
) -> Painted {
    // 整行文本：诊断用（`issue_path` 与"被切了"那条消息都要一个可读的身份）。
    let whole: String = parts.iter().map(|p| p.text.as_str()).collect();
    let path = issue_path(&whole);
    let Some(font_file) = font_file else {
        log.record(
            "subtitle_font_missing",
            &path,
            format!("这一帧要画带 `.hl` 的「{whole}」，而宿主没有给字体（--font-file）"),
        );
        return Painted::Failed;
    };

    // 逻辑字宽（em -> 目标像素）累加出的**行内偏移**。
    // 参照用的是 canvas 真实字宽；这里只能是逻辑宽，理由见 `paint_one` 那段注释。
    let font_px = placement.font_px as f32;
    let widths: Vec<f32> = parts
        .iter()
        .map(|p| dhampir_core::timeline::text_layout::measure_em(&p.text) * font_px)
        .collect();
    let total: f32 = widths.iter().sum();
    let offset_y = dy_px.round() as i32;
    let alpha = (opacity.clamp(0.0, 1.0) * 255.0).round() as u32;
    // 阴影参数整行算一次（它是**样式级**的，不是逐段的）。
    let shadow = shadow_spec(style);

    let mut clipped = false;
    let mut cursor = -total / 2.0;
    for (index, run) in parts.iter().enumerate() {
        let key = TextRasterKey {
            text: run.text.clone(),
            // `drawtext` 那边是 `x=(w-text_w)/2+本项`，而 `text_w` 只知道自己这一段
            // —— 所以"整行居中"由这里算好（见 `paint_one` 的残差说明）。
            x_offset: cursor.round() as i32,
            font_px: placement.font_px,
            color: run.color,
            outline: style.outline,
            // 与老路同一条：描边要乘这一条的缩放（参照 `swEff = sw * scale`）。
            stroke_px: (style.stroke_px * scale).round() as u32,
            stroke_color: style.stroke_color,
            // 这一张是**文字**位图（阴影是另一张，见下面的 `paint_shadow`）。
            shadow_color: None,
            shadow_blur_px: 0,
            shadow_dx_px: 0,
            shadow_dy_px: 0,
            shadow_pad: 0,
            font_file: font_file.clone(),
            // **契约里的家族名**带下去：libass 按名字找字体，而名字从文件名推
            // 不可靠（实测会静默回退到 ArialMT，见 `TextRasterKey::font_family`）。
            font_family: style.family.clone(),
            font_dir: font_dir.clone(),
            width: placement.bitmap_width,
            height: placement.bitmap_height,
        };
        cursor += widths[index];
        // **先阴影、后文字**（阴影在底下）。逐段各画各的阴影 ——
        // 与浏览器那条路一致（canvas 的 `fillText` 也是一段一次）。
        // 已知残差：后一段的阴影会压在**前一段的文字**上（顺序决定的），
        // 两端同序，所以它不影响"两端一致"这件事。
        if let Some(shadow) = shadow {
            paint_shadow(
                &mut *rasterize,
                &key,
                shadow,
                image,
                (placement.x, placement.y + offset_y),
                alpha,
                &path,
                log,
            );
        }
        let bitmap = match rasterize(&key) {
            Ok(bitmap) => bitmap,
            Err(error) => {
                log.record("subtitle_raster_failed", &path, error);
                return Painted::Failed;
            }
        };
        let report = match blit_scaled(image, &bitmap, placement.x, placement.y + offset_y, alpha)
        {
            Ok(report) => report,
            Err(error) => {
                log.record("subtitle_blit_failed", &path, error);
                return Painted::Failed;
            }
        };
        if judge_clip {
            if let Some(message) = clip_message(&run.text, &bitmap, placement, target, report.skipped)
            {
                log.record("subtitle_ink_clipped", &path, message);
                clipped = true;
            }
        }
    }
    Painted::Drawn { clipped }
}

/// 一条文字这一帧要不要画阴影 —— 要的话：什么色、挪多远、糊多少。
///
/// # 判据只有一条，而且它决定"老路走不走得通"
///
/// `shadow_color` 是 `Some` **且** alpha 不为 0。缺省（`None`）与"全透明"是**同一个
/// 答案**：不新增 ffmpeg 调用、不扩边、参数串与像素**逐字节不变**（理由见
/// `crate::text_raster` 的阴影那一节）。所以这里必须早返回 `None`，
/// 而不是"画一张透明度为 0 的位图" —— 后者会多起一次进程，而且位图尺寸也变了。
#[derive(Debug, Clone, Copy)]
struct ShadowSpec {
    color: [u8; 4],
    /// 偏移（目标像素）。契约里写的是**文档像素**，与 `stroke_px` 同量纲
    /// （求值层拿到的那个尺寸），所以这里只取整，不再换算一次。
    dx: i32,
    dy: i32,
    /// 模糊半径（目标像素；0 = 硬阴影）。
    blur_px: u32,
}

/// 取这一套样式里的阴影参数（`None` = 这一行不画阴影）。
fn shadow_spec(style: &dhampir_core::overlay::TextStyle) -> Option<ShadowSpec> {
    let color = style.shadow_color?;
    // 全透明的阴影 = 看不见的阴影 = 不画。**不许为它起一次栅格化**：
    // 那会多一次 ffmpeg 进程，而画面上一个像素都不多。
    if color[3] == 0 {
        return None;
    }
    Some(ShadowSpec {
        color,
        dx: style.shadow_dx_px.round() as i32,
        dy: style.shadow_dy_px.round() as i32,
        // 负数与 NaN 都按 0 算（`max` 对 NaN 的取舍：NaN.max(0.0) 给 0.0，
        // 于是这里不会把 NaN 转成一个巨大的 u32 —— 那种值会直接 OOM）。
        blur_px: style.shadow_blur_px.max(0.0).round() as u32,
    })
}

/// 把这一条的阴影贴上去。**必须在文字之前调用**（阴影在底下）。
///
/// 返回 `false` = 阴影没画出来（问题已经记进 `log`）。**文字仍然照画** ——
/// 与模块文档那条原则一致：已经画出来的帧仍然值得看、值得查，而"失败了但画面上一片空"
/// 会让人以为失败的原因在别处。整次出片照旧按问题清单判失败。
///
/// 阴影越出画面**不记问题、也不计数**：它本来就允许被画布边缘切掉
/// （偏移 + 模糊），而"墨迹被切"那条判据是**字幕行**的（见模块文档的判据表）。
#[allow(clippy::too_many_arguments)]
fn paint_shadow(
    rasterize: &mut impl FnMut(&TextRasterKey) -> Result<Rc<TextBitmap>, String>,
    text_key: &TextRasterKey,
    shadow: ShadowSpec,
    image: &mut Rgba8Image,
    // 文字位图的落点。阴影的原点 = 它 - pad + 偏移。
    origin: (i32, i32),
    alpha: u32,
    path: &str,
    log: &mut IssueLog,
) -> bool {
    let key = shadow_key(text_key, shadow.color, shadow.blur_px, shadow.dx, shadow.dy);
    // 位图四周扩过 `pad`，而字在两张位图里**都居中** —— 所以原点要往回挪一份 pad，
    // 再叠上偏移。少挪那一份，整张阴影会往右下角偏一个 pad（而"看着有点偏"
    // 正是最难查的一类）。
    let pad = key.shadow_pad as i32;
    let bitmap = match rasterize(&key) {
        Ok(bitmap) => bitmap,
        Err(error) => {
            log.record(
                "subtitle_raster_failed",
                path,
                format!("画文字阴影失败（这一行的字仍会画出来）：{error}"),
            );
            return false;
        }
    };
    match blit_scaled(
        image,
        &bitmap,
        origin.0 - pad + shadow.dx,
        origin.1 - pad + shadow.dy,
        alpha,
    ) {
        Ok(_) => true,
        Err(error) => {
            log.record("subtitle_blit_failed", path, error);
            false
        }
    }
}

fn paint_one(
    rasterize: &mut impl FnMut(&TextRasterKey) -> Result<Rc<TextBitmap>, String>,
    font_file: Option<PathBuf>,
    // 同 `paint_parts`：字体目录。
    font_dir: Option<PathBuf>,
    image: &mut Rgba8Image,
    target: (u32, u32),
    text: &str,
    rect: dhampir_core::timeline::text_layout::NormalizedRect,
    // **这一条的颜色**（求值层已解析）。不是轨道默认 —— 那个只是它的兜底。
    color: [u8; 4],
    // **这一条被整体缩了多少**（1.0 = 没缩）。描边要跟着它缩 ——
    // 参照是 `swEff = sw * wrapped.scale`。函数参数上不能用文档注释。
    scale: f32,
    // **按高亮切好的分段**（颜色已解析）。空 = 没有 `.hl`，走一次画完的老路。
    parts: &[dhampir_core::overlay::TextRun],
    style: &dhampir_core::overlay::TextStyle,
    // 这一帧的不透明度（淡入淡出算出来的，**契约层给的**）。
    opacity: f32,
    // 这一帧的纵向偏移（文档像素，正为向下）。
    dy_px: f32,
    judge_clip: bool,
    log: &mut IssueLog,
) -> Painted {
    // **全透明直接不画**：不是优化，是正确性 —— `blit` 会把 alpha=0 当"没有墨"
    // 跳过，但先返回 `Nothing` 能让"这一帧没有这一行"在计数上也如实。
    if opacity <= 0.0 {
        return Painted::Nothing;
    }
    let Some(placement) = place_line(rect, target, style.font_ratio) else {
        return Painted::Nothing;
    };
    let path = issue_path(text);
    let Some(font_file) = font_file else {
        log.record(
            "subtitle_font_missing",
            &path,
            format!(
                "这一帧要画「{text}」，而宿主没有给字体（--font-file）。\
                 本仓不内嵌字体、也不猜系统字体，所以这里画不出来 —— \
                 不给字体就不出一份「看起来成功、其实没有字幕」的片子"
            ),
        );
        return Painted::Failed;
    };
    // ---------------------------------------------------------------------
    // **有 `.hl` 分段时：逐段各画一张（各自上色 + 各自的行内偏移）。**
    // ---------------------------------------------------------------------
    //
    // 参照也是这么画的（`index.html:1985-1994`）：
    //
    // ```text
    // line.forEach(p => {
    //   ctx.strokeText(p.text, sx, ly);            // 逐段描边
    //   ctx.fillStyle = p.hl ? hlColor : color;    // 逐段填色
    //   ctx.fillText(p.text, sx, ly);
    //   sx += pw;                                  // pw = measureText(p.text).width
    // });
    // ```
    //
    // # 行内偏移怎么来的（**这里是本仓与参照的一处已知残差**）
    //
    // 参照的 `pw` 是 **canvas 量出来的真实字宽** —— 量字与画字同一个引擎。
    // 而这里 `drawtext` 的 `text_w` 只在**它自己那一张**里可用，
    // 过滤器之间不能互引 —— **拿不到另一些段的真实宽度**。
    // 所以偏移用**布局的逻辑字宽**（`measure_em`）累加，与行盒那套几何同源。
    //
    // 影响：全角字（中文）逻辑宽与实际推进一致，**偏移是准的**；
    // 中英混排时英文半角的逻辑宽是估算值，**偏移会差几像素**。
    // 这是"逻辑度量 vs 真实字体"那一类既有差异的延续，不是新引入的形状。
    if !parts.is_empty() {
        return paint_parts(
            rasterize,
            // `paint_one` 上面已经确认过字体给没给，这里带下去就行。
            Some(font_file),
            font_dir.clone(),
            image,
            target,
            parts,
            style,
            scale,
            opacity,
            dy_px,
            judge_clip,
            log,
            placement,
        );
    }
    let key = TextRasterKey {
        text: text.to_string(),
        // 整行居中：老行为（`.hl` 的分段走上面那条分支）。
        x_offset: 0,
        font_px: placement.font_px,
        // **用这一条自己的颜色**（求值层已解析）。
        // 这里以前读的是 `style.color`（轨道默认）—— 于是"逐条颜色"在求值层
        // 算对了、到渲染这一层被丢掉。像素级核对才看得出来。
        color,
        outline: style.outline,
        // `style.stroke_px` 已经是**目标像素**（`evaluate_overlay` 用 `stroke_ratio * 目标高`
        // 算的），而位图恒等于目标宽（见 `LinePlacement::bitmap_width`）
        // —— 所以两者同一套单位，不用再缩一次。
        // **描边要乘这一条的缩放**：参照 `swEff = sw * wrapped.scale`。
        // 不乘的症状是"缩过的字幕描边显得特别粗" —— 字号与行高都对，
        // 只有描边不对，很难一眼看出来。
        stroke_px: (style.stroke_px * scale).round() as u32,
        stroke_color: style.stroke_color,
        // 这一张是**文字**位图：阴影是另一张（`paint_shadow`），
        // 两者的键在缓存里各占一格（不然会把没模糊的那张递给有模糊的）。
        shadow_color: None,
        shadow_blur_px: 0,
        shadow_dx_px: 0,
        shadow_dy_px: 0,
        shadow_pad: 0,
        font_file: font_file.to_path_buf(),
        // 同上：家族名从契约带下来。
        font_family: style.family.clone(),
        font_dir: font_dir.clone(),
        width: placement.bitmap_width,
        height: placement.bitmap_height,
    };
    // 纵向偏移（淡入上浮/退场下移）：**只挪落点**，不重算布局 ——
    // 位置是"布局给的行盒 + 这一帧的动画偏移"，两件事分开才说得清。
    let offset_y = dy_px.round() as i32;
    let alpha = (opacity.clamp(0.0, 1.0) * 255.0).round() as u32;
    // ---------------------------------------------------------------------
    // **先阴影、后文字**（阴影必须在底下）。
    // ---------------------------------------------------------------------
    //
    // 判据全在 `shadow_spec` 里：`shadow_color` 缺省（或 alpha = 0）时这里是
    // 一个不做任何事的 `None` —— 不新增 ffmpeg 调用、不扩边、参数串与像素
    // 逐字节不变。**"不画阴影"这条分支就是老路本身**，不是它旁边的一条路。
    if let Some(shadow) = shadow_spec(style) {
        // 位图是**先取后贴**：两张都取到缓存里，贴的顺序才是阴影 -> 文字。
        // （阴影画不出来时不拦文字：问题已经记下，字照画，见 `paint_shadow`。）
        paint_shadow(
            &mut *rasterize,
            &key,
            shadow,
            image,
            (placement.x, placement.y + offset_y),
            alpha,
            &path,
            log,
        );
    }
    let bitmap = match rasterize(&key) {
        Ok(bitmap) => bitmap,
        Err(error) => {
            log.record("subtitle_raster_failed", &path, error);
            return Painted::Failed;
        }
    };
    let report = match blit_scaled(
        image,
        &bitmap,
        placement.x,
        placement.y + offset_y,
        alpha,
    ) {
        Ok(report) => report,
        Err(error) => {
            log.record("subtitle_blit_failed", &path, error);
            return Painted::Failed;
        }
    };
    let clipped = if judge_clip {
        match clip_message(text, &bitmap, placement, target, report.skipped) {
            Some(message) => {
                log.record("subtitle_ink_clipped", &path, message);
                true
            }
            None => false,
        }
    } else {
        false
    };
    Painted::Drawn { clipped }
}

/// 真正干活的那一段。**栅格化器是参数**：于是「落点、叠加、问题、计数」这四件事
/// 能在不起 ffmpeg 的前提下被单测（与 text_raster 里那个缓存缝同一个理由）。
fn paint_lines(
    rasterize: &mut impl FnMut(&TextRasterKey) -> Result<Rc<TextBitmap>, String>,
    // 按样式解析出字体文件的闭包（`OverlayPainter::font_for` 的借用版）——
    // 字体不是"整份一个"，字重/族名都可能不同。
    font_for: &dyn Fn(&dhampir_core::overlay::TextStyle) -> Option<PathBuf>,
    // **字体目录**（CLI 的 --font-dir）：libass 找字体与回退字体的入口。
    font_dir: &Option<PathBuf>,
    image: &mut Rgba8Image,
    overlay: &TextOverlay,
    target: (u32, u32),
    log: &mut IssueLog,
    stats: &mut OverlayStats,
) {
    stats.lines_dropped += overlay.dropped_lines;
    stats.danmaku_dropped += overlay.dropped_danmaku;

    for item in &overlay.items {
        match paint_one(
            &mut *rasterize,
            font_for(&overlay.subtitle_style),
            font_dir.clone(),
            image,
            target,
            &item.text,
            item.rect,
            // **这一条的颜色**（求值层已解析：cue 自带覆盖轨道默认）。
            // 描边与开关仍走轨道样式 —— 那两样在契约里就是轨道级的。
            item.color,
            // 字幕的缩放：布局算出来的（参照从不截断，缩字是它的常规路径）。
            item.scale,
            // **逐段颜色**：空 = 没有 `.hl`，走一次画完的老路。
            &item.parts,
            &overlay.subtitle_style,
            item.opacity,
            item.dy_px,
            true,
            log,
        ) {
            Painted::Drawn { clipped } => {
                stats.lines_drawn += 1;
                if clipped {
                    stats.lines_clipped += 1;
                }
            }
            Painted::Failed => stats.lines_failed += 1,
            Painted::Nothing => {}
        }
    }

    for item in &overlay.danmaku {
        match paint_one(
            &mut *rasterize,
            font_for(&overlay.danmaku_style),
            font_dir.clone(),
            image,
            target,
            &item.text,
            item.rect,
            item.color,
            // **弹幕不缩字**（参照的弹幕路径没有缩字逻辑），恒传 1.0。
            // 不把这个字段放进 `DanmakuTextItem` —— 放进去就等于宣称弹幕会缩，
            // 一个恒为 1 的字段只会让读的人以为它有意义。
            1.0,
            // 弹幕没有 `.hl` 标记。
            &[],
            &overlay.danmaku_style,
            item.opacity,
            item.dy_px,
            // **不判切线**：滚动中越界是常态（见模块文档）。画面外的墨像素由 `blit`
            // 丢掉，但那既不进问题清单、也不计数 —— 计数只数"画了几条"。
            false,
            log,
        ) {
            Painted::Drawn { .. } => stats.danmaku_drawn += 1,
            Painted::Failed => stats.danmaku_failed += 1,
            Painted::Nothing => {}
        }
    }
}

/// 问题清单里的路径：按**行内容**而不是帧号。
///
/// 同一条字幕会连续出现在几十帧里，按帧号记会把清单刷满（上限 24 条），
/// 而人真正想看到的是「哪一行字画不下」。截断是为了让清单还能一眼扫过去。
fn issue_path(text: &str) -> String {
    const MAX_CHARS: usize = 24;
    let mut short: String = text.chars().take(MAX_CHARS).collect();
    if text.chars().count() > MAX_CHARS {
        short.push('…');
    }
    format!("overlay[{short}]")
}

/// 「字被切了」的判词。没这回事就给 `None`。
///
/// 两个信号，两个都独立成立、都要报：
///
/// * `skipped`：有墨像素落在画面外 —— **确定**被切；
/// * 墨迹顶到位图的边界 —— **可能**被切。横向的位图边界就是画面边界（位图宽等于目标宽），
///   所以横向顶边意味着「这一行比画面还宽」；纵向顶边意味着行盒加上下边距都装不下这个字。
fn clip_message(
    text: &str,
    bitmap: &TextBitmap,
    placement: LinePlacement,
    target: (u32, u32),
    skipped: u64,
) -> Option<String> {
    let (x0, y0, x1, y1) = bitmap.ink_bounds()?;
    let mut reasons: Vec<String> = Vec::new();
    if skipped > 0 {
        reasons.push(format!("有 {skipped} 个有墨像素落在画面外"));
    }
    if x0 == 0 || x1 == bitmap.width.saturating_sub(1) {
        reasons.push("墨迹顶到位图的左右边界（位图宽就是画面宽）".to_string());
    }
    if y0 == 0 || y1 == bitmap.height.saturating_sub(1) {
        reasons.push("墨迹顶到位图的上下边界（行盒加上下边距装不下这个字）".to_string());
    }
    if reasons.is_empty() {
        return None;
    }
    let left = placement.x + x0 as i32;
    let top = placement.y + y0 as i32;
    let right = placement.x + x1 as i32;
    let bottom = placement.y + y1 as i32;
    Some(format!(
        "「{text}」的墨迹在画面里是 {left}..{right} / {top}..{bottom}（画面 {}x{}），{}。\
         共享布局按「按字宽分类」的模型换行，而真字体比模型宽 3%~25%，\
         所以这是可达的：调小 font_ratio，或者把这一行改短",
        target.0,
        target.1,
        reasons.join("；")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use dhampir_core::overlay::{DanmakuTextItem, TextItem};
    use dhampir_core::timeline::schema::Issue;
    // 落点几何搬去契约层之后，矩形类型只在这个测试模块里还用到。
    use dhampir_core::timeline::text_layout::NormalizedRect;

    fn rect(x: f32, y: f32, width: f32, height: f32) -> NormalizedRect {
        NormalizedRect {
            x,
            y,
            width,
            height,
        }
    }

    fn overlay(lines: &[(&str, NormalizedRect)], dropped: usize) -> TextOverlay {
        TextOverlay {
            items: lines
                .iter()
                .map(|(text, rect)| TextItem {
                    text: (*text).to_string(),
                    rect: *rect,
                    // 老行为：满不透明、不位移（淡入淡出默认关）。
                    opacity: 1.0,
                    dy_px: 0.0,
                    color: [255, 255, 255, 255],
                    font_ratio: 0.04,
                    scale: 1.0,
                    parts: Vec::new(),
                })
                .collect(),
            danmaku: Vec::new(),
            subtitle_style: dhampir_core::overlay::TextStyle {
                color: [255, 255, 255, 255],
                highlight_color: None,
                font_ratio: 0.04,
                outline: true,
                stroke_px: 0.0,
                stroke_color: [0, 0, 0, 255],
                family: None,
                weight: 400,
                // **不画阴影**：这是"老工程"的那套样式。
                shadow_color: None,
                shadow_dx_px: 0.0,
                shadow_dy_px: 0.0,
                shadow_blur_px: 0.0,
            },
            danmaku_style: dhampir_core::overlay::TextStyle::default(),
            dropped_lines: dropped,
            dropped_danmaku: 0,
        }
    }

    /// 只有弹幕、没有字幕的一帧。`dropped_danmaku` 是泳道排不下丢掉的那几条。
    fn danmaku_overlay(items: &[(&str, NormalizedRect, u32)], dropped_danmaku: usize) -> TextOverlay {
        TextOverlay {
            items: Vec::new(),
            danmaku: items
                .iter()
                .map(|(text, rect, lane)| DanmakuTextItem {
                    text: (*text).to_string(),
                    rect: *rect,
                    lane: *lane,
                    // 在屏区间在这一点上无关紧要（画法只看这一帧的矩形），给一对确定值。
                    enter: 0,
                    exit: 100,
                    // 在屏区间在这一点上无关紧要；travel 同（画法只看这一帧的矩形）。
                    travel_frames: 600,
                    opacity: 1.0,
                    dy_px: 0.0,
                    color: [255, 255, 255, 255],
                    font_ratio: 0.04,
                })
                .collect(),
            subtitle_style: dhampir_core::overlay::TextStyle::default(),
            danmaku_style: dhampir_core::overlay::TextStyle {
                color: [255, 255, 255, 255],
                highlight_color: None,
                font_ratio: 0.04,
                outline: true,
                stroke_px: 0.0,
                stroke_color: [0, 0, 0, 255],
                family: None,
                weight: 400,
                // 弹幕恒不画阴影（`DanmakuSpec` 里没有阴影字段）。
                shadow_color: None,
                shadow_dx_px: 0.0,
                shadow_dy_px: 0.0,
                shadow_blur_px: 0.0,
            },
            dropped_lines: 0,
            dropped_danmaku,
        }
    }

    /// 一张 8x8 的位图：全不透明。用来验「叠上去」这件事本身。
    fn opaque_bitmap(width: u32, height: u32) -> TextBitmap {
        TextBitmap {
            width,
            height,
            pixels: vec![255u8; width as usize * height as usize * 4],
        }
    }

    fn frame(width: u32, height: u32, value: [u8; 4]) -> Rgba8Image {
        let mut pixels = Vec::with_capacity((width * height * 4) as usize);
        for _ in 0..(width * height) {
            pixels.extend_from_slice(&value);
        }
        Rgba8Image {
            width,
            height,
            pixels,
        }
    }

    /// 一张只有中间一块墨的位图：四周留白，于是**不会**触发「墨迹顶到边界」那条判据 ——
    /// 真位图就是这样（drawtext 把字放在中间，四周是 pad）。拿「整张全不透明」当假位图
    /// 会把切线判据顺带点着，测试就不再只验它想验的那件事。
    fn ink_bitmap(width: u32, height: u32) -> TextBitmap {
        ink_bitmap_of(width, height, [255, 255, 255, 255])
    }

    /// 同上，但指定墨色 —— 阴影那条用例靠**颜色**分辨"贴的是哪一张"。
    fn ink_bitmap_of(width: u32, height: u32, color: [u8; 4]) -> TextBitmap {
        let mut bitmap = TextBitmap::blank(width, height);
        let x0 = width / 2 - width / 8;
        let x1 = width / 2 + width / 8;
        let y0 = height / 2 - height / 4;
        let y1 = height / 2 + height / 4;
        for y in y0..y1 {
            for x in x0..x1 {
                let at = ((y as usize) * (width as usize) + x as usize) * 4;
                bitmap.pixels[at..at + 4].copy_from_slice(&color);
            }
        }
        bitmap
    }

    /// 起一次假栅格化：要画的那行按给定尺寸造一张带墨的位图。
    fn fake_rasterizer(
        calls: &mut usize,
    ) -> impl FnMut(&TextRasterKey) -> Result<Rc<TextBitmap>, String> + '_ {
        move |key: &TextRasterKey| {
            *calls += 1;
            Ok(Rc::new(ink_bitmap(key.width, key.height)))
        }
    }

    fn codes(log: &[Issue]) -> Vec<&str> {
        log.iter().map(|issue| issue.code.as_str()).collect()
    }

    // ---- 几何 ----

    #[test]
    fn 落点把位图中心对准行盒中心() {
        // 字号 = font_ratio × 目标高 = (20/360) × 360 = 20。
        // **它是布局给的事实**，不再由行盒反推（见 TextLine::font_ratio）。
        let placement =
            place_line(rect(0.25, 0.5, 0.5, 0.066), (640, 360), 20.0 / 360.0).expect("能落点");
        assert_eq!(placement.font_px, 20);
        assert_eq!((placement.bitmap_width, placement.bitmap_height), (640, 36));
        // 行盒中心 = (0.5, 0.5 + 0.033) -> (320, 191.88)；位图中心对准它。
        assert_eq!(placement.x, 0);
        assert_eq!(placement.y, 174);
    }

    #[test]
    fn 字号跟着目标高度走而不是文档坐标() {
        // 同一个行盒，目标高一倍：字号与位图都要跟着放大 —— 归一化矩形与渲染尺寸无关，
        // 但**像素**当然有关。
        let small =
            place_line(rect(0.25, 0.5, 0.5, 0.066), (640, 360), 20.0 / 360.0).expect("能落点");
        let large =
            place_line(rect(0.25, 0.5, 0.5, 0.066), (1280, 720), 20.0 / 360.0).expect("能落点");
        assert_eq!(small.font_px, 20);
        assert_eq!(large.font_px, 40);
        assert_eq!(large.bitmap_height, 74);
    }

    #[test]
    fn 零高度或零尺寸没有可画的东西() {
        // 行盒零高：没有字可画 -> 不是「画失败」，是「没东西」。
        assert!(place_line(rect(0.5, 0.5, 0.2, 0.0), (640, 360), 0.04).is_none());
        // NaN 也要挡住（比较恒假，于是走同一条分支）。
        assert!(place_line(rect(0.5, 0.5, 0.2, f32::NAN), (640, 360), 0.04).is_none());
        assert!(place_line(rect(0.5, 0.5, 0.2, 0.1), (0, 360), 0.04).is_none());
    }

    // ---- 叠加 ----

    #[test]
    fn 全不透明的位图是逐字节覆盖() {
        let mut image = frame(16, 16, [10, 20, 30, 255]);
        let bitmap = opaque_bitmap(8, 8);
        let report = blit(&mut image, &bitmap, 2, 3).expect("叠得上");
        assert_eq!(
            report,
            BlitReport {
                written: 64,
                skipped: 0
            }
        );
        // 盖住的地方是位图的字节，其余一个字节没动。
        for y in 0..16u32 {
            for x in 0..16u32 {
                let got = image.pixel(x, y).expect("在画面里");
                let inside = (2..10).contains(&x) && (3..11).contains(&y);
                let want = if inside {
                    [255, 255, 255, 255]
                } else {
                    [10, 20, 30, 255]
                };
                assert_eq!(got, want, "({x},{y}) 不对");
            }
        }
    }

    #[test]
    fn 半透明按直排_alpha_叠加() {
        let mut image = frame(4, 4, [0, 0, 0, 255]);
        let mut pixels = vec![0u8; 4 * 4 * 4];
        for px in pixels.chunks_exact_mut(4) {
            px.copy_from_slice(&[255, 255, 255, 128]);
        }
        let bitmap = TextBitmap {
            width: 4,
            height: 4,
            pixels,
        };
        let report = blit(&mut image, &bitmap, 0, 0).expect("叠得上");
        assert_eq!(report.written, 16);
        // 白 128/255 叠在黑上：每通道 128；alpha 直排相加 -> 满。
        let got = image.pixel(1, 1).expect("在画面里");
        assert_eq!(got, [128, 128, 128, 255], "直排 source-over 的算术不对");
    }

    #[test]
    fn 全透明的像素一个字节都不动() {
        let mut image = frame(4, 4, [7, 8, 9, 200]);
        let before = image.clone();
        let bitmap = TextBitmap::blank(4, 4);
        let report = blit(&mut image, &bitmap, 0, 0).expect("叠得上");
        assert_eq!(
            report,
            BlitReport {
                written: 0,
                skipped: 0
            }
        );
        assert_eq!(image, before, "没有墨的位图不该动任何字节");
    }

    #[test]
    fn 落在画面外的墨迹要被数出来而不是回绕() {
        let mut image = frame(16, 16, [0, 0, 0, 255]);
        let bitmap = opaque_bitmap(4, 4);
        let report = blit(&mut image, &bitmap, -2, -2).expect("叠得上");
        // 4x4 里只有右下 2x2 落在画面内。
        assert_eq!(
            report,
            BlitReport {
                written: 4,
                skipped: 12
            }
        );
        assert_eq!(image.pixel(0, 0).expect("在画面里"), [255, 255, 255, 255]);
        // 回绕的话右下角会被画上（x = -2 绕到末尾）——这条把它钉住。
        assert_eq!(image.pixel(15, 15).expect("在画面里"), [0, 0, 0, 255]);
    }

    // ---- 判据 ----

    #[test]
    fn 没有覆盖层就一个字节都不动() {
        let mut painter = OverlayPainter::new(Some(Path::new("C:/fake/font.ttf")));
        let mut image = frame(16, 16, [1, 2, 3, 4]);
        let before = image.clone();
        let mut log = IssueLog::new();
        painter.paint(&mut image, &overlay(&[], 0), (16, 16), &mut log);
        assert_eq!(image, before);
        assert!(log.is_empty(), "空覆盖层不该记问题");
        assert!(painter.stats().is_silent());
    }

    #[test]
    fn 有字要画而宿主没有字体是问题而不是空操作() {
        let mut painter = OverlayPainter::new(None);
        let mut image = frame(64, 64, [0, 0, 0, 255]);
        let before = image.clone();
        let mut log = IssueLog::new();
        let lines = overlay(&[("第一行中文", rect(0.25, 0.8, 0.5, 0.066))], 0);
        painter.paint(&mut image, &lines, (64, 64), &mut log);
        let issues = log.into_vec();
        assert_eq!(codes(&issues), vec!["subtitle_font_missing"]);
        assert_eq!(image, before, "画不出来就不该动像素");
        assert_eq!(painter.stats().lines_failed, 1);
        assert_eq!(painter.stats().lines_drawn, 0);
    }

    #[test]
    fn 墨迹顶到边界要记成问题_没顶到就不记() {
        // 反向用例：两张位图只差「墨迹在不在边界上」，判定必须跟着变 ——
        // 否则这条判据要么恒真、要么恒假。
        let inside = rect(0.25, 0.8, 0.5, 0.066);
        for (ink_at_edge, expected) in [(true, 1usize), (false, 0usize)] {
            let mut painter = OverlayPainter::new(Some(Path::new("C:/fake/font.ttf")));
            let mut image = frame(640, 360, [0, 0, 0, 255]);
            let mut log = IssueLog::new();
            let mut calls = 0;
            let rasterize = |key: &TextRasterKey| {
                let mut bitmap = TextBitmap::blank(key.width, key.height);
                let (x0, x1) = if ink_at_edge {
                    (0, key.width - 1)
                } else {
                    (key.width / 2 - 4, key.width / 2 + 4)
                };
                for y in 4..8u32 {
                    for x in x0..=x1 {
                        let at = ((y as usize) * (key.width as usize) + x as usize) * 4;
                        bitmap.pixels[at..at + 4].copy_from_slice(&[255, 255, 255, 255]);
                    }
                }
                Ok(Rc::new(bitmap))
            };
            let counter = &mut calls;
            let lines = overlay(&[("一", inside)], 0);
            let stats = &mut painter.stats;
            paint_lines(
                &mut |key| {
                    *counter += 1;
                    rasterize(key)
                },
                &|_: &dhampir_core::overlay::TextStyle| painter.font_file.clone(),
                // 测试里不给字体目录（走 `--font-file` 的父目录兜底）。
                &None,
                &mut image,
                &lines,
                (640, 360),
                &mut log,
                stats,
            );
            let issues = log.into_vec();
            assert_eq!(issues.len(), expected, "边界判定反了：{issues:?}");
            assert_eq!(painter.stats().lines_clipped, expected);
            assert_eq!(painter.stats().lines_drawn, 1, "画还是画了的");
            assert_eq!(calls, 1);
        }
    }

    #[test]
    fn 叠到画面外的行要被报成被切() {
        let mut painter = OverlayPainter::new(Some(Path::new("C:/fake/font.ttf")));
        // 行盒整个在画面之上（中心 y = -0.4 + 0.033），于是位图连同上下边距一起露在画面外。
        let mut image = frame(640, 360, [0, 0, 0, 255]);
        let before = image.clone();
        let mut log = IssueLog::new();
        let mut calls = 0;
        let lines = overlay(&[("一", rect(0.25, -0.4, 0.5, 0.066))], 0);
        paint_lines(
            &mut |key| fake_rasterizer(&mut calls)(key),
            &|_: &dhampir_core::overlay::TextStyle| Some(PathBuf::from("C:/fake/font.ttf")),
            // 测试里不给字体目录（走 `--font-file` 的父目录兜底）。
            &None,
            &mut image,
            &lines,
            (640, 360),
            &mut log,
            &mut painter.stats,
        );
        let issues = log.into_vec();
        assert_eq!(codes(&issues), vec!["subtitle_ink_clipped"]);
        assert!(
            issues[0].message.contains("落在画面外"),
            "{}",
            issues[0].message
        );
        assert_eq!(painter.stats().lines_clipped, 1);
        assert_eq!(painter.stats().lines_drawn, 1);
        assert_eq!(calls, 1);
        assert_eq!(image, before, "整行都在画面外就不该动任何字节");
    }

    #[test]
    fn 画不出来要记成问题() {
        let mut painter = OverlayPainter::new(Some(Path::new("C:/fake/font.ttf")));
        let mut image = frame(64, 64, [0, 0, 0, 255]);
        let before = image.clone();
        let mut log = IssueLog::new();
        let stats = &mut painter.stats;
        let lines = overlay(&[("一", rect(0.25, 0.8, 0.5, 0.066))], 0);
        paint_lines(
            &mut |_key| Err("ffmpeg 画不出这一行（退出码 1）：字体不认得".to_string()),
            &|_: &dhampir_core::overlay::TextStyle| Some(PathBuf::from("C:/fake/font.ttf")),
            // 测试里不给字体目录（走 `--font-file` 的父目录兜底）。
            &None,
            &mut image,
            &lines,
            (64, 64),
            &mut log,
            stats,
        );
        let issues = log.into_vec();
        assert_eq!(codes(&issues), vec!["subtitle_raster_failed"]);
        assert_eq!(image, before);
        assert_eq!(painter.stats().lines_failed, 1);
    }

    #[test]
    fn 丢掉的行是事实不是问题() {
        let mut painter = OverlayPainter::new(Some(Path::new("C:/fake/font.ttf")));
        let mut image = frame(64, 64, [0, 0, 0, 255]);
        let mut log = IssueLog::new();
        let mut calls = 0;
        let lines = overlay(&[("一", rect(0.25, 0.8, 0.5, 0.066))], 3);
        paint_lines(
            &mut |key| fake_rasterizer(&mut calls)(key),
            &|_: &dhampir_core::overlay::TextStyle| Some(PathBuf::from("C:/fake/font.ttf")),
            // 测试里不给字体目录（走 `--font-file` 的父目录兜底）。
            &None,
            &mut image,
            &lines,
            (64, 64),
            &mut log,
            &mut painter.stats,
        );
        assert!(log.is_empty(), "丢行不该判失败：那是样式自己写的上限");
        assert_eq!(painter.stats().lines_dropped, 3);
        assert_eq!(painter.stats().lines_drawn, 1);
    }

    // ---- 文字阴影（B5）：先阴影、后文字，而且阴影真的挪了 ----

    /// **不画阴影时：一个像素都不动、一次多余的栅格化都不发生。**
    ///
    /// 这是"既有工程逐字节不变"在这一层的执行处：`shadow_color` 缺省时
    /// 不许出现第二张位图（多起一次 ffmpeg 进程），也不许动任何像素。
    #[test]
    fn 不画阴影时不多栅格化也不动像素() {
        let target = (640u32, 360u32);
        let rect = rect(0.25, 0.8, 0.5, 0.066);
        let mut baseline = frame(640, 360, [0, 0, 0, 255]);
        let mut calls = 0usize;
        let mut log = IssueLog::new();
        let mut painter = OverlayPainter::new(Some(Path::new("C:/fake/font.ttf")));
        let lines = overlay(&[("一", rect)], 0);
        assert_eq!(lines.subtitle_style.shadow_color, None, "夹具就是老样式");
        paint_lines(
            &mut |key| fake_rasterizer(&mut calls)(key),
            &|_: &dhampir_core::overlay::TextStyle| Some(PathBuf::from("C:/fake/font.ttf")),
            &None,
            &mut baseline,
            &lines,
            target,
            &mut log,
            &mut painter.stats,
        );
        assert_eq!(calls, 1, "只有文字那一张，阴影不该引出第二次栅格化");

        // 同一帧再画一次"全透明的阴影"：**判据与缺省相同**（看不见就别画）。
        let mut transparent = frame(640, 360, [0, 0, 0, 255]);
        let mut calls = 0usize;
        let mut lines = overlay(&[("一", rect)], 0);
        lines.subtitle_style.shadow_color = Some([0, 0, 0, 0]);
        lines.subtitle_style.shadow_dy_px = 4.0;
        paint_lines(
            &mut |key| fake_rasterizer(&mut calls)(key),
            &|_: &dhampir_core::overlay::TextStyle| Some(PathBuf::from("C:/fake/font.ttf")),
            &None,
            &mut transparent,
            &lines,
            target,
            &mut log,
            &mut painter.stats,
        );
        assert_eq!(calls, 1, "全透明的阴影也该走同一条老路");
        assert_eq!(transparent, baseline, "全透明的阴影与不画必须逐字节相同");
    }

    /// **画阴影时：阴影真的落在偏移处，而且文字压在它上面。**
    ///
    /// 假栅格器按"这一张是不是阴影"给不同颜色（阴影蓝、文字白），于是
    /// 「贴了几张、贴在哪、谁在上面」这几件事在像素上都能读出来。
    #[test]
    fn 画阴影时阴影落在偏移处且文字压在它上面() {
        let target = (640u32, 360u32);
        let rect = rect(0.25, 0.8, 0.5, 0.066);
        // 落点取共享几何给的那个（不手算）：这条要验的是叠加那一步，不是布局。
        let placement = place_line(rect, target, 0.04).expect("能落点");
        let (w, h) = (placement.bitmap_width, placement.bitmap_height);

        let mut image = frame(640, 360, [0, 0, 0, 255]);
        let mut log = IssueLog::new();
        let mut calls = 0usize;
        let mut painter = OverlayPainter::new(Some(Path::new("C:/fake/font.ttf")));
        let mut lines = overlay(&[("一", rect)], 0);
        lines.subtitle_style.shadow_color = Some([0, 0, 0, 255]);
        // 只留垂直偏移（不用模糊）：**阴影的落点=文字落点+(0,20)**，于是
        // 两块墨在纵向完全错开，"偏移生效了没有"一眼可判。
        lines.subtitle_style.shadow_dx_px = 0.0;
        lines.subtitle_style.shadow_dy_px = 20.0;
        lines.subtitle_style.shadow_blur_px = 0.0;
        paint_lines(
            &mut |key| {
                calls += 1;
                let color = if key.shadow_color.is_some() {
                    // 阴影那张：画布四周扩过 pad，墨在**画布中心**（与文字那张同一个规则）。
                    assert_eq!(key.width, w + 40, "blur=0、dy=20 时四周各扩 20");
                    assert_eq!(key.height, h + 40);
                    [0, 0, 255, 255]
                } else {
                    assert_eq!((key.width, key.height), (w, h), "文字那张尺寸不许变");
                    [255, 255, 255, 255]
                };
                Ok(Rc::new(ink_bitmap_of(key.width, key.height, color)))
            },
            &|_: &dhampir_core::overlay::TextStyle| Some(PathBuf::from("C:/fake/font.ttf")),
            // 测试里不给字体目录（走 `--font-file` 的父目录兜底）。
            &None,
            &mut image,
            &lines,
            target,
            &mut log,
            &mut painter.stats,
        );
        assert!(log.is_empty(), "这里不该有问题：{:?}", log.into_vec());
        assert_eq!(calls, 2, "文字一张 + 阴影一张");

        // 墨块在各自的位图里居中：文字那张占 [y0, y1)，阴影那张整体下移 20。
        let cx = placement.x + (w as i32) / 2;
        let text_y = placement.y + (h as i32) / 2;
        let shadow_y = text_y + 20;
        let got_text = image.pixel(cx as u32, text_y as u32).expect("在画面里");
        let got_shadow = image.pixel(cx as u32, shadow_y as u32).expect("在画面里");
        assert_eq!(got_text, [255, 255, 255, 255], "文字应当在最上面");
        assert_eq!(
            got_shadow, [0, 0, 255, 255],
            "阴影必须落在**偏移之后**的位置（dy=20）—— 落在文字原处就是没挪"
        );
        // 反向：偏移方向**上方**（文字原处）不能是阴影色。
        assert_ne!(
            image.pixel(cx as u32, (text_y - 20) as u32).expect("在画面里"),
            [0, 0, 255, 255],
            "偏移的反方向出现了阴影色：说明它被贴到了别处"
        );
    }

    /// 阴影画不出来时：**文字仍然画出来**，但问题要记下来（整次出片照样判失败）。
    ///
    /// 反过来的做法（把文字也丢掉）看上去"更一致"，代价是画面上一片空 ——
    /// 而查问题的人需要的恰恰是"字在哪、阴影缺了"。
    #[test]
    fn 阴影画不出来也要把字画出来并记问题() {
        let target = (640u32, 360u32);
        let rect = rect(0.25, 0.8, 0.5, 0.066);
        let mut image = frame(640, 360, [0, 0, 0, 255]);
        let mut log = IssueLog::new();
        let mut calls = 0usize;
        let mut painter = OverlayPainter::new(Some(Path::new("C:/fake/font.ttf")));
        let mut lines = overlay(&[("一", rect)], 0);
        lines.subtitle_style.shadow_color = Some([0, 0, 0, 255]);
        lines.subtitle_style.shadow_blur_px = 6.0;
        paint_lines(
            &mut |key| {
                calls += 1;
                if key.shadow_color.is_some() {
                    return Err("ffmpeg 说不出这一张".to_string());
                }
                Ok(Rc::new(ink_bitmap(key.width, key.height)))
            },
            &|_: &dhampir_core::overlay::TextStyle| Some(PathBuf::from("C:/fake/font.ttf")),
            // 测试里不给字体目录（走 `--font-file` 的父目录兜底）。
            &None,
            &mut image,
            &lines,
            target,
            &mut log,
            &mut painter.stats,
        );
        assert_eq!(calls, 2, "两张都要试过");
        let issues = log.into_vec();
        assert_eq!(codes(&issues), vec!["subtitle_raster_failed"]);
        assert!(
            issues[0].message.contains("阴影"),
            "消息要说清是阴影那一张：{}",
            issues[0].message
        );
        // 文字画出来了：账面上算"画了"，帧上也有墨（阴影缺了）。
        assert_eq!(painter.stats().lines_drawn, 1);
        assert_eq!(painter.stats().lines_failed, 0);
        let placement = place_line(rect, target, 0.04).expect("能落点");
        let px = placement.x + placement.bitmap_width as i32 / 2;
        let py = placement.y + placement.bitmap_height as i32 / 2;
        assert_eq!(
            image.pixel(px as u32, py as u32).expect("在画面里"),
            [255, 255, 255, 255],
            "阴影失败不该把文字也丢掉"
        );
    }

    // ---- 弹幕：同一条画法，不同的判据 ----

    #[test]
    fn 弹幕按自己的那一列记账_且只动弹幕那几格() {
        let mut painter = OverlayPainter::new(Some(Path::new("C:/fake/font.ttf")));
        let mut image = frame(640, 360, [0, 0, 0, 255]);
        let before = image.clone();
        let mut log = IssueLog::new();
        let mut calls = 0;
        // 第一条在画面里（左边缘 0.6），第二条整个滚到画面左边之外 —— 两条都要算"画了"：
        // 滚动中越界是常态，与字幕那条「墨迹被切」不是一回事。
        let items = danmaku_overlay(
            &[
                ("第一条", rect(0.6, 0.1, 0.25, 0.066), 0),
                ("第二条", rect(-0.5, 0.2, 0.25, 0.066), 1),
            ],
            2,
        );
        paint_lines(
            &mut |key| fake_rasterizer(&mut calls)(key),
            &|_: &dhampir_core::overlay::TextStyle| Some(PathBuf::from("C:/fake/font.ttf")),
            // 测试里不给字体目录（走 `--font-file` 的父目录兜底）。
            &None,
            &mut image,
            &items,
            (640, 360),
            &mut log,
            &mut painter.stats,
        );
        assert!(log.is_empty(), "弹幕滚出画面是常态，不该记问题：{:?}", log.into_vec());
        let stats = painter.stats();
        assert_eq!(stats.danmaku_drawn, 2);
        assert_eq!(stats.danmaku_dropped, 2);
        assert_eq!(stats.danmaku_failed, 0);
        // **字幕那几格必须不动**：混在一起就查不出是哪一个在丢/画不出。
        assert_eq!(stats.lines_drawn, 0);
        assert_eq!(stats.lines_clipped, 0);
        assert_eq!(stats.lines_dropped, 0);
        assert_eq!(stats.lines_failed, 0);
        assert!(!stats.is_silent(), "只有弹幕的一帧不是静默");
        assert_ne!(image, before, "在画面里的那条确实要落笔");
        assert_eq!(calls, 2, "两条各栅格化一次");
    }

    #[test]
    fn 弹幕画不出来记在弹幕那一列而不是字幕那一列() {
        // 反向：把弹幕计数写进 `lines_failed` 的话，这两条断言会红。
        let mut painter = OverlayPainter::new(None);
        let mut image = frame(640, 360, [0, 0, 0, 255]);
        let before = image.clone();
        let mut log = IssueLog::new();
        let items = danmaku_overlay(&[("第一条", rect(0.6, 0.1, 0.25, 0.066), 0)], 0);
        paint_lines(
            &mut |_key| unreachable!("没有字体时不该走到栅格化"),
            &|_: &dhampir_core::overlay::TextStyle| None,
            // 测试里不给字体目录（走 `--font-file` 的父目录兜底）。
            &None,
            &mut image,
            &items,
            (640, 360),
            &mut log,
            &mut painter.stats,
        );
        let issues = log.into_vec();
        // 问题代码与字幕共用一对：原因与修法是同一件事（不给字体就画不出字）。
        assert_eq!(codes(&issues), vec!["subtitle_font_missing"]);
        assert_eq!(image, before, "画不出来就不该动像素");
        let stats = painter.stats();
        assert_eq!(stats.danmaku_failed, 1);
        assert_eq!(stats.lines_failed, 0);
        assert_eq!(stats.danmaku_drawn, 0);
    }

    #[test]
    fn 字幕与弹幕同在时两套计数器各归各() {
        let mut painter = OverlayPainter::new(Some(Path::new("C:/fake/font.ttf")));
        let mut image = frame(640, 360, [0, 0, 0, 255]);
        let mut log = IssueLog::new();
        let mut calls = 0;
        let mut items = overlay(&[("字幕一", rect(0.25, 0.8, 0.5, 0.066))], 1);
        items.danmaku.push(DanmakuTextItem {
            text: "弹幕一".to_string(),
            rect: rect(0.6, 0.1, 0.25, 0.066),
            lane: 0,
            enter: 0,
            exit: 100,
            travel_frames: 600,
            opacity: 1.0,
            dy_px: 0.0,
            color: [255, 255, 255, 255],
            font_ratio: 0.04,
        });
        items.dropped_danmaku = 3;
        paint_lines(
            &mut |key| fake_rasterizer(&mut calls)(key),
            &|_: &dhampir_core::overlay::TextStyle| Some(PathBuf::from("C:/fake/font.ttf")),
            // 测试里不给字体目录（走 `--font-file` 的父目录兜底）。
            &None,
            &mut image,
            &items,
            (640, 360),
            &mut log,
            &mut painter.stats,
        );
        assert!(log.is_empty());
        let stats = painter.stats();
        assert_eq!((stats.lines_drawn, stats.lines_dropped), (1, 1));
        assert_eq!((stats.danmaku_drawn, stats.danmaku_dropped), (1, 3));
        assert_eq!(calls, 2, "字幕一行 + 弹幕一条");
    }
}
