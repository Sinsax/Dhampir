# T5 证据：解码与素材通路（倒放 / 同源多帧 / 并发数）

> 这一段照 `plan/roadmap.md` T5 段三条展开：T5.1（A6、D3）**先量化再决定**、
> T5.2（A7、D4）按 (源, 源内帧) 键的纹理池、T5.3（A10、D11）并发数与代价。
> 判据按**退出码**与**逐帧摘要是否相等**算，不按日志里有没有"ok"字样。
>
> **已收口**：T5.1–T5.3 全部落地，D3 / D4 / A6 / A7 / A10 / D11 六条已转 `done`。
> 数字本体在 `plan/measurements.md` 第七项；这份文件是**怎么验的**与**边界在哪**。

## 一句话版

| 步 | 落点 | 它的一把尺子 | 结论 |
|---|---|---|---|
| T5.1 | `examples/rewind_census.rs` + `measurements.md` 七 | 先有数再决定（roadmap 明文） | 回退 30.7%，p50 98 帧 -> 选**有界池子 + 重启** |
| T5.2 | `pipeline.rs` 的 `SourcePool` / `PoolCursor` | 倒放工程与**顺序引用版本逐像素一致** | 90 帧摘要全等（真机） |
| T5.2 | 同上，同源画中画 | 同上 | 30 帧摘要全等（真机） |
| T5.3 | `examples/decode_cost.rs` + `measurements.md` 7.4 | 并发数与代价都要有数 | 并发数 = **1**，每源帧读 9.7~18.9 ms |

## T5.1（A6、D3）先量化：回退有多常见

**没有数字就不许决定**，所以第一步既不是改代码也不是选方案，是造一把尺子。
`crates/dhampir-worker/examples/rewind_census.rs`：**纯的**（不碰 GPU、不碰 ffmpeg）——
取帧顺序从求值层摊出来，池子行为跑**产品同一段状态机**（`PoolCursor`）。

    $ cargo run -q -p dhampir-worker --example rewind_census -- \
        fixtures/sample-project.doc.json fixtures/rewind-project.doc.json fixtures/pip-project.doc.json
    ... 三行工程 + 一行合计 ...
    合计: 请求 290, 回退 89 (30.7%), 距离 p50 98 / p90 160 / 最大 178

**两条据实的结论**：

1. 回退**很常见**（30.7% 的请求），而且**退得不浅**（中位 98 帧）—— 所以
   **「按源内顺序重排渲染」被否掉**：它会打乱输出帧顺序，而判据是**逐像素一致**，
   重排等于把"同一帧的结果"变成两件事；
2. 选 **有界池子 + 装不下就重启解码器从 0 再读**。回退因此从"整次出片失败"变成
   "**慢一点，但给得出来**"。

## T5.2（A7、D4）池子：键是 (源, 源内帧)

### 结构

`pipeline.rs` 里 `SourceStream`（一路源一张纹理）换成 `SourcePool`：

* **键**：`slots: HashMap<Frame, TextureView>`，按**源内帧号**存，
  于是同一输出帧里同一个源要两个不同源内帧也给得出来（这就是 D4）；
* **游标**：`PoolCursor`（`plan_fetch` / `commit`）—— **纯状态机**，
  真池子与量化脚本走的是同一段规则；
* **留什么**：只留「**等一下还会再被要**」的帧（需求表按**次数**记，见 `demand_of`）。
  顺序出片时每帧只被要一次，用完就扔 —— 于是池子占用与"一路源一张纹理"时一样，
  **常见路径不涨内存**（这一条由 7.2 里 `sample-project` 在所有槽数下读数不变钉住）；
* **回退**：`restart()` 杀掉这一路解码器**重新起一个**（不许 seek，那只能从头再来），
  池子里的纹理不受影响。

### 判据一：倒放工程与顺序引用版本**逐像素一致**（真机）

夹具**成对**，内容一模一样，差别只在"每一路要不要回退"：

* `fixtures/rewind-project.doc.json` —— 一份素材、三段在时间线上前后颠倒（`a.mp4`，倒放）；
* `fixtures/rewind-sequential.doc.json` —— 同样三段、同样源内位置，但登记成三个 id
  （`a/b/c.mp4` **指向同一个文件**），于是每一路都只向前读；
* `fixtures/pip-project.doc.json` —— 同一素材同时当"底"与"画中画"，相距 100 帧；
* `fixtures/pip-sequential.doc.json` —— 同样两层，小窗那层换另一个 id。

    $ cargo test -p dhampir-worker --test rewind -- --ignored --test-threads=1 --nocapture
    rewind-project.doc.json: 90 帧, 解码器 1 路, 命中 32, 向前 57, **重启 1**, 读源帧 234
    rewind-sequential.doc.json: 90 帧, 解码器 3 路, 命中 0, 向前 90, **重启 0**, 读源帧 357
    pip-project.doc.json: 30 帧, 解码器 1 路, 命中 29, 向前 31, **重启 0**, 读源帧 159
    pip-sequential.doc.json: 30 帧, 解码器 2 路, 命中 0, 向前 60, **重启 0**, 读源帧 218
    test result: ok. 2 passed; 0 failed; 0 ignored

* **回退那条路真的被走到**，而且是**断言**出来的（`reverse.decode.replays > 0`）——
  没被走到的判据等于没有判据；
* 90 帧 / 30 帧的 FNV-1a 摘要**逐帧相等**，且这些帧彼此**互不相同**
  （90 个摘要 90 个值），所以这不是"两边都渲染成空白"那种假绿；
* 四个夹具的**问题清单都是空的** —— `source_rewind` / `source_frame_conflict`
  这两条以前正是在这里把整次出片判失败的。

### 判据二：这条用例**抓得住**回归（反向用例，验过）

只跑绿不够，还要证明它**会红**。把池子的命中路径改坏
（`slots.get(&target)` -> `slots.values().next()`，即交回一张**随便**的纹理）：

    倒序引用改了像素：第 66 帧像素不同：d88f88c6d1684634 vs d0d0706d81457b4d
    同源两帧被搞混或错位了：第 2 帧像素不同：ad80a6e390cc93bf vs d2fc21c74797afbe
    test result: FAILED. 0 passed; 2 failed

改坏之后**两条都红**，且指到具体帧号；按字节还原（SHA256 相同）后两条复绿。

> 顺带一条：**画中画那条现在不是靠"重启"过的**。池子上限从 192 MiB 抬到 256 MiB 之后
> （1080p 下 24 -> 32 槽），它落在"刚好留得住"的那一格，于是第二次要旧帧是**命中**、
> 一次都不重读（读帧量 371 -> 159）。这条比"能出片"更值得钉：**复用的收益本身就是判据**，
> 所以用例里还断言了「池子那条读的帧比两路顺序解码更少」（159 < 218）。

## T5.3（A10、D11）并发数与代价

**并发数是 1**：每个输出帧里按层序依次调 `texture_for`
（`crates/dhampir-core/src/render/timeline.rs:362`），每一次都是一次**阻塞**的 `read_exact`
（`crates/dhampir-worker/src/pipeline.rs:649`）。所以同时最多一个源在解码，
哪怕 `opened_streams` 开着 N 路 —— **"开着几路"与"并行度"是两件事**。

代价两项（进程与内存：N 个 ffmpeg 子进程 + N 个池子上限；不重叠：解码不与合成/读回重叠）
与实测表在 `plan/measurements.md` 7.4。**D11 的"下界"补测**也在那里：
同一条时间线、同样 271 次读，四份不同文件 56.96 ms/帧 vs 四份同一文件 40.90 ms/帧。

**为什么不顺手把解码并发化**：没有量到它是瓶颈（每帧 25~57 ms，整段 4.6~8.2 s，
而池子已经把回退变成命中）。并发化的代价是确定的，收益是没量到的 —— 所以这一轮不动它。

## 边界：这一段**没验到**什么

1. **mp4 编码那条腿这次没跑。** 这个 agent 会话里进程**起不了编码器**
   （`所有的管道范例都在使用中。 (os error 231)`，见 `plan/next-steps.md` 坑 19）。
   上面两条像素判据走的是 `render_frames_png_run` —— **同一条**解码 -> 上传 -> 求值 ->
   合成 -> 回读的路，只把 sink 从 mp4 换成 PNG，少的只有"喂编码器"那一跳（与源无关）。
   所以"倒放工程能出片"这句话**这次只验到 PNG 那一半**；mp4 那一半要在一个普通终端里
   跑 `node scripts/check-cli.mjs`（它带 `render` 用例）才算数。
2. **`measure-export.mjs` 那个数与第七项那把尺子对不上账**（14.56 对 40.90 ms/帧）。
   要在一个能起编码器的终端里重跑才说得清 —— 详见 `measurements.md` 7.4，
   那里同时写了"在有人重跑之前，两边的数都别当结论用"。
3. **"四份同一文件"与"四份文件大小不同"两个原因没分开**（7.4 的量纲限制里写了）。
4. **池子上限是算出来的、不是扫出来的**：256 MiB / 64 帧来自"画中画要 29 帧、
   1080p 一帧 8.29 MB"这两条实测，但**没有**在真机上扫过"抬到 512 MiB 会不会更快"
   （扫得先能起编码器，见 1）。

## 台账怎么改的

| id | 原状态 | 现状态 | 依据 |
|---|---|---|---|
| D3 | todo | **done** | 倒放工程 90 帧全等 + 重启被断言到（上面判据一） |
| D4 | todo | **done** | 画中画 30 帧全等 + 按 (源, 源内帧) 复用（159 < 218） |
| A6 | todo | **done** | 先量化（七项 7.1）再选路（池子 + 重启） |
| A7 | todo | **done** | `SourcePool` 键为源内帧号 |
| A10 | todo | **done** | 并发数 = 1 并说明代价（7.4） |
| D11 | todo | **done** | 四份不同文件补测 + 口径与边界写进 7.4 |

## 改动清单

* `crates/dhampir-worker/src/pipeline.rs` —— 池子、纯状态机、`pool_slots` 与两个常数、
  `PngRun`（PNG 那条路也报解码账，与出片那条路同一组事实）；
* `crates/dhampir-worker/src/bin/dhampir.rs` —— `done` 事件带上 `decode`；
* `crates/dhampir-worker/examples/rewind_census.rs`（新）—— T5.1 的尺子；
* `crates/dhampir-worker/examples/decode_cost.rs`（新）—— T5.3 的尺子；
* `crates/dhampir-worker/tests/rewind.rs`（新）—— 上面两条真机判据 + 它们的反向用例；
* `fixtures/rewind-project.doc.json`、`fixtures/rewind-sequential.doc.json`、
  `fixtures/pip-project.doc.json`、`fixtures/pip-sequential.doc.json`、
  `fixtures/four-asset-project.doc.json`；
* `plan/measurements.md` 第七项、`plan/defects.md` 六条状态。
