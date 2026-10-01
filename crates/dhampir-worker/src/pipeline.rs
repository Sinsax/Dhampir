//! 后端出片管道：**逐源顺序解码 -> 上传 -> 工程求值 -> 合成 -> 读回 -> 编码**。
//!
//! # 它在补哪一个洞
//!
//! 这个仓库此前有两条各自成立、但**合不起来**的东西：
//!
//! * examples/decode_sequence.rs：顺序解码管道是真的，但它的 SequenceSource
//!   **对任何 source 都返回同一张纹理** —— 样本工程有四个 source，它只会画同一路流；
//! * examples/render_project.rs：能按工程求值多源多层，但源像素是
//!   synthetic_source_rgba8 合成的，**不接解码器**。
//!
//! 于是「后端渲染导出」这条路实际上不能用。这个模块把两者合成一条流：
//! **每个 asset 一路 ffmpeg 顺序解码器，按工程求值出的帧号把对应帧上传上去。**
//!
//! # 硬约束：只许顺序，不许逐帧 seek
//!
//! plan 把这条列为硬约束，理由是量化的：逐帧 seek 每帧都要回到关键帧重解，
//! 比顺序解码慢一个量级。守卫 scripts/check-sequential-decode.mjs 按文件检查
//! ffmpeg 调用附近**必须**同时出现 rawvideo 与 out_color_matrix。
//!
//! 而"只许向前"是这条约束的**直接推论**，它不是一条独立的限制：
//!
//! > 每一路源只能向前推进。工程如果要求某个 source 回退到一个已经读过去的帧
//! > （例如同一素材的两个片段在时间线上前后颠倒），这一路**给不出那一帧** ——
//! > 除非把解码器**从头再来一遍**。
//!
//! 所以回退给得出来，只是**要再读一遍**。这里不是"报错让整次出片失败"，
//! 而是：池子里有就直接给；没有就重启这一路解码器，从头读到目标帧（沿途把
//! 「这一趟还会再被要」的帧留在池子里，于是下一次回退多半能命中）。
//! 代价随目标帧号线性增长，所以**先量化回退有多常见**（T5.1，见 plan/measurements.md），
//! 再据实定池子多大。
//!
//! # 池子：键是 (source, 源内帧)
//!
//! 池子按 **(source, 源内帧)** 键复用纹理。于是同一输出帧里同一个 source 要
//! **两个不同源内帧**（同素材画中画、同素材转场）也能出片 —— 此前那是
//! source_frame_conflict，因为"一路源一张纹理"。
//!
//! 池子的容量按字节封顶（见 [`pool_slots`]），**只留这一趟真的还会被要的帧**
//! （[`demand_of`]）：顺序出片时中间帧永远不会被要第二次，传上去就是白传。
//!
//! # 音轨
//!
//! **不渲染。** 本模块只出视频。有音轨就在 stderr 明说，不给一份「看起来很成功」的哑片。


use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ffi::OsString;
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::time::Instant;

use dhampir_core::compose;
use dhampir_core::gpu::NATIVE_BACKENDS;
use dhampir_core::overlay::{SubtitleTable, evaluate_overlay};
use dhampir_core::readback;
use dhampir_core::render::{AnimationTextures, RenderSpace, SourceResolver, TimelineRenderer};
use dhampir_core::timeline::layer::{AssetTimebases, TimelineV2, seconds_at_sequence_frame};
use dhampir_core::timeline::schema::{Frame, Issue, TimebaseDto};
use dhampir_core::wgpu;

use crate::audio::{AUDIO_CHANNELS, AUDIO_PCM_FORMAT, AUDIO_SAMPLE_RATE, AudioPlan, AudioSegment};
use crate::baseline::open_leg;
use crate::text_overlay::{OverlayPainter, OverlayStats};

/// 解码器吐出来的像素格式。**两条路与两个宿主都用它**，别在这里换格式：
/// 换格式等于给「两端同一个渲染图」这句话加一个未验证的转换。
pub const DECODE_PIXEL_FORMAT: &str = "rgba";

/// 渲染目标与源纹理的格式。与 examples/decode_sequence.rs 保持一致。
pub const WORK_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

// ---------------------------------------------------------------------------
// 纯逻辑：先写这些，因为它们能被自检与真跑同时走到
// ---------------------------------------------------------------------------

/// 一趟出片会按什么顺序要哪些 (源, 源内帧)。
///
/// **它是纯的**，因为三件事都要它：
///
/// * 池子靠它知道「读到的这一帧以后还会不会被要」（不在需求里的帧直接丢掉，省一次上传）；
/// * 量化脚本靠它数「回退有多常见、退多远」（T5.1 的闸门：没有数字就不许定池子多大）；
/// * 判定靠它把「要过哪些帧」变成可复算的事实，而不是从渲染过程中去猜。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceRequest {
    /// 第几个**输出帧**要的。
    pub frame: Frame,
    pub source: String,
    pub source_frame: Frame,
}

/// 走一遍求值层，把这一段的取帧顺序摊平。
///
/// 顺序**就是渲染器真会请求的顺序**（轨道序即层序，层序即 texture_for 的调用序）——
/// 这一点很重要：如果这里换一个顺序，量化出来的回退次数就不是产品跑出来的那个数。
pub fn request_schedule(
    timeline: &TimelineV2,
    assets: &AssetTimebases,
    from: Frame,
    to: Frame,
) -> Vec<SourceRequest> {
    let mut rows = Vec::new();
    for frame in from..=to {
        let composite = compose::evaluate_v2_with_assets(timeline, frame, Some(assets));
        for layer in &composite.layers {
            // 调整图层没有素材（空串是如实的表达，不是占位符）。
            if layer.source.is_empty() {
                continue;
            }
            rows.push(SourceRequest {
                frame,
                source: layer.source.clone(),
                source_frame: layer.source_frame,
            });
        }
    }
    rows
}

/// 每个源这一趟会要哪些源内帧、**各要几次**。
///
/// 池子只留"还有下一次"的帧（见 [`plan_fetch`]），所以这里要的是**次数**不是集合：
/// 顺序出片时每一帧都只被要一次，用完就该扔 —— 把它留着既不给谁用，又白占内存。
/// 这一条是"顺序出片的内存开销与改之前一样"的保证。
pub fn demand_of(schedule: &[SourceRequest]) -> HashMap<String, BTreeMap<Frame, usize>> {
    let mut demand: HashMap<String, BTreeMap<Frame, usize>> = HashMap::new();
    for row in schedule {
        *demand
            .entry(row.source.clone())
            .or_default()
            .entry(row.source_frame)
            .or_insert(0) += 1;
    }
    demand
}

/// 池子一次取帧要做什么。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FetchAction {
    /// 池里已经有这一帧。
    Hit,
    /// 顺着游标往前读。
    Forward,
    /// 要的帧在游标**后面**（已经读过去了）—— 重启这一路解码器再往前读。
    ///
    /// 这是「不许 seek」那条硬约束下唯一能给出旧帧的方式：代价是**再读一遍**，
    /// 随目标帧号线性增长。它是一次**取了旧帧**的事实，不是错误。
    Replay,
}

/// 一次取帧的计划。**纯的** —— 真池子与量化模拟走的是同一段规则。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchPlan {
    pub action: FetchAction,
    pub target: Frame,
    /// 要真读的帧号（游标到目标，含两端）。Hit 时为空。
    pub reads: Vec<Frame>,
    /// reads 里要**留进池子**的（等一下还会被要的那些）。
    pub keep: Vec<Frame>,
    /// 留完之后被挤出去的（先来先走）。
    pub evict: Vec<Frame>,
}

/// 定一次取帧计划。
///
/// * `cursor`：解码器**下一段字节**对应的帧号（读过的帧都小于它）；
/// * `kept`：池子里现在有哪些帧（插入顺序）；
/// * `slots`：池子容量（帧数）；
/// * `remaining`：这个源**还没被服务过**的请求次数（来源是 [`demand_of`] 的计数表）。
///
/// **留什么**是这段规则里唯一有讲究的地方：只留「等一下还会再被要」的帧 ——
/// 包括路上经过的，但不包括**目标帧自己**（马上就用掉了），除非它还要被要第二次。
pub fn plan_fetch(
    cursor: Frame,
    kept: &[Frame],
    target: Frame,
    slots: usize,
    remaining: &BTreeMap<Frame, usize>,
) -> FetchPlan {
    if kept.contains(&target) {
        return FetchPlan {
            action: FetchAction::Hit,
            target,
            reads: Vec::new(),
            keep: Vec::new(),
            evict: Vec::new(),
        };
    }
    let action = if target < cursor {
        FetchAction::Replay
    } else {
        FetchAction::Forward
    };
    // 回退只能从头再读一遍；向前就从游标接着读。
    let start = if action == FetchAction::Replay {
        0
    } else {
        cursor
    };
    let reads: Vec<Frame> = (start..=target).collect();
    let left = |frame: Frame| remaining.get(&frame).copied().unwrap_or(0);
    let keep: Vec<Frame> = reads
        .iter()
        .copied()
        .filter(|frame| {
            // 目标帧这一次马上用掉，所以它要"还剩 ≥ 2 次"才值得留。
            let threshold = if *frame == target { 1 } else { 0 };
            left(*frame) > threshold && !kept.contains(frame)
        })
        .collect();
    let mut order: Vec<Frame> = kept.to_vec();
    order.extend(keep.iter().copied());
    let evict = if order.len() > slots {
        order[..order.len() - slots].to_vec()
    } else {
        Vec::new()
    };
    FetchPlan {
        action,
        target,
        reads,
        keep,
        evict,
    }
}

/// 池子这一趟的账。**事实**，不是判据：回退多说明工程这么排的，不说明出了错。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct PoolStats {
    pub hits: usize,
    pub forward: usize,
    pub replays: usize,
    /// 真的从解码器读了多少帧（回退会让它大于不同帧数）。
    pub frames_read: usize,
}

/// 离屏帧槽：把一帧画进一块**跨帧复用**的纹理。
///
/// 这是 `dhampir_core::io::FrameSink` 在 **native 侧**的形态 —— `io.rs` 的文档写的就是它
/// （"native 侧：离屏 texture，交给 readback 读回、再交给编码器"）。
/// wasm 侧的对称物是 `CanvasFrameSink`（`acquire` 抓 surface 纹理、`finish` 才 `present`）。
///
/// # 所有权（这一条是从现有语义读出来的，不是新定的）
///
/// 原先是在渲染区间**之前**就地 `create_texture` 一次、跨帧复用 —— 所以"纹理归帧槽自己、
/// 生命周期 = 一次渲染运行"**与既有行为完全一致**，换成它不改变任何东西。
pub struct OffscreenFrameSink {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
}

impl OffscreenFrameSink {
    /// 一次渲染运行建一块，之后跨帧复用。
    pub fn new(device: &wgpu::Device, width: u32, height: u32, label: &'static str) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: WORK_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        Self { texture, view }
    }

    /// 本帧要画进去的视图。（每帧同一个 —— 与"跨帧复用"一致。）
    pub fn view(&self) -> &wgpu::TextureView {
        &self.view
    }

    /// 底层纹理（读回 / 编码要用它的 `COPY_SRC`）。
    pub fn texture(&self) -> &wgpu::Texture {
        &self.texture
    }
}

/// 让 native 侧也**实现** `io::FrameSink` —— "两个入口各实现同一对 trait" 的后一半。
///
/// 它很薄（`acquire` 就是返回那个复用的 view、`finish` 无处可做），这是**对的**：
/// 两个入口的差别本来就只在"帧槽去哪"，而那是 `acquire`/`finish` 两行的事。
impl dhampir_core::io::FrameSink for OffscreenFrameSink {
    fn acquire(&mut self, _device: &wgpu::Device) -> wgpu::TextureView {
        self.view.clone()
    }

    /// 离屏帧槽没有"提交"这一步：读回/编码由调用方接着做。
    fn finish(&mut self, _frame: i64) {}
}

impl PoolStats {
    /// 把另一份账并进来（**分块并行**时每块各有一份，最后要汇总）。
    ///
    /// 逐字段相加，不是"取更大的那个"：这些是**累计量**，两块各读了 100 帧
    /// 就是一共读了 200 帧。
    pub fn merge(&mut self, other: Self) {
        self.hits += other.hits;
        self.forward += other.forward;
        self.replays += other.replays;
        self.frames_read += other.frames_read;
    }
}

/// 池子的纯状态机：谁在里面、游标到哪、花了多少。
///
/// 抽出来的理由与 plan_advance 当时一样：**量化脚本与真池子必须是同一段规则**，
/// 否则量化出来的数不描述产品。
#[derive(Debug, Clone)]
pub struct PoolCursor {
    cursor: Frame,
    kept: Vec<Frame>,
    slots: usize,
    remaining: BTreeMap<Frame, usize>,
    stats: PoolStats,
}

impl PoolCursor {
    pub fn new(slots: usize, remaining: BTreeMap<Frame, usize>) -> Self {
        Self {
            cursor: 0,
            kept: Vec::new(),
            // 容量至少 1：0 槽的池子不是"不缓存"，是"永远给不出旧帧"。
            slots: slots.max(1),
            remaining,
            stats: PoolStats::default(),
        }
    }

    pub fn cursor(&self) -> Frame {
        self.cursor
    }

    pub fn kept(&self) -> &[Frame] {
        &self.kept
    }

    /// 池子里现在占了几帧 —— 量化脚本用它算"这个工程至少要几槽"。**内存就是这个数乘帧字节**。
    pub fn held(&self) -> usize {
        self.kept.len()
    }

    pub fn stats(&self) -> PoolStats {
        self.stats
    }

    pub fn plan(&self, target: Frame) -> FetchPlan {
        plan_fetch(self.cursor, &self.kept, target, self.slots, &self.remaining)
    }

    /// 照计划推进状态。**调用方必须先真的做完 I/O**（否则游标与解码器会脱节）。
    pub fn commit(&mut self, plan: &FetchPlan) {
        match plan.action {
            FetchAction::Hit => self.stats.hits += 1,
            FetchAction::Forward => {
                self.stats.forward += 1;
                self.stats.frames_read += plan.reads.len();
            }
            FetchAction::Replay => {
                self.stats.replays += 1;
                self.stats.frames_read += plan.reads.len();
            }
        }
        if let Some(last) = plan.reads.last() {
            self.cursor = last + 1;
        }
        // 这一次请求把"还要被要几次"用掉一次。
        if let Some(count) = self.remaining.get_mut(&plan.target) {
            *count -= 1;
        }
        // **用完了就扔**：留着它也没人再来要（顺序出片时每一帧都走这条路，
        // 于是池子的占用与"一路源一张纹理"时一样）。
        let dead: Vec<Frame> = self
            .kept
            .iter()
            .copied()
            .filter(|frame| self.remaining.get(frame).copied().unwrap_or(0) == 0)
            .collect();
        if !dead.is_empty() {
            self.kept.retain(|frame| !dead.contains(frame));
        }
        // 先挤旧的、再放新的，**两边都要过一遍淘汰名单**：
        // 挤出去的可能是刚才读进来的那一批（读得比容量多时必然如此）。
        if !plan.evict.is_empty() {
            self.kept.retain(|frame| !plan.evict.contains(frame));
        }
        for frame in &plan.keep {
            if !plan.evict.contains(frame) {
                self.kept.push(*frame);
            }
        }
    }
}

/// 一个源的池子能放几帧。
///
/// 按**字节**封顶而不是按帧数：1080p 一帧 8 MB、640x360 一帧 0.9 MB，
/// 同一个帧数在两种尺寸下差九倍内存。上限写在 [`POOL_BYTES_PER_SOURCE`]，
/// 边际帧数封在 [`POOL_SLOTS_MAX`]。
pub fn pool_slots(width: u32, height: u32) -> usize {
    let bytes = frame_bytes(width, height);
    if bytes == 0 {
        return 1;
    }
    (POOL_BYTES_PER_SOURCE / bytes).clamp(2, POOL_SLOTS_MAX)
}

/// 每个源的池子字节上限。**这是 T5.1 量化之后反推出来的数**，不是拍的
/// （实测表在 plan/measurements.md）。
///
/// 要**同时留住**几帧，取决于「这个源上还有几个没轮到它的请求」——
/// **不是回退的距离**。实测两种常见形态：
///
/// * 同一素材画中画（两处相距 98 帧）：最多要留住 **29** 帧；
/// * 同一素材三段倒放（距离 p50 120）：最多要留住 **60** 帧。
///
/// 默认导出尺寸是 1080p（`render_hints`），一帧 8.29 MB，所以「覆盖画中画那一种」
/// 要 `29 x 8.29 MB = 240 MB` —— 留一点余量取 **256 MiB**，1080p 下正好 32 槽
/// （实测：32 槽那一刻重启归零，读帧量从 371 掉到 159，也就是 6.18x -> 2.65x）。
///
/// 为什么不去覆盖 60 帧那一种：`60 x 8.29 MB = 498 MB/源`，四个源就是 2 GB 的**天花板**；
/// 而那笔钱买到的只是把倒放那种工程的读帧量从 2.60x 降到 1.99x。
/// 注意这**是一条上限、不是一次分配**（池子只留"还有下一次请求"的帧，见 [`plan_fetch`]，
/// 顺序出片时每个源只占 1 帧），但天花板本身在小显存机器上就是风险。
pub const POOL_BYTES_PER_SOURCE: usize = 256 * 1024 * 1024;

/// 单个源的池子最多几帧。按字节算在小尺寸下会给出很大的数（360p 能放 291 帧），
/// 所以封一下；这个封顶取**实测要留住的最多帧数**（倒放那种形态的 60 帧）上的整数。
pub const POOL_SLOTS_MAX: usize = 64;

/// 一帧裸像素的字节数。
pub fn frame_bytes(width: u32, height: u32) -> usize {
    width as usize * height as usize * 4
}

/// 编码器要的帧率。**由工程 timebase 决定，不是源视频的 fps。**
///
/// 这条修错过一次：当时把「从源取帧率」当成了通用修法，于是 90 帧被编成
/// 60fps / 1.5 秒，而浏览器那条是 30fps / 3.0 秒 —— **帧数一样、时长差一半**。
/// 只看帧数发现不了。
pub fn encoder_fps(timebase: &TimebaseDto) -> Result<f64, String> {
    if timebase.den == 0 {
        return Err("工程的 timebase 分母为 0".to_string());
    }
    let fps = f64::from(timebase.num) / f64::from(timebase.den);
    if !(fps > 0.0) {
        return Err(format!(
            "工程的 timebase 不是正帧率：{}/{}",
            timebase.num, timebase.den
        ));
    }
    Ok(fps)
}

/// 以 timebase 为尺子，frames 帧是多长（秒）。
pub fn seconds_for(frames: usize, timebase: &TimebaseDto) -> f64 {
    if timebase.num == 0 {
        return 0.0;
    }
    frames as f64 * f64::from(timebase.den) / f64::from(timebase.num)
}

/// 送进编码器 -r 的有理数形式。用 30/1 而不是浮点：
/// ffmpeg 认有理数，而浮点会在 30000/1001 这种地方出现舍入。
pub fn ffmpeg_rate(timebase: &TimebaseDto) -> String {
    format!("{}/{}", timebase.num, timebase.den)
}

/// 问题清单的累加器。**按 (code, path) 去重并封顶** ——
/// 90 帧的渲染里同一个缺陷会被撞到 90 次，原样列出来等于没列。
#[derive(Debug, Default)]
pub struct IssueLog {
    seen: BTreeSet<(String, String)>,
    issues: Vec<Issue>,
    suppressed: usize,
}

/// 最多保留几条。超出只计数 —— detail 该短，结论该准。
pub const MAX_ISSUES: usize = 24;

impl IssueLog {
    pub fn new() -> Self {
        Self {
            seen: BTreeSet::new(),
            issues: Vec::new(),
            suppressed: 0,
        }
    }

    /// 记一条。同一个 (code, path) 只记第一次；超出上限的部分只计数。
    pub fn record(&mut self, code: &str, path: &str, message: String) {
        let key = (code.to_string(), path.to_string());
        if !self.seen.insert(key) {
            return;
        }
        if self.issues.len() >= MAX_ISSUES {
            self.suppressed += 1;
            return;
        }
        self.issues.push(Issue::new(code, path, message));
    }

    pub fn is_empty(&self) -> bool {
        self.issues.is_empty()
    }

    pub fn suppressed(&self) -> usize {
        self.suppressed
    }

    /// 取走清单。被抑制的条数会作为最后一条附上。
    pub fn into_vec(mut self) -> Vec<Issue> {
        if self.suppressed > 0 {
            self.issues.push(Issue::new(
                "issues_suppressed",
                "render",
                format!("另有 {} 处同类问题未逐条列出", self.suppressed),
            ));
        }
        self.issues
    }
}

// ---------------------------------------------------------------------------
// 素材表
// ---------------------------------------------------------------------------

/// asset id -> 文件。**位置由宿主解释**，这正是契约里只写 asset_id 的意义。
#[derive(Debug, Default, Clone)]
pub struct SourceTable {
    entries: HashMap<String, PathBuf>,
}

impl SourceTable {
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }

    pub fn insert(&mut self, asset_id: impl Into<String>, file: impl Into<PathBuf>) {
        self.entries.insert(asset_id.into(), file.into());
    }

    pub fn file_for(&self, asset_id: &str) -> Option<&Path> {
        self.entries.get(asset_id).map(PathBuf::as_path)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 按 id 排序的 (id, 文件) 列表。给「要开几路解码器」这种确定性输出用。
    pub fn sorted(&self) -> Vec<(String, PathBuf)> {
        let mut rows: Vec<(String, PathBuf)> = self
            .entries
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        rows.sort();
        rows
    }
}

// ---------------------------------------------------------------------------
// 一路源的顺序解码器
// ---------------------------------------------------------------------------

/// 一路素材的解码器 + 它那一池纹理。
///
/// **纹理必须复用**：1080p 每帧 8 MB，90 帧里每帧新建一次就是 720 MB 的分配抖动，
/// 而且会把「两端的差异」混进分配顺序的差异里。
struct SourcePool {
    source: String,
    file: PathBuf,
    child: Child,
    stdout: ChildStdout,
    width: u32,
    height: u32,
    /// 取帧策略与它的账（纯状态机，见 [`PoolCursor`]）。
    cursor: PoolCursor,
    /// 池子里的纹理，键是**源内帧号**。与 `cursor.kept()` 同步 ——
    /// 谁进谁出只由 [`PoolCursor::commit`] 决定，这里只是把像素放上去。
    slots: HashMap<Frame, wgpu::TextureView>,
    /// 读帧用的缓冲。**不每帧新分配**：8 MB 一次的分配抖动会混进测量。
    buffer: Vec<u8>,
    /// 这一路重启过几次（回退的代价，作为事实上报）。
    restarts: usize,
}

/// 问一次媒体尺寸。
///
/// **csv 的列序不是请求顺序**（这个坑踩过一次：把宽度当成了帧数）。
/// 这里只要两个字段，并且**断言字段数**，字段数不对就报错而不是猜。
fn probe_size(file: &Path) -> Result<(u32, u32), String> {
    let output = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=width,height",
            "-of",
            "csv=p=0",
        ])
        .arg(file)
        .output()
        .map_err(|error| format!("起不了 ffprobe：{error}（PATH 里有 ffprobe 吗？）"))?;
    if !output.status.success() {
        return Err(format!(
            "ffprobe 读不了 {}：{}",
            file.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let text = String::from_utf8(output.stdout).map_err(|error| error.to_string())?;
    let fields: Vec<&str> = text.trim().split(',').collect();
    if fields.len() != 2 {
        return Err(format!(
            "ffprobe 本该给 2 个字段（width,height），实际 {} 个：{:?} —— 解析前提不成立",
            fields.len(),
            text.trim()
        ));
    }
    let width: u32 = fields[0]
        .trim()
        .parse()
        .map_err(|_| format!("宽度不是数：{}", fields[0]))?;
    let height: u32 = fields[1]
        .trim()
        .parse()
        .map_err(|_| format!("高度不是数：{}", fields[1]))?;
    if width == 0 || height == 0 {
        return Err(format!("ffprobe 给出了 {width}x{height}，无法按帧切分"));
    }
    Ok((width, height))
}

/// 起一路解码器：吐裸 RGBA 到 stdout。
///
/// **色彩矩阵必须显式。** 不写的话 FFmpeg 从容器元数据里猜，而浏览器
/// （WebCodecs）也有一套自己的猜法，两边默认值不一定相同（BT.601 vs 709），
/// 同一帧看起来就偏色 —— 而那不是渲染 bug。
/// 这条也被 check-sequential-decode 守卫盯着，别删。
///
/// **不回退、不 seek**：起点永远是流的开头（这条同样是硬约束，见模块头）。
fn spawn_decoder(file: &Path) -> Result<(Child, ChildStdout), String> {
    if !file.exists() {
        return Err(format!("素材文件不在：{}", file.display()));
    }
    let mut child = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(file)
        .args([
            "-vf",
            "scale=out_color_matrix=bt709",
            "-f",
            "rawvideo",
            "-pix_fmt",
            DECODE_PIXEL_FORMAT,
            "-",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|error| format!("起不了 ffmpeg：{error}（PATH 里有 ffmpeg 吗？）"))?;
    let stdout = child.stdout.take().ok_or("拿不到解码器的 stdout")?;
    Ok((child, stdout))
}

impl SourcePool {
    fn open(source: &str, file: &Path, demand: BTreeMap<Frame, usize>) -> Result<Self, String> {
        let (width, height) = probe_size(file)?;
        let (child, stdout) = spawn_decoder(file)?;
        let frame_bytes = frame_bytes(width, height);
        Ok(Self {
            source: source.to_string(),
            file: file.to_path_buf(),
            child,
            stdout,
            width,
            height,
            cursor: PoolCursor::new(pool_slots(width, height), demand),
            slots: HashMap::new(),
            buffer: vec![0u8; frame_bytes],
            restarts: 0,
        })
    }

    /// **重启这一路解码器**（回退的唯一做法：不许 seek，那只能从头再来）。
    ///
    /// 池子里的纹理**不受影响** —— 它们已经解出来了，重启只是让游标回到 0。
    fn restart(&mut self) -> Result<(), String> {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let (child, stdout) = spawn_decoder(&self.file)?;
        self.child = child;
        self.stdout = stdout;
        self.restarts += 1;
        Ok(())
    }

    /// 把一帧读进来并按计划处理（留 / 不留 / 挤掉谁）。返回**目标帧**那张纹理。
    fn read_frame(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        target: Frame,
        plan: &FetchPlan,
    ) -> Result<wgpu::TextureView, String> {
        let mut wanted: Option<wgpu::TextureView> = None;
        for frame in plan.reads.iter().copied() {
            match self.stdout.read_exact(&mut self.buffer) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
                    return Err(format!(
                        "源 {}（{}）在第 {} 帧就结束了，而工程要第 {target} 帧",
                        self.source,
                        self.file.display(),
                        frame
                    ));
                }
                Err(error) => {
                    return Err(format!("读源 {} 的第 {frame} 帧失败：{error}", self.source));
                }
            }
            // 不在需求里的中间帧**只读不传**：省一次 8 MB 的 PCIe 往返，而像素本来就要丢掉。
            let keep = plan.keep.contains(&frame);
            if !keep && frame != target {
                continue;
            }
            let view = self.upload(device, queue);
            if frame == target {
                wanted = Some(view.clone());
            }
            // 目标帧**这一趟一定要有**（它马上要被绑上去画），
            // 但只在"还有下一次"的时候才留在池子里。
            if keep {
                self.slots.insert(frame, view);
            }
        }
        for frame in &plan.evict {
            self.slots.remove(frame);
        }
        wanted.ok_or_else(|| {
            format!(
                "读完第 {target} 帧却没拿到它的纹理（{}）—— 计划与读循环对不上",
                self.source
            )
        })
    }

    /// 把 buffer 里那一帧传成一张纹理。
    fn upload(&self, device: &wgpu::Device, queue: &wgpu::Queue) -> wgpu::TextureView {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("dhampir pipeline source"),
            size: wgpu::Extent3d {
                width: self.width,
                height: self.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: WORK_FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &self.buffer,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(self.width * 4),
                rows_per_image: Some(self.height),
            },
            wgpu::Extent3d {
                width: self.width,
                height: self.height,
                depth_or_array_layers: 1,
            },
        );
        texture.create_view(&wgpu::TextureViewDescriptor::default())
    }

    /// 取某一帧的纹理。池里有就给，没有就（必要时重启再）读到它。
    fn frame_for(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        target: Frame,
    ) -> Result<wgpu::TextureView, String> {
        let plan = self.cursor.plan(target);
        if plan.action == FetchAction::Hit {
            let view = self
                .slots
                .get(&target)
                .ok_or_else(|| format!("池子里说第 {target} 帧在，实际不在（{}）", self.source))?;
            let view = view.clone();
            self.cursor.commit(&plan);
            return Ok(view);
        }
        if plan.action == FetchAction::Replay {
            self.restart()?;
        }
        let view = self.read_frame(device, queue, target, &plan)?;
        self.cursor.commit(&plan);
        Ok(view)
    }

    /// 收工。解码器还没读到 EOF，直接杀掉而不是等它跑完整条流。
    fn close(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    fn stats(&self) -> PoolStats {
        self.cursor.stats()
    }
}


/// 把**动图解码器读出来的延迟表**并进资产时间表（方案 §8.3-2「时间真值归一」）。
///
/// # 为什么在**建计划**的时候就要做
///
/// 求值（`evaluate_v2_with_assets`）与渲染（resolver）是两处，而它们必须用**同一张表**：
/// 一张决定"第几帧"、一张决定"那一帧长什么样"。只在渲染那一侧用解码真值，
/// 就会出现"帧号按工程 JSON 算、像素按解码帧数给"——那正是要消灭的那种漂移。
///
/// 所以计划里那张表就该是解码真值。**两侧用的是同一个解码器**，
/// 于是 resolver 里的惰性加载与这里算出来的是同一份东西（缓存命中，不重复解码）。
///
/// # 只认 magic，不认 kind
///
/// 登记表里的 kind 是录入时的猜测，magic 是事实（方案 §9.3-4）。
/// 但**只读文件头**：视频几百 MB，为了判断"是不是动图"整个读进来是不能接受的。
/// 解不开的动图**不改表**也**不在这里报错** —— 报错归渲染那一侧
/// （`ensure_animation` 记 issue），这里只负责"能解的就用它的真值"。
pub fn asset_timebases_with_animations(
    base: &AssetTimebases,
    sources: &SourceTable,
) -> AssetTimebases {
    let mut merged = base.clone();
    for (asset_id, file) in sources.sorted() {
        let mut head = [0u8; 12];
        let is_animation = match std::fs::File::open(&file) {
            Ok(mut handle) => match handle.read(&mut head) {
                Ok(read) => dhampir_core::animation::detect_format(&head[..read]).is_some(),
                Err(_) => false,
            },
            Err(_) => false,
        };
        if !is_animation {
            continue;
        }
        let Some(timebase) = merged.get(&asset_id).copied() else {
            // 登记表里没有时间基：那是"这个素材还没被登记"，不是这里能补的。
            continue;
        };
        let Ok(bytes) = std::fs::read(&file) else {
            continue;
        };
        let Ok(animation) = dhampir_core::animation::decode(&bytes) else {
            continue;
        };
        merged.insert_with_delays(asset_id, timebase, animation.delays_ms());
    }
    merged
}

// ---------------------------------------------------------------------------
// 按工程解析源的 resolver
// ---------------------------------------------------------------------------

/// 把「asset_id -> 文件」变成「这一帧上这个 source 的哪一帧用哪张纹理」。
pub struct DecodingSources<'a> {
    device: &'a wgpu::Device,
    queue: &'a wgpu::Queue,
    table: &'a SourceTable,
    /// 每个源这一趟会要的全部帧号（见 [`demand_of`]）——
    /// 池子靠它决定"读到的这一帧要不要留下"。
    demand: HashMap<String, BTreeMap<Frame, usize>>,
    streams: HashMap<String, SourcePool>,
    /// 开失败过的源。记下来是为了**不要每帧重试一次并刷屏**。
    failed: BTreeSet<String>,
    log: IssueLog,
    current_frame: Frame,
    /// 动图（GIF / 动画 WebP）的逐帧纹理：**引擎自己解码、自己供帧**。
    ///
    /// 出片这一侧与预览那一侧用的是**同一份** core 解码器（方案 §4.6）：
    /// 「同一个贴纸在预览里第 30 帧不透明、在成片里第 31 帧」这类错在
    /// 结构上就不存在。
    animations: AnimationTextures,
    /// 已经**试过**当动图加载的源（成或不成都算）：避免每帧重读一次文件头，
    /// 也避免同一个坏文件每帧刷一条 issue。
    animation_tried: BTreeSet<String>,
}

impl<'a> DecodingSources<'a> {
    pub fn new(
        device: &'a wgpu::Device,
        queue: &'a wgpu::Queue,
        table: &'a SourceTable,
        demand: HashMap<String, BTreeMap<Frame, usize>>,
    ) -> Self {
        Self {
            device,
            queue,
            table,
            demand,
            streams: HashMap::new(),
            failed: BTreeSet::new(),
            log: IssueLog::new(),
            current_frame: 0,
            animations: AnimationTextures::new(device.clone(), queue.clone(), 0),
            animation_tried: BTreeSet::new(),
        }
    }

    /// 进入下一个输出帧。**只是把帧号记进诊断路径** ——
    /// 同一帧里同一个源要几个源内帧由池子自己回答，不需要这里记账了。
    pub fn begin_frame(&mut self, frame: Frame) {
        self.current_frame = frame;
    }

    /// 已经开起来的解码器路数。给报告用。
    pub fn opened_streams(&self) -> usize {
        self.streams.len()
    }

    /// 这一趟解码侧的账（命中 / 向前 / 重启 / 读了多少帧）。**事实，不是判据。**
    pub fn stats(&self) -> PoolStats {
        let mut total = PoolStats::default();
        for stream in self.streams.values() {
            let one = stream.stats();
            total.hits += one.hits;
            total.forward += one.forward;
            total.replays += one.replays;
            total.frames_read += one.frames_read;
        }
        total
    }

    /// 收工：杀掉所有解码器。
    pub fn close(&mut self) {
        for stream in self.streams.values_mut() {
            stream.close();
        }
        self.streams.clear();
    }

    /// 取走问题清单。
    pub fn issues(self) -> Vec<Issue> {
        self.log.into_vec()
    }

    fn path_of(&self, source: &str) -> String {
        format!("frame[{}].source[{}]", self.current_frame, source)
    }


    /// 需要时才把这一路**当动图**加载：读文件头认 magic，是动图就整段解码。
    ///
    /// 返回「现在动图缓存里有它」。三条规矩：
    ///
    /// 1. **先看 magic 再决定读不读整个文件**：视频动辄几百 MB，
    ///    为了判断"是不是动图"把它整个读进来是不能接受的。头 12 字节足够
    ///    （GIF8 与 `RIFF????WEBP` 都在前 12 字节里）。
    /// 2. **只试一次**（成功或失败都记进 `animation_tried`）：失败的文件每帧重试
    ///    会把出片日志刷成一堵墙，而失败原因一帧与一万帧是同一个。
    /// 3. **失败不改路由**：认不出 magic 就返回 false，交给下面 ffmpeg 那条路
    ///    （契约：动图这条路只收动图，别的素材照旧）。
    fn ensure_animation(&mut self, source: &str) -> bool {
        if self.animations.contains(source) {
            return true;
        }
        if !self.animation_tried.insert(source.to_string()) {
            return false;
        }
        let Some(file) = self.table.file_for(source).map(Path::to_path_buf) else {
            // 素材表里没有它 —— 这是 `ensure_stream` 会报的那种情况，
            // 不在这里重复报（同一个源会由 ffmpeg 那条路给出 `unknown_asset`）。
            return false;
        };
        let mut head = [0u8; 12];
        let is_animation = match std::fs::File::open(&file) {
            Ok(mut handle) => {
                match handle.read(&mut head) {
                    Ok(read) => dhampir_core::animation::detect_format(&head[..read]).is_some(),
                    Err(_) => false,
                }
            }
            Err(_) => false,
        };
        if !is_animation {
            return false;
        }
        let bytes = match std::fs::read(&file) {
            Ok(bytes) => bytes,
            Err(error) => {
                let path = self.path_of(source);
                self.log.record(
                    "asset_unavailable",
                    &path,
                    format!("读不了素材文件 {}：{error}", file.display()),
                );
                return false;
            }
        };
        let animation = match dhampir_core::animation::decode(&bytes) {
            Ok(animation) => animation,
            Err(error) => {
                let path = self.path_of(source);
                // 记 issue 而不是让它掉到 ffmpeg 那条路：ffmpeg 对着一张**坏掉的动图**
                // 的报错（"没有视频流"）会把病因指错方向。
                self.log.record("animation_decode_failed", &path, error.to_string());
                return false;
            }
        };
        // 时间真值走**解码器读出来的那份**：与 wasm 侧同一个口径（方案 §8.3-2）。
        // 出片这一侧不经过宿主，所以直接把它并进 demand 用的资产表交给求值层
        // —— 见 `render_frames_png` 那一层对 `asset_timebases` 的处理。
        match self.animations.upload(source, &animation) {
            Ok(()) => true,
            Err(error) => {
                let path = self.path_of(source);
                self.log.record("animation_upload_failed", &path, error.to_string());
                false
            }
        }
    }

    /// 需要时才开解码器。开失败就记一次并进 failed。
    fn ensure_stream(&mut self, source: &str) -> bool {
        if self.streams.contains_key(source) {
            return true;
        }
        if self.failed.contains(source) {
            return false;
        }
        let Some(file) = self.table.file_for(source) else {
            let path = self.path_of(source);
            self.log.record(
                "unknown_asset",
                &path,
                format!("工程引用了素材 {source}，而宿主没有它的位置（素材表里没有）"),
            );
            self.failed.insert(source.to_string());
            return false;
        };
        let file = file.to_path_buf();
        let demand = self.demand.get(source).cloned().unwrap_or_default();
        match SourcePool::open(source, &file, demand) {
            Ok(stream) => {
                self.streams.insert(source.to_string(), stream);
                true
            }
            Err(error) => {
                let path = self.path_of(source);
                self.log.record("asset_unavailable", &path, error);
                self.failed.insert(source.to_string());
                false
            }
        }
    }
}

impl SourceResolver for DecodingSources<'_> {
    fn texture_for(
        &mut self,
        source: &str,
        source_frame: Frame,
    ) -> Option<(wgpu::TextureView, (u32, u32))> {
        // ---- 动图优先 ----
        // 顺序是有讲究的：动图的帧号由求值层算，**不是**「按时间 seek 一个视频」。
        // 拿它去喂 ffmpeg 会得到一个"能解码但内容不是这一帧"的结果 ——
        // 画面看起来完全正常，只是贴纸的相位是错的。
        if let Some(hit) = self.animations.texture_for(source, source_frame) {
            return Some(hit);
        }
        if self.ensure_animation(source) {
            if let Some(hit) = self.animations.texture_for(source, source_frame) {
                return Some(hit);
            }
        }
        if !self.ensure_stream(source) {
            return None;
        }
        let device = self.device;
        let queue = self.queue;
        let stream = self.streams.get_mut(source)?;
        let size = (stream.width, stream.height);
        match stream.frame_for(device, queue, source_frame) {
            Ok(view) => Some((view, size)),
            Err(error) => {
                let path = self.path_of(source);
                self.log.record("source_decode_failed", &path, error);
                None
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 出片
// ---------------------------------------------------------------------------

/// 音频这一趟怎么办。
///
/// 为什么是**模式**而不是"要不要音频"的布尔：这两条的差别不只是声音，
/// 而是**走哪条代码路径** —— `Silent` 保证与引入音频之前逐字节相同的那条路。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AudioMode {
    /// 工程里有音轨就出声音。**没有音轨时与 `Silent` 走的是同一条路** ——
    /// 于是"没有音频的工程"这次改动前后产物逐字节相同。
    #[default]
    Auto,
    /// 明确不要声音（`--no-audio`）。它是一道**可以主动选的**逃生门，
    /// 而不是"还没实现所以静音"。
    Silent,
}

/// 一次出片的入参。
pub struct RenderPlan<'a> {
    pub timeline: &'a TimelineV2,
    pub sources: &'a SourceTable,
    /// 素材 id → 时间基。**源帧号靠它换算**（见 compose::evaluate_v2_with_assets）。
    /// 传空的不是错 —— 那是"假设素材帧率与时间线一致"的旧语义，
    /// 但素材帧率真的不同时画面会变速，所以调用方应当把工程文件的资产表带上。
    pub asset_timebases: &'a AssetTimebases,
    /// 帧区间 [from, to]，单位整数帧。
    pub from: Frame,
    pub to: Frame,
    pub width: u32,
    pub height: u32,
    /// **文档坐标系**（工程的 render_hints）。transform.x/y 这类像素量以它度量。
    ///
    /// 默认路径上 width/height 就等于它（导出尺寸取 render_hints），于是比例是 1.0、
    /// 行为与引入 RenderSpace 之前逐字节一致。显式指定了别的导出尺寸时才发生缩放 ——
    /// 那正是「同一个工程在不同尺寸下位移比例不同」被修掉的地方。
    pub sequence: (u32, u32),
    /// 字幕素材 id → 解析好的字幕条。**由调用方读文件 + 解析**（core 是纯函数、不做 I/O）。
    ///
    /// 空表表示"没有字幕可画"，与"有字幕轨但这一帧是空的"处理相同：什么都不画。
    /// 但**表里缺了某条轨道引用的 id** 就是宿主自己的错 —— 那种情况由调用方
    /// 在装载阶段报错，不许静默降级成"这部片子没有字幕"。
    pub subtitles: &'a SubtitleTable,
    /// 栅格化用的字体文件。`None` 表示这个宿主没有字体 ——
    /// 于是"有字要画"会变成 `subtitle_font_missing` 这条问题，让整次出片判失败。
    ///
    /// 为什么不给一个默认字体：本仓不内嵌字体、也不去猜系统字体在哪。
    /// 猜错的后果是**产出一份字全是方框的片子**，而"看起来成功、其实不对"正是要消灭的。
    pub font_file: Option<&'a Path>,
    /// 粗体字体文件（可选）。契约里的 `font_weight >= 600` 时用它 ——
    /// ffmpeg 的 `drawtext` 没有 `bold` 开关，**粗体就是换一个字体文件**。
    pub font_bold_file: Option<&'a Path>,
    /// 字体目录（可选）：按契约里的 `font_family` 名字在里面找。
    ///
    /// 语义是**"从你给我的目录里找"**，不是"去系统里猜" —— 那条纪律没有破。
    pub font_dir: Option<&'a Path>,
    /// **分块并行**要开几个 worker：`1` = 不分块（默认，产物与从前同参数）、
    /// `0` = 自动（按可用并行度，最多 8）、`n` = 指定 n。
    ///
    /// 为什么默认 `1`：分块会把一段视频切成 N 段各自编码再 concat，
    /// **每段开头都是一个新的 GOP**，所以产物与"一口气编码"不是逐字节相同。
    /// 本仓有一批判据钉的是"同一份工程出同样的字节"，默认保持单趟；
    /// 要速度就显式开（`--chunk-workers 0`）。
    pub chunk_workers: usize,
    /// 音轨怎么办。AudioPlan 由本函数从 `timeline`/`sources`/`asset_timebases` 摊出来 ——
    /// **同源求值**要的就是"同一份入参"，让调用方另传一份计划进来反而会分叉。
    pub audio: AudioMode,
    pub output: &'a Path,
}

/// 音频这一趟干了什么。**是事实，不是判据** —— 判据仍然是问题清单。
///
/// 报出来的理由与解码侧的 [`PoolStats`] 一样：出片慢或声音不对的时候，
/// "这一趟读了几路素材、从哪儿重新开始读、补了多少静音"是第一个该看的数。
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct AudioStats {
    pub sample_rate: u32,
    pub channels: u16,
    /// 摊出来的音频段数（0 = 这一趟没有声音）。
    pub segments: usize,
    /// 这一趟音轨应当有的采样点数（由帧区间有理数算出）。
    pub expected_samples: i64,
    /// 段与段之间补的静音（时间线上本来就没声音的地方）。
    pub gap_samples: i64,
    /// 素材不够长而补的静音 —— **这个数不为零就说明有东西被截断了**。
    pub padded_samples: i64,
    /// 从素材里**真读出来**的采样点总数（不含补的静音）。
    ///
    /// 它是"这一段到底用了多少素材"的数：拿它与 `expected_samples - gap_samples`
    /// 比一比，就知道有没有素材白读了或者读漏了。
    pub source_samples_read: i64,
    /// 与已有内容**相加**的采样点数（音效叠加，T13）。
    ///
    /// 0 = 这一趟没有叠加，音轨是纯顺序写出来的 —— 与引入音效之前走的是同一条路。
    pub mixed_samples: i64,
    /// 相加之后被**钳位**的采样点数。
    ///
    /// **这个数不为零必须看得见**：它是"增益调大了、该改小一点"的信号。
    /// 钳位本身是安全的（不会回绕成爆音），但它意味着那一瞬间的波形被削平了。
    pub clipped_samples: i64,
    /// 发生叠加的地方（T13）：`"轨/层 与 轨/层 在第 N 帧叠上，共 M 个采样点"`。
    ///
    /// # 为什么这个也要报出来
    ///
    /// 叠加本身**不是错**（音效天生就压在背景音上），所以它不进问题清单。
    /// 但"我没想到会叠"是常见的排错起点，而叠加**听感上完全正常** ——
    /// 只听见音效、背景人声被盖住，听起来也像"有个声音"。
    /// 所以它必须与 `clipped_samples` 一样**看得见**：不是报警，是让事实可见。
    pub overlaps: Vec<String>,
}

/// 一次出片的结果。**问题清单不在这里判** —— 本模块只出事实，
/// 由调用方（CLI）决定非空清单意味着失败。
#[derive(Debug, Clone)]
pub struct RenderReport {
    pub output: PathBuf,
    /// 本管道画出去并写进编码器的帧数。
    pub frames: usize,
    /// ffprobe 在产物里数出来的帧数。
    pub encoded_frames: Option<usize>,
    pub width: u32,
    pub height: u32,
    pub encoder_fps: f64,
    pub seconds: f64,
    pub elapsed_ms: u128,
    pub opened_streams: usize,
    /// 合成本该有内容、却一层都没画出来的帧号。
    pub empty_frames: Vec<Frame>,
    /// 文字覆盖层那边发生了什么（字幕画了几行、切了几行、丢了几行；弹幕画了几条、
    /// 丢了几条；缓存命中多少）。
    ///
    /// **它是事实，不是判据** —— 画不出来与画被切都进了 `issues`，
    /// 于是 `failed()` 只看问题清单就能判；而"超过 max_lines 丢了几行"不会把出片判失败。
    pub overlay: OverlayStats,
    /// 音轨那一路的账。无声的那一趟这份是 `Default`（全 0 / 0 段）。
    pub audio: AudioStats,
    /// 解码侧池子这一趟干了什么（命中 / 向前 / **重启** / 读了多少帧）。
    ///
    /// **同样是事实，不是判据**：重启多说明工程里回退多（代价随目标帧号线性增长），
    /// 不说明这一趟出了错。它是"这一趟为什么慢"的第一个可查的数。
    pub decode: PoolStats,
    pub issues: Vec<Issue>,
}

impl RenderReport {
    /// 这份报告算不算失败。**判据只写一次**，CLI 与测试都读它。
    pub fn failed(&self) -> bool {
        if !self.issues.is_empty() || !self.empty_frames.is_empty() {
            return true;
        }
        match self.encoded_frames {
            Some(encoded) => encoded != self.frames,
            // 数不出来就是**没验**，不能当通过。
            None => true,
        }
    }
}

/// 视频编码器的命令行。**抽成纯函数是为了能被冻住**：
/// "无声那条路逐字节不变"这句话需要一个可执行的判据，而这个判据就是
/// "这段 argv 与引入音频之前的那一刻逐项相等"（见单元测试 `无声路径的命令行没有变`）。
///
/// 返回 `OsString` 而不是 `String`：输出路径**原样**传给子进程，不经 `display()`
/// 那道有损转换 —— 路径里有什么字节，进命令行的就是什么字节。
pub fn encoder_args(width: u32, height: u32, fps: f64, output: &Path) -> Vec<OsString> {
    vec![
        "-v".into(),
        "error".into(),
        "-f".into(),
        "rawvideo".into(),
        "-pix_fmt".into(),
        DECODE_PIXEL_FORMAT.into(),
        "-s".into(),
        format!("{width}x{height}").into(),
        "-r".into(),
        format!("{fps}").into(),
        "-i".into(),
        "-".into(),
        "-c:v".into(),
        "libx264".into(),
        "-preset".into(),
        "veryfast".into(),
        "-crf".into(),
        "20".into(),
        "-pix_fmt".into(),
        "yuv420p".into(),
        "-movflags".into(),
        "+faststart".into(),
        "-y".into(),
        output.as_os_str().to_os_string(),
    ]
}

/// 起编码器。裸流没有尺寸与帧率信息，必须显式告诉它。
///
/// `target` 与 `plan.output` 是**分开的两个参数**：有音轨时视频先落到临时文件，
/// 随后与音轨复用写成真正的产物。没有音轨时两者是同一个路径 ——
/// 于是那条路上连参数都不用变（这一点由上面的 argv 冻结测试钉住）。
fn spawn_encoder(target: &Path, width: u32, height: u32, fps: f64) -> Result<Child, String> {
    if let Some(parent) = target.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| format!("建不了输出目录：{e}"))?;
        }
    }
    Command::new("ffmpeg")
        .args(encoder_args(width, height, fps, target))
        .stdin(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|error| format!("起不了编码器 ffmpeg：{error}（PATH 里有 ffmpeg 吗？）"))
}

/// 数一遍产物里的帧。**列序是 width,height,nb_read_frames** —— 帧数在第三列。
fn probe_output_frames(path: &Path) -> Result<(u32, u32, usize), String> {
    let output = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-count_frames",
            "-show_entries",
            "stream=nb_read_frames,width,height",
            "-of",
            "csv=p=0",
        ])
        .arg(path)
        .output()
        .map_err(|error| format!("起不了 ffprobe：{error}"))?;
    if !output.status.success() {
        return Err(format!(
            "ffprobe 读不了产物：{}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let text = String::from_utf8(output.stdout).map_err(|error| error.to_string())?;
    let fields: Vec<&str> = text.trim().split(',').collect();
    if fields.len() != 3 {
        return Err(format!(
            "ffprobe 本该给 3 个字段，实际 {} 个：{:?} —— 解析前提不成立",
            fields.len(),
            text.trim()
        ));
    }
    let width: u32 = fields[0]
        .trim()
        .parse()
        .map_err(|_| "宽度不是数".to_string())?;
    let height: u32 = fields[1]
        .trim()
        .parse()
        .map_err(|_| "高度不是数".to_string())?;
    let frames: usize = fields[2]
        .trim()
        .parse()
        .map_err(|_| "帧数不是数".to_string())?;
    Ok((width, height, frames))
}

// ---------------------------------------------------------------------------
// 音频那一路（T6）
//
// 与视频那一路**同一个原则**：素材只能向前读。区别只有一处 ——
// 视频读的是帧、走的是纹理池；音频读的是采样点、直接落进临时 PCM。
// ---------------------------------------------------------------------------

/// 一次从解码器读多少个采样点。够大摊薄系统调用，够小不至于占内存。
const AUDIO_CHUNK_SAMPLES: usize = 4096;

/// 一个采样点在 PCM 里占多少字节（`f32` × 声道数）。
fn audio_bytes_per_sample(channels: u16) -> usize {
    4 * usize::from(channels.max(1))
}

fn write_silence(
    out: &mut impl Write,
    samples: i64,
    bytes_per_sample: usize,
) -> Result<(), String> {
    if samples <= 0 {
        return Ok(());
    }
    let zeros = vec![0u8; AUDIO_CHUNK_SAMPLES * bytes_per_sample];
    let mut left = samples;
    while left > 0 {
        let take = left.min(AUDIO_CHUNK_SAMPLES as i64) as usize;
        out.write_all(&zeros[..take * bytes_per_sample])
            .map_err(|error| format!("写静音失败：{error}"))?;
        left -= take as i64;
    }
    Ok(())
}

/// 从素材里把**一段**拉出来（`f32le` / 48 kHz / 立体声），写进 `out`。返回真读到的采样点数。
///
/// # 为什么用 `atrim` 而不是 `-ss`
///
/// 两件都能"从中间开始"，但 `atrim` 的 `start_sample` / `end_sample` 是**整数采样点**，
/// 于是"从第几个采样点开始"这件事不必经过浮点秒 —— 而 `-ss` 只认时间，
/// 还会破「后端不许逐帧 seek」那条硬约束（`scripts/check-sequential-decode.mjs` 盯着它）。
///
/// # 为什么不是"开一路、往后丢"的游标
///
/// 那条路更省：同一份素材连着用几段，只解一遍。但它有个不好收的尾巴 ——
/// 每段读够了就得**提前掐掉**解码器，而 ffmpeg 会因此往 stderr 吐一句
/// `Error submitting a packet to the muxer`。**一次成功的出片不该有那句话**：
/// 它会让人以为出了错，然后去查一个并不存在的问题。
///
/// 所以这里选了更笨、但更干净的一条：**每段起一路**，解码到这段末尾自然结束。
/// 每段的代价是"从素材头解到这段末尾"（而不是只解这一段），
/// 这个代价量在 `plan/t6-evidence.md` 里 —— 别把它当成"不要钱"。
fn extract_segment(segment: &AudioSegment, out: &mut impl Write) -> Result<i64, String> {
    extract_segment_samples(segment, |chunk| out.write_all(chunk).map_err(|e| e.to_string()))
}

/// 与 [`extract_segment`] 相同，但把每块 PCM 交给回调而不是自己写文件。
///
/// **抽出来是为了混音**：混音那一趟要把这一段与已经写在盘上的内容**相加**，
/// 而"相加"没法用一个顺序 write 表达（它要读回、相加、再写回）。
/// 让这一段只负责"产出采样"，读回与相加交给调用方，两边就都不必知道对方的细节。
fn extract_segment_samples(
    segment: &AudioSegment,
    mut sink: impl FnMut(&[u8]) -> Result<(), String>,
) -> Result<i64, String> {
    let bytes_per_sample = audio_bytes_per_sample(AUDIO_CHANNELS);
    let start = segment.source_start_sample.max(0);
    let end = start.saturating_add(segment.output_samples);
    let mut child = Command::new("ffmpeg")
        .args(["-v", "error"])
        .arg("-i")
        .arg(&segment.file)
        .args([
            // 只要声音。**不加 -ss**（守卫不许 seek）。
            "-vn",
            "-filter_complex",
            &format!("atrim=start_sample={start}:end_sample={end},asetpts=PTS-STARTPTS"),
            "-f",
            AUDIO_PCM_FORMAT,
            "-ac",
            &AUDIO_CHANNELS.to_string(),
            "-ar",
            &AUDIO_SAMPLE_RATE.to_string(),
            // 输出到 stdout：**读的那一端**在本机会话里是通的
            //（不通的是 stdin 管道，见 plan/next-steps.md 坑 19）。
            "-",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|error| {
            format!(
                "起不了音频解码器 ffmpeg（{}）：{error}（PATH 里有 ffmpeg 吗？）",
                segment.file.display()
            )
        })?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "拿不到音频解码器的 stdout".to_string())?;
    let mut reader = BufReader::new(stdout);
    let mut buffer = vec![0u8; AUDIO_CHUNK_SAMPLES * bytes_per_sample];
    let mut written = 0i64;
    while written < segment.output_samples {
        let want = (segment.output_samples - written).min(AUDIO_CHUNK_SAMPLES as i64) as usize;
        let slice = &mut buffer[..want * bytes_per_sample];
        let mut filled = 0usize;
        while filled < slice.len() {
            match reader.read(&mut slice[filled..]) {
                Ok(0) => break,
                Ok(read) => filled += read,
                Err(error) => return Err(format!("读音频解码器失败：{error}")),
            }
        }
        let got = filled / bytes_per_sample;
        if got == 0 {
            break; // 素材到此为止 —— 调用方负责补静音。
        }
        sink(&buffer[..got * bytes_per_sample])?;
        written += got as i64;
    }
    let _ = child.wait();
    Ok(written)
}

/// 把一段 PCM **按增益相加**进已经写在盘上的那一块。
///
/// # 为什么必须读回来加，而不是"两趟各写一遍"
///
/// 两趟顺序 write 会把后面那一趟**覆盖**前面那一趟 —— 那是"只听见音效、
/// 背景人声没了"。听感上完全正常（确实有声音），所以**没人会去查**。
///
/// # 采样格式是 `f32le`（**不是 i16**）
///
/// 这一条曾经写错过：按 i16 去解 f32 的字节会得到一堆垃圾数，
/// 表现是**每一段都在疯狂削顶**（实测 48000 个采样点里削了 12059 个），
/// 而"钳位"看起来又像是在正常工作 —— 于是错得很像对的。
/// 判据是 [`crate::audio::AUDIO_PCM_FORMAT`]，它只有一个来源。
///
/// # 削顶处理
///
/// 相加后超出 `[-1, 1]` 的**钳位**而不是回绕。回绕会把一个响亮的音效变成
/// 一声爆裂的噪声，那比"稍微糊一下"难听得多的多。
///
/// 返回 `(相加的采样点数, 被钳位的采样点数)` —— **钳位那个数不为零必须看得见**：
/// 它是"该调增益了"的信号。
fn mix_segment_into(
    file: &mut std::fs::File,
    segment: &AudioSegment,
    offset_bytes: u64,
    bytes_per_sample: usize,
) -> Result<(i64, i64), String> {
    use std::io::{Seek, SeekFrom};

    /// 一个 f32 采样点占几个字节。**从格式推出来，不写死 4**（见上面的说明）。
    const F32_BYTES: usize = std::mem::size_of::<f32>();

    if bytes_per_sample != F32_BYTES * 2 {
        // 立体声 = 两个 f32。不是这个形状说明上游换了格式而这个函数没跟上 ——
        // **当场报出来**，比按错的字节宽度去算要诚实得多。
        return Err(format!(
            "混音只支持 f32le 立体声（每采样 {F32_BYTES} 字节 × 2 声道），\
             而这一趟的每采样字节数是 {bytes_per_sample} —— 与 AUDIO_PCM_FORMAT 对不上了"
        ));
    }

    let mut written = 0i64;
    let mut clipped = 0i64;
    let mut position = offset_bytes;

    // 一次读回一块、相加、再写回。块大小按帧算，所以总是整数个采样点。
    let mut existing = vec![0u8; AUDIO_CHUNK_SAMPLES * bytes_per_sample];
    extract_segment_samples(segment, |chunk| {
        let samples = chunk.len() / bytes_per_sample;
        let span = &mut existing[..chunk.len()];
        file.seek(SeekFrom::Start(position))
            .map_err(|error| format!("定位音频临时文件失败：{error}"))?;
        std::io::Read::read_exact(file, span)
            .map_err(|error| format!("读回音频临时文件失败：{error}"))?;

        // 逐**声道**采样点相加：一个采样点 = 2 个 f32（左右声道）。
        for index in 0..samples * 2 {
            let at = index * F32_BYTES;
            let base = f32::from_le_bytes([span[at], span[at + 1], span[at + 2], span[at + 3]]);
            let add = f32::from_le_bytes([chunk[at], chunk[at + 1], chunk[at + 2], chunk[at + 3]]);
            // 增益在**加法之前**乘在加数上：乘在结果上会把背景音也放大。
            let sum = base + add * segment.gain;
            // 钳到 [-1, 1]：这是 f32 归一化采样的满幅。
            let clamped = sum.clamp(-1.0, 1.0);
            if (clamped - sum).abs() > f32::EPSILON {
                clipped += 1;
            }
            let bytes = clamped.to_le_bytes();
            span[at..at + F32_BYTES].copy_from_slice(&bytes);
        }

        file.seek(SeekFrom::Start(position))
            .map_err(|error| format!("定位音频临时文件失败：{error}"))?;
        file.write_all(span)
            .map_err(|error| format!("写回混音结果失败：{error}"))?;
        position += chunk.len() as u64;
        written += samples as i64;
        Ok(())
    })?;

    Ok((written, clipped))
}

/// 把 AudioPlan 摊成一整条 PCM 轨（临时文件）。
///
/// **总长严格等于 `audio.total_samples`**：段与段之间补静音、素材不够长也补静音。
/// 于是"音轨时长 == 视频时长"是**写出来的**，不是事后对齐出来的。
///
/// # 两趟，而不是一趟（T13）
///
/// 1. **底轨**：互不重叠的那些段按时间顺序写下去，空档补静音。
///    全是顺序 write —— 这一趟与引入音效之前**逐字节相同**。
/// 2. **叠加**：与已有内容重叠的段**读回来相加**（音效）。
///
/// 分成两趟是因为它们的正确写法不同：底轨是"排好队写下去"，
/// 叠加是"读回来加"。一趟里混着做，就要在顺序写的过程中插读回，
/// 而那个交错的正确性很难用测试钉住。
///
/// **一趟都没有的时候**（`segments` 为空）仍然是"整条静音"，与从前一致。
pub fn build_audio_track(audio: &AudioPlan, pcm: &Path) -> Result<AudioStats, String> {
    let bytes_per_sample = audio_bytes_per_sample(audio.info.channels);
    let mut stats = AudioStats {
        sample_rate: audio.info.sample_rate,
        channels: audio.info.channels,
        segments: audio.segments.len(),
        expected_samples: audio.total_samples,
        ..Default::default()
    };
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(pcm)
        .map_err(|error| format!("建不了音频临时文件（{}）：{error}", pcm.display()))?;

    // ---- 第一趟：分出来哪些是"底轨"、哪些是"叠加" ----
    //
    // 判据是**这个段与它前面已放置的段有没有重叠**，而不是"它是不是音效"。
    // 靠 kind 判会让"两段背景乐不小心叠了"变成静默覆盖；
    // 靠几何判则无论来源如何都得到同一个正确答案：**重叠就是相加**。
    let mut placed_end = 0i64;
    let mut layered: Vec<&AudioSegment> = Vec::new();
    let mut cursor = 0i64;

    for segment in &audio.segments {
        if segment.output_start_sample < placed_end {
            // 与前面叠上了 —— 留给第二趟相加。
            layered.push(segment);
            continue;
        }
        // 段之前的位置：时间线上本来就没声音，补静音。
        if segment.output_start_sample > cursor {
            let gap = segment.output_start_sample - cursor;
            write_silence(&mut file, gap, bytes_per_sample)?;
            stats.gap_samples += gap;
            cursor = segment.output_start_sample;
        }
        let written = extract_segment(segment, &mut file)?;
        stats.source_samples_read += written;
        // 素材不够长 -> 这一段的后半是补的静音。**这个数不为零必须看得见。**
        stats.padded_samples += segment.output_samples - written;
        cursor += segment.output_samples;
        placed_end = cursor;
    }
    // 尾巴上的空档。
    if cursor < audio.total_samples {
        let tail = audio.total_samples - cursor;
        write_silence(&mut file, tail, bytes_per_sample)?;
        stats.gap_samples += tail;
    }
    file.flush()
        .map_err(|error| format!("收尾音频临时文件失败：{error}"))?;

    // ---- 第二趟：叠加 ----
    //
    // 没挂音效时 `layered` 是空的，**这一整段不执行** —— 于是"没有叠加"的
    // 那条路与本改动之前逐字节相同。
    for segment in layered {
        let offset = if segment.output_start_sample > 0 {
            segment.output_start_sample as u64 * bytes_per_sample as u64
        } else {
            0
        };
        let (mixed, clipped) = mix_segment_into(&mut file, segment, offset, bytes_per_sample)?;
        stats.mixed_samples += mixed;
        stats.clipped_samples += clipped;
        // 叠加的段素材不够长时，**不补静音**：补静音等于"把已经在那儿的
        // 背景音擦掉一段"，而擦掉是听不见的（静音加什么都还是原样，
        // 但补静音是**覆盖**不是相加）。这里直接不写。
        stats.source_samples_read += mixed;
        // 把"哪里叠了、叠了多久"记下来。计划层已经算过 `overlaps`，
        // 这里把**实际相加的采样点数**补上 —— 计划说"会叠"，实际说"叠了多少"。
        //
        // `overlaps` 里存的是 `轨[层]` 的路径形式（见 `audio::plan_audio`），
        // 所以按路径匹配，不是按轨名 —— 同一轨上两层叠起来也是叠。
        let path = format!("{}[{}]", segment.track, segment.layer);
        if let Some(overlap) = audio
            .overlaps
            .iter()
            .find(|o| o.first == path || o.second == path)
        {
            stats.overlaps.push(format!(
                "{} 与 {} 在第 {} 帧叠上，相加了 {} 个采样点",
                overlap.first, overlap.second, overlap.at, mixed
            ));
        }
    }
    file.flush()
        .map_err(|error| format!("收尾音频临时文件失败：{error}"))?;
    Ok(stats)
}

/// 把已经编好的视频与一条 PCM 轨复用成产物。
///
/// **视频是 `-c:v copy`**：它在上一跳已经编完了，这里一个字节都不该再动 ——
/// 重编一次等于给"视频那一半没变"这句话白加一层风险（还要多花一遍编码时间）。
///
/// `pub` 是有意的：音频这一跳**不经过 GPU、也不需要 stdin 管道**
/// （它读素材文件、写临时文件），所以它可以被脱离出片那条路地整段验。
/// 那条路上的视频编码器要 rawvideo stdin，本机会话里起不来（见 plan/next-steps.md 坑 19）。
pub fn mux_audio(video: &Path, pcm: &Path, audio: &AudioPlan, out: &Path) -> Result<(), String> {
    let status = Command::new("ffmpeg")
        .args(["-v", "error"])
        .arg("-i")
        .arg(video)
        .args([
            "-f",
            AUDIO_PCM_FORMAT,
            "-ar",
            &audio.info.sample_rate.to_string(),
            "-ac",
            &audio.info.channels.to_string(),
        ])
        .arg("-i")
        .arg(pcm)
        .args([
            "-c:v", "copy", "-c:a", "aac", "-b:a", "192k", "-movflags", "+faststart",
        ])
        .arg("-y")
        .arg(out)
        // **不给 stdin 开管道**：这条命令本来就不需要喂东西，而"给子进程开 stdin 管道"
        // 在本机会话里会 ERROR_PIPE_BUSY（见 plan/next-steps.md 坑 19）。
        // 显式写 null 是为了让"凭什么这里能跑"这个问题在代码里就有答案。
        .stdin(Stdio::null())
        .stderr(Stdio::inherit())
        .status()
        .map_err(|error| format!("起不了复用器 ffmpeg：{error}（PATH 里有 ffmpeg 吗？）"))?;
    if !status.success() {
        return Err(format!("音视频复用失败：ffmpeg 退出码 {:?}", status.code()));
    }
    Ok(())
}

/// 有音轨时，视频先落到产物**旁边**的临时文件；复用完再删掉。
///
/// 用 `.` 前缀：一眼能看出是中间产物；放在产物同目录而不是系统临时目录 ——
/// 跨盘搬 90 帧的 1080p 是把时间花在最不值的地方。
fn sidecar_path(output: &Path, suffix: &str) -> PathBuf {
    let name = output
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "out".to_string());
    output.with_file_name(format!(".{name}.{suffix}"))
}

/// 出片。on_progress(已出帧数, 总帧数)。
pub fn render_plan(
    plan: &RenderPlan,
    mut on_progress: impl FnMut(usize, usize) + Send,
) -> Result<RenderReport, String> {
    if plan.to < plan.from {
        return Err(format!("帧区间是空的：from={} to={}", plan.from, plan.to));
    }
    if plan.width == 0 || plan.height == 0 {
        return Err(format!("输出尺寸不合法：{}x{}", plan.width, plan.height));
    }
    let fps = encoder_fps(&plan.timeline.timebase)?;
    let total = (plan.to - plan.from + 1) as usize;

    // 音轨的计划在**碰 GPU 之前**就摊出来：它是纯的，因此"工程里有没有声音"
    // 这件事不该等到出片跑到一半才知道。
    let audio_plan = match plan.audio {
        AudioMode::Silent => AudioPlan::silent(plan.from, plan.to, &plan.timeline.timebase)?,
        AudioMode::Auto => crate::audio::plan_audio(
            plan.timeline,
            plan.sources,
            plan.asset_timebases,
            plan.from,
            plan.to,
        )?,
    };
    let wants_audio = !audio_plan.is_silent();
    // **没有音轨时 video_target 就是 plan.output** —— 于是无声那条路
    // 连"写到哪儿"都没变，产物与引入音频之前逐字节相同。
    let video_target = if wants_audio {
        sidecar_path(plan.output, "video.mp4")
    } else {
        plan.output.to_path_buf()
    };

    let started = Instant::now();
    let workers = resolve_workers(plan, total);

    let mut frames = 0usize;
    let mut empty_frames: Vec<Frame> = Vec::new();
    let mut issues: Vec<Issue> = Vec::new();
    let mut overlay = OverlayStats::default();
    let mut decode = PoolStats::default();
    let mut opened_streams = 0usize;

    if workers <= 1 {
        let report = render_range(plan, plan.from, plan.to, &video_target, &mut on_progress)?;
        frames = report.frames;
        empty_frames = report.empty_frames;
        issues = report.issues;
        overlay = report.overlay;
        decode = report.decode;
        opened_streams = report.opened_streams;
    } else {
        // ---- 分块并行 ----
        //
        // 每块一个线程、一套 GPU 上下文、一个 ffmpeg 进程 —— 与 参照实现 的
        // `render/pipeline.rs:173`（`parallelism` 块各一个 `thread::spawn`）同一个形状。
        // 本仓先前是单线程逐帧 `submit` + 同步读回 + 同步写管道，三者完全不重叠。
        let bounds: Vec<(Frame, Frame)> =
            (0..workers).map(|index| chunk_bounds(plan.from, plan.to, workers, index)).collect();
        let parts: Vec<PathBuf> = (0..workers)
            .map(|index| sidecar_path(plan.output, &format!("chunk_{index}.mp4")))
            .collect();

        // 进度用**一个共享计数**汇总：每块各报各的会让总数来回跳。
        let done = std::sync::atomic::AtomicUsize::new(0);
        let mut outcomes: Vec<Result<RangeReport, String>> = Vec::with_capacity(workers);
        let progress = std::sync::Mutex::new(&mut on_progress);
        std::thread::scope(|scope| {
            let handles: Vec<_> = bounds
                .iter()
                .zip(parts.iter())
                .map(|((from, to), part)| {
                    let progress = &progress;
                    let done = &done;
                    scope.spawn(move || {
                        let mut local = |_: usize, _: usize| {
                            let seen = done
                                .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                                + 1;
                            if let Ok(mut callback) = progress.lock() {
                                (**callback)(seen, total);
                            }
                        };
                        render_range(plan, *from, *to, part, &mut local)
                    })
                })
                .collect();
            for handle in handles {
                outcomes.push(
                    handle.join().unwrap_or_else(|_| Err("分块线程 panic".to_string())),
                );
            }
        });

        // 一块失败就整次失败：留着半份产物比什么都没有更容易误导人。
        let mut parts_done: Vec<PathBuf> = Vec::with_capacity(workers);
        let mut failure: Option<String> = None;
        for (index, outcome) in outcomes.into_iter().enumerate() {
            match outcome {
                Ok(report) => {
                    frames += report.frames;
                    empty_frames.extend(report.empty_frames);
                    issues.extend(report.issues);
                    decode.merge(report.decode);
                    opened_streams += report.opened_streams;
                    overlay.merge(report.overlay);
                    parts_done.push(parts[index].clone());
                }
                Err(error) => {
                    failure = Some(format!("第 {index} 块：{error}"));
                    break;
                }
            }
        }
        if let Some(error) = failure {
            for part in &parts {
                remove_quietly(part);
            }
            if wants_audio {
                remove_quietly(&video_target);
            }
            return Err(error);
        }

        let joined = concat_video(&parts_done, &video_target);
        for part in &parts {
            remove_quietly(part);
        }
        if let Err(error) = joined {
            if wants_audio {
                remove_quietly(&video_target);
            }
            return Err(error);
        }
    }

    if frames == 0 {
        if wants_audio {
            remove_quietly(&video_target);
        }
        return Err("一帧都没处理".to_string());
    }

    // 有音轨时，视频只是**中间产物**：真正的产物要等复用之后才有。
    // 所以从这里往下，凡是提前返回的路径都要把那个临时文件带走。
    let mut audio = AudioStats::default();
    if wants_audio {
        let pcm = sidecar_path(plan.output, "audio.f32");
        let muxed = build_audio_track(&audio_plan, &pcm).and_then(|stats| {
            audio = stats;
            mux_audio(&video_target, &pcm, &audio_plan, plan.output)
        });
        remove_quietly(&pcm);
        remove_quietly(&video_target);
        muxed?;
    }

    let elapsed_ms = started.elapsed().as_millis();
    // 音轨装载阶段的问题（缺素材 / 音轨上没有素材的图层 / 多轨重叠）
    // 与其它问题走同一条路：非空就是这次出片失败。
    issues.extend(audio_plan.issues.iter().cloned());
    let (width, height, encoded_frames) = probe_output_frames(plan.output)?;
    Ok(RenderReport {
        output: plan.output.to_path_buf(),
        frames,
        encoded_frames: Some(encoded_frames),
        width,
        height,
        encoder_fps: fps,
        seconds: seconds_for(frames, &plan.timeline.timebase),
        elapsed_ms,
        opened_streams,
        empty_frames,
        overlay,
        audio,
        decode,
        issues,
    })
}

/// 一段帧区间的产出。**分块并行时每块一份**，最后汇总。
#[derive(Debug, Default)]
struct RangeReport {
    frames: usize,
    empty_frames: Vec<Frame>,
    issues: Vec<Issue>,
    overlay: OverlayStats,
    decode: PoolStats,
    opened_streams: usize,
}

/// 这块要开几个 worker。
///
/// `plan.chunk_workers`：`1` = 不分块（**默认**，产物逐字节与从前相同）；
/// `0` = 自动（按可用并行度，最多 8，与 参照实现 的上限一致）；`n` = 指定 n。
///
/// # 为什么默认不分块
///
/// 分块会把一段视频切成 N 段各自编码再 concat —— **每一段开头都是一个新的 GOP**，
/// 所以产物与"一口气编码"不是逐字节相同（画面质量基本一致，但字节数会变）。
/// 本仓有一批判据钉的是"同一份工程出同样的字节"，所以默认保持单趟；
/// 要速度就显式开。
fn resolve_workers(plan: &RenderPlan, total: usize) -> usize {
    let requested = plan.chunk_workers;
    let available = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
    // **自动档的上限是 4，不是核数。**
    //
    // 实测（16 核，1920x1080 60fps，2000 帧）：
    //     workers   帧/秒   加速比
    //        1      52.1    1.00x
    //        2      74.2    1.42x
    //        3      86.5    1.66x   <- 最好
    //        4      85.2    1.60x
    //        5      74.5    1.54x
    //        6      73.7    1.52x
    //        8      67.4    1.29x   <- 比 3 慢 22%
    //
    // 只有**一块 GPU**：每个 worker 各开一套 wgpu 上下文、各自同步读回，
    // 开到 5 个以上就开始互相抢设备，"并行"变成"排队 + 额外的上下文开销"。
    // 所以自动档按 4 封顶 —— 拿核数当上限会在这台机器上白白慢 22%。
    let workers = if requested == 0 { available.min(4) } else { requested };
    // 帧数比 worker 还少时多开的线程只会互相抢设备、不会更快。
    workers.min(total).max(1)
}

/// 第 `index` 块要出的帧区间（闭区间，尽量均分）。
fn chunk_bounds(from: Frame, to: Frame, workers: usize, index: usize) -> (Frame, Frame) {
    let total = (to - from + 1) as usize;
    let base = total / workers;
    let extra = total % workers;
    // 前 `extra` 块各多一帧 —— 余数不摊掉的话最后一块会明显偏大。
    let before: usize = (0..index).map(|i| base + usize::from(i < extra)).sum();
    let len = base + usize::from(index < extra);
    let start = from + before as Frame;
    (start, start + len as Frame - 1)
}

/// 出 `[from, to]` 这一段到一个**视频**文件（不含音频）。
///
/// 这一层专门为**分块并行**而分出来：每个 worker 调一次，各自开自己的 GPU 上下文、
/// 自己的解码器池、自己的 ffmpeg 进程 —— **不共享任何可变状态**，所以能真并行。
/// 分块之间唯一的耦合是"最后要 concat"，而那一步在调用方。
fn render_range(
    plan: &RenderPlan,
    from: Frame,
    to: Frame,
    video_target: &Path,
    on_progress: &mut dyn FnMut(usize, usize),
) -> Result<RangeReport, String> {
    let fps = encoder_fps(&plan.timeline.timebase)?;
    let (ctx, _init) =
        open_leg(NATIVE_BACKENDS).map_err(|error| format!("拿不到 GPU 上下文：{error}"))?;
    let renderer = TimelineRenderer::new(&ctx.device, WORK_FORMAT);

    // 走 `io::FrameSink` 在 native 侧的形态：纹理归它、生命周期 = 一次渲染运行（与原来逐值一致）。
    let target = OffscreenFrameSink::new(&ctx.device, plan.width, plan.height, "dhampir pipeline target");

    // **先把这一趟要哪些 (源, 源内帧) 算出来**：池子靠它决定"读到的帧要不要留下"。
    // 这一步是纯的、不碰 GPU 也不碰解码器，所以它失败不了，也不会让出片慢多少。
    let schedule = request_schedule(plan.timeline, plan.asset_timebases, from, to);
    let demand = demand_of(&schedule);
    let mut sources = DecodingSources::new(&ctx.device, &ctx.queue, plan.sources, demand);
    let mut encoder = spawn_encoder(video_target, plan.width, plan.height, fps)?;
    // 字幕的账走**自己一份** IssueLog：源那边的那份在 sources 里（按 (code,path) 去重），
    // 两份在收尾时合并 —— 于是"同一行字画不下"按行内容去重，不会按帧号刷满清单。
    let mut overlay_log = IssueLog::new();
    let mut painter =
        OverlayPainter::new(plan.font_file).with_fonts(plan.font_bold_file, plan.font_dir);
    let mut frames = 0usize;
    let mut empty_frames: Vec<Frame> = Vec::new();

    let mut result: Result<(), String> = Ok(());
    for frame in from..=to {
        sources.begin_frame(frame);
        let composite =
            compose::evaluate_v2_with_assets(plan.timeline, frame, Some(plan.asset_timebases));
        // 调整图层没有素材，所以「本该有内容」只数有素材的那些层。
        let source_layers = composite
            .layers
            .iter()
            .filter(|layer| !layer.is_adjustment)
            .count();

        let mut command = ctx.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("dhampir pipeline encoder"),
        });
        let drawn = renderer.render_frame_at(
            &ctx.device,
            &ctx.queue,
            &mut command,
            target.view(),
            RenderSpace {
                sequence: plan.sequence,
                target: (plan.width, plan.height),
            },
            &composite,
            &mut sources,
            wgpu::Color::TRANSPARENT,
            // **序列时间（秒）**：Warp 的位移场以它为自变量。
            // 与浏览器侧用**同一个换算**（都在 timeline 的 `seconds_at_sequence_frame`），
            // 否则同一个工程在两个宿主的抖动相位会不一致 —— 那正是"两端可比"要防的。
            seconds_at_sequence_frame(frame, &plan.timeline.timebase).unwrap_or(0.0) as f32,
        );
        ctx.queue.submit([command.finish()]);

        if drawn == 0 && source_layers > 0 {
            // 有素材的层一层都没画出来 —— 这是「渲染成功但画面是空的」，必须浮出来。
            empty_frames.push(frame);
        }

        let mut image = match pollster::block_on(readback::read_texture_rgba8(
            &ctx.device,
            &ctx.queue,
            target.texture(),
        )) {
            Ok(image) => image,
            Err(error) => {
                result = Err(format!("第 {frame} 帧读回失败：{error}"));
                break;
            }
        };
        // 文字**叠在读回来的帧上**，再送进编码器：与 PNG 那条路用的是同一个函数、
        // 同一份结构（都在 evaluate_overlay + painter.paint 上），所以两条路不会分叉。
        if let Some(overlay) =
            evaluate_overlay(plan.timeline, frame, plan.sequence, Some(plan.subtitles))
        {
            painter.paint(
                &mut image,
                &overlay,
                (plan.width, plan.height),
                &mut overlay_log,
            );
        }
        let stdin = match encoder.stdin.as_mut() {
            Some(stdin) => stdin,
            None => {
                result = Err("拿不到编码器的 stdin".to_string());
                break;
            }
        };
        if let Err(error) = stdin.write_all(&image.pixels) {
            result = Err(format!("第 {frame} 帧写进编码器失败：{error}"));
            break;
        }
        frames += 1;
        on_progress(frames, (to - from + 1) as usize);
    }

    // 无论成功还是中途退出，都要收干净：**不关编码器的 stdin，它会一直等**。
    let _ = encoder.stdin.take();
    let _ = encoder.wait();
    // 路数与账要在 close() **之前**读 —— close 会清掉 streams，读晚了就永远是 0。
    let opened_streams = sources.opened_streams();
    let decode = sources.stats();
    sources.close();
    // `issues(self)` **消费** self，所以它必须排在最后（close 之后、读数之后）。
    let source_issues = sources.issues();

    // 失败时把这一块的中间产物带走：留在盘上会让人以为"这次成功过"。
    if let Err(error) = result {
        remove_quietly(video_target);
        return Err(error);
    }
    if frames == 0 {
        remove_quietly(video_target);
        return Err("一帧都没处理".to_string());
    }

    let mut issues = source_issues;
    issues.extend(overlay_log.into_vec());
    Ok(RangeReport {
        frames,
        empty_frames,
        issues,
        overlay: painter.stats(),
        decode,
        opened_streams,
    })
}

/// 把若干段**同参数**编码出来的 mp4 接成一个。
///
/// 用 concat demuxer 而不是 concat filter：这些段是同一套编码参数逐段编出来的，
/// demuxer 直接拼流、**不重编码**（重编码会再压一代，也会把刚省下的时间还回去）。
fn concat_video(parts: &[PathBuf], output: &Path) -> Result<(), String> {
    if parts.is_empty() {
        return Err("没有可拼接的分块".to_string());
    }
    let list_path = sidecar_path(output, "concat.txt");
    let mut list = String::new();
    for part in parts {
        // **必须是绝对路径。**
        //
        // concat demuxer 把 `file` 行里的相对路径按**清单文件所在目录**解析 ——
        // 而清单就放在输出旁边（`out/chunks/`），于是 `out/chunks/.x.chunk_0.mp4`
        // 会被拼成 `out/chunks/out/chunks/.x.chunk_0.mp4`。
        // 报出来的错是"Impossible to open"，看起来像"分块没生成"，其实路径被叠了一次。
        let absolute = std::fs::canonicalize(part)
            .map_err(|error| format!("分块 {} 不在：{error}", part.display()))?;
        // concat demuxer 的 `file` 行：单引号包住、内部单引号要转义成 '\''。
        let text = absolute.display().to_string().replace('\'', "'\\''");
        list.push_str(&format!("file '{text}'\n"));
    }
    std::fs::write(&list_path, list).map_err(|error| format!("写分块清单失败：{error}"))?;
    let result = (|| -> Result<(), String> {
        let output_std = output.as_os_str().to_os_string();
        let status = std::process::Command::new("ffmpeg")
            .args(["-v", "error", "-f", "concat", "-safe", "0", "-i"])
            .arg(&list_path)
            // `-c copy`：拼的是同一套参数的裸流，不重编码。
            .args(["-c", "copy", "-movflags", "+faststart", "-y"])
            .arg(&output_std)
            .status()
            .map_err(|error| format!("起不了 ffmpeg：{error}"))?;
        if !status.success() {
            return Err(format!("拼接分块失败（ffmpeg 退出码 {status}）"));
        }
        Ok(())
    })();
    remove_quietly(&list_path);
    result
}

/// 删中间产物。**失败不报错** —— 它只是个临时文件，为它把一次已经成功的出片
/// 判失败是本末倒置（真正该看的是产物本身）。
fn remove_quietly(path: &Path) {
    let _ = std::fs::remove_file(path);
}

/// 一张 PNG 的出图结果。
#[derive(Debug, Clone)]
pub struct FramePng {
    pub frame: Frame,
    pub path: PathBuf,
    /// 写出去那份像素的 FNV-1a 摘要（**含字幕**：先叠再摘，摘的是落地的东西）。
    pub digest: String,
    /// 字幕那边到现在为止的账（**累计**，从这次调用的第一帧算起 ——
    /// frame 子命令一次只要一帧，所以它就是这一帧的数）。
    pub overlay: OverlayStats,
    /// 这一帧的字幕问题。非空 = 这张图里的字幕不对，调用方该判失败。
    pub issues: Vec<Issue>,
}

/// 只出几帧 PNG 这一趟的全部产物：帧 + **解码侧的账**。
///
/// 与出片那条路报的是**同一组事实**（[`PoolStats`] 与 `opened_streams`）——
/// 这不是"给测试用的脚手架"：两条路走的是同一个 [`DecodingSources`]，
/// 一条报得出解码账、另一条报不出，就会让人以为"PNG 那条不解码"，
/// 而"看不见的东西不会有人去量"（T5.3 要的并发数与读写代价正是从这里来的）。
#[derive(Debug, Clone)]
pub struct PngRun {
    pub frames: Vec<FramePng>,
    /// 命中 / 向前 / **重启** / 一共读了多少源帧。
    pub decode: PoolStats,
    /// 这一趟开了几路解码器（= 工程里引用到的源数）。
    pub opened_streams: usize,
}

/// 只出几帧 PNG（给 CLI 的 frame 子命令与调试用）。
///
/// 走**同一条**求值/合成/读回路径，只是 sink 换成 PNG 文件 ——
/// 另起一条「只画一帧」的路就会让 render 与 frame 从那天起开始分叉。
/// 字幕也**走同一条**：evaluate_overlay + painter.paint，与出片那条路径一份实现。
///
/// 输出目录取计划里 output 的父目录；文件名是 frame-<帧号>.png。
pub fn render_frames_png(plan: &RenderPlan, frames: &[Frame]) -> Result<Vec<FramePng>, String> {
    render_frames_png_run(plan, frames).map(|run| run.frames)
}

/// 与 [`render_frames_png`] 同一条路，只是把**解码侧的账**也交出来。
pub fn render_frames_png_run(
    plan: &RenderPlan,
    frames: &[Frame],
) -> Result<PngRun, String> {
    if plan.width == 0 || plan.height == 0 {
        return Err(format!("输出尺寸不合法：{}x{}", plan.width, plan.height));
    }
    let (ctx, _init) =
        open_leg(NATIVE_BACKENDS).map_err(|error| format!("拿不到 GPU 上下文：{error}"))?;
    let renderer = TimelineRenderer::new(&ctx.device, WORK_FORMAT);
    let target = OffscreenFrameSink::new(&ctx.device, plan.width, plan.height, "dhampir frame target");
    // 与出片那条路**同一条**规矩：需求先算出来，再交给池子。
    // 一次只要一帧时需求就是那一帧，池子于是退化成"直接读过去、只留那一帧"。
    // 取 min/max 而不是 first/last：调用方给的帧号不保证有序，
    // 而 `from..=to` 反着给会**悄悄变成空集**（于是池子不留任何帧）。
    let demand = match (
        frames.iter().copied().min(),
        frames.iter().copied().max(),
    ) {
        (Some(first), Some(last)) => demand_of(&request_schedule(
            plan.timeline,
            plan.asset_timebases,
            first,
            last,
        )),
        _ => HashMap::new(),
    };
    let mut sources = DecodingSources::new(&ctx.device, &ctx.queue, plan.sources, demand);

    let dir = plan
        .output
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();
    std::fs::create_dir_all(&dir).map_err(|e| format!("建不了目录 {}：{e}", dir.display()))?;

    let mut painter = OverlayPainter::new(plan.font_file)
        .with_fonts(plan.font_bold_file, plan.font_dir);
    let mut written = Vec::new();
    for frame in frames {
        let frame = *frame;
        sources.begin_frame(frame);
        let composite =
            compose::evaluate_v2_with_assets(plan.timeline, frame, Some(plan.asset_timebases));
        let mut command = ctx.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("dhampir frame encoder"),
        });
        renderer.render_frame(
            &ctx.device,
            &ctx.queue,
            &mut command,
            target.view(),
            RenderSpace {
                sequence: plan.sequence,
                target: (plan.width, plan.height),
            },
            &composite,
            &mut sources,
            wgpu::Color::TRANSPARENT,
        );
        ctx.queue.submit([command.finish()]);
        let mut image = pollster::block_on(readback::read_texture_rgba8(
            &ctx.device,
            &ctx.queue,
            target.texture(),
        ))
        .map_err(|error| format!("第 {frame} 帧读回失败：{error}"))?;
        let mut overlay_log = IssueLog::new();
        if let Some(overlay) =
            evaluate_overlay(plan.timeline, frame, plan.sequence, Some(plan.subtitles))
        {
            painter.paint(
                &mut image,
                &overlay,
                (plan.width, plan.height),
                &mut overlay_log,
            );
        }
        // 摘要在**叠完之后**取：它就是落进 PNG 的那些字节。
        let digest = dhampir_core::timeline::selfcheck::fnv1a64(&image.pixels);
        let path = dir.join(format!("frame-{frame:04}.png"));
        image
            .write_png(&path)
            .map_err(|e| format!("写 PNG 失败：{e:?}"))?;
        written.push(FramePng {
            frame,
            path,
            digest: format!("{digest:016x}"),
            overlay: painter.stats(),
            issues: overlay_log.into_vec(),
        });
    }
    // 账要在 close 之前取：close 会把 streams 清掉。
    let decode = sources.stats();
    let opened_streams = sources.opened_streams();
    sources.close();
    Ok(PngRun {
        frames: written,
        decode,
        opened_streams,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 分块必须**不重不漏**地盖住整个区间 —— 漏一帧成片就短一帧，
    /// 重一帧就会在拼接处看到同一帧闪两下。
    ///
    /// 余数最容易分错：5812 / 4 = 1453 整除，看不出问题；
    /// 5813 / 4 = 1453 余 1，前三块必须各多一帧，否则最后一块会大一帧。
    #[test]
    fn 分块不重不漏地盖住整个区间() {
        for total in [1usize, 2, 3, 7, 2000, 5812, 5813] {
            let from = 100i64;
            let to = from + total as i64 - 1;
            for workers in 1..=8usize {
                let workers = workers.min(total);
                let parts: Vec<(Frame, Frame)> =
                    (0..workers).map(|i| chunk_bounds(from, to, workers, i)).collect();
                // 第一块从头开始
                assert_eq!(parts[0].0, from, "total={total} workers={workers} 起点不对");
                // 最后一块到尾结束
                assert_eq!(parts[workers - 1].1, to, "total={total} workers={workers} 终点不对");
                // 首尾相接、不重不漏
                for pair in parts.windows(2) {
                    assert_eq!(
                        pair[0].1 + 1,
                        pair[1].0,
                        "total={total} workers={workers} 在 {}..{} 之间有缝或重叠",
                        pair[0].1,
                        pair[1].0
                    );
                }
                // 帧数合计必须等于总数
                let counted: i64 = parts.iter().map(|(a, b)| b - a + 1).sum();
                assert_eq!(counted, total as i64, "total={total} workers={workers} 帧数对不上");
            }
        }
    }

    fn tb(num: u32, den: u32) -> TimebaseDto {
        TimebaseDto { num, den }
    }

    /// 需求计数表：测试里"每一帧只要一次"的那种工程。
    fn once(frames: &[Frame]) -> BTreeMap<Frame, usize> {
        frames.iter().map(|frame| (*frame, 1usize)).collect()
    }

    /// 用 JSON 造时间线：`Layer` 有十几个字段，逐字段写结构字面量会让测试
    /// 因为**无关字段**变红。走契约自己的反序列化，形状与工程文件一致。
    fn timeline_of(text: &str) -> TimelineV2 {
        serde_json::from_str(text).expect("测试用的时间线 JSON 必须载得进来")
    }

    #[test]
    fn 池子里的帧直接命中_向前就从游标接着读() {
        // 帧 0 要被要**两次**（同一个输出帧里两层用同一帧）——留得住的前提就是这个：
        // 需求表说它等一下还会被要。
        let mut demand = once(&[0, 1, 2, 3, 4]);
        demand.insert(0, 2);
        let mut pool = PoolCursor::new(4, demand);
        // 第一帧：从 0 读到 0。它还要被要一次，所以留进池子。
        let plan = pool.plan(0);
        assert_eq!(plan.action, FetchAction::Forward);
        assert_eq!(plan.reads, vec![0]);
        pool.commit(&plan);
        assert_eq!(pool.cursor(), 1);
        assert_eq!(pool.kept(), &[0]);
        // 再要同一帧：命中，游标不动。
        let plan = pool.plan(0);
        assert_eq!(plan.action, FetchAction::Hit);
        assert!(plan.reads.is_empty());
        pool.commit(&plan);
        assert_eq!(pool.cursor(), 1);
        // 这一次要完，需求用尽 -> 出池子。**"用完了就扔"是顺序出片不涨内存的原因。**
        assert!(pool.kept().is_empty());
        // 向前两帧：路上那帧（1）等一下还要被要，就留；目标帧（2）这一次马上用掉，不留。
        let plan = pool.plan(2);
        assert_eq!(plan.action, FetchAction::Forward);
        assert_eq!(plan.reads, vec![1, 2]);
        assert_eq!(plan.keep, vec![1]);
        pool.commit(&plan);
        assert_eq!(pool.cursor(), 3);
        assert_eq!(pool.kept(), &[1]);
        assert_eq!(pool.stats().hits, 1);
        assert_eq!(pool.stats().forward, 2);
        assert_eq!(pool.stats().frames_read, 3);
    }

    #[test]
    fn 回退是重启从头再读而不是报错() {
        // 帧 1 要被要两次（后面那一层回头用它）；其余各一次。
        let mut demand = once(&[0, 1, 2, 3, 4, 5, 6, 7]);
        demand.insert(1, 2);
        let mut pool = PoolCursor::new(3, demand);
        let plan = pool.plan(7);
        pool.commit(&plan);
        assert_eq!(pool.stats().frames_read, 8);
        // 池子只有 3 槽：先来先走，留下的是最后读到的那三帧。
        assert_eq!(pool.kept(), &[4, 5, 6]);
        // 要一个被挤掉的旧帧 —— **不是错误**，是从头再读一遍。
        let plan = pool.plan(1);
        assert_eq!(plan.action, FetchAction::Replay);
        assert_eq!(plan.reads, vec![0, 1]);
        pool.commit(&plan);
        assert_eq!(pool.stats().replays, 1);
        // 游标回到刚读到的位置后面，不是留在 8。
        assert_eq!(pool.cursor(), 2);
        assert_eq!(pool.stats().frames_read, 8 + 2);
        // 而且**要的那一帧真的给出来了**：它还在池子里（还欠一次请求）。
        assert!(pool.kept().contains(&1));
    }

    #[test]
    fn 同一输出帧里同一个源要两帧也给得出来() {
        // 这一条正是原来 source_frame_conflict 挡掉的东西（同素材画中画、同素材转场）：
        // 一层要 58、另一层要 10，**同一个源、同一个输出帧**。
        let demand = once(&[58, 10]);

        // 池子装得下：读向 58 的路上顺手把 10 留下（需求表说它等一下要被要），
        // 于是第二次是命中 —— 一帧都不用多读。
        let mut pool = PoolCursor::new(8, demand.clone());
        let plan = pool.plan(58);
        assert_eq!(plan.action, FetchAction::Forward);
        pool.commit(&plan);
        assert_eq!(pool.plan(10).action, FetchAction::Hit);
        assert_eq!(pool.stats().replays, 0);
        assert_eq!(pool.stats().frames_read, 59);

        // 一槽池子也一样给得出来：路上要留的**只有 10 这一帧**，一槽就够。
        let mut tight = PoolCursor::new(1, demand);
        let plan = tight.plan(58);
        tight.commit(&plan);
        assert_eq!(tight.plan(10).action, FetchAction::Hit);
        assert_eq!(tight.stats().replays, 0);

        // 但**来回要很多次**时一槽就露怯了：留得住这一对里的一个，
        // 就留不住下一个，于是每次都得从头再读。
        let mut churn = PoolCursor::new(1, {
            let mut counts = once(&[58, 10]);
            counts.insert(58, 2);
            counts.insert(10, 2);
            counts
        });
        let plan = churn.plan(58);
        churn.commit(&plan);
        let plan = churn.plan(10);
        assert_eq!(plan.action, FetchAction::Replay);
        assert_eq!(plan.reads, (0..=10).collect::<Vec<Frame>>());
        churn.commit(&plan);
        assert_eq!(churn.stats().replays, 1);
        assert_eq!(churn.stats().frames_read, 59 + 11);
    }

    #[test]
    fn 不在需求里的帧不留进池子() {
        // 源 60fps、时间线 30fps：中间那些奇数帧永远不会被要第二次。
        let demand = once(&[0, 2, 4]);
        let mut pool = PoolCursor::new(16, demand);
        let plan = pool.plan(4);
        assert_eq!(plan.reads, vec![0, 1, 2, 3, 4]);
        // 两条规则一起看：**不在需求里的**（1、3）不留，
        // **这一趟用掉的**（目标 4，只欠一次）也不留。
        assert_eq!(plan.keep, vec![0, 2]);
        pool.commit(&plan);
        assert_eq!(pool.kept(), &[0, 2]);
    }

    #[test]
    fn 池子容量至少一帧() {
        // 「容量为零」不是"不缓存"，是"永远给不出旧帧" —— 那是另一回事，不许悄悄发生。
        // 给 0 槽会被夹到 1 槽，而 1 槽确实留得住一帧：留的是**还欠一次请求**的那帧。
        let mut demand = once(&[0, 1]);
        demand.insert(0, 2);
        let mut pool = PoolCursor::new(0, demand);
        let plan = pool.plan(1);
        pool.commit(&plan);
        assert_eq!(pool.kept(), &[0]);
        assert_eq!(pool.plan(0).action, FetchAction::Hit);
    }

    #[test]
    fn 池子大小按字节封顶而不是按帧数() {
        // 640x360（0.92 MB/帧）与 1920x1080（8.29 MB/帧）装得下的帧数差一个量级。
        let small = pool_slots(640, 360);
        let big = pool_slots(1920, 1080);
        assert!(small > big, "{small} 应该大于 {big}");
        assert!(big >= 2);
        assert!(small <= POOL_SLOTS_MAX);
        // 尺寸为零时不是 panic，也不是 0 槽。
        assert_eq!(pool_slots(0, 0), 1);
    }

    #[test]
    fn 取帧顺序与渲染器真会请求的顺序一致() {
        // 层序即 texture_for 的调用序：轨道序 -> 层序。这条断言是为了让量化脚本
        // 数出来的回退次数**就是产品跑出来的那个数**。
        let timeline = timeline_of(
            r#"{
              "schema": 3,
              "timebase": { "num": 30, "den": 1 },
              "tracks": [
                { "id": "v1", "kind": "video", "layers": [
                    { "id": "a", "start": 0, "end": 30,
                      "source": { "asset_id": "a.mp4", "source_in": 0 } } ] },
                { "id": "v2", "kind": "video", "layers": [
                    { "id": "b", "start": 0, "end": 30,
                      "source": { "asset_id": "b.mp4", "source_in": 100 } } ] }
              ]
            }"#,
        );
        let rows = request_schedule(&timeline, &AssetTimebases::new(), 0, 1);
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[0].source, "a.mp4");
        assert_eq!(rows[1].source, "b.mp4");
        assert_eq!(rows[2].frame, 1);
        assert_eq!(rows[3].source_frame, 101);
    }

    #[test]
    fn 调整图层不出现在取帧计划里() {
        // 调整图层没有素材（source 是空串），它不该被当成"要读第 0 帧"。
        let timeline = timeline_of(
            r#"{
              "schema": 3,
              "timebase": { "num": 30, "den": 1 },
              "tracks": [
                { "id": "v1", "kind": "video", "layers": [
                    { "id": "a", "start": 0, "end": 10,
                      "source": { "asset_id": "a.mp4", "source_in": 3 } } ] },
                { "id": "v2", "kind": "video", "layers": [
                    { "id": "f", "start": 0, "end": 10,
                      "effects": [ { "kind": "gaussian_blur", "params": { "radius": 4 } } ] } ] }
              ]
            }"#,
        );
        let rows = request_schedule(&timeline, &AssetTimebases::new(), 0, 9);
        assert_eq!(rows.len(), 10);
        assert!(rows.iter().all(|row| row.source == "a.mp4"));
    }

    #[test]
    fn 需求表按源分开且去重() {
        let rows = vec![
            SourceRequest {
                frame: 0,
                source: "a".to_string(),
                source_frame: 10,
            },
            SourceRequest {
                frame: 1,
                source: "a".to_string(),
                source_frame: 10,
            },
            SourceRequest {
                frame: 1,
                source: "b".to_string(),
                source_frame: 3,
            },
        ];
        let demand = demand_of(&rows);
        assert_eq!(demand["a"].len(), 1);
        // 要了两次就是两次 —— 池子靠这个数决定"用完了没有"。
        assert_eq!(demand["a"][&10], 2);
        assert_eq!(demand["b"].keys().copied().collect::<Vec<Frame>>(), vec![3]);
    }

    #[test]
    fn 编码帧率跟着工程的时间基走() {
        assert_eq!(encoder_fps(&tb(30, 1)).unwrap(), 30.0);
        assert!((encoder_fps(&tb(30000, 1001)).unwrap() - 29.970_029).abs() < 1e-4);
        // 分母为 0 是坏的工程，不是「默认 30」。
        assert!(encoder_fps(&tb(30, 0)).is_err());
        assert!(encoder_fps(&tb(0, 1)).is_err());
    }

    #[test]
    fn 秒数由整数帧号与时间基算出() {
        // 90 帧 @30fps = 3 秒整。
        assert!((seconds_for(90, &tb(30, 1)) - 3.0).abs() < 1e-12);
        assert!((seconds_for(0, &tb(30, 1))).abs() < 1e-12);
    }

    #[test]
    fn 送给编码器的是有理数而不是浮点() {
        assert_eq!(ffmpeg_rate(&tb(30, 1)), "30/1");
        assert_eq!(ffmpeg_rate(&tb(30000, 1001)), "30000/1001");
    }

    #[test]
    fn 一帧裸像素的字节数() {
        assert_eq!(frame_bytes(640, 360), 640 * 360 * 4);
        assert_eq!(frame_bytes(1920, 1080), 8_294_400);
    }

    #[test]
    fn 问题按代码与路径去重并封顶() {
        // 用一条**产品真的会发**的码：拿已经不再产生的码（比如从前的 source_rewind）
        // 当例子，会让人以为那个缺陷还在。
        let mut log = IssueLog::new();
        for _ in 0..90 {
            log.record(
                "unknown_asset",
                "frame[3].source[a.mp4]",
                "素材表里没有它".to_string(),
            );
        }
        // 90 次同一个缺陷 -> 只留一条。
        let issues = log.into_vec();
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].code, "unknown_asset");
    }

    #[test]
    fn 超出上限的同类问题只计数不刷屏() {
        let mut log = IssueLog::new();
        for index in 0..(MAX_ISSUES + 5) {
            log.record("x", &format!("path[{index}]"), "m".to_string());
        }
        assert_eq!(log.suppressed(), 5);
        let issues = log.into_vec();
        // 上限条 + 一条汇总。
        assert_eq!(issues.len(), MAX_ISSUES + 1);
        assert_eq!(issues[MAX_ISSUES].code, "issues_suppressed");
    }

    #[test]
    fn 素材表按_id_取位置_没登记就是没有() {
        let mut table = SourceTable::new();
        table.insert("a.mp4", "target/s3/proxy1080p.mp4");
        assert_eq!(table.len(), 1);
        assert!(table.file_for("a.mp4").is_some());
        // 不许「猜一个同名文件」：没登记就是 None。
        assert!(table.file_for("b.mp4").is_none());
    }

    #[test]
    fn 无声路径的命令行没有变() {
        // **这是一条冻结测试。** T6 引入音频之后，"没有音轨的工程产物逐字节不变"
        // 这句话就靠它成立：argv 一样、输入一样、编码器一样，产物就一样。
        // 它红了的唯一合理原因是**你故意改了视频编码参数** ——
        // 那时请连同这条一起改，并想清楚"已经出过的片子要不要重出"。
        let args = encoder_args(1920, 1080, 30.0, Path::new("out.mp4"));
        let text: Vec<String> = args
            .iter()
            .map(|value| value.to_string_lossy().to_string())
            .collect();
        assert_eq!(
            text,
            vec![
                "-v",
                "error",
                "-f",
                "rawvideo",
                "-pix_fmt",
                "rgba",
                "-s",
                "1920x1080",
                "-r",
                "30",
                "-i",
                "-",
                "-c:v",
                "libx264",
                "-preset",
                "veryfast",
                "-crf",
                "20",
                "-pix_fmt",
                "yuv420p",
                "-movflags",
                "+faststart",
                "-y",
                "out.mp4",
            ]
        );
    }

    #[test]
    fn 有音轨时视频那一跳只换了落点() {
        // 音频那一跳**不许动视频的编码参数**：它只是把已经编好的东西换个地方放。
        let silent = encoder_args(640, 360, 30.0, Path::new("out.mp4"));
        let with_audio = encoder_args(640, 360, 30.0, Path::new(".out.mp4.video.mp4"));
        assert_eq!(silent.len(), with_audio.len());
        assert_eq!(
            silent[..silent.len() - 1],
            with_audio[..with_audio.len() - 1],
            "除了落点，参数必须逐项相同"
        );
        assert_ne!(silent[silent.len() - 1], with_audio[with_audio.len() - 1]);
    }

    #[test]
    fn 中间产物落在产物旁边而且带点前缀() {
        assert_eq!(
            sidecar_path(Path::new("target/t6/film.mp4"), "video.mp4"),
            PathBuf::from("target/t6/.film.mp4.video.mp4")
        );
        assert_eq!(
            sidecar_path(Path::new("film.mp4"), "audio.f32"),
            PathBuf::from(".film.mp4.audio.f32")
        );
        // 不带扩展名的产物也不能把路径拼坏。
        assert_eq!(
            sidecar_path(Path::new("out"), "audio.f32"),
            PathBuf::from(".out.audio.f32")
        );
    }

    #[test]
    fn 音频每采样点的字节数按声道算() {
        assert_eq!(audio_bytes_per_sample(1), 4);
        assert_eq!(audio_bytes_per_sample(2), 8);
        // 零声道是坏输入，但不许除零 —— 当单声道处理，让错误在别处浮出来。
        assert_eq!(audio_bytes_per_sample(0), 4);
    }

    #[test]
    fn 报告判据把空帧与问题都算作失败() {
        let base = RenderReport {
            output: PathBuf::from("x.mp4"),
            frames: 90,
            encoded_frames: Some(90),
            width: 640,
            height: 360,
            encoder_fps: 30.0,
            seconds: 3.0,
            elapsed_ms: 1,
            opened_streams: 4,
            empty_frames: Vec::new(),
            overlay: OverlayStats::default(),
            audio: AudioStats::default(),
            decode: PoolStats::default(),
            issues: Vec::new(),
        };
        assert!(!base.failed());
        // 帧数不符 -> 失败。
        let short = RenderReport {
            encoded_frames: Some(89),
            ..base.clone()
        };
        assert!(short.failed());
        // 数不出来 -> 失败（不是「没验」当「通过」）。
        let unknown = RenderReport {
            encoded_frames: None,
            ..base.clone()
        };
        assert!(unknown.failed());
        // 有空的合成帧 -> 失败。
        let empty = RenderReport {
            empty_frames: vec![3],
            ..base.clone()
        };
        assert!(empty.failed());
        // 有问题清单 -> 失败。
        let dirty = RenderReport {
            issues: vec![Issue::new("unknown_asset", "p", "m".to_string())],
            ..base.clone()
        };
        assert!(dirty.failed());
    }
}
