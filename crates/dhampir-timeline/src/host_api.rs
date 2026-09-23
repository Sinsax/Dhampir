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
//! 形状冻结在 [`HOST_API_VERSION`]。版本号**目前不在返回体里** ——
//! 加它会改变形状，所以要单独作为一次破坏性改动来做，不能顺手加。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::schema::{Frame, Issue};

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
/// 序列化成 JSON 字符串。
///
/// 所有字段都可序列化，所以这里的兜底**理论上不可达** ——
/// 但把兜底放在一个地方，好过在每个导出里各写一次 unwrap。
pub fn to_json<T: Serialize>(value: &T) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| String::from("{\"error\":\"序列化失败\"}"))
}
/// 形状版本。**字段增减都要 +1。**
pub const HOST_API_VERSION: u32 = 1;

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
#[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl OpenResult {
    /// 解析成功。`ok` 表示**校验**是否通过（解析成功但校验不过也是正常的）。
    pub fn opened(issues: Vec<Issue>) -> Self {
        Self { parsed: true, ok: issues.is_empty(), issues: Some(issues), error: None }
    }

    /// 连 JSON 都没解析成功。
    pub fn unparsed(message: String) -> Self {
        Self { parsed: false, ok: false, issues: None, error: Some(message) }
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

/// `dhampir_project_frame` 的返回体。
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FrameResult {
    pub frame: Frame,
    pub layers: Vec<LayerView>,
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
            error: Some("还没有载入通过校验的工程".to_string()),
        };
        let value = serde_json::to_value(&result).unwrap();
        assert_eq!(keys(&value), sorted(&["frame", "layers", "error"]));
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
}
