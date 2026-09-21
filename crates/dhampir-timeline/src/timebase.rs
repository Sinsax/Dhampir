//! 有理数时间基：帧率 `fps = num / den`。
//!
//! 这是整个引擎的时间标尺。所有帧率相关的东西（时间码进位、音频对齐、
//! 分片边界）都从这里推导，不各自算各自的。

use core::fmt;

/// 帧率的有理数表示，`fps = num / den`。`den` 必须非零。
///
/// 常用值以常量给出（见 [`Timebase::FILM_24`] 等），但 `new` 接受任意合法比值——
/// 摄像机什么怪帧率都有，枚举是关不住的。
///
/// 字段公开且直接参与 serde：指导文档 §5.2 的时间线 JSON 就是
/// `"timebase": { "num": 30000, "den": 1001 }`，这里不做任何隐藏或重命名，
/// 免得契约层和内存表示各有一套。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Timebase {
    /// 分子。`30000/1001` 里的 `30000`。
    pub num: u32,
    /// 分母。必须非零。
    pub den: u32,
}

/// [`Timebase::try_new`] 的失败原因。
///
/// `dhampir-timeline` 刻意保持零依赖（指导文档「纯数据，零 GPU 依赖」），
/// 所以这里不引 thiserror，手写 `Display`。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimebaseError {
    /// `den == 0`：不是合法的帧率。
    ZeroDenominator,
    /// `num == 0`：0 fps 没有意义，且会让时间码换算除零。
    ZeroNumerator,
}

impl fmt::Display for TimebaseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroDenominator => f.write_str("时间基的分母为 0"),
            Self::ZeroNumerator => f.write_str("时间基的分子为 0"),
        }
    }
}

impl core::error::Error for TimebaseError {}

impl Timebase {
    /// 24 fps，电影。
    pub const FILM_24: Self = Self::new(24, 1);
    /// 25 fps，PAL / 大部分欧洲视频。
    pub const PAL_25: Self = Self::new(25, 1);
    /// 整数 30 fps，屏幕录制 / 网页视频常见。
    pub const WEB_30: Self = Self::new(30, 1);
    /// 整数 60 fps。
    pub const WEB_60: Self = Self::new(60, 1);
    /// 29.97 fps，NTSC。**不是** 29.97 这个小数，是 30000/1001。
    pub const NTSC_2997: Self = Self::new(30_000, 1001);
    /// 23.976 fps，电影转 NTSC。**不是** 23.976。
    pub const NTSC_FILM_2398: Self = Self::new(24_000, 1001);
    /// 59.94 fps。
    pub const NTSC_5994: Self = Self::new(60_000, 1001);

    /// 构造。`num` 与 `den` 都必须非零。
    ///
    /// 刻意**不做约分**：`30000/1001` 与 `60000/2002` 数值相等，但前者是契约里
    /// 要原样回写的那个值。要约分请显式调 [`Timebase::reduced`]。
    pub const fn new(num: u32, den: u32) -> Self {
        assert!(num != 0, "时间基的分子不能为 0");
        assert!(den != 0, "时间基的分母不能为 0");
        Self { num, den }
    }

    /// 可失败的构造，给外部输入（JSON、CLI 参数）用。
    pub const fn try_new(num: u32, den: u32) -> Result<Self, TimebaseError> {
        if num == 0 {
            return Err(TimebaseError::ZeroNumerator);
        }
        if den == 0 {
            return Err(TimebaseError::ZeroDenominator);
        }
        Ok(Self { num, den })
    }

    /// SMPTE 时间码进位用的**标称帧率**：`ceil(num/den)`。
    ///
    /// `30000/1001` → `30`。non-drop-frame 时间码就按它进位，所以 29.97 素材的
    /// 时间码会与墙钟缓慢漂移——这是 NDF 的定义，不是 bug。
    pub const fn nominal_fps(&self) -> u32 {
        // `div_ceil` 而不是 `(num + den - 1) / den`：后者在 `num` 接近 u32::MAX 时
        // 会先溢出，且溢出发生在**取整之前**——商看着还挺像样，错得很安静。
        self.num.div_ceil(self.den)
    }

    /// 一帧 = `den` 个 `1/num` 秒。**精确，无舍入。**
    ///
    /// 这就是 [`crate::timecode::frame_to_exact_ticks`] 里 tick 的单位来源：
    /// 第 `f` 帧的起点是 `f * den` 个 tick，一个 tick = `1/num` 秒。
    pub const fn ticks_per_frame(&self) -> u32 {
        self.den
    }

    /// 标称帧率下的数值（浮点），**只用于打印和粗略比较，禁止参与时间计算**。
    pub fn as_f64(&self) -> f64 {
        f64::from(self.num) / f64::from(self.den)
    }

    /// 约分到最简形式。已经最简时原样返回。
    pub const fn reduced(&self) -> Self {
        let g = gcd(self.num, self.den);
        Self {
            num: self.num / g,
            den: self.den / g,
        }
    }

    /// 是否已经是契约里那种"干净"写法（`den` 为 1，或 `num` 是千级整百数）。
    /// 供时间线校验用：`30000/1001` 合格，`2997/100` 会让人怀疑是手滑。
    pub const fn is_standard_form(&self) -> bool {
        self.den == 1 || (self.num.is_multiple_of(100) && self.den > 1)
    }
}

const fn gcd(mut a: u32, mut b: u32) -> u32 {
    while b != 0 {
        let t = b;
        b = a % b;
        a = t;
    }
    a
}

impl fmt::Display for Timebase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.num, self.den)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nominal_fps_rounds_up() {
        assert_eq!(Timebase::NTSC_2997.nominal_fps(), 30);
        assert_eq!(Timebase::NTSC_FILM_2398.nominal_fps(), 24);
        assert_eq!(Timebase::NTSC_5994.nominal_fps(), 60);
        assert_eq!(Timebase::FILM_24.nominal_fps(), 24);
        assert_eq!(Timebase::PAL_25.nominal_fps(), 25);
    }

    #[test]
    fn ntsc_is_not_a_float() {
        // 29.97 与 30000/1001 差得不多，但差得不是零。这条测试是防"顺手用 2997/100"。
        let proper = Timebase::NTSC_2997;
        let sloppy = Timebase::new(29_970, 1000);

        // **约分救不了它**：29970/1000 就是 2997/100，与 30000/1001 只差 0.0000132，
        // 但分母不是 1001 就说明这个数来自小数反算，不是来自契约。
        assert_ne!(proper.reduced(), sloppy.reduced());
        assert_eq!(sloppy.reduced(), Timebase::new(2997, 100));
        assert_ne!(proper, sloppy);

        // 契约层还要求写法"干净"：30000/1001 合格，2997/100 一看就是手滑。
        assert!(proper.is_standard_form());
        assert!(!sloppy.is_standard_form());
    }

    #[test]
    fn ticks_per_frame_is_exact() {
        // 29.97 一帧 = 1001 个 1/30000 秒。整数，没有 0.0333666…
        assert_eq!(Timebase::NTSC_2997.ticks_per_frame(), 1001);
        assert_eq!(Timebase::FILM_24.ticks_per_frame(), 1);
    }

    #[test]
    fn try_new_rejects_zero() {
        assert_eq!(Timebase::try_new(0, 1), Err(TimebaseError::ZeroNumerator));
        assert_eq!(Timebase::try_new(1, 0), Err(TimebaseError::ZeroDenominator));
        assert_eq!(Timebase::try_new(30_000, 1001), Ok(Timebase::NTSC_2997));
    }
}
