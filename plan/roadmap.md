# Dhampir 阶段路线图（T0–T7）

写于 P7.4a 收口之后。缺陷与架构缺失的**唯一台账**在 [defects.md](./defects.md)；
本文件是**怎么修**与**按什么顺序修**，台账是**修到什么程度算完**。

**这份文件是什么**：长期任务的分段计划。每段都独立可验收，且**每段结束时全仓必须是绿的**。
**它不是什么**：不是决策真相（真相在 plan/video-editor-plan.md 与 plan/video-editor-tech-guide.md）。

## 硬规矩（每段都适用）

1. 台账里每一条 id 都必须在本文件被引用；本文件引用的每个 id 都必须在台账里存在（守卫核对）。
2. 每段结束：cargo test --workspace 全绿、warning（--all-targets 口径）为 0、
   全部守卫与各自的 --self-test 全绿、工作区干净并提交。
3. 只勾真做完的。状态改 done 必须同时在台账里给出**存在的证据文件**。
4. 每段补一次测量（plan/measurements.md），口径与边界一起写。
5. records/ 不得编辑；target/ 是 gitignore 的草稿区。
6. 不为让某一段变绿而放宽已定的判据；判据要改就改在文档里并说明理由。

---

## T0 可观测性 + 台账（本轮）

只做可观测性与台账，**不动渲染语义、不动契约版本、不动 G5 的字节**。

- T0.0 登记长期目标。
- T0.1 plan/defects.md（台账）。
- T0.2 scripts/check-defects.mjs（第 9 个守卫）：--self-test、反向用例（断言"因为那一条红"）、
  拒绝空集合、只设 process.exitCode、判定写成纯函数使自检与真跑同路、每个数实数上报、
  detail 最多 6 条后接"另有 N 处"。退出码：不认识参数 2、--help 0、自检失败 2
  （先打印"先修守卫，别信它的结论"）、台账缺失 2、有红项 1。
- T0.3 plan/roadmap.md（本文件）。
- T0.4 **预览判定回传通道**（A8、D8）：本地后端加 POST /verdict 与 GET /verdict；
  web/app.js 暴露 window.dhampir.reportVerdict；scripts/web-check.mjs 加 --verdict，
  拿到就给明确结论，**拿不到就明说没拿到，不得静默通过**。
  用它补上那条一直没成立的判据：**预览里执行一次 trim 后的 doc 指纹 == CLI 执行同一 op 的指纹**。
- T0.5 plan/remaining-work.md 就地标注（M2 时代内容已陈旧，加指针，不删历史）。
- T0.6 收口：测试、9 个守卫与自检全绿、文档里写死的守卫数量同步、按主题提交。

**验收**：check-defects 自检与真跑都 EXIT=0，且抠掉一条状态会因那一条变红；
trim 指纹判据给出明确结论并留下证据文件。

---

## T1 文档坐标系（正确性，最高优先） → 依赖 T0

**状态：已完成**（见 [t1-evidence.md](./t1-evidence.md)；D1、A1 已转 done）。
实际做法与开工时的设想有两处不同，都记在下面 —— **计划与事实不一致时改计划**：

1. 没有让「预览也按 render_hints 渲染再缩放到画布」，而是**让 core 收下坐标系**
   （RenderSpace{sequence,target}），预览仍在画布尺寸上渲染、只换算一次比例。
   结果是**零额外像素开销**，所以 T1.4 担心的「1080p 预览变慢」没有发生，
   逃生口 preview_scale **不需要**。
2. 台账里「gaussian_blur.radius 也要换算」是**错的**：每层那个半径跑在**源**纹理上，
   是源像素、不换算；只有调整图层的模糊半径是文档像素、要换算。

- T1.1（A1、D1）dhampir-core 接收 sequence_size：
  inverse_affine(transform, source_size, sequence_size, target_size)；
  当 target != sequence 时按 target/sequence 缩放**全部像素量**。
  实核只有 transform.x/y 是**文档像素**（SubtitleStyle 已经全用比例，说明这个结论
  此前被局部发现过，但没上升为架构）。adjst 图层的模糊半径也是文档像素，一并换算。
- T1.2 规则：**文档坐标系 = render_hints**（缺省 1920x1080）。
  导出默认 = render_hints；预览默认按 render_hints 渲染，显示层只做缩放。
- T1.3 显式给不同导出尺寸 -> 走 T1.1 的缩放路径，并在渲染报告里点明"这是缩放输出"。
- T1.4 预览性能风险：1080p 预览可能太慢。逃生口是显式的**预览档** preview_scale，
  它走同一条缩放路径，因此几何仍一致。**先测量再定默认档**。
- T1.5 新守卫 scripts/check-preview-parity.mjs（--self-test 加反向用例）。

**验收（已达成）**：里程碑 milestones/edited-milestone.mp4 逐字节未变（33287 字节）；
G5 SSIM 1.000000；core 单元测试四种目标尺寸下归一化落点一致 + 一条反向用例；
scripts/check-preview-parity.mjs 与它的 12 条自检断言全绿。
**也已达成**：端到端那一半由 crates/dhampir-worker/tests/preview_parity.rs 钉住 ——
自造一块有区分度的源（中间一块不透明、四周透明），三种目标尺寸量包围盒落点，
外加一条反向用例（用目标尺寸当坐标系时必须明显不同）。默认 #[ignore]，需真 GPU。

---

## T2 文字与字幕上屏（P7.4b） → 依赖 T1

**状态：已完成（T2 收口）**。七步（T2.1 到 T2.7）全部落地，证据在 [t2-evidence.md](./t2-evidence.md)。

没有 T1 就不能做这段：文字位置在归一化坐标下是对的，一换成像素就会跟着分辨率漂。

**开工决定（先落文档，再写代码）**：共享布局的字形度量用**按字宽分类的模型**，
不引第三方 crate、也不问宿主拿度量。理由是 A3 的目标是**两端结构一致**：
让宿主把度量喂回来（measure-then-layout）看起来更准，但那样布局就依赖宿主，
两端结构可能分叉 —— 与目标相反。

| 类别 | 前进宽度 | 例子 |
|---|---|---|
| 全角 | 1.0 em | CJK 汉字/假名、全角标点 |
| 半角 | 0.5 em | ASCII 字母数字与半角标点 |
| 空格 | 0.25 em | 空格；制表符按一个空格算 |

行高 1.2 em；字号 = font_ratio × 文档坐标系的高度；行盒按 bottom_margin 从底部往上排；
超过 max_lines 的部分**丢弃并计数**（与弹幕的丢弃口径一致，不叠、不缩）。
换行：CJK 逐字断行，拉丁按空格断行，一个词长于一行时硬断。

**代价写清楚**：与真字体度量有偏差，等宽与窄字（i/l 与 W/M）会让换行点与肉眼预期不同；
调 font_ratio 只能整体挪，不能让两端更接近。它只影响**换行点**，不影响字形像素 ——
而 T2 的验收口径要的正是「结构一致、字形允许不同」。

- T2.1（A4、D5）契约 —— **评估层已完成**；两端「真的把字画出来」在 T2.3 / T2.5 里接上。
  **计划改了，理由记在这里**：没有把 overlay 塞进 Composite，而是新出一份结果
  （dhampir-core::overlay 的 evaluate_overlay）。因为文字的「要画什么」与图层的
  「哪张纹理怎么叠」是两件事：文字没有纹理，它要先由宿主栅格化。塞进一个结构里会让
  「谁负责栅格化」变含糊，而含糊的代价是两端各自决定。
  代价是宿主必须**两个都调**，所以接线要由结构守卫盯着（与 check-preview-parity 同一手法）——
  该守卫写在 next-steps.md 的 T2.6 里，两个宿主都接上之后才写（提前写就是恒真守卫）。
  **已落地**：`scripts/check-overlay-plumbing.mjs` —— 一条通用律（渲染了帧、又读了时间线的文件
  必须评估 overlay）加两个钉死的宿主，18 条自检与两个真实反向用例（见 t2-evidence.md）。
  已落地：TextItem（归一化矩形）/ TextOverlay / SubtitleTable / evaluate_overlay（13 条单测），
  以及它的第一个真实调用方 —— CLI 的 subtitle 子命令（`dhampir subtitle --project P --frame N`，
  只出结构、不需要 GPU，宿主的输出与它不一致就是宿主错）。
  **已完成**（T2.7，见 [t2-evidence.md](./t2-evidence.md) 的 T2.7 段）：HOST_API_VERSION 升到 2
  （版本号**不进返回体**，由 `dhampir_host_api_version` 问一次，`docs/host-api.md` 作人读的那一份、
  由 `scripts/api-surface.mjs` 钉住）、--subtitle-out 侧挂导出（先写文件再出片；时间重定基到这一趟产物；
  格式「明说的优先，没明说看扩展名」，打架与认不出都退 2）；弹幕见 T3。
  **编号**：本文件早先把这两项拆成 T2.6 / T2.7 两条，next-steps.md 里它们合起来是 T2.7
  （T2.6 在那边是结构守卫）—— 以 next-steps.md 的 T2 剩余表为准。
- T2.2（A3）共享布局 —— **已完成**（见 [t2-evidence.md](./t2-evidence.md)）：
  crates/dhampir-timeline/src/text_layout.rs，零依赖，17 条单测含 2 条反向用例；
  参照输出 crates/dhampir-timeline/examples/layout_subtitles.rs（宿主与它不一致就是宿主错）。
  字形由宿主栅格化，但**结构与归一化矩形必须一致**。
- T2.3（A4）两宿主都追加 overlay：**单趟与分段两条路径都要**，各留一个用例（这里最容易漏）。
- T2.4 CLI：text_raster.rs，每项一张 PNG（ffmpeg drawtext textfile=...）、带缓存、自做 alpha 合成；
  加 --font-file。
- T2.5 浏览器：canvas 栅格化加 createImageBitmap(canvas, premultiplyAlpha none)，
  走 T0.4 的判定通道验证（不再依赖 --exec）。
- T2.6（D5）侧挂导出 --subtitle-out：**burned-in 与 sidecar 两种都要** —— **已完成**（T2.7 的一部分）：
  burned-in 仍是 `--font-file` 那条路，sidecar 是 `--subtitle-out` 加可选 `--format srt|ass`，
  四条 CLI 判据与五个真实变异见 t2-evidence.md。**弹幕不在这一段**（那是 T3 —— 弹幕接上后
  D5 在 T3 收口，见下面的 T3 段；T2 当时只到「字幕」这一半）。
- T2.7（A4）HOST_API_VERSION 从 1 升到 2（FrameResult 加字段）—— **已完成**：`FrameResult.overlay`
  （形状在 `crates/dhampir-timeline/src/host_api.rs`，与 CLI / wasm 逐字段同名；None 时**不出现该键**），
  `dhampir_host_api_version()` 加 `docs/host-api.md`，两条都由 `scripts/api-surface.mjs` 钉住。

**验收（已达成，T2 收口）**：无文字工程**逐字节不变**（`target/t2/byte-identical.cjs` 的对账：
22552 字节、SHA256 42E6195C…D21A6 同参数出片一致）；带字工程两端结构（项数/文本/归一化矩形）一致，
容差写死在 `scripts/web-check.mjs`（`SUBTITLE_TOLERANCE = 1e-6`）；G5 仍绿（`check-dual-end`：最差 SSIM 1.000000）。

---

## T3 弹幕（P7.5） → 依赖 T2

**状态：已完成（T3 收口）**。证据在 [t3-evidence.md](./t3-evidence.md)；**D5 已转 done**
（它的两条验收这一轮都核过了：带字工程两端结构一致且容差写死 —— T2.5 的字幕那半加 T3.4 的
弹幕那半；无文字工程逐字节不变 —— 22552 字节 / SHA256 42E6195C…D21A6 重跑仍一致）。

- T3.1（D5）dhampir-timeline::danmaku：parse_ass_danmaku 加**共享的 layout() 泳道分配**。
  分配必须是确定性的；泳道耗尽时**丢弃并计数**，不许叠。 —— **已完成**：
  `DanmakuItem{text,lane,enter,exit}`（**矩形不进结构**，一帧一个答案），
  `layout()` 顺序即优先级、`上一条 exit < 这一条 enter` 才复用泳道（闭区间边界不许共用）、
  排不下与 `lanes == 0` 都丢弃并计数、坏时间基丢弃并计数；`rect_at` 进入帧 x=1、
  离开帧 x=-width、中间线性（线性是**可复算**的选择）；在屏时长取自 `duration_ms`，
  素材 ASS 的 `End` 不参与。
- T3.2（A4）求值进共享 overlay；to_ass 带 \move。 —— **已完成**：`evaluate_overlay` 加
  Danmaku 轨循环，`TextOverlay.danmaku` + `dropped_danmaku`，None 判据改为**两半都空**；
  `to_ass_danmaku` 落点复用 `rect_at`（同一份数学）。宿主 API 因此升到 **3**
  （`OverlayView.danmaku` / `dropped_danmaku`，T3.2c）。
- T3.3 与 T2.2 同一条规矩：结构与归一化矩形一致，字形像素允许不同。 —— **已完成**：
  CLI `subtitle` 加 `danmaku`/`dropped_danmaku` 两键（T3.3b）；wasm 宿主清单/位图两份、
  弹幕**不做切线判失败**（`judge_clip` 是显式入参，T3.3c）。

**验收（已达成，T3 收口）**：同一输入在两端给出**相同**的 (text, 泳道, 进入/离开帧)；
丢弃数一致。落点是 `--verdict subtitle` 那条既有通道（不新开）：五个帧
0/60/120/180/240 逐条比 `(text, lane, enter, exit)` 与 `dropped_danmaku`，
矩形按 `SUBTITLE_TOLERANCE = 1e-6` 比 —— 实测 `弹幕结构一致（7 条·泳道/进入/离开帧 +
丢弃 1 条）`。另加两条**非空白**判据（工程有弹幕轨时，一条都没见到 / 一帧都没丢过 → 红），
三个真实反例都真的红了（泳道改一格、`lanes: 5`、`lanes: 0`），还原后逐字节相同。

---

## T4 编辑模型补完 → 依赖 T0（与 T2/T3 的写域需要排开）

**状态：已完成（T4 收口）**。设计先落文档（[t4-design.md](./t4-design.md)），证据在
[t4-evidence.md](./t4-evidence.md)；**D2 / D6 / A2 三条已转 done**（三条验收这一轮都核过：
切缝两侧端点值精确、undo 逐字节回到原文件、CLI 与预览共用同一条历史）。

- T4.1（A2、D6）dhampir-timeline::history（零依赖）：push/undo/redo、上限、可选合并；
  CLI 与预览共用同一条历史。 —— **已完成**：`history.rs` 存**整份快照**（不是反向 op），
  上限 64，9 条单测；CLI 侧 `edit --undo` / `--redo` / `--history` 三个只有 `edit` 认；
  wasm 侧 `dhampir_project_undo` / `dhampir_project_redo` 加预览那两颗真按钮。
  `push_coalescing` 有实现与单测，但**生产路径零调用点**（T4.3 换了做法，见下）。
- T4.2（D2）split 支持关键帧：切点两侧各插"当时的值"。
  **注意依赖方向**：timeline 不能依赖 core，所以曲线求值要么在 timeline 里实现一份，
  要么把求值下沉到 timeline 供两端共用。**这是本段的架构决定，必须先落文档再写代码。**
  —— **已完成**：选 **B（下沉 timeline）**，判据是「有像素的地方逐字节不变 + core 既有关键帧
  用例一字不改继续绿」；`curve.rs` 的 `opacity_from` 逐行照抄、`compose.rs` 改成转发；
  `split` 删掉拒绝分支，两半各**条件插入**切缝键（左 `local-1`、右整体左移后 `0`）。
  承诺收窄为**「切点两侧的端点值 == 原曲线在切点的值」**（4 种缓动 × 3 组键配置的量化表
  在证据文档），**不承诺逐帧精确**；没有关键帧的元素走老路径、逐字节不变。
- T4.3 预览拖拽/吸附：时间线上的点击与拖动生成 edit op，不加新契约。 —— **已完成**：
  不新增 EditOp 变体，拖拽只生成已有的 `{op:"move", layer, to}`；吸附半径 6 px 换算成帧；
  `web/app.js` 里**一个业务错误码字面量都没有**（守卫盯着）。落点只在 **pointerup** 提一次 ——
  **偏离设计文档 4.2 的「中间落点用 push_coalescing 合并」**，理由与代价（拖动中看不到
  "会被拒"）已在证据文档写明。

**验收（已达成，T4 收口）**：CLI 有 --undo/--redo；op 之后再 undo 得到逐字节相同的 doc ——
实测两趟都逐字节回到当初那一份（e2e 22 条全 ok，两份哈希全串见证据文档）。
拖拽吸附落**既有判定通道** `--verdict undo-drag`（不新开：驱动按回传 `kind` 分派）：
原始落点第 1 帧（不是任何边界）→ 吸到第 0 帧（别人的边界，半径 `round(6 × 90 / 381) = 1` 帧）；
同一条 `move` 由 CLI 复算后与"拖动后"逐字段相同；撤销逐字段回到拖动前、重做原样放回来。
第一次真机跑是**红的**（场景挑错：固定拖一条已排满的轨道 → `layer_overlap`），根因、修法与
两次原始输出都在证据文档 —— 判定函数**不许预测** Rust 会不会接受。
`HOST_API_VERSION` 因此升到 **4**（导出面 +2）。

---

## T5 解码与素材通路（规模） → 独立；先测量

**状态：已完成（T5 收口）**。证据在 [t5-evidence.md](./t5-evidence.md)；**D3、D4、A6、A7、
A10、D11 六条已转 done**。数字与边界在 [measurements.md](./measurements.md) 第七项。
两处与开工设想不同，都是量出来才看清的，记在下面：

1. **"回退距离"不是池子要多少槽的判据** —— 判据是「这个源上还有几个没轮到它的请求」。
   实测里回退距离 p50 是 98 帧，但画中画那种形态**只要留住 29 帧**、倒放那种要 60 帧。
   所以池子上限按"要留住几帧"反推（256 MiB / 64 帧），而不是按距离；
2. **量化的数不许从渲染过程里猜，也不许自称同源** —— 所以池子被抽成纯状态机
   （`PoolCursor`），量化脚本与产品跑同一段规则；收口时两边的数**一位不差地对上了**
   （t5-evidence.md 判据一那张表与 measurements 7.2 的 32 槽那一行）。

- T5.1（A6、D3）**先量化**真实多素材工程里"回退"有多常见，再决定做有界回退窗口
  还是按源内顺序重排渲染。没有数字就不许决定。
- T5.2（A7、D4）SourceTexturePool（N 槽，键 (source, frame)）。
- T5.3（A10、D11）并发数给出并写进 measurements.md；
  同时把出片吞吐"是下界"这件事的口径写清楚。

**验收（已达成）**：倒序引用同一素材的工程能出片，且与顺序引用版本逐像素一致 ——
由 `crates/dhampir-worker/tests/rewind.rs` 在真机上钉住（90 帧摘要逐帧相等，
含一条"改坏命中路径必须变红"的反向用例）。**mp4 编码那一腿在 agent 会话里跑不了**
（坑 19），边界写在 t5-evidence.md。

---

## T6 音频（功能） → 独立

**状态：已完成（T6 收口）**。证据在 [t6-evidence.md](./t6-evidence.md)；**A5、D7 两条已转 done**。
数字与边界在 [measurements.md](./measurements.md) 第八项，口径在
[consistency-criteria.md](./consistency-criteria.md) 的「音视频同步口径」一节。
三处与开工设想不同，都是做出来才看清的，记在下面：

1. **"每段起一路解码器、按采样点裁"比"开一路往后丢"更好** —— 后者每段读够就得
   提前掐掉解码器，而 ffmpeg 会因此往 stderr 吐 `Error submitting a packet to the muxer`；
   一次成功的出片不该有那句话（它会让人去查一个并不存在的问题）。
   代价量过了：主要是进程启动（每段约 150 ms），**与这段在第几秒关系很弱**（60 秒整段也才 194 ms）。
2. **"逐字节不变"这句验收要求必须变成可执行的判据**，否则它只是一句承诺 ——
   所以编码器的命令行抽成了纯函数 `encoder_args`，并配一条与**冻结的黄金 argv** 逐项比的测试。
3. **`AudioPlan` 由 `render_plan` 从同一份入参摊出来，不接受调用方另传一份** ——
   "同源求值"要的就是同一份入参；让调用方传一份计划进来，两边迟早分叉。

- T6.1（A5、D7）AudioPlan：与视频 RenderPlan **同源求值**；
  帧号与采样数用**有理数**换算（复用 timebase），不许用浮点秒。
- T6.2 消费媒体层**已经声明**的 audio_info / add_audio_packet
  （trait 在，零调用：这是接线缺口，不是从零造）。编码侧从"只喂裸帧 stdin"
  变成视频加音频两路。
- T6.3 A/V 同步口径写进 plan/consistency-criteria.md。

**验收（已达成）**：产物带音轨，时长与视频一致（正负 1 帧）；原有无声路径仍可选且逐字节不变 ——
由 `crates/dhampir-worker/tests/audio.rs` 在真机上钉住（音轨与视频**差 0.000000 秒**、
视频码流 SHA256 与复用前逐字符相同、PCM 与独立解出的参考逐字节相同）。
**"GPU 那 90 帧 + 音轨整条出片"在 agent 会话里验不了**（视频编码器要 stdin 管道，坑 19），
那一半要普通终端里跑 `check-cli`；边界写在 t6-evidence.md。

**一处对照**：媒体层那 5 个 trait 仍然是**零实现**（`check-media-status` 盯着），
本段用的是它的**词表**（`AudioInfo` / `SampleFormat`），不是替它写实现 ——
那个 crate 描述的是"in-process 的一条腿"，而 worker 与 ffmpeg 说话走的是子进程。

---

## T7 交付面收口 → 各段之后

- **T7.1** ✅ CLI 具名子命令（clip / sequence / undo / redo / 批处理脚本）——**同一实现的糖**。
- **T7.2** ✅（D12）驱动检测到陈旧 wasm pkg 时自动重建一次，并保持"真的坏了仍然红"。
- **T7.3** ✅（A9）渲染任务的持久化与续渲：**明确记为不做**，理由与数字见 [t7-evidence.md](./t7-evidence.md) 的 T7.3。
- **T7.4** ✅（D16）Linux 可移植性守卫（见下一节）。
- **T7.5** ✅ api-surface 重生成、docs/ 同步、台账计数用实数上报；**守卫清单落仓库**（`scripts/run-guards.mjs`）。

**验收**：CLI 子命令有契约检查（`scripts/check-cli.mjs`：30 条判据 / 14 个子命令）；
D16 的检查能因写死 Windows 语义而变红（在真树里放探针 -> 退出码 1、三条、行号都对）。
**两条都满足。**

**T7 整段已收口**（详见 [t7-evidence.md](./t7-evidence.md)）。四处与开工设想不同，记在下面：

1. **「糖」的判据不是"看起来一样"，是"逐字节一样"**：具名写法与等价的 `edit --op`
   被钉在**产物字节 + stdout** 上（单测比 `EditOp` 逐字段，契约检查比文件与输出）。
   只比"summary 文案对得上"会放过缩进、issues 顺序、字段缺省值这三类分叉。
2. **动作名做成第二个位置参数**（`clip split …`），而不是又一层 `--op-name` 开关；
   每个动作**要哪些开关、认哪些开关**都在解析阶段判死，**多给一个也报错** ——
   多给比少给更隐蔽：那个开关会被静静丢掉，而用户以为它生效了。
3. **`batch` 是一次写、一条历史**（不是一个"循环调 N 次 edit"的宏）：中途有一步不成立
   就**整份不落盘**并明说卡在第几行；空脚本**不算成功**（它会打印 ok 却什么都没做，
   与"脚本路径写错了"分不开）。
   顺带修掉一处**我自己引入又自己抓到**的回归：`--write` 原先被收紧成"只有编辑那一组认"，
   而 `import` 也要落盘 —— 契约检查里 `import-write` 当场变红，于是两张表分开
   （`EDIT_FAMILY` 与 `WRITE_FAMILY`），并加了一条单测钉住"import 认 --write 但不认 --history"。
4. **D16 的"不许盘符"那一条，量完之后决定不做。** roadmap 原文写的是"不许手写反斜杠**或盘符**"，
   但先量了一遍：整棵树有 **12 个文件**含 `C:/…`，几乎全是**正当**的 —— 字体候选表
   （`C:/Windows/Fonts/msyh.ttc`）、Chrome 候选表（**它还同时列了 `/usr/bin/google-chrome`**）、
   以及单测里当输入的假路径。禁掉会造出一批**假红**，而假红比没有守卫更坏（人会开始无视它）。
   所以只实现"不许反斜杠"，并把这条不一致写进守卫的「这条守卫不管什么」一节 —— **不默默放过**。

**T7.3 的结论（不做）**：出片那条路是流式的（GPU 合成 -> 读回像素 -> 写进 ffmpeg 的 stdin），
MP4 基本流不能从第二个进程接着写，所以"续渲"最多只能是逐帧检查点。实测检查点
**8.29 MB/帧**（1080p30 一分钟 14.93 GB）、裸 RGBA 落盘 **2.81 ms/帧**，
对照渲染 14.56~56.96 ms/帧 —— 代价与收益不在一个量级。重审触发条件写在 t7-evidence.md 的 T7.3。

---

## 跨平台（Linux）考虑

**已决定**：Linux 的两条取证腿**不跑**（D13，状态 wontfix）——本机没有 docker，WSL 也没有发行版。
**但没有取消考虑**：不跑腿意味着**只能靠静态检查兜住**，所以 D16 要求补齐三类检查：

1. 路径一律用 join/resolve 拼接，**不许**手写反斜杠或盘符；
2. 行尾统一 LF（已由 check-text-hygiene.mjs 覆盖），新增文件不得引入 CRLF；
3. 不许假设路径大小写不敏感，也不许假设文件名编码。

如果能装上 x86_64-unknown-linux-gnu target，就补一次 cargo check 作为交叉验证；
装不上就如实记为不可测，**不假装跑过**。

**D16 已落地**（`scripts/check-linux-portability.mjs`，细节见 [t7-evidence.md](./t7-evidence.md) 的 T7.4）：

* 上面第 1 条只实现了"不许反斜杠"，"**盘符**"那半条量完之后按"会造假红"驳回（理由见 T7 段第 4 点）；
* 第 2 条仍由 `check-text-hygiene.mjs` 覆盖，D16 只**交叉声明**那个载体还在，**不重复实现**；
* 第 3 条拆成两条规则（拿小写化的**整条路径**去访问文件系统 / 假定路径一定是合法 UTF-8）。

**交叉 cargo check 真跑了**（不是"记为不可测"）：本机本来就装着那个 target，退出码 0。
但要连边界一起记：它的范围只是"**我们自己的代码在 Linux cfg 下类型正确**"——
原生媒体栈按平台门控，在 Linux 依赖图里**根本不存在**（116 个 crate，对 Windows 的 134 个），
所以这**不等于"Linux 上能跑"**。正向对照做了：故意写坏一行 -> 交叉 check 变红，源码还原后逐字节相同。

---

## 明确不做（写进台账，不假装）

| id | 内容 | 为什么 |
|---|---|---|
| D9 | 素材上传与素材 UI | 单机可用、远端不可用；属下游工程 |
| D10 | 真实远端部署与 CI | 已移出范围；--remote 只证明代码路径跨源 |
| D13 | Linux 两条取证腿 | 用户决定不跑（考虑见上一节） |
| D14 | 与浏览器逐像素对齐的色彩矩阵 | 后端 bt709 与 WebCodecs 不同源，记为架构限制 |
| D15 | 含解码的逐像素双端比对 | 两端解码路径不同，**架构性不可测**，不是待办 |

---

## 依赖总览

    T0 ─┬─> T1 ──> T2 ──> T3
        ├─> T4
        └─> T7
        T5（独立，先测量）
        T6（独立）
