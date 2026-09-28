# 承接 V-Trim polish：缺口清单与分阶段计划

> **转译器已搬到 V-Trim 那一侧。** 这份文档里出现的 `tools/polish-to-dhampir.mjs`
> 指的是**当时**它在 Dhampir 里的位置；现在它在：
>
>     <V-Trim>/tools/polish-to-dhampir.mjs
>     <V-Trim>/scripts/check-vtrim-translator.mjs
>     <V-Trim>/docs/vtrim-integration.md   （转译接口正文）
>
> 下面的命令与读数**保持原样不动** —— 它们是当时的实测记录，改掉就不是记录了。

这份文档把 `tools/polish-to-dhampir.mjs` 转译时**报出来的每一处缺口**
汇总成一张表，并给出分阶段的落地计划。数字全部来自实测
（样本 A = `海市蜃楼之馆`，样本 B = `最鱿鱼之人`）。

**读数基线**：两个样本都能出片，与 V-Trim 成片的平均逐像素差 **10.5 / 255**
（JPEG q=0.6 的底噪约 6）。剩下的差异基本都来自下面这些缺口。

---

## 1. 缺口总表

按「动哪一层」分组。**优先级 = 视觉影响 / 改动成本**。

### A. 文本样式（最直接对应「字幕样式」「弹幕样式」两条反馈）

| # | 项 | V-Trim 的实际值 | 本仓现状 | 优先级 |
|---|---|---|---|---|
| **A1** | 字幕淡入/淡出 + 入场上浮 | `fadeIn 0.35s`、`fadeOut 0.18s`、入场 `+20px` 浮上来、退场 `-8px` | **无字段** | **高** |
| **A2** | 弹幕淡入/淡出 | `0.3s` / `0.2s` | **无字段** | **高** |
| **A3** | 弹幕基础不透明度 | **`0.9`**（不是 1.0） | 写死 1.0 | **高** |
| **A4** | 弹幕颜色 | `#ffffff` | **无字段**，与字幕**共用**一份 | **高** |
| **A5** | 字幕/弹幕共用一个颜色 | 两者不同色（字幕 `#dcbda0`、弹幕白） | `TextOverlay.color` 只有一份 → 必然错一个 | **高**（A4 的前提） |
| A6 | 描边色与宽度 | 字幕 `12px #403c3b`、弹幕 `2px #000` | 只有 `outline: bool` | 中 |
| A7 | 高亮词 | 字幕 `#f56b41`、弹幕 `#ff6bcb`（`.hl` spans） | 无 | 中 |
| A8 | 字重 / 行高 / 文字阴影 | `700` / `1.5` / `0 2px 12px rgba(0,0,0,.4)` | 无 | 低 |

### B. 弹幕时长

| # | 项 | 现状 | 优先级 |
|---|---|---|---|
| **B1** | **逐条**滚动时长（按文本字节数算） | 本仓在**轨道级**一个值，一份素材一个数 | 中 |

实测四条：`20.21 / 16.37 / 16.80 / 14.24` 秒，取平均 `16.91s` 会
让短句滚太慢、长句滚太快。

> **拆轨不是出路**：泳道分配是**每轨各自从 0 开始**
> （`overlay.rs:203` 在 track 循环里调 `layout_danmaku`），
> 拆成 4 条轨会把每条都塞进第 0 泳道、叠成一坨。

### C. 音频

| # | 项 | 现状 | 成本 | 优先级 |
|---|---|---|---|---|
| **C1** | **每层音量 `gain`** | `AudioSegment.gain` **运行时已经有了**（混音器真的乘它），但**契约没有字段** → `audio.rs:334` 写死 `1.0` | **极小** | **高** |
| C2 | 混音总线音量（`[sfx] volume = 0.1`） | 无总线增益 | 小 | 中 |
| C3 | 音效驱动画面（`shake_enabled`） | 无链路 —— 音频与画面两个子系统之间没有连线 | 大 | 低 |

`C1` 是**整张表里最便宜的一处**：加一个 `Layer.gain`，混音器那行不用改。

### D. 画面构成

| # | 项 | 现状 | 优先级 |
|---|---|---|---|
| D1 | `stage.bg_mode`（模糊/纯色铺底） | 本仓没有"背景层"。远景（`zoom=0.55`）四周露黑边 —— 与 V-Trim 一致（它也是黑底），但它还能铺模糊底 | 中 |
| D2 | `handheld` 的真实摆动波形 | 用整层 `shake` 近似（V-Trim 有自己的 `handheldSway`） | 低 |
| D3 | 分区变换（只放大 `layout.liver` 那一块） | 整帧缩放 —— **这条已经不必做了**：运镜公式对齐之后平均差降到 10.5，分区变换只是"更理论正确"，不是可见错的来源 | 低 |

### E. 边缘

| # | 项 | 现状 |
|---|---|---|
| E1 | 贴纸安全区推导（`position_mode = "auto"`） | 本仓没有障碍物求解；落点直接取 `polish.toml` 里的百分比 |
| E2 | 封面产出 | 另一条路（静态图），V-Trim 自己也是另开一条 |
| E3 | `filter.danmaku` 过滤词 | 本仓弹幕没有过滤功能 |
| E4 | `pow2_out` / `back_out` 缓动 | 本仓只有 `linear/ease_in/ease_out/ease_in_out`。运镜与贴纸都只能近似 |
| E5 | 字体名（`LXGW WenKai`） | 本仓用 `--font-file`，没有"字体名"概念（**这是有意的**：不猜系统字体） |
| E6 | `entrance = "cycle-5"` | **不是缺口**：V-Trim 自己也没实现 —— 全仓只有 `polish.toml` 提到它，没有任何代码读 |

---

## 2. 分阶段计划

### 阶段 1（本次）：把「用户点名的两条反馈」做掉

对应 §1 的 **A1–A5 + C1**。这五项合起来是一个连贯的改动：
「文本样式从"一套"变成"每类一套 + 带时间函数"」。

1. **C1** `Layer.gain: f32`（默认 1.0）
   - 接进 `plan_audio`，`audio.rs:334` 的写死值换成读契约
   - 校验：`gain` 必须有限且 ≥ 0

2. **A5** `TextOverlay` 的 `color`/`outline` 拆成**字幕一套、弹幕一套**
   - 现在是一份，模块文档里写着"要分开就得动契约"
   - 拆成 `subtitle: TextStyle` / `danmaku: TextStyle`

3. **A4 + A3 + A2** `DanmakuSpec` 加
   `color` / `opacity` / `fade_in_ms` / `fade_out_ms` / `stroke_ratio` / `stroke_color`

4. **A1 + A6** `SubtitleStyle` 加
   `fade_in_ms` / `fade_out_ms` / `rise_in_px` / `rise_out_px` / `stroke_ratio` / `stroke_color`

5. **两端都实现**：native（`text_overlay.rs`）+ wasm（`TextLineSpec` 带出去、web 照画）
   - 淡入淡出的**时间函数住在契约层**（与 `Effect::strength` 同一条理由：
     "这一帧多透明"只能有一个定义）

### 阶段 2：观感对齐

- **A7** 高亮词（解析 `<span class="hl">`）
- **A8** 字重/行高/阴影
- **B1** 逐条弹幕时长（`DanmakuSpec` 支持"按文本算"，或 cue 级时长）
- **D1** 背景层（`stage.bg_mode`）
- **E4** 缓动补齐 `pow2_out`（运镜与贴纸都能用上，贴纸的 `back_out` 也可补）

### 阶段 3：链路打通

- **C2** 混音总线增益
- **C3** 音效驱动画面（跨子系统，需要先定接口）
- **E1** 贴纸安全区推导（这是个**求解**问题，不是样式问题）

### 明确不做

- **E2 封面**：静态图产出，与逐帧合成不是同一条路
- **E5 字体名**：本仓**故意**不猜系统字体
- **D3 分区变换**：运镜对齐之后不是可见错的来源（见 §1-D3）

---

## 3. 阶段 1 的契约改动明细

```rust
// dhampir-timeline/src/layer.rs

pub struct Layer {
    // ... 既有字段 ...
    /// 音频增益（线性倍数，1.0 = 原样）。**只有音轨层用它**。
    /// 视频层上写它等于没写（渲染器不看），但也不报错 —— 与 `opacity` 不同，
    /// 它不是"这一层怎么画"而是"这一层怎么响"。
    #[serde(default = "one")]
    pub gain: f32,
}

pub struct SubtitleStyle {
    // ... 既有 ...
    /// 淡入时长（毫秒）。0 = 硬出现。
    #[serde(default = "subtitle_fade_in")]
    pub fade_in_ms: u64,
    #[serde(default = "subtitle_fade_out")]
    pub fade_out_ms: u64,
    /// 入场时从下方浮上来的距离（**文档像素**）。
    #[serde(default = "subtitle_rise_in")]
    pub rise_in_px: f32,
    /// 退场时向上浮的距离。
    #[serde(default = "subtitle_rise_out")]
    pub rise_out_px: f32,
    /// 描边宽度 = 目标高度 * 这个比例。
    #[serde(default = "subtitle_stroke_ratio")]
    pub stroke_ratio: f32,
    #[serde(default = "subtitle_stroke_color")]
    pub stroke_color: [u8; 4],
}

pub struct DanmakuSpec {
    // ... 既有 ...
    #[serde(default = "danmaku_color")]
    pub color: [u8; 4],
    /// 基础不透明度。V-Trim 用 0.9 —— 弹幕压在画面上，全不透明会太抢。
    #[serde(default = "danmaku_opacity")]
    pub opacity: f32,
    #[serde(default = "danmaku_fade_in")]
    pub fade_in_ms: u64,
    #[serde(default = "danmaku_fade_out")]
    pub fade_out_ms: u64,
    #[serde(default = "danmaku_stroke_ratio")]
    pub stroke_ratio: f32,
    #[serde(default = "danmaku_stroke_color")]
    pub stroke_color: [u8; 4],
}
```

**默认值全部取本仓原有行为**（`fade_*_ms = 0`、`opacity = 1.0`、
`stroke_*` = 现有 `outline: bool` 的等价），于是**既有工程逐字节不变**
（`schema` 版本不用升）。

`TextOverlay` 的改动：

```rust
pub struct TextStyle {
    pub color: [u8; 4],
    pub outline: bool,
    pub stroke_ratio: f32,
    pub stroke_color: [u8; 4],
}

pub struct TextOverlay {
    pub items: Vec<TextItem>,
    pub danmaku: Vec<DanmakuTextItem>,
    pub subtitle_style: TextStyle,   // 原来是共用的 color / outline
    pub danmaku_style: TextStyle,
    pub dropped_lines: usize,
    pub dropped_danmaku: usize,
}
```

**时间函数住在契约层**（`dhampir-timeline`），两端调同一份：

```rust
/// 一条字幕/弹幕这一帧的（不透明度, 纵向偏移）。
/// `local_ms` 是它在屏的第几毫秒，`span_ms` 是它在屏总长。
pub fn text_envelope(
    local_ms: u64, span_ms: u64, fade_in_ms: u64, fade_out_ms: u64,
    rise_in_px: f32, rise_out_px: f32,
) -> (f32, f32);
```

与 `Effect::strength` 同一条理由：**"这一帧多透明"只能有一个定义**，
否则两端会各写一遍，而"两端各自的都对"这件事让人查不出来。

---

## 4. 验收判据

阶段 1 做完之后要能证明：

1. **`gain`**：三条音效的音量（0.4/0.5/0.45）**在成片里真的生效** ——
   用 `ffmpeg volumedetect` 量，而不是看 JSON。
2. **字幕/弹幕颜色互不影响**：同一份工程里字幕是暖色、弹幕是白色
   （现在必然有一个错）。
3. **淡入淡出**：抽三帧（入场中、稳态、退场中）量不透明度递增/递减。
4. **既有工程逐字节不变**：`check-dual-end` 的 SSIM 仍是 `1.000000`，
   `check-cli` 的字节数不变。
5. 全仓绿：`cargo test --workspace`、20 条守卫、0 warning。

---

## 5. 阶段 1 落地记录（已收口）

**改动**：

| 层 | 内容 |
|---|---|
| 契约 | `Layer.gain`；`SubtitleStyle` +6 字段；`DanmakuSpec` +6 字段；`text_envelope()` |
| `dhampir-core` | `TextOverlay` 拆成 `subtitle_style`/`danmaku_style`；条目带 `opacity`/`dy_px` |
| `dhampir-worker` | `blit_scaled`（alpha 缩放）；描边宽度/颜色由契约给；纵向偏移 |
| `dhampir-wasm` | `TextItemView`/`DanmakuItemView` 带淡入淡出；`OverlayView` 拆两套样式 |
| `host_api` | `TextStyleView`（**不含** `From<core>`：依赖是单向的） |
| `web/engine.js` | `rasterizeLine(line, style)`；两类各传各的；描边宽度/颜色跟契约 |
| 守卫脚本 | `web-check.mjs` 比两套样式 + 逐条 `opacity`/`dy_px`；`check-overlay-plumbing.mjs` 判据跟着改 |

**两个我先前报错了、这一轮纠正的**：

1. **`dhampir-wasm` 在 wasm32 上编译不过**，而我只跑了宿主的
   `cargo check --workspace --all-targets`（**exit 0**，因为那几处导出在
   `#[cfg(target_arch = "wasm32")]` 里）+ grep `^warning:` 数了 0 条就宣布"干净"。
   → **wasm32 编译已进验收清单**（§6.2）。
2. **`stroke_ratio` 的默认值破掉了"既有工程逐字节不变"**：
   写成 `12/1080` 会让 640×360 预览的描边从 `border_px(20)=1px` 变成 4px。
   → 默认改成 `0.0`（= 从字号推，老路径），并让 `stroke_px == 0` 时
   **颜色也走 `black` 老路径**。加了用例钉死。

**读数**：

| 项 | 读数 |
|---|---|
| `cargo test --workspace` | **645 passed**（阶段 0 是 644） |
| `cargo build -p dhampir-wasm --target wasm32-unknown-unknown` | **exit 0** |
| `cargo check --workspace --all-targets` | **exit 0 / 0 warning** |
| `node scripts/run-guards.mjs` | **20 / 20** |
| `check-dual-end` 最差 SSIM | **1.000000**（既有工程逐字节不变） |
| 字幕淡入淡出（墨迹量） | 11374k（无字幕）→ **11390k（opacity 0）** → 14050k → 15680k → **16618k（满）** → … → 12090k（淡出中）→ 11486k（消失） |
| 音效增益（`volumedetect` 平均） | gain `0.4/0.5/0.45` = **-18.0 dB**；gain `1.0` = **-12.9 dB** |

**两条都是拿真产物量出来的，不是看 JSON。**

---

## 6. 两个系统性的改进

### 6.1 一条守卫：**「写好了没接上」**

这个模式已经**抓到 4 次**：

| # | 字段 | 表现 |
|---|---|---|
| 1 | `Effect.window` / `Effect::strength` | 瞬时闪一下变成整段一直闪着 |
| 2 | `TRANSFORM_TARGETS` | 关键帧 target 拼错静默失效 |
| 3 | `AudioPlan.overlaps` | 削顶看得见、叠加看不见 |
| 4 | `CueStyle.color` | **逐条颜色永远不生效**（本轮发现，见 §2-A1） |

**新守卫 `scripts/check-wired-fields.mjs`**：扫契约 crate 的 `pub` 结构字段，
对每个字段检查"除定义文件外，全仓至少有一处读它"。零读取者 = 红灯。
**允许白名单**（纯序列化 DTO、测试专用）。

价值在于：4 次里有 3 次是**测试全绿、校验通过、只有出片才看得出**。

### 6.2 验收清单里加 **wasm32 编译**

```powershell
cargo build -p dhampir-wasm --target wasm32-unknown-unknown   # 必须 exit 0
```

理由见 §5 的第 1 条：宿主的 `cargo check --workspace` **看不见**
`cfg(wasm32)` 里的代码，而"预览坏掉"正是这个仓最贵的一类错。

---

## 7. 阶段 2 落地记录（已收口）

### 7.1 逐条弹幕颜色（§2-A1）—— **契约一个字段都没加**

`CueStyle.color` **早就定义了**，却有两处断线：`parse_ass` 从不解析 `\c&HBBGGRR&`
（一律写 `default()`），且全仓没有一处读它。**第 4 次"写好了没接上"。**

接线四段：

1. `ass_text_to_plain` 顺带抽 `\c` / `\1c`（**ASS 是 BGR**）
2. `CueStyle.color` → `DanmakuItem.color`（结构里，与泳道并列）
3. `evaluate_overlay` **就在求值层解析掉** → `TextItem.color` / `DanmakuTextItem.color`
4. 宿主只读结果；wasm/web 的 `TextLineSpec.color` 一路带到 JS

**第 4 步第一次漏了**：求值层算对了，渲染那一层读的还是 `style.color`
—— CLI 的 JSON 里 `[227,63,255,255]` 是对的，而出帧**一个品红像素都没有**。
是像素级核对抓出来的。

### 7.2 弹幕泳道带（§2-B1）

缺口是**量出来的**：逐行差在 40-60 行出现低谷（6.81），两边各有一条文字带 ——
本仓在 0-40、V-Trim 在 60-100。

契约加 `lane_top_ratio` / `lane_spacing_ratio`（**默认都是 0 = 复现老行为**）。
转译器写 `300/3840` 与 `200/3840`（这个换算是与画布尺寸无关的）。

### 7.3 读数

| 项 | 读数 |
|---|---|
| `cargo test --workspace` | **651 passed**（阶段 1 是 645） |
| wasm32 / 0 warning / 守卫 | exit 0 / 0 / **20 / 20** |
| `check-dual-end` SSIM | **1.000000** |
| 弹幕带位置 | `rect.y = 0.0781`（V-Trim 期望 0.0781） |
| 单帧（30s）逐像素差 | 8.18 → **6.20**（JPEG 底噪约 6） |
| 全片 13 个时刻平均 | 10.52 → **10.30**；最差 22.68 → 21.69 |

**逐时刻**（改动前后的对比，注意有些时刻**略微变差**是正常的：
两次独立渲染的同一测量本身有 ±1 的抖动）：

    30s   8.18 → 6.24    ✓ 泳道带
    79s   6.64 → 5.08    ✓
    65s  11.99 → 9.06    ✓
    20s  22.68 → 21.69   ✓
    5s   11.27 → 11.72   ✗（噪声级）
    48.8s 11.33 → 12.51  ✗（噪声级 + 贴纸动效近似）

### 7.4 剩下的差异是什么（诚实交代）

平均 10.3 里，**大于底噪的那部分主要来自两处，都不是底座缺陷**：

1. **字体**：本仓只能用 `--font-file` 给替代字体，V-Trim 的 `LXGW WenKai`
   这台机器上没有。字形不同 → 字幕/弹幕所在的**那几行**逐像素差必然大
   （实测 60-140 行的差是 11~24，而 0-60 行只有 3~5）。
2. **`shake` 的幅度标定**：V-Trim 的 `intensity` 是观感档位，本仓是像素位移比例，
   我按 0.05 缩（§1-C 那一条），过冲与衰减曲线不同。20s 那一帧（正好在
   `shake` 窗口里）差 21.69 就是它。

想再往下压，要动的是 §3 的阶段 3（字体选择 + 字重/行高）与阶段 5（缓动补齐），
**不是**底座还缺什么结构。

---

## 8. 阶段 3–5 一次做完（已收口）

用户要求「把没做的整合起来一次性处理」。这一轮把 §2 的 B2–B5 与 B6/B7 全做了。

### 8.1 先纠正两处**我搞错了的**

1. **V-Trim 的默认字体不是 `LXGW WenKai`。**
   那是个**老工程写进 toml 的值**，而 `font_dir()` 里没有这个字模 ——
   V-Trim 自己的注释写着（`render/server.rs:148`）：
   > 解析不到就**什么都不注入**：老工程的 `font = "LXGW WenKai"` 走到这里，
   > 行为与从前一致（**当系统字体族名用**）。

   于是它落进字体栈
   `'LXGW WenKai','Noto Sans SC','Microsoft YaHei',sans-serif` ——
   前两个这台机器上都没有，**实际生效的是微软雅黑**。
   （实测：msyh 与 Deng 的逐像素差在噪声内，所以字体**不是**残余的主因。）

2. **`pow2_out` / `pow2_in` 与本仓的 `EaseOut` / `EaseIn` 逐值相同。**
   `pow2_out(t) = 1-(1-t)^2`、`pow2_in(t) = t^2` —— 我先前把它们报成
   "同族不同参、形状略有差别"，**那是错的**。加了用例逐值钉住。

### 8.2 这一轮真正改掉的东西（按实测影响排）

| # | 项 | 实测 |
|---|---|---|
| 1 | **贴纸动画帧率**：GIF 是 `100/3`（33.3fps），转译器写死 10fps → **慢 3.33×** | 48.8s −2.45、75s −2.66 |
| 2 | **字幕落点换算**：V-Trim 的 `cy = CH-120` 是**文字中心**，本仓 `bottom_margin` 是**行盒底边** —— 直接搬 120 会让字幕**高 54px** | 55s 字幕带 34.16 → 26.42 |
| 3 | **描边口径**：CSS `text-stroke:12px` 居中（外侧 6px），ffmpeg `borderw` 全在外侧 —— 契约定**外侧宽度**，转译器折半 | 55s −0.98 |
| 4 | **字重**：V-Trim 700（字幕）/ 600（弹幕），本仓**完全不设** | −0.3 |
| 5 | 行高 `1.5`（本仓常量 1.2）、字体族如实带进契约 | 随 #2 一起 |
| 6 | `Easing::BackOut`（贴纸弹入的**过冲**） | 小窗口 |
| 7 | `TrackV2.gain`（音效母线；V-Trim 是 `ev.volume \|\| bus`） | 音频正确性 |

### 8.3 一个**又犯了**的错（同一类，第三次）

`probeImage` 里我用 `-of csv=p=0` 按**位置**解字段 —— 而 ffprobe 的字段顺序
**加一个字段就会重排**：请求 `nb_read_frames,width,height` 时吐
`width,height,nb_read_frames`；再加 `r_frame_rate` 就变成
`width,height,r_frame_rate,nb_read_frames`。于是我把**帧率当成了帧数**。

**改成 `-of default=nw=1` 按 `键=值` 解** —— 顺序不再要紧。
（这个坑本仓已经踩过两次，注释里还写着教训，第三次仍然踩了。
所以不是"注意一点"，是**不要按位置解 CSV**。）

### 8.4 读数

| 项 | 读数 |
|---|---|
| `cargo test --workspace` | **653 passed**（阶段 2 是 651） |
| wasm32 / 0 warning / 守卫 | exit 0 / 0 / **20 / 20** |
| 全片 13 个时刻平均逐像素差 | 10.30 → **9.19** |
| 最差时刻 | 21.69 → 21.20 |
| 逐时刻 | **13 个全部改善**，最大 −2.66 |

    5s   11.72 →  9.86      45s   8.90 →  7.96     75s  10.98 →  8.32
    10s  10.17 →  8.62      48.8s 12.51 → 10.06     79s   5.08 →  4.98
    20s  21.69 → 21.20      55s  15.80 → 14.42     90s   8.95 →  7.69
    30s   6.24 →  5.98      65s   9.06 →  8.75
    35s   6.29 →  6.02      70s   6.55 →  5.68

### 8.5 剩下的 9.19 是什么（诚实交代）

**不是契约缺口**，三件事：

1. **文字栅格化器不同**：V-Trim 的成片走 headless Chrome 的 **Canvas2D/Skia**，
   本仓的成片走 **ffmpeg drawtext/FreeType**。同一个字体、同一个字号、
   同一个位置，两种引擎画出来的字形边缘逐像素不同 ——
   实测字幕带是一个**连续的**高差区域（不是两条），说明"位置对、字形不同"。
   要抹掉它得把出片那一路也换成 canvas，那是另一件事。
2. **贴纸 GIF 的逐帧延时不均匀**：V-Trim 用 `sticker_frames.json` 里的
   `delays` 数组逐帧推进；本仓的 `timebase` 是一个有理数，只能表达**均匀**间隔。
   这批 GIF 恰好是均匀的（全 30ms），所以对齐了；不均匀的会对不上。
3. **`shake` 的幅度标定**：`intensity` 是观感档位、本仓 `amount` 是像素位移比例，
   我按 0.05 缩 —— 20s 那一帧（正在 `shake` 窗口里）差 21.20 就是它。



