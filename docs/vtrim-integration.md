# V-Trim → Dhampir 接入指导

这份文档写给**要把 V-Trim 的 `polish.toml` 喂给 Dhampir 出片的人**。
它只讲两件事：**怎么接**、**接了之后哪里会不一样**。

> **位置**：转译器在 `tools/polish-to-dhampir.mjs`（零依赖 ESM，一个文件）。
> 它**不属于** `dhampir-core` 底座 —— 底座不知道 V-Trim 是什么。
> 转译器是**下游**，它把 V-Trim 的方言翻译成底座能吃的契约。

---

## 一、它是什么形状

```
polish.toml  ──[tools/polish-to-dhampir.mjs]──▶  timeline JSON  ──▶  dhampir render
（V-Trim 方言）          转译器                    （底座契约）        （出片）
```

**为什么不让底座直接吃 `polish.toml`**：底座要同时服务浏览器预览与服务器出片，
它的契约必须窄、可校验、与"某一代编辑器的配置形状"无关。
V-Trim 的字段名、默认值、历史包袱属于**一方方言**，翻译放在边上是唯一能同时
"接得快"和"底座不脏"的形状。

**转译器不做的事**：
- 不探测媒体（`frame_count` 留空，由 `dhampir probe` 回填）
- 不解析系统字体（`--font-file` 显式给）
- 不猜缺省（凡是没有明确对应关系的一律进 DROP/CLAMP，见第五节）

---

## 二、快速开始

```bash
# 1. 转译。第一个参数是**片段目录**（里面要有 polish.toml），不是 toml 文件本身。
node tools/polish-to-dhampir.mjs \
  "E:/media/<项目>/output/clips/<片段名>" \
  --out out/vtrim/t.json

# 输出：
#   转译完成 -> out/vtrim/t.json
#     轨道 12 条 / 素材 10 个
#     运镜 14 条、贴纸 4 张、音效 3 条、overlay 0 条、原声 3 段、字幕 ✓、弹幕 4 条 ✓
#     丢失 3 / 降级 5 / 注意 39
#     ---- DROP (3) ----   ...（详见第五节）

# 2. 校验（**不需要 GPU、不需要 ffmpeg**）
dhampir probe --project out/vtrim/t.json

# 3. 回填素材帧数（转译器故意不探测）
dhampir probe --project out/vtrim/t.json --write   # 或按 --help

# 4. 出片
dhampir render --project out/vtrim/t.json --from 0 --to 5811 --out out/dhampir.mp4 \
  --asset-root "E:/media/<项目>/output/clips/<片段名>" \
  --font-file C:/Windows/Fonts/msyh.ttc \
  --font-bold-file C:/Windows/Fonts/msyhbd.ttc
```

**字幕/弹幕是**：转译器把 V-Trim 的 `clip.srt` / `clip.dm.srt` 旁写成
`dhampir-subtitle.srt` / `dhampir-danmaku.ass`，**写在工程文件旁边**，
再在工程里登记成 `Subtitle` 素材。别把它们删了。

**字体必须显式给**（`--font-file`）。工程有字幕轨而你没给，render 会**判失败**而不是
偷偷用一个系统字体 —— 因为"同一份工程在两台机器上挑到不同字体"是两端可比最难查的一类失败。

---

## 三、契约里最小要知道的几条

| | |
|---|---|
| **帧号是整数** | 时间一律是 `Frame`（整数）。转译器负责 `秒 × fps` 的取整，取整规则见第四节 |
| **帧率是有理数** | `timebase = {num, den}`，不是 f32。素材自己的时间基与序列的分开记 |
| **关键帧帧号是图层相对的** | 不是绝对帧号。最后一个关键帧必须 **`< duration`** |
| **缓动写在"到达"的那个关键帧上** | 求值取的是相邻一对里**后一个**键的 `easing` = "怎么到达 b" |
| **没有素材 + 有特效 = 调整图层** | 这是**推导**出来的，不是标志位。`shake`/`flash` 这类必须挂这种层 |
| **有素材的层只跑模糊** | 别的管线挂上去会被 `validate_timeline_v2` 报 `effect_would_be_ignored` |

---

## 四、行为对照表（逐项）

这一节是**转译器的规格说明**。左边是 V-Trim 的行为（引用行号），右边是本仓的表达。

### 4.1 运镜（camera）

| V-Trim | 本仓 |
|---|---|
| 状态机：`zoom=1.0` 初值；`td = min(duration*0.5, 0.6)`；过渡发生在每个镜头的**起点** | 每个事件在 `srcStart` 与 `srcStart + tdFrames` 各写一组关键帧 |
| `hardCut = cut \|\| tgt==远 \|\| zoom==远` | 硬切时 `tdFrames = 0`，起点即终点 |
| `pow2_out(t) = 1-(1-t)²` | `Easing::EaseOut` —— **逐值相同**（`schema.rs` 有用例钉着） |
| `pow2_in(t) = t²` | `Easing::EaseIn` —— 逐值相同 |
| `ox = lerp(ax, bx, p)`，`origin` 是百分比 | `cameraOffset(z, origin) = (1-z)·(origin - 中心)`，单位是文档像素 |
| **手持摇摆** `handheldSway(t, amp)`：两条正弦 × dx/dy/rot/scale | **按帧采样**成 `x`/`y`/`scale`/`rotation` 关键帧（见 4.2） |

### 4.2 三条"按帧采样"的曲线

**凡是 `t` 的纯函数，就按帧采样成关键帧，不要用手写几条关键帧去猜它。**
这是本项目反复验证过的教训（手写必漏口径）。

| 曲线 | 为什么不能手写 |
|---|---|
| **手持摇摆** | 4 个通道 × 每条 2 个正弦分量，共 6 个频率/相位参数，手写关键帧必然失真 |
| **贴纸弹入** | 用 `back_out(p, 3)`（`s=3`），而契约的 `Easing::BackOut` 是经典 1.70158 —— 峰值 1.10 vs 1.25，**装不下这个参数** |
| **贴纸四段相位** | 四段的边界与那条"够不到的淡出"（见 4.3）必须一起复现 |

代价是关键帧变多（手持一段 8.5 秒 ≈ 510 帧 × 4 通道），收益是**逐帧精确**。
底座的关键帧求值本来就是 O(log n) 二分 + 线性插值，这个量级不构成负担。

### 4.3 贴纸（sticker）

| V-Trim（`getActiveSticker`） | 本仓 |
|---|---|
| 窗口 `[et, et + dur + 0.65]` | 图层 `[startFrame, startFrame + windowFrames + 1]` |
| `popIn 0.35`：`scale = 0.3+0.7·back_out(p,3)`、`rot = -15+15·bp`、`opacity = pow2_out(p)` | 逐帧采样（4.2） |
| `floatUp 0.4`：`yOff = -18·SK·p`，**线性** | 逐帧采样 |
| `hold = dur`：`yOff = -18·SK` | 逐帧采样 |
| `fadeOut 0.3`：**够不到**（见下） | 如实复现"够不到"，并在报告里 NOTE |
| `SK = min(CW,CH)/1080`，盒子 `S = 280·SK` | `SK` 按短边算；`fit = S/素材宽` |
| 落点 `x: left + S/2, y: top + S/2 + yOff`（中心对齐） | `transform.x/y` 加 `+box/2` 把左上角换成中心 |

> **参照侧的不一致（值得回给 V-Trim）**：窗口判据给到 `dur + 0.65`，
> 而四段加起来是 `dur + 1.05` == 淡出那一支要 `t >= dur + 0.75`，**永远够不到**。
> `0.65` 正好等于旧的 `popIn(0.35) + fadeOut(0.3)` —— 看起来是后来插入
> `floatUp(0.4)` 时忘了改守卫。
> **结果是贴纸没有淡出、硬切。** 本仓按参照复现，不自作主张加回淡出。

### 4.4 字幕（subtitle）

| V-Trim（`getActiveSubs`） | 本仓 |
|---|---|
| `fadeIn 0.35` / `fadeOut 0.18`，**`if / else if` 互斥** | `text_envelope`，同样互斥 |
| 淡入：`opacity = pow2_out(p)`，`yOff = 20·(1-opacity)` | 同上 |
| 淡出：`opacity = pow2_in(p)`，`yOff = -8·(1-p)` ← **注意乘的是 `(1-p)` 不是 `(1-opacity)`** | 同上（两段口径**故意不对称**） |
| `cy = CH - 120·CKH` | `bottom_margin = (120 - 行盒/2)/H` |
| 换行安全宽 `CW * 0.875` | `SubtitleStyle::safe_width_ratio = 0.875` |
| CSS：`font-size:72px`、`color:#dcbda0`、`stroke:12px #403c3b`、`font-weight:700`、`line-height:1.5` | `font_ratio` / `color` / `stroke_ratio` / `stroke_color` / `font_weight` / `line_height` |

**`entrance` 是死配置**：`"cycle-5"` / `"fade-up"` 全仓（JS、Rust、webui）
**没有一个读者** —— 只被写进 toml、只在 `types.rs` 里有定义。动效是写死的。
转译器把它记进 DROP 并说明。

### 4.5 弹幕（danmaku）

| V-Trim（`getActiveDms`） | 本仓 |
|---|---|
| `if (t < d.start \|\| t >= d.end) continue` —— **在屏窗口 = 素材自己的 `[start,end)`** | `exit = frame_at_ms(cue.end_ms) - 1` |
| `travel` **只管滚动速度**，不管在屏时长 | `scroll_start = enter + fade_frames`，`travel_frames` 独立 |
| `travel = (1080 + utf8_字节数*32)/150` 秒 | 同上（**按字节，不是字符**） |
| `x = CW - min(progress,1)·(CW + textW)` | 同上 |
| `y = (d.y/1920)·(540·CK)` → 归一化 `d.y/3840` | `lane_top_ratio` / `lane_spacing_ratio` |
| `TRACK_YS = [300,500,…,1500]` | `lane_top_ratio = 300/3840`、`lane_spacing_ratio = 200/3840` |
| **颜色只有轨道级**（`getActiveDms` 不返回颜色） | 轨道级 `color`；素材里的 `\c` 标签转译器**剥掉** |
| CSS：`font-size:42px`、`#ffffff`、`stroke:2px #000`、`font-weight:600`、`base opacity 0.9` | 对应字段 |

### 4.6 瞬时特效（fx）

| V-Trim | 本仓 |
|---|---|
| `shake`（canvas）`getShake`：固定 0.2s 的**四级阶梯** | ⚠️ **尚未对齐**（见第六节） |
| `flash` / `vignette` / `noise` / `overlay` | `ColorMask` 管线，挂在**调整图层** |
| `blur` | `SeparableBlur` |
| `hue_shift` / `color_shift` | `ColorAdjust` / `ColorMask` |
| `zoom_bounce` / `pulse` / `split` | **`Warp` 管线** |
| 竖屏下 `camera` 被屏蔽（其余照做） | 转译时按 `orientation` 处理 |

### 4.7 音频

| V-Trim | 本仓 |
|---|---|
| `mute_ranges` = **把这几段掐掉**（不是静音） | 主画面拆段 + 所有事件时间重映射 |
| 原声是**默认就有的底** | 必须**显式**写一条引用视频素材的音频层（第一版漏了这个，症状是成片整个没声音） |
| `sfx` 的 `vol = ev.volume \|\| CFG.sfx.volume \|\| 0.1` —— **覆盖，不是相乘** | `layer.gain` |
| `shake_enabled` / 音效驱动画面 | ⚠️ **没接**：本仓没有"音效驱动画面"这条链路 |

---

## 五、三级记录：DROP / CLAMP / NOTE

转译器**每次运行都打印一份三方清单**。这是这个接口最重要的产物 ——
它把"哪里不一样"从"事后靠对帧发现"变成"转译时就知道"。

| 级别 | 含义 | 使用者该做什么 |
|---|---|---|
| **DROP** | 参照里有、本仓**根本不表达** | 判断能不能接受；不能就得扩契约 |
| **CLAMP** | 有对应概念但**表达力或口径不够**，转出来的与原意不同 | 逐条看，多数是"量化后可接受" |
| **NOTE** | 信息，行为一致或已按参照复现 | 供审阅；**含参照侧的不一致** |

**按级别统计**（本样例）：DROP 3 / CLAMP 5 / NOTE 39。

**NOTE 里有两类要特别看**：
- `**这是参照侧可以修的不一致**` —— 比如贴纸那条够不到的淡出。**这类要回给 V-Trim。**
- `这一条用了事件自己的 origin（…），与 layout.liver 中心（…）不同` —— 这类是**如实记录**，
  因为参照确实允许两者不同，不是错。

**给下游的用法**：把清单当**构建产物**接进流程，不要只打日志。
例如 CI 里断言 `DROP == 0`，或把 CLAMP 逐条挂到"已知差异"的台账上。

---

## 六、当前已知的差异（**未对齐，按影响排序**）

| # | 差异 | 影响 | 状态 |
|---|---|---|---|
| 1 | **`shake` 移植错了版本** | 本样例 `intensity=0.4` 像素，影响小；但幅度大的项目会明显不对 | 见下 |
| 2 | 字幕**折行缩字**未实现 | 参照装不下会缩字号（下限 0.7、最多 3 行）；本仓直接丢行 | 未做 |
| 3 | `max_lines` 默认 2（参照 3） | 长字幕的断行位置可能不同 | 未做 |
| 4 | **高亮词 `.hl`** 未实现 | `clip.srt` 有 `<span class="hl">` 时颜色不生效 | 未做（本样例没有） |
| 5 | 弹幕**逐条滚动时长** | 参照按文本字节数逐条算（本样例 20.2/16.4/16.8/14.2 秒），本仓在**轨道级**取平均 16.9s | 要动契约 |
| 6 | `stage.bg_mode` 的模糊底 | 画中画档四周黑边；参照能铺模糊底 | 未做 |
| 7 | 贴纸 GIF **逐帧延迟** | `AssetKind::ImageSequence` 需要一张延迟表，本仓没有 | 未做 |
| 8 | 音效驱动画面 | 参照的 `shake_enabled` 会按音效强弱触发抖动 | 未做 |
| 9 | 封面（`polish cover`） | 另一条输出通道（静态图），本仓不做 | 有意不做 |
| 10 | 预览侧的**事件联动脉冲** | 字幕 `scale 1.25 / y -20`，**只在预览**、不在出片 | 未做（不影响出片） |

### 第 1 条细说：`shake` 抄了哪一份

参照有**两份** `shake`，形状完全不同：

| 出处 | 用途 | 形状 |
|---|---|---|
| `index.html:538` `shake(tl,…)` | **GSAP DOM 预览** | `n = dur/0.05` 个 0.025s 的交替偏移 |
| `index.html:1578` `getShake(t, items)` | **canvas 出片** | **固定 0.2s 的四级阶梯**，与事件 duration 无关 |

```js
// 出片那一份（getShake）
if (t >= sh.time && t < sh.time + 0.2) {
  var off = sh.ev.intensity || 14;          // 直接当**像素**用
  var dt = t - sh.time;
  if (dt < 0.04) return { x:  off,     y: -off     };
  if (dt < 0.08) return { x: -off,     y:  off     };
  if (dt < 0.12) return { x:  off/2,   y: -off/2   };
  if (dt < 0.16) return { x: -off/2,   y:  off/2   };
  return             { x: 0,        y: 0        };
}
```

**转译器目前按预览那一份做成了连续波形 —— 这是错的，下一轮要改。**
这条同时是"**同一个名字在两条腿上可能是两种东西**"的样本：转译时
**认的是出片那条腿**，不是预览。

---

## 七、怎么扩：加一个新的 V-Trim 事件类型

1. 在 `tools/polish-to-dhampir.mjs` 的逐事件 `switch` 里加一个 `case`。
2. **先在参照里找出片那条腿的实现**（`getActiveXxx` 或 `renderFrame` 里那一段），
   不要看 GSAP 时间轴 —— 那是预览（第六节第 1 条就是这么踩的）。
3. 判断它能不能用**现有契约**表达：
   - 能 → 用关键帧 / 调整图层 / 音频层表达
   - 是 `t` 的纯函数 → **按帧采样**（4.2）
   - 不能 → 在报告里 `clamp(...)` 说明差在哪，**不要静默近似**
4. **对帧验证**：出片后与参照逐帧比。**不要只看平均值** ——
   本项目有 4 个 bug 在 13 个时刻的平均差里完全看不出来（−0.08/−0.02/−0.82/0.00），
   只有把两边并排看才发现。
5. 有新的常量/口径，补一条**会被变异打红**的用例（"一条不会红的用例不算用例"）。

---

## 八、要给回 V-Trim 的清单

转译过程中发现的、**参照侧本身可以修**的问题：

| # | 问题 | 出处 | 建议 |
|---|---|---|---|
| 1 | 贴纸的 `fadeOut` 相位**永远够不到**（窗口 `dur+0.65` vs 相位要 `dur+0.75`），贴纸硬切 | `templates/index.html` `getActiveSticker` | 判据改成 `dur + 1.05`，或把 `floatUp` 算进窗口 |
| 2 | 配置项 `entrance`（`"cycle-5"` / `"fade-up"`）**没有任何读者** | `vtrim-render-core/src/types.rs:570` | 要么实现，要么从 schema 去掉（现在会让人以为能配） |
| 3 | 同一个名字 `shake` 在**预览**与**出片**是两套完全不同的实现 | 见第六节第 1 条 | 统一，或改名区分 |
| 4 | 字幕动效只在**预览**有"事件联动脉冲"，**出片没有** | `index.html:1128`（GSAP）vs `getActiveSubs` | 明确哪一端是想要的 |

---

## 九、这条接口的边界

**转译器只读不写 V-Trim 的东西**：它读 `polish.toml` / `clip.srt` / `clip.dm.srt`，
把旁写文件放在**输出目录**里。

**它不承诺兼容**：它不是 `dhampir-core` 底座的一部分，形状可以随 V-Trim 演化而改。
底座承诺兼容的只有 `docs/api-surface.md` 里"底座 API"那一栏。

**它不做媒体探测**：`frame_count` 留空由 `dhampir probe` 回填 ——
转译要能在一台没有 ffmpeg 的机器上跑完并给出清单。
