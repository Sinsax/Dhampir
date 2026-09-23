# T2 证据：文字与字幕上屏

分段记录。T2 还没收口，所以这里先只有已完成的那几段。

## T2.3a 栅格化：一行文字 → RGBA8 位图 —— 已完成

### 它解决什么

字形像素由各宿主自己造（字体、抗锯齿、hinting 本来就不同，这点差异是允许的），
但**画的是哪一行的哪些字**必须能查。宿主的入口就是一段窄接口：一行文本 + 样式 → 一张 RGBA8 位图。
本机这条路走 ffmpeg 的 drawtext。

### 落在哪

crates/dhampir-worker/src/text_raster.rs。契约是**直排 RGBA8 位图**（与 readback 同一个约定），
所以叠加那一步不用做任何格式判断。四件在实测里才定下来的事：

* **文本走 `textfile=` 参数，不进 filter 串**：用户文本里的冒号、单引号、方括号会毁掉整条命令；
* **`expansion=none`**：默认档下文本里一个 `%` 就让这条命令失败（`Stray %`），`%{n}` 还会被换成帧号；
* **ffmpeg 只画白字 + 黑描边**，样式色与 alpha 由 CPU 侧的 `tint()` 乘上去 ——
  同一条字幕换颜色不该再起一次进程；
* **位图宽度取整条目标宽**：宽度跟着墨迹走的话，各行中心会漂，而「字画在哪」正是最想量的东西。

缓存键是 `TextRasterKey{text, font_px, color, outline, font_file, width, height}`——
**尺寸必须进键**：只按文本与字号缓存，换一个目标尺寸就会拿到上一张尺寸的图。单测里有一条反向用例盯着它。

### 成本

数字与量纲在 plan/measurements.md 六：一行 1080p 字幕冷 **55.7 ms**（几乎全是 ffmpeg 起进程），
命中缓存 **0.5 µs**。所以这条路的代价是「**每行一次进程**」，与这行出现在几帧上无关 ——
缓存不是优化，是可行性。

### 单元测试

    $ cargo test -p dhampir-worker --lib
    test result: ok. 73 passed; 0 failed; 3 ignored

其中 text_raster 占 21 条（18 条纯函数 + 3 条 `#[ignore]` 真起 ffmpeg 的）。值得单独说的：

* **用户文本全打不进命令行**：断言的是「参数里没有那段文本」**加上**「`textfile=` 在」——
  少了后半句，这条在「文本干脆没被传进去」时也会通过；
* 缓存键里少了位图尺寸就会拿错图（这就是那条反向用例）；
* 字体路径里的冒号与引号要转义；空文本给全透明位图且**不叫进程**；
* 样式色的不透明度只缩放、不改色相。

三条真跑用例默认 `#[ignore]`：画出一行中文字、一行长英文不被切、用户文本里的 `%` 不被展开。

## T2.4 宿主把字画进像素 —— 已完成

### 落在哪

crates/dhampir-worker/src/text_overlay.rs（几何 + 混合 + 判据），接线在 pipeline.rs 与 bin/dhampir.rs：

| 接缝 | 改了什么 |
|---|---|
| `RenderPlan` | 加 `subtitles: &SubtitleTable` 与 `font_file: Option<&Path>`——**读文件仍在宿主**，库侧不做 I/O |
| `render_plan` / `render_frames_png` | 读回像素之后、写进编码器之前叠一次；字幕问题走**独立的 IssueLog**，收尾再并进主账 |
| `RenderReport` / `FramePng` | 加 `overlay: OverlayStats`（事实：画了几行、丢了几行、缓存命中几次、被切几行） |
| CLI | 加 `--font-file`；`frame` / `render` 的 done 里多一个 `overlay`；`frame` 在字幕问题非空时退 1 |

几何一句话：**位图中心对准行盒中心**（`x = round(rect.center_x × 目标宽 - 位图宽 / 2)`）。
字号由行盒高反推（`font_px = round(行盒高 / LINE_HEIGHT_EM)`）——只从 `TextOverlay` 的结构推，
不重读一遍轨道样式：两处各算一次就会漂。

### 判据：三条问题 + 一条事实

| 情况 | 落地成什么 |
|---|---|
| 有字要画、宿主没给字体 | 问题 `subtitle_font_missing`——**不静默出一份没有字幕的片子** |
| ffmpeg 画不出这一行 | 问题 `subtitle_raster_failed`，消息带 ffmpeg 的原文 |
| 墨迹被切（顶到位图边界，或有墨像素落在画面外） | 问题 `subtitle_ink_clipped`——判失败 |
| 超过 max_lines 被丢掉的行 | **事实**：只计数（`lines_dropped`），不判失败 |

第三条判失败不是口味：实测真字体比共享布局的按字宽分类模型**宽 3%~25%**，
所以「模型说放得下、真字体放不下」是可达的（模型 88% 宽的行 × 1.25 = 110%），
那种片子里的字是真的被画面边缘切掉了。反过来，丢行是样式里写明的上限在起作用，
把它判成失败会让失败变常见，而失败一旦常见人就忽略它。

### 两条路径各一个用例（真机）

crates/dhampir-worker/tests/overlay.rs。两条用例共用一份夹具（640x360 棋盘格背景 + 一条字幕轨），
差别只有**多一条带模糊的调整图层** —— 于是差异只剩路径本身：没有调整图层时 `render_frame`
一趟画完，有时走 `render_segmented`。

量法与背景无关：同一份夹具渲染两次（带字幕 / 不带字幕），**两帧的像素差异就是字的墨迹**，
再拿它与共享布局给的矩形中心对。这样也不依赖「白字比背景亮」这类关于颜色的前提。

    $ cargo test -p dhampir-worker --test overlay -- --ignored --nocapture
    单趟：改了 1255 个像素（画面 640x360）
    单趟：墨迹 (269, 316, 370, 336)，中心 (319.5, 326)，期望 320/326.52，尺寸 102x21
    分段：调整图层改了 230384 个像素，范围 Some((0, 0, 639, 359))
    分段：墨迹 (269, 316, 370, 337)，中心 (319.5, 326.5)，期望 320/326.52，尺寸 102x22
    test result: ok. 3 passed; 0 failed; 0 ignored; 1 filtered out; finished in 2.83s

从这几行能读出来的：字真的进了像素（1255 个）；落点与共享布局的期望中心差 **0.5 px**（容差 2）；
墨迹 102x21 是**一行字**的量级（容差横 40..=180、纵 6..=30）；
分段的 230384 个像素说明调整图层确实被调度了（不是悄悄又走了单趟），而字仍在同一处。

### 没有字要画时：像素逐字节不变（端到端）

判据的原话是「无字幕工程逐字节不变」。这条不靠测试里的摘要，而是**改动前后的两个二进制同参数出片**：
`target/wt-head`（HEAD dc517fe 的独立 worktree，独立 target 目录）与当前构建，
同一个工程（fixtures/sample-project.doc.json，**没有字幕轨**）、同一段帧、同一个尺寸、同一个素材根：

    $ <改动前>/dhampir.exe render --project fixtures/sample-project.doc.json \
        --from 0 --to 89 --width 320 --height 180 --asset-root .../target/s3 --out .../head.mp4
    {"encoded_frames":90,"failed":false,"frames":90,"issues":[],"opened_streams":4,...}
    $ <当前>/dhampir.exe render ... --out .../now.mp4
    {"encoded_frames":90,"failed":false,...,"overlay":{"cache_hits":0,"cache_misses":0,"lines_clipped":0,
     "lines_drawn":0,"lines_dropped":0,"lines_failed":0},...}

    head.mp4  22552 字节  SHA256 42E6195C0E090D3CA7545D1E8FE3EFE3ABE2771BEA4B6E2A8A808B6DA04D21A6
    now.mp4   22552 字节  SHA256 42E6195C0E090D3CA7545D1E8FE3EFE3ABE2771BEA4B6E2A8A808B6DA04D21A6

同一个 SHA256。当前版本多出来的 `overlay` 字段**全零**：没有字幕的工程里，这一阶段没动任何像素。
（原始 NDJSON 留在 target/t2/overlay-evidence/{head,now}.stdout.txt，两份都是 92 行。）

同一份测试文件里还摆了三个「没有字要画」的形态（无字幕轨 + 给字体 / 无字幕轨 + 不给字体 /
有字幕轨但这一帧没有活着的字幕），它们的帧摘要必须与「有字要画、但没给字体」那一次**完全相同** ——
画不出来就一个字节都不许动，另记一条问题：

    无字三态 + 画不出来的像素摘要一致：5d935540d6cf8325

### 顺带修好的一处派生物漂移（HEAD 上就红，不是这一段引入的）

    $ node scripts/timeline-contract.mjs
      - schema/timeline-v1.schema.json 与当前 Rust 类型不一致——跑 --write        EXIT=1

`TrackKind` 在 Rust 里早就有 `subtitle` / `danmaku` 两个变体，而提交的 schema 派生物还停在
「只有 video / audio」—— `870e1ec`（字幕轨与弹幕轨的契约）改了类型却没重生成派生物，
而上一次生成是更早的 `ab1ed5c`。跑 `--write` 之后契约脚本转绿，派生物补上了那两个变体。

### 覆盖边界（不假装）

* **浏览器侧还没有**：canvas 栅格化与它的叠加是 T2.5。所以「两端画出来一致」还没接线
  （D15 已记：含解码的逐像素双端比对测不了）。
* **`HOST_API_VERSION` 还是 1**：`frame` 的产物多了 `overlay` 字段，按本仓口径这是 API 变更，
  要到 T2.7 才升到 2 —— 在那之前对端不能依赖这个字段。
* **一条字幕全部超过 max_lines 时，丢行计数拿不到**：`evaluate_overlay` 按契约返回 `None`
  （它把「没有可画的东西」与「没有字幕」做成同形），于是那份 `dropped_lines` 在中途就没了。
  改它要动 core 的返回口径（T2.1 的契约），不在这一段的范围里。
* **真机只量了一帧**（第 15 帧，一条字幕的中间）：字幕的上下边界、两条字幕相接的那一帧、
  多行字幕的逐行落点，靠的是 core 的单元测试与 text_overlay 的 13 条单测。
* **没有量画质**：判的是「墨迹在不在该在的地方」，不是「好不好看」——抗锯齿与 hinting 都没量。
* **分段路径只取了一条**：调整图层的种类只有模糊一种，所以「有调整图层」这件事被验到，
  「多种特效叠加时字幕还在」没验。

## T2.2 共享文本布局（A3）—— 已完成

### 它解决什么

两端都要把同一段字幕画出来。字形可以由各宿主自己栅格化（字体、抗锯齿、hinting 本来就不同），
但**结构**必须一致：几行、每行是什么、每行占哪个矩形。结构一旦分叉，
同一个工程在两个宿主上就是两份不同的字幕。

### 落在哪

crates/dhampir-timeline/src/text_layout.rs（零依赖）。度量模型按 roadmap 里先落下的决定：

| 类别 | 前进宽度 |
|---|---|
| 全角（CJK 汉字/假名/谚文、全角标点） | 1.0 em |
| 半角（ASCII） | 0.5 em |
| 空格 / 制表符 | 0.25 em |
| 组合记号、零宽空格、变体选择符 | 0 |

行高 1.2 em；字号 = font_ratio x 文档坐标系高度；左右安全边距复用 bottom_margin；
行盒按 bottom_margin 从底部往上排、水平居中；超过 max_lines 的行**丢弃并计数**。
换行：全角逐字断、半角按词断、超长单词硬断。

### 参照输出（crates/dhampir-timeline/examples/layout_subtitles.rs）

这个例子的存在有两个理由：宿主的输出要与它逐字段一致；以及那个模块**一出生就有调用方**
（只写不用的公共 API 比没有更容易误导）。

    $ cargo run -q -p dhampir-timeline --example layout_subtitles -- \
        --srt fixtures/sample-subtitle.srt --fps 30/1 --frame 90 --sequence 640x360
    {"active":[{"dropped_lines":0,"end_frame":120,"end_ms":4000,"lines":[
      {"rect":{"height":0.06599999964237213,"width":0.8121093511581421,
               "x":0.09394532442092896,"y":0.8079999685287476},
       "text":"Mixed 混排 text with a rather long tail that ought to wrap"},
      {"rect":{"height":0.06599999964237213,"width":0.1392187476158142,
               "x":0.4303906261920929,"y":0.8739999532699585},
       "text":"somewhere"}],"start_frame":60,...}],"cues_total":4,...}

从这一份输出里可以当场核出四条不变量（不用另外写测试）：

* 居中：0.09394532442092896 + 0.8121093511581421 / 2 = 0.5；
  第二行 0.4303906261920929 + 0.1392187476158142 / 2 = 0.5；
* 底边：0.8739999532699585 + 0.06599999964237213 = 0.94 = 1 - bottom_margin(0.06)；
* 行高：0.066 = font_ratio(0.055) x 行高比(1.2)；
* 英文没被从词中间切断（"...ought to wrap" / "somewhere"）。

再看一帧，长段落被丢弃并如实计数：

    $ ... --frame 210
    {"active":[{"dropped_lines":3, ...,"lines":[{"text":"一"},{ "text":"二"}],...}],...}

五行的字幕在 max_lines=2 下显示两行、丢掉三行，**dropped_lines 是 3**。

### 单元测试

    $ cargo test -p dhampir-timeline --lib
    test result: ok. 152 passed; 0 failed; 0 ignored

其中 17 条是 text_layout 的，包含两条反向性质的用例：

* 分类生效_全角比半角宽一倍 —— 不分类（一律半角）的实现会让这条红；
* 英文按空格断行_不从词中间切 —— 逐字符断行的实现会让这条红；
* 同样输入给同样输出 —— 布局必须确定性，否则两端结构谈不上一致。

### 明知没做（写在模块文档里，不藏）

* **禁则处理**（行首不出现逗号句号、行尾不出现左引号）没做 —— 它是独立的排版课题，
  做了就要有对照用例集。
* 字距微调与连字没做。
* 与真字体度量有偏差：等宽字体的 i 与 W 在这个模型里一样宽，比例字体会让换行点
  与肉眼预期不同。调 font_ratio 只能整体挪，不能让两端更接近。

## T2.1 评估层（A4 / D5 的前一半）—— 已完成

### 落在哪

crates/dhampir-core/src/overlay.rs：TextItem（归一化矩形）、TextOverlay、SubtitleTable、
evaluate_overlay、cue_visible_at。纯函数，不做 I/O —— 读文件与解析由宿主做完交进来
（与 AssetTimebases 是同一个缝）。

**没有塞进 Composite，这是有意的**：文字没有纹理，它要先由宿主栅格化。
把「要画什么字」与「哪张纹理怎么叠」放进一个结构，会让「谁负责栅格化」变含糊，
而含糊的代价是两端各自决定。代价是宿主必须**两个都调** —— 所以接线得由结构守卫盯着，
这条守卫**还没写**，见下面「覆盖边界」。

时间口径：SRT/ASS 的时间戳是绝对时间，与素材帧率无关，所以毫秒换算成帧号用**时间线**的时间基；
区间取闭开 [start, end)；短于一帧的字幕仍显示一帧。

### 单元测试

    $ cargo test -p dhampir-core --lib
    test result: ok. 154 passed; 0 failed（新增 13 条）

三条值得单独说：

* 结构与共享布局逐字段一致 —— core 只许挑帧，**不许对结构做任何再加工**（否则参照就不参照了）；
* 不同宽高比下归一化宽度不同 —— 归一化不是「与一切无关」，而是「与渲染目标尺寸无关」；
* 同样输入给同样输出 —— 评估必须确定性，否则两端结构谈不上一致。

### 端到端（真实工程 + 真实 SRT）

target/sub-test.json 由 fixtures/sample-project.doc.json 加上一条字幕轨与一个字幕素材得到
（它是 target/ 下的草稿，不进库）。

    $ target/debug/dhampir.exe subtitle --project target/sub-test.json \
        --asset-root fixtures --frame 15
    {"frame":15,"sequence":[640,360],"subtitle_assets":1,"items":[
      {"text":"第一行中文","rect":{"x":0.42265623807907104,"y":0.8740000128746033,
                                    "width":0.15468749403953552,"height":0.06599999964237213}}],
     "color":[255,240,200,255],"outline":false,"dropped_lines":0}          EXIT=0

    $ ... --frame 210
    {"items":[{"text":"一",...},{"text":"二",...}],"dropped_lines":3}      EXIT=0

**这两个矩形与 examples/layout_subtitles.rs 打出来的逐位相同** —— 两条独立的路
（CLI 的评估 vs 例子的参照）给出同一份结构，这正是「结构只有一份」的证据。

### 它进了默认关卡

    $ node scripts/check-cli.mjs
    CLI 契约：18 / 18 条判据通过

新增两条判据 subtitle 与 subtitle-blank。判的不是一串魔数，而是三条**结构不变量** ——
居中（x + width/2 = 0.5）、底边（y + height = 1 - bottom_margin）、行高（= font_ratio x 1.2）。
subtitle-blank 那条除了要求 items 为空，还要求 subtitle_assets = 1：
少了后半句，它在「字幕素材根本没读进来」时也会通过 —— 而那正是它要抓的东西。

### 覆盖边界（不假装）

* **没有任何宿主真的把字画出来**。栅格化、overlay 渲染阶段（单趟与分段两条路径）、
  HOST_API_VERSION 1 到 2、--subtitle-out 侧挂导出，全都没做。D5 与 A4 仍是 todo。
* 「宿主忘了调 evaluate_overlay」**目前不会让任何测试变红**。这条结构守卫没写，
  而它应当在两个宿主都接上之后写 —— 在那之前它是空转的。

