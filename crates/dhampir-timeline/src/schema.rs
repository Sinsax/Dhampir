//! 时间线 **schema v1**：唯一跨边界契约。
//!
//! # 为什么这一层的形状值得较真
//!
//! 两端（浏览器、服务端）只通过这一份 JSON 说话。**它错了，两端就"各自对、合起来错"**，
//! 而那种错误在 M2 那套摘要比对的框架下会表现为"渲染有差异"，让人去查 GPU——
//! 实际根因却在时间轴。所以校验要在这里做足，错误要能直接被人读懂。
//!
//! # 铁律
//!
//! - 时间一律**整数帧号**（i64），不用浮点秒；
//! - 帧率是**有理数** num/den，不用浮点；
//! - 特效是**声明式**的「类型 + 参数」，不接受可上传的 shader。
//!
//! # 版本策略
//!
//! 不兼容就 +1，**服务端拒绝未知版本**（不是尽力而为地猜）。这条比"兼容"重要：
//! 一份 v2 的工程被 v1 的实现按 v1 解释，出来的片子看着像对的，这才是最坏的情况。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::timebase::{Timebase, TimebaseError};

/// 当前契约版本。改形状就要 +1。
pub const SCHEMA_VERSION: u32 = 1;

/// 帧号。**整数**，这是整个仓库的铁律。
pub type Frame = i64;

fn default_opacity() -> f32 {
    1.0
}

/// 工程的根对象。
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Project {
    /// 契约版本。解析后必须等于 SCHEMA_VERSION，否则拒绝。
    pub schema: u32,
    pub timebase: TimebaseDto,
    #[serde(default)]
    pub tracks: Vec<Track>,
}

/// Timebase 的传输形态。用 num/den 而不是浮点 29.97。
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimebaseDto {
    pub num: u32,
    pub den: u32,
}

/// **没有时间基**时的占位。刻意不是 0/0 —— 那是非法的，
/// 会让 `to_timebase()` 失败，从而在"忘了填"的时候**报错而不是悄悄按 0 帧率走**。
impl Default for TimebaseDto {
    fn default() -> Self {
        Self { num: 0, den: 0 }
    }
}

impl TimebaseDto {
    /// 转成真正的 Timebase，并做合法性检查。
    ///
    /// 用 try_new 而不是 new：后者是 const fn，遇到 0 会**断言**。
    /// 契约层的输入来自外部，绝不能让它把进程带走。
    pub fn to_timebase(self) -> Result<Timebase, TimebaseError> {
        Timebase::try_new(self.num, self.den)
    }
}

impl From<Timebase> for TimebaseDto {
    fn from(value: Timebase) -> Self {
        Self {
            num: value.num,
            den: value.den,
        }
    }
}

#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackKind {
    Video,
    Audio,
    /// 字幕轨：轨上的元素引用 .srt / .ass 素材，样式在**轨道级**。
    Subtitle,
    /// 弹幕轨：轨上带一个 DanmakuSpec，指向 .ass 弹幕素材。
    Danmaku,
}

#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Track {
    pub id: String,
    pub kind: TrackKind,
    #[serde(default)]
    pub clips: Vec<Clip>,
}

/// 时间线上的一个片段。
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Clip {
    pub id: String,
    /// 素材标识。**不含路径语义**——由宿主解释（浏览器是 URL，服务端是本地文件）。
    pub source: String,
    /// 在素材里的起始帧号。
    pub source_in: Frame,
    /// 在时间线上的起始帧号。
    pub track_at: Frame,
    /// 占多少帧。必须为正。
    pub duration: Frame,
    #[serde(default)]
    pub transform: Transform,
    #[serde(default = "default_opacity")]
    pub opacity: f32,
    #[serde(default)]
    pub effects: Vec<Effect>,
    #[serde(default)]
    pub keyframes: Vec<Keyframe>,
    /// 入场转场。挂在**后一个**片段上，占它开头的若干帧。
    #[serde(default)]
    pub transition_in: Option<TransitionSpec>,
}

/// 转场：把前一个相邻片段淡出的同时把自己淡入。
///
/// 用「挂在片段上」而不是「在轨道上单列一条」，是为了让**重叠规则保持简单**：
/// 轨道内片段仍然不许重叠——转场不破坏这条不变量，也就不需要为它开特例。
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TransitionSpec {
    /// 类型串，对应 core 的转场注册表。
    ///
    /// v3 及以前这里是一个 Rust enum（只有 CrossDissolve 一个变体）。
    /// v4 起改成字符串，与 Effect 同形 —— 加一个转场不再需要动契约版本。
    pub kind: String,
    /// 占多少帧。必须为正，且不超过本片段的时长。
    pub duration: Frame,
}

/// 已知的转场类型串。
///
/// 用常量而不是 enum：**加一个转场不该升契约版本**。
/// 这与 Effect 的处理保持一致 —— 两条路同形，读代码的人只需要理解一套。
pub mod transition_kind {
    /// 交叉溶解：把前一个相邻片段淡出的同时把自己淡入。
    pub const CROSS_DISSOLVE: &str = "cross_dissolve";
}

/// 转场类型串是否已被登记。校验层用它判 `unknown_transition`。
pub fn known_transition_kind(kind: &str) -> bool {
    kind == transition_kind::CROSS_DISSOLVE
}

/// 全部已登记的转场类型串，已排序。UI 生成下拉框用。
pub fn transition_kinds() -> Vec<&'static str> {
    vec![transition_kind::CROSS_DISSOLVE]
}

#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Transform {
    pub x: f32,
    pub y: f32,
    pub scale: f32,
    /// 角度制。整数帧号之外的东西可以是浮点——**只有时间必须是整数**。
    pub rotation_deg: f32,
}

impl Default for Transform {
    fn default() -> Self {
        Self {
            x: 0.0,
            y: 0.0,
            scale: 1.0,
            rotation_deg: 0.0,
        }
    }
}

/// 特效：类型 + 参数。**不接受可上传的 shader**——那等于把两端一致性交给用户。
///
/// # 三个字段各管一件事
///
/// - `kind` / `params`：**算什么**（已有）
/// - `window`：**哪几帧生效、强度怎么起落**（新增）—— 让"第 30 帧闪一下白"
///   不必发明一个新的特效类型
/// - `opacity`：**整体混合多少**（新增）—— 与 `window` 相乘，且**可被关键帧驱动**
///
/// `window` 与 `opacity` 分开而不是把强度塞进 `params`：
/// 它们是**所有**特效共有的量，而 `params` 是每个特效私有的。混在一起的话
/// `params` 的键空间会被通用名污染（每个特效都得叫 `amount` 还是 `strength`？）。
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Effect {
    /// 类型串，对应 core 的特效注册表。
    pub kind: String,
    /// 参数用 BTreeMap：同一份工程序列化出来必须**逐字节相同**，HashMap 做不到这点。
    #[serde(default)]
    pub params: BTreeMap<String, f32>,
    /// 时间窗。缺省 `Always` = 老工程的行为（整个图层生命周期都生效）。
    #[serde(default, skip_serializing_if = "is_always_window")]
    pub window: Window,
    /// 整体混合强度，与 `window` 的包络**相乘**。缺省 1.0。
    #[serde(default = "default_opacity", skip_serializing_if = "is_one")]
    pub opacity: f32,
}

/// `window` 是 `Always` 时不必写进文件 —— 老工程重写时不产生噪音。
fn is_always_window(window: &Window) -> bool {
    matches!(window, Window::Always)
}

fn is_one(value: &f32) -> bool {
    (*value - 1.0).abs() < f32::EPSILON
}

impl Effect {
    /// 这一帧这条特效的**总强度** = 时间窗包络 × 自身不透明度 ×（关键帧驱动值）。
    ///
    /// `keyframe_opacity` 由调用方从 `Keyframe.target == "effect.<i>.opacity"`
    /// 求出来；没有那条曲线就传 `self.opacity`。
    pub fn strength(&self, local: Frame, duration: Frame, keyframe_opacity: Option<f32>) -> f32 {
        let base = keyframe_opacity.unwrap_or(self.opacity);
        (self.window.envelope(local, duration) * base).clamp(0.0, 1.0)
    }
}

/// 缺省：`Always` 窗口、满强度。**这是"老工程"的语义** ——
/// 在 `window`/`opacity` 存在之前，特效就是整个图层生命周期都满强度生效的。
///
/// 提供 `Default` 是为了让构造点（测试夹具、宿主拼装）能写
/// `Effect { kind, params, ..Default::default() }`，
/// 而不是每加一个通用字段就去改十几处字面量 —— 那些改动会淹掉真正的改动。
impl Default for Effect {
    fn default() -> Self {
        Self {
            kind: String::new(),
            params: BTreeMap::new(),
            window: Window::Always,
            opacity: 1.0,
        }
    }
}

/// 关键帧。frame 是**相对片段起点**的偏移，不是绝对帧号——
/// 这样片段一挪，关键帧跟着走，不会静默错位。
///
/// # `target`：这个键在驱动**哪个**量
///
/// 在它成为字段之前，求值函数叫 `opacity_from` —— **结构上只算一个标量**：
/// 既不问自己在驱动什么，也装不下第二条曲线。于是"运镜"（要同时驱动
/// `scale` 与 `x/y`）在这份契约里**根本表达不出来**。
///
/// 取值：
/// - `"opacity"`（缺省）/`"x"`/`"y"`/`"scale"`/`"rotation"` —— 驱动元素自身的量；
/// - `"effect.<下标>.<参数名>"` —— 驱动**某条特效的参数**。
///   例：`"effect.0.radius"` 让第 0 条特效的模糊半径随帧变化 ——
///   "模糊从小涨到大再回落"这类**瞬时特效**因此不需要新的特效类型。
///
/// **缺省是 `"opacity"`**：老工程里没有这个键，语义必须与从前逐字节一致。
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Keyframe {
    pub frame: Frame,
    #[serde(default = "default_keyframe_target")]
    pub target: String,
    pub value: f32,
    #[serde(default)]
    pub easing: Easing,
}

/// 缺省驱动不透明度 —— 这是 `Keyframe` 有 `target` 之前的唯一用途。
pub fn default_keyframe_target() -> String {
    "opacity".to_string()
}

/// 元素自身可被关键帧驱动的量。**只列这一组**：其余一律走 `effect.<i>.<param>`。
///
/// 为什么不做成枚举：`target` 还要能装 `effect.0.radius` 这种**带下标**的串，
/// 枚举装不下，而两套表示（枚举 + 字符串）迟早在某一处只认其中一种。
/// 所以这里只提供**判定与解析**，真值仍是字符串。
pub const TRANSFORM_TARGETS: [&str; 5] = ["opacity", "x", "y", "scale", "rotation"];

/// `effect.<下标>.<参数名>` 的解析结果。
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectTarget {
    pub index: usize,
    pub param: String,
}

/// 解析 `effect.<下标>.<参数名>`。不是这个形状就返回 None。
///
/// **参数名允许含点**：`effect.0.color.r` 会解析成下标 0、参数名 `color.r`
/// （按**第一个**点切、末段整体当参数名），因为参数名本来就可能带点
/// （见 `Effect::params` 的键名约定）。
pub fn parse_effect_target(target: &str) -> Option<EffectTarget> {
    let rest = target.strip_prefix("effect.")?;
    let (index_text, param) = rest.split_once('.')?;
    if param.is_empty() {
        return None;
    }
    Some(EffectTarget {
        index: index_text.parse().ok()?,
        param: param.to_string(),
    })
}

/// 缓动曲线。公式**写死在这里**，两端调同一个函数——
/// 各写一遍迟早在某个控制点上差一个像素，而那种差异最难归因。
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Easing {
    #[default]
    Linear,
    EaseIn,
    EaseOut,
    EaseInOut,
    /// **回弹**：先冲过头再落回来（`back_out`）。
    ///
    /// # 与 `EaseOut` / `EaseIn` 的关系（值得写下来）
    ///
    /// 参照实现 的三条缓动里，两条**已经**能对本仓的现成变体：
    ///
    /// ```text
    /// 参照实现  pow2_out(t) = 1-(1-t)^2   ==  EaseOut
    /// 参照实现  pow2_in(t)  = t^2         ==  EaseIn
    /// ```
    ///
    /// 我先前在转译器里把运镜的过渡报成"用 `ease_out` 近似"——
    /// **那是错的，它们逐值相同**。所以这一条不是"补一个近似"，
    /// 是"第三条缓动（过冲）本仓确实没有"。
    ///
    /// 系数取经典的 `1.70158`（CSS 的 `easeOutBack` 同款）。参照实现 的贴纸
    /// 弹入用的是 `s = 3`（更猛的过冲），差别只有峰值那一下（约 1.10 vs 1.25），
    /// 而且只在 0.35 秒的窗口里 —— 转译器会把它报出来。
    BackOut,
}

impl Easing {
    /// [0,1] 上的映射。输入会被夹到 [0,1]，避免外部算出越界值。
    pub fn apply(self, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        match self {
            Self::Linear => t,
            Self::EaseIn => t * t,
            Self::EaseOut => 1.0 - (1.0 - t) * (1.0 - t),
            Self::EaseInOut => {
                if t < 0.5 {
                    2.0 * t * t
                } else {
                    1.0 - 2.0 * (1.0 - t) * (1.0 - t)
                }
            }
            // `back_out`（经典系数 1.70158）：
            //     (t-1)^2 * ((s+1)*(t-1) + s) + 1
            // 与 参照实现 的 `back_out(t, s)` 同一个式子，只是它的贴纸用 s=3。
            //
            // **过冲**：t≈0.7 时超过 1（约 1.10），然后落回 1。
            // 不是"更慢的 ease_out"——`EaseOut` 单调不减且**永不大于 1**。
            Self::BackOut => {
                const S: f32 = 1.70158;
                let u = t - 1.0;
                u * u * ((S + 1.0) * u + S) + 1.0
            }
        }
    }
}

/// 校验问题。**结构化**，不是一句话——UI 要能直接渲染成人话，
/// 也要能按 code 分类（比如把警告和错误分开）。
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Issue {
    /// 机器可读的错误码。
    pub code: String,
    /// 出错位置，如 tracks[0].clips[1].duration。
    pub path: String,
    /// 人话。
    pub message: String,
}

impl Issue {
    /// 构造一条问题。**pub** 是因为别的模块（layer / project）也要造 Issue ——
    /// 复用同一套错误格式，而不是各造一份。
    pub fn new(code: &str, path: &str, message: String) -> Self {
        Self {
            code: code.to_string(),
            path: path.to_string(),
            message,
        }
    }
}

/// 特效的**时间窗**：它在自己所属元素的生命周期内，于哪几帧生效、强度怎么变。
///
/// # 为什么需要它（而不是"特效一直生效"）
///
/// 参照实现 那套「闪白 0.25 秒」「抖动 0.3 秒」是**瞬时事件**：一个 flat 的
/// 事件列表里，每条自带 `time` 与时长。本仓的模型是"图层 + 挂在图层上的特效"，
/// 特效默认跟着图层整个生命周期走 —— 于是"第 30 帧闪一下白"**表达不出来**。
///
/// 补法不是给特效加 `start`/`end`（那会与图层的区间形成两套时间真相），
/// 而是加一条**纯函数包络**：特效仍在图层区间内，只是强度按这条曲线起落。
///
/// # 为什么是纯 `t` 的函数
///
/// 参照实现 的 `handheld_sway` 注释里写着这条纪律的由来：纯函数意味着
/// **跳帧求值、并行求值、seek 到任意时刻，结果都一样**。有累积状态的话，
/// 预览跳到中间某帧与出片顺序播放会给出不同的图 —— 而那种差异只在成片里看得出来。
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Window {
    /// 一直生效（缺省）。强度恒为 1，除非被关键帧驱动。
    Always,
    /// 瞬时：上升 `attack` 帧、满值 `hold` 帧、回落 `release` 帧，**之后归零**。
    ///
    /// 三段包络，覆盖"闪一下"的全部形态：
    /// - `flash` = attack 1 / hold 1 / release 4
    /// - `shake` = attack 1 / hold 4 / release 4
    /// - `blur`  = attack 2 / hold 3 / release 5
    ///
    /// 全 0 时长会得到"瞬间满值再瞬间归零"，那是合法输入（不是错误），
    /// 所以不在这里拒绝 —— 拒绝它会把"最难的一帧"变成不可表达。
    Transient {
        attack: Frame,
        hold: Frame,
        release: Frame,
        /// 是否在结束之后**回到 0**（缺省 true）。
        /// false 用于"涨上去就一直保持"——它不同于 `Always`：`Always` 从一开始就是满值。
        #[serde(default = "yes")]
        fall_to_zero: bool,
    },
    /// 图层区间内线性淡入淡出，中间满值。用于"整段轻微变暗"这类**持续**效果。
    Fade {
        fade_in: Frame,
        fade_out: Frame,
    },
}

fn yes() -> bool {
    true
}

impl Default for Window {
    fn default() -> Self {
        Self::Always
    }
}

impl Window {
    /// 求这一帧的强度包络，落在 `[0, 1]`。
    ///
    /// `local` 是**相对所属元素起点**的帧偏移（与 `Keyframe.frame` 同一个口径）。
    /// `duration` 是元素时长，`Fade` 要靠它算回落的起点。
    ///
    /// **纯函数**：同样的输入永远同样的输出，不读任何外部状态。
    pub fn envelope(&self, local: Frame, duration: Frame) -> f32 {
        let local = local.max(0);
        match self {
            Self::Always => 1.0,
            Self::Transient { attack, hold, release, fall_to_zero } => {
                let attack = (*attack).max(0);
                let hold = (*hold).max(0);
                let release = (*release).max(0);
                if local < attack {
                    // 上升段：attack 为 0 时这一支走不到（local < 0 不可能）。
                    return local as f32 / attack as f32;
                }
                let after_attack = local - attack;
                if after_attack < hold {
                    return 1.0;
                }
                let into_release = after_attack - hold;
                if into_release < release {
                    return 1.0 - into_release as f32 / release as f32;
                }
                if *fall_to_zero { 0.0 } else { 1.0 }
            }
            Self::Fade { fade_in, fade_out } => {
                let fade_in = (*fade_in).max(0);
                let fade_out = (*fade_out).max(0);
                if fade_in > 0 && local < fade_in {
                    return local as f32 / fade_in as f32;
                }
                // 回落从"结束前 fade_out 帧"开始；元素太短时两者重叠，取剩下的那截。
                let out_start = (duration - fade_out).max(0);
                if fade_out > 0 && local >= out_start {
                    let remaining = duration - local;
                    if remaining <= 0 {
                        return 0.0;
                    }
                    return (remaining as f32 / fade_out as f32).clamp(0.0, 1.0);
                }
                1.0
            }
        }
    }
}

/// 特效跑在**哪个像素空间**上。
///
/// # 为什么这件事必须是数据，不能只写在注释里
///
/// 它直接决定**要不要按目标尺寸缩放参数**：
/// - [`EffectSpace::Source`]：跑在**源纹理**上，半径是源像素，与文档坐标系无关 -> **不缩放**；
/// - [`EffectSpace::Document`]：跑在**目标尺寸**的中间纹理上，同一数值在不同输出尺寸下
///   看起来不一样 -> **必须缩放**。
///
/// 在这条成为字段之前，这个区别只活在 render/timeline.rs 的两段注释里。
/// 而声明错的后果是**两端静默分叉**：预览与成片的输出尺寸通常不同，
/// 于是同一个半径在两边糊出不同的图，且没有任何一处会报错。
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectSpace {
    /// 源纹理像素。与目标尺寸无关。
    Source,
    /// 文档像素（= render_hints 坐标系）。要按目标/文档比例换算。
    Document,
}

/// 这个特效**怎么渲染**。
///
/// 注册表提供它，渲染器按它派发 —— 这样加一个特效不必再改渲染主路径里的
/// kind 分支。
///
/// 两种管线的区别是**数学性质**，不是实现细节：
///
/// * [`SeparableBlur`] 是**邻域**算子 —— 输出要看周围像素，
///   所以要两趟、要中间纹理，半径是在源/文档空间里要换算的量；
/// * [`ColorAdjust`] 是**逐像素**算子 —— 输出只看自己，
///   所以一趟直写目标，而且与坐标系无关（不需要空间换算）。
///
/// 按管线派发而不是按 kind 字符串，加特效就不必改渲染主路径 ——
/// 而"忘了改"正是字符串派发最容易出的错（S3 修的就是这个）。
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectPipeline {
    /// 横竖两趟的可分离模糊，核是定长展开的。
    SeparableBlur,
    /// 逐像素色彩调整：亮度 / 对比度 / 饱和度 / 色调，一趟直写。
    ColorAdjust,
    /// 逐像素**用常量色**叠加：闪白 / 暗角 / 噪声 / 纯色覆盖。
    ///
    /// 与 `ColorAdjust` 的差别：那些是"重新映射现有像素"，这些是"引入一个
    /// 与输入无关的颜色分量"。数学上都是逐像素、与坐标系无关，
    /// 但着色器的 uniform 完全不同，所以分成两条管线。
    ColorMask,
    /// **坐标重映射**：按一个位移场去取源像素（抖动 / 脉冲 / 挤压 / 缩放弹跳）。
    ///
    /// 与前三条的关键差别是它**读邻域**（像 SeparableBlur 那样），
    /// 但不止一趟、且位移场可以是任意的 —— 所以单独一条。
    Warp,
}

/// 一个特效的参数规格，由 core 的注册表提供。
///
/// 放在参数里而不是写死在 timeline 里，是为了让"登记表"只有一份（在 core），
/// 而这里只负责"照着表校对"。
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EffectSpec {
    pub kind: &'static str,
    /// 参数名 -> (最小值, 最大值)
    pub params: &'static [(&'static str, f32, f32)],
    /// 跑在哪个像素空间。决定要不要按目标尺寸缩放。
    pub space: EffectSpace,
    /// 怎么渲染。渲染器按它派发。
    pub pipeline: EffectPipeline,
    /// 这个特效作为**瞬时事件**插入时的默认时长。`None` = 它只该是持续的。
    ///
    /// 放在 `EffectSpec` 里而不是让 UI 自己写一张表：UI 那张表一定会与
    /// 渲染侧对"这特效是不是瞬时的"的判断漂开，而漂了没有任何东西会红。
    pub window_default: Option<WindowDefault>,
}

impl EffectSpec {
    /// 这个特效能否作为瞬时事件使用。
    pub const fn is_transient_capable(&self) -> bool {
        self.window_default.is_some()
    }
}

/// 瞬时特效的**默认时长**（帧）。
///
/// 与 `EffectSpec` 分开，是因为它描述的是"**这个特效在 UI 上点一下默认多长**"，
/// 而 `EffectSpec` 描述的是渲染契约。参照实现 那 15 个变体各自硬编码一个时长
/// （`0.25s` / `0.3s` / `0.4s`），散在一张 `match` 表里；
/// 收在这里之后，"加一个特效"不必再去改那张表。
///
/// 单位是**帧**而不是秒：铁律 1 要求时间一律整数帧，
/// 而"0.25 秒在 30fps 下是 7.5 帧"这种事不该由每个特效各自换算。
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowDefault {
    pub attack: Frame,
    pub hold: Frame,
    pub release: Frame,
}

impl WindowDefault {
    /// 给 UI 用的一次性默认窗口。
    pub const fn transient(self) -> Window {
        Window::Transient {
            attack: self.attack,
            hold: self.hold,
            release: self.release,
            fall_to_zero: true,
        }
    }

    /// 总时长（帧）。UI 显示"这个特效持续多久"。
    pub const fn total(self) -> Frame {
        self.attack + self.hold + self.release
    }
}

impl EffectSpec {
    /// 某个参数的上界。找不到就是 None（调用方不该猜一个默认上界）。
    pub fn param_max(&self, name: &str) -> Option<f32> {
        self.params.iter().find(|(n, _, _)| *n == name).map(|(_, _, max)| *max)
    }

    /// 某个参数的下界。
    pub fn param_min(&self, name: &str) -> Option<f32> {
        self.params.iter().find(|(n, _, _)| *n == name).map(|(_, min, _)| *min)
    }
}

/// 只校验结构与时间，不校验特效种类。
pub fn validate_project(project: &Project) -> Vec<Issue> {
    validate_project_with_effects(project, &[])
}

/// 完整校验。effects 为空表示"不检查特效种类与参数"。
pub fn validate_project_with_effects(project: &Project, effects: &[EffectSpec]) -> Vec<Issue> {
    let mut issues = Vec::new();

    if project.schema != SCHEMA_VERSION {
        issues.push(Issue::new(
            "unsupported_schema",
            "schema",
            format!(
                "工程是 schema v{}，本实现只认 v{}；请升级实现或另存为旧版本",
                project.schema, SCHEMA_VERSION
            ),
        ));
        // 版本都不认，后面字段的含义就无从谈起——直接返回，别给一堆二次错误。
        return issues;
    }

    if let Err(message) = project.timebase.to_timebase() {
        issues.push(Issue::new("invalid_timebase", "timebase", message.to_string()));
    }

    // 片段 id 必须全局唯一：否则"选中哪个片段"就成了未定义行为。
    let mut seen_clip_ids: BTreeMap<&str, String> = BTreeMap::new();

    for (track_index, track) in project.tracks.iter().enumerate() {
        let track_path = format!("tracks[{}]", track_index);
        if track.id.is_empty() {
            issues.push(Issue::new("empty_track_id", &track_path, "轨道的 id 不能为空".to_string()));
        }

        // 轨道内重叠：同一条轨上两个片段占了同一帧。
        // 排序后只看相邻——不相邻的区间若重叠，一定存在相邻的一对也重叠。
        let mut spans: Vec<(Frame, Frame, usize)> = Vec::new();
        for (clip_index, clip) in track.clips.iter().enumerate() {
            let start = clip.track_at;
            let end = clip.track_at.saturating_add(clip.duration);
            spans.push((start, end, clip_index));
        }
        spans.sort_by_key(|(start, _, _)| *start);
        for pair in spans.windows(2) {
            let (_, previous_end, _) = pair[0];
            let (start, _, index) = pair[1];
            if start < previous_end {
                issues.push(Issue::new(
                    "clip_overlap",
                    &format!("{}.clips[{}]", track_path, index),
                    format!(
                        "与同轨的另一个片段重叠：本片段从第 {} 帧开始，而前一区段到第 {} 帧才结束",
                        start, previous_end
                    ),
                ));
            }
        }

        for (clip_index, clip) in track.clips.iter().enumerate() {
            let clip_path = format!("{}.clips[{}]", track_path, clip_index);

            if clip.id.is_empty() {
                issues.push(Issue::new("empty_clip_id", &clip_path, "片段的 id 不能为空".to_string()));
            } else if let Some(first) = seen_clip_ids.insert(clip.id.as_str(), clip_path.clone()) {
                issues.push(Issue::new(
                    "duplicate_clip_id",
                    &clip_path,
                    format!("片段 id {} 与 {} 重复", clip.id, first),
                ));
            }

            if clip.duration <= 0 {
                issues.push(Issue::new(
                    "duration_not_positive",
                    &format!("{}.duration", clip_path),
                    format!("片段时长必须是正的帧数，得到 {}", clip.duration),
                ));
            }
            if clip.source_in < 0 {
                issues.push(Issue::new(
                    "negative_source_in",
                    &format!("{}.source_in", clip_path),
                    format!("素材内起始帧号不能为负，得到 {}", clip.source_in),
                ));
            }
            if clip.track_at < 0 {
                issues.push(Issue::new(
                    "negative_track_at",
                    &format!("{}.track_at", clip_path),
                    format!("时间线上的起始帧号不能为负，得到 {}", clip.track_at),
                ));
            }
            if clip.source.is_empty() {
                issues.push(Issue::new(
                    "empty_source",
                    &format!("{}.source", clip_path),
                    "片段必须指明素材".to_string(),
                ));
            }

            // 不透明度：0..=1，且不接受 NaN（NaN 会让所有比较都为假，静默穿过去）。
            if !clip.opacity.is_finite() || !(0.0..=1.0).contains(&clip.opacity) {
                issues.push(Issue::new(
                    "opacity_out_of_range",
                    &format!("{}.opacity", clip_path),
                    format!("不透明度必须在 0..=1，得到 {}", clip.opacity),
                ));
            }

            if !clip.transform.scale.is_finite() || clip.transform.scale <= 0.0 {
                issues.push(Issue::new(
                    "scale_not_positive",
                    &format!("{}.transform.scale", clip_path),
                    format!("缩放必须是正的有限数，得到 {}", clip.transform.scale),
                ));
            }

            // 关键帧必须落在片段自己的时间范围内。
            for (keyframe_index, keyframe) in clip.keyframes.iter().enumerate() {
                if keyframe.frame < 0 || keyframe.frame >= clip.duration {
                    issues.push(Issue::new(
                        "keyframe_out_of_clip",
                        &format!("{}.keyframes[{}]", clip_path, keyframe_index),
                        format!(
                            "关键帧在第 {} 帧，而片段只有 {} 帧（相对片段起点，0..={}）",
                            keyframe.frame,
                            clip.duration,
                            clip.duration - 1
                        ),
                    ));
                }
                if !keyframe.value.is_finite() {
                    issues.push(Issue::new(
                        "keyframe_value_not_finite",
                        &format!("{}.keyframes[{}]", clip_path, keyframe_index),
                        "关键帧的值必须是有限数".to_string(),
                    ));
                }
            }

            if let Some(transition) = &clip.transition_in {
                // 去枚举化之后，类型串是**开放**的，所以这里必须校验它。
                // 不校验的话，一个拼错的 kind 会被静静接受 —— 而渲染只读 duration，
                // 于是它**看起来完全正常**，只是行为可能不是用户要的那个转场。
                if !known_transition_kind(&transition.kind) {
                    issues.push(Issue::new(
                        "unknown_transition",
                        &format!("{}.transition_in.kind", clip_path),
                        format!(
                            "没有登记叫 {} 的转场；可用的是 {}",
                            transition.kind,
                            transition_kinds().join(" / ")
                        ),
                    ));
                }
                if transition.duration <= 0 {
                    issues.push(Issue::new(
                        "transition_duration_invalid",
                        &format!("{}.transition_in.duration", clip_path),
                        format!("转场时长必须是正的帧数，得到 {}", transition.duration),
                    ));
                } else if transition.duration > clip.duration {
                    issues.push(Issue::new(
                        "transition_longer_than_clip",
                        &format!("{}.transition_in.duration", clip_path),
                        format!("转场要 {} 帧，而片段本身只有 {} 帧", transition.duration, clip.duration),
                    ));
                }
                // 转场要和前一个片段**紧邻**——否则「淡出」的那一头根本不存在，
                // 画面会凭空从黑里淡进来，而用户以为自己配了交叉溶解。
                let has_previous = spans
                    .iter()
                    .any(|(_, end, index)| *index != clip_index && *end == clip.track_at);
                if !has_previous {
                    issues.push(Issue::new(
                        "transition_without_previous",
                        &format!("{}.transition_in", clip_path),
                        format!("第 {} 帧之前没有紧邻的片段，转场无处淡出", clip.track_at),
                    ));
                }
            }

            if !effects.is_empty() {
                for (effect_index, effect) in clip.effects.iter().enumerate() {
                    let effect_path = format!("{}.effects[{}]", clip_path, effect_index);
                    match effects.iter().find(|spec| spec.kind == effect.kind) {
                        None => issues.push(Issue::new(
                            "unknown_effect",
                            &effect_path,
                            format!("没有登记叫 {} 的特效；可用的是 {}", effect.kind, known_kinds(effects)),
                        )),
                        Some(spec) => {
                            for (name, value) in &effect.params {
                                match spec.params.iter().find(|(param, _, _)| param == name) {
                                    None => issues.push(Issue::new(
                                        "unknown_effect_param",
                                        &format!("{}.params.{}", effect_path, name),
                                        format!("特效 {} 没有参数 {}", effect.kind, name),
                                    )),
                                    Some((_, min, max)) => {
                                        if !value.is_finite() || value < min || value > max {
                                            issues.push(Issue::new(
                                                "effect_param_out_of_range",
                                                &format!("{}.params.{}", effect_path, name),
                                                format!("参数 {} 必须在 {}..={}，得到 {}", name, min, max, value),
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
    }

    issues
}

fn known_kinds(effects: &[EffectSpec]) -> String {
    let mut names: Vec<&str> = effects.iter().map(|spec| spec.kind).collect();
    names.sort_unstable();
    names.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 缺省 target 的简写：这些用例讲的是不透明度曲线，别让 target 喧宾夺主。
    fn opacity_target() -> String {
        default_keyframe_target()
    }

    // ===== T9 的时间窗：验收要求"纯函数（同 t 同结果，跳帧可复现）" =====

    #[test]
    fn 缺省窗口是满强度() {
        // 老工程每条特效都没有 window —— 必须恒为 1，否则"引入时间窗"
        // 会把所有既有工程改掉。
        assert_eq!(Window::default(), Window::Always);
        for local in [0, 1, 50, 10_000] {
            assert_eq!(Window::Always.envelope(local, 60), 1.0);
        }
    }

    #[test]
    fn 瞬时窗口是三段包络() {
        // attack 2 / hold 3 / release 4，共 9 帧。
        //
        // 段边界（左闭右开）：上升 [0,2)、满值 [2,5)、回落 [5,9)、之后 0。
        // 第 5 帧是**回落段的起点**，所以它仍是满值（`1 - 0/4`）。
        let window = Window::Transient { attack: 2, hold: 3, release: 4, fall_to_zero: true };
        let at = |local| window.envelope(local, 60);
        // 上升段：从 0 线性涨到 1（第 0 帧是 0.0，第 1 帧是 0.5）。
        assert_eq!(at(0), 0.0);
        assert!((at(1) - 0.5).abs() < 1e-6, "实得 {}", at(1));
        // 满值段。
        assert_eq!(at(2), 1.0);
        assert_eq!(at(4), 1.0);
        // 回落段：第 5 帧是起点（满值），第 6 帧开始降。
        assert_eq!(at(5), 1.0, "回落段的起点仍是满值");
        assert!((at(6) - 0.75).abs() < 1e-6, "回落 1/4：实得 {}", at(6));
        assert!((at(8) - 0.25).abs() < 1e-6, "回落 3/4：实得 {}", at(8));
        // 结束之后归零（第 9 帧正好走完 2+3+4）。
        assert_eq!(at(9), 0.0);
        assert_eq!(at(100), 0.0, "窗口之后必须一直是 0，不然它就不是瞬时的");
    }

    #[test]
    fn 瞬时窗口能选择不回落() {
        // `fall_to_zero = false`：涨上去就保持。**它不等于 `Always`** ——
        // `Always` 从第 0 帧就是满值，这个从 0 开始爬。
        let window = Window::Transient { attack: 2, hold: 3, release: 4, fall_to_zero: false };
        assert_eq!(window.envelope(0, 60), 0.0);
        assert_eq!(window.envelope(1, 60), 0.5);
        assert_eq!(window.envelope(4, 60), 1.0);
        assert_eq!(window.envelope(9, 60), 1.0, "回落段走完之后保持满值");
        assert_eq!(window.envelope(100, 60), 1.0, "永远不归零");
    }

    #[test]
    fn 零长的瞬时窗口是合法的() {
        // 全 0 会得到"瞬间满值再瞬间归零"。**合法输入**（不是错误）——
        // 拒绝它会把"最难的那一帧"变成不可表达。
        let window = Window::Transient { attack: 0, hold: 0, release: 0, fall_to_zero: true };
        assert_eq!(window.envelope(0, 60), 0.0, "零窗口在第 0 帧就结束了");
        assert_eq!(window.envelope(5, 60), 0.0);
    }

    #[test]
    fn 淡入淡出窗口在中间是满值() {
        let window = Window::Fade { fade_in: 10, fade_out: 10 };
        let at = |local| window.envelope(local, 100);
        assert_eq!(at(0), 0.0);
        assert!((at(5) - 0.5).abs() < 1e-6, "淡入一半：实得 {}", at(5));
        assert_eq!(at(10), 1.0);
        assert_eq!(at(50), 1.0, "中段满值");
        assert!(at(95) < 1.0 && at(95) > 0.0, "淡出一半：实得 {}", at(95));
        assert_eq!(at(100), 0.0);
    }

    #[test]
    fn 负数帧被当成第零帧() {
        // 求值层不该给出负数，但真给了也不许出 NaN 或越界 ——
        // 那会在画面上变成一个难查的亮点。
        let window = Window::Fade { fade_in: 10, fade_out: 10 };
        assert_eq!(window.envelope(-5, 100), window.envelope(0, 100));
    }

    #[test]
    fn 包络永远落在零到一之间() {
        // **扫一遍各种窗口的整个取值域**：包络跑到 [0,1] 之外会让强度
        // 变成负的或超过满值，那在着色器里表现为"反相"或"过曝"。
        let windows = [
            Window::Always,
            Window::Transient { attack: 1, hold: 1, release: 4, fall_to_zero: true },
            Window::Transient { attack: 0, hold: 0, release: 0, fall_to_zero: true },
            Window::Transient { attack: 5, hold: 0, release: 0, fall_to_zero: false },
            Window::Fade { fade_in: 3, fade_out: 7 },
            // 淡入淡出比时长还长：这是"图层太短"的常见情形，不许算出界。
            Window::Fade { fade_in: 200, fade_out: 200 },
        ];
        for window in windows {
            for local in -3..120 {
                let value = window.envelope(local, 60);
                assert!(
                    (0.0..=1.0).contains(&value),
                    "{window:?} 在第 {local} 帧给出 {value}，跑到 [0,1] 之外了"
                );
                assert!(value.is_finite(), "{window:?} 在第 {local} 帧给出非有限值");
            }
        }
    }

    #[test]
    fn 时间窗是纯函数_同样的输入永远同样的输出() {
        // **这是 T9 的验收判据**："同 t 同结果，跳帧可复现"。
        //
        // 反例是"用累积时间/随机数算包络"：那样顺序播放到第 N 帧
        // 与直接跳到第 N 帧会得到不同的强度 —— 而那正是最难归因的一类差异
        // （成片与预览不一致，但两边各自的逻辑都"看着对"）。
        let window = Window::Transient { attack: 3, hold: 5, release: 7, fall_to_zero: true };
        for local in 0..20 {
            // 反复求、乱序求、掺入别的调用 —— 结果必须一模一样。
            let first = window.envelope(local, 60);
            let _ = window.envelope(999, 60);
            let second = window.envelope(local, 60);
            assert_eq!(first, second, "第 {local} 帧两次求值不一致");
        }
        // 正着走一遍与倒着走一遍，逐帧结果相同（回放/seek 的等价性）。
        let forward: Vec<f32> = (0..20).map(|f| window.envelope(f, 60)).collect();
        let backward: Vec<f32> = (0..20).rev().map(|f| window.envelope(f, 60)).rev().collect();
        assert_eq!(forward, backward, "顺序求值与逆序求值结果不同");
    }

    #[test]
    fn 特效的总强度是包络乘自身不透明度() {
        let effect = Effect {
            kind: "flash".to_string(),
            params: BTreeMap::new(),
            window: Window::Transient { attack: 2, hold: 0, release: 2, fall_to_zero: true },
            opacity: 0.5,
        };
        // 第 0 帧包络 0 -> 总强度 0。
        assert_eq!(effect.strength(0, 60, None), 0.0);
        // 第 1 帧包络 0.5、自身 0.5 -> 0.25。
        assert!((effect.strength(1, 60, None) - 0.25).abs() < 1e-6);
        // 第 2 帧包络 1 -> 0.5。
        assert!((effect.strength(2, 60, None) - 0.5).abs() < 1e-6);
        // 关键帧驱动值**取代**自身不透明度（不是再乘一遍）。
        assert!((effect.strength(2, 60, Some(0.8)) - 0.8).abs() < 1e-6);
    }

    #[test]
    fn 特效强度永远落在零到一之间() {
        // 关键帧给的值可以是任何数（用户在曲线上拖出来的），
        // 所以这一层必须兜住 —— 强度超过 1 会在着色器里变成过曝。
        let effect = Effect {
            kind: "flash".to_string(),
            params: BTreeMap::new(),
            window: Window::Always,
            opacity: 1.0,
        };
        for driven in [-5.0, -0.1, 0.0, 0.5, 1.0, 1.5, 100.0, f32::INFINITY] {
            let value = effect.strength(0, 60, Some(driven));
            assert!(
                (0.0..=1.0).contains(&value),
                "驱动值 {driven} 给出强度 {value}，跑到 [0,1] 之外"
            );
        }
    }

    fn minimal() -> Project {
        Project {
            schema: SCHEMA_VERSION,
            timebase: TimebaseDto { num: 30000, den: 1001 },
            tracks: vec![Track {
                id: "v1".to_string(),
                kind: TrackKind::Video,
                clips: vec![Clip {
                    id: "c1".to_string(),
                    source: "proxy.mp4".to_string(),
                    source_in: 0,
                    track_at: 0,
                    duration: 60,
                    transform: Transform::default(),
                    opacity: 1.0,
                    effects: Vec::new(),
                    keyframes: Vec::new(),
                    transition_in: None,
                }],
            }],
        }
    }

    fn codes(issues: &[Issue]) -> Vec<&str> {
        issues.iter().map(|i| i.code.as_str()).collect()
    }

    const BLUR: EffectSpec = EffectSpec {
        kind: "gaussian_blur",
        params: &[("radius", 0.0, 64.0)],
        space: EffectSpace::Document,
        pipeline: EffectPipeline::SeparableBlur,
        window_default: None,
    };

    #[test]
    fn 最小工程无问题() {
        assert!(validate_project(&minimal()).is_empty());
    }

    #[test]
    fn 未知版本被拒绝且不产生二次错误() {
        let mut project = minimal();
        project.schema = 2;
        // 顺带把一个字段也弄坏：版本不认时不该再报字段的错，否则用户看到的是噪音。
        project.tracks[0].clips[0].duration = -5;
        let issues = validate_project(&project);
        assert_eq!(codes(&issues), vec!["unsupported_schema"]);
    }

    #[test]
    fn 同轨重叠被报出而相邻不算重叠() {
        let mut project = minimal();
        project.tracks[0].clips.push(Clip {
            id: "c2".to_string(),
            source: "proxy.mp4".to_string(),
            source_in: 0,
            track_at: 60, // 前一片段是 0..60，紧邻不算重叠
            duration: 30,
            transform: Transform::default(),
            opacity: 1.0,
            effects: Vec::new(),
            keyframes: Vec::new(),
                    transition_in: None,
        });
        assert!(validate_project(&project).is_empty(), "紧邻不该算重叠");

        // 把它挪到 59 就压上了
        project.tracks[0].clips[1].track_at = 59;
        let issues = validate_project(&project);
        assert_eq!(codes(&issues), vec!["clip_overlap"]);
        assert!(issues[0].message.contains("59"));
    }

    #[test]
    fn 时长与负数被报出() {
        let mut project = minimal();
        project.tracks[0].clips[0].duration = 0;
        project.tracks[0].clips[0].source_in = -1;
        project.tracks[0].clips[0].track_at = -3;
        let issues = validate_project(&project);
        let mut got = codes(&issues);
        got.sort_unstable();
        assert_eq!(got, vec!["duration_not_positive", "negative_source_in", "negative_track_at"]);
    }

    #[test]
    fn 不透明度越界与_nan_都被报出() {
        let mut project = minimal();
        project.tracks[0].clips[0].opacity = 1.5;
        assert_eq!(codes(&validate_project(&project)), vec!["opacity_out_of_range"]);

        // NaN 是最危险的那种：所有比较都为假，若不用 is_finite 就会静默通过
        project.tracks[0].clips[0].opacity = f32::NAN;
        assert_eq!(codes(&validate_project(&project)), vec!["opacity_out_of_range"]);
    }

    #[test]
    fn 关键帧边界() {
        let mut project = minimal();
        // duration = 60，合法范围是 0..=59
        project.tracks[0].clips[0].keyframes = vec![
            Keyframe { frame: 0, target: opacity_target(), value: 0.0, easing: Easing::Linear },
            Keyframe { frame: 59, target: opacity_target(), value: 1.0, easing: Easing::EaseInOut },
        ];
        assert!(validate_project(&project).is_empty(), "边界上的 0 与 59 都应当合法");

        project.tracks[0].clips[0].keyframes[1].frame = 60;
        assert_eq!(codes(&validate_project(&project)), vec!["keyframe_out_of_clip"]);
    }

    #[test]
    fn 片段_id_重复被报出() {
        let mut project = minimal();
        let mut second = project.tracks[0].clips[0].clone();
        second.track_at = 60;
        // id 保持一样
        project.tracks[0].clips.push(second);
        assert_eq!(codes(&validate_project(&project)), vec!["duplicate_clip_id"]);
    }

    #[test]
    fn 特效要照登记表校对() {
        let mut project = minimal();
        project.tracks[0].clips[0].effects = vec![Effect {
            kind: "gaussian_blur".to_string(),
            params: BTreeMap::from([("radius".to_string(), 8.0)]),
            ..Default::default()
        }];
        assert!(validate_project_with_effects(&project, &[BLUR]).is_empty());

        // 未登记的特效
        project.tracks[0].clips[0].effects[0].kind = "sharpen".to_string();
        assert_eq!(
            codes(&validate_project_with_effects(&project, &[BLUR])),
            vec!["unknown_effect"]
        );

        // 参数越界
        project.tracks[0].clips[0].effects[0].kind = "gaussian_blur".to_string();
        project.tracks[0].clips[0].effects[0].params.insert("radius".to_string(), 100.0);
        assert_eq!(
            codes(&validate_project_with_effects(&project, &[BLUR])),
            vec!["effect_param_out_of_range"]
        );

        // 参数名不存在
        project.tracks[0].clips[0].effects[0].params.remove("radius");
        project.tracks[0].clips[0].effects[0].params.insert("sigma".to_string(), 1.0);
        assert_eq!(
            codes(&validate_project_with_effects(&project, &[BLUR])),
            vec!["unknown_effect_param"]
        );
    }

    #[test]
    fn 不传登记表时不检查特效() {
        let mut project = minimal();
        project.tracks[0].clips[0].effects = vec![Effect {
            kind: "随便什么".to_string(),
            params: BTreeMap::new(),
            ..Default::default()
        }];
        assert!(validate_project(&project).is_empty());
    }

    #[test]
    fn 帧率零被报出() {
        let mut project = minimal();
        project.timebase = TimebaseDto { num: 0, den: 1 };
        assert_eq!(codes(&validate_project(&project)), vec!["invalid_timebase"]);
    }


    #[test]
    fn 转场必须紧邻前一片段() {
        let mut project = minimal();
        let mut second = project.tracks[0].clips[0].clone();
        second.id = "c2".to_string();
        second.track_at = 60;
        second.duration = 30;
        second.transition_in = Some(TransitionSpec {
            kind: transition_kind::CROSS_DISSOLVE.to_string(),
            duration: 15,
        });
        project.tracks[0].clips.push(second);
        assert!(validate_project(&project).is_empty(), "紧邻的转场应当合法");

        project.tracks[0].clips[1].track_at = 70;
        assert_eq!(
            codes(&validate_project(&project)),
            vec!["transition_without_previous"]
        );
    }

    #[test]
    fn 轨道首片段不能有入场转场() {
        let mut project = minimal();
        project.tracks[0].clips[0].transition_in = Some(TransitionSpec {
            kind: transition_kind::CROSS_DISSOLVE.to_string(),
            duration: 10,
        });
        assert_eq!(
            codes(&validate_project(&project)),
            vec!["transition_without_previous"]
        );
    }

    #[test]
    fn 转场时长必须正且不超过片段() {
        let mut project = minimal();
        let mut second = project.tracks[0].clips[0].clone();
        second.id = "c2".to_string();
        second.track_at = 60;
        second.duration = 30;
        project.tracks[0].clips.push(second);

        project.tracks[0].clips[1].transition_in = Some(TransitionSpec {
            kind: transition_kind::CROSS_DISSOLVE.to_string(),
            duration: 0,
        });
        assert_eq!(codes(&validate_project(&project)), vec!["transition_duration_invalid"]);

        project.tracks[0].clips[1].transition_in = Some(TransitionSpec {
            kind: transition_kind::CROSS_DISSOLVE.to_string(),
            duration: 31,
        });
        assert_eq!(codes(&validate_project(&project)), vec!["transition_longer_than_clip"]);

        project.tracks[0].clips[1].transition_in = Some(TransitionSpec {
            kind: transition_kind::CROSS_DISSOLVE.to_string(),
            duration: 30,
        });
        assert!(validate_project(&project).is_empty());
    }

    #[test]
    fn 转场不破坏不重叠这条不变量() {
        let mut project = minimal();
        let mut second = project.tracks[0].clips[0].clone();
        second.id = "c2".to_string();
        second.track_at = 60;
        second.transition_in = Some(TransitionSpec {
            kind: transition_kind::CROSS_DISSOLVE.to_string(),
            duration: 20,
        });
        project.tracks[0].clips.push(second);
        let issues = validate_project(&project);
        assert!(
            !issues.iter().any(|i| i.code == "clip_overlap"),
            "转场不该被当成重叠：{:?}",
            codes(&issues)
        );
    }

    #[test]
    fn 缓动的端点与中点() {
        for easing in [
            Easing::Linear,
            Easing::EaseIn,
            Easing::EaseOut,
            Easing::EaseInOut,
            Easing::BackOut,
        ] {
            assert!((easing.apply(0.0) - 0.0).abs() < 1e-6, "{:?} 在 0 处应当是 0", easing);
            assert!((easing.apply(1.0) - 1.0).abs() < 1e-6, "{:?} 在 1 处应当是 1", easing);
            // 越界输入要被夹住，而不是外推
            assert!((easing.apply(-5.0) - 0.0).abs() < 1e-6);
            assert!((easing.apply(5.0) - 1.0).abs() < 1e-6);
        }
        assert!((Easing::EaseInOut.apply(0.5) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn 回弹缓动真的会冲过头() {
        // **这条钉的是 `BackOut` 的定义性质**：它**不是**"更慢的 ease_out"，
        // 而是"冲过头再落回来"。少了这个不变量，把它实现成 EaseOut 也能过端点用例。
        let peak = (1..100)
            .map(|i| Easing::BackOut.apply(i as f32 / 100.0))
            .fold(f32::MIN, f32::max);
        assert!(peak > 1.02, "回弹必须冲过 1，实得峰值 {peak}");
        assert!(peak < 1.2, "但也不该冲得离谱，实得 {peak}");

        // 而 `EaseOut` **永不大于 1**（两者不是同一个东西）。
        let ease_out_peak = (1..100)
            .map(|i| Easing::EaseOut.apply(i as f32 / 100.0))
            .fold(f32::MIN, f32::max);
        assert!(ease_out_peak <= 1.0 + 1e-6, "EaseOut 不该过冲，实得 {ease_out_peak}");
    }

    #[test]
    fn 逐值对上_参照实现的_pow2_两条() {
        // 参照实现 的 `pow2_out(t) = 1-(1-t)^2` 与 `pow2_in(t) = t^2`
        // **与本仓的 EaseOut / EaseIn 逐值相同** —— 所以转译器把那两条
        // 报成"近似"是错的（我先前就报错了）。
        for i in 0..=20 {
            let t = i as f32 / 20.0;
            let want_out = 1.0 - (1.0 - t) * (1.0 - t);
            let want_in = t * t;
            assert!((Easing::EaseOut.apply(t) - want_out).abs() < 1e-6, "pow2_out 在 {t} 处");
            assert!((Easing::EaseIn.apply(t) - want_in).abs() < 1e-6, "pow2_in 在 {t} 处");
        }
    }

    #[test]
    fn json_往返且序列化稳定() {
        let mut project = minimal();
        project.tracks[0].clips[0].effects = vec![Effect {
            // 故意让插入顺序与字典序不同：BTreeMap 必须把它排回来
            kind: "gaussian_blur".to_string(),
            params: BTreeMap::from([
                ("sigma".to_string(), 2.0),
                ("alpha".to_string(), 1.0),
                ("radius".to_string(), 8.0),
            ]),
            ..Default::default()
        }];
        let text = serde_json::to_string(&project).expect("应当能序列化");
        let back: Project = serde_json::from_str(&text).expect("应当能反序列化");
        assert_eq!(project, back);

        // 同样的数据，换个插入顺序，序列化结果必须逐字节相同
        let mut other = project.clone();
        other.tracks[0].clips[0].effects[0].params = BTreeMap::from([
            ("radius".to_string(), 8.0),
            ("alpha".to_string(), 1.0),
            ("sigma".to_string(), 2.0),
        ]);
        assert_eq!(text, serde_json::to_string(&other).unwrap());
        // 字典序在前
        assert!(text.find("alpha").unwrap() < text.find("radius").unwrap());
    }

    #[test]
    fn 校验结果能直接变成_json_给_ui_用() {
        let mut project = minimal();
        project.tracks[0].clips[0].duration = -1;
        let issues = validate_project(&project);
        let text = serde_json::to_string(&issues).expect("问题清单应当能序列化");
        assert!(text.contains("duration_not_positive"));
        assert!(text.contains("tracks[0].clips[0].duration"), "路径要能定位到字段：{text}");
    }
}

