//! T5.1 的量化工具：**回退有多常见、退多远、池子要多大**。
//!
//! # 它为什么必须在改代码之前跑
//!
//! 「按源内顺序重排渲染」与「有界回退窗口」是两条路，选哪条取决于回退的距离分布：
//!
//! * 退得浅（几帧到几十帧）-> 池子装得下 -> 回退变成命中，只有内存代价；
//! * 退得深（整段倒放）-> 池子装不下 -> 每次回退都要**重启解码器从头读**，
//!   代价随目标帧号线性增长。
//!
//! 所以先出数，再定 [`dhampir_worker::pipeline::POOL_BYTES_PER_SOURCE`]。
//! **没有数字就不许决定** —— 这是 plan 的原话。
//!
//! # 它为什么是纯的
//!
//! 它不碰 GPU、不碰 ffmpeg：取帧顺序是从**求值层**摊出来的
//! （[`request_schedule`] 走的正是渲染器真会请求的那条路），池子行为走的是
//! **产品同一段规则**（[`PoolCursor`]）。所以它快、可重复，且量的不是"另一种实现"。
//!
//! # 用法
//!
//!     cargo run -p dhampir-worker --example rewind_census -- <工程文件>...
//!
//! 输出 NDJSON：一行一个工程，末行是合计。
//!
//! # 量纲限制
//!
//! * 数的是**请求**，不是时间：一次回退要读多少帧是精确的，但"多花多少毫秒"取决于
//!   素材分辨率与解码速度，不在这个数里；
//! * 工程自带的资产时间基决定源帧号怎么换算（60fps 素材放进 30fps 时间线会跳帧），
//!   少了它算出来的回退次数是另一个数 —— 所以这里**要求**工程文件本身；
//! * 池子模型含"只留需求表里的帧"这条策略：顺序出片时中间帧不留，于是
//!   同一个池子在两条路上给出的命中率不同，这是有意的。

use std::collections::{BTreeMap, HashMap};

use dhampir_core::compose;
use dhampir_core::timeline::layer::{AssetTimebases, TimelineV2};
use dhampir_core::timeline::project::load_doc;
use dhampir_core::timeline::schema::Frame;
use dhampir_worker::pipeline::{PoolCursor, demand_of, request_schedule};

/// 扫一遍工程，把「这一段会怎么要帧」摊平。
fn schedule_of(timeline: &TimelineV2, assets: &AssetTimebases) -> Vec<(Frame, String, Frame)> {
    let from = compose::first_frame_v2(timeline).unwrap_or(0);
    let to = compose::end_frame_v2(timeline).unwrap_or(0).saturating_sub(1);
    if to < from {
        return Vec::new();
    }
    request_schedule(timeline, assets, from, to)
        .into_iter()
        .map(|row| (row.frame, row.source, row.source_frame))
        .collect()
}

/// 一个工程身上的数。
struct Census {
    project: String,
    frames: usize,
    requests: usize,
    /// 每个源要过哪些帧（去重）。
    distinct: BTreeMap<String, usize>,
    /// 要过的帧号比"这个源此前要过的最大帧号"还小的次数 —— 这就是回退。
    backward: usize,
    /// 每次回退退了多远（正数）。
    distances: Vec<Frame>,
}

fn census_of(project: &str, rows: &[(Frame, String, Frame)]) -> Census {
    let mut max_seen: HashMap<String, Frame> = HashMap::new();
    let mut seen: HashMap<String, std::collections::BTreeSet<Frame>> = HashMap::new();
    let mut backward = 0usize;
    let mut distances = Vec::new();
    let mut frames = std::collections::BTreeSet::new();

    for (frame, source, source_frame) in rows {
        frames.insert(*frame);
        seen.entry(source.clone())
            .or_default()
            .insert(*source_frame);
        let high = max_seen.get(source).copied();
        match high {
            Some(previous) if *source_frame < previous => {
                backward += 1;
                distances.push(previous - *source_frame);
            }
            _ => {}
        }
        let next = high.map_or(*source_frame, |previous| previous.max(*source_frame));
        max_seen.insert(source.clone(), next);
    }

    Census {
        project: project.to_string(),
        frames: frames.len(),
        requests: rows.len(),
        distinct: seen.into_iter().map(|(k, v)| (k, v.len())).collect(),
        backward,
        distances,
    }
}

/// 一个池子尺寸下的代价。
struct PoolCost {
    slots: usize,
    frames_read: usize,
    replays: usize,
    hits: usize,
}

/// 用**产品同一段规则**跑一遍池子。
///
/// `slots_for` 按源给槽数，因为产品就是这么算的（[`pool_slots`] 吃的是**源自己的**
/// 尺寸，不是输出尺寸）：扫描各档时给它一个常数，复现产品配置时给它
/// `pool_slots(asset.width, asset.height)`。
fn pool_cost(
    rows: &[(Frame, String, Frame)],
    slots_for: &dyn Fn(&str) -> usize,
) -> PoolCost {
    let requests: Vec<_> = rows
        .iter()
        .map(|(frame, source, source_frame)| dhampir_worker::pipeline::SourceRequest {
            frame: *frame,
            source: source.clone(),
            source_frame: *source_frame,
        })
        .collect();
    let demand = demand_of(&requests);
    let mut pools: HashMap<String, PoolCursor> = HashMap::new();
    for row in rows {
        let cursor = pools.entry(row.1.clone()).or_insert_with(|| {
            PoolCursor::new(slots_for(&row.1), demand.get(&row.1).cloned().unwrap_or_default())
        });
        let plan = cursor.plan(row.2);
        cursor.commit(&plan);
    }
    let mut total = PoolCost {
        slots: 0,
        frames_read: 0,
        replays: 0,
        hits: 0,
    };
    for (source, cursor) in &pools {
        let stats = cursor.stats();
        total.frames_read += stats.frames_read;
        total.replays += stats.replays;
        total.hits += stats.hits;
        // 一个工程里几个源槽数可能不同（1080p 与 4K 混用），取最大的那个当"这一档"。
        total.slots = total.slots.max(slots_for(source));
    }
    total
}

/// 分位数（最近秩法）。给不出就返回 None —— 空集上的"分位数"是编出来的。
fn quantile(sorted: &[Frame], ratio: f64) -> Option<Frame> {
    if sorted.is_empty() {
        return None;
    }
    let index = ((sorted.len() - 1) as f64 * ratio).round() as usize;
    sorted.get(index).copied()
}

fn histogram(distances: &[Frame]) -> BTreeMap<String, usize> {
    let mut buckets: BTreeMap<String, usize> = BTreeMap::new();
    for distance in distances {
        let key = match distance {
            d if *d <= 1 => "1".to_string(),
            d if *d <= 4 => "2-4".to_string(),
            d if *d <= 16 => "5-16".to_string(),
            d if *d <= 64 => "17-64".to_string(),
            d if *d <= 256 => "65-256".to_string(),
            _ => ">256".to_string(),
        };
        *buckets.entry(key).or_default() += 1;
    }
    buckets
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let projects: Vec<String> = if args.is_empty() {
        vec![
            "fixtures/sample-project.doc.json".to_string(),
            "fixtures/sample-subtitle.doc.json".to_string(),
        ]
    } else {
        args
    };

    let mut all: Vec<Census> = Vec::new();
    for project in &projects {
        let text = match std::fs::read_to_string(project) {
            Ok(text) => text,
            Err(error) => {
                eprintln!("读不了 {project}：{error}");
                std::process::exit(2);
            }
        };
        let doc = match load_doc(&text) {
            Ok(doc) => doc,
            Err(error) => {
                eprintln!("工程 {project} 载入失败：{error}");
                std::process::exit(2);
            }
        };
        let assets = doc.asset_timebases();
        let rows = schedule_of(&doc.timeline, &assets);
        let census = census_of(project, &rows);

        // **产品真的会用几槽**：按源声明里的尺寸算（与 `SourcePool::open` 同一条规则），
        // 缺尺寸就按 1080p 算 —— 这一档要能被复算，所以把每个源的槽数一并打印出来。
        let sizes: HashMap<String, (u32, u32)> = doc
            .assets
            .iter()
            .map(|asset| {
                (
                    asset.id.clone(),
                    (
                        asset.width.unwrap_or(1920),
                        asset.height.unwrap_or(1080),
                    ),
                )
            })
            .collect();
        let product_slots = |source: &str| {
            let (width, height) = sizes.get(source).copied().unwrap_or((1920, 1080));
            dhampir_worker::pipeline::pool_slots(width, height)
        };

        let mut distances = census.distances.clone();
        distances.sort_unstable();
        let costs: Vec<PoolCost> = [1usize, 2, 4, 8, 16, 32, 64]
            .into_iter()
            .map(|slots| pool_cost(&rows, &|_| slots))
            .collect();
        let product = pool_cost(&rows, &product_slots);
        let distinct_total: usize = census.distinct.values().sum();

        println!(
            "{}",
            serde_json::json!({
                "project": census.project,
                "frames": census.frames,
                "requests": census.requests,
                "distinct_source_frames": distinct_total,
                "sources": census.distinct,
                "backward": census.backward,
                "backward_share": if census.requests == 0 { 0.0 } else { census.backward as f64 / census.requests as f64 },
                "distance": {
                    "max": distances.last().copied(),
                    "p50": quantile(&distances, 0.5),
                    "p90": quantile(&distances, 0.9),
                    "p99": quantile(&distances, 0.99),
                    "histogram": histogram(&distances),
                },
                // **这一档才是产品的实际配置**（槽数按源尺寸算），上面那条扫描是参数研究。
                "product": {
                    "slots_per_source": sizes.iter().map(|(id, (width, height))| {
                        (id.clone(), dhampir_worker::pipeline::pool_slots(*width, *height))
                    }).collect::<BTreeMap<String, usize>>(),
                    "frames_read": product.frames_read,
                    "replays": product.replays,
                    "hits": product.hits,
                    "read_ratio": if distinct_total == 0 { 0.0 } else { product.frames_read as f64 / distinct_total as f64 },
                },
                "pool": costs.iter().map(|cost| serde_json::json!({
                    "slots": cost.slots,
                    "frames_read": cost.frames_read,
                    "replays": cost.replays,
                    "hits": cost.hits,
                    "read_ratio": if distinct_total == 0 { 0.0 } else { cost.frames_read as f64 / distinct_total as f64 },
                })).collect::<Vec<_>>(),
            })
        );
        all.push(census);
    }

    let requests: usize = all.iter().map(|c| c.requests).sum();
    let backward: usize = all.iter().map(|c| c.backward).sum();
    let mut distances: Vec<Frame> = all.iter().flat_map(|c| c.distances.clone()).collect();
    distances.sort_unstable();
    println!(
        "{}",
        serde_json::json!({
            "event": "total",
            "projects": all.len(),
            "requests": requests,
            "backward": backward,
            "backward_share": if requests == 0 { 0.0 } else { backward as f64 / requests as f64 },
            "distance_max": distances.last().copied(),
            "distance_p50": quantile(&distances, 0.5),
            "distance_p90": quantile(&distances, 0.9),
        })
    );
}
