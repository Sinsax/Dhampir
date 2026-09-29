//! **只看「求值算出了哪些层」** —— 用来回答"某一层为什么没被画出来"。
//!
//! # 为什么要有它
//!
//! 贴纸（`image_sequence`）那类层在浏览器里逐层排查时，**一轮只能问一层**：
//! `seek` → `sources_for` → `evaluate`，而每问一层都要跑一次真页面。
//! 这个工具把这条链**一次问到底**，而且不需要浏览器、wasm 构建与服务端。
//!
//! # 它同时打印两份东西，因为断点可能在两者之间
//!
//! * **工程里有哪些轨/层**（`project.timeline.tracks`）—— 若这里就没有，问题在**解析**；
//! * **求值后有哪些层**（`composite.layers`）—— 若这里有、那里没有，问题在**求值**。
//!
//! 跑：
//!
//! ```text
//! cargo run -q -p dhampir-worker --example eval_layers -- <project.json> <frame> [source-substring]
//! ```
//!
//! 传了 `source-substring` 时，命中与否决定退出码（0 / 1）—— 这样它也能当一条判据用。

use dhampir_core::compose;
// **用 v2 的类型与求值入口**：这份工程是 schema 4（字段 `layers`），而 v1 的 `Project`
// 字段叫 `clips` —— 用 v1 解析会**静默丢光所有层**（clips=0），求值自然 0 层。
// 浏览器里 wasm 走的就是 v2（`evaluate_v2_with_assets`），工具必须与它一致。
use dhampir_core::timeline::layer::TimelineV2;
use dhampir_core::timeline::project::ProjectDoc;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let path = args.first().ok_or("用法：eval_layers <project.json> <frame> [source-substring]")?;
    let frame: i64 = args
        .get(1)
        .and_then(|s| s.parse().ok())
        .ok_or("第二个参数要给帧号")?;
    let want = args.get(2).cloned();

    let text = std::fs::read_to_string(path)?;
    // 转译器给的是**外层 doc**（assets / extensions / timeline / render_hints …），
    // 而 `Project` 要的是里面的 `timeline` 那一层。少取一层就是 "missing field `schema`"。
    let doc: serde_json::Value = serde_json::from_str(&text)?;
    let timeline = doc
        .get("timeline")
        .cloned()
        .unwrap_or_else(|| doc.clone());
    let project: TimelineV2 = serde_json::from_value(timeline)?;

    println!("== 工程里的轨 ==");
    let tracks = &project.tracks; // `Project` 本身就是时间线（`timeline` 那层在外层 doc 里）
    println!("  轨数 {}", tracks.len());
    for track in tracks {
        // 字段名叫 `clips`（不是 JSON 里的 `layers`）—— 先把真实形状打出来再说。
        println!("  {:<14} kind={:<10} layers={}", track.id, format!("{:?}", track.kind), track.layers.len());
        // **只打 id/区间/源** —— 打印整个 Layer 的 Debug 会把几百个 keyframe 倒出来，
        // 把真正的答案淹掉（我这么干过一次，白跑一轮）。
        for layer in &track.layers {
            let src = match &layer.source {
                Some(s) => format!("{} (in {})", s.asset_id, s.source_in),
                None => "（无源）".to_string(),
            };
            println!(
                "      {:<31} {:>5}..{:<5} en={:<5} loop={:<5} {}",
                layer.id, layer.start, layer.end, layer.enabled, layer.loop_source, src
            );
        }
    }
    println!();

    // **与 wasm 逐字相同的那条调用**：`doc.asset_timebases()` + `Some(&assets)`。
    // 之前我手搓 assets 是错的 —— `TimebaseDto` 是私有的，公开路径只有这一个入口，
    // 而它**连帧数一起登记**（循环素材取模要周期，见 project.rs 的说明）。
    let doc2: ProjectDoc = serde_json::from_str(&text)?;
    let real_assets = doc2.asset_timebases();
    println!("== engine 实际用的素材表（doc.asset_timebases()）==");
    println!("  {} 项", real_assets.len());
    if let Some(w) = &want {
        for a in doc2.assets.iter().filter(|a| a.id.contains(w.as_str())) {
            println!(
                "    {}：timebase={:?} frame_count={:?}",
                a.id,
                real_assets.get(&a.id),
                real_assets.frame_count(&a.id)
            );
        }
    }
    println!();

    for (label, assets) in [
        ("None（不带素材表）", None),
        ("Some(doc.asset_timebases())", Some(&real_assets)),
    ] {
        let c = compose::evaluate_v2_with_assets(&project, frame, assets);
        let hits: Vec<_> = c
            .layers
            .iter()
            .filter(|l| match &want {
                Some(w) => l.source.contains(w.as_str()),
                None => false,
            })
            .collect();
        println!(
            "  assets = {:<28} → {} 层，含贴纸={} {:?}",
            label,
            c.layers.len(),
            !hits.is_empty(),
            hits.iter().map(|l| (l.source.clone(), l.source_frame)).collect::<Vec<_>>()
        );
    }
    println!();

    let composite = compose::evaluate_v2_with_assets(&project, frame, None);
    println!("== 帧 {} 求值后的层（{} 层）==", frame, composite.layers.len());
    let mut hit = false;
    for l in &composite.layers {
        let tag = match &want {
            Some(w) if l.source.contains(w.as_str()) => {
                hit = true;
                "  ← **命中**"
            }
            _ => "",
        };
        println!(
            "  {:<34} source_frame={:<6} opacity={:.3}{}",
            l.source, l.source_frame, l.opacity, tag
        );
    }

    match &want {
        Some(w) => {
            println!();
            if hit {
                println!("✓ 帧 {} 上有包含 {:?} 的层", frame, w);
                Ok(())
            } else {
                println!("✗ 帧 {} 上**没有**包含 {:?} 的层 —— 断点在求值（或它上游）", frame, w);
                std::process::exit(1);
            }
        }
        None => Ok(()),
    }
}
