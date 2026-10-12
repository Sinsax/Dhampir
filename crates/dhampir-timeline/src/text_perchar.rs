//! **逐字格子**（P3，2026-10-13）—— 把一行文字拆成"每个字各自的盒子"，
//! 供下游做逐字位移 / 逐字缩放 / 逐字旋转 / 竖排。
//!
//! # 为什么必须新增这一层
//!
//! 在这之前，底座对一行的认知**只有整行一个盒子**（[`crate::text_layout::TextLine`]
//! 的 `rect` + `font_ratio`），下游拿不到"每个字在哪、多大、转多少"。
//! 实测：`per_char` / `perchar` / `tategaki` / `letter_spacing` 在本仓
//! **命中数全是 0** —— 也就是说「逐字错峰」「逐字变换」「竖排」这三族
//! （C/D/E 组共 21 档）**一个都做不了**，不是"没接上"，是"没有那个概念"。
//!
//! # 口径：说得清"每个字在哪"，就够了吗
//!
//! 不够。逐字变换至少要能表达四件事，而且**每件都要能逐字不同**：
//!
//! 1. **字在哪**（`advance_em` 累加出的行内偏移）—— 逐字错峰要按序号取时间；
//! 2. **字多大**（`font_px`，逐字可不同）—— 「逐字放大」；
//! 3. **字转多少**（`rotation_deg`，逐字可不同）—— 「逐字歪」；
//! 4. **字往哪挪**（`offset_x/y`，逐字可不同）—— 「散开」「沿弧线」。
//!
//! 所以 [`CharCell`] 把四件事都带上，**且默认全是恒等值** ⇒
//! 不配逐字效果时，下游走的仍是老路（逐字节不变，R4）。
//!
//! # ⚠️ 为什么格子住在这里（`dhampir-timeline`）而不是渲染侧
//!
//! 位置要用 [`advance_em`] 算，而那份**字宽表**（全角 / 半角 / 空格）
//! 是布局层的既有资产与既有契约。渲染侧再算一份 = 两处实现必然漂移，
//! 而症状是"竖排的字挤在一起"或"逐字散开的间距与预览不同"，**两边各自看都正常**。
//! 与 `text_layout::measure_em` 那条"行宽只有一份"同一条纪律。
//!
//! [`advance_em`]: crate::text_layout::advance_em

use crate::text_layout::advance_em;

/// 一个字的格子（**行内**局部坐标，单位是 em × `font_px` 之前的逻辑宽）。
///
/// ⚠️ 坐标原点是**这一行的左端**（不是画面中心）—— 居中的事由调用方做，
/// 因为只有它知道这一行要摆在画面的哪儿。这里只管"行内的第几个字在哪"。
#[derive(Debug, Clone, PartialEq)]
pub struct CharCell {
    /// 这个字（`char` 而不是 `String`：逐字效果都是单字粒度的，
    /// 而 `String` 会让"一个字"与"一个簇"混淆 —— emoji ZWJ 序列那种
    /// 目前按码位拆，与 `advance_em` 的口径一致，**两边必须是同一种拆法**）。
    pub ch: char,
    /// **行内左端的逻辑偏移**（em 单位，未乘 `font_px`）。
    ///
    /// 是这个字的**左边界**，不是中心。乘 `font_px` 得到目标像素。
    pub offset_em: f32,
    /// 这个字占的**逻辑宽度**（em）。`offset_em` 与它一起给出左右边界。
    pub advance_em: f32,
    /// **逐字缩放**（1.0 = 不缩）。默认 1.0。
    pub scale: f32,
    /// **逐字旋转**（顺时针角度制，与 [`crate::layer::TransformV2::rotation`] 同向）。
    /// 默认 0.0。
    pub rotation_deg: f32,
    /// **逐字位移**（em 单位，x 向右 / y 向下）。默认 0。
    ///
    /// ⚠️ 是 em 而**不是**像素：字号逐字不同时，像素单位的位移会让
    /// "同样挪 10px"在大字上看着没动、小字上跳很远。em 单位下观感一致。
    pub offset_x_em: f32,
    pub offset_y_em: f32,
}

impl CharCell {
    /// 恒等格子（只有位置与宽度，没有任何变换）。
    ///
    /// ⚠️ **恒等的判据要包含"位置"**：`offset_em` / `advance_em` 是**内容**
    /// 不是变换，所以它们不参与 [`Self::is_identity_transform`]。
    pub fn plain(ch: char, offset_em: f32, advance_em: f32) -> Self {
        Self {
            ch,
            offset_em,
            advance_em,
            scale: 1.0,
            rotation_deg: 0.0,
            offset_x_em: 0.0,
            offset_y_em: 0.0,
        }
    }

    /// 这个字的**变换**是不是恒等（位移 / 缩放 / 旋转都没动）。
    ///
    /// ⚠️ 调用方**必须**用它来分流：恒等时走原来的"整行一次栅格化"路径，
    /// 非恒等时才逐字画。这条分流是**性能与回归**的双重保障 ——
    /// 不判恒等就等于"所有字幕都改走逐字路"，那会让既有产物全部重算
    /// （缓存键变了、字节也变了，R4 当场破）。
    pub fn is_identity_transform(&self) -> bool {
        self.scale == 1.0
            && self.rotation_deg == 0.0
            && self.offset_x_em == 0.0
            && self.offset_y_em == 0.0
    }
}

/// 把一段文本拆成逐字格子。
///
/// `advance_em` 与 [`crate::text_layout::measure_em`] **共用同一张字宽表**
/// ⇒ `cells.last().offset + advance` 恒等于 `measure_em(text)`（有单测钉住）。
/// 这条等式就是"逐字画出来的行宽与整行画的完全一致"的依据。
///
/// ⚠️ 不配逐字效果时，调用方**不该调这个函数**（`Vec` 虽小，但它会进
/// 每一帧的求值路径）。它是"要逐字时才付的代价"。
pub fn char_cells(text: &str) -> Vec<CharCell> {
    let mut out = Vec::with_capacity(text.chars().count());
    let mut offset = 0.0_f32;
    for ch in text.chars() {
        let adv = advance_em(ch);
        out.push(CharCell::plain(ch, offset, adv));
        offset += adv;
    }
    out
}

/// 逐字格子 + 这一行的**整行**信息（下游画的时候两样都要）。
#[derive(Debug, Clone, PartialEq)]
pub struct CharLayout {
    pub cells: Vec<CharCell>,
    /// 整行的逻辑宽度（em）—— 与 `measure_em` 同值。
    pub total_em: f32,
}

impl CharLayout {
    pub fn of(text: &str) -> Self {
        let cells = char_cells(text);
        let total_em = cells.iter().map(|c| c.advance_em).sum();
        Self { cells, total_em }
    }

    /// **整行有没有任何一个字带变换**。
    ///
    /// 与 [`CharCell::is_identity_transform`] 是同一件事的行级版本 ——
    /// 渲染侧第一句就判它，恒等时**一个逐字分支都不进**。
    pub fn has_transform(&self) -> bool {
        self.cells.iter().any(|c| !c.is_identity_transform())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text_layout::measure_em;

    // =====================================================================
    // ① 与 `measure_em` 逐值自洽 —— 这是"逐字画 = 整行画"的地基
    // =====================================================================
    //
    // ⚠️ 这条不是"顺手测一下"：它是**唯一**能证明"拆成逐字之后行宽没变"的判据。
    //    破了它的症状是行会随字数**缓慢漂移**（居中时看出来是"越长的行越偏"），
    //    而逐字与整行**各自看都正常**。

    fn last_end(l: &CharLayout) -> f32 {
        l.cells.last().map(|c| c.offset_em + c.advance_em).unwrap_or(0.0)
    }

    #[test]
    fn 逐字格子累加出的行宽与整行一致() {
        for s in [
            "",
            "a",
            "abc",
            "你好",
            "你好世界",
            "abc你好def",
            "就是有人在他的那个直播间里刷屏",
            "a b c",
            "会 有 空 格",
            "！？。，、；：",
            "\u{3000}全角空格\u{3000}",
            "A中B文C",
        ] {
            let l = CharLayout::of(s);
            let want = measure_em(s);
            let got = last_end(&l);
            assert!(
                (got - want).abs() < 1e-5,
                "「{s}」：逐字累加 {got} != measure_em {want}"
            );
            assert!(
                (l.total_em - want).abs() < 1e-5,
                "「{s}」：total_em {} != measure_em {want}",
                l.total_em
            );
        }
    }

    // =====================================================================
    // ② 位置是**左边界**且严格递增（相邻字不重叠、不留洞）
    // =====================================================================
    //
    // ⚠️ 零宽字符（组合记号）的 advance 是 0 ⇒ **允许相等**。
    //    断言写成"严格递增"会把合法的组合序列判红。

    #[test]
    fn 位置是左边界且不倒退() {
        let l = CharLayout::of("a你b好");
        assert_eq!(l.cells.len(), 4);
        for w in l.cells.windows(2) {
            assert!(
                w[1].offset_em >= w[0].offset_em,
                "位置倒退了：{} → {}",
                w[0].offset_em,
                w[1].offset_em
            );
        }
        assert_eq!(l.cells[0].offset_em, 0.0, "第一个字的偏移必须是 0");
        // 第二个字紧接第一个（左边界 = 前一个的左边界 + 它的宽）
        assert!((l.cells[1].offset_em - l.cells[0].advance_em).abs() < 1e-6);
    }

    // =====================================================================
    // ③ 默认全是恒等 —— R4 的地基
    // =====================================================================

    #[test]
    fn 默认格子全是恒等变换() {
        let l = CharLayout::of("你好abc");
        assert!(
            !l.has_transform(),
            "刚拆出来的格子不该带任何变换（否则老工程会被改走逐字路）"
        );
        for c in &l.cells {
            assert!(c.is_identity_transform(), "{:?} 不是恒等", c.ch);
            assert_eq!(c.scale, 1.0);
            assert_eq!(c.rotation_deg, 0.0);
            assert_eq!(c.offset_x_em, 0.0);
            assert_eq!(c.offset_y_em, 0.0);
        }
    }

    #[test]
    fn 任一变换都会让行级判据翻面() {
        let mut l = CharLayout::of("abc");
        assert!(!l.has_transform());
        // 逐字缩放
        l.cells[1].scale = 1.5;
        assert!(l.has_transform(), "逐字缩放必须被行级判据看见");
        l.cells[1].scale = 1.0;
        assert!(!l.has_transform());
        // 逐字旋转
        l.cells[2].rotation_deg = 12.0;
        assert!(l.has_transform(), "逐字旋转必须被行级判据看见");
        l.cells[2].rotation_deg = 0.0;
        assert!(!l.has_transform());
        // 逐字位移（x）
        l.cells[0].offset_x_em = 0.3;
        assert!(l.has_transform(), "逐字位移必须被行级判据看见");
        l.cells[0].offset_x_em = 0.0;
        assert!(!l.has_transform());
        // 逐字位移（y）
        l.cells[0].offset_y_em = -0.2;
        assert!(l.has_transform(), "逐字纵向位移必须被行级判据看见");
    }

    // =====================================================================
    // ④ 零宽字符：不占宽但仍要有格子（逐字序号要连续）
    // =====================================================================
    //
    // ⚠️ 逐字错峰按 **序号** 取时间 ⇒ 序号必须与"用户看到的第几个字"一致。
    //    把零宽字符过滤掉会让序号整体前移，于是"第 3 个字"指到了别的字上
    //    （而观感上"某个字没动"，无从归因）。

    #[test]
    fn 零宽字符保留格子但零宽() {
        // U+0301 = 组合尖音符（零宽）
        let l = CharLayout::of("e\u{0301}x");
        assert_eq!(l.cells.len(), 3, "零宽字符也要有格子（序号要连续）");
        assert_eq!(l.cells[1].ch, '\u{0301}');
        assert_eq!(l.cells[1].advance_em, 0.0, "组合记号本身不占宽");
        // ⚠️ 组合记号的 `offset` 落在**前一个字的右边界**上（= `e` 之后），
        //    而不是"与前一个字同起"。这是对的：它修饰前一个字、位置紧随其后。
        //    ⚠️ 我第一版写的是"与 `cells[0]` 位置相同" ⇒ **判据自己错了**
        //      （实测 `cells[1].offset = 0.5 = cells[0].offset + cells[0].advance`）。
        //    真正该钉的不变量是：它与**后一个字**同起（因为自己不占宽）。
        assert!(
            (l.cells[1].offset_em - l.cells[2].offset_em).abs() < 1e-6,
            "零宽字符应当与后一个字同起：{} vs {}",
            l.cells[1].offset_em,
            l.cells[2].offset_em
        );
        // 而且它紧跟前一个字的右边界（不产生额外空隙）
        assert!(
            (l.cells[1].offset_em - (l.cells[0].offset_em + l.cells[0].advance_em)).abs() < 1e-6,
            "零宽字符必须紧贴前一个字的右边界"
        );
    }

    // =====================================================================
    // ⑤ 全角 / 半角真的区分开了（逐字定位的核心假设）
    // =====================================================================

    #[test]
    fn 全角与半角宽度不同() {
        let l = CharLayout::of("A中");
        assert!(
            l.cells[1].advance_em > l.cells[0].advance_em * 1.5,
            "全角应当明显宽于半角：{} vs {}",
            l.cells[1].advance_em,
            l.cells[0].advance_em
        );
    }
}
