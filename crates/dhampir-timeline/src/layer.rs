//! 元素模型 **v2**：基础元素 + 拓展。
//!
//! # 这一版在解决什么
//!
//! v1 的 `Clip` 把 `source` 当**必填**，等于「图层必须挂素材」。
//! 而**调整图层恰恰是没有素材、只有特效的图层** —— 它在 v1 里根本表达不出来。
//!
//! 所以 v2 把层次摆正：
//!
//! - **基础元素**（[`Layer`] 的前半）：时间范围、变换、不透明度、混合模式、标识与记录。
//!   **它不认识素材** —— 实拍片段、调整图层、纯色、占位符都只需要这一层；
//! - **拓展**（[`Layer`] 的可选字段）：素材引用、特效、转场、关键帧。
//!
//! # 与 v1 的关系
//!
//! v2 的**渲染语义与 v1 完全一致** —— 它只是把 v1 的隐含假设显式化并补上缺失的字段。
//! 所以迁移只做字段搬运，不需要重新判定渲染结果；
//! 最终判据是「同一份工程在 v1 与 v2 下出片**逐字节相同**」。
//!
//! # 时间：为什么是 start/end 而不是 start+duration
//!
//! 与仓库既有约定天然一致（`compose::end_frame` 返回的就是「下一位」，帧区间一律左闭右开）。
//! 而且 duration 与 end 是两份真理、会不一致；v1 的 `track_at + duration` 用的是
//! `saturating_add`，**溢出会被静默截断**。
//! `duration` 仍然对外提供，但作为派生值，不是第二份真相。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::schema::{
    Effect, EffectSpec, Frame, Issue, Keyframe, Project, TimebaseDto, TrackKind, TransitionSpec,
};

/// 当前契约版本。
pub const LAYER_SCHEMA_VERSION: u32 = 4;

/// v3 的版本号。**留着是因为迁移梯子要它** —— 与 V2 同理。
pub const LAYER_SCHEMA_VERSION_V3: u32 = 3;

/// v2 的版本号。**留着是因为迁移梯子要它**：v1 → v2 → v3 是一级一级走的，
/// 每一步都必须产出一个"当时那个版本"的中间物，否则中间那一级没有名字。
pub const LAYER_SCHEMA_VERSION_V2: u32 = 2;

/// 混合模式。
///
/// **枚举留全，但不都实现**：能用固定混合方程表达的只有前四种（见 render::compose 的说明）。
/// 后面的需要读目标像素，得走 ping-pong —— 那是另一个数量级的改动。
/// 渲染遇到未实现的模式**必须报错**，不许静默按 normal 画：静默降级正是这个项目最要避免的。
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlendMode {
    #[default]
    Normal,
    Add,
    Multiply,
    Screen,
    // ---- 以下需要读取目标像素，v2 不实现（枚举先占位，免得将来又要改版本号）----
    Darken,
    Lighten,
    Overlay,
    SoftLight,
    Difference,
}

impl BlendMode {
    /// 全部变体。给「从 is_implemented() 生成能力清单」用 ——
    /// **手写一份支持清单一定会与渲染器漂开**，所以这里只列枚举，能力由谓词推出来。
    pub const ALL: [BlendMode; 9] = [
        BlendMode::Normal,
        BlendMode::Add,
        BlendMode::Multiply,
        BlendMode::Screen,
        // 以下需要读取目标像素，v2 不实现（枚举先占位，与上面同因）。
        BlendMode::Darken,
        BlendMode::Lighten,
        BlendMode::Overlay,
        BlendMode::SoftLight,
        BlendMode::Difference,
    ];

    /// 能不能用固定混合方程表达。
    ///
    /// 渲染器**必须**先问这个，再决定要不要往下走 —— 让「做不到」在渲染前就显形，
    /// 而不是画出一张悄悄降级的图。
    pub const fn is_implemented(self) -> bool {
        matches!(self, Self::Normal | Self::Add | Self::Multiply | Self::Screen)
    }
}

/// v2 的变换。
///
/// 与 v1 的 Transform 差别只有一个：rotation_deg 改名成 rotation。
/// 看起来是小事，但「度数」写进字段名等于把单位焊进契约 —— 而单位本来就该由契约规定，
/// 不该靠名字暗示。改名属于 v2 的破坏性改动之一。
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TransformV2 {
    pub x: f32,
    pub y: f32,
    pub scale: f32,
    /// **角度制**（契约规定，不由字段名暗示）。
    pub rotation: f32,
}

impl Default for TransformV2 {
    fn default() -> Self {
        Self { x: 0.0, y: 0.0, scale: 1.0, rotation: 0.0 }
    }
}

/// 素材引用。**有它才是实拍片段。**
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceRef {
    /// 指向工程文件里资产登记表的 id。**契约里只有它，没有位置信息** ——
    /// 位置由宿主解释，这正是前后端分离能成立的前提。
    pub asset_id: String,
    /// 素材内起点（帧）。
    pub source_in: Frame,
}

/// 挂在元素上的标记。
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Marker {
    pub id: String,
    /// **相对该元素 start 的偏移**。用绝对帧号的话，元素一挪标记就错位了。
    pub frame: Frame,
#[serde(default)]
    pub name: String,
#[serde(default)]
    pub color: Option<String>,
}

/// 任何元素都能挂的记录。抽出来共用，避免每类元素各写一份。
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Recorded {
    /// 键值分类维度。用 BTreeMap 保证序列化逐字节稳定。
#[serde(default)]
    pub tags: BTreeMap<String, String>,
#[serde(default)]
    pub note: String,
#[serde(default)]
    pub markers: Vec<Marker>,
}

/// 基础元素 + 拓展。
///
/// 判定规则（三行，不需要额外字段）：
/// - 有 `source` → 实拍片段；
/// - **没有** `source`、**有** `effects` → **调整图层**（影响下面已画上去的全部内容）；
/// - 都没有 → 占位。
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Layer {
    // ---- 基础元素 ----
    /// 全局唯一。v1 里只保证轨内唯一，v2 提到全局 —— 否则跨轨引用无从谈起。
    pub id: String,
    /// 帧区间**左闭右开**：`[start, end)`。
    pub start: Frame,
    pub end: Frame,
#[serde(default)]
    pub transform: TransformV2,
#[serde(default = "one")]
    pub opacity: f32,
#[serde(default)]
    pub blend: BlendMode,
#[serde(default = "yes")]
    pub enabled: bool,
#[serde(default, flatten)]
    pub recorded: Recorded,

    // ---- 拓展（按需挂）----
    /// 没有它就不是实拍片段 —— 这正是调整图层能存在的原因。
#[serde(default)]
    pub source: Option<SourceRef>,
    /// **素材放完了要不要从头再来。**
    ///
    /// 默认为假，也就是老行为：素材不够长就是**越界错误**
    /// （`source_range_exceeded`）。这个默认值是故意的 ——
    /// 静默循环会把"我配错了素材长度"变成一个看不出来的错。
    ///
    /// # 为什么必须有这个字段
    ///
    /// 动图贴纸（GIF）是**短素材铺长区间**：实测一张 12 帧的 GIF
    /// 要覆盖 224 个时间线帧（3.7 秒）。没有这个开关时只有两条路 ——
    /// 谎报 `frame_count`（校验过、渲染读不存在的帧），
    /// 或者把贴纸缩短（动图放完就消失，与 V-Trim 行为不同）。
    /// 两条都是**用错的形状去套**，所以这里加一个正当的表达。
    ///
    /// 循环在**素材帧**上取模，不是时间线帧 —— 素材 10fps、时间线 60fps 时
    /// 一个素材帧要停 6 个时间线帧，按时间线帧取模会把这 6 帧拆散。
#[serde(default)]
    pub loop_source: bool,
#[serde(default)]
    pub effects: Vec<Effect>,
#[serde(default)]
    pub transition_in: Option<TransitionSpec>,
#[serde(default)]
    pub keyframes: Vec<Keyframe>,
}

fn one() -> f32 { 1.0 }
fn yes() -> bool { true }

impl Layer {
    /// 时长。**派生值**，不是第二份真相。
    pub fn duration(&self) -> Frame {
        self.end.saturating_sub(self.start)
    }

    /// 是不是调整图层：没有素材、但有特效。
    pub fn is_adjustment(&self) -> bool {
        self.source.is_none() && !self.effects.is_empty()
    }

    /// 这一帧在不在范围内（左闭右开）。
    pub fn covers(&self, frame: Frame) -> bool {
        frame >= self.start && frame < self.end
    }
}

/// 字幕样式。**放在轨道级，不是元素级。**
///
/// 理由：一条字幕轨 = 一个字幕素材 + 一套样式。想要两套样式就开两条轨 ——
/// 这正是 PR 里字幕轨的做法。放元素级会让每个 Layer 字面量都多一个字段，
/// 而换来的只是"同一条轨上两种样式"，那件事本来就该用两条轨表达。
///
/// 字号与边距用**比例**（相对目标高度），不用像素：像素在预览（640x360）与
/// 成片（1920x1080）里含义不同，两端就不一致了。
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SubtitleStyle {
    /// 字号 = 目标高度 * 这个比例。
    #[serde(default = "subtitle_font_ratio")]
    pub font_ratio: f32,
    /// 底边距 = 目标高度 * 这个比例。
    #[serde(default = "subtitle_bottom_margin")]
    pub bottom_margin: f32,
    /// 最多几行（超出的行丢掉 —— 字幕不该盖住半屏）。
    #[serde(default = "subtitle_max_lines")]
    pub max_lines: u32,
    /// 文字颜色，RGBA。
    #[serde(default = "subtitle_color")]
    pub color: [u8; 4],
    /// 是否加描边（压住亮背景）。
    #[serde(default = "yes")]
    pub outline: bool,
}

fn subtitle_font_ratio() -> f32 { 0.055 }
fn subtitle_bottom_margin() -> f32 { 0.06 }
fn subtitle_max_lines() -> u32 { 2 }
fn subtitle_color() -> [u8; 4] { [255, 255, 255, 255] }

impl Default for SubtitleStyle {
    fn default() -> Self {
        Self {
            font_ratio: subtitle_font_ratio(),
            bottom_margin: subtitle_bottom_margin(),
            max_lines: subtitle_max_lines(),
            color: subtitle_color(),
            outline: true,
        }
    }
}

/// 弹幕参数。**放在轨道级**：一条弹幕轨 = 一份弹幕素材 + 一套泳道参数。
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DanmakuSpec {
    /// 指向弹幕素材（AssetKind::Subtitle，内容是 ASS）。
    pub asset_id: String,
    /// 泳道数。排不下就**丢该条并计数**，不叠在一起。
    #[serde(default = "danmaku_lanes")]
    pub lanes: u32,
    /// 一条弹幕从右滚到左要多久（毫秒）。
    #[serde(default = "danmaku_duration")]
    pub duration_ms: u64,
    /// 字号 = 目标高度 * 这个比例。
    #[serde(default = "danmaku_font_ratio")]
    pub font_ratio: f32,
}

fn danmaku_lanes() -> u32 { 8 }
fn danmaku_duration() -> u64 { 8000 }
fn danmaku_font_ratio() -> f32 { 0.04 }

impl Default for DanmakuSpec {
    fn default() -> Self {
        Self {
            asset_id: String::new(),
            lanes: danmaku_lanes(),
            duration_ms: danmaku_duration(),
            font_ratio: danmaku_font_ratio(),
        }
    }
}

/// v2 的轨道：v1 是 clips，v2 是 layers。
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrackV2 {
    pub id: String,
    pub kind: TrackKind,
#[serde(default)]
    pub layers: Vec<Layer>,
    /// 字幕轨的样式。非字幕轨忽略它。
#[serde(default, skip_serializing_if = "Option::is_none")]
    pub subtitle: Option<SubtitleStyle>,
    /// 弹幕轨的参数。非弹幕轨忽略它。
#[serde(default, skip_serializing_if = "Option::is_none")]
    pub danmaku: Option<DanmakuSpec>,
}

/// v2 契约：与 v1 同形，但 `tracks` 用 v2 轨道，并多了工程级标记。
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TimelineV2 {
    pub schema: u32,
    pub timebase: TimebaseDto,
#[serde(default)]
    pub markers: Vec<Marker>,
#[serde(default)]
    pub tracks: Vec<TrackV2>,
}

/// 迁移失败的原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MigrateError {
    /// 不是 v1 契约。
    NotV1,
    /// 撞 id：两个片段共用一个 id，而 v2 要求全局唯一。
    DuplicateId(String),
}

impl core::fmt::Display for MigrateError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NotV1 => f.write_str("不是 schema v1 的契约"),
            Self::DuplicateId(id) => write!(
                f,
                "元素 id {id} 重复：v1 只保证轨内唯一，v2 要求全局唯一，迁移动不了 —— 请先改名"
            ),
        }
    }
}

impl core::error::Error for MigrateError {}

/// v1 契约 → v2。**只做字段搬运，不改语义。**
///
/// 撞 id 时**报错而不是静默改名**：静默改名会让"引用某个元素"的东西（标记、下游工具）
/// 指向另一个元素，而用户看不到任何提示。
pub fn migrate_v1_to_v2(project: &Project) -> Result<TimelineV2, MigrateError> {
    if project.schema != 1 {
        return Err(MigrateError::NotV1);
    }
    let mut seen: BTreeMap<&str, ()> = BTreeMap::new();
    let mut tracks = Vec::with_capacity(project.tracks.len());
    for track in &project.tracks {
        let mut layers = Vec::with_capacity(track.clips.len());
        for clip in &track.clips {
            if seen.insert(clip.id.as_str(), ()).is_some() {
                return Err(MigrateError::DuplicateId(clip.id.clone()));
            }
            layers.push(Layer {
                id: clip.id.clone(),
                start: clip.track_at,
                // v1 是 [track_at, track_at + duration)，v2 直接存右端点。
                end: clip.track_at.saturating_add(clip.duration),
                transform: TransformV2 {
                    x: clip.transform.x,
                    y: clip.transform.y,
                    scale: clip.transform.scale,
                    rotation: clip.transform.rotation_deg,
                },
                opacity: clip.opacity,
                blend: BlendMode::Normal,
                enabled: true,
                recorded: Recorded::default(),
                // v1 的 source 是个裸字符串；它本来就是"资产 id"，这里如实搬过来。
                source: Some(SourceRef {
                    asset_id: clip.source.clone(),
                    source_in: clip.source_in,
                }),
                // v1 没有循环这个概念，迁移后就是**不循环**。
                loop_source: false,
                effects: clip.effects.clone(),
                // v4 起 TransitionSpec 含 String，不再是 Copy —— 必须 clone。
                transition_in: clip.transition_in.clone(),
                keyframes: clip.keyframes.clone(),
            });
        }
        tracks.push(TrackV2 {
            id: track.id.clone(),
            kind: track.kind,
            layers,
            // v1 没有字幕/弹幕的概念，如实留空。
            subtitle: None,
            danmaku: None,
        });
    }
    Ok(TimelineV2 {
        // **产出 v2，不是当前版本。** 迁移梯子一级一级走，
        // 否则这个函数的名字与它实际产出的东西会对不上。
        schema: LAYER_SCHEMA_VERSION_V2,
        timebase: project.timebase,
        markers: Vec::new(),
        tracks,
    })
}

/// v2 → v3。**字段一个都不动，只把版本号推上去。**
///
/// # 为什么单为"单位"升一个版本
///
/// v3 改的是 source_in 的**单位**：v2 及以前它按"第 N 帧素材"被消费
/// （后端直接数解码器吐出的第 N 帧），v3 起它明确是**素材自己的帧号**，
/// 由时间线的时间基换算过去。
///
/// 数值上是连续的（旧行为本来就是"文件第 N 帧"），所以**不需要改任何字段** ——
/// 但**含义变了**，而含义变了比加字段更容易出静默错误：
/// 素材帧率与时间线一致时两者完全相同，不一致时画面节奏会变。
/// 这正是"该升版本"的定义。
pub fn migrate_v2_to_v3(timeline: &TimelineV2) -> TimelineV2 {
    let mut next = timeline.clone();
    next.schema = LAYER_SCHEMA_VERSION_V3;
    next
}

/// v3 契约 → v4。**只改转场类型的表示，不改渲染语义。**
///
/// v3 的 transition_in.kind 是一个 Rust enum，序列化出来是字符串 cross_dissolve；
/// v4 把它改成**同名的字符串字段**，所以**字节完全不变** —— 旧文件能被 v4 直接读。
///
/// 那为什么还要升版本？因为变的是**类型**而不是字段：
/// v3 的实现只认那一个变体，v4 接受任意串（由校验层判是否登记）。
/// 一个 v4 写的、带新转场的文件若被 v3 读，会**静默降级**成"未知转场"——
/// 而按仓库的规矩，静默降级比报错更坏，所以要让旧实现明确拒绝它。
///
/// 判据：同一份工程在 v3 与 v4 下**出片逐字节相同**（由测试钉住）。
pub fn migrate_v3_to_v4(timeline: &TimelineV2) -> TimelineV2 {
    let mut next = timeline.clone();
    next.schema = LAYER_SCHEMA_VERSION;
    next
}

/// source_in 的单位换算：**时间线上的局部帧号 → 素材自己的帧号**。
///
/// # 语义
///
/// source_in 是**素材的帧号**（PR 的 in 点是源时间码，这里同理）；
/// local_frame 是"这一层自己的第几帧"（帧号减去 layer.start）。
/// 两者相加之前，先把 local_frame 按**时间**换算到素材的帧率上。
///
/// # 为什么是纯整数
///
/// 全仓的铁律是"时间一律用整数帧号，不用浮点秒"。用浮点算这一步，
/// 0.1 + 0.2 那类误差会变成"某些帧被吃掉/重复"，而那种错只在导出后才看得见。
/// 中间量用 i128，溢出不发生在 i64 上。
///
/// # 取整方向
///
/// **向下取整**（div_euclid，负数也朝下）。理由：它让"60fps 素材 / 30fps 时间线"
/// 精确地取 +0,+2,+4…（舍去一半），而"30fps 素材 / 60fps 时间线"取 +0,+0,+1,+1
/// （每帧停两次，**时长不变**）。四舍五入在两种最常见的情形里给出同样结果，
/// 但在 30000/1001 这类非整数帧率上会产生周期性抖动 —— 向下取整是可预测的那一个。
pub fn source_frame_at(
    source_in: Frame,
    local_frame: Frame,
    timeline: &TimebaseDto,
    asset: &TimebaseDto,
) -> Result<Frame, String> {
    if timeline.num == 0 || timeline.den == 0 {
        return Err(format!("时间线的时间基不合法：{}/{}", timeline.num, timeline.den));
    }
    if asset.num == 0 || asset.den == 0 {
        return Err(format!("素材的时间基不合法：{}/{}", asset.num, asset.den));
    }
    // local 是时间线上的帧数；先换成"秒"的分子，再换成素材的帧数，全程整数。
    let numerator = i128::from(local_frame) * i128::from(timeline.den) * i128::from(asset.num);
    let denominator = i128::from(timeline.num) * i128::from(asset.den);
    let scaled = numerator.div_euclid(denominator);
    let clamped = scaled.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64;
    Ok(source_in.saturating_add(clamped))
}

/// 与 [`source_frame_at`] 同样的换算，但**可以循环**。
///
/// `loop_source` 为真时，走到素材末尾就**回到素材第 0 帧**接着播。
///
/// # 循环的语义：整张素材循环，不是"从 source_in 起的一段"循环
///
/// 想清楚这条很重要。有 `source_in = 3`、素材 6 帧时，两种可能的语义：
///
/// - **整张循环**（本函数）：`3,4,5,0,1,2,3,4,5,0,…`
/// - 从 source_in 起一段循环：`3,4,5,6,7,8,3,4,…` —— 而 6/7/8 **超出素材**，
///   那是坏帧号。
///
/// 第一版我写的是后者（`(raw - source_in).rem_euclid(count) + source_in`），
/// 于是会算出素材里不存在的帧号。按"整张循环"才对：
/// 素材是个循环的圈，`source_in` 只是**从圈的哪里开始进**。
///
/// 实测最常见的用法（贴纸动图）`source_in` 就是 0，两种语义重合；
/// 数值上不重合时**只有整张循环不会越界**。
///
/// # `frame_count` 未知时
///
/// （`None` 或 0）**不循环** —— 不知道周期就谈不上取模。
/// 此时退化成 `source_frame_at`（老行为）。
pub fn source_frame_looped(
    source_in: Frame,
    local_frame: Frame,
    timeline: &TimebaseDto,
    asset: &TimebaseDto,
    frame_count: Option<Frame>,
    loop_source: bool,
) -> Result<Frame, String> {
    let raw = source_frame_at(source_in, local_frame, timeline, asset)?;
    if !loop_source {
        return Ok(raw);
    }
    let Some(count) = frame_count.filter(|count| *count > 0) else {
        return Ok(raw);
    };
    // `rem_euclid` 而不是 `%`：负数取模在 Rust 里是负数，而我们要的是"回到圈里"。
    Ok(raw.rem_euclid(count))
}

/// 素材帧号 → 秒。**用素材自己的时间基**，不是时间线的。
///
/// 宿主靠它把"这一帧要 video 元素停在哪一秒"算出来。用错时间基的表现是
/// **画面看起来正常但比预期慢/快**，而那是查起来最费劲的一类。
pub fn seconds_at_asset_frame(asset_frame: Frame, asset: &TimebaseDto) -> Option<f64> {
    if asset.num == 0 {
        return None;
    }
    Some(asset_frame as f64 * f64::from(asset.den) / f64::from(asset.num))
}

/// 序列帧号 → 秒。**用序列自己的时间基**。
///
/// # 为什么需要它（而不是让渲染器按 30fps 猜）
///
/// Warp 的位移场（抖动 / 弹跳 / 脉冲）以"秒"为自变量。渲染器**不知道**工程时间基
/// （那在 timeline 层，渲染器只吃 `Composite`），所以两个宿主各自换算一次。
///
/// 两边各写一遍这条除法，迟早在某个帧率上差一个系数 —— 而表现是
/// "预览在抖、成片抖得慢一点"，那种差异**只在成片里看得出来**。
/// 所以它住在契约层，两端调同一份。
pub fn seconds_at_sequence_frame(sequence_frame: Frame, timebase: &TimebaseDto) -> Option<f64> {
    if timebase.den == 0 {
        return None;
    }
    Some(sequence_frame as f64 * f64::from(timebase.den) / f64::from(timebase.num))
}

/// 「素材 id → 它的时间基 + 帧数」。给求值层用来做上面那个换算。
///
/// 用 BTreeMap 而不是 HashMap：工程文件里的迭代顺序要**逐字节稳定**
/// （同一份数据序列化两次必须一样），而遍历顺序会影响任何"顺手聚合"的结果。
///
/// # `frame_count` 为什么在这里
///
/// 循环素材（`Layer::loop_source`）取模要**周期**，而周期就是帧数。
/// 契约层算这个换算，就不能只知道时间基、不知道长度 ——
/// 否则求值层得自己再拿一份帧数，两份真相迟早在某个动图上漂开。
#[derive(Debug, Default, Clone, PartialEq)]
pub struct AssetTimebases {
    entries: BTreeMap<String, AssetTiming>,
}

/// 一个素材的时间信息。`frame_count` 可以不知道（那就不能循环）。
#[derive(Debug, Default, Clone, PartialEq)]
pub struct AssetTiming {
    pub timebase: TimebaseDto,
    pub frame_count: Option<Frame>,
}

impl AssetTimebases {
    pub fn new() -> Self {
        Self { entries: BTreeMap::new() }
    }

    pub fn insert(&mut self, asset_id: impl Into<String>, timebase: TimebaseDto) {
        self.entries.insert(asset_id.into(), AssetTiming { timebase, frame_count: None });
    }

    /// 连帧数一起登记。循环素材必须走这个 —— 见 `AssetTiming` 的说明。
    pub fn insert_with_count(
        &mut self,
        asset_id: impl Into<String>,
        timebase: TimebaseDto,
        frame_count: Option<Frame>,
    ) {
        self.entries.insert(asset_id.into(), AssetTiming { timebase, frame_count });
    }

    pub fn get(&self, asset_id: &str) -> Option<&TimebaseDto> {
        self.entries.get(asset_id).map(|timing| &timing.timebase)
    }

    /// 帧数。登记时没给就是 `None`（不能循环）。
    pub fn frame_count(&self, asset_id: &str) -> Option<Frame> {
        self.entries.get(asset_id).and_then(|timing| timing.frame_count)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{transition_kind, Clip, Track, Transform};

    fn v1_project() -> Project {
        Project {
            schema: 1,
            timebase: TimebaseDto { num: 30000, den: 1001 },
            tracks: vec![Track {
                id: "v1".to_string(),
                kind: TrackKind::Video,
                clips: vec![Clip {
                    id: "c1".to_string(),
                    source: "a.mp4".to_string(),
                    source_in: 10,
                    track_at: 30,
                    duration: 20,
                    transform: Transform { x: 5.0, y: -2.0, scale: 1.5, rotation_deg: 90.0 },
                    opacity: 0.5,
                    effects: Vec::new(),
                    keyframes: Vec::new(),
                    transition_in: None,
                }],
            }],
        }
    }

    fn base_layer(id: &str, start: Frame, end: Frame) -> Layer {
        Layer {
            id: id.to_string(),
            start,
            end,
            transform: TransformV2::default(),
            opacity: 1.0,
            blend: BlendMode::Normal,
            enabled: true,
            recorded: Recorded::default(),
            source: None,
            loop_source: false,
            effects: Vec::new(),
            transition_in: None,
            keyframes: Vec::new(),
        }
    }

    fn tb(num: u32, den: u32) -> TimebaseDto {
        TimebaseDto { num, den }
    }

    #[test]
    fn 迁移梯子的末端是当前版本() {
        // 梯子现在是三级：v1 -> v2 -> v3 -> v4。
        // **每一级都要验** —— 否则中间某级被跳过时这条测试照样绿，
        // 而"跳过中间那一级"正是迁移梯子最容易出的错。
        let v2 = migrate_v1_to_v2(&v1_project()).expect("v1 应当能迁移");
        assert_eq!(v2.schema, LAYER_SCHEMA_VERSION_V2);

        let v3 = migrate_v2_to_v3(&v2);
        assert_eq!(v3.schema, LAYER_SCHEMA_VERSION_V3, "第二级应当落在 v3");
        // **字段一个都不动**：v3 改的是单位，不是形状。
        assert_eq!(v3.timebase, v2.timebase);
        assert_eq!(v3.tracks, v2.tracks);

        let v4 = migrate_v3_to_v4(&v3);
        assert_eq!(v4.schema, LAYER_SCHEMA_VERSION, "梯子末端必须是当前版本");
        // v4 只改转场的**类型表示**，不改任何值 —— 所以除 version 外必须逐字段相同。
        assert_eq!(v4.timebase, v3.timebase);
        assert_eq!(v4.tracks, v3.tracks, "v4 只动 version，不动任何元素");
    }

    #[test]
    fn 转场类型的字符串在迁移前后不变() {
        // v4 把 transition_in.kind 从 enum 改成 String。
        // 序列化出来的**必须还是同一个串** —— 否则旧文件读进来会变成另一个转场，
        // 而那正是"静默改语义"，是本仓库最不能接受的一类变化。
        let mut timeline = migrate_v2_to_v3(&migrate_v1_to_v2(&v1_project()).expect("迁移"));
        timeline.tracks[0].layers[0].transition_in = Some(TransitionSpec {
            kind: transition_kind::CROSS_DISSOLVE.to_string(),
            duration: 4,
        });
        let v4 = migrate_v3_to_v4(&timeline);
        assert_eq!(
            v4.tracks[0].layers[0].transition_in.as_ref().map(|t| t.kind.as_str()),
            Some("cross_dissolve"),
            "迁移不该改类型串"
        );
    }

    #[test]
    fn 六十帧素材放进三十帧时间线要舍去一半() {
        // 这是这轮的核心判据：60fps 的素材、30fps 的时间线，
        // 时间线每前进一步，素材要走两步 —— 也就是**舍去一半的画面**。
        let timeline = tb(30, 1);
        let asset = tb(60, 1);
        let got: Vec<Frame> = (0..6)
            .map(|local| source_frame_at(100, local, &timeline, &asset).expect("合法"))
            .collect();
        assert_eq!(got, vec![100, 102, 104, 106, 108, 110]);
    }

    #[test]
    fn 三十帧素材放进六十帧时间线要每帧停两次_时长不变() {
        let timeline = tb(60, 1);
        let asset = tb(30, 1);
        let got: Vec<Frame> = (0..6)
            .map(|local| source_frame_at(0, local, &timeline, &asset).expect("合法"))
            .collect();
        assert_eq!(got, vec![0, 0, 1, 1, 2, 2]);
    }

    #[test]
    fn 帧率一致时换算就是恒等() {
        // **这一条是兼容性的根据**：既有工程（素材帧率 == 时间线）行为逐字节不变。
        let timeline = tb(30000, 1001);
        for local in [0, 1, 17, 999, 100_000] {
            assert_eq!(source_frame_at(7, local, &timeline, &timeline).expect("合法"), 7 + local);
        }
    }

    #[test]
    fn 非整数帧率的换算不靠浮点() {
        // 29.97 -> 59.94：每一步正好两步。
        let timeline = tb(30000, 1001);
        let asset = tb(60000, 1001);
        let got: Vec<Frame> = (0..5)
            .map(|local| source_frame_at(0, local, &timeline, &asset).expect("合法"))
            .collect();
        assert_eq!(got, vec![0, 2, 4, 6, 8]);
        // 反向：59.94 -> 29.97，每帧停两次。
        let back: Vec<Frame> = (0..5)
            .map(|local| source_frame_at(0, local, &asset, &timeline).expect("合法"))
            .collect();
        assert_eq!(back, vec![0, 0, 1, 1, 2]);
    }

    #[test]
    fn 时间基不合法要报错而不是猜() {
        assert!(source_frame_at(0, 0, &tb(0, 1), &tb(30, 1)).is_err());
        assert!(source_frame_at(0, 0, &tb(30, 1), &tb(30, 0)).is_err());
        assert!(source_frame_at(0, 0, &tb(30, 1), &tb(0, 0)).is_err());
    }

    #[test]
    fn 素材帧号换算成秒要用素材自己的时间基() {
        // 同一个素材帧号，在 60fps 与 30fps 下是不同的时刻。
        let at60 = seconds_at_asset_frame(60, &tb(60, 1)).expect("合法");
        let at30 = seconds_at_asset_frame(60, &tb(30, 1)).expect("合法");
        assert!((at60 - 1.0).abs() < 1e-12, "{at60}");
        assert!((at30 - 2.0).abs() < 1e-12, "{at30}");
        assert!(seconds_at_asset_frame(0, &tb(0, 1)).is_none());
    }

    #[test]
    fn 序列帧号换算成秒要用序列自己的时间基() {
        // Warp 的位移场以秒为自变量，两个宿主各调这一条。
        let at30 = seconds_at_sequence_frame(30, &tb(30, 1)).expect("合法");
        assert!((at30 - 1.0).abs() < 1e-12, "30fps 下第 30 帧应当是 1 秒，实得 {at30}");
        // **有理帧率**：30000/1001 下第 30000 帧是 1001 秒，不是一个近似值。
        let at_smpte = seconds_at_sequence_frame(30000, &tb(30000, 1001)).expect("合法");
        assert!((at_smpte - 1001.0).abs() < 1e-9, "实得 {at_smpte}");
        // 分母为 0 的时间基是坏的：**报没有，不猜一个值**。
        assert!(seconds_at_sequence_frame(1, &tb(30, 0)).is_none());
    }

    #[test]
    fn 资产时间基表按_id_查_没登记就是没有() {
        let mut table = AssetTimebases::new();
        table.insert("a.mp4", tb(60, 1));
        assert_eq!(table.len(), 1);
        assert_eq!(table.get("a.mp4").map(|t| t.num), Some(60));
        // 没登记 = 走恒等换算。**不许猜一个帧率。**
        assert!(table.get("b.mp4").is_none());
    }

    #[test]
    fn 迁移把字段逐个搬对() {
        let migrated = migrate_v1_to_v2(&v1_project()).expect("v1 应当能迁移");
        // **一级一级走**：这个函数产出 v2，再往上那一级由 migrate_v2_to_v3 做。
        assert_eq!(migrated.schema, LAYER_SCHEMA_VERSION_V2);
        assert_eq!(migrated.timebase.num, 30000);
        let layer = &migrated.tracks[0].layers[0];
        assert_eq!(layer.id, "c1");
        assert_eq!(layer.start, 30);
        assert_eq!(layer.end, 50);
        assert_eq!(layer.duration(), 20);
        assert_eq!(layer.opacity, 0.5);
        assert_eq!(layer.transform.rotation, 90.0);
        assert!(layer.enabled, "v1 没有这个概念，迁移后应当默认开着");
        assert_eq!(layer.blend, BlendMode::Normal, "v1 只有 normal");
    }

    #[test]
    fn 迁移把裸字符串_source_变成资产引用() {
        let migrated = migrate_v1_to_v2(&v1_project()).unwrap();
        let source = migrated.tracks[0].layers[0].source.clone().expect("应当有 source");
        assert_eq!(source.asset_id, "a.mp4");
        assert_eq!(source.source_in, 10);
    }

    #[test]
    fn 迁移不改渲染语义_左闭右开() {
        let migrated = migrate_v1_to_v2(&v1_project()).unwrap();
        let layer = &migrated.tracks[0].layers[0];
        assert!(!layer.covers(29), "起点之前不覆盖");
        assert!(layer.covers(30), "左端闭");
        assert!(layer.covers(49), "右端前一帧仍覆盖");
        assert!(!layer.covers(50), "右端开");
    }

    #[test]
    fn 迁移撞_id_要报错并指出是哪个() {
        let mut project = v1_project();
        let mut second = project.tracks[0].clips[0].clone();
        second.track_at = 100;
        project.tracks[0].clips.push(second);
        match migrate_v1_to_v2(&project) {
            Err(MigrateError::DuplicateId(id)) => assert_eq!(id, "c1"),
            other => panic!("应当报重复 id，得到 {other:?}"),
        }
    }

    #[test]
    fn 不是_v1_就拒绝迁移() {
        let mut project = v1_project();
        project.schema = 7;
        assert_eq!(migrate_v1_to_v2(&project), Err(MigrateError::NotV1));
    }

    #[test]
    fn 调整图层的判定() {
        let base = base_layer("x", 0, 10);
        assert!(!base.is_adjustment(), "都没有 → 占位，不是调整图层");

        let mut with_source = base.clone();
        with_source.source = Some(SourceRef { asset_id: "a".to_string(), source_in: 0 });
        assert!(!with_source.is_adjustment(), "有素材 → 实拍片段");

        let mut adjustment = base.clone();
        adjustment.effects.push(Effect {
            kind: "gaussian_blur".to_string(),
            params: std::collections::BTreeMap::new(),
            ..Default::default()
        });
        assert!(adjustment.is_adjustment(), "没素材、有特效 → 调整图层");

        let mut both = with_source.clone();
        both.effects = adjustment.effects.clone();
        assert!(!both.is_adjustment(), "有素材就还是实拍片段，不是调整图层");
    }

    #[test]
    fn 混合模式可实现性与枚举留全() {
        for mode in [BlendMode::Normal, BlendMode::Add, BlendMode::Multiply, BlendMode::Screen] {
            assert!(mode.is_implemented(), "{mode:?} 应当可实现");
        }
        for mode in [
            BlendMode::Darken,
            BlendMode::Lighten,
            BlendMode::Overlay,
            BlendMode::SoftLight,
            BlendMode::Difference,
        ] {
            assert!(!mode.is_implemented(), "{mode:?} 需要读目标像素，v2 不该说它可实现");
        }
    }

    #[test]
    fn 标记是相对偏移_元素平移后跟着走() {
        let mut layer = base_layer("x", 100, 160);
        layer.recorded.markers.push(Marker {
            id: "m1".to_string(),
            frame: 12,
            name: "这里".to_string(),
            color: None,
        });
        assert_eq!(layer.start + layer.recorded.markers[0].frame, 112);
        layer.start = 200;
        layer.end = 260;
        assert_eq!(layer.start + layer.recorded.markers[0].frame, 212, "元素挪了，标记跟着挪");
    }
}

/// v2 契约的校验。
///
/// 与 v1 那套的关系：**错误格式完全复用**（Issue 的 code/path/message），
/// 但检查的东西不同——v2 多了「全局 id 唯一」「end 必须大于 start」「blend 是否可实现」。
///
/// 刻意**不**在这里检查资产引用：那要工程文件壳才知道（assets 表在壳里），
/// 属于另一层的职责。这里只认时间线自己的事。
pub fn validate_timeline_v2(timeline: &TimelineV2, effects: &[EffectSpec]) -> Vec<Issue> {
    let mut issues = Vec::new();

    if timeline.schema != LAYER_SCHEMA_VERSION {
        issues.push(Issue::new(
            "unsupported_schema",
            "timeline.schema",
            format!(
                "时间线是 schema v{}，本实现只认 v{}",
                timeline.schema, LAYER_SCHEMA_VERSION
            ),
        ));
        // 版本都不认，后面字段的含义无从谈起——直接返回，别给一堆二次错误。
        return issues;
    }
    if let Err(message) = timeline.timebase.to_timebase() {
        issues.push(Issue::new("invalid_timebase", "timeline.timebase", message.to_string()));
    }

    // id 必须**全局**唯一（v1 只保证轨内唯一）。
    let mut seen: BTreeMap<&str, String> = BTreeMap::new();

    for (track_index, track) in timeline.tracks.iter().enumerate() {
        let track_path = format!("tracks[{track_index}]");
        if track.id.is_empty() {
            issues.push(Issue::new("empty_track_id", &track_path, "轨道的 id 不能为空".to_string()));
        }
        // 同轨重叠。排序后只看相邻——不相邻的区间若重叠，一定存在相邻的一对也重叠。
        let mut spans: Vec<(Frame, Frame, usize)> = track
            .layers
            .iter()
            .enumerate()
            .map(|(index, layer)| (layer.start, layer.end, index))
            .collect();
        spans.sort_by_key(|(start, _, _)| *start);
        for pair in spans.windows(2) {
            let (_, previous_end, _) = pair[0];
            let (start, _, index) = pair[1];
            if start < previous_end {
                issues.push(Issue::new(
                    "layer_overlap",
                    &format!("{track_path}.layers[{index}]"),
                    format!("与同轨的另一层重叠：本层从第 {start} 帧开始，而前一层到第 {previous_end} 帧才结束"),
                ));
            }
        }

        for (index, layer) in track.layers.iter().enumerate() {
            let path = format!("{track_path}.layers[{index}]");
            if layer.id.is_empty() {
                issues.push(Issue::new("empty_layer_id", &path, "图层的 id 不能为空".to_string()));
            } else if let Some(first) = seen.insert(layer.id.as_str(), path.clone()) {
                issues.push(Issue::new(
                    "duplicate_element_id",
                    &path,
                    format!("图层 id {} 与 {first} 重复（v2 要求全局唯一）", layer.id),
                ));
            }
            if layer.end <= layer.start {
                issues.push(Issue::new(
                    "end_not_after_start",
                    &format!("{path}.end"),
                    format!("区间必须左闭右开且非空：start={} end={}", layer.start, layer.end),
                ));
            }
            if layer.start < 0 {
                issues.push(Issue::new(
                    "negative_frame",
                    &format!("{path}.start"),
                    format!("起始帧不能为负：{}", layer.start),
                ));
            }
            if !layer.opacity.is_finite() || !(0.0..=1.0).contains(&layer.opacity) {
                issues.push(Issue::new(
                    "opacity_out_of_range",
                    &format!("{path}.opacity"),
                    format!("不透明度必须在 0..=1：{}", layer.opacity),
                ));
            }
            if !layer.transform.scale.is_finite() || layer.transform.scale <= 0.0 {
                issues.push(Issue::new(
                    "scale_not_positive",
                    &format!("{path}.transform.scale"),
                    format!("缩放必须是正的有限数：{}", layer.transform.scale),
                ));
            }
            if !layer.enabled {
                // 关掉的层不参与渲染，但范围内的字段问题仍该报——所以这里什么都不跳过，
                // 只是把它记在心里：将来若要"只校验启用的层"，这里就是分叉点。
            }
            // 元素级标记是**相对 start 的偏移**，所以范围是 [0, end-start)。
            for (marker_index, marker) in layer.recorded.markers.iter().enumerate() {
                if marker.frame < 0 || marker.frame >= layer.duration() {
                    issues.push(Issue::new(
                        "marker_out_of_layer",
                        &format!("{path}.markers[{marker_index}].frame"),
                        format!("标记在第 {} 帧（相对元素起点），而元素只有 {} 帧", marker.frame, layer.duration()),
                    ));
                }
            }
            if !effects.is_empty() {
                for (effect_index, effect) in layer.effects.iter().enumerate() {
                    let effect_path = format!("{path}.effects[{effect_index}]");
                    match effects.iter().find(|spec| spec.kind == effect.kind) {
                        None => issues.push(Issue::new(
                            "unknown_effect",
                            &effect_path,
                            format!("没有登记叫 {} 的特效", effect.kind),
                        )),
                        Some(spec) => {
                            for (name, value) in &effect.params {
                                if let Some((_, min, max)) = spec.params.iter().find(|(param, _, _)| param == name) {
                                    if !value.is_finite() || value < min || value > max {
                                        issues.push(Issue::new(
                                            "effect_param_out_of_range",
                                            &format!("{effect_path}.params.{name}"),
                                            format!("参数 {name} 必须在 {min}..={max}，得到 {value}"),
                                        ));
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    issues
}

/// blend 是否可用固定混合方程表达。**渲染器必须先问这个再往下走。**
///
/// 单独成一条函数而不是塞进 validate：因为「契约接受」与「本实现能做」是两件事——
/// 枚举留全是为了将来支持时不必再改版本号，但渲染时遇到未实现的必须明确报错，
/// 不许静默按 normal 画。
pub fn unimplemented_blends(timeline: &TimelineV2) -> Vec<(String, BlendMode)> {
    let mut out = Vec::new();
    for (track_index, track) in timeline.tracks.iter().enumerate() {
        for (layer_index, layer) in track.layers.iter().enumerate() {
            if !layer.blend.is_implemented() {
                out.push((
                    format!("tracks[{track_index}].layers[{layer_index}].blend"),
                    layer.blend,
                ));
            }
        }
    }
    out
}

#[cfg(test)]
mod v2_tests {
    use super::*;
    use crate::schema::TimebaseDto;

    fn layer(id: &str, start: Frame, end: Frame) -> Layer {
        Layer {
            id: id.to_string(),
            start,
            end,
            transform: TransformV2::default(),
            opacity: 1.0,
            blend: BlendMode::Normal,
            enabled: true,
            recorded: Recorded::default(),
            source: None,
            loop_source: false,
            effects: Vec::new(),
            transition_in: None,
            keyframes: Vec::new(),
        }
    }

    fn track(id: &str, layers: Vec<Layer>) -> TrackV2 {
        TrackV2 {
            id: id.to_string(),
            kind: crate::schema::TrackKind::Video,
            layers,
            subtitle: None,
            danmaku: None,
        }
    }

    fn timeline(tracks: Vec<TrackV2>) -> TimelineV2 {
        TimelineV2 {
            schema: LAYER_SCHEMA_VERSION,
            timebase: TimebaseDto { num: 60, den: 1 },
            markers: Vec::new(),
            tracks,
        }
    }

    fn codes(issues: &[Issue]) -> Vec<&str> {
        issues.iter().map(|i| i.code.as_str()).collect()
    }

    #[test]
    fn 空区间与非负检查() {
        let issues = validate_timeline_v2(&timeline(vec![track("v", vec![layer("a", 10, 10)])]), &[]);
        assert_eq!(codes(&issues), vec!["end_not_after_start"]);

        let issues = validate_timeline_v2(&timeline(vec![track("v", vec![layer("a", -5, 10)])]), &[]);
        assert!(codes(&issues).contains(&"negative_frame"));
    }

    #[test]
    fn id_必须全局唯一_跨轨也要抓() {
        // v1 只保证轨内唯一；v2 提到全局。这条是迁移时最容易被忽略的约束。
        let issues = validate_timeline_v2(
            &timeline(vec![
                track("v1", vec![layer("same", 0, 10)]),
                track("v2", vec![layer("same", 0, 10)]),
            ]),
            &[],
        );
        assert_eq!(codes(&issues), vec!["duplicate_element_id"]);
        assert!(issues[0].message.contains("same"), "要指出撞了哪一个");
    }

    #[test]
    fn 同轨重叠被抓而紧邻不抓() {
        let ok = validate_timeline_v2(
            &timeline(vec![track("v", vec![layer("a", 0, 10), layer("b", 10, 20)])]),
            &[],
        );
        assert!(ok.is_empty(), "紧邻（左闭右开）不该算重叠：{ok:?}");

        let bad = validate_timeline_v2(
            &timeline(vec![track("v", vec![layer("a", 0, 10), layer("b", 9, 20)])]),
            &[],
        );
        assert_eq!(codes(&bad), vec!["layer_overlap"]);
    }

    #[test]
    fn 未实现的混合模式_契约接受但渲染前必须被发现() {
        let mut l = layer("a", 0, 10);
        l.blend = BlendMode::Overlay;
        let timeline = timeline(vec![track("v", vec![l])]);
        // 契约层**不报错**：枚举留全就是为了将来支持时不必改版本号。
        assert!(validate_timeline_v2(&timeline, &[]).is_empty(), "契约应当接受它");
        // 但渲染前必须能知道「这个我做不了」——不许静默按 normal 画。
        let pending = unimplemented_blends(&timeline);
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].1, BlendMode::Overlay);
        assert!(pending[0].0.contains("blend"), "path 要指到字段：{}", pending[0].0);
    }

    #[test]
    fn 版本不认就只报一条() {
        let mut tl = timeline(vec![]);
        tl.schema = 99;
        tl.tracks.push(track("v", vec![layer("a", 10, 10)]));
        let issues = validate_timeline_v2(&tl, &[]);
        assert_eq!(codes(&issues), vec!["unsupported_schema"], "不该产生二次错误");
    }

    // -----------------------------------------------------------------------
    // 循环素材（`Layer::loop_source`）
    //
    // 这一组是**真数据逼出来的**：一张 12 帧的 GIF 贴纸要铺 224 个时间线帧，
    // 没有循环就只能谎报 `frame_count`（校验过、渲染读不存在的帧）。
    // -----------------------------------------------------------------------

    #[test]
    fn 不循环时素材帧号一直往上走() {
        // 老行为必须**逐位不变** —— 默认值就是不循环。
        let tl = TimebaseDto { num: 60, den: 1 };
        let asset = TimebaseDto { num: 10, den: 1 };
        // 10fps 素材放进 60fps 时间线：1 素材帧 = 6 时间线帧
        let got: Vec<Frame> = (0..12)
            .map(|local| source_frame_looped(0, local, &tl, &asset, Some(12), false).unwrap())
            .collect();
        assert_eq!(got, vec![0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 1, 1]);
    }

    #[test]
    fn 循环时超过素材长度就回到开头() {
        let tl = TimebaseDto { num: 60, den: 1 };
        let asset = TimebaseDto { num: 10, den: 1 };
        // 12 帧的素材，取 0..80 个时间线帧 —— 够走到循环点再过一点
        let got: Vec<Frame> = (0..80)
            .map(|local| source_frame_looped(0, local, &tl, &asset, Some(12), true).unwrap())
            .collect();
        // 每 6 个时间线帧走一个素材帧，12 个素材帧 = 72 个时间线帧一个周期
        assert_eq!(got[0], 0);
        assert_eq!(got[6], 1);
        assert_eq!(got[66], 11, "第 11 个素材帧");
        assert_eq!(got[72], 0, "第 72 个时间线帧回到素材第 0 帧");
        assert_eq!(got[78], 1, "然后继续");
        // **这条是关键**：不循环的话 got[72] 会是 12（越界）。
        assert!(got.iter().all(|f| *f < 12), "循环后绝不该出现超出 12 的素材帧：{got:?}");
    }

    #[test]
    fn 循环的周期是素材帧不是时间线帧() {
        // 素材 10fps、时间线 60fps。若按**时间线帧**取模（12），
        // 则 6 个时间线帧才走一个素材帧、却 12 个时间线帧就回头 ——
        // 表现是**一张 12 帧的动图只播了 2 帧就重来**。
        let tl = TimebaseDto { num: 60, den: 1 };
        let asset = TimebaseDto { num: 10, den: 1 };
        let at = |local| source_frame_looped(0, local, &tl, &asset, Some(12), true).unwrap();
        assert_eq!(at(11), 1, "第 11 个时间线帧还在第 1 个素材帧上");
        assert_eq!(at(66), 11, "第 66 个时间线帧才到第 11 个素材帧");
        assert_eq!(at(71), 11, "同一素材帧要停满 6 个时间线帧");
        assert_eq!(at(72), 0, "第 72 个时间线帧才回头");
    }

    #[test]
    fn 循环的是整张素材而不是从source_in起的一段() {
        // 素材 6 帧、从第 3 帧进：
        //   整张循环（对的）  -> 3,4,5,0,1,2,3,4
        //   从 3 起一段循环（错的）-> 3,4,5,6,7,8,3,4  <- 6/7/8 素材里不存在
        //
        // **这条用例就是被后者坑出来的。** 我第一版实现写的是后者，
        // 于是循环素材会算出越界帧号 —— 而校验放行（它只看 source_in），
        // 表现是渲染时读到不存在的帧。
        let tl = TimebaseDto { num: 1, den: 1 };
        let asset = TimebaseDto { num: 1, den: 1 };
        let got: Vec<Frame> = (0..8)
            .map(|local| source_frame_looped(3, local, &tl, &asset, Some(6), true).unwrap())
            .collect();
        assert_eq!(got, vec![3, 4, 5, 0, 1, 2, 3, 4]);
        // 关键不变量：循环后**永远落回素材范围内**。
        assert!(got.iter().all(|f| (0..6).contains(f)), "不许出现越界帧号：{got:?}");
    }

    #[test]
    fn 帧数未知时不循环() {
        // 不知道周期就谈不上取模 —— 必须退化成老行为，
        // 而不是自作主张按 0 或按 1 循环（那会画出一张静帧）。
        let tl = TimebaseDto { num: 60, den: 1 };
        let asset = TimebaseDto { num: 10, den: 1 };
        let got = source_frame_looped(0, 100, &tl, &asset, None, true).unwrap();
        assert_eq!(got, 16, "100 个时间线帧 / 6 = 素材第 16 帧");
    }

    #[test]
    fn 帧数为零也不循环() {
        // 0 是非法帧数。**不能拿它取模**（除零 panic），也不能当成"无限长"。
        let tl = TimebaseDto { num: 60, den: 1 };
        let asset = TimebaseDto { num: 10, den: 1 };
        let got = source_frame_looped(0, 100, &tl, &asset, Some(0), true).unwrap();
        assert_eq!(got, 16);
    }

    #[test]
    fn 负数素材帧也能正确取模() {
        // `rem_euclid` 才是"回到开头"的语义；Rust 的 `%` 会给出负数。
        // 负的 source_in 在契约层是别的错，但换算函数本身不该 panic 或给负数。
        let tl = TimebaseDto { num: 1, den: 1 };
        let asset = TimebaseDto { num: 1, den: 1 };
        let got = source_frame_looped(-2, 0, &tl, &asset, Some(5), true).unwrap();
        assert_eq!(got, 3, "-2 在模 5 下应当落到 3");
    }
}
