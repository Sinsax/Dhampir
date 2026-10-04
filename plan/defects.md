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
- 计数声明行 <!-- ledger: D=22 A=10 --> 必须与实际条数一致。

<!-- ledger: D=22 A=10 -->

## 缺陷（D）

- [D1] status=done phase=T1
  症状: 同一工程在 640x480 预览与 640x360 成片里，图层位移的相对位置不同（预览所见 != 成片所得）
  根因: crates/dhampir-core/src/render/compose.rs:107 web/app.js:592
  验收: core 单元测试「位移的归一化落点与目标尺寸无关」（640x360/320x180/1280x720/960x540 四种目标，容差 1e-5）加一条反向用例；接线由 scripts/check-preview-parity.mjs 钉（自检 12 条断言，10 条是反向用例）；真机端到端由 crates/dhampir-worker/tests/preview_parity.rs 钉（三种目标尺寸量包围盒落点，含一条反向用例）
  证据: plan/t1-evidence.md

- [D2] status=done phase=T4
  症状: 带关键帧的元素不能剃刀（split 直接拒绝，不是悄悄切歪）
  根因: crates/dhampir-timeline/src/edit.rs:577
  验收: 切点两侧的端点值 == 原曲线在切点的值（两侧各插一个当时的值）
  证据: plan/t4-evidence.md

- [D3] status=done phase=T5
  症状: 素材在时间线上前后颠倒时整次出片失败（source_rewind）
  根因: crates/dhampir-worker/src/pipeline.rs:172 crates/dhampir-worker/src/pipeline.rs:629
  验收: 倒序引用同一素材的工程能出片，且与顺序引用版本逐像素一致
  证据: plan/t5-evidence.md

- [D4] status=done phase=T5
  症状: 同一输出帧里同一素材需要两个不同源内帧时失败（source_frame_conflict）
  根因: crates/dhampir-worker/src/pipeline.rs:241
  验收: 同素材画中画的工程能出片；纹理池按 (source, frame) 键复用
  证据: plan/t5-evidence.md

- [D5] status=done phase=T3
  症状: 字幕与弹幕存得下、查得出、画不出（解析器与契约校验都在，渲染阶段不存在）
  根因: crates/dhampir-timeline/src/subtitle.rs:1 crates/dhampir-timeline/src/danmaku.rs:1
  验收: 带字工程两端结构（项数/文本/归一化矩形）一致且容差写死；无文字工程逐字节不变
  证据: plan/t2-evidence.md plan/t3-evidence.md

- [D6] status=done phase=T4
  症状: 不能撤销/重做，编辑不可逆
  根因: crates/dhampir-timeline/src/edit.rs:577
  验收: 执行一个 op 再 undo，得到与执行前逐字节相同的 doc；CLI 与预览共用同一条历史
  证据: plan/t4-evidence.md

- [D7] status=done phase=T6
  症状: 产物没有声音（编码器只吃 rawvideo stdin，没有任何音频输入）
  根因: crates/dhampir-worker/src/pipeline.rs:1226 crates/dhampir-worker/src/audio.rs:1
  验收: 产物带音轨，时长与视频一致（正负 1 帧）；原有无声路径仍可选且逐字节不变
  证据: plan/t6-evidence.md

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

- [D11] status=done phase=T5
  症状: 出片吞吐是下界（四个 asset 指向同一文件）；并发数没有落进测量
  根因: plan/measurements.md:1 scripts/measure-export.mjs:1 crates/dhampir-worker/examples/decode_cost.rs:1
  验收: 补测并把口径与边界写进 measurements.md（拿带边界的数当结论用比没有更危险）
  证据: plan/t5-evidence.md

- [D12] status=done phase=T7
  症状: 改完 Rust 后守卫变红（陈旧 wasm pkg），需要手工重建
  根因: scripts/check-web-invariants.mjs:1
  验收: 驱动检测到陈旧时自动重建一次，并保持"真的坏了仍然红"
  证据: plan/t7-evidence.md

- [D13] status=done phase=-
  症状: M1 的环境矩阵缺 Linux 两条腿（当时的判据是本机无 docker、WSL 无发行版）
  根因: plan/p1-p5-status.md:1
  验收: 两条 Linux 腿各归档 5 份文件 + 80 张 PNG，两次独立运行逐字节一致（compare.json identical），probe 摘要等于 golden；守卫按它们自己的口径复核（不跨机器比字节、不套 10 ms 预算）。复核：node scripts/check-m1-record.mjs --record records/m1
  证据: records/m1/linux-gpu records/m1/linux-lavapipe records/m1/README.md

- [D14] status=wontfix phase=-
  症状: 色彩矩阵两端不同源（后端显式 bt709，浏览器由 WebCodecs 决定）
  根因: crates/dhampir-worker/src/pipeline.rs:19
  验收: 记为架构限制，不追求与浏览器逐像素对齐；实测量级（只有基础层的探针，两边同一张画面）：4x4 块均值 4.58、逐通道均值 R 1.53 G 14.14 B 1.42；带变换的层 20.99；复合 14.22。复查：node scripts/web-check.mjs --verdict realframe --local
  证据: docs/api.md

- [D15] status=unmeasurable phase=-
  症状: 含解码的逐像素双端比对测不了
  根因: scripts/check-dual-end.mjs:1
  验收: 记为架构性不可测（两端解码路径不同），不是待办
  证据: -

- [D16] status=done phase=T7
  症状: Linux 可移植性没有守卫保护（既然不跑 Linux 腿，就更只能靠静态检查）
  根因: scripts/check-text-hygiene.mjs:1
  验收: 新增检查覆盖写死 Windows 语义的做法（路径分隔符、行尾、大小写假设）；若本机装有 x86_64-unknown-linux-gnu target 就补一次 cargo check，装不上则如实记为不可测
  证据: plan/t7-evidence.md

- [D17] status=done phase=-
  症状: 发布流程在 Linux 上**出不了产物** —— `scripts/package.mjs` 的压包只认 PowerShell（Linux 上通常没有，实测 ENOENT），流程走到压包就断；而 README 的命题是「同一个底座编译到两个宿主」，服务端出片的目标环境正是 Linux
  根因: scripts/package.mjs:272
  验收: 按平台分叉：Windows 仍走 .NET `ZipFile`（原样不动），Linux / macOS 走纯 Node（只依赖 `node:zlib`，不引 `zip` 命令行依赖）；两条路共用 `verifyZip` 的结果判据。实测产出 `dhampir-0.1.0-linux-x64.zip`（3.68 MB）+ `.sha256.txt`，`bsdtar` 解开后 `bin/dhampir --help` / `probe` / `frame` 全绿
  证据: dist/dhampir-0.1.0-linux-x64.zip

- [D18] status=done phase=-
  症状: 打包出的 zip **不写 Unix 权限位**，解压后 `bin/dhampir` 是 `-rw-r--r--`，**跑不起来**。而"zip 打得开、文件都在"这类自检发现不了它（是 D17 顺带挖出来的：第一版 Node 压包只写了条目名与内容）
  根因: scripts/package.mjs:407
  验收: 中央目录写 `external_attr` 高 16 位（`create_system = 3` = Unix，mode 取低 12 位）；`verifyZip` 增加"非 Windows 上 `bin/dhampir` 必须带可执行位"的判据。**反向验过**：拿未修的那份 zip 跑，判据退出码 1；修后 `bsdtar` 解开是 `-rwxr-xr-x`。注意 `python -m zipfile` / `ZipFile.extractall` **不还原权限位**，验这条要用 `bsdtar` 或直接读中央目录
  证据: scripts/package.mjs:435

- [D19] status=done phase=-
  症状: `cargo fmt --all --check` 是红的（848 处 / 65 文件）——**代码从未按 `rustfmt.toml` 排过**。而 CI 的 `check-native` 跑这条，且工作流明写"不忽略任何退出码"，所以推上去必拦
  根因: crates/dhampir-timeline/src/danmaku.rs:266
  验收: `cargo fmt --all` 后 `--check` 退出码 0。**先排除过工具链漂移**：系统 rustfmt 与钉住的 1.97.0 都是 1.9.0-stable、给出**同一份 diff**，所以不是版本问题。行为未变的证据：生成文档 md5 前后一致、`records/m1` 整表摘要 `71ecc80cade3d73d` 复现、双端 SSIM 1.000000、705 passed / 0 failed
  证据: crates/dhampir-timeline/src/danmaku.rs

- [D20] status=done phase=-
  症状: `cargo clippy --workspace --all-targets -- -D warnings` 报 23 条（原先判成"只在 1.98 上出现"，**实测在钉住的 1.97.0 上同样红，先前判断是错的**）
  根因: crates/dhampir-timeline/src/edit.rs:144
  验收: 逐条处理后 `-D warnings` 退出码 0。其中**三类是 lint 误报，必须保留原判据**：(1) `neg_cmp_op_on_partial_ord` 四处 —— `!(x > 0.0)` 与 `x <= 0.0` **在 NaN 上不等价**，而挡 NaN 正是本仓意图（`text_layout.rs:450` / `:611`、`pipeline.rs:451`、`decode_sequence.rs:126`），有测试 `NaN 也要挡住` 盯着，故 `#[allow]` + 理由，**没有**按 lint 改判据；(2) `too_many_arguments` 三处（`compose` / `paint_one` / `overlay_expected`）—— 参数是 wgpu 或着色器 uniform 的天然形状，拆结构体只是把 lint 关掉而让热路径更难读；(3) `assertions_on_constants` 改为 `const _: () = assert!(...)`，是编译期断言，语义等价。其余是机械修正（`clone_on_copy` 13 处、`bool_assert_comparison` 4 处、`field_reassign_with_default` 6 处，以及 `useless_conversion` / `needless_range_loop` / `derivable_impls` / `useless_vec` / `needless_borrows_for_generic_args` 各 1）。另修掉一处**早就存在**、只因编译中断而没被报出的重复 `#[allow(too_many_arguments)]`（`text_overlay.rs`）
  证据: crates/dhampir-timeline/src/text_layout.rs:450

- [D21] status=todo phase=-
  症状: `scripts/check-m2-record.mjs` 的自检与正跑都红，红在 `diff-images` 一项：归档的 9 张差异图**重编码后与盘上不是同一份字节**（如 `gradient-f000.png` 盘上 2706 vs 重编码 2674）。而守卫自己的规矩是「自检先过才谈结论」，所以**它现在给出的任何结论都不可信**
  根因: scripts/check-m2-record.mjs:3388
  验收: 查清这 9 张图**归档时**用的编码器/参数与现在 `encodePng` 的差异（最可能是 zlib 版本或滤波选择不同），据此二选一：或让判据只比**解码后的像素**而不比容器字节（差异图的价值在像素，不在压缩参数），或按当前编码器**重新归档**这 9 张。判定标准：自检转绿，且换一份已知不同的差异图仍然红
  证据: records/m2/
  备注: **与本次改动无关**。已用 `git stash` 把 `crates/` 全部改动临时撤下复跑，报错**逐字相同**；`records/` 自 2026-10-01 起未动（`git log -1 -- records/` = 7907c22）。属历史归档的取证问题，不影响 0.1.0 产物

- [D22] status=wontfix phase=-
  症状: `check-web-invariants` 在**守卫套件里偶发转红**（单独跑次次绿），红在「wasm pkg 比源码旧」。一度被当成 flake
  根因: scripts/stale-pkg.mjs:45
  验收: 记为**非缺陷**：那是**真阳性**。套件里别的守卫会跑 `cargo` / `wasm-pack`，而 `WASM_SOURCE_PATHS` 里的源目录会被 cargo 写入 —— pkg 与源的 mtime 是**亚秒级**比较，谁新谁旧取决于最后写的是哪边。已确证：手工 `touch` 一个源文件（`dhampir-core/src/lib.rs`）后该守卫**稳定转红**并打印正确的重建命令；`wasm-pack build --dev --target web --out-dir www/pkg` 重建后稳定转绿。即判据本身是对的，只是它把「pkg 与源同步」这件事**如实地**暴露在了套件中途
  证据: scripts/check-web-invariants.mjs:178
  备注: 若要消掉套件内的偶发红，正确做法是**让驱动在跑守卫前统一重建一次 pkg**，而不是放宽判据。这次没做（不在 0.1.0 产物范围内）；单独跑整套守卫时几乎不会遇到。**已实测该处置有效**：先跑一次 `wasm-pack build --dev --target web --out-dir www/pkg`，再跑整套守卫得到稳定 **19/20**（唯一红的仍是 D21）

## 架构缺失（A）

- [A1] status=done phase=T1
  症状: 文档坐标系没有建模 —— render_hints 只是元数据，渲染器不认它；预览把"显示尺寸"当"渲染尺寸"
  根因: crates/dhampir-core/src/render/compose.rs:107 web/app.js:592
  验收: core 新增 RenderSpace{sequence,target}（必填形参，8 个调用点都要说清坐标系）；target != sequence 时换算像素量。**台账原先写「gaussian_blur.radius 也要换」是错的**：每层那个半径是源纹理像素、不换，只有调整图层的模糊半径是文档像素、要换。默认路径逐字节不变：里程碑 33287 字节未变、G5 SSIM 1.000000
  证据: plan/t1-evidence.md

- [A2] status=done phase=T4
  症状: 没有文档历史层，edit::apply 已经是纯函数却没有安放"前后 doc"的地方
  根因: crates/dhampir-timeline/src/edit.rs:577
  验收: dhampir-timeline::history（零依赖）提供 push/undo/redo 与上限，CLI 与预览共用
  证据: plan/t4-evidence.md

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

- [A5] status=done phase=T6
  症状: 没有音频通路：无 AudioPlan，也没有 A/V 同步口径（帧号与采样数之间没有换算基准）
  根因: crates/dhampir-worker/src/audio.rs:1
  验收: AudioPlan 与视频 RenderPlan 同源求值；帧号与采样数用有理数换算（复用 timebase），口径写进 consistency-criteria.md
  证据: plan/t6-evidence.md plan/consistency-criteria.md

- [A6] status=done phase=T5
  症状: 解码器是单向游标，没有有界回退/随机访问窗口（这是结构选择，不是遗漏）
  根因: crates/dhampir-worker/src/pipeline.rs:172 crates/dhampir-worker/examples/rewind_census.rs:1
  验收: 先量化真实工程里回退有多常见，再决定做有界回退窗口还是按源内顺序重排渲染
  证据: plan/t5-evidence.md

- [A7] status=done phase=T5
  症状: 一路源一张纹理（结构选择）
  根因: crates/dhampir-worker/src/pipeline.rs:241
  验收: SourceTexturePool（N 槽，键 (source, frame)）
  证据: plan/t5-evidence.md

- [A8] status=done phase=T0
  症状: 没有页面到驱动的确定性判定通道；唯一通道是 CDP 求值，而它在本机 Chrome 上坏了
  根因: scripts/web-check.mjs:327
  验收: 本地后端提供 POST /verdict 与 GET /verdict；页面用 window.dhampir.reportVerdict 主动回传；驱动据此给出明确结论
  证据: plan/t0-evidence.md

- [A9] status=wontfix phase=T7
  症状: 渲染任务没有持久化与续渲，一次失败全丢
  根因: crates/dhampir-worker/src/pipeline.rs:1
  验收: 出片能留下可恢复的中间态，或明确记为不做并说明理由 —— **选了后者**。出片那条路是流式的（GPU 合成 -> 读回像素 -> 写进 ffmpeg 的 stdin），而 MP4 基本流不能从第二个进程接着写，所以"续渲"最多只能是"别把合成过的帧再合成一遍"，即逐帧检查点；检查点在磁盘上是 8.29 MB/帧（1920x1080x4，1080p30 一分钟 14.93 GB），对照实测渲染成本 14.56~56.96 ms/帧 —— 代价与收益不在一个量级。唯一磁盘上可接受的形态（存 PNG）每帧要一次 PNG 编码，与它想省下的渲染在同一量级（**这一句是估计，不是量测**：PNG 编码的每帧成本本仓没量过；量过的是裸 RGBA 落盘 2.81 ms/帧与 2957 MB/s，见 plan/measurements.md 第九项）；而"每 N 帧一个检查点 + 分片 MP4 追加"会改产物字节，撞上 T6 冻结的 argv 与"无声路径逐字节不变"。理由、数字与重审触发条件写在 plan/t7-evidence.md 的 T7.3
  证据: plan/t7-evidence.md

- [A10] status=done phase=T5
  症状: 单线程顺序解码，吞吐上限写死
  根因: crates/dhampir-worker/src/pipeline.rs:649 plan/measurements.md:1
  验收: 给出并发数并说明代价；没有数字就不许写进结论
  证据: plan/t5-evidence.md
