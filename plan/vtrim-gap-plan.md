# 承接 V-Trim polish：缺口清单与分阶段计划

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

