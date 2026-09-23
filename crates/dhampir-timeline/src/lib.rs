//! # dhampir-timeline
//!
//! 时间线的**纯数据**模型。这一层不依赖 wgpu，也不依赖任何渲染概念——
//! 它定义的是"时间轴长什么样"，不是"怎么画"。
//!
//! ## 铁律
//!
//! 时间一律用**整数帧号**表示，不用浮点秒。剪辑里的"帧精确"是天然成立的，
//! 不靠四舍五入；而 `0.1 + 0.2 != 0.3` 这种问题一旦进入时间轴就会变成
//! "某些片段的最后一帧被吃掉"这类只有在导出后才看得见的 bug。
//!
//! 帧率用**有理数** `num/den`（29.97 其实是 `30000/1001`）。用浮点表示帧率
//! 除了不精确，还会让"同一份源码两个运行时输出一致"这条验收失去意义——
//! 探针要测的是运行时差异，不是浮点差异。
//!
//! ## 模块
//!
//! - [`timebase`]：有理数帧率与它的推导量（标称帧率、每帧的精确 tick 数）
//! - [`timecode`]：帧号 ↔ SMPTE non-drop-frame 时间码，纯整数
//! - [`selfcheck`]：**跨运行时等价性探针**——把一组固定用例算成逐字节可比的
//!   文本与摘要，native 与 wasm 各跑一次，摘要相等即证明"同一份源码两个运行时"
//! - [`golden`]：探针报告的**期望字节**，两端共用同一份、同一个比对函数
//!
//! ## M0 明确不做
//!
//! 完整时间线 Schema（轨道 / 片段 / 特效参数，M4）、OTIO 适配、drop-frame
//! 时间码（只影响显示与解析，M4）、帧号解析（`FromStr`）。

pub mod golden;
// 元素模型 v2。与 v1（schema 模块）**并存**：v1 保持可用，切换是下一步。
// 这样每个提交都是绿的，而不是把仓库停在「改了一半」的状态。
// 同样建立在 serde 上，所以跟 schema 一起按 feature 门控。
// PR 式编辑操作。**业务规则只在这一份**：CLI 与预览都调它。
#[cfg(feature = "serde")]
pub mod edit;
#[cfg(feature = "serde")]
pub mod host_api;
#[cfg(feature = "serde")]
pub mod layer;
// 工程文件（壳）：渲染契约的超集（契约 + 资产表 + 元信息 + 应用状态）。
#[cfg(feature = "serde")]
pub mod project;
// schema 整个模块都建立在 serde 上（类型要能进 JSON），所以关掉 serde 就没它。
#[cfg(feature = "serde")]
pub mod schema;
// 字幕与弹幕：**解析只有这一份实现**，两端都调它（纯文本，零依赖）。
#[cfg(feature = "serde")]
pub mod subtitle;
// 弹幕的**共享泳道分配**与滚动落点。与字幕的 text_layout 是同一条分工：
// 结构由这里算一份，宿主只负责把字画进给定的矩形。
#[cfg(feature = "serde")]
pub mod danmaku;
pub mod text_layout;
pub mod selfcheck;
pub mod timebase;
pub mod timecode;

pub use golden::{SELFCHECK_REPORT_V1, golden_matches_current_report, golden_verdict};
pub use selfcheck::{PROBE_FORMAT_VERSION, fnv1a64, probe_digest, probe_report};
pub use timebase::{Timebase, TimebaseError};
pub use timecode::{
    Timecode, exact_ticks_to_frame, frame_to_exact_ticks, frame_to_timecode, timecode_to_frame,
};
