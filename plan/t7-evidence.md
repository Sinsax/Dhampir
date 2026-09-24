# T7 证据：交付面收口（具名子命令 / 陈旧 pkg 自动重建 / 续渲 / Linux 守卫）

> 这一段照 `plan/roadmap.md` T7 段展开：T7.1 具名子命令、T7.2（D12）陈旧 wasm pkg、
> T7.3（A9）渲染任务续渲、T7.4（D16）Linux 可移植性守卫、T7.5 派生面同步。
> 判据按**退出码**与**逐字节/逐项相等**算，不按日志里有没有"ok"字样。
>
> 数字本体在 `plan/measurements.md`；口径本体在 `plan/consistency-criteria.md`；
> 这份文件是**怎么验的**与**边界在哪**。

## 一句话版

| 步 | 落点 | 它的一把尺子 | 结论 |
|---|---|---|---|
| T7.1 | `crates/dhampir-worker/src/bin/dhampir.rs` | 具名写法 == 等价的 `edit --op` | **产物字节 + stdout 都逐字符相同** |
| T7.1 | `scripts/check-cli.mjs` | 子命令名单与契约 | **27 / 30**（红的 3 条全是坑 19 的编码器） |
| T7.2 | （待补） | 陈旧 pkg 自动重建一次，真坏仍红 | |
| T7.3 | （待补） | 可恢复的中间态，或明确不做 | |
| T7.4 | （待补） | 写死 Windows 语义要变红 | |

## T7.1 具名子命令：`undo` / `redo` / `clip` / `sequence` / `batch`

### 「同一实现的糖」到底被什么钉住

这一句验收要求最容易被糊弄过去 —— 只要新写一份"差不多"的实现，也能让所有例子跑通。
所以它被拆成两把**可执行**的尺子：

1. **纯函数那一层**（`cargo test -p dhampir-worker --bin dhampir`）：
   `build_named_op` 拼出来的 `EditOp` 与手写 JSON 解析出来的 `EditOp`
   **逐字段相等**（`assert_eq!` 比的是整个枚举值，不是几个可见字段）。
   同一条路还比了 `undo` / `redo`：`history_alias` 折出来的 `Args` 与
   `edit --undo` / `edit --redo` 解析出来的 `Args` **逐字段相等**。
2. **真跑那一层**（`scripts/check-cli.mjs`）：
   具名写法与等价的 `edit --op` 在**同一个夹具的两份拷贝**上各做一次，
   比**产物字节**与 **stdout 全文**。比"summary 文案对得上"会放过
   缩进、`issues` 顺序、字段缺省值这三类分叉。

两把尺子都在，是因为它们挡的不是同一件事：第 1 把挡"枚举拼错了"，
第 2 把挡"走了一条不同的落盘路径"（比如漏了历史、或写盘顺序反了）。

### 新增的八条契约判据

`scripts/check-cli.mjs` 的 `EXPECTED` 从 22 条加到 30 条：

| 判据 | 问的是什么 |
|---|---|
| `clip-equivalence` | `clip split` 与 `edit --op` 的**产物字节 + stdout** 相同 |
| `sequence-equivalence` | `sequence set` 与 `edit --op` 同上（含 `--timebase` 的有理数换算） |
| `history-alias` | `undo` 与 `edit --undo` 同上 |
| `clip-dry-run` | 不给 `--write` 时 `written=false` 且**文件一个字节没动** |
| `clip-rejects-bad-usage` | 不认识的动作名 / **多给一个开关**都退 2 |
| `batch-matches-edits` | 批处理的结果与**逐条 `edit`** 逐字节相同 |
| `batch-atomic` | 中途一步不成立 → 退 2、**整份不落盘**、summary 里明说**第几行** |
| `batch-empty` | 空脚本（只有注释）→ 退 2 且不动文件（**不是成功**） |

`SUBCOMMANDS` 同步加到 14 个，与 `COMMANDS` 表一致 —— 那条判据本来就是
「加了命令但没登记」与「登记了但 `--help` 没列出来」都要红。

### 三处设计选择，与理由是量出来的

1. **动作名做成第二个位置参数**（`clip split --layer c --at 75`）。
   备选是再来一层 `--op-name split`。选位置参数是因为它更接近 `git`/`ffmpeg` 的习惯，
   而且解析器只需要放宽一处（原先只收一个位置参数）。代价是多了一个 `Args` 字段
   与一段"只有 clip / sequence 收第二个位置参数"的规矩 —— 那条规矩本身有单测。
2. **多给一个开关也报错**，不只是少给。
   少给会被当成"忘了"，多给会被当成"反正不用它" —— 而两者的表现**完全一样**：
   那个值被丢掉，用户以为它生效了。所以每个动作有一张"总共认哪些"的表
   （`op_shape`），多给与少给都退 2，并把人话（`它认的是 …`）打出来。
3. **`batch` 不是"循环调 edit"**。
   若做成宏，中途失败会留下"做了一半"的工程 —— 那既不是改前也不是改后，
   而 `undo` 只能整步退。所以做成**一次写、一条历史**：全成则一次落盘 + 一条历史，
   有一步不成立则**整份不落盘**，并在 summary 与 issues 里带上**行号**
   （"批处理在第 2 行停下"），否则用户只能靠 diff 猜是哪一步。

### 一处自我引入又自我抓到的回归（记下来，因为它是这类改动的典型形状）

把 `--write` 收紧成"只有编辑那一组认"时，漏了 `import` —— 它也要落盘。
表现是 `scripts/check-cli.mjs` 的 `import-write` 当场变红，然后
`library` 与 `import-duplicate` 跟着红（它们依赖上一步真的写进去了）。

修法不是放宽 `--write`，而是**把两张表分开**：

* `EDIT_FAMILY`（6 个）—— 认 `--history` 的；
* `WRITE_FAMILY`（7 个）—— 认 `--write` 的，多一个 `import`。

并把这条钉进单测：`import --write` 必须合法，`import --history` 必须报错。
**合成一张表的代价**正是"顺手把「import 认不认 --history」答成了认"—— 那是错的。
