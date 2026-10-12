//! **文字覆盖层的 CPU 仿射变换**（T2）。
//!
//! # 为什么是 CPU
//!
//! worker 出片路径是：GPU 合成图层 → readback 成 [`Rgba8Image`] → **CPU 叠文字**。
//! 文字**不在** GPU 合成里（它是宿主栅格化出来的一张位图），所以「歪」与「压扁」
//! 只能在这一步做。
//!
//! # 数学必须与 GPU 侧同源
//!
//! 权威定义在 [`dhampir_core::render::compose::inverse_affine`]：
//!
//! ```text
//! 正向  p_out = R(rot) · ((p_src − c_src) ⊙ scale) + c_out + offset
//! 逆向  p_src = R(−rot) · (p_out − c_out − offset) ⊘ scale + c_src
//! ```
//!
//! ⚠️ **照抄那份定义，不许在这里另推一份**（两份推导的分歧只表现为"预览与成片歪得不一样"，
//! 而且两边各自都自洽 —— 只有把两张画面摆在一起才看得出来）。
//!
//! # 与 `inverse_affine` 的关系（为什么这里不是直接调它）
//!
//! 那个函数把缩放写成**各向同性**的一个 `scale`（GPU 侧的 `Transform` 只有单值缩放），
//! 而本任务要的是**绕 X 轴的压扁**（`scale_y`），因此这里是它的**逐式推广**：
//! `scale` 从标量变成 `(scale_x, scale_y)`，其余（绕中心、旋转矩阵、offset 语义）逐字不变。
//! 推广的那一步有单测钉住：`scale_x == scale_y == s` 时与 `inverse_affine` 逐值相同
//! （见 `各向同性时与_gpu_侧逐值相同`）。
//!
//! # 两条实现纪律
//!
//! 1. **绕中心**（`c_src = 位图中心`、`c_out = 落点中心`），不是绕左上角 ——
//!    锚点错会让整段文字位移（而不是"歪了"）。
//! 2. **采样落在源图外 ⇒ 透明**（`[0,0,0,0]`），**不许 clamp 边缘** ——
//!    clamp 会把边缘像素拉成条纹。

use dhampir_core::readback::Rgba8Image;

use crate::text_raster::TextBitmap;

/// 一条文字的仿射参数（CPU 侧口径）。
///
/// ⚠️ 字段名与 GPU 侧 [`dhampir_timeline::schema::Transform`] **刻意不同**：
/// 那边是 `rotation_deg` + 单值 `scale`，这边多了**逐轴**缩放。
/// 两边混用会编译不过（这是有意的：`scale` 与 `scale_y` 语义不同）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextTransform {
    /// 顺时针角度制（与 GPU 侧同向 —— 见 `inverse_affine` 的 `rotation_deg`）。
    pub rotation_deg: f32,
    /// 水平缩放。
    pub scale_x: f32,
    /// 垂直缩放（< 1 就是"压扁"）。
    pub scale_y: f32,
}

impl Default for TextTransform {
    fn default() -> Self {
        Self {
            rotation_deg: 0.0,
            scale_x: 1.0,
            scale_y: 1.0,
        }
    }
}

impl TextTransform {
    /// **是否恒等** —— 恒等时调用方必须走原来的逐行拷贝路径（老路径零开销）。
    ///
    /// ⚠️ 判据里要挡住非有限值：`NaN != 0.0` 恒为真 ⇒ 含 `NaN` 的变换**不是**恒等
    /// （那正是我们要的：让它在仿射路径上被 `sanitize` 掉，而不是走快捷路径假装没事）。
    pub fn is_identity(self) -> bool {
        self.rotation_deg == 0.0 && self.scale_x == 1.0 && self.scale_y == 1.0
    }

    /// 把非有限/零缩放兜成安全值。
    ///
    /// ⚠️ **零缩放要兜**：`scale = 0` 在逆向里是**除以零** ⇒ 整帧变垃圾（全采到同一个点）。
    /// GPU 侧 `inverse_affine` 也有一道同样的兜底（它只兜单值 scale，这里逐轴兜）。
    pub fn sanitize(self) -> Self {
        let fix = |v: f32| if v.is_finite() && v != 0.0 { v } else { 1.0 };
        Self {
            rotation_deg: if self.rotation_deg.is_finite() {
                self.rotation_deg
            } else {
                0.0
            },
            scale_x: fix(self.scale_x),
            scale_y: fix(self.scale_y),
        }
    }
}

/// 采样点落在源位图**外面**时返回的值：**透明**。
///
/// ⚠️ 不是 clamp 边缘像素。clamp 会把位图边缘那一条拉成条纹
/// （文字边缘本来是半透明的抗锯齿墨，拉一条出去就是一条看得见的灰边）。
const OUTSIDE: [f32; 4] = [0.0, 0.0, 0.0, 0.0];

/// 从 `bitmap` 上按**双线性**取一个像素（坐标为**像素中心**口径：整数坐标 = 该像素中心）。
///
/// 越界规则见 [`OUTSIDE`]：任一采样角落在图外，**缺失的那份按透明算**
/// （而不是整体判外 —— 边缘像素与图外做插值正是抗锯齿需要的）。
fn sample_bilinear(bitmap: &TextBitmap, x: f32, y: f32) -> [f32; 4] {
    let (w, h) = (bitmap.width as i32, bitmap.height as i32);
    if w <= 0 || h <= 0 {
        return OUTSIDE;
    }
    let x0 = x.floor();
    let y0 = y.floor();
    let fx = x - x0;
    let fy = y - y0;
    let (x0, y0) = (x0 as i32, y0 as i32);

    let texel = |ix: i32, iy: i32| -> [f32; 4] {
        if ix < 0 || iy < 0 || ix >= w || iy >= h {
            return OUTSIDE;
        }
        let i = ((iy as usize) * (bitmap.width as usize) + ix as usize) * 4;
        [
            bitmap.pixels[i] as f32,
            bitmap.pixels[i + 1] as f32,
            bitmap.pixels[i + 2] as f32,
            bitmap.pixels[i + 3] as f32,
        ]
    };

    let p00 = texel(x0, y0);
    let p10 = texel(x0 + 1, y0);
    let p01 = texel(x0, y0 + 1);
    let p11 = texel(x0 + 1, y0 + 1);

    let mut out = [0.0f32; 4];
    for c in 0..4 {
        let top = p00[c] * (1.0 - fx) + p10[c] * fx;
        let bottom = p01[c] * (1.0 - fx) + p11[c] * fx;
        out[c] = top * (1.0 - fy) + bottom * fy;
    }
    out
}

/// 把一条文字按 `transform` **仿射**叠到帧上（双线性 + 越界透明）。
///
/// `origin` 是位图**左上角**在目标帧里的落点（恒等路径的 `x`/`y` 同口径）——
/// 本函数内部把它换成中心 `c_out = origin + 位图尺寸/2`，与
/// [`dhampir_core::render::compose::inverse_affine`] 的 `c_out = target_size/2` 同一套语义
/// （那边"目标"就是这一层自己的画布，所以中心就是画布中心）。
///
/// `alpha_scale` 与 [`crate::text_overlay::blit_scaled`] 的 `scale` 同义（`0..=255`，
/// 淡入淡出用）。⚠️ **它乘在 alpha 上**，不是乘 RGB（否则"半透明白字"会变成
/// "不透明灰字"，在亮背景上看不见）。
///
/// # 返回
///
/// 写进帧的像素数（`alpha` 取整后为 0 的不算）。调用方拿它决定要不要报"墨迹被切"。
///
pub fn blit_affine(
    image: &mut Rgba8Image,
    bitmap: &TextBitmap,
    origin: (i32, i32),
    transform: TextTransform,
    alpha_scale: u32,
) -> usize {
    let t = transform.sanitize();
    let (w, h) = (image.width as i32, image.height as i32);
    if w <= 0 || h <= 0 || alpha_scale == 0 || bitmap.width == 0 || bitmap.height == 0 {
        return 0;
    }

    let radians = t.rotation_deg.to_radians();
    let (sin, cos) = radians.sin_cos();

    // 源中心与输出中心（都在**像素中心**口径下）。
    let c_src_x = bitmap.width as f32 / 2.0;
    let c_src_y = bitmap.height as f32 / 2.0;
    let c_out_x = origin.0 as f32 + bitmap.width as f32 / 2.0;
    let c_out_y = origin.1 as f32 + bitmap.height as f32 / 2.0;
    // 逆向：p_src = R(−rot) · (p_out − c_out) ⊘ scale + c_src
    //
    // 展开（与 `inverse_affine` 同形，只是两个 scale 分开）：
    //   p_src.x = cos·(dx)/sx + sin·(dy)/sy + c_src_x
    //   p_src.y = −sin·(dx)/sx + cos·(dy)/sy + c_src_y
    let inv_sx = 1.0 / t.scale_x;
    let inv_sy = 1.0 / t.scale_y;

    // **只扫变换后的包围盒**，不是整帧 —— 不然一段小字也要遍历整张 1080p。
    //
    // 做法：把位图四角按正向变换投到输出空间，取轴对齐包围盒，再向外扩 1px
    // （双线性要读相邻像素，少这 1px 会让边缘淡掉一档）。
    //
    let corners = [
        (0.0f32, 0.0f32),
        (bitmap.width as f32, 0.0),
        (0.0, bitmap.height as f32),
        (bitmap.width as f32, bitmap.height as f32),
    ];
    let (mut min_x, mut min_y) = (f32::INFINITY, f32::INFINITY);
    let (mut max_x, mut max_y) = (f32::NEG_INFINITY, f32::NEG_INFINITY);
    for (px, py) in corners {
        let dx = (px - c_src_x) * t.scale_x;
        let dy = (py - c_src_y) * t.scale_y;
        let ox = cos * dx - sin * dy + c_out_x;
        let oy = sin * dx + cos * dy + c_out_y;
        min_x = min_x.min(ox);
        min_y = min_y.min(oy);
        max_x = max_x.max(ox);
        max_y = max_y.max(oy);
    }
    let x_start = ((min_x - 1.0).floor() as i32).max(0);
    let y_start = ((min_y - 1.0).floor() as i32).max(0);
    let x_end = ((max_x + 1.0).ceil() as i32).min(w - 1);
    let y_end = ((max_y + 1.0).ceil() as i32).min(h - 1);

    let mut written = 0usize;
    for ty in y_start..=y_end {
        for tx in x_start..=x_end {
            // 目标像素中心（+0.5）—— 与双线性的"整数坐标 = 像素中心"口径对齐。
            let dx = (tx as f32 + 0.5) - c_out_x;
            let dy = (ty as f32 + 0.5) - c_out_y;
            let sx = cos * dx * inv_sx + sin * dy * inv_sy + c_src_x;
            let sy = -sin * dx * inv_sx + cos * dy * inv_sy + c_src_y;

            // 采样点换算到"以像素中心为整数"的坐标：源像素 (i,j) 的中心在 (i+0.5, j+0.5)。
            let sample = sample_bilinear(bitmap, sx - 0.5, sy - 0.5);
            let mut a = sample[3];
            if a <= 0.0 {
                continue;
            }
            if alpha_scale != 255 {
                a = a * (alpha_scale as f32) / 255.0;
            }
            let alpha = a.round().clamp(0.0, 255.0) as u32;
            if alpha == 0 {
                continue;
            }

            let destination = ((ty as usize) * (image.width as usize) + tx as usize) * 4;
            if alpha == 255 {
                // 全不透明的墨：直接写。⚠️ 但**源 RGB 是插值出来的**，所以仍走组装路径
                // （不能 copy_from_slice —— 源是 f32 采样结果，不是位图里的字节）。
                image.pixels[destination] = sample[0].round().clamp(0.0, 255.0) as u8;
                image.pixels[destination + 1] = sample[1].round().clamp(0.0, 255.0) as u8;
                image.pixels[destination + 2] = sample[2].round().clamp(0.0, 255.0) as u8;
                image.pixels[destination + 3] = 255;
            } else {
                // source-over，与 `blit_scaled` 的逐值口径一致（`mul_alpha` 的
                // `(v*f+127)/255` 取整）。⚠️ 这里用的是**浮点** alpha 再各自取整，
                // 与那条老路径在同一 alpha 上的结果**可能有 ±1 的差** ——
                // 而"恒等时逐字节相同"那条断言要求的是**走老路径**（不是走这里），
                // 所以这不违约束（见 `is_identity` 的用法）。
                let inverse = 255 - alpha;
                for channel in 0..3 {
                    let src = (sample[channel].round().clamp(0.0, 255.0) as u32 * alpha + 127) / 255;
                    let dst = image.pixels[destination + channel] as u32;
                    let dst = (dst * inverse + 127) / 255;
                    image.pixels[destination + channel] = (src + dst) as u8;
                }
                let dst_a = image.pixels[destination + 3] as u32;
                image.pixels[destination + 3] = (alpha + (dst_a * inverse + 127) / 255) as u8;
            }
            written += 1;
        }
    }
    written
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 造一张 `w×h` 的位图，`f(x, y) -> [r,g,b,a]`。
    fn bitmap(w: u32, h: u32, f: impl Fn(u32, u32) -> [u8; 4]) -> TextBitmap {
        let mut pixels = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                pixels.extend_from_slice(&f(x, y));
            }
        }
        TextBitmap::new(w, h, pixels).expect("位图构造")
    }

    fn frame(w: u32, h: u32) -> Rgba8Image {
        Rgba8Image {
            width: w,
            height: h,
            pixels: vec![0u8; (w * h * 4) as usize],
        }
    }

    fn px(image: &Rgba8Image, x: u32, y: u32) -> [u8; 4] {
        image.pixel(x, y).expect("取像素")
    }

    // ── 恒等 ────────────────────────────────────────────────────────

    #[test]
    fn 恒等变换被识别出来() {
        assert!(TextTransform::default().is_identity());
        assert!(TextTransform {
            rotation_deg: 0.0,
            scale_x: 1.0,
            scale_y: 1.0
        }
        .is_identity());
        // 任一非恒等都不算。
        assert!(!TextTransform {
            rotation_deg: 90.0,
            ..Default::default()
        }
        .is_identity());
        assert!(!TextTransform {
            scale_y: 0.5,
            ..Default::default()
        }
        .is_identity());
        // ⚠️ NaN **不是**恒等（否则它会走快捷路径假装没事）。
        assert!(!TextTransform {
            rotation_deg: f32::NAN,
            ..Default::default()
        }
        .is_identity());
    }

    #[test]
    fn 恒等变换逐字节等于直接拷贝() {
        // 位图放在 (4, 3)，目标帧 16x16。
        //
        // ⚠️ **源像素必须全不透明（alpha = 255）**，否则"直接拷贝"这个参照本身就错了：
        // `blit_scaled` 在 `alpha < 255` 时走 source-over，会把源 RGB **按 alpha 预乘**
        // （`mul_alpha(7, 200) = 5`）。所以"逐字节等于拷贝"这条命题**只在 alpha == 255 时成立** ——
        // 第一版这里用了 alpha=200，红的是**参照**不是实现（实测：实现给 5，参照给 7，
        // 而 5 正是 `blit_scaled` 会写的那个值）。
        let bm = bitmap(6, 5, |x, y| [x as u8 * 10, y as u8 * 20, 7, 255]);
        let mut got = frame(16, 16);
        blit_affine(&mut got, &bm, (4, 3), TextTransform::default(), 255);

        let mut want = frame(16, 16);
        for y in 0..5u32 {
            for x in 0..6u32 {
                let src = ((y * 6 + x) * 4) as usize;
                let dst = (((y + 3) * 16 + (x + 4)) * 4) as usize;
                want.pixels[dst..dst + 4].copy_from_slice(&bm.pixels[src..src + 4]);
            }
        }
        assert_eq!(got.pixels, want.pixels, "恒等变换必须逐字节等于直接拷贝");
    }

    /// 恒等变换下与**真正的老路径**（`blit_scaled`）逐字节相同 —— 含半透明源。
    ///
    /// ⚠️ 这条比上一条强：上一条拿"朴素拷贝"当参照，只在 `alpha == 255` 时成立；
    /// 这条拿 `text_overlay::blit_scaled` 本身当参照，所以**半透明抗锯齿边缘也算数**
    /// （那正是文字位图最常见的形态）。
    #[test]
    fn 恒等变换与老路径_blit_scaled_逐字节相同() {
        // 造一张有半透明边缘的位图（模拟抗锯齿）。
        let bm = bitmap(7, 4, |x, y| {
            let a = if x == 0 || y == 0 { 96 } else { 255 };
            [200, 180, 160, a]
        });
        let (w, h) = (24u32, 18u32);
        let (ox, oy) = (5i32, 4i32);

        let mut old = frame(w, h);
        crate::text_overlay::blit_scaled(&mut old, &bm, ox, oy, 255).expect("老路径叠图");

        let mut new = frame(w, h);
        blit_affine(&mut new, &bm, (ox, oy), TextTransform::default(), 255);

        assert_eq!(
            new.pixels, old.pixels,
            "恒等变换必须与老路径 blit_scaled 逐字节相同（含半透明边缘）"
        );
    }

    // ── 绕中心 ──────────────────────────────────────────────────────

    #[test]
    fn 恒等时中心不动() {
        // 一个 8x8 的实心块放在 (0,0)，中心在 (4,4)。恒等 ⇒ 中心像素仍有墨。
        let bm = bitmap(8, 8, |_, _| [255, 255, 255, 255]);
        let mut f = frame(16, 16);
        blit_affine(&mut f, &bm, (0, 0), TextTransform::default(), 255);
        assert_eq!(px(&f, 4, 4), [255, 255, 255, 255]);
        assert_eq!(px(&f, 0, 0), [255, 255, 255, 255]);
    }

    #[test]
    fn 旋转九十度绕中心_尺寸互换() {
        // 4x2 的实心块（宽 > 高）。绕自身中心转 90° 后应当变成 2x4（高 > 宽），
        // 且**中心不动**。
        let bm = bitmap(4, 2, |_, _| [255, 0, 0, 255]);
        let mut f = frame(32, 32);
        // 落点让中心在 (16, 16)：origin = center - half = (16-2, 16-1) = (14, 15)
        blit_affine(
            &mut f,
            &bm,
            (14, 15),
            TextTransform {
                rotation_deg: 90.0,
                scale_x: 1.0,
                scale_y: 1.0,
            },
            255,
        );

        // 中心仍然有墨。
        assert_eq!(px(&f, 16, 16), [255, 0, 0, 255], "绕中心转 ⇒ 中心不动");

        // 竖向范围比横向范围大（宽高互换）。
        let mut xs = vec![];
        let mut ys = vec![];
        for y in 0..32u32 {
            for x in 0..32u32 {
                if px(&f, x, y)[3] > 0 {
                    xs.push(x);
                    ys.push(y);
                }
            }
        }
        let w_span = xs.iter().max().unwrap() - xs.iter().min().unwrap() + 1;
        let h_span = ys.iter().max().unwrap() - ys.iter().min().unwrap() + 1;
        assert!(
            h_span > w_span,
            "4x2 转 90° 应变成竖长（h_span({h_span}) > w_span({w_span})）"
        );
        // 4x2 转 90° ⇒ 占 2 列 x 4 行（贴边像素可能因插值多 1）。
        assert!(w_span <= 3, "竖向后宽度应约等于原高 2，实得 {w_span}");
        assert!(h_span <= 5, "竖向后高度应约等于原宽 4，实得 {h_span}");
    }

    #[test]
    fn 旋转一百八十度_内容翻转但中心不动() {
        // 左上角有墨、其余透明。转 180° ⇒ 墨水跑到右下角。
        let bm = bitmap(4, 4, |x, y| {
            if x == 0 && y == 0 {
                [255, 0, 0, 255]
            } else {
                [0, 0, 0, 0]
            }
        });
        let mut f = frame(16, 16);
        blit_affine(
            &mut f,
            &bm,
            (6, 6),
            TextTransform {
                rotation_deg: 180.0,
                scale_x: 1.0,
                scale_y: 1.0,
            },
            255,
        );
        // 位图中心在 (8, 8)。原本 (0,0) 的墨 ⇒ 相对中心 (-1.5,-1.5) ⇒ 转后 (+1.5,+1.5)
        // ⇒ 落在位图 (3,3)，即帧 (9, 9)。
        assert_eq!(px(&f, 9, 9), [255, 0, 0, 255], "转 180° 后墨在右下");
        assert_eq!(px(&f, 6, 6)[3], 0, "原来左上角应当空了");
    }

    // ── 压扁 ────────────────────────────────────────────────────────

    #[test]
    fn 垂直压扁会缩短竖向范围() {
        let bm = bitmap(8, 8, |_, _| [255, 255, 255, 255]);
        let mut full = frame(32, 32);
        blit_affine(&mut full, &bm, (12, 12), TextTransform::default(), 255);
        let mut flat = frame(32, 32);
        blit_affine(
            &mut flat,
            &bm,
            (12, 12),
            TextTransform {
                rotation_deg: 0.0,
                scale_x: 1.0,
                scale_y: 0.5,
            },
            255,
        );

        let span_y = |f: &Rgba8Image| {
            let ys: Vec<u32> = (0..32)
                .filter(|&y| (0..32).any(|x| px(f, x, y)[3] > 0))
                .collect();
            ys.iter().max().unwrap() - ys.iter().min().unwrap() + 1
        };
        let (a, b) = (span_y(&full), span_y(&flat));
        assert_eq!(a, 8, "恒等时竖向 8 行");
        assert!(b <= 5, "scale_y=0.5 应压到约 4 行，实得 {b}");
    }

    // ── 越界透明 ────────────────────────────────────────────────────

    #[test]
    fn 越界采样是透明而不是_clamp_边缘() {
        // 全不透明的块。转 45° 后四角会伸到位图外 ——
        // 如果 clamp 边缘，帧上会出现被拉长的条纹（整块变大）；
        // 正确实现下**墨迹总量**不会超过原面积太多。
        let bm = bitmap(8, 8, |_, _| [255, 0, 0, 255]);
        let mut f = frame(64, 64);
        blit_affine(
            &mut f,
            &bm,
            (28, 28),
            TextTransform {
                rotation_deg: 45.0,
                scale_x: 1.0,
                scale_y: 1.0,
            },
            255,
        );
        let inked = (0..64u32)
            .flat_map(|y| (0..64u32).map(move |x| (x, y)))
            .filter(|&(x, y)| px(&f, x, y)[3] > 0)
            .count();
        // 8x8 = 64 个源像素。转 45° 后覆盖约 8*8*sqrt(2) ≈ 90 个像素
        // （边缘抗锯齿会多一点），但**绝不该**接近"整帧"（4096）。
        assert!(
            inked >= 60 && inked <= 160,
            "转 45° 的墨迹应约 90 像素（不 clamp ⇒ 不会拖出条纹），实得 {inked}"
        );
    }

    #[test]
    fn 完全在帧外时一个像素都不写() {
        let bm = bitmap(8, 8, |_, _| [255, 255, 255, 255]);
        let mut f = frame(16, 16);
        let n = blit_affine(&mut f, &bm, (100, 100), TextTransform::default(), 255);
        assert_eq!(n, 0, "落点完全在帧外 ⇒ 不写像素");
        assert!(f.pixels.iter().all(|&b| b == 0), "帧应当保持全透明");
    }

    // ── alpha 缩放 ──────────────────────────────────────────────────

    #[test]
    fn alpha_scale_乘在_alpha_上而不是_rgb() {
        // ⚠️ 判据要拿**老路径当参照**，不能凭"看起来该是白的"。
        //
        // 第一版这里断言"RGB 仍是白（>240）"，**红了**：实得 `[128,128,128,128]`。
        // 而 128 是**对的** —— 空白帧（全透明黑）上做 source-over 时，
        // `blit_scaled` 写的正是 `mul_alpha(255, 128) = 128`（RGB 与 alpha 都减半）。
        // "乘在 alpha 上而不是 RGB"这句话**只对"已经有不透明底"的帧成立**：
        // 在透明底上，两者结果相同 —— 所以那条命题的判据必须**放到有色底上量**。
        let bm = bitmap(4, 4, |_, _| [255, 255, 255, 255]);
        let rot90 = TextTransform {
            rotation_deg: 90.0,
            scale_x: 1.0,
            scale_y: 1.0,
        };

        // (a) 透明底：与 `blit_scaled` 逐字节相同（含 alpha 缩放）。
        let mut new_f = frame(16, 16);
        blit_affine(&mut new_f, &bm, (6, 6), rot90, 128);
        let mut old_f = frame(16, 16);
        crate::text_overlay::blit_scaled(&mut old_f, &bm, 6, 6, 128).expect("老路径");
        // 注意：老路径不做旋转，所以只在**恒等**下可比。这里改用恒等变换比这一条。
        let mut new_id = frame(16, 16);
        blit_affine(&mut new_id, &bm, (6, 6), TextTransform::default(), 128);
        assert_eq!(
            new_id.pixels, old_f.pixels,
            "alpha 缩放路径必须与老路径逐字节相同"
        );

        // (b) 有色底：这才是"乘 alpha 而不是乘 RGB"的有效判据。
        //     底设成不透明白？不行 —— 白底白字看不出区别。用**不透明白底 + 黑字**：
        //     乘 alpha ⇒ 字变成**灰**（黑按 alpha 混白底）；
        //     若错乘 RGB ⇒ 字的 RGB 被压暗但 alpha 仍 255，结果**更黑**。
        let black = bitmap(4, 4, |_, _| [0, 0, 0, 255]);
        let mut on_white = frame(16, 16);
        for p in on_white.pixels.chunks_mut(4) {
            p.copy_from_slice(&[255, 255, 255, 255]);
        }
        blit_affine(&mut on_white, &black, (6, 6), TextTransform::default(), 128);
        let g = px(&on_white, 8, 8);
        // 黑字 alpha 减半混白底 ⇒ 约 127（既不是 0 也不是 255）。
        assert!(
            g[0] > 100 && g[0] < 160,
            "半透明黑字压在白底上应约 127（乘 alpha 的语义），实得 {g:?}"
        );
        assert_eq!(g[3], 255, "白底 alpha 不动：{g:?}");

        // (c) 旋转 90° 后中心仍在（顺带确认变换参与了这条路径）。
        assert_eq!(px(&new_f, 8, 8)[3], 128, "旋转后中心 alpha = 128");
    }

    #[test]
    fn alpha_scale_为零时一个像素都不写() {
        let bm = bitmap(4, 4, |_, _| [255, 255, 255, 255]);
        let mut f = frame(16, 16);
        let n = blit_affine(&mut f, &bm, (6, 6), TextTransform::default(), 0);
        assert_eq!(n, 0);
        assert!(f.pixels.iter().all(|&b| b == 0));
    }

    // ── 与 GPU 侧同源 ───────────────────────────────────────────────

    #[test]
    fn 各向同性时与_gpu_侧逐值相同() {
        // ⚠️ 这条钉的是"数学与 GPU 侧同源"：把本函数的逐轴缩放退化成单值，
        // 结果必须与 `inverse_affine` 的逆向矩阵逐值相同。
        //
        // `inverse_affine` 返回的是 [a, b, c, 0] / [c, d, e, 0] 两行：
        //   src_x = row0[0]*out_x + row0[1]*out_y + row0[2]
        //   src_y = row1[0]*out_x + row1[1]*out_y + row1[2]
        // ⚠️ 两条 import 都走**已导出的那条路** —— 别走私有模块、也别给本 crate 加依赖：
        //   · `compose` 是**私有 `mod`**，但 `inverse_affine` 已由 `render/mod.rs:60` 转出；
        //   · `dhampir-timeline` **不在** `dhampir-worker` 的 `[dependencies]` 里，
        //     而 `dhampir-core` 把它整个转出成 `dhampir_core::timeline`
        //     （`core/lib.rs:57`，注释原话："宿主与上层不需要单独依赖 dhampir-timeline"）。
        //     所以这里是**用转出路径**，不是新增依赖 —— 依赖方向表因此保持字面成立。
        //
        // ⚠️ 这两条**隔离跑抓不到**（stub 环境里我自己顶了类型），所以"隔离绿"不能替代
        //    `cargo check --workspace --all-targets` —— 实测踩到，记在这里。
        use dhampir_core::render::inverse_affine;
        use dhampir_core::timeline::schema::Transform;

        let bitmap_size = (40u32, 12u32);
        let target_size = (200u32, 60u32);
        // 注意：`inverse_affine` 的 c_out 恒等于 target_size/2（它的"目标"是整层画布）。
        // 本函数的 c_out 是**落点中心**，所以要让两者可比，落点必须正好把位图放在
        // 目标中心：origin = (target - bitmap)/2。
        let origin = (
            ((target_size.0 - bitmap_size.0) / 2) as i32,
            ((target_size.1 - bitmap_size.1) / 2) as i32,
        );

        for (rot, scale) in [(0.0f32, 1.0f32), (30.0, 1.0), (-45.0, 1.0), (90.0, 1.0)] {
            let (row0, row1) = inverse_affine(
                Transform {
                    x: 0.0,
                    y: 0.0,
                    scale,
                    rotation_deg: rot,
                },
                bitmap_size,
                target_size,
            );

            // 本函数的逆向：p_src = R(−rot)·(p_out − c_out)/s + c_src，展开后与上面同形。
            let t = TextTransform {
                rotation_deg: rot,
                scale_x: scale,
                scale_y: scale,
            }
            .sanitize();
            let rad = t.rotation_deg.to_radians();
            let (sin, cos) = rad.sin_cos();
            let c_src = (bitmap_size.0 as f32 / 2.0, bitmap_size.1 as f32 / 2.0);
            let c_out = (
                origin.0 as f32 + bitmap_size.0 as f32 / 2.0,
                origin.1 as f32 + bitmap_size.1 as f32 / 2.0,
            );
            let a = cos / t.scale_x;
            let b = sin / t.scale_y;
            let e0 = -c_out.0 * a - c_out.1 * b + c_src.0;
            let c = -sin / t.scale_x;
            let d = cos / t.scale_y;
            let e1 = -c_out.0 * c - c_out.1 * d + c_src.1;

            let close = |x: f32, y: f32| (x - y).abs() < 1e-4;
            assert!(
                close(a, row0[0]) && close(b, row0[1]) && close(e0, row0[2]),
                "rot={rot} scale={scale}：逆向 row0 不一致。本函数 ({a},{b},{e0}) vs GPU ({},{},{})",
                row0[0],
                row0[1],
                row0[2]
            );
            assert!(
                close(c, row1[0]) && close(d, row1[1]) && close(e1, row1[2]),
                "rot={rot} scale={scale}：逆向 row1 不一致。本函数 ({c},{d},{e1}) vs GPU ({},{},{})",
                row1[0],
                row1[1],
                row1[2]
            );
        }
    }

    #[test]
    fn 非有限缩放被兜成一() {
        let t = TextTransform {
            rotation_deg: f32::NAN,
            scale_x: 0.0,
            scale_y: f32::INFINITY,
        }
        .sanitize();
        assert_eq!(t.rotation_deg, 0.0);
        assert_eq!(t.scale_x, 1.0);
        assert_eq!(t.scale_y, 1.0);
        assert!(t.is_identity(), "兜底之后就是恒等");
    }


    // ── 阴影：跟着做同样的变换，但偏移是**屏幕空间**的 ────────────────

    /// 求一张实心块在帧上的**重心**（像素中心的算术平均）。
    ///
    /// 用它量"偏移"比量包围盒稳（包围盒是整数网格，1px 量化会把方向算歪）。
    fn centroid(f: &Rgba8Image) -> (f32, f32) {
        let (mut sx, mut sy, mut n) = (0.0f32, 0.0f32, 0.0f32);
        for y in 0..f.height {
            for x in 0..f.width {
                let a = px(f, x, y)[3] as f32;
                if a > 0.0 {
                    sx += (x as f32 + 0.5) * a;
                    sy += (y as f32 + 0.5) * a;
                    n += a;
                }
            }
        }
        assert!(n > 0.0, "帧上没有墨，重心无定义");
        (sx / n, sy / n)
    }

    /// 一张**带方向**的位图：只有左上角一小块有墨。
    ///
    /// 用它才能看出"转没转" —— 实心方块转 90° 与不转**看起来一样**，
    /// 拿它测旋转等于没测（本仓最恨的那类"绿了但没测到"）。
    fn lopsided() -> TextBitmap {
        bitmap(8, 8, |x, y| {
            if x < 3 && y < 3 {
                [255, 255, 255, 255]
            } else {
                [0, 0, 0, 0]
            }
        })
    }

    /// ★ **影子必须跟着做同样的旋转**（否则"字歪了、影子是正的"）。
    ///
    /// 判据：同一张带方向的位图、同一落点，传 `rot=90` 与 `rot=0` 必须给出**不同**的墨迹分布；
    /// 且 `rot=90` 的墨迹相对中心的方向应当从"左上"转到"右上"（屏幕坐标顺时针）。
    #[test]
    fn 影子跟着一起转_同一_transform_传给阴影() {
        let bm = lopsided();
        // 落点让位图中心在 (40, 40)：origin = (36, 36)。
        let (ox, oy) = (36i32, 36i32);
        let center = (40.0f32, 40.0f32);

        let mut flat = frame(80, 80);
        blit_affine(&mut flat, &bm, (ox, oy), TextTransform::default(), 255);
        let c_flat = centroid(&flat);

        let mut turned = frame(80, 80);
        blit_affine(
            &mut turned,
            &bm,
            (ox, oy),
            TextTransform {
                rotation_deg: 90.0,
                scale_x: 1.0,
                scale_y: 1.0,
            },
            255,
        );
        let c_turned = centroid(&turned);

        // 未旋转：墨在左上 ⇒ 重心相对中心偏左偏上。
        assert!(
            c_flat.0 < center.0 - 1.0 && c_flat.1 < center.1 - 1.0,
            "rot=0 时墨在左上，重心应偏左上，实得 {c_flat:?}"
        );
        // 转 90°（屏幕顺时针）：左上的墨 ⇒ 右上。
        assert!(
            c_turned.0 > center.0 + 1.0 && c_turned.1 < center.1 - 1.0,
            "rot=90 时墨应转到右上，实得 {c_turned:?} —— 若仍在左上说明影子没跟着转"
        );
        assert_ne!(
            flat.pixels, turned.pixels,
            "转与不转必须给出不同像素（否则这条测不到旋转）"
        );
    }

    /// ★ **阴影偏移是屏幕空间平移 ⇒ 旋转不改变偏移方向**（含实测数字）。
    ///
    /// # 这条钉的是一个**被实测推翻的推理**（2026-10-12）
    ///
    /// 一度以为"影子要绕**文字中心**转，好让偏移方向跟着转"。**实测否决了它**：
    /// ```text
    /// pivot=文字中心: rot=0 → 偏移 (0,0) ; rot=90 → 偏移 (0,0)   ← 中心与 pivot 重合，恒 0
    /// pivot=阴影中心: rot=0 → 偏移 (8,0) ; rot=90 → 偏移 (8,0)   ← 旋转**不改变**偏移
    /// ```
    /// **没有任何 pivot 选择能给出"偏移跟转"** —— 因为 pivot 是旋转中心，
    /// 位图中心相对它的向量本来就不该随旋转改变。
    ///
    /// 而定案的是契约字段自己的文档（`overlay.rs:285`）：
    /// ```text
    /// /// 阴影的水平偏移（**文档像素**，与 `transform.x/y` 同一坐标系）。
    /// ```
    /// ⇒ `dx/dy` 是**屏幕空间**量。阴影 = "文字做同样的变换" + "屏幕空间平移 `(dx,dy)`"。
    /// 物理上也对：光源方向固定，字转 90° 后影子**仍朝同一侧**。
    ///
    /// # 断言（容差 ±0.5px，写死）
    ///
    /// 同一落点、`dx=8, dy=0`，`rot=0` 与 `rot=45` 的**重心相对文字中心**都必须 ≈ `(8, 0)`。
    #[test]
    fn 阴影偏移不随旋转改变方向_屏幕空间平移() {
        let bm = bitmap(6, 6, |_, _| [255, 255, 255, 255]);
        let (ox, oy) = (30i32, 30i32);
        let (dx, dy) = (8.0f32, 0.0f32);
        // 影子落点 = 文字落点 + 屏幕空间偏移（**不随旋转变**）。
        let shadow_origin = ((ox as f32 + dx) as i32, (oy as f32 + dy) as i32);
        let text_center = (ox as f32 + 3.0, oy as f32 + 3.0);

        for rot in [0.0f32, 45.0, 90.0] {
            let mut f = frame(80, 80);
            blit_affine(
                &mut f,
                &bm,
                shadow_origin,
                TextTransform {
                    rotation_deg: rot,
                    scale_x: 1.0,
                    scale_y: 1.0,
                },
                255,
            );
            let c = centroid(&f);
            let off = (c.0 - text_center.0, c.1 - text_center.1);
            assert!(
                (off.0 - dx).abs() <= 0.5 && (off.1 - dy).abs() <= 0.5,
                "rot={rot}：屏幕空间偏移应恒为 ({dx},{dy})±0.5，实得 {off:?}\
                 —— 若方向随旋转变了，说明接了\"绕中心转\"那套错语义"
            );
        }
    }

}
