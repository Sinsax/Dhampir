//! 文字覆盖层：某一帧上要画的**字幕**结构。
//!
//! # 为什么它是独立的一份结果，而不是塞进 Composite
//!
//! 图层清单说的是「哪几张纹理、用哪个变换叠上去」，而文字**没有纹理** ——
//! 它要先由宿主栅格化成位图。把两者塞进一个结构里会让「谁负责栅格化」变得含糊，
//! 而含糊的代价是两端各自决定，结构与像素一起分叉。
//!
//! 所以评估层出两份结果：图层清单（Composite）与文字覆盖层（这里）。
//! 两份都是**纯函数**算出来的；宿主不许自己再算一遍。
//!
//! # 为什么在 core 而不是在宿主
//!
//! 结构必须一致：几行、每行什么、占哪个归一化矩形。这条由 dhampir_timeline::text_layout
//! 算，core 负责「这一帧哪条字幕活着」。宿主只做一件事：把文字**画**进给定的矩形里。
//!
//! # 时间口径
//!
//! SRT/ASS 的时间戳是**绝对时间**（毫秒），与素材帧率无关 ——
//! 所以毫秒换算成帧号用**时间线**的时间基，不用字幕素材自己的时间基。
//! 一条字幕素材登记了什么时间基都不影响它什么时候出现。
//!
//! # 与文档坐标系的关系
//!
//! 归一化矩形依赖文档坐标系：字号是按**高度**的比例算的，而宽度是按**宽度**归一化的，
//! 所以同一段文字在 4:3 与 16:9 里的归一化宽度不同。这与 T1 确立的口径一致 ——
//! 归一化不是「与一切无关」，而是「与渲染目标尺寸无关」。
//!
//! # 弹幕走同一条路
//!
//! 弹幕与字幕在这层**没有分家**：同一张表（素材 id -> 已解析的条），同一个入口
//! （[`evaluate_overlay`]），出同一个结构。理由是两者对下游是同一种东西 ——
//! 「要画的一行字 + 它占的归一化矩形」，宿主那一侧的画字路径一个字都不用改。
//!
//! 差异只在两处，都放在 [`TextOverlay`] 的 `danmaku` 那半边：
//!
//! * 弹幕的矩形**随帧变化**（从右滚到左），由 `dhampir_timeline::danmaku::rect_at` 现算；
//! * 弹幕的泳道与在屏区间是**结构**，由 `dhampir_timeline::danmaku::layout` 分配 ——
//!   两端要报出同一张表，所以分配只能有一份实现。
//!
//! **颜色与描边两者共用**：`DanmakuSpec` 没有颜色字段（要分开就得动契约）。
//! 只有弹幕的工程看不出这件事（默认白字带描边）。

use dhampir_timeline::curve::channel_from;
use dhampir_timeline::danmaku::{layout as layout_danmaku, rect_at};
use dhampir_timeline::layer::{TimelineV2, default_plate_pad_px};
use dhampir_timeline::schema::{Frame, TimebaseDto, TrackKind};
use dhampir_timeline::subtitle::{Cue, frame_at_ms};
use dhampir_timeline::text_layout::{NormalizedRect, layout};
use std::collections::BTreeMap;

/// 一行要画的文字：内容 + 它占的**行盒**（归一化，相对文档坐标系）+ 这一帧的淡入淡出。
#[derive(Debug, Clone, PartialEq)]
pub struct TextItem {
    pub text: String,
    pub rect: NormalizedRect,
    /// 这一帧的不透明度（淡入淡出算出来的）。**由契约层的 [`text_envelope`] 给**，
    /// 宿主只照用 —— 见那个函数的说明。
    pub opacity: f32,
    /// 这一帧的纵向偏移（**文档像素**，正为向下）。同样是契约层算好的。
    pub dy_px: f32,
    /// 这一帧的**横向**偏移（**文档像素**，正为向右）。契约层算好的。
    ///
    /// # 为什么现在才需要它（T4）
    ///
    /// 在 T4 之前，文字的位移只有"入场上浮 / 退场上移"这一种，而它**只有纵向** ——
    /// 于是这个字段不存在，宿主拿 `dy_px` 就够了。抖动是**两维**的
    /// （只抖一个方向看着像"在呼吸"而不是"在抖"），所以横向必须单独有一条通道。
    ///
    /// 与 `dy_px` 一样是**文档像素**：换导出尺寸时两端的位移比例不变。
    /// 默认恒 0 ⇒ 既有工程逐字节不变（宿主加不加它都画在同一个位置）。
    pub dx_px: f32,
    /// **这一条的颜色**（已解析）。
    ///
    /// 与"轨道样式里的颜色"的关系：轨道给的是**默认**，一条 cue 自己带的
    /// （ASS 的 `\c&HBBGGRR&`）覆盖它。**在求值层就解析掉**，
    /// 于是宿主只看到"这一条是什么颜色"，不用自己判断该用哪一个 ——
    /// 两端各判一次就会在"有的条有颜色、有的没有"的工程上分叉。
    pub color: [u8; 4],
    /// **字号**（相对序列高的比例）。**布局算出来的事实**，跟着条目走 ——
    /// 宿主不该从行盒反推（行高可配之后那个反推是错的，见
    /// `dhampir_timeline::text_layout::TextLine::font_ratio`）。
    pub font_ratio: f32,
    /// **这一条被整体缩了多少**（1.0 = 没缩）。
    ///
    /// 参照的 `swEff = sw * wrapped.scale` —— **描边要跟着缩**，
    /// 而描边是宿主按 `stroke_px` 画的。与 `font_ratio` 同一条理由：
    /// 布局算出来的事实，跟着条目走。
    pub scale: f32,
    /// **按高亮切好的分段，颜色已经解析好**。
    ///
    /// 空 = 这一条没有 `.hl` 标记 —— 渲染侧照老路用 `color` 一次画完
    /// （**既有工程逐字节不变**）。
    ///
    /// # 为什么颜色在这里就解析掉
    ///
    /// 与 `color` 同一条规矩：轨道/样式给的是**默认**，`<span class="hl">`
    /// 覆盖它。**求值层解析掉**，宿主只看到"这一段是什么颜色"，
    /// 不用自己判该用哪一个 —— 两端各判一次就会在"有的条有标记、有的没有"
    /// 的工程上分叉。
    pub parts: Vec<TextRun>,
    /// **逐字格子**（P3，2026-10-13）：每个字的位置 + 这一帧它自己的变换。
    ///
    /// # 空 `Vec` = 这一条没有逐字效果（**绝大多数情况**）
    ///
    /// 与 `parts` 的"空 = 没有 `.hl`"同一个形状：宿主判一次空就分流。
    /// ⚠️ 逐字与 `.hl` **可以同时有** —— 逐字是按字定位、`.hl` 是按段配色，
    /// 两件事正交。真的同时出现时宿主走逐字路（逐字能把段色逐字带上，
    /// 因为每个字都知道自己属于哪一段 —— 但**目前不实现**，
    /// 同时出现时逐字路优先、`.hl` 的颜色暂时不生效，这是已知缺口）。
    ///
    /// # 为什么在**求值层**就把变换算进格子里
    ///
    /// 与 `font_ratio` 那条"乘过缩放的才是这一帧真正的事实"同一条理由：
    /// 时间只有求值层知道（`frame`），宿主拿到的是"这一帧的样子"。
    /// 让宿主自己按 `local_frame` 求值 = 两条渲染路（worker / wasm）
    /// 各写一份错峰逻辑 —— 而"预览对、成片不对"正是那么来的。
    ///
    /// 默认空 ⇒ 既有工程逐字节不变（宿主不读它就画在原来的位置）。
    pub cells: Vec<crate::timeline::text_perchar::CharCell>,
    /// **这一条所属字幕轨的轨道级样式。**
    ///
    /// # ⚠️ 为什么挂在条目上，而不是 `TextOverlay.subtitle_style` 那一个共享槽
    ///
    /// 这里曾经是**一个共享槽**：`subtitle_style` 在 `for track in &timeline.tracks`
    /// 循环里**每轨整份重写**，循环结束后只返回**最后一份**。于是
    /// **最后一条字幕轨说了算，前面所有轨的轨道级样式被整份吃掉** ——
    /// 症状是"元素轨 + 字幕轨并存时，元素轨的样式（含 T6 底衬）完全不生效"，
    /// 而**单条字幕轨时行为完全正确**（"最后一条"就是"唯一一条"）。
    ///
    /// ★ **它为什么活了这么久**：`grep -c "TrackKind::Subtitle"` 在当时的
    /// `overlay.rs` 里**只有 2** —— 既有单测**全都只放一条字幕轨**，
    /// 而这个缺陷**需要 ≥2 条轨才可能显形**。
    /// **不是测试写得差，是测试的形状恰好绕过了它。**
    /// ⇒ 补的判据必须**两条字幕轨并存**（`两条字幕轨各自的轨道级样式都生效`）。
    ///
    /// # 为什么是"挂在条目上"而不是"frame 带一份 `Vec<(track_id, TextStyle)>`"
    ///
    /// 1. **渲染侧本来就是逐条取的**：`paint_lines` 的循环体里，
    ///    字体解析、描边、底衬**全都按这一条**来。挂条目上 ⇒ 取用是
    ///    `&item.track_style`，**没有查找、没有"按 id 匹配"这一步**，
    ///    也就不存在"查不到时用哪个"这个必然出错的分支。
    /// 2. **形状上不可能再犯同一个错**：共享槽的病根是"多条轨共用一个位置"。
    ///    挂条目上之后，**同一个位置里的样式天生只属于那一条**，
    ///    "被后一条覆盖"这件事在类型上就写不出来了。
    /// 3. `Vec<(track_id, TextStyle)>` 那条路要求**每个取用点都记得按 id 查一次** ——
    ///    而漏掉一处的症状恰恰是"看着有效、其实用了别人的样式"，
    ///    与本仓最恨的那类静默同形。
    ///
    /// # 与逐条字段（`color` / `font_ratio` / `dx_px`）的关系
    ///
    /// 那三个字段**不受本 bug 影响** —— 它们在覆盖发生**之前**就 push 进 items 了。
    /// 这正好解释了实测现象：**T1（逐条 `font_ratio`）/ T4（逐条 `dx_px`）活着，
    /// T6（轨道级 `plate_color`）死了**。
    /// 现在轨道级的那些（`outline` / `stroke_px` / `shadow_*` / `plate_*` / 字体族与字重）
    /// 走这一条通道，两类字段各归各。
    pub track_style: TextStyle,
    /// **这一条要旋转的角度**（角度制，正为顺时针）。恒 `0.0` = 老行为。
    ///
    /// # 为什么是条级字段，不是轨级
    ///
    /// 与 `dx_px` / `dy_px` 同一条规矩（见上面 `track_style` 的长注释）：
    /// **逐条的字段天生不会被"后一条覆盖前一条"**。旋转如果做成轨级共享槽，
    /// 就会重演 `subtitle_style` 那个 bug —— 两条字幕轨并存时最后一条轨说了算，
    /// 而**单条轨时行为完全正确**（"最后一条"就是"唯一一条"），
    /// 于是缺陷只在"≥2 条轨"的工程上显形，单测全都绕过它。
    ///
    /// # 为什么需要它
    ///
    /// 宿主拿它画"倾斜的文字"（漫画感的强调、贴纸式排字）。在此之前契约层
    /// **没有任何位置**能表达"这一条要斜着画" —— 文字的变换只有位移（`dx_px`/`dy_px`）
    /// 与缩放（`scale`），三者各管一件事，旋转是缺的那一个自由度。
    ///
    /// # ✅ 来源与**已连接**的关键帧（2026-10-13）
    ///
    /// 这里填的是**文字元素自己的 `LayerV2.transform.rotation`**，
    /// 但**逐帧求值**：静态值只是 fallback，写了键就走曲线。
    ///
    /// ```text
    /// channel_from(element.transform.rotation, &element.keyframes, "rotation",
    ///              frame.saturating_sub(element.start))
    /// ```
    ///
    /// ⚠️ `channel_from` 与 `compose.rs::element_to_draw` 用的是**同一个函数**
    /// ⇒ 求值逻辑仍然只有一处，不会漂移。
    /// ⚠️ `local_frame` 用 **element 自己的** `start`，不是轨的 —— 元素可以晚于轨开始。
    ///
    /// > 沿革：本条原先写着"关键帧求值在 `compose.rs` 那条路上，与本字段**尚未连接**，
    /// > 给文字元素写 `rotation` 关键帧，本字段不会跟着动"。
    /// > 那句在 `bcff0a5` 之后**已过期**。「摇摆（rotation 往复）」因此可以做了。
    /// > ⇒ "**字段有了 ≠ 动画就有了**"那条教训仍然成立 —— 所以这里显式写出**已接通**，
    /// > 免得后来的人照着旧注释以为写键没用。
    ///
    /// # 落到工程 JSON 的坐标（下游写 tilt 时看这条）
    ///
    /// 要倾斜一段文字，写 **文字图层自己的** `layers[i].transform.rotation`；
    /// 写进 `TrackV2.subtitle`（轨级样式）**不会生效** —— 本字段读的是
    /// **element 的 transform**，不是轨道级样式。
    /// 想让它动，把关键帧写在**同一个元素**上（`layers[i].keyframes`，
    /// `target = "rotation"`，**帧号相对元素自己的 `start`**）。
    pub rotation_deg: f32,
    /// **这一条纵向额外拉伸的倍数**（`1.0` = 不拉伸）。恒 `1.0` = 老行为。
    ///
    /// 语义与 `TransformV2::scale_y` 一致、口径也一致：**乘在 `scale` 之上**
    /// （最终纵向倍数 = `scale × scale_y`）。缺省 1.0 ⇒ 纵向跟随 `scale` ⇒ 等比，
    /// 与"没有这个字段"逐字节相同。
    ///
    /// 用途是"非等比压缩"：横向压扁、纵向拉长（或反过来）做出挤扁/拉长的字。
    /// 与 `rotation_deg` 一起构成"倾斜 + 非等比压缩"那两种形变 —— 在此之前
    /// 契约层只有等比 `scale`，压扁一个字是**表达不出来**的。
    ///
    /// ⚠️ 同样是**条级**，理由与 `rotation_deg` 一样（见上）。
    /// ⚠️ 也**逐帧求值**（`bcff0a5` 接通，与 `rotation_deg` 共用一条 `channel_from`）——
    /// 但 `scale_y` **不在 `TRANSFORM_TARGETS` 里** ⇒ 写 `target = "scale_y"`
    /// 会被**校验层拒绝**（而不是静默忽略，那是好事）。
    /// 要做"逐帧压扁"得先把它加进那张表，见 `docs/plan/text-option-d-scope.md` §4.5。
    /// ⚠️ 落点坐标同 `rotation_deg`：`layers[i].transform.scale_y`。
    pub scale_y: f32,
}

/// 一段要画的文字 + 它**最终**的颜色。
#[derive(Debug, Clone, PartialEq)]
pub struct TextRun {
    pub text: String,
    pub color: [u8; 4],
}

/// 一条要画的弹幕：内容 + **这一帧**的矩形 + 泳道与在屏区间 + 这一帧的淡入淡出。
///
/// 矩形与另外几个字段放一起，是为了让「两端给出同一张表」这件事**能逐字段对账**：
/// 只比矩形的话，泳道被分配错了（两条换了位置）在单帧里可能看不出来。
#[derive(Debug, Clone, PartialEq)]
pub struct DanmakuTextItem {
    pub text: String,
    /// 这一帧的归一化矩形（滚动位置是帧的函数）。
    pub rect: NormalizedRect,
    pub lane: u32,
    pub enter: Frame,
    pub exit: Frame,
    /// 这一帧的不透明度（含 `DanmakuSpec.opacity` 这个基础值）。
    pub opacity: f32,
    /// 这一帧的纵向偏移（文档像素）。
    pub dy_px: f32,
    /// **这一条的颜色**（已解析）：cue 自带（ASS 的 `\c`）覆盖轨道默认。
    pub color: [u8; 4],
    /// **字号**（相对序列高的比例）。同 [`TextItem::font_ratio`]。
    pub font_ratio: f32,
    /// **这一条的滚动时长（帧）** —— 逐条不同（见 `Cue::travel_ms`）。
    ///
    /// 带着它是为了**逐字段对账**：`rect` 是 `travel_frames` 的函数，
    /// 只比矩形的话，"两条的时长被对调了"在某些帧上可能看不出来。
    /// 与 `lane`/`enter`/`exit` 同一条理由。
    pub travel_frames: i64,
    /// **这一条所属弹幕轨的轨道级样式。**
    ///
    /// # 与 `TextItem::track_style` 同一个理由、同一个 bug
    ///
    /// 帧级的 `TextOverlay.danmaku_style` 与字幕那个共享槽**形状完全相同**：
    /// 在 `for track in &timeline.tracks` 里**每轨整份重写**，循环后只留最后一份
    /// ⇒ **两条弹幕轨并存时，前面那条轨的轨道级样式被整份吃掉**。
    ///
    /// ★ 而 `grep -c "TrackKind::Danmaku"` 在测试文件里**只有 2**
    /// —— **与字幕那个 bug 完全相同的盲区形状**（都需要 ≥2 条轨才显形）。
    ///
    /// # 哪些字段会丢，哪些不会
    ///
    /// **不会丢**：`color` / `font_ratio` —— 它们是**逐条**字段，
    /// 在覆盖发生**之前**就已经 push 进 `DanmakuTextItem` 了。
    ///
    /// **会丢**：`outline` / `stroke_px` / `stroke_color` / `font_family` / `font_weight`
    /// —— 这几项**只**在帧级 `danmaku_style` 里读，而宿主画弹幕时把
    /// **同一份**传给**每一条**。真机实测：两条弹幕轨并存时，
    /// 前面那条轨的描边**从 246 个墨像素掉到 2 个**。
    pub track_style: TextStyle,
}

/// 一类文字的画法（颜色 + 描边）。
///
/// # 为什么字幕与弹幕**各一套**
///
/// 这个结构以前是没有的：`TextOverlay` 上只有一份 `color` + `outline` 给两者共用，
/// 模块文档还写明了"要分开就得动契约"。而**字幕与弹幕的默认色本来就不同**
/// —— 共用一份时**必然有一个错**，
/// 而"两边的字都能看见"这件事让人以为没问题。
#[derive(Debug, Clone, PartialEq)]
pub struct TextStyle {
    pub color: [u8; 4],
    /// **高亮词的颜色**（`None` = 不做高亮）。
    ///
    /// 与 `SubtitleStyle::highlight_color` 同一件事，但从契约带到这里时
    /// 已经**逐条解析成 `TextItem::parts` 里的具体颜色** —— 这个字段留着
    /// 是给"想知道样式默认值"的宿主看的（以及清单里能对账）。
    pub highlight_color: Option<[u8; 4]>,
    /// **字号**（相对序列高的比例）。布局**本来就知道**它，带着走，
    /// 宿主就不必再从行盒反推（行高可配之后那个反推是错的）。
    pub font_ratio: f32,
    pub outline: bool,
    /// 描边宽度（**文档像素**；由 `stroke_ratio * 目标高` 换算而来，
    /// 换算在 `evaluate_overlay` 里做，因为只有它知道目标尺寸）。
    ///
    /// **口径是"外侧宽度"**（见 `SubtitleStyle::stroke_ratio` 的说明）：
    /// CSS 的居中描边是一半，ffmpeg 的 `borderw` 是全部 —— 契约挑了后者。
    pub stroke_px: f32,
    pub stroke_color: [u8; 4],
    /// **外圈描边**宽度（**目标像素**；0 = 不画外圈）。
    ///
    /// 与 [`TextStyle::stroke_px`] 同一处、同一条理由（只有求值层知道目标尺寸）：
    /// `SubtitleStyle::outer_stroke.ratio * 目标高`。口径是「在 `stroke_px` 那一圈**之外**
    /// 再向外扩多少」⇒ 白环那一遍的 `Outline` = `stroke_px + outer_stroke_px`。
    pub outer_stroke_px: f32,
    /// 外圈颜色（RGBA）。`outer_stroke_px == 0` 时被忽略。
    pub outer_stroke_color: [u8; 4],
    /// 字体族名。宿主**从你给的字体目录里按这个名字找**，找不到要**报出来**
    /// （不是悄悄换一个字体画）。
    pub family: Option<String>,
    /// 字重（CSS 刻度）。宿主据此在字体目录里挑一个粗体文件。
    pub weight: u32,
    /// **文字阴影的颜色**（`None` = 不画阴影）。
    ///
    /// 与 `stroke_ratio` 那边同一条分工：**求值层已经解析完**
    /// （`None` 与"alpha = 0"在这里是同一个答案），宿主只照用。
    pub shadow_color: Option<[u8; 4]>,
    /// 阴影的水平偏移（**文档像素**，与 `transform.x/y` 同一坐标系）。
    ///
    /// 量纲与 [`TextStyle::stroke_px`] 一致：都是"求值层拿到的那个尺寸"下的像素。
    /// 默认路径（导出尺寸 = `render_hints`）上它就是成片里的像素；显式换导出尺寸时
    /// 它与描边宽度**一起**保持原样 —— 两者同一条边界，见 `docs/usage.md` 的字幕阴影一节。
    pub shadow_dx_px: f32,
    /// 阴影的垂直偏移（文档像素，**正数向下**）。
    pub shadow_dy_px: f32,
    /// **阴影的模糊半径**（**像素**；0 = 硬阴影）。
    ///
    /// 由契约里的 `shadow_blur_ratio` 乘**目标高**换算而来 —— 与
    /// `stroke_ratio` -> `stroke_px` 是同一处、同一条理由：
    /// **只有求值层知道目标尺寸**，宿主只看到"多少像素"。
    ///
    /// 两端核不同（canvas `shadowBlur` / ffmpeg `gblur`），所以只保证观感近似。
    pub shadow_blur_px: f32,
    /// **文字块底边的归一化 y**（1.0 = 贴底）。
    ///
    /// 与 `SubtitleStyle::anchor_bottom` **同名同形**，原样带过去（不换算）——
    /// 它本来就是归一化坐标，与目标尺寸无关（同 `plate_color` 那条理由的反面：
    /// 那个不需要换算是因为它是纯色，这个不需要是因为它已经归一化了）。
    pub anchor_bottom: f32,
    /// **底衬的颜色**（`None` = 不铺底衬）。T6。
    ///
    /// 与 `SubtitleStyle::plate_color` 是同一个值，原样带过来 —— 底衬色
    /// **不需要**按目标尺寸换算（它是纯色，没有"宽度"这种要换算的量）。
    pub plate_color: Option<[u8; 4]>,
    /// **底衬向四周外扩的像素数**（目标像素）。T6。
    ///
    /// 契约里是 `Option`（`None` = 默认 12），这里**已经解析成具体像素值** ——
    /// 与 `stroke_px` / `shadow_blur_px` 同一条分工：**只有求值层知道
    /// "没写时算多少"**，宿主只看到"多少像素"，不用再问一次默认值
    /// （两处各问一次就是两个实现，改一处漏一处）。
    pub plate_pad_px: f32,
}

impl Default for TextStyle {
    fn default() -> Self {
        Self {
            color: [255, 255, 255, 255],
            // None = 不做高亮（默认）。
            highlight_color: None,
            font_ratio: 0.0,
            outline: true,
            stroke_px: 0.0,
            stroke_color: [0, 0, 0, 255],
            // 0 = 不画外圈（老工程逐字节不变）。
            outer_stroke_px: 0.0,
            outer_stroke_color: [255, 255, 255, 255],
            family: None,
            weight: 400,
            // None = 不画阴影；偏移与模糊都是 0。
            // **这一组就是"老工程什么都不变"** —— 少了 None，没写过阴影的工程
            // 会突然多出阴影（而且它压在所有字上）。
            shadow_color: None,
            shadow_dx_px: 0.0,
            shadow_dy_px: 0.0,
            shadow_blur_px: 0.0,
            // **1.0 = 贴底**，与 `SubtitleStyle::anchor_bottom` 的缺省同值 ——
            // 同样属于"老工程什么都不变"那一组。
            anchor_bottom: 1.0,
            // None = 不铺底衬（默认）—— 与阴影同一组："老工程什么都不变"。
            plate_color: None,
            // 没有底衬时这个数不会被读到；给 `SubtitleStyle` 的同一个默认值，
            // 免得出现"两个 12"（见 `Default` 那段注释的同一理由）。
            plate_pad_px: dhampir_timeline::layer::default_plate_pad_px(),
        }
    }
}

/// 某一帧的文字覆盖层。
#[derive(Debug, Clone, PartialEq)]
pub struct TextOverlay {
    /// 按轨道顺序（先画的在前），轨内按行。
    pub items: Vec<TextItem>,
    /// 字幕之外还有弹幕：同一帧里活着的弹幕条，按轨道顺序、轨内按分配顺序。
    ///
    /// 与 `items` 分开而不是混在一起：两者的**落点规则不同**（字幕居中于整条目标宽，
    /// 弹幕按自己的宽度左对齐），宿主据此选不同的画法。混在一起就得在每个元素上带一个
    /// 种类标签，而那与"这里是纯结构"的定位冲突。
    pub danmaku: Vec<DanmakuTextItem>,
    /// ⚠️ **字幕与弹幕都没有帧级的"画法"字段了** —— 两者的轨道级样式
    /// 都跟着**每一条**走：[`TextItem::track_style`] 与
    /// [`DanmakuTextItem::track_style`]。
    ///
    /// 理由是同一条真 bug 的**两处实例**：这里曾经有两个**共享槽**，
    /// 循环里每轨整份重写、只留最后一份 ⇒ **最后一条轨说了算**
    /// （元素轨 + 字幕轨并存时，前面那条轨的样式全失效）。
    /// 两处都已按同一形状修掉。
    /// 因为超过 max_lines 被丢弃的**行数**（所有字幕轨加起来）。
    ///
    /// 与 text_layout 的口径一致：丢弃必须计数，否则「字幕只显示了一半」
    /// 看起来和「字幕就是这样」一样。
    pub dropped_lines: usize,
    /// 因为泳道排不下被丢弃的**弹幕条数**（所有弹幕轨加起来）。
    ///
    /// 与 `dropped_lines` 同款：少的几条看起来和「素材里就那几条」一样。
    pub dropped_danmaku: usize,
}

impl TextOverlay {
    pub fn is_empty(&self) -> bool {
        self.items.is_empty() && self.danmaku.is_empty()
    }
}

/// 字幕素材 id -> 已解析的字幕条。
///
/// **由宿主提供**：core 是纯函数，不做 I/O，读文件与解析都由宿主做完再交进来。
/// 这与 AssetTimebases 是同一个缝。
pub type SubtitleTable = BTreeMap<String, Vec<Cue>>;

/// 算某一帧的文字覆盖层。没有字幕/弹幕轨、或者这一帧没有活着的字时返回 None。
///
/// 返回 None 而不是空结构，是为了让调用方**不必区分**「没有字」与
/// 「有字但这一帧是空的」—— 两种情况的处理完全一样：什么都不画。
///
/// 判据是「**字幕与弹幕两半都空**才 None」：只有弹幕的一帧照样要画，
/// 只有字幕的一帧也照样要画。没有任何弹幕轨的工程走的是与 T2 完全相同的分支
/// （弹幕那半边恒空），所以无弹幕工程的输出逐字节不变。
pub fn evaluate_overlay(
    timeline: &TimelineV2,
    frame: Frame,
    sequence: (u32, u32),
    subtitles: Option<&SubtitleTable>,
) -> Option<TextOverlay> {
    let table = subtitles?;
    if table.is_empty() {
        return None;
    }

    let timebase: TimebaseDto = timeline.timebase;
    let mut items: Vec<TextItem> = Vec::new();
    let mut danmaku: Vec<DanmakuTextItem> = Vec::new();
    let mut dropped_lines = 0_usize;
    let mut dropped_danmaku = 0_usize;
    // 两类文字**各一套**画法（见 `TextStyle` 的说明：以前共用一份，必错一个）。
    //
    // ⚠️ 字幕这一份**不再有共享槽** —— 它跟着每条 `TextItem` 走（见 `TextItem::track_style`）。
    // 这里只在"这一帧到底有没有字幕"这个**布尔**上留一个账（`styled`）。
    let mut styled = false;
    let mut danmaku_styled = false;
    // 描边宽度是"目标高 * 比例" —— 换算需要目标尺寸，所以在这里做一次。
    let target_h = sequence.1 as f32;

    for track in &timeline.tracks {
        if track.kind != TrackKind::Subtitle {
            continue;
        }
        // 轨道级的样式：一条字幕轨 = 一个字幕素材 + 一套样式。
        // `SubtitleStyle` 现在带 `Option<String>`（`font_family`），所以**不是 Copy**
        // —— 这里借它，不搬它。
        let Some(style) = track.subtitle.as_ref() else {
            continue;
        };
        // 活着的元素至多一个（轨内不许重叠，由校验保证）。
        let Some(element) = track.layers.iter().find(|layer| layer.covers(frame)) else {
            continue;
        };
        if !element.enabled {
            continue;
        }
        let Some(source) = element.source.as_ref() else {
            continue;
        };
        let Some(cues) = table.get(&source.asset_id) else {
            continue;
        };
        // ---------------------------------------------------------------------
        // **这条轨的轨道级样式**（契约的比例 → 像素在这里落地一次）。
        // ---------------------------------------------------------------------
        //
        // ⚠️ 它算在**每条轨自己的作用域里**，然后跟着这一轨的每个 `TextItem` 走。
        //
        // 以前这里是把结果写进循环外的**一个共享槽** `subtitle_style`，
        // 循环结束只返回最后一份 ⇒ **最后一条字幕轨说了算**，前面所有轨的
        // 轨道级样式被整份吃掉。这就是"元素轨 + 字幕轨并存时元素轨样式全失效"
        // 的根因（含 T6 底衬）。详见 `TextItem::track_style`。
        let track_style = TextStyle {
            color: style.color,
            highlight_color: style.highlight_color,
            font_ratio: style.font_ratio,
            outline: style.outline,
            stroke_px: style.stroke_ratio * target_h,
            stroke_color: style.stroke_color,
            // **外圈**与上面那条同一条口径、同一处落地（`ratio × 目标高`）。
            // `None` ⇒ 0 ⇒ 渲染侧一次多遍栅格化都不做（既有工程逐字节不变）。
            outer_stroke_px: style
                .outer_stroke
                .map(|outer| outer.ratio * target_h)
                .unwrap_or(0.0),
            outer_stroke_color: style
                .outer_stroke
                .map(|outer| outer.color)
                .unwrap_or([255, 255, 255, 255]),
            family: style.font_family.clone(),
            weight: style.font_weight,
            // **阴影也在这里落地**：契约给的是比例（`shadow_blur_ratio`），
            // 换算成像素要目标尺寸 —— 与上面 `stroke_px` 同一条理由、同一处。
            // 偏移本来就是像素（文档像素），原样带过去。
            shadow_color: style.shadow_color,
            shadow_dx_px: style.shadow_dx_px,
            shadow_dy_px: style.shadow_dy_px,
            // **文字纵向锚**：归一化坐标，原样带过去（不换算）。
            anchor_bottom: style.anchor_bottom,
            shadow_blur_px: style.shadow_blur_ratio * target_h,
            // **底衬（T6）**：颜色原样带过去；外扩量在这里把契约的
            // `Option` 解析成具体像素 —— `None` 走同一个默认值函数
            // （`default_plate_pad_px`），不在这里另写一个 12.0。
            plate_color: style.plate_color,
            plate_pad_px: style.plate_pad_px.unwrap_or_else(default_plate_pad_px),
        };

        for cue in cues {
            if !cue_visible_at(cue, frame, &timebase) {
                continue;
            }
            // 这一帧在 cue 内的位置（毫秒）与 cue 总长（毫秒）。
            let (Some(local_ms), Some(span_ms)) = (
                ms_at_frame_milli(frame, &timebase).map(|now| now.saturating_sub(cue.start_ms)),
                cue.end_ms.checked_sub(cue.start_ms),
            ) else {
                continue;
            };
            // **淡入淡出 + 弹入缩放由契约层算**（`text_envelope_scaled`），宿主只照用。
            //
            // 取值口径集中在 `text_motion` 的两个辅助函数里（`unwrap_or` 只在那里
            // 出现一次）—— 六个字段散在调用点各写一遍 `unwrap_or`，
            // 就一定会有一处漏掉，而漏掉的那处在"字段写了但没生效"之前**没有症状**。
            let (scale_from, scale_peak, scale_ms) =
                dhampir_timeline::text_motion::scale_in_params(style);
            let (opacity, dy_px, enter_scale) = dhampir_timeline::text_motion::text_envelope_scaled(
                local_ms,
                span_ms,
                style.fade_in_ms,
                style.fade_out_ms,
                style.rise_in_px,
                style.rise_out_px,
                scale_from,
                scale_peak,
                scale_ms,
            );
            // **周期缩放（心跳，2026-10-18）优先**：启用（`scale_curve` + `scale_cycle_ms`
            // 都给了）时它**完全接管**缩放 —— `scale_in_*` 那条是 `back_out3` 的**单峰**、
            // 且只在入场窗口里，表达不了"放大→回落→再小放大、每秒来一次"。
            // 缺省（老工程 / 其它档）⇒ 原样用上面算出来的 `enter_scale`（**零回归**）。
            let enter_scale = match dhampir_timeline::text_motion::scale_curve_params(style) {
                Some((points, cycle)) => {
                    dhampir_timeline::text_motion::scale_curve_at(local_ms, cycle, points)
                }
                None => enter_scale,
            };
            // **抖动（T4）**：纯 `local_ms` 的函数 —— 顺序播放与 `seek` 同源。
            //
            // 相位默认给 0；"同屏多条要错开"由宿主显式写 `shake_phase` 决定
            // （契约不给"自动错开"，那要靠条目身份，而契约层没有那个身份）。
            let (shake_px, shake_hz, shake_phase) =
                dhampir_timeline::text_motion::shake_params(style);
            let (shake_dx, shake_dy) = dhampir_timeline::text_motion::text_shake_px(
                local_ms as f32 / 1000.0,
                shake_px,
                shake_hz,
                shake_phase,
            );
            let laid: dhampir_timeline::text_layout::TextLayout =
                layout(&cue.text, style, sequence);
            dropped_lines += laid.dropped_lines;
            // **这一条的颜色**：cue 自己带的覆盖轨道默认。
            let item_color = cue.style.color.unwrap_or(style.color);
            // 高亮色：样式给了才做高亮（没给 = 不做，既有工程不变）。
            let hl_color = style.highlight_color.unwrap_or(item_color);
            for line in laid.lines {
                // **只有真的带标记时才填 `parts`。**
                //
                // 全部 `highlight == false` 时留空 —— 那样渲染侧走的是
                // 原来那条"一次画完"的路，既有工程逐字节不变。
                // 反过来（无标记也填一段）会让每一帧都多一次栅格化。
                let has_mark = line.parts.iter().any(|p| p.highlight);
                // **逐字格子（P3）**：只有这一轨真的配了逐字动效时才拆。
                //
                // ⚠️ 判据走 `PerCharSpec::is_active()` 而**不是** `per_char.is_some()`：
                //    一个"有名字但四个通道端点全相等"的档什么都不会动，
                //    拆开它等于让这一条白白放弃老路的栅格化缓存
                //    （每个字一张位图），而画面**一点变化都没有**。
                // ⚠️ 拆出来的格子在这一层就带上**这一帧**的变换（`local_frame`
                //    口径与上面 `rotation_deg` 那条一致：`frame - element.start`）。
                let cells: Vec<dhampir_timeline::text_perchar::CharCell> =
                    match style.per_char.as_ref().filter(|s| s.is_active()) {
                        None => Vec::new(),
                        Some(spec) => {
                            let local = frame.saturating_sub(element.start);
                            let mut layout =
                                dhampir_timeline::text_perchar::CharLayout::of(&line.text);
                            // **逐字出场窗口**（2026-10-18，用户口径「固定为入场逐字变大、
                            // 出场逐字变小」）：落在出点前 `out_frames`（含逐字错峰）之内就
                            // 走"反着走"那条 —— 同一条 `from/to`、进度倒过来。
                            //
                            // ⚠️ 判据必须**在窗口内**才切：出场那条在窗口之前返回的是"终止值"
                            //    （正常大小），无条件切过去会把入场动画整个盖掉。
                            // ⚠️ 窗口按**帧**算：`span_ms` 经时基换成帧（`num/den` 就是帧率）。
                            //    时基非法（num/den 为 0）⇒ `span_frames = 0` ⇒ 退回入场那条
                            //    （不猜、不 panic —— 契约层输入来自外部）。
                            let count = layout.cells.len();
                            let out_frames = style.per_char_out_frames.unwrap_or(0) as i64;
                            let span_frames = {
                                let fps = if timebase.den > 0 && timebase.num > 0 {
                                    timebase.num as f64 / timebase.den as f64
                                } else {
                                    0.0
                                };
                                if fps > 0.0 {
                                    (span_ms as f64 * fps / 1000.0).round() as i64
                                } else {
                                    0
                                }
                            };
                            let stagger_span =
                                spec.stagger_frames.max(0) * count.saturating_sub(1) as i64;
                            let in_exit = out_frames > 0
                                && span_frames > 0
                                && local >= span_frames - out_frames - stagger_span;
                            let transforms = if in_exit {
                                dhampir_timeline::text_perchar_anim::layout_transforms_at_out(
                                    spec,
                                    &layout,
                                    local,
                                    span_frames,
                                    out_frames,
                                )
                            } else {
                                dhampir_timeline::text_perchar_anim::layout_transforms_at(
                                    spec, &layout, local,
                                )
                            };
                            dhampir_timeline::text_perchar_anim::apply_transforms(
                                &mut layout.cells,
                                &transforms,
                            );
                            layout.cells
                        }
                    };
                let runs: Vec<TextRun> = if has_mark {
                    line.parts
                        .iter()
                        .map(|p| TextRun {
                            text: p.text.clone(),
                            color: if p.highlight { hl_color } else { item_color },
                        })
                        .collect()
                } else {
                    Vec::new()
                };
                // **三个缩放相乘**：
                //   `line.scale`  = 装不下时的整体缩字（布局的事实，改动前就有）
                //   `enter_scale` = T1 入场弹入（窗口外恒 1.0）
                //   `cue.scale`   = T3 逐条强调（None = 1.0）
                // 相乘而不是覆盖：三者是独立来源，"弹入没结束就命中强调"要连续。
                let item_scale = line.scale * enter_scale * cue.scale.unwrap_or(1.0);
                items.push(TextItem {
                    text: line.text,
                    rect: line.rect,
                    opacity,
                    // 抖动**加在**包络位移之上，不是替换它 ——
                    // 两者来源不同（入场包络 vs 抖动），同一帧可以都在。
                    dy_px: dy_px + shake_dy,
                    dx_px: shake_dx,
                    color: item_color,
                    // ⚠️ **字号必须乘上 `item_scale`，否则缩放对像素完全没有影响。**
                    //
                    // 2026-10-11 实测踩到的：`scale` 当时**只**被宿主用来缩描边宽度
                    // （`stroke_px * scale`），而**字形大小**由渲染侧
                    // `place_line(rect, target, font_ratio)` 的 `font_ratio` 决定 ——
                    // 那一路**根本不读 `scale`**。于是 T1 弹入在数据层算得好好的、
                    // 单测全绿，出片里却**一帧都不动**：
                    //
                    //     出片 21 帧逐帧量文字外接框宽度 → 与未开启的基线**一模一样**
                    //     （`f_p_t1` 与 `f_p_base` 全部 102×20，比值恒 1.0000）
                    //
                    // 这正是本仓最怕的那类错：**跑得通、日志正常、结果不对**。
                    //
                    // 修在**求值层**而不是各宿主：两条渲染路（worker 的栅格化、
                    // wasm 宿主）都从这里取 `font_ratio`，在这里乘一次就都盖住了 ——
                    // 放到宿主里就会有两份实现，而"只有一条路生效"的症状
                    // 恰恰是"预览对、成片不对"。
                    //
                    // `font_ratio` 的文档本来就写着它是"**布局算出来的事实**"；
                    // 乘过缩放的才是这一帧真正的事实。
                    font_ratio: line.font_ratio * item_scale,
                    scale: item_scale,
                    parts: runs,
                    // 逐字格子跟着这一条走（空 = 没有逐字效果，宿主走老路）。
                    cells,
                    // 轨道级样式**跟着这一条走**（见 `TextItem::track_style` 的长注释）——
                    // 这里就是"共享槽"那个 bug 的修复点：以前是循环里往一个
                    // `subtitle_style` 上整份重写，于是最后一条轨吃掉前面所有轨。
                    track_style: track_style.clone(),
                    // **文字形变（T1）**：取这一轨**当前活着那个元素**自己的 `transform`。
                    //
                    // ★ **关键帧已接通（2026-10-13）**：这两个值现在**逐帧求值** ——
                    //   静态值是 fallback，键存在时按 `local_frame` 走曲线。
                    //   于是"摇摆（rotation 往复）"这条效果**真的可以做了**。
                    //
                    //   ⚠️ 原先是直接读静态值，注释写着"`evaluate_overlay` 不求值关键帧，
                    //   关键帧求值在 `compose.rs::element_to_draw` 那条路上，与这里**尚未连接**"。
                    //   现在连上了 —— 用的是**同一条** `channel_from`（不是第二份实现），
                    //   所以不会与 GPU 那条路漂移。
                    //
                    //   `local_frame = frame - element.start`：与 `compose.rs:294`
                    //   （`let local = frame - layer.start`）**同一个口径**。
                    //   ⚠️ 用 `element` 自己的 `start`，不是轨的 —— 元素可以晚于轨开始。
                    rotation_deg: channel_from(
                        element.transform.rotation,
                        &element.keyframes,
                        "rotation",
                        frame.saturating_sub(element.start),
                    ),
                    scale_y: channel_from(
                        element.transform.scale_y,
                        &element.keyframes,
                        "scale_y",
                        frame.saturating_sub(element.start),
                    ),
                });
            }
            // ⚠️ 这里原来有一句 `subtitle_style = TextStyle { … }` —— 就是那个 bug。
            // 现在轨道级样式在**进 cue 循环之前**算好（`track_style`），
            // 并跟着每条 item 走；这里只剩"这一帧有字幕"这个布尔账。
            styled = true;
        }
    }

    for track in &timeline.tracks {
        if track.kind != TrackKind::Danmaku {
            continue;
        }
        // 泳道参数在轨道级（一条弹幕轨 = 一份素材 + 一套泳道参数）。
        let Some(spec) = track.danmaku.as_ref() else {
            continue;
        };
        // 与字幕同一条：轨内活着的元素至多一个，且要 enabled。
        let Some(element) = track.layers.iter().find(|layer| layer.covers(frame)) else {
            continue;
        };
        if !element.enabled {
            continue;
        }
        let Some(source) = element.source.as_ref() else {
            continue;
        };
        let Some(cues) = table.get(&source.asset_id) else {
            continue;
        };
        // 泳道分配**整条素材算一次**（与帧无关），再挑这一帧活着的那些。
        // 每帧重算是纯函数的天性；素材几百条时这是 O(条数 × 泳道数)，写进边界。
        let laid: dhampir_timeline::danmaku::DanmakuLayout = layout_danmaku(cues, spec, &timebase);
        dropped_danmaku += laid.dropped;
        // ---------------------------------------------------------------------
        // **这条轨的轨道级样式**（与字幕那半边同一个形状、同一个 bug）。
        // ---------------------------------------------------------------------
        //
        // ⚠️ 它算在**每条轨自己的作用域里**，然后跟着这一轨的每条 `DanmakuTextItem` 走。
        // 以前是写进循环外的**一个共享槽** `danmaku_style`，循环结束只返回最后一份
        // ⇒ **最后一条弹幕轨说了算**，前面所有轨的轨道级样式被整份吃掉。
        // 详见 `DanmakuTextItem::track_style`。
        let track_style = TextStyle {
            color: spec.color,
            // 弹幕没有 `.hl` 标记（参照的弹幕路径也不解析它）。
            highlight_color: None,
            font_ratio: spec.font_ratio,
            outline: spec.outline,
            stroke_px: spec.stroke_ratio * target_h,
            stroke_color: spec.stroke_color,
            // **弹幕没有外圈描边**：`DanmakuSpec` 里没有这个字段（用户口径只提了
            // 字幕与文字元素）。如实给「不画」，而不是跟着字幕走 —— 同阴影那条。
            outer_stroke_px: 0.0,
            outer_stroke_color: [255, 255, 255, 255],
            family: spec.font_family.clone(),
            weight: spec.font_weight,
            // **弹幕恒不画阴影**：`DanmakuSpec` 没有阴影字段（加一个就要动契约，
            // 而"弹幕要不要阴影"没人提过）。这里如实给"不画"，而不是跟着字幕走
            // —— 跟着走的话，给字幕配的阴影会**莫名其妙地**出现在弹幕上。
            shadow_color: None,
            shadow_dx_px: 0.0,
            shadow_dy_px: 0.0,
            shadow_blur_px: 0.0,
            // **弹幕恒贴底那套**（1.0）：`DanmakuSpec` 没有纵向锚字段。
            // 与阴影同一条理由 —— 跟着字幕走的话，给字幕选的"上"会让
            // **弹幕也整体上移**，而弹幕的纵向位置本来就是泳道分配的。
            anchor_bottom: 1.0,
            // **弹幕恒不铺底衬**（T6）：`DanmakuSpec` 没有这两个字段，与阴影
            // 同一条理由 —— 跟着字幕走的话，字幕的底衬会**莫名其妙地**
            // 变成每条滚动弹幕背后的一块色块，那是把画面糊掉。
            plate_color: None,
            plate_pad_px: default_plate_pad_px(),
        };
        for item in laid.items {
            if frame < item.enter || frame > item.exit {
                continue;
            }
            let Some(rect) = rect_at(&item, frame, spec, sequence) else {
                continue;
            };
            // 在屏区间是**闭**的，所以总长要多算一帧的时长。
            let span_ms = ms_between_frames(item.enter, item.exit.saturating_add(1), &timebase);
            let local_ms = ms_between_frames(item.enter, frame, &timebase);
            let (fade, dy_px) = dhampir_timeline::layer::text_envelope(
                local_ms,
                span_ms,
                spec.fade_in_ms,
                spec.fade_out_ms,
                0.0,
                0.0,
            );
            // **基础不透明度乘在淡入淡出之上** —— 参照实现 的弹幕是 `0.9 * fade`。
            danmaku.push(DanmakuTextItem {
                text: item.text,
                rect,
                lane: item.lane,
                enter: item.enter,
                exit: item.exit,
                // **逐条的滚动时长**（`\move(...,t1,t2)` 抽出来的；读不到才是轨道级）。
                travel_frames: item.travel_frames,
                opacity: spec.opacity.clamp(0.0, 1.0) * fade,
                dy_px,
                // **这条自带的颜色**覆盖轨道默认。
                color: item.color.unwrap_or(spec.color),
                font_ratio: spec.font_ratio,
                // 轨道级样式**跟着这一条走**（见 `DanmakuTextItem::track_style`）。
                track_style: track_style.clone(),
            });
        }
        // ⚠️ 这里原来有一整段 `danmaku_style = TextStyle { … }` —— 就是那个 bug。
        // 现在轨道级样式在**进 item 循环之前**算好并跟着每条走；
        // 这里只剩"这一帧有弹幕"这个布尔账。
        danmaku_styled = true;
    }

    // 两半都空才是 None —— 只有弹幕的一帧不能被当成"没有字"。
    let has_subtitle = styled && !items.is_empty();
    let has_danmaku = danmaku_styled && !danmaku.is_empty();
    if !has_subtitle && !has_danmaku {
        return None;
    }
    Some(TextOverlay {
        items,
        danmaku,
        dropped_lines,
        dropped_danmaku,
    })
}

/// 两个帧号之间相差多少毫秒（`to - from`，负数取 0）。
///
/// **用同一份时间基换算**（`ms_at_frame`），不在这里重写一遍除法 ——
/// 重写一遍就是给"两处算法慢慢分叉"另一个机会。
fn ms_between_frames(from: Frame, to: Frame, timebase: &TimebaseDto) -> u64 {
    if to <= from {
        return 0;
    }
    // 单帧毫秒 = den/num * 1000；乘以帧数，全程整数化以免累积浮点误差。
    let num = timebase.num.max(1) as u128;
    let den = timebase.den as u128;
    let frames = (to - from) as u128;
    let ms = frames.saturating_mul(den).saturating_mul(1000) / num;
    ms.min(u64::MAX as u128) as u64
}

/// 这一帧从时间线起点算起过了多少毫秒。
fn ms_at_frame_milli(frame: Frame, timebase: &TimebaseDto) -> Option<u64> {
    if frame < 0 {
        return Some(0);
    }
    Some(ms_between_frames(0, frame, timebase))
}

/// 这条字幕在这一帧显示吗。
///
/// 区间取**闭开** [start_frame, end_frame)：SRT 的结束时间是「什么时候消失」。
/// 短于一帧的字幕（end <= start）仍然在 start 那一帧显示 —— 否则一条都不显示，
/// 而「一条都不显示」看起来和「这条字幕不存在」一样，属于最难查的那类。
///
/// 与 examples/layout_subtitles.rs 里那段判定是**同一套规则**（那边是参照输出）：
/// 两处只要有一处改了规则，参照就不参照了。
pub fn cue_visible_at(cue: &Cue, frame: Frame, timebase: &TimebaseDto) -> bool {
    let Some(start) = frame_at_ms(cue.start_ms, timebase) else {
        return false;
    };
    let Some(end) = frame_at_ms(cue.end_ms, timebase) else {
        return false;
    };
    if end > start {
        frame >= start && frame < end
    } else {
        frame == start
    }
}

/// 这条字幕条活着的**帧区间**（闭区间）。
///
/// 与 [`cue_visible_at`] 是同一套规则（区间闭开、短于一帧仍算一帧）——
/// 只是一个按帧问、一个按区间答。**两边各写一份是会漂的**，
/// 所以有一条测试逐帧比对两者（`帧区间与逐帧判定一致`）。
pub fn cue_frames(cue: &Cue, timebase: &TimebaseDto) -> Option<(Frame, Frame)> {
    let start = frame_at_ms(cue.start_ms, timebase)?;
    let end = frame_at_ms(cue.end_ms, timebase)?;
    Some(if end > start {
        (start, end - 1)
    } else {
        (start, start)
    })
}

/// 一条要写进**侧挂字幕文件**的字幕条：文本 + 它在这段出片区间里出现的帧区间。
///
/// 帧号是**绝对**的（时间线坐标系，不是产物坐标系）：重定基是调用方的事，
/// 因为「产物的第 0 帧是时间线的哪一帧」只有调用方知道。
#[derive(Debug, Clone, PartialEq)]
pub struct OverlaySpan {
    /// 字幕素材 id。**一份侧挂文件只装得下一份素材** —— 哪条来自哪份素材要跟着条目走，
    /// 否则调用方无从发现「这次出片用到了两份」。
    pub asset_id: String,
    pub text: String,
    /// 第一次出现的帧（已经裁到出片区间里）。
    pub first: Frame,
    /// 最后一次出现的帧（含）。
    pub last: Frame,
}

/// 出片区间 `[from, to]` 里，画面上出现过哪些字幕条、各占哪几帧。
///
/// # 与 [`evaluate_overlay`] 的关系
///
/// 同一件事的两种问法：那边问「这一帧画什么」，这边问「这段区间里哪些条目出现过」。
/// 侧挂导出要的是后者 —— 逐帧问也能拼出来，但拼的人自己就得再实现一遍合并规则，
/// 而「哪两帧属于同一条」正是会漂的那部分。
///
/// 两边的规则**必须一致**，所以这里的可见性判定走同一套 [`cue_visible_at`] 的口径
/// （经由 [`cue_frames`]）、元素的覆盖走同一个 `Layer::covers`、
/// 「没样式的字幕轨不参与」也是同一条。测试里逐帧比对两者的结论。
///
/// # 已知边界（不假装）
///
/// * **轨内元素重叠**时这里会把两个元素的条目都列出来，而 `evaluate_overlay`
///   只画第一个覆盖该帧的元素 —— 重叠是校验禁止的（`layer_overlap`），
///   走到这里说明工程已经在校验那关被拦下了。
/// * 不看轨道/元素是不是**被静音**（`enabled` 在元素上，见上：它是参与的）。
/// * 不看 `max_lines`：被丢掉的行在侧挂文件里**仍然在** —— 侧挂文件是「这份素材在这段
///   区间里说了什么」，不是「这一帧的像素里有几个字」。
pub fn overlay_spans(
    timeline: &TimelineV2,
    from: Frame,
    to: Frame,
    subtitles: &SubtitleTable,
) -> Vec<OverlaySpan> {
    let timebase = timeline.timebase;
    let mut spans: Vec<OverlaySpan> = Vec::new();

    for track in &timeline.tracks {
        if track.kind != TrackKind::Subtitle {
            continue;
        }
        if track.subtitle.is_none() {
            // 与 evaluate_overlay 同一条：没有样式的字幕轨不参与（位置与字号都无从谈起）。
            continue;
        }
        for element in &track.layers {
            if !element.enabled {
                continue;
            }
            let Some(source) = element.source.as_ref() else {
                continue;
            };
            let Some(cues) = subtitles.get(&source.asset_id) else {
                continue;
            };
            // 元素自己的覆盖区间（左闭右开）先裁到出片区间里 —— 元素不覆盖的帧，
            // 那条字幕没被画过，就不该出现在侧挂文件里。
            let low = element.start.max(from);
            let high = element.end.saturating_sub(1).min(to);
            if high < low {
                continue;
            }
            for cue in cues {
                let Some((first, last)) = cue_frames(cue, &timebase) else {
                    continue;
                };
                let first = first.max(low);
                let last = last.min(high);
                if last < first {
                    continue;
                }
                spans.push(OverlaySpan {
                    asset_id: source.asset_id.clone(),
                    text: cue.text.clone(),
                    first,
                    last,
                });
            }
        }
    }
    spans
}

#[cfg(test)]
mod tests {
    use super::*;

    const SEQUENCE: (u32, u32) = (640, 360);

    fn timeline(json: &str) -> TimelineV2 {
        serde_json::from_str(json).expect("测试用的时间线 JSON 必须能反序列化")
    }

    /// 一条字幕轨 + 一个覆盖 [0, 120) 的字幕元素。时间基 30/1。
    fn subtitle_timeline() -> TimelineV2 {
        timeline(
            r#"{
                "schema": 3,
                "timebase": { "num": 30, "den": 1 },
                "tracks": [{
                    "id": "sub",
                    "kind": "subtitle",
                    "layers": [{
                        "id": "cue",
                        "start": 0,
                        "end": 120,
                        "source": { "asset_id": "sub.srt", "source_in": 0 }
                    }],
                    "subtitle": {
                        "font_ratio": 0.055,
                        "bottom_margin": 0.06,
                        "max_lines": 2,
                        "color": [255, 240, 200, 255],
                        "outline": false
                    }
                }]
            }"#,
        )
    }

    fn cues() -> Vec<Cue> {
        vec![
            Cue {
                start_ms: 0,
                end_ms: 2000,
                text: "第一行中文".to_string(),
                travel_ms: None,
                scale: None,
                style: Default::default(),
            },
            Cue {
                start_ms: 2000,
                end_ms: 4000,
                text: "第二句".to_string(),
                travel_ms: None,
                scale: None,
                style: Default::default(),
            },
        ]
    }

    fn table() -> SubtitleTable {
        let mut table = SubtitleTable::new();
        table.insert("sub.srt".to_string(), cues());
        table
    }

    /// 与 [`table`] 同，但**第一条**的 `cue.scale` 是给定的（T3 强调）。
    ///
    /// `Cue.scale` 没有"从工程 JSON 读"这条路（它是宿主机给的逐条事实，
    /// 见那个字段的文档），所以 T3 **只能**在这一层用 Rust 构造出来验 ——
    /// 工程文件的 `layers[].scale` 写进去也不会被读到。
    fn table_with_cue_scale(scale: Option<f32>) -> SubtitleTable {
        let mut list = cues();
        list[0].scale = scale;
        let mut table = SubtitleTable::new();
        table.insert("sub.srt".to_string(), list);
        table
    }

    // =====================================================================
    // T1 弹入 / T3 强调：**求值层到 `TextItem` 的整条路**
    // =====================================================================

    /// T1：弹入窗口内 `font_ratio` 真的随帧变化，窗口外回到单位值。
    ///
    /// # ⚠️ 这条判据是被"出片实测"逼出来的
    ///
    /// 一开始只有 `text_motion` 的函数级判据（`scale` 算得对），
    /// 而**出片里文字一帧都不动** —— 因为渲染侧的字号读的是
    /// `place_line(rect, target, style.font_ratio)`，**不读 `scale`**。
    /// 21 帧逐帧量外接框宽度，与未开启的基线**一模一样**（全部 102×20）。
    ///
    /// 所以这条判据**必须盯 `font_ratio`**，不能只盯 `scale` ——
    /// 只盯 `scale` 的话，"算对了但没送到渲染"这个洞它照样漏过去。
    #[test]
    fn 弹入窗口内字号随帧变化而窗口外回到单位() {
        let style_json = r#"{
            "schema": 3,
            "timebase": { "num": 60, "den": 1 },
            "tracks": [{
                "id": "sub",
                "kind": "subtitle",
                "layers": [{
                    "id": "cue", "start": 0, "end": 120,
                    "source": { "asset_id": "sub.srt", "source_in": 0 }
                }],
                "subtitle": {
                    "font_ratio": 0.055, "bottom_margin": 0.06, "max_lines": 2,
                    "color": [255,255,255,255], "outline": false,
                    "scale_in_from": 0.7, "scale_in_peak": 1.15, "scale_in_ms": 180
                }
            }]
        }"#;
        let tl = timeline(style_json);

        // 基线：不开启（另造一份，只有 font_ratio）。
        let base_tl = subtitle_timeline();
        let base = evaluate_overlay(&base_tl, 0, SEQUENCE, Some(&table())).expect("基线要有覆盖层");

        let fr = |t: &TimelineV2, frame: i64| -> Vec<f32> {
            evaluate_overlay(t, frame, SEQUENCE, Some(&table()))
                .expect("要有覆盖层")
                .items
                .iter()
                .map(|i| i.font_ratio)
                .collect()
        };

        let base_fr = base.items[0].font_ratio;
        assert!(
            (fr(&base_tl, 0)[0] - base_fr).abs() < 1e-9,
            "基线（没写弹入字段）的字号必须一点不变"
        );

        // 第 0 帧 = 窗口起点 ⇒ 字号应当是 `from` 倍。
        let f0 = fr(&tl, 0)[0];
        assert!(
            (f0 - base_fr * 0.7).abs() < 1e-4,
            "第 0 帧字号应当是 0.7 倍（{:.6}），实际 {f0:.6}",
            base_fr * 0.7
        );

        // 窗口过半附近应当**超过**基线（过冲）。
        let mut peak = 0.0_f32;
        for frame in 0..12 {
            peak = peak.max(fr(&tl, frame)[0]);
        }
        assert!(
            peak > base_fr * 1.15,
            "窗口内必须出现过冲（字号超过基线的 1.15 倍），实际峰值 {peak:.6} / 基线 {base_fr:.6}"
        );

        // 窗口过了（180ms @60fps = 10.8 帧）⇒ 回到**恰好**基线字号。
        for frame in [11, 12, 30, 119] {
            let got = fr(&tl, frame)[0];
            assert!(
                (got - base_fr).abs() < 1e-6,
                "第 {frame} 帧已过窗口，字号必须回到基线 {base_fr:.6}，实际 {got:.6}"
            );
        }
    }

    /// T3：`cue.scale` **乘进字号**，不开启时一点不影响。
    ///
    /// 这条同样盯 `font_ratio`（理由见上一条）：盯 `scale` 抓不住
    /// "求值层算对、渲染侧没收到"。
    #[test]
    fn 逐条强调缩放乘进字号() {
        let tl = subtitle_timeline();
        let at = |t: &SubtitleTable, frame: i64| {
            evaluate_overlay(&tl, frame, SEQUENCE, Some(t))
                .expect("要有覆盖层")
                .items
        };

        let none = at(&table(), 0);
        let none_fr = none[0].font_ratio;

        // `None` = 1.0（老工程逐字节不变的那条路）。
        assert_eq!(none[0].scale, 1.0, "`cue.scale = None` 必须完全等价于 1.0");

        let x2 = at(&table_with_cue_scale(Some(2.0)), 0);
        assert!(
            (x2[0].font_ratio - none_fr * 2.0).abs() < 1e-5,
            "`cue.scale = 2.0` 必须把字号也乘成两倍：期望 {:.6}，实际 {:.6}",
            none_fr * 2.0,
            x2[0].font_ratio
        );
        assert!((x2[0].scale - 2.0).abs() < 1e-6, "`scale` 字段同时要是 2.0");

        // 第二句（2–4 秒）没有设 scale ⇒ 在**它自己的帧上**不受影响
        // （强调是**逐条**的，不是轨道级的）。
        // 时间基 30/1 ⇒ 第 75 帧 = 2500ms，落在第二句（2000–4000ms）里。
        let second_none = at(&table(), 75);
        let second_x2 = at(&table_with_cue_scale(Some(2.0)), 75);
        assert_eq!(second_none.len(), 1, "第 75 帧应当只有第二句在");
        assert!(
            (second_x2[0].font_ratio - second_none[0].font_ratio).abs() < 1e-9,
            "没设 scale 的那一条不该被动到：{} vs {}",
            second_x2[0].font_ratio,
            second_none[0].font_ratio
        );
        assert!(
            (second_x2[0].scale - 1.0).abs() < 1e-9,
            "没设 scale 的那一条的 `scale` 应当恒为 1.0"
        );
    }

    /// T1 × T3：两个缩放**相乘**，不是覆盖。
    ///
    /// "弹入还没结束就命中高亮"时，若写成覆盖，字会**突然跳一下**
    /// （从包络的 0.7 跳到强调的 2.0）。这条钉住相乘。
    #[test]
    fn 弹入缩放与逐条强调相乘() {
        let style_json = r#"{
            "schema": 3,
            "timebase": { "num": 60, "den": 1 },
            "tracks": [{
                "id": "sub",
                "kind": "subtitle",
                "layers": [{
                    "id": "cue", "start": 0, "end": 120,
                    "source": { "asset_id": "sub.srt", "source_in": 0 }
                }],
                "subtitle": {
                    "font_ratio": 0.055, "bottom_margin": 0.06, "max_lines": 2,
                    "color": [255,255,255,255], "outline": false,
                    "scale_in_from": 0.7, "scale_in_peak": 1.15, "scale_in_ms": 180
                }
            }]
        }"#;
        let tl = timeline(style_json);
        let with_both = table_with_cue_scale(Some(2.0));
        let items = evaluate_overlay(&tl, 0, SEQUENCE, Some(&with_both))
            .expect("要有覆盖层")
            .items;

        // 第 0 帧：包络给 0.7，强调给 2.0 ⇒ 相乘 = 1.4（覆盖的话是 2.0 或 0.7）。
        let base_fr = evaluate_overlay(&subtitle_timeline(), 0, SEQUENCE, Some(&table()))
            .expect("基线")
            .items[0]
            .font_ratio;
        let expected = base_fr * 0.7 * 2.0;
        assert!(
            (items[0].font_ratio - expected).abs() < 1e-4,
            "两个缩放必须**相乘**（期望 {expected:.6}），实际 {:.6}；\
             若等于 {:.6} 说明强调**覆盖**了弹入",
            items[0].font_ratio,
            base_fr * 2.0
        );
    }

    /// T4：抖动**真的落到 `dy_px` / `dx_px` 上**，且关掉时恒为 0。
    ///
    /// # 这条是被变异台补上的
    ///
    /// 原来只有 `text_motion` 的抖动函数级判据 —— 把
    /// `dy_px: dy_px + shake_dy` 改成 `dy_px`（即**抖动不接线**）时，
    /// 那些判据**全部照绿**：它们测的是函数返回值，不是"值有没有被用上"。
    ///
    /// 与 `font_ratio` 那个洞是同一类：**算对了 ≠ 送到了**。
    /// 所以每条动效都必须有一条判据盯"它出现在了 `TextItem` 的哪个字段上"。
    #[test]
    fn 抖动落到偏移字段上而关掉时为零() {
        let mk = |extra: &str| {
            format!(
                r#"{{
                    "schema": 3,
                    "timebase": {{ "num": 60, "den": 1 }},
                    "tracks": [{{
                        "id": "sub", "kind": "subtitle",
                        "layers": [{{ "id": "cue", "start": 0, "end": 240,
                            "source": {{ "asset_id": "sub.srt", "source_in": 0 }} }}],
                        "subtitle": {{
                            "font_ratio": 0.055, "bottom_margin": 0.06, "max_lines": 2,
                            "color": [255,255,255,255], "outline": false{extra}
                        }}
                    }}]
                }}"#
            )
        };

        // 关（没写字段）⇒ 两个偏移都恒为 0。
        let off = timeline(&mk(""));
        for frame in [0_i64, 3, 7, 20] {
            let items =
                evaluate_overlay(&off, frame, SEQUENCE, Some(&table())).expect("要有覆盖层");
            assert_eq!(items.items[0].dx_px, 0.0, "没写抖动 ⇒ 横向偏移恒 0");
            assert_eq!(items.items[0].dy_px, 0.0, "没写抖动 ⇒ 纵向偏移恒 0");
        }

        // 开 ⇒ 至少有一帧、至少一个方向**非零**，且随时间变（不是常量偏置）。
        let on = timeline(&mk(r#", "shake_px": 14.0, "shake_hz": 18.0"#));
        let mut seen_dx = false;
        let mut seen_dy = false;
        let mut vals = std::collections::BTreeSet::new();
        for frame in 0_i64..60 {
            let items = evaluate_overlay(&on, frame, SEQUENCE, Some(&table())).expect("要有覆盖层");
            let it = &items.items[0];
            if it.dx_px.abs() > 1e-4 {
                seen_dx = true;
            }
            if it.dy_px.abs() > 1e-4 {
                seen_dy = true;
            }
            vals.insert((it.dx_px * 1000.0).round() as i64);
        }
        assert!(seen_dx, "开了抖动之后横向偏移必须出现过非零值");
        assert!(seen_dy, "开了抖动之后纵向偏移必须出现过非零值");
        assert!(
            vals.len() > 10,
            "偏移必须随时间**变化**（常量偏置 = 错位，不是抖）；只有 {} 个不同值",
            vals.len()
        );
    }

    /// **字幕轨 + 弹幕轨同时存在**，且两者的颜色刻意不同。
    ///
    /// 这是拆开 `TextOverlay.color` 的那条用例的前提：共用一个字段时，
    /// 这两个颜色**不可能同时成立**。
    fn both_timeline() -> TimelineV2 {
        timeline(
            r#"{
                "schema": 3,
                "timebase": { "num": 30, "den": 1 },
                "tracks": [
                    {
                        "id": "sub",
                        "kind": "subtitle",
                        "layers": [{
                            "id": "cue",
                            "start": 0,
                            "end": 120,
                            "source": { "asset_id": "sub.srt", "source_in": 0 }
                        }],
                        "subtitle": {
                            "font_ratio": 0.055,
                            "bottom_margin": 0.06,
                            "max_lines": 2,
                            "color": [255, 240, 200, 255],
                            "outline": false
                        }
                    },
                    {
                        "id": "dm",
                        "kind": "danmaku",
                        "layers": [{
                            "id": "shots",
                            "start": 0,
                            "end": 120,
                            "source": { "asset_id": "dm.ass", "source_in": 0 }
                        }],
                        "danmaku": {
                            "asset_id": "dm.ass",
                            "lanes": 4,
                            "duration_ms": 2000,
                            "font_ratio": 0.04,
                            "color": [0, 255, 0, 255]
                        }
                    }
                ]
            }"#,
        )
    }

    fn both_table() -> SubtitleTable {
        let mut table = SubtitleTable::new();
        table.insert("sub.srt".to_string(), cues());
        // 弹幕复用同一批 cue：这条用例只看颜色，不看文本内容。
        table.insert("dm.ass".to_string(), cues());
        table
    }

    /// 与 [`both_timeline`] 同形，但**字幕那一边写了阴影**。
    ///
    /// 弹幕那一半没有阴影字段可写 —— 那正是这条夹具要证明的事：
    /// 字幕配了阴影，弹幕**不该跟着有**。
    fn shadow_timeline() -> TimelineV2 {
        timeline(
            r#"{
                "schema": 3,
                "timebase": { "num": 30, "den": 1 },
                "tracks": [
                    {
                        "id": "sub",
                        "kind": "subtitle",
                        "layers": [{
                            "id": "cue",
                            "start": 0,
                            "end": 120,
                            "source": { "asset_id": "sub.srt", "source_in": 0 }
                        }],
                        "subtitle": {
                            "font_ratio": 0.055,
                            "color": [255, 240, 200, 255],
                            "outline": false,
                            "shadow_color": [0, 0, 0, 102],
                            "shadow_dx_px": 3.0,
                            "shadow_dy_px": 2.0,
                            "shadow_blur_ratio": 0.011111111
                        }
                    },
                    {
                        "id": "dm",
                        "kind": "danmaku",
                        "layers": [{
                            "id": "shots",
                            "start": 0,
                            "end": 120,
                            "source": { "asset_id": "dm.ass", "source_in": 0 }
                        }],
                        "danmaku": {
                            "asset_id": "dm.ass",
                            "lanes": 4,
                            "duration_ms": 2000,
                            "font_ratio": 0.04,
                            "color": [0, 255, 0, 255]
                        }
                    }
                ]
            }"#,
        )
    }

    /// **逐字格子真的被求值层填上了**（P3，2026-10-13）。
    ///
    /// ⚠️ 这条是"逐字"整条链的**接缝判据**：`paint_per_char` 只有在
    /// `cells` 非空时才会被调用，而 `cells` 是求值层填的。
    /// 少了这条，逐字功能的失败模式是**静默的** ——
    /// 渲染侧一切正常，只是永远走老路（画面里一个字都不动）。
    #[test]
    fn 配了逐字动效时格子被填上且带变换() {
        let line = timeline(
            r#"{
                "schema": 3,
                "timebase": { "num": 30, "den": 1 },
                "tracks": [{
                    "id": "sub",
                    "kind": "subtitle",
                    "layers": [{
                        "id": "cue",
                        "start": 0,
                        "end": 120,
                        "source": { "asset_id": "sub.srt", "source_in": 0 }
                    }],
                    "subtitle": {
                        "font_ratio": 0.055,
                        "color": [255, 240, 200, 255],
                        "outline": false,
                        "per_char": {
                            "name": "scatter",
                            "offset_x_to": 0.4,
                            "per_char_frames": 10,
                            "stagger_frames": 0,
                            "easing": "linear"
                        }
                    }
                }]
            }"#,
        );
        // 第 60 帧（30fps ⇒ 2s）：`per_char_frames = 10` 早跑完 ⇒ 全部到位。
        let overlay = evaluate_overlay(&line, 60, SEQUENCE, Some(&table())).expect("有字幕");
        let item = &overlay.items[0];
        assert_eq!(
            item.cells.len(),
            item.text.chars().count(),
            "格子数必须等于字数"
        );
        // 每个字都应当已经被挪到 0.4em（全部错峰为 0 ⇒ 同时动完）
        for (i, c) in item.cells.iter().enumerate() {
            assert!(
                (c.offset_x_em - 0.4).abs() < 1e-5,
                "第 {i} 个字的横向位移应当是 0.4em，实得 {}",
                c.offset_x_em
            );
            assert!(!c.is_identity_transform(), "配了动效就不该是恒等");
        }
    }

    /// **没配逐字动效时 `cells` 必须是空的**（R4：走老路、缓存不碎）。
    #[test]
    fn 没配逐字动效时格子是空的() {
        let overlay =
            evaluate_overlay(&subtitle_timeline(), 15, SEQUENCE, Some(&table())).expect("有字幕");
        assert!(
            overlay.items.iter().all(|i| i.cells.is_empty()),
            "没配 `per_char` 时 `cells` 必须为空，否则所有字幕都被迫走逐字路"
        );
    }

    /// **配了一个「什么都不动」的档时也必须是空的**。
    ///
    /// ⚠️ 判据是 `is_active()` 而不是 `is_some()` —— 这条钉的就是那个区别。
    #[test]
    fn 空档的格子也是空的() {
        let line = timeline(
            r#"{
                "schema": 3,
                "timebase": { "num": 30, "den": 1 },
                "tracks": [{
                    "id": "sub",
                    "kind": "subtitle",
                    "layers": [{
                        "id": "cue",
                        "start": 0,
                        "end": 120,
                        "source": { "asset_id": "sub.srt", "source_in": 0 }
                    }],
                    "subtitle": {
                        "font_ratio": 0.055,
                        "color": [255, 240, 200, 255],
                        "outline": false,
                        "per_char": { "name": "nothing", "per_char_frames": 10 }
                    }
                }]
            }"#,
        );
        let overlay = evaluate_overlay(&line, 60, SEQUENCE, Some(&table())).expect("有字幕");
        assert!(
            overlay.items.iter().all(|i| i.cells.is_empty()),
            "一个四个端点全相等的档什么都不会动，不该进逐字路"
        );
    }

    #[test]
    fn 没有字幕表时没有覆盖层() {
        assert!(evaluate_overlay(&subtitle_timeline(), 15, SEQUENCE, None).is_none());
    }

    #[test]
    fn 空表时没有覆盖层() {
        let empty = SubtitleTable::new();
        assert!(evaluate_overlay(&subtitle_timeline(), 15, SEQUENCE, Some(&empty)).is_none());
    }

    #[test]
    fn 活着的字幕给出覆盖层() {
        // 第 15 帧 = 500ms，落在第一条字幕 [0, 2000) 里。
        let overlay = evaluate_overlay(&subtitle_timeline(), 15, SEQUENCE, Some(&table()))
            .expect("这一帧应当有字幕");
        assert_eq!(overlay.items.len(), 1);
        assert_eq!(overlay.items[0].text, "第一行中文");
    }

    /// **结构必须与参照输出逐字段一致。**
    ///
    /// 参照是 dhampir_timeline::text_layout 本身（examples/layout_subtitles.rs 打的也是它）。
    /// 这条测试的意义是：core 只是「挑出这一帧的字幕」，**不许**对结构做任何再加工。
    #[test]
    fn 结构与共享布局逐字段一致() {
        let overlay = evaluate_overlay(&subtitle_timeline(), 15, SEQUENCE, Some(&table())).unwrap();
        let style = dhampir_timeline::layer::SubtitleStyle {
            color: [255, 240, 200, 255],
            outline: false,
            ..Default::default()
        };
        let reference = layout("第一行中文", &style, SEQUENCE);
        assert_eq!(overlay.items.len(), reference.lines.len());
        for (item, line) in overlay.items.iter().zip(reference.lines.iter()) {
            assert_eq!(item.text, line.text);
            assert_eq!(item.rect, line.rect, "矩形必须逐字段相同");
        }
    }

    #[test]
    fn 颜色与描边来自轨道样式() {
        let overlay = evaluate_overlay(&subtitle_timeline(), 15, SEQUENCE, Some(&table())).unwrap();
        assert_eq!(overlay.items[0].track_style.color, [255, 240, 200, 255]);
        assert!(!overlay.items[0].track_style.outline, "轨道样式写了不描边");
    }

    // =========================================================================
    // ★ task-18：**两条字幕轨并存**（这条就是当初漏掉的那条）
    // =========================================================================
    //
    // # 为什么必须有它
    //
    // 这个 bug（轨道级样式被最后一条轨整份覆盖）**需要 ≥2 条轨才可能显形** ——
    // 单轨时"最后一条"就是"唯一一条"，行为完全正确。
    // 而在它被修掉之前，`grep -c "TrackKind::Subtitle"` 在这个文件里**只有 2**
    // ⇒ **所有既有的单测都只放了一条字幕轨**，形状上恰好绕过了它。
    //
    // ⇒ **不是测试写得差，是测试的形状绕过了它。**
    // ⇒ 所以这条判据不是"再补一个 case"，是**补上那个缺掉的形状**。
    //
    // 旁证（YekiTrim 侧实测，同一条二进制、只改"有没有第二条轨"）：
    // | 工程 | 红线（底衬）像素 |
    // |---|---|
    // | 含两条轨 | **0** |
    // | 只留配了样式的那条 | **7676** |
    // 对照组（单轨）**在修复前也是绿的** —— 所以它自己单独抓不到 bug。
    //
    // # 判据怎么立
    //
    // 两条轨给**互不相同**的轨道级字段，且两边的字段都要被检查到：
    // - 轨 A：`plate_color`（T6 底衬）+ 不描边；
    // - 轨 B：`outline` / `stroke_ratio`（且**没有**底衬）。
    //
    // 覆盖 bug 的症状是"**只剩最后一条**" ⇒ 只要两条轨的东西**同时**在、
    // 且各自挂在自己那条 item 上，就说明没有被覆盖。
    #[test]
    fn 两条字幕轨各自的轨道级样式都生效() {
        let line = timeline(
            r#"{
                "schema": 3,
                "timebase": { "num": 30, "den": 1 },
                "tracks": [
                    {
                        "id": "elements",
                        "kind": "subtitle",
                        "layers": [{
                            "id": "cue",
                            "start": 0,
                            "end": 120,
                            "source": { "asset_id": "sub.srt", "source_in": 0 }
                        }],
                        "subtitle": {
                            "font_ratio": 0.055,
                            "bottom_margin": 0.06,
                            "max_lines": 2,
                            "color": [255, 0, 0, 255],
                            "outline": false,
                            "plate_color": [0, 0, 0, 140]
                        }
                    },
                    {
                        "id": "srt",
                        "kind": "subtitle",
                        "layers": [{
                            "id": "cue",
                            "start": 0,
                            "end": 120,
                            "source": { "asset_id": "srt.srt", "source_in": 0 }
                        }],
                        "subtitle": {
                            "font_ratio": 0.055,
                            "bottom_margin": 0.06,
                            "max_lines": 2,
                            "color": [0, 0, 255, 255],
                            "outline": true,
                            "stroke_ratio": 0.02
                        }
                    }
                ]
            }"#,
        );
        let mut table = SubtitleTable::new();
        table.insert("sub.srt".to_string(), cues());
        table.insert(
            "srt.srt".to_string(),
            vec![Cue {
                start_ms: 0,
                end_ms: 2000,
                text: "另一轨".to_string(),
                travel_ms: None,
                scale: None,
                style: Default::default(),
            }],
        );

        let overlay = evaluate_overlay(&line, 15, SEQUENCE, Some(&table)).unwrap();
        assert_eq!(overlay.items.len(), 2, "两条轨各一行，都该在");

        let a = &overlay.items[0]; // 轨 `elements`
        let b = &overlay.items[1]; // 轨 `srt`

        // ① 轨 A 的**底衬**在 —— T6 在真实工程里失效的就是这一项。
        assert_eq!(
            a.track_style.plate_color,
            Some([0, 0, 0, 140]),
            "轨 A 写了 plate_color，它必须在这一条上"
        );
        // ② 轨 A 的**不描边**在 —— 若被轨 B 覆盖，这里会变成 `true`。
        assert!(!a.track_style.outline, "轨 A 写了不描边，不该被轨 B 覆盖");
        assert_eq!(a.track_style.color, [255, 0, 0, 255], "轨 A 的颜色");

        // ③ 轨 B 的**描边**在 —— 若它反被轨 A 覆盖，这里会是 0。
        assert!(b.track_style.outline, "轨 B 写了描边");
        assert!(
            b.track_style.stroke_px > 0.0,
            "轨 B 的 stroke_ratio 要落地成像素，实际 {}",
            b.track_style.stroke_px
        );
        assert_eq!(b.track_style.color, [0, 0, 255, 255], "轨 B 的颜色");
        // ④ 轨 B **没有**底衬 —— 两条轨的样式必须各自独立，
        //    不能出现"一条轨的字段漏到另一条"。
        assert_eq!(
            b.track_style.plate_color, None,
            "轨 B 没写 plate_color，不该沾到轨 A 的"
        );

        // ⑤ 两条轨的样式**互不相同** —— 否则上面那些断言可能只是
        //    "两份恰好一样"，而不是在测覆盖。
        assert_ne!(
            a.track_style, b.track_style,
            "两条轨的样式必须不同，否则这条测试测不出覆盖"
        );
    }

    /// 单轨时与**改动前逐字段相等**（R4 存量零回归）。
    ///
    /// 既有工程绝大多数是单轨，它们必须**一个字段都不变**。
    /// 与上面那条是一对：上面钉"多条轨不复用"，这条钉"单条轨没被改坏"。
    #[test]
    fn 单条字幕轨的样式与改动前逐字段相等() {
        let overlay = evaluate_overlay(&subtitle_timeline(), 15, SEQUENCE, Some(&table())).unwrap();
        // 单轨时"按条取"与"共享槽"本来就该**完全一致** ——
        // 这正是这个 bug 能藏那么久的原因（单轨场景下两者没有区别）。
        let style = &overlay.items[0].track_style;
        assert_eq!(style.color, [255, 240, 200, 255]);
        assert_eq!(style.font_ratio, 0.055);
        assert!(!style.outline);
        assert_eq!(
            style.stroke_px, 0.0,
            "没写 stroke_ratio ⇒ 0（既有工程不变）"
        );
        // 描边色的默认不是纯黑 —— 是 `subtitle_stroke_color()` 的 `[0x40,0x3c,0x3b]`。
        // （我第一版按"想当然"写了 `[0,0,0,255]` 被这条测试当场抓住；
        //   这里照 `dhampir_timeline::layer` 的实际默认值写。）
        assert_eq!(style.stroke_color, [0x40, 0x3c, 0x3b, 255]);
        assert_eq!(style.shadow_color, None, "没写阴影");
        assert_eq!(style.plate_color, None, "没写底衬");
        assert_eq!(style.plate_pad_px, 12.0, "底衬外扩取默认值");
        // 同一帧里每条都该是**同一份**（它们同属一条轨）。
        for item in &overlay.items {
            assert_eq!(&item.track_style, style, "同一条轨的样式必须一致");
        }
    }

    #[test]
    fn 没写描边比例的工程描边宽度与升级前一致() {
        // **这条钉的是"既有工程逐字节不变"。**
        //
        // `subtitle_timeline()` 没写 `stroke_ratio`，于是它取默认值 0；
        // 而 `stroke_px` 算出来必须是 **0**（= "别用契约的值，让栅格器按字号推"）。
        //
        // 我第一版把默认值写成 `12/1080`（参照实现 的实际值），于是这里会算出
        // `12/1080 * 360 = 4`，而老行为是 `border_px(20) = 1` —— 描边粗了 4 倍，
        // 既有工程的产物全变。默认值是**契约的一部分**，不是风格偏好。
        let overlay = evaluate_overlay(&subtitle_timeline(), 15, SEQUENCE, Some(&table())).unwrap();
        assert_eq!(
            overlay.items[0].track_style.stroke_px, 0.0,
            "默认必须是 0（走字号推的老路径）"
        );

        // 显式写一个比例时才算，且按**目标高**换算。
        let mut styled = subtitle_timeline();
        if let Some(style) = styled.tracks[0].subtitle.as_mut() {
            style.stroke_ratio = 12.0 / 1080.0;
        }
        let overlay = evaluate_overlay(&styled, 15, SEQUENCE, Some(&table())).unwrap();
        let expected = 12.0 / 1080.0 * SEQUENCE.1 as f32;
        assert!(
            (overlay.items[0].track_style.stroke_px - expected).abs() < 1e-4,
            "写了的比例要按目标高换算：期望 {expected}，实得 {}",
            overlay.items[0].track_style.stroke_px
        );
    }

    /// **文字阴影**：契约给的是比例，求值层按目标高换算成像素 ——
    /// 与 `stroke_px` 同一处、同一条理由（只有求值层知道目标尺寸）。
    ///
    /// 这条同时把**两种量纲**分开钉住：`shadow_blur_ratio` 是比例（换目标尺寸就差），
    /// `shadow_dx_px` / `shadow_dy_px` 是**文档像素**（换目标尺寸**不**变）。
    /// 只验其中一半的话，"把偏移也当比例算"这种错会一路绿到画面上。
    #[test]
    fn 阴影字段透传且模糊按目标高换算成像素() {
        let overlay = evaluate_overlay(&shadow_timeline(), 15, SEQUENCE, Some(&table()))
            .expect("这一帧应当有字幕");
        let style = &overlay.items[0].track_style;
        assert_eq!(style.shadow_color, Some([0, 0, 0, 102]), "颜色要原样透传");
        assert_eq!(
            (style.shadow_dx_px, style.shadow_dy_px),
            (3.0, 2.0),
            "偏移要原样透传（它们是文档像素，不是比例）"
        );
        // 12/1080 × 360 = 4px。**乘的是求值层拿到的那个高**，与描边同一处。
        let expected = 12.0 / 1080.0 * SEQUENCE.1 as f32;
        assert!(
            (style.shadow_blur_px - expected).abs() < 1e-4,
            "模糊半径要按目标高换算：期望 {expected}，实得 {}",
            style.shadow_blur_px
        );

        // 换一个目标尺寸：**比例换出来不同的像素**，而偏移一动不动。
        let large = evaluate_overlay(&shadow_timeline(), 15, (1920, 1080), Some(&table()))
            .expect("这一帧应当有字幕");
        assert!(
            (large.items[0].track_style.shadow_blur_px - 12.0).abs() < 1e-4,
            "1080 高下 12/1080 应当换出 12px，实得 {}",
            large.items[0].track_style.shadow_blur_px
        );
        assert_eq!(
            large.items[0].track_style.shadow_dx_px, 3.0,
            "偏移是文档像素：它**不**随目标尺寸变（变了就是预览与成片不一致）"
        );
    }

    /// **反向用例**：没写过阴影的工程必须一个阴影都不画。
    ///
    /// 少了"默认 None"，**所有既有工程**都会突然多出一圈阴影（而且它压在所有字上）
    /// —— 那是"看起来只是有点脏"的一类错，最难查。
    #[test]
    fn 没写过阴影的工程一个阴影都不画() {
        let overlay = evaluate_overlay(&subtitle_timeline(), 15, SEQUENCE, Some(&table())).unwrap();
        let style = &overlay.items[0].track_style;
        assert_eq!(style.shadow_color, None);
        assert_eq!(
            (style.shadow_dx_px, style.shadow_dy_px, style.shadow_blur_px),
            (0.0, 0.0, 0.0)
        );
        // `Default` 也必须是不画 —— 它是"没给样式"那条路的兜底。
        assert_eq!(TextStyle::default().shadow_color, None);
    }

    /// **弹幕不跟着字幕画阴影**：`DanmakuSpec` 里没有阴影字段。
    ///
    /// 跟着走（"弹幕复用轨道样式"那种顺手实现）的症状是：给字幕配的阴影
    /// **莫名其妙地出现在弹幕上**，而弹幕本来是要"轻"一点的东西。
    #[test]
    fn 字幕配了阴影弹幕也不画() {
        let overlay = evaluate_overlay(&shadow_timeline(), 15, SEQUENCE, Some(&both_table()))
            .expect("这一帧应当有字");
        assert_eq!(
            overlay.items[0].track_style.shadow_color,
            Some([0, 0, 0, 102])
        );
        assert_eq!(
            overlay.danmaku[0].track_style.shadow_color, None,
            "弹幕不该继承字幕的阴影"
        );
        assert_eq!(overlay.danmaku[0].track_style.shadow_blur_px, 0.0);
    }

    /// ★ **两条弹幕轨并存**时，各自的轨道级样式是否都生效？
    ///
    /// # 为什么补这条（写它的动机本身就是发现）
    ///
    /// task-18 修的是**字幕**那个共享槽。修完写文档时回头核对"弹幕那一边呢"，
    /// 看到弹幕走的是**一模一样的形状**（`danmaku_style` 也是一个共享槽，
    /// 在 `for track in &timeline.tracks` 里每轨整份重写、循环后只留最后一份）。
    /// 而 `grep -c "TrackKind::Danmaku"` 在当时的测试文件里**只有 1**
    /// —— **与字幕那个 bug 是完全相同的盲区形状**。
    ///
    /// # 但它**没有**变成第二个 bug —— 这条判据就是这个结论的证据
    ///
    /// 与字幕同一条道理：**逐条**字段在覆盖发生**之前**就 push 进 items 了。
    /// 弹幕的逐条字段是 `color` / `font_ratio`（`DanmakuTextItem` 上有），
    /// 而宿主画弹幕时**颜色与字号都读逐条的**（`&item.color` / `item.font_ratio`）——
    /// ⇒ 这两项**不会**被覆盖。
    ///
    /// ⚠️ **仍然有暴露面**：`outline` / `stroke_px` / `stroke_color` /
    /// `font_family` / `font_weight` 是**只**在帧级 `danmaku_style` 里读的
    /// （`text_overlay.rs` 画弹幕时传的就是它）。**两条弹幕轨共存**时，
    /// 前面那条轨的这几项**会**被后面那条吃掉 —— 与字幕那个 bug 同形。
    ///
    /// ⇒ 下面第二条断言（描边）就是**钉这个暴露面**的。
    /// **⚠️ 本仓当前没有"两条弹幕轨"的工程**，所以这是**潜在**缺陷而非实测故障；
    /// 真要修，做法与 `TextItem::track_style` 完全一样（把帧级那份也挂到
    /// `DanmakuTextItem` 上）—— 但没有现场证据，**不在 task-18 的范围里**，
    /// 单独开任务做。
    #[test]
    fn 两条弹幕轨各自的轨道级样式都生效() {
        let line = timeline(
            r#"{
                "schema": 3,
                "timebase": { "num": 30, "den": 1 },
                "tracks": [
                    {
                        "id": "dm_a",
                        "kind": "danmaku",
                        "layers": [{
                            "id": "shots",
                            "start": 0,
                            "end": 120,
                            "source": { "asset_id": "dm_a.ass", "source_in": 0 }
                        }],
                        "danmaku": {
                            "asset_id": "dm_a.ass",
                            "lanes": 4,
                            "duration_ms": 2000,
                            "font_ratio": 0.04,
                            "color": [255, 0, 0, 255],
                            "outline": true,
                            "stroke_ratio": 0.05
                        }
                    },
                    {
                        "id": "dm_b",
                        "kind": "danmaku",
                        "layers": [{
                            "id": "shots",
                            "start": 0,
                            "end": 120,
                            "source": { "asset_id": "dm_b.ass", "source_in": 0 }
                        }],
                        "danmaku": {
                            "asset_id": "dm_b.ass",
                            "lanes": 4,
                            "duration_ms": 2000,
                            "font_ratio": 0.04,
                            "color": [0, 0, 255, 255],
                            "outline": false,
                            "stroke_ratio": 0.0
                        }
                    }
                ]
            }"#,
        );
        let mut table = SubtitleTable::new();
        table.insert("dm_a.ass".to_string(), cues());
        table.insert("dm_b.ass".to_string(), cues());

        let overlay = evaluate_overlay(&line, 15, SEQUENCE, Some(&table)).unwrap();
        assert!(!overlay.danmaku.is_empty(), "两条弹幕轨都该有内容");

        // ① **逐条**字段（颜色 / 字号）：两项都不会被覆盖 —— 这是**结论**，不是缺陷。
        let colors: Vec<[u8; 4]> = overlay.danmaku.iter().map(|d| d.color).collect();
        assert!(
            colors.contains(&[255, 0, 0, 255]) && colors.contains(&[0, 0, 255, 255]),
            "两条轨的**颜色**都该在（逐条字段，覆盖发生前已解析）：实际 {colors:?}"
        );

        // ② ★ **轨道级**字段（描边）：这一项**只**在轨道级样式里读 ⇒
        //    两条轨并存时，前面那条的描边**曾经**会被后面那条吃掉（共享槽）。
        //    修完之后，每条 item 拿到的都是**自己那条轨**的样式。
        assert!(
            overlay.danmaku[0].track_style.stroke_px > 0.0,
            "★ 轨 A 配了 `stroke_ratio`（描边），但它拿到的样式里 `stroke_px = {}` \
             —— 被后面那条轨覆盖了（与 task-18 修掉的字幕 bug 同形）。",
            overlay.danmaku[0].track_style.stroke_px
        );
        assert!(
            overlay.danmaku[1].track_style.stroke_px == 0.0,
            "轨 B 没配描边，它的样式里不该有描边"
        );
    }

    #[test]
    fn 字幕与弹幕各有一套颜色() {
        // **这是拆开 `color` 的理由**：参照实现 的字幕是暖色、弹幕是白色，
        // 共用一份时必然有一个错 —— 而"两边的字都看得见"让人以为没问题。
        let overlay =
            evaluate_overlay(&both_timeline(), 15, SEQUENCE, Some(&both_table())).unwrap();
        assert_eq!(
            overlay.items[0].track_style.color,
            [255, 240, 200, 255],
            "字幕用字幕的颜色"
        );
        assert_eq!(
            overlay.danmaku[0].track_style.color,
            [0, 255, 0, 255],
            "弹幕用弹幕的颜色"
        );
        assert_ne!(
            overlay.items[0].track_style.color, overlay.danmaku[0].track_style.color,
            "两者必须能不同 —— 这正是共用一个字段时做不到的事"
        );
    }

    #[test]
    fn 这一帧没有字幕时没有覆盖层() {
        // 第 10 帧 = 333ms 有字幕；这里挑一条**不在范围内**的：
        // 2000ms 之后需要 frame >= 60，所以 59 帧仍是第一条；换一条表来验空。
        let mut table = SubtitleTable::new();
        table.insert(
            "sub.srt".to_string(),
            vec![Cue {
                start_ms: 20_000,
                end_ms: 22_000,
                text: "很久以后".to_string(),
                travel_ms: None,
                scale: None,
                style: Default::default(),
            }],
        );
        assert!(evaluate_overlay(&subtitle_timeline(), 15, SEQUENCE, Some(&table)).is_none());
    }

    #[test]
    fn 字幕区间是闭开的() {
        let timebase = TimebaseDto { num: 30, den: 1 };
        let cue = Cue {
            start_ms: 2000,
            end_ms: 4000,
            text: "x".to_string(),
            travel_ms: None,
            scale: None,
            style: Default::default(),
        };
        // 30fps：2000ms -> 第 60 帧，4000ms -> 第 120 帧。
        assert!(!cue_visible_at(&cue, 59, &timebase), "还没到");
        assert!(cue_visible_at(&cue, 60, &timebase), "起点包含");
        assert!(cue_visible_at(&cue, 119, &timebase), "最后一帧还在");
        assert!(!cue_visible_at(&cue, 120, &timebase), "终点不包含");
    }

    #[test]
    fn 短于一帧的字幕仍然显示一帧() {
        let timebase = TimebaseDto { num: 30, den: 1 };
        // 3ms 的间隔在 30fps 下取整之后是同一帧 —— 不能一条都不显示。
        let cue = Cue {
            start_ms: 1000,
            end_ms: 1003,
            text: "x".to_string(),
            travel_ms: None,
            scale: None,
            style: Default::default(),
        };
        assert!(cue_visible_at(&cue, 30, &timebase));
    }

    #[test]
    fn 超出最大行数时丢弃并计数() {
        let mut table = SubtitleTable::new();
        table.insert(
            "sub.srt".to_string(),
            vec![Cue {
                start_ms: 0,
                end_ms: 2000,
                text: "一\n二\n三\n四\n五".to_string(),
                travel_ms: None,
                scale: None,
                style: Default::default(),
            }],
        );
        let overlay = evaluate_overlay(&subtitle_timeline(), 15, SEQUENCE, Some(&table)).unwrap();
        assert_eq!(overlay.items.len(), 2, "max_lines 是 2");
        assert_eq!(overlay.dropped_lines, 3, "丢了多少行必须如实计数");
    }

    #[test]
    fn 视频轨上的同名字幕素材不参与() {
        // 把轨的 kind 换成 video：同一个表、同一帧，不该给出覆盖层。
        let video = timeline(
            r#"{
                "schema": 3,
                "timebase": { "num": 30, "den": 1 },
                "tracks": [{
                    "id": "v",
                    "kind": "video",
                    "layers": [{
                        "id": "c",
                        "start": 0,
                        "end": 120,
                        "source": { "asset_id": "sub.srt", "source_in": 0 }
                    }]
                }]
            }"#,
        );
        assert!(evaluate_overlay(&video, 15, SEQUENCE, Some(&table())).is_none());
    }

    #[test]
    fn 没有样式的字幕轨不参与() {
        // 轨上没写 subtitle 样式：位置与字号都无从谈起，所以不画，而不是猜一套默认值。
        let no_style = timeline(
            r#"{
                "schema": 3,
                "timebase": { "num": 30, "den": 1 },
                "tracks": [{
                    "id": "sub",
                    "kind": "subtitle",
                    "layers": [{
                        "id": "cue",
                        "start": 0,
                        "end": 120,
                        "source": { "asset_id": "sub.srt", "source_in": 0 }
                    }]
                }]
            }"#,
        );
        assert!(evaluate_overlay(&no_style, 15, SEQUENCE, Some(&table())).is_none());
    }

    #[test]
    fn 同样输入给同样输出() {
        let a = evaluate_overlay(&subtitle_timeline(), 15, SEQUENCE, Some(&table()));
        let b = evaluate_overlay(&subtitle_timeline(), 15, SEQUENCE, Some(&table()));
        assert_eq!(a, b, "评估必须确定性，否则两端结构谈不上一致");
    }

    #[test]
    fn 不同宽高比下归一化宽度不同() {
        // 字号按高度算、宽度按宽度归一化 —— 所以 4:3 里同一段字占比更宽。
        let wide = evaluate_overlay(&subtitle_timeline(), 15, (640, 360), Some(&table())).unwrap();
        let tall = evaluate_overlay(&subtitle_timeline(), 15, (480, 360), Some(&table())).unwrap();
        assert!(
            tall.items[0].rect.width > wide.items[0].rect.width,
            "4:3 里同一行的归一化宽度应当更大：{:?} vs {:?}",
            tall.items[0].rect.width,
            wide.items[0].rect.width
        );
    }

    // ---- 侧挂导出（T2.7）：两种问法必须说的是同一件事 ----

    fn cue(start_ms: u64, end_ms: u64, text: &str) -> Cue {
        Cue {
            start_ms,
            end_ms,
            text: text.to_string(),
            travel_ms: None,
            scale: None,
            style: Default::default(),
        }
    }

    /// 一个带样式的字幕轨（两个紧邻、不重叠的元素）、一条**没样式**的轨、
    /// 以及一个**关掉**的元素。后两者是反向用例：它们都不该出现在侧挂导出里。
    fn spans_timeline() -> TimelineV2 {
        timeline(
            r#"{
                "schema": 3,
                "timebase": { "num": 30, "den": 1 },
                "tracks": [
                    { "id": "sub", "kind": "subtitle",
                      "layers": [
                        { "id": "e1", "start": 0, "end": 120, "source": { "asset_id": "sub.srt", "source_in": 0 } },
                        { "id": "e2", "start": 120, "end": 240, "source": { "asset_id": "sub.srt", "source_in": 0 } }
                      ],
                      "subtitle": { "font_ratio": 0.055, "bottom_margin": 0.06, "max_lines": 2 } },
                    { "id": "nos", "kind": "subtitle",
                      "layers": [{ "id": "n1", "start": 0, "end": 240, "source": { "asset_id": "other.srt", "source_in": 0 } }] },
                    { "id": "off", "kind": "subtitle",
                      "layers": [{ "id": "o1", "start": 0, "end": 240, "enabled": false,
                                   "source": { "asset_id": "other.srt", "source_in": 0 } }],
                      "subtitle": { "font_ratio": 0.055, "bottom_margin": 0.06, "max_lines": 2 } }
                ]
            }"#,
        )
    }

    /// 四条**单字**字幕（不会换行、不会被 max_lines 丢 —— 于是
    /// `evaluate_overlay` 某一帧的 items 正好是「那一刻活着的条」，可以逐帧对账）
    /// 加上另一份素材的字幕。
    fn spans_table() -> SubtitleTable {
        let mut table = SubtitleTable::new();
        table.insert(
            "sub.srt".to_string(),
            vec![
                cue(0, 2000, "甲"),
                cue(2000, 4000, "乙"),
                cue(4000, 6000, "丙"),
                cue(6000, 8000, "丁"),
            ],
        );
        table.insert("other.srt".to_string(), vec![cue(0, 8000, "别的")]);
        table
    }

    /// **这一条才是把两种问法钉在一起的东西。** 逐帧问 `evaluate_overlay`
    /// （画面里出现的字），与按区间问 `overlay_spans`（侧挂导出要写的字）对账。
    ///
    /// 「没样式的轨不参与」「关掉的元素不参与」都由这条顺手盯住 ——
    /// 那两条规则只要有一边改了口径，对账立刻红。
    #[test]
    fn 侧挂导出的区间与画面里出现的字一致() {
        let timeline = spans_timeline();
        let table = spans_table();
        let (from, to) = (0, 300);
        let spans = overlay_spans(&timeline, from, to, &table);
        assert!(!spans.is_empty(), "这份时间线上有字幕，不该一条都算不出来");

        for frame in from..=to {
            let mut want: Vec<&str> = spans
                .iter()
                .filter(|span| span.first <= frame && frame <= span.last)
                .map(|span| span.text.as_str())
                .collect();
            want.sort_unstable();
            let overlay = evaluate_overlay(&timeline, frame, SEQUENCE, Some(&table));
            let mut got: Vec<&str> = overlay
                .as_ref()
                .map(|overlay| {
                    overlay
                        .items
                        .iter()
                        .map(|item| item.text.as_str())
                        .collect()
                })
                .unwrap_or_default();
            got.sort_unstable();
            assert_eq!(want, got, "第 {frame} 帧：侧挂导出与画面必须说的是同一件事");
        }
        assert!(
            spans.iter().all(|span| span.asset_id == "sub.srt"),
            "没样式的轨与关掉的元素都不该进来：{spans:?}"
        );
    }

    #[test]
    fn 出片区间外的条不进侧挂导出() {
        let timeline = spans_timeline();
        let table = spans_table();
        // 只出「乙」那一段：侧挂导出里就只该有它。
        let spans = overlay_spans(&timeline, 60, 119, &table);
        assert_eq!(spans.len(), 1, "{spans:?}");
        assert_eq!(
            (spans[0].text.as_str(), spans[0].first, spans[0].last),
            ("乙", 60, 119)
        );
        // 区间把一条从中间切开时，报的是它**在这段区间里**出现的帧，不是整条的帧。
        let spans = overlay_spans(&timeline, 30, 89, &table);
        assert_eq!(spans.len(), 2, "{spans:?}");
        assert_eq!(
            (spans[0].text.as_str(), spans[0].first, spans[0].last),
            ("甲", 30, 59)
        );
        assert_eq!(
            (spans[1].text.as_str(), spans[1].first, spans[1].last),
            ("乙", 60, 89)
        );
    }

    #[test]
    fn 元素没覆盖的帧不算它出现过() {
        let timeline = timeline(
            r#"{
                "schema": 3,
                "timebase": { "num": 30, "den": 1 },
                "tracks": [{
                    "id": "sub", "kind": "subtitle",
                    "layers": [{ "id": "e", "start": 0, "end": 30, "source": { "asset_id": "sub.srt", "source_in": 0 } }],
                    "subtitle": { "font_ratio": 0.055, "bottom_margin": 0.06, "max_lines": 2 }
                }]
            }"#,
        );
        let table = spans_table();
        let spans = overlay_spans(&timeline, 0, 239, &table);
        // 「甲」到第 59 帧才结束，但元素只覆盖 [0,30) —— 第 30 帧起画面上没有它。
        assert_eq!(spans.len(), 1, "{spans:?}");
        assert_eq!(
            (spans[0].text.as_str(), spans[0].first, spans[0].last),
            ("甲", 0, 29)
        );
        assert!(evaluate_overlay(&timeline, 30, SEQUENCE, Some(&table)).is_none());
    }

    #[test]
    fn 短于一帧的条在侧挂导出里仍占一帧() {
        let mut table = SubtitleTable::new();
        table.insert("sub.srt".to_string(), vec![cue(1000, 1003, "闪一下")]);
        let spans = overlay_spans(&spans_timeline(), 0, 239, &table);
        // 1000..1003ms 在 30fps 下取整是同一帧 —— 不能一条都不出现。
        assert_eq!(spans.len(), 1, "{spans:?}");
        assert_eq!((spans[0].first, spans[0].last), (30, 30));
    }

    #[test]
    fn 空表时侧挂导出也是空的() {
        assert!(overlay_spans(&spans_timeline(), 0, 239, &SubtitleTable::new()).is_empty());
    }

    #[test]
    fn 时间基坏掉时不算出现过() {
        // num 为 0 的工程是坏的（校验会拦），但这里也不能装作算得出来。
        let mut bad = spans_timeline();
        bad.timebase = TimebaseDto { num: 0, den: 1 };
        assert!(overlay_spans(&bad, 0, 239, &spans_table()).is_empty());
    }

    #[test]
    fn 帧区间与逐帧判定一致() {
        // 约 29.97fps：两种问法在非整数帧率下最容易分叉。
        let timebase = TimebaseDto {
            num: 30000,
            den: 1001,
        };
        for (start, end) in [(0_u64, 2000_u64), (1000, 1003), (500, 500), (33, 34)] {
            let item = cue(start, end, "x");
            let (first, last) = cue_frames(&item, &timebase).expect("时间基合法");
            for frame in (first - 5)..=(last + 5) {
                assert_eq!(
                    cue_visible_at(&item, frame, &timebase),
                    frame >= first && frame <= last,
                    "cue {start}..{end} 在第 {frame} 帧：逐帧判定与帧区间必须同答"
                );
            }
        }
    }

    // =====================================================================
    // T1：文字条级 `rotation_deg` / `scale_y`（倾斜 + 非等比压缩）
    // =====================================================================

    /// 一条字幕轨 + 一个覆盖 `[0, 120)` 的元素，元素 `transform` 按参数给。
    ///
    /// ⚠️ 旋转/压扁写在**元素自己的 `transform`**（`layers[i].transform`），
    /// **不是**轨道级 `subtitle` 样式 —— 那正是本组的判据之一。
    fn tilted_timeline(rotation: f32, scale_y: f32) -> TimelineV2 {
        timeline(&format!(
            r#"{{
                "schema": 3,
                "timebase": {{ "num": 30, "den": 1 }},
                "tracks": [
                    {{
                        "id": "sub",
                        "kind": "subtitle",
                        "layers": [{{
                            "id": "cue",
                            "start": 0,
                            "end": 120,
                            "source": {{ "asset_id": "sub.srt", "source_in": 0 }},
                            "transform": {{
                                "x": 0.0, "y": 0.0,
                                "scale": 1.0,
                                "rotation": {rotation},
                                "scale_y": {scale_y}
                            }}
                        }}],
                        "subtitle": {{ "font_ratio": 0.05 }}
                    }}
                ]
            }}"#
        ))
    }

    /// **倾斜真的透传到 `TextItem`**：写 12° ⇒ 这一条就是 12°。
    #[test]
    fn 文字条带旋转角度() {
        let tl = tilted_timeline(12.0, 1.0);
        let overlay = evaluate_overlay(&tl, 0, SEQUENCE, Some(&table())).expect("要有覆盖层");
        assert!(!overlay.items.is_empty(), "至少一条字幕");
        for item in &overlay.items {
            assert_eq!(item.rotation_deg, 12.0, "元素写的 12° 必须逐值到达条目");
        }
    }

    /// **非等比压缩真的透传**：写 0.6 ⇒ 这一条纵向 0.6 倍。
    #[test]
    fn 文字条带纵向倍数() {
        let tl = tilted_timeline(0.0, 0.6);
        let overlay = evaluate_overlay(&tl, 0, SEQUENCE, Some(&table())).expect("要有覆盖层");
        for item in &overlay.items {
            assert_eq!(item.scale_y, 0.6, "元素写的 0.6 必须逐值到达条目");
        }
    }

    /// ⚠️ **没有写这两个键的老工程 ⇒ 恒 `0.0` / `1.0`**（= 老行为，逐字节不变）。
    ///
    /// 这条与上面两条是**一对**：只测"写了生效"会漏掉"没写时会不会自己变成别的值"，
    /// 而缺省值错了症状是**所有老工程的文字一起歪掉 / 压扁**。
    #[test]
    fn 没写旋转与纵向倍数时是老行为() {
        let tl = subtitle_timeline();
        let overlay = evaluate_overlay(&tl, 0, SEQUENCE, Some(&table())).expect("要有覆盖层");
        assert!(
            !overlay.items.is_empty(),
            "夹具本身要有字幕，否则这条判据是空转"
        );
        for item in &overlay.items {
            assert_eq!(item.rotation_deg, 0.0, "没写 ⇒ 0°（不转）");
            assert_eq!(item.scale_y, 1.0, "没写 ⇒ 1.0（纵向跟随 scale = 等比）");
        }
    }

    // =====================================================================
    // 摇摆：`rotation` 的**逐帧求值**（2026-10-13）
    //
    // 这一组把「文字形变能不能被关键帧驱动」钉死 —— 它是"摇摆"效果的地基：
    // 昨天这里读的是**静态值**，所以关键帧写了也**不生效**（不报错、只是没动）。
    // =====================================================================

    /// 一条字幕轨 + 带 `rotation` 关键帧的元素（往复摆动 = 摇摆）。
    ///
    /// 关键帧：0 帧 → −8°，30 帧 → +8°，60 帧 → −8°（一个来回）。
    /// 时间基 30/1 ⇒ 30 帧 = 1 秒。
    fn sway_timeline() -> TimelineV2 {
        timeline(
            r#"{
                "schema": 3,
                "timebase": { "num": 30, "den": 1 },
                "tracks": [{
                    "id": "sub",
                    "kind": "subtitle",
                    "layers": [{
                        "id": "cue",
                        "start": 0,
                        "end": 120,
                        "source": { "asset_id": "sub.srt", "source_in": 0 },
                        "transform": { "x": 0.0, "y": 0.0, "scale": 1.0, "rotation": 0.0, "scale_y": 1.0 },
                        "keyframes": [
                            { "frame": 0,  "target": "rotation", "value": -8.0, "easing": "linear" },
                            { "frame": 30, "target": "rotation", "value":  8.0, "easing": "linear" },
                            { "frame": 60, "target": "rotation", "value": -8.0, "easing": "linear" }
                        ]
                    }],
                    "subtitle": { "font_ratio": 0.05 }
                }]
            }"#,
        )
    }

    /// ★ **`rotation` 关键帧真的逐帧求值**（摇摆的地基）。
    ///
    /// ⚠️ 判据必须是**多个帧各不相同**：只测一帧的话，
    /// "读静态值 0.0" 与"求值恰好也 0.0"分不开。
    /// 这里取 0 / 15 / 30 三帧 —— 线性曲线上是 −8 / 0 / +8。
    #[test]
    fn 旋转关键帧逐帧求值() {
        let tl = sway_timeline();
        let at = |frame| {
            let overlay =
                evaluate_overlay(&tl, frame, SEQUENCE, Some(&table())).expect("每一帧都要有覆盖层");
            overlay.items.first().expect("至少一条字幕").rotation_deg
        };

        assert_eq!(at(0), -8.0, "第 0 帧应是曲线起点 −8°");
        assert_eq!(at(30), 8.0, "第 30 帧应是曲线峰值 +8°");
        // 中点：线性插值 ⇒ 0°。⚠️ 这条是"真的在求值"的关键 ——
        // 读静态值的话三帧**全是 0.0**，只有这一条能分辨出来。
        assert_eq!(at(15), 0.0, "第 15 帧应是 +8/−8 的中点 0°");
        // 后半程往回摆（往复 = 摇摆）。
        assert_eq!(at(60), -8.0, "第 60 帧应摆回 −8°");
    }

    /// ⚠️ **反向**：没有 `rotation` 关键帧时**必须仍是静态值**（老工程逐字节不变）。
    ///
    /// 与上面那条是一对：只测"写了生效"会漏掉"没写时会不会自己变成别的值"，
    /// 而那个症状是**所有老工程的文字一起漂移**。
    #[test]
    fn 没有旋转关键帧时仍用静态值() {
        // 静态 12°，**不带 keyframes**。
        let tl = tilted_timeline(12.0, 1.0);
        for frame in [0, 15, 30, 60, 119] {
            let overlay =
                evaluate_overlay(&tl, frame, SEQUENCE, Some(&table())).expect("每一帧都要有覆盖层");
            for item in &overlay.items {
                assert_eq!(
                    item.rotation_deg, 12.0,
                    "第 {frame} 帧：没写键就该恒用静态值 12°"
                );
            }
        }
    }

    /// ⚠️ **关键帧作用在自己那个元素上**：`local_frame = frame − element.start`。
    ///
    /// 元素若晚于轨开始（`start = 30`），第 30 帧才是它的**第 0 帧** ——
    /// 用轨的 `start` 会整体错位（症状：动画提前/延后一段，肉眼像"没对齐"）。
    #[test]
    fn 关键帧按元素自己的起点算() {
        let tl = timeline(
            r#"{
                "schema": 3,
                "timebase": { "num": 30, "den": 1 },
                "tracks": [{
                    "id": "sub",
                    "kind": "subtitle",
                    "layers": [{
                        "id": "cue",
                        "start": 30,
                        "end": 120,
                        "source": { "asset_id": "sub.srt", "source_in": 0 },
                        "transform": { "x": 0.0, "y": 0.0, "scale": 1.0, "rotation": 0.0, "scale_y": 1.0 },
                        "keyframes": [
                            { "frame": 0,  "target": "rotation", "value": -8.0, "easing": "linear" },
                            { "frame": 30, "target": "rotation", "value":  8.0, "easing": "linear" }
                        ]
                    }],
                    "subtitle": { "font_ratio": 0.05 }
                }]
            }"#,
        );
        let at = |frame| {
            evaluate_overlay(&tl, frame, SEQUENCE, Some(&table()))
                .expect("覆盖层在")
                .items
                .first()
                .expect("一条字幕")
                .rotation_deg
        };
        // 元素第 30 帧才出现 ⇒ 那一刻是它的第 0 帧 ⇒ 曲线起点 −8°。
        assert_eq!(
            at(30),
            -8.0,
            "元素自己的第 0 帧 = 时间线第 30 帧，应取曲线起点"
        );
        assert_eq!(
            at(60),
            8.0,
            "元素自己的第 30 帧 = 时间线第 60 帧，应取曲线终点"
        );
    }

    /// ⚠️ **形变是「条级」的，不是轨级**：两条字幕轨各写各的，互不污染。
    /// 这正是 `TextItem::track_style` 那个"共享槽"bug 的形状：
    /// 轨级共享槽会让**最后一条轨说了算**，而单条轨时完全正确 ——
    /// 所以判据必须**两条轨并存**，否则它永远绿。
    #[test]
    fn 两条字幕轨各自的形变互不污染() {
        let tl = timeline(
            r#"{
                "schema": 3,
                "timebase": { "num": 30, "den": 1 },
                "tracks": [
                    {
                        "id": "sub_a",
                        "kind": "subtitle",
                        "layers": [{
                            "id": "cue_a", "start": 0, "end": 120,
                            "source": { "asset_id": "sub.srt", "source_in": 0 },
                            "transform": { "x": 0.0, "y": 0.0, "scale": 1.0, "rotation": 10.0, "scale_y": 1.0 }
                        }],
                        "subtitle": { "font_ratio": 0.05 }
                    },
                    {
                        "id": "sub_b",
                        "kind": "subtitle",
                        "layers": [{
                            "id": "cue_b", "start": 0, "end": 120,
                            "source": { "asset_id": "sub.srt", "source_in": 0 },
                            "transform": { "x": 0.0, "y": 0.0, "scale": 1.0, "rotation": 30.0, "scale_y": 0.5 }
                        }],
                        "subtitle": { "font_ratio": 0.05 }
                    }
                ]
            }"#,
        );
        let overlay = evaluate_overlay(&tl, 0, SEQUENCE, Some(&table())).expect("要有覆盖层");
        // 两条轨各出一条（cue 文本相同，靠角度区分是哪条轨的）
        let angles: Vec<f32> = overlay.items.iter().map(|i| i.rotation_deg).collect();
        assert!(
            angles.contains(&10.0),
            "第一条轨的 10° 必须还在，实际：{angles:?}"
        );
        assert!(
            angles.contains(&30.0),
            "第二条轨的 30° 必须也在，实际：{angles:?}"
        );
        assert_eq!(angles.len(), 2, "两条轨各一条，不许被后一条吃掉");
        let ys: Vec<f32> = overlay.items.iter().map(|i| i.scale_y).collect();
        assert!(
            ys.contains(&1.0) && ys.contains(&0.5),
            "纵向倍数同样各归各：{ys:?}"
        );
    }

    /// ⚠️ **关键帧尚未连接** —— 这条把"已知限制"钉成判据，防止有人误以为已经支持。
    ///
    /// 元素上写了 `rotation` 关键帧，但 `evaluate_overlay` 不求值 ⇒
    /// 条目拿到的仍是**静态值**。若哪天求值层打通了，这条会红 ——
    /// **那时应该红**：它提醒你去更新 `TextItem::rotation_deg` 的注释与
    /// `docs/plan/text-option-d-scope.md`，而不是删掉这条测试。
    /// ★ **关键帧已连接** —— 这条**原本是反向的**（钉"已知限制"）。
    ///
    /// # 它的来历（别把它改回去）
    ///
    /// 前一版是 `关键帧尚未连接_条目取的是静态值`，断言第 60 帧仍拿静态值 5.0，
    /// 并在注释里写着：
    ///
    /// > 若哪天求值层打通了，这条会红 —— **那时应该红**：它提醒你去更新
    /// > `TextItem::rotation_deg` 的注释与 `docs/plan/text-option-d-scope.md`，
    /// > 而不是删掉这条测试。
    ///
    /// 2026-10-13 接通关键帧时它**确实红了**（`left: 45.0, right: 5.0`）——
    /// 那条绊线按设计生效了。于是按它的嘱咐**更新成正向判据**（不是删掉）：
    /// 同一份夹具、同一帧，现在必须取到**曲线值 45.0**。
    ///
    /// ⇒ 摇摆（rotation 往复）的地基在这一层已经通了。
    #[test]
    fn 关键帧已连接_条目取的是曲线值() {
        let tl = timeline(
            r#"{
                "schema": 3,
                "timebase": { "num": 30, "den": 1 },
                "tracks": [
                    {
                        "id": "sub",
                        "kind": "subtitle",
                        "layers": [{
                            "id": "cue", "start": 0, "end": 120,
                            "source": { "asset_id": "sub.srt", "source_in": 0 },
                            "transform": { "x": 0.0, "y": 0.0, "scale": 1.0, "rotation": 5.0, "scale_y": 1.0 },
                            "keyframes": [
                                { "target": "rotation", "frame": 0, "value": 5.0, "easing": "linear" },
                                { "target": "rotation", "frame": 60, "value": 45.0, "easing": "linear" }
                            ]
                        }],
                        "subtitle": { "font_ratio": 0.05 }
                    }
                ]
            }"#,
        );
        let overlay = evaluate_overlay(&tl, 60, SEQUENCE, Some(&table())).expect("要有覆盖层");
        for item in &overlay.items {
            assert_eq!(
                item.rotation_deg, 45.0,
                "关键帧已连接：第 60 帧应取曲线值 45.0（静态 5.0 只是 fallback）"
            );
        }
        // ⚠️ 反向仍要立着：静态值**不许**被无条件丢掉 —— 第 0 帧的曲线值恰好
        //    也是 5.0，所以另取中点验"确实是插值出来的"。
        let mid = evaluate_overlay(&tl, 30, SEQUENCE, Some(&table())).expect("要有覆盖层");
        for item in &mid.items {
            assert_eq!(item.rotation_deg, 25.0, "第 30 帧应是 5→45 的中点 25.0");
        }
    }
}
