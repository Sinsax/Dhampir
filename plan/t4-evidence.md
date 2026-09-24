# T4 证据：编辑模型补完（历史层 / 带关键帧的剃刀 / 拖拽吸附）

> 这一段照 `plan/roadmap.md` T4 段三条展开：T4.1（A2、D6）历史层与 `--undo`/`--redo`、
> T4.2（D2）split 支持关键帧、T4.3 预览拖拽/吸附。判据按**退出码**算，不按日志里有没有
> "ok" 字样；本段全部验证跑在 Windows 上。
>
> **已收口**：T4.0–T4.3 全部落地，D2 / D6 / A2 三条已转 `done`。三条的证据都在下面。
> 四段的顺序是：T4.1（已提交 `6475c11`）、T4.2（同 `6475c11`）、T4.3（收口这一笔）。

## 一句话版

| 步 | 落点 | 它的一把尺子 | 结论 |
|---|---|---|---|
| T4.0 | `plan/t4-design.md` | 先落文档再写代码（roadmap 明文要求） | 曲线下沉 `timeline`（决定 A） |
| T4.1 | `dhampir-timeline::history` + CLI `--undo/--redo/--history` | op 之后再 undo 得到逐字节相同的 doc | 22 条 ok / 0 条 FAIL |
| T4.2 | `timeline::curve` 一份求值 + `split` 补切缝键 | 切缝两帧与两端点 == 原曲线 | 4 个缓动全部精确（下表） |
| T4.3 | 预览拖拽 + 吸附 + 两颗按钮（wasm `undo`/`redo`） | 拖一下只生成一条 `move`，撤销回到拖动前 | 真机判定 ✓ |

## T4.0 先落文档：曲线求值放哪儿 —— 决定 A（下沉 timeline）

`crates/dhampir-core` 的 `opacity_from` 是**唯一**的关键帧求值实现（`compose.rs`），而
`dhampir-timeline` 不能依赖 core（依赖方向是 core → timeline）。`split` 要算"切点那一刻的
值"，就得有两个选择：① 在 timeline 里另写一份求值；② 把求值**下沉**到 timeline，core 转发过去。

选了 **②（决定 A）**，判据两条，都验过：

1. **有像素的地方逐字节不变** —— 见下面「收口时的口径」里的对拍（两条腿 22552 字节、
   SHA256 `42E6…D21A6` 完全相同）；
2. **core 既有的关键帧用例一字不改继续绿** —— `crates/dhampir-core/src/compose.rs` 的求值
   调用点改成转发之后，它的用例与断言一个字符都没动。

② 的代价是"多一次函数调用"，换来的是**求值只有一份**：两份实现迟早会在某个缓动上分叉，
而分叉的那一帧只有肉眼能看见。

## T4.1 历史层（A2、D6）：零依赖的 `history` + CLI `--undo` / `--redo`

### 它解决什么

`edit::apply` 一直是纯函数（不合理就原样还回来），所以"编辑不可逆"缺的不是规则，
是**安放"改动前的那一份"的地方**。历史层补的就是这一处，并且**只补这一处**：

* `crates/dhampir-timeline/src/history.rs`：零依赖（不引任何第三方、不碰 core）。
  公共 API：`new(cap)` / `cap` / `depth` / `redo_depth` / `can_undo` / `can_redo` /
  `reset` / `push` / `push_coalescing` / `undo` / `redo`。`new` 是 `const fn`（wasm 侧要放进
  `thread_local!` 的 `const` 初始化里），`cap == 0` 时当 1 处理 —— "上限为零"不是"退不动"。
* **退一步拿回来的是当初那一份快照**，不是"把 op 反过来做一遍"。反向 op 要为每种 op 各写一条
  逆运算，其中一条写歪就是"撤销之后看起来对、逐字节不对"；快照栈没有这个失效模式。
* 新的一步压进来时**重做栈作废**（线性历史，不是树）。

### 判据一：CLI 端到端 22 条 ok（原始输出）

`target/t4/e2e-undo.cjs` 走的是**真二进制**（`target/debug/dhampir.exe`），
两步编辑 + undo + redo + 退到底 + 空历史 + 干跑 + 四条用法错误，逐条看退出码与 stdout：

    $ node target/t4/e2e-undo.cjs
      ok   第一步 split 成功（exit 0）
         H1（split 之后）= 7069 字节 / D05942151CC0CD38DBB42C712664994EEA23BC753430C742E5B607135CE0456E
      ok   第二步 trim 成功（exit 0）
      ok   第二步真的改了文件（H2 != H1）
         H2（trim 之后）= 7069 字节 / A0C2B08C309474CD5337C435CA40563B58450CE679E0B379434808EC4CB72173
      ok   undo 成功（exit 0）   <- "summary": "撤销：修剪元素 a-b 到 22"
      ok   undo 之后与「第一步之后」逐字节相同  <- D0594…456E vs D0594…456E
      ok   undo 的说明说的是被退掉的那一步
      ok   redo 成功（exit 0）   <- "summary": "重做：修剪元素 a-b 到 22"
      ok   redo 之后与「第二步之后」逐字节相同  <- A0C2B…2173 vs A0C2B…2173
      ok   再 undo 回到 H1
      ok   退到底之后还能退（exit 0）
      ok   退到底之后连退第三次都要给 no_to_undo
      ok   历史文件不在 = 空历史，undo 退 2 且报 nothing_to_undo
      ok   空历史那次 undo 不许碰工程文件
      ok   空历史那次 undo 不许凭空造出历史文件
      ok   干跑成功（exit 0）
      ok   干跑之后 written=false
      ok   干跑不许写工程文件
      ok   干跑不许写历史文件
      ok   --undo 不给 --history 要退 2  <- --undo / --redo 要跟 --history <文件>：历史存哪得由你说，本工具不替你猜
      ok   --undo 与 --op 同时给要退 2  <- --op 与 --undo / --redo 只能给一个：一次只做一件事
      ok   render --undo 要退 2（别的子命令不许静默收下）  <- --undo / --redo / --history 只有 edit 认（现在给的是 render）
      ok   什么都不给要退 2 并说清两条路  <- edit 要 --op <JSON>，或者 --undo / --redo（配 --history <文件>）
    全部通过

日志原文：`target/t4/e2e-undo.log`（22 条 `ok`、0 条 `FAIL`、末行「全部通过」）。
三处值得单拎出来：

* **"逐字节相同"是拿两次运行的文件哈希比出来的**，不是"看着像"：undo 之后重算的 SHA256
  与第一步之后那一份完全相同；
* **退不动是 exit 2 且带码**（`nothing_to_undo` / `nothing_to_redo`），不是静默成功 ——
  "没得退"与"退成功"给同一个退出码的话，脚本与人都分不出来；
* **退不动的那一次不许碰任何文件**（工程文件与历史文件都验了）—— 失败路径不该有副作用。

CLI 落点：`--undo` / `--redo` 进 `KNOWN_FLAGS`（5 → 7）、`--history <文件>` 进
`KNOWN_VALUE_FLAGS`（13 → 14），`HISTORY_CAP = 64`；
**历史存哪必须由调用方说**（不给 `--history` 就完全无状态）—— 工具不替用户猜一个路径出来写盘。

### 判据二：预览侧与 CLI 共用同一条规则（wasm 5 条 + 页面判定）

预览侧的历史是 wasm 宿主里的一个 `thread_local`（`HISTORY`，`History::new(64)`），
`dhampir_project_open` 成功就 `reset()`（换一份工程就是换一条历史）。
**存的规则与 CLI 是同一个 `History`** —— 两份实现的话，"CLI 退得回去、预览退不回去"迟早出现。

    $ node scripts/run-wasm-tests.mjs
      test timeline_host::tests::撤销一步逐字节回到编辑前 ... ok
      test timeline_host::tests::重做把上一步原样放回来 ... ok
      test timeline_host::tests::退不动时给码且一个字节都不动 ... ok
      test timeline_host::tests::失败的那一步不进历史 ... ok
      test timeline_host::tests::换一份工程就换一条历史 ... ok
      ✓ wasm32 运行时上 15 个测试全过（2 个目标，退出码全为 0）
        与源码对账：15 个 #[wasm_bindgen_test] == 15 条实际列出并跑过

页面那一路（点两颗真按钮）由 T4.3 的判定通道走到，见下一段。**"失败的那一步不进历史"**
这条是刻意的：编辑没生效就不该占一格撤销，否则用户要连按两次才退得动。

### 边界（不假装）

* **属性面板的改动不进历史**（T4.1 的显式边界，写在 `plan/t4-design.md` 4.5）：面板那条路走的是
  `open`，而 `open` 的语义是"换一份工程"（历史随之清空）。要让面板也进历史，得给它造一条新的
  写入通道（新 EditOp 变体或新导出）—— 那是改契约，这一段不做。**它不会让撤销出错**：面板改完
  再撤销，撤的是上一条时间线操作，面板的改动被当作"当前状态"保留。
* **`push_coalescing` 实现了但没接线**（它的单测在 `history.rs` 里，生产路径上零调用点）：
  原计划是让拖拽过程中的每一步 `move` 用合并键压成一步，T4.3 换了更省的做法（见下），
  于是这条 API 暂时是"备着"的 —— 这一点如实记在这里，免得下次读代码的人以为它在用。
* **没有多档位 / 命名快照 / 分支**：只有一条线性历史 + 上限 64。

## T4.2 带关键帧的剃刀（D2）：切缝两侧各插"当时的值"

### 它解决什么

`split` 原先遇到带关键帧的元素**直接拒绝**（不是切歪，是"这个不会做"）。现在两半各自在切缝上
补一个键：左半段在 `cut - 1` 补"原曲线在那里的值"、右半段把 `frame >= cut` 的键整体左移之后再在
`0` 补"原曲线在 `cut` 的值"。两处插入都是**条件插入**（那一帧本来就有键就不插）——
否则切点正好落在键上时会多出一个重复键。两半都按 `frame` 再排一次，
因为"存下来的顺序稳定"才谈得上逐字节可比。

两个细节值得写下来：

* **缓动抄谁**：新插的键用 `seam_easing(keys, cut)` 定 —— 取切点右侧最近的键的缓动，
  右侧没有键就退回最右那个键的缓动。随手给个 `linear` 会在 ease 系列上把切缝周围的形状改掉。
* **切缝的端点值在两半里都一样**：左半段的 `cut - 1` 与右半段的 `0` 都是**原曲线**在
  对应帧的值，所以拼接处的画面接得上。

### 判据：切缝两帧与两端点恒精确

Rust 用例（`crates/dhampir-timeline/src/edit.rs`）：

    test edit::tests::剃刀切带关键帧的元素_切缝两帧与端点精确 ... ok
    test edit::tests::切点正好落在键上时右半段不再插一个重复的 ... ok
    test edit::tests::剃刀之后两段的源帧必须连续 ... ok
    test edit::tests::剃刀不接受落在元素外面的切点 ... ok

第一条就是 D2 的验收原话：**切点两侧的端点值 == 原曲线在切点的值**（两侧各插一个当时的值）。

### 精确到什么程度（草稿区的量化）

承诺收窄成"切缝两帧与两端点精确"不是偷懒，是量出来的（`target/t4/split-exactness.cjs`，
把 `opacity_from` 的语义照抄一遍，逐个缓动比每一帧）：

    linear      左半段最大偏差=0.000000  右半段最大偏差=0.000000  切缝两帧：原 [0.617500, 0.625000] 新 [0.617500, 0.625000]  两端点 ok
    ease_in     左半段最大偏差=0.000000  右半段最大偏差=0.093750  切缝两帧：原 [0.430075, 0.437500] 新 [0.430075, 0.437500]  两端点 ok
    ease_out    左半段最大偏差=0.093673  右半段最大偏差=0.000000  切缝两帧：原 [0.804925, 0.812500] 新 [0.804925, 0.812500]  两端点 ok
    ease_in_out 左半段最大偏差=0.120000  右半段最大偏差=0.124950  切缝两帧：原 [0.610150, 0.625000] 新 [0.610150, 0.625000]  两端点 ok

也就是说：**切缝那两帧与两个端点，四种缓动下都逐位相同**；而两半段**内部**的中间帧在非线性
缓动下会有偏差（最大 0.125，即 8 位色里约 32 级）。第二种情形（切点正好落在某个键上）另有一份
（`split-exactness-keyboundary.cjs`）：两侧都落键时偏差 0，只一侧落键时那一侧 0、另一侧很小。

**这一条是刻意的边界**：要逐帧精确就得"每帧一个键"，那是把曲线换成点表 ——
工程文件会长得没法看，而用户手里的曲线也再改不动了。所以：**不承诺逐帧精确**，
承诺的是接缝处接得上、两端点不掉。

## T4.3 预览拖拽 / 吸附（不加新契约）

### 它解决什么

时间线上的元素现在可以**拖到别的时间位置**，并且落点会**吸**到附近已有元素的边界上。
两条纪律写死在实现里（`plan/t4-design.md` 五）：

* 拖一下只生成**已有的** `{op:"move", layer, to}` —— **不新增 EditOp 变体**，
  "能不能挪到那儿"永远由 Rust 的 `move` 给结论；
* 拖动过程中**只改那一条 bar 的 `left`**（纯视觉预览，`state.doc` 一个字都不动），
  落点在 `pointerup` 时向 Rust 提**一次**。

吸附的阈值与换算只有一份事实：`pointerdown` 时量**轨道**的 `clientWidth`
（不量 bar —— bar 自己有最小宽度 1.5%，拿它换算会在短元素上算歪），
`framesPerPixel = end / trackWidth`，阈值 `snapFrames = round(6 × framesPerPixel)`。

**wasm 侧新增的是一对导出**：`dhampir_project_undo` / `dhampir_project_redo`
（返回体沿用编辑那一套 `{ok, summary, issues}`），于是 `HOST_API_VERSION` 3 → 4 ——
**升的不是形状**（形状一个字节没变），是"对端能看到的导出面"多了一对函数。
`docs/host-api.md` 的 `v3 -> v4 变了什么` 与文末名单都补了，两处由 `scripts/api-surface.mjs` 钉住。

### 判定通道：`--verdict undo-drag`

与 `subtitle` 判定同一条通道（页面 `reportVerdict` → 本机后端 → 驱动读回），**手动**跑：

    $ node scripts/web-check.mjs --local --verdict undo-drag

页面做三件事、**结论不在页面里下**：

1. 合成 pointer 事件**真的拖一下**（走的是与手拖同一条 pointerdown/move/up 路）：挑一个
   "别人的边界旁边一帧"的落点，于是"有没有吸附"在回传的事实里看得见 —— 原始落点不是任何边界、
   最终落点是；
2. 把那一刻的**预览帧**从 `bar.style.left` 读回来（那时 `state.doc` 一个字都还没动，
   所以读到的只可能是预览）；
3. 点那两颗**真按钮**（撤销 / 重做）各一次，把拖动前 / 拖动后 / 撤销后 / 重做后四份工程原样回传。

驱动侧（`runUndoDragParity`）拿 CLI 与 fixture 对账：`before` 必须是 fixture 的子集、
原始落点不能是任何边界、落地必须是别人的边界且距离 ≤ 重算出来的半径、
**同一条 `move` 由 CLI 复算后与"拖动后"逐字段相同**、撤销逐字段回到拖动前、重做原样放回来。

### 第一次真机跑是红的（原始输出）

    $ node scripts/web-check.mjs --local --verdict undo-drag      # 退出码 1
    判定回传：undo-drag
      - 页面自己说这次编辑没成立：落地在 0 帧，不是吸附边界 10 帧；引擎的说明是 "移动 没生效：layer_overlap"，
        不是 "移动：把 a 移到第 10 帧"；撤销的说明是 "撤销 没生效：nothing_to_undo"，
        不是 "撤销：把 a 移到第 10 帧"；重做之后 a 在 0 帧，不是 10 帧；重做的说明是 "重做 没生效：nothing_to_redo"，
        不是 "重做：把 a 移到第 10 帧"

原文：`target/t4/verdict-undo-drag.utf8.log`。**这一趟红得有价值**：根因不是代码错了，是**场景挑错了** ——
第一版判定固定拖"第一条视频轨的第一个元素"（fixture 里的 `a`，0–30），而那条轨道
（`a` 0–30 / `b` 30–60 / `c` 60–90）**已经排满**，`a` 挪到任何位置都会撞同轨邻居，
Rust 当然拒（`layer_overlap`）。也就是说：判定在验"吸附"，却先被"这条轨道挪不动"挡住了。

修法**不是**在页面里预判 Rust 会不会接受（那会把"规则只有一份"这句话咬断），而是：
**场景由"边界旁边一帧"这个一般形态生成**（先挑只有一层的轨道 —— 那种轨道上同轨重叠这条规则
不可能触发，但结论仍由 Rust 下），一个一个试，**靠状态栏那句话变没变判引擎收没收**，
被拒就换下一个；一次都没拖成就如实报 `ok:false`，不掩盖。

### 第二次真机跑（收口这一趟）

    $ node scripts/web-check.mjs --local --verdict undo-drag      # 退出码 0
      ✓ 拖 d：原始落点第 1 帧（不是任何边界）→ 吸到第 0 帧（别人的边界；半径 round(6 × 90 / 381) = 1 帧）；
        同一条 move 由 CLI 复算后与「拖动后」逐字段相同；撤销逐字段回到拖动前、重做原样放回来；
        引擎自己给的说明：移动：把 d 移到第 0 帧 → 撤销：把 d 移到第 0 帧 → 重做：把 d 移到第 0 帧

这一行里可核的事实五条：① 拖的是**单层轨道**上的 `d`（10–50），落点原始值 1 帧
（`a` 的边界 0 旁边一帧，**不是**任何一条边界）；② 吸附把它拉到 0 帧（`a` 的起点 = 别人的边界），
半径按页面上量的轨道宽 381 px 重算是 `round(6 × 90 / 381) = 1` 帧，与页面报的一致；
③ 同一条 `move` 由 **CLI 复算**，改出来的工程与页面"拖动后"**逐字段相同**；
④ 撤销逐字段回到拖动前、重做原样放回来（四份工程都是页面回传的原文）；
⑤ 三句说明都是引擎自己给的（`移动：` / `撤销：` / `重做：` 前缀 + 被操作的那一步）。

### 偏离设计文档的一处：中间落点不发 Rust

`plan/t4-design.md` 4.2 原计划是"拖拽过程中的连续 `move` 用 `push_coalescing` 合并成一步"。
实现时换了更省的做法：**中间落点根本不发给 Rust**（那些帧只存在于 `bar.style.left` 这个
视觉预览里），只在 `pointerup` 提**一次** `move`。

* 结果一样：拖一下在历史里正好是**一步**，撤销一次就回到拖动之前（上面判定里验的就是这个）；
* 少两样东西：Rust 侧少一条新接口、历史侧不必依赖合并语义；
* 代价：拖动过程中**看不到**"这一步会被拒"（比如同轨重叠）—— 拒绝发生在松手那一下，
  与"点按钮"走同一条失败路径（状态栏写 `移动 没生效：layer_overlap`，`state.doc` 不动）。
  这是**已知的体验边界**，不是漏了。

于是 `history::push_coalescing` 这一段**没有生产调用点**（它的单测仍在，见 T4.1 的边界那一段）。

### 边界（不假装）

* **判定只验一次拖拽**（一条单层轨道上的一个元素、一个落点）；多元素、连续多次拖拽、
  跨轨道拖拽都没验 —— 拖拽只生成 `move`，而 `move` 的合法性本来就由 Rust 的既有用例覆盖。
* **真机判定不在 17 个守卫里**（单次约 1–3 分钟：真实浏览器 + 本机后端 + 一次 CLI 复算），
  它是**手动**通道 —— 这份证据就是它的原始输出。
* **触摸与多指没有验**：合成事件走的是 `pointerdown/move/up` 那条路，
  `touch-action: none` 只是让浏览器别截走手势，没有真机触摸这一趟。
* **吸附半径随窗口宽度变**：`round(6 × end / trackWidth)`，本趟窗口下是 1 帧。
  窗口更宽时半径会变小（到 0 就是"不吸附"）—— 这是刻意的（阈值是像素概念，1 px 的抖动
  在小窗口下就是几帧），但也就意味着"吸附"这件事的**手感**与窗口尺寸有关。

## 收口时的口径（全绿清单）

    $ node target/t4/verify.cjs
    guards exit=0（59s）
    cargo-test exit=0（1s）
    cargo-check exit=0（0s）
    wasm-tests exit=0（6s）
    api-surface exit=0（0s）
    byte-identical exit=0（5s）
    全部退出码 0

逐条的内层结论（日志在 `target/t4/*.log`）：

* **17 / 17 个守卫全绿**（每个守卫都跑了 `--self-test` 与正跑，`target/t4/guards.log`）；
  其中与本段直接相关的三句：
  `✓ 调用面清单与宿主 API 都与代码一致（6 个模块 / 62 个导出；版本 4，docs/host-api.md 列了 26 个导出）`、
  `✓ web 层不变量成立（叶子依赖 / 引擎无框架 / 业务规则单份 / 里程碑在）`、
  `OK 缺陷台账自洽（条目 26 条，路线图引用齐全，证据路径都存在）`。
* **`cargo test --workspace` 全绿**：`dhampir-timeline` **199 passed; 0 failed**（其余各 crate 的
  `test result: ok` 一律 0 failed；有 `ignored` 的那几条是既有的、与本段无关的用例）。
* **`cargo check --workspace --all-targets` 退出 0**（0 warning —— Windows 上 link.exe 的进度行
  与 PowerShell 的 `NativeCommandError` 行不算代码告警）。
* **对拍逐字节不变**：`target/wt-head-target/debug/dhampir.exe`（"改动前"那一份）与当前的
  `target/debug/dhampir.exe`，同参数出 90 帧：

      字节 22552   SHA256 42E6195C0E090D3CA7545D1E8FE3EFE3ABE2771BEA4B6E2A8A808B6DA04D21A6   （两条腿相同）

  这是"曲线搬迁之后有像素的地方逐字节不变"那条判据的实测 —— 曲线下沉到 timeline 之后，
  同一个工程出片的字节一个都没变。

## 这一段没改契约（除了导出面）

* **EditOp 一个变体没加**、`ProjectDoc` 的键一个没动、`fixtures/sample-project.doc.json` 与
  `fixtures/sample-project.json` 一个字节没改（本段不新增 fixture）。
* `HOST_API_VERSION` 3 → 4，动的**只有导出面**（多一对 `undo`/`redo`），形状不变 ——
  理由写在 `crates/dhampir-timeline/src/host_api.rs` 那一行常量上面的注释里。
* `www/pkg` 是 gitignored 的构建产物，改完 Rust 后用 `wasm-pack build --dev --target web`
  重建过一次（`check-web-invariants` 钉着"pkg 比源码旧就红"，本段没碰那个守卫）。

## 覆盖边界（不假装）

* **只验了 Windows**：Linux 两条腿本机没有（`D13` 记的是不跑腿），本段的证据都是 Windows 上的。
* **真机判定只覆盖一次拖拽**（见 T4.3 的边界）。
* **`split` 的承诺是"接缝精确"，不是"逐帧精确"**（见 T4.2 的量化）。
* **属性面板不进历史**（见 T4.1 的边界）。
* **`push_coalescing` 没有生产调用点**（同上，如实记着）。
* **CLI 的历史文件没有并发保护**：两个进程同时对同一个 `--history` 文件写，后写的赢
  （快照是整份、顺序是"先写历史后写工程"，所以不会读到半份，但会丢一步）。
  单机单人场景下这不是问题，写下来免得被当成保证。
