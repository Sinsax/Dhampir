# T3 证据：弹幕

> 这一段的三条验收照 `plan/roadmap.md` T3 段：泳道分配**确定性**、排不下**丢弃并计数**、
> 同一输入在两端给出**相同**的 `(text, 泳道, 进入/离开帧)` 且**丢弃数一致**。
> 判据按退出码算，不按日志里有没有"ok"字样。本段全部验证跑在 Windows 上。
>
> **已收口（HEAD 68aca8b）**：T3.1–T3.4 全部落地，D5 已转 done（它的两条验收这一轮
> 都核过了）。四步的证据都在下面，按新的在前排。

## 收口时补的一条：`.srt` / `.ass` 进文本卫生守卫 —— 已完成

写证据时才发现的洞：`scripts/check-text-hygiene.mjs` 的 `TEXT_EXTENSIONS` 里**没有**
`.srt` / `.ass` —— 而这两个扩展名的东西正是**判定输入**（`fixtures/` 里的字幕与弹幕素材），
两端都直接解析它们。文件本身是干净的（`sample-subtitle.ass` 2077 字节、0 个 CR、无 BOM），
但**守卫没在看**：一行 CRLF 溜进去，`chunk` 里多一个 0x0D，"同一份素材两端给出同一张表"
这件事就会在别的平台上换个结论 —— 与 golden 文件同理（守卫自己的注释里就是这么写的）。

补的是**覆盖面**，不是判据（判据一个字节没动）：`TEXT_EXTENSIONS` 加
`.srt` / `.ass` / `.ssa` / `.vtt` 四个，扫描数从 167 涨到 **169**（正好多这两个文件）。

**反向验证**（真仓库里放一个脏文件，验完删掉）：

    $ node -e "...writeFileSync('fixtures/probe-hygiene-reverse.ass','[Events]\r\n...')"
    $ node scripts/check-text-hygiene.mjs
    ✗ 有 1 个文件违反 LF + 无 BOM + 合法 UTF-8（共扫了 170 个）：
      fixtures/probe-hygiene-reverse.ass: 有 2 个 CR（第一个在第 1 行、字节偏移 8）
    退出码 1

删掉探针后 169 个文件全绿。也就是说这条新覆盖真的会红，不是"名单加了但没人扫"。

**同一件事还得钉在守卫自己的自检上**：`--self-test` 原来 12 条内存用例 + 3 条退出码用例 +
4 条磁盘用例，那 4 条磁盘用例里写脏文件的用的是 `.txt` —— 对**扩展名名单**什么都没说。
名单再被删掉一次仍然一路全绿，直到哪天真去写一个带 CRLF 的素材。补第 5 条磁盘用例：
临时目录里放 `probe.ass` 与 `probe.srt`（都带 CRLF），要求 `collectFiles` 收到 2 个、
抓到 2 个违规。

**这条自检的反向验证**（草稿区脚本 `target/t3/probe-hygiene-noext.cjs` 把扩展名那一行从
源码里删掉，生成副本再跑它的 `--self-test`）：

    $ node target/t3/probe-hygiene-noext.cjs
    已生成去扩展名副本：target/t3/_probe-hygiene-noext.mjs
    $ node target/t3/_probe-hygiene-noext.mjs --self-test > target/t3/reverse/r4-noext-self-test.txt 2>&1
    exit=2
    （原始输出里 PowerShell 自己插的 `node :` / `所在位置` / `CategoryInfo` 三行是**重定向噪声**，
    逐字原文见该文件；守卫真正说的两句是：
    ✗ 守卫自检失败——先修守卫，别信它的结论：
      - 自检「.ass / .srt 素材在名单里」期望收集到 2 个文件 / 2 个违规，实际 0 个 / 0 个
    —— 即"名单里没有这两个扩展名"这件事被自检当场抓住，而不是等到有人真写脏素材。）

（副本在 `target/` 下，跑完即删；原始输出在 `target/t3/reverse/r4-noext-self-test.txt`。）
现在真守卫的自检口径：`✓ 守卫自检通过（12 条内存用例 + 3 条退出码用例 + 5 条磁盘用例）`，
扫描 169 个文件全绿。

## T3.4 两端一致验收：`(text, lane, enter, exit)` + 丢弃数 —— 已完成

### 它解决什么

T3.1–T3.3 让弹幕"算得出、画得出、导得出"，但**两端各自都对**并不等于**两端给出同一份**。
T2.5 已经为字幕建过这条通道（`--verdict subtitle`），字幕那份比的是"项数/文本/归一化矩形"；
弹幕比字幕多三样**结构**（泳道、进入帧、离开帧），而这三样正是最容易两端漂的东西：
分配算法一分为二就会漂，且两边各自的表都自洽 —— 只比单帧矩形看不出来（滚动中的矩形
随帧变化，一条被分错泳道的弹幕在某一帧里可能"看起来正常"）。

所以 T3.4 **不新开通道**，落在既有 `--verdict subtitle` 里：页面把宿主的
`dhampir_project_text_frame` 原样回传，驱动再拿 CLI `subtitle` 的同一帧对账。

### 判据：逐字段比结构 + 按容差比矩形 + 防空白两条

比的是 `(text, lane, enter, exit)` 四样，矩形按 `SUBTITLE_TOLERANCE = 1e-6` 比四键：

* `lane`/`enter`/`exit` 是整数，**严格比**（`scripts/web-check.mjs` 的 `compareSubtitleDanmaku`）；
* `rect` 是 `danmaku::rect_at` 这个**时间的函数**算出来的，随帧变化，所以按容差比 ——
  它和上面三样是同一条判据的两半：少了它，"结构对但位置算错"看不出来；
* `dropped_danmaku` **不是日志，是结论的一部分**（少的那几条看起来和"素材里就那几条"
  一模一样），所以也逐帧严格比。

**两端都一条没算出来时，上面每条判据都成立 —— 而那样的结论是白说的。** 于是再加两条
（只在工程**有**弹幕轨时判）：

1. 这 5 帧里**一条都没见到** → 红（`弹幕那一半等于什么也没验`）；
2. **一帧都没丢过条** → 红（`「丢弃数一致」这条判据没被走到`）。

工程里没有弹幕轨时两条都不判（那种工程本来没有这一半可验），弹幕那一半的实测事实
（见到几条、丢过几条）照旧写进结论 —— 只说"一致"的话，看不出这一趟到底验到了什么。

### 判定输入：`fixtures/sample-subtitle.ass` 与工程里新加的 `dm` 轨

弹幕素材在资产表里就是一份 `kind: subtitle` 的素材（内容是 ASS）——**不另起一个 kind**：
解析、登记、贴图三条路与字幕完全同一条，多一个 kind 只会多一处分支。

`fixtures/sample-subtitle.ass`（2077 字节、LF、无 BOM）5 条 `Dialogue`，其中三条 `0:00:00.00`
起、一条 `0:00:04.00` 起、一条 `0:00:06.00` 起；`End` 一律 `0:00:05.00` ——**它不参与**：
在屏时长取自 `DanmakuSpec.duration_ms`（这份工程是 2000 ms），素材的 `End` 只是语法必填。
`\move` 里的坐标也不参与布局（泳道由 spec 统一分配，让素材坐标参与布局，两端就会各读出一套）。

这五条把泳道分配的三条规则一次走完（30fps / `lanes: 2` / `duration_ms: 2000`）：

| 素材条目 | 起始 | 结果 |
|---|---|---|
| 第一条弹幕 | 0 s | 泳道 0，`enter 0 exit 60` |
| 第二条弹幕 | 0 s | 泳道 1，`enter 0 exit 60` |
| 第三条排不下 | 0 s | 两条泳道都被占 → **丢弃并计数** |
| 第四条复用泳道 | 4 s（帧 120） | 0 号泳道上一轮 `exit 60 < enter 120` → **复用 0 号** |
| 第五条换泳道 | 6 s（帧 180） | 与第四条在**闭区间边界**（180）相接 → 不许共用 → **换 1 号** |

工程侧 `fixtures/sample-subtitle.doc.json` 只**追加**（既有键一个没改）：`assets` 加
`dm.ass`（`kind: subtitle`、`uri: sample-subtitle.ass`）、`timeline.tracks` 追加 `dm` 轨
（`kind: danmaku`，layer `shots` `start 0 end 240`，`danmaku{asset_id, lanes: 2,
duration_ms: 2000, font_ratio: 0.04}`）—— 于是这份工程从 6 assets / 5 tracks 变成
6 assets / 5 tracks 里含一条弹幕轨。它与字幕轨**覆盖同一段区间**，所以"这一帧两半都空
→ 整个覆盖层为 `None`"这条契约口径也在同一份工程里被走到（第 240 帧）。

**摆渡**：本机后端只有一个 asset root，驱动起后端前把 `fixtures/` 下的 `sample-subtitle.srt`
与 `sample-subtitle.ass` 都拷到 `target/s3/`；CLI 那半边读 `fixtures/` 本身
（`--asset-root fixtures`），两种 asset root 实测给出同一份结果。

### CLI 那一半的原始输出（`target/t3/cli-frames.txt`）

    $ target/debug/dhampir.exe subtitle --project fixtures/sample-subtitle.doc.json --frame 0 --asset-root fixtures
      退出码=0
      frame=0 sequence=640,360 subtitle_assets=2 dropped_lines=0 dropped_danmaku=1
      color=[255,255,255,255] outline=true
      item   text="第一行中文" rect={"height":0.06599999964237213,"width":0.15468749403953552,"x":0.42265623807907104,"y":0.8740000128746033}
      danmaku text="第一条弹幕" lane=0 enter=0 exit=60 rect={"height":0.04800000041723251,"width":0.11249999701976776,"x":1,"y":0}
      danmaku text="第二条弹幕" lane=1 enter=0 exit=60 rect={"height":0.04800000041723251,"width":0.11249999701976776,"x":1,"y":0.04800000041723251}

    $ target/debug/dhampir.exe subtitle --project fixtures/sample-subtitle.doc.json --frame 60 --asset-root fixtures
      退出码=0
      frame=60 sequence=640,360 subtitle_assets=2 dropped_lines=0 dropped_danmaku=1
      item   text="Mixed 混排 text with a rather long tail that ought to wrap" …
      item   text="somewhere" …
      danmaku text="第一条弹幕" lane=0 enter=0 exit=60 rect={…,"x":-0.11249995231628418,"y":0}
      danmaku text="第二条弹幕" lane=1 enter=0 exit=60 rect={…,"x":-0.11249995231628418,"y":0.04800000041723251}

    $ target/debug/dhampir.exe subtitle --project fixtures/sample-subtitle.doc.json --frame 120 --asset-root fixtures
      退出码=0
      frame=120 sequence=640,360 subtitle_assets=2 dropped_lines=0 dropped_danmaku=1
      item   text="短" …
      danmaku text="第四条复用泳道" lane=0 enter=120 exit=180 rect={…,"width":0.1574999988079071,"x":1,"y":0}

    $ target/debug/dhampir.exe subtitle --project fixtures/sample-subtitle.doc.json --frame 180 --asset-root fixtures
      退出码=0
      frame=180 sequence=640,360 subtitle_assets=2 dropped_lines=3 dropped_danmaku=1
      item   text="一" …
      item   text="二" …
      danmaku text="第四条复用泳道" lane=0 enter=120 exit=180 rect={…,"x":-0.1575000286102295,"y":0}
      danmaku text="第五条换泳道" lane=1 enter=180 exit=240 rect={…,"x":1,"y":0.04800000041723251}

    $ target/debug/dhampir.exe subtitle --project fixtures/sample-subtitle.doc.json --frame 240 --asset-root fixtures
      退出码=0
      frame=240 sequence=640,360 subtitle_assets=2 dropped_lines=0 dropped_danmaku=0
      color=null outline=null

三处可核的事实：帧 0 两条的 `x` 都正好是 `1`（进入帧左边缘贴右边缘）；
帧 60 两条的 `x` 都是 `-width`（离开帧整条移出左边，`1 + (-w-1) × 1 = -w`）；
帧 180 下半段 `入 180` 与前一条 `出 180` **相接而不共用泳道**（闭区间边界规则）。
帧 240 两半都空，`color`/`outline` 是 `null`、两个丢弃数都是 0。

### 判定通道的原始输出（`target/t3/verdict-subtitle.txt`）

    $ node scripts/web-check.mjs --local --verdict subtitle
    判定回传：subtitle
      ✓ 帧 0/60/120/180/240：两端清单一致（文本逐字节、矩形容差 0.000001）；弹幕结构一致（7 条·泳道/进入/离开帧 + 丢弃 1 条）；墨迹逐行自洽（可见的行都有像素、没有字的帧为 0、行间不重叠）
      · 帧 0：1 行，画布 640x360，墨迹 1397 px（逐行 1397）；头一行位图 640x36 @(0,309) 字号 20；弹幕 2 条：第一条弹幕(泳道 0 0..60)、第二条弹幕(泳道 1 0..60)（整条素材丢 1 条）
      · 帧 60：2 行，画布 640x360，墨迹 8642 px（逐行 7173/1469）；头一行位图 640x36 @(0,285) 字号 20；弹幕 2 条：第一条弹幕(泳道 0 0..60)、第二条弹幕(泳道 1 0..60)（整条素材丢 1 条）
      · 帧 120：1 行，画布 640x360，墨迹 394 px（逐行 394）；头一行位图 640x36 @(0,309) 字号 20；弹幕 1 条：第四条复用泳道(泳道 0 120..180)（整条素材丢 1 条）
      · 帧 180：2 行，画布 640x360，墨迹 248 px（逐行 88/160）；头一行位图 640x36 @(0,285) 字号 20；弹幕 2 条：第四条复用泳道(泳道 0 120..180)、第五条换泳道(泳道 1 180..240)（整条素材丢 1 条）
      · 帧 240：0 行，画布 640x360，墨迹 0 px（逐行 ）

`7 条` = 2 + 2 + 1 + 2 + 0 帧上见到的条数之和；`丢弃 1 条` 取各帧最大值（整条素材算一次，
每帧都报同一个数，按帧累加会重复计数）。这一份事实里**逐条给出泳道与进出帧** ——
"一致"两个字本身看不出验到了什么，所以这七条都打印出来。

### 三个反例（都真的红了，也确实还原了）

判据要能**红**才算判据。三次都只改一处、跑同一条命令、看到 exit 1，然后按备份还原并
比对 SHA256：

1. **页面回传的泳道改一格**（`web/app.js` 里第 0 帧那份清单 `danmaku[0].lane += 1`）：
   exit 1，一条指到项 ——
   `帧 0 第 0 条弹幕：lane 不同（CLI 0、页面 1）`；
2. **泳道给够，于是没有一条被丢**（工程里 `lanes: 2` → `5`）：exit 1 ——
   `这份工程有弹幕轨，但一帧都没丢过条 —— 「丢弃数一致」这条判据没被走到`；
3. **一条泳道也不给**（工程里 `lanes: 2` → `0`）：exit 1 ——
   `这份工程有弹幕轨，但这些帧里一条都没见到 —— 弹幕那一半等于什么也没验`。

第 2、3 条两个方向合起来证明那两条"非空白"判据**不是装饰**：它们各自都能被触发，而且
触发它们的那两次**逐字段比对本身是绿的**（两端当然一致 —— 素材一条没少、只是分配规则
不同）—— 也就是说，没有这两条的话这两次会**白过**。

原始输出留在 `target/t3/reverse/{r1-lane.txt,r2-lanes5.txt,r3-lanes0.txt}`（UTF-16，是
PowerShell 重定向写的）。还原后 `web/app.js` 与 `fixtures/sample-subtitle.doc.json` 的
SHA256 与注入前**逐字节相同**：

    web/app.js                 5A0BE682832654D00E2225E4932763B6FB3BE515A3A8F0E08381F86279066035
    fixtures/sample-subtitle.doc.json
                               96692C8706C46078D8C23C832EF7B7976CA023E7A4CD8B01D64A7D158E1471B8

（还原用 `Copy-Item` 从 `target/t3/reverse/*.bak` 拷回 —— 不让 PowerShell 的文本管道
经手源码，那会把 LF 换成 CRLF 或加 BOM。）

### 这一段没改 Rust

T3.4 只动了三处 JS/fixture：`fixtures/sample-subtitle.ass`（新增）、
`fixtures/sample-subtitle.doc.json`（追加）、`scripts/web-check.mjs` 与 `web/app.js`。
所以 **wasm pkg 不需要重建**（pkg 仍与 `57e5fda` 一致，`check-web-invariants` 绿）。

### 覆盖边界（不假装）

* **弹幕不进墨迹报告**：`dhampir_project_text_probe` 仍旧只判字幕行 —— 弹幕每趟进出
  必然经过画面边，"墨迹被切"是常态而不是缺陷（T3.3c 的 `judge_clip` 已经这么分了），
  所以 `probe.lines.length === placements.length` 这条判据不动，弹幕的墨迹**只有肉眼
  那一条路**（帧 0 的弹幕确实画上去了没有？证据里只有"清单里有、位图画了"这一层，
  没有"像素落在哪"那一层）；
* **只验了 5 帧**（0/60/120/180/240），弹幕的**相接帧**（119/120、179/180）没逐帧验 ——
  闭区间边界的规则由 `danmaku::layout` 的单测钉住，判定通道只覆盖这五帧；
* **`\move` 坐标仍不参与**：素材里写了 `{\an7\move(640,22,-260,22)}`，两端都**不读**它
  （泳道由 spec 分配）。这一条是**刻意的**，不是漏了 —— 但也就意味着"素材自带坐标"
  这条路没有迹可循，将来要支持它得另开一份口径；
* **侧挂 ASS 的弹幕半边不接线**：`to_ass_danmaku`（带 `\move`）有单测但没接进
  `--subtitle-out`（T3.3b 已记：一份 ASS 只有一个 Style，字幕字号来自
  `AssStyle::font_size`、弹幕字号来自 `font_ratio × 序列高`，合进同一个文件要动共享的
  `ass_header`），这一段没有改变那个边界；
* **字体仍是两端各自一份**：字形像素本来就允许不同（T2 的验收口径），弹幕同理 ——
  比的是结构，不是像素；
* **判定耗时长**（单次约 1–3 分钟：真实浏览器 + 本机后端 + 每帧一次 CLI 对照），
  所以它不在 17 个守卫里，是**手动**通道 —— 这份证据就是它的原始输出；
* **没有量过速度**：`evaluate_overlay` 每帧对**整条素材**重算一次 `danmaku::layout`
  （纯函数的天性，O(条数 × 泳道数)，边界写在 `crates/dhampir-core/src/overlay.rs` 的注释里）。
  T3 的验收是「结构与丢弃数一致」，不依赖速度，所以这一轮没造大 ASS、也没测 ——
  `plan/measurements.md` 的「未做」里记了这笔。
