//! 拿真实素材跑一遍动图解码：帧数、延迟表、总时长、内存账。
//!
//! 为什么要有这个例子：单测用的是自己造的字节流，只能证明**逻辑对**；
//! 真实素材才会带出"这份文件到底长什么样"的事实（帧数、延迟是不是零、
//! 画布是不是比帧大）。这些数直接决定显存预算够不够。

use std::path::PathBuf;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("用法：cargo run -p dhampir-core --example animation_probe -- <文件>...");
        std::process::exit(2);
    }
    for raw in args {
        let path = PathBuf::from(&raw);
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| raw.clone());
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) => {
                println!("{name}: 读不了 —— {error}");
                continue;
            }
        };
        println!("=== {name} ===");
        println!("  文件字节：{}", bytes.len());
        match dhampir_core::animation::detect_format(&bytes) {
            Some(format) => println!("  magic 认出来：{}", format.as_str()),
            None => {
                println!("  magic 认不出来 —— 不是 GIF / 动画 WebP");
                continue;
            }
        }
        match dhampir_core::animation::decode(&bytes) {
            Ok(animation) => {
                let delays = animation.delays_ms();
                let zeros = delays.iter().filter(|delay| **delay == 0).count();
                let min = delays.iter().copied().min().unwrap_or(0);
                let max = delays.iter().copied().max().unwrap_or(0);
                println!(
                    "  画布：{}x{}  帧数：{}  循环：{}",
                    animation.width,
                    animation.height,
                    animation.frame_count(),
                    if animation.loop_count == 0 {
                        "无限".to_string()
                    } else {
                        animation.loop_count.to_string()
                    }
                );
                println!(
                    "  总时长：{} ms（{:.2} s）",
                    animation.total_ms,
                    animation.total_ms as f64 / 1000.0
                );
                println!(
                    "  延迟：min={min}ms max={max}ms 零延迟帧={zeros}/{}  前 12 个={:?}",
                    delays.len(),
                    &delays[..delays.len().min(12)]
                );
                let decoded = animation.memory_bytes();
                let gpu = (animation.width as u64) * (animation.height as u64) * 4
                    * animation.frame_count() as u64;
                println!(
                    "  内存：解出来 {:.1} MiB  传上显存 {:.1} MiB",
                    decoded as f64 / 1048576.0,
                    gpu as f64 / 1048576.0
                );
                // 逐帧平均延迟与总时长对不对得上
                let sum: u64 = delays.iter().map(|delay| u64::from(*delay)).sum();
                println!(
                    "  校验：延迟之和 {sum} ms vs total_ms {}  => {}",
                    animation.total_ms,
                    if sum == animation.total_ms { "一致" } else { "不一致（要查）" }
                );
            }
            Err(error) => println!("  解码失败：{error}"),
        }
    }
}
