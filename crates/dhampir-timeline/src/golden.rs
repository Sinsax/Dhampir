//! 报告 golden：**"探针报告应该长什么样"的唯一真相源**。
//!
//! # 为什么需要一个 golden 文件
//!
//! M0 验收第 3 条要求"同一个纯逻辑函数在 native 与 wasm 上输出一致"。
//! 摘要（见 [`crate::selfcheck::probe_digest`]）能告诉你"不一致了"，
//! 但不能告诉你"哪一行不一致"；而跨运行时排错时，你最先需要的正是后者。
//!
//! 所以这里放一份**逐字节的期望报告**。它由 `dhampir-render --probe-only`
//! 生成，两端 `include_str!` 同一份文件、调用**同一个**比对函数
//! （[`golden_verdict`]）——连"怎么算不一致"的逻辑都不许各写一份。
//!
//! # 为什么 golden 放在 `tests/golden/` 里却用 `include_str!` 编进库
//!
//! 因为浏览器那一端也需要它。把期望值编进 wasm 模块，页面就能在没有网络、
//! 没有服务端的情况下自证："我算出的这份报告，与仓库里钉住的那份逐字节相同"。
//! 截图里那个 ✓ 于是是一个**真结论**，而不是脚本打印的一句好话。
//!
//! # 重新生成
//!
//! ```text
//! cargo run -p dhampir-worker --bin dhampir-render -- --probe-only --out records/m0
//! copy records/m0/selfcheck-native.txt crates/dhampir-timeline/tests/golden/selfcheck-report-v1.txt
//! ```
//!
//! 改完 golden 必须同时改 [`crate::selfcheck::PROBE_FORMAT_VERSION`]，
//! 否则新旧两种格式会顶着同一个版本号被比较。
//!
//! **注意**：重新生成时不要用 PowerShell 的 `Set-Content` / `Out-File`——
//! 它们会写 BOM 或 CRLF，而本文件的存在意义就是逐字节相等。
//! `scripts/check-text-hygiene.mjs` 会替你守住这一条。

use core::fmt::Write as _;

use crate::selfcheck::{PROBE_FORMAT_VERSION, fnv1a64, probe_report};

/// 期望的探针报告全文。生成方式见本模块文档。
///
/// 文件名里的 `v1` 与 [`PROBE_FORMAT_VERSION`] 对应；格式改动时两者一起动。
pub const SELFCHECK_REPORT_V1: &str = include_str!("../tests/golden/selfcheck-report-v1.txt");

/// 探针报告在 golden 里应有的首行前缀。
const HEADER_PREFIX: &str = "dhampir-probe v";

/// 检查 golden 文件本身是否可用（而不是检查报告）。
///
/// 这条检查是"守卫的守卫"：如果 golden 自己被某个编辑器改成了 CRLF 或加了 BOM，
/// 那么此后**每一次**比对都会失败，而且失败信息会指向探针代码——把排查引到
/// 完全错误的方向。所以先把 golden 自己的毛病喊出来。
pub fn golden_self_check() -> Result<(), String> {
    let expected = SELFCHECK_REPORT_V1;

    if expected.is_empty() {
        return Err("golden 报告是空的——文件还在吗？".into());
    }
    if expected.starts_with('\u{feff}') {
        return Err("golden 报告开头有 UTF-8 BOM。它必须是纯 ASCII，否则逐字节比对无意义。".into());
    }
    if expected.contains('\r') {
        return Err(
            "golden 报告里有 CR（0x0D）——多半是被 Windows 编辑器或 Set-Content 改成了 CRLF。"
                .into(),
        );
    }
    if !expected.ends_with('\n') {
        return Err("golden 报告没有以换行结尾——生成时被截断过？".into());
    }
    if !expected.is_ascii() {
        return Err("golden 报告里有非 ASCII 字节——跨运行时比对要求纯 ASCII。".into());
    }
    let want_header = format!("{HEADER_PREFIX}{PROBE_FORMAT_VERSION}\n");
    if !expected.starts_with(&want_header) {
        let got = expected.lines().next().unwrap_or("<空>");
        return Err(format!(
            "golden 报告的首行是 {got:?}，而当前 PROBE_FORMAT_VERSION={PROBE_FORMAT_VERSION} \
             要求它以 {want_header:?} 开头。格式改过而版本号没改，或者版本号改了而 golden 没重新生成。"
        ));
    }
    Ok(())
}

/// 把实际报告与 golden 逐字节比对。
///
/// `Ok(())` 表示两端一致；`Err(说明)` 带上**第一处差异的行号与两侧原文**，
/// 以及两侧的 FNV-1a 摘要。跨运行时不一致时，你需要的是这些，不是一个 `false`。
pub fn golden_verdict(actual: &str) -> Result<(), String> {
    // 先查 golden 自己。它不是探针产出的一部分，坏了必须明确指出来。
    golden_self_check()?;

    let expected = SELFCHECK_REPORT_V1;
    if actual == expected {
        return Ok(());
    }

    let mut message = String::new();
    let _ = write!(
        message,
        "探针报告与 golden 不一致：expected {} 字节 / actual {} 字节",
        expected.len(),
        actual.len()
    );

    // 第一处差异。按行比而不是按字节比，因为"第 37 行"比"偏移 1234"好找太多了。
    //
    // 注意 `split('\n')` 之后，CRLF 的 `\r` 会粘在每一行的末尾。所以不能直接
    // 拿整行去比——那样 CRLF 会被报成"第 1 行内容不同"，把排查引到"数据错了"
    // 而不是"行尾错了"。这是最容易发生、也最不像 bug 的那种差异：内容一字不差。
    let expected_lines: Vec<&str> = expected.split('\n').collect();
    let actual_lines: Vec<&str> = actual.split('\n').collect();
    let lines = expected_lines.len().max(actual_lines.len());
    let mut reported = false;
    let mut cr_only = 0_usize;
    for index in 0..lines {
        let want = expected_lines
            .get(index)
            .copied()
            .unwrap_or("<这一行在 actual 里不存在>");
        let got = actual_lines
            .get(index)
            .copied()
            .unwrap_or("<这一行在 expected 里不存在>");
        if want == got {
            continue;
        }
        if want.trim_end_matches('\r') == got.trim_end_matches('\r') {
            // 只差行尾。记下来，继续找真正的内容差异——如果全是这种，下面统一说。
            cr_only += 1;
            continue;
        }
        let _ = write!(
            message,
            "；第 {} 行 expected={want:?} actual={got:?}",
            index + 1
        );
        reported = true;
        break;
    }
    if !reported {
        if cr_only > 0 {
            let _ = write!(
                message,
                "；所有行的内容都相同，只有行尾不同：有 {cr_only} 行以 CR 结尾（CRLF）。\
                 golden 与产出都必须是纯 LF——多半是某个编辑器或 Set-Content 动过文件"
            );
        } else {
            let _ = write!(
                message,
                "；每一行都相同但整体字节不同，检查行尾与首尾多余字节"
            );
        }
    }

    let _ = write!(
        message,
        "；摘要 expected={:016x} actual={:016x}",
        fnv1a64(expected.as_bytes()),
        fnv1a64(actual.as_bytes())
    );
    Err(message)
}

/// 当前构建自己算出的报告与 golden 是否一致。
///
/// 宿主的自检入口都调它——CLI、wasm 导出、测试，走的都是这一条路径。
pub fn golden_matches_current_report() -> Result<(), String> {
    golden_verdict(&probe_report())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn golden_is_wellformed() {
        golden_self_check().unwrap();
    }

    /// M0 验收第 3 条的 native 半边。wasm 侧调的是同一个函数。
    #[test]
    fn current_report_matches_golden() {
        golden_matches_current_report().unwrap();
    }

    #[test]
    fn verdict_names_the_first_differing_line() {
        // 把第 5 行改掉一个字符。行号是 1 起的，与编辑器一致。
        let mut mutated: Vec<String> = SELFCHECK_REPORT_V1
            .split('\n')
            .map(str::to_string)
            .collect();
        let target = mutated.get_mut(4).expect("golden 至少要有 5 行");
        let victim = target.find(' ').expect("第 5 行里应当有空格");
        target.replace_range(victim..victim + 1, "X");

        let err = golden_verdict(&mutated.join("\n")).unwrap_err();
        assert!(err.contains("第 5 行"), "错误信息要指出行号：{err}");
        assert!(err.contains("expected="), "错误信息要带上两侧原文：{err}");
        assert!(err.contains("摘要"), "错误信息要带上两侧摘要：{err}");
    }

    #[test]
    fn verdict_notices_truncation() {
        let truncated = SELFCHECK_REPORT_V1.rsplit_once('\n').unwrap().0;
        let err = golden_verdict(truncated).unwrap_err();
        assert!(err.contains("expected"), "{err}");
    }

    #[test]
    fn verdict_notices_a_lone_carriage_return() {
        // 只差行尾：内容一字不差，但整体字节不同。这正是 Windows 上最容易发生的事。
        let crlf = SELFCHECK_REPORT_V1.replace('\n', "\r\n");
        let err = golden_verdict(&crlf).unwrap_err();
        assert!(err.starts_with("探针报告与 golden 不一致"), "{err}");
        assert!(
            err.contains("CR"),
            "要说清是行尾问题，别让人去逐行数：{err}"
        );
    }

    #[test]
    fn golden_self_check_rejects_a_broken_golden() {
        // golden_self_check 读的是编译进来的常量，这里只能验"它当前是好"，
        // 以及验那些判断本身的依据：报告确实是 ASCII、确实以换行结尾。
        let expected = SELFCHECK_REPORT_V1;
        assert!(expected.is_ascii());
        assert!(expected.ends_with('\n'));
        assert!(!expected.contains('\r'));
    }
}
