//! M0 验收第 3 条的 **wasm 半边**：同一份断言，在 wasm32 运行时里再跑一遍。
//!
//! 与 `crates/dhampir-timeline/tests/golden_report.rs` 的关系不是"抄了一份"，
//! 而是"同一个断言的两次执行"：
//!
//! | | native | wasm |
//! |---|---|---|
//! | 数据 | `crates/dhampir-timeline/tests/golden/selfcheck-report-v1.txt` | **同一份**（`include_str!` 编进两个产物） |
//! | 比对函数 | `dhampir_timeline::golden_verdict` | **同一个**（不许各写一份） |
//! | 算术运行时 | x86-64 原生整数 | wasm32 整数 |
//!
//! 三者里只有最后一栏不同。所以这两条测试同时通过，才真正回答了验收第 3 条
//! ——"同一份源码两个运行时输出一致"——而不是"我在同一台机器上跑了两遍"。
//!
//! 用 `wasm-pack test --node crates/dhampir-wasm` 运行。
//! **不要**只依赖 `cargo check --target wasm32-unknown-unknown`：
//! 编译通过只证明它**能**编译，不证明它算出同样的字节。
//!
//! 这个文件刻意只碰纯逻辑（不碰 `web_sys::window()`）：Node 里没有 window，
//! canvas 相关的验证归浏览器，见 M0.7 的截图。

// `wasm-bindgen-test` 只是 **wasm32 的** dev-dependency（见 Cargo.toml），
// native 下它根本不在依赖图里。所以整个文件按 target 门掉：native 下它是个
// 空测试目标，`cargo test --workspace` 照常通过，而不是因为"找不到 crate"红掉。
// 门在**文件顶层**而不是逐个函数：一个文件里混着两种 target 的测试，
// 迟早有人加一条 native 下永远不跑的测试还以为它被跑到了。
#![cfg(target_arch = "wasm32")]

use dhampir_wasm::probe::{
    ProbeSummary, golden_check, golden_digest_hex, probe_digest_hex, probe_report,
};
use wasm_bindgen_test::wasm_bindgen_test;

/// **验收第 3 条的本体。**
#[wasm_bindgen_test]
fn report_matches_golden_on_wasm() {
    let summary = ProbeSummary::capture();
    if let Err(why) = summary.verify_golden() {
        panic!("wasm 侧与 golden 不一致：{why}");
    }
}

/// 走另一条入口（不经过 `ProbeSummary`）再验一次，确保两条路径都过同一个比对。
#[wasm_bindgen_test]
fn golden_check_entry_point_agrees() {
    if let Err(why) = golden_check() {
        panic!("{why}");
    }
    assert_eq!(golden_digest_hex().len(), 16);
    assert_ne!(golden_digest_hex(), "0000000000000000");
}

/// 报告文本与摘要必须互相自洽——跨运行时比对的是字节，这一步保证两边
/// 比的确实是同一件事的两种呈现。
#[wasm_bindgen_test]
fn report_and_digest_describe_the_same_bytes() {
    let report = probe_report();
    let digest = probe_digest_hex();
    assert!(report.is_ascii(), "报告必须纯 ASCII");
    assert!(report.ends_with('\n'));
    assert_eq!(digest.len(), 16);
    assert_eq!(
        digest,
        ProbeSummary::capture().digest_hex,
        "两次采集的摘要不同——说明采集过程本身不确定"
    );

    // ① 页面跨进程边界拿到的摘要是**十六进制字符串**（u64 超过 JS `Number` 的
    //    53 位尾数，传数字会得到假绿灯）。这里在证明这条字符串通道是忠实的：
    //    字符串解出来的值与内部 u64 相同。
    let digest_from_u64 = u64::from_str_radix(&digest, 16).expect("摘要应当是十六进制");
    assert_eq!(
        dhampir_wasm::probe::fnv1a64(report.as_bytes()),
        digest_from_u64
    );

    // ② 再走一次**导出给 JS 的**那个实现，确认它与内部实现没有分叉——
    //    M2 要拿它算 PNG 字节的摘要，那时对不上会非常难查。
    assert_eq!(
        dhampir_wasm::web::dhampir_fnv1a64_hex(report.as_bytes()),
        digest
    );
}

/// 摘要比对也必须真的会红：拿一个必然错的摘要喂进去。
#[wasm_bindgen_test]
fn verify_against_rejects_a_wrong_digest() {
    let summary = ProbeSummary::capture();
    assert!(summary.verify_against(&summary.digest_hex).is_ok());
    let err = summary.verify_against("0000000000000000").unwrap_err();
    assert!(err.contains("native=0000000000000000"), "{err}");
}
