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
