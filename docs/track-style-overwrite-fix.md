# task-18：轨道级字幕样式被「最后一条字幕轨」整份覆盖
**仓**：dhampir 底座　**提交**：`ec62ade`（字幕修复）→ `523f716`（字幕判据）→ `79a701f`（端到端）→ `0e3209b`（**弹幕同形缺陷**，见 §9）
## 1. 现象
真实工程（元素轨 + SRT 字幕轨并存）：底衬 `plate_color = [255,0,0,255]` 一个红像素都没有。同一条二进制、同一份工程，只删 SRT 轨：

| 工程 | 红像素 |
|---|---|
| 含两条字幕轨（`elements` + `srt`） | **0** |
| 只留 `elements`（配底衬那条） | **7676** |
⇒ 转译器写对了，是底座在求值层把它吃掉的。
## 2. 根因
`crates/dhampir-core/src/overlay.rs` 把轨道级样式放在循环外的共享槽里：
```rust
:434       subtitle_style = TextStyle { … };   ← 每轨整份重写（循环外 :297 声明、:304 遍历所有字幕轨）
:554   subtitle_style,                         ← 循环结束后只返回最后一份
```
**影响面**：一切轨道级字段 —— `color` / `highlight_color` / `font_ratio` / `outline` / `stroke_ratio` / `stroke_color` / `shadow_*` / `fade_*` / `rise_*` / `scale_in_*` / `shake_*` / `plate_*` / `font_family` / `font_weight`。**不受影响**：逐条字段（`TextItem` 的 `dx_px` / `font_ratio` / `scale` / `parts`）—— 在覆盖发生**之前**已 `items.push`（`:400` 早于 `:434`）。
## 3. 它为什么活了这么久
`grep -c "TrackKind::Subtitle" crates/dhampir-core/src/overlay.rs` 当时只有 **2** ⇒ 既有单测都只放一条字幕轨，而缺陷需 ≥2 条轨才显形（单轨时「最后一条」就是「唯一一条」）。精确复现原 bug 的变异下：`两条字幕轨各自的轨道级样式都生效` **RED**，`单条字幕轨的样式与改动前逐字段相等` GREEN —— 单轨判据在 bug 存在时就是绿的。
## 4. 修法：轨道级样式挂到每个 `TextItem`
给 `TextItem` 加 `pub track_style: TextStyle`（这一条所属轨的轨道级样式）。

挂条目上而不是 `Vec<(track_id, TextStyle)>`：渲染侧本来就逐条取（`&item.track_style`），没有查找、也就没有「查不到时用哪个」的分支；按 id 查要求每个取用点都记得查一次，漏一处的症状是「看着有效、其实用了别人的样式」。

| 位置 | 改动 |
|---|---|
| `dhampir-worker/src/text_overlay.rs` | `font_for(&item.track_style)`；`paint_one(…, &item.track_style, …)` |
| `dhampir-worker/src/bin/dhampir.rs` | `text_item_json` 逐条多一个 `"style"` 键；帧级 `"subtitle_style"` **删除** |
| `dhampir-worker` 测试夹具 / `dhampir-core` 测试 | 新增 `set_track_style` 助手；13 处 `overlay.subtitle_style` → `items[0].track_style` |
⚠️ 帧级 `danmaku_style` 一并删除（同一形状，见 §9）。**JSON 契约变了**：`dhampir overlay --frame` 的 `--json` 里 `subtitle_style` 从帧级挪到每条 item 的 `"style"`；wasm 侧 `text_item_json` 要逐字段同名。
## 5. 判据与变异自证
| 判据 | 位置 | 变异下 |
|---|---|---|
| `两条字幕轨各自的轨道级样式都生效` | `dhampir-core`（单测） | **RED** |
| `单条字幕轨的样式与改动前逐字段相等`（R4） | `dhampir-core`（单测） | GREEN（**这本身是证据**） |
| `两条字幕轨并存时配了底衬的那条轨仍然铺底衬` | `dhampir-worker/tests/text_plate.rs`（端到端） | **RED** |
端到端报错原文：`两条字幕轨并存时底衬没画出来：红色像素只有 0（该 ≥ 500）—— 轨道级样式被后面那条轨覆盖了（task-18 的原症状）`。⚠️ 变异自己也要验：第一版变异不分轨、把什么都弄红，证明不了判据有分辨力；改成按轨区间覆盖后两条判据的差异才显出来。
## 6. 顺带修掉的判据缺陷
`单条字幕轨的样式与改动前逐字段相等` 第一版把 `stroke_color` 默认写成 `[0,0,0,255]`（想当然），实际是 `subtitle_stroke_color()` 的 `[0x40,0x3c,0x3b,255]`；已按真实默认值改。
## 7. 怎么跑端到端（三条坑）
```bash
# 在仓库根执行
mkdir -p .workbuddy-tmp && cargo test -p dhampir-worker --test text_plate --no-run
T="$PWD/.workbuddy-tmp" TMPDIR="$T" TEMP="$T" TMP="$T" ./target/debug/deps/text_plate-*.exe --ignored
```
1. **`TEMP` 不能给 `cargo`** —— 链接器的 `lnk*.tmp` 也会指过去，`link.exe` 报 `LNK1104: 无法打开文件` 失败；所以先 `--no-run`、再单跑二进制。
2. **「跳过」≠「绿」**：不给 TMPDIR 时用例**静默跳过**并打印 `ok … finished in 0.00s` —— 判据看**耗时**（真的跑了是几秒，跳过是 0.00s）+ 有没有那行「跳过」。
3. **陈旧二进制副本**：`target/debug/dhampir/bin/dhampir.exe`（发布包形状）在 `find_binary()` 里**优先于** `target/debug/dhampir.exe`，旧的 ⇒ 新编译的底座根本不会被用到，症状是「改了没效果、还不报错」（本机这次不存在这个副本）。
## 9. 同一个病根的第二处：弹幕轨（已一并修掉）
弹幕走一模一样的形状：`danmaku_style = TextStyle { … }` 在循环里每轨整份重写、循环后只留最后一份；`grep -c "TrackKind::Danmaku"` 在测试文件里也只有 **2**。

| 弹幕字段 | 两条弹幕轨并存时 | 原因 |
|---|---|---|
| `color` / `font_ratio` | **不受影响** | 逐条字段，覆盖**之前**已 push 进 `DanmakuTextItem` |
| `outline` / `stroke_px` / `stroke_color` / `font_family` / `font_weight` | **被吃掉** | **只**在帧级 `danmaku_style` 里读 |
可达性已核：`dhampir-worker/src/text_overlay.rs` 画弹幕时把 `&overlay.danmaku_style` 传给每一条。真机复现（数墨像素）：单轨（轨 A 带描边）**246**，双轨（轨 A + 排在后面、不带描边的轨 B）**2**（123 倍塌陷），修好后回 246。
修法同字幕：`DanmakuTextItem` 加 `track_style: TextStyle`、帧级 `danmaku_style` 删除；变异自证改回共享槽 ⇒ 双轨 2 vs 单轨 246（**RED**）。判据踩坑：数纯绿（`stroke_color`）单轨对照也是 0（`text_raster.rs` 已知边界：libass 填充与描边共用掩码）⇒ 改量**墨迹面积**；两项相加也不行（同泳道重叠）⇒ 改成「双轨 vs 单轨同量级」。⚠️ 未碰两条有意为之的设计：弹幕恒传 `scale = 1.0`、`shake = 0.0`。
## 10. 验收
- `cargo check -p dhampir-core -p dhampir-worker` → **0 warnings / 0 errors**
- `cargo test -p dhampir-core --lib` → **252 passed / 0 failed**；`-p dhampir-timeline --lib` → **336**；`-p dhampir-worker --lib` → **134**
- 端到端 `text_plate --ignored` → **4 passed / 0 failed**（真起 ffmpeg + GPU）
