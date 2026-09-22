# dhampir · 落地执行计划（M0–M7）

> **配套文档**：`plan/video-editor-tech-guide.md` —— 技术指导回答"为什么这么选"，本文件回答"从哪开始、每步做什么、怎么算做完"。
> 项目 / 引擎名：**`dhampir`** ｜ 形态：浏览器端编辑 + 服务端渲染导出 ｜ 底座：Rust + wgpu 双运行时
> 制定日期：2026-09-22 ｜ 状态：待执行

**一句话路线**：M0 立骨架 → M1 服务端出图 → **M2 双运行时同帧比对（架构命门）** → M3 浏览器预览 → M4 契约闭环 → M5 分片 → M6 保障 → M7 产品化。

---

## 0. 怎么用这份文件

- **阅读顺序**：指导文档「命名与项目定义」→ 指导文档 §9 → 本文件。名字来历、架构决策、风险背景都在指导文档里，本文件不复述。
- **执行规则**：按里程碑顺序推进，**上一个里程碑的「退出标准」全绿才允许进入下一个**。每个里程碑的「明确不做」是防线，不要提前引入。
- **勾选规则**：任务与验收都是 `- [ ]`，做完就勾。延期、变更、否决直接改本文件，不另开文档——本文件是唯一进度真相。
- **会话节奏**：**一个会话只推一个里程碑**，避免上下文污染。新会话开头的自包含信息就是本文件 + 指导文档，不需要原会话上下文。
- **证据纪律**：每个里程碑结束时必须留下可复核的产物（PNG、CSV、决策文件、阈值表），不接受"跑通了"这种口头结论。
- **估算口径**：所有「预估」按 **1 名熟悉 Rust + 图形编程的全职人力** 计；并行说明写在 §11.4。

---

## 1. 里程碑总览

| # | 里程碑 | 目标（一句话） | 对应指导文档 | 预估 | 前置 | 闸门（不过就得改方案） |
|---|---|---|---|---|---|---|
| **M0** | 骨架与双编译贯通 | 同一份源码在两个 target 上编译并输出一致 | §9.3 / §9.4 | 1–2 人日 | 环境准备 | 双编译过不了 → 命名空间/工具链重选 |
| **M1** | 服务端 headless wgpu | 目标环境能离屏出图且可复现 | §9.1 #1 | 2–4 人日 | M0 | 云环境跑不起来 → 换实例/降级 |
| **M2** | **双运行时同帧 SSIM** | 同一份 WGSL，两端渲染同一帧结果一致 | §9.1 #2 | 3–5 人日 | M1 + M0 的 wasm 壳 | **架构命门**，不过则触发 Plan B |
| **M3** | 浏览器预览链路 | proxy 硬解 → 零拷贝上 GPU → 出画面 | §9.1 #3 / §6 / §3.2 | 1–2 周 | M0（可与 M1/M2 并行） | WebCodecs/外部纹理走不通 → 改预览架构 |
| **M4** | 契约闭环 | 时间线 JSON 驱动两端，出片与预览一致 | §9.1 #4 / §5 / §7 | 2–3 周 | M2 + M3 | 含解码差异不可控 → 收紧色彩管线 |
| **M5** | 分布式分片渲染 | 分片结果 == 整体渲染 | §9.1 #5 / §7.1–7.3 | 2–3 周 | M4 | 分片≠整体 → 最坏退回单片 |
| **M6** | 一致性与发布保障 | SSIM 进 CI + 影子环境闸门 | §9.1 #6 / §8 | 1 周 + 持续 | M2 / M5 | — |
| **M7** | 产品化（占位） | 上传→proxy→存储→导出→前端 UI | §2 / §6.2 | 待定 | M4 | — |

**关键路径**：M0 → M1 → M2 → M4 → M5。

**可并行**：M3（浏览器侧）不依赖 M1/M2 的服务端结论，只依赖 M0 的骨架 + 一个 proxy 素材，因此**第二人力从 M3 切入收益最高**。

**顺序的理由**（指导文档 §9.2，此处展开）：先在服务端把 `dhampir-core` 写对——服务端能 dump 帧、能单步、能跑比对，浏览器里调 GPU 的痛苦程度高一个量级；但**第一天就要把 wasm 空壳跑起来**，它的作用不是功能，而是当场暴露 native-only 的 API 使用。

---

## 2. 环境准备（M0 之前，半天）

**工具链**

- [ ] Rust 稳定版（在 `rust-toolchain.toml` 里 **pin 具体版本**，不用 `stable` 漂移）
- [ ] `rustup target add wasm32-unknown-unknown`
- [ ] `wasm-pack` / `wasm-bindgen-cli` —— **版本必须与 `wasm-bindgen` crate 版本对齐**（最常见的"第一天就红"来源，写进 README）
- [ ] Node 20+ / pnpm（M3 起）
- [ ] FFmpeg 8.x CLI（**M0–M3 只用 CLI，M4 之后才引 Rust 绑定**）
- [ ] Windows：VS Build Tools（MSVC）；可选 Vulkan SDK（`vulkaninfo` 用于环境探针）

**环境矩阵（M1 起要用）**

| 环境 | 用途 | 备注 |
|---|---|---|
| Windows 本机（DX12 + Vulkan 各跑一遍） | 日常开发 | 最快反馈回路，M1 先在这里做 |
| Linux 容器 + NVIDIA Container Toolkit | 目标部署形态 | Vulkan ICD 注入，见 §7.5 的部署代价 |
| Linux + lavapipe（CPU 软渲染） | 兜底/廉价 CI | 性能掉一个数量级，只用于链路验证 |
| 云 GPU 实例（spot） | M1 之后才开 | 按小时计费，能本机做完的就别开云 |

**仓库侧**

- [ ] GitHub 建仓（`dhampir` 用户名/组织名占用状态未查，先查再建）
- [ ] **crates.io 占名**（时间敏感，见 M0 T0.1）

---

## 3. M0 —— 骨架与双编译贯通

> **目标**：把 workspace 骨架立起来，让双编译链路当天就暴露问题。**这一步不写任何功能。**

**前置**：§2 环境准备。

### 任务

- [ ] **T0.1 命名落地（时间敏感，先做）** —— ⏳ **仍待用户决定**：crates.io 发布授权、GitHub 建仓与可见性
  - ✅ 已备好：`LICENSE-MIT` / `LICENSE-APACHE`（Apache 全文含 APPENDIX）、根 `README.md`（写明项目意图、命名三层、crate 地图、版本对齐纪律），四个 crate 都是**能 `cargo build` 的真实最小 crate**，不是空占位
  - ⏳ 待做：`cargo publish` 四个 0.0.1（`dhampir` / `dhampir-timeline` / `dhampir-media` / `dhampir-core`）、建 GitHub 仓、域名自查 `.dev` / `.rs` / `.io`
  - ⏳ `[workspace.package] repository` 回填 —— 依赖建仓结果，**不编造 URL**
  - 上层应用名（yeki 相关）：**暂缓**，需要平台与罗马字才能定，不阻塞 M0
- [x] **T0.2 workspace 骨架**
  - `Cargo.toml`：`[workspace]` + `members` + `[workspace.package]`（version / edition / license）+ `[workspace.dependencies]` 统一版本
  - `rust-toolchain.toml`：pin 1.97.0 + `targets = ["wasm32-unknown-unknown"]` + components `rustfmt` / `clippy`
  - `rustfmt.toml`（`newline_style = "Unix"`）/ `.gitignore` / **`.gitattributes`**
  - 五个 crate，依赖方向与指导文档一致（`media` 依赖 `timeline` —— 理由见根 `Cargo.toml` 头部）
  - 「防依赖方向被写反」的检查：**没引 `cargo deny`**，换成 `scripts/check-dep-graph.mjs`（自己解析 manifest，检查允许的内部边 + 纯层不得碰平台 crate + 找环 + workspace members 与磁盘目录一致）。理由：`cargo deny`/`cargo tree -d` 只能看**已解析的**依赖，看不到"禁止引入"这件事；自写守卫还能对**被删掉的边**报警
  - 新增（不在原计划里，但被实践逼出来）：`.gitattributes` 的 `* text=auto eol=lf`。没有它，文本卫生守卫在 Windows 上会因 git 按 CRLF 检出而无辜变红——**误报会诱导人删掉守卫**
- [x] **T0.3 跨运行时等价性探针（本里程碑真正的验收核心）**
  - `dhampir-timeline`：`frame_to_timecode(frame: i64, tb: Timebase) -> Timecode`
    - **纯整数运算，不碰浮点**——探针要测的是"同一份源码两个运行时"，不要引入浮点变量混淆结论
    - ✅ 实际做的比原计划更硬：探针输出一份 **72 行、70 用例的纯 ASCII 报告**（6 个时间基 × 10 个帧号 + 3 条 offgrid + 7 条 tick 逆变换），FNV-1a 64 摘要 `c3f0da6b37577e55`
  - native 侧 `#[test]`：断言典型 timebase（如 30000/1001）下的已知值
- [x] **T0.4 wasm 侧同一函数 + 断言** —— 两条路径**都做了**
  - `wasm-bindgen-test` + `wasm-pack test --node`：4 条 `#[wasm_bindgen_test]` 在 wasm32 上真跑
  - 导出自检函数由 JS/页面调用断言：`dhampir_probe_golden_check()` 等 6 个导出
  - ✅ 关键做法：**golden 报告用 `include_str!` 编进库**，两端共用同一个 `golden_verdict()` 逐字节比对。浏览器端因此不需要服务端先跑一遍就能自证——截图里那个 ✓ 是真结论
  - ✅ 跨运行时摘要传**十六进制字符串**而不是 u64（JS `Number` 只有 53 位尾数，u64 会假绿灯）
- [x] **T0.5 `dhampir-core` 最小渲染 + 唯一分叉**
  - core：一个纯色三角形 / 单帧写入即可（`render_frame(device, queue, target)`）
  - **`Instance` 的创建是唯一允许分叉的地方**，且它只出现在两个宿主里：
    ```rust
    // dhampir-wasm 入口
    Instance::new(Backends::BROWSER_WEBGPU)
    // dhampir-worker 入口
    Instance::new(Backends::VULKAN | Backends::DX12 | Backends::METAL)
    ```
  - `FrameSource` / `FrameSink` trait 先落签名（实现在 M2/M3 补）
  - ✅ 零 `#[cfg]` 由 `scripts/check-core-purity.mjs` 守着（只扫**去注释后**的代码，且带反向验证；豁免仅 `#[cfg(test)]`）。"能编译"另有一层证明：真建 shader module、真跑 render pass——**不引 naga 做语法验证**
  - ✅ 读回等待逻辑（`MapWait` / `MapSlot`）写在 core 里，宿主只提供 executor（native `pollster`、wasm `wasm_bindgen_futures`）——否则"同一份 core"会从等待语义这里漏出去
  - ✅ 踩坑记录：canvas surface **必须显式 `SurfaceTarget::Canvas`**。wgpu 30 的 blanket `Into` 要求 `HasWindowHandle + HasDisplayHandle`，而 `HtmlCanvasElement` 的 impl 需要 raw-window-handle 的 `wasm-bindgen-0-2` 特性——**wgpu 的 `web` feature 不替下游打开它**。这个坑值得在 M3 之前记住
- [x] **T0.6 CI 骨架**（推荐，半小时的事） —— ✅ 已写 `.github/workflows/ci.yml`（`check-native` / `check-wasm` / `guard` 三个 job）
  - 见 §11.1，先只跑三条 check，不含 GPU 任务
  - ⚠️ **尚未在真 runner 上跑过**（本机没有 GitHub runner，仓库也还没建）。首次推送时验证
  - ✅ 已在本机预验最易首跑就红的那一路：`cargo check --workspace --target x86_64-unknown-linux-gnu` → 退出码 0。`cargo check` 不链接，所以这条能真正证明 Linux 的 cfg 分支都在（`dhampir-worker` 的 `vulkan`/`dx12`/`metal` feature 在 Linux 上确实编得过，靠的是 wgpu 自己的 `wgpu-core-deps-windows-linux-android`）
  - ✅ **没有任何 `continue-on-error` / `|| true`**：忽略退出码就是把守卫改成永远绿，而"永远绿"通常就是这样开始的
  - ✅ wasm-bindgen CLI 的版本从 `Cargo.lock` 读（`node scripts/run-wasm-tests.mjs --print-locked-version`），不在 workflow 里写死第二份

### 产出物

可编译 workspace + 三条绿线 + CI 骨架（**待首次推送验证**）+ crates.io 四个名字（**待授权**）。

里程碑记录见 [`records/m0/`](../records/m0/README.md)：验收逐条原始输出、两个宿主各自的探针图与 adapter 信息、
浏览器宿主自检页的整页截图与落盘结论。记录都是文件而非截图——截图可以骗人，文件可以被人重新算一遍摘要。

- ✅ 截图不是"打开浏览器按 PrintScreen"：`scripts/capture-harness-screenshot.mjs` 自己起服务、
  起无头 Chrome、经 CDP **等页面跑完**再整页拍，落成 `screenshot-browser-harness.{png,json}`。
  它**先判定、后落图**——`golden_check_passed` / `expected_png_match` / 落盘三者任一不成立就拒写 PNG。
- ✅ 从"点按钮"改成"等状态栏"：页面已经暴露 `?autorun=1`，脚本不重复触发 `run()`，
  否则两次运行会 POST 两次，图上和图下的结论可能来自不同那一次。

### 退出标准（三条全绿才算过，源自指导文档 §9.4）

- [x] `cargo check --workspace`（native）通过 —— ✅ 另加了 `x86_64-unknown-linux-gnu` 交叉 check
- [x] `cargo check -p dhampir-wasm --target wasm32-unknown-unknown` 通过
  - 注：wasm 侧**不能用 `--workspace`**——`dhampir-worker` 是 native-only，`gateway/` 与 `web/` 不在 Cargo workspace 里
- [x] 帧号 → 时间码换算在 native `#[test]` 与 wasm 侧断言输出 **完全一致** —— ✅ 不只是"数值对得上"：两端逐**字节**比对同一份 72 行报告（native 48 条 + wasm32 4 条测试）

> 第 3 条是这一步真正的目的：它证明"同一份源码两个运行时"不是幻灯片上的话。
> 结论已由 `node scripts/record-acceptance.mjs --milestone m0` 落进 `records/m0/acceptance.json`
> （7 条判据全绿，每条都留了原始 stdout/stderr 与退出码）。

### 明确不做

FFmpeg 绑定（先造假帧源）、WebCodecs、任务队列、任何 UI、任何真正的渲染特性。

### 风险与提示

| 风险 | 对策 |
|---|---|
| `wasm-bindgen` 版本漂移 | crate 与 CLI 版本对齐 + 写进 README；`run-wasm-tests.mjs` **先比版本再跑测试**，不一致直接退出 2；CI 从 `Cargo.lock` 读版本，不写死第二份 |
| `#[cfg]` 从 core 漏进来 | `scripts/check-core-purity.mjs`（带 `--self-test` 与反向验证，只扫去注释后的代码，不在空文件集上通过） |
| 占位包被 crates.io 判为 name squatting | 四个 crate 都是可编译的真实最小 crate，README 说明意图，MIT/Apache-2.0 双许可文件已就位 |
| **守卫自己坏掉**（误报 → 诱导人删守卫；或恒绿 → 什么也挡不住） | 每个守卫都带 `--self-test`，都做过**反向验证**（临时植入违例，确认它真会红），都拒绝在空文件集上通过 |
| **测试真的全过、退出码却是崩的**（Windows + Node 的 libuv `UV_HANDLE_CLOSING` 断言） | `scripts/wasm-test-node-exit-shim.cjs` 接管 `process.exit`（只设 `exitCode`）。**只治真有缺陷的平台**（`decideShim` 按 `platform` 判定，可用 `DHAMPIR_WASM_TEST_SHIM=1\|0` 强制）；代价是可能挂住，所以设了超时。**绝不改成"忽略退出码、只 grep `test result: ok`"** |
| git 在 Windows 上按 CRLF 检出，让文本卫生守卫无辜变红 | `.gitattributes` 的 `* text=auto eol=lf` 在仓库层面钉死，不靠每个人的本地配置 |

---

## 4. M1 —— 服务端 headless wgpu 基线

> **目标**：证明 headless wgpu 能在目标环境离屏出图，且输出稳定可复现。
> **为什么排这个位置**：整条链路最不确定的是容器 GPU 注入，而不是渲染逻辑。先用最简单的东西把它钉死。

**前置**：M0 退出标准全绿。

### 任务

- [x] **T1.1 worker CLI 骨架** —— `dhampir-render`（`dhampir-worker` 的 `[[bin]]`），11 条 CLI 契约测试钉住接口
  - `dhampir-render --scene <name> --frames <range> --out <dir>`
  - ✅ 实际接口比原计划宽：`--scene all|<name>`、`--frames a..b`（**半开区间**，`..=` 直接拒收，空区间/反写报错）、`--out`（corpus 模式**必须显式给**——默认值指向 `records/m0` 归档，不给就拦下）、`--backend all|dx12|vulkan`、`--compare-run <run.json>`、`--skip-timing`、`--probe-only`
  - ✅ `--frames` / `--compare-run` / `--skip-timing` 在没有 `--scene` 时**报错**而不是静默忽略——静默忽略会让人以为自己验过
  - 此时**没有时间线概念，没有解码**，只有硬编码场景
- [x] **T1.2 离屏渲染 + 读回**
  - 目标纹理 `Rgba8UnormSrgb`，usage `RENDER_ATTACHMENT | COPY_SRC`
  - `copy_texture_to_buffer` —— 注意 **`bytes_per_row` 256 字节对齐**（经典坑）
  - ✅ 对齐不是"知道"而是探针实测：1366×768（5464 字节/行）→ 填充到 5632 后才可比，`exercises_padding: true`、1049088 像素最差距离 0；且**在"本来就不需要填充"的宽度上拒绝报通过**
  - ✅ PNG 编码落进 core（`readback::Rgba8Image::encode_png`），依赖用 `png` 0.17 而不是整个 `image`——链接面小一个量级；守卫侧的 PNG **解码**器自写（只用 `node:zlib`），不引第三方 PNG 库
- [x] **T1.3 合成测试场景集（corpus，M2 直接复用）**
  - `gradient`：全范围渐变 —— 考精度
  - `checker`：像素级棋盘 —— 考采样
  - `srgb_linear`：sRGB ↔ linear 往返色块 —— 考色彩/传输函数
  - `alpha_stack`：多层半透明叠加 —— 考混合顺序
  - `blur`：可分离高斯 —— 考浮点累加顺序
  - **全部确定性**：无时间、无随机；必须随机时用固定 seed 的确定性 PRNG
  - 场景代码进 `dhampir-core`（同一份代码两个宿主都要调）
  - ✅ 全部落进 core：`render/scene.rs`（注册表 / 入口名 / 采样表 / 混合状态 / 容差）+ `render/scene_model.rs`（纯 `f64` 数值模型）+ `shaders/scene.wgsl`（253 行，`wc -l` 口径：`split('\n')` 会数出 254，文件以换行结尾）——M2 的 wasm 侧直接复用，不需要第二份实现
  - ✅ 确定性是构造出来的，不是靠 PRNG：五场景无时间无随机，**连 PRNG 都没用上**
  - ✅ 判据 = 与 clamp 模型的**字节距离 ≤ 1**（`byte_tolerance`）；故意的缺陷模型距离在 2–78 之间——容差没有宽到能放过缺陷
  - ✅ 预测表两张网：`PINNED` 逐点相等 + 整周期整表的 FNV-1a 64 摘要 `fff8d28ff54c24d8`；模型测试**必须包含"缺陷模型"的距离断言**，否则"模型正确"只是自说自话
  - ✅ 踩坑：`fs_blur_h` / `fs_blur_v` 首版用 `frag.xy` 当纹理坐标直接采样，**编译期全绿、真跑 GPU 才暴露**；修成 `texel_of(frag.xy)` 后由 160 帧 corpus 全绿确认
  - ✅ `alpha_stack` 是直通 alpha 的 source-over，**不能用 `PREMILLIPLIED_ALPHA_BLENDING`**；blur 权重按 6 位小数四舍五入、Σ = 1.000000
- [x] **T1.4 环境探针与复现性** —— Windows 两条腿全达成；Linux 两条腿 **⏳ 待补**（缺的是环境，不是代码路径）
  - 记录 `adapter.get_info()`（name / backend / driver）+ wgpu 版本 + 时间戳 → `adapter.json`
    - ✅ 记的是 `describe_adapter` 的人类可读输出（name / backend / driver / device_type / subgroup / limits 摘要），不是 `wgpu::AdapterInfo` 的 Debug；`adapter.json` 与 `timing.json` **刻意拆开**（"几乎不变" vs "每次都变"），两份**共用同一个** `unix_epoch_millis`（守卫会真的比对这两个数，并校验 `unix_epoch_seconds === floor(ms / 1000)`）。**"拆开"不等于"键不重叠"**——先前这里写作"且键不重叠"，复核实测后改正：两份实测 18 / 15 个键里有 **10 个同名**，其中 `kind`（`"adapter"` vs `"timing"`）与 `nondeterministic_fields`（各自的非确定项清单）两键**值不同**，其余 8 个（`adapter_name`、`backend_slug`、`build_profile`、`milestone`、`requested_backends`、`schema`、`unix_epoch_millis`、`unix_epoch_seconds`）刻意取同值。"拆开"说的是**非确定项各归各**，不是"没有同名键"
  - 复现性检查：同机同后端，同帧渲染两次（同进程 + 跨进程）**逐字节相同**
    - ✅ 同进程：每帧渲染两次比字节，两条腿各 80 帧，`repeat_mismatches: []`；真不一致时**照记不误**、该帧不做颜色判定（`passed` 三态），不是失败而是发现
    - ✅ 跨进程：第二条腿带 `--compare-run` 与第一条腿比，`identical: true`、`matched_frames: 80`，两条腿整表摘要同为 `71ecc80cade3d73d`
    - ✅ 顺带拿到一条计划外结论：**DX12 与 Vulkan 的同名 PNG 逐字节相同（80/80）**——同一份 WGSL 在两个驱动栈上出了同样的字节
  - 依次跑：Windows/DX12 ✅ → Windows/Vulkan ✅ → Linux 容器 GPU ⏳ → Linux/lavapipe ⏳（本机无 docker、WSL 无发行版）
- [x] **T1.5 性能基线**：Init 时间、单帧渲染时间、读回时间（1080p）—— 每场景 24 次取**中位数**，极值照记
  - ✅ DX12：init `268.345 ms`；「渲染 + 读回」往返中位 2.119–2.289 ms、纯 CPU 提交中位 0.103–0.124 ms
  - ✅ Vulkan：init `126.412 ms`；往返中位 2.099–2.217 ms、纯 CPU 提交中位 0.058–0.074 ms
  - ✅ 两个计时数名字说清各是什么：`worst_frame_cpu_ms` 是 CPU 编码 + `submit`（异步，**不含 GPU**），`worst_roundtrip_ms` 是渲染 + 读回往返。**判预算用后者**——前者没有能力否证"一帧画完 ≤ 10 ms"，两者不一致时选会高估的那个；`budget_metric` / `budget_metric_note` 把选择写进记录，守卫按 `worst_roundtrip_ms` 自己重算一遍 verdict

### 产出物

`frames/*.png` + `adapter.json` + 计时表 + 四种环境矩阵结果（**2/4，Linux 两条 ⏳**）。

里程碑记录见 [`records/m1/`](../records/m1/README.md)：两条腿各 5 份 JSON/TXT + 80 张 PNG（全目录 185 文件 / 1169175 字节，含本里程碑的独立复核报告 `review-independent.md`）、9 条判据的原始 stdout/stderr 与退出码、native 侧 72 行纯逻辑探针报告（摘要 `c3f0da6b37577e55`，与 M0 归档的那份**逐字节相同**——M1 往 core 里加了一整个渲染模块，这就是"探针契约没被碰坏"的直接证据）。

- ✅ 帧文件名三位补零（`{scene}-f{frame:03}.png`）：字典序 == 帧号序，`ls` 一遍就是时间顺序
- ✅ `records/m1/` 只归档一次运行的字节（第二条腿）；跨进程那一半靠 `compare.json` 的**双侧摘要** + 记录里的可重跑命令立住
- ✅ 记录里不装干净：`run.json` 声明 `nondeterministic_fields: []`，时间戳全住进 `adapter.json` / `timing.json`，两份各自声明自己的非确定项

### 退出标准

- [ ] 目标环境（含 Linux 容器）能跑出 PNG，且重复运行**逐字节一致**
  - ✅ Windows 两条腿达成：同进程（每帧渲染两次）与跨进程（`--compare-run`）都逐字节一致，两腿整表摘要同为 `71ecc80cade3d73d`
  - ⏳ Linux 容器未跑（本机无 docker、WSL 无发行版），**这一条没有勾**
- [ ] 四种环境（Win/DX12、Win/Vulkan、Linux/GPU、Linux/lavapipe）的 adapter 与通过情况全部记录 —— 当前 **2/4**
  - ✅ `records/m1/dx12/` 与 `records/m1/vulkan/` 的 5 份文件 + 80 张 PNG 已归档
  - ⏳ Linux 两条腿的记录缺（同上）
- [x] 1080p 单帧渲染 ≤ 10ms（不含读回）——**起始值，按实测定档** —— ✅ 实测后 `FRAME_BUDGET_MS` 仍留 10 ms：判的是「渲染 + 读回」往返（含 GPU，数字比"纯渲染"更大，是**高估**），最慢 2.289 ms；不含 GPU 的 CPU 提交另记 0.124 ms。预算不参与退出码

> 第 1 条是这一步真正的目的：headless wgpu 出图**稳定可复现**。而它最不确定的部分从来不是渲染逻辑，
> 是**容器里的 GPU 注入**——所以 Windows 两条腿只是把链路先钉住了，Linux 两条腿（上面两处 ⏳）
> 才是这条判据真正的考点。**2026-09-22 用户明确决定：Linux 两条腿延期（⏳），以 Windows 两条腿的
> 结论先进 M2**；有 Linux 环境时补跑并回填 ① ②（补跑命令见 `records/m1/README.md`「怎么重跑」）。
> 本机部分已由 `node scripts/record-acceptance.mjs --milestone m1` 落进 `records/m1/acceptance.json`
> （9 条判据全绿，每条都留了原始 stdout/stderr 与退出码）。

### 明确不做

FFmpeg 绑定、解码、时间线、分片、任务队列。

### 风险与提示

| 风险 | 对策 |
|---|---|
| 云 GPU 贵 | 本机能做的全部做完再开云机；用 spot |
| 容器缺 Vulkan ICD | 先跑通 lavapipe 证明链路，再解决 GPU 注入（NVIDIA Container Toolkit） |
| 读回格式/对齐踩坑 | 256 字节行对齐 + 通道序检查，写成工具函数复用 |

---

## 5. M2 —— 双运行时同帧 SSIM 比对（架构命门）

> **目标**：同一份 WGSL，浏览器 WebGPU 与服务端 headless wgpu 渲染同一帧，SSIM 在容差内一致。
> **这一步花不了几天，但决定了后面半年是"一份代码两个运行时"还是"两份实现互相追赶"。**

**前置**：M1（native 出图）+ M0 的 wasm 壳。

### 设计要点（先看，再动手）

1. **比对必须隔离解码差异**：M2 的输入是**合成帧**，不引入任何视频解码。唯一变量是"渲染图 + WGSL + 编译器 + GPU"。真实素材解码带来的差异留到 M4 单独处理——混在一起会让归因变成玄学。
2. **两端都不从 canvas 抄像素**：wasm 侧同样渲染到**离屏纹理**再 `copy_texture_to_buffer`（canvas 纹理通常没有 `COPY_SRC`；这同时正好验证 `FrameSink` 抽象——"预览与导出共用渲染图，只是 sink 不同"）。
3. **比对前统一像素格式**：两侧都输出 `Rgba8UnormSrgb` 的 PNG，比对在编码后的字节上进行，避免把色彩空间问题再引入一次。

### 任务

- [ ] **T2.1 core 落地 corpus 场景**（从 M1 搬入 `dhampir-core`，两个宿主共用）
- [ ] **T2.2 native 侧出图**（M1 已具备）
- [ ] **T2.3 wasm 侧出图**
  - `dhampir-wasm` + 最小 HTML/JS 驱动（先手工跑通，自动化留到 M6）
  - 需要浏览器带 WebGPU：Chrome/Edge 113+ / Safari 26+ / Firefox 141+(Windows)
- [ ] **T2.4 比对工具 `tools/dhampir-framediff`**（workspace 内小 crate）
  - 输入两组 PNG 目录 → 输出：逐帧 SSIM + PSNR + 最大绝对差 + **放大差异图 PNG** + `summary.csv`；超阈值退出码非 0
  - 建议**不引 Python**：CI 里少一个依赖，工具本身也进 Cargo workspace 统一管理
  - 阈值文件 `thresholds.toml` 按场景分档
  - **初始建议值**：mean SSIM ≥ 0.995、min ≥ 0.98（灰度上算，8×8 或 11×11 高斯窗）——**只是起点，必须按实测定档**
- [ ] **T2.5 差异归因与固化**
  - **结构性差异（边缘错位、色块偏移）一律当 bug 修**，不接受"浮点误差"解释
  - 均匀低幅噪声 → 记录进"容差说明"，不进 bad case
  - 产出 **《WGSL 可移植性子集》**：允许 / 禁止的构造 + 替代写法。已知高危区：
    - 依赖 `fract` / `pow` 极限行为的写法
    - `fwidth` / 导数依赖（实现自由度大）
    - 长链累加（顺序敏感）
    - 非规格化数与精度限定（优先 f32，避免隐式 mediump 行为假设）
    - 纹理采样 LOD 语义差异
- [ ] **T2.6 跨机比对**：把浏览器换到与服务端不同厂商的 GPU/驱动组合，重复 T2.4
  - 这才是真实部署场景（用户设备千奇百怪，服务端又是一家）

### 退出标准

- [ ] 5 个 corpus 场景在同一对运行时上 SSIM **全部达标**（同机）
- [ ] 跨厂商 GPU 组合下**不出现结构性差异**
- [ ] 《WGSL 可移植性子集》第一版落文件
- [ ] 差异归因清单完成：每条已知差异都有"是否可接受 + 理由 + 是否进 bad case"

### 风险与提示

| 风险 | 对策 |
|---|---|
| 双编译器（Tint vs naga）边界差异比浮点误差更隐蔽 | 能力下限写法（指导文档 §4.3①）+ 子集清单 + 容差 |
| 浮点不确定性跨 GPU | 容差必须设，禁止要求 1.0；优先 f32、避免运算顺序依赖 |
| 归因困难 | 一次只改一个变量；`--dump-raw`（M4 引入）思路在这里就要用上 |

> **证伪点**：若在合理容差内无法收敛 → 触发 Plan B（§11.5）。

---

## 6. M3 —— 浏览器预览链路

> **目标**：proxy 硬解 → 零拷贝进 GPU → 渲染进 canvas，拖动 scrub 手感可用；并**定下"源帧采样策略"**。

**前置**：M0。需要一个 proxy 素材（本阶段用 FFmpeg **CLI** 生成，还不引绑定）。
**可与 M1/M2 并行**（第二人力切入点）。

### 前置 spike（先做，结论必须落文件）

- [ ] **S3.1 源帧采样策略三选一 —— 本里程碑最重要的决策**

  三条已核实的硬约束（W3C WebGPU 规范 / webgpufundamentals）：

  1. 外部纹理在 WGSL 里**只能用 `texture_external` 绑定**，采样**只能用 `textureSampleBaseClampToEdge`**：没有 mipmap、没有 repeat（要 repeat 得自己 `fract`）
  2. `VideoFrame` 来源的外部纹理**在 VideoFrame `close()` 时失效**（HTMLVideoElement 来源才是"当前 task 结束即失效"）——我们的路径是 VideoFrame
  3. 跨源视频不是 origin-clean 会直接抛 `SecurityError`

  三个候选：

  | 选项 | 做法 | 代价 |
  |---|---|---|
  | (a) **零拷贝外部纹理** | core 用 shader 组合层（如 `naga_oil` 的 `#ifdef`）生成两版 WGSL：native 用 `texture_2d` + `textureSample`，wasm 预览用 `texture_external` + `textureSampleBaseClampToEdge` | 采样自由度受限（无 mip/repeat）；WGSL 文本产生分叉（但逻辑单一来源） |
  | (b) **`copyExternalImageToTexture`** | 每帧一次真拷贝，换成普通 `texture_2d` | 放弃零拷贝；换来两侧 WGSL 完全同文、采样语义完全自由 |
  | (c) **混合** | 默认 (a)，遇到需要 mip/repeat 的特效切 (b) | 两套路径都要维护 |

  **决策依据**：1080p 下实测 (b) 的拷贝成本 vs (a) 的约束成本。结论写进 `plan/` 下的决策文件。

- [ ] **S3.2 proxy 生成规格**（参考命令，参数按实测调）
  ```
  ffmpeg -i src.mp4 -vf scale=-2:720 \
         -c:v libx264 -preset veryfast -g 60 -keyint_min 60 -sc_threshold 0 -crf 23 \
         -c:a aac -b:a 128k proxy.mp4
  ```
  - `-g 60 -keyint_min 60 -sc_threshold 0`：60fps 素材下 **1 秒 1 个 I 帧**，禁掉场景切换产生的额外关键帧
  - **关键帧密度直接决定 seek 手感，比分辨率重要得多**
  - 同时产出封面图 + 雪碧图（时间线缩略图带）：`-vf fps=1/5,scale=160:-1,tile=10x10`
- [ ] **S3.3 VideoFrame 生命周期约定**
  - RAII 包装，**在 `queue.on_submitted_work_done()` 之后再 `close()`**——规范上说 VideoFrame 关闭即外部纹理失效，比"提交后立刻 close"稳
  - 不及时 `close()` 会耗尽显存，长视频/多轨场景必爆

### 任务

- [ ] **T3.1 `web/` 骨架**：Vite + React + TS；加载 wasm-pack `--target web` 产物
- [ ] **T3.2 demux + 解码**：mp4box.js 取样本 → `VideoDecoder` → VideoFrame
  - **只解 H.264，不引 FFmpeg WASM**（有意取舍：WASM 那 0.3x 性能和几十兆包体积不值得，异构格式交服务端）
- [ ] **T3.3 上屏链路**：`import_external_texture` → core 渲染图 → canvas surface
  - wasm 侧实现 `FrameSource` / `FrameSink`
- [ ] **T3.4 帧缓存**：LRU + 显存上限（参考 MASterSelects 的 300 张 VRAM 纹理）+ RAM 预览缓存（参考 900 帧）
- [ ] **T3.5 测量**：seek p50/p95、播放丢帧率、解码 → 上屏延迟、显存/内存曲线

### 退出标准

- [ ] 1080p proxy **全速播放 60fps**，无 FFmpeg WASM 回退路径
- [ ] 拖动 scrub p95 ≤ 50ms ——**起始值，按实测定档**
- [ ] 4K 源素材下浏览器内存/显存不越界（上限 + LRU 淘汰生效）
- [ ] S3.1 决策文件落盘，含实测数字

### 明确不做

时间线 UI（M4 之后的事）、多轨、特效面板、导出。

### 风险与提示

| 风险 | 对策 |
|---|---|
| 跨源 VideoFrame 受 CORS 限制 | proxy 与编辑器同源，或配好 CORS |
| Firefox Linux WebGPU 仍在 flag 后 | 能力检测 + 降级提示；`gfx.webgpu.ignore-blocklist` |
| 老 Safari（<16.4）无 WebCodecs | 能力检测 + 降级提示 |
| 4K 三层缓存吃满内存 | 上限 + LRU；显存预算 |

---

## 7. M4 —— 契约闭环：时间线 → 服务端出片

> **目标**：时间线 JSON 驱动两端；服务端出片与浏览器预览在同一阈值内一致。
> **这是"编辑器"真正成立的地方**——M3 之前只是播放器。

**前置**：M2（渲染一致性机制已验证）+ M3（浏览器侧有渲染路径）。

### 任务

- [ ] **T4.1 schema v1 定稿（先定契约，再写两边）**
  - Rust 类型（serde）+ 校验：轨道内重叠、`source_in + duration` 越界、效果参数范围、timebase 兼容性
  - 校验错误输出**结构化 JSON**，UI 能直接渲染成人话
  - TS 类型生成：`schemars` 出 JSON Schema → `json-schema-to-typescript`（备选 `typeshare`）
  - `schema` 版本号 + 迁移策略：不兼容就 +1，服务端**拒绝未知版本**
  - 铁律（指导文档 §5.1）：**时间用帧号，不用浮点秒**；特效走"类型 + 参数"声明式，不做可上传 shader
- [ ] **T4.2 `dhampir-core` 渲染图 v1**
  - 多轨合成、transform / opacity、**1 个特效**（建议 `gaussian_blur`）、`cross_dissolve` 转场、关键帧 + easing
  - **所有时间参数从整数帧号推导，不碰 wall clock**；easing 公式写死并保证两端一致
  - 特效注册表：`type` 字符串 → pipeline + 参数 schema（同一份 schema 给 UI 生成控件）
- [ ] **T4.3 wasm 宿主接时间线**（替换 M3 的单片段路径）
- [ ] **T4.4 worker：时间线 → 渲染 → 编码 → mux**
  - **必做开关 `--dump-raw`**：导出编码前的原始帧。**这是隔离"渲染差异"和"编码差异"的唯一手段**，不是可选项——M2/M4/M5 的每一次不一致排查都要靠它
  - 音频走 FFmpeg filtergraph；音频模型保持最小（音量关键帧、淡入淡出、静音、变速）——**别在 v1 做 DAW**
  - **服务端一次性 mix 音频，音频不分片**（分片是视频的事，见 M5）
- [ ] **T4.5 样本工程 + 双端比对**
  - 样本：3–5 片段 + 1 转场 + 2 特效 + 关键帧
  - **本阶段比对含真实解码，期望值会比 M2 低**，原因必须记录清楚：
    - 浏览器外部纹理走**浏览器的** YUV→RGB（自带矩阵/LUT 与色度上采样策略）
    - 服务端走 FFmpeg 解码 + **自己实现的**转换
    - 差异是必然的 → 处理方式：把转换尽量对齐 → **阈值单独标定**（不与 M2 混用）→ 差异样本进 bad case 库

### 退出标准

- [ ] 样本工程端到端出片；时长与 timebase **完全一致**；音画同步 ≤ 1 帧
- [ ] `--dump-raw` 可复现
- [ ] 含解码的双端 SSIM 达标（阈值单独标定并在文档里说明来源）
- [ ] schema v1 + TS 类型 + 校验错误格式**冻结**（改动需 +1 版本号）

### 风险与提示

| 风险 | 对策 |
|---|---|
| 色彩范围/矩阵配错（画面发灰或过饱和） | 最容易被忽略；把两侧转换矩阵与色度上采样策略写成文档，逐项对齐 |
| 音频两端各实现一次 | 音频浮点容忍度高，不做逐样本比对；但参数语义必须一致 |
| schema 反复改 | 先定契约再写代码；破坏性改动一律 +1 |

---

## 8. M5 —— 分布式分片渲染

> **目标**：GOP 边界分片并行渲染 + merge，结果与整体渲染完全一致。
> **帧精确 ⇒ 可任意分片且结果一致**——这是分布式能成立的唯一前提。

**前置**：M4 退出标准全绿。

### 任务

- [ ] **T5.1 分片逻辑**
  - 按 **GOP 边界**切（4 秒/片，初值）
  - 每片独立随机访问源帧——依赖 proxy 的密集关键帧 + 帧号定位（这就是 §5.1 坚持"用帧号不用浮点秒"的兑现处）
- [ ] **T5.2 合并**
  - 所有分片用**完全相同的编码参数 + 闭合 GOP**：`keyint=120:min-keyint=120:scenecut=0:open-gop=0`
  - concat demuxer `-c copy`（备选：NAL 级拼接）
  - **音频单独一次编码到底，最后 mux**
- [ ] **T5.3 编排 v0**
  - 任务表 + `SELECT ... FOR UPDATE SKIP LOCKED` 取任务（Postgres；v0 允许先单机多进程，先验证正确性再分布式）
  - **每 5 秒 checkpoint**；心跳丢失即重排
  - worker 无状态容器，spot 占 80%
- [ ] **T5.4 等价性测试（本里程碑的核心验收）**
  - 整体渲染 vs 2/4/8 分片，逐帧比对：先比 `--dump-raw`（纯渲染层），再比成品解码帧（含编码）
  - `ffprobe` 校验**帧数 + PTS 连续**，确认无丢帧/重复帧
- [ ] **T5.5 性能**：33 秒素材端到端（参考：腾讯同类实现 9 秒级）；目标 **≥ 1x 实时**起步，拉伸到 ≥ 3x
- [ ] **T5.6 故障注入**：`kill -9` worker → 重排 → 输出仍与整体一致

### 退出标准

- [ ] 4 分片 == 整体（`--dump-raw` 逐帧达标；成品解码帧在编码容差内）
- [ ] 无边界丢帧/重复帧（帧数 + PTS 校验通过）
- [ ] checkpoint 恢复验证通过
- [ ] 单 GPU 吞吐基线记录（供容量规划）

### 风险与提示

| 风险 | 对策 |
|---|---|
| 分片边界产生编码瑕疵 | 闭合 GOP + 固定 keyint + `scenecut=0`；边界帧逐个核对 |
| 编排复杂度吃掉工期 | v0 单机多进程先验证等价性，分布式其次 |
| 无状态 worker 的中断 | checkpoint + 心跳重排；用故障注入测 |

> **证伪点**：分片与整体不一致且无法在编码容差内解释 → 检查 GOP 闭合与时间戳；最坏情况退回"单片 = 整片"（放弃分布式吞吐收益）。

---

## 9. M6 —— 一致性保障与发布流程

> **目标**：SSIM 比对进 CI + 影子环境闸门；把"一致性"从一次性验证变成长期自动保障。

**前置**：M2（比对工具）+ M5（完整出片链路）。

### 任务

- [ ] **T6.1 CI 闸门**：编译（Win + Linux）+ headless 渲染 corpus + framediff 对 golden
  - **必须写清的权衡**：lavapipe 便宜，但浮点路径与真 GPU 不同 →
    - lavapipe 只适合"结构正确性"闸门
    - SSIM 闸门要么用**自托管 GPU runner**，要么给 lavapipe **单独标定阈值**。二选一，别混
- [ ] **T6.2 golden 资产**：样本工程 + 预期 MP4 + **bad case 库**（每条线上事故入库并附 issue 链接）
- [ ] **T6.3 影子环境**：发布前抽样跑线上数据比对，全过才允许发布
- [ ] **T6.4 阈值表 + 差异说明文档定稿**（按场景分档，标注标定时间与环境）

### 退出标准

- [ ] PR 闸门能拦住 SSIM 回归（人为造一次回归验证拦截生效）
- [ ] 一次完整影子发布流程走通

---

## 10. M7 —— 产品化（占位，暂不展开）

**前置 = M4 结论。** 在 M4 之前展开是浪费——契约与一致性机制没验证完，产品层所有设计都可能推倒。

范围预告（不承诺顺序）：

- 上传 → probe → 分流 → proxy 管线（服务端）
- 对象存储：原片 / proxy / 封面图 / 雪碧图 / 成片
- 导出 API + 预设；任务查询与错误面
- 前端时间线 UI：**视图中控**（点击生成拖拽实体、影子元素渲染、松手才更新轨道数据）+ 本地/云端双模式（指导文档 §6.2）

---

## 11. 横切事项

### 11.1 CI 蓝图

| 阶段 | Job | 触发 | 说明 |
|---|---|---|---|
| M0 起 | `check-native`：`cargo check --workspace` + `cargo test` | 全部 PR | Win + Linux |
| M0 起 | `check-wasm`：`cargo check -p dhampir-wasm --target wasm32-unknown-unknown` | 全部 PR | 只 check wasm crate，不用 `--workspace` |
| M0 起 | `guard`：core 目录无 `#[cfg(` / 依赖方向检查 | 全部 PR | 用脚本 grep，简单有效 |
| M2 起 | `render-corpus` + `framediff` | 全部 PR | 见 T6.1 的 lavapipe 权衡 |
| M4 起 | `e2e-sample`：样本工程出片 + 成品比对 | nightly / 发布前 | 重，别放 PR 路径 |
| M6 起 | `shadow` | 发布前 | 抽样线上数据 |

### 11.2 测试资产清单（长期累积）

- **合成 corpus**：5 个确定性场景（M1 建，M2 用，M6 进 CI）
- **样本工程**：3–5 片段 + 转场 + 特效 + 关键帧（M4 建）
- **golden**：预期 MP4 / PNG 与其 `adapter.json` 环境记录
- **bad case 库**：每条真实事故，附最小复现 + 结论

### 11.3 必须产出的文档（交接资产）

| 文档 | 产出于 | 作用 |
|---|---|---|
| 《WGSL 可移植性子集》 | M2 | 让后续每个新特效都写在安全区内 |
| proxy 生成规格 + 命令 | M3 | 素材管线的一致性来源 |
| 源帧采样策略决策（外部纹理 vs 拷贝） | M3 | 影响 core 的采样接口形状 |
| 时间线 schema v1 + TS 类型 | M4 | 前后端唯一契约 |
| SSIM 阈值表 + 差异说明 | M2 / M4 / M6 | 判断"是否回归"的唯一依据 |

### 11.4 时间与成本粗算

**人日口径**：1 名熟悉 Rust + 图形编程的全职人力。

- M0 + M1 + M2：约 **6–11 人日**——这是整个项目的技术风险集中区，做完就有 80% 的确定性
- M3：1–2 周 ｜ M4：2–3 周 ｜ M5：2–3 周 ｜ M6：1 周 + 持续
- **合计到 M5：约 8–12 周（单人）**
- **并行方案**：第 2 人力从 M3 切入 → 约 **6–8 周**（M3 与 M1/M2 无依赖）

**成本项**

- 开发期：本机为主，云 GPU 按小时零星开销（M1 起）
- M5 起：持续 GPU 开销（spot + 优先级路由）；若要压成本上 CPU 软渲染，性能掉一个数量级
- 长期：headless GPU 容器化本身是持续维护成本（Vulkan ICD / 驱动版本漂移）

### 11.5 证伪点与 Plan B

每个闸门都必须预先写好"不过怎么办"，否则闸门会变成一句口号：

| 闸门 | 触发条件 | Plan B（按优先级） |
|---|---|---|
| **M1** | headless wgpu 在目标云环境不可用 | 换实例类型 / 换 GPU 厂商 → lavapipe 降级（性能掉一个数量级，只适合小规模） |
| **M2** | 双编译器差异无法在合理容差内收敛 | (a) 收缩 WGSL 子集（最可能够用）；(b) 问题算子改成 LUT 纹理或两端同一实现的 CPU pass；(c) 兜底：服务端改走 FFmpeg filtergraph——**特效写两遍且长期互相追赶，这是要尽量避免的结果**；(d) (c) 之后的选项：服务端改用 headless 浏览器 + WebGPU 农场（实现唯一，但成本与吞吐更差，值得评估） |
| **M4** | 含解码的差异过大 | 收紧色彩管线：统一转换矩阵 + 色度上采样策略；或预览侧改走 `copyExternalImageToTexture` 与 native 对齐（牺牲零拷贝换一致） |
| **M5** | 分片与整体不一致 | 核对 GOP 闭合与时间戳 → 最坏退回"单片 = 整片" |

---

## 12. 交接说明（给下一个会话）

**开始工作前**：

1. 读指导文档「命名与项目定义」+ §0 + §9（理解边界与决策）
2. 读本文件 §0–§2（用法、总览、环境）+ 你要做的那个里程碑章节
3. 检查上一里程碑的退出标准是否真的全绿（别信勾选，信产物）

**工作结束时**：

- 勾掉本文件的对应项；把新发现写进对应里程碑的「风险与提示」或 §11.3 的文档清单
- 若推翻了某个决策：**改指导文档，别只在代码里改**——指导文档是这个项目的决策真相
- 若某条验收被证明不可达：改本文件并注明原因与替代标准

**第一个动作（时间敏感）**：M0 T0.1 —— crates.io 占名。名额不可回收，越早越好。
