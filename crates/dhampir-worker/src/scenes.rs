//! M1 corpus 的 **native 宿主侧**：解析命令行取值 → 交给 core 跑 → 落盘 → 跨进程比对。
//!
//! 这里**不发明场景**：名字、尺寸、清屏色、入口、采样点全部来自 [`SELECTABLE_SCENES`]——
//! 记录里出现的每个坐标都能在 core 里查到出处。M0 已经因为"记录写一套坐标、断言查
//! 另一套"吃过一次亏，这里不重演。
//!
//! # 这个模块剩下的三件事
//!
//! M2 起，驱动与记录都住进了 [`dhampir_core::render::corpus`]，本模块只剩：
//!
//! 1. **解析命令行取值**（[`parse_selection`] / [`parse_frames`]）——只有 CLI 才需要；
//! 2. **落盘**（[`write_frames`]）——浏览器宿主没有文件系统，它把字节交给 JS；
//! 3. **跨进程比对**（[`compare_runs`]）——这是 `--compare-run` 这个 CLI 开关的服务对象。
//!
//! 搬走的每一个东西都带着同一个理由：**它必须只有一份实现**。两端各写一遍驱动代码，
//! M2 那句"同一帧在两个运行时里画出同样的字节"就退化成"两份驱动大致相当"；
//! 两端各写一遍记录的形状，比出来的就不只是渲染差异了。
//!
//! # 一帧画两遍
//!
//! 每一帧**渲染两次**再比字节，这件事在 core
//! （[`dhampir_core::render::corpus::render_frame_pair`]）。两次不一致时**照记不误**——
//! 那是一个发现（驱动？后端？提交顺序？），不是失败，更不该用颜色断言把它盖过去：
//! 判定在这种帧上直接留空（`null`），因为"没验"和"验过通过"是两件不同的事。
//!
//! # 尺寸
//!
//! corpus 一律按 [`SceneSpec::size`]（256×256）渲染：采样点的坐标是按这个尺寸定的，
//! `expected_bytes` 也只在这个尺寸上成立。1080p 的计时用的是
//! [`dhampir_core::render::SceneRenderer::new_at`]，在 [`crate::baseline`] 里——
//! 两处的尺寸是**两个不同的问题**，不要为了让记录好看而把它们统一。

use std::path::{Path, PathBuf};

use dhampir_core::gpu::GpuContext;
use dhampir_core::render::corpus;
use dhampir_core::render::{SELECTABLE_SCENES, SceneSpec, scene_by_name};

// 搬到 core 的那些类型与函数在这里**原样再导出一次**：本模块的调用方与测试不用改一个字，
// 而"东西住在哪"这件事由 core 决定。M2 的浏览器宿主导的是同一批名字。
pub use dhampir_core::render::corpus::{Counts, PointReading, SceneFrame, SceneRun, frame_rel_path};
pub use dhampir_core::render::{leg_json, report_text};


/// 跑满一个整周期需要的帧数：`gradient` 的平移周期是 16 帧，`checker` / `srgb_linear`
/// / `alpha_stack` 的周期是 3 / 8 / 4——**都整除 16**。
///
/// 所以 `--frames 0..16` 不是"随便挑个整数"，而是"每个随帧变化的场景都被走完了一整圈"。
/// 少一帧就会漏掉一个相位，而那正是这一类场景唯一要考的东西。
pub const FULL_PERIOD_FRAMES: u32 = 16;

/// `--scene` 支持的最大帧数。
///
/// 一个笔误就能写出 `--frames 0..100000`：那时你不是在跑记录，是在填满磁盘。
/// 上限只拦笔误，不拦需求——真的要看更多帧，把上限改掉，顺便想清楚为什么要看。
pub const MAX_FRAMES: u32 = 1024;

// ---------------------------------------------------------------------------
// 选场景
// ---------------------------------------------------------------------------

/// `--scene` 的取值。
#[derive(Clone, Copy, Debug)]
pub enum SceneSelection {
    /// 注册表里的全部场景，按注册顺序。
    All,
    /// 一个具体场景。
    One(&'static SceneSpec),
}

/// **按场景名**比较，不是按指针。
///
/// `SceneSpec` 里有一堆 `wgpu::Color` / 函数指针式的字段，derive 不出 `Eq`；而
/// "两个选择是不是同一个选择"这个问题，答案只取决于**选的是哪个场景**。
/// 名字在注册表里唯一（否则 `--scene` 本身就有歧义），所以拿名字比是准的。
impl PartialEq for SceneSelection {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::All, Self::All) => true,
            (Self::One(a), Self::One(b)) => a.name == b.name,
            _ => false,
        }
    }
}

impl Eq for SceneSelection {}

impl SceneSelection {
    /// 展开成要渲染的场景列表。`All` 的顺序就是 [`SELECTABLE_SCENES`] 的顺序——
    /// 记录里的顺序因此是稳定的，跨版本 `diff` 不会因为遍历顺序而抖动。
    pub fn specs(self) -> Vec<&'static SceneSpec> {
        match self {
            Self::All => SELECTABLE_SCENES.iter().collect(),
            Self::One(spec) => vec![spec],
        }
    }
}

/// 解析 `--scene` 的取值。**纯函数**，所以能单测——命令行的错值必须在碰 GPU 之前就被拒。
pub fn parse_selection(value: &str) -> Result<SceneSelection, String> {
    if value == "all" {
        return Ok(SceneSelection::All);
    }
    match scene_by_name(value) {
        Some(spec) => Ok(SceneSelection::One(spec)),
        None => Err(format!(
            "不认识的场景：{value}。可用：all、{}",
            SELECTABLE_SCENES
                .iter()
                .map(|s| s.name)
                .collect::<Vec<_>>()
                .join("、")
        )),
    }
}

// ---------------------------------------------------------------------------
// 选帧区间
// ---------------------------------------------------------------------------

/// 解析 `--frames` 的取值：`0..16`（**半开**，与 Rust 的区间写法一致）。
///
/// 也接受一个光杆数字：`5` 就是 `5..6`。
///
/// **纯函数**，所以写错的区间在碰 GPU 之前就被拒。三条刻意收紧的规矩：
///
/// - 不认 `..=`。（`0..=15` 与 `0..16` 都是 16 帧，两套写法并存的结果就是有人
///   把 `..=` 当成 `..` 用，然后在记录里多跑或少跑一帧而没人发现。）
/// - 空区间（`16..16`）与反着写（`9..3`）都报错，不"跑出零帧然后报全绿"。
/// - 帧数上限（[`MAX_FRAMES`]）按**要跑几帧**算，不按终点算：`1000..2024` 是
///   1024 帧，合规；`1000..2025` 不是。
pub fn parse_frames(value: &str) -> Result<(u32, u32), String> {
    if value.contains("..=") {
        return Err(format!(
            "--frames 用半开区间（`0..16` 含 0 不含 16），不认 `..=`：{value}"
        ));
    }

    let (start, end) = match value.split_once("..") {
        Some((start, end)) => (parse_frame_index(start, value)?, parse_frame_index(end, value)?),
        None => {
            let only = parse_frame_index(value, value)?;
            let end = only
                .checked_add(1)
                .ok_or_else(|| format!("--frames 的帧号太大，加一就溢出了：{value}"))?;
            (only, end)
        }
    };

    if end <= start {
        return Err(format!(
            "--frames 的区间是空的：{value}（终点要大于起点，半开区间）"
        ));
    }

    let count = end - start;
    if count > MAX_FRAMES {
        return Err(format!(
            "--frames 要跑 {count} 帧，超过上限 {MAX_FRAMES}——这看着像笔误。\
             真要跑这么多帧，先改 MAX_FRAMES，顺便想清楚为什么要看这么多。"
        ));
    }

    Ok((start, end))
}

/// 解析一个帧号。**只认十进制数字**：`0x10`、`+3`、` 5`、`-1` 一律拒——
/// 否则记录里的帧号可能来自一个没人想过的写法。
fn parse_frame_index(text: &str, whole: &str) -> Result<u32, String> {
    if text.is_empty() {
        return Err(format!("--frames 的区间缺了一头：{whole}"));
    }
    if !text.chars().all(|c| c.is_ascii_digit()) {
        return Err(format!("--frames 里只认十进制数字与 `..`：{whole}"));
    }
    text.parse::<u32>()
        .map_err(|e| format!("--frames 里的帧号读不出来：{whole}（{e}）"))
}

// ---------------------------------------------------------------------------
// 渲染：宿主侧只剩薄薄的一层
// ---------------------------------------------------------------------------

/// 跑一组场景 × 一个帧区间，出图、读回、逐点判定。
/// **实现搬去了 [`dhampir_core::render::corpus::render_run`]**。
///
/// 本模块只负责把它变成同步的：native 宿主用 `pollster` 把 future 跑完，浏览器宿主
/// 编译的是同一份 core 代码、只是直接 `.await`。这一层薄得可以忽略，换来的是
/// "两端的调用形状完全相同"。
///
/// 管线与 uniform **每场景建一次**（不是每帧一次）：这才是真实运行的样子
/// （M4 导出也是一个场景连续出多帧），而且"换帧不需要重建管线"这件事因此被真的走到。
/// 这条纪律也在 core 里——宿主不该有机会把它写错。
pub fn run_scenes(
    ctx: &GpuContext,
    specs: &[&'static SceneSpec],
    frames: (u32, u32),
) -> Result<SceneRun, Box<dyn std::error::Error>> {
    Ok(pollster::block_on(corpus::render_run(ctx, specs, frames))?)
}


// ---------------------------------------------------------------------------
// 写记录
// ---------------------------------------------------------------------------

/// 把全部帧写成 PNG，返回写出的文件（含目录，供调用方打进记录）。
pub fn write_frames(dir: &Path, run: &SceneRun) -> Result<Vec<PathBuf>, std::io::Error> {
    let frames_dir = dir.join("frames");
    std::fs::create_dir_all(&frames_dir)?;
    let mut written = Vec::with_capacity(run.frames.len());
    for frame in &run.frames {
        let path = dir.join(frame_rel_path(frame.spec.name, frame.frame));
        std::fs::write(&path, &frame.png)?;
        written.push(path);
    }
    Ok(written)
}

// ---------------------------------------------------------------------------
// 跨进程比对
// ---------------------------------------------------------------------------

/// 把两次运行的 `run.json` 对上。**纯函数**：两个 JSON 进、一个 JSON 出，
/// 所以"比对会不会漏掉一帧"这件事本身能被单测钉住。
///
/// 四条硬要求：
///
/// 1. **每个后端都要出现**，包括"对方没跑这个后端"和"本次没跑对方跑了的那个"。
///    静默跳过是最坏的一种比对：报告会显示 `identical: true`，把"只比了两个后端里
///    的一个"说成"全一致"。
/// 2. **逐帧比**，不只比那个总的摘要：总摘要不一致时要知道是哪几帧。
/// 3. 帧的缺口要**两边都报**（`missing_in_other` / `missing_in_current`）。只报一侧的
///    话，"对方多出来的帧"的唯一痕迹是 `matched_frames != other_frame_count`——
///    而记录里没有任何字段指认**是哪几帧**，等于让人去猜。
/// 4. 像素与 PNG **分开比**：像素不同是渲染不同，PNG 不同是编码不同，两者的归因
///    方向完全不一样，合成一个布尔值等于把线索丢掉。
pub fn compare_runs(other: &serde_json::Value, current: &serde_json::Value) -> serde_json::Value {
    let no_backends = Vec::new();
    let other_backends = backends_of(other).unwrap_or(&no_backends);
    let current_backends = backends_of(current).unwrap_or(&no_backends);

    // 后端取**并集**：本侧的在前（那是这次运行真的跑了的东西），只在对方出现的在后。
    let mut requested_names: Vec<String> = Vec::new();
    for backend in current_backends.iter().chain(other_backends.iter()) {
        let name = requested_of(backend).to_string();
        if !requested_names.contains(&name) {
            requested_names.push(name);
        }
    }

    let mut entries = Vec::new();
    let mut all_identical = true;
    let mut compared = 0_usize;
    let mut not_compared = 0_usize;

    for requested in &requested_names {
        let mine = find_backend(current_backends, requested);
        let twin = find_backend(other_backends, requested);

        let entry = match (mine, twin) {
            (Some(mine), Some(twin)) => {
                compared += 1;
                compare_backend(requested, mine, twin)
            }
            (Some(mine), None) => {
                not_compared += 1;
                serde_json::json!({
                    "requested": requested,
                    "adapter_name": adapter_name(mine),
                    "other_adapter_name": serde_json::Value::Null,
                    "frames_digest": mine.get("frames_digest").cloned(),
                    "other_frames_digest": serde_json::Value::Null,
                    "identical": false,
                    "frame_count": frame_count(mine),
                    "other_frame_count": serde_json::Value::Null,
                    "matched_frames": serde_json::Value::Null,
                    "pixel_mismatches": [],
                    "png_mismatches": [],
                    "missing_in_other": frame_labels(mine),
                    "missing_in_current": [],
                    "note": "对方那份 run.json 里没有这个后端——**没比**，不是比过了",
                })
            }
            // 镜像的那一半。上一支少了它，"被比的那份跑了个本次没跑的后端"就会
            // 从报告里消失——而那正是"回到旧机器上重跑一遍"这种场景。
            (None, Some(twin)) => {
                not_compared += 1;
                serde_json::json!({
                    "requested": requested,
                    "adapter_name": serde_json::Value::Null,
                    "other_adapter_name": adapter_name(twin),
                    "frames_digest": serde_json::Value::Null,
                    "other_frames_digest": twin.get("frames_digest").cloned(),
                    "identical": false,
                    "frame_count": serde_json::Value::Null,
                    "other_frame_count": frame_count(twin),
                    "matched_frames": serde_json::Value::Null,
                    "pixel_mismatches": [],
                    "png_mismatches": [],
                    "missing_in_other": [],
                    "missing_in_current": frame_labels(twin),
                    "note": "本次运行没有跑这个后端（对方跑了）——**没比**，不是比过了",
                })
            }
            (None, None) => unreachable!("并集里的名字至少来自一侧"),
        };

        // 三态收敛成布尔：这里要的是"有没有一条**比过且一致**"。
        all_identical &= entry.get("identical").and_then(|v| v.as_bool()) == Some(true);
        entries.push(entry);
    }

    serde_json::json!({
        "schema": 1,
        "identical": all_identical && !entries.is_empty(),
        "backends_compared": compared,
        "backends_not_compared": not_compared,
        "backends": entries,
        "note": "本文件由第二个进程写出（`--compare-run`）：它与被比的那份 `run.json` 是两次独立运行。\
                 逐帧比的是像素摘要（渲染结果）与 PNG 摘要（文件字节）两样——\
                 退出标准说的是「重复运行逐字节一致」，那指的是文件。",
    })
}

/// 比一个后端的两次运行：逐帧比像素摘要与 PNG 摘要两样。
fn compare_backend(
    requested: &str,
    mine: &serde_json::Value,
    twin: &serde_json::Value,
) -> serde_json::Value {
    let current_digest = mine.get("frames_digest").cloned();
    let other_digest = twin.get("frames_digest").cloned();
    let current_frames = frames_of(mine);
    let other_frames = frames_of(twin);

    let mut pixel_mismatches = Vec::new();
    let mut png_mismatches = Vec::new();
    let mut missing_in_other = Vec::new();
    let mut matched = 0_usize;

    for frame in &current_frames {
        let label = frame_label(frame);
        let twin_frame = other_frames.iter().find(|f| frame_label(f) == label);
        let Some(twin_frame) = twin_frame else {
            missing_in_other.push(label);
            continue;
        };
        matched += 1;
        if frame.get("pixel_digest") != twin_frame.get("pixel_digest") {
            pixel_mismatches.push(label.clone());
        }
        if frame.get("png_digest") != twin_frame.get("png_digest") {
            png_mismatches.push(label);
        }
    }

    let missing_in_current: Vec<String> = other_frames
        .iter()
        .filter(|f| !current_frames.iter().any(|c| frame_label(c) == frame_label(f)))
        .map(frame_label)
        .collect();

    let identical = current_digest == other_digest
        && pixel_mismatches.is_empty()
        && png_mismatches.is_empty()
        && missing_in_other.is_empty()
        && missing_in_current.is_empty()
        // 冗余，但留着：上面那几条都是"列表非空"，这一条是"数目对上了"——
        // 对方一份记录里有重复帧名时，只有它还拦得住。
        && matched == other_frames.len();

    serde_json::json!({
        "requested": requested,
        "adapter_name": adapter_name(mine),
        "other_adapter_name": adapter_name(twin),
        "frames_digest": current_digest,
        "other_frames_digest": other_digest,
        "identical": identical,
        "frame_count": current_frames.len(),
        "other_frame_count": other_frames.len(),
        "matched_frames": matched,
        "pixel_mismatches": pixel_mismatches,
        "png_mismatches": png_mismatches,
        "missing_in_other": missing_in_other,
        "missing_in_current": missing_in_current,
        "note": if identical { serde_json::Value::Null } else {
            serde_json::Value::from("不一致——这是一个**发现**：先看像素还是先看 PNG 不同，两者归因方向不同".to_string())
        },
    })
}

/// `run.json` 里的 `backends` 数组。没有这个键时返回 `None`——调用方拿它当空表用，
/// 但"没有后端"和"后端列表是空的"在 `identical` 那一栏的算法里是一回事：
/// **空集合永不算一致**。
fn backends_of(run: &serde_json::Value) -> Option<&Vec<serde_json::Value>> {
    run.get("backends").and_then(|v| v.as_array())
}

/// 一条后端记录自报的后端名。
fn requested_of(backend: &serde_json::Value) -> &str {
    backend
        .get("requested")
        .and_then(|v| v.as_str())
        // 名字缺失不该让整条记录静默消失：给它一个能被看见的名字，让它出现在报告里。
        .unwrap_or("<未知后端>")
}

/// 在 `backends` 里找某个后端的记录。
fn find_backend<'a>(
    backends: &'a [serde_json::Value],
    requested: &str,
) -> Option<&'a serde_json::Value> {
    backends.iter().find(|b| requested_of(b) == requested)
}

/// 一份记录里的帧列表。缺这个键时给空表：下面每个循环都是"按本侧有的帧去比"，
/// 空表会让"一帧都没比过"这件事如实反映在 `matched_frames` 与两个缺口列表上。
fn frames_of(backend: &serde_json::Value) -> Vec<serde_json::Value> {
    backend
        .get("frames")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default()
}

/// 一份记录里的帧数。**缺 `frames` 键时是 `None` 而不是 0**：记录里"没这项"和
/// "这项是空的"是两件不同的事。
fn frame_count(backend: &serde_json::Value) -> Option<usize> {
    backend
        .get("frames")
        .and_then(|v| v.as_array())
        .map(Vec::len)
}

/// 一份记录里所有帧的名字。
fn frame_labels(backend: &serde_json::Value) -> Vec<String> {
    frames_of(backend).iter().map(frame_label).collect()
}

/// 取一个后端条目里的 adapter 名。记录里没有就写 `null`，不编一个。
fn adapter_name(backend: &serde_json::Value) -> serde_json::Value {
    backend
        .get("adapter_name")
        .cloned()
        .unwrap_or(serde_json::Value::Null)
}

/// 帧在比对里的身份：`场景 + 帧号`。用两个字段拼，而不是用文件路径——
/// 路径会被目录结构影响，而"哪一帧"不该跟着目录变。
fn frame_label(frame: &serde_json::Value) -> String {
    let scene = frame.get("scene").and_then(|v| v.as_str()).unwrap_or("?");
    let index = frame.get("frame").and_then(|v| v.as_u64()).unwrap_or(u64::MAX);
    format!("{scene} f{index:03}")
}

#[cfg(test)]
mod tests {
    use super::*;
    // 注册表的名字来自 core：本模块的测试只拿它当"合法的场景名"用。
    use dhampir_core::render::scene_names;

    #[test]
    fn selection_parses_all_and_named_scenes() {
        assert_eq!(parse_selection("all").unwrap(), SceneSelection::All);
        for name in scene_names() {
            let selection = parse_selection(name).expect("注册表里的名字必须都能选");
            let specs = selection.specs();
            assert_eq!(specs.len(), 1);
            assert_eq!(specs[0].name, name);
        }

        // 错值必须在**碰 GPU 之前**被拒，而且要把可选值说出来——只说"不认识"的话，
        // 使用者的下一步是去读源码。
        let reason = parse_selection("gradientt").unwrap_err();
        assert!(reason.contains("不认识的场景"), "{reason}");
        assert!(reason.contains("gradient"), "{reason}");
        assert!(reason.contains("all"), "{reason}");
    }

    #[test]
    fn all_selection_keeps_registry_order() {
        let names: Vec<&str> = SceneSelection::All
            .specs()
            .iter()
            .map(|s| s.name)
            .collect();
        assert_eq!(names, scene_names());
    }

    /// `--frames` 只认半开区间这一种写法，而且不许"跑出零帧还报绿"。
    #[test]
    fn frame_ranges_parse_only_the_half_open_form() {
        assert_eq!(parse_frames("0..16"), Ok((0, 16)));
        assert_eq!(parse_frames("0..1"), Ok((0, 1)));
        // 一个光杆数字 = 单独那一帧。
        assert_eq!(parse_frames("5"), Ok((5, 6)));
        assert_eq!(parse_frames("0"), Ok((0, 1)));

        // 空区间 / 反着写 / 缺一头 / 两种"看着像"的写法：全部要报错。
        for bad in [
            "", "16..16", "9..3", "..8", "3..", "a..b", "0..=15", "0 .. 16", "-1..2", "0x2..4",
            "5,6",
        ] {
            assert!(parse_frames(bad).is_err(), "{bad:?} 不该被接受");
        }
        // 上限按**要跑几帧**算，不按终点算。
        assert!(parse_frames("1000..2024").is_ok(), "正好 {MAX_FRAMES} 帧应当合规");
        assert!(parse_frames("1000..2025").is_err(), "多一帧就该被拦下");
        assert!(parse_frames("0..4294967295").is_err(), "端点顶到 u32 上限也要报错而不是回绕");
    }

    // ---- 跨进程比对 ------------------------------------------------------

    fn backend_json(requested: &str, frames: &[(&str, u32, &str, &str)]) -> serde_json::Value {
        let frames: Vec<serde_json::Value> = frames
            .iter()
            .map(|(scene, frame, pixel, png)| {
                serde_json::json!({
                    "scene": scene,
                    "frame": frame,
                    "pixel_digest": pixel,
                    "png_digest": png,
                })
            })
            .collect();
        serde_json::json!({
            "requested": requested,
            "adapter_name": "测试 adapter",
            "frames": frames,
            "frames_digest": format!("{:016x}", frames.len() as u64),
        })
    }

    fn run_json(backends: Vec<serde_json::Value>) -> serde_json::Value {
        serde_json::json!({ "backends": backends })
    }

    #[test]
    fn identical_runs_compare_identical() {
        let frames = [("gradient", 0, "aa", "bb"), ("gradient", 1, "cc", "dd")];
        let a = run_json(vec![backend_json("DX12", &frames)]);
        let b = run_json(vec![backend_json("DX12", &frames)]);
        let report = compare_runs(&a, &b);
        assert_eq!(report["identical"], true);
        assert_eq!(report["backends"][0]["identical"], true);
        assert_eq!(report["backends"][0]["matched_frames"], 2);
        assert_eq!(report["backends_compared"], 1);
        assert_eq!(report["backends_not_compared"], 0);
        assert_eq!(
            report["backends"][0]["missing_in_current"],
            serde_json::json!([])
        );
        // 一致的时候不该有 note——`null` 才不会被人读成"有不一致但没说清"。
        assert_eq!(report["backends"][0]["note"], serde_json::Value::Null);
    }

    /// 像素不同与 PNG 不同要**分开报**：前者是渲染，后者是编码，归因方向不同。
    #[test]
    fn pixel_and_png_mismatches_are_reported_separately() {
        let a = run_json(vec![backend_json(
            "DX12",
            &[("gradient", 0, "aa", "bb"), ("gradient", 1, "cc", "dd")],
        )]);
        let b = run_json(vec![backend_json(
            "DX12",
            &[("gradient", 0, "aa", "ff"), ("gradient", 1, "ee", "dd")],
        )]);
        let report = compare_runs(&a, &b);
        assert_eq!(report["identical"], false);
        assert_eq!(
            report["backends"][0]["pixel_mismatches"],
            serde_json::json!(["gradient f001"])
        );
        assert_eq!(
            report["backends"][0]["png_mismatches"],
            serde_json::json!(["gradient f000"])
        );
    }

    /// 对方少了一帧 = **没比**。不能因为"另一侧没有"就当它们相同。
    ///
    /// 参数顺序是 `(other, current)`，`other` 是**被比的那份记录**——第一版测试把
    /// 两个参数传反了，于是它验的是另一件事，还"通过"了。
    #[test]
    fn a_missing_frame_is_not_a_match() {
        let mine = run_json(vec![backend_json("DX12", &[("gradient", 0, "aa", "bb")])]);
        let theirs = run_json(vec![backend_json("DX12", &[])]);
        let report = compare_runs(&theirs, &mine);
        assert_eq!(report["identical"], false);
        assert_eq!(
            report["backends"][0]["missing_in_other"],
            serde_json::json!(["gradient f000"])
        );
        assert_eq!(
            report["backends"][0]["missing_in_current"],
            serde_json::json!([])
        );
        assert_eq!(report["backends"][0]["matched_frames"], 0);
        assert_eq!(report["backends_compared"], 1);
        assert_eq!(report["backends_not_compared"], 0);
    }

    /// 反过来那一半：**对方有、本次没有**的帧也要指名道姓。
    ///
    /// 在这次改动之前这种缺口是看不见的——唯一的痕迹是
    /// `matched_frames != other_frame_count`，而"是哪几帧"记录里没有。
    #[test]
    fn a_frame_only_the_other_side_has_is_reported_too() {
        let mine = run_json(vec![backend_json("DX12", &[("gradient", 0, "aa", "bb")])]);
        let theirs = run_json(vec![backend_json(
            "DX12",
            &[("gradient", 0, "aa", "bb"), ("gradient", 1, "cc", "dd")],
        )]);
        let report = compare_runs(&theirs, &mine);
        assert_eq!(report["identical"], false);
        assert_eq!(
            report["backends"][0]["missing_in_other"],
            serde_json::json!([])
        );
        assert_eq!(
            report["backends"][0]["missing_in_current"],
            serde_json::json!(["gradient f001"])
        );
        assert_eq!(report["backends"][0]["matched_frames"], 1);
        assert_eq!(report["backends"][0]["other_frame_count"], 2);
    }

    /// 一边没跑的后端要**出现**在报告里并说明"没比"——两个方向都要。
    ///
    /// 只按本侧遍历的话，第二个方向会静默消失，而那时报告会显示
    /// `identical: true`：一次"只比了两个后端里的一个"的运行会被记成"全一致"。
    #[test]
    fn a_missing_backend_is_reported_as_not_compared() {
        let mine = run_json(vec![backend_json("DX12", &[("gradient", 0, "aa", "bb")])]);
        let theirs = run_json(vec![backend_json("VULKAN", &[("gradient", 0, "aa", "bb")])]);
        let report = compare_runs(&theirs, &mine);

        assert_eq!(report["identical"], false);
        assert_eq!(report["backends_compared"], 0);
        assert_eq!(report["backends_not_compared"], 2);

        // 本侧跑了、对方没有 → 排在前面（本侧的在前）。
        assert_eq!(report["backends"][0]["requested"], "DX12");
        assert_eq!(
            report["backends"][0]["other_adapter_name"],
            serde_json::Value::Null
        );
        assert!(
            report["backends"][0]["note"]
                .as_str()
                .unwrap()
                .contains("没比"),
            "{report}"
        );

        // 对方跑了、本侧没有 → 也要有一条，而且要把"是哪一侧缺的"说清楚。
        assert_eq!(report["backends"][1]["requested"], "VULKAN");
        assert_eq!(
            report["backends"][1]["adapter_name"],
            serde_json::Value::Null
        );
        assert_eq!(report["backends"][1]["other_adapter_name"], "测试 adapter");
        assert!(
            report["backends"][1]["note"]
                .as_str()
                .unwrap()
                .contains("本次运行没有跑"),
            "{report}"
        );
        assert_eq!(
            report["backends"][1]["missing_in_current"],
            serde_json::json!(["gradient f000"])
        );
    }

    /// 空的当前记录（比如 `--scene` 没给）不该被当成"全部一致"。
    #[test]
    fn an_empty_current_run_is_never_identical() {
        let report = compare_runs(&run_json(vec![]), &run_json(vec![]));
        assert_eq!(report["identical"], false);
    }
}
