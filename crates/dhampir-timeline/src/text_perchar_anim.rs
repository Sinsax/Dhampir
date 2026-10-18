//! **逐字动效的求值**（P3/P4，2026-10-13）：把一个「逐字档位」在某一帧上
//! 摊成**每个字各自的变换**。
//!
//! # 这个模块与 [`crate::text_perchar`] 的分工
//!
//! * `text_perchar` 管**几何**：字在哪、多宽（与 `measure_em` 同一张表）；
//! * **本模块管时间**：第 `i` 个字在这一帧应该缩多少、转多少、挪多远。
//!
//! 分开的理由与 `text_motion` 那边一样：几何是静态契约（改它要动字宽表），
//! 而时间曲线是要反复调的东西。合成一个文件会让改动互相踩。
//!
//! # ⚠️⚠️ 为什么错峰必须**按序号算时间**，而不是"累加状态"
//!
//! 逐字动效的本质是**同一个曲线在每个字上晚开始一会儿**（错峰）。
//! 天真的写法是"每帧把进度加到每个字上"，那是**累积状态**：
//! `seek` 到第 N 帧与顺序播到第 N 帧会给出**不同**的结果 ——
//! 而预览是 `seek` 的、成片是顺序的（本仓 `handheld_sway` 与
//! `text_motion` 都立过这条规矩）。
//!
//! ⇒ 这里全部是**纯函数**：`local` 帧 + 字的序号 ⇒ 变换。
//! 同一个 `(frame, index)` 永远得到同一个结果，**与怎么到达那一帧无关**。
//!
//! # 口径：错峰量取"行内位置比例"还是"序号比例"
//!
//! 取**序号比例**（`index / (count - 1)`），不取像素位置比例。
//! 理由：混排（中日文 + 拉丁）时全角字占的宽是半角的 2 倍，
//! 按像素比例错峰会让"视觉上相邻的两个字"错开一大截，而"视觉上隔很远的
//! 两个字"几乎同时动 —— 观感是"节奏乱了"，但**看不出是哪里乱了**。
//! 按序号则每相邻一对的间隔恒定，与规格 §1.2「逐字错峰」的时序图一致。

use serde::{Deserialize, Serialize};

use crate::text_perchar::{CharCell, CharLayout};

/// **一个逐字档位的参数**（对应 `polish.toml` 里 `anim` 的一档）。
///
/// ⚠️ 全是**裸值 + 明确的默认**（不是 `Option`）—— 与 `SubtitleStyle` 那六个
/// `Option` 字段**不同的口径**，理由在这里值得写清楚：
///
/// * `SubtitleStyle` 上的 `Option` 是为了**区分"没写"与"写了默认值"**，
///   因为那几个字段由用户在 toml 里手写（"显式写了 1.0"是用户意图）。
/// * 本结构是**代码内部的中间表示**：它由转译器从 `anim` 档名**展开**而来，
///   展开完就是一个完整答案，没有"用户写没写"这一说。
///   ⇒ 用裸值，省掉下游每一处 `unwrap_or`。
///
/// ⚠️ 但 `name` 例外地是 `Option` —— 它纯诊断用（报告里点名是哪一档），
/// 且 `None` 有明确含义（"这一条没配逐字效果"）。它**不参与任何求值**。
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PerCharSpec {
    /// **这是第几档**（诊断用）。`None` = 本档不做逐字动效。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// 逐字**起始缩放**（1.0 = 不缩）。
    #[serde(default = "one")]
    pub scale_from: f32,
    /// 逐字**终止缩放**（1.0 = 不缩）。
    #[serde(default = "one")]
    pub scale_to: f32,
    /// 逐字**起始旋转**（角度）。
    #[serde(default)]
    pub rotation_from: f32,
    #[serde(default)]
    pub rotation_to: f32,
    /// 逐字**位移**（em；x 向右、y 向下），起始 / 终止。
    #[serde(default)]
    pub offset_x_from: f32,
    #[serde(default)]
    pub offset_x_to: f32,
    #[serde(default)]
    pub offset_y_from: f32,
    #[serde(default)]
    pub offset_y_to: f32,
    /// **每个字自己的动画时长**（帧）。
    #[serde(default)]
    pub per_char_frames: i64,
    /// **相邻两个字开始的间隔**（帧）。0 = 全部同时（"逐字"退化成"整行"）。
    #[serde(default)]
    pub stagger_frames: i64,
    /// **曲线**（`back_out` / `linear` / …），名字与
    /// [`crate::schema::Easing::from_text`] 认的那套一致。
    ///
    /// ⚠️ 默认 `linear` **不是** `Easing::default()`（那是同一个值，但这里
    /// 刻意用字符串字面量：字段类型是 `String`，与 `Keyframe::easing` 同款，
    /// 认不出的串由求值处退回线性、由校验层报 `unknown_easing`）。
    #[serde(default = "linear_easing")]
    pub easing: String,
    /// **错峰方向**：`false` = 从第 0 个字往后（默认）。
    ///
    /// `true` = 从**最后一个字往前**（"从尾巴收"那类）。做成显式开关而不是
    /// "把序号倒过来"由调用方做 —— 倒序号会让 `stagger` 的语义跟着反，
    /// 两处反了刚好抵消、看着"没生效"。
    #[serde(default, skip_serializing_if = "is_false")]
    pub reverse: bool,
    /// **按序号放大的位移**（em）：越靠后的字挪得越多（负数 = 越靠前挪得多）。
    ///
    /// # ⚠️⚠️ 为什么必须有它（2026-10-13 真机实测踩到的）
    ///
    /// 上面那四个 `offset_*` 是**同一个值给每个字**。于是一个
    /// `offset_x_to = 0.4` 的档把**所有字一起右移** —— 字距一点没变，
    /// 只是整行平移。**真机实测**：`一二三四五六七八` 八个字的
    /// 起点差恒为 `20, 20, 20, …`，与没有逐字效果时**完全一样**，
    /// 包围盒只差 2px（那点差来自落点取整）。
    ///
    /// 而"整行平移"用 `transform.translate` 就够了，**不需要逐字拆行** ——
    /// 所以那个档实际上是"付了逐字的代价、拿到整行的效果"，且
    /// **判据会假绿**（包围盒确实变了一点）。
    ///
    /// ⇒ "散开"的本意是**越往后的字挪得越多**（fan out），那是
    /// `rank × spread`，不是常数。本字段就管这个：
    ///
    /// ```text
    /// rank = reverse ? (count-1-index) : index
    /// 额外位移 x = spread_x_em × rank / (count-1)
    /// ```
    ///
    /// 除 `count-1` 是**归一化**：让"最后一个字挪满 `spread_x_em`"，
    /// 且**与字数无关**（否则 20 个字的一句话会散到画面外，观感完全不同）。
    /// `count <= 1` 时 rank 项取 0（一个字没有"散开"可言）。
    ///
    /// 与 `offset_*` 的关系：两者**相加**（`offset` 是共同的基线偏移、
    /// `spread` 是逐字的额外差）。只写 `spread` 是最常见的用法。
    #[serde(default)]
    pub spread_x_em: f32,
    #[serde(default)]
    pub spread_y_em: f32,
    /// 逐字位移的**基线**（每个字共同的，与 `offset_x_from/to` 同形）。
    #[serde(default)]
    pub spread_x_from: f32,
    #[serde(default)]
    pub spread_x_to: f32,
    #[serde(default)]
    pub spread_y_from: f32,
    #[serde(default)]
    pub spread_y_to: f32,
    /// **按序号放大的字号**（乘数，不是加法）：越靠后的字越大（负数 = 越小）。
    ///
    /// # ⚠️⚠️ 为什么单有 `scale_from/to` 不够（2026-10-13，C 组施工时发现）
    ///
    /// `scale_from` / `scale_to` 是**每个字同一个值** ⇒ 只能整行缩放。
    /// 而 C 组的「大小字」（末/首/中字重音）、「变小」（递减/骤降/尾音）、
    /// 「放大」（递增/爆开/首字引爆）**共 11 档**要的都是
    /// **按序号改字号** —— 一个档把**某一个字**放大、其余不变。
    /// 那是 `rank` 的函数，不是常数。
    ///
    /// ```text
    /// rank = reverse ? (count-1-index) : index   // 与 spread_x_em 同一个
    /// 第 i 个字最终 scale = mix(scale_from, scale_to) × (1 + scale_spread × rank)
    /// ```
    ///
    /// # 为什么是**乘法**而不是加法
    ///
    /// 字号是**倍率**。倍率的"再大 20%"与"再加 0.2"在 `scale_from != 1.0` 时
    /// 完全不是一回事：上式里 `scale_from = 0.6`（先缩小）再 `+0.2`
    /// 得到的是 `0.8`（相对原字仍小），而 `×(1+0.2)` 得到 `0.72`。
    /// 观感上要的是**相对当前倍率的比例**，所以用乘法。
    ///
    /// ⚠️ `rank` 与 `spread_x_em` **共用**：同一个"第几个字"。
    ///    各算一份的话，"沿弧线散开"这类组合档（位移递增 + 字号递增）
    ///    会在 `reverse` 下**方向不一致**（一个从左起、一个从右起）。
    ///
    /// ⚠️ `count <= 1` 时 rank 项为 0（一个字没有"哪个字重音"可言）。
    #[serde(default)]
    pub scale_spread: f32,
}

/// `1.0` 的 serde 默认（缩放的两个端点）。
///
/// ⚠️ **不能省成 `#[serde(default)]`**：`f32` 的默认是 `0.0`，
/// 而缩放的"什么都不做"是 `1.0` —— 少写一个键就会让字**缩成不可见**
/// （画面里"字幕没了"，却没有任何报错，因为 0 是合法值）。
fn one() -> f32 {
    1.0
}

/// 缓动串的 serde 默认。
fn linear_easing() -> String {
    "linear".to_string()
}

/// `false` 不写进文件（"既有工程逐字节不变"的执行处，与 `layer.rs` 的 `is_false` 同款）。
fn is_false(v: &bool) -> bool {
    !*v
}

impl Default for PerCharSpec {
    fn default() -> Self {
        Self {
            name: None,
            scale_from: 1.0,
            scale_to: 1.0,
            rotation_from: 0.0,
            rotation_to: 0.0,
            offset_x_from: 0.0,
            offset_x_to: 0.0,
            offset_y_from: 0.0,
            offset_y_to: 0.0,
            per_char_frames: 0,
            stagger_frames: 0,
            easing: "linear".to_string(),
            reverse: false,
            spread_x_em: 0.0,
            spread_y_em: 0.0,
            spread_x_from: 0.0,
            spread_x_to: 0.0,
            spread_y_from: 0.0,
            spread_y_to: 0.0,
            scale_spread: 0.0,
        }
    }
}

impl PerCharSpec {
    /// **本档有没有逐字动效**。
    ///
    /// ⚠️ 判据是"时长 > 0 且**至少有一个通道真的会动**"，
    /// 不是"`name.is_some()`" —— 一个只有名字、所有端点都相等的档
    /// （比如手滑配成 `scale_from == scale_to == 1.0`）**什么都不会动**，
    /// 让它进逐字路径等于白白把整行拆开重画。
    pub fn is_active(&self) -> bool {
        if self.per_char_frames <= 0 {
            return false;
        }
        let moves = |a: f32, b: f32| (a - b).abs() > 1e-6;
        moves(self.scale_from, self.scale_to)
            || moves(self.rotation_from, self.rotation_to)
            || moves(self.offset_x_from, self.offset_x_to)
            || moves(self.offset_y_from, self.offset_y_to)
            // ⚠️ 散开那对通道也各自要能单独生效 —— 漏了它们的症状是
            //    "只配了 spread 的档**完全不生效**"（`is_active` 为假 ⇒ 走老路）。
            || moves(self.spread_x_from, self.spread_x_to)
            || moves(self.spread_y_from, self.spread_y_to)
            // ⚠️⚠️ **`scale_spread` 单独判**，不能并进上面那行 ——
            //    它是**乘数**、且只看它自己非零（不看 `scale_from/to`）：
            //    「按序号放大字号」那类档的两个端点**是相等的**（都是 1.0），
            //    用 `moves(scale_from, scale_to)` 判会**漏掉整个档**
            //    ⇒ 症状是"只配字号递增的档完全不生效"（实测：4 条判据一起红）。
            || self.scale_spread.abs() > 1e-6
    }

    /// 第 `index` 个字（共 `count` 个）**自己那一份动画的起始帧**。
    ///
    /// ⚠️ `count <= 1` 时恒 0（单个字没有"错峰"可言）。
    /// ⚠️ **负数结果要夹到 0**：这样"最后一个字往前错峰"时，
    /// 靠后的字起始为负 ⇒ 在 `local = 0` 时它们**已经动了一部分**，
    /// 而 `start <= 0` 与 `start == 0` 在下面的求值里是**同一个答案**
    /// （`t` 会被夹到 `[0,1]`）⇒ 不会引入"倒放"这种怪状态。
    pub fn start_frame_for(&self, index: usize, count: usize) -> i64 {
        if count <= 1 {
            return 0;
        }
        let last = (count - 1) as i64;
        let i = index.min(count - 1) as i64;
        let rank = if self.reverse { last - i } else { i };
        rank * self.stagger_frames
    }
}

/// **一个字的变换**（在某帧上求出来的结果）。
///
/// 与 [`CharCell`] 的字段一一对应，但**已经乘过曲线与字号** ——
/// 它是"这一帧这个字长什么样"的完整答案。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CharTransform {
    pub scale: f32,
    pub rotation_deg: f32,
    /// em 单位（与 `CharCell` 同口径）。
    pub offset_x_em: f32,
    pub offset_y_em: f32,
}

impl Default for CharTransform {
    fn default() -> Self {
        Self {
            scale: 1.0,
            rotation_deg: 0.0,
            offset_x_em: 0.0,
            offset_y_em: 0.0,
        }
    }
}

impl CharTransform {
    pub fn is_identity(&self) -> bool {
        self.scale == 1.0
            && self.rotation_deg == 0.0
            && self.offset_x_em == 0.0
            && self.offset_y_em == 0.0
    }
}

/// 逐字档位在 `local_frame` 帧、第 `index` 个字上的变换。
///
/// `local_frame` 是**相对这一条文字自己的起点**（与
/// `overlay.rs::evaluate_overlay` 的 `frame - element.start` 同口径）。
///
/// # 三条边界（每一条都有测试）
///
/// 1. **没动效**（[`PerCharSpec::is_active`] 为假）⇒ 恒等。
/// 2. **还没轮到**（`local < start`）⇒ **起始值**。⚠️ 不是"恒等" ——
///    「逐字弹出」的第 5 个字在它开始之前必须是"缩到最小且透明"，
///    给恒等会让它**先以正常大小显示一下再重新弹**（闪一下，极难归因）。
/// 3. **已经完了**（`local >= start + dur`）⇒ **终止值**（不是恒等）。
///    同理：散开的字不该在动画结束后"飞回去"。
pub fn char_transform_at(
    spec: &PerCharSpec,
    index: usize,
    count: usize,
    local_frame: i64,
) -> CharTransform {
    if !spec.is_active() {
        return CharTransform::default();
    }
    let start = spec.start_frame_for(index, count);
    let dur = spec.per_char_frames;
    // 进度 `t`：`< start` ⇒ 0（起始值）、`>= start + dur` ⇒ 1（终止值）。
    // ⚠️ 整数除法会算出 0 ⇒ 全部字都停在起始值（"配了不动"）。必须浮点。
    let t = ((local_frame - start) as f32 / dur as f32).clamp(0.0, 1.0);
    // ⚠️ 曲线走**既有**的 `schema::Easing`（`from_text` + `apply`），**不另写一份**
    //    —— 那样两条会漂。它自己已经处理了"认不出的串退回线性"
    //    （逐帧路径里不许 panic：一帧 panic = 整块 chunk 失败）。
    let e = crate::schema::Easing::from_text(spec.easing.clone()).apply(t);
    let mix = |a: f32, b: f32| a + (b - a) * e;
    // **按序号放大的位移**：`rank / (count-1)` 归一化 ⇒ 最后一个字挪满
    // `spread_*_em`，且**与字数无关**（20 个字的一句话不会散到画面外）。
    // `count <= 1` 时 rank 项为 0（一个字没有"散开"可言）。
    //
    // ⚠️ 这一段是 2026-10-13 真机实测之后补的：没有它时"散开"退化成整行平移
    //    （每个字同一个位移），字距一点没变 ⇒ 判据只能靠"包围盒宽了 2px"
    //    假绿。见 `spread_x_em` 的文档。
    let rank = if count > 1 {
        let last = count - 1;
        let i = index.min(last);
        let r = if spec.reverse { last - i } else { i };
        r as f32 / last as f32
    } else {
        0.0
    };
    let spread_x = mix(spec.spread_x_from, spec.spread_x_to) * spec.spread_x_em * rank;
    let spread_y = mix(spec.spread_y_from, spec.spread_y_to) * spec.spread_y_em * rank;
    CharTransform {
        // ⚠️ `scale_spread` 是**乘数**（见它的文档：倍率的比例才是"大一点"的直觉）。
        //    `rank` 与位移**共用**同一个，保证组合档在 `reverse` 下方向一致。
        scale: mix(spec.scale_from, spec.scale_to) * (1.0 + spec.scale_spread * rank),
        rotation_deg: mix(spec.rotation_from, spec.rotation_to),
        offset_x_em: mix(spec.offset_x_from, spec.offset_x_to) + spread_x,
        offset_y_em: mix(spec.offset_y_from, spec.offset_y_to) + spread_y,
    }
}

/// **出场版**的 [`char_transform_at`]：同一条 `from/to`，但**反着走**（`to → from`），
/// 窗口**锚在出点**（用户口径 2026-10-18：「逐字变化…固定为入场逐字变大，出场逐字变小」）。
///
/// # 为什么是这样实现的（**没有**再抄一份 `mix`）
///
/// 入场那条的进度是 `t = (local - start_i) / per_char_frames`，`start_i` 由
/// [`PerCharSpec::start_frame_for`] 给（逐字错峰）。
/// ⇒ 想要 `t = 1 - u`（`u` = 出场进度）**只需要喂一个换算过的 `local`**：
///
/// ```text
/// X = start_i + per_char_frames * (1 - u)
/// ```
///
/// 于是曲线、缓动、`rank`、`spread_*` 一个字节都不用重写（抄一份必然漂，
/// 而漂的表现是"入场的字变大、出场的字却在旋转"）。
///
/// # 边界（与入场同一条纪律：**不许回到恒等**）
///
/// * 还没轮到（`local < start_out`）⇒ **终止值**（`t = 1`，即正常大小）；
/// * 已经收完（`local >= start_out + out_frames`）⇒ **起始值**（`t = 0`，即缩到最小）——
///   给恒等会让字在消失前"弹回原大小"，那一下很显眼但极难归因。
///
/// `out_frames <= 0` 或 `span_frames <= 0` ⇒ 恒等（**缺省不启用 = 老工程零回归**）。
pub fn char_transform_at_out(
    spec: &PerCharSpec,
    index: usize,
    count: usize,
    local_frame: i64,
    span_frames: i64,
    out_frames: i64,
) -> CharTransform {
    if !spec.is_active() || span_frames <= 0 || out_frames <= 0 || spec.per_char_frames <= 0 {
        return CharTransform::default();
    }
    // 逐字错峰沿用入场那一套偏移，但**整段往左挪 out_frames**（锚在出点）。
    let off = spec.start_frame_for(index, count);
    let start_out = span_frames - out_frames - off;
    let dur = spec.per_char_frames;
    let elapsed = local_frame - start_out;
    if elapsed < 0 {
        // 还没轮到 —— 停在终止值（正常大小）。
        return char_transform_at(spec, index, count, off + dur);
    }
    if elapsed >= out_frames {
        // 已经收完 —— 停在起始值（最小），不许弹回去。
        return char_transform_at(spec, index, count, off);
    }
    let u = elapsed as f32 / out_frames as f32;
    let x = off as f32 + dur as f32 * (1.0 - u);
    char_transform_at(spec, index, count, x.round() as i64)
}

/// 把一行的**全部字**在某一帧上的变换求出来。
///
/// 返回的 `Vec` 与 `layout.cells` **一一对应**（同长度、同序）。
/// ⚠️ 长度相同这件事是下游按 `zip` 画的依据 —— 短了会**静默少画几个字**
/// （画面里"最后几个字没了"，看着像排版问题）。有测试钉住。
pub fn layout_transforms_at(
    spec: &PerCharSpec,
    layout: &CharLayout,
    local_frame: i64,
) -> Vec<CharTransform> {
    let count = layout.cells.len();
    (0..count)
        .map(|i| char_transform_at(spec, i, count, local_frame))
        .collect()
}

/// [`layout_transforms_at`] 的**出场版**：窗口锚在出点、进度反着走（逐字变小）。
///
/// 长度同样与 `layout.cells` 一一对应（下游按 `zip` 画）。求值见
/// [`char_transform_at_out`]；**何时切到这条由宿主决定**（`dhampir-core/src/overlay.rs`，
/// 判据是 `local >= span - out - 错峰跨度` —— 无条件切会把入场整个盖掉）。
pub fn layout_transforms_at_out(
    spec: &PerCharSpec,
    layout: &CharLayout,
    local_frame: i64,
    span_frames: i64,
    out_frames: i64,
) -> Vec<CharTransform> {
    let count = layout.cells.len();
    (0..count)
        .map(|i| char_transform_at_out(spec, i, count, local_frame, span_frames, out_frames))
        .collect()
}

/// 把求出来的变换**写回**格子（渲染侧要的就是这个）。
///
/// ⚠️ 原地改 `cells` 而**不是**返回新 `Vec<CharCell>`：`CharCell` 里的
/// `ch` / 位置是布局的产物，复制一遍等于把布局重算一次（而且两次可能不一致）。
/// 这里只覆盖那四个变换字段。
pub fn apply_transforms(cells: &mut [CharCell], transforms: &[CharTransform]) {
    for (cell, t) in cells.iter_mut().zip(transforms.iter()) {
        cell.scale = t.scale;
        cell.rotation_deg = t.rotation_deg;
        cell.offset_x_em = t.offset_x_em;
        cell.offset_y_em = t.offset_y_em;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 一个"逐字弹出"式的档位：从 0 缩到 1，每个字 10 帧、相邻错开 3 帧。
    fn pop() -> PerCharSpec {
        PerCharSpec {
            name: Some("pop_perchar".into()),
            scale_from: 0.0,
            scale_to: 1.0,
            per_char_frames: 10,
            stagger_frames: 3,
            easing: "linear".into(),
            ..Default::default()
        }
    }

    // =====================================================================
    // ① `is_active`：**只有名字、没有运动**的档不算生效
    // =====================================================================
    //
    // ⚠️ 这条挡的是"配了一个空档"⇒ 白白把整行拆开重画（性能），
    //    而画面**一点变化都没有**（用户以为配错了效果名）。

    #[test]
    fn 没有运动的档不算生效() {
        // 有名字，但所有端点相等
        let flat = PerCharSpec {
            name: Some("whatever".into()),
            per_char_frames: 30,
            ..Default::default()
        };
        assert!(
            !flat.is_active(),
            "端点全相等 ⇒ 什么都不会动，不该进逐字路径"
        );

        // 有运动但时长为 0
        let zero_dur = PerCharSpec {
            scale_from: 0.0,
            scale_to: 1.0,
            per_char_frames: 0,
            ..Default::default()
        };
        assert!(!zero_dur.is_active(), "时长为 0 ⇒ 没有动画");

        // 正常的一份
        assert!(pop().is_active());

        // 四个通道**各自**都要能单独把 is_active 打开
        for one in [
            PerCharSpec {
                scale_from: 0.5,
                ..pop()
            },
            PerCharSpec {
                rotation_from: -20.0,
                ..pop()
            },
            PerCharSpec {
                offset_x_from: -0.5,
                ..pop()
            },
            PerCharSpec {
                offset_y_from: 0.8,
                ..pop()
            },
        ] {
            assert!(one.is_active(), "单独一个通道也要能生效：{one:?}");
        }
    }

    // =====================================================================
    // ② 错峰：第 i 个字真的比第 i-1 个晚 `stagger` 帧
    // =====================================================================

    #[test]
    fn 错峰按序号依次推迟() {
        let s = pop();
        assert_eq!(s.start_frame_for(0, 5), 0);
        assert_eq!(s.start_frame_for(1, 5), 3);
        assert_eq!(s.start_frame_for(2, 5), 6);
        assert_eq!(s.start_frame_for(3, 5), 9);
        assert_eq!(s.start_frame_for(4, 5), 12);
        // 单字：没有错峰可言
        assert_eq!(s.start_frame_for(0, 1), 0);
        assert_eq!(s.start_frame_for(0, 0), 0);
    }

    #[test]
    fn 反向错峰从尾部开始() {
        let mut s = pop();
        s.reverse = true;
        let n = 5;
        // 最后一个字最先动
        assert_eq!(s.start_frame_for(n - 1, n), 0);
        assert_eq!(s.start_frame_for(0, n), 12);
        // 与正向**严格相反**
        let mut f = pop();
        f.reverse = false;
        for i in 0..n {
            assert_eq!(
                s.start_frame_for(i, n),
                f.start_frame_for(n - 1 - i, n),
                "反向的第 {i} 个 = 正向的第 {} 个",
                n - 1 - i
            );
        }
    }

    // =====================================================================
    // ③ 三个阶段：还没轮到 / 正在动 / 已经完了
    // =====================================================================
    //
    // ⚠️ 边界两侧各测一次（"还没轮到给起始值、已经完了给终止值"是两条
    //    各自会踩的坑：给恒等会让字"先正常显示再重弹 / 动画完又飞回去"）。

    #[test]
    fn 还没轮到给起始值() {
        let s = pop();
        // 第 3 个字从第 9 帧开始
        for f in [0, 1, 8] {
            let t = char_transform_at(&s, 3, 5, f);
            assert_eq!(t.scale, 0.0, "第 {f} 帧：还没轮到 ⇒ 必须是起始缩放 0");
        }
    }

    #[test]
    fn 正在动时按曲线插值() {
        let s = pop();
        // 第 0 个字：0..10 帧
        assert!((char_transform_at(&s, 0, 5, 0).scale - 0.0).abs() < 1e-6);
        assert!(
            (char_transform_at(&s, 0, 5, 5).scale - 0.5).abs() < 1e-6,
            "线性曲线中点"
        );
        assert!((char_transform_at(&s, 0, 5, 10).scale - 1.0).abs() < 1e-6);
    }

    #[test]
    fn 已经完了给终止值而不是恒等() {
        let s = PerCharSpec {
            scale_from: 1.0,
            scale_to: 0.0, // 收缩到 0（不是回到恒等！）
            per_char_frames: 10,
            stagger_frames: 0,
            ..Default::default()
        };
        let t = char_transform_at(&s, 0, 1, 999);
        assert_eq!(t.scale, 0.0, "动画完了要停在**终止值**，不能弹回 1.0");
    }

    // =====================================================================
    // ④ 纯函数：与"怎么到达这一帧"无关（seek == 顺序播）
    // =====================================================================
    //
    // ⚠️ 这是本模块**最要紧**的一条：预览是 seek 的、成片是顺序的。
    //    用累积状态实现错峰时，两者会在**每一帧**上都不同，
    //    而"预览看着没问题、成片里节奏不对"正是最难归因的那一类。

    #[test]
    fn 同一帧的结果与到达方式无关() {
        let s = pop();
        let layout = CharLayout::of("逐字错峰的测试文本");
        // 顺序"播"一遍，把每一帧的结果记下来
        let mut seq = Vec::new();
        for f in 0..60 {
            seq.push(layout_transforms_at(&s, &layout, f));
        }
        // 现在**乱序 seek** 回去，结果必须逐值相同
        for f in [59, 0, 30, 7, 7, 30, 0, 59] {
            let again = layout_transforms_at(&s, &layout, f);
            assert_eq!(again, seq[f as usize], "第 {f} 帧 seek 回来结果不同");
        }
    }

    // =====================================================================
    // ⑤ 长度必须与格子一一对应（短了会静默少画字）
    // =====================================================================

    #[test]
    fn 变换数量与字数一致() {
        let s = pop();
        for text in ["", "a", "你好", "a你b好c", "很长的一句话用来数格子"] {
            let layout = CharLayout::of(text);
            let ts = layout_transforms_at(&s, &layout, 5);
            assert_eq!(
                ts.len(),
                layout.cells.len(),
                "「{text}」：变换数 {} != 格子数 {}",
                ts.len(),
                layout.cells.len()
            );
        }
    }

    #[test]
    fn 写回格子只动变换四个字段() {
        let s = pop();
        let mut layout = CharLayout::of("abc");
        let before: Vec<_> = layout
            .cells
            .iter()
            .map(|c| (c.ch, c.offset_em, c.advance_em))
            .collect();
        let ts = layout_transforms_at(&s, &layout, 4);
        apply_transforms(&mut layout.cells, &ts);
        let after: Vec<_> = layout
            .cells
            .iter()
            .map(|c| (c.ch, c.offset_em, c.advance_em))
            .collect();
        assert_eq!(before, after, "位置与字不能被写回改掉（那是布局的产物）");
        // 而变换确实写进去了
        assert!(layout.has_transform(), "写回之后行级判据必须翻面");
    }

    // =====================================================================
    // ⑥ 不动效时**一个字段都不改**（R4 的地基）
    // =====================================================================

    #[test]
    fn 不动效时恒等且写回不改任何东西() {
        let s = PerCharSpec {
            name: Some("fade_only".into()),
            ..Default::default()
        };
        assert!(!s.is_active());
        let mut layout = CharLayout::of("你好");
        let ts = layout_transforms_at(&s, &layout, 12);
        assert!(ts.iter().all(|t| t.is_identity()), "不动效 ⇒ 全恒等");
        apply_transforms(&mut layout.cells, &ts);
        assert!(!layout.has_transform(), "写回恒等之后仍然没有变换");
    }

    // =====================================================================
    // ⑦ 曲线认得出、也认得错
    // =====================================================================

    #[test]
    fn 曲线走既有那条路认不出的退回线性() {
        let mut s = pop();
        // 认得出：`back_out` 会**过冲**（中点值**高于**线性中点）。
        // ⚠️ 我第一版把方向写反了（断言 `mid_back < mid_lin`）⇒ **判据自己红**。
        //    实测 `back_out(0.5) = 1.0877`：`u = t-1`，`u²((S+1)u+S)+1`
        //    在 `t=0.5` 给 `0.25·(2.70158·(-0.5)+1.70158)+1 ≈ 1.0877` > 0.5。
        //    ⇒ **过冲是向上的**（先冲过头再落回 1.0），这才是"回弹"。
        s.easing = "back_out".into();
        let mid_back = char_transform_at(&s, 0, 1, 5).scale;
        s.easing = "linear".into();
        let mid_lin = char_transform_at(&s, 0, 1, 5).scale;
        assert!(
            mid_back > mid_lin,
            "back_out 会过冲，中点应当**高于**线性中点：{mid_back} vs {mid_lin}"
        );
        // 而且终点仍收敛到终止值（过冲是中途的，不是终点跑偏）
        s.easing = "back_out".into();
        assert!(
            (char_transform_at(&s, 0, 1, 10).scale - 1.0).abs() < 1e-5,
            "过冲之后必须落回终点值"
        );
        // 认不出：退回线性（不许 panic —— 一帧里 panic 会让整个出片失败）
        s.easing = "definitely-not-a-curve".into();
        assert!(
            (char_transform_at(&s, 0, 1, 5).scale - mid_lin).abs() < 1e-6,
            "认不出的曲线串必须退回线性"
        );
    }

    // =====================================================================
    // ⑨ **散开**：位移按序号递增（这是 2026-10-13 真机实测之后补的）
    // =====================================================================
    //
    // ⚠️⚠️ 这组判据守的是**一个假绿**：没有 `spread_*` 时"散开"退化成
    //    整行平移，而整行平移**照样**让包围盒变宽一点点（落点取整）
    //    ⇒ 真机判据会绿着，而字距一个像素都没变。

    #[test]
    fn 散开按序号递增而常数位移只是整行平移() {
        let n = 8;
        // ① 常数位移：每个字**同一个**位移 ⇒ 字距不变
        let flat = PerCharSpec {
            offset_x_from: 0.0,
            offset_x_to: 0.4,
            per_char_frames: 10,
            ..Default::default()
        };
        let ts: Vec<f32> = (0..n)
            .map(|i| char_transform_at(&flat, i, n, 10).offset_x_em)
            .collect();
        assert!(
            ts.windows(2).all(|w| (w[1] - w[0]).abs() < 1e-6),
            "常数位移下每个字的位移必须**相同**（否则就不是「常数」）：{ts:?}"
        );

        // ② 散开：位移**线性递增**，最后一个是满值
        let fan = PerCharSpec {
            spread_x_from: 0.0,
            spread_x_to: 1.0,
            spread_x_em: 2.0,
            per_char_frames: 10,
            ..Default::default()
        };
        let ts: Vec<f32> = (0..n)
            .map(|i| char_transform_at(&fan, i, n, 10).offset_x_em)
            .collect();
        assert!((ts[0] - 0.0).abs() < 1e-6, "第一个字不该挪：{}", ts[0]);
        assert!(
            (ts[n - 1] - 2.0).abs() < 1e-6,
            "最后一个字挪满 2em：{}",
            ts[n - 1]
        );
        // 相邻间隔 = 2.0 / 7（线性）
        let step = 2.0 / 7.0;
        for w in ts.windows(2) {
            assert!(
                (w[1] - w[0] - step).abs() < 1e-5,
                "相邻间隔该是 {step}，实得 {}（{ts:?}）",
                w[1] - w[0]
            );
        }
    }

    #[test]
    fn 散开与字数无关() {
        let fan = PerCharSpec {
            spread_x_from: 0.0,
            spread_x_to: 1.0,
            spread_x_em: 2.0,
            per_char_frames: 10,
            ..Default::default()
        };
        // 3 个字与 30 个字，最后一个字都挪满同一个 2em（归一化的意义）
        for n in [3, 8, 30] {
            let last = char_transform_at(&fan, n - 1, n, 10).offset_x_em;
            assert!(
                (last - 2.0).abs() < 1e-5,
                "{n} 个字时最后一个字挪了 {last}，应当恒为 2em（归一化）"
            );
        }
    }

    #[test]
    fn 散开也能反向() {
        let mut fan = PerCharSpec {
            spread_x_from: 0.0,
            spread_x_to: 1.0,
            spread_x_em: 2.0,
            per_char_frames: 10,
            ..Default::default()
        };
        fan.reverse = true;
        let n = 5;
        let ts: Vec<f32> = (0..n)
            .map(|i| char_transform_at(&fan, i, n, 10).offset_x_em)
            .collect();
        assert!((ts[0] - 2.0).abs() < 1e-5, "反向时**第一个**字挪满：{ts:?}");
        assert!(
            (ts[n - 1] - 0.0).abs() < 1e-5,
            "反向时**最后一个**字不动：{ts:?}"
        );
    }

    #[test]
    fn 只配散开也能让档生效() {
        // ⚠️ `is_active` 漏掉 spread 那一对的症状是"只配散开的档完全不生效"。
        let only_spread = PerCharSpec {
            spread_x_from: 0.0,
            spread_x_to: 1.0,
            spread_x_em: 1.5,
            per_char_frames: 10,
            ..Default::default()
        };
        assert!(only_spread.is_active(), "只配散开也必须算生效");
        let only_spread_y = PerCharSpec {
            spread_y_from: -1.0,
            spread_y_to: 0.0,
            spread_y_em: 0.8,
            per_char_frames: 10,
            ..Default::default()
        };
        assert!(only_spread_y.is_active(), "只配纵向散开也必须算生效");
    }

    #[test]
    fn 散开与基线位移相加() {
        let both = PerCharSpec {
            offset_x_from: 0.0,
            offset_x_to: 0.25, // 所有字共同挪 0.25em
            spread_x_from: 0.0,
            spread_x_to: 1.0,
            spread_x_em: 1.0, // 再按序号最多多挪 1em
            per_char_frames: 10,
            ..Default::default()
        };
        let n = 3;
        let ts: Vec<f32> = (0..n)
            .map(|i| char_transform_at(&both, i, n, 10).offset_x_em)
            .collect();
        // 0.25 + rank×1.0，rank = 0 / 0.5 / 1
        assert!((ts[0] - 0.25).abs() < 1e-5, "{ts:?}");
        assert!((ts[1] - 0.75).abs() < 1e-5, "{ts:?}");
        assert!((ts[2] - 1.25).abs() < 1e-5, "{ts:?}");
    }

    // =====================================================================
    // ⑩ **按序号放大字号**（`scale_spread`）—— C 组 11 档靠它
    // =====================================================================
    //
    // ⚠️⚠️ 这组判据守的是**一个能力缺口**，不是一个边界情况：
    //    `scale_from/to` 是每个字同一个值 ⇒ 只能整行缩放，而
    //    「大小字」「变小」「放大」共 11 档要的都是**某一个字**与其他不同。
    //    没有它时那 11 档**做不出来**（不是"做得不好"）。

    #[test]
    fn 字号按序号放大而常数缩放是整行缩放() {
        let n = 6;
        // ① 常数缩放：每个字**同一个**倍数 ⇒ 只能是整行缩放
        let flat = PerCharSpec {
            scale_from: 0.5,
            scale_to: 1.0,
            per_char_frames: 10,
            ..Default::default()
        };
        let ss: Vec<f32> = (0..n)
            .map(|i| char_transform_at(&flat, i, n, 10).scale)
            .collect();
        assert!(
            ss.windows(2).all(|w| (w[1] - w[0]).abs() < 1e-6),
            "常数缩放下每个字的倍数必须相同，实得 {ss:?}"
        );

        // ② 按序号：最后一个是 1+scale_spread 倍
        let acc = PerCharSpec {
            scale_from: 1.0,
            scale_to: 1.0,
            scale_spread: 0.5,
            per_char_frames: 10,
            ..Default::default()
        };
        let ss: Vec<f32> = (0..n)
            .map(|i| char_transform_at(&acc, i, n, 10).scale)
            .collect();
        assert!((ss[0] - 1.0).abs() < 1e-6, "第一个字不该变：{}", ss[0]);
        assert!(
            (ss[n - 1] - 1.5).abs() < 1e-6,
            "最后一个字该是 1.5 倍：{}",
            ss[n - 1]
        );
        let step = 0.5 / (n - 1) as f32;
        for w in ss.windows(2) {
            assert!((w[1] - w[0] - step).abs() < 1e-5, "{ss:?}");
        }
    }

    #[test]
    fn 字号按序号是乘法而不是加法() {
        // ⚠️ 这条钉的是**语义选择**：倍率的"再大 20%"是 ×1.2，不是 +0.2。
        //    `scale_from = 0.5` 时两者差一倍多（0.6 vs 0.5×1.2=0.6 恰好同？
        //    不 —— 用 0.5 举不出来，取 scale_to = 0.6、scale_spread = 1.0：
        //      加法 0.6+1.0 = 1.6，乘法 0.6×2.0 = 1.2）
        let spec = PerCharSpec {
            scale_from: 0.6,
            scale_to: 0.6,
            scale_spread: 1.0,
            per_char_frames: 10,
            ..Default::default()
        };
        let last = char_transform_at(&spec, 3, 4, 10).scale;
        assert!(
            (last - 1.2).abs() < 1e-5,
            "该是乘法 0.6×2.0 = 1.2，实得 {last}（1.6 就是写成了加法）"
        );
    }

    #[test]
    fn 字号递增也能反向() {
        let mut spec = PerCharSpec {
            scale_from: 1.0,
            scale_to: 1.0,
            scale_spread: 1.0,
            per_char_frames: 10,
            ..Default::default()
        };
        spec.reverse = true;
        let n = 4;
        let ss: Vec<f32> = (0..n)
            .map(|i| char_transform_at(&spec, i, n, 10).scale)
            .collect();
        assert!((ss[0] - 2.0).abs() < 1e-5, "反向时**第一个**字最大：{ss:?}");
        assert!((ss[n - 1] - 1.0).abs() < 1e-5, "{ss:?}");
    }

    #[test]
    fn 字号递增与位移递增共用同一个序号() {
        // ⚠️ 组合档（散开 + 放大）在 `reverse` 下两维必须**同向**。
        //    各算一份 rank 的话会一个从左起、一个从右起 ——
        //    画面是"字往左散、字号往右大"，两边各自看都正常。
        let mut spec = PerCharSpec {
            scale_from: 1.0,
            scale_to: 1.0,
            scale_spread: 0.5,
            spread_x_from: 0.0,
            spread_x_to: 1.0,
            spread_x_em: 1.0,
            per_char_frames: 10,
            ..Default::default()
        };
        spec.reverse = true;
        let n = 5;
        let ts: Vec<_> = (0..n).map(|i| char_transform_at(&spec, i, n, 10)).collect();
        // 反向：index 0 的 rank 最大 ⇒ 字号最大、位移最多
        assert!(
            ts[0].scale > ts[n - 1].scale,
            "字号该从前往后递减：{:?}",
            ts.iter().map(|t| t.scale).collect::<Vec<_>>()
        );
        assert!(
            ts[0].offset_x_em > ts[n - 1].offset_x_em,
            "位移该从前往后递减：{:?}",
            ts.iter().map(|t| t.offset_x_em).collect::<Vec<_>>()
        );
    }

    #[test]
    fn 只配字号递增也能让档生效() {
        // ⚠️ `is_active` 漏掉它的症状：只配字号的档**完全不生效**。
        //    ⚠️ 注意不能只看 `scale_from != scale_to` —— 这一个档那两个是**相等**的。
        let only_scale = PerCharSpec {
            scale_from: 1.0,
            scale_to: 1.0,
            scale_spread: 0.8,
            per_char_frames: 10,
            ..Default::default()
        };
        assert!(
            only_scale.is_active(),
            "只配了「按序号放大字号」的档也必须算生效（两个端点相等也算）"
        );
    }

    #[test]
    fn 字号递增与字数无关() {
        let spec = PerCharSpec {
            scale_from: 1.0,
            scale_to: 1.0,
            scale_spread: 1.0,
            per_char_frames: 10,
            ..Default::default()
        };
        for n in [2, 5, 40] {
            let last = char_transform_at(&spec, n - 1, n, 10).scale;
            assert!(
                (last - 2.0).abs() < 1e-5,
                "{n} 个字时最后一个字是 {last}，应当恒为 2.0（归一化）"
            );
        }
    }

    // =====================================================================
    // ⑧ 四个通道**同时**动时互不干扰
    // =====================================================================

    #[test]
    fn 四个通道各自独立插值() {
        let s = PerCharSpec {
            scale_from: 1.0,
            scale_to: 2.0,
            rotation_from: -30.0,
            rotation_to: 30.0,
            offset_x_from: -1.0,
            offset_x_to: 1.0,
            offset_y_from: 0.5,
            offset_y_to: -0.5,
            per_char_frames: 10,
            stagger_frames: 0,
            easing: "linear".into(),
            ..Default::default()
        };
        let t = char_transform_at(&s, 0, 1, 5); // 中点
        assert!((t.scale - 1.5).abs() < 1e-6, "scale {}", t.scale);
        assert!(t.rotation_deg.abs() < 1e-5, "rotation {}", t.rotation_deg);
        assert!(t.offset_x_em.abs() < 1e-5, "offset_x {}", t.offset_x_em);
        assert!(t.offset_y_em.abs() < 1e-5, "offset_y {}", t.offset_y_em);
        // 起点与终点
        let a = char_transform_at(&s, 0, 1, 0);
        assert!((a.scale - 1.0).abs() < 1e-6 && (a.rotation_deg + 30.0).abs() < 1e-5);
        assert!((a.offset_x_em + 1.0).abs() < 1e-6 && (a.offset_y_em - 0.5).abs() < 1e-6);
    }
}

#[cfg(test)]
mod out_window_tests {
    use super::*;

    fn spec() -> PerCharSpec {
        PerCharSpec {
            name: Some("perchar_big".to_string()),
            scale_from: 0.2,
            scale_to: 1.0,
            per_char_frames: 10,
            stagger_frames: 0,
            easing: "linear".to_string(),
            ..PerCharSpec::default()
        }
    }

    /// 出场窗口：逐字变小（to -> from），窗口锚在出点；
    /// 窗口前停在终止值、窗口后停在起始值（不许弹回恒等 —— 弹一下极显眼又难归因）。
    ///
    /// 变异自证：把 char_transform_at_out 里那句 1.0 - u 改成 u
    /// 则第二条断言红（出场变成越来越大）。
    #[test]
    fn out_window_shrinks_each_char_and_never_bounces_back() {
        let s = spec();
        let (span, out) = (100i64, 20i64);
        assert!(
            (char_transform_at_out(&s, 0, 1, 60, span, out).scale - 1.0).abs() < 1e-6,
            "窗口之前应当停在正常大小"
        );
        let mid = char_transform_at_out(&s, 0, 1, 90, span, out).scale;
        assert!((mid - 0.6).abs() < 1e-6, "出场半程应当 0.6，实际 {mid}");
        assert!((char_transform_at_out(&s, 0, 1, 100, span, out).scale - 0.2).abs() < 1e-6);
        let mut prev = f32::MAX;
        for f in 60..=100 {
            let sc = char_transform_at_out(&s, 0, 1, f, span, out).scale;
            assert!(sc <= prev + 1e-6, "缩放在 f={f} 反弹：{prev} -> {sc}");
            prev = sc;
        }
    }

    /// 缺省不启用（out_frames 或 span_frames <= 0）则恒等 —— 老工程零回归那条。
    #[test]
    fn out_window_is_off_by_default() {
        let s = spec();
        assert!(char_transform_at_out(&s, 0, 1, 50, 100, 0).is_identity());
        assert!(char_transform_at_out(&s, 0, 1, 50, 0, 20).is_identity());
    }
}
