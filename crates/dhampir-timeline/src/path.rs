//! SVG 路径（`d`）→ 折线的**细分器**（`clip-path: path(...)` 用的那一份）。
//!
//! # 为什么在 timeline 而不是 core
//!
//! 它是**契约层的语义**（`ClipShape::Path` 怎么读），而且**没有 GPU 依赖** ——
//! 放在这里，校验层与渲染层用的是**同一份**解析：解析错误能在 `probe` 阶段就报给作者，
//! 而不是等到渲染时才炸。
//!
//! # 口径（写下来，因为两端共用）
//!
//! - 坐标是**文档像素**，原点是图层框的**左上角**（与 CSS `clip-path: path()` 同一套 ——
//!   而 CSS 的 `path()` 也只用 px，所以 DOM 侧可以**原样透传**，不必换算）；
//! - 支持命令：`M m L l H h V v C c Q q S s T t A a Z z`（第 26 轮补齐 `A` 与光滑续接）；
//! - 弧按 SVG 规范 F.6.5 的**端点参数化**转成圆心参数（含"半径不够大就按规范放大"那一步），
//!   再按固定段数采样；`S`/`T` 是**控制点镜像**，与显式写出等价（有等价性单测钉着）；
//! - 认不出的命令仍然**报错点名**，不猜一个近似；
//! - 曲线按**固定段数**细分（三次 16 段、二次 8 段）：确定性优先 ——
//!   自适应细分会让「两端算出来的折线不完全一样」，而那正是这个项目最要避免的。

/// 三次贝塞尔每段的细分数。
const CUBIC_SEGMENTS: u32 = 16;
/// 二次贝塞尔每段的细分数。
const QUADRATIC_SEGMENTS: u32 = 8;
/// 椭圆弧的采样段数。
const ARC_SEGMENTS: u32 = 24;

/// 把 SVG 路径数据细分成折线顶点（**文档像素**）。失败时返回一句人话。
pub fn flatten_path(data: &str) -> Result<Vec<[f32; 2]>, String> {
    let tokens = tokenize(data)?;
    let mut points: Vec<[f32; 2]> = Vec::new();
    let mut current = [0.0f32, 0.0f32];
    let mut start = [0.0f32, 0.0f32];
    // `S` / `T` 要**镜像上一个控制点**，所以得一路记着。
    let mut last_cubic_control: Option<[f32; 2]> = None;
    let mut last_quadratic_control: Option<[f32; 2]> = None;
    let mut index = 0usize;
    let mut command = ' ';
    while index < tokens.len() {
        if let Token::Command(letter) = tokens[index] {
            command = letter;
            index += 1;
            if command == 'Z' || command == 'z' {
                if !points.is_empty() {
                    current = start;
                }
                continue;
            }
        } else if command == ' ' {
            return Err("路径必须以命令字母开头（M / L / C …）".to_string());
        }
        let relative = command.is_ascii_lowercase();
        let upper = command.to_ascii_uppercase();
        let arity = match upper {
            'M' | 'L' => 2,
            'H' | 'V' => 1,
            'C' => 6,
            'S' => 4,
            'Q' => 4,
            'T' => 2,
            'A' => 7,
            other => {
                return Err(unsupported(other));
            }
        };
        if index + arity > tokens.len() {
            return Err(format!("命令 {command} 后面的数字不够（要 {arity} 个）"));
        }
        let mut take = |count: usize| -> Result<Vec<f32>, String> {
            let mut out = Vec::with_capacity(count);
            for _ in 0..count {
                match tokens[index] {
                    Token::Number(value) => {
                        out.push(value);
                        index += 1;
                    }
                    _ => return Err(format!("命令 {command} 后面的位置应当是数字")),
                }
            }
            Ok(out)
        };
        let offset = |x: f32, y: f32| -> [f32; 2] {
            if relative {
                [current[0] + x, current[1] + y]
            } else {
                [x, y]
            }
        };
        match upper {
            'M' => {
                let values = take(2)?;
                current = offset(values[0], values[1]);
                start = current;
                points.push(current);
                command = if relative { 'l' } else { 'L' };
            }
            'L' => {
                let values = take(2)?;
                current = offset(values[0], values[1]);
                points.push(current);
            }
            'H' => {
                let values = take(1)?;
                current = if relative {
                    [current[0] + values[0], current[1]]
                } else {
                    [values[0], current[1]]
                };
                points.push(current);
            }
            'V' => {
                let values = take(1)?;
                current = if relative {
                    [current[0], current[1] + values[0]]
                } else {
                    [current[0], values[0]]
                };
                points.push(current);
            }
            'C' => {
                let values = take(6)?;
                let c1 = offset(values[0], values[1]);
                let c2 = offset(values[2], values[3]);
                let end = offset(values[4], values[5]);
                last_cubic_control = Some(c2);
                last_quadratic_control = None;
                for step in 1..=CUBIC_SEGMENTS {
                    let t = step as f32 / CUBIC_SEGMENTS as f32;
                    points.push(cubic_at(current, c1, c2, end, t));
                }
                current = end;
            }
            'S' => {
                let values = take(4)?;
                // 第一个控制点是**上一个三次控制点关于当前点的镜像**（前面不是 C/S 时就取当前点）。
                let c1 = match last_cubic_control {
                    Some(previous) => [2.0 * current[0] - previous[0], 2.0 * current[1] - previous[1]],
                    None => current,
                };
                let c2 = offset(values[0], values[1]);
                let end = offset(values[2], values[3]);
                for step in 1..=CUBIC_SEGMENTS {
                    let t = step as f32 / CUBIC_SEGMENTS as f32;
                    points.push(cubic_at(current, c1, c2, end, t));
                }
                last_cubic_control = Some(c2);
                last_quadratic_control = None;
                current = end;
            }
            'T' => {
                let values = take(2)?;
                let control = match last_quadratic_control {
                    Some(previous) => [2.0 * current[0] - previous[0], 2.0 * current[1] - previous[1]],
                    None => current,
                };
                let end = offset(values[0], values[1]);
                for step in 1..=QUADRATIC_SEGMENTS {
                    let t = step as f32 / QUADRATIC_SEGMENTS as f32;
                    points.push(quadratic_at(current, control, end, t));
                }
                last_quadratic_control = Some(control);
                last_cubic_control = None;
                current = end;
            }
            'A' => {
                let values = take(7)?;
                let end = offset(values[5], values[6]);
                points.extend(arc_points(
                    current,
                    values[0],
                    values[1],
                    values[2],
                    values[3] != 0.0,
                    values[4] != 0.0,
                    end,
                ));
                last_cubic_control = None;
                last_quadratic_control = None;
                current = end;
            }
            'Q' => {
                let values = take(4)?;
                let control = offset(values[0], values[1]);
                let end = offset(values[2], values[3]);
                last_quadratic_control = Some(control);
                last_cubic_control = None;
                for step in 1..=QUADRATIC_SEGMENTS {
                    let t = step as f32 / QUADRATIC_SEGMENTS as f32;
                    points.push(quadratic_at(current, control, end, t));
                }
                current = end;
            }
            _ => unreachable!(),
        }
    }
    Ok(points)
}

/// 不支持的命令：把**是哪一条**说清楚，不留"反正不支持"。
fn unsupported(letter: char) -> String {
    let mut text = String::from("不支持路径命令 ");
    text.push(letter);
    text.push_str("（本仓只做 M / L / H / V / C / Q / Z；弧 A 与光滑续接 S/T 明说不支持，不猜近似）");
    text
}

/// 椭圆弧：**端点参数化 → 圆心参数化**（SVG 规范 F.6.5），再按固定段数采样。
///
/// 规范里那一步"半径不够大"的放大（`Λ > 1`）必须做：不做的话圆心解出 NaN，
/// 而那表现为"这个形状整个不见了"——很难查。
#[allow(clippy::too_many_arguments)]
fn arc_points(
    from: [f32; 2],
    rx_in: f32,
    ry_in: f32,
    x_rotation_deg: f32,
    large_arc: bool,
    sweep: bool,
    to: [f32; 2],
) -> Vec<[f32; 2]> {
    let mut rx = rx_in.abs();
    let mut ry = ry_in.abs();
    // 半径是 0（或端点重合）时长弧退化成一条直线。
    if rx == 0.0 || ry == 0.0 || (from[0] == to[0] && from[1] == to[1]) {
        return vec![to];
    }
    let phi = x_rotation_deg.to_radians();
    let (cos_phi, sin_phi) = (phi.cos(), phi.sin());
    let dx2 = (from[0] - to[0]) / 2.0;
    let dy2 = (from[1] - to[1]) / 2.0;
    let x1p = cos_phi * dx2 + sin_phi * dy2;
    let y1p = -sin_phi * dx2 + cos_phi * dy2;
    let lambda = (x1p * x1p) / (rx * rx) + (y1p * y1p) / (ry * ry);
    if lambda > 1.0 {
        let scale = lambda.sqrt();
        rx *= scale;
        ry *= scale;
    }
    let numerator = (rx * rx * ry * ry - rx * rx * y1p * y1p - ry * ry * x1p * x1p).max(0.0);
    let denominator = rx * rx * y1p * y1p + ry * ry * x1p * x1p;
    let mut coefficient = if denominator == 0.0 {
        0.0
    } else {
        (numerator / denominator).sqrt()
    };
    if large_arc == sweep {
        coefficient = -coefficient;
    }
    let cxp = coefficient * (rx * y1p / ry);
    let cyp = -coefficient * (ry * x1p / rx);
    let cx = cos_phi * cxp - sin_phi * cyp + (from[0] + to[0]) / 2.0;
    let cy = sin_phi * cxp + cos_phi * cyp + (from[1] + to[1]) / 2.0;
    let angle = |ux: f32, uy: f32, vx: f32, vy: f32| -> f32 {
        let dot = ux * vx + uy * vy;
        let len = ((ux * ux + uy * uy) * (vx * vx + vy * vy)).sqrt();
        let mut value = if len == 0.0 { 0.0 } else { (dot / len).clamp(-1.0, 1.0).acos() };
        if ux * vy - uy * vx < 0.0 {
            value = -value;
        }
        value
    };
    let ux = (x1p - cxp) / rx;
    let uy = (y1p - cyp) / ry;
    let vx = (-x1p - cxp) / rx;
    let vy = (-y1p - cyp) / ry;
    let theta_start = angle(1.0, 0.0, ux, uy);
    let mut delta = angle(ux, uy, vx, vy);
    if !sweep && delta > 0.0 {
        delta -= std::f32::consts::TAU;
    }
    if sweep && delta < 0.0 {
        delta += std::f32::consts::TAU;
    }
    let mut out = Vec::with_capacity(ARC_SEGMENTS as usize);
    for step in 1..=ARC_SEGMENTS {
        let t = step as f32 / ARC_SEGMENTS as f32;
        let theta = theta_start + delta * t;
        let (ex, ey) = (rx * theta.cos(), ry * theta.sin());
        out.push([
            cos_phi * ex - sin_phi * ey + cx,
            sin_phi * ex + cos_phi * ey + cy,
        ]);
    }
    out
}

fn cubic_at(p0: [f32; 2], p1: [f32; 2], p2: [f32; 2], p3: [f32; 2], t: f32) -> [f32; 2] {
    let u = 1.0 - t;
    let (a, b, c, d) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
    [
        a * p0[0] + b * p1[0] + c * p2[0] + d * p3[0],
        a * p0[1] + b * p1[1] + c * p2[1] + d * p3[1],
    ]
}

fn quadratic_at(p0: [f32; 2], p1: [f32; 2], p2: [f32; 2], t: f32) -> [f32; 2] {
    let u = 1.0 - t;
    let (a, b, c) = (u * u, 2.0 * u * t, t * t);
    [
        a * p0[0] + b * p1[0] + c * p2[0],
        a * p0[1] + b * p1[1] + c * p2[1],
    ]
}

enum Token {
    Command(char),
    Number(f32),
}

/// 把 `d` 切成命令与数字。数字之间可以只用逗号或空格分隔（SVG 的规矩）。
fn tokenize(data: &str) -> Result<Vec<Token>, String> {
    let mut out = Vec::new();
    let chars: Vec<char> = data.chars().collect();
    let mut index = 0usize;
    while index < chars.len() {
        let ch = chars[index];
        if ch.is_whitespace() || ch == ',' {
            index += 1;
            continue;
        }
        if ch.is_ascii_alphabetic() {
            out.push(Token::Command(ch));
            index += 1;
            continue;
        }
        let start = index;
        if ch == '-' || ch == '+' {
            index += 1;
        }
        let mut seen_dot = false;
        let mut seen_exp = false;
        while index < chars.len() {
            let digit = chars[index];
            if digit.is_ascii_digit() {
                index += 1;
            } else if digit == '.' && !seen_dot && !seen_exp {
                seen_dot = true;
                index += 1;
            } else if (digit == 'e' || digit == 'E') && !seen_exp {
                seen_exp = true;
                index += 1;
                if index < chars.len() && (chars[index] == '-' || chars[index] == '+') {
                    index += 1;
                }
            } else {
                break;
            }
        }
        let text: String = chars[start..index].iter().collect();
        let value = text
            .parse::<f32>()
            .map_err(|_| format!("读不出这个数字：{text}"))?;
        out.push(Token::Number(value));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 折线路径按顶点原样给出来() {
        let points = flatten_path("M 0 0 L 32 0 L 32 32 Z").expect("能解析");
        assert_eq!(points, vec![[0.0, 0.0], [32.0, 0.0], [32.0, 32.0]]);
    }

    #[test]
    fn 相对命令与横竖线() {
        let points = flatten_path("M 4 4 h 8 v 8 z").expect("能解析");
        assert_eq!(points, vec![[4.0, 4.0], [12.0, 4.0], [12.0, 12.0]]);
    }

    #[test]
    fn 三次曲线的终点对得上而且点数确定() {
        let points = flatten_path("M 0 0 C 0 10 10 10 10 0").expect("能解析");
        assert_eq!(points.len(), 1 + CUBIC_SEGMENTS as usize, "固定段数 ⇒ 点数可预期");
        let last = points[points.len() - 1];
        assert!((last[0] - 10.0).abs() < 1e-4 && last[1].abs() < 1e-4, "终点应当是 (10, 0)");
        let middle = points[CUBIC_SEGMENTS as usize / 2];
        assert!(middle[1] > 1.0, "中点应当被曲线抬起来，得到 {middle:?}");
    }

    #[test]
    fn 二次曲线也能细分() {
        let points = flatten_path("M 0 0 Q 5 10 10 0").expect("能解析");
        assert_eq!(points.len(), 1 + QUADRATIC_SEGMENTS as usize);
    }

    #[test]
    fn 第一个命令之后的数字会重复上一条命令() {
        let points = flatten_path("M 0 0 10 0 10 10").expect("能解析");
        assert_eq!(points, vec![[0.0, 0.0], [10.0, 0.0], [10.0, 10.0]]);
    }

    #[test]
    fn 认不出的命令明说而不是猜一个近似() {
        // A / S / T 从第 26 轮起是支持的，所以这里换真正认不出的命令。
        let error = flatten_path("M 0 0 X 5 5").expect_err("应当报错");
        assert!(error.contains('X'), "错误里要点出那个命令：{error}");
    }

    #[test]
    fn 弧上的点都在那个圆上_而且终点精确() {
        // 半径 5、弦长 √200 > 2r ⇒ 规范会把半径放大到 5√2，圆心是 (5,5)。
        let points = flatten_path("M 0 0 A 5 5 0 0 1 10 10").expect("能解析");
        // 起点的那个 `M` 也占一个（与曲线那两条判据同一个口径）。
        assert_eq!(points.len(), 1 + ARC_SEGMENTS as usize);
        let last = points[points.len() - 1];
        assert!(
            (last[0] - 10.0).abs() < 1e-3 && (last[1] - 10.0).abs() < 1e-3,
            "终点要精确：{last:?}"
        );
        // **用方程判**（不判轴向）：每个采样点到圆心的距离都该是那个半径。
        let radius = (2.0f32).sqrt() * 5.0;
        for point in &points {
            let distance = ((point[0] - 5.0).powi(2) + (point[1] - 5.0).powi(2)).sqrt();
            assert!(
                (distance - radius).abs() < 1e-2,
                "点 {point:?} 到圆心是 {distance}，应当 ≈ {radius}"
            );
        }
    }

    #[test]
    fn 弧的_sweep_决定往哪边鼓() {
        let clockwise = flatten_path("M 0 0 A 5 5 0 0 1 10 0").expect("能解析");
        let counter = flatten_path("M 0 0 A 5 5 0 0 0 10 0").expect("能解析");
        let peak = |points: &[[f32; 2]]| {
            points
                .iter()
                .map(|point| point[1])
                .fold(0.0f32, |acc, y| if y.abs() > acc.abs() { y } else { acc })
        };
        let (a, b) = (peak(&clockwise), peak(&counter));
        assert!(a * b < 0.0, "两种 sweep 应当往相反方向鼓：{a} vs {b}");
        for point in clockwise.iter().chain(counter.iter()) {
            let distance = ((point[0] - 5.0).powi(2) + point[1].powi(2)).sqrt();
            assert!((distance - 5.0).abs() < 1e-2, "点 {point:?} 应当在半径 5 的圆上");
        }
    }

    #[test]
    fn 光滑续接与显式写出控制点等价() {
        // C 之后用 S 镜像 —— 与把镜像出来的控制点**显式写出来**必须完全一致。
        let smooth = flatten_path("M 0 0 C 0 10 10 10 10 0 S 20 -10 20 0").expect("能解析");
        let explicit = flatten_path("M 0 0 C 0 10 10 10 10 0 C 10 -10 20 -10 20 0").expect("能解析");
        assert_eq!(smooth.len(), explicit.len());
        for (a, b) in smooth.iter().zip(explicit.iter()) {
            assert!(
                (a[0] - b[0]).abs() < 1e-4 && (a[1] - b[1]).abs() < 1e-4,
                "S 与显式 C 应当一致：{a:?} vs {b:?}"
            );
        }
        let smooth = flatten_path("M 0 0 Q 5 10 10 0 T 20 0").expect("能解析");
        let explicit = flatten_path("M 0 0 Q 5 10 10 0 Q 15 -10 20 0").expect("能解析");
        for (a, b) in smooth.iter().zip(explicit.iter()) {
            assert!(
                (a[0] - b[0]).abs() < 1e-4 && (a[1] - b[1]).abs() < 1e-4,
                "T 与显式 Q 应当一致：{a:?} vs {b:?}"
            );
        }
    }

    #[test]
    fn 数字不够或不是命令开头时报错() {
        assert!(flatten_path("M 0 0 L 10").is_err(), "少一个数应当报错");
        assert!(flatten_path("10 10").is_err(), "不是命令开头应当报错");
    }

    #[test]
    fn 数字可以贴在一起也可以用指数() {
        let points = flatten_path("M0 0L-1.5.5Z").expect("能解析");
        assert_eq!(points, vec![[0.0, 0.0], [-1.5, 0.5]]);
        let points = flatten_path("M 1e1 0 L 0 0").expect("能解析");
        assert_eq!(points[0], [10.0, 0.0]);
    }
}
