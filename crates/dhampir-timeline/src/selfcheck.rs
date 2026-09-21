//! 跨运行时等价性探针。
//!
//! # 它是什么
//!
//! 这个模块**不是测试**，是**证据生成器**。指导文档 §9.4 的验收第 3 条要求
//! "同一个纯逻辑函数在两端输出一致"——但"一致"不能靠嘴说，得有一个能被
//! 逐字节比对的东西。所以这里把一组固定输入在两个运行时上各算一遍，格式化成
//! ASCII 文本，再取一个 FNV-1a 摘要。
//!
//! - native 侧：`dhampir-render --probe` 写文件，`cargo test` 断言摘要
//! - wasm 侧：导出 `dhampir_probe_report()` / `dhampir_probe_digest()`，Node 或
//!   浏览器里调用
//!
//! 两个摘要相等 ⇒ "同一份源码两个运行时"在 M0 的范围内成立。
//!
//! # 为什么用摘要
//!
//! 逐行比对当然更强，但摘要有个逐行比对没有的性质：**它对格式变化敏感**。
//! 有人偷偷改了打印格式，摘要立刻变；而如果比对工具做了容错规范化，格式漂移
//! 就溜过去了。这里要的是"逐字节相同"，摘要最不容易被无意间放宽。
//!
//! # 输出必须是纯 ASCII
//!
//! 不引任何 locale、不做本地化、不用非 ASCII 字符。跨运行时比对的前提是两边
//! 的字节流可比，任何编码相关的变量都得先排除掉。

use core::fmt::Write as _;

use crate::timebase::Timebase;
use crate::timecode::{
    Timecode, exact_ticks_to_frame, frame_to_exact_ticks, frame_to_timecode, timecode_to_frame,
};

/// 报告格式版本。**改动输出格式必须同时改这个数**，否则新旧摘要会混在一起比较。
pub const PROBE_FORMAT_VERSION: u32 = 1;

/// 参与探针的时间基，顺序即输出顺序。含整数帧率与三种 NTSC 比值，
/// 覆盖 `den == 1` 与 `den == 1001` 两条代码路径。
pub const PROBE_TIMEBASES: [Timebase; 6] = [
    Timebase::FILM_24,
    Timebase::PAL_25,
    Timebase::WEB_30,
    Timebase::NTSC_FILM_2398,
    Timebase::NTSC_2997,
    Timebase::NTSC_5994,
];

/// 参与探针的帧号。刻意挑进位边界（29/30、1799/1800）与长时间值，
/// 因为 off-by-one 只会在边界上现形。
pub const PROBE_FRAMES: [i64; 10] = [0, 1, 23, 24, 29, 30, 1799, 1800, 108_000, 1_000_000];

/// 生成探针报告。
///
/// 输出是确定性的纯 ASCII 多行文本；同样的输入在任何运行时上都必须给出同样的字节。
pub fn probe_report() -> String {
    let mut out = String::with_capacity(2048);
    let _ = writeln!(out, "dhampir-probe v{PROBE_FORMAT_VERSION}");

    let mut cases = 0_usize;
    for tb in PROBE_TIMEBASES {
        for frame in PROBE_FRAMES {
            let tc = frame_to_timecode(frame, tb);
            let round_trip = timecode_to_frame(tc, tb);
            let rt = match round_trip {
                Some(f) => f.to_string(),
                None => "-".to_string(),
            };
            let _ = writeln!(
                out,
                "tb={tb} nominal={} f={frame} tc={tc} ticks={} rt={rt}",
                tb.nominal_fps(),
                frame_to_exact_ticks(frame, tb),
            );
            cases += 1;
        }
    }

    // 逆函数在网格外必须返回 None —— 这条也要跨运行时一致，否则"取整策略"
    // 可能在两端悄悄分叉。
    for (h, m, s, f) in [(0_u64, 0_u64, 0_u64, 30_u64), (0, 0, 60, 0), (0, 60, 0, 0)] {
        let tc = Timecode::new(h, m, s, f);
        let r = timecode_to_frame(tc, Timebase::NTSC_2997)
            .map_or_else(|| "-".to_string(), |v| v.to_string());
        let _ = writeln!(out, "tb=30000/1001 offgrid tc={tc} rt={r}");
        cases += 1;
    }

    // tick 逆变换的取整方向（div_euclid 的负数行为）也纳入。
    for ticks in [-1001_i64, -1, 0, 1, 1000, 1001, i64::from(i32::MAX)] {
        let _ = writeln!(
            out,
            "tb=30000/1001 ticks={ticks} f={}",
            exact_ticks_to_frame(ticks, Timebase::NTSC_2997)
        );
        cases += 1;
    }

    let _ = writeln!(out, "cases={cases}");
    out
}

/// 报告的 FNV-1a 64 位摘要。
///
/// 用 FNV-1a 而不是 `DefaultHasher`：后者在 Rust 里明确不保证跨版本稳定
/// （随机种子 + 实现可变），拿它做跨运行时比对是自找麻烦。FNV-1a 是纯整数、
/// 十行、跨版本跨平台行为固定——这里要的正是这个。
pub fn probe_digest() -> u64 {
    fnv1a64(probe_report().as_bytes())
}

/// FNV-1a 64。偏移基数与质数是该算法的固定常数。
pub const fn fnv1a64(bytes: &[u8]) -> u64 {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    let mut hash = OFFSET_BASIS;
    let mut i = 0;
    while i < bytes.len() {
        hash ^= bytes[i] as u64;
        hash = hash.wrapping_mul(PRIME);
        i += 1;
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_is_ascii_and_versioned() {
        let report = probe_report();
        assert!(
            report.is_ascii(),
            "探针报告必须是纯 ASCII，否则跨运行时字节比对不可信"
        );
        assert!(report.starts_with(&format!("dhampir-probe v{PROBE_FORMAT_VERSION}\n")));
        assert!(report.ends_with("\n"));
    }

    #[test]
    fn report_is_deterministic() {
        // 同一个进程里跑 100 遍必须一字不差。听着像废话，但如果以后有人往
        // 探针里加了 HashMap 迭代或时间戳，这条会第一个响。
        let first = probe_report();
        for _ in 0..100 {
            assert_eq!(probe_report(), first);
        }
        assert_eq!(probe_digest(), fnv1a64(first.as_bytes()));
    }

    #[test]
    fn case_count_matches() {
        let expected = PROBE_TIMEBASES.len() * PROBE_FRAMES.len() + 3 + 7;
        assert!(probe_report().ends_with(&format!("cases={expected}\n")));
    }

    #[test]
    fn fnv1a_matches_published_vectors() {
        // FNV-1a 64 的公开测试向量。这两条不通过说明实现写错了，
        // 那么摘要相等这件事本身就失去意义。
        assert_eq!(fnv1a64(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a64(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(fnv1a64(b"foobar"), 0x85944171f73967e8);
    }

    /// 这条测试是 M0 的**本地棘轮**：把期望摘要钉在代码里，
    /// 之后任何一次改动（升级工具链、改数学、改输出格式）都会让它变红，逼人解释原因。
    ///
    /// 跨运行时的那一半在 `dhampir-wasm` 侧：`crates/dhampir-timeline/tests/golden/`
    /// 里的报告文本被两端 `include_str!` 进来逐字节比对——摘要是本地棘轮，
    /// 逐字节比对才是"两端一致"的证据。
    #[test]
    fn probe_digest_is_pinned() {
        let actual = probe_digest();
        // 首次运行会打印实际值，把它填进来即可。填好之后下面那个分支就再也不会走；
        // 留着它，是为了万一有人把常量改回 0 时能立刻看到该怎么做。
        let pinned: u64 = 0xc3f0_da6b_3757_7e55;
        if pinned == 0 {
            panic!("首次运行：把 probe_digest() = 0x{actual:016x} 填进 pinned");
        }
        assert_eq!(
            actual, pinned,
            "探针摘要变了。是工具链/浮点/代码改动中的哪一个？"
        );
    }
}
