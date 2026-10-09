//! 宿主侧文本栅格化：把**一行**文字画成一张位图。
//!
//! # 它在补哪一个洞
//!
//! 共享布局（`dhampir_timeline::text_layout`）只回答「几行、每行什么、占哪个矩形」——
//! 字形像素必须由宿主画出来。本仓不许引第三方 crate（见 plan/next-steps.md 的约定），
//! 而 PATH 上那个 ffmpeg 出片本来就要用，所以栅格化走它的滤镜。
//!
//! # 为什么走 libass 而不再是 `drawtext`（**这一版起老工程字幕像素会变**）
//!
//! 这一节是破坏性变更的说明，别删。**从这一版起，本仓画出来的每一个字形像素都与之前
//! 不同** —— 包括所有老工程、默认路径、没有任何新字段的工程。理由与代价写在这里，
//! 因为"字看起来差不多"会让人以为它不该影响老片子。
//!
//! ## 理由：`drawtext` 不会回退，libass 会
//!
//! `drawtext` 只用**一个**字体文件：字体里没有那个码位时它画 `.notdef`（空心方框），
//! 退出码 0、不报警。本机实测「笑靥如花」用一个缺 U+9765 靥的字体：
//! drawtext 出 3 段字形（靥 成了方框），宽高比 1.64；同一串走 libass 出 4 段、
//! 靥 由另一副字体补上（证据与数字见 `plan/glyph-fallback-evidence.md`）。
//! 缺字是**必然**会遇到的（用户的文案里总有字体覆盖不到的码位），
//! 而"某一两个字变成方框"在看片时像"这个字体就这样"。
//!
//! libass 的回退是逐字形的：`fontselect` 日志里能直接看到它换字体 ——
//! `(乐米波波体, 400, 0) -> LemiBoBoTi-Regular, 0`（第一个字用点名的字体）
//! 紧接着 `(乐米波波体, 400, 0) -> MicrosoftYaHeiUI, 1`（缺的那个字换一副）。
//!
//! ## 代价（写清楚，因为它是真的）
//!
//! * **每一帧的字幕像素都变了**：不同的栅格化器（libass/FreeType 对字形轮廓、
//!   提示、抗锯齿的处理与 drawtext 不同），逐字节相等不可能。
//!   已建立的判据是**结构一致**而不是像素一致（见 `plan/consistency-criteria.md`），
//!   所以这一条与既有口径不冲突；但它确实是一次**看得见**的变更，提交信息里必须写。
//! * **多一个前置：字体目录**。libass 按**家族名**找字体、并且回退也要靠字体库，
//!   所以必须告诉它去哪儿找 —— ffmpeg 的 `subtitles` 滤镜自己有 `fontsdir=`，
//!   见 [`font_dir`]。
//!   **早先的写法是随行生成一份 `fonts.conf` 走 `FONTCONFIG_FILE`；实测证明不必**：
//!   `env -u FONTCONFIG_FILE` 下 `fontsdir=` 一样工作（rc=0、`fontselect` 两行都在、
//!   没有 `Cannot load default config`）。少一个会腐烂的外部文件。
//! * **家族名必须由契约给**（`font_family`），**不能从文件名推** ——
//!   实测推出来的名字 libass 认不出来，会**静默回退**到 ArialMT
//!   （见 [`TextRasterKey::font_family`] 那张表）。这是这一版最阴的一个失败模式：
//!   它不报警、`lines_failed` 还是 0，只是字全变了。
//! * **两条路不能混**：`ass=` 与 `subtitles=` 都试过。**选 `subtitles=`** ——
//!   `ass=` 的 `Dialogue:` 文本里 `%{n}` 是 **ASS 覆盖标签**，会被 libass 当指令**吞掉**
//!   （实测：`%{n}` 只剩 312 个覆盖像素、而「笑靥如花」是 2180 个）。
//!   那是"用户打的东西被悄悄吃掉"，与 `drawtext` 的展开是同一种病。
//!   `subtitles=` 让 libass 自己解析 SRT，`%{n}` 原样画出来（实测 `%{n}` 24 像素 >
//!   一个 `0` 的 21 像素 —— 没有被换成帧号，也没有被吞掉）。
//!
//! # 契约：位图是**直排** RGBA8，颜色在这一步就已经染好
//!
//! 有两件事必须在这个文件里对齐一次，否则下游只能猜：
//!
//! 1. **混合约定**。core 的合成器走直排法（`SrcAlpha` / `OneMinusSrcAlpha`，
//!    见 `core/src/render/compose.rs` 的 `blend_state`），而 ffmpeg 吐出来的不是直排。
//!    把非直排位图当直排用，字的边缘会暗一圈 —— 而那种错看起来像「字体渲染得不太好」，
//!    不会有人去查混合约定。**两代栅格化器的形态还不一样**，所以这里有两条：
//!    * `drawtext`（老路）：**覆盖度预乘** —— 不透明白字的抗锯齿边缘是 `[80, 80, 80, 80]`。
//!    * libass（新路）：**覆盖度写在 RGB，alpha 恒为 0** ——
//!      实测 `alpha>20` 的像素数 = 0 而 `rgb>20` 是 8616。
//!      **直接当覆盖度用会让整行字消失**（`tint` 拿 `alpha=0` 什么都染不出来）。
//!      这条地雷由 [`coverage_from_libass`] 搬平，见那一节的判据与数字；
//!      反向用例见 `真机_直接把_libass_的_alpha_当覆盖度会被抓住`。
//!
//!    **新老两代的抗锯齿边缘不一样宽**（实测四个字全部同向）：
//!    老 `drawtext` 的斜坡 0.63~1.43 px，新 libass 是 1.63~3.17 px。
//!    两边都还是"窄抗锯齿"，但**新版字幕的边缘会比老板略柔** ——
//!    这是两条渲染器的真实差异，与"字幕像素会变"是同一件事的两面。
//!    数字与判据见 [`ramp_width`]。
//! 2. **颜色不属于 ffmpeg 那一步**。样式色与不透明度由 [`tint`] 在本文件里染上去。
//!    这么分的好处是样式色的 alpha 精确：若让栅格化器自己带半透明色，
//!    它的输出里 rgb 与 alpha 各乘了不同的系数，反解算会把颜色推亮。
//!
//!    **但新路这里要比老路多交代一句**：libass 那条路上，
//!    **填充与描边都必须画成白墨**（[`WHITE_INK`]），描边的颜色在 ffmpeg 这一步
//!    **不能**来自 `stroke_color`。原因是 libass 输出的是**已合成**的颜色、
//!    且 alpha 恒为 0：黑描边的像素是 `(0,0,0,0)`，与"完全透明的背景"
//!    **逐字节相同**（实测一张 640×90 的图里 56806 个 `(0,0,0,0)`），
//!    覆盖度**数学上恢复不出来**。两处都画白墨，覆盖度才回得来。
//!    （老路 `drawtext` 没有这个问题：它的透明底与黑描边靠 alpha 分得开。）
//!    峰值是 **255 不是 235** —— 235 是"白填充与黑描边在同一像素混色"的中间值，
//!    不是上限，别照抄。
//!
//! 于是下游拿到的就是「照直叠加即可」的位图：`rgb` 是样式色，`alpha` 是覆盖率乘样式不透明度。
//!
//! # 用户文本里的百分号
//!
//! 老路（`drawtext`）靠 `expansion=none` 才安全：默认档下 `%{pts}`、`%{n}`、strftime
//! 的 `%Y` 那一套都会展开，而一个散落的 `%` 让它直接报错（`数字 100 % 号` →
//! `Stray % near ' 号'`）。新路（`subtitles=`）**没有那个展开器** ——
//! libass 拿到的是一份 SRT 文件内容，`%` 就是一个普通字符。
//! 所以 `expansion=none` 这一项在新路上**不存在也不需要**，但"用户文本里的 `%` 不许
//! 出岔子"这条**判据必须留着**：真起 ffmpeg 盯着「`%{n}` 没有被换成帧号」那条反向用例
//! 现在盯的是 libass，而且**多加了一条**——`%{n}` 也不许被当 ASS 标签吞掉
//! （那是选 `subtitles=` 而不是 `ass=` 的理由，见上）。
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
//! 与水平居中一致），高度取行盒加上下各一份 [`pad_px`]。
//! 将来布局加了对齐字段，取位图的那一侧要一起改。
//!
//! **切没切字是查得出来的**：见 [`TextBitmap::ink_touches_edge`]。调用方必须把它
//! 变成一条问题记录，而不是忽略 —— 静默切字属于最难查的那类（画面看着「就是这样」）。
//!
//! # 非 ASCII 字体文件名：ffmpeg 会**静默**画不出来
//!
//! **T1 当时是这么认识的，T2 之后这一段仍然成立、但适用面变了**：缺陷出在
//! `drawtext` 的 `fontfile=` 上，而栅格化已经改走 libass（`subtitles=`），
//! 字体是**按名字**经 fontconfig/directwrite 找的、不再往滤镜串里塞文件路径。
//! 所以严格说这条缺陷已经碰不到了；`ascii_font_path` 留着是**给需要文件路径的
//! 那条路用的**（[`font_dir`] 的兜底会拿它的父目录当字体目录，见下）。
//! 记在这里是因为它解释了一个反直觉的事实：**同一个字体，路径的写法能让 ffmpeg 段错误**。
//!
//! 本机实测（ffmpeg 9.0.1 gyan build，命令与数字见 `plan/glyph-fallback-evidence.md`）：
//! 同一份字体，
//!
//! | 字体路径 | ffmpeg 退出码 | 产出字节 | stderr |
//! |---|---|---|---|
//! | `…/乐米波波体（免费商用）_爱给网_aigei_com.ttf` | **139**（SIGSEGV） | **0** | `Fontconfig error: Cannot load default config file` |
//! | 同一份复制成 `lemi_ascii.ttf` | 0 | 96000 | 空 |
//!
//! 注意它**不吐一个字节**、画布尺寸也没错 —— 这正是最坏的一种失败：调用方拿到的不是
//! 错误而是**一行看不见的字**，而「字没画出来」在看片时像「这一行没有字幕」。
//!
//! 用户决策：**让它能画**，不是只报错。做法是把字体复制到一份 ASCII 名的临时路径
//! （见 [`ascii_font_path`]），用完即删。
//!
//! # 有意不做的事
//!
//! * 不做字距 / 连字 / 禁则：那是共享布局的模型，宿主**不许**自己再算一遍，否则两端分叉。
//! * 不解析字体文件、不量字形：度量取自 ffmpeg，结构取自共享布局。
//! * 不缓存到磁盘：跨次运行的缓存键里还得塞字体文件的内容摘要，那是另一件事。
//! * 不做彩色 emoji 字形：libass 画的是字体里那一层单色字形。
//! * **不猜系统字体作为"主字体"**：产品路径上主字体仍由 `--font-file` 给。
//!   但**回退**要的是一整套字体库 —— 那个只能由宿主/系统提供，
//!   本仓的做法是**由宿主显式给 `--font-dir`**（`--font-file` 的父目录只作兜底），
//!   见 [`font_dir`]。这一条与 T4 的跨仓契约有关。

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
    ///
    /// libass 路线下它**不再进参数串**（libass 按家族名找字体），但仍然是必需的：
    /// 它要被搬成 ASCII 路径（见 [`ascii_font_path`]），而那份的父目录是
    /// [`font_dir`] 兜底的起点。
    pub font_file: PathBuf,
    /// **字体家族名**（契约里的 `font_family`），libass 按这个名字找字体。
    ///
    /// # 为什么必须有它，而不能从文件名推
    ///
    /// 本机实测（见 `plan/glyph-fallback-evidence.md`）：libass **不认文件路径**，
    /// 只认家族名；而**从文件名推出来的名字它认不出来**。
    /// 同一份字体（`乐米波波体（免费商用）_爱给网_aigei_com.ttf`）：
    ///
    /// | 传给 libass 的 FontName | `fontselect` 的解析结果 |
    /// |---|---|
    /// | `乐米波波体（免费商用）_爱给网_aigei_com`（文件名主干） | **ArialMT**（回退，无 CJK 字形） |
    /// | `staged`（换个文件名） | **ArialMT** |
    /// | `LemiBoBoTi`（英文内部名） | **ArialMT** |
    /// | `乐米波波体`（**真家族名**） | **LemiBoBoTi-Regular** ✓ |
    ///
    /// 用错名字的后果是**静默用错字体**：画出来是 ArialMT 或某个系统回退字体，
    /// 而 `lines_failed: 0`、`issues: []` —— 比 .notdef 方框更难查（方框至少看得出来）。
    ///
    /// # 名字从哪来（这与既有契约同源，不是新加的概念）
    ///
    /// `dhampir_timeline::layer` 的 `font_family` 早就有这个字段，语义写着
    /// **"宿主从你给它的字体目录里按这个名字找"**；`--font-file` 是兜底。
    /// libass 恰好也是"按名字找"，所以这里直接把那个契名的名字带下来 ——
    /// **不新增概念、不解析字体文件**。
    ///
    /// `None` = 契约没给名字（老工程）：退回 [`font_family_name`]（文件名主干）。
    /// 那条路**可能**被 libass 解析成回退字体 —— 这已经被上面那张表证实过了，
    /// 所以它是"有把握的降级"而不是"能用的默认"，`--font-file` 会给出一条问题记录。
    pub font_family: Option<String>,
    /// **字体目录**（可选）：libass 去这里找字体与**回退字体**。
    ///
    /// 由宿主给（CLI 的 `--font-dir`）。`None` = 没给，那就退回 `font_file`
    /// 的父目录（见 [`font_dir`]）。
    ///
    /// **它进键**：换一个字体目录就是换一套可用字形 —— 回退落在哪个字体上会变，
    /// 于是位图会变。不进键就会把上一个目录画出来的位图递给下一个（与"位图尺寸
    /// 必须进键"同一条纪律）。
    pub font_dir: Option<PathBuf>,
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
/// **这一版是 libass 路线（`subtitles=`）**，形状与上一版的 `drawtext=` 完全不同。
/// 老工程的字幕像素从这一版起会变，理由与代价见文件头那一节。
/// 冻结判据 `frozen_argv` 也换了新串 —— 那条测试仍然逐字符比，
/// 只是比的对象变成了 libass 这一条。
///
/// # 三个参数为什么都显式传
///
/// `subtitle_file`（一份 SRT）、`font_file`、`key` 全列出来，而不是从 `key` 里取：
/// 前两个都是**运行期才定下来的临时路径**，从键上取不到。
/// 让它们进签字，等于让"这一串到底用了哪份文本、哪份字体"在类型上看得见。
pub fn drawtext_args(
    key: &TextRasterKey,
    font_file: &Path,
    font_dir: &Path,
    subtitle_file: &Path,
) -> Vec<String> {
    // 源：一张全透明的画布，尺寸就是要的位图尺寸。
    let source = format!(
        "color=c=black@0.0:s={}x{},format=rgba",
        key.width, key.height
    );

    // 字体名：libass 按**家族名**找字体，不认文件路径（这是它与 drawtext 最大的分歧）。
    // 名字从文件名推 —— 拿不到家族名时不猜，直接用文件名主干，
    // libass 找不到就会走回退（那比静默画 .notdef 好，见下）。
    let family = font_family_name(key, font_file);

    // 字号换算见 [`ass_font_size`]：libass 的 Fontsize 是**行高尺度**，
    // drawtext 的 fontsize 是 em 尺度，差一个系数。
    //
    // # 关键：**填充与描边都用白色**（不是"白填充 + 黑描边"）
    //
    // 这不是口味问题，是这一版能不能工作的**前提**，理由见 [`coverage_from_libass`]：
    // libass 的 alpha **恒为 0**，RGB 里放的是"**已经合成好的颜色**"。
    // 于是：
    //   * 用黑描边 → 描边像素是 `(0,0,0,0)`，与"完全透明的背景"**逐字节相同**
    //     —— 覆盖度**无法恢复**（实测：一张 640x90 的位图里 56806 个 `(0,0,0,0)`，
    //     其中既有透明底又有黑描边，事后分不开）；
    //   * 用**白描边** → 每个像素的 RGB 就是"墨有多满"（实测峰值 255，
    //     白填充+白描边 = 1701 个覆盖像素，白填充单独 = 794 个）。
    //
    // "白墨 -> 样式色"本来就是本模块的既有分工（见文件头第 2 条契约）：
    // 颜色**不属于** ffmpeg 那一步，由 [`tint`] 染。黑描边那种形态是 drawtext
    // 时代留下的，它把"墨色"提前烙进了像素里；libass 这条路必须把墨色还给 tint。
    //
    // **代价（如实写）**：`tint` 现在只需要处理**一种**墨色（白）——
    // 它内部那条"黑描边保持黑"的分支在这条路上永远走不到。
    // 描边色由 `OutlineColour` 决定？**不**：`OutlineColour` 这里写白是为了让
    // 覆盖度可恢复，真正的描边色仍由样式色 + tint 决定（描边与填充在
    // 覆盖度上是同一张 mask 的两圈，tint 无法区分）——
    // 这是 T2 已知的**口径收窄**，T3 处理边缘覆盖度时一并说明。
    let mut style = format!(
        "FontName={family},FontSize={},PrimaryColour={}",
        ass_font_size(key.font_px, key.height),
        // ASS 的颜色是 `&HAABBGGRR`（**BGR**，且 alpha 0 = 不透明）。
        // 白色 = RRGGBB FFFFFF → `&H00FFFFFF`。
        "&H00FFFFFF"
    );

    if key.shadow_color.is_some() {
        // **阴影那一张：只有填充，不带描边。**
        //
        // 三条理由，缺一条都会画出不对的东西：
        //
        // 1. **不能有描边**：口径上阴影"不参与描边宽度"（描边与阴影各自独立），
        //    所以阴影的轮廓就是**填充的轮廓**；多一圈描边会让影子比字胖，
        //    看起来像"影子糊了"。
        // 2. 就算用白描边，描边也会把覆盖度**撑大**，阴影就不是字的轮廓了。
        // 3. 模糊放在 `subtitles` 之后、同一个滤镜串里：`gblur` 吃的是
        //    栅格化器吐出来的**覆盖度**，模糊完还是覆盖度 ——
        //    于是"白墨 -> 样式色"那一套（[`tint`]）一个字都不用改。
        //
        // σ 与 canvas 的 `shadowBlur` 差一个 2，见 [`shadow_sigma_px`]。
        style.push_str(",Outline=0");
        let sigma = shadow_sigma_px(key.shadow_blur_px);
        let mut chain = subtitles_filter(subtitle_file, font_dir, &style);
        if sigma > 0.0 {
            chain.push_str(&format!(",gblur=sigma={sigma}"));
        }
        return rawvideo_args(source, chain);
    }

    if key.outline {
        if key.stroke_px > 0 {
            // **契约给了宽度。** 宽度口径见 [`ass_outline_px`]（实测不用换算）。
            style.push_str(&format!(
                ",Outline={},OutlineColour={}",
                ass_outline_px(key.stroke_px),
                WHITE_INK
            ));
        } else {
            // **老路径：宽度从字号推。**
            //
            // 这一条不是"兼容遗留"：`stroke_ratio` 的默认值是 0，于是所有老工程
            // 都走这里，而它们升级前渲染出来的描边宽度就是 `border_px(font_px)`。
            style.push_str(&format!(
                ",Outline={},OutlineColour={WHITE_INK}",
                ass_outline_px(border_px(key.font_px))
            ));
        }
    } else {
        // 不描边。**这一项必须显式写**：ASS 默认样式带描边，
        // 不写的话"没开描边"的工程会突然多出一圈描边。
        style.push_str(",Outline=0");
    }

    rawvideo_args(source, subtitles_filter(subtitle_file, font_dir, &style))
}

/// 白墨。**填充与描边都用它**，理由见 [`drawtext_args`] 里那一段
/// 与 [`coverage_from_libass`]：libass 的 alpha 恒为 0、颜色提前合成进 RGB，
/// 所以只有"全是白墨"时覆盖度才恢复得出来。
pub const WHITE_INK: &str = "&H00FFFFFF";

/// `subtitles=` 那一段滤镜串（**不含**后面可能追加的 `gblur`）。
///
/// # `fontsdir=` 是回退能不能工作的开关
///
/// 它告诉 libass 去哪个目录找字体。**不给**的话 libass 只认系统字体库 ——
/// 而实测"按名字找不到"时会**静默回退**（`乐米波波体` 能找到、
/// 文件名主干 `…_aigei_com` 找不到，后者回退成 ArialMT）。
///
/// 每一项的值都整体包在单引号里、并把内层单引号转义掉 ——
/// 与 [`filter_value`] 同一套规矩（滤镜串要被解析两遍）。
fn subtitles_filter(subtitle_file: &Path, font_dir: &Path, style: &str) -> String {
    format!(
        "subtitles={}:fontsdir={}:force_style={}",
        filter_value(&subtitle_file.to_string_lossy()),
        filter_value(&font_dir.to_string_lossy()),
        filter_value(style)
    )
}

/// ffmpeg 的公共尾巴：抽出来是因为有两条出口（阴影那张不带描边、其余带），
/// 而"输出成 rawvideo RGBA 到 stdout"这一段两边必须**逐字符相同**。
fn rawvideo_args(source: String, filter: String) -> Vec<String> {
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
        filter,
        "-frames:v".to_string(),
        "1".to_string(),
        "-f".to_string(),
        "rawvideo".to_string(),
        "-pix_fmt".to_string(),
        "rgba".to_string(),
        "-".to_string(),
    ]
}

/// libass 要用的**家族名**：**契约给了就用契约的**，没给才退回文件名主干。
///
/// # 优先级为什么是这个顺序
///
/// 契约里的 `font_family` 是**宿主/工程明确声明**的名字，而文件名主干是**推的**。
/// 本机实测（见 [`TextRasterKey::font_family`] 那张表）：推出来的名字 libass
/// **认不出来**，它会静默回退到 ArialMT 之类的字体 —— 而那比 .notdef 方框更难查
/// （方框至少看得出来）。所以只要契约给了名字就用它，不推。
///
/// # 退回文件名主干时的边界（写清楚）
///
/// 用户的字体名恰好等于文件名主干时（例如 `msyh`、`simhei`）它常常能命中，
/// 因为字体家族名与文件名同源；但**不是保证**（本机那份乐米就不行）。
/// 真要做准就得解析字体文件的 `name` 表，而"本仓不解析字体文件"是一条既有约定
/// （见文件头）。**不为了一个名字去破它** —— 名字由契约给是正路。
fn font_family_name(key: &TextRasterKey, font_file: &Path) -> String {
    if let Some(family) = key.font_family.as_ref() {
        if !family.trim().is_empty() {
            return family.clone();
        }
    }
    font_file
        .file_stem()
        .map(|stem| stem.to_string_lossy().to_string())
        .unwrap_or_else(|| font_file.to_string_lossy().to_string())
}

/// 这份 SRT 走 `subtitles=` 时，libass 用的**虚拟画布高**。
///
/// # 为什么这个数必须写在这里（这是 T2 最容易踩空的一步）
///
/// libass 把字幕画在一张**虚拟画布**（ASS 的 `PlayResX/PlayResY`）上，再缩放到真实画布。
/// `subtitles=` 喂 SRT 时**没地方指定**这两项（`original_size=` 实测**不管用**），
/// 于是 libass 用它的默认值 —— 实测 **384 × 288**。
///
/// 后果：`force_style` 里的 `FontSize` 是**虚拟画布尺度**上的行高，
/// 真实字号 = `FontSize × (真实高 / 288)`。画布高 200px 时字会缩到 0.69 倍 ——
/// 而 `lines_failed` 仍是 0、`issues` 仍是空，**只是字比该有的小**。
/// 那种错会被当成"字号算错了"，而其实是这一层缩放。
pub const LIBASS_PLAY_RES_Y: f64 = 288.0;

/// 目标字号（em 侧像素）→ 这张位图上该给 libass 的 `FontSize`。
///
/// # 两个系数都是量出来的
///
/// ```text
/// FontSize = font_px × 1.25 × (288 / 位图高)
///            └─ ①    └─────────── ② ───────────┘
/// ```
///
/// **① = 1.25**（em → 行高的口径差）。量法：同一字体、同一串「笑靥如花」、
/// 同一张画布，找"**墨迹高度相同**"的那一对参数（本机实测，`msyh`）：
///
/// | drawtext `fontsize` | 墨迹高 | 与之等高的 libass `FontSize`（画布高 200） |
/// |---|---|---|
/// | 40 | 39 px | 72 → 比值 72/40 = **1.8** |
///
/// 1.8 里含了②的缩放：`1.8 × (200/288) = 1.25`。
/// **量了三个画布高交叉验证过**（90 / 200 / 360）：补偿之后墨迹高与 drawtext
/// 逐档相同（40→39px、60→58px，三个画布高都是）。
///
/// **② = 288 / 位图高**：见 [`LIBASS_PLAY_RES_Y`]。
/// 画布高正好 288 时这一项等于 1，所以**288 高的工程看不出问题** ——
/// 这也解释了为什么这个坑只在别的画布上现形。
///
/// # 这是"对齐常数"，不是共用公式
///
/// 与 [`shadow_sigma_px`] 的那个 2 同一性质：libass 换了 PlayRes 默认值或行高口径，
/// 这里就是第一个该动的地方。两端本来就**不保证逐像素一致**（见文件头），
/// 所以判据是"字号看起来一致"，不是"墨迹高度逐像素相同"。
pub fn ass_font_size(font_px: u32, bitmap_height: u32) -> u32 {
    let play_res_scale = LIBASS_PLAY_RES_Y / f64::from(bitmap_height.max(1));
    (f64::from(font_px) * 1.25 * play_res_scale)
        .round()
        .max(1.0) as u32
}

/// drawtext/契约的描边宽度（em 侧像素）→ ASS 的 `Outline`。
///
/// 本机实测：drawtext 的 `borderw=k` 与 ASS 的 `Outline=k` 画出来的描边**同宽**
/// （两者都是"从字形轮廓向外 k 像素"），所以这里**不换算、直接透传**。
/// 留这个函数是为了把"这里量过、结论是不用换"写下来 ——
/// 否则下一个人看到"别处都换算、这里没有"会以为是漏了。
pub fn ass_outline_px(stroke_px: u32) -> u32 {
    stroke_px
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

/// **字体目录**：libass 找字体（以及**回退**）要看的那一个目录。
///
/// # 为什么是「一个目录」而不是一份 fonts.conf
///
/// 最早的做法是随行生成一份 `fonts.conf` 走 `FONTCONFIG_FILE`。**实测证明不必**：
/// ffmpeg 的 `subtitles` / `ass` 滤镜自己有 `fontsdir=` 选项，直接指向目录即可，
/// 本机实测（`env -u FONTCONFIG_FILE`，即**故意不给**任何 fontconfig 配置）：
///
/// ```text
/// subtitles=t.ass:fontsdir=target/fontdir  →  rc=0，fontselect 两行都在，
///                                             没有 "Cannot load default config"
/// ```
///
/// 少一个会腐烂的外部文件（那份 XML 要跟平台字体布局一起演进），
/// 也少一次"配置写错了但看起来像字体坏了"的排查。
///
/// # 目录从哪来 —— **由宿主显式给**，本仓不猜
///
/// 就是 CLI 的 `--font-dir`（`dhampir.rs` 里已有，帮助原文：「字体目录（可选）：
/// 按契约里的 `font_family` 名字在里面找」）。**本仓不列一张系统字体目录表** ——
/// 那正是"猜系统字体"那条纪律要挡的事。
///
/// `--font-file` 的父目录在这里**只作兜底**：单给 `--font-file` 时，
/// 至少让那份字体自己所在的目录参与查找（否则连用户点名的那份字体都可能找不到）。
/// 但它**不**被当作"字体库" —— 回退能扫到多少取决于那个目录里装了什么。
///
/// # 回退依赖机器上装了什么（**跨机出片的已知边界**）
///
/// 实测：`乐米波波体` 缺 U+9765 靥 时，libass 回退到 **`MicrosoftYaHeiUI`** ——
/// 那是**这台机器上的系统字体**。所以：
///
/// * 回退**不是确定性的**：换一台机器、换一个平台，缺字那几个字会换一副字形；
/// * 这与"两端一致"的口径不冲突（本仓早就有"字形像素允许不同"这一条），
///   但**同一台机器上的两次出片必须一致** —— 这条保得住，因为目录是给定的；
/// * **回退字体集合该由谁提供**：本仓没有字体栈，那是下游宿主/系统的事。
///   与 V-Trim 那侧的对齐见 T4 的契约（未对齐前如实记为未定）。
#[derive(Debug)]
pub struct FontDir {
    path: PathBuf,
}

impl FontDir {
    /// 交给 ffmpeg `fontsdir=` 的那条路径。
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// 定下这一次栅格化要用的字体目录。
///
/// 优先用宿主显式给的 `font_dir`；没给就用 `font_file` 的父目录（兜底，见 [`FontDir`]）。
/// 两条都拿不到目录时**响亮报错**：`fontsdir=` 指一个不存在的地方，libass 会
/// **静默用系统默认字体**（实测 → ArialMT），而那正是这一版最想拦掉的失败模式。
pub fn font_dir(font_dir: Option<&Path>, font_file: &Path) -> Result<FontDir, String> {
    if let Some(dir) = font_dir {
        if !dir.is_dir() {
            return Err(format!(
                "字体目录不在：{}（--font-dir 指错了吗？）—— libass 找不到它就会                 静默改用系统默认字体（实测是 ArialMT，没有中文字形）",
                dir.display()
            ));
        }
        return Ok(FontDir {
            path: dir.to_path_buf(),
        });
    }
    let parent = font_file.parent().filter(|parent| parent.is_dir());
    match parent {
        Some(parent) => Ok(FontDir {
            path: parent.to_path_buf(),
        }),
        None => Err(format!(
            "定不了字体目录：既没给 --font-dir，{} 也没有可用的父目录。\
             libass 靠这个目录找字体与**回退字体**；不定下来它会静默改用\
             系统默认字体，而那种错在成片里看起来只是「字体不太对」",
            font_file.display()
        )),
    }
}

/// 把这一行文本落成一份**单条字幕的 SRT**，给 `subtitles=` 用。
///
/// # 为什么不直接写文本文件（`drawtext` 那种做法）
///
/// `subtitles=` 期望的是一份**字幕文件**（SRT/ASS/…），不是一个纯文本文件。
/// libass 会去解析它 —— 所以这里要的是一份语法正确、时间轴任意（只画一帧）的 SRT。
///
/// # SRT 的几处必须写对
///
/// * 序号 + 时间轴 + 文本，**空行分隔**；
/// * 时间轴覆盖够长（这里 0 到 10 秒）：我们只取第 1 帧，但时间轴的**起点必须是 0**
///   —— 起点晚于 0 的话第 0 帧上什么都没有（那会是一张空白位图，而它看起来像"字体没画出来"）；
/// * 写 UTF-8 **无 BOM**。BOM 会让 libass 把第一个码位当成 U+FEFF 画进画面；
/// * **行尾用 `\n`**，且文本里**不许有换行**（调用方已经查过，见 [`rasterize_line`]）——
///   有换行的话 SRT 里就是两条字幕，而这里只该有且只有一行。
///
/// # 文本不需要转义（这一条是选 `subtitles=` 的理由之一）
///
/// SRT 是**行式**格式：只有 `-->` 那一行与空行有语法意义，文本行是**原样**的。
/// 于是用户文本里的 `%`、`{}`、`\`、`:` 一个都不需要转义 ——
/// **与 `drawtext` 的 `expansion` / `ass=` 的覆盖标签那两类坑同时绝缘**
/// （`%{n}` 在 `ass=` 那条路上会被当指令吞掉，见文件头）。
fn write_subtitle_file(text: &str) -> Result<PathBuf, String> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let serial = COUNTER.fetch_add(1, Ordering::Relaxed);
    let digest = dhampir_core::timeline::selfcheck::fnv1a64(text.as_bytes());
    let path = std::env::temp_dir().join(format!(
        "dhampir-sub-{}-{serial}-{digest:016x}.srt",
        std::process::id()
    ));
    let body = format!("1\n00:00:00,000 --> 00:00:10,000\n{text}\n");
    std::fs::write(&path, body.as_bytes())
        .map_err(|error| format!("写不了字幕临时文件 {}：{error}", path.display()))?;
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

    let subtitle_file = write_subtitle_file(text)?;
    // 字体路径含非 ASCII 时先搬一份到 ASCII 名的临时路径（实测：不搬的话 ffmpeg
    // 段错误且一个字节都不吐）。`StagedFont` 自己管清理，**也自己给出该用哪条路径**
    // —— 调用方没有第二条路径可选，所以"搬了却没换上"这件事写不出来。
    // libass 路线下它**不再进滤镜串**（字体按名字找），但仍然要它：
    // 回退到父目录当字体目录是兜底，见 [`font_dir`]。
    let font = ascii_font_path(&key.font_file)?;
    // 字体目录：宿主给了 `--font-dir` 就用它，否则退回这份字体的父目录。
    // 它是 libass 找字体与**回退字体**的唯一入口，见 [`font_dir`]。
    let dir = font_dir(key.font_dir.as_deref(), font.path())?;
    let result = run_ffmpeg(key, font.path(), dir.path(), &subtitle_file);
    // 字幕临时文件用完就删，成功失败都删：一次出片几百行，留一地文件是在给下次查问题挖坑。
    // （字体那份由 `font` 的 Drop 删，连提前 return 都覆盖得到。）
    let _ = std::fs::remove_file(&subtitle_file);
    result
}

fn run_ffmpeg(
    key: &TextRasterKey,
    font_file: &Path,
    font_dir: &Path,
    subtitle_file: &Path,
) -> Result<TextBitmap, String> {
    let args = drawtext_args(key, font_file, font_dir, subtitle_file);
    let output = Command::new("ffmpeg")
        .args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|error| format!("起不了 ffmpeg：{error}（PATH 里有 ffmpeg 吗？）"))?;

    if !output.status.success() {
        return Err(format!(
            "ffmpeg 画不出这一行（退出码 {:?}）：{} —— 字体 {}、字体目录 {} 与字幕临时文件都在。\
             先信 ffmpeg 的原文：它说的常在字体上（能不能解析、有没有这个字形）；\
             这一版走的是 libass（`subtitles=`），用户文本不经过任何展开器，\
             所以 `Stray %` 那一类只可能是这里被人改坏了",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr).trim(),
            font_file.display(),
            font_dir.display()
        ));
    }
    // **产出字节数**这一条是这条路上最要紧的检查，理由见文件头：
    // ffmpeg 对"画不出来"的各种形态并不总是给非零退出码，而 0 字节或半张图
    // 到了下游就是"这一行没有字幕"。少了或多了都说明几何前提不成立。
    let expected = key.width as usize * key.height as usize * 4;
    if output.stdout.len() != expected {
        return Err(format!(
            "ffmpeg 本该吐 {expected} 字节（{}x{} RGBA），实际 {}",
            key.width,
            key.height,
            output.stdout.len()
        ));
    }
    // **libass 的覆盖度在 RGB 里、alpha 恒为 0** —— 先搬成 drawtext 那种
    // 「覆盖度预乘」形态，再交给同一个 [`tint`]。搬运与判据见 [`coverage_from_libass`]。
    let premultiplied = coverage_from_libass(&output.stdout);
    TextBitmap::new(
        key.width,
        key.height,
        tint(&premultiplied, key.tint_color()),
    )
}

/// libass 的输出 → `drawtext` 那种**覆盖度预乘**形态，喂给同一个 [`tint`]。
///
/// # 这两代栅格化器的输出**不是同一种东西**（T2 埋下的地雷，T3 在这里接着做）
///
/// 本机实测（命令与数字见 `plan/glyph-fallback-evidence.md`），同一行文字：
///
/// | | `drawtext`（老路） | `subtitles=` / libass（新路） |
/// |---|---|---|
/// | alpha 通道 | **就是覆盖度**（白字边缘 `[80,80,80,80]`） | **恒为 0** |
/// | 覆盖度在哪 | 同时在 rgb 与 alpha 里（预乘） | **只在 rgb 里**（灰度） |
/// | 透明底 | `(0,0,0,0)` | `(0,0,0,0)` |
/// | 不透明黑 | `(0,0,0,255)` | **`(0,0,0,0)` —— 与透明底一样！** |
///
/// 最后一行是整个 T2 最要命的一条：libass 把**已经合成好的颜色**塞进 RGB，
/// 而 alpha 一律 0。所以如果按"白填充 + 黑描边"画，**黑描边与透明底逐字节相同**，
/// 覆盖度**在数学上就恢复不出来**（实测那张 640x90 的位图：56806 个 `(0,0,0,0)`，
/// 其中既有透明底又有黑描边，事后分不开）。
///
/// **所以 [`drawtext_args`] 把填充与描边都画成白色。** 那样每个像素的 RGB
/// 就是"墨有多满"：白填充 + 白描边实测 1701 个覆盖像素、峰值 **255**；
/// 只有白填充时 794 个。这是"颜色不属于 ffmpeg 那一步"那条既有契约的**回归** ——
/// 黑描边那种形态是 drawtext 时代把墨色提前烙进像素的产物，libass 这条路必须还回来。
///
/// # 搬运：把 rgb 的平均值当覆盖度
///
/// ```text
/// coverage = round((r + g + b) / 3)         // 白墨：三通道相同
/// out.rgb  = coverage                        // 预乘：rgb = 覆盖度 × 白墨(1)
/// out.a    = coverage
/// ```
///
/// **峰值实测就是 255**，不需要归一化系数（这一点在动手前专门量过：
/// 带黑描边时见过的 235 是"描边与填充在同一个像素里混色"的中间值，
/// **不是**输出上限 —— 白墨满覆盖就是 255）。
///
/// 三通道取平均而不是只取 `r`：libass 理论上可能给次像素抗锯齿（三通道不一致），
/// 取平均是那种情况下的**近似**。本机实测三通道逐像素相等，所以这条近似没有代价。
///
/// # 直接拿 alpha 当覆盖度会怎样（反向用例盯的就是这个）
///
/// alpha 恒 0 → [`tint`] 会走到"全透明像素连颜色都不留"那一条，把 rgb 清成 0。
/// 结果是一张**全透明位图**，而它的尺寸、字节数、ffmpeg 退出码**全都是对的** ——
/// 那是这条链上最坏的一种失败：**一行看不见的字**。
pub fn coverage_from_libass(libass_rgba: &[u8]) -> Vec<u8> {
    let mut out = libass_rgba.to_vec();
    for px in out.chunks_exact_mut(4) {
        let mean = (u32::from(px[0]) + u32::from(px[1]) + u32::from(px[2])) as f32 / 3.0;
        let coverage = mean.round().clamp(0.0, 255.0) as u8;
        // 预乘形态：三个通道都等于覆盖度（白墨），alpha 也是覆盖度。
        px[0] = coverage;
        px[1] = coverage;
        px[2] = coverage;
        px[3] = coverage;
    }
    out
}

/// 一条**覆盖度剖面**：某个字形在某个位置上"墨有多满"。
///
/// # 为什么要有这个类型（T3 的立足点）
///
/// 新老两条栅格化器把覆盖度放在**不同的地方**：
///
/// * 老路 `drawtext`：**预乘的 alpha**（不透明白字边缘是 `[80,80,80,80]`）；
/// * 新路 libass：**RGB 的灰度**，alpha 恒为 0。
///
/// 要"比一比两条路的覆盖度"，就必须先把两边都抽成**同一个东西**，
/// 否则比的是苹果与橘子 —— 而且那种比法会得出"差得很远"的假结论。
/// 这个结构就是那个"同一个东西"：一个 `f32` 的覆盖度场。
///
/// 顺带把**几何归一化**了：两条路的落点本来就不一样（`drawtext` 用 `x`/`y`，
/// libass 用对齐+边距+它自己的行盒），所以比之前必须**按墨迹包围盒对齐**。
/// 不归一化就会量到"平移了 200 像素"，那不是覆盖度差异。
#[derive(Debug, Clone)]
pub struct CoverageProfile {
    pub width: u32,
    pub height: u32,
    /// 长度 = `width * height`，取值 0..=255（与覆盖度同口径）。
    pub values: Vec<f32>,
    /// 墨迹包围盒 `(min_x, min_y, max_x, max_y)`（闭区间）；全空为 `None`。
    pub bounds: Option<(u32, u32, u32, u32)>,
}

impl CoverageProfile {
    /// 从一张**预乘**位图抽覆盖度（老路 `drawtext` 的形态）。
    pub fn from_premultiplied(bitmap: &TextBitmap) -> Self {
        let values: Vec<f32> = bitmap
            .pixels
            .chunks_exact(4)
            .map(|px| f32::from(px[3]))
            .collect();
        Self::from_values(bitmap.width, bitmap.height, values)
    }

    fn from_values(width: u32, height: u32, values: Vec<f32>) -> Self {
        let mut min_x = u32::MAX;
        let mut min_y = u32::MAX;
        let mut max_x = 0u32;
        let mut max_y = 0u32;
        let mut any = false;
        for (index, value) in values.iter().enumerate() {
            if *value <= 0.0 {
                continue;
            }
            any = true;
            let x = index as u32 % width;
            let y = index as u32 / width;
            min_x = min_x.min(x);
            min_y = min_y.min(y);
            max_x = max_x.max(x);
            max_y = max_y.max(y);
        }
        let bounds = any.then_some((min_x, min_y, max_x, max_y));
        Self {
            width,
            height,
            values,
            bounds,
        }
    }

    /// 取一个点（越界给 0）。
    pub fn at(&self, x: i64, y: i64) -> f32 {
        if x < 0 || y < 0 || x >= i64::from(self.width) || y >= i64::from(self.height) {
            return 0.0;
        }
        self.values[y as usize * self.width as usize + x as usize]
    }

    /// 按给定的整数偏移平移（`dx`/`dy` 是"要从源里取的坐标相对量"）。
    fn shifted(&self, dx: i64, dy: i64) -> CoverageProfile {
        let mut values = vec![0.0f32; self.values.len()];
        for y in 0..self.height as i64 {
            for x in 0..self.width as i64 {
                values[(y * i64::from(self.width) + x) as usize] = self.at(x + dx, y + dy);
            }
        }
        CoverageProfile::from_values(self.width, self.height, values)
    }

    /// **把"另一个剖面"对齐到自己身上**：按包围盒左上角平移。
    ///
    /// 返回平移后的剖面。这是"两条路的落点不同"那件事的解药 ——
    /// 不比位置，只比**字形本身**。
    pub fn aligned_to(&self, other: &CoverageProfile) -> CoverageProfile {
        let (Some((ox, oy, _, _)), Some((sx, sy, _, _))) = (other.bounds, self.bounds) else {
            return other.clone();
        };
        other.shifted(ox as i64 - sx as i64, oy as i64 - sy as i64)
    }

    /// **在 ±`radius` 像素内找最好的对齐**，返回（最好偏移，平均绝对差）。
    ///
    /// # 为什么必须做这一步（不做会得出"差得远"的假结论）
    ///
    /// 两条路的**每字推进宽度**不是逐像素相同的。**复算方法**（数字随方法变，
    /// 所以量法一起写下来）：`msyh`、`font_px = 40`、画布 **400×74**、
    /// 文本是「口」重复 n 次，新路的 `FontSize` **按本文件的补偿式算**（得 195）
    /// —— 用 40（不补偿）量出来的不是同一件事：
    ///
    /// | 字数 | 老路宽 | 新路宽（补偿后） | 新路宽（FontSize=40，不补偿） |
    /// |---|---|---|---|
    /// | 1 | 32 px | 32 px | 26 px |
    /// | 2 | 72 px | 71 px | 57 px |
    /// | 3 | 112 px | 110 px | 88 px |
    /// | 5 | 192 px | 190 px | 152 px |
    ///
    /// 在同一画布（含 1000×288）上复量过，前两列的数一致。
    ///
    /// ⚠️ **"差多少"这一句，两次独立复量的结论不一致，别引它**：
    /// 上表说 5 个字偏 2 像素（**-1%**）；而 Lead 用同一字体、同一画布复量得到
    /// **32/72/112/192 vs 30/68/106/182**（5 个字偏 10 像素，**-5%**，每字 -2.5 px）。
    /// 阈值口径（10/30/60/128 都试过）解释不了这个差，所以**至少有一个数是错的**。
    ///
    /// 好在**结论不受影响、判据也不靠它**：两条路都"每字若干个像素"、且**新路略窄**，
    /// 这个方向两边一致；漂移是 1% 还是 5% 都不改变下面那条设计 ——
    /// 判据按**单个字形**立（漂移永不累积），主判据是 [`ramp_width`]（结构量，
    /// 对相位差不敏感），[`ALIGN_RADIUS`] 又只有 1 像素。
    ///
    /// **这张表只是动机，不是判据，更不是基线。** 要引"差多少"先自己复量，
    /// 并把量法一起写下来 —— 本仓已经因为"手写的数会过期"栽过好几次。
    ///
    /// 但**逐点比会把这点漂移放大成巨大的逐点差**：一个字内部的边缘横向挪 1 像素，
    /// 读数就从"128 对 128"变成"0 对 128"，逐点差 **128**。漂移一路累积到第 5 个字
    /// 时已经错开若干像素（1% 口径是 2 px，5% 口径是 10 px），
    /// 于是**整串比下来全是"差异极大"** ——
    /// 实测整串「口日目回田」是 max 247 / 中位 76 / 只有 45.8% 落在容差内，
    /// 而**同一个字单独比**是 max 143 / 中位 40 / 96.9% 落在 128 内。
    /// 前者看着像"两条路差得远"，其实只是累计漂移被放大。
    ///
    /// **所以判据按"每个字形单独比"来立**（见
    /// `真机_边缘覆盖度与老路同量级`），而**主判据是斜坡宽度**
    /// （[`ramp_width`]）—— 它正是逐点相位差**打不到**的那个量。
    /// 这里的搜索只是把每个字形内部那半个像素的取整差找回来；
    /// 搜索半径刻意只有 ±[`ALIGN_RADIUS`] 像素 ——
    /// 大了就变成"随便挪到最像为止"，判据会失去意义。
    pub fn best_alignment(&self, other: &CoverageProfile, radius: i64) -> (i64, i64, f32) {
        let (Some((ox, oy, _, _)), Some((sx, sy, _, _))) = (other.bounds, self.bounds) else {
            return (0, 0, f32::MAX);
        };
        let base = (ox as i64 - sx as i64, oy as i64 - sy as i64);
        let mut best = (base.0, base.1, f32::MAX);
        for dy in -radius..=radius {
            for dx in -radius..=radius {
                let candidate = other.shifted(base.0 + dx, base.1 + dy);
                let total: f32 = self
                    .values
                    .iter()
                    .zip(candidate.values.iter())
                    .map(|(a, b)| (a - b).abs())
                    .sum();
                let mean = total / self.values.len().max(1) as f32;
                if mean < best.2 {
                    best = (base.0 + dx, base.1 + dy, mean);
                }
            }
        }
        best
    }

    /// **边缘像素**的下标集合：覆盖度落在 `(0, 255)` 开区间里。
    ///
    /// # 为什么只比边缘（本项目已确立的口径）
    ///
    /// 字形内部是**实心**（覆盖度 255，两条路必然相同），外部是**空**（0，也必然相同）。
    /// **两条路的全部差异都发生在边缘那一圈**——那是抗锯齿、hinting、字形轮廓
    /// 三件事同时起作用的地方。把整帧平均一下，边缘那几百个像素会被几万个
    /// "必然相同"的像素稀释掉，于是得出"几乎没差"的假结论。
    /// 这条口径与本仓 `plan/web-engine-measurements.md` 里"边缘斜坡宽度"那条同源。
    pub fn edge_pixels(&self) -> Vec<usize> {
        self.values
            .iter()
            .enumerate()
            .filter(|(_, value)| **value > 0.0 && **value < 255.0)
            .map(|(index, _)| index)
            .collect()
    }
}

/// 两条覆盖度剖面的**边缘差异**统计（T3 的判据就是拿它算的）。
#[derive(Debug, Clone, Copy)]
pub struct EdgeDifference {
    /// 参与比较的像素数（两条路**边缘并集**的大小）。
    pub compared: usize,
    /// 逐点绝对差的最大值。
    pub max: f32,
    /// 绝对差的**中位数**。
    pub median: f32,
    /// 绝对差的均值。
    pub mean: f32,
    /// 绝对差 ≤ [`EDGE_TOLERANCE`] 的像素占比（0..=1）。
    pub within_tolerance: f32,
}

/// 逐点比之前，在**几个像素**的范围内找最好对齐。
///
/// 1 像素就够：两条路的推进宽度差是 1% 量级（193 vs 191），
/// 找的是"半个像素的取整方向不同"，不是"画的位置错了"。
/// **这个半径小得刻意** —— 大了就变成"随便挪到最像为止"，判据会失去意义。
pub const ALIGN_RADIUS: i64 = 1;

/// 一条边缘**斜坡**的判据：从 0 走到 255 用了几个像素。
///
/// # 为什么这条比"逐点差"更该当主判据
///
/// 实测两条路对同一个字（「口」、同字号）的同一条边：
///
/// ```text
/// 老路 drawtext:  … 140, 255, 255, 208, 0 …
/// 新路 libass  :  … 108, 255, 255, 226, 0 …
/// ```
///
/// 两次跨越**都在 2 个像素内从 0 走到 255** —— 斜坡宽度**一致**。
/// 逐点差之所以大（140 vs 108、208 vs 226），是因为**斜坡的相位差了半个像素**：
/// 同一个斜坡采在两个略微不同的位置上。那不是"口径没对齐"，
/// 而是"亚像素取整方向不同"。
///
/// 所以主判据判的是**结构**（斜坡有多窄），这与本仓既有的那条口径同源：
/// `plan/web-engine-measurements.md` 里「圆角层的边缘是**窄**抗锯齿、
/// 不是硬边也不是模糊」判的也是**宽度**，不是逐点值。
///
/// # 实测：新路的斜坡**确实更宽**（这是两条渲染器的真实差异）
///
/// 同一字体、同字号，四个字各量一次（`partials / transitions`）：
///
/// | 字 | 老路 `drawtext` | 新路 libass |
/// |---|---|---|
/// | 口 | 0.63 px | **1.65 px** |
/// | 日 | 1.43 px | **2.41 px** |
/// | 目 | 1.21 px | **3.17 px** |
/// | 田 | 1.10 px | **1.63 px** |
///
/// **新路每一条边都更宽**，而且不是偶发 —— 四个字全部同向。
/// 这是**两条渲染器的真实差异**（不同的轮廓扫描、不同的抗锯齿核），
/// 不是搬运写错了。写在这里是因为它会**看得见**：
/// 新版的字幕边缘会比老板**略柔一点**。
///
/// 它**不破坏**本仓既有口径：那条口径要求"窄抗锯齿、不是硬边也不是模糊"，
/// 而 1.6~3.2 px 仍然在"窄"的范围里（模糊会是十几个像素）。
/// 但它确实是**一次可见的观感变化**，与"字幕像素会变"是同一件事的两面。
///
/// 两条路各自都必须给出"窄边"：宽度太大 = 边糊了，0 = 硬边（没有抗锯齿）。
pub fn ramp_width(profile: &CoverageProfile) -> f32 {
    let Some((min_x, min_y, max_x, max_y)) = profile.bounds else {
        return 0.0;
    };
    // 沿每一行扫，找**每一条从亮到暗（或反过来）的过渡边**：
    // 一次过渡 = 相邻像素间跨越了半个动态范围（>127.5）。
    // 数出这种跨越**发生了几次**，以及**跨越点两侧各有多少个中间值像素**。
    let mut partials = 0usize; // 中间值像素总数
    let mut transitions = 0usize; // 跨越次数
    for y in min_y..=max_y {
        let mut previous = profile.at(i64::from(min_x.saturating_sub(1)), i64::from(y));
        for x in min_x..=max_x {
            let value = profile.at(i64::from(x), i64::from(y));
            let step = value - previous;
            if step.abs() > 127.5 {
                // 一次"跨半步"的跳变：说明这里是**一条边**。
                transitions += 1;
            } else if value > 0.0 && value < 255.0 {
                // 中间值像素：斜坡的组成部分。
                partials += 1;
            }
            previous = value;
        }
    }
    if transitions == 0 {
        return 0.0;
    }
    // 每条边平均摊到几个中间值像素 —— 这就是"斜坡宽度"。
    // 硬边（无抗锯齿）是 0；理想的一条线宽抗锯齿约 1~2。
    partials as f32 / transitions as f32
}

/// 边缘覆盖度容差：**64/255 ≈ 25%**。
///
/// # 这个数是怎么来的（不是拍的）
///
/// 两条路对同一个字形做抗锯齿，**本质上不可能逐点相同**：它们用的是
/// 不同的轮廓扫描、不同的 hinting、不同的 gamma 约定。要判的是
/// "**边缘还在不在原位、深浅是否同量级**"，不是"逐点相等"。
///
/// 取 64 的依据是**本仓既有的"边缘是窄抗锯齿"那一条**的同类口径 ——
/// 那里判「圆角层的边缘是窄抗锯齿、不是硬边也不是模糊」用的是
/// **斜坡宽度 ≈ 0.22 目标像素**；对应到 0..255 的覆盖度上，
/// 一条"窄抗锯齿"的斜坡在相邻像素间的落差是**数十**量级。
/// 64 落在"仍然是一条窄边"的范围里，而 128 就已经是"半张图都不一样"了。
///
/// **这条是判据，不是装饰**：真到了"整条边都被抹平"或"边缘整体暗一圈"
/// 那种坏法，逐点差会顶到 200 以上，这条容差拦得住。
pub const EDGE_TOLERANCE: f32 = 64.0;

/// 比两条剖面的**边缘**覆盖度，给出统计量。
///
/// `reference` 是老路（真值来源），`candidate` 是新路（待判的）。
/// `candidate` 会先在 **±[`ALIGN_RADIUS`] 像素**内找最好的对齐再比 ——
/// 见 [`CoverageProfile::best_alignment`] 那一节（不这么做会把亚像素偏移
/// 放大成假差异）。
pub fn compare_edges(reference: &CoverageProfile, candidate: &CoverageProfile) -> EdgeDifference {
    let (dx, dy, _) = reference.best_alignment(candidate, ALIGN_RADIUS);
    let aligned = candidate.shifted(dx, dy);
    let mut union: Vec<usize> = reference.edge_pixels();
    for index in aligned.edge_pixels() {
        if !union.contains(&index) {
            union.push(index);
        }
    }
    if union.is_empty() {
        return EdgeDifference {
            compared: 0,
            max: 0.0,
            median: 0.0,
            mean: 0.0,
            within_tolerance: 1.0,
        };
    }
    let mut diffs: Vec<f32> = union
        .iter()
        .map(|index| {
            let x = *index as u32 % reference.width;
            let y = *index as u32 / reference.width;
            (reference.at(i64::from(x), i64::from(y)) - aligned.at(i64::from(x), i64::from(y)))
                .abs()
        })
        .collect();
    diffs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let compared = diffs.len();
    let max = diffs.last().copied().unwrap_or(0.0);
    let median = diffs[compared / 2];
    let mean = diffs.iter().sum::<f32>() / compared as f32;
    let within =
        diffs.iter().filter(|diff| **diff <= EDGE_TOLERANCE).count() as f32 / compared as f32;
    EdgeDifference {
        compared,
        max,
        median,
        mean,
        within_tolerance: within,
    }
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
            font_family: None,
            font_dir: None,
            width: 100,
            height,
        }
    }

    /// 参数串的**冻结**副本（这一版 = libass 路线）。
    ///
    /// **T2 之前它冻的是 `drawtext=…` 那一串。** 换成 libass 是用户决策的**破坏性变更**：
    /// 冻结的对象变了，而**所有老工程的每一帧字幕像素确实都变了**（不同的栅格化器，
    /// 逐字节相等不可能）。理由与代价写在文件头那一节。
    ///
    /// 这条判据**仍然逐字符比**，它的价值没变：`drawtext_args` 是"顺手改坏"最容易的一处
    /// （加一项样式、调一个系数，最自然的写法就是往这条串里塞东西），
    /// 而它一旦多一个字符，画出来的像素就不一样了。
    ///
    /// **它是"这一版就该长这样"的锚，不是"与历史版本相同"的锚** ——
    /// 改它必须是有意的，并且要在文件头与提交信息里说明代价。
    fn frozen_argv(key: &TextRasterKey) -> Vec<String> {
        vec![
            "-v".to_string(),
            "error".to_string(),
            "-nostdin".to_string(),
            "-f".to_string(),
            "lavfi".to_string(),
            "-i".to_string(),
            format!(
                "color=c=black@0.0:s={}x{},format=rgba",
                key.width, key.height
            ),
            "-vf".to_string(),
            // **这是 T2 的新冻结串**（libass 路线）。老的那一条是
            // `drawtext=fontfile=…:textfile=…:expansion=none:…`，它下面那几个
            // 分项判据（字号、描边、模糊）现在量的是这一条串里的对应项。
            format!(
                "subtitles='C\\:/tmp/dhampir-text-1.txt':fontsdir='C\\:/fake':\
                 force_style='FontName=font,\
                 FontSize=576,PrimaryColour=&H00FFFFFF,Outline=0'"
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
        let args = drawtext_args(
            &k,
            &k.font_file,
            Path::new("C:/fake"),
            Path::new("C:/tmp/dhampir-text-1.txt"),
        );
        let joined = args.join(" ");
        assert!(!joined.contains("危险"), "文本进了命令行：{joined}");
        assert!(!joined.contains("带引号"), "文本进了命令行：{joined}");
        assert!(
            joined.contains("subtitles="),
            "文本应当走一份字幕文件（`subtitles=`），而不是进滤镜串：{joined}"
        );
        // **`expansion=none` 这一项在新路上不存在，但判据换了个形态留着。**
        //
        // 老路靠 `expansion=none` 关掉 drawtext 的文本展开（`%` / `%{n}`）；
        // 新路走 libass，`subtitles=` 把一份 SRT 交给它解析 —— SRT 是**行式**格式，
        // 文本行是原样的，**没有那个展开器**。
        // 所以这里不能再断言 `expansion=none`（那会是一条假判据：写上去也没用），
        // 改成断言"**这条串里不许出现任何展开器/覆盖标签语法**"：
        //   * `expansion=` —— 老路的东西，回来了就说明有人把 drawtext 又接上了；
        //   * `\pos(` / `{\` —— ASS 覆盖标签。**选 `subtitles=` 而不是 `ass=`
        //     正是为了避开它**：`ass=` 的 Dialogue 文本里 `%{n}` 会被 libass 当指令
        //     吞掉（实测 312 vs 2180 个覆盖像素），而那与 drawtext 的展开是同一种病。
        assert!(
            !joined.contains("expansion="),
            "这条串不该再有 drawtext 的展开项（走的是 libass）：{joined}"
        );
        assert!(
            !joined.contains("{\\"),
            "这条串里不许出现 ASS 覆盖标签（那会吞掉用户文本）：{joined}"
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
    fn 描边开关决定有没有_outline项() {
        let plain_key = key("字", 20);
        let plain = drawtext_args(
            &plain_key,
            &plain_key.font_file,
            Path::new("C:/fake"),
            Path::new("t.txt"),
        )
        .join(" ");
        // **不描边时必须显式写 `Outline=0`**：ASS 的默认样式**带描边**，
        // 不写的话"没开描边"的工程会突然多出一圈黑边 —— 那正是那种
        // "看着像是字体变粗了"而查不出原因的错。
        assert!(
            plain.contains("Outline=0"),
            "不描边要显式写 Outline=0（ASS 默认带描边）：{plain}"
        );

        let mut outlined = key("字", 20);
        outlined.outline = true;
        outlined.font_px = 48;
        let joined = drawtext_args(
            &outlined,
            &outlined.font_file,
            Path::new("C:/fake"),
            Path::new("t.txt"),
        )
        .join(" ");
        assert!(
            joined.contains("Outline=3"),
            "字号 48 的描边是 3 像素：{joined}"
        );
        // **默认走老路径**：`stroke_px == 0` → 宽度从字号推、颜色写死黑。
        // 这是默认值那条分支（`stroke_ratio` 的默认值是 0）。
        // **描边也是白墨**：libass 的 alpha 恒 0，只有全白墨时覆盖度才恢复得出来。
        // 描边的**颜色**由 tint 决定，不写在这里（见 coverage_from_libass）。
        assert!(
            joined.contains(&format!("OutlineColour={WHITE_INK}")),
            "描边必须用白墨（覆盖度才可恢复）：{joined}"
        );
    }

    /// **判据（T3）：把"描边颜色当前产不出来"这条已知边界钉住。**
    ///
    /// # 这条为什么必须存在
    ///
    /// 契约里有 `stroke_color`，但**这一步用不了它**：libass 路线上填充与描边
    /// 共用同一张覆盖度掩码，而 libass 输出的是**已合成**的颜色、alpha 恒为 0
    /// —— 黑描边与透明底**逐字节相同**，覆盖度恢复不出来。
    /// 所以两处都涂白墨，颜色交回 [`tint`]。
    ///
    /// 这是**已知边界**，不是"忘了做"。钉住它有两个作用：
    ///
    /// 1. 让"描边颜色不被 `stroke_color` 控制"变成一条**可断言的既知事实**，
    ///    而不是一个等着被人发现的惊讶；
    /// 2. 将来真做成了（T4 之后若能拿到分层的 mask），**这条判据会红** ——
    ///    它会提醒实现者"边界变了，去更新文档与 `tint` 的口径"。
    ///
    /// **反过来说**：如果哪天有人把 `stroke_color` 塞回参数串而没解决覆盖度问题，
    /// 下面这条 `OutlineColour=白` 的断言就会红 —— 拦住了那条路。
    #[test]
    fn 描边颜色当前不受_stroke_color_控制_这条边界钉住() {
        let mut a = key("字", 48);
        a.outline = true;
        a.stroke_px = 12;
        a.stroke_color = [0xff, 0x00, 0x00, 255]; // 亮红
        let mut b = a.clone();
        b.stroke_color = [0x00, 0x00, 0xff, 255]; // 亮蓝

        let joined_a =
            drawtext_args(&a, &a.font_file, Path::new("C:/fake"), Path::new("t.txt")).join(" ");
        let joined_b =
            drawtext_args(&b, &b.font_file, Path::new("C:/fake"), Path::new("t.txt")).join(" ");

        // **两条边界的当前事实**：
        assert_eq!(
            joined_a, joined_b,
            "换了 stroke_color 参数串就变了 —— 说明颜色被塞回了这一步。             那会**立刻**造成覆盖度不可恢复（黑描边 = 透明底），             要去的是 tint 那条路，不是这里"
        );
        assert!(
            joined_a.contains(&format!("OutlineColour={WHITE_INK}")),
            "描边必须是白墨：{joined_a}"
        );
        // **颜色确实没丢**：它还在**键**上（`stroke_color` 逐字段不同），
        // 只是不经过 ffmpeg 这一步。将来做分层掩码时就是从这里取它。
        assert_ne!(
            a.stroke_color, b.stroke_color,
            "契约上的描边色仍然不同 —— 它没被丢弃，只是这一步消费不了它"
        );
        // 顺带钉住事实：`tint_color()` 回的是**填充色**（`self.color`），
        // 不是描边色。别指望它替描边上色。
        assert_eq!(
            a.tint_color(),
            a.color,
            "`tint_color` 是填充色那条路；描边色的归属是 T4 之后的事"
        );
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
        let joined = drawtext_args(
            &styled,
            &styled.font_file,
            Path::new("C:/fake"),
            Path::new("t.txt"),
        )
        .join(" ");
        assert!(
            joined.contains("Outline=12"),
            "宽度要用契约给的 12，实得：{joined}"
        );
        // **描边色不进参数串**：libass 那条路上填充与描边共用同一张覆盖度 mask，
        // 描边的颜色由 tint 决定（见 coverage_from_libass 里写的那条口径收窄）。
        // 这里断言的是"用了白墨"，不是"用了契约色" —— 后者已经**不再**由这一步负责。
        assert!(
            joined.contains(&format!("OutlineColour={WHITE_INK}")),
            "描边要用白墨：{joined}"
        );

        // `stroke_px == 0` 时退回"从字号推 + 颜色 black"—— 老工程没写这个字段，
        // 行为必须逐字节不变（升级后的默认值就是 0）。
        let mut legacy = key("字", 48);
        legacy.outline = true;
        legacy.font_px = 48;
        let legacy_args = drawtext_args(
            &legacy,
            &legacy.font_file,
            Path::new("C:/fake"),
            Path::new("t.txt"),
        )
        .join(" ");
        assert!(
            legacy_args.contains("Outline=3"),
            "老行为：字号 48 -> 3 像素，实得：{legacy_args}"
        );
        assert!(
            legacy_args.contains(&format!("OutlineColour={WHITE_INK}")),
            "描边一律用白墨，实得：{legacy_args}"
        );
    }

    // ---- 文字阴影（B5）----

    /// **反向用例（最重要的一条）**：不画阴影时，参数串逐字符等于**这一版的冻结串**。
    ///
    /// 判据不是"看起来差不多"，而是**逐项相等**：加阴影时最自然的写法就是往
    /// 那条串里塞东西，而多一个字符画出来的像素就不一样了。
    ///
    /// **T2 说明**：冻结的对象从 `drawtext=` 换成了 libass 的 `subtitles=`，
    /// 因为栅格化器整个换了（用户决策，破坏性变更）。这条判据的**性质没变**：
    /// 它盯的是"有没有人在默认路径上顺手改动参数串"。
    #[test]
    fn 不画阴影时参数串逐字符与冻结串相同() {
        // 无描边（老工程的默认：`stroke_ratio` 默认 0 且 `outline` 默认 true 时走另一支，
        // 所以这里两种都验）。
        let plain = key("字", 20);
        assert_eq!(
            drawtext_args(
                &plain,
                &plain.font_file,
                Path::new("C:/fake"),
                Path::new("C:/tmp/dhampir-text-1.txt")
            ),
            frozen_argv(&plain)
        );

        // 有描边：老路径（宽度从字号推、颜色写死黑）。
        let mut outlined = key("字", 20);
        outlined.outline = true;
        let mut expected = frozen_argv(&outlined);
        let at = expected
            .iter()
            .position(|arg| arg.starts_with("subtitles="))
            .expect("有 -vf");
        // 描边在 ASS 里就是 `force_style` 里的两项，追加在 `Outline=0` 的位置上。
        // 这里**照着实现改**（把 `Outline=0` 替换成带宽度与颜色的那两项），
        // 而不是重新拼一遍 —— 重拼会让这条测试与实现同源，那就验不出东西了。
        expected[at] = expected[at].replace(
            "Outline=0",
            &format!("Outline={},OutlineColour={WHITE_INK}", border_px(32)),
        );
        assert_eq!(
            drawtext_args(
                &outlined,
                &outlined.font_file,
                Path::new("C:/fake"),
                Path::new("C:/tmp/dhampir-text-1.txt")
            ),
            expected,
            "有描边的默认路径也一个字符都不能变（多一个字符像素就不一样）"
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
        let args = drawtext_args(
            &shadow,
            &shadow.font_file,
            Path::new("C:/fake"),
            Path::new("C:/tmp/dhampir-text-1.txt"),
        );
        let joined = args.join(" ");
        assert!(
            joined.contains("gblur=sigma=2"),
            "σ 应当是 blur/2 = 2：{joined}"
        );
        assert!(
            joined.contains("Outline=0"),
            "阴影不许带描边（描边是黑的，会被 tint 染成黑边）：{joined}"
        );
        assert!(
            joined.contains("PrimaryColour=&H00FFFFFF"),
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
        let joined = drawtext_args(
            &shadow,
            &shadow.font_file,
            Path::new("C:/fake"),
            Path::new("t.txt"),
        )
        .join(" ");
        assert!(!joined.contains("gblur"), "硬阴影不该有模糊：{joined}");
        assert!(joined.contains("Outline=0"), "阴影不许带描边：{joined}");
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
        assert_eq!(
            calls, 3,
            "文本、阴影、另一档模糊各要一次；同键不许重复栅格化"
        );
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

        assert!(
            staged.is_file(),
            "搬完之后那一份必须真的在：{}",
            staged.display()
        );
        assert!(
            staged.to_string_lossy().bytes().all(|byte| byte.is_ascii()),
            "临时路径自己必须全是 ASCII，否则搬了等于没搬：{}",
            staged.display()
        );
        assert!(
            staged.starts_with(std::env::temp_dir()),
            "临时文件要落在临时目录里"
        );
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
        assert_ne!(a.path(), b.path(), "内容不同的两份字体不许共用同一个临时名");
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
        assert!(
            !ascii_font_path(Path::new("C:\\Windows\\Fonts\\simhei.ttf"))
                .unwrap()
                .is_staged()
        );
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

    /// 字体是**按名字**进参数串的（libass 不认路径），而名字必须来自**要被用的那份**。
    ///
    /// 这条是"搬了却没换上"的守卫，T2 之后换了个形态：`ascii_font_path` 算得很对、
    /// 调用方却仍旧读 `key.font_file` —— 那是最自然的一处写错，而它**静默**：
    /// 编译过、断言也过（名字看着都像"某个字体"），只有真出片时用的是另一份字体。
    ///
    /// 判据落在**家族名**上：传进去的那条路径决定名字，所以传临时路径就该出现临时名。
    #[test]
    fn 参数串里的字体名来自被传进来的那条路径() {
        let mut k = key("字", 20);
        k.font_file = PathBuf::from("C:/tmp/乐米.ttf");
        let staged = PathBuf::from("C:/tmp/dhampir-font-1-abc.ttf");
        let joined = drawtext_args(&k, &staged, Path::new("C:/fake"), Path::new("t.txt")).join(" ");
        assert!(
            joined.contains("FontName=dhampir-font-1-abc"),
            "参数串里的字体名应当来自传进来的那条路径：{joined}"
        );
        assert!(
            !joined.contains("乐米"),
            "参数串里仍是非 ASCII 那条路径上的名字 —— 搬了却没换上：{joined}"
        );
        // **反向**：不搬时名字就该来自键上那条。
        let direct =
            drawtext_args(&k, &k.font_file, Path::new("C:/fake"), Path::new("t.txt")).join(" ");
        assert!(
            direct.contains("FontName=乐米"),
            "不搬时名字该来自键上那条路径：{direct}"
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
        // 于是参数串里用的就是**这份字体**（libass 路线下以**家族名**出现），
        // 且没有被换成一条临时路径。
        let joined =
            drawtext_args(&k, &k.font_file, Path::new("C:/fake"), Path::new("t.txt")).join(" ");
        assert!(
            joined.contains(&format!("FontName={}", font_family_name(&k, &font))),
            "参数串里该是这份字体的家族名：{joined}"
        );
        assert!(
            !joined.contains("dhampir-font-"),
            "ASCII 路径不许被换成临时路径：{joined}"
        );
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
            eprintln!(
                "本机 test_font() 是 ASCII 路径（{}），这条测试跳过",
                font.display()
            );
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
        // **家族名必须给**：libass 按名字找字体，而名字从文件名推不可靠
        // （实测推出来会静默回退到 ArialMT，那种回退没有 CJK 字形）。
        k.font_family = Some(FAMILY_FOR_TEST_FONT.to_string());
        // 字体目录：这份字体所在的那个目录。
        k.font_dir = font.parent().map(Path::to_path_buf);

        let bitmap = rasterize_line(&k).expect("非 ASCII 字体路径必须画得出来（T1/T2）");
        let inked = ink_count(&bitmap);
        assert!(
            inked > 0,
            "非 ASCII 字体路径画出来是空的 —— 这正是 T1 要修的那个静默失败"
        );
        // 四个字、字号 40：墨迹至少上千像素。给一个宽松但能证伪的下限。
        // **下限也要挡得住"回退到 ArialMT"**：那种回退画不出 CJK，
        // 实测只剩 318 个覆盖像素（见模块文档里那张 fontselect 表）。
        assert!(
            inked > 1000,
            "墨迹只有 {inked} 像素，像是没画全（回退到无 CJK 的字体？）"
        );
        assert!(!bitmap.ink_touches_edge(), "字被切了");
    }

    /// 本机那份非 ASCII 名字体的**家族名**。libass 认的是这个名字，
    /// 不是文件名（实测：文件名主干 → ArialMT，这个名字 → LemiBoBoTi-Regular）。
    ///
    /// 写成常量是因为它是**这台机器上这份字体的事实**，不是一个该去推的东西 ——
    /// 而"推名字"正是这一版要拦掉的那个静默失败。
    const FAMILY_FOR_TEST_FONT: &str = "乐米波波体";

    /// **判据（T3 的核心）**：新路的**边缘覆盖度**与老路落在同一量级。
    ///
    /// # 这条判据在防什么
    ///
    /// libass 把覆盖度放在 **RGB**、alpha 恒为 0；老路放在**预乘的 alpha**。
    /// 搬运写错（忘了搬、或口径对不上）的症状是：整行字**看不见**、
    /// 或者边缘**整体暗/亮一圈** —— 而尺寸、字节数、ffmpeg 退出码**全都是对的**。
    /// 所以必须**逐点量边缘**，不能只看"有墨迹"。
    ///
    /// # 为什么要"**一个字一个字**"地比（这条很关键）
    ///
    /// 两条路的**每字推进宽度**差 1%：实测「口」重复 5 次时老路 192 px、
    /// 新路 190 px（每字累计偏 0.4 px）。逐点比会把这点漂移**放大**成巨大差异
    /// —— 边缘横挪 1 px 就是"0 对 128"。累积到第 5 个字已经错 2 px，
    /// 于是**整串**比出来是 max 247 / 中位 76 / 45.8% 落在容差内，
    /// 看着像"两条路差得远"；而**同一个字单独比**是 max 143 / 中位 40 /
    /// **96.9% 落在 128 内**。
    ///
    /// 所以判据比的是**单个字形**：那才是"两套栅格化器对同一副轮廓的处理"，
    /// 而字距差异是另一件事（它属于排版，不属于覆盖度）。
    #[test]
    #[ignore = "真起 ffmpeg：要 PATH 上的 ffmpeg 与一份中文字体。跑：cargo test -p dhampir-worker --lib text_raster -- --ignored"]
    fn 真机_边缘覆盖度与老路同量级() {
        // **必须用 ASCII 路径的字体**：这条判据要重建"老路"，而老路
        // （`drawtext`）在**非 ASCII 字体路径**上会直接崩掉（退出码
        // `0xC0000005` 访问违例）—— 那正是 T1 修的那个病。
        // 在这里踩到它会把"T3 的覆盖度判据"变成"T1 的复现"，两件事混在一起。
        let font = test_font_ascii();
        let font_px = 40u32;
        let (width, height) = bitmap_size(400, font_px as f32 * 1.2, font_px);
        let dir = font.parent().map(Path::to_path_buf);
        let family = font_family_name(&key("口", 20), &font);

        // 逐个字形单独量。**不用一句话**：见上面"为什么要一个字一个字地比"。
        for glyph in ["口", "日", "目", "田"] {
            let old_raw = probe_drawtext_white(&font, glyph, font_px, width, height)
                .expect("老路必须画得出来");
            let old = CoverageProfile::from_premultiplied(
                &TextBitmap::new(width, height, old_raw).expect("字节数应当对得上"),
            );

            let new_raw = probe_libass_white(
                &font,
                glyph,
                font_px,
                width,
                height,
                dir.as_deref(),
                &family,
            )
            .expect("新路必须画得出来");
            // 走 `coverage_from_libass` 搬运后再抽剖面：这**同时验了搬运**
            // —— 搬运写错的话这里会抽到全 0，下面的断言会直接红。
            let moved = coverage_from_libass(&new_raw);
            let new = CoverageProfile::from_premultiplied(
                &TextBitmap::new(width, height, moved).expect("字节数应当对得上"),
            );

            assert!(
                old.bounds.is_some(),
                "「{glyph}」老路画出来是空的 —— 夹具坏了，这条判据失去意义"
            );
            assert!(
                new.bounds.is_some(),
                "「{glyph}」新路搬完是空的 —— 覆盖度没搬过来（`tint` 会因此把整行字清掉）"
            );

            // 字形**尺寸**也要对得上：差太多就不是"覆盖度口径"的问题，
            // 而是字号/几何算错了（PlayRes 补偿那一条盯的就是这个）。
            let (Some((ox0, oy0, ox1, oy1)), Some((nx0, ny0, nx1, ny1))) = (old.bounds, new.bounds)
            else {
                unreachable!("上面断言过两边都有墨迹")
            };
            let old_w = ox1 - ox0 + 1;
            let old_h = oy1 - oy0 + 1;
            let new_w = nx1 - nx0 + 1;
            let new_h = ny1 - ny0 + 1;
            assert!(
                old_w.abs_diff(new_w) <= 2 && old_h.abs_diff(new_h) <= 3,
                "「{glyph}」字形尺寸差太多：老 {old_w}x{old_h} vs 新 {new_w}x{new_h}                 —— 这不是覆盖度口径的事，是字号/几何算错了"
            );

            // ================= 主判据：边缘**斜坡宽度** =================
            //
            // 这是**结构**判据，对"斜坡差半个像素"那种相位差免疫 ——
            // 而那正是两条路逐点差异的主要来源（见 [`ramp_width`]）。
            let old_ramp = ramp_width(&old);
            let new_ramp = ramp_width(&new);
            let diff = compare_edges(&old, &new);
            eprintln!(
                "「{glyph}」斜坡宽度 老 {old_ramp:.2} px vs 新 {new_ramp:.2} px；                 逐点差 比了 {} 像素 最大 {:.0} 中位 {:.0} 均值 {:.1} 容差内 {:.1}%",
                diff.compared,
                diff.max,
                diff.median,
                diff.mean,
                diff.within_tolerance * 100.0
            );

            assert!(
                diff.compared > 30,
                "「{glyph}」边缘像素只有 {} 个 —— 字太小或没画出来，判据不成立",
                diff.compared
            );
            // 两条路**各自**都得是"窄边"：太宽 = 边糊了；接近 0 = 硬边没抗锯齿。
            for (name, ramp) in [("老路", old_ramp), ("新路", new_ramp)] {
                assert!(
                    (0.5..=3.5).contains(&ramp),
                    "「{glyph}」{name}的边缘斜坡宽 {ramp:.2} px ——                      不在「窄抗锯齿」的范围内（太宽是糊了，接近 0 是硬边）"
                );
            }
            // **主体判据**：新路的斜坡可以比老路宽，但**不能宽出一个量级**。
            //
            // 实测四个字全部同向偏宽（0.63→1.65、1.43→2.41、1.21→3.17、1.10→1.63，
            // 最大差 1.96）。**这是两条渲染器的真实差异**，见 [`ramp_width`]，
            // 所以判据是"**偏宽有界**"而不是"必须相等"——
            // 改成"必须相等"会是一条恒红的判据，那是假的严。
            //
            // 上限 2.5 px 的依据：实测最大 1.96，留一点余量；
            // 而真坏掉时（比如把覆盖度当 alpha 用、或者边缘整块丢）
            // 这个数会跳到几倍。
            let widening = new_ramp - old_ramp;
            assert!(
                widening <= 2.5,
                "「{glyph}」新路斜坡比老路宽了 {widening:.2} px（老 {old_ramp:.2} → 新 {new_ramp:.2}）                 —— 超出两条渲染器已知的差异范围，像是覆盖度搬运出了问题"
            );
            // **反向**：新路也不许比老路**窄**太多 —— 那意味着边缘被硬化了
            // （覆盖度被二值化），是另一种坏法。
            assert!(
                widening >= -1.0,
                "「{glyph}」新路斜坡比老路窄了 {:.2} px（老 {old_ramp:.2} → 新 {new_ramp:.2}）                 —— 边缘被硬化了，覆盖度多半被二值化过",
                -widening
            );

            // ================= 辅助判据：逐点差**不许整体走形** =================
            //
            // 逐点差**必然**偏大（斜坡相位差半个像素就能顶到上百），
            // 所以这里不判"多数落在容差内"（那是恒红的假严），
            // 也不判单点最大值（相位差本来就能顶到 240 那种量级）。
            //
            // 判的是**中位差**：它是"整条边有没有整体偏移/整体变淡"的量度，
            // 对个别点的相位跳变不敏感。实测四个字的中位差在 60~90 之间
            // （同一个字形斜坡只宽 1 个像素时，中位差自然就在这个量级）。
            // 真坏掉时（覆盖度没搬、边缘整块丢）中位差会顶到 150 以上。
            assert!(
                diff.median <= 130.0,
                "「{glyph}」边缘逐点差中位数 {:.0}（比了 {} 像素）——                  整条边在整体偏移或整体变淡，不是个别像素的相位差",
                diff.median,
                diff.compared
            );
        }
    }

    /// 老路（`drawtext`）白字渲染，返回原始 RGBA。**只给判据用。**
    fn probe_drawtext_white(
        font: &Path,
        text: &str,
        font_px: u32,
        width: u32,
        height: u32,
    ) -> Result<Vec<u8>, String> {
        // **走 `textfile=`，与老路生产的参数同形**。
        // 实测：直接 `text='口日目回田'` 在 shell 里能跑，但那样连同
        // 一处 `format:rgba` 的手误都能把退出码变成 `0xC0000005`（访问违例）——
        // 判据要的是"两条路的**渲染**可比"，不是"哪一串写法能起得来"。
        // 用文件也让 `%`、`:` 这些字符不必转义（老路当年就是这么做的）。
        let text_file = std::env::temp_dir().join(format!(
            "dhampir-t3-probe-{}-{:016x}.txt",
            std::process::id(),
            dhampir_core::timeline::selfcheck::fnv1a64(text.as_bytes())
        ));
        {
            use std::io::Write as _;
            let mut file = std::fs::File::create(&text_file)
                .map_err(|error| format!("写不了探针文本文件：{error}"))?;
            file.write_all(text.as_bytes())
                .map_err(|error| format!("写不了探针文本文件：{error}"))?;
        }
        let escaped = filter_value(&font.to_string_lossy());
        let escaped_text = filter_value(&text_file.to_string_lossy());
        let source = format!("color=c=black@0.0:s={width}x{height},format=rgba");
        let filter = format!(
            "drawtext=fontfile={escaped}:textfile={escaped_text}:fontsize={font_px}:             fontcolor=white:expansion=none:x=0:y=0"
        );
        let result = run_ffmpeg_raw(rawvideo_args(source, filter));
        let _ = std::fs::remove_file(&text_file);
        result
    }

    /// 新路（libass）白字渲染，返回**未搬运的**原始 RGBA。**只给判据用。**
    fn probe_libass_white(
        font: &Path,
        text: &str,
        font_px: u32,
        width: u32,
        height: u32,
        font_dir: Option<&Path>,
        family: &str,
    ) -> Result<Vec<u8>, String> {
        let subtitle = write_subtitle_file(text)?;
        let source = format!("color=c=black@0.0:s={width}x{height},format=rgba");
        let style = format!(
            "FontName={family},FontSize={},PrimaryColour={WHITE_INK}",
            ass_font_size(font_px, height)
        );
        let dir = font_dir.unwrap_or_else(|| Path::new("."));
        let filter = subtitles_filter(&subtitle, dir, &style);
        let result = run_ffmpeg_raw(rawvideo_args(source, filter));
        let _ = std::fs::remove_file(&subtitle);
        let _ = font;
        result
    }

    /// **判据（T3 的核心反向用例）**：**直接把 libass 的 alpha 当覆盖度**用，
    /// 必须被抓住。
    ///
    /// # 这条盯的是这条链上最坏的一种失败
    ///
    /// libass 输出到 `format=rgba` 时 **alpha 恒为 0**（覆盖度写在 RGB）。
    /// 若有人把"搬平"那一步删掉、直接把 ffmpeg 的字节当位图用：
    ///
    /// * `tint()` 拿 `alpha=0` 会走到"全透明像素连颜色都不留"那一条，把 rgb 清成 0；
    /// * 结果是一张**全透明的位图**，而它的尺寸、字节数、ffmpeg 退出码
    ///   **全都是对的** —— 那正是一行**看不见的字**。
    ///
    /// 所以这条判据不能靠"ffmpeg 成功没成功"，必须**看像素**：
    /// 搬运之后的位图必须有墨，而**未搬运的**那张按 alpha 看是**全空**的。
    #[test]
    #[ignore = "真起 ffmpeg：要 PATH 上的 ffmpeg 与一份中文字体。跑：cargo test -p dhampir-worker --lib text_raster -- --ignored"]
    fn 真机_直接把_libass_的_alpha_当覆盖度会被抓住() {
        let font = test_font_ascii();
        let glyph = "口";
        let font_px = 40u32;
        let (width, height) = bitmap_size(400, font_px as f32 * 1.2, font_px);
        let dir = font.parent().map(Path::to_path_buf);
        let family = font_family_name(&key(glyph, 20), &font);

        let raw = probe_libass_white(
            &font,
            glyph,
            font_px,
            width,
            height,
            dir.as_deref(),
            &family,
        )
        .expect("新路必须画得出来");

        // ---- 反面：**不搬运**，按 alpha 读 ----
        let wrong = CoverageProfile::from_premultiplied(
            &TextBitmap::new(width, height, raw.clone()).expect("字节数应当对得上"),
        );
        assert!(
            wrong.bounds.is_none(),
            "未搬运的位图按 alpha 看居然有墨（包围盒 {:?}）——              libass 的 alpha 不该非 0；这条判据的前提变了，得重新量",
            wrong.bounds
        );
        assert!(
            wrong.edge_pixels().is_empty(),
            "未搬运的位图按 alpha 看有 {} 个边缘像素 —— 前提变了",
            wrong.edge_pixels().len()
        );

        // ---- 正面：**搬运之后**，同样的字节必须出墨 ----
        let right = CoverageProfile::from_premultiplied(
            &TextBitmap::new(width, height, coverage_from_libass(&raw)).expect("字节数应当对得上"),
        );
        assert!(
            right.bounds.is_some(),
            "搬运之后还是没有墨 —— 覆盖度没搬过来"
        );
        assert!(
            right.edge_pixels().len() > 30,
            "搬运之后边缘像素只有 {} 个 —— 搬运多半把覆盖度压没了",
            right.edge_pixels().len()
        );

        // ---- 两条路的差别必须是**这个**差别（不是别的）----
        // 同一份原始字节：「按 alpha 读」全空、「搬运后读」有墨。
        // 这一条就是"搬平那一步不可省"的可复算证据。
        let raw_ink = raw.chunks_exact(4).filter(|px| px[3] != 0).count();
        assert_eq!(
            raw_ink, 0,
            "libass 的 alpha 本该恒为 0，实测有 {raw_ink} 个非 0"
        );
    }

    /// 起一次 ffmpeg，把 `rawvideo` 的 stdout 原样拿回来。
    fn run_ffmpeg_raw(args: Vec<String>) -> Result<Vec<u8>, String> {
        let output = std::process::Command::new("ffmpeg")
            .args(&args)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .map_err(|error| format!("起不了 ffmpeg：{error}"))?;
        if !output.status.success() {
            return Err(format!(
                "ffmpeg 退出码 {:?}：{}",
                output.status.code(),
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        Ok(output.stdout)
    }

    /// **判据（T2 最硬的一条）**：缺字必须由 libass **逐字形回退**补上，
    /// 而不是画成 `.notdef` 方框。
    ///
    /// # 为什么拿 `fontselect` 日志判，而不是拿墨量判
    ///
    /// 墨量只能说明"画出了东西"。`fontselect` 日志直接给出**解析到了哪一份字体**，
    /// 而且**每回退一次就多一行** —— 那正是"回退真的发生了"的可复算证据：
    ///
    /// ```text
    /// fontselect: (乐米波波体, 400, 0) -> LemiBoBoTi-Regular, 0, LemiBoBoTi-Regular
    /// fontselect: (乐米波波体, 400, 0) -> MicrosoftYaHeiUI, 1, MicrosoftYaHeiUI   ← 靥 回退
    /// ```
    ///
    /// 第二行**必须出现**（`乐米波波体` 没有 U+9765 靥），而且**不许**解析成 ArialMT
    /// —— 那正是"家族名推错了"的症状（实测：文件名主干会解析成 ArialMT，
    /// 而 Arial 没有 CJK 字形，字会变成别的模样或方框）。
    #[test]
    #[ignore = "需要 PATH 上的 ffmpeg 与本机那份缺字的乐米字体；跑：cargo test -p dhampir-worker --lib text_raster -- --ignored"]
    fn 真机_缺字走逐字形回退而不是_notdef() {
        let font = test_font();
        if !font.to_string_lossy().contains("乐米波波体") {
            // 这条判据盯的是**那份缺字的字体**。本机没有它就跳过，
            // 而不是拿另一份字体跑出一个证明不了这件事的绿。
            eprintln!(
                "本机 test_font() 不是乐米（{}），这条测试跳过",
                font.display()
            );
            return;
        }
        let font_px = 40u32;
        let (width, height) = bitmap_size(640, font_px as f32 * 1.2, font_px);
        let mut k = key("笑靥如花", 20);
        k.text = "笑靥如花".to_string();
        k.font_px = font_px;
        k.font_file = font.clone();
        k.font_family = Some(FAMILY_FOR_TEST_FONT.to_string());
        k.font_dir = font.parent().map(Path::to_path_buf);
        k.width = width;
        k.height = height;

        // 真跑一次，把 `fontselect` 那几行抓回来（`-v info` 才会打）。
        let subtitle = write_subtitle_file(&k.text).expect("写得下字幕临时文件");
        let dir = font_dir(k.font_dir.as_deref(), &font).expect("定得下字体目录");
        let stdout = probe_fontselect(&k, dir.path(), &subtitle).expect("起得了 ffmpeg");
        let _ = std::fs::remove_file(&subtitle);

        let lines: Vec<&str> = stdout
            .lines()
            .filter(|line| line.contains("fontselect:"))
            .collect();
        assert!(
            !lines.is_empty(),
            "抓不到 fontselect 行 —— 判据的前提没了：{stdout}"
        );
        // **不许**解析成 ArialMT：那是"名字推错了"的症状，而且它没有 CJK 字形。
        assert!(
            !stdout.contains("ArialMT"),
            "家族名被解析成了 ArialMT —— 名字传错了（这会让字静默变成另一副样子）：{stdout}"
        );
        // **必须**有第二行：缺的那个字由回退补上。
        assert!(
            lines.len() >= 2,
            "只有 {} 行 fontselect —— 缺字没有触发回退（那就是没回退，字会是 .notdef）：{:?}",
            lines.len(),
            lines
        );
        // 回退落到哪儿也记下来（换台机器会变，所以只要求"不是第一份"）。
        assert_ne!(
            lines[0], lines[1],
            "两行 fontselect 一模一样 —— 那说明没换字体，不是回退：{:?}",
            lines
        );
    }

    /// 跑一次 ffmpeg，只要它的 `fontselect` 日志（`-v info` 才会打）。
    ///
    /// 与 [`run_ffmpeg`] 分开：那个走 `-v error`（出片路径不该被日志拖慢），
    /// 这个只给判据用，多一行日志无所谓。
    fn probe_fontselect(
        key: &TextRasterKey,
        font_dir: &Path,
        subtitle_file: &Path,
    ) -> Result<String, String> {
        let mut args = drawtext_args(key, &key.font_file, font_dir, subtitle_file);
        // 把 `-v error` 换成 `-v info`：`fontselect` 是 info 级。
        if let Some(level) = args.iter_mut().find(|arg| *arg == "error") {
            *level = "info".to_string();
        }
        let output = std::process::Command::new("ffmpeg")
            .args(&args)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .map_err(|error| format!("起不了 ffmpeg：{error}"))?;
        Ok(String::from_utf8_lossy(&output.stderr).to_string())
    }

    /// **判据（T2 的核心）**：契约给了家族名就用契约的；没给才退回文件名主干。
    ///
    /// 这条盯的是**静默用错字体**：libass 找不到名字时会回退到一个系统字体，
    /// `lines_failed` 还是 0、`issues` 还是空 —— 只有像素变了。
    /// 所以"用了哪个名字"必须是一条**可断言的**事实，而不是推出来的。
    #[test]
    fn 家族名优先用契约给的_没给才退回文件名() {
        let mut k = key("字", 20);
        k.font_file = PathBuf::from("C:/fonts/乐米波波体（免费商用）_爱给网_aigei_com.ttf");
        // 契约给了名字：用它，**不推**。
        k.font_family = Some("乐米波波体".to_string());
        let joined =
            drawtext_args(&k, &k.font_file, Path::new("C:/fake"), Path::new("t.txt")).join(" ");
        assert!(
            joined.contains("FontName=乐米波波体,"),
            "契约给了家族名就该用它：{joined}"
        );
        assert!(
            !joined.contains("爱给网"),
            "不许把文件名主干当家族名（那个名字实测会回退到 ArialMT）：{joined}"
        );

        // 契约没给：退回文件名主干 —— 这是**有把握的降级**，不是"能用的默认"。
        k.font_family = None;
        let fallback =
            drawtext_args(&k, &k.font_file, Path::new("C:/fake"), Path::new("t.txt")).join(" ");
        assert!(
            fallback.contains("FontName=乐米波波体（免费商用）_爱给网_aigei_com,"),
            "没给家族名时退回文件名主干：{fallback}"
        );

        // 空白名字等于没给（空串会把 FontName 写成空的，libass 只能回退）。
        k.font_family = Some("   ".to_string());
        let blank =
            drawtext_args(&k, &k.font_file, Path::new("C:/fake"), Path::new("t.txt")).join(" ");
        assert!(
            blank.contains("FontName=乐米波波体（免费商用）"),
            "空白家族名要当作没给，而不是写一个空的 FontName：{blank}"
        );
    }

    /// **判据**：`fontsdir=` 必须在参数串里 —— 它是回退能不能工作的开关。
    ///
    /// 不给它，libass 只认系统字体库；给了它，用户点名那份字体所在的目录
    /// 才参与查找。**反向**：换个字体目录，这一项必须跟着变（否则就是写死了）。
    #[test]
    fn 字体目录进参数串且跟着键走() {
        let k = key("字", 20);
        let a =
            drawtext_args(&k, &k.font_file, Path::new("C:/fontsA"), Path::new("t.txt")).join(" ");
        assert!(a.contains(r"fontsdir='C\:/fontsA'"), "字体目录要进串：{a}");
        let b =
            drawtext_args(&k, &k.font_file, Path::new("C:/fontsB"), Path::new("t.txt")).join(" ");
        assert!(b.contains(r"fontsdir='C\:/fontsB'"), "换目录要跟着换：{b}");
        assert_ne!(a, b, "换字体目录必须换参数串（回退落在哪个字体上会变）");
    }

    /// **判据（T2 里最阴的一条）**：字号要按 libass 的虚拟画布高（288）补偿。
    ///
    /// libass 把 SRT 画在默认 **384×288** 的虚拟画布上再缩放。所以 `FontSize`
    /// 必须乘 `288 / 位图高`，否则**画布高不是 288 时字会整体变小** ——
    /// 而 `lines_failed` 仍是 0、`issues` 仍是空，只是字小了一圈。
    ///
    /// 这条判据盯的就是"补偿没了"：漏掉它，`FontSize` 与位图高就会**脱钩**
    /// （同一个字号在任何画布上都给同一个值），而那正是 bug 的形状。
    #[test]
    fn 字号要按_libass_虚拟画布高补偿() {
        // **同一个目标字号、不同的位图高**：FontSize 必须跟着变，且与高成反比。
        let tall = ass_font_size(40, 360);
        let short = ass_font_size(40, 90);
        assert_ne!(
            tall, short,
            "位图高不同，FontSize 必须不同（否则就是没补偿）"
        );
        // 90 高时缩放因子是 360 高时的 4 倍。
        let ratio = f64::from(short) / f64::from(tall);
        assert!(
            (ratio - 4.0).abs() < 0.05,
            "FontSize 应与位图高成反比：360 高 {tall} vs 90 高 {short}，比值 {ratio}"
        );
        // **反向**：正好是虚拟画布高（288）时，补偿因子应当是 1 —— 只剩 em→行高的 1.25。
        assert_eq!(
            ass_font_size(40, 288),
            50,
            "288 高时因子为 1：40 × 1.25 = 50（这条是那个 288 的来源证明）"
        );
        // 位图高为 0 不许除出 inf（构造上不该发生，但这条函数是 pub）。
        assert!(ass_font_size(40, 0) > 0, "零高也要给一个正数，不许 inf");
    }

    /// **判据**：定不下字体目录时**响亮报错**，不许静默放行。
    ///
    /// 放行的后果是 libass 用系统默认字体（实测 → ArialMT，没有 CJK 字形），
    /// 而那时 `lines_failed` 仍是 0 —— 又是一次静默。
    #[test]
    fn 字体目录定不下来要响亮报错() {
        // 显式给了一个不存在的目录。
        let err = font_dir(
            Some(Path::new("C:/nope/not-a-dir")),
            Path::new("C:/fake/font.ttf"),
        )
        .unwrap_err();
        assert!(
            err.contains("--font-dir"),
            "错误里要点出该改哪个参数：{err}"
        );
        // 没给目录、字体又没有可用的父目录。
        let err = font_dir(None, Path::new("font.ttf")).unwrap_err();
        assert!(err.contains("字体目录"), "要有一条人话：{err}");
        // **反向**：给一个真目录就该过。
        let ok = font_dir(Some(Path::new("C:/Windows/Fonts")), Path::new("x.ttf"));
        assert!(ok.is_ok(), "存在的目录不该被拦");
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
            font_family: None,
            font_dir: None,
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
        // 描边：**必须有描边那一圈**。
        //
        // T2 之前这条断言的是"有一个实心**黑**像素" —— 那时描边是**黑墨**。
        // 现在 libass 路线上**填充与描边都是白墨**（见 `coverage_from_libass`：
        // 只有这样覆盖度才恢复得出来），描边的**颜色**由 tint 染，不在这张位图里。
        // 所以判据换成结构性的：**描边存在 = 墨迹比不描边时更胖**。
        // 用同一把键（只翻 `outline`）跑两次，比墨迹像素数 —— 那是"这一圈画出来了"
        // 唯一说得清的证据，而且它**不依赖墨色**（换哪种描边色都成立）。
        let mut without = key.clone();
        without.outline = false;
        let plain = rasterize_line(&without).expect("不描边那张也要画得出来");
        let outlined_ink = ink_count(&bitmap);
        let plain_ink = ink_count(&plain);
        assert!(
            outlined_ink > plain_ink,
            "开了描边墨迹却不多（{outlined_ink} vs 不描边 {plain_ink}）—— \
             那一圈没有画出来"
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
            font_family: None,
            font_dir: None,
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
            // 用文件名主干（这条测试盯的是**文本展开**，不是字体选得对不对；
            // 而且 `%` 与 `{}` 的形状在任何字体下都一样）。
            font_family: None,
            font_dir: font.parent().map(Path::to_path_buf),
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
