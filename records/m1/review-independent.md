# M1 独立复核报告（对抗式）

复核人：独立 verifier（与 M1 记录的生产者不是同一个会话，也不复用它的推理链）。
复核信条：**先设法推翻结论，推不动才记 PASS；推不动但也没推到底的地方，写进「未验证」。**

## 0. 被审对象与复核环境

| 项 | 值 |
| --- | --- |
| 被审提交 | `89708db`（「M1：服务端 headless wgpu 基线」，199 个文件） |
| 复核基线 | `7fc543b`（= HEAD） |
| `89708db → HEAD` 的差异 | 只动 3 个文件：`records/m1/acceptance.json`、`records/m1/native-tests.txt`、`records/m1/wasm-check.txt`——是**验收快照重跑**，不是内容改写（我自己重跑的 `wasm-check.txt` 与 `89708db` 版一致、与 HEAD 版字节不同） |
| 提交链 | `98b517a`(M0) ← `855cc18`(守卫修复) ← `e70466c`(M0 复核归档) ← `89708db`(M1) ← `7fc543b`(记录刷新) |
| 复核时间 | 2026-09-22 05:0x–06:2x +08:00（本机时钟） |
| 工具链 | Node v25.5.0；cargo / rustc 1.97.0 |
| 硬件 | RTX 4070。DX12 腿 `driver=32.0.16.1074`，Vulkan 腿 `driver=NVIDIA/610.74`——两条腿都是**真跑 GPU**，不是软渲染 |
| 复核方式 | 一切实验在 `target/review-m1/clone`（我自己 clone 的副本）里做；构建走 `CARGO_TARGET_DIR=target/review-m1/target`；判定一律看**退出码**；自写工具先自校（公开测试向量 / 第三方 oracle）再上岗 |
| 真实仓库改动 | 复核期间 `git status --porcelain` = **0 行**；本报告是**唯一**新增文件 |

## 1. 结论

**PASS。** 八组断言我都没能证伪：M1 的验收结论（Windows 两条腿可复现、判定链自洽、记录没有粉饰）**站得住**。
另外查出 **2 条中等严重度的守卫缺口** 和 **4 条低严重度的文档/数字瑕疵**——它们不推翻 M1，但会在**下一次**记录里变成假绿通道或对不上的数字。

| # | 断言 | 我的判定 | 独立证据（摘要） |
| --- | --- | --- | --- |
| 1 | corpus 稳定性 | 成立 | 我自己驱动 4 次运行：`p1-fresh` exit 0、`p2` 与 `p1` 逐字节+逐像素 0 差异、`p3` 与归档（`--compare-run records/m1`）exit 0、`p4`（故意给文件路径）exit 1。两侧整表摘要同为 `71ecc80cade3d73d` |
| 2 | 摘要数字 | 成立 | 自写 FNV-1a64（5 条公开向量自校通过）算 `selfcheck-native.txt` = `c3f0da6b37577e55`（4061 B、72 个 LF、70 条 `tb=`、`cases=70`）；自写 PNG 解码器（只用 `node:zlib`）对 12 张归档 PNG 与 GDI+/.NET `System.Drawing` 的 RGBA 像素 SHA256 **12/12 相同** |
| 3 | 判定链 | 成立 | 独立复算器对归档与 `p3` 复跑均 exit 0；两条腿各 `frames=80 points=368 files=80`，mismatch 计数全 0，`anchorBad=0`。**增量证据**：把 368 个 `measured` 与我自己解码的像素按 `(y*256+x)*4` 逐点对齐，**0 处不符**（仓库守卫不做这一步） |
| 4 | 验收链 | 成立 | `acceptance.json`：`green=true`、`exit_code=0`、`commit=89708db8fca6`、`dirty=false`、9 条判据全 `ok/exit 0`；我重跑 `cargo test --workspace`，13 个 `test result:` 行求和 = **134 passed / 0 failed**，与记录一致；克隆里 `check-m1-record.mjs --record records/m1` exit 0（31 项） |
| 5 | 守卫对抗 | 守卫能红，但**有 2 个缺口**（发现 1、2） | 记录侧 24 次 + 守卫侧 20 次注入实测：改数字/删帧 PNG/翻 PNG 字节/截断 JSON/改摘要/清空非确定项 → 全 exit 1；坏参数、缺目录、空集合 → 全 exit 2 |
| 6 | 两个边界问题 | (a) 自述与行为**一致**；(b) **缺口成立** | (a) 空扫描根在合成根上实测 exit 2（`decideScan` 的「拒绝在空集合上通过」真的会走）；(b) 情形 A（目录在、无 manifest、不在 members）实测 **exit 0 静默绿**，情形 B（有 manifest 不在 members）exit 1 |
| 7 | plan 一致性 | 成立（4 处数字/描述有瑕疵） | T1.1–T1.5 全 `[x]`；退出标准第 1、2 条仍是 `- [ ]` 且带 `⏳`；§4 尾句「在那之前不进 M2」未被越过，§5 T2.* 全 `- [ ]`，`records/m2` 不存在；`wsl -l -v` exit 1（无发行版）复现 |
| 8 | M0 冻结面 | 成立 | `git show --name-status 89708db -- records/m0` **为空**；M0 之后 `records/m0` 只多出 `e70466c` 新增的 `review-independent.md` |

## 2. 发现（按严重度）

### 中-1 `check-m1-record.mjs` 的诚实性闸门能被一个**空壳 `linux/` 目录**绕过

- **复现**（克隆的 `target/review-m1/mut/work` 副本里，脚本自读 `REPO_ROOT`，不需要改脚本）：`mkdir records/m1/linux`（空的）+ 删掉 `README.md` 里全部 `⏳` → `node scripts/check-m1-record.mjs --record records/m1` → **exit 0，31 项全绿**。
- **根因**：`scripts/check-m1-record.mjs:1015` 的 `const hasLinux = subdirNames.some((name) => /^linux/i.test(name));`——只看子目录**名字**，不看腿里有没有东西。而腿清单是硬编码的 `LEGS = ['dx12','vulkan']`，空 `linux/` 永远不会被加载、也不会被报。
- **为什么算「中」**：这条闸门存在的唯一理由，就是拦住「只留两条腿、README 却写成四种环境通过」。现在加一个空目录即可把它关掉，而 31 项**仍然全绿**——即**假绿是可构造的**。
- **现状澄清**：真实记录里**没有** `linux/`，README 的 `⏳` 齐全。本发现**不是**在说 M1 造假，而是在说这条防线可绕，而 M2 记录会复用同一个守卫。

### 中-2 `check-dep-graph.mjs` 的「磁盘 / members 一致」只覆盖**有 manifest** 的目录

- 情形 B（目录有 `Cargo.toml`、但不在 workspace `members` 里）→ 实测 **exit 1**，错误文本同时列出磁盘清单与 members 清单（这条做得好）。
- 情形 A（`crates/foo/` 目录**存在**、**没有** `Cargo.toml`、也不在 members）→ 实测 **exit 0 静默绿**。根因：`diskCrates()` 只把**有 `Cargo.toml`** 的目录算作 crate。
- **影响**：一个「半拉子 crate 目录」（建了目录、写了源码、manifest 忘了放或被误删）不会触发这条守卫，也不会触发 `core-purity`（它只扫 `dhampir-core` 的源文件）。M1 记录里 5 个 crate 都完整，未触发。

### 低-1 plan §T1.4 说 `adapter.json` 与 `timing.json`「键不重叠」——实测**重叠 10 个键**

- 实测（两条腿一样）：`adapter.json` 18 键、`timing.json` 15 键，**交集 10 键**：`adapter_name, backend_slug, build_profile, kind, milestone, nondeterministic_fields, requested_backends, schema, unix_epoch_millis, unix_epoch_seconds`。
- 其中 8 键值相等；2 键**值不同**且本就是各写各的：`kind`（`"adapter"` vs `"timing"`）、`nondeterministic_fields`（两份各自声明自己的非确定项）。`unix_epoch_millis` 两文件相同（这正是计划想要的）。
- **判读**：**描述不准确**，不是风险（`kind` 能区分来源）。但计划文本自身也矛盾：同一句里既说「两份共用同一个 `unix_epoch_millis`」又说「键不重叠」。

### 低-2 plan:211「全目录 183 文件 / 1.1 MB」——实测 **184**

- 磁盘、`89708db` 的 git 树、`HEAD` 的 git 树**三处都是 184 个文件 / 1152126 字节（1.10 MB）**。差 1，不是统计口径问题。
- 备查：`records/m0` = 21 文件 / 173937 字节。

### 低-3 plan:188「`shaders/scene.wgsl`（254 行）」——**口径**需要说明

- `crates/dhampir-core/src/shaders/scene.wgsl`：14141 字节、**253 个 LF、末字节就是 LF**（即 253 行文本）；`split('\n')` 才得 254（多一个尾随空串）。工作树 / `89708db` / `HEAD` 三者**字节完全相同**。
- **判读**：254 = 「按 `\n` 切分的元素数」，而编辑器与 `wc -l` 会给 253。不是造假，是口径；但同一份计划里「72 行探针报告」用的是 LF 口径，两种口径混用会让人对不上数。

### 低-4 本机其实**有** `docker`，与 plan:201「本机无 docker」有张力

- `Get-Command docker` → present；`wsl -l -v` → exit 1（无发行版，这条与自述相符）。
- **判读**：我**没有**深挖这个 `docker` 是不是真引擎（也可能是 Podman / Desktop 的壳或一个 stub）。即便它在，也改变不了「Linux 容器里注入 GPU」是另一件事——**⏳ 的合理性不受影响**，只是「本机无 docker」这半句在本机上核不实。

### 观察-1 `--compare-run` 只认**目录**，而任务书措辞像文件路径

- 我按任务书的字面先给了**文件**路径 `records/m1/dx12/run.json` → exit 1，报「比过 0 个后端、没比 1 个」；改给目录 `records/m1` → exit 0。
- CLI 的行为是对的（给文件路径无法定位腿，应该红），记的是**措辞歧义**：文档读者很容易照字面传文件。

### 观察-2 「文本卫生 57 个文件」我**在克隆里复现不出来**，原因已定位

- **真实仓库整树 = 57 个文本文件 / 0 违规**——这是我用**自己的**枚举 + **自写**的字节检查（BOM / CR / 严格 UTF-8）跑出来的，不是引用守卫的话。
- **克隆整树 = 52 个**；差集**恰好**是 5 个未跟踪、被 `.gitignore` 忽略的 `crates/dhampir-wasm/pkg-node/` 产物：`.gitignore`、`dhampir_wasm.d.ts`、`dhampir_wasm.js`、`dhampir_wasm_bg.wasm.d.ts`、`package.json`。
- 所以：`records/m1/guard-text-hygiene.txt` 的「57」是**整树口径（含未跟踪文件）**，此刻在本机仍可复现；而被审提交的**被跟踪文本面 = 52 个文件，同样 0 违规**。**两者都干净**，但不要把 57 当成提交的属性（它是「某次 dirty 运行的整树快照」）。
## 3. 未验证项（我明确没做、或做不到的）

1. **Linux 两条腿（容器 GPU / lavapipe）——没跑。** 本机 `wsl -l -v` exit 1（无发行版）；我也没有去验那个 `docker` 是否真能用、更没有容器里注入 GPU 的能力。所以我**只能验「这两条腿的缺口被如实记着」，不能替它补跑，也不能否证「缺的是环境不是代码路径」**。可读到的**间接**支持：core 里无 `#[cfg]`（守卫 9 文件全绿）、wasm32 交叉编译与跨运行时 golden 都在绿——仅此而已。
2. **别的 GPU / 别的驱动栈**——只有 RTX 4070 一台机器，`adapter.*`、`init_ms` 这类「换机器就变」的字段无法验其敏感性。
3. **`budget_ms = 10` 这个数本身的合理性**——我只验证了判据口径（`worst_roundtrip_ms <= budget_ms`）会被守卫**重算**、且 `timing.json` 里的 `verdict` 与我的重算一致；没有论证 10 ms 从哪来、是否该更紧/更松。
4. **`acceptance.json` 的 `seconds` 字段**（0.2 / 0.7 / 2 秒等）——只核了退出码与「134 passed」的求和，没核时间。
5. **`records/m0` 的内容级正确性**——本次只核「M0 面是否被冻结」（结论：冻结），没有重审 M0 自己的结论。
6. **CLI 未穷举**——我按契约跑了 9 条（含 6 条错误路径），没有做参数空间穷举。
7. **守卫的 `--self-test` 我只看了退出码**（四个全 0），没有逐条走读它们的用例语义（40 / 12+3+4 / 8+5+3+3 / 11 条）。

**补做后销掉的一条未验证项**：wasm 侧记录的可重生成性——我在克隆里跑了 `node scripts/run-wasm-tests.mjs --out records/m1`：**exit 0**，4 个 wasm 测试全过（2 个目标、与源码 4 个 `#[wasm_bindgen_test]` 对账一致）；把重跑结果与归档逐文件比：184 个文件里 **183 个逐字节相同**，唯一变化的是 `wasm-tests.json` 里 3 处**绝对路径**（shim 与两个 `.wasm` 落在克隆的 `target/` 下）。`cross-runtime.txt` **逐字节可复现**。→ 这条从「未验证」升级为「已复现」；仍未覆盖的是「换平台」（只在 Windows 上跑过）。

## 4. 自曝（这次复核自身的问题与边界）

1. **我的第一批注入有 4 例打偏，是我的写法错、不是守卫失效。** 我按 `run.json` 顶层写 `counts.points`，真实结构是 `backends[0].counts.points`，于是那几例的 `actual` 无法解释成守卫的结论。我重做了第二批（`mut-report-m1b`），7 例全部落在真实结构上。第一批里**不依赖该结构**的例子（删帧 PNG / 翻 PNG 字节 / 删 `⏳` / 改摘要 / 截断 JSON / 改 CLI 调用）结论仍有效。
2. **我改过盘面，所以「全绿」必须限定在哪个盘面上。** 注入都在克隆的 `mut/work` 副本里做，每轮前后用 `cpSync` 还原；真实仓库全程 `git status --porcelain` = 0 行（写本报告前）/ 1 行（写完本报告后，即本文件本身）。
3. **我的 FNV 只有一个 runtime 的实现**：`tools/fnv-ps.ps1` 有传参 bug（`$args[0]` 没接住路径，所有结果退化成 offset basis），我**没有**修它，所以 FNV 侧只有「自实现 + 5 条公开向量自校」，没有第二实现交叉。像素侧的第二 oracle 是 GDI+（.NET `System.Drawing`），那条是**真独立**的。
4. **我自己也踩过口径错，草稿里有两处数字是错的**：一处记成「`selfcheck-native.txt` 无尾随换行」（实为末字节就是 LF），一处记成「`scene.wgsl` 在 `89708db` 是 264 个 LF」（实为 253，与工作树、HEAD 完全一致）。**本报告里的数字都是重测后的那一版**；这两处我在写正文时已改对。
5. **我在被审目录里新增了本文件（本次唯一写入）。** 我确认过它不扰动任何守卫：`check-m1-record.mjs` 的诚实性检查只读**子目录名**、各腿只读固定文件名（`adapter.json`/`run.json`/`timing.json`/`compare.json`/`readings.txt`/`frames/`），`check-text-hygiene.mjs` 的 `SKIP_DIRS` 含 `records`。但它会把 `records/m1` 的文件数从 **184 变成 185**——**写完本报告之后，任何引用「184」的地方都要跟着改口**（这也正是低-2 那条数字争议会变复杂的地方）。
6. **判定一律走退出码，不看文案**：所以「守卫报了什么」这类结论只在我**真读过**那条文案时给出，其余只给退出码。
7. **我没有逐个走读 `89708db` 的 199 个文件。** 我的覆盖面是：`records/` 全部记录、`scripts/` 的四个守卫 + 验收脚本、关键 crate 源文件（`scene.rs` / `baseline.rs` / `scenes.rs` / `main.rs`）、`plan/`。**没有走读**的是 wgpu 渲染管线的具体实现（shader 数学、混合状态细节）——对它的验证方式是**端到端**的（368 个采样点的「GPU 实际像素 ↔ f64 模型」逐点对齐，容差 1），不是读代码。
8. **有一条我是「发现张力后主动停手」的**：低-4 的 `docker`。继续查下去要动本机环境（真的起容器），超出「只读复核」的边界。
## 5. 工具与证据清单（全部在 `target/review-m1/`，**没有**进仓库）

| 工具 | 干什么 | 自校方式 |
| --- | --- | --- |
| `tools/fnv64.mjs` | FNV-1a 64 | 5 条公开向量 + 「分段算 = 一次算」的增量性质，0 失败 |
| `tools/png.mjs` | PNG 解码（只用 `node:zlib`，含 5 种 filter 与 CRC 校验） | 与 GDI+ 逐像素比（见下） |
| `tools/gdi-oracle.ps1` + `tools/oracle-check.mjs` | 第三方 oracle：.NET `System.Drawing` 解码 → RGBA SHA256 | 12 张归档 PNG **12/12** 与我的解码器一致 |
| `tools/verify-record.mjs` | 独立复算 `run.json` ↔ `readings.txt` ↔ PNG 三方差分；并做 `measured ↔ 像素` 逐点对齐（368 点） | 对归档与 `p3` 复跑均 exit 0；368 点 0 处不符 |
| `tools/run-corpus.mjs` | 我自己驱动 4 次真跑 GPU（`p1` 全新 / `p2` 与 p1 比 / `p3` 与归档比 / `p4` 故意给文件路径） | 退出码 |
| `tools/cli-contract.mjs` | 9 条 CLI 契约（含 6 条错误路径） | 退出码 |
| `tools/mutate-m1.mjs` / `mutate-m1b.mjs` | 针对记录的注入（两批，共 24 次） | 退出码 |
| `tools/mutate-guards3.mjs` | 针对三个守卫的注入（20 次，含 4 次 `--self-test`） | 退出码 |
| `tools/hygiene-scan.mjs` | 复刻守卫声明的文本文件管辖范围 + **自写**字节检查（BOM / CR / 严格 UTF-8） | 真实仓库 57 文件 0 违规；克隆 52 文件 0 违规 |
| `tools/recheck-numbers.mjs` / `recheck-keys*.mjs` / `summarize-reports*.mjs` / `list-textfiles*.mjs` | 写报告前把每个要引用的数字**重测一遍**的取证脚本 | 输出即证据 |

证据文件：`corpus/{p1,p2,p3,p4}/`、`corpus/runs.json`、`guard-m1-record-myclone.txt`（克隆里 31 项全绿的原始输出）、`mut-report-m1*.json`、`mut-report-guards3*.json`、`cli-report.json`、`hygiene-scan.json`、`wasm-backup-records-m1/`（跑 wasm 重生成前的归档副本，用于逐字节比对）。

### 复核边界声明

- 本报告的每条结论都能追到上面某个产物或某条退出码；**没有引用生产者的话作为证据**。
- 我**没有**修改 `89708db` 的任何字节；唯一写入是本文件。
- 结论的**时效**：与硬件、本机环境绑定的部分（GPU 两条腿）只在「2026-09-22 这台机器上」成立。