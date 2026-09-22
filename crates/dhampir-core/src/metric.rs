//! 图像相似度指标：SSIM 与 PSNR。
//!
//! # 为什么这一层要自己写、而且要能单测
//!
//! 「双端一致性」这条验收最终要落成一个数。如果这个数是调某个库、或者照某篇文章抄一遍，
//! 那它就只能"看起来对"——而阈值标定全靠它，指标本身错了会把整个验收带偏。
//! 所以这里用纯算术实现，并用**能手工推出的例子**钉住它（相同图 = 1、黑白 ≈ 0）。
//!
//! # 口径（写死，改口径等于改验收）
//!
//! - 在 **luma（Y）** 上算，不是逐通道平均：视频比对看的是亮度结构，
//!   逐通道平均会让色度上的差异被稀释；
//! - 窗口 **8×8、步长 8（不重叠）**：确定性、可复现，不引入重叠加权的实现差异；
//! - C1 = (K1·L)²、C2 = (K2·L)²，K1 = 0.01、K2 = 0.03、L = 255（SSIM 原论文取值）；
//! - 局部 SSIM 取**平均**（不是取最差），与常见实现一致。
//!
//! luma 用 BT.601 的整数系数（77/150/29 近似 0.299/0.587/0.114）与 >>8：
//! 整数系数保证两端算出**逐位相同**的灰度，不引入浮点差异。

/// 一块窗口的灰度统计。抽出来是为了能被单测直接钉住。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WindowStats {
    pub mean_a: f64,
    pub mean_b: f64,
    pub var_a: f64,
    pub var_b: f64,
    pub cov: f64,
}

impl WindowStats {
    /// 两块同长灰度切片之间的统计量。
    ///
    /// 方差用**总体**方差（除以 n）而不是样本方差（除以 n-1）：窗口大小固定，
    /// 除以 n 才能让「常数窗口的方差恰为 0」成立，也就才能让「两张相同常数图 SSIM = 1」成立。
    pub fn of(a: &[u8], b: &[u8]) -> Option<Self> {
        if a.len() != b.len() || a.is_empty() {
            return None;
        }
        let n = a.len() as f64;
        let mean_a = a.iter().map(|v| f64::from(*v)).sum::<f64>() / n;
        let mean_b = b.iter().map(|v| f64::from(*v)).sum::<f64>() / n;
        let mut var_a = 0.0;
        let mut var_b = 0.0;
        let mut cov = 0.0;
        for (x, y) in a.iter().zip(b.iter()) {
            let da = f64::from(*x) - mean_a;
            let db = f64::from(*y) - mean_b;
            var_a += da * da;
            var_b += db * db;
            cov += da * db;
        }
        Some(Self {
            mean_a,
            mean_b,
            var_a: var_a / n,
            var_b: var_b / n,
            cov: cov / n,
        })
    }

    /// 单窗口 SSIM。公式照原论文写，常数写在这里、不外传。
    pub fn ssim(&self) -> f64 {
        const L: f64 = 255.0;
        let c1 = (0.01 * L) * (0.01 * L);
        let c2 = (0.03 * L) * (0.03 * L);
        let numerator = (2.0 * self.mean_a * self.mean_b + c1) * (2.0 * self.cov + c2);
        let denominator = (self.mean_a * self.mean_a + self.mean_b * self.mean_b + c1)
            * (self.var_a + self.var_b + c2);
        numerator / denominator
    }
}

/// 一次比对的结论。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Comparison {
    /// 通常落在 0..=1；结构完全相反时理论上可为负。
    pub ssim: f64,
    /// 峰值信噪比，单位 dB。两图**完全相同**时为 f64::INFINITY。
    pub psnr_db: f64,
    /// 平均绝对误差，0..=255。给「到底差多少」一个直观量。
    pub mae: f64,
    /// 参与统计的窗口数。
    pub windows: usize,
}

/// 比对两张 RGBA8 图（默认 8×8 窗口）。
///
/// 尺寸不一致或长度不足时返回 None——**不是**给一个默认通过值。
/// 比对器悄悄放过尺寸不匹配，是那种「两个 bug 互相抵消成绿灯」的经典来源。
pub fn compare_rgba8(a: &[u8], b: &[u8], width: u32, height: u32) -> Option<Comparison> {
    compare_rgba8_with(a, b, width, height, 8)
}

/// 指定窗口大小的版本（窗口 = 步长，不重叠）。主要给单测用小块。
pub fn compare_rgba8_with(
    a: &[u8],
    b: &[u8],
    width: u32,
    height: u32,
    window: u32,
) -> Option<Comparison> {
    let need = (width as usize).checked_mul(height as usize)?.checked_mul(4)?;
    if a.len() < need || b.len() < need || width == 0 || height == 0 || window == 0 {
        return None;
    }
    // 政策：至少要有一个**完整**窗口。比窗口还小的图没有被裁短的余地，
    // 拿部分窗口算出来的数会冒充「8x8 SSIM」，口径就变了。
    if window > width || window > height {
        return None;
    }

    let luma = |pixels: &[u8]| -> Vec<u8> {
        let mut out = Vec::with_capacity((width * height) as usize);
        for index in 0..(width as usize * height as usize) {
            let at = index * 4;
            // BT.601 的整数近似：(77*R + 150*G + 29*B + 128) >> 8。
            let y = (77 * u32::from(pixels[at])
                + 150 * u32::from(pixels[at + 1])
                + 29 * u32::from(pixels[at + 2])
                + 128)
                >> 8;
            out.push(y.min(255) as u8);
        }
        out
    };
    let luma_a = luma(a);
    let luma_b = luma(b);

    let mut sum_ssim = 0.0_f64;
    let mut windows = 0_usize;

    for start_y in (0..height).step_by(window as usize) {
        for start_x in (0..width).step_by(window as usize) {
            let end_y = (start_y + window).min(height);
            let end_x = (start_x + window).min(width);
            let mut patch_a = Vec::new();
            let mut patch_b = Vec::new();
            for y in start_y..end_y {
                for x in start_x..end_x {
                    let at = (y * width + x) as usize;
                    patch_a.push(luma_a[at]);
                    patch_b.push(luma_b[at]);
                }
            }
            let stats = WindowStats::of(&patch_a, &patch_b)?;
            sum_ssim += stats.ssim();
            windows += 1;
        }
    }
    if windows == 0 {
        return None;
    }

    // PSNR / MAE 在 **RGB** 上算（比只看亮度更能反映色彩差异）。
    // 与 SSIM 只看亮度是两个不同的口径，故意分开，不混用。
    let mut sum_sq_error = 0.0_f64;
    let mut sum_abs_error = 0.0_f64;
    for index in 0..(width as usize * height as usize) {
        for channel in 0..3 {
            let at = index * 4 + channel;
            let diff = f64::from(a[at]) - f64::from(b[at]);
            sum_sq_error += diff * diff;
            sum_abs_error += diff.abs();
        }
    }
    let samples = f64::from(width * height * 3);
    let mse = sum_sq_error / samples;
    let psnr_db = if mse == 0.0 { f64::INFINITY } else { 10.0 * (255.0 * 255.0 / mse).log10() };

    Some(Comparison {
        ssim: sum_ssim / windows as f64,
        psnr_db,
        mae: sum_abs_error / samples,
        windows,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(width: u32, height: u32, rgba: [u8; 4]) -> Vec<u8> {
        let mut out = Vec::with_capacity((width * height * 4) as usize);
        for _ in 0..(width * height) {
            out.extend_from_slice(&rgba);
        }
        out
    }

    /// 带纹理的图：每个像素亮度递增，「结构」这件事才有意义。
    fn ramp(width: u32, height: u32, base: u8, step: u8) -> Vec<u8> {
        let mut out = Vec::with_capacity((width * height * 4) as usize);
        for y in 0..height {
            for x in 0..width {
                let value = base.wrapping_add(step.wrapping_mul((x + y) as u8));
                out.extend_from_slice(&[value, value, value, 255]);
            }
        }
        out
    }

    #[test]
    fn 相同的图完全一致() {
        let image = ramp(16, 16, 10, 3);
        let result = compare_rgba8(&image, &image, 16, 16).expect("应当能比对");
        assert!((result.ssim - 1.0).abs() < 1e-12, "得到 {}", result.ssim);
        assert!(result.psnr_db.is_infinite(), "全等时 PSNR 应当是无穷");
        assert_eq!(result.mae, 0.0);
        assert_eq!(result.windows, 4, "16x16 / 8x8 = 4 个窗口");
    }

    #[test]
    fn 全黑与全白的_ssim_接近零() {
        // 手工推：常数图方差与协方差都是 0，于是
        // SSIM = C1 / (255^2 + C1) = 6.5025 / 65031.5 ≈ 1e-4。
        let black = solid(8, 8, [0, 0, 0, 255]);
        let white = solid(8, 8, [255, 255, 255, 255]);
        let result = compare_rgba8(&black, &white, 8, 8).expect("应当能比对");
        let expected = 6.5025 / (255.0 * 255.0 + 6.5025);
        assert!((result.ssim - expected).abs() < 1e-9, "得到 {}，期望 {}", result.ssim, expected);
        assert!(result.ssim < 0.001);
        assert!(result.psnr_db.is_finite(), "全黑对全白不该是无穷 PSNR");
        assert!((result.mae - 255.0).abs() < 1e-9);
    }

    #[test]
    fn 相同常数图_ssim_为一() {
        let black = solid(8, 8, [0, 0, 0, 255]);
        let result = compare_rgba8(&black, &black, 8, 8).expect("应当能比对");
        assert!(result.ssim.is_finite());
        assert!((result.ssim - 1.0).abs() < 1e-12);
    }

    #[test]
    fn 噪声让_ssim_下降但仍可辨() {
        let clean = ramp(16, 16, 20, 4);
        let mut noisy = clean.clone();
        // 每 4 个像素挑一个加 8：一处小改动，不该让整张图判为不同
        for index in (0..noisy.len()).step_by(16) {
            noisy[index] = noisy[index].saturating_add(8);
        }
        let result = compare_rgba8(&clean, &noisy, 16, 16).expect("应当能比对");
        assert!(result.ssim < 1.0, "改过了就不该还是 1");
        assert!(result.ssim > 0.9, "只改了一点点，不该掉太多：{}", result.ssim);
        assert!(result.psnr_db.is_finite() && result.psnr_db > 20.0, "得到 {}", result.psnr_db);
    }

    #[test]
    fn ssim_只看亮度不看颜色() {
        // 这条同时钉住两件事：BT.601 的整数系数、以及 SSIM 走 luma 这个口径。
        // 纯红 (255,0,0) 的 luma = (77*255 + 128) >> 8 = 77，所以它与 (77,77,77) 同亮度。
        let red = solid(8, 8, [255, 0, 0, 255]);
        let gray = solid(8, 8, [77, 77, 77, 255]);
        let result = compare_rgba8(&red, &gray, 8, 8).expect("应当能比对");
        assert!((result.ssim - 1.0).abs() < 1e-12, "同亮度应当 SSIM=1，得到 {}", result.ssim);
        assert!(result.psnr_db.is_finite(), "但 RGB 上确实不同，PSNR 必须看出差别");
        assert!(result.mae > 50.0, "MAE 也该看出差别，得到 {}", result.mae);
    }

    #[test]
    fn 尺寸与长度不匹配一律返回_none() {
        let image = solid(8, 8, [1, 2, 3, 255]);
        assert!(compare_rgba8(&image, &image, 4, 8).is_none(), "尺寸不匹配不该给结论");
        assert!(compare_rgba8(&image[..100], &image, 8, 8).is_none(), "长度不足不该给结论");
        assert!(compare_rgba8(&image, &image, 0, 8).is_none());
        assert!(compare_rgba8_with(&image, &image, 8, 8, 16).is_none(), "窗口比图大就不给结论");
    }

    #[test]
    fn 窗口统计量的手算例子() {
        // a = [10,20,30,40]，均值 25，总体方差 125，自己与自己协方差 125
        let a = [10_u8, 20, 30, 40];
        let stats = WindowStats::of(&a, &a).expect("应当能算");
        assert_eq!(stats.mean_a, 25.0);
        assert_eq!(stats.var_a, 125.0);
        assert_eq!(stats.cov, 125.0);
        assert!((stats.ssim() - 1.0).abs() < 1e-12);
    }

    #[test]
    fn 非八的整数倍尺寸也能算完() {
        // 边缘窗口会被裁短。13x17 里有 2x3 = 6 个窗口（右侧与下方那些是裁短的）。
        let a = ramp(13, 17, 5, 2);
        let result = compare_rgba8(&a, &a, 13, 17).expect("非整倍尺寸不该失败");
        assert!((result.ssim - 1.0).abs() < 1e-12);
        assert_eq!(result.windows, 2 * 3, "13x17 按 8 切出 2x3 个窗口");
    }

    #[test]
    fn 比窗口还小的图不给结论() {
        // 政策：至少要有一个**完整**窗口，否则「8x8 SSIM」这句话本身就不成立。
        // 宁可返回 None，也不要拿一个被裁到 13x7 的窗口算出来的数冒充 SSIM——
        // 那种数看起来像指标，实际口径已经变了。
        let small = ramp(13, 7, 5, 2);
        assert!(compare_rgba8(&small, &small, 13, 7).is_none());
    }
}


