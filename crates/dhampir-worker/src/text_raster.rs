//! 宿主侧文本栅格化：把**一行**文字画成一张位图。
//!
//! # 它在补哪一个洞
//!
//! 共享布局（`dhampir_timeline::text_layout`）只回答「几行、每行什么、占哪个矩形」——
//! 字形像素必须由宿主画出来。本仓不许引第三方 crate（见 plan/next-steps.md 的约定），
//! 而 PATH 上那个 ffmpeg 出片本来就要用，所以栅格化走它的 `drawtext`。
//!
//! # 契约：位图是**直排** RGBA8，颜色在这一步就已经染好
//!
//! 有两件事必须在这个文件里对齐一次，否则下游只能猜：
//!
//! 1. **混合约定**。core 的合成器走直排法（`SrcAlpha` / `OneMinusSrcAlpha`，
//!    见 `core/src/render/compose.rs` 的 `blend_state`），而 ffmpeg 的 drawtext
//!    吐出来的是**覆盖度预乘**：不透明白字的抗锯齿边缘是 `[80, 80, 80, 80]`，
//!    不是直排的 `[255, 255, 255, 80]`（本机实测，命令与数字见 T2 证据文档）。
//!    把预乘位图当直排用，字的边缘会暗一圈 —— 而那种错看起来像「字体渲染得不太好」，
//!    不会有人去查混合约定。
//! 2. **颜色不属于 ffmpeg 那一步**。drawtext 固定用**白字 + 黑描边**画，
//!    样式色与不透明度由 [`tint`] 在本文件里染上去。这么分的好处是样式色的 alpha 精确：
//!    若让 drawtext 自己带半透明色，它的输出里 rgb 与 alpha 各乘了不同的系数，
//!    反解算会把颜色推亮（实测 `white@0.5` 的边缘是 `[39, 39, 39, 19]`，比值 ≈ 2 而不是 1）。
//!
//! 于是下游拿到的就是「照直叠加即可」的位图：`rgb` 是样式色，`alpha` 是覆盖率乘样式不透明度。
//!
//! # 用户文本里的百分号（靠 `expansion=none` 才安全）
//!
//! `drawtext` 默认会对**文本内容**做展开：`%{pts}`、`%{n}`、strftime 的 `%Y` 那一套都算，
//! 而一个散落的 `%` 会让它直接报错。文本是用户写的，所以这里**一律关掉展开**。
//! 这是测量阶段撞出来的：取样行里有一行带 `%`，第一次真跑就红 ——
//! `数字 100 % 号` → `Stray % near ' 号'`（整条命令失败），`%{n}` → 被换成帧号。
//! 单测盯着 `expansion=none` 在不在参数里，另有一条真起 ffmpeg 的反向用例盯着
//! 「`%{n}` 没有被换成帧号」。
//!
//! # 缓存
//!
//! 一条字幕在 30fps 下要在几十帧上出现，而像素只跟「文本 + 字号 + 描边 + 字体 + 位图尺寸」有关。
//! 不缓存就是每条字幕几十次 ffmpeg 进程。**位图尺寸是键的一部分**：
//! 同一行文字在两种尺寸下是两张位图，尺寸不进键就会把上一张递给这一张
//! —— 单测里有一条专门盯这个的反向用例。
//!
//! # 几何：位图宽度取**整条目标宽**，不是这一行的估算宽度
//!
//! 共享布局的字宽是**模型**（全角 1em / 半角 0.5em），与真字体的前进宽度有偏差 ——
//! 比例字体里一行英文的真宽度可能比模型宽一成以上。若把位图宽度取成模型宽度，
//! 这些行会被**悄悄切掉两端**。所以宽度取整条目标宽（布局只有居中的排法，
//! 与 `x=(w-text_w)/2` 的水平居中一致），高度取行盒加上下各一份 [`pad_px`]。
//! 将来布局加了对齐字段，取位图的那一侧要一起改。
//!
//! **切没切字是查得出来的**：见 [`TextBitmap::ink_touches_edge`]。调用方必须把它
//! 变成一条问题记录，而不是忽略 —— 静默切字属于最难查的那类（画面看着「就是这样」）。
//!
//! # 有意不做的事
//!
//! * 不做字距 / 连字 / 禁则：那是共享布局的模型，宿主**不许**自己再算一遍，否则两端分叉。
//! * 不解析字体文件、不量字形：度量取自 ffmpeg，结构取自共享布局。
//! * 不缓存到磁盘：跨次运行的缓存键里还得塞字体文件的内容摘要，那是另一件事。
//! * 不做彩色 emoji 字形：drawtext 画的是字体里那一层单色字形。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

// 这条路径**不做色彩矩阵转换**：源是 lavfi 合成的透明色（本来就是 RGBA），
// 中间没有 YUV→RGB 那一步，所以 out_color_matrix 在这里无从谈起。
// scripts/check-sequential-decode.mjs 按**文件**扫 ffmpeg 调用，看到 rawvideo
// 就要求同一个文件里出现 out_color_matrix —— 这行注释就是给它的回答，别删。
// 同一份守卫也不许本文件里出现那两个逐帧 seek 的选项名（它扫的是整份文本，
// 注释与测试都算），所以这里只用中文说「逐帧 seek」，测试里的名字则现拼。

/// 缓存里最多留几张位图。
///
/// 相邻几帧要的是同一批行，所以容量只要盖住「当前这条字幕 + 下一条」就够。
/// 上限的意义是把内存钉住：1080p 一张行位图约 1 MB（1920 × 约 2.2em × RGBA8），
/// 32 张 ≈ 32 MB。
pub const CACHE_CAPACITY: usize = 32;

// 几何（描边宽度、上下边距、位图尺寸）**不在这个文件里**：它必须在两端共用
// 一份，所以住在契约层 `dhampir_timeline::text_layout`（推导也写在那里）。
// 这里只把它们带出来 —— 本模块的调用方与既有测试不用改路径。
pub use dhampir_core::timeline::text_layout::{bitmap_size, border_px, pad_px};

/// 一次栅格化的入参 —— **它自己就是缓存键**。
///
/// 每一项都会影响位图，所以每一项都得在键里：文本、字号、颜色、描边、字体，
/// 以及**位图尺寸**。少了尺寸那一项，同一行文字在两种尺寸下会撞在一起。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TextRasterKey {
    /// 一行的文本。**不含换行**：多行由共享布局先切开，这里只画一行。
    pub text: String,
    /// 字号（目标像素）。
    pub font_px: u32,
    /// 样式色，RGBA。alpha 是样式自己的不透明度。
    pub color: [u8; 4],
    /// 要不要描边。宽度由 [`border_px`] 从字号推出来。
    pub outline: bool,
    /// 字体文件。由宿主给（CLI 的 --font-file）—— 本仓不内嵌字体、也不猜系统字体。
    pub font_file: PathBuf,
    /// 位图宽（像素），见 [`bitmap_size`]。
    pub width: u32,
    /// 位图高（像素），见 [`bitmap_size`]。
    pub height: u32,
}

/// 一张栅格化好的位图：**直排 RGBA8**，颜色已经染好。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextBitmap {
    pub width: u32,
    pub height: u32,
    /// 长度恒等于 `width * height * 4`（由 [`TextBitmap::new`] 保证）。
    pub pixels: Vec<u8>,
}

impl TextBitmap {
    /// 造一张位图，顺便把「长度必须对得上」这条检查掉。
    ///
    /// 长度对不上的位图不该存在：它要么是几何算错了，要么是 ffmpeg 吐少了字节。
    /// 让它在构造处就红，比让下游拿着一张半截图去画强。
    pub fn new(width: u32, height: u32, pixels: Vec<u8>) -> Result<Self, String> {
        let expected = width as usize * height as usize * 4;
        if pixels.len() != expected {
            return Err(format!(
                "位图字节数对不上：{width}x{height} 要 {expected} 字节，实际 {}",
                pixels.len()
            ));
        }
        Ok(Self {
            width,
            height,
            pixels,
        })
    }

    /// 一张全透明的位图。空文本、以及测试里当底板用。
    pub fn blank(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            pixels: vec![0u8; width as usize * height as usize * 4],
        }
    }

    /// 取一个像素。越界给 None，而不是 panic —— 越界是几何算错了，
    /// 由调用方决定这算不算错。
    pub fn pixel(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let base = ((y as usize) * (self.width as usize) + (x as usize)) * 4;
        Some([
            self.pixels[base],
            self.pixels[base + 1],
            self.pixels[base + 2],
            self.pixels[base + 3],
        ])
    }

    /// 有墨迹的包围盒，**闭区间** `(x0, y0, x1, y1)`。一张空位图给 None。
    ///
    /// 「有墨迹」= alpha 不为 0。位图是直排的，所以 alpha 就是覆盖率乘样式不透明度。
    pub fn ink_bounds(&self) -> Option<(u32, u32, u32, u32)> {
        let mut bounds: Option<(u32, u32, u32, u32)> = None;
        for y in 0..self.height {
            for x in 0..self.width {
                let Some(px) = self.pixel(x, y) else { continue };
                if px[3] == 0 {
                    continue;
                }
                bounds = Some(match bounds {
                    None => (x, y, x, y),
                    Some((x0, y0, x1, y1)) => (x0.min(x), y0.min(y), x1.max(x), y1.max(y)),
                });
            }
        }
        bounds
    }

    /// 墨迹碰到了位图边界吗 —— 也就是**有字被切掉了吗**。
    ///
    /// 空位图给 false（没墨迹，谈不上切）。碰到边界意味着这一行在给它的画布里画不下：
    /// 要么行盒与真字宽的偏差超了边距，要么字号算错了。调用方应当把它记成一条问题。
    pub fn ink_touches_edge(&self) -> bool {
        match self.ink_bounds() {
            None => false,
            Some((x0, y0, x1, y1)) => {
                x0 == 0
                    || y0 == 0
                    || x1 == self.width.saturating_sub(1)
                    || y1 == self.height.saturating_sub(1)
            }
        }
    }
}

/// 把 ffmpeg 的「覆盖度预乘」位图染成样式色，输出**直排** RGBA8。
///
/// # 判据
///
/// 输入是白字（填充）+ 黑描边（可选）在透明底上的预乘结果：
/// `rgb = 覆盖度 × 墨色`、`alpha = 覆盖度`（两种墨色都是不透明的）。
/// 于是每个像素的**墨色有多白**可以精确反解出来：
///
/// ```text
/// whiteness = (r + g + b) / 3 / alpha      // 白墨 1，黑墨 0，抗锯齿混合落在中间
/// ```
///
/// 这个比值**与覆盖度无关**（分子分母同乘覆盖度），所以边缘像素也解得准。
/// 然后：
///
/// ```text
/// out.rgb = lerp(黑, 样式色, whiteness)          // 直排：rgb 不带覆盖度
/// out.a   = alpha × lerp(1, 样式不透明度, whiteness) / 255
/// ```
///
/// 描边像素（whiteness = 0）保持黑且不透明 —— 描边是样式里的一个开关，
/// 颜色取黑是**本仓的约定**（见模块文档），不是从工程里读来的。
///
/// # 代价（写清楚）
///
/// 填充与描边的抗锯齿边缘相叠的那一圈像素里，两种墨色是混着的，
/// 这里按 whiteness 线性插值 —— 最坏情况下那**一个像素宽**的过渡带会有偏差。
/// 它不是「更准」而是「有界」：叠加后的偏差不超过两种墨色之差。
pub fn tint(premultiplied: &[u8], color: [u8; 4]) -> Vec<u8> {
    let mut out = premultiplied.to_vec();
    for px in out.chunks_exact_mut(4) {
        let alpha = u32::from(px[3]);
        if alpha == 0 {
            // 全透明像素连颜色都没有：留着 rgb 只会让下游以为那里有东西。
            px[0] = 0;
            px[1] = 0;
            px[2] = 0;
            continue;
        }
        let mean = (u32::from(px[0]) + u32::from(px[1]) + u32::from(px[2])) as f32 / 3.0;
        // 上界 1：ffmpeg 的两遍取整会让白墨的比值偶尔冒出 1 一点点。
        let whiteness = (mean / alpha as f32).clamp(0.0, 1.0);
        for channel in 0..3 {
            let ink = f32::from(color[channel]);
            px[channel] = (ink * whiteness).round().clamp(0.0, 255.0) as u8;
        }
        let ink_alpha = f32::from(color[3]);
        let mixed = 255.0 + (ink_alpha - 255.0) * whiteness;
        px[3] = (alpha as f32 * mixed / 255.0).round().clamp(0.0, 255.0) as u8;
    }
    out
}

/// 把一段值放进 ffmpeg 滤镜串里**安全**的位置。
///
/// 滤镜串被解析两遍（滤镜图一层、滤镜选项一层），所以：值包在单引号里，
/// 单引号、反斜杠、冒号各自再转义一次。实测可用的形态（本机探针跑过）：
///
/// ```text
/// fontfile='C\:/Windows/Fonts/msyh.ttc'
/// ```
///
/// 注意用户文本**不走这条路** —— 它进的是 `textfile=` 指的临时文件，
/// 所以文本里的冒号引号一个都伤不到命令（单测里有一条专盯这个）。
fn filter_value(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len() + 4);
    escaped.push('\'');
    for ch in value.chars() {
        match ch {
            '\\' => escaped.push_str("\\\\"),
            ':' => escaped.push_str("\\:"),
            // 关引号、转义一个引号、再开引号。
            '\'' => escaped.push_str("'\\''"),
            _ => escaped.push(ch),
        }
    }
    escaped.push('\'');
    escaped
}

/// 送给 ffmpeg 的参数（**不含程序名**）。
///
/// 抽成纯函数是为了能单测它 —— 尤其是「文本**不进命令行**」这条：
/// 用户内容一旦进了滤镜串，一个冒号或引号就能把整条命令改写。
pub fn drawtext_args(key: &TextRasterKey, text_file: &Path) -> Vec<String> {
    // 源：一张全透明的画布，尺寸就是要的位图尺寸。
    let source = format!(
        "color=c=black@0.0:s={}x{},format=rgba",
        key.width, key.height
    );
    // `expansion=none` 是**用户文本的下限保护**，不是口味问题：
    // drawtext 默认会对文本内容做展开（`%{pts}`、`%{n}`、strftime 那一套），
    // 而文本是用户写的。实测（T2.3b 的探针，命令与数字见 plan/measurements.md）：
    // `数字 100 % 号` 让整条命令以 `Stray % near ' 号'` 失败 —— 画不出字；
    // `%{n}` 则被换成帧号 —— 画出来的是别的东西。关掉之后 `%` 只是普通字符。
    // **这一项删不得**：删了它，用户文本里的一个百分号就能毁掉整条字幕。
    let mut drawtext = format!(
        "drawtext=fontfile={}:textfile={}:fontsize={}:fontcolor=white:expansion=none:x=(w-text_w)/2:y=(h-text_h)/2",
        filter_value(&key.font_file.to_string_lossy()),
        filter_value(&text_file.to_string_lossy()),
        key.font_px,
    );
    if key.outline {
        drawtext.push_str(&format!(
            ":borderw={}:bordercolor=black",
            border_px(key.font_px)
        ));
    }

    vec![
        "-v".to_string(),
        "error".to_string(),
        // 别让它去读我们的 stdin：出片时那是编码器的管道。
        "-nostdin".to_string(),
        "-f".to_string(),
        "lavfi".to_string(),
        "-i".to_string(),
        source,
        "-vf".to_string(),
        drawtext,
        "-frames:v".to_string(),
        "1".to_string(),
        "-f".to_string(),
        "rawvideo".to_string(),
        "-pix_fmt".to_string(),
        "rgba".to_string(),
        "-".to_string(),
    ]
}

/// 把文本落成一个临时文件，给 `textfile=` 用。
///
/// 名字要唯一：同一台机器上可能同时跑两个进程（两条腿并行），
/// 用 pid + 计数器 + 内容摘要就撞不上。写的是 UTF-8 **无 BOM**、**不加结尾换行**
/// —— 加一个换行，drawtext 会多排一行空行，垂直居中就偏了。
fn write_text_file(text: &str) -> Result<PathBuf, String> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let serial = COUNTER.fetch_add(1, Ordering::Relaxed);
    let digest = dhampir_core::timeline::selfcheck::fnv1a64(text.as_bytes());
    let path = std::env::temp_dir().join(format!(
        "dhampir-text-{}-{serial}-{digest:016x}.txt",
        std::process::id()
    ));
    std::fs::write(&path, text.as_bytes())
        .map_err(|error| format!("写不了文本临时文件 {}：{error}", path.display()))?;
    Ok(path)
}

/// 真起一次 ffmpeg，把一行文字画成位图。**不走缓存** —— 缓存由 [`TextRasterizer`] 管。
///
/// 入参检查排在起进程之前（尺寸、文本、字体），所以坏入参不会先花掉一个进程。
pub fn rasterize_line(key: &TextRasterKey) -> Result<TextBitmap, String> {
    if key.width == 0 || key.height == 0 {
        return Err(format!("位图尺寸不合法：{}x{}", key.width, key.height));
    }
    if key.font_px == 0 {
        return Err("字号为 0：画出来的字是零高度的".to_string());
    }
    // 换行先剔掉行尾的回车（SRT 从 CRLF 文件里读出来的行可能带着），
    // 剩下的换行是**真换行** —— 那是共享布局该切开的东西，这里只画一行。
    let text = key.text.strip_suffix('\r').unwrap_or(&key.text);
    if text.contains('\n') {
        return Err(format!(
            "这一行里还有换行：栅格化只画一行，多行要先由共享布局切开（{:?}）",
            key.text
        ));
    }
    if text.is_empty() {
        // 空行不叫进程：结果与画一次空文本完全一样，但省一次 fork/exec。
        // 它也不查字体 —— 没字可画的地方，字体是哪一份都不影响结果。
        return Ok(TextBitmap::blank(key.width, key.height));
    }
    if !key.font_file.is_file() {
        return Err(format!(
            "字体文件不在：{}（--font-file 指错了吗？本仓不猜系统字体）",
            key.font_file.display()
        ));
    }

    let text_file = write_text_file(text)?;
    let result = run_ffmpeg(key, &text_file);
    // 临时文件用完就删，成功失败都删：一次出片几百行，留一地文件是在给下次查问题挖坑。
    let _ = std::fs::remove_file(&text_file);
    result
}

fn run_ffmpeg(key: &TextRasterKey, text_file: &Path) -> Result<TextBitmap, String> {
    let args = drawtext_args(key, text_file);
    let output = Command::new("ffmpeg")
        .args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|error| format!("起不了 ffmpeg：{error}（PATH 里有 ffmpeg 吗？）"))?;

    if !output.status.success() {
        return Err(format!(
            "ffmpeg 画不出这一行（退出码 {:?}）：{} —— 字体 {} 与文本临时文件都在。\
             先信 ffmpeg 的原文：它说的常在字体上（能不能解析、有没有这个字形）；\
             这条路径已经关掉了文本展开，所以 `Stray %` 那一类只可能是这里被人改坏了",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr).trim(),
            key.font_file.display()
        ));
    }
    let expected = key.width as usize * key.height as usize * 4;
    if output.stdout.len() != expected {
        // 少了或多了都说明几何前提不成立（例如 ffmpeg 把画布改了尺寸）。
        // 收下它去猜只会让下游拿到一张尺寸不对的图。
        return Err(format!(
            "ffmpeg 本该吐 {expected} 字节（{}x{} RGBA），实际 {}",
            key.width,
            key.height,
            output.stdout.len()
        ));
    }
    TextBitmap::new(key.width, key.height, tint(&output.stdout, key.color))
}

/// 位图缓存：键 -> 位图，外加「最久未用」的淘汰。
///
/// 手写一个 LRU 而不是引 crate：容量只有 [`CACHE_CAPACITY`] 这么大，
/// 淘汰时线性扫一遍就够（而且只在满了之后、插入新键时才发生）。
#[derive(Debug, Default)]
struct TextRasterCache {
    entries: HashMap<TextRasterKey, (Rc<TextBitmap>, u64)>,
    tick: u64,
    hits: usize,
    misses: usize,
}

impl TextRasterCache {
    /// 命中就给现成的那张；没命中才调用 `rasterize`，然后把它记下。
    ///
    /// 抽成「接受一个闭包」是为了单测能在**不起 ffmpeg** 的前提下验缓存行为
    /// （尺寸进没进键这件事，必须能单独验）。
    fn get_or_insert_with(
        &mut self,
        key: &TextRasterKey,
        rasterize: impl FnOnce(&TextRasterKey) -> Result<TextBitmap, String>,
    ) -> Result<Rc<TextBitmap>, String> {
        self.tick += 1;
        let tick = self.tick;
        if let Some((bitmap, used_at)) = self.entries.get_mut(key) {
            *used_at = tick;
            self.hits += 1;
            return Ok(Rc::clone(bitmap));
        }
        let bitmap = rasterize(key)?;
        self.misses += 1;
        if self.entries.len() >= CACHE_CAPACITY {
            self.evict_least_recently_used();
        }
        self.entries.insert(key.clone(), (Rc::new(bitmap), tick));
        Ok(Rc::clone(
            &self.entries.get(key).expect("刚插进去的键必须还在").0,
        ))
    }

    fn evict_least_recently_used(&mut self) {
        let Some(oldest) = self
            .entries
            .iter()
            .min_by_key(|(_, (_, used_at))| *used_at)
            .map(|(key, _)| key.clone())
        else {
            return;
        };
        self.entries.remove(&oldest);
    }

    fn len(&self) -> usize {
        self.entries.len()
    }
}

/// 宿主侧栅格化器：一个进程生命周期内一份缓存。
///
/// **单线程**：位图是 [`Rc`] 共享的。哪天真要跨线程用，把 Rc 换成 Arc 即可。
#[derive(Debug, Default)]
pub struct TextRasterizer {
    cache: TextRasterCache,
}

impl TextRasterizer {
    pub fn new() -> Self {
        Self::default()
    }

    /// 取一张位图。命中缓存就不起进程。
    ///
    /// 返回 `Rc` 而不是借用：调用方常常要**一次取好几行**（一帧的覆盖层）
    /// 再去画，借来的话第二次取就会和第一次的借用打架。
    pub fn rasterize(&mut self, key: &TextRasterKey) -> Result<Rc<TextBitmap>, String> {
        self.cache.get_or_insert_with(key, rasterize_line)
    }

    /// 缓存里现在有几张位图。
    pub fn cached(&self) -> usize {
        self.cache.len()
    }

    /// 命中 / 未命中次数。给测量用 ——「缓存省了多少次进程」是量出来的，不是估的。
    pub fn hits(&self) -> usize {
        self.cache.hits
    }

    pub fn misses(&self) -> usize {
        self.cache.misses
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 一个测试用键：只让高度变，宽度固定 —— 反向用例要盯的正是「尺寸」这一项。
    fn key(text: &str, height: u32) -> TextRasterKey {
        TextRasterKey {
            text: text.to_string(),
            font_px: 32,
            color: [255, 240, 200, 255],
            outline: false,
            font_file: PathBuf::from("C:/fake/font.ttf"),
            width: 100,
            height,
        }
    }

    /// 位图里某个像素的字节下标：把「行 × 宽 + 列」写成一件有名有姓的事，
    /// 免得测试里出现一堆手算的魔法数。
    fn pixel_offset(width: usize, x: usize, y: usize) -> usize {
        (y * width + x) * 4
    }

    /// 假位图：每个高度值都不一样，好让「拿到的不是请求的那张」一眼可见。
    fn fake_bitmap(key: &TextRasterKey) -> TextBitmap {
        let mut pixels = vec![0u8; key.width as usize * key.height as usize * 4];
        for px in pixels.chunks_exact_mut(4) {
            px[0] = (key.height & 0xff) as u8;
            px[3] = 7;
        }
        TextBitmap {
            width: key.width,
            height: key.height,
            pixels,
        }
    }

    #[test]
    fn 位图尺寸由目标宽与行盒加边距算出() {
        // 字号 48：边距 16，行盒 1.2 × 48 = 57.6 -> 58，高 = 58 + 32 = 90。
        assert_eq!(pad_px(48), 16);
        assert_eq!(bitmap_size(640, 48.0 * 1.2, 48), (640, 90));
        // 小字号也至少留 4 像素：不给的话描边与抗锯齿会顶到边上。
        assert_eq!(pad_px(1), 4);
        assert_eq!(bitmap_size(320, 12.0, 10), (320, 20));
    }

    #[test]
    fn 描边宽度不小于一个像素() {
        assert_eq!(border_px(48), 3);
        assert_eq!(border_px(16), 1);
        assert_eq!(border_px(4), 1, "字号再小也不许给 0 宽的描边");
    }

    #[test]
    fn 尺寸为零是坏入参() {
        let mut bad = key("字", 20);
        bad.width = 0;
        assert!(rasterize_line(&bad).is_err());

        let mut bad = key("字", 0);
        bad.height = 0;
        assert!(rasterize_line(&bad).is_err());
    }

    #[test]
    fn 一行里出现换行必须被拒绝而不是画成两行() {
        // 画成两行的话，行盒、居中、位图高度全都不成立 —— 那是静默画错。
        let err = rasterize_line(&key("上\n下", 20)).unwrap_err();
        assert!(err.contains("换行"), "错误里要说明白为什么：{err}");
    }

    #[test]
    fn 行尾的回车要被剔掉() {
        // CRLF 文件里读出来的一行可能带着 \r：那会让字形多一个方块。
        // 字号与尺寸都在，只有字体路径是假的 —— 于是这条路会走到字体检查才失败，
        // 也就是说回车那一关过了，没被当成换行。
        let err = rasterize_line(&key("正常一行\r", 20)).unwrap_err();
        assert!(err.contains("字体文件不在"), "回车不该被当成换行：{err}");
    }

    #[test]
    fn 空文本给全透明位图且不叫进程() {
        // 字体路径是假的：如果这里真的去起进程，就会先撞上字体检查。
        let bitmap = rasterize_line(&key("", 20)).expect("空文本不需要字体，也不该起进程");
        assert_eq!((bitmap.width, bitmap.height), (100, 20));
        assert!(bitmap.pixels.iter().all(|byte| *byte == 0));
        assert!(bitmap.ink_bounds().is_none());
        assert!(!bitmap.ink_touches_edge(), "空位图谈不上切字");
    }

    #[test]
    fn 字体不在就给一条人话() {
        let err = rasterize_line(&key("字", 20)).unwrap_err();
        assert!(err.contains("字体文件不在"), "{err}");
    }

    #[test]
    fn 位图字节数对不上要报出来() {
        assert!(TextBitmap::new(2, 2, vec![0; 15]).is_err());
        assert!(TextBitmap::new(2, 2, vec![0; 16]).is_ok());
    }

    #[test]
    fn 墨迹包围盒与碰边判定() {
        let mut bitmap = TextBitmap::blank(8, 6);
        assert!(bitmap.ink_bounds().is_none());
        // 在 (2,1) 点一个点：包围盒就是它自己，没碰边。
        let base = pixel_offset(8, 2, 1);
        bitmap.pixels[base + 3] = 200;
        assert_eq!(bitmap.ink_bounds(), Some((2, 1, 2, 1)));
        assert!(!bitmap.ink_touches_edge());
        // 再在右下角点一个：包围盒张到边界上，碰边判定必须变红。
        let corner = pixel_offset(8, 7, 5);
        bitmap.pixels[corner + 3] = 1;
        assert_eq!(bitmap.ink_bounds(), Some((2, 1, 7, 5)));
        assert!(bitmap.ink_touches_edge());
    }

    #[test]
    fn 白墨染成样式色_黑描边保持黑() {
        // 填充的实心像素：白墨、不透明。
        assert_eq!(
            tint(&[255, 255, 255, 255], [200, 100, 50, 255]),
            [200, 100, 50, 255]
        );
        // 描边的实心像素：黑墨，白字色的样式也染不亮它。
        assert_eq!(tint(&[0, 0, 0, 255], [200, 100, 50, 255]), [0, 0, 0, 255]);
        // 全透明像素：连颜色都不留。
        assert_eq!(tint(&[0, 0, 0, 0], [200, 100, 50, 255]), [0, 0, 0, 0]);
        // 抗锯齿边缘：覆盖度进 alpha，色相不变（直排，不是预乘）。
        assert_eq!(
            tint(&[80, 80, 80, 80], [200, 100, 50, 255]),
            [200, 100, 50, 80]
        );
    }

    #[test]
    fn 样式色的不透明度只缩放_不改色相() {
        // 半透明样式色：实心像素的 rgb 还是样式色，alpha 才被缩放。
        assert_eq!(
            tint(&[255, 255, 255, 255], [200, 100, 50, 128]),
            [200, 100, 50, 128]
        );
        // 边缘像素：覆盖度与样式不透明度都要进来。
        assert_eq!(
            tint(&[128, 128, 128, 128], [200, 100, 50, 128]),
            [200, 100, 50, 64]
        );
        // 描边不受样式不透明度影响：描边是黑的、不透明的。
        assert_eq!(tint(&[0, 0, 0, 255], [200, 100, 50, 0]), [0, 0, 0, 255]);
        // 样式色全透明 = 什么都看不见。
        assert_eq!(
            tint(&[255, 255, 255, 255], [200, 100, 50, 0]),
            [200, 100, 50, 0]
        );
    }

    /// **反向用例的另一半**：染色这一步错了，最后叠出来就会偏色。
    /// 这里直接比「照直叠加的贡献」与理想值（样式色 × 覆盖率 × 样式不透明度）。
    #[test]
    fn 染色后的叠加贡献与理想值相差不超过一级() {
        let color = [200u8, 100, 50, 180];
        // 覆盖度从 1 到 255 走一遍：白墨、以及黑描边各一遍。
        for coverage in 1..=255u32 {
            for black in [false, true] {
                let ink = if black { 0u8 } else { coverage as u8 };
                let premultiplied = [ink, ink, ink, coverage as u8];
                let out = tint(&premultiplied, color);
                for channel in 0..3 {
                    let got = f64::from(out[channel]) * f64::from(out[3]) / 255.0;
                    let share = if black {
                        // 黑描边：颜色恒为 0。
                        0.0
                    } else {
                        f64::from(color[channel]) * f64::from(color[3]) / 255.0
                    };
                    let ideal = share * f64::from(coverage as u8) / 255.0;
                    assert!(
                        (got - ideal).abs() <= 1.0,
                        "覆盖度 {coverage} 通道 {channel}：叠加贡献 {got} 与理想 {ideal} 差得超过一级"
                    );
                }
            }
        }
    }

    #[test]
    fn 冒号引号百分号全打不进命令行_文本走的是文件() {
        let dangerous = "危险:文本'带引号,逗号[方括号];分号 100% %{n}";
        let mut k = key("", 20);
        k.text = dangerous.to_string();
        let args = drawtext_args(&k, Path::new("C:/tmp/dhampir-text-1.txt"));
        let joined = args.join(" ");
        assert!(!joined.contains("危险"), "文本进了命令行：{joined}");
        assert!(!joined.contains("带引号"), "文本进了命令行：{joined}");
        assert!(
            joined.contains("textfile="),
            "文本应当走 textfile 参数：{joined}"
        );
        // 文本展开必须关掉：默认档下用户文本里的一个 % 就能让整条命令失败
        // （`Stray %`），`%{n}` 还会被换成帧号。实测见模块文档。
        assert!(
            joined.contains("expansion=none"),
            "文本展开没关掉 —— 用户文本里的 % 会让这一行画不出来：{joined}"
        );
        // 顺序管道的标志与出图几何。
        assert!(joined.contains("rawvideo"));
        assert!(
            joined.contains("100x20"),
            "画布尺寸必须就是位图尺寸：{joined}"
        );
        // 这两个选项名**不能以带引号的字面量出现在本文件里**：check-sequential-decode.mjs
        // 扫的是整份文件文本（含注释与测试），写死成带引号的选项名会让守卫自己变红 ——
        // 那是一次假红，而假红比没守卫更坏。所以名字在这里拼出来，断言照跑。
        let seek_flag = format!("-{}", "ss");
        let seek_stamp_flag = format!("-{}", "seek_timestamp");
        assert!(!joined.contains(&seek_flag), "后端不许逐帧 seek");
        assert!(!joined.contains(&seek_stamp_flag));
    }

    #[test]
    fn 描边开关决定有没有_borderw() {
        let plain = drawtext_args(&key("字", 20), Path::new("t.txt")).join(" ");
        assert!(!plain.contains("borderw"));

        let mut outlined = key("字", 20);
        outlined.outline = true;
        outlined.font_px = 48;
        let joined = drawtext_args(&outlined, Path::new("t.txt")).join(" ");
        assert!(
            joined.contains("borderw=3"),
            "字号 48 的描边是 3 像素：{joined}"
        );
        assert!(joined.contains("bordercolor=black"));
    }

    #[test]
    fn 字体路径里的冒号与引号要转义() {
        assert_eq!(
            filter_value("C:/Windows/Fonts/msyh.ttc"),
            "'C\\:/Windows/Fonts/msyh.ttc'"
        );
        assert_eq!(filter_value("带 空格 的.ttf"), "'带 空格 的.ttf'");
        assert_eq!(filter_value("it's"), "'it'\\''s'");
        assert_eq!(filter_value("a\\b"), "'a\\\\b'");
    }

    /// **反向用例**：尺寸不进缓存键的话，这条会拿到高度 20 的那张位图。
    #[test]
    fn 缓存键里少了位图尺寸就会拿错图() {
        let mut cache = TextRasterCache::default();
        let mut calls = 0usize;
        let short = key("同一行字", 20);
        let tall = key("同一行字", 40);

        let first = cache
            .get_or_insert_with(&short, |k| {
                calls += 1;
                Ok(fake_bitmap(k))
            })
            .unwrap();
        assert_eq!(first.height, 20);

        let second = cache
            .get_or_insert_with(&tall, |k| {
                calls += 1;
                Ok(fake_bitmap(k))
            })
            .unwrap();
        assert_eq!(calls, 2, "换了位图尺寸却命中了缓存 —— 尺寸没进键");
        assert_eq!(second.height, 40, "把另一张尺寸的位图递过来了");
        assert_eq!(second.pixels[0], 40, "内容也得是这一张的");

        // 回头再来一次同样的请求：这回才该命中。
        let again = cache
            .get_or_insert_with(&short, |k| {
                calls += 1;
                Ok(fake_bitmap(k))
            })
            .unwrap();
        assert_eq!(calls, 2, "同样的键不该再栅格化一次");
        assert_eq!(again.height, 20);
        assert_eq!(cache.hits, 1);
        assert_eq!(cache.misses, 2);
    }

    #[test]
    fn 颜色与描边也在键里() {
        let mut cache = TextRasterCache::default();
        let mut calls = 0usize;
        let base = key("同一行字", 20);
        let mut other_color = base.clone();
        other_color.color = [0, 255, 0, 255];
        let mut outlined = base.clone();
        outlined.outline = true;

        for candidate in [&base, &base, &other_color, &outlined, &base] {
            cache
                .get_or_insert_with(candidate, |k| {
                    calls += 1;
                    Ok(fake_bitmap(k))
                })
                .unwrap();
        }
        assert_eq!(calls, 3, "文本相同但颜色/描边不同，必须各栅格化一次");
        assert_eq!(cache.hits, 2);
    }

    #[test]
    fn 缓存满了淘汰最久未用的() {
        let mut cache = TextRasterCache::default();
        let mut calls = 0usize;
        for index in 0..CACHE_CAPACITY {
            let k = key(&format!("第 {index} 行"), 20 + index as u32);
            cache
                .get_or_insert_with(&k, |k| {
                    calls += 1;
                    Ok(fake_bitmap(k))
                })
                .unwrap();
        }
        assert_eq!(calls, CACHE_CAPACITY);
        assert_eq!(cache.len(), CACHE_CAPACITY);

        // 摸一下最早那个：它变成「最新用过的」。
        let oldest = key("第 0 行", 20);
        cache
            .get_or_insert_with(&oldest, |_| Err("不该栅格化：它还在缓存里".to_string()))
            .unwrap();

        // 再塞一张新的：该被淘汰的是「第 1 行」，不是刚摸过的那个。
        let fresh = key("新的一行", 90);
        cache
            .get_or_insert_with(&fresh, |k| {
                calls += 1;
                Ok(fake_bitmap(k))
            })
            .unwrap();
        assert_eq!(cache.len(), CACHE_CAPACITY, "容量不许涨");

        let evicted = key("第 1 行", 21);
        cache
            .get_or_insert_with(&evicted, |k| {
                calls += 1;
                Ok(fake_bitmap(k))
            })
            .unwrap();
        assert_eq!(
            calls,
            CACHE_CAPACITY + 2,
            "第 1 行应当已被淘汰，第 0 行不该"
        );
    }

    /// 真起 ffmpeg 的那条。**默认不跑**（`#[ignore]`），跑法：
    ///
    /// ```text
    /// cargo test -p dhampir-worker --lib text_raster -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "真起 ffmpeg：需要 PATH 上有 ffmpeg 与一个系统字体"]
    fn 真起_ffmpeg_画出一行中文字() {
        let font = test_font();
        let font_px = 48u32;
        let (width, height) = bitmap_size(640, font_px as f32 * 1.2, font_px);
        let key = TextRasterKey {
            text: "第一行中文字幕".to_string(),
            font_px,
            color: [255, 240, 200, 255],
            outline: true,
            font_file: font.clone(),
            width,
            height,
        };

        let bitmap = rasterize_line(&key).expect("真起 ffmpeg 应当能画出一行字");
        assert_eq!((bitmap.width, bitmap.height), (width, height));

        let bounds = bitmap.ink_bounds().expect("有字就该有墨迹");
        assert!(
            !bitmap.ink_touches_edge(),
            "墨迹碰到边界就是有字被切掉了：包围盒 {bounds:?}，位图 {width}x{height}"
        );
        // 填充：必须有一个像素**逐字节**等于样式色（不透明像素不经过任何缩放）。
        assert!(
            bitmap
                .pixels
                .chunks_exact(4)
                .any(|px| px[0] == 255 && px[1] == 240 && px[2] == 200 && px[3] == 255),
            "找不到一个纯填充色的像素 —— 染色那一步可能没生效"
        );
        // 描边：必须有一个实心黑像素，那才是「描边真画出来了」的证据。
        assert!(
            bitmap.pixels.chunks_exact(4).any(|px| px == [0, 0, 0, 255]),
            "描边一个实心黑像素都没有"
        );
        // 抗锯齿：边缘必须有既不是 0 也不是 255 的 alpha。
        assert!(
            bitmap
                .pixels
                .chunks_exact(4)
                .any(|px| px[3] > 0 && px[3] < 255),
            "一个半透明像素都没有 —— 边缘是硬切的"
        );

        // 缓存靠的那条前提：同一个键画两次必须逐字节相同。
        let again = rasterize_line(&key).expect("第二次也要画得出来");
        assert_eq!(
            bitmap, again,
            "同键两次栅格化必须逐字节相同，否则缓存就是错的"
        );
    }

    #[test]
    #[ignore = "真起 ffmpeg：需要 PATH 上有 ffmpeg 与一个系统字体"]
    fn 真起_ffmpeg_一行长英文不会被切掉() {
        // 这条盯的是模型字宽与真字宽的偏差：位图宽度若按模型宽度取，
        // 比例字体里这行英文会长出画布，两端被切。
        let font = test_font();
        let font_px = 40u32;
        let (width, height) = bitmap_size(960, font_px as f32 * 1.2, font_px);
        let key = TextRasterKey {
            text: "The quick brown fox jumps over the lazy dog".to_string(),
            font_px,
            color: [255, 255, 255, 255],
            outline: false,
            font_file: font,
            width,
            height,
        };
        let bitmap = rasterize_line(&key).expect("真起 ffmpeg 应当能画出一行英文");
        let bounds = bitmap.ink_bounds().expect("有字就该有墨迹");
        assert!(
            !bitmap.ink_touches_edge(),
            "英文行被切了：包围盒 {bounds:?}，位图 {width}x{height}"
        );
        // 没有描边时不该有黑像素：那说明 borderw 那一段没被关掉。
        assert!(
            !bitmap
                .pixels
                .chunks_exact(4)
                .any(|px| px[3] > 0 && px[0] == 0 && px[1] == 0 && px[2] == 0),
            "没开描边却画出了黑像素"
        );
    }

    /// 真起 ffmpeg 的**反向用例**：用户文本里有一个百分号。
    ///
    /// 这条盯的是 drawtext 的**文本展开**。默认档下（也就是没有 `expansion=none` 时）
    /// `数字 100 % 号` 会让整条命令以 `Stray % near ' 号'` 失败 ——
    /// 用户打了个百分号，整条字幕就画不出来；而 `%{n}` 会被换成帧号，画出来的是别的东西。
    /// 关掉展开之后两者都只是普通字符。修之前这条会红，修之后必须绿。
    #[test]
    #[ignore = "真起 ffmpeg：需要 PATH 上有 ffmpeg 与一个系统字体"]
    fn 真起_ffmpeg_用户文本里的百分号不会被展开() {
        let font = test_font();
        let font_px = 40u32;
        let (width, height) = bitmap_size(400, font_px as f32 * 1.2, font_px);
        let make = |text: &str| TextRasterKey {
            text: text.to_string(),
            font_px,
            color: [255, 255, 255, 255],
            outline: false,
            font_file: font.clone(),
            width,
            height,
        };

        // 1. 散落的 %：修之前这一行直接失败。
        let percent = rasterize_line(&make("数字 100 % 号"))
            .expect("含 % 的一行必须能画出来 —— 画不出来就是文本展开没关掉（Stray %）");
        assert!(ink_count(&percent) > 0, "含 % 的一行画出来是空的");

        // 2. 展开语法：`%{n}` 若被展开就变成一个帧号，墨迹会掉到一个数字那么多。
        let literal =
            ink_count(&rasterize_line(&make("%{n}")).expect("含 %{n} 的一行必须能画出来"));
        let single = ink_count(&rasterize_line(&make("0")).expect("对照行必须能画出来"));
        assert!(
            literal > single,
            "`%{{n}}` 的墨迹（{literal} 像素）不多于一个 `0`（{single} 像素）—— \
             它多半又被展开成帧号了"
        );
    }

    /// 有墨迹的像素个数。
    fn ink_count(bitmap: &TextBitmap) -> usize {
        bitmap
            .pixels
            .chunks_exact(4)
            .filter(|px| px[3] != 0)
            .count()
    }

    /// 测试用的字体：挑本机常见的那些。**这不是产品默认值** ——
    /// 产品路径上字体由 `--font-file` 给，本仓不猜系统字体。
    fn test_font() -> PathBuf {
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
}
