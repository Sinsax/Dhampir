//! 帧号 ↔ 时间码。**纯整数运算，全程不碰浮点。**
//!
//! 为什么强调这一点：这个模块是 M0 的跨运行时等价性探针本体。探针要回答的是
//! "同一份源码在 native 与 wasm 上是否给出同一结果"，如果函数内部用了 `f64`，
//! 那么两端不一致时你分不清是运行时差异还是浮点差异——探针就白做了。
//!
//! ## M0 只做 non-drop-frame
//!
//! NDF 时间码按 [`Timebase::nominal_fps`] 进位：`30000/1001` 素材按 30 进位，
//! 于是时间码会与墙钟缓慢漂移（NDF 的定义如此）。drop-frame 的 `;` 记法只影响
//! **显示与解析**，不影响引擎内部表示——内部一律是帧号。所以 DF 留到 M4 做时间码
//! 显示时再说，不进 M0。

use crate::timebase::Timebase;

/// SMPTE non-drop-frame 时间码 `HH:MM:SS:FF`。
///
/// 四个字段都是 `u64`，不是 `u32`：`frame` 的入参类型是 `i64`，用 `u32` 会让
/// 极端输入下的行为依赖溢出语义（debug panic / release 回绕），而 debug 与
/// release 的差异恰恰会让"双运行时一致"这条验收变得不可信。`u64` 覆盖 `i64`
/// 全部输入且不会溢出，函数因此是**全函数、无 panic、无分支差异**。
///
/// `hours` 用两位补零打印；超过 99 小时会自然变宽（`100:00:00:00`），不截断。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Timecode {
    pub hours: u64,
    pub minutes: u64,
    pub seconds: u64,
    pub frames: u64,
}

impl Timecode {
    /// 全零时间码 `00:00:00:00`。剪辑起点。
    pub const ZERO: Self = Self {
        hours: 0,
        minutes: 0,
        seconds: 0,
        frames: 0,
    };

    pub const fn new(hours: u64, minutes: u64, seconds: u64, frames: u64) -> Self {
        Self {
            hours,
            minutes,
            seconds,
            frames,
        }
    }
}

impl core::fmt::Display for Timecode {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "{:02}:{:02}:{:02}:{:02}",
            self.hours, self.minutes, self.seconds, self.frames
        )
    }
}

/// 帧号 → SMPTE non-drop-frame 时间码。
///
/// 全程整数：`nominal = ceil(num/den)`，然后反复整除取余。
///
/// **负数帧**：按绝对值换算（`-25` 在 25fps 下得到 `00:00:01:00`）。时间线的
/// 段落起点一律 ≥ 0，负帧不该出现；但这里也不 panic——一个"哪都能调、不返回
/// `Result`、不 panic"的换算函数比一个会在生产里炸的函数有用。真要区分正负请
/// 在调用点处理，别指望时间码字符串带符号。
///
/// 这是指导文档 §9.4 验收第 3 条指定的那个"纯逻辑函数"。
pub fn frame_to_timecode(frame: i64, tb: Timebase) -> Timecode {
    let nominal = u64::from(tb.nominal_fps());
    let f = frame.unsigned_abs();

    let total_seconds = f / nominal;
    let frames = f % nominal;

    Timecode {
        hours: total_seconds / 3600,
        minutes: (total_seconds / 60) % 60,
        seconds: total_seconds % 60,
        frames,
    }
}

/// 时间码 → 帧号。**只对 NDF 网格上存在的时刻有定义**。
///
/// 返回 `None` 的条件（都是"这个时间码不代表任何一帧"，而不是出错）：
/// - `frames >= nominal_fps`：例如 30fps 下的 `00:00:00:45`
/// - `minutes >= 60` 或 `seconds >= 60`：`00:00:75:00` 这类越界写法
/// - 算出来的帧号超出 `i64`：只有病态输入才会碰到
///
/// 与 [`frame_to_timecode`] 构成往返：对任意 `frame >= 0`，
/// `timecode_to_frame(frame_to_timecode(frame, tb), tb) == Some(frame)`。
pub fn timecode_to_frame(tc: Timecode, tb: Timebase) -> Option<i64> {
    let nominal = u64::from(tb.nominal_fps());
    if tc.frames >= nominal || tc.seconds >= 60 || tc.minutes >= 60 {
        return None;
    }
    let total_seconds = tc
        .hours
        .checked_mul(3600)?
        .checked_add(tc.minutes.checked_mul(60)?)?
        .checked_add(tc.seconds)?;
    total_seconds
        .checked_mul(nominal)?
        .checked_add(tc.frames)?
        .try_into()
        .ok()
}

/// 帧号 → **精确** tick 数。一个 tick = `1 / tb.num` 秒，所以第 `f` 帧的起点是
/// `f * den` 个 tick，误差为零。
///
/// 时间码是给人看的，tick 是给机器算的：音频对齐、片段拼接点比较、分片边界
/// 推导都用这个。它同时是"帧号是整数"这条铁律的数学落点——换算全程只有一次
/// 乘法，没有除法，也就没有舍入。
pub fn frame_to_exact_ticks(frame: i64, tb: Timebase) -> i64 {
    frame.saturating_mul(i64::from(tb.ticks_per_frame()))
}

/// [`frame_to_exact_ticks`] 的逆，**向下取整**：落在第 `f` 帧中间的 tick 归给 `f`。
///
/// 剪辑里的"吸附到帧"就是它。
pub fn exact_ticks_to_frame(ticks: i64, tb: Timebase) -> i64 {
    ticks.div_euclid(i64::from(tb.ticks_per_frame()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 29.97 的 NDF 进位：按 30 数。这些值是可以手算复核的。
    #[test]
    fn ndf_2997_known_values() {
        let tb = Timebase::NTSC_2997;
        let cases = [
            (0_i64, (0, 0, 0, 0)),
            (1, (0, 0, 0, 1)),
            (29, (0, 0, 0, 29)),
            (30, (0, 0, 1, 0)),
            (1799, (0, 0, 59, 29)),
            (1800, (0, 1, 0, 0)),
            // 30000 帧 ÷ 30 = 1000 秒 = 16 分 40 秒
            (30_000, (0, 16, 40, 0)),
            // 108000 帧 ÷ 30 = 3600 秒 = 1 小时
            (108_000, (1, 0, 0, 0)),
        ];
        for (frame, (h, m, s, f)) in cases {
            assert_eq!(
                frame_to_timecode(frame, tb),
                Timecode::new(h, m, s, f),
                "frame {frame}"
            );
        }
    }

    #[test]
    fn ndf_25_and_24_known_values() {
        let pal = Timebase::PAL_25;
        assert_eq!(frame_to_timecode(24, pal), Timecode::new(0, 0, 0, 24));
        assert_eq!(frame_to_timecode(25, pal), Timecode::new(0, 0, 1, 0));
        assert_eq!(frame_to_timecode(90_000, pal), Timecode::new(1, 0, 0, 0));

        let film = Timebase::FILM_24;
        assert_eq!(frame_to_timecode(86_400, film), Timecode::new(1, 0, 0, 0));
        // 一天整：24:00:00:00，不截断到 99 小时
        assert_eq!(
            frame_to_timecode(2_073_600, film),
            Timecode::new(24, 0, 0, 0)
        );
    }

    #[test]
    fn round_trip_over_a_sweep() {
        for tb in [
            Timebase::FILM_24,
            Timebase::PAL_25,
            Timebase::NTSC_2997,
            Timebase::NTSC_FILM_2398,
            Timebase::NTSC_5994,
        ] {
            for frame in [0_i64, 1, 23, 24, 1_000, 8_192, 99_999, 1_000_000] {
                let tc = frame_to_timecode(frame, tb);
                assert_eq!(
                    timecode_to_frame(tc, tb),
                    Some(frame),
                    "tb={tb} frame={frame} tc={tc}"
                );
            }
        }
    }

    #[test]
    fn timecode_to_frame_rejects_off_grid() {
        let tb = Timebase::NTSC_2997; // 标称 30
        assert_eq!(timecode_to_frame(Timecode::new(0, 0, 0, 30), tb), None);
        assert_eq!(timecode_to_frame(Timecode::new(0, 0, 0, 45), tb), None);
        assert_eq!(timecode_to_frame(Timecode::new(0, 0, 60, 0), tb), None);
        assert_eq!(timecode_to_frame(Timecode::new(0, 60, 0, 0), tb), None);
        assert_eq!(timecode_to_frame(Timecode::new(0, 0, 0, 29), tb), Some(29));
    }

    #[test]
    fn negative_frames_use_absolute_value_and_never_panic() {
        let tb = Timebase::PAL_25;
        assert_eq!(frame_to_timecode(-25, tb), Timecode::new(0, 0, 1, 0));
        // i64::MIN 也不能炸——unsigned_abs 在这里是唯一安全的写法
        let _ = frame_to_timecode(i64::MIN, tb);
        let _ = frame_to_timecode(i64::MAX, tb);
    }

    #[test]
    fn exact_ticks_are_lossless() {
        let tb = Timebase::NTSC_2997; // 一帧 = 1001 tick
        assert_eq!(frame_to_exact_ticks(1, tb), 1001);
        assert_eq!(frame_to_exact_ticks(30_000, tb), 30_030_000);
        // 30_000 帧 × 1001 tick ÷ 30000 = 1001 秒 —— 这正是"精确"的含义：
        // 用浮点算 30000 × (1001/30000) 会得到 1000.9999999999999 之类的东西。
        assert_eq!(frame_to_exact_ticks(30_000, tb) / i64::from(tb.num), 1001);

        assert_eq!(exact_ticks_to_frame(0, tb), 0);
        assert_eq!(exact_ticks_to_frame(1000, tb), 0); // 落在第 0 帧中间
        assert_eq!(exact_ticks_to_frame(1001, tb), 1);
        assert_eq!(exact_ticks_to_frame(-1, tb), -1); // div_euclid：向下取整
    }
}
