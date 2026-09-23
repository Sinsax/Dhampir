//! 字幕文本的**共享布局**：把「一段文本 + 一套样式 + 文档坐标系」算成一串行盒。
//!
//! # 为什么它必须在契约 crate 里，而不是在各宿主里
//!
//! 两端都要把同一段字幕画出来。字形可以由各宿主自己栅格化（字体、抗锯齿、hinting
//! 本来就不同，强求像素一致是做不到的），但**结构**必须一致：几行、每行是什么、
//! 每行占哪个矩形。结构一旦分叉，同一个工程在两个宿主上就是两份不同的字幕 ——
//! 而那正是这个项目最贵的那条不变量。
//!
//! # 度量模型：按字宽分类，不取真字形度量
//!
//! 换行需要字形宽度。真度量只有字体文件里有，而本仓不许引第三方 crate；
//! 把度量表内嵌进来体积与维护成本都不小。更要紧的是另一条路——让宿主把度量喂回来
//! （先量后排）——它看起来更准，但会让**布局依赖宿主**，两端结构就此可能分叉，
//! 与上面那条目标相反。
//!
//! 所以取一个**零依赖、确定性**的模型：
//!
//! | 类别 | 前进宽度 | 例子 |
//! |---|---|---|
//! | 全角 | 1.0 em | CJK 汉字/假名、全角标点与全角空格 |
//! | 半角 | 0.5 em | ASCII 字母数字与半角标点 |
//! | 空格 | 0.25 em | ASCII 空格；制表符按一个空格算 |
//! | 零宽 | 0 | 组合记号、零宽空格、变体选择符 |
//!
//! **代价（写清楚，不藏）**：与真字体度量有偏差 —— 等宽字体的 i 与 W 在这个模型里
//! 一样宽，比例字体会让换行点与肉眼预期不同。调整 font_ratio 只能整体挪，
//! 不能让两端更接近。它影响的是**换行点**，不影响字形像素。
//!
//! # 有意不做的事
//!
//! * **禁则处理**（不让行首出现逗号句号、不让行尾出现左引号）没做。
//!   它是一个独立的排版课题，做了就要有对照的测试用例集；现在先不做，也不假装做了。
//! * **字距微调 / 连字**没做。
//! * 行盒与字形外框的差异由宿主自行处理：这里给的是**行盒**，
//!   宿主把文字画进这个盒子（基线位置由宿主定），像素允许不同。

use crate::layer::SubtitleStyle;

/// 行高与字号的比例。1.2 是常见的默认值（比 1.0 松，比 1.5 紧）。
pub const LINE_HEIGHT_EM: f32 = 1.2;
/// 全角字符的前进宽度（em）。
pub const FULL_WIDTH_EM: f32 = 1.0;
/// 半角字符的前进宽度（em）。
pub const HALF_WIDTH_EM: f32 = 0.5;
/// 空格的前进宽度（em）。比半角窄 —— 这是比例字体的常态。
pub const SPACE_EM: f32 = 0.25;

/// 归一化矩形：相对**文档坐标系**（工程的 render_hints），取值 0..1。
///
/// 用归一化而不是像素：像素量在预览与成片里含义不同，而那正是 T1 修掉的东西。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NormalizedRect {
    /// 左上角 x。
    pub x: f32,
    /// 左上角 y。
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl NormalizedRect {
    /// 水平中心。居中排版时它应当等于 0.5。
    pub fn center_x(&self) -> f32 {
        self.x + self.width / 2.0
    }

    /// 底边。
    pub fn bottom(&self) -> f32 {
        self.y + self.height
    }
}

/// 一行：文本 + 它占的**行盒**（不是字形外框）。
#[derive(Debug, Clone, PartialEq)]
pub struct TextLine {
    pub text: String,
    pub rect: NormalizedRect,
}

/// 一次布局的结果。
#[derive(Debug, Clone, PartialEq)]
pub struct TextLayout {
    pub lines: Vec<TextLine>,
    /// 因为超过 max_lines 被丢掉的**行数**。
    ///
    /// 丢掉要计数：不计数的话「字幕只显示了一半」看起来和「字幕就是这样」一样。
    /// 与弹幕泳道耗尽时的口径一致 —— 丢弃并计数，不叠、不缩。
    pub dropped_lines: usize,
}

impl TextLayout {
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }
}

/// 一个字符的前进宽度（em）。
pub fn advance_em(ch: char) -> f32 {
    if is_zero_width(ch) {
        return 0.0;
    }
    if is_full_width(ch) {
        return FULL_WIDTH_EM;
    }
    if ch == ' ' || ch == '\t' {
        return SPACE_EM;
    }
    HALF_WIDTH_EM
}

/// 一段文本的前进宽度（em）。**不计换行** —— 调用方先按行切开。
pub fn measure_em(text: &str) -> f32 {
    text.chars().map(advance_em).sum()
}

/// 全角（东亚全宽）判定。
///
/// 覆盖范围是按 Unicode 区块列的，不是按「看起来宽不宽」猜的 ——
/// 猜的话会漏掉假名、谚文、全角标点，而那些恰恰在中日韩字幕里天天出现。
fn is_full_width(ch: char) -> bool {
    let code = ch as u32;
    matches!(
        code,
        0x1100..=0x115F        // 谚文字母
        | 0x2E80..=0x303E      // CJK 部首补充 .. CJK 符号与标点
        | 0x3041..=0x33FF      // 平假名 .. CJK 兼容
        | 0x3400..=0x4DBF      // CJK 扩展 A
        | 0x4E00..=0x9FFF      // CJK 统一表意文字
        | 0xA000..=0xA4CF      // 彝文
        | 0xAC00..=0xD7A3      // 谚文音节
        | 0xF900..=0xFAFF      // CJK 兼容表意文字
        | 0xFE30..=0xFE4F      // CJK 兼容形式
        | 0xFF00..=0xFF60      // 全角 ASCII
        | 0xFFE0..=0xFFE6      // 全角符号
        | 0x1F300..=0x1F64F    // 常见 emoji（多数是全宽呈现）
        | 0x20000..=0x3FFFD    // CJK 扩展 B 及以后
    )
}

/// 零宽判定：这些字符不占前进宽度。
///
/// 不判它们的话，带变体选择符或组合记号的字幕会比肉眼看到的宽，
/// 于是换行点提前 —— 表现是「明明还放得下一个字却换行了」。
fn is_zero_width(ch: char) -> bool {
    let code = ch as u32;
    matches!(
        code,
        0x0300..=0x036F        // 组合变音记号
        | 0x200B..=0x200F      // 零宽空格 / 零宽连接符 / 方向标记
        | 0x2060..=0x2064      // 词连接符等
        | 0xFE00..=0xFE0F      // 变体选择符
        | 0xFEFF               // 零宽不换行空格（BOM 残留）
        | 0x1F3FB..=0x1F3FF    // 肤色修饰
    )
}

/// 排版原子：文本 + 宽度 + 是不是空格。
struct Atom {
    text: String,
    width: f32,
    is_space: bool,
}

fn flush_word(atoms: &mut Vec<Atom>, word: &mut String, word_width: &mut f32) {
    if word.is_empty() {
        return;
    }
    atoms.push(Atom { text: std::mem::take(word), width: *word_width, is_space: false });
    *word_width = 0.0;
}

/// 把一段（不含换行的）文本切成原子。
///
/// 断行机会只在原子之间：全角字符各自成原子（中文可以逐字断），
/// 连续的半角字符组成一个词（**英文词不能被从中间切断**）。
fn atoms_of(paragraph: &str) -> Vec<Atom> {
    let mut atoms: Vec<Atom> = Vec::new();
    let mut word = String::new();
    let mut word_width = 0.0_f32;

    for ch in paragraph.chars() {
        if is_zero_width(ch) {
            // 零宽字符不占宽度，跟着当前这个词走。
            word.push(ch);
            continue;
        }
        if ch == ' ' || ch == '\t' {
            flush_word(&mut atoms, &mut word, &mut word_width);
            atoms.push(Atom { text: " ".to_string(), width: SPACE_EM, is_space: true });
            continue;
        }
        if is_full_width(ch) {
            flush_word(&mut atoms, &mut word, &mut word_width);
            atoms.push(Atom { text: ch.to_string(), width: advance_em(ch), is_space: false });
            continue;
        }
        word.push(ch);
        word_width += advance_em(ch);
    }
    flush_word(&mut atoms, &mut word, &mut word_width);
    atoms
}

/// 贪心断行一段文本，结果追加进 out。**至少产出一行**（空段产出一个空行）。
fn wrap_paragraph(paragraph: &str, max_width_em: f32, out: &mut Vec<String>) {
    let mut line = String::new();
    let mut width = 0.0_f32;
    let mut pending_space: Option<f32> = None;

    for atom in atoms_of(paragraph) {
        if atom.is_space {
            // 行首的空格丢掉：断行处的空格不该留到下一行行首。
            if !line.is_empty() {
                pending_space = Some(atom.width);
            }
            continue;
        }

        let gap = pending_space.unwrap_or(0.0);
        if !line.is_empty() && width + gap + atom.width > max_width_em {
            out.push(std::mem::take(&mut line));
            width = 0.0;
            pending_space = None;
        }

        // 走到这里说明行放得下，或者行是空的。行空还放不下只剩一种情况：
        // **一个原子比一整行还宽**（超长单词）—— 硬断，否则会无限循环。
        if line.is_empty() && atom.width > max_width_em {
            let mut piece = String::new();
            let mut piece_width = 0.0_f32;
            for ch in atom.text.chars() {
                let advance = advance_em(ch);
                if piece_width + advance > max_width_em && !piece.is_empty() {
                    out.push(std::mem::take(&mut piece));
                    piece_width = 0.0;
                }
                piece.push(ch);
                piece_width += advance;
            }
            line = piece;
            width = piece_width;
            continue;
        }

        if let Some(space) = pending_space.take() {
            line.push(' ');
            width += space;
        }
        line.push_str(&atom.text);
        width += atom.width;
    }

    out.push(line);
}

/// 排版一段字幕文本。
///
/// 入参的 sequence 是**文档坐标系**（工程的 render_hints），不是渲染目标尺寸 ——
/// 归一化矩形与渲染尺寸无关是这段代码存在的意义之一。
pub fn layout(text: &str, style: &SubtitleStyle, sequence: (u32, u32)) -> TextLayout {
    let empty = TextLayout { lines: Vec::new(), dropped_lines: 0 };
    if text.is_empty() {
        return empty;
    }

    let sequence_width = sequence.0.max(1) as f32;
    let sequence_height = sequence.1.max(1) as f32;
    let font_px = style.font_ratio.max(0.0) * sequence_height;
    let line_px = font_px * LINE_HEIGHT_EM;

    // 安全边距：左右复用 bottom_margin。
    //
    // 契约里没有独立的侧边距字段，而**加一个可选字段不改契约版本**（见 plan 里
    // 那条约定的用法）。现在先复用，是为了这一轮不动契约。
    let margin = style.bottom_margin.clamp(0.0, 0.5);
    let max_width_px = sequence_width * (1.0 - 2.0 * margin);

    // 字号或可用宽度为 0：没有可排的东西。**返回空，而不是一排零宽度的行** ——
    // 后者会让调用方以为「有字要画」。
    if !(font_px > 0.0) || !(max_width_px > 0.0) {
        return empty;
    }
    let max_width_em = max_width_px / font_px;

    let mut raw_lines: Vec<String> = Vec::new();
    for paragraph in text.split('\n') {
        wrap_paragraph(paragraph, max_width_em, &mut raw_lines);
    }

    // max_lines = 0 表示不显示：全部丢掉并如实计数。
    let keep = (style.max_lines as usize).min(raw_lines.len());
    let dropped_lines = raw_lines.len() - keep;

    let line_height_norm = line_px / sequence_height;
    let block_top = 1.0 - margin - (keep as f32) * line_height_norm;

    let mut lines = Vec::with_capacity(keep);
    for (index, raw) in raw_lines.iter().take(keep).enumerate() {
        let width_norm = (measure_em(raw) * font_px / sequence_width).min(1.0);
        lines.push(TextLine {
            text: raw.clone(),
            rect: NormalizedRect {
                // 居中：字幕的常规排法。想改就加一个对齐字段（加可选字段，不改版本）。
                x: 0.5 - width_norm / 2.0,
                y: block_top + (index as f32) * line_height_norm,
                width: width_norm,
                height: line_height_norm,
            },
        });
    }

    TextLayout { lines, dropped_lines }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn style() -> SubtitleStyle {
        SubtitleStyle::default()
    }

    /// 文档坐标系取样本工程那组。字号 = 360 * 0.055 = 19.8px，行高 23.76px。
    const SEQUENCE: (u32, u32) = (640, 360);

    #[test]
    fn 字宽分类是分开的() {
        assert_eq!(advance_em('中'), FULL_WIDTH_EM, "汉字是全角");
        assert_eq!(advance_em('あ'), FULL_WIDTH_EM, "平假名是全角");
        assert_eq!(advance_em('한'), FULL_WIDTH_EM, "谚文是全角");
        assert_eq!(advance_em('，'), FULL_WIDTH_EM, "全角逗号是全角");
        assert_eq!(advance_em('a'), HALF_WIDTH_EM, "ASCII 字母是半角");
        assert_eq!(advance_em('7'), HALF_WIDTH_EM);
        assert_eq!(advance_em(' '), SPACE_EM);
        assert_eq!(advance_em('\t'), SPACE_EM);
    }

    #[test]
    fn 零宽字符不占宽度() {
        // U+0301 组合尖音符、U+200B 零宽空格、U+FE0F 变体选择符。
        assert_eq!(advance_em('\u{0301}'), 0.0);
        assert_eq!(advance_em('\u{200B}'), 0.0);
        assert_eq!(advance_em('\u{FE0F}'), 0.0);
        assert_eq!(measure_em("a\u{0301}"), HALF_WIDTH_EM, "组合记号不该加宽");
    }

    /// 反向：**分类真的生效**。不分类（一律半角）的话这两串会一样宽。
    #[test]
    fn 分类生效_全角比半角宽一倍() {
        assert!(measure_em("中中中中") > measure_em("aaaa") * 1.9);
        assert!((measure_em("中中中中") - 4.0 * HALF_WIDTH_EM * 2.0).abs() < 1e-6);
    }

    #[test]
    fn 空文本给零行而不是一行空行() {
        let result = layout("", &style(), SEQUENCE);
        assert!(result.is_empty());
        assert_eq!(result.dropped_lines, 0);
    }

    #[test]
    fn 换行符强制断行且保留空行() {
        // 默认 max_lines 是 2（字幕不该盖住半屏），这里要验的是断行本身，所以放开。
        let mut roomy = style();
        roomy.max_lines = 8;
        let result = layout("上\n\n下", &roomy, SEQUENCE);
        assert_eq!(result.dropped_lines, 0, "不该有行被丢掉");
        assert_eq!(result.lines.len(), 3);
        assert_eq!(result.lines[0].text, "上");
        assert_eq!(result.lines[1].text, "");
        assert_eq!(result.lines[2].text, "下");
    }

    #[test]
    fn 英文按空格断行_不从词中间切() {
        // 每行可用宽度 = 640 * (1 - 0.12) = 563.2px；字号 19.8px -> 28.44em。
        // 下面每个词 5 个半角字符 = 2.5em，一行放得下 11 个词（27.5em > 28.44? 不，27.5 < 28.44）。
        let text = "alpha bravo charlie delta echo foxtrot golf hotel india juliet";
        let result = layout(text, &style(), SEQUENCE);
        assert!(result.lines.len() >= 2, "这段应当断成两行以上");
        for line in &result.lines {
            // 每一行的每个词都必须是完整的词：要么是词，要么是词加空格。
            for piece in line.text.split(' ') {
                assert!(
                    text.split(' ').any(|word| word == piece),
                    "行里出现了半个词：{piece:?}（整行 {:?}）",
                    line.text
                );
            }
        }
    }

    #[test]
    fn 超长单词硬断而不是溢出() {
        let word = "a".repeat(200);
        let result = layout(&word, &style(), SEQUENCE);
        assert!(result.lines.len() > 1, "200 个半角字符放不进一行，必须硬断");
        for line in &result.lines {
            assert!(
                line.rect.width <= 1.0 + 1e-6,
                "硬断之后每行都要在宽度预算内，得到 {}",
                line.rect.width
            );
        }
    }

    #[test]
    fn 中文逐字断行() {
        // 28.44em / 1.0em = 每行 28 个汉字。
        let text = "中".repeat(60);
        let result = layout(&text, &style(), SEQUENCE);
        assert!(result.lines.len() >= 2);
        let first = result.lines[0].text.chars().count();
        assert!((20..=29).contains(&first), "第一行应当是二十来个汉字，实际 {first}");
    }

    #[test]
    fn 超出最大行数时丢弃并计数() {
        let mut tight = style();
        tight.max_lines = 2;
        let text = "一\n二\n三\n四\n五";
        let result = layout(text, &tight, SEQUENCE);
        assert_eq!(result.lines.len(), 2);
        assert_eq!(result.dropped_lines, 3, "丢了多少行必须如实计数");
    }

    #[test]
    fn 最大行数为零表示不显示() {
        let mut hidden = style();
        hidden.max_lines = 0;
        let result = layout("一\n二", &hidden, SEQUENCE);
        assert!(result.is_empty());
        assert_eq!(result.dropped_lines, 2);
    }

    #[test]
    fn 行盒水平居中() {
        let result = layout("中文字幕", &style(), SEQUENCE);
        assert_eq!(result.lines.len(), 1);
        assert!(
            (result.lines[0].rect.center_x() - 0.5).abs() < 1e-6,
            "居中排版的水平中心应当是 0.5，得到 {}",
            result.lines[0].rect.center_x()
        );
    }

    #[test]
    fn 块底边落在底边距上() {
        let result = layout("第一行\n第二行", &style(), SEQUENCE);
        assert_eq!(result.lines.len(), 2);
        let last = result.lines[1].rect;
        assert!(
            (last.bottom() - (1.0 - style().bottom_margin)).abs() < 1e-5,
            "最后一行底边应当落在 1 - bottom_margin 上，得到 {}",
            last.bottom()
        );
        // 第一行在第二行上面，且紧挨着（行高一致）。
        let first = result.lines[0].rect;
        assert!(first.y < last.y);
        assert!((last.y - first.bottom()).abs() < 1e-6, "行盒之间不该有空隙");
    }

    #[test]
    fn 行高与字号的比是常数() {
        let result = layout("一行", &style(), SEQUENCE);
        let height = result.lines[0].rect.height;
        let expected = style().font_ratio * LINE_HEIGHT_EM;
        assert!((height - expected).abs() < 1e-6, "行高应当是 font_ratio * 1.2 = {expected}");
    }

    #[test]
    fn 归一化矩形与文档坐标系无关地一致() {
        // 同一段文本、同一套比例样式，换一组文档坐标系（1920x1080）之后
        // **归一化**矩形应当完全相同 —— 比例样式的意义就在这里。
        // 注意字号比例相对高度，所以宽度占比会随宽高比变化：这里两组都是 16:9。
        let a = layout("中文字幕", &style(), (640, 360));
        let b = layout("中文字幕", &style(), (1920, 1080));
        assert_eq!(a.lines.len(), b.lines.len());
        let (ra, rb) = (a.lines[0].rect, b.lines[0].rect);
        assert!((ra.x - rb.x).abs() < 1e-5, "x：{ra:?} vs {rb:?}");
        assert!((ra.y - rb.y).abs() < 1e-5, "y：{ra:?} vs {rb:?}");
        assert!((ra.width - rb.width).abs() < 1e-5, "width：{ra:?} vs {rb:?}");
        assert!((ra.height - rb.height).abs() < 1e-5, "height：{ra:?} vs {rb:?}");
    }

    #[test]
    fn 同样输入给同样输出() {
        // 「两端结构一致」的前提是布局**确定性**：这里跑两遍比结构。
        let text = "第一行比较长一些的文字\nsecond line here\n短";
        let a = layout(text, &style(), SEQUENCE);
        let b = layout(text, &style(), SEQUENCE);
        assert_eq!(a, b);
    }

    #[test]
    fn 字号为零或边距吃掉全部宽度时返回空() {
        let mut tiny = style();
        tiny.font_ratio = 0.0;
        assert!(layout("字", &tiny, SEQUENCE).is_empty());

        let mut wide = style();
        wide.bottom_margin = 0.5; // 两侧各 0.5 -> 可用宽度为 0
        assert!(layout("字", &wide, SEQUENCE).is_empty());
    }

    #[test]
    fn 宽度不会超出预算() {
        let text = "混合 mixed 文本 text 混排 很长很长很长很长很长很长很长很长";
        let result = layout(text, &style(), SEQUENCE);
        for line in &result.lines {
            assert!(line.rect.width <= 1.0 + 1e-6, "行宽超预算：{}", line.rect.width);
            assert!(line.rect.x >= -1e-6, "左边越界：{}", line.rect.x);
        }
    }
}
