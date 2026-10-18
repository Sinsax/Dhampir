# 五分钟跑通 dhampir

从零到看见画面、拿到一个 mp4 的最短路。全部开关见 [usage.md](usage.md)。

## 一、装什么

| 需要 | 用途 | 没有它会怎样 |
|---|---|---|
| **Rust 1.97.0** | 全部构建 | `rust-toolchain.toml` 会自动切到它 |
| **ffmpeg / ffprobe** | 素材探测、字幕栅格化、出片编码 | 0.2.0 起必须带 `--enable-libass`；缺了 `info` / `gop` / `render` 直接失败 |
| **GPU**（Vulkan / DX12 / Metal） | `frame` / `render` 的合成 | `probe` 能跑，出图不能 |
| Chrome / Edge | 浏览器预览 | 只有预览用得到 |

自检：`ffmpeg -filters | grep subtitles`，有输出即带了 libass。字体不内嵌、不猜系统字体：工程写 `subtitle.font_family`（**字体内部的真族名，不是文件名**）+ `--font-dir` 指字体目录，`--font-file` 只兜底；名字不对会静默解析成 `ArialMT`，见 [usage.md](usage.md)。

## 二、确认工具链是活的（不需要工程文件）

```bash
cargo check --workspace
cargo check -p dhampir-wasm --target wasm32-unknown-unknown   # wasm 侧不能带 --workspace
cargo test --workspace                     # native 全量
node scripts/run-wasm-tests.mjs            # wasm32 运行时
cargo run -q -p dhampir-worker --bin dhampir -- --help        # 权威说明在这里
```

## 三、出一份片（单机，CLI）

```bash
node scripts/make-test-media.mjs      # 生成 target/s3/*.mp4（要 ffmpeg；`--check` 只核对；素材不在仓库里）
cargo run -q -p dhampir-worker --bin dhampir -- probe --project fixtures/sample-project.doc.json       # 不要 GPU / ffmpeg
cargo run -q -p dhampir-worker --bin dhampir -- frame --project fixtures/sample-project.doc.json --frame 42 --out ./out   # 要 GPU
cargo run -q -p dhampir-worker --bin dhampir -- render --project fixtures/sample-project.doc.json --from 0 --to 89 --out out.mp4  # 要 GPU + ffmpeg
```

stdout 是 NDJSON 进度流（`start` → 若干 `progress` → `done`），诊断走 stderr。退出码只有三种：`0` 成功 / `2` 用法或校验错（**你能改的**）/ `1` 运行期失败。

## 四、看见画面（浏览器）

```bash
node scripts/web-check.mjs --serve --local
```

打印出的 URL（形如 `http://127.0.0.1:5xxxx/?backend=local&port=8802&project=sample-project.doc`）直接打开：静态服务与本机后端都已起好。改前端后刷新即可，Ctrl+C 结束；**不要接到 `| Select-Object` / `| head`**（管道提前关闭会让它退出）。

## 五、跑一遍验收

```bash
node scripts/run-guards.mjs          # 全部守卫（每条先自检、再正跑）；判定以退出码为准
node scripts/run-guards.mjs --list   # 只列清单
```

## 六、接下来读什么

[usage.md](usage.md)（每个开关）· [api.md](api.md)（wasm 导出 / CLI 子命令）· [host-api.md](host-api.md)（宿主 API 形状与版本）· [../plan/next-steps.md](../plan/next-steps.md)（还剩什么）
