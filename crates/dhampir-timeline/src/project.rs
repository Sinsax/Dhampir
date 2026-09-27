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
    AssetTimebases, LAYER_SCHEMA_VERSION, LAYER_SCHEMA_VERSION_V2, LAYER_SCHEMA_VERSION_V3, TimelineV2,
    migrate_v1_to_v2, migrate_v2_to_v3, migrate_v3_to_v4, source_frame_at, validate_timeline_v2,
};
use crate::schema::{
    parse_effect_target, EffectSpec, Frame, Issue, Project, TimebaseDto, TrackKind,
    TRANSFORM_TARGETS,
};

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
    /// 动图（GIF / APNG / 动画 WebP）。
    ///
    /// # 为什么与 `Image` 分开
    ///
    /// 静态图的"素材内定位"是**没有意义**的：它永远同一张。动图有 ——
    /// 第 0 帧与第 40 帧是两张不同的画。
    ///
    /// 分开之后，校验层能对两者提不同的要求：
    /// 动图**必须**给出 `frame_count`（否则不知道该播到哪一帧、也不知道怎么循环），
    /// 静态图给了也无所谓（多余信息，不报错）。
    ///
    /// 渲染侧**不需要**为它写第二条路径：`frame_count` + `timebase` 这套
    /// 定位机制与视频共用，所以"取动图的第 N 帧"就是"取素材的第 N 帧"。
    ///
    /// # 目前的完成度（**别把它当成已通**）
    ///
    /// - **契约与求值层：已通**。逐帧定位复用视频那一套，有测试钉着。
    /// - **浏览器宿主：已通**（`<img>` / `ImageBitmap` 自己按动画时序给帧）。
    /// - **原生宿主：未通**。解码器是 `ffmpeg` 一把梭当帧序列读，
    ///   而它对动图是**按标称帧率把帧铺开**的：实测一个 2 帧的 GIF 解出 20 帧，
    ///   于是作者填的 `frame_count` 与实际帧数对不上，表现是"动图停在前几帧"且**不报错**。
    ///
    /// 详见 `plan/p9-polish-parity-design.md` §11.1。
    ImageSequence,
    /// 字幕（SRT / ASS）。**它也是一种素材** —— 位置同样由 uri 给，
    /// 于是"字幕"不必发明第二套引用机制：轨道引用它，与引用一段视频没有区别。
    Subtitle,
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

/// 每个资产被引用了多少次。
///
/// **判断"引用了没有"只有这一份实现** —— 校验里的 unused_asset 与 CLI 的
/// library 都读它。两份实现一定会漂，而漂了以后"CLI 说没用、校验说用了"
/// 这种自相矛盾会让人不信任何一边。
pub fn asset_reference_counts(doc: &ProjectDoc) -> BTreeMap<String, usize> {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for track in &doc.timeline.tracks {
        for layer in &track.layers {
            if let Some(source) = &layer.source {
                *counts.entry(source.asset_id.clone()).or_insert(0) += 1;
            }
        }
        // 弹幕轨的素材是在**轨道级**引用的。不算进来，一份正在用的弹幕素材
        // 会被报成 unused_asset —— 而那种误报会让人去删掉它。
        if let Some(spec) = &track.danmaku {
            if !spec.asset_id.is_empty() {
                *counts.entry(spec.asset_id.clone()).or_insert(0) += 1;
            }
        }
    }
    counts
}

impl ProjectDoc {
    /// 资产 id → 时间基（+ 帧数）。求值层用它把时间线帧号换算成素材帧号。
    ///
    /// **没登记时间基的资产不进表**：不进表 = 走恒等换算（素材帧率按时间线算），
    /// 这正是升级前的行为。**不要给它猜一个帧率** —— 猜错的表现是画面变速，
    /// 而那是"看起来完全正常"的那一类错。
    ///
    /// # 帧数也要一起登记
    ///
    /// 循环素材（`Layer::loop_source`）取模要**周期**，而周期就是 `frame_count`。
    /// 早先这里只登记时间基，于是 `AssetTimebases::frame_count()` 永远是 `None`、
    /// 循环静默失效 —— 表现是**校验放行、渲染时读越界帧**
    /// （实测：`呆(贴纸)_1.gif` 32 帧的动图铺 241 帧，到第 193 个时间线帧就报
    /// `source_decode_failed`）。两个入口登记的东西不一致是这类错的经典来源。
    pub fn asset_timebases(&self) -> AssetTimebases {
        let mut table = AssetTimebases::new();
        for asset in &self.assets {
            if asset.id.is_empty() {
                continue;
            }
            if let Some(timebase) = asset.timebase.clone() {
                table.insert_with_count(asset.id.clone(), timebase, asset.frame_count);
            }
        }
        table
    }

    /// **文档坐标系**：契约里的像素量（现在只有 transform.x/y，以及调整图层的模糊半径）
    /// 以它为度量单位。
    ///
    /// # 为什么不是「把 render_hints 当元数据」
    ///
    /// render_hints 以前只是「宿主的提示」。但只要契约里有像素量，就**必须**有一个
    /// 像素量所属的坐标系，否则同一个工程在不同渲染尺寸下含义不同 ——
    /// 表现就是「预览所见 != 成片所得」。所以它从提示升级成了坐标系的定义者。
    ///
    /// 尺寸为 0 时兜到 1：调用方拿它做除数，除零会让整帧变成 NaN。**不猜一个「合理」尺寸** ——
    /// 猜错的表现是画面位置对但比例错，属于「看起来完全正常」的那一类错。
    pub fn sequence_size(&self) -> (u32, u32) {
        (self.render_hints.width.max(1), self.render_hints.height.max(1))
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
        // 迁移梯子：v2 -> v3 -> v4，一级一级走。
        // 不能从 v2 直接跳到 v4 —— 中间那级的语义变化会被跳过。
        if from == LAYER_SCHEMA_VERSION_V2 {
            doc.timeline = migrate_v2_to_v3(&doc.timeline);
        }
        if from == LAYER_SCHEMA_VERSION_V2 || from == LAYER_SCHEMA_VERSION_V3 {
            doc.timeline = migrate_v3_to_v4(&doc.timeline);
            doc.migrated_from = Some(from);
        } else if from != LAYER_SCHEMA_VERSION {
            return Err(format!(
                "工程文件里的时间线是 v{from}，本实现只认 v{LAYER_SCHEMA_VERSION_V2}、v{LAYER_SCHEMA_VERSION_V3} 与 v{LAYER_SCHEMA_VERSION}"
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
        let mut doc = shell_from_timeline(migrate_v3_to_v4(&migrate_v2_to_v3(&v2)));
        doc.migrated_from = Some(1);
        return Ok(doc);
    }

    // ---- 裸契约 v2 或 v3 ----
    let timeline: TimelineV2 =
        serde_json::from_value(value).map_err(|e| format!("v2/v3 契约字段不符：{e}"))?;
    let from = timeline.schema;
    let needs_migration = from == LAYER_SCHEMA_VERSION_V2 || from == LAYER_SCHEMA_VERSION_V3;
    let mut doc = if from == LAYER_SCHEMA_VERSION_V2 {
        shell_from_timeline(migrate_v3_to_v4(&migrate_v2_to_v3(&timeline)))
    } else if from == LAYER_SCHEMA_VERSION_V3 {
        shell_from_timeline(migrate_v3_to_v4(&timeline))
    } else {
        shell_from_timeline(timeline)
    };
    if needs_migration {
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
    let counts = asset_reference_counts(doc);
    for (track_index, track) in doc.timeline.tracks.iter().enumerate() {
        for (layer_index, layer) in track.layers.iter().enumerate() {
            let Some(source) = &layer.source else { continue };
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
                    //
                    // **`loop_source` 为真时根本不越界** —— 那正是这个开关的用途：
                    // 短动图铺长区间。不在这里放行的话，动图贴纸永远过不了校验，
                    // 于是只剩下"谎报 frame_count"这条歪路。
                    if let Some(frame_count) = asset.frame_count.filter(|_| !layer.loop_source) {
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
        if !asset.id.is_empty() && !counts.contains_key(asset.id.as_str()) {
            warnings.push(Issue::new(
                "unused_asset",
                &format!("assets[{index}]"),
                format!("资产 {} 登记了但没被任何图层引用", asset.id),
            ));
        }
    }

    // ---- 字幕 / 弹幕轨：素材种类要对得上 ----
    //
    // 这两条是**新的轨类型带来的新契约**：字幕轨只能引用字幕素材，
    // 弹幕轨必须带参数。不查的话，把一段 mp4 挂到字幕轨上会一路静默到渲染，
    // 而那时的表现是"什么都没有"。
    for (asset_index, asset) in doc.assets.iter().enumerate() {
        // 动图必须有帧数：没有它就不知道"这一帧该取第几张"，
        // 也不知道循环从哪里回卷 —— 而表现是**它一直停在第一帧**，
        // 看起来像"动图没动"，查起来要翻到素材登记表才发现。
        if asset.kind == AssetKind::ImageSequence && asset.frame_count.is_none() {
            errors.push(Issue::new(
                "image_sequence_needs_frame_count",
                &format!("assets[{asset_index}].frame_count"),
                format!(
                    "动图素材 {} 必须给出 frame_count（它决定这一帧取第几张、以及循环点）",
                    asset.id
                ),
            ));
        }
        // 帧数给了就必须是正的：0 或负数会让"取哪一帧"变成一个空集合。
        if asset.kind == AssetKind::ImageSequence
            && let Some(count) = asset.frame_count
            && count <= 0
        {
            errors.push(Issue::new(
                "image_sequence_bad_frame_count",
                &format!("assets[{asset_index}].frame_count"),
                format!("动图素材 {} 的 frame_count 必须是正数，实得 {count}", asset.id),
            ));
        }
    }

    for (track_index, track) in doc.timeline.tracks.iter().enumerate() {
        let path = format!("tracks[{track_index}]");
        match track.kind {
            TrackKind::Subtitle => {
                for (layer_index, layer) in track.layers.iter().enumerate() {
                    let Some(source) = &layer.source else { continue };
                    let kind = index_of
                        .get(source.asset_id.as_str())
                        .map(|index| doc.assets[*index].kind);
                    match kind {
                        Some(AssetKind::Subtitle) => {}
                        Some(_) => errors.push(Issue::new(
                            "subtitle_asset_kind",
                            &format!("{path}.layers[{layer_index}].source.asset_id"),
                            format!("字幕轨只能引用 subtitle 素材，而 {} 不是", source.asset_id),
                        )),
                        // 资产根本不存在时 unknown_asset 已经报过，这里不重复报。
                        None => {}
                    }
                }
            }
            TrackKind::Danmaku => match &track.danmaku {
                None => errors.push(Issue::new(
                    "missing_danmaku_spec",
                    &path,
                    "弹幕轨必须有 danmaku 参数（它指向弹幕素材）".to_string(),
                )),
                Some(spec) => {
                    let kind = index_of
                        .get(spec.asset_id.as_str())
                        .map(|index| doc.assets[*index].kind);
                    match kind {
                        Some(AssetKind::Subtitle) => {}
                        Some(_) => errors.push(Issue::new(
                            "danmaku_asset_kind",
                            &format!("{path}.danmaku.asset_id"),
                            format!("弹幕必须指向 subtitle 素材，而 {} 不是", spec.asset_id),
                        )),
                        None => errors.push(Issue::new(
                            "unknown_asset",
                            &format!("{path}.danmaku.asset_id"),
                            format!("引用了登记表里没有的资产：{}", spec.asset_id),
                        )),
                    }
                }
            },
            TrackKind::Video | TrackKind::Audio => {}
        }
    }

    // ---- 关键帧的 target 必须指得到东西 ----
    //
    // **为什么这条必须有。** `target` 是一个自由字符串：写错了（`"rotatoin"`、
    // `"scal"`）求值时**静默不生效** —— 曲线还在、工程还是合法的、
    // 预览也不报错，只是那条动画**根本没动**。这正是最难查的一类问题：
    // 用户看到"我明明打了关键帧，画面却不动"。
    //
    // 判定用 [`TRANSFORM_TARGETS`] 与 [`parse_effect_target`] —— 也就是
    // "这个常量到底管什么"的那个答案。**从前它只被定义、从没被用过**，
    // 于是它描述的规则和实际接受的输入是两回事。
    for (track_index, track) in doc.timeline.tracks.iter().enumerate() {
        for (layer_index, layer) in track.layers.iter().enumerate() {
            let base = format!("timeline.tracks[{track_index}].layers[{layer_index}]");
            for (key_index, keyframe) in layer.keyframes.iter().enumerate() {
                let at = format!("{base}.keyframes[{key_index}].target");
                let target = keyframe.target.as_str();
                if TRANSFORM_TARGETS.contains(&target) {
                    continue;
                }
                match parse_effect_target(target) {
                    // `effect.<下标>.<参数名>`：下标必须指向**这一层真有**的那条特效，
                    // 否则这个关键帧同样是空转的。
                    Some(parsed) => {
                        if parsed.index >= layer.effects.len() {
                            errors.push(Issue::new(
                                "keyframe_effect_index",
                                &at,
                                format!(
                                    "关键帧指向 effect.{}，可这一层只有 {} 条特效",
                                    parsed.index,
                                    layer.effects.len()
                                ),
                            ));
                        } else if !layer.effects[parsed.index]
                            .params
                            .contains_key(&parsed.param)
                        {
                            errors.push(Issue::new(
                                "keyframe_effect_param",
                                &at,
                                format!(
                                    "关键帧指向 effect.{}.{}，可这条特效没有「{}」这个参数 —— \
                                     驱动一个不存在的参数不会报错，那条动画就是不动",
                                    parsed.index, parsed.param, parsed.param
                                ),
                            ));
                        }
                    }
                    None => errors.push(Issue::new(
                        "unknown_keyframe_target",
                        &at,
                        format!(
                            "「{target}」既不是可驱动的元素量（{}），\
                             也不是 effect.<下标>.<参数名> 的形状 —— \
                             这条曲线不会驱动任何东西",
                            TRANSFORM_TARGETS.join(" / ")
                        ),
                    )),
                }
            }
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
            gain: 1.0,
            recorded: Recorded::default(),
            source: asset.map(|a| crate::layer::SourceRef {
                asset_id: a.to_string(),
                source_in,
            }),
            loop_source: false,
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
            tracks: vec![TrackV2 {
                id: "v1".to_string(),
                kind: TrackKind::Video,
                layers,
                subtitle: None,
                danmaku: None,
            }],
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

    // ---- 关键帧 target 的判定 ----

    /// 造一层带若干关键帧的工程。
    fn doc_with_keyframes(targets: &[&str], effects: Vec<crate::schema::Effect>) -> ProjectDoc {
        let mut layer = layer_with("l1", 0, 30, Some("a.mp4"), 0);
        layer.effects = effects;
        layer.keyframes = targets
            .iter()
            .map(|target| crate::schema::Keyframe {
                frame: 0,
                target: target.to_string(),
                value: 1.0,
                easing: crate::schema::Easing::Linear,
            })
            .collect();
        doc(vec![asset("a.mp4", Some(100))], vec![layer])
    }

    /// 跑一次校验，只看错误码。
    fn validate_codes(d: &ProjectDoc) -> Vec<String> {
        validate_project_doc(d, &[])
            .errors
            .iter()
            .map(|i| i.code.clone())
            .collect()
    }

    #[test]
    fn 拼错的关键帧目标要报错() {
        // **这是"静默不动"的那一类。** 曲线在、工程合法、预览不报错，
        // 只是那条动画根本没动 —— 用户看到的是"我打了关键帧，画面却不动"。
        for typo in ["rotatoin", "scal", "Opacity", "effect", ""] {
            let found = validate_codes(&doc_with_keyframes(&[typo], Vec::new()));
            assert!(
                found.contains(&"unknown_keyframe_target".to_string()),
                "「{typo}」应当被判成未知目标，实得 {found:?}"
            );
        }
    }

    #[test]
    fn 合法的关键帧目标一个都不许报错() {
        // 反向用例：把判定写得过严会让**所有**正常工程都报错。
        for target in TRANSFORM_TARGETS {
            let found = validate_codes(&doc_with_keyframes(&[target], Vec::new()));
            assert!(
                !found.contains(&"unknown_keyframe_target".to_string()),
                "「{target}」是合法的元素量，不该报错，实得 {found:?}"
            );
        }
    }

    #[test]
    fn 指向不存在特效的关键帧要报错() {
        // `effect.3.radius` 而这一层只有 0 条特效 —— 同样是空转。
        let found = validate_codes(&doc_with_keyframes(&["effect.3.radius"], Vec::new()));
        assert!(
            found.contains(&"keyframe_effect_index".to_string()),
            "实得 {found:?}"
        );
    }

    #[test]
    fn 指向不存在的参数要报错() {
        // 有这条特效，但没这个参数。**这条比下标越界更隐蔽**：
        // `effect.0.radisu` 看起来完全合理。
        let effect = crate::schema::Effect {
            kind: "gaussian_blur".to_string(),
            params: [("radius".to_string(), 4.0)].into_iter().collect(),
            ..Default::default()
        };
        let found = validate_codes(&doc_with_keyframes(&["effect.0.radisu"], vec![effect]));
        assert!(
            found.contains(&"keyframe_effect_param".to_string()),
            "实得 {found:?}"
        );
    }

    #[test]
    fn 指向真实存在的特效参数要放行() {
        let effect = crate::schema::Effect {
            kind: "gaussian_blur".to_string(),
            params: [("radius".to_string(), 4.0)].into_iter().collect(),
            ..Default::default()
        };
        let found = validate_codes(&doc_with_keyframes(&["effect.0.radius"], vec![effect]));
        assert!(
            !found.contains(&"keyframe_effect_param".to_string())
                && !found.contains(&"keyframe_effect_index".to_string()),
            "合法引用不该报错，实得 {found:?}"
        );
    }

    #[test]
    fn 动图没有帧数要报错() {
        // 没有 frame_count 就不知道"这一帧取第几张"，也不知道循环点在哪。
        // 不报的话表现是**它一直停在第一帧** —— 看起来像"动图没动"，
        // 而查起来要翻到素材登记表才发现。
        let mut anim = asset("anim", None);
        anim.kind = AssetKind::ImageSequence;
        let d = doc(vec![anim], vec![layer_with("l1", 0, 30, Some("anim"), 0)]);
        let issues = validate_project_doc(&d, &[]);
        assert!(
            codes(&issues.errors).contains(&"image_sequence_needs_frame_count"),
            "应当报动图缺帧数，实得 {:?}",
            codes(&issues.errors)
        );
        // path 要指到那个字段，光说"有问题"没法定位。
        assert!(
            issues
                .errors
                .iter()
                .any(|i| i.path.contains("assets[0].frame_count")),
            "path 要指到 frame_count：{:?}",
            issues.errors.iter().map(|i| &i.path).collect::<Vec<_>>()
        );
    }

    #[test]
    fn 动图的帧数必须是正数() {
        let mut anim = asset("anim", Some(0));
        anim.kind = AssetKind::ImageSequence;
        let d = doc(vec![anim], vec![layer_with("l1", 0, 30, Some("anim"), 0)]);
        let issues = validate_project_doc(&d, &[]);
        assert!(
            codes(&issues.errors).contains(&"image_sequence_bad_frame_count"),
            "帧数 0 要报错，实得 {:?}",
            codes(&issues.errors)
        );
    }

    #[test]
    fn 静态图没有帧数不算错() {
        // **静态图的"素材内定位"没有意义**：它永远同一张。
        // 对它提 frame_count 的要求会是假报错 —— 而假报错比不报更糟，
        // 因为下一个人会把这条校验删掉。
        let mut still = asset("logo", None);
        still.kind = AssetKind::Image;
        let d = doc(vec![still], vec![layer_with("l1", 0, 30, Some("logo"), 0)]);
        let issues = validate_project_doc(&d, &[]);
        assert!(issues.is_ok(), "静态图不该被要求给帧数：{:?}", codes(&issues.errors));
    }

    #[test]
    fn 动图给了合法帧数就通过() {
        let mut anim = asset("anim", Some(12));
        anim.kind = AssetKind::ImageSequence;
        // 12 帧的素材，取 0..12 正好取完（左闭右开）。
        let d = doc(vec![anim], vec![layer_with("l1", 0, 12, Some("anim"), 0)]);
        let issues = validate_project_doc(&d, &[]);
        assert!(issues.is_ok(), "合法的动图工程不该报错：{:?}", codes(&issues.errors));
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

    // -----------------------------------------------------------------------
    // `asset_timebases()` 要把**帧数**一起带过去
    //
    // 这一组是真机逼出来的：循环素材（贴纸动图）取模要周期，而周期是帧数。
    // 早先这里只登记时间基，于是 `frame_count()` 永远 `None`、
    // **循环静默失效** —— 校验放行（它看 `loop_source` 就跳过越界检查），
    // 渲染时才炸（`source_decode_failed`：32 帧的动图要第 32 帧）。
    // -----------------------------------------------------------------------

    #[test]
    fn 资产表要带帧数过去() {
        // 没带过去的话，循环素材无从取模 —— 而那是**看不出来**的错：
        // 工程合法、校验全绿，只在渲染时越界。
        let mut a = asset("gif", Some(32));
        a.timebase = Some(TimebaseDto { num: 10, den: 1 });
        a.kind = AssetKind::ImageSequence;
        let table = doc(vec![a], vec![layer_with("l", 0, 10, Some("gif"), 0)]).asset_timebases();
        assert_eq!(table.get("gif"), Some(&TimebaseDto { num: 10, den: 1 }), "时间基要过去");
        assert_eq!(table.frame_count("gif"), Some(32), "**帧数也要过去**，否则循环取不了模");
    }

    #[test]
    fn 没登记时间基的资产仍然不进表() {
        // 老行为不许变：不进表 = 恒等换算（素材帧率按时间线算）。
        // 「不要给它猜一个帧率」是 `asset_timebases` 注释里就写着的纪律。
        let table = doc(vec![asset("a", Some(50))], vec![layer_with("l", 0, 10, Some("a"), 0)])
            .asset_timebases();
        assert!(table.get("a").is_none(), "没有时间基就不该进表");
        assert!(table.frame_count("a").is_none());
    }

    #[test]
    fn 资产表里的帧数只反映真的登记了长度的那些() {
        // `frame_count: None`（裸契约载入后就是这样）要如实是 `None` ——
        // 编一个长度会让循环取模取到一个假周期。
        let mut a = asset("gif", None);
        a.timebase = Some(TimebaseDto { num: 10, den: 1 });
        let table = doc(vec![a], vec![layer_with("l", 0, 10, Some("gif"), 0)]).asset_timebases();
        assert_eq!(table.get("gif").is_some(), true, "有时间基就进表");
        assert_eq!(table.frame_count("gif"), None, "没登记长度就如实是 None");
    }
}
