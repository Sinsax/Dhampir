//! CSS 缓动（关键字 / `cubic-bezier()` / `steps()`）的**唯一解析与求值实现**。
//!
//! # 为什么有它
//!
//! 作者层是 **WAAPI / CSS**，它给出的缓动值**就是这些字符串**
//! （`KeyframeEffect.getKeyframes()[i].easing` 与 `getTiming().easing`）。
//! 所以底座必须能直接吃 CSS 缓动语法，而不是让转译器去维护一张 CSS ↔ 本仓枚举 的映射表——
//! 那张表迟早在某一处对不上，而表现为预览看着对、成片慢半拍。
//!
//! 口径在 `plan/web-animation-criteria.md` 的 **D2**：**JSON 里就是 CSS 缓动字符串**，
//! 本模块负责把它解析成可求值的形式。
//!
//! # 一个必须记住的坑：`ease_in` 与 `ease-in` 是**两条不同的曲线**
//!
//! | 写法 | 含义 | 来历 |
//! |---|---|---|
//! | `ease_in` / `ease_out` / `ease_in_out` | 二次曲线（`t²` 一族） | 本仓既有取值，与 参照实现 逐值相同 |
//! | `ease-in` / `ease-out` / `ease-in-out` | CSS 关键字 = `cubic-bezier(...)` | WAAPI / CSS 给的值 |
//!
//! 下划线换成连字符就换了曲线 —— 所以两条都保留、都不近似，并有测试钉着它们不相等。
//! **不许**把 `ease-in` 归一化成 `ease_in`：那正是看着差不多 的那类错。
//!
//! # 只解析，不存储
//!
//! 真值仍然是**字符串**（D2）。这里只给出它算出来是什么，
//! 解析失败一律**明确报错**（调用方把它变成结构化 Issue `unknown_easing`），
//! **不静默降级成 linear** —— 静默降级正是这个项目最要避免的。
//!
//! # 待与浏览器对照的一项
//!
//! `steps()` 四个变体在**端点**（t=0 / t=1）附近的取值以 CSS 规范为准，
//! 这里的实现是闭式 + 夹到 [0,1]。它与浏览器原生值的逐点比对属于阶段 1-B 的判据
//! （`scripts/easing-reference.mjs` 产出的参考数据），本模块的测试只钉
//! **内部点**与单调性，不假装已经验过端点。

/// `steps()` 的跳跃位置。CSS 的 `start` / `end` 是前两者的别名。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepPosition {
    /// 在区间**起点**跳跃（CSS 的 `jump-start` / `start`）。
    JumpStart,
    /// 在区间**终点**跳跃（CSS 的 `jump-end` / `end`，也是缺省）。
    JumpEnd,
    /// 两端都不跳（要求 `count >= 2`）。
    JumpNone,
    /// 两端都跳。
    JumpBoth,
}

/// 解析出来的缓动形式。**它不是存储格式**（存储是字符串，见模块头）。
/// **不是 `Copy`**：`LinearStops` 带一个 `Vec`（第 37 轮加的 `linear()` 断点表）。
/// 这让 `apply` 从 `match *self` 改成借用匹配 —— 顺手也更省。
#[derive(Debug, Clone, PartialEq)]
pub enum EasingForm {
    Linear,
    /// 二次缓入（本仓既有的 `ease_in`）。
    QuadIn,
    /// 二次缓出（本仓既有的 `ease_out`）。
    QuadOut,
    /// 二次缓入缓出（本仓既有的 `ease_in_out`）。
    QuadInOut,
    /// 回弹过冲（本仓既有的 `back_out`，系数 1.70158）。
    BackOut,
    /// 三次贝塞尔：`P0=(0,0)`、`P3=(1,1)`，两个控制点由参数给出。
    CubicBezier {
        x1: f32,
        y1: f32,
        x2: f32,
        y2: f32,
    },
    Steps {
        count: u32,
        position: StepPosition,
    },
    /// CSS `linear()` 的**断点表**：一串 `(位置, 值)`，位置非递减。
    ///
    /// 值**允许在 [0,1] 之外**（CSS 里 `linear(0, 1.2 50%, 1)` 是合法的过冲写法），
    /// 所以这里不做值域校验 —— 只校验位置。
    LinearStops { stops: Vec<(f32, f32)> },
}

/// 解析失败的原因。**分类**而不是一句话：调用方要能按类别决定怎么处置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EasingError {
    /// 空串。
    Empty,
    /// 不认识的写法。
    Unknown(String),
    /// 认识但**这一版不做**（例如 `linear(...)` 的断点表）。
    Unsupported(String),
    /// 认得函数名但参数不合法（个数、范围、语法）。
    Malformed(String),
    /// 参数越界（当前只有 `cubic-bezier` 的 x 必须落在 [0,1]）。
    OutOfRange(String),
}

impl EasingError {
    /// 给 Issue 用的稳定短码（与 `unknown_effect` 同一套风格）。
    pub fn code(&self) -> &'static str {
        "unknown_easing"
    }

    /// 人话。写清楚**期望什么**，而不是只说不合法。
    pub fn message(&self) -> String {
        match self {
            Self::Empty => "缓动是空串；缺省应当是 linear".to_string(),
            Self::Unknown(text) => format!(
                "不认识的缓动 {text}；支持 linear、ease_in|ease_out|ease_in_out|back_out（本仓既有）、                 ease|ease-in|ease-out|ease-in-out|step-start|step-end（CSS 关键字）、                 cubic-bezier(x1,y1,x2,y2)、steps(n[, position])"
            ),
            Self::Unsupported(text) => format!(
                "缓动 {text} 这一版不支持；linear() 断点表在 plan/web-animation-criteria.md 的重审清单里"
            ),
            Self::Malformed(text) => format!("缓动 {text} 的参数不合法（个数或语法）"),
            Self::OutOfRange(text) => format!(
                "缓动 {text} 的参数越界：cubic-bezier 的两个 x 必须落在 [0,1]（y 可以越界，过冲就是靠它表达的）"
            ),
        }
    }
}

impl std::fmt::Display for EasingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message())
    }
}

/// CSS 关键字 `ease` 的系数（规范值）。
const CSS_EASE: (f32, f32, f32, f32) = (0.25, 0.1, 0.25, 1.0);
/// CSS 关键字 `ease-in` 的系数（规范值）。
const CSS_EASE_IN: (f32, f32, f32, f32) = (0.42, 0.0, 1.0, 1.0);
/// CSS 关键字 `ease-out` 的系数（规范值）。
const CSS_EASE_OUT: (f32, f32, f32, f32) = (0.0, 0.0, 0.58, 1.0);
/// CSS 关键字 `ease-in-out` 的系数（规范值）。
const CSS_EASE_IN_OUT: (f32, f32, f32, f32) = (0.42, 0.0, 0.58, 1.0);

/// 解析一个缓动串。
///
/// **大小写不敏感**（CSS 的缓动关键字不敏感），但**下划线与连字符不通用**（见模块头）。
pub fn parse(text: &str) -> Result<EasingForm, EasingError> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(EasingError::Empty);
    }
    let lower = trimmed.to_ascii_lowercase();

    // 快路径：关键字。命中即返回，不分配、不进解析器。
    match lower.as_str() {
        "linear" => return Ok(EasingForm::Linear),
        // 本仓既有（与 参照实现 逐值相同）—— 注意是**下划线**
        "ease_in" => return Ok(EasingForm::QuadIn),
        "ease_out" => return Ok(EasingForm::QuadOut),
        "ease_in_out" => return Ok(EasingForm::QuadInOut),
        "back_out" => return Ok(EasingForm::BackOut),
        // CSS 关键字 —— 注意是**连字符**
        "ease" => return Ok(cubic(CSS_EASE)),
        "ease-in" => return Ok(cubic(CSS_EASE_IN)),
        "ease-out" => return Ok(cubic(CSS_EASE_OUT)),
        "ease-in-out" => return Ok(cubic(CSS_EASE_IN_OUT)),
        "step-start" => {
            return Ok(EasingForm::Steps {
                count: 1,
                position: StepPosition::JumpStart,
            })
        }
        "step-end" => {
            return Ok(EasingForm::Steps {
                count: 1,
                position: StepPosition::JumpEnd,
            })
        }
        _ => {}
    }

    if let Some(args) = function_args(&lower, "cubic-bezier") {
        return parse_cubic_bezier(trimmed, args);
    }
    if let Some(args) = function_args(&lower, "steps") {
        return parse_steps(trimmed, args);
    }
    if let Some(args) = function_args(&lower, "linear") {
        return parse_linear(trimmed, args);
    }
    Err(EasingError::Unknown(trimmed.to_string()))
}

fn cubic(coefficients: (f32, f32, f32, f32)) -> EasingForm {
    let (x1, y1, x2, y2) = coefficients;
    EasingForm::CubicBezier { x1, y1, x2, y2 }
}

/// 取出 `name(...)` 里括号内那段。名字大小写不敏感（传进来的都已经是小写）。
fn function_args<'a>(lower: &'a str, name: &str) -> Option<&'a str> {
    let rest = lower.strip_prefix(name)?;
    let rest = rest.trim_start();
    let inner = rest.strip_prefix('(')?;
    let inner = inner.strip_suffix(')')?;
    Some(inner)
}

/// 按逗号切参数，并去掉空白。**不接受空参数**（`cubic-bezier(1,,0,0)` 是 malformed）。
fn split_args(inner: &str) -> Option<Vec<&str>> {
    let parts: Vec<&str> = inner.split(',').map(str::trim).collect();
    if parts.iter().any(|part| part.is_empty()) {
        return None;
    }
    Some(parts)
}

fn parse_cubic_bezier(original: &str, args: &str) -> Result<EasingForm, EasingError> {
    let parts = split_args(args).ok_or_else(|| EasingError::Malformed(original.to_string()))?;
    if parts.len() != 4 {
        return Err(EasingError::Malformed(original.to_string()));
    }
    let mut numbers = [0.0f32; 4];
    for (slot, text) in numbers.iter_mut().zip(parts) {
        *slot = text
            .parse::<f32>()
            .map_err(|_| EasingError::Malformed(original.to_string()))?;
        if !slot.is_finite() {
            return Err(EasingError::Malformed(original.to_string()));
        }
    }
    // 规范：两个 x 必须在 [0,1]；y 不限（过冲就是靠 y 越界表达的）。
    if !(0.0..=1.0).contains(&numbers[0]) || !(0.0..=1.0).contains(&numbers[2]) {
        return Err(EasingError::OutOfRange(original.to_string()));
    }
    Ok(cubic((numbers[0], numbers[1], numbers[2], numbers[3])))
}

fn parse_steps(original: &str, args: &str) -> Result<EasingForm, EasingError> {
    let parts = split_args(args).ok_or_else(|| EasingError::Malformed(original.to_string()))?;
    if parts.is_empty() || parts.len() > 2 {
        return Err(EasingError::Malformed(original.to_string()));
    }
    let count = parts[0]
        .parse::<u32>()
        .map_err(|_| EasingError::Malformed(original.to_string()))?;
    if count == 0 {
        return Err(EasingError::Malformed(original.to_string()));
    }
    let position = match parts.get(1).copied() {
        None | Some("jump-end") | Some("end") => StepPosition::JumpEnd,
        Some("jump-start") | Some("start") => StepPosition::JumpStart,
        Some("jump-none") => StepPosition::JumpNone,
        Some("jump-both") => StepPosition::JumpBoth,
        Some(_) => return Err(EasingError::Malformed(original.to_string())),
    };
    // 规范：jump-none 要求至少两段（否则两端都不跳就没有中间可站）。
    if position == StepPosition::JumpNone && count < 2 {
        return Err(EasingError::Malformed(original.to_string()));
    }
    Ok(EasingForm::Steps { count, position })
}

/// CSS `linear()` 的断点表。
///
/// # 位置怎么定（规范那三条）
///
/// 1. 只写值的：位置**均匀分布在相邻两个写明了位置的断点之间**；
/// 2. 第一个的缺省位置是 `0%`，最后一个是 `100%`；
/// 3. 位置必须**非递减**（相等允许 —— 那是"瞬跳"），否则报错。
///
/// 值允许在 `[0,1]` 之外（过冲），所以只校验位置。
fn parse_linear(original: &str, args: &str) -> Result<EasingForm, EasingError> {
    let parts = split_args(args).ok_or_else(|| EasingError::Malformed(original.to_string()))?;
    if parts.len() < 2 {
        return Err(EasingError::Malformed(original.to_string()));
    }
    // 先收成"值 + 可选位置"，再补全位置。
    let mut values: Vec<f32> = Vec::with_capacity(parts.len());
    let mut positions: Vec<Option<f32>> = Vec::with_capacity(parts.len());
    for part in &parts {
        let bits: Vec<&str> = part.split_whitespace().collect();
        if bits.is_empty() || bits.len() > 2 {
            return Err(EasingError::Malformed(original.to_string()));
        }
        let value = bits[0]
            .parse::<f32>()
            .map_err(|_| EasingError::Malformed(original.to_string()))?;
        if !value.is_finite() {
            return Err(EasingError::Malformed(original.to_string()));
        }
        let position = match bits.get(1) {
            None => None,
            Some(text) => {
                let percent = text
                    .strip_suffix('%')
                    .ok_or_else(|| EasingError::Malformed(original.to_string()))?;
                let parsed = percent
                    .parse::<f32>()
                    .map_err(|_| EasingError::Malformed(original.to_string()))?;
                if !parsed.is_finite() || !(0.0..=100.0).contains(&parsed) {
                    return Err(EasingError::OutOfRange(original.to_string()));
                }
                Some(parsed / 100.0)
            }
        };
        values.push(value);
        positions.push(position);
    }
    // 首末缺省，然后逐段均匀填中间的。
    let last = positions.len() - 1;
    if positions[0].is_none() {
        positions[0] = Some(0.0);
    }
    if positions[last].is_none() {
        positions[last] = Some(1.0);
    }
    let mut index = 0usize;
    while index <= last {
        if positions[index].is_some() {
            index += 1;
            continue;
        }
        // 找下一个写明了位置的断点，把中间这些均匀铺开。
        let mut next = index;
        while positions[next].is_none() {
            next += 1;
        }
        let start = positions[index - 1].unwrap_or(0.0);
        let end = positions[next].unwrap_or(1.0);
        let steps = (next - index + 1) as f32;
        for (offset, slot) in (index..next).enumerate() {
            positions[slot] = Some(start + (end - start) * (offset as f32 + 1.0) / steps);
        }
        index = next + 1;
    }
    let mut stops: Vec<(f32, f32)> = Vec::with_capacity(values.len());
    let mut previous = f32::NEG_INFINITY;
    for (value, position) in values.iter().zip(positions.iter()) {
        let position = position.unwrap_or(0.0);
        if position < previous {
            return Err(EasingError::Malformed(original.to_string()));
        }
        previous = position;
        stops.push((position, *value));
    }
    Ok(EasingForm::LinearStops { stops })
}

impl EasingForm {
    /// `[0,1]` 上的映射。输入会被夹到 `[0,1]`（与既有 `Easing::apply` 同一条纪律）。
    pub fn apply(&self, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        // **借用匹配**（不是 `match *self`）：断点表是 `Vec`，这个枚举因此不是 `Copy`。
        match self {
            Self::Linear => t,
            Self::QuadIn => t * t,
            Self::QuadOut => 1.0 - (1.0 - t) * (1.0 - t),
            Self::QuadInOut => {
                if t < 0.5 {
                    2.0 * t * t
                } else {
                    1.0 - 2.0 * (1.0 - t) * (1.0 - t)
                }
            }
            // back_out（经典系数 1.70158）：(t-1)^2 * ((s+1)*(t-1) + s) + 1
            Self::BackOut => {
                const S: f32 = 1.70158;
                let u = t - 1.0;
                u * u * ((S + 1.0) * u + S) + 1.0
            }
            Self::CubicBezier { x1, y1, x2, y2 } => cubic_bezier_at(t, *x1, *y1, *x2, *y2),
            Self::Steps { count, position } => steps_at(t, *count, *position),
            Self::LinearStops { stops } => linear_stops_at(stops, t),
        }
    }
}

/// 断点表在 `t` 处的值：找到所在那一段，线性插值。
///
/// 位置相等（瞬跳）的写法按规范取**后一个**的值 —— 那正是"瞬跳"该有的行为。
fn linear_stops_at(stops: &[(f32, f32)], t: f32) -> f32 {
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
            let ratio = (t - p0) / (p1 - p0);
            return v0 + (v1 - v0) * ratio;
        }
    }
    stops[stops.len() - 1].1
}

/// 三次贝塞尔在给定 x（= 进度）处的 y。
///
/// 标准做法：**先由 x 反解参数 u，再取 y(u)**。先用牛顿迭代（快），失败再二分（稳）
/// —— 只用牛顿会在 x 接近 0 或 1 时因导数趋零而发散。
fn cubic_bezier_at(t: f32, x1: f32, y1: f32, x2: f32, y2: f32) -> f32 {
    let cx = 3.0 * x1;
    let bx = 3.0 * (x2 - x1) - cx;
    let ax = 1.0 - cx - bx;
    let cy = 3.0 * y1;
    let by = 3.0 * (y2 - y1) - cy;
    let ay = 1.0 - cy - by;
    let u = solve_curve_x(ax, bx, cx, t);
    ((ay * u + by) * u + cy) * u
}

fn sample_x(ax: f32, bx: f32, cx: f32, u: f32) -> f32 {
    ((ax * u + bx) * u + cx) * u
}

fn sample_dx(ax: f32, bx: f32, cx: f32, u: f32) -> f32 {
    (3.0 * ax * u + 2.0 * bx) * u + cx
}

/// 由 x 反解 u。**迭代次数写死**：不靠收敛精度决定结果，两端才对得上。
fn solve_curve_x(ax: f32, bx: f32, cx: f32, x: f32) -> f32 {
    const EPSILON: f32 = 1e-6;
    // 牛顿迭代
    let mut u = x;
    for _ in 0..8 {
        let error = sample_x(ax, bx, cx, u) - x;
        if error.abs() < EPSILON {
            return u;
        }
        let derivative = sample_dx(ax, bx, cx, u);
        if derivative.abs() < 1e-6 {
            break;
        }
        u -= error / derivative;
    }
    // 二分兜底（只在牛顿发散时走到）
    let mut low = 0.0f32;
    let mut high = 1.0f32;
    let mut u = x;
    for _ in 0..24 {
        let value = sample_x(ax, bx, cx, u);
        if (value - x).abs() < EPSILON {
            return u;
        }
        if x > value {
            low = u;
        } else {
            high = u;
        }
        u = (high + low) * 0.5;
    }
    u
}

/// `steps()` 的闭式。
///
/// 四个变体的差别只在第几级 + 除以几：
/// - `jump-start`：`(floor(n·t) + 1) / n` —— 起点就跳一级，所以输出**取不到 0**
/// - `jump-end`：`floor(n·t) / n` —— 缺省
/// - `jump-none`：`floor(n·t) / (n-1)` —— 两端都不跳
/// - `jump-both`：`(floor(n·t) + 1) / (n+1)`
///
/// 结果夹到 `[0,1]`：端点那一格按 输入 1 输出 1 处理，与 `apply` 的端点约定一致。
/// **端点与浏览器的逐点一致性待 1-B 的参考数据确认**（模块头写了这一条）。
fn steps_at(t: f32, count: u32, position: StepPosition) -> f32 {
    let n = count as f32;
    let level = (t * n).floor();
    let value = match position {
        StepPosition::JumpStart => (level + 1.0) / n,
        StepPosition::JumpEnd => level / n,
        StepPosition::JumpNone => level / (n - 1.0),
        StepPosition::JumpBoth => (level + 1.0) / (n + 1.0),
    };
    value.clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linear_断点表按位置线性插值() {
        // `linear(0, 0.25 75%, 1)` ⇒ 断点 (0,0) (0.75,0.25) (1,1)。
        let form = parse("linear(0, 0.25 75%, 1)").expect("能解析");
        assert_eq!(form, EasingForm::LinearStops { stops: vec![(0.0, 0.0), (0.75, 0.25), (1.0, 1.0)] });
        assert!((form.apply(0.0) - 0.0).abs() < 1e-6);
        assert!((form.apply(0.75) - 0.25).abs() < 1e-6, "断点上的值应当精确");
        assert!((form.apply(1.0) - 1.0).abs() < 1e-6);
        // 0.5 落在第一段 (0,0)→(0.75,0.25)：0 + 0.25 × (0.5/0.75) = 1/6。
        assert!((form.apply(0.5) - 1.0 / 6.0).abs() < 1e-5, "得到 {}", form.apply(0.5));
    }

    #[test]
    fn linear_没写位置的那些均匀铺开() {
        let form = parse("linear(0, 0.5, 1)").expect("能解析");
        assert_eq!(form, EasingForm::LinearStops { stops: vec![(0.0, 0.0), (0.5, 0.5), (1.0, 1.0)] });
        assert!((form.apply(0.25) - 0.25).abs() < 1e-6);
        // 位置也可以只写中间那个：`linear(0, 1 25%, 0)` ⇒ (0,0) (0.25,1) (1,0)
        let spike = parse("linear(0, 1 25%, 0)").expect("能解析");
        assert!((spike.apply(0.25) - 1.0).abs() < 1e-6);
        assert!((spike.apply(0.625) - 0.5).abs() < 1e-6, "得到 {}", spike.apply(0.625));
    }

    #[test]
    fn linear_的值允许过冲_位置必须非递减() {
        // 值越过 1 是合法的 CSS 写法（过冲），不该被拦。
        let overshoot = parse("linear(0, 1.2 50%, 1)").expect("能解析");
        assert!((overshoot.apply(0.5) - 1.2).abs() < 1e-6);
        assert!(parse("linear(0, 1 50%, 1 10%)").is_err(), "位置递减应当报错");
        assert!(parse("linear(0, 1 150%)").is_err(), "位置越界应当报错");
        assert!(parse("linear(0)").is_err(), "单断点应当报错");
    }

    fn form(text: &str) -> EasingForm {
        parse(text).unwrap_or_else(|error| panic!("{text} 应当能解析：{error}"))
    }


/// 与**浏览器原生缓动**的逐值对照（阶段 1-B 的判据）。
///
/// 数据不是本仓算出来的：它由 `web/easing-probe.html` 在**真实浏览器**里采集，
/// 经 `scripts/easing-reference.mjs` 落到 `target/easing-reference.json`。
/// 采集必须在普通终端里跑 —— 本仓的 agent 会话起不了浏览器。
///
/// # 为什么是 `#[ignore]`
///
/// 这份数据要本机上的东西（浏览器），与仓库里那些 ignored 测试同一个理由。
/// **不在空文件集上通过**：读不到数据就**报错退出**，不假装对过了。
///
/// # 两个数组各是什么（别比错）
///
/// * `values` —— 由 `opacity` 采出来。**浏览器会把 opacity 夹到 [0,1]**，
///   所以它对应 `apply(t).clamp(0,1)`；
/// * `unclamped` —— 由 `translateX` 除以位移量采出来，**不被夹**，
///   所以它对应 `apply(t)` 本身。过冲（`back_out` / y 越界的贝塞尔）只能在这里看出来。
#[test]
#[ignore = "需要浏览器采到的 target/easing-reference.json（先跑 node scripts/easing-reference.mjs）"]
fn 与浏览器原生缓动逐值对得上() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("target")
        .join("easing-reference.json");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!(
            "读不到 {}：{error}\n先在普通终端里跑：node scripts/easing-reference.mjs",
            path.display()
        )
    });
    let data: serde_json::Value = serde_json::from_str(&text).expect("参考数据应当是 JSON 对象");
    let table = data
        .as_object()
        .unwrap_or_else(|| panic!("参考数据的顶层应当是对象（每个缓动串一项）"));

    // 只有"浏览器认了的串"才可比；认不得的（error 项）跳过但要数出来。
    let mut compared = 0usize;
    let mut refused = 0usize;
    let mut worst: (f32, String) = (0.0, String::new());
    for (easing, entry) in table {
        if entry.get("error").is_some() {
            refused += 1;
            println!("浏览器不认这个串，跳过：{easing}");
            continue;
        }
        let form = parse(easing).unwrap_or_else(|error| {
            panic!("浏览器认了 {easing}，本仓却不认：{error}（这正是转译会踩的坑）")
        });
        for key in ["values", "unclamped"] {
            let samples = entry
                .get(key)
                .and_then(|value| value.as_array())
                .unwrap_or_else(|| panic!("{easing} 的 {key} 应当是数组"));
            let count = samples.len();
            for (index, sample) in samples.iter().enumerate() {
                let Some(expected) = sample.as_f64() else {
                    continue; // 采集时是 null（浏览器没给出这个点）——不猜
                };
                let t = index as f32 / (count - 1) as f32;
                let mine = form.apply(t);
                let mine = if key == "values" { mine.clamp(0.0, 1.0) } else { mine };
                let delta = (mine - expected as f32).abs();
                // 浏览器给的是 getComputedStyle 的十进制文本，只到小数点后几位，
                // 所以容差按"读数的分辨率"取，不按浮点误差取。
                let tolerance = if key == "values" { 2e-3 } else { 5e-3 };
                if delta > worst.0 {
                    worst = (delta, format!("{easing} @ t={t:.2}（{key}）：浏览器 {expected}，本仓 {mine}"));
                }
                assert!(
                    delta <= tolerance,
                    "{easing} 在 t={t:.2} 对不上（{key}）：浏览器 {expected}，本仓 {mine}，差 {delta}",
                );
                compared += 1;
            }
        }
    }
    assert!(compared >= 100, "比对点太少（{compared}）——参考数据不完整，不许当它通过");
    println!("逐值对照通过：{compared} 个点，浏览器拒绝 {refused} 个串，最大偏差 {:.6}（{}）", worst.0, worst.1);
}

    #[test]
    fn 关键字各就各位() {
        assert_eq!(form("linear"), EasingForm::Linear);
        assert_eq!(form("ease_in"), EasingForm::QuadIn);
        assert_eq!(form("ease_out"), EasingForm::QuadOut);
        assert_eq!(form("ease_in_out"), EasingForm::QuadInOut);
        assert_eq!(form("back_out"), EasingForm::BackOut);
        assert_eq!(form("ease"), cubic(CSS_EASE));
        assert_eq!(form("ease-in"), cubic(CSS_EASE_IN));
        assert_eq!(form("ease-out"), cubic(CSS_EASE_OUT));
        assert_eq!(form("ease-in-out"), cubic(CSS_EASE_IN_OUT));
        assert_eq!(
            form("step-start"),
            EasingForm::Steps { count: 1, position: StepPosition::JumpStart }
        );
        assert_eq!(
            form("step-end"),
            EasingForm::Steps { count: 1, position: StepPosition::JumpEnd }
        );
    }

    #[test]
    fn 下划线与连字符是两条曲线_不许被归一化() {
        // 这是本模块最想钉住的一条：手滑把 _ 打成 - 就换了曲线。
        for (underscore, hyphen) in [
            ("ease_in", "ease-in"),
            ("ease_out", "ease-out"),
            ("ease_in_out", "ease-in-out"),
        ] {
            let a = form(underscore);
            let b = form(hyphen);
            assert_ne!(a, b, "{underscore} 与 {hyphen} 不能是同一个形式");
            let differs = [0.25f32, 0.5, 0.75]
                .iter()
                .any(|t| (a.apply(*t) - b.apply(*t)).abs() > 1e-3);
            assert!(differs, "{underscore} 与 {hyphen} 的取值也应当不同");
        }
    }

    #[test]
    fn 大小写不敏感() {
        assert_eq!(form("LINEAR"), EasingForm::Linear);
        assert_eq!(
            form("Cubic-Bezier(0.1, 0.2, 0.3, 0.4)"),
            form("cubic-bezier(0.1,0.2,0.3,0.4)")
        );
        assert_eq!(form(" Steps(4, Start) "), form("steps(4,start)"));
    }

    #[test]
    fn 恒等曲线与线性一致() {
        let identity = form("cubic-bezier(0,0,1,1)");
        for step in 0..=20 {
            let t = step as f32 / 20.0;
            let value = identity.apply(t);
            assert!((value - t).abs() < 1e-4, "cubic-bezier(0,0,1,1) 在 {t} 处应当等于 t，得到 {value}");
        }
    }

    #[test]
    fn css_ease_在中点的量级正确() {
        // 公开文献里 CSS ease 在 0.5 处约 0.8024。这里只钉**量级**（1e-2），
        // 精确的逐点比对归 1-B 的浏览器参考数据 —— 不假装本测试就是权威。
        let value = form("ease").apply(0.5);
        assert!((value - 0.8024).abs() < 1e-2, "ease 在 0.5 处得到 {value}");
    }

    #[test]
    fn 缓动的两个端点都归位() {
        // 只钉**端点**，不钉中间是否落在 [0,1] —— 过冲是合法且被需要的（见下一条）。
        // jump-start / jump-both / step-start 在 0 处**不取 0**（起点就跳一级），
        // 所以这里只要求输出落在 [0,1] 且 1 处归位；具体值由各自的专项用例钉。
        for text in [
            "linear",
            "ease",
            "ease-in",
            "ease-out",
            "ease-in-out",
            "ease_in",
            "ease_out",
            "ease_in_out",
            "back_out",
            "cubic-bezier(0.2,0.9,0.8,0.1)",
            "cubic-bezier(0.68,-0.55,0.265,1.55)",
            "steps(4)",
            "steps(4, jump-start)",
            "steps(4, jump-none)",
            "steps(4, jump-both)",
            "step-start",
            "step-end",
        ] {
            let easing = form(text);
            let start = easing.apply(0.0);
            let end = easing.apply(1.0);
            assert!((0.0..=1.0).contains(&start), "{text} 在 0 处的输出越界：{start}");
            assert!((end - 1.0).abs() < 1e-5, "{text} 在 1 处应当是 1，得到 {end}");
        }
    }

    #[test]
    fn 不过冲的缓动单调不减() {
        // y 的控制点落在 [0,1] 之内时曲线单调；这是单调与过冲的分界。
        for text in ["linear", "ease", "ease-in-out", "cubic-bezier(0.2,0.9,0.8,0.1)"] {
            let easing = form(text);
            let mut previous = f32::NEG_INFINITY;
            for step in 0..=100 {
                let value = easing.apply(step as f32 / 100.0);
                assert!(value >= previous - 1e-5, "{text} 在 {step} 处不应当下降");
                previous = value;
            }
        }
    }

    #[test]
    fn 过冲的缓动真的会冲过头() {
        // 与 schema 里那条既有用例同一个意思：过冲**必须保留**，
        // 不能因为输出应该落在 [0,1] 就把它夹掉 —— 夹掉就没有回弹了。
        for text in ["back_out", "cubic-bezier(0.68,-0.55,0.265,1.55)"] {
            let easing = form(text);
            let peak = (0..=100)
                .map(|step| easing.apply(step as f32 / 100.0))
                .fold(f32::NEG_INFINITY, f32::max);
            assert!(peak > 1.0 + 1e-3, "{text} 应当冲过 1，峰值只有 {peak}");
        }
    }

    #[test]
    fn steps_的内部点是闭式算出来的() {
        assert!((form("steps(4)").apply(0.5) - 0.5).abs() < 1e-6);
        assert!((form("steps(4, end)").apply(0.5) - 0.5).abs() < 1e-6);
        assert!((form("steps(4, jump-end)").apply(0.5) - 0.5).abs() < 1e-6);
        // jump-start 在起点就跳一级：0.0 处是 1/4
        assert!((form("steps(4, jump-start)").apply(0.0) - 0.25).abs() < 1e-6);
        assert!((form("steps(4, start)").apply(0.0) - 0.25).abs() < 1e-6);
        // jump-none 除以 n-1
        assert!((form("steps(4, jump-none)").apply(0.5) - 2.0 / 3.0).abs() < 1e-6);
        // jump-both 除以 n+1
        assert!((form("steps(4, jump-both)").apply(0.0) - 0.2).abs() < 1e-6);
        assert!((form("steps(4, jump-both)").apply(0.5) - 0.6).abs() < 1e-6);
    }

    #[test]
    fn 不过冲的缓动与所有_steps_的输出恒在零到一之间() {
        for text in [
            "linear",
            "ease",
            "ease-in-out",
            "steps(3, jump-both)",
            "steps(5, jump-none)",
            "steps(1, jump-start)",
        ] {
            let easing = form(text);
            for step in -5..=25 {
                let value = easing.apply(step as f32 / 20.0);
                assert!((0.0..=1.0).contains(&value), "{text} 在越界输入处给出了 {value}");
            }
        }
    }

    #[test]
    fn 越界输入被夹住() {
        let easing = form("cubic-bezier(0.2,0.8,0.4,1)");
        assert!((easing.apply(-5.0) - easing.apply(0.0)).abs() < 1e-6);
        assert!((easing.apply(5.0) - easing.apply(1.0)).abs() < 1e-6);
    }

    #[test]
    fn 报错分类明确() {
        assert_eq!(parse(""), Err(EasingError::Empty));
        assert_eq!(parse("   "), Err(EasingError::Empty));
        assert!(matches!(parse("bounce"), Err(EasingError::Unknown(_))));
        // `linear()` 第 37 轮起**是支持的** —— 这条旧断言正是那时过期的。
        // 现在 `Unsupported` 这个分类**暂时没有用户**（CSS 定义的缓动形式本仓都有了），
        // 但留着它：下一版 CSS 加新形式时，"认识但这一版不做"要能表达出来。
        assert!(parse("linear(0, 0.5, 1)").is_ok(), "linear() 现在应当能解析");
        // 真的画不出来的形式仍然归"不认识"。
        assert!(matches!(parse("wobble(1, 2)"), Err(EasingError::Unknown(_))));
        assert!(matches!(parse("cubic-bezier(1,0,0)"), Err(EasingError::Malformed(_))));
        assert!(matches!(parse("cubic-bezier(1,,0,0)"), Err(EasingError::Malformed(_))));
        assert!(matches!(parse("cubic-bezier(1.2,0,0,1)"), Err(EasingError::OutOfRange(_))));
        assert!(matches!(parse("steps(0)"), Err(EasingError::Malformed(_))));
        assert!(matches!(parse("steps(1, jump-none)"), Err(EasingError::Malformed(_))));
        assert!(matches!(parse("steps(4, sideways)"), Err(EasingError::Malformed(_))));
        assert!(matches!(parse("steps(4, end, extra)"), Err(EasingError::Malformed(_))));
        assert_eq!(parse("bounce").unwrap_err().code(), "unknown_easing");
    }
}
