//! M1 corpus：core 的五个确定性场景 → PNG + 逐点判定 + 摘要。
//!
//! 与 M0 的 [`crate::offscreen`] 是同一个形状（建纹理 → 渲染 → 读回 → 判定 → 写记录），
//! 差别只在"画什么"。**本模块不发明场景**：名字、尺寸、清屏色、入口、采样点全部来自
//! [`SELECTABLE_SCENES`]——记录里出现的每个坐标都能在 core 里查到出处。M0 已经因为
//! "记录写一套坐标、断言查另一套"吃过一次亏，这里不重演。
//!
//! # 一帧画两遍
//!
//! 每一帧**渲染两次**（两块纹理、两条命令缓冲），比字节。这不是浪费：M1 的退出标准
//! 就是"重复运行逐字节一致"，而一致性必须由**代码**去比，不能由"我看着一样"来宣布。
//! 两次不一致时**照记不误**——那是一个发现（驱动？后端？提交顺序？），不是失败，
//! 更不该用颜色断言把它盖过去：判定在这种帧上直接留空（`null`），
//! 因为"没验"和"验过通过"是两件不同的事。
//!
//! # 尺寸
//!
//! corpus 一律按 [`SceneSpec::size`]（256×256）渲染：采样点的坐标是按这个尺寸定的，
//! `expected_bytes` 也只在这个尺寸上成立。1080p 的计时用的是
//! [`SceneRenderer::new_at`]，在 [`crate::baseline`] 里——两处的尺寸是**两个不同的问题**，
//! 不要为了让记录好看而把它们统一。

use std::path::{Path, PathBuf};

use dhampir_core::gpu::GpuContext;
use dhampir_core::readback::{self, Rgba8Image};
use dhampir_core::render::{
    BYTE_TOLERANCE, SCENE_TARGET_FORMAT, SCENE_TARGET_SIZE, SELECTABLE_SCENES, SamplePoint,
    SampleVerdict, SceneRenderer, SceneSpec, expected_bytes, judge_sample, scene_by_name,
};
use dhampir_core::timeline::fnv1a64;
use dhampir_core::wgpu;

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
// 一帧的读数与判定
// ---------------------------------------------------------------------------

/// 一个采样点的**声明 + 实测 + 判定**。
///
/// `measured` 与 `verdict` 都可能是 `None`，两者的含义**不同**：
///
/// - `measured: None` —— 坐标越界。模型算得出期望值，硬件那边没这个点，是硬缺陷。
/// - `verdict: None` —— 这一帧没被判（重复渲染两次不一致，或整帧作废）。
///   `measured` 照样留着：**发现要能被看见**，不能因为"判不了"就把读数丢了。
#[derive(Clone, Debug)]
pub struct PointReading {
    pub point: SamplePoint,
    pub measured: Option<[u8; 4]>,
    pub verdict: Option<SampleVerdict>,
}

/// 一帧的产物：像素摘要、PNG 字节、逐点读数。
///
/// 留着**编码后的 PNG 字节**而不是 `Rgba8Image`：写文件只需要字节，判定在渲染时
/// 就已经做完了——再留一份 256 KiB 的原始像素，只是为了将来某天能用上。记录里要的是
/// 文件，不是内存里的中间物。
pub struct SceneFrame {
    pub spec: &'static SceneSpec,
    pub frame: u32,
    /// 第一次渲染的像素字节摘要（FNV-1a 64，喂的是**紧密打包**的 RGBA8，
    /// 不是带行填充的拷贝缓冲）。
    pub digest: u64,
    /// 第二次渲染的像素摘要。与 `digest` 相等才算"同进程内逐字节一致"。
    pub repeat_digest: u64,
    /// PNG **文件字节**的摘要。退出标准说的是"跑出来的 PNG 逐字节一致"，
    /// 所以文件本身也要有一个可比的数——只比像素的话，编码器换了就没人发现。
    pub png_digest: u64,
    pub png: Vec<u8>,
    pub points: Vec<PointReading>,
}

impl SceneFrame {
    /// 同帧两次渲染是否逐字节相同。
    pub fn repeat_identical(&self) -> bool {
        self.digest == self.repeat_digest
    }

    /// 记录里那一行自报家门用的名字，例如 `gradient f003`。
    pub fn label(&self) -> String {
        format!("{} f{:03}", self.spec.name, self.frame)
    }
}

/// 一次 corpus 运行的全部帧。
pub struct SceneRun {
    pub frames: Vec<SceneFrame>,
}

impl SceneRun {
    /// 同帧两次渲染**不一致**的那些帧。
    ///
    /// 返回名字而不是布尔：不一致是发现，发现要能指名道姓——"有一帧不对"这句话
    /// 没法让人去查。
    pub fn in_process_mismatches(&self) -> Vec<String> {
        self.frames
            .iter()
            .filter(|f| !f.repeat_identical())
            .map(SceneFrame::label)
            .collect()
    }

    /// 整轮（所有场景 × 所有帧）的摘要，跨进程比对用。
    ///
    /// 喂进去的是"场景名 + 帧号 + 像素摘要"，顺序由 [`SceneRun::frames`] 决定
    /// （注册表顺序 × 帧号递增）——顺序一变摘要就变，所以它同时也是"跑的顺序没变"
    /// 的一条证据。**刻意不喂 PNG 字节**：像素才是渲染的结果，PNG 是它的编码；
    /// 两者都在记录里，比对时分开看，归因才有地方落脚。
    pub fn frames_digest(&self) -> u64 {
        frames_digest(&self.frames)
    }

    /// 逐点统计。（帧数、点数、失败数、越界数、未判定数）
    pub fn counts(&self) -> Counts {
        let mut counts = Counts {
            frames: self.frames.len(),
            ..Counts::default()
        };
        for frame in &self.frames {
            for reading in &frame.points {
                counts.points += 1;
                match (&reading.verdict, reading.measured) {
                    (_, None) => counts.out_of_range += 1,
                    (None, Some(_)) => counts.unjudged += 1,
                    (Some(v), Some(_)) if !v.passed => counts.failed += 1,
                    (Some(_), Some(_)) => {}
                }
            }
        }
        counts
    }
}

/// [`SceneRun::counts`] 的结果。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub frames: usize,
    pub points: usize,
    pub failed: usize,
    pub out_of_range: usize,
    pub unjudged: usize,
}

impl Counts {
    /// 三个"不干净"的桶都空着，才算这一次跑干净了。
    ///
    /// 收敛成一个布尔是**给退出码用的**；记录里三个数照样分开写——"判错了"
    /// （`failed`）与"校验不了"（`unjudged`）的归因方向完全不同，
    /// 合成一个布尔就再也分不开了。
    ///
    /// 空集合（`frames == 0`）在这里**算干净**——它不是一个错误结论。拦住"零帧记录"
    /// 是别处的事（[`parse_frames`] 不许空区间，调用方也不许什么都不选）。
    pub fn clean(&self) -> bool {
        self.failed == 0 && self.out_of_range == 0 && self.unjudged == 0
    }
}

/// 把若干帧按固定规则摘要成一个 64 位整数。见 [`SceneRun::frames_digest`]。
pub fn frames_digest(frames: &[SceneFrame]) -> u64 {
    let mut bytes = Vec::with_capacity(frames.len() * 24);
    for frame in frames {
        bytes.extend_from_slice(frame.spec.name.as_bytes());
        bytes.push(0);
        bytes.extend_from_slice(&frame.frame.to_le_bytes());
        bytes.extend_from_slice(&frame.digest.to_le_bytes());
    }
    fnv1a64(&bytes)
}

// ---------------------------------------------------------------------------
// 渲染
// ---------------------------------------------------------------------------

/// 渲染一帧 corpus 场景并读回。
///
/// 每次都用**全新的纹理**：复用同一块纹理会把"上一帧的残留"和"这一帧真的画对了"
/// 混在一起，而 M1 要判的正是后者。
fn render_frame(
    ctx: &GpuContext,
    renderer: &SceneRenderer,
    frame: u32,
) -> Result<Rgba8Image, Box<dyn std::error::Error>> {
    let (width, height) = renderer.size();
    let texture = ctx.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("dhampir scene target"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: SCENE_TARGET_FORMAT,
        // COPY_SRC 是读回的前提：canvas 纹理通常没有这个用途，这也正是
        // "wasm 侧不要从 canvas 抄像素"（M2）在 native 这边的同一条约束。
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

    let mut encoder = ctx
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("dhampir scene encoder"),
        });
    renderer.render(&mut encoder, &ctx.queue, &view, frame);
    ctx.queue.submit([encoder.finish()]);

    Ok(pollster::block_on(readback::read_texture_rgba8(
        &ctx.device,
        &ctx.queue,
        &texture,
    ))?)
}

/// 跑一组场景 × 一个帧区间，出图、读回、逐点判定。
///
/// 管线与 uniform **每场景建一次**（不是每帧一次）：这才是真实运行的样子
/// （M4 导出也是一个场景连续出多帧），而且"换帧不需要重建管线"这件事因此被真的走到。
pub fn run_scenes(
    ctx: &GpuContext,
    specs: &[&'static SceneSpec],
    frames: (u32, u32),
) -> Result<SceneRun, Box<dyn std::error::Error>> {
    let mut rendered = Vec::new();

    for spec in specs {
        let renderer = SceneRenderer::new(&ctx.device, SCENE_TARGET_FORMAT, spec);
        for frame in frames.0..frames.1 {
            let first = render_frame(ctx, &renderer, frame)?;
            let second = render_frame(ctx, &renderer, frame)?;
            let digest = fnv1a64(&first.pixels);
            let repeat_digest = fnv1a64(&second.pixels);
            let points = judge_frame(spec, frame, &first, digest == repeat_digest);
            let png = first
                .encode_png()
                .map_err(|e| std::io::Error::other(e.to_string()))?;
            rendered.push(SceneFrame {
                spec,
                frame,
                digest,
                repeat_digest,
                png_digest: fnv1a64(&png),
                png,
                points,
            });
        }
    }

    Ok(SceneRun { frames: rendered })
}

/// 判定一帧的全部采样点。**纯函数**（不碰 GPU、不碰文件），所以能被单测钉住。
///
/// `repeat_identical` 为假时**不判**：那一帧的读数已经不是一个可信的观测了，
/// 再用颜色断言去判它，等于用一个坏观测的结论去覆盖"这帧本身就不稳定"这个发现。
pub fn judge_frame(
    spec: &SceneSpec,
    frame: u32,
    image: &Rgba8Image,
    repeat_identical: bool,
) -> Vec<PointReading> {
    spec.samples
        .iter()
        .map(|point| {
            let point = *point;
            let measured = image.pixel(point.x, point.y);
            let verdict = match (repeat_identical, measured) {
                (true, Some(rgba)) => Some(judge_sample(spec, frame, point, rgba)),
                _ => None,
            };
            PointReading {
                point,
                measured,
                verdict,
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// 写记录
// ---------------------------------------------------------------------------

/// 一帧在记录目录里的相对路径，`/` 分隔（跨平台一致）。
///
/// 帧号固定三位：记录里的文件按**字典序**排要与帧号顺序一致，否则人一眼扫过去
/// 看到的是 `f10` 排在 `f2` 前面——那种记录没人会认真读。
pub fn frame_rel_path(spec_name: &str, frame: u32) -> String {
    format!("frames/{spec_name}-f{frame:03}.png")
}

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

/// 逐点读数的人读版报告。
///
/// `run.json` 里已经有结构化的一份；这一份存在是因为**给人看的记录和给程序看的记录
/// 不是同一种东西**：前者要能直接读出一行行"哪一帧、哪个点、实测多少、模型多少、
/// 差几个字节"，后者要能稳定地被比对。
pub fn report_text(run: &SceneRun) -> String {
    let mut out = String::new();
    let counts = run.counts();

    out.push_str("dhampir M1 corpus 逐点读数\n");
    out.push_str(&format!(
        "帧 {}、采样点 {}、失败 {}、越界 {}、未判定 {}；容差 {} 字节\n\n",
        counts.frames,
        counts.points,
        counts.failed,
        counts.out_of_range,
        counts.unjudged,
        dhampir_core::render::BYTE_TOLERANCE,
    ));

    for frame in &run.frames {
        out.push_str(&format!(
            "--- {} （{}；同帧两次渲染{}）\n",
            frame.label(),
            frame.spec.description,
            if frame.repeat_identical() {
                "逐字节一致".to_string()
            } else {
                format!("不一致：{:016x} vs {:016x}", frame.digest, frame.repeat_digest)
            }
        ));
        for reading in &frame.points {
            let point = reading.point;
            match (&reading.verdict, reading.measured) {
                (Some(verdict), Some(measured)) => {
                    out.push_str(&verdict.report_line(frame.spec, frame.frame, point, measured));
                }
                (_, None) => out.push_str(&format!(
                    "{:<12} f{:<3} ({:>3},{:>3}) {:<14} 越界——采样表里的坐标落在图像外",
                    frame.spec.name, frame.frame, point.x, point.y, point.label,
                )),
                (None, Some(measured)) => out.push_str(&format!(
                    "{:<12} f{:<3} ({:>3},{:>3}) {:<14} 实测 {:>3} {:>3} {:>3} {:>3} \
                     | 未判定：同帧两次渲染结果不一致",
                    frame.spec.name,
                    frame.frame,
                    point.x,
                    point.y,
                    point.label,
                    measured[0],
                    measured[1],
                    measured[2],
                    measured[3],
                )),
            }
            out.push('\n');
        }
        out.push('\n');
    }
    out
}

/// 一帧的结构化记录（`run.json` 里的 `frames[]`）。
pub fn frame_json(frame: &SceneFrame) -> serde_json::Value {
    let points: Vec<serde_json::Value> = frame.points.iter().map(point_json).collect();
    serde_json::json!({
        "scene": frame.spec.name,
        "frame": frame.frame,
        "png": frame_rel_path(frame.spec.name, frame.frame),
        "pixel_digest": format!("{:016x}", frame.digest),
        "repeat_pixel_digest": format!("{:016x}", frame.repeat_digest),
        "repeat_identical": frame.repeat_identical(),
        "png_digest": format!("{:016x}", frame.png_digest),
        "png_bytes": frame.png.len(),
        "points": points,
    })
}

/// 一个采样点的结构化记录。
///
/// `passed` 是三态的：`true` / `false` / `null`（没判）。**"没判"绝不能写成 `true`。**
pub fn point_json(reading: &PointReading) -> serde_json::Value {
    let point = reading.point;
    let (expected, distance, tolerance, passed, detail) = match &reading.verdict {
        Some(v) => (
            serde_json::Value::from(v.expected.to_vec()),
            serde_json::Value::from(v.distance),
            serde_json::Value::from(v.tolerance),
            serde_json::Value::from(v.passed),
            serde_json::Value::from(v.detail.clone()),
        ),
        None => (
            serde_json::Value::Null,
            serde_json::Value::Null,
            serde_json::Value::from(dhampir_core::render::BYTE_TOLERANCE),
            serde_json::Value::Null,
            serde_json::Value::from(if reading.measured.is_none() {
                "越界：采样坐标落在图像外".to_string()
            } else {
                "未判定：同帧两次渲染结果不一致".to_string()
            }),
        ),
    };
    serde_json::json!({
        "label": point.label,
        "x": point.x,
        "y": point.y,
        "purpose": point.purpose,
        "measured": reading.measured.map(|p| p.to_vec()),
        "expected": expected,
        "distance": distance,
        "tolerance": tolerance,
        "passed": passed,
        "detail": detail,
    })
}

/// 场景注册表的结构化记录：**记录要能自解释**，复核的人不该被迫去读 core 的源码
/// 才知道"gradient 是在考什么、几趟、尺寸多少"。
pub fn scene_json(spec: &SceneSpec) -> serde_json::Value {
    let samples: Vec<serde_json::Value> = spec
        .samples
        .iter()
        .map(|s| {
            serde_json::json!({
                "label": s.label,
                "x": s.x,
                "y": s.y,
                "purpose": s.purpose,
                "expected": expected_bytes(spec, 0, *s).to_vec(),
            })
        })
        .collect();
    serde_json::json!({
        "name": spec.name,
        "description": spec.description,
        "size": format!("{}x{}", spec.size.0, spec.size.1),
        "passes": spec.pass_count(),
        "fragment_entries": spec.fragment_entries(),
        "uses_frame": spec.uses_frame,
        "samples": samples,
    })
}

/// 一条腿的 `run.json`：**这个后端这次画出了什么**。**纯函数**。
///
/// 一条腿一份、写在自己的目录里，而不是把两个后端塞进同一个文件。M0 的
/// `run.json` 是一个文件装两个后端，靠**文件名**区分（`probe-native-dx12.png`）；
/// 到 M1，一条腿有 80 张图，靠文件名区分已经不够了——一条腿一个目录，
/// 目录里这份 `run.json` 说的就是这一条腿。
///
/// 形状是刻意对齐 [`compare_runs`] 的：它按 `backends[]` 里的 `requested` /
/// `frames_digest` / `frames[]` 逐个比，所以这里必须给出一个**只有一条**的
/// `backends` 数组。于是"跨进程比对"不需要知道目录结构，只需要两份 JSON。
///
/// `nondeterministic_fields` 是**空的**——这不是漏填：corpus 这一份里没有任何
/// 一项被允许变化，退出标准那句"重复运行逐字节一致"说的就是它。会变的东西
/// （时间戳、计时、adapter）在 `adapter.json` / `timing.json` 里，各有各的声明。
pub fn leg_json(
    run: &SceneRun,
    specs: &[&'static SceneSpec],
    requested: &str,
    adapter_name: Option<&str>,
    frames: (u32, u32),
) -> serde_json::Value {
    let counts = run.counts();
    let frames_json: Vec<serde_json::Value> = run.frames.iter().map(frame_json).collect();
    let scenes: Vec<serde_json::Value> = specs.iter().map(|s| scene_json(s)).collect();

    serde_json::json!({
        "schema": 1,
        "milestone": "M1",
        "kind": "corpus",
        "frame_range": format!("{}..{}", frames.0, frames.1),
        "frames_per_scene": frames.1 - frames.0,
        "target_size": format!("{}x{}", SCENE_TARGET_SIZE.0, SCENE_TARGET_SIZE.1),
        "target_format": format!("{SCENE_TARGET_FORMAT:?}"),
        "byte_tolerance": BYTE_TOLERANCE,
        // 场景注册表自述：记录要能自解释，复核的人不该被迫去读 core 的源码
        // 才知道"gradient 在考什么"。
        "scenes": scenes,
        "artifacts": {
            "adapter": "adapter.json",
            "readings": "readings.txt",
            "frames_dir": "frames",
            "frame_count": run.frames.len(),
            // `timing` 这一项由调用方在真写了计时表之后才插进来：
            // 记录里**不许**出现一个并不存在的文件名。
        },
        "backends": [{
            "requested": requested,
            "adapter_name": adapter_name,
            "frames_digest": format!("{:016x}", run.frames_digest()),
            "counts": {
                "frames": counts.frames,
                "points": counts.points,
                "failed": counts.failed,
                "out_of_range": counts.out_of_range,
                "unjudged": counts.unjudged,
                "clean": counts.clean(),
            },
            "repeat_mismatches": run.in_process_mismatches(),
            "frames": frames_json,
        }],
        "nondeterministic_fields": [],
    })
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

    /// 帧文件名必须**按字典序 == 按帧号序**。这条到第 100 帧才会显形，
    /// 而那正是没人会去看记录的时候。
    #[test]
    fn frame_paths_sort_in_numeric_order() {
        assert_eq!(frame_rel_path("gradient", 0), "frames/gradient-f000.png");
        assert_eq!(frame_rel_path("blur", 15), "frames/blur-f015.png");
        let mut names = vec![
            frame_rel_path("gradient", 2),
            frame_rel_path("gradient", 10),
            frame_rel_path("gradient", 1),
        ];
        names.sort();
        assert_eq!(
            names,
            vec![
                frame_rel_path("gradient", 1),
                frame_rel_path("gradient", 2),
                frame_rel_path("gradient", 10),
            ]
        );
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

    /// `Counts::clean` 是给退出码用的收敛——三个桶各自都要被算进去。
    #[test]
    fn clean_requires_all_three_buckets_empty() {
        let clean = Counts {
            frames: 1,
            points: 4,
            ..Counts::default()
        };
        assert!(clean.clean());

        for dirty in [
            Counts { failed: 1, ..clean },
            Counts { out_of_range: 1, ..clean },
            Counts { unjudged: 1, ..clean },
        ] {
            assert!(!dirty.clean(), "{dirty:?} 不该算干净");
        }
    }

    /// 一条腿的记录要能被 [`compare_runs`] 读、也要能被人读。
    #[test]
    fn a_leg_record_is_shaped_for_the_comparison() {
        let spec = scene_by_name("checker").unwrap();
        let run = model_run(spec, 0, true);
        let json = leg_json(&run, &[spec], "DX12", Some("测试 adapter"), (0, 16));

        // 比对要读的那几个键必须真的在、且必须对得上。
        let backend = &json["backends"][0];
        assert_eq!(backend["requested"], "DX12");
        assert_eq!(backend["adapter_name"], "测试 adapter");
        assert_eq!(
            backend["frames_digest"],
            format!("{:016x}", run.frames_digest())
        );
        assert_eq!(backend["frames"][0]["scene"], "checker");
        assert_eq!(backend["frames"][0]["frame"], 0);
        assert_eq!(backend["frames"][0]["png"], "frames/checker-f000.png");
        assert_eq!(
            backend["frames"][0]["png_digest"],
            format!("{:016x}", fnv1a64(&run.frames[0].png))
        );

        // 干净的那一轮：三个桶都是 0，`clean` 为真，没有不一致的帧。
        assert!(backend["counts"]["clean"].as_bool().unwrap());
        assert_eq!(backend["counts"]["failed"], 0);
        assert_eq!(backend["counts"]["frames"], 1);
        assert!(backend["repeat_mismatches"].as_array().unwrap().is_empty());

        // corpus 这一份里**不许**有非确定项——有的话，"重复运行逐字节一致"
        // 这条退出标准就失去意义了。会变的东西在 adapter/timing 那两份里。
        assert!(json["nondeterministic_fields"].as_array().unwrap().is_empty());
        assert_eq!(json["frame_range"], "0..16");
        assert_eq!(json["frames_per_scene"], 16);
        assert_eq!(json["scenes"][0]["name"], "checker");
        assert_eq!(json["artifacts"]["frames_dir"], "frames");
        assert_eq!(json["artifacts"]["frame_count"], 1);
        // 计时表这一项此刻**不该**在：调用方还没写 timing.json。
        assert!(json["artifacts"].get("timing").is_none());
    }

    /// 同帧两次不一致 → 那一帧的点**没被判**（既不是通过也不是失败），
    /// 这一腿因此不干净，而且不一致的帧要被指名。
    #[test]
    fn an_unstable_frame_makes_the_leg_not_clean() {
        let spec = scene_by_name("gradient").unwrap();
        let run = model_run(spec, 0, false);
        let counts = run.counts();
        assert_eq!(counts.unjudged, spec.samples.len(), "不一致的帧不该被判");
        assert_eq!(counts.failed, 0, "没判 ≠ 判错");
        assert!(!counts.clean());

        let json = leg_json(&run, &[spec], "DX12", Some("a"), (0, 16));
        assert!(!json["backends"][0]["counts"]["clean"].as_bool().unwrap());
        assert_eq!(json["backends"][0]["repeat_mismatches"][0], "gradient f000");
    }

    /// 填一张"每个采样点都恰好等于模型预测"的图。用来验证判定本身的接线：
    /// 全对必须全绿。
    fn ideal_image(spec: &SceneSpec, frame: u32) -> Rgba8Image {
        let (width, height) = spec.size;
        let mut image = Rgba8Image {
            width,
            height,
            pixels: vec![0; (width * height * 4) as usize],
        };
        for point in spec.samples {
            let bytes = expected_bytes(spec, frame, *point);
            let index = ((point.y * width + point.x) * 4) as usize;
            image.pixels[index..index + 4].copy_from_slice(&bytes);
        }
        image
    }

    /// 一条腿（一个后端的一轮 corpus）里的一帧 —— 用模型自己的预测值造的，
    /// 所以"干净"是它的正常状态。`repeat_identical` 为假时，第二次的摘要真的不同，
    /// 免得"不一致"是一条假的绿。
    fn model_run(spec: &'static SceneSpec, frame: u32, repeat_identical: bool) -> SceneRun {
        let image = ideal_image(spec, frame);
        let digest = fnv1a64(&image.pixels);
        let png = image.encode_png().unwrap();
        SceneRun {
            frames: vec![SceneFrame {
                spec,
                frame,
                digest,
                repeat_digest: if repeat_identical {
                    digest
                } else {
                    digest ^ 0xdead_beef
                },
                png_digest: fnv1a64(&png),
                png,
                points: judge_frame(spec, frame, &image, repeat_identical),
            }],
        }
    }

    /// 每条场景的判定必须**会绿**：一个永远红的判据和永远绿的判据一样没用。
    #[test]
    fn every_scene_passes_on_its_own_model() {
        for name in scene_names() {
            let spec = scene_by_name(name).unwrap();
            let frame = if spec.uses_frame { 5 } else { 0 };
            let frame = frame % 16;
            let readings = judge_frame(spec, frame, &ideal_image(spec, frame), true);
            assert_eq!(readings.len(), spec.samples.len());
            for reading in &readings {
                let verdict = reading
                    .verdict
                    .as_ref()
                    .unwrap_or_else(|| panic!("{} {} 没被判", name, reading.point.label));
                assert!(
                    verdict.passed,
                    "{} f{} {} 用模型自己的预测值却没通过：距离 {}",
                    name, frame, reading.point.label, verdict.distance
                );
                assert_eq!(verdict.distance, 0);
            }
        }
    }

    /// 把其中一个点改掉，必须**只有它**报错——否则"哪一点不对"这条信息就没了，
    /// 而记录里最有用的恰恰是这一条。
    #[test]
    fn a_wrong_pixel_is_reported_and_named() {
        let spec = scene_by_name("gradient").unwrap();
        let frame = 5;
        let mut image = ideal_image(spec, frame);
        let point = spec.samples[2];
        let index = ((point.y * spec.size.0 + point.x) * 4) as usize;
        // 偏 40：远超容差（1），不是那种"擦着过的模糊状态"。
        image.pixels[index] = image.pixels[index].saturating_sub(40);

        let readings = judge_frame(spec, frame, &image, true);
        let failed: Vec<&PointReading> =
            readings.iter().filter(|r| !r.verdict.as_ref().unwrap().passed).collect();
        assert_eq!(failed.len(), 1, "只有被改的那一点该失败");
        assert_eq!(failed[0].point.label, point.label);
        assert!(failed[0].verdict.as_ref().unwrap().distance >= 39);
    }

    /// 重复渲染不一致时**不判**，但读数照样留着。
    ///
    /// 这是本模块最容易写错的一处：把 `verdict` 留空是"没验"，把它填成"通过"是把
    /// 一个坏观测记成合格。
    #[test]
    fn unstable_frames_are_left_unjudged() {
        let spec = scene_by_name("checker").unwrap();
        let readings = judge_frame(spec, 1, &ideal_image(spec, 1), false);
        for reading in &readings {
            assert!(reading.verdict.is_none(), "不一致的帧不该有判定");
            assert!(reading.measured.is_some(), "读数要留着——发现要能被看见");
        }
        let json = point_json(&readings[0]);
        assert_eq!(json["passed"], serde_json::Value::Null);
        assert_eq!(json["expected"], serde_json::Value::Null);
        assert!(json["detail"].as_str().unwrap().contains("未判定"));
    }

    /// 越界与"没判定"是两件事，记录里不能混成一句话。
    #[test]
    fn out_of_range_is_not_the_same_as_unjudged() {
        let spec = scene_by_name("blur").unwrap();
        let out_of_range = PointReading {
            point: SamplePoint {
                label: "越界点",
                x: 9999,
                y: 9999,
                purpose: "测试用",
            },
            measured: None,
            verdict: None,
        };
        let json = point_json(&out_of_range);
        assert!(json["detail"].as_str().unwrap().contains("越界"));
        assert_eq!(json["measured"], serde_json::Value::Null);
        assert!(
            !json["detail"].as_str().unwrap().contains("未判定"),
            "越界不该被说成未判定：{json}"
        );

        // 顺便钉住"判定用的场景"这件事本身：blur 的采样坐标必须在图像里。
        for point in spec.samples {
            assert!(point.x < spec.size.0 && point.y < spec.size.1);
        }
    }

    /// 摘要要对顺序敏感：顺序变了就是"跑法变了"，不该看起来一样。
    #[test]
    fn frames_digest_is_order_sensitive() {
        let spec = scene_by_name("gradient").unwrap();
        let make = |frame: u32, digest: u64| SceneFrame {
            spec,
            frame,
            digest,
            repeat_digest: digest,
            png_digest: 0,
            png: Vec::new(),
            points: Vec::new(),
        };
        let forward = vec![make(0, 1), make(1, 2)];
        let backward = vec![make(1, 2), make(0, 1)];
        // 内容相同 → 摘要相同（摘要里不许有任何随指针、随分配地址变的东西）。
        let same_contents = vec![make(0, 1), make(1, 2)];
        assert_eq!(frames_digest(&forward), frames_digest(&same_contents));
        // 顺序变了 → 摘要要变。
        assert_ne!(frames_digest(&forward), frames_digest(&backward));

        // 摘要必须真的吃到帧号：只吃像素摘要的话，"第 3 帧和第 4 帧画成了同一张"
        // 这种错误在总摘要里看不出来。
        let a = vec![make(3, 7)];
        let b = vec![make(4, 7)];
        assert_ne!(frames_digest(&a), frames_digest(&b));
    }

    /// 统计要把四种结局分开数：失败、越界、未判定、通过。
    #[test]
    fn counts_separate_the_four_outcomes() {
        let spec = scene_by_name("gradient").unwrap();
        let run = SceneRun {
            frames: vec![SceneFrame {
                spec,
                frame: 0,
                digest: 1,
                repeat_digest: 1,
                png_digest: 1,
                png: Vec::new(),
                points: vec![
                    PointReading {
                        point: spec.samples[0],
                        measured: Some(expected_bytes(spec, 0, spec.samples[0])),
                        verdict: Some(judge_sample(
                            spec,
                            0,
                            spec.samples[0],
                            expected_bytes(spec, 0, spec.samples[0]),
                        )),
                    },
                    PointReading {
                        point: spec.samples[1],
                        measured: None,
                        verdict: None,
                    },
                    PointReading {
                        point: spec.samples[2],
                        measured: Some([0, 0, 0, 255]),
                        verdict: None,
                    },
                ],
            }],
        };
        assert_eq!(
            run.counts(),
            Counts {
                frames: 1,
                points: 3,
                failed: 0,
                out_of_range: 1,
                unjudged: 1,
            }
        );
        assert!(run.in_process_mismatches().is_empty());
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
