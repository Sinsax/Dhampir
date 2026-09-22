//! M1 的环境基线与计时：adapter、wgpu 版本、时间戳、行对齐探针、1080p 计时。
//!
//! 与 [`crate::scenes`] 分工不同：`scenes` 回答"**画出来的东西对不对**"，
//! 本模块回答"**这是在什么环境里、多快画出来的**"。两者的记录因此也分开：
//! `run.json` 是可比对的渲染结果，`adapter.json` / `timing.json` 是本机环境。
//!
//! # 为什么环境与计时分成两个文件
//!
//! 时间戳、计时数字每次运行都不一样，adapter 几乎不变。把两者放同一个文件，就等于让
//! "这台机器上的 adapter 是什么"这个问题的答案每次 diff 都变——一份每次都变的记录，
//! 没人会去看它的 diff，于是它作为记录的价值就没了。分开之后：
//!
//! - `run.json` / `readings.txt` / `frames/*.png`：**确定**，同机同后端两次运行逐字节相同
//! - `adapter.json`：几乎不变（换机器/驱动才变），因此**在计时之前就能写**
//! - `timing.json`：每次都变（由 `nondeterministic_fields` 自己声明）
//!
//! 两份记录共用同一个 `unix_epoch_millis`：谁都能看出它们属于**同一次**运行。
//!
//! # 与 M0 的 `adapter.json` 的关系
//!
//! 刻意**不复用**：那份是 M0 的已归档产物，字段集合一改，"用本提交的代码重跑 M0
//! 会得到记录里的那份字节"这句话就不再成立。两份记录回答的问题也不同——M0 那份只描述
//! 探针三角形那一张图，这份要说清 256×256 的 corpus 与 1920×1080 的计时**两套尺寸**
//! 各自是什么（见 [`dhampir_core::render::SceneRenderer::new_at`]：换尺寸不是缩放）。

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use dhampir_core::gpu::GpuContext;
use dhampir_core::readback::{
    COPY_BYTES_PER_ROW_ALIGNMENT, Rgba8Image, padded_bytes_per_row, read_texture_rgba8,
};
use dhampir_core::render::{
    BYTE_TOLERANCE, SCENE_TARGET_FORMAT, SCENE_TARGET_SIZE, SceneRenderer, SceneSpec, scene_model,
};
use dhampir_core::timeline;
use dhampir_core::wgpu;

/// 1080p 单帧的预算（毫秒）。**判据是"渲染 + 读回"那一趟往返**，见 [`Baseline::budget_verdict`]。
///
/// plan 里写明这是"起始值，按实测定档"，所以它是一条**会被改的**线。实测之后仍然留
/// 10.0：本机 worst 是 2.73 ms（RTX 4070 / DX12，见 `records/m1/`），留着意味着这条线
/// 比实测宽——宽的那一侧会被别的环境打中（Linux 软件光栅化 lavapipe 在 1080p 上必然
/// 超过 10 ms，而那一腿的任务是"证明链路能通"，不是比快慢）。
///
/// 它**不参与退出码**：一台正在跑别的东西的机器能让任何预算失败，而那种红只会教人
/// 忽略这条判据。它进记录，作为"当时有多快"的一个可比较的数字。
pub const FRAME_BUDGET_MS: f64 = 10.0;

/// 计时用的尺寸。1920×1080 是 M4 导出要面对的真实尺寸。
pub const TIMING_SIZE: (u32, u32) = (1920, 1080);

/// 计时用的帧号。**刻意不为 0**：随帧变化的场景在 `frame % N == 0` 时可能走到
/// 更省事的分支，而计时表要量的是常态那一支。
pub const TIMING_FRAME: u32 = 5;

/// 预热帧数。第一帧要建管线、第一次分配缓冲，它不代表后续任何一帧。
pub const TIMING_WARMUP: usize = 3;

/// 有效测量次数。取**中位数**而不是平均：平均值会被调度抖动拉走，
/// 而这里要回答的是"常态有多快"。极值照样记下来，好知道有没有哪一次特别糟。
pub const TIMING_REPEATS: usize = 24;

/// 行对齐探针的尺寸：宽 1366 → 一行 5464 字节，**不是** 256 的倍数（要填到 5632）。
///
/// 挑这个宽度是有意的：1920×1080 的一行是 7680 字节，正好整除 256——**它测不出对齐错误**。
/// 只在那样的宽度上验过，等于没验（这正是 M0 文档里那句话的由来）。
pub const ALIGN_PROBE_SIZE: (u32, u32) = (1366, 768);

/// 行对齐探针用哪个场景。`checker` 是唯一一个**按像素坐标定义**的场景
/// （着色器只用 `frag.x` / `frag.y`，不除以尺寸），所以它的模型在任何尺寸上都成立，
/// 可以拿来逐像素复核一张非对齐宽度的读回图。
pub const ALIGN_PROBE_SCENE: &str = "checker";

/// 探针用的帧号。0 → 格子边长 4 像素（见
/// [`scene_model::checker_cell_px`]），细节频率最高，最能暴露"行错位"。
pub const ALIGN_PROBE_FRAME: u32 = 0;

/// 报告里最多列几个失败例子。列到第 8 个已经足够让人看出错位的模式，
/// 再多只是把同一句话重复一千遍。
pub const MAX_EXAMPLES: usize = 8;

/// 当前是 debug 还是 release 构建。
///
/// **必须进记录**：debug 构建下的计时数字（尤其 1080p）会比 release 慢一个数量级以上，
/// 拿它去对 10ms 的预算没有意义。记录里没有这一项的话，读的人只能自己猜。
pub fn build_profile() -> &'static str {
    if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    }
}

/// 编进二进制的 wgpu 版本。**实现在 core**（[`dhampir_core::gpu::WGPU_VERSION`]）。
///
/// 这里只留一层转发：记录里那个键叫 `wgpu_version`，而"这个数从哪来"必须只有一个答案。
/// M2 之前它是 worker 自己 `build.rs` 编进去的——那时只有 native 一个宿主要写这份记录。
/// 浏览器那条腿也要写同一个键之后，"两个宿主各自编一个版本号"就多出了一处可能的漂移：
/// 版本号对上了，记录却不同。搬进 core 之后，两个宿主读的是同一个常量。
///
/// 读不到时是 `"unknown"`，而不是一个编造的数字：**记录里出现 unknown 是可见的缺陷**。
/// [`wgpu_version_is_real`] 钉着这一条。
pub fn wgpu_version() -> &'static str {
    dhampir_core::gpu::WGPU_VERSION
}

/// 编进二进制的 naga 版本（`wgpu` 的着色器前端，`wgpu::naga`）。**实现在 core**。
///
/// 记它是因为 WGSL 的可移植子集检查（`wgsl_subset`）针对的就是这个前端的行为：
/// 将来排查"某段 WGSL 在浏览器上过了、在 native 上没过"时，第一个要问的就是两边
/// naga 是不是同一个版本。
///
/// **但它只描述 native 这条腿。** 浏览器上 WGSL 由浏览器自己的实现编译
/// （Chrome 是 Dawn/Tint），naga 不在那条路径上——浏览器那条腿的记录里对这一点
/// 必须如实写明，不能把这个数抄过去。
pub fn naga_version() -> &'static str {
    dhampir_core::gpu::NAGA_VERSION
}

// ---------------------------------------------------------------------------
// 开一条腿
// ---------------------------------------------------------------------------

/// 建 Instance + adapter + device，并**量出** init 用时。
///
/// 计时从建 `Instance` 开始，到 device 能用为止——[`Baseline::init`] 记的就是这一段。
/// 不量它的话，"冷启动为什么慢"将来只能靠猜；而它又是唯一一个"想量也量不回来"的数字
/// （进程起来之后，那段就已经过去了）。
///
/// **`Instance::new` 只写在这一处。** 它是宿主之间唯一允许分叉的那一行（浏览器侧在
/// `dhampir-wasm` 里对应地分叉一次），探针路径与 corpus 路径都从这个函数进 GPU——
/// 两条路径各写一遍的话，"哪个后端"这件事就有了两个真相。
pub fn open_leg(
    backends: wgpu::Backends,
) -> Result<(GpuContext, Duration), Box<dyn std::error::Error>> {
    let start = Instant::now();
    // `Instance::new` 按值收 descriptor（wgpu 30 起），且没有 `Default`——
    // 用 `new_without_display_handle()` 起底，只覆盖 `backends` 这一个字段。
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends,
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    });

    // 这个 `block_on` 之所以合法，是因为 `request_context` 里的 future 在 native 上
    // 第一次 poll 就会就绪（wgpu-core 的 adapter/device 申请本体是同步的）。
    // 真正需要等待的是读回，见 `dhampir_core::readback::MapWait` 的文档。
    let ctx = pollster::block_on(dhampir_core::gpu::request_context(&instance, None))?;
    Ok((ctx, start.elapsed()))
}

// ---------------------------------------------------------------------------
// adapter 身份
// ---------------------------------------------------------------------------

/// adapter 的身份：**记录里要出现的那几个字段**。
///
/// 存的是 [`dhampir_core::gpu::describe_adapter`] 的输出，而不是 `wgpu::AdapterInfo`
/// 本身。原因有两个：字段集合由 core 钉死（两个宿主写出来的记录因此长得一样），
/// 而且这个类型能被单测直接构造——否则 [`adapter_json`] 就只能靠"跑一次真的"来验。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdapterIdentity {
    /// 请求的后端位标志（`--backend` 选的那个，不是 wgpu 最后给的）。
    pub requested: wgpu::Backends,
    /// `describe_adapter` 给的键值。
    pub fields: Vec<(&'static str, String)>,
}

impl AdapterIdentity {
    pub fn from_context(ctx: &GpuContext, requested: wgpu::Backends) -> Self {
        Self {
            requested,
            fields: dhampir_core::gpu::describe_adapter(&ctx.adapter_info),
        }
    }

    /// 取某个字段。取不到返回 `None`——**不编一个占位符**：`<未知>` 会让
    /// "记录里没有这一项"和"这一项的值真的叫 <未知>"长得一样。
    pub fn field(&self, key: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v.as_str())
    }

    /// adapter 名字。判定与打印都要它。
    pub fn name(&self) -> Option<&str> {
        self.field("name")
    }

    /// 把身份字段插进一张已有的表。
    ///
    /// 探针的 `adapter.json` 要在这几个字段上面续写"这次出图的条件"，插进已有的表
    /// 比来回拆 `Value::Object` 干净。
    pub fn insert_into(&self, map: &mut serde_json::Map<String, serde_json::Value>) {
        for (key, value) in &self.fields {
            map.insert((*key).to_string(), serde_json::Value::from(value.clone()));
        }
    }

    /// 记录里的形态。
    pub fn to_json(&self) -> serde_json::Value {
        let mut map = serde_json::Map::new();
        self.insert_into(&mut map);
        serde_json::Value::Object(map)
    }
}

/// 请求的后端位标志 → **记录里那个后端名**（`DX12`、`VULKAN`、`BROWSER_WEBGPU`）。
/// **实现在 core**（[`dhampir_core::gpu::backend_label`]）。
///
/// 不直接 `{:?}` 出来：`wgpu::Backends` 是位标志包装，它的 `Debug` 是
/// `Backends(DX12)`——那是 wgpu 的内部形态，不是后端名。M0 归档的
/// `records/m0/*.json` 里就是那个形态（当时直接 `{:?}` 了），**不追溯改写**：
/// 那些文件是那一次运行的证据。M1 起统一走本函数，于是"记录里写的后端名"和
/// "产物目录名"（[`backend_slug`]）说的是同一件事，而不是两套拼法。
///
/// M2 之后实现搬去了 core：浏览器那条腿要写**同一个键**，而两个宿主互不依赖。
/// 这里只留一层转发，调用点与归档记录都不用动。见 core 里那段注释。
pub fn backend_label(backends: wgpu::Backends) -> String {
    dhampir_core::gpu::backend_label(backends)
}

/// 后端名 → 目录/文件名里的小写形式（`DX12` → `dx12`）。**实现在 core**。
///
/// 每个后端的产物必须落在**自己的目录**里：M0 已经踩过"后跑的盖了先跑的"这个坑
/// （见 `offscreen::ProbeRun::stem` 的注释），一次 `--backend all` 会让两个后端
/// 写出同一批 `frames/*.png`。
///
/// 只留下 `[a-z0-9-]`：位或起来的多后端（`DX12 | VULKAN`）也要能当目录名，
/// 而 `|` 在 Windows 上不是合法文件名字符。
pub fn backend_slug(backends: wgpu::Backends) -> String {
    dhampir_core::gpu::backend_slug(backends)
}

// ---------------------------------------------------------------------------
// 计时
// ---------------------------------------------------------------------------

/// 一串测量值的摘要。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Measurement {
    pub n: usize,
    pub median_ms: f64,
    pub min_ms: f64,
    pub max_ms: f64,
}

/// 把一串毫秒值摘要成中位数 + 极值。空集合返回 `None`——**不是** 0。
///
/// 返回 0 的话，"一次都没测到"会显示成"快得不花时间"。
pub fn measurement(values: &[f64]) -> Option<Measurement> {
    if values.is_empty() {
        return None;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = sorted.len();
    let median = if n % 2 == 1 {
        sorted[n / 2]
    } else {
        (sorted[n / 2 - 1] + sorted[n / 2]) / 2.0
    };
    Some(Measurement {
        n,
        median_ms: median,
        min_ms: sorted[0],
        max_ms: sorted[n - 1],
    })
}

/// 一个场景在计时尺寸下的两个数字。
#[derive(Clone, Debug)]
pub struct SceneTiming {
    pub scene: &'static str,
    /// 编码 + 提交的 CPU 时间，**不含读回**。
    ///
    /// 这是 T1.5 要的那个数字。它量的是 CPU 侧把这一帧交出去花多久——**不是**
    /// GPU 那边画完花多久（`submit` 是异步的，画完之前它早就返回了）。写成
    /// "渲染时间"会让人以为它是 GPU 时间，所以这里的字段名与文档都写清楚是哪一段。
    pub frame: Measurement,
    /// 读回：GPU 拷贝 → 映射等待 → 去掉行填充 → 拷进紧密打包的 `Vec`。
    pub readback: Measurement,
}

/// 把一个 `Duration` 换算成毫秒。
pub fn millis(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

/// 四舍五入到三位小数。记录里写 `0.853`，不写 `0.8529999999999999`。
pub fn round3(value: f64) -> f64 {
    (value * 1000.0).round() / 1000.0
}

/// 在指定尺寸上量一个场景的单帧与读回时间。
///
/// 纹理**建一次、全程复用**：这是真实导出循环的样子（M4 也是一个场景连续出多帧），
/// 而"每帧新建一张目标纹理"会把分配开销算进帧时间里，量的就不是画的那一段了。
pub fn time_scene(
    ctx: &GpuContext,
    spec: &'static SceneSpec,
    size: (u32, u32),
) -> Result<SceneTiming, Box<dyn std::error::Error>> {
    let renderer = SceneRenderer::new_at(&ctx.device, SCENE_TARGET_FORMAT, spec, size);
    let texture = create_target(ctx, renderer.size(), "dhampir timing target");
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

    for _ in 0..TIMING_WARMUP {
        render_and_read(ctx, &renderer, &view, &texture, TIMING_FRAME)?;
    }

    let mut frames = Vec::with_capacity(TIMING_REPEATS);
    let mut readbacks = Vec::with_capacity(TIMING_REPEATS);
    for _ in 0..TIMING_REPEATS {
        let started = Instant::now();
        let mut encoder = ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("dhampir timing encoder"),
            });
        renderer.render(&mut encoder, &ctx.queue, &view, TIMING_FRAME);
        ctx.queue.submit([encoder.finish()]);
        frames.push(millis(started.elapsed()));

        let started = Instant::now();
        render_and_read(ctx, &renderer, &view, &texture, TIMING_FRAME)?;
        readbacks.push(millis(started.elapsed()));
    }

    // 上面第一次 `render_and_read` 把渲染也做了一遍，所以读回时间里含一次重复的渲染——
    // 这会**高估**读回时间。宁可高估：读回不是退出标准里的那个数字，而单帧那条
    // （`frames`）是干净地量出来的。
    let frame = measurement(&frames).ok_or("一帧都没测到")?;
    let readback = measurement(&readbacks).ok_or("读回一次都没测到")?;
    Ok(SceneTiming {
        scene: spec.name,
        frame,
        readback,
    })
}

/// 渲染一帧并读回，丢弃图像。计时循环里只要"做完了这件事"。
fn render_and_read(
    ctx: &GpuContext,
    renderer: &SceneRenderer,
    view: &wgpu::TextureView,
    texture: &wgpu::Texture,
    frame: u32,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut encoder = ctx
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("dhampir timing encoder"),
        });
    renderer.render(&mut encoder, &ctx.queue, view, frame);
    ctx.queue.submit([encoder.finish()]);
    pollster::block_on(read_texture_rgba8(&ctx.device, &ctx.queue, texture))?;
    Ok(())
}

/// 建一张能渲染也能读回的目标纹理。
fn create_target(ctx: &GpuContext, size: (u32, u32), label: &str) -> wgpu::Texture {
    ctx.device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width: size.0,
            height: size.1,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: SCENE_TARGET_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    })
}

// ---------------------------------------------------------------------------
// 行对齐探针
// ---------------------------------------------------------------------------

/// 行对齐探针的结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RowAlignment {
    pub size: (u32, u32),
    /// `width * 4`。
    pub unpadded_bytes_per_row: u32,
    /// 向上取整到 [`COPY_BYTES_PER_ROW_ALIGNMENT`] 之后的值。
    pub padded_bytes_per_row: u32,
    /// 这个宽度**真的**需要行填充吗。为假时本趟探针什么也没证明。
    pub exercises_padding: bool,
    pub frame: u32,
    /// 参与复核的像素数，以及其中对上的。
    pub pixels_compared: usize,
    pub pixels_ok: usize,
    /// 逐像素最大字节距离。
    pub worst_distance: u8,
    /// 最多 [`MAX_EXAMPLES`] 条失败样例。
    pub examples: Vec<String>,
    pub ok: bool,
    /// `ok` 为假时的原因；`ok` 为真时是 `None`。
    pub detail: Option<String>,
}

/// 逐像素复核一张读回图，看它是否与 `checker` 的模型处处吻合。
///
/// **这是本模块唯一能真正抓住"行错位"的东西。** 别的检查都抓不住它：
/// 尺寸对不对、字节数对不对，在错位时照样全对；两次渲染互比也一样——
/// 同一个错误会稳定地重复。只有拿**独立答案**（按像素坐标定义、与尺寸无关的
/// 场景模型）逐像素比，才能让一整块错位的像素现形。
///
/// 复核用 [`BYTE_TOLERANCE`] 那一条线，和其它判定一致。
pub fn examine_alignment(image: &Rgba8Image, frame: u32) -> RowAlignment {
    let size = (image.width, image.height);
    let unpadded = image.width * 4;
    let padded = padded_bytes_per_row(unpadded, COPY_BYTES_PER_ROW_ALIGNMENT);
    let mut result = RowAlignment {
        size,
        unpadded_bytes_per_row: unpadded,
        padded_bytes_per_row: padded,
        exercises_padding: padded != unpadded,
        frame,
        pixels_compared: 0,
        pixels_ok: 0,
        worst_distance: 0,
        examples: Vec::new(),
        ok: false,
        detail: None,
    };

    // 一张"本来就不需要行填充"的图，通过与否都不说明任何事。这跟空文件集不能算
    // 全绿是同一个道理：**没有分辨力的检查不能算通过**。
    if !result.exercises_padding {
        result.detail = Some(format!(
            "{} 宽的一行是 {unpadded} 字节，正好是 {COPY_BYTES_PER_ROW_ALIGNMENT} 的倍数——\
             这趟探针证明不了任何关于行填充的事",
            size.0
        ));
        return result;
    }

    let expected_len = (u64::from(size.0) * u64::from(size.1) * 4) as usize;
    if image.pixels.len() != expected_len {
        result.detail = Some(format!(
            "图像有 {} 字节，{size:?} 该有 {expected_len} 字节——去行填充那一步写错了",
            image.pixels.len()
        ));
        return result;
    }

    for y in 0..size.1 {
        for x in 0..size.0 {
            let expected = scene_model::bytes_of_linear_rgba(scene_model::checker_linear(frame, x, y));
            let Some(measured) = image.pixel(x, y) else {
                result.detail = Some(format!("({x}, {y}) 取不到像素"));
                return result;
            };
            result.pixels_compared += 1;
            let distance = scene_model::distance_bytes(measured, expected);
            result.worst_distance = result.worst_distance.max(distance);
            if distance <= BYTE_TOLERANCE {
                result.pixels_ok += 1;
            } else if result.examples.len() < MAX_EXAMPLES {
                result.examples.push(format!(
                    "({x}, {y}) 实测 {measured:?} 应为 {expected:?}，差 {distance} 字节"
                ));
            }
        }
    }

    // 两个条件都要：比对过（非空）且全对。`pixels_compared == 0` 在尺寸非零时不可能，
    // 但"不可能"正是需要写下来的东西——将来谁把循环改坏了，这里会红。
    result.ok = result.pixels_compared > 0 && result.pixels_ok == result.pixels_compared;
    if !result.ok {
        result.detail = Some(format!(
            "{} / {} 个像素对上（最大字节距离 {}，容差 {BYTE_TOLERANCE}）",
            result.pixels_ok, result.pixels_compared, result.worst_distance
        ));
    }
    result
}

/// 在 [`ALIGN_PROBE_SIZE`] 上渲染 [`ALIGN_PROBE_SCENE`] 并复核读回。
pub fn probe_alignment(
    ctx: &GpuContext,
    spec: &'static SceneSpec,
) -> Result<RowAlignment, Box<dyn std::error::Error>> {
    let renderer = SceneRenderer::new_at(&ctx.device, SCENE_TARGET_FORMAT, spec, ALIGN_PROBE_SIZE);
    let texture = create_target(ctx, renderer.size(), "dhampir alignment target");
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

    let mut encoder = ctx
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("dhampir alignment encoder"),
        });
    renderer.render(&mut encoder, &ctx.queue, &view, ALIGN_PROBE_FRAME);
    ctx.queue.submit([encoder.finish()]);

    let image = pollster::block_on(read_texture_rgba8(&ctx.device, &ctx.queue, &texture))?;
    Ok(examine_alignment(&image, ALIGN_PROBE_FRAME))
}

// ---------------------------------------------------------------------------
// 组装记录
// ---------------------------------------------------------------------------

/// 一次运行的环境基线。
#[derive(Clone, Debug)]
pub struct Baseline {
    pub adapter: AdapterIdentity,
    pub init: Duration,
    pub timings: Vec<SceneTiming>,
    pub alignment: RowAlignment,
    pub epoch_millis: u64,
}

impl Baseline {
    /// 所有场景里最慢的那个"CPU 侧编码 + 提交"（中位数口径，毫秒）。
    ///
    /// **这个数不含 GPU 干活的时间**（`submit` 是异步的，画完之前它早就返回了）。
    /// 它回答的是"CPU 把一帧交出去要多久"，不是"一帧画完要多久"。
    pub fn worst_frame_ms(&self) -> Option<f64> {
        worst_by(self, |t| t.frame.median_ms)
    }

    /// 所有场景里最慢的那一趟"渲染 + 读回"往返（中位数口径，毫秒）。
    ///
    /// 这个数**含** GPU 把这一帧画完的时间：读回要等 `copy_texture_to_buffer` 完成，
    /// 而那必须等渲染结束。它同时也含一次 1080p 的显存→内存拷贝与映射等待——那是
    /// 比渲染更重的一段。**所以它是"GPU 画一帧花了多久"的一个高估上界**：
    /// 一个 < 10 ms 的往返，足以证明它的两个组成部分（渲染、读回）各自也 < 10 ms。
    pub fn worst_readback_ms(&self) -> Option<f64> {
        worst_by(self, |t| t.readback.median_ms)
    }

    /// 单帧预算的结论。三态：`true` / `false` / `None`（**没验**）。
    ///
    /// 判据用 [`Self::worst_readback_ms`]（往返），**不用** [`Self::worst_frame_ms`]
    /// （纯 CPU 提交）。理由是这两个数能证明的事情不一样：
    ///
    /// - CPU 提交那个数**没有能力否证**"一帧画完 ≤ 10 ms"——它压根不含 GPU 那一段。
    ///   拿它判"通过"，等于用一个不测这件事的仪器宣布这件事合格。
    /// - 往返那个数**有能力否证**：它含渲染。它过了，渲染就必然也过了。
    ///
    /// 纯 GPU 时间没有被直接测量（要 `TIMESTAMP_QUERY`，而它不是一个到处都有的能力，
    /// 缺了就得换一套测量口径——那会让记录在不同机器上不可比）。**测不准就选会高估
    /// 的那一个**：高估的判据只会漏报，低估的判据会误报。
    ///
    /// 在 debug 构建下不判：那里的数学运算没优化，数字会比 release 慢一个数量级，
    /// 拿它去对预算只会得出"预算太紧"这个错误结论。这时候记 `null`，
    /// 并在 `verdict_note` 里说清为什么——"没验"与"验过通过"是两件事。
    pub fn budget_verdict(&self) -> Option<bool> {
        if cfg!(debug_assertions) {
            return None;
        }
        self.worst_readback_ms().map(|ms| ms <= FRAME_BUDGET_MS)
    }
}

/// 按某个字段取"所有场景里最慢的那个"（中位数口径）。
fn worst_by(baseline: &Baseline, value: impl Fn(&SceneTiming) -> f64) -> Option<f64> {
    baseline
        .timings
        .iter()
        .map(&value)
        .fold(None, |acc: Option<f64>, v| {
            Some(acc.map_or(v, |a| a.max(v)))
        })
}

/// 采集一次环境基线：1080p 计时 + 行对齐探针。
///
/// `init` 由调用方计（从建 `Instance` 开始，见 [`open_leg`]），因为那一段发生在本函数
/// 之前。`epoch_millis` **也由调用方给**：同一条腿的 `adapter.json` 要在这之前就写下去，
/// 而两份记录必须带**同一个**时间戳，否则没人能证明它们是同一次运行的两个部分。
pub fn measure(
    ctx: &GpuContext,
    requested: wgpu::Backends,
    specs: &[&'static SceneSpec],
    init: Duration,
    epoch_millis: u64,
) -> Result<Baseline, Box<dyn std::error::Error>> {
    let mut timings = Vec::with_capacity(specs.len());
    for spec in specs {
        timings.push(time_scene(ctx, spec, TIMING_SIZE)?);
    }

    let align_spec = specs
        .iter()
        .find(|s| s.name == ALIGN_PROBE_SCENE)
        .ok_or_else(|| format!("场景集里没有 {ALIGN_PROBE_SCENE}，行对齐探针无场景可用"))?;
    let alignment = probe_alignment(ctx, align_spec)?;

    Ok(Baseline {
        adapter: AdapterIdentity::from_context(ctx, requested),
        init,
        timings,
        alignment,
        epoch_millis,
    })
}

/// 读一次系统时钟（Unix 毫秒）。
///
/// 读不到时返回 **0**，而 0 在记录里表现为 `null`（见 [`epoch_seconds`]）——
/// 编一个假的时间戳会让"这份记录是什么时候写的"变成一句谎话。
pub fn unix_epoch_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// `epoch_millis` → 记录里"秒"那一栏。0（读不到时钟）写成 `null`。
/// **实现在 core**（[`dhampir_core::render::corpus::epoch_seconds`]）。
///
/// 秒与毫秒都给：M0 的记录里就是秒（整数），跨记录对时间时毫秒更有用。
///
/// 搬进 core 的理由：M2 的浏览器腿也要写这一对键，而它那边的时钟是页面从
/// `Date.now()` 传进来的——"0 要写成 null"这条 M0 就定下的规矩必须两条腿都成立，
/// 不能一边一个实现。
fn epoch_seconds(epoch_millis: u64) -> serde_json::Value {
    dhampir_core::render::corpus::epoch_seconds(epoch_millis)
}

/// `adapter.json` 的内容：**这条腿是在什么环境里跑的**。**纯函数**。
///
/// 只依赖 adapter 身份与时间戳，所以能在**计时之前**就写下去。计时那一段是整个 M1 里
/// 最长的一段，它中途失败（驱动挂了、场景没建起来）时，这个文件已经落盘："
/// 在哪个 adapter 上失败的"于是仍然有据可查。这正是它不跟 `timing.json` 合成一个
/// 文件的原因——合成一个，就等于把"环境"押在"跑得快不快"的成功之上。
///
/// 两份记录共用同一个 `unix_epoch_millis`，谁都能看出它们属于同一次运行。
pub fn adapter_json(adapter: &AdapterIdentity, epoch_millis: u64) -> serde_json::Value {
    serde_json::json!({
        "schema": 1,
        "milestone": "M1",
        "kind": "adapter",
        "build_profile": build_profile(),
        "adapter": adapter.to_json(),
        "adapter_name": adapter.name(),
        // 记录里的后端名与产物**目录名**同源（`baseline::backend_label` / `backend_slug`）：
        // M0 那批记录里是 `Backends(DX12)`（wgpu 的内部写法），M1 起统一成 `DX12`。
        "requested_backends": backend_label(adapter.requested),
        "backend_slug": backend_slug(adapter.requested),
        "target_format": format!("{SCENE_TARGET_FORMAT:?}"),
        "corpus_target_size": format!("{}x{}", SCENE_TARGET_SIZE.0, SCENE_TARGET_SIZE.1),
        "crate_version": env!("CARGO_PKG_VERSION"),
        "wgpu_version": wgpu_version(),
        "naga_version": naga_version(),
        "probe_digest": format!("{:016x}", timeline::probe_digest()),
        "probe_format_version": timeline::PROBE_FORMAT_VERSION,
        "unix_epoch_seconds": epoch_seconds(epoch_millis),
        "unix_epoch_millis": epoch_millis,
        // 记录自己声明自己的非确定项：复核的人不必先读完这个文件才知道哪里会变。
        "nondeterministic_fields": [
            "unix_epoch_seconds",
            "unix_epoch_millis",
            "build_profile（换构建就变）",
            "adapter.* / adapter_name（换机器就变）",
        ],
    })
}

/// `timing.json` 的内容：**跑起来多快**。**纯函数**。
///
/// 与 `adapter.json` 分开写，是因为两者回答的问题不同、且**能变的东西不同**：
/// 计时每次都变，"这是在什么环境里跑的"几乎不变。混在一个文件里，等于让
/// "这台机器的 adapter 是什么"这个问题的答案每次 diff 都在抖。
pub fn timing_json(baseline: &Baseline) -> serde_json::Value {
    let timings: Vec<serde_json::Value> = baseline
        .timings
        .iter()
        .map(|t| {
            serde_json::json!({
                "scene": t.scene,
                "frame_cpu_ms": measurement_json(Some(t.frame)),
                "readback_ms": measurement_json(Some(t.readback)),
            })
        })
        .collect();

    let worst_cpu = baseline.worst_frame_ms();
    let worst_roundtrip = baseline.worst_readback_ms();
    let verdict = baseline.budget_verdict();

    serde_json::json!({
        "schema": 1,
        "milestone": "M1",
        "kind": "timing",
        "build_profile": build_profile(),
        "requested_backends": backend_label(baseline.adapter.requested),
        "backend_slug": backend_slug(baseline.adapter.requested),
        "adapter_name": baseline.adapter.name(),
        "unix_epoch_seconds": epoch_seconds(baseline.epoch_millis),
        "unix_epoch_millis": baseline.epoch_millis,
        "init_ms": round3(millis(baseline.init)),
        "timing_target_size": format!("{}x{}", TIMING_SIZE.0, TIMING_SIZE.1),
        "align_probe": {
            "scene": ALIGN_PROBE_SCENE,
            "size": format!("{}x{}", ALIGN_PROBE_SIZE.0, ALIGN_PROBE_SIZE.1),
            "frame": ALIGN_PROBE_FRAME,
            // 1366 挑得有意：1920 的一行正好整除 256，那样的宽度测不出对齐错误。
            "unpadded_bytes_per_row": ALIGN_PROBE_SIZE.0 * 4,
            "padded_bytes_per_row": padded_bytes_per_row(ALIGN_PROBE_SIZE.0 * 4, COPY_BYTES_PER_ROW_ALIGNMENT),
        },
        "timing": {
            "size": format!("{}x{}", TIMING_SIZE.0, TIMING_SIZE.1),
            "frame": TIMING_FRAME,
            "warmup": TIMING_WARMUP,
            "repeats": TIMING_REPEATS,
            "scenes": timings,
            // 两个数都记，但**名字说清各是什么**：CPU 提交那个不含 GPU，
            // 往返那个含。只记一个的话，"这一帧到底多快"就只能靠字段名去猜。
            "worst_frame_cpu_ms": worst_cpu.map(round3),
            "worst_roundtrip_ms": worst_roundtrip.map(round3),
            "budget_ms": FRAME_BUDGET_MS,
            "budget_metric": "worst_roundtrip_ms",
            "budget_metric_note": "判的是「渲染 + 读回」往返（含 GPU 画完那一段，故为渲染时间的高估上界）。worst_frame_cpu_ms 不含 GPU，没有能力否证这条预算，所以只记不判。",
            "verdict": verdict,
            "verdict_note": match verdict {
                None => serde_json::Value::from(
                    "没判：debug 构建的计时不能用来对预算（数学运算没优化）".to_string(),
                ),
                Some(_) => serde_json::Value::Null,
            },
        },
        "row_alignment": alignment_json(&baseline.alignment),
        "nondeterministic_fields": [
            "unix_epoch_seconds",
            "unix_epoch_millis",
            "init_ms",
            "timing.*",
            "row_alignment.ok / row_alignment.detail（只在硬件真的错位时才会变）",
            "build_profile（换构建就变）",
        ],
    })
}

/// 一次测量的结构化记录。
pub fn measurement_json(measurement: Option<Measurement>) -> serde_json::Value {
    match measurement {
        Some(m) => serde_json::json!({
            "n": m.n,
            "median_ms": round3(m.median_ms),
            "min_ms": round3(m.min_ms),
            "max_ms": round3(m.max_ms),
        }),
        None => serde_json::json!({
            "n": 0,
            "median_ms": serde_json::Value::Null,
            "min_ms": serde_json::Value::Null,
            "max_ms": serde_json::Value::Null,
        }),
    }
}

/// 行对齐探针的结构化记录。
pub fn alignment_json(alignment: &RowAlignment) -> serde_json::Value {
    serde_json::json!({
        "size": format!("{}x{}", alignment.size.0, alignment.size.1),
        "frame": alignment.frame,
        "unpadded_bytes_per_row": alignment.unpadded_bytes_per_row,
        "padded_bytes_per_row": alignment.padded_bytes_per_row,
        "exercises_padding": alignment.exercises_padding,
        "pixels_compared": alignment.pixels_compared,
        "pixels_ok": alignment.pixels_ok,
        "worst_distance": alignment.worst_distance,
        "byte_tolerance": BYTE_TOLERANCE,
        "examples": alignment.examples,
        "ok": alignment.ok,
        "detail": alignment.detail,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `build.rs` 的守卫。它读不到 `Cargo.lock` 时会静默地把版本写成 `unknown`——
    /// 那正是"记录里写了一个不存在的版本号"的近亲。这条测试让那种失效变红。
    #[test]
    fn versions_are_real_numbers() {
        for (label, version) in [
            ("wgpu", wgpu_version()),
            ("naga", naga_version()),
        ] {
            assert_ne!(version, "unknown", "{label} 的版本没从 Cargo.lock 里取到");
            let mut parts = version.split('.');
            for part in [
                parts.next().unwrap_or_default(),
                parts.next().unwrap_or_default(),
            ] {
                assert!(
                    !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()),
                    "{label} 的版本号看着不像版本号：{version}"
                );
            }
        }
    }

    #[test]
    fn backend_slug_is_lowercase() {
        assert_eq!(backend_slug(wgpu::Backends::DX12), "dx12");
        assert_eq!(backend_slug(wgpu::Backends::VULKAN), "vulkan");
    }

    /// 记录里那一栏要是 `DX12`，不是 `Backends(DX12)`。
    ///
    /// 这条断言的存在理由：`{:?}` 出来的东西**看起来也是对的**——它确实含
    /// "DX12" 四个字符。只有把"不许出现包装"本身写下来，才会有人发现
    /// `records/` 里躺着的是 wgpu 的内部形态。
    #[test]
    fn backend_label_is_not_the_debug_wrapper() {
        assert_eq!(backend_label(wgpu::Backends::DX12), "DX12");
        assert_eq!(backend_label(wgpu::Backends::VULKAN), "VULKAN");
        for backends in [wgpu::Backends::DX12, wgpu::Backends::VULKAN] {
            let label = backend_label(backends);
            assert!(!label.contains("Backends"), "{label}");
            assert!(!label.contains('('), "{label}");
        }
    }

    /// 目录名必须**始终**是合法文件名——包括"多后端位或"这种情况。
    ///
    /// 撞名比难看严重得多：两个后端的产物落进同一个目录，先跑的那份就被盖掉了，
    /// 而记录里两行 `files` 会指向同一个文件。
    #[test]
    fn backend_slug_is_a_legal_directory_name() {
        for backends in [wgpu::Backends::DX12, wgpu::Backends::VULKAN] {
            let slug = backend_slug(backends);
            assert!(!slug.is_empty(), "空目录名会把两个后端混到一起");
            assert!(
                slug.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()),
                "{slug}"
            );
        }

        let both = backend_slug(wgpu::Backends::DX12 | wgpu::Backends::VULKAN);
        assert_ne!(both, backend_slug(wgpu::Backends::DX12));
        assert_ne!(both, backend_slug(wgpu::Backends::VULKAN));
        assert!(
            both
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
            "{both}"
        );
    }

    #[test]
    fn measurement_takes_the_median() {
        let odd = measurement(&[3.0, 1.0, 2.0]).unwrap();
        assert_eq!(
            odd,
            Measurement {
                n: 3,
                median_ms: 2.0,
                min_ms: 1.0,
                max_ms: 3.0
            }
        );

        // 偶数个取中间两个的平均——不是"取靠前的那个"，那会让中位数偏小。
        let even = measurement(&[4.0, 1.0, 3.0, 2.0]).unwrap();
        assert_eq!(even.median_ms, 2.5);
        assert_eq!(even.n, 4);

        // 一次都没测到 → 没有数字，而不是 0.0。0.0 会被读成"快得不花时间"。
        assert_eq!(measurement(&[]), None);
    }

    #[test]
    fn json_milliseconds_are_rounded() {
        assert_eq!(round3(0.123_456), 0.123);
        assert_eq!(round3(0.999_9), 1.0);
        assert_eq!(round3(12.0), 12.0);
    }

    /// 按模型画出来的 `checker` 图 —— 探针的"正确输入"。
    fn model_checker_image(size: (u32, u32), frame: u32) -> Rgba8Image {
        let mut pixels = Vec::with_capacity((size.0 * size.1 * 4) as usize);
        for y in 0..size.1 {
            for x in 0..size.0 {
                pixels.extend_from_slice(&scene_model::bytes_of_linear_rgba(
                    scene_model::checker_linear(frame, x, y),
                ));
            }
        }
        Rgba8Image {
            width: size.0,
            height: size.1,
            pixels,
        }
    }

    /// 一组"行需要填充"的尺寸。70 × 4 = 280 字节一行 → 填到 512。
    const PADDED_SIZE: (u32, u32) = (70, 4);

    #[test]
    fn alignment_probe_passes_on_the_model_itself() {
        let image = model_checker_image(PADDED_SIZE, ALIGN_PROBE_FRAME);
        let result = examine_alignment(&image, ALIGN_PROBE_FRAME);
        assert!(result.exercises_padding, "{result:?}");
        assert!(result.ok, "{result:?}");
        assert_eq!(result.pixels_compared, (PADDED_SIZE.0 * PADDED_SIZE.1) as usize);
        assert_eq!(result.pixels_ok, result.pixels_compared);
        assert_eq!(result.worst_distance, 0);
        assert!(result.detail.is_none());
        assert_eq!(result.padded_bytes_per_row, 512);
    }

    /// **这条测试是探针存在的理由。** 把某一行的内容整体左移 1 像素（这正是"用错了
    /// 行跨度"在图上留下的痕迹），探针必须炸，而且要说清差在哪。
    #[test]
    fn alignment_probe_catches_a_row_shift() {
        let image = model_checker_image(PADDED_SIZE, ALIGN_PROBE_FRAME);
        let mut shifted = image.clone();
        let row = 1_usize;
        let stride = (PADDED_SIZE.0 * 4) as usize;
        let start = row * stride;
        shifted.pixels.copy_within(start + 4..start + stride, start);

        let result = examine_alignment(&shifted, ALIGN_PROBE_FRAME);
        assert!(!result.ok, "行错位必须被抓住：{result:?}");
        assert!(result.pixels_ok < result.pixels_compared);
        assert!(result.worst_distance > BYTE_TOLERANCE);
        assert!(!result.examples.is_empty());
        assert!(result.detail.is_some());
    }

    /// 宽度本来就不需要填充时，探针要**自己承认没证明什么**，而不是报通过。
    #[test]
    fn alignment_probe_refuses_a_width_that_needs_no_padding() {
        let image = model_checker_image((64, 4), ALIGN_PROBE_FRAME);
        let result = examine_alignment(&image, ALIGN_PROBE_FRAME);
        assert!(!result.exercises_padding);
        assert!(!result.ok, "没有分辨力的探针不能算通过：{result:?}");
        let detail = result.detail.unwrap();
        assert!(detail.contains("证明不了"), "{detail}");
    }

    /// 字节数不对（去填充那一步把 stride 用错了长度）要被当场抓住。
    #[test]
    fn alignment_probe_catches_a_wrong_byte_count() {
        let mut image = model_checker_image(PADDED_SIZE, ALIGN_PROBE_FRAME);
        image.pixels.truncate(image.pixels.len() - 4);
        let result = examine_alignment(&image, ALIGN_PROBE_FRAME);
        assert!(!result.ok);
        assert_eq!(result.pixels_compared, 0, "长度不对就不该开始逐像素比");
        assert!(result.detail.unwrap().contains("字节"));
    }

    fn test_adapter() -> AdapterIdentity {
        AdapterIdentity {
            requested: wgpu::Backends::DX12,
            fields: vec![("name", "测试 adapter".to_string()), ("backend", "Dx12".into())],
        }
    }

    /// 两个场景的**两个数故意交叉**：CPU 提交最慢的是 `blur`（9.0），往返最慢的是
    /// `gradient`（40.0）。这样"判定到底用了哪一个数"可以从结论本身读出来——
    /// 两个数若同增同减，用错一个也看不出来。
    fn test_baseline() -> Baseline {
        Baseline {
            adapter: test_adapter(),
            init: Duration::from_millis(1234),
            timings: vec![
                SceneTiming {
                    scene: "gradient",
                    frame: measurement(&[1.0, 2.0, 3.0]).unwrap(),
                    readback: measurement(&[40.0]).unwrap(),
                },
                SceneTiming {
                    scene: "blur",
                    frame: measurement(&[9.0]).unwrap(),
                    readback: measurement(&[5.0]).unwrap(),
                },
            ],
            alignment: examine_alignment(
                &model_checker_image(PADDED_SIZE, ALIGN_PROBE_FRAME),
                ALIGN_PROBE_FRAME,
            ),
            epoch_millis: 1_790_000_000_123,
        }
    }

    /// 一个只有一个场景、两个数都指定的基线。用来单独摆布"CPU 提交"与"往返"。
    fn baseline_with(frame_ms: f64, readback_ms: f64) -> Baseline {
        let mut baseline = test_baseline();
        baseline.timings = vec![SceneTiming {
            scene: "gradient",
            frame: measurement(&[frame_ms]).unwrap(),
            readback: measurement(&[readback_ms]).unwrap(),
        }];
        baseline
    }

    #[test]
    fn worst_frame_is_the_slowest_scene() {
        let baseline = test_baseline();
        assert_eq!(baseline.worst_frame_ms(), Some(9.0));
        assert_eq!(
            baseline.worst_readback_ms(),
            Some(40.0),
            "两个数取的是不同的场景，用错一个就会取到另一场景"
        );
        let json = timing_json(&baseline);
        assert_eq!(json["timing"]["worst_frame_cpu_ms"], 9.0);
        assert_eq!(json["timing"]["worst_roundtrip_ms"], 40.0);
        assert_eq!(json["timing"]["budget_ms"], FRAME_BUDGET_MS);
        // 时间戳按 M0 的规矩拆成两个键：秒是整数（M0 记录里就是整数），毫秒是新键。
        assert_eq!(json["unix_epoch_seconds"], 1_790_000_000_u64);
        assert_eq!(json["unix_epoch_millis"], 1_790_000_000_123_u64);
        assert_eq!(json["init_ms"], 1234.0);
        assert_eq!(json["adapter_name"], "测试 adapter");
        assert_eq!(json["requested_backends"], "DX12");
        assert_eq!(json["row_alignment"]["ok"], true);
        // 行对齐探针那一栏自己带上"为什么这个宽度测得出问题"：1366×4 = 5464 字节，
        // 不是 256 的倍数，要填到 5632。写在这里比写在文档里更难被改坏。
        assert_eq!(json["align_probe"]["unpadded_bytes_per_row"], 5464);
        assert_eq!(json["align_probe"]["padded_bytes_per_row"], 5632);
    }

    /// **预算判的是往返，不是 CPU 提交。**
    ///
    /// 摆两组数，每一组里"该看的那个数"与"不该看的那个数"给出**相反**的结论：
    ///
    /// | | CPU 提交 | 往返 | 只有往返能证否"一帧画完 ≤ 10 ms" |
    /// |---|---|---|---|
    /// | 第一组 | 9.0（达标） | 40.0（超标） | 10.0 只是预算，40.0 才是"画完要多久" |
    /// | 第二组 | 99.0（超标） | 1.0（达标） | CPU 慢不等于 GPU 慢 |
    ///
    /// 断言两组都按**往返**给结论。用错那个数时，两次断言会一起反过来。
    #[test]
    fn the_budget_judges_the_roundtrip_not_the_cpu_submit() {
        if cfg!(debug_assertions) {
            // debug 构建下两个都不判，这条测试没有分辨力——明说，不假装验过。
            return;
        }
        assert_eq!(
            baseline_with(9.0, 40.0).budget_verdict(),
            Some(false),
            "CPU 提交达标、往返超标 → 必须不过"
        );
        assert_eq!(
            baseline_with(99.0, 1.0).budget_verdict(),
            Some(true),
            "CPU 接一个慢活不等于 GPU 画得慢 → 不该因此判不过"
        );
    }

    /// 记录里要写明**判的是哪个数**：只写 `verdict` 与 `budget_ms` 的话，
    /// 读的人只能猜那个 10 ms 是对着哪个数字比的。
    #[test]
    fn the_record_names_the_metric_it_judged() {
        let json = timing_json(&test_baseline());
        assert_eq!(json["timing"]["budget_metric"], "worst_roundtrip_ms");
        assert!(
            json["timing"]["budget_metric_note"]
                .as_str()
                .unwrap()
                .contains("高估"),
            "{json}"
        );
        assert!(!json["timing"]["budget_metric_note"]
            .as_str()
            .unwrap()
            .contains("worst_frame_cpu_ms 是判据"));
    }

    /// 两份记录回答**两个不同的问题**：`adapter.json` 里没有计时，`timing.json` 里
    /// 没有"这台机器是什么"。
    ///
    /// **这不等价于"键不重叠"**（先前这句注释这么写过，M1 独立复核实测后改正）：
    /// 两份有 10 个同名骨架键（`schema` / `milestone` / `kind` / `unix_epoch_millis` …），
    /// 其中 `kind` 与 `nondeterministic_fields` 两键值还不同。要守住的是下面那两组
    /// 断言——**对方的专有内容不许出现在自己这里**；"这个数该信哪一份"因此不会
    /// 变成问题。同一次运行的两份还必须能被认出来是一对（时间戳、后端名对齐）。
    #[test]
    fn the_two_records_do_not_answer_the_same_question() {
        let baseline = test_baseline();
        let adapter = adapter_json(&baseline.adapter, baseline.epoch_millis);
        let timing = timing_json(&baseline);

        for key in ["init_ms", "timing", "row_alignment"] {
            assert!(adapter.get(key).is_none(), "adapter.json 里不该有 {key}");
        }
        for key in [
            "adapter",
            "wgpu_version",
            "naga_version",
            "probe_digest",
            "corpus_target_size",
        ] {
            assert!(timing.get(key).is_none(), "timing.json 里不该有 {key}");
        }

        // 同一次运行的两份记录必须能被认出来是一对：时间戳与后端名都要对得上。
        assert_eq!(adapter["unix_epoch_millis"], timing["unix_epoch_millis"]);
        assert_eq!(adapter["requested_backends"], timing["requested_backends"]);
        assert_eq!(adapter["backend_slug"], timing["backend_slug"]);
        assert!(!adapter["backend_slug"].as_str().unwrap().is_empty());

        // 两份各自声明自己的非确定项，且都没有把确定项说成不确定的。
        for json in [&adapter, &timing] {
            let listed = json["nondeterministic_fields"]
                .as_array()
                .unwrap()
                .iter()
                .map(|f| f.as_str().unwrap())
                .collect::<Vec<_>>()
                .join(" ");
            assert!(listed.contains("unix_epoch"), "{listed}");
            assert!(!listed.contains("wgpu_version"), "{listed}");
            assert!(!listed.contains("probe_digest"), "{listed}");
        }
    }

    /// `adapter.json` 只依赖 adapter 与时间戳——**计时还没跑就能写**。
    ///
    /// 这条测试把那个性质钉在类型上：给它一个 adapter（而不是一整份 `Baseline`）就够。
    /// 计时那一段是整个 M1 里最长的一段，它中途挂掉时，这个文件必须已经在盘上了。
    #[test]
    fn the_adapter_record_needs_no_measurement() {
        let adapter = test_adapter();
        let json = adapter_json(&adapter, 1_790_000_000_123);
        assert_eq!(json["adapter_name"], "测试 adapter");
        assert_eq!(json["adapter"]["backend"], "Dx12");
        assert_eq!(json["backend_slug"], "dx12");
        assert_eq!(json["crate_version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(json["wgpu_version"], wgpu_version());
        assert_eq!(json["kind"], "adapter");
    }

    /// 读不到时钟时记 `null`，**不是** 1970 年。
    ///
    /// `0` 与"unix 纪元那一刻真的写了这份记录"长得一模一样——那正是编造数据。
    #[test]
    fn an_unreadable_clock_is_null_not_1970() {
        let json = adapter_json(&test_adapter(), 0);
        assert!(json["unix_epoch_seconds"].is_null(), "{json}");
        assert_eq!(json["unix_epoch_millis"], 0);
    }

    /// 预算判定是三态的：debug 构建下**没判**，不是"通过"。
    ///
    /// 断言按当前构建 profile 走，所以 `cargo test --release` 也成立。
    #[test]
    fn budget_verdict_is_three_state() {
        let baseline = test_baseline();
        let json = timing_json(&baseline);
        assert_eq!(json["timing"]["verdict"].is_null(), cfg!(debug_assertions));
        assert_eq!(baseline.budget_verdict().is_none(), cfg!(debug_assertions));
        if cfg!(debug_assertions) {
            assert!(
                json["timing"]["verdict_note"]
                    .as_str()
                    .unwrap()
                    .contains("debug"),
                "{json}"
            );
        } else {
            // release 下这个基线的往返是 40.0 ms > 预算 10.0：必须判**不过**。
            // 判成通过，就说明判定用错了数（CPU 提交那个是 9.0，达标）。
            assert_eq!(baseline.budget_verdict(), Some(false), "{json}");
            assert!(json["timing"]["verdict_note"].is_null(), "{json}");
        }
    }
}
