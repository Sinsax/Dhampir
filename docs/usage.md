# dhampir 使用说明

这份文件回答一个问题：**怎么用它**。任务导向 —— 按"我想做什么"分节，不按源码结构分。

| 我想 | 去 |
|---|---|
| 五分钟跑通 | [quickstart.md](quickstart.md) |
| 承诺边界、入口地图 | [api.md](api.md) |
| 为什么这样设计 | [../plan/video-editor-plan.md](../plan/video-editor-plan.md) |
| 现在做到哪了 | [../plan/next-steps.md](../plan/next-steps.md) |

**`dhampir --help` 永远比这份文档新。** 本文是导览，子命令的权威说明在它自己的帮助里。

---

## 一、先决条件

| 需要 | 用途 | 没有它会怎样 |
|---|---|---|
| **Rust 1.97.0** | 全部构建 | `rust-toolchain.toml` 自动切换，装好 rustup 即可 |
| **ffmpeg / ffprobe** | 素材探测、字幕栅格化、出片编码 | `info` / `gop` / `render` 直接失败 |
| **GPU（Vulkan/DX12/Metal）** | `frame` / `render` 的合成 | `probe` 能跑，出图不能 |
| **字体文件** | 烧进画面的字幕 | 工程有字幕轨时 `frame` / `render` **判失败**（`subtitle_font_missing`） |
| Chrome / Edge | 浏览器预览 | 只有预览用得到 |

**本仓不内嵌字体、也不猜系统字体** —— 要烧字就自己给 `--font-file`。
静默出一份没有字幕的片子比报错难查得多。

---

## 二、我想看看工程对不对（不要 GPU、不要 ffmpeg）

```bash
dhampir probe --project fixtures/sample-project.doc.json
```

打印 DocIssues。**退出码就是结论**：`0` 过 / `2` 有 error（用户能改）。

配套的只读命令：

```bash
dhampir info --asset clip.mp4        # 尺寸 / 帧数 / 时间基 / GOP 长度
dhampir gop  --asset clip.mp4        # GOP 切片表（顺序解码的定位依据）
dhampir library --project P.json     # 每个资产被引用了几次
```

---

## 三、我想出一帧或一段图

```bash
# 单帧
dhampir frame --project P.json --frame 42 --out ./out

# 一段（一张一帧）
dhampir frame --project P.json --from 0 --to 29 --out ./out
```

**`--frame` 与 `--from/--to` 互斥**：同时给退 2，而不是让其中一个悄悄赢。

两个缺省**刻意不对称**：

| 只给 | 出哪几帧 |
|---|---|
| `--from 3` | 第 3 帧**到结尾** |
| `--to 5` | 第 0 帧**到第 5 帧** |
| 都不给 | 退 2 —— **不替你猜** |

---

## 四、我想出一份 mp4

```bash
dhampir render --project P.json --from 0 --to 89 --out out.mp4
```

**stdout 是 NDJSON 进度流**（`start` → 若干 `progress` → `done`），不是给人读的日志；
诊断一律走 stderr。所以可以这样用：

```bash
dhampir render --project P.json --from 0 --to 89 --out out.mp4 | jq -c 'select(.event=="done")'
```

### 4.1 声音：默认有，除非你明确说不要

工程里有音轨就**默认出声音**。`--no-audio` 明确不要 —— 它存在的理由是
**与引入音频之前逐字节相同**，排查声音问题时这是该拿来对照的那一份。

### 4.2 字幕有**两种口径**，互相独立

| 开关 | 做什么 | 要什么 |
|---|---|---|
| `--font-file <ttf/ttc/otf>` | **烧进画面** | 字体 + GPU。工程有字幕轨时少给就**判失败** |
| `--font-bold-file <ttf/ttc/otf>` | 上面那份的**粗体**（`font_weight >= 600` 时用） | 可选。不给就退回 `--font-file` |
| `--font-dir <目录>` | **按字体族名找字体**的目录 | 可选。工程里写了 `font_family` 时才需要 |
| `--subtitle-out <文件>` | 另存**侧挂文件** | 什么都不要。**只有 render 认**这个开关 |

**字体是显式给的，不猜系统字体** —— 本仓故意不做"字体名 -> 系统文件"的解析：
同一份工程在两台机器上挑到不同字体，是"两端可比"最难查的一类失败。
`--font-file` 就是"这一趟用这个"。

##### 字体怎么定位：两条来源、一个名字（契约）

烧字这条路上，**字体来源有两条**，按下面的优先级取：

| 顺序 | 来源 | 说明 |
|---|---|---|
| 1 | **`--font-dir` + 工程里的 `font_family`** | 按**族名**在那个目录里找文件 |
| 2 | **`--font-file`** | 点名一份，**兜底** |

`font_family` 是**工程契约里的字段**（`SubtitleStyle::font_family` /
`DanmakuSpec::font_family`，见 `schema/*.json`），它的语义一直是
**"从你给我的目录里按这个名字找"** —— 不是"去系统里猜"。

**为什么要按名字找、而不是只要一份文件**：libass 要的是**家族名**
（它不认文件路径），而"把一份字体文件交给引擎"这件事在 libass 里
表达不成"文件路径"，只能表达成"名字 + 去哪找"。
`--font-dir` 就是那个"去哪找"，`font_family` 就是那个"名字"。

**名字推不出来，必须由契约给。** 实测（本机，见
`plan/glyph-fallback-evidence.md`）：从**文件名**推出来的名字 libass
**认不出来**，它会**静默回退**到系统默认字体（实测解析成 `ArialMT`，
没有中文字形），而 `lines_failed` 仍是 0、`issues` 仍是空 ——
成片里只是"字体不太对"。所以 **`font_family` 该写就写**。

##### 字体定位的几条边界（都是实测出来的，都会响亮报错）

| 情况 | 行为 |
|---|---|
| `--font-dir` 指不到目录 | **退出码 2**，说明它必须是一个装字体的目录 |
| `--font-dir` 里一个字体文件都没有，且没给 `--font-file` | **退出码 2** |
| 工程点名了 `font_family`，但目录里没有这份字体 | 出片**判失败**（`subtitle_font_missing`），消息点明是"点名了哪个名字、在哪个目录里没找到" |
| `--font-file` 和 `--font-dir` 都没给，工程却有字幕 | **判失败**，并**在出片前**就出声（不等一趟分钟级的活白跑） |

**一处已知边界，如实记**：`font_family` 与目录文件名是**归一化后包含**匹配
（小写、去掉非字母数字），所以 `"Noto Sans SC"` 与 `NotoSansSC-Regular.ttf`
能对上，但**名字与文件名不同源时会找不到**（找不到就报错，不静默换）。

##### 回退：缺字会借别的字体，跨机可能不同

字体里缺某个字形时（用户的文案总有覆盖不到的码位），libass 会**逐字形回退**
到另一份字体上把它补出来 —— 这是 libass 相对 `drawtext` 的关键好处
（`drawtext` 只会画一个 `.notdef` 空心方框，而且**退出码 0、不报警**）。

**但回退落到哪一份字体，取决于机器上装了什么**（本机实测落到
`MicrosoftYaHeiUI`）。于是：

* **同一台机器、同一个 `--font-dir`，两次出片一致**（这条保得住）；
* **换一台机器或换一个平台，缺字那几个字可能换一副字形** ——
  这是"两端可比"的**已知边界**；
* **回退用的字体集合该由谁提供，尚未与下游宿主（V-Trim）对齐，
  如实记为未定** —— 本仓没有字体栈，那是宿主/系统的事，这里不单方面假设。

这条与既有的"两端比结构、不比字形"那条口径**不冲突**（wasm 侧走 canvas
`fillText` + 系统字体，本来就是**第三条独立路径**）。

#### 字幕装不下时：缩字，不是丢行

字幕样式里有三个字段管这件事（都**默认关闭**，既有工程逐字节不变）：

| 字段 | 默认 | 作用 |
|---|---|---|
| `safe_width_ratio` | `0.0` | 换行安全宽（0 = 沿用 `bottom_margin`，凑出约 88%）。参照实现 是**写死 87.5%** |
| `shrink_min_scale` | `0.0` | **装不下时整体缩字号的下限**（0 = 不缩，超出就丢行） |
| `keep_all_lines` | `false` | **不截断**，折多少行画多少行 |

参照实现的换行规则是：

```text
if (tw > maxLineW + EPS) scale = max(0.7, min(1, maxLineW * 3 / tw));
```

`tw` 是**基准字号下的总宽**，那个 `3` **不是截断阈值** —— 参照**从不丢行**，
它只出现在这条公式里，作用是"缩到恰好三行装得下"。

⚠️ **光有缩字还不够**，必须同时 `keep_all_lines`：那条公式只保证
`总宽 × scale == 可用宽 × 3`，而**贪心折行每行会浪费一点**，所以**实际会折出 4 行**。
实测（140 字、`max_lines = 3`、`shrink_min_scale = 0.7`）：折出 4 行、**丢 1**；
加上 `keep_all_lines` 才与参照一致（**丢 0**）。

缩字号时**描边也跟着缩**（参照 `swEff = sw * wrapped.scale`）——
不缩的症状是"缩过的字幕描边显得特别粗"，而字号与行高都对，只有描边不对。

#### 高亮词：`<span class="hl">`

字幕文本里可以写 `<span class="hl">…</span>`，那几段用 `highlight_color` 上色
（其余用正文色）。**只有填色不同，描边仍用 `stroke_color`** —— 与参照一致：

```text
ctx.strokeText(p.text, sx, ly);            // 逐段描边
ctx.fillStyle = p.hl ? hlColor : color;    // 逐段填色
ctx.fillText(p.text, sx, ly);
```

| 字段 | 默认 | 作用 |
|---|---|---|
| `highlight_color` | `None` | **`None` = 不做高亮**（既有工程逐字节不变）。转译器会给它一个显式值 |

**没有标记时一行只栅格化一次**（走老路）；有标记时才逐段各栅格化一次。

⚠️ **两端各有一处已知残差**：段与段之间的**水平推进量**。

* **浏览器**：`ctx.measureText` —— **量字与画字同一个引擎**，与参照条件完全一致，**偏移是准的**
* **CLI**：`drawtext` 的 `text_w` 只在**它自己那一张**里可用，过滤器之间不能互引，
  所以偏移用**布局的逻辑字宽**累加。全角字（中文）准；中英混排时英文半角会差几像素

#### 文字阴影：`shadow_color`

字幕可以带一层**带模糊的阴影**（参照是 CSS 的 `text-shadow: 0 2px 12px rgba(0,0,0,.4)`）。

| 字段 | 默认 | 作用 |
|---|---|---|
| `shadow_color` | `None` | 阴影颜色（RGBA，**alpha 生效**）。**`None` = 不画阴影**，既有工程逐字节不变 |
| `shadow_dx_px` | `0.0` | 水平偏移（**文档像素**，与 `transform.x/y` 同一坐标系） |
| `shadow_dy_px` | `0.0` | 垂直偏移（文档像素，**正数向下**）。参照的 `0 2px` 就是这里写 `2.0` |
| `shadow_blur_ratio` | `0.0` | **模糊半径**（占**文档高**的比例；0 = 硬阴影）。`12/1080` 就是参照的 12px |

三条口径（不写清就会各写各的）：

1. **只画一次，不参与描边宽度**：阴影是"同一行字按偏移再画一遍"，
   它取的是**填充**的轮廓 —— `stroke_ratio` / `stroke_color` 一点都不进来；
2. **偏移是文档像素、模糊是比例**：与描边那对（`stroke_ratio` → `stroke_px`）同一条分工，
   求值层把比例乘目标高换成像素；
3. **`shadow_color = None` 与 alpha = 0 是同一件事**：都不画。
   所以给字幕配阴影**不会**影响弹幕（`DanmakuSpec` 里根本没有阴影字段），
   也不会让老工程多出一次栅格化。

⚠️ **两端只保证"观感近似"，不保证逐像素一致**（与"字形像素允许不同"同一条口径）：

* **浏览器**：canvas 原生的 `shadowColor` / `shadowOffsetX` / `shadowOffsetY` / `shadowBlur`；
* **CLI**：ffmpeg 的 `drawtext`（**只有白字填充，不带描边**）+ `gblur=sigma=模糊半径/2`。
  那个 **÷2** 就是两边对齐的那一步：canvas 的 `shadowBlur` 等价于 σ 的两倍，
  而 `gblur` 直接吃 σ。谁要把这条当逐像素判据，先看 `plan/text-shadow-design.md` §2。

阴影**不进墨迹报告**（每行都画，判据会恒真）—— 与弹幕同理。

#### 弹幕的滚动时长：逐条，不是轨道级

弹幕素材（ASS）里 `\move` 有两种写法，含义**不同**：

| 写法 | 含义 |
|---|---|
| `\move(x1,y1,x2,y2)` | 跨的是 **cue 窗口**（起止由 `Start`/`End` 定）|
| `\move(x1,y1,x2,y2,t1,t2)` | **显式**给出移动的起止时刻（毫秒，相对 cue 起点）|

"滚多快"是后者的 `t2 - t1`。**读不到才回退**到 `DanmakuSpec::duration_ms`（轨道级）。

⚠️ **轨道级那一个值只能取平均**：实测四条是 15.52 / 10.40 / 12.11 / 9.76 秒，
平均 11.95 秒。用平均的后果是**长句滚得太快、短句滚得太慢** ——
症状是"同一时刻参照的弹幕已经在左边、本仓的还在右边"。

**已知残差**：`\pos(x,y)` 那种**静态** cue 没有 `\move` 可读，仍走轨道级。
（参照的渲染器其实**忽略 `\pos`**、照样按 travel 滚 —— 所以这一条是**转译侧的缺口**，
不是底座的问题。）

### 4.3 分块并行（默认关）

```bash
dhampir render --project P.json --from 0 --to 5811 --out out.mp4 \
  --chunk-workers 8         # 0 = 自动（`min(核数, 4)`）
```

把区间切成 N 段、各起一套 wgpu 上下文并行渲，最后 `-c copy` 拼起来。

**默认是 1（关）**，因为收益**取决于用哪条腿**，而 `resolve_workers` 不知道
当前选中的是哪个 adapter：

| | workers=1 | auto(4) | 8 |
|---|---|---|---|
| **GPU**（RTX 4070） | 0.0206 秒/帧 | **0.0185**（最好） | 0.0231（变慢） |
| **纯 CPU**（WARP，16 逻辑核） | 0.274 秒/帧（26.6 分钟/全片） | 0.080 | **0.054**（5.2 分钟，最好） |

**为什么相反**：WARP 几乎不占线程（渲染期间整机 CPU 只 13%~23%），
而真 GPU 只有**一块**，每个 worker 各开一套上下文、各自同步读回，超过 4 个就互相挤。

代价是**内存按 worker 数涨**（每个 worker 一套 ffmpeg 解码器 + 编码器）：

| workers | 主进程 | ffmpeg 子进程 | 合计峰值 |
|---|---|---|---|
| 1 | ~200 MB | 2 个共 ~940 MB | **~1.14 GB** |
| 8 | ~878 MB | 17 个共 ~3823 MB | **~4.7 GB** |

**分块会改变字节**（边界处 x264 的码率控制不同，实测帧差均值 0.0000–1.9）。
要**逐字节可比**就别开它。

侧挂文件的三条边界：

- 时间是**相对这一趟产物**从 0 起算的毫秒，不是源里的绝对时间；
- 内容只有文本与时间 —— **源里的加粗/斜体/颜色不进侧挂**；
- `--format` 不给就看扩展名（`.ass`/`.ssa` → ASS，其余 → SRT）；
  **看不出来不猜**，直接报错。

没给 `--font-file` 时画面上的字一个都不会有，**侧挂文件照写**。

---

## 五、我想改工程（CLI）

形状是一个带 `op` 字段的 JSON 对象，**六个操作**：
`insert` / `trim` / `split` / `move` / `remove` / `set_sequence`。

```bash
dhampir edit --project P.json --op '{"op":"split","layer":"c","at":75}'   # 干跑
dhampir edit --project P.json --op '...' --write                          # 真写
```

> **不给 `--write` 就是干跑**：只在内存里做一遍并打印结果，一个字节都不落盘。
> 先干跑看清楚，再决定写不写 —— 这是默认值，别把它当成"没生效"。

### 5.1 具名子命令（同一实现的糖）

`clip` / `sequence` / `undo` / `redo` / `batch` 与 `edit` **走同一个 `apply`、
同一条落盘路径**。它们不是另一套实现，所以行为不可能有第二份 ——
这一点由 `check-cli` 钉在**产物字节 + stdout** 上，不是"看起来一样"。

```bash
dhampir clip insert --project P.json --track v1 --asset a1 --at 0 --source-in 0 --length 30
dhampir clip trim   --project P.json --layer c --edge in --to 10
dhampir clip split  --project P.json --layer c --at 75
dhampir clip move   --project P.json --layer c --to 20 --track v2
dhampir clip remove --project P.json --layer c --ripple

dhampir sequence set --project P.json --timebase 30000/1001 --width 1920 --height 1080 --write
dhampir undo --project P.json --history h.json --write
dhampir redo --project P.json --history h.json --write
```

**每个动作"要哪些开关、认哪些开关"都在参数这一关判死 —— 多给一个也报错。**
多给比少给更隐蔽：那个开关会被静静丢掉，而用户以为它生效了。

改帧率会**按时间重算所有序列帧号**；不给 `--timebase` / `--width` / `--height` 的那一项就不动它。

### 5.2 撤销 / 重做

**历史存哪必须由你说**（不替你往工程旁边写文件）。历史文件不在 = 从空历史开始，**不是错误**。
没有可撤销的步骤时退出码 `2` 并明说 —— 静默什么都不做更难查。

### 5.3 批处理

```bash
dhampir batch --project P.json --script ops.txt --history h.json --write
```

脚本是**一行一个 op 的 JSON**（空行与 `#` 开头是注释）。

- **一次写、一条历史** —— 不是一个"循环调 N 次"的宏；
- 中途有一步不成立就**整份不落盘**，并明说**卡在第几行**；
- **空脚本不算成功**（它会打印 ok 却什么都没做，与"脚本路径写错了"分不开）。

### 5.4 公共选项

| 选项 | 作用 |
|---|---|
| `--asset-root <目录>` | 工程里 `asset.uri` 的相对根（默认 `target/s3`） |
| `--asset-map <文件>` | 兜底资产表（`assets.<id>.file`）。**只补工程没登记的 id**，工程里的位置永远优先 |
| `--width` / `--height` | 输出尺寸（默认取 `render_hints`） |
| `--font-file` | 画字幕的字体（见 4.2） |

---

## 六、我想在浏览器里剪

**这里没有打包器**（不用 Vite / webpack / pnpm）。`wasm-pack --target web` 的产物本身就是
ES module，浏览器能直接 `import`。少一层构建就少一层"改了没生效"。

### 6.1 完整应用（前后端一起起）

```bash
node scripts/web-check.mjs --serve --local
```

它同时起**静态服务**与**本机后端**，并打印一个 URL，形如：

```
http://127.0.0.1:5xxxx/?backend=local&port=8802&project=sample-project.doc
```

用浏览器打开那个 URL 即可。结束用 **Ctrl+C**。

> **不要把这个命令的输出接进 `| Select-Object` / `| head`。** 管道提前关闭会让它立刻退出 ——
> 表现是"起不来"，而真正的原因在管道那一侧。

改前端代码后**刷新页面**就行（服务带 `no-store`，不吃缓存）。

### 6.2 只起服务，人工看

```bash
node scripts/web-check.mjs --serve
```

### 6.3 程序化验收（五种跑法）

```bash
node scripts/web-check.mjs --probe       # W0：工程帧能不能上 canvas
node scripts/web-check.mjs               # 降级模式：逐帧导出 -> FFmpeg 编码
node scripts/web-check.mjs --local       # 本机模式：走**产品导出路径**（提交 -> 轮询 -> 下载）
node scripts/web-check.mjs --remote      # 分离模式的**代码路径**
node scripts/web-check.mjs --synthetic N # 合成源导出（双端比对用）
```

另有 `--frames-only`、`--canvas WxH`、`--timeout-ms N`、`--ready-only`。

**为什么 `--local` 与默认跑法要分开**：默认那条验的是"浏览器渲染的**帧**对不对"；
`--local` 验的是"浏览器把工程交给后端、后端出片、能下载"。两条路终点都是 mp4，
但中间完全不是一回事 —— 混在一起，失败时只说得出"失败了"。

### 6.4 判定回传（`--verdict`）

```bash
node scripts/web-check.mjs --verdict playback --local
node scripts/web-check.mjs --verdict trim-drag --local
node scripts/web-check.mjs --verdict ui --local          # 界面骨架（DOM 契约）还在不在
node scripts/web-check.mjs --verdict realframe --local   # 预览那一帧 vs 出片那一帧，差多少
```

页面自己跑一次验收并**主动 POST 回后端**，驱动只读后端。为什么不让驱动钻进页面取：
CDP 的 `Runtime.evaluate`（`awaitPromise` 与 `returnByValue` 同用）在本机 Chrome 上
**给回空对象** —— 而"返回空对象"和"什么都没发生"长得一模一样，那种诊断工具比没有更坏。

**拿不到就是没拿到，不会伪装成通过。**

`--verdict ui` 盯的是 `web/index.html`：**改版最容易顺手删掉一个 id，而那种坏法在别的
判定里完全看不见**（驱动只读 `window.__dhampirMarks` 与 `document.title`，一个按钮点了
没反应它照样报绿）。它逐个点 32 个契约 id，并且确认 6 个图标按钮里**真的画出了 svg**
—— 「按钮在」不等于「图标画出来了」，`setIcon` 是启动末尾才跑的。

`--verdict realframe` 是**核心承诺的判据**：它让页面走 `POST /frame` 把后端渲染的
同一帧取回来，盖在画布上逐像素比，并报出实测的差。判据取 **4x4 块均值**而不是
单像素最大值 —— 最大值对重采样的边缘极敏感（一个像素差 205 可能只是缩放滤波器
不同），块均值对滤波不敏感、对真实差异敏感。

**它不判逐像素一致**：两端色彩矩阵不同源（台账 D14，wontfix），含解码的逐像素双端比对
按 D15 是 unmeasurable。逐像素一致不是本工程的承诺，拿它当通过线只会得到一条永远红的
假红。它判的是「这一帧确实取回来了、尺寸对得上、帧号是这一个、读数可信」，
外加一条粗栏杆（块均值 > 60 已远超 D14 的量级）。差异数字照实打出来。
实测量级与它**已知不覆盖**的东西见 `docs/api.md` 4.1。

### 6.5 预览性能：实测数据（**别猜，这一节是量出来的**）

有两个测量入口，**它们回答的不是同一个问题**：

```bash
node scripts/web-check.mjs --verdict perf --local         # 一帧里各段各占多少
node scripts/web-check.mjs --verdict playthrough --local  # 整条时间线播一遍，丢多少帧
```

#### 一帧花在哪：解码，不是代码

`--verdict playthrough` 会把一帧拆成 `sourcesFor / clearBitmaps / seek / createImageBitmap /
setBitmap / text`。四路工程（90 帧）实测，**慢帧里 99% 的时间在 `seek`**：

```
sourcesFor 0.11ms  clearBitmaps 0.00ms  seek 43.85ms
createImageBitmap 0.00ms  setBitmap 0.00ms  text 0.18ms  = 合计 44.15ms
```

| 素材 | 分辨率 | 像素 | 单次 seek 中位 |
|---|---|---|---|
| 720p | 1280x720 | 0.92MP | **22ms** |
| 1080p | 1920x1080 | 2.07MP | **45ms** |
| 4K | 3840x2160 | 8.29MP | **151ms** |

而 30fps 的每帧预算是 **33.3ms**。
**链路本身（宿主求值 + 文字 + 画）只有 1ms 上下** —— 那条链路上没有可精简的余地。

#### 别被中位数骗了（这一条是本仓库踩过的坑）

同一次测量里，单帧**中位 10ms**、**均值 38ms**、**最坏 334ms**，而丢帧 37/90。
中位之所以好看，是因为**贵的帧已经被丢掉了** —— 剩下的自然便宜。
所以判定写的是：

> 单帧中位达标（10ms <= 33.33ms），但丢了 37 帧 —— 中位只覆盖画出来的那些帧。

**只看中位数会得出「跑得满」这个相反结论。** 报数时中位、均值、最坏、丢帧**必须一起看**。

#### 已经证伪的做法（别再花时间）

这一节的每一条都做过**受控对照**，否掉的结论和留下的结论一样值钱：

1. **设 `<video>` 的 `width`/`height` 来降低解码分辨率 —— 没用。**
   四组对照（不设 / 设属性 / 设 CSS / 两者都设）`videoWidth` **全是 3840**，seek 也没变快。
2. **多路 seek 改成并行 —— 没用，而且略慢。**
   受控对照（同一组位置、各自先回同一起点）：2 路 **1.01**、3 路 **0.91**、
   1 路（无并行可言的对照）**0.95**。第三行是关键：只有一路时加速比也是 0.95，
   说明这个量级的差是**位置噪声**。浏览器把多路解码排在同一条队列上。
   探针：`node scripts/seek-parallel-probe.mjs`。
3. **关键帧距离** —— 单看数据 `r = 0.992` 像是强相关，但那是假相关：
   把位置控住之后，seek 代价与「距上一个关键帧多远」**无关**。
4. **`createImageBitmap` / `set_bitmap`（拷进 GPU 纹理）/ 文字栅格化** —— 都是 0.00ms。
5. **页面被限速** —— 实测 `visibility=visible`、`focus=true`、rAF 中位 6.9ms，没有限速。

#### 一个仍然没查清的事实（如实记下来）

在**独立探针页**里跑同一段 seek 序列是 **0.1ms**，在**应用页**里是 **40ms** 上下 ——
同一个浏览器、同一个文件、同一段代码。把引擎的加载阶段分开量（wasm 加载前 / 加载后 /
全部起来后）**三段都是 38–41ms**，也就是说**与引擎无关**，页面一起来就是这样。

试过并否掉的解释：跨源媒体、Range/206 应答、分块传输、`display:none`、
WebGPU 消费者、音频元素、四路轮询、`--enable-logging` 开关、测量顺序。
**在应用页内部的同页对照（引擎绑定的 `<video>` 45.2ms vs 同页新建的 `<video>` 45.5ms）
是这条结论的硬证据**：两个 video 一样慢，所以不是引擎那条路带出来的。

结论：**40ms 是「这个页面环境」的现状，不是某一行代码的账**。
要真正提速，只有下一条里那两条路；在那之前，判定如实报数、不假装跑得满。

#### 真要跑满帧率，只有两条路（都不在预览这一层）

- **用低分辨率的预览代理素材**（架构上正确：预览用代理、出片用原片）；
- 换一条不走 `<video>` 的解码通路（WebCodecs）—— 是另一个量级的改动。

另外，**稀疏 GOP 的素材对逐帧 seek 特别不友好**
（本仓库的 `sparse-proxy720p.mp4`：480 帧只有 8 个关键帧，约 1 秒一个），
它与分辨率是**两个独立的原因**。

### 6.6 播放与丢帧

播放**按序列帧率推进帧号**，不用 `<video>.play()` —— 第二个时钟一定会与出片漂开，
而"预览与成片不一样"就再也说不清是谁的问题。

跟不上的时候**允许丢帧，但丢了多少会报出来**（状态栏，播放按钮右侧）：

```
丢帧 38 帧：跳过 35 帧、卡顿丢弃约 3 帧（93ms）；实际画出 54 帧
```

两类**分开记**，因为原因完全不同：

| 类别 | 含义 | 精确性 |
|---|---|---|
| `skipped` | 时间推进一步跨了 n 帧，中间 n-1 帧没画 | **确定值** |
| `stalled` | 单拍耗时超过 250ms 上限、被砍掉的那段时间 | **估算值**（按帧率折算） |

第二类标"约"：那段**根本没进入帧号换算**，只能说约等于几帧。把估算说成精确值就是骗人。

### 6.7 界面：这一层长什么样、按什么规矩长

打开页面就是**剪辑台**：左边舞台 + 传输条，下面是时间线，右边是检视器，
最上面一条承担工程状态与导出。三条规矩：

* **零构建**。没有 npm、没有打包器、没有前端框架 —— `scripts/check-web-invariants.mjs`
  盯着 `web/node_modules` 不许存在，也盯着 `app.js` 里不许出现业务规则字符串
  （校验只在 Rust 里有一份实现）。所以样式是**一张 `web/app.css`**，图标是**内联 SVG 表**。
* **三态分明**：载入中 / 空（真的没有） / 读不到（出了事）。
  「读不到」必须说清是读不到，**不许显示成空** —— 两者要修的东西完全不同。
* **帧是契约单位**。时间码是给眼睛的，帧号才是真的；播放头永远按整数帧走。

舞台右上角的 **「真实出片帧」** 按钮是本工程核心承诺的兑现口：点一下，后端用
`dhampir frame` 渲染**当前这一帧**，取回来盖在画布上，并报出两边的像素差。
再点一下收回；播放头一动自动收回（留着它比让人比错更坏）。判据与实测见 6.4 与
`docs/api.md` 4.1。

键盘：`空格` 播放/暂停，`←/→` ±1 帧（`Shift` ±1 秒），`Home/End` 首尾，
`S` 切开，`Delete` 删除，`Ctrl+Z` / `Ctrl+Shift+Z` 撤销/重做，`M` 静音，
`,` / `.` 或 `J` / `L` 微调，`?` 打开快捷键表，`Esc` 关掉它。

### 6.8 本机后端单独起

```bash
node scripts/dhampir-local.mjs --port 8787 --asset-root target/s3
```

分工是 **HTTP 在 Node，渲染在 Rust**：这个脚本只管路由、任务生命周期、
把请求落成临时文件、把 Rust 的输出翻成 HTTP。渲染一律调 CLI。
路由清单见 [api.md 第四节](api.md)。

---

## 七、特效与转场

特效是**「类型串 + 参数」的声明式数据**，不是代码。加特效只需在登记表里加一格；
渲染器按 `pipeline` 派发，不需要改渲染主路径。

### 7.1 已登记的特效（13 个 / 4 条管线）

| kind | 参数 | 范围 | 管线 | 说明 |
|---|---|---|---|---|
| `gaussian_blur` | `radius` | 0 – 16 | SeparableBlur | 上界与着色器抽头数钉在一起，改一边必须改另一边 |
| `brightness` | `amount` | -1 – 1 | ColorAdjust | 加性亮度偏移，**归一化色值**（不是百分比） |
| `contrast` | `amount` | 0 – 4 | ColorAdjust | 绕 0.5 中灰缩放，`1` = 不变 |
| `saturation` | `amount` | 0 – 4 | ColorAdjust | 向亮度插值，`1` = 不变，`0` = 完全灰度 |
| `hue` | `degrees` | -180 – 180 | ColorAdjust | 色相旋转，单位是**度**（不是弧度） |
| `flash` | `opacity` / `color` | — | ColorMask | 常量色叠加（加性/混合），不是逐像素变换 |
| `vignette` | `strength` | — | ColorMask | 径向暗角 |
| `noise` | `amount` | — | ColorMask | 噪点 |
| `overlay` | `color` / `stops` | — | ColorMask | 覆盖层（纯色 / 渐变） |
| `shake` | `amount` / `frequency` / `seed` | — | Warp | 位移场抖动。`amount` 是**归一化位移比例**，乘回目标高才是像素 |
| `zoom_bounce` | — | — | Warp | 瞬时推近再回弹 |
| `pulse` | — | — | Warp | 瞬时脉冲 |
| `split` | — | — | Warp | 分裂位移 |

`ColorAdjust` 与 `ColorMask` **共用"逐像素"这一档**，可以同时挂；同类叠加可交换。
参数填恒等值（亮度 0 / 对比度 1 / 饱和度 1 / 色调 0）等价于不挂 —— **已实测出片逐字节相同**。

**登记表是这条链路的唯一真相**，运行时可用 `--effects` 之类的入口自查；
`scripts/check-effect-registry.mjs` 盯着"登记了的管线都真的实现了"。

### 7.2 执行顺序（三段，不是两段）

`PassStage` 把 13 个特效分成三段，**按段执行**：

| 段 | 含哪些管线 | 做什么 |
|---|---|---|
| `PerPixel` = 0 | ColorAdjust + ColorMask | 逐像素色彩变换与常量色叠加 |
| `Warp` = 1 | Warp | **坐标重映射** —— 它改变"哪个像素在哪" |
| `Neighborhood` = 2 | SeparableBlur | 邻域算子（模糊） |

顺序有实际影响：亮度是加性的，先调整再模糊与先模糊再调整**结果不同**。
定成"先决定这张图长什么样、再糊它"更符合直觉。

**模糊不能排在 Warp 之前**：Warp 的位移场是按**目标坐标**算的，
先糊会把位移场的边缘糊成一片，位移就不准了。

这个顺序只写在渲染主路径里**一处**（`PassStage::of` 与 `effect_passes`），
两端一致因此是**结构保证，不是纪律**。

### 7.3 有素材的图层**只执行 SeparableBlur**

这一条是最容易踩的：

> **挂了素材的图层（实拍片段、贴纸）只跑模糊；别的管线挂上去通过校验、进 JSON、
> 被求值层算出来，然后整条丢掉 —— 画面一点不变。**

原因是那几条管线是"对**合成结果**做变换"的，而素材层是"把素材画上去"。
所以 `shake` / `flash` / `color_shift` 这类要挂在**调整图层**上
（没有素材、只有 `effects` 的那一层）。

**这条不再靠人记**：`validate_timeline_v2` 会报 `effect_would_be_ignored` 错误。
判据是 `layer.source.is_some() && spec.pipeline != SeparableBlur`。

### 7.4 已登记的转场

| kind | 参数 | 说明 |
|---|---|---|
| `cross_dissolve` | `duration`（帧） | 交叉溶解：把前一个相邻片段淡出的同时把自己淡入 |

转场挂在片段的 `transition_in` 上。校验拦三种情形：时长非正、时长超过片段本身、
以及**前面没有紧邻片段**（那样"淡出"的那一头不存在，画面会凭空从黑里淡进来）。

**契约 v4 起转场类型是字符串而不是枚举** —— 加转场与加特效同样不必升版本。
但注意：渲染侧目前**只读 `duration`**，不认 `kind`。换了 `kind` 不会改变画面，
只是通过校验的类型串不同。真要有第二种转场，需要在渲染侧认出它。

---

## 八、验证与守卫

### 8.1 跑全部守卫

```bash
node scripts/run-guards.mjs              # 跑全部 20 条（每条先自检、再正跑）
node scripts/run-guards.mjs --list       # 只列清单
node scripts/run-guards.mjs --self-test  # 只跑各自的 --self-test
```

每条都被**反向验证**过（临时植入违例，确认它真的会红）。
**守卫若不会红，就不是守卫。**

**自检与正跑分开报**：自检证明守卫自己没坏，正跑证明仓库没坏 ——
分开才知道是哪一边出了问题。守卫报红时**先跑它的 `--self-test`**。

### 8.2 CI 里只跑 3 条

```bash
node scripts/check-core-purity.mjs    # core 里没有 #[cfg] / cfg!
node scripts/check-dep-graph.mjs      # 依赖方向单向无环
node scripts/check-text-hygiene.mjs   # 全仓 LF + 无 BOM + 合法 UTF-8
```

另外 16 条要 ffmpeg / GPU / 真浏览器 / `records/` 里的取证存档，只在开发机上跑。
**这不是"少验一点"** —— 放进 CI 只会让"环境没装好"长得像"代码回归"。

三条的纪律：**不在空文件集上通过**。没有文件可查时退出码是 `2` 不是 `0` ——
"没扫到"和"扫过了没问题"是两件事。

### 8.3 完整命令

```bash
cargo check --workspace
cargo check -p dhampir-wasm --target wasm32-unknown-unknown
#   注意：wasm 侧不能带 --workspace —— dhampir-worker 是 native-only
cargo test --workspace
node scripts/run-wasm-tests.mjs
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
```

---

## 九、留取证记录

```bash
node scripts/record-acceptance.mjs --milestone m0   # 也支持 m1 / m2
```

按 `plan/` 里写的退出标准逐条跑，把**原始 stdout/stderr** 与每项的退出码落进
`records/<里程碑>/`，最后写 `acceptance.json`。**判定以退出码为准。**

两个宿主各自的产物：

```bash
# native：离屏探针图 + adapter 信息（--probe-only 则完全不碰 GPU）
cargo run -q -p dhampir-worker --bin dhampir-render -- --out records/m0
cargo run -q -p dhampir-worker --bin dhampir-render -- --probe-only --out records/m0

# 浏览器：本地静态服务 + 记录落盘口
node scripts/serve-wasm-harness.mjs --port 8787
#   打开打印出来的 URL（带 ?autorun=1&expect=<native PNG 的 fnv1a64>）
#   页面结论会 POST 回服务，写成 records/m0/browser-harness.json 与 probe-browser-webgpu.png

# 浏览器那一份的整页截图
node scripts/capture-harness-screenshot.mjs
#   无头下拿不到 WebGPU 时加 --headed
```

---

## 十、常见问题

### 10.1 `wasm-bindgen` 版本不一致（第一天就红的头号来源）

报 `it looks like the Rust project used to create this wasm file was linked against a
different version of wasm-bindgen` 并拒绝生成胶水代码。
**看着像"构建坏了"，其实是版本没对齐。**

- crate 版本由 `Cargo.toml` 的 `[workspace.dependencies]` 钉住；
- CLI 由 `wasm-pack` 下到自己的缓存里，**不在 PATH 上**；
  `scripts/run-wasm-tests.mjs` 会去缓存里找，并**先比对版本再跑**（不一致直接退出 2，不去猜）。

### 10.2 改了 Rust 之后守卫变红（陈旧 wasm pkg）

驱动检测到陈旧 pkg 会**自动重建一次**（`scripts/stale-pkg.mjs`），守卫**只判不修**。
若重建后仍红，那是真的坏了 —— **不要手工去改 pkg 让它变绿**。

已知的坑：**失败的 wasm 构建会毒掉增量缓存**，此后每次重建都 ICE
（`rmeta/encoder.rs:2457`）。遇到时加 `CARGO_INCREMENTAL=0` 冷跑一次。

### 10.3 本机 node 的 `UV_HANDLE_CLOSING` 断言

Windows + Node 25.5.0 上，`wasm-bindgen-test-runner` 会在**测试全部通过之后**
报 `UV_HANDLE_CLOSING` 并把退出码变成 `-1073740791` —— 与测试内容无关。
`scripts/wasm-test-node-exit-shim.cjs` 接管 `process.exit`、只设 `process.exitCode`；
代价是"挂住"成为新风险，所以调用方设了超时。

**不要改成"忽略退出码、只 grep `test result: ok`"** —— 那是把守卫改成永远绿。

### 10.4 受限会话里起不了子进程

某些受限会话里给子进程开 **stdin 管道**会失败（`os error 231`）。
不喂 stdin 的调用走 `scripts/spawn-tool.mjs`（`stdin: 'ignore'`）——
这是**如实声明这次调用不喂 stdin**，不是垫片。渲染必须往编码器 stdin 写帧，
那部分**修不掉**，而且**只应为这一个原因红**。

**不要套 `--require` 垫片**：它会让"真的起不了子进程"也变绿，**比假红更坏**。

### 10.5 守卫的清理被"安全删除"绊倒

本机 CLI 给 `fs.rmSync` 套了按 turn 计数的安全删除，超阈值直接抛
`SAFE_DELETE_BULK_CONFIRM_REQUIRED`。跑一遍套件要删几百个临时路径。
与判据无关的清理走 `scripts/safe-remove.mjs`（删不掉只警告、**不动退出码**）。

边界：清不掉时上一轮残留可能还在，所以**别拿"文件在不在"当判据**，
要判就判内容/摘要或"这一轮刚写出来的字节"。

### 10.6 `EADDRINUSE`：端口被占

浏览器里报连不上、或后端起不来并报端口占用，通常是**已经有一个实例在跑**
（`--serve` 的窗口没关）。先确认并结束它，再重开 ——
两个实例抢同一个端口时，页面连上的是**先起的那个**，
于是"改了代码没生效"会看起来像缓存问题。

---

## 十一、约定（这套仓库对"怎么算通过"的纪律）

用它的时候会一直遇到，所以集中写在这里：

1. **退出码优先**。判定以退出码为准，不靠读日志、不靠 grep `ok` 字样。
2. **不做软判据**。没有 `|| true`，没有 `continue-on-error`。
   "守卫永远绿"是这套东西最危险的失效模式。
3. **自检与正跑分开报**。见 8.1。
4. **不猜**。看不出来的东西（比如侧挂格式）直接报错，不选一个默认值糊过去。
5. **假红比没有守卫更坏**。触发条件做不到精确时，要么改措辞、要么写明
   "这条守卫不管什么" —— **不默默放过**。
6. **量出来的数才写进文档**。性能结论必须给测量入口与原始数字；
   证伪过的做法要留在文档里，免得下一个人再试一遍。
