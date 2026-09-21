//! M0 验收第 3 条的 **native 半边**——而且刻意只走公开 API。
//!
//! 为什么单独写一个集成测试，而不是都塞进 `src/golden.rs` 的单元测试里：
//! 集成测试只能看见 crate 的公开接口，也就是 `dhampir-wasm` 能看见的那些。
//! 于是这个文件与 wasm 侧的测试**结构上逐句对应**，差异只剩"跑在哪个运行时上"。
//! 如果某天有人在 core 里加了个 `pub(crate)` 的捷径让 native 走通了，这里会先红。
//!
//! 数据文件在 `tests/golden/selfcheck-report-v1.txt`，由
//! `dhampir-render --probe-only` 生成。生成方法写在 `src/golden.rs` 的模块文档里。

use dhampir_timeline::{
    PROBE_FORMAT_VERSION, SELFCHECK_REPORT_V1, golden_verdict, probe_digest, probe_report,
};

/// 本次构建算出来的报告，必须与仓库里钉住的字节完全相同。
#[test]
fn report_from_public_api_matches_golden() {
    let report = probe_report();
    if let Err(why) = golden_verdict(&report) {
        panic!("{why}");
    }
}

/// golden 本身就是报告格式的版本锁：报告首行的版本号必须与常量一致。
///
/// 这条看着像重复（`src/golden.rs` 里也有），但它锁的是**外部可见的常量**
/// 与**文件内容**之间的关系——有人改了常量而没重新生成 golden，只在这里会红。
#[test]
fn golden_header_matches_format_version() {
    let expected = format!("dhampir-probe v{PROBE_FORMAT_VERSION}\n");
    assert!(
        SELFCHECK_REPORT_V1.starts_with(&expected),
        "golden 首行是 {:?}，期望以 {expected:?} 开头",
        SELFCHECK_REPORT_V1.lines().next().unwrap_or("<空>")
    );
}

/// 摘要与报告是同一份数据的两种呈现，不能各说各话。
///
/// 这里不比对"两次调用 probe_digest() 是否相等"（那是废话），而是拿
/// **golden 文件的字节**算一次摘要，与代码自己算的摘要比。两者不等意味着
/// "报告变了而 golden 没重新生成"或"golden 被改了而报告没跟着变"——
/// 这正是 golden 机制最容易失效的方式：文件停在旧版本，从此没人发现。
#[test]
fn digest_matches_golden_bytes() {
    assert_eq!(
        probe_digest(),
        dhampir_timeline::fnv1a64(SELFCHECK_REPORT_V1.as_bytes()),
        "代码算出的摘要与 golden 文件的摘要不同——报告与 golden 至少有一个是旧的"
    );
}

/// 报告必须是纯 ASCII：跨运行时字节比对的前提。
#[test]
fn report_is_pure_ascii() {
    assert!(probe_report().is_ascii());
    assert!(SELFCHECK_REPORT_V1.is_ascii());
    assert_ne!(probe_digest(), 0, "摘要为 0 说明报告是空的");
}

/// 比对函数面对**别人的**报告时必须说不。
///
/// 这条防的是"比对函数永远返回 Ok"这类坏法——守卫最怕的不是红，是永远绿。
#[test]
fn verdict_rejects_a_report_from_another_world() {
    let mut foreign = probe_report();
    foreign.push_str("tb=99/1 f=0 tc=99:99:99:99\n");
    let err = golden_verdict(&foreign).unwrap_err();
    assert!(err.contains("第 "), "要说清第一处差异在哪一行：{err}");
}
