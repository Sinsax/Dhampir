//! 弹幕：解析、**共享泳道分配**与滚动落点。
//!
//! # 为什么泳道分配必须共享
//!
//! 弹幕与字幕最大的不同：字幕的落点是**静态**的（一条字幕一行字，位置由共享布局算好），
//! 而弹幕**从右滚到左**，位置是时间的函数。但「哪一条在第几泳道」「它在哪几帧活着」
//! 是**结构**，不是像素 —— 两端各写一份分配算法，就一定会漂成两张不同的弹幕表，
//! 而"两端各自的表都自洽"这件事让人查不出问题。
//!
//! 所以：分配与落点都只在本模块实现一份，宿主只负责把字**画**进给定的矩形里。
//! 这与 `text_layout`（字幕的行盒）是同一条分工。
//!
//! # 时间口径：在屏时长来自 [`DanmakuSpec`]，不是素材的 End
//!
//! 一条弹幕在屏多久由 `duration_ms` 说了算，素材里 Dialogue 的 `End` **不参与**。
//! 这不是偷懒：同一条弹幕素材配 6 秒还是 8 秒是**播放器的参数**（弹幕池的常态），
//! 而 ASS 的 `End` 是语法的必填位、多数工具随手填。让素材的 End 生效，等于让
//! "这份素材是从别处抓来的"决定这一趟出片的观感。
//!
//! 进入帧取素材的 `Start`（毫秒按序列时间基换成帧号，与字幕同一条换算），
//! 离开帧取 `Start + duration_ms`。**两者都算不出来时那一条被丢弃并计数**。
//!
//! # 落点是时间的函数，不是结构的一部分
//!
//! [`DanmakuItem`] 里**没有**矩形：一帧只有一个答案，把矩形写进结构会让"同一份素材
//! 在两端给出同一张表"变成"两端各自算一次矩形"。矩形由 [`rect_at`] 现算。
//!
//! # 归一化矩形依赖**宽高比**（与 T1 口径一致）
//!
//! 字号是"目标高度 × `font_ratio`"，所以归一化行盒高与尺寸无关；而文本的归一化宽度
//! 是"像素宽 ÷ 目标宽"，于是它依赖**宽高比** —— 同一段文字在 4:3 与 16:9 里归一化宽度
//! 不同。这不是 bug，是 T1 确立的口径：归一化承诺的是"与渲染目标尺寸无关"，
//! 不是"与一切无关"。

use crate::layer::DanmakuSpec;
use crate::schema::{Frame, TimebaseDto};
use crate::subtitle::{AssStyle, Cue, ParseReport, format_ass_time, frame_at_ms, ms_at_frame};
use crate::text_layout::{LINE_HEIGHT_EM, NormalizedRect, measure_em};

/// 解析一份弹幕素材（ASS）。
///
/// 弹幕素材就是 ASS：一条 `Dialogue` 一行弹幕。[`crate::subtitle::parse_ass`] 的
/// **覆盖标签一律不进文本**这条正好是这里要的 —— 包括 `\move(x1,y1,x2,y2)`：
/// 那些坐标是素材作者写的，而落点由本模块按 [`DanmakuSpec`] 与目标尺寸统一算
/// （见 [`rect_at`]）。让素材的坐标参与布局，两端就会各读出一套，
/// 而"两端一致"正是这一层存在的理由。
///
/// 单独一个入口而不是直接叫 `parse_ass`：名字就是口径。下一个人看到
/// `parse_ass_danmaku` 不会以为弹幕素材要走另一条解析路，也不会以为 `\move` 的
/// 坐标有意义 —— 而这两件事都值得写下来。
pub fn parse_ass_danmaku(text: &str) -> Result<ParseReport, String> {
    crate::subtitle::parse_ass(text)
}

/// 一条弹幕的**结构**：文本 + 泳道 + 在屏帧区间。
///
/// 这三样与渲染目标尺寸无关（进入/离开帧只依赖时间基），所以两端拿同一份素材
/// 算出来的必须逐字段相同。滚动位置**不在这里**，见 [`rect_at`]。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DanmakuItem {
    pub text: String,
    /// 第几泳道。0 是**最上面**那条。
    pub lane: u32,
    /// 第一次出现的帧（闭区间起点）。
    pub enter: Frame,
    /// 最后一次出现的帧（**闭**区间终点，与 `subtitle::cue_frames` 同款）。
    pub exit: Frame,
    /// **这条弹幕自带的颜色**（ASS 的 `\c&HBBGGRR&`），`None` = 用轨道默认。
    ///
    /// 它在**结构**里而不是在"画法"里：一条弹幕有没有自己的颜色，
    /// 与"泳道分到几号"一样是**素材本身的事实**，不是渲染目标的事。
    ///
    /// 实测需要它：V-Trim 的 `danmaku.json` 里同一条轨上的
    /// `还能续约吗` 是 `#E33FFF`，其余是白色。
    pub color: Option<[u8; 4]>,
    /// **起滚的帧**（= `enter` + 淡入帧数）。
    ///
    /// 与 [`Self::enter`] 分开：V-Trim 的滚动是 `progress = (el - fadeIn) / travel`
    /// —— **淡入期间 x 钉在右边缘**（`x = CW`），淡入走完才开始滚。
    /// 把这两件事合成一个就会让弹幕"一进场就在滚"。
    pub scroll_start: Frame,
    /// **滚完要多少帧**（`travel`）。滚到左边就**停住**（V-Trim 的 `min(progress,1)`）。
    ///
    /// 为什么不在这里存毫秒：`rect_at` 是**不拿时间基**的纯函数
    /// （两端拿同一份素材、同一组帧号就必须算出同一个矩形）。
    /// 毫秒→帧的换算在 [`layout`] 里做掉，那是唯一拿得到时间基的地方。
    pub travel_frames: i64,
}

/// 泳道分配的结果。**丢掉的条数是结论的一部分**，不是日志。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DanmakuLayout {
    pub items: Vec<DanmakuItem>,
    /// 泳道排不下（或 `lanes == 0`）、以及时间算不出来而**丢掉**的条数。
    ///
    /// 丢掉必须计数：「弹幕少了几条」看起来和「素材里就那几条」一样，
    /// 而那是最难查的一类 —— 画面本身完全正常。
    pub dropped: usize,
}

/// 给一批弹幕条分配泳道。
///
/// # 规则（三条，都是确定的）
///
/// 1. **顺序即优先级**：按 `cues` 的顺序处理。`parse_ass_danmaku` 交出来的已经按
///    起点排好序（同起点保持素材顺序），所以"确定性"不是靠再排一次序，而是靠
///    "不偷偷改变调用方给的顺序"。
/// 2. **不叠**：一条泳道只有在"上一条已经离开"之后才能复用 —— 判据是
///    `上一条的 exit < 这一条的 enter`。闭区间的边界**不能**共用，否则两条会在
///    同一帧同时出现在同一泳道上，而叠在一起的弹幕两条都看不清。
/// 3. **排不下就丢并计数**，不叠在一起。
///
/// `lanes == 0` 时一条也排不进（分配不到泳道就是画不出来），全部丢弃并计数。
pub fn layout(cues: &[Cue], spec: &DanmakuSpec, timebase: &TimebaseDto) -> DanmakuLayout {
    let mut result = DanmakuLayout::default();
    // 每条泳道"最后被占用到的帧"。`lanes == 0` 时它是空的 —— 下面一条也选不出泳道。
    let mut free_at: Vec<Frame> = vec![Frame::MIN; spec.lanes as usize];
    for cue in cues {
        let Some(enter) = frame_at_ms(cue.start_ms, timebase) else {
            result.dropped += 1;
            continue;
        };
        // # 在屏窗口 = **cue 自己的** [start, end)，不是 `start + travel`
        //
        // V-Trim 的 `getActiveDms` 第一行就是 `if (t < d.start || t >= d.end) continue;`
        // —— `travel` 只决定**滚多快**（`progress = (el - fadeIn) / travel`）。
        //
        // 这条先前写错了：`exit` 取的是 `start + spec.duration_ms`（travel），
        // 于是一条 cue 窗口 5 秒、travel 10.4 秒的弹幕会在屏上**待 10.4 秒**。
        // 实测 55s 那一帧：V-Trim 什么都没有，本仓还挂着品红的「还能续约吗」。
        let Some(end_exclusive) = frame_at_ms(cue.end_ms, timebase) else {
            result.dropped += 1;
            continue;
        };
        // 闭区间：`end` 是开边界，最后在屏的一帧是它前一帧。
        // 时长为 0（或时间基碎到算不出前进）仍然让它占**一帧**。
        let exit = end_exclusive.saturating_sub(1).max(enter);
        // 淡入走完才起滚（V-Trim 的 `el - fadeIn`）。
        let fade_frames = frames_for_ms(spec.fade_in_ms, cue.start_ms, timebase);
        let scroll_start = enter.saturating_add(fade_frames);
        // **滚动时长优先用这条 cue 自己的**（`\move(...,t1,t2)` 的 `t2-t1`），
        // 读不到才回退到轨道级的 `duration_ms`。
        //
        // V-Trim 的 `progress = (el - fadeIn) / travel`，而 **`travel` 逐条不同**
        // （它按文本字节数算：实测四条 20.21 / 16.37 / 16.80 / 14.24 秒）。
        // 轨道级那一个值只能取平均，于是**长句滚得太快、短句滚得太慢** ——
        // 症状是"同一时刻参照的弹幕在左边、本仓的还在右边"。
        //
        // 回退不是权宜：老素材（3 参数 `\move` 或 `\pos`）本来就只该有轨道级口径，
        // 有它才保证"既有工程逐字节不变"。
        let travel_ms = cue.travel_ms.unwrap_or(spec.duration_ms);
        let travel_frames = frames_for_ms(travel_ms, cue.start_ms, timebase);
        let mut chosen = None;
        for (index, free) in free_at.iter_mut().enumerate() {
            if *free < enter {
                *free = exit;
                chosen = Some(index as u32);
                break;
            }
        }
        match chosen {
            Some(lane) => {
                result.items.push(DanmakuItem {
                    text: cue.text.clone(),
                    lane,
                    enter,
                    exit,
                    // 这条 cue 自带的颜色（`parse_ass` 从 `\c` 抽出来的）。
                    color: cue.style.color,
                    scroll_start,
                    travel_frames,
                });
            }
            None => result.dropped += 1,
        }
    }
    result
}

/// 一段**时长**（毫秒）换成帧数。
///
/// 用 `frame_at_ms(base + ms) - frame_at_ms(base)` 而不是自己乘：
/// 时间基是**有理数**（`1001/30000` 这种），自己乘会在长时长上累积误差，
/// 而"同一段时长在时间轴不同位置换出不同帧数"是两端最容易漂的地方。
fn frames_for_ms(ms: u64, base_ms: u64, timebase: &TimebaseDto) -> i64 {
    let (Some(from), Some(to)) = (
        frame_at_ms(base_ms, timebase),
        frame_at_ms(base_ms.saturating_add(ms), timebase),
    ) else {
        return 0;
    };
    to.saturating_sub(from).max(0)
}

/// 这一帧这条弹幕占的归一化矩形（相对**文档坐标系**）。
///
/// # 横向：从右滚到左
///
/// 进入的那一帧**左边缘**在画面右边缘（`x = 1`），离开的那一帧整条刚好移出左边
/// （`x = -width`），中间**线性**。线性不是美学选择，是"可复算"的选择：
/// 两端都不用去猜缓动曲线，验收也比对得起。
///
/// # 纵向：泳道决定
///
/// `y = lane × 归一化行盒高`，0 在画面最上面。泳道多到超出画面时 `y > 1` 是正常的
/// —— 与字幕的"负 x 表示落在画面外"同款：算得出来就出数，越界的部分由宿主数出来。
///
/// 目标尺寸或行盒高为 0 时给 `None`：**没有可画的东西**（不是"画失败"）。
pub fn rect_at(
    item: &DanmakuItem,
    frame: Frame,
    spec: &DanmakuSpec,
    sequence: (u32, u32),
) -> Option<NormalizedRect> {
    if sequence.0 == 0 || sequence.1 == 0 {
        return None;
    }
    let height = spec.font_ratio * LINE_HEIGHT_EM;
    if !height.is_finite() || height <= 0.0 {
        return None;
    }
    let width = measure_em(&item.text) * spec.font_ratio * (sequence.1 as f32 / sequence.0 as f32);
    if !width.is_finite() {
        return None;
    }
    let span = item.travel_frames;
    // # 滚动进度由 **travel** 决定，不由"在屏区间"决定
    //
    // 这两件事先前被混成一个：`exit` 曾经等于 `enter + duration_ms`，
    // 于是滚动正好铺满在屏区间 —— **看起来自洽**。
    //
    // 但 V-Trim 是分开的（`templates/index.html` 的 `getActiveDms`）：
    //
    //     if (t < d.start || t >= d.end) continue;          // 在屏窗口 = cue 自己的
    //     var progress = max(0, (el - fadeIn) / travel);    // travel 只决定滚多快
    //     var x = CW - min(progress, 1) * (CW + textW);     // 滚到左边就停
    //
    // 混起来之后，一条 cue 窗口 5 秒、travel 10.4 秒的弹幕会在屏上**待 10.4 秒**
    // —— 实测 55s 那一帧：V-Trim 什么都没有，本仓还挂着一条品红的
    // 「还能续约吗」（它的 cue 早在 50.03s 就结束了）。
    let progress = if span > 0 {
        ((frame - item.scroll_start) as f32 / span as f32).clamp(0.0, 1.0)
    } else {
        // travel 为 0（没写时长）：钉在右边缘。滚不动好过"瞬移"。
        0.0
    };
    let x = 1.0 + (-width - 1.0) * progress;
    // # 泳道带：`lane_top_ratio` + `lane * lane_spacing_ratio`
    //
    // 默认（两个都是 0）就是老规则：**0 号泳道贴着画面最上面**、
    // 间距取行盒高（`font_ratio * LINE_HEIGHT_EM`）—— 既有工程的产物一字不变。
    //
    // 实测需要可配：V-Trim 的弹幕带从 **0.0781** 开始、间距 **0.0521**
    // （它的 `TRACK_YS = [300, 500, 700…]` 在 1920 宽的日志坐标里，
    // 换算是 `y / 3840`）。本仓老规则是 0 起、间距 0.048 ——
    // 差值是**肉眼可见的一整条**（1080p 下约 84px）。
    let spacing = if spec.lane_spacing_ratio > 0.0 {
        spec.lane_spacing_ratio
    } else {
        height
    };
    let y = spec.lane_top_ratio + item.lane as f32 * spacing;
    Some(NormalizedRect { x, y, width, height })
}

/// 把一批弹幕**写成 ASS**（供旁挂导出），每条带 `\move`。
///
/// # 为什么必须由这一层写
///
/// `\move(x1,y1,x2,y2)` 的四个数是**像素**，而像素落点正好是 [`rect_at`] 算出的
/// 归一化矩形乘上目标尺寸。让调用方各自去算，两端就会各写一套像素 ——
/// 而"同一份素材在两端给出同一张表"正是这一层存在的理由。
///
/// # 写得进去的是什么
///
/// * **时间**：起止帧换成毫秒，并**重定基**到 `base`（这一趟产物的第 0 帧）。
///   与侧挂字幕（CLI 的 `sidecar_text`）同一条口径：终点取"最后一帧的下一个起点"，
///   因为 ASS 的 `End` 说的是"什么时候消失"。
/// * **位置**：进入帧的左边缘在**右边缘**（`x = 序列宽`），离开帧整条移出左边
///   （`x = -文本宽`），纵向按泳道。用 `\an7` 把锚点定在左上角，
///   这样 `\move` 的两个数就是 [`rect_at`] 的 `x`/`y`（左边缘、上边缘），
///   不会因为播放器的默认锚点在中心而整体偏半条。
/// * **字号**：**忽略传入 `style` 的 `font_size`**，改用 `font_ratio × 序列高`。
///   `\move` 的终点是按 `font_ratio` 算的，字号要是另一个来源，
///   文件里的字就会比画面上的字大或小，而横滚的终点会跟着一起错。
///
/// **排不下的条目不在这里**：它们在 [`layout`] 那一步就已经被丢掉了，`items` 里没有
/// 它们。所以写出来的条数就是留下来的条数 —— 丢掉的那几条由 `DanmakuLayout::dropped`
/// 报，不由这份文件报。
///
/// 条目顺序 = `items` 的顺序（[`layout`] 已按起点排好），这里**不再排一次**：
/// 再排一次就得给"同一起点"定一个第二判据，而那个判据只在这一处存在。
pub fn to_ass_danmaku(
    items: &[DanmakuItem],
    spec: &DanmakuSpec,
    base: Frame,
    timebase: &TimebaseDto,
    sequence: (u32, u32),
    style: &AssStyle,
) -> Result<String, String> {
    let broken = || format!("时间基坏掉（{}/{}），算不出弹幕的时间", timebase.num, timebase.den);
    let base_ms = ms_at_frame(base, timebase).ok_or_else(broken)?;
    let font_size = spec.font_ratio * sequence.1 as f32;
    if !font_size.is_finite() || font_size <= 0.0 {
        return Err(format!(
            "弹幕字号算出来是 {font_size}（font_ratio {} × 序列高 {}）—— 字号是 0 就没有东西可画",
            spec.font_ratio, sequence.1
        ));
    }
    let font_size = font_size.round() as u32;
    let seq_w = sequence.0 as f32;

    let mut out = crate::subtitle::ass_header(&style.font, font_size, style.margin_v);
    out.push_str("[Events]\n");
    out.push_str(&format!("Format: {}\n", crate::subtitle::ASS_FIELDS.join(", ")));
    for item in items {
        let start = ms_at_frame(item.enter, timebase).ok_or_else(broken)?;
        let end = ms_at_frame(item.exit.saturating_add(1), timebase).ok_or_else(broken)?;
        let start = u64::try_from(start - base_ms)
            .map_err(|_| format!("弹幕的时间算出来是负的（第 {} 帧）", item.enter))?;
        let mut end = u64::try_from(end - base_ms)
            .map_err(|_| format!("弹幕的时间算出来是负的（第 {} 帧）", item.exit))?;
        // 一帧比 1ms 还短时这里表示不出来；至少不让终点落在起点之前 ——
        // 那样的条目有的播放器直接丢掉。
        if end <= start {
            end = start + 1;
        }
        let enter_rect = rect_at(item, item.enter, spec, sequence).ok_or_else(|| {
            format!(
                "算不出第 {} 帧这条弹幕的落点（序列 {}x{} 或字号为 0）",
                item.enter, sequence.0, sequence.1
            )
        })?;
        let exit_rect = rect_at(item, item.exit, spec, sequence).ok_or_else(|| {
            format!(
                "算不出第 {} 帧这条弹幕的落点（序列 {}x{} 或字号为 0）",
                item.exit, sequence.0, sequence.1
            )
        })?;
        let x1 = round_px(enter_rect.x * seq_w)?;
        let y = round_px(enter_rect.y * sequence.1 as f32)?;
        let x2 = round_px(exit_rect.x * seq_w)?;
        // 文本里的换行在 ASS 里是反斜杠 + N（与 `to_ass` 同一处理）。
        let body = item.text.replace('\n', "\\N");
        out.push_str(&format!(
            "Dialogue: 0,{},{},Default,,0,0,0,,{{\\an7\\move({x1},{y},{x2},{y})}}{body}\n",
            format_ass_time(start),
            format_ass_time(end),
        ));
    }
    Ok(out)
}

/// 归一化坐标换成 ASS 要的整数像素。不是有限数就是"算不出落点"。
fn round_px(value: f32) -> Result<i64, String> {
    if !value.is_finite() {
        return Err(format!("弹幕的落点算出来是 {value}，不是有限数"));
    }
    Ok(value.round() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subtitle::CueStyle;

    fn tb(num: u32, den: u32) -> TimebaseDto {
        TimebaseDto { num, den }
    }

    /// 一条弹幕条：起点给毫秒，`End` 默认落在 1 秒后。
    ///
    /// **`End` 参与在屏窗口**（V-Trim 的 `getActiveDms` 用 `[start, end)`）——
    /// 这条注释先前写的是"反正不参与在屏时长"，而那个前提是错的。
    fn cue(start_ms: u64, text: &str) -> Cue {
        Cue {
            travel_ms: None,
            start_ms,
            end_ms: start_ms + 1000,
            text: text.to_string(),
            style: CueStyle::default(),
        }
    }

    /// 同上，但**带上这一条自己的 travel**（`\move(...,t1,t2)` 抽出来的）。
    fn cue_travel(start_ms: u64, travel_ms: u64, text: &str) -> Cue {
        Cue {
            travel_ms: Some(travel_ms),
            ..cue(start_ms, text)
        }
    }

    /// 同上，但显式给 `End`（要测窗口长度时用）。
    fn cue_span(start_ms: u64, end_ms: u64, text: &str) -> Cue {
        Cue {
            travel_ms: None,
            start_ms,
            end_ms,
            text: text.to_string(),
            style: CueStyle::default(),
        }
    }

    fn spec(lanes: u32, duration_ms: u64) -> DanmakuSpec {
        DanmakuSpec { asset_id: "d".to_string(), lanes, duration_ms, ..DanmakuSpec::default() }
    }

    const SEQUENCE: (u32, u32) = (640, 360);
    /// **滚动时长用这条 cue 自己的，不是轨道级的。**
    ///
    /// V-Trim 的 `progress = (el - fadeIn) / travel`，而 **`travel` 逐条不同**
    /// （它按文本字节数算）。轨道级那一个值只能取平均，于是长句滚得太快、短句滚得太慢。
    ///
    /// 这条用例盯的是 `layout` 里那一行的**取值来源** —— 先前没有用例，
    /// 把 `cue.travel_ms` 换回 `spec.duration_ms` 也**不会红**。
    #[test]
    fn 滚动时长用_cue_自己的而不是轨道级的() {
        let spec = spec(4, 20_000); // 轨道级 20 秒
        let tb = tb(60, 1);
        // 两条 cue，同一个轨道级规格，但各自的 travel 不同。
        let cues = vec![cue_travel(0, 5_000, "短"), cue_travel(0, 10_000, "长")];
        let out = layout(&cues, &spec, &tb);
        assert_eq!(out.items.len(), 2, "两条都该排到泳道");
        // 60fps：5 秒 = 300 帧、10 秒 = 600 帧。轨道级是 20 秒 = 1200 帧。
        let mut travels: Vec<i64> = out.items.iter().map(|i| i.travel_frames).collect();
        travels.sort();
        assert_eq!(
            travels,
            vec![300, 600],
            "必须逐条用自己的 travel（轨道级会给出两个 1200）"
        );
        // 没有 travel 的那条回退到轨道级。
        let fallback = layout(&vec![cue(0, "没写")], &spec, &tb);
        assert_eq!(fallback.items[0].travel_frames, 1200, "读不到就该回退到轨道级");
    }


    #[test]
    fn 弹幕素材里的_move_坐标不进文本() {
        // 素材作者写的坐标不参与布局 —— 泳道由 spec 统一分配。
        let text = "[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n\
            Dialogue: 0,0:00:01.00,0:00:05.00,Default,,0,0,0,,{\\move(1920,20,-200,20)}路过一下\n\
            Dialogue: 0,0:00:02.00,0:00:06.00,Default,,0,0,0,,普通一条\n";
        let report = parse_ass_danmaku(text).expect("能解析");
        assert_eq!(report.cues.len(), 2);
        assert_eq!(report.cues[0].text, "路过一下", "覆盖标签（含 \\move）不该进文本");
        assert_eq!(report.cues[1].text, "普通一条");
    }

    #[test]
    fn 在屏窗口来自素材的_end_而不是_spec_的时长() {
        // **这条先前是反着写的，而且写错了。**
        //
        // 老版本叫「在屏时长来自 spec 而不是素材的 end」，断言 `exit == 240`
        // （= `start + spec.duration_ms`），还写着"素材的 End 不该生效"。
        //
        // V-Trim 的源码正相反（`templates/index.html` 的 `getActiveDms` 第一行）：
        //
        //     if (t < d.start || t >= d.end) continue;
        //
        // **在屏窗口是 cue 自己的 [start, end)**；`travel` 只决定滚多快。
        // 老写法让"cue 窗口 5 秒、travel 10.4 秒"的弹幕在屏上待 10.4 秒 ——
        // 实测 55s 那一帧 V-Trim 什么都没有，本仓还挂着一条。
        //
        // 30fps：素材 End=5000ms（第 150 帧）是开边界，最后在屏的是第 149 帧。
        // 而 spec 的 8000ms **不该**拉长窗口。
        let cues = vec![cue_span(0, 5000, "久一点")];
        let laid = layout(&cues, &spec(8, 8000), &tb(30, 1));
        assert_eq!(laid.items.len(), 1);
        assert_eq!(laid.items[0].enter, 0);
        assert_eq!(laid.items[0].exit, 149, "在屏窗口取 cue 的 end（开边界减一）");
        // 而 travel 仍然来自 spec：8 秒 @30fps = 240 帧。
        assert_eq!(laid.items[0].travel_frames, 240, "滚动时长仍然来自 spec");
        assert!(laid.items[0].exit < laid.items[0].travel_frames, "窗口比 travel 短是常态");
        assert_eq!(laid.dropped, 0);
    }

    #[test]
    fn 同一起点只有前两条进得去() {
        // 两条泳道、三条同时进入：前两条各占一条，第三条排不下 -> 丢弃并计数。
        let cues = vec![cue(0, "甲"), cue(0, "乙"), cue(0, "丙")];
        let laid = layout(&cues, &spec(2, 8000), &tb(30, 1));
        assert_eq!(laid.items.len(), 2);
        assert_eq!(laid.dropped, 1, "排不下要计数：少了几条看起来和素材里就那几条一样");
        assert_eq!(laid.items[0].lane, 0);
        assert_eq!(laid.items[1].lane, 1);
    }

    #[test]
    fn 上一条离开之后泳道才复用() {
        // 一条泳道。第一条占 [0, 240]；第二条起点在第 300 帧之后 -> 复用第 0 泳道。
        let cues = vec![cue(0, "先"), cue(10_000, "后")];
        let laid = layout(&cues, &spec(1, 8000), &tb(30, 1));
        assert_eq!(laid.items.len(), 2);
        assert_eq!(laid.dropped, 0);
        assert_eq!(laid.items[0].lane, 0);
        assert_eq!(laid.items[1].lane, 0, "上一条走了以后泳道可以复用");
    }

    #[test]
    fn 闭区间边界不许共用() {
        // 第一条占 [0, 29]（cue 的 End=1000ms，开边界 -> 最后在屏第 29 帧）。
        // 第二条正好在第 30 帧进入？不 —— 让第二条**在第 29 帧进入**，
        // 那样两条在第 29 帧同时在屏，必须换泳道。
        let cues = vec![cue(0, "先"), cue(967, "后")];
        let laid = layout(&cues, &spec(2, 8000), &tb(30, 1));
        assert_eq!(laid.items[0].exit, 29, "在屏窗口来自 cue 的 End（开边界减一）");
        assert_eq!(laid.items[1].enter, 29, "第 29 帧两条都在屏上");
        assert_eq!(laid.items[1].lane, 1, "边界帧不能共用泳道，否则两条叠在一起");
    }

    #[test]
    fn 零泳道全部丢弃并计数() {
        let cues = vec![cue(0, "甲"), cue(100, "乙")];
        let laid = layout(&cues, &spec(0, 8000), &tb(30, 1));
        assert!(laid.items.is_empty());
        assert_eq!(laid.dropped, 2, "一条也画不出来要如实计数");
    }

    #[test]
    fn 分配是确定的() {
        let cues = vec![cue(0, "甲"), cue(0, "乙"), cue(500, "丙"), cue(500, "丁")];
        let once = layout(&cues, &spec(2, 8000), &tb(30, 1));
        let twice = layout(&cues, &spec(2, 8000), &tb(30, 1));
        assert_eq!(once, twice, "同一输入两次分配必须逐字段相同");
    }

    #[test]
    fn 坏时间基不猜而是丢弃并计数() {
        let cues = vec![cue(0, "甲")];
        let laid = layout(&cues, &spec(8, 8000), &tb(0, 1));
        assert!(laid.items.is_empty());
        assert_eq!(laid.dropped, 1);
    }

    #[test]
    fn 进入帧在右边缘离开帧移出左边() {
        let item = DanmakuItem { text: "abc".to_string(), lane: 0, enter: 0, exit: 100 , color: None, scroll_start: 0, travel_frames: 100};
        let start = rect_at(&item, 0, &spec(8, 8000), SEQUENCE).expect("能算");
        assert_eq!(start.x, 1.0, "进入的那一帧左边缘在画面右边缘");
        let end = rect_at(&item, 100, &spec(8, 8000), SEQUENCE).expect("能算");
        // `1.0 + (-w - 1.0)` 与 `-w` 在浮点下差一个 ULP，按容差比。
        assert!((end.x - (-end.width)).abs() < 1e-6, "离开的那一帧整条刚好移出左边");
        // 中间线性：第 50 帧在两者的正中。
        let middle = rect_at(&item, 50, &spec(8, 8000), SEQUENCE).expect("能算");
        assert!((middle.x - (start.x + end.x) / 2.0).abs() < 1e-6, "滚动不是线性的");
    }

    #[test]
    fn 区间外的帧被夹到两端() {
        // 首末两帧之外（调用方过滤前）也给出确定答案，而不是外推。
        let item = DanmakuItem { text: "abc".to_string(), lane: 2, enter: 10, exit: 20 , color: None, scroll_start: 0, travel_frames: 100};
        let before = rect_at(&item, 0, &spec(8, 8000), SEQUENCE).expect("能算");
        // 用**真正越界**的帧（travel 是 100 帧）：进度夹到 1，整条刚好移出左边。
        let after = rect_at(&item, 200, &spec(8, 8000), SEQUENCE).expect("能算");
        assert_eq!(before.x, 1.0);
        assert!((after.x - (-after.width)).abs() < 1e-6);
    }

    #[test]
    fn 纵向由泳道决定且在最上面起算() {
        let item = DanmakuItem { text: "x".to_string(), lane: 3, enter: 0, exit: 10 , color: None, scroll_start: 0, travel_frames: 100};
        let rect = rect_at(&item, 0, &spec(8, 8000), SEQUENCE).expect("能算");
        // font_ratio 默认 0.04 -> 行盒高 0.048；第 3 条泳道在 3 倍处。
        let height = 0.04 * LINE_HEIGHT_EM;
        assert!((rect.height - height).abs() < 1e-6);
        assert!((rect.y - 3.0 * height).abs() < 1e-6, "0 号泳道在最上面，往下顺排");
    }

    #[test]
    fn 泳道带可以配起算点与间距() {
        // **默认（两个都是 0）必须复现老行为** —— 既有工程一字不变。
        let plain = spec(8, 8000);
        let item = DanmakuItem { text: "x".to_string(), lane: 2, enter: 0, exit: 10, color: None, scroll_start: 0, travel_frames: 100 };
        let base = rect_at(&item, 0, &plain, SEQUENCE).expect("能算");
        assert!((base.y - 2.0 * (0.04 * LINE_HEIGHT_EM)).abs() < 1e-6, "默认是 0 起、间距取行盒高");

        // 配了之后按配的算（V-Trim 的实测值：0.0781 起、0.0521 间距）。
        let mut banded = spec(8, 8000);
        banded.lane_top_ratio = 300.0 / 3840.0;
        banded.lane_spacing_ratio = 200.0 / 3840.0;
        let shifted = rect_at(&item, 0, &banded, SEQUENCE).expect("能算");
        let want = 300.0 / 3840.0 + 2.0 * (200.0 / 3840.0);
        assert!((shifted.y - want).abs() < 1e-6, "期望 {want}，实得 {}", shifted.y);
        assert!(shifted.y > base.y, "配了起算点之后应当更低");
        // **行盒高不受影响**：带的位置与字号是两件事。
        assert!((shifted.height - base.height).abs() < 1e-6);
    }

    #[test]
    fn 泳道间距为零时退回行盒高而不是叠在一起() {
        // 间距写 0 的语义是"用行盒高"，不是"所有泳道都重叠在第 0 行"。
        let mut zero = spec(8, 8000);
        zero.lane_top_ratio = 0.1;
        zero.lane_spacing_ratio = 0.0;
        let item = DanmakuItem { text: "x".to_string(), lane: 1, enter: 0, exit: 10, color: None, scroll_start: 0, travel_frames: 100 };
        let rect = rect_at(&item, 0, &zero, SEQUENCE).expect("能算");
        let want = 0.1 + 1.0 * (0.04 * LINE_HEIGHT_EM);
        assert!((rect.y - want).abs() < 1e-6, "期望 {want}，实得 {}", rect.y);
    }

    #[test]
    fn 归一化宽度依赖宽高比而不是渲染尺寸() {
        let item = DanmakuItem { text: "半角abc".to_string(), lane: 0, enter: 0, exit: 10 , color: None, scroll_start: 0, travel_frames: 100};
        let a = rect_at(&item, 0, &spec(8, 8000), (640, 360)).expect("能算");
        let b = rect_at(&item, 0, &spec(8, 8000), (1280, 720)).expect("能算");
        let c = rect_at(&item, 0, &spec(8, 8000), (640, 480)).expect("能算");
        assert!((a.width - b.width).abs() < 1e-6, "同宽高比下归一化宽度必须一样");
        assert!((a.width - c.width).abs() > 1e-6, "换了宽高比，归一化宽度就该变（T1 的口径）");
    }

    #[test]
    fn 零尺寸或零字号没有可画的() {
        let item = DanmakuItem { text: "x".to_string(), lane: 0, enter: 0, exit: 10 , color: None, scroll_start: 0, travel_frames: 100};
        assert!(rect_at(&item, 0, &spec(8, 8000), (0, 360)).is_none());
        assert!(rect_at(&item, 0, &spec(8, 8000), (640, 0)).is_none());
        let mut zero_font = spec(8, 8000);
        zero_font.font_ratio = 0.0;
        assert!(rect_at(&item, 0, &zero_font, SEQUENCE).is_none(), "字号为 0 就是没东西可画");
    }

    #[test]
    fn 空素材给零条而不是报错() {
        assert!(parse_ass_danmaku("").expect("能解析").cues.is_empty());
        let laid = layout(&[], &spec(8, 8000), &tb(30, 1));
        assert!(laid.items.is_empty());
        assert_eq!(laid.dropped, 0);
    }

    // ---- T3.2b：写成 ASS（带 \move） ----

    /// 从一行 Dialogue 里把 `\move(x1,y1,x2,y2)` 的四个数抠出来。
    fn move_of(line: &str) -> (i64, i64, i64, i64) {
        let inside = line.split("\\move(").nth(1).expect("这行有 \\move").split(')').next().expect("有右括号");
        let nums: Vec<i64> =
            inside.split(',').map(|part| part.trim().parse().expect("是整数")).collect();
        assert_eq!(nums.len(), 4, "\\move 要四个数：{inside}");
        (nums[0], nums[1], nums[2], nums[3])
    }

    /// 写出弹幕那一行唯一的一条 Dialogue。
    fn dialogue_of(text: &str) -> String {
        text.lines()
            .find(|line| line.starts_with("Dialogue:"))
            .expect("至少有一条 Dialogue")
            .to_string()
    }

    #[test]
    fn 写出去的弹幕能读回同一批文本() {
        // `\an7` 与 `\move` 都是覆盖标签 —— 它们不该出现在读回来的文本里，
        // 否则"导出再导入"会让弹幕越滚越长。
        let cues = vec![cue(0, "第一条"), cue(1000, "第二条")];
        let laid = layout(&cues, &spec(8, 8000), &tb(30, 1));
        let text =
            to_ass_danmaku(&laid.items, &spec(8, 8000), 0, &tb(30, 1), SEQUENCE, &AssStyle::default())
                .expect("能写");
        let back = parse_ass_danmaku(&text).expect("能读回自己写的");
        assert_eq!(back.skipped, 0);
        assert_eq!(back.cues.len(), 2);
        assert_eq!(back.cues[0].text, "第一条");
        assert_eq!(back.cues[1].text, "第二条");
    }

    #[test]
    fn 横滚从右边缘到移出左边且纵向不动() {
        // 30fps，travel 1000ms（30 帧）。**在屏窗口拉长到 2 秒**：
        // 窗口比 travel 长的时候才会"滚到左边就停住"（V-Trim 的 min(progress,1)），
        // 而这正是最该被钉住的那一种 —— 窗口与 travel 相等时两者分不开。
        let laid = layout(&[cue_span(0, 2000, "abc")], &spec(8, 1000), &tb(30, 1));
        let text =
            to_ass_danmaku(&laid.items, &spec(8, 1000), 0, &tb(30, 1), SEQUENCE, &AssStyle::default())
                .expect("能写");
        let line = dialogue_of(&text);
        assert!(line.contains("\\an7"), "锚点必须是左上角，否则整条偏半条：{line}");
        let (x1, y1, x2, y2) = move_of(&line);
        assert_eq!(x1, 640, "进入帧的左边缘在序列右边缘");
        assert_eq!(y1, y2, "\\move 只横向滚，纵向不动");
        assert!(x2 < 0, "离开帧整条移出左边（x2 是负的）：{line}");
        // 纵向落点就是 rect_at 的 y 换成像素 —— 同一份数学，不是另写一遍。
        let rect = rect_at(&laid.items[0], 0, &spec(8, 1000), SEQUENCE).expect("能算");
        assert_eq!(y1, (rect.y * SEQUENCE.1 as f32).round() as i64);
        assert_eq!(
            x2,
            (rect_at(&laid.items[0], laid.items[0].exit, &spec(8, 1000), SEQUENCE)
                .expect("能算")
                .x
                * 640.0)
                .round() as i64,
            "\\move 的终点就是最后一帧在屏的落点"
        );
    }

    #[test]
    fn 泳道决定纵向像素位置() {
        // 两条弹幕竞泳道 0 与 1：y 必须不同，且都等于 rect_at 的 y 换像素。
        let laid = layout(&[cue(0, "甲"), cue(0, "乙")], &spec(2, 1000), &tb(30, 1));
        let text =
            to_ass_danmaku(&laid.items, &spec(2, 1000), 0, &tb(30, 1), SEQUENCE, &AssStyle::default())
                .expect("能写");
        let ys: Vec<i64> = text
            .lines()
            .filter(|line| line.starts_with("Dialogue:"))
            .map(|line| move_of(line).1)
            .collect();
        assert_eq!(ys.len(), 2);
        assert_ne!(ys[0], ys[1], "不同泳道不能落在同一个 y");
        assert!(ys[0] < ys[1], "0 号泳道在最上面");
    }

    #[test]
    fn 时间重定基到这一趟的第零帧() {
        // 30fps。起点 1000ms 是第 30 帧；base = 30 时它应该写成 0:00:00.00。
        // **在屏窗口是 cue 的 [1000, 2000)**：End 是开边界，最后在屏的是第 59 帧。
        // 终点取"最后一帧的下一个起点"：第 60 帧 = 2000ms，减 base 得 1000ms。
        let spec = spec(8, 1000);
        let laid = layout(&[cue(1000, "甲")], &spec, &tb(30, 1));
        assert_eq!((laid.items[0].enter, laid.items[0].exit), (30, 59));
        let text = to_ass_danmaku(&laid.items, &spec, 30, &tb(30, 1), SEQUENCE, &AssStyle::default())
            .expect("能写");
        let line = dialogue_of(&text);
        assert!(line.starts_with("Dialogue: 0,0:00:00.00,0:00:01.00,Default,"), "{line}");
    }

    #[test]
    fn 字号来自_font_ratio_乘序列高而不是传入的样式() {
        // 传入样式的字号写的是 48，但弹幕字号 = 0.04 × 360 = 14.4 -> 14。
        // 这条是"文件里的字与画面上的字一样大"的钉子。
        let style = AssStyle { font: "某字体".to_string(), font_size: 48, margin_v: 36 };
        let laid = layout(&[cue(0, "甲")], &spec(8, 1000), &tb(30, 1));
        let text =
            to_ass_danmaku(&laid.items, &spec(8, 1000), 0, &tb(30, 1), SEQUENCE, &style).expect("能写");
        assert!(text.contains("Style: Default,某字体, 14,"), "{text}");
        assert!(!text.contains(", 48,"), "不该把传入样式的字号原样写进去");
    }

    #[test]
    fn 极短条目不会写出终点早于起点的行() {
        // 一帧短于 1ms 的时间基（ms_at_frame 会把相邻两帧压成同一毫秒）。
        let dense = tb(1_000_000, 1);
        let laid = layout(&[cue(0, "一闪")], &spec(8, 0), &dense);
        let text = to_ass_danmaku(&laid.items, &spec(8, 0), 0, &dense, SEQUENCE, &AssStyle::default())
            .expect("能写");
        let back = parse_ass_danmaku(&text).expect("能读");
        assert_eq!(back.cues.len(), 1);
        assert!(back.cues[0].end_ms >= back.cues[0].start_ms, "终点不能早于起点");
    }

    #[test]
    fn 坏时间基零字号零尺寸与负时间都报错而不是写出坏文件() {
        let laid = layout(&[cue(0, "甲")], &spec(8, 1000), &tb(30, 1));
        let style = AssStyle::default();
        let write = |spec: &DanmakuSpec, base: Frame, t: &TimebaseDto, seq: (u32, u32)| {
            to_ass_danmaku(&laid.items, spec, base, t, seq, &style)
        };
        // 时间基坏掉：算不出时间。
        assert!(write(&spec(8, 1000), 0, &tb(0, 1), SEQUENCE).is_err());
        // 字号为 0：没有东西可画。
        let mut zero_font = spec(8, 1000);
        zero_font.font_ratio = 0.0;
        let error = write(&zero_font, 0, &tb(30, 1), SEQUENCE).expect_err("字号为 0 要报错");
        assert!(error.contains("字号"), "{error}");
        // 序列宽为 0：落点算不出来。
        assert!(write(&spec(8, 1000), 0, &tb(30, 1), (0, 360)).is_err());
        assert!(write(&spec(8, 1000), 0, &tb(30, 1), (640, 0)).is_err());
        // 条目落在 base 之前：重定基之后是负时间。
        let error = write(&spec(8, 1000), 30, &tb(30, 1), SEQUENCE).expect_err("负时间要报错");
        assert!(error.contains("负的"), "{error}");
    }
}
