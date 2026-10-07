//! 路径细分器的**薄壳**：实现搬去了 `dhampir_timeline::path`（契约层语义、无 GPU 依赖），
//! 这样**校验层与渲染层用同一份解析** —— 解析错误能在 `probe` 阶段就报给作者。
//!
//! 这里只做再导出，免得 `crate::render::flatten_path` 的调用点全要改。

pub use dhampir_timeline::path::flatten_path;
