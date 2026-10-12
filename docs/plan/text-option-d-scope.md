# 文字形变：「倾斜 + 非等比压缩」（T1 契约层）

> 状态：**契约层已落地**（T1）。求值层打通（可动画）**尚未做** —— 见 §4。
> 改动：`dhampir-timeline` 的 `TransformV2`、`dhampir-core` 的 `TextItem`。

## 1. 这一版加了什么

在此之前，契约层能表达的**文字形变**只有两种：

| 能力 | 字段 | 层次 |
|---|---|---|
| 位移 | `TextItem.dx_px` / `dy_px` | 条级 |
| 等比缩放 | `TextItem.scale` | 条级 |

缺的两个自由度：**旋转**与**非等比拉伸**。于是"倾斜的文字""压扁的文字"
在契约里**根本没有位置** —— 下游只能用等比缩放近似，压扁是表达不出来的。

本版补上：

| 能力 | 契约层字段 | 条级字段 |
|---|---|---|
| 旋转 | `TransformV2::rotation`（已有） | `TextItem::rotation_deg`（新增） |
| 非等比缩放 | `TransformV2::scale_y`（新增） | `TextItem::scale_y`（新增） |

## 2. ★ 下游写 JSON 的坐标（T3/T4 看这条）

要倾斜 / 压扁**一段文字**，写在**文字图层自己的 transform** 上：

```jsonc
{
  "tracks": [{
    "kind": "subtitle",
    "subtitle": { /* 轨道级样式：字体、描边、底衬…… */ },
    "layers": [{
      "id": "sub-1",
      "transform": {
        "x": 0.0, "y": 0.0,
        "scale": 1.0,
        "rotation": 12.0,   // ← 角度制，正为顺时针（倾斜）
        "scale_y": 0.6      // ← 纵向额外倍数（压扁）；不写 = 1.0 = 等比
      }
    }]
  }]
}
```

⚠️ **写进 `TrackV2.subtitle`（轨道级样式）不会生效。**
`evaluate_overlay` 读的是 **element 的 `transform`**（`overlay.rs` 里
`let Some(element) = track.layers.iter().find(...)` 之后取 `element.transform`），
**不是**轨道级 `SubtitleStyle`。

⚠️ 同理，**别往 `TextStyle` 加同名字段**：`dx_px` 的源是轨道级 `TextStyle.shake_*`，
而本版两个字段的源是**元素 transform** —— 来源不同，往 `TextStyle` 加会变成**第二份真相**。

### 语义（两个量各管一件事）

* `scale` = **横向**缩放；`scale_y` 缺省时它同时管纵向 ⇒ **等比**（= 本版之前的行为）；
* `scale_y` = **纵向的额外倍数**，**乘在 `scale` 之上** ⇒ 最终纵向倍数 = `scale × scale_y`。

之所以是"倍数"而不是"纵向绝对值"：**缺省 1.0 恰好等于老语义**。
做成绝对值的话，缺省只能靠哨兵值（`0`？`NaN`？）表达，而哨兵值是第二份真相。

⇒ **非等比压缩** = `scale != scale_y`；相等时走的就是老工程那条路，**一个像素都不差**。

## 3. 缺省一个键都不许出现

两个新字段都带 `#[serde(default = "one", skip_serializing_if = "is_one_f32")]`
（`rotation_deg` 在 `TextItem` 上是内存结构、不参与序列化，不受此约束）。

复用仓内**既有**的一对 helper（`layer.rs` 的 `one` / `is_one_f32`，与
`SubtitleStyle::anchor_bottom` 同一对），**没有新造**。

判据（单测钉死，见 §5）：不写这两个键时，序列化出来的 JSON **里没有这两个键**——
否则每个老工程的变换键集都变大，R4 判据当场红。

## 4. ⚠️ 关键帧**尚未连接**（本版已知限制）

`TextItem::rotation_deg` / `scale_y` 填的是 **`element.transform` 的静态值**。

**`evaluate_overlay` 不求值关键帧** —— 它是纯函数，不读 `keyframes`，
连 `local_frame` 都没有。关键帧求值目前在**另一条路**上：
`compose.rs::element_to_draw` 的

```rust
rotation_deg: animated(element.transform.rotation, "rotation"),
```

⇒ **给文字元素写 `rotation` 关键帧，`TextItem` 不会跟着动。**

### 为什么本版不做

要做就得让 `evaluate_overlay` 也求值一次，或把已求值的 transform 传进来 ——
两种做法都会引入**第二处关键帧求值**，而求值逻辑现在唯一在 `compose.rs`。
两份实现必然漂移，那正是本仓最恨的形状。

而且这会改 `evaluate_overlay` 的签名，牵连 5 个调用点
（`dhampir-wasm/src/timeline_host.rs:976/1737`、
`dhampir-worker/src/bin/dhampir.rs:2133`、`dhampir-worker/src/pipeline.rs:2205/2423`）——
那是**另一件事**的范围。

### `TRANSFORM_TARGETS` 为什么没加 `scale_y`

`schema.rs` 的 `TRANSFORM_TARGETS = ["opacity","x","y","scale","rotation"]` **保持 5 项**。

加了 `scale_y` 就等于宣告"它可被关键帧驱动"，那就**必须有人求值** ——
而按本节，`evaluate_overlay` 不求值 ⇒ 加了是个**死字段**，
用户写了关键帧会被**静默忽略**，比不加更坏。

⇒ **等真做"可动画"时一起加**（求值打通 + 进表，同一件事的两半）。

## 5. 判据

```bash
cargo test --workspace
```

钉住的单测（名字 + 断言值）：

### 契约层（`dhampir-timeline`，5 条）

| 单测 | 断言 |
|---|---|
| `缺省_scale_y_不写进_json` | `to_string(TransformV2::default())` 的结果**不含** `"scale_y"`，而 `x`/`y`/`scale`/`rotation` 四个键**都在** |
| `非缺省的_scale_y_要写进_json` | `scale_y = 0.6` ⇒ JSON 含 `"scale_y":0.6` |
| `老工程没有_scale_y_键时按_1_0_补齐` | `{"x":0,"y":0,"scale":1.5,"rotation":0}` ⇒ `scale_y == 1.0`、`scale == 1.5` |
| `default_与_serde_缺省值一致` | `TransformV2::default().scale_y == 1.0 == one()`，且反序列化缺省与之一致 |
| `v1_迁移过来的_scale_y_是_1_0` | v1 → v2 后 `scale_y == 1.0`、`scale == 1.5`、`rotation == 90.0` |

### 求值层（`dhampir-core`，5 条）

| 单测 | 断言 |
|---|---|
| `文字条带旋转角度` | 元素写 `rotation = 12.0` ⇒ 每条 `TextItem.rotation_deg == 12.0` |
| `文字条带纵向倍数` | 元素写 `scale_y = 0.6` ⇒ 每条 `TextItem.scale_y == 0.6` |
| `没写旋转与纵向倍数时是老行为` | 老夹具（无这两个键）⇒ 恒 `0.0` / `1.0` |
| `两条字幕轨各自的形变互不污染` | 两条字幕轨并存：10° 与 30° **都在**（`len == 2`），`1.0` 与 `0.5` 也都在 |
| `关键帧尚未连接_条目取的是静态值` | 元素有 `rotation` 关键帧（5°→45°），第 60 帧仍取**静态值 `5.0`** |

⚠️ 最后一条钉的是 §4 那条**已知限制**。哪天求值层打通了它**会红** ——
那时应该红：它提醒你去更新 `TextItem::rotation_deg` 的注释与本文件，**不是**删掉它。

### 变异自证（都真跑过，都翻红）

| 变异 | 翻红的判据 |
|---|---|
| `TextItem` 改填常量（0.0 / 1.0），切断透传 | `文字条带旋转角度` + `文字条带纵向倍数` + `两条字幕轨各自的形变互不污染` |
| 去掉 `scale_y` 的 `skip_serializing_if` | `缺省_scale_y_不写进_json`（R4 键纪律） |
| v1 → v2 迁移改给 `0.0` | `v1_迁移过来的_scale_y_是_1_0` |

## 6. 契约版本

`host_api` **未动**：本版没有改 `TransformView`（`host_api.rs`）也没有改 `Layer`
（`schema.rs`）——那两处各自的注释都写着"改名属于破坏性改动，不在这里顺手做"，
加字段同理。**下游 API 的形状一个字节都没变。**

## 7. T5：形变真的落进像素（真机出片验证）

§5 那些判据证明的是"值到达了求值层 / 函数算得对 / 预览公式与成片一致"。
**它们合起来仍不排除"出片里文字一动不动"** —— `text_enter.rs` 头部记的
2026-10-11 那次漏检就是这个形状：函数全对、各层判据全绿、屏幕上没变化。

判据在 **`crates/dhampir-worker/tests/text_tilt.rs`**（真 GPU，640×360）。
量的是**墨迹的包围盒 + 二阶中心矩**，不是函数返回值：

| 量 | 怎么算 | 为什么是它 |
|---|---|---|
| 主轴倾角 | `θ = ½·atan2(2μ11, μ20−μ02)` | **方向性**判据 —— 包围盒大小对倾斜与压扁都敏感，倾角只对旋转敏感 |
| 包围盒宽度比 | `w_变 / w_恒` | 非等比的**唯一**判据 —— 否则分不出"压扁"与"整条等比缩小"（那是 `scale`） |

### 实测量

| 用例 | 设定 | 实测 |
|---|---|---|
| 倾斜 | `rotation = 12.0` | 反解 **12.52°**（恒等基线 0.58°）；包围盒 158×20 → 153×45 |
| 压扁 | `scale_y = 0.5` | 高度比 **0.550**；宽度比 **1.000**；倾角 0.58° → 0.35° |
| 叠加 | `10° + 0.6` | 对照**纯倾斜**档：高度 40 → 24（比 0.600）；宽度 155 → 155 |
| 恒等 | 显式 `scale_y: 1.0` vs 老工程（无该键） | **921600 字节逐字节相同** |
| 范围 | `12°` | 文字带外改动像素 = **0** |

### 变异自证

把 `blit_affine` 里的 `transform` 抹成恒等（模拟"接线漏了"）⇒
倾斜 / 压扁 / 叠加**三条当场红**，另外两条（逐字节相同、范围）不受影响仍绿
—— 证明它们真在测**接线**，不是测别的东西。

### ⚠️ `TMP` 不是可选的

Windows 上 Rust 的 `std::env::temp_dir()` 读 **`TMP`**，**忽略 `TMPDIR` 与 `TEMP`**。
给错了**不报错**，而是走"跳过"分支报 **`0 passed` 且 `0.00s`** ——
一条都没跑却看着像绿的。真跑是秒级。

```text
TMP=F:\para\Code\Dhampir\.workbuddy-tmp \
cargo test -p dhampir-worker --test text_tilt -- --ignored
```

### 预览 ↔ 成片：真浏览器矩阵比对

`check-text-tilt.mjs` 比的是预览源码里的**公式**。这里补**实测**那一半：
用 CDP 让真 Chrome 把 `engine.js::textTiltTransform` 拼出的 CSS 折成矩阵，
再与出片侧正向 `R(rot) · diag(scale·scale_x, scale·scale_y)` 逐元素比。

| 用例 | 最大逐元素差 |
|---|---|
| 7 组（含 `tilt=0`、`squash=1`、`scale≠1` 各档） | **4.8e-7**（float32 舍入） |
| 反向对照：写反成 `scale() rotate()`（= `S·R`） | **3.9e-1**（差 6 个数量级） |

⇒ 预览与成片在**浏览器实际计算出的矩阵**这一层也对上了；顺序写反是 **0.39 量级**的偏差，
肉眼可见（斜着的字被沿**旋转后的轴**拉伸），不是"理论上的差别"。

✅ **`.hl` 逐段那条路已验**（2026-10-13，`t5_hl_逐段一样有形变`）—— 本条原先记为「未验」。

它确实是**另一条路**：不带高亮走 `paint_lines`（整行一次画完），
带 `<span class="hl">` 走 `paint_parts`（**逐段各栅格化一次**、各自算行内偏移 `offset_x`、各自上色）。
两条路共用同一份 `blit_affine` 与同一个 `transform`，**但 `.hl` 自己算落点**。

实测（真 GPU，640×360，带 `<span class="hl">` 的问句）：

| 量什么 | 恒等 | 倾斜 12° | 压扁 `scale_y=0.5` |
|---|---|---|---|
| 主轴倾角 | **−0.02°** | **12.05°** | — |
| 包围盒 | 80×20 | — | 79×**11** |

⇒ 倾斜在逐段那条路上**照样**落进像素（12.05° 对设定 12°），
压扁**只改高度**（20→11）、宽度不动（80→79）—— 与整行那条同款结论。

★ **变异自证**（这条判据为什么值得存在）：把 `paint_parts` 里那次 `blit_affine`
的 `transform` 换成 `TextTransform::default()` ⇒
**只有这一条红、其余 5 条全绿**。即它覆盖的是别的用例**看不见**的那条路。
（原先担心的"`.hl` 落点计算有既有残差"**没有出现**：宽度 80→79 的 1px 差
是双线性重采样的边缘效应，不是落点漂移 —— 落点若偏，倾角不会精确落在 12°。）

### 两条被实测纠正的**判据**错误（不是实现错误）

1. 老工程夹具省了 `rotation` ⇒ 反序列化失败：`rotation` **没有** `serde(default)`
   （`layer.rs:154`），只有 `scale_y` 有 ⇒ 正确形状是"**有 `rotation`、无 `scale_y`**"。
2. 叠加档第一版拿**填充率**当"压扁也生效"的判据，**方向错了**：
   倾斜把包围盒面积 ×3.9，压扁只把墨迹 ×0.6 ⇒ 填充率必然**下降**（实测 0.32 → 0.19）。
   正确做法是拿**纯倾斜**当对照判高度 —— 两档旋转相同，高度差只可能来自压扁。
