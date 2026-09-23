# Dhampir 缺陷台账

写于 P7.4a 收口之后（HEAD 870e1ec，工作区干净）。

**这份文件是什么**：已知缺陷（D）与架构缺失（A）的**唯一台账**，也是长期任务的分段依据。
**它不是什么**：不是决策真相。真相仍是 plan/video-editor-plan.md 与 plan/video-editor-tech-guide.md；
本文件只补它们没有的两样：① 缺陷的**准确状态**；② 每条的**验收方式**。

阶段计划在 [roadmap.md](./roadmap.md)；本文件的每一条 id 都必须在那里被引用（否则守卫判红）。

## 格式（机器解析，不许改列）

    列出条目：- [<id>] status=<status> phase=<phase>
    随后四行（各自一行，缩进两格）：症状: / 根因: / 验收: / 证据:

- id 形如 D1、A1。
- status 取 todo | doing | done | wontfix | unmeasurable。
- phase 取 T0..T7 之一，或 - 表示不归属任何阶段。
- 根因是一串**空格分隔的记号**，每个记号要么是仓库内真实存在的路径（可带 :行号 或 :起-止），
  要么是另一个条目 id。至少一个。
- 证据是 - 或一串同样形式的真实路径；**status=done 时必须给出至少一个存在的文件**。
- 计数声明行 <!-- ledger: D=16 A=10 --> 必须与实际条数一致。

<!-- ledger: D=16 A=10 -->

## 缺陷（D）

- [D1] status=done phase=T1
  症状: 同一工程在 640x480 预览与 640x360 成片里，图层位移的相对位置不同（预览所见 != 成片所得）
  根因: crates/dhampir-core/src/render/compose.rs:107 web/app.js:592
  验收: core 单元测试「位移的归一化落点与目标尺寸无关」（640x360/320x180/1280x720/960x540 四种目标，容差 1e-5）加一条反向用例；接线由 scripts/check-preview-parity.mjs 钉（自检 12 条断言，10 条是反向用例）；真机端到端由 crates/dhampir-worker/tests/preview_parity.rs 钉（三种目标尺寸量包围盒落点，含一条反向用例）
  证据: plan/t1-evidence.md

- [D2] status=todo phase=T4
  症状: 带关键帧的元素不能剃刀（split 直接拒绝，不是悄悄切歪）
  根因: crates/dhampir-timeline/src/edit.rs:577
  验收: 切点两侧的端点值 == 原曲线在切点的值（两侧各插一个当时的值）
  证据: -

- [D3] status=todo phase=T5
  症状: 素材在时间线上前后颠倒时整次出片失败（source_rewind）
  根因: crates/dhampir-worker/src/pipeline.rs:557
  验收: 倒序引用同一素材的工程能出片，且与顺序引用版本逐像素一致
  证据: -

- [D4] status=todo phase=T5
  症状: 同一输出帧里同一素材需要两个不同源内帧时失败（source_frame_conflict）
  根因: crates/dhampir-worker/src/pipeline.rs:544
  验收: 同素材画中画的工程能出片；纹理池按 (source, frame) 键复用
  证据: -

- [D5] status=todo phase=T2
  症状: 字幕与弹幕存得下、查得出、画不出（解析器与契约校验都在，渲染阶段不存在）
  根因: crates/dhampir-timeline/src/subtitle.rs:1
  验收: 带字工程两端结构（项数/文本/归一化矩形）一致且容差写死；无文字工程逐字节不变
  证据: -

- [D6] status=todo phase=T4
  症状: 不能撤销/重做，编辑不可逆
  根因: crates/dhampir-timeline/src/edit.rs:577
  验收: 执行一个 op 再 undo，得到与执行前逐字节相同的 doc；CLI 与预览共用同一条历史
  证据: -

- [D7] status=todo phase=T6
  症状: 产物没有声音（编码器只吃 rawvideo stdin，没有任何音频输入）
  根因: crates/dhampir-worker/src/pipeline.rs:661 crates/dhampir-media/src/lib.rs:271
  验收: 产物带音轨，时长与视频一致（正负 1 帧）；原有无声路径仍可选且逐字节不变
  证据: -

- [D8] status=done phase=T0
  症状: 预览里跑的 JS 拿不到结果（awaitPromise 与 returnByValue 同用时本机 Chrome 回空对象），预览侧的断言无法自证
  根因: scripts/web-check.mjs:327
  验收: 页面把判定主动回传到本地后端，驱动读后端并给出明确通过/失败；拿不到就明说没拿到，不得静默通过
  证据: plan/t0-evidence.md

- [D9] status=wontfix phase=-
  症状: 加素材只能按路径，浏览器里没有上传
  根因: crates/dhampir-worker/src/bin/dhampir.rs:1
  验收: 记为不做 —— 单机可用、远端不可用；上传与素材 UI 属下游工程
  证据: -

- [D10] status=wontfix phase=-
  症状: 真实远端部署没验过
  根因: scripts/check-backend-seam.mjs:1
  验收: 文档写明未验，不假装；--remote 只证明代码路径跨源
  证据: -

- [D11] status=todo phase=T5
  症状: 出片吞吐是下界（四个 asset 指向同一文件）；并发数没有落进测量
  根因: plan/measurements.md:1 scripts/measure-concurrency.mjs:1
  验收: 补测并把口径与边界写进 measurements.md（拿带边界的数当结论用比没有更危险）
  证据: -

- [D12] status=todo phase=T7
  症状: 改完 Rust 后守卫变红（陈旧 wasm pkg），需要手工重建
  根因: scripts/check-web-invariants.mjs:1
  验收: 驱动检测到陈旧时自动重建一次，并保持"真的坏了仍然红"
  证据: -

- [D13] status=wontfix phase=-
  症状: M1 的环境矩阵缺 Linux 两条腿（本机无 docker、WSL 无发行版）
  根因: plan/p1-p5-status.md:1
  验收: 用户决定不跑腿 —— 但**设计必须考虑**（Linux 相关考虑由 D16 覆盖），台账如实记为已知缺口
  证据: -

- [D14] status=wontfix phase=-
  症状: 色彩矩阵两端不同源（后端显式 bt709，浏览器由 WebCodecs 决定）
  根因: crates/dhampir-worker/src/pipeline.rs:19
  验收: 记为架构限制，不追求与浏览器逐像素对齐
  证据: -

- [D15] status=unmeasurable phase=-
  症状: 含解码的逐像素双端比对测不了
  根因: scripts/check-dual-end.mjs:1
  验收: 记为架构性不可测（两端解码路径不同），不是待办
  证据: -

- [D16] status=todo phase=T7
  症状: Linux 可移植性没有守卫保护（既然不跑 Linux 腿，就更只能靠静态检查）
  根因: scripts/check-text-hygiene.mjs:1
  验收: 新增检查覆盖写死 Windows 语义的做法（路径分隔符、行尾、大小写假设）；若本机装有 x86_64-unknown-linux-gnu target 就补一次 cargo check，装不上则如实记为不可测
  证据: -

## 架构缺失（A）

- [A1] status=done phase=T1
  症状: 文档坐标系没有建模 —— render_hints 只是元数据，渲染器不认它；预览把"显示尺寸"当"渲染尺寸"
  根因: crates/dhampir-core/src/render/compose.rs:107 web/app.js:592
  验收: core 新增 RenderSpace{sequence,target}（必填形参，8 个调用点都要说清坐标系）；target != sequence 时换算像素量。**台账原先写「gaussian_blur.radius 也要换」是错的**：每层那个半径是源纹理像素、不换，只有调整图层的模糊半径是文档像素、要换。默认路径逐字节不变：里程碑 33287 字节未变、G5 SSIM 1.000000
  证据: plan/t1-evidence.md

- [A2] status=todo phase=T4
  症状: 没有文档历史层，edit::apply 已经是纯函数却没有安放"前后 doc"的地方
  根因: crates/dhampir-timeline/src/edit.rs:577
  验收: dhampir-timeline::history（零依赖）提供 push/undo/redo 与上限，CLI 与预览共用
  证据: -

- [A3] status=done phase=T2
  症状: 没有共享文本布局层（换行/对齐/行高需要字形度量，两端各做必然结构分叉）
  根因: crates/dhampir-timeline/src/subtitle.rs:1
  验收: 布局层已落在 dhampir-timeline::text_layout（零依赖、17 条单测含 2 条反向用例），并有参照输出 examples/layout_subtitles.rs；**宿主消费同一条路径**属 A4/D5 的验收，不在本条的范围内
  证据: plan/t2-evidence.md

- [A4] status=done phase=T2
  症状: 没有 overlay 渲染阶段，HOST_API_VERSION 停在 1
  根因: crates/dhampir-timeline/src/host_api.rs:1
  验收: overlay 追加在单趟与分段两条路径上（各留用例）；FrameResult 加字段时 HOST_API_VERSION 升到 2
  证据: plan/t2-evidence.md crates/dhampir-worker/tests/overlay.rs docs/host-api.md

- [A5] status=todo phase=T6
  症状: 没有音频通路：无 AudioPlan，也没有 A/V 同步口径（帧号与采样数之间没有换算基准）
  根因: crates/dhampir-worker/src/pipeline.rs:661
  验收: AudioPlan 与视频 RenderPlan 同源求值；帧号与采样数用有理数换算（复用 timebase），口径写进 consistency-criteria.md
  证据: -

- [A6] status=todo phase=T5
  症状: 解码器是单向游标，没有有界回退/随机访问窗口（这是结构选择，不是遗漏）
  根因: crates/dhampir-worker/src/pipeline.rs:557
  验收: 先量化真实工程里回退有多常见，再决定做有界回退窗口还是按源内顺序重排渲染
  证据: -

- [A7] status=todo phase=T5
  症状: 一路源一张纹理（结构选择）
  根因: crates/dhampir-worker/src/pipeline.rs:544
  验收: SourceTexturePool（N 槽，键 (source, frame)）
  证据: -

- [A8] status=done phase=T0
  症状: 没有页面到驱动的确定性判定通道；唯一通道是 CDP 求值，而它在本机 Chrome 上坏了
  根因: scripts/web-check.mjs:327
  验收: 本地后端提供 POST /verdict 与 GET /verdict；页面用 window.dhampir.reportVerdict 主动回传；驱动据此给出明确结论
  证据: plan/t0-evidence.md

- [A9] status=todo phase=T7
  症状: 渲染任务没有持久化与续渲，一次失败全丢
  根因: crates/dhampir-worker/src/pipeline.rs:1
  验收: 出片能留下可恢复的中间态，或明确记为不做并说明理由
  证据: -

- [A10] status=todo phase=T5
  症状: 单线程顺序解码，吞吐上限写死
  根因: crates/dhampir-worker/src/pipeline.rs:1
  验收: 给出并发数并说明代价；没有数字就不许写进结论
  证据: -
