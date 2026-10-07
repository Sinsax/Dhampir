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
//! # 非 ASCII 字体文件名：ffmpeg 会**静默**画不出来
//!
//! `drawtext` 的 `fontfile=` 走的是 ffmpeg 内部的 fontconfig 查找路径，而那条路对
//! 非 ASCII 文件名不可靠。本机实测（ffmpeg 9.0.1 gyan build，命令与数字见
//! `plan/glyph-fallback-evidence.md`）：同一份字体，
//!
//! | 字体路径 | ffmpeg 退出码 | 产出字节 | stderr |
//! |---|---|---|---|
//! | `…/乐米波波体（免费商用）_爱给网_aigei_com.ttf` | 0 | **0** | `Fontconfig error: Cannot load default config file` |
//! | 同一份复制成 `lemi_ascii.ttf` | 0 | 96000（3184 个非零 alpha） | 空 |
//!
//! 注意退出码是 **0**、画布尺寸也没错 —— 它只是**什么都不吐**。这正是最坏的一种失败：
//! 调用方拿到的不是错误而是**一行看不见的字**，而「字没画出来」在看片时像「这一行没有字幕」。
//! 现有的 `run_ffmpeg` 有一条「产出字节数对不上就报错」的检查，非零退出码那条挡不住这里，
//! 挡住它的是**字节数**那条 —— 那是本模块敢在这条路上犯错的前提。
//!
//! 用户决策：**让它能画**，不是只报错。做法是把字体复制到一份 ASCII 名的临时路径再喂给
//! ffmpeg（见 [`ascii_font_path`]），用完即删。
//!
//! # 有意不做的事
//!
//! * 不做字距 / 连字 / 禁则：那是共享布局的模型，宿主**不许**自己再算一遍，否则两端分叉。
//! * 不解析字体文件、不量字形：度量取自 ffmpeg，结构取自共享布局。
//! * 不缓存到磁盘：跨次运行的缓存键里还得塞字体文件的内容摘要，那是另一件事。
//! * 不做彩色 emoji 字形：drawtext 画的是字体里那一层单色字形。
//! * **不改字体集合、不猜系统字体**：产品路径上字体仍由 `--font-file` 给。
//!   上面那个复制**只改路径的写法，不改用的是哪一份字体** —— 内容摘要进临时名，
//!   所以「挪一份字体」与「换一份字体」在参数串上是可区分的。

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
    ///
    /// 带 `.hl` 时这是**其中一段**（渲染侧逐段各要一次栅格化，见 `paint_one`）。
    pub text: String,
    /// **这一段相对"整行居中位置"的水平偏移**（目标像素）。
    ///
    /// 0 = 老行为（整行居中画完）。非 0 时 `drawtext` 的 `x` 加上它 ——
    /// 于是"某一段"能画在行内的正确位置上，而**不必引第二个栅格器**。
    pub x_offset: i32,
    /// 字号（目标像素）。
    pub font_px: u32,
    /// 样式色，RGBA。alpha 是样式自己的不透明度。
    pub color: [u8; 4],
    /// 要不要描边。宽度由 `stroke_px` 说了算。
    pub outline: bool,
    /// **描边宽度（目标像素）。** 由 `stroke_ratio * 目标高` 换算而来
    /// （换算在 `overlay::evaluate_overlay` 里做，因为只有它知道目标尺寸）。
    ///
    /// 以前宽度是从字号推的（`border_px(font_px)`）—— 那是"参照实现 的 12px
    /// 与 2px 差不多能对上"的巧合，不是契约。参照实现 的字幕 `12px`、
    /// 弹幕 `2px`，两者字号也不同，从字号推必然对不上。
    pub stroke_px: u32,
    /// 描边颜色。参照实现 字幕是 `#403c3b`、弹幕是 `#000`。
    pub stroke_color: [u8; 4],
    /// **这一张是不是"阴影"那一张**（`Some` = 是，颜色就是它）。
    ///
    /// `None` = 文本位图本身，也就是**老路**：命令行参数与画出来的像素都必须逐字节不变。
    ///
    /// `Some` 时 `drawtext` 只画**白字**（**不带描边** —— 阴影是"同一行字按偏移
    /// 再画一遍"，不参与描边宽度），后面接一道 `gblur`，颜色仍由 [`tint`] 染上去。
    /// 于是"白墨 -> 样式色"这条既有的分工一点没变，模糊只是多了一道覆盖度处理。
    pub shadow_color: Option<[u8; 4]>,
    /// 阴影的模糊半径（**目标像素**；0 = 硬阴影）。
    ///
    /// 进键是因为它决定 `gblur` 的 σ —— 也正是"位图尺寸必须进键"那条纪律的
    /// 另一个实例：模糊半径变了，整张位图都要重画。
    pub shadow_blur_px: u32,
    /// 阴影的水平偏移（目标像素）。进键的理由是**扩边量随它变**（见 [`shadow_pad_px`]）。
    pub shadow_dx_px: i32,
    /// 阴影的垂直偏移（目标像素，正数向下）。
    pub shadow_dy_px: i32,
    /// 阴影位图四周扩出来的边（像素），由 [`shadow_pad_px`] 算出来。
    ///
    /// 它本来可以从 `width` 与"文本位图宽"反推，但**显式存一份**：叠加那一步要用它
    /// 把原点挪回去（`x = 文本原点 - pad + dx`），而"从两条尺寸反推 padding"
    /// 是那种一旦哪一边改了口径就会**静默**漂的算法。（文本位图的键里它是 0。）
    pub shadow_pad: u32,
    /// 字体文件。由宿主给（CLI 的 --font-file）—— 本仓不内嵌字体、也不猜系统字体。
    pub font_file: PathBuf,
    /// 位图宽（像素），见 [`bitmap_size`]。
    pub width: u32,
    /// 位图高（像素），见 [`bitmap_size`]。
    pub height: u32,
}

impl TextRasterKey {
    /// 这一张位图要染成什么色：**阴影位图用阴影色**，普通位图用样式色。
    fn tint_color(&self) -> [u8; 4] {
        self.shadow_color.unwrap_or(self.color)
    }
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

/// RGBA -> ffmpeg 认的 `0xRRGGBB`（**丢掉 alpha**）。
///
/// 描边色不需要 alpha：它压在文字底下，半透明的描边看起来像"字边上脏了一圈"，
/// 而 参照实现 给的也是不透明的颜色（`#403c3b` / `#000`）。
fn border_color_value(color: [u8; 4]) -> String {
    format!("0x{:02X}{:02X}{:02X}", color[0], color[1], color[2])
}

// ---------------------------------------------------------------------------
// 文字阴影：几何与模糊半径的口径
//
// 这一段全是**纯函数**，因为它们是两端"看起来差不多"这件事里唯一说得清的部分：
// canvas 那边的 `shadowBlur` / `shadowOffsetX/Y` 与这里的 σ / pad 靠它们对齐。
// ---------------------------------------------------------------------------

/// 阴影模糊半径（像素）-> ffmpeg `gblur` 的 σ（像素）。
///
/// # 为什么除以 2（这就是"观感近似"的来源）
///
/// canvas 的 `shadowBlur` **不是**高斯 σ：规范与两个主流实现跑的是
/// `σ = shadowBlur / 2`（`shadowBlur` 更像"模糊直径的量级"）。
/// 而 ffmpeg 的 `gblur=sigma=` 直接吃 σ。所以同一个数进两边要差一个 2 ——
/// **这个 2 是一次对齐，不是一条共用公式**，将来任一侧改了核，这里就是第一个该动的地方。
///
/// 两端**不保证逐像素一致**（与"字形像素允许不同"同一条口径），
/// 依据见 `plan/text-shadow-design.md` §2 的 B 方案。
pub fn shadow_sigma_px(blur_px: u32) -> f32 {
    blur_px as f32 / 2.0
}

/// 阴影位图四周要扩出来的边（像素）。
///
/// 两样东西要吃这份余量，**缺一样就会被自己的画布切掉**：
///
/// * **模糊的扩散**：高斯在 3σ 之外的贡献可以忽略，而 σ = blur/2，
///   所以 3σ = 1.5 × blur —— 这里给到 2 × blur，留够。
/// * **偏移**：阴影整张要挪 (dx, dy)，画布不跟着长，挪出去的那部分就没了。
///
/// 用 `max(|dx|, |dy|)` 而不是 `|dx| + |dy|`：两者是横竖两个方向各自需要的余量，
/// 四周等量扩边时取大的那一个就够（两个方向不会同时把同一侧吃掉）。
pub fn shadow_pad_px(blur_px: u32, dx: i32, dy: i32) -> u32 {
    let spread = blur_px as f32 * 2.0;
    let shift = dx.unsigned_abs().max(dy.unsigned_abs()) as f32;
    (spread + shift).ceil() as u32
}

/// 从**文本位图那把键**派生**阴影位图那把键**。
///
/// 除画布尺寸与那四个阴影项之外**逐字段相同**：同一个文本、同一个字号、
/// 同一个行内偏移、同一个字体。于是"阴影就是同一行字按偏移再画一遍"这件事
/// 在代码里也是一句话。
///
/// `pad` 由 [`shadow_pad_px`] 算出，位图在四周各扩 `pad` —— 叠加那一侧据此把
/// 原点挪回去（`原点 = 文本原点 - pad + 偏移`），见 `text_overlay::paint_shadow`。
pub fn shadow_key(
    text: &TextRasterKey,
    color: [u8; 4],
    blur_px: u32,
    dx: i32,
    dy: i32,
) -> TextRasterKey {
    debug_assert!(
        text.shadow_color.is_none(),
        "阴影键要从**文本**那把键派生 —— 从阴影键再派生一次会把扩边叠加两遍"
    );
    let pad = shadow_pad_px(blur_px, dx, dy);
    TextRasterKey {
        shadow_color: Some(color),
        shadow_blur_px: blur_px,
        shadow_dx_px: dx,
        shadow_dy_px: dy,
        shadow_pad: pad,
        width: text.width + 2 * pad,
        height: text.height + 2 * pad,
        ..text.clone()
    }
}

/// 一张阴影位图的字节数上限。**超过就报错，不静默截断。**
///
/// 这一条不是洁癖：`shadow_blur_ratio` 是用户写的数，写错一位（`0.011` 写成 `1.1`）
/// 就让扩边量涨到几万像素，而这里的分配是 `宽 × 高 × 4` —— 直接 OOM。
/// 本仓对"参数超出实现能做的范围"的既有做法就是**如实报出来**（见 `BLUR_MAX_RADIUS`），
/// 所以这里给一条人话，让整次出片按 `subtitle_raster_failed` 判失败，而不是把机器拖死。
pub const SHADOW_MAX_BITMAP_BYTES: u64 = 64 * 1024 * 1024;

/// 送给 ffmpeg 的参数（**不含程序名**）。
///
/// 抽成纯函数是为了能单测它 —— 尤其是「文本**不进命令行**」这条：
/// 用户内容一旦进了滤镜串，一个冒号或引号就能把整条命令改写。
///
/// `font_file` 与 `key` **分开传**，而不是直接用 `key.font_file`：非 ASCII 那一条路上
/// 喂给 ffmpeg 的是**搬过一份的临时路径**（见 [`ascii_font_path`]），而键上那份仍是
/// 调用方给的。把它显式列出来，等于让"这一串里用的是哪份字体"在类型上就看得见 ——
/// 顺手从 `key` 里取会是一条静默走回老路的岔路。
pub fn drawtext_args(key: &TextRasterKey, font_file: &Path, text_file: &Path) -> Vec<String> {
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
        // `x` 里的 `text_w` 是**这一段自己的**宽（drawtext 的表达式只看当前 filter），
        // 所以"整行居中"这件事**由调用方算好偏移传进来**（`x_offset`）。
        // 0 时不写那一项 —— 既有工程的滤镜串逐字符不变。
        "drawtext=fontfile={}:textfile={}:fontsize={}:fontcolor=white:expansion=none:x=(w-text_w)/2{}:y=(h-text_h)/2",
        filter_value(&font_file.to_string_lossy()),
        filter_value(&text_file.to_string_lossy()),
        key.font_px,
        if key.x_offset == 0 {
            String::new()
        } else if key.x_offset > 0 {
            format!("+{}", key.x_offset)
        } else {
            format!("-{}", -key.x_offset)
        },
    );
    if key.shadow_color.is_some() {
        // **阴影那一张：只有填充，不带描边。**
        //
        // 三条理由，缺一条都会画出不对的东西：
        //
        // 1. **不能有 `borderw`**：描边是黑的，而 [`tint`] 是按"墨色有多白"上色的
        //    —— 黑像素会被染成**黑**（不是阴影色），字的四周就多出一圈脏边。
        // 2. **口径**：契约里写着阴影"不参与描边宽度"（描边与阴影各自独立），
        //    所以阴影的轮廓就是**填充的轮廓**。
        // 3. 模糊放在 `drawtext` 之后、同一个滤镜串里：`gblur` 吃的是 drawtext
        //    吐出来的**覆盖度**（白字透明底 = 预乘覆盖度），模糊完还是覆盖度 ——
        //    于是"白墨 -> 样式色"那一套（[`tint`]）一个字都不用改。
        //
        // σ 与 canvas 的 `shadowBlur` 差一个 2，见 [`shadow_sigma_px`]。
        let sigma = shadow_sigma_px(key.shadow_blur_px);
        if sigma > 0.0 {
            drawtext.push_str(&format!(",gblur=sigma={sigma}"));
        }
    } else if key.outline {
        if key.stroke_px > 0 {
            // **契约给了宽度与颜色。**
            drawtext.push_str(&format!(
                ":borderw={}:bordercolor={}",
                key.stroke_px,
                border_color_value(key.stroke_color)
            ));
        } else {
            // **老路径：宽度从字号推、颜色写死 `black`。**
            //
            // 这一条不是"兼容遗留"，是**契约默认值必须让既有工程逐字节不变**：
            // `stroke_ratio` 的默认值是 0，于是所有老工程都走这里，
            // 而它们升级前渲染出来的就是 `borderw=border_px(font_px):bordercolor=black`。
            // 让颜色也走 `stroke_color` 会在默认值上把黑描边变成别的颜色。
            drawtext.push_str(&format!(":borderw={}:bordercolor=black", border_px(key.font_px)));
        }
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

/// 把字体搬一份到**纯 ASCII 名的临时路径**，返回**接下来要喂给 ffmpeg 的那条路径**。
///
/// # 返回值为什么是「一条路径」而不是「路径 + 清理标记」
///
/// 它返回的就是**接下来该用的那条**：ASCII 路径原样返回，非 ASCII 返回搬过去的那份。
/// 调用方只有一条用法（`ascii_font_path(..)?.path()`），于是「搬了却没换上」
/// 这件事**在类型上就写不出来** —— 没有第二条路径可选。
///
/// 早先的写法是返回 `Option<(路径, 清理路径)>` + 调用方 `match` 两条路各接一次。
/// 那种写法有一个**静默**的错法：`match` 里把 `key.font_file` 接上去 ——
/// 编译过、测试过、`Some` 也拿到了，只有真出片时字又是空的（本机的反向用例量过：
/// 这个错法**不被任何一条断言接住**，所以改成本形态，让它不可写）。
/// 搬出来的那份自己知道该删，见 [`StagedFont`]。
///
/// # 为什么要搬（这不是洁癖，是实测出来的）
///
/// `drawtext` 的 `fontfile=` 对非 ASCII 文件名会**静默失败**：退出码 0、不吐一个字节，
/// stderr 只有一句 fontconfig 的抱怨（模块文档里那张表就是实测数字）。
/// 同一份字体复制成 ASCII 名之后画得好好的 —— 所以问题在**路径的写法**，
/// 不在字体内容，搬一份就能绕过去。
///
/// # 临时名：内容摘要进名字
///
/// 名字里塞**字体内容的 FNV-1a**（与文本临时文件同一个摘要函数），于是：
/// * 同一次出片里几百行共用同一份字体 → 文件名稳定，重复调用撞上同一份；
/// * 换一份字体 → 名字变了 → 不会被上一份的残留顶替；
/// * 名字与内容对不上时能被发现（下面是**先读后算**，算的是读到的那些字节）。
///
/// 用 `copy` 而不是 `hard_link`：Windows 上跨卷硬链会失败，而临时目录与字体目录
/// 常常不在一个卷上。一份中文字体约 2.5 MB，搬一次的代价远小于一次 ffmpeg 进程。
pub fn ascii_font_path(font_file: &Path) -> Result<StagedFont, String> {
    // 路径是字节，不是字符索引 —— Windows 上的非 UTF-8 路径也要能被判成"非 ASCII"，
    // 所以这里看的是 `OsStr` 的编码字节，而不是 `to_string_lossy` 之后的 char。
    if font_file.as_os_str().as_encoded_bytes().is_ascii() {
        // **不用搬**：原样还回去，连 `to_string_lossy` 那一次往返都不做。
        // 这一条是**默认路径字节冻结**的前提 —— 老工程的参数串一个字节都不许变。
        return Ok(StagedFont {
            path: font_file.to_path_buf(),
            staged: None,
        });
    }
    let staged = stage_font_file(font_file)?;
    Ok(StagedFont {
        path: staged.clone(),
        staged: Some(staged),
    })
}

/// 一次栅格化用的字体路径，**顺带管着**为它搬出来的那份临时文件。
///
/// 临时文件交给 [`Drop`] 删，而不是调用方在 `run_ffmpeg` 之后手写一次 `remove_file`：
/// 手写那种要求**每一个出口**都想起来删（现在是成功/失败两条，将来多一条就漏一条），
/// 而 `Drop` 连提前 return 与 panic 都覆盖得到。
///
/// 进程被 Ctrl-C 掉时残留一份几 MB 的副本是无解的（没有 atexit 钩子），
/// 所以临时名带了 `dhampir-font-` 前缀与内容摘要，好让它**可识别、可认领**。
#[derive(Debug)]
pub struct StagedFont {
    /// 喂给 ffmpeg 的那条路径（路径全是 ASCII 时 = 调用方原来那条）。
    path: PathBuf,
    /// 搬出来的那份；`None` = 没搬。
    staged: Option<PathBuf>,
}

impl StagedFont {
    /// 喂给 ffmpeg 用的路径。
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 这一份是不是**搬出来的**（给测试与诊断用：老工程的路径必须给 `false`）。
    pub fn is_staged(&self) -> bool {
        self.staged.is_some()
    }
}

impl Drop for StagedFont {
    fn drop(&mut self) {
        if let Some(staged) = &self.staged {
            let _ = std::fs::remove_file(staged);
        }
    }
}

/// 真的把文件搬过去 —— 与 [`ascii_font_path`] 拆开只为让"要不要搬"这一个判断
/// 能单独看（它是默认路径字节冻结那条判据的全部）。
fn stage_font_file(font_file: &Path) -> Result<PathBuf, String> {
    let bytes = std::fs::read(font_file)
        .map_err(|error| format!("读不了字体文件 {}：{error}", font_file.display()))?;
    let digest = dhampir_core::timeline::selfcheck::fnv1a64(&bytes);
    let path = std::env::temp_dir().join(format!(
        "dhampir-font-{}-{digest:016x}{}",
        std::process::id(),
        font_file
            .extension()
            .and_then(|extension| extension.to_str())
            .map(|extension| extension.to_ascii_lowercase())
            .map(|extension| format!(".{extension}"))
            .unwrap_or_default()
    ));
    std::fs::write(&path, &bytes)
        .map_err(|error| format!("写不了字体临时文件 {}：{error}", path.display()))?;
    Ok(path)
}

/// 把文本落成一个临时文件，给 `textfile=` 用。///
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
    // 阴影那一张的尺寸是**扩过边**的：`shadow_blur_ratio` 写错一位就能要几十 GB。
    // 在起进程之前拦下（与下面那些入参检查同一处），并说清该改哪个字段。
    let bytes = key.width as u64 * key.height as u64 * 4;
    if key.shadow_color.is_some() && bytes > SHADOW_MAX_BITMAP_BYTES {
        return Err(format!(
            "阴影位图太大了：{}x{} = {bytes} 字节（上限 {SHADOW_MAX_BITMAP_BYTES}）—— \
             模糊半径与偏移决定四周要扩多少边。把样式里的 shadow_blur_ratio 调小",
            key.width, key.height
        ));
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
    // 字体路径含非 ASCII 时先搬一份到 ASCII 名的临时路径（实测：不搬的话 ffmpeg
    // 退出码 0 但一个字节都不吐）。`StagedFont` 自己管清理，**也自己给出该用哪条路径**
    // —— 调用方没有第二条路径可选，所以"搬了却没换上"这件事写不出来。
    let font = ascii_font_path(&key.font_file)?;
    let result = run_ffmpeg(key, font.path(), &text_file);
    // 文本临时文件用完就删，成功失败都删：一次出片几百行，留一地文件是在给下次查问题挖坑。
    // （字体那份由 `font` 的 Drop 删，连提前 return 都覆盖得到。）
    let _ = std::fs::remove_file(&text_file);
    result
}

fn run_ffmpeg(
    key: &TextRasterKey,
    font_file: &Path,
    text_file: &Path,
) -> Result<TextBitmap, String> {
    let args = drawtext_args(key, font_file, text_file);
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
    TextBitmap::new(key.width, key.height, tint(&output.stdout, key.tint_color()))
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
    ///
    /// **阴影那几项全是"不画"**：这个助手就是"老工程那把键"，
    /// 于是任何一条拿它跑出来的参数串都必须与改动前逐字符相同。
    fn key(text: &str, height: u32) -> TextRasterKey {
        TextRasterKey {
            x_offset: 0,
            text: text.to_string(),
            font_px: 32,
            color: [255, 240, 200, 255],
            outline: false,
            stroke_px: 0,
            stroke_color: [0, 0, 0, 255],
            shadow_color: None,
            shadow_blur_px: 0,
            shadow_dx_px: 0,
            shadow_dy_px: 0,
            shadow_pad: 0,
            font_file: PathBuf::from("C:/fake/font.ttf"),
            width: 100,
            height,
        }
    }

    /// 老工程那把参数串的**冻结**副本（改动前逐字符抄下来）。
    ///
    /// 这不是"顺手存一份"：`drawtext_args` 是"既有工程逐字节不变"里最容易被
    /// 顺手改坏的一处（加阴影时最自然的写法就是往这条串里塞东西），
    /// 而它一旦多一个字符，**所有老工程的每一帧字幕**都变了样。
    fn frozen_argv(key: &TextRasterKey) -> Vec<String> {
        vec![
            "-v".to_string(),
            "error".to_string(),
            "-nostdin".to_string(),
            "-f".to_string(),
            "lavfi".to_string(),
            "-i".to_string(),
            format!("color=c=black@0.0:s={}x{},format=rgba", key.width, key.height),
            "-vf".to_string(),
            format!(
                "drawtext=fontfile='C\\:/fake/font.ttf':textfile='C\\:/tmp/dhampir-text-1.txt':\
                 fontsize=32:fontcolor=white:expansion=none:x=(w-text_w)/2:y=(h-text_h)/2"
            ),
            "-frames:v".to_string(),
            "1".to_string(),
            "-f".to_string(),
            "rawvideo".to_string(),
            "-pix_fmt".to_string(),
            "rgba".to_string(),
            "-".to_string(),
        ]
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
        let args = drawtext_args(&k, &k.font_file, Path::new("C:/tmp/dhampir-text-1.txt"));
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
        let plain_key = key("字", 20);
        let plain = drawtext_args(&plain_key, &plain_key.font_file, Path::new("t.txt")).join(" ");
        assert!(!plain.contains("borderw"));

        let mut outlined = key("字", 20);
        outlined.outline = true;
        outlined.font_px = 48;
        let joined = drawtext_args(&outlined, &outlined.font_file, Path::new("t.txt")).join(" ");
        assert!(
            joined.contains("borderw=3"),
            "字号 48 的描边是 3 像素：{joined}"
        );
        // **默认走老路径**：`stroke_px == 0` → 宽度从字号推、颜色写死 black。
        // 这是"既有工程逐字节不变"的那条分支（`stroke_ratio` 的默认值是 0）。
        assert!(joined.contains("bordercolor=black"), "实得：{joined}");
    }

    #[test]
    fn 描边宽度与颜色都来自契约() {
        // **以前两者都是推出来的**：宽度从字号推（`border_px`）、颜色写死 black。
        // 而 参照实现 的字幕是 `12px #403c3b`、弹幕是 `2px #000` —— 两组数
        // 在"从字号推"的规则下**对不上**，颜色更是只能有一个。
        let mut styled = key("字", 48);
        styled.outline = true;
        styled.stroke_px = 12;
        styled.stroke_color = [0x40, 0x3c, 0x3b, 255];
        let joined = drawtext_args(&styled, &styled.font_file, Path::new("t.txt")).join(" ");
        assert!(joined.contains("borderw=12"), "宽度要用契约给的 12，实得：{joined}");
        assert!(
            joined.contains("bordercolor=0x403C3B"),
            "颜色要用契约给的 #403c3b，实得：{joined}"
        );

        // `stroke_px == 0` 时退回"从字号推 + 颜色 black"—— 老工程没写这个字段，
        // 行为必须逐字节不变（升级后的默认值就是 0）。
        let mut legacy = key("字", 48);
        legacy.outline = true;
        legacy.font_px = 48;
        let legacy_args = drawtext_args(&legacy, &legacy.font_file, Path::new("t.txt")).join(" ");
        assert!(legacy_args.contains("borderw=3"), "老行为：字号 48 -> 3 像素，实得：{legacy_args}");
        assert!(
            legacy_args.contains("bordercolor=black"),
            "老行为：默认颜色是 black（不是 0x000000），实得：{legacy_args}"
        );
    }

    // ---- 文字阴影（B5）----

    /// **反向用例（最重要的一条）**：不画阴影时，参数串逐字符与改动前相同。
    ///
    /// 判据不是"看起来差不多"，而是**逐项相等**：加阴影时最自然的写法就是往
    /// `drawtext` 那条串里塞东西，而多一个字符就意味着**所有老工程**的每一帧字幕
    /// 都变了样（那条串决定画出来的像素）。
    #[test]
    fn 不画阴影时参数串逐字符与改动前相同() {
        // 无描边（老工程的默认：`stroke_ratio` 默认 0 且 `outline` 默认 true 时走另一支，
        // 所以这里两种都验）。
        let plain = key("字", 20);
        assert_eq!(drawtext_args(&plain, &plain.font_file, Path::new("C:/tmp/dhampir-text-1.txt")), frozen_argv(&plain));

        // 有描边：老路径（宽度从字号推、颜色写死 black）。
        let mut outlined = key("字", 20);
        outlined.outline = true;
        let mut expected = frozen_argv(&outlined);
        let at = expected.iter().position(|arg| arg.starts_with("drawtext=")).expect("有 -vf");
        expected[at] = format!("{}:borderw={}:bordercolor=black", expected[at], border_px(32));
        assert_eq!(
            drawtext_args(&outlined, &outlined.font_file, Path::new("C:/tmp/dhampir-text-1.txt")),
            expected,
            "有描边的老路径也一个字符都不能变"
        );
        // 而且它里面**没有** gblur：模糊只属于阴影那条新路。
        assert!(!expected[at].contains("gblur"));
    }

    /// 画阴影时：**白字 + gblur**，并且**不带描边**。
    ///
    /// 三条判据各挡一类错：
    ///   * 有 `gblur` —— 否则"模糊阴影"是假声明（字段读了、命令里没体现）；
    ///   * 没有 `borderw` —— 有的话描边会被 `tint` 染成**黑**（墨色不白），
    ///     字边上多出一圈脏边；口径上也错（阴影不参与描边宽度）；
    ///   * 画布尺寸 = 文本位图 + 四周扩 `pad` —— 否则模糊与偏移会被自己的画布切掉。
    #[test]
    fn 画阴影时用白字加模糊而不是描边() {
        let text = key("字", 20);
        let shadow = shadow_key(&text, [0, 0, 0, 102], 4, 3, 2);
        let args = drawtext_args(&shadow, &shadow.font_file, Path::new("C:/tmp/dhampir-text-1.txt"));
        let joined = args.join(" ");
        assert!(joined.contains("gblur=sigma=2"), "σ 应当是 blur/2 = 2：{joined}");
        assert!(!joined.contains("borderw"), "阴影不许带描边：{joined}");
        assert!(
            joined.contains("fontcolor=white"),
            "覆盖度仍由白字给（颜色归 tint）：{joined}"
        );
        // 扩边量：blur 4 -> 8，偏移 max(3,2) = 3 -> pad = 11。
        let pad = shadow_pad_px(4, 3, 2);
        assert_eq!(pad, 11);
        assert_eq!(
            (shadow.width, shadow.height),
            (text.width + 22, text.height + 22),
            "画布要四周各扩一份 pad"
        );
        assert!(
            joined.contains(&format!("{}x{}", shadow.width, shadow.height)),
            "画布尺寸必须就是扩过边的那个：{joined}"
        );
    }

    /// 硬阴影（blur = 0）时**不许出现 gblur**：`sigma=0` 在 ffmpeg 那边没有意义。
    #[test]
    fn 硬阴影不带_gblur() {
        let shadow = shadow_key(&key("字", 20), [0, 0, 0, 255], 0, 0, 4);
        let joined = drawtext_args(&shadow, &shadow.font_file, Path::new("t.txt")).join(" ");
        assert!(!joined.contains("gblur"), "硬阴影不该有模糊：{joined}");
        assert!(!joined.contains("borderw"), "阴影不许带描边：{joined}");
        assert_eq!(shadow_pad_px(0, 0, 4), 4, "偏移仍要余量");
    }

    /// 扩边量与 σ 是**纯函数**：两端对齐就靠这两个数（canvas 的 `shadowBlur` 吃 σ×2）。
    #[test]
    fn 阴影的扩边量与_sigma() {
        // σ = blur / 2 —— canvas 的 shadowBlur 是 σ 的两倍，这里就是那个 2。
        assert_eq!(shadow_sigma_px(0), 0.0);
        assert_eq!(shadow_sigma_px(4), 2.0);
        assert_eq!(shadow_sigma_px(25), 12.5);
        // 扩边 = 2 × blur + max(|dx|, |dy|)：模糊的扩散与偏移各要一份余量。
        assert_eq!(shadow_pad_px(0, 0, 0), 0);
        assert_eq!(shadow_pad_px(4, 0, 0), 8);
        assert_eq!(shadow_pad_px(4, 3, 2), 11);
        assert_eq!(shadow_pad_px(4, -9, 2), 17, "负偏移按绝对值算");
        // **反向**：余量必须真的覆盖模糊的扩散（3σ = 1.5 × blur）。
        for blur in [1_u32, 4, 12, 40] {
            assert!(
                shadow_pad_px(blur, 0, 0) as f32 >= blur as f32 * 1.5,
                "blur={blur} 的余量盖不住 3σ"
            );
        }
    }

    /// 阴影键与文本键**必须不同**：它们是两张位图，撞在一起就会把没模糊的递给有模糊的。
    #[test]
    fn 阴影与文本是缓存里的两张位图() {
        let text = key("同一行字", 20);
        let shadow = shadow_key(&text, [0, 0, 0, 102], 4, 3, 2);
        assert_ne!(shadow, text, "阴影键不许与文本键相等");
        // 文本键本身仍然是"不画阴影"。
        assert_eq!(text.shadow_color, None);
        assert_eq!(shadow.shadow_color, Some([0, 0, 0, 102]));
        // **反向**：只差模糊半径的两把阴影键也要各画一次
        // （模糊半径进键 = "位图尺寸进键"那条纪律的另一个实例）。
        let mut blurry = shadow.clone();
        blurry.shadow_blur_px = 8;
        blurry.shadow_pad = shadow_pad_px(8, 3, 2);

        let mut cache = TextRasterCache::default();
        let mut calls = 0usize;
        for candidate in [&text, &shadow, &text, &blurry, &shadow] {
            cache
                .get_or_insert_with(candidate, |k| {
                    calls += 1;
                    Ok(fake_bitmap(k))
                })
                .unwrap();
        }
        assert_eq!(calls, 3, "文本、阴影、另一档模糊各要一次；同键不许重复栅格化");
        assert_eq!(cache.hits, 2);
    }

    /// 模糊半径写错一位就能要几十 GB：**必须在起进程之前报出来**，不许静默分配。
    #[test]
    fn 阴影位图太大要报出来而不是硬分配() {
        let huge = shadow_key(&key("字", 20), [0, 0, 0, 102], 100_000, 0, 0);
        let err = rasterize_line(&huge).unwrap_err();
        assert!(err.contains("阴影位图太大"), "{err}");
        // 报错要说清该改哪个字段 —— 否则拿到这句话的人不知道该动什么。
        assert!(err.contains("shadow_blur_ratio"), "{err}");
        // 反向：正常的模糊半径要过这一关（走到字体检查就说明尺寸那关过了）。
        let normal = shadow_key(&key("字", 20), [0, 0, 0, 102], 12, 0, 2);
        let err = rasterize_line(&normal).unwrap_err();
        assert!(err.contains("字体文件不在"), "正常尺寸不该被拦：{err}");
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

    // ---- 非 ASCII 字体路径（T1）----

    /// 一个**能写**的草稿目录，给"要造一份源文件/看临时目录"的用例用。
    ///
    /// 正常情况下就是 `std::env::temp_dir()` 底下的一层。但**沙箱化的开发环境**
    /// （例如 DSH 的文件沙箱）会让某个路径下的可执行文件**写不了 `%TEMP%`**
    /// —— 那种环境下 `temp_dir()` 建目录就会以 `拒绝访问 (os error 5)` 失败。
    /// 那不是产品缺陷，所以这里**返回 `None` 让用例自己如实跳过**，
    /// 而不是让一条环境差异伪装成"实现坏了"。
    ///
    /// 判据是**真去试着建一次**，不是去嗅探环境变量：能建就返回，建不了就 None。
    fn writable_scratch_dir() -> Option<PathBuf> {
        let dir = std::env::temp_dir().join(format!("dhampir-t1-{}", std::process::id()));
        match std::fs::create_dir_all(&dir) {
            Ok(()) => Some(dir),
            Err(error) => {
                eprintln!(
                    "跳过：这个环境写不了临时目录 {}（{error}）—— \
                     环境限制，不是实现坏了",
                    dir.display()
                );
                None
            }
        }
    }

    /// **判据（正向）**：路径里有非 ASCII 字节 → 搬到 ASCII 的临时路径，且那一份**真的存在**。
    ///
    /// 只说"给了 is_staged() == true"是不够的：命令里写着一个不存在的路径同样是"搬过了"，
    /// 而 ffmpeg 对不存在的字体报的错与这一条要修的那个缺陷**不是一回事**。
    /// 所以这里必须真去 `is_file()` —— 那是"搬"这个动作唯一算数的证据。
    #[test]
    fn 非_ascii_字体路径会被搬到临时_ascii_路径() {
        let Some(scratch) = writable_scratch_dir() else {
            return;
        };
        // 造一份**真的存在**的源文件：这条要证的是"搬"，不是"报错"。
        // 名字里带全角括号与中文 —— 正是本机那份字体文件名的形态。
        let source = scratch.join("乐米波波体（免费商用）.ttf");
        // 内容不必是真字体：这条只看"搬"这个动作（内容比对是逐字节的）。
        let bytes: Vec<u8> = (0..=255u8).cycle().take(4096).collect();
        std::fs::write(&source, &bytes).expect("造一份源文件");

        let staged_font = ascii_font_path(&source).expect("读不到源文件");
        assert!(
            staged_font.is_staged(),
            "非 ASCII 路径**必须**搬 —— 不搬的话 ffmpeg 退出码 0 却一个字节都不吐"
        );
        let staged = staged_font.path().to_path_buf();

        assert!(staged.is_file(), "搬完之后那一份必须真的在：{}", staged.display());
        assert!(
            staged
                .to_string_lossy()
                .bytes()
                .all(|byte| byte.is_ascii()),
            "临时路径自己必须全是 ASCII，否则搬了等于没搬：{}",
            staged.display()
        );
        assert!(staged.starts_with(std::env::temp_dir()), "临时文件要落在临时目录里");
        assert!(
            staged
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("dhampir-font-"),
            "临时名要可识别（进程被杀时靠它认领残留）：{}",
            staged.display()
        );
        // 内容必须与源**逐字节相同**：搬的是路径不是字体。
        assert_eq!(
            std::fs::read(&staged).unwrap(),
            bytes,
            "搬过去的那一份内容必须与源逐字节相同"
        );
        // 搬出来的那份由 Drop 清理。
        let staged_copy = staged.clone();
        drop(staged_font);
        assert!(
            !staged_copy.exists(),
            "StagedFont 掉了之后临时字体还在：{}",
            staged_copy.display()
        );
        let _ = std::fs::remove_dir_all(&scratch);
    }

    /// **反向用例（内容这一半）**：换一份字体（内容变了）→ 临时名必须跟着变。
    ///
    /// 摘要不进名字的话，同一次出片里换一份字体（或上一次进程留下的残留）
    /// 会让 ffmpeg 拿到**上一份字体** —— 画出来的字形是错的，而没有任何报错。
    #[test]
    fn 换成另一份字体时临时名会变() {
        let Some(dir) = writable_scratch_dir() else {
            return;
        };
        let first = dir.join("字体甲.ttf");
        let second = dir.join("字体乙.ttf");
        std::fs::write(&first, b"AAAA").unwrap();
        std::fs::write(&second, b"BBBB").unwrap();

        let a = ascii_font_path(&first).unwrap();
        let b = ascii_font_path(&second).unwrap();
        assert_ne!(
            a.path(),
            b.path(),
            "内容不同的两份字体不许共用同一个临时名"
        );
        // 内容相同（路径不同）时**应当**共用：同一次出片里同一份字体只搬一次。
        let twin = dir.join("字体丙.ttf");
        std::fs::write(&twin, b"AAAA").unwrap();
        let c = ascii_font_path(&twin).unwrap();
        assert_eq!(
            a.path(),
            c.path(),
            "内容相同的字体应当落到同一个临时名（一次出片里几百行共用一份）"
        );

        drop((a, b, c));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **判据（反向）**：路径全是 ASCII 时**不许**搬 —— 这是"老工程逐字节不变"的前提。
    ///
    /// 返回 `None` 在这里的含义是"参数串里用的就是你给我的那条路径"。
    /// 若这条红成 `Some`，那 `drawtext_args` 里就会换成一条临时路径，
    /// 而那条串决定画出来的像素 —— **所有老工程**的每一帧字幕都会变。
    #[test]
    fn ascii_字体路径不搬_这一条是默认路径字节冻结() {
        let plain = PathBuf::from("C:/Windows/Fonts/msyh.ttc");
        let staged = ascii_font_path(&plain).expect("ASCII 路径不该读盘");
        assert!(
            !staged.is_staged(),
            "全 ASCII 的路径不许触发复制 —— 老工程的参数串会因此改变"
        );
        assert_eq!(
            staged.path(),
            plain.as_path(),
            "不搬时必须**原样**给回调用方那条路径"
        );
        // 连反斜杠这种"看着像转义"的 ASCII 字符也不许触发。
        assert!(!ascii_font_path(Path::new("C:\\Windows\\Fonts\\simhei.ttf"))
            .unwrap()
            .is_staged());
    }

    /// **反向用例（会红的那一半）**：源文件不在时，搬这一步必须**响亮报错**，
    /// 而不是返回一个指向不存在文件的路径让 ffmpeg 去撞。
    ///
    /// 这条挡的是"写错分支"：把 `stage_font_file` 的错误吞掉（`.ok()`、`unwrap_or_default()`
    /// 那一类），这条立刻红 —— 因为那样会返回 `Some` 或 panic，而不是 Err。
    #[test]
    fn 搬字体失败要响亮报错而不是递一个空路径() {
        let missing = PathBuf::from("C:/tmp/不存在的中文字体（T1）.ttf");
        let err = ascii_font_path(&missing).unwrap_err();
        assert!(
            err.contains("读不了字体文件"),
            "读不到源文件要给一条人话：{err}"
        );
        assert!(
            err.contains("不存在的中文字体"),
            "错误里要点出是哪份文件：{err}"
        );
    }

    /// 搬到 ASCII 路径之后，**画出来的那一串里用的必须是临时路径**，而不是键上那条。
    ///
    /// 这条是"搬了却没换上"的守卫：`ascii_font_path` 算得很对、`drawtext_args` 里
    /// 却仍旧读 `key.font_file` —— 那是最自然的一处写错，而它**静默**：
    /// 编译过、测试过、`Some` 也拿到了，只有真出片时字又是空的。
    #[test]
    fn 参数串里用的是搬过去的路径而不是键上那条() {
        let mut k = key("字", 20);
        k.font_file = PathBuf::from("C:/tmp/乐米.ttf");
        let staged = PathBuf::from("C:/tmp/dhampir-font-1-abc.ttf");
        let joined = drawtext_args(&k, &staged, Path::new("t.txt")).join(" ");
        assert!(
            joined.contains("dhampir-font-1-abc.ttf"),
            "参数串里没有搬过去的那条路径：{joined}"
        );
        assert!(
            !joined.contains("乐米"),
            "参数串里还留着非 ASCII 的那条路径 —— 搬了却没换上：{joined}"
        );
    }

    /// **判据（反向用例的关键一半）**：路径是纯 ASCII 时，搬到不搬**参数串一模一样**。
    ///
    /// 这一条与上面那条合起来才是完整的：上面证明"搬了就换"，这条证明"不搬就不换"。
    /// 两条都在，`ascii_font_path` 的返回分支才没有第三种走法。
    ///
    /// 用的字体是 [`test_font_ascii`]（**保证全 ASCII 路径**）而不是 [`test_font`]：
    /// 这条要盯的正是"路径里有没有非 ASCII 字节"这一个判断，字体随机换会把这半条证稀释掉。
    #[test]
    #[ignore = "读一个真字体文件的字节：本机要有候选字体"]
    fn 真起_ffmpeg_ascii_字体路径不搬_参数串逐字符不变() {
        let font = test_font_ascii();
        assert!(
            font.to_string_lossy().bytes().all(|byte| byte.is_ascii()),
            "这一条的前提是 ASCII 路径，拿到的是：{}",
            font.display()
        );
        let mut k = key("字", 20);
        k.font_file = font.clone();

        // 搬这一步必须说"不用搬"，并且给回的就是原来那条路径。
        let staged = ascii_font_path(&font).expect("ASCII 路径连读都不该读");
        assert!(
            !staged.is_staged(),
            "全 ASCII 的路径不许触发复制 —— 老工程的参数串会因此改变"
        );
        assert_eq!(staged.path(), font.as_path(), "不搬时路径必须原样返回");
        // 于是参数串里就是**键上那一条路径**，与不搬时逐字符相同。
        let joined = drawtext_args(&k, &k.font_file, Path::new("t.txt")).join(" ");
        assert!(
            joined.contains(&font_file_value(&font)),
            "参数串里该是键上那条路径：{joined}"
        );
        assert!(
            !joined.contains("dhampir-font-"),
            "ASCII 路径不许被换成临时路径：{joined}"
        );
    }

    /// 参数串里那条 `fontfile='…'` 的**转义形态** —— 拿它去 `contains` 才算真比对过。
    fn font_file_value(font: &Path) -> String {
        filter_value(&font.to_string_lossy())
    }

    /// **反向用例（真起 ffmpeg）**：非 ASCII 字体路径现在能画出非空位图了。
    ///
    /// 修之前这条会**红得很难看**：`rasterize_line` 会以「ffmpeg 本该吐 N 字节，实际 0」
    /// 失败 —— 也就是说"安静地画不出来"这件事被**字节数**那条检查抓住了。
    /// 修之后必须绿，而且墨迹数是可复算的（见断言里的下限）。
    #[test]
    #[ignore = "真起 ffmpeg：需要一个非 ASCII 文件名的中文字体"]
    fn 真起_ffmpeg_非_ascii_字体路径也画得出字() {
        let font = test_font();
        if font.to_string_lossy().bytes().all(|byte| byte.is_ascii()) {
            // 本机没有非 ASCII 名的字体：这条**跳过**而不是假装通过 ——
            // 拿一个 ASCII 路径跑出来的绿，证明不了这一条要证的事。
            eprintln!("本机 test_font() 是 ASCII 路径（{}），这条测试跳过", font.display());
            return;
        }
        let font_px = 40u32;
        let (width, height) = bitmap_size(640, font_px as f32 * 1.2, font_px);
        let mut k = key("笑靥如花", 20);
        k.text = "笑靥如花".to_string();
        k.font_px = font_px;
        k.font_file = font.clone();
        k.width = width;
        k.height = height;

        let bitmap = rasterize_line(&k).expect("非 ASCII 字体路径必须画得出来（T1）");
        let inked = ink_count(&bitmap);
        assert!(
            inked > 0,
            "非 ASCII 字体路径画出来是空的 —— 这正是 T1 要修的那个静默失败"
        );
        // 四个字、字号 40：墨迹至少上千像素。给一个宽松但能证伪的下限。
        assert!(inked > 1000, "墨迹只有 {inked} 像素，像是没画全");
        assert!(!bitmap.ink_touches_edge(), "字被切了");
    }

    /// 搬字体的临时文件用完必须删掉（成功那条路）。
    ///
    /// 清理写在 `rasterize_line` 里，所以这里只能真跑一次再看临时目录 ——
    /// **不跑就都是在猜**：一次出片几百行，留一地几 MB 的字体副本是实打实的浪费。
    #[test]
    #[ignore = "真起 ffmpeg：需要一个非 ASCII 文件名的中文字体"]
    fn 真起_ffmpeg_搬完的字体临时文件会被清掉() {
        let font = test_font();
        if font.to_string_lossy().bytes().all(|byte| byte.is_ascii()) {
            eprintln!("本机 test_font() 是 ASCII 路径，这条测试跳过");
            return;
        }
        let staged_name_after = || -> usize {
            std::fs::read_dir(std::env::temp_dir())
                .map(|entries| {
                    entries
                        .filter_map(|entry| entry.ok())
                        .filter(|entry| {
                            entry
                                .file_name()
                                .to_string_lossy()
                                .starts_with("dhampir-font-")
                        })
                        .count()
                })
                .unwrap_or(0)
        };
        let before = staged_name_after();

        let font_px = 40u32;
        let (width, height) = bitmap_size(640, font_px as f32 * 1.2, font_px);
        let mut k = key("清理", 20);
        k.text = "清理".to_string();
        k.font_px = font_px;
        k.font_file = font;
        k.width = width;
        k.height = height;
        rasterize_line(&k).expect("这一行应当画得出来");

        assert_eq!(
            staged_name_after(),
            before,
            "搬过去的字体副本没被删掉 —— 临时目录里每画一行就多留几 MB"
        );
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
            x_offset: 0,
            text: "第一行中文字幕".to_string(),
            font_px,
            color: [255, 240, 200, 255],
            outline: true,
            stroke_px: 0,
            stroke_color: [0, 0, 0, 255],
            shadow_color: None,
            shadow_blur_px: 0,
            shadow_dx_px: 0,
            shadow_dy_px: 0,
            shadow_pad: 0,
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
            x_offset: 0,
            text: "The quick brown fox jumps over the lazy dog".to_string(),
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
            x_offset: 0,
            text: text.to_string(),
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
    ///
    /// **非 ASCII 名的那一份排在前面**：本机（Windows）用户字体目录里就有一份，
    /// 而「非 ASCII 路径」正是 T1 要盯的那条路 —— 让既有的真机测试就踩到它，
    /// 比专门写一条"只在有人记得时才跑"的路结实。找不到就退回下面那张常青候选表。
    fn test_font() -> PathBuf {
        let user_fonts = PathBuf::from(std::env::var("LOCALAPPDATA").unwrap_or_default())
            .join("Microsoft")
            .join("Windows")
            .join("Fonts");
        if let Ok(entries) = std::fs::read_dir(&user_fonts) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                let lower = name.to_ascii_lowercase();
                let is_font = [".ttf", ".ttc", ".otf"]
                    .iter()
                    .any(|extension| lower.ends_with(extension));
                if is_font && !name.bytes().all(|byte| byte.is_ascii()) {
                    return entry.path();
                }
            }
        }
        test_font_ascii()
    }

    /// 一份**保证全 ASCII 路径**的字体。
    ///
    /// T1 的反向用例（"ASCII 路径不许触发复制"）拿它当对照 —— 用 [`test_font`] 的话
    /// 那一条会随机器变，反向那一半就证不实了。
    fn test_font_ascii() -> PathBuf {
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
}
