# dhampir · 剩余任务交接记录（M2 暂停点）

写于 **2026-09-22** ｜ HEAD `63782435ffa2e0c2bf923f503919b60da45a7f5a`（`6378243`）｜ 工作区 **27 项未提交**（含本文件）

> **2026-09-22 后续重整（两轮）**：
> ① 交付物收窄为**技术调研 + 一个底座**——不需要服务器的整体架构；M5/M7 移出，M6 部分移出。
> ② 范围收敛为**只做本地处理**（不发布、不建远端仓库、不接远端 CI），并清除与实现无关的命名来源信息。
> 详见 §2「范围」与各章的就地标注。

> **这份文件是什么**：暂停点上「还剩什么没做完」的记录，供下一个会话直接接手。
>
> **它不是什么**：不是决策真相。真相是 `plan/video-editor-plan.md`（下称 **plan**）与
> `plan/video-editor-tech-guide.md`（下称**指导文档**）。本文件只补 plan 里没有的两样东西：
> ① 在飞半成品的**准确状态**；② **开工顺序**与每步的**验收口径**。
> 与 plan 冲突时以 plan 为准，并把本文件改掉。

---

## 0. 一分钟版本

- **M0、M1 已收官并已提交**（`records/m0/`、`records/m1/` 都在库里，M1 守卫 `scripts/check-m1-record.mjs` 跑真记录应 EXIT=0）。
- **M2 的工程活已全部做完**，plan §5「退出标准」**4/4 已勾**，证据落在 `records/m2/`（**230 个文件**）。
  （**引用一律用 § 与任务号，不再写行号**——plan 本轮被重整过，行号会漂。）
- **M2 只剩四件收尾**，其中**代码活只有第一件**（守卫下半截），其余是跑验收、提交、独立复核。
- ⚠️ **最危险的一条**：`scripts/check-m2-record.mjs` 现在**没有 `main()`** ——
  `node scripts/check-m2-record.mjs`、`--self-test`、`--help` 三条命令**全是零输出、EXIT=0**（实测）。
  此刻跑 `node scripts/record-acceptance.mjs --milestone m2`，会把 `guard-m2-record` 与
  `guard-m2-record-self-test` 两条判据记成 `ok: true` —— **产出一份假绿的验收快照**。
  **顺序不能颠倒：先补 `main()`，再跑验收。**
- **M3–M4 未开始**（plan §6–§7）；**M5–M7 已移出本仓库**（plan §8–§10 保留为下游形态参考）。
- 另有**两项早先挂起的作业**：M1 的 Linux 两条腿（用户已决定延期）、plan §5 T2.6 一处疑似陈旧的 ⏳ 标记。
- **本轮已重整**（2026-09-22）：plan §1 重写为「带状态的总览 + 当前进度 + 采纳的建议」；
  M5/M6/M7 正文移入 plan **附录 B**；[`foundation-architecture.md`](./foundation-architecture.md)
  是**实测**的底座架构。架构建议 **R1–R7** 已并入 plan §1.2 与下面 §4 的开工顺序。
- **交付物已收窄为「技术调研结论 + 一个底座」**：`dhampir-core` + 两个宿主（preview / render）+ 时间线契约。
  **不需要服务器的整体架构**——网关 / 任务队列 / 对象存储 / 部署 / 分片编排 / 影子环境与发布 / 编辑 UI 产品化
  **全部移出本仓库**，留给下游工程（下游可接成客户端-服务端分离，或本地预览 + 渲染合并）。
- **只做本地处理**：不发布 crates.io、不建远端仓库、不接远端 CI。原 T0.1「命名落地」的发布/建仓部分
  **整条移出范围**（名称 `dhampir` 已定并落地）——是**移除**不是完成。

---

## 1. 当前坐标（全部实核过）

| 项 | 真实值 |
|---|---|
| HEAD | `63782435ffa2e0c2bf923f503919b60da45a7f5a` |
| 工作区 | 27 项未提交：**13 改 + 1 删 + 13 新**（文档重整后：+`README.md`、+`plan/video-editor-tech-guide.md`） |
| `records/m2/` | 230 个文件：`browser/` 86、`browser-amd/` 87、`framediff/` 3、`framediff-crossvendor/` 52、根 `README.md`(7952 B) + `wasm-tests.json`(3959 B) |
| 守卫脚本 | `scripts/check-m2-record.mjs`：**184617 字节 / 3816 行 / 61 个具名导出**，LF、无 BOM、结尾停在第 3813 行 `}`（`checkRecordHonesty`）——**不可执行** |
| 缺的记录件 | `records/m2/acceptance.json`、13 份 `<判据 id>.txt`、`records/m2/review-independent.md` |
| 退出标准 | M2 4/4 ✅；M3–M4 未开始；M5–M7 移出范围 |

**未提交清单**（`git status --porcelain` 实测）：

- 改（13）：`crates/dhampir-core/src/{gpu.rs,render/corpus.rs,render/mod.rs}`、
  `crates/dhampir-wasm/src/{lib.rs,web.rs}`、`crates/dhampir-worker/src/{baseline.rs,main.rs}`、
  `plan/{video-editor-plan.md,video-editor-tech-guide.md}`、`README.md`、
  `scripts/{capture-harness-screenshot.mjs,record-acceptance.mjs,run-wasm-tests.mjs}`
- 删（1）：`crates/dhampir-worker/build.rs`
- 新（13）：`crates/dhampir-core/build.rs`、`crates/dhampir-wasm/src/corpus.rs`、`crates/dhampir-wasm/www/corpus.html`、
  `plan/{remaining-work.md,wgsl-portable-subset.md}`、`records/m2/`、`scripts/check-m2-record.mjs`、
  `scripts/dhampir-framediff.mjs`、`scripts/dhampir-wgsl-census.mjs`、
  `scripts/framediff-thresholds-exact.toml`、`scripts/framediff-thresholds.toml`、
  `scripts/run-browser-corpus.mjs`、`scripts/serve-corpus-harness.mjs`

---

## 2. 剩余任务

### P0-1 守卫下半截：把 `check-m2-record.mjs` 补成可执行的

**为什么它排第一**：它现在是个**静默报绿的半成品**——最危险的失效模式。验收快照口径是「退出码 0 = 绿」，
所以只要它还没 `main()`，跑出来的 `acceptance.json` **必然是假绿**，而这份快照正是 M2 的验收产物。

**缺的五件东西**（实核：全文 `MUTATIONS` / `runSelfTest` / `function main` / `process.exitCode` 出现次数**均为 0**）：

1. `MUTATIONS` —— 反向用例表
2. 合成模型 —— 合成腿 / 合成 spec / 合成 framediff 目录 / 合成整份记录模型
3. `runSelfTest()` —— 返回 `{ failures, count }`（照 M1 的 `scripts/check-m1-record.mjs` L1528 同形）
4. `main()` —— 参数解析 + 真跑
5. 结尾 `process.exitCode = main();`

**覆盖要求：37 个检查项，每一项都要有反向用例**（数量须由自检**实数上报**，不能写死）：

| 组 | 数量 | id |
|---|---|---|
| `LEG_CHECKS` | 15 | required-files / run-shape / frame-set / png-bytes-and-digest / pixels / repeat / frames-digest / counts / points / measured-vs-png / readings / adapter / host-gpu / screenshot / rerun-repro |
| `NATIVE_CHECKS` | 3 | archive / frames-digest / readings（结果 id 带 `native-` 前缀） |
| `FRAMEDIFF_CHECKS` | 8 | dir-listing / inputs / thresholds / summary-csv / verdict-json / report-txt / shape-json / diff-images |
| `CROSS_CHECKS` | 6 | browser-vs-native-bytes / amd-vs-native-readings / adapter-drift / screenshot-drift / run-json-drift / leg-distinctness |
| `HONESTY_CHECKS` | 5 | root-listing / readme-claims / wasm-tests / acceptance / review-independent |

**已定好的实现路线**（省一次试错）：合成模型走「**字段自洽 + 字节由守卫现编**」——
用守卫自己的 `encodePng` 现造 PNG，让 `pixels` / `measured-vs-png` / `frameImage` 缓存 /
`framediff` 重算层**走与真跑同一条代码路径**，而不是给这些检查项开旁路。

**合成模型必须满足的硬约束**（这轮通读守卫时逐条核实出来的，写在别处会丢）：

- `spec` 是 `checkLegModel(leg, spec)` 的**第二个参数**，大量检查项读 `spec.*`
  （`framesPerScene`/`frameRange`/`targetSize`/`points`/`pngBytes`/`readingsBytes`/`readingsLines`/
  `framesDigest`/`requestedBackends`/`buildProfile`/`wgpuVersion`/`inPage.*`/`gpu.*`/`extraArgs`/
  `findingsFalse`/`auditFindingsFalse`/`rerunRepro`/`probeDigest`…）。
  **必须配一份合成 spec**；传 `undefined` 会让这些项**抛异常变红**（不是静默通过）——这条行为本身也该有用例。
- `run.json` / `adapter.json` / 截图 json 的键必须**逐字满足 `KEYS`**（`exactKeys`，L1522–1592）；两条真腿的
  `milestone` 是 `M1`（表契约），截图 json 是 `M2`（运行时间）；`nondeterministic_fields`：run 是**空数组**、adapter **恰好 5 条**。
- `backend.adapter_name` 必须 `null`；`adapter.producer='browser-webgpu/wasm32'`、`adapter_name=null`、
  `naga_version=null`、`timing_record=null`；`adapter.adapter` 端口必须是「空/零」那一组
  （device `'0'`、device_type `'Other'`、driver `''`、driver_info `''`、name `''`、vendor `'0'`、backend `'BrowserWebGpu'`）。
- `gpu_identity.state='unresolved'` / `resolved_by='harness'` / `resolves_to='host-gpu.json'`；
  `in_page.is_fallback_adapter=false`。
- `host-gpu.json` 的 `resolved` 必须按 `deviceString`+`driverVersion` 在 `devices` 里命中（**不**断言 `driver_vendor`）。
- `scenes[].size` 必须等于 `spec.targetSize`；每帧采样点数 × 16 == `points`（368 → 5/5/5/4/4 = 23 点/帧）。
- `pixels` / `measured-vs-png`：同一张 PNG 在 `(x,y)` 处的 RGBA **必须逐字等于** `point.measured`
  —— 所以合成 PNG 的像素要**按采样点位置现编**。
- `renderReadings`（`readings.txt`）：首行 `dhampir M1 corpus 逐点读数`；帧标题带
  `（<description>；同帧两次渲染逐字节一致）`；逐点行按 **Unicode 标量**填宽（`pad`/`codePointLength`）；
  块尾一个空行；`join('\n')` 且**结尾有换行**。
- `frameSetDigest` 配方：按**配对顺序**拼 `文件名 + 0x00 + 文件字节`。
- `framesDigest` 配方：**按表序**拼 `场景名 + 0x00 + 帧号 LE u32 + 像素摘要 LE u64`（12 字节 scratch）。
- 帧文件名只需满足 `^([A-Za-z_][A-Za-z0-9_]*)-f(\d+)\.png$`（`pairNames` 靠命名配对，`assertPairing` 靠尺寸与解码校验）。
- SSIM 窗 **11×11**、只取 valid 区域 → 合成帧**不能太小**（真记录是 256×256，合成也别显著小于它）。
- 合成腿的 `frames/` 里**不能混非 PNG**（`required-files` 会红）；`ignoredFrames` 支路要**另造一个模型**单独测。
- `screenshot.version.captured_content.height > viewport.height` 必须成立。
- **合成腿复现不了的一处**：两条真腿 `readings.txt` 的差异形状是「**38 行不同 / 首个第 22 行（0 基）**」，
  来自 `gradient f2 (200,128)` 的 244 vs 245。`amd-vs-native-readings` 这一支要么**拷真字节**做子模型，
  要么只断言该检查项的其余分支——别硬造。

**尚未覆盖的分支**（补 `MUTATIONS` 时一并补上）：

- `acceptance` 判据：**两类**——「在且合规」与「在但坏」（`dirty: true`、短于 1024 字节、无 PASS/FAIL/通过/不通过 结论词、
  **有 `.txt` 却无 `acceptance.json` 的半份记录**）；含 `ACCEPTANCE_ITEM_*` 的缺栏/多栏/`ok !== true`/`exit_code !== 0`。
- `review-independent` 判据：同样两类。
- `wasm-tests` 判据：`milestone !== 'M0'`、`listed_tests.length !== passed`、源码里少数一条/多数一条测试。
- `recordByteHygiene`：BOM、CR；`flattenProse`：折行与**原文行号**（`flattenProse.lineOf`）。
- 传 `undefined` spec 的行为（应抛异常 → 红，而不是静默过）。

**`main()` 的退出码约定（照 M1）**：不认识的参数 → **2**；`--help` → **0**；自检失败 → **2**
（且**先打印「先修守卫，别信它的结论」并逐条列出失败**）；记录目录不在 → **2**；缺腿 → **2**；0 张 PNG → **2**；有红项 → **1**。
**只用 `process.exitCode`，绝不 `process.exit()`**（本机 win32 + Node 实测会把退出码变成 `-1073740791`）。
**空文件集绝不允许通过。**

**做完的验收口径**：

```
node scripts/check-m2-record.mjs --self-test        # 打印自检条数 + 全绿
node scripts/check-m2-record.mjs --record records/m2 # EXIT=0，逐项给出 37 个检查项各自的结论
```

外加**反向验证**：随便抠掉/改坏一个检查项，必须**因为那一条**变红（且自检要跑完，不能半路炸）。

---

### P0-2 落验收快照（守卫可执行之后才做）

```
node scripts/record-acceptance.mjs --milestone m2
```

产出 `records/m2/acceptance.json` + `records/m2/<判据 id>.txt`（**13 份**）。13 条判据 id（守卫 `EXPECTED.acceptanceIds`
与 `record-acceptance.mjs` 的 m2 清单**已逐字核对一致**）：

```
native-check / native-tests / wasm-check / cross-runtime /
guard-core-purity / guard-dep-graph / guard-text-hygiene /
guard-m1-record / guard-m1-record-self-test /
framediff-self-test / wgsl-census /
guard-m2-record / guard-m2-record-self-test
```

- **顺序**：`records/m2/README.md` 已改过（7952 字节）→ 验收快照**必须在守卫补齐、README 定稿之后重跑**才作数。
- `acceptance.json` 的 `dirty` 字段**剔除 `records/`**（否则自己写完自己就是脏的）。
- 这 13 份 `.txt` 与 `acceptance.json` 在守卫的 `root-listing` 里是**允许但非必需**的
  （`EXPECTED.acceptanceIds` + `optionalRootFiles`，L3567–3568）；但**一旦在，就必须整份齐、整份绿、
  id 一条不差、每项都有对应 `.txt`**——半份记录是红。
- 隐含前提：`guard-m2-record` 与 `guard-m2-record-self-test` 这两条**自己不先绿，验收快照就不可能绿**。
  这也是「先 P0-1、后 P0-2」的硬理由。

---

### P0-3 提交

把上面 27 项未提交改动提交（建议按主题拆：`core/wasm corpus 共用`、`浏览器取证驱动`、`framediff 工具 + 两份档`、
`WGSL 子集文档 + 普查工具`、`records/m2 + 守卫`、`文档重整：去命名彩蛋 + 收敛为本地处理`）。提交前跑一遍
`node scripts/check-text-hygiene.mjs`（本文件也在它的管辖范围内：LF + 无 BOM + 合法 UTF-8）。

---

### P1 M2 独立复核（verifier 子代理）

M0 与 M1 各有一次先例（子代理 + `records/m1/review-independent.md` 17049 字节）。
M2 这次还没发起。产出 `records/m2/review-independent.md`，守卫对它的要求只有三条：
**≥1024 字节**、**至少提到一个本记录钉住的值**（`EXPECTED` 里的任一摘要，或 `1105968`）、**含结论词**
（PASS/FAIL/通过/不通过）。复核范围建议：**别信勾选，信产物** —— 从 `records/m2/` 重新算一遍，
重点打守卫自己的可信度（自检是否真能证伪、37 项是否每项都有反向用例、空帧集是否真被拒）。

---

### P2 M3–M4（未开始；M5–M7 已移出本仓库）

| 里程碑 | plan 位置 | 内容 | 前置 |
|---|---|---|---|
| **M3** 浏览器预览链路 | §6 | 前置 spike **S3.1 源帧采样策略三选一**（本里程碑最重要决策，结论必须落文件）+ S3.2 proxy 生成规格 + S3.3 VideoFrame 生命周期；任务 T3.1 `web/` 骨架 / T3.2 mp4box+H.264 硬解 / T3.3 external texture 上屏 / T3.4 帧缓存 LRU / T3.5 测量。退出标准 4 条 | M0（可与 M1/M2 并行） |
| **M4** 契约闭环 | §7 | T4.1 schema v1 定稿 / T4.2 core 渲染图 v1（多轨+1 特效+转场+关键帧）/ T4.3 wasm 接时间线 / T4.4 worker 出片（**必做 `--dump-raw`**）/ T4.5 样本工程 + 双端比对。退出标准 4 条 | M2 + M3 |
| ~~**M5**~~ 分布式分片渲染 ⛔ | 附录 B.1 | **已移出本仓库**——分布式编排属服务器整体架构，仅留作下游形态参考 | — |
| **M6** 一致性保障与发布流程 ⚠️ | 附录 B.2 | **部分移出**：T6.1 闸门 / T6.2 golden + bad case / T6.4 阈值表**保留**；T6.3 影子环境 + 发布流程**移出** | M2 |
| ~~**M7**~~ 产品化 ⛔ | 附录 B.3 | **已移出本仓库**——上传/存储/导出 API/时间线 UI 属下游工程 | — |

横切（plan §11）：§11.3 的文档清单里，M3 的「proxy 生成规格」「源帧采样策略决策」与 M4 的「schema v1 + TS 类型」**尚未产出**；
**bad case 库当前为空**（这是结论，不是漏项——见 plan §5 结论）。

**范围结论**（2026-09-22）：本仓库交付「技术调研 + 底座」；M5/M7 整章移出，M6 只留闸门与资产部分。
因此 plan §1 总览里 M5/M6/M7 三行已就地标注，原章节保留为下游工程的形态参考。

---

### 挂起项（不在 M2 收尾路径上，但别忘）

1. ~~**T0.1 命名落地**~~ —— **已移出范围**（只做本地处理）：不发布 crates.io、不建远端仓库、不查域名。
   引擎名 `dhampir` 已定并落地；`[workspace.package] repository` 保持**不填**（不编造 URL）。
2. **M1 的 Linux 两条腿**（plan §4 M1「环境矩阵」与「退出标准」）：本机无 docker、WSL 无发行版；
   用户已于 2026-09-22 明确决定**延期**。条目本身没勾，属于已知缺口而非遗漏。
3. **plan §5 T2.6 的 ⏳ 是陈旧的（已核实）**：它说「留给 T2.6 两件事：跨厂商腿 `[scenario.*]` 定档 + 归因」，
   两件**都已兑现**——定档见 T2.4 与 T2.6 的回填（`0.9995` / `max_abs_diff_max = 1` / `checker` 与
   `srgb_linear` 各写 0），归因见 T2.5 + [`wgsl-portable-subset.md`](./wgsl-portable-subset.md) §6。
   **只差一行编辑**：把那行 ⏳ 翻成 ✅。plan 已被本轮重整编辑过，该行仍在 §5 正文内，可一并处理。
4. **plan 引用的四组负对照探针都活在 gitignore 的 `target/` 下**（本轮实核：文件都在）：
   `target/m2-framediff-rev.mjs`（+`rev-0..7`）、`target/m2-framediff-negative.mjs`、
   `target/wgsl-doc-rev.mjs`、`target/m2-shape-rev.mjs`（+`rev-0..10`），另有 `target/m2-backup/` 里的两条腿备份。
   plan §5（M2）多处把这几个探针当作「反向验证 / 负对照」的**出处**引用；
   `target/` 一旦被清理（清缓存、换机、重开工作区），这些引用就悬空、结论只剩口述。
   要长期留证：**清理 `target/` 之前**先把它们迁进仓库——注意**别放 `records/m2/` 根**（`root-listing` 会红）。

---

## 3. 交接给下一会话的硬约束与坑

### 3.1 架构（动了就动摇验收）

- **命名**：引擎命名空间只有 `dhampir` 一个前缀（crate 名、包名、CLI、文档）；不引入第二套命名。
- **依赖方向单向无环**：`dhampir-core` 与 `dhampir-media` **都只依赖 `dhampir-timeline`**（两者是兄弟，
  **core 不依赖 media**——渲染图不该知道 MP4 长什么样）；两个宿主依赖 `core + media`，互不依赖；
  **core 零 `#[cfg]`**（唯一例外：独占一行的 `#[cfg(test)]`）；
  `dhampir-wasm`（仅 wasm32）与 `dhampir-worker`（仅 native）**互不依赖**；core 只用 `std` + `wgsl`。
- **铁律**：整数帧号（不用浮点秒）；有理数帧率；声明式特效（不做可上传 shader）；
  按 WebGPU **能力下限**写（无导数、无循环、无隐式 LOD、只用 f32）；`Instance` 创建是唯一允许的分叉处。

### 3.2 执行纪律

- 上一个里程碑退出标准**全绿才进下一个**；**只勾真做完的**。
- 记录必须是**可复核产物（文件，不是口述）**。改决策 → 改 `plan/` 真相文档，别只在代码里改。
- **不引第三方库**；**解码器必须独立于生成器**（守卫各自重写 FNV-1a、PNG 解码、SHA-256、指标算法，不共享实现）。
- **文档与守卫不一致时补守卫，不改软文档**；工具**只出事实、不出结论**。
- **档位文件改一个字节，摘要就变** → 对应记录**必须重跑**（摘要进 `verdict.json`）。
- **唯一不许的做法**：为让某次记录变绿，去删掉或放宽 `[scenario.checker]` / `[scenario.srgb_linear]` 的
  `max_abs_diff_max = 0`。

### 3.3 守卫纪律（写 P0-1 时逐条照做）

- 必须带 `--self-test`；必须做**反向验证**；**拒绝空文件集通过**；**不得误报**。
- 只设 `process.exitCode`，**不调 `process.exit()`**。
- 判定逻辑写成**纯函数**（自检与真跑走同一段代码）。
- **每个检查项都要有结论**；失败行必须**点名理由**；detail 最多 6 条 + `…另有 N 处`。
- 自检的 `expect(name, condition, detail)` 要**真数断言条数**——守卫报的每个数都是结论的一部分。
- 反向用例必须断言「**因为那一条红**」，且必须要求**自检跑完**；不放过恒真检查。

### 3.4 环境与工具坑（本机 win32 + PowerShell）

- 逻辑一律用 **Node**，不用 PowerShell 做判定；判据看 `$LASTEXITCODE`；跑工具时 `$ErrorActionPreference='Continue'`。
- **不跑 `cargo fmt` / `clippy --fix`** 之类批量改写命令（会和在建改动互相踩）；清理用 `rm`。
- `git status --porcelain` / `record-acceptance` 的 `dirty` 都**剔除 `records/`**。
- 读 `records/` 里的 `report.txt` / `verdict.json` 一律用文件读工具（CLI 管道会踩编码）；
  **`Set-Content` 会加 BOM —— 不要用它往 `records/` 落证据**；看中文先设
  `[Console]::OutputEncoding=[System.Text.Encoding]::UTF8`；本机无 `grep`/`head`；`&&` 不是 PowerShell 分隔符；here-string 会吃引号。
- **`Get-Content` 的行数与 `split('\n')` 不一致**（本次实测差 232 行）——**报行数一律用 Node 数**，别信 PowerShell 的 `.Count`。
- 一次性探查脚本放已忽略的 `target/` 下（现有探针：`m2-*.mjs`、`m2-backup/`）；`records/` 在 text-hygiene 的 `SKIP_DIRS` 里，
  **记录里的证据文件自己管编码**（BOM/CR 由守卫的 `recordByteHygiene` 查）。

### 3.5 别重走的两条弯路

- **不要现在跑验收**：守卫没有 `main()`，跑出来的 `acceptance.json` 一定假绿（见 §0）。
- **不要往 `records/m2/` 根目录或新增子目录放文件**：`root-listing` 把根目录钉死成
  「4 个子目录 + `README.md` + `wasm-tests.json` (+ 允许 `acceptance.json`、`review-independent.md`、13 份 `<id>.txt`)」，
  多一个没人认领的文件就红。**本文件因此放在 `plan/` 下**，不进记录目录。

---

## 4. 开工顺序（照着做即可）

```
[ ] 1. 读 plan §1（总览 + 当前进度 + 建议 R1–R7）+ §5（M2 退出标准）+ 本文件 §2、§3
[ ] 2. 实核起点：git status --porcelain == 28 项（含本文件）；records/m2 230 个文件
[x] 3. P0-1 补 check-m2-record.mjs 的 runSelfTest + 37 条反向用例 + main + process.exitCode —— ✅ **已完成**
       └ 自检 53 条断言、37/37 覆盖；--record records/m2 → EXIT=0；空文件集 EXIT=2；改坏副本 EXIT=1
[ ] 4. 复跑 §2 里「尚未覆盖的分支」清单，确认每条都有用例
[ ] 5. P0-3 提交（拆主题）；提交前 node scripts/check-text-hygiene.mjs
       └ **必须在验收之前**：acceptance.json 的 dirty 剔除 records/，树脏就会写出 dirty:true → 守卫判红
[ ] 6. P0-2 node scripts/record-acceptance.mjs --milestone m2 → acceptance.json + 13 份 txt，全绿
[ ] 7. node scripts/check-m2-record.mjs --record records/m2 → EXIT=0（此时记录才自证）
[ ] 8. P1 发起 verifier 独立复核 → records/m2/review-independent.md
[ ] 9. 复核通过 → 回填 plan + 再提交 → **冻结 M2 记录**
[ ] 10. 冻结后才评估「拆取证脚手架」（R2）；M3 开工前给 scene.rs 的渲染器/场景规格加注释分界（R6）
[ ] 11. M3 第一件事：S3.1 决策实测并落文件（R1）——不定它不写 M3 代码
[ ] 12. M4 T4.1 定契约时钉死：跨进程只传 timeline JSON（R3）
```

---

## 5. 怎么自己复核这份记录

```powershell
cd F:/para/Code/Dhampir
git rev-parse HEAD                      # 63782435ffa2e0c2bf923f503919b60da45a7f5a
git status --porcelain                  # 28 项（含本文件；文档重整后）
node scripts/check-m2-record.mjs        # 当前：零输出 EXIT=0  ← 这就是"还没有 main()"的证据
node scripts/check-m1-record.mjs --record records/m1   # M1 归档仍应全绿
node scripts/record-acceptance.mjs --list --milestone m2   # 13 条判据 id
node scripts/check-text-hygiene.mjs     # 全仓编码/换行
```

守卫里钉住的**关键锚**（改记录前先确认这些数字还对得上）：

| 项 | 值 |
|---|---|
| 帧集摘要 `setDigest` | `browser` / `native`=`4bc004b502a1301a`；`browser-amd`=`bc09803eb44beac8` |
| 像素摘要 `framesDigest` | `browser`=`71ecc80cade3d73d`；`browser-amd`=`a37b0ab5140b18e6` |
| `pngBytes` | 129619 / 129765 |
| `readings` | 两条腿同为 112215 字节 / 532 行 |
| corpus | 5 场景 × 16 帧 = 80 帧、368 采样点、256×256、`Rgba8UnormSrgb` |
| 档位摘要 | `framediff`=`fce01cde0b735693`（exact）；`framediff-crossvendor`=`3f9560d1e3d4fb3f` |
| 跨厂商差异 | 48 张差异图 / 1105968 差异像素；differing=α_stack,blur,gradient；identical=checker,srgb_linear |
| `readings.txt` 跨腿差异形状 | 38 行不同 / 首个第 22 行（0 基） |
| wasm 测试 | 10/10（`records/m2/wasm-tests.json`） |
| `probeDigest` / crate | `c3f0da6b37577e55` / `dhampir` `0.0.1` |
