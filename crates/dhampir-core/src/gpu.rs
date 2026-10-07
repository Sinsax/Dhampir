//! 与后端无关的 GPU 上下文创建。**这是"双血统"的接缝。**
//!
//! `wgpu::Instance` 由宿主按各自的后端创建（唯一允许分叉的地方），之后的
//! adapter 选择、device 申请在两个宿主里走**同一份代码**——就是本模块。
//!
//! # 为什么 device 的 limits 要显式写死
//!
//! [`request_context`] 用 [`wgpu::Limits::default()`]，也就是 **WebGPU 规范默认值**，
//! 不是 `downlevel_defaults()` 也不是 adapter 的上限。两个宿主用同一组 limits，
//! 意味着"服务端能跑、浏览器跑不了"这类问题会在开发期暴露，而不是等用户打开页面。
//! 指导文档 §4.3① 的"始终按 WebGPU 的能力下限写"，落点就在这里。

use crate::wgpu;

// ---------------------------------------------------------------------------
// 版本号：**一份实现，两个宿主共用**
// ---------------------------------------------------------------------------

/// 编进本 crate 的 wgpu 版本。来源是 `Cargo.lock`，由本 crate 的 `build.rs` 取出。
///
/// 住在 core 而不是各宿主里，是因为它要回答的是"**两个宿主是不是同一个 wgpu 编出来的**"
/// ——这个问题一旦有两个来源，漂移的那天正好是最不该有漂移的那天。
/// 读不到锁文件时会编进 `"unknown"`：那是一个**可见的缺陷**（[`the_versions_are_real`]
/// 钉着它），比一个看起来很象真的假版本号好得多。
pub const WGPU_VERSION: &str = env!("DHAMPIR_WGPU_VERSION");

/// 编进本 crate 的 naga 版本（`wgpu` 的 WGSL 前端）。
///
/// **注意它说的是"编进来的那个 naga"，不是"编译这段 WGSL 的那个编译器"。**
/// native 宿主上两者是同一个；浏览器宿主上 WGSL 由浏览器自己的实现编译（Chrome 是
/// Dawn/Tint），naga **不在那条路径上**。记录里对这一点必须如实写明——
/// 把 native 的 naga 版本抄到浏览器那条腿上，就等于说了一句当时不成立的话。
pub const NAGA_VERSION: &str = env!("DHAMPIR_NAGA_VERSION");

#[cfg(test)]
mod tests {
    use super::*;

    /// 版本号不许是 `unknown`。
    ///
    /// 这条测的是 **`build.rs` 有没有真的跑通**：`env!` 在编译期展开，所以"读到锁文件"
    /// 这件事失败时会静默变成 `"unknown"`——一个能编译、能跑、记录里也看不出来的结果。
    /// 一条断言把它变成编译后立刻可见的缺陷。
    #[test]
    fn the_versions_are_real() {
        for (name, version) in [("wgpu", WGPU_VERSION), ("naga", NAGA_VERSION)] {
            assert_ne!(
                version, "unknown",
                "{name} 的版本号是 unknown——build.rs 没读到 Cargo.lock"
            );
            // 形如 `30.0.1`：至少三段、每段以数字开头、每段都非空。
            let parts: Vec<&str> = version.split('.').collect();
            assert!(
                parts.len() >= 3
                    && parts
                        .iter()
                        .all(|p| p.chars().next().is_some_and(|c| c.is_ascii_digit())),
                "{name} 的版本号长得不像版本号：{version}"
            );
        }
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
        // 浏览器那条腿要写的是同一个键，值由 wgpu 自己的 flag 名给出。
        // 钉住它：这个字符串会出现在两条腿的记录里，"哪条腿"靠它区分。
        assert_eq!(backend_label(BROWSER_BACKENDS), "BROWSER_WEBGPU");
        for backends in [NATIVE_BACKENDS, BROWSER_BACKENDS] {
            let label = backend_label(backends);
            assert!(!label.contains("Backends"), "{label}");
            assert!(!label.contains('('), "{label}");
        }
    }

    /// 目录名必须**始终**是合法文件名——包括"多后端位或"与浏览器那条腿。
    ///
    /// 撞名比难看严重得多：两个后端的产物落进同一个目录，先跑的那份就被盖掉了，
    /// 而记录里两行 `files` 会指向同一个文件。
    #[test]
    fn backend_slug_is_a_legal_directory_name() {
        assert_eq!(backend_slug(wgpu::Backends::DX12), "dx12");
        assert_eq!(backend_slug(wgpu::Backends::VULKAN), "vulkan");
        for backends in [
            wgpu::Backends::DX12,
            wgpu::Backends::VULKAN,
            // 位或起来的两个后端也要能当目录名（`|` 不是合法文件名字符）。
            wgpu::Backends::DX12.union(wgpu::Backends::VULKAN),
            BROWSER_BACKENDS,
        ] {
            let slug = backend_slug(backends);
            assert!(!slug.is_empty(), "空目录名会把两条腿的产物混到一起");
            assert!(
                slug.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
                "{slug} 里有不安全的字符"
            );
        }
        // 两条腿的目录名必须真的不同，否则"一条腿一个目录"这句话是假的。
        assert_ne!(
            backend_slug(wgpu::Backends::DX12),
            backend_slug(BROWSER_BACKENDS)
        );
    }
}

/// native 宿主允许的后端。
///
/// **这个常量与 [`BROWSER_BACKENDS`] 是仓库里仅有的两处后端分叉**，而且它们都只是
/// 常量——真正的 `Instance::new` 调用在宿主里。把它们放在 core 而不是各自宿主里，
/// 是为了让"允许哪两种写法"在一处可查、可 review。
///
/// - `VULKAN`：Linux 容器部署形态（NVIDIA Container Toolkit 注入 ICD），
///   Windows 上也是 NVIDIA 驱动的稳定路径
/// - `DX12`：Windows 原生，AMD / Intel 核显上通常比 Vulkan 更省事
/// - `METAL`：macOS。在非 Apple 平台上是空操作，留着以免将来加平台时忘掉
///
/// 后端 feature 由宿主在 Cargo.toml 里追加（core 自己不开任何后端，
/// 否则会把 wasm32 编译打死）。
pub const NATIVE_BACKENDS: wgpu::Backends = wgpu::Backends::VULKAN
    .union(wgpu::Backends::DX12)
    .union(wgpu::Backends::METAL);

/// 浏览器宿主唯一可用的后端。
///
/// `BROWSER_WEBGPU` 对应页面里的 `navigator.gpu`。**不写 `WEBGL`**：
/// WebGL2 的能力下限（没有 compute、没有 storage texture、没有 `texture_external`）
/// 会让同一份 WGSL 在两个宿主上真的变成两份代码，那正是本架构要避免的事。
pub const BROWSER_BACKENDS: wgpu::Backends = wgpu::Backends::BROWSER_WEBGPU;

// ---------------------------------------------------------------------------
// 后端名 → 记录里的字符串
// ---------------------------------------------------------------------------

/// 请求的后端位标志 → **记录里那个后端名**（`DX12`、`VULKAN`、`BROWSER_WEBGPU`）。
///
/// 不直接 `{:?}` 出来：`wgpu::Backends` 是位标志包装，它的 `Debug` 是
/// `Backends(DX12)`——那是 wgpu 的内部形态，不是后端名。M0 归档的
/// `records/m0/*.json` 里就是那个形态（当时直接 `{:?}` 了），**不追溯改写**：
/// 那些文件是那一次运行的证据。M1 起统一走本函数，于是"记录里写的后端名"和
/// "产物目录名"（[`backend_slug`]）说的是同一件事，而不是两套拼法。
///
/// # 为什么住在 core
///
/// 它原先在 `dhampir-worker` 里（那时只有 native 一条腿要写记录）。M2 的浏览器腿
/// 要写**同一个键**（值是 `BROWSER_WEBGPU`），而两个宿主互不依赖——在 wasm 侧再写
/// 一遍这段剥壳逻辑，就等于让"记录里写的后端名"有两个来源，而漂移的那天正好是
/// 两条腿的记录该对上的那天。搬进来之后两个宿主调的是同一个函数；
/// worker 侧留了一层转发，它的调用点与归档记录都不用动。
pub fn backend_label(backends: wgpu::Backends) -> String {
    let raw = format!("{backends:?}");
    match raw
        .strip_prefix("Backends(")
        .and_then(|inner| inner.strip_suffix(')'))
    {
        Some(inner) => inner.to_string(),
        // 万一日后 wgpu 换了写法：原样返回也不致命——**名字难看总好过名字为空**，
        // 而"空名字"会让两个后端的产物落进同一个目录。
        None => raw,
    }
}

/// 后端名 → 目录/文件名里的小写形式（`DX12` → `dx12`、`BROWSER_WEBGPU` → `browser-webgpu`）。
///
/// 每个后端的产物必须落在**自己的目录**里：M0 已经踩过"后跑的盖了先跑的"这个坑
/// （见 worker 里 `offscreen::ProbeRun::stem` 的注释），一次 `--backend all` 会让
/// 两个后端写出同一批 `frames/*.png`。
///
/// 只留下 `[a-z0-9-]`：位或起来的多后端（`DX12 | VULKAN`）也要能当目录名，
/// 而 `|` 在 Windows 上不是合法文件名字符。
pub fn backend_slug(backends: wgpu::Backends) -> String {
    backend_label(backends)
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

/// 一个可用的 GPU 上下文。两个宿主拿到的都是这个类型。
pub struct GpuContext {
    /// 选中的适配器。记录 `adapter.get_info()` 是 M1 环境探针的一部分。
    pub adapter: wgpu::Adapter,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    /// `adapter.get_info()` 的缓存。写 `adapter.json` 用，避免重复查询。
    pub adapter_info: wgpu::AdapterInfo,
}

impl GpuContext {
    /// 申请 device 时用的 limits。两个宿主一致。
    ///
    /// 用 `defaults()` 而不是 `default()`：前者是 `const fn`，能把"这组数字是
    /// 编译期常量"这件事写进类型系统，顺手也断了"某天有人在运行期改成 adapter 上限"的路。
    pub const LIMITS: wgpu::Limits = wgpu::Limits::defaults();
}

/// 上下文创建失败。
///
/// 刻意不用 `thiserror`：core 的错误类型要能在两个宿主里原样 `Display`，
/// 而这里真正需要携带的信息就是底层错误原文。
#[derive(Debug)]
pub enum GpuError {
    /// 没有可用的 adapter。
    ///
    /// 在 native 上最常见的原因是**后端 feature 没开**（Cargo.toml 里没写
    /// `vulkan`/`dx12`），在容器里则是 Vulkan ICD 没注入。看到这个错误先查
    /// Cargo.toml 再查驱动。
    NoAdapter(String),
    /// 有 adapter，但要不到 device（通常是 limits 或 features 要高了）。
    NoDevice(String),
}

impl core::fmt::Display for GpuError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NoAdapter(m) => write!(f, "没有可用的 GPU 适配器：{m}"),
            Self::NoDevice(m) => write!(f, "无法申请 GPU 设备：{m}"),
        }
    }
}

impl core::error::Error for GpuError {}

/// 从宿主创建的 [`wgpu::Instance`] 里取出一套可用的 device / queue。
///
/// 这是两个宿主**共有**的初始化路径：[`crate::gpu::NATIVE_BACKENDS`] 的宿主编译出的
/// `Instance` 和 [`crate::gpu::BROWSER_BACKENDS`] 的宿主编译出的 `Instance`，
/// 进来之后的代码一字不差。
///
/// `compatible_surface`：浏览器侧需要传 canvas surface，否则可能选到一个无法
/// present 到该 surface 的 adapter。离屏（服务端）传 `None`。
pub async fn request_context(
    instance: &wgpu::Instance,
    compatible_surface: Option<&wgpu::Surface<'_>>,
) -> Result<GpuContext, GpuError> {
    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface,
            // 不要 software fallback（lavapipe / WARP）。渲染结果会和真实驱动不一致，
            // 而 M2 的比对基准必须是真硬件。要测 fallback 就显式另开一次请求。
            force_fallback_adapter: false,
            // 限制分桶是给"不想被指纹识别"的浏览器场景用的，会把 limits 压到某个档位。
            // 我们每次都显式写死同一组 limits，不需要它来帮忙——关掉，减少一个变量。
            apply_limit_buckets: false,
        })
        .await
        .map_err(|e| GpuError::NoAdapter(format!("{e:?}")))?;

    let adapter_info = adapter.get_info();

    let (device, queue) = adapter
        .request_device(&wgpu::DeviceDescriptor {
            label: Some("dhampir device"),
            // 一个 feature 都不要：多要一个 feature 就多一条"服务端能跑、浏览器跑不了"的路。
            required_features: wgpu::Features::empty(),
            // WebGPU 默认限制，两端一致。见模块文档。
            required_limits: GpuContext::LIMITS,
            // 不接受实验性 API 的风险（wgpu 30 起是显式的 opt-in token）。
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
            memory_hints: wgpu::MemoryHints::default(),
            trace: wgpu::Trace::Off,
        })
        .await
        .map_err(|e| GpuError::NoDevice(format!("{e:?}")))?;

    log::info!("GPU 上下文就绪：{adapter_info:?}");

    Ok(GpuContext {
        adapter,
        device,
        queue,
        adapter_info,
    })
}

/// 把 adapter 信息整理成稳定顺序的一行文本，写进记录文件用。
///
/// 刻意手写而不是 `#[derive(Serialize)]`：`AdapterInfo` 的 Debug 输出格式不属于
/// wgpu 的公开契约，换个版本就可能变。记录文件是要长期留存、跨版本比较的，
/// 字段**命名**必须由我们自己钉死。
///
/// 这里返回的 `Vec` 顺序是固定的；但要注意**消费方未必保留它**：写进
/// `serde_json::Map` 时（本仓库未开 `preserve_order`）对象键会按字母序输出。
/// 那同样是确定的，只是不是这个顺序——所以别把"顺序"当成本函数的保证。
pub fn describe_adapter(info: &wgpu::AdapterInfo) -> Vec<(&'static str, String)> {
    vec![
        ("name", info.name.clone()),
        ("backend", format!("{:?}", info.backend)),
        ("device_type", format!("{:?}", info.device_type)),
        ("driver", info.driver.clone()),
        ("driver_info", info.driver_info.clone()),
        ("vendor", format!("{}", info.vendor)),
        ("device", format!("{}", info.device)),
    ]
}
