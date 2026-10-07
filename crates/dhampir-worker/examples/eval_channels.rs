//! 逐帧打出**每个图层的五个通道**（NDJSON）—— 给网页动画的第二个宿主当对照值。
//!
//! # 它回答什么
//!
//! `web/anim-eval.mjs` 是**第二份求值实现**（网页那边要脱离 wasm 也能跑）。
//! 两份实现会漂，而漂了没有任何东西会红 —— 所以需要这条对照：
//! 同一份工程、同一批帧，core 算出来的每个通道值，与网页算出来的逐个比。
//! 判据在 `scripts/check-anim-eval.mjs`（容差 1e-6）。
//!
//! 跑：
//!
//! ```text
//! cargo run -q -p dhampir-worker --example eval_channels -- <project.doc.json> <from> <to>
//! ```
//!
//! 输出一行一帧（NDJSON），**顺序按帧**，层按**从下往上**（与渲染顺序一致）：
//!
//! ```json
//! {"frame":0,"layers":[{"id":"title","opacity":1.0,"x":-80.0,"y":12.0,"scale":0.9,"rotation":0.0}]}
//! ```

use dhampir_core::compose;
use dhampir_core::timeline::layer::TimelineV2;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let path = args
        .first()
        .ok_or("用法：eval_channels <project.doc.json> <from> <to>")?;
    let from: i64 = args
        .get(1)
        .and_then(|value| value.parse().ok())
        .ok_or("第二个参数是起始帧")?;
    let to: i64 = args
        .get(2)
        .and_then(|value| value.parse().ok())
        .ok_or("第三个参数是结束帧（含）")?;

    let text = std::fs::read_to_string(path)?;
    // 转译器给的是**外层 doc**；`TimelineV2` 要的是里面 `timeline` 那一层。
    // 少取一层就是 missing field schema（eval_layers 那边也记过这一脚）。
    let doc: serde_json::Value = serde_json::from_str(&text)?;
    let timeline_value = doc.get("timeline").cloned().unwrap_or_else(|| doc.clone());
    let timeline: TimelineV2 = serde_json::from_value(timeline_value)?;

    for frame in from..=to {
        let composite = compose::evaluate_v2_with_assets(&timeline, frame, None);
        let layers: Vec<serde_json::Value> = composite
            .layers
            .iter()
            .map(|layer| {
                serde_json::json!({
                    "id": layer.clip_id,
                    "opacity": layer.opacity,
                    "x": layer.transform.x,
                    "y": layer.transform.y,
                    "scale": layer.transform.scale,
                    "rotation": layer.transform.rotation_deg,
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::json!({ "frame": frame, "layers": layers })
        );
    }
    Ok(())
}
