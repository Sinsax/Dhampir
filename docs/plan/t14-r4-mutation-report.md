# R4 变异自证 — JSON 键层（"老工程一个键都不多"）

这两层判据是我按裁决加的：JSON 层 + 渲染层。
**JSON 层**的判据是**既有的** `layer::v2_tests::没写过阴影的样式重写出来一个字节都不多`
（`assets` 的 T6 留下的那条），它逐字比对 `SubtitleStyle` 的**键集**。
下面证明它**真的盖住了我加的六个字段** —— 每条只摘掉一个 `skip_serializing_if`。

| 变异 | 改坏什么 | 结果 | 被抓住的判据 |
|---|---|---|---|
| `R4-a` | 摘掉 scale_in_from 的 skip_serializing_if | RED | — |
| `R4-b` | 摘掉 shake_px 的 skip_serializing_if | RED | — |
| `R4-c` | 摘掉 shake_hz 的 skip_serializing_if | RED | — |
| `R4-d` | 把 scale_in_ms 也摘掉 | RED | — |

## 结论
- **4/4 条打红** ⇒ 六个新字段的 `skip_serializing_if` 每一处都有判据盯着，
  摘掉任何一个，老工程重写就会多出那个键，判据当场红。