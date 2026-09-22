//! 把依赖的版本编进二进制。
//!
//! # 为什么需要一个 build script
//!
//! 记录里要出现 **wgpu 版本**，而 wgpu 没有给出任何运行时可读的版本号：
//! `wgpu` crate 里没有 `pub const VERSION`（`wgpu-core` / `naga` 也没有），
//! `Instance` 上也没有这类查询。唯一的真相来源是 `Cargo.lock`——那也正是
//! "这次编译实际用了哪个版本"的**定义**。
//!
//! 两条备选都不如它：
//!
//! - 在我们自己的代码里手写一个字面量 `"30.0.1"`：它会在某次 `cargo update`
//!   之后变成一句谎话，而且没有任何东西会报错。
//! - `include_str!("../../../Cargo.lock")`：编译期就把锁文件绑死在源码路径上，
//!   一旦没有锁文件（打包、`cargo install`）就直接编译失败。
//!
//! 所以：能读到就编进去，读不到就编进 `"unknown"`——**记录里出现 `"unknown"`
//! 是一个可见的缺陷，比一个看起来很象真的假版本号好得多**。`gpu.rs` 里有一条测试钉着
//! "不许是 unknown"，所以本脚本失效会红。
//!
//! # 为什么在 core 而不是每个宿主各来一份
//!
//! 两个宿主都要写"这是哪个 wgpu 编出来的"。各写一份 build script，等于让同一个
//! 事实有两个来源——而漂移的那天正好是最不该有漂移的那天（版本号对上了，记录却不同）。
//! 这里编进 core，`dhampir_core::gpu::WGPU_VERSION` 是唯一出口。

use std::path::{Path, PathBuf};

fn main() {
    let lock = find_lock_file();
    let Some(lock) = lock else {
        emit_unknown("在源码树里找不到 Cargo.lock");
        return;
    };
    println!("cargo:rerun-if-changed={}", lock.display());

    let text = match std::fs::read_to_string(&lock) {
        Ok(text) => text,
        Err(e) => {
            emit_unknown(&format!("读不了 {}：{e}", lock.display()));
            return;
        }
    };

    for (env_key, package) in [
        ("DHAMPIR_WGPU_VERSION", "wgpu"),
        ("DHAMPIR_NAGA_VERSION", "naga"),
    ] {
        match package_version(&text, package) {
            Some(version) => println!("cargo:rustc-env={env_key}={version}"),
            None => {
                emit_unknown_package(env_key, package, &lock);
            }
        }
    }
}

/// 从 `crates/<crate>` 往上找 `Cargo.lock`。
///
/// **走上去找而不是拼死路径**：本仓库现在是 `crates/<crate>/`，但"往上有几层"
/// 这种事会在某次目录调整里悄悄变——变成"版本号静默变成 unknown"的那种变化。
/// 往上找的代价是三行代码，收益是这个脚本不会因为搬目录而失声。
fn find_lock_file() -> Option<PathBuf> {
    let mut dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").ok()?);
    for _ in 0..4 {
        let candidate = dir.join("Cargo.lock");
        if candidate.is_file() {
            return Some(candidate);
        }
        if !dir.pop() {
            break;
        }
    }
    None
}

/// 从锁文件文本里取出某个包的版本。
///
/// 只认 `[[package]]` 段里紧跟 `name = "..."` 的 `version = "..."`，
/// 不做"整段里找 version"那种偷懒写法：`Cargo.lock` 里别的字段（`dependencies`）
/// 也长得像列表，认错一行的后果是记下一个别的 crate 的版本。
fn package_version(lock: &str, package: &str) -> Option<String> {
    let mut in_package = false;
    let mut name_matches = false;
    for line in lock.lines() {
        let line = line.trim();
        if line == "[[package]]" {
            in_package = true;
            name_matches = false;
            continue;
        }
        if !in_package {
            continue;
        }
        if let Some(rest) = line.strip_prefix("name = ") {
            name_matches = rest == format!("\"{package}\"");
            continue;
        }
        if name_matches {
            if let Some(rest) = line.strip_prefix("version = ") {
                return Some(rest.trim_matches('"').to_string());
            }
        }
    }
    None
}

fn emit_unknown(reason: &str) {
    println!("cargo:warning=dhampir-core：读不到 Cargo.lock（{reason}），版本号将记为 unknown");
    for env_key in ["DHAMPIR_WGPU_VERSION", "DHAMPIR_NAGA_VERSION"] {
        println!("cargo:rustc-env={env_key}=unknown");
    }
}

fn emit_unknown_package(env_key: &str, package: &str, lock: &Path) {
    println!(
        "cargo:warning=dhampir-core：{} 里没有 {package} 包的记录，{env_key} 记为 unknown",
        lock.display()
    );
    println!("cargo:rustc-env={env_key}=unknown");
}
