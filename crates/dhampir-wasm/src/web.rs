//! 浏览器宿主：`wasm-bindgen` 导出、canvas surface、WebGPU 后端。
//!
//! 只有 `wasm32` 会编译这个模块（Cargo.toml 里的 target-specific 依赖负责这件事）。
//!
//! # 整个 crate 里唯一的分叉
//!
//! [`new_instance`] 里的那一行。它与 native 侧
//! [`dhampir_core::gpu::NATIVE_BACKENDS`] 对称，除此之外两个宿主的初始化路径
//! 都走 [`dhampir_core::gpu::request_context`]——一字不差。
//!
//! # 为什么不从 canvas 抄像素
//!
//! M0 就同时提供两条路径：
//!
//! - [`dhampir_probe_render_canvas`]：画到 canvas 给用户看
//! - [`dhampir_probe_offscreen_png`]：画到离屏纹理 → `copy_texture_to_buffer` → PNG
//!
//! 第二条是为 M2 准备的：canvas 纹理通常没有 `COPY_SRC` 用途，根本读不回来；
//! 而且指导文档要求比对在"编码后的字节"上进行。现在就把它跑通，
//! 是为了让 M2 的失败只剩下"两端渲染不同"这一个原因。
//!
//! # 为什么这里是取证工具，而不是底座 API
//!
//! 这一整个模块是 **M0–M2 的验收证据**，不是给下游用的接口：
//! `crates/dhampir-wasm/www/index.html` 是 M0 的浏览器腿（`records/m0/README.md`
//! 把它钉成 `harness: crates/dhampir-wasm/www/index.html`，产物是
//! `records/m0/browser-harness.json` + 三张 PNG），M2 又复用同一条腿。
//! 所以**「web/ 没调用」不是删它的理由** —— 调用方在仓库内部的验收链上。
//!
//! 收口时删掉的只有一个：`dhampir_probe_verify`。它是全仓唯一一个连
//! 取证链都没接的导出（页面调的是 `dhampir_probe_golden_check`，比的是
//! **编进 crate 的** golden，不需要先有另一端跑过一遍）。删它不丢能力：
//! 它直通的 `ProbeSummary::verify_against` 在 `crate::probe` 里有单测
//! （`verify_reports_which_side_differs`），而"与另一端对摘要"这条路
//! 由 `crates/dhampir-wasm/tests/cross_runtime.rs` 在 Rust 侧真跑着
//! （它拿 native 写的摘要去 `verify_golden`）。

use std::cell::RefCell;

use dhampir_core::gpu::{self, GpuContext};
use dhampir_core::render::{PROBE_CLEAR_COLOR, PROBE_TARGET_SIZE, ProbeRenderer};
use dhampir_core::{readback, wgpu};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use web_sys::{HtmlCanvasElement, console};

use crate::probe::ProbeSummary;

// ---------------------------------------------------------------------------
// 初始化
// ---------------------------------------------------------------------------

/// 把 `log` 的输送到浏览器控制台。
///
/// wgpu 会打相当多有价值的日志（后端选择、适配器能力、校验失败的具体原因）。
/// 不接这一层的话，浏览器里出问题时你只能看到一句 `JsValue(undefined)`。
struct ConsoleLogger;

impl log::Log for ConsoleLogger {
    fn enabled(&self, _: &log::Metadata<'_>) -> bool {
        true
    }

    fn log(&self, record: &log::Record<'_>) {
        let msg = format!("[{}] {}", record.level(), record.args());
        match record.level() {
            log::Level::Error => console::error_1(&msg.into()),
            log::Level::Warn => console::warn_1(&msg.into()),
            _ => console::log_1(&msg.into()),
        }
    }

    fn flush(&self) {}
}

static LOGGER: ConsoleLogger = ConsoleLogger;

/// wasm 模块加载时自动调用。
#[wasm_bindgen(start)]
pub fn start() {
    console_error_panic_hook::set_once();
    // 重复设置（热重载、多次 init）会返回 Err，忽略即可。
    let _ = log::set_logger(&LOGGER).map(|()| log::set_max_level(log::LevelFilter::Info));
    log::info!("dhampir-wasm {} 已加载", env!("CARGO_PKG_VERSION"));
}

/// 报错给页面的**唯一**写法。
///
/// `pub(crate)` 而不是私有：corpus 侧（[`crate::corpus`]）也要用它，
/// 但"怎么把错误交给 JS"这件事不该因此多出第二个定义——
/// 一边抛 `Err(JsValue::from_str(...))`、另一边抛别的形状，
/// 页面就得写两套 catch。
pub(crate) fn js_err(message: impl Into<String>) -> JsValue {
    JsValue::from_str(&message.into())
}

// ---------------------------------------------------------------------------
// 唯一的分叉
// ---------------------------------------------------------------------------

/// 创建浏览器侧的 `wgpu::Instance`。
///
/// **这是本 crate 里唯一与 native 不同的代码行。** 对比
/// `crates/dhampir-worker/src/offscreen.rs` 的同名逻辑：那边用的是
/// `dhampir_core::gpu::NATIVE_BACKENDS`。
///
/// `pub(crate)` 不是为了让"唯一的分叉"变成两句：corpus 侧的离屏设备也**必须**
/// 走这一个函数（[`crate::corpus`] 自己再写一遍 `Instance::new` 就等于有了第二个
/// 分叉点，guard 也拦不住它）。可见性放开的是调用，不是定义。
pub(crate) fn new_instance() -> wgpu::Instance {
    wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: gpu::BROWSER_BACKENDS,
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    })
}

// ---------------------------------------------------------------------------
// 纯逻辑探针：不需要 GPU，所以先保证它可用
// ---------------------------------------------------------------------------

/// 探针报告全文（纯 ASCII 多行文本）。
#[wasm_bindgen]
pub fn dhampir_probe_report() -> String {
    ProbeSummary::capture().report
}

/// 探针摘要，16 个十六进制字符。
///
/// **传字符串而不是数字**：`u64` 超过 JS `Number` 的 53 位尾数，
/// 传数字会得到"看起来相等其实不等"的假绿灯。
#[wasm_bindgen]
pub fn dhampir_probe_digest_hex() -> String {
    ProbeSummary::capture().digest_hex
}

/// 报告格式版本。摘要对不上时先看它。
#[wasm_bindgen]
pub fn dhampir_probe_format_version() -> u32 {
    ProbeSummary::capture().format_version
}

/// 报告行数。用于页面上的快速健全性检查。
#[wasm_bindgen]
pub fn dhampir_probe_line_count() -> usize {
    ProbeSummary::capture().lines
}

/// FNV-1a 64 摘要，十六进制。
///
/// 暴露出来是为了让页面能对**任意字节串**（尤其是 PNG 字节）算摘要，
/// 从而与 native 侧记录的摘要比对。用同一个实现算，才有可比性——
/// 页面如果自己用 JS 写一遍，那就又多了一个变量。
#[wasm_bindgen]
pub fn dhampir_fnv1a64_hex(bytes: &[u8]) -> String {
    format!("{:016x}", dhampir_core::timeline::fnv1a64(bytes))
}

/// 与**编译进本模块的** golden 报告逐字节比对。
///
/// 返回空字符串表示一致，否则是带上"第几行、两侧原文、两侧摘要"的说明。
/// M0.7 的浏览器截图要拍的正是它——它不需要服务端先跑一遍，
/// 所以截图里那个 ✓ 是一个真结论，不是脚本打印的一句好话。
#[wasm_bindgen]
pub fn dhampir_probe_golden_check() -> String {
    match ProbeSummary::capture().verify_golden() {
        Ok(()) => String::new(),
        Err(e) => e,
    }
}

/// golden 报告的摘要，16 个十六进制字符。
///
/// 页面拿它与 native 侧 `run.json` 里记的 `golden_digest` 比一比，就能发现
/// "本地这份 wasm 是用旧 golden 编出来的"。
#[wasm_bindgen]
pub fn dhampir_probe_golden_digest_hex() -> String {
    crate::probe::golden_digest_hex()
}

// ---------------------------------------------------------------------------
// canvas 渲染
// ---------------------------------------------------------------------------

thread_local! {
    /// 浏览器的 GPU 设备创建一次就够了——每帧建一个 device 会慢到没法用。
    ///
    /// 用 `thread_local` 而不是 `static`：wasm 目前是单线程的，但 wgpu 的类型
    /// 都不是 `Sync`，`static` 根本过不了编译。这个写法同时也把
    /// "只有一个 canvas host"这件事表达清楚。
    static CANVAS_HOST: RefCell<Option<CanvasHost>> = const { RefCell::new(None) };
}

/// canvas 渲染所需的一切。
///
/// 字段顺序不是随意的：`Instance` 必须比 `Surface` 活得久（surface 由 instance
/// 创建），Rust 的 drop 顺序正好是声明顺序的逆序，所以 instance 放最前。
///
/// **这也是 [`dhampir_core::io::FrameSink`] 的第一个真实实现形态**：
/// `acquire` 对应 `surface.get_current_texture()`，`finish` 对应 `queue.present(frame)`。
/// M3 会把它正式收进 trait——M0 先不抽象，因为只有一个实现，
/// 抽象错了改起来比现在直接写还贵。
struct CanvasHost {
    _instance: wgpu::Instance,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    ctx: GpuContext,
    renderer: ProbeRenderer,
}

fn get_canvas(canvas_id: &str) -> Result<HtmlCanvasElement, JsValue> {
    let window = web_sys::window().ok_or_else(|| js_err("没有 window 对象"))?;
    let document = window
        .document()
        .ok_or_else(|| js_err("没有 document 对象"))?;
    let element = document
        .get_element_by_id(canvas_id)
        .ok_or_else(|| js_err(format!("页面上找不到 id=\"{canvas_id}\" 的元素")))?;
    element
        .dyn_into::<HtmlCanvasElement>()
        .map_err(|_| js_err(format!("id=\"{canvas_id}\" 的元素不是 canvas")))
}

/// 建（或复用）canvas host，并返回 adapter 信息 JSON。
#[wasm_bindgen]
pub async fn dhampir_probe_init_canvas(canvas_id: String) -> Result<String, JsValue> {
    if let Some(json) = CANVAS_HOST.with(|h| h.borrow().as_ref().map(|host| host.describe())) {
        return Ok(json);
    }

    let canvas = get_canvas(&canvas_id)?;
    // canvas 的 CSS 尺寸与绘制缓冲尺寸是两回事，这里用绘制缓冲尺寸。
    let width = canvas.width().max(1);
    let height = canvas.height().max(1);

    let instance = new_instance();
    // 传 owned canvas 才能拿到 `Surface<'static>`——surface 需要活得和 host 一样久。
    //
    // 显式写 `SurfaceTarget::Canvas`，不走 `Into`：wgpu 30 的 blanket impl 要求目标
    // 实现 `HasWindowHandle + HasDisplayHandle`，而 `HtmlCanvasElement` 的这两个
    // impl 由 raw-window-handle 自己的 `wasm-bindgen-0-2` 特性提供，wgpu 的 `web`
    // 特性并不会替下游打开它。canvas 就是 canvas，不该伪装成某个平台的原生窗口。
    let surface = instance
        .create_surface(wgpu::SurfaceTarget::Canvas(canvas))
        .map_err(|e| js_err(format!("无法从 canvas 创建 surface：{e:?}")))?;

    let ctx = gpu::request_context(&instance, Some(&surface))
        .await
        .map_err(|e| js_err(e.to_string()))?;

    let caps = surface.get_capabilities(&ctx.adapter);
    // 从 wgpu 给的默认配置起步，只覆盖两处。这样将来 wgpu 加了新的必填字段
    // （比如 30 的 `color_space`），这里不会因为"少写一个字段"而编译不过。
    let mut config = surface
        .get_default_config(&ctx.adapter, width, height)
        .ok_or_else(|| js_err("该 adapter 不支持这个 canvas surface"))?;

    // 优先 sRGB：两端要"看起来一致"，色彩空间的差异必须先在预览侧就消掉。
    // `Auto` 色彩空间下 formats 里都是标准 SDR 格式，sRGB 一定在候选里。
    if let Some(srgb) = caps
        .formats
        .iter()
        .copied()
        .find(wgpu::TextureFormat::is_srgb)
    {
        config.format = srgb;
    }
    // Fifo（垂直同步）：预览是给人看的，不是测性能的。撕裂画面会干扰"看起来对不对"的判断。
    config.present_mode = wgpu::PresentMode::Fifo;

    surface.configure(&ctx.device, &config);
    let renderer = ProbeRenderer::new(&ctx.device, config.format);

    let host = CanvasHost {
        _instance: instance,
        surface,
        config,
        ctx,
        renderer,
    };
    let json = host.describe();
    CANVAS_HOST.with(|h| *h.borrow_mut() = Some(host));
    Ok(json)
}

impl CanvasHost {
    /// 一行 JSON，页面直接显示。
    fn describe(&self) -> String {
        let info = &self.ctx.adapter_info;
        serde_json::json!({
            "name": info.name,
            "backend": format!("{:?}", info.backend),
            "device_type": format!("{:?}", info.device_type),
            "driver": info.driver,
            "driver_info": info.driver_info,
            "surface_format": format!("{:?}", self.config.format),
            "surface_size": format!("{}x{}", self.config.width, self.config.height),
        })
        .to_string()
    }
}

/// 把探针三角形画到 canvas 上。
///
/// 返回一帧的耗时（毫秒，`performance.now()` 的差值）。M0 只用它做"确实画了"
/// 的证据，性能基线是 M1 的事。
#[wasm_bindgen]
pub async fn dhampir_probe_render_canvas(canvas_id: String) -> Result<f64, JsValue> {
    dhampir_probe_init_canvas(canvas_id).await?;

    // 把 host 取出来再放回去：`Surface::get_current_texture` 需要 &self，
    // 而 frame 的 present 时机由我们控制，不能跨 await 持有 RefCell 借用。
    let host = CANVAS_HOST
        .with(|h| h.borrow_mut().take())
        .ok_or_else(|| js_err("canvas host 尚未初始化"))?;

    let start = performance_now();

    // wgpu 30 起 `get_current_texture` 不再返回 `Result`，而是把各种失败
    // （超时、遮挡、配置过期、表面丢失）都列成一个枚举——因为它们的处置方式
    // 各不相同，用一个 `Err` 全糊在一起反而会诱导调用方一律重试。
    let frame = match host.surface.get_current_texture() {
        wgpu::CurrentSurfaceTexture::Success(frame) => frame,
        // Suboptimal 也是能用的帧，只是配置不再最优。M0 不打日志噪音，直接画。
        wgpu::CurrentSurfaceTexture::Suboptimal(frame) => frame,
        other => return Err(js_err(format!("取 canvas 纹理失败：{other:?}"))),
    };
    let view = frame
        .texture
        .create_view(&wgpu::TextureViewDescriptor::default());

    let mut encoder = host
        .ctx
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("dhampir canvas probe"),
        });
    host.renderer
        .render(&mut encoder, &view, wgpu::LoadOp::Clear(PROBE_CLEAR_COLOR));
    host.ctx.queue.submit([encoder.finish()]);
    // 提交与呈现是两步（wgpu 30 起）：`submit` 排队，`present` 才把它交给合成器。
    host.ctx.queue.present(frame);

    let elapsed = performance_now() - start;
    CANVAS_HOST.with(|h| *h.borrow_mut() = Some(host));
    Ok(elapsed)
}

/// 离屏渲染探针图，读回，编码成 PNG，返回字节。
///
/// **这是 M2 的种子。** 它与 `dhampir-worker` 的 `offscreen::render_once` 走的是
/// 同一份 `dhampir_core::render` 与 `dhampir_core::readback`，区别只有
/// `Instance` 的后端。也就是说：M2 要验的那条路，M0 就已经通到底了。
#[wasm_bindgen]
pub async fn dhampir_probe_offscreen_png() -> Result<Vec<u8>, JsValue> {
    let instance = new_instance();
    let ctx = gpu::request_context(&instance, None)
        .await
        .map_err(|e| js_err(e.to_string()))?;

    let (width, height) = PROBE_TARGET_SIZE;
    let texture = ctx.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("dhampir probe offscreen"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

    let renderer = ProbeRenderer::new(&ctx.device, wgpu::TextureFormat::Rgba8UnormSrgb);
    let mut encoder = ctx
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("dhampir offscreen probe"),
        });
    renderer.render(&mut encoder, &view, wgpu::LoadOp::Clear(PROBE_CLEAR_COLOR));
    ctx.queue.submit([encoder.finish()]);

    let image = readback::read_texture_rgba8(&ctx.device, &ctx.queue, &texture)
        .await
        .map_err(|e| js_err(e.to_string()))?;
    image.encode_png().map_err(|e| js_err(e.to_string()))
}

/// 取一帧的离屏渲染 + 读回 + PNG 编码的总耗时（毫秒）。
/// M1 的计时表要用。
#[wasm_bindgen]
pub async fn dhampir_probe_offscreen_timing() -> Result<String, JsValue> {
    let start = performance_now();
    let png = dhampir_probe_offscreen_png().await?;
    let total = performance_now() - start;
    Ok(serde_json::json!({
        "png_bytes": png.len(),
        "total_ms": total,
        "target": format!("{}x{}", PROBE_TARGET_SIZE.0, PROBE_TARGET_SIZE.1),
    })
    .to_string())
}

/// `performance.now()`，取不到时退化成 0（宁可让计时难看，也不要让渲染失败）。
fn performance_now() -> f64 {
    web_sys::window()
        .and_then(|w| w.performance())
        .map_or(0.0, |p| p.now())
}
