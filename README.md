# dhampir

**渲染与预览的共用底座**，外加配套的技术调研结论。

目标形态：素材存在服务器，渲染导出由服务器处理；**浏览器只做剪辑编辑处理**。
本质是 **render 与 preview 的分别处理**——但两者必须产出**可比的帧**。

| | |
|---|---|
| 底座 | **Rust + wgpu** |
| preview 侧 | **wasm32** 宿主（浏览器 WebGPU + WebCodecs） |
| render 侧 | **native** 宿主（headless wgpu） |

同一个 `dhampir-core` 编译到这两个宿主。底座的核心命题只有一句：

> 同一份工程，在浏览器里看到的和服务器上导出的，是**可比的帧**。

**本仓库不做**（那是下游工程的形态）：API 网关 / 任务队列 / 对象存储 / 部署与容器化 /
分布式分片编排 / 编辑 UI 产品化。下游可以据此接成**客户端-服务端分离的架构**，
也可以接成**本地预览 + 渲染的合并处理**。

`dhampir` 是底座的名字。其余一切都是为上面那句话服务的工程手段。

---

## 名字

引擎名 **`dhampir`**：crate 名、包名、CLI、文档一律用它，不再引入第二套命名。

「dhampir」是英语里半人半吸血鬼的存在——白天一侧（浏览器/预览）与夜晚一侧
（服务端/出片）同源异形，且两边都得能活。词源到此为止，不要往外延伸。

> **范围**：本项目**只做本地开发与本地验证**——不发布包、不建远端仓库、不接远端 CI。

---

## crate 地图与依赖方向

```
        dhampir-timeline          （纯数据：整数帧号 / 有理数时间基 / 时间码）
           ↑          ↑
   dhampir-media   dhampir-core    （CPU 编解码契约 ‖ 渲染图 + WGSL）
           ↑          ↑             ← 两者是**兄弟**，互不依赖
           └────┬─────┘
        dhampir-wasm   dhampir-worker
       （wasm32 宿主）    （native 宿主）   ← 两个宿主互不依赖
```

几条硬规则：

- **单向无环**。`media` 依赖 `timeline` 不是笔误：`VideoInfo` 的时间基必须与时间轴
  共用同一个 `Timebase`，各层各写一份有理数定义，迟早在 `30000/1001` 上分叉。
- **`dhampir-core` 里不允许出现任何 `#[cfg]`**（`#[cfg(test)]` 除外）。
  平台差异用 Cargo 的 target-specific 依赖表达，不在代码里堆条件编译。
  理由：core 是两个宿主唯一的公共资产，它一有条件编译，"同一份源码"就不再是事实。
- **分叉只有两处**：`Instance` 的后端选择（只在两个宿主里：wasm 用 `Backends::BROWSER_WEBGPU`，
  worker 用 `NATIVE_BACKENDS`），以及 `core::io` 的 `FrameSource` / `FrameSink` 实现。
- 上面每一条都有守卫脚本盯着，不靠自觉（见「验证」）。

### 五条铁律

1. **时间用整数帧号，不用浮点秒**。
2. **帧率是有理数**：29.97 是 `30000/1001`，不是 `29.97`。
3. **特效是「类型 + 参数」的声明式数据**，不是代码。
4. **始终按 WebGPU 的能力下限写**，不为某个后端开后门（否则两端等值当场失效）。
5. **SSIM 必须设容差**，且容差表要单独成文件、可被引用——判断"是否回归"的唯一依据。

---

## 现在到哪了

| 里程碑 | 内容 | 状态 |
|---|---|---|
| **M0** | 骨架与双编译贯通 | ✅ 见 [`records/m0/`](records/m0/) |
| **M1** | 服务端 headless wgpu 基线（离屏出图 + 合成 corpus + 复现性） | ✅ 已收官（Linux 两条腿延期，属已知缺口） |
| **M2** | 双运行时同帧 SSIM 比对（架构命门） | ✅ 工程完成（退出标准 4/4）；收尾 4 件待做 |
| M3 | 浏览器预览链路 | ⬜ 未开始（第一件事：S3.1 决策） |
| M4 | 契约闭环：时间线 → 服务端出片 | ⬜ 未开始 |
| ~~M5~~ | ~~分布式分片渲染~~ | ⛔ 移出范围（下游工程） |
| M6 | 一致性保障与发布流程 | ⚠️ 部分移出（只留闸门与资产） |
| ~~M7~~ | ~~产品化~~ | ⛔ 移出范围（下游工程） |

文档在 [`plan/`](plan/)：`video-editor-plan.md` 是执行计划（任务、退出标准、明确不做、风险），
`video-editor-tech-guide.md` 是决策真相，[`foundation-architecture.md`](plan/foundation-architecture.md)
是**实测**的底座架构，[`remaining-work.md`](plan/remaining-work.md) 是交接记录。
**改决策要改文档，不能只在代码里改。**

### M0 的结论

「同一份源码两个运行时」不是幻灯片上的话，它是这样被钉住的：

- `dhampir-timeline` 里的自检探针（6 个时间基 × 10 个帧号 + 3 条 offgrid +
  7 条 tick 逆变换 = 70 个用例）算出一份**纯 ASCII 文本报告**，共 72 行，
  FNV-1a 64 摘要 `c3f0da6b37577e55`。
- 这份报告的期望值以 `include_str!` **编进库里**
  （`crates/dhampir-timeline/tests/golden/selfcheck-report-v1.txt`），
  两端用同一个 `golden_verdict()` 逐字节比对。
- native `cargo test --workspace` 与 wasm32 的 `#[wasm_bindgen_test]` 都断言同一份字节。
  浏览器端因此**不需要服务端先跑一遍**就能自证——页面截图里那个 ✓ 是真结论。
- 跨进程/跨运行时传递摘要时用**十六进制字符串**而不是 u64：JS 的 `Number` 只有
  53 位尾数，u64 会假绿灯。

---

## 构建与验证

工具链由 `rust-toolchain.toml` 钉在 `1.97.0`（含 rustfmt / clippy /
`wasm32-unknown-unknown`）。Windows 上用的是 MSVC 工具链。

```bash
# 双编译（M0 验收 ①②）
cargo check --workspace
cargo check -p dhampir-wasm --target wasm32-unknown-unknown
#   注意：wasm 侧不能用 --workspace —— dhampir-worker 是 native-only

# 测试
cargo test --workspace                     # native 全量
node scripts/run-wasm-tests.mjs            # wasm32 运行时（wasm-bindgen-test-runner）

# 卫生
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
```

### 守卫脚本

一共 **17 个**（`scripts/` 下，各带自己的 `--self-test`）。每个守卫都被**反向验证**过
（临时植入违例，确认它真的会红）。守卫若不会红，就不是守卫。

CI 里跑的是其中**不依赖本机资产**的三条：

```bash
node scripts/check-core-purity.mjs    # core 里没有 #[cfg] / cfg!（只扫去注释后的代码）
node scripts/check-dep-graph.mjs      # 依赖方向单向无环 + 纯层不碰平台 crate
node scripts/check-text-hygiene.mjs   # 全仓 LF + 无 BOM + 合法 UTF-8
```

三者的共同纪律：**不在空文件集上通过**。没有文件可查时退出码是 2，不是 0——
"没扫到"和"扫过了没问题"是两件事。

另外 14 条要本机上的东西（ffmpeg、GPU、真实浏览器、`records/` 里的取证存档），
所以只在开发机上跑，不进 CI —— 它们是：核对台账与路线图引用的 `check-defects`、
顺序解码的 `check-sequential-decode`、wasm pkg 是否陈旧的 `check-web-invariants`、
后端缝的 `check-backend-seam`、预览坐标系的 `check-preview-parity`、
文字叠加接线的 `check-overlay-plumbing`、复核两份里程碑记录的 `check-m1-record` /
`check-m2-record`、media 状态声明的 `check-media-status`、本机后端端到端的
`check-local-backend`、调用面与宿主 API 清单的 `api-surface`、时间线契约派生物的
`timeline-contract`、CLI 契约的 `check-cli`、双端比对的 `check-dual-end`。

**绿不绿以守卫自己的输出为准**：`records/` 里存的是当时的原始输出，不随代码走；
想知道今天绿不绿，跑一遍。

### 跑一遍里程碑验收并留记录

```bash
node scripts/record-acceptance.mjs --milestone m0
```

它按 `plan/` 里写的退出标准逐条跑，把**原始 stdout/stderr** 与每项的退出码落进
`records/m0/`，最后写 `acceptance.json`。判定以退出码为准，不靠读日志下结论。

### 两个宿主各自的产物

```bash
# native：离屏渲染探针图 + adapter 信息 + run.json（--probe-only 则完全不碰 GPU）
cargo run -q -p dhampir-worker --bin dhampir-render -- --out records/m0
cargo run -q -p dhampir-worker --bin dhampir-render -- --probe-only --out records/m0

# 浏览器：本地静态服务 + 记录落盘口
cd crates/dhampir-wasm && wasm-pack build --dev --target web --out-dir www/pkg && cd -
node scripts/serve-wasm-harness.mjs --port 8787
#   打开打印出来的 URL（带 ?autorun=1&expect=<native PNG 的 fnv1a64>）
#   页面结论会 POST 回服务，写成 records/m0/browser-harness.json 与 probe-browser-webgpu.png

# 浏览器那一份的整页截图（自己起服务、起无头 Chrome、等页面跑完再拍）
node scripts/capture-harness-screenshot.mjs
#   落成 records/m0/screenshot-browser-harness.{png,json}
#   先判定、后落图：golden / 与 native 逐字节比对 / 落盘 三者任一不成立就拒写 PNG
#   无头下拿不到 WebGPU 时加 --headed
```

---

## wasm-bindgen 版本对齐（第一天就红的头号来源）

`wasm-bindgen` 的 **crate 版本**必须与 **CLI 版本**一致，否则会报
`it looks like the Rust project used to create this wasm file was linked against a
different version of wasm-bindgen` 并拒绝生成胶水代码——看起来像"构建坏了"，
其实是版本没对齐。

- crate 版本由 `Cargo.toml` 的 `[workspace.dependencies]` 钉住（当前 `0.2.128`）。
- CLI 由 `wasm-pack` 自动下载到自己的缓存里，**不在 PATH 上**；
  `scripts/run-wasm-tests.mjs` 会去缓存目录里找，并**先比对版本再跑测试**
  （不一致直接退出 2，不去猜）。
- 换机器/换 CI 时若报上面那句话，先看这两个版本，别先怀疑代码。

### 本机 node 的一个坑（已绕过，别再踩）

Windows + Node v25.5.0 上，`wasm-bindgen-test-runner` 会在**测试全部通过之后**
报 `UV_HANDLE_CLOSING` 断言并把退出码变成 `-1073740791`——与测试内容无关，
但只要真的调用被强制执行的 `process.exit()` 就可能触发（12 次实测：不接管崩 10 次）。
`scripts/wasm-test-node-exit-shim.cjs` 接管 `process.exit`、只设 `process.exitCode`，
让 node 自然退出；代价是"挂住"成为新风险，所以调用方设了超时。

**不要改成"忽略退出码、只 grep `test result: ok`"**——那是把守卫改成永远绿。

---

## `records/` 是什么

里程碑证据。截图可以骗人，文件可以被人重新算一遍摘要，所以结论都要落成文件：

| 文件 | 内容 |
|---|---|
| `selfcheck-native.txt` | native 侧的探针报告全文（就是 golden 的那份字节） |
| `probe-native-<backend>.png` / `.adapter.json` | 每个 native 后端各一张探针图 + adapter 信息 |
| `probe-browser-webgpu.png` / `browser-harness.json` | 浏览器侧同一张图 + 页面结论 |
| `screenshot-browser-harness.png` / `.json` | 自检页的整页截图 + 这次运行的硬结论（浏览器版本、截图 sha256、状态栏文案） |
| `run.json` / `wasm-tests.json` | 跑了什么、退出码、摘要 |
| `acceptance.json` + `<criterion>.txt` | 退出标准逐条结果 + 原始输出 |
| `guard-*.txt` | 守卫脚本的输出 |
| `README.md` | 这一目录的导览：每份文件证明什么、怎么重跑、读的时候会踩哪些坑 |

行尾不做任何转换：`.gitattributes` 的 `* text=auto eol=lf` 在仓库层面钉死 LF。
（**不要**给 `records/**` 标 `-text`：那会让 JSON 变成不可读的 Binary diff，
而记录就是要被人逐行看的。）

---

## 目录结构

```
crates/
  dhampir-timeline/   帧号 ↔ 时间码（纯整数）、自检探针、golden 报告
  dhampir-media/      纯契约 trait（解码/编码接口，M2+ 填充）
  dhampir-core/       渲染、GPU 抽象、读回、PNG 编码。零 #[cfg]
  dhampir-wasm/       浏览器宿主：wasm-bindgen 导出 + canvas + www/ 自检页
  dhampir-worker/     native 宿主：dhampir-render CLI
plan/                 执行计划与决策真相（改决策要改这里）
records/              里程碑证据（要提交）
scripts/              守卫脚本、记录脚本、本地服务
```

---

## 明确不做（M0/M1 阶段）

FFmpeg 绑定与解码、WebCodecs、任务队列、任何产品 UI、任何真正的渲染特性。
这些都在后面的里程碑里，提前做只会让"两端一致"这件事更难收敛。

## 许可证

MIT OR Apache-2.0，见 [`LICENSE-MIT`](LICENSE-MIT) 与 [`LICENSE-APACHE`](LICENSE-APACHE)。
