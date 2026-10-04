//! 双端比对：把两个目录里的同帧 PNG 拿来算 SSIM。
//!
//! 跑：cargo run -q -p dhampir-worker --example compare_project -- \
//!       target/s4/worker target/s4/browser 0 15 30 45 75
//!
//! 指标与解码都来自 core —— 比对器本身两端共用一份，不是各写各的。

use std::path::Path;

use dhampir_core::metric::compare_rgba8;
use dhampir_core::readback::Rgba8Image;

fn load(
    dir: &str,
    frame: i64,
) -> Result<dhampir_core::readback::Rgba8Image, Box<dyn std::error::Error>> {
    let path = Path::new(dir).join(format!("frame-{frame:04}.png"));
    let bytes = std::fs::read(&path)?;
    Rgba8Image::decode_png(&bytes).map_err(|e| format!("{} 解码失败：{e}", path.display()).into())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let left = args
        .first()
        .ok_or("用法：compare_project <dir-a> <dir-b> [帧号...]")?;
    let right = args
        .get(1)
        .ok_or("用法：compare_project <dir-a> <dir-b> [帧号...]")?;
    let frames: Vec<i64> = if args.len() > 2 {
        args[2..]
            .iter()
            .filter_map(|text| text.parse().ok())
            .collect()
    } else {
        vec![0, 15, 30, 45, 75]
    };

    let mut worst = f64::INFINITY;
    println!("[");
    for (index, frame) in frames.iter().enumerate() {
        let a = load(left, *frame)?;
        let b = load(right, *frame)?;
        let result = compare_rgba8(&a.pixels, &b.pixels, a.width, a.height).ok_or(format!(
            "第 {frame} 帧尺寸不一致或数据不足：{}x{} vs {}x{}",
            a.width, a.height, b.width, b.height
        ))?;
        worst = worst.min(result.ssim);
        let comma = if index + 1 == frames.len() { "" } else { "," };
        println!(
            "  {{\"frame\": {frame}, \"ssim\": {:.6}, \"psnr_db\": {:.3}, \"mae\": {:.4}}}{comma}",
            result.ssim, result.psnr_db, result.mae
        );
    }
    println!("]");
    eprintln!("最差 SSIM = {worst:.6}");
    // 退出码刻意不因为"没到阈值"而失败：阈值还没标定，先让数字出来。
    Ok(())
}
