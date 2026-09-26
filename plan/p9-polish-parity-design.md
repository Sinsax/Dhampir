# P9 —— 承接 V-Trim polish：功能收集与底座化设计

> **这份文件是什么**：把 V-Trim `polish`（精修出片）所需的**全部功能**收齐，逐条对着
> Dhampir 现有契约分类，并给出**泛用化之后**的底座设计。
>
> **它不是什么**：不是 V-Trim 的移植计划。V-Trim 的 `polish.toml ↔ 本工程文件` 的**转译留在
> V-Trim 侧**（见 §7），本仓只管"把工程文件这条路做成能承接得下所有需求的样子"。
>
> **上游**：用户决策 2026-09（本项目立项即为替代 V-Trim 的 wgpu 渲染链路）。
> **改决策要改这份文件，不能只在代码里改。**

---

## 1. 目标与两条硬约束

**目标**：本仓的工程文件要**简洁、丰富、格式整齐**，且**泛用**——
后期要加复合特效、新的运镜、新的图层类型时，**不必改渲染主路径**。

**约束一：契约纯净**（已有铁律，不许破坏）。`dhampir-core` 零 `#[cfg]`；WGSL 只有一份，
两个宿主读同一份文本；采样一律 `textureLoad([wgsl-portable-subset.md](./wgsl-portable-subset.md))`。

**约束二：特效是"类型 + 参数"的声明式数据，不是代码**（README 铁律 3）。
P9 的全部设计都服从这一条 —— V-Trim 那边是 `Event` 枚举 15 个变体 + `EffectUniform`
一个**定长 struct**（`vtrim-render-core/src/effects.rs:14-28`），加一个特效要同时改
枚举、改 enum 的 4 个方法、改 uniform 结构、改着色器。**那正是要摆脱的形状。**

---

## 2. V-Trim polish 功能全表（从源码收齐）

来源：`vtrim-render-core/src/types.rs`（`Event` 15 变体 + `StyleRoot`/`Layout`）、
`vtrim-polish/src/project/config.rs`（`ProjectFile` 15 个 section）、
`templates/index.html`（14 个 handler）、`render/native/mod.rs`（FramePipeline）。

### 2.1 视觉事件（`Event`，15 个变体）

| # | V-Trim 变体 | 语义 | 起止 | Dhampir 现状 |
|---|---|---|---|---|
| 1 | `Camera` | 运镜：`中/近/远` 三档 + 聚焦点 `origin` + `anchor→anchor_to` 移动 + `cut` 硬切 + `handheld` 手持摇摆 | **持续**（跟到下一条） | ⚠️ 地基有（`Transform`+`Keyframe`），缺"keyframe 驱动 transform" |
| 2 | `Sticker` | 贴纸：入/浮/出三段缓动 + `manual` 时间锁 | 持续 | ⚠️ `AssetKind::Image` 有（静态），**GIF 动图无** |
| 3 | `Shake` | 抖动 | 瞬时 ~0.3s | ❌ 无 |
| 4 | `Flash` | 闪白（可带色） | 瞬时 0.25s | ❌ 无 |
| 5 | `Blur` | 模糊脉冲 `blur(8px)→0` | 瞬时 0.3s | ✅ **有** `gaussian_blur` |
| 6 | `ZoomBounce` | 缩放弹跳 1.10 | 瞬时 0.4s | ❌ 无（`scale` 机制有） |
| 7 | `Vignette` | 暗角 | 瞬时 0.4s | ❌ 无（V-Trim 自己也缺画） |
| 8 | `HueShift` | 色相叠加 | 瞬时 0.35s | ⚠️ 有 `hue`，但语义不同（旋转 vs 叠色） |
| 9 | `Stutter` | 卡顿（时间轴跳帧） | 瞬时 0.25s | ❌ 无（**时间轴层**，非像素） |
| 10 | `Split` | 分屏：`scale/skew_x/x` | 瞬时 0.25s | ❌ 无 |
| 11 | `Noise` | 噪声 | 瞬时 0.3s | ❌ 无 |
| 12 | `Pulse` | 脉冲 | 瞬时 0.4s | ❌ 无 |
| 13 | `ColorShift` | 色调偏移（sepia+saturate） | 瞬时 0.35s | ⚠️ 有 `saturation`，**无 sepia** |
| 14 | `Overlay` | 覆盖层：纯色/渐变（linear/radial）+ 4 stop | 持续 1.5s | ⚠️ 有渐变着色器，**不在注册表** |
| 15 | `Sfx` | 纯音效（无视觉） | 按文件长度 | ❌ 无（音频只拼轨） |

### 2.2 样式 / 布局 / 画布

| 组 | 字段（实测） | Dhampir 现状 |
|---|---|---|
| `SubtitleStyle` | font / size / color / stroke / highlight_color / entrance / position | ⚠️ 部分（`font_ratio`/`color`/`outline`/`max_lines`；**无 stroke/highlight/entrance**） |
| `DanmakuStyle` | font_size / color / stroke / highlight_color / speed / area / scroll_direction | ⚠️ 部分（`lanes`/`duration_ms`/`font_ratio`；**无 highlight/direction**） |
| `CameraStyle` | `zoom_mid=1.3` / `zoom_close=1.5` / `zoom_wide=0.55`（**只有这一张表**） | ❌ 无（但 `scale` 表达得了） |
| `Layout` | liver / danmaku_area / sticker_area / reaction_zone / game_area，每个 = 九宫格 `grid` 或自由选框 `top_left`+`bottom_right` | ❌ 无（**这是 V-Trim 特有业务**） |
| `CanvasConfig` + `StageConfig` | 方向 → 宽高；主画面位置；背景 `none/black/color/blur`；`blur_px`/`blur_img_scale`/`scale`/`padding` | ✅ `RenderHints{width,height,format}` 够 |
| `CoverConfig` | 20+ 字段：帧位置 / 标题断行 / 字体 / 描边 / 渐变方向 / 暗角 / 阴影 / `CoverEffect[]` | ✅ **`dhampir frame` 就是截帧** + 上述特效齐了即可 |
| `MuteRange` / `silence_ranges` | 屏蔽区间（去静音） | ⚪ **不属底座**（业务对时间轴的计算，见 §7） |
| `SfxConfig` | volume / blacklist / reactions_enabled | ❌ 无 |
| `FilterConfig` | danmaku/highlight 关键词 | ⚪ **不属底座**（LLM 业务） |
| `StickerConfig` + `sticker_source` | 落点策略 + 来源 | ⚠️ 落点 = `Transform`，够 |

### 2.3 复合结构（V-Trim 有、Dhampir 要能表达）

- **同帧多事件叠加**：一条时间轴上 blur + shake + flash + vignette 同时生效 ——
  所以特效**不能挤进一个定长 uniform**。
- **调整图层**：Dhampir 已有（`Layer::is_adjustment`，`layer.rs:198`）。
- **运镜是持续状态机**：跟到下一条运镜，不是一次性 —— 这正是 `Keyframe` 的形态。

---

## 3. 设计原则（从 V-Trim 的坑里倒推出来的四条）

1. **特效 = 数据行，不是枚举变体。** 每行 `{kind, params{...}, window, space, blend}`。
   加一个特效 = 注册表加一条 + 加一个管线 + 加一份 WGSL；**渲染主路径一行不改**。
2. **参数是 map 不是 struct。** V-Trim 用 `EffectUniform`（定长、字段写死、注释里
   编号复用 `misc.xyz`）。改成"**一个参数缓冲区 + 按注册表布局**"，参数个数不受限。
3. **时间窗只有两种**：`persistent`（跟到下一条/到 end）与 `transient`（有默认时长与上限）。
   V-Trim 靠 `Event::default_end()` 一个 15 分支的 match 表达 —— 那也是一张要泛用的表。
4. **两端同一份求值**：窗口函数**纯 `t` 的函数、无累积状态**（V-Trim 的 `handheld_sway`
   就是这么写的，注释说明"跳帧/并行/seek 结果一样"）。本仓照此办理。

---

## 4. 泛用化设计：特效与时间窗

### 4.1 特效条目（契约层）

⚠️ **先记一条已核实的现状**：`Effect` **已经是** `{kind: String, params: BTreeMap<String, f32>}`
（`schema.rs:180-186`），`space` 也已经是 `EffectSpec` 的字段。**§5.2 里"改成 map"这条不必做** ——
这个泛用形状本仓已经有了。P9 要加的只是**条目上的时间窗与强度**：

```jsonc
{ "kind": "gaussian_blur",
  "params": { "radius": 8.0 },       // ✅ 已存在
  "window": { "kind": "transient", "attack": 2, "hold": 4, "release": 6 },  // 新增
  "opacity": 1.0 }                    // 新增：混合强度，0 = 不生效（可被 keyframe 驱动）
```

**关键点**：
- `params` 是 **`BTreeMap<String, f32>`**（**已经是**）→ 参数个数不受限，
  序列化**逐字节稳定**（与 `Recorded.tags` 同一个理由，`layer.rs:141`）。
  代价：**参数只能是 `f32`**。颜色、枚举、字符串**进不去** —— 见下面这条。
- ⚠️ **`params: BTreeMap<String, f32>` 装不下颜色**：V-Trim 的 Flash / HueShift /
  ColorShift / Overlay 都要颜色。**建议**：颜色拆成 4 个 `f32` 通道键
  （`color_r/g/b/a`，0~1），或给 `Effect` 加一个平行的
  `strings: BTreeMap<String, String>`。**这是 P9 要决策的第一件事**，
  倾向后者（一个字段一个含义，不靠键名约定）。
- `window` 抽出**两种通用形态**，覆盖 V-Trim 15 个变体的全部时长语义：
  - `transient`：`attack/hold/release`（帧数）→ 一条**纯函数**包络；
  - `persistent`：`{kind:"persistent", fade_in, fade_out}` → 吃到 `end` 为止。
- `space` 复用已存在的 `EffectSpace`（`schema.rs:270`）—— **它是已决策的字段，不要重造**。
- `seed` 只给噪声类用；**确定性**，与帧号一起进 hash（不许用系统随机）。

### 4.2 注册表扩展（`dhampir-core/src/effects.rs`）

现有 `EffectSpec{kind, params, space, pipeline}` 是好形状，只扩两处：

```rust
pub struct EffectSpec {
    pub kind: &'static str,
    pub params: &'static [ParamSpec],   // 从 (name,min,max) 扩成带默认值
    pub space: EffectSpace,
    pub pipeline: EffectPipeline,
    pub window_default: WindowDefault,  // 新增：transient 的默认时长（V-Trim 那张表）
}
```

`EffectPipeline` 枚举**加 4 个**（每个都是有独立数学性质的**算子族**，不是单个特效）：

| pipeline | 性质 | 覆盖的 V-Trim 特效 |
|---|---|---|
| `ColorAdjust`（已有） | 逐像素 | HueShift / ColorShift |
| `SeparableBlur`（已有） | 邻域·两趟 | Blur |
| `ColorMask`（新） | 逐像素 + 常量色 | Flash / Vignette / Noise / Overlay |
| `Warp`（新） | 邻域·坐标重映射 | Shake / ZoomBounce / Pulse / Split /（Camera） |
| `Composite`（新） | 读目标像素（ping-pong） | Overlay 的混合、将来 blend 全 9 种 |

**注意**：`Stutter` 不是像素特效 —— 它是**时间轴**上的行为（某几帧复制/跳），
落在 render plan 层，不进 `EffectPipeline`。这条要写进契约，免得以后有人硬塞。

### 4.3 运镜泛用化（不新造概念）

运镜**就是 `Transform` 上的关键帧**，不引入 `Camera` 这种"持续事件"新类型：

```jsonc
{ "id": "cam1", "start": 0, "end": 900, "source": null,
  "effects": [],
  "transform": { "x": 0.0, "y": 0.0, "scale": 1.3, "rotation": 0.0 },
  "keyframes": [                          // 现有字段，现在只驱动 opacity
    { "frame": 0,   "target": "scale", "value": 1.0,  "easing": "ease_in_out" },
    { "frame": 120, "target": "scale", "value": 1.3,  "easing": "linear" },
    { "frame": 120, "target": "x",     "value": -0.2, "easing": "linear" }
  ] }
```

**唯一的契约改动**：`Keyframe` 加一个 `target` 字段（现在是**隐式的"只作用于 opacity"**）。
这一点有源码佐证：求值函数就叫 **`opacity_from`**（`curve.rs:28`），签名
`(opacity, keyframes, local_frame) -> f32` —— 它**结构上只算一个标量**，
既不知道自己在驱动什么，也装不下第二条曲线。

这正是 `compose.rs:14` 那句注释预言的
「动画化特效参数留到 v2（那时 Keyframe 要加一个"作用于谁"的字段）」—— **P9 兑现它**。

因此 T8 的改动比"加个字段"多一步：`curve.rs` 从 `opacity_from` 泛化成

```rust
/// 按 target 取某个通道的关键帧曲线。没有该 target 的键就返回 fallback。
pub fn channel_from(fallback: f32, keyframes: &[Keyframe], target: &str, local: Frame) -> f32
```

**`opacity_from` 保留为它的特化**（`target = "opacity"`），于是 `compose.rs:89` 的
`pub use` 与 `edit.rs` 剃刀那处调用**一行不改**（`curve.rs:8` 说明剃刀也要这份求值）。
排序/边界/取后键缓动/`span<=0` 那四条语义**逐条照抄**，不许顺手"改进"。

`target` 取值：`opacity` | `x` | `y` | `scale` | `rotation` | `effect.<index>.<param>`。
最后一种让**特效参数本身可被动画化**，复合特效就自然出来了。

- V-Trim 的 `zoom_mid/close/wide` 三档 = 三条 `scale` keyframe，**不需要 CameraStyle**。
- `handheld` = 一条**内建生成器**（数据上是一个带 `seed` 的 `Warp` 特效，或保存为
  关键帧序列）。**不写进契约**：它是观感，不是契约概念。
- `cut` 硬切 = `easing: "linear"` + 两个 keyframe 同帧。

### 4.4 复合特效：为什么这样设计就"更好接"

用户的目标是"**后期复合特效或其他效果能实现得更方便**"。泛用化之后，
复合有**三条现成的组合路径**，都不需要新概念：

| 复合方式 | 怎么做 | 例子 |
|---|---|---|
| **同层叠加** | 一个 `Layer.effects` 里放多条，按 pipeline 排序执行 | 模糊 + 闪白 + 暗角 |
| **参数动画** | keyframe 的 `target = "effect.0.radius"` | 模糊从 0 涨到 8 再回落（= V-Trim 的 Blur 事件） |
| **分层/调整图层的嵌套** | 已有 `Step::Draw` / `Step::Adjust` 交错 | 只对下半屏做特效：画 → 调整图层 → 再画 |

**关键**：这三条**今天就已经是数据形状**，泛用化之后只是"可用的特效变多"，
而**组合机制不用再设计**。这正是"泛用底座"相对于"15 个枚举变体"的差别 ——
V-Trim 那边每加一种组合都要新写一个 `Event` 变体。

---

## 5. 落地形状：工程文件怎么变

**目标（用户要求）：简洁、丰富、格式整齐。**

### 5.1 保持不动的部分（已经是好形状）

`ProjectDoc{project_schema, generator, meta, assets, timeline, view, render_hints, extensions}`
—— **不动**。`timeline` **原样内嵌**（不拍平），这是已决策的。

### 5.2 新增/扩展（全部向后兼容）

| 位置 | 改动 | 理由 |
|---|---|---|
| `Layer.effects` | 条目加 `window` + `opacity`（`kind`/`params` **已有，不动**） | 泛用 |
| `Keyframe.target` | 新增字段，缺省 `"opacity"` | 兑现预留；**老文件逐字节不变** |
| `EffectPipeline` | 加 `ColorMask`/`Warp`/`Composite` | 派发不改主路径 |
| `EffectSpec.window_default` | 新增 | 替代 V-Trim 的 15 分支 match |
| `AssetKind` | **已有 `Image`**；补 `ImageSequence`（GIF/APNG/WebP 动图） | 贴纸动图 |
| `Asset` | 已有 `frame_count`/`timebase`/`width`/`height` —— 动图直接复用 | **不必新字段** |
| `Effect` | 颜色类特效要能带颜色（见 §4.1 的注） | Flash/HueShift/Overlay |
| `Layer` | 加 `speed: f32 = 1.0`（倒放/变速） | 顺带；与"素材内帧"同一套换算 |
| `AudioTrack` | 加 `sfx` 子表（`name`/`asset_id`/`gain`/`window`） | SFX 混音 |

### 5.3 "格式整齐"的具体做法

1. **参数名一致**：幅度类一律 `amount`，半径类一律 `radius`，颜色一律 `color`（RGBA 数组）。
2. **绝不复用字段槽**。V-Trim 的 `misc: [_, vignette, noise, grad_stops.len()]` 是反例
   （注释里靠编号记用途）——本仓**一个字段一个含义**，宁可多一层结构。
3. **时间单位**：进度用**帧**（整数，铁律 1）；素材内偏移用**帧**；样式比例用 **0~1 浮点**
   （`font_ratio` 已是此形）；**秒只出现在 `assets[].timebase` 与宿主的换算边界**。
4. **可省略的默认值**：`skip_serializing_if` —— 老工程不会被重写成噪音
   （V-Trim 的 `is_false` 就是这个动机，`types.rs:274`）。
5. **`BTreeMap` 而非 `HashMap`** 用于任何进序列化的 map → 逐字节稳定。

---

## 6. 渲染侧设计（怎么做到"加特效不改主路径"）

### 6.1 求值 → 计划 → 提交（三段，与 V-Trim 的 `FrameState` 对齐但泛用）

```
evaluate(layer, frame)
  → 每个 effect 条目求出一个 ResolvedEffect {kind, params[], opacity, pipeline, space}
  → 按 pipeline 归组：ColorAdjust/ColorMask 合成一趟，Warp 一趟，blur 两趟
  → 生成 RenderPlan（有序 pass 列表）
  → 宿主执行（native 离屏 / wasm surface）
```

**与现在的关系**：现状比"if/else 调 renderer"要更接近目标 ——
`render/timeline.rs` **已经有一个 `Step` 计划结构**（`Step::Adjust{effects,..}` 等），
而且**按 `spec.pipeline` 派发已经做了**（`:64`、`:166`，注释 `:54` 明说
"改成按 `spec.pipeline` 认，加特效就只需要在登记表里声明管线"）。

**真正卡住的地方是"顺序"**：`Step::Adjust` 的分支体里，
色彩调整与模糊是**两段写死的代码块**（`:394-411` 与 `:414-439`），
顺序由注释 `:396-400` 手工论证并**定死在这里**。再加 `ColorMask`/`Warp` 两个 pipeline，
就要再写两段 —— **每加一个 pipeline 都要重排一次顺序论证**。这是 T9 的实质工作量：

> 把"两段写死"改成**按 pipeline 排序的 pass 列表**，
> 而"顺序"这条信息**从契约来**（条目上的 `order`，或由 pipeline 的算子性质推导）。

顺序不能靠猜：`:396-400` 那段论证（逐像素先、邻域后）是**正确性知识**，要搬进
一条可测的规则，而不是留在注释里。

### 6.2 WGSL 侧

新增 `color_mask.wgsl` / `warp.wgsl` / `composite.wgsl`，各**只有一份**，两个宿主共用。
**必须过子集闸**（`wgsl_subset.rs` 的 6 条禁词 + 允许表申报）——
新增构造要**同时**改 [wgsl-portable-subset.md](./wgsl-portable-subset.md) 的允许表，
两条命令都要重跑（那份文件 §1 写明了）。

`Warp` 要小心 **§5 高危区第 1 条**（`fract`/`pow` 极限行为）：坐标重映射容易踩 `fract`。

---

## 7. 边界：什么**不**进本仓

用户已定：`polish.toml ↔ 工程文件` 的**转译交给 V-Trim 那边**。据此，以下**明确不做**：

| 不进本仓 | 为什么 | 谁做 |
|---|---|---|
| `polish.toml`（TOML）解析 | 那是 V-Trim 的格式；本仓只有一种工程文件（JSON） | V-Trim：`polish.toml → dhampir ProjectDoc` 转译器 |
| `Layout` 九宫格 / 自由选框 | **V-Trim 特有的直播布局概念**，不是通用能力。转译时落成 `Transform` | V-Trim 转译器 |
| `FilterConfig` 关键词 | LLM 业务 | V-Trim |
| `MuteRange` / 去静音 | 对时间轴的计算，与渲染无关 | V-Trim |
| `sticker_source` 来源策略 | 素材挑选业务 | V-Trim |
| `CoverConfig` 的 20 个字段 | 封面是"截帧 + 上述特效"的组合；**字段是 V-Trim 的排版预设** | V-Trim 转译成一次 `frame` 调用 + 特效条目 |
| `CameraStyle` 三档倍率 | 转译成 `scale` keyframe | V-Trim |

**判断准则**：**凡是"这个直播间怎么排版"的，归 V-Trim；凡是"像素怎么算出来"的，归本仓。**

---

## 8. 分阶段（对齐既有 T 段编号习惯，从 T8 起）

| 段 | 做什么 | 验收 | 依赖 |
|---|---|---|---|
| ~~**T8**~~ | ✅ **已完成**（2026-09 落地）：`Keyframe.target` + `curve.rs` 的 `opacity_from` 泛化成 `channel_from`；`Transform` 的 x/y/scale/rotation 与 `effect.<i>.<param>` 都由关键帧驱动 | 573 测试全绿（+26）、20/20 守卫全绿、`target?` 可选故老工程逐字节不变 | — |
| **T9** | `Step::Adjust` 从"色彩+模糊两段写死"改成**按 pipeline 排序的 pass 列表**；条目加 `window`/`opacity`；`EffectSpec.window_default` | 顺序规则**可测**（不是注释）；加一个 pipeline 不改主路径；window 纯函数（同 `t` 同结果，跳帧可复现） | T8 |
| **T10** | `ColorMask` 管线：Flash / Vignette / Noise / Overlay（含 radial 渐变） | 每加一个特效**不改渲染主路径**（由守卫钉住）；WGSL 过子集闸 | T9 |
| **T11** | `Warp` 管线：Shake / ZoomBounce / Pulse / Split | 同上；Warp 的 `fract` 高危区有极限输入用例 | T10 |
| **T12** | `ImageSequence` 素材（GIF/APNG 动图贴纸） | 动图帧相位与"素材内帧"同一套换算；两端一致 | T8 |
| **T13** | SFX 音轨（`AudioPlan` 扩成多路 + gain + window） | 与视频同源求值；拼轨时长不偏 | T8 |
| **T14** | **端到端出片验证**（普通终端） | `check-cli.mjs` + `measure-export.mjs` 全绿；**这是 T8–T13 全部价值的兑现口** | 全部 |

**T14 必须排最后但不是可选项**：本仓那三条"没证到"的边界（README「还差什么」）
第一条就是它。**特效搬得再多，出片腿没跑通就等于没搬。**

---

## 9. 与 V-Trim 的接口（一次性说清）

```
V-Trim 侧（下游）                      本仓（底座）
─────────────────                     ──────────────
polish.toml ──┐
              ├─→ 转译器 ──→ ProjectDoc(JSON) ──→ dhampir（预览 wasm / 出片 native）
clip.json  ───┘                 ▲
                                └── V-Trim 负责：布局→Transform、运镜→keyframes、
                                    封面→frame+effects、去静音→时间轴裁剪、
                                    关键词→已求值的事件列表
```

**分工一句话**：**V-Trim 决定"画什么"，本仓决定"怎么画出来，且两端画得一样"。**

---

## 10. 边界（这次没做的，别当成做了）

**先记三条"设计时实地核对，发现比预想的更现成"的更正**（这些是**已核实**的，
不是猜测；写下来是为了防止有人按老印象去做重复工作）：

| 原本以为要做的 | 实际 | 证据 |
|---|---|---|
| 把 `Effect` 从"kind + 定长参数"改成 map | **已经是** `{kind: String, params: BTreeMap<String,f32>}` | `schema.rs:180-186` |
| 把渲染器派发从 if/else 改成按 pipeline | **已经按 `spec.pipeline` 派发**；已有 `Step` 计划结构 | `render/timeline.rs:64`、`:166`、`:885` |
| 特效的像素空间要新造一个字段 | **已经是** `EffectSpec.space`（含 `Source`/`Document`） | `schema.rs:270`、`:311` |

**所以 P9 的真正工作量集中在三处**：① `curve.rs` 泛化（**T8 已完成**）；
② **pass 顺序**从注释搬进可测规则（T9）；③ 三个新 pipeline 的 WGSL（T10/T11）。

其余边界：

- **本文是设计，不是实现**。§8 的 T8–T14 全部**未开工**。
- **没有量过任何性能**。`Warp`/`Composite` 的 pass 数与带宽影响**未测**；
  `Composite` 需要 ping-pong，那是"另一个数量级的改动"（`layer.rs:48` 原话）。
- **V-Trim 侧的转译器未设计**（只定了边界与分工）。
- **§2 的表是从源码读出来的**，但**没有跑过** V-Trim 的任何一条链路
  （本会话 shell 不可用）——"V-Trim 有 14 个 handler"来自 `templates/index.html`
  的 grep，不是运行结果。
- **`EffectPipeline` 的五个算子族是否真的够**，要等 T10/T11 落地才知道；
  不够就再扩一个变体，**不要**退回去按 kind 字符串派发。

---

## 11. 落地记录（T8–T12，逐条附证据）

**T8–T11 已完成并提交**（`6f77973`、`f63ac31`、`32b0097`、`6fc35aa`）。
五个算子族**够用**：V-Trim 那 8 个缺失特效全部落进了 `ColorMask` / `Warp`，
没有出现"退回去按 kind 字符串派发"的需要。

### 11.1 T12 动图：实地核对后的**真实边界**（重要）

`AssetKind::ImageSequence` 已加，校验规则（必须有正数 `frame_count`）已加，
逐帧定位**复用视频那一套**（`frame_count` + `timebase` → `source_frame`，
`compose.rs` 里 `EvalContext::source_frame` 一份代码），有测试钉着。

**但原生侧有一个已实测的限制，写在这里免得下一个人以为它能跑：**

原生解码走 `ffmpeg`（`pipeline.rs:585` `spawn_decoder`），一把梭地当**帧序列**读。
对动图容器的实测结果（8×8、**2 帧**的动画 GIF）：

```
ffprobe  nb_frames        = 2
ffprobe  avg_frame_rate   = 10/1        <- GIF 的**标称**帧率
ffmpeg（现有命令）解出     = 20 帧       <- 2 帧按 10fps 铺成 0.2 秒
```

于是**作者在登记表里写的 `frame_count`（2）与解码器实际吐出的帧数（20）不一致**。
后果是 `source_frame` 落到 0..20 上，而按时间基换算出来的索引只覆盖 0..2 ——
表现是**动图停在前两帧、剩下都在重复**，而且**不报任何错**。

**结论**：静态图（PNG/JPG，实测解出 1 帧）在原生侧**是好的**；
**动图在原生侧尚未真正端到端打通**。要打通，两条路：

1. **解码器按容器给的时间基出帧**（`-vsync 0` 或显式 `-r`），
   让"帧数"由素材决定而不是由标称帧率铺出来；
2. 或者**登记表里的 `frame_count` 以解码器实际帧数为准**（由 `info` 子命令回填），
   而不是让作者手填。

**浏览器侧不受这条影响**：`<img>`/`ImageBitmap` 自己按动画时序给帧，
`bitmaps` 那条路已经通了（`timeline_host.rs:347`）。

**这一条没做**，所以 §9 的"V-Trim 贴纸"**还不能算承接完毕**。

- **颜色怎么进 `Effect`**（§4.1 那条注）**尚未决策**，是 T9 的第一件事。
