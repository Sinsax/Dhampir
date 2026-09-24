# T4.0 设计：编辑模型补完（历史层 / 带关键帧的剃刀 / 预览拖拽）

> 本段有一处**架构决定必须先落纸**：`split` 要给切点两侧各插「当时的值」，
> 而那条求值曲线**只有 core 有**，timeline 又不能依赖 core。
> 先决定曲线归谁、再动手；不然写出来的会「看起来对、切开以后曲线悄悄变了」。
>
> 对应 `plan/roadmap.md` 第 173 行起的 T4 段；三个缺陷是台账里的 **A2 / D6**、**D2**、
> 以及 T4.3 这条（它没有台账条目，是新增面）。
>
> **状态：已实现并收口（T4 收口）** —— 实现与验收的原始输出在
> [t4-evidence.md](./t4-evidence.md)；与本文档 4.2 的一处偏离（中间落点不发 Rust，
> 改成只在 `pointerup` 提一次）也记在那里。

## 一、这一段要对上三件事

| 条 | 台账 | 现在的样子（行号都是核对过的） |
|---|---|---|
| **T4.1** | A2、D6 | **没有任何历史层**。`crates/dhampir-timeline/src/edit.rs:577` 的 `pub fn apply` 已经是**纯函数**（进来一份 `doc`，出去一份 `EditOutcome`），但它没有安放「前后两份 doc」的地方 —— 所以 CLI 与预览都不可撤销 |
| **T4.2** | D2 | `edit.rs:283` 的 `split` 在 `edit.rs:307-319` **直接拒绝**带关键帧的元素，报 `split_across_keyframes`（注释写着「宁可明说做不到，也不悄悄切歪」）。拒绝本身是对的，缺的是「切得不歪」的做法 |
| **T4.3** | — | `web/app.js:459` 的 `renderTimeline` 里，每一层只有 `click` 选中，**没有 pointerdown / drag**。预览能改属性、能按按钮，就是不能拖 |

**验收**（roadmap 原话）：CLI 有 `--undo/--redo`；op 之后再 undo 得到逐字节相同的 doc。

---

## 二、架构决定：曲线求值归 **timeline**（T4.2 的前置）

### 2.1 现状（都带行号，别再翻一遍）

* 求值住在 core：`crates/dhampir-core/src/compose.rs:88`
  `pub fn opacity_from(opacity: f32, keyframes: &[Keyframe], local_frame: Frame) -> f32`。
  语义（**搬迁时要逐条照抄**）：
  1. 先把键按 `frame` 排序 —— 契约不要求有序，注释明说「依赖用户会按顺序写」是会在别人手写的工程上炸的假设；
  2. `local <= first.frame` → 取 `first.value`；
  3. `local >= last.frame` → 取 `last.value`；
  4. 否则在**相邻对** `(a, b)` 里插值，用的是 **`b.easing`**（`compose.rs:109` 的 `b.easing.apply(t)`），
     `span <= 0` 时取 `t = 1.0` 防除零。
* `compose.rs:117` 的 `opacity_at(clip, local_frame)` 只是 v1 的薄包装（注释：「**逻辑只有一份**，在 `opacity_from` 里」）；四个调用点在 `163 / 178 / 268 / 275`。
* 缓动公式**已经在 timeline**：`crates/dhampir-timeline/src/schema.rs:188` 的 `Easing`、`:198` 的 `apply`（Linear / EaseIn / EaseOut / EaseInOut）。所以 timeline 自己求值缺的只是那二十来行，**不是数学**。
* 依赖方向：core → timeline（`crates/dhampir-core/src/lib.rs:48` `pub use dhampir_timeline as timeline;`），反向由 `scripts/check-dep-graph.mjs` 钉住。
* 键的合法范围由 `schema.rs:370-382` 校验：`0..=duration-1`（`duration = end - start`），越界报 `keyframe_out_of_clip`。

### 2.2 三个选项

| 选项 | 做法 | 代价 | 判 |
|---|---|---|---|
| **A** 求值留在 core，timeline 里再写一份 | 把那二十来行复制进 timeline | 「同一份逻辑两份实现」——`opacity_at` 的注释就是反对这件事的那句话。两份会漂，而**漂了没有任何东西会红** | ✗ |
| **B** 求值**下沉到 timeline**，core 转发 | timeline 新建 `curve.rs`；core 的 `compose.rs` 改成 `pub use` 转发 + 内部调用点改路径 | 纯搬迁：语义不变、**调用路径不变**、可逐字节验 | **✓ 选它** |
| **C** 让 core 反向暴露给 timeline | timeline 依赖 core | 依赖方向反了，`check-dep-graph` 会红 | ✗ |

### 2.3 决定 B 的落点（一行为单位说清）

1. 新文件 `crates/dhampir-timeline/src/curve.rs`：
   `pub fn opacity_from(opacity: f32, keyframes: &[Keyframe], local_frame: Frame) -> f32`，
   **函数体逐行照抄 `compose.rs:88` 的语义**（上面 2.1 那四条），注释一并搬（含「先排一次」的理由）。
2. `crates/dhampir-core/src/compose.rs`：删掉函数体，改成
   `pub use crate::timeline::curve::opacity_from;`（`crate::timeline` 就是 core 对 timeline 的重导出）。
   —— 保这条重导出的原因是**下游一行都不用改**：`dhampir_core::compose::opacity_from` 这个路径继续存在。
3. `opacity_at`（v1 的入口）**留在 core 不动** —— 它的签名带 `&Clip`，那是 core 的形状。
4. T4.2 的 `split` 直接调 `crate::curve::opacity_from`（timeline 内部）。

**搬迁的判据**：golden **逐字节**（`target/t2/byte-identical.cjs` 那条腿 + 里程碑字节数不变），
外加 core 自己那几条关键帧用例**一个字不改**地继续绿。

**为什么这一步敢做**：它不动语义、不动调用路径，唯一变的是「函数体住在哪个 crate」。
这类改动有一个干净的判据 —— **有像素的地方逐字节不变**。

---

## 三、T4.2 剃刀（`split` 带关键帧）的精确形状

### 3.1 算法（**就按这个写**）

设 `original` 是被切的元素，`local = at_frame - original.start`。现有代码已经保证
`at_frame > start`（→ `local >= 1`）且 `at_frame < end`（→ `local <= duration - 1`），所以插入点恒在区间内部。

* **左半段**（`start..at_frame`，`duration = local`）：
  保留 `frame < local` 的键；若**没有** `frame == local-1` 的键，就**插一个**：
  * `frame = local - 1`
  * `value = opacity_from(original.opacity, keys, local-1)`
  * `easing = 原来那一段右端键的缓动` = 排序后第一个 `frame > local-1` 的键的 `easing`；没有就取最后一个键的
* **右半段**（`at_frame..end`，`duration = duration - local`）：
  保留 `frame >= local` 的键并**整体左移 `local`**（这样它们相对新起点仍是原来的相对位置）；
  若**没有**落到 `frame == 0` 的键，就**插一个**：
  * `frame = 0`
  * `value = opacity_from(original.opacity, keys, local)`
  * `easing = 第一个 frame >= local 的键的 easing`；没有就取最后一个键的

其余（`source` 的连续推进、`transition_in = None`、`recorded.markers` 的重定位）**沿用现有实现，一个字不改**。

### 3.2 为什么插进去的键继承「那一段**右端**键」的缓动

因为本仓的求值语义是**用相邻对里后一个键的缓动**插值（`compose.rs:109`）。
插进去的键要顶替的是它原本所在那一段的**右端角色**，所以它继承的必须是**原来那一段的右端键**的缓动。
这一条不照抄的话，左边会漂 —— 3.3 表里「49 落键」那行就是它（左 0.000000 / 右 0.011622）。

### 3.3 允诺边界（**量出来的，不是想出来的**）

量法：照抄 `opacity_from` 的语义写成一个纯 JS 草稿，把整条曲线逐帧跑两遍（切之前 / 切之后），
取每一半的最大绝对偏差。草稿在 `target/t4/`（**gitignored，只用来定口径，不当证据**）：

* `target/t4/split-exactness.cjs` —— 键在 `0`（值 0.25）与 `100`（值 1.0），切点在 `50`，`duration = 101`，四种缓动各一遍；
* `target/t4/split-exactness-keyboundary.cjs` —— 键正好落在切点上 / 两侧都落键的四种配置。

**第一种：缓动逐个跑（切点两侧都不落键）**

| 缓动 | 左半段最大偏差 | 右半段最大偏差 | 切缝两帧（`local-1` 与 `local`） |
|---|---|---|---|
| `linear` | 0.000000 | 0.000000 | 完全相同 |
| `ease_in` | 0.000000 | 0.093750 | 完全相同 |
| `ease_out` | 0.093673 | 0.000000 | 完全相同 |
| `ease_in_out` | 0.120000 | 0.124950 | 完全相同 |

**第二种：键的位置（缓动取 `ease_in_out`）**

| 配置 | 左半段最大偏差 | 右半段最大偏差 |
|---|---|---|
| 键只在 `0 / 100`（两侧都不落键） | 0.160000 | 0.166600 |
| 外加 `49`（左半段末帧正好是键） | 0.000000 | 0.011622 |
| 外加 `50`（切点正好是键） | 0.009892 | 0.000000 |
| `49` 与 `50` 都落键 | 0.000000 | 0.000000 |

**结论（这就是本段的承诺，也是要写进代码注释的那句）**：

* **切缝的两帧恒精确**（`local-1` 在左半段、`local` 在右半段，取的都是原曲线在那两帧的值），**两端点恒精确**；四种缓动、四种键位都成立。
* `linear` 与 `ease_in` 这类**幂律**缓动，除了切缝之外也能逐帧精确（幂律对区间缩放不变）。
* `ease_out` / `ease_in_out` **不是**幂律，只有切缝与端点精确，半段内部有**有界**偏差（上表实测 ≤ 0.167）。
* 所以**不做**「逐帧精确」的承诺。承诺就写成 D2 台账的那句话：
  **切点两侧的端点值 == 原曲线在切点的值**。
* **不做**「多插几个键把缓动拟合回来」：要真的逐帧一样就得每帧一个键，否则引入的是另一种误差 —— 两头都比现在差。

> **口径落地**：上面这些数字来自 `target/` 里的草稿，**草稿不能当证据**。
> T4.2 必须把「切缝两帧精确」写成一条 **Rust 用例**（拿 `opacity_from` 跑原曲线与两半，逐帧比），
> 让这条承诺由仓库自己的测试钉住。

### 3.4 边界与退化

| 情形 | 会怎样 |
|---|---|
| 切点正好是某个键（`frame == local`） | 右半段左移后自然有 `frame == 0` 的键，**不插**；左半段照常插 |
| 切点两侧都落键（`local-1` 与 `local` 都有） | 两边都不插，**逐帧精确** |
| 只有一个键 | 左右两半各得一个常量键（值都等于它）——`opacity_from` 在两个方向上本来就返回它 |
| 元素没有关键帧 | 走**老路径**，行为逐字节不变（本段不碰） |
| 插入的值不是有限数 | 到不了这里：`schema.rs:384-390` 已经在校验阶段拒掉非有限的键值与不透明度 |
| 插进去的键越界 | 到不了这里：左半段最大是 `local-1 = duration-1`、右半段最大是 `duration-1-local`，都在 `0..=duration-1` 内 |

### 3.5 要改的既有测试、要删的码

* **改**：`edit.rs:722` 的 `剃刀不碰带关键帧的元素_并且明说为什么()` —— 它现在断言**拒绝**。
  改成断言：**成功** + 切缝两帧的值 == 原曲线在那两帧的值 + 两端点不变 + 两半都在合法范围内（不触发 `keyframe_out_of_clip`）。
  **改名**（原名说的是已经被推翻的结论）：`剃刀切带关键帧的元素_切缝两帧与端点精确()`。
* **删**：`split_across_keyframes` 这个 issue code（`edit.rs:313` 产生它、`edit.rs:731` 断言它）。
  已 grep 全仓：**没有守卫、没有文档**引用它，删掉不影响任何判据；「拒绝带关键帧的元素」这条路径随之消失。

---

## 四、T4.1 历史层（D6 / A2）

### 4.1 为什么存**整份快照**，而不是「反向 op」

`remove` / `split` / `trim` 都是**有损**的（被删掉的区间、切之前那份 keyframes 都回不来），
反向推不回来。而 `ProjectDoc` 是**可序列化的纯数据**，一份克隆换一个「一定能回到原样」——
这正是验收要的那句「**逐字节相同**」。

### 4.2 形状（`crates/dhampir-timeline/src/history.rs`，零新依赖）

```rust
pub struct Snapshot { pub label: String, pub doc: ProjectDoc }

pub struct History {
    past: Vec<Snapshot>,
    future: Vec<Snapshot>,
    cap: usize,
    last_key: Option<String>,
}
```

* `new(cap)` —— 上限按**条数**算。到顶丢**最旧**的一条：编辑不该因为「历史满了」而失败。
* `push(label, before_doc)` —— `before_doc` 是**改动前**的整份文档；顺带清空 `future`（新编辑让重做失效）。
* `push_coalescing(label, before_doc, key) -> bool` —— `key` 与上一次相同、且 `past` 非空时**不压新快照**（返回 `false`）。
  这一条不是锦上添花：T4.3 的一次拖拽会生成一长串 `move`，撤销应当回到**拖拽之前**，而不是往回挪一帧。
  其它任何 `push` 都把 `last_key` 清掉。
* `undo(current, current_label) -> Option<Snapshot>` / `redo(...)` —— 返回**要恢复的那份**，
  并把当前状态压到对侧栈（这样重做的标签是「当前这一步做过什么」，不是「将要做什么」）。
* `can_undo() / can_redo() / depth()`，以及 `serde` 序列化（CLI 要落盘）。

### 4.3 CLI 面（`crates/dhampir-worker/src/bin/dhampir.rs`）

* 新增两个**不带值**开关 `--undo` / `--redo` → 进 `KNOWN_FLAGS`（5 → 7 条），`USAGE` 的 `edit` 段补两行。
* 新增一个**带值**开关 `--history <文件>` → 进 `KNOWN_VALUE_FLAGS`（13 → 14 条）。
  **历史存哪由调用方说**，理由：随手往工程旁边写一个隐藏文件会在用户没要求的地方留下东西
  （本仓自己的 `fixtures/` 第一个就中），而「看不出来不猜」是本仓一贯口径。
* `--undo` / `--redo` **必须**配 `--history`，否则 exit 2 并明说；
  `--op` 与 `--undo/--redo` **互斥**（同时给 exit 2）。
* 退出码：撤销/重做成功 **0**；**没有可撤销/可重做的** exit **2** + `issues:[{code:"nothing_to_undo"}]`
  —— 静默什么都不做比报错难查得多。
* 输出体**沿用** `{ok, summary, written, issues}`，**不新增键**。
* `--op` 配了 `--history` 时：**先压「改动前」的快照，再执行 op，成功才把历史写回文件**
  （失败不该在历史里留一步空的）。
* `--write` 的语义不变：不给就是干跑（`written:false`）。

### 4.4 预览面与 API 版本

* wasm 新增两个导出 `dhampir_project_undo` / `dhampir_project_redo`，返回体**沿用** `{ok, summary, issues}`（不新增形状）。
  这是 API 变更 → `HOST_API_VERSION` **3 → 4**（`crates/dhampir-timeline/src/host_api.rs:208`）。
* 落地三件套（本仓既定流程）：`node scripts/api-surface.mjs --write` 会整份重写 `docs/api-surface.md`
  并只改 `docs/host-api.md` 的 `Version: N` 那一行；**`docs/host-api.md` 的导出名单要人补两行**（守卫双向对，漏了会红）。
* wasm 宿主的 `PROJECT`（`crates/dhampir-wasm/src/timeline_host.rs:60` 的 thread_local）旁边放一份 `History`；
  `dhampir_project_edit` 成功时压栈，**`dhampir_project_open` 成功时清空**（换工程 = 换历史）。
* `web/app.js`：`#editBar` 加「撤销 / 重做」两个按钮，走 `engine.undo()` / `engine.redo()`；`engine.js` 各加一个薄包装。

### 4.5 **两条编辑路径**的口径（必须写下来，否则会踩）

预览里改文档有**两条**路，而历史只认识其中一条：

| 路 | 走法 | 进历史吗 |
|---|---|---|
| 时间线操作（剃刀 / 删除 / 波纹删除 / 拖拽 / 帧率 / CLI 的 `--op`） | `runEdit`（`app.js:436`）→ `dhampir_project_edit` | **进** |
| 属性面板（`renderInspector` 的 `apply`，`app.js:566`） | 直接改 `state.doc`，再 `engine.open(...)` —— **绕过 EditOp** | **不进**（本段边界） |

理由：属性面板那条路走的是 `open`，而 `open` 的语义是**换一份工程**（历史随之清空）。
要让面板也进历史，得给面板造一条新的写入通道（新 EditOp 变体或新导出），
那是**改契约**——本段不做，记在第六节。**这不会让撤销出错**：面板改完再撤销，撤的是**上一条时间线操作**，
所以面板的改动态被当作「当前状态」保留，不会被吞掉。

---

## 五、T4.3 预览拖拽 / 吸附（不加新契约）

* `web/app.js:459` 的 `renderTimeline`：给每个 layer 元素加 `pointerdown` → `pointermove` → `pointerup`。
* 落点用**已有的**坐标换算（时间线元素宽度 ↔ 帧数），`pointerup` 时算 `to` 帧，生成**已有的** `{op:"move", layer, to}`，
  交给现有的 `runEdit`（`app.js:436`）——**不新增 EditOp 变体、不改 wasm 导出、不动 `docs/host-api.md`**。
* 吸附：把落点吸到附近**已有元素的边界**（阈值内）；纯前端计算，仍然只生成同一种 op。
* 拖拽过程中的连续 `move` 用 `push_coalescing` 合并成**一步**（见 4.2）。
* 拖拽**不改 `state.doc` 的结构**：只有 `pointerup` 成功后 `runEdit` 会把新 doc 取回来，与按钮路径完全一致。
* 守卫纪律：`scripts/check-web-invariants.mjs` 钉着「业务规则单份」——`app.js` 里**不许**出现
  `clip_overlap` / `keyframe_out_of_clip` 等码字面量，拖拽的合法性判断一律由 Rust 侧给结论。

---

## 六、没做什么（本段的边界，写下来免得下一轮重新想）

* **不承诺逐帧精确**的剃刀（见 3.3），也不做「每帧一个键」的替代方案。
* **不给属性面板造写入通道**（见 4.5）：面板的改动不进历史，直到有一条真正的「设置属性」编辑操作。
* **不做多档位历史 / 命名快照 / 分支**：只有一条线性历史 + 上限。
* **不做跨会话的自动历史**：CLI 的历史存哪由 `--history` 指定，不给就完全无状态（与今天一致）。
* **不动 `fixtures/`**：本段不新增 fixture。

## 七、验收判据

| 判据 | 为什么是它 |
|---|---|
| CLI `edit --op ... --history H --write` 之后再 `edit --undo --history H --write`，**文件逐字节相同** | roadmap 的原话，也是「快照而不是反向 op」这个选择的直接后果 |
| `--redo` 把上一步**原样**放回来（不是第二份近似的东西） | 快照栈的自然性质，能防「redo 其实是重放 op」 |
| 带关键帧的元素切开后，**切缝两帧与两端点**的值 == 原曲线在那几帧的值 | D2 的验收；也是 3.3 里唯一无条件的承诺 |
| **没有关键帧的元素切开后行为逐字节不变** | 防「为了支持关键帧把老路径改了」 |
| 曲线搬迁之后有像素的地方**逐字节不变** | 纯搬迁的判据（本仓反复证明有效的那种） |
| 拖拽只生成 `move`，且落点与「直接调 CLI move」得到同一个 doc | 防「前端自己算了一套编辑语义」 |
