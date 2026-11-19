# 文字动效的逐帧通道 —— 宿主接入指南（T1 弹入 / T3 强调 / T4 抖动）

**适用产物**：`dhampir-0.3.0+`（`project_schema = 1`、`host_api >= 6`）。写哪些键、值口径、不写会怎样，就这一份；形状权威 [host-api.md](host-api.md)，设计见 [../plan/](../plan/)。
## 一、这一版变了什么
文字的逐帧动效从宿主搬进底座：宿主只写动效参数，底座按帧号算 —— 预览（浏览器）与成片（CLI）同一份计算。

| 面 | 变没变 |
|---|---|
| 工程文件形状（`project_schema = 1`） | 老工程照读，不写就不出现 |
| `dhampir_host_api_version` | 该版未动（当时 6；现 7），这次是加字段 |
| 既有字段 | 全都不变 |
**判据**：只写老字段的工程出片逐字节相同 —— `text_enter::全部缺省时逐字节等于老工程`。
## 二、字段一览（契约）
| 键 | 类型 | 单位 | 默认（不写时） | 干什么 |
|---|---|---|---|---|
| `scale_in_from` | `f32` | 倍率 | `1.0` | 弹入起点缩放，参照 `0.7` |
| `scale_in_peak` | `f32` | 倍率 | `1.0` | 弹入曲线终点倍率，参照 `1.15`（不是实际峰值，见 §3.1） |
| `scale_in_ms` | `u64` | 毫秒 | `0` | 弹入窗口时长；**`0` = 关**（唯一开关） |
| `shake_px` | `f32` | **文档像素** | `0.0` | 抖动幅度；**`0` = 关** |
| `shake_hz` | `f32` | Hz | `18.0` | 抖动基频；**`<= 0` 视为关** |
| `shake_phase` | `f32` | 弧度 | `0.0` | 抖动相位；同屏多条给不同值 |

六个键都挂在字幕轨的 `subtitle` 对象上（与 `font_ratio` / `color` 同级），**全部可选**：不写 = 关。单位是**文档像素**（与 `rise_in_px` / `transform.x/y` 同一套），预览小画布与成片 1080p 落同一比例。
**参照值**：`{"subtitle":{"font_ratio":0.055,"scale_in_from":0.7,"scale_in_peak":1.15,"scale_in_ms":180,"shake_px":14.0,"shake_hz":18.0,"shake_phase":0.0}}`
## 三、三条容易踩的口径
### 3.1 `scale_in_peak` 是曲线端点，不是实际峰值
`scale(p) = from + (peak − from) × back_out3(p)`；`back_out3` 在 p=0.5 冲到 1.25 ⇒ 实际最大 = `0.7 + 0.45 × 1.25 = 1.2625`，不是 1.15（实测峰值帧宽比 ≈ 1.245）。改过冲要同时动两个端点。
### 3.2 曲线用 `s = 3`，不是经典的 1.70158
底座 `easing::EasingForm::BackOut` 写死 1.70158（峰值 1.10），文字弹入用参照实现的 `s = 3`（峰值 1.25），在 `dhampir_timeline::text_motion::back_out3` 单独算。宿主若自己实现曲线必须与它逐值一致。
### 3.3 抖动是纯 `t` 的函数
`text_shake_px(t, …)` 只吃当前时刻，没有累积状态（预览是 seek 的）；宿主不要自己推进抖动状态。同屏两条 `shake_phase` 相同会整体同拍，看着像画面在抖。
### 3.4 T3 强调：`cue.scale` 不在工程 JSON 里
| 位置 | 类型 | 说明 |
|---|---|---|
| 字幕轨 `subtitle.*` | 上面那 6 个键 | 轨道级，写进工程 JSON |
| 条目 `Cue.scale` | `Option<f32>`，`None` = 1.0 | 逐条，宿主在 `SubtitleTable` 里设 |

往 `layers[].scale` 里写**不会被读到**。最终字号倍率 = `line.scale × enter_scale × cue.scale`（相乘，不是覆盖）。
## 四、存量零回归
| 层 | 判据 |
|---|---|
| JSON 键 | `layer::v2_tests::没写过阴影的样式重写出来一个字节都不多` |
| 求值 | `text_motion::默认样式求值出来是单位变换` |
| 出片 | `text_enter::全部缺省时逐字节等于老工程` |

六个键都带 `#[serde(default, skip_serializing_if = "Option::is_none")]`：不写 ⇒ `None` ⇒ 单位变换、出片逐字节相同、重写工程不多出这些键；写 ⇒ 就是写的那个值，哪怕等于默认值。
## 五、宿主接入清单
1. 写 6 个键进字幕轨的 `subtitle`（参照值见 §二）。
2. 删掉宿主侧自己算的逐帧缩放 / 位移（两边都算会叠加两次，症状：动效幅度是两倍）。
3. 强调放大走 `Cue.scale`，不要往 `layers[].scale` 写。
4. 抖动相位逐条错开（条目序号 × 一个常数即可）。
5. 判据：只写老字段时出片逐字节相同；写新字段时预览与成片的逐帧外接框 / 质心一致。
## 六、顺手修掉的既有 bug
渲染侧字号原来只取轨道级 `font_ratio`（`place_line(rect, target, style.font_ratio)`），逐条 `scale` 只落在描边宽度上 ⇒ `line.scale` 与 T1/T3 都不动字号，wasm 与 CLI 还各读一处。已修：求值层给 `font_ratio` 乘上逐条缩放，两条路读同一个值。
**判据**：`text_enter::t1_弹入逐帧先缩后过冲再回落`（外接框先 `<1`、再 `>1`、后回 `1.0`）。
## 七、相关文件
| 东西 | 在哪 |
|---|---|
| 曲线 / 抖动相位表 / 包络求值 | `crates/dhampir-timeline/src/text_motion.rs` |
| 字段声明 | `crates/dhampir-timeline/src/layer.rs` 的 `SubtitleStyle` |
| 逐条 `Cue.scale` | `crates/dhampir-timeline/src/subtitle.rs` |
| 求值到 `TextItem` | `crates/dhampir-core/src/overlay.rs` |
| 出片端到端判据 | `crates/dhampir-worker/tests/text_enter.rs` |
