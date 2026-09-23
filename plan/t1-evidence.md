# T1 证据：文档坐标系

写于 T1 收口时。这份文件是 [defects.md](./defects.md) 里 D1 与 A1 的**证据文件**。

## 1. 改了什么

契约里的像素量以前**没有归属的坐标系**：transform.x/y 按目标像素解释，
而目标尺寸在预览里是画布、在出片里是导出尺寸。于是同一个工程在不同尺寸下
位移的**相对位置**不同 —— 「预览所见 != 成片所得」。

修法是把「坐标系」和「渲染到多大」显式分成两件事：

    RenderSpace { sequence: 文档坐标系, target: 实际渲染尺寸 }

* sequence 来自工程的 render_hints（唯一入口 ProjectDoc::sequence_size）；
* target 是这一帧渲染到多大；
* 两者不等时，transform.x/y 按 target/sequence 换算成目标像素；
* **相等时比例是 1.0，行为与从前逐字节一致** —— 出片的默认路径（导出尺寸取 render_hints）
  和预览的默认画布（640x360 == 样本工程的 render_hints）都走在那条路上。

RenderSpace 是**必填形参、没有默认值**：8 个调用点都必须说清坐标系是什么。
这正是「静默退回旧语义」这类回归最难发生的地方。

## 2. 一处台账修正（实核之后才发现）

台账里原先写着「像素量只有 transform.x/y 与 gaussian_blur.radius，两个都要缩放」。
**这一半是错的**，实核之后分开看：

| 量 | 度量单位 | 要不要换算 |
|---|---|---|
| transform.x / y | 文档像素 | **要** |
| 每层的 gaussian_blur.radius | **源纹理像素**（模糊跑在源尺寸的纹理上） | **不换** |
| 调整图层的模糊半径 | 文档像素（模糊跑在目标尺寸的中间纹理上） | **要** |

换算每层那个半径反而是错的：同一个源在不同导出尺寸下会糊得不一样。
两处都写了注释说明为什么。

## 3. 判据与结果

### 3.1 语义：归一化落点与目标尺寸无关（Rust 单元测试）

    $ cargo test --workspace
    test result: ok. 141 passed ...（dhampir-core）
    合计 368 passed / 10 ignored / 0 failed        EXIT=0

新增四条（都在 dhampir-core 的 render/compose.rs）：

* 相同尺寸时比例是_1_且不改变任何像素量
* 文档尺寸为零时兜到_1_而不是除零
* 位移的归一化落点与目标尺寸无关 —— **这就是 D1 的判据**：
  640x360 / 320x180 / 1280x720 / 960x540 四种目标尺寸下，一层的归一化落点必须
  相同且等于 0.5 + transform.x / sequence.x（容差 1e-5，f32）。
* 不换算文档像素时归一化落点会随目标尺寸变 —— **反向用例**：自己造出修复前的行为，
  要求它必须被抓出来，并断言错的方向是「小目标里位移占比更大」。
  没有这条，上面那条可能是恒真的。

### 3.2 结构：两个宿主有没有把坐标系接上（scripts/check-preview-parity.mjs）

    $ node scripts/check-preview-parity.mjs                    EXIT=0
    ✓ 预览与成片共用同一个文档坐标系（core 在换算、两个宿主都从工程取 sequence）
    $ node scripts/check-preview-parity.mjs --self-test        EXIT=0
    ✓ 预览坐标系守卫自检通过（12 条断言）

它管的是**接线**而不是数学（数学在 Rust 里测）：core 有没有在换算、出片有没有用
plan.sequence、CLI 有没有用 doc.sequence_size()、预览宿主有没有用 doc.sequence_size()、
有没有人把**裸元组**当坐标系传、以及 RenderSpace::square 有没有出现在允许清单以外的地方
（square 是「这条路上没有工程」的显式声明）。
12 条断言里有 10 条是**必须变红**的反向用例。

### 3.3 默认路径没变：里程碑逐字节不变

    $ node scripts/web-check.mjs
      "bytes": 33287, "frames": 90, "expected": 90, "size": "640x360", "fps": "30/1"
    ✓ app 验收通过：逐帧导出 -> FFmpeg 编码 -> 帧数与工程一致      EXIT=0
    $ git status --porcelain milestones/
    （空 —— milestones/edited-milestone.mp4 一个字节都没变）

这是「默认路径行为不变」最直接的一条证据，而且它是**端到端**的：
浏览器渲染 -> FFmpeg 编码 -> 落盘，与修复前的 33287 字节完全一致。

### 3.4 双端一致性没受影响

    $ node scripts/check-dual-end.mjs
    {"frame": 0,  "ssim": 1.000000, ...}
    {"frame": 30, "ssim": 1.000000, ...}
    {"frame": 60, "ssim": 1.000000, ...}
    {"frame": 89, "ssim": 1.000000, ...}
    ✓ 双端一致性通过                                              EXIT=0

合成源那条路（RenderSpace::square）本来就是「目标即坐标系」，所以它**应当**不受影响。
它绿说明这次改动没有波及没有工程的路径。

### 3.6 端到端：真机量落点（本来是 §4 里那条缺口，已补）

样本工程量不出位置 —— 它是一张满帧视频，不管位移怎么算整帧都被盖住，包围盒永远是整张图。
所以另造了一块**有区分度**的夹具：源 320x180、中间一块 80x40 不透明、四周透明，
位移取文档像素 (160, 90)，文档坐标系取样本工程那组 640x360。

    $ cargo test -p dhampir-worker --test preview_parity -- --ignored
    test 同一工程在不同目标尺寸下的落点一致 ... ok
    test 拿目标尺寸当坐标系时落点会明显不同 ... ok
    test result: ok. 2 passed; 0 failed; 0 ignored

第一条把 640x360 / 320x180 / 1280x720 三种目标各渲一张图出来，量非透明像素的包围盒，
要求归一化落点都等于 0.5 + 位移/文档尺寸（容差 0.02，即约一个像素）。
第二条是**反向验证**：改用 RenderSpace::square（也就是修复前的行为），
要求落点必须明显不同（差 > 0.1）—— 否则那个夹具量不出位置，第一条就是空转的。

它默认 #[ignore]（整套测试不该依赖一台有 GPU 的机器），所以它**不是默认关卡**：
默认关卡是语义那半（不需要 GPU）加接线那半（结构守卫）。三层各钉一层，分工是写明的。

### 3.5 预览那条路还活着

    $ wasm-pack build --dev --target web --out-dir www/pkg         EXIT=0
    $ node scripts/web-check.mjs --local --verdict trim-parity
      ✓ 预览与 CLI 对同一个 op 给出逐字段相同的工程                EXIT=0

## 4. 覆盖边界（不假装）

（原先这里记着一条「没有做端到端像素比对」—— **已补**，见 §3.6。）
* 预览**性能**没有重新测量：这次没有增加任何渲染像素（预览仍按画布尺寸渲染，
  只是换算了一次比例），所以 T1.4 里担心的「1080p 预览变慢」**没有发生**，
  那个逃生口（preview_scale）**不需要**。

## 5. 改动一览

| 文件 | 改动 |
|---|---|
| crates/dhampir-core/src/render/compose.rs | 新增 RenderSpace（sequence/target/pixel_scale/offset）；compose 用它换算 transform；4 条单元测试 |
| crates/dhampir-core/src/render/timeline.rs | render_frame/render_segmented/compose_layers 收 RenderSpace；新增 scale_document_radius（只用于调整图层） |
| crates/dhampir-core/src/render/mod.rs | 导出 RenderSpace、scale_document_radius |
| crates/dhampir-timeline/src/project.rs | 新增 ProjectDoc::sequence_size()（文档坐标系的唯一定义处） |
| crates/dhampir-worker/src/pipeline.rs | RenderPlan 加 sequence；两处出片调用交 RenderSpace |
| crates/dhampir-worker/src/bin/dhampir.rs | 两处 RenderPlan 用 doc.sequence_size() |
| crates/dhampir-wasm/src/timeline_host.rs | 工程绘制用 doc.sequence_size()；探针/语料两条路显式 square；改掉「预览尺寸由宿主决定」的注释 |
| crates/dhampir-worker/tests/*、examples/* | 调用点显式给 RenderSpace::square |
| scripts/check-preview-parity.mjs | 新增结构守卫（第 10 个） |
