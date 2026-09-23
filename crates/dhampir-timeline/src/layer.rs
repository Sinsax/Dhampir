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

use crate::schema::{Effect, Frame, Keyframe, Project, TimebaseDto, TrackKind, TransitionSpec};

/// v2 契约版本。
pub const LAYER_SCHEMA_VERSION: u32 = 2;

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

/// v2 的轨道：v1 是 `clips`，v2 是 `layers`。
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrackV2 {
    pub id: String,
    pub kind: TrackKind,
#[serde(default)]
    pub layers: Vec<Layer>,
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
                effects: clip.effects.clone(),
                transition_in: clip.transition_in,
                keyframes: clip.keyframes.clone(),
            });
        }
        tracks.push(TrackV2 {
            id: track.id.clone(),
            kind: track.kind,
            layers,
        });
    }
    Ok(TimelineV2 {
        schema: LAYER_SCHEMA_VERSION,
        timebase: project.timebase,
        markers: Vec::new(),
        tracks,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{Clip, Track, Transform};

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
            effects: Vec::new(),
            transition_in: None,
            keyframes: Vec::new(),
        }
    }

    #[test]
    fn 迁移把字段逐个搬对() {
        let migrated = migrate_v1_to_v2(&v1_project()).expect("v1 应当能迁移");
        assert_eq!(migrated.schema, LAYER_SCHEMA_VERSION);
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
