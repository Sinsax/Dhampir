//! 把契约导成 JSON Schema —— TS 类型与文档的上游。
//!
//! 跑：
//!   cargo run -q -p dhampir-timeline --example emit_schema --features json-schema
//!   cargo run -q -p dhampir-timeline --example emit_schema --features json-schema -- --doc
//!   cargo run -q -p dhampir-timeline --example emit_schema --features json-schema -- --timeline
//!   cargo run -q -p dhampir-timeline --example emit_schema --features json-schema -- --all
//!
//! 为什么走 JSON Schema 而不是直接手写 .d.ts：**手写的类型一定会漂**。
//! 契约改一个字段、忘了改 TS，前端拿到的是"看起来对"的类型，直到运行时才炸。
//! 从 Rust 类型推导，就只有一份来源。
//!
//! # 三种形态，别只导一种
//!
//! * ProjectDoc  —— **写入形态**（工程文件）。带资产表、视图、渲染提示。这是该导的那个。
//! * TimelineV2  —— 渲染契约本体（工程文件里内嵌它）。
//! * Project     —— 裸契约 v1，**兼容形态，不再扩展**。以前只导了它，于是
//!   "工程文件长什么样"这件事没有任何机器可读的来源 —— 只能去读 struct。
//!
//! 默认行为仍是打印 Project（不动既有调用方的输出形状）。

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let what = args
        .iter()
        .find(|arg| arg.starts_with("--"))
        .map(String::as_str)
        .unwrap_or("--project-v1");

    let text = match what {
        "--doc" => pretty(schemars::schema_for!(dhampir_timeline::project::ProjectDoc)),
        "--timeline" => pretty(schemars::schema_for!(dhampir_timeline::layer::TimelineV2)),
        "--all" => {
            let bundle = serde_json::json!({
                "note": "dhampir 契约的三种形态。写入一律写 ProjectDoc。",
                "write_form": "ProjectDoc",
                "ProjectDoc": serde_json::to_value(schemars::schema_for!(dhampir_timeline::project::ProjectDoc)).unwrap_or_default(),
                "TimelineV2": serde_json::to_value(schemars::schema_for!(dhampir_timeline::layer::TimelineV2)).unwrap_or_default(),
                "ProjectV1": serde_json::to_value(schemars::schema_for!(dhampir_timeline::schema::Project)).unwrap_or_default(),
            });
            serde_json::to_string_pretty(&bundle)
        }
        "--project-v1" => pretty(schemars::schema_for!(dhampir_timeline::schema::Project)),
        other => {
            eprintln!("不认识的选项：{other}");
            eprintln!("用法：emit_schema [--doc|--timeline|--project-v1|--all]");
            std::process::exit(2);
        }
    };

    match text {
        Ok(text) => println!("{text}"),
        Err(error) => {
            eprintln!("schema 序列化失败：{error}");
            std::process::exit(1);
        }
    }
}

fn pretty<T: serde::Serialize>(value: T) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(&value)
}
