//! PR 式编辑操作：**纯函数**，CLI 与预览都调它。
//!
//! # 为什么单独一个模块，而不是写进 CLI
//!
//! 剪辑操作（剃刀、修剪、波纹删除、序列设置）是**业务规则**。
//! 一旦 CLI 和预览各写一遍，两边就会各自演化 —— 而"同一份工程在两条路上结果不同"
//! 是这个项目最贵的那条不变量。所以规则只有这一份：CLI 调它，wasm 也调它，
//! 前端只负责把点击翻成一次调用（check-web-invariants.mjs 的"app.js 不许有业务规则"）。
//!
//! # 一条纪律：**有错就不改**
//!
//! 每个操作都先在副本上改、再走一遍完整校验；只要有一条 error，就把**原样的 doc**
//! 还回去，连同问题清单。半改状态比失败更难查 —— 它会让人以为操作成功了。
//!
//! # 与 PR 对齐的几处语义（这几条最容易做错）
//!
//! * **剃刀**：切点两侧的源帧必须**连续**。右侧的 source_in 要按时间基换算推进，
//!   直接抄左边的 source_in 会让右半段跳回开头。
//! * **修剪**：PR 里拖出点不动入点、拖入点不动画面 —— 也就是 Out 只改 end、
//!   In 改 start 的同时把 source_in 推进相应的素材帧数。
//! * **波纹删除**：删掉之后，**同一条轨道上后面的元素整体左移**被删掉的时长，
//!   而不是留一个洞。
//! * **序列设置改帧率**：所有**序列帧号**（start / end / 转场时长 / 标记 / 关键帧）
//!   都要按**时间**重算。只改 timebase 会让整条时间线变速 —— 那是这类操作里
//!   最容易被漏掉、而且漏了以后"画面看起来完全正常"的一处。
//!   source_in **不动**：它是素材自己的帧号，与序列帧率无关。

use crate::layer::{Layer, source_frame_at};
use crate::project::ProjectDoc;
use crate::project::validate_project_doc;
use crate::schema::{EffectSpec, Frame, Issue, TimebaseDto};

/// 一次编辑的结果。
#[derive(Debug, Clone, PartialEq)]
pub struct EditOutcome {
    /// 通过校验的新工程；**有 error 时是原样的那一份**。
    pub doc: ProjectDoc,
    /// 全部问题（错误 + 警告）。
    pub issues: Vec<Issue>,
    /// 这次操作实际做了什么，给人看的（CLI 打印、界面提示）。
    pub summary: String,
}

/// 校验层里属于**警告**（不阻断）的代码。
///
/// is_ok() 靠这张表判断"这次操作到底成没成"。**新加一种警告就要来这里加一笔** ——
/// 忘了加的表现是"一次成功的编辑被报成失败"，而 edit.rs 里有一条测试专门盯它。
const WARNING_CODES: [&str; 2] = ["unused_asset", "timebase_changed_by_migration"];

impl EditOutcome {
    /// 这次操作成没成（**警告不算失败**）。
    pub fn is_ok(&self) -> bool {
        !self
            .issues
            .iter()
            .any(|issue| !WARNING_CODES.contains(&issue.code.as_str()))
    }
}

/// 修剪哪一边。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrimEdge {
    /// 入点：画面**不动**（保持当前画面），起点与 source_in 一起推进。
    In,
    /// 出点：起点**不动**，只改结束。
    Out,
}

/// 从素材库往轨道上放一个引用。
#[derive(Debug, Clone, PartialEq)]
pub struct InsertRequest {
    pub track_id: String,
    pub asset_id: String,
    /// 放在序列的哪一帧。
    pub at: Frame,
    /// 素材内起点（**素材自己的帧号**）。
    pub source_in: Frame,
    /// 占多长（序列帧数）。
    pub length: Frame,
    /// 不给就按 asset id 生成一个唯一的。
    pub id: Option<String>,
}

/// 把候选工程交出去：过了就返回它，没过就把原样的还回去。
fn commit(original: &ProjectDoc, candidate: ProjectDoc, effects: &[EffectSpec]) -> EditOutcome {
    let issues = validate_project_doc(&candidate, effects);
    let mut all = issues.errors.clone();
    all.extend(issues.warnings.clone());
    if issues.is_ok() {
        EditOutcome { doc: candidate, issues: all, summary: String::new() }
    } else {
        // **一个字段都不改**：让调用方看到的就是"什么都没发生 + 为什么"。
        EditOutcome { doc: original.clone(), issues: all, summary: String::new() }
    }
}

/// 找一个元素在哪儿。返回 (轨下标, 层下标)。
fn locate(doc: &ProjectDoc, layer_id: &str) -> Option<(usize, usize)> {
    for (track_index, track) in doc.timeline.tracks.iter().enumerate() {
        for (layer_index, layer) in track.layers.iter().enumerate() {
            if layer.id == layer_id {
                return Some((track_index, layer_index));
            }
        }
    }
    None
}

/// 全局唯一的元素 id（v2 要求全局唯一）。
fn unique_id(doc: &ProjectDoc, base: &str) -> String {
    let taken = |candidate: &str| {
        doc.timeline
            .tracks
            .iter()
            .any(|track| track.layers.iter().any(|layer| layer.id == candidate))
    };
    if !taken(base) {
        return base.to_string();
    }
    let mut index = 2;
    loop {
        let candidate = format!("{base}-{index}");
        if !taken(&candidate) {
            return candidate;
        }
        index += 1;
    }
}

/// 素材自己的时间基。没登记就退回序列的（等于恒等换算）。
fn asset_timebase(doc: &ProjectDoc, asset_id: &str) -> TimebaseDto {
    doc.assets
        .iter()
        .find(|asset| asset.id == asset_id)
        .and_then(|asset| asset.timebase.clone())
        .unwrap_or_else(|| doc.timeline.timebase.clone())
}

fn no_such_layer(doc: &ProjectDoc, layer_id: &str) -> EditOutcome {
    EditOutcome {
        doc: doc.clone(),
        issues: vec![Issue::new(
            "no_such_layer",
            "timeline",
            format!("没有这个元素：{layer_id}"),
        )],
        summary: String::new(),
    }
}

/// 从素材库往轨道上放一个引用（PR：从素材箱拖到时间线）。
///
/// 轨道内**不许重叠** —— 是覆盖还是插入是两件事，而契约现在只表达得了前者。
/// 重叠会被契约层报 layer_overlap，操作整体不生效。
pub fn insert_reference(
    doc: &ProjectDoc,
    effects: &[EffectSpec],
    request: &InsertRequest,
) -> EditOutcome {
    if request.length <= 0 {
        return EditOutcome {
            doc: doc.clone(),
            issues: vec![Issue::new(
                "length_not_positive",
                "length",
                "放进轨道的长度必须是正的".to_string(),
            )],
            summary: String::new(),
        };
    }
    let Some(track_index) = doc
        .timeline
        .tracks
        .iter()
        .position(|track| track.id == request.track_id)
    else {
        return EditOutcome {
            doc: doc.clone(),
            issues: vec![Issue::new(
                "no_such_track",
                "timeline.tracks",
                format!("没有这条轨道：{}", request.track_id),
            )],
            summary: String::new(),
        };
    };
    let mut candidate = doc.clone();
    let id = match request.id.clone() {
        Some(explicit) => explicit,
        None => unique_id(doc, &request.asset_id),
    };
    candidate.timeline.tracks[track_index].layers.push(Layer {
        id: id.clone(),
        start: request.at,
        end: request.at.saturating_add(request.length),
        transform: crate::layer::TransformV2::default(),
        opacity: 1.0,
        blend: crate::layer::BlendMode::Normal,
        enabled: true,
        recorded: crate::layer::Recorded::default(),
        source: Some(crate::layer::SourceRef {
            asset_id: request.asset_id.clone(),
            source_in: request.source_in,
        }),
        effects: Vec::new(),
        transition_in: None,
        keyframes: Vec::new(),
    });
    // 同一轨内按起点排序：契约不要求有序，但**存下来的顺序稳定**才谈得上逐字节可比。
    candidate.timeline.tracks[track_index]
        .layers
        .sort_by_key(|layer| layer.start);
    let mut outcome = commit(doc, candidate, effects);
    if outcome.is_ok() {
        outcome.summary = format!(
            "在轨道 {} 的第 {} 帧放入 {}（{} 帧）",
            request.track_id, request.at, request.asset_id, request.length
        );
    }
    outcome
}

/// 修剪一个元素的边缘。
pub fn trim(
    doc: &ProjectDoc,
    effects: &[EffectSpec],
    layer_id: &str,
    edge: TrimEdge,
    to_frame: Frame,
) -> EditOutcome {
    let Some((track_index, layer_index)) = locate(doc, layer_id) else {
        return no_such_layer(doc, layer_id);
    };
    let mut candidate = doc.clone();
    let timebase = candidate.timeline.timebase.clone();
    // **先把要用的东西读出来，再拿可变借用。** 借用与改动的边界摆整齐，
    // 混在一起写出来的就是"既要 &mut 又要 &"那种绕不过去的报错。
    let (start, source_in, asset_id) = {
        let layer = &candidate.timeline.tracks[track_index].layers[layer_index];
        (
            layer.start,
            layer.source.as_ref().map(|source| source.source_in),
            layer.source.as_ref().map(|source| source.asset_id.clone()),
        )
    };
    let asset = match asset_id.as_deref() {
        Some(id) => asset_timebase(&candidate, id),
        None => timebase.clone(),
    };
    // 入点：**画面不动** —— 把 source_in 推进"被剪掉的那几帧"对应的素材帧数。
    let advanced = match (edge, source_in) {
        (TrimEdge::In, Some(source_in)) => {
            source_frame_at(source_in, to_frame.saturating_sub(start), &timebase, &asset).ok()
        }
        _ => None,
    };
    let layer = &mut candidate.timeline.tracks[track_index].layers[layer_index];
    match edge {
        TrimEdge::Out => {
            layer.end = to_frame;
        }
        TrimEdge::In => {
            if let Some(value) = advanced {
                if let Some(target) = layer.source.as_mut() {
                    target.source_in = value;
                }
            }
            layer.start = to_frame;
        }
    }
    let mut outcome = commit(doc, candidate, effects);
    if outcome.is_ok() {
        outcome.summary = format!("修剪元素 {layer_id} 到 {to_frame}");
    }
    outcome
}

/// 剃刀：在某一帧把一个元素切成两个。
///
/// **切点两侧的源帧必须连续** —— 右侧的 source_in 是"切点那一刻的素材帧号"，
/// 直接抄左边的会让右半段跳回开头（画面上一眼看得出来，但只有真去看才发现）。
pub fn split(
    doc: &ProjectDoc,
    effects: &[EffectSpec],
    layer_id: &str,
    at_frame: Frame,
) -> EditOutcome {
    let Some((track_index, layer_index)) = locate(doc, layer_id) else {
        return no_such_layer(doc, layer_id);
    };
    let original = doc.timeline.tracks[track_index].layers[layer_index].clone();
    if at_frame <= original.start || at_frame >= original.end {
        return EditOutcome {
            doc: doc.clone(),
            issues: vec![Issue::new(
                "split_outside_layer",
                "at",
                format!(
                    "切点 {at_frame} 必须落在元素区间 ({}, {}) 里面",
                    original.start, original.end
                ),
            )],
            summary: String::new(),
        };
    }
    if !original.keyframes.is_empty() {
        // 带关键帧时，切点两侧要各插一个"当时的值"才不跳变，而那需要插值曲线 ——
        // 它现在只有一份实现，在 core 的求值里。**宁可明说做不到，也不悄悄切歪。**
        return EditOutcome {
            doc: doc.clone(),
            issues: vec![Issue::new(
                "split_across_keyframes",
                "keyframes",
                format!("元素 {layer_id} 带关键帧，剃刀会改变关键帧曲线；请先在关键帧处切开"),
            )],
            summary: String::new(),
        };
    }

    let mut candidate = doc.clone();
    let timebase = candidate.timeline.timebase.clone();
    let asset = original
        .source
        .as_ref()
        .map(|source| asset_timebase(&candidate, &source.asset_id))
        .unwrap_or_else(|| timebase.clone());
    let local = at_frame - original.start;

    let right_id = unique_id(doc, &format!("{layer_id}-b"));
    let mut left = original.clone();
    left.end = at_frame;
    let mut right = original.clone();
    right.id = right_id.clone();
    right.start = at_frame;
    // **源帧连续**：右侧从"切点那一刻的素材帧"开始。
    right.source = match original.source.as_ref() {
        Some(source) => {
            let advanced = source_frame_at(source.source_in, local, &timebase, &asset)
                .unwrap_or_else(|_| source.source_in.saturating_add(local));
            Some(crate::layer::SourceRef {
                asset_id: source.asset_id.clone(),
                source_in: advanced,
            })
        }
        None => None,
    };
    // 转场属于"入场那一刀"，所以留在左半段；右半段的入点是一个硬切。
    right.transition_in = None;
    // 标记是相对元素起点的偏移 —— 右半段只保留落在它里面的，并重新定位。
    right.recorded.markers = original
        .recorded
        .markers
        .iter()
        .filter(|marker| marker.frame >= local)
        .map(|marker| {
            let mut moved = marker.clone();
            moved.frame -= local;
            moved
        })
        .collect();
    left.recorded.markers = original
        .recorded
        .markers
        .iter()
        .filter(|marker| marker.frame < local)
        .cloned()
        .collect();

    let layers = &mut candidate.timeline.tracks[track_index].layers;
    layers[layer_index] = left;
    layers.push(right);
    layers.sort_by_key(|layer| layer.start);

    let mut outcome = commit(doc, candidate, effects);
    if outcome.is_ok() {
        outcome.summary = format!("在 {at_frame} 处把 {layer_id} 切成两段（新元素 {right_id}）");
    }
    outcome
}

/// 移动一个元素（换起点、可选换轨）。移动**不**推动别人 —— 那是波纹的活。
pub fn move_layer(
    doc: &ProjectDoc,
    effects: &[EffectSpec],
    layer_id: &str,
    to_start: Frame,
    to_track: Option<&str>,
) -> EditOutcome {
    let Some((track_index, layer_index)) = locate(doc, layer_id) else {
        return no_such_layer(doc, layer_id);
    };
    let mut candidate = doc.clone();
    let layer = candidate.timeline.tracks[track_index].layers.remove(layer_index);
    let length = layer.end.saturating_sub(layer.start);
    let target_track = match to_track {
        Some(id) => match candidate
            .timeline
            .tracks
            .iter()
            .position(|track| track.id == id)
        {
            Some(index) => index,
            None => {
                return EditOutcome {
                    doc: doc.clone(),
                    issues: vec![Issue::new(
                        "no_such_track",
                        "timeline.tracks",
                        format!("没有这条轨道：{id}"),
                    )],
                    summary: String::new(),
                };
            }
        },
        None => track_index,
    };
    let mut moved = layer;
    moved.start = to_start.max(0);
    moved.end = moved.start.saturating_add(length);
    candidate.timeline.tracks[target_track].layers.push(moved);
    candidate.timeline.tracks[target_track]
        .layers
        .sort_by_key(|layer| layer.start);
    let mut outcome = commit(doc, candidate, effects);
    if outcome.is_ok() {
        outcome.summary = format!("把 {layer_id} 移到第 {to_start} 帧");
    }
    outcome
}

/// 删除一个元素。ripple 为真时，**同一条轨道上后面的元素整体左移**被删掉的时长。
pub fn remove(
    doc: &ProjectDoc,
    effects: &[EffectSpec],
    layer_id: &str,
    ripple: bool,
) -> EditOutcome {
    let Some((track_index, layer_index)) = locate(doc, layer_id) else {
        return no_such_layer(doc, layer_id);
    };
    let mut candidate = doc.clone();
    let removed = candidate.timeline.tracks[track_index]
        .layers
        .remove(layer_index);
    let gap = removed.end.saturating_sub(removed.start);
    if ripple && gap > 0 {
        for layer in candidate.timeline.tracks[track_index].layers.iter_mut() {
            if layer.start >= removed.end {
                layer.start -= gap;
                layer.end -= gap;
            }
        }
    }
    let mut outcome = commit(doc, candidate, effects);
    if outcome.is_ok() {
        outcome.summary = if ripple {
            format!("删除 {layer_id}，后面的元素左移 {gap} 帧")
        } else {
            format!("删除 {layer_id}，留下 {gap} 帧的空档")
        };
    }
    outcome
}

/// 序列设置：改帧率 / 分辨率。
///
/// **改帧率必须重算所有序列帧号**（见模块头）。用四舍五入而不是向下取整：
/// 这一步要保住的是"时刻"，而向下取整会系统性地把每个元素往前偏半帧，
/// 二十个元素之后就明显了。
pub fn set_sequence(
    doc: &ProjectDoc,
    effects: &[EffectSpec],
    timebase: TimebaseDto,
    width: u32,
    height: u32,
) -> EditOutcome {
    if timebase.num == 0 || timebase.den == 0 {
        return EditOutcome {
            doc: doc.clone(),
            issues: vec![Issue::new("invalid_timebase", "timebase", "帧率不合法".to_string())],
            summary: String::new(),
        };
    }
    let from = doc.timeline.timebase.clone();
    let mut candidate = doc.clone();
    let scale = |frame: Frame| rescale_frame(frame, &from, &timebase);

    for track in candidate.timeline.tracks.iter_mut() {
        for layer in track.layers.iter_mut() {
            layer.start = scale(layer.start);
            layer.end = scale(layer.end);
            if let Some(transition) = layer.transition_in.as_mut() {
                transition.duration = scale(transition.duration).max(0);
            }
            for keyframe in layer.keyframes.iter_mut() {
                keyframe.frame = scale(keyframe.frame);
            }
            for marker in layer.recorded.markers.iter_mut() {
                // 元素内标记是**相对**起点的，所以也按时间重算。
                marker.frame = scale(marker.frame);
            }
        }
    }
    for marker in candidate.timeline.markers.iter_mut() {
        marker.frame = scale(marker.frame);
    }
    candidate.timeline.timebase = timebase.clone();
    if width > 0 {
        candidate.render_hints.width = width;
    }
    if height > 0 {
        candidate.render_hints.height = height;
    }
    let mut outcome = commit(doc, candidate, effects);
    if outcome.is_ok() {
        outcome.summary = format!(
            "序列帧率 {}/{} -> {}/{}；输出 {}x{}",
            from.num,
            from.den,
            timebase.num,
            timebase.den,
            outcome.doc.render_hints.width,
            outcome.doc.render_hints.height
        );
    }
    outcome
}

/// 一次编辑操作。**CLI 与 wasm 共用这一个形状** —— 前端不需要认识第二种。
///
/// 用 internally-tagged（一个 op 字段带上其余参数，形如
/// {"op": "split", "layer": "c", "at": 75}）而不是位置参数：
/// 它是**数据**，能被脚本生成、能进日志、能原样重放；
/// 而命令行开关的集合每加一个操作就要动一次解析器。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum EditOp {
    /// 从素材库往轨道上放一个引用。
    Insert {
        track: String,
        asset: String,
        at: Frame,
        source_in: Frame,
        length: Frame,
        #[serde(default)]
        id: Option<String>,
    },
    /// 修剪边缘。
    Trim { layer: String, edge: TrimEdge, to: Frame },
    /// 剃刀。
    Split { layer: String, at: Frame },
    /// 移动。
    Move {
        layer: String,
        to: Frame,
        #[serde(default)]
        track: Option<String>,
    },
    /// 删除（ripple 为真时波纹删除）。
    Remove {
        layer: String,
        #[serde(default)]
        ripple: bool,
    },
    /// 序列设置。
    SetSequence {
        timebase: TimebaseDto,
        #[serde(default)]
        width: u32,
        #[serde(default)]
        height: u32,
    },
}

/// 执行一次编辑操作。**唯一的入口** —— CLI 与 wasm 都走这里。
pub fn apply(doc: &ProjectDoc, effects: &[EffectSpec], op: &EditOp) -> EditOutcome {
    match op {
        EditOp::Insert { track, asset, at, source_in, length, id } => insert_reference(
            doc,
            effects,
            &InsertRequest {
                track_id: track.clone(),
                asset_id: asset.clone(),
                at: *at,
                source_in: *source_in,
                length: *length,
                id: id.clone(),
            },
        ),
        EditOp::Trim { layer, edge, to } => trim(doc, effects, layer, *edge, *to),
        EditOp::Split { layer, at } => split(doc, effects, layer, *at),
        EditOp::Move { layer, to, track } => move_layer(doc, effects, layer, *to, track.as_deref()),
        EditOp::Remove { layer, ripple } => remove(doc, effects, layer, *ripple),
        EditOp::SetSequence { timebase, width, height } => {
            set_sequence(doc, effects, timebase.clone(), *width, *height)
        }
    }
}

/// 把一个序列帧号从一种时间基换算到另一种（**按时间，四舍五入**）。
///
/// 中间量用 i128；分母恒正（num/den 都是 u32 且已校验非零），所以这里
/// 用最朴素的 (n + d/2) / d 就得到"四舍五入"，不需要处理符号。
fn rescale_frame(frame: Frame, from: &TimebaseDto, to: &TimebaseDto) -> Frame {
    if from.num == 0 || from.den == 0 || to.num == 0 || to.den == 0 {
        return frame;
    }
    let numerator = i128::from(frame) * i128::from(from.den) * i128::from(to.num);
    let denominator = i128::from(from.num) * i128::from(to.den);
    if denominator == 0 {
        return frame;
    }
    let rounded = (numerator + denominator / 2) / denominator;
    rounded.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layer::{BlendMode, Marker, Recorded, SourceRef, TrackV2, TransformV2};
    use crate::project::{Asset, AssetKind, shell_from_timeline};
    use crate::schema::{Easing, Keyframe, TrackKind};

    fn tb(num: u32, den: u32) -> TimebaseDto {
        TimebaseDto { num, den }
    }

    /// 一个 60fps 的素材（与 30fps 的序列不同帧率 —— 换算才有得测）。
    fn clip_asset() -> Asset {
        Asset {
            id: "clip".to_string(),
            kind: AssetKind::Video,
            name: "clip".to_string(),
            uri: "proxy.mp4".to_string(),
            frame_count: Some(480),
            timebase: Some(tb(60, 1)),
            width: Some(1920),
            height: Some(1080),
            content_hash: None,
            tags: Default::default(),
            note: String::new(),
        }
    }

    fn layer(id: &str, start: Frame, end: Frame, source_in: Option<Frame>) -> Layer {
        Layer {
            id: id.to_string(),
            start,
            end,
            transform: TransformV2::default(),
            opacity: 1.0,
            blend: BlendMode::Normal,
            enabled: true,
            recorded: Recorded::default(),
            source: source_in.map(|source_in| SourceRef {
                asset_id: "clip".to_string(),
                source_in,
            }),
            effects: Vec::new(),
            transition_in: None,
            keyframes: Vec::new(),
        }
    }

    fn doc(timeline_tb: TimebaseDto, layers: Vec<Layer>) -> ProjectDoc {
        let mut built = shell_from_timeline(crate::layer::TimelineV2 {
            schema: crate::layer::LAYER_SCHEMA_VERSION,
            timebase: timeline_tb,
            markers: Vec::new(),
            tracks: vec![TrackV2 {
                id: "v1".to_string(),
                kind: TrackKind::Video,
                layers,
            }],
        });
        built.assets = vec![clip_asset()];
        built
    }

    fn ids(outcome: &EditOutcome) -> Vec<String> {
        outcome.doc.timeline.tracks[0]
            .layers
            .iter()
            .map(|layer| layer.id.clone())
            .collect()
    }

    #[test]
    fn 剃刀之后两段的源帧必须连续() {
        // 30fps 序列 + 60fps 素材：每步走两帧。
        // 在 10 处切开：左半段最后一帧落在素材 18，右半段必须从 20 开始。
        let original = doc(tb(30, 1), vec![layer("c", 0, 30, Some(0))]);
        let outcome = split(&original, &[], "c", 10);
        assert!(outcome.is_ok(), "{:?}", outcome.issues);
        assert_eq!(ids(&outcome), vec!["c".to_string(), "c-b".to_string()]);
        let left = &outcome.doc.timeline.tracks[0].layers[0];
        let right = &outcome.doc.timeline.tracks[0].layers[1];
        assert_eq!((left.start, left.end), (0, 10));
        assert_eq!((right.start, right.end), (10, 30));
        assert_eq!(left.source.as_ref().unwrap().source_in, 0);
        assert_eq!(
            right.source.as_ref().unwrap().source_in,
            20,
            "右半段必须接着走，不是跳回开头"
        );
    }

    #[test]
    fn 剃刀不接受落在元素外面的切点() {
        let original = doc(tb(30, 1), vec![layer("c", 10, 20, Some(0))]);
        for at in [10, 20, 5, 30] {
            let outcome = split(&original, &[], "c", at);
            assert!(!outcome.is_ok(), "切点 {at} 不该被接受");
            assert_eq!(ids(&outcome), vec!["c".to_string()], "不生效时元素一个都不该变");
        }
    }

    #[test]
    fn 剃刀不碰带关键帧的元素_并且明说为什么() {
        let mut original = doc(tb(30, 1), vec![layer("c", 0, 30, Some(0))]);
        original.timeline.tracks[0].layers[0].keyframes =
            vec![Keyframe { frame: 0, value: 0.0, easing: Easing::Linear }];
        let outcome = split(&original, &[], "c", 10);
        assert!(!outcome.is_ok());
        assert!(outcome
            .issues
            .iter()
            .any(|issue| issue.code == "split_across_keyframes"));
    }

    #[test]
    fn 波纹删除把后面的元素整体左移() {
        let original = doc(
            tb(30, 1),
            vec![layer("a", 0, 10, Some(0)), layer("b", 10, 25, Some(0))],
        );
        let outcome = remove(&original, &[], "a", true);
        assert!(outcome.is_ok(), "{:?}", outcome.issues);
        assert_eq!(ids(&outcome), vec!["b".to_string()]);
        let moved = &outcome.doc.timeline.tracks[0].layers[0];
        assert_eq!((moved.start, moved.end), (0, 15));
    }

    #[test]
    fn 普通删除留下空档() {
        let original = doc(
            tb(30, 1),
            vec![layer("a", 0, 10, Some(0)), layer("b", 10, 25, Some(0))],
        );
        let outcome = remove(&original, &[], "a", false);
        assert!(outcome.is_ok());
        let kept = &outcome.doc.timeline.tracks[0].layers[0];
        assert_eq!((kept.start, kept.end), (10, 25), "不留空档的删除就不是普通删除了");
    }

    #[test]
    fn 移动撞上别的元素要整体不生效() {
        let original = doc(
            tb(30, 1),
            vec![layer("a", 0, 10, Some(0)), layer("b", 100, 110, Some(0))],
        );
        let outcome = move_layer(&original, &[], "b", 5, None);
        assert!(!outcome.is_ok(), "重叠必须被拦住");
        assert!(outcome.issues.iter().any(|issue| issue.code == "layer_overlap"));
        // **原样还回来**：不能留下"b 已经挪过去了"这种半改状态。
        assert_eq!(outcome.doc.timeline.tracks[0].layers[1].start, 100);
    }

    #[test]
    fn 修剪入点保持画面_出点保持起点() {
        let original = doc(tb(30, 1), vec![layer("c", 0, 30, Some(0))]);
        // 入点推到 10：起点变成 10，源内起点推进 20（30->60 是两步一帧）
        let trimmed = trim(&original, &[], "c", TrimEdge::In, 10);
        assert!(trimmed.is_ok(), "{:?}", trimmed.issues);
        let layer = &trimmed.doc.timeline.tracks[0].layers[0];
        assert_eq!((layer.start, layer.end), (10, 30));
        assert_eq!(layer.source.as_ref().unwrap().source_in, 20);

        // 出点拉到 20：起点与源内起点都不动
        let out = trim(&original, &[], "c", TrimEdge::Out, 20);
        assert!(out.is_ok());
        let layer = &out.doc.timeline.tracks[0].layers[0];
        assert_eq!((layer.start, layer.end), (0, 20));
        assert_eq!(layer.source.as_ref().unwrap().source_in, 0);
    }

    #[test]
    fn 改序列帧率要按时间重算所有序列帧号() {
        let mut original = doc(tb(30, 1), vec![layer("c", 0, 30, Some(7))]);
        original.timeline.markers.push(Marker {
            id: "m".to_string(),
            frame: 15,
            name: String::new(),
            color: None,
        });
        original.timeline.tracks[0].layers[0]
            .recorded
            .markers
            .push(Marker {
                id: "lm".to_string(),
                frame: 10,
                name: String::new(),
                color: None,
            });
        let outcome = set_sequence(&original, &[], tb(60, 1), 1280, 720);
        assert!(outcome.is_ok(), "{:?}", outcome.issues);
        let layer = &outcome.doc.timeline.tracks[0].layers[0];
        assert_eq!((layer.start, layer.end), (0, 60), "30 帧 @30fps = 1 秒 -> 60 帧 @60fps");
        assert_eq!(layer.recorded.markers[0].frame, 20, "元素内标记也要按时间走");
        assert_eq!(outcome.doc.timeline.markers[0].frame, 30);
        // **source_in 不动**：它是素材自己的帧号，与序列帧率无关。
        assert_eq!(layer.source.as_ref().unwrap().source_in, 7);
        assert_eq!(outcome.doc.render_hints.width, 1280);
    }

    #[test]
    fn 序列帧率不合法要整体不生效() {
        let original = doc(tb(30, 1), vec![layer("c", 0, 30, Some(0))]);
        let outcome = set_sequence(&original, &[], tb(0, 1), 0, 0);
        assert!(!outcome.is_ok());
        assert_eq!(outcome.doc.timeline.timebase.num, 30, "不许改一半");
    }

    #[test]
    fn 插入引用会按起点排序并给唯一_id() {
        let original = doc(tb(30, 1), vec![layer("clip", 100, 110, Some(0))]);
        // 不给 id -> 按 asset id 生成，且必须避开已占用的 clip
        let outcome = insert_reference(
            &original,
            &[],
            &InsertRequest {
                track_id: "v1".to_string(),
                asset_id: "clip".to_string(),
                at: 0,
                source_in: 0,
                length: 10,
                id: None,
            },
        );
        assert!(outcome.is_ok(), "{:?}", outcome.issues);
        assert_eq!(
            ids(&outcome),
            vec!["clip-2".to_string(), "clip".to_string()],
            "按起点排序"
        );
    }

    #[test]
    fn 插入到不存在的轨道要报错而不是新建() {
        let original = doc(tb(30, 1), vec![]);
        let outcome = insert_reference(
            &original,
            &[],
            &InsertRequest {
                track_id: "nope".to_string(),
                asset_id: "clip".to_string(),
                at: 0,
                source_in: 0,
                length: 10,
                id: None,
            },
        );
        assert!(!outcome.is_ok());
        assert!(outcome.issues.iter().any(|issue| issue.code == "no_such_track"));
    }

    #[test]
    fn 校验层的警告代码都在白名单里() {
        // is_ok() 靠白名单判断"不阻断的提示"。新加一种警告却忘了登记，
        // 会让一次成功的编辑被报成失败 —— 这条就是防它的。
        let mut original = doc(tb(30, 1), vec![layer("c", 0, 30, Some(0))]);
        original.assets.push(Asset { id: "unused".to_string(), ..clip_asset() });
        let issues = validate_project_doc(&original, &[]);
        assert!(issues.is_ok(), "只有警告时不该报错：{:?}", issues.errors);
        assert!(!issues.warnings.is_empty(), "这条测试要一个警告才有意义");
        for warning in &issues.warnings {
            assert!(
                WARNING_CODES.contains(&warning.code.as_str()),
                "未登记的警告代码：{}",
                warning.code
            );
        }
        for error in &issues.errors {
            assert!(
                !WARNING_CODES.contains(&error.code.as_str()),
                "错误被当成警告放行了：{}",
                error.code
            );
        }
    }

    #[test]
    fn 操作的_json_形状能往返() {
        let op = EditOp::Split { layer: "c".to_string(), at: 75 };
        let text = serde_json::to_string(&op).expect("能序列化");
        assert_eq!(text, "{\"op\":\"split\",\"layer\":\"c\",\"at\":75}");
        let back: EditOp = serde_json::from_str(&text).expect("能反序列化");
        assert_eq!(back, op);
        // 修剪的 edge 用 in / out 两个小写词。
        let trim_op = EditOp::Trim { layer: "c".to_string(), edge: TrimEdge::In, to: 3 };
        assert_eq!(
            serde_json::to_string(&trim_op).expect("能序列化"),
            "{\"op\":\"trim\",\"layer\":\"c\",\"edge\":\"in\",\"to\":3}"
        );
    }
}

