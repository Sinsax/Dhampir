# polish.toml → Dhampir 工程：一次真实的转译

> **转译器已搬到 V-Trim 那一侧。** 这份文档里出现的 `tools/polish-to-dhampir.mjs`
> 指的是**当时**它在 Dhampir 里的位置；现在它在：
>
>     <V-Trim>/tools/polish-to-dhampir.mjs
>     <V-Trim>/scripts/check-vtrim-translator.mjs
>     <V-Trim>/docs/vtrim-integration.md   （转译接口正文）
>
> 下面的命令与读数**保持原样不动** —— 它们是当时的实测记录，改掉就不是记录了。

两个样本，都记在这份文档里：

| 样本 | 目录 | 特点 |
|---|---|---|
| **A** | `[海市蜃楼之馆]\...\不追星了但看到粉丝吵架...` | 贴纸 `name`/`source` 空（Step2b 没跑）、`danmaku.json` 为空、无 shake |
| **B** | `最鱿鱼之人\...\小雨现在跟很多人关系不错...` | **更完整**：Step2b 跑过、有真弹幕数据、有 `shake` 事件、贴纸平铺、**有 `mute_ranges`** |

转译器在 `tools/polish-to-dhampir.mjs`。**这份文档记的是实测，不是设计推演。**

**读数总表**（与 V-Trim 成片逐像素比，越低越像；V-Trim 走 JPEG q=0.6 有底噪）：

| | 第一轮 | 第二轮 | 第三轮（当前） |
|---|---|---|---|
| 平均绝对差 | 48.5 / 255 | 22.1 | **10.5** |
| 最差时刻 | 93.7（79s） | 67.9（75s） | **22.7**（20s） |

---

## 0. 第三轮：四个「看起来对、其实完全不对」的地方

用户的原话是「**效果完全不对，但是已经有类似的效果了**」。查完发现
四个都是**结构性的错**，而且**抽帧看静止画面全都看不出来**。

### 1. `mute_ranges` 是**把那些段掐掉**，不是"静音"

这是最要命的一条，**整条时间轴都错了**。

| | 时长 |
|---|---|
| 源 `clip.mp4` | 114.383s |
| **V-Trim 成片** | **96.900s** ← 短了 17.49s |
| 三段 `mute_ranges` | 5.30 + 2.36 + 9.83 = **17.49s** ← 正好对上 |

也就是说 `mute_ranges` 里那些段**在成片里根本不存在**，后面所有内容
**整体前移**。按"只静音、不裁剪"转出来的东西从第一刀（56.14s）之后
**全部错位**，最大偏 17.49 秒。

实测对账（把 V-Trim 的时刻换算成"源时刻"之后）：

    V-Trim 65s  <-> 源 70.30s   逐像素差  9.17（错位时是 28.18）
    V-Trim 79s  <-> 源 84.30s   逐像素差  6.72（错位时是 93.74）
    V-Trim 90s  <-> 源 97.66s   逐像素差  8.39

**修法**（三件事一起做）：

1. 算出**保留区间**（mute 的补集，在源时间与输出时间上都记着）；
2. 主画面从"一整层"变成**每个保留区间一层**，`source_in` 指向该区间的源起点
   —— 实测三段：输出 `[0,3368)` / `[3368,4790)` / `[4790,5812)`；
3. **所有东西的时间都重映射**：运镜关键帧、贴纸、音效、字幕、弹幕。
   字幕与弹幕要另写一份文件（`dhampir-subtitle.srt` / `dhampir-danmaku.ass`），
   跨剪辑点的 cue **切成两段**（只按起点重映射会让整条错位）。

改完成片 96.867s，与 V-Trim 的 96.900s 只差 33ms（2 帧取整）。

### 2. 运镜：V-Trim 是**状态机**，不是"每条事件画两个端点"

权威实现在 `vtrim-polish/templates/index.html:1398 getActiveCamera()`：

```js
var zoom = 1.0                        // 初值，不是第一个事件的值
for each camera 事件 c:
    tgt = camFactor(c.zoom)
    td  = min(c.duration * 0.5, 0.6)   // 过渡窗口，最多 0.6 秒
    hardCut = c.cut || tgt == 远 || zoom == 远
    p   = (t >= ce || hardCut) ? 1 : pow2_out((t - c.time) / td)
    zoom = (t >= ce || hardCut) ? tgt : lerp(zoom, tgt, p)
```

**我先前三条都做错了**：

- **过渡只占事件开头 `td`（≤0.6s）**，我却让它在相邻两条事件之间
  一路插值 —— 实测 45s 处 zoom 是 1.3→1.5 的中间值，而 V-Trim 稳稳的 1.3
  （**差 42.46/255**）。
- **远景（`远`）进出是硬切**（`td` 当 0），我按普通过渡算。
- **初值是 1.0**，我直接用了第一个事件的值。

还有一个便宜的巧合让关键帧能**精确**表达它：`offset = (1-z)*(origin-中心)`，
过渡期内 `origin` 不变（`to_origin` 缺省即 origin），而 `z` 按 `pow2_out`
插值 —— 于是 `offset` 也是同一条缓动曲线上的线性量，
`x`/`y`/`scale` 用同一个 easing 写关键帧就是**逐值等价**的。

唯一的近似：V-Trim 用 `pow2_out`、本仓只有 `ease_out`（同族不同参）。

**硬切要"钉前一帧"**：两个关键帧落在同一帧上会互相覆盖，
曲线仍会从**上一个关键帧**插值过来。所以硬切要在 `t-1` 帧把
**上一个状态**钉住。这里我还踩了一个坑：只记 `prevZoom` 不够，
**`prevOffset` 也要记** —— offset 同时依赖 zoom 与不动点，
拿"本事件自己的 origin"重算会让位置从 `-86.52` 一路插值到 `0.00`
（第 65 秒差 **44.75/255**，缩放对、位置不对）。

修完平均差 **48.5 → 10.5**。

### 3. 贴纸动效：弹出 / 上浮 / 淡出（`getActiveSticker`）

`templates/index.html:1517`：

```
popIn = 0.35, floatUp = 0.4, fadeOut = 0.3
[popIn)             scale = 0.3 + 0.7*back_out(p), rot = -15+15*bp, opacity = pow2_out(p)
[popIn, +floatUp)   opacity = 1, scale = 1, yOff = -18*SK*p
[.., +dur)          opacity = 1, yOff = -18*SK
[+dur, +fadeOut)    opacity = 1-p, scale = 1-0.6*p
```

三段**都能用本仓的关键帧直接表达**（`opacity`/`scale`/`rotation`/`y`
五条通道都接了，见 `compose.rs:287`），所以不用动契约。
实测覆盖率 0 → 75675px → 8719px，与公式吻合。

**这里踩了一个大坑**：关键帧的帧号是**相对图层起点**的
（`schema.rs:732`："相对片段起点，0..=M-1"）。我第一版写了**绝对**帧号，
于是 `keyframes[0].frame = 2809` 远大于该层的 duration，
**一个关键帧都不落在区间里** → 动画完全不生效。
而它**不报错**（v2 的 `validate_timeline_v2` 不查这个越界）、
**抽帧也看不出来**（`opacity` 的 fallback 是 `layer.opacity = 1.0`，
等于"一直满不透明"）。只有对比"有贴纸 vs 无贴纸"才看得出来。

### 4. 字幕/弹幕样式：抄 V-Trim 模板里的**实际值**

V-Trim 的字幕/弹幕样式写在 `templates/index.html:747` 的 CSS 里：

| | V-Trim | 我先前（缺省） |
|---|---|---|
| 字幕字号 | `72px` → `font_ratio = 72/1080` | `0.055`（≈59px） |
| 字幕颜色 | `#dcbda0` | 纯白 |
| 字幕落点 | `cy = CH - 120` → `bottom_margin = 120/1080` | `0.06`（≈65px） |
| 弹幕字号 | `42px` → `font_ratio = 42/1080` | `0.04`（≈43px，碰巧接近） |
| 弹幕颜色 | `#ffffff` | 纯白（碰巧一致） |

实测字幕字色：本仓 `(211,183,159)` / V-Trim `(213,185,162)` / 目标 `(220,189,160)` ✓

**弹幕滚动时长**从 V-Trim 源码推出（`parse/text.rs:74`）：

```rust
let travel = (1080.0 + entry.text.len() as f64 * 32.0) / 150.0;
```

单位秒，`text.len()` 是 **UTF-8 字节数**（一个汉字 3 字节）。
四项实测全部吻合：39字节→15.52s、15字节→10.40s、23字节→12.11s、12字节→9.76s。

### 5. 顺带：每条 camera 自己的 `origin` 要用

V-Trim 每条 camera 都带 `origin`，而且**不一定等于 `layout.liver` 的中心**
—— 样本 B 里就有一条 `origin = "50.00% 50.00%" anchor="center"`
（画中画拉远那一档）。我先前一律用 liver 中心。

### 6. 还做不到的（要动契约）

| 项 | 为什么 |
|---|---|
| 字幕淡入淡出 | `SubtitleStyle` 只有 `font_ratio/bottom_margin/max_lines/color/outline`，**没有动画字段** |
| 弹幕 color / opacity / 淡入淡出 | `DanmakuSpec` 只有 `asset_id/lanes/duration_ms/font_ratio` |
| 字幕描边色与宽度 | 只有 `outline: bool`（V-Trim 是 `12px #403c3b`） |
| 弹幕逐条滚动时长 | 本仓在**轨道级**一个值；V-Trim 每条按文本字节数算 |

实测的对应公式（要实现时照这个抄）：

```
字幕: fadeIn 0.35s, fadeOut 0.18s, 入场从下方 +20px 浮上来
      透明度 opacity = pow2_out(p) / 退场 pow2_in(p)
弹幕: fadeIn 0.3s, fadeOut 0.2s, **基础不透明度 0.9**（不是 1.0）
      滚动 x 从 CW 到 -textW，y = (d.y/1920)*(540*CK)
```

**泳道不能靠拆轨解决**：分配是**每轨各自从 0 开始**
（`overlay.rs:203` 在 track 循环里调 `layout_danmaku`），
拆成多条轨会把每条都塞进第 0 泳道、叠成一坨。

---

## 0b. 第一轮漏掉的四个缺陷（用户一眼就看出来的）

第一版出片"成功"了（6496 帧、时长与源一致、`issues: []`），
但**用户一看就发现四样东西不对**。逐条查完，四个都是真缺陷，
而且**没有一个是自动化检查能抓到的**。

| # | 现象 | 根因 | 性质 |
|---|---|---|---|
| 1 | **原声整个没了** | 音轨只写了音效，**没有任何一层引用视频素材的音频** | 我漏了 |
| 2 | **贴纸全丢** | 查了 `assets/stickers/`（那一层是空的），真文件在 `assets/stickers/ysyk_0.9.0/` | 我查错目录 |
| 3 | **字幕没渲染** | 字幕文件存在、本仓有子系统，我没接 | 我漏了 |
| 4 | 弹幕"没渲染" | 样本 A 的 `danmaku.json` 是 `[]` —— **画面里的弹幕是源像素自带的** | **不是缺陷** |

### 1. 原声：最要命的一个

实测证据（样本 A，第 20-30 秒平均音量）：

| | 平均音量 |
|---|---|
| 源 `clip.mp4` | **-24.2 dB** |
| 我的产物（第一版） | **-91.0 dB**（数字静音） |
| V-Trim 自己的成片 | **-24.3 dB** |
| 我的产物（修好后） | **-24.2 dB** ✅ |

**根因**：本仓的音轨是**显式**的 —— 不写一条引用视频素材的音频层，
就真的没有原声。而 V-Trim 那边原声是**默认就有的底**
（`mute_ranges` 正说明它把原声当成一条可静音的基线）。

**修法**：补一条原声轨，并把 `mute_ranges` 表达成**把原声切段**。
样本 A 的 2 段静音 → 3 段原声层；样本 B 的 3 段 → 3 段。

### 2. 贴纸：查错目录 + `sticker_tag` 是意图词

`assets/stickers/` 下有 `ysyk_0.9.0/`（样本 A，210 个 GIF），
或者**平铺**（样本 B，6 个 GIF）。两种布局都要认。

**更麻烦的一层**：V-Trim 的 `sticker_tag` 是 LLM 的**意图词**
（`搞笑`/`震惊`/`看热闹`），不是贴纸包里的 tag（`笑`/`惊讶`/`害怕`）——
这是两段流程：LLM 挑 tag，`Step2b` 再按 tag 查文件（V-Trim `llm/mod.rs:280`）。

- **样本 A**：`name`/`source` 都是空的（Step2b 没跑）→ 只能同义映射 + 借贴纸包；
- **样本 B**：`name`/`source` **填好了** → 直接用 `name`，最准。

所以转译器**优先用 `name`**，空了才退回按 tag 猜（同义表 + 借用兄弟目录的
`ysyk.toml`，借用前要核对它能对上本地文件）。

### 3. 字幕：接上就行

本仓字幕子系统的输入是 `AssetKind::Subtitle` 素材 + `TrackKind::Subtitle` 轨，
认 `.srt`。转译器直接写这两样。

**注意**：渲染时**必须给 `--font-file`** —— 本仓不内嵌字体、也不猜系统字体，
不给就判失败（`subtitle_font_missing`），而不是静默出一份没有字幕的片子。
这个设计是对的（"看起来成功、其实没有字幕"是最坏的结果）。

### 4. 弹幕：样本 A 不是缺陷，样本 B 要真的接

- **样本 A**：`danmaku.json` 是 `[]`、`danmaku_count: 0`。
  画面右侧那个弹幕面板是**源素材像素的一部分**（录制时就烧进去了）——
  已经在画面里了，重做一遍是错的。
- **样本 B**：`danmaku.json` 有 4 条、`clip.dm.srt` 也有内容 —— **真有独立弹幕数据**。

**样本 B 的弹幕要转格式**：本仓的弹幕轨吃的是 **ASS**
（一条 `Dialogue` 一行，`danmaku::parse_ass_danmaku`），
而 V-Trim 给的是 `clip.dm.srt` —— 一个"SRT 外壳 + 里面塞了 ASS 覆盖标签"
（`{\move(...)}`、`{\pos(...)}`、`{\c&H...}`）的混合体。所以转一道写成 ASS。

**覆盖标签原样留着**：本仓解析时本来就会剥掉，而且 `\move` 的坐标
**故意不参与布局**（落点由 `DanmakuSpec` 与目标尺寸统一算，两端拿同一份）。
留着的好处是"这份 ASS 就是 V-Trim 那份的直译"。

转出来的 ASS 用**绝对路径**登记 —— 它住在工程文件旁边，
而 `--asset-root` 通常指向 clip 目录，用相对 uri 会找不到（实测报 `os error 2`）。

---

## 0b. 第二轮：`shake` 事件与贴纸尺寸

样本 B 曝出两个新问题，都是**只有量过才知道**的。

### `shake` 是一个独立事件类型（样本 B 才出现）

形状 `{ time, end, intensity, sfx? }`。注意它跟 `camera` 里的 `handheld=true`
**不是一回事**：

- `handheld` 是"整段镜头轻微晃"（持续、幅度小）；
- 独立 `shake` 事件是"这一下猛地一晃"（短、幅度大）。

本仓两者都落在 `shake` 特效上，区别只在**时间窗**与**幅度**。
`intensity`（0.4/0.6）是 V-Trim 的观感档位，本仓 `amount` 是像素位移比例 ——
**不是同一把尺子**，所以按 0.05 缩放并报 `CLAMP`。

另外一个坑：`seed` 参数有范围（**0..=4096**，校验会拦）。
直接拿帧号会在长切片上越界（样本 B 有一条 start 是第 6018 帧）。
折进范围即可（`startFrame % 4096`）。

### 贴纸尺寸：猜的和真的是两回事

我第一版猜 `STICKER_SIZE = 220`，实测 V-Trim 成片里量出来约 **290px**。
去 V-Trim 源码里找到了确切规则（`process/placement.rs:36-92`）：

```rust
pub const STICKER_SIZE: f64 = 280.0;           // 盒子边长基准
pub fn sticker_scale(&self) -> f64 { min(W,H)/1080.0 }   // 短边基准
pub fn sticker_size(&self) -> f64 { STICKER_SIZE * self.sticker_scale() }
```

而且 `ev.x/ev.y` 是**这个盒子的左上角**（同文件 276 行注释）——
我的"左上角"理解是对的，但**尺寸猜错了**，于是"挪半个盒子"也挪错了。

修法：盒子 = `280 * min(W,H)/1080`；素材是 500×500 原图，所以
`scale = 盒子 / max(素材宽, 素材高)`。改完实测与 V-Trim 对齐。

`280/1080 = 25.9%` —— 记这个比例比记"280px"更通用（竖屏短边也是 1080）。

---

## 0c. 转译器自己的错（都是写出帧/量过才发现的）

### `ffprobe` 的 CSV 字段顺序**不是你写的顺序**

    -show_entries stream=nb_read_frames,width,height -of csv=p=0
    实际吐出：500,500,32        <- width,height,nb_read_frames

我按"我写的顺序"解，于是**把宽当成帧数、把帧数当成高** ——
而且**看起来挺合理**（500 像个像样的帧数），不盯着数据看根本发现不了。
教训：`-of csv` 的字段顺序由 ffprobe 定，要用 `default=nw=1` 先看清。

### 偏移公式多乘了一次 scale（第一轮）

渲染器几何是写死的（`render/compose.rs:169`）：

    p_out = (p_src - c_src) * scale + c_out + offset

我第一版写 `(0.5 - focus) * size * scale` —— **两边都乘了 scale**。
表现是画面右侧一条白带（1.5 倍近景时偏移被放大成 1.5×1.5）。

### 首尾相接的运镜产生重复关键帧

运镜事件首尾相接（0.9→5.0、5.0→10.7…），前一条的终点与后一条的起点落在
**同一帧**，写出重复的 `target@frame`；`channel_from` 取后一个，
表现是**一段运镜被吃掉**。修法：按 `frame|target` 去重。

### 组装轨道时把字段丢了

`loop_source` 在构造 `stickerLayers` 时写了，但**组装轨道时重新拼对象、
没带过来** —— 表现是校验照旧报错，而看代码"明明写了"。
**纯手工拼对象最容易出的错。**

### 音效文件不存在却生成了引用

`polish.toml` 写 `poka01`/`poka02`，而 `sfx/` 下只有 `poka.mp3`。
按名字生成引用会得到指向不存在文件的音轨，而它在工程校验里**是合法的**
（只看"有没有登记"、不看"文件在不在"），要到渲染时才炸。
修法：转译时当场查盘。

### `asset_timebases()` 把帧数丢了（**契约层**的错，不是转译器的）

这一条值得单独记，因为它差点让 `loop_source` 白做。

`AssetTimebases` 我加了 `frame_count`（循环取模要周期），
求值层也改成走 `source_frame_looped()` —— 单独出一帧看着是对的。
但**整片渲染到第 193 个时间线帧就炸**：

    source_decode_failed: 源 ...呆(贴纸)_1.gif 在第 32 帧就结束了，而工程要第 32 帧

根因：`ProjectDoc::asset_timebases()` 只登记了时间基

```rust
table.insert(asset.id.clone(), timebase);   // frame_count 默认 None
```

于是 `AssetTimebases::frame_count()` 永远 `None` → 循环静默退化成不循环 →
原始帧号一路涨到 32、越界。

**为什么单帧测试抓不到**：循环点在 local frame 192 之后
（32 素材帧 × 6 = 192 时间线帧），而抽帧抽的是 2928（local 119），
**还没到循环点**。差一点又掉进"看起来成功"的坑里。

修法：改成 `insert_with_count(...)`，并补 3 条用例钉住
（含"没登记时间基的资产仍然不进表"这条老行为）；
变异测试确认能把 `left: None / right: Some(32)` 抓出来 ——
**"两个入口登记的东西不一致"是这类错的经典来源。**


---

## 5. 顺带补的契约缺口：`Layer.loop_source`

修贴纸时撞上一个**真的契约缺口**。

贴纸是**短动图铺长区间**：实测一张 12 帧的 GIF 要覆盖 224 个时间线帧（3.7 秒）。
而校验 `source_range_exceeded` 要求图层落在素材范围内 ——
**契约里没有"循环"这个概念**，于是只有两条歪路：

1. **谎报 `frame_count`** —— 校验过，但渲染时读不存在的帧；
2. **缩短贴纸层** —— 动图放完就消失，与 V-Trim 行为不同。

两条都是**用错的形状去套**。所以加了 `Layer.loop_source: bool`（默认 `false`）：

- 默认不循环 —— 静默循环会把"素材长度配错了"变成一个看不出来的错；
- 契约校验在 `loop_source` 为真时放行；
- 求值层走 `source_frame_looped()`，两端共用同一份换算。

**这里的第二个坑**：我第一版实现是
`source_in + (raw - source_in).rem_euclid(count)`（"从 source_in 起一段循环"），
它会算出**素材里不存在的帧号**（`source_in=3`、6 帧素材时给出 `6,7,8`）。
正确的语义是**整张素材循环**：`raw.rem_euclid(count)`。
这个错是写用例时才发现的 —— 6 条用例里有 1 条专门钉这个不变量
（"循环后永远落回素材范围内"），变异测试确认它能抓住。

---

## 6. 最终转译结果

    node tools/polish-to-dhampir.mjs "<clip 目录>" --out out/vtrim/translated.doc.json

| | 样本 A | 样本 B |
|---|---|---|
| 轨道 / 素材 | 10 / 6 | 9 / 10 |
| 运镜 | 19 | 14 |
| 贴纸 | 5 ✅ | 4 ✅ |
| shake | — | 3 ✅ |
| 音效 | 3（2 条转成） | 3 ✅ |
| 原声 | 3 段 ✅ | 3 段 ✅ |
| 字幕 | 24 条 ✅ | 19 条 ✅ |
| 弹幕 | —（源头就没有） | 4 条 ✅ |
| `probe` | **0 error / 0 warning** | **0 error / 0 warning** |
| 全片出片 | 6496 帧 ✅ | 6863 帧 ✅ |

### 样本 B 的全片读数（收口证据）

```
encoded_frames: 6863      seconds: 114.383     1920x1080 h264 + aac
issues: []                failed: false
danmaku_drawn: 1924       danmaku_dropped: 0
lines_drawn:   4590       lines_dropped:   0     <- 字幕
source_samples_read: 4890752   mixed_samples: 241152   clipped: 89
overlaps: ["a0[voice-0] 与 a1[sfx-0-suspicion1] 在第 222 帧叠上，相加了 48000 个采样点",
           "a0[voice-2] 与 a1[sfx-2-suspicion3] 在第 5772 帧叠上，相加了 48000 个采样点"]
墙钟 142.9 秒（114.4 秒的片子 → 快于实时）
```

**音量对账**（第 20-30 秒平均）：

| | 平均音量 |
|---|---|
| 我的产物 | **-25.7 dB** |
| 源 `clip.mp4` | **-25.7 dB** |
| V-Trim 成片 | **-25.8 dB** |


---

## 7. `camera` 是**两种不同的运镜**（第一轮就发现，仍然成立）

### V-Trim 的 camera

`layout.liver` 定义主播区域（自由选框）。`camera` 放大的是**那一块**，
四周（游戏画面、弹幕面板）留在原地。所以 V-Trim 的成片**永远铺满**。

### 本仓的 `Transform.scale`

放大的是**整个图层**（整帧）。要把主播移到中心就必须平移，
**一平移就露出边界** —— 实测 `scale=1.3` 时右侧露出 **622px 空带**。

### 处理

夹住偏移让画面始终铺满：

    halfW  = (width * factor - width) / 2
    shiftX = clamp(-(focus.x - 0.5) * width * factor, -halfW, halfW)

代价是"聚焦"弱一些，换**画面完整**。报成 `CLAMP` 而不是假装没差。

**一个实测出来的副作用**：夹住之后画面比 V-Trim 更"推进" ——
所以**源素材像素里烧着的东西会被裁掉**（样本 B 的日文歌词字幕在左下角，
V-Trim 的成片保留着，我这里裁出去了）。这不是 bug，是"整帧缩放 vs 分区缩放"
这个语义差的直接后果，也是为什么补"分区变换"最有价值。

---

## 8. 剩余 DROP

| 项 | 为什么 |
|---|---|
| `cover` | 封面是**静态图产出**（标题 4~6 行 + 26 个参数），与逐帧合成不是同一条路 |
| `style.subtitle` | V-Trim 的字幕样式（字体/颜色/描边/入场动画 `cycle-5`），本仓字幕有独立样式契约 |
| `style.danmaku` | 本仓弹幕只渲染成统一颜色与缺省泳道（素材里的 `\c` 高亮标签会被剥掉） |
| `sfx/poka02.mp3`（样本 A） | 文件不存在 |

### 仍需注意的 CLAMP（观感差异是真的）

- **运镜偏移被夹**：见 §7（样本 A 24 条、样本 B 12 条）。
- **`shake intensity` 缩放**：V-Trim 的档位 vs 本仓的像素位移比例。
- **`sfx` 单条音量**：本仓契约**还没有每层增益字段**（T13 的 `gain` 固定 1.0）。
  样本 B 三条音效的音量（0.4/0.5/0.45）都会丢。
- **`handheld=true`**（样本 A）：转成整层 `shake` 特效。

以及两条 NOTE：`[sfx] volume = 0.1` 是**混音总线音量**（本仓没有总线增益，
转译后音效满音量播放）；`shake_enabled = true` 在 V-Trim 里是
"按音效强弱触发画面抖动"，本仓没有这条链路。

---

## 9. 对"这个底座能不能对接 V-Trim"的回答

**能。两轮下来缺的四块里，三块是转译器的问题（已修），一块是契约缺口（已补）。**

### 已经对得很好的部分

- **运镜** → `keyframes`：两轮共 33 条全部转过去，占比最大的一类。
- **原声 / 字幕 / 弹幕 / 贴纸 / 音效 / shake**：都能转，且**渲染出来是对的**。
- **转译产物直接通过校验、直接出片**，零手工修补。

### 仍然缺的一块（明确归本仓）

**分区变换** —— 只对某个矩形应用缩放。这是唯一一处"语义差"而非"没有"，
也是差距最大的一处，需要 `Composite` 层支持。
补上它，`camera` 才能与 V-Trim 完全等价，源像素里的内容也不会被误裁。

### 两个仍然没定的判断

1. **每层音频增益**：契约里 `gain` 字段 T13 已经有了，但没接进序列化，
   转译器只能丢掉音量。这是个小改动。
2. **`mute_ranges`**：现在表达成"切出 N 段不连续原声"，语义等价、时长一致。
   但它到底是"底座该会的表达"还是"V-Trim 该在写工程前就裁好"，
   按设计文档 §7 的边界仍可争议。

---

## 10. 复现

```bash
node tools/polish-to-dhampir.mjs "<clip 目录>" --out out/vtrim2/translated.doc.json

target/debug/dhampir.exe probe --project out/vtrim2/translated.doc.json

# 出帧：--asset-root 指向 clip 目录；有字幕轨时 --font-file 是必填的
target/debug/dhampir.exe frame --project out/vtrim2/translated.doc.json \
    --frame 2928 --out out/vtrim2/f --asset-root "<clip 目录>" \
    --font-file C:\Windows\Fonts\Deng.ttf

target/debug/dhampir.exe render --project out/vtrim2/translated.doc.json \
    --from 0 --to 6862 --out out/vtrim2/full.mp4 --asset-root "<clip 目录>" \
    --font-file C:\Windows\Fonts\Deng.ttf
```

弹幕的 ASS 会写到**工程文件旁边**（用绝对路径登记）。


---

## 0. 第二轮的四个缺陷（**第一版漏掉/做错的**）

第一版出片"成功"了（6496 帧、时长与源一致、`issues: []`），
但**用户一看就发现四样东西不对**。逐条查完，四个都是真缺陷，
而且**没有一个是自动化检查能抓到的** —— 这一节值得先看。

| # | 现象 | 根因 | 性质 |
|---|---|---|---|
| 1 | **原声整个没了** | 音轨只写了音效，**没有任何一层引用视频素材的音频** | 我漏了 |
| 2 | **贴纸全丢** | 我查了 `assets/stickers/`（空），真文件在 `assets/stickers/ysyk_0.9.0/` | 我查错目录 |
| 3 | **字幕没渲染** | 字幕文件存在、本仓有子系统，我没接 | 我漏了 |
| 4 | 弹幕"没渲染" | `danmaku.json` 是 `[]` —— **画面里的弹幕是源像素自带的** | **不是缺陷** |

### 1. 原声：最要命的一个

实测证据：

| | 第 20-30 秒平均音量 |
|---|---|
| 源 `clip.mp4` | **-24.2 dB** |
| 我的产物（第一版） | **-91.0 dB**（数字静音） |
| V-Trim 自己的成片 | **-24.3 dB** |

**根因**：本仓的音轨是**显式**的 —— 不写一条引用视频素材的音频层，
就真的没有原声。而 V-Trim 那边原声是**默认就有的底**
（`mute_ranges` 正说明它把原声当成一条可静音的基线）。

**修法**：补一条原声轨，并把 `mute_ranges` 表达成**把原声切段**
（V-Trim 是"掐掉不要"，本仓没有"静音区间"，但"N 段不连续的原声"语义等价）。
实测这份工程的 2 段静音产生 3 段原声层。

### 2. 贴纸：我查错了目录

`assets/stickers/` **不是空的**，里面有 `ysyk_0.9.0/`（210 个 GIF）。
我第一版只看了 `assets/stickers/` 这一层，就得出"没有文件"的结论。

但这里还有个真问题：**没有 `ysyk.toml`**（tag → 文件的映射表），
而 `polish.toml` 里的 `sticker_tag`（`搞笑`/`震惊`/`看热闹`）
是 LLM 的**意图词**，不是贴纸包里的 tag（`笑`/`惊讶`/`害怕`）——
这是 V-Trim 的两段流程：LLM 挑 tag，`Step2b` 再按 tag 查文件
（见 V-Trim `llm/mod.rs:280`）。这份工程的 `name`/`source` 都是空的，
说明 **Step2b 从没跑过**。

**修法**：
1. 同义词表把意图词映到真实 tag（`搞笑`→`笑`、`震惊`→`惊讶`、`看热闹`→`吃`）；
2. `ysyk.toml` 本切片没有，就在**别的切片目录**里借一份，
   借之前**核对它能对上本地文件**（引用 163 个文件、本地 161 个都在才认）。
   搜索范围要逐级放大 —— 同一次录制的 7 个切片**全都没有** toml，
   只找同级兄弟会一个都借不到（第一版就是这个错）。

5 张贴纸全部转出。

### 3. 字幕：接上就行

本仓字幕子系统的输入是 `AssetKind::Subtitle` 素材 + `TrackKind::Subtitle` 轨，
认 `.srt`。转译器直接写这两样，24 条字幕全部接进来。

### 4. 弹幕：**不是缺陷**，要说准

`data/danmaku.json` 是 `[]`，`clip-meta.json` 里 `danmaku_count: 0` ——
**没有独立弹幕数据**。画面右侧那个弹幕面板是**源素材像素的一部分**
（录制时就烧进去了），第一轮渲染里**已经在画面里了**。

所以这条不是"漏了"，是"没有东西可转"。真正的弹幕轨要等有弹幕数据的切片。

---

## 5. 顺带补的契约缺口：`Layer.loop_source`

修贴纸时撞上一个**真的契约缺口**，值得单独记。

贴纸是**短动图铺长区间**：实测一张 12 帧的 GIF 要覆盖 224 个时间线帧（3.7 秒）。
而校验 `source_range_exceeded` 要求图层落在素材范围内 ——
**契约里没有"循环"这个概念**，于是只有两条歪路：

1. **谎报 `frame_count`** —— 校验过，但渲染时读不存在的帧；
2. **缩短贴纸层** —— 动图放完就消失，与 V-Trim 行为不同。

两条都是**用错的形状去套**。所以加了 `Layer.loop_source: bool`（默认 `false`）：

- 默认不循环 —— 静默循环会把"素材长度配错了"变成一个看不出来的错；
- 契约校验在 `loop_source` 为真时放行；
- 求值层走 `source_frame_looped()`，两端共用同一份换算。

**这里的第二个坑**：我第一版实现是
`source_in + (raw - source_in).rem_euclid(count)`（"从 source_in 起一段循环"），
它会算出**素材里不存在的帧号**（`source_in=3`、6 帧素材时给出 `6,7,8`）。
正确的语义是**整张素材循环**：`raw.rem_euclid(count)`。
这个错是写用例时才发现的 —— 6 条用例里有 1 条专门钉这个不变量
（"循环后永远落回素材范围内"），变异测试确认它能抓住。

---

## 6. 最终转译结果（第二轮）

    node tools/polish-to-dhampir.mjs "<clip 目录>" --out out/vtrim/translated.doc.json

**10 条轨 / 6 个素材 / 丢失 4 / 降级 24 / 注意 18**（第一版是 3 轨 / 12 丢）。

| 事件 | 数量 | 结果 |
|---|---|---|
| `camera` | 19 | ✅ 90 个关键帧（语义差见 §7） |
| `sticker` | 5 | ✅ **本轮接通**（借贴纸包 + 同义映射 + 循环） |
| `sfx` | 3 | ⚠️ 2 条转成（`poka02` 文件不存在） |
| `overlay` | 1 | ✅ |
| `zoom_bounce` | 1 | ✅ |
| 原声 | 3 段 | ✅ **本轮接通**（2 段静音切出来） |
| 字幕 | 24 条 | ✅ **本轮接通** |

**`probe`：0 error / 0 warning。**

---

## 7. `camera` 是**两种不同的运镜**（第一轮就发现，仍然成立）

### V-Trim 的 camera

`layout.liver` 定义主播区域（自由选框 `78.78%~94.15%` × `46.37%~77.51%`）。
`camera` 放大的是**那一块**，四周（游戏画面、弹幕面板）留在原地。
所以 V-Trim 的成片**永远铺满**。

### 本仓的 `Transform.scale`

放大的是**整个图层**（整帧）。要把 86% 处的主播移到中心就必须平移，
**一平移就露出边界** —— 实测 `scale=1.3` 时右侧露出 **622px 空带**。

### 处理

夹住偏移让画面始终铺满：

    halfW  = (width * factor - width) / 2
    shiftX = clamp(-(focus.x - 0.5) * width * factor, -halfW, halfW)

代价是"聚焦"弱一些，换**画面完整**。报成 `CLAMP` 而不是假装没差。
24 条 CLAMP 里大部分是这个。

---

## 8. 转译器自己的错（写出帧才发现）

### 8.1 偏移公式多乘了一次 scale

渲染器几何是写死的（`render/compose.rs:169`）：

    p_out = (p_src - c_src) * scale + c_out + offset

我第一版写 `(0.5 - focus) * size * scale` —— **两边都乘了 scale**。

### 8.2 首尾相接的运镜产生重复关键帧

运镜事件首尾相接（0.9→5.0、5.0→10.7…），前一条的终点与后一条的起点落在
**同一帧**，写出重复的 `target@frame`；`channel_from` 取后一个，
表现是**一段运镜被吃掉**。修法：按 `frame|target` 去重。

### 8.3 组装轨道时把字段丢了

`loop_source` 在构造 `stickerLayers` 时写了，但**组装轨道时重新拼对象、
没带过来** —— 表现是校验照旧报错，而看代码"明明写了"。
**纯手工拼对象最容易出的错。**

### 8.4 音效文件不存在却生成了引用

`polish.toml` 写 `poka01`/`poka02`，而 `sfx/` 下只有 `poka.mp3`。
按名字生成引用会得到指向不存在文件的音轨，而它在工程校验里**是合法的**
（只看"有没有登记"、不看"文件在不在"），要到渲染时才炸。
修法：转译时当场查盘。

---

## 9. 剩余 DROP（4 条）

| 项 | 为什么 |
|---|---|
| `sfx/poka02.mp3` | 文件不存在（同目录里没有） |
| `cover`（4 行标题 + 26 参数） | 封面是**静态图产出**，与逐帧合成不是同一条路 |
| `style.subtitle` | V-Trim 的字幕样式参数，本仓字幕有独立样式契约，没逐项对应 |
| `style.danmaku` | 本切片没有弹幕数据，样式无从应用 |

### 仍需注意的 CLAMP（观感差异是真的）

- **运镜偏移被夹**（多条）：见 §7。
- **`handheld=true` ×3**：V-Trim 是"镜头手持感"，本仓转成整层 `shake` 特效。
- **`sfx` 单条音量 ×2**：本仓契约**还没有每层增益字段**（T13 的 `gain` 固定 1.0）。

以及两条 NOTE：`[sfx] volume = 0.1` 是**混音总线音量**（本仓没有总线增益，
转译后音效满音量播放）；`shake_enabled = true` 在 V-Trim 里是
"按音效强弱触发画面抖动"，本仓没有这条链路。

---

## 10. 对"这个底座能不能对接 V-Trim"的回答

**能。第一轮缺的四块里，三块是转译器的问题（已修），一块是契约缺口（已补）。**

### 已经对得很好的部分

- `camera` → `keyframes`：**19 条全部转过去**，占比最大的一类。
  T8 的"运镜就是 Transform 上的关键帧"这个判断在真数据上成立。
- 原声 / 字幕 / 音效 / overlay / zoom_bounce：干净，零手工修补。
- **转译产物直接通过校验、直接出片。**

### 仍然缺的一块（明确归本仓）

**分区变换** —— 只对某个矩形应用缩放。这是唯一一处"语义差"而非"没有"，
也是差距最大的一处，需要 `Composite` 层支持。
补上它，`camera` 才能与 V-Trim 完全等价。

### 一个仍然没定的判断

`mute_ranges` 现在表达成"切出 N 段不连续原声"，语义等价、时长一致。
但它到底是"底座该会的表达"还是"V-Trim 该在写工程前就裁好"，
按设计文档 §7 的边界仍可争议 —— 我倾向后者，但这轮先按前者做通了。

---

## 11. 复现

```bash
node tools/polish-to-dhampir.mjs "<clip 目录>" --out out/vtrim/translated.doc.json
target/debug/dhampir.exe probe --project out/vtrim/translated.doc.json
target/debug/dhampir.exe frame  --project out/vtrim/translated.doc.json \
    --frame 900 --out out/vtrim/frame900 --asset-root "<clip 目录>"
target/debug/dhampir.exe render --project out/vtrim/translated.doc.json \
    --from 0 --to 6495 --out out/vtrim/full.mp4 --asset-root "<clip 目录>"
```

`--asset-root` 必须指向 clip 目录（素材路径都相对它）。


---

## 1. 输入长什么样

| 项 | 值 |
|---|---|
| `clip.mp4` | h264 1920×1080 **60fps**，6496 帧，108.27 秒，带 aac 音轨 |
| `polish.toml` | 570 行，7 个 segment、**34 个事件** |
| 事件构成 | camera ×19、sticker ×5、sfx ×3、overlay ×1、zoom_bounce ×1 |
| `sfx/` | 4 个 mp3（`poka` / `question` / `surprise` / `Ikkyu_san`） |
| `assets/stickers/` | **空目录** |
| `data/danmaku.json` | **2 字节**（空数组） |

事件类型分布**极不均衡**：运镜占一半以上，而 sticker 全部落在没有素材文件的目录上。

---

## 2. 转译结果

    node tools/polish-to-dhampir.mjs "<clip 目录>" --out out/vtrim/translated.doc.json

产出：3 条轨（1 视频 + 1 调整图层 + 1 音轨）、3 个素材、
19 条运镜（展开成 90 个关键帧）、1 条 overlay。

**`probe` 校验：0 error / 0 warning。**
**全片渲染：6496 帧 1920×1080 出片成功，`issues: []`。**

### 逐类事件的去向

| V-Trim 事件 | 数量 | 转译成 | 结论 |
|---|---|---|---|
| `camera` | 19 | `Layer.keyframes` 驱动 `scale`/`x`/`y` | ✅ 语义**接近**，见 §3 |
| `overlay`（gradient） | 1 | `Effect{kind:overlay}` 挂在调整图层 | ✅ 干净 |
| `zoom_bounce` | 1 | `Effect{kind:zoom_bounce}` + `transient` 窗口 | ✅ 干净 |
| `sfx` | 3 | 音轨图层引用 mp3 | ⚠️ 1 条（`poka02`）**文件不存在** |
| `sticker` | 5 | ❌ 无 | **5 条全丢**（没有图片文件） |

---

## 3. 最重要的发现：`camera` 是**两种不同的运镜**

这一条值得单独写，因为第一版我把它当成"转过去了"，而**出帧一看不是**。

### V-Trim 的 camera

`layout.liver` 定义主播区域（自由选框 `78.78%~94.15%` × `46.37%~77.51%`）。
`camera` 放大的是**那一块**，四周（游戏画面、弹幕面板）留在原地。

所以 V-Trim 的成片**永远铺满** —— 抽它的第 10/15/20 秒核对过，
游戏画面始终完整，人物始终在右侧。

### 本仓的 `Transform.scale`

放大的是**整个图层**（整帧），`origin` 是画面中心。
要把 86% 处的主播移到中心，就必须平移，**一平移就露出边界**。

实测：`scale=1.3` + 把 `86.47%` 移到中心 → 右侧露出 **622px 空带**。

### 处理

夹住偏移，让画面始终铺满：

    halfW  = (width * factor - width) / 2
    shiftX = clamp(-(focus.x - 0.5) * width * factor, -halfW, halfW)

代价是"聚焦"弱一些，换**画面完整**。报成 `CLAMP` 而不是假装没差。

**这是设计文档 §10 那条边界的具体体现**：V-Trim 的 camera 里混着
"这个直播间怎么排版"（liver 区域的定义），而本仓只管"像素怎么算出来"。
要**完全**对齐，本仓需要"分区缩放"（只对某个矩形应用变换），
那是 `Composite` 管线里的事 —— 现在没有。

---

## 4. 抓到的三类"转译器自己的错"

都是**先写错、出帧才发现**的，记下来因为它们很容易再犯。

### 4.1 偏移公式多乘了一次 scale

渲染器的几何是写死的（`render/compose.rs:169`）：

    p_out = (p_src - c_src) * scale + c_out + offset

我第一版写 `(0.5 - focus) * size * scale` —— **两边都乘了 scale**，
于是 1.5 倍近景的偏移被放大成 1.5×1.5。表现是右侧一条白带。

正确：`offset = -scale * (focus - center) * size`。

### 4.2 首尾相接的运镜产生重复关键帧

运镜事件是**首尾相接**的（0.9→5.0、5.0→10.7……），
于是前一条的终点与后一条的起点落在**同一帧**上，写出重复的 `target@frame`。
`channel_from` 对重复帧取"后一个"，表现是**一段运镜被吃掉**。

修法：按 `frame|target` 去重。

### 4.3 音效文件不存在却生成了引用

`polish.toml` 里写着 `poka01`/`poka02`，而 `sfx/` 下只有 `poka.mp3`。
直接按名字生成引用会得到一条**指向不存在文件**的音轨 ——
而它在工程校验里**是合法的**（校验只看"有没有登记"，不看"文件在不在"），
要到渲染时才炸。**那正是"看着成功、其实没验"。**

修法：转译时当场查盘，缺了就报 `DROP`。

---

## 5. 转不了的部分（12 DROP / 24 CLAMP / 14 NOTE）

### DROP —— 本仓没有对应概念

| 项 | 为什么 |
|---|---|
| `sticker` ×5 | `assets/stickers/` **是空的**，没有图片文件 |
| `mute_ranges`（16.90 秒） | **本仓没有"压掉原声"的表达** —— 转译后这些内容仍在时间线上，成片会变长 16.9 秒 |
| `cover`（4 行标题 + 26 个参数） | 封面是**静态图产出**，与逐帧合成不是同一条路 |
| `filter.danmaku`（7 个词） | 弹幕素材是空的（`danmaku.json` 只有 2 字节） |
| `style.subtitle` / `style.danmaku` | 本仓字幕/弹幕走独立子系统，转译器这一版没接 |
| `stage.bg_mode` | 本仓没有"背景模式"这一层 |

### CLAMP —— 有概念但表达力不够

- **`handheld=true` ×3**：V-Trim 是"镜头手持感"，本仓转成整层 `shake` 特效
- **`sfx` 单条音量 ×2**：本仓契约**还没有每层增益字段**（T13 的 `gain` 固定 1.0）
- **运镜偏移被夹**（多条）：见 §3

### NOTE —— 语义差但转过去了

最值得注意的三条：

1. **`[sfx] volume = 0.1`** 是**混音总线音量**，本仓没有总线增益概念。
   转译后音效将以满音量播放 —— 比 V-Trim 响约 10 倍。
2. **`shake_enabled = true`**：V-Trim 会**按音效强弱触发画面抖动**，
   本仓没有"音效驱动画面"这条链路（两个子系统之间没有连线）。
3. **`source.subtitles = clip.srt`** 存在且本仓有字幕子系统，
   只是转译器这一版没接 —— 这是**最容易补上的一块**。

---

## 6. 对"这个底座能不能对接 V-Trim"的回答

**能，但缺三块，且缺的位置很明确。**

### 已经对得很好的部分

- `camera` → `keyframes`：**19 条运镜全部转过去了**，这是占比最大的一类。
  T8 做的"运镜就是 Transform 上的关键帧"这个判断在真数据上成立。
- `overlay` / `zoom_bounce` / `sfx` → 干净，一个参数都不用猜。
- **转译产物直接通过校验、直接出片**，零手工修补。

### 缺的三块（按"补起来值不值"排序）

1. **分层/分区变换**（对应 `camera` 的完全等价）。
   现在只有整层 `Transform`。要做 V-Trim 那种"只放大 liver 那块"，
   需要 `Composite` 层支持"对矩形区域应用变换"。
   **这是唯一一处"语义差"而非"没有"的地方**，也是差距最大的一处。

2. **每层音频增益**。`polish.toml` 给每条音效单独配音量，
   契约里 `gain` 字段已经有了（T13 加的）**但没有接进契约的序列化**，
   转译器只能丢掉它。这是个**小改动**。

3. **"压掉某段原声"**（`mute_ranges`）。
   16.9 秒的内容差异，是**成片长度级别**的影响，不是观感级别。
   V-Trim 的做法是"在时间轴上掐掉"，本仓的音轨只有"播什么"。

### 一个必须说清的判断

上面三块里，**第 3 块最可能是"归 V-Trim"而不是"归底座"**。
理由：`mute_ranges` 是**时间轴的裁剪决策**（"这段不要"），
与"像素怎么算"无关 —— 按设计文档 §7 的边界（"凡是这个直播间怎么排版归 V-Trim"），
它更像"V-Trim 应该在写工程文件之前就把区间裁好"，
而不是"底座要学会压原声"。

**第 1 块则相反，明确归本仓**：怎么把一个矩形区域的像素放大，是纯渲染问题。

---

## 7. 复现

```bash
# 转译（会打印完整的丢弃/降级/注意清单）
node tools/polish-to-dhampir.mjs "<clip 目录>" --out out/vtrim/translated.doc.json

# 校验
target/debug/dhampir.exe probe --project out/vtrim/translated.doc.json

# 出一帧看（--asset-root 指向素材目录）
target/debug/dhampir.exe frame --project out/vtrim/translated.doc.json \
    --frame 900 --out out/vtrim/frame900 --asset-root "<clip 目录>"

# 出片
target/debug/dhampir.exe render --project out/vtrim/translated.doc.json \
    --from 0 --to 6495 --out out/vtrim/full.mp4 --asset-root "<clip 目录>"
```

`--asset-root` 必须指向 clip 目录，因为 `polish.toml` 里的素材路径
（`clip.mp4`、`sfx/*.mp3`）都是**相对 clip 目录**的。
