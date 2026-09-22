# M2 记录归档 · 独立复核报告

复核者：dhampir verifier（独立于记录作者）
复核对象：`records/m2/`（两条浏览器腿 + 两份 framediff 比对 + 诚实性产物），native 锚取自 `records/m1/dx12/`
复核方式：**不引用记录里的任何结论**，逐项重算；守卫自检与真跑各跑一遍，并**主动破坏守卫**验证它能不能被证伪。

---

## 0. 总判定

**通过。**

- 记录里被钉住的每一个数，我都用自己写的代码（自己的 PNG 解码器、自己的 FNV-1a 64、从 `crates/dhampir-core/src/render/corpus.rs` 的 `table_digest` 契约反推的整表摘要口径）重算了一遍，**全部对上**：`71ecc80cade3d73d`、`a37b0ab5140b18e6`、`4bc004b502a1301a`、`bc09803eb44beac8`、`129619`、`129765`、`1105968`、`fce01cde0b735693`、`3f9560d1e3d4fb3f`、`496a5e3ef2e8e2ba`、`38 行`、`第 22 行`、`273 条`、`10/10`。
- 守卫 `node scripts/check-m2-record.mjs --record records/m2` → **EXIT=0（合计 60/60 项绿）**；`--self-test` → **EXIT=0（53 条断言；37/37 个检查项各一条反向用例）**。两条我都实跑复现。
- 守卫**可以被证伪**（见 §3）：把任一检查项改成恒真、或删掉一条反向用例，自检立刻变红退 2；空帧集退 2；缺一张 PNG 退 1。
- 验收快照 `acceptance.json` 的 13 条判据，我逐条读了 `.txt` 原文，并重跑了其中 7 条 —— 与快照记的 `exit_code: 0` 一致（见 §4）。

**但这个"通过"要带两个限定**，它们是我这次复核最重要的产出（都是**守卫**的缺陷，不是记录的缺陷）：

1. `acceptance` 这一项**证明不了快照诚实**：它只检查"快照有没有承认失败"，从不拿 `exit_code` / `ok` / `commit` 去对 `.txt` 原文或 git。我造了一份"JSON 说全绿、`.txt` 实际是红的"的副本，守卫照样报 **60/60 绿、退 0**（§5.2 · P1，有实证）。
2. 自检的覆盖断言是**自指**的（检查项表与反向用例表互相校验），所以"同时删掉一个检查项和它的反向用例"能骗过自检：副本跑出自检绿、真跑 `58/58` 绿，而屏幕上仍然印着"160 张 PNG 的像素摘要逐张重算"（§6 · P2，有实证）。

这两条不影响本记录结论的成立（记录本体我用第三方口径全部复算过），但它们意味着：**读这份记录的人不能只信"守卫绿了"，必须自己看 `acceptance.json` 旁边那 13 份 `.txt`**。我这样做了。

---

## 1. 我实际跑的命令与看到的退出码

| 命令 | 退出码 | 我看重的输出 |
|---|---|---|
| `node scripts/check-m2-record.mjs --record records/m2` | **0** | 腿 browser 15/15、browser-amd 15/15、native 3/3、跨腿 6/6、framediff 8/8、crossvendor 8/8、诚实性 5/5；合计 60/60 项绿 |
| `node scripts/check-m2-record.mjs --self-test` | **0** | 53 条断言；37/37 个检查项各一条反向用例 |
| `node scripts/check-m1-record.mjs --record records/m1` | **0** | 2 条腿 × 80 张 PNG；整表摘要 `71ecc80cade3d73d` 由重算复现；31 项全绿 |
| `node scripts/check-m1-record.mjs --self-test` | **0** | 47 条用例（14 个检查项各一条反向用例） |
| `node scripts/check-core-purity.mjs` | **0** | dhampir-core 10 个文件无 `#[cfg]` |
| `node scripts/check-dep-graph.mjs` | **0** | 5 个 crate 依赖单向无环 |
| `node scripts/check-text-hygiene.mjs` | **0** | 70 个文本文件全 LF、无 BOM |
| `node scripts/dhampir-framediff.mjs --self-test` | **0** | 自检通过（95 条用例） |
| `node scripts/dhampir-wgsl-census.mjs --declared plan/wgsl-portable-subset.md` | **0** | 申报 28 条；用了但没申报 0 条 |
| `node target/m2review/verify1.mjs`（我自己写的：独立 PNG 解码 + 摘体重算） | **0** | 160/160 张 PNG 的 pixel_digest / png_digest / png_bytes 全对 |
| `node target/m2review/verify2.mjs`（自己的帧集合摘要 + 逐场景差异） | **0** | 集合摘要三条腿对上；AMD↔native 差异逐场景复现 |
| `node target/m2review/verify3.mjs`（JSON 漂移路径计数、readings 行差） | **0** | 2 条 / 273 条 / 6 条 / 38 行 / 第 22 行 全部复现 |
| `node target/m2review/verify4.mjs`（截图 sha256、rerun 复现、wasm 计数） | **0** | 截图 sha256 逐位对上；源码里 6+4=10 条 `#[wasm_bindgen_test]` |
| 守卫 CLI：`--bogus` / `--record` 缺值 / 目录不存在 | **2 / 2 / 2** | 三条都退 2，不乱跑 |
| `--help` | **0** | 只打用法 |

一次性脚本都放在 `target/m2sab/`、`target/m2review/`、`target/m2empty/`、`target/m2noframes/`、`target/m2partial/`、`target/m2accept-*/`（已在 gitignore 内）。**仓库里的 `scripts/` 与 `records/` 我一个字节都没改**（本次只新建 `records/m2/review-independent.md`）。

---

## 2. 记录里的数被第二个人算出来过吗 —— 自己重算的结果

### 2.1 像素摘要（全 160 张，不抽样）

我**自己写了一个 PNG 解码器**（IHDR/IDAT、zlib inflate、五种滤波器反解、8 位 RGBA、非隔行）和一份 FNV-1a 64，**没有调用守卫的任何函数**。对两条浏览器腿的 80+80 张 PNG 逐张：

- 重算 `pixel_digest`（解码后 RGBA 字节过 FNV-1a 64）不一致：**0 张**；
- 重算 `png_digest`（文件字节过 FNV-1a 64）不一致：**0 张**；
- `png_bytes` 与盘上文件长度不一致：**0 张**；
- PNG 总字节：browser `129619`、browser-amd `129765`，与 README 逐字相符。

随后按 `crates/dhampir-core/src/render/corpus.rs` 的 `table_digest` 契约（**场景名 + 0x00 + 帧号 LE u32 + 像素摘要 LE u64，整体 FNV-1a 64**）自己拼字节重算整表摘要：

- browser → `71ecc80cade3d73d`（与记录声明值相同）
- browser-amd → `a37b0ab5140b18e6`（与记录声明值相同）

M1 锚也一样：我自己解码 `records/m1/dx12/frames` 的 80 张 PNG，`pixel_digest` 不一致 **0 张**，整表摘要 `71ecc80cade3d73d`。

### 2.2 两条腿真的共用同一批字节吗

- browser 的 80 个 PNG 与 `records/m1/dx12` 的同名文件**逐个字节相等（80/80）**，文件名集合完全相同，逐字段差异路径恰好 **2 条**（`backends[0].adapter_name`、`backends[0].requested`）——差的是"这是谁跑的"，不是"画了什么"。
- 帧**集合**摘要（按场景码元序 + 帧号数值序拼 `文件名 + 0x00 + 文件字节`）我自己算：browser = `4bc004b502a1301a`，native = `4bc004b502a1301a`（相等），browser-amd = `bc09803eb44beac8`（不同，且这正确）。这个口径不是我发明来凑的：`framediff/verdict.json` 与 `framediff-crossvendor/shape.json` 里独立落着同样的 `set_digest`。

### 2.3 跨厂商差异的形状

我用自己的解码器逐场景逐帧算"任一 RGBA 通道不同即算差异像素"与 max|Δ|：

| 场景 | 差异像素（我算） | max\|Δ\|（我算） | README 写 |
|---|---|---|---|
| gradient | 57344 | 1 | 57344 / 1 |
| alpha_stack | 1048576 | 1 | 1048576 / 1 |
| blur | 48 | 1 | 48 / 1 |
| checker | 0 | 0 | 0 / 0 |
| srgb_linear | 0 | 0 | 0 / 0 |
| 合计 | **1105968** | 1 | 1105968 |

逐字节相同的正好是 32 张（checker 16 + srgb_linear 16），不同的正好 48 张，落在 `alpha_stack` / `blur` / `gradient` 三个场景 —— 与"差异集中在经过 sRGB 编解码的通道、且只有 1 个字节"的说法一致。`alpha_stack` 的 1048576 = 256×256×16，即整张全动，SSIM 仍接近 1 是合理的。`framediff/` 下确实**没有 `diff/` 目录**（0 差异不写黑图），`framediff-crossvendor/diff/` 确实是 **48 张**。

### 2.4 文本与漂移

- `readings.txt`：三条腿各 **112215 字节 / 532 行**；browser 与 native **逐字节相同**（两者 sha256 前 16 位同为 `58c75975032bb780`，差异 **0 行**）；browser-amd 与它们差 **38 行**，**第一个不同的行是第 22 行（0 基）**，那一行是 `gradient f2 (200,128)`：browser 实测 244、AMD 实测 245，模型值都是 244 —— 正是舍入边界另一侧的 1 LSB。
- `run.json`：browser vs native **2 条**路径；browser vs browser-amd **273 条**，我按组拆开正好是 48+48+48（pixel/png/repeat 三种摘要）+ 1（`frames_digest`）+ 31（`png_bytes`）+ 97（`measured` / `distance` / `detail`）= **273**；browser-amd vs native = 275 = 273+2。README 的"31 不是 48，长度撞上了按实测钉"这一点也站得住。
- `adapter.json`：两条腿逐字段比下来**恰好 6 条**路径不同，就是 README 列的那六条；字节数 2398 / 2397，所以"只有 6 条路径不同"与"文件确实不同"同时成立。
- 截图：我用 `node:crypto` 重算两张 `screenshot-browser-corpus.png` 的 sha256，与 json 里 `screenshot.sha256` **逐位相同**（`32abaf72f28809eb…` / `0fb072c683cde5a8…`），字节数 173297 / 176458 也对上。
- `rerun-repro.json`：盘上 `browser-amd/readings.txt` 的 sha256 前 16 位实测就是 `496a5e3ef2e8e2ba`，与复跑佐证声明的一致；80/80 帧逐字节相同、`mismatches` 为空。
- `wasm-tests.json`：`passed 10 / failed 0 / listed_total 10`，两个 target 的 `listed_tests` 我都拿去与源码对：`crates/dhampir-wasm/src/corpus.rs` 的属性行是 **6** 条、`crates/dhampir-wasm/tests/cross_runtime.rs` 是 **4** 条，合计 **10/10**。（我自己第一遍用裸字符串数出 `corpus.rs` 是 8 —— 多出的 2 条是注释里逐字提到这个属性的那两行；守卫源码里正好把这个坑写下来了，它对。）

---

## 3. 守卫的自检是不是真能证伪

### 3.1 先自己数（不信它自己报的数）

从源码独立解析：

- `LEG_CHECKS` 15 + `NATIVE_CHECKS` 3 + `FRAMEDIFF_CHECKS` 8 + `CROSS_CHECKS` 6 + `HONESTY_CHECKS` 5 = **37 个检查项**；
- `MUTATIONS` **37 条**；`expect` 值**无重复**、**无遗漏**（37 个检查项都有且仅有一条）、**无指向不存在项的多余项**；分组计数 15/3/6/8/5 与检查项表完全同构。
- 自检断言条数也能对上账：7 组基线 × 2 条 + 1 条（漏传 spec）+ 37 条反向用例 + 1 条覆盖 = **53**，正是它报的 53。

结论：**37 项 ↔ 37 条，一一对应，没有"恒真检查"能在自检里蒙混过关**（每一条都有自己的定向破坏）。

### 3.2 真的破坏一次（只改 `target/` 下的副本，没碰仓库）

| 副本 | 改法 | 自检 | 真跑 |
|---|---|---|---|
| `target/m2sab/s1-tautology.mjs` | 让 `pixels` 检查**恒真**（`return []`） | **EXIT=2**，点名"改动「改一帧的 pixel_digest（文件没动）」必须被 pixels 抓到：红的是 repeat、frames-digest" | **EXIT=2**（自检先挂，守卫拒绝用它自己的结论去核记录） |
| `target/m2sab/s2-dropmutation.mjs` | 删掉 `pixels` 那条反向用例 | **EXIT=2**："每个检查项都有反向用例：没有反向用例的是 pixels" | — |
| `target/m2sab/s3-scopeshrink.mjs` | **同时**删掉 `pixels` 检查项与它的反向用例 | ✗ **EXIT=0**（52 条断言；36/36） | ✗ **EXIT=0**，报"合计 58/58 项绿"，并继续印"160 张 PNG 的像素摘要逐张重算" |

前两条说明：自检**不是**摆设，恒真检查与覆盖缺口都能被它抓住，而且真跑会把自检放在最前面（守卫坏了就不给记录背书）。第三条是缺陷，见 P2。

---

## 4. 空帧集、半份记录、坏参数

| 造的记录 | 命令 | 退出码 | 输出 |
|---|---|---|---|
| 只有腿目录、`frames/` 空（0 张 PNG） | `--record target/m2empty` | **2** | ✗ frames/ 里一张 PNG 都没有——空文件集绝不允许通过 |
| 腿目录在、`frames/` 根本不存在 | `--record target/m2noframes` | **2** | 同上（不是"抛异常崩掉"，是明确判死） |
| 整份拷贝、只删掉 `browser/frames/checker-f007.png` | `--record target/m2partial` | **1** | 合计 48/60，12 项红，并**点名**"native 有而 browser 没有：checker-f007.png" |

**空帧集被拒（2）、半份记录被拒（1）、好记录才给 0** —— 三档分得开。

---

## 5. 验收快照诚实吗

### 5.1 对得上

- `commit: a35f535f0906`，`dirty: false`。实查：`a35f535f0906` 是 HEAD `d23b8664b3c7` 的**父提交**，而 `git diff --stat a35f535f0906 HEAD` 只多出 `records/m2/acceptance.json` + 13 份 `.txt`（14 files, 607 insertions），**`scripts/` 与 `crates/` 一个字节没动**。所以快照跑的那棵树 = 我现在复算的这棵树；被核的守卫版本也正是这一版。`dirty` 的口径（剔除 `records/`，因为工具自己就往里写）在 `record-acceptance.mjs` 里写明了，合理。
- 13 条判据的 `exit_code` 全是 0、`ok` 全是 true。我逐条读了 `.txt` 的原始输出，**没有一条与"成功"矛盾**：`native-tests` 里每个二进制都是 `test result: ok`，我数了一遍 passed = 70+3+21+5+5+0+33+11 = **148**，与 `tests.passed: 148` 一致。我另外**重跑了其中 7 条**（两条 M2 守卫、两条 M1 守卫、core-purity / dep-graph / text-hygiene / framediff-self-test / wgsl-census），全部 EXIT=0，输出与 `.txt` 记的一致（连"10 个文件""70 个文本文件""95 条用例""申报 28 条"这些数都一样）。

### 5.2 但守卫在这件事上帮不了你（P1，实证）

`acceptance` 这一项**只单向检查**：要求 `green === true`、`exit_code === 0`、每条判据 `ok === true` + `exit_code === 0`，再对 `.txt` 做 BOM/CR 卫生检查。它**从不**把 JSON 里的 `exit_code` / `ok` / `tests` / `commit` 拿去和 `.txt` 原文或 git 对比。

实验：把记录拷一份，`acceptance.json` 原样不动（全部 `exit_code: 0`、`ok: true`），只把

- `guard-m2-record.txt` 正文改成 `✗ 腿 browser：3/15` / `✗ 合计 12/60 项绿，48 项红`，
- `native-tests.txt` 改成 `test result: FAILED. 1 passed; 147 failed`，
- `commit` 改成 `000000000000`，

结果：`node scripts/check-m2-record.mjs --record target/m2accept-lie` → **合计 60/60 项绿，EXIT=0**。
对照实验：把 JSON **老实**写成 `exit_code: 1` / `ok: false` / `green: false` → 守卫立刻红（`✗ acceptance`，59/60，EXIT=1）。

也就是说：**守卫能发现"快照承认自己失败"，但发现不了"快照谎称成功"**；而且 `.txt` 里根本没有退出码这一行（`record-acceptance.mjs` 只落命令 + stdout + stderr），所以 JSON 的 `exit_code` 在记录内**不可复算**。

对本记录的实际影响：**没有**。本记录的 `.txt` 我逐条看过、关键几条重跑过，都是真绿。但这条判据的证明力只能到"快照声称成功"，剩下的必须由人读原文补上。

---

## 6. 我发现的全部问题

**P1（守卫 · 实证）** `acceptance` 检查单向：只验"有没有承认失败"，不验 `exit_code` / `ok` / `tests` / `commit` 与 `.txt` / git 是否相符。伪造的成功声明 + 红掉的原文 + 假 commit 能拿 60/60 EXIT=0（§5.2）。建议：把退出码写进 `.txt`（例如一行 `exit: N`），守卫再逐条比对；`commit` 至少校验它是不是 HEAD 的祖先。

**P2（守卫 · 实证）** 自检的覆盖断言**自指**：`ALL_CHECK_IDS` 与 `MUTATIONS` 互相校验，没有对着一个硬编码的总数（37 / 60）锚一下。所以"协同缩表"（删检查项 + 删它的反向用例）能骗过自检：副本自检绿、真跑 58/58 绿，**屏幕上仍然印着"160 张 PNG 的像素摘要逐张重算"**（§3.2 第三行）。残余杀伤有限（`png_digest` 与整表摘要锚仍钉着像素，且那条 mutation 还会被 `repeat` / `frames-digest` 抓到），但"守卫印的这句承诺可以变成假的而它照样绿"是真实的可信度漏洞。建议：自检里把期望项数与期望断言数**写死**（例如断言 `ALL_CHECK_IDS.length === 37`）。

**P3（守卫 · 口径）** `readme-claims` 是**字面量存在性检查**（15 个锚点必须出现、3 句禁令不许出现），它判断不了 README 里的数**是不是真的**。同理 `review-independent` 只是地板（≥1024 字节 + 提到一个锚 + 出现结论词），一份灌水到 1KB 的"假复核"能过。这两条都得靠人——我这次做的就是这件事。

**P4（记录 · 观察）** `acceptance.json.source` 指向 `plan/video-editor-plan.md` §5，而该文件当前在工作树里**未提交地被改过**（+24/−12）。快照当时的 `dirty=false` 是诚实的（`records/` 被剔除；那时那份 plan 还干净），但**今天原地重跑会得到 `dirty=true`**，快照不再可逐字节再生。这不是记录造假，是"快照的出处文档后来又动了"。

**P5（我自己的边界）** `verdict.json` / `report.txt` 里的 **SSIM / PSNR 数**我只做了独立侧证，没有独立重算：我复算的差异像素与 max|Δ| 与 SSIM 结论方向一致（全等场景 SSIM 必为 1，我这边就是 0 差异；有差异的三个场景 |Δ| 全为 1），但 SSIM 的**实现口径**（窗口、常数）我没重写第二份。守卫那份与工具那份是各写一遍、逐字节对账的——但同一个作者，共同的理解偏差两边都抓不到。这条结论我标注为"由守卫背书，本次未独立复现"。

**P6（记录 · 细节）** `wasm-tests.json` 里嵌着机器本地绝对路径（`runner.path` 在 C 盘用户目录下、两个 `.wasm` 在 `target/` 下）。作为"当时用了什么跑出来的"证据没问题，但它泄露了用户名，且别人无法照它复现；只适合当出处、不适合当指令。

**P7（无害观察）** 两份 `run.json` / `adapter.json` 的 `milestone` 写 `M1`、`wasm-tests.json` 写 `M0`、截图 json 写 `M2` —— README 第 1 条说这是**记录契约版本**而不是跑的时刻。我认可：`run.json` 的键集与 M1 完全同源，把它改成 `M2` 反而会让 M1 守卫与记录对不上。`browser-amd` 里那几处 `false`（`server_findings` 的第 2、3 条、`audit.findings` 的第 4、5 条）而 `audit.ok === true`，我实查确认：那是"这一腿与 native 在某些维度上确实不同"的如实回答，不是失败。

---

## 7. 一句话结论

M2 归档里**所有被钉住的数都经得起第二个人用另一套代码重算**，空帧集 / 半份记录被正确拒绝，守卫自检可以被破坏也被证伪 —— 结论 **通过**；但守卫的 `acceptance` 与覆盖断言各有一次**可实证的可信度漏洞**（P1 / P2），因此"守卫绿"不能替代"人读 13 份判据原文"，本报告已经把这一步做完了。
