//! 跨运行时等价性探针的宿主侧包装。**目标无关**。
//!
//! 这个模块存在的意义比它的代码量重要得多：它在 native 与 wasm32 上编译的是
//! **同一份字节**。`cargo check --workspace`（native）和
//! `cargo check -p dhampir-wasm --target wasm32-unknown-unknown` 都通过，
//! 就已经说明"探针逻辑没有平台分叉"；剩下的只是把摘要值比一比。
//!
//! 真正的数学在 [`dhampir_core::timeline::selfcheck`]，这里不复制任何逻辑——
//! 一旦这里出现"wasm 版本的重实现"，整个验收就变成自证了。

pub use dhampir_core::timeline::{
    PROBE_FORMAT_VERSION, SELFCHECK_REPORT_V1, fnv1a64, golden_matches_current_report,
    probe_digest, probe_report,
};

/// 与仓库里钉住的 golden 逐字节比对，空字符串表示一致。
///
/// 浏览器页面上那个 ✓ 就是调它得到的。它比"端到端的摘要比对"更强的一点在于：
/// 期望值**编在模块里**，所以页面在没有网络、没有服务端的情况下也能自证。
pub fn golden_check() -> Result<(), String> {
    golden_matches_current_report()
}

/// golden 报告的 FNV-1a 摘要，十六进制。
///
/// 存在的意义：页面把 golden 摘要与 native 侧 `run.json` 里记的值比一比，
/// 就能发现"我本地这份 wasm 是用旧 golden 编出来的"。
pub fn golden_digest_hex() -> String {
    format!("{:016x}", fnv1a64(SELFCHECK_REPORT_V1.as_bytes()))
}

/// 探针摘要的十六进制短写。
///
/// 之所以要有十六进制形式：`u64` 在 JS 里会掉精度（`Number` 只有 53 位尾数），
/// 直接传数字给页面会得到"看起来相等其实不等"的假绿灯。传字符串。
pub fn probe_digest_hex() -> String {
    format!("{:016x}", probe_digest())
}

/// 一次探针运行的完整结果，供页面展示与比对。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProbeSummary {
    pub format_version: u32,
    pub digest_hex: String,
    pub lines: usize,
    pub bytes: usize,
    pub report: String,
}

impl ProbeSummary {
    /// 采集一次。
    pub fn capture() -> Self {
        let report = probe_report();
        Self {
            format_version: PROBE_FORMAT_VERSION,
            digest_hex: probe_digest_hex(),
            lines: report.lines().count(),
            bytes: report.len(),
            report,
        }
    }

    /// 与另一端给出的摘要比对。`expected` 通常来自 native 侧写出的
    /// `selfcheck-native.txt` / `run.json`。
    ///
    /// 返回 `Err(说明)` 而不是 `bool`：跨运行时不一致的时候，你需要的是
    /// "哪一段对不上"，不是一个 `false`。
    pub fn verify_against(&self, expected_digest_hex: &str) -> Result<(), String> {
        let expected = expected_digest_hex.trim().to_ascii_lowercase();
        if expected.len() != 16 {
            return Err(format!(
                "期望摘要长度应为 16 个十六进制字符，收到 {expected:?}"
            ));
        }
        if self.digest_hex == expected {
            return Ok(());
        }
        Err(format!(
            "跨运行时摘要不一致：native={expected} wasm={}。\
             这**不是**浮点误差——探针全程整数运算，不一致只有两种可能：\
             两边跑的不是同一份代码，或者报告格式被改过而 PROBE_FORMAT_VERSION 没跟着改。",
            self.digest_hex
        ))
    }

    /// 与编译进本模块的 golden 逐字节比对。
    ///
    /// 与 [`ProbeSummary::verify_against`] 的区别：那个比的是"另一端的摘要"，
    /// 这个比的是"仓库里钉住的字节"。后者不需要另一端先跑过一遍，
    /// 因此在 CI 里、在浏览器里都能独立成立。
    pub fn verify_golden(&self) -> Result<(), String> {
        dhampir_core::timeline::golden_verdict(&self.report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_is_stable_within_a_run() {
        let a = ProbeSummary::capture();
        let b = ProbeSummary::capture();
        assert_eq!(a, b);
        assert_eq!(a.digest_hex.len(), 16);
        assert!(a.bytes > 0);
        assert!(a.lines > 0);
    }

    #[test]
    fn verify_reports_which_side_differs() {
        let s = ProbeSummary::capture();
        assert!(s.verify_against(&s.digest_hex.to_ascii_uppercase()).is_ok());
        let err = s.verify_against("0000000000000000").unwrap_err();
        assert!(
            err.contains("native=0000000000000000"),
            "错误信息要带上两边的值：{err}"
        );
        assert!(s.verify_against("abc").is_err());
    }

    /// golden 是**编进本模块**的，所以 native 与 wasm32 两份构建都能独立自证，
    /// 不需要"另一端先跑一遍"。
    #[test]
    fn golden_check_passes_on_this_build() {
        if let Err(why) = golden_check() {
            panic!("{why}");
        }
        assert_eq!(golden_digest_hex().len(), 16);
        assert_ne!(
            golden_digest_hex(),
            "0000000000000000",
            "全零只可能是读到了空报告"
        );
    }

    /// `verify_golden` 比的是**capture 到的那份**报告，而不是重算一遍。
    /// 把报告改一个字就该红——否则它就是"永远绿"的那类守卫。
    #[test]
    fn verify_golden_looks_at_the_captured_report() {
        let mut s = ProbeSummary::capture();
        s.verify_golden().unwrap();
        s.report.push_str("tb=99/1 f=0 tc=99:99:99:99\n");
        assert!(
            s.verify_golden().is_err(),
            "报告被改过却仍然通过，说明比的根本不是它"
        );
    }

    /// 一个字的差异也要能被指出来，而且要指向正确的行。
    #[test]
    fn verify_golden_points_at_the_line_that_moved() {
        let mut s = ProbeSummary::capture();
        let mut lines: Vec<String> = s.report.split('\n').map(str::to_string).collect();
        let mid = lines.len() / 2;
        let original = lines[mid].clone();
        lines[mid].push('Z');
        s.report = lines.join("\n");

        let err = s.verify_golden().unwrap_err();
        let expected_line = mid + 1;
        assert!(
            err.contains(&format!("第 {expected_line} 行")),
            "要指向第 {expected_line} 行：{err}"
        );
        assert!(err.contains(&original), "要带上原始那一行：{err}");
    }
}
