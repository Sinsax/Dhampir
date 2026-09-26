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

use dhampir_timeline::layer::{AssetTimebases, BlendMode, Layer as LayerV2, TimelineV2, source_frame_at};
use dhampir_timeline::schema::{
    Clip, Effect, Frame, Project, TimebaseDto, TrackKind, Transform, TransitionSpec,
};

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
    /// 混合模式。v1 只有 normal；v2 的元素自带它。
    pub blend: BlendMode,
    /// **这一层是不是调整图层**：没有素材、只有特效，要影响「已经画上去的全部内容」。
    ///
    /// 求值层**不做切段**（那是渲染器的事），但必须把位置信息给出去 ——
    /// 渲染器按清单顺序扫，遇到 true 就切：先合成到中间纹理，跑这一层的特效，再往下继续。
    pub is_adjustment: bool,
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

/// 关键帧求值。**逻辑只有一份，在 `dhampir_timeline::curve` 里** —— 这里只是转发。
///
/// 为什么住那边：`Keyframe` / `Easing` 都是 timeline 的形状，而依赖方向只能是
/// core → timeline；timeline 自己（剃刀）也要这份求值。留一个同样的路径在这里，
/// 是让下游一行都不用改（`dhampir_core::compose::opacity_from` 继续存在）。
pub use crate::timeline::curve::opacity_from;

/// 按 `target` 取某一条关键帧曲线。**与 `opacity_from` 同一份逻辑** ——
/// 后者是它的特化（`target = "opacity"`）。
///
/// 放在这里是为了让 `element_to_draw` 能一行取到，且下游不必知道
/// 求值住在 timeline（那件事由 `curve.rs` 的模块头解释）。
pub use crate::timeline::curve::channel_from;

/// 关键帧求值（v1 的入口）。**逻辑只有一份**，在 `opacity_from` 里。
pub fn opacity_at(clip: &Clip, local_frame: Frame) -> f32 {
    opacity_from(clip.opacity, &clip.keyframes, local_frame)
}

/// 转场权重（**与 v1 共用**）。后一个片段在它开头的 duration 帧里，从 0 涨到 1。
/// 第 0 帧是完全的前一个片段（权重 0），第 duration 帧起就完全是自己了。
pub fn transition_weight_from(
    transition_in: Option<&TransitionSpec>,
    local_frame: Frame,
) -> f32 {
    match transition_in {
        Some(spec) if spec.duration > 0 && local_frame < spec.duration => {
            local_frame as f32 / spec.duration as f32
        }
        _ => 1.0,
    }
}

/// 转场权重（v1 的入口）。
pub fn transition_weight(clip: &Clip, local_frame: Frame) -> f32 {
    transition_weight_from(clip.transition_in.as_ref(), local_frame)
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
                    // v1 的契约里没有这两个概念，所以是恒定的默认值。
                    blend: BlendMode::Normal,
                    is_adjustment: false,
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
            // v1 的契约里没有这两个概念，所以是恒定的默认值。
            blend: BlendMode::Normal,
            is_adjustment: false,
        });
    }

    Composite { frame, layers }
}

/// 在**元素模型 v2** 上求值。
///
/// 与 v1 那条路只有一处**结构性**差别：v1 是「轨上唯一一条当前片段」，
/// v2 是每层**自带 start/end**。其余（关键帧不透明度、转场冻帧）
/// **共用同一份实现** —— 复制一遍就会漂，而「两端一致」正是这个项目最贵的东西。
///
/// 调整图层在这里**只被标记、不被处理**：渲染器按清单顺序扫，遇到它才切段。
/// 求值时要用的"外部事实"：时间线的时间基 + 资产时间基表。
///
/// **为什么不让 evaluate_v2 自己去读资产表**：契约层不依赖工程文件壳
/// （方向是 core → timeline，壳在 timeline 里但语义上更靠上）。
/// 传进来的是**事实**，不是壳本身。
struct EvalContext<'a> {
    timeline: &'a TimebaseDto,
    assets: Option<&'a AssetTimebases>,
}

impl EvalContext<'_> {
    /// 把"这一层的第几帧"换算成**素材自己的帧号**。
    ///
    /// 没有资产时间基时**退回恒等**（素材帧率按时间线算）—— 那正是升级前的行为，
    /// 也是"资产表里没登记这个素材"时的唯一合理默认：**不猜帧率**。
    fn source_frame(&self, element: &LayerV2, local_frame: Frame) -> Frame {
        let Some(source) = element.source.as_ref() else {
            return 0;
        };
        let identity = source.source_in.saturating_add(local_frame);
        match self.assets.and_then(|table| table.get(&source.asset_id)) {
            // 时间基不合法时退回恒等而不是 panic：那是**契约层**该报的错
            // （invalid_timebase），渲染器没必要在这里死给你看。
            Some(asset) => source_frame_at(source.source_in, local_frame, self.timeline, asset)
                .unwrap_or(identity),
            None => identity,
        }
    }
}

/// 在**元素模型 v2** 上求值（恒等换算）。
///
/// 等价于"假设素材帧率与时间线一致"。**既有工程与既有测试走的就是这条**，
/// 所以它的行为在 P7 里逐字节未变。
pub fn evaluate_v2(timeline: &TimelineV2, frame: Frame) -> Composite {
    evaluate_v2_with_assets(timeline, frame, None)
}

/// 在元素模型 v2 上求值，并按**资产自己的时间基**换算源帧号。
///
/// 这是 P7 的核心：60fps 的素材放进 30fps 的时间线时，时间线每前进一步
/// 素材要走两步 —— 也就是**舍去一半的画面**，而不是"半速播放"。
pub fn evaluate_v2_with_assets(
    timeline: &TimelineV2,
    frame: Frame,
    assets: Option<&AssetTimebases>,
) -> Composite {
    let ctx = EvalContext { timeline: &timeline.timebase, assets };
    let mut layers = Vec::new();

    for track in &timeline.tracks {
        if track.kind != TrackKind::Video {
            continue;
        }
        // 轨内不许重叠（校验保证），所以活着的至多一条。
        let Some(index) = track.layers.iter().position(|layer| layer.covers(frame)) else {
            continue;
        };
        let layer = &track.layers[index];
        if !layer.enabled {
            continue;
        }
        let local = frame - layer.start;
        let weight = transition_weight_from(layer.transition_in.as_ref(), local);

        // 转场还没走完：把前一层**冻在末帧**垫在下面，权重递减（与 v1 同一套语义）。
        if weight < 1.0 {
            if let Some(previous) = index.checked_sub(1).and_then(|i| track.layers.get(i)) {
                if previous.enabled {
                    let last_local = previous.duration() - 1;
                    let opacity = opacity_from(previous.opacity, &previous.keyframes, last_local)
                        * (1.0 - weight);
                    layers.push(element_to_draw(previous, last_local, opacity, true, &ctx));
                }
            }
        }

        let opacity = opacity_from(layer.opacity, &layer.keyframes, local) * weight;
        layers.push(element_to_draw(layer, local, opacity, false, &ctx));
    }

    Composite { frame, layers }
}

/// 把一个 v2 元素摊成「要画的一层」。
fn element_to_draw(
    element: &LayerV2,
    local_frame: Frame,
    opacity: f32,
    frozen: bool,
    ctx: &EvalContext<'_>,
) -> Layer {
    let keys = &element.keyframes;
    // **元素自身的每个量各自一条曲线**：`opacity` 由调用方按转场权重叠过，
    // 所以这里只取 x/y/scale/rotation —— 它们没有别的来源，静态值就是 fallback。
    //
    // 没有对应 keyframe 时 `channel_from` 返回 fallback（元素上的静态值），
    // 于是老工程（键全是缺省的 "opacity"）结果逐字节不变。
    let animated = |fallback: f32, target: &str| -> f32 {
        crate::timeline::curve::channel_from(fallback, keys, target, local_frame)
    };
    Layer {
        clip_id: element.id.clone(),
        // 调整图层**没有素材** —— 空串是如实的表达，不是占位符。
        // 渲染器靠 is_adjustment 区分它，不靠 source 为空。
        source: element
            .source
            .as_ref()
            .map(|source| source.asset_id.clone())
            .unwrap_or_default(),
        // **按时间换算**，不是直接加 local_frame。见 EvalContext::source_frame。
        source_frame: ctx.source_frame(element, local_frame),
        opacity,
        // v2 的 rotation 是角度制，渲染器要的字段叫 rotation_deg。语义相同，只是名字。
        transform: Transform {
            x: animated(element.transform.x, "x"),
            y: animated(element.transform.y, "y"),
            scale: animated(element.transform.scale, "scale"),
            rotation_deg: animated(element.transform.rotation, "rotation"),
        },
        effects: resolve_effects(element, local_frame, element.duration()),
        frozen_for_transition: frozen,
        blend: element.blend,
        is_adjustment: element.is_adjustment(),
    }
}

/// 求值这一帧的特效清单：**每条的参数都可被同层的关键帧驱动**，
/// **并且每条的时间窗都已经折进它的 `opacity`**。
///
/// `target` 形如 `effect.<下标>.<参数名>`（见 `Keyframe::target`），
/// 只覆盖它指名的那一个键，其余键原样 —— 于是"模糊半径随帧变化"
/// 不需要新特效类型，也不需要新管线。
///
/// # 时间窗为什么在**这里**折进去
///
/// 算一条特效的时间窗需要两样只有求值层才知道的东西：
/// "这一帧是它所属图层的第几帧"（`local_frame`）与"那个图层多长"（`duration`）。
/// 渲染器手上只有图层内偏移，让它回头去查契约等于把同一件事实现两遍。
///
/// 折进 `opacity` 之后，渲染器那边**完全不必知道时间窗的存在** ——
/// 它照旧读 `opacity`，而那已经是"这一帧这条特效该有多强"。
/// 一个通用字段喂四套管线，比给每条管线各加一个 `envelope` 参数干净。
///
/// **老工程逐字节不变**：那时每条都是 `Window::Always`，`envelope` 恒为 1.0。
fn resolve_effects(element: &LayerV2, local_frame: Frame, duration: Frame) -> Vec<Effect> {
    let keys = &element.keyframes;
    let has_effect_target = keys
        .iter()
        .any(|key| crate::timeline::schema::parse_effect_target(&key.target).is_some());
    // 没有任何东西要改时**返回原清单的克隆**：绝大多数帧走这条，不该为它多分配。
    if element.effects.is_empty() {
        return element.effects.clone();
    }

    let mut out = element.effects.clone();
    for effect in out.iter_mut() {
        // 1) 总强度：折进 opacity。窗口外它归零，于是这条特效**什么都不画**。
        //
        // 走 `Effect::strength` 而不是在这里现写一遍乘法 ——
        // "这条特效这一帧多强"只能有**一个**定义，否则求值层与别处
        // （比如 V-Trim 直接读强度时）会算出两个数。
        effect.opacity = effect.strength(local_frame, duration, None);
    }
    if !has_effect_target {
        return out;
    }
    // 2) 关键帧驱动的参数。
    for (index, effect) in out.iter_mut().enumerate() {
        for (param, value) in effect.params.iter_mut() {
            let target = format!("effect.{index}.{param}");
            // 只挑**这个** target 的键。用它自己当前的值当 fallback，
            // 于是"没有键驱动这个参数"与"键算出来就是原值"是同一个结果。
            *value = crate::timeline::curve::channel_from(*value, keys, &target, local_frame);
        }
    }
    out
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

/// v2 的第一条有内容的帧。
///
/// **与 v1 那份分开写而不是共用一个**：v1 的轨道是 clips（轨内唯一一条活着的），
/// v2 是 layers（每层自带 start/end）。共用一个就得先降级到某一种形态，
/// 而那正是"两份真相"的开头。两份实体都只有四行，重复的代价比降级小。
pub fn first_frame_v2(timeline: &TimelineV2) -> Option<Frame> {
    timeline
        .tracks
        .iter()
        .filter(|track| track.kind == TrackKind::Video)
        .flat_map(|track| track.layers.iter())
        .filter(|layer| layer.enabled)
        .map(|layer| layer.start)
        .min()
}

/// v2 的时间线长度（下一位）。空时间线或只有音轨时返回 None。
pub fn end_frame_v2(timeline: &TimelineV2) -> Option<Frame> {
    timeline
        .tracks
        .iter()
        .filter(|track| track.kind == TrackKind::Video)
        .flat_map(|track| track.layers.iter())
        .filter(|layer| layer.enabled)
        .map(|layer| layer.end)
        .max()
}

#[cfg(test)]
mod tests {
    use super::*;
    use dhampir_timeline::schema::{
        transition_kind, Easing, Keyframe, Project, SCHEMA_VERSION, TimebaseDto, Track, TrackKind,
        TransitionSpec,
    };

    /// 缺省 target 的简写：讲不透明度曲线的用例别让 target 喧宾夺主。
    fn kf_target() -> String {
        dhampir_timeline::schema::default_keyframe_target()
    }

    /// 造一条只有一层的 v2 时间线，给动画用例用。
    fn one_layer_v2(layer: dhampir_timeline::layer::Layer) -> TimelineV2 {
        use dhampir_timeline::layer::{LAYER_SCHEMA_VERSION, TrackV2};
        TimelineV2 {
            schema: LAYER_SCHEMA_VERSION,
            timebase: TimebaseDto { num: 30, den: 1 },
            markers: Vec::new(),
            tracks: vec![TrackV2 {
                id: "v1".to_string(),
                kind: TrackKind::Video,
                layers: vec![layer],
                subtitle: None,
                danmaku: None,
            }],
        }
    }

    /// 造一条 v2 元素：只给关心的字段，其余按契约缺省。
    ///
    /// `Layer` 自己没有 `Default`（它是契约类型，加一个会在别处被误用成
    /// "零值元素"），所以测试里这份"缺省"由本地函数提供 ——
    /// 它只影响用例的可读性，不污染契约。
    fn element(
        id: &str,
        start: Frame,
        end: Frame,
        source: Option<dhampir_timeline::layer::SourceRef>,
    ) -> dhampir_timeline::layer::Layer {
        dhampir_timeline::layer::Layer {
            id: id.to_string(),
            start,
            end,
            transform: Default::default(),
            opacity: 1.0,
            blend: BlendMode::Normal,
            enabled: true,
            recorded: Default::default(),
            source,
            effects: Vec::new(),
            transition_in: None,
            keyframes: Vec::new(),
        }
    }

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

    #[test]
    fn 时间窗真的会改变求值出来的特效强度() {
        // **这是 T9 那条"window 是纯函数"之外真正要紧的判据。**
        //
        // 我曾经把 `Window::envelope` 与 `Effect::strength` 写完就搁在那儿 ——
        // 契约里有、工程文件里能写、文档里写着，而**渲染路径一次都没读过它**。
        // 于是"瞬时闪一下"会变成"整段一直闪着"，且不报任何错。
        // 这条用例钉的就是那个断点：窗口必须**真的**走到求值结果里。
        use dhampir_timeline::schema::{Effect, Window};
        use std::collections::BTreeMap;

        let mut layer = element("e1", 0, 60, None);
        layer.effects = vec![Effect {
            kind: "flash".to_string(),
            params: BTreeMap::new(),
            window: Window::Transient { attack: 1, hold: 2, release: 3, fall_to_zero: true },
            opacity: 1.0,
        }];
        let timeline = one_layer_v2(layer);

        // 每一帧上这条特效的实际强度（`opacity` 已被折进时间窗）。
        let strength_at = |frame: Frame| -> f32 {
            evaluate_v2(&timeline, frame)
                .layers
                .iter()
                .flat_map(|layer| layer.effects.iter())
                .map(|effect| effect.opacity)
                .next()
                .expect("这一帧应当有一条特效")
        };

        // 窗口是 attack 1 / hold 2 / release 3 = 6 帧，段边界左闭右开：
        // 上升 [0,1)、满值 [1,3)、回落 [3,6)、之后 0。
        assert_eq!(strength_at(0), 0.0, "第 0 帧还在上升段起点");
        assert_eq!(strength_at(1), 1.0, "hold 段满值");
        assert_eq!(strength_at(3), 1.0, "回落段的起点仍是满值");
        assert!(strength_at(4) < 1.0, "回落段该降下来，实得 {}", strength_at(4));
        assert_eq!(strength_at(6), 0.0, "窗口走完之后必须归零");
        assert_eq!(strength_at(30), 0.0, "之后一直归零 —— 而图层本身还有内容");

        // 反向用例：没有窗口时强度**必须**恒为 1，否则老工程会被改掉。
        let mut plain = element("e2", 0, 60, None);
        plain.effects = vec![Effect {
            kind: "flash".to_string(),
            params: BTreeMap::new(),
            window: Window::Always,
            opacity: 1.0,
        }];
        let plain = one_layer_v2(plain);
        for frame in [0, 1, 30, 59] {
            let strength = evaluate_v2(&plain, frame)
                .layers
                .iter()
                .flat_map(|layer| layer.effects.iter())
                .map(|effect| effect.opacity)
                .next()
                .unwrap();
            assert_eq!(strength, 1.0, "没有窗口时第 {frame} 帧的强度该是满值");
        }
    }

    #[test]
    fn 特效自己的不透明度与时间窗是相乘的() {
        // 两个旋钮管两件事：`opacity` 是"这条特效整体多强"，
        // `window` 是"什么时候生效"。相乘，而不是谁覆盖谁。
        use dhampir_timeline::schema::{Effect, Window};
        use std::collections::BTreeMap;

        let mut layer = element("e1", 0, 60, None);
        layer.effects = vec![Effect {
            kind: "flash".to_string(),
            params: BTreeMap::new(),
            window: Window::Fade { fade_in: 4, fade_out: 4 },
            opacity: 0.5,
        }];
        let timeline = one_layer_v2(layer);
        let strength_at = |frame: Frame| -> f32 {
            evaluate_v2(&timeline, frame)
                .layers
                .iter()
                .flat_map(|layer| layer.effects.iter())
                .map(|effect| effect.opacity)
                .next()
                .unwrap()
        };
        // 淡入一半（第 2 帧）：包络 0.5 × 自身 0.5 = 0.25。
        assert!((strength_at(2) - 0.25).abs() < 1e-6, "实得 {}", strength_at(2));
        // 中段：包络 1 × 自身 0.5 = 0.5。
        assert!((strength_at(30) - 0.5).abs() < 1e-6, "实得 {}", strength_at(30));
    }

    #[test]
    fn 动图的逐帧定位走的是与视频同一套换算() {
        // T12 的**核心断言**：动图不必发明第二条渲染路径。
        //
        // `AssetKind::ImageSequence` 只是登记表里的一个标记（它带来校验规则），
        // 而"这一帧该取第几张"由 `frame_count` + `timebase` 那套算出来 ——
        // 与视频**完全同一份代码**。所以这里用一条 video 轨 + 一个 12fps 的
        // 动图素材，验证时间线帧号确实被换算成了不同的素材帧。
        use dhampir_timeline::layer::{AssetTimebases, SourceRef};

        let timeline = one_layer_v2(element(
            "sticker",
            0,
            30,
            Some(SourceRef { asset_id: "anim.gif".to_string(), source_in: 0 }),
        ));

        // 时间线 30fps，动图 12fps：走 1 秒（30 帧）应当走完 12 张。
        let mut assets = AssetTimebases::new();
        assets.insert("anim.gif".to_string(), TimebaseDto { num: 12, den: 1 });

        let at = |frame: Frame| {
            evaluate_v2_with_assets(&timeline, frame, Some(&assets))
                .layers
                .first()
                .map(|layer| layer.source_frame)
        };

        assert_eq!(at(0), Some(0), "第 0 帧取第 0 张");
        assert_eq!(at(10), Some(4), "时间线 10 帧 = 1/3 秒 = 动图第 4 张");
        assert_eq!(at(20), Some(8));
        // 区间是**左闭右开** `[0, 30)`：第 30 帧已经不在这一层里了。
        assert_eq!(at(29), Some(11), "最后一帧是第 29 帧");
        assert_eq!(at(30), None, "右端不含 —— 第 30 帧不属于这一层");

        // **关键**：相邻时间线帧上，动图的素材帧不总是相同 ——
        // 若这条不成立，"动图"其实就是一张静态图（它会一直停在第一帧）。
        let distinct: std::collections::BTreeSet<_> = (0..30).filter_map(at).collect();
        assert!(
            distinct.len() >= 3,
            "30 帧里只取到 {} 个不同的素材帧，动图等于没动",
            distinct.len()
        );
    }

    #[test]
    fn 没有素材时间基时动图退回恒等换算() {
        // 契约层没登记时间基时**不猜帧率**（那是 `asset_timebases` 注释里的纪律）：
        // 退回"素材帧 == 时间线帧"。表现是动图按时间线的帧率放，
        // 而不是按它自己的 —— 那是个可见的近似，比猜一个数好。
        use dhampir_timeline::layer::SourceRef;

        let timeline = one_layer_v2(element(
            "sticker",
            0,
            30,
            Some(SourceRef { asset_id: "anim.gif".to_string(), source_in: 5 }),
        ));

        let composite = evaluate_v2_with_assets(&timeline, 10, None);
        assert_eq!(composite.layers[0].source_frame, 15, "5 + 10，恒等换算");
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
            Keyframe { frame: 0, target: kf_target(), value: 0.0, easing: Easing::Linear },
            Keyframe { frame: 10, target: kf_target(), value: 1.0, easing: Easing::Linear },
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
            Keyframe { frame: 10, target: kf_target(), value: 1.0, easing: Easing::Linear },
            Keyframe { frame: 0, target: kf_target(), value: 0.0, easing: Easing::Linear },
        ];
        assert!((opacity_at(&c, 5) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn 缓动在后一个关键帧上生效() {
        let mut c = clip("a", 0, 11);
        c.keyframes = vec![
            Keyframe { frame: 0, target: kf_target(), value: 0.0, easing: Easing::Linear },
            Keyframe { frame: 10, target: kf_target(), value: 1.0, easing: Easing::EaseIn },
        ];
        assert!((opacity_at(&c, 5) - 0.25).abs() < 1e-6, "ease_in 是 t*t");
    }

    #[test]
    fn 同一帧上的两个关键帧不除零() {
        let mut c = clip("a", 0, 11);
        c.keyframes = vec![
            Keyframe { frame: 5, target: kf_target(), value: 0.0, easing: Easing::Linear },
            Keyframe { frame: 5, target: kf_target(), value: 1.0, easing: Easing::Linear },
        ];
        let value = opacity_at(&c, 5);
        assert!(value.is_finite(), "不能是 NaN/Inf：{value}");
    }

    #[test]
    fn 转场把前一片段冻在末帧并各占一半() {
        let a = clip("a", 0, 10);
        let mut b = clip("b", 10, 10);
        b.transition_in = Some(TransitionSpec { kind: transition_kind::CROSS_DISSOLVE.to_string(), duration: 4 });
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
        a.transition_in = Some(TransitionSpec { kind: transition_kind::CROSS_DISSOLVE.to_string(), duration: 4 });
        let p = project(vec![video(vec![a])]);
        let composite = evaluate(&p, 0);
        assert_eq!(composite.layers.len(), 1, "没有前驱就不该造层");
        assert!((composite.layers[0].opacity - 0.0).abs() < 1e-6, "从黑里淡进来");
    }

    #[test]
    fn 转场权重与关键帧相乘() {
        let mut b = clip("b", 10, 10);
        b.keyframes = vec![
            Keyframe { frame: 0, target: kf_target(), value: 0.0, easing: Easing::Linear },
            Keyframe { frame: 9, target: kf_target(), value: 1.0, easing: Easing::Linear },
        ];
        b.transition_in = Some(TransitionSpec { kind: transition_kind::CROSS_DISSOLVE.to_string(), duration: 2 });
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

    #[test]
    fn v1_求值出的层带恒定的默认混合与调整标记() {
        use dhampir_timeline::schema::{Clip, Project, TimebaseDto, Track, TrackKind, Transform};
        let project = Project {
            schema: 1,
            timebase: TimebaseDto { num: 30, den: 1 },
            tracks: vec![Track {
                id: "v1".to_string(),
                kind: TrackKind::Video,
                clips: vec![Clip {
                    id: "c1".to_string(),
                    source: "a.mp4".to_string(),
                    source_in: 0,
                    track_at: 0,
                    duration: 10,
                    transform: Transform { x: 0.0, y: 0.0, scale: 1.0, rotation_deg: 0.0 },
                    opacity: 1.0,
                    effects: Vec::new(),
                    keyframes: Vec::new(),
                    transition_in: None,
                }],
            }],
        };
        let composite = evaluate(&project, 0);
        assert_eq!(composite.layers.len(), 1);
        // v1 的契约里**没有**这两个概念。求值结果必须是恒定的默认值 ——
        // 否则就是「从不存在的信息里编出了东西」，那种编造在两端会各自演化。
        assert_eq!(composite.layers[0].blend, BlendMode::Normal);
        assert!(!composite.layers[0].is_adjustment);
    }

    fn v1_fixture() -> Project {
        use dhampir_timeline::schema::{transition_kind, Clip, Project, TimebaseDto, Track, TrackKind, Transform, TransitionSpec};
        let clip = |id: &str, at: Frame, duration: Frame, transition: Option<TransitionSpec>| Clip {
            id: id.to_string(),
            source: format!("{id}.mp4"),
            source_in: 0,
            track_at: at,
            duration,
            transform: Transform { x: 0.0, y: 0.0, scale: 1.0, rotation_deg: 30.0 },
            opacity: 0.5,
            effects: Vec::new(),
            keyframes: Vec::new(),
            transition_in: transition,
        };
        Project {
            schema: 1,
            timebase: TimebaseDto { num: 30, den: 1 },
            tracks: vec![Track {
                id: "v1".to_string(),
                kind: TrackKind::Video,
                clips: vec![
                    clip("a", 0, 10, None),
                    clip(
                        "b",
                        10,
                        10,
                        Some(TransitionSpec { kind: transition_kind::CROSS_DISSOLVE.to_string(), duration: 4 }),
                    ),
                ],
            }],
        }
    }

    #[test]
    fn v1_与_v2_两条求值路径必须给出同一份清单() {
        // **这是本步最重要的一条**：两条路共用关键帧与转场逻辑，
        // 但结构不同（v1 轨上是「当前片段」，v2 每层自带 start/end）。
        // 如果它们对同一份工程给出不同结果，那「两端一致」就有了两个来源。
        use dhampir_timeline::layer::migrate_v1_to_v2;
        let v1 = v1_fixture();
        let v2 = migrate_v1_to_v2(&v1).expect("应当能迁移");
        for frame in 0..20 {
            let from_v1 = evaluate(&v1, frame);
            let from_v2 = evaluate_v2(&v2, frame);
            assert_eq!(from_v1, from_v2, "第 {frame} 帧两条路不一致");
        }
    }

    #[test]
    fn 转场那几帧两条路都给出冻帧层() {
        use dhampir_timeline::layer::migrate_v1_to_v2;
        let v2 = migrate_v1_to_v2(&v1_fixture()).unwrap();
        let composite = evaluate_v2(&v2, 11);
        assert_eq!(composite.layers.len(), 2, "转场中应当有冻帧层 + 当前层");
        assert!(composite.layers[0].frozen_for_transition);
        assert!(!composite.layers[1].frozen_for_transition);
    }

    #[test]
    fn v2_关键帧能驱动_transform_的每个量() {
        use dhampir_timeline::layer::{Layer as LayerV2, Recorded, TransformV2};
        let layer = LayerV2 {
            id: "cam".to_string(),
            start: 0,
            end: 100,
            transform: TransformV2 { x: 0.0, y: 0.0, scale: 1.0, rotation: 0.0 },
            opacity: 1.0,
            blend: BlendMode::Normal,
            enabled: true,
            recorded: Recorded::default(),
            source: None,
            effects: Vec::new(),
            transition_in: None::<TransitionSpec>,
            keyframes: vec![
                Keyframe { frame: 0, target: "scale".to_string(), value: 1.0, easing: Easing::Linear },
                Keyframe { frame: 50, target: "scale".to_string(), value: 2.0, easing: Easing::Linear },
                Keyframe { frame: 0, target: "x".to_string(), value: 0.0, easing: Easing::Linear },
                Keyframe { frame: 50, target: "x".to_string(), value: 100.0, easing: Easing::Linear },
            ],
        };
        let timeline = one_layer_v2(layer);

        // 中点是各自曲线的中点 —— **两条曲线互不干扰**。
        let mid = evaluate_v2(&timeline, 25);
        assert!((mid.layers[0].transform.scale - 1.5).abs() < 1e-5, "scale 应当在推近");
        assert!((mid.layers[0].transform.x - 50.0).abs() < 1e-5, "x 应当平移了一半");

        // 没被任何键驱动的量保持静态值。
        assert_eq!(mid.layers[0].transform.y, 0.0);
        assert_eq!(mid.layers[0].transform.rotation_deg, 0.0);

        // 到位之后停住，不外推。
        let done = evaluate_v2(&timeline, 99);
        assert_eq!(done.layers[0].transform.scale, 2.0);
        assert_eq!(done.layers[0].transform.x, 100.0);
    }

    #[test]
    fn v2_只驱动不透明度的老工程不动_transform() {
        use dhampir_timeline::layer::{Layer as LayerV2, Recorded, TransformV2};
        // 这是"老工程"的形态：只有一个缺省 target 的键。
        // 它**不许**把 x/scale/rotation 拉走 —— 那是泛化最容易踩的错。
        let layer = LayerV2 {
            id: "old".to_string(),
            start: 0,
            end: 100,
            transform: TransformV2 { x: 42.0, y: 7.0, scale: 0.5, rotation: 30.0 },
            opacity: 0.8,
            blend: BlendMode::Normal,
            enabled: true,
            recorded: Recorded::default(),
            source: None,
            effects: Vec::new(),
            transition_in: None::<TransitionSpec>,
            keyframes: vec![
                Keyframe { frame: 0, target: kf_target(), value: 0.0, easing: Easing::Linear },
                Keyframe { frame: 50, target: kf_target(), value: 1.0, easing: Easing::Linear },
            ],
        };
        let mid = evaluate_v2(&one_layer_v2(layer), 25);
        assert_eq!(mid.layers[0].transform.x, 42.0, "x 不该被 opacity 的键动");
        assert_eq!(mid.layers[0].transform.scale, 0.5);
        assert_eq!(mid.layers[0].transform.rotation_deg, 30.0);
        assert!((mid.layers[0].opacity - 0.5).abs() < 1e-5, "不透明度仍由那条曲线驱动");
    }

    #[test]
    fn v2_关键帧能驱动特效参数() {
        use dhampir_timeline::layer::{Layer as LayerV2, Recorded, SourceRef, TransformV2};
        // 「模糊从小涨到大」——**不需要**新的特效类型，只是参数被驱动。
        let layer = LayerV2 {
            id: "blur".to_string(),
            start: 0,
            end: 100,
            transform: TransformV2::default(),
            opacity: 1.0,
            blend: BlendMode::Normal,
            enabled: true,
            recorded: Recorded::default(),
            source: Some(SourceRef { asset_id: "a.mp4".to_string(), source_in: 0 }),
            effects: vec![Effect {
                kind: "gaussian_blur".to_string(),
                params: [("radius".to_string(), 0.0)].into_iter().collect(),
                window: dhampir_timeline::schema::Window::Always,
                opacity: 1.0,
            }],
            transition_in: None::<TransitionSpec>,
            keyframes: vec![
                Keyframe { frame: 0, target: "effect.0.radius".to_string(), value: 0.0, easing: Easing::Linear },
                Keyframe { frame: 10, target: "effect.0.radius".to_string(), value: 8.0, easing: Easing::Linear },
            ],
        };
        let timeline = one_layer_v2(layer);
        let radius = evaluate_v2(&timeline, 5).layers[0].effects[0].params["radius"];
        assert!((radius - 4.0).abs() < 1e-5, "半径应当在第 5 帧到中点，实得 {radius}");
        assert_eq!(
            evaluate_v2(&timeline, 99).layers[0].effects[0].params["radius"],
            8.0,
            "到位后停住"
        );
    }

    #[test]
    fn v2_调整图层被标出来且没有素材() {
        use dhampir_timeline::layer::{Layer as LayerV2, LAYER_SCHEMA_VERSION, Recorded, TrackV2, TransformV2};
        use dhampir_timeline::schema::{Effect, TimebaseDto, TransitionSpec};
        let mut adjustment = LayerV2 {
            id: "adj".to_string(),
            start: 0,
            end: 5,
            transform: TransformV2::default(),
            opacity: 1.0,
            blend: BlendMode::Screen,
            enabled: true,
            recorded: Recorded::default(),
            source: None, // 没有素材
            effects: vec![Effect {
                kind: "gaussian_blur".to_string(),
                params: std::collections::BTreeMap::new(),
                window: dhampir_timeline::schema::Window::Always,
                opacity: 1.0,
            }],
            transition_in: None::<TransitionSpec>,
            keyframes: Vec::new(),
        };
        let timeline = TimelineV2 {
            schema: LAYER_SCHEMA_VERSION,
            timebase: TimebaseDto { num: 30, den: 1 },
            markers: Vec::new(),
            tracks: vec![TrackV2 {
                id: "v1".to_string(),
                kind: TrackKind::Video,
                layers: vec![adjustment.clone()],
                subtitle: None,
                danmaku: None,
            }],
        };
        let composite = evaluate_v2(&timeline, 0);
        assert_eq!(composite.layers.len(), 1);
        assert!(composite.layers[0].is_adjustment, "无素材有特效 → 调整图层");
        assert_eq!(composite.layers[0].blend, BlendMode::Screen, "blend 要透传");
        assert!(composite.layers[0].source.is_empty(), "调整图层没有素材");

        // 关掉的层**不产生层** —— 求值层不该把决定权推给渲染器。
        adjustment.enabled = false;
        let off = TimelineV2 {
            schema: LAYER_SCHEMA_VERSION,
            timebase: TimebaseDto { num: 30, den: 1 },
            markers: Vec::new(),
            tracks: vec![TrackV2 {
                id: "v1".to_string(),
                kind: TrackKind::Video,
                layers: vec![adjustment],
                subtitle: None,
                danmaku: None,
            }],
        };
        assert!(evaluate_v2(&off, 0).layers.is_empty(), "关掉的层不该出现在清单里");
    }
}
