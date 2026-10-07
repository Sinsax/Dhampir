# 第三方许可清单（生成物，不要手改）

本仓自身的许可证是 **Apache-2.0**（见 [`LICENSE-APACHE`](LICENSE-APACHE)）。
下表是**编译进来的第三方 crate** 的许可声明，来自 `cargo metadata`（即 `Cargo.lock` 钉住的那一套）。

```bash
node scripts/licenses.mjs --write     # 重新生成（改了 Cargo.toml/Cargo.lock 之后必须重生成）
node scripts/licenses.mjs --check     # 核对：漂了就退 1
node scripts/licenses.mjs --bundle <目录>  # 抽取每个依赖自带的 LICENSE*/NOTICE*（分发时用）
```

分发（例如 `node scripts/package.mjs` 打的产物）时，必须**随附**本文件与 `LICENSE-APACHE`，
并保留各依赖的版权与许可声明；`--bundle` 就是替你把每个依赖自带的文本抽出来的那一步。

共 **163** 个第三方 crate。

## 按许可表达式汇总

| 许可表达式 | crate 数 |
|---|---|
| `MIT OR Apache-2.0` | 103 |
| `MIT` | 15 |
| `Apache-2.0 OR MIT` | 9 |
| `Unlicense OR MIT` | 8 |
| `Zlib OR Apache-2.0 OR MIT` | 8 |
| `Apache-2.0/MIT` | 4 |
| `Apache-2.0` | 2 |
| `BSD-2-Clause OR Apache-2.0 OR MIT` | 2 |
| `MIT OR Zlib OR Apache-2.0` | 2 |
| `MIT/Apache-2.0` | 2 |
| `Unlicense/MIT` | 2 |
| `Zlib` | 2 |
| `(MIT OR Apache-2.0) AND Unicode-3.0` | 1 |
| `0BSD OR MIT OR Apache-2.0` | 1 |
| `ISC` | 1 |
| `MIT OR Apache-2.0 OR Zlib` | 1 |

## 明细

| crate | 版本 | 许可 |
|---|---|---|
| `adler2` | 2.0.1 | `0BSD OR MIT OR Apache-2.0` |
| `aho-corasick` | 1.1.5 | `Unlicense OR MIT` |
| `allocator-api2` | 0.2.21 | `MIT OR Apache-2.0` |
| `android_system_properties` | 0.1.6 | `MIT OR Apache-2.0` |
| `anstream` | 1.0.0 | `MIT OR Apache-2.0` |
| `anstyle` | 1.0.14 | `MIT OR Apache-2.0` |
| `anstyle-parse` | 1.0.0 | `MIT OR Apache-2.0` |
| `anstyle-query` | 1.1.5 | `MIT OR Apache-2.0` |
| `anstyle-wincon` | 3.0.11 | `MIT OR Apache-2.0` |
| `arrayvec` | 0.7.8 | `MIT OR Apache-2.0` |
| `ash` | 0.38.0+1.3.281 | `MIT OR Apache-2.0` |
| `async-trait` | 0.1.92 | `MIT OR Apache-2.0` |
| `autocfg` | 1.5.1 | `Apache-2.0 OR MIT` |
| `bit-set` | 0.10.0 | `Apache-2.0 OR MIT` |
| `bit-vec` | 0.9.1 | `Apache-2.0 OR MIT` |
| `bitflags` | 1.3.2 | `MIT/Apache-2.0` |
| `bitflags` | 2.13.2 | `MIT OR Apache-2.0` |
| `block2` | 0.6.2 | `MIT` |
| `bumpalo` | 3.20.3 | `MIT OR Apache-2.0` |
| `bytemuck` | 1.25.2 | `Zlib OR Apache-2.0 OR MIT` |
| `bytemuck_derive` | 1.12.1 | `Zlib OR Apache-2.0 OR MIT` |
| `byteorder-lite` | 0.1.0 | `Unlicense OR MIT` |
| `cast` | 0.3.0 | `MIT OR Apache-2.0` |
| `cc` | 1.4.7 | `MIT OR Apache-2.0` |
| `cfg_aliases` | 0.2.2 | `MIT` |
| `cfg-if` | 1.0.5 | `MIT OR Apache-2.0` |
| `codespan-reporting` | 0.13.1 | `Apache-2.0` |
| `color_quant` | 1.1.0 | `MIT` |
| `colorchoice` | 1.0.5 | `MIT OR Apache-2.0` |
| `console_error_panic_hook` | 0.1.7 | `Apache-2.0/MIT` |
| `crc32fast` | 1.5.2 | `MIT OR Apache-2.0` |
| `crunchy` | 0.2.4 | `MIT` |
| `defmt` | 1.1.1 | `MIT OR Apache-2.0` |
| `defmt-macros` | 1.1.1 | `MIT OR Apache-2.0` |
| `defmt-parser` | 1.0.0 | `MIT OR Apache-2.0` |
| `dispatch2` | 0.3.1 | `Zlib OR Apache-2.0 OR MIT` |
| `document-features` | 0.2.12 | `MIT OR Apache-2.0` |
| `env_filter` | 2.0.0 | `MIT OR Apache-2.0` |
| `env_logger` | 0.11.11 | `MIT OR Apache-2.0` |
| `equivalent` | 1.0.2 | `Apache-2.0 OR MIT` |
| `fdeflate` | 0.3.7 | `MIT OR Apache-2.0` |
| `find-msvc-tools` | 0.1.13 | `MIT OR Apache-2.0` |
| `flate2` | 1.1.10 | `MIT OR Apache-2.0` |
| `foldhash` | 0.2.0 | `Zlib` |
| `futures-core` | 0.3.34 | `MIT OR Apache-2.0` |
| `futures-task` | 0.3.34 | `MIT OR Apache-2.0` |
| `futures-util` | 0.3.34 | `MIT OR Apache-2.0` |
| `gif` | 0.14.2 | `MIT OR Apache-2.0` |
| `gpu-allocator` | 0.28.0 | `MIT OR Apache-2.0` |
| `half` | 2.7.1 | `MIT OR Apache-2.0` |
| `hashbrown` | 0.16.1 | `MIT OR Apache-2.0` |
| `hashbrown` | 0.17.1 | `MIT OR Apache-2.0` |
| `image-webp` | 0.2.4 | `MIT OR Apache-2.0` |
| `indexmap` | 2.14.2 | `Apache-2.0 OR MIT` |
| `is_terminal_polyfill` | 1.70.2 | `MIT OR Apache-2.0` |
| `itoa` | 1.0.18 | `MIT OR Apache-2.0` |
| `jiff` | 0.2.37 | `Unlicense OR MIT` |
| `jiff-core` | 0.1.1 | `Unlicense OR MIT` |
| `jiff-static` | 0.2.37 | `Unlicense OR MIT` |
| `js-sys` | 0.3.105 | `MIT OR Apache-2.0` |
| `libc` | 0.2.189 | `MIT OR Apache-2.0` |
| `libloading` | 0.8.9 | `ISC` |
| `libm` | 0.2.16 | `MIT` |
| `litrs` | 1.0.0 | `MIT OR Apache-2.0` |
| `lock_api` | 0.4.14 | `MIT OR Apache-2.0` |
| `log` | 0.4.34 | `MIT OR Apache-2.0` |
| `memchr` | 2.8.3 | `Unlicense OR MIT` |
| `minicov` | 0.3.8 | `Apache-2.0/MIT` |
| `miniz_oxide` | 0.8.9 | `MIT OR Zlib OR Apache-2.0` |
| `miniz_oxide` | 0.9.1 | `MIT OR Zlib OR Apache-2.0` |
| `naga` | 30.0.1 | `MIT OR Apache-2.0` |
| `naga-types` | 30.0.1 | `MIT OR Apache-2.0` |
| `nu-ansi-term` | 0.50.3 | `MIT` |
| `num-traits` | 0.2.19 | `MIT OR Apache-2.0` |
| `objc2` | 0.6.4 | `MIT` |
| `objc2-core-foundation` | 0.3.2 | `Zlib OR Apache-2.0 OR MIT` |
| `objc2-core-graphics` | 0.3.2 | `Zlib OR Apache-2.0 OR MIT` |
| `objc2-encode` | 4.1.0 | `MIT` |
| `objc2-foundation` | 0.3.2 | `MIT` |
| `objc2-io-surface` | 0.3.2 | `Zlib OR Apache-2.0 OR MIT` |
| `objc2-metal` | 0.3.2 | `Zlib OR Apache-2.0 OR MIT` |
| `objc2-quartz-core` | 0.3.2 | `Zlib OR Apache-2.0 OR MIT` |
| `once_cell` | 1.21.4 | `MIT OR Apache-2.0` |
| `once_cell_polyfill` | 1.70.2 | `MIT OR Apache-2.0` |
| `oorandom` | 11.1.5 | `MIT` |
| `ordered-float` | 5.4.0 | `MIT` |
| `parking_lot` | 0.12.5 | `MIT OR Apache-2.0` |
| `parking_lot_core` | 0.9.12 | `MIT OR Apache-2.0` |
| `pin-project-lite` | 0.2.17 | `Apache-2.0 OR MIT` |
| `png` | 0.17.16 | `MIT OR Apache-2.0` |
| `pollster` | 0.4.0 | `Apache-2.0/MIT` |
| `portable-atomic` | 1.15.0 | `Apache-2.0 OR MIT` |
| `portable-atomic-util` | 0.2.8 | `Apache-2.0 OR MIT` |
| `presser` | 0.3.1 | `MIT OR Apache-2.0` |
| `proc-macro2` | 1.0.107 | `MIT OR Apache-2.0` |
| `profiling` | 1.0.18 | `MIT OR Apache-2.0` |
| `quick-error` | 2.0.1 | `MIT/Apache-2.0` |
| `quote` | 1.0.47 | `MIT OR Apache-2.0` |
| `range-alloc` | 0.1.5 | `MIT OR Apache-2.0` |
| `raw-window-handle` | 0.6.2 | `MIT OR Apache-2.0 OR Zlib` |
| `raw-window-metal` | 1.1.0 | `MIT OR Apache-2.0` |
| `redox_syscall` | 0.5.18 | `MIT` |
| `regex` | 1.13.1 | `MIT OR Apache-2.0` |
| `regex-automata` | 0.4.18 | `MIT OR Apache-2.0` |
| `regex-syntax` | 0.8.11 | `MIT OR Apache-2.0` |
| `renderdoc-sys` | 1.1.0 | `MIT OR Apache-2.0` |
| `rustc-hash` | 1.1.0 | `Apache-2.0/MIT` |
| `rustversion` | 1.0.23 | `MIT OR Apache-2.0` |
| `same-file` | 1.0.6 | `Unlicense/MIT` |
| `scopeguard` | 1.2.0 | `MIT OR Apache-2.0` |
| `serde` | 1.0.229 | `MIT OR Apache-2.0` |
| `serde_core` | 1.0.229 | `MIT OR Apache-2.0` |
| `serde_derive` | 1.0.229 | `MIT OR Apache-2.0` |
| `serde_json` | 1.0.151 | `MIT OR Apache-2.0` |
| `shlex` | 2.0.1 | `MIT OR Apache-2.0` |
| `simd-adler32` | 0.3.10 | `MIT` |
| `slab` | 0.4.12 | `MIT` |
| `smallvec` | 1.16.1 | `MIT OR Apache-2.0` |
| `spirv` | 0.4.0+sdk-1.4.341.0 | `Apache-2.0` |
| `static_assertions` | 1.1.0 | `MIT OR Apache-2.0` |
| `syn` | 2.0.119 | `MIT OR Apache-2.0` |
| `syn` | 3.0.6 | `MIT OR Apache-2.0` |
| `termcolor` | 1.4.1 | `Unlicense OR MIT` |
| `thiserror` | 2.0.20 | `MIT OR Apache-2.0` |
| `thiserror-impl` | 2.0.20 | `MIT OR Apache-2.0` |
| `unicode-ident` | 1.0.26 | `(MIT OR Apache-2.0) AND Unicode-3.0` |
| `unicode-width` | 0.2.2 | `MIT OR Apache-2.0` |
| `utf8parse` | 0.2.2 | `Apache-2.0 OR MIT` |
| `walkdir` | 2.5.0 | `Unlicense/MIT` |
| `wasm-bindgen` | 0.2.128 | `MIT OR Apache-2.0` |
| `wasm-bindgen-futures` | 0.4.78 | `MIT OR Apache-2.0` |
| `wasm-bindgen-macro` | 0.2.128 | `MIT OR Apache-2.0` |
| `wasm-bindgen-macro-support` | 0.2.128 | `MIT OR Apache-2.0` |
| `wasm-bindgen-shared` | 0.2.128 | `MIT OR Apache-2.0` |
| `wasm-bindgen-test` | 0.3.78 | `MIT OR Apache-2.0` |
| `wasm-bindgen-test-macro` | 0.3.78 | `MIT OR Apache-2.0` |
| `wasm-bindgen-test-shared` | 0.2.128 | `MIT OR Apache-2.0` |
| `web-sys` | 0.3.105 | `MIT OR Apache-2.0` |
| `weezl` | 0.1.12 | `MIT OR Apache-2.0` |
| `wgpu` | 30.0.1 | `MIT OR Apache-2.0` |
| `wgpu-core` | 30.0.1 | `MIT OR Apache-2.0` |
| `wgpu-core-deps-apple` | 30.0.1 | `MIT OR Apache-2.0` |
| `wgpu-core-deps-windows-linux-android` | 30.0.1 | `MIT OR Apache-2.0` |
| `wgpu-hal` | 30.0.1 | `MIT OR Apache-2.0` |
| `wgpu-naga-bridge` | 30.0.1 | `MIT OR Apache-2.0` |
| `wgpu-types` | 30.0.1 | `MIT OR Apache-2.0` |
| `winapi-util` | 0.1.11 | `Unlicense OR MIT` |
| `windows` | 0.62.2 | `MIT OR Apache-2.0` |
| `windows-collections` | 0.3.2 | `MIT OR Apache-2.0` |
| `windows-core` | 0.62.2 | `MIT OR Apache-2.0` |
| `windows-future` | 0.3.2 | `MIT OR Apache-2.0` |
| `windows-implement` | 0.60.2 | `MIT OR Apache-2.0` |
| `windows-interface` | 0.59.3 | `MIT OR Apache-2.0` |
| `windows-link` | 0.2.1 | `MIT OR Apache-2.0` |
| `windows-numerics` | 0.3.1 | `MIT OR Apache-2.0` |
| `windows-result` | 0.4.1 | `MIT OR Apache-2.0` |
| `windows-strings` | 0.5.1 | `MIT OR Apache-2.0` |
| `windows-sys` | 0.61.2 | `MIT OR Apache-2.0` |
| `windows-threading` | 0.2.1 | `MIT OR Apache-2.0` |
| `zerocopy` | 0.8.57 | `BSD-2-Clause OR Apache-2.0 OR MIT` |
| `zerocopy-derive` | 0.8.57 | `BSD-2-Clause OR Apache-2.0 OR MIT` |
| `zlib-rs` | 0.6.8 | `Zlib` |
| `zmij` | 1.0.23 | `MIT` |

<!-- 生成时 HEAD：a03327c -->
