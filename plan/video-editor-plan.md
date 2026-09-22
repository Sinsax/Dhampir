# dhampir · 落地执行计划（M0–M7）

> **配套文档**：`plan/video-editor-tech-guide.md` —— 技术指导回答"为什么这么选"，本文件回答"从哪开始、每步做什么、怎么算做完"。
> **目标产物**：**技术调研结论 + 一个底座**（渲染与预览的共用内核），不是完整软件 ｜ 名称：**`dhampir`** ｜ 底座：Rust + wgpu 双运行时
> **核心命题**：同一份工程，浏览器里看到的 = 服务器上导出的（可比的帧）
> **形态**：素材在服务器，渲染导出由服务器处理，浏览器只做剪辑编辑处理——本质是 **render 与 preview 的分别处理**
> 制定日期：2026-09-22 ｜ 状态：待执行

**一句话路线**：M0 立骨架 → M1 render 侧出图 → **M2 双运行时同帧比对（架构命门）** → M3 preview 侧预览 → M4 契约闭环。
（原路线里的 M5 分片 / M6 保障 / M7 产品化**已移出本仓库**，见 §2「范围」与 §1 总览的就地标注。）

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

**范围**（详见 §2）：本仓库交付**技术调研结论 + 一个底座**（预览/渲染共用内核）。
下表带**当前状态**；⛔ / ⚠️ 的行已移出本仓库，内容见 §8–10 与附录 B。

| # | 里程碑 | 目标（一句话） | 状态 | 前置 |
|---|---|---|---|---|
| **M0** | 骨架与双编译贯通 | 同一份源码在两个 target 上编译并输出一致 | ✅ **已收官**（退出标准 3/3） | 环境准备 |
| **M1** | 服务端 headless wgpu | 目标环境能离屏出图且可复现 | ✅ **已收官**（1/3；Linux 两条腿延期，属已知缺口） | M0 |
| **M2** | **双运行时同帧 SSIM**（架构命门） | 同一份 WGSL，两端渲染同一帧结果一致 | ✅ **已收官**：退出标准 4/4 ｜ 记录自证 `EXIT=0` ｜ 验收快照 13/13 ｜ **独立复核「通过」** | M1 + M0 的 wasm 壳 |
| **M3** | 浏览器预览链路 | proxy 硬解 → 拷贝上 GPU → 出画面 | 🔵 **进行中**：**S3.1 / S3.2 / S3.3 三个前置 spike 全部已决**；T3.x 未开始 | M0（可与 M1/M2 并行） |
| **M4** | 契约闭环 | 时间线 JSON 驱动两端，出片与预览一致 | ⬜ **未开始**（T4.4 编码/mux 归下游） | M2 + M3 |
| ⛔ **M5** | 分布式分片渲染 | 分片结果 == 整体渲染 | **移出范围** → 附录 B | — |
| ⚠️ **M6** | 一致性与发布保障 | SSIM 闸门（留）/ 影子环境与发布（移出） | **部分移出** → 附录 B | M2 |
| ⛔ **M7** | 产品化 | 上传→proxy→存储→导出→前端 UI | **移出范围** → 附录 B | — |

**关键路径**：M0 → M1 → M2 → M4。（原为 …→ M5，M5 移出后终止于 M4）

**可并行**：M3（浏览器侧）不依赖 M1/M2 的服务端结论，只依赖 M0 的骨架 + 一个 proxy 素材，因此**第二人力从 M3 切入收益最高**。

**顺序的理由**（指导文档 §9.2）：先在服务端把 `dhampir-core` 写对——服务端能 dump 帧、能单步、能跑比对；但**第一天就要把 wasm 空壳跑起来**，它的作用是当场暴露 native-only 的 API 使用。

### 1.1 当前进度（截至 2026-09-22，全部实核）

**已做完**：

| 项 | 证据 |
|---|---|
| M0 | `records/m0/` |
| M1 | `records/m1/`；`check-m1-record.mjs --record records/m1` → EXIT=0（160 张 PNG 逐张重算一致） |
| M2 工程活 | `records/m2/` 230 个文件；本文件 §5 退出标准 4/4 已勾 |
| **M2 记录自证** | `check-m2-record.mjs --record records/m2` → **EXIT=0**（60/60）；`--self-test` → **EXIT=0**（53 条断言 / 37 项反向用例全覆盖） |
| **M2 验收快照** | `records/m2/acceptance.json`：**13/13 全绿、`dirty=false`、commit `a35f535f0906`**，每份判据都留了原样 stdout/stderr |
| 底座依赖方向 | `check-dep-graph.mjs` → EXIT=0（5 个 crate，无环） |
| 文本卫生 | `check-text-hygiene.mjs` → EXIT=0（70 个文件） |

**M2 收尾 4 件（4/4）✅**：

| # | 任务 | 状态 |
|---|---|---|
| P0-1 | 守卫可执行化（`runSelfTest()` + 37 条反向用例 + `main()` + `process.exitCode`） | ✅ **已完成** |
| P0-3 | 按主题拆提交 | ✅ **已完成**（7 个提交） |
| P0-2 | 验收快照 `acceptance.json` + 13 份判据 | ✅ **已完成**（13/13、`dirty=false`） |
| P1 | 独立复核 → `records/m2/review-independent.md` | ✅ **已完成**：**18471 字节**，结论**通过** |

**本轮共修掉 6 个缺陷**。前 4 个只在「真的执行守卫」之后才暴露（守卫此前从未跑过）；后 2 个是**独立复核实证出来的**：

来源：① 直接执行；② [`records/m2/review-independent.md`](../records/m2/review-independent.md)（独立复核报告，结论「通过」，附 7 条发现 P1–P7）

| # | 缺陷 | 性质 |
|---|---|---|
| 1 | 截图 json 的 `leg.slug` 被拿**目录名**去比（记录里写的是记录内腿名 `m2`） | 守卫与产出工具的契约不一致 |
| 2 | `browser.revision` 被要求是**整数**（CDP 给的是字符串 `@792bf67…`） | 同上 |
| 3 | `rerun-repro.json` 的 `question` 键不被 `KEYS` 接受 | 同上 |
| 4 | **自指死锁两处**：守卫自检的基线含 `acceptance.json`（而它的一条判据就是「跑守卫」）；验收工具边跑边写 `.txt`，让守卫在自己的判据上必然看到「半份记录」 | 设计缺陷 |
| 5 | **复核 P1**：`acceptance` 单向——只验「有没有承认失败」，不验 `exit_code`/`ok`/`commit` 与判据原文。伪造的「全绿声明 + 红掉的原文 + 假 commit」能拿 60/60 `EXIT=0` | **守卫可信度漏洞** |
| 6 | **复核 P2**：覆盖率断言**自指**——`ALL_CHECK_IDS` 与 `MUTATIONS` 同源互比，「同时删掉一个检查项与它的反向用例」能骗过自检（副本 36/36 绿、真跑 58/58 绿） | 同上 |

**P1 / P2 的修补与实证**：

- **P1**：`record-acceptance.mjs` 把退出码写进每份 `.txt` 头两行（`$ <命令>` / `exit: N`），守卫逐条拿原文与 `acceptance.json` 对账，并校验 `commit` 格式。
  实测：把 `.txt` 的 exit 行与正文改成红的、JSON 照旧说全绿 → 守卫 **`EXIT=1`**，点名「`guard-m2-record.txt` 写 exit: 1，acceptance.json 记 0」。
- **P2**：各族 id 清单、检查项总数（37）、断言条数（59）全部钉死。
  实测：复现复核的同一手法（同时删掉 `pixels` 与其反向用例）→ 自检 **`EXIT=2`**，点名三处锚不一致，真跑也拒跑。
- **残留边界（写进了守卫代码注释）**：`acceptance` 项**不重跑**那 13 条判据（其中两条就是「跑守卫」，重跑会成环），
  所以「守卫绿」只等于**快照自洽**，不等于**快照诚实**——判据原文在 `records/m2/*.txt`，人得自己看。复核报告已把这一步做完。
- **未处置的发现**：P3（`readme-claims` 只是字面量存在性检查）、P5（SSIM/PSNR 未由复核者独立重算，只做了 |Δ|/差异像素侧证）、
  P6（`wasm-tests.json` 内嵌机器本地绝对路径，泄露用户名且不可照搬复现）——三条都属「工具/记录的天花板」，已在复核报告里写明，**不假装解决**。

⚠️ **两条顺序约束（都是实测出来的，别再颠倒）**：

1. 守卫没有 `main()` 之前跑验收会产出**假绿快照**（已解除）。
2. **提交（P0-3）必须排在验收（P0-2）之前**：`acceptance.json` 的 `dirty` 剔除 `records/`，
   树脏就会写出 `dirty: true`，守卫按红判（`dirty=true——记录是在一棵脏树上跑的`）。

**下一步**：M3 的 T3.x（web 骨架 / 硬解 / 上屏 / 帧缓存 / 测量）——三个前置 spike 已全部有结论；然后 M4。
逐条交接见 [`remaining-work.md`](./remaining-work.md)。

**M2 收官口径（三条同时成立才算）**：① `--record records/m2` → `EXIT=0`；② `--self-test` → `EXIT=0`；
③ `acceptance.json` 13/13、`dirty=false`；④ `records/m2/review-independent.md` 存在且结论为「通过」。**四条现在都成立。**

### 1.2 本轮采纳的架构建议

来自 [`foundation-architecture.md`](./foundation-architecture.md) §9，已并入本计划：

| # | 建议 | 落点 |
|---|---|---|
| **R1** | **先定 S3.1 再写 M3 代码**——它决定底座能否零拷贝 | ✅ 已决：见 [`s3.1-source-frame-sampling.md`](./s3.1-source-frame-sampling.md) |
| **R2** | **暂不拆取证脚手架**（占仓库过半体积），等 M2 记录冻结后再拆 | §11 |
| **R3** | 钉死**唯一跨边界契约 = timeline JSON**；`FrameSource`/`FrameSink` 不跨进程 | M4 T4.1 |
| **R4** | 给 `dhampir-media` 标注「未验证契约」（五个 trait 零实现） | §11.3 |
| **R5** | 补 `check-m2-record.mjs` 的 `main()`（= P0-1） | §5 收尾 |
| **R6** | `scene.rs` 的渲染器与场景规格先加注释分界 | §11 |

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
| ~~云 GPU 实例（spot）~~ | ~~M1 之后才开~~ | ⛔ **移出范围**：属下游工程的部署形态，本仓库只做本地 |

**范围**

- 本项目**只做本地开发与本地验证**：不发布到 crates.io、不建远端仓库、不接远端 CI。
  下面所有任务的验收口径都是「本机能跑出绿」，不依赖任何远端服务。
- 交付物是**技术调研结论 + 一个底座**，不是完整软件。**不需要服务器的整体架构。**

| | 内容 |
|---|---|
| **交付（底座）** | `dhampir-timeline` / `dhampir-media`（纯契约 trait）/ `dhampir-core`（渲染图 + WGSL + 特效）+ 两个宿主 `dhampir-wasm`（preview）/ `dhampir-worker`（render）；以及调研结论文档、守卫与比对工具 |
| **不交付（下游形态）** | API 网关 / 任务队列 / 对象存储 / 容器与部署（Vulkan ICD、spot）/ 分布式分片编排 / 影子环境与发布流程 / 编辑 UI 产品化 / FFmpeg 编码与 mux 的具体实现 |

- 下游工程可以拿这个底座接成两种形态：**客户端-服务端分离**，或**本地预览 + 渲染的合并处理**。
  底座必须对这两种形态都中立——所以契约里不预设网络、不预设进程边界。

---

## 3. M0 —— 骨架与双编译贯通

> **目标**：把 workspace 骨架立起来，让双编译链路当天就暴露问题。**这一步不写任何功能。**

**前置**：§2 环境准备。

### 任务

- [x] **T0.1 命名落地（已收口）** —— 引擎名 `dhampir` 已定并落地到 crate 名 / 包名 / 文档
  - ✅ 已备好：`LICENSE-MIT` / `LICENSE-APACHE`（Apache 全文含 APPENDIX）、根 `README.md`（写明项目意图、crate 地图、版本对齐纪律），四个 crate 都是**能 `cargo build` 的真实最小 crate**，不是空占位
  - ✅ **发布与建仓已移出范围**（见 §2「范围」）：不发 crates.io、不建远端仓库、不查域名。
    因此 `[workspace.package] repository` 保持**不填**（不编造 URL），`cargo publish` 不做。
    这是**移除**，不是完成——别把它当成已验收的产物
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
- [x] **T0.6 本地检查清单** —— ✅ 已写 `.github/workflows/ci.yml`（`check-native` / `check-wasm` / `guard` 三个 job）
  - 见 §11.1，先只跑三条 check，不含 GPU 任务
  - ⚠️ **不在远端 runner 上跑**（只做本地处理，不建远端仓库）：该文件当**本地命令清单**用，
    人在本机按同样顺序跑一遍即可，不承诺任何远端绿灯
  - ✅ 已在本机预验最易首跑就红的那一路：`cargo check --workspace --target x86_64-unknown-linux-gnu` → 退出码 0。`cargo check` 不链接，所以这条能真正证明 Linux 的 cfg 分支都在（`dhampir-worker` 的 `vulkan`/`dx12`/`metal` feature 在 Linux 上确实编得过，靠的是 wgpu 自己的 `wgpu-core-deps-windows-linux-android`）
  - ✅ **没有任何 `continue-on-error` / `|| true`**：忽略退出码就是把守卫改成永远绿，而"永远绿"通常就是这样开始的
  - ✅ wasm-bindgen CLI 的版本从 `Cargo.lock` 读（`node scripts/run-wasm-tests.mjs --print-locked-version`），不在 workflow 里写死第二份

### 产出物

可编译 workspace + 三条绿线 + 本地检查清单（`.github/workflows/ci.yml`，本机跑，不接远端）。

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
| ~~云 GPU 贵~~ | ⛔ **移出范围**：那是下游工程的部署成本，本仓库只做本地 |
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

- [x] **T2.1 core 落地 corpus 场景**（从 M1 搬入 `dhampir-core`，两个宿主共用）
  - ✅ 搬进去的不只是「场景数据」，而是**整条驱动 + 记录形状**（`6378243`）：`render/corpus.rs`
    持有「一帧怎么渲染、怎么判、整表摘要怎么算、记下来的 JSON/TXT 长什么样」；宿主只剩
    「把字符串写进文件」——native 落盘，浏览器交给 JS。理由是下一步那条腿要和这条腿比字节：
    两个宿主各拼一遍 JSON，比出来的就不只是渲染差异，记录本身成了第二个变量。
  - ✅ 搬完的**证伪**是重跑而不是「看着没坏」：`--scene all --frames 0..16 --out target/m2-p2`
    与 M1 归档 `records/m1` 比 169 个文件 → **165 个逐字节相同**（两条腿的 `run.json`、
    `readings.txt`、160 张 PNG、`selfcheck-native.txt`）；剩下 4 个（每条腿一份 `adapter.json` /
    `timing.json`）只差记录**自己声明过**的非确定字段，没有未归因差异；整表摘要仍 `71ecc80cade3d73d`。
  - ✅ `run.json` 自述 `nondeterministic_fields: []`，时间戳全住在 `adapter.json` / `timing.json`——
    所以「搬完还能逐字节复现」这句话是**可以被证伪的**，不是容差换来的。
  - ✅ 「两个宿主**真的**共用」这半句已由 T2.3 的浏览器腿实证（见下）：两条腿的 80 帧 PNG 与
    `readings.txt` **逐字节相同**，整表摘要同为 `71ecc80cade3d73d`。所以"共用"不是"看起来像"，
    而是"同一份 core 代码（`corpus::render_run`）在另一个宿主上出了同样的字节"。
  - ✅ 顺带：worker 侧 `scenes.rs` 1376 → 683 行，只剩解析 CLI 取值 / 落盘 / 跨进程比对三件事。
- [x] **T2.2 native 侧出图**（M1 已具备）
  - ✅ M1 两条腿（Win/DX12、Win/Vulkan）各 80 张 PNG 已归档；本里程碑把它当**重构的回归基线**
    再用一次：同一份 WGSL、同一份驱动搬进 core 之后，出的字节与归档逐字节相同。
- [x] **T2.3 wasm 侧出图**（浏览器腿真跑并留证：`records/m2/browser/`）
  - `dhampir-wasm` + 最小 HTML/JS 驱动
  - 需要浏览器带 WebGPU：Chrome/Edge 113+ / Safari 26+ / Firefox 141+(Windows)
    - ✅ 实跑用 Chrome/153.0.8010.53 **无头**（无头下拿得到 WebGPU，不必 `--headed`）
  - ✅ 三层各管一段：`dhampir-wasm/src/corpus.rs` 导出 open/adapter/run/frame/readings/close，
    其中渲染走的是 core 的 `corpus::render_run`——与 native **同一条代码路径**；页面
    `www/corpus.html` 读卡身份、发起一轮、把 core 给的字符串转成 POST；驱动
    `scripts/run-browser-corpus.mjs` 起 Chrome、送宿主设备表、**自己触发这一轮**、落盘 + 截图。
  - ✅ **偏离 plan 原文一处（记下来）**：原文说"先手工跑通，自动化留到 M6"，这里直接写成脚本。
    理由是 T2.3 的证据必须是**可复核产物**——手点留不下"谁都能重跑出同一批字节"这种东西；
    M6 那条自动化（CI 里跑）与这个驱动不是同一件事。运行时位置定在 `scripts/`。
  - ✅ 设计要点 2 落在**两端同一条约束**上：渲染进离屏纹理再 `copy_texture_to_buffer`
    （core `render/corpus.rs:177` 的用途里必须有 `COPY_SRC`；canvas 纹理通常没有这个用途）。
  - ✅ 设计要点 3：两侧都出 `Rgba8UnormSrgb` PNG，比对在**编码后的字节**上做。
  - ✅ 真跑（`node scripts/run-browser-corpus.mjs --out records/m2/browser --leg m2 --native records/m1/dx12`，EXIT=0）：
    80 帧落盘（`png_bytes_total 129619`）、服务端 12 条完整性检查全过、页面整轮 1307 ms / 其中渲染 1022 ms；
    截图 + 旁证 JSON 落在 `records/m2/browser/`。
  - ✅ **与 native 腿逐字节等同**（用独立于服务端的探针复核，不接受服务端自证）：文件名集合相同、
    **80/80 帧逐字节相同**、`readings.txt` 逐字节相同（112215 字节，sha256 前 16 位 `58c75975032bb780`）、
    整表摘要同为 `71ecc80cade3d73d`。`run.json` 不同（305089 vs 305100 字节）但**只差 2 栏**：
    `backends.0.adapter_name`（`null` vs `"NVIDIA GeForce RTX 4070"`）与 `backends.0.requested`
    （`BROWSER_WEBGPU` vs `DX12`）——两栏都在记录**自己声明**的"因宿主而异"里，声明栏之外的差异 **0 处**。
    `adapter.json` 两份**形状本就不同**（浏览器那份多出 in_page 与缺口说明），不是"差几个字节"。
  - ✅ 卡身份做成**三份物证对账**：驱动从 CDP `SystemInfo.getInfo` 原样搬来宿主设备表（本机 3 块：
    RTX 4070 / AMD Radeon(TM) Graphics / Microsoft Basic Render Driver）→ POST 给服务端 →
    服务端**等页面报的 `vendorId` 到了**才在表里认人（设备表先到时只收不判），落 `host-gpu.json`，
    `run.json` 的 `adapter_name` 与它同源。匹配只走数字 `vendorId`：实测 `vendorString` 全是空串；
    查不到厂商即拒、同厂商 ≥2 块卡即拒（不自动消解二义性）。
  - ✅ 三条自检全绿 + 一份记录：服务端 `--self-test` **142/142**、驱动 `--self-test` **52/52**、
    wasm 侧 `node scripts/run-wasm-tests.mjs --out records/m2` **10/10**（快照 `records/m2/wasm-tests.json`，
    与 M1 归档同一约定：每个里程碑一份；记录里的 `milestone: "M0"` 是**记录契约版本**，不是"什么时候跑的"）。
  - ✅ **踩坑（真事故，已修 + 已加反向自检）**：`run-wasm-tests.mjs` 的 `--out` 默认值曾是 `records/m0`——
    一次不带 `--out` 的裸跑把 M0 的冻结快照覆盖成了 M2 的 10 条测试集。默认值改到
    `target/wasm-test-record/`（非归档；CI 与 `record-acceptance.mjs` 本就显式传 `--out`，不受影响），
    自检加一条反向用例「默认输出目录不许落在 `records/` 里」，M0 快照已按 HEAD 还原（当时是 4/4）。
  - ✅ **踩坑（真缺陷，已修 + 已补守卫）**：页面拿 `text.length`（UTF-16 码元数）当字节数报，
    `readings.txt` 被写成 78791 字节而真身 112215 字节——字段名还叫 `readings_bytes`。修法：
    字节数**只能来自写文件的那一侧**（服务端实测 `body.length`，三个落盘口一律回 `bytes`），
    页面照抄、缺了就抛（不自己算替代品）；驱动再 `stat` 盘上文件对一遍，对不上不写截图。
    反向验证用**修之前那份旁证的原始值**喂新判定（旧值红、真值绿），探针留在 `target/`。
  - ✅ 顺带给 T2.4 一条底数：**同厂商同栈（浏览器 WebGPU/NVIDIA vs native DX12/NVIDIA）出的是同样的字节**，
    所以逐帧 SSIM 在这对运行时上必然 ≈ 1.0。量出来的 1.0 是**对照**，真正的考题在 T2.6 换厂商那一腿。
- [x] **T2.4 比对工具**（原文位置 `tools/dhampir-framediff` 已偏离 → `scripts/dhampir-framediff.mjs`；真跑记录 `records/m2/framediff/`）
  - 输入两组 PNG 目录 → 输出：逐帧 SSIM + PSNR + 最大绝对差 + **放大差异图 PNG** + `summary.csv`；超阈值退出码非 0
    - ✅ 落盘四件：`summary.csv`（逐帧十列）、`verdict.json`（逐场景判定 + 档位 + 两侧帧集摘要）、
      `report.txt`（屏幕上那份报告，记录要的是**文件**不是口述）、`diff/*.png`（**有差异才写**——
      全等的帧不写黑图充数，"没有差异"由 `max_abs_diff=0` 说话）
  - 建议**不引 Python**：CI 里少一个依赖，工具本身也进 Cargo workspace 统一管理
    - ✅ 前半句照办（全工具只用 `node:zlib`）；后半句没照办，见下面那条位置偏离
  - 阈值文件 `thresholds.toml` 按场景分档
    - ✅ 机制已实现且自检覆盖（`[default]` 打底 + `[scenario.<名字>]` 覆盖；不认识的键/节、
      同一个键写两遍、场景没档位可解析——一律**报错**，"拼错的键被静默忽略"等于这道闸没设）
    - ✅ **但档位里一个场景覆盖都还没写：没有实测就不写数**。两份档各管一腿：
      `scripts/framediff-thresholds.toml`（跨机/跨厂商腿，`[default]` 停在下面的初始建议值上，
      **故意不写 `max_abs_diff_max`**——跨厂商差多少算正常只有真跑过才知道，先写个数等于把猜的数
      伪装成定的档，留给 T2.6 实测）与 `scripts/framediff-thresholds-exact.toml`
      （同机同栈腿，判逐字节相等：`mean=1`/`min=1`/`max_abs_diff_max=0`）。
      所用档的文件摘要（FNV-1a）进 `verdict.json`，读记录的人能确认"当时用的就是这一份档"
    - ✅ **上面那半句写于 T2.4（当时确实一个场景覆盖都没写）**，T2.6 已按实测定档：
      `mean_ssim_min` / `min_ssim_min` = 0.9995、`max_abs_diff_max` = 1，
      `[scenario.checker]` / `[scenario.srgb_linear]` 各写 `max_abs_diff_max = 0`。
      规则、实测数字与"为什么不用 plan 原文的 0.995/0.98"都写在档文件头（那是这份档的真相所在，此处不复述）
  - **初始建议值**：mean SSIM ≥ 0.995、min ≥ 0.98（灰度上算，8×8 或 11×11 高斯窗）——**只是起点，必须按实测定档**
    - ✅ 取值口径按原文：**11×11** 高斯窗、σ=1.5、"灰度"取 luma（`0.2126R + 0.7152G + 0.0722B`，
      取 PNG 里存的 sRGB 编码字节，**不是线性值**）、K1=0.01/K2=0.03/L=255、窗口只取 **valid 区域不 padding**
    - ✅ PSNR 走 **RGB 三通道** MSE（`10·log10(255²/MSE)`），最大绝对差走 **RGBA 四通道**
      （渲染是 `Rgba8UnormSrgb`，alpha 漂移一样是缺陷），差异像素 = 任一通道不同的像素数。
      三个指标都留着的理由：SSIM 会漏窄通道的差（蓝通道权重只有 0.0722），maxdiff 会漏大面积低幅漂移，
      PSNR 居中——**谁替不了谁**
    - ✅ 退出码把"判红"与"用不了"分开：**0** 达标 / **1** 判定红 / **2** 参数错、目录缺、两侧名字集对不上、
      空集、解码失败、阈值档缺。读记录的人才能归因
    - ✅ 名字契约 `<场景>-fNNN.png`；**两侧文件名集合必须相等，不取交集**——取交集会让
      "某一侧少了一批帧"伪装成全过（被截尾另由记录守卫对着 `run.json` 的 `frame_count` 抓）
  - ✅ **偏离 plan 原文一处（记下来）**：原文写的是"workspace 内小 crate"，改成了 Node 脚本。三条理由
    （都写在脚本文件头上）：① **解码器必须独立于生成器**——两侧 PNG 都是 core 用 `png` crate 编出来的，
    比对工具再拿同一个 crate 解回来，"验"与"生成"就共用同一套别人写的代码，库错在哪、滤波器理解错在哪，
    两侧会同样地错、SSIM 照样报 1.0，工具根本看不出来；所以这里**自写解码**，只借 `node:zlib` 解 DEFLATE
    （那是压缩标准，不是 PNG 知识），块结构、五个滤波器、CRC 全自己来——与 `scripts/check-m1-record.mjs` 同一范式。
    ② CI 不新增运行时依赖（Node 本就是守卫套件的依赖）。③ 与现有证据链同族：`scripts/` 下的工具都以
    "`--self-test` + 退出码 0/1/2"为契约，记录守卫能独立地再跑一遍它
  - ✅ 真跑（`node scripts/dhampir-framediff.mjs --a records/m2/browser/frames --b records/m1/dx12/frames
    --label-a browser-webgpu-nvidia --label-b native-dx12-nvidia --out records/m2/framediff
    --thresholds scripts/framediff-thresholds-exact.toml`，EXIT=0）：**5 个场景 80 帧全部逐字节相等**
    （逐帧 `SSIM=1`、`PSNR=inf`、`max|Δ|=0`、差异像素 0），两侧帧集摘要同为 `4bc004b502a1301a`，
    **0 张差异图**。这正是上面 T2.3 那句底数的直接印证——量出来的 1.0 是**对照**，
    工具在这种输入上本来就没有分辨力，**真正的考题在 T2.6 换厂商那一腿**（见下面 T2.6）
  - ✅ 产出**幂等**：同一条命令连跑两次，三个文件字节不动——`summary.csv`
    （sha256 前 16 位 `def780908c2eb8d0`）、`verdict.json`（`ea88c17ba5d219c6`）、`report.txt`（`80c85a946ec1875b`）。
    报告里的落盘位置写成**仓库相对路径**（一律正斜杠）：否则换台机器/换个检出重跑的复核者会看到不同文本，
    把正常差异误当成不一致。差异 PNG 自己**不承诺**跨 zlib 版本逐字节稳定（它是证据不是判据），
    前三个**承诺**可复现
  - ✅ 自检 `--self-test` **72/72**：参数解析、五个滤波器往返、六种坏 PNG、全等图 SSIM **恰为 1**
    （不是"约等于"）、单像素改动的三个指标、错位一格的条纹、配对反向（缺帧/多余帧/不合契约名/帧号重复/帧号断档）、
    阈值解析反向、四条判定闸各自判红且**理由点名那一条**、端到端落盘复现、差异图内容 = `|Δ|×amp` clamp、CSV/JSON/报告的形状
  - ✅ **反向验证 8/8**（探针 `target/m2-framediff-rev.mjs`，不进仓库）：把这 8 处检查从源码里逐个抠掉或改坏，
    对应用例必须红，**且要求自检跑完**（有那份失败清单）——只判 `status===1` 的话，抛异常崩掉也算通过。
    这条要求当场抓出真缺陷：删掉 `report.txt` 的写入后，自检**崩在 `readFileSync` 上**，
    屏幕上只有一段栈、没有"哪条用例红了"，**归因丢了**。修法：落盘件一律先确认"在"再读内容，
    缺文件按**用例名**报红（可复现性那三条也一样）
  - ✅ **真数据负对照 2/2**（探针 `target/m2-framediff-negative.mjs`）：合成图的负用例只能证明公式没写反，
    所以在**真 corpus** 上造一次真错位——把 native 那 80 帧抄一份、`checker` 场景整体错开一帧
    （最后一帧绕回第一帧）。两份档都判红（EXIT=1）且**只点名 `checker`**，其余四个场景各自保持 ✓
    （失败不串场景）：`mean SSIM 0.23822525937671718`、`min SSIM 0.12608851772776503`、
    `min PSNR 9.29031166980374 dB`、`max|Δ| 174`、差异像素 472000、**15 张**差异图。
    15 而不是 16 有出处：`checker` 场景的动画周期是 **3**（`f000=f003=f006=…=f015`，三种图案循环），
    错开一相位后 `f015→f000` 那一对正好落回同一相位——**不是工具漏判**
  - ✅ **原留给 T2.6 的两件事都已兑现**（本行原为 ⏳，属陈旧标记）：
    跨厂商腿的 `[scenario.*]` 定档见 T2.4 回填与 T2.6 的实测表（`0.9995` / `max_abs_diff_max = 1`，
    `checker` 与 `srgb_linear` 各写 0）；"这些差异算结构性缺陷还是均匀低幅噪声"的归因见
    T2.5 与 [`wgsl-portable-subset.md`](./wgsl-portable-subset.md) §6
- [x] **T2.5 差异归因与固化**
  - **结构性差异（边缘错位、色块偏移）一律当 bug 修**，不接受"浮点误差"解释
    - ✅ M2 没有触发这一条——但**"没触发"本身不是判据**，判据是下面那件承重墙：差异的**形状**成为可复核产物
  - 均匀低幅噪声 → 记录进"容差说明"，不进 bad case
    - ✅ 记录进 `records/m2/framediff-crossvendor/shape.json` 与档位文件头的实测表；**bad case 库保持为空**
  - 产出 **《WGSL 可移植性子集》**：允许 / 禁止的构造 + 替代写法。已知高危区：
    - 依赖 `fract` / `pow` 极限行为的写法
    - `fwidth` / 导数依赖（实现自由度大）
    - 长链累加（顺序敏感）
    - 非规格化数与精度限定（优先 f32，避免隐式 mediump 行为假设）
    - 纹理采样 LOD 语义差异
    - ✅ 落文件 `plan/wgsl-portable-subset.md`（第一版）：**穷尽**允许表 28 行（带 `<!-- wgsl-allow-table -->` 标记，
      工具认标记不认表头——表头认法会把禁止表里的名字也算成申报过的，等于把这道核对变成恒真）、
      禁止表 6 条（**从守卫 `crates/dhampir-core/src/render/wgsl_subset.rs` 读出来的，唯一一份来源**，
      两边手抄迟早漂移）、"你本来想写 → 写成什么"替代表 7 行、五个高危区各自写"本仓现状 + 要碰时先回答什么"
    - ✅ **两道机器判定都跑通**：`cargo test -p dhampir-core wgsl`（7 passed，禁词 6 条 0 命中）+
      `node scripts/dhampir-wgsl-census.mjs --declared plan/wgsl-portable-subset.md`
      （`申报 28 条；用了但没申报 0 条；申报了但当前没用 0 条`，EXIT=0）
    - ✅ **反向验证 17/17**（探针 `target/wgsl-doc-rev.mjs`，不进仓库）：四种变异各自**点名叫得出那条红**——
      抠掉允许表一行（点名 `fract(`）、抠掉标记（点名"找不到允许表标记"）、把标记贴到禁止表上
      （点名"表头不含「构造」"）、把某行第一列的反引号去掉（点名"第一列不是"）；
      另加两条行为核查：**未变异的文档必须绿**（反向集里要有"不红"的那一侧）、
      多申报一条只报告不判红（`申报了但当前没用 1 条`，EXIT 仍 0）
    - ✅ 允许表的计数口径写在文档第 1 节：**不同条目的计数会重叠、不能相加**（`f32` 也会数到 `vec2<f32>` 里那个），
      且词首 `\b` 匹配**故意保守**（`fwidth` 也命中 `fwidthCoarse`——普查要是比守卫钝，
      就会出现"普查说 0 次、`cargo test` 却红了"的死角）
  - ✅ **承重墙：差异的"形状"变成可复核产物**（`framediff --shape` → `records/m2/framediff-crossvendor/shape.json`，271845 B）
    - 动机：±1 LSB 的均匀噪声与"差一个像素"的错位，在 `max|Δ|` 上**可以一样**——只报幅度分不开这两种局面。
      所以要把差异的**位置 / 符号 / 整高列 / 有没有 ≥2 的差**记成文件，而不是留在不入库的一次性探针里
    - 工具侧 `pixelDiffShape` + `summarizeShape` + `buildShapeJson`：`--shape` 要求同时给 `--out`（否则 EXIT=2）；
      **刻意不改报告文本**（于是 `report.txt` 与不带 `--shape` 时逐字节相同，记录里的摘要不受 `--shape` 影响）；
      有符号差写死 **甲 − 乙**；**`per_channel` / 象限数「通道实例」（像素×通道），`diff_pixels` / bbox 数「像素」**——
      两套口径分开命名，读的人不会去相加；并集与逐帧分开记
    - 自检 **94/94**（T2.4 那 72 条之外新增 22 条，全是 `--shape` 的：默认关、全等图一切为空、**整图皆差的正例**防恒真、
      整高列与整宽行分开数、有符号差对调两侧正负互换、`|Δ|≥2` 与 `|Δ|=1` 分开数、「并集 ≠ 逐帧相加」、
      端到端**不动记录三件**、可复现、绑定输入）。过程中抓出一处**用例自己写错**（两次落盘到不同目录 →
      `report.txt` 的"记录 → <路径>"那行必然不同），已改成落**同一目录**再比
    - 反向验证 **11/11 + 6/6**（探针 `target/m2-shape-rev.mjs`，不进仓库）
    - 实测形状（这一对运行时，`α`/`β` 由 `shape.json` 的 `signed_delta_definition` 固定为甲 − 乙）：
      `alpha_stack` 每帧整幅 65536 px 全差 1（256 列全部整高 + 256 行全部整宽；只在 G/B，四象限计数对称）；
      `blur` 每帧 3 px、**不成带**（bbox = rows 39..41 × cols 39..44，整高列 0 条）；
      `gradient` 每帧 14 条整高窄带，**"14 条"是条数（逐帧不变），"是哪 14 列"逐帧在变**——
      16 帧 16 个不同集合、并集 128 列（= 16 × 8）、每 16 列一档的并集恒为 `{3,4,5,7,8,10,11,12}`；
      `checker` / `srgb_linear` 全 0；**没有任何一处 ≥2 的差**
    - ✅ 归因清单进 `plan/wgsl-portable-subset.md` §6（每条：形状 / 幅度 / 是否可接受 / 理由 / 进不进 bad case）。
      **事实与推断分开写**：形状（差异长在哪、多大）是实测；"它是量化边界上的舍入"是**与形状一致的推断**——
      要升级成事实得做"一次只改一个变量"的实验
- [x] **T2.6 跨机比对**：把浏览器换到与服务端不同厂商的 GPU/驱动组合，重复 T2.4
  - 这才是真实部署场景（用户设备千奇百怪，服务端又是一家）
  - ✅ 换了厂商：甲 = 浏览器 WebGPU / **AMD Radeon(TM) Graphics**（驱动 `32.0.21030.2001`，Chrome 153 无头
    `--force_low_power_gpu`），乙 = native DX12 / **NVIDIA RTX 4070**（驱动 `32.0.16.1074`）。
    浏览器腿 80 帧另存 `records/m2/browser-amd/frames`（帧集摘要 `bc09803eb44beac8`，`png_bytes_total 129765`），
    乙腿用 M1 归档 `records/m1/dx12/frames`（`4bc004b502a1301a`，129619 B）——**两条腿各 80 帧，都不是新生成的**
  - ✅ **按实测定档、规则化而不是挑数**（`scripts/framediff-thresholds.toml` 文件头有完整推导）：
    `mean_ssim_min` / `min_ssim_min` = `1 − 2 × (1 − 实测最差)` 向下取 4 位 → **0.9995**（实测最差 0.999781）；
    `max_abs_diff_max = 1`（规范给 8 位 sRGB 编码的余量就是 1 LSB，与本仓 `render/scene.rs` 的
    `BYTE_TOLERANCE = 1` 同源；负对照量到 **174**，1 与 174 之间没有灰度地带，放到 2 或 3 等于给真缺陷留藏身处）；
    **故意不写 `psnr_db_min`**——它多能抓的只有"全图每通道都差 1"（≈ 48.13 dB），而那正是这份档明说接受的容差，
    写一个专抓"刚声明接受的东西"的闸只会让下一位看不懂这份档
  - ✅ `[scenario.checker]` / `[scenario.srgb_linear]` 两条 `max_abs_diff_max = 0`：**定位是对照/诊断，不是"更严"**。
    这两个场景的输出颜色都是常量、算术里既没有插值也没有混合——实测跨厂商**逐字节相同**，
    所以"要求相等"在这里是让它们去**分开两种局面**：红了 → 差异已不在"多操作数算术的舍入"范围内（那必须查）；
    绿了 → 那 ±1 的来处被夹在"插值/混合"里。文件头写明了"唯一不许的做法：为让某次记录变绿而删掉或放宽它"
  - ✅ 真跑 EXIT=0（`records/m2/framediff-crossvendor/`）：`✓ 5 个场景、80 帧全部达标`；
    `checker`/`srgb_linear` 的 min PSNR 是 `inf`，`alpha_stack` 49.89 dB、`blur` 92.62 dB、`gradient` 65.52 dB；
    所有帧 `max|Δ| ≤ 1`
  - ✅ **改了档位文件 → 记录按规矩重跑**：52 个文件 → 52 个（**0 增、0 减**），变的只有 3 个文本件、
    且各只差摘要那一行（`report.txt` / `verdict.json` / `shape.json`），48 张差异图与 `summary.csv`
    **逐字节未动**（`summary.csv` 的 sha256 前 16 位 `5923087ead660af2` 前后相同）。
    紧接着再跑一次：**52/52 一个字节都不动**（幂等）。中途还核清了一处容易混的口径：
    记录里的档位摘要 = **档文件原始字节的 FNV-1a 64**（当前 `3f9560d1e3d4fb3f`），**不是**文件 sha256
    （两者都是 16 位十六进制，第一次核对时我拿 sha256 比、对不上才发现）
  - ✅ 负对照仍 2/2（`target/m2-framediff-negative.mjs`）：同一份档下把 `checker` 整体错开一帧 →
    只点名 `checker`、EXIT=1、`max|Δ| 174`。**这份档分得开"能容的 1"与"不能容的 174"**

### 退出标准

- [x] 5 个 corpus 场景在同一对运行时上 SSIM **全部达标**（同机）
  - ✅ 判据 `records/m2/framediff/verdict.json`：5 场景 16 帧×5 全部 `verdict: pass`，`exit_code: 0`；
    而且比"达标"更强——**80 帧逐字节相等**（`SSIM=1`、`PSNR=inf`、`max|Δ|=0`），
    两侧帧集摘要同为 `4bc004b502a1301a`。这一腿用的是"判逐字节相等"的严格档
    （`scripts/framediff-thresholds-exact.toml`），不是 plan 的初始建议值 0.995/0.98——
    同机同栈既然实测就是同样的字节，就不该拿淡化过的闸去量它
  - ⚠️ 但要老实说清：这一条"达标"是**对照**不是**考题**（见上面 T2.4 那条）。
    同厂商同栈下工具本来就没有分辨力，它的分辨力由三处另证：自检里的合成负用例（72 条）、
    反向验证（8 处检查逐个抠掉都要红）、真数据负对照（真错位 → 只点名 `checker`，EXIT=1）
- [x] 跨厂商 GPU 组合下**不出现结构性差异**
  - ✅ 判据 `records/m2/framediff-crossvendor/`：`verdict.json`（5 场景 80 帧，`exit_code 0`，档位摘要
    `3f9560d1e3d4fb3f`）+ `shape.json`（差异的**形状**）。"不是结构性差异"这条结论不靠幅度说话——
    幅度分不开"均匀 ±1"与"错位一格"——靠的是形状：差异只有三种（整幅 1 LSB、高光块尾迹 3 px、
    量化边界上 1 px 宽的整高窄带），**没有一种是"连续边界带偏向同一侧"那种错位的形状**；
    而且 `checker`（像素级棋盘，对 1 px 错位最敏感）与 `srgb_linear` **逐字节相同**
  - ⚠️ 边界要老实说：这条**只对"这两个厂商 + 这五个场景"成立**。下一块卡（Intel Arc、Apple M 系）
    进记录时，归因表要么被验证、要么被改写——这也是"容差说明"留在档文件头与子集文档 §6 里的理由
- [x] 《WGSL 可移植性子集》第一版落文件
  - ✅ `plan/wgsl-portable-subset.md`：穷尽允许表 28 行（普查工具按 `<!-- wgsl-allow-table -->` 标记认表，
    **没申报却在用直接判红**）、禁止表 6 条（来源唯一 = `render/wgsl_subset.rs`，工具从那里读）、
    替代写法 7 行、五个高危区、差异归因清单、以及"这道闸的边界（明确不做）"
  - ✅ 两道机器判定：`cargo test -p dhampir-core wgsl` 7 passed；
    `node scripts/dhampir-wgsl-census.mjs --declared plan/wgsl-portable-subset.md` EXIT=0
    （`用了但没申报 0 条`）；两道各自做过反向验证（守卫 4 条测试逐个反例 + 探针 `target/wgsl-doc-rev.mjs` 17/17）
- [x] 差异归因清单完成：每条已知差异都有"是否可接受 + 理由 + 是否进 bad case"
  - ✅ 清单在 `plan/wgsl-portable-subset.md` §6，6 条（五个场景 + 一条"错位一类"的**证伪素材**）：
    `alpha_stack` / `blur` / `gradient` 判**可接受（容差内）**且写明理由，`checker` / `srgb_linear`
    判**不需要容差**（0 差异），第 6 条是**工具分辨力**的证据（`max|Δ| 174`）而不是 bad case。
    结论：**bad case 库为空**

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

> ✅ **已决（2026-09-22）：选 (b) `copyExternalImageToTexture`。**
> 决策文件：[`s3.1-source-frame-sampling.md`](./s3.1-source-frame-sampling.md)（含实机实测数字与复现方式）。
> 一句话理由：1080p 下**两条路输出逐字节相同**、成本差在噪声内（`copy+render − import+render = +0.04 ms`），
> 而 (a) 的结构性代价是与 native **WGSL 分叉**——那正是底座的立身之本。
> 直接后果：`dhampir-core::io` 里「`frame_view` 装不下外部纹理」那条已知张力**解除**，不必给 `FrameSource` 开分支。

- [x] ~~**S3.1 源帧采样策略三选一 —— 本里程碑最重要的决策**~~（见上方「已决」块）

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

> ✅ **已决（2026-09-22）**：[s3.2-proxy-spec.md](./s3.2-proxy-spec.md)。
> 关键实测：规格里的 -g 60 / -keyint_min 60 / -sc_threshold 0 会把**稀疏源归一化**——
> 一个 -g 250 的源（2 个关键帧、最大间隔 250 帧）经本规格出来是 **8 个关键帧、间隔恒为 60 帧（1.0 s）**；
> 代理体积为源的 19.0%（1080p 7.66 MB → 720p 1.46 MB）。
> 踩过的坑：ffprobe 的 csv=p=0 输出带尾逗号，拿 === "1" 数关键帧会**静默少算**（规格第一版就数错了）。

- [x] ~~**S3.2 proxy 生成规格**（参考命令，参数按实测调）~~
  ```
  ffmpeg -i src.mp4 -vf scale=-2:720 \
         -c:v libx264 -preset veryfast -g 60 -keyint_min 60 -sc_threshold 0 -crf 23 \
         -c:a aac -b:a 128k proxy.mp4
  ```
  - `-g 60 -keyint_min 60 -sc_threshold 0`：60fps 素材下 **1 秒 1 个 I 帧**，禁掉场景切换产生的额外关键帧
  - **关键帧密度直接决定 seek 手感，比分辨率重要得多**
  - 同时产出封面图 + 雪碧图（时间线缩略图带）：`-vf fps=1/5,scale=160:-1,tile=10x10`
> ✅ **已决（2026-09-22）**：[s3.3-videoframe-lifetime.md](./s3.3-videoframe-lifetime.md)。
> 实测：在这套栈上**提前 close 与完成后 close 输出逐字节相同、0 错误**——
> 但「调用点即快照」是实现细节而非规范保证，所以**仍定「完成后才 close」**（不花钱，保可移植性）。
> 边界：本实验的源是 new VideoFrame(videoElement)，**不是解码器输出**，真实路径留 T3.2 补测。

- [x] ~~**S3.3 VideoFrame 生命周期约定**~~
  - RAII 包装，**在 `queue.on_submitted_work_done()` 之后再 `close()`**——规范上说 VideoFrame 关闭即外部纹理失效，比"提交后立刻 close"稳
  - 不及时 `close()` 会耗尽显存，长视频/多轨场景必爆

### 任务

- [ ] **T3.1 `web/` 骨架**：Vite + React + TS；加载 wasm-pack `--target web` 产物
- [ ] **T3.2 demux + 解码**：mp4box.js 取样本 → `VideoDecoder` → VideoFrame
  - **只解 H.264，不引 FFmpeg WASM**（有意取舍：WASM 那 0.3x 性能和几十兆包体积不值得，异构格式交服务端）
- [ ] **T3.3 上屏链路**：~~`import_external_texture`~~ → **拷贝进 `texture_2d` + `textureLoad`** → core 渲染图 → canvas surface
  - ✅ **已落地（2026-09-22）**：core 侧的第一级 [`BlitRenderer`]（`render/blit.rs` + `shaders/blit.wgsl`）——
    全屏三角形 + `textureLoad(src, vec2<i32>(pos.xy), 0)` 逐纹素恒等搬运；**真机验证通过**
    （`cargo test -p dhampir-worker --test blit -- --ignored`：16×16 全不相同图案，读出逐字节相等）。
    该测试默认 `#[ignore]`，**不把 GPU 依赖塞进 `cargo test --workspace`**（默认套件仍是 148 passed / 1 ignored）。
    子集普查 EXIT=0（用了但没申报 0 条）；**wasm 侧 `FrameSource` / `FrameSink` 与 canvas 上屏还未做**。
  - ⚠️ 按 S3.1 的结论改了做法。两条实测理由：① 子集**禁隐式 LOD 采样**（`textureSample(`），
    而 `texture_external` **只能**用 `textureSampleBaseClampToEdge` 采——选外部纹理就等于给子集开例外；
    ② `textureLoad` **没有 external 重载**（实测编译失败：`no matching call to textureLoad(texture_external, …)`）。
    见 [`s3.1-source-frame-sampling.md`](./s3.1-source-frame-sampling.md) §2.2。
  - wasm 侧实现 `FrameSource` / `FrameSink`
- [ ] **T3.4 帧缓存**：LRU + 显存上限（参考 MASterSelects 的 300 张 VRAM 纹理）+ RAM 预览缓存（参考 900 帧）
- [ ] **T3.5 测量**：seek p50/p95、播放丢帧率、解码 → 上屏延迟、显存/内存曲线

### 退出标准

- [ ] 1080p proxy **全速播放 60fps**，无 FFmpeg WASM 回退路径
- [ ] 拖动 scrub p95 ≤ 50ms ——**起始值，按实测定档**
- [ ] 4K 源素材下浏览器内存/显存不越界（上限 + LRU 淘汰生效）
- [x] S3.1 决策文件落盘，含实测数字 —— ✅ [`plan/s3.1-source-frame-sampling.md`](./s3.1-source-frame-sampling.md)

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

> ⚠️ **范围收窄**：本章的**契约**（T4.1 schema v1）与 **core 渲染图**（T4.2/T4.3）属底座；
> T4.4 的 FFmpeg 编码 / mux 出片属下游形态——本仓库只保证渲染层（`--dump-raw` 等价物）可复现。

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

## 8–10. M5 / M6 / M7 —— 已移出本仓库

> ⛔ 这三章属「服务器整体架构」与「产品化」，**不在本仓库范围内**（见 §2「范围」）。
> 完整内容见**附录 B：下游形态参考**——保留作为下游工程的参考，**不在本仓库实现、不计入收官**。

| 章 | 处置 | 一句话 |
|---|---|---|
| **M5** 分布式分片渲染 | ⛔ 整章移出 | 帧精确 ⇒ 可任意分片；分片结果必须等于整体。属下游编排 |
| **M6** 一致性保障与发布流程 | ⚠️ 部分移出 | SSIM 闸门 / golden / bad case / 阈值表**留在底座**；影子环境与发布流程移出 |
| **M7** 产品化 | ⛔ 整章移出 | 上传 / 存储 / 导出 API / 时间线 UI，全部属下游 |

---

## 11. 横切事项

### 11.1 本地验证蓝图

只做本地处理：**没有远端 CI**。下表就是「提交前在本机按序跑一遍」的清单；
`.github/workflows/ci.yml` 只是这份清单的另一种写法，**不在远端跑**（见 §2「范围」）。

| 阶段 | 检查 | 何时跑 | 说明 |
|---|---|---|---|
| M0 起 | `check-native`：`cargo check --workspace` + `cargo test` | 每次提交前 | Win；另加 `--target x86_64-unknown-linux-gnu` 交叉 check |
| M0 起 | `check-wasm`：`cargo check -p dhampir-wasm --target wasm32-unknown-unknown` | 每次提交前 | 只 check wasm crate，不用 `--workspace` |
| M0 起 | `guard`：core 无 `#[cfg(` / 依赖方向检查 | 每次提交前 | 用脚本判定，简单有效 |
| M2 起 | `render-corpus` + `framediff` | 改动触及渲染时 | 见 T6.1 的 lavapipe 权衡 |
| M4 起 | `e2e-sample`：样本工程出片 + 成品比对 | 里程碑收官前 | 重，别放手边快速回路里 |
| ~~M6 起~~ | ~~`shadow`：影子环境抽样~~ | — | ⛔ 已移出本仓库（属服务器/产品形态） |

### 11.2 测试资产清单（长期累积）

- **合成 corpus**：5 个确定性场景（M1 建，M2 用，M6 进闸门）
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
- M3：1–2 周 ｜ M4：2–3 周
- ~~M5：2–3 周 ｜ M6：1 周 + 持续~~ ← 已移出本仓库，不计入
- **合计到 M4：约 4–7 周（单人）**
- **并行方案**：第 2 人力从 M3 切入 → 约 **3–5 周**（M3 与 M1/M2 无依赖）

**成本项**

> 以下为**下游工程**的成本，本仓库只做本地、不承担：

- ~~开发期云 GPU 按小时零星开销~~
- ~~M5 起持续 GPU 开销（spot + 优先级路由）~~
- ~~headless GPU 容器化的持续维护成本（Vulkan ICD / 驱动版本漂移）~~

本仓库的成本 = 本机开发工时（见上）。

### 11.5 证伪点与 Plan B

每个闸门都必须预先写好"不过怎么办"，否则闸门会变成一句口号：

| 闸门 | 触发条件 | Plan B（按优先级） |
|---|---|---|
| **M1** | headless wgpu 在目标环境不可用 | 换实例类型 / 换 GPU 厂商 → lavapipe 降级（性能掉一个数量级，只适合小规模） |
| **M2** | 双编译器差异无法在合理容差内收敛 | (a) 收缩 WGSL 子集（最可能够用）；(b) 问题算子改成 LUT 纹理或两端同一实现的 CPU pass；(c) 兜底：服务端改走 FFmpeg filtergraph——**特效写两遍且长期互相追赶，这是要尽量避免的结果**；(d) (c) 之后的选项：服务端改用 headless 浏览器 + WebGPU 农场（实现唯一，但成本与吞吐更差，值得评估） |
| **M4** | 含解码的差异过大 | 收紧色彩管线：统一转换矩阵 + 色度上采样策略；或预览侧改走 `copyExternalImageToTexture` 与 native 对齐（牺牲零拷贝换一致） |
| ~~**M5**~~ | ~~分片与整体不一致~~ | ⛔ 已移出本仓库（下游工程） |

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

**第一个动作**：M2 收尾（见 [`plan/remaining-work.md`](./remaining-work.md)）——先给 `scripts/check-m2-record.mjs`
补上 `main()`，**再**跑验收；顺序颠倒会产出一份假绿的验收快照。

---

## 附录 B：下游形态参考（M5 / M6 / M7）

> 本附录收录**已移出本仓库范围**的三章。它们是**下游工程**的形态参考，
> **不在本仓库实现、不计入收官**。正文位置见 §8–10 的说明。

### B.1 M5 —— 分布式分片渲染

> **目标**：GOP 边界分片并行渲染 + merge，结果与整体渲染完全一致。
> **帧精确 ⇒ 可任意分片且结果一致**——这是分布式能成立的唯一前提。

**前置**：M4 退出标准全绿。

> ⛔ **已移出本仓库范围**：分布式编排属「服务器整体架构」，本仓库只交付底座。

#### 任务

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

#### 退出标准

- [ ] 4 分片 == 整体（`--dump-raw` 逐帧达标；成品解码帧在编码容差内）
- [ ] 无边界丢帧/重复帧（帧数 + PTS 校验通过）
- [ ] checkpoint 恢复验证通过
- [ ] 单 GPU 吞吐基线记录（供容量规划）

#### 风险与提示

| 风险 | 对策 |
|---|---|
| 分片边界产生编码瑕疵 | 闭合 GOP + 固定 keyint + `scenecut=0`；边界帧逐个核对 |
| 编排复杂度吃掉工期 | v0 单机多进程先验证等价性，分布式其次 |
| 无状态 worker 的中断 | checkpoint + 心跳重排；用故障注入测 |

> **证伪点**：分片与整体不一致且无法在编码容差内解释 → 检查 GOP 闭合与时间戳；最坏情况退回"单片 = 整片"（放弃分布式吞吐收益）。

### B.2 M6 —— 一致性保障与发布流程

> **目标**：SSIM 比对进闸门 + 影子环境；把"一致性"从一次性验证变成长期自动保障。
> ⛔ **部分移出本仓库范围**：SSIM 闸门与比对工具属底座（**保留**）；
> **影子环境与发布流程**属服务器/产品形态，**移出本仓库**。

**前置**：M2（比对工具）+ M5（完整出片链路）。

#### 任务

- [ ] **T6.1 CI 闸门**：编译（Win + Linux）+ headless 渲染 corpus + framediff 对 golden
  - **必须写清的权衡**：lavapipe 便宜，但浮点路径与真 GPU 不同 →
    - lavapipe 只适合"结构正确性"闸门
    - SSIM 闸门要么用**自托管 GPU runner**，要么给 lavapipe **单独标定阈值**。二选一，别混
- [ ] **T6.2 golden 资产**：样本工程 + 预期 MP4 + **bad case 库**（每条线上事故入库并附 issue 链接）
- [ ] **T6.3 影子环境**：发布前抽样跑线上数据比对，全过才允许发布
- [ ] **T6.4 阈值表 + 差异说明文档定稿**（按场景分档，标注标定时间与环境）

#### 退出标准

- [ ] 闸门能拦住 SSIM 回归（人为造一次回归验证拦截生效）
- [ ] ~~一次完整影子发布流程走通~~ ← 随发布流程一并移出

### B.3 M7 —— 产品化（占位）

> ⛔ **已移出本仓库范围**：产品化（上传 / 存储 / 导出 API / 时间线 UI）属下游工程。

**前置 = M4 结论。** 在 M4 之前展开是浪费——契约与一致性机制没验证完，产品层所有设计都可能推倒。

范围预告（不承诺顺序）：

- 上传 → probe → 分流 → proxy 管线（服务端）
- 对象存储：原片 / proxy / 封面图 / 雪碧图 / 成片
- 导出 API + 预设；任务查询与错误面
- 前端时间线 UI：**视图中控**（点击生成拖拽实体、影子元素渲染、松手才更新轨道数据）+ 本地/云端双模式（指导文档 §6.2）

