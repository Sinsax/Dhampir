//! 工程文件（**壳**）：渲染契约的超集。
//!
//! # 为什么要有壳
//!
//! 契约已经冻结，并对外承诺「破坏性改动 +1、服务端拒绝未知版本」。
//! 如果用户存的文件就是契约本身，那么任何 UI 需求（播放头、选中、缩放）都会逼着契约改版本 ——
//! **契约会被非渲染因素推着动**，这是最糟的耦合。所以拆两层：
//!
//! - **契约**：两端渲染一帧必需且充分的信息（原样内嵌在 `timeline` 字段里，一个字段都不改）；
//! - **壳**：契约 + 资产登记表 + 元信息 + 应用状态 + 扩展位。
//!
//! # 判据：什么进哪一层
//!
//! **两端渲染这一帧的结果依赖它吗？** 依赖 → 契约；不依赖 → 壳。
//!
//! 所以 **asset 的 uri 在壳里** —— 两端可以不同（浏览器是 URL、服务端是本地路径），
//! 写进契约等于把部署拓扑焊死进渲染定义。契约里只留一个不透明的 `asset_id`。
//! 这既是前后端分离能成立的根据，也让**容器层第一次能校验素材内越界**（v1 做不到，
//! 因为它根本不知道素材有多长）。

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::layer::{
    AssetTimebases, LAYER_SCHEMA_VERSION, LAYER_SCHEMA_VERSION_V2, TimelineV2, migrate_v1_to_v2,
    migrate_v2_to_v3, source_frame_at, validate_timeline_v2,
};
use crate::schema::{EffectSpec, Frame, Issue, Project, TimebaseDto};

/// 壳的版本。与契约版本**互相独立**：壳可以到 v3 而契约还在 v2。
pub const PROJECT_SCHEMA_VERSION: u32 = 1;

/// 谁生成了这份工程。出问题时第一个要问的东西。
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Generator {
#[serde(default)]
    pub app: String,
#[serde(default)]
    pub version: String,
}

/// 元信息。纯给人看的。
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Meta {
#[serde(default)]
    pub title: String,
    /// ISO-8601 字符串。**时间不进渲染，就不该用强类型把宿主绑死。**
#[serde(default)]
    pub created_at: Option<String>,
#[serde(default)]
    pub modified_at: Option<String>,
}

#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetKind {
    Video,
    Audio,
    Image,
}

/// 一条资产的登记项。
///
/// 它是「引用与实体分离」的实体那一半：契约里只有 id，位置与元信息在这里。
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Asset {
    pub id: String,
    pub kind: AssetKind,
    /// 显示名。UI 用它，不再拿 URI 当标题。
#[serde(default)]
    pub name: String,
    /// 位置。**语义由宿主解释**（浏览器是 URL，服务端是路径或对象存储 key）。
#[serde(default)]
    pub uri: String,
    /// 素材总帧数。**有它才谈得上校验素材内越界。**
#[serde(default)]
    pub frame_count: Option<Frame>,
    /// 素材自身帧率。与工程时基不同就要换算。
#[serde(default)]
    pub timebase: Option<TimebaseDto>,
#[serde(default)]
    pub width: Option<u32>,
#[serde(default)]
    pub height: Option<u32>,
    /// 内容指纹。**只记录，不校验** —— 怎么算是宿主的事。
#[serde(default)]
    pub content_hash: Option<String>,
#[serde(default)]
    pub tags: BTreeMap<String, String>,
#[serde(default)]
    pub note: String,
}

/// 应用状态。**渲染时忽略** —— 它不该影响出片结果。
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct View {
#[serde(default)]
    pub playhead: Frame,
#[serde(default)]
    pub selection: Option<String>,
#[serde(default = "default_zoom")]
    pub zoom: f32,
}

fn default_zoom() -> f32 { 1.0 }

impl Default for View {
    fn default() -> Self {
        Self { playhead: 0, selection: None, zoom: 1.0 }
    }
}

/// 输出的**建议值**，宿主可覆盖（预览尺寸归宿主这条判断不变）。
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RenderHints {
#[serde(default = "default_width")]
    pub width: u32,
#[serde(default = "default_height")]
    pub height: u32,
#[serde(default = "default_format")]
    pub format: String,
}

fn default_width() -> u32 { 1920 }
fn default_height() -> u32 { 1080 }
fn default_format() -> String { "mp4".to_string() }

impl Default for RenderHints {
    fn default() -> Self {
        Self { width: 1920, height: 1080, format: "mp4".to_string() }
    }
}

/// 工程文件。
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectDoc {
    pub project_schema: u32,
#[serde(default)]
    pub generator: Generator,
#[serde(default)]
    pub meta: Meta,
#[serde(default)]
    pub assets: Vec<Asset>,
    /// 渲染契约，**原样内嵌** —— 不拍平，这样 `doc.timeline` 直接可以喂给渲染器。
    pub timeline: TimelineV2,
#[serde(default)]
    pub view: View,
#[serde(default)]
    pub render_hints: RenderHints,
    /// 下游放自己的东西，不污染契约。
#[serde(default)]
    pub extensions: BTreeMap<String, serde_json::Value>,

    /// **载入时从哪个契约版本升上来的**（没有升级过就是 None）。
    ///
    /// 刻意 skip 掉：它描述的是"这份文件是怎么被读进来的"，不是文件的内容 ——
    /// 写回磁盘时出现它就等于把运行时状态污染进契约。
    /// 它存在的唯一目的是让校验能说一句"你是从 v2 升上来的，而 v2 的时间语义不同"。
#[serde(skip)]
    pub migrated_from: Option<u32>,
}
/// 载入结果里的问题清单。
///
/// **刻意分成 errors 与 warnings，而不是给 Issue 加一个 severity 字段** ——
/// Issue 是已冻结契约的一部分，为了一处提示去动它的形状不划算。
// Serialize 是**必须的**，不是顺手加的：这份清单要跨进程交给宿主
// （CLI 的 stdout、HTTP 的响应体），而宿主不该为它再写一遍翻译。
// 与 Issue 共用同一套形状（code/path/message），前端只认一种错误。
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DocIssues {
    pub errors: Vec<Issue>,
    /// 警告**不阻断**载入（例如「登记了但没被引用」）。
    pub warnings: Vec<Issue>,
}

impl DocIssues {
    pub fn is_ok(&self) -> bool {
        self.errors.is_empty()
    }
}

/// 把一份裸契约包成壳。
///
/// **从 source 字符串合成资产表**：v1/v2 的裸契约里 `asset_id` 只是个字符串，
/// 没有任何位置与长度信息。合成出来的资产表是**最小可用**的 ——
/// name 与 uri 都用那个字符串（位置由宿主解释），frame_count 等留空
/// （所以裸契约载入后 **source_range_exceeded 查不出来** —— 那是信息缺失，不是疏漏）。
pub fn shell_from_timeline(timeline: TimelineV2) -> ProjectDoc {
    let mut assets = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for track in &timeline.tracks {
        for layer in &track.layers {
            let Some(source) = &layer.source else { continue };
            if seen.insert(source.asset_id.clone()) {
                assets.push(Asset {
                    id: source.asset_id.clone(),
                    kind: AssetKind::Video,
                    name: source.asset_id.clone(),
                    uri: source.asset_id.clone(),
                    frame_count: None,
                    timebase: None,
                    width: None,
                    height: None,
                    content_hash: None,
                    tags: BTreeMap::new(),
                    note: String::new(),
                });
            }
        }
    }
    ProjectDoc {
        project_schema: PROJECT_SCHEMA_VERSION,
        generator: Generator::default(),
        meta: Meta::default(),
        assets,
        timeline,
        view: View::default(),
        render_hints: RenderHints::default(),
        extensions: BTreeMap::new(),
        migrated_from: None,
    }
}

impl ProjectDoc {
    /// 资产 id → 时间基。求值层用它把时间线帧号换算成素材帧号。
    ///
    /// **没登记时间基的资产不进表**：不进表 = 走恒等换算（素材帧率按时间线算），
    /// 这正是升级前的行为。**不要给它猜一个帧率** —— 猜错的表现是画面变速，
    /// 而那是"看起来完全正常"的那一类错。
    pub fn asset_timebases(&self) -> AssetTimebases {
        let mut table = AssetTimebases::new();
        for asset in &self.assets {
            if asset.id.is_empty() {
                continue;
            }
            if let Some(timebase) = asset.timebase.clone() {
                table.insert(asset.id.clone(), timebase);
            }
        }
        table
    }
}

/// 载入一份工程。**接受三种形态**：
///
/// 1. 工程文件（顶层有 `project_schema`）—— 直接用；
/// 2. **裸契约 v1**（顶层是 `schema: 1`）—— 迁移到 v2 再包壳；
/// 3. **裸契约 v2** —— 直接包壳。
///
/// 裸契约是**兼容形态，不再扩展**；写入一律写工程文件。
/// 判定规则刻意做得简单（看顶层有没有 project_schema），因为复杂的判定规则本身就是 bug 来源。
pub fn load_doc(text: &str) -> Result<ProjectDoc, String> {
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|e| format!("不是合法 JSON：{e}"))?;

    // ---- 工程文件。内嵌的时间线可能还是旧版本，一级一级升上来。 ----
    if value.get("project_schema").is_some() {
        let mut doc: ProjectDoc =
            serde_json::from_value(value).map_err(|e| format!("工程文件字段不符：{e}"))?;
        let from = doc.timeline.schema;
        if from == LAYER_SCHEMA_VERSION_V2 {
            doc.timeline = migrate_v2_to_v3(&doc.timeline);
            doc.migrated_from = Some(from);
        } else if from != LAYER_SCHEMA_VERSION {
            return Err(format!(
                "工程文件里的时间线是 v{from}，本实现只认 v{LAYER_SCHEMA_VERSION_V2} 与 v{LAYER_SCHEMA_VERSION}"
            ));
        }
        return Ok(doc);
    }

    // ---- 裸契约 v1：v1 → v2 → v3，两级都走完 ----
    let version = value.get("schema").and_then(serde_json::Value::as_u64);
    if version == Some(1) {
        let project: Project =
            serde_json::from_value(value).map_err(|e| format!("v1 契约字段不符：{e}"))?;
        let v2 = migrate_v1_to_v2(&project).map_err(|e| e.to_string())?;
        let mut doc = shell_from_timeline(migrate_v2_to_v3(&v2));
        doc.migrated_from = Some(1);
        return Ok(doc);
    }

    // ---- 裸契约 v2 或 v3 ----
    let timeline: TimelineV2 =
        serde_json::from_value(value).map_err(|e| format!("v2/v3 契约字段不符：{e}"))?;
    let from = timeline.schema;
    let mut doc = if from == LAYER_SCHEMA_VERSION_V2 {
        shell_from_timeline(migrate_v2_to_v3(&timeline))
    } else {
        shell_from_timeline(timeline)
    };
    if from == LAYER_SCHEMA_VERSION_V2 {
        doc.migrated_from = Some(from);
    }
    Ok(doc)
}

/// 校验一份工程：壳 + 引用完整性 + 契约。
///
/// **错误格式完全复用契约层那一套**（Issue 的 code/path/message），不新造 ——
/// 已经有一条测试钉住它能直接进 JSON 给 UI 用，再造一套等于让 UI 认两种。
pub fn validate_project_doc(doc: &ProjectDoc, effects: &[EffectSpec]) -> DocIssues {
    let mut errors = Vec::new();
    let mut warnings = Vec::new();

    if doc.project_schema != PROJECT_SCHEMA_VERSION {
        errors.push(Issue::new(
            "unsupported_project_schema",
            "project_schema",
            format!(
                "工程文件是 v{}，本实现只认 v{}",
                doc.project_schema, PROJECT_SCHEMA_VERSION
            ),
        ));
        // 壳的版本都不认，timeline 的含义就无从谈起 —— 直接返回，别给一堆二次错误。
        return DocIssues { errors, warnings };
    }

    // ---- 资产表本身 ----
    let mut index_of: BTreeMap<&str, usize> = BTreeMap::new();
    for (index, asset) in doc.assets.iter().enumerate() {
        let path = format!("assets[{index}]");
        if asset.id.is_empty() {
            errors.push(Issue::new("asset_id_empty", &format!("{path}.id"), "资产的 id 不能为空".to_string()));
        } else if let Some(first) = index_of.insert(asset.id.as_str(), index) {
            errors.push(Issue::new(
                "asset_id_duplicate",
                &format!("{path}.id"),
                format!("资产 id {} 与 assets[{first}] 重复", asset.id),
            ));
        }
        if asset.uri.is_empty() {
            errors.push(Issue::new(
                "asset_uri_empty",
                &format!("{path}.uri"),
                format!("资产 {} 必须有位置（uri）", asset.id),
            ));
        }
    }

    // ---- 引用完整性 + 素材内越界 ----
    // 后半条是这次的主要收益：v1 做不到，因为容器层不知道素材有多长。
    let mut used: BTreeSet<&str> = BTreeSet::new();
    for (track_index, track) in doc.timeline.tracks.iter().enumerate() {
        for (layer_index, layer) in track.layers.iter().enumerate() {
            let Some(source) = &layer.source else { continue };
            used.insert(source.asset_id.as_str());
            let base = format!("timeline.tracks[{track_index}].layers[{layer_index}]");
            match index_of.get(source.asset_id.as_str()) {
                None => errors.push(Issue::new(
                    "unknown_asset",
                    &format!("{base}.source.asset_id"),
                    format!("引用了登记表里没有的资产：{}", source.asset_id),
                )),
                Some(&asset_index) => {
                    let asset = &doc.assets[asset_index];
                    // 只有**知道素材有多长**时才谈得上越界。
                    if let Some(frame_count) = asset.frame_count {
                        // **先换算再比。** 以前是拿"时间线帧数"直接比"素材帧数" ——
                        // 素材帧率与时间线不一致时两边单位根本不同，这个检查是错的
                        // （60fps 素材放进 30fps 时间线时它会放行两倍的长度）。
                        let asset_timebase = asset
                            .timebase
                            .clone()
                            .unwrap_or_else(|| doc.timeline.timebase.clone());
                        let last = source_frame_at(
                            source.source_in,
                            layer.duration().saturating_sub(1),
                            &doc.timeline.timebase,
                            &asset_timebase,
                        );
                        // 时间基不合法是**契约层**的错误（invalid_timebase），这里不重复报。
                        let exceeded = match last {
                            Ok(last) => source.source_in < 0 || last >= frame_count,
                            Err(_) => false,
                        };
                        if exceeded {
                            errors.push(Issue::new(
                                "source_range_exceeded",
                                &format!("{base}.source.source_in"),
                                format!(
                                    "素材 {} 只有 {frame_count} 帧，而这一层要从第 {} 帧起取 {} 个时间线帧（换算后越界）",
                                    source.asset_id, source.source_in, layer.duration()
                                ),
                            ));
                        }
                    }
                }
            }
        }
    }

    // ---- 从旧版本升上来的工程：**节奏可能变了**，警告 ----
    //
    // v3 改的是 source_in 的**单位**（见 layer::migrate_v2_to_v3）。
    // 素材帧率与时间线一致时行为完全相同；不一致时 local_frame 从"按帧数累加"
    // 变成"按时间换算"，画面节奏会变 —— 那不是 bug，但它**必须是可见的**。
    if matches!(doc.migrated_from, Some(LAYER_SCHEMA_VERSION_V2) | Some(1)) {
        for (index, asset) in doc.assets.iter().enumerate() {
            let Some(timebase) = asset.timebase.clone() else { continue };
            if timebase == doc.timeline.timebase {
                continue;
            }
            warnings.push(Issue::new(
                "timebase_changed_by_migration",
                &format!("assets[{index}].timebase"),
                format!(
                    "工程是从旧版本升上来的：素材 {} 的帧率（{}/{}）与时间线（{}/{}）不同。                     旧版本下 source_in 之后按帧数累加，现在按时间换算 —— **画面节奏会变**，请复核。",
                    asset.id,
                    timebase.num,
                    timebase.den,
                    doc.timeline.timebase.num,
                    doc.timeline.timebase.den
                ),
            ));
        }
    }

    // ---- 登记了但没被引用：**警告**，不阻断 ----
    for (index, asset) in doc.assets.iter().enumerate() {
        if !asset.id.is_empty() && !used.contains(asset.id.as_str()) {
            warnings.push(Issue::new(
                "unused_asset",
                &format!("assets[{index}]"),
                format!("资产 {} 登记了但没被任何图层引用", asset.id),
            ));
        }
    }

    // ---- 契约层：错误原样冒泡，path 前缀由它自己给出 ----
    errors.extend(validate_timeline_v2(&doc.timeline, effects));

    DocIssues { errors, warnings }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layer::{BlendMode, LAYER_SCHEMA_VERSION, Layer, Recorded, TrackV2, TransformV2};
    use crate::schema::TrackKind;

    fn layer_with(id: &str, start: Frame, end: Frame, asset: Option<&str>, source_in: Frame) -> Layer {
        Layer {
            id: id.to_string(),
            start,
            end,
            transform: TransformV2::default(),
            opacity: 1.0,
            blend: BlendMode::Normal,
            enabled: true,
            recorded: Recorded::default(),
            source: asset.map(|a| crate::layer::SourceRef {
                asset_id: a.to_string(),
                source_in,
            }),
            effects: Vec::new(),
            transition_in: None,
            keyframes: Vec::new(),
        }
    }

    fn timeline(layers: Vec<Layer>) -> TimelineV2 {
        TimelineV2 {
            schema: LAYER_SCHEMA_VERSION,
            timebase: TimebaseDto { num: 60, den: 1 },
            markers: Vec::new(),
            tracks: vec![TrackV2 { id: "v1".to_string(), kind: TrackKind::Video, layers }],
        }
    }

    fn asset(id: &str, frames: Option<Frame>) -> Asset {
        Asset {
            id: id.to_string(),
            kind: AssetKind::Video,
            name: id.to_string(),
            uri: format!("file://{id}"),
            frame_count: frames,
            timebase: None,
            width: None,
            height: None,
            content_hash: None,
            tags: BTreeMap::new(),
            note: String::new(),
        }
    }

    fn doc(assets: Vec<Asset>, layers: Vec<Layer>) -> ProjectDoc {
        ProjectDoc {
            project_schema: PROJECT_SCHEMA_VERSION,
            generator: Generator::default(),
            meta: Meta::default(),
            assets,
            timeline: timeline(layers),
            view: View::default(),
            render_hints: RenderHints::default(),
            extensions: BTreeMap::new(),
            migrated_from: None,
        }
    }

    fn codes(issues: &[Issue]) -> Vec<&str> {
        issues.iter().map(|i| i.code.as_str()).collect()
    }

    #[test]
    fn 合法工程文件没有问题() {
        let d = doc(vec![asset("a", Some(100))], vec![layer_with("l1", 0, 30, Some("a"), 10)]);
        let issues = validate_project_doc(&d, &[]);
        assert!(issues.is_ok(), "不该有错误：{:?}", codes(&issues.errors));
        assert!(issues.warnings.is_empty());
    }

    #[test]
    fn 引用不存在的资产要指出是哪个() {
        let d = doc(vec![asset("a", None)], vec![layer_with("l1", 0, 30, Some("b"), 0)]);
        let issues = validate_project_doc(&d, &[]);
        assert_eq!(codes(&issues.errors), vec!["unknown_asset"]);
        assert!(issues.errors[0].path.contains("source.asset_id"), "path 要指到字段：{}", issues.errors[0].path);
        assert!(issues.errors[0].message.contains("b"));
    }

    #[test]
    fn 素材内越界_这是有了资产表才能验的() {
        // 素材只有 100 帧，而这一层要从第 90 帧取 30 帧 → 越界 20 帧。
        let d = doc(vec![asset("a", Some(100))], vec![layer_with("l1", 0, 30, Some("a"), 90)]);
        let issues = validate_project_doc(&d, &[]);
        assert_eq!(codes(&issues.errors), vec!["source_range_exceeded"]);
        assert!(issues.errors[0].message.contains("100"), "提示里要带上素材真实长度");

        // 正好取到末尾是允许的（左闭右开：从 70 取 30 帧 = 70..100）
        let ok = doc(vec![asset("a", Some(100))], vec![layer_with("l1", 0, 30, Some("a"), 70)]);
        assert!(validate_project_doc(&ok, &[]).is_ok());
    }

    #[test]
    fn 不知道素材长度就不查越界_而不是默认通过() {
        // frame_count 为空是**信息缺失**，不是「长度为零」。
        // 所以这里不报越界——但要清楚这是"查不了"，不是"查过没问题"。
        let d = doc(vec![asset("a", None)], vec![layer_with("l1", 0, 30, Some("a"), 999)]);
        assert!(validate_project_doc(&d, &[]).is_ok());
    }

    #[test]
    fn 资产_id_重复与空_uri() {
        let mut second = asset("a", None);
        second.uri = String::new();
        let d = doc(vec![asset("a", None), second], vec![layer_with("l1", 0, 30, Some("a"), 0)]);
        let issues = validate_project_doc(&d, &[]);
        let got = codes(&issues.errors);
        assert!(got.contains(&"asset_id_duplicate"), "得到 {got:?}");
        assert!(got.contains(&"asset_uri_empty"), "得到 {got:?}");
    }

    #[test]
    fn 未被引用的资产是警告_不阻断() {
        let d = doc(
            vec![asset("a", None), asset("b", None)],
            vec![layer_with("l1", 0, 30, Some("a"), 0)],
        );
        let issues = validate_project_doc(&d, &[]);
        assert!(issues.is_ok(), "警告不该阻断载入");
        assert_eq!(codes(&issues.warnings), vec!["unused_asset"]);
    }

    #[test]
    fn 壳版本不认就只报一条() {
        let mut d = doc(vec![], vec![]);
        d.project_schema = 99;
        d.assets.push(asset("", None)); // 顺带塞一个坏资产
        let issues = validate_project_doc(&d, &[]);
        assert_eq!(codes(&issues.errors), vec!["unsupported_project_schema"], "不该产生二次错误");
    }

    #[test]
    fn 契约层的错误原样冒泡() {
        // 同轨重叠：来自 validate_timeline_v2，path 前缀由契约层给出。
        let d = doc(
            vec![asset("a", None)],
            vec![
                layer_with("l1", 0, 30, Some("a"), 0),
                layer_with("l2", 20, 40, Some("a"), 0),
            ],
        );
        let issues = validate_project_doc(&d, &[]);
        let got = codes(&issues.errors);
        assert!(got.contains(&"layer_overlap"), "得到 {got:?}");
    }

    #[test]
    fn 裸契约载入会合成资产表() {
        // v1 裸契约：source 是裸字符串，合成出来的资产表是**最小可用**的。
        let v1 = r#"{"schema":1,"timebase":{"num":60,"den":1},"tracks":[{"id":"v1","kind":"video","clips":[{"id":"c1","source":"a.mp4","source_in":0,"track_at":0,"duration":10,"transform":{"x":0.0,"y":0.0,"scale":1.0,"rotation_deg":0.0},"opacity":1.0}]}]}"#;
        let d = load_doc(v1).expect("v1 裸契约应当能载入");
        assert_eq!(d.project_schema, PROJECT_SCHEMA_VERSION, "载入后一律是壳");
        assert_eq!(d.assets.len(), 1);
        assert_eq!(d.assets[0].id, "a.mp4");
        assert_eq!(d.assets[0].uri, "a.mp4", "位置就用那个字符串，由宿主解释");
        assert!(d.assets[0].frame_count.is_none(), "裸契约里没有长度信息，不能瞎编");
        assert_eq!(d.timeline.schema, LAYER_SCHEMA_VERSION, "应当已迁移到 v2");
        assert_eq!(d.timeline.tracks[0].layers[0].end, 10);
    }

    #[test]
    fn 工程文件形态直接载入() {
        let json = serde_json::to_string(&doc(
            vec![asset("a", Some(50))],
            vec![layer_with("l1", 0, 10, Some("a"), 0)],
        ))
        .expect("应当能序列化");
        let d = load_doc(&json).expect("工程文件应当能载入");
        assert_eq!(d.assets[0].frame_count, Some(50));
    }

    #[test]
    fn 不是_json_要给人话的错() {
        let message = load_doc("这不是 JSON").expect_err("应当报错");
        assert!(message.contains("JSON"), "报错要能看懂：{message}");
    }
}
