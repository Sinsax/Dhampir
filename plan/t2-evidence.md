# T2 证据：文字与字幕上屏

分段记录。T2 还没收口，所以这里先只有已完成的那几段。

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

