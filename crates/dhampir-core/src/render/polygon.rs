//! 多边形 → **覆盖度位图**（多边形裁剪用的那一份栅格化器）。
//!
//! # 为什么在 core 里（而不是各宿主各写一份）
//!
//! 两个宿主（浏览器 / native）要给出**同一张**遮罩，否则"预览所见 != 成片所得"就回来了。
//! 放在 core 里、纯 Rust、零 `#[cfg]` —— 两端跑的是同一份代码、同一组浮点运算。
//!
//! # 口径（写下来，因为它是两端共用的那一份）
//!
//! - 顶点是**图层框内的归一化坐标**（0..1，与 `ClipShape::Circle` 的 `center` 同一套）；
//! - **非零环绕**规则（even-odd 会让自交多边形表现得很意外，而"意外"比"不支持"更难查）；
//! - 采样在**像素中心**（半像素偏移，与光栅化惯例一致）；
//! - 每像素 **2×2 超采样**取平均 —— 硬边的多边形在缩放时会抖，而这点成本在小尺寸上可以忽略。

/// 把归一化多边形栅格化成 `width × height` 的覆盖度（0..=255，行优先）。
///
/// 顶点少于 3 个 → 全 0（也就是"什么都裁掉"）。校验层本来就会为这种输入报错，
/// 这里只保证**不崩、也不给出一个看起来正常的图**。
pub fn rasterize_polygon_coverage(points: &[[f32; 2]], width: u32, height: u32) -> Vec<u8> {
    let mut out = vec![0u8; (width as usize) * (height as usize)];
    if points.len() < 3 || width == 0 || height == 0 {
        return out;
    }
    for y in 0..height {
        for x in 0..width {
            let mut hits = 0u32;
            for sample_y in 0..2u32 {
                for sample_x in 0..2u32 {
                    let px = (x as f32 + (sample_x as f32 + 0.5) * 0.5) / width as f32;
                    let py = (y as f32 + (sample_y as f32 + 0.5) * 0.5) / height as f32;
                    if winding_number(points, px, py) != 0 {
                        hits += 1;
                    }
                }
            }
            out[(y as usize) * (width as usize) + (x as usize)] = (hits * 255 / 4) as u8;
        }
    }
    out
}

/// 非零环绕数。标准算法：一条水平射线向右，数**向上穿过**与**向下穿过**的边。
fn winding_number(points: &[[f32; 2]], px: f32, py: f32) -> i32 {
    let mut winding = 0;
    let count = points.len();
    for index in 0..count {
        let a = points[index];
        let b = points[(index + 1) % count];
        if a[1] <= py {
            if b[1] > py && is_left(a, b, px, py) > 0.0 {
                winding += 1;
            }
        } else if b[1] <= py && is_left(a, b, px, py) < 0.0 {
            winding -= 1;
        }
    }
    winding
}

/// `p` 在 `a → b` 的左侧还是右侧（叉积）。
fn is_left(a: [f32; 2], b: [f32; 2], px: f32, py: f32) -> f32 {
    (b[0] - a[0]) * (py - a[1]) - (px - a[0]) * (b[1] - a[1])
}

/// 把覆盖度写进一张纹理（**alpha = 覆盖度**），给掩码通路用。
///
/// 返回 `(纹理, 视图)`：**两个都要留着** —— 视图不能比纹理活得久。
pub fn coverage_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    size: (u32, u32),
    coverage: &[u8],
    format: wgpu::TextureFormat,
) -> (wgpu::Texture, wgpu::TextureView) {
    let width = size.0.max(1);
    let height = size.1.max(1);
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("dhampir polygon clip coverage"),
        size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let mut pixels = Vec::with_capacity((width as usize) * (height as usize) * 4);
    for y in 0..height {
        for x in 0..width {
            let value = coverage.get((y as usize) * (width as usize) + (x as usize)).copied().unwrap_or(0);
            // rgb 也填成覆盖度（亮度通道也就跟着对了），alpha 是主通道。
            pixels.extend_from_slice(&[value, value, value, value]);
        }
    }
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &pixels,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(width * 4),
            rows_per_image: Some(height),
        },
        wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
    );
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    (texture, view)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(coverage: &[u8], size: u32, x: u32, y: u32) -> u8 {
        coverage[(y as usize) * (size as usize) + (x as usize)]
    }

    #[test]
    fn 整框多边形处处都是满覆盖() {
        let coverage = rasterize_polygon_coverage(&[[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]], 16, 16);
        assert!(coverage.iter().all(|value| *value == 255), "整框应当处处 255");
    }

    #[test]
    fn 三角形只盖住它那一半() {
        let points = [[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]];
        let coverage = rasterize_polygon_coverage(&points, 32, 32);
        assert_eq!(at(&coverage, 32, 4, 4), 255, "左下角那一片在三角形里");
        assert_eq!(at(&coverage, 32, 28, 28), 0, "右上角那一片不在");
        // 斜边两侧：中点附近应当一侧满、一侧空。
        assert_eq!(at(&coverage, 32, 12, 8), 255, "斜边下方应当满");
        assert_eq!(at(&coverage, 32, 24, 8), 0, "斜边上方应当空");
    }

    #[test]
    fn 顶点顺序反着绕结果一样() {
        let forward = [[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]];
        let backward = [[0.0, 1.0], [1.0, 0.0], [0.0, 0.0]];
        let a = rasterize_polygon_coverage(&forward, 16, 16);
        let b = rasterize_polygon_coverage(&backward, 16, 16);
        assert_eq!(a, b, "非零环绕规则下，绕向不该改变结果");
    }

    #[test]
    fn 凹多边形按非零环绕填() {
        // 一个 L 形：右下角那块凹口**不该**被填上。
        let points = [[0.0, 0.0], [1.0, 0.0], [1.0, 0.5], [0.5, 0.5], [0.5, 1.0], [0.0, 1.0]];
        let coverage = rasterize_polygon_coverage(&points, 32, 32);
        assert_eq!(at(&coverage, 32, 4, 4), 255, "左上在 L 里");
        assert_eq!(at(&coverage, 32, 4, 28), 255, "左下在 L 里");
        assert_eq!(at(&coverage, 32, 28, 4), 255, "右上在 L 里");
        assert_eq!(at(&coverage, 32, 28, 28), 0, "右下的凹口不该被填");
    }

    #[test]
    fn 顶点不够就全零_不给出看起来正常的图() {
        let coverage = rasterize_polygon_coverage(&[[0.0, 0.0], [1.0, 1.0]], 8, 8);
        assert!(coverage.iter().all(|value| *value == 0));
    }

    #[test]
    fn 框外的顶点不报错_只是被裁掉() {
        // 巨大的三角形盖满整个框：框内应当全满。
        let points = [[-5.0, -5.0], [5.0, -5.0], [0.0, 5.0]];
        let coverage = rasterize_polygon_coverage(&points, 8, 8);
        assert!(coverage.iter().all(|value| *value == 255), "框内应当全满");
    }
}
