//! 把 schema v1 导成 JSON Schema —— TS 类型的上游。
//!
//! 跑：cargo run -p dhampir-timeline --features json-schema --example emit_schema
//!
//! 为什么走 JSON Schema 而不是直接手写 .d.ts：**手写的类型一定会漂**。
//! 契约改一个字段、忘了改 TS，前端拿到的是"看起来对"的类型，直到运行时才炸。
//! 从 Rust 类型推导，就只有一份来源。

fn main() {
    let schema = schemars::schema_for!(dhampir_timeline::schema::Project);
    match serde_json::to_string_pretty(&schema) {
        Ok(text) => println!("{text}"),
        Err(error) => {
            eprintln!("schema 序列化失败：{error}");
            std::process::exit(1);
        }
    }
}
