# task-18：轨道级字幕样式被"最后一条字幕轨"整份覆盖

**仓**：`F:\para\Code\Dhampir`（底座）
**提交**：`ec62ade`（字幕修复）→ `523f716`（字幕判据）→ `79a701f`（端到端）
→ `0e3209b`（**弹幕同形缺陷**，见 §9）

---

## 1. 现象（YekiTrim 端到端数像素抓到，不是推理）

一个真实剪辑工程里（**元素轨 + SRT 字幕轨并存**），底衬
`plate_color = [255,0,0,255]`（纯红不透明）**一个红像素都没有**。

隔离验证 —— **同一条底座二进制、同一份工程，只删掉 SRT 轨**：

| 工程 | 红像素 |
|---|---|
| 含两条字幕轨（`elements` + `srt`） | **0** |
| 只留 `elements`（配底衬的那条） | **7676** |

⇒ 转译器写对了，是**底座在求值层把它吃掉的**。

## 2. 根因

`crates/dhampir-core/src/overlay.rs` 把**轨道级样式放在一个共享槽**里：

```
:297   let mut subtitle_style = TextStyle::default();     ← 循环外，一个槽
:304   for track in &timeline.tracks {                    ← 遍历所有字幕轨
:434       subtitle_style = TextStyle { … };              ← 【每轨整份重写】
:554   subtitle_style,                                    ← 循环【结束后】只返回最后一份
```

⇒ **这一帧活着的最后一条字幕轨说了算，前面所有轨的轨道级样式被整份覆盖。**

### 影响面（远大于 T6）

受影响的是**一切"轨道级"字段**：
`color` / `highlight_color` / `font_ratio` / `outline` / `stroke_ratio` / `stroke_color` /
`shadow_*` / `fade_*` / `rise_*` / `scale_in_*` / `shake_*` / `plate_*` /
`font_family` / `font_weight`。

**不受影响**：**逐条**字段（`TextItem` 的 `dx_px` / `font_ratio` / `scale` / `parts`）——
它们在覆盖发生**之前**就已经 `items.push` 了（`:400` 早于 `:434`）。

⇒ 这正好解释了实测现象：**T1（逐条 `font_ratio`）/ T4（逐条 `dx_px`）活着，
T6（轨道级 `plate_color`）死了**。

**实测旁证**（同一条二进制，唯一变量 = 有没有 SRT 轨，逐帧量外接框与质心）：

```
帧 | 含sub宽 | 无sub宽 | 含sub质心y | 无sub质心y
 0 |    164 |    134 |   1008.04 |   1008.37
 9 |    303 |    254 |    984.42 |    977.65   ← 两条都过冲
19 |    272 |    196 |    989.22 |    990.19
```

起始缩（164/134）、过冲（303/254）、质心抖动**两条都在** ⇒ 逐条通道确实没被碰；
**唯一差别是外接框宽度**（272 vs 196）= **底衬那一圈**。

## 3. ★ 它为什么活了这么久

```bash
grep -c "TrackKind::Subtitle" crates/dhampir-core/src/overlay.rs   # 当时只有 2
```

**整个测试文件里字幕轨只出现过两次** ⇒ **所有既有单测都只放一条字幕轨**，
而这个缺陷**需要 ≥2 条轨才可能显形**（单轨时"最后一条"就是"唯一一条"，行为完全正确）。

⇒ **不是测试写得差，是测试的形状恰好绕过了它。**

**这条已被实测证实**：在"精确复现原 bug"的变异下 ——

| 判据 | 变异下 |
|---|---|
| `两条字幕轨各自的轨道级样式都生效` | **RED** |
| `单条字幕轨的样式与改动前逐字段相等` | **GREEN** |

**单轨判据在 bug 存在时就是绿的** ⇒ 当年无论加多少单轨判据都抓不到它。

## 4. 修法：轨道级样式**挂到每个 `TextItem`**（`track_style`）

```rust
pub struct TextItem {
    // …既有字段…
    pub track_style: TextStyle,   // ← 这一条**所属轨**的轨道级样式
}
```

### 为什么选这个形状（而不是 `Vec<(track_id, TextStyle)>`）

1. **渲染侧本来就逐条取**：`paint_lines` 的循环体里，字体解析、描边、底衬
   **全都按这一条**来。挂条目上 ⇒ 取用是 `&item.track_style`，
   **没有查找、也就没有"查不到时用哪个"这个必然出错的分支**。
2. **形状上不可能再犯同一个错**：共享槽的病根是"多条轨共用一个位置"。
   挂条目上之后，"被后一条覆盖"在类型上就写不出来了。
3. 按 id 查那条路要求**每个取用点都记得查一次**，漏一处的症状恰恰是
   "看着有效、其实用了别人的样式" —— 与本仓最恨的那类静默同形。

### 跟着改的调用方

| 位置 | 改动 |
|---|---|
| `dhampir-worker/src/text_overlay.rs` | `font_for(&item.track_style)`；`paint_one(…, &item.track_style, …)` |
| `dhampir-worker/src/bin/dhampir.rs` | `text_item_json` **逐条**多一个 `"style"` 键；帧级 `"subtitle_style"` **删除** |
| `dhampir-worker` 测试夹具 | 新增 `set_track_style` 助手（"改一处、全帧生效"的共享槽没有了） |
| `dhampir-core` 测试 | 13 处 `overlay.subtitle_style` → `items[0].track_style` |

⚠️ **帧级 `danmaku_style` 也一起删掉了** —— 它有**同一个形状**的问题，
详见下面 §9（已一并修掉）。

⚠️ **JSON 契约变了**（`dhampir overlay --frame` 的 `--json` 输出）：
`subtitle_style` 从**帧级**挪到**每条 item 的 `"style"`**。
wasm 侧 `text_item_json` 要**逐字段同名** —— 漏在任一端就是"预览与成片不一样"，
而两端各自都看不出来。

## 5. 判据与变异自证

| 判据 | 位置 | 变异下 |
|---|---|---|
| `两条字幕轨各自的轨道级样式都生效` | `dhampir-core`（单测） | **RED** ✅ |
| `单条字幕轨的样式与改动前逐字段相等`（R4） | `dhampir-core`（单测） | GREEN（**这本身是证据**） |
| `两条字幕轨并存时配了底衬的那条轨仍然铺底衬` | `dhampir-worker/tests/text_plate.rs`（端到端） | **RED** ✅ |

端到端变异下的原始报错：

```
两条字幕轨并存时底衬没画出来：红色像素只有 0（该 ≥ 500）
—— 轨道级样式被后面那条轨覆盖了（task-18 的原症状）
```

**`红色像素只有 0`** 与现场实测**逐字一致**。

⚠️ **变异自己也要验**：第一版变异写得太粗（不分轨、把所有条目都盖掉），
结果**连单轨判据也红了** —— 那说明变异没复现对，不是判据强。
改成"按轨区间覆盖被盖住的那部分"之后，两条判据的差异才显出来。
**一个"把什么都弄红"的变异证明不了判据有分辨力。**

## 6. 顺带修掉的一个判据缺陷

`单条字幕轨的样式与改动前逐字段相等` 第一版把 `stroke_color` 的默认写成
`[0,0,0,255]`（想当然），实际是 `subtitle_stroke_color()` 的 `[0x40,0x3c,0x3b,255]`。
**是这条测试自己把它抓住的** —— 已按真实默认值改，并在注释里记下来。

## 7. 怎么跑端到端（三条坑，都踩过）

```bash
cd F:/para/Code/Dhampir && mkdir -p .workbuddy-tmp
cargo test -p dhampir-worker --test text_plate --no-run     # ① 先构建
TMPDIR='F:\para\Code\Dhampir\.workbuddy-tmp' \
TEMP='F:\para\Code\Dhampir\.workbuddy-tmp' \
TMP='F:\para\Code\Dhampir\.workbuddy-tmp' \
  ./target/debug/deps/text_plate-*.exe --ignored            # ② 再单跑二进制
```

1. **`TEMP` 不能给 `cargo`** —— 链接器的 `lnk*.tmp` 也会指过去，`link.exe` 直接
   `LNK1104: 无法打开文件` 失败。所以**先 `--no-run`、再单跑二进制**。
2. **"跳过"≠"绿"**：不给 TMPDIR 时用例会**静默跳过**并打印
   `ok … finished in 0.00s` —— 看着像通过，其实一个像素都没画。
   判据看**耗时**（真的跑了是几秒，跳过是 0.00s）+ 看有没有那行"跳过"。
3. **陈旧二进制副本**：`target/debug/dhampir/bin/dhampir.exe`（发布包形状）
   在 `find_binary()` 里**优先于** `target/debug/dhampir.exe`。
   本机那份若是旧的 ⇒ **新编译的底座根本不会被用到**，而症状是
   "改了没效果、还不报错"。跑端到端前先确认它是不是你刚构建的。
   （本机这次**不存在**这个副本，已确认。）

## 9. ★ 同一个病根的**第二处**：弹幕轨（已一并修掉）

修完字幕、写这份文档时回头核对"弹幕那一边呢"，看到弹幕走的是**一模一样的形状**：

```rust
// crates/dhampir-core/src/overlay.rs（修前）
danmaku_style = TextStyle { … };   // ← 循环里每轨整份重写、循环后只留最后一份
```

而 `grep -c "TrackKind::Danmaku"` 在测试文件里**只有 2**
—— **与字幕那个 bug 是完全相同的盲区形状**。

### 但它**没有**变成第二个故障（部分成立）

| 弹幕字段 | 两条弹幕轨并存时 | 原因 |
|---|---|---|
| `color` / `font_ratio` | ✅ **不受影响** | 逐条字段，覆盖**之前**已 push 进 `DanmakuTextItem` |
| `outline` / `stroke_px` / `stroke_color` / `font_family` / `font_weight` | ❌ **被吃掉** | **只**在帧级 `danmaku_style` 里读 |

**可达性已核**：`dhampir-worker/src/text_overlay.rs` 画弹幕时把
`&overlay.danmaku_style` 传给**每一条**弹幕 ⇒ 上面那几项确实会被后一条轨覆盖。

### 先复现，再修（这条最要紧）

弹幕这个原本**只有推理**（字幕那个有 YekiTrim 侧 `7676 → 0` 的实测）。
所以先在**真机**上把它复现出来 —— 端到端数墨像素：

| 夹具 | 墨像素 |
|---|---|
| 单轨（轨 A 带描边） | **246** |
| 双轨（轨 A + 排在后面、不带描边的轨 B） | **2** |

**123 倍塌陷**，与字幕那个 bug 的"从有到无"是同一个签名 ⇒ 可达性推理成立。

### 修法（与字幕完全对齐）

`DanmakuTextItem` 加 `track_style: TextStyle`，帧级 `danmaku_style` **删除**；
样式在**每条轨自己的作用域里**算好、跟着每条 item 走。

**变异自证**（真机端到端）：改回共享槽 ⇒
`双轨 2 vs 单轨（轨 A 带描边）246` —— **RED**；修好后回到 **246**。

⚠️ **没碰那两条有意为之的设计**：弹幕恒传 `scale = 1.0`、恒传 `shake = 0.0`
（`text_overlay.rs` 的注释写了理由：一个恒为常数的字段只会让读的人以为它有意义）。
这次只加了 `track_style`。

### 写这条判据时我自己踩的两个坑（都是"判据建错了量"）

1. **第一版数纯绿（`stroke_color`）** ⇒ 单轨对照也是 0。根因是本仓一条**既有的已知边界**
   （`text_raster.rs` 的 `描边颜色当前不受_stroke_color_控制_这条边界钉住`）：
   libass 路线上填充与描边**共用同一张覆盖度掩码**，`stroke_color` 这一步**消费不了**。
   ⇒ 改成量**墨迹面积**。**判据要建在"能观测的量"上，不是"契约里有的量"上。**
2. **第二版把几项相加**（轨 B + 轨 A 不描边 + 描边增量）—— 也错：两条弹幕的字
   **画在同一批泳道上会重叠**，墨迹不能相加。改成"双轨 vs 单轨**同量级**"。

## 10. 验收

- `cargo check -p dhampir-core -p dhampir-worker` → **0 warnings / 0 errors**
- `cargo test -p dhampir-core --lib` → **252 passed / 0 failed**
- `cargo test -p dhampir-timeline --lib` → **336 passed / 0 failed**
- `cargo test -p dhampir-worker --lib` → **134 passed / 0 failed**
- 端到端 `text_plate --ignored` → **4 passed / 0 failed**（真起 ffmpeg + GPU）
