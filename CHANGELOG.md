# 变更记录

格式遵循 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)；版本号遵循语义化版本。

⚠️ **契约版本与产品版本是两条独立的线**：产品版本（本文件）是发布节奏，
`project_schema` / `host_api` 是**兼容性承诺**。一次只有后者变化的发布，产品版本仍要动 ——
因为下游钉固的是产物 zip 的字节，不是契约号。

---

## [0.2.0] — 字形回退、渲染能力面、Web 动画对齐

**契约号一个都没动**（`project_schema = 1`、`host_api = 6`），但**产物字节变了**：
字幕的栅格化器从 `drawtext` 换成 libass。按上面那条规矩，产品版本照样要动 ——
下游钉固的是 zip 的字节，不是契约号。

### ⚠️ 破坏性变更：老工程的字幕像素会变

**没动契约字段，所以下游不会在任何一处契约校验里收到提示。** 只能靠这段文字知道。

- **每一帧字幕的像素都变了**。换了栅格化器（libass / FreeType 对同一副字形轮廓的
  扫描与抗锯齿核与 `drawtext` 不同）。实测两处可见差异：
  - **边缘略柔**：同字号下边缘斜坡宽度从 0.63~1.43 px 变成 1.63~3.17 px（四个字全部同向）。
    仍在"窄抗锯齿"口径内（模糊会是十几像素），但**看得见**。
  - **字距有漂移**：每字推进宽度不是逐像素相同（实测老路 40 px/字、新路 38 px/字，量法见
    `text_raster.rs` 的 `best_alignment` 注释）。
- **下游该做什么**：如果你们拿 dhampir 的出帧做过**视觉基线或截图回归**，
  这次必须**重做基线**，否则会全是红。判定口径（SSIM/PSNR、边缘口径）不用改，**基线的图要重拍**。
- **不涉及无字幕的工程**：没有文字层时两条路一个像素都不差 —— 实测
  `fixtures/sample-project.doc.json` 第 15 帧 sha256 在整轮改造前后**逐字节相同**
  （`91ac70187a5d3fdd7…`）。
  ⚠️ 这是**读数，不是被钉住的断言**：那个 hash 不在任何代码或守卫里，改坏了**不会自动变红**，
  判定方式是"改动前后各跑一次 `frame` 比 sha256"。**无自动守卫是一个已知缺口。**

### Added

- **字形回退（缺字不再是方框）**。字体里缺某个字形时（用户的文案总有覆盖不到的码位），
  libass **逐字形**回退到别的字体把那个字补出来。此前 `drawtext` 不会回退，缺字直接画成
  `.notdef` 空心方框 **且退出码为 0**（静默）。复现与位证见 `plan/glyph-fallback-evidence.md`：
  乐米波波体缺 `靥 U+9765`，「笑靥如花」曾渲染成「笑☒如花」。
- **`--font-dir` + 契约的 `font_family` 成为字体定位的正路**，`--font-file` 退居兜底。
  （字段与选项此前就在契约里，这一版把它接到了栅格化上，并写进 `docs/usage.md`。）
- **渲染能力面**：`blend_fn`（混合模式）、`gradient`（渐变）、`path` / `polygon`（路径与多边形）
  四个渲染模块 + 对应 WGSL；契约侧新增 `easing.rs`、`path.rs`。
- **WAAPI/CSS 动画对齐**：缓动全集、`anim2doc` 转译器、DOM 第二宿主（`web/dom-host.mjs`）。
  HTML 侧是**刻意的第二实现**，由 `scripts/check-anim-eval.mjs` 逐帧逐通道与 core 对照
  （容差 1e-4，实测最大偏差 9.37e-6）。
- **效果演示**：`fixtures/effects-demo.json` + `web/effects-demo.html`（WASM 与 DOM 两版）
  + 联络表 `web/effects-demo/effects-demo-contact-sheet.png`。

### Changed

- **`drawtext` → `libass`（`subtitles=`）**：理由与取舍写在 `text_raster.rs` 模块头。
  选 `subtitles=` 而不是 `ass=` 的理由也在那里。
- **字体路径含非 ASCII 字符时不再失败**：此前 ffmpeg 会**段错误**（退出码 139）且 0 字节输出 ——
  这是"静默失败"里最坏的一种。现在会把字体复制到 ASCII 临时路径再用，用后清理。
- **三条静默路改成响亮失败**：字体目录不存在、目录里没有可用字体、`font_family` 给了却
  找不到 —— 都退非零退出码并**点出是哪个名字、哪个目录**。此前会静默回退到系统默认字体
  （实测解析成 `ArialMT`），而 `lines_failed: 0`、`issues: []`。
- **覆盖度口径**：libass 输出到 `rgba` 时 **alpha 恒为 0**、覆盖度在 RGB 通道，
  且**黑描边与透明底逐字节相同**（`(0,0,0,0)`）—— 覆盖度事后数学上不可恢复。
  因此填充与描边都画成白墨，颜色交回 `tint`。

### 已知限制（新增两条）

- **描边颜色当前不受 `stroke_color` 控制**：填充与描边共用一张覆盖掩码。已用判据钉住
  （`描边颜色当前不受_stroke_color_控制_这条边界钉住`），拿到分层掩码时那条判据会红，
  提醒实现者去更新文档与 `tint` 口径。
- **字形回退落到哪一份字体取决于机器上装了什么字体**。同机同目录两次出片一致，
  **换机器/平台缺字那几个字可能换一副字形**。回退用的字体集合该由谁提供，
  **尚未与下游对齐，如实记为未定**（见 `docs/usage.md`）。

### 依赖前置（变了，装环境时要注意）

- **ffmpeg 必须带 `--enable-libass`**。此前只需要 `--enable-libfreetype`。
  没编 libass 时行为是**响亮失败**（filter 不存在 → 非零退出码 → 走既有报错路径），
  不会静默出没字幕的片子；但会在出片那一刻才炸，所以建议装前先确认。
  自检：`ffmpeg -filters | grep subtitles`。

### 这一版的验证读数

- `cargo test --workspace`：**766 passed / 0 failed**；`cargo check --all-targets`：**0 warning**
- 守卫：**26 / 26 全绿**（含各自 `--self-test`）
- 真机（真起 ffmpeg、逐像素量）：**9 / 9 全绿**
- 无字幕工程帧 sha256：**整轮未变**（`91ac70187a5d3fdd7…`）—— **读数，暂无自动守卫**

---

## [0.1.0] — 首个发布版本

第一个正式发布的版本。此前所有开发都在 `0.0.1` 这个占位号下进行，**从未发布过**。

### 发布物

- `dhampir-0.1.0-win32-x64.zip`（+ `.sha256.txt`）—— 下游宿主钉固这一个
- 内含：`bin/dhampir.exe`（出片）、`preview/`（浏览器预览）、`VERSION`、许可件

### 契约版本

- `project_schema = 1` —— 工程文件格式
- `host_api = 6` —— wasm 导出面

### 这一版有什么

- **同一份 `dhampir-core` 编译到两个宿主**：native（headless wgpu 出片）与 wasm32（浏览器 WebGPU 预览），
  两条腿跑出**逐字节相同**的 golden 报告（M0 起就是硬判据）。
- **动图（GIF / 动画 WebP）原生解码**：宿主交 **GIF 原始字节**即可，引擎自己解、自己常驻显存、
  自己按帧号供帧 —— 不再需要宿主逐帧栅格化后喂位图。预览与出片**用同一个解码器**，
  所以贴纸相位不会各算各的。接入见 [docs/host-animation-integration.md](docs/host-animation-integration.md)。
- **Apache-2.0 单许可**（2026-09-30 由 `MIT OR Apache-2.0` 收窄；收窄前未发布过任何版本）。
  自带专利授权，允许商用与闭源集成。

### 已知限制

- 本仓只做渲染与预览底座：网关 / 任务队列 / 对象存储 / 部署 / UI 产品化都在下游。
- 只提供 **Windows x64** 预编译产物；其他平台需自行构建（`cargo build --release` + `wasm-pack`）。
- `check-web-invariants` / `check-dual-end` 要求 `crates/dhampir-wasm/www/pkg` 比源码新；
  克隆后需跑一次 `wasm-pack build crates/dhampir-wasm --target web --out-dir www/pkg --dev`。

### 本版前的仓库侧整理

- 去掉文档与注释里「把某个下游宿主当成唯一宿主」的写法（引擎本身是宿主中立的）。
- `scripts/bench-animation-upload.mjs` 的素材目录不再写死本机路径，改为**参数必填**。
- 补 `LICENSE`（GitHub 许可证识别器只认标准名；与 `LICENSE-APACHE` 逐字节相同）。
