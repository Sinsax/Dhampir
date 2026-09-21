# Dhampir M0 独立复核报告

- **复核对象**：提交 `98b517a3d0a13d3d0ff8eb448cea964292de5f67`（`main`，根提交，`git show --stat` = 69 files changed, 12126 insertions(+), 0 deletions）
- **复核方式**：以证伪为目的。全部实验在克隆 `target/review-m0/clone`（HEAD = 上述 sha）内进行，构建产物走独立 `CARGO_TARGET_DIR=target/review-m0/target`，**全部实验与构建都在克隆里做、不改真实仓库任何被跟踪文件**。此处有一处必须自曝的例外：我的一次脚本副作用在真实仓库跑过一遍验收（含 `cargo check` / `cargo test`，因此也用过真实仓库的 `target/`）并临时覆盖了 4 个记录文件，之后已完整还原——详见「观察 O-4」
- **环境**：win32 10.0.19045 / Rust 工具链同记录 / Chrome 153.0.8010.50（`C:\Program Files\Google\Chrome\Application\chrome.exe`）/ wasm-pack 0.15.0 / wasm-bindgen 0.2.128 / 本机 GPU：RTX 4070（**未用于重跑探针**，见第 3 节）
- **我写的工具**：`target/review-m0/tools/`（`recompute.mjs`、`digest.mjs`、`bytecompare.mjs`、`lab-core2/3/4.mjs`、`lab-text.mjs`、`lab-png.mjs`、`lab-dep.mjs`、`myscan.mjs`、`noSideEffect/record-acceptance.mjs`）。**我没有调用仓库里任何摘要/判定脚本来自证摘要值**——FNV-1a 64 与 sha256 由我自实现/标准库独立算出。

## 1. 结论

1. **三条退出标准在干净克隆里全部独立复现为退出码 0，我没有找到假绿。** `cargo check --workspace`、`cargo check -p dhampir-wasm --target wasm32-unknown-unknown`、以及「同一份 golden 报告 native/wasm32 逐字节相等」三条都成立，且第三条我用两条互不相关的路径各验一次（`run-wasm-tests.mjs` 重跑 + 浏览器自检页重跑）。
2. **记录里的硬数字我逐条重算，全部对得上**：48 passed / 0 failed、golden 摘要 `c3f0da6b37577e55`（我用自实现 FNV-1a 64 独立算出，工具本身先用三组公开向量自校）、三份 256×256 探针 PNG 各 8789 字节且三方逐字节相同（`85bbc2017f35dda9`）、`records/m0/*.txt` 与 `selfcheck-native.txt` 一致。
3. **7 个守卫经得起对抗**：我注入 27 个载荷（注释/字符串/原始串/多行串/游离引号致剥离器失步/空白变形/空集合/反向依赖/CRLF/BOM/孤立续字节/截断多字节…），**没有一次假绿**；唯一「过严」而非「过松」的是 `#[cfg(test)]` 豁免边界（见 O-1）。
4. **发现 1 条重要、4 条次要**：都不推翻 M0 的「三条全绿」，但其中 F-1（Linux 交叉 check 的证据指针失效）会让照记录复核的人复现不到证据，F-2/F-3 是守卫自身承诺与行为不一致。
5. **本次未能独立验证的只有「需要真 GPU 的 native 探针重跑」与「CI/Linux 实机运行」**——前者我只做了文件级核对（字节/摘要/尺寸/IHDR 全对），未在克隆里重跑 `dhampir-render`。

## 2. 发现

### 重要

#### F-1 记录里对 Linux 交叉 check 的证据指针失效，且该命令在 `records/m0/` 内没有任何产物

- **现象**：`records/m0/README.md:144-145` 写「**不证明 Linux 上跑得通。** 本机是 Windows。`x86_64-unknown-linux-gnu` 的 `cargo check` 在本机过了（**见根 `README.md`**），但 `cargo check` 不链接、也不运行。」——而根 `README.md` 里**没有任何** `Linux` / `linux` / `x86_64` / `交叉` 字样（全文件 grep，0 命中）；`records/m0/` 里也没有该命令的任何产物。真正做了这条断言的是 `plan/video-editor-plan.md:135`（退出标准第 1 条的子句「另加了 `x86_64-unknown-linux-gnu` 交叉 check」）与 `:116`（§3 T0.6 下的「已在本机预验」），两处都只是文字，没有落盘证据。
- **我的独立测量**：交叉 check 本身是**真的**——
  ```powershell
  cd F:/para/Code/Dhampir/target/review-m0/clone
  $env:CARGO_TARGET_DIR='F:/para/Code/Dhampir/target/review-m0/target'
  cargo check --workspace --target x86_64-unknown-linux-gnu
  # 退出码 0（Finished `dev` profile），target 已预装
  ```
- **结论**：断言成立、**证据链断了**。`records/m0/` 里唯一提到该命令的文件是 `README.md:144` 的免责声明本身；根 README 没有可跳转的内容。
- **影响**：复核者按记录去核这条会扑空，无法区分「作者没跑」与「跑了没存」；这也是本次唯一「按记录无法复现」的项。建议要么把该命令输出存成 `records/m0/cross-check-linux.txt`，要么把 `README.md:144` 的指针直接写成命令行 + 预期退出码。

### 次要

#### F-2 `check-text-hygiene.mjs` 的空集合承诺与行为不符

- **现象**：`records/m0/README.md:127` 承诺「没有文件可查时退出码是 2」（脚本 `:186-188` 也确实写了「拒绝在空集合上通过」并 `console.error`）。
- **我的实测**：在一个只含该脚本自身的合成根上跑，**退出码 0**（脚本把自己算进了「可查文件」，因而集合永不为空，`:187` 的分支在实践中不可达）。
  ```powershell
  # 合成根 target/review-m0/empty3（只含 scripts/check-text-hygiene.mjs 及其自身 .mjs）
  node scripts/check-text-hygiene.mjs    # 退出码 0
  ```
- **对照**：同一类承诺在 `check-core-purity.mjs` 上是**兑现**的（`crates/dhampir-core/src` 存在但无 `.rs` 时退出码 **2**，明说「拒绝在空集合上通过」）。
- **影响**：低。不影响 M0 判定；但「空集合 = 2」这一族承诺在文档层面被过度统一，读者会以为两个守卫行为一致。

#### F-3 `check-dep-graph.mjs` 在缺 manifest 时以未捕获异常退出 1，而非干净报错

- **现象**：`check-dep-graph.mjs:418` 有空集合检查（「一个 workspace 成员都没解析到…拒绝在空集合上通过」），但当某个成员目录**存在却没 `Cargo.toml`** 时，脚本先崩在 fs 层。
- **我的实测**：合成根里让 `crates/dhampir-timeline` 无 `Cargo.toml` → 退出码 **1**，栈上 `node:fs:435` 未捕获异常（不是 `:418` 的干净报错）。
- **影响**：低（仍是非零退出，不会假绿）；但报错信息指向 node 内部而非「这个 crate 缺 manifest」，排障时会误导。

#### F-4 `records/m0/README.md:13` 的时间戳与 `acceptance.json` 不一致

- **现象**：`README.md:13` 写「（2026-09-21T**19:35:36Z** 重跑）」，而 `records/m0/acceptance.json` 的 `generated_at` 是 `2026-09-21T**19:39:33.372Z**`。
- **影响**：低，纯文档漂移；两处指的应是同一次「7 条判据全绿」的运行，但差 3 分 57 秒，给「这份 README 是否描述它旁边那批产物」留下了不必要的怀疑。

#### F-5 `records/m0/README.md:54` 的「全仓 50 个文本文件」是作者机器口径；且 `records/` 整目录被卫生守卫跳过

- **现象**：`README.md:54` 写「全仓 50 个文本文件全是 LF、无 BOM、合法 UTF-8」。同一命令在**干净克隆**里是 **45** 个文件：
  ```powershell
  cd F:/para/Code/Dhampir/target/review-m0/clone
  node scripts/check-text-hygiene.mjs   # ✓ 文本卫生：45 个文件全是 LF、无 BOM、合法 UTF-8；退出码 0
  ```
  记录里 `guard-text-hygiene.txt` 是 50。差的 5 个是被 `.gitignore:15` 忽略的 `crates/dhampir-wasm/pkg-node/` 构建产物——即 50 是「作者机器当时的现场数」，不是「提交内容」的属性。
- **第二层**：`check-text-hygiene.mjs` 的 `SKIP_DIRS` **包含 `records`**，所以 `records/m0/` 里的 20 个文件**不被任何守卫覆盖**；而 `README.md:54` 的「全仓」措辞会让读者以为那 50 个文件里含记录文本。
- **我的补测**：我用自己的扫描器把 `records/` 一并纳入（真实仓库 50 个文件 / **0 违规**；克隆 45 / 0 违规），所以**结论仍然成立**，只是措辞会误导。
- **影响**：低。断言真、口径与措辞假。

### 观察（不构成缺陷，但复核者应当知道）

- **O-1 豁免是 token 级，不是模块级。** `check-core-purity.mjs` 的豁免只认 `#[cfg(test)]` 这一个属性本身：`#[cfg(test)]` 同行再写 `#[cfg(unix)]`（`fn _e1() {}`）、下一行紧跟 `#[cfg(unix)]`、以及 `#[cfg(test)] mod t { #[cfg(unix)] ... }` **全部被报出**（退出码 1）。这比我预期的更严（测试模块内的平台分叉也会红），方向是**保守**的，不产生假绿；但 M1 之后若要在测试里做平台分叉，会撞上这条。
- **O-2 浏览器截图不是逐次可复现产物。** 我在克隆里重跑同一条命令，得到 88773 字节 / sha256 `80f3fdb2…`，而记录是 88779 字节 / `e845a011…`；两者尺寸一致（1400×1278、depth 8、colortype 2），差异来自页面里的时延与绝对路径（页面本身即包含 `canvas_render_ms`、`total_ms`、runner 路径）。**这是预期行为**，但意味着 `screenshot.sha256` 是一次性快照而非可复算摘要——记录里没写这一点。
- **O-3 `wasm-tests.json` 的机器相关字段。** 重跑后 2864 → 2933 字节，差异**仅**为绝对路径（作者机器 vs 我的克隆）与 `received_unix_epoch_seconds`（1790020060 vs 1790019302）；`cross-runtime.txt` 与 `selfcheck-native.txt` 重跑后**逐字节相同**（见我下面的精确比对）。
- **O-4 复核过程披露（必须记录）。** 我最初写的核对脚本 `tools/says-check.mjs` 用动态 import 读了真实仓库的 `scripts/record-acceptance.mjs`，该文件末尾有无条件的 `process.exitCode = main();` 副作用，导致**我在真实仓库里重跑了一遍验收**，覆盖了 `records/m0/acceptance.json`、`native-check.txt`、`native-tests.txt`、`wasm-check.txt` 四个文件（差异仅 `generated_at`、`seconds` 0.2→0.3 / 0.6→0.7 / 1.9→2.3、cargo `Finished … in 0.13s→0.23s` / `0.10s→0.16s` 与测试输出行序，无语义变化）。我已 `git checkout -- <4 个文件>` 完整还原，随后 `git status --porcelain` 为空。**没有提交、没有 push、没有改动任何其它文件**；根会话若在 19:42Z 前后观察到 `records/m0/` 变动，那是本次复核所为。此后我改用去副作用的忠实副本（`tools/noSideEffect/record-acceptance.mjs`，仅删掉那一行）做字面量比对。**这次意外的实际外溢范围**：(a) `cargo check` / `cargo test` / wasm 检查 / 7 个守卫都在真实仓库执行过一次，用的是真实仓库的 `target/`（因而与并行的 M1 构建共享过该目录，存在锁竞争风险）；(b) 4 个记录文件被覆盖后已逐字节还原；(c) `git status` 在我还原后为空，未产生任何提交。这是我的操作失误（不该对带副作用的脚本做裸 import），不是被测仓库的缺陷，但它确实使「本次复核从未在真实仓库跑过 cargo」这句话不成立，故在此明确改正。另：复核结束时真实仓库出现一个**不是本复核所为**的未跟踪文件 `crates/dhampir-core/src/shaders/scene.wgsl`（并行 M1 的痕迹）。

## 3. 未能验证的项（以及为什么）

| # | 未验证的项 | 原因 | 我做了哪些替代核对 | 残留风险 |
|---|---|---|---|---|
| U-1 | **native DX12 / Vulkan 探针的真实渲染**（`probe-native-dx12.png` / `probe-native-vulkan.png` / `*.adapter.json` / `run.json`） | 我没有在克隆里跑 `cargo run -p dhampir-worker --bin dhampir-render`：一次真实 GPU 渲染会占用驱动与窗口路径，且当时后台正跑浏览器复现任务；时间/磁盘预算内我选择优先覆盖「记录是否假绿」的判据 ③ | 文件级核对：三份 PNG 各 8789 字节、`fnv1a64=85bbc2017f35dda9`、`sha256=1e555d22…4646b`（**三方互为逐字节相同**）、IHDR 256×256 / depth 8 / colortype 6；`adapter.json` 内部自洽性（与 `run.json`、`browser-harness.json` 的 surface 字段一致） | **这些字节是否确由本提交的代码生成、`adapter.json` 里的设备名/驱动版本是否真是现场**——未被证伪，也未被证实。若有人伪造 PNG，我会看不出来（但伪造者还需同时伪造与 wasm32 侧逐字节相等，成本高） |
| U-2 | **CI 在真 runner 上跑通** | 静态核对（YAML + 步骤命令存在性）可做，实机执行不可做 | YAML `yaml.safe_load` 合法；3 个 job（`check-native` 7 steps / ubuntu+windows 矩阵、`check-wasm` 8 steps、`guard` 4 steps）；`continue-on-error`、`|| true` 各 1 处**均在第 11 行的注释里**；`--print-locked-version` → `0.2.128` 实测存在 | `check-wasm` 在 ubuntu 上靠 `cargo install wasm-bindgen-cli` 后由 `run-wasm-tests.mjs:125-130`（先查 PATH）发现 runner——**静态推理，未在 ubuntu 实机执行** |
| U-3 | **浏览器路径在非 Windows 宿主上的行为** | 只在 Windows + Chrome 153.0.8010.50 上重跑 | 该宿主上完整复现（见第 1 节结论 1、第 4 节 D-1 ~ D-6） | 其它宿主上 `serve-wasm-harness.mjs` / 截图脚本的端口与 chrome 查找逻辑未测 |
| U-4 | **「全仓 50 个文本文件」这一现场口径** | 干净检出恒为 45；差的 5 个是被 `.gitignore` 忽略的 `pkg-node/` 产物，属环境现场，无法在干净检出里重现 | 我用自己的扫描器在**真实仓库**同样得到 50 / 0 违规（与作者现场吻合），在克隆得到 45 / 0 违规 | 无（断言成立，仅口径不可复现） |
| U-5 | `records/m0/` **内文本被判为卫生的独立覆盖** | 仓库守卫的 `SKIP_DIRS` 跳过 `records`，所以「记录自身是否合规」无守卫背书 | 我用独立扫描器把 `records/` 纳入（真实仓库 50 文件 / 0 违规；克隆 45 / 0 违规），并单独验了 golden 文件与三份 guard txt 的 UTF-8/LF/无 BOM | 低 |

**未做且有意不做**：不重跑 `wasm-pack` 的 release 优化构建、不主动在真实仓库跑 cargo（O-4 那次意外除外）、不 push、不做 M1 范围的事；不重跑需要真 GPU 的 `dhampir-render`（见 U-1，这是本次复核最大的覆盖缺口）。磁盘用量：`target/review-m0/` 见文末附注。

## 4. 核对过的断言（逐条，含命令与退出码）

> 表中「我的测量」均为克隆 `target/review-m0/clone`（HEAD `98b517a3…`）内、`CARGO_TARGET_DIR=target/review-m0/target` 下所得。凡带「退出码」的均为我实测。

### A. 自包含性与三条退出标准

| # | 断言 | 记录说法 | 我的独立结果 | 证据（命令 → 退出码 / 关键输出） |
|---|---|---|---|---|
| A-1 | 提交规模 | 69 文件 / 12126 行新增 | 一致 | `git show --stat 98b517a3` → `69 files changed, 12126 insertions(+)` |
| A-2 | 干净检出自包含 | （隐含） | 成立 | 克隆到全新目录 + 全新 target：`cargo check --workspace` → **0**（`Finished dev … in 18.50s`） |
| A-3 | 判据 ① 同 A-2 | 退出码 0 | 成立 | 同上 |
| A-4 | 判据 ② | 退出码 0 | 成立 | `cargo check -p dhampir-wasm --target wasm32-unknown-unknown` → **0**（24.88s） |
| A-5 | 判据 ③ | native/wasm32 逐字节相等 | 成立，双路径 | (a) `node scripts/run-wasm-tests.mjs --out records/m0` → **0**；`running 4 tests` / `test result: ok. 4 passed; 0 failed`；重生成 `cross-runtime.txt` sha256 `2589623A…97EFF44C` **与提交版逐字节相同**；(b) 浏览器侧见 A-13 |
| A-6 | 测试规模 | 48 passed / 0 failed | 一致 | 我自己对 `cargo test --workspace` 输出里全部 `test result:` 行求和 → **48 passed / 0 failed**，退出码 **0**（24.51s） |
| A-7 | wasm 侧测试数 | 4 条 | 一致 | 4 个 `#[wasm_bindgen_test]` 与实际列出并跑过的 4 条一一对应 |
| A-8 | 行尾策略 | `* text=auto eol=lf` | 成立 | `git ls-files --eol` 全部 `i/lf w/lf`（40 个文本文件）；工作区 53 个文件含 CR 数 = **0** |

### B. 记录里的硬数字（不调用仓库脚本，全部独立重算）

| # | 断言 | 记录说法 | 我的独立结果 | 证据 |
|---|---|---|---|---|
| B-1 | golden 报告摘要 | `c3f0da6b37577e55` | **一致** | 自实现 FNV-1a 64（offset `0xcbf29ce484222325` / prime `0x100000001b3`），先用公开向量自校：`""`→`cbf29ce484222325`、`"a"`→`af63dc4c8601ec8c`、`"foobar"`→`85944171f73967e8`（3/3 OK）→ 对 golden 文件算得 `c3f0da6b37577e55` |
| B-2 | golden 形态 | 版本 1 / 72 行 / `cases=70` | 一致 | 4061 字节 / 72 行 / 纯 ASCII / 无 BOM / 无 CR / LF 结尾 / 合法 UTF-8；`cases=70` 命中 |
| B-3 | golden 与 `selfcheck-native.txt` 同源 | 同一份文件（`golden.rs:40` `include_str!`） | 成立 | sha256 `47043B1A57180A1CC0CDAECF00595C5A2AEAA5469D70B382EB017183B8195E3A`；提交版与工作区 Buffer 级比对 `相同=true` |
| B-4 | 三方 PNG 逐字节相同 | dx12 / vulkan / browser-webgpu | 成立 | 各 8789 字节；`fnv1a64=85bbc2017f35dda9`；`sha256=1e555d2266ba0f5b9deee605bb27dfde2dd1e69610cefb08ee96248ba2e4646b`；IHDR 256×256 / depth 8 / colortype 6 |
| B-5 | 浏览器截图元数据 | 1400×1278 / sha256 `e845a011…` | 一致 | 从提交里取 blob 后读 IHDR：1400×1278、depth 8、colortype 2、88779 字节、sha256 `E845A011…0DA0`，与其 json 声明一致 |
| B-6 | 7 条判据的字面量 | `says` / `command` 逐字 | 全部逐字相同 | 用去副作用的忠实副本动态 import：7 条 `says`、7 条 `command`、`title`、`source` **逐字相同**；7 个同名 `.txt` 均存在且首行 = `$ <command>`（此前怀疑的「记录旧、脚本新」不存在） |
| B-7 | `records/m0/README.md` 双向完整性 | 覆盖全部产物 | 成立 | README 提到的文件全部存在；目录内 20 个文件均有对应描述（含 `guard-*.txt`） |
| B-8 | 文本卫生计数 | 50 个文件 | **干净检出差 5（45）** | 见 F-5（口径差，非违规） |

### C. 守卫（对抗测试，7 个脚本、27 个注入载荷）

| # | 断言 | 我的独立结果 | 证据（注入 → 跑守卫 → 还原） |
|---|---|---|---|
| C-1 | core-purity 对真代码敏感 | **4/4 报出** | `#[cfg(unix)]` → 退出码 **1**，报 `crates/dhampir-core/src/readback.rs:391:1: #[cfg(unix)]`；`cfg!(unix)`、`#[cfg_attr(...)]`、空白变形 `# [ cfg ( unix ) ]` 同样报出 |
| C-2 | core-purity 对非代码不误报 | **7/7 绿** | 行注释 / 块注释 / 嵌套块注释 / 文档注释 / 字符串 / 原始字符串 / `'#'` 字符字面量 → 退出码 **0** |
| C-3 | 剥离器失步抗性（最容易被做假绿的路径） | **9/9 判对** | 反例：行注释里游离 `"` 后跟真 `#[cfg(unix)]`、块注释里游离 `"`、字符串里含 `/*`、字符串里含 `r#"` → **全部报出**（未失步）；正例：多行字符串（反斜杠续行）里的 `#[cfg]`、`r###"#[cfg(unix)]"###`、行尾注释里的 `#[cfg]` → 不报；属性跨行 `#[cfg\n(unix)]` → 报出 |
| C-4 | 豁免边界 | **6/6 报出（更严）** | `#[cfg(test)] #[cfg(unix)]` 同行、`#[cfg(test)]` 下一行紧接 `#[cfg(unix)]`、`#[cfg(test)] mod t { #[cfg(unix)] … }` 全部报出；仅「干净的 `#[cfg(test)]`」放行 |
| C-5 | dep-graph 抓反向依赖 | 报出 | 注入 `dhampir-timeline → dhampir-core` → 退出码 **2**（走其 `--self-test` 的「真实 manifest 被误报」路径）；`git checkout` 还原后退出码 **0** |
| C-6 | 依赖方向真实无环 | 成立 | 我独立读 5 个 manifest + 守卫输出：`dhampir-timeline`（无内部依赖）← `dhampir-media` ← `dhampir-core` ← {`dhampir-wasm`, `dhampir-worker`}；wasm 与 worker **互不依赖** |
| C-7 | wasm-only crate 的写法 | 成立 | `dhampir-wasm/Cargo.toml` 用 `[target.'cfg(target_arch = "wasm32")'.dependencies]`，未在代码里写 `#[cfg]` |
| C-8 | text-hygiene 抓脏字节 | **全抓** | CRLF / BOM / 孤立续字节 `0x80` / 截断多字节 `0xe4 0xb8` / Latin-1 中文 → 退出码 **1**（逐项各一次） |
| C-9 | text-hygiene 不误报二进制 | 成立 | `assets/probe.png` → 退出码 **0**；**同一份字节改名 `.txt`** → 被抓（说明按内容判定，不是按扩展名） |
| C-10 | 空集合拒绝 | **部分落空** | core-purity：`crates/dhampir-core/src` 存在但无 `.rs` → 退出码 **2**（明说「拒绝在空集合上通过」）；text-hygiene：只含脚本自身的根 → 退出码 **0**（见 F-2）；dep-graph：成员缺 `Cargo.toml` → 未捕获异常退出 **1**（见 F-3） |
| C-11 | 7 个脚本的 `--self-test` 规模 | **7/7 吻合** | `core-purity` 11；`dep-graph` 8 + 5（额外含「真实 manifest 无误报」）；`text-hygiene` 12 内存 + 2 磁盘；`record-acceptance` 17；`run-wasm-tests` 34；`serve-wasm-harness` 16；`capture-harness-screenshot` 19；**退出码全 0** |
| C-12 | 所有注入实验的现场还原 | 已还原 | 每个载荷后 `git checkout -- <file>` 并做字节级校验（还原后与初始内容 `相同=true`）；收尾 `git status --porcelain` 仅剩我在克隆里重跑判据 3 与浏览器复现造成的 4 个记录文件（预期内） |

### D. 浏览器宿主路径（我自己重跑，不是读记录）

| # | 断言 | 我的独立结果 | 证据 |
|---|---|---|---|
| D-1 | wasm 构建可出 web 包 | 成立 | `wasm-pack build crates/dhampir-wasm --target web --out-dir www/pkg --dev` → 退出码 **0**（`Done in 3.03s`） |
| D-2 | 自检页在浏览器里判绿 | 成立 | `node scripts/capture-harness-screenshot.mjs` → 退出码 **0**；`✓ 自检页判定：全部通过` |
| D-3 | 页面内的 golden 摘要 | 与我的独立值一致 | 页面 `golden c3f0da6b37577e55（期望 c3f0da6b37577e55）`——与我用自实现 FNV-1a 64 算出的值相同（跨实现互证） |
| D-4 | 页面内的 PNG 与 native 侧逐字节比对 | 成立 | `expected_png_match: true`、`browser_png_fnv1a64 = expected_png_fnv1a64 = 85bbc2017f35dda9`、`audit.ok: true`（期望值由本地服务从 `probe-native-dx12.png` 现算） |
| D-5 | 截图为整页且可复拍 | 成立（内容随运行变化） | 我的复拍 1400×1278 / 88773 字节，与提交版同尺寸不同字节（见 O-2）；我逐字看过这张图：三栏判定均绿、正文可读、`与 native 侧记录比对: 逐字节相同`（页面原文） |
| D-6 | 我复拍图与记录图的语义一致性 | 一致 | 除 `captured_at`、端口、时延（`canvas_render_ms` 1.80 vs 1.60、`total_ms` 65.10 vs 154.70）、epoch 秒外，`verdicts` / `status_bar` / `browser`（product `Chrome/153.0.8010.50`、revision `@583c5b46…`）**逐字段相同** |

### E. 工程规范 / CI / 文档（README 承诺的命令能不能原样跑通）

| # | 断言 | 我的独立结果 | 证据 |
|---|---|---|---|
| E-1 | `cargo fmt --all --check` | 成立 | 退出码 **0** |
| E-2 | `cargo clippy --workspace --all-targets -- -D warnings` | 成立 | 退出码 **0**（README 两条命令原样可跑） |
| E-3 | worker CLI 与 README 一致 | 一致 | `cargo run -q -p dhampir-worker --bin dhampir-render -- --help` → **0**，`--out` / `--backend` / `--probe-only` 齐备 |
| E-4 | Linux 交叉 check（plan `:135`） | 断言成立、**记录无产物** | `cargo check --workspace --target x86_64-unknown-linux-gnu` → **0**；见 F-1 |
| E-5 | CI 没有真实忽略 | 成立 | YAML `safe_load` 合法；`continue-on-error` 与 `|| true` **各 1 处、都在第 11 行注释里**；步骤引用的 `cargo fmt/clippy/check/test`、`node scripts/*.mjs`、`--print-locked-version`（实测 `0.2.128`）均真实存在 |
| E-6 | 命名层级（`yeki` 不得进引擎） | 成立 | 全仓 `yeki` 仅 4 处，全在文档里**声明它不得进引擎**（根 `README.md:19`、`tech-guide.md:51/89`、`plan:82`）；crate 名只有 `dhampir-*` 与 bin `dhampir-render`，无词源彩蛋名 |
| E-7 | `plan/video-editor-plan.md` 勾选未虚报 | 成立 | 环境清单（`:47-52`）、仓库侧建仓/占名（`:65-66`）、T0.1（`:78`）**均为未勾选 `- [ ]`**（T0.1 注明「⏳ 仍待用户决定」）；M0 退出标准三条（`:135-138`）为 `- [x]`，且 `:135` 的 Linux 交叉 check 与 `:113` 的 T0.6 均配了免责标注（`:115` 明确「⚠️ 尚未在真 runner 上跑过」，`:116` 只是本机预验），**未把未完成项勾成完成** |

**附注（磁盘实测）**：复核结束时 `target/review-m0/` 合计 **3404290106 字节（约 3.17 GB）**，含克隆源码、独立构建缓存与临时工具；未越过 6 GB 预算。真实仓库的 `target/` 只在 O-4 披露的那次意外中被使用过一次（非我主动发起），此后所有构建都走独立 target。

---

*复核者：独立验证会话。报告生成时间戳见文件系统；所有实验可重放：报告内每条命令都可直接粘贴执行（Windows PowerShell + 已装 Rust/Chrome/wasm-pack/wasm-bindgen 0.2.128）。本报告不含对被测仓库的任何修改建议的实现，仅列发现。*
