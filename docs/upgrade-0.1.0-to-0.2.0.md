# 0.1.0 → 0.2.0 下游对接说明

**给下游宿主（V-Trim）**。写于 `2c55e78`，产物 `dhampir-0.2.0-win32-x64.zip`。

这份文件只回答一件事：**从 0.1.0 换到 0.2.0，你那边要改什么、要重做什么、什么不用动。**

---

## 0. 一句话

**契约号一个都没动**（`project_schema = 1`、`host_api = 6`），**但你拿到的字节变了** ——
字幕栅格化器从 `drawtext` 换成了 libass，所以**老工程的字幕像素会变**。

这意味着：**你的契约校验会全绿，而画面会变。** 没有任何自动提示，只能靠这份文件知道。

---

## 1. 必须做的三件事

| # | 做什么 | 为什么 |
|---|---|---|
| **1** | **重拍视觉基线**（凡是有字幕的截图回归 / SSIM / PSNR 基线） | 字幕每帧像素都变了。口径不用改，**基线的图要重拍**，否则会全红 |
| **2** | **确认打包的 ffmpeg 带 `--enable-libass`** | 0.2.0 起是硬前置。没编不会静默出错（会响亮失败），但会在出片那一刻才炸 |
| **3** | **给字体改成走 `font_family` + `--font-dir`**（若你还在只给 `--font-file`） | 这是 0.2.0 的新正路；且**缺字回退**要靠它才能生效 |

自检命令（第 2 条）：

```bash
ffmpeg -filters | grep subtitles      # 有输出 = 带了 libass
```

---

## 2. 破坏性变更：字幕像素会变

**范围**：只有**含文字层（字幕 / 弹幕）**的工程受影响。**无字幕工程一个像素都不差** ——
这条是硬判据，实测 `fixtures/sample-project.doc.json` 第 15 帧的 sha256 在整轮改造前后
**逐字节相同**（`91ac70187a5d3fdd7d01a568effa7f7df7463783c0623b583bd6a8aeca3ceff2`）。

**变了两处**（都实测过）：

| 差异 | 0.1.0（`drawtext`） | 0.2.0（libass） |
|---|---|---|
| 边缘抗锯齿 | 斜坡 0.63 ~ 1.43 px | **斜坡 1.63 ~ 3.17 px（更柔）** |
| 字距 | 每字推进 40 px | **每字推进 ~38 px（有漂移）** |

两边都仍在"窄抗锯齿"口径内（模糊会是十几像素），所以**判定口径（SSIM/PSNR/边缘口径）不用改**，
但**字确实会比 0.1.0 略柔、略紧**。四个字全部同向，不是偶发。

**你不用做的**：不用改契约校验、不用改工程文件、不用改 API 调用。

---

## 3. 契约面：没动，但是"加了一堆能用"

### 导出面：**零变化**

```
host_api = 6        （0.1.0 = 6，没动）
```

我逐符号 diff 过：0 个导出新增、0 个删除。`dhampir_host_api_version()` 仍返 6，
所以你现有的版本校验**不需要任何改动**。

### 工程文件 schema：**纯新增，0 删除**

`project_schema = 1` 没动（旧工程照读），但你能**写**的东西变多了：

| 能力 | 新字段 |
|---|---|
| **裁剪形状** | `clip`、`kind`（circle / ellipse / inset）、`center`、`radius`、`radius_x`、`radius_y`、`corner_radius`、`left`/`top`/`right`/`bottom` |
| **渐变** | `gradient`、`stops`、`angle_deg`、`offset_x`、`offset_y` |
| **遮罩 / 蒙版** | `mask`、`coverage`、`invert`、`channel` |
| **模糊** | `blur_sigma` |
| **阴影** | `shadow` |
| **路径 / 点** | `points`、`at`、`data` |
| **背景特效** | `backdrop_effects` |
| **画布底色** | `background` —— **只在 `doc-v1`**，见下 |

### 还有一处"字段没变、能用的值变多了"：混合模式 4 → 9

`blend` 字段 **0.1.0 就有**，所以它**不在**上面那张新增表里 —— 但**能用的值从 4 个变成 9 个**：

| | 0.1.0 可用 | 0.2.0 可用 |
|---|---|---|
| `blend` 的取值 | 4 个：`normal` / `add` / `multiply` / `screen` | **9 个**（上面 4 个 + `darken` / `lighten` / `overlay` / `soft_light` / `difference`） |

0.1.0 里后 5 个是**枚举占位**（`is_implemented()` 返回 false，渲染器不实现）；
0.2.0 全部实现了。这条**在契约校验上完全看不出来**（schema 没动），
所以如果你那边按 `is_implemented()` / 能力清单挡过这几个值，现在可以放开了。

> ⚠️ `clip` 只支持 **circle / ellipse / inset（圆角矩形）** 三种 —— 一共用一套无分支 SDF 表达式。
> **`polygon()` 与 `path()` 明确不做**（需要一般多边形求交或掩码纹理）。别在下游 UI 里放开这两个。

**两条 schema 的新增集合不完全一样**，别当成同一份：

| schema | 新增字段数 | 差异 |
|---|---|---|
| `timeline-v4` | **25** | — |
| `doc-v1` | **26** | 多一个 `background`（只 `doc-v1` 有） |

`background` 是**文档级画布底色**（`render_hints.background`），所以它只在 `doc-v1`；
timeline schema 里没有这个字段是正确的，不是漏了。

---

## 4. 字体：这是 0.2.0 最需要你配合的一块

### 0.1.0 的问题

`drawtext` **不会做字形回退**：字体缺某个字形就直接画成 `.notdef` **空心方框，且退出码为 0**。
实测：乐米波波体缺 `靥 U+9765`，「笑靥如花」渲染成「笑☒如花」—— **静默出错**。

### 0.2.0 的做法

libass **逐字形回退**：哪个字缺就换一份有它的字体补上。

```
0.1.0:  drawtext  → 缺字 = 方框
0.2.0:  libass    → 缺字 = 从别的字体补（逐字形）
```

### 你要改的：字体定位从"给文件"变成"给名字"

| | 0.1.0 | 0.2.0 |
|---|---|---|
| 主路 | `--font-file <文件>` | 工程里 `subtitle.font_family` = **族名** + `--font-dir <目录>` |
| 兜底 | — | `--font-file` |

**两个坑，都会静默出错**：

1. **`font_family` 必须是字体内部的真族名，不是文件名。**
   实测四种推法里**只有真族名能命中**，其余全部**静默**解析成 `ArialMT`
   （而 `lines_failed: 0`、`issues: []` —— 比 .notdef 还隐蔽）：

   | 传给 libass 的名字 | 实际解析到 |
   |---|---|
   | `lemi`（文件名主干） | ❌ ArialMT |
   | `乐米波波体（免费商用）_爱给网_aigei_com` | ❌ ArialMT |
   | `LemiBoBoTi`（英文内部名） | ❌ ArialMT |
   | **`乐米波波体`（真族名）** | ✅ LemiBoBoTi-Regular |

2. **回退落到哪份字体取决于机器上装了什么。** 实测缺字那次落到 `MicrosoftYaHeiUI`。
   同机同目录两次出片一致，但**换机器/平台，缺字那几个字可能换一副字形**。

### 需要你们定的（本仓答不了）

> **回退用的字体集合该由谁提供 —— 未对齐，如实记为未定。**

本仓没有字体栈（那是宿主/系统的事），所以**没有单方面假设**。建议由你那边
**随应用附带一套回退字体 + 一个 `--font-dir`**，这样跨机才确定。
定了之后本仓可以把它写进契约文档。

---

## 5. 你这边的改动清单（建议顺序）

1. **换产物**：`dhampir-0.2.0-win32-x64.zip`（+ `.sha256.txt`，下载地址写稳定名那个）。
2. **确认 ffmpeg 带 libass**（第 1 节的自检命令）。
3. **字体改走 `font_family` + `--font-dir`**，族名用真族名；确认旧工程的 `font_family` 值合法。
4. **重拍视觉基线**。
5. **（可选）** 评估要不要放开新 schema 字段（裁剪 / 渐变 / 混合 / 遮罩）到你的 UI。
6. **（待定）** 定下回退字体集合的归属，回报本仓。

---

## 6. 怎么确认你拿到的是哪一版

产物根目录有一份 `VERSION`：

```
version=0.2.0
project_schema=1
host_api=6
license=Apache-2.0
git=a03327c
built_at=2026-10-07T19-07-16
platform=win32-x64
```

> ⚠️ **product version 没有运行时 API**。wasm 侧只有 `dhampir_host_api_version()`（返 6），
> **没有** `dhampir_version()`。所以"我这份产物是 0.1.0 还是 0.2.0"**只能读 `VERSION` 文件**。
> 如果你们要在运行时区分，得自己读那个文件 —— 或者告诉本仓，加一个导出（那要 `host_api` +1）。

---

## 7. 0.2.0 的其他新增（可能对你有用）

| 能力 | 在哪 |
|---|---|
| **渲染能力面**：混合模式 / 渐变 / 路径 / 多边形 | `dhampir-core` 的 `render/{blend_fn,gradient,path,polygon}.rs` + 对应 WGSL |
| **WAAPI / CSS 动画对齐**：缓动全集 + `anim2doc` 转译器 + DOM 第二宿主 | `web/anim-eval.mjs`、`web/dom-host.mjs`、`scripts/waapi2doc.mjs` |
| **效果演示** | `fixtures/effects-demo.json` + `web/effects-demo.html`（WASM 与 DOM 两版）+ 联络表 PNG |

### 能力清单是**机器核对的**，别抄文档

如果你要判断"某个 CSS / WAAPI 特性底座支不支持"，**真值是
[`web/capabilities.json`](../web/capabilities.json)**（28 条，由 `scripts/check-capabilities.mjs` 核对：
`status=supported` 必须给出底座原语与判据证据，否则守卫判红）。

当前分布：**supported 15 / partial 9 / explicitly-not-doing 3 / needs-primitive 1**。
`partial` 与 `explicitly-not-doing` 都写了理由，**要放开 UI 之前先读它**，
不要凭印象假设某个 CSS 特性支持。

### 关于"两宿主一致"的既有口径（没变，但值得知道）

* **wasm ↔ native**：**同一个渲染器**（一份 core + 一份 WGSL），像素可比，有容差表。
* **HTML ↔ wasm/native**：**不是同一个渲染器**（CSS 合成器 vs WebGPU）。HTML 侧
  `web/anim-eval.mjs` 是**刻意的第二实现**，由 `scripts/check-anim-eval.mjs` 逐帧逐通道对照
  （容差 1e-4，实测最大偏差 9.37e-6）—— **只保证数值一致，不承诺像素**。
* 浏览器侧画字走 canvas `fillText`（**系统字体**），是**第三条独立路径**，
  与 CLI 本来就不同源；判据只比结构、不比字形。这条口径 0.2.0 不变。

---

## 8. 已知边界（如实记，别当已解决）

| 项 | 状态 |
|---|---|
| **描边颜色当前不受 `stroke_color` 控制** | 填充与描边共用一张覆盖掩码（libass 的覆盖度在 RGB、alpha 恒 0，黑描边与透明底逐字节相同，事后不可恢复）。**已用判据钉住**，拿到分层掩码时会红 |
| **回退字体集合归属** | **未与 V-Trim 对齐，未定** |
| **非 Windows 平台** | 全部实测都在本机 Windows + gyan.dev ffmpeg 上做的，没验过 |
| **`1.25` / `288` 两个补偿常数** | 只测了 msyh / 乐米 / NotoSansSC 三个字体族，**是否对所有字体族成立未验** |
| 推进宽度"差 1% 还是 5%" | 两次独立复量结论不一致，已写进代码注释标注**别引它**（判据不依赖它） |

---

## 9. 本版的验证读数（可复算）

| 项 | 读数 |
|---|---|
| `cargo test --workspace` | **766 passed / 0 failed** |
| `cargo check --all-targets` | **0 warning** |
| `node scripts/run-guards.mjs` | **26 / 26 全绿**（含各自 `--self-test`） |
| 真机测试（真起 ffmpeg、逐像素量） | **9 / 9 全绿** |
| `node scripts/licenses.mjs --check` | EXIT=0 |
| 打包 | `dist/dhampir-0.2.0-win32-x64.zip`，sha256 `616ed6e15cb5914e14603dba2c0b501290aeb383d7066b2e08b8fa7f35eb28d9` |
| 打包产物里的 exe 端到端复跑 | 缺字仍正确回退（「笑靥如花」，非透明像素 1099） |
| 无字幕工程帧 sha256 | **整轮未变**（`91ac70187a5d3fdd7…`） |
