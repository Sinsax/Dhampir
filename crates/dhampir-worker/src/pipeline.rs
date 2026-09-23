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
//! 这条约束有一个**必然的代价**，必须写清楚而不是藏着：
//!
//! > **每一路源只能向前推进。** 工程如果要求某个 source 回退到一个已经读过去的帧
//! > （例如同一素材的两个片段在时间线上前后颠倒），这一路**给不出那一帧**。
//!
//! 遇到回退时本模块**报错并让整次出片失败**，而不是悄悄少画一层或者画错一层。
//! 静默降级正是这个项目最要避免的失效模式。
//!
//! # 另一条限制：一路源一张纹理
//!
//! SourceResolver 给出的是「这一帧上这个 source 用哪张纹理」。所以同一输出帧里
//! 同一个 source 只能有**一个**源内帧。两个片段引用同一素材的不同帧、又同时可见时，
//! 记为 source_frame_conflict 并失败。真要做需要多张纹理 + 多路解码器，
//! 那是另一个数量级的改动，这里如实不做。
//!
//! # 音轨
//!
//! **不渲染。** 本模块只出视频。有音轨就在 stderr 明说，不给一份「看起来很成功」的哑片。

use std::collections::{BTreeSet, HashMap};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::time::Instant;

use dhampir_core::compose;
use dhampir_core::gpu::NATIVE_BACKENDS;
use dhampir_core::readback;
use dhampir_core::render::{RenderSpace, SourceResolver, TimelineRenderer};
use dhampir_core::timeline::layer::{AssetTimebases, TimelineV2};
use dhampir_core::timeline::schema::{Frame, Issue, TimebaseDto};
use dhampir_core::wgpu;

use crate::baseline::open_leg;

/// 解码器吐出来的像素格式。**两条路与两个宿主都用它**，别在这里换格式：
/// 换格式等于给「两端同一个渲染图」这句话加一个未验证的转换。
pub const DECODE_PIXEL_FORMAT: &str = "rgba";

/// 渲染目标与源纹理的格式。与 examples/decode_sequence.rs 保持一致。
pub const WORK_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

// ---------------------------------------------------------------------------
// 纯逻辑：先写这些，因为它们能被自检与真跑同时走到
// ---------------------------------------------------------------------------

/// 一次 texture_for 该做什么。
///
/// 抽成纯函数是有意的：自检与真跑走**同一段判定**，否则自检验的是另一套规则。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Advance {
    /// 纹理里已经是这一帧了，直接用。
    Cached,
    /// 向前读到目标帧。
    Forward,
    /// 目标帧在已经读过去的位置 —— **顺序解码给不出来**。
    Rewind,
    /// 同一帧上这个源已经被要求过另一个源内帧。
    Conflict,
}

/// 判定「这一路源该做什么」。
///
/// * next_frame：解码器**下一段字节**对应的帧号（读过的帧都小于它）；
/// * uploaded：纹理里当前是哪一帧（从没上传过是 None）；
/// * served：本次输出帧上这个源已经被服务过的帧号。
pub fn plan_advance(
    next_frame: Frame,
    uploaded: Option<Frame>,
    target: Frame,
    served: Option<Frame>,
) -> Advance {
    if let Some(already) = served {
        // 同一帧里重复要同一个源内帧是合法的（同一素材的多个片段各占一层）。
        if already == target {
            return Advance::Cached;
        }
        return Advance::Conflict;
    }
    if uploaded == Some(target) {
        return Advance::Cached;
    }
    if target < next_frame {
        return Advance::Rewind;
    }
    Advance::Forward
}

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
        Self { seen: BTreeSet::new(), issues: Vec::new(), suppressed: 0 }
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
        Self { entries: HashMap::new() }
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
        let mut rows: Vec<(String, PathBuf)> =
            self.entries.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        rows.sort();
        rows
    }
}

// ---------------------------------------------------------------------------
// 一路源的顺序解码器
// ---------------------------------------------------------------------------

/// 一路素材的解码器 + 它那一张复用纹理。
///
/// **纹理必须复用**：1080p 每帧 8 MB，90 帧里每帧新建一次就是 720 MB 的分配抖动，
/// 而且会把「两端的差异」混进分配顺序的差异里。
struct SourceStream {
    source: String,
    file: PathBuf,
    child: Child,
    stdout: ChildStdout,
    width: u32,
    height: u32,
    frame_bytes: usize,
    /// 下一段字节对应的帧号。
    next_frame: Frame,
    /// 纹理里当前是哪一帧。
    uploaded_frame: Option<Frame>,
    texture: wgpu::Texture,
    view: wgpu::TextureView,
}

/// 问一次媒体尺寸。
///
/// **csv 的列序不是请求顺序**（这个坑踩过一次：把宽度当成了帧数）。
/// 这里只要两个字段，并且**断言字段数**，字段数不对就报错而不是猜。
fn probe_size(file: &Path) -> Result<(u32, u32), String> {
    let output = Command::new("ffprobe")
        .args([
            "-v", "error",
            "-select_streams", "v:0",
            "-show_entries", "stream=width,height",
            "-of", "csv=p=0",
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
    let width: u32 = fields[0].trim().parse().map_err(|_| format!("宽度不是数：{}", fields[0]))?;
    let height: u32 = fields[1].trim().parse().map_err(|_| format!("高度不是数：{}", fields[1]))?;
    if width == 0 || height == 0 {
        return Err(format!("ffprobe 给出了 {width}x{height}，无法按帧切分"));
    }
    Ok((width, height))
}

impl SourceStream {
    fn open(device: &wgpu::Device, source: &str, file: &Path) -> Result<Self, String> {
        if !file.exists() {
            return Err(format!("素材文件不在：{}", file.display()));
        }
        let (width, height) = probe_size(file)?;

        // 解码器：吐裸 RGBA。
        //
        // **色彩矩阵必须显式。** 不写的话 FFmpeg 从容器元数据里猜，而浏览器
        // （WebCodecs）也有一套自己的猜法，两边默认值不一定相同（BT.601 vs 709），
        // 同一帧看起来就偏色 —— 而那不是渲染 bug。
        // 这条也被 check-sequential-decode 守卫盯着，别删。
        let mut child = Command::new("ffmpeg")
            .args(["-v", "error", "-i"])
            .arg(file)
            .args([
                "-vf", "scale=out_color_matrix=bt709",
                "-f", "rawvideo",
                "-pix_fmt", DECODE_PIXEL_FORMAT,
                "-",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|error| format!("起不了 ffmpeg：{error}（PATH 里有 ffmpeg 吗？）"))?;
        let stdout = child.stdout.take().ok_or("拿不到解码器的 stdout")?;

        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("dhampir pipeline source"),
            size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: WORK_FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

        Ok(Self {
            source: source.to_string(),
            file: file.to_path_buf(),
            child,
            stdout,
            width,
            height,
            frame_bytes: frame_bytes(width, height),
            next_frame: 0,
            uploaded_frame: None,
            texture,
            view,
        })
    }

    /// 向前推进到 target。**只许向前** —— 调用方已经用 plan_advance 判过。
    fn advance_to(&mut self, queue: &wgpu::Queue, target: Frame) -> Result<(), String> {
        if self.uploaded_frame == Some(target) {
            return Ok(());
        }
        if target < self.next_frame {
            return Err(format!(
                "源 {} 要回退到第 {target} 帧，而解码器已经在第 {} 帧之后（顺序解码给不出来）",
                self.source, self.next_frame
            ));
        }
        let mut buffer = vec![0u8; self.frame_bytes];
        while self.next_frame <= target {
            match self.stdout.read_exact(&mut buffer) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
                    return Err(format!(
                        "源 {}（{}）在第 {} 帧就结束了，而工程要第 {target} 帧",
                        self.source,
                        self.file.display(),
                        self.next_frame
                    ));
                }
                Err(error) => {
                    return Err(format!(
                        "读源 {} 的第 {} 帧失败：{error}",
                        self.source, self.next_frame
                    ));
                }
            }
            // 中间的帧**只读不传**：省一次 8 MB 的 PCIe 往返，而像素本来就要丢掉。
            if self.next_frame == target {
                queue.write_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture: &self.texture,
                        mip_level: 0,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    &buffer,
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
                self.uploaded_frame = Some(target);
            }
            self.next_frame += 1;
        }
        Ok(())
    }

    /// 收工。解码器还没读到 EOF，直接杀掉而不是等它跑完整条流。
    fn close(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// ---------------------------------------------------------------------------
// 按工程解析源的 resolver
// ---------------------------------------------------------------------------

/// 把「asset_id -> 文件」变成「这一帧上这个 source 用哪张纹理」。
pub struct DecodingSources<'a> {
    device: &'a wgpu::Device,
    queue: &'a wgpu::Queue,
    table: &'a SourceTable,
    streams: HashMap<String, SourceStream>,
    /// 开失败过的源。记下来是为了**不要每帧重试一次并刷屏**。
    failed: BTreeSet<String>,
    log: IssueLog,
    current_frame: Frame,
    /// 本次输出帧上已经服务过的 (source, 源内帧)。
    served: Vec<(String, Frame)>,
}

impl<'a> DecodingSources<'a> {
    pub fn new(device: &'a wgpu::Device, queue: &'a wgpu::Queue, table: &'a SourceTable) -> Self {
        Self {
            device,
            queue,
            table,
            streams: HashMap::new(),
            failed: BTreeSet::new(),
            log: IssueLog::new(),
            current_frame: 0,
            served: Vec::new(),
        }
    }

    /// 进入下一个输出帧。同一帧内重复的 (source, frame) 会被当作缓存命中。
    pub fn begin_frame(&mut self, frame: Frame) {
        self.current_frame = frame;
        self.served.clear();
    }

    /// 已经开起来的解码器路数。给报告用。
    pub fn opened_streams(&self) -> usize {
        self.streams.len()
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

    fn served_frame(&self, source: &str) -> Option<Frame> {
        self.served
            .iter()
            .find(|(name, _)| name == source)
            .map(|(_, frame)| *frame)
    }

    fn path_of(&self, source: &str) -> String {
        format!("frame[{}].source[{}]", self.current_frame, source)
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
        match SourceStream::open(self.device, source, &file) {
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
        let served = self.served_frame(source);
        let (next_frame, uploaded) = match self.streams.get(source) {
            Some(stream) => (stream.next_frame, stream.uploaded_frame),
            None => (0, None),
        };

        // 判定先做完，再动状态 —— 借用与改动的边界摆在这儿，别混在一起。
        match plan_advance(next_frame, uploaded, source_frame, served) {
            Advance::Cached => {
                let stream = self.streams.get(source)?;
                return Some((stream.view.clone(), (stream.width, stream.height)));
            }
            Advance::Conflict => {
                let path = self.path_of(source);
                let already = served.unwrap_or_default();
                self.log.record(
                    "source_frame_conflict",
                    &path,
                    format!(
                        "第 {} 帧上素材 {source} 同时被要求第 {already} 帧与第 {source_frame} 帧；\
                         一路源只有一张纹理，后端不做回退解码",
                        self.current_frame
                    ),
                );
                return None;
            }
            Advance::Rewind => {
                let path = self.path_of(source);
                self.log.record(
                    "source_rewind",
                    &path,
                    format!(
                        "第 {} 帧要素材 {source} 的源内第 {source_frame} 帧，而解码器已经在第 {next_frame} 帧之后。\
                         顺序解码只能向前 —— 请把引用同一素材的片段按时间顺序排列",
                        self.current_frame
                    ),
                );
                return None;
            }
            Advance::Forward => {}
        }

        if !self.ensure_stream(source) {
            return None;
        }
        // 开了解码器之后判定可能变了（原本没流时 next_frame 是 0）。
        let next_frame = self.streams.get(source)?.next_frame;
        if source_frame < next_frame {
            let path = self.path_of(source);
            self.log.record(
                "source_rewind",
                &path,
                format!(
                    "第 {} 帧要素材 {source} 的第 {source_frame} 帧，而它已经读过去了",
                    self.current_frame
                ),
            );
            return None;
        }

        let stream = self.streams.get_mut(source)?;
        if let Err(error) = stream.advance_to(self.queue, source_frame) {
            let path = self.path_of(source);
            self.log.record("source_decode_failed", &path, error);
            return None;
        }
        self.served.push((source.to_string(), source_frame));
        let stream = self.streams.get(source)?;
        Some((stream.view.clone(), (stream.width, stream.height)))
    }
}

// ---------------------------------------------------------------------------
// 出片
// ---------------------------------------------------------------------------

/// 一次出片的入参。
pub struct RenderPlan<'a> {
    pub timeline: &'a TimelineV2,
    pub sources: &'a SourceTable,
    /// 素材 id → 时间基。**源帧号靠它换算**（见 compose::evaluate_v2_with_assets）。
    /// 传空的不是错 —— 那是"假设素材帧率与时间线一致"的旧语义，
    /// 但素材帧率真的不同时画面会变速，所以调用方应当把工程文件的资产表带上。
    pub asset_timebases: &'a AssetTimebases,
    /// 闭区间 [from, to]，单位整数帧。
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
    pub output: &'a Path,
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

/// 起编码器。裸流没有尺寸与帧率信息，必须显式告诉它。
fn spawn_encoder(plan: &RenderPlan, fps: f64) -> Result<Child, String> {
    if let Some(parent) = plan.output.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| format!("建不了输出目录：{e}"))?;
        }
    }
    Command::new("ffmpeg")
        .args(["-v", "error", "-f", "rawvideo", "-pix_fmt", DECODE_PIXEL_FORMAT])
        .arg("-s")
        .arg(format!("{}x{}", plan.width, plan.height))
        .args(["-r", &format!("{fps}"), "-i", "-"])
        .args([
            "-c:v", "libx264", "-preset", "veryfast", "-crf", "20",
            "-pix_fmt", "yuv420p", "-movflags", "+faststart",
        ])
        .arg("-y")
        .arg(plan.output)
        .stdin(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|error| format!("起不了编码器 ffmpeg：{error}（PATH 里有 ffmpeg 吗？）"))
}

/// 数一遍产物里的帧。**列序是 width,height,nb_read_frames** —— 帧数在第三列。
fn probe_output_frames(path: &Path) -> Result<(u32, u32, usize), String> {
    let output = Command::new("ffprobe")
        .args([
            "-v", "error",
            "-select_streams", "v:0",
            "-count_frames",
            "-show_entries", "stream=nb_read_frames,width,height",
            "-of", "csv=p=0",
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
    let width: u32 = fields[0].trim().parse().map_err(|_| "宽度不是数".to_string())?;
    let height: u32 = fields[1].trim().parse().map_err(|_| "高度不是数".to_string())?;
    let frames: usize = fields[2].trim().parse().map_err(|_| "帧数不是数".to_string())?;
    Ok((width, height, frames))
}

/// 出片。on_progress(已出帧数, 总帧数)。
pub fn render_plan(
    plan: &RenderPlan,
    mut on_progress: impl FnMut(usize, usize),
) -> Result<RenderReport, String> {
    if plan.to < plan.from {
        return Err(format!("帧区间是空的：from={} to={}", plan.from, plan.to));
    }
    if plan.width == 0 || plan.height == 0 {
        return Err(format!("输出尺寸不合法：{}x{}", plan.width, plan.height));
    }
    let fps = encoder_fps(&plan.timeline.timebase)?;
    let total = (plan.to - plan.from + 1) as usize;

    let (ctx, _init) =
        open_leg(NATIVE_BACKENDS).map_err(|error| format!("拿不到 GPU 上下文：{error}"))?;
    let renderer = TimelineRenderer::new(&ctx.device, WORK_FORMAT);

    let target = ctx.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("dhampir pipeline target"),
        size: wgpu::Extent3d { width: plan.width, height: plan.height, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: WORK_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let target_view = target.create_view(&wgpu::TextureViewDescriptor::default());

    let mut sources = DecodingSources::new(&ctx.device, &ctx.queue, plan.sources);
    let mut encoder = spawn_encoder(plan, fps)?;
    let started = Instant::now();
    let mut frames = 0usize;
    let mut empty_frames: Vec<Frame> = Vec::new();

    let mut result: Result<(), String> = Ok(());
    for frame in plan.from..=plan.to {
        sources.begin_frame(frame);
        let composite = compose::evaluate_v2_with_assets(
            plan.timeline,
            frame,
            Some(plan.asset_timebases),
        );
        // 调整图层没有素材，所以「本该有内容」只数有素材的那些层。
        let source_layers = composite.layers.iter().filter(|layer| !layer.is_adjustment).count();

        let mut command = ctx.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("dhampir pipeline encoder"),
        });
        let drawn = renderer.render_frame(
            &ctx.device,
            &ctx.queue,
            &mut command,
            &target_view,
            RenderSpace { sequence: plan.sequence, target: (plan.width, plan.height) },
            &composite,
            &mut sources,
            wgpu::Color::TRANSPARENT,
        );
        ctx.queue.submit([command.finish()]);

        if drawn == 0 && source_layers > 0 {
            // 有素材的层一层都没画出来 —— 这是「渲染成功但画面是空的」，必须浮出来。
            empty_frames.push(frame);
        }

        let image = match pollster::block_on(readback::read_texture_rgba8(
            &ctx.device,
            &ctx.queue,
            &target,
        )) {
            Ok(image) => image,
            Err(error) => {
                result = Err(format!("第 {frame} 帧读回失败：{error}"));
                break;
            }
        };
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
        on_progress(frames, total);
    }

    // 无论成功还是中途退出，都要收干净：**不关编码器的 stdin，它会一直等**。
    let _ = encoder.stdin.take();
    let _ = encoder.wait();
    // 路数要在 close() **之前**读 —— close 会清掉 streams，读晚了就永远是 0。
    let opened_streams = sources.opened_streams();
    sources.close();

    if let Err(error) = result {
        return Err(error);
    }
    if frames == 0 {
        return Err("一帧都没处理".to_string());
    }

    let elapsed_ms = started.elapsed().as_millis();
    let issues = sources.issues();
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
        issues,
    })
}

/// 只出几帧 PNG（给 CLI 的 frame 子命令与调试用）。
///
/// 走**同一条**求值/合成/读回路径，只是 sink 换成 PNG 文件 ——
/// 另起一条「只画一帧」的路就会让 render 与 frame 从那天起开始分叉。
///
/// 输出目录取计划里 output 的父目录；文件名是 frame-<帧号>.png。
pub fn render_frames_png(
    plan: &RenderPlan,
    frames: &[Frame],
) -> Result<Vec<(Frame, PathBuf, String)>, String> {
    if plan.width == 0 || plan.height == 0 {
        return Err(format!("输出尺寸不合法：{}x{}", plan.width, plan.height));
    }
    let (ctx, _init) =
        open_leg(NATIVE_BACKENDS).map_err(|error| format!("拿不到 GPU 上下文：{error}"))?;
    let renderer = TimelineRenderer::new(&ctx.device, WORK_FORMAT);
    let target = ctx.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("dhampir frame target"),
        size: wgpu::Extent3d { width: plan.width, height: plan.height, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: WORK_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let target_view = target.create_view(&wgpu::TextureViewDescriptor::default());
    let mut sources = DecodingSources::new(&ctx.device, &ctx.queue, plan.sources);

    let dir = plan.output.parent().unwrap_or_else(|| Path::new(".")).to_path_buf();
    std::fs::create_dir_all(&dir).map_err(|e| format!("建不了目录 {}：{e}", dir.display()))?;

    let mut written = Vec::new();
    for frame in frames {
        let frame = *frame;
        sources.begin_frame(frame);
        let composite = compose::evaluate_v2_with_assets(
            plan.timeline,
            frame,
            Some(plan.asset_timebases),
        );
        let mut command = ctx.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("dhampir frame encoder"),
        });
        renderer.render_frame(
            &ctx.device,
            &ctx.queue,
            &mut command,
            &target_view,
            RenderSpace { sequence: plan.sequence, target: (plan.width, plan.height) },
            &composite,
            &mut sources,
            wgpu::Color::TRANSPARENT,
        );
        ctx.queue.submit([command.finish()]);
        let image = pollster::block_on(readback::read_texture_rgba8(
            &ctx.device,
            &ctx.queue,
            &target,
        ))
        .map_err(|error| format!("第 {frame} 帧读回失败：{error}"))?;
        let digest = dhampir_core::timeline::selfcheck::fnv1a64(&image.pixels);
        let path = dir.join(format!("frame-{frame:04}.png"));
        image.write_png(&path).map_err(|e| format!("写 PNG 失败：{e:?}"))?;
        written.push((frame, path, format!("{digest:016x}")));
    }
    sources.close();
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tb(num: u32, den: u32) -> TimebaseDto {
        TimebaseDto { num, den }
    }

    #[test]
    fn 顺序解码只许向前_回退要被判出来() {
        // 纹理里是第 9 帧，解码器已经读到第 10 帧之后；要第 5 帧 —— 给不出来。
        assert_eq!(plan_advance(10, Some(9), 5, None), Advance::Rewind);
        // 要的正是纹理里那一帧：缓存命中，不算回退。
        assert_eq!(plan_advance(10, Some(9), 9, None), Advance::Cached);
        // 向前。
        assert_eq!(plan_advance(10, Some(9), 20, None), Advance::Forward);
        // 同一帧内重样的 (source, frame) 合法。
        assert_eq!(plan_advance(10, Some(9), 9, Some(9)), Advance::Cached);
        // 同一帧内同一个源的另一个源内帧：一个源一张纹理，做不到。
        assert_eq!(plan_advance(10, Some(9), 20, Some(9)), Advance::Conflict);
        // 还没开流（next_frame=0, uploaded=None）：第 0 帧是向前的第一步。
        assert_eq!(plan_advance(0, None, 0, None), Advance::Forward);
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
        let mut log = IssueLog::new();
        for _ in 0..90 {
            log.record("source_rewind", "frame[3].source[a.mp4]", "回退".to_string());
        }
        // 90 次同一个缺陷 -> 只留一条。
        let issues = log.into_vec();
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].code, "source_rewind");
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
            issues: Vec::new(),
        };
        assert!(!base.failed());
        // 帧数不符 -> 失败。
        let short = RenderReport { encoded_frames: Some(89), ..base.clone() };
        assert!(short.failed());
        // 数不出来 -> 失败（不是「没验」当「通过」）。
        let unknown = RenderReport { encoded_frames: None, ..base.clone() };
        assert!(unknown.failed());
        // 有空的合成帧 -> 失败。
        let empty = RenderReport { empty_frames: vec![3], ..base.clone() };
        assert!(empty.failed());
        // 有问题清单 -> 失败。
        let dirty = RenderReport {
            issues: vec![Issue::new("source_rewind", "p", "m".to_string())],
            ..base.clone()
        };
        assert!(dirty.failed());
    }
}
