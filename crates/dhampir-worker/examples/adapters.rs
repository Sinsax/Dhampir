//! 探针：这台机器上 wgpu 能看见哪些 adapter、**有没有软件实现**。
//!
//! 跑：
//!   cargo run -q -p dhampir-worker --example adapters
//!
//! # 为什么要有它
//!
//! 本仓的渲染路径有一条**明确的策略**（`dhampir_core::gpu::request_context`）：
//!
//! ```text
//! force_fallback_adapter: false,   // 不要 software fallback（lavapipe / WARP）
//! ```
//!
//! 那条策略的理由写在源码里：「渲染结果会和真实驱动不一致，而 M2 的比对基准必须是真硬件」。
//! 这个探针把那条策略的**代价与收益**都摆出来 —— 换机器、上 CI、进容器的时候，
//! "这里能不能跑"这个问题应该有一条命令能回答，而不是靠猜。
//!
//! # 实测（Windows 11 + RTX 4070 + AMD 核显，wgpu 30.0.1）
//!
//! ```text
//! enumerate_adapters -> 5 个
//!   NVIDIA GeForce RTX 4070            Vulkan  DiscreteGpu
//!   AMD Radeon(TM) Graphics            Vulkan  IntegratedGpu
//!   NVIDIA GeForce RTX 4070            Dx12    DiscreteGpu
//!   AMD Radeon(TM) Graphics            Dx12    IntegratedGpu
//!   Microsoft Basic Render Driver      Dx12    Cpu        <- 软件实现
//!
//! force_fallback_adapter: true  -> Dx12 Microsoft Basic Render Driver (Cpu)
//! force_fallback_adapter: false -> Vulkan NVIDIA GeForce RTX 4070 (DiscreteGpu)
//! ```
//!
//! 于是三件事是**量出来的**、不是推断的：
//!
//! 1. **没有 GPU 的机器也能跑**（把那一行翻成 `true` 即可）：同一帧 + 同一段 30 帧，
//!    软件实现与真硬件**逐像素完全相同**（平均差 0.0000，最大 0）。
//!    也就是说"结果会不一致"这条理由，在**这个工程**上没有被复现 ——
//!    但这个工程没有模糊层、抖动也只有 0.4 像素，**精度敏感的那几条路并没有被压到**。
//!    要动那条策略，得先在带 `gaussian_blur` / 大位移 warp 的工程上重跑一遍。
//! 2. **纯 CPU 慢约 13 倍**：0.274 秒/帧（3.6 帧/秒）vs GPU 0.0206 秒/帧。
//! 3. **纯 CPU 上"分块并行"几乎线性地救回来** —— 与 GPU 上**恰好相反**。
//!
//! # 分块并行在两边的行为是相反的（300 帧，1920x1080 60fps）
//!
//! ```text
//! GPU（Vulkan / RTX 4070）          纯 CPU（WARP / DX12，16 逻辑核）
//!   workers=1   0.0206 秒/帧            workers=1   0.274 秒/帧   (26.6 分钟/全片)
//!   auto(4)     0.0185 秒/帧  最好       workers=4   0.080 秒/帧   ( 7.8 分钟)
//!   workers=8   0.0231 秒/帧  变慢       workers=8   0.054 秒/帧  最好 ( 5.2 分钟)
//!                                        workers=16  0.060 秒/帧   ( 5.8 分钟)
//! ```
//!
//! 为什么相反：**WARP 几乎不占线程** —— 纯 CPU 渲染期间整机 CPU 只有 13%~23%
//! （16 逻辑核，即约 2~4 个核）。剩下的核是**空的**，多开几个 WARP 实例正好填上。
//! 而 GPU 只有**一块**，每个 worker 各开一套 wgpu 上下文、各自同步读回，
//! 开到 5 个以上就开始互相抢设备。
//!
//! 所以 `render` 的自动档按 **4** 封顶（那是给 GPU 的）；
//! **纯 CPU 出片要自己把 `--chunk-workers` 提到 8** —— 26.6 分钟变成 5.2 分钟。
//! 两者相差 **5.1 倍**，而 `resolve_workers` 并不知道当前选中的是哪个 adapter。
//! （把"按 adapter 的 device_type 决定默认 worker 数"接上是个明确的小改进。）

fn main() {
    // 本仓 native 侧实际请求的三个后端（`dhampir_core::gpu::NATIVE_BACKENDS`）。
    let backends = dhampir_core::gpu::NATIVE_BACKENDS;
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends,
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    });

    println!("请求的后端位标志：{backends:?}");
    println!();

    // wgpu 30 起 `enumerate_adapters` 是异步的。
    let adapters = pollster::block_on(instance.enumerate_adapters(backends));
    println!("enumerate_adapters 看到 {} 个 adapter：", adapters.len());
    for adapter in &adapters {
        let info = adapter.get_info();
        // lavapipe / llvmpipe / WARP / SwiftShader 都算"软件实现"，
        // wgpu 把它们的 device_type 标成 `Cpu`。
        let fallback = if matches!(info.device_type, wgpu::DeviceType::Cpu) {
            "   <- 软件实现（CPU）"
        } else {
            ""
        };
        println!(
            "  {:<34} backend={:<8} type={:<13} driver={}{}",
            info.name,
            format!("{:?}", info.backend),
            format!("{:?}", info.device_type),
            info.driver,
            fallback
        );
    }

    // 本仓**没有**这条路径（策略是关掉的）。这里单独问一次：
    // 如果它给得出 adapter，那"没有 GPU 也能跑"就只差一行。
    let fallback = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::LowPower,
        compatible_surface: None,
        force_fallback_adapter: true,
        apply_limit_buckets: false,
    }));
    println!();
    match fallback {
        Ok(adapter) => {
            let info = adapter.get_info();
            println!(
                "force_fallback_adapter: true  -> {:?} {}（type={:?}）",
                info.backend, info.name, info.device_type
            );
        }
        Err(error) => println!("force_fallback_adapter: true  -> 没有：{error:?}"),
    }

    // 本仓实际走的那条（`force_fallback_adapter: false`）。
    let normal = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
        apply_limit_buckets: false,
    }));
    println!();
    match normal {
        Ok(adapter) => {
            let info = adapter.get_info();
            println!(
                "本仓实际请求（false）        -> {:?} {}（type={:?}）",
                info.backend, info.name, info.device_type
            );
        }
        Err(error) => println!("本仓实际请求 -> 失败：{error:?}"),
    }
}
