//! 线性渐变的覆盖度栅格化 —— **遮罩通路**用的（设计见 D12 的渐变一节）。
//!
//! # 几何口径照 CSS
//!
//! - `angle_deg` 里 **0° 朝上、90° 朝右**（屏幕 y 轴向下）；
//! - 渐变轴过图层矩形中心，轴长 = `|w·sinθ| + |h·cosθ|`（CSS 对盒子的那条规矩）；
//! - 每个像素取**中心**到中心向量在轴上的投影，归一化到 `[0,1]`；
//! - 超出首尾断点就取端点值（CSS 也是这么截的）。

/// 采样断点表（与缓动那边的折线采样同一套口径：位置相等取后一个）。
fn sample_stops(stops: &[(f32, f32)], t: f32) -> f32 {
    if stops.is_empty() {
        return t;
    }
    if t <= stops[0].0 {
        return stops[0].1;
    }
    for window in stops.windows(2) {
        let (p0, v0) = window[0];
        let (p1, v1) = window[1];
        if t <= p1 {
            if p1 <= p0 {
                return v1;
            }
            return v0 + (v1 - v0) * (t - p0) / (p1 - p0);
        }
    }
    stops[stops.len() - 1].1
}

/// 逐像素覆盖度（0..=255），长度 = `width × height`。
pub fn rasterize_linear_gradient(
    angle_deg: f32,
    stops: &[(f32, f32)],
    width: u32,
    height: u32,
) -> Vec<u8> {
    let width = width.max(1);
    let height = height.max(1);
    let (w, h) = (width as f32, height as f32);
    let theta = angle_deg.to_radians();
    let (sin, cos) = (theta.sin(), theta.cos());
    // 屏幕 y 向下 ⇒ "朝上"对应方向向量 (sinθ, −cosθ)。
    let length = (w * sin.abs() + h * cos.abs()).max(1e-6);
    let (cx, cy) = (w / 2.0, h / 2.0);
    let mut out = Vec::with_capacity((width as usize) * (height as usize));
    for y in 0..height {
        for x in 0..width {
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            let projection = (px - cx) * sin + (py - cy) * (-cos);
            let t = 0.5 + projection / length;
            let value = sample_stops(stops, t).clamp(0.0, 1.0);
            out.push((value * 255.0).round() as u8);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(pixels: &[u8], width: u32, x: u32, y: u32) -> u8 {
        pixels[(y * width + x) as usize]
    }

    #[test]
    fn 零度朝上_九十度朝右() {
        // 0° = 朝上：屏幕下方暗、上方亮（y 向下）。
        let up = rasterize_linear_gradient(0.0, &[(0.0, 0.0), (1.0, 1.0)], 8, 16);
        assert!(at(&up, 8, 4, 15) <= 12, "0° 时最下边应当接近 0，得到 {}", at(&up, 8, 4, 15));
        assert!(at(&up, 8, 4, 0) >= 243, "0° 时最上边应当接近 255，得到 {}", at(&up, 8, 4, 0));
        // 90° = 朝右：左侧暗、右侧亮。
        let right = rasterize_linear_gradient(90.0, &[(0.0, 0.0), (1.0, 1.0)], 16, 8);
        assert!(at(&right, 16, 0, 4) <= 12, "90° 时最左边应当接近 0，得到 {}", at(&right, 16, 0, 4));
        assert!(at(&right, 16, 15, 4) >= 243, "90° 时最右边应当接近 255，得到 {}", at(&right, 16, 15, 4));
        // 同一列上不同 y 必须一样（90° 的轴是水平的）。
        for y in 0..8 {
            assert_eq!(at(&right, 16, 7, y), at(&right, 16, 7, 0), "90° 的同一列应当相同");
        }
    }

    #[test]
    fn 断点插值手算对得上() {
        // 4 像素宽、90°：x=1 的中心 1.5，轴长 4、中心 2 ⇒ t = 0.5 + (1.5−2)/4 = 0.375。
        // 断点 (0,0) (0.5,1) ⇒ 值 = 0.375/0.5 = 0.75 ⇒ 191。
        let pixels = rasterize_linear_gradient(90.0, &[(0.0, 0.0), (0.5, 1.0)], 4, 2);
        assert!(at(&pixels, 4, 1, 0).abs_diff(191) <= 2, "得到 {}", at(&pixels, 4, 1, 0));
        // 超出末尾断点 ⇒ 取端点值（x=3 的 t = 0.875 > 0.5 ⇒ 1.0 ⇒ 255）。
        assert!(at(&pixels, 4, 3, 0) >= 253, "超出末尾应当取端点：{}", at(&pixels, 4, 3, 0));
    }
}
