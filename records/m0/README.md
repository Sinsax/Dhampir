# records/m0 —— M0 里程碑记录

这一目录里全是**产物**，不是感想。每份文件都能被重新算一遍：命令写在第一行、
原始 stdout/stderr 一字未改、退出码单独记。human 能读，机器也能读。

M0 = 「骨架与双编译贯通」。它要证明的只有一件事：**同一份 `dhampir-core` 源码，
在 native 与 wasm32 两个运行时上跑出逐字节相同的结果**——不是"数值看起来一样"。

---

## 一句话结论

`acceptance.json` → `green: true`、`exit_code: 0`，7 条判据全绿（重跑时间以
`acceptance.json` 的 `generated_at` 为准：**2026-09-21T19:39:33.372Z**——这里先前手写过一个
比它早 4 分钟的时间，复核实测后改正；往后别再手抄，指向字段本身）。

其中真正撑起 M0 的那一条是 `cross-runtime`：native 侧 48 条测试与 wasm32 侧 4 条测试
读的是**同一份 72 行 golden 报告**，摘要是 `c3f0da6b37577e55`（两端相同）。
另有一个更强的证据：native（DX12、Vulkan）与浏览器 WebGPU 三方渲染出的离屏 PNG
**逐字节相同**，`fnv1a64 85bbc2017f35dda9`。

---

## 现场（复核这些文件的人需要知道的前提）

| 项 | 值 |
|---|---|
| OS | Windows 10 build 19045（x64） |
| CPU | AMD Ryzen 7 9700X 8-Core |
| GPU | NVIDIA GeForce RTX 4070（驱动 32.0.16.1074）；另有 AMD Radeon 核显（32.0.21030.2001） |
| rustc / cargo | 1.97.0（`rust-toolchain.toml` 钉住；edition 2024，MSRV 1.87 = wgpu 30.0.1 的） |
| Node | v25.5.0 |
| wgpu | 30.0.1（`default-features = false`，后端 feature 只在宿主开） |
| wasm-pack / wasm-bindgen CLI | 0.15.0 / 0.2.128（版本从 `Cargo.lock` 读，不写第二份） |
| 浏览器 | Chrome 153.0.8010.50（无头，用于 `screenshot-*`）；更早一轮的 `browser-harness.json` 来自 Chrome/152.0.7977.76 |
| git | 2.51.0.windows.2 |

---

## 文件清单

### ① 验收：`acceptance.json` + 七份 `*.txt`

`acceptance.json` 是入口：`criteria[]` 里每一条都有 `id` / `command` / `exit_code` /
`ok` / `seconds`，对应同名的 `.txt`——**原始 stdout 与 stderr 分开写**（cargo 把
`Running ...` 打给 stderr、测试结果打给 stdout，拼在一起会让顺序看着像个 bug）。

| id | 命令 | 这份文件证明什么 |
|---|---|---|
| `native-check` | `cargo check --workspace` | native 全工作区编得过 |
| `wasm-check` | `cargo check -p dhampir-wasm --target wasm32-unknown-unknown` | wasm 宿主编得过（**不能用 `--workspace`**：`dhampir-worker` 是 native-only） |
| `native-tests` | `cargo test --workspace` | 48 passed / 0 failed |
| `cross-runtime` | `node scripts/run-wasm-tests.mjs --out records/m0` | 同一份 golden 在 wasm32 上逐字节相等（4 passed，两个 target 退出码均为 0） |
| `guard-core-purity` | `node scripts/check-core-purity.mjs` | `dhampir-core` 的 6 个文件里没有 `#[cfg]` / `cfg!`（豁免仅 `#[cfg(test)]`） |
| `guard-dep-graph` | `node scripts/check-dep-graph.mjs` | 5 个 crate 依赖方向正确、无环 |
| `guard-text-hygiene` | `node scripts/check-text-hygiene.mjs` | 整棵树 50 个文本文件全是 LF、无 BOM、合法 UTF-8（自检 12 内存 + 2 磁盘用例） |

> `acceptance.json` 的 `green` 有个前提：**至少写出一份 `.txt`**。一条都没写出来时
> 它拒绝通过——"没跑"和"跑过了"必须区分开。

> 表里「50 个文本文件」是**整棵树**的口径：含被 `.gitignore` 忽略的产物（如 `pkg-node/`），
> 干净检出里是 **45** 个。另外 `records/`、`target/`、`node_modules/` 等目录按守卫的
> `SKIP_DIRS` 直接跳过——"记录自身合不合规"不在它的结论里（复核报告 U-5 用独立扫描器补过这块）。

### ② 探针：两个 native 后端各一份图 + adapter 信息

时间是假的、GPU 是真的。探针报告契约：首行 `dhampir-probe v1`，格式版本 1，
6 个时间基（`24/1`、`25/1`、`30/1`、`24000/1001`、`30000/1001`、`60000/1001`）
× 10 个帧号 + 3 个 off-grid 帧号，再加 7 条 tick 逆变换，共 `cases=70` → **72 行，纯 ASCII**。

| 文件 | 内容 |
|---|---|
| `probe-native-dx12.png` / `probe-native-vulkan.png` | 256×256 `Rgba8UnormSrgb` 离屏渲染的纯色三角形（8789 字节，三者逐字节相同） |
| `probe-native-dx12.adapter.json` | `backend: Dx12`、`name: NVIDIA GeForce RTX 4070`、`device_type: DiscreteGpu`、`driver: 32.0.16.1074`、`probe_digest: c3f0da6b37577e55` |
| `probe-native-vulkan.adapter.json` | `backend: Vulkan`、同一个 GPU、`driver: NVIDIA` / `driver_info: 610.74`、同一个摘要 |
| `run.json` | 汇总：两个后端 `deterministic_in_process: true`、`error: null`、5 个采样像素的 RGBA、`sample_check_passed: true`、`probe.lines = 72` |
| `selfcheck-native.txt` | native 侧的 72 行报告全文——`cross-runtime` 比的就是它和 wasm 侧那份 |

复现：见本文件末尾「怎么重跑」。

### ③ 浏览器宿主（第三个后端）

| 文件 | 内容 |
|---|---|
| `browser-harness.json` | 页面把结论 POST 回本地服务落盘的**机器可读**版本：`golden_check_passed: true`、`digest_hex == golden_digest_hex == c3f0da6b37577e55`、`lines: 72`、canvas adapter（`BrowserWebGpu`、surface `Bgra8Unorm` @ 256×256）、离屏 PNG 8789 字节 / `fnv1a64 85bbc2017f35dda9`、`expected_png_match: true` |
| `probe-browser-webgpu.png` | 浏览器渲染并读回的同一张图，与 native 侧逐字节相同 |
| `screenshot-browser-harness.png` | 自检页的**整页截图**（1400×1278，88779 字节）。上面能看到状态栏、两处摘要、两个 canvas、落盘清单 |
| `screenshot-browser-harness.json` | 截图的旁证：浏览器版本、页面 URL、截图 sha256、状态栏文案，以及这次运行的硬结论 |

`expected_png_fnv1a64` 这一项值得单独说：它不是页面自己算的，而是**本地服务从
`probe-native-dx12.png` 的字节算出**后经 URL 传进页面的。页面拿它跟自己渲染的结果比，
所以"两端一致"不是页面在自说自话。

### ④ 守卫的输出

`guard-*.txt` 与上表同源，单独列出来是因为它们是最容易被"改到永远绿"的东西：
每个守卫都带 `--self-test`、都做过**反向验证**（临时植入违例，确认它真会红）、
都拒绝在空文件集上通过，且**任何脚本都不许"忽略退出码、只 grep 成功字样"**。

`wasm-tests.json` 里还记着一条容易踩的坑：Windows + Node 上真调用 `process.exit()`
可能撞 libuv 的 `UV_HANDLE_CLOSING` 断言，退出码变成 `-1073740791`（与测试内容无关）。
`node_exit_shim` 这个对象如实记下**这一次到底注没注**（`applied` / `platform` /
`reason` / `shim` / `override`）；非 win32 平台不注入——本机没跑过的退出路径不多带一条。

---

## 怎么重跑

```bash
# 验收 7 条（会覆盖 acceptance.json 与 7 份 *.txt）
node scripts/record-acceptance.mjs --milestone m0

# 探针图与 adapter（两个后端）
cargo run -p dhampir-worker --bin dhampir-render -- --probe-only --backend dx12   --out records/m0
cargo run -p dhampir-worker --bin dhampir-render -- --probe-only --backend vulkan --out records/m0

# 浏览器那一份：先打包 wasm，再起本地服务，然后自动跑 + 截图
wasm-pack build crates/dhampir-wasm --target web --out-dir www/pkg --dev
node scripts/capture-harness-screenshot.mjs
```

`capture-harness-screenshot.mjs` 自己会起服务、起无头 Chrome、等页面跑完、再整页截图。
它**先判定、后落图**：`golden_check_passed` / `expected_png_match` / 落盘三者任一不成立
就拒写 PNG——宁可没有这张图，也不要一张看着全绿的图。无头下拿不到 WebGPU 时加 `--headed`。

---

## 读这些记录时会踩的坑

1. **`run.json` 里的摘要不在顶层。** 它是 `run.json.probe.digest`，不是 `run.json.probe_digest`。
   顶层另有一个同名键的只是 `probe-native-*.adapter.json`（那两个文件里确实在顶层）。
2. **`canvas_adapter.name` 是空字符串。** 浏览器不把底层 GPU 名字给页面。
   所以浏览器那一栏只能证明"用的是 WebGPU 后端"，证明不了"用的是哪块卡"。
3. **计时项不要当判据。** `canvas_render_ms`、`offscreen.total_ms` 每次都不同；
   被钉死的只有 `pinned_expectations`（目标格式 `Rgba8UnormSrgb`、尺寸 `256x256`）
   与两个摘要。
4. **`unix_epoch_seconds` / `received_unix_epoch_seconds` / `captured_at` 是记录里
   唯一的非确定项**——它们存在的意义就是让人能判断"这份记录比那份新"。
5. **两个 `.png` 别混。** `probe-*.png` 是 256×256 的渲染产物（8789 字节，三方逐字节相同）；
   `screenshot-browser-harness.png` 是自检页的整页截图（88779 字节）。名字里已经写清了。
6. **`selfcheck-native.txt` 与 `cross-runtime.txt` 是同一次比对的两半。** 前者是 native
   跑出来的报告全文，后者是 wasm32 跑出来的测试日志；只有两边都在，`cross-runtime`
   这条判据才是闭合的。
7. **`guard-*.txt` 里的数字是 M0 那棵树的数字。** 它们是那次运行的 stdout 原文
   （`50 个文件`、`6 个文件`、crate 的打印顺序都按当时的树）；M1 之后树变了、守卫也
   改过措辞与自检条数，**重跑会得到语义等价但数字不同的输出**。要今天这一轮的数，
   看 `records/m1/acceptance.json`（那是最新一次落盘的快照）。

---

## 这份记录**不**证明什么

- **不证明性能。** 这里没有任何一处对帧时/吞吐下结论；`seconds` 字段只是命令耗时的记录。
- **不证明 Linux 上跑得通。** 本机是 Windows。`x86_64-unknown-linux-gnu` 的
  `cargo check` 在本机过了（见 `plan/video-editor-plan.md` §3 的 T0.6 与退出标准第 1 条，
  以及 `.github/workflows/ci.yml` 顶部那段预验注释），但 `cargo check` 不链接、也不运行；
  **`records/m0/` 里没有这条命令的产物**——它是文字断言，由 `records/m0/review-independent.md`
  的 E-4 在独立克隆里重跑过一次（退出码 0）。
- **不证明 CI 跑得通。** `.github/workflows/ci.yml` 尚未在真 runner 上跑过——
  仓库还没建，首次推送时才会验证。
- **不证明 Safari / Firefox 上跑得通。** 浏览器那一栏只有一个 Chromium 系宿主。
- **不证明 crates.io 上的名字是 `dhampir`。** 占名（T0.1）还等授权，见 `plan/video-editor-plan.md` §3。

---

## 相关文档

- 计划与决策真相：`plan/video-editor-plan.md`（本目录的结论回填在 §3）
- 本目录的独立复核（另一位复核者在隔离克隆里重跑）：`records/m0/review-independent.md`
  ——它列出的 F-1 / F-4 / F-5 三条瑕疵已在本目录里改正，U-1～U-5 是**未被覆盖的面**
- 里程碑 M1 的记录（更新一轮的现场与守卫输出）：`records/m1/README.md`
- 项目总览与构建方式：`README.md`（仓库根）
- 守卫与记录工具：`scripts/`（`record-acceptance.mjs`、三个 `check-*.mjs`、
  `run-wasm-tests.mjs`、`serve-wasm-harness.mjs`、`capture-harness-screenshot.mjs`）
