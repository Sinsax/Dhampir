//! corpus 的**公共部分**：一帧怎么渲染、怎么判、整表摘要怎么算。
//!
//! 这个模块存在的理由是 M2：同一帧要在**两个运行时**里画出可比的字节。
//! 如果"渲染一帧"这件事在 native 宿主和 wasm 宿主里各写一遍，那么两端出现的
//! 任何差异都要先回答"是渲染不同，还是两份驱动代码不同"——而后者是一个
//! 本来就不该存在的问题。所以它进 core：两个宿主调的是**同一个函数**。
//!
//! # 与 `wgpu::Instance` 的分工
//!
//! 这里**不创建 instance、不选 adapter**。那两个动作是宿主唯一允许分叉的地方
//! （见 [`crate::gpu::NATIVE_BACKENDS`] / [`crate::gpu::BROWSER_BACKENDS`]）。
//! 本模块接的是已经建好的 [`GpuContext`]，所以"同机同卡、只换编译器"这件事
//! 在两端是**字面成立**的。
//!
//! # 为什么渲染两次
//!
//! [`render_frame_pair`] 每帧渲染两次再比字节。这不是浪费，而是 M1 就定下的纪律：
//! "重复运行逐字节一致"必须由代码去比，不能由"我看着一样"来宣布。第二次的结果
//! 不会覆盖第一次——两次不一致时照记不误，判定直接留空（`verdict: None`），
//! 因为"没验"和"验过通过"是两件不同的事。
//!
//! # 异步
//!
//! [`readback::read_texture_rgba8`] 要等 map 回调，所以这里全是 `async fn`。core 不带执行器：
//! native 宿主用 `pollster::block_on` 包一层，wasm 宿主直接 `.await`。
//! 这一层薄得可以忽略，换来的是"两端的调用形状完全相同"。
//!
//! # 记录的形状为什么也在这里
//!
//! 渲染一致只是"同帧"的一半：另一半是**两端把同一次运行写成同一张表**。如果
//! native 写一套键名、浏览器写另一套，那么"两端的记录对不对得上"就退化成一个
//! 需要人来读的问题，而人读出来的结论没法进守卫。所以
//! [`frame_json`] / [`point_json`] / [`scene_json`] / [`leg_json`] 与它们服务的
//! [`SceneFrame`] / [`SceneRun`] / [`Counts`] 全在 core：宿主只负责**把字符串写进文件**
//! （native 落盘、浏览器交给 JS），不负责决定它长什么样。
//!
//! 这不是"顺手把文件搬过来"：搬完之后的证伪标准是 native 重跑一遍能**逐字节**
//! 复现 M1 归档的 `run.json` 与 `readings.txt`。形状一旦被谁偷偷改过，这条就红。

use serde_json::{Value, json};

use crate::readback::{self, PngError, Rgba8Image, ReadbackError};
use crate::render::scene::{
    BYTE_TOLERANCE, SCENE_TARGET_FORMAT, SCENE_TARGET_SIZE, SamplePoint, SampleVerdict,
    SceneRenderer, SceneSpec, expected_bytes, judge_sample,
};
use crate::timeline::fnv1a64;
use crate::{gpu::GpuContext, wgpu};

// ---------------------------------------------------------------------------
// 整表摘要
// ---------------------------------------------------------------------------

/// 整表摘要的一行：**场景名 + 帧号 + 像素摘要**。
///
/// 刻意不带 PNG 字节：像素才是渲染的结果，PNG 是它的编码。两者都在记录里，
/// 但进摘要的只有像素——编码器换了不该让"渲染结果变了"这条结论成立。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TableRow {
    pub scene: &'static str,
    pub frame: u32,
    pub pixel_digest: u64,
}

/// 整表摘要的**唯一实现**：按行序拼 `场景名 + 0x00 + 帧号(LE u32) + 像素摘要(LE u64)`，
/// 再取 FNV-1a 64。
///
/// 为什么把字节规则写进 core 而不是留给各自的宿主：M1 归档的整表摘要是
/// `71ecc80cade3d73d`，M2 要让浏览器**打印出同一个数**——只有一条实现时，
/// 这个数相等才是"两端渲染逐字节一致"，而不是"两段拼字节的代码碰巧都对"。
///
/// 顺序敏感是有意的：行序一变摘要就变，于是它同时也是"跑的顺序没变"的一条证据。
pub fn table_digest(rows: &[TableRow]) -> u64 {
    let mut bytes = Vec::with_capacity(rows.len() * 24);
    for row in rows {
        bytes.extend_from_slice(row.scene.as_bytes());
        bytes.push(0);
        bytes.extend_from_slice(&row.frame.to_le_bytes());
        bytes.extend_from_slice(&row.pixel_digest.to_le_bytes());
    }
    fnv1a64(&bytes)
}

// ---------------------------------------------------------------------------
// 判定
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
// 渲染
// ---------------------------------------------------------------------------

/// 一帧渲染两次的产物。
///
/// `image` 是**第一次**那张：它才是要归档的字节。第二次只留下一个摘要，
/// 因为它存在的唯一目的是回答"同一份输入再来一遍，字节是不是同一个"。
pub struct RenderPair {
    pub image: Rgba8Image,
    /// 第一次渲染的像素摘要（喂的是**紧密打包**的 RGBA8，不是带行填充的拷贝缓冲）。
    pub pixel_digest: u64,
    /// 第二次渲染的像素摘要。与 `pixel_digest` 相等才算"同进程内逐字节一致"。
    pub repeat_digest: u64,
}

impl RenderPair {
    /// 同帧两次渲染是否逐字节相同。
    pub fn identical(&self) -> bool {
        self.pixel_digest == self.repeat_digest
    }
}

/// 渲染一帧 corpus 场景并读回。
///
/// 每次都用**全新的纹理**：复用同一块纹理会把"上一帧的残留"和"这一帧真的画对了"
/// 混在一起。纹理用途里必须有 `COPY_SRC`——canvas 纹理通常没有这个用途，
/// 这也正是"wasm 侧不要从 canvas 抄像素"（M2 设计要点 2）在两端同一条约束。
pub async fn render_frame(
    ctx: &GpuContext,
    renderer: &SceneRenderer,
    frame: u32,
) -> Result<Rgba8Image, ReadbackError> {
    let (width, height) = renderer.size();
    let texture = ctx.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("dhampir corpus frame"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        // 用渲染器自己的格式，不硬写 `SCENE_TARGET_FORMAT`：格式是构造渲染器时
        // 给它的，两者是同一个问题的两个答案，写死一处就等着它们哪天对不上。
        format: renderer.format(),
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

    let mut encoder = ctx
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("dhampir corpus encoder"),
        });
    renderer.render(&mut encoder, &ctx.queue, &view, frame);
    ctx.queue.submit([encoder.finish()]);

    readback::read_texture_rgba8(&ctx.device, &ctx.queue, &texture).await
}

/// 同一帧渲染两次，返回第一次的图像与两次的摘要。
///
/// 两次**各自一条命令缓冲、各自一块纹理**：共用一块的话，"第二次没画上"和
/// "两次都画对了"会得到同样的字节，这个检查就白做了。
pub async fn render_frame_pair(
    ctx: &GpuContext,
    renderer: &SceneRenderer,
    frame: u32,
) -> Result<RenderPair, ReadbackError> {
    let first = render_frame(ctx, renderer, frame).await?;
    let second = render_frame(ctx, renderer, frame).await?;
    Ok(RenderPair {
        pixel_digest: fnv1a64(&first.pixels),
        repeat_digest: fnv1a64(&second.pixels),
        image: first,
    })
}

/// corpus 场景的**目标格式**，给宿主写记录时引用；与 [`SCENE_TARGET_FORMAT`] 同值。
///
/// 留这个别名只是为了让"宿主不该自己决定格式"这件事在签名上看得见：
/// 宿主想要格式时应当问渲染器（[`SceneRenderer::format`]），而不是自己写一个常量。
pub const CORPUS_TARGET_FORMAT: wgpu::TextureFormat = SCENE_TARGET_FORMAT;

// ---------------------------------------------------------------------------
// 一轮的产物
// ---------------------------------------------------------------------------

/// 跑一轮 corpus 会出的两类错。
///
/// 分成两支而不是一个字符串：读回失败是"GPU 那边没把字节交出来"（格式不支持、
/// 设备丢了、轮询失败），编码失败是"字节拿到了但 PNG 编不出来"。归因方向不同，
/// 合成一句话就再也拆不开。
#[derive(Debug)]
pub enum CorpusError {
    Readback(ReadbackError),
    Png(PngError),
}

impl core::fmt::Display for CorpusError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Readback(error) => write!(f, "读回失败：{error}"),
            Self::Png(error) => write!(f, "PNG 编码失败：{error}"),
        }
    }
}

impl core::error::Error for CorpusError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Readback(error) => Some(error),
            Self::Png(error) => Some(error),
        }
    }
}

impl From<ReadbackError> for CorpusError {
    fn from(error: ReadbackError) -> Self {
        Self::Readback(error)
    }
}

impl From<PngError> for CorpusError {
    fn from(error: PngError) -> Self {
        Self::Png(error)
    }
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
    /// 是宿主的事（native 侧是 `--frames` 不许空区间）。
    pub fn clean(&self) -> bool {
        self.failed == 0 && self.out_of_range == 0 && self.unjudged == 0
    }
}

/// 把若干帧按固定规则摘要成一个 64 位整数。见 [`SceneRun::frames_digest`]。
///
/// 字节规则在 [`table_digest`]：M2 要让浏览器端算出同一个数（`71ecc80cade3d73d`），
/// 那就只能有一条实现——两端各拼一遍字节、再指望它们碰巧一样，是不该出现的那种"巧合"。
pub fn frames_digest(frames: &[SceneFrame]) -> u64 {
    let rows: Vec<TableRow> = frames
        .iter()
        .map(|frame| TableRow {
            scene: frame.spec.name,
            frame: frame.frame,
            pixel_digest: frame.digest,
        })
        .collect();
    table_digest(&rows)
}

/// 渲染一帧并把它变成记录里的一行：渲染两次 → 判定 → 编 PNG → 摘要。
///
/// 场景从**渲染器**身上取（[`SceneRenderer::spec`]），不由调用方另传一个：
/// 传两次就有两个答案，"这一帧是哪条场景的"这种问题不该有第二种可能。
pub async fn render_frame_record(
    ctx: &GpuContext,
    renderer: &SceneRenderer,
    frame: u32,
) -> Result<SceneFrame, CorpusError> {
    let spec = renderer.spec();
    let pair = render_frame_pair(ctx, renderer, frame).await?;
    let points = judge_frame(spec, frame, &pair.image, pair.identical());
    let png = pair.image.encode_png()?;
    Ok(SceneFrame {
        spec,
        frame,
        digest: pair.pixel_digest,
        repeat_digest: pair.repeat_digest,
        png_digest: fnv1a64(&png),
        png,
        points,
    })
}

/// 跑一组场景 × 一个帧区间，出图、读回、逐点判定。**两个宿主共用这一份**。
///
/// 管线与 uniform **每场景建一次**（不是每帧一次）：这才是真实运行的样子
/// （M4 导出也是一个场景连续出多帧），而且"换帧不需要重建管线"这件事因此被真的走到。
///
/// 浏览器宿主可以照搬整轮，也可以逐帧调 [`render_frame_record`] 自己攒一个
/// [`SceneRun`]——两条路都通向同一个 [`SceneRun`]，所以记录的形状不会因此分叉。
pub async fn render_run(
    ctx: &GpuContext,
    specs: &[&'static SceneSpec],
    frames: (u32, u32),
) -> Result<SceneRun, CorpusError> {
    let mut rendered = Vec::new();

    for spec in specs {
        let renderer = SceneRenderer::new(&ctx.device, CORPUS_TARGET_FORMAT, spec);
        for frame in frames.0..frames.1 {
            rendered.push(render_frame_record(ctx, &renderer, frame).await?);
        }
    }

    Ok(SceneRun { frames: rendered })
}

// ---------------------------------------------------------------------------
// 记录
// ---------------------------------------------------------------------------

/// 记录里 `schema` 的值。**改了它就必须同时改守卫**，否则旧守卫会去读新形状。
pub const CORPUS_RECORD_SCHEMA: u32 = 1;

/// 记录里 `kind` 的值：这一份说的是 corpus 表。
pub const CORPUS_RECORD_KIND: &str = "corpus";

/// 记录里 `milestone` 的值——**它指的是这张表的契约版本，不是产生它的里程碑**。
///
/// 这张表在 M1 冻结；M2 起的腿（包括浏览器那条）沿用同一个值，于是"两条腿用的是不是
/// 同一张表"这个问题由**字节**回答，不需要谁来解释。产生记录的里程碑与时间由记录
/// **目录**（`records/m2/…`）和那一腿自己的 `adapter.json` 承担，不挤进这张表：
/// 挤进来的话，重跑一遍就复现不了已经归档的那份 `run.json`，而"能复现"是这份
/// 记录最有价值的地方。
pub const CORPUS_TABLE_MILESTONE: &str = "M1";

/// 一帧在记录目录里的相对路径，`/` 分隔（跨平台一致）。
///
/// 帧号固定三位：记录里的文件按**字典序**排要与帧号顺序一致，否则人一眼扫过去
/// 看到的是 `f10` 排在 `f2` 前面——那种记录没人会认真读。
pub fn frame_rel_path(spec_name: &str, frame: u32) -> String {
    format!("frames/{spec_name}-f{frame:03}.png")
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
        BYTE_TOLERANCE,
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

/// 一个采样点的结构化记录。
///
/// `passed` 是三态的：`true` / `false` / `null`（没判）。**"没判"绝不能写成 `true`。**
pub fn point_json(reading: &PointReading) -> Value {
    let point = reading.point;
    let (expected, distance, tolerance, passed, detail) = match &reading.verdict {
        Some(v) => (
            Value::from(v.expected.to_vec()),
            Value::from(v.distance),
            Value::from(v.tolerance),
            Value::from(v.passed),
            Value::from(v.detail.clone()),
        ),
        None => (
            Value::Null,
            Value::Null,
            Value::from(BYTE_TOLERANCE),
            Value::Null,
            Value::from(if reading.measured.is_none() {
                "越界：采样坐标落在图像外".to_string()
            } else {
                "未判定：同帧两次渲染结果不一致".to_string()
            }),
        ),
    };
    json!({
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

/// 一帧的结构化记录（`run.json` 里的 `frames[]`）。
pub fn frame_json(frame: &SceneFrame) -> Value {
    let points: Vec<Value> = frame.points.iter().map(point_json).collect();
    json!({
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

/// 场景注册表的结构化记录：**记录要能自解释**，复核的人不该被迫去读 core 的源码
/// 才知道"gradient 是在考什么、几趟、尺寸多少"。
pub fn scene_json(spec: &SceneSpec) -> Value {
    let samples: Vec<Value> = spec
        .samples
        .iter()
        .map(|s| {
            json!({
                "label": s.label,
                "x": s.x,
                "y": s.y,
                "purpose": s.purpose,
                "expected": expected_bytes(spec, 0, *s).to_vec(),
            })
        })
        .collect();
    json!({
        "name": spec.name,
        "description": spec.description,
        "size": format!("{}x{}", spec.size.0, spec.size.1),
        "passes": spec.pass_count(),
        "fragment_entries": spec.fragment_entries(),
        "uses_frame": spec.uses_frame,
        "samples": samples,
    })
}

/// 一条腿的 `run.json`：**这个运行时这次画出了什么**。**纯函数**。
///
/// 一条腿一份、写在自己的目录里，而不是把两个后端塞进同一个文件。M0 的
/// `run.json` 是一个文件装两个后端，靠**文件名**区分（`probe-native-dx12.png`）；
/// 到 M1，一条腿有 80 张图，靠文件名区分已经不够了——一条腿一个目录，
/// 目录里这份 `run.json` 说的就是这一条腿。M2 起浏览器那条腿照同一套写。
///
/// 形状是刻意对齐 native 的 `compare_runs` 的：它按 `backends[]` 里的 `requested` /
/// `frames_digest` / `frames[]` 逐个比，所以这里必须给出一个**只有一条**的
/// `backends` 数组。于是"跨进程比对"不需要知道目录结构，只需要两份 JSON。
///
/// `nondeterministic_fields` 是**空的**——这不是漏填：corpus 这一份里没有任何
/// 一项被允许变化，退出标准那句"重复运行逐字节一致"说的就是它。会变的东西
/// （时间戳、计时、适配器）在 `adapter.json` / `timing.json` 里，各有各的声明。
///
/// `milestone` 写的是**表的契约版本**而不是这一次运行的时间，见
/// [`CORPUS_TABLE_MILESTONE`]。
pub fn leg_json(
    run: &SceneRun,
    specs: &[&'static SceneSpec],
    requested: &str,
    adapter_name: Option<&str>,
    frames: (u32, u32),
) -> Value {
    let counts = run.counts();
    let frames_json: Vec<Value> = run.frames.iter().map(frame_json).collect();
    let scenes: Vec<Value> = specs.iter().map(|s| scene_json(s)).collect();

    json!({
        "schema": CORPUS_RECORD_SCHEMA,
        "milestone": CORPUS_TABLE_MILESTONE,
        "kind": CORPUS_RECORD_KIND,
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
// 记录的字节：全仓唯一的一处
// ---------------------------------------------------------------------------

/// 一条记录 → **要写进文件的那串字节**。
///
/// # 为什么连"怎么序列化"也要收进 core
///
/// 形状（有哪些键）与字节（缩进、换行、数字怎么打）是两件事，但它们一起决定
/// "两份记录能不能逐字节比"。M1 归档的 `run.json` 是 `to_string_pretty` + 一个结尾换行
/// 的产物；M2 的浏览器腿要走同一条路，否则"两端的记录一样"就得靠人去读。
/// 两个宿主各写一遍 `to_string_pretty` 的话，哪天一边加了结尾换行、另一边没加，
/// 差异会淹没在几百行 JSON 里。
///
/// 返回 `Result` 而不是直接 `unwrap`：序列化本身不该失败（我们的值里没有 NaN、
/// 没有非字符串的键），但**一份写不出来的记录必须让退出码红**，而不是让进程炸在
/// 别的地方、留下半份文件。native 侧把这句话变成 `Result<(), String>`，
/// 浏览器侧变成抛给页面的异常。
pub fn record_text(value: &Value) -> Result<String, String> {
    let mut text = serde_json::to_string_pretty(value)
        .map_err(|e| format!("记录序列化失败：{e}"))?;
    text.push('\n');
    Ok(text)
}

/// `epoch_millis` → 记录里"秒"那一栏。0（读不到时钟）写成 `null`。
///
/// 秒与毫秒都给：M0 的记录里就是秒（整数），跨记录对时间时毫秒更有用。
///
/// 住在这里的理由与 [`record_text`] 相同：两条腿的记录里都有这一对键，
/// **"0 要写成 null"是一条记录规则，不是某一侧的实现细节**。浏览器侧的时间戳由
/// 页面从 `Date.now()` 传进来（wasm 里没有系统时钟），判定它"读到了没有"的规矩
/// 必须与 native 完全一致——否则 M0 就定下的"绝不编造时间戳"会在浏览器腿失效。
pub fn epoch_seconds(epoch_millis: u64) -> Value {
    if epoch_millis == 0 {
        Value::Null
    } else {
        Value::from(epoch_millis / 1000)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::{SELECTABLE_SCENES, scene_by_name, scene_names};

    /// 摘要的字节布局**逐字**钉住：多一个分隔符、少一次 LE 编码，
    /// 都会让 M2 里"浏览器打印出 M1 那个数"这句话失去意义。
    #[test]
    fn table_digest_layout_is_pinned() {
        let rows = [
            TableRow {
                scene: "gradient",
                frame: 0,
                pixel_digest: 0x0102_0304_0506_0708,
            },
            TableRow {
                scene: "blur",
                frame: 16,
                pixel_digest: 0xffff_ffff_ffff_ffff,
            },
        ];
        let mut expected = Vec::new();
        expected.extend_from_slice(b"gradient");
        expected.push(0);
        expected.extend_from_slice(&0u32.to_le_bytes());
        expected.extend_from_slice(&0x0102_0304_0506_0708u64.to_le_bytes());
        expected.extend_from_slice(b"blur");
        expected.push(0);
        expected.extend_from_slice(&16u32.to_le_bytes());
        expected.extend_from_slice(&0xffff_ffff_ffff_ffffu64.to_le_bytes());
        assert_eq!(table_digest(&rows), fnv1a64(&expected));
    }

    #[test]
    fn table_digest_is_order_sensitive() {
        let a = TableRow {
            scene: "checker",
            frame: 3,
            pixel_digest: 7,
        };
        let b = TableRow {
            scene: "checker",
            frame: 4,
            pixel_digest: 7,
        };
        assert_ne!(table_digest(&[a, b]), table_digest(&[b, a]));
    }

    /// 帧号与摘要都进字节：只改其中一个，摘要必须变。
    #[test]
    fn table_digest_sees_every_field() {
        let base = TableRow {
            scene: "srgb_linear",
            frame: 5,
            pixel_digest: 11,
        };
        assert_ne!(
            table_digest(&[base]),
            table_digest(&[TableRow {
                scene: "srgb_linear",
                frame: 6,
                ..base
            }])
        );
        assert_ne!(
            table_digest(&[base]),
            table_digest(&[TableRow {
                pixel_digest: 12,
                ..base
            }])
        );
        assert_ne!(
            table_digest(&[base]),
            table_digest(&[TableRow {
                scene: "alpha_stack",
                ..base
            }])
        );
    }

    /// 空表也算得出摘要，但**它不等于任何一行非空表的摘要**——
    /// 守卫那边"拒绝空文件集通过"的纪律在数据层也得站得住。
    #[test]
    fn empty_table_is_not_a_pass() {
        let empty = table_digest(&[]);
        let one = table_digest(&[TableRow {
            scene: "gradient",
            frame: 0,
            pixel_digest: 0,
        }]);
        assert_ne!(empty, one);
    }

    /// 越界坐标：`measured` 为空、`verdict` 也为空，但这条读数**照样留在表里**。
    #[test]
    fn out_of_range_point_is_kept_without_a_verdict() {
        let spec = &SELECTABLE_SCENES[0];
        let image = Rgba8Image {
            width: 4,
            height: 4,
            pixels: vec![0; 4 * 4 * 4],
        };
        let readings = judge_frame(spec, 0, &image, true);
        assert_eq!(readings.len(), spec.samples.len());
        assert!(readings.iter().all(|r| r.measured.is_none()));
        assert!(readings.iter().all(|r| r.verdict.is_none()));
    }

    /// 两次渲染不一致时**一个点都不判**，但实测值要留下。
    #[test]
    fn unstable_frame_is_recorded_but_not_judged() {
        let spec = &SELECTABLE_SCENES[0];
        let (width, height) = spec.size;
        let image = Rgba8Image {
            width,
            height,
            pixels: vec![255; (width * height * 4) as usize],
        };
        let stable = judge_frame(spec, 0, &image, true);
        let unstable = judge_frame(spec, 0, &image, false);
        assert!(stable.iter().all(|r| r.verdict.is_some()));
        assert!(unstable.iter().all(|r| r.verdict.is_none()));
        assert!(unstable.iter().all(|r| r.measured.is_some()));
    }

    /// 别名与它指向的常量同值——这类"留一个名字"的写法最容易在改名时漂移。
    #[test]
    fn corpus_format_alias_points_at_the_scene_format() {
        assert_eq!(CORPUS_TARGET_FORMAT, SCENE_TARGET_FORMAT);
    }

    /// 注册表里每个场景的采样点都判得完，且判出来的条数就是声明条数。
    #[test]
    fn every_scene_judges_its_own_declared_points() {
        for spec in SELECTABLE_SCENES.iter() {
            let (width, height) = spec.size;
            let image = Rgba8Image {
                width,
                height,
                pixels: vec![0; (width * height * 4) as usize],
            };
            assert_eq!(
                judge_frame(spec, 0, &image, false).len(),
                spec.samples.len()
            );
        }
        assert_eq!(scene_names().len(), SELECTABLE_SCENES.len());
    }

    // ---- 记录的形状 --------------------------------------------------------
    //
    // 这一组测试原本住在 `dhampir-worker`：它们验的是"记录长什么样"，而记录的形状
    // 现在归 core（M2 的两端要产出同一张表）。跟着代码搬家，否则守卫会以为
    // 搬走的东西还有人看着。

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
        let failed: Vec<&PointReading> = readings
            .iter()
            .filter(|r| !r.verdict.as_ref().unwrap().passed)
            .collect();
        assert_eq!(failed.len(), 1, "只有被改的那一点该失败");
        assert_eq!(failed[0].point.label, point.label);
        assert!(failed[0].verdict.as_ref().unwrap().distance >= 39);
    }

    /// 重复渲染不一致时**不判**，但读数照样留着；记录里 `passed` 是 `null`，
    /// 不是 `false` 也不是 `true`。
    ///
    /// 这是最容易写错的一处：把 `verdict` 留空是"没验"，把它填成"通过"是把一个
    /// 坏观测记成合格。
    #[test]
    fn unstable_frames_are_left_unjudged() {
        let spec = scene_by_name("checker").unwrap();
        let readings = judge_frame(spec, 1, &ideal_image(spec, 1), false);
        for reading in &readings {
            assert!(reading.verdict.is_none(), "不一致的帧不该有判定");
            assert!(reading.measured.is_some(), "读数要留着——发现要能被看见");
        }
        let json = point_json(&readings[0]);
        assert_eq!(json["passed"], Value::Null);
        assert_eq!(json["expected"], Value::Null);
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
        assert_eq!(json["measured"], Value::Null);
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

    /// 一条腿的记录要能被 native 的 `compare_runs` 读、也要能被人读。
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
        assert_eq!(json["nondeterministic_fields"], json!([]));
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

    /// **表的形状本身要被钉住**：三份记录的键集一字不差。
    ///
    /// `serde_json` 的 `Value` 是 BTreeMap，键按字典序输出——所以"键集相同"等价于
    /// "`run.json` 的行序相同"，而 `records/m1/` 里归档的那一份正是靠这一点被
    /// 逐字节复现的。加一个键、改一个名字，都会先在这里红。
    #[test]
    fn the_record_keys_are_pinned() {
        let spec = scene_by_name("checker").unwrap();
        let run = model_run(spec, 0, true);
        let leg = leg_json(&run, &[spec], "DX12", None, (0, 16));

        let keys = |value: &Value| -> Vec<String> {
            value
                .as_object()
                .expect("记录必须是对象")
                .keys()
                .cloned()
                .collect()
        };
        assert_eq!(
            keys(&leg),
            [
                "artifacts",
                "backends",
                "byte_tolerance",
                "frame_range",
                "frames_per_scene",
                "kind",
                "milestone",
                "nondeterministic_fields",
                "scenes",
                "schema",
                "target_format",
                "target_size",
            ]
        );
        assert_eq!(keys(&frame_json(&run.frames[0])), [
            "frame",
            "pixel_digest",
            "png",
            "png_bytes",
            "png_digest",
            "points",
            "repeat_identical",
            "repeat_pixel_digest",
            "scene",
        ]);
        assert_eq!(keys(&point_json(&run.frames[0].points[0])), [
            "detail",
            "distance",
            "expected",
            "label",
            "measured",
            "passed",
            "purpose",
            "tolerance",
            "x",
            "y",
        ]);
        assert_eq!(
            keys(&leg["backends"][0]),
            [
                "adapter_name",
                "counts",
                "frames",
                "frames_digest",
                "repeat_mismatches",
                "requested",
            ]
        );
        assert_eq!(
            keys(&leg["artifacts"]),
            ["adapter", "frame_count", "frames_dir", "readings"]
        );
        assert_eq!(keys(&leg["scenes"][0]), [
            "description",
            "fragment_entries",
            "name",
            "passes",
            "samples",
            "size",
            "uses_frame",
        ]);
        assert_eq!(keys(&leg["scenes"][0]["samples"][0]), [
            "expected",
            "label",
            "purpose",
            "x",
            "y",
        ]);

        // 契约版本写死在记录里：M2 的浏览器腿沿用同一个值（见 `CORPUS_TABLE_MILESTONE`）。
        assert_eq!(leg["schema"], CORPUS_RECORD_SCHEMA);
        assert_eq!(leg["milestone"], CORPUS_TABLE_MILESTONE);
        assert_eq!(leg["kind"], CORPUS_RECORD_KIND);
    }

    /// 记录文本的**字节**：两空格缩进 + **一个**结尾换行，且结尾只有一个。
    ///
    /// 钉在这儿是因为 M1 归档的那些文件就是它。谁改了缩进或换行，M2 的浏览器腿
    /// 与 `records/m1/` 的逐字节比对就会红——那时应当先问"为什么改"，
    /// 而不是去改归档。
    #[test]
    fn record_text_bytes_are_pinned() {
        let text = record_text(&json!({"b": [1, 2], "a": {"n": null}})).unwrap();
        assert_eq!(text, "{\n  \"a\": {\n    \"n\": null\n  },\n  \"b\": [\n    1,\n    2\n  ]\n}\n");
        // 结尾**只有一个**换行：多一个，M1 归档的 diff 会多出一行空行。
        assert!(text.ends_with("}\n"));
        assert!(!text.ends_with("\n\n"));
    }

    /// 读不到时钟记 `null`，**不是** 1970 年。
    ///
    /// 这条规矩 M0 就定了（`records/m0/*.json` 里能读到），M2 的浏览器腿沿用同一个
    /// 判定——两条腿的 `adapter.json` 在这一点上必须是同一个答案。
    #[test]
    fn a_missing_clock_is_null_not_1970() {
        assert_eq!(epoch_seconds(0), Value::Null);
        assert_eq!(epoch_seconds(1_790_000_000_123), Value::from(1_790_000_000_u64));
        // 毫秒被**截断**而不是四舍五入：秒那一栏说的是"这一秒"，不是"最接近的一秒"。
        assert_eq!(epoch_seconds(1_790_000_000_999), Value::from(1_790_000_000_u64));
    }
}
