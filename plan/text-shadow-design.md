# 文字阴影：契约扩充设计（交接单 B5）

> **这份文件是什么**：要不要给字幕样式加"文字阴影"、以及**加哪种**的设计与成本。
> 起因：下游交接单 `plan/dhampir-base-handoff.md` 的 **B5** —— 他们那边 `style.subtitle` 的
> 文字阴影与入场动画 `cycle-5` 一直如实报 **DROP**；"要不要扩契约由底座决定"。
>
> **它不是什么**：不是实现记录（**还没写代码**）。按本仓规矩：**契约改动先落文档**，
> 形状定了再写；写的时候两端都要接，缺一处就是第 6 次"写好了没接上"。

---

## 1. 现状

`SubtitleStyle`（`crates/dhampir-timeline/src/layer.rs`）已有：`font_ratio` / `color` / `outline` /
`stroke_color` / `stroke_ratio` / `font_family` / `font_weight` / `line_height_ratio` /
`safe_width_ratio` / `fade_in_ms` / `fade_out_ms` / `rise_in_px` / `rise_out_px` / `highlight_color`。
**没有任何阴影字段**。

参照那边的写法是 CSS：`text-shadow: 0 2px 12px rgba(0,0,0,.4)` —— 也就是
**偏移 (0, 2px) + 模糊半径 12px + 半透明黑**。这三个参数里，**"模糊"是两端能力的分水岭**。

## 2. 决策点：要不要"模糊"

| | **A. 硬阴影（推荐先做）** | **B. 带模糊** |
|---|---|---|
| 字段 | `shadow_color` + `shadow_dx_px` + `shadow_dy_px` | A + `shadow_blur_ratio` |
| CLI（ffmpeg `drawtext`） | **原生支持** `shadowcolor` / `shadowx` / `shadowy` | ⚠️ `drawtext` **没有**模糊 ⇒ 得在 Rust 侧把栅格化出来的位图**自己模糊**（可分离两趟），多一段 CPU 卷积 |
| 浏览器（canvas） | `shadowColor` / `shadowOffsetX` / `shadowOffsetY`（`shadowBlur = 0`） | 原生 `shadowBlur` |
| 两端能对齐吗 | ✅ 能（不涉及模糊核：偏移与颜色都是确定量） | ⚠️ 只能"观感近似"：canvas 的 `shadowBlur` 与我们的高斯核不是同一条公式。**属字形级差异**（本仓允许字形像素不同、结构必须一致） |
| 风险 | 小：纯新增字段 + 两个宿主各加几个参数 | 中：出片路径每行字幕多一趟模糊（性能未量）；"近似"要写清口径，否则以后有人拿它当逐像素判据 |
| 与参照的观感差 | 边缘硬（12px 模糊变成 0） | 接近 |
| 改动面 | 契约 + core 求值 + 两宿主 | 上面那些 **+ CLI 侧新写一段模糊** |
| 建议 | **先做 A**：改动小、能立刻验、两端能对齐 | 真需要"观感一致"时再追加 |
| **定案（2026-10-01）** | ⬇️ **用户选的是 B**：要"观感接近参照"，接受"两端近似、非逐像素" | → **已按 B 实现，见 §9** |

> **为什么不一次做到 B**：B 的 `shadow_blur_ratio` 如果**先加字段却不实现**，那就是**假声明**
> ——正是本仓最不想要的东西（"契约里 ✓、文档里 ✓、执行路径一次都没读过它"）。
> 所以 B 的字段**等实现它的那一刻再加**。

## 3. 字段形状（A）

```jsonc
"subtitle": {
  // …既有字段不动…
  "shadow_color": [0, 0, 0, 102],   // Option<[u8;4]>；缺省 None = 不画阴影
  "shadow_dx_px": 0.0,              // **文档像素**（与 transform.x/y 同一坐标系，T1 的换算照走）
  "shadow_dy_px": 2.0
}
```

三条口径要一起写进字段注释（否则以后会各写各的）：

1. **坐标系**：`dx/dy` 是**文档像素**，不是目标像素 —— 预览 640×360 与成片 1920×1080 必须落同一个比例；
2. **只画一次**：阴影是"同一行字按偏移再画一遍"，**不参与描边宽度**（描边与阴影各自独立）；
3. **`shadow_color` 的 alpha 生效**（半透明阴影是常态），而 `color` 那一支的 alpha 语义不变。

## 4. 版本与兼容（决定"要不要升版本"）

| 面 | 结论 | 依据 |
|---|---|---|
| 老工程是否逐字节不变 | **必须**不变（`SkipSerializingIf` + `Default` ⇒ 新字段不进老 JSON） | T2/T7 系列一直拿"既有工程逐字节不变"当硬判据 |
| `LAYER_SCHEMA_VERSION` | **不升** | 既有规矩："加特效与加转场**不必升版本**"（`docs/usage.md` §7.4 原话）；纯新增可选字段同一档 |
| `HOST_API_VERSION` | **4 → 5** | 既有规矩：**wasm 返回体加字段即视为 API 变更**（T2.7 就是这么升到 2 的）；`TextStyleView` 要加这三个字段 |
| 派生面 | `schema/*`（`timeline-contract.mjs --write`）、`docs/host-api.md`（`api-surface.mjs --write`） | 本仓"有派生物就得重生成" |

## 5. 落地清单（**两端都要接**，缺一处就是"写好了没接上"）

| # | 位置 | 做什么 |
|---|---|---|
| 1 | `crates/dhampir-timeline/src/layer.rs` | `SubtitleStyle` + 3 字段（注释按 §3 的三条口径） |
| 2 | `crates/dhampir-core/src/overlay.rs` | `TextStyle` 带上它们，并**在求值层解析掉**（宿主只读结果） |
| 3 | `crates/dhampir-timeline/src/host_api.rs` | `TextStyleView` + 3 字段（**HOST_API_VERSION +1**） |
| 4a | `crates/dhampir-worker/src/text_raster.rs` | ffmpeg `drawtext` 加 `shadowcolor` / `shadowx` / `shadowy`（颜色要转成它认的 `0xRRGGBB@a`） |
| 4b | `crates/dhampir-worker/src/text_overlay.rs` | 位图叠加路径按同一偏移落阴影；`--font-file` 缺省路径不受影响 |
| 5 | `crates/dhampir-wasm/src/timeline_host.rs` | 把 3 个字段带给 JS（清单里要能看见） |
| 6 | `web/engine.js` | canvas `shadowColor/OffsetX/OffsetY` + `shadowBlur = 0` |
| 7 | `scripts/web-check.mjs` | 两端**结构**比对加上这 3 个字段（字形像素不比） |
| 8 | 用例 | ① 求值层单测（字段透传 + 缺省 None 时**逐字节不变**）；② 一条真机 GPU 出帧：阴影**真的落在偏移处**（按 §3 的坐标系换算）；③ 一条反向：把 `shadow_color` 设成 None 时与老工程逐字节相同 |
| 9 | 文档 | `docs/usage.md` 的字幕样式表 + `docs/api.md` 的承诺表（若涉及版本）；`plan/defects.md` 若要把这条记成条目则按台账格式 |

## 6. 明确不做（别顺手做）

* **`entrance = "cycle-5"` 那类逐帧循环入场动画**：现在的 `fade_in/out` + `rise_in/out` 是"一条包络"，
  而 `cycle-5` 是**逐帧循环**的入场样式 —— 那是"逐行动画"这一层的新概念，与本条不是一件事；
* **阴影不进墨迹报告**：它与弹幕同理（每行都画，判据会恒真）；
* ~~**不做"阴影模糊"**~~：**2026-10-01 已按 B 落地**（用户定案，见 §9）。
  原设计这条的理由是"字段先加却不实现 = 假声明"——落地时**字段与实现是同一次加的**，所以那条纪律没有被破坏。

## 7. 建议与触发条件

* **建议**：按 **A（硬阴影）** 落地。理由：两端能对齐、改动面小、可立刻用现有判定通道验；
  它能把下游那条 DROP 从"没有"变成"有（边缘硬）"，**并且把 `cycle-5` 那条明确留在 DROP 里**（理由写清）。
  ⚠️ **2026-10-01 定案：用户选了 B**（带模糊），已实现并验收，见 §9 —— 本节其余内容按原样留作设计史。
* **转 B 的触发条件**：下游出现"字幕阴影必须与参照**观感**一致"的验收项，且他们接受
  "两端近似、非逐像素"这条口径。

---

## 附：本次同时发现的一处**契约与实现不一致**（不是扩充，是修）—— **已修**

`overlay` 的 `shape=0`（纯色）在 `color_mask.wgsl` 里取的是 **`r2/g2/b2`**（因为 `grad_t = is_solid = 1`），
而 `OVERLAY` 的文档写着"渐变的第二个颜色用 `r2/g2/b2`"（言下之意纯色只用 `r/g/b`）。
平时看不出来，是因为参数打包让 `r2` 缺省回落到 `r`。

**2026-10-01 定案：改实现**（`grad_t` 去掉 `is_solid`）。判据是下游转译器的 `solidEffect`
把两组颜色写成一样（`r2: col.r, g2: col.g, b2: col.b`）⇒ 修正对现有工程**零影响**。
钉子：`tests/timeline.rs` 里一条**故意 `r2≠r`** 的纯色判据 + `handoff-probe.mjs --self-test`。
详见交接单 §7.9。

---

## 8. 验收清单（**先量基线，再改代码**）

### 8.1 逐字节不变的基线（**2026-10-01 15:2x 量于改动之前**）

命令（需要 `target/s3/` 里有素材；字幕素材要先从 `fixtures/` 复制过去 —— 一个 `--asset-root` 盖不住两个目录）：

```bash
cp fixtures/sample-subtitle.srt fixtures/sample-subtitle.ass target/s3/
dhampir frame --project fixtures/sample-subtitle.doc.json --frame <N> \
  --out target/shadow-baseline/f<N> --asset-root target/s3 \
  --font-file C:/Windows/Fonts/msyh.ttc
```

| 帧 | SHA256 前 16 位（**无阴影**的期望值） |
|---|---|
| 0 | `70184BEE185BEA2E` |
| 30 | `56B92D0FD0149112` |
| 60 | `72188AE65E72A442` |
| 90 | `72188AE65E72A442` |

**判据**：加了阴影字段的代码，在**不带** `shadow_color` 的老工程上重跑这四帧，摘要必须**逐位相同**。
（若不同 → 说明"不画阴影走老路"没做到，**不许调基线**，去修实现。）

### 8.2 功能判据

| # | 判据 | 怎么量 |
|---|---|---|
| 1 | 阴影**真的落在偏移处** | 同一帧 + 带 `shadow_color` / `shadow_dx_px` / `shadow_dy_px`，在文本以外、偏移方向那一侧取像素，与无阴影同一帧比：必须变暗 |
| 2 | 阴影**带模糊**（不是硬边） | 跨阴影边界取一条剖面：亮度必须是**渐变**的，不是台阶（用"中间值像素占比"或相邻差分都行） |
| 3 | 两端**结构一致** | `web-check.mjs` 的 `--verdict subtitle` 通道加上这 4 个字段的比对（字形像素不比） |
| 4 | 求值层换算 | `shadow_blur_ratio`（比例）→ `shadow_blur_px`（目标像素）的单测，且与 `stroke_ratio`/`stroke_px` 同一处换算 |
| 5 | 老工程**不出现新键** | `edit --write` 往返后 JSON 里不得多出 `shadow_*`（`skip_serializing_if` 生效） |
| 6 | **独立验收**（不重用实现方的用例） | `node scripts/handoff-probe.mjs shadow` —— 它自己在像素上验四件事：字段在场但不给颜色时**逐字节相同**、给了颜色画面确实变、差异**落在偏移方向**、墨迹之外是**渐变而不是硬台阶**。字段还没落地时它**明确跳过**（跳过 ≠ 通过） |

### 8.3 收口命令（全绿才算完）

```bash
cargo check --workspace --all-targets            # 0 warning
cargo test --workspace                           # 0 failed
cargo test -p dhampir-worker --test <阴影用例> -- --ignored   # 真机那条 ok
node scripts/timeline-contract.mjs --write && node scripts/timeline-contract.mjs
node scripts/api-surface.mjs --write && node scripts/api-surface.mjs   # HOST_API_VERSION 5
node scripts/check-text-hygiene.mjs
node scripts/run-guards.mjs                      # 20/20
node scripts/handoff-probe.mjs                   # 15/15（另一件事，不许被弄红）
```

---

## 9. 落地记录（2026-10-01：**按 B 实现**）

用户定案：要"观感接近参照" ⇒ 走 **B（4 字段，带模糊）**。
**字段与实现是同一次加的**，所以 §2 那条"先加字段却不实现 = 假声明"的纪律没有被破坏。

### 9.1 与 §3 的差别

§3 只列 3 个字段；实际落地多一个 `shadow_blur_ratio`（占**文档高**的比例，0 = 硬阴影）：

```jsonc
"subtitle": { …, "shadow_color": [0,0,0,102], "shadow_dx_px": 0.0, "shadow_dy_px": 2.0,
             "shadow_blur_ratio": 0.0111 }
```

`TextStyle`（core）里是**像素** `shadow_blur_px = shadow_blur_ratio × 目标高`，与 `stroke_ratio`/`stroke_px` 同一处换算。

### 9.2 两个宿主各自在哪读它、怎么画

| 宿主 | 读的地方 | 怎么画 |
|---|---|---|
| **CLI（worker）** | `text_overlay.rs::shadow_spec` —— **全四键的唯一判据**：给了颜色**且 alpha≠0**；`None` 与"全透明"都走老路 | `text_raster.rs` 同一趟 ffmpeg 里：`drawtext`（白字覆盖度）→ **`gblur=sigma=blur_px/2`** → 既有 `tint` 染成阴影色；位图四周扩 `shadow_pad_px(blur,dx,dy)`（**pad 进缓存键**）；贴图原点 = 文字原点 **− pad + (dx,dy)**；`paint_one` 与 `paint_parts` **两条路都接**，且**先影子、后文字** |
| **wasm / 浏览器** | `timeline_host.rs::text_style_json` 把 4 键发给 JS | `engine.js::drawShadow` 用 canvas 原生 `shadowColor/Blur/OffsetX/OffsetY`；**不画阴影时一个 `shadow*` 属性都不设**（设成 `blur=0`/透明色仍会走一遍阴影合成，像素会变） |

σ 的口径：canvas 的 `shadowBlur` 是**半径**、ffmpeg `gblur` 吃 **σ** ⇒ 同一个数过界要 **÷2**（`shadow_sigma_px`）。两端只保证**观感近似**。

### 9.3 版本与兼容

* `HOST_API_VERSION` **4 → 5**（wasm 返回体加字段）；`LAYER_SCHEMA_VERSION` **不升**（与"加特效/加转场不必升版本"同档）。派生面已重生成（`schema/*`、`docs/host-api.md`）。
* 老工程逐字节不变：**实测四帧摘要与改动前逐位相同**（§8.1 那张表）。

### 9.4 验收读数（都在改完后的树上取的）

| 判据 | 读数 |
|---|---|
| `cargo test --workspace` | **687 passed / 0 failed / 30 ignored** |
| 真机 `tests/text_shadow.rs`（3 条） | **3 passed**：影子只往偏移侧长（字 `(270,317,369,336)` / 影子 `(261,317,377,355)`，上边不冒）；模糊边缘 **8 档渐变**；硬影轮廓外 **0** 像素 vs 模糊 **204** 像素（alpha 1..21）；不画阴影两种写法 **sha 相同** |
| `handoff-probe.mjs`（**独立判据**） | **21 / 21**（shadow 支 6 条：不给颜色逐字节相同 / 给色后确实变 / 差异落在偏移方向 / 墨迹之外是渐变（**档数 36**）…） |
| `run-guards.mjs` | **20 / 20**；`check-dual-end` 最差 **SSIM 1.000000** |
| 文本卫生 / 契约 / 调用面 | **231** 全绿 / 无漂移 / 版本 **5** |

### 9.5 已知边界（**没证到的，别当证过了**）

1. **浏览器侧影子的像素没有自动判据**：`web-check --verdict subtitle` 只比**结构**（含这 4 个键），不比字形；样例工程也没写阴影 ⇒ 只能算"两端不矛盾"，**不算"像素验过"**。
2. **影子超出该行位图时两端行为不同**：浏览器画在与文字同一张画布上（会被裁），CLI 另开一张扩过边的位图（不裁）。已写进 `docs/host-api.md` 的已知边界。
3. **偏移/模糊与 `stroke_px` 同量纲**（都用求值层拿到的那个目标尺寸），**没有**再做 doc→target 二次换算 —— 沿用既有边界；默认路径上两者相等。

### 9.6 顺带发现（**不是本次引入**，未顺手改）

wasm 的 `text_item_json`（`timeline_host.rs:2784`）**不吐逐条 `color`**，而 CLI 的同名函数吐（还有 `font_ratio`/`scale`），`web-check` 又逐条比 `color` ⇒ **浏览器 subtitle 判定在 HEAD 上就是红的**（实测报"一端没有、另一端有"）；
同一处 `TextLineSpec.color/scale` 在 wasm32 构建里是 `dead_code` 警告。
这属于"写好了没接上"，**不在本条范围内**——要不要单独立项由用户定。

