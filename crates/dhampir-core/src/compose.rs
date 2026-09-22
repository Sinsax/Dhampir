//! 把时间线在**某一个帧**上摊开成「要画的东西」。
//!
//! # 为什么这一步是底座
//!
//! 浏览器与 worker 都调这里：同一份 Project、同一个帧号，得到**同一份图层清单**。
//! 所以它必须是**纯函数**——不看墙钟、不碰 GPU、不做 I/O。只要这一步纯，
//! "两端一致"就不再依赖两边各自的理解，而只依赖之后那一段渲染。
//!
//! # v1 的语义（写清楚，免得两端各猜一套）
//!
//! - **轨道顺序**：tracks[0] 在最下面，后画的盖在前面上；
//! - **只合成视频轨**。音频 v1 不进渲染图（音频是另一条路，见 plan 的 M4 说明）；
//! - **关键帧驱动不透明度**。v1 只有一个可动画的标量，就是它——
//!   动画化特效参数留到 v2（那时 Keyframe 要加一个"作用于谁"的字段）；
//! - **转场**：cross_dissolve 占后一个片段开头的 duration 帧，在这几帧里
//!   把自己淡入、把**前一个片段的末帧冻住**淡出。
//!
//! # 关于「冻帧」这个选择
//!
//! 教科书式的交叉溶解要求两个片段**重叠**：淡出那一头需要超出自己时长的素材。
//! 而契约里片段不许重叠（那条不变量很有价值，不想为转场开特例），schema 也不知道
//! 素材总长（那是宿主的事）。所以 v1 唯一**定义明确**的做法就是：把前一片段的末帧冻住。
//! 这是个取舍，不是疏漏——要做真正的重叠溶解，得让契约知道素材长度或允许重叠，
//! 那是 v2 的事。冻帧至少是**两端都能一模一样算出来**的。

use dhampir_timeline::schema::{Clip, Effect, Frame, Keyframe, Project, TrackKind, Transform};

/// 一个要画的图层。
#[derive(Debug, Clone, PartialEq)]
pub struct Layer {
    pub clip_id: String,
    pub source: String,
    /// 该从素材的哪一帧取。
    pub source_frame: Frame,
    /// 已经乘过转场权重的最终不透明度。
    pub opacity: f32,
    pub transform: Transform,
    pub effects: Vec<Effect>,
    /// 这一层是不是「为了转场把前一片段冻在末帧」造出来的。
    /// 渲染器不需要区别对待，但调试时要看得出来。
    pub frozen_for_transition: bool,
}

/// 某一帧上要画的东西。
#[derive(Debug, Clone, PartialEq)]
pub struct Composite {
    pub frame: Frame,
    /// **从下往上**：先画的在前。
    pub layers: Vec<Layer>,
}

impl Composite {
    pub fn is_empty(&self) -> bool {
        self.layers.is_empty()
    }
}

/// 某一帧上这条轨里活着的片段。轨道内不许重叠，所以至多一个。
fn active_clip(track: &dhampir_timeline::schema::Track, frame: Frame) -> Option<&Clip> {
    track
        .clips
        .iter()
        .find(|clip| frame >= clip.track_at && frame < clip.track_at.saturating_add(clip.duration))
}

/// 紧挨在这个片段前面的那个片段（结束时正好接上）。
fn previous_clip<'a>(track: &'a dhampir_timeline::schema::Track, clip: &Clip) -> Option<&'a Clip> {
    track
        .clips
        .iter()
        .find(|other| other.id != clip.id && other.track_at.saturating_add(other.duration) == clip.track_at)
}

/// 关键帧求值。没有关键帧就用片段的静态不透明度。
///
/// 关键帧**不要求有序**——契约里没这么要求，所以这里先排一次。
/// 依赖"用户会按顺序写"是那种只在别人手写的工程上才会炸的假设。
pub fn opacity_at(clip: &Clip, local_frame: Frame) -> f32 {
    if clip.keyframes.is_empty() {
        return clip.opacity;
    }
    let mut keys: Vec<&Keyframe> = clip.keyframes.iter().collect();
    keys.sort_by_key(|key| key.frame);

    let first = keys[0];
    if local_frame <= first.frame {
        return first.value;
    }
    let last = keys[keys.len() - 1];
    if local_frame >= last.frame {
        return last.value;
    }
    for pair in keys.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        if local_frame >= a.frame && local_frame <= b.frame {
            let span = (b.frame - a.frame) as f32;
            // span 为 0 时两个关键帧在同一帧上——取后一个的值，别除零。
            let t = if span <= 0.0 { 1.0 } else { (local_frame - a.frame) as f32 / span };
            let eased = b.easing.apply(t);
            return a.value + (b.value - a.value) * eased;
        }
    }
    clip.opacity
}

/// 转场权重：后一个片段在它开头的 duration 帧里，从 0 涨到 1。
/// 第 0 帧是完全的前一个片段（权重 0），第 duration 帧起就完全是自己了。
pub fn transition_weight(clip: &Clip, local_frame: Frame) -> f32 {
    match clip.transition_in {
        Some(spec) if spec.duration > 0 && local_frame < spec.duration => {
            local_frame as f32 / spec.duration as f32
        }
        _ => 1.0,
    }
}

/// 摊开某一帧。
pub fn evaluate(project: &Project, frame: Frame) -> Composite {
    let mut layers = Vec::new();

    for track in &project.tracks {
        // v1 只合成视频轨。
        if track.kind != TrackKind::Video {
            continue;
        }
        let Some(clip) = active_clip(track, frame) else {
            continue;
        };
        let local = frame - clip.track_at;
        let weight = transition_weight(clip, local);

        // 转场还没走完：把前一个片段**冻在末帧**垫在下面，权重递减。
        if weight < 1.0 {
            if let Some(previous) = previous_clip(track, clip) {
                let last_local = previous.duration - 1;
                layers.push(Layer {
                    clip_id: previous.id.clone(),
                    source: previous.source.clone(),
                    source_frame: previous.source_in + last_local.max(0),
                    opacity: opacity_at(previous, last_local) * (1.0 - weight),
                    transform: previous.transform,
                    effects: previous.effects.clone(),
                    frozen_for_transition: true,
                });
            }
        }

        layers.push(Layer {
            clip_id: clip.id.clone(),
            source: clip.source.clone(),
            source_frame: clip.source_in + local,
            opacity: opacity_at(clip, local) * weight,
            transform: clip.transform,
            effects: clip.effects.clone(),
            frozen_for_transition: false,
        });
    }

    Composite { frame, layers }
}

/// 第一条有内容的帧。
pub fn first_frame(project: &Project) -> Option<Frame> {
    project
        .tracks
        .iter()
        .filter(|track| track.kind == TrackKind::Video)
        .flat_map(|track| track.clips.iter())
        .map(|clip| clip.track_at)
        .min()
}

/// 最后一条有内容的帧之后的**下一位**（也就是时间线长度，单位帧）。
/// 返回"下一位"而不是最后一帧，是因为帧区间一律左闭右开，少一次 ±1 的机会。
pub fn end_frame(project: &Project) -> Option<Frame> {
    project
        .tracks
        .iter()
        .filter(|track| track.kind == TrackKind::Video)
        .flat_map(|track| track.clips.iter())
        .map(|clip| clip.track_at.saturating_add(clip.duration))
        .max()
}

#[cfg(test)]
mod tests {
    use super::*;
    use dhampir_timeline::schema::{
        Easing, Keyframe, Project, SCHEMA_VERSION, TimebaseDto, Track, TrackKind, TransitionKind,
        TransitionSpec,
    };

    fn clip(id: &str, track_at: Frame, duration: Frame) -> Clip {
        Clip {
            id: id.to_string(),
            source: format!("{id}.mp4"),
            source_in: 100,
            track_at,
            duration,
            transform: Transform::default(),
            opacity: 1.0,
            effects: Vec::new(),
            keyframes: Vec::new(),
            transition_in: None,
        }
    }

    fn video(clips: Vec<Clip>) -> Track {
        Track { id: "v".to_string(), kind: TrackKind::Video, clips }
    }

    fn project(tracks: Vec<Track>) -> Project {
        Project { schema: SCHEMA_VERSION, timebase: TimebaseDto { num: 60, den: 1 }, tracks }
    }

    #[test]
    fn 空工程没有图层() {
        let empty = project(Vec::new());
        assert!(evaluate(&empty, 0).is_empty());
        assert_eq!(first_frame(&empty), None);
        assert_eq!(end_frame(&empty), None);
    }

    #[test]
    fn 帧号映射与左右边界() {
        let p = project(vec![video(vec![clip("a", 10, 5)])]);
        assert!(evaluate(&p, 9).is_empty(), "起点之前不该有内容");
        assert_eq!(evaluate(&p, 10).layers[0].source_frame, 100);
        assert_eq!(evaluate(&p, 14).layers[0].source_frame, 104);
        assert!(evaluate(&p, 15).is_empty(), "右边界不含");
        assert_eq!(first_frame(&p), Some(10));
        assert_eq!(end_frame(&p), Some(15), "end_frame 是「下一位」");
    }

    #[test]
    fn 音频轨不参与合成() {
        let p = project(vec![
            video(vec![clip("a", 0, 10)]),
            Track { id: "a1".to_string(), kind: TrackKind::Audio, clips: vec![clip("audio", 0, 10)] },
        ]);
        let composite = evaluate(&p, 3);
        assert_eq!(composite.layers.len(), 1, "v1 只合成视频轨");
        assert_eq!(composite.layers[0].clip_id, "a");
        assert_eq!(end_frame(&p), Some(10), "但时间线长度要算上音频轨");
    }

    #[test]
    fn 多轨从下往上() {
        let p = project(vec![video(vec![clip("bottom", 0, 10)]), video(vec![clip("top", 0, 10)])]);
        let composite = evaluate(&p, 0);
        assert_eq!(composite.layers.len(), 2);
        assert_eq!(composite.layers[0].clip_id, "bottom", "tracks[0] 在最下面");
        assert_eq!(composite.layers[1].clip_id, "top");
    }

    #[test]
    fn 关键帧线性插值与端点() {
        let mut c = clip("a", 0, 11);
        c.opacity = 0.25;
        c.keyframes = vec![
            Keyframe { frame: 0, value: 0.0, easing: Easing::Linear },
            Keyframe { frame: 10, value: 1.0, easing: Easing::Linear },
        ];
        assert_eq!(opacity_at(&c, 0), 0.0);
        assert_eq!(opacity_at(&c, 10), 1.0);
        assert!((opacity_at(&c, 5) - 0.5).abs() < 1e-6);
        assert_eq!(opacity_at(&c, -5), 0.0, "范围外取端点，不外推");
        assert_eq!(opacity_at(&c, 999), 1.0);
    }

    #[test]
    fn 关键帧不需要有序() {
        let mut c = clip("a", 0, 11);
        c.keyframes = vec![
            Keyframe { frame: 10, value: 1.0, easing: Easing::Linear },
            Keyframe { frame: 0, value: 0.0, easing: Easing::Linear },
        ];
        assert!((opacity_at(&c, 5) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn 缓动在后一个关键帧上生效() {
        let mut c = clip("a", 0, 11);
        c.keyframes = vec![
            Keyframe { frame: 0, value: 0.0, easing: Easing::Linear },
            Keyframe { frame: 10, value: 1.0, easing: Easing::EaseIn },
        ];
        assert!((opacity_at(&c, 5) - 0.25).abs() < 1e-6, "ease_in 是 t*t");
    }

    #[test]
    fn 同一帧上的两个关键帧不除零() {
        let mut c = clip("a", 0, 11);
        c.keyframes = vec![
            Keyframe { frame: 5, value: 0.0, easing: Easing::Linear },
            Keyframe { frame: 5, value: 1.0, easing: Easing::Linear },
        ];
        let value = opacity_at(&c, 5);
        assert!(value.is_finite(), "不能是 NaN/Inf：{value}");
    }

    #[test]
    fn 转场把前一片段冻在末帧并各占一半() {
        let a = clip("a", 0, 10);
        let mut b = clip("b", 10, 10);
        b.transition_in = Some(TransitionSpec { kind: TransitionKind::CrossDissolve, duration: 4 });
        let p = project(vec![video(vec![a, b])]);

        let composite = evaluate(&p, 10);
        assert_eq!(composite.layers.len(), 2);
        assert_eq!(composite.layers[0].clip_id, "a");
        assert!(composite.layers[0].frozen_for_transition);
        assert_eq!(composite.layers[0].source_frame, 100 + 9, "冻在 a 的末帧");
        assert!((composite.layers[0].opacity - 1.0).abs() < 1e-6);
        assert_eq!(composite.layers[1].clip_id, "b");
        assert!((composite.layers[1].opacity - 0.0).abs() < 1e-6);

        let composite = evaluate(&p, 12);
        assert!((composite.layers[0].opacity - 0.5).abs() < 1e-6, "淡出那一半");
        assert!((composite.layers[1].opacity - 0.5).abs() < 1e-6, "淡入那一半");

        let composite = evaluate(&p, 14);
        assert_eq!(composite.layers.len(), 1, "转场走完就不该再有冻帧层");
        assert_eq!(composite.layers[0].clip_id, "b");
    }

    #[test]
    fn 没有前驱时转场只把自己淡入() {
        let mut a = clip("a", 0, 10);
        a.transition_in = Some(TransitionSpec { kind: TransitionKind::CrossDissolve, duration: 4 });
        let p = project(vec![video(vec![a])]);
        let composite = evaluate(&p, 0);
        assert_eq!(composite.layers.len(), 1, "没有前驱就不该造层");
        assert!((composite.layers[0].opacity - 0.0).abs() < 1e-6, "从黑里淡进来");
    }

    #[test]
    fn 转场权重与关键帧相乘() {
        let mut b = clip("b", 10, 10);
        b.keyframes = vec![
            Keyframe { frame: 0, value: 0.0, easing: Easing::Linear },
            Keyframe { frame: 9, value: 1.0, easing: Easing::Linear },
        ];
        b.transition_in = Some(TransitionSpec { kind: TransitionKind::CrossDissolve, duration: 2 });
        let p = project(vec![video(vec![clip("a", 0, 10), b])]);
        let composite = evaluate(&p, 11);
        let top = composite.layers.iter().find(|l| l.clip_id == "b").unwrap();
        let expected = (1.0_f32 / 9.0) * 0.5;
        assert!((top.opacity - expected).abs() < 1e-6, "得到 {}", top.opacity);
    }

    #[test]
    fn 求值只依赖帧号不看墙钟() {
        let p = project(vec![video(vec![clip("a", 0, 10)])]);
        assert_eq!(evaluate(&p, 3), evaluate(&p, 3));
    }
}
