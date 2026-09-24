//! 宿主 API 的**返回体形状**。
//!
//! # 为什么需要这个模块
//!
//! 跨边界的不止渲染契约。wasm 宿主往页面回的这些 JSON 同样跨了边界，
//! 但以前它们是用 `serde_json::json!` 手写的宏 —— **没有类型、没有名字、没有守卫**。
//! 下游一旦依赖它们，就变成了**改不动的隐式契约**，而且比显式契约更危险：
//! 改的人不知道有人在用。
//!
//! 所以把形状提出来命名。**这一步刻意不改任何字段名或结构** ——
//! 目标就是「让形状有名字」，而不是顺手改形状。改形状是另一次破坏性改动。
//!
//! # 版本
//!
//! 形状冻结在 [`HOST_API_VERSION`]。**字段增减都要 +1**，而版本号本身
//! **不在每个返回体里**：往每个形状里塞一个 version 键，每加一次版本就要动所有形状，
//! 而这里的形状是钉死的（每条都有键集断言）—— 问一次记住就够了。
//!
//! 那对端怎么知道对面是哪个版本？问：wasm 侧有 `dhampir_host_api_version`，
//! 文档在 `docs/host-api.md`。**那两处与这个常量由守卫比对**（scripts/api-surface.mjs），
//! 所以「升了常量忘了改文档」不会静默通过。
//!
//! ## v1 -> v2（T2.7）
//!
//! [`FrameResult`] 多了 `overlay`：这一帧的文字覆盖层（要画哪几行字、各占哪个归一化矩形）。
//! 加键就是**破坏性改动** —— 按本仓口径（对端拿到的形状变了）版本要跟着升。
//!
//! ## v2 -> v3（T3.2c）
//!
//! [`OverlayView`] 多了 `danmaku` 与 `dropped_danmaku`：弹幕与字幕共用这一帧的文字覆盖层，
//! 但**分成两个键**（落点规则不同：字幕居中于整条目标宽，弹幕按自己的宽度左对齐）。
//! 又是加键，所以再升一次版本。
//!
//! 弹幕那一条带上 `lane` / `enter` / `exit`：只比矩形的话，**泳道被分配错了**
//! （两条换了位置）在单帧里可能完全看不出来 —— 而那正是两端最容易漂的地方。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::layer::{BlendMode, LAYER_SCHEMA_VERSION, TimelineV2};
use crate::schema::{Frame, Issue, TimebaseDto};

/// `dhampir_demux_samples` 里的一条样本。
///
/// **键名保持单字母**（o/s/d/u/k）：那是当初手写时定下的，改键名属于破坏性改动。
/// 但字段名现在是可读的 —— 这正是「给形状命名」的意义：
/// 以前它只是 json! 里的一个字符串，没人说得清 d 到底是偏移还是时间戳。
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SampleView {
    #[serde(rename = "o")]
    pub offset: usize,
    #[serde(rename = "s")]
    pub size: usize,
    #[serde(rename = "d")]
    pub dts: u64,
    #[serde(rename = "u")]
    pub duration: u32,
    #[serde(rename = "k")]
    pub is_sync: bool,
}

/// `dhampir_cache_stats` 的返回体。
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheStatsView {
    pub vram_bytes: usize,
    pub ram_bytes: usize,
    pub vram_len: usize,
    pub ram_len: usize,
    pub vram_over: bool,
    pub ram_over: bool,
}
/// `dhampir_project_render_probe` 的返回体（离屏渲染的像素摘要）。
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProbeResult {
    pub frame: Frame,
    pub width: u32,
    pub height: u32,
    /// 这一帧实际画了几层。
    pub layers: usize,
    pub bytes: usize,
    /// 像素的 FNV-1a 64，十六进制。
    pub digest: String,
}
/// 一份实现的能力声明。
///
/// **为什么需要它**：`BlendMode::is_implemented()` 是**编译期**的知识，
/// 而分离模式下「对方能不能做」是**运行期**的事实 —— 前端新、后端旧是常态。
/// 把编译期的假设当成运行期的保证，正是那种「在自己机器上永远测不出来」的坑。
///
/// 没有它，用户配了一个后端不支持的东西，要等**分钟级任务跑完**才报错。
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Capabilities {
    /// 对端支持的契约版本。
    pub timeline_versions: Vec<u32>,
    /// **只列真正实现的**混合模式。
    pub blend_modes: Vec<BlendMode>,
    /// 支持的特效 kind。
    pub effects: Vec<String>,
    /// 能渲染的最大模糊半径（0 = 不支持模糊）。
    pub max_blur_radius: u32,
    /// 有没有解码器。
    pub has_decoder: bool,
    /// 能不能编码成片（浏览器只有 PNG 序列，所以这里是 false）。
    pub has_encoder: bool,
}

impl Capabilities {
    /// 由**编译期的谓词**推出可推的部分；宿主特有的事实由调用方给。
    ///
    /// 特效清单之所以要调用方给：timeline 不能依赖 core（方向是 core → timeline），
    /// 所以这里读不到特效登记表。这是方向约束，不是偷懒。
    pub fn new(
        effects: Vec<String>,
        max_blur_radius: u32,
        has_decoder: bool,
        has_encoder: bool,
    ) -> Self {
        Self {
            timeline_versions: vec![LAYER_SCHEMA_VERSION],
            // **从谓词筛，不手写** —— 手写一份支持清单一定会与渲染器漂开。
            blend_modes: BlendMode::ALL
                .into_iter()
                .filter(|mode| mode.is_implemented())
                .collect(),
            effects,
            max_blur_radius,
            has_decoder,
            has_encoder,
        }
    }
}

/// **出片前的预检**：这份工程里有没有超出对端能力的东西。
///
/// 与 `unimplemented_blends()` 的区别很重要：那个问「**我自己**能不能做」，
/// 这个问「**对端**能不能做」。分离模式下这是两个不同的问题。
///
/// 返回的是**提交前**就能报的错，而不是等任务跑完。
pub fn precheck(timeline: &TimelineV2, capabilities: &Capabilities) -> Vec<Issue> {
    let mut issues = Vec::new();
    if !capabilities.timeline_versions.contains(&timeline.schema) {
        issues.push(Issue::new(
            "schema_unsupported_by_peer",
            "timeline.schema",
            format!(
                "对端只认契约 v{:?}，而这份是 v{}",
                capabilities.timeline_versions, timeline.schema
            ),
        ));
        // 版本都不认，字段的含义无从谈起 —— 直接返回，别给一堆二次错误。
        return issues;
    }
    for (track_index, track) in timeline.tracks.iter().enumerate() {
        for (layer_index, layer) in track.layers.iter().enumerate() {
            let path = format!("tracks[{track_index}].layers[{layer_index}]");
            if !capabilities.blend_modes.contains(&layer.blend) {
                issues.push(Issue::new(
                    "blend_unsupported_by_peer",
                    &format!("{path}.blend"),
                    format!("对端不支持混合模式 {:?}", layer.blend),
                ));
            }
            for (effect_index, effect) in layer.effects.iter().enumerate() {
                let effect_path = format!("{path}.effects[{effect_index}]");
                if !capabilities.effects.iter().any(|kind| kind == &effect.kind) {
                    issues.push(Issue::new(
                        "effect_unsupported_by_peer",
                        &effect_path,
                        format!("对端不支持特效 {}", effect.kind),
                    ));
                    continue;
                }
                // 参数上限也属于能力：登记表知道契约允许到哪，对端才知道自己做到哪。
                if let Some(radius) = effect.params.get("radius") {
                    if radius.is_finite() && *radius > capabilities.max_blur_radius as f32 {
                        issues.push(Issue::new(
                            "effect_exceeds_capability",
                            &format!("{effect_path}.params.radius"),
                            format!(
                                "半径 {radius} 超出对端上限 {}",
                                capabilities.max_blur_radius
                            ),
                        ));
                    }
                }
            }
        }
    }
    issues
}

/// 序列化成 JSON 字符串。
///
/// 所有字段都可序列化，所以这里的兜底**理论上不可达** ——
/// 但把兜底放在一个地方，好过在每个导出里各写一次 unwrap。
pub fn to_json<T: Serialize>(value: &T) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| String::from("{\"error\":\"序列化失败\"}"))
}
/// 形状版本。**字段增减都要 +1。**
///
/// 对端问版本用的是 wasm 侧的 `dhampir_host_api_version`，不是这个常量本身 ——
/// 文档在 `docs/host-api.md`，那份与这个值由 `scripts/api-surface.mjs` 比对。
///
/// ## v3 -> v4（T4.3）
///
/// 多了一对导出 `dhampir_project_undo` / `dhampir_project_redo`。
/// **形状一个都没变**（两者都沿用编辑那套 `{ok, summary, issues}`）—— 升版本是因为
/// 对端能看到的**导出面**变了：老前端不会知道有这两条路，而新前端对着老宿主调它们
/// 会直接 `undefined`。这正是「问一次版本」要拦的那类错配。
pub const HOST_API_VERSION: u32 = 4;

/// `dhampir_project_open` 的返回体。
///
/// 两种形态：解析成功带 `issues`，解析失败带 `error`。
/// 用 `skip_serializing_if` 保证**每个形态只出现自己那几个键** ——
/// 直接 `Option` 序列化会多出 `null` 字段，那是形状变化。
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpenResult {
    pub parsed: bool,
    pub ok: bool,
#[serde(default, skip_serializing_if = "Option::is_none")]
    pub issues: Option<Vec<Issue>>,
    /// **warnings**：不阻断载入（例如「登记了但没被引用」）。
    /// 与 errors 分开而不是给 Issue 加 severity —— Issue 是已冻结契约的一部分。
#[serde(default, skip_serializing_if = "Option::is_none")]
    pub warnings: Option<Vec<Issue>>,
#[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl OpenResult {
    /// 解析成功。`ok` 表示**校验**是否通过（解析成功但校验不过也是正常的）。
    pub fn opened(issues: Vec<Issue>) -> Self {
        Self { parsed: true, ok: issues.is_empty(), issues: Some(issues), warnings: None, error: None }
    }

    /// 从工程文件的校验结果来。**errors 决定 ok，warnings 单独给出** ——
    /// 把警告混进 errors 会让「有提示」看起来像「不能用」。
    pub fn from_doc_issues(issues: &crate::project::DocIssues) -> Self {
        Self {
            parsed: true,
            ok: issues.is_ok(),
            issues: Some(issues.errors.clone()),
            warnings: Some(issues.warnings.clone()),
            error: None,
        }
    }

    /// 连 JSON 都没解析成功。
    pub fn unparsed(message: String) -> Self {
        Self { parsed: false, ok: false, issues: None, warnings: None, error: Some(message) }
    }
}

/// 图层清单里的变换。**键名仍是 `rotation_deg`** ——
/// v2 的契约把它改名成了 `rotation`，但这是宿主 API 的形状，改名属于破坏性改动，不在这里顺手做。
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TransformView {
    pub x: f32,
    pub y: f32,
    pub scale: f32,
    pub rotation_deg: f32,
}

#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EffectView {
    pub kind: String,
    pub params: BTreeMap<String, f32>,
}

/// 图层清单里的一层。
///
/// `clip_id` 这个名字**比 v2 的 `layer` 命名早**。保留它是因为改键名是破坏性改动 ——
/// 而这一步的目标只是给形状命名。
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LayerView {
    pub clip_id: String,
    pub source: String,
    pub source_frame: Frame,
    pub opacity: f32,
    /// 是不是「为了转场把前一片段冻在末帧」造出来的那一层。
    pub frozen_for_transition: bool,
    pub transform: TransformView,
    pub effects: Vec<EffectView>,
}

/// 归一化矩形（相对**文档坐标系**）：与 `text_layout::NormalizedRect` 逐字段同名。
///
/// 形状在这里**再定义一次**，而不是直接拿那个类型来序列化：宿主 API 的形状是本模块的事，
/// 而 `NormalizedRect` 是共享几何的一部分 —— 让后者牵着前者走，等于把契约形状
/// 交给一个可以随时改的内部结构。两边的对应关系由下面那个 `From` 实现与它的单测钉住。
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RectView {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl From<crate::text_layout::NormalizedRect> for RectView {
    fn from(rect: crate::text_layout::NormalizedRect) -> Self {
        Self { x: rect.x, y: rect.y, width: rect.width, height: rect.height }
    }
}

/// 要画的一行字：内容 + 它占的行盒（归一化，相对文档坐标系）。
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TextItemView {
    pub text: String,
    pub rect: RectView,
}

impl From<crate::text_layout::TextLine> for TextItemView {
    fn from(line: crate::text_layout::TextLine) -> Self {
        Self { text: line.text, rect: RectView::from(line.rect) }
    }
}

/// 要画的一条弹幕：内容 + **这一帧**的矩形 + 泳道与在屏区间。
///
/// 四个字段一起给，是为了让「两端给出同一张表」**能逐字段对账**：只比矩形的话，
/// 泳道被分配错了（两条换了位置）在单帧里可能看不出来。
///
/// `rect` 是**这一帧**的滚动位置（帧的函数），不像字幕那样是静态行盒 ——
/// 别把它当成「这条弹幕的框」存起来复用。
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DanmakuItemView {
    pub text: String,
    pub rect: RectView,
    /// 第几泳道。0 是**最上面**那条。
    pub lane: u32,
    /// 第一次出现的帧（闭区间起点）。
    pub enter: Frame,
    /// 最后一次出现的帧（**闭**区间终点）。
    pub exit: Frame,
}

/// 某一帧的文字覆盖层：这一帧还要画哪几行字、哪几条弹幕。
///
/// # 为什么与 `layers` 分开
///
/// 图层说的是「哪张纹理怎么叠」，文字**没有纹理** —— 它要先由宿主栅格化成位图。
/// 塞进一个结构里会让「谁负责栅格化」变含糊，而含糊的代价是两端各自决定。
/// 所以这里是两个字段，不是把字塞进某一张 `LayerView`。
///
/// # 为什么字幕与弹幕也分开
///
/// 两者的**落点规则不同**：字幕的每一行居中于整条目标宽，弹幕按自己的宽度左对齐、
/// 位置是帧的函数。混在一个数组里就得在每个元素上带一个种类标签 ——
/// 而那与「这里是纯结构」的定位冲突。
///
/// # 键名与另外两处逐字段同名
///
/// `items[{text,rect}]`、`danmaku[{text,rect,lane,enter,exit}]`、`color`、`outline`、
/// `dropped_lines`、`dropped_danmaku` 与 CLI 的 `subtitle` 子命令、预览宿主的
/// `dhampir_project_text_frame` 一致 —— 三处同名，比对时不需要一张映射表（映射表自己会漂）。
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OverlayView {
    /// 按轨道顺序（先画的在前），轨内按行。
    pub items: Vec<TextItemView>,
    /// 同一帧里活着的弹幕条，按轨道顺序、轨内按分配顺序。
    pub danmaku: Vec<DanmakuItemView>,
    /// 文字颜色，RGBA。来自轨道样式。**弹幕也用它**。
    pub color: [u8; 4],
    /// 是否描边。来自轨道样式。**弹幕也用它**。
    pub outline: bool,
    /// 因为超过 `max_lines` 被丢弃的**行数**（所有字幕轨加起来）。
    pub dropped_lines: usize,
    /// 因为泳道排不下被丢弃的**弹幕条数**（所有弹幕轨加起来）。
    pub dropped_danmaku: usize,
}

/// `dhampir_project_frame` 的返回体。
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FrameResult {
    pub frame: Frame,
    pub layers: Vec<LayerView>,
    /// 这一帧的文字覆盖层。
    ///
    /// **没有字幕时这个键根本不出现**，而不是给一个空数组：「没有字幕」与
    /// 「有字幕但这一帧是空的」在评估层就是同一个答案（`None`），
    /// 在这里为它们造出两种形状，等于把那个区分又搬回来一次。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overlay: Option<OverlayView>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// `dhampir_project_sources_for` 里的一条源。
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceView {
    pub source: String,
    pub source_frame: Frame,
    /// 由**整数帧号**经时间基算出的秒数（frame * den / num）。
    pub seconds: f64,
}

/// `dhampir_project_sources_for` 的返回体。
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourcesResult {
    pub frame: Frame,
    pub sources: Vec<SourceView>,
#[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 取一个 JSON 对象的所有键，排序后返回。
    ///
    /// **排序是为了让断言与键的书写顺序无关** —— 否则它测的是序列化顺序，不是形状。
    fn keys(value: &serde_json::Value) -> Vec<String> {
        let mut out: Vec<String> = value
            .as_object()
            .expect("应当是对象")
            .keys()
            .cloned()
            .collect();
        out.sort();
        out
    }

    fn sorted(items: &[&str]) -> Vec<String> {
        let mut out: Vec<String> = items.iter().map(|s| s.to_string()).collect();
        out.sort();
        out
    }

    #[test]
    fn 打开成功只出现三个键() {
        let result = OpenResult::opened(Vec::new());
        assert!(result.ok);
        let value = serde_json::to_value(&result).unwrap();
        assert_eq!(keys(&value), sorted(&["parsed", "ok", "issues"]));
        // 关键：**不许出现 error 的 null**。Option 直接序列化会多出这个键，那就是形状变化。
        assert!(value.get("error").is_none(), "成功形态不该有 error 键");
    }

    #[test]
    fn 打开失败只出现三个键() {
        let result = OpenResult::unparsed("不是 JSON".to_string());
        let value = serde_json::to_value(&result).unwrap();
        assert_eq!(keys(&value), sorted(&["parsed", "ok", "error"]));
        assert!(value.get("issues").is_none(), "失败形态不该有 issues 键");
    }

    #[test]
    fn 工程文件校验有警告时_ok_仍为真_但警告要出现() {
        // 「有提示」不该看起来像「不能用」。
        let issues = crate::project::DocIssues {
            errors: Vec::new(),
            warnings: vec![Issue::new("unused_asset", "assets[0]", "登记了但没被引用".to_string())],
        };
        let result = OpenResult::from_doc_issues(&issues);
        assert!(result.parsed && result.ok);
        let value = serde_json::to_value(&result).unwrap();
        assert_eq!(keys(&value), sorted(&["parsed", "ok", "issues", "warnings"]));
        assert_eq!(value["warnings"].as_array().map(Vec::len), Some(1));
    }

    #[test]
    fn 工程文件校验有错时_ok_为假() {
        let issues = crate::project::DocIssues {
            errors: vec![Issue::new("nope", "p", "m".to_string())],
            warnings: Vec::new(),
        };
        let result = OpenResult::from_doc_issues(&issues);
        assert!(result.parsed);
        assert!(!result.ok);
    }

    #[test]
    fn 校验不过时_parsed_为真而_ok_为假() {
        // 这两个字段是**两件事**：JSON 能不能解析 vs 工程能不能用。
        let issue = Issue::new("x", "y", "z".to_string());
        let result = OpenResult::opened(vec![issue]);
        assert!(result.parsed, "解析成功了");
        assert!(!result.ok, "但校验没过");
    }

    #[test]
    fn 图层清单的键是钉死的() {
        let result = FrameResult {
            frame: 0,
            layers: vec![LayerView {
                clip_id: "l1".to_string(),
                source: "a.mp4".to_string(),
                source_frame: 10,
                opacity: 0.5,
                frozen_for_transition: true,
                transform: TransformView { x: 1.0, y: 2.0, scale: 1.5, rotation_deg: 90.0 },
                effects: vec![EffectView {
                    kind: "gaussian_blur".to_string(),
                    params: BTreeMap::from([("radius".to_string(), 4.0_f32)]),
                }],
            }],
            overlay: None,
            error: None,
        };
        let value = serde_json::to_value(&result).unwrap();
        assert_eq!(keys(&value), sorted(&["frame", "layers"]));
        let layer = &value["layers"][0];
        assert_eq!(
            keys(layer),
            sorted(&[
                "clip_id",
                "source",
                "source_frame",
                "opacity",
                "frozen_for_transition",
                "transform",
                "effects"
            ])
        );
        assert_eq!(keys(&layer["transform"]), sorted(&["x", "y", "scale", "rotation_deg"]));
        assert_eq!(keys(&layer["effects"][0]), sorted(&["kind", "params"]));
    }

    #[test]
    fn 变换的键名仍是_rotation_deg() {
        // v2 的**契约**把它改名成了 rotation，但宿主 API 的形状改名是另一次破坏性改动。
        // 这条测试把这个「刻意不改」钉住 —— 否则将来有人会顺手改掉。
        let transform = TransformView { x: 0.0, y: 0.0, scale: 1.0, rotation_deg: 0.0 };
        let value = serde_json::to_value(transform).unwrap();
        assert!(value.get("rotation_deg").is_some());
        assert!(value.get("rotation").is_none());
    }

    #[test]
    fn 源清单的键是钉死的() {
        let result = SourcesResult {
            frame: 30,
            sources: vec![SourceView {
                source: "a.mp4".to_string(),
                source_frame: 29,
                seconds: 29.0 / 30.0,
            }],
            error: None,
        };
        let value = serde_json::to_value(&result).unwrap();
        assert_eq!(keys(&value), sorted(&["frame", "sources"]));
        assert_eq!(keys(&value["sources"][0]), sorted(&["source", "source_frame", "seconds"]));
    }

    #[test]
    fn 没有工程时是带_error_的空清单() {
        let result = FrameResult {
            frame: 0,
            layers: Vec::new(),
            overlay: None,
            error: Some("还没有载入通过校验的工程".to_string()),
        };
        let value = serde_json::to_value(&result).unwrap();
        assert_eq!(keys(&value), sorted(&["frame", "layers", "error"]));
    }

    /// 有文字时：**多出来的是 overlay 这一个键**，且它的子键集是钉死的。
    #[test]
    fn 有文字时多出_overlay_这一个键() {
        let result = FrameResult {
            frame: 7,
            layers: Vec::new(),
            overlay: Some(OverlayView {
                items: vec![TextItemView {
                    text: "第一行中文".to_string(),
                    rect: RectView { x: 0.25, y: 0.8, width: 0.5, height: 0.066 },
                }],
                danmaku: vec![DanmakuItemView {
                    text: "飘过".to_string(),
                    rect: RectView { x: 0.9, y: 0.0, width: 0.08, height: 0.048 },
                    lane: 0,
                    enter: 10,
                    exit: 250,
                }],
                color: [255, 240, 200, 255],
                outline: false,
                dropped_lines: 3,
                dropped_danmaku: 2,
            }),
            error: None,
        };
        let value = serde_json::to_value(&result).unwrap();
        assert_eq!(keys(&value), sorted(&["frame", "layers", "overlay"]));
        let overlay = &value["overlay"];
        assert_eq!(
            keys(overlay),
            sorted(&["items", "danmaku", "color", "outline", "dropped_lines", "dropped_danmaku"])
        );
        assert_eq!(keys(&overlay["items"][0]), sorted(&["text", "rect"]));
        assert_eq!(keys(&overlay["items"][0]["rect"]), sorted(&["x", "y", "width", "height"]));
        // 弹幕那一条：泳道与在屏区间必须在，否则「泳道分配错了」在单帧里查不出来。
        assert_eq!(
            keys(&overlay["danmaku"][0]),
            sorted(&["text", "rect", "lane", "enter", "exit"])
        );
        assert_eq!(keys(&overlay["danmaku"][0]["rect"]), sorted(&["x", "y", "width", "height"]));
        assert_eq!(overlay["danmaku"][0]["lane"], serde_json::json!(0));
        assert_eq!(overlay["danmaku"][0]["exit"], serde_json::json!(250));
    }

    /// **反向**：没有文字时不许出现 `overlay` 键。
    ///
    /// `Option` 直接序列化会多出一个 `null`，那是形状变化 —— 对端拿到的
    /// 「这一帧什么都没有」与「这个宿主不懂文字」会长得一模一样。
    #[test]
    fn 没有文字时不许出现_overlay_键() {
        let result = FrameResult { frame: 0, layers: Vec::new(), overlay: None, error: None };
        let value = serde_json::to_value(&result).unwrap();
        assert!(value.get("overlay").is_none(), "没有文字时不该有 overlay 键");
        // 反过来说：有文字时**必须**有，否则这条反向用例本身是恒真的。
        let with_text = FrameResult {
            frame: 0,
            layers: Vec::new(),
            overlay: Some(OverlayView {
                items: vec![TextItemView {
                    text: "x".to_string(),
                    rect: RectView { x: 0.0, y: 0.0, width: 1.0, height: 0.1 },
                }],
                danmaku: Vec::new(),
                color: [255, 255, 255, 255],
                outline: true,
                dropped_lines: 0,
                dropped_danmaku: 0,
            }),
            error: None,
        };
        let value = serde_json::to_value(&with_text).unwrap();
        assert!(value.get("overlay").is_some(), "有文字时必须有 overlay 键");
    }

    /// **只有弹幕**的一帧：`items` 空但 `danmaku` 不空，形状照样成立。
    ///
    /// 这条钉的是「字幕与弹幕两个键各自独立」—— 把两者合成一个数组的话，
    /// 这条就写不出来（而症状是"弹幕工程看起来像没有文字"）。
    #[test]
    fn 只有弹幕时_danmaku_可以非空而_items_为空() {
        let result = FrameResult {
            frame: 1,
            layers: Vec::new(),
            overlay: Some(OverlayView {
                items: Vec::new(),
                danmaku: vec![DanmakuItemView {
                    text: "只此一条".to_string(),
                    rect: RectView { x: 0.5, y: 0.048, width: 0.2, height: 0.048 },
                    lane: 1,
                    enter: 0,
                    exit: 30,
                }],
                color: [255, 255, 255, 255],
                outline: true,
                dropped_lines: 0,
                dropped_danmaku: 0,
            }),
            error: None,
        };
        let value = serde_json::to_value(&result).unwrap();
        assert_eq!(value["overlay"]["items"].as_array().map(Vec::len), Some(0));
        assert_eq!(value["overlay"]["danmaku"].as_array().map(Vec::len), Some(1));
    }

    /// 矩形与文本行的字段是**逐字段对应**的：换名或漏字段都在这条上红。
    #[test]
    fn 矩形与文本行逐字段对应() {
        use crate::text_layout::{NormalizedRect, TextLine};
        let rect = NormalizedRect { x: 0.125, y: 0.75, width: 0.5, height: 0.0625 };
        let view = RectView::from(rect);
        assert_eq!(view, RectView { x: 0.125, y: 0.75, width: 0.5, height: 0.0625 });
        let item = TextItemView::from(TextLine { text: "两行\n两行".to_string(), rect });
        let value = serde_json::to_value(&item).unwrap();
        assert_eq!(value["text"], serde_json::json!("两行\n两行"));
        assert_eq!(value["rect"]["x"], serde_json::json!(0.125));
        assert_eq!(value["rect"]["height"], serde_json::json!(0.0625));
    }

    #[test]
    fn 样本表的键仍是单字母() {
        // 改键名是破坏性改动，所以 o/s/d/u/k 原样保留；
        // 但字段名（offset/size/dts/duration/is_sync）让含义第一次有了出处。
        let sample = SampleView { offset: 100, size: 4, dts: 0, duration: 1000, is_sync: true };
        let value = serde_json::to_value(sample).unwrap();
        assert_eq!(keys(&value), sorted(&["o", "s", "d", "u", "k"]));
    }

    #[test]
    fn 缓存账的键是钉死的() {
        let stats = CacheStatsView {
            vram_bytes: 1, ram_bytes: 2, vram_len: 3, ram_len: 4, vram_over: false, ram_over: true,
        };
        let value = serde_json::to_value(&stats).unwrap();
        assert_eq!(
            keys(&value),
            sorted(&["vram_bytes", "ram_bytes", "vram_len", "ram_len", "vram_over", "ram_over"])
        );
    }

    #[test]
    fn 能力声明里的混合模式是从谓词推出来的() {
        // **不是手写的清单** —— 手写一份一定会与渲染器漂开。
        let caps = Capabilities::new(Vec::new(), 16, false, false);
        assert_eq!(caps.blend_modes, vec![BlendMode::Normal, BlendMode::Add, BlendMode::Multiply, BlendMode::Screen]);
        assert_eq!(caps.timeline_versions, vec![LAYER_SCHEMA_VERSION]);
    }

    #[test]
    fn 对端版本不认就只报一条() {
        let caps = Capabilities::new(Vec::new(), 16, false, false);
        let mut timeline = TimelineV2 {
            schema: 99,
            timebase: crate::schema::TimebaseDto { num: 60, den: 1 },
            markers: Vec::new(),
            tracks: Vec::new(),
        };
        timeline.tracks.push(crate::layer::TrackV2 {
            id: "v".to_string(),
            kind: crate::schema::TrackKind::Video,
            layers: Vec::new(),
            subtitle: None,
            danmaku: None,
        });
        let issues = precheck(&timeline, &caps);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].code, "schema_unsupported_by_peer");
    }

    #[test]
    fn 预检能指出是对端的哪一项不支持() {
        let mut layer = crate::layer::Layer {
            id: "l1".to_string(),
            start: 0,
            end: 10,
            transform: crate::layer::TransformV2::default(),
            opacity: 1.0,
            blend: BlendMode::Overlay, // 对端不支持
            enabled: true,
            recorded: crate::layer::Recorded::default(),
            source: None,
            effects: vec![
                crate::schema::Effect {
                    kind: "gaussian_blur".to_string(),
                    params: std::collections::BTreeMap::from([("radius".to_string(), 64.0_f32)]),
                },
                crate::schema::Effect {
                    kind: "vignette".to_string(), // 对端没有这个特效
                    params: std::collections::BTreeMap::new(),
                },
            ],
            transition_in: None,
            keyframes: Vec::new(),
        };
        layer.blend = BlendMode::Overlay;
        let timeline = TimelineV2 {
            schema: LAYER_SCHEMA_VERSION,
            timebase: crate::schema::TimebaseDto { num: 60, den: 1 },
            markers: Vec::new(),
            tracks: vec![crate::layer::TrackV2 {
                id: "v".to_string(),
                kind: crate::schema::TrackKind::Video,
                layers: vec![layer],
                subtitle: None,
                danmaku: None,
            }],
        };
        // 对端：支持模糊但半径上限只有 16；没有 vignette。
        let caps = Capabilities::new(vec!["gaussian_blur".to_string()], 16, true, true);
        let issues = precheck(&timeline, &caps);
        let mut codes: Vec<&str> = issues.iter().map(|i| i.code.as_str()).collect();
        codes.sort();
        assert_eq!(
            codes,
            vec!["blend_unsupported_by_peer", "effect_exceeds_capability", "effect_unsupported_by_peer"]
        );
        // path 要指到具体字段，否则用户不知道该改哪一项。
        assert!(issues.iter().any(|i| i.path.ends_with(".blend")));
        assert!(issues.iter().any(|i| i.path.ends_with(".params.radius")));
        // 半径那条要带上对端上限，否则用户不知道怎么改。
        assert!(issues.iter().any(|i| i.message.contains("16")));
    }

    #[test]
    fn 对端全支持时预检是空的() {
        let timeline = TimelineV2 {
            schema: LAYER_SCHEMA_VERSION,
            timebase: crate::schema::TimebaseDto { num: 60, den: 1 },
            markers: Vec::new(),
            tracks: Vec::new(),
        };
        let caps = Capabilities::new(vec!["gaussian_blur".to_string()], 16, true, true);
        assert!(precheck(&timeline, &caps).is_empty());
    }
}

// ============================================================================
// 素材取用契约
// ============================================================================
//
// 远端模式下前端**不能下载整片**（几 GB），只能"只取我需要的这一段"。
// 而"最小可解单位"在 H.264 里就是 **GOP** —— 只有关键帧能起解，
// 想解第 N 帧必须从它前面最近的关键帧开始喂。
//
// 所以分片按 GOP 切，不是按字节切。按字节切会让前端拿到"起点不是关键帧"的片段，
// **解不出第一帧**。
//
// 而 S3.2 定的 proxy 规格本身就是关键帧对齐的（-g 60 -keyint_min 60 -sc_threshold 0），
// 所以切片天然对齐 —— 不需要额外算索引。

/// 后端**现在能提供**的素材信息。
///
/// 与工程文件里资产表的关系：资产表是「工程里记了什么」，
/// 这一份是「后端现在能提供什么」。两者**信息重叠但不相同** ——
/// 例如工程里登记了原片，而原片离线、只剩 proxy。
///
/// **不一致时以后端为准**，且前端要能把差异报给用户，而不是默默用另一份。
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AssetInfoView {
    pub id: String,
    pub kind: String,
    /// 总帧数。**没有它就无法校验素材内越界**（见 project::validate_project_doc）。
    pub frame_count: Frame,
    pub timebase: TimebaseDto,
    pub width: u32,
    pub height: u32,
    /// 关键帧间隔（帧）。切片按它对齐；0 表示未知，此时**不能**按 GOP 切。
    pub gop_length: u32,
    /// 后端有没有可编辑用的低码率代理。
    pub proxy_available: bool,
}

/// 一个 GOP 切片。
///
/// **服务器零计算**：它只是「字节范围 + 样本表下标」——
/// 不需要解码、不需要转码、不需要重新封装。
/// 前端拿原始字节范围 + 自己的 MP4 分离器就能喂给解码器。
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct GopSliceView {
    /// 第几个 GOP（从 0 起）。
    pub index: u64,
    /// 这一段在样本表里的起点下标。
    pub first_sample: u32,
    pub sample_count: u32,
    /// 这一段在文件里的字节范围。
    pub byte_offset: u64,
    pub byte_length: u64,
}

/// 帧号 -> 它所在 GOP 的下标。
///
/// gop_length 为 0 表示未知 —— 返回 None 而不是除零，
/// 也**不是**退回"按帧切"（那会破坏"从关键帧起解"的前提）。
pub fn gop_index_for_frame(frame: Frame, gop_length: u32) -> Option<u64> {
    if gop_length == 0 || frame < 0 {
        return None;
    }
    Some(frame as u64 / u64::from(gop_length))
}

/// 第 index 个 GOP 覆盖的帧区间，**左闭右开**。
///
/// 最后一段会被 frame_count 截断（素材长度通常不是 GOP 长度的整数倍）。
/// 返回 None 表示这个 GOP 完全在素材之外。
pub fn gop_frame_range(index: u64, gop_length: u32, frame_count: Frame) -> Option<(Frame, Frame)> {
    if gop_length == 0 || frame_count <= 0 {
        return None;
    }
    let length = i64::from(gop_length);
    let start = (index as i64).checked_mul(length)?;
    if start >= frame_count {
        return None;
    }
    let end = start.saturating_add(length).min(frame_count);
    Some((start, end))
}

#[cfg(test)]
mod asset_tests {
    use super::*;

    #[test]
    fn 第k个gop恰好覆盖k乘gop到k加1乘gop() {
        // 这是「帧号精确」在分片这一层的最低要求：
        // 取第 k 段，解出来的帧号必须**不多不少**。
        for k in 0..8u64 {
            let (start, end) = gop_frame_range(k, 60, 480).expect("在范围内");
            assert_eq!(start, (k as i64) * 60);
            assert_eq!(end, (k as i64 + 1) * 60);
        }
    }

    #[test]
    fn 最后一段被素材长度截断() {
        let (start, end) = gop_frame_range(2, 60, 155).unwrap();
        assert_eq!((start, end), (120, 155));
        assert!(gop_frame_range(3, 60, 155).is_none());
        assert!(gop_frame_range(0, 60, 0).is_none());
    }

    #[test]
    fn 帧号到_gop_下标的换算与区间互为逆() {
        for frame in 0..300i64 {
            let index = gop_index_for_frame(frame, 60).unwrap();
            let (start, end) = gop_frame_range(index, 60, 480).unwrap();
            assert!(frame >= start && frame < end, "第 {frame} 帧不在它自己的 GOP 区间里");
        }
    }

    #[test]
    fn gop_长度未知时返回_none_而不是除零或退回按帧切() {
        // 0 表示"未知"。退回按帧切会破坏「只有关键帧能起解」这个前提，
        // 于是前端拿到解不了的片段 —— 那比明确失败更糟。
        assert!(gop_index_for_frame(10, 0).is_none());
        assert!(gop_frame_range(0, 0, 480).is_none());
    }

    #[test]
    fn 负帧号不参与换算() {
        assert!(gop_index_for_frame(-1, 60).is_none());
    }

    #[test]
    fn 相邻两段首尾相接不重叠() {
        // 漏一帧会让拖动时"跳帧"，重叠一帧会让同一帧被取两次。
        let mut previous_end = 0;
        for k in 0..8u64 {
            let (start, end) = gop_frame_range(k, 60, 480).unwrap();
            if k > 0 {
                assert_eq!(start, previous_end, "第 {k} 段与上一段不相接");
            }
            previous_end = end;
        }
    }
}

/// 按**同步样本**把样本表切成 GOP 段。
///
/// # 为什么按 is_sync 切，而不是"每 N 个样本切一刀"
///
/// 真正的 GOP 边界就是关键帧，而 gop_length 只是「通常如此」的期望值。
/// 拿它当切分依据，一旦素材的实际关键帧间隔与期望不符（转码参数变了、或 VFR），
/// 切出来的段就会**起点不是关键帧** —— 前端解不出第一帧。
/// 用 is_sync 则是**读事实**，不是**信期望**。
///
/// # 段与样本表的关系
///
/// 每一段从它的同步样本开始，到下一个同步样本**之前**为止。
/// 样本表开头若有不属于任何关键帧的样本，它们**不属于任何段** ——
/// 那些样本没有可起解的关键帧，本来就解不出来。
/// 所以第 0 段的 first_sample 可能不是 0，这是事实，不是缺口。
///
/// 一个同步样本都没有时返回**空**：那意味着这份素材根本没法按帧定位，
/// 此时切一刀出一个"看起来能用"的段，只会让前端在解码时才失败。
pub fn gop_slices(samples: &[SampleView]) -> Vec<GopSliceView> {
    let sync_indices: Vec<usize> = samples
        .iter()
        .enumerate()
        .filter(|(_, sample)| sample.is_sync)
        .map(|(index, _)| index)
        .collect();
    if sync_indices.is_empty() {
        return Vec::new();
    }

    let mut slices = Vec::with_capacity(sync_indices.len());
    for (position, &first) in sync_indices.iter().enumerate() {
        let end = sync_indices.get(position + 1).copied().unwrap_or(samples.len());
        let head = &samples[first];
        // 字节范围取这一段所有样本的并集 —— 用 max 而不是只取最后一个，
        // 免得样本在文件里的顺序与表里的顺序不一致时算短了。
        let mut last_end = head.offset.saturating_add(head.size);
        for sample in &samples[first..end] {
            last_end = last_end.max(sample.offset.saturating_add(sample.size));
        }
        slices.push(GopSliceView {
            index: position as u64,
            first_sample: first as u32,
            sample_count: (end - first) as u32,
            byte_offset: head.offset as u64,
            byte_length: last_end.saturating_sub(head.offset) as u64,
        });
    }
    slices
}

#[cfg(test)]
mod gop_slice_tests {
    use super::*;

    /// 造一张样本表：每 gop 个样本一个关键帧，偏移从 0 开始每条 100 字节。
    fn samples(len: usize, gop: usize) -> Vec<SampleView> {
        (0..len)
            .map(|index| SampleView {
                offset: index * 100,
                size: 100,
                dts: index as u64,
                duration: 1,
                is_sync: index % gop == 0,
            })
            .collect()
    }

    #[test]
    fn 每段从关键帧开始且到下一个关键帧之前为止() {
        let slices = gop_slices(&samples(10, 3));
        assert_eq!(slices.len(), 4, "10 个样本、每 3 个一个关键帧 -> 4 段");
        assert_eq!(slices[0].first_sample, 0);
        assert_eq!(slices[0].sample_count, 3);
        assert_eq!(slices[1].first_sample, 3);
        assert_eq!(slices[2].first_sample, 6);
        // 最后一段到表尾为止（9 是最后一个关键帧，只剩它自己）。
        assert_eq!(slices[3].first_sample, 9);
        assert_eq!(slices[3].sample_count, 1);
    }

    #[test]
    fn 相邻两段首尾相接() {
        // 有空洞会让前端"少一帧"，重叠会让同一帧被取两次。
        let slices = gop_slices(&samples(20, 4));
        for pair in slices.windows(2) {
            assert_eq!(
                pair[0].first_sample + pair[0].sample_count,
                pair[1].first_sample,
                "第 {} 段与下一段不相接",
                pair[0].index
            );
        }
    }

    #[test]
    fn 每段的字节范围覆盖它自己那些样本() {
        let slices = gop_slices(&samples(10, 3));
        for slice in &slices {
            let first = slice.first_sample as usize;
            let last = first + slice.sample_count as usize - 1;
            assert_eq!(slice.byte_offset, (first * 100) as u64);
            // 最后一条样本的末尾（偏移 + 长度）。
            assert_eq!(slice.byte_length, ((last + 1) * 100 - first * 100) as u64);
        }
    }

    #[test]
    fn 开头没有关键帧时那些样本不属于任何段() {
        // 这是事实而不是缺口：它们前面没有可起解的关键帧，本来就解不出来。
        let mut table = samples(10, 3);
        table[0].is_sync = false;
        let slices = gop_slices(&table);
        assert_eq!(slices[0].first_sample, 3, "第 0 段应当从第一个**关键帧**开始");
    }

    #[test]
    fn 一个关键帧都没有时返回空而不是切一刀() {
        // 那意味着这份素材没法按帧定位。切出一个"看起来能用"的段，
        // 只会让前端在解码时才失败 —— 明确失败好得多。
        let mut table = samples(10, 3);
        for sample in &mut table {
            sample.is_sync = false;
        }
        assert!(gop_slices(&table).is_empty());
        assert!(gop_slices(&[]).is_empty());
    }

    #[test]
    fn 实际关键帧间隔与期望不符时仍然正确() {
        // 这才是"按 is_sync 切"的意义：gop_length 只是期望值。
        // 混合间隔（3、3、7）也要切对，因为它读的是事实。
        let mut table = samples(13, 3);
        for (index, sample) in table.iter_mut().enumerate() {
            sample.is_sync = index == 0 || index == 3 || index == 6;
        }
        let slices = gop_slices(&table);
        assert_eq!(slices.len(), 3);
        assert_eq!(slices[0].sample_count, 3);
        assert_eq!(slices[1].sample_count, 3);
        assert_eq!(slices[2].sample_count, 7, "最后一段一直延伸到表尾");
    }

    #[test]
    fn 每个样本都是关键帧时每段一个样本() {
        let slices = gop_slices(&samples(5, 1));
        assert_eq!(slices.len(), 5);
        for slice in &slices {
            assert_eq!(slice.sample_count, 1);
        }
    }
}

/// 检查后端**声明的**关键帧间隔与样本表里的**实际**间隔是否一致。
///
/// # 为什么需要它
///
/// gop_length 是**声明**，样本表是**事实**。前端会拿 gop_length 去算
/// gop_frame_range，而 gop_slices 读的是事实 —— 两者一旦不一致，
/// 「第 k 段覆盖哪些帧」就有了**两个答案**，而那种分歧不会崩，
/// 只会让画面少一帧或多一帧，非常难查。
///
/// 返回 None 表示没问题（含"间隔无从谈起"的几种正常情形）。
pub fn gop_length_mismatch(samples: &[SampleView], declared: u32) -> Option<Issue> {
    if declared == 0 {
        return Some(Issue::new(
            "gop_length_unknown",
            "gop_length",
            "后端没有声明关键帧间隔 —— 前端无法把帧号换算成段号".to_string(),
        ));
    }

    let sync_indices: Vec<usize> = samples
        .iter()
        .enumerate()
        .filter(|(_, sample)| sample.is_sync)
        .map(|(index, _)| index)
        .collect();

    // 关键帧少于两个时"间隔"无从谈起 —— 那不是不一致。
    // 完全没有关键帧的情形由 gop_slices 返回空来表达，不在这里重复报。
    if sync_indices.len() < 2 {
        return None;
    }

    let declared = declared as usize;
    let spacings: Vec<usize> = sync_indices.windows(2).map(|pair| pair[1] - pair[0]).collect();
    let first_bad = spacings.iter().position(|spacing| *spacing != declared)?;

    Some(Issue::new(
        "gop_length_mismatch",
        &format!("samples[{}]", sync_indices[first_bad]),
        format!(
            "后端声明关键帧间隔 {declared}，而样本表里第 {first_bad} 个关键帧到下一个隔了 {}",
            spacings[first_bad]
        ),
    ))
}

#[cfg(test)]
mod gop_length_tests {
    use super::*;

    fn table(spacing: usize, count: usize) -> Vec<SampleView> {
        (0..count)
            .map(|index| SampleView {
                offset: index * 10,
                size: 10,
                dts: index as u64,
                duration: 1,
                is_sync: index % spacing == 0,
            })
            .collect()
    }

    #[test]
    fn 声明与实际一致时没问题() {
        assert!(gop_length_mismatch(&table(60, 240), 60).is_none());
    }

    #[test]
    fn 不一致时指出第一处并带上两个数字() {
        // 关键帧落在 0 / 60 / 90 / 150：间隔依次是 60 / 30 / 60，
        // 第一处不一致就在 60 与 90 之间。
        let mut samples = table(60, 240);
        for (index, sample) in samples.iter_mut().enumerate() {
            sample.is_sync = [0usize, 60, 90, 150].contains(&index);
        }
        let issue = gop_length_mismatch(&samples, 60).expect("应当报不一致");
        assert_eq!(issue.code, "gop_length_mismatch");
        assert!(issue.path.starts_with("samples["), "path 要指到具体位置：{}", issue.path);
        // 提示要同时带上「声明的」与「实际的」，否则用户不知道该改哪一边。
        assert!(issue.message.contains("60"), "{}", issue.message);
        assert!(issue.message.contains("30"), "{}", issue.message);
    }

    #[test]
    fn 没声明间隔要明确报出来() {
        // 0 表示未知。此时前端**不能**按 GOP 换算 —— 那比报错更糟。
        let issue = gop_length_mismatch(&table(60, 120), 0).expect("应当报未声明");
        assert_eq!(issue.code, "gop_length_unknown");
    }

    #[test]
    fn 关键帧少于两个时不算不一致() {
        // 间隔无从谈起。完全没有关键帧的情形由 gop_slices 返回空来表达。
        let mut none_sync = table(60, 10);
        for sample in &mut none_sync {
            sample.is_sync = false;
        }
        assert!(gop_length_mismatch(&none_sync, 60).is_none());
        assert!(gop_length_mismatch(&[], 60).is_none());

        let mut one_sync = table(60, 10);
        for (index, sample) in one_sync.iter_mut().enumerate() {
            sample.is_sync = index == 0;
        }
        assert!(gop_length_mismatch(&one_sync, 60).is_none());
    }
}

// ============================================================================
// 出片任务契约
// ============================================================================
//
// 出片是**分钟级**的（实测：90 帧 640x360 的逐帧渲染加编码就要几分钟），
// 所以它**不能**是同步 HTTP —— 必须「提交 -> 任务号 -> 轮询/推送」。
//
// 这一组形状就是那条链路的契约。放在这里而不是散在服务端代码里，
// 是因为**分离模式与本机模式必须说同一种话**。

/// 出片任务的状态。
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportState {
    Queued,
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

impl ExportState {
    /// 是不是**终态** —— 到了这里任务就不该再动。
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled)
    }

    /// 允不允许转到 next。
    ///
    /// # 为什么要把这张表写死
    ///
    /// 不写死的话，「任务从完成又变回运行中」这类错误**只在并发下复现**，
    /// 而且一旦发生，前端已经拿到 download_url 又被告知"还在跑" ——
    /// 表现是随机闪烁，几乎不可能靠复现去查。
    ///
    /// 允许 Queued 直接到 Succeeded：任务可能在第一次轮询之前就跑完了。
    /// 那是正常情形，不该被状态机判成非法。
    pub const fn can_transition_to(self, next: Self) -> bool {
        if self.is_terminal() {
            return false;
        }
        matches!(
            (self, next),
            (Self::Queued, Self::Running)
                | (Self::Queued, Self::Succeeded)
                | (Self::Queued, Self::Failed)
                | (Self::Queued, Self::Cancelled)
                | (Self::Running, Self::Succeeded)
                | (Self::Running, Self::Failed)
                | (Self::Running, Self::Cancelled)
        )
    }
}

/// 提交出片后的回执。**只有任务号** —— 别的一律靠查状态拿。
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExportAcceptedView {
    pub job_id: String,
}

/// 查一个出片任务。
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExportStatusView {
    pub job_id: String,
    pub state: ExportState,
    /// 0.0..=1.0。**未知时为 None，不要用 0.0 冒充** ——
    /// 「进度未知」与「进度是零」是两件事，前者界面上该显示不确定态。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub download_url: Option<String>,
    /// 失败原因。**复用同一套 Issue**，不新造一种错误格式 ——
    /// 前端已经在渲染那一套了，多一种就要多认一次。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<Issue>,
}

#[cfg(test)]
mod export_tests {
    use super::*;

    #[test]
    fn 终态不许再转() {
        for state in [ExportState::Succeeded, ExportState::Failed, ExportState::Cancelled] {
            assert!(state.is_terminal());
            for next in [
                ExportState::Queued,
                ExportState::Running,
                ExportState::Succeeded,
                ExportState::Failed,
                ExportState::Cancelled,
            ] {
                assert!(
                    !state.can_transition_to(next),
                    "{state:?} 是终态，不该能转到 {next:?}"
                );
            }
        }
    }

    #[test]
    fn 未终态只能沿着可行的边走() {
        assert!(ExportState::Queued.can_transition_to(ExportState::Running));
        assert!(ExportState::Queued.can_transition_to(ExportState::Succeeded), "任务可能在第一次轮询前就跑完");
        assert!(ExportState::Queued.can_transition_to(ExportState::Cancelled));
        assert!(ExportState::Running.can_transition_to(ExportState::Succeeded));
        assert!(ExportState::Running.can_transition_to(ExportState::Failed));
        assert!(ExportState::Running.can_transition_to(ExportState::Cancelled));
        // 回不去：这就是「完成又变回运行中」那条。
        assert!(!ExportState::Running.can_transition_to(ExportState::Queued));
        assert!(!ExportState::Running.can_transition_to(ExportState::Running));
    }

    #[test]
    fn 状态序列化用蛇形且是稳定的字符串() {
        // 状态串是**跨进程**的：本机后端写、前端读。所以它不能随枚举改名字而变。
        assert_eq!(serde_json::to_string(&ExportState::Running).unwrap(), "\"running\"");
        assert_eq!(serde_json::to_string(&ExportState::Succeeded).unwrap(), "\"succeeded\"");
        assert_eq!(serde_json::to_string(&ExportState::Cancelled).unwrap(), "\"cancelled\"");
    }

    #[test]
    fn 可选字段为空时不出现而不是塞_null() {
        let status = ExportStatusView {
            job_id: "j1".to_string(),
            state: ExportState::Running,
            progress: None,
            download_url: None,
            error: None,
        };
        let value = serde_json::to_value(&status).unwrap();
        let mut keys: Vec<&str> = value.as_object().unwrap().keys().map(|k| k.as_str()).collect();
        keys.sort_unstable();
        assert_eq!(keys, vec!["job_id", "state"], "为空的可选项不该出现");
    }

    #[test]
    fn 完成时带上下载地址且不带错误() {
        let status = ExportStatusView {
            job_id: "j1".to_string(),
            state: ExportState::Succeeded,
            progress: Some(1.0),
            download_url: Some("/export/j1/download".to_string()),
            error: None,
        };
        let value = serde_json::to_value(&status).unwrap();
        assert!(value.get("download_url").is_some());
        assert!(value.get("error").is_none(), "成功时不该有 error 键");
    }

    #[test]
    fn 失败时带的是同一套_issue_而不是自定义错误串() {
        let status = ExportStatusView {
            job_id: "j1".to_string(),
            state: ExportState::Failed,
            progress: None,
            download_url: None,
            error: Some(Issue::new(
                "encoder_missing",
                "ffmpeg",
                "找不到 ffmpeg".to_string(),
            )),
        };
        let value = serde_json::to_value(&status).unwrap();
        let error = &value["error"];
        let mut keys: Vec<&str> = error.as_object().unwrap().keys().map(|k| k.as_str()).collect();
        keys.sort_unstable();
        assert_eq!(keys, vec!["code", "message", "path"], "要复用 Issue 的形状");
    }
}
