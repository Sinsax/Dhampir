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
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TransitionSpec {
    pub kind: TransitionKind,
    /// 占多少帧。必须为正，且不超过本片段的时长。
    pub duration: Frame,
}

#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransitionKind {
    CrossDissolve,
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
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Effect {
    /// 类型串，对应 core 的特效注册表。
    pub kind: String,
    /// 参数用 BTreeMap：同一份工程序列化出来必须**逐字节相同**，HashMap 做不到这点。
    #[serde(default)]
    pub params: BTreeMap<String, f32>,
}

/// 关键帧。frame 是**相对片段起点**的偏移，不是绝对帧号——
/// 这样片段一挪，关键帧跟着走，不会静默错位。
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Keyframe {
    pub frame: Frame,
    pub value: f32,
    #[serde(default)]
    pub easing: Easing,
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
/// 目前只有一种：可分离高斯模糊。留成枚举而不是直接写死，是因为下一个
/// 逐像素查表类特效（亮度/对比度/饱和度）会需要第二种。
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectPipeline {
    /// 横竖两趟的可分离模糊，核是定长展开的。
    SeparableBlur,
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
}

impl EffectSpec {
    /// 某个参数的上界。找不到就是 None（调用方不该猜一个默认上界）。
    pub fn param_max(&self, name: &str) -> Option<f32> {
        self.params.iter().find(|(n, _, _)| *n == name).map(|(_, _, max)| *max)
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
            Keyframe { frame: 0, value: 0.0, easing: Easing::Linear },
            Keyframe { frame: 59, value: 1.0, easing: Easing::EaseInOut },
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
            kind: TransitionKind::CrossDissolve,
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
            kind: TransitionKind::CrossDissolve,
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
            kind: TransitionKind::CrossDissolve,
            duration: 0,
        });
        assert_eq!(codes(&validate_project(&project)), vec!["transition_duration_invalid"]);

        project.tracks[0].clips[1].transition_in = Some(TransitionSpec {
            kind: TransitionKind::CrossDissolve,
            duration: 31,
        });
        assert_eq!(codes(&validate_project(&project)), vec!["transition_longer_than_clip"]);

        project.tracks[0].clips[1].transition_in = Some(TransitionSpec {
            kind: TransitionKind::CrossDissolve,
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
            kind: TransitionKind::CrossDissolve,
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
        for easing in [Easing::Linear, Easing::EaseIn, Easing::EaseOut, Easing::EaseInOut] {
            assert!((easing.apply(0.0) - 0.0).abs() < 1e-6, "{:?} 在 0 处应当是 0", easing);
            assert!((easing.apply(1.0) - 1.0).abs() < 1e-6, "{:?} 在 1 处应当是 1", easing);
            // 越界输入要被夹住，而不是外推
            assert!((easing.apply(-5.0) - 0.0).abs() < 1e-6);
            assert!((easing.apply(5.0) - 1.0).abs() < 1e-6);
        }
        assert!((Easing::EaseInOut.apply(0.5) - 0.5).abs() < 1e-6);
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

